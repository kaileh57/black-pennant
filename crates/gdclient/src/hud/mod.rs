//! Unit `gd-hud-core`. `BpHud` (autoload `Hud`): read access to everything the sortie HUD shows, as change
//! signals plus pull getters. This file holds the class, its signals, the per-frame update, the event log and
//! the officer lines. DESIGN.md L1653-1674 (sortie HUD rules), L1676-1687 (alerts), L1671 (log, rule 15),
//! L1706 (officer and ace lines), L1449 (2 ms adapter budget).
//! Sibling units add `#[godot_api(secondary)]` blocks to the same class: `ship.rs` = `gd-hud-ship`,
//! `tracks.rs` = `gd-hud-tracks`, `alerts.rs` = `gd-hud-alerts`, `board.rs` = `gd-hud-board`,
//! `damage.rs` = `gd-hud-damage`.
//!
//! # Cost model (frozen)
//! - `process` never builds a Dictionary. Each frame it compares the sections of the newest snapshot with the
//!   previous one (`PartialEq` on the protocol types), bumps the section's revision counter and emits that
//!   section's change signal, at most once per frame per section.
//! - Getters build their Dictionaries on demand and cache the result per section revision, so a second call
//!   in the same revision returns the cached Array.
//! - UI scripts connect to the signal of the section they show and pull in the handler. They never poll in
//!   `_process`. The plot does not use these getters at all: `BpPlotView` reads the Rust data directly.
//! - Update rates are those of the protocol: motion 30 Hz, tracks and mounts 10 Hz, parts, compartments,
//!   power and networks 2 Hz, the rest on change.
//!
//! # Event flow
//! `process` walks `Services::feed.events` once: track, control, alert, objective and officer events become
//! signals and log entries here; physical events are left to the world view and the audio feed.
//!
//! # What sibling units use from here
//! - [`BpHud::cached`]: the per-revision cache. A getter passes a name, the sections its result depends on
//!   and a builder; the builder runs once per revision of those sections. Derive single-item getters
//!   (`mount(slot)`, `alert(id)`) from the cached list rather than caching each id.
//! - [`BpHud::snap`], [`BpHud::ship`], [`BpHud::design`], [`BpHud::own_view`], [`BpHud::core`]: the newest
//!   snapshot, the viewer's `ShipStatus`, the own design (static facts; may arrive after the hull does, which
//!   bumps `Section::Static` and every own-ship section), the own hull's `VesselView`, and the state only the
//!   event walk sees ([`HudCore`]: entity introductions, player names, the alert book with every alert of the
//!   sortie, the silence deadline and flood mode, the sortie tick).
//! - [`alert_dict`] builds `AlertDict` (shared by `alert_raised` here and the alerts getters).
//! - [`part_name_msg`] names a part as the HUD does everywhere ("Gun 2", or the player's label).
//!
//! # Decisions where the contract is silent
//! - Section changes are found only when `FrameFeed::snapshot_rev` moves (about 30 Hz): the feed's snapshot is
//!   re-interpolated every frame, so diffing it per frame would fire `motion_changed` at the frame rate.
//!   Board and tracks are further held to 10 Hz and allies to 2 Hz. Tracks also refresh when the own hull
//!   moved more than 5 m or a sortie second passed, because range, bearing, closest approach and age are
//!   relative to the own hull and to time.
//! - Repeated events: a kill-cam clip, its end and a replay seek feed an opening (`SortieStarted`, reveals,
//!   pacing) and old events again. State-building events (introductions, alert book) always apply; log
//!   entries and announcement signals (`alert_raised`, `officer_line`, `track_event`, `control_changed`) are
//!   produced only for events at or after the newest tick already walked. A new sortie (the session entering
//!   `Bp.SESSION_LOADING`) or a replay's `SortieStarted` resets everything. Pacing entries are logged only
//!   when the pacing differs from the last one logged.
//! - Engagement entries are logged when a mount's status changes kind or target, never per engagement tick;
//!   "Solution unconverged" at most once per mount and track every 10 s.
//! - The log keeps at most [`LOG_CAP`] entries (about 4 hours of a heavy battle); past that the oldest silent
//!   entry goes first, then the oldest entry.
//! - `log_entries` returns the newest `limit` matches, oldest first, so a panel that opens late shows the
//!   latest lines and an incremental read after `after_seq` still loses nothing in normal use.
//! - Officer lines are those of the viewer's own hull; ace lines come from every ace the side hears.
//!   `new_speaker` is true when the speaker differs from the previous line's or 30 s passed since it.
//! - Alert sounds and radio cues are queued by `BpAudioFeed` from the same events; the dictionaries name the
//!   stem it plays.
//! - Text keys of this unit and its siblings `gd-hud-ship`, `gd-hud-tracks` and `gd-orders` live in
//!   `game/i18n/log.csv` under `log.*` (log lines), `hud.*` (HUD text) and `orders.*` (order refusals).
pub mod alerts;
pub mod board;
pub mod damage;
pub mod ship;
pub mod tracks;

use crate::enums::{GdEnum, LogKind, SessionKind, SessionState};
use crate::services::services;
use content::enums::{OfficerRole, Stance};
use content::graph::SinkKind;
use content::ids::{CompartmentId, ContentHash, PartSlot, PlayerSlot, SideId, TextKey};
use glam::DVec2;
use godot::prelude::*;
use sim_core::events::PhysEvent;
use sim_core::EntityId;
use sim_systems::ids::{AlertId, TrackId};
use sim_systems::protocol::event::SimEvent;
use sim_systems::protocol::{
    AlertPriority, AlertState, AlertView, ControlHolder, ControlRole, EngageStatus, Msg, MsgArg, OverrideKey, Pacing, QueryReply, ShipStatus, Snapshot, Unit, VesselView, Viewer,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Sections with their own revision counter and change signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section { Motion, Power, Parts, Mounts, Compartments, Crew, Networks, Board, Tracks, Alerts, Objectives, Allies, Actions, Offboard, Static }
impl Section { pub const COUNT: usize = 15; }

impl Section {
    /// Every section in declaration order (index = `self as usize`).
    pub const ALL: [Section; Section::COUNT] = [
        Section::Motion, Section::Power, Section::Parts, Section::Mounts, Section::Compartments, Section::Crew, Section::Networks, Section::Board,
        Section::Tracks, Section::Alerts, Section::Objectives, Section::Allies, Section::Actions, Section::Offboard, Section::Static,
    ];
    /// Sections that describe the own hull: all of them change when the hull or its design changes.
    const OWN_SHIP: [Section; 11] = [
        Section::Motion, Section::Power, Section::Parts, Section::Mounts, Section::Compartments, Section::Crew, Section::Networks, Section::Board,
        Section::Actions, Section::Offboard, Section::Static,
    ];
    pub fn index(self) -> usize { self as usize }
    fn bit(self) -> u32 { 1 << self.index() }
    /// The name `BpHud::rev` takes.
    pub fn name(self) -> &'static str {
        match self {
            Section::Motion => "motion", Section::Power => "power", Section::Parts => "parts", Section::Mounts => "mounts",
            Section::Compartments => "compartments", Section::Crew => "crew", Section::Networks => "networks", Section::Board => "board",
            Section::Tracks => "tracks", Section::Alerts => "alerts", Section::Objectives => "objectives", Section::Allies => "allies",
            Section::Actions => "actions", Section::Offboard => "offboard", Section::Static => "static",
        }
    }
    pub fn from_name(name: &str) -> Option<Section> { Section::ALL.into_iter().find(|s| s.name() == name) }
    /// The change signal of the section.
    fn signal(self) -> &'static str {
        match self {
            Section::Motion => "motion_changed", Section::Power => "power_changed", Section::Parts => "parts_changed", Section::Mounts => "mounts_changed",
            Section::Compartments => "compartments_changed", Section::Crew => "crew_changed", Section::Networks => "networks_changed",
            Section::Board => "board_changed", Section::Tracks => "tracks_changed", Section::Alerts => "alerts_changed",
            Section::Objectives => "objectives_changed", Section::Allies => "allies_changed", Section::Actions => "actions_changed",
            Section::Offboard => "offboard_changed", Section::Static => "ship_static_changed",
        }
    }
    /// Shortest time between two bumps of the section, seconds (the rates in the module docs).
    fn min_interval_s(self) -> f64 {
        match self {
            Section::Motion | Section::Static => 0.0,
            Section::Mounts | Section::Board | Section::Tracks | Section::Alerts => 0.1,
            Section::Actions => 0.25,
            Section::Power | Section::Parts | Section::Compartments | Section::Crew | Section::Networks | Section::Allies | Section::Objectives | Section::Offboard => 0.5,
        }
    }
}

/// One line of the HUD log before conversion (rule 15).
#[derive(Clone, Debug, PartialEq)]
pub struct LogEntry { pub seq: u64, pub tick: u64, pub kind: crate::enums::LogKind, pub text: sim_systems::protocol::Msg, pub track: TrackId, pub part: Option<content::ids::PartSlot>, pub vessel: Option<sim_core::EntityId>, pub pos: Option<[f64; 2]>, pub unread: bool, pub silent: bool }

/// Most log entries kept; see the module docs.
pub const LOG_CAP: usize = 20_000;
/// Officer and ace lines kept for `officer_lines`.
const LINES_CAP: usize = 64;
/// History points per track: one a second for 10 minutes.
pub const HISTORY_POINTS: usize = 600;
/// Default and largest `log_entries` page.
const LOG_PAGE: usize = 200;
const LOG_PAGE_MAX: usize = 5_000;
/// Ticks per second of sortie time.
const TICK_HZ: u64 = sim_core::TICK_HZ as u64;
/// A speaker is named again after this much silence (L1706: speaker named at changes).
const SPEAKER_GAP_TICKS: u64 = 30 * TICK_HZ;
/// "Solution unconverged" repeats at most this often per mount and track.
const UNCONVERGED_GAP_TICKS: u64 = 10 * TICK_HZ;
/// Scope values go out at most this often per port (10 Hz).
const SCOPE_GAP_S: f64 = 0.1;
/// Tracks refresh when the own hull moved this far since the last refresh.
const TRACKS_MOVE_M: f64 = 5.0;

/// Sortie seconds of a tick.
pub(crate) fn tick_s(tick: u64) -> f64 { tick as f64 / sim_core::TICK_HZ as f64 }

// ---------------------------------------------------------------------------------------------------------
// Plain state the event walk keeps (no Godot types: unit-tested without the engine)
// ---------------------------------------------------------------------------------------------------------

/// Facts kept from an `EntityRevealed`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct IntroFacts { pub blueprint: ContentHash, pub side: SideId, pub name: String, pub player: Option<PlayerSlot>, pub class: Option<content::ids::ClassId>, pub track: TrackId }

/// Every alert of the sortie as the event walk and the snapshots told it, ascending id; plus silence and flood.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct AlertBook {
    /// Ascending id. Updated from `AlertRaised`, `AlertEscalated`, `AlertChanged` and each snapshot's list.
    pub history: Vec<AlertView>,
    /// Sortie tick until which alert sounds are silenced (`AlertsSilenced`); 0 = not silenced.
    pub silenced_until_tick: u64,
    /// Flood mode (more than 10 alerts in 10 minutes, L1687).
    pub flood: bool,
}
impl AlertBook {
    fn upsert(&mut self, view: AlertView) {
        match self.history.binary_search_by_key(&view.id, |a| a.id) { Ok(i) => self.history[i] = view, Err(i) => self.history.insert(i, view) }
    }
    fn set_state(&mut self, id: AlertId, state: AlertState) -> Option<AlertView> {
        let i = self.history.binary_search_by_key(&id, |a| a.id).ok()?;
        self.history[i].state = state;
        Some(self.history[i])
    }
    pub(crate) fn get(&self, id: AlertId) -> Option<&AlertView> { self.history.binary_search_by_key(&id, |a| a.id).ok().map(|i| &self.history[i]) }
    /// Seconds of silence left at `tick`.
    pub(crate) fn silenced_s(&self, tick: u64) -> f64 { tick_s(self.silenced_until_tick.saturating_sub(tick)) }
}

/// Who an officer or ace line is from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Speaker { Officer(OfficerRole), Ace(content::ids::AceId) }

/// One officer or ace line before conversion to `OfficerLineDict`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LineFacts { pub speaker: Speaker, pub text: Msg, pub track: TrackId, pub tick: u64, pub new_speaker: bool }

/// A log line the walk wants written. `detail` is a second sentence appended to `text` as its last argument:
/// as a nested key when it has no arguments of its own, else translated in place.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LogDraft { pub kind: LogKind, pub tick: u64, pub text: Msg, pub detail: Option<Msg>, pub track: TrackId, pub part: Option<PartSlot>, pub vessel: Option<EntityId>, pub pos: Option<[f64; 2]>, pub silent: bool }
impl LogDraft {
    fn new(kind: LogKind, text: Msg) -> Self { Self { kind, tick: 0, text, detail: None, track: TrackId::NONE, part: None, vessel: None, pos: None, silent: false } }
    fn silent(mut self) -> Self { self.silent = true; self }
    fn track(mut self, t: TrackId) -> Self { self.track = t; self }
    fn part(mut self, p: PartSlot) -> Self { self.part = Some(p); self }
    fn vessel(mut self, v: EntityId) -> Self { self.vessel = Some(v); self }
    fn pos(mut self, p: Option<[f64; 2]>) -> Self { if self.pos.is_none() { self.pos = p; } self }
    fn detail(mut self, m: Msg) -> Self { self.detail = Some(m); self }
}

/// What one walked event asks of the Godot side.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Out {
    Log(LogDraft),
    TrackEvent { kind: &'static str, track: TrackId, other: TrackId },
    Control { role: ControlRole, holder: ControlHolder },
    Override { key: OverrideKey, overridden: bool },
    Line(LineFacts),
    AlertRaised(AlertView),
    Reply(QueryReply),
    /// A section changed through an event (silence, flood, alert state).
    Bump(Section),
    /// A replay opened or seeked: the log, lines, history and selection start again.
    Reset,
}

/// Cached getter results: (getter name, argument) to (combined revision, value).
pub(crate) type Memo = BTreeMap<(&'static str, i64), (u64, Variant)>;

/// Everything the core keeps besides the contract fields of [`BpHud`]. Plain data; `Default` is inert.
#[derive(Default)]
pub(crate) struct HudCore {
    /// Getter results per (name, argument) with the combined revision they were built at.
    pub(crate) memo: RefCell<Memo>,
    pub(crate) own_vessel: Option<EntityId>,
    pub(crate) own_side: Option<SideId>,
    pub(crate) own_slot: Option<PlayerSlot>,
    /// Design hash of the own hull (its `EntityRevealed`), looked up in `Services::designs` until found.
    pub(crate) own_hash: Option<ContentHash>,
    pub(crate) intros: BTreeMap<EntityId, IntroFacts>,
    pub(crate) players: BTreeMap<PlayerSlot, String>,
    pub(crate) alerts: AlertBook,
    /// Tick of the newest snapshot (sortie time of getters).
    pub(crate) tick: u64,
    /// Operation key of the sortie (`SortieIntro::operation`), for the station actions it offers.
    pub(crate) operation: String,
    /// Wall-clock seconds of this frame (`FrameFeed::now_s`).
    now_s: f64,
    last_state: SessionState,
    /// Newest event tick walked: older events are repeats (kill cam, seek) and announce nothing.
    watermark: Option<u64>,
    /// After a re-opening of the live stream (kill-cam clip, its end), events older than this repeat what was
    /// already walked: they build state but announce nothing, until the stream reaches it again.
    mute_below: Option<u64>,
    /// "Sortie started" is logged once per sortie, not again at every re-opening.
    started_logged: bool,
    /// Sections changed but held back by their rate, and when each was last bumped.
    pending: u32,
    last_bump_s: [f64; Section::COUNT],
    /// Own position and tick at the last tracks refresh.
    tracks_at: Option<(DVec2, u64)>,
    history_tick: Option<u64>,
    last_pacing: Option<Pacing>,
    /// Last logged engagement status per own mount: (engaging, track).
    mount_status: BTreeMap<PartSlot, (u8, TrackId)>,
    unconverged: BTreeMap<(PartSlot, TrackId), u64>,
    scope_sent: BTreeMap<(u32, u8), f64>,
    last_speaker: Option<(Speaker, u64)>,
    /// Roles the player holds as last announced, ascending; `None` until a hull is seen.
    announced_held: Option<Vec<ControlRole>>,
    /// Tick of the last `ControlChanged` per role: older snapshots do not contradict it.
    control_event_tick: BTreeMap<ControlRole, u64>,
    /// Damage facts only events carry (cut and jury-rigged conduits, flooded magazines). Unit `gd-hud-damage`.
    pub(crate) dc: damage::DcBook,
    /// The live-settings mirror (override flags and the server's override lists). Unit `gd-hud-board`.
    pub(crate) board: board::BoardBook,
}

fn msg_track(key: &str, t: TrackId) -> Msg { Msg::new(key).arg(MsgArg::Track(t)) }
fn word(key: TextKey) -> MsgArg { MsgArg::Key(key) }
fn word_key(kind: &str, value: i64) -> MsgArg { MsgArg::Key(TextKey::new(crate::text::word_key_of(kind, value).unwrap_or_default())) }
fn map_pos(p: glam::DVec3) -> Option<[f64; 2]> { (p.x.is_finite() && p.z.is_finite()).then_some([p.x, p.z]) }

impl HudCore {
    /// Forget everything of the last sortie.
    fn reset(&mut self) {
        let memo = std::mem::take(&mut self.memo);
        memo.borrow_mut().clear();
        let last_state = self.last_state;
        *self = HudCore { memo, last_state, ..HudCore::default() };
    }

    /// Own hull facts from the newest snapshot and the feed.
    fn observe_viewer(&mut self, snap: Option<&Snapshot>, intro: Option<&crate::seams::SortieIntro>, welcome: Option<&crate::seams::Welcome>) {
        if let Some(i) = intro {
            self.own_side = Some(i.side);
            if let Viewer::Player(p) = i.viewer { self.own_slot = Some(p); }
            if self.operation != i.operation { self.operation = i.operation.clone(); }
        }
        if self.own_slot.is_none() { if let Some(w) = welcome { self.own_slot = Some(w.slot); } }
        if let Some(ship) = snap.and_then(|s| s.ship.as_ref()) {
            if self.own_vessel != Some(ship.vessel) {
                self.own_vessel = Some(ship.vessel);
                self.own_hash = self.intros.get(&ship.vessel).map(|f| f.blueprint);
            }
        }
        if let Some(v) = self.own_vessel {
            if let Some(f) = self.intros.get(&v) { if self.own_side.is_none() { self.own_side = Some(f.side); } }
        }
    }

    fn own_side_is(&self, side: SideId) -> bool { self.own_side.is_none_or(|s| s == side) }
    fn is_own(&self, vessel: EntityId) -> bool { self.own_vessel == Some(vessel) }
    fn is_own_slot(&self, p: PlayerSlot) -> bool { self.own_slot.is_none_or(|s| s == p) }

    /// The track an entity is held as, from the snapshot or its introduction.
    fn track_of(&self, snap: Option<&Snapshot>, e: EntityId) -> TrackId {
        snap.and_then(|s| s.vessels.iter().find(|v| v.entity == e)).map(|v| v.track).filter(|t| !t.is_none())
            .or_else(|| self.intros.get(&e).map(|f| f.track))
            .unwrap_or(TrackId::NONE)
    }
    fn track_pos(snap: Option<&Snapshot>, t: TrackId) -> Option<[f64; 2]> {
        let s = snap?;
        let i = s.tracks.binary_search_by_key(&t, |v| v.id).ok()?;
        map_pos(s.tracks[i].pos)
    }
    fn vessel_pos(snap: Option<&Snapshot>, e: EntityId) -> Option<[f64; 2]> { snap?.vessels.iter().find(|v| v.entity == e).and_then(|v| map_pos(v.kin.pose.pos)) }
    fn alert_pos(&self, snap: Option<&Snapshot>, a: &AlertView) -> Option<[f64; 2]> {
        match alert_subject(a).0 { t if !t.is_none() => Self::track_pos(snap, t), _ => Self::vessel_pos(snap, a.vessel) }
    }

    /// Walk this frame's events in order. Every event updates state; `fresh` ones also log and announce.
    /// Events arrive in channel order, not strictly by tick, so only a re-opening `SortieStarted` behind the
    /// newest tick walked mutes: in a replay, a splash basin or the tutorial it is a seek, rewind or restart
    /// and everything starts again; elsewhere it opens a kill-cam clip (or ends one) and older events repeat.
    fn walk(&mut self, events: &[crate::seams::WireEvent], snap: Option<&Snapshot>, state: SessionState, kind: SessionKind, out: &mut Vec<Out>) {
        for e in events {
            if matches!(e.event, SimEvent::SortieStarted { .. }) {
                let behind = self.watermark.is_some_and(|w| e.tick < w);
                if state == SessionState::Replay || (behind && matches!(kind, SessionKind::Splash | SessionKind::Tutorial)) {
                    self.reset();
                    out.push(Out::Reset);
                } else if behind {
                    self.mute_below = self.watermark;
                }
            }
            let fresh = match self.mute_below {
                Some(w) if e.tick < w => false,
                Some(_) => { self.mute_below = None; true }
                None => true,
            };
            if fresh { self.watermark = Some(self.watermark.map_or(e.tick, |w| w.max(e.tick))); }
            self.event(e.tick, &e.event, snap, fresh, out);
        }
    }

    fn event(&mut self, tick: u64, event: &SimEvent, snap: Option<&Snapshot>, fresh: bool, out: &mut Vec<Out>) {
        self.dc.observe(self.own_vessel, tick, event, out);
        self.board.observe(self.own_vessel, self.own_slot, tick, event, fresh, out);
        let log = |out: &mut Vec<Out>, mut d: LogDraft| if fresh { d.tick = tick; out.push(Out::Log(d)); };
        match event {
            SimEvent::SortieStarted { .. } => {
                if fresh && !self.started_logged {
                    self.started_logged = true;
                    log(out, LogDraft::new(LogKind::System, Msg::new("log.system.started")).silent());
                }
            }
            SimEvent::PacingChanged(p) => {
                let prev = self.last_pacing.replace(p.clone());
                if let Some(d) = pacing_entry(prev.as_ref(), p) { log(out, d); }
            }
            SimEvent::LadderStep { rung, sub_step, up } => log(out, LogDraft::new(LogKind::Pacing, ladder_msg(*rung, *sub_step, *up))),
            SimEvent::SortieEnded(summary) => {
                log(out, LogDraft::new(LogKind::System, Msg::new("log.system.ended").arg(word_key("outcome", summary.outcome.to_gd()))));
            }
            SimEvent::PlayerJoined { player, name, .. } => {
                self.players.insert(*player, name.clone());
                if !self.is_own_slot(*player) || self.own_slot.is_none() { log(out, LogDraft::new(LogKind::Network, Msg::new("log.network.joined").arg(MsgArg::Name(name.clone())))); }
                out.push(Out::Bump(Section::Allies));
            }
            SimEvent::PlayerDropped { player, officers_in_command } => {
                let name = self.players.get(player).cloned().unwrap_or_default();
                let key = if *officers_in_command { "log.network.dropped" } else { "log.network.left" };
                log(out, LogDraft::new(LogKind::Network, Msg::new(key).arg(MsgArg::Name(name))));
                out.push(Out::Bump(Section::Allies));
            }
            SimEvent::CommandRejected { player, reason, .. } => {
                if self.is_own_slot(*player) { log(out, LogDraft::new(LogKind::System, Msg::new("log.system.rejected")).detail(sim_systems::text::reject(*reason)).silent()); }
            }
            SimEvent::QueryReply { player, reply } => {
                if fresh && self.is_own_slot(*player) {
                    if let QueryReply::Scope { port, .. } = reply {
                        let k = (port.node.0, port.port);
                        let last = self.scope_sent.get(&k).copied();
                        if last.is_some_and(|t| self.now_s - t < SCOPE_GAP_S) { return; }
                        self.scope_sent.insert(k, self.now_s);
                    }
                    out.push(Out::Reply(reply.clone()));
                }
            }
            SimEvent::EntityRevealed(i) => {
                let facts = IntroFacts { blueprint: i.blueprint, side: i.side, name: i.name.clone(), player: i.player, class: i.class.clone(), track: i.track };
                if self.own_vessel == Some(i.entity) && self.own_hash != Some(i.blueprint) { self.own_hash = Some(i.blueprint); out.push(Out::Bump(Section::Static)); }
                if let Some(p) = i.player { if !i.name.is_empty() { self.players.entry(p).or_insert_with(|| i.name.clone()); } }
                self.intros.insert(i.entity, facts);
            }
            SimEvent::EntityHidden { .. } => {}
            SimEvent::Phys(p) => self.phys(p, snap, &mut |d| log(out, d)),
            SimEvent::TrackCreated { side, track, domain } => {
                if !self.own_side_is(*side) { return; }
                if fresh { out.push(Out::TrackEvent { kind: "created", track: *track, other: TrackId::NONE }); }
                log(out, LogDraft::new(LogKind::Track, msg_track("log.track.created", *track).arg(word_key("domain", domain.to_gd()))).track(*track).pos(Self::track_pos(snap, *track)).silent());
            }
            SimEvent::TrackDropped { side, track } => {
                if !self.own_side_is(*side) { return; }
                if fresh { out.push(Out::TrackEvent { kind: "dropped", track: *track, other: TrackId::NONE }); }
                log(out, LogDraft::new(LogKind::Track, msg_track("log.track.dropped", *track)).track(*track).pos(Self::track_pos(snap, *track)).silent());
            }
            SimEvent::TrackMerged { side, from, into } => {
                if !self.own_side_is(*side) { return; }
                if fresh { out.push(Out::TrackEvent { kind: "merged", track: *from, other: *into }); }
                log(out, LogDraft::new(LogKind::Track, msg_track("log.track.merged", *from).arg(MsgArg::Track(*into))).track(*into).pos(Self::track_pos(snap, *into)));
            }
            SimEvent::TrackSplit { side, from, new } => {
                if !self.own_side_is(*side) { return; }
                if fresh { out.push(Out::TrackEvent { kind: "split", track: *from, other: *new }); }
                log(out, LogDraft::new(LogKind::Track, msg_track("log.track.split", *from).arg(MsgArg::Track(*new))).track(*new).pos(Self::track_pos(snap, *new)));
            }
            SimEvent::RungChanged { side, track, rung, reason, class } => {
                if !self.own_side_is(*side) { return; }
                if fresh { out.push(Out::TrackEvent { kind: "rung", track: *track, other: TrackId::NONE }); }
                let name = class.as_ref().and_then(services_class_name);
                let text = match name {
                    Some(n) => msg_track("log.track.rung_class", *track).arg(MsgArg::Key(n)),
                    None => msg_track("log.track.rung", *track).arg(word(sim_systems::text::rung_word(*rung))),
                };
                log(out, LogDraft::new(LogKind::Track, text).detail(sim_systems::text::rung_reason(*reason)).track(*track).pos(Self::track_pos(snap, *track)).silent());
            }
            SimEvent::StanceChanged { side, track, stance, reason } => {
                if !self.own_side_is(*side) { return; }
                if fresh { out.push(Out::TrackEvent { kind: "stance", track: *track, other: TrackId::NONE }); }
                let d = LogDraft::new(LogKind::Track, msg_track("log.track.stance", *track).arg(word(sim_systems::text::stance_word(*stance))))
                    .detail(sim_systems::text::stance_reason(*reason)).track(*track).pos(Self::track_pos(snap, *track));
                // A track turning Hostile is news; the rest is routine.
                log(out, if *stance == Stance::Hostile { d } else { d.silent() });
            }
            SimEvent::EsmWarning { vessel, .. } => { if self.is_own(*vessel) { out.push(Out::Bump(Section::Board)); } }
            SimEvent::Radiating { vessel, slot, on } => {
                if !self.is_own(*vessel) { return; }
                let key = if *on { "log.officer.radiating" } else { "log.officer.silent" };
                log(out, LogDraft::new(LogKind::Officer, Msg::new(key).arg(MsgArg::Part(*slot))).part(*slot).vessel(*vessel).silent());
                out.push(Out::Bump(Section::Board));
            }
            SimEvent::Engaging { mount, track, reason } => {
                if !self.is_own(mount.vessel) { return; }
                if self.mount_status.insert(mount.slot, (1, *track)) == Some((1, *track)) { return; }
                let reason_msg = sim_systems::text::fire_reason(reason);
                log(out, LogDraft::new(LogKind::Engagement, Msg::new("log.engage.engaging").arg(MsgArg::Part(mount.slot)).arg(MsgArg::Track(*track)))
                    .detail(reason_msg).part(mount.slot).track(*track).pos(Self::track_pos(snap, *track)).silent());
            }
            SimEvent::Holding { mount, track, reason } => {
                if !self.is_own(mount.vessel) { return; }
                if self.mount_status.insert(mount.slot, (2, *track)) == Some((2, *track)) { return; }
                let text = if track.is_none() { Msg::new("log.engage.holding_none").arg(MsgArg::Part(mount.slot)) } else { Msg::new("log.engage.holding").arg(MsgArg::Part(mount.slot)).arg(MsgArg::Track(*track)) };
                log(out, LogDraft::new(LogKind::Engagement, text).detail(sim_systems::text::hold_reason(reason)).part(mount.slot).track(*track).pos(Self::track_pos(snap, *track)).silent());
            }
            SimEvent::SolutionUnconverged { mount, track, error_m } => {
                if !self.is_own(mount.vessel) { return; }
                let k = (mount.slot, *track);
                if self.unconverged.get(&k).is_some_and(|t| tick.saturating_sub(*t) < UNCONVERGED_GAP_TICKS) { return; }
                self.unconverged.insert(k, tick);
                let err = if error_m.is_finite() { *error_m as f64 } else { 0.0 };
                log(out, LogDraft::new(LogKind::Engagement, Msg::new("log.engage.unconverged").arg(MsgArg::Part(mount.slot)).arg(MsgArg::Track(*track)).num(err, Unit::Metres)).part(mount.slot).track(*track).silent());
            }
            SimEvent::Seeker { .. } | SimEvent::ShotLogged(_) | SimEvent::TickNotReproducible { .. } | SimEvent::EnemyAlert { .. } | SimEvent::KillCam { .. } => {}
            SimEvent::Seduced { .. } => log(out, LogDraft::new(LogKind::Engagement, Msg::new("log.engage.seduced")).silent()),
            SimEvent::OfficerMessage { vessel, role, text } => {
                if !self.is_own(*vessel) { return; }
                if fresh { out.push(Out::Line(self.line(Speaker::Officer(*role), text.clone(), TrackId::NONE, tick))); }
                log(out, LogDraft::new(LogKind::Officer, text.clone()).vessel(*vessel).silent());
            }
            SimEvent::AceMessage { ace, text, from, .. } => {
                let track = from.unwrap_or(TrackId::NONE);
                if fresh { out.push(Out::Line(self.line(Speaker::Ace(ace.clone()), text.clone(), track, tick))); }
                log(out, LogDraft::new(LogKind::Ace, text.clone()).track(track).pos(Self::track_pos(snap, track)));
            }
            SimEvent::ControlChanged { vessel, role, holder } => {
                if !self.is_own(*vessel) { return; }
                let t = self.control_event_tick.entry(*role).or_insert(tick);
                *t = (*t).max(tick);
                self.note_control(*role, *holder, tick, fresh, out);
            }
            SimEvent::OverrideChanged { vessel, key, overridden } => {
                if !self.is_own(*vessel) { return; }
                if fresh { out.push(Out::Override { key: key.clone(), overridden: *overridden }); }
                let k = if *overridden { "log.control.overridden" } else { "log.control.reverted" };
                log(out, LogDraft::new(LogKind::Control, Msg::new(k)).vessel(*vessel).silent());
                out.push(Out::Bump(Section::Board));
            }
            SimEvent::ScriptGreyedOut { vessel, fault, .. } => {
                if self.is_own(*vessel) { log(out, LogDraft::new(LogKind::System, Msg::new("log.system.script")).detail(sim_systems::text::script_fault(*fault)).vessel(*vessel)); }
            }
            SimEvent::AlertRaised(a) | SimEvent::AlertEscalated(a) => {
                if !self.alert_is_ours(a) { return; }
                self.alerts.upsert(*a);
                out.push(Out::Bump(Section::Alerts));
                if fresh { out.push(Out::AlertRaised(*a)); }
                let (track, part, _) = alert_subject(a);
                let mut d = LogDraft::new(LogKind::Alert, sim_systems::text::alert(&a.kind, a.priority)).track(track).vessel(a.vessel).pos(self.alert_pos(snap, a));
                if let Some(p) = part { d = d.part(p); }
                log(out, if a.state == AlertState::LogOnly { d.silent() } else { d });
            }
            SimEvent::AlertChanged { id, state } => {
                let Some(a) = self.alerts.set_state(*id, *state) else { return };
                out.push(Out::Bump(Section::Alerts));
                // Rectified lower alerts go to the log unread (L1687); Alarms and Emergencies show "Cleared".
                if *state == AlertState::Cleared {
                    let (track, part, _) = alert_subject(&a);
                    let mut d = LogDraft::new(LogKind::Alert, Msg::new("log.alert.cleared")).detail(sim_systems::text::alert(&a.kind, a.priority)).track(track).vessel(a.vessel);
                    if let Some(p) = part { d = d.part(p); }
                    log(out, if a.priority >= AlertPriority::Alarm { d.silent() } else { d });
                }
            }
            SimEvent::AlertsSilenced { vessel, until_tick } => {
                if !self.is_own(*vessel) && self.own_vessel.is_some() { return; }
                self.alerts.silenced_until_tick = *until_tick;
                out.push(Out::Bump(Section::Alerts));
                log(out, LogDraft::new(LogKind::Alert, Msg::new("log.alert.silenced").num(tick_s(until_tick.saturating_sub(tick)).round(), Unit::Seconds)).silent());
            }
            SimEvent::AlertFlood { vessel, on } => {
                if !self.is_own(*vessel) && self.own_vessel.is_some() { return; }
                if self.alerts.flood == *on { return; }
                self.alerts.flood = *on;
                out.push(Out::Bump(Section::Alerts));
                log(out, LogDraft::new(LogKind::Alert, Msg::new(if *on { "log.alert.flood_on" } else { "log.alert.flood_off" })));
            }
            SimEvent::ObjectiveChanged { id, state, .. } => {
                out.push(Out::Bump(Section::Objectives));
                let obj = snap.and_then(|s| s.objectives.iter().find(|o| o.id == *id));
                let ghost = obj.is_some_and(|o| o.ghost);
                use sim_systems::protocol::ObjectiveState as O;
                let key = match (state, ghost) {
                    (O::Done, false) => "log.objective.done", (O::Done, true) => "log.objective.ghost_done",
                    (O::Failed, false) => "log.objective.failed", (O::Failed, true) => "log.objective.ghost_failed",
                    (O::Active, _) | (O::Hidden, _) => return,
                };
                let mut d = LogDraft::new(LogKind::Objective, Msg::new(key)).pos(obj.and_then(|o| o.marker).map(|m| [m.x, m.y]));
                d.detail = Some(obj.map(|o| o.text.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| Msg::new("log.objective.unnamed")));
                log(out, d);
            }
            SimEvent::ActionChanged(a) => {
                if !self.is_own(a.actor) { return; }
                out.push(Out::Bump(Section::Actions));
                use sim_systems::protocol::ActionState as A;
                let action = word_key("station", a.action.to_gd());
                let (key, loud) = match a.state { A::Done => ("log.action.done", true), A::Aborted => ("log.action.aborted", true), A::Paused => ("log.action.paused", false), A::Running => ("log.action.running", false) };
                let mut d = LogDraft::new(LogKind::Objective, Msg::new(key).arg(action)).vessel(a.actor).pos(a.at.map(|p| [p.x, p.y]));
                if matches!(a.state, A::Aborted | A::Paused) { d.detail = Some(a.block.map(sim_systems::text::action_block).unwrap_or_else(|| Msg::new("log.action.no_reason"))); }
                log(out, if loud { d } else { d.silent() });
            }
            SimEvent::PhotoTaken { by, track } => {
                if self.is_own(*by) { log(out, LogDraft::new(LogKind::Objective, msg_track("log.action.photo", *track)).track(*track).pos(Self::track_pos(snap, *track))); }
            }
            SimEvent::PrizeTaken { prize, by, class } => {
                if !self.is_own_slot(*by) { return; }
                let text = match class.as_ref().and_then(services_class_name) { Some(n) => Msg::new("log.action.prize_class").arg(MsgArg::Key(n)), None => Msg::new("log.action.prize") };
                log(out, LogDraft::new(LogKind::Objective, text).vessel(*prize).pos(Self::vessel_pos(snap, *prize)));
                out.push(Out::Bump(Section::Allies));
            }
            SimEvent::Reinforcements { side, at } => {
                if self.own_side.is_some_and(|s| s == *side) { return; }
                log(out, LogDraft::new(LogKind::Track, Msg::new("log.track.reinforcements")).pos(map_pos(*at)));
            }
        }
    }

    /// Physical events the log reports: the own hull's damage and the fate of other hulls.
    fn phys(&mut self, p: &PhysEvent, snap: Option<&Snapshot>, log: &mut dyn FnMut(LogDraft)) {
        let own = |v: &EntityId| self.own_vessel == Some(*v);
        let comp = |c: &CompartmentId| MsgArg::Compartment(*c);
        let damage = |text: Msg, v: EntityId| LogDraft::new(LogKind::Damage, text).vessel(v).silent();
        match p {
            PhysEvent::PartDamaged { vessel, slot, level } if own(vessel) => {
                log(damage(Msg::new("log.damage.part").arg(MsgArg::Part(*slot)).arg(word_key("damage", level.to_gd())), *vessel).part(*slot));
            }
            PhysEvent::Breach { vessel, compartment, below_waterline } if own(vessel) => {
                let key = if *below_waterline { "log.damage.breach_below" } else { "log.damage.breach" };
                log(damage(Msg::new(key).arg(comp(compartment)), *vessel));
            }
            PhysEvent::ConduitCut { vessel, conduit } if own(vessel) => log(damage(Msg::new("log.damage.conduit").arg(MsgArg::Int(*conduit as i64 + 1)), *vessel)),
            PhysEvent::FireStarted { vessel, compartment, .. } if own(vessel) => log(damage(Msg::new("log.damage.fire").arg(comp(compartment)), *vessel)),
            PhysEvent::FireOut { vessel, compartment, .. } if own(vessel) => log(damage(Msg::new("log.damage.fire_out").arg(comp(compartment)), *vessel)),
            PhysEvent::CompartmentHalfFlooded { vessel, compartment } if own(vessel) => log(damage(Msg::new("log.damage.half_flooded").arg(comp(compartment)), *vessel)),
            PhysEvent::MagazineFlooded { vessel, magazine } if own(vessel) => log(damage(Msg::new("log.damage.magazine_flooded").arg(MsgArg::Part(*magazine)), *vessel).part(*magazine)),
            PhysEvent::CookOff { vessel, magazine, .. } if own(vessel) => {
                let mut d = damage(Msg::new("log.damage.cook_off").arg(MsgArg::Part(*magazine)), *vessel).part(*magazine);
                d.silent = false;
                log(d);
            }
            PhysEvent::PassengersLost { vessel, count } if own(vessel) => {
                let mut d = damage(Msg::new("log.damage.passengers").arg(MsgArg::Int(*count as i64)), *vessel);
                d.silent = false;
                log(d);
            }
            PhysEvent::JobDone { vessel, job } if own(vessel) => {
                use sim_core::crew::JobDone as J;
                let text = match job {
                    J::ConduitJuryRigged(c) => Msg::new("log.damage.jury_rigged").arg(MsgArg::Int(*c as i64 + 1)),
                    J::FacePatched(c) => Msg::new("log.damage.patched").arg(comp(c)),
                    J::PartRepaired(s) => Msg::new("log.damage.repaired").arg(MsgArg::Part(*s)),
                    J::FireFought(c) => Msg::new("log.damage.fire_fought").arg(comp(c)),
                };
                log(damage(text, *vessel));
            }
            PhysEvent::Grounded { vessel, point } if own(vessel) => log(damage(Msg::new("log.damage.grounded"), *vessel).pos(map_pos(*point))),
            PhysEvent::Collision { a, b, point, .. } if own(a) || own(b) => log(damage(Msg::new("log.damage.collision"), if own(a) { *a } else { *b }).pos(map_pos(*point))),
            PhysEvent::VesselLost { vessel, side, .. } => {
                if own(vessel) {
                    log(LogDraft::new(LogKind::Damage, Msg::new("log.damage.own_lost")).vessel(*vessel));
                } else if self.own_side.is_some_and(|s| s != *side) {
                    let t = self.track_of(snap, *vessel);
                    let text = if t.is_none() { Msg::new("log.engage.lost_vessel") } else { msg_track("log.engage.lost", t) };
                    log(LogDraft::new(LogKind::Engagement, text).track(t).vessel(*vessel).pos(Self::vessel_pos(snap, *vessel)));
                } else {
                    let name = self.intros.get(vessel).map(|f| f.name.clone()).unwrap_or_default();
                    log(LogDraft::new(LogKind::Damage, Msg::new("log.damage.ally_lost").arg(MsgArg::Name(name))).vessel(*vessel).pos(Self::vessel_pos(snap, *vessel)));
                }
            }
            PhysEvent::StatusChanged { vessel, status: sim_core::vessel::VesselStatus::Disabled }
                if !own(vessel) && self.intros.get(vessel).is_some_and(|f| self.own_side.is_some_and(|s| s != f.side)) => {
                let t = self.track_of(snap, *vessel);
                // A hull that struck its colours under the surrender rule logs as struck, not as disabled.
                let struck = snap.and_then(|s| s.vessels.iter().find(|v| v.entity == *vessel)).is_some_and(|v| v.flags.contains(sim_systems::protocol::snapshot::VesselFlags::STRUCK_COLOURS));
                let key = if struck { "log.engage.struck" } else { "log.engage.disabled" };
                if !t.is_none() { log(LogDraft::new(LogKind::Engagement, msg_track(key, t)).track(t).vessel(*vessel).pos(Self::vessel_pos(snap, *vessel))); }
            }
            _ => {}
        }
    }

    /// A role changed hands: announce it once, whether the event or the held list told it first.
    fn note_control(&mut self, role: ControlRole, holder: ControlHolder, tick: u64, fresh: bool, out: &mut Vec<Out>) {
        let held = self.announced_held.get_or_insert_with(Vec::new);
        let want = holder == ControlHolder::Player;
        if held.contains(&role) == want { return; }
        if want { held.push(role); held.sort_unstable(); } else { held.retain(|r| *r != role); }
        out.push(Out::Bump(Section::Board));
        out.push(Out::Bump(Section::Mounts));
        if !fresh { return; }
        out.push(Out::Control { role, holder });
        let (text, part) = control_text(role, holder);
        let mut d = LogDraft::new(LogKind::Control, text);
        d.tick = tick;
        if let Some(v) = self.own_vessel { d = d.vessel(v); }
        if let Some(p) = part { d = d.part(p); }
        out.push(Out::Log(d));
    }

    /// Takeovers and releases the server reports only through the held list (the simulation announces
    /// `ControlChanged` for handbacks it makes itself). Snapshot ticks lag events, so a role an event moved
    /// more recently than this snapshot is left alone.
    fn observe_held(&mut self, snap: &Snapshot, out: &mut Vec<Out>) {
        let Some(ship) = snap.ship.as_ref() else { self.announced_held = None; return };
        let Some(prev) = self.announced_held.clone() else {
            let mut held = ship.held.clone();
            held.sort_unstable();
            self.announced_held = Some(held);
            return;
        };
        let recent = |r: &ControlRole| self.control_event_tick.get(r).is_some_and(|t| *t > snap.tick);
        let taken: Vec<ControlRole> = ship.held.iter().filter(|r| !prev.contains(r) && !recent(r)).copied().collect();
        let released: Vec<ControlRole> = prev.iter().filter(|r| !ship.held.contains(r) && !recent(r)).copied().collect();
        let fresh = self.mute_below.is_none();
        for r in taken { self.note_control(r, ControlHolder::Player, snap.tick, fresh, out); }
        for r in released { self.note_control(r, ControlHolder::Officer, snap.tick, fresh, out); }
    }

    fn alert_is_ours(&self, a: &AlertView) -> bool { self.own_vessel.is_none_or(|v| v == a.vessel) }

    fn line(&mut self, speaker: Speaker, text: Msg, track: TrackId, tick: u64) -> LineFacts {
        let new_speaker = match &self.last_speaker { Some((s, t)) => *s != speaker || tick.saturating_sub(*t) >= SPEAKER_GAP_TICKS, None => true };
        self.last_speaker = Some((speaker.clone(), tick));
        LineFacts { speaker, text, track, tick, new_speaker }
    }
}

/// The class's name key from the campaign data.
fn services_class_name(c: &content::ids::ClassId) -> Option<TextKey> { services().campaign().classes.get(c).map(|d| d.name.clone()) }

/// A pacing log line when something the player sees changed (every change shows in HUD and log, L1025).
fn pacing_entry(prev: Option<&Pacing>, p: &Pacing) -> Option<LogDraft> {
    let new = |key: &str| LogDraft::new(LogKind::Pacing, Msg::new(key));
    let Some(prev) = prev else {
        // The first pacing of a sortie is only news when it is not the plain 1x start.
        return if p.paused { Some(new("log.pacing.paused")) } else { None };
    };
    if prev.paused != p.paused { return Some(new(if p.paused { "log.pacing.paused" } else { "log.pacing.resumed" })); }
    if prev.effective != p.effective { return Some(LogDraft::new(LogKind::Pacing, Msg::new("log.pacing.rate").arg(MsgArg::Int(crate::enums::time_rate_to_gd(p.effective))))); }
    if prev.blocked != p.blocked {
        return Some(match p.blocked { Some(b) => new("log.pacing.blocked").detail(sim_systems::text::compression_block(b)).silent(), None => new("log.pacing.unblocked").silent() });
    }
    if (prev.time_scale - p.time_scale).abs() > 1e-3 && p.time_scale.is_finite() {
        return Some(LogDraft::new(LogKind::Pacing, Msg::new("log.pacing.scale").num((p.time_scale as f64 * 100.0).round(), Unit::Percent)));
    }
    None
}

/// Log text of a role changing hands, and the part it names.
fn control_text(role: ControlRole, holder: ControlHolder) -> (Msg, Option<PartSlot>) {
    let player = holder == ControlHolder::Player;
    let pick = |take: &str, back: &str| Msg::new(if player { take } else { back });
    match role {
        ControlRole::Helm => (pick("log.control.take.helm", "log.control.back.helm"), None),
        ControlRole::Sensors => (pick("log.control.take.sensors", "log.control.back.sensors"), None),
        ControlRole::Mount(s) => (pick("log.control.take.mount", "log.control.back.mount").arg(MsgArg::Part(s)), Some(s)),
        ControlRole::WeaponGroup(g) => (pick("log.control.take.group", "log.control.back.group").arg(MsgArg::Int(g as i64 + 1)), None),
    }
}

/// Track, part and compartment an alert is about.
pub(crate) fn alert_subject(a: &AlertView) -> (TrackId, Option<PartSlot>, Option<CompartmentId>) {
    use sim_systems::protocol::AlertKind as K;
    match a.kind {
        K::WeaponInbound { track } | K::TorpedoInbound { track } | K::NewHostileTrack { track } => (track, None, None),
        K::Flooding { compartment } | K::FloodingSevere { compartment } | K::Fire { compartment } => (TrackId::NONE, None, Some(compartment)),
        K::MagazineFire { magazine, .. } => (TrackId::NONE, Some(magazine), None),
        K::PartHeavyDamage { slot } | K::PartDestroyed { slot } | K::PartOnBuffer { slot, .. } | K::PartLocalControl { slot } | K::AmmunitionLow { slot } => (TrackId::NONE, Some(slot), None),
        _ => (TrackId::NONE, None, None),
    }
}

/// Which sections differ between two snapshots, as a bit set of `Section::bit`. Rates are applied later.
pub(crate) fn diff_sections(prev: Option<&Snapshot>, next: Option<&Snapshot>, tracks_at: Option<(DVec2, u64)>) -> u32 {
    let (Some(p), Some(n)) = (prev, next) else {
        return if prev.is_none() && next.is_none() { 0 } else { u32::MAX >> (32 - Section::COUNT) };
    };
    let mut bits = 0u32;
    let mut set = |s: Section, changed: bool| if changed { bits |= s.bit() };
    match (p.ship.as_ref(), n.ship.as_ref()) {
        (Some(a), Some(b)) => {
            let (va, vb) = (own_view_in(p, a.vessel), own_view_in(n, b.vessel));
            set(Section::Static, a.vessel != b.vessel);
            set(Section::Motion, motion_key(a, va) != motion_key(b, vb));
            set(Section::Power, a.power != b.power || shed_count(a) != shed_count(b));
            set(Section::Parts, a.parts != b.parts);
            set(Section::Mounts, a.mounts != b.mounts || a.held != b.held || mount_readouts(a) != mount_readouts(b) || va.map(|v| &v.turrets) != vb.map(|v| &v.turrets));
            set(Section::Compartments, a.compartments != b.compartments);
            set(Section::Crew, a.crew != b.crew);
            set(Section::Networks, links(a) != links(b) || a.crew.teams != b.crew.teams);
            // A player-held helm reports the ship's real ordered course and speed on the board (`board::player_helm_lines`).
            let manual_helm = |s: &ShipStatus| s.held.contains(&sim_systems::protocol::ControlRole::Helm).then_some((s.ordered_speed_kn, s.ordered_heading_rad));
            set(Section::Board, a.board != b.board || a.held != b.held || manual_helm(a) != manual_helm(b));
            set(Section::Actions, a.actions != b.actions || levels(a) != levels(b));
            set(Section::Offboard, a.offboard != b.offboard);
        }
        (None, None) => {}
        _ => { for s in Section::OWN_SHIP { set(s, true); } }
    }
    let own_now = n.ship.as_ref().and_then(|s| own_view_in(n, s.vessel)).map(|v| DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z));
    let moved = match (tracks_at, own_now) { (Some((at, _)), Some(now)) => at.distance(now) > TRACKS_MOVE_M, (None, Some(_)) => true, _ => false };
    let aged = tracks_at.is_some_and(|(_, t)| n.tick.saturating_sub(t) >= TICK_HZ) && !n.tracks.is_empty();
    let designated = |s: &Snapshot| s.ship.as_ref().map(|x| x.designated);
    set(Section::Tracks, p.tracks != n.tracks || designated(p) != designated(n) || moved || aged);
    set(Section::Alerts, p.alerts != n.alerts);
    set(Section::Objectives, p.objectives != n.objectives);
    let ally_poses = |s: &Snapshot| s.allies.iter().map(|a| s.vessels.iter().find(|v| v.entity == a.vessel).map(|v| v.kin.pose)).collect::<Vec<_>>();
    set(Section::Allies, p.allies != n.allies || ally_poses(p) != ally_poses(n));
    bits
}

fn own_view_in(s: &Snapshot, vessel: EntityId) -> Option<&VesselView> { s.vessels.iter().find(|v| v.entity == vessel) }
#[derive(PartialEq)]
struct MotionKey<'a> { speed: f32, heading: f32, heel: f32, trim: f32, os: f32, oh: f32, route: &'a [DVec2], designated: TrackId, envelope: f32, lost: f32, pose: Option<(glam::DVec3, f32, f32, sim_core::vessel::VesselStatus)> }
fn motion_key<'a>(s: &'a ShipStatus, v: Option<&'a VesselView>) -> MotionKey<'a> {
    MotionKey { speed: s.speed_kn, heading: s.heading_rad, heel: s.heel_deg, trim: s.trim_deg, os: s.ordered_speed_kn, oh: s.ordered_heading_rad, route: &s.route, designated: s.designated, envelope: s.longest_envelope_m, lost: s.cells_lost, pose: v.map(|v| (v.kin.pose.pos, v.rudder, v.power, v.status)) }
}
fn shed_count(s: &ShipStatus) -> usize { s.parts.iter().filter(|p| p.shed).count() }
fn mount_readouts(s: &ShipStatus) -> Vec<sim_systems::protocol::PartReadout> {
    s.mounts.iter().filter_map(|m| s.parts.iter().find(|p| p.slot == m.slot).map(|p| p.readout)).collect()
}
fn links(s: &ShipStatus) -> Vec<(sim_core::networks::LinkState, sim_core::networks::LinkState, sim_core::networks::LinkState, bool)> { s.parts.iter().map(|p| (p.power, p.fuel, p.data, p.shed)).collect() }
fn levels(s: &ShipStatus) -> Vec<content::enums::DamageLevel> { s.parts.iter().map(|p| p.level).collect() }

/// Pure log operations (the log lives in `BpHud::log`).
pub(crate) mod logbook {
    use super::*;

    /// Optional filters of `log_entries`.
    #[derive(Clone, Debug, Default, PartialEq)]
    pub(crate) struct Filter { pub kinds: Option<Vec<LogKind>>, pub track: Option<TrackId>, pub part: Option<PartSlot>, pub vessel: Option<EntityId>, pub unread_only: bool }
    impl Filter {
        pub(crate) fn admits(&self, e: &LogEntry) -> bool {
            self.kinds.as_ref().is_none_or(|k| k.contains(&e.kind))
                && self.track.is_none_or(|t| e.track == t)
                && self.part.is_none_or(|p| e.part == Some(p))
                && self.vessel.is_none_or(|v| e.vessel == Some(v))
                && (!self.unread_only || e.unread)
        }
    }

    /// Append an entry with the next sequence number; evict past [`LOG_CAP`].
    pub(crate) fn push(log: &mut Vec<LogEntry>, next_seq: &mut u64, mut entry: LogEntry) -> u64 {
        entry.seq = *next_seq;
        *next_seq += 1;
        let seq = entry.seq;
        log.push(entry);
        if log.len() > LOG_CAP {
            let i = log.iter().position(|e| e.silent).unwrap_or(0);
            log.remove(i);
        }
        seq
    }

    /// Matches after `after_seq`, the newest `limit` of them, oldest first.
    pub(crate) fn select<'a>(log: &'a [LogEntry], filter: &Filter, after_seq: u64, limit: usize) -> Vec<&'a LogEntry> {
        let start = log.partition_point(|e| e.seq <= after_seq);
        let mut picked: Vec<&LogEntry> = log[start..].iter().rev().filter(|e| filter.admits(e)).take(limit).collect();
        picked.reverse();
        picked
    }

    pub(crate) fn unread(log: &[LogEntry]) -> usize { log.iter().filter(|e| e.unread && !e.silent).count() }

    /// Mark entries up to and including `seq` as read; true when anything changed.
    pub(crate) fn mark_read(log: &mut [LogEntry], seq: u64) -> bool {
        let end = log.partition_point(|e| e.seq <= seq);
        let mut changed = false;
        for e in &mut log[..end] { if e.unread { e.unread = false; changed = true; } }
        changed
    }
}

/// Opaque `OverrideDict.id` of an override key, and back (`Orders.revert`). Hand-written so no format
/// dependency is needed; a slider key is free text and always the tail.
pub(crate) mod override_id {
    use super::*;

    fn sink(s: &SinkKind) -> String {
        let slot = |p: &PartSlot| p.0.to_string();
        match s {
            SinkKind::HelmHeading => "helm_heading".into(), SinkKind::HelmSpeed => "helm_speed".into(),
            SinkKind::MountAimPoint(p) => format!("mount_aim_point/{}", slot(p)), SinkKind::MountFirePermission(p) => format!("mount_fire_permission/{}", slot(p)),
            SinkKind::MissileLaunch(p) => format!("missile_launch/{}", slot(p)), SinkKind::MissileTarget(p) => format!("missile_target/{}", slot(p)),
            SinkKind::EmissionState(p) => format!("emission_state/{}", slot(p)), SinkKind::PowerPriority(p) => format!("power_priority/{}", slot(p)),
            SinkKind::Pump(p) => format!("pump/{}", slot(p)), SinkKind::Door(p) => format!("door/{}", slot(p)),
            SinkKind::FloodValve(c) => format!("flood_valve/{}", c.0),
            SinkKind::TrackStanceOverride => "track_stance_override".into(), SinkKind::TrackIdOverride => "track_id_override".into(),
            SinkKind::ShipPreset => "ship_preset".into(), SinkKind::ShipEmcon => "ship_emcon".into(),
        }
    }
    fn sink_from(s: &str) -> Option<SinkKind> {
        let (name, arg) = match s.split_once('/') { Some((n, a)) => (n, Some(a)), None => (s, None) };
        let slot = || arg.and_then(|a| a.parse::<u16>().ok()).map(PartSlot);
        let plain = |k: SinkKind| arg.is_none().then_some(k);
        match name {
            "helm_heading" => plain(SinkKind::HelmHeading), "helm_speed" => plain(SinkKind::HelmSpeed),
            "mount_aim_point" => slot().map(SinkKind::MountAimPoint), "mount_fire_permission" => slot().map(SinkKind::MountFirePermission),
            "missile_launch" => slot().map(SinkKind::MissileLaunch), "missile_target" => slot().map(SinkKind::MissileTarget),
            "emission_state" => slot().map(SinkKind::EmissionState), "power_priority" => slot().map(SinkKind::PowerPriority),
            "pump" => slot().map(SinkKind::Pump), "door" => slot().map(SinkKind::Door),
            "flood_valve" => arg.and_then(|a| a.parse::<u16>().ok()).map(|c| SinkKind::FloodValve(CompartmentId(c))),
            "track_stance_override" => plain(SinkKind::TrackStanceOverride), "track_id_override" => plain(SinkKind::TrackIdOverride),
            "ship_preset" => plain(SinkKind::ShipPreset), "ship_emcon" => plain(SinkKind::ShipEmcon),
            _ => None,
        }
    }

    pub(crate) fn encode(key: &OverrideKey) -> String {
        match key {
            OverrideKey::Preset => "preset".into(),
            OverrideKey::MountPreset(p) => format!("mount_preset/{}", p.0),
            OverrideKey::Slider(k) => format!("slider/{k}"),
            OverrideKey::Emcon => "emcon".into(),
            OverrideKey::BurstTriggerRange => "burst_trigger_range".into(),
            OverrideKey::SelfDefenceRange => "self_defence_range".into(),
            OverrideKey::Doctrine { mount, index } => format!("doctrine/{}/{}", mount.map_or_else(|| "ship".to_string(), |m| m.0.to_string()), index),
            OverrideKey::PowerPriority(p) => format!("power_priority/{}", p.0),
            OverrideKey::RepairOrder => "repair_order".into(),
            OverrideKey::Sink(s) => format!("sink/{}", sink(s)),
            OverrideKey::TrackStance(t) => format!("track_stance/{}", t.0),
            OverrideKey::TrackIdentity(t) => format!("track_identity/{}", t.0),
        }
    }

    pub(crate) fn decode(id: &str) -> Option<OverrideKey> {
        if id.len() > 512 { return None; }
        let (head, tail) = match id.split_once('/') { Some((h, t)) => (h, Some(t)), None => (id, None) };
        let num16 = || tail.and_then(|t| t.parse::<u16>().ok());
        let num32 = || tail.and_then(|t| t.parse::<u32>().ok()).filter(|n| *n > 0);
        let plain = |k: OverrideKey| tail.is_none().then_some(k);
        match head {
            "preset" => plain(OverrideKey::Preset),
            "mount_preset" => num16().map(|s| OverrideKey::MountPreset(PartSlot(s))),
            "slider" => tail.filter(|t| !t.is_empty()).map(|t| OverrideKey::Slider(t.to_string())),
            "emcon" => plain(OverrideKey::Emcon),
            "burst_trigger_range" => plain(OverrideKey::BurstTriggerRange),
            "self_defence_range" => plain(OverrideKey::SelfDefenceRange),
            "doctrine" => {
                let (m, i) = tail?.split_once('/')?;
                let mount = if m == "ship" { None } else { Some(PartSlot(m.parse::<u16>().ok()?)) };
                Some(OverrideKey::Doctrine { mount, index: i.parse::<u8>().ok()? })
            }
            "power_priority" => num16().map(|s| OverrideKey::PowerPriority(PartSlot(s))),
            "repair_order" => plain(OverrideKey::RepairOrder),
            "sink" => tail.and_then(sink_from).map(OverrideKey::Sink),
            "track_stance" => num32().map(|t| OverrideKey::TrackStance(TrackId(t))),
            "track_identity" => num32().map(|t| OverrideKey::TrackIdentity(TrackId(t))),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Dictionaries this file builds
// ---------------------------------------------------------------------------------------------------------

use crate::convert::{d, empty_msg, msg_dict};

fn opt_track(t: TrackId) -> i64 { crate::convert::track_to_gd(t) }
fn opt_vec2(p: Option<[f64; 2]>) -> Variant { match p { Some([x, z]) if x.is_finite() && z.is_finite() => Vector2::new(x as f32, z as f32).to_variant(), _ => Variant::nil() } }

/// The sound stem `BpAudioFeed` plays for an alert priority.
fn alert_stem(p: AlertPriority) -> StringName { StringName::from(crate::seams::sound::stem(crate::seams::sound::mix::alert_sound(p.to_gd().clamp(0, 3) as u8))) }

/// `AlertDict`, shared by `alert_raised` and the alert getters (unit `gd-hud-alerts`). Parts in the text are
/// named as the HUD names them (`BpHud::named_parts`).
pub(crate) fn alert_dict(hud: &BpHud, a: &AlertView) -> VarDictionary {
    let (track, part, compartment) = alert_subject(a);
    let word = crate::text::word_key_of("priority", a.priority.to_gd()).unwrap_or_default();
    d! {
        "id" => a.id.0 as i64,
        "priority" => a.priority.to_gd(),
        "subsystem" => a.subsystem.to_gd(),
        "state" => a.state.to_gd(),
        "text" => msg_dict(&hud.named_parts(&sim_systems::text::alert(&a.kind, a.priority))),
        "word" => StringName::from(word.as_str()),
        "raised_s" => tick_s(a.raised_tick),
        "officer" => a.officer.map_or(-1, |o| o.to_gd()),
        "blink_fast" => a.blink_fast,
        "track" => opt_track(track),
        "part" => part.map_or(-1, crate::convert::slot_to_gd),
        "compartment" => compartment.map_or(-1, |c| c.0 as i64),
        "sound" => alert_stem(a.priority),
    }
}

/// The HUD's name for a part of the own design: the player's label, else category word and order ("Gun 2").
pub(crate) fn part_name_msg(design: Option<&sim_systems::ship::ShipDesign>, slot: PartSlot) -> Msg {
    let Some(design) = design else { return Msg::new("units.part.numbered").arg(MsgArg::Int(slot.0 as i64 + 1)) };
    let parts = &design.blueprint().parts;
    let Some(placement) = parts.get(slot.index()) else { return Msg::new("units.part.numbered").arg(MsgArg::Int(slot.0 as i64 + 1)) };
    if let Some(label) = placement.label.as_ref().filter(|l| !l.trim().is_empty()) { return Msg::new("hud.name").arg(MsgArg::Name(label.clone())); }
    let catalogue = services().catalogue();
    let Some(category) = catalogue.part(&placement.part).map(|d| d.category) else { return Msg::new("units.part.numbered").arg(MsgArg::Int(slot.0 as i64 + 1)) };
    let order = parts[..=slot.index()].iter().filter(|p| catalogue.part(&p.part).is_some_and(|d| d.category == category)).count();
    Msg::new("units.part.generated").arg(word_key("category", category.to_gd())).arg(MsgArg::Int(order as i64))
}

/// `text` with `detail` as its last argument: a nested key, or translated text when the detail has arguments.
fn compose(text: &Msg, detail: Option<&Msg>) -> Msg {
    let mut m = text.clone();
    if let Some(d) = detail {
        if d.args.is_empty() { m.args.push(MsgArg::Key(d.key.clone())); } else { m.args.push(MsgArg::Name(crate::text::msg_text(&crate::text::EngineLookup::new(), d))); }
    }
    m
}

fn log_entry_dict(e: &LogEntry) -> VarDictionary {
    d! {
        "seq" => e.seq as i64,
        "time_s" => tick_s(e.tick),
        "kind" => e.kind.to_gd(),
        "text" => msg_dict(&e.text),
        "track" => opt_track(e.track),
        "part" => e.part.map_or(-1, crate::convert::slot_to_gd),
        "vessel" => e.vessel.map_or(-1, crate::convert::entity_to_gd),
        "pos" => opt_vec2(e.pos),
        "unread" => e.unread,
        "silent" => e.silent,
    }
}

fn officer_line_dict(l: &LineFacts) -> VarDictionary {
    use crate::seams::sound::{RadioSfx, SoundId};
    let (speaker, ace, role, sound) = match &l.speaker {
        Speaker::Officer(r) => (Msg::new(sim_systems::text::officer_word(*r).as_str()), StringName::default(), r.to_gd(), SoundId::Radio(RadioSfx::SquelchOpen)),
        Speaker::Ace(id) => {
            let name = services().campaign().aces.get(id).map(|a| a.callsign.clone()).unwrap_or_else(|| TextKey::new(id.as_str()));
            (Msg::new(name.as_str()), StringName::from(id.as_str()), -1, SoundId::Radio(RadioSfx::AceOpen))
        }
    };
    d! {
        "speaker" => msg_dict(&speaker),
        "text" => msg_dict(&l.text),
        "ace" => ace,
        "role" => role,
        "track" => opt_track(l.track),
        "time_s" => tick_s(l.tick),
        "new_speaker" => l.new_speaker,
        "sound" => StringName::from(crate::seams::sound::stem(sound)),
    }
}

fn why_dict(slot: PartSlot, chain: &[sim_systems::protocol::WhyLink]) -> VarDictionary {
    let links: VarArray = chain.iter().map(|l| d! { "text" => msg_dict(&l.text), "node" => l.node.map_or(-1, |n| n.0 as i64), "part" => l.part.map_or(-1, crate::convert::slot_to_gd) }.to_variant()).collect();
    d! { "slot" => crate::convert::slot_to_gd(slot), "chain" => links }
}

fn track_card_dict(track: TrackId, candidates: &[sim_systems::protocol::CandidateView], evidence: &[(content::enums::IdRung, sim_systems::protocol::RungReason, u64)], stance: sim_systems::protocol::StanceReason) -> VarDictionary {
    let campaign = services().campaign();
    let cands: VarArray = candidates.iter().map(|c| {
        let (id, name) = match &c.class {
            Some(class) => (StringName::from(class.as_str()), campaign.classes.get(class).map_or_else(empty_msg, |d| msg_dict(&Msg::new(d.name.as_str())))),
            None => (StringName::default(), msg_dict(&Msg::new(if c.civilian { "hud.candidate.civilian" } else { "hud.candidate.unknown" }))),
        };
        d! { "class_id" => id, "name" => name, "domain" => c.domain.to_gd(), "civilian" => c.civilian, "own_side" => c.own_side }.to_variant()
    }).collect();
    let ev: VarArray = evidence.iter().map(|(rung, reason, tick)| d! { "rung" => rung.to_gd(), "reason" => msg_dict(&sim_systems::text::rung_reason(*reason)), "time_s" => tick_s(*tick) }.to_variant()).collect();
    d! { "track" => opt_track(track), "candidates" => cands, "evidence" => ev, "stance_reason" => msg_dict(&sim_systems::text::stance_reason(stance)) }
}

fn override_dict(o: &sim_systems::protocol::OverrideView) -> VarDictionary {
    d! {
        "id" => GString::from(override_id::encode(&o.key).as_str()),
        "label" => msg_dict(&o.label),
        "default_text" => crate::convert::value_text(&o.default),
        "current_text" => crate::convert::value_text(&o.current),
        "layer" => o.layer.to_gd(),
    }
}

// ---------------------------------------------------------------------------------------------------------
// The class
// ---------------------------------------------------------------------------------------------------------

/// Sortie HUD data. Autoload `Hud`.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct BpHud {
    pub(crate) snapshot: Option<Arc<Snapshot>>,
    pub(crate) seen_snapshot_rev: u64,
    pub(crate) revs: [u64; Section::COUNT],
    /// Cached conversions per section revision.
    pub(crate) cache: Vec<(u64, Variant)>,
    pub(crate) log: Vec<LogEntry>,
    pub(crate) next_seq: u64,
    pub(crate) lines: Vec<VarDictionary>,
    pub(crate) selected: TrackId,
    /// Position history per track for the plot's history layer, world x and z.
    pub(crate) history: std::collections::BTreeMap<TrackId, Vec<[f32; 2]>>,
    /// The viewer's own design for static part and compartment facts.
    pub(crate) own_design: Option<Arc<sim_systems::ship::ShipDesign>>,
    /// Everything else the core keeps (see [`HudCore`]).
    pub(crate) core: HudCore,
    base: Base<Node>,
}

/// Signals raised while a frame runs, emitted after every borrow is dropped.
enum Sig { Section(Section), Track(&'static str, TrackId, TrackId), Control(ControlRole, ControlHolder), Override(String, bool), Line(VarDictionary), Alert(VarDictionary), Log(VarDictionary), Reply(QueryReply), Selection(TrackId) }

#[godot_api]
impl INode for BpHud {
    fn init(base: Base<Node>) -> Self {
        Self { snapshot: None, seen_snapshot_rev: 0, revs: [0; Section::COUNT], cache: Vec::new(), log: Vec::new(), next_seq: 1, lines: Vec::new(), selected: TrackId::NONE, history: Default::default(), own_design: None, core: HudCore::default(), base }
    }
    fn ready(&mut self) { self.base_mut().set_process_priority(-90); }
    /// Unit `gd-hud-core`: the per-frame update described in the module docs. Inert outside a sortie.
    fn process(&mut self, _delta: f64) {
        let mut sigs = Vec::new();
        self.frame(&mut sigs);
        self.emit(sigs);
    }
}

impl BpHud {
    fn frame(&mut self, sigs: &mut Vec<Sig>) {
        let services = services();
        let feed = services.feed.borrow();
        let state = feed.state;
        let was = self.core.last_state;
        self.core.last_state = state;
        self.core.now_s = feed.now_s;
        let in_sortie = matches!(state, SessionState::Loading | SessionState::InSortie | SessionState::Ended | SessionState::Replay);
        let new_sortie = state == SessionState::Loading && was != SessionState::Loading;
        if !in_sortie || new_sortie {
            let had = self.snapshot.is_some() || !self.log.is_empty() || self.core.own_vessel.is_some();
            if had || new_sortie { self.reset_sortie(sigs); }
            if !in_sortie { return; }
        }
        let snap = feed.snapshot.clone();
        self.core.observe_viewer(snap.as_deref(), feed.intro.as_ref(), feed.welcome.as_ref());

        // 1. Events.
        let mut outs = Vec::new();
        self.core.walk(&feed.events, snap.as_deref().or(self.snapshot.as_deref()), state, feed.kind, &mut outs);
        drop(feed);
        let mut changed = 0u32;
        for o in outs { self.apply(o, &mut changed, sigs); }

        // 2. Sections, once the newest tick moved.
        let rev = services.feed.borrow().snapshot_rev;
        if rev != self.seen_snapshot_rev || snap.is_some() != self.snapshot.is_some() {
            self.seen_snapshot_rev = rev;
            let bits = diff_sections(self.snapshot.as_deref(), snap.as_deref(), self.core.tracks_at);
            if let Some(s) = snap.as_deref() {
                self.core.tick = s.tick;
                if bits & Section::Tracks.bit() != 0 {
                    let own = s.ship.as_ref().and_then(|x| own_view_in(s, x.vessel)).map(|v| DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z)).unwrap_or(DVec2::ZERO);
                    self.core.tracks_at = Some((own, s.tick));
                }
                self.sample_history(s);
                let mut outs = Vec::new();
                self.core.observe_held(s, &mut outs);
                for o in outs { self.apply(o, &mut changed, sigs); }
                // Events are timely and the sampled snapshot lags them, so the snapshot only adds alerts the
                // book never heard of (a join mid-sortie, a kill cam that hid the live events).
                for a in &s.alerts { if self.core.alert_is_ours(a) && self.core.alerts.get(a.id).is_none() { self.core.alerts.upsert(*a); } }
            }
            self.snapshot = snap;
            changed |= bits;
            // A selection whose track left the picture clears.
            if !self.selected.is_none() && !self.snapshot.as_ref().is_some_and(|s| s.tracks.binary_search_by_key(&self.selected, |t| t.id).is_ok()) {
                self.selected = TrackId::NONE;
                changed |= Section::Tracks.bit();
                sigs.push(Sig::Selection(TrackId::NONE));
            }
        }

        // 3. The own design, which may arrive after the hull.
        if self.resolve_design() { changed |= Section::Static.bit(); }
        if changed & Section::Static.bit() != 0 { for s in Section::OWN_SHIP { changed |= s.bit(); } }

        // The Bridge Board's live settings: ask for the override lists it is missing (unit `gd-hud-board`).
        self.board_sync();

        // 4. Rates, revisions, signals.
        self.core.pending |= changed;
        let now = self.core.now_s;
        for s in Section::ALL {
            if self.core.pending & s.bit() == 0 { continue; }
            let last = self.core.last_bump_s[s.index()];
            if now - last < s.min_interval_s() && last <= now { continue; }
            self.core.pending &= !s.bit();
            self.core.last_bump_s[s.index()] = now;
            self.revs[s.index()] += 1;
            sigs.push(Sig::Section(s));
        }
    }

    /// Look the own design up by hash until found; true when it changed.
    fn resolve_design(&mut self) -> bool {
        let Some(hash) = self.core.own_hash else {
            return if self.own_design.is_some() && self.core.own_vessel.is_none() { self.own_design = None; true } else { false };
        };
        if self.own_design.as_ref().is_some_and(|d| d.hash == hash) { return false; }
        let services = services();
        let found = match services.designs.borrow().get(&hash) {
            Some(crate::worker::ReceivedDesign::Full { design, .. }) => Some(design.clone()),
            _ => None,
        };
        let found = found.or_else(|| services.design.borrow().as_ref().filter(|c| c.hash == hash).and_then(|c| c.design.clone()));
        match found { Some(d) => { self.own_design = Some(d); true } None => false }
    }

    /// One point a sortie second per live track; forget tracks gone from the picture.
    fn sample_history(&mut self, s: &Snapshot) {
        if self.core.history_tick.is_some_and(|t| s.tick < t + TICK_HZ && s.tick >= t) { return; }
        self.core.history_tick = Some(s.tick);
        self.history.retain(|id, _| s.tracks.binary_search_by_key(id, |t| t.id).is_ok());
        for t in s.tracks.iter().filter(|t| !t.last_known) {
            if !(t.pos.x.is_finite() && t.pos.z.is_finite()) { continue; }
            let h = self.history.entry(t.id).or_default();
            h.push([t.pos.x as f32, t.pos.z as f32]);
            if h.len() > HISTORY_POINTS { let extra = h.len() - HISTORY_POINTS; h.drain(..extra); }
        }
    }

    fn reset_sortie(&mut self, sigs: &mut Vec<Sig>) {
        self.snapshot = None;
        self.core.reset();
        self.clear_views(sigs);
        for s in Section::ALL { self.revs[s.index()] += 1; sigs.push(Sig::Section(s)); }
    }

    /// Forget the log, lines, history, selection and design (the core is reset by the caller).
    fn clear_views(&mut self, sigs: &mut Vec<Sig>) {
        self.log.clear();
        self.next_seq = 1;
        self.lines.clear();
        let had_selection = !self.selected.is_none();
        self.selected = TrackId::NONE;
        self.history.clear();
        self.own_design = None;
        self.cache.clear();
        if had_selection { sigs.push(Sig::Selection(TrackId::NONE)); }
    }

    fn apply(&mut self, o: Out, changed: &mut u32, sigs: &mut Vec<Sig>) {
        match o {
            Out::Log(d) => {
                let text = self.named_parts(&d.text);
                let detail = d.detail.as_ref().map(|m| self.named_parts(m));
                let entry = LogEntry { seq: 0, tick: d.tick, kind: d.kind, text: compose(&text, detail.as_ref()), track: d.track, part: d.part, vessel: d.vessel, pos: d.pos, unread: true, silent: d.silent };
                logbook::push(&mut self.log, &mut self.next_seq, entry);
                if let Some(e) = self.log.last() { sigs.push(Sig::Log(log_entry_dict(e))); }
            }
            Out::TrackEvent { kind, track, other } => { *changed |= Section::Tracks.bit(); sigs.push(Sig::Track(kind, track, other)); }
            Out::Control { role, holder } => sigs.push(Sig::Control(role, holder)),
            Out::Override { key, overridden } => sigs.push(Sig::Override(override_id::encode(&key), overridden)),
            Out::Line(l) => {
                let l = LineFacts { text: self.named_parts(&l.text), ..l };
                let d = officer_line_dict(&l);
                self.lines.push(d.clone());
                if self.lines.len() > LINES_CAP { let extra = self.lines.len() - LINES_CAP; self.lines.drain(..extra); }
                sigs.push(Sig::Line(d));
            }
            Out::AlertRaised(a) => sigs.push(Sig::Alert(alert_dict(self, &a))),
            Out::Reply(r) => sigs.push(Sig::Reply(r)),
            Out::Bump(s) => *changed |= s.bit(),
            Out::Reset => { self.clear_views(sigs); *changed |= u32::MAX >> (32 - Section::COUNT); }
        }
    }

    /// Part arguments named as the HUD names them ("Gun 2", the player's label), from the own design. The
    /// log keeps sentences for the whole sortie, so it names parts once, when the line is written.
    pub(crate) fn named_parts(&self, msg: &Msg) -> Msg {
        let Some(design) = self.design() else { return msg.clone() };
        if !msg.args.iter().any(|a| matches!(a, MsgArg::Part(_))) { return msg.clone(); }
        let lk = crate::text::EngineLookup::new();
        let args = msg.args.iter().map(|a| match a {
            MsgArg::Part(s) => MsgArg::Name(crate::text::msg_text(&lk, &part_name_msg(Some(design), *s))),
            other => other.clone(),
        }).collect();
        Msg { key: msg.key.clone(), args }
    }

    pub(crate) fn emit_selection(&mut self) {
        let t = crate::convert::track_to_gd(self.selected);
        self.base_mut().emit_signal("selection_changed", &[t.to_variant()]);
    }

    fn emit(&mut self, sigs: Vec<Sig>) {
        for sig in sigs {
            match sig {
                Sig::Section(s) => { self.base_mut().emit_signal(s.signal(), &[]); }
                Sig::Track(kind, t, other) => { self.base_mut().emit_signal("track_event", &[StringName::from(kind).to_variant(), opt_track(t).to_variant(), opt_track(other).to_variant()]); }
                Sig::Control(role, holder) => { self.base_mut().emit_signal("control_changed", &[crate::convert::role_dict(role).to_variant(), holder.to_gd().to_variant()]); }
                Sig::Override(id, on) => { self.base_mut().emit_signal("override_changed", &[GString::from(id.as_str()).to_variant(), on.to_variant()]); }
                Sig::Line(d) => { self.base_mut().emit_signal("officer_line", &[d.to_variant()]); }
                Sig::Alert(d) => { self.base_mut().emit_signal("alert_raised", &[d.to_variant()]); }
                Sig::Log(d) => { self.base_mut().emit_signal("log_added", &[d.to_variant()]); }
                Sig::Selection(t) => { self.base_mut().emit_signal("selection_changed", &[opt_track(t).to_variant()]); }
                Sig::Reply(r) => match r {
                    QueryReply::Why { slot, chain } => {
                        let chain: Vec<_> = chain.into_iter().map(|l| sim_systems::protocol::WhyLink { text: self.named_parts(&l.text), ..l }).collect();
                        let d = why_dict(slot, &chain);
                        self.base_mut().emit_signal("why_reply", &[d.to_variant()]);
                    }
                    QueryReply::TrackCard { track, candidates, evidence, stance_reason } => {
                        let d = track_card_dict(track, &candidates, &evidence, stance_reason);
                        self.base_mut().emit_signal("track_card_reply", &[d.to_variant()]);
                    }
                    QueryReply::Overrides { role, values } => {
                        let a: Array<VarDictionary> = values.iter().map(override_dict).collect();
                        self.base_mut().emit_signal("overrides_reply", &[role.to_gd().to_variant(), a.to_variant()]);
                    }
                    QueryReply::Scope { port, value } => {
                        let v = crate::convert::value_dict(&value);
                        self.base_mut().emit_signal("scope_value", &[(port.node.0 as i64).to_variant(), (port.port as i64).to_variant(), v.to_variant()]);
                    }
                },
            }
        }
    }

    // ---- Shared with the sibling units ----

    /// The result of `build`, cached per revision of `sections` (and per `arg` for getters with an argument).
    /// A second call in the same revision returns the same Array or Dictionary.
    pub(crate) fn cached<T: ToGodot + FromGodot>(&self, name: &'static str, arg: i64, sections: &[Section], build: impl FnOnce(&Self) -> T) -> T {
        let rev = sections.iter().fold(0x9E37_79B9_7F4A_7C15u64, |h, s| h.wrapping_mul(1_000_003).wrapping_add(self.revs[s.index()]).rotate_left(7));
        let hit = self.core.memo.borrow().get(&(name, arg)).filter(|(r, _)| *r == rev).map(|(_, v)| v.clone());
        if let Some(v) = hit { if let Ok(t) = v.try_to::<T>() { return t; } }
        let value = build(self);
        let mut memo = self.core.memo.borrow_mut();
        // Per-argument entries could pile up: drop stale ones when the cache grows.
        if memo.len() > 256 { memo.clear(); }
        memo.insert((name, arg), (rev, value.to_variant()));
        value
    }
    /// The newest snapshot.
    pub(crate) fn snap(&self) -> Option<&Snapshot> { self.snapshot.as_deref() }
    /// The viewer's own hull in detail; `None` without a hull.
    pub(crate) fn ship(&self) -> Option<&ShipStatus> { self.snapshot.as_ref().and_then(|s| s.ship.as_ref()) }
    /// The own design, once it is built.
    pub(crate) fn design(&self) -> Option<&sim_systems::ship::ShipDesign> { self.own_design.as_deref() }
    /// The own hull's pose and state.
    pub(crate) fn own_view(&self) -> Option<&VesselView> { let s = self.snap()?; own_view_in(s, s.ship.as_ref()?.vessel) }
    /// The state the event walk keeps.
    pub(crate) fn core(&self) -> &HudCore { &self.core }
}

#[godot_api]
impl BpHud {
    // ---- Own ship ----
    /// `motion()` changed (30 Hz).
    #[signal]
    fn motion_changed();
    /// `power()` changed (2 Hz).
    #[signal]
    fn power_changed();
    /// `parts()` changed (2 Hz).
    #[signal]
    fn parts_changed();
    /// `mounts()` or `weapon_groups()` changed (10 Hz).
    #[signal]
    fn mounts_changed();
    /// `compartments()` changed (2 Hz).
    #[signal]
    fn compartments_changed();
    /// `crew()` changed.
    #[signal]
    fn crew_changed();
    /// `networks()` changed (2 Hz).
    #[signal]
    fn networks_changed();
    /// `objectives()` changed.
    #[signal]
    fn objectives_changed();
    /// `allies()` changed (2 Hz).
    #[signal]
    fn allies_changed();
    /// `actions()` or `available_actions()` changed.
    #[signal]
    fn actions_changed();
    /// `offboard()` changed.
    #[signal]
    fn offboard_changed();
    /// The viewer's hull or its design changed: re-read `parts_static()` and `compartments_static()`.
    /// Also fires with no hull (`has_ship()` false) when the hull is lost.
    #[signal]
    fn ship_static_changed();

    // ---- Bridge Board and control ----
    /// `board()` changed (at most 10 Hz).
    #[signal]
    fn board_changed();
    /// A role changed hands: `role` is a `RoleDict`, `holder` is `Bp.HOLDER_*`. The HUD draws the mode-change
    /// box for 10 s (L1649, L1665); the log entry is already written.
    #[signal]
    fn control_changed(role: VarDictionary, holder: i64);
    /// A value was overridden or reverted: `id` matches `OverrideDict.id`.
    #[signal]
    fn override_changed(id: GString, overridden: bool);
    /// An officer or ace line (`OfficerLineDict`). The text is shown no later than the sound starts (L1706).
    #[signal]
    fn officer_line(line: VarDictionary);

    // ---- Tracks ----
    /// `tracks()` changed (10 Hz).
    #[signal]
    fn tracks_changed();
    /// Something happened to a track that the symbol and log must show (rule 5 and 7): `kind` is one of
    /// `&"created"`, `&"dropped"`, `&"merged"` (`other` = surviving track), `&"split"` (`other` = new track),
    /// `&"rung"`, `&"stance"`.
    #[signal]
    fn track_event(kind: StringName, track: i64, other: i64);
    /// The selected track changed (`Bp.NONE` = none).
    #[signal]
    fn selection_changed(track: i64);

    // ---- Alerts ----
    /// `alert_strip()` or `alerts()` changed.
    #[signal]
    fn alerts_changed();
    /// First annunciation of an alert (`AlertDict`); its sound is queued on the audio feed already.
    #[signal]
    fn alert_raised(alert: VarDictionary);

    // ---- Log and query replies ----
    /// A log entry was added (`LogEntryDict`).
    #[signal]
    fn log_added(entry: VarDictionary);
    /// Reply to `Orders.query_why` (`WhyDict`).
    #[signal]
    fn why_reply(why: VarDictionary);
    /// Reply to `Orders.query_track_card` (`TrackCardDict`).
    #[signal]
    fn track_card_reply(card: VarDictionary);
    /// Reply to `Orders.query_overrides`: `values` is `Array[OverrideDict]`.
    #[signal]
    fn overrides_reply(officer: i64, values: Array<VarDictionary>);
    /// A scoped graph port's value (`ValueDict`), at most 10 Hz per port.
    #[signal]
    fn scope_value(node: i64, port: i64, value: VarDictionary);

    // ---- Core ----

    /// True while the viewer has a live hull in a sortie.
    #[func]
    fn has_ship(&self) -> bool {
        use sim_core::vessel::VesselStatus as V;
        self.ship().is_some() && !self.own_view().is_some_and(|v| matches!(v.status, V::Lost(_) | V::Wreck))
    }
    /// Revision counter of one section: `&"motion"`, `&"power"`, `&"parts"`, `&"mounts"`, `&"compartments"`,
    /// `&"crew"`, `&"networks"`, `&"board"`, `&"tracks"`, `&"alerts"`, `&"objectives"`, `&"allies"`,
    /// `&"actions"`, `&"offboard"`, `&"static"`. For widgets that are shown late and need to know whether
    /// what they drew is current.
    #[func]
    fn rev(&self, section: StringName) -> i64 {
        Section::from_name(&section.to_string()).map_or(-1, |s| self.revs[s.index()].min(i64::MAX as u64) as i64)
    }

    // ---- Log (rule 15, L1671) ----

    /// Log entries, oldest first. `filter` keys (all optional): `kinds: PackedInt32Array` (`Bp.LOG_*`),
    /// `track: int`, `part: int`, `vessel: int`, `unread_only: bool`. `after_seq` returns entries with a higher
    /// sequence number (0 = from the start); `limit` caps the count (0 = 200). Returns `Array[LogEntryDict]`.
    #[func]
    fn log_entries(&self, filter: VarDictionary, after_seq: i64, limit: i64) -> Array<VarDictionary> {
        let f = log_filter(&filter);
        let limit = if limit <= 0 { LOG_PAGE } else { (limit as usize).min(LOG_PAGE_MAX) };
        logbook::select(&self.log, &f, after_seq.max(0) as u64, limit).into_iter().map(log_entry_dict).collect()
    }
    /// Number of unread, non-silent entries.
    #[func]
    fn log_unread(&self) -> i64 { logbook::unread(&self.log) as i64 }
    /// Mark entries up to and including `seq` as read.
    #[func]
    fn log_mark_read(&mut self, seq: i64) { if seq > 0 { logbook::mark_read(&mut self.log, seq as u64); } }
    /// The whole log as plain translated text with times, for the clipboard.
    #[func]
    fn log_text(&self) -> GString {
        let lk = crate::text::EngineLookup::new();
        let mut out = String::new();
        for e in &self.log {
            out.push_str(&crate::text::clock_text(&lk, tick_s(e.tick)));
            out.push_str("  ");
            out.push_str(&crate::text::msg_text(&lk, &e.text));
            out.push('\n');
        }
        GString::from(out.as_str())
    }
    /// The most recent officer and ace lines, oldest first (`Array[OfficerLineDict]`, at most `limit`).
    #[func]
    fn officer_lines(&self, limit: i64) -> Array<VarDictionary> {
        let n = if limit <= 0 { 0 } else { (limit as usize).min(self.lines.len()) };
        self.lines[self.lines.len() - n..].iter().cloned().collect()
    }
}

/// `log_entries` filter keys from GDScript; mistyped keys are ignored.
fn log_filter(dict: &VarDictionary) -> logbook::Filter {
    use crate::convert::{get_bool, get_i64};
    let kinds = dict.get("kinds").and_then(|v| v.try_to::<PackedInt32Array>().ok().or_else(|| v.try_to::<VarArray>().ok().map(|a| a.iter_shared().filter_map(|x| x.try_to::<i32>().ok()).collect())))
        .map(|k| k.as_slice().iter().filter_map(|i| LogKind::from_gd(*i as i64)).collect::<Vec<_>>());
    logbook::Filter {
        kinds,
        track: get_i64(dict, "track").filter(|t| *t >= 0).map(|t| u32::try_from(t).map(TrackId).unwrap_or(TrackId(u32::MAX))),
        part: get_i64(dict, "part").filter(|p| *p >= 0).map(|p| PartSlot(u16::try_from(p).unwrap_or(u16::MAX))),
        vessel: get_i64(dict, "vessel").filter(|v| *v >= 0).map(|v| EntityId::from_u64(v as u64)),
        unread_only: get_bool(dict, "unread_only").unwrap_or(false),
    }
}

/// Every text key this unit and its siblings in `log.csv` emit, for the coverage test.
pub fn text_keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = LOG_KEYS.to_vec();
    keys.extend_from_slice(ship::TEXT_KEYS);
    keys.extend_from_slice(tracks::TEXT_KEYS);
    keys.extend_from_slice(damage::TEXT_KEYS);
    keys.extend_from_slice(board::TEXT_KEYS);
    keys.extend_from_slice(crate::orders::TEXT_KEYS);
    keys
}

/// A host overload ladder step in plain words (L1025: every change shows in the log). Up names what the host
/// just simplified; down says the load is easing and what is still in force. Rung 0 steps are the time
/// compression ceiling (4x, 2x, 1x) the host lowers before rung 2; the event does not say which.
pub(crate) fn ladder_msg(rung: u8, sub_step: u8, up: bool) -> Msg {
    let what = match (rung, sub_step) {
        (0 | 1, _) => "log.pacing.step.compression",
        (2, 0) => "log.pacing.step.kinematic_far",
        (2, _) => "log.pacing.step.kinematic_all",
        (3, 0) => "log.pacing.step.projectiles_1500",
        (3, _) => "log.pacing.step.projectiles_1000",
        _ => "log.pacing.step.time_scale",
    };
    let word = MsgArg::Key(TextKey::new(what));
    if up { Msg::new("log.pacing.load_up").arg(word) } else if rung <= 1 { Msg::new("log.pacing.load_down_clear") } else { Msg::new("log.pacing.load_down").arg(word) }
}

const LOG_KEYS: &[&str] = &[
    "hud.name", "hud.candidate.civilian", "hud.candidate.unknown",
    "log.system.started", "log.system.ended", "log.system.rejected", "log.system.script",
    "log.pacing.paused", "log.pacing.resumed", "log.pacing.rate", "log.pacing.blocked", "log.pacing.unblocked", "log.pacing.scale",
    "log.pacing.load_up", "log.pacing.load_down", "log.pacing.load_down_clear",
    "log.pacing.step.compression", "log.pacing.step.kinematic_far", "log.pacing.step.kinematic_all",
    "log.pacing.step.projectiles_1500", "log.pacing.step.projectiles_1000", "log.pacing.step.time_scale",
    "log.network.joined", "log.network.dropped", "log.network.left",
    "log.track.created", "log.track.dropped", "log.track.merged", "log.track.split", "log.track.rung", "log.track.rung_class",
    "log.track.stance", "log.track.reinforcements",
    "log.officer.radiating", "log.officer.silent",
    "log.engage.engaging", "log.engage.holding", "log.engage.holding_none", "log.engage.unconverged", "log.engage.seduced",
    "log.engage.lost", "log.engage.lost_vessel", "log.engage.disabled", "log.engage.struck",
    "log.control.take.helm", "log.control.back.helm", "log.control.take.sensors", "log.control.back.sensors",
    "log.control.take.mount", "log.control.back.mount", "log.control.take.group", "log.control.back.group",
    "log.control.overridden", "log.control.reverted",
    "log.alert.cleared", "log.alert.silenced", "log.alert.flood_on", "log.alert.flood_off",
    "log.objective.done", "log.objective.failed", "log.objective.ghost_done", "log.objective.ghost_failed", "log.objective.unnamed",
    "log.action.done", "log.action.aborted", "log.action.paused", "log.action.running", "log.action.no_reason",
    "log.action.photo", "log.action.prize", "log.action.prize_class",
    "log.damage.part", "log.damage.breach", "log.damage.breach_below", "log.damage.conduit", "log.damage.fire", "log.damage.fire_out",
    "log.damage.half_flooded", "log.damage.magazine_flooded", "log.damage.cook_off", "log.damage.passengers", "log.damage.jury_rigged",
    "log.damage.patched", "log.damage.repaired", "log.damage.fire_fought", "log.damage.grounded", "log.damage.collision",
    "log.damage.own_lost", "log.damage.ally_lost",
];

#[cfg(test)]
mod core_tests;
