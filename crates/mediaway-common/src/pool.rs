//! Single-owner CPU byte-buffer pool (see crate ADR-0001).

use bytes::{Bytes, BytesMut};

/// Recycles CPU byte buffers ([`BytesMut`]) so steady-state packet/frame
/// assembly does not hit the allocator once per item.
///
/// **Ownership model:** single owner, explicit return — `&mut self` methods,
/// no lock, no `Arc`, no `dyn`. Typical use is one pool per muxer / packetizer
/// / I/O adapter loop:
///
/// 1. [`take`](Self::take) a cleared buffer (capacity ≥ `buf_capacity`),
/// 2. fill it, optionally [`BytesMut::freeze`] it into [`Bytes`] and hand it on,
/// 3. give it back with [`recycle`](Self::recycle) /
///    [`recycle_bytes`](Self::recycle_bytes) once the consumer is done.
///
/// Buffers never returned are simply freed on drop (no leak, just no reuse).
/// [`recycle_bytes`](Self::recycle_bytes) only reclaims a `Bytes` whose
/// allocation is uniquely owned (no other clone/slice alive); otherwise the
/// buffer is dropped and the next `take` allocates.
///
/// **Cost contract:** `take` on an empty pool allocates `buf_capacity` bytes;
/// on a hit it is a `Vec::pop`. Recycled buffers are cleared (length 0) but
/// their contents are **not zeroed** — never expose uninitialized capacity.
/// The pool holds at most `max_pooled` buffers; extra returns are freed.
#[derive(Debug)]
pub struct BufferPool {
    free: Vec<BytesMut>,
    buf_capacity: usize,
    max_pooled: usize,
}

impl BufferPool {
    /// New, empty pool handing out buffers of at least `buf_capacity` bytes and
    /// retaining up to `max_pooled` returned buffers. Allocates nothing until
    /// the first [`take`](Self::take) / [`recycle`](Self::recycle).
    #[must_use]
    pub const fn new(buf_capacity: usize, max_pooled: usize) -> Self {
        Self {
            free: Vec::new(),
            buf_capacity,
            max_pooled,
        }
    }

    /// Minimum capacity of every buffer [`take`](Self::take) returns.
    #[must_use]
    pub const fn buf_capacity(&self) -> usize {
        self.buf_capacity
    }

    /// Buffers currently idle in the pool.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.free.len()
    }

    /// Whether no idle buffer is available (next `take` allocates).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.free.is_empty()
    }

    /// An empty (`len() == 0`) buffer with capacity ≥ `buf_capacity`, reused
    /// from the pool when possible, freshly allocated otherwise.
    #[must_use]
    pub fn take(&mut self) -> BytesMut {
        self.free
            .pop()
            .unwrap_or_else(|| BytesMut::with_capacity(self.buf_capacity))
    }

    /// Return a buffer for reuse. It is cleared; buffers smaller than
    /// `buf_capacity` (e.g. split-off tails) or beyond `max_pooled` are freed.
    pub fn recycle(&mut self, mut buf: BytesMut) {
        if self.free.len() >= self.max_pooled || buf.capacity() < self.buf_capacity {
            return;
        }
        buf.clear();
        self.free.push(buf);
    }

    /// Return a frozen buffer. Reclaimed only when `bytes` uniquely owns its
    /// allocation ([`Bytes::try_into_mut`]); returns `true` if it was pooled.
    pub fn recycle_bytes(&mut self, bytes: Bytes) -> bool {
        if self.free.len() >= self.max_pooled {
            return false;
        }
        match bytes.try_into_mut() {
            Ok(buf) if buf.capacity() >= self.buf_capacity => {
                self.recycle(buf);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
#[path = "pool_tests.rs"]
mod tests;
