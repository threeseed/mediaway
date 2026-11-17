//! Unit tests for `BufferPool`.

#![cfg(test)]

use bytes::BufMut;

use super::BufferPool;

#[test]
fn take_allocates_then_reuses_recycled_buffer() {
    let mut pool = BufferPool::new(1024, 4);
    assert!(pool.is_empty());
    let mut buf = pool.take();
    assert!(buf.capacity() >= 1024);
    buf.put_slice(b"hello");
    let ptr = buf.as_ptr();
    pool.recycle(buf);
    assert_eq!(pool.len(), 1);

    let again = pool.take();
    assert!(again.is_empty(), "recycled buffers come back cleared");
    assert_eq!(again.as_ptr(), ptr, "same allocation reused");
}

#[test]
fn recycle_respects_max_pooled_and_min_capacity() {
    let mut pool = BufferPool::new(64, 1);
    pool.recycle(bytes::BytesMut::with_capacity(8)); // too small
    assert!(pool.is_empty());
    pool.recycle(bytes::BytesMut::with_capacity(64));
    pool.recycle(bytes::BytesMut::with_capacity(64)); // over max_pooled
    assert_eq!(pool.len(), 1);
}

#[test]
fn recycle_bytes_reclaims_only_unique_allocations() {
    let mut pool = BufferPool::new(32, 4);
    let mut buf = pool.take();
    buf.put_slice(&[7; 32]);
    let frozen = buf.freeze();
    let shared = frozen.slice(4..8);
    assert!(!pool.recycle_bytes(frozen), "a live slice keeps it shared");
    assert!(pool.is_empty());
    drop(shared);

    let mut buf = pool.take();
    buf.put_slice(&[1; 40]);
    assert!(pool.recycle_bytes(buf.freeze()));
    assert_eq!(pool.len(), 1);
    assert!(pool.take().capacity() >= 32);
}
