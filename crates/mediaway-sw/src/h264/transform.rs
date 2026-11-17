//! H.264 4x4 integer inverse transform and dequantization (ITU-T H.264 § 8.5.9-8.5.13).
//!
//! Flat/default scaling lists only (`weightScale(i,j) == 16` everywhere,
//! `Flat_4x4_16`, § 8.5.9) — [`super::Sps::parse`] already only skips past
//! `scaling_list()` bodies rather than retaining their values (see that module's docs),
//! so this crate structurally cannot honor a stream that signals custom scaling lists;
//! flat scaling is by far the common case for Baseline-profile encoders. A stream with
//! non-default scaling lists will still decode without panicking, just with numerically
//! wrong dequantized values — a known, documented limitation (see
//! `adr/0003-cavlc-i-slice-first-decode.md`), not a silently-assumed one.
//!
//! **Provenance / verification:** [`NORM_ADJUST`] and [`QPI_TO_QPC`] were cross-checked
//! against `FFmpeg`'s `h264data.c` numeric constants (read only to fact-check the numbers
//! independently recalled from the spec, not copied as code). The luma Plane intra-pred
//! formula in [`super::intra_pred`] was independently verified via an algebraically
//! equivalent alternate formulation found in a public reference during this session; the
//! chroma Plane-mode constants were not — see the ADR's "Negative / Trade-offs" section.

#![forbid(unsafe_code)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "raster/QP indices below are bounded by fixed 4x4/QP (0..=51) ranges checked \
              at each cast site or by the caller (running QP state uses checked wraparound)"
)]

use super::error::H264Error;

/// `normAdjust` (ITU-T H.264 § 8.5.9, the `m`-indexed part of `LevelScale4x4`), indexed
/// `[qP % 6][position class]` where position class `0` = both row/col even, `1` = both
/// odd, `2` = mixed (§ 8.5.9's `v` pattern).
const NORM_ADJUST: [[i64; 3]; 6] = [
    [10, 16, 13],
    [11, 18, 14],
    [13, 20, 16],
    [14, 23, 18],
    [16, 25, 20],
    [18, 29, 23],
];

/// Default (`Flat_4x4_16`) weight scale factor used everywhere — see module docs on the
/// custom-scaling-list scope cut.
const FLAT_WEIGHT_SCALE: i64 = 16;

/// `QPi` (`Clip3(0, 51, QPy + chroma_qp_index_offset)`) to `QPc` mapping for `QPi` `30..=51`
/// (ITU-T H.264 Table 8-15); `QPi < 30` maps to itself.
const QPI_TO_QPC: [i32; 22] = [
    29, 30, 31, 32, 32, 33, 34, 34, 35, 35, 36, 36, 37, 37, 37, 38, 38, 38, 39, 39, 39, 39,
];

/// Derive the chroma quantization parameter `QPc` from the luma `qp` and
/// `pps.chroma_qp_index_offset` (ITU-T H.264 § 8.5.8, Table 8-15).
#[must_use]
pub(super) fn qpc_from_qp(qp: i32, chroma_qp_index_offset: i32) -> i32 {
    let qpi = (qp + chroma_qp_index_offset).clamp(0, 51);
    if qpi < 30 {
        qpi
    } else {
        QPI_TO_QPC[usize::try_from(qpi - 30).unwrap_or(0)]
    }
}

/// Position class (`v(i,j)` in § 8.5.9) for [`NORM_ADJUST`]'s second index, per raster
/// position (`row * 4 + col`): `0` = both even, `1` = both odd, `2` = mixed.
const POSITION_CLASS: [usize; 16] = [0, 2, 0, 2, 2, 1, 2, 1, 0, 2, 0, 2, 2, 1, 2, 1];

/// Per-block dequant shift, resolved once per block (not per coefficient) so the
/// per-coefficient loop below is branch-free and vectorizable.
#[derive(Clone, Copy)]
enum Shift {
    /// `(c * scale) << n`.
    Up(u32),
    /// `(c * scale + round) >> n`, `round = 1 << (n - 1)`.
    Down(u32, i64),
}

impl Shift {
    /// Resolve `qP / 6` against the spec's threshold `pivot` (`4` for normal coefficients,
    /// `6` for luma DC). Errors mirror the former per-coefficient checked arithmetic: a
    /// shift amount outside `0..64` is [`H264Error::FieldOverflow`]. (One difference: the
    /// former code skipped zero coefficients before checking, so an all-zero block with a
    /// QP far outside `0..=51` used to succeed; the decoder only ever passes `0..=51`.)
    fn resolve(qp: i32, pivot: i32) -> Result<Self, H264Error> {
        let shift = qp.div_euclid(6);
        if shift >= pivot {
            let up = u32::try_from(shift - pivot).map_err(|_err| H264Error::FieldOverflow)?;
            if up >= i64::BITS {
                return Err(H264Error::FieldOverflow);
            }
            Ok(Self::Up(up))
        } else {
            let down = u32::try_from(pivot - shift).map_err(|_err| H264Error::FieldOverflow)?;
            if down >= i64::BITS {
                return Err(H264Error::FieldOverflow);
            }
            Ok(Self::Down(down, 1i64 << (down - 1)))
        }
    }
}

/// `LevelScale4x4` (flat weights) for `qp`, per position class.
fn level_scales(qp: i32) -> [i64; 3] {
    let row = NORM_ADJUST[usize::try_from(qp.rem_euclid(6)).unwrap_or(0)];
    [
        FLAT_WEIGHT_SCALE * row[0],
        FLAT_WEIGHT_SCALE * row[1],
        FLAT_WEIGHT_SCALE * row[2],
    ]
}

/// Apply `scale[i]` and `shift` to every coefficient, then range-check the whole block
/// back into `i32` once.
///
/// Overflow analysis (why the per-coefficient math needs no checked ops): `|c| < 2^31`,
/// `scale <= 16 * 29 < 2^9`, so `|c * scale| < 2^40`; `Up` shifts are at most
/// `51 / 6 - 4 = 4` for any in-range QP (larger shifts from an out-of-range QP wrap the
/// same way the former `checked_shl` did — it only checked the shift amount), and the
/// final `i32` range check reports [`H264Error::FieldOverflow`] exactly when the former
/// per-coefficient `i32::try_from` did. A zero coefficient produces `0` on both branches
/// (`round < 2^down`), so the former `c == 0` skip is not needed for bit-exactness.
fn scale_block<const N: usize>(
    coeffs: &[i32; N],
    scale: &[i64; N],
    shift: Shift,
) -> Result<[i32; N], H264Error> {
    let mut wide = [0i64; N];
    match shift {
        Shift::Up(up) => {
            for ((w, &c), &s) in wide.iter_mut().zip(coeffs).zip(scale) {
                *w = (i64::from(c) * s) << up;
            }
        }
        Shift::Down(down, round) => {
            for ((w, &c), &s) in wide.iter_mut().zip(coeffs).zip(scale) {
                *w = (i64::from(c) * s + round) >> down;
            }
        }
    }
    narrow(&wide)
}

/// Narrow a block to `i32`, failing if any element is out of range (one check per block,
/// no early exit inside the loop).
fn narrow<const N: usize>(wide: &[i64; N]) -> Result<[i32; N], H264Error> {
    let in_range = wide.iter().fold(true, |ok, &w| {
        ok & (w >= i64::from(i32::MIN)) & (w <= i64::from(i32::MAX))
    });
    if !in_range {
        return Err(H264Error::FieldOverflow);
    }
    let mut out = [0i32; N];
    for (o, &w) in out.iter_mut().zip(wide) {
        *o = w as i32;
    }
    Ok(out)
}

/// Dequantize the AC/normal coefficients of a 4x4 block (ITU-T H.264 § 8.5.12.1). `raster`
/// position `0` (the DC coefficient) is dequantized the same way as any other position —
/// callers decoding an `I_16x16` macroblock overwrite raster position `0` afterward with
/// the separately-dequantized luma DC value (see [`dequant_luma_dc`]); AC-only residual
/// blocks already carry `0` at that position, so dequantizing it is a harmless no-op there.
///
/// # Errors
///
/// [`H264Error::FieldOverflow`] if a dequantized value does not fit `i32` (only reachable
/// with an adversarially large decoded coefficient level) or `qp` is wildly out of range.
pub(super) fn dequant_normal(raster: &[i32; 16], qp: i32) -> Result<[i32; 16], H264Error> {
    let classes = level_scales(qp);
    let scale = POSITION_CLASS.map(|class| classes[class]);
    scale_block(raster, &scale, Shift::resolve(qp, 4)?)
}

/// Dequantize 16 luma DC coefficients (already inverse-Hadamard-transformed by
/// [`inverse_hadamard_4x4`]) using the luma QP (ITU-T H.264 § 8.5.10). `raster` is indexed
/// the same way as a normal 4x4 block (`row * 4 + col`), but here each entry is one
/// `I_16x16` luma 4x4 sub-block's DC value, not a pixel-position coefficient.
///
/// # Errors
///
/// [`H264Error::FieldOverflow`] on adversarial-input arithmetic overflow.
pub(super) fn dequant_luma_dc(raster: &[i32; 16], qp: i32) -> Result<[i32; 16], H264Error> {
    let scale = [level_scales(qp)[0]; 16];
    scale_block(raster, &scale, Shift::resolve(qp, 6)?)
}

/// Dequantize the chroma DC 2x2 block (already inverse-Hadamard-transformed by
/// [`inverse_hadamard_2x2`]) using the chroma QP (ITU-T H.264 § 8.5.11.2, `ChromaArrayType
/// == 1` / 4:2:0 only — the only formula this crate needs).
///
/// # Errors
///
/// [`H264Error::FieldOverflow`] on adversarial-input arithmetic overflow.
pub(super) fn dequant_chroma_dc(c: &[i32; 4], qpc: i32) -> Result<[i32; 4], H264Error> {
    let up = u32::try_from(qpc.div_euclid(6)).map_err(|_err| H264Error::FieldOverflow)?;
    if up >= i64::BITS {
        return Err(H264Error::FieldOverflow);
    }
    let scale = level_scales(qpc)[0];
    let mut wide = [0i64; 4];
    for (w, &value) in wide.iter_mut().zip(c) {
        *w = ((i64::from(value) * scale) << up) >> 5;
    }
    narrow(&wide)
}

/// Inverse 4x4 core transform (ITU-T H.264 § 8.5.12.2): column pass, row pass, then the
/// final `(x + 32) >> 6` rounding/normalization. `d` holds dequantized coefficients in
/// raster order; the result is residual sample values (still signed, not yet added to a
/// prediction or clipped to `0..=255`).
#[must_use]
pub(super) fn inverse_transform_4x4(d: &[i32; 16]) -> [i32; 16] {
    let butterflied = hadamard_like_pass::<false>(d);
    let mut out = [0i32; 16];
    for (idx, &value) in butterflied.iter().enumerate() {
        out[idx] = (value + 32) >> 6;
    }
    out
}

/// Inverse 4x4 Hadamard transform for the 16 `I_16x16` luma DC coefficients (ITU-T H.264
/// § 8.5.10, applied before [`dequant_luma_dc`] — raw levels in, raw Hadamard output out,
/// no rounding/shift at this stage since that is folded into the DC dequant formula).
#[must_use]
pub(super) fn inverse_hadamard_4x4(c: &[i32; 16]) -> [i32; 16] {
    hadamard_like_pass::<true>(c)
}

/// Shared column-then-row separable butterfly for both the core inverse transform and the
/// luma DC Hadamard transform — they use the same structure, differing only in whether the
/// odd-indexed intermediate terms are halved (`>> 1`, core transform) or not (Hadamard is a
/// pure +/-1 transform). `HADAMARD` is a const generic so each instantiation is
/// branch-free (the former runtime `bool` was re-tested per butterfly).
fn hadamard_like_pass<const HADAMARD: bool>(input: &[i32; 16]) -> [i32; 16] {
    // Column pass: raster index `row * 4 + col`.
    let mut cols = [0i32; 16];
    for col in 0..4 {
        let c0 = input[col];
        let c1 = input[4 + col];
        let c2 = input[8 + col];
        let c3 = input[12 + col];
        let (e0, e1, e2, e3) = butterfly::<HADAMARD>(c0, c1, c2, c3);
        cols[col] = e0 + e3;
        cols[4 + col] = e1 + e2;
        cols[8 + col] = e1 - e2;
        cols[12 + col] = e0 - e3;
    }
    // Row pass.
    let mut out = [0i32; 16];
    for row in 0..4 {
        let base = row * 4;
        let r0 = cols[base];
        let r1 = cols[base + 1];
        let r2 = cols[base + 2];
        let r3 = cols[base + 3];
        let (e0, e1, e2, e3) = butterfly::<HADAMARD>(r0, r1, r2, r3);
        out[base] = e0 + e3;
        out[base + 1] = e1 + e2;
        out[base + 2] = e1 - e2;
        out[base + 3] = e0 - e3;
    }
    out
}

/// One 1-D butterfly stage shared by the core transform and the Hadamard transform.
/// Overflow analysis: inputs here are dequantized coefficients or their first-pass
/// intermediates; a coefficient level is always a valid `i32` (CAVLC decode only returns
/// values that fit `i32`, see `cavlc::decode_levels`) and dequantization bounds its
/// `i64`-checked output back to `i32`, so these `i32` add/sub/shift operations stay many
/// orders of magnitude below `i32`'s overflow boundary for any input this crate's own
/// decode/dequant stages can produce.
const fn butterfly<const HADAMARD: bool>(
    v0: i32,
    v1: i32,
    v2: i32,
    v3: i32,
) -> (i32, i32, i32, i32) {
    if HADAMARD {
        (v0 + v2, v0 - v2, v1 - v3, v1 + v3)
    } else {
        (v0 + v2, v0 - v2, (v1 >> 1) - v3, v1 + (v3 >> 1))
    }
}

/// Inverse 2x2 Hadamard transform for the chroma DC block (ITU-T H.264 § 8.5.11.1),
/// `c == [c(0,0), c(0,1), c(1,0), c(1,1)]`.
#[must_use]
pub(super) const fn inverse_hadamard_2x2(c: &[i32; 4]) -> [i32; 4] {
    let (c00, c01, c10, c11) = (c[0], c[1], c[2], c[3]);
    [
        c00 + c01 + c10 + c11,
        c00 - c01 + c10 - c11,
        c00 + c01 - c10 - c11,
        c00 - c01 - c10 + c11,
    ]
}

#[cfg(test)]
#[path = "transform_tests.rs"]
mod tests;
