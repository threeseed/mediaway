//! CPU (`sw` path class) bilinear scaler for 8-bit planes (Y / U / V, 1 channel) and packed
//! 4-channel pixels (RGBA / BGRA).
//!
//! **Cost contract (ADR-0006 honesty):** a full CPU read of the source footprint and write
//! of the destination per call — no GPU, no Zero-Copy. Prefer a GPU/platform scaler when
//! frames are already on the GPU. See `adr/0005-cpu-colorconv-and-scaler.md`.
//!
//! - **Plan once, scale many:** [`BilinearPlan::new`] precomputes the horizontal source
//!   indices and weights (one `Vec` per axis, allocated **once** at plan time, not per
//!   frame). Scaling itself never allocates.
//! - **Row-based:** [`BilinearPlan::scale_row`] produces exactly one destination row and
//!   only reads two source rows, so callers can parallelize over destination rows.
//! - **Sampling:** pixel-centre aligned (`sx = (dx + 0.5) * src_w / dst_w - 0.5`, clamped
//!   to the edge), 8-bit fractional weights, round-half-up. Same-size scaling is an exact
//!   copy. Downscaling by more than 2x is **not** area-averaged (bilinear only reads a 2x2
//!   neighbourhood), so heavy downscales alias — scale in 2x steps if that matters.
//!
//! I420 / NV12 frames: scale each plane separately (`CHANNELS = 1` for Y/U/V, or
//! `CHANNELS = 2` for an NV12 `UV` plane) with a plan sized to that plane.

#![forbid(unsafe_code)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::many_single_char_names,
    reason = "a/b/c/d name the 2x2 bilinear taps; fixed-point positions are non-negative and bounded by the u32 plane sizes \
              validated in BilinearPlan::new (< 2^48, no i64 wrap); blended values are <= 255 by construction"
)]

use crate::colorconv::{ColorConvError, check_plane};

/// One axis tap: left/top source index, right/bottom source index, weight of the second
/// sample in `0..=256`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tap {
    i0: u32,
    i1: u32,
    w1: u32,
}

/// Fractional bits of the 8-bit weights.
const WBITS: u32 = 8;
const WONE: u32 = 1 << WBITS;

fn axis_taps(src: u32, dst: u32) -> Vec<Tap> {
    // Q16 source position of each destination centre: (2*d + 1) * src / (2 * dst) - 0.5.
    let src64 = u64::from(src);
    let dst64 = u64::from(dst);
    (0..dst64)
        .map(|d| {
            let num = (2 * d + 1) * src64 * 65_536;
            let pos = (num / (2 * dst64)) as i64 - 32_768;
            let pos = pos.clamp(0, (i64::from(src) - 1) * 65_536);
            let i0 = (pos >> 16) as u32;
            let frac = ((pos & 0xFFFF) + 128) >> 8; // round to 8 bits
            let (i0, w1) = if frac >= i64::from(WONE) {
                (i0 + 1, 0)
            } else {
                (i0, frac as u32)
            };
            let i0 = i0.min(src - 1);
            Tap {
                i0,
                i1: (i0 + 1).min(src - 1),
                w1,
            }
        })
        .collect()
}

/// Precomputed bilinear scaling plan for one plane geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BilinearPlan<const CHANNELS: usize> {
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
    x_taps: Vec<Tap>,
    y_taps: Vec<Tap>,
}

impl<const CHANNELS: usize> BilinearPlan<CHANNELS> {
    /// Plan a `src_w x src_h -> dst_w x dst_h` scale (dimensions in pixels).
    ///
    /// # Errors
    ///
    /// [`ColorConvError::EmptyFrame`] if any dimension is zero.
    pub fn new(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Result<Self, ColorConvError> {
        if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 || CHANNELS == 0 {
            return Err(ColorConvError::EmptyFrame);
        }
        Ok(Self {
            src_w: src_w as usize,
            src_h: src_h as usize,
            dst_w: dst_w as usize,
            dst_h: dst_h as usize,
            x_taps: axis_taps(src_w, dst_w),
            y_taps: axis_taps(src_h, dst_h),
        })
    }

    /// Destination height in rows.
    #[must_use]
    pub const fn dst_height(&self) -> usize {
        self.dst_h
    }

    /// Source rows `(top, bottom)` destination row `dy` reads — lets a caller hand
    /// [`Self::scale_row`] only those two rows (e.g. from a streaming source).
    #[must_use]
    pub fn source_rows(&self, dy: usize) -> Option<(usize, usize)> {
        self.y_taps.get(dy).map(|t| (t.i0 as usize, t.i1 as usize))
    }

    /// Produce destination row `dy` from its two source rows (see [`Self::source_rows`]).
    ///
    /// `top`/`bottom` hold at least `src_w * CHANNELS` bytes, `dst` at least
    /// `dst_w * CHANNELS` bytes; out-of-range `dy` or short slices write nothing.
    pub fn scale_row(&self, dy: usize, top: &[u8], bottom: &[u8], dst: &mut [u8]) {
        let row_in = self.src_w * CHANNELS;
        let row_out = self.dst_w * CHANNELS;
        let (Some(ty), Some(top), Some(bottom), Some(dst)) = (
            self.y_taps.get(dy),
            top.get(..row_in),
            bottom.get(..row_in),
            dst.get_mut(..row_out),
        ) else {
            return;
        };
        let wy1 = ty.w1;
        let wy0 = WONE - wy1;
        let (top_px, _) = top.as_chunks::<CHANNELS>();
        let (bot_px, _) = bottom.as_chunks::<CHANNELS>();
        let (out_px, _) = dst.as_chunks_mut::<CHANNELS>();
        for (out, tx) in out_px.iter_mut().zip(&self.x_taps) {
            // Indices come from `axis_taps(src_w, ..)`, so they are `< src_w`; `get` keeps
            // the loop panic-free without a per-channel bounds check.
            let (Some(a), Some(b), Some(c), Some(d)) = (
                top_px.get(tx.i0 as usize),
                top_px.get(tx.i1 as usize),
                bot_px.get(tx.i0 as usize),
                bot_px.get(tx.i1 as usize),
            ) else {
                return;
            };
            let wx1 = tx.w1;
            let wx0 = WONE - wx1;
            for ch in 0..CHANNELS {
                let t = u32::from(a[ch]) * wx0 + u32::from(b[ch]) * wx1;
                let btm = u32::from(c[ch]) * wx0 + u32::from(d[ch]) * wx1;
                out[ch] = ((t * wy0 + btm * wy1 + (1 << (2 * WBITS - 1))) >> (2 * WBITS)) as u8;
            }
        }
    }

    /// Scale a whole strided plane (sequential loop over [`Self::scale_row`]).
    ///
    /// # Errors
    ///
    /// [`ColorConvError`] if a stride or buffer is too small for the planned geometry.
    pub fn scale(
        &self,
        src: &[u8],
        src_stride: usize,
        dst: &mut [u8],
        dst_stride: usize,
    ) -> Result<(), ColorConvError> {
        check_plane(src.len(), src_stride, self.src_w * CHANNELS, self.src_h)?;
        check_plane(dst.len(), dst_stride, self.dst_w * CHANNELS, self.dst_h)?;
        for (dy, ty) in self.y_taps.iter().enumerate() {
            let top = &src[ty.i0 as usize * src_stride..];
            let bottom = &src[ty.i1 as usize * src_stride..];
            self.scale_row(dy, top, bottom, &mut dst[dy * dst_stride..]);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "scale_tests.rs"]
mod tests;
