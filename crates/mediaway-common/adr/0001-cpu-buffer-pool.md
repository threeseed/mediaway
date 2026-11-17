# ADR-0001: Single-owner CPU byte-buffer pool

- **Status**: Accepted
- **Date**: 2026-11-17
- **Deciders**: @dev-nyxie (+ agent)
- **Crate**: mediaway-common

## Context

Mux and packetize paths (MPEG-TS, RTP, WebM, FLV) build one output buffer
per packet / frame. Steady-state streaming therefore hits the allocator once
per item even though buffer sizes are stable. Callers need a low-level,
dependency-free way to recycle CPU buffers that stays usable from sans-io
cores and does not impose a runtime, lock, or trait object.

## Decision

> We adopt `mediaway_common::BufferPool`: a single-owner pool of
> `bytes::BytesMut` with explicit return.

- `take(&mut self) -> BytesMut` (cleared, capacity ≥ `buf_capacity`).
- `recycle(&mut self, BytesMut)` and `recycle_bytes(&mut self, Bytes) -> bool`
  (the latter reclaims only uniquely owned allocations via
  `Bytes::try_into_mut`).
- Bounded by `max_pooled`; undersized buffers are dropped, not pooled.
- No lock, no `Arc`, no `Box`/`dyn`; contents are not zeroed (cleared only).
- Not wired into existing APIs in this change: doing so needs signature
  changes (callers must hand buffers back). It ships as a first-class
  low-level API; adoption is per-crate follow-up.

## Alternatives Considered

| Alternative | Why not |
|-------------|---------|
| Return-on-drop guard (`PooledBuf` holding `Rc`/`Arc<Mutex<…>>`) | Refcount + lock on every buffer; guard type leaks into every API; cannot freeze into `Bytes` without losing the return path |
| Thread-shared lock-free pool (`crossbeam` etc.) | New dependency; no current multi-thread consumer |
| Per-crate ad-hoc `Vec` reuse only | Already done where internal (WebM scratch); no shared low-level API for adapters |

## Consequences

### Positive

- Zero new dependencies; allocation-free steady state for callers that recycle.
- Composes with `Bytes` hand-offs (freeze → consume → `recycle_bytes`).

### Negative / Trade-offs

- Caller must return buffers explicitly; forgotten returns only lose reuse.
- `recycle_bytes` silently fails (returns `false`) while slices are alive.
- Not `Sync`-shared; one pool per owner/thread.

## References

- `crates/mediaway-common/src/pool.rs`
- `docs/spec/zero-cost-abstractions.md`, `docs/spec/caveats-and-clarity.md`
- wiki: `docs/ai/wiki/common/buffer-pool.md`
