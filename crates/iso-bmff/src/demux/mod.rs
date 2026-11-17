//! Sans-IO MP4 demuxer — fMP4 + unfragmented `stbl`; optional `ClearKey` CENC.
//!
//! Two input modes (ADR-0010):
//!
//! - **Push** ([`Demuxer::new`]): the caller feeds the file front to back. An unfragmented file
//!   with `moov` after `mdat` makes the demuxer keep every `mdat` until the `moov` arrives.
//! - **Seekable** ([`Demuxer::new_seekable`]): the demuxer may ask the caller to continue from
//!   another absolute offset ([`Demuxer::poll_seek`]). `mdat` payloads are skipped while the
//!   `moov` is found, then samples are read by their `stbl` offsets — nothing is retained.
//!
//! Sample payloads are [`Bytes`] slices of the pushed data (no per-sample copy); the only copy
//! is a protected sample being decrypted in place (see [`Demuxer::set_decryption_key`]).

#![forbid(unsafe_code)]

use crate::isobmff::{
    SencSample, StblSample, TrackEncryption, TrunSample, parse_header, parse_moof, parse_moov,
    parse_senc,
};
use crate::types::{Bytes, Sample, Track};
use crate::{INLINE_SAMPLES, INLINE_TRACKS};
use bytes::{Buf, BytesMut};
use iso_cenc::{Pattern, decrypt_cenc, iv_from_8, iv_from_constant};
use smallvec::SmallVec;
use std::collections::VecDeque;

#[derive(Debug, Clone)]
struct PendingStbl {
    stream_id: u32,
    sample: StblSample,
}

#[derive(Debug, Clone)]
struct MdatPart {
    file_offset: u64,
    data: Bytes,
}

/// Sans-IO demuxer: `push_bytes` → `poll_packet` (and, when seekable, `poll_seek`).
#[derive(Debug, Default)]
pub struct Demuxer {
    /// Unconsumed input. Complete boxes are split off the front (`split_to` + `freeze`, O(1)),
    /// so there is no compaction `memmove`; only an incomplete tail stays here.
    buffer: BytesMut,
    /// Absolute file offset of `buffer[0]`.
    file_base: u64,
    streams: SmallVec<[Track; INLINE_TRACKS]>,
    track_encryption: SmallVec<[Option<TrackEncryption>; INLINE_TRACKS]>,
    packets: VecDeque<Sample>,
    pending: SmallVec<[TrunSample; INLINE_SAMPLES]>,
    pending_senc: Vec<SencSample>,
    track_id: u32,
    base_dts: u64,
    stbl_pending: VecDeque<PendingStbl>,
    mdat_parts: Vec<MdatPart>,
    /// `ClearKey` content key (16 bytes) when set.
    decryption_key: Option<[u8; 16]>,
    /// Seekable mode: the caller honors [`Demuxer::poll_seek`].
    seekable: bool,
    /// Seekable mode: offset the caller must continue from (input is ignored until taken).
    seek_request: Option<u64>,
    /// Seekable mode: requested but not yet taken by [`Demuxer::poll_seek`].
    seek_unread: bool,
    /// Seekable mode: top-level offset to resume the box walk at once `stbl` samples are read.
    resume_offset: u64,
}

impl Demuxer {
    /// Empty push-mode demuxer (input front to back, never asks to seek).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Empty seekable demuxer: the caller can reposition its input on request.
    ///
    /// After every [`Demuxer::push_bytes`], call [`Demuxer::poll_seek`]; when it returns
    /// `Some(offset)`, the next pushed byte must be the file's byte at `offset`. Bytes pushed
    /// while a seek is outstanding (not yet taken by `poll_seek`) are **dropped**.
    ///
    /// The demuxer asks to skip each `mdat` payload while it looks for the `moov`, so a
    /// moov-at-end file costs header reads plus one seek, not a buffered `mdat`. Unfragmented
    /// samples are then read in file-offset order (interleaved across tracks), seeking only over
    /// gaps; afterwards the walk resumes after the `moov` (fragments, if any, follow).
    #[must_use]
    pub fn new_seekable() -> Self {
        Self {
            seekable: true,
            ..Self::default()
        }
    }

    /// Supply a `ClearKey` AES-128 content key for CENC sample decrypt.
    ///
    /// **Cost:** a protected sample is decrypted in place, so its payload is copied out of the
    /// shared input buffer into its own allocation first (one `memcpy` per protected sample).
    /// Clear samples stay zero-copy slices.
    pub const fn set_decryption_key(&mut self, key: [u8; 16]) {
        self.decryption_key = Some(key);
    }

    /// Clear any previously set decryption key.
    pub const fn clear_decryption_key(&mut self) {
        self.decryption_key = None;
    }

    /// Feed container bytes (the next bytes of the file, or — in seekable mode — the bytes at
    /// the offset last returned by [`Demuxer::poll_seek`]).
    ///
    /// `chunk` is appended to an internal buffer (one copy of the input, inherent to a borrowed
    /// `&[u8]`); emitted sample payloads share that buffer.
    pub fn push_bytes(&mut self, chunk: &[u8]) {
        if self.seek_unread {
            return;
        }
        self.buffer.extend_from_slice(chunk);
        self.step();
    }

    /// Seekable mode: the absolute file offset the caller must continue pushing from, if the
    /// demuxer wants to jump. Always `None` in push mode.
    pub const fn poll_seek(&mut self) -> Option<u64> {
        if self.seek_unread {
            self.seek_unread = false;
            return self.seek_request;
        }
        None
    }

    /// Track list from `moov`.
    #[must_use]
    pub fn streams(&self) -> &[Track] {
        &self.streams
    }

    /// Next Sample.
    pub fn poll_packet(&mut self) -> Option<Sample> {
        self.packets.pop_front()
    }

    fn step(&mut self) {
        if self.seekable {
            if self.stbl_pending.is_empty() {
                self.pump();
            }
            if !self.seek_unread && !self.stbl_pending.is_empty() {
                self.read_stbl_seekable();
            }
        } else {
            self.pump();
            self.emit_stbl();
        }
    }

    fn request_seek(&mut self, offset: u64) {
        self.buffer.clear();
        self.file_base = offset;
        self.seek_request = Some(offset);
        self.seek_unread = true;
    }

    fn pump(&mut self) {
        while self.buffer.len() >= 8 {
            let Some(hdr) = parse_header(&self.buffer) else {
                break;
            };
            if hdr.size < hdr.header_len {
                break;
            }
            let box_offset = self.file_base;
            let wanted = matches!(&hdr.typ.0, b"moov" | b"moof")
                || (&hdr.typ.0 == b"mdat" && !self.pending.is_empty());
            if self.seekable && !wanted && hdr.size > self.buffer.len() {
                // Not needed in full (an `mdat` is reached later by `stbl` offsets): skip it.
                self.request_seek(box_offset.saturating_add(hdr.size as u64));
                break;
            }
            if hdr.size > self.buffer.len() {
                break;
            }
            let bx = self.buffer.split_to(hdr.size).freeze();
            self.file_base = box_offset.saturating_add(hdr.size as u64);
            let body = bx.slice(hdr.header_len..);
            match &hdr.typ.0 {
                b"moov" if self.streams.is_empty() => {
                    self.on_moov(&body);
                    if self.seekable && !self.stbl_pending.is_empty() {
                        self.resume_offset = self.file_base;
                        break;
                    }
                }
                b"moof" => self.on_moof(&body),
                b"mdat" => {
                    let file_off = box_offset.saturating_add(hdr.header_len as u64);
                    let retain = !self.seekable && self.should_retain_mdat();
                    if !self.pending.is_empty() {
                        self.drain_mdat(&body);
                    }
                    if retain {
                        self.mdat_parts.push(MdatPart {
                            file_offset: file_off,
                            data: body,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    fn on_moov(&mut self, body: &[u8]) {
        let tracks = parse_moov(body);
        self.streams.clear();
        self.track_encryption.clear();
        self.stbl_pending.clear();
        for t in tracks {
            let sid = t.info.id;
            for s in t.samples {
                self.stbl_pending.push_back(PendingStbl {
                    stream_id: sid,
                    sample: s,
                });
            }
            self.track_encryption.push(t.encryption);
            self.streams.push(t.info);
        }
        if self.seekable {
            // File order: sequential reads, a seek only over gaps.
            self.stbl_pending
                .make_contiguous()
                .sort_by_key(|p| p.sample.offset);
        }
    }

    fn on_moof(&mut self, body: &[u8]) {
        let info = parse_moof(body);
        self.track_id = info.track_id;
        self.base_dts = info.base_dts;
        self.pending = info.samples;
        let iv_size = self
            .track_encryption
            .get(info.track_id as usize)
            .and_then(|e| e.as_ref())
            .map_or(0, |e| e.per_sample_iv_size);
        // `senc` may sit under `traf` — re-scan moof for it.
        self.pending_senc = find_senc_in_moof(body, iv_size);
    }

    fn should_retain_mdat(&self) -> bool {
        self.streams.is_empty() || !self.stbl_pending.is_empty()
    }

    fn drain_mdat(&mut self, mdat: &Bytes) {
        let samples = std::mem::take(&mut self.pending);
        let senc = std::mem::take(&mut self.pending_senc);
        let mut off = 0usize;
        let mut dts = self.base_dts;
        let tid = self.track_id;
        for (i, s) in samples.into_iter().enumerate() {
            let e = off.saturating_add(s.size as usize);
            if e > mdat.len() {
                break;
            }
            let (payload, decrypt_ok) = self.maybe_decrypt(tid, mdat.slice(off..e), senc.get(i));
            self.packets.push_back(Sample {
                stream_id: tid,
                pts: pts_from_dts(dts as i64, s.cto),
                dts: dts as i64,
                duration: u64::from(s.duration),
                is_keyframe: s.key,
                is_discard: !decrypt_ok,
                payload,
            });
            off = e;
            dts = dts.saturating_add(u64::from(s.duration));
        }
    }

    /// Decrypt a protected sample (copying it out of the shared buffer — the one documented
    /// copy on this path); clear samples pass through as the same slice.
    fn maybe_decrypt(
        &self,
        stream_id: u32,
        payload: Bytes,
        senc: Option<&SencSample>,
    ) -> (Bytes, bool) {
        if let (Some(key), Some(Some(tenc))) = (
            self.decryption_key,
            self.track_encryption.get(stream_id as usize),
        ) && tenc.is_protected
        {
            let mut owned = payload.to_vec();
            let ok = decrypt_sample(&mut owned, key, tenc, senc).is_ok();
            return (Bytes::from(owned), ok);
        }
        (payload, true)
    }

    fn push_stbl_sample(&mut self, stream_id: u32, sample: &StblSample, bytes: Bytes) {
        let (payload, decrypt_ok) = self.maybe_decrypt(stream_id, bytes, None);
        self.packets.push_back(Sample {
            stream_id,
            pts: pts_from_dts(sample.dts, sample.cto),
            dts: sample.dts,
            duration: u64::from(sample.duration),
            is_keyframe: sample.key,
            is_discard: sample.discard || !decrypt_ok,
            payload,
        });
    }

    fn emit_stbl(&mut self) {
        if self.stbl_pending.is_empty() {
            // Keep retained `mdat` parts until `moov`/`stbl` arrives (mdat-before-moov).
            return;
        }
        while let Some(front) = self.stbl_pending.front() {
            let Some(bytes) = self.read_file_range(front.sample.offset, front.sample.size as usize)
            else {
                break;
            };
            let Some(PendingStbl { stream_id, sample }) = self.stbl_pending.pop_front() else {
                break;
            };
            self.push_stbl_sample(stream_id, &sample, bytes);
        }
        if self.stbl_pending.is_empty() {
            self.mdat_parts.clear();
        }
    }

    /// Seekable mode: read `stbl` samples in offset order straight from the input.
    fn read_stbl_seekable(&mut self) {
        while let Some(front) = self.stbl_pending.front() {
            let off = front.sample.offset;
            let size = front.sample.size as usize;
            let have_end = self.file_base.saturating_add(self.buffer.len() as u64);
            if off < self.file_base || off > have_end {
                self.request_seek(off);
                return;
            }
            if off.saturating_add(size as u64) > have_end {
                return;
            }
            let skip = (off - self.file_base) as usize;
            if skip > 0 {
                // Drop the gap (O(1) view advance, no copy).
                self.buffer.advance(skip);
            }
            let bytes = self.buffer.split_to(size).freeze();
            self.file_base = off.saturating_add(size as u64);
            let Some(PendingStbl { stream_id, sample }) = self.stbl_pending.pop_front() else {
                break;
            };
            self.push_stbl_sample(stream_id, &sample, bytes);
        }
        let resume = self.resume_offset;
        self.request_seek(resume);
    }

    fn read_file_range(&self, file_offset: u64, size: usize) -> Option<Bytes> {
        for part in &self.mdat_parts {
            let part_end = part.file_offset.saturating_add(part.data.len() as u64);
            if file_offset >= part.file_offset
                && file_offset.saturating_add(size as u64) <= part_end
            {
                let local = (file_offset - part.file_offset) as usize;
                return Some(part.data.slice(local..local + size));
            }
        }
        None
    }
}

fn pts_from_dts(dts: i64, cto: i32) -> i64 {
    dts.saturating_add(i64::from(cto))
}

fn find_senc_in_moof(moof: &[u8], per_sample_iv_size: u8) -> Vec<SencSample> {
    let mut pos = 0;
    while pos + 8 <= moof.len() {
        let Some(hdr) = parse_header(&moof[pos..]) else {
            break;
        };
        if pos + hdr.size > moof.len() {
            break;
        }
        let body = &moof[pos + hdr.header_len..pos + hdr.size];
        if &hdr.typ.0 == b"traf" {
            let mut tpos = 0;
            while tpos + 8 <= body.len() {
                let Some(th) = parse_header(&body[tpos..]) else {
                    break;
                };
                if tpos + th.size > body.len() {
                    break;
                }
                if &th.typ.0 == b"senc" {
                    return parse_senc(
                        &body[tpos + th.header_len..tpos + th.size],
                        per_sample_iv_size,
                    );
                }
                tpos += th.size;
            }
        }
        pos += hdr.size;
    }
    Vec::new()
}

fn decrypt_sample(
    payload: &mut [u8],
    key: [u8; 16],
    tenc: &TrackEncryption,
    senc: Option<&SencSample>,
) -> Result<(), iso_cenc::Error> {
    let iv = if let Some(s) = senc {
        if s.iv.is_empty() {
            iv_from_constant(&tenc.constant_iv)?
        } else {
            match s.iv.len() {
                8 => {
                    let mut a = [0u8; 8];
                    a.copy_from_slice(&s.iv);
                    iv_from_8(&a)
                }
                16 => {
                    let mut a = [0u8; 16];
                    a.copy_from_slice(&s.iv);
                    a
                }
                _ => return Ok(()),
            }
        }
    } else if !tenc.constant_iv.is_empty() {
        iv_from_constant(&tenc.constant_iv)?
    } else {
        return Ok(());
    };
    let subs: &[iso_cenc::Subsample] = senc.map_or(&[], |s| s.subsamples.as_slice());
    decrypt_cenc(&key, &iv, Pattern::NONE, payload, subs)
}
