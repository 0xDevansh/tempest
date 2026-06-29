# Tempest — BitTorrent CLI Client: Build Plan

## Goal

Grow `tempest` from a bencode library into a working BitTorrent **CLI client** matching:

- Custom bencode parser for `.torrent` files, SHA-1 verified piece reassembly.
- Rarest-first piece selection, availability tracked with bitfields; tit-for-tat choke
  algorithm to prioritize altruistic peers.
- Async Tokio task architecture with a sliding window of in-flight requests
  (target: **8 peers × 12 requests** = 96 outstanding block requests).

### Decisions
- **Crate policy: pragmatic.** Hand-write bencode + all protocol logic; use crates for
  plumbing (`tokio`, `sha1`, `reqwest`, `rand`, `bytes`, `url`, optionally
  `anyhow`/`thiserror`).
- **Trackers: UDP + HTTP/HTTPS.** UDP is required because the bundled `test.torrent`
  announces only UDP/WebSocket trackers. WebSocket is out of scope.
- **Scope: full leech + choke algorithm.** Download to completion (rarest-first, SHA-1
  verified), serve blocks to peers while downloading, implement tit-for-tat
  choke/unchoke. No long-term seeding loop after completion.

### Reference facts about `test.torrent`
Multi-file (3 files, total 276,445,467 bytes), **1055 pieces**, piece length **262144
(256 KiB)**, `info_hash = dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c`. Trackers are UDP +
WebSocket only (no HTTP), so UDP tracker support is mandatory to use it directly.

---

## Status

- [x] **Phase 1 — Bencode encode + decode** (`src/lib/bencoding.rs`). Tokenize → parse,
  `BString`-backed (binary-safe), `i64` numbers, accessor helpers
  (`as_int/as_bytes/as_str/as_list/as_dict/get`). Tested.
- [x] **Phase 2 — Metainfo + info_hash** (`src/lib/metainfo.rs`). `MetaInfo::parse`,
  `Info`, `Layout::Single|Multi`, multi-file support, `info_hash` over the raw info-dict
  byte span (`info_dict_span`/`skip_value`). Verified against a known-good hash.
- [x] Phase 3 — Trackers (UDP + HTTP)
- [x] Phase 4 — Single-peer sequential download (the "sequential actor")
- [x] Phase 5 — Refactor to async Tokio tasks + sliding window
- [x] Phase 6 — Rarest-first + bitfields
- [x] Phase 7 — Choke algorithm (tit-for-tat)
- [ ] Phase 8 — (optional) endgame, random-first-piece, resume

---

## Target architecture

A single **coordinator** task owns all shared download state (`MetaInfo` + `PieceManager`
+ per-peer bookkeeping). Each connected peer runs as its own Tokio task. Peer tasks and
the coordinator communicate over `tokio::sync::mpsc` channels — no shared `Mutex` on the
hot path. The resume's "sequential actors → async Tokio" story is realized by building
Phases 3–4 synchronously (single peer, blocking I/O), then introducing Tokio in Phase 5.

```
                 tracker::announce (UDP/HTTP)  ->  peer list
                          │
                          ▼
   ┌─────────────────────────────────────────────┐
   │              Coordinator task                 │
   │   owns: MetaInfo, PieceManager, PeerState map │
   │   - rarest-first piece assignment             │
   │   - choke algorithm (tit-for-tat, 10s tick)   │
   │   - writes verified pieces to disk            │
   └───▲───────────────┬───────────────┬──────────┘
       │ PeerMsg        │ CmdMsg         │ CmdMsg
   ┌───┴────┐     ┌─────┴───┐     ┌──────┴──┐
   │ Peer 0 │ ... │ Peer i  │ ... │ Peer N  │   each = 1 Tokio task, owns 1 TcpStream
   └────────┘     └─────────┘     └─────────┘
```

- **CmdMsg** (coordinator → peer): `Choke/Unchoke`, `Interested/NotInterested`,
  `RequestBlock(index, begin, len)`, `CancelBlock(...)`, `Have(index)`, `SendBitfield`.
- **PeerMsg** (peer → coordinator): `Bitfield`, `Have(index)`, `BlockReceived{index,
  begin, data}`, `RequestFromPeer{index, begin, len}`, `Choked/Unchoked`,
  `Interested/NotInterested`, `Connected{peer_id}`, `Disconnected`.

The sliding window (8 peers × 12) is enforced by the coordinator: ≤12 outstanding
`RequestBlock` per peer, refilled as `BlockReceived` arrives, across up to ~8 actively
downloading peers.

---

## Files & modules

Follows the existing nested `src/lib/` module style (root is `src/lib/mod.rs`).

```
Cargo.toml                 # add deps as phases land
src/main.rs                # CLI: parse args, load .torrent, run client
src/lib/mod.rs             # module root (pub mod ...)
src/lib/bencoding.rs       # [done] encoder + decoder
src/lib/metainfo.rs        # [done] .torrent parsing, info_hash
src/lib/tracker/mod.rs     # announce dispatch by URL scheme
src/lib/tracker/http.rs    # HTTP/HTTPS tracker (reqwest + bencode response)
src/lib/tracker/udp.rs     # UDP tracker protocol (BEP 15)
src/lib/peer/mod.rs        # peer wire protocol
src/lib/peer/message.rs    # Message enum + length-prefixed framing
src/lib/peer/handshake.rs  # handshake build/parse
src/lib/peer/connection.rs # per-peer Tokio task (read/write loop)
src/lib/piece.rs           # PieceManager, bitfield, rarest-first, verification
src/lib/coordinator.rs     # coordinator task, choke algorithm, disk writes
src/lib/storage.rs         # multi-file piece↔file mapping + writes
src/lib/error.rs           # shared error type
```

---

## Data structures (remaining phases)

### peer/message.rs
```rust
pub enum Message {
    KeepAlive,
    Choke, Unchoke, Interested, NotInterested,
    Have(u32),
    Bitfield(Vec<u8>),
    Request { index: u32, begin: u32, length: u32 },
    Piece   { index: u32, begin: u32, block: Vec<u8> },
    Cancel  { index: u32, begin: u32, length: u32 },
    Port(u16),   // DHT; parse & ignore
}
```
Wire framing: 4-byte big-endian length prefix + 1-byte id + payload; `KeepAlive` is a
zero-length frame. Encode/decode are pure functions (unit-testable). In the async peer
task, use `tokio_util::codec::Framed` with a custom `Decoder`/`Encoder` for backpressure.

### peer/handshake.rs
`<pstrlen=19><"BitTorrent protocol"><8 reserved zero bytes><info_hash 20><peer_id 20>` —
build and parse; verify the peer echoes our `info_hash`, capture their `peer_id`.

### piece.rs — the download brain
```rust
pub struct Bitfield { bits: Vec<u8>, num_pieces: usize }  // has(i), set(i), count()
pub enum PieceState { Missing, InFlight, Have }
pub struct BlockRequest { index: u32, begin: u32, length: u32 }  // 16 KiB blocks

pub struct PieceManager {
    piece_length: u64,
    total_length: u64,
    hashes: Vec<[u8;20]>,
    state: Vec<PieceState>,
    availability: Vec<u16>,               // peers having each piece (rarest-first)
    partial: HashMap<u32, PartialPiece>,
    have_bitfield: Bitfield,              // our completed pieces
}
struct PartialPiece { buf: Vec<u8>, received: Bitfield /*over blocks*/, blocks_left: u32 }
```
Key methods: `on_peer_bitfield/on_peer_have/on_peer_disconnect` (maintain `availability`);
`pick_piece(peer_has)` → **rarest-first** among the peer's `Missing` pieces (random tie-
break; random-first-piece for the first few); `next_blocks(index, max)` → 16 KiB requests
respecting last-piece/last-block short sizes; `on_block(...)` → fill buffer, on full
**SHA-1 verify** vs `hashes[index]`, on match mark `Have` + return assembled piece, on
mismatch reset to `Missing`; `is_complete`, `progress`. Block size = 16384 (2^14).

### storage.rs — multi-file reassembly
```rust
pub struct Storage { files: Vec<FileSpan>, piece_length: u64 }
struct FileSpan { handle: File, offset: u64, length: u64 }  // absolute byte range
```
`write_piece(index, data)`: map piece to absolute range `[index*piece_length, +len)`,
split across overlapping `FileSpan`s, `seek`+`write` into each. Pre-create dirs/files and
set lengths on init. This is what makes reassembly correct for multi-file torrents.

### peer/connection.rs — per-peer async task
Owns one `TcpStream`. `tokio::select!` over inbound frames → `PeerMsg` to coordinator, and
`CmdMsg` from coordinator → outbound `Message`. Tracks `am_choking`, `am_interested`,
`peer_choking`, `peer_interested`. Serves peer `Request`s by asking the coordinator for the
block (upload side, needed for tit-for-tat).

### coordinator.rs — orchestration + choke algorithm
Owns `PieceManager`, `Storage`, `HashMap<PeerId, PeerHandle>`.
- **Assignment**: on `Unchoked`/`BlockReceived`, for each peer that unchoked us with < 12
  in-flight, `pick_piece` + `next_blocks`, dispatch up to the window cap; cap
  simultaneously-downloading peers at ~8.
- **Verify + write**: on completed piece, `Storage::write_piece`, broadcast `Have`.
- **Choke (tit-for-tat)**, 10 s interval: rank interested peers by recent download rate
  from them, unchoke top ~3, every 30 s optimistically unchoke one random choked peer,
  choke the rest, reset rate window.
- **Endgame** (optional): when few blocks remain, request from all peers, `Cancel` on
  first arrival.

### tracker/*
- `mod.rs`: `announce(meta, peer_id, port, event) -> Vec<SocketAddr>`, dispatch on URL
  scheme across `announce_list` tiers (BEP 12 ordering); return first that answers.
- `http.rs`: build query (`info_hash`, `peer_id` URL-encoded raw bytes, `port`,
  `uploaded`, `downloaded`, `left`, `compact=1`), GET, bencode-decode, parse compact
  peers (6 bytes: 4 IP + 2 port).
- `udp.rs` (BEP 15): connect (magic `0x41727101980`) → connection_id → announce →
  compact peers, over `tokio::net::UdpSocket` with transaction IDs, timeouts, retries.

### error.rs
Shared error enum (`Bencode`, `Metainfo`, `Tracker`, `Peer`, `Io`, `HashMismatch{index}`);
`main` can use `anyhow`.

### main.rs (CLI)
```
tempest <path-to.torrent> [--output DIR] [--port 6881] [--max-peers 40]
```
Read file → `MetaInfo::parse` → generate 20-byte `peer_id` (`-TE0001-` + random) →
`tracker::announce` → spawn coordinator → spawn a peer task per address (bounded by
`--max-peers`) → progress to stderr → exit when complete. Args via `clap`.

---

## Cargo.toml additions (as phases land)
```toml
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["codec"] }
reqwest = { version = "0.12" }
sha1 = "0.10"          # already added
rand = "0.8"
bytes = "1"
url = "2"
clap = { version = "4", features = ["derive"] }
# optional: anyhow, thiserror
```

---

## Verification

- **Unit tests** (`cargo test`): bencode round-trip; message encode/decode round-trip;
  bitfield get/set/count; rarest-first picks lowest-availability piece; piece verification
  accepts a correct block set and rejects a corrupted one; multi-file `write_piece` byte-
  range splitting. (info_hash-vs-known-value test already in place.)
- **Tracker integration** (network, ignored by default): announce to the bundled UDP
  trackers, assert a non-empty peer list.
- **End-to-end**: `tempest test.torrent --output /tmp/bbb`; on completion re-hash every
  piece against `info.pieces` and assert all match. Because a 276 MB public-swarm download
  is slow/flaky, also test against a **local swarm**: seed the same torrent from
  `transmission-cli`/`aria2c` on localhost, or use a small self-made torrent for fast runs.
- **Sliding-window sanity**: log outstanding-request counts; assert ≤12 per peer and that
  throughput scales with peer count vs. the Phase-4 sequential baseline.

---

## Key risks / call-outs
- **info_hash from raw bytes** (not re-encoded) — done via `info_dict_span`.
- **Binary-safe bencode** — `pieces`/`peer_id` are non-UTF-8; `BString` values avoid
  corruption.
- **Multi-file piece mapping** — pieces straddle file boundaries; `storage.rs` must split.
- **UDP tracker required** for the bundled torrent; HTTP alone won't exercise it.
- **Window bookkeeping** — the coordinator is the single source of truth for in-flight
  counts to keep the 8×12 window correct under concurrency.
