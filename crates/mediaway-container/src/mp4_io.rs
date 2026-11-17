//! `Read + Seek` adapters over the sans-io MP4 core: seekable demux and faststart copy.
//!
//! The cores ([`iso_bmff::Demuxer::new_seekable`], [`iso_bmff::isobmff::plan_faststart`]) never
//! touch I/O; these adapters own the reader/writer (iso-bmff ADR-0010 / ADR-0011).

#![forbid(unsafe_code)]

#[cfg(feature = "demux")]
use crate::convert::{to_packet, to_stream_info};
use iso_bmff::isobmff::{TopLevelBox, parse_header, plan_faststart, tag};
#[cfg(feature = "demux")]
use mediaway_common::{Packet, StreamInfo};
use std::io::{self, Read, Seek, SeekFrom, Write};
use thiserror::Error;

/// Error from an MP4 I/O adapter.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum IoError {
    /// The reader or writer failed.
    #[error("MP4 I/O failed: {0}")]
    Io(#[from] io::Error),
    /// The container data is malformed.
    #[error("MP4 container error: {0}")]
    Container(#[from] iso_bmff::Error),
}

/// Read size per `push_bytes` call.
const READ_CHUNK: usize = 64 * 1024;

/// Demux an MP4 from a `Read + Seek` source, jumping over `mdat` to find a trailing `moov` and
/// then reading samples by offset — nothing is buffered beyond one read chunk plus the sample
/// being assembled.
///
/// **Cost:** each read is copied once into the core's buffer (`Read` fills a caller buffer);
/// emitted payloads share that buffer. Samples of an unfragmented file come out in file order
/// (interleaved across tracks), not grouped per track.
#[cfg(feature = "demux")]
#[derive(Debug)]
pub struct SeekDemuxer<R> {
    reader: R,
    inner: iso_bmff::Demuxer,
    streams: Vec<StreamInfo>,
    chunk: Vec<u8>,
    eof: bool,
}

#[cfg(feature = "demux")]
impl<R: Read + Seek> SeekDemuxer<R> {
    /// Start at the reader's beginning and read until the tracks are known (or EOF).
    ///
    /// # Errors
    ///
    /// [`IoError::Io`] when seeking or reading fails.
    pub fn open(mut reader: R) -> Result<Self, IoError> {
        reader.seek(SeekFrom::Start(0))?;
        let mut this = Self {
            reader,
            inner: iso_bmff::Demuxer::new_seekable(),
            streams: Vec::new(),
            chunk: vec![0; READ_CHUNK],
            eof: false,
        };
        while this.inner.streams().is_empty() && this.advance()? {}
        this.sync_streams();
        Ok(this)
    }

    /// Supply a `ClearKey` content key (see [`iso_bmff::Demuxer::set_decryption_key`]).
    pub const fn set_decryption_key(&mut self, key: [u8; 16]) {
        self.inner.set_decryption_key(key);
    }

    /// Tracks.
    #[must_use]
    pub fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    /// Next packet, or `None` at end of input.
    ///
    /// # Errors
    ///
    /// [`IoError::Io`] when seeking or reading fails.
    pub fn next_packet(&mut self) -> Result<Option<Packet>, IoError> {
        loop {
            if let Some(s) = self.inner.poll_packet() {
                return Ok(Some(to_packet(s)));
            }
            if !self.advance()? {
                return Ok(None);
            }
            self.sync_streams();
        }
    }

    /// Give back the reader.
    pub fn into_inner(self) -> R {
        self.reader
    }

    /// Honor a pending seek, else push one read. `false` at EOF with nothing pending.
    fn advance(&mut self) -> Result<bool, IoError> {
        if let Some(off) = self.inner.poll_seek() {
            self.reader.seek(SeekFrom::Start(off))?;
            self.eof = false;
            return Ok(true);
        }
        if self.eof {
            return Ok(false);
        }
        let n = self.reader.read(&mut self.chunk)?;
        if n == 0 {
            self.eof = true;
            return Ok(false);
        }
        self.inner.push_bytes(&self.chunk[..n]);
        Ok(true)
    }

    fn sync_streams(&mut self) {
        let inner = self.inner.streams();
        if inner.len() != self.streams.len() {
            self.streams = inner.iter().map(to_stream_info).collect();
        }
    }
}

/// Faststart copy: `reader` to `writer` with the `moov` moved in front of the first `mdat`.
///
/// Chunk offsets are rewritten. Returns `false` (and copies the file unchanged) when it
/// already was faststart.
///
/// Only the `moov` is held in memory; `mdat` bytes stream through [`io::copy`].
///
/// # Errors
///
/// [`IoError::Io`] on read/write failure; [`IoError::Container`] when the top-level box walk
/// or the `moov` is malformed, or there is no `moov`.
pub fn faststart<R: Read + Seek, W: Write>(mut reader: R, mut writer: W) -> Result<bool, IoError> {
    let len = reader.seek(SeekFrom::End(0))?;
    let mut boxes = Vec::new();
    let mut pos = 0u64;
    while pos < len {
        reader.seek(SeekFrom::Start(pos))?;
        let mut hdr = [0u8; 16];
        let take = usize::try_from((len - pos).min(16)).unwrap_or(16);
        reader.read_exact(&mut hdr[..take])?;
        let h = parse_header(&hdr[..take]).ok_or(iso_bmff::Error::InvalidData)?;
        let size = h.size as u64;
        if size < h.header_len as u64 || pos.saturating_add(size) > len {
            return Err(iso_bmff::Error::InvalidData.into());
        }
        boxes.push(TopLevelBox {
            typ: h.typ,
            offset: pos,
            size,
        });
        pos += size;
    }
    let moov = *boxes
        .iter()
        .find(|b| b.typ == tag::MOOV)
        .ok_or(iso_bmff::Error::InvalidData)?;
    let mut moov_bytes =
        vec![0u8; usize::try_from(moov.size).map_err(|_| iso_bmff::Error::InvalidData)?];
    reader.seek(SeekFrom::Start(moov.offset))?;
    reader.read_exact(&mut moov_bytes)?;
    let Some(plan) = plan_faststart(&boxes, &moov_bytes)? else {
        copy_range(&mut reader, &mut writer, 0, len)?;
        return Ok(false);
    };
    copy_range(&mut reader, &mut writer, 0, plan.insert_at)?;
    writer.write_all(&plan.moov)?;
    copy_range(
        &mut reader,
        &mut writer,
        plan.insert_at,
        plan.old_moov.offset,
    )?;
    let after = plan.old_moov.offset + plan.old_moov.size;
    copy_range(&mut reader, &mut writer, after, len)?;
    Ok(true)
}

fn copy_range<R: Read + Seek, W: Write>(
    reader: &mut R,
    writer: &mut W,
    start: u64,
    end: u64,
) -> Result<(), IoError> {
    if end <= start {
        return Ok(());
    }
    reader.seek(SeekFrom::Start(start))?;
    let copied = io::copy(&mut reader.by_ref().take(end - start), writer)?;
    if copied != end - start {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "mp4_io_tests.rs"]
mod tests;
