//! Criterion benches for the `iso-bmff` sans-IO core.
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

use bytes::Bytes;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use iso_bmff::bitstream::{avcc_payload_to_annex_b, to_avcc};
use iso_bmff::{Codec, Demuxer, Muxer, Rational, Sample, Track};

/// Number of video samples per iteration (10 s at 30 fps).
const FRAMES: usize = 300;
/// Payload bytes per sample (excluding the 4-byte length prefix).
const FRAME_BYTES: usize = 4096;

const AVC_C: &[u8] = &[
    1, 0x42, 0, 0x1e, 0xff, 0xe1, 0, 4, 0x67, 0x42, 0x00, 0x1e, 1, 0, 4, 0x68, 0xce, 0x06, 0xe2,
];

/// NAL body bytes that never contain `00 00 01` (values cycle 1..=250).
fn nal_body(len: usize, seed: usize) -> impl Iterator<Item = u8> {
    (0..len).map(move |i| ((i + seed) % 250 + 1) as u8)
}

fn avcc_samples() -> Vec<Sample> {
    (0..FRAMES)
        .map(|i| {
            let mut p = Vec::with_capacity(FRAME_BYTES + 4);
            p.extend_from_slice(&(FRAME_BYTES as u32).to_be_bytes());
            p.push(if i % 30 == 0 { 0x65 } else { 0x41 });
            p.extend(nal_body(FRAME_BYTES - 1, i));
            Sample {
                stream_id: 0,
                pts: i as i64 * 33,
                dts: i as i64 * 33,
                duration: 33,
                is_keyframe: i % 30 == 0,
                is_discard: false,
                payload: Bytes::from(p),
            }
        })
        .collect()
}

fn track() -> Track {
    Track {
        id: 0,
        codec: Codec::H264,
        time_base: Rational::new(1, 1000),
        width: 1920,
        height: 1080,
        extra_data: Bytes::from_static(AVC_C),
    }
}

fn mux_all(samples: &[Sample]) -> Vec<u8> {
    let mut open = Muxer::new();
    open.add_track(track()).expect("track");
    let mut mux = open.begin();
    for s in samples {
        mux.push_packet(s).expect("push");
    }
    mux.flush();
    let mut out = Vec::new();
    mux.poll_bytes(&mut out);
    out
}

fn payload_bytes(samples: &[Sample]) -> u64 {
    samples.iter().map(|s| s.payload.len() as u64).sum()
}

fn bench_fmp4(c: &mut Criterion) {
    let samples = avcc_samples();
    let file = mux_all(&samples);

    let mut g = c.benchmark_group("pure_mp4");
    g.throughput(Throughput::Bytes(payload_bytes(&samples)));
    g.bench_function("pure_mp4_fmp4_mux_h264", |b| {
        b.iter(|| black_box(mux_all(black_box(&samples))));
    });
    g.throughput(Throughput::Bytes(file.len() as u64));
    g.bench_function("pure_mp4_fmp4_demux_h264", |b| {
        b.iter(|| {
            let mut demux = Demuxer::new();
            demux.push_bytes(black_box(&file));
            let mut n = 0usize;
            while let Some(s) = demux.poll_packet() {
                n += s.payload.len();
            }
            assert_eq!(n, FRAMES * (FRAME_BYTES + 4));
            black_box(n)
        });
    });
    g.finish();
}

fn bench_bitstream(c: &mut Criterion) {
    // One access unit: SPS + PPS + IDR slice in Annex-B, ~64 KiB.
    let mut annex_b = vec![
        0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1e, 0, 0, 0, 1, 0x68, 0xce, 0x06, 0xe2,
    ];
    annex_b.extend_from_slice(&[0, 0, 0, 1, 0x65]);
    annex_b.extend(nal_body(64 * 1024, 0));
    let avcc = to_avcc(&annex_b).payload;

    let mut g = c.benchmark_group("pure_bitstream");
    g.throughput(Throughput::Bytes(annex_b.len() as u64));
    g.bench_function("pure_annexb_to_avcc_h264", |b| {
        b.iter(|| black_box(to_avcc(black_box(&annex_b))));
    });
    g.throughput(Throughput::Bytes(avcc.len() as u64));
    g.bench_function("pure_avcc_to_annexb_h264", |b| {
        b.iter(|| black_box(avcc_payload_to_annex_b(black_box(&avcc), 4)));
    });
    g.finish();
}

criterion_group!(benches, bench_fmp4, bench_bitstream);
criterion_main!(benches);
