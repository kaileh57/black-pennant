//! Unit `gd-editor-io`. Blueprint files, folders and share strings on `BpEditor`.
//! DESIGN.md L2067-2087 (blueprint format: RON, content hash, limits, unknown fields, newer formats,
//! migrations), L2089-2097 (sharing before Steam: files and share strings, import by paste; above 4,000
//! characters the dialog also offers Save as file), L233 (blueprint folders), L126 (player modes refuse a
//! blueprint over 90 m and show the reason), L1359-1360 (saves under the user data directory, long and
//! non-ASCII paths), L2123 (scripts in shared designs arrive disabled).
//! Formats, hashing, migration and validation are `content`; this unit does the file work through Godot's
//! `FileAccess` and `DirAccess` so long paths work on Windows. Player designs live under
//! `user://blueprints/`, kits under `user://blueprints/kits/`; stock designs are read-only.
//!
//! # Paths
//! Every path a caller passes is confined to `user://blueprints/`: it is either a `user://blueprints/...` path
//! or a path relative to that folder, with forward or back slashes. Segments `.` and `..`, empty segments,
//! drive letters and other schemes are refused, and every name this unit creates goes through
//! [`clean_segment`] (no characters Windows forbids, no reserved device names, no leading or trailing dots
//! and spaces, at most [`MAX_SEGMENT_CHARS`] characters), so a hostile ship name inside a share string cannot
//! leave `Imported/`. Stock files are addressed as `stock:<path under blueprints/>` and are never written.
//!
//! # Choices the spec leaves open
//! - A new or stock design opens clean (as `open` does); a load that migrated the file, removed parts or
//!   changed the design's kind opens dirty, so the change reaches the disk on the next save.
//! - Saving writes a temporary file beside the target and renames it over the target, so a crash mid-write
//!   never leaves half a blueprint. The written copy carries the running game's version and format; when that
//!   differs from the design under edit, the design is updated in place first (no undo step), so the file and
//!   the design under edit always have the same hash.
//! - Lineage grows on a fork: the first save of a design under a path other than the file it came from (a
//!   stock starter, an imported file, a Save as) appends that origin's hash and author. Saving over the same
//!   file does not record the design's own history.
//! - Imported ships land in `Imported/` (or the folder given); imported kits always land in
//!   `user://blueprints/kits/`, where the parts tray reads them. A kit whose id is taken by a stock kit or by a
//!   different player kit is given a free id. An identical kit already on disk is reused.
//! - Renaming or moving the file under edit moves the design's path with it; deleting it leaves the design
//!   open with no path and unsaved. Scorecard history (`analysis.rs`) follows renames, moves and copies.
//! - The stock tree has three folders: `starters`, `classes` (enemy classes by act) and `kits`. Stock ships
//!   are listed shortest first.
use super::BpEditor;
use crate::convert::{d, msg_dict};
use crate::enums::EditorMode;
use content::blueprint::{Blueprint, BlueprintKind, Shareable};
use content::ids::{ContentHash, KitId};
use content::kit::{KitDef, LineageEntry};
use godot::classes::{DirAccess, FileAccess, Os, ProjectSettings, Time};
use godot::prelude::*;
use sim_systems::protocol::{Msg, MsgArg};
use std::collections::{BTreeMap, BTreeSet};

/// The player's blueprint folder.
pub(crate) const ROOT: &str = "user://blueprints";
/// Where imported ships land by default (L2067).
pub(crate) const IMPORTED: &str = "Imported";
pub(crate) const SHIP_EXT: &str = ".bp.ron";
pub(crate) const KIT_EXT: &str = ".kit.ron";
const STOCK: &str = "stock:";
/// Most characters of one file or folder name.
pub(crate) const MAX_SEGMENT_CHARS: usize = 96;
/// Most folder levels below the root.
const MAX_DEPTH: usize = 8;
/// Most entries one `browse` lists.
const MAX_ENTRIES: usize = 2_000;
/// Copies tried for a free file name ("Name 2", "Name 3", ...).
const MAX_COPIES: u32 = 999;
/// Most player kit files scanned for a free kit id.
const MAX_KIT_SCAN: usize = 512;

/// Text keys this unit emits (`game/i18n/dock.csv`).
pub(crate) const IO_KEYS: &[&str] = &[
    "dock.io.not_open", "dock.io.bad_path", "dock.io.exists", "dock.io.write_failed", "dock.io.untitled", "dock.io.migrated",
];

// ---------------------------------------------------------------------------------------------------------
// Pure path rules (tested without the engine).

/// A name the player typed or a share string carried, made safe as one path segment on every platform.
/// Returns `""` when nothing usable is left.
pub(crate) fn clean_segment(name: &str) -> String {
    const BAD: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    let mapped: String = name.chars().filter(|c| !c.is_control()).map(|c| if BAD.contains(&c) { '_' } else { c }).collect();
    let trimmed: String = mapped.trim_matches(|c: char| c == '.' || c.is_whitespace()).chars().take(MAX_SEGMENT_CHARS).collect();
    let trimmed = trimmed.trim_end_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    if trimmed.is_empty() { return String::new(); }
    // Windows device names, alone or before an extension.
    let stem = trimmed.split('.').next().unwrap_or("").trim().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
    if reserved { format!("_{trimmed}") } else { trimmed }
}

/// The segments of a caller's path under the blueprint root, or `None` when it would leave the root or names
/// something unsafe. `""` and the root itself give no segments.
pub(crate) fn user_segments(path: &str) -> Option<Vec<String>> {
    let p = path.trim().replace('\\', "/");
    let rel = if let Some(r) = p.strip_prefix(ROOT) {
        if !(r.is_empty() || r.starts_with('/')) { return None; }
        r.to_string()
    } else if p.contains("://") || p.starts_with(STOCK) || p.starts_with('/') || p.as_bytes().get(1) == Some(&b':') {
        return None;
    } else {
        p
    };
    let mut out = Vec::new();
    for seg in rel.split('/') {
        if seg.is_empty() { continue; }
        if seg == "." || seg == ".." || clean_segment(seg) != seg { return None; }
        out.push(seg.to_string());
    }
    if out.len() > MAX_DEPTH + 1 { return None; }
    Some(out)
}

/// Full `user://blueprints/...` path of some segments.
pub(crate) fn join_user(segs: &[String]) -> String {
    let mut s = ROOT.to_string();
    for seg in segs { s.push('/'); s.push_str(seg); }
    s
}

/// The confined full path of a caller's path, or `None`.
pub(crate) fn user_path(path: &str) -> Option<String> { user_segments(path).map(|s| join_user(&s)) }

/// The folder part of a full path (`user://blueprints` for a file at the root).
pub(crate) fn parent_of(full: &str) -> String { full.rsplit_once('/').map_or(ROOT.to_string(), |(a, _)| if a.len() < ROOT.len() { ROOT.to_string() } else { a.to_string() }) }

/// The last segment of a path.
pub(crate) fn file_name_of(path: &str) -> &str { path.rsplit(['/', '\\']).next().unwrap_or(path) }

/// `(stem, extension)` of a blueprint or kit file name; other names have an empty extension.
pub(crate) fn split_ext(name: &str) -> (&str, &str) {
    for ext in [SHIP_EXT, KIT_EXT] {
        if name.len() > ext.len() && name.is_char_boundary(name.len() - ext.len()) && name[name.len() - ext.len()..].eq_ignore_ascii_case(ext) {
            return (&name[..name.len() - ext.len()], &name[name.len() - ext.len()..]);
        }
    }
    (name, "")
}

/// A file name of a stem and an extension that fits one segment ([`MAX_SEGMENT_CHARS`] with the extension
/// and `room` more characters), or `None` when nothing usable is left of the stem.
pub(crate) fn file_segment(stem: &str, ext: &str, room: usize) -> Option<String> {
    let s = clean_segment(stem);
    let s: String = s.chars().take(MAX_SEGMENT_CHARS.saturating_sub(ext.chars().count() + room)).collect();
    let s = s.trim_end_matches(|c: char| c == '.' || c.is_whitespace());
    (!s.is_empty()).then(|| format!("{s}{ext}"))
}

/// A file name of `stem` and `ext` that `taken` does not hold: `stem.ext`, then `stem 2.ext`, `stem 3.ext`...
/// Every name it gives is a valid segment (it passes [`user_segments`]).
pub(crate) fn free_name(stem: &str, ext: &str, taken: &dyn Fn(&str) -> bool) -> Option<String> {
    let first = file_segment(stem, ext, 0).unwrap_or_else(|| format!("_{ext}"));
    if !taken(&first) { return Some(first); }
    // Leave room for the counter inside the segment limit.
    let short = file_segment(stem, "", ext.chars().count() + 4).unwrap_or_else(|| "_".to_string());
    (2..=MAX_COPIES).map(|n| format!("{short} {n}{ext}")).find(|c| !taken(c))
}

/// The stock path key (`starters/fac22.bp.ron`) of a `stock:` path, if it is one.
pub(crate) fn stock_path_key(path: &str) -> Option<String> {
    let p = path.trim().strip_prefix(STOCK)?.replace('\\', "/");
    let p = p.trim_matches('/');
    if p.split('/').any(|s| s == ".." || s == ".") { return None; }
    Some(p.strip_prefix("blueprints/").unwrap_or(p).to_string())
}

/// Folders of the stock tree as (virtual name, real folder under `blueprints/`).
const STOCK_FOLDERS: [(&str, &str); 3] = [("starters", "starters"), ("classes", "combine"), ("kits", "kits")];

/// What one stock folder holds: sub-folders (virtual paths) and files (real `stock:` keys), sorted.
pub(crate) fn stock_listing(folder: &str, files: &[&str]) -> (Vec<String>, Vec<String>) {
    let folder = folder.trim().trim_matches('/');
    if folder.is_empty() { return (STOCK_FOLDERS.iter().map(|(v, _)| v.to_string()).collect(), Vec::new()); }
    let (head, rest) = folder.split_once('/').unwrap_or((folder, ""));
    let Some((_, real)) = STOCK_FOLDERS.iter().find(|(v, _)| *v == head) else { return (Vec::new(), Vec::new()) };
    let prefix = if rest.is_empty() { format!("{real}/") } else { format!("{real}/{rest}/") };
    let mut dirs = BTreeSet::new();
    let mut out = Vec::new();
    for f in files {
        let Some(tail) = f.strip_prefix(&prefix) else { continue };
        match tail.split_once('/') {
            Some((dir, _)) => { dirs.insert(format!("{folder}/{dir}")); }
            None => out.push(f.to_string()),
        }
    }
    out.sort();
    (dirs.into_iter().collect(), out)
}

/// Turn off every script of a design that arrived from outside (L2123) and refresh its hash. Returns true
/// when any was on.
pub(crate) fn disable_scripts(bp: &mut Blueprint) -> bool {
    let mut changed = false;
    for s in &mut bp.graph.scripts { changed |= std::mem::replace(&mut s.enabled, false); }
    if changed { bp.header.hash = content::blueprint::io::compute_hash(bp); }
    changed
}

/// A blueprint with its hash field emptied, for comparing content.
fn unhashed(bp: &Blueprint) -> Blueprint { let mut b = bp.clone(); b.header.hash = ContentHash::EMPTY; b }

/// The design as it is written: the running game's version and format, the origin appended to the lineage
/// when this save forks it, and the hash filled in.
pub(crate) fn prepared_for_save(bp: &Blueprint, fork_of: Option<&LineageEntry>) -> Blueprint {
    let mut next = bp.clone();
    next.header.format = content::blueprint::BLUEPRINT_FORMAT;
    next.header.game_version = env!("CARGO_PKG_VERSION").to_string();
    if let Some(origin) = fork_of {
        if next.header.lineage.last() != Some(origin) && origin.hash != ContentHash::EMPTY {
            next.header.lineage.push(origin.clone());
            let max = content::kit::MAX_LINEAGE;
            if next.header.lineage.len() > max { let cut = next.header.lineage.len() - max; next.header.lineage.drain(..cut); }
        }
    }
    next.header.hash = content::blueprint::io::compute_hash(&next);
    next
}

/// A skeleton of a blueprint for the browser's info cache: the header and the part list only.
fn skeleton(bp: &Blueprint) -> Blueprint {
    let mut s = bp.clone();
    s.cells = Default::default();
    s.markings.clear();
    s.kits.clear();
    s.embedded_kits.clear();
    s.officers = Default::default();
    s.rules.clear();
    s.graph = Default::default();
    s
}

// ---------------------------------------------------------------------------------------------------------
// Caches.

/// What the browser knows of one file.
#[derive(Clone)]
enum FileInfo {
    Ship { skeleton: Box<Blueprint>, stats: Box<content::stats::BlueprintStats> },
    Kit(Box<KitDef>),
    Broken,
}

/// Per-file and per-revision caches of this unit.
#[derive(Default)]
pub(crate) struct IoCache {
    /// Browser info by path, with the modification time and size it was read at.
    info: std::cell::RefCell<BTreeMap<String, (u64, u64, FileInfo)>>,
    /// The share string of a revision (zstd level 19 is too slow to repeat per call).
    share: std::cell::RefCell<Option<(u64, String)>>,
    /// The file the design under edit came from, for lineage on a fork: (first revision of the design, path,
    /// the origin's hash and author).
    origin: Option<(u64, String, LineageEntry)>,
}

// ---------------------------------------------------------------------------------------------------------
// Engine-side helpers.

fn gs(s: &str) -> GString { GString::from(s) }
fn ok(e: godot::global::Error) -> bool { e == godot::global::Error::OK }

fn io_issue(text: Msg, fatal: bool) -> VarDictionary { d! { "text" => msg_dict(&text), "fatal" => fatal, "part" => -1i64, "cell" => Variant::nil() } }
fn issues_array(list: Vec<VarDictionary>) -> Array<VarDictionary> { let mut a = Array::new(); for d in &list { a.push(d); } a }
fn import_failure(issue: VarDictionary) -> VarDictionary { d! { "ok" => false, "kind" => StringName::default(), "path" => GString::new(), "name" => GString::new(), "issues" => issues_array(vec![issue]) } }

/// Read a text file with the 16 MiB blueprint limit checked before reading (L2081).
pub(crate) fn read_text(path: &str) -> Result<String, content::ContentError> {
    if !FileAccess::file_exists(path) { return Err(content::ContentError::Io(format!("{}: no such file", file_name_of(path)))); }
    let Some(f) = FileAccess::open(path, godot::classes::file_access::ModeFlags::READ) else { return Err(content::ContentError::Io(format!("{}: cannot open", file_name_of(path)))) };
    if f.get_length() as usize > content::blueprint::MAX_RON_BYTES { return Err(content::ContentError::Limit("blueprint over 16 MiB")); }
    let text = f.get_as_text().to_string();
    if text.is_empty() { return Err(content::ContentError::Io(format!("{}: empty or unreadable", file_name_of(path)))); }
    Ok(text)
}

/// Write text to `path` through a temporary file renamed over it. Creates the folder.
pub(crate) fn write_atomic(path: &str, text: &str) -> bool {
    let _ = DirAccess::make_dir_recursive_absolute(parent_of(path).as_str());
    let tmp = format!("{path}.tmp");
    {
        let Some(mut f) = FileAccess::open(tmp.as_str(), godot::classes::file_access::ModeFlags::WRITE) else { return false };
        let stored = f.store_string(text);
        f.flush();
        let err = f.get_error();
        f.close();
        if !stored || !ok(err) {
            let _ = DirAccess::remove_absolute(tmp.as_str());
            return false;
        }
    }
    if ok(DirAccess::rename_absolute(tmp.as_str(), path)) { return true; }
    // A rename over an existing file can fail on some file systems: replace it in two steps.
    if FileAccess::file_exists(path) && ok(DirAccess::remove_absolute(path)) && ok(DirAccess::rename_absolute(tmp.as_str(), path)) { return true; }
    let _ = DirAccess::remove_absolute(tmp.as_str());
    false
}

pub(crate) fn exists(path: &str) -> bool { FileAccess::file_exists(path) || DirAccess::dir_exists_absolute(path) }

/// Seconds since 1970 by the system clock.
pub(crate) fn now_unix() -> i64 { Time::singleton().get_unix_time_from_system() as i64 }

/// Move a file or folder to the OS trash. False when the trash refused (never a hard delete).
pub(crate) fn trash(path: &str) -> bool {
    let global = ProjectSettings::singleton().globalize_path(path);
    ok(Os::singleton().move_to_trash(&global))
}

/// Translated untitled name for a file of a design with no name.
fn untitled() -> String { godot::classes::TranslationServer::singleton().translate("dock.io.untitled").to_string() }

/// Load a ship or a kit from file text, whichever it is (by extension, then by trying both).
fn parse_any(text: &str, file: &str) -> Result<Shareable, content::ContentError> {
    let lower = file.to_ascii_lowercase();
    if content::share::looks_like_share_string(text) { return content::share::decode_share_string(text); }
    if lower.ends_with(KIT_EXT) { return content::kit::parse_kit(text, file).map(|k| Shareable::Kit(Box::new(k))); }
    if lower.ends_with(SHIP_EXT) { return content::blueprint::io::load_blueprint(text, file).map(|l| Shareable::Ship(Box::new(l.blueprint))); }
    match content::blueprint::io::load_blueprint(text, file) {
        Ok(l) => Ok(Shareable::Ship(Box::new(l.blueprint))),
        Err(e) => content::kit::parse_kit(text, file).map(|k| Shareable::Kit(Box::new(k))).map_err(|_| e),
    }
}

/// The ruleset a lobby picker checks a file against: a lobby ruleset id, else the dock's, else the default.
fn ruleset_for(id: &str, dock: Option<&content::ruleset::Ruleset>) -> content::ruleset::Ruleset {
    let id = id.trim();
    if !id.is_empty() {
        if let Some((_, r)) = super::lobby_rulesets().iter().find(|(k, _)| k == id) { return r.clone(); }
        if id == "campaign" { return content::ruleset::Ruleset::campaign(content::enums::Act::Act4); }
    }
    dock.cloned().unwrap_or_default()
}

/// Ids of the player's kit files on disk.
fn player_kit_ids() -> BTreeSet<String> {
    let dir = super::parts::PLAYER_KIT_DIR;
    if !DirAccess::dir_exists_absolute(dir) { return BTreeSet::new(); }
    DirAccess::get_files_at(dir).as_slice().iter().map(|f| f.to_string()).filter(|f| f.to_ascii_lowercase().ends_with(KIT_EXT)).take(MAX_KIT_SCAN)
        .filter_map(|f| read_text(&format!("{dir}/{f}")).ok().and_then(|t| content::kit::parse_kit(&t, &f).ok()).map(|k| k.id.as_str().to_string()))
        .collect()
}

/// A player kit by id from the kit folder (its file is usually named after the id).
fn player_kit(id: &KitId) -> Option<KitDef> {
    let dir = super::parts::PLAYER_KIT_DIR;
    let named = format!("{dir}/{}", file_segment(id.as_str(), KIT_EXT, 0).unwrap_or_default());
    if let Some(k) = read_text(&named).ok().and_then(|t| content::kit::parse_kit(&t, "kit").ok()).filter(|k| &k.id == id) { return Some(k); }
    if !DirAccess::dir_exists_absolute(dir) { return None; }
    DirAccess::get_files_at(dir).as_slice().iter().map(|f| format!("{dir}/{f}")).filter(|p| p.to_ascii_lowercase().ends_with(KIT_EXT)).take(MAX_KIT_SCAN)
        .find_map(|p| read_text(&p).ok().and_then(|t| content::kit::parse_kit(&t, "kit").ok()).filter(|k| &k.id == id))
}

impl BpEditor {
    /// The hull-length refusal and kind fix-up `open` applies, for a design about to be installed. `Err` is
    /// the fatal issue; `Ok` carries the non-fatal notices and whether the kind changed.
    fn admit(&self, bp: &mut Blueprint) -> Result<(Vec<VarDictionary>, bool), VarDictionary> {
        let Some(m) = self.model.as_ref() else { return Err(io_issue(Msg::new("dock.io.not_open"), true)) };
        let max = super::max_length_m(m.context.mode, &m.context.ruleset);
        let length = super::design_length_m(bp);
        if length > max + 1e-9 {
            let mut dict = crate::convert::issue_dict(&content::validate::Issue::TooLong { length_m: length, max_m: max });
            dict.set("fatal", true);
            return Err(dict);
        }
        let kind = if m.context.mode == EditorMode::EnemyClass { BlueprintKind::EnemyClass } else { BlueprintKind::Ship };
        let mut notes = Vec::new();
        let changed = bp.header.kind != kind;
        if changed {
            bp.header.kind = kind;
            bp.header.hash = content::blueprint::io::compute_hash(bp);
            notes.push(io_issue(Msg::new("dock.issue.kind_changed"), false));
        }
        Ok((notes, changed))
    }

    /// Notices for parts a migration removed (one per part, at its cell).
    fn removed_notices(removed: &[content::migrate::RemovedPart]) -> Vec<VarDictionary> {
        let cat = crate::services::services().catalogue();
        removed.iter().map(|r| {
            let name = cat.part(&r.part).map_or(MsgArg::Name(r.part.as_str().to_string()), |d| MsgArg::Key(d.name.clone()));
            let mut dict = io_issue(Msg::new("dock.issue.part_removed").arg(name), false);
            dict.set("cell", Vector3i::new(r.pos.x as i32, r.pos.y as i32, r.pos.z as i32));
            dict
        }).collect()
    }

    /// Remember where the design under edit came from (lineage on a fork).
    fn set_origin(&mut self, path: &str, bp: &Blueprint) {
        let author = bp.header.author.clone().unwrap_or_default();
        let hash = content::blueprint::io::compute_hash(bp);
        self.state.io.origin = Some((self.state.first_rev, path.to_string(), LineageEntry { hash, author }));
    }

    /// The origin of the design under edit, if it is known: what `set_origin` recorded for this design, else
    /// the file at its current path as it is on disk.
    fn origin(&self) -> Option<(String, LineageEntry)> {
        if let Some((_, path, entry)) = self.state.io.origin.as_ref().filter(|(r, _, _)| *r == self.state.first_rev) {
            return Some((path.clone(), entry.clone()));
        }
        let m = self.model.as_ref()?;
        if m.path.is_empty() { return None; }
        let loaded = super::read_blueprint(&m.path).ok()?;
        Some((m.path.clone(), LineageEntry { hash: loaded.blueprint.header.hash, author: loaded.blueprint.header.author.clone().unwrap_or_default() }))
    }

    /// Write the design under edit to a confined full path and mark it saved there.
    pub(crate) fn write_design(&mut self, full: &str) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        let fork = self.origin().filter(|(p, _)| p != full).map(|(_, e)| e);
        let mut current = m.blueprint.clone();
        let folded = self.fold_graph_layout(&mut current);
        let next = prepared_for_save(&current, fork.as_ref());
        if !write_atomic(full, &content::blueprint::io::to_file_text(&next)) {
            godot_warn!("dry dock: could not write {}", file_name_of(full));
            crate::services::services().audio.borrow_mut().ui(crate::seams::sound::UiSfx::Invalid);
            return false;
        }
        crate::services::services().audio.borrow_mut().ui(crate::seams::sound::UiSfx::Saved);
        // Written: the design under edit becomes exactly what is on disk (no undo step); a hash that was only
        // out of date is refreshed without a new revision.
        if let Some(m) = self.model.as_mut() {
            if unhashed(&m.blueprint) != unhashed(&next) { m.replace_in_place(next.clone()); } else { m.blueprint.header.hash = next.header.hash; }
        }
        if folded { self.graph_layout_saved(); }
        self.announce();
        self.state.io.info.borrow_mut().remove(full);
        self.state.io.origin = Some((self.state.first_rev, full.to_string(), LineageEntry { hash: next.header.hash, author: next.header.author.clone().unwrap_or_default() }));
        self.mark_saved(full);
        true
    }

    /// Point the design under edit and the saved design at a file's new path after a rename or move.
    fn follow_path(&mut self, from: &str, to: &str) {
        let moved = |p: &str| -> Option<String> {
            if p == from { return Some(to.to_string()); }
            p.strip_prefix(from).filter(|r| r.starts_with('/')).map(|r| format!("{to}{r}"))
        };
        if let Some(m) = self.model.as_mut() { if let Some(p) = moved(&m.path) { m.path = p; } }
        if let Some(m) = self.state.parked.as_mut() { if let Some(p) = moved(&m.path) { m.path = p; } }
        if let Some((_, p, _)) = self.state.io.origin.as_mut() { if let Some(n) = moved(p) { *p = n; } }
        {
            let services = crate::services::services();
            let mut slot = services.design.borrow_mut();
            if let Some(cur) = slot.as_ref() {
                if let Some(p) = moved(&cur.path) {
                    *slot = Some(std::sync::Arc::new(super::CurrentDesign { blueprint: cur.blueprint.clone(), hash: cur.hash, design: cur.design.clone(), path: p, prepared: cur.prepared.clone(), complexity_points: cur.complexity_points }));
                }
            }
        }
        let keys: Vec<String> = self.state.io.info.borrow().keys().filter(|k| moved(k).is_some()).cloned().collect();
        for k in keys { self.state.io.info.borrow_mut().remove(&k); }
    }

    /// Browser info of a file, cached by modification time and size.
    fn info_of(&self, path: &str) -> Option<(FileInfo, u64)> {
        if let Some(key) = stock_path_key(path) {
            if let Some((_, _, i)) = self.state.io.info.borrow().get(path) { return Some((i.clone(), 0)); }
            let info = if key.ends_with(KIT_EXT) {
                content::data::stock_kit_files().find(|(p, _)| *p == key).and_then(|(p, t)| content::kit::parse_kit(t, p).ok()).map_or(FileInfo::Broken, |k| FileInfo::Kit(Box::new(k)))
            } else {
                match content::campaign::load::stock_blueprint(&key) {
                    Ok(bp) => FileInfo::Ship { stats: Box::new(content::stats::compute_stats(&bp, &crate::services::services().catalogue())), skeleton: Box::new(skeleton(&bp)) },
                    Err(_) => return None,
                }
            };
            self.state.io.info.borrow_mut().insert(path.to_string(), (0, 0, info.clone()));
            return Some((info, 0));
        }
        if !FileAccess::file_exists(path) { return None; }
        let mtime = FileAccess::get_modified_time(path);
        let len = FileAccess::open(path, godot::classes::file_access::ModeFlags::READ).map_or(0, |f| f.get_length());
        if let Some((t, l, i)) = self.state.io.info.borrow().get(path) {
            if *t == mtime && *l == len { return Some((i.clone(), mtime)); }
        }
        let info = match read_text(path).and_then(|t| parse_any(&t, file_name_of(path))) {
            Ok(Shareable::Ship(bp)) => FileInfo::Ship { stats: Box::new(content::stats::compute_stats(&bp, &crate::services::services().catalogue())), skeleton: Box::new(skeleton(&bp)) },
            Ok(Shareable::Kit(k)) => FileInfo::Kit(k),
            Err(_) => FileInfo::Broken,
        };
        let mut cache = self.state.io.info.borrow_mut();
        if cache.len() > 4 * MAX_ENTRIES { cache.clear(); }
        cache.insert(path.to_string(), (mtime, len, info.clone()));
        Some((info, mtime))
    }

    fn info_dict(&self, path: &str) -> Option<VarDictionary> {
        let (info, mtime) = self.info_of(path)?;
        let stock = path.starts_with(STOCK);
        match info {
            FileInfo::Ship { skeleton, stats } => {
                let mut dict = crate::convert::blueprint_info_dict(&skeleton, &stats, path, stock, mtime as i64);
                // A scorecard measured for this exact design carries its complexity.
                if let Some(c) = self.measured_complexity(&skeleton.header.hash) { dict.set("complexity", c); }
                Some(dict)
            }
            FileInfo::Kit(k) => Some(kit_info_dict(&k, path, stock, mtime as i64)),
            FileInfo::Broken => None,
        }
    }

    /// Store an imported item. `folder` is the target folder for ships ("" = `Imported/`).
    fn store_import(&mut self, item: Shareable, folder: &str) -> VarDictionary {
        match item {
            Shareable::Ship(mut bp) => {
                disable_scripts(&mut bp);
                let Some(segs) = user_segments(if folder.trim().is_empty() { IMPORTED } else { folder }) else { return import_failure(io_issue(Msg::new("dock.io.bad_path"), true)) };
                let dir = join_user(&segs);
                let stem = if bp.header.name.trim().is_empty() { untitled() } else { bp.header.name.clone() };
                let Some(name) = free_name(&stem, SHIP_EXT, &|n: &str| exists(&format!("{dir}/{n}"))) else { return import_failure(io_issue(Msg::new("dock.io.exists"), true)) };
                let full = format!("{dir}/{name}");
                if !write_atomic(&full, &content::blueprint::io::to_file_text(&bp)) { return import_failure(io_issue(Msg::new("dock.io.write_failed"), true)); }
                // What the player should know before opening it: problems under the dock's rules.
                let issues = self.file_problems(&bp, None);
                d! { "ok" => true, "kind" => StringName::from("ship"), "path" => gs(&full), "name" => gs(&bp.header.name), "issues" => issues_array(issues) }
            }
            Shareable::Kit(mut kit) => {
                let cat = crate::services::services().catalogue();
                let dir = super::parts::PLAYER_KIT_DIR;
                let file_of = |id: &str| format!("{dir}/{}", file_segment(id, KIT_EXT, 0).unwrap_or_else(|| format!("_{KIT_EXT}")));
                let hash_as = |k: &KitDef, id: &str| { let mut c = k.clone(); c.id = KitId::new(id); content::kit::kit_hash(&c) };
                let mut base = kit.id.as_str().to_string();
                if clean_segment(&base).is_empty() { base = super::kit_id_from_name(&kit.name); }
                // Reuse an identical kit; otherwise a free id that no stock kit and no other player kit holds.
                let taken = player_kit_ids();
                let mut id = base.clone();
                let mut n = 2;
                loop {
                    let stock = cat.kit(&KitId::new(id.clone())).is_some();
                    if !stock {
                        if let Some(existing) = player_kit(&KitId::new(id.clone())) {
                            if content::kit::kit_hash(&existing) == hash_as(&kit, &id) {
                                self.state.parts.forget_player_kits();
                                let path = file_of(&id);
                                return d! { "ok" => true, "kind" => StringName::from("kit"), "path" => gs(&path), "name" => gs(&existing.name), "issues" => issues_array(Vec::new()) };
                            }
                        } else if !taken.contains(&id) && !exists(&file_of(&id)) {
                            break;
                        }
                    }
                    if n > MAX_COPIES { return import_failure(io_issue(Msg::new("dock.io.exists"), true)); }
                    id = format!("{base}_{n}");
                    n += 1;
                }
                if kit.id.as_str() != id { kit.id = KitId::new(id.clone()); }
                kit.hash = content::kit::kit_hash(&kit);
                let path = file_of(&id);
                if !write_atomic(&path, &content::kit::kit_to_file_text(&kit)) { return import_failure(io_issue(Msg::new("dock.io.write_failed"), true)); }
                self.state.parts.forget_player_kits();
                let ruleset = self.model.as_ref().map(|m| m.context.ruleset.clone()).unwrap_or_else(|| content::ruleset::Ruleset::campaign(content::enums::Act::Act4));
                let issues: Vec<VarDictionary> = content::validate::validate_kit(&kit, &cat, &ruleset).iter().map(crate::convert::issue_dict).collect();
                d! { "ok" => true, "kind" => StringName::from("kit"), "path" => gs(&path), "name" => gs(&kit.name), "issues" => issues_array(issues) }
            }
        }
    }

    /// Validation of a design that is not under edit, against the dock's rules (or `ruleset`), as non-fatal
    /// issues. The length limit of the dock's mode is checked too (L126).
    fn file_problems(&self, bp: &Blueprint, ruleset: Option<&content::ruleset::Ruleset>) -> Vec<VarDictionary> {
        let cat = crate::services::services().catalogue();
        let dock = self.model.as_ref().map(|m| &m.context);
        let r = ruleset.cloned().or_else(|| dock.map(|c| c.ruleset.clone())).unwrap_or_default();
        let available = if ruleset.is_some() { None } else { dock.and_then(|c| c.available.as_deref()).map(|v| v.as_slice()) };
        let enemy_class = bp.header.kind == BlueprintKind::EnemyClass;
        let complexity = self.measured_complexity(&content::blueprint::io::compute_hash(bp));
        let ctx = content::validate::ValidationContext { ruleset: &r, available, enemy_class, complexity };
        let issues = content::validate::validate_blueprint(bp, &cat, &ctx).issues;
        let mut out: Vec<VarDictionary> = issues.iter().map(crate::convert::issue_dict).collect();
        if let (None, Some(c)) = (ruleset, dock) {
            let max = super::max_length_m(c.mode, &c.ruleset);
            let length = super::design_length_m(bp);
            if length > max + 1e-9 && !issues.iter().any(|i| matches!(i, content::validate::Issue::TooLong { .. })) {
                out.push(crate::convert::issue_dict(&content::validate::Issue::TooLong { length_m: length, max_m: max }));
            }
        }
        out
    }

    /// The design file under edit, if it has one.
    fn current_path(&self) -> Option<&str> { self.model.as_ref().map(|m| m.path.as_str()).filter(|p| !p.is_empty()) }

    /// The share string of the design under edit, cached per revision.
    fn share_text(&self) -> String {
        let Some(m) = self.model.as_ref() else { return String::new() };
        if let Some((r, s)) = self.state.io.share.borrow().as_ref() { if *r == m.rev && !self.graph_layout_pending() { return s.clone(); } }
        let mut bp = m.blueprint.clone();
        self.fold_graph_layout(&mut bp);
        bp.header.hash = content::blueprint::io::compute_hash(&bp);
        let s = content::share::encode_share_string(&Shareable::Ship(Box::new(bp)));
        *self.state.io.share.borrow_mut() = Some((m.rev, s.clone()));
        s
    }
}

/// `BlueprintInfoDict` of a kit file (`kind` `&"kit"`). Kits have no hull: length, beam and displacement are
/// their footprint's extent and their parts' mass.
fn kit_info_dict(kit: &KitDef, path: &str, stock: bool, modified_unix: i64) -> VarDictionary {
    let cat = crate::services::services().catalogue();
    let summary = crate::content_api::model::kit_summary(kit, &cat);
    let lineage: VarArray = kit.lineage.iter().map(|l| d! { "hash" => crate::convert::hash_hex(&l.hash), "author" => gs(&l.author) }.to_variant()).collect();
    let name = if kit.name.is_empty() { kit.name_key.as_ref().map(|k| godot::classes::TranslationServer::singleton().translate(k.as_str()).to_string()).unwrap_or_default() } else { kit.name.clone() };
    d! {
        "path" => gs(path),
        "stock" => stock,
        "name" => gs(&name),
        "author" => gs(kit.author.as_deref().unwrap_or("")),
        "hash" => crate::convert::hash_hex(&content::kit::kit_hash(kit)),
        "format" => kit.format as i64,
        "game_version" => gs(env!("CARGO_PKG_VERSION")),
        "kind" => StringName::from("kit"),
        "length_m" => summary.size[0] as f64 * content::cell::CELL_M,
        "beam_m" => summary.size[2] as f64 * content::cell::CELL_M,
        "displacement_t" => summary.mass_kg / 1_000.0,
        "cost" => summary.cost as i64,
        "complexity" => -1.0,
        "parts" => kit.parts.len() as i64,
        "modified_unix" => modified_unix,
        "lineage" => lineage,
        "needs_migration" => kit.format < content::kit::KIT_FORMAT,
        "newer_format" => kit.format > content::kit::KIT_FORMAT,
    }
}

#[godot_api(secondary)]
impl BpEditor {
    /// Start a new design: a copy of a stock starter (`starter` = stock path) or, with `starter` empty, a
    /// default shell of `length_m`.
    #[func]
    fn new_design(&mut self, starter: GString, length_m: f64) -> Array<VarDictionary> {
        let Some(m) = self.model.as_ref() else { return issues_array(vec![io_issue(Msg::new("dock.io.not_open"), true)]) };
        let starter = starter.to_string();
        let (mut bp, origin) = if starter.trim().is_empty() {
            let max = super::max_length_m(m.context.mode, &m.context.ruleset);
            let len = if length_m.is_finite() && length_m > 0.0 { length_m } else { 40.0 };
            (content::blueprint::io::new_blueprint("", len.clamp(content::cell::MIN_PLAYER_HULL_M, max)), None)
        } else {
            let Some(key) = stock_path_key(&starter).or_else(|| super::stock_key(&starter)) else { return issues_array(vec![io_issue(Msg::new("dock.io.bad_path"), true)]) };
            match content::campaign::load::stock_blueprint(&key) {
                Ok(b) => (b, Some(format!("{STOCK}{key}"))),
                Err(e) => return issues_array(vec![crate::convert::error_issue_dict(&e)]),
            }
        };
        let notes = match self.admit(&mut bp) { Ok((n, _)) => n, Err(fatal) => return issues_array(vec![fatal]) };
        let origin_bp = bp.clone();
        if !self.install_design(bp, String::new(), Vec::new(), true) { return issues_array(vec![io_issue(Msg::new("dock.io.not_open"), true)]); }
        match origin {
            Some(p) => self.set_origin(&p, &origin_bp),
            None => self.state.io.origin = None,
        }
        issues_array(notes)
    }
    /// Load a design into the dock. Returns `Array[IssueDict]`: empty = loaded cleanly; non-fatal issues
    /// (migrated parts, clamped paint) still load; a fatal issue leaves the dock unchanged.
    #[func]
    fn load_design(&mut self, path: GString) -> Array<VarDictionary> {
        if self.model.is_none() { return issues_array(vec![io_issue(Msg::new("dock.io.not_open"), true)]); }
        let path = path.to_string();
        let full = if let Some(key) = stock_path_key(&path) {
            format!("{STOCK}{key}")
        } else if let Some(p) = user_path(&path) {
            p
        } else if path.trim().starts_with("res://") && !path.contains("..") {
            path.trim().to_string()
        } else {
            return issues_array(vec![io_issue(Msg::new("dock.io.bad_path"), true)]);
        };
        let loaded = match super::read_blueprint(&full) { Ok(l) => l, Err(e) => return issues_array(vec![crate::convert::error_issue_dict(&e)]) };
        let mut bp = loaded.blueprint;
        let (mut notes, kind_changed) = match self.admit(&mut bp) { Ok(x) => x, Err(fatal) => return issues_array(vec![fatal]) };
        let migrated = loaded.from_format < content::blueprint::BLUEPRINT_FORMAT;
        if migrated { notes.push(io_issue(Msg::new("dock.io.migrated").arg(MsgArg::Int(loaded.from_format as i64)).arg(MsgArg::Int(content::blueprint::BLUEPRINT_FORMAT as i64)), false)); }
        notes.extend(Self::removed_notices(&loaded.removed_parts));
        let editable = full.starts_with(ROOT);
        let clean = !(kind_changed || migrated || !loaded.removed_parts.is_empty());
        let origin_bp = bp.clone();
        if !self.install_design(bp, if editable { full.clone() } else { String::new() }, loaded.removed_parts, clean) { return issues_array(vec![io_issue(Msg::new("dock.io.not_open"), true)]); }
        self.set_origin(&full, &origin_bp);
        issues_array(notes)
    }
    /// Save to the current path (asks for `save_design_as` first when there is none). Saving also caches the
    /// built design and starts the trial battery. Returns false on a write error.
    #[func]
    fn save_design(&mut self) -> bool {
        let Some(path) = self.current_path().map(str::to_string) else { return false };
        let Some(full) = user_path(&path) else { return false };
        self.write_design(&full)
    }
    #[func]
    fn save_design_as(&mut self, path: GString) -> bool {
        if self.model.is_none() { return false; }
        let Some(mut segs) = user_segments(&path.to_string()) else { return false };
        let Some(last) = segs.pop() else { return false };
        let (stem, ext) = split_ext(&last);
        if ext.eq_ignore_ascii_case(KIT_EXT) { return false; }
        let Some(name) = file_segment(stem, SHIP_EXT, 0) else { return false };
        segs.push(name);
        let full = join_user(&segs);
        if DirAccess::dir_exists_absolute(full.as_str()) { return false; }
        self.write_design(&full)
    }
    /// Current file path, or `""` for an unsaved design.
    #[func]
    fn design_path(&self) -> GString { self.model.as_ref().map_or_else(GString::new, |m| gs(&m.path)) }
    #[func]
    fn is_dirty(&self) -> bool { self.model.as_ref().is_some_and(|m| m.is_dirty()) }
    /// List a folder: `folder` is relative to `user://blueprints/` (`""` = root), or starts with `stock:` for
    /// the stock tree (`stock:starters`, `stock:classes`). Returns `Array[FolderEntryDict]`, folders first.
    #[func]
    fn browse(&self, folder: GString) -> Array<VarDictionary> {
        let folder = folder.to_string();
        let mut out = Array::new();
        if let Some(rest) = folder.trim().strip_prefix(STOCK) {
            let files: Vec<&str> = content::data::STOCK_FILES.iter().map(|(p, _)| *p).collect();
            let (dirs, items) = stock_listing(rest, &files);
            for dir in dirs {
                let name = dir.rsplit('/').next().unwrap_or(&dir).to_string();
                out.push(&d! { "name" => gs(&name), "path" => gs(&format!("{STOCK}{dir}")), "is_folder" => true, "info" => Variant::nil() });
            }
            let mut entries: Vec<(f64, String, VarDictionary)> = Vec::new();
            for key in items {
                let path = format!("{STOCK}{key}");
                let info = self.info_dict(&path);
                let length = info.as_ref().and_then(|i| crate::convert::get_f64(i, "length_m")).unwrap_or(0.0);
                let name = info.as_ref().and_then(|i| crate::convert::get_text(i, "name")).filter(|n| !n.is_empty()).unwrap_or_else(|| split_ext(file_name_of(&key)).0.to_string());
                let entry = d! { "name" => gs(&name), "path" => gs(&path), "is_folder" => false, "info" => info.map_or(Variant::nil(), |i| i.to_variant()) };
                entries.push((length, name.to_lowercase(), entry));
            }
            entries.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            for (_, _, e) in entries { out.push(&e); }
            return out;
        }
        let Some(segs) = user_segments(&folder) else { return out };
        let dir = join_user(&segs);
        if !DirAccess::dir_exists_absolute(dir.as_str()) { return out; }
        let mut dirs: Vec<String> = DirAccess::get_directories_at(dir.as_str()).as_slice().iter().map(|s| s.to_string()).filter(|s| !s.starts_with('.')).collect();
        dirs.sort_by_key(|s| s.to_lowercase());
        let mut files: Vec<String> = DirAccess::get_files_at(dir.as_str()).as_slice().iter().map(|s| s.to_string()).filter(|s| !s.starts_with('.') && !split_ext(s).1.is_empty()).collect();
        files.sort_by_key(|s| s.to_lowercase());
        for name in dirs.into_iter().take(MAX_ENTRIES) {
            out.push(&d! { "name" => gs(&name), "path" => gs(&format!("{dir}/{name}")), "is_folder" => true, "info" => Variant::nil() });
        }
        for name in files.into_iter().take(MAX_ENTRIES) {
            let path = format!("{dir}/{name}");
            let info = self.info_dict(&path);
            let shown = split_ext(&name).0.to_string();
            out.push(&d! { "name" => gs(&shown), "path" => gs(&path), "is_folder" => false, "info" => info.map_or(Variant::nil(), |i| i.to_variant()) });
        }
        out
    }
    #[func]
    fn make_folder(&mut self, folder: GString) -> bool {
        let Some(segs) = user_segments(&folder.to_string()) else { return false };
        if segs.is_empty() || segs.len() > MAX_DEPTH { return false; }
        let full = join_user(&segs);
        if FileAccess::file_exists(full.as_str()) { return false; }
        DirAccess::dir_exists_absolute(full.as_str()) || ok(DirAccess::make_dir_recursive_absolute(full.as_str()))
    }
    #[func]
    fn rename_file(&mut self, path: GString, new_name: GString) -> bool {
        let Some(segs) = user_segments(&path.to_string()) else { return false };
        if segs.is_empty() { return false; }
        let from = join_user(&segs);
        if !exists(&from) { return false; }
        let is_dir = DirAccess::dir_exists_absolute(from.as_str());
        if is_dir && segs.len() == 1 && segs[0] == "kits" { return false; }
        let old_name = segs.last().cloned().unwrap_or_default();
        let (_, ext) = split_ext(&old_name);
        let typed = new_name.to_string();
        let (typed_stem, typed_ext) = split_ext(typed.trim());
        let typed_stem = if !is_dir && (typed_ext.eq_ignore_ascii_case(ext) || typed_ext.is_empty()) { typed_stem } else { typed.trim() };
        let Some(name) = file_segment(typed_stem, if is_dir { "" } else { ext }, 0) else { return false };
        let to = format!("{}/{name}", parent_of(&from));
        if to == from { return true; }
        // A change of case only is a rename on case-insensitive file systems too.
        if exists(&to) && to.to_lowercase() != from.to_lowercase() { return false; }
        if !ok(DirAccess::rename_absolute(from.as_str(), to.as_str())) { return false; }
        if is_dir { super::analysis::history_folder_moved(&from, &to); } else { super::analysis::history_moved(&from, &to); }
        self.follow_path(&from, &to);
        true
    }
    #[func]
    fn move_file(&mut self, path: GString, folder: GString) -> bool {
        let Some(segs) = user_segments(&path.to_string()) else { return false };
        let Some(dest) = user_segments(&folder.to_string()) else { return false };
        if segs.is_empty() || dest.len() > MAX_DEPTH { return false; }
        let from = join_user(&segs);
        if !exists(&from) { return false; }
        let is_dir = DirAccess::dir_exists_absolute(from.as_str());
        // A folder never moves into itself, and the kit folder stays where the tray reads it.
        if is_dir && (dest.len() >= segs.len() && dest[..segs.len()] == segs[..] || segs.len() == 1 && segs[0] == "kits") { return false; }
        let dir = join_user(&dest);
        let to = format!("{dir}/{}", segs.last().cloned().unwrap_or_default());
        if to == from { return true; }
        if exists(&to) { return false; }
        if !DirAccess::dir_exists_absolute(dir.as_str()) && !ok(DirAccess::make_dir_recursive_absolute(dir.as_str())) { return false; }
        if !ok(DirAccess::rename_absolute(from.as_str(), to.as_str())) { return false; }
        if is_dir { super::analysis::history_folder_moved(&from, &to); } else { super::analysis::history_moved(&from, &to); }
        if from.starts_with(super::parts::PLAYER_KIT_DIR) || to.starts_with(super::parts::PLAYER_KIT_DIR) { self.state.parts.forget_player_kits(); }
        self.follow_path(&from, &to);
        true
    }
    #[func]
    fn duplicate_file(&mut self, path: GString) -> GString {
        let path = path.to_string();
        // A stock file is copied into the player's folders: the way to fork a stock ship or kit.
        if let Some(key) = stock_path_key(&path) {
            if key.ends_with(KIT_EXT) {
                let Some(kit) = content::data::stock_kit_files().find(|(p, _)| *p == key).and_then(|(p, t)| content::kit::parse_kit(t, p).ok()) else { return GString::new() };
                let mut kit = kit;
                kit.id = KitId::new(super::kit_id_from_name(&kit.name));
                let item = self.store_import(Shareable::Kit(Box::new(kit)), "");
                return crate::convert::get_text(&item, "path").filter(|_| crate::convert::get_bool(&item, "ok") == Some(true)).map_or_else(GString::new, |p| gs(&p));
            }
            let Ok(bp) = content::campaign::load::stock_blueprint(&key) else { return GString::new() };
            let name = if bp.header.name.trim().is_empty() { split_ext(file_name_of(&key)).0.to_string() } else { bp.header.name.clone() };
            let Some(file) = free_name(&name, SHIP_EXT, &|n: &str| exists(&format!("{ROOT}/{n}"))) else { return GString::new() };
            let full = format!("{ROOT}/{file}");
            return if write_atomic(&full, &content::blueprint::io::to_file_text(&bp)) { gs(&full) } else { GString::new() };
        }
        let Some(segs) = user_segments(&path) else { return GString::new() };
        if segs.is_empty() { return GString::new(); }
        let from = join_user(&segs);
        if !FileAccess::file_exists(from.as_str()) { return GString::new(); }
        let dir = parent_of(&from);
        let (stem, ext) = split_ext(segs.last().map(String::as_str).unwrap_or(""));
        if ext.is_empty() { return GString::new(); }
        let Some(file) = free_name(stem, ext, &|n: &str| exists(&format!("{dir}/{n}"))) else { return GString::new() };
        let to = format!("{dir}/{file}");
        if ext.eq_ignore_ascii_case(KIT_EXT) {
            // Two kit files with one id would shadow each other in the tray: the copy gets its own id.
            let Ok(mut kit) = read_text(&from).and_then(|t| content::kit::parse_kit(&t, &file)) else { return GString::new() };
            let taken = player_kit_ids();
            let cat = crate::services::services().catalogue();
            let base = kit.id.as_str().to_string();
            let Some(id) = (2..=MAX_COPIES).map(|n| format!("{base}_{n}")).find(|i| !taken.contains(i) && cat.kit(&KitId::new(i.clone())).is_none()) else { return GString::new() };
            kit.id = KitId::new(id);
            kit.hash = content::kit::kit_hash(&kit);
            if !write_atomic(&to, &content::kit::kit_to_file_text(&kit)) { return GString::new(); }
            self.state.parts.forget_player_kits();
        } else {
            if !ok(DirAccess::copy_absolute(from.as_str(), to.as_str())) { return GString::new(); }
            super::analysis::history_copied(&from, &to);
        }
        gs(&to)
    }
    /// Move a file or an empty folder to the OS trash.
    #[func]
    fn delete_file(&mut self, path: GString) -> bool {
        let Some(segs) = user_segments(&path.to_string()) else { return false };
        if segs.is_empty() { return false; }
        let full = join_user(&segs);
        let is_dir = DirAccess::dir_exists_absolute(full.as_str());
        if is_dir {
            if segs.len() == 1 && segs[0] == "kits" { return false; }
            let empty = DirAccess::get_files_at(full.as_str()).is_empty() && DirAccess::get_directories_at(full.as_str()).is_empty();
            if !empty { return false; }
        } else if !FileAccess::file_exists(full.as_str()) {
            return false;
        }
        if !trash(&full) { return false; }
        if !is_dir {
            super::analysis::history_deleted(&full);
            if full.starts_with(super::parts::PLAYER_KIT_DIR) { self.state.parts.forget_player_kits(); }
        }
        self.state.io.info.borrow_mut().remove(&full);
        // The design under edit loses its file: it stays open, unsaved, with no path.
        let mut emit = false;
        if let Some(m) = self.model.as_mut().filter(|m| m.path == full) {
            m.path = String::new();
            m.saved_rev = m.rev.saturating_sub(1);
            emit = true;
        }
        if let Some(m) = self.state.parked.as_mut().filter(|m| m.path == full) {
            m.path = String::new();
            m.saved_rev = m.rev.saturating_sub(1);
        }
        {
            let services = crate::services::services();
            let mut slot = services.design.borrow_mut();
            if let Some(cur) = slot.as_ref().filter(|c| c.path == full) {
                *slot = Some(std::sync::Arc::new(super::CurrentDesign { blueprint: cur.blueprint.clone(), hash: cur.hash, design: cur.design.clone(), path: String::new(), prepared: cur.prepared.clone(), complexity_points: cur.complexity_points }));
            }
        }
        if emit { self.base_mut().emit_signal("undo_changed", &[]); }
        true
    }
    /// `BlueprintInfoDict` of a file without loading it into the dock, or an empty Dictionary.
    #[func]
    fn file_info(&self, path: GString) -> VarDictionary {
        let path = path.to_string();
        let full = if let Some(k) = stock_path_key(&path) { format!("{STOCK}{k}") } else if let Some(p) = user_path(&path) { p } else { return VarDictionary::new() };
        self.info_dict(&full).unwrap_or_default()
    }
    /// Check a file against a ruleset without loading it (lobby pickers): `Array[IssueDict]`.
    #[func]
    fn file_issues(&self, path: GString, ruleset: GString) -> Array<VarDictionary> {
        let path = path.to_string();
        let full = if let Some(k) = stock_path_key(&path) { format!("{STOCK}{k}") } else if let Some(p) = user_path(&path) { p } else { return issues_array(vec![io_issue(Msg::new("dock.io.bad_path"), true)]) };
        let loaded = match super::read_blueprint(&full) { Ok(l) => l, Err(e) => return issues_array(vec![crate::convert::error_issue_dict(&e)]) };
        let r = ruleset_for(&ruleset.to_string(), self.model.as_ref().map(|m| &m.context.ruleset));
        let mut out = Self::removed_notices(&loaded.removed_parts);
        out.extend(self.file_problems(&loaded.blueprint, Some(&r)));
        issues_array(out)
    }

    // ---- Share strings (L2093) ----

    /// The design under edit as a share string (`BPN1:` then base64url of the compressed canonical RON).
    #[func]
    fn share_string(&self) -> GString { gs(&self.share_text()) }
    /// True when the string is long enough (over 4,000 characters) that the dialog also offers a file.
    #[func]
    fn share_string_is_long(&self) -> bool { self.share_text().chars().count() > content::share::LONG_SHARE_CHARS }
    /// A kit as a share string, by kit id.
    #[func]
    fn kit_share_string(&self, kit: StringName) -> GString {
        let id = KitId::new(kit.to_string());
        let cat = crate::services::services().catalogue();
        let def = self.model.as_ref().and_then(|m| m.blueprint.embedded_kits.get(&id).cloned()).or_else(|| player_kit(&id)).or_else(|| cat.kit(&id).cloned());
        match def { Some(k) => gs(&content::share::encode_share_string(&Shareable::Kit(Box::new(k)))), None => GString::new() }
    }
    /// True when the text looks like a share string (paste handling).
    #[func]
    fn is_share_string(&self, text: GString) -> bool { content::share::looks_like_share_string(&text.to_string()) }
    /// Decode a pasted share string and store it as a file in `folder`. Returns `{ "ok": bool,
    /// "kind": StringName (&"ship" or &"kit"), "path": String, "name": String, "issues": Array[IssueDict] }`.
    #[func]
    fn import_share_string(&mut self, text: GString, folder: GString) -> VarDictionary {
        let cat = crate::services::services().catalogue();
        match content::share::decode_share_string_with(&text.to_string(), &cat) {
            Ok(item) => self.store_import(item, &folder.to_string()),
            Err(e) => import_failure(crate::convert::error_issue_dict(&e)),
        }
    }
    /// Import a blueprint or kit file from anywhere on disk (absolute OS path). Same result shape.
    #[func]
    fn import_file(&mut self, os_path: GString, folder: GString) -> VarDictionary {
        let os_path = os_path.to_string();
        let os_path = os_path.trim();
        let foreign_scheme = os_path.contains("://") && !os_path.starts_with("user://");
        if os_path.is_empty() || foreign_scheme { return import_failure(io_issue(Msg::new("dock.io.bad_path"), true)); }
        let text = match read_text(os_path) { Ok(t) => t, Err(e) => return import_failure(crate::convert::error_issue_dict(&e)) };
        match parse_any(&text, file_name_of(os_path)) {
            Ok(item) => self.store_import(item, &folder.to_string()),
            Err(e) => import_failure(crate::convert::error_issue_dict(&e)),
        }
    }
    /// Write the design under edit to an absolute OS path chosen in a native file dialog.
    #[func]
    fn export_file(&self, os_path: GString) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        let mut path = os_path.to_string().trim().to_string();
        if path.is_empty() || (path.contains("://") && !path.starts_with("user://")) { return false; }
        let name = file_name_of(&path).to_string();
        if clean_segment(split_ext(&name).0).is_empty() { return false; }
        if split_ext(&name).1.is_empty() {
            if name.to_ascii_lowercase().ends_with(".ron") { path.truncate(path.len() - 4); }
            path.push_str(SHIP_EXT);
        }
        let mut bp = m.blueprint.clone();
        self.fold_graph_layout(&mut bp);
        let bp = prepared_for_save(&bp, None);
        write_atomic(&path, &content::blueprint::io::to_file_text(&bp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe_on_every_platform() {
        assert_eq!(clean_segment("  My ship  "), "My ship");
        assert_eq!(clean_segment("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(clean_segment("a<b>c:d\"e|f?g*h"), "a_b_c_d_e_f_g_h");
        assert_eq!(clean_segment("CON"), "_CON");
        assert_eq!(clean_segment("com1.bp.ron"), "_com1.bp.ron");
        assert_eq!(clean_segment("COMPASS"), "COMPASS");
        assert_eq!(clean_segment("..."), "");
        assert_eq!(clean_segment("name.  "), "name");
        assert_eq!(clean_segment("Kōbe 丸"), "Kōbe 丸");
        assert_eq!(clean_segment(&"x".repeat(500)).chars().count(), MAX_SEGMENT_CHARS);
        assert_eq!(clean_segment("a\u{0}b\nc"), "abc");
    }

    #[test]
    fn paths_stay_inside_the_blueprint_folder() {
        assert_eq!(user_path("").as_deref(), Some("user://blueprints"));
        assert_eq!(user_path("user://blueprints").as_deref(), Some("user://blueprints"));
        assert_eq!(user_path("Fleet/a.bp.ron").as_deref(), Some("user://blueprints/Fleet/a.bp.ron"));
        assert_eq!(user_path("Fleet\\a.bp.ron").as_deref(), Some("user://blueprints/Fleet/a.bp.ron"));
        assert_eq!(user_path("user://blueprints/Fleet//a.bp.ron").as_deref(), Some("user://blueprints/Fleet/a.bp.ron"));
        for bad in ["../x.bp.ron", "Fleet/../../x", "user://blueprints/../settings.cfg", "user://blueprintsX/a", "res://a.bp.ron", "/etc/passwd", "C:/x", "C:\\x", "stock:starters/fac22.bp.ron", "a/./b", "bad:name", "CON/x.bp.ron"] {
            assert!(user_path(bad).is_none(), "{bad}");
        }
        assert_eq!(parent_of("user://blueprints/a.bp.ron"), "user://blueprints");
        assert_eq!(parent_of("user://blueprints/F/a.bp.ron"), "user://blueprints/F");
        assert_eq!(split_ext("Ship.bp.ron"), ("Ship", ".bp.ron"));
        assert_eq!(split_ext("Mount.KIT.RON"), ("Mount", ".KIT.RON"));
        assert_eq!(split_ext("notes.txt"), ("notes.txt", ""));
        assert_eq!(split_ext("丸.bp.ron"), ("丸", ".bp.ron"));
        assert_eq!(stock_path_key("stock:starters/fac22.bp.ron").as_deref(), Some("starters/fac22.bp.ron"));
        assert!(stock_path_key("stock:../x").is_none());
    }

    #[test]
    fn free_names_count_up() {
        let taken = |n: &str| n == "Ship.bp.ron" || n == "Ship 2.bp.ron";
        assert_eq!(free_name("Ship", SHIP_EXT, &taken).as_deref(), Some("Ship 3.bp.ron"));
        assert_eq!(free_name("New", SHIP_EXT, &taken).as_deref(), Some("New.bp.ron"));
        assert_eq!(free_name("../evil", SHIP_EXT, &|_| false).as_deref(), Some("_evil.bp.ron"));
        assert_eq!(free_name("", SHIP_EXT, &|_| false).as_deref(), Some("_.bp.ron"));
        assert!(free_name("x", SHIP_EXT, &|_| true).is_none());
        let long = "y".repeat(200);
        let n = free_name(&long, SHIP_EXT, &|n: &str| !n.contains(' ')).unwrap();
        assert!(n.chars().count() <= MAX_SEGMENT_CHARS);
        // Every name the unit makes is one it accepts back.
        for stem in [long.as_str(), "  ..dots..  ", "CON", "丸".repeat(150).as_str(), "a/b\\c", "x. . ."] {
            for taken in [false, true] {
                let name = free_name(stem, SHIP_EXT, &|n: &str| taken && !n.contains(" 2")).unwrap();
                assert_eq!(user_segments(&name).map(|v| v.len()), Some(1), "{name}");
            }
            if let Some(name) = file_segment(stem, KIT_EXT, 0) { assert_eq!(user_segments(&name).map(|v| v.len()), Some(1), "{name}"); }
        }
    }

    #[test]
    fn the_stock_tree_lists_its_folders() {
        let files: Vec<&str> = content::data::STOCK_FILES.iter().map(|(p, _)| *p).collect();
        let (dirs, items) = stock_listing("", &files);
        assert_eq!(dirs, vec!["starters", "classes", "kits"]);
        assert!(items.is_empty());
        let (dirs, items) = stock_listing("starters", &files);
        assert!(dirs.is_empty());
        assert_eq!(items.len(), 6, "six starter hulls (L142)");
        assert!(items.iter().all(|i| content::campaign::load::stock_blueprint(i).is_ok()));
        let (dirs, _) = stock_listing("classes", &files);
        assert!(dirs.iter().any(|d| d == "classes/act1"), "{dirs:?}");
        let (_, act1) = stock_listing("classes/act1", &files);
        assert!(!act1.is_empty() && act1.iter().all(|f| f.starts_with("combine/act1/")));
        let (_, kits) = stock_listing("kits", &files);
        assert!(kits.iter().all(|k| k.ends_with(KIT_EXT)) && !kits.is_empty());
        assert_eq!(stock_listing("tutorial", &files), (Vec::new(), Vec::new()));
    }

    #[test]
    fn a_fork_records_its_origin_once() {
        let bp = content::campaign::load::stock_blueprint("starters/fac22.bp.ron").unwrap();
        let origin = LineageEntry { hash: content::blueprint::io::compute_hash(&prepared_for_save(&bp, None)), author: "Stock".into() };
        // A plain copy is a fork too: it descends from its origin.
        let same = prepared_for_save(&bp, Some(&origin));
        assert_eq!(same.header.lineage.last(), Some(&origin));
        assert_eq!(same.header.hash, content::blueprint::io::compute_hash(&same));
        assert_eq!(prepared_for_save(&bp, None).header.lineage, bp.header.lineage, "a save in place adds nothing");
        let mut edited = bp.clone();
        edited.header.name = "Mine".into();
        let saved = prepared_for_save(&edited, Some(&origin));
        assert_eq!(saved.header.lineage.last(), Some(&origin));
        let again = prepared_for_save(&saved, Some(&origin));
        assert_eq!(again.header.lineage.len(), saved.header.lineage.len(), "no duplicate entry");
        assert_eq!(saved.header.game_version, env!("CARGO_PKG_VERSION"));
        // The written text loads back to the same design and hash.
        let text = content::blueprint::io::to_file_text(&saved);
        let loaded = content::blueprint::io::load_blueprint(&text, "x.bp.ron").unwrap();
        assert_eq!(loaded.blueprint.header.hash, saved.header.hash);
        assert!(!loaded.rehashed);
    }

    #[test]
    fn imported_scripts_arrive_disabled() {
        let mut bp = content::campaign::load::stock_blueprint("starters/patrol15.bp.ron").unwrap();
        bp.graph.scripts.push(content::graph::ScriptSource { name: "s".into(), source: "return function() end".into(), inputs: vec![], outputs: vec![], enabled: true });
        bp.header.hash = content::blueprint::io::compute_hash(&bp);
        assert!(disable_scripts(&mut bp));
        assert!(!bp.graph.scripts[0].enabled);
        assert_eq!(bp.header.hash, content::blueprint::io::compute_hash(&bp));
        assert!(!disable_scripts(&mut bp));
    }

    #[test]
    fn share_strings_round_trip_with_scripts_off() {
        let mut bp = content::campaign::load::stock_blueprint("starters/fac22.bp.ron").unwrap();
        bp.graph.scripts.push(content::graph::ScriptSource { name: "s".into(), source: "return function() end".into(), inputs: vec![], outputs: vec![], enabled: true });
        bp.header.hash = content::blueprint::io::compute_hash(&bp);
        let s = content::share::encode_share_string(&Shareable::Ship(Box::new(bp.clone())));
        let wrapped: String = s.chars().enumerate().flat_map(|(i, c)| if i % 70 == 69 { vec![c, '\n'] } else { vec![c] }).collect();
        match parse_any(&wrapped, "paste.txt").unwrap() {
            Shareable::Ship(back) => {
                assert!(!back.graph.scripts[0].enabled);
                assert_eq!(back.cells, bp.cells);
            }
            Shareable::Kit(_) => panic!("a ship"),
        }
    }
}
