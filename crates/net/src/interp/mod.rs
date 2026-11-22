//! Unit `net-interp` (hard: the extrapolation gate and the 0.5 m own-hull correction gate depend on it).
//! Client-side presentation of received state. DESIGN.md L910 (remote vessels drawn 100 to 150 ms in the
//! past; corrections blend through a decaying error offset with a 150 ms half-life; errors over 10 m snap),
//! L914-916 (default delay 4 intervals = 133 ms; gate: extrapolate in 1% or fewer of rendered frames at
//! 150 ms, 5% loss, 30 ms jitter; fallback jitter-adaptive delay 2 intervals + 2 x p95 jitter over 5 s,
//! clamped 3 to 4.5 intervals, moved by changing playback rate up to 5%, never jumping), L926 (shells
//! integrated on the client with the same drag model; guided bodies interpolated between 5 Hz updates), L935
//! (fallback 1 sets the delay to 3 x 50 ms), L939 (client shells within 0.5 m of the server's at impact).
//!
//! Timeline: frames are keyed by their sim tick, but tick spacing in real time varies with time compression,
//! the overload time scale and pause, so the buffer maps arrival time to sim tick with a smoothed playback
//! clock and samples at `playback tick = newest tick estimate - delay`. Each entity keeps its own sample ring
//! (an item may be absent from a frame when the accumulator deferred it), and interpolation uses that
//! entity's two samples around the playback tick; past the newest sample it extrapolates from velocity and
//! counts the frame as extrapolated.
//!
//! [`SnapshotBuffer::sample`] returns the same `Snapshot` type `ReplayPlayer::snapshot` returns, so gdclient's
//! render path is one path for live play, replays, the kill cam and golden captures. Poses of entities not yet
//! revealed by an `EntityRevealed` event are held back until the reveal arrives (channels are independent).
//!
//! # Choices this unit makes where the spec is silent (design decisions)
//! - The newest-tick estimate is the mean of `k + rate x (now - arrival)` over the frames that arrived in the
//!   last two seconds: the tick the client typically holds by now. The fixed delay of 4 intervals is taken
//!   behind it, so the buffer holds 133 ms beyond typical delivery and late frames eat into that margin
//!   rather than into the delay itself. (Against the fastest deliveries instead, the jitter spread comes out
//!   of the 133 ms: at the L915 profile through the conditioner that measured 1.2% of frames extrapolated
//!   against 0.7% this way, for about 30 ms more display delay under 30 ms of jitter and none on a clean
//!   link.) The tick rate is the least-squares slope of (arrival, tick) over the adaptive window once a
//!   pacing epoch has a second of frames, and the pacing section's nominal rate (60 x compression x time
//!   scale, 0 while paused) before that. A change of pause, compression or time scale starts a new epoch.
//! - The playback clock never jumps for jitter or delay changes: it runs at the tick rate times a playback
//!   rate within 1 +/- `max_rate_change`, steering toward the target with a 1 s time constant, in both delay
//!   modes. It snaps only when it is more than a second off (first frame, a stall, a compression step that
//!   the pacing section announced late) and never runs backwards otherwise.
//! - Jitter is each frame's lateness behind the newest-tick estimate at its arrival, in seconds (0 for an
//!   early frame); the p95 over the adaptive window feeds the adaptive delay (2 intervals + 2 x p95, clamped
//!   to 3 to 4.5 intervals, L916) and is reported in both modes.
//! - A rendered frame counts as extrapolated when the playback tick is past the newest frame received: the
//!   client ran out of snapshots (L915). A vessel whose own newest sample is older (deferred by the priority
//!   accumulator or shed over the cap) is extrapolated from its velocity silently, for at most
//!   [`MAX_ENTITY_EXTRAPOLATION_TICKS`], then held; the playback tick itself is never more than
//!   [`MAX_EXTRAPOLATION_S`] past the newest frame.
//! - Positions interpolate with cubic Hermite curves through both samples' velocities (hulls and missiles
//!   carry velocity, so a 5 Hz missile still curves smoothly); rotations slerp; turret train takes the short
//!   way round. Discrete state (status, flags, phase, track pairing) comes from the sample at or before the
//!   playback tick.
//! - An entity's end comes from its events (hide, despawn, impact, burst, expiry) at their exact tick, and
//!   from its removal from a frame otherwise; it stops being drawn when the playback tick reaches that tick.
//!   A vessel or decoy that appears is drawn at its first pose until the playback tick reaches it; a body
//!   (missile, torpedo) is not drawn before its first sample, so it never hangs in the air at launch.
//! - Non-entity sections (tracks, ship status, allies, alerts, objectives, pacing) come from the newest frame
//!   at or before the playback tick, so plot symbols and HUD stay in step with the drawn hulls.
//! - Client shells end at their impact or splash point: [`ShellTracker::close`] keeps a shell until the
//!   playback tick reaches the impact, so it disappears where the effect plays instead of a delay early. A
//!   shell the client never hears the end of is dropped below the deepest wave trough or past the server's
//!   own flight limit.
use crate::delta::{DecodedFrame, ItemKey};
use crate::msg::WireEvent;
use glam::{DQuat, DVec3};
use serde::{Deserialize, Serialize};
use sim_core::events::PhysEvent;
use sim_core::math::{Kinematics, Pose};
use sim_core::projectiles::{ProjectileId, ProjectileSpawn};
use sim_core::EntityId;
use sim_systems::protocol::event::SimEvent;
use sim_systems::protocol::snapshot::{BodyView, DecoyView, Pacing, Relevance, Snapshot, TurretView, VesselView};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

mod clock;
#[cfg(test)]
mod tests;

use clock::Playback;

/// Sim tick rate (ticks per sim second).
const TICK_HZ: f64 = sim_core::TICK_HZ as f64;
/// Longest the playback tick may run past the newest frame before it holds (0.25 s at the current rate).
pub const MAX_EXTRAPOLATION_S: f64 = 0.25;
/// Longest one entity is extrapolated past its own newest sample before its pose is held (0.5 s).
pub const MAX_ENTITY_EXTRAPOLATION_TICKS: f64 = 30.0;
/// Samples kept per entity at most (a little over 2 s at 30 Hz).
const MAX_SAMPLES: usize = 72;
/// Frames kept for their non-entity sections at most.
const MAX_FRAMES: usize = 96;
/// An ended entity's record is forgotten this many ticks after the playback tick passed its end.
const FORGET_TICKS: f64 = 120.0;
/// A gap between two samples longer than this interpolates linearly (Hermite tangents over a long gap
/// overshoot).
const HERMITE_MAX_GAP_TICKS: u64 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct InterpTuning {
    /// Snapshot interval in seconds (1/30; 1/20 under fallback 1).
    pub interval_s: f64,
    /// Fixed delay in intervals (default 4).
    pub delay_intervals: f64,
    /// Jitter-adaptive fallback (L916).
    pub adaptive: bool,
    pub adaptive_min_intervals: f64,
    pub adaptive_max_intervals: f64,
    pub adaptive_window_s: f64,
    /// Largest playback-rate change while moving between delays.
    pub max_rate_change: f64,
    pub correction_half_life_s: f64,
    pub snap_error_m: f64,
}
impl Default for InterpTuning {
    fn default() -> Self { Self { interval_s: 1.0 / 30.0, delay_intervals: 4.0, adaptive: false, adaptive_min_intervals: 3.0, adaptive_max_intervals: 4.5, adaptive_window_s: 5.0, max_rate_change: 0.05, correction_half_life_s: 0.15, snap_error_m: 10.0 } }
}
impl InterpTuning {
    /// Fallback 1 of L935: 20 Hz other vessels, delay 3 x 50 ms.
    pub fn fallback_20hz() -> Self { Self { interval_s: 1.0 / 20.0, delay_intervals: 3.0, ..Self::default() } }

    /// The interpolation delay in seconds for a measured p95 jitter (fixed or adaptive, L914-916).
    pub(crate) fn delay_s(&self, jitter_p95_s: f64) -> f64 {
        let iv = finite_or(self.interval_s, 1.0 / 30.0).max(1e-3);
        if self.adaptive {
            let lo = finite_or(self.adaptive_min_intervals, 3.0).max(0.0);
            let hi = finite_or(self.adaptive_max_intervals, 4.5).max(lo);
            let j = finite_or(jitter_p95_s, 0.0).max(0.0);
            (2.0 * iv + 2.0 * j).clamp(lo * iv, hi * iv)
        } else {
            finite_or(self.delay_intervals, 4.0).max(0.0) * iv
        }
    }
    /// The jitter window in seconds, sanitised.
    pub(crate) fn window_s(&self) -> f64 { finite_or(self.adaptive_window_s, 5.0).clamp(1.0, 60.0) }
    /// The largest playback-rate change, sanitised.
    pub(crate) fn max_change(&self) -> f64 { finite_or(self.max_rate_change, 0.05).clamp(0.0, 0.5) }
}

fn finite_or(v: f64, d: f64) -> f64 { if v.is_finite() { v } else { d } }

/// Counters for the extrapolation gate (L915).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InterpStats { pub frames: u64, pub extrapolated: u64, pub delay_s: f64, pub jitter_p95_s: f64, pub playback_rate: f64 }

/// One sampled render frame.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderFrame {
    /// Interpolated state at the playback tick.
    pub snapshot: Snapshot,
    /// Playback position in sim ticks (fractional); `floor` goes into `client_render_tick` (L949).
    pub render_tick: f64,
    pub extrapolating: bool,
}

/// The samples around a playback tick: the newest at or before it and the oldest after it.
type Around<'a, T> = (Option<&'a (u64, T)>, Option<&'a (u64, T)>);

/// One entity's samples, ascending tick, at most one per tick.
#[derive(Clone, Debug)]
pub(crate) struct Ring<T> { pub(crate) samples: VecDeque<(u64, T)> }
impl<T> Default for Ring<T> { fn default() -> Self { Self { samples: VecDeque::new() } } }
impl<T> Ring<T> {
    fn push(&mut self, tick: u64, v: T) {
        while self.samples.back().is_some_and(|(t, _)| *t >= tick) { self.samples.pop_back(); }
        self.samples.push_back((tick, v));
        while self.samples.len() > MAX_SAMPLES { self.samples.pop_front(); }
    }
    fn first_tick(&self) -> Option<u64> { self.samples.front().map(|s| s.0) }
    fn last_tick(&self) -> Option<u64> { self.samples.back().map(|s| s.0) }
    /// The samples around `r`: the newest at or before it and the oldest after it.
    fn around(&self, r: f64) -> Around<'_, T> {
        let i = self.samples.partition_point(|(t, _)| (*t as f64) <= r);
        (if i > 0 { self.samples.get(i - 1) } else { None }, self.samples.get(i))
    }
    /// Drop samples no playback at or after `r` can need: keep the newest at or before `r` and all later.
    fn prune_before(&mut self, r: f64) {
        while self.samples.len() > 2 && self.samples.get(1).is_some_and(|(t, _)| (*t as f64) <= r) { self.samples.pop_front(); }
    }
    fn drop_before(&mut self, tick: u64) { while self.samples.front().is_some_and(|(t, _)| *t < tick) { self.samples.pop_front(); } }
}

/// What the reliable event stream and the frames say about one entity's presence.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Life {
    /// An `EntityRevealed` arrived.
    pub(crate) revealed: bool,
    /// Tick from which the entity is no longer drawn (hide, despawn, impact, removal from a frame).
    pub(crate) gone_at: Option<u64>,
}

pub struct SnapshotBuffer {
    pub(crate) tuning: InterpTuning,
    pub(crate) frames: std::collections::VecDeque<(Duration, DecodedFrame)>,
    pub(crate) stats: InterpStats,
    /// The smoothed playback clock.
    pub(crate) playback: Playback,
    pub(crate) vessels: BTreeMap<EntityId, Ring<VesselView>>,
    pub(crate) bodies: BTreeMap<EntityId, Ring<BodyView>>,
    pub(crate) decoys: BTreeMap<EntityId, Ring<DecoyView>>,
    pub(crate) life: BTreeMap<EntityId, Life>,
}

impl SnapshotBuffer {
    pub fn new(tuning: InterpTuning) -> Self {
        Self { tuning, frames: Default::default(), stats: InterpStats::default(), playback: Playback::default(), vessels: BTreeMap::new(), bodies: BTreeMap::new(), decoys: BTreeMap::new(), life: BTreeMap::new() }
    }
    pub fn tuning(&self) -> &InterpTuning { &self.tuning }
    pub fn set_tuning(&mut self, tuning: InterpTuning) { self.tuning = tuning; }

    /// Add a decoded frame with its local arrival time. Unit: `net-interp`.
    ///
    /// Entities listed in the frame's `fresh` get a sample at the frame's tick; entities the buffer knows that
    /// are absent from the frame end at its tick. A frame whose tick is older than the newest one starts a
    /// new timeline (a restored keyframe): frames, samples and the clock start again, while what the reliable
    /// events said (reveals) is kept, since those events are sent once and may already have arrived for the
    /// new timeline. A new sortie is a full [`SnapshotBuffer::clear`].
    pub fn push(&mut self, frame: DecodedFrame, arrival: Duration) {
        let mut frame = frame;
        if self.newest_tick().is_some_and(|n| frame.tick < n) {
            let life = std::mem::take(&mut self.life);
            self.clear();
            self.life = life.into_iter().map(|(e, l)| (e, Life { revealed: l.revealed, gone_at: None })).collect();
        }
        let tick = frame.tick;
        self.playback.on_frame(arrival.as_secs_f64(), tick, &frame.snapshot.pacing, &self.tuning);

        let snap = &mut frame.snapshot;
        let vessels = std::mem::take(&mut snap.vessels);
        let bodies = std::mem::take(&mut snap.bodies);
        let decoys = std::mem::take(&mut snap.decoys);
        let fresh: BTreeSet<ItemKey> = frame.fresh.iter().copied().collect();
        let present_v: BTreeSet<EntityId> = vessels.iter().map(|v| v.entity).collect();
        let present_b: BTreeSet<EntityId> = bodies.iter().map(|b| b.entity).collect();
        let present_d: BTreeSet<EntityId> = decoys.iter().map(|d| d.entity).collect();
        // Items carried forward unchanged (not fresh) add no sample: their value belongs to an older tick.
        for v in vessels {
            if !fresh.contains(&ItemKey::Vessel(v.entity)) && self.vessels.contains_key(&v.entity) { continue; }
            if !(v.kin.pose.pos.is_finite() && v.kin.pose.rot.is_finite() && v.kin.lin_vel.is_finite() && v.kin.ang_vel.is_finite()) { continue; }
            self.reappear(v.entity, tick);
            self.vessels.entry(v.entity).or_default().push(tick, v);
        }
        for b in bodies {
            if !fresh.contains(&ItemKey::Body(b.entity)) && self.bodies.contains_key(&b.entity) { continue; }
            if !(b.pos.is_finite() && b.vel.is_finite()) { continue; }
            self.reappear(b.entity, tick);
            self.bodies.entry(b.entity).or_default().push(tick, b);
        }
        for d in decoys {
            if !fresh.contains(&ItemKey::Decoy(d.entity)) && self.decoys.contains_key(&d.entity) { continue; }
            if !d.pos.is_finite() { continue; }
            self.reappear(d.entity, tick);
            self.decoys.entry(d.entity).or_default().push(tick, d);
        }
        // Removal from the frame ends an entity at this tick unless an event already ended it earlier.
        let gone: Vec<EntityId> = self.vessels.keys().filter(|e| !present_v.contains(e))
            .chain(self.bodies.keys().filter(|e| !present_b.contains(e)))
            .chain(self.decoys.keys().filter(|e| !present_d.contains(e)))
            .copied().collect();
        for e in gone {
            let life = self.life.entry(e).or_default();
            if life.gone_at.is_none() { life.gone_at = Some(tick); }
        }

        self.frames.push_back((arrival, frame));
        while self.frames.len() > MAX_FRAMES { self.frames.pop_front(); }
    }

    /// A fresh sample at `tick` for an entity that had ended at or before it: it came back (re-entered visual
    /// range). Forget the earlier appearance so playback never draws it crossing the gap.
    fn reappear(&mut self, e: EntityId, tick: u64) {
        let Some(life) = self.life.get_mut(&e) else { return };
        let Some(g) = life.gone_at else { return };
        if g > tick { return; }
        life.gone_at = None;
        self.drop_samples_before(e, g);
    }

    fn drop_samples_before(&mut self, e: EntityId, tick: u64) {
        if let Some(r) = self.vessels.get_mut(&e) { r.drop_before(tick); }
        if let Some(r) = self.bodies.get_mut(&e) { r.drop_before(tick); }
        if let Some(r) = self.decoys.get_mut(&e) { r.drop_before(tick); }
    }

    /// An event ends `e` at `tick`; the earliest end wins.
    fn end(&mut self, e: EntityId, tick: u64) {
        let life = self.life.entry(e).or_default();
        life.gone_at = Some(life.gone_at.map_or(tick, |g| g.min(tick)));
    }

    /// Entity lifecycle from reliable events: `EntityRevealed`, `EntityHidden`, splits and despawns. Unit: `net-interp`.
    ///
    /// `PacingChanged` also starts a new playback epoch at once, ahead of the pacing section of the frames.
    pub fn on_event(&mut self, event: &WireEvent) {
        let tick = event.tick;
        match &event.event {
            SimEvent::EntityRevealed(intro) => {
                let e = intro.entity;
                let life = self.life.entry(e).or_default();
                life.revealed = true;
                if let Some(g) = life.gone_at.filter(|g| *g <= tick) {
                    // A new appearance: the old samples belong to the previous one.
                    life.gone_at = None;
                    self.drop_samples_before(e, g);
                }
            }
            SimEvent::EntityHidden { entity } => self.end(*entity, tick),
            SimEvent::PacingChanged(p) => self.playback.on_pacing(p),
            SimEvent::Phys(p) => match p {
                PhysEvent::VesselDespawned { vessel } => self.end(*vessel, tick),
                PhysEvent::WreckDespawned { wreck } => self.end(*wreck, tick),
                PhysEvent::DecoyExpired { decoy } => self.end(*decoy, tick),
                PhysEvent::BodyImpact { body, .. } | PhysEvent::BodyBurst { body, .. } | PhysEvent::BodyDestroyed { body, .. } | PhysEvent::BodyRecovered { body, .. } => self.end(*body, tick),
                // A split's piece is a new entity that gets its own reveal; the parent keeps its samples, so
                // the parent's (now smaller) hull is drawn without a gap.
                _ => {}
            },
            _ => {}
        }
    }

    /// Sample at local time `now`; `None` before the first frame. Updates the gate counters. Unit: `net-interp`.
    pub fn sample(&mut self, now: Duration) -> Option<RenderFrame> {
        let newest = self.newest_tick()?;
        let r = self.playback.advance(now.as_secs_f64(), newest, &self.tuning)?;
        let extrapolating = r > newest as f64 + 1e-9;
        self.stats.frames += 1;
        if extrapolating { self.stats.extrapolated += 1; }
        self.stats.delay_s = self.playback.delay_s(&self.tuning);
        self.stats.jitter_p95_s = self.playback.jitter_p95_s();
        self.stats.playback_rate = self.playback.speed();

        // Sections from the newest frame at or before the playback tick (the oldest kept before that).
        let idx = self.frames.partition_point(|(_, f)| (f.tick as f64) <= r).saturating_sub(1);
        let base = &self.frames[idx.min(self.frames.len() - 1)].1.snapshot;
        let mut snapshot = Snapshot {
            tick: r.max(0.0) as u64,
            pacing: base.pacing.clone(),
            vessels: Vec::with_capacity(self.vessels.len()),
            bodies: Vec::with_capacity(self.bodies.len()),
            decoys: Vec::with_capacity(self.decoys.len()),
            tracks: base.tracks.clone(),
            ship: base.ship.clone(),
            allies: base.allies.clone(),
            alerts: base.alerts.clone(),
            objectives: base.objectives.clone(),
        };
        for (e, ring) in &self.vessels {
            if self.drawn(*e, r, ring.first_tick(), false) { if let Some(v) = sample_vessel(ring, r) { snapshot.vessels.push(v); } }
        }
        for (e, ring) in &self.bodies {
            if self.drawn(*e, r, ring.first_tick(), true) { if let Some(b) = sample_body(ring, r) { snapshot.bodies.push(b); } }
        }
        for (e, ring) in &self.decoys {
            if self.drawn(*e, r, ring.first_tick(), false) { if let Some(d) = sample_decoy(ring, r) { snapshot.decoys.push(d); } }
        }
        // Own-side vessels first, then the others, each ascending (the order `Sim::snapshot` uses).
        snapshot.vessels.sort_by_key(|v| (!matches!(v.relevance, Relevance::Own | Relevance::OwnSide), v.entity));

        self.prune(r);
        Some(RenderFrame { snapshot, render_tick: r, extrapolating })
    }

    fn drawn(&self, e: EntityId, r: f64, first: Option<u64>, strict_start: bool) -> bool {
        let Some(life) = self.life.get(&e) else { return false };
        if !life.revealed || life.gone_at.is_some_and(|g| (g as f64) <= r) { return false; }
        !(strict_start && first.is_some_and(|f| (f as f64) > r))
    }

    /// Forget what no later playback can need.
    fn prune(&mut self, r: f64) {
        let life = &self.life;
        let ended = |e: &EntityId, last: Option<u64>| life.get(e).and_then(|l| l.gone_at).is_some_and(|g| (g as f64) + FORGET_TICKS <= r && last.is_none_or(|t| t < g));
        self.vessels.retain(|e, ring| { ring.prune_before(r); !ended(e, ring.last_tick()) });
        self.bodies.retain(|e, ring| { ring.prune_before(r); !ended(e, ring.last_tick()) });
        self.decoys.retain(|e, ring| { ring.prune_before(r); !ended(e, ring.last_tick()) });
        let (v, b, d) = (&self.vessels, &self.bodies, &self.decoys);
        // A reveal can precede its first frame by a long way (blueprint loading), so lifecycle records without
        // samples are forgotten only once they have ended.
        self.life.retain(|e, l| v.contains_key(e) || b.contains_key(e) || d.contains_key(e) || !l.gone_at.is_some_and(|g| (g as f64) + FORGET_TICKS <= r));
        // Frames: keep the newest at or before the playback tick and every later one.
        while self.frames.len() > 1 && self.frames.get(1).is_some_and(|(_, f)| (f.tick as f64) <= r) { self.frames.pop_front(); }
    }

    pub fn stats(&self) -> InterpStats { self.stats }
    /// Newest sim tick received.
    pub fn newest_tick(&self) -> Option<u64> { self.frames.back().map(|(_, f)| f.tick) }
    /// Forget everything (new sortie, rejoin). Real code.
    pub fn clear(&mut self) {
        self.frames.clear();
        self.stats = InterpStats::default();
        self.playback = Playback::default();
        self.vessels.clear();
        self.bodies.clear();
        self.decoys.clear();
        self.life.clear();
    }
}

/// Cubic Hermite through two positions with their velocities over `dt_s` seconds, at fraction `u`.
fn hermite(p0: DVec3, v0: DVec3, p1: DVec3, v1: DVec3, dt_s: f64, u: f64) -> DVec3 {
    let u2 = u * u;
    let u3 = u2 * u;
    let h00 = 2.0 * u3 - 3.0 * u2 + 1.0;
    let h10 = u3 - 2.0 * u2 + u;
    let h01 = -2.0 * u3 + 3.0 * u2;
    let h11 = u3 - u2;
    p0 * h00 + v0 * (h10 * dt_s) + p1 * h01 + v1 * (h11 * dt_s)
}

fn interp_pos(p0: DVec3, v0: DVec3, p1: DVec3, v1: DVec3, gap_ticks: u64, u: f64) -> DVec3 {
    if gap_ticks <= HERMITE_MAX_GAP_TICKS { hermite(p0, v0, p1, v1, gap_ticks as f64 / TICK_HZ, u) } else { p0.lerp(p1, u) }
}

/// Rotation advanced by a world-frame angular velocity over `dt_s`.
fn rotate_by(q: DQuat, w: DVec3, dt_s: f64) -> DQuat {
    let a = w * dt_s;
    if !a.is_finite() || a.length_squared() < 1e-18 { return q; }
    (DQuat::from_scaled_axis(a) * q).normalize()
}

/// An angle wrapped into [-pi, pi).
fn wrap_pi(d: f64) -> f64 {
    let x = (d + core::f64::consts::PI).rem_euclid(core::f64::consts::TAU) - core::f64::consts::PI;
    if x.is_finite() { x } else { 0.0 }
}

fn lerp_turrets(a: &[TurretView], b: &[TurretView], u: f64) -> Vec<TurretView> {
    a.iter().map(|t| match b.iter().find(|x| x.slot == t.slot) {
        Some(x) => {
            let train = t.train_rad as f64 + wrap_pi(x.train_rad as f64 - t.train_rad as f64) * u;
            let elev = t.elevation_rad as f64 + (x.elevation_rad as f64 - t.elevation_rad as f64) * u;
            TurretView { slot: t.slot, train_rad: wrap_pi(train) as f32, elevation_rad: elev as f32 }
        }
        None => *t,
    }).collect()
}

fn lerp_f32(a: f32, b: f32, u: f64) -> f32 { (a as f64 + (b as f64 - a as f64) * u) as f32 }

/// Seconds past a sample to extrapolate, capped.
fn extrapolation_s(from_tick: u64, r: f64) -> f64 { (r - from_tick as f64).clamp(0.0, MAX_ENTITY_EXTRAPOLATION_TICKS) / TICK_HZ }

fn fraction(ta: u64, tb: u64, r: f64) -> f64 { ((r - ta as f64) / (tb - ta).max(1) as f64).clamp(0.0, 1.0) }

fn sample_vessel(ring: &Ring<VesselView>, r: f64) -> Option<VesselView> {
    match ring.around(r) {
        (Some((ta, a)), Some((tb, b))) => {
            let u = fraction(*ta, *tb, r);
            let (ka, kb) = (&a.kin, &b.kin);
            let pos = interp_pos(ka.pose.pos, ka.lin_vel, kb.pose.pos, kb.lin_vel, tb - ta, u);
            let rot = ka.pose.rot.slerp(kb.pose.rot, u).normalize();
            let mut v = a.clone();
            v.kin = Kinematics { pose: Pose::new(pos, rot), lin_vel: ka.lin_vel.lerp(kb.lin_vel, u), ang_vel: ka.ang_vel.lerp(kb.ang_vel, u) };
            v.turrets = lerp_turrets(&a.turrets, &b.turrets, u);
            v.rudder = lerp_f32(a.rudder, b.rudder, u);
            v.power = lerp_f32(a.power, b.power, u);
            Some(v)
        }
        (Some((ta, a)), None) => {
            let dt = extrapolation_s(*ta, r);
            let mut v = a.clone();
            v.kin.pose.pos = a.kin.pose.pos + a.kin.lin_vel * dt;
            v.kin.pose.rot = rotate_by(a.kin.pose.rot, a.kin.ang_vel, dt);
            Some(v)
        }
        (None, Some((_, b))) => Some(b.clone()),
        (None, None) => None,
    }
}

fn sample_body(ring: &Ring<BodyView>, r: f64) -> Option<BodyView> {
    match ring.around(r) {
        (Some((ta, a)), Some((tb, b))) => {
            let u = fraction(*ta, *tb, r);
            let mut v = *a;
            v.pos = interp_pos(a.pos, a.vel, b.pos, b.vel, tb - ta, u);
            v.vel = a.vel.lerp(b.vel, u);
            Some(v)
        }
        (Some((ta, a)), None) => {
            let mut v = *a;
            v.pos = a.pos + a.vel * extrapolation_s(*ta, r);
            Some(v)
        }
        (None, Some((_, b))) => Some(*b),
        (None, None) => None,
    }
}

fn sample_decoy(ring: &Ring<DecoyView>, r: f64) -> Option<DecoyView> {
    match ring.around(r) {
        (Some((ta, a)), Some((tb, b))) => {
            let u = fraction(*ta, *tb, r);
            let mut v = *a;
            v.pos = a.pos.lerp(b.pos, u);
            v.age_s = lerp_f32(a.age_s, b.age_s, u);
            Some(v)
        }
        (Some((ta, a)), None) => {
            let mut v = *a;
            v.age_s = a.age_s + ((r - *ta as f64).max(0.0) / TICK_HZ) as f32;
            Some(v)
        }
        (None, Some((_, b))) => Some(*b),
        (None, None) => None,
    }
}

/// Decaying error offset for a presentation-predicted body (the player's own hull and turrets under the hand):
/// when the authoritative pose arrives, the difference becomes an offset that decays with the half-life;
/// errors above `snap_error_m` snap (L910).
#[derive(Clone, Debug, Default)]
pub struct CorrectionFilter { pub(crate) offsets: std::collections::BTreeMap<EntityId, (DVec3, glam::DQuat)> }

/// Offsets below these are dropped as settled.
const SETTLED_M: f64 = 1e-4;
const SETTLED_RAD: f64 = 1e-6;

/// Rotation angle of a unit quaternion with w >= 0, radians (2 asin |xyz|, by its small-angle bound).
fn rot_size(q: DQuat) -> f64 { 2.0 * DVec3::new(q.x, q.y, q.z).length().min(1.0) }

impl CorrectionFilter {
    pub fn new() -> Self { Self::default() }
    /// Register a correction: `shown` is what the client drew, `authoritative` what the server says. Returns
    /// true when it snapped. Unit: `net-interp`.
    ///
    /// The new offset is the whole difference between what was drawn (which already included the previous
    /// offset) and the authoritative pose, so corrections never stack. A non-finite pose snaps.
    pub fn correct(&mut self, entity: EntityId, shown: &Pose, authoritative: &Pose, tuning: &InterpTuning) -> bool {
        let dp = shown.pos - authoritative.pos;
        let snap_m = finite_or(tuning.snap_error_m, 10.0).max(0.0);
        let mut dq = (shown.rot.normalize() * authoritative.rot.normalize().inverse()).normalize();
        if !(dp.is_finite() && dq.is_finite()) || dp.length() > snap_m {
            self.offsets.remove(&entity);
            return true;
        }
        if dq.w < 0.0 { dq = -dq; }
        if dp.length() < SETTLED_M && rot_size(dq) < SETTLED_RAD { self.offsets.remove(&entity); } else { self.offsets.insert(entity, (dp, dq)); }
        false
    }
    /// Decay all offsets by `dt_s`. Unit: `net-interp`.
    ///
    /// The remaining fraction after `dt_s` is `2^(-dt_s / half_life)`; a zero or invalid half-life settles
    /// every offset at once.
    pub fn decay(&mut self, dt_s: f64, tuning: &InterpTuning) {
        if !(dt_s.is_finite() && dt_s > 0.0) { return; }
        let h = tuning.correction_half_life_s;
        let keep = if h.is_finite() && h > 0.0 { libm::exp2(-dt_s / h) } else { 0.0 };
        self.offsets.retain(|_, (dp, dq)| {
            *dp *= keep;
            *dq = DQuat::IDENTITY.slerp(*dq, keep).normalize();
            dp.length() >= SETTLED_M || rot_size(*dq) >= SETTLED_RAD
        });
    }
    /// Pose to draw: authoritative plus the remaining offset. Unit: `net-interp`.
    pub fn apply(&self, entity: EntityId, pose: &Pose) -> Pose {
        match self.offsets.get(&entity) {
            Some((dp, dq)) => Pose::new(pose.pos + *dp, (*dq * pose.rot).normalize()),
            None => *pose,
        }
    }
    /// Largest current offset in metres (own-hull 0.5 m gate, L933).
    pub fn max_offset_m(&self) -> f64 { self.offsets.values().map(|(d, _)| d.length()).fold(0.0, f64::max) }
}

/// Client-side ballistic shells: spawned from `PhysEvent::ProjectileSpawned`, integrated with
/// `sim_core::projectiles::ballistic_step` at the sim's fixed tick using the band's drag constant from the
/// catalogue, removed on the reliable impact or splash event (L926).
///
/// `Shell::pos` and `Shell::vel` are the state at the start of sim tick `Shell::tick`: the spawn sets them to
/// the origin and muzzle velocity at the spawn tick, and each whole tick is one `ballistic_step`, exactly as
/// the server's projectile store steps a shell in the tick it was fired and every tick after. A snapshot of
/// tick N shows the world after N ticks, so a shell at tick N lines up with the hulls of that snapshot. An
/// impact or splash event carries the tick after the step that hit, so the hit lies on the segment from tick
/// `event - 1` to `event`.
#[derive(Clone, Debug, Default)]
pub struct ShellTracker {
    pub(crate) shells: std::collections::BTreeMap<ProjectileId, Shell>,
    /// Shells whose impact or splash arrived: (event tick, point).
    pub(crate) ends: std::collections::BTreeMap<ProjectileId, (u64, DVec3)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shell { pub spawn: ProjectileSpawn, pub drag_k: f64, pub tick: u64, pub pos: DVec3, pub vel: DVec3 }

/// A shell this far under the sea surface has certainly ended on the server (the deepest trough of the
/// highest sea state is far above it); the client drops it if it never heard the splash.
const SHELL_FLOOR_M: f64 = -40.0;

impl Shell {
    fn step(&self) -> (DVec3, DVec3) { sim_core::projectiles::ballistic_step(self.pos, self.vel, self.drag_k, sim_core::time::DT) }
    fn expired(&self) -> bool {
        !(self.pos.is_finite() && self.vel.is_finite()) || self.pos.y < SHELL_FLOOR_M || self.tick.saturating_sub(self.spawn.tick) > sim_core::projectiles::MAX_FLIGHT_TICKS
    }
    /// Integrate to `tick` in whole ticks; false when the shell ended on the way.
    fn advance(&mut self, tick: u64) -> bool {
        // Never more than the server's flight limit: a shell past it is gone there too.
        let tick = tick.min(self.spawn.tick.saturating_add(sim_core::projectiles::MAX_FLIGHT_TICKS + 1));
        while self.tick < tick {
            let (p, v) = self.step();
            self.pos = p;
            self.vel = v;
            self.tick += 1;
            if self.expired() { return false; }
        }
        !self.expired()
    }
}

/// Fraction along `a -> b` of the point closest to `p`, in [0, 1].
fn closest_fraction(a: DVec3, b: DVec3, p: DVec3) -> f64 {
    let d = b - a;
    let l2 = d.length_squared();
    if !(l2 > 0.0 && l2.is_finite()) { return 0.0; }
    ((p - a).dot(d) / l2).clamp(0.0, 1.0)
}

#[allow(clippy::len_without_is_empty)]
impl ShellTracker {
    pub fn new() -> Self { Self::default() }
    /// Unit: `net-interp`.
    ///
    /// A repeated id or a non-finite origin or velocity is ignored.
    pub fn spawn(&mut self, spawn: &ProjectileSpawn, drag_k: f64) {
        if !(spawn.origin.is_finite() && spawn.velocity.is_finite()) || self.shells.contains_key(&spawn.id) { return; }
        let k = if drag_k.is_finite() && drag_k > 0.0 { drag_k } else { 0.0 };
        self.shells.insert(spawn.id, Shell { spawn: *spawn, drag_k: k, tick: spawn.tick, pos: spawn.origin, vel: spawn.velocity });
    }
    /// Remove on impact or splash; returns the client's position at removal (cross-platform check, L939).
    /// Unit: `net-interp`.
    pub fn remove(&mut self, id: ProjectileId) -> Option<DVec3> {
        self.ends.remove(&id);
        self.shells.remove(&id).map(|s| s.pos)
    }
    /// End a shell at its impact or splash: it stays drawn until the render tick reaches `point`, then goes
    /// at the first `advance_to` at or past `event_tick`, the tick the impact or splash event carries.
    pub fn close(&mut self, id: ProjectileId, event_tick: u64, point: DVec3) {
        if self.shells.contains_key(&id) { self.ends.insert(id, (event_tick, point)); }
    }
    /// Integrate every shell to `tick` in whole fixed ticks (never past the newest server tick). Unit: `net-interp`.
    ///
    /// Shells that have reached their closing tick, sunk below any sea or outlived the server's flight limit
    /// are removed. A closed shell is integrated only to the start of its closing segment.
    pub fn advance_to(&mut self, tick: u64) {
        let ends = &self.ends;
        self.shells.retain(|id, s| match ends.get(id) {
            Some((e, _)) if *e <= tick => false,
            Some((e, _)) => s.advance(tick.min(e.saturating_sub(1))),
            None => s.advance(tick),
        });
        let shells = &self.shells;
        self.ends.retain(|id, _| shells.contains_key(id));
    }
    /// Position at a fractional render tick for drawing (linear within the current tick). Unit: `net-interp`.
    ///
    /// Each entry is (id, position, velocity). Shells not yet fired at the render tick are left out; a closed
    /// shell is left out once the render tick reaches its impact point. `out` is cleared first.
    pub fn render_positions(&self, render_tick: f64, out: &mut Vec<(ProjectileId, DVec3, DVec3)>) {
        out.clear();
        if !render_tick.is_finite() { return; }
        let r = render_tick.max(0.0);
        let base = r.floor() as u64;
        for (id, s) in &self.shells {
            if (s.spawn.tick as f64) > r { continue; }
            let mut sh = *s;
            if sh.tick < base && !sh.advance(base) { continue; }
            // `u` is the fraction of the tick from `sh.tick`; a shell integrated past the render tick (the
            // caller advanced it further) is drawn where it is.
            let u = if sh.tick == base { r - base as f64 } else { 0.0 };
            let (next, vnext) = sh.step();
            if let Some(&(e, point)) = self.ends.get(id) {
                if e <= base { continue; }
                if sh.tick + 1 == e && point.is_finite() && u >= closest_fraction(sh.pos, next, point) { continue; }
            }
            let pos = sh.pos.lerp(next, u);
            if pos.is_finite() { out.push((*id, pos, sh.vel.lerp(vnext, u))); }
        }
    }
    pub fn len(&self) -> usize { self.shells.len() }
    pub fn clear(&mut self) { self.shells.clear(); self.ends.clear(); }

    /// The client's point closest to the server's impact `point` on the shell's segment into `event_tick`
    /// (the impact event's tick), integrating the shell there first; `None` for an unknown shell. The
    /// cross-platform check (L939) compares this with the server's point.
    pub(crate) fn impact_point(&mut self, id: ProjectileId, event_tick: u64, point: DVec3) -> Option<DVec3> {
        let s = self.shells.get_mut(&id)?;
        if !s.advance(event_tick.saturating_sub(1)) { return Some(s.pos); }
        let (next, _) = s.step();
        let f = if point.is_finite() { closest_fraction(s.pos, next, point) } else { 0.0 };
        Some(s.pos.lerp(next, f))
    }
}

/// Drag constant of a calibre band from the catalogue, as the server's projectile store reads it: the band
/// row's `drag_k` when positive, 0 for a row without drag, and the fitted spec default for a missing row.
pub fn band_drag_k(catalogue: &content::catalogue::Catalogue, band: content::part::weapons::CalibreBand) -> f64 {
    match catalogue.bands.iter().find(|b| b.band == band) {
        Some(b) if b.drag_k.is_finite() && b.drag_k > 0.0 => b.drag_k,
        Some(_) => 0.0,
        None => sim_core::projectiles::fit_drag_k(&content::part::weapons::BandDef::spec_default(band)),
    }
}

/// Apply one received event to a buffer and a shell tracker the way every client path does (live sessions,
/// the wire capture decode): lifecycle into the buffer, spawns into the tracker, impacts and splashes close
/// their shells.
pub(crate) fn apply_event(buffer: &mut SnapshotBuffer, shells: &mut ShellTracker, catalogue: &content::catalogue::Catalogue, event: &WireEvent) {
    buffer.on_event(event);
    match &event.event {
        SimEvent::Phys(PhysEvent::ProjectileSpawned(s)) => shells.spawn(s, band_drag_k(catalogue, s.band)),
        SimEvent::Phys(PhysEvent::ShellImpact { projectile, world_point, .. }) => shells.close(*projectile, event.tick, *world_point),
        SimEvent::Phys(PhysEvent::Splash { projectile, pos, .. }) => shells.close(*projectile, event.tick, *pos),
        _ => {}
    }
}

/// Sim ticks per real second the pacing asks for: 60 x compression x time scale, 0 while paused.
pub(crate) fn nominal_rate(p: &Pacing) -> f64 {
    if p.paused { return 0.0; }
    let scale = if p.time_scale.is_finite() && p.time_scale > 0.0 { (p.time_scale as f64).min(1.0) } else { 1.0 };
    TICK_HZ * p.effective.factor() as f64 * scale
}
