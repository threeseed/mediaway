//! CPU (`sw` path class) pixel format conversion: NV12 <-> I420 and I420/NV12 -> RGBA/BGRA.
//!
//! **Cost contract (ADR-0006 honesty):** every function here reads and writes CPU memory —
//! a full pass over each plane, no GPU, no Zero-Copy. Use a platform/GPU converter when
//! frames already live on the GPU; use this when they are already CPU bytes (software
//! decode output, `mediaway-sw::h264`, screen-capture readbacks, tests). See
//! `adr/0005-cpu-colorconv-and-scaler.md`.
//!
//! Shape:
//! - **Allocation-free:** callers own every buffer; functions only validate sizes once and
//!   then run tight per-row loops.
//! - **Row-based:** each `*_row` function converts exactly one output row (or one chroma
//!   row pair) and is independent of every other row, so callers may split the frame across
//!   threads (e.g. `rayon` `par_chunks_mut` over destination rows) without any API from this
//!   crate. The frame-level helpers just loop rows sequentially.
//! - **Vectorization-friendly:** fixed-point integer math (Q16), fixed-width chunks, no
//!   per-pixel branches; no hand-written intrinsics (autovectorization only).
//!
//! Chroma upsampling for YUV -> RGB is **nearest / sample-replicate** (each 4:2:0 chroma
//! sample covers a 2x2 luma block), the same as libyuv's default path. It is not a filtered
//! upsample; edges in saturated chroma can look blocky at 1:1 zoom.

#![forbid(unsafe_code)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    reason = "r/g/b/u/v/y are the colour-science names; Q16 results are clamped to 0..=255 before narrowing to u8; coefficient \
              rounding casts are bounded (|coef| < 4 * 65536)"
)]

use mediaway_common::ColorRange;
use thiserror::Error;

/// Errors from frame-level conversion helpers (size validation only; row functions do not
/// fail — they panic-free slice to the shortest input, see each function's docs).
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ColorConvError {
    /// `width` or `height` is zero.
    #[error("frame width and height must be non-zero")]
    EmptyFrame,
    /// A stride is smaller than the row width it must hold.
    #[error("stride is smaller than the row width")]
    StrideTooSmall,
    /// A plane buffer is smaller than `stride * (rows - 1) + row_width`.
    #[error("plane buffer is too small for the given dimensions and stride")]
    BufferTooSmall,
}

/// YUV -> RGB matrix coefficients standard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum YuvMatrix {
    /// ITU-R BT.601 (SD; `Kr = 0.299`, `Kb = 0.114`).
    #[default]
    Bt601,
    /// ITU-R BT.709 (HD; `Kr = 0.2126`, `Kb = 0.0722`).
    Bt709,
}

/// Packed 8-bit RGB output byte order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RgbOrder {
    /// `R G B A` bytes ([`mediaway_common::PixelFormat::Rgba8`]).
    Rgba,
    /// `B G R A` bytes ([`mediaway_common::PixelFormat::Bgra8`]).
    Bgra,
}

const Q: u32 = 16;
const HALF: i32 = 1 << (Q - 1);

/// Precomputed Q16 fixed-point YUV -> RGB coefficients for one matrix + range.
///
/// Build once per stream ([`YuvToRgb::new`] does a little `f64` math) and reuse for every
/// frame. Output alpha is always `255`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YuvToRgb {
    y_offset: i32,
    y_gain: i32,
    v_to_r: i32,
    u_to_g: i32,
    v_to_g: i32,
    u_to_b: i32,
}

impl YuvToRgb {
    /// Coefficients for `matrix` with `range` input (`Video` = luma 16-235 / chroma 16-240
    /// expanded to full-range RGB; `Full` = 0-255 in and out).
    #[must_use]
    pub fn new(matrix: YuvMatrix, range: ColorRange) -> Self {
        let (kr, kb) = match matrix {
            YuvMatrix::Bt601 => (0.299_f64, 0.114_f64),
            YuvMatrix::Bt709 => (0.2126, 0.0722),
        };
        let kg = 1.0 - kr - kb;
        let (y_offset, y_scale, c_scale) = match range {
            ColorRange::Full => (0, 1.0, 1.0),
            // `ColorRange` is non_exhaustive; any future range falls back to video range.
            _ => (16, 255.0 / 219.0, 255.0 / 224.0),
        };
        let q = f64::from(1u32 << Q);
        let fx = |v: f64| (v * q).round() as i32;
        Self {
            y_offset,
            y_gain: fx(y_scale),
            v_to_r: fx(2.0 * (1.0 - kr) * c_scale),
            u_to_g: fx(2.0 * kb * (1.0 - kb) / kg * c_scale),
            v_to_g: fx(2.0 * kr * (1.0 - kr) / kg * c_scale),
            u_to_b: fx(2.0 * (1.0 - kb) * c_scale),
        }
    }

    /// Convert one pixel's chroma into the three additive Q16 terms `(r, g, b)`.
    #[inline]
    const fn chroma_terms(&self, u: u8, v: u8) -> (i32, i32, i32) {
        let cu = u as i32 - 128;
        let cv = v as i32 - 128;
        (
            self.v_to_r * cv + HALF,
            HALF - self.u_to_g * cu - self.v_to_g * cv,
            self.u_to_b * cu + HALF,
        )
    }

    /// Write one 2-pixel pair sharing a chroma sample into `out` (`8` bytes).
    #[inline]
    fn pair<const BGRA: bool>(&self, y: [u8; 2], u: u8, v: u8, out: &mut [u8; 8]) {
        let (cr, cg, cb) = self.chroma_terms(u, v);
        for (i, &luma) in y.iter().enumerate() {
            let yy = (i32::from(luma) - self.y_offset) * self.y_gain;
            let r = ((yy + cr) >> Q).clamp(0, 255) as u8;
            let g = ((yy + cg) >> Q).clamp(0, 255) as u8;
            let b = ((yy + cb) >> Q).clamp(0, 255) as u8;
            let px = if BGRA { [b, g, r, 255] } else { [r, g, b, 255] };
            out[i * 4..i * 4 + 4].copy_from_slice(&px);
        }
    }

    /// Generic row body over a chroma iterator yielding one `(u, v)` per luma pair.
    #[inline]
    fn row<const BGRA: bool>(
        &self,
        y: &[u8],
        mut chroma: impl Iterator<Item = (u8, u8)>,
        dst: &mut [u8],
        width: usize,
    ) {
        let width = width.min(y.len()).min(dst.len() / 4);
        let (y, dst) = (&y[..width], &mut dst[..width * 4]);
        let (y_pairs, y_tail) = y.as_chunks::<2>();
        let (d_pairs, d_tail) = dst.as_chunks_mut::<8>();
        for (yp, dp) in y_pairs.iter().zip(d_pairs.iter_mut()) {
            let Some((u, v)) = chroma.next() else { return };
            self.pair::<BGRA>(*yp, u, v, dp);
        }
        if let (Some(&last), Some((u, v))) = (y_tail.first(), chroma.next()) {
            let mut tmp = [0u8; 8];
            self.pair::<BGRA>([last, last], u, v, &mut tmp);
            d_tail.copy_from_slice(&tmp[..4]);
        }
    }

    /// Convert one row of planar 4:2:0 (I420) to packed RGBA/BGRA.
    ///
    /// `y` holds `width` luma samples, `u`/`v` hold `width.div_ceil(2)` chroma samples (the
    /// chroma row for this luma row, i.e. chroma row `luma_row / 2`), `dst` holds
    /// `width * 4` bytes. Shorter slices truncate the row instead of panicking.
    pub fn i420_row(
        &self,
        y: &[u8],
        u: &[u8],
        v: &[u8],
        dst: &mut [u8],
        width: usize,
        order: RgbOrder,
    ) {
        let chroma = u.iter().copied().zip(v.iter().copied());
        match order {
            RgbOrder::Rgba => self.row::<false>(y, chroma, dst, width),
            RgbOrder::Bgra => self.row::<true>(y, chroma, dst, width),
        }
    }

    /// Convert one row of semi-planar 4:2:0 (NV12, interleaved `U V`) to packed RGBA/BGRA.
    /// `uv` holds `2 * width.div_ceil(2)` bytes; otherwise as [`Self::i420_row`].
    pub fn nv12_row(&self, y: &[u8], uv: &[u8], dst: &mut [u8], width: usize, order: RgbOrder) {
        let chroma = uv.as_chunks::<2>().0.iter().map(|&uv| uv.into());
        match order {
            RgbOrder::Rgba => self.row::<false>(y, chroma, dst, width),
            RgbOrder::Bgra => self.row::<true>(y, chroma, dst, width),
        }
    }
}

/// Borrowed planar 4:2:0 (I420) frame. Chroma planes are `width.div_ceil(2)` x
/// `height.div_ceil(2)`.
#[derive(Debug, Clone, Copy)]
pub struct I420Ref<'a> {
    /// Luma plane.
    pub y: &'a [u8],
    /// Luma row stride in bytes.
    pub y_stride: usize,
    /// Cb plane.
    pub u: &'a [u8],
    /// Cb row stride in bytes.
    pub u_stride: usize,
    /// Cr plane.
    pub v: &'a [u8],
    /// Cr row stride in bytes.
    pub v_stride: usize,
}

/// Mutable planar 4:2:0 (I420) frame; layout as [`I420Ref`].
#[derive(Debug)]
pub struct I420Mut<'a> {
    /// Luma plane.
    pub y: &'a mut [u8],
    /// Luma row stride in bytes.
    pub y_stride: usize,
    /// Cb plane.
    pub u: &'a mut [u8],
    /// Cb row stride in bytes.
    pub u_stride: usize,
    /// Cr plane.
    pub v: &'a mut [u8],
    /// Cr row stride in bytes.
    pub v_stride: usize,
}

/// Borrowed semi-planar 4:2:0 (NV12) frame. The `uv` plane is `height.div_ceil(2)` rows of
/// `2 * width.div_ceil(2)` interleaved `U V` bytes.
#[derive(Debug, Clone, Copy)]
pub struct Nv12Ref<'a> {
    /// Luma plane.
    pub y: &'a [u8],
    /// Luma row stride in bytes.
    pub y_stride: usize,
    /// Interleaved chroma plane.
    pub uv: &'a [u8],
    /// Chroma row stride in bytes.
    pub uv_stride: usize,
}

/// Mutable semi-planar 4:2:0 (NV12) frame; layout as [`Nv12Ref`].
#[derive(Debug)]
pub struct Nv12Mut<'a> {
    /// Luma plane.
    pub y: &'a mut [u8],
    /// Luma row stride in bytes.
    pub y_stride: usize,
    /// Interleaved chroma plane.
    pub uv: &'a mut [u8],
    /// Chroma row stride in bytes.
    pub uv_stride: usize,
}

/// Check that a plane of `rows` rows, each `row_bytes` wide, fits `len` bytes at `stride`.
pub(crate) const fn check_plane(
    len: usize,
    stride: usize,
    row_bytes: usize,
    rows: usize,
) -> Result<(), ColorConvError> {
    if stride < row_bytes {
        return Err(ColorConvError::StrideTooSmall);
    }
    match stride.checked_mul(rows - 1) {
        Some(n) if n.saturating_add(row_bytes) <= len => Ok(()),
        _ => Err(ColorConvError::BufferTooSmall),
    }
}

const fn check_dims(width: usize, height: usize) -> Result<(usize, usize), ColorConvError> {
    if width == 0 || height == 0 {
        return Err(ColorConvError::EmptyFrame);
    }
    Ok((width.div_ceil(2), height.div_ceil(2)))
}

fn check_i420(f: &I420Ref<'_>, width: usize, height: usize) -> Result<(), ColorConvError> {
    let (cw, ch) = check_dims(width, height)?;
    check_plane(f.y.len(), f.y_stride, width, height)?;
    check_plane(f.u.len(), f.u_stride, cw, ch)?;
    check_plane(f.v.len(), f.v_stride, cw, ch)
}

fn check_nv12(f: &Nv12Ref<'_>, width: usize, height: usize) -> Result<(), ColorConvError> {
    let (cw, ch) = check_dims(width, height)?;
    check_plane(f.y.len(), f.y_stride, width, height)?;
    check_plane(f.uv.len(), f.uv_stride, cw * 2, ch)
}

/// Copy `rows` rows of `row_bytes` between strided planes.
fn copy_plane(
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_stride: usize,
    row_bytes: usize,
    rows: usize,
) {
    for r in 0..rows {
        let s = &src[r * src_stride..r * src_stride + row_bytes];
        dst[r * dst_stride..r * dst_stride + row_bytes].copy_from_slice(s);
    }
}

/// Deinterleave one NV12 chroma row (`U V U V ...`) into separate `u`/`v` rows.
pub fn deinterleave_uv_row(uv: &[u8], u: &mut [u8], v: &mut [u8]) {
    for ((&[cu, cv], du), dv) in uv
        .as_chunks::<2>()
        .0
        .iter()
        .zip(u.iter_mut())
        .zip(v.iter_mut())
    {
        *du = cu;
        *dv = cv;
    }
}

/// Interleave separate `u`/`v` rows into one NV12 chroma row (`U V U V ...`).
pub fn interleave_uv_row(u: &[u8], v: &[u8], uv: &mut [u8]) {
    for ((d, &cu), &cv) in uv.as_chunks_mut::<2>().0.iter_mut().zip(u).zip(v) {
        *d = [cu, cv];
    }
}

/// NV12 -> I420 (`sw`: one CPU pass; luma is a straight row copy, chroma is deinterleaved).
///
/// # Errors
///
/// [`ColorConvError`] if dimensions are zero or any plane/stride is too small.
pub fn nv12_to_i420(
    src: &Nv12Ref<'_>,
    dst: &mut I420Mut<'_>,
    width: usize,
    height: usize,
) -> Result<(), ColorConvError> {
    check_nv12(src, width, height)?;
    let (cw, ch) = check_dims(width, height)?;
    check_plane(dst.y.len(), dst.y_stride, width, height)?;
    check_plane(dst.u.len(), dst.u_stride, cw, ch)?;
    check_plane(dst.v.len(), dst.v_stride, cw, ch)?;
    copy_plane(src.y, src.y_stride, dst.y, dst.y_stride, width, height);
    for r in 0..ch {
        let uv = &src.uv[r * src.uv_stride..r * src.uv_stride + cw * 2];
        let u = &mut dst.u[r * dst.u_stride..r * dst.u_stride + cw];
        let v = &mut dst.v[r * dst.v_stride..r * dst.v_stride + cw];
        deinterleave_uv_row(uv, u, v);
    }
    Ok(())
}

/// I420 -> NV12 (`sw`: one CPU pass; luma is a straight row copy, chroma is interleaved).
///
/// # Errors
///
/// [`ColorConvError`] if dimensions are zero or any plane/stride is too small.
pub fn i420_to_nv12(
    src: &I420Ref<'_>,
    dst: &mut Nv12Mut<'_>,
    width: usize,
    height: usize,
) -> Result<(), ColorConvError> {
    check_i420(src, width, height)?;
    let (cw, ch) = check_dims(width, height)?;
    check_plane(dst.y.len(), dst.y_stride, width, height)?;
    check_plane(dst.uv.len(), dst.uv_stride, cw * 2, ch)?;
    copy_plane(src.y, src.y_stride, dst.y, dst.y_stride, width, height);
    for r in 0..ch {
        let u = &src.u[r * src.u_stride..r * src.u_stride + cw];
        let v = &src.v[r * src.v_stride..r * src.v_stride + cw];
        let uv = &mut dst.uv[r * dst.uv_stride..r * dst.uv_stride + cw * 2];
        interleave_uv_row(u, v, uv);
    }
    Ok(())
}

/// I420 -> packed RGBA/BGRA (`sw`: one CPU pass, nearest chroma upsampling, alpha `255`).
/// `dst` holds `height` rows of `width * 4` bytes at `dst_stride`.
///
/// # Errors
///
/// [`ColorConvError`] if dimensions are zero or any plane/stride is too small.
#[allow(
    clippy::too_many_arguments,
    reason = "explicit frame geometry, no hidden defaults"
)]
pub fn i420_to_rgb(
    conv: &YuvToRgb,
    src: &I420Ref<'_>,
    dst: &mut [u8],
    dst_stride: usize,
    width: usize,
    height: usize,
    order: RgbOrder,
) -> Result<(), ColorConvError> {
    check_i420(src, width, height)?;
    check_plane(dst.len(), dst_stride, width * 4, height)?;
    let cw = width.div_ceil(2);
    for r in 0..height {
        let c = r / 2;
        conv.i420_row(
            &src.y[r * src.y_stride..r * src.y_stride + width],
            &src.u[c * src.u_stride..c * src.u_stride + cw],
            &src.v[c * src.v_stride..c * src.v_stride + cw],
            &mut dst[r * dst_stride..r * dst_stride + width * 4],
            width,
            order,
        );
    }
    Ok(())
}

/// NV12 -> packed RGBA/BGRA; otherwise as [`i420_to_rgb`].
///
/// # Errors
///
/// [`ColorConvError`] if dimensions are zero or any plane/stride is too small.
pub fn nv12_to_rgb(
    conv: &YuvToRgb,
    src: &Nv12Ref<'_>,
    dst: &mut [u8],
    dst_stride: usize,
    width: usize,
    height: usize,
    order: RgbOrder,
) -> Result<(), ColorConvError> {
    check_nv12(src, width, height)?;
    check_plane(dst.len(), dst_stride, width * 4, height)?;
    let cw2 = width.div_ceil(2) * 2;
    for r in 0..height {
        let c = r / 2;
        conv.nv12_row(
            &src.y[r * src.y_stride..r * src.y_stride + width],
            &src.uv[c * src.uv_stride..c * src.uv_stride + cw2],
            &mut dst[r * dst_stride..r * dst_stride + width * 4],
            width,
            order,
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "colorconv_tests.rs"]
mod tests;
