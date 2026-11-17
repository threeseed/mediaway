#![cfg(test)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    reason = "test modules may unwrap; generated test data uses small casts"
)]

use super::*;

fn ramp(n: usize, mul: usize) -> Vec<u8> {
    (0..n).map(|i| ((i * mul) % 256) as u8).collect()
}

#[test]
fn same_size_is_an_exact_copy() {
    let src = ramp(13 * 7, 37);
    let plan = BilinearPlan::<1>::new(13, 7, 13, 7).unwrap();
    let mut dst = vec![0u8; 13 * 7];
    plan.scale(&src, 13, &mut dst, 13).unwrap();
    assert_eq!(dst, src);

    let rgba = ramp(5 * 3 * 4, 11);
    let plan = BilinearPlan::<4>::new(5, 3, 5, 3).unwrap();
    let mut out = vec![0u8; rgba.len()];
    plan.scale(&rgba, 20, &mut out, 20).unwrap();
    assert_eq!(out, rgba);
}

#[test]
fn halving_averages_each_2x2_block_rounding_half_up() {
    // 4x2 -> 2x1: each output is the rounded mean of a 2x2 block.
    let src = [0u8, 10, 100, 101, 20, 31, 200, 255];
    let plan = BilinearPlan::<1>::new(4, 2, 2, 1).unwrap();
    let mut dst = [0u8; 2];
    plan.scale(&src, 4, &mut dst, 2).unwrap();
    // (0+10+20+31)/4 = 15.25 -> 15 ; (100+101+200+255)/4 = 164.0 -> 164.
    assert_eq!(dst, [15, 164]);
}

#[test]
fn doubling_interpolates_between_centres_and_clamps_edges() {
    // 2x1 -> 4x1: positions -0.25 (clamped 0), 0.25, 0.75, 1.25 (clamped 1).
    let plan = BilinearPlan::<1>::new(2, 1, 4, 1).unwrap();
    let mut dst = [0u8; 4];
    plan.scale(&[0, 200], 2, &mut dst, 4).unwrap();
    assert_eq!(dst, [0, 50, 150, 200]);
}

#[test]
fn rgba_channels_are_scaled_independently() {
    let src = [255u8, 0, 0, 255, 0, 0, 255, 255];
    let plan = BilinearPlan::<4>::new(2, 1, 1, 1).unwrap();
    let mut dst = [0u8; 4];
    plan.scale(&src, 8, &mut dst, 4).unwrap();
    assert_eq!(dst, [128, 0, 128, 255]);
}

#[test]
fn constant_plane_stays_constant_for_any_ratio() {
    let src = vec![77u8; 31 * 17];
    for (dw, dh) in [(1, 1), (7, 3), (64, 40), (30, 18)] {
        let plan = BilinearPlan::<1>::new(31, 17, dw, dh).unwrap();
        let mut dst = vec![0u8; (dw * dh) as usize];
        plan.scale(&src, 31, &mut dst, dw as usize).unwrap();
        assert!(dst.iter().all(|&v| v == 77), "{dw}x{dh}");
    }
}

#[test]
fn row_api_matches_frame_api() {
    let src = ramp(20 * 10, 13);
    let plan = BilinearPlan::<1>::new(20, 10, 9, 23).unwrap();
    let mut whole = vec![0u8; 9 * 23];
    plan.scale(&src, 20, &mut whole, 9).unwrap();
    for dy in 0..plan.dst_height() {
        let (t, b) = plan.source_rows(dy).unwrap();
        let mut row = [0u8; 9];
        plan.scale_row(dy, &src[t * 20..], &src[b * 20..], &mut row);
        assert_eq!(row[..], whole[dy * 9..dy * 9 + 9]);
    }
}

#[test]
fn rejects_zero_dimensions_and_short_buffers() {
    assert_eq!(
        BilinearPlan::<1>::new(0, 1, 1, 1),
        Err(ColorConvError::EmptyFrame)
    );
    let plan = BilinearPlan::<1>::new(4, 4, 2, 2).unwrap();
    let mut dst = [0u8; 4];
    assert_eq!(
        plan.scale(&[0u8; 15], 4, &mut dst, 2),
        Err(ColorConvError::BufferTooSmall)
    );
    assert_eq!(
        plan.scale(&[0u8; 16], 3, &mut dst, 2),
        Err(ColorConvError::StrideTooSmall)
    );
}
