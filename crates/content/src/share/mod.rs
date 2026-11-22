//! Unit `content-share`. Share strings and the compressed network form of blueprints.
//! DESIGN.md L2093 (share strings), L2081 (limits), L943-945 (network encoding).
//!
//! Format: the prefix `BPN1:` then base64url without padding (RFC 4648 section 5) of a zstd frame (level 19)
//! of the canonical RON. The digit is the share-string version, separate from the blueprint `format`.
//! Whitespace and line breaks inside a string are ignored.
//!
//! Codec: zstd level 19 through the `zstd` crate, as the spec says (decided for the build). [`compress`] and
//! [`decompress_bounded`] are the only two functions that touch it.
//!
//! The frame holds the item's file text: canonical RON with its `hash` field filled in, so a decoder can refuse
//! content that no longer matches the hash it carries (L2079) without re-serialising, whatever format version
//! wrote it. The network form ([`pack`]) carries the same bytes.
use crate::blueprint::Shareable;
use crate::error::{ContentError, ContentResult};
use crate::ids::ContentHash;

mod codec;
#[cfg(test)]
mod tests;

pub const SHARE_PREFIX: &str = "BPN1:";
/// Above this length the copy dialog also offers Save as file (L2093).
pub const LONG_SHARE_CHARS: usize = 4_000;

/// Compress canonical RON bytes into one frame (spec: zstd level 19). Unit: `content-share`.
pub fn compress(bytes: &[u8]) -> Vec<u8> { codec::compress(bytes) }
/// Decompress one frame, refusing any frame that would expand past `max_out` bytes (16 MiB, L2081) without
/// allocating that much first. Unit: `content-share`.
///
/// Accepts exactly one standard zstd frame and nothing after it; a frame that is truncated, corrupt or followed
/// by other bytes is a `Parse` error, and one that declares or produces more than `max_out` bytes is a `Limit`
/// error. Decoder memory is bounded too: the frame's window may not exceed 8 MiB or `max_out` rounded up to a power of
/// two, whichever is larger.
pub fn decompress_bounded(frame: &[u8], max_out: usize) -> ContentResult<Vec<u8>> { codec::decompress_bounded(frame, max_out) }

/// Encode a ship or kit as a share string. Unit: `content-share`.
///
/// The item is canonicalised first (the same normalisation a load applies), so decoding gives back an equal
/// item with the same hash.
pub fn encode_share_string(item: &Shareable) -> String {
    use base64::Engine;
    let frame = compress(file_text(item).as_bytes());
    let mut out = String::with_capacity(SHARE_PREFIX.len() + frame.len() * 4 / 3 + 4);
    out.push_str(SHARE_PREFIX);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode_string(&frame, &mut out);
    out
}
/// Decode a pasted share string: strip whitespace, check the prefix, decode, decompress within limits, load
/// with migrations, and refuse content that does not match its hash as damaged (L2079). Scripts arrive
/// disabled (L2123). Unit: `content-share`.
///
/// A string with a later version digit (`BPN2:`) is refused as `FormatTooNew`; one that is not a share string
/// at all is `NotAShareString`; one whose frame is corrupt or whose content does not match the hash it carries
/// is `DamagedShareString`.
pub fn decode_share_string(text: &str) -> ContentResult<Shareable> { decode_share_string_with(text, crate::catalogue::catalogue()) }

/// [`decode_share_string`] against a given catalogue (for the removed-part migration).
pub fn decode_share_string_with(text: &str, catalogue: &crate::catalogue::Catalogue) -> ContentResult<Shareable> {
    use base64::Engine;
    if text.len() > MAX_SHARE_CHARS {
        return Err(ContentError::Limit("share string too long"));
    }
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let body = match compact.strip_prefix(SHARE_PREFIX) {
        Some(body) => body,
        None => return Err(newer_version(&compact).unwrap_or(ContentError::NotAShareString)),
    };
    let body = body.trim_end_matches('=');
    let frame = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(body).map_err(|_| ContentError::NotAShareString)?;
    let text = frame_text(&frame, crate::blueprint::MAX_RON_BYTES)?;
    let mut item = load_verified(&text, "share string", catalogue)?;
    // Scripts in anything imported arrive disabled; the player enables them by hand (L2123).
    if let Shareable::Ship(bp) = &mut item {
        let mut changed = false;
        for s in &mut bp.graph.scripts {
            changed |= std::mem::replace(&mut s.enabled, false);
        }
        if changed {
            bp.header.hash = crate::blueprint::io::compute_hash(bp);
        }
    }
    Ok(item)
}
/// True when the text looks like a share string (prefix check after trimming); used by paste handling.
pub fn looks_like_share_string(text: &str) -> bool { text.trim_start().starts_with(SHARE_PREFIX) }

/// Network and cache form: the compressed canonical RON addressed by its hash; sent in 16 KiB chunks (L945).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackedBlueprint { pub hash: ContentHash, pub compressed: Vec<u8> }
pub const BLUEPRINT_CHUNK_BYTES: usize = 16 * 1024;

/// Pack for the network. Unit: `content-share`.
///
/// `hash` is the content hash of the canonicalised item (blueprint header hash or kit hash) and `compressed` is
/// one zstd frame of its file text, the same bytes a share string carries.
pub fn pack(item: &Shareable) -> PackedBlueprint {
    let item = canonical(item);
    PackedBlueprint { hash: item_hash(&item), compressed: compress(file_text_of_canonical(&item).as_bytes()) }
}
/// Unpack a received blueprint: size limits ([`crate::blueprint::MAX_COMPRESSED_BYTES`]), decompress, load,
/// verify the hash. The caller then runs server validation. Unit: `content-share`.
///
/// Bytes that do not match the hash they carry, a carried hash that is not `packed.hash`, or content whose
/// loaded form no longer hashes to `packed.hash` (a part this build lacks, non-canonical text) are refused as
/// `DamagedShareString`. Scripts keep their enabled state: the bytes are the owner's design addressed by hash.
pub fn unpack(packed: &PackedBlueprint) -> ContentResult<Shareable> { unpack_with(packed, crate::catalogue::catalogue()) }

/// [`unpack`] against a given catalogue (for the removed-part migration).
pub fn unpack_with(packed: &PackedBlueprint, catalogue: &crate::catalogue::Catalogue) -> ContentResult<Shareable> {
    if packed.compressed.len() > crate::blueprint::MAX_COMPRESSED_BYTES {
        return Err(ContentError::Limit("blueprint over 8 MiB compressed"));
    }
    let text = frame_text(&packed.compressed, crate::blueprint::MAX_RON_BYTES)?;
    match crate::kit::text::verify_text_hash(&text) {
        Some(h) if h == packed.hash => {}
        _ => return Err(ContentError::DamagedShareString),
    }
    let item = load_verified(&text, "packed blueprint", catalogue)?;
    // Loading may migrate or normalise; what comes out must still be the design the address names.
    if item_hash(&item) != packed.hash {
        return Err(ContentError::DamagedShareString);
    }
    Ok(item)
}

/// Longest share string accepted before any decoding: the base64 of an 8 MiB frame plus generous room for
/// line breaks a chat client inserts.
pub const MAX_SHARE_CHARS: usize = crate::blueprint::MAX_COMPRESSED_BYTES / 3 * 4 * 2;

/// `BPNn:` with n above 1: a share-string version from a later release.
fn newer_version(compact: &str) -> Option<ContentError> {
    let rest = compact.strip_prefix("BPN")?;
    let (digits, _) = rest.split_once(':')?;
    let n: u32 = digits.parse().ok()?;
    (n > 1).then(|| ContentError::FormatTooNew { found: n, supported: 1, needs_release: format!("later than {}", env!("CARGO_PKG_VERSION")) })
}

/// Decompress a frame into UTF-8 text; a corrupt frame or text reads as a damaged share string.
fn frame_text(frame: &[u8], max_out: usize) -> ContentResult<String> {
    let bytes = decompress_bounded(frame, max_out).map_err(|e| match e {
        e @ ContentError::Limit(_) => e,
        _ => ContentError::DamagedShareString,
    })?;
    String::from_utf8(bytes).map_err(|_| ContentError::DamagedShareString)
}

/// Check the carried hash, then load the ship or kit with limits and migrations.
fn load_verified(text: &str, file: &str, catalogue: &crate::catalogue::Catalogue) -> ContentResult<Shareable> {
    crate::kit::text::check_untrusted(text)?;
    if crate::kit::text::verify_text_hash(text).is_none() {
        return Err(ContentError::DamagedShareString);
    }
    match crate::kit::text::doc_kind(text) {
        Some(crate::kit::text::DocKind::Ship) => Ok(Shareable::Ship(Box::new(crate::blueprint::io::load_blueprint_with(text, file, catalogue)?.blueprint))),
        Some(crate::kit::text::DocKind::Kit) => Ok(Shareable::Kit(Box::new(crate::kit::parse_kit(text, file)?))),
        None => Err(ContentError::Parse { file: file.to_string(), message: "neither a ship nor a kit".into() }),
    }
}

/// The item after the normalisation every load applies.
fn canonical(item: &Shareable) -> Shareable {
    match item {
        Shareable::Ship(bp) => {
            let mut bp = bp.clone();
            crate::blueprint::io::canonicalize_blueprint(&mut bp);
            Shareable::Ship(bp)
        }
        Shareable::Kit(kit) => {
            let mut kit = kit.clone();
            crate::kit::canonicalize_kit(&mut kit);
            Shareable::Kit(kit)
        }
    }
}

fn item_hash(item: &Shareable) -> ContentHash {
    match item {
        Shareable::Ship(bp) => crate::blueprint::io::compute_hash(bp),
        Shareable::Kit(kit) => crate::kit::kit_hash(kit),
    }
}

fn file_text_of_canonical(item: &Shareable) -> String {
    match item {
        Shareable::Ship(bp) => crate::blueprint::io::to_file_text(bp),
        Shareable::Kit(kit) => crate::kit::kit_to_file_text(kit),
    }
}

/// File text of the canonicalised item.
fn file_text(item: &Shareable) -> String { file_text_of_canonical(&canonical(item)) }
