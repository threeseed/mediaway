//! Integration: zero-copy demux, seekable demux of a moov-at-end file, and faststart.
//!
//! Fixtures are generated here (`write_ftyp` + an `mdat` + `write_indexed_moov`), not committed.

#![forbid(unsafe_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::missing_const_for_fn,
    reason = "integration tests may unwrap"
)]

use bytes::Bytes;
use iso_bmff::isobmff::{
    FourCc, ProgressiveTrack, StblSample, TopLevelBox, faststart, parse_header, parse_moov,
    relocate_moov, write_ftyp, write_indexed_moov,
};
use iso_bmff::{Codec, Demuxer, Rational, Sample, Track};

fn tracks() -> [Track; 2] {
    [
        Track {
            id: 0,
            codec: Codec::H264,
            time_base: Rational::new(1, 90_000),
            width: 64,
            height: 48,
            extra_data: Bytes::new(),
        },
        Track {
            id: 1,
            codec: Codec::Aac,
            time_base: Rational::new(1, 48_000),
            width: 0,
            height: 0,
            extra_data: Bytes::from_static(&[0x11, 0x90]),
        },
    ]
}

/// `ftyp`, `mdat` (interleaved video/audio, distinct byte patterns), then `moov`.
fn moov_at_end(n: usize) -> Vec<u8> {
    let t = tracks();
    let mut file = Vec::new();
    write_ftyp(&mut file, &t);
    let mut payload = Vec::new();
    let mdat_body = file.len() as u64 + 8;
    let (mut v, mut a) = (Vec::new(), Vec::new());
    for i in 0..n {
        let vs = 100 + (i % 7) as u32 * 13;
        v.push(StblSample {
            offset: mdat_body + payload.len() as u64,
            size: vs,
            dts: i as i64 * 3000,
            duration: 3000,
            cto: 0,
            key: i % 5 == 0,
            discard: false,
        });
        payload.extend((0..vs).map(|b| (i as u8).wrapping_mul(3) ^ b as u8));
        let as_ = 40 + (i % 3) as u32;
        a.push(StblSample {
            offset: mdat_body + payload.len() as u64,
            size: as_,
            dts: i as i64 * 1024,
            duration: 1024,
            cto: 0,
            key: true,
            discard: false,
        });
        payload.extend((0..as_).map(|b| 0x80 ^ (i as u8) ^ b as u8));
    }
    file.extend_from_slice(&(payload.len() as u32 + 8).to_be_bytes());
    file.extend_from_slice(b"mdat");
    file.extend_from_slice(&payload);
    write_indexed_moov(
        &mut file,
        &[
            ProgressiveTrack {
                track: &t[0],
                samples: &v,
                edits: &[],
            },
            ProgressiveTrack {
                track: &t[1],
                samples: &a,
                edits: &[],
            },
        ],
    );
    file
}

fn top_level(file: &[u8]) -> Vec<FourCc> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < file.len() {
        let h = parse_header(&file[pos..]).unwrap();
        out.push(h.typ);
        pos += h.size;
    }
    out
}

fn push_all(file: &[u8], chunk: usize) -> Vec<Sample> {
    let mut d = Demuxer::new();
    let mut out = Vec::new();
    for c in file.chunks(chunk) {
        d.push_bytes(c);
        while let Some(s) = d.poll_packet() {
            out.push(s);
        }
    }
    out
}

/// Drive a seekable demuxer over an in-memory "file", honoring seek requests.
/// Returns samples and the number of bytes pushed.
fn seek_all(file: &[u8], chunk: usize) -> (Vec<Sample>, usize) {
    let mut d = Demuxer::new_seekable();
    let mut out = Vec::new();
    let (mut pos, mut pushed) = (0usize, 0usize);
    loop {
        while let Some(s) = d.poll_packet() {
            out.push(s);
        }
        if let Some(off) = d.poll_seek() {
            pos = off as usize;
            continue;
        }
        if pos >= file.len() {
            break;
        }
        let end = (pos + chunk).min(file.len());
        d.push_bytes(&file[pos..end]);
        pushed += end - pos;
        pos = end;
    }
    (out, pushed)
}

fn by_track(mut s: Vec<Sample>) -> Vec<(u32, i64, Bytes)> {
    s.sort_by_key(|x| (x.stream_id, x.dts));
    s.into_iter()
        .map(|x| (x.stream_id, x.dts, x.payload))
        .collect()
}

#[test]
fn push_mode_samples_are_slices_of_one_buffer() {
    let file = moov_at_end(10);
    let mut d = Demuxer::new();
    d.push_bytes(&file);
    let mut prev: Option<Bytes> = None;
    let mut n = 0;
    while let Some(s) = d.poll_packet() {
        if s.stream_id == 0 {
            if let Some(p) = prev.as_ref() {
                // Interleaved audio sits between consecutive video samples, same allocation.
                assert!(s.payload.as_ptr() > p.as_ptr());
            }
            prev = Some(s.payload);
            n += 1;
        }
    }
    assert_eq!(n, 10);
}

#[test]
fn adjacent_samples_share_the_input_buffer() {
    let file = moov_at_end(4);
    let mut d = Demuxer::new();
    d.push_bytes(&file);
    let mut all = Vec::new();
    while let Some(s) = d.poll_packet() {
        all.push(s);
    }
    // Video 0 is immediately followed by audio 0 in the mdat: contiguous memory, no copies.
    let v0 = all.iter().find(|s| s.stream_id == 0 && s.dts == 0).unwrap();
    let a0 = all.iter().find(|s| s.stream_id == 1 && s.dts == 0).unwrap();
    assert_eq!(
        v0.payload.as_ptr().wrapping_add(v0.payload.len()),
        a0.payload.as_ptr()
    );
}

#[test]
fn seekable_mode_matches_push_mode_without_retaining_mdat() {
    let file = moov_at_end(40);
    let pushed_ref = by_track(push_all(&file, 4096));
    for chunk in [7usize, 64, 4096, file.len()] {
        let (got, pushed) = seek_all(&file, chunk);
        assert_eq!(by_track(got), pushed_ref, "chunk {chunk}");
        // The mdat is skipped once while looking for the moov, then read once by offsets.
        assert!(
            pushed <= file.len() + 2 * chunk,
            "chunk {chunk}: pushed {pushed}"
        );
    }
}

#[test]
fn seekable_mode_reads_samples_in_file_order() {
    let file = moov_at_end(6);
    let (got, _) = seek_all(&file, 4096);
    let ids: Vec<u32> = got.iter().map(|s| s.stream_id).collect();
    assert_eq!(ids, [0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1]);
}

#[test]
fn push_mode_never_asks_to_seek() {
    let file = moov_at_end(3);
    let mut d = Demuxer::new();
    d.push_bytes(&file);
    assert_eq!(d.poll_seek(), None);
}

#[test]
fn faststart_moves_moov_first_and_keeps_samples() {
    let file = moov_at_end(25);
    let fast = faststart(&file).unwrap().expect("moov was at the end");
    assert_eq!(fast.len(), file.len());
    let order = top_level(&fast);
    assert_eq!(
        order,
        [FourCc(*b"ftyp"), FourCc(*b"moov"), FourCc(*b"mdat")]
    );
    let want = by_track(push_all(&file, 4096));
    assert_eq!(by_track(push_all(&fast, 333)), want);
    assert_eq!(by_track(seek_all(&fast, 333).0), want);
    // Already faststart: nothing to do.
    assert_eq!(faststart(&fast).unwrap(), None);
}

#[test]
fn faststart_widens_stco_past_four_gigabytes() {
    let t = tracks();
    let base = u64::from(u32::MAX) - 20;
    let samples = [
        StblSample {
            offset: 1_000,
            size: 10,
            dts: 0,
            duration: 3000,
            cto: 0,
            key: true,
            discard: false,
        },
        StblSample {
            offset: base,
            size: 10,
            dts: 3000,
            duration: 3000,
            cto: 0,
            key: false,
            discard: false,
        },
    ];
    let mut moov = Vec::new();
    write_indexed_moov(
        &mut moov,
        &[ProgressiveTrack {
            track: &t[0],
            samples: &samples,
            edits: &[],
        }],
    );
    let old = TopLevelBox {
        typ: FourCc(*b"moov"),
        offset: base + 10,
        size: moov.len() as u64,
    };
    let out = relocate_moov(&moov, 500, old).unwrap();
    assert!(out.len() > moov.len(), "stco widened to co64");
    let parsed = parse_moov(&out[8..]);
    let offs: Vec<u64> = parsed[0].samples.iter().map(|s| s.offset).collect();
    assert_eq!(offs, [1_000 + out.len() as u64, base + out.len() as u64]);
}
