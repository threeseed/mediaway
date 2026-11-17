# `iso-bmff` benchmarks

Methodology: [`docs/conventions/benchmarking.md`](../../../docs/conventions/benchmarking.md) ·
machines: [`docs/benchmarks/machines.md`](../../../docs/benchmarks/machines.md).

Harness: Criterion, target `benches/pure_mp4.rs`. Path class: **`pure`** (CPU sans-IO, no device, no I/O).
Inputs are generated in memory by the bench (deterministic byte patterns); no media files are read or committed.
Throughput is reported as `Throughput::Bytes`.

## Run

```bash
cargo bench -p iso-bmff --bench pure_mp4               # full run
cargo bench -p iso-bmff --bench pure_mp4 -- --quick    # smoke
```

## Benches

| Id | Workload | Bytes counted |
|----|----------|---------------|
| `pure_mp4_fmp4_mux_h264` | 300 H.264 AVCC samples × 4 KiB → fragmented MP4 (default batch) | sample payloads |
| `pure_mp4_fmp4_demux_h264` | demux the file above, drain all samples | file size |
| `pure_annexb_to_avcc_h264` | `bitstream::to_avcc` on one ~64 KiB SPS+PPS+IDR access unit | input size |
| `pure_avcc_to_annexb_h264` | `bitstream::avcc_payload_to_annex_b` on the AVCC result | input size |

## Results

No published baseline yet. Add rows only with a registry `machine_id`
(`ref-*`, `ci-smoke`, or `ad-hoc` + full hardware block). `oracle_ref`: N/A —
these isolate the sans-IO core in-process; a system `ffmpeg` CLI run is not a
like-for-like comparison (process + I/O overhead dominate).
