//! Criterion benches for the `rtp-core` sans-IO core.
//!
//! Path class: `pure` (CPU sans-IO, no device, no sockets). Inputs are
//! generated in memory. Packetize benches include `RtpPacket::write` to wire
//! bytes; depacketize benches include `RtpPacket::parse`. See
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
use rtp_core::{RtpPacket, h264, hevc};

/// NAL size: one large IDR slice forcing FU-A / FU fragmentation at MTU 1200.
const NAL_BYTES: usize = 64 * 1024;
const MTU: usize = 1200;

fn nal(header: &[u8]) -> Vec<u8> {
    let mut v = header.to_vec();
    v.extend((0..NAL_BYTES).map(|i| (i % 250 + 1) as u8));
    v
}

fn to_wire(packets: &[RtpPacket]) -> Vec<Vec<u8>> {
    packets
        .iter()
        .map(|p| {
            let mut w = Vec::new();
            p.write(&mut w).expect("write");
            w
        })
        .collect()
}

fn bench_h264(c: &mut Criterion) {
    let nal = nal(&[0x65]);
    let mut pk = h264::Packetizer::new(MTU, 96, 0x1234_5678, 1).expect("packetizer");
    let wire = to_wire(&pk.packetize(&nal, 90_000, true).expect("packetize"));

    let mut g = c.benchmark_group("pure_rtp_h264");
    g.throughput(Throughput::Bytes(nal.len() as u64));
    g.bench_function("pure_rtp_h264_packetize", |b| {
        b.iter(|| {
            let packets = pk
                .packetize(black_box(&nal), 90_000, true)
                .expect("packetize");
            let mut w = Vec::with_capacity(MTU);
            for p in &packets {
                w.clear();
                p.write(&mut w).expect("write");
                black_box(&w);
            }
        });
    });
    g.bench_function("pure_rtp_h264_depacketize", |b| {
        b.iter(|| {
            let mut depk = h264::Depacketizer::new();
            let mut out = None;
            for w in &wire {
                let p = RtpPacket::parse(black_box(w)).expect("parse");
                if let Some(n) = depk.depacketize(&p.payload).expect("depacketize") {
                    out = Some(n);
                }
            }
            assert_eq!(out.expect("nal").len(), nal.len());
        });
    });
    g.finish();
}

fn bench_hevc(c: &mut Criterion) {
    let nal = nal(&[0x26, 0x01]);
    let mut pk = hevc::Packetizer::new(MTU, 98, 0x1234_5679, 1).expect("packetizer");
    let wire = to_wire(&pk.packetize(&nal, 90_000, true).expect("packetize"));

    let mut g = c.benchmark_group("pure_rtp_hevc");
    g.throughput(Throughput::Bytes(nal.len() as u64));
    g.bench_function("pure_rtp_hevc_packetize", |b| {
        b.iter(|| {
            let packets = pk
                .packetize(black_box(&nal), 90_000, true)
                .expect("packetize");
            let mut w = Vec::with_capacity(MTU);
            for p in &packets {
                w.clear();
                p.write(&mut w).expect("write");
                black_box(&w);
            }
        });
    });
    g.bench_function("pure_rtp_hevc_depacketize", |b| {
        b.iter(|| {
            let mut depk = hevc::Depacketizer::new();
            let mut out = None;
            for w in &wire {
                let p = RtpPacket::parse(black_box(w)).expect("parse");
                if let Some(n) = depk.depacketize(&p.payload).expect("depacketize") {
                    out = Some(n);
                }
            }
            assert_eq!(out.expect("nal").len(), nal.len());
        });
    });
    g.finish();
}

criterion_group!(benches, bench_h264, bench_hevc);
criterion_main!(benches);
