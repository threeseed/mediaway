# ADR-0011: Faststart — relocate a trailing `moov`

- **Status**: Accepted
- **Date**: 2026-11-17
- **Deciders**: @dev-nyxie (+ agent)
- **Crate**: `iso-bmff`

## Context

`write_progressive` (ADR-0008) already writes `moov` first when every sample is known up front.
Files from other writers (or a streaming progressive writer) end with the `moov`; a player must
fetch the end of the file before the first frame. Moving the `moov` shifts every byte after the
insertion point, so every chunk offset must change.

## Decision

- **Sans-io core** (`isobmff::faststart`): `plan_faststart(top_level_boxes, moov_bytes)` returns a
  `FaststartPlan` (rewritten `moov`, insertion offset, removed range) or `None` when the file is
  already faststart. `relocate_moov` rebuilds `moov/trak/mdia/minf/stbl` and rewrites `stco`/`co64`;
  other boxes are copied verbatim. Offsets before the old `moov` move by the new `moov` length,
  offsets after it by `new - old`.
- **Widening:** a shifted `stco` past 4 GiB becomes `co64`, which lengthens the `moov`; the shift is
  recomputed until the length is stable (at most a few passes).
- `faststart(&[u8])` is a whole-buffer convenience (full in-memory copy — documented).
- **Adapter:** `mediaway_container::mp4::faststart(Read + Seek, Write)` walks headers with seeks,
  reads only the `moov`, and streams the other ranges with `io::copy`.

## Alternatives Considered

| Alternative | Why not |
|-------------|---------|
| Reserve space up front (`free` box) and rewrite in place | Needs a size guess at mux time; complementary, not a fix for existing files. |
| Rewrite via `parse_moov` + `write_indexed_moov` | Loses boxes the writer does not model (`udta`, `sgpd`, ...). |

## Consequences

- Fragmented files (moov already first, `moof` offsets relative) are left unchanged.
- Offsets in boxes other than `stco`/`co64` (e.g. `saio`, `iloc`) are not rewritten; such files
  (CENC with `saio` in `stbl`, HEIF) are out of scope.
- The output is a second file; in-place relocation is the caller's choice.

## References

- `src/isobmff/faststart.rs`, `tests/moov_at_end.rs`; `mediaway-container/src/mp4_io.rs`.
