#![cfg(test)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::many_single_char_names,
    clippy::suboptimal_flops,
    reason = "test modules may unwrap; generated test data uses small casts"
)]

use super::*;

/// Deterministic pseudo-random bytes (xorshift), so tests need no committed media.
fn gen_bytes(n: usize, mut seed: u32) -> Vec<u8> {
    (0..n)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed >> 24) as u8
        })
        .collect()
}

/// Floating-point reference conversion straight from the matrix definition.
fn float_ref(matrix: YuvMatrix, range: ColorRange, y: u8, u: u8, v: u8) -> [u8; 3] {
    let (kr, kb) = match matrix {
        YuvMatrix::Bt601 => (0.299, 0.114),
        YuvMatrix::Bt709 => (0.2126, 0.0722),
    };
    let kg = 1.0 - kr - kb;
    let (yf, uf, vf) = match range {
        ColorRange::Full => (f64::from(y), f64::from(u) - 128.0, f64::from(v) - 128.0),
        _ => (
            (f64::from(y) - 16.0) * 255.0 / 219.0,
            (f64::from(u) - 128.0) * 255.0 / 224.0,
            (f64::from(v) - 128.0) * 255.0 / 224.0,
        ),
    };
    let r = yf + 2.0 * (1.0 - kr) * vf;
    let b = yf + 2.0 * (1.0 - kb) * uf;
    let g = (yf - kr * r - kb * b) / kg;
    [r, g, b].map(|c| c.round().clamp(0.0, 255.0) as u8)
}

#[test]
fn bt601_video_range_reference_values() {
    let conv = YuvToRgb::new(YuvMatrix::Bt601, ColorRange::Video);
    let mut out = [0u8; 8];
    // White / black / mid grey.
    conv.i420_row(&[235, 16], &[128], &[128], &mut out, 2, RgbOrder::Rgba);
    assert_eq!(out, [255, 255, 255, 255, 0, 0, 0, 255]);
    conv.i420_row(&[126, 126], &[128], &[128], &mut out, 2, RgbOrder::Rgba);
    assert_eq!(out[..4], [128, 128, 128, 255]);
    // 75% colour bars red (BT.601 video range): Y=65 U=100 V=212 -> ~ (191, 0, 0).
    conv.i420_row(&[65, 65], &[100], &[212], &mut out, 2, RgbOrder::Rgba);
    assert_eq!(out[3], 255);
    assert!(out[0] >= 189 && out[1] <= 2 && out[2] <= 2, "{out:?}");
}

#[test]
fn full_range_grey_is_identity() {
    for matrix in [YuvMatrix::Bt601, YuvMatrix::Bt709] {
        let conv = YuvToRgb::new(matrix, ColorRange::Full);
        let y: Vec<u8> = (0..=255).collect();
        let c = vec![128u8; 128];
        let mut out = vec![0u8; 256 * 4];
        conv.i420_row(&y, &c, &c, &mut out, 256, RgbOrder::Bgra);
        for (i, px) in out.as_chunks::<4>().0.iter().enumerate() {
            assert_eq!(*px, [i as u8, i as u8, i as u8, 255], "{matrix:?} y={i}");
        }
    }
}

#[test]
fn fixed_point_matches_float_reference_within_one() {
    let samples = gen_bytes(3 * 20_000, 7);
    for matrix in [YuvMatrix::Bt601, YuvMatrix::Bt709] {
        for range in [ColorRange::Video, ColorRange::Full] {
            let conv = YuvToRgb::new(matrix, range);
            for s in samples.as_chunks::<3>().0 {
                let mut out = [0u8; 8];
                conv.i420_row(&[s[0], s[0]], &[s[1]], &[s[2]], &mut out, 2, RgbOrder::Rgba);
                let want = float_ref(matrix, range, s[0], s[1], s[2]);
                for ch in 0..3 {
                    let diff = i32::from(out[ch]) - i32::from(want[ch]);
                    assert!(
                        diff.abs() <= 1,
                        "{matrix:?} {range:?} {s:?}: {out:?} vs {want:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn bgra_swaps_red_and_blue() {
    let conv = YuvToRgb::new(YuvMatrix::Bt709, ColorRange::Video);
    let (mut rgba, mut bgra) = ([0u8; 8], [0u8; 8]);
    conv.i420_row(&[90, 90], &[60], &[200], &mut rgba, 2, RgbOrder::Rgba);
    conv.i420_row(&[90, 90], &[60], &[200], &mut bgra, 2, RgbOrder::Bgra);
    assert_eq!([rgba[0], rgba[1], rgba[2]], [bgra[2], bgra[1], bgra[0]]);
}

#[test]
fn nv12_and_i420_rgb_paths_agree_on_odd_dimensions() {
    let (w, h) = (7usize, 5usize);
    let (cw, ch) = (4usize, 3usize);
    let y = gen_bytes(w * h, 1);
    let u = gen_bytes(cw * ch, 2);
    let v = gen_bytes(cw * ch, 3);
    let i420 = I420Ref {
        y: &y,
        y_stride: w,
        u: &u,
        u_stride: cw,
        v: &v,
        v_stride: cw,
    };

    let mut uv = vec![0u8; cw * 2 * ch];
    let mut y2 = vec![0u8; w * h];
    i420_to_nv12(
        &i420,
        &mut Nv12Mut {
            y: &mut y2,
            y_stride: w,
            uv: &mut uv,
            uv_stride: cw * 2,
        },
        w,
        h,
    )
    .unwrap();
    assert_eq!(y2, y);
    assert_eq!(uv[..4], [u[0], v[0], u[1], v[1]]);

    let conv = YuvToRgb::new(YuvMatrix::Bt601, ColorRange::Video);
    let mut a = vec![0u8; w * 4 * h];
    let mut b = vec![0u8; w * 4 * h];
    i420_to_rgb(&conv, &i420, &mut a, w * 4, w, h, RgbOrder::Rgba).unwrap();
    let nv12 = Nv12Ref {
        y: &y2,
        y_stride: w,
        uv: &uv,
        uv_stride: cw * 2,
    };
    nv12_to_rgb(&conv, &nv12, &mut b, w * 4, w, h, RgbOrder::Rgba).unwrap();
    assert_eq!(a, b);
    // Last (odd) column uses chroma sample 3 of its chroma row.
    let last = &a[(w - 1) * 4..w * 4];
    let mut want = [0u8; 8];
    conv.i420_row(
        &[y[w - 1], y[w - 1]],
        &[u[3]],
        &[v[3]],
        &mut want,
        2,
        RgbOrder::Rgba,
    );
    assert_eq!(last, &want[..4]);
}

#[test]
fn nv12_i420_round_trip_with_padded_strides() {
    let (w, h) = (6usize, 4usize);
    let (ys, uvs) = (8usize, 10usize);
    let y = gen_bytes(ys * h, 4);
    let uv = gen_bytes(uvs * 2, 5);
    let src = Nv12Ref {
        y: &y,
        y_stride: ys,
        uv: &uv,
        uv_stride: uvs,
    };
    let (mut yo, mut uo, mut vo) = (vec![0u8; w * h], vec![0u8; 3 * 2], vec![0u8; 3 * 2]);
    nv12_to_i420(
        &src,
        &mut I420Mut {
            y: &mut yo,
            y_stride: w,
            u: &mut uo,
            u_stride: 3,
            v: &mut vo,
            v_stride: 3,
        },
        w,
        h,
    )
    .unwrap();
    let (mut yb, mut uvb) = (vec![0u8; w * h], vec![0u8; 6 * 2]);
    let back = I420Ref {
        y: &yo,
        y_stride: w,
        u: &uo,
        u_stride: 3,
        v: &vo,
        v_stride: 3,
    };
    i420_to_nv12(
        &back,
        &mut Nv12Mut {
            y: &mut yb,
            y_stride: w,
            uv: &mut uvb,
            uv_stride: 6,
        },
        w,
        h,
    )
    .unwrap();
    for r in 0..h {
        assert_eq!(yb[r * w..r * w + w], y[r * ys..r * ys + w]);
    }
    for r in 0..2 {
        assert_eq!(uvb[r * 6..r * 6 + 6], uv[r * uvs..r * uvs + 6]);
    }
}

#[test]
fn frame_helpers_reject_bad_geometry() {
    let conv = YuvToRgb::new(YuvMatrix::Bt601, ColorRange::Video);
    let y = [0u8; 16];
    let c = [0u8; 4];
    let src = I420Ref {
        y: &y,
        y_stride: 4,
        u: &c,
        u_stride: 2,
        v: &c,
        v_stride: 2,
    };
    let mut dst = [0u8; 64];
    assert_eq!(
        i420_to_rgb(&conv, &src, &mut dst, 16, 0, 4, RgbOrder::Rgba),
        Err(ColorConvError::EmptyFrame)
    );
    assert_eq!(
        i420_to_rgb(&conv, &src, &mut dst, 15, 4, 4, RgbOrder::Rgba),
        Err(ColorConvError::StrideTooSmall)
    );
    assert_eq!(
        i420_to_rgb(&conv, &src, &mut dst[..63], 16, 4, 4, RgbOrder::Rgba),
        Err(ColorConvError::BufferTooSmall)
    );
    assert_eq!(
        i420_to_rgb(&conv, &src, &mut dst, 16, 4, 5, RgbOrder::Rgba),
        Err(ColorConvError::BufferTooSmall)
    );
    assert!(i420_to_rgb(&conv, &src, &mut dst, 16, 4, 4, RgbOrder::Rgba).is_ok());
}
