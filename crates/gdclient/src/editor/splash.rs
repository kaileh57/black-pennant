//! Unit `gd-editor-splash`. Instant testing on `BpEditor`. DESIGN.md L214-229 (Splash: Space saves the design
//! and puts the ship in a test basin; water calm, Beaufort 4 or Beaufort 7; target drones or a patrol boat
//! that fights back; 30 s rewind, scope on any port, live edits to kits, tuning and graphs; changing the hull
//! shell returns to dry dock; every splash ends with a report card and one tip), L218-223 (load p95 at most
//! 1 s, hard limit 2 s; pre-built and progressive splash fallbacks switch themselves on and persist).
//! The basin is an ordinary session: this unit prepares it (`sim_systems::editor::splash`) and leaves a
//! `PendingSortie` in `Services::handoff`; `BpSession` runs it; the HUD and world view draw it.
//!
//! # How a splash runs
//! - `splash()` writes the design to its file (see "Saving" below), marks it saved (which caches it in
//!   `Services::design` and starts the trial battery), checks that it floats at all, then hands over either
//!   the pre-built world (`HostStart::Splash`, when the worker's `PreparedSplash` is for this revision, these
//!   options and this seed) or the basin config (`HostStart::New`, so the world is built on the clock
//!   thread, not in the frame). Progressive splash is `SplashStage::ShipOnly`, or
//!   `sim_systems::editor::splash::progressive` of the config.
//! - [`BpEditor::splash_frame`] runs once a frame from `BpEditor::process`. It times the load from the
//!   `splash()` call to the first frame the session is in the sortie (helm input accepted, L218), applies the
//!   fallbacks, follows the own hull's heel and the first hit from `Services::view` and `Services::feed`
//!   (the simulation's own splash log lives on the clock thread), and ends the splash when the session
//!   hands back a `FinishedSortie` of kind Splash, goes idle or fails.
//! - Rewind, restart and live edits are `Command::SplashRewind`, `SplashRestart` and `SplashApplyDesign`
//!   sent on the link; `splash_end` asks the clock thread to stop the sortie, and the next frames deliver the
//!   result. After `splash_ended` the session is in `Bp.SESSION_ENDED` (or already idle); the dock screen
//!   calls `Session.leave(false)` to drop the basin, as for any finished session.
//!
//! # Choices the spec leaves open
//! - Saving goes through the files unit's own write (`BpEditor::write_design`: lineage, format, hash, trial
//!   history, `mark_saved`). A design with a `user://` path is written there. A new design, or one opened from
//!   the stock set, is written to `user://blueprints/` under its name (`name.bp.ron`, then `name 2.bp.ron`...),
//!   and that becomes its path.
//! - "Cannot float at all" is the Float gauge's `floats` for the current revision. When Space comes before
//!   the worker has built the current revision, the Float gauge is computed here, at the 10 degree step.
//! - The seed is the same for every splash, so two splashes of one design meet the same water and the same
//!   targets: the report cards compare edits, not luck.
//! - Splash refuses while this process is in a multiplayer session (a lobby host would start the basin for
//!   every member; a member would leave the lobby) and while a sortie is loading or running.
//! - A live edit the simulation refuses (an invalid design) closes the basin and returns to dry dock, as a
//!   changed hull shell does; both fire `returned_to_dock`, and the splash still ends with its report card.
//! - The report card is built from the session's summary and result. Peak heel and time to first hit are
//!   followed on this side at the playback tick; a rewind or restart forgets what came after the new tick.
//!   When the summary carries no tip (the player left from the pause menu), the tip is picked from the dock's
//!   own notes and gauges in the simulation's order (`sim_systems::editor::splash::tip`), without the hold
//!   reasons only the simulation sees.
//! - Load p95 is over the last [`LOAD_WINDOW`] splashes (the gate's 20 runs), nearest rank.
//! - `launch()` is allowed exactly when `launch_state().can_launch` is true; it saves and caches the design
//!   (`Services::design`) and returns true. The briefing and lobby screens then call
//!   `Campaign.launch_sortie()` or `Lobby.select_blueprint(Editor.design_path())`, which read it (facades
//!   never call each other).
// Client glue, not simulation (clippy.toml): std floats are allowed here.
#![allow(clippy::disallowed_methods)]
use super::BpEditor;
use crate::convert::{d, msg_dict};
use crate::enums::{EditorMode, GdEnum, SessionKind, SessionState};
use content::blueprint::Blueprint;
use content::ids::{ContentHash, SideId};
use content::settings::{SplashOptions, SplashTarget};
use content::weather::SeaState;
use godot::prelude::*;
use sim_core::EntityId;
use sim_systems::editor::gauges::GaugeStrip;
use sim_systems::editor::notes::{NoteTier, ShipNote};
use sim_systems::editor::splash::ReportCard;
use sim_systems::editor::Check;
use sim_systems::protocol::{Command, Msg, Outcome, SortieResult, SortieStats, SortieSummary, Unit};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

/// Text keys this unit emits (`game/i18n/dock.csv`).
pub(crate) const SPLASH_KEYS: &[&str] = &[
    "dock.splash.shell_changed", "dock.splash.refused_edit", "dock.splash.save_failed", "dock.splash.cannot_float",
    "dock.splash.busy",
];

/// Splashes the load p95 is taken over (the gate's 20 runs, L218).
pub(crate) const LOAD_WINDOW: usize = 20;
/// A splash slower than this turns the pre-built splash on (L221).
pub(crate) const PREBUILT_AFTER_MS: f64 = 1_000.0;
/// A pre-built splash slower than this turns the progressive splash on (L224).
pub(crate) const PROGRESSIVE_AFTER_MS: f64 = 2_000.0;
/// The rewind reaches back at most this far (L216).
pub(crate) const MAX_REWIND_S: f64 = 30.0;
/// Seconds `splash_end` waits for the session's result before it ends the splash with what it saw.
const END_WAIT_S: f64 = 3.0;
/// A refusal of a live edit counts as one when it arrives within this many seconds of sending it.
const APPLY_WAIT_S: f64 = 3.0;
/// A playback tick this far behind the last one seen is a rewind or a restart, not jitter.
const REWIND_TICKS: u64 = 6;
/// Seconds the hand-off may sit untaken before the splash is given up (the session never started it).
const START_WAIT_S: f64 = 10.0;
/// Fixed basin seed (see the module doc).
pub(crate) const SPLASH_SEED: u64 = 1;

/// What the basin saw of the own ship, at the playback tick. Pure bookkeeping; tests run it headless.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Observed {
    /// First playback tick seen.
    pub start_tick: Option<u64>,
    pub last_tick: u64,
    /// The highest heel per second of sortie time (second, degrees).
    pub heel: BTreeMap<u64, f64>,
    pub first_hit: Option<u64>,
    /// Target hulls out of the fight and the tick they went.
    pub targets_out: BTreeMap<EntityId, u64>,
}
impl Observed {
    /// A new playback tick: one well behind the last means the clock went back, so everything after it is
    /// forgotten.
    pub fn at(&mut self, tick: u64) {
        if self.start_tick.is_none() { self.start_tick = Some(tick); }
        if tick + REWIND_TICKS < self.last_tick {
            let sec = tick / sim_core::TICK_HZ as u64;
            self.heel.retain(|s, _| *s < sec);
            if self.first_hit.is_some_and(|t| t > tick) { self.first_hit = None; }
            self.targets_out.retain(|_, t| *t <= tick);
            if self.start_tick.is_some_and(|s| s > tick) { self.start_tick = Some(tick); }
        }
        self.last_tick = tick;
    }
    pub fn heel_deg(&mut self, tick: u64, deg: f64) {
        if !deg.is_finite() { return; }
        let e = self.heel.entry(tick / sim_core::TICK_HZ as u64).or_insert(0.0);
        if deg.abs() > *e { *e = deg.abs(); }
    }
    pub fn hit(&mut self, tick: u64) { if self.first_hit.is_none_or(|t| tick < t) { self.first_hit = Some(tick); } }
    pub fn target_out(&mut self, e: EntityId, tick: u64) { self.targets_out.entry(e).or_insert(tick); }
    pub fn peak_heel_deg(&self) -> f64 { self.heel.values().copied().fold(0.0, f64::max).min(180.0) }
    pub fn first_hit_s(&self) -> Option<f64> {
        let hit = self.first_hit?;
        Some(hit.saturating_sub(self.start_tick.unwrap_or(0)) as f64 / sim_core::TICK_HZ as f64)
    }
}

/// One splash from `splash()` to `splash_ended`.
pub(crate) struct SplashRun {
    pressed: Instant,
    /// Load time once the first frame of the sortie was seen.
    loaded_ms: Option<f64>,
    /// The worker's pre-built world was swapped in.
    prebuilt: bool,
    /// The blueprint the basin was launched with, canonical: its shell is the one live edits must keep.
    base: Arc<Blueprint>,
    /// Hash of the design last pushed into the basin.
    applied: ContentHash,
    /// The session has picked the sortie up (left idle).
    seen_session: bool,
    stop_sent: Option<Instant>,
    apply_sent: Option<Instant>,
    /// `returned_to_dock` already fired.
    returned: bool,
    own_side: SideId,
    obs: Observed,
}

/// Splash state kept on `BpEditor` (`EditorState::splash`). Writer: this unit.
#[derive(Default)]
pub(crate) struct SplashState {
    pub(crate) run: Option<SplashRun>,
    /// Load times of the last splashes, milliseconds, newest last.
    pub(crate) samples: VecDeque<f64>,
    pub(crate) last_ms: f64,
    /// Splashes started in this process (the first is cold, L218).
    pub(crate) count: u32,
    /// Why the last `splash()` or `launch()` refused, for the Launch button's text.
    pub(crate) refused: Option<Msg>,
    /// The last report card.
    pub(crate) last_card: Option<ReportCard>,
}
impl SplashState {
    fn record(&mut self, ms: f64) {
        self.last_ms = ms;
        self.samples.push_back(ms);
        while self.samples.len() > LOAD_WINDOW { self.samples.pop_front(); }
    }
    pub(crate) fn p95(&self) -> f64 { p95(self.samples.iter().copied()) }
}

/// p95 by nearest rank; 0 with no samples.
pub(crate) fn p95(samples: impl Iterator<Item = f64>) -> f64 {
    let mut v: Vec<f64> = samples.filter(|s| s.is_finite()).collect();
    if v.is_empty() { return 0.0; }
    v.sort_by(|a, b| a.total_cmp(b));
    let rank = ((0.95 * v.len() as f64).ceil() as usize).clamp(1, v.len());
    v[rank - 1]
}

/// The fallbacks one splash's load time switches on: (pre-built, progressive) (L221-224).
pub(crate) fn fallbacks(ms: f64, prebuilt_used: bool) -> (bool, bool) {
    if !ms.is_finite() { return (false, false); }
    (ms > PREBUILT_AFTER_MS, prebuilt_used && ms > PROGRESSIVE_AFTER_MS)
}

/// A water option from GDScript: the WMO code nearest calm, Beaufort 4 or Beaufort 7.
pub(crate) fn water_of(code: i64) -> SeaState {
    match code { i64::MIN..=1 => SeaState::CALM, 2..=4 => SeaState::BEAUFORT_4, _ => SeaState::BEAUFORT_7 }
}

/// Rewind seconds to ticks: at least one tick, at most 30 s; `None` for nonsense.
pub(crate) fn rewind_ticks(seconds: f64) -> Option<u32> {
    if !seconds.is_finite() || seconds <= 0.0 { return None; }
    let max = (MAX_REWIND_S * sim_core::TICK_HZ as f64) as u32;
    let t = (seconds.min(MAX_REWIND_S) * sim_core::TICK_HZ as f64).round();
    Some((t as u32).clamp(1, max))
}

/// The blueprint in the form every load leaves it, hash recomputed (`sim_systems::editor::splash` does the
/// same before comparing shells).
pub(crate) fn canonical(bp: &Blueprint) -> Blueprint {
    let mut b = bp.clone();
    content::blueprint::io::canonicalize_blueprint(&mut b);
    b.header.hash = content::blueprint::io::compute_hash(&b);
    b
}

/// The hull shell changed: what `Sim::apply_design` refuses (`hull` or `cells` differ).
pub(crate) fn shell_changed(base: &Blueprint, next: &Blueprint) -> bool { base.hull != next.hull || base.cells != next.cells }

/// Every shot fired and every hit, shells, missiles and torpedoes together.
fn shots_and_hits(s: &SortieStats) -> (u32, u32) {
    let shots = s.rounds_fired.saturating_add(s.missiles_fired as u32).saturating_add(s.torpedoes_fired as u32);
    let hits = s.rounds_hit.saturating_add(s.missiles_hit as u32).saturating_add(s.torpedoes_hit as u32);
    (shots, hits.min(shots))
}
fn finite01(v: f64) -> f64 { if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 } }

/// Gauge tips in the simulation's order (`sim_systems::editor::splash`'s table).
const GAUGE_TIPS: [&str; 8] = [
    "splash.tip.gauge.float", "splash.tip.gauge.speed", "splash.tip.gauge.power", "splash.tip.gauge.punch",
    "splash.tip.gauge.sight", "splash.tip.gauge.resilience", "splash.tip.gauge.endurance", "splash.tip.gauge.complexity",
];
const LOW_HIT_RATE: f64 = sim_systems::editor::splash::LOW_HIT_RATE;
const LOW_HIT_MIN_SHOTS: u32 = sim_systems::editor::splash::LOW_HIT_MIN_SHOTS;
const HEAVY_DAMAGE: f64 = sim_systems::editor::splash::HEAVY_DAMAGE;

/// The tip when the simulation gave none: the dock's notes and gauges, then what the fight showed.
pub(crate) fn fallback_tip(notes: &[ShipNote], gauges: Option<&GaugeStrip>, shots: u32, hits: u32, cells_lost: f64, targets: bool) -> Msg {
    let note = |pred: &dyn Fn(NoteTier) -> bool| notes.iter().find(|n| pred(n.tier) && !n.text.is_empty()).map(|n| n.text.clone());
    if let Some(m) = note(&|t| t == NoteTier::CannotFloatMoveOrPower) { return m; }
    if let Some(m) = note(&|t| t <= NoteTier::CannotWork) { return m; }
    if let Some(g) = gauges {
        let checks = [g.float.check, g.speed.check, g.power.check, g.punch.check, g.sight.check, g.resilience.check, g.endurance.check, g.complexity.check];
        for want in [Check::Fail, Check::Caution] {
            if let Some(i) = checks.iter().position(|c| *c == want) { return Msg::new(GAUGE_TIPS[i]); }
        }
    }
    if targets && shots == 0 { return Msg::new("splash.tip.no_shots"); }
    if shots >= LOW_HIT_MIN_SHOTS && (hits as f64) < LOW_HIT_RATE * shots as f64 {
        return Msg::new("splash.tip.low_hit_rate").num(100.0 * hits as f64 / shots as f64, Unit::Percent);
    }
    if cells_lost > HEAVY_DAMAGE { return Msg::new("splash.tip.heavy_damage").num(100.0 * cells_lost.min(1.0), Unit::Percent); }
    if let Some(m) = note(&|_| true) { return m; }
    Msg::new("splash.tip.none")
}

/// What a host result adds to the summary: its counters, length and the enemy hulls taken out.
pub(crate) struct ResultFacts<'a> { pub stats: &'a SortieStats, pub duration_ticks: u64, pub kills: usize }
impl<'a> ResultFacts<'a> {
    pub(crate) fn of(r: &'a SortieResult) -> Self { Self { stats: &r.stats, duration_ticks: r.duration_ticks, kills: r.kills.len() } }
}

/// The report card from what the session handed back and what this side saw (L216). `tip` picks one from
/// (shots, hits, share of cells lost) when the summary has none.
pub(crate) fn build_card(summary: Option<&SortieSummary>, result: Option<ResultFacts<'_>>, obs: &Observed, tip: impl FnOnce(u32, u32, f64) -> Msg) -> ReportCard {
    let empty = SortieStats::default();
    // A summary written by `Session.leave` has no counters; the host result has them.
    let stats = match (summary, result.as_ref()) {
        (Some(s), Some(r)) if s.stats == empty => r.stats,
        (Some(s), _) => &s.stats,
        (None, Some(r)) => r.stats,
        (None, None) => &empty,
    };
    let (shots, hits) = shots_and_hits(stats);
    let ticks = summary.map_or(0, |s| s.duration_ticks).max(result.as_ref().map_or(0, |r| r.duration_ticks));
    let ticks = if ticks == 0 { obs.last_tick.saturating_sub(obs.start_tick.unwrap_or(0)) } else { ticks };
    let damage_taken = finite01(stats.own_cells_lost_fraction);
    let destroyed = result.as_ref().map_or(0, |r| r.kills).max(obs.targets_out.len()).min(u16::MAX as usize) as u16;
    let score = sim_systems::objectives::trial_score(stats);
    let tip = match summary.and_then(|s| s.tip.clone()) { Some(t) if !t.is_empty() => t, _ => tip(shots, hits, damage_taken) };
    ReportCard {
        duration_s: ticks as f64 / sim_core::TICK_HZ as f64,
        rounds_fired: shots,
        hit_rate: if shots > 0 { (hits as f64 / shots as f64).clamp(0.0, 1.0) } else { 0.0 },
        targets_destroyed: destroyed,
        time_to_first_hit_s: obs.first_hit_s(),
        damage_taken,
        peak_heel_deg: obs.peak_heel_deg(),
        score: if score.is_finite() { score } else { 0.0 },
        tip,
    }
}

/// `ReportCardDict` (shapes.rs).
pub(crate) fn card_dict(c: &ReportCard) -> VarDictionary {
    d! {
        "duration_s" => c.duration_s,
        "rounds_fired" => c.rounds_fired as i64,
        "hit_rate" => c.hit_rate,
        "targets_destroyed" => c.targets_destroyed as i64,
        "time_to_first_hit_s" => c.time_to_first_hit_s.unwrap_or(-1.0),
        "damage_taken" => c.damage_taken,
        "peak_heel_deg" => c.peak_heel_deg,
        "score" => c.score,
        "tip" => msg_dict(&c.tip),
    }
}

/// Heel of a render-space rotation (x, y, z, w) in degrees: the tilt of the beam axis out of the horizontal.
pub(crate) fn heel_of(rot: [f32; 4]) -> f64 {
    let q = glam::Quat::from_array(rot);
    if !q.is_finite() || q.length_squared() < 1e-6 { return 0.0; }
    let z = q.normalize() * glam::Vec3::Z;
    (z.y.clamp(-1.0, 1.0) as f64).asin().abs().to_degrees()
}

impl BpEditor {
    /// Write the design under edit to its file (or a fresh one, see the module doc) and mark it saved, unless
    /// it is already saved there and cached. False on a write error.
    pub(crate) fn save_for_launch(&mut self) -> bool {
        use super::io::{exists, free_name, user_path, ROOT, SHIP_EXT};
        let Some(m) = self.model.as_ref() else { return false };
        let hash = content::blueprint::io::compute_hash(&m.blueprint);
        // Only a player file is written over; an unsaved or stock design gets a file of its own.
        let full = Some(m.path.trim()).filter(|p| !p.is_empty()).and_then(user_path).filter(|f| f.as_str() != ROOT && f.ends_with(SHIP_EXT));
        let cached = crate::services::services().design.borrow().as_ref().is_some_and(|c| c.hash == hash && Some(&c.path) == full.as_ref());
        if let Some(f) = full.as_ref() { if !m.is_dirty() && cached && exists(f) { return true; } }
        let full = match full {
            Some(f) => f,
            None => match free_name(&m.blueprint.header.name, SHIP_EXT, &|seg| exists(&format!("{ROOT}/{seg}"))) {
                Some(name) => format!("{ROOT}/{name}"),
                None => return false,
            },
        };
        // The files unit's own write: lineage, format, hash, trial history and the saved design all follow.
        self.write_design(&full)
    }

    /// Does the design under edit float at all? From the current Float gauge, else computed here.
    fn floats_now(&self) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        if let Some((rev, g)) = m.gauges.as_ref() {
            if *rev == m.rev && self.state.results.fast_rev == m.rev { return g.float.floats; }
        }
        let cat = crate::services::services().catalogue();
        let prefab = match self.current_design() {
            Some(d) => d.prefab.clone(),
            None => Arc::new(sim_core::vessel::build_prefab(&m.blueprint, &cat)),
        };
        sim_systems::editor::gauges::float_gauge(&prefab, m.context.cap_t.max(1.0), 10.0, &cat).floats
    }

    fn refuse(&mut self, key: &str) -> bool {
        self.state.splash.refused = Some(Msg::new(key));
        false
    }

    /// Send a command on the live link. False without a joined link.
    fn send_command(&self, command: Command) -> bool {
        let services = crate::services::services();
        let Ok(mut conn) = services.connection.try_borrow_mut() else { return false };
        match conn.as_mut() {
            Some(c) if c.link.status().state == crate::seams::LinkState::Joined => { c.link.send(crate::seams::ClientMsg::Command(command)); true }
            _ => false,
        }
    }

    /// The session state this frame.
    fn session_state() -> SessionState { crate::services::services().feed.try_borrow().map_or(SessionState::Idle, |f| f.state) }

    /// Ask the clock thread to end the basin. False when nothing is hosted here.
    fn stop_basin(&mut self) -> bool {
        let services = crate::services::services();
        let sent = services.connection.try_borrow().ok().and_then(|c| c.as_ref().and_then(|c| c.host.as_ref()).map(|h| h.send(crate::session::host::HostControl::StopSortie(Outcome::Aborted)))).is_some();
        drop(services);
        if let Some(run) = self.state.splash.run.as_mut() { if run.stop_sent.is_none() { run.stop_sent = Some(Instant::now()); } }
        sent
    }

    /// The live edit could not go in: the basin closes and the dock reopens (L216).
    fn return_to_dock(&mut self, key: &str) {
        let Some(run) = self.state.splash.run.as_mut() else { return };
        if run.returned { return; }
        run.returned = true;
        self.stop_basin();
        self.base_mut().emit_signal("returned_to_dock", &[msg_dict(&Msg::new(key)).to_variant()]);
    }

    /// Once a frame from `process` (unit `gd-editor-splash`): load timing and fallbacks, what the basin shows,
    /// refused live edits and the end of the splash. Inert without a splash.
    pub(crate) fn splash_frame(&mut self) {
        if self.state.splash.run.is_none() { return; }
        let state = Self::session_state();
        let mut refused_edit = false;
        let mut first_frame: Option<(f64, bool)> = None;
        {
            let services = crate::services::services();
            let Some(run) = self.state.splash.run.as_mut() else { return };
            if !matches!(state, SessionState::Idle) { run.seen_session = true; }
            if run.loaded_ms.is_none() && state == SessionState::InSortie {
                let ms = run.pressed.elapsed().as_secs_f64() * 1_000.0;
                run.loaded_ms = Some(ms);
                first_frame = Some((ms, run.prebuilt));
            }
            if matches!(state, SessionState::InSortie | SessionState::Ended) {
                if let (Ok(view), Ok(feed)) = (services.view.try_borrow(), services.feed.try_borrow()) {
                    let tick = if view.render_tick.is_finite() && view.render_tick > 0.0 { view.render_tick as u64 } else { 0 };
                    run.obs.at(tick);
                    let own = view.own.and_then(|i| view.vessels.get(i as usize));
                    let own_id = own.map(|v| v.entity);
                    if let Some(v) = own { run.obs.heel_deg(tick, heel_of(v.rot)); }
                    for ev in &feed.events {
                        use sim_core::events::PhysEvent as P;
                        use sim_systems::protocol::SimEvent as E;
                        match &ev.event {
                            E::Phys(P::ShellImpact { vessel, shooter, .. }) if Some(*shooter) == own_id && Some(*vessel) != own_id => run.obs.hit(ev.tick),
                            E::Phys(P::CellsDamaged { vessel, attacker, .. }) if Some(*attacker) == own_id && Some(*vessel) != own_id => run.obs.hit(ev.tick),
                            E::Phys(P::VesselLost { vessel, side, .. }) if *side != run.own_side => run.obs.target_out(*vessel, ev.tick),
                            E::Phys(P::StatusChanged { vessel, status }) if own_id.is_some() && Some(*vessel) != own_id && !matches!(status, sim_core::vessel::VesselStatus::Active) => run.obs.target_out(*vessel, ev.tick),
                            E::CommandRejected { reason: sim_systems::protocol::RejectReason::OnlyInDryDockOrSplash, .. } if run.apply_sent.is_some_and(|t| t.elapsed().as_secs_f64() < APPLY_WAIT_S) => refused_edit = true,
                            _ => {}
                        }
                    }
                }
            }
        }
        if let Some((ms, prebuilt)) = first_frame {
            self.state.splash.record(ms);
            let (pre, prog) = fallbacks(ms, prebuilt);
            if pre { crate::settings::write("editor.prebuilt_splash", crate::settings::SettingValue::Bool(true)); }
            if prog { crate::settings::write("editor.progressive_splash", crate::settings::SettingValue::Bool(true)); }
        }
        if refused_edit { self.return_to_dock("dock.splash.refused_edit"); }

        // The end: the session's result, or the session gone without one.
        let (finished, pending) = {
            let services = crate::services::services();
            let Ok(mut h) = services.handoff.try_borrow_mut() else { return };
            let seen = self.state.splash.run.as_ref().is_some_and(|r| r.seen_session);
            let f = if seen && h.finished.as_ref().is_some_and(|f| f.kind == SessionKind::Splash) { h.finished.take() } else { None };
            (f, h.sortie.as_ref().is_some_and(|p| p.kind == SessionKind::Splash))
        };
        let Some(run) = self.state.splash.run.as_ref() else { return };
        let gone = run.seen_session && matches!(state, SessionState::Idle | SessionState::Failed | SessionState::Lobby);
        let timed_out = run.stop_sent.is_some_and(|t| t.elapsed().as_secs_f64() > END_WAIT_S);
        let never_started = !run.seen_session && !pending && run.pressed.elapsed().as_secs_f64() > START_WAIT_S;
        if finished.is_some() || gone || timed_out || never_started {
            let (summary, result) = match finished { Some(f) => (Some(f.summary), f.result), None => (None, None) };
            self.finish_splash(summary.as_ref(), result.as_deref());
        }
    }

    /// Close the splash with its report card.
    fn finish_splash(&mut self, summary: Option<&SortieSummary>, result: Option<&SortieResult>) {
        let Some(run) = self.state.splash.run.take() else { return };
        let (notes, gauges) = match self.model.as_ref() { Some(m) => (m.notes.1.clone(), m.gauges.as_ref().map(|(_, g)| (**g).clone())), None => (Vec::new(), None) };
        // Every basin has targets (drones or the patrol boat).
        let card = build_card(summary, result.map(ResultFacts::of), &run.obs, |shots, hits, lost| fallback_tip(&notes, gauges.as_ref(), shots, hits, lost, true));
        self.in_splash = false;
        self.state.splash.last_card = Some(card.clone());
        crate::services::services().tutorial_signal(sim_systems::tutorial::Signal::SplashEnded);
        self.idle_s = 0.0;
        self.base_mut().emit_signal("splash_ended", &[card_dict(&card).to_variant()]);
    }

    /// The options of the next splash from a GDScript dictionary; the current ones fill what it lacks.
    fn read_options(&self, options: &VarDictionary) -> SplashOptions {
        let mut o = self.state.splash_options;
        if let Some(w) = crate::convert::get_i64(options, "water") { o.water = water_of(w); }
        if let Some(t) = crate::convert::get_i64(options, "target").and_then(SplashTarget::from_gd) { o.target = t; }
        o
    }
}

#[godot_api(secondary)]
impl BpEditor {
    /// Save the design and start a splash. `options` keys: `water: int` (WMO sea-state code: 0 calm,
    /// 3 Beaufort 4, 6 Beaufort 7), `target: int` (`Bp.SPLASH_TARGET_*`). Returns false when the design
    /// cannot float at all or the save failed; `launch_state()` says why. `Session.state_changed` follows.
    #[func]
    fn splash(&mut self, options: VarDictionary) -> bool {
        self.state.splash.refused = None;
        if self.model.is_none() || self.in_splash { return false; }
        let busy = {
            let services = crate::services::services();
            matches!(Self::session_state(), SessionState::Loading | SessionState::InSortie | SessionState::Lobby)
                || services.connection.try_borrow().map_or(true, |c| c.as_ref().is_some_and(|c| c.net != crate::enums::NetRole::Offline))
                || services.handoff.try_borrow().map_or(true, |h| h.sortie.is_some())
        };
        if busy { return self.refuse("dock.splash.busy"); }
        // A result of an earlier splash that arrived after it was given up must not end this one.
        {
            let services = crate::services::services();
            if let Ok(mut h) = services.handoff.try_borrow_mut() {
                if h.finished.as_ref().is_some_and(|f| f.kind == SessionKind::Splash) { h.finished = None; }
            };
        }
        let opts = self.read_options(&options);
        self.state.splash_options = opts;
        self.state.splash_seed = SPLASH_SEED;
        if !self.floats_now() { return self.refuse("dock.splash.cannot_float"); }
        if !self.save_for_launch() { return self.refuse("dock.splash.save_failed"); }
        let Some(m) = self.model.as_ref() else { return false };
        let rev = m.rev;
        let base = Arc::new(canonical(&m.blueprint));
        let services = crate::services::services();
        let progressive = services.settings.borrow().bool("editor.progressive_splash");
        let prepared = self.state.results.prepared.as_ref()
            .filter(|(r, p, _)| *r == rev && p.blueprint_hash == base.header.hash && p.config.splash == Some(opts) && p.config.seed == SPLASH_SEED)
            .map(|(_, p, _)| p.clone());
        let prebuilt = prepared.is_some();
        let stage = if progressive { sim_systems::editor::splash::SplashStage::ShipOnly } else { sim_systems::editor::splash::SplashStage::Full };
        let start = match prepared {
            Some(prepared) => crate::session::host::HostStart::Splash { prepared, stage },
            None => {
                let config = sim_systems::editor::splash::splash_config(&base, opts, SPLASH_SEED, &services.campaign());
                let config = if progressive { sim_systems::editor::splash::progressive(&config) } else { config };
                crate::session::host::HostStart::New { config: Box::new(config) }
            }
        };
        services.handoff.borrow_mut().sortie = Some(crate::session::PendingSortie { kind: SessionKind::Splash, start, options: crate::session::host::SortieOptions { checkpoints: false, fixed_step: false } });
        services.tutorial_signal(sim_systems::tutorial::Signal::SplashStarted);
        drop(services);
        self.state.splash.count += 1;
        self.state.splash.run = Some(SplashRun {
            pressed: Instant::now(), loaded_ms: None, prebuilt, applied: base.header.hash, base, seen_session: false, stop_sent: None, apply_sent: None, returned: false, own_side: SideId(0), obs: Observed::default(),
        });
        self.in_splash = true;
        true
    }
    /// The options the next splash will use (last used, or what the tutorial set).
    #[func]
    fn splash_options(&self) -> VarDictionary {
        let o = self.state.splash_options;
        d! { "water" => o.water.0 as i64, "target" => o.target.to_gd() }
    }
    /// Choose the options the next splash uses (same keys as `splash()`; a missing key keeps its value), so
    /// Space and the Splash button start the basin the player picked. Added by `ui-editor-splash`: the
    /// options popup had no other way to set them.
    #[func]
    fn set_splash_options(&mut self, options: VarDictionary) {
        let o = self.read_options(&options);
        if o != self.state.splash_options {
            self.state.splash_options = o;
            // The pre-built basin was for the old options (as the tutorial's own option change does).
            self.state.prebuild_rev = 0;
        }
    }
    /// True from `splash()` until the basin closes.
    #[func]
    fn in_splash(&self) -> bool { self.in_splash }
    /// Why the last `splash()` or `launch()` refused, as a `MsgDict` (empty when it did not refuse), for
    /// the dock's status line. Added by `ui-editor-shell`.
    #[func]
    fn refusal(&self) -> VarDictionary { msg_dict(&self.state.splash.refused.clone().unwrap_or_default()) }
    /// Load timing and fallback state: `{ "last_ms": float, "p95_ms": float, "prebuilt": bool,
    /// "progressive": bool, "ready": bool }` (`ready`: a pre-built world matches the current design).
    #[func]
    fn splash_stats(&self) -> VarDictionary {
        let services = crate::services::services();
        let (prebuilt, progressive) = services.settings.try_borrow().map_or((false, false), |s| (s.bool("editor.prebuilt_splash"), s.bool("editor.progressive_splash")));
        let opts = self.state.splash_options;
        let ready = self.model.as_ref().is_some_and(|m| self.state.results.prepared.as_ref().is_some_and(|(r, p, _)| *r == m.rev && p.config.splash == Some(opts) && p.config.seed == self.state.splash_seed));
        d! { "last_ms" => self.state.splash.last_ms, "p95_ms" => self.state.splash.p95(), "prebuilt" => prebuilt, "progressive" => progressive, "ready" => ready }
    }
    /// Rewind the basin by up to 30 s.
    #[func]
    fn splash_rewind(&mut self, seconds: f64) -> bool {
        if !self.in_splash || Self::session_state() != SessionState::InSortie { return false; }
        let Some(ticks) = rewind_ticks(seconds) else { return false };
        if !self.send_command(Command::SplashRewind { ticks }) { return false; }
        // A rewind across a live edit brings back the design of that moment (the simulation restores it with
        // the clock). The edits are the player's current design, not part of the past: push them again.
        if let Some(run) = self.state.splash.run.as_mut() { run.applied = ContentHash::EMPTY; }
        self.push_edits();
        true
    }
    /// Start the same basin again from the beginning.
    #[func]
    fn splash_restart(&mut self) -> bool {
        if !self.in_splash || Self::session_state() != SessionState::InSortie { return false; }
        if !self.send_command(Command::SplashRestart) { return false; }
        if let Some(run) = self.state.splash.run.as_mut() {
            // The basin restarts with the design it was launched with.
            run.applied = run.base.header.hash;
            run.obs = Observed::default();
        }
        true
    }
    /// Push the edits made since the splash began into the running basin. Kits, tuning, rules, graphs and
    /// scripts apply live; if the hull shell changed the basin closes instead and `returned_to_dock` fires.
    #[func]
    fn splash_apply_edits(&mut self) -> bool {
        if !self.in_splash || Self::session_state() != SessionState::InSortie { return false; }
        self.push_edits()
    }
    /// End the splash and return to dry dock; `splash_ended` carries the report card.
    #[func]
    fn splash_end(&mut self) {
        self.end_splash();
    }
    /// The Launch button outside Splash: in a campaign briefing commit the refit and hand the sortie to the
    /// session; in a lobby lock the design in. Returns false when `launch_state().can_launch` is false.
    #[func]
    fn launch(&mut self) -> bool {
        self.launch_design()
    }
}

impl BpEditor {
    /// Push the dock's design into the running basin if it differs from what the basin last got. False when
    /// it could not go (the shell changed: the basin closes; no link).
    fn push_edits(&mut self) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        let next = canonical(&m.blueprint);
        let Some(run) = self.state.splash.run.as_ref() else { return false };
        if run.returned || run.stop_sent.is_some() { return false; }
        if shell_changed(&run.base, &next) {
            self.return_to_dock("dock.splash.shell_changed");
            return false;
        }
        if next.header.hash == run.applied { return true; }
        let hash = next.header.hash;
        // The host's blueprint store gets the edited design first, so a hull the edit replaces can be drawn
        // when it is revealed under its new hash.
        {
            let packed = content::share::pack(&content::blueprint::Shareable::Ship(Box::new(next.clone())));
            let services = crate::services::services();
            if let Ok(conn) = services.connection.try_borrow() {
                if let Some(h) = conn.as_ref().and_then(|c| c.host.as_ref()) { h.send(crate::session::host::HostControl::PutBlueprint(packed)); }
            };
        }
        if !self.send_command(Command::SplashApplyDesign(Box::new(next))) { return false; }
        if let Some(run) = self.state.splash.run.as_mut() {
            run.applied = hash;
            run.apply_sent = Some(Instant::now());
        }
        true
    }
    fn end_splash(&mut self) {
        if !self.in_splash { return; }
        let running = matches!(Self::session_state(), SessionState::Loading | SessionState::InSortie);
        if running && self.stop_basin() { return; }
        // Nothing is running to stop (it already ended, or never started): close now with what there is.
        let finished = {
            let services = crate::services::services();
            let Ok(mut h) = services.handoff.try_borrow_mut() else { return };
            if h.sortie.as_ref().is_some_and(|p| p.kind == SessionKind::Splash) { h.sortie = None; }
            if h.finished.as_ref().is_some_and(|f| f.kind == SessionKind::Splash) { h.finished.take() } else { None }
        };
        let (summary, result) = match finished { Some(f) => (Some(f.summary), f.result), None => (None, None) };
        self.finish_splash(summary.as_ref(), result.as_deref());
    }
    fn launch_design(&mut self) -> bool {
        self.state.splash.refused = None;
        if self.in_splash || self.model.is_none() { return false; }
        // One answer for the button and the action: the analysis unit's launch state (validation, and in a
        // campaign whether the Salvage on hand pays for the refit).
        let can = self.launch_state().get("can_launch").and_then(|v| v.try_to::<bool>().ok()).unwrap_or(false);
        if !can { return false; }
        if !self.save_for_launch() { return self.refuse("dock.splash.save_failed"); }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewind_is_clamped_to_thirty_seconds() {
        assert_eq!(rewind_ticks(10.0), Some(600));
        assert_eq!(rewind_ticks(30.0), Some(1_800));
        assert_eq!(rewind_ticks(500.0), Some(1_800));
        assert_eq!(rewind_ticks(0.001), Some(1));
        for bad in [0.0, -3.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] { assert_eq!(rewind_ticks(bad), None, "{bad}"); }
    }

    #[test]
    fn water_snaps_to_the_three_basins() {
        assert_eq!(water_of(0), SeaState::CALM);
        assert_eq!(water_of(-7), SeaState::CALM);
        assert_eq!(water_of(3), SeaState::BEAUFORT_4);
        assert_eq!(water_of(2), SeaState::BEAUFORT_4);
        assert_eq!(water_of(6), SeaState::BEAUFORT_7);
        assert_eq!(water_of(i64::MAX), SeaState::BEAUFORT_7);
    }

    #[test]
    fn fallbacks_follow_the_load_gates() {
        assert_eq!(fallbacks(800.0, false), (false, false));
        assert_eq!(fallbacks(1_200.0, false), (true, false), "a splash over 1 s turns the pre-built splash on");
        assert_eq!(fallbacks(2_500.0, false), (true, false), "only a pre-built splash over 2 s turns progressive on");
        assert_eq!(fallbacks(2_500.0, true), (true, true));
        assert_eq!(fallbacks(1_500.0, true), (true, false));
        assert_eq!(fallbacks(f64::NAN, true), (false, false));
    }

    #[test]
    fn p95_is_nearest_rank_over_the_window() {
        let mut s = SplashState::default();
        assert_eq!(s.p95(), 0.0);
        for i in 1..=40 { s.record(i as f64 * 10.0); }
        assert_eq!(s.samples.len(), LOAD_WINDOW);
        // The window holds 210..=400; rank ceil(0.95 * 20) = 19 is 390.
        assert_eq!(s.p95(), 390.0);
        assert_eq!(s.last_ms, 400.0);
    }

    #[test]
    fn observations_forget_what_a_rewind_undoes() {
        let mut o = Observed::default();
        for t in 0..=1_200u64 {
            o.at(t);
            o.heel_deg(t, if t == 900 { 22.0 } else { 4.0 });
        }
        o.hit(1_000);
        o.target_out(EntityId::from_u64(7), 1_100);
        assert_eq!(o.peak_heel_deg(), 22.0);
        assert_eq!(o.first_hit_s(), Some(1_000.0 / 60.0));
        // Small jitter backwards is not a rewind.
        o.at(1_198);
        assert_eq!(o.first_hit, Some(1_000));
        // Back to tick 600: the heel peak at tick 900 (second 15), the hit and the loss are undone.
        o.at(600);
        assert_eq!(o.first_hit, None);
        assert!(o.targets_out.is_empty());
        assert_eq!(o.peak_heel_deg(), 4.0);
        o.at(601);
        o.hit(700);
        assert_eq!(o.first_hit_s(), Some(700.0 / 60.0));
        o.heel_deg(601, f64::NAN);
        assert_eq!(o.peak_heel_deg(), 4.0);
    }

    #[test]
    fn heel_reads_the_beam_axis() {
        assert!(heel_of([0.0, 0.0, 0.0, 1.0]).abs() < 1e-6);
        let q = glam::Quat::from_rotation_x(20f32.to_radians());
        assert!((heel_of(q.to_array()) - 20.0).abs() < 1e-3);
        let q = glam::Quat::from_rotation_y(1.0) * glam::Quat::from_rotation_x(-30f32.to_radians());
        assert!((heel_of(q.to_array()) - 30.0).abs() < 1e-3, "heading does not count, and port or starboard reads the same");
        assert_eq!(heel_of([f32::NAN, 0.0, 0.0, 1.0]), 0.0);
        assert_eq!(heel_of([0.0; 4]), 0.0);
    }

    fn stats(fired: u32, hit: u32, lost: f64) -> SortieStats { SortieStats { rounds_fired: fired, rounds_hit: hit, own_cells_lost_fraction: lost, enemy_tonnage_total: 100.0, enemy_tonnage_disabled: 50.0, ..Default::default() } }

    #[test]
    fn the_card_takes_counters_from_the_result_when_the_summary_has_none() {
        let obs = Observed { start_tick: Some(0), last_tick: 3_600, first_hit: Some(300), heel: [(3, 12.5)].into_iter().collect(), ..Default::default() };
        let left = SortieSummary { outcome: Outcome::Aborted, duration_ticks: 3_600, ..Default::default() };
        let rs = stats(40, 10, 0.1);
        let card = build_card(Some(&left), Some(ResultFacts { stats: &rs, duration_ticks: 3_600, kills: 2 }), &obs, |_, _, _| Msg::new("splash.tip.none"));
        assert_eq!(card.rounds_fired, 40);
        assert!((card.hit_rate - 0.25).abs() < 1e-9);
        assert_eq!(card.duration_s, 60.0);
        assert_eq!(card.targets_destroyed, 2);
        assert_eq!(card.time_to_first_hit_s, Some(5.0));
        assert_eq!(card.peak_heel_deg, 12.5);
        assert!((card.score - (50.0 - 5.0)).abs() < 1e-9, "100 x 0.5 disabled - 50 x 0.1 lost");
        assert_eq!(card.tip, Msg::new("splash.tip.none"));
        // The simulation's own tip wins.
        let summary = SortieSummary { stats: stats(5, 5, 0.0), duration_ticks: 600, tip: Some(Msg::new("splash.tip.masked")), ..Default::default() };
        let card = build_card(Some(&summary), None, &Observed::default(), |_, _, _| unreachable!("the summary has a tip"));
        assert_eq!((card.rounds_fired, card.hit_rate, card.time_to_first_hit_s), (5, 1.0, None));
        assert_eq!(card.tip, Msg::new("splash.tip.masked"));
        // Nothing at all: a card of zeros over the ticks seen.
        let seen = Observed { start_tick: Some(60), last_tick: 660, ..Default::default() };
        let card = build_card(None, None, &seen, |s, h, l| { assert_eq!((s, h, l), (0, 0, 0.0)); Msg::new("splash.tip.no_shots") });
        assert_eq!(card.duration_s, 10.0);
        assert_eq!(card.score, 0.0);
        assert_eq!(card.tip, Msg::new("splash.tip.no_shots"));
    }

    #[test]
    fn the_fallback_tip_follows_the_simulation_order() {
        let note = |tier: NoteTier, key: &str| ShipNote { tier, kind: sim_systems::editor::notes::NoteKind::Advisory("t".into()), part: None, affected_parts: 0, text: Msg::new(key), fix: None, flips_tag: false, score_effect: None };
        let mut g = GaugeStrip::default();
        assert_eq!(fallback_tip(&[], None, 0, 0, 0.0, true), Msg::new("splash.tip.no_shots"));
        assert_eq!(fallback_tip(&[], None, 0, 0, 0.0, false), Msg::new("splash.tip.none"));
        assert_eq!(fallback_tip(&[], None, 40, 2, 0.0, true).key.as_str(), "splash.tip.low_hit_rate");
        assert_eq!(fallback_tip(&[], None, 10, 1, 0.0, true), Msg::new("splash.tip.none"), "too few shots to judge");
        assert_eq!(fallback_tip(&[], None, 10, 8, 0.4, true).key.as_str(), "splash.tip.heavy_damage");
        g.power.check = Check::Caution;
        g.sight.check = Check::Fail;
        assert_eq!(fallback_tip(&[], Some(&g), 0, 0, 0.0, true), Msg::new("splash.tip.gauge.sight"), "Fail before Caution");
        let notes = [note(NoteTier::Advisory, "a"), note(NoteTier::CannotWork, "b")];
        assert_eq!(fallback_tip(&notes, Some(&g), 0, 0, 0.0, true), Msg::new("b"));
        let notes = [note(NoteTier::CannotWork, "b"), note(NoteTier::CannotFloatMoveOrPower, "c")];
        assert_eq!(fallback_tip(&notes, None, 0, 0, 0.0, true), Msg::new("c"));
        assert_eq!(fallback_tip(&[note(NoteTier::Advisory, "a")], None, 3, 3, 0.0, true), Msg::new("a"));
    }

    #[test]
    fn a_changed_shell_is_seen_and_a_repaint_is_not() {
        let bp = content::campaign::load::stock_blueprint("starters/patrol15.bp.ron").expect("starter");
        let base = canonical(&bp);
        let mut painted = bp.clone();
        if let Some(p) = painted.paint.first_mut() { p.rgb = [0.3, 0.3, 0.32]; }
        assert!(!shell_changed(&base, &canonical(&painted)));
        let mut lower = bp.clone();
        lower.hull.design_waterline_y += 1;
        assert!(shell_changed(&base, &canonical(&lower)));
    }

    #[test]
    fn every_splash_key_has_an_english_row() {
        let csv = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/i18n/dock.csv")).expect("dock.csv");
        for k in SPLASH_KEYS { assert!(csv.lines().any(|l| l.starts_with(&format!("{k},"))), "{k} has no row in dock.csv"); }
    }
}
