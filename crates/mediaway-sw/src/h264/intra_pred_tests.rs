#![cfg(test)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test modules may unwrap"
)]

use super::*;

#[test]
fn predict_16x16_vertical_repeats_top_row() {
    let top = [10u8; 16];
    let out = predict_16x16(0, Some(&top), None, None).unwrap();
    assert_eq!(out, [10u8; 256]);
}

#[test]
fn predict_16x16_horizontal_repeats_left_column_per_row() {
    let mut left = [0u8; 16];
    for (i, v) in left.iter_mut().enumerate() {
        *v = i as u8;
    }
    let out = predict_16x16(1, None, Some(&left), None).unwrap();
    for y in 0..16 {
        assert_eq!(&out[y * 16..y * 16 + 16], [y as u8; 16].as_slice());
    }
}

#[test]
fn predict_16x16_dc_averages_uniform_top_and_left() {
    let top = [16u8; 16];
    let left = [16u8; 16];
    let out = predict_16x16(2, Some(&top), Some(&left), None).unwrap();
    assert_eq!(out, [16u8; 256]);
}

#[test]
fn predict_16x16_dc_falls_back_to_128_when_no_neighbors() {
    let out = predict_16x16(2, None, None, None).unwrap();
    assert_eq!(out, [128u8; 256]);
}

#[test]
fn predict_16x16_dc_uses_top_only_when_left_unavailable() {
    let top = [16u8; 16];
    let out = predict_16x16(2, Some(&top), None, None).unwrap();
    assert_eq!(out, [16u8; 256]);
}

#[test]
fn predict_16x16_plane_of_uniform_neighbors_reproduces_the_constant() {
    let top = [100u8; 16];
    let left = [100u8; 16];
    let out = predict_16x16(3, Some(&top), Some(&left), Some(100)).unwrap();
    assert_eq!(out, [100u8; 256]);
}

#[test]
fn predict_16x16_rejects_vertical_without_top() {
    assert_eq!(
        predict_16x16(0, None, Some(&[0; 16]), None),
        Err(H264Error::UnavailableIntraNeighbor)
    );
}

#[test]
fn predict_16x16_rejects_invalid_mode() {
    assert_eq!(
        predict_16x16(4, Some(&[0; 16]), Some(&[0; 16]), Some(0)),
        Err(H264Error::InvalidMbType)
    );
}

#[test]
fn predict_chroma_8x8_vertical_repeats_top_row() {
    let top = [7u8; 8];
    let out = predict_chroma_8x8(2, Some(&top), None, None).unwrap();
    assert_eq!(out, [7u8; 64]);
}

#[test]
fn predict_chroma_8x8_horizontal_repeats_left_column_per_row() {
    let mut left = [0u8; 8];
    for (i, v) in left.iter_mut().enumerate() {
        *v = (i as u8) * 10;
    }
    let out = predict_chroma_8x8(1, None, Some(&left), None).unwrap();
    for y in 0..8 {
        assert_eq!(&out[y * 8..y * 8 + 8], [(y as u8) * 10; 8].as_slice());
    }
}

#[test]
fn predict_chroma_8x8_dc_uses_per_quadrant_averaging_rules() {
    let top = [100u8; 8];
    let left = [50u8; 8];
    let out = predict_chroma_8x8(0, Some(&top), Some(&left), None).unwrap();

    // Top-left combines top+left: (400 + 200 + 4) >> 3 = 75.
    assert_eq!(out[0], 75);
    // Top-right prefers top only: avg4(100,100,100,100) = 100.
    assert_eq!(out[4], 100);
    // Bottom-left prefers left only: avg4(50,50,50,50) = 50.
    assert_eq!(out[4 * 8], 50);
    // Bottom-right combines top+left: same sums as top-left = 75.
    assert_eq!(out[4 * 8 + 4], 75);
}

#[test]
fn predict_chroma_8x8_dc_falls_back_to_128_with_no_neighbors() {
    let out = predict_chroma_8x8(0, None, None, None).unwrap();
    assert_eq!(out, [128u8; 64]);
}

#[test]
fn predict_chroma_8x8_plane_of_uniform_neighbors_reproduces_the_constant() {
    let top = [50u8; 8];
    let left = [50u8; 8];
    let out = predict_chroma_8x8(3, Some(&top), Some(&left), Some(50)).unwrap();
    assert_eq!(out, [50u8; 64]);
}

#[test]
fn predict_chroma_8x8_rejects_plane_without_corner() {
    assert_eq!(
        predict_chroma_8x8(3, Some(&[0; 8]), Some(&[0; 8]), None),
        Err(H264Error::UnavailableIntraNeighbor)
    );
}

/// Per-pixel spec formula (§ 8.3.3.4 / § 8.3.4.4) as an oracle for the row-incremental
/// `plane_fill`. `k` is the centring offset (7 luma, 3 chroma).
#[allow(clippy::many_single_char_names, reason = "spec variable names")]
fn reference_plane(n: i32, k: i32, a: i32, b: i32, c: i32) -> Vec<u8> {
    let mut out = Vec::new();
    for y in 0..n {
        for x in 0..n {
            out.push(((a + b * (x - k) + c * (y - k) + 16) >> 5).clamp(0, 255) as u8);
        }
    }
    out
}

#[test]
fn plane_fill_is_bit_exact_with_per_pixel_formula() {
    let mut seed = 0x1234_5678_u32;
    for _ in 0..5_000 {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let a = i32::try_from(seed % 8161).unwrap(); // 16 * (255 + 255) + 1
        let b = i32::try_from((seed >> 13) % 600).unwrap() - 300;
        let c = i32::try_from((seed >> 3) % 600).unwrap() - 300;
        assert_eq!(
            plane_fill::<16, 256>(a + 16 - 7 * b - 7 * c, b, c).to_vec(),
            reference_plane(16, 7, a, b, c)
        );
        assert_eq!(
            plane_fill::<8, 64>(a + 16 - 3 * b - 3 * c, b, c).to_vec(),
            reference_plane(8, 3, a, b, c)
        );
    }
}

#[test]
fn predict_16x16_plane_of_gradient_matches_hand_computed_corners() {
    let mut top = [0u8; 16];
    let mut left = [0u8; 16];
    for i in 0..16 {
        top[i] = (i * 8) as u8;
        left[i] = (i * 4) as u8;
    }
    // H = sum_{x'=0..6} (x'+1) * 16(x'+1) + 8 * (t[15] - corner) = 16 * 140 + 960 = 3200
    // -> b = (5*3200+32)>>6 = 250; V = 1600 -> c = 125; a = 16*(60+120) = 2880.
    let out = predict_16x16(3, Some(&top), Some(&left), Some(0)).unwrap();
    assert_eq!(out.to_vec(), reference_plane(16, 7, 2880, 250, 125));
    // Spot-check corners by hand: (2880 - 7*250 - 7*125 + 16) >> 5 = 8, and
    // (2880 + 8*250 + 8*125 + 16) >> 5 = 184.
    assert_eq!(out[0], 8);
    assert_eq!(out[255], 184);
}

#[test]
fn predict_chroma_8x8_dc_with_only_left_fills_quadrants() {
    let left = [10, 10, 10, 10, 30, 30, 30, 30];
    let out = predict_chroma_8x8(0, None, Some(&left), None).unwrap();
    for y in 0..8 {
        for x in 0..8 {
            let expect = if y < 4 { 10 } else { 30 };
            assert_eq!(out[y * 8 + x], expect, "({x},{y})");
        }
    }
}

#[test]
fn predict_chroma_8x8_rejects_invalid_mode() {
    assert_eq!(
        predict_chroma_8x8(4, Some(&[0; 8]), Some(&[0; 8]), Some(0)),
        Err(H264Error::InvalidMbType)
    );
}
