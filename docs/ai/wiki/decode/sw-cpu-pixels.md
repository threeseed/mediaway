# mediaway-sw CPU pixels: colorconv, scaler, H.264 hot loops

Crate ADR: [`crates/mediaway-sw/adr/0005-cpu-colorconv-and-scaler.md`](../../../../crates/mediaway-sw/adr/0005-cpu-colorconv-and-scaler.md).
Benches: [`crates/mediaway-sw/docs/benchmarks.md`](../../../../crates/mediaway-sw/docs/benchmarks.md).

## Surfaces (path class `sw`, always compiled, no deps)

| Module | Entry points |
|--------|--------------|
| `mediaway_sw::colorconv` | `nv12_to_i420`, `i420_to_nv12`, `i420_to_rgb`, `nv12_to_rgb`; row fns `YuvToRgb::{i420_row, nv12_row}`, `deinterleave_uv_row`, `interleave_uv_row` |
| `mediaway_sw::scale` | `BilinearPlan<CHANNELS>::{new, scale, scale_row, source_rows}` (1 = plane, 2 = NV12 UV, 4 = RGBA) |

- `YuvToRgb::new(YuvMatrix::{Bt601,Bt709}, ColorRange)` — Q16 fixed point; within +/-1 of a
  float reference (tested over random samples). Nearest chroma upsampling.
- Callers own every buffer; frame helpers validate stride/size once, rows never panic.
- Parallelism is the caller's: rows are independent. No `rayon` feature (not a workspace dep).
- Bilinear: centre-aligned, 8-bit weights, exact at 1:1, no area filter (>2x downscale aliases).

## H.264 decode hot loops (bit-exact rewrite)

- `transform.rs` dequant: all-zero block -> early return (most blocks); else shift resolved
  once per block, `i32` lanes when `|level| < 2^18` and QP in range, `i64` + one range check
  otherwise. Errors identical to the old per-coefficient checked math (oracle test).
- Butterfly selector is a const generic (`hadamard_like_pass::<HADAMARD>`).
- `reconstruct.rs`: `as_chunks` row views for block writes / residual add; I_PCM reads rows.
- `intra_pred.rs`: Plane mode is row-incremental (`plane_fill`), chroma DC fills rows.
- Lesson: a fully branch-free i64 dequant was ~30% **slower** on a zero-residual stream than
  the old zero-skip — keep the sparse fast path; measure before assuming "branch-free wins".

## Gaps

- `pcm.rs` has no sample conversions (pure `Bytes` passthrough) — nothing to vectorize.
- No inter prediction / deblocking exists yet in `h264`.
- H.264 bench stream has no residual coefficients (CAVLC level writing not synthesized).
- 10-bit / P010 / YUYV conversion not implemented.
