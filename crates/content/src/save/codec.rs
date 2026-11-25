//! Unit `content-save`: save and suspend encoding, the save hash chain and co-op copy reconciliation
//! (DESIGN.md L112, L826-828, L845).
//!
//! On disk a save is one zstd frame of the game serializer's canonical RON (the blueprint serializer: fields in
//! declaration order, LF, nesting past two levels on one line, so each hull record, group and region is one
//! line and two saves diff cleanly once decompressed). Saves are written often and are larger than blueprints,
//! so the frame uses a fast level ([`LEVEL`]) with a content checksum; any standard frame decodes.
//!
//! The suspend slot carries the sortie keyframe and config as opaque bytes. RON would write them as a list of
//! numbers (four to five bytes of text per byte), so the file form stores them as base64 strings.
use super::{Reconcile, SaveGame, SaveHeader, SortieCheckpoint, SuspendSave, TheatreState, SAVE_FORMAT};
use crate::blueprint::MAX_RON_DEPTH;
use crate::error::{ContentError, ContentResult};
use crate::ids::ContentHash;
use crate::kit::text;
use serde::{Deserialize, Serialize};

/// Largest decompressed save or suspend file. A suspend holds a whole-battle keyframe, so this is well above
/// the 16 MiB blueprint limit; the decoder still never allocates past it.
pub const MAX_SAVE_BYTES: usize = 128 << 20;
/// zstd level for saves: fast enough for a large suspend keyframe on quit, still a strong ratio on RON.
const LEVEL: i32 = 6;

const SAVE_FILE: &str = "save";
const SUSPEND_FILE: &str = "suspend save";

fn compress(bytes: &[u8]) -> Vec<u8> {
    let framed = || -> std::io::Result<Vec<u8>> {
        let mut c = zstd::bulk::Compressor::new(LEVEL)?;
        c.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true))?;
        c.compress(bytes)
    };
    // Compression fails only when allocation does; the share codec writes the same frame format.
    framed().unwrap_or_else(|_| crate::share::compress(bytes))
}

/// Decompress, check the text limits, migrate and parse.
fn decode_text<T: serde::de::DeserializeOwned>(bytes: &[u8], file: &str) -> ContentResult<T> {
    let raw = crate::share::decompress_bounded(bytes, MAX_SAVE_BYTES).map_err(|e| text::rename_file(e, file))?;
    let text = String::from_utf8(raw).map_err(|_| ContentError::Parse { file: file.into(), message: "not UTF-8 text".into() })?;
    if text::max_depth(&text) > MAX_RON_DEPTH {
        return Err(ContentError::Limit("RON nesting deeper than 128"));
    }
    let from = crate::migrate::peek_format(&text).map_err(|e| text::rename_file(e, file))?;
    let migrated = crate::migrate::migrate_text(&text, crate::migrate::SAVE_MIGRATIONS, from, SAVE_FORMAT)?;
    crate::data::parse_ron(&migrated, file)
}

/// Canonical RON of a save, the text that is compressed and hashed.
fn save_text(save: &SaveGame) -> String { text::to_canonical(save) }

pub(super) fn encode_save(save: &SaveGame) -> Vec<u8> { compress(save_text(save).as_bytes()) }

pub(super) fn decode_save(bytes: &[u8]) -> ContentResult<SaveGame> {
    let save: SaveGame = decode_text(bytes, SAVE_FILE)?;
    check_header(&save.header, SAVE_FILE)?;
    Ok(save)
}

fn check_header(h: &SaveHeader, file: &str) -> ContentResult<()> {
    if h.format != SAVE_FORMAT {
        return Err(ContentError::Parse { file: file.into(), message: format!("format {} after migration, expected {SAVE_FORMAT}", h.format) });
    }
    Ok(())
}

#[derive(Serialize)]
struct SuspendOut<'a> {
    header: &'a SaveHeader,
    theatre: &'a TheatreState,
    sortie: CheckpointFile,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SuspendIn {
    header: SaveHeader,
    theatre: TheatreState,
    sortie: CheckpointFile,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointFile {
    tick: u64,
    keyframe_format: u32,
    /// Base64 (standard alphabet, no padding).
    config: String,
    keyframe: String,
}

fn b64() -> base64::engine::GeneralPurpose { base64::engine::general_purpose::STANDARD_NO_PAD }

pub(super) fn encode_suspend(save: &SuspendSave) -> Vec<u8> {
    use base64::Engine;
    let s = &save.sortie;
    let out = SuspendOut {
        header: &save.header,
        theatre: &save.theatre,
        sortie: CheckpointFile { tick: s.tick, keyframe_format: s.keyframe_format, config: b64().encode(&s.config), keyframe: b64().encode(&s.keyframe) },
    };
    compress(text::to_canonical(&out).as_bytes())
}

pub(super) fn decode_suspend(bytes: &[u8]) -> ContentResult<SuspendSave> {
    use base64::Engine;
    let s: SuspendIn = decode_text(bytes, SUSPEND_FILE)?;
    check_header(&s.header, SUSPEND_FILE)?;
    let bytes_of = |field: &str, v: &str| b64().decode(v.as_bytes()).map_err(|_| ContentError::Parse { file: SUSPEND_FILE.into(), message: format!("sortie {field} is not base64") });
    let sortie = SortieCheckpoint { tick: s.sortie.tick, keyframe_format: s.sortie.keyframe_format, config: bytes_of("config", &s.sortie.config)?, keyframe: bytes_of("keyframe", &s.sortie.keyframe)? };
    Ok(SuspendSave { header: s.header, theatre: s.theatre, sortie })
}

pub(super) fn save_hash(save: &SaveGame) -> ContentHash { crate::blueprint::io::blake3_hash(save_text(save).as_bytes()) }

/// Highest sequence wins. Adjacent copies must chain (the newer one's `prev_hash` is the older one's hash);
/// equal sequences with different content, a broken link or another campaign are forks. A gap of more than
/// one save cannot be checked from two headers and is accepted: a member who missed several syncs takes the
/// newest copy.
pub(super) fn reconcile(mine: &SaveHeader, mine_hash: ContentHash, theirs: &SaveHeader, theirs_hash: ContentHash) -> Reconcile {
    if mine.campaign_id != theirs.campaign_id {
        return Reconcile::Forked;
    }
    if mine_hash == theirs_hash {
        return if mine.sequence == theirs.sequence { Reconcile::Same } else { Reconcile::Forked };
    }
    let chained = |older: &SaveHeader, older_hash: ContentHash, newer: &SaveHeader| newer.sequence > older.sequence.saturating_add(1) || newer.prev_hash == older_hash;
    match mine.sequence.cmp(&theirs.sequence) {
        std::cmp::Ordering::Equal => Reconcile::Forked,
        std::cmp::Ordering::Less => {
            if chained(mine, mine_hash, theirs) { Reconcile::TakeTheirs } else { Reconcile::Forked }
        }
        std::cmp::Ordering::Greater => {
            if chained(theirs, theirs_hash, mine) { Reconcile::TakeMine } else { Reconcile::Forked }
        }
    }
}
