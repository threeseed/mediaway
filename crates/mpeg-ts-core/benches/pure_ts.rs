//! Criterion benches for the `mpeg-ts-core` sans-IO core.
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

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use mpeg_ts_core::{Demuxer, ElementaryStream, Muxer, StreamType};

/// Video access units per iteration (10 s at 30 fps).
const FRAMES: usize = 300;
/// Bytes per video access unit.
const FRAME_BYTES: usize = 4096;
const VIDEO_PID: u16 = 256;

fn access_units() -> Vec<Vec<u8>> {
    (0..FRAMES)
        .map(|i| {
            (0..FRAME_BYTES)
                .map(|j| ((i + j) % 250 + 1) as u8)
                .collect()
        })
        .collect()
}

fn mux_all(units: &[Vec<u8>]) -> Vec<u8> {
    let streams = [ElementaryStream {
        pid: VIDEO_PID,
        stream_type: StreamType::H264,
    }];
    let mut mux = Muxer::new(1, 4096, &streams).expect("mux");
    let mut out = Vec::with_capacity(units.len() * (FRAME_BYTES + 512));
    mux.write_pat_pmt(&mut out);
    for (i, unit) in units.iter().enumerate() {
        let pts = i as u64 * 3_000;
        mux.write_access_unit(VIDEO_PID, unit, pts, Some(pts), i % 30 == 0, &mut out)
            .expect("access unit");
    }
    out
}

fn bench_ts(c: &mut Criterion) {
    let units = access_units();
    let stream = mux_all(&units);

    let mut g = c.benchmark_group("pure_ts");
    g.throughput(Throughput::Bytes((FRAMES * FRAME_BYTES) as u64));
    g.bench_function("pure_ts_mux_h264", |b| {
        b.iter(|| black_box(mux_all(black_box(&units))));
    });
    g.throughput(Throughput::Bytes(stream.len() as u64));
    g.bench_function("pure_ts_demux_h264", |b| {
        b.iter(|| {
            let mut demux = Demuxer::new();
            demux.push_bytes(black_box(&stream));
            let mut n = 0usize;
            while let Some(unit) = demux.poll_access_unit().expect("poll") {
                n += unit.data.len();
            }
            for unit in demux.finish() {
                n += unit.data.len();
            }
            assert_eq!(n, FRAMES * FRAME_BYTES);
            black_box(n)
        });
    });
    g.finish();
}

criterion_group!(benches, bench_ts);
criterion_main!(benches);
