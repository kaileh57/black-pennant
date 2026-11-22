//! Unit `net-replay`. Replays in the session and the wire-stream capture. DESIGN.md L1043-1050 (replays are
//! the authoritative output stream; 20 most recent plus pinned kept in the user data directory; PvP replays
//! released only after the match on the low-priority reliable lane with exterior-only blueprints for the other
//! team), L939 (nightly: record the server output stream on one OS, decode it on the other; decoded poses
//! within quantisation, client-integrated shells within 0.5 m of the server's at impact).
//!
//! Recording itself is `sim_systems::replay::record::ReplayRecorder`; the PvP cut is `cut_for_side`; retention
//! is `retention`. This unit wraps them for the session and the file system and adds the wire capture.
//!
//! # Choices this unit makes where the spec is silent (design decisions)
//! - Store layout: `<dir>/NNNNNN-hhhhhhhh.bpreplay`, a six-digit save sequence and the first 8 hex digits of
//!   the BLAKE3 of the bytes, plus `<dir>/index.ron` holding the next sequence and each file's sequence and
//!   pinned flag. The sequence is the recorded order for retention, so the order survives copies and clock
//!   changes. Files and the index are written through a temporary file and a rename.
//! - Files the index does not know (copied in by hand) are listed with the sequence in their name, or 0;
//!   files that are not replays of this build's format are left alone and not listed. The list is newest
//!   first, and each entry's `header.pinned` shows the index's flag.
//! - Only names the store itself writes are accepted by `load` and `set_pinned`: no path can leave the
//!   directory.
//! - The wire decode reads vessel poses straight from the decoder's quantised state (integers, so two
//!   platforms must agree bit for bit) for the vessels each frame updated. A shell's client position at
//!   impact is the point of the client's own flight, from its decoded spawn, closest to the server's impact
//!   or splash point on the step that hit (`ShellTracker`'s convention).
use crate::channel::Channel;
use crate::delta::{ItemKey, ItemVal, SnapshotDecoder};
use crate::error::NetError;
use crate::interp::{band_drag_k, ShellTracker};
use crate::msg::WireEvent;
use content::ids::SideId;
use content::replay::ReplayHeader;
use serde::{Deserialize, Serialize};
use sim_core::events::PhysEvent;
use sim_systems::protocol::event::{OutEvent, SimEvent};
use sim_systems::replay::record::{RecordOptions, ReplayRecorder};
use sim_systems::Sim;
use std::io::Read;
use std::path::PathBuf;

/// The session's recorder: started with the sortie, fed every drained event batch, finished at the end.
pub struct ReplaySink { pub(crate) recorder: ReplayRecorder, pub(crate) pvp: bool }
impl ReplaySink {
    /// Unit: `net-replay`.
    pub fn start(sim: &Sim, game_version: &str, options: RecordOptions) -> ReplaySink {
        let pvp = matches!(sim.config().mode, content::enums::GameMode::PvpElimination | content::enums::GameMode::PvpEscort);
        ReplaySink { recorder: ReplayRecorder::start(sim, game_version, options), pvp }
    }
    /// Unit: `net-replay`.
    ///
    /// Call after each `Sim::step` with the events it emitted (all audiences).
    pub fn record(&mut self, sim: &Sim, events: &[OutEvent]) { self.recorder.record_tick(sim, events); }
    /// Unit: `net-replay`.
    pub fn finish(self) -> Vec<u8> { self.recorder.finish() }
    /// True for a PvP sortie: the full replay stays on the host and each side gets [`release_pvp`]'s cut.
    pub fn is_pvp(&self) -> bool { self.pvp }
}

/// Per-side copies of a finished PvP replay for release after the match. Unit: `net-replay`.
///
/// Each side's copy holds only what that side could see, with exterior-only designs for the other team
/// (`cut_for_side`). A side listed twice gets one copy.
pub fn release_pvp(replay: &[u8], sides: &[SideId]) -> Result<Vec<(SideId, Vec<u8>)>, NetError> {
    let mut sides: Vec<SideId> = sides.to_vec();
    sides.sort();
    sides.dedup();
    sides.into_iter().map(|s| Ok((s, sim_systems::replay::record::cut_for_side(replay, s)?))).collect()
}

/// One stored replay as the replay list shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplayEntry { pub file: String, pub header: ReplayHeader, pub bytes: u64, pub pinned: bool }

/// File extension of stored replays.
pub const REPLAY_EXT: &str = "bpreplay";
const INDEX_FILE: &str = "index.ron";

/// The RON index beside the replay files.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct StoreIndex { next: u64, files: Vec<IndexRow> }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct IndexRow { file: String, order: u64, pinned: bool }

/// Replays in `<user data>/replays`: pinned state in a small RON index beside the files.
pub struct ReplayStore { pub(crate) dir: PathBuf }
impl ReplayStore {
    pub fn new(dir: PathBuf) -> Self { Self { dir } }

    fn read_index(&self) -> StoreIndex {
        match std::fs::read_to_string(self.dir.join(INDEX_FILE)) {
            Ok(text) => ron::from_str(&text).unwrap_or_default(),
            Err(_) => StoreIndex::default(),
        }
    }
    fn write_index(&self, index: &StoreIndex) -> std::io::Result<()> {
        let text = ron::to_string(index).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        write_atomic(&self.dir.join(INDEX_FILE), text.as_bytes())
    }
    /// Replay files on disk, by name.
    fn files_on_disk(&self) -> std::io::Result<Vec<(String, u64)>> {
        let mut out = Vec::new();
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for entry in rd {
            let Ok(entry) = entry else { continue };
            let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
            if !valid_name(&name) { continue; }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_file() { out.push((name, meta.len())); }
        }
        out.sort();
        Ok(out)
    }

    /// Unit: `net-replay`.
    pub fn list(&self) -> std::io::Result<Vec<ReplayEntry>> {
        let index = self.read_index();
        let mut out: Vec<(u64, ReplayEntry)> = Vec::new();
        for (file, bytes) in self.files_on_disk()? {
            let Ok(mut header) = read_header_from(&self.dir.join(&file)) else { continue };
            let row = index.files.iter().find(|r| r.file == file);
            let pinned = row.is_some_and(|r| r.pinned);
            let order = row.map_or_else(|| sequence_of(&file), |r| r.order);
            header.pinned = pinned;
            out.push((order, ReplayEntry { file, header, bytes, pinned }));
        }
        out.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.file.cmp(&a.1.file)));
        Ok(out.into_iter().map(|e| e.1).collect())
    }
    /// Save and apply retention (20 most recent plus pinned). Returns the file name. Unit: `net-replay`.
    ///
    /// Bytes that are not a replay of this build's format are refused (`InvalidData`).
    pub fn save(&self, bytes: &[u8]) -> std::io::Result<String> {
        sim_systems::replay::play::ReplayPlayer::read_header(bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        std::fs::create_dir_all(&self.dir)?;
        let mut index = self.read_index();
        let on_disk = self.files_on_disk()?;
        // Rows of files that are gone are forgotten; files the index never saw join with their own sequence.
        index.files.retain(|r| on_disk.iter().any(|(f, _)| *f == r.file));
        for (f, _) in &on_disk {
            if !index.files.iter().any(|r| r.file == *f) { index.files.push(IndexRow { file: f.clone(), order: sequence_of(f), pinned: false }); }
        }
        let next = index.next.max(index.files.iter().map(|r| r.order + 1).max().unwrap_or(0));
        let hash = content::blueprint::io::blake3_hash(bytes).to_hex();
        let name = format!("{next:06}-{}.{REPLAY_EXT}", &hash[..8]);
        write_atomic(&self.dir.join(&name), bytes)?;
        index.next = next + 1;
        index.files.retain(|r| r.file != name);
        index.files.push(IndexRow { file: name.clone(), order: next, pinned: false });
        let rows: Vec<(String, u64, bool)> = index.files.iter().map(|r| (r.file.clone(), r.order, r.pinned)).collect();
        for gone in sim_systems::replay::record::retention(&rows) {
            if gone == name { continue; }
            match std::fs::remove_file(self.dir.join(&gone)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            index.files.retain(|r| r.file != gone);
        }
        index.files.sort_by_key(|r| r.order);
        self.write_index(&index)?;
        Ok(name)
    }
    /// Unit: `net-replay`.
    pub fn set_pinned(&self, file: &str, pinned: bool) -> std::io::Result<()> {
        if !valid_name(file) { return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a replay file name")); }
        if !self.dir.join(file).is_file() { return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no such replay")); }
        let mut index = self.read_index();
        match index.files.iter_mut().find(|r| r.file == file) {
            Some(r) => r.pinned = pinned,
            None => index.files.push(IndexRow { file: file.to_string(), order: sequence_of(file), pinned }),
        }
        index.next = index.next.max(index.files.iter().map(|r| r.order + 1).max().unwrap_or(0));
        index.files.sort_by_key(|r| r.order);
        self.write_index(&index)
    }
    /// Unit: `net-replay`.
    pub fn load(&self, file: &str) -> std::io::Result<Vec<u8>> {
        if !valid_name(file) { return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a replay file name")); }
        std::fs::read(self.dir.join(file))
    }
}

/// A name the store writes: `NNNNNN-hhhhhhhh.bpreplay` with lowercase hex (no separators, no dots elsewhere).
fn valid_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(&format!(".{REPLAY_EXT}")) else { return false };
    let Some((seq, hash)) = stem.split_once('-') else { return false };
    !seq.is_empty() && seq.len() <= 20 && seq.bytes().all(|b| b.is_ascii_digit()) && hash.len() == 8 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn sequence_of(name: &str) -> u64 { name.split('-').next().and_then(|s| s.parse().ok()).unwrap_or(0) }

/// Read only the preamble and header of a replay file.
fn read_header_from(path: &std::path::Path) -> std::io::Result<ReplayHeader> {
    let mut f = std::fs::File::open(path)?;
    let mut pre = [0u8; 12];
    f.read_exact(&mut pre)?;
    let len = u32::from_le_bytes([pre[8], pre[9], pre[10], pre[11]]) as usize;
    if len > sim_systems::replay::record::HEADER_MAX_BYTES { return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "header too long")); }
    let mut bytes = pre.to_vec();
    bytes.resize(12 + len, 0);
    f.read_exact(&mut bytes[12..])?;
    sim_systems::replay::play::ReplayPlayer::read_header(&bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, bytes)?;
    // Windows rename does not replace an existing file.
    if path.exists() { let _ = std::fs::remove_file(path); }
    std::fs::rename(&tmp, path).inspect_err(|_| { let _ = std::fs::remove_file(&tmp); })
}

/// The exact bytes the server sent one client, with the sim tick of each, for the cross-platform decode
/// check. Snapshot frames and event batches only.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamCapture { pub protocol: u32, pub frames: Vec<(u64, u8, Vec<u8>)> }
impl StreamCapture {
    pub fn push(&mut self, tick: u64, channel: crate::channel::Channel, bytes: &[u8]) { self.frames.push((tick, channel.id(), bytes.to_vec())); }
}

/// Decoded content of a capture: every vessel pose at every frame and every shell's client-integrated
/// position at its impact, for comparison with the other OS's decode or with the server's own record.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DecodedStream { pub poses: Vec<(u64, u64, crate::schema::QuantPose)>, pub shell_impacts: Vec<(u32, [f64; 3], [f64; 3])> }

/// Decode a capture through the real client path (`SnapshotDecoder`, `ShellTracker`). Unit: `net-replay`.
///
/// `poses` holds (frame tick, entity as u64, quantised pose) for each vessel a frame updated, in stream
/// order; `shell_impacts` holds (projectile, client point, server point) for each shell whose impact or
/// splash the stream carries after its spawn. A capture of another wire version, or a frame or batch that
/// does not decode, is an error.
pub fn decode_capture(capture: &StreamCapture, catalogue: &content::catalogue::Catalogue) -> Result<DecodedStream, NetError> {
    if capture.protocol != crate::WIRE_VERSION { return Err(NetError::Decode(format!("capture of wire version {}, this build speaks {}", capture.protocol, crate::WIRE_VERSION))); }
    let mut decoder = SnapshotDecoder::new();
    let mut shells = ShellTracker::new();
    let mut out = DecodedStream::default();
    for (i, (_tick, ch, bytes)) in capture.frames.iter().enumerate() {
        match Channel::from_id(*ch) {
            Some(Channel::Unreliable) => {
                let Some(frame) = decoder.decode(bytes).map_err(|e| NetError::Decode(format!("frame {i}: {e}")))? else { continue };
                let Some((_, state)) = decoder.history.back() else { continue };
                for key in &frame.fresh {
                    if let (ItemKey::Vessel(e), Some(ItemVal::Vessel(w))) = (key, state.items.get(key)) { out.poses.push((frame.tick, e.to_u64(), w.pose)); }
                }
            }
            Some(Channel::Events | Channel::Entity | Channel::Projectile) => {
                let batch = crate::schema::codec::decode_events(bytes).map_err(|e| NetError::Decode(format!("event batch {i}: {e}")))?;
                for e in &batch.events { decode_event(e, &mut shells, catalogue, &mut out); }
            }
            _ => {}
        }
    }
    Ok(out)
}

fn decode_event(e: &WireEvent, shells: &mut ShellTracker, catalogue: &content::catalogue::Catalogue, out: &mut DecodedStream) {
    match &e.event {
        SimEvent::Phys(PhysEvent::ProjectileSpawned(s)) => shells.spawn(s, band_drag_k(catalogue, s.band)),
        SimEvent::Phys(PhysEvent::ShellImpact { projectile, world_point: p, .. }) | SimEvent::Phys(PhysEvent::Splash { projectile, pos: p, .. }) => {
            if let Some(c) = shells.impact_point(*projectile, e.tick, *p) { out.shell_impacts.push((projectile.0, c.to_array(), p.to_array())); }
            shells.remove(*projectile);
        }
        _ => {}
    }
}

/// Most mismatch lines reported per kind, so a broken run does not flood the CI log.
const MAX_LINES: usize = 20;

/// Compare two decodes: poses identical (they are integers), shells within `tolerance_m` (0.5). Returns
/// human-readable mismatch lines for the CI log (developer-facing). Unit: `net-replay`.
///
/// Shells are checked on each side against the server's point the stream carried, and the two sides must
/// have decoded the same shells. An empty result means the decodes agree.
pub fn compare_decodes(a: &DecodedStream, b: &DecodedStream, tolerance_m: f64) -> Vec<String> {
    let tol = if tolerance_m.is_finite() { tolerance_m.max(0.0) } else { 0.5 };
    let mut out = Vec::new();
    if a.poses.len() != b.poses.len() { out.push(format!("pose count differs: {} against {}", a.poses.len(), b.poses.len())); }
    let mut n = 0;
    for (i, (x, y)) in a.poses.iter().zip(&b.poses).enumerate() {
        if x != y {
            n += 1;
            if n <= MAX_LINES { out.push(format!("pose {i}: tick {} entity {:#x} {:?} against tick {} entity {:#x} {:?}", x.0, x.1, x.2, y.0, y.1, y.2)); }
        }
    }
    if n > MAX_LINES { out.push(format!("... {} more pose mismatches", n - MAX_LINES)); }
    let dist = |p: [f64; 3], q: [f64; 3]| { let d = glam::DVec3::from_array(p) - glam::DVec3::from_array(q); if d.is_finite() { d.length() } else { f64::INFINITY } };
    for (label, s) in [("first", a), ("second", b)] {
        let mut n = 0;
        for (id, client, server) in &s.shell_impacts {
            let d = dist(*client, *server);
            if d > tol {
                n += 1;
                if n <= MAX_LINES { out.push(format!("{label} decode: shell {id} lands {d:.3} m from the server's impact (limit {tol} m)")); }
            }
        }
        if n > MAX_LINES { out.push(format!("... {} more {label}-decode shell misses", n - MAX_LINES)); }
    }
    let ids = |s: &DecodedStream| s.shell_impacts.iter().map(|x| x.0).collect::<Vec<u32>>();
    let (ia, ib) = (ids(a), ids(b));
    if ia != ib {
        let only_a = ia.iter().filter(|x| !ib.contains(x)).count();
        let only_b = ib.iter().filter(|x| !ia.contains(x)).count();
        out.push(format!("shell sets differ: {only_a} only in the first decode, {only_b} only in the second"));
    }
    for (x, y) in a.shell_impacts.iter().zip(&b.shell_impacts) {
        if x.0 == y.0 && dist(x.1, y.1) > tol { out.push(format!("shell {}: the decodes put it {:.3} m apart", x.0, dist(x.1, y.1))); }
    }
    out
}

#[cfg(test)]
mod tests;
