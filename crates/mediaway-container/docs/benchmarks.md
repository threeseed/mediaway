# `mediaway-container` benchmarks

Methodology: [`docs/conventions/benchmarking.md`](../../../docs/conventions/benchmarking.md) ·
machines: [`docs/benchmarks/machines.md`](../../../docs/benchmarks/machines.md).

Harness: Criterion, target `benches/pure_adapters.rs`. Path class: **`pure`** (CPU sans-IO, no device, no I/O).
Inputs are generated in memory by the bench (deterministic byte patterns); no media files are read or committed.
Throughput is reported as `Throughput::Bytes`.

## Run

```bash
cargo bench -p mediaway-container --bench pure_adapters               # full run
cargo bench -p mediaway-container --bench pure_adapters -- --quick    # smoke
```

## Benches

| Id | Workload | Bytes counted |
|----|----------|---------------|
| `pure_container_flv_mux_h264` | `flv::Muxer` adapter: 300 H.264 `Packet`s × 4 KiB | packet payloads |
| `pure_container_flv_demux_h264` | `flv::Demuxer` adapter: drain all `Packet`s | file size |
| `pure_container_webm_mux_vp9` | `webm::Muxer` adapter: 300 VP9 `Packet`s × 4 KiB | packet payloads |
| `pure_container_webm_demux_vp9` | `webm::Demuxer` adapter: drain all `Packet`s | file size |

## Results

No published baseline yet. Add rows only with a registry `machine_id`
(`ref-*`, `ci-smoke`, or `ad-hoc` + full hardware block). `oracle_ref`: N/A —
these isolate the sans-IO core in-process; a system `ffmpeg` CLI run is not a
like-for-like comparison (process + I/O overhead dominate).
