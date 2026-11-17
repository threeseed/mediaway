//! Unit tests for the MP4 `Read + Seek` adapters (sibling of `mp4_io.rs`).

#![cfg(test)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "test modules may unwrap"
)]

use super::*;
use iso_bmff::isobmff::{ProgressiveTrack, StblSample, write_ftyp, write_indexed_moov};
use iso_bmff::{Codec, Rational, Track};
use std::io::Cursor;

/// Generated `ftyp` + `mdat` + trailing `moov` with one video track.
fn moov_at_end(n: usize) -> Vec<u8> {
    let t = Track {
        id: 0,
        codec: Codec::H264,
        time_base: Rational::new(1, 90_000),
        width: 64,
        height: 48,
        extra_data: mediaway_common::Bytes::new(),
    };
    let mut file = Vec::new();
    write_ftyp(&mut file, std::slice::from_ref(&t));
    let body = file.len() as u64 + 8;
    let mut payload = Vec::new();
    let mut samples = Vec::new();
    for i in 0..n {
        let size = 200 + (i as u32 % 4) * 50;
        samples.push(StblSample {
            offset: body + payload.len() as u64,
            size,
            dts: i as i64 * 3000,
            duration: 3000,
            cto: 0,
            key: i == 0,
            discard: false,
        });
        payload.extend((0..size).map(|b| (i as u8) ^ (b as u8)));
    }
    file.extend_from_slice(&(payload.len() as u32 + 8).to_be_bytes());
    file.extend_from_slice(b"mdat");
    file.extend_from_slice(&payload);
    write_indexed_moov(
        &mut file,
        &[ProgressiveTrack {
            track: &t,
            samples: &samples,
            edits: &[],
        }],
    );
    file
}

fn read_all(file: Vec<u8>) -> Vec<Packet> {
    let mut d = SeekDemuxer::open(Cursor::new(file)).unwrap();
    assert_eq!(d.streams().len(), 1);
    let mut out = Vec::new();
    while let Some(p) = d.next_packet().unwrap() {
        out.push(p);
    }
    out
}

#[test]
fn seek_demuxer_reads_moov_at_end_file() {
    let packets = read_all(moov_at_end(300));
    assert_eq!(packets.len(), 300);
    for (i, p) in packets.iter().enumerate() {
        assert_eq!(p.dts, i as i64 * 3000);
        assert_eq!(p.payload[1], (i as u8) ^ 1);
    }
}

#[test]
fn faststart_copy_matches_and_demuxes_the_same() {
    let src = moov_at_end(50);
    let mut out = Vec::new();
    assert!(faststart(Cursor::new(src.clone()), &mut out).unwrap());
    assert_eq!(out.len(), src.len());
    assert_eq!(&out[4..8], b"ftyp");
    let moov_at = u32::from_be_bytes(out[0..4].try_into().unwrap()) as usize;
    assert_eq!(&out[moov_at + 4..moov_at + 8], b"moov");
    // Same bytes as the sans-io whole-buffer path.
    let core = iso_bmff::isobmff::faststart(&src).unwrap().unwrap();
    assert_eq!(out, core);
    let a: Vec<_> = read_all(src).into_iter().map(|p| p.payload).collect();
    let b: Vec<_> = read_all(out.clone())
        .into_iter()
        .map(|p| p.payload)
        .collect();
    assert_eq!(a, b);
    // Second pass: already faststart, copied unchanged.
    let mut again = Vec::new();
    assert!(!faststart(Cursor::new(out.clone()), &mut again).unwrap());
    assert_eq!(again, out);
}
