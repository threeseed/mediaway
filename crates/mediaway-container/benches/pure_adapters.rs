//! Criterion benches for the `mediaway-container` FLV and `WebM` facade
//! adapters (Mediaway `Packet`/`StreamInfo` over `flv-core` / `ebml-webm`).
//!
//! Path class: `pure` (CPU sans-IO, no device). Inputs are generated in
//! memory. These measure the adapter cost on top of the cores; compare with
//! the `flv-core` / `ebml-webm` benches for the core-only number. See
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
use mediaway_common::{Bytes, CodecKind, Packet, Rational, StreamInfo, VideoGeometry};
use mediaway_container::{flv, webm};

/// Video packets per iteration (10 s at 30 fps).
const FRAMES: usize = 300;
/// Payload bytes per packet.
const FRAME_BYTES: usize = 4096;

const AVC_C: &[u8] = &[
    1, 0x42, 0, 0x1e, 0xff, 0xe1, 0, 4, 0x67, 0x42, 0x00, 0x1e, 1, 0, 4, 0x68, 0xce, 0x06, 0xe2,
];

fn video_track(id: u32, codec: CodecKind, extra: &'static [u8]) -> StreamInfo {
    StreamInfo::Video {
        id,
        codec,
        time_base: Rational::new(1, 1000),
        geometry: VideoGeometry {
            width: 1920,
            height: 1080,
        },
        extra_data: Bytes::from_static(extra),
    }
}

/// AVCC-style packets (4-byte length prefix + body without start codes).
fn packets(stream_id: u32) -> Vec<Packet> {
    (0..FRAMES)
        .map(|i| {
            let mut p = Vec::with_capacity(FRAME_BYTES);
            p.extend_from_slice(&((FRAME_BYTES - 4) as u32).to_be_bytes());
            p.extend((0..FRAME_BYTES - 4).map(|j| ((i + j) % 250 + 1) as u8));
            Packet {
                stream_id,
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

fn flv_mux(pkts: &[Packet]) -> Vec<u8> {
    let mut mux = flv::Muxer::new();
    mux.add_track(&video_track(0, CodecKind::H264, AVC_C))
        .expect("track");
    let mut out = Vec::with_capacity(pkts.len() * (FRAME_BYTES + 32));
    mux.write_header(false, true, &mut out);
    for p in pkts {
        mux.push_packet(p, &mut out).expect("push");
    }
    out
}

fn webm_mux(pkts: &[Packet]) -> Vec<u8> {
    let mut open = webm::Muxer::new();
    let id = open
        .add_track(video_track(1, CodecKind::Vp9, &[]))
        .expect("track");
    let mut live = open.begin();
    for p in pkts {
        assert_eq!(p.stream_id, id);
        live.push_packet(p).expect("push");
    }
    live.flush();
    let mut out = Vec::new();
    live.poll_bytes(&mut out);
    out
}

fn bench_flv(c: &mut Criterion) {
    let pkts = packets(0);
    let file = flv_mux(&pkts);

    let mut g = c.benchmark_group("pure_container_flv");
    g.throughput(Throughput::Bytes((FRAMES * FRAME_BYTES) as u64));
    g.bench_function("pure_container_flv_mux_h264", |b| {
        b.iter(|| black_box(flv_mux(black_box(&pkts))));
    });
    g.throughput(Throughput::Bytes(file.len() as u64));
    g.bench_function("pure_container_flv_demux_h264", |b| {
        b.iter(|| {
            let mut demux = flv::Demuxer::new();
            demux.push_bytes(black_box(&file));
            let mut n = 0usize;
            while let Some(p) = demux.poll_packet() {
                n += p.payload.len();
            }
            assert_eq!(n, FRAMES * FRAME_BYTES);
            black_box(n)
        });
    });
    g.finish();
}

fn bench_webm(c: &mut Criterion) {
    let pkts = packets(1);
    let file = webm_mux(&pkts);

    let mut g = c.benchmark_group("pure_container_webm");
    g.throughput(Throughput::Bytes((FRAMES * FRAME_BYTES) as u64));
    g.bench_function("pure_container_webm_mux_vp9", |b| {
        b.iter(|| black_box(webm_mux(black_box(&pkts))));
    });
    g.throughput(Throughput::Bytes(file.len() as u64));
    g.bench_function("pure_container_webm_demux_vp9", |b| {
        b.iter(|| {
            let mut demux = webm::Demuxer::new();
            demux.push_bytes(black_box(&file));
            let mut n = 0usize;
            while let Some(p) = demux.poll_packet() {
                n += p.payload.len();
            }
            assert_eq!(n, FRAMES * FRAME_BYTES);
            black_box(n)
        });
    });
    g.finish();
}

criterion_group!(benches, bench_flv, bench_webm);
criterion_main!(benches);
