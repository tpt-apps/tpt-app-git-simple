//! Packfile access: `.idx` v2 lookup + `.pack` object extraction with
//! ofs/ref delta resolution.
//!
//! A repo with any real history keeps most objects in
//! `.git/objects/pack/*.pack` (+ their `.idx` companions). This module
//! parses enough of both to fetch an object by oid:
//!
//! - `.idx` v2: magic `\377tOc`, fanout table, sorted oids, CRCs, 4-byte
//!   offsets (MSB set → index into the 8-byte large-offset table).
//! - `.pack`: per-object varint header (type+size), zlib payload, and the
//!   two delta encodings (`ofs_delta` with its backward relative offset,
//!   `ref_delta` with the base oid) applied against recursively-resolved
//!   bases via git's copy/insert instruction format.

use std::collections::HashMap;

use crate::error::{GitError, Result};
use crate::oid::Oid;
use crate::objects::ObjectType;

pub struct Pack {
    idx: PackIndex,
    pack_data: Vec<u8>,
    /// Delta depth guard: chains longer than this are corrupt in practice.
    max_delta_depth: usize,
}

struct PackIndex {
    /// Oids in index order (sorted), parallel to `offsets`.
    oids: Vec<Oid>,
    /// Byte offset of each object in the pack file.
    offsets: Vec<u64>,
    /// oid → position in `oids`.
    lookup: HashMap<Oid, u32>,
}

impl Pack {
    /// Opens a pack from its `.idx` and `.pack` file contents.
    pub fn open(idx_bytes: Vec<u8>, pack_bytes: Vec<u8>) -> Result<Pack> {
        let idx = parse_idx_v2(&idx_bytes)?;
        Ok(Pack { idx, pack_data: pack_bytes, max_delta_depth: 64 })
    }

    pub fn len(&self) -> usize {
        self.idx.oids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.idx.oids.is_empty()
    }

    pub fn contains(&self, oid: &Oid) -> bool {
        self.idx.lookup.contains_key(oid)
    }

    /// Reads and fully resolves one object (applying delta chains).
    pub fn read_object(&self, oid: &Oid) -> Result<(ObjectType, Vec<u8>)> {
        let pos = self
            .idx
            .lookup
            .get(oid)
            .ok_or_else(|| GitError::NotFound(format!("object {} not in pack", oid.to_hex())))?;
        let offset = self.idx.offsets[*pos as usize];
        self.read_at(offset, 0)
    }

    fn read_at(&self, offset: u64, depth: usize) -> Result<(ObjectType, Vec<u8>)> {
        if depth > self.max_delta_depth {
            return Err(GitError::Corrupt("delta chain too deep".into()));
        }
        let mut cur = offset as usize;
        let (obj_type, size) = parse_obj_header(&self.pack_data, &mut cur)?;
        match obj_type {
            ObjKind::Commit => Ok((ObjectType::Commit, read_zlib_at(&self.pack_data, cur, size)?)),
            ObjKind::Tree => Ok((ObjectType::Tree, read_zlib_at(&self.pack_data, cur, size)?)),
            ObjKind::Blob => Ok((ObjectType::Blob, read_zlib_at(&self.pack_data, cur, size)?)),
            ObjKind::Tag => Ok((ObjectType::Tag, read_zlib_at(&self.pack_data, cur, size)?)),
            ObjKind::OfsDelta => {
                let (base_offset, after) = parse_ofs_delta_offset(&self.pack_data, cur)?;
                let delta = read_zlib_at(&self.pack_data, after, size)?;
                let (base_type, base) = self.read_at(offset - base_offset, depth + 1)?;
                Ok((base_type, apply_delta(&base, &delta)?))
            }
            ObjKind::RefDelta => {
                if self.pack_data.len() < cur + 20 {
                    return Err(GitError::Corrupt("ref-delta truncated base oid".into()));
                }
                let mut raw = [0u8; 20];
                raw.copy_from_slice(&self.pack_data[cur..cur + 20]);
                let after = cur + 20;
                let delta = read_zlib_at(&self.pack_data, after, size)?;
                let (base_type, base) = self.read_object(&Oid(raw))?;
                Ok((base_type, apply_delta(&base, &delta)?))
            }
        }
    }
}

enum ObjKind {
    Commit,
    Tree,
    Blob,
    Tag,
    OfsDelta,
    RefDelta,
}

/// Parses the per-object varint header: `(type, inflated size)`. Advances
/// `cur` past it.
fn parse_obj_header(data: &[u8], cur: &mut usize) -> Result<(ObjKind, usize)> {
    let mut byte = *data
        .get(*cur)
        .ok_or_else(|| GitError::Corrupt("pack object header truncated".into()))?;
    *cur += 1;
    let type_bits = (byte >> 4) & 0x7;
    let mut size = (byte & 0x0f) as usize;
    let mut shift = 4;
    while byte & 0x80 != 0 {
        byte = *data
            .get(*cur)
            .ok_or_else(|| GitError::Corrupt("pack object size truncated".into()))?;
        *cur += 1;
        size |= ((byte & 0x7f) as usize) << shift;
        shift += 7;
    }
    let kind = match type_bits {
        1 => ObjKind::Commit,
        2 => ObjKind::Tree,
        3 => ObjKind::Blob,
        4 => ObjKind::Tag,
        6 => ObjKind::OfsDelta,
        7 => ObjKind::RefDelta,
        other => return Err(GitError::Corrupt(format!("bad pack object type {other}"))),
    };
    Ok((kind, size))
}

/// Parses the ofs-delta negative offset (git's "offset encoding": unlike the
/// size varint, each continuation byte's value is added to the running total
/// *before* shifting by 7 more). Returns `(relative_offset, bytes_consumed)`.
fn parse_ofs_delta_offset(data: &[u8], cur: usize) -> Result<(u64, usize)> {
    let mut byte = *data
        .get(cur)
        .ok_or_else(|| GitError::Corrupt("ofs-delta offset truncated".into()))?;
    let mut offset = (byte & 0x7f) as u64;
    let mut consumed = 1;
    while byte & 0x80 != 0 {
        byte = *data
            .get(cur + consumed)
            .ok_or_else(|| GitError::Corrupt("ofs-delta offset truncated".into()))?;
        consumed += 1;
        offset = ((offset + 1) << 7) | (byte & 0x7f) as u64;
    }
    Ok((offset, cur + consumed))
}

fn read_zlib_at(data: &[u8], start: usize, declared_size: usize) -> Result<Vec<u8>> {
    let tail = data
        .get(start..)
        .ok_or_else(|| GitError::Corrupt("pack object payload out of range".into()))?;
    let out = crate::zlib::inflate(tail)?;
    if out.len() != declared_size {
        return Err(GitError::Corrupt(format!(
            "pack object size mismatch: declared {declared_size}, got {}",
            out.len()
        )));
    }
    Ok(out)
}

/// Applies a git delta instruction stream: two size varints (little-endian
/// 7-bit groups), then copy/insert ops.
fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>> {
    let (_source_size, pos) = read_varint(delta, 0)?;
    let (target_size, mut pos) = read_varint(delta, pos)?;

    let mut out = Vec::with_capacity(target_size.min(1 << 20));
    while pos < delta.len() {
        let op = delta[pos];
        pos += 1;
        if op & 0x80 != 0 {
            // Copy from base: bits 0-3 pick offset bytes, 4-6 pick size bytes.
            let mut offset = 0u64;
            let mut size = 0u64;
            for bit in 0..4 {
                if op & (1 << bit) != 0 {
                    offset |= (*delta.get(pos).ok_or_else(|| {
                        GitError::Corrupt("delta copy offset truncated".into())
                    })? as u64)
                        << (8 * bit);
                    pos += 1;
                }
            }
            for bit in 0..3 {
                if op & (1 << (4 + bit)) != 0 {
                    size |= (*delta.get(pos).ok_or_else(|| {
                        GitError::Corrupt("delta copy size truncated".into())
                    })? as u64)
                        << (8 * bit);
                    pos += 1;
                }
            }
            if size == 0 {
                size = 0x10000;
            }
            let start = offset as usize;
            let end = start + size as usize;
            let chunk = base
                .get(start..end)
                .ok_or_else(|| GitError::Corrupt("delta copy out of base range".into()))?;
            out.extend_from_slice(chunk);
        } else {
            // Insert: the low 7 bits are a literal byte count.
            let count = (op & 0x7f) as usize;
            let end = pos + count;
            let chunk = delta
                .get(pos..end)
                .ok_or_else(|| GitError::Corrupt("delta insert truncated".into()))?;
            out.extend_from_slice(chunk);
            pos = end;
        }
    }
    if out.len() != target_size {
        return Err(GitError::Corrupt(format!(
            "delta target size mismatch: declared {target_size}, got {}",
            out.len()
        )));
    }
    Ok(out)
}

/// Little-endian base-128 varint; returns `(value, next_pos)`.
fn read_varint(data: &[u8], mut pos: usize) -> Result<(usize, usize)> {
    let mut value = 0usize;
    let mut shift = 0;
    loop {
        let byte = *data
            .get(pos)
            .ok_or_else(|| GitError::Corrupt("delta varint truncated".into()))?;
        pos += 1;
        value |= ((byte & 0x7f) as usize) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, pos));
        }
        shift += 7;
    }
}

fn be32(data: &[u8], at: usize) -> Result<u32> {
    let chunk = data
        .get(at..at + 4)
        .ok_or_else(|| GitError::Corrupt("idx truncated u32".into()))?;
    Ok(u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
}

fn be64(data: &[u8], at: usize) -> Result<u64> {
    let chunk = data
        .get(at..at + 8)
        .ok_or_else(|| GitError::Corrupt("idx truncated u64".into()))?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(chunk);
    Ok(u64::from_be_bytes(raw))
}

fn parse_idx_v2(data: &[u8]) -> Result<PackIndex> {
    const MAGIC: [u8; 4] = [0xff, b't', b'O', b'c'];
    if data.len() < 8 || data[..4] != MAGIC {
        return Err(GitError::Corrupt("pack idx: not a v2 index (no magic)".into()));
    }
    if be32(data, 4)? != 2 {
        return Err(GitError::Corrupt("pack idx: unsupported version".into()));
    }
    // Fanout: 256 entries; the last one is the total object count.
    let count = be32(data, 8 + 255 * 4)? as usize;
    let oids_start = 8 + 256 * 4;
    let oids_end = oids_start + count * 20;
    let crc_end = oids_end + count * 4;
    let offsets_end = crc_end + count * 4;
    if data.len() < offsets_end {
        return Err(GitError::Corrupt("pack idx truncated".into()));
    }

    let mut oids = Vec::with_capacity(count);
    for i in 0..count {
        let at = oids_start + i * 20;
        let mut raw = [0u8; 20];
        raw.copy_from_slice(&data[at..at + 20]);
        oids.push(Oid(raw));
    }

    let mut offsets = Vec::with_capacity(count);
    let mut large: Vec<u64> = Vec::new();
    for i in 0..count {
        let raw = be32(data, crc_end + i * 4)?;
        let offset = if raw & 0x8000_0000 != 0 {
            let idx = (raw & 0x7fff_ffff) as usize;
            if idx > large.len() {
                // Large offsets are referenced in order; a forward jump is
                // corrupt (backward references are legal).
                return Err(GitError::Corrupt("pack idx: sparse large-offset table".into()));
            }
            if idx == large.len() {
                large.push(be64(data, offsets_end + idx * 8)?);
            }
            large[idx]
        } else {
            raw as u64
        };
        offsets.push(offset);
    }

    let lookup = oids
        .iter()
        .enumerate()
        .map(|(i, oid)| (*oid, i as u32))
        .collect();
    Ok(PackIndex { oids, offsets, lookup })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trip() {
        for value in [0usize, 1, 127, 128, 300, 1 << 20] {
            let mut encoded = Vec::new();
            let mut v = value;
            loop {
                let mut byte = (v & 0x7f) as u8;
                v >>= 7;
                if v != 0 {
                    byte |= 0x80;
                }
                encoded.push(byte);
                if v == 0 {
                    break;
                }
            }
            let (decoded, next) = read_varint(&encoded, 0).unwrap();
            assert_eq!(decoded, value);
            assert_eq!(next, encoded.len());
        }
    }

    #[test]
    fn rejects_non_idx() {
        assert!(parse_idx_v2(b"not an index at all....").is_err());
    }
}

