//! Unit tests for the top-level MPEG-TS `Muxer`.

#![cfg(test)]
#![allow(clippy::unwrap_used, reason = "unit tests")]

use super::Muxer;
use crate::error::Error;
use crate::packet::PACKET_LEN;
use crate::types::{ElementaryStream, StreamType};

fn one_video_stream() -> Vec<ElementaryStream> {
    vec![ElementaryStream {
        pid: 256,
        stream_type: StreamType::H264,
    }]
}

#[test]
fn new_rejects_reserved_pmt_pid() {
    assert!(matches!(
        Muxer::new(1, 0, &one_video_stream()),
        Err(Error::InvalidPid(0))
    ));
    assert!(matches!(
        Muxer::new(1, 1, &one_video_stream()),
        Err(Error::InvalidPid(1))
    ));
}

#[test]
fn new_rejects_reserved_stream_pid() {
    let streams = vec![ElementaryStream {
        pid: 1,
        stream_type: StreamType::Aac,
    }];
    assert!(Muxer::new(1, 4096, &streams).is_err());
}

#[test]
fn write_pat_pmt_produces_two_aligned_packets() {
    let mut mux = Muxer::new(1, 4096, &one_video_stream()).unwrap();
    let mut out = Vec::new();
    mux.write_pat_pmt(&mut out);
    assert_eq!(out.len(), 2 * PACKET_LEN);
    assert_eq!(out[0], 0x47);
    assert_eq!(out[PACKET_LEN], 0x47);
}

#[test]
fn write_access_unit_rejects_unregistered_pid() {
    let mut mux = Muxer::new(1, 4096, &one_video_stream()).unwrap();
    let mut out = Vec::new();
    let err = mux
        .write_access_unit(999, b"data", 0, None, false, &mut out)
        .unwrap_err();
    assert!(matches!(err, Error::UnknownPid(999)));
}

#[test]
fn write_access_unit_produces_packet_aligned_output() {
    let mut mux = Muxer::new(1, 4096, &one_video_stream()).unwrap();
    let mut out = Vec::new();
    mux.write_access_unit(256, b"keyframe payload", 90_000, None, true, &mut out)
        .unwrap();
    assert_eq!(out.len() % PACKET_LEN, 0);
    assert!(!out.is_empty());
}

/// Pre-optimization reference: PES header + AU concatenated in a `Vec`, then
/// packetized. The zero-concat path must stay byte-identical.
#[test]
fn access_unit_output_matches_concatenating_reference() {
    use crate::packet::write_ts_packets;
    use crate::pes::{build_pes_header, stream_id_for};
    for len in [0usize, 1, 165, 166, 170, 171, 183, 184, 500, 70_000] {
        let data: Vec<u8> = (0..len).map(|i| (i % 251).to_le_bytes()[0]).collect();
        for (dts, ra) in [(None, false), (Some(1000u64), true)] {
            let mut mux = Muxer::new(1, 4096, &one_video_stream()).unwrap();
            let mut actual = Vec::new();
            mux.write_access_unit(256, &data, 3000, dts, ra, &mut actual)
                .unwrap();

            let mut expected_payload =
                build_pes_header(stream_id_for(true), len, 3000, dts).to_vec();
            expected_payload.extend_from_slice(&data);
            let mut expected = Vec::new();
            let mut cc = 0;
            write_ts_packets(&mut expected, 256, &mut cc, &expected_payload, true, ra);
            assert_eq!(actual, expected, "len={len}");
        }
    }
}
