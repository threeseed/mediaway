# CPU buffer pool and mux copy discipline

## `mediaway_common::BufferPool` (ADR-0001 in `crates/mediaway-common/adr/`)

Single-owner pool of `BytesMut` — `&mut self`, no lock, no `Arc`, no `dyn`.

- `take()` → cleared buffer, capacity ≥ `buf_capacity` (allocates on a miss).
- `recycle(BytesMut)` → cleared and kept unless too small or pool full.
- `recycle_bytes(Bytes) -> bool` → reclaimed only if the allocation is unique
  (`Bytes::try_into_mut`); any live clone/slice means it is dropped instead.
- Contents are **not zeroed** on recycle. Unreturned buffers are just freed.
- Not wired into any crate yet: every candidate (RTP packetize, TS/WebM mux)
  would need a public signature change to hand buffers back. Thread-shared
  pooling is deferred (would need a lock or channel; see the ADR).

## Single-copy patterns used in mux / packetize cores

| Crate | Pattern |
|-------|---------|
| `mpeg-ts-core` | `write_ts_packets_split(head, body)`: PES header (stack `SmallVec<[u8;19]>`) + AU written straight into 188-byte packets; PSI `pointer_field` passed as a 1-byte head |
| `rtp-core` | `OutgoingPacket { header, prefix: PayloadPrefix (≤3 inline bytes), body: Bytes }` from `packetize_bytes` — body is a `Bytes::slice` of the NAL; `write_into(&mut [u8])` serializes with no allocation. `packetize(&[u8])` copies the NAL once into one shared buffer (all fragments slice it) |
| `ebml-webm` | Muxer-owned `scratch` holds `SimpleBlock` header / `Cluster` `Timecode`; size is computed from `header + payload`, payload copied once |
| `mediaway-container::flv` | Demux payloads/extra data are `tag.data.slice(..)` |

All four are covered by tests that compare against the old (copying)
implementation kept verbatim in the sibling `*_tests.rs` — output must stay
byte-identical.
