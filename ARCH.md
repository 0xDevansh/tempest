# Tempest Architecture

Tempest is a BitTorrent leeching CLI: parse a `.torrent`, announce to trackers, download
pieces over the peer wire protocol with rarest-first selection and tit-for-tat choking,
verify with SHA-1, and write multi-file layouts to disk.

The design centers on a single **coordinator** task that owns all shared download state.
Each peer runs as its own Tokio task and talks to the coordinator over `mpsc` channels —
no shared `Mutex` on the hot path. The sliding window target is **8 peers × 12 in-flight
block requests**.

---

## Runtime overview

```
  CLI (main.rs)
       │
       ├─ MetaInfo::parse(.torrent)          ← bencoding + metainfo
       ├─ tracker::announce(...)             ← UDP / HTTP
       │         └─ Vec<SocketAddr>
       └─ Coordinator::run(peers, max_peers)
                 │
    ┌────────────┼──────────────────────────────┐
    │            ▼                              │
    │   Coordinator task                        │
    │   owns: MetaInfo, PieceManager, Storage,  │
    │         HashMap<PeerId, PeerState>        │
    │   - rarest-first assignment               │
    │   - 8×12 request window                   │
    │   - tit-for-tat choke (10s tick)          │
    │   - verified piece → disk write           │
    └───▲──────────────┬──────────────┬─────────┘
        │ PeerMsg      │ CmdMsg       │ CmdMsg
   ┌────┴───┐    ┌─────┴────┐   ┌─────┴────┐
   │ Peer 0 │ …  │ Peer i   │ … │ Peer N   │  each = 1 Tokio task + TcpStream
   └────────┘    └──────────┘   └──────────┘
```

**End-to-end flow**

1. Parse metainfo and compute `info_hash` over the raw info-dict bytes.
2. Generate a 20-byte peer id (`-TE0001-` + random).
3. Announce (`Started`) to UDP/HTTP trackers; collect compact peer addresses.
4. Bind a TCP listener; dial outbound peers (bounded by `--max-peers`).
5. Per peer: handshake → exchange bitfields / interest → request 16 KiB blocks.
6. On full piece: SHA-1 verify → `Storage::write_piece` → broadcast `Have`.
7. Exit when `PieceManager` reports complete (no long-term seed loop).

---

## Crate layout

| Path | Role |
|------|------|
| `Cargo.toml` | Binary + lib (`src/lib/mod.rs`); Tokio, reqwest, sha1, clap, etc. |
| `src/main.rs` | CLI entrypoint |
| `src/lib/mod.rs` | Library root — module declarations |
| `src/lib/error.rs` | Shared error type |
| `src/lib/bencoding.rs` | Bencode tokenize / parse / encode |
| `src/lib/metainfo.rs` | `.torrent` → `MetaInfo` + `info_hash` |
| `src/lib/tracker/mod.rs` | Announce dispatch (BEP 12 tiers) |
| `src/lib/tracker/udp.rs` | UDP tracker (BEP 15) |
| `src/lib/tracker/http.rs` | HTTP/HTTPS tracker |
| `src/lib/peer/mod.rs` | Peer submodule root + re-exports |
| `src/lib/peer/handshake.rs` | 68-byte handshake build/parse |
| `src/lib/peer/message.rs` | Wire `Message` + `MessageCodec` |
| `src/lib/peer/connection.rs` | Per-peer Tokio task, `CmdMsg` / `PeerMsg` |
| `src/lib/piece.rs` | Bitfields, rarest-first, block assembly, verify |
| `src/lib/storage.rs` | Multi-file piece ↔ file byte mapping |
| `src/lib/coordinator.rs` | Orchestration, window, choke algorithm |

---

## File-by-file walkthrough

### `src/main.rs` — CLI

**Types**

- `Args` — clap-derived: `torrent`, `--output`, `--port`, `--max-peers`.

**Behavior**

- Reads the torrent file, `MetaInfo::parse`, prints summary.
- `generate_peer_id()` → `[u8; 20]` with Azureus-style prefix `-TE0001-`.
- `tracker::announce(..., AnnounceEvent::Started)` → peer list.
- Builds `Coordinator` and `await`s `run` until download completes.

---

### `src/lib/error.rs` — Shared errors

```text
Error { Bencode, Metainfo, Tracker(String), Peer(String), Io, HashMismatch { index }, Other }
Result<T> = Result<T, Error>
```

`From` impls map `ParsingError` and `MetaError`. `main` wraps these in `anyhow`.

---

### `src/lib/bencoding.rs` — Bencode

Hand-written parser used for `.torrent` files and HTTP tracker responses.

| Type | Purpose |
|------|---------|
| `Token` | Lexer output: `ListBegin`, `DictBegin`, `End`, `Number(i64)`, `String(BString)` |
| `Bencodable` | AST: `Number`, `String`, `List`, `Dict` |
| `ParsingError` | `InvalidInput` |

**Pipeline:** bytes → `tokenize` → `parse` → `Bencodable`.

**Accessors on `Bencodable`:** `as_int`, `as_bytes`, `as_str`, `as_list`, `as_dict`, `get(key)`.

Strings are `bstr::BString` so binary payloads (`pieces`, peer ids) stay intact.

---

### `src/lib/metainfo.rs` — Torrent metadata

| Type | Fields / variants |
|------|-------------------|
| `MetaError` | `Bencode`, `MissingField`, `NoInfoDict`, `BadPieces` |
| `TorrentFile` | `length: u64`, `path: PathBuf` |
| `Layout` | `Single { length }` \| `Multi { files: Vec<TorrentFile> }` |
| `Info` | `name`, `piece_length`, `piece_hashes: Vec<[u8;20]>`, `layout` |
| `MetaInfo` | `announce`, `announce_list` (BEP 12 tiers), `info`, `info_hash`, `total_length`, `num_pieces` |

**`info_hash`:** `info_dict_span` + `skip_value` locate the raw `info` value in the
file bytes; SHA-1 is taken over that span (not a re-encode), which is required for
swarm compatibility.

---

### `src/lib/tracker/` — Peer discovery

#### `mod.rs`

| Type | Purpose |
|------|---------|
| `AnnounceEvent` | `None`, `Started`, `Completed`, `Stopped` — maps to HTTP query / UDP `u32` |

**`announce(meta, peer_id, port, downloaded, uploaded, event) → Vec<SocketAddr>`**

- Walks BEP 12 tiers; skips `ws://` / `wss://`.
- Within a tier, races UDP/HTTP announces in parallel; first non-empty peer list wins
  (25s tier deadline).
- `parse_compact_peers`: 6-byte groups → `Ipv4Addr` + port.

#### `udp.rs` (BEP 15)

Constants: `PROTOCOL_ID = 0x41727101980`, `ACTION_CONNECT`, `ACTION_ANNOUNCE`.

1. Resolve host (prefer IPv4); bind matching family; connect.
2. Connect request/response → `connection_id`.
3. Announce request (info_hash, peer_id, stats, event, port) → compact peers.
4. Retries with backoff `3s / 6s / 12s` (3 attempts).

#### `http.rs`

Builds a query with **percent-encoded raw bytes** for `info_hash` / `peer_id`,
`compact=1`. GETs via reqwest, bencode-decodes the body, reads compact or dict-list
`peers`. Surfaces `failure reason` if present.

---

### `src/lib/peer/` — Wire protocol

#### `handshake.rs`

Fixed 68-byte frame:

```text
<pstrlen=19><"BitTorrent protocol"><8 reserved zeros><info_hash 20><peer_id 20>
```

- `build_handshake` / `parse_handshake`
- `perform_handshake`: write ours, read theirs, verify `info_hash`, return remote peer id

#### `message.rs`

| Type | Purpose |
|------|---------|
| `Message` | `KeepAlive`, `Choke`, `Unchoke`, `Interested`, `NotInterested`, `Have(u32)`, `Bitfield(Vec<u8>)`, `Request { index, begin, length }`, `Piece { index, begin, block }`, `Cancel { ... }`, `Port(u16)` (parsed, ignored) |
| `MessageCodec` | `tokio_util` `Decoder`/`Encoder`: 4-byte BE length + payload |

`KeepAlive` is a zero-length frame. Encode/decode are pure and unit-tested.

#### `connection.rs` — Peer task

| Type | Purpose |
|------|---------|
| `PeerId` | `u64` — coordinator-assigned connection id (not the 20-byte BitTorrent peer id) |
| `CmdMsg` | Coordinator → peer: choke/interest, `RequestBlock`/`CancelBlock`, `Have`, `SendBitfield`, `SendPiece`, `Shutdown` |
| `PeerMsg` | Peer → coordinator: `Connected`, `Disconnected`, `Bitfield`, `Have`, `BlockReceived`, `RequestFromPeer`, choke/interest signals |

**`run_peer`:** owns one `TcpStream`. Handshake, then `Framed<TcpStream, MessageCodec>`
with `tokio::select!` over inbound frames and `cmd_rx`. Maps wire messages to `PeerMsg`;
executes `CmdMsg` as outbound `Message`s. On exit, always sends `Disconnected`.

---

### `src/lib/piece.rs` — Download brain

| Type | Purpose |
|------|---------|
| `BLOCK_SIZE` | `16384` (2¹⁴) |
| `Bitfield` | `bits: Vec<u8>`, `num_pieces` — `has` / `set` / `clear` / `count` / `from_bytes` |
| `PieceState` | `Missing` \| `InFlight` \| `Have` |
| `BlockRequest` | `{ index, begin, length }` |
| `PartialPiece` | `buf`, `received` (per-block bitfield), `blocks_left` |
| `PieceManager` | See below |
| `PieceError` | `BadBlock`, `HashMismatch { index }` |

**`PieceManager` fields**

| Field | Role |
|-------|------|
| `piece_length`, `total_length` | Sizing (last piece may be short) |
| `hashes` | Expected SHA-1 per piece |
| `state` | Per-piece `PieceState` |
| `availability` | Peer count per piece (rarest-first) |
| `partial` | In-progress piece buffers |
| `have_bitfield` | What we advertise / serve |
| `completed_count` | Progress + random-first gate |

**Key methods**

- `on_peer_bitfield` / `on_peer_have` / `on_peer_disconnect` — maintain `availability`
- `pick_piece(peer_has)` — rarest-first among `Missing` pieces the peer has; random among
  ties; **random-first** for the first 4 completed pieces
- `blocks_still_needed` / `next_blocks` — emit 16 KiB (or shorter last-block) requests
- `on_block` — fill `PartialPiece`; when full, SHA-1 vs `hashes[i]`; success → `Have` +
  return bytes; failure → reset to `Missing`
- `abandon_piece` / `reclaim_piece` — peer churn without wiping partial data

The coordinator is the source of truth for **which** `(index, begin)` pairs are in-flight
per peer; `PieceManager` only tracks received blocks.

---

### `src/lib/storage.rs` — Disk layout

| Type | Purpose |
|------|---------|
| `FileSpan` | `path`, absolute torrent `offset`, `length` |
| `Storage` | `files: Vec<FileSpan>`, `piece_length`, `root` |

- `Storage::new` — under `--output/<info.name>/`, pre-create dirs/files and `set_len`.
- `write_piece(index, data)` — map `[index * piece_length, …)` across overlapping
  `FileSpan`s; seek + write each slice.
- `read_block(index, begin, length)` — reverse mapping for upload serving.

This is what makes multi-file torrents reassemble correctly when pieces straddle files.

---

### `src/lib/coordinator.rs` — Orchestration

**Constants**

| Name | Value | Meaning |
|------|-------|---------|
| `MAX_INFLIGHT_PER_PEER` | 12 | Outstanding block requests per peer |
| `MAX_DOWNLOADING_PEERS` | 8 | Cap on actively downloading peers |
| `UNCHOKE_SLOTS` | 3 | Tit-for-tat upload slots |
| `CHOKE_INTERVAL` | 10s | Choke recalculation period |
| `OPTIMISTIC_EVERY` | 3 ticks | Optimistic unchoke every ~30s |

| Type | Purpose |
|------|---------|
| `PeerState` | Per-connection bookkeeping (private) |
| `Coordinator` | Owns meta, pieces, storage, peers, channels |

**`PeerState`**

| Field | Role |
|-------|------|
| `cmd_tx` | Channel into that peer’s task |
| `bitfield` | What the remote claims to have |
| `peer_choking` / `peer_interested` | Remote → us |
| `am_choking` | Us → remote (upload gate) |
| `downloaded_window` | Bytes from this peer since last choke tick |
| `inflight` | `HashSet<(piece, begin)>` outstanding requests |
| `assigned_piece` | Piece this peer is currently fetching |

**`Coordinator` fields:** `meta`, `pieces`, `storage`, `peers`, `next_peer_id`,
`our_peer_id`, `listen_port`, `peer_tx`/`peer_rx`, `choke_ticks`, `optimistic_unchoke`,
`uploaded`.

**`run` loop** (`tokio::select!`)

- Accept inbound TCP; dial outbound (bounded concurrent dials).
- Handle `PeerMsg` (bitfields, blocks, choke, upload requests).
- 10s choke tick; 1s progress logging.
- Exit on `pieces.is_complete()`, then `Shutdown` peers.

**Assignment (`fill_requests` / `plan_requests_for`)**

For each unchoked-us peer with room under 12 inflight (and under the 8-downloader cap
when idle): pick/reclaim a piece, issue `RequestBlock` cmds for needed blocks not already
in that peer’s `inflight` set.

**On `BlockReceived`:** update rates/inflight → `pieces.on_block` → on success
`storage.write_piece` + broadcast `Have`.

**On `RequestFromPeer`:** if we are not choking them and we `have` the piece, read from
storage and `SendPiece`.

**Choke algorithm (`run_choke_algorithm`)**

1. Rank interested peers by `downloaded_window`.
2. Unchoke top 3.
3. Every 3rd tick, optimistically unchoke one random remaining interested peer.
4. Choke everyone else; reset windows.

---

## Message protocol between tasks

```
CmdMsg (coordinator → peer)          PeerMsg (peer → coordinator)
───────────────────────────          ────────────────────────────
Choke / Unchoke                      Connected { id, peer_id }
Interested / NotInterested           Disconnected { id }
RequestBlock { index, begin, len }   Bitfield { id, bits }
CancelBlock { ... }                  Have { id, index }
Have(index)                          BlockReceived { id, index, begin, data }
SendBitfield(bytes)                  RequestFromPeer { id, index, begin, len }
SendPiece { index, begin, block }    Choked / Unchoked { id }
Shutdown                             Interested / NotInterested { id }
```

Peer tasks never touch `PieceManager` or `Storage` directly. Upload path:
`RequestFromPeer` → coordinator reads disk → `SendPiece` → peer writes `Message::Piece`.

---

## Data ownership summary

| Owner | State |
|-------|--------|
| Coordinator task | `MetaInfo`, `PieceManager`, `Storage`, `HashMap<PeerId, PeerState>`, choke timers |
| Each peer task | `TcpStream` / `Framed`, local cmd receiver |
| Channels only | Cross-task events (`CmdMsg`, `PeerMsg`); dial results (`TcpStream` mpsc) |

---

## Algorithms (where they live)

| Algorithm | Location |
|-----------|----------|
| Bencode parse | `bencoding.rs` |
| info_hash over raw bytes | `metainfo.rs` (`info_dict_span`) |
| UDP connect → announce | `tracker/udp.rs` |
| Compact peer decode | `tracker/mod.rs` |
| Rarest-first (+ random-first open) | `piece.rs` (`pick_piece`) |
| SHA-1 piece verify | `piece.rs` (`on_block`) |
| Multi-file write split | `storage.rs` (`map_range`) |
| 8×12 sliding window | `coordinator.rs` (`fill_requests`) |
| Tit-for-tat + optimistic unchoke | `coordinator.rs` (`run_choke_algorithm`) |

---

## Out of scope (Phase 8)

- Endgame mode (redundant requests + `Cancel`)
- Resume / bitfield from existing on-disk data
- WebSocket trackers, DHT, magnet links, long-term seeding after completion
