//! Criterion benches for the `adts-core` sans-IO core.
//!
//! Path class: `pure` (CPU sans-IO, no device). Inputs are generated in
//! memory (deterministic byte patterns, no committed media). See
//! `docs/benchmarks.md` in this crate and `docs/conventions/benchmarking.md`.

#![forbid(unsafe_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_const_for_fn,
    missing_docs,
    clippy::cast_possible_truncation,
    reason = "bench harness: generated inputs, panics are acceptable"
)]

use std::hint::black_box;

use adts_core::{AacProfile, AdtsConfig, Demuxer, Muxer};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};

/// AAC frames per iteration (~23 s of 48 kHz audio).
const FRAMES: usize = 1000;
/// Raw AAC bytes per frame (~128 kbit/s stereo scale).
const FRAME_BYTES: usize = 384;

fn build_adts() -> Vec<u8> {
    let mux = Muxer::new(AdtsConfig {
        profile: AacProfile::Lc,
        sample_rate: 48_000,
        channels: 2,
    })
    .expect("mux");
    let mut out = Vec::new();
    for i in 0..FRAMES {
        let raw: Vec<u8> = (0..FRAME_BYTES).map(|j| ((i + j) % 251) as u8).collect();
        mux.write_frame(&raw, &mut out).expect("write_frame");
    }
    out
}

fn bench_adts(c: &mut Criterion) {
    let stream = build_adts();

    let mut g = c.benchmark_group("pure_adts");
    g.throughput(Throughput::Bytes(stream.len() as u64));
    g.bench_function("pure_adts_parse", |b| {
        b.iter(|| {
            let mut demux = Demuxer::new();
            demux.push_bytes(black_box(&stream));
            let mut n = 0usize;
            while let Some(f) = demux.poll_frame().expect("poll_frame") {
                n += f.len();
            }
            assert_eq!(n, FRAMES * FRAME_BYTES);
            black_box(n)
        });
    });
    g.finish();
}

criterion_group!(benches, bench_adts);
criterion_main!(benches);
