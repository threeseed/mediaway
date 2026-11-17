//! Criterion benches for the CPU (`sw`) colour conversion and bilinear scaler.
//!
//! Path class: `sw` (pure CPU, no GPU). Input is generated in memory (no media files).
//! Run: `cargo bench -p mediaway-sw --bench sw_pixels`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    missing_docs,
    clippy::many_single_char_names,
    reason = "bench harness"
)]

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use mediaway_common::ColorRange;
use mediaway_sw::colorconv::{
    I420Mut, I420Ref, Nv12Ref, RgbOrder, YuvMatrix, YuvToRgb, i420_to_rgb, nv12_to_i420,
};
use mediaway_sw::scale::BilinearPlan;

const W: usize = 1920;
const H: usize = 1080;
const CW: usize = W / 2;
const CH: usize = H / 2;

fn pattern(n: usize, mul: usize) -> Vec<u8> {
    (0..n).map(|i| ((i * mul) >> 3) as u8).collect()
}

fn colorconv(c: &mut Criterion) {
    let y = pattern(W * H, 7);
    let u = pattern(CW * CH, 3);
    let v = pattern(CW * CH, 5);
    let uv = pattern(CW * 2 * CH, 11);
    let src = I420Ref {
        y: &y,
        y_stride: W,
        u: &u,
        u_stride: CW,
        v: &v,
        v_stride: CW,
    };
    let conv = YuvToRgb::new(YuvMatrix::Bt709, ColorRange::Video);
    let mut rgba = vec![0u8; W * H * 4];

    let mut g = c.benchmark_group("sw_colorconv_1080p");
    g.throughput(Throughput::Elements((W * H) as u64));
    g.bench_function("sw_i420_to_rgba_bt709", |b| {
        b.iter(|| {
            i420_to_rgb(
                &conv,
                black_box(&src),
                &mut rgba,
                W * 4,
                W,
                H,
                RgbOrder::Rgba,
            )
            .unwrap();
        });
    });
    let (mut yo, mut uo, mut vo) = (vec![0u8; W * H], vec![0u8; CW * CH], vec![0u8; CW * CH]);
    let nv12 = Nv12Ref {
        y: &y,
        y_stride: W,
        uv: &uv,
        uv_stride: CW * 2,
    };
    g.bench_function("sw_nv12_to_i420", |b| {
        b.iter(|| {
            let mut dst = I420Mut {
                y: &mut yo,
                y_stride: W,
                u: &mut uo,
                u_stride: CW,
                v: &mut vo,
                v_stride: CW,
            };
            nv12_to_i420(black_box(&nv12), &mut dst, W, H).unwrap();
        });
    });
    g.finish();
}

fn scaler(c: &mut Criterion) {
    let (dw, dh) = (1280usize, 720usize);
    let luma = pattern(W * H, 7);
    let rgba = pattern(W * H * 4, 13);
    let plan1 = BilinearPlan::<1>::new(W as u32, H as u32, dw as u32, dh as u32).unwrap();
    let plan4 = BilinearPlan::<4>::new(W as u32, H as u32, dw as u32, dh as u32).unwrap();
    let mut out1 = vec![0u8; dw * dh];
    let mut out4 = vec![0u8; dw * dh * 4];

    let mut g = c.benchmark_group("sw_scale_1080p_to_720p");
    g.throughput(Throughput::Elements((dw * dh) as u64));
    g.bench_function("sw_bilinear_plane_u8", |b| {
        b.iter(|| plan1.scale(black_box(&luma), W, &mut out1, dw).unwrap());
    });
    g.bench_function("sw_bilinear_rgba", |b| {
        b.iter(|| {
            plan4
                .scale(black_box(&rgba), W * 4, &mut out4, dw * 4)
                .unwrap();
        });
    });
    g.finish();
}

criterion_group!(benches, colorconv, scaler);
criterion_main!(benches);
