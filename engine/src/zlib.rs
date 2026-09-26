//! Thin zlib wrappers over `miniz_oxide` (pure Rust, wasm32-safe).
//!
//! Git objects (loose and inside packfiles) are zlib streams, i.e. deflate
//! data with a 2-byte header and an adler32 trailer. miniz_oxide's
//! `decompress_to_vec_zlib` / `compress_to_vec_zlib` handle the framing.

use crate::error::{GitError, Result};

/// Inflates a zlib stream to its original bytes.
pub fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    miniz_oxide::inflate::decompress_to_vec_zlib(data)
        .map_err(|e| GitError::Corrupt(format!("zlib inflate failed: {e:?}")))
}

/// Deflates bytes as a zlib stream at the default compression level
/// (level 6 — a good size/speed point for the occasional object write).
pub fn deflate(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
}
