//! Unit `gd-campaign-saves`. Save slots on `BpCampaign`: new campaign, load, save, suspend and resume,
//! Ironman, co-op copies. DESIGN.md L826-828 (full saves at coves and between operations; a mid-sortie quit
//! writes a suspend save from the latest 60 s checkpoint to the single suspend slot, and loading it deletes
//! it; Ironman keeps one rolling save and makes hull loss permanent), L843-845 (host writes the save, every
//! member keeps a copy; sequence number plus hash of the previous save; highest sequence wins, a forked copy
//! is refused with an explanation), L1359-1360 (saves under the user data directory; non-ASCII and long
//! paths). Encoding is `content::save`; gating and headers are `sim_systems::theatre::saves`.
//! Files: `user://saves/<campaign id>/<slot>.bpsave` and `user://saves/<campaign id>/suspend.bpsave`.
//!
//! # Decisions where the contract is silent
//! - File stems are fixed words, never the typed slot name (see `store`): `ironman` (Ironman's one rolling
//!   save), `autosave` (the rolling save between operations), `save-<sequence>` (each `save_now` outside
//!   Ironman; the newest 20 are kept), `copy` (a co-op member's copy), `suspend` (one in all).
//! - Every full save chains: `Theatre::to_save` advances the sequence and stores the previous hash.
//! - `load_slot` on the suspend file resumes it, as `resume_suspend` does.
//! - A new campaign is written to disk at once; if that fails the campaign does not start.
//! - Members never write full saves of their own: `save_now` refuses them (`campaign.reject.not_host`),
//!   and their copies arrive from the host.
//! - `saved` (which may toast) fires for a save the player asked for and for the save written as a result
//!   comes in; the quiet rolling saves while the theatre settles do not announce themselves.
//! - A new campaign whose id (derived by the theatre from the seed and the first client id) already has a
//!   folder gets a fresh id, so reusing a seed never overwrites another campaign's saves.
//! - An empty slot name becomes `LANTERN` and four digits of the seed (a callsign, not English copy); the
//!   local player's name is the display name, or `LANTERN` when none is set.
use super::{store, Backend, BpCampaign, CampaignRun, Sig};
use crate::enums::{CampaignPhase, GdEnum, SessionState};
use godot::classes::ProjectSettings;
use godot::prelude::*;
use sim_systems::protocol::Msg;
use sim_systems::theatre::saves::SaveGate;
use sim_systems::theatre::{NewCampaign, Theatre, TheatreReject};
use std::path::PathBuf;

/// The saves root as an OS path.
pub(crate) fn saves_root() -> PathBuf { PathBuf::from(ProjectSettings::singleton().globalize_path(store::SAVES_DIR).to_string()) }

fn msgs(list: Vec<Msg>) -> Array<VarDictionary> {
    let mut out = Array::new();
    for m in &list { out.push(&crate::convert::msg_dict(m)); }
    out
}

/// A seed for a new campaign when none is given: wall time mixed with the client id (splitmix64). Seed
/// choice is outside the simulation, so wall time is fine here.
pub(crate) fn fresh_seed(client_id: u64) -> u64 {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    let mut z = nanos ^ client_id.rotate_left(17) ^ 0x9E37_79B9_7F4A_7C15;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31)).max(1)
}

/// The next campaign id after `id` (splitmix64 steps) that is not zero and not `taken`.
pub(crate) fn fresh_campaign_id(id: u64, taken: impl Fn(u64) -> bool) -> u64 {
    let mut x = id;
    for _ in 0..64 {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        if z != 0 && !taken(z) { return z; }
    }
    id ^ 1
}

/// A slot name of at most 64 characters with control characters removed.
pub(crate) fn clean_slot_name(raw: &str) -> String {
    raw.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(64).collect()
}

/// The stock key of a starter, if it is one the campaign offers in Act 1.
pub(crate) fn act1_starter(path: &str, data: &content::campaign::CampaignData) -> Option<String> {
    let key = crate::content_api::model::normalize_stock(path.strip_prefix("stock:").unwrap_or(path));
    let offered = crate::content_api::model::starter_paths(&data.acts, Some(content::enums::Act::Act1));
    offered.into_iter().map(|p| crate::content_api::model::normalize_stock(&p)).find(|p| *p == key)
}

#[godot_api(secondary)]
impl BpCampaign {
    /// Every save on disk, newest first: `Array[SaveSlotDict]`. Unreadable or newer-format files are listed
    /// with `loadable: false` and a `problem`.
    #[func]
    fn slots(&self) -> Array<VarDictionary> {
        let root = saves_root();
        let list = store::list(&root, &mut self.run.slot_cache.borrow_mut());
        let mut out = Array::new();
        for s in &list { out.push(&super::dicts::save_slot_dict(s)); }
        out
    }
    /// Start a campaign. `options` keys: `slot_name: String`, `difficulty: DifficultyDict`, `ironman: bool`,
    /// `seed: int` (0 = random), `starter: String` (stock path of the first hull; default the 22 m fast attack
    /// craft), `tutorial_done: bool`. In co-op the lobby supplies the other players. Returns
    /// `Array[MsgDict]` of problems; empty = started at the chart table.
    #[func]
    fn new_campaign(&mut self, options: VarDictionary) -> Array<VarDictionary> {
        let problems = self.new_campaign_inner(&options);
        self.flush();
        msgs(problems)
    }
    /// Load a save file. Returns `Array[MsgDict]` of problems; empty = loaded.
    #[func]
    fn load_slot(&mut self, path: GString) -> Array<VarDictionary> {
        let problems = self.load_inner(&path.to_string());
        self.flush();
        msgs(problems)
    }
    /// Whether a full save is allowed now (`Bp.SAVE_*`).
    #[func]
    fn can_save(&self) -> i64 { self.save_gate() }
    /// Write a full save (Ironman overwrites its one rolling save). Emits `saved`.
    #[func]
    fn save_now(&mut self) -> bool {
        let ok = self.save_inner(true, true);
        self.flush();
        ok
    }
    #[func]
    fn delete_slot(&mut self, path: GString) -> bool {
        let root = saves_root();
        let ok = match store::resolve(&root, &path.to_string()) {
            Some(p) => store::delete(&p),
            None => { self.reject(Msg::new("campaign.save.outside")); false }
        };
        self.flush();
        ok
    }
    /// The suspend slot's `SaveSlotDict`, or `null` when there is none.
    #[func]
    fn suspend_slot(&self) -> Variant {
        let root = saves_root();
        let found = store::find_suspend(&root, &mut self.run.slot_cache.borrow_mut());
        found.map_or(Variant::nil(), |s| super::dicts::save_slot_dict(&s).to_variant())
    }
    /// Resume the suspended sortie: loads the theatre, deletes the slot (advancing the sequence) and hands
    /// the checkpoint to the session.
    #[func]
    fn resume_suspend(&mut self) -> bool {
        let root = saves_root();
        let found = store::find_suspend(&root, &mut self.run.slot_cache.borrow_mut());
        let ok = match found {
            Some(s) => match self.resume_inner(&s.path) { Ok(()) => true, Err(m) => { self.reject(m); false } },
            None => { self.reject(Msg::new("campaign.save.not_found")); false }
        };
        self.flush();
        ok
    }
    /// Save if allowed, close the campaign and return to `Bp.PHASE_NONE` (main menu).
    #[func]
    fn close_campaign(&mut self) {
        // The pause menu calls `Session.leave` and then this in one frame: take the quit sortie's hand-off
        // now, so its checkpoint reaches the suspend slot before the theatre is dropped.
        if self.phase == CampaignPhase::Sortie && self.local().is_some() {
            let services = crate::services::services();
            self.watch_sortie(&services);
        }
        if self.local().is_none() && self.backend.is_some() { self.close_mirror(); } else { self.close_inner(true); }
        self.flush();
    }
    /// The loaded campaign's own `SaveSlotDict` (sequence, played time, Ironman).
    #[func]
    fn current_slot(&self) -> VarDictionary {
        let (header, state) = match &self.backend {
            Some(Backend::Local(t)) => (Some(t.header().clone()), Some(&t.state)),
            Some(Backend::Remote { .. }) => (self.run.coop.copy.as_ref().map(|s| s.header.clone()), self.run.coop.copy.as_ref().map(|s| &s.theatre)),
            None => (None, None),
        };
        let Some(header) = header else { return VarDictionary::new() };
        let root = saves_root();
        let path = store::resolve(&root, &self.slot_path);
        let modified = path.as_ref().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs().min(i64::MAX as u64) as i64);
        let rel = path.as_ref().and_then(|p| p.strip_prefix(&root).ok()).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
        let info = store::SlotInfo {
            path: path.unwrap_or_default(), rel: rel.clone(), header: Some(header), act: state.map(|s| s.act), hour: state.map_or(0, |s| s.hour),
            players: state.map(|s| s.players.iter().map(|p| p.name.clone()).collect()).unwrap_or_default(), modified_unix: modified, is_suspend: false, problem: None,
        };
        let mut d = super::dicts::save_slot_dict(&info);
        if rel.is_empty() { d.set("path", &GString::new()); }
        d
    }
}

impl BpCampaign {
    /// `Bp.SAVE_*` now.
    pub(crate) fn save_gate(&self) -> i64 {
        let gate = match &self.backend {
            Some(Backend::Local(t)) => {
                if self.phase == CampaignPhase::Sortie || t.active().is_some() { SaveGate::InSortie }
                else if self.advancing || self.run.under_way { SaveGate::NotAtCoveOrBetweenOperations }
                else { sim_systems::theatre::saves::can_save(t) }
            }
            Some(Backend::Remote { .. }) if self.phase == CampaignPhase::Sortie => SaveGate::InSortie,
            _ => SaveGate::NotAtCoveOrBetweenOperations,
        };
        gate.to_gd()
    }

    /// Write a full save now if the gate allows. `manual` is a save the player asked for (its own file
    /// outside Ironman); otherwise the rolling autosave. `announce` emits `saved` (the toast).
    pub(crate) fn save_inner(&mut self, manual: bool, announce: bool) -> bool {
        if self.backend.is_none() { self.reject(Msg::new("campaign.reject.no_campaign")); return false; }
        if self.local().is_none() { if manual { self.reject(super::words::reject_msg(TheatreReject::NotHost)); } return false; }
        if self.save_gate() != SaveGate::Allowed.to_gd() { if manual { self.reject(Msg::new("campaign.save.not_allowed")); } return false; }
        let root = saves_root();
        let Some(t) = self.local_mut() else { return false };
        let save = t.to_save(env!("CARGO_PKG_VERSION"));
        let stem = if save.header.ironman { store::IRONMAN_STEM.to_string() } else if manual { store::manual_stem(save.header.sequence) } else { store::AUTOSAVE_STEM.to_string() };
        self.pump();
        match store::write_save(&root, &save, &stem) {
            Ok(path) => {
                if manual && !save.header.ironman { store::prune_manual(&root, save.header.campaign_id); }
                let info = store::slot_info(&root, &path, &mut self.run.slot_cache.borrow_mut());
                self.slot_path = store::user_path(&info.rel);
                self.run.autosave_due = false;
                self.run.last_autosave_s = self.run.now_s;
                if announce { self.run.sigs.push(Sig::Saved(Box::new((*info).clone()))); }
                self.coop_send_copy(&save, None);
                true
            }
            Err(e) => {
                godot_warn!("campaign: the save could not be written: {e}");
                self.reject(Msg::new("campaign.save.write_failed"));
                false
            }
        }
    }

    /// The rolling save when one is due and allowed; `now` skips the spacing (after a result).
    pub(crate) fn maybe_autosave(&mut self, now: bool) {
        if self.local().is_none() { return; }
        if !now && (!self.run.autosave_due || self.run.now_s - self.run.last_autosave_s < super::AUTOSAVE_MIN_S) { return; }
        if self.save_gate() != SaveGate::Allowed.to_gd() { return; }
        if !self.save_inner(false, now) { self.run.autosave_due = false; }
    }

    /// Reset everything that belongs to one loaded campaign (queued signals, the co-op roster and the slot
    /// cache stay).
    fn reset_run(&mut self) {
        let coop = std::mem::take(&mut self.run.coop);
        let cache = std::mem::take(&mut self.run.slot_cache);
        let sigs = std::mem::take(&mut self.run.sigs);
        let now = self.run.now_s;
        self.run = CampaignRun { coop, slot_cache: cache, sigs, now_s: now, ..Default::default() };
        self.run.coop.copy = None;
        self.run.coop.bundle = None;
        self.run.coop.briefed.clear();
        self.after_action = None;
        self.advancing = false;
        self.advance_accum_s = 0.0;
        self.slot_path.clear();
    }

    fn session_busy() -> bool {
        let s = crate::services::services();
        let st = s.feed.borrow().state;
        matches!(st, SessionState::Loading | SessionState::InSortie) || s.handoff.borrow().sortie.is_some()
    }

    fn new_campaign_inner(&mut self, options: &VarDictionary) -> Vec<Msg> {
        use crate::convert::{get_bool, get_dict, get_i64, get_text};
        if Self::session_busy() { return vec![Msg::new("campaign.new.busy")]; }
        let services = crate::services::services();
        let campaign = services.campaign();
        let mut problems = Vec::new();
        let difficulty = match get_dict(options, "difficulty") {
            Some(d) => crate::convert::difficulty_from_dict(&d).unwrap_or_else(|| { problems.push(Msg::new("campaign.new.bad_difficulty")); content::campaign::DifficultySliders::NORMAL }),
            None => content::campaign::DifficultySliders::NORMAL,
        };
        let starter = get_text(options, "starter").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| super::DEFAULT_STARTER.to_string());
        let blueprint = match act1_starter(&starter, &campaign) {
            Some(key) => content::campaign::load::stock_blueprint(&key).map_err(|e| crate::convert::error_msg(&e)),
            None => Err(Msg::new("campaign.new.bad_starter")),
        };
        let blueprint = match blueprint { Ok(bp) => bp, Err(m) => { problems.push(m); return problems; } };
        if !problems.is_empty() { return problems; }
        let client_id = self.run.coop.client_id();
        let seed = get_i64(options, "seed").filter(|s| *s != 0).map_or_else(|| fresh_seed(client_id), |s| s as u64);
        let display = services.settings.borrow().display_name.trim().to_string();
        let own_name = if display.is_empty() { "LANTERN".to_string() } else { display };
        let mut slot_name = clean_slot_name(&get_text(options, "slot_name").unwrap_or_default());
        if slot_name.is_empty() { slot_name = format!("LANTERN {:04}", seed % 10_000); }
        drop(services);
        // Co-op: the session's connected members sail too, each in the same starter hull (L822).
        let mut players = vec![(client_id, own_name, blueprint.clone())];
        if self.hosting() {
            for m in self.run.coop.members.values().filter(|m| m.connected) {
                if players.len() >= sim_systems::theatre::MAX_CAMPAIGN_PLAYERS { break; }
                if players.iter().all(|p| p.0 != m.client_id) { players.push((m.client_id, m.name.clone(), blueprint.clone())); }
            }
        }
        let new = NewCampaign { seed, difficulty, ironman: get_bool(options, "ironman").unwrap_or(false), slot_name, players, start_act: content::enums::Act::Act1 };
        let mut theatre = Theatre::new_campaign(new, super::theatre_data());
        let opening = theatre.drain_events();
        // The campaign id comes from the seed and the first client id, so a reused seed would land in an
        // existing campaign's folder and overwrite its saves: give the new campaign a fresh id instead.
        let root = saves_root();
        if store::campaign_dir(&root, theatre.header().campaign_id).exists() {
            let mut save = theatre.to_save(env!("CARGO_PKG_VERSION"));
            save.header.campaign_id = fresh_campaign_id(save.header.campaign_id, |id| store::campaign_dir(&root, id).exists());
            theatre = Theatre::from_save(save, super::theatre_data());
        }
        self.close_inner(true);
        self.reset_run();
        self.run.tutorial_done = get_bool(options, "tutorial_done").unwrap_or(false);
        self.backend = Some(Backend::Local(Box::new(theatre)));
        for e in &opening { self.observe_event(e, 0); }
        self.pump();
        if !self.save_inner(false, false) {
            self.backend = None;
            self.reset_run();
            self.run.sigs.retain(|s| matches!(s, Sig::Rejected(_)));
            return vec![Msg::new("campaign.new.save_failed")];
        }
        self.bump();
        self.set_phase(CampaignPhase::Chart);
        self.run.sigs.push(Sig::Resources);
        self.coop_send_chart(None);
        Vec::new()
    }

    fn load_inner(&mut self, path: &str) -> Vec<Msg> {
        if Self::session_busy() { return vec![Msg::new("campaign.new.busy")]; }
        let root = saves_root();
        let Some(file) = store::resolve(&root, path) else { return vec![Msg::new("campaign.save.outside")] };
        if store::is_suspend_path(&file) {
            return match self.resume_inner(&file) { Ok(()) => Vec::new(), Err(m) => vec![m] };
        }
        let save = match store::read_save(&file) { Ok(s) => s, Err(m) => return vec![m] };
        self.close_inner(true);
        self.reset_run();
        self.open_theatre(Theatre::from_save(save, super::theatre_data()));
        self.slot_path = store::user_path(&file.strip_prefix(&root).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default());
        Vec::new()
    }

    /// Show a loaded theatre at the chart table (or the end of a finished Ironman campaign).
    fn open_theatre(&mut self, t: Theatre) {
        let over = sim_systems::theatre::saves::campaign_over(&t);
        self.backend = Some(Backend::Local(Box::new(t)));
        // Load-time events (offers refreshed) are the campaign's opening state, not news.
        if let Some(t) = self.local_mut() { let _ = t.drain_events(); }
        self.bump();
        self.set_phase(if over { CampaignPhase::Over } else { CampaignPhase::Chart });
        self.run.sigs.push(Sig::Resources);
        self.coop_send_chart(None);
    }

    fn resume_inner(&mut self, file: &std::path::Path) -> Result<(), Msg> {
        if Self::session_busy() { return Err(Msg::new("campaign.new.busy")); }
        let save = store::read_suspend(file)?;
        let (theatre, checkpoint) = sim_systems::theatre::saves::resume_suspend(save, super::theatre_data());
        // Loading the suspend slot consumes it (L828).
        if !store::delete(file) { return Err(Msg::new("campaign.save.write_failed")); }
        self.close_inner(true);
        self.reset_run();
        self.backend = Some(Backend::Local(Box::new(theatre)));
        if let Some(t) = self.local_mut() { let _ = t.drain_events(); }
        let hour = self.state().map_or(0, |s| s.hour);
        let services = crate::services::services();
        services.handoff.borrow_mut().sortie = Some(crate::session::PendingSortie {
            kind: crate::enums::SessionKind::Campaign,
            start: crate::session::host::HostStart::Checkpoint { checkpoint: Box::new(checkpoint) },
            options: crate::session::host::SortieOptions { checkpoints: true, fixed_step: false },
        });
        self.coop_sortie_started(&services);
        drop(services);
        self.run.sortie = Some(super::SortieWatch { launched_s: self.run.now_s, hour, seen_running: false, finished_s: None });
        self.bump();
        self.set_phase(CampaignPhase::Sortie);
        self.run.sigs.push(Sig::Resources);
        Ok(())
    }

    /// The player quit mid-sortie: the suspend slot takes the checkpoint and the campaign closes (L112,
    /// L828). Without a checkpoint the sortie never happened, and the campaign closes from the chart table.
    pub(crate) fn sortie_quit(&mut self, checkpoint: Option<Box<content::save::SortieCheckpoint>>) {
        let root = saves_root();
        let written = match (checkpoint, self.local()) {
            (Some(c), Some(t)) => {
                let s = sim_systems::theatre::saves::make_suspend(t, *c, env!("CARGO_PKG_VERSION"));
                match store::write_suspend(&root, &s) {
                    Ok(_) => true,
                    Err(e) => { godot_warn!("campaign: the suspend save could not be written: {e}"); self.reject(Msg::new("campaign.save.suspend_failed")); false }
                }
            }
            _ => false,
        };
        if !written {
            // Back to the state before the sortie, which is a legal full save.
            if let Some(t) = self.local_mut() { let r = super::aborted_result(t); let _ = t.apply_result(&r); }
            self.pump();
            self.run.sortie = None;
            self.set_phase(CampaignPhase::Chart);
        }
        self.close_inner(!written);
    }

    /// Close the loaded campaign, saving first when `save` and the gate allows.
    pub(crate) fn close_inner(&mut self, save: bool) {
        if self.backend.is_none() { return; }
        if save && self.local().is_some() && self.save_gate() == SaveGate::Allowed.to_gd() { let _ = self.save_inner(false, false); }
        if let Some(t) = self.local_mut() { let _ = t.drain_events(); }
        self.backend = None;
        self.reset_run();
        self.bump();
        self.set_phase(CampaignPhase::None);
        self.run.sigs.push(Sig::Resources);
    }
}
