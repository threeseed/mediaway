# `flv-core` benchmarks

Methodology: [`docs/conventions/benchmarking.md`](../../../docs/conventions/benchmarking.md) ·
machines: [`docs/benchmarks/machines.md`](../../../docs/benchmarks/machines.md).

Harness: Criterion, target `benches/pure_flv.rs`. Path class: **`pure`** (CPU sans-IO, no device, no I/O).
Inputs are generated in memory by the bench (deterministic byte patterns); no media files are read or committed.
Throughput is reported as `Throughput::Bytes`.

## Run

```bash
cargo bench -p flv-core --bench pure_flv               # full run
cargo bench -p flv-core --bench pure_flv -- --quick    # smoke
```

## Benches

| Id | Workload | Bytes counted |
|----|----------|---------------|
| `pure_flv_tag_parse` | parse 600 interleaved audio/video tags × 2 KiB | file size |

## Results

No published baseline yet. Add rows only with a registry `machine_id`
(`ref-*`, `ci-smoke`, or `ad-hoc` + full hardware block). `oracle_ref`: N/A —
these isolate the sans-IO core in-process; a system `ffmpeg` CLI run is not a
like-for-like comparison (process + I/O overhead dominate).
