//! The zstd codec behind share strings, packed blueprints and replays (L2093, L2081, L943).
use crate::error::{ContentError, ContentResult};
use std::io::Read;

/// Compression level the spec names (L2093).
pub(super) const LEVEL: i32 = 19;
/// Magic number of a standard zstd frame, little-endian `0xFD2FB528`. Legacy and skippable frames are refused.
const MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
/// Smallest and largest window logs a frame may ask the decoder for. Any frame may use an 8 MiB window (level
/// 19's largest), so a frame from another encoder setting still decodes; beyond that the window must fit the
/// output bound.
const MIN_WINDOW_LOG: u32 = 23;
const MAX_WINDOW_LOG: u32 = 27;

fn bad(message: impl Into<String>) -> ContentError { ContentError::Parse { file: "zstd frame".into(), message: message.into() } }

pub(super) fn compress(bytes: &[u8]) -> Vec<u8> {
    let framed = || -> std::io::Result<Vec<u8>> {
        let mut c = zstd::bulk::Compressor::new(LEVEL)?;
        // A content checksum lets the decoder catch a damaged frame even where the hash check would not run.
        c.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true))?;
        c.compress(bytes)
    };
    // Compression only fails when the allocator does; fall back to the plain one-shot call, which writes the
    // same format without a checksum.
    framed().or_else(|_| zstd::bulk::compress(bytes, LEVEL)).unwrap_or_else(|e| panic!("zstd compression failed: {e}"))
}

/// Window log needed to hold `max_out` bytes, so a frame cannot make the decoder allocate much more than its
/// output may grow to.
fn window_log_for(max_out: usize) -> u32 {
    let need = usize::BITS - max_out.saturating_sub(1).leading_zeros();
    need.clamp(MIN_WINDOW_LOG, MAX_WINDOW_LOG)
}

pub(super) fn decompress_bounded(frame: &[u8], max_out: usize) -> ContentResult<Vec<u8>> {
    if frame.len() < MAGIC.len() || frame[..4] != MAGIC {
        return Err(bad("not a zstd frame"));
    }
    match zstd::zstd_safe::find_frame_compressed_size(frame) {
        Ok(n) if n == frame.len() => {}
        Ok(_) => return Err(bad("bytes after the frame")),
        Err(_) => return Err(bad("truncated or corrupt frame")),
    }
    let declared = match zstd::zstd_safe::get_frame_content_size(frame) {
        Ok(size) => size,
        Err(_) => return Err(bad("corrupt frame header")),
    };
    if declared.is_some_and(|n| n > max_out as u64) {
        return Err(ContentError::Limit("zstd frame expands past the limit"));
    }
    let mut decoder = zstd::stream::read::Decoder::with_buffer(frame).map_err(|e| bad(e.to_string()))?.single_frame();
    decoder.window_log_max(window_log_for(max_out)).map_err(|e| bad(e.to_string()))?;
    // Grow the buffer as output arrives instead of trusting the declared size.
    let start = declared.map_or(frame.len().saturating_mul(4), |n| n as usize).min(max_out).min(1 << 20);
    let mut out = Vec::with_capacity(start);
    let limit = (max_out as u64).saturating_add(1);
    decoder.take(limit).read_to_end(&mut out).map_err(|e| bad(e.to_string()))?;
    if out.len() > max_out {
        return Err(ContentError::Limit("zstd frame expands past the limit"));
    }
    if declared.is_some_and(|n| n != out.len() as u64) {
        return Err(bad("frame size does not match its header"));
    }
    Ok(out)
}
