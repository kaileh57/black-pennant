//! Part of unit `gd-session`. The kill cam over a live sortie (L1049).
//!
//! Decision: the clip is recorded on the client from what the link delivered, not cut from the server's
//! replay. Neither seam can hand the live recording to the main thread (a host's `ReplaySink` lives inside
//! the session on the clock thread, and a joined client has none), and the client's own stream is exactly
//! what L1049 allows: the killed player's side knowledge, plus the killer, which the server keeps drawable
//! over the window it announces with `SimEvent::KillCam`.
//!
//! [`ClipRecorder`] keeps the last [`KEEP_TICKS`] of sampled frames (at 15 Hz, the replay pose rate) and
//! every event, and folds events that age out of the ring into a base state: each drawable entity's
//! introduction with its damage, removed cells and status, the weather and the pacing. A clip starts from
//! that base advanced to the clip's first tick, so it opens the way a replay seek does: `SortieStarted`,
//! the weather, the pacing, then an `EntityRevealed` with damage so far for everything drawable, and every
//! renderer resets on the first of those. Stopping the clip feeds the same opening for the live state.
use super::super::link::channel::Presenter;
use crate::seams::{InterpTuning, LinkInbox, WireEvent};
use content::catalogue::Catalogue;
use sim_core::events::PhysEvent;
use sim_core::EntityId;
use sim_systems::protocol::config::EntityIntro;
use sim_systems::protocol::event::SimEvent;
use sim_systems::protocol::Snapshot;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

/// Ring length in simulation ticks: the 10 s window (`sim::interest::KILL_CAM_TICKS`) and 2 s in hand for
/// the interpolation delay and a late press.
pub(crate) const KEEP_TICKS: u64 = sim_systems::sim::interest::KILL_CAM_TICKS + 120;
/// Frame spacing of the ring in ticks: 15 Hz, the replay pose rate (`content::replay::REPLAY_POSE_HZ`).
const FRAME_EVERY_TICKS: f64 = (sim_core::TICK_HZ / content::replay::REPLAY_POSE_HZ) as f64;
/// The clip opens this many ticks before its window so the first drawn moment is the window's start.
const LEAD_TICKS: f64 = 2.0 * FRAME_EVERY_TICKS;
/// Longest frame step a clip advances by (a hitch does not skip the clip).
const MAX_STEP_S: f64 = 0.25;

/// One entity as of some tick: its introduction, damage by cell, cells removed.
#[derive(Clone, Debug)]
struct Folded { intro: EntityIntro, damage: BTreeMap<u32, u8>, removed: BTreeSet<u32> }

impl Folded {
    fn new(intro: &EntityIntro) -> Self {
        let mut f = Folded { intro: intro.clone(), damage: BTreeMap::new(), removed: BTreeSet::new() };
        for c in &intro.damage { f.damage.insert(c.cell.0, c.bucket); }
        f.removed.extend(intro.removed.iter().map(|c| c.0));
        f.intro.damage.clear();
        f.intro.removed.clear();
        f
    }
    fn intro(&self) -> EntityIntro {
        let mut i = self.intro.clone();
        i.damage = self.damage.iter().map(|(&cell, &bucket)| content::cell::CellChange { cell: content::ids::CellIndex(cell), bucket }).collect();
        i.removed = self.removed.iter().map(|&c| content::ids::CellIndex(c)).collect();
        i
    }
}

/// The state a clip or a return to live opens with.
#[derive(Clone, Debug, Default)]
struct State { weather: Option<WireEvent>, pacing: Option<WireEvent>, entities: BTreeMap<EntityId, Folded> }

impl State {
    fn fold(&mut self, e: &WireEvent) {
        match &e.event {
            SimEvent::EntityRevealed(i) => { self.entities.insert(i.entity, Folded::new(i)); }
            SimEvent::EntityHidden { entity } => { self.entities.remove(entity); }
            SimEvent::PacingChanged(_) => self.pacing = Some(e.clone()),
            SimEvent::Phys(p) => match p {
                PhysEvent::WeatherChanged { .. } => self.weather = Some(e.clone()),
                PhysEvent::CellsDamaged { vessel, changes, .. } => if let Some(f) = self.entities.get_mut(vessel) { for c in changes { f.damage.insert(c.cell.0, c.bucket); } },
                PhysEvent::Split { vessel, cells, .. } | PhysEvent::Debris { vessel, cells, .. } => if let Some(f) = self.entities.get_mut(vessel) { f.removed.extend(cells.iter().map(|c| c.0)); },
                PhysEvent::StatusChanged { vessel, status } => if let Some(f) = self.entities.get_mut(vessel) { f.intro.status = *status; },
                PhysEvent::VesselDespawned { vessel } => { self.entities.remove(vessel); }
                PhysEvent::WreckDespawned { wreck } => { self.entities.remove(wreck); }
                _ => {}
            },
            _ => {}
        }
    }
    /// Events that rebuild this state on a client that knows nothing, stamped `tick`.
    fn opening(&self, started: Option<&WireEvent>, tick: u64) -> Vec<WireEvent> {
        let stamp = |e: &WireEvent| WireEvent { tick, event: e.event.clone() };
        let mut out = Vec::with_capacity(self.entities.len() + 3);
        out.push(started.map(stamp).unwrap_or(WireEvent { tick, event: SimEvent::SortieStarted { seed: 0, tick_hz: sim_core::TICK_HZ } }));
        out.extend(self.weather.iter().map(stamp));
        out.extend(self.pacing.iter().map(stamp));
        out.extend(self.entities.values().map(|f| WireEvent { tick, event: SimEvent::EntityRevealed(Box::new(f.intro())) }));
        out
    }
}

/// The rolling record of a live sortie on this client.
#[derive(Default)]
pub(crate) struct ClipRecorder {
    started: Option<WireEvent>,
    /// State as of the oldest event still in `events`.
    base: State,
    events: VecDeque<WireEvent>,
    /// (render tick, sampled snapshot), ascending.
    frames: VecDeque<(f64, Arc<Snapshot>)>,
}

impl ClipRecorder {
    /// Forget everything (a new sortie).
    pub(crate) fn clear(&mut self) { *self = Self::default(); }

    /// Record this frame's events; a `SortieStarted` begins a new record.
    pub(crate) fn record_events(&mut self, events: &[WireEvent]) {
        for e in events {
            if matches!(e.event, SimEvent::SortieStarted { .. }) {
                self.clear();
                self.started = Some(e.clone());
                continue;
            }
            self.events.push_back(e.clone());
        }
        self.trim();
    }

    /// Record a sampled frame; kept at 15 Hz of render ticks.
    pub(crate) fn record_frame(&mut self, render_tick: f64, snapshot: &Arc<Snapshot>) {
        if !render_tick.is_finite() { return; }
        if let Some((last, _)) = self.frames.back() {
            // A step back (a rewind) starts the frame ring again.
            if render_tick + 1e-9 < *last { self.frames.clear(); }
            else if render_tick - *last < FRAME_EVERY_TICKS { return; }
        }
        self.frames.push_back((render_tick, snapshot.clone()));
        self.trim();
    }

    fn newest_tick(&self) -> u64 {
        let e = self.events.back().map_or(0, |e| e.tick);
        let f = self.frames.back().map_or(0, |f| f.0.max(0.0) as u64);
        e.max(f)
    }

    fn trim(&mut self) {
        let start = self.newest_tick().saturating_sub(KEEP_TICKS);
        while self.events.front().is_some_and(|e| e.tick < start) {
            if let Some(e) = self.events.pop_front() { self.base.fold(&e); }
        }
        while self.frames.front().is_some_and(|f| f.0 < start as f64) { self.frames.pop_front(); }
    }

    /// A clip of `[from_tick, to_tick]`, clamped to what the ring holds; `None` when it holds no frame of it.
    pub(crate) fn clip(&self, from_tick: u64, to_tick: u64, catalogue: Arc<Catalogue>) -> Option<ClipPlayer> {
        if to_tick < from_tick { return None; }
        let first = self.frames.front()?.0;
        let from = (from_tick as f64).max(first);
        let to = (to_tick as f64).min(self.frames.back()?.0.max(self.newest_tick() as f64));
        if to <= from { return None; }
        let start = (from - LEAD_TICKS).max(first);
        let mut state = self.base.clone();
        for e in self.events.iter().filter(|e| (e.tick as f64) < start) { state.fold(e); }
        let opening = state.opening(self.started.as_ref(), start.floor() as u64);
        let events: VecDeque<WireEvent> = self.events.iter().filter(|e| (e.tick as f64) >= start && (e.tick as f64) <= to).cloned().collect();
        let frames: VecDeque<(f64, Arc<Snapshot>)> = self.frames.iter().filter(|f| f.0 >= start && f.0 <= to).cloned().collect();
        if frames.is_empty() { return None; }
        let tuning = InterpTuning { interval_s: 1.0 / content::replay::REPLAY_POSE_HZ as f64, delay_intervals: 2.0, ..InterpTuning::default() };
        Some(ClipPlayer { presenter: Presenter::new(tuning, catalogue), opening, events, frames, clock_tick: start, end_tick: to, last_now: None, finished: false })
    }

    /// The opening that brings a client back to the live state after a clip.
    pub(crate) fn live_opening(&self) -> Vec<WireEvent> {
        let mut state = self.base.clone();
        for e in &self.events { state.fold(e); }
        state.opening(self.started.as_ref(), self.newest_tick())
    }
}

/// Plays one clip through its own presenter at 1x, holding the last frame at the end.
pub(crate) struct ClipPlayer {
    presenter: Presenter,
    opening: Vec<WireEvent>,
    events: VecDeque<WireEvent>,
    frames: VecDeque<(f64, Arc<Snapshot>)>,
    clock_tick: f64,
    end_tick: f64,
    last_now: Option<f64>,
    pub(crate) finished: bool,
}

impl ClipPlayer {
    /// Advance by wall time and fill `inbox` as a link would.
    pub(crate) fn poll(&mut self, now_s: f64, inbox: &mut LinkInbox) {
        let dt = self.last_now.map_or(0.0, |l| (now_s - l).clamp(0.0, MAX_STEP_S));
        if self.last_now.is_none() {
            for e in std::mem::take(&mut self.opening) { self.presenter.push_event(&e); inbox.events.push(e); }
        }
        self.last_now = Some(now_s);
        if !self.finished {
            self.clock_tick += dt * sim_core::TICK_HZ as f64;
            if self.clock_tick >= self.end_tick { self.clock_tick = self.end_tick; self.finished = true; }
        }
        while self.events.front().is_some_and(|e| e.tick as f64 <= self.clock_tick) {
            if let Some(e) = self.events.pop_front() { self.presenter.push_event(&e); inbox.events.push(e); }
        }
        while self.frames.front().is_some_and(|f| f.0 <= self.clock_tick) {
            if let Some((t, s)) = self.frames.pop_front() { self.presenter.push_snapshot((*s).clone(), Duration::from_secs_f64(t.max(0.0) / sim_core::TICK_HZ as f64)); }
        }
        self.presenter.sample(Duration::from_secs_f64(self.clock_tick.max(0.0) / sim_core::TICK_HZ as f64), inbox);
    }
}

#[cfg(test)]
mod tests;
