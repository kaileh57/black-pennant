//! Unit `net-blueprints`. Content-addressed blueprints in multiplayer and the generic chunked bulk transfer.
//! DESIGN.md L943 (BLAKE3 address of canonical RON; bytes travel and are stored compressed with the share
//! string codec; 16 KiB chunks on the low-priority reliable lane, at most 25% of the client's cap during a live
//! match and uncapped in the lobby; client cache 512 MiB, least recently used out; the server refuses any
//! blueprint over 8 MiB compressed, then re-validates schema, part legality, ruleset limits, bounds and
//! connectivity and recomputes every stat), L945 (PvP opponents only ever get the exterior-only form).
//!
//! Compression and hashing go through `content::share::{pack, unpack}` and
//! `content::blueprint::io::{exterior_only, exterior_hash}`; this module does not touch the codec, so the open
//! zstd decision does not block it. Building the received ship (worker thread, upload slices of at most
//! 0.5 ms per frame) is gdclient's job.
//!
//! Choices this unit makes where the spec is silent (design decisions):
//! - `TransferStart::hash` is the BLAKE3 of the transferred bytes for every kind, set by [`Outgoing::enqueue`]
//!   and checked by [`Incoming`] when a transfer completes. A blueprint's content address (the hash of its
//!   canonical RON, carried in `TransferKind::Blueprint`) is checked when the bytes are unpacked:
//!   [`accept_upload`] on the host, [`unpack_exterior`] or `content::share::unpack` on a client.
//! - Bulk budget carries over: a live match gives the lane about 133 to 800 bytes per 30 Hz interval, less
//!   than one 16 KiB chunk, so unused budget accumulates (up to one chunk plus one interval) while anything
//!   is queued and resets when the queue is idle. The long-run rate is exactly the budget.
//! - Incoming transfers must arrive in order (the bulk lane is reliable and ordered), with every chunk 16 KiB
//!   except the last; at most [`MAX_PARTIAL_TRANSFERS`] run at once from one peer and no buffer is
//!   allocated ahead of the bytes that arrive. A save copy is limited like a checkpoint, 64 MiB.
//! - The disk cache names each file by the 64-digit hex hash, writes through a temporary file and a rename,
//!   and bumps a file's modified time when it is read so least-recently-used order survives a restart.
use crate::msg::{BlueprintRefusal, BulkMsg, TransferKind, TransferStart};
use content::blueprint::{Blueprint, ExteriorBlueprint};
use content::catalogue::Catalogue;
use content::ids::ContentHash;
use content::ruleset::Ruleset;
use content::share::PackedBlueprint;
use std::path::PathBuf;

pub const CLIENT_CACHE_BYTES: u64 = 512 * 1024 * 1024;
pub const CHUNK_BYTES: usize = content::share::BLUEPRINT_CHUNK_BYTES;
/// Share of the client's cap the bulk lane may use while a match is live (L943).
pub const LIVE_BULK_SHARE: f32 = 0.25;
/// Largest transfer accepted per kind; blueprints use `content::blueprint::MAX_COMPRESSED_BYTES`.
pub const MAX_CHECKPOINT_BYTES: u32 = 64 << 20;
pub const MAX_REPLAY_BYTES: u32 = 64 << 20;
/// Largest save copy accepted.
pub const MAX_SAVE_COPY_BYTES: u32 = MAX_CHECKPOINT_BYTES;
/// Concurrent partial incoming transfers kept from one peer.
pub const MAX_PARTIAL_TRANSFERS: usize = 8;
/// Bytes a `Start` message costs against the bulk budget (encoded size, rounded up).
const START_COST: usize = 64;
/// Framing on top of a chunk's payload (id, index, length prefix, enum tag), rounded up.
const CHUNK_OVERHEAD: usize = 16;

/// Size limit of a transfer kind.
pub fn max_transfer_bytes(kind: TransferKind) -> u32 {
    match kind {
        TransferKind::Blueprint(_) => content::blueprint::MAX_COMPRESSED_BYTES as u32,
        TransferKind::Checkpoint(_) => MAX_CHECKPOINT_BYTES,
        TransferKind::SaveCopy(_) => MAX_SAVE_COPY_BYTES,
        TransferKind::Replay => MAX_REPLAY_BYTES,
    }
}

fn chunk_count(total: usize) -> usize { total.div_ceil(CHUNK_BYTES) }

/// Where compressed blueprints live: the client's on-disk cache, the host's in-memory set.
pub trait BlueprintStore: Send {
    fn get(&mut self, hash: &ContentHash) -> Option<Vec<u8>>;
    fn contains(&self, hash: &ContentHash) -> bool;
    fn put(&mut self, hash: ContentHash, compressed: Vec<u8>);
}

/// In-memory store (host, tests). Real code.
#[derive(Clone, Debug, Default)]
pub struct MemoryStore { pub items: std::collections::BTreeMap<ContentHash, Vec<u8>> }
impl BlueprintStore for MemoryStore {
    fn get(&mut self, hash: &ContentHash) -> Option<Vec<u8>> { self.items.get(hash).cloned() }
    fn contains(&self, hash: &ContentHash) -> bool { self.items.contains_key(hash) }
    fn put(&mut self, hash: ContentHash, compressed: Vec<u8>) { self.items.insert(hash, compressed); }
}

/// The client's on-disk cache: one file per hash (hex name) under `dir`, LRU by access, `budget` bytes.
pub struct DiskCache { pub(crate) dir: PathBuf, pub(crate) budget: u64, pub(crate) index: indexmap::IndexMap<ContentHash, u64>, pub(crate) total: u64 }
impl DiskCache {
    /// Open or create; scan existing files into the LRU index (oldest modified first). Unit: `net-blueprints`.
    pub fn open(dir: PathBuf, budget: u64) -> std::io::Result<DiskCache> {
        std::fs::create_dir_all(&dir)?;
        let mut found: Vec<(std::time::SystemTime, ContentHash, u64)> = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
            // Leftovers of an interrupted write.
            if name.ends_with(TMP_SUFFIX) { let _ = std::fs::remove_file(&path); continue; }
            let Some(hash) = parse_name(name) else { continue };
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() { continue; }
            let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            found.push((modified, hash, meta.len()));
        }
        // Oldest first; ties by hash so the order is the same on every platform.
        found.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut cache = DiskCache { dir, budget, index: indexmap::IndexMap::new(), total: 0 };
        for (_, hash, len) in found {
            cache.index.insert(hash, len);
            cache.total += len;
        }
        cache.evict();
        Ok(cache)
    }

    /// Bytes currently held.
    pub fn total_bytes(&self) -> u64 { self.total }

    fn path_of(&self, hash: &ContentHash) -> PathBuf { self.dir.join(hash.to_hex()) }

    /// Drop least recently used entries until the cache fits its budget.
    fn evict(&mut self) {
        while self.total > self.budget {
            let Some((hash, len)) = self.index.shift_remove_index(0) else { break };
            self.total = self.total.saturating_sub(len);
            let _ = std::fs::remove_file(self.path_of(&hash));
        }
    }

    fn forget(&mut self, hash: &ContentHash) {
        if let Some(len) = self.index.shift_remove(hash) { self.total = self.total.saturating_sub(len); }
    }

    /// Mark as most recently used, in memory and on disk.
    fn touch(&mut self, hash: &ContentHash) {
        if let Some(len) = self.index.shift_remove(hash) { self.index.insert(*hash, len); }
        if let Ok(f) = std::fs::File::options().write(true).open(self.path_of(hash)) {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
    }
}

const TMP_SUFFIX: &str = ".part";

/// A cache file name: exactly 64 lowercase hex digits, not the empty hash.
fn parse_name(name: &str) -> Option<ContentHash> {
    if name.len() != 64 || !name.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { return None; }
    ContentHash::from_hex(name).filter(|h| !h.is_empty())
}

impl BlueprintStore for DiskCache {
    fn get(&mut self, hash: &ContentHash) -> Option<Vec<u8>> {
        if !self.index.contains_key(hash) { return None; }
        match std::fs::read(self.path_of(hash)) {
            Ok(bytes) => {
                // Someone changed the file under us: keep the index honest.
                let len = bytes.len() as u64;
                if let Some(old) = self.index.get_mut(hash) {
                    self.total = self.total.saturating_sub(*old) + len;
                    *old = len;
                }
                self.touch(hash);
                self.evict();
                Some(bytes)
            }
            Err(_) => { self.forget(hash); None }
        }
    }
    fn contains(&self, hash: &ContentHash) -> bool { self.index.contains_key(hash) }
    fn put(&mut self, hash: ContentHash, compressed: Vec<u8>) {
        if hash.is_empty() { return; }
        let len = compressed.len() as u64;
        if len > self.budget { return; }
        if self.index.get(&hash) == Some(&len) { self.touch(&hash); return; }
        let path = self.path_of(&hash);
        let tmp = self.dir.join(format!("{}{TMP_SUFFIX}", hash.to_hex()));
        let written = std::fs::write(&tmp, &compressed).and_then(|_| {
            // Windows rename does not replace an existing file.
            if path.exists() { let _ = std::fs::remove_file(&path); }
            std::fs::rename(&tmp, &path)
        });
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        self.forget(&hash);
        self.index.insert(hash, len);
        self.total += len;
        self.evict();
    }
}

/// Outgoing chunked transfers to one peer, oldest first.
#[derive(Clone, Debug, Default)]
///
/// Each queue entry is (start record, bytes, messages sent): message 0 is the `Start`, message k is chunk k-1.
pub struct Outgoing { pub(crate) next_id: u32, pub(crate) queue: std::collections::VecDeque<(TransferStart, Vec<u8>, u32)>, pub(crate) credit: usize }
impl Outgoing {
    pub fn new() -> Self { Self::default() }
    /// Queue a transfer; returns its start record (also sent as the first `BulkMsg`). Unit: `net-blueprints`.
    pub fn enqueue(&mut self, kind: TransferKind, bytes: Vec<u8>) -> TransferStart {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let total_bytes = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        let chunks = u32::try_from(chunk_count(bytes.len())).unwrap_or(u32::MAX);
        let start = TransferStart { id, kind, total_bytes, chunks, hash: content::blueprint::io::blake3_hash(&bytes) };
        self.queue.push_back((start, bytes, 0));
        start
    }
    /// Messages to send now within `budget_bytes` (whole chunks only). Unit: `net-blueprints`.
    ///
    /// Unused budget carries over to the next call while anything is queued, up to one chunk plus one call's
    /// budget, so a budget smaller than a chunk still moves data at its average rate.
    pub fn take(&mut self, budget_bytes: usize) -> Vec<BulkMsg> {
        let mut out = Vec::new();
        if self.queue.is_empty() { self.credit = 0; return out; }
        let ceiling = (CHUNK_BYTES + CHUNK_OVERHEAD).saturating_add(budget_bytes);
        self.credit = self.credit.saturating_add(budget_bytes).min(ceiling);
        while let Some((start, bytes, sent)) = self.queue.front_mut() {
            let total_msgs = start.chunks as u64 + 1;
            if (*sent as u64) >= total_msgs { self.queue.pop_front(); continue; }
            let msg_cost = if *sent == 0 { START_COST } else {
                let i = (*sent - 1) as usize * CHUNK_BYTES;
                (bytes.len().min(i + CHUNK_BYTES) - i.min(bytes.len())) + CHUNK_OVERHEAD
            };
            if msg_cost > self.credit { break; }
            self.credit -= msg_cost;
            if *sent == 0 {
                out.push(BulkMsg::Start(*start));
            } else {
                let index = *sent - 1;
                let i = index as usize * CHUNK_BYTES;
                let end = bytes.len().min(i + CHUNK_BYTES);
                out.push(BulkMsg::Chunk { id: start.id, index, bytes: bytes[i.min(end)..end].to_vec() });
            }
            *sent += 1;
            if (*sent as u64) >= total_msgs { self.queue.pop_front(); }
        }
        if self.queue.is_empty() { self.credit = 0; }
        out
    }
    pub fn is_idle(&self) -> bool { self.queue.is_empty() }
    /// Progress of a transfer, 0..=1. Unit: `net-blueprints`.
    ///
    /// `None` once the transfer has been sent completely (or for an unknown id).
    pub fn progress(&self, id: u32) -> Option<f32> {
        let (start, _, sent) = self.queue.iter().find(|(s, _, _)| s.id == id)?;
        if start.chunks == 0 { return Some(if *sent > 0 { 1.0 } else { 0.0 }); }
        Some((sent.saturating_sub(1) as f64 / start.chunks as f64).clamp(0.0, 1.0) as f32)
    }
    /// Stop a queued transfer; returns the `Cancel` to send if any part of it went out.
    pub fn cancel(&mut self, id: u32) -> Option<BulkMsg> {
        let i = self.queue.iter().position(|(s, _, _)| s.id == id)?;
        let (_, _, sent) = self.queue.remove(i)?;
        (sent > 0).then_some(BulkMsg::Cancel { id })
    }
}

/// A completed incoming transfer.
#[derive(Clone, Debug, PartialEq)]
pub struct Completed { pub kind: TransferKind, pub bytes: Vec<u8> }

/// Why an incoming transfer was abandoned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncomingError { TooLarge, BadChunk, HashMismatch, Unknown }

/// Reassembly of incoming chunked transfers from one peer, with per-kind size limits checked on `Start`.
#[derive(Clone, Debug, Default)]
///
/// Each partial entry is (start record, bytes so far, chunks received).
pub struct Incoming { pub(crate) partial: std::collections::BTreeMap<u32, (TransferStart, Vec<u8>, u32)> }
impl Incoming {
    pub fn new() -> Self { Self::default() }
    /// Feed one message; returns a completed transfer (hash verified with BLAKE3 for blueprints, by
    /// `TransferStart::hash` for the rest). Unit: `net-blueprints`.
    ///
    /// Every kind's bytes are checked against `TransferStart::hash` (BLAKE3 of the bytes); see the module
    /// notes. A failed transfer is dropped; others from the same peer carry on.
    pub fn on_msg(&mut self, msg: BulkMsg) -> Result<Option<Completed>, IncomingError> {
        match msg {
            BulkMsg::Start(start) => {
                // A restarted id replaces its old partial.
                self.partial.remove(&start.id);
                if start.total_bytes > max_transfer_bytes(start.kind) { return Err(IncomingError::TooLarge); }
                if start.chunks as usize != chunk_count(start.total_bytes as usize) { return Err(IncomingError::BadChunk); }
                if self.partial.len() >= MAX_PARTIAL_TRANSFERS { return Err(IncomingError::TooLarge); }
                if start.chunks == 0 { return finish(start, Vec::new()).map(Some); }
                self.partial.insert(start.id, (start, Vec::new(), 0));
                Ok(None)
            }
            BulkMsg::Chunk { id, index, bytes } => {
                let Some((start, buf, got)) = self.partial.get_mut(&id) else { return Err(IncomingError::Unknown) };
                let offset = *got as usize * CHUNK_BYTES;
                let expected = (start.total_bytes as usize - offset).min(CHUNK_BYTES);
                if index != *got || bytes.len() != expected {
                    self.partial.remove(&id);
                    return Err(IncomingError::BadChunk);
                }
                buf.extend_from_slice(&bytes);
                *got += 1;
                if *got < start.chunks { return Ok(None); }
                let Some((start, buf, _)) = self.partial.remove(&id) else { return Err(IncomingError::Unknown) };
                finish(start, buf).map(Some)
            }
            BulkMsg::Cancel { id } => { self.partial.remove(&id); Ok(None) }
        }
    }
    /// Progress of the transfer of a kind, 0..=1 (lobby upload bar). Unit: `net-blueprints`.
    pub fn progress(&self, kind: TransferKind) -> Option<f32> {
        let (start, _, got) = self.partial.values().find(|(s, _, _)| s.kind == kind)?;
        Some(if start.chunks == 0 { 1.0 } else { (*got as f64 / start.chunks as f64).clamp(0.0, 1.0) as f32 })
    }
}

fn finish(start: TransferStart, bytes: Vec<u8>) -> Result<Completed, IncomingError> {
    if bytes.len() != start.total_bytes as usize { return Err(IncomingError::BadChunk); }
    if content::blueprint::io::blake3_hash(&bytes) != start.hash { return Err(IncomingError::HashMismatch); }
    Ok(Completed { kind: start.kind, bytes })
}

/// Bytes the bulk lane may use this snapshot interval: 25% of the cap in a live match, the whole interval
/// budget otherwise. Real code.
pub fn bulk_budget(interval_bytes: usize, live: bool) -> usize { if live { (interval_bytes as f32 * LIVE_BULK_SHARE) as usize } else { interval_bytes } }

/// Host side: accept an uploaded design. Refuse over 8 MiB compressed, unpack (hash checked), validate under
/// the lobby ruleset with stats recomputed (`content::validate::validate_blueprint`). Unit: `net-blueprints`.
pub fn accept_upload(packed: &PackedBlueprint, catalogue: &Catalogue, ruleset: &Ruleset) -> Result<Blueprint, BlueprintRefusal> {
    if packed.compressed.len() > content::blueprint::MAX_COMPRESSED_BYTES { return Err(BlueprintRefusal::TooLarge); }
    if packed.hash.is_empty() { return Err(BlueprintRefusal::Invalid); }
    let item = content::share::unpack_with(packed, catalogue).map_err(|e| match e {
        content::error::ContentError::Limit(_) => BlueprintRefusal::TooLarge,
        _ => BlueprintRefusal::Invalid,
    })?;
    let content::blueprint::Shareable::Ship(bp) = item else { return Err(BlueprintRefusal::Invalid) };
    let ctx = content::validate::ValidationContext { ruleset, available: None, enemy_class: false, complexity: None };
    let report = content::validate::validate_blueprint(&bp, catalogue, &ctx);
    if !report.is_ok() { return Err(BlueprintRefusal::Invalid); }
    Ok(*bp)
}

/// The exterior-only render form and its packed bytes for the other PvP team (L945). Unit: `net-blueprints`.
pub fn pack_exterior(bp: &Blueprint, catalogue: &Catalogue) -> (ExteriorBlueprint, PackedBlueprint) {
    let ext = content::blueprint::io::exterior_only(bp, catalogue);
    let text = content::blueprint::io::exterior_canonical_ron(&ext);
    let packed = PackedBlueprint { hash: content::blueprint::io::blake3_hash(text.as_bytes()), compressed: content::share::compress(text.as_bytes()) };
    (ext, packed)
}

/// Client side: decode a received exterior form and check its hash against `content::blueprint::io::exterior_hash`.
/// Unit: `net-blueprints`.
pub fn unpack_exterior(packed: &PackedBlueprint) -> Result<ExteriorBlueprint, BlueprintRefusal> {
    if packed.compressed.len() > content::blueprint::MAX_COMPRESSED_BYTES { return Err(BlueprintRefusal::TooLarge); }
    let bytes = content::share::decompress_bounded(&packed.compressed, content::blueprint::MAX_RON_BYTES).map_err(|e| match e {
        content::error::ContentError::Limit(_) => BlueprintRefusal::TooLarge,
        _ => BlueprintRefusal::Invalid,
    })?;
    // The address is the hash of exactly these bytes; check it before parsing anything.
    if content::blueprint::io::blake3_hash(&bytes) != packed.hash { return Err(BlueprintRefusal::Invalid); }
    let text = std::str::from_utf8(&bytes).map_err(|_| BlueprintRefusal::Invalid)?;
    let ext: ExteriorBlueprint = ron::Options::default().with_recursion_limit(content::blueprint::MAX_RON_DEPTH).from_str(text).map_err(|_| BlueprintRefusal::Invalid)?;
    if content::blueprint::io::exterior_hash(&ext) != packed.hash { return Err(BlueprintRefusal::Invalid); }
    Ok(ext)
}

/// Which hashes each client may fetch: own-side full hashes, exterior hashes of the other team, stock classes.
/// Requests for anything else are refused `NotEntitled`; a PvP opponent can never pull a full blueprint.
#[derive(Clone, Debug, Default)]
pub struct Entitlements { pub(crate) per_client: std::collections::BTreeMap<u64, std::collections::BTreeSet<ContentHash>> }
impl Entitlements {
    pub fn grant(&mut self, client_id: u64, hash: ContentHash) { self.per_client.entry(client_id).or_default().insert(hash); }
    pub fn allowed(&self, client_id: u64, hash: &ContentHash) -> bool { self.per_client.get(&client_id).is_some_and(|s| s.contains(hash)) }
    pub fn clear(&mut self) { self.per_client.clear(); }
    /// Forget one client's grants (it left the session).
    pub fn revoke_client(&mut self, client_id: u64) { self.per_client.remove(&client_id); }
}

#[cfg(test)]
mod tests;
