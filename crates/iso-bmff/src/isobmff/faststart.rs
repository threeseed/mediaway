//! Faststart: move a trailing `moov` in front of the first `mdat`, fixing `stco`/`co64`.
//!
//! A progressive MP4 written by a streaming muxer ends with its `moov`, so a player must read
//! (or seek to) the end of the file before the first frame. Relocating the `moov` shifts every
//! byte after the insertion point, so each chunk offset is rewritten. A 32-bit `stco` whose
//! shifted offsets pass 4 GiB is widened to `co64` (which grows the `moov`, so the shift is
//! recomputed until it is stable). See ADR-0011.
//!
//! Sans-io: [`relocate_moov`] works on the `moov` box alone, given where things sit in the file;
//! [`FaststartPlan`] says which byte ranges to write in which order. The caller moves the bytes
//! (a `Read + Seek` adapter copies the `mdat` ranges without holding them in memory).
//! [`faststart`] is the whole-buffer convenience.

#![forbid(unsafe_code)]

use super::{FourCc, parse_header, tag, write_box};
use crate::error::Error;

/// A top-level box: where it starts in the file, its total size and its type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopLevelBox {
    /// Box type.
    pub typ: FourCc,
    /// Absolute file offset of the box header.
    pub offset: u64,
    /// Total box size (header included).
    pub size: u64,
}

/// How to write a faststart copy of a file whose `moov` sits after its first `mdat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaststartPlan {
    /// The relocated `moov` box (offsets already rewritten).
    pub moov: Vec<u8>,
    /// Source bytes `[0, insert_at)` are copied first, then [`Self::moov`], then
    /// `[insert_at, old_moov.offset)` and `[old_moov.offset + old_moov.size, file_len)`.
    pub insert_at: u64,
    /// The original `moov` (left out of the output).
    pub old_moov: TopLevelBox,
}

/// Build a [`FaststartPlan`] from the file's top-level boxes and the original `moov` box bytes.
///
/// Returns `Ok(None)` when the file is already faststart (its `moov` precedes every `mdat`, or
/// there is no `mdat`).
///
/// # Errors
///
/// [`Error::InvalidData`] when there is no `moov` in `boxes`, `moov_box` does not match it, or the
/// `moov` cannot be rewritten (malformed child boxes, offsets past `u64`).
pub fn plan_faststart(
    boxes: &[TopLevelBox],
    moov_box: &[u8],
) -> Result<Option<FaststartPlan>, Error> {
    let old_moov = *boxes
        .iter()
        .find(|b| b.typ == tag::MOOV)
        .ok_or(Error::InvalidData)?;
    if old_moov.size != moov_box.len() as u64 {
        return Err(Error::InvalidData);
    }
    let Some(first_mdat) = boxes.iter().find(|b| b.typ == tag::MDAT) else {
        return Ok(None);
    };
    if old_moov.offset < first_mdat.offset {
        return Ok(None);
    }
    let moov = relocate_moov(moov_box, first_mdat.offset, old_moov)?;
    Ok(Some(FaststartPlan {
        moov,
        insert_at: first_mdat.offset,
        old_moov,
    }))
}

/// Rewrite `moov_box` for insertion at `insert_at` (before the first `mdat`), with the original
/// `moov` at `old_moov` removed from behind it.
///
/// A chunk offset `o` before `old_moov` moves by the new `moov`'s length; one after it by the
/// new length minus the old. Containers on the path `moov/trak/mdia/minf/stbl` are rebuilt; other
/// boxes are copied verbatim.
///
/// # Errors
///
/// [`Error::InvalidData`] on malformed boxes or an offset that overflows `u64`.
pub fn relocate_moov(
    moov_box: &[u8],
    insert_at: u64,
    old_moov: TopLevelBox,
) -> Result<Vec<u8>, Error> {
    let hdr = parse_header(moov_box).ok_or(Error::InvalidData)?;
    if hdr.typ != tag::MOOV || hdr.size != moov_box.len() || hdr.header_len > hdr.size {
        return Err(Error::InvalidData);
    }
    // The new length decides the shift and the shift may widen `stco` (longer box): iterate.
    // Lengths only grow, and only once per table, so this settles in a few passes.
    let mut guess = moov_box.len() as u64;
    for _ in 0..4 {
        let shift = |o: u64| -> Option<u64> {
            if o < insert_at {
                Some(o)
            } else if o < old_moov.offset {
                o.checked_add(guess)
            } else {
                o.checked_add(guess)?.checked_sub(old_moov.size)
            }
        };
        let mut out = Vec::with_capacity(moov_box.len());
        rewrite_container(&mut out, tag::MOOV, &moov_box[hdr.header_len..], &shift)?;
        if out.len() as u64 == guess {
            return Ok(out);
        }
        guess = out.len() as u64;
    }
    Err(Error::InvalidData)
}

/// Whole-buffer faststart: the file with its `moov` moved in front of the first `mdat`.
///
/// Returns `Ok(None)` when `file` is already faststart. **Cost:** builds a full copy of the file
/// in memory; for large files use [`plan_faststart`] with a streaming copy instead.
///
/// # Errors
///
/// [`Error::InvalidData`] on a malformed top-level box walk or `moov`.
pub fn faststart(file: &[u8]) -> Result<Option<Vec<u8>>, Error> {
    let mut boxes = Vec::new();
    let mut pos = 0usize;
    while pos < file.len() {
        let h = parse_header(&file[pos..]).ok_or(Error::InvalidData)?;
        if h.size < h.header_len || pos.saturating_add(h.size) > file.len() {
            return Err(Error::InvalidData);
        }
        boxes.push(TopLevelBox {
            typ: h.typ,
            offset: pos as u64,
            size: h.size as u64,
        });
        pos += h.size;
    }
    let moov = boxes
        .iter()
        .find(|b| b.typ == tag::MOOV)
        .ok_or(Error::InvalidData)?;
    let moov_bytes = &file[moov.offset as usize..(moov.offset + moov.size) as usize];
    let Some(plan) = plan_faststart(&boxes, moov_bytes)? else {
        return Ok(None);
    };
    let ins = plan.insert_at as usize;
    let mo = plan.old_moov.offset as usize;
    let me = mo + plan.old_moov.size as usize;
    let mut out = Vec::with_capacity(file.len() + plan.moov.len() - plan.old_moov.size as usize);
    out.extend_from_slice(&file[..ins]);
    out.extend_from_slice(&plan.moov);
    out.extend_from_slice(&file[ins..mo]);
    out.extend_from_slice(&file[me..]);
    Ok(Some(out))
}

fn rewrite_container<F: Fn(u64) -> Option<u64>>(
    out: &mut Vec<u8>,
    typ: FourCc,
    body: &[u8],
    shift: &F,
) -> Result<(), Error> {
    let mut res = Ok(());
    write_box(out, typ, |buf| {
        res = rewrite_children(buf, body, shift);
    });
    res
}

fn rewrite_children<F: Fn(u64) -> Option<u64>>(
    out: &mut Vec<u8>,
    body: &[u8],
    shift: &F,
) -> Result<(), Error> {
    let mut pos = 0usize;
    while pos < body.len() {
        let h = parse_header(&body[pos..]).ok_or(Error::InvalidData)?;
        if h.size < h.header_len || pos.saturating_add(h.size) > body.len() {
            return Err(Error::InvalidData);
        }
        let child = &body[pos..pos + h.size];
        let payload = &child[h.header_len..];
        match &h.typ.0 {
            b"trak" | b"mdia" | b"minf" | b"stbl" => rewrite_container(out, h.typ, payload, shift)?,
            b"stco" | b"co64" => write_offsets(out, &h.typ.0 == b"co64", payload, shift)?,
            _ => out.extend_from_slice(child),
        }
        pos += h.size;
    }
    Ok(())
}

fn write_offsets<F: Fn(u64) -> Option<u64>>(
    out: &mut Vec<u8>,
    is_co64: bool,
    payload: &[u8],
    shift: &F,
) -> Result<(), Error> {
    let count_bytes = payload.get(4..8).ok_or(Error::InvalidData)?;
    let count = u32::from_be_bytes([
        count_bytes[0],
        count_bytes[1],
        count_bytes[2],
        count_bytes[3],
    ]);
    let width = if is_co64 { 8 } else { 4 };
    let table = payload
        .get(8..8 + count as usize * width)
        .ok_or(Error::InvalidData)?;
    let mut shifted = Vec::with_capacity(count as usize);
    for e in table.chunks_exact(width) {
        let o = if is_co64 {
            u64::from_be_bytes([e[0], e[1], e[2], e[3], e[4], e[5], e[6], e[7]])
        } else {
            u64::from(u32::from_be_bytes([e[0], e[1], e[2], e[3]]))
        };
        shifted.push(shift(o).ok_or(Error::InvalidData)?);
    }
    let wide = is_co64 || shifted.iter().any(|&o| o > u64::from(u32::MAX));
    let typ = if wide {
        FourCc(*b"co64")
    } else {
        FourCc(*b"stco")
    };
    write_box(out, typ, |buf| {
        buf.extend_from_slice(&[0, 0, 0, 0]);
        buf.extend_from_slice(&count.to_be_bytes());
        for o in &shifted {
            if wide {
                buf.extend_from_slice(&o.to_be_bytes());
            } else {
                buf.extend_from_slice(&(*o as u32).to_be_bytes());
            }
        }
    });
    Ok(())
}
