//! Unit `gd-link-replay`. A [`ClientLink`] that plays a replay file, so replays, the kill cam and golden
//! captures drive the same HUD and renderer as live play. DESIGN.md L1043-1050.
//! No re-simulation: `sim_systems::replay::play::ReplayPlayer` yields the same `Snapshot` type as a live
//! session, and this link feeds it through the same [`Presenter`] the in-process link uses, so playback is
//! interpolated and its shells are flown exactly as live frames are.
//!
//! Decisions where the contract is silent:
//! - Viewers. The sides offered are those whose own introductions the recording carries anywhere (each
//!   side's `EntityRevealed` addressed to that side alone; the file is scanned once at open). Ground truth is
//!   offered when the recording opens with the omniscient introductions (addressed to no side) and the mode
//!   is not PvP. A PvP recording never offers
//!   ground truth, and an uncut one (the listen host's own copy, which holds both teams) offers only the
//!   host's side, player slot 0, so a replay never shows interiors the match hid from that player (L1049).
//!   A recording cut for one side offers that side.
//! - Replay time advances by wall time times the rate, with a single frame step held to 0.25 s so a hitch
//!   does not jump the playback. Reaching the end of the file or `control.end_tick` pauses.
//! - A viewer change replays from the current tick (a seek), because each side has its own introductions.
use super::channel::Presenter;
use crate::seams::{ClientEvent, ClientLink, InterpTuning, LinkInbox, LinkState, LinkStatus};
use content::ids::{ContentHash, PlayerSlot, SideId};
use content::share::PackedBlueprint;
use net::blueprints::BlueprintStore;
use sim_systems::ids::SideMask;
use sim_systems::protocol::event::{Audience, OutEvent, SimEvent};
use sim_systems::protocol::Viewer;
use sim_systems::replay::ReplayError;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

/// Playback controls shared between `BpSession` and the link.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReplayControl { pub paused: bool, pub rate: f64, pub seek_to: Option<u64>, pub viewer: Viewer, pub end_tick: Option<u64> }
impl Default for ReplayControl { fn default() -> Self { Self { paused: false, rate: 1.0, seek_to: None, viewer: Viewer::Omniscient, end_tick: None } } }

/// Playback rate limits (`Session.replay_set_rate`).
pub const MIN_RATE: f64 = 0.25;
pub const MAX_RATE: f64 = 8.0;
/// Longest wall-clock step one poll advances replay time by, before the rate.
const MAX_STEP_S: f64 = 0.25;
/// Simulation ticks between recorded poses (15 Hz).
const POSE_TICKS: u64 = (sim_core::TICK_HZ / content::replay::REPLAY_POSE_HZ) as u64;

pub struct ReplayLink {
    pub(crate) player: sim_systems::replay::play::ReplayPlayer,
    pub(crate) presenter: Presenter,
    /// The install's blueprint cache: a replay file names blueprints by hash only (`ReplayHeader::blueprints`).
    pub(crate) store: Option<net::blueprints::DiskCache>,
    pub control: ReplayControl,
    /// Replay time in seconds: advances by wall time times `control.rate`, and is the presenter's clock.
    pub(crate) clock_s: f64,
    pub(crate) last_now_s: Option<f64>,
    /// Next simulation tick whose poses go to the presenter.
    pub(crate) next_pose_tick: u64,
    /// The opening introductions taken at open time, handed out on the first poll.
    pub(crate) opening: Option<Vec<OutEvent>>,
    pub(crate) started: bool,
    /// Viewers the recording allows, ground truth first when allowed.
    pub(crate) allowed: Vec<Viewer>,
    /// Which side each player slot played, learned from introductions and joins (resolves player audiences).
    pub(crate) roster: BTreeMap<PlayerSlot, SideId>,
    /// The viewer the presenter's state was built for.
    pub(crate) built_for: Viewer,
}

/// What the opening events say about the recording: sides with their own introductions, whether the
/// omniscient introductions are present, and the player roster.
#[derive(Default)]
struct Scan { sides: BTreeSet<SideId>, omniscient: bool, roster: BTreeMap<PlayerSlot, SideId> }

fn scan(events: &[OutEvent], into: &mut Scan) {
    let Scan { sides, omniscient, roster } = into;
    for e in events {
        match &e.event {
            SimEvent::EntityRevealed(intro) => {
                if let Some(p) = intro.player { roster.insert(p, intro.side); }
                match e.audience {
                    Audience::Sides(m) if m == SideMask::NONE => *omniscient = true,
                    Audience::Sides(m) if m.iter().count() == 1 => sides.extend(m.iter()),
                    _ => {}
                }
            }
            SimEvent::PlayerJoined { player, side, .. } => { roster.insert(*player, *side); }
            _ => {}
        }
    }
}

fn is_pvp(mode: content::enums::GameMode) -> bool { matches!(mode, content::enums::GameMode::PvpElimination | content::enums::GameMode::PvpEscort) }

impl ReplayLink {
    /// Open replay bytes. A different format version is refused with both versions named (L1045). The
    /// presenter is tuned to the replay's pose rate: `InterpTuning { interval_s: 1 / REPLAY_POSE_HZ,
    /// delay_intervals: 2.0, ..default }`. `cache_dir` is the blueprint cache (`user://blueprint_cache`).
    pub fn open(bytes: &[u8], catalogue: std::sync::Arc<content::catalogue::Catalogue>, cache_dir: std::path::PathBuf) -> Result<ReplayLink, ReplayError> {
        let mut player = sim_systems::replay::play::ReplayPlayer::open(bytes)?;
        let start = player.tick();
        // Which sides and players the recording holds: the opening, then the whole file a keyframe interval
        // at a time (a side's first sighting can come at any moment), then back to the start.
        let mut found = Scan::default();
        scan(&player.seek(start), &mut found);
        let step = content::replay::REPLAY_KEYFRAME_S as u64 * sim_core::TICK_HZ as u64;
        let end = player.header().duration_ticks;
        let mut t = start;
        while t < end {
            t = t.saturating_add(step).min(end);
            scan(&player.advance(t), &mut found);
        }
        let opening = player.seek(start);
        let Scan { sides, omniscient, roster } = found;
        let pvp = is_pvp(player.header().mode);
        let mut allowed = Vec::new();
        if !pvp && (omniscient || sides.is_empty()) { allowed.push(Viewer::Omniscient); }
        if pvp && sides.len() > 1 {
            // The host's own uncut copy: only the host's side.
            if let Some(s) = roster.get(&PlayerSlot(0)).filter(|s| sides.contains(s)) { allowed.push(Viewer::Side(*s)); }
        } else {
            allowed.extend(sides.iter().map(|s| Viewer::Side(*s)));
        }
        let tuning = InterpTuning { interval_s: 1.0 / content::replay::REPLAY_POSE_HZ as f64, delay_intervals: 2.0, ..InterpTuning::default() };
        let store = net::blueprints::DiskCache::open(cache_dir, net::blueprints::CLIENT_CACHE_BYTES).ok();
        let viewer = allowed.first().copied().unwrap_or(Viewer::Omniscient);
        Ok(ReplayLink {
            player,
            presenter: Presenter::new(tuning, catalogue),
            store,
            control: ReplayControl { viewer, ..ReplayControl::default() },
            clock_s: start as f64 / sim_core::TICK_HZ as f64,
            last_now_s: None,
            next_pose_tick: start,
            opening: Some(opening),
            started: false,
            allowed,
            roster,
            built_for: viewer,
        })
    }
    /// A clip of an existing replay or of the live recording, for the kill cam (L1049).
    pub fn clip(bytes: &[u8], from_tick: u64, to_tick: u64, viewer: Viewer, catalogue: std::sync::Arc<content::catalogue::Catalogue>, cache_dir: std::path::PathBuf) -> Result<ReplayLink, ReplayError> {
        let mut link = ReplayLink::open(bytes, catalogue, cache_dir)?;
        let end = link.duration_ticks();
        let to = to_tick.min(end);
        let from = from_tick.min(to);
        if link.allowed.contains(&viewer) { link.control.viewer = viewer; }
        link.control.seek_to = Some(from);
        link.control.end_tick = Some(to);
        Ok(link)
    }
    pub fn tick(&self) -> u64 { self.player.tick() }
    pub fn duration_ticks(&self) -> u64 { self.player.header().duration_ticks }
    /// Sides a viewer may pick: all in single-player and co-op replays; in PvP only what was released (L1049).
    pub fn viewers(&self) -> Vec<Viewer> { self.allowed.clone() }

    /// Playback position in seconds of replay time (the newest tick handed to the presenter).
    pub(crate) fn clock_s(&self) -> f64 { self.clock_s }

    /// Whether the current viewer may receive `e`.
    fn admits(&self, e: &OutEvent) -> bool {
        match self.built_for {
            Viewer::Omniscient => match e.audience {
                Audience::Host => false,
                // Only the omniscient introductions and removals: the sides' own would hide ships a side lost.
                a => match e.event { SimEvent::EntityRevealed(_) | SimEvent::EntityHidden { .. } => a == Audience::Sides(SideMask::NONE), _ => true },
            },
            Viewer::Side(s) => match e.audience {
                Audience::All => true,
                Audience::Sides(m) => m.has(s),
                Audience::Players(m) => self.roster.iter().any(|(p, ps)| *ps == s && m.has(*p)),
                Audience::Host => false,
            },
            Viewer::Player(p) => match self.roster.get(&p) { Some(s) => e.audience.admits(p, *s), None => matches!(e.audience, Audience::All) },
        }
    }

    /// Filter, convert, apply and surface a batch of recorded events.
    fn emit(&mut self, events: Vec<OutEvent>, inbox: &mut LinkInbox) {
        for e in events {
            match &e.event {
                SimEvent::EntityRevealed(intro) => { if let Some(p) = intro.player { self.roster.insert(p, intro.side); } }
                SimEvent::PlayerJoined { player, side, .. } => { self.roster.insert(*player, *side); }
                _ => {}
            }
            if !self.admits(&e) { continue; }
            let wire = net::events::to_wire(&e);
            self.presenter.push_event(&wire);
            inbox.events.push(wire);
        }
    }

    fn end(&self) -> u64 {
        let d = self.duration_ticks();
        self.control.end_tick.map_or(d, |e| e.min(d))
    }

    /// Jump to `tick`: rebuild the presenter from the keyframe at or before it.
    fn seek(&mut self, tick: u64, inbox: &mut LinkInbox) {
        let tick = tick.min(self.duration_ticks());
        let events = self.player.seek(tick);
        self.presenter.clear();
        self.built_for = self.control.viewer;
        self.clock_s = tick as f64 / sim_core::TICK_HZ as f64;
        self.next_pose_tick = tick;
        self.emit(events, inbox);
    }
}

impl ClientLink for ReplayLink {
    /// Advance replay time by `(now_s - last now_s) x control.rate` (0 while paused); apply a pending seek
    /// (`ReplayPlayer::seek`, then `Presenter::clear`); `ReplayPlayer::advance` to the new tick and, once per
    /// recorded pose interval passed (`content::replay::REPLAY_POSE_HZ`, 15 Hz of replay time),
    /// `Presenter::push_snapshot(player.snapshot(viewer), replay time)`;
    /// events become `WireEvent`s with `net::events::to_wire` after `Audience::admits` for a side viewer
    /// (`Viewer::Omniscient` takes all but `Audience::Host`), go through `Presenter::push_event` and into
    /// `inbox.events`; then `Presenter::sample`. The first poll appends `ClientEvent::SortieRunning`; reaching
    /// `control.end_tick` or the end of the file pauses.
    fn poll(&mut self, now_s: f64, inbox: &mut crate::seams::LinkInbox) {
        let dt = match self.last_now_s { Some(last) if now_s.is_finite() => (now_s - last).clamp(0.0, MAX_STEP_S), _ => 0.0 };
        if now_s.is_finite() { self.last_now_s = Some(now_s); }
        if !self.control.viewer_allowed(&self.allowed) { self.control.viewer = self.allowed.first().copied().unwrap_or(Viewer::Omniscient); }
        if !self.started {
            self.started = true;
            inbox.session.push(ClientEvent::SortieRunning { tick: self.player.tick() });
            if self.control.viewer != self.built_for {
                // The opening was taken for the default viewer; rebuild for the one asked for.
                self.opening = None;
                if self.control.seek_to.is_none() { self.control.seek_to = Some(self.player.tick()); }
            } else if let Some(opening) = self.opening.take() {
                self.emit(opening, inbox);
            }
        }
        if self.control.viewer != self.built_for && self.control.seek_to.is_none() { self.control.seek_to = Some(self.player.tick()); }
        if let Some(t) = self.control.seek_to.take() { self.seek(t, inbox); }
        let end = self.end();
        if !self.control.paused {
            let rate = if self.control.rate.is_finite() { self.control.rate.clamp(MIN_RATE, MAX_RATE) } else { 1.0 };
            self.clock_s += dt * rate;
        }
        let mut target = (self.clock_s.max(0.0) * sim_core::TICK_HZ as f64).floor() as u64;
        if target >= end {
            target = end;
            self.clock_s = end as f64 / sim_core::TICK_HZ as f64;
            self.control.paused = true;
        }
        let viewer = self.built_for;
        while self.next_pose_tick <= target {
            let k = self.next_pose_tick;
            if k > self.player.tick() {
                let events = self.player.advance(k);
                self.emit(events, inbox);
            }
            let snapshot = self.player.snapshot(viewer);
            self.presenter.push_snapshot(snapshot, Duration::from_secs_f64(k as f64 / sim_core::TICK_HZ as f64));
            self.next_pose_tick = k + POSE_TICKS;
        }
        if target > self.player.tick() {
            let events = self.player.advance(target);
            self.emit(events, inbox);
        }
        self.presenter.sample(Duration::from_secs_f64(self.clock_s.max(0.0)), inbox);
    }
    /// Commands are ignored during playback.
    fn send(&mut self, msg: crate::seams::ClientMsg) {}
    fn status(&self) -> LinkStatus { LinkStatus { state: LinkState::Joined, link: Default::default(), interp: self.presenter.stats() } }
    /// From the blueprint cache, where the live session that was recorded stored it; `None` when this
    /// install never held the design (the vessel then draws as its class's stock design).
    fn blueprint(&mut self, hash: &content::ids::ContentHash) -> Option<content::share::PackedBlueprint> {
        let store = self.store.as_mut()?;
        store.get(hash).map(|compressed| PackedBlueprint { hash: *hash, compressed })
    }
    fn close(&mut self) {}
}

impl ReplayControl {
    fn viewer_allowed(&self, allowed: &[Viewer]) -> bool { allowed.is_empty() || allowed.contains(&self.viewer) }
}

#[cfg(test)]
mod tests;
