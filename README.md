## Tempest

BitTorrent CLI client in Rust: custom bencode parser, SHA-1 verified piece reassembly,
rarest-first selection, tit-for-tat choking, and async Tokio peer tasks with a sliding
window of in-flight requests (8 peers × 12 requests).

### Build

```bash
cargo build --release
```

### Usage

```bash
tempest <path-to.torrent> [--output DIR] [--port 6881] [--max-peers 40]
```

Example:

```bash
cargo run --release -- test.torrent --output /tmp/bbb
```

### Tests

```bash
cargo test
cargo test -- --ignored   # UDP tracker announce (needs network)
```
