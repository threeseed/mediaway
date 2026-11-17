# mediaway-sw — benchmarks

Criterion benches; all inputs generated in memory (no media files).

```bash
cargo bench -p mediaway-sw --bench sw_pixels                      # colorconv + scaler
cargo bench -p mediaway-sw --bench sw_codecs --features h264      # H.264 I-frame + PCM
```

| Bench id | Class | What |
|----------|-------|------|
| `sw_colorconv_1080p/sw_i420_to_rgba_bt709` | `sw` | 1920x1080 I420 -> RGBA, BT.709 video range |
| `sw_colorconv_1080p/sw_nv12_to_i420` | `sw` | 1920x1080 NV12 -> I420 (copy + deinterleave) |
| `sw_scale_1080p_to_720p/sw_bilinear_plane_u8` | `sw` | one 8-bit plane, bilinear |
| `sw_scale_1080p_to_720p/sw_bilinear_rgba` | `sw` | RGBA, bilinear |
| `sw_h264/sw_h264_decode_i_frame_720p_synth` | `sw` | 1280x720 synthetic IDR: `I_PCM` top row + `I_16x16` DC/Plane, **no residual** |
| `pcm/zc_pcm_passthrough_push_poll_20ms` | `zc` | PCM passthrough (refcount `Bytes` share, no memcpy) |

`oracle_ref`: N/A — the H.264 bench decodes a partial-feature synthetic stream FFmpeg has
no equivalent path for; colorconv/scaler are pure CPU microbenches (`swscale` comparison
not set up yet).

## Latest (ad-hoc)

`machine_id: ad-hoc` — cloud VM, Intel Xeon @ 2.30GHz, 4 vCPU, Linux 6.18, Rust 1.98,
`--warm-up-time 1..2 --measurement-time 3..8`, single-threaded. Shared host: expect ~10%
noise. Commit: this change's branch.

| Bench | Median |
|-------|--------|
| `sw_i420_to_rgba_bt709` | 3.95 ms (~525 Mpx/s) |
| `sw_nv12_to_i420` | 0.50 ms |
| `sw_bilinear_plane_u8` | 1.51 ms |
| `sw_bilinear_rgba` | 4.83 ms |
| `sw_h264_decode_i_frame_720p_synth` | 2.52 ms (before hot-loop rewrite: 2.80 ms) |
| `zc_pcm_passthrough_push_poll_20ms` | 35 ns |
