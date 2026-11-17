//! Criterion benches for the `ogg-core` sans-IO core.
//!
//! Path class: `pure` (CPU sans-IO, no device). Inputs are generated in
//! memory (deterministic byte patterns, no committed media). Includes page
//! CRC-32 computation on write and verification on read. See
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
use ogg_core::{Demuxer, Muxer};

/// Packets per iteration (one page each).
const PACKETS: usize = 500;
/// Bytes per packet (~Opus-at-high-bitrate scale; well under one page).
const PACKET_BYTES: usize = 1024;

fn packets() -> Vec<Vec<u8>> {
    (0..PACKETS)
        .map(|i| (0..PACKET_BYTES).map(|j| ((i + j) % 251) as u8).collect())
        .collect()
}

fn mux_all(packets: &[Vec<u8>]) -> Vec<u8> {
    let mut mux = Muxer::new(0x00C0_FFEE);
    let mut out = Vec::with_capacity(packets.len() * (PACKET_BYTES + 64));
    for (i, p) in packets.iter().enumerate() {
        mux.push_packet(p, i as i64 * 960, i + 1 == packets.len(), &mut out)
            .expect("push_packet");
    }
    out
}

fn bench_ogg(c: &mut Criterion) {
    let packets = packets();
    let stream = mux_all(&packets);

    let mut g = c.benchmark_group("pure_ogg");
    g.throughput(Throughput::Bytes((PACKETS * PACKET_BYTES) as u64));
    g.bench_function("pure_ogg_page_write", |b| {
        b.iter(|| black_box(mux_all(black_box(&packets))));
    });
    g.throughput(Throughput::Bytes(stream.len() as u64));
    g.bench_function("pure_ogg_page_read", |b| {
        b.iter(|| {
            let mut demux = Demuxer::new();
            demux.push_bytes(black_box(&stream));
            let mut n = 0usize;
            while let Some(p) = demux.poll_packet().expect("poll") {
                n += p.data.len();
            }
            assert_eq!(n, PACKETS * PACKET_BYTES);
            black_box(n)
        });
    });
    g.finish();
}

criterion_group!(benches, bench_ogg);
criterion_main!(benches);
