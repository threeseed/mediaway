//! Criterion benches for the `flv-core` sans-IO core.
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

use bytes::Bytes;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use flv_core::{Demuxer, Muxer, Tag, TagType};

/// Tags per iteration (video + audio interleaved).
const TAGS: usize = 600;
/// Bytes per tag body.
const TAG_BYTES: usize = 2048;

fn build_flv() -> (Vec<u8>, u64) {
    let mut mux = Muxer::new();
    let mut out = Vec::new();
    mux.write_header(true, true, &mut out);
    let mut body_bytes = 0u64;
    for i in 0..TAGS {
        let video = i % 2 == 0;
        let mut data = vec![if video { 0x27 } else { 0xAF }, 1];
        data.extend((0..TAG_BYTES - 2).map(|j| ((i + j) % 251) as u8));
        body_bytes += data.len() as u64;
        let tag = Tag {
            tag_type: if video {
                TagType::Video
            } else {
                TagType::Audio
            },
            timestamp_ms: (i as u32) * 16,
            data: Bytes::from(data),
        };
        mux.write_tag(&tag, &mut out).expect("write_tag");
    }
    (out, body_bytes)
}

fn bench_flv(c: &mut Criterion) {
    let (file, body_bytes) = build_flv();

    let mut g = c.benchmark_group("pure_flv");
    g.throughput(Throughput::Bytes(file.len() as u64));
    g.bench_function("pure_flv_tag_parse", |b| {
        b.iter(|| {
            let mut demux = Demuxer::new();
            demux.push_bytes(black_box(&file));
            let mut n = 0u64;
            while let Some(tag) = demux.poll_tag().expect("poll_tag") {
                n += tag.data.len() as u64;
            }
            assert_eq!(n, body_bytes);
            black_box(n)
        });
    });
    g.finish();
}

criterion_group!(benches, bench_flv);
criterion_main!(benches);
