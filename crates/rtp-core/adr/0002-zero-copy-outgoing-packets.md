# ADR-0002: Zero-copy outgoing packets (`OutgoingPacket`)

- **Status**: Accepted
- **Date**: 2026-11-17
- **Deciders**: @dev-nyxie (+ agent)
- **Crate**: `rtp-core`

## Context

`packetize(&[u8]) -> Vec<RtpPacket>` copied every FU fragment into its own
`Vec` (prefix + fragment), and `RtpPacket::write` copied the payload again
into the send buffer. `RtpPacket::payload` is a contiguous `Bytes`, which
`Depacketizer` callers rely on, so it cannot carry a split prefix + body.

## Decision

> Add a separate send-side type instead of changing `RtpPacket`.

- `OutgoingPacket { header, prefix: PayloadPrefix, body: Bytes }` —
  `PayloadPrefix` is ≤3 inline bytes (`Copy`), `body` is a `Bytes::slice`
  of the NAL unit.
- `Packetizer::packetize_bytes(&Bytes, ..) -> Vec<OutgoingPacket>` (H.264
  and HEVC): no NAL byte is copied before serialization.
- `OutgoingPacket::write` / `write_into(&mut [u8])` and
  `RtpPacket::write_into` serialize directly into the caller's buffer
  (`write_into` never allocates). `into_rtp_packet` is the documented
  copying bridge.
- `packetize(&[u8])` keeps its signature but lays out all fragments in one
  shared allocation (one copy, one allocation per NAL).

## Alternatives Considered

| Alternative | Why not |
|-------------|---------|
| Add a prefix field to `RtpPacket` | Silently changes what `payload` means for receive-side users |
| Change `packetize` return type | Breaking; slice-input callers still need one copy anyway |

## Consequences

- Wire bytes are unchanged (reference tests in `outgoing_tests.rs`).
- Two packet types: `RtpPacket` (contiguous, parse/receive) and
  `OutgoingPacket` (split, send).
