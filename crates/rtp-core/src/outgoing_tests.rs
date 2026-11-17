//! Byte-identity tests: the shared-buffer `packetize` and zero-copy
//! `packetize_bytes` paths must serialize exactly like the pre-optimization
//! per-fragment `Vec` reference implementation below.

#![cfg(test)]
#![allow(clippy::unwrap_used, reason = "unit tests")]

use bytes::Bytes;

use crate::error::Error;
use crate::header::{HEADER_LEN, RtpHeader};
use crate::{h264, hevc};

/// Reference (old) FU fragmentation: one fresh `Vec` per fragment.
fn reference_payloads(nal: &[u8], max: usize, hevc: bool) -> Vec<Vec<u8>> {
    if nal.len() <= max {
        return vec![nal.to_vec()];
    }
    let (prefix_head, nal_type, hdr_len): (Vec<u8>, u8, usize) = if hevc {
        let nal_type = (nal[0] >> 1) & 0x3F;
        let b0 = (nal[0] & 0x81) | (49 << 1);
        (vec![b0, nal[1]], nal_type, 2)
    } else {
        (vec![(nal[0] & 0xE0) | 0x1C], nal[0] & 0x1F, 1)
    };
    let cap = max - prefix_head.len() - 1;
    let body = &nal[hdr_len..];
    let mut out = Vec::new();
    let mut off = 0;
    while off < body.len() {
        let end = (off + cap).min(body.len());
        let mut p = prefix_head.clone();
        p.push((u8::from(off == 0) << 7) | (u8::from(end == body.len()) << 6) | nal_type);
        p.extend_from_slice(&body[off..end]);
        out.push(p);
        off = end;
    }
    out
}

fn reference_wire(nal: &[u8], max: usize, hevc: bool, seq0: u16) -> Vec<Vec<u8>> {
    let payloads = reference_payloads(nal, max, hevc);
    let n = payloads.len();
    payloads
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            let header = RtpHeader {
                marker: i + 1 == n,
                payload_type: 96,
                sequence_number: seq0.wrapping_add(u16::try_from(i).unwrap()),
                timestamp: 1234,
                ssrc: 7,
            };
            let mut w = Vec::new();
            header.write(&mut w).unwrap();
            w.extend_from_slice(&p);
            w
        })
        .collect()
}

fn nal(first: &[u8], len: usize) -> Vec<u8> {
    let mut v = first.to_vec();
    v.extend((0..len).map(|i| (i % 251).to_le_bytes()[0]));
    v
}

#[test]
fn h264_and_hevc_paths_match_reference_bytes() {
    for len in [3usize, 50, 197, 198, 199, 200, 201, 400, 5000] {
        for max in [5usize, 200, 1400] {
            let h = nal(&[0x65], len);
            let e = nal(&[0x26, 0x01], len);
            for (data, is_hevc) in [(&h, false), (&e, true)] {
                let expected = reference_wire(data, max, is_hevc, 65_530);
                let shared = Bytes::from(data.clone());
                let (old_api, zc) = if is_hevc {
                    let mut a = hevc::Packetizer::new(max, 96, 7, 65_530).unwrap();
                    let mut b = hevc::Packetizer::new(max, 96, 7, 65_530).unwrap();
                    (
                        a.packetize(data, 1234, true).unwrap(),
                        b.packetize_bytes(&shared, 1234, true).unwrap(),
                    )
                } else {
                    let mut a = h264::Packetizer::new(max, 96, 7, 65_530).unwrap();
                    let mut b = h264::Packetizer::new(max, 96, 7, 65_530).unwrap();
                    (
                        a.packetize(data, 1234, true).unwrap(),
                        b.packetize_bytes(&shared, 1234, true).unwrap(),
                    )
                };
                assert_eq!(old_api.len(), expected.len());
                assert_eq!(zc.len(), expected.len());
                for ((o, z), exp) in old_api.iter().zip(&zc).zip(&expected) {
                    let mut w = Vec::new();
                    o.write(&mut w).unwrap();
                    assert_eq!(&w, exp, "packetize len={len} max={max} hevc={is_hevc}");

                    let mut w = Vec::new();
                    z.write(&mut w).unwrap();
                    assert_eq!(&w, exp);
                    assert_eq!(z.wire_len(), exp.len());

                    let mut buf = [0u8; 1500];
                    let n = z.write_into(&mut buf).unwrap();
                    assert_eq!(&buf[..n], &exp[..]);
                    let n = o.write_into(&mut buf).unwrap();
                    assert_eq!(&buf[..n], &exp[..]);

                    assert_eq!(z.clone().into_rtp_packet(), *o);
                }
            }
        }
    }
}

#[test]
fn packetize_bytes_bodies_share_the_nal_allocation() {
    let shared = Bytes::from(nal(&[0x65], 3000));
    let mut pk = h264::Packetizer::new(300, 96, 7, 0).unwrap();
    let packets = pk.packetize_bytes(&shared, 0, true).unwrap();
    let range = shared.as_ptr() as usize..shared.as_ptr() as usize + shared.len();
    for p in &packets {
        assert!(range.contains(&(p.body.as_ptr() as usize)));
        assert_eq!(p.prefix.as_bytes().len(), 2);
    }
}

#[test]
fn write_into_rejects_short_buffer() {
    let mut pk = h264::Packetizer::new(1400, 96, 7, 0).unwrap();
    let packets = pk
        .packetize_bytes(&Bytes::from_static(&[0x65, 1, 2]), 0, true)
        .unwrap();
    let mut buf = [0u8; HEADER_LEN + 2];
    assert!(matches!(
        packets[0].write_into(&mut buf),
        Err(Error::BufferTooShort {
            needed: 15,
            got: 14
        })
    ));
}
