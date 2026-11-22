//! Unit `sys-lod`. Simulation level of detail by relevance and the host overload ladder.
//! DESIGN.md L1538-1549 (LOD by relevance), L1500-1527 and L1019-1029 (degradation ladder), L116
//! (compression steps down before any unit moves to coarse simulation).
//!
//! Fidelity rule: a hull runs full physics if any of: a player-side sensor tracks it; a player-side weapon
//! targets it; it targets a player-side unit; a projectile will reach its bounding box within the next tick
//! (`Projectiles::threatens`). Untracked hulls within 10 km of a player hull stay full until rung 2. All
//! other hulls beyond 10 km of every player hull are kinematic with analytic buoyancy. Full-physics hulls
//! beyond 10 km tick at 30 Hz.
//!
//! Ladder (rung 1 is per client and visual only): rung 2 untracked hulls kinematic beyond 5 km, then at any
//! range; rung 3 projectile cap 2,000 to 1,500 to 1,000; rung 4 time scale 0.9 down to 0.5 in 0.1 steps.
//! Step up one rung or sub-step when the tick p95 over a 2 s window exceeds 8.0 ms, then wait at least 4 s;
//! step down when p95 stays under 6.0 ms for 10 s; time-scale steps only after rungs 2 and 3 are spent and
//! only while the tick backlog exceeds 3 ticks. Compression steps 4x to 2x to 1x before rung 2.
//! The ladder never reads a clock: the driver measures tick times and backlog and passes them in as
//! [`HostLoad`]. Headless tests pass synthetic loads; replays and golden runs pass `HostLoad::IDLE`, so the
//! ladder is inert and results stay deterministic.
use crate::prelude::*;
use crate::ship::ShipTable;
use crate::tracks::Pictures;
use sim_core::vessel::Fidelity;

/// Host timing measured by the driver, the one wall-clock input to the simulation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HostLoad { /// p95 of sim tick wall time over the last 2 s, milliseconds.
    pub tick_p95_ms: f32, /// Ticks the simulation is behind real time.
    pub backlog_ticks: u32 }
impl HostLoad { pub const IDLE: HostLoad = HostLoad { tick_p95_ms: 0.0, backlog_ticks: 0 }; }

/// Ladder thresholds (L1502-1517; the oscillation fallback is 5.0 ms and 30 s, L1029).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LadderTuning { pub up_ms: f32, pub down_ms: f32, pub up_dwell_s: f32, pub down_dwell_s: f32, pub backlog_ticks: u32, pub kinematic_near_m: f64 }
impl Default for LadderTuning { fn default() -> Self { Self { up_ms: 8.0, down_ms: 6.0, up_dwell_s: 4.0, down_dwell_s: 10.0, backlog_ticks: 3, kinematic_near_m: 5_000.0 } } }

/// Ladder position. Rung 0 = nothing degraded. Sub-steps: rung 2 has 2 (beyond 5 km, any range), rung 3 has 2
/// (1,500, 1,000), rung 4 has 5 (0.9 to 0.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LadderState { pub rung: u8, pub sub_step: u8, pub last_step_tick: u64, pub below_since_tick: Option<u64>, pub highest_rung: u8, /// Compression ceiling imposed by load before rung 2: 4, 2 or 1.
    pub max_rate: u8,
    /// True once the ladder has stepped up at least once (`last_step_tick` is meaningful). Added by `sys-lod`.
    #[serde(default)]
    pub(crate) stepped_up: bool,
    /// Tick of the last step down, for the follow-up interval. Added by `sys-lod`.
    #[serde(default)]
    pub(crate) last_down_tick: Option<u64> }
impl LadderState {
    /// Time scale in force: 1.0 unless on rung 4. Real code.
    pub fn time_scale(&self) -> f32 { if self.rung >= 4 { 0.9 - 0.1 * self.sub_step.min(4) as f32 } else { 1.0 } }
    /// Projectile cap rung 0..=2 for `World::set_projectile_cap_rung`. Real code.
    pub fn projectile_cap_rung(&self) -> u8 { if self.rung > 3 { 2 } else if self.rung == 3 { 1 + self.sub_step.min(1) } else { 0 } }
}

/// Advance the ladder one tick from the measured load. Returns `Some(up)` when it stepped.
/// Unit: `sys-lod`.
///
/// Positions, mildest first: compression ceiling 4x, 2x, 1x; rung 2 sub-steps 0 (untracked hulls kinematic
/// beyond 5 km) and 1 (at any range); rung 3 sub-steps 0 (1,500 projectiles) and 1 (1,000); rung 4 sub-steps
/// 0 to 4 (time scale 0.9 to 0.5). A `max_rate` of 0 (the default state) reads as 4x.
///
/// - Up: when `tick_p95_ms` exceeds `up_ms`. Compression steps come first and take effect at once, one per
///   tick (a lower rate never hides anything from the player and relieves the backlog fastest). A rung
///   sub-step then needs `up_dwell_s` since the last rung step up. Rung 4 is entered or deepened only after
///   rungs 2 and 3 are spent and only while `backlog_ticks` exceeds `tuning.backlog_ticks`; otherwise the
///   ladder holds.
/// - Down: when `tick_p95_ms` stays under `down_ms` for `down_dwell_s`, one position down; every further
///   position needs 1.5 s more of continued quiet (most of one 2 s p95 window, so each step's effect shows
///   before the next). From the deepest position that is 10 + 10 x 1.5 = 25 s back to rung 0, inside the
///   30 s gate even with the p95 window's own 2 s lag.
///   Between the thresholds nothing changes and the quiet clock restarts.
/// - Dwell times are counted in simulation ticks at 60 Hz. The ladder never reads a clock.
/// - At rung 0 with the full ceiling, a quiet load changes nothing, so `HostLoad::IDLE` leaves the state
///   untouched (replays and golden runs).
pub fn step_ladder(state: &mut LadderState, load: HostLoad, tick: u64, tuning: &LadderTuning) -> Option<bool> {
    let p95 = if load.tick_p95_ms.is_finite() { load.tick_p95_ms } else { 0.0 };
    let secs = |s: f32| -> u64 { if s.is_finite() && s > 0.0 { (s as f64 * sim_core::TICK_HZ as f64).round() as u64 } else { 0 } };
    let pos = position(state);
    if p95 > tuning.up_ms {
        state.below_since_tick = None;
        state.last_down_tick = None;
        // Compression ceiling first, no dwell.
        if pos < 2 { set_position(state, pos + 1); return Some(true); }
        if state.stepped_up && tick.saturating_sub(state.last_step_tick) < secs(tuning.up_dwell_s) { return None; }
        if pos >= LAST_POSITION { return None; }
        // Entering or deepening rung 4 needs a backlog (rungs 2 and 3 are spent once pos reaches 6).
        if pos + 1 >= FIRST_TIME_SCALE && load.backlog_ticks <= tuning.backlog_ticks { return None; }
        set_position(state, pos + 1);
        state.last_step_tick = tick;
        state.stepped_up = true;
        state.highest_rung = state.highest_rung.max(state.rung);
        return Some(true);
    }
    if pos == 0 { state.below_since_tick = None; state.last_down_tick = None; return None; }
    if p95 >= tuning.down_ms { state.below_since_tick = None; state.last_down_tick = None; return None; }
    let since = *state.below_since_tick.get_or_insert(tick);
    let due = match state.last_down_tick {
        Some(last) => tick.saturating_sub(last) >= secs(FOLLOW_DOWN_S),
        None => tick.saturating_sub(since) >= secs(tuning.down_dwell_s),
    };
    if !due { return None; }
    set_position(state, pos - 1);
    state.last_down_tick = Some(tick);
    if position(state) == 0 { state.below_since_tick = None; state.last_down_tick = None; }
    Some(false)
}

/// Quiet time between successive steps down after the first, seconds.
const FOLLOW_DOWN_S: f32 = 1.5;
/// Index of the first time-scale position (rung 4, sub-step 0) and of the deepest one.
const FIRST_TIME_SCALE: u8 = 7;
const LAST_POSITION: u8 = 11;

/// Linear ladder position 0..=11 (see [`step_ladder`]).
fn position(s: &LadderState) -> u8 {
    match s.rung {
        0 | 1 => match s.max_rate { 0 | 4.. => 0, 2 | 3 => 1, _ => 2 },
        2 => 3 + s.sub_step.min(1),
        3 => 5 + s.sub_step.min(1),
        _ => FIRST_TIME_SCALE + s.sub_step.min(4),
    }
}

fn set_position(s: &mut LadderState, pos: u8) {
    let (rung, sub, rate) = match pos.min(LAST_POSITION) {
        0 => (0, 0, 4),
        1 => (0, 0, 2),
        2 => (0, 0, 1),
        p @ 3..=4 => (2, p - 3, 1),
        p @ 5..=6 => (3, p - 5, 1),
        p => (4, p - FIRST_TIME_SCALE, 1),
    };
    s.rung = rung;
    s.sub_step = sub;
    s.max_rate = rate;
}

/// Decide every hull's fidelity for this tick by the rule above and the ladder rung, ascending vessel.
/// Unit: `sys-lod`.
///
/// Every vessel in the world gets an entry, wrecks included. Player-side hulls (a ship on a player side)
/// always run full physics. "Tracks it" means a live track in any player side's picture; "targets" means a
/// ship's standing designation, the targets its mounts are engaging and the targets its fire-permission and
/// missile sinks named this graph tick, resolved through that ship's own side picture. Distances are
/// horizontal, to the nearest player-side hull. With no player side at all (headless AI-only runs) every hull
/// stays full: there is no viewer to save detail for.
pub fn assign_fidelity(world: &World, ships: &ShipTable, pictures: &Pictures, player_sides: SideMask, ladder: &LadderState, tuning: &LadderTuning) -> Vec<(EntityId, Fidelity)> {
    let mut hulls: Vec<HullFacts> = world.vessels().map(|(_, v)| HullFacts {
        id: v.id,
        pos: v.kin.pose.pos,
        player_side: player_sides.has(v.side) && ships.get(v.id).is_some(),
        relevant: world.projectiles().threatens(&v.bounds),
    }).collect();
    hulls.sort_by_key(|h| h.id);
    let find = |hulls: &mut Vec<HullFacts>, e: EntityId| -> Option<usize> { hulls.binary_search_by_key(&e, |h| h.id).ok() };
    // Tracked by any player-side picture.
    for side in player_sides.iter() {
        let Some(p) = pictures.side(side) else { continue };
        for t in p.tracks.iter().filter(|t| t.is_live()) {
            if let Some(i) = find(&mut hulls, t.truth) { hulls[i].relevant = true; }
        }
    }
    // Targeting either way.
    for s in &ships.ships {
        let Some(picture) = pictures.side(s.side) else { continue };
        let own_player_side = player_sides.has(s.side);
        let Some(me) = find(&mut hulls, s.vessel) else { continue };
        for track in ship_targets(s) {
            let Some(t) = picture.get(track) else { continue };
            let Some(i) = find(&mut hulls, t.truth) else { continue };
            if own_player_side { hulls[i].relevant = true; }
            else if hulls[i].player_side { hulls[me].relevant = true; }
        }
    }
    fidelity_core(&hulls, !player_sides.is_empty(), ladder, tuning)
}

/// Full-physics radius around player hulls (L1547), metres.
pub const FULL_RADIUS_M: f64 = 10_000.0;

#[derive(Clone, Copy, Debug)]
struct HullFacts { id: EntityId, pos: DVec3, player_side: bool, relevant: bool }

/// Tracks a ship is aiming at: designation, engaged mounts, and this tick's fire and missile sinks.
fn ship_targets(s: &crate::ship::Ship) -> impl Iterator<Item = TrackId> + '_ {
    use content::graph::SinkKind;
    use crate::graph::PortValue;
    let sinks = s.sinks.writes.iter().filter_map(|w| match (w.sink, w.values.get(if matches!(w.sink, SinkKind::MountFirePermission(_)) { 1 } else { 0 })) {
        (SinkKind::MountFirePermission(_) | SinkKind::MissileTarget(_), Some(PortValue::Track(t))) => Some(*t),
        _ => None,
    });
    core::iter::once(s.orders.designated)
        .chain(s.stages.engage.mounts.iter().map(|m| m.target))
        .chain(sinks)
        .filter(|t| !t.is_none())
}

fn fidelity_core(hulls: &[HullFacts], any_player_side: bool, ladder: &LadderState, tuning: &LadderTuning) -> Vec<(EntityId, Fidelity)> {
    let players: Vec<DVec3> = hulls.iter().filter(|h| h.player_side && h.pos.is_finite()).map(|h| h.pos).collect();
    // Rung 2 sub-step 0: untracked hulls beyond the near radius go kinematic; sub-step 1 and every deeper rung:
    // at any range.
    let near_limit = match ladder.rung { 0 | 1 => None, 2 if ladder.sub_step == 0 => Some(if tuning.kinematic_near_m.is_finite() { tuning.kinematic_near_m.max(0.0) } else { 5_000.0 }), _ => Some(0.0) };
    hulls.iter().map(|h| {
        if h.player_side || !any_player_side { return (h.id, Fidelity::Full); }
        let d = players.iter().map(|p| { let (dx, dz) = (p.x - h.pos.x, p.z - h.pos.z); libm::sqrt(dx * dx + dz * dz) }).fold(f64::INFINITY, f64::min);
        let d = if d.is_finite() { d } else { f64::INFINITY };
        let f = if h.relevant {
            if d <= FULL_RADIUS_M { Fidelity::Full } else { Fidelity::Full30 }
        } else if d <= FULL_RADIUS_M {
            match near_limit { Some(lim) if d > lim => Fidelity::Kinematic, _ => Fidelity::Full }
        } else {
            Fidelity::Kinematic
        };
        (h.id, f)
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: LadderTuning = LadderTuning { up_ms: 8.0, down_ms: 6.0, up_dwell_s: 4.0, down_dwell_s: 10.0, backlog_ticks: 3, kinematic_near_m: 5_000.0 };
    fn hot(backlog: u32) -> HostLoad { HostLoad { tick_p95_ms: 9.0, backlog_ticks: backlog } }
    fn cool() -> HostLoad { HostLoad { tick_p95_ms: 4.0, backlog_ticks: 0 } }

    #[test]
    fn idle_leaves_rung_zero_untouched() {
        let mut s = LadderState::default();
        for t in 0..2_000 { assert_eq!(step_ladder(&mut s, HostLoad::IDLE, t, &T), None); }
        assert_eq!(s, LadderState::default());
    }

    #[test]
    fn compression_goes_first_then_rungs_with_dwell() {
        let mut s = LadderState::default();
        assert_eq!(step_ladder(&mut s, hot(0), 100, &T), Some(true));
        assert_eq!(s.max_rate, 2);
        assert_eq!(step_ladder(&mut s, hot(0), 101, &T), Some(true));
        assert_eq!((s.max_rate, s.rung), (1, 0));
        // Rung 2 at once (no rung step yet), then a 4 s wait.
        assert_eq!(step_ladder(&mut s, hot(0), 102, &T), Some(true));
        assert_eq!((s.rung, s.sub_step), (2, 0));
        assert_eq!(step_ladder(&mut s, hot(0), 102 + 239, &T), None);
        assert_eq!(step_ladder(&mut s, hot(0), 102 + 240, &T), Some(true));
        assert_eq!((s.rung, s.sub_step), (2, 1));
        let mut t = 102 + 240;
        for want in [(3, 0), (3, 1)] { t += 240; assert_eq!(step_ladder(&mut s, hot(0), t, &T), Some(true)); assert_eq!((s.rung, s.sub_step), want); }
        assert_eq!(s.projectile_cap_rung(), 2);
        // Rung 4 only with a backlog.
        t += 240;
        assert_eq!(step_ladder(&mut s, hot(3), t, &T), None);
        assert_eq!(step_ladder(&mut s, hot(4), t, &T), Some(true));
        assert_eq!((s.rung, s.sub_step), (4, 0));
        assert!((s.time_scale() - 0.9).abs() < 1e-6);
        for _ in 0..4 { t += 240; assert_eq!(step_ladder(&mut s, hot(10), t, &T), Some(true)); }
        assert!((s.time_scale() - 0.5).abs() < 1e-6);
        t += 240;
        assert_eq!(step_ladder(&mut s, hot(10), t, &T), None, "0.5x is the floor");
        assert_eq!(s.highest_rung, 4);
    }

    #[test]
    fn returns_to_rung_zero_within_30_s_of_quiet() {
        let mut s = LadderState::default();
        set_position(&mut s, LAST_POSITION);
        let mut t = 10_000;
        let start = t;
        let mut downs = 0;
        // Between the thresholds nothing happens.
        assert_eq!(step_ladder(&mut s, HostLoad { tick_p95_ms: 7.0, backlog_ticks: 0 }, t, &T), None);
        while position(&s) > 0 {
            t += 1;
            if step_ladder(&mut s, cool(), t, &T) == Some(false) { downs += 1; }
            assert!(t - start < 30 * 60, "back to rung 0 inside 30 s");
        }
        assert_eq!(downs, LAST_POSITION);
        assert_eq!(s.max_rate, 4);
        assert!(t - start >= 10 * 60, "first step waits 10 s");
        assert_eq!(step_ladder(&mut s, cool(), t + 1, &T), None);
    }

    #[test]
    fn a_hot_tick_restarts_the_quiet_clock() {
        let mut s = LadderState::default();
        set_position(&mut s, 3);
        for t in 0..599 { assert_eq!(step_ladder(&mut s, cool(), t, &T), None); }
        // Not enough dwell for a step up yet the quiet clock resets.
        s.stepped_up = true; s.last_step_tick = 590;
        assert_eq!(step_ladder(&mut s, hot(0), 599, &T), None);
        assert_eq!(step_ladder(&mut s, cool(), 600, &T), None);
        assert_eq!(step_ladder(&mut s, cool(), 1_199, &T), None);
        assert_eq!(step_ladder(&mut s, cool(), 1_200, &T), Some(false));
    }

    fn hull(id: u32, x: f64, player: bool, relevant: bool) -> HullFacts { HullFacts { id: EntityId { index: id, generation: 0 }, pos: DVec3::new(x, 0.0, 0.0), player_side: player, relevant } }

    #[test]
    fn fidelity_follows_relevance_and_the_ladder() {
        let hs = [hull(0, 0.0, true, false), hull(1, 3_000.0, false, false), hull(2, 8_000.0, false, false), hull(3, 40_000.0, false, true), hull(4, 40_000.0, false, false), hull(5, 9_000.0, false, true)];
        let f = |l: &LadderState| fidelity_core(&hs, true, l, &T).into_iter().map(|x| x.1).collect::<Vec<_>>();
        use Fidelity::*;
        assert_eq!(f(&LadderState::default()), vec![Full, Full, Full, Full30, Kinematic, Full]);
        let mut l = LadderState::default();
        set_position(&mut l, 3);
        assert_eq!(f(&l), vec![Full, Full, Kinematic, Full30, Kinematic, Full]);
        set_position(&mut l, 4);
        assert_eq!(f(&l), vec![Full, Kinematic, Kinematic, Full30, Kinematic, Full]);
        set_position(&mut l, 9);
        assert_eq!((l.rung, l.sub_step), (4, 2));
        assert_eq!(f(&l), vec![Full, Kinematic, Kinematic, Full30, Kinematic, Full]);
        // No player side: nothing degrades.
        assert!(fidelity_core(&hs, false, &l, &T).iter().all(|x| x.1 == Full));
    }
}
