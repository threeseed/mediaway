# Benchmarking

Canonical: [`docs/conventions/benchmarking.md`](../../../conventions/benchmarking.md).

- Label paths: `zc` · `copy` · `readback` · `sw` · `pure`
- **`zc`** = GPU-resident **or** shared CPU (no payload memcpy) — [marks](../zero-copy/marks.md)
- **`machine_id`** on every published row — [`machines.md`](../../../benchmarks/machines.md)
- **`oracle_ref`** beside Mediaway when comparable; fair timing + **same class** (`zc` vs `zc`)
- Official baselines only from `ref-*` profiles
- Default CI stays light; HW benches optional
- Never sell a copy path as Zero-Copy in bench names or claims

## Existing `pure` Criterion harnesses

All inputs generated in memory (no media blobs); `Throughput::Bytes`; ids prefixed `pure_`.

| Crate | Bench target | Covers |
|-------|--------------|--------|
| `iso-bmff` | `pure_mp4` | fMP4 H.264 mux/demux · Annex-B ↔ AVCC |
| `mpeg-ts-core` | `pure_ts` | TS mux/demux (single H.264 PID) |
| `rtp-core` | `pure_rtp` | H.264/HEVC packetize (+write) / depacketize (+parse), FU path |
| `ebml-webm` | `pure_webm` | WebM VP9 mux/demux |
| `ogg-core` | `pure_ogg` | page write/read (incl. CRC) |
| `flv-core` / `adts-core` | `pure_flv` / `pure_adts` | tag / frame parse |
| `mediaway-container` | `pure_adapters` | FLV + WebM facade adapters over the cores |

- Run one target with args: `cargo bench -p <crate> --bench <target> -- --quick`. Without `--bench`, Criterion flags reach the lib's libtest harness and fail (`Unrecognized option`).
- Per-crate notes: `crates/<crate>/docs/benchmarks.md` (no numbers until a `machine_id` run is recorded).
- `bench-daily` picks these up automatically (any `[[bench]]` → `cargo bench --workspace`).
