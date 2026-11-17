# ADR-0005: CPU colour conversion and bilinear scaler (`sw` path), autovectorized

- **Status**: Accepted
- **Date**: 2026-11-17
- **Deciders**: @dev-nyxie (+ agent)
- **Crate**: `mediaway-sw`

## Context

The workspace had no CPU pixel-format conversion or scaling. Software decode
(`h264::decode_i_frame` emits I420), CPU capture readbacks and tests need NV12 <-> I420,
YUV -> RGBA/BGRA for display/upload, and resizing. GPU/platform converters exist only where a
platform backend provides them. Workspace rules: no C deps (no libyuv / swscale), ZCA
(minimize `Box`, no per-frame allocation), honest cost naming (ADR-0006), and the
autovectorization-first stance in `docs/conventions/code-style.md`.

## Decision

> Add two always-compiled, dependency-free modules: `colorconv` and `scale`.

- `colorconv`: `nv12_to_i420`, `i420_to_nv12`, `i420_to_rgb`, `nv12_to_rgb` over borrowed
  strided planes (`I420Ref`/`I420Mut`/`Nv12Ref`/`Nv12Mut`); `YuvToRgb::new(YuvMatrix,
  ColorRange)` precomputes Q16 fixed-point coefficients for BT.601 / BT.709, video or full
  range; `RgbOrder::{Rgba, Bgra}` selects byte order via a const generic (no per-pixel branch).
  Chroma upsampling is nearest (sample replicate), documented.
- `scale`: `BilinearPlan<CHANNELS>` (1 = Y/U/V plane, 2 = NV12 UV, 4 = RGBA/BGRA) precomputes
  per-axis taps once (`Vec` at plan time only); pixel-centre aligned, 8-bit weights, exact
  copy at 1:1. No area averaging for >2x downscale (documented aliasing).
- **Row-based API** (`YuvToRgb::{i420_row, nv12_row}`, `deinterleave_uv_row`,
  `interleave_uv_row`, `BilinearPlan::{source_rows, scale_row}`): every output row is
  independent, so callers parallelize (e.g. rayon over destination rows) without this crate
  depending on a thread pool.
- **No `rayon` feature:** `rayon` is not a workspace dependency (only transitive via `rav1e`),
  and the row API already gives callers the parallelism hook; adding a pool dependency to a
  sans-io codec crate is not justified under `docs/conventions/deps-policy.md`.
- **No intrinsics:** fixed-width chunks (`as_chunks`), integer math, hoisted bounds checks;
  autovectorization only until a measurement says otherwise.
- Errors: `ColorConvError` (`thiserror`, non-exhaustive) — geometry/stride/size validated once
  per frame; row functions truncate on short slices instead of panicking.

## Alternatives Considered

| Alternative | Why not |
|-------------|---------|
| libyuv / swscale FFI | C dependency; swscale is LGPL (forbidden) |
| `yuv` / `dcv-color-primitives` crates | New dependency + `unsafe` SIMD trees for a small, testable surface |
| Hand-written `std::arch` SIMD | Repo policy: autovectorization first; no measurement yet showing need |
| `f32` math | Slower to vectorize on narrow targets; non-deterministic rounding across targets |
| Optional `rayon` feature | Not a workspace dep; row API already allows external parallelism |

## Consequences

### Positive

- CPU-only consumers get conversions/scaling with zero new dependencies.
- Deterministic integer output (fixed-point matches a float reference within +/-1).

### Negative / Trade-offs

- `sw` cost class: full CPU passes; not a Zero-Copy path. GPU frames should use GPU converters.
- Nearest chroma upsampling and plain bilinear (no area filter) trade quality for speed.
- No 10-bit / P010 / YUYV input yet.

## References

- `src/colorconv.rs`, `src/scale.rs`, `benches/sw_pixels.rs`, `docs/benchmarks.md`
- Workspace ADR-0006 (costly paths), ADR-0009 (ZCA)
- `docs/ai/wiki/decode/sw-cpu-pixels.md`
