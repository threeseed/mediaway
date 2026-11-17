# `rtp-core` benchmarks

Methodology: [`docs/conventions/benchmarking.md`](../../../docs/conventions/benchmarking.md) ·
machines: [`docs/benchmarks/machines.md`](../../../docs/benchmarks/machines.md).

Harness: Criterion, target `benches/pure_rtp.rs`. Path class: **`pure`** (CPU sans-IO, no device, no I/O).
Inputs are generated in memory by the bench (deterministic byte patterns); no media files are read or committed.
Throughput is reported as `Throughput::Bytes`.

## Run

```bash
cargo bench -p rtp-core --bench pure_rtp               # full run
cargo bench -p rtp-core --bench pure_rtp -- --quick    # smoke
```

## Benches

| Id | Workload | Bytes counted |
|----|----------|---------------|
| `pure_rtp_h264_packetize` | one 64 KiB H.264 NAL → FU-A at MTU 1200, incl. `RtpPacket::write` | NAL size |
| `pure_rtp_h264_depacketize` | `RtpPacket::parse` + reassembly of the packets above | NAL size |
| `pure_rtp_hevc_packetize` | one 64 KiB HEVC NAL → FU at MTU 1200, incl. write | NAL size |
| `pure_rtp_hevc_depacketize` | parse + reassembly | NAL size |

## Results

No published baseline yet. Add rows only with a registry `machine_id`
(`ref-*`, `ci-smoke`, or `ad-hoc` + full hardware block). `oracle_ref`: N/A —
these isolate the sans-IO core in-process; a system `ffmpeg` CLI run is not a
like-for-like comparison (process + I/O overhead dominate).
