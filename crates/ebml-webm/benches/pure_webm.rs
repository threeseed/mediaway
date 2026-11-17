//! Criterion benches for the `ebml-webm` sans-IO core.
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
    clippy::cast_possible_wrap,
    reason = "bench harness: generated inputs, panics are acceptable"
)]

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use ebml_webm::{Demuxer, Muxer, TrackInfo};

/// Video frames per iteration (10 s at 30 fps).
const FRAMES: usize = 300;
/// Bytes per frame.
const FRAME_BYTES: usize = 4096;

fn frames() -> Vec<Vec<u8>> {
    (0..FRAMES)
        .map(|i| (0..FRAME_BYTES).map(|j| ((i + j) % 251) as u8).collect())
        .collect()
}

fn mux_all(frames: &[Vec<u8>]) -> Vec<u8> {
    let mut open = Muxer::new();
    open.add_track(TrackInfo {
        track_number: 1,
        track_type: 1,
        codec_id: "V_VP9".to_owned(),
        codec_private: None,
        width: 1920,
        height: 1080,
        sample_rate: 8000.0,
        channels: 1,
    })
    .expect("track");
    let mut live = open.begin();
    for (i, f) in frames.iter().enumerate() {
        live.push_frame(1, i as i64 * 33, i % 30 == 0, f)
            .expect("frame");
    }
    live.flush();
    let mut out = Vec::new();
    live.poll_bytes(&mut out);
    out
}

fn bench_webm(c: &mut Criterion) {
    let frames = frames();
    let file = mux_all(&frames);

    let mut g = c.benchmark_group("pure_webm");
    g.throughput(Throughput::Bytes((FRAMES * FRAME_BYTES) as u64));
    g.bench_function("pure_webm_mux_vp9", |b| {
        b.iter(|| black_box(mux_all(black_box(&frames))));
    });
    g.throughput(Throughput::Bytes(file.len() as u64));
    g.bench_function("pure_webm_demux_vp9", |b| {
        b.iter(|| {
            let mut demux = Demuxer::new();
            demux.push_bytes(black_box(&file));
            let mut n = 0usize;
            while let Some(f) = demux.poll_frame() {
                n += f.payload.len();
            }
            assert_eq!(n, FRAMES * FRAME_BYTES);
            black_box(n)
        });
    });
    g.finish();
}

criterion_group!(benches, bench_webm);
criterion_main!(benches);
