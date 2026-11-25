//! Part of unit `gd-campaign-saves`. The save files on disk, in plain `std::fs` on an OS path so the rules are
//! testable without the engine (DESIGN.md L1359-1360: saves live under the user data directory, opened only
//! through `std::fs` or Godot's file classes, never through a lossy path conversion; Rust std prefixes long
//! Windows paths itself).
//!
//! Layout under the saves root (`user://saves`):
//! `<campaign id as 16 hex digits>/<stem>.bpsave`, where the stem is
//! - `ironman`: an Ironman campaign's one rolling full save (every save overwrites it, L828);
//! - `autosave`: the rolling save written between operations outside Ironman;
//! - `save-<sequence>`: a save the player asked for (the newest [`MANUAL_KEEP`] per campaign are kept);
//! - `copy`: a co-op member's copy of the host's save (L834);
//! - `suspend`: the single suspend slot (L112). Writing one removes every other suspend file, so at most
//!   one exists across all campaigns.
//!
//! The slot name the player typed never becomes a file name. Every write goes to a temporary file first and
//! is renamed over the old one, so a crash mid-write leaves the previous save intact.
use content::save::{SaveGame, SaveHeader, SuspendSave};
use sim_systems::protocol::Msg;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

pub(crate) const SAVES_DIR: &str = "user://saves";
pub(crate) const EXT: &str = "bpsave";
pub(crate) const SUSPEND_STEM: &str = "suspend";
pub(crate) const IRONMAN_STEM: &str = "ironman";
pub(crate) const AUTOSAVE_STEM: &str = "autosave";
pub(crate) const COPY_STEM: &str = "copy";
/// Saves the player asked for, kept per campaign (older ones are removed).
pub(crate) const MANUAL_KEEP: usize = 20;
/// Largest save or suspend file read (the decoder's own decompressed limit is 128 MiB).
pub(crate) const MAX_FILE_BYTES: u64 = 128 << 20;

/// What the slot list shows of one file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SlotInfo {
    /// OS path of the file.
    pub path: PathBuf,
    /// `<campaign dir>/<file name>` relative to the saves root.
    pub rel: String,
    pub header: Option<SaveHeader>,
    pub act: Option<content::enums::Act>,
    pub hour: u64,
    pub players: Vec<String>,
    pub modified_unix: i64,
    pub is_suspend: bool,
    /// Why the file cannot be loaded; `None` = loadable.
    pub problem: Option<Msg>,
}

/// Decoded slot facts by path, valid while the file's modified time and size are unchanged.
#[derive(Default)]
pub(crate) struct SlotCache { entries: BTreeMap<PathBuf, (Option<SystemTime>, u64, Arc<SlotInfo>)> }

pub(crate) fn campaign_dir_name(campaign_id: u64) -> String { format!("{campaign_id:016x}") }
pub(crate) fn campaign_dir(root: &Path, campaign_id: u64) -> PathBuf { root.join(campaign_dir_name(campaign_id)) }
pub(crate) fn file_name(stem: &str) -> String { format!("{stem}.{EXT}") }
pub(crate) fn manual_stem(sequence: u64) -> String { format!("save-{sequence:08}") }

fn is_campaign_dir(name: &str) -> bool { name.len() == 16 && name.bytes().all(|b| b.is_ascii_hexdigit()) }
fn is_save_file(name: &str) -> bool {
    name.strip_suffix(&format!(".{EXT}")).is_some_and(|stem| !stem.is_empty() && stem.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
}

/// The OS path of a save given as `user://saves/<dir>/<file>` (or as the bare `<dir>/<file>`), or `None`
/// when it names anything outside the saves root or not a save file.
pub(crate) fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
    let p = path.trim().replace('\\', "/");
    let rel = p.strip_prefix(SAVES_DIR).map(|r| r.trim_start_matches('/')).unwrap_or(p.as_str());
    let mut parts = rel.split('/');
    let (dir, file) = (parts.next()?, parts.next()?);
    if parts.next().is_some() || !is_campaign_dir(dir) || !is_save_file(file) { return None; }
    Some(root.join(dir).join(file))
}

/// `user://saves/<dir>/<file>` of a slot.
pub(crate) fn user_path(rel: &str) -> String { format!("{SAVES_DIR}/{rel}") }

/// Write bytes through a temporary file and a rename.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => { let _ = std::fs::remove_file(&tmp); Err(e) }
    }
}

/// Write a full save under `stem` in its campaign's folder. Returns the file's path.
pub(crate) fn write_save(root: &Path, save: &SaveGame, stem: &str) -> std::io::Result<PathBuf> {
    let path = campaign_dir(root, save.header.campaign_id).join(file_name(stem));
    write_atomic(&path, &content::save::encode_save(save))?;
    Ok(path)
}

/// Write the suspend slot and remove every other suspend file.
pub(crate) fn write_suspend(root: &Path, save: &SuspendSave) -> std::io::Result<PathBuf> {
    let path = campaign_dir(root, save.header.campaign_id).join(file_name(SUSPEND_STEM));
    write_atomic(&path, &content::save::encode_suspend(save))?;
    for other in suspend_files(root) { if other != path { let _ = std::fs::remove_file(&other); } }
    Ok(path)
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, Msg> {
    let len = std::fs::metadata(path).map_err(|_| Msg::new("campaign.save.not_found"))?.len();
    if len > MAX_FILE_BYTES { return Err(crate::convert::error_msg(&content::ContentError::Limit("save file too large"))); }
    std::fs::read(path).map_err(|_| Msg::new("campaign.save.not_found"))
}
pub(crate) fn read_save(path: &Path) -> Result<SaveGame, Msg> {
    content::save::decode_save(&read_bytes(path)?).map_err(|e| crate::convert::error_msg(&e))
}
pub(crate) fn read_suspend(path: &Path) -> Result<SuspendSave, Msg> {
    content::save::decode_suspend(&read_bytes(path)?).map_err(|e| crate::convert::error_msg(&e))
}
pub(crate) fn is_suspend_path(path: &Path) -> bool { path.file_name().and_then(|n| n.to_str()) == Some(file_name(SUSPEND_STEM).as_str()) }

/// Remove one save file (never anything else). A missing file counts as removed.
pub(crate) fn delete(path: &Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => { remove_dir_if_empty(path.parent()); true }
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}
fn remove_dir_if_empty(dir: Option<&Path>) {
    if let Some(d) = dir { if std::fs::read_dir(d).is_ok_and(|mut r| r.next().is_none()) { let _ = std::fs::remove_dir(d); } }
}

fn campaign_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<PathBuf> = rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).filter(|e| e.file_name().to_str().is_some_and(is_campaign_dir)).map(|e| e.path()).collect();
    out.sort();
    out
}
fn save_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file())).filter(|e| e.file_name().to_str().is_some_and(is_save_file)).map(|e| e.path()).collect();
    out.sort();
    out
}
fn suspend_files(root: &Path) -> Vec<PathBuf> {
    campaign_dirs(root).into_iter().map(|d| d.join(file_name(SUSPEND_STEM))).filter(|p| p.is_file()).collect()
}

/// The suspend slot, if one exists (the newest, should several have survived a crash). A suspend no newer
/// than its campaign's latest full save carries [`stale_suspend`]'s problem, so the menu does not offer it.
pub(crate) fn find_suspend(root: &Path, cache: &mut SlotCache) -> Option<Arc<SlotInfo>> {
    let found = suspend_files(root).into_iter().map(|p| slot_info(root, &p, cache)).max_by_key(|s| s.modified_unix)?;
    match (&found.problem, &found.header) {
        // The campaign's full saves through the slot cache: the menu asks each time it opens.
        (None, Some(h)) => match stale_against(h, save_files(&campaign_dir(root, h.campaign_id)).into_iter().filter(|p| !is_suspend_path(p)).filter_map(|p| slot_info(root, &p, cache).header.as_ref().map(|s| s.sequence)).max()) {
            Some(m) => Some(Arc::new(SlotInfo { problem: Some(m), ..(*found).clone() })),
            None => Some(found),
        },
        _ => Some(found),
    }
}

/// A suspend whose sortie keyframe another build wrote: the host refuses a keyframe of another protocol
/// version (`Sim::from_checkpoint`), so resuming it could only fail. DECISION (r4): refused before the theatre
/// is loaded, so a failed resume never forks the campaign from an old suspend slot.
pub(crate) fn suspend_version_problem(s: &SuspendSave) -> Option<Msg> {
    (s.sortie.keyframe_format != sim_systems::PROTOCOL_VERSION).then(|| Msg::new("campaign.save.other_build"))
}

/// A suspend no newer than the newest full save of its campaign: the campaign was played on after the
/// sortie was suspended (a resume that failed and left the slot behind), so resuming it would rebuild an older
/// theatre and overwrite the newer save at the next autosave.
pub(crate) fn stale_suspend(root: &Path, header: &SaveHeader) -> Option<Msg> {
    stale_against(header, latest_save(root, header.campaign_id).map(|s| s.header.sequence))
}
fn stale_against(header: &SaveHeader, latest_sequence: Option<u64>) -> Option<Msg> {
    (header.sequence <= latest_sequence?).then(|| Msg::new("campaign.save.stale_suspend"))
}

/// A theatre this build cannot play: its act has no act row in the shipped data (a save from a later build,
/// or one an earlier build carried past this build's last act). Open Theatre has no act clock and plays
/// whatever its act. DECISION (r4): refused on load, with the reason, rather than restarted in an act with no
/// story operations, no capstone and a clock that only restarts it.
pub(crate) fn act_problem(theatre: &content::save::TheatreState, acts: &[content::campaign::ActDef]) -> Option<Msg> {
    (!theatre.open_theatre && !acts.iter().any(|a| a.act == theatre.act)).then(|| Msg::new("campaign.save.later_act"))
}

/// The newest full save of one campaign by sequence (the co-op copy check, L843).
pub(crate) fn latest_save(root: &Path, campaign_id: u64) -> Option<SaveGame> {
    save_files(&campaign_dir(root, campaign_id)).into_iter().filter(|p| !is_suspend_path(p)).filter_map(|p| read_save(&p).ok()).max_by_key(|s| s.header.sequence)
}

/// Keep only the newest [`MANUAL_KEEP`] saves the player asked for in a campaign folder.
pub(crate) fn prune_manual(root: &Path, campaign_id: u64) {
    let mut manual: Vec<PathBuf> = save_files(&campaign_dir(root, campaign_id)).into_iter().filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("save-"))).collect();
    // Names carry the zero-padded sequence, so name order is save order.
    manual.sort();
    while manual.len() > MANUAL_KEEP { let p = manual.remove(0); let _ = std::fs::remove_file(p); }
}

fn modified_unix(meta: &std::fs::Metadata) -> i64 {
    meta.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs().min(i64::MAX as u64) as i64)
}

/// Facts about one file, decoded once per (modified time, size).
pub(crate) fn slot_info(root: &Path, path: &Path, cache: &mut SlotCache) -> Arc<SlotInfo> {
    let meta = std::fs::metadata(path).ok();
    let stamp = meta.as_ref().and_then(|m| m.modified().ok());
    let len = meta.as_ref().map_or(0, |m| m.len());
    if let Some((t, l, info)) = cache.entries.get(path) { if *t == stamp && *l == len { return info.clone(); } }
    let rel = path.strip_prefix(root).ok().map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
    let is_suspend = is_suspend_path(path);
    let mut info = SlotInfo { path: path.to_path_buf(), rel, header: None, act: None, hour: 0, players: Vec::new(), modified_unix: meta.as_ref().map_or(0, modified_unix), is_suspend, problem: None };
    let decoded = if is_suspend { read_suspend(path).map(|s| { let v = suspend_version_problem(&s); (s.header, s.theatre, v) }) } else { read_save(path).map(|s| (s.header, s.theatre, None)) };
    match decoded {
        Ok((header, theatre, version)) => {
            info.problem = version.or_else(|| act_problem(&theatre, &content::campaign::load::shipped_campaign().acts));
            info.act = Some(theatre.act);
            info.hour = theatre.hour;
            info.players = theatre.players.iter().map(|p| p.name.clone()).collect();
            info.header = Some(header);
        }
        Err(msg) => info.problem = Some(msg),
    }
    let info = Arc::new(info);
    cache.entries.insert(path.to_path_buf(), (stamp, len, info.clone()));
    info
}

/// Every save on disk, newest first (modified time, then sequence). Cache entries of files that are gone
/// are dropped.
pub(crate) fn list(root: &Path, cache: &mut SlotCache) -> Vec<Arc<SlotInfo>> {
    let files: Vec<PathBuf> = campaign_dirs(root).iter().flat_map(|d| save_files(d)).collect();
    cache.entries.retain(|p, _| files.contains(p));
    let mut out: Vec<Arc<SlotInfo>> = files.iter().map(|p| slot_info(root, p, cache)).collect();
    out.sort_by(|a, b| b.modified_unix.cmp(&a.modified_unix).then_with(|| b.header.as_ref().map_or(0, |h| h.sequence).cmp(&a.header.as_ref().map_or(0, |h| h.sequence))).then_with(|| a.rel.cmp(&b.rel)));
    out
}
