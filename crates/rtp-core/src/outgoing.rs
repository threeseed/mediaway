//! Zero-copy outgoing RTP packets: an inline payload prefix (FU indicator /
//! FU header bytes) plus a [`Bytes`] slice of the caller's NAL unit, so
//! fragmentation never copies NAL bytes. The only copy is the final
//! serialization into the caller's send buffer ([`OutgoingPacket::write`] /
//! [`OutgoingPacket::write_into`]).

#![forbid(unsafe_code)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "crate-private helpers used by h264.rs/hevc.rs"
)]

use std::ops::Range;

use bytes::{BufMut, Bytes, BytesMut};

use crate::error::Error;
use crate::header::{HEADER_LEN, RtpHeader, RtpPacket};

/// Max payload prefix this crate emits (HEVC FU: 2-byte payload header + FU header).
const MAX_PREFIX_LEN: usize = 3;

/// Up to 3 inline payload-prefix bytes placed before an [`OutgoingPacket`]'s body.
///
/// E.g. the H.264 FU indicator + FU header (2 bytes) or the HEVC
/// payload header + FU header (3 bytes). Empty for single-NAL-unit packets.
/// Stored inline (`Copy`, no heap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PayloadPrefix {
    bytes: [u8; MAX_PREFIX_LEN],
    len: u8,
}

impl PayloadPrefix {
    /// No prefix (single-NAL-unit packet).
    pub const EMPTY: Self = Self {
        bytes: [0; MAX_PREFIX_LEN],
        len: 0,
    };

    /// Prefix bytes, `0..=3` long.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    pub(crate) const fn two(a: u8, b: u8) -> Self {
        Self {
            bytes: [a, b, 0],
            len: 2,
        }
    }

    pub(crate) const fn three(a: u8, b: u8, c: u8) -> Self {
        Self {
            bytes: [a, b, c],
            len: 3,
        }
    }
}

/// One outgoing RTP packet whose payload is `prefix ++ body`, where `body` is
/// a refcounted [`Bytes`] slice of the packetized NAL unit (no copy).
///
/// Produced by `h264::Packetizer::packetize_bytes` /
/// `hevc::Packetizer::packetize_bytes`. Serialize with [`Self::write`]
/// (append to a `Vec`) or [`Self::write_into`] (fill a caller-owned slice,
/// e.g. a reused UDP send buffer); both copy each byte exactly once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingPacket {
    /// Fixed RTP header for this packet.
    pub header: RtpHeader,
    /// Inline bytes preceding `body` in the payload (FU framing), or empty.
    pub prefix: PayloadPrefix,
    /// Remaining payload bytes — a slice sharing the NAL unit's allocation.
    pub body: Bytes,
}

impl OutgoingPacket {
    /// RTP payload length in bytes (`prefix + body`).
    #[must_use]
    pub fn payload_len(&self) -> usize {
        self.prefix.as_bytes().len() + self.body.len()
    }

    /// Total serialized length in bytes (12-byte header + payload).
    #[must_use]
    pub fn wire_len(&self) -> usize {
        HEADER_LEN + self.payload_len()
    }

    /// Append header + prefix + body to `out`. Byte-identical to
    /// [`RtpPacket::write`] for the equivalent contiguous payload.
    ///
    /// # Errors
    ///
    /// [`Error::PayloadTypeOutOfRange`] — see [`RtpHeader::write`].
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        out.reserve(self.wire_len());
        self.header.write(out)?;
        out.extend_from_slice(self.prefix.as_bytes());
        out.extend_from_slice(&self.body);
        Ok(())
    }

    /// Serialize into the start of `out`; returns the number of bytes written
    /// ([`Self::wire_len`]). No allocation.
    ///
    /// # Errors
    ///
    /// - [`Error::BufferTooShort`] if `out.len() < self.wire_len()` (nothing is
    ///   written in that case).
    /// - [`Error::PayloadTypeOutOfRange`] — see [`RtpHeader::write`].
    pub fn write_into(&self, out: &mut [u8]) -> Result<usize, Error> {
        let needed = self.wire_len();
        if out.len() < needed {
            return Err(Error::BufferTooShort {
                needed,
                got: out.len(),
            });
        }
        let header = self.header.to_bytes()?;
        let mut dst = &mut out[..needed];
        dst.put_slice(&header);
        dst.put_slice(self.prefix.as_bytes());
        dst.put_slice(&self.body);
        Ok(needed)
    }

    /// Convert into a contiguous [`RtpPacket`].
    ///
    /// **Copies** the payload when `prefix` is non-empty (one allocation of
    /// `payload_len` bytes); free (refcount move) for single-NAL-unit packets.
    #[must_use]
    pub fn into_rtp_packet(self) -> RtpPacket {
        let payload = if self.prefix.as_bytes().is_empty() {
            self.body
        } else {
            let mut buf = BytesMut::with_capacity(self.payload_len());
            buf.put_slice(self.prefix.as_bytes());
            buf.put_slice(&self.body);
            buf.freeze()
        };
        RtpPacket {
            header: self.header,
            payload,
        }
    }
}

/// Split `body_len` bytes into consecutive ranges of at most `capacity` bytes,
/// calling `f(is_first, is_last, range)` for each. At least one call is made
/// only when `body_len > 0`.
pub(crate) fn for_each_fragment(
    body_len: usize,
    capacity: usize,
    mut f: impl FnMut(bool, bool, Range<usize>),
) {
    let mut offset = 0;
    while offset < body_len {
        let end = (offset + capacity).min(body_len);
        f(offset == 0, end == body_len, offset..end);
        offset = end;
    }
}

/// Lay out every fragment payload (`prefix ++ nal[body_start + range]`)
/// contiguously in one allocation and return per-fragment `Bytes` slices of it.
/// One allocation + one copy of the NAL bytes per call (instead of one `Vec`
/// per fragment).
pub(crate) fn fragment_payloads_copied(
    nal: &[u8],
    body_start: usize,
    capacity: usize,
    mut prefix_for: impl FnMut(bool, bool) -> PayloadPrefix,
    mut emit: impl FnMut(Bytes, bool),
) {
    let body_len = nal.len() - body_start;
    let count = body_len.div_ceil(capacity);
    let mut buf = BytesMut::with_capacity(body_len + count * MAX_PREFIX_LEN);
    let mut spans: Vec<(Range<usize>, bool)> = Vec::with_capacity(count);
    for_each_fragment(body_len, capacity, |first, last, r| {
        let start = buf.len();
        buf.put_slice(prefix_for(first, last).as_bytes());
        buf.put_slice(&nal[body_start + r.start..body_start + r.end]);
        spans.push((start..buf.len(), last));
    });
    let shared = buf.freeze();
    for (span, last) in spans {
        emit(shared.slice(span), last);
    }
}

#[cfg(test)]
#[path = "outgoing_tests.rs"]
mod tests;
