# ADR-0010: Zero-copy demux payloads and a sans-io seek request

- **Status**: Accepted
- **Date**: 2026-11-17
- **Deciders**: @dev-nyxie (+ agent)
- **Crate**: `iso-bmff`

## Context

`Demuxer` kept a `Vec<u8>` input buffer and copied every sample out of it (`to_vec`), then
compacted with `drain(..read_pos)` (a `memmove` of the unread tail). For an unfragmented file
with the `moov` at the end (what most streaming muxers and cameras write), push mode has no
choice but to keep every `mdat` in memory until the `moov` arrives: the whole file.

## Decision

1. **`BytesMut` input, `Bytes` slices out.** A complete top-level box is split off the front of
   the buffer (`split_to` + `freeze`, O(1), no `memmove`); samples are `Bytes::slice`s of it, and
   a retained `mdat` is that same `Bytes`. The one remaining copy is a protected sample being
   decrypted in place (`to_vec` before `decrypt_cenc`), documented on `set_decryption_key`.
2. **Seekable mode** (`Demuxer::new_seekable`). The core may ask the caller to continue from an
   absolute offset: `poll_seek() -> Option<u64>`, checked after each `push_bytes`. While a seek
   is outstanding, pushed bytes are dropped (the contract that keeps `file_base` honest).
   - Top-level walk: any box that is not `moov`/`moof` (or an `mdat` holding pending `trun`
     samples) and is not yet fully buffered is skipped with a seek past it.
   - After a `moov` with `stbl` samples: samples are sorted by file offset and read straight
     from the input, seeking only over gaps; then the walk resumes after the `moov`.
   - Push mode (`Demuxer::new`) is unchanged and never asks to seek.
3. **I/O stays in the facade:** `mediaway_container::mp4::SeekDemuxer<R: Read + Seek>` honors
   the requests.

## Alternatives Considered

| Alternative | Why not |
|-------------|---------|
| A separate "index" API (`parse_top_level` + caller-driven reads) | Duplicates the box walk in every adapter; the request/answer loop keeps one state machine. |
| `NeedRange { offset, len }` per sample | More precise but chatty for interleaved files; file-order reads with gap seeks cover it. |
| Keep `Vec` + `drain` | The per-sample copy and compaction are exactly the cost being removed. |

## Consequences

- Clear-sample payloads are zero-copy (`zc`, shared CPU buffer) relative to the pushed input; the
  push itself still copies a borrowed `&[u8]` once into the buffer.
- A retained/emitted `Bytes` keeps its whole box allocation alive until every slice is dropped.
- Seekable mode emits unfragmented samples in file order (interleaved tracks), not per track.
- `Demuxer` is no longer the place to buffer a moov-at-end file when the input can seek.

## References

- `src/demux/mod.rs`, `tests/moov_at_end.rs`; `mediaway-container/src/mp4_io.rs`.
