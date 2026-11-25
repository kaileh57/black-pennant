//! Unit `sys-timeflow`. Pause and time compression. DESIGN.md L110-116.
//!
//! | Mode | Pause | Compression |
//! | Single-player | any time | the player's rate is the effective rate, subject to the safety test |
//! | Co-op | never | effective = minimum of all players' requested rates |
//! | PvP | no | no |
//! Rates are 1x, 2x and 4x. Compression is allowed only while all hold, using only player-side knowledge
//! (undetected contacts never block): no inbound weapon and no player-side track with stance Suspect or
//! Hostile inside the longest weapon envelope of any player hull; and no player hull inside the weapon
//! envelope the plot table draws for any such track. Friend and Neutral tracks never block. When the test
//! fails the rate drops to 1x at once. Compression is capped by the host's simulation budget (4x to 2x to 1x
//! before any unit moves to coarse simulation) and is unavailable while the overload time scale is below 1.
//! The driver runs `Pacing::effective.factor()` simulation ticks per real tick, scaled by `time_scale`.
use crate::lod::LadderState;
use crate::prelude::*;
use crate::protocol::command::TimeRate;
use crate::protocol::snapshot::{CompressionBlock, Pacing};
use crate::ship::ShipTable;
use crate::tracks::Pictures;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TimeState { pub paused: bool, pub requested: Vec<(PlayerSlot, TimeRate)>, pub effective: TimeRate, pub blocked: Option<CompressionBlock> }

/// Record a pause request; refused outside single-player. Unit: `sys-timeflow`.
///
/// Single-player means at most one connected human: campaign, skirmish and sandbox played alone, the
/// tutorial, splash and trials. PvP never pauses and co-op never pauses. Returns true when the request was
/// recorded (including a request for the state already in force).
pub fn set_pause(state: &mut TimeState, on: bool, mode: GameMode, human_players: usize) -> bool {
    if !pause_allowed(mode, human_players) { return false; }
    state.paused = on;
    true
}

fn is_pvp(mode: GameMode) -> bool { matches!(mode, GameMode::PvpElimination | GameMode::PvpEscort) }
fn pause_allowed(mode: GameMode, human_players: usize) -> bool { !is_pvp(mode) && human_players <= 1 }

/// Record a player's requested rate. Unit: `sys-timeflow`.
///
/// One entry per player, ascending slot; a new request replaces that player's last one. The effective rate is
/// only recomputed by [`update`].
pub fn request_rate(state: &mut TimeState, player: PlayerSlot, rate: TimeRate) {
    match state.requested.binary_search_by(|(p, _)| p.cmp(&player)) {
        Ok(i) => state.requested[i].1 = rate,
        Err(i) => state.requested.insert(i, (player, rate)),
    }
}

/// The compression safety test (L113). `Ok` when compression may run. Unit: `sys-timeflow`.
///
/// Player hulls are the ships a player commands on a player side. The tracks are those of the player sides'
/// pictures (live tracks only), so contacts the player side has not detected never block. A track's envelope
/// is the one the plot table draws (`sim::snapshot`): for an identified class, the longest effective range of
/// any non-decoy mount on a hull of that class in this sortie; unidentified tracks draw none. (The plot also
/// falls back to the campaign's class engagement range, which this function cannot see; every class fielded
/// in a sortie has a design in the ship table, so the fallback only matters for classes not present.) Ranges
/// are horizontal. When several blocks hold, the first in the order weapon inbound, threat in own envelope,
/// inside threat envelope is reported.
pub fn safety_test(world: &World, ships: &ShipTable, pictures: &Pictures, player_sides: SideMask) -> Result<(), CompressionBlock> {
    let tick = world.tick().0;
    let mut hulls: Vec<(DVec3, f64)> = Vec::new();
    for s in ships.ships.iter().filter(|s| s.player.is_some() && player_sides.has(s.side)) {
        let Some(v) = world.vessel(s.vessel) else { continue };
        if !v.kin.pose.pos.is_finite() { continue; }
        hulls.push((v.kin.pose.pos, crate::engage::longest_envelope_m(&s.design.mounts, &s.doctrine, &v.parts)));
    }
    let mut class_env: BTreeMap<ClassId, f64> = BTreeMap::new();
    let mut envelope = |c: &ClassId| -> f64 {
        *class_env.entry(c.clone()).or_insert_with(|| {
            ships.ships.iter()
                .filter(|s| world.vessel(s.vessel).and_then(|v| v.class.as_ref()) == Some(c))
                .flat_map(|s| s.design.mounts.iter().filter(|m| m.weapon != content::graph::WeaponClass::Decoy).map(|m| m.effective_range_m))
                .filter(|r| r.is_finite())
                .fold(0.0f64, f64::max)
        })
    };
    let tracks = player_sides.iter().filter_map(|side| pictures.side(side)).flat_map(|p| p.tracks.iter());
    safety_core(&hulls, tracks, &mut envelope, tick)
}

/// Effective stance of a track: a datalink Friend stays Friend, otherwise a manual or graph override wins.
fn effective_stance(t: &crate::tracks::Track) -> Stance { if t.stance == Stance::Friend { Stance::Friend } else { t.stance_override.unwrap_or(t.stance) } }

fn horizontal(a: DVec3, b: DVec3) -> f64 { let (dx, dz) = (a.x - b.x, a.z - b.z); libm::sqrt(dx * dx + dz * dz) }

/// The test over plain data: player hulls as (position, longest own envelope), the player sides' tracks, and
/// the envelope the plot draws for an identified class.
fn safety_core<'a>(hulls: &[(DVec3, f64)], tracks: impl Iterator<Item = &'a crate::tracks::Track>, envelope: &mut dyn FnMut(&ClassId) -> f64, tick: u64) -> Result<(), CompressionBlock> {
    let mut worst: Option<CompressionBlock> = None;
    let rank = |b: CompressionBlock| match b { CompressionBlock::WeaponInbound => 0, CompressionBlock::ThreatInOwnEnvelope => 1, _ => 2 };
    let mut note = |b: CompressionBlock| { if worst.is_none_or(|w| rank(b) < rank(w)) { worst = Some(b); } };
    for t in tracks.filter(|t| t.is_live()) {
        let stance = effective_stance(t);
        if stance == Stance::Friend { continue; }
        if t.domain == Domain::Weapon && t.inbound { note(CompressionBlock::WeaponInbound); continue; }
        if stance < Stance::Suspect { continue; }
        let p = t.predict(tick);
        if !p.is_finite() { continue; }
        if hulls.iter().any(|(h, env)| *env > 0.0 && horizontal(*h, p) <= *env) { note(CompressionBlock::ThreatInOwnEnvelope); continue; }
        if let Some(c) = &t.class {
            let env = envelope(c);
            if env > 0.0 && hulls.iter().any(|(h, _)| horizontal(*h, p) <= env) { note(CompressionBlock::InsideThreatEnvelope); }
        }
    }
    match worst { Some(b) => Err(b), None => Ok(()) }
}

/// Recompute the effective rate: mode rules, minimum over connected human players, safety test, ladder
/// ceiling. Returns true when pacing changed. Unit: `sys-timeflow`.
///
/// `humans` are the connected human players. With no request above 1x the rate is 1x and nothing is blocked.
/// Otherwise the first rule that holds decides: PvP forbids compression (`ModeForbids`); in co-op any connected
/// player without a matching request holds the rate at the lowest one, and 1x requests block it outright
/// (`NotAllPlayers`); the safety test drops it to 1x; an overload time scale below 1 makes it unavailable
/// (`TimeScaleDegraded`); and the ladder's compression ceiling caps it (`HostLoad`, the capped rate still
/// runs). The requested rates persist, so compression resumes on its own once the conditions hold again,
/// exactly as a co-op player still holding the toggle expects. Co-op and PvP clear a pause.
pub fn update(state: &mut TimeState, mode: GameMode, humans: &[PlayerSlot], safety: Result<(), CompressionBlock>, ladder: &LadderState) -> bool {
    let before = (state.paused, state.effective, state.blocked);
    if !pause_allowed(mode, humans.len()) { state.paused = false; }
    let rate_of = |p: &PlayerSlot| state.requested.binary_search_by(|(q, _)| q.cmp(p)).map(|i| state.requested[i].1).unwrap_or(TimeRate::X1);
    let wanted_max = humans.iter().map(rate_of).max().unwrap_or(TimeRate::X1);
    let wanted = humans.iter().map(rate_of).min().unwrap_or(TimeRate::X1);
    let (effective, blocked) = if wanted_max == TimeRate::X1 {
        (TimeRate::X1, None)
    } else if is_pvp(mode) {
        (TimeRate::X1, Some(CompressionBlock::ModeForbids))
    } else if wanted == TimeRate::X1 {
        (TimeRate::X1, Some(CompressionBlock::NotAllPlayers))
    } else if let Err(b) = safety {
        (TimeRate::X1, Some(b))
    } else if ladder.time_scale() < 1.0 {
        (TimeRate::X1, Some(CompressionBlock::TimeScaleDegraded))
    } else {
        let ceiling = match ladder.max_rate { 0 | 4.. => TimeRate::X4, 2 | 3 => TimeRate::X2, _ => TimeRate::X1 };
        if wanted > ceiling { (ceiling, Some(CompressionBlock::HostLoad)) } else { (wanted, None) }
    };
    state.effective = effective;
    state.blocked = blocked;
    before != (state.paused, state.effective, state.blocked)
}

/// The pacing block for snapshots and `SimEvent::PacingChanged`. Real code.
pub fn pacing(state: &TimeState, ladder: &LadderState, host_tick_ms: f32) -> Pacing {
    Pacing { paused: state.paused, requested: state.requested.clone(), effective: state.effective, blocked: state.blocked, time_scale: ladder.time_scale(), rung: ladder.rung, sub_step: ladder.sub_step, host_tick_ms }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tracks::Track;

    pub(crate) fn track(id: u32, pos: DVec3, stance: Stance, domain: Domain) -> Track {
        Track {
            id: TrackId(id), side: SideId(0), domain, truth: EntityId::NONE, pos, vel: DVec3::ZERO, r0_m: 10.0, sigma_v: 1.0,
            bearing_only: None, created_tick: 0, last_update_tick: 0, last_lock_tick: None, dropped_tick: None, grade: TrackGrade::Fine,
            sources: Default::default(), trend: Default::default(), rung: IdRung::Unknown, rung_reason: Default::default(),
            candidates: Default::default(), class: None, history: Vec::new(), rcs_m2: 100.0, consistent_ticks: 0, emitters: Vec::new(),
            id_override: None, stance, stance_reason: Default::default(), stance_override: None, fired_on_us: false,
            inbound: false, engaged_by: 0, emitting: false, fit: None, out_of_action: false, struck: false,
        }
    }

    #[test]
    fn pause_only_alone_and_never_in_pvp() {
        let mut s = TimeState::default();
        assert!(set_pause(&mut s, true, GameMode::Campaign, 1));
        assert!(s.paused);
        assert!(set_pause(&mut s, false, GameMode::Campaign, 1));
        assert!(!set_pause(&mut s, true, GameMode::Campaign, 2));
        assert!(!set_pause(&mut s, true, GameMode::PvpElimination, 1));
        assert!(!s.paused);
        assert!(set_pause(&mut s, true, GameMode::Tutorial, 0));
    }

    #[test]
    fn requests_are_one_per_player_ascending() {
        let mut s = TimeState::default();
        request_rate(&mut s, PlayerSlot(2), TimeRate::X4);
        request_rate(&mut s, PlayerSlot(0), TimeRate::X2);
        request_rate(&mut s, PlayerSlot(2), TimeRate::X2);
        assert_eq!(s.requested, vec![(PlayerSlot(0), TimeRate::X2), (PlayerSlot(2), TimeRate::X2)]);
    }

    #[test]
    fn single_player_rate_is_the_effective_rate_until_the_test_fails() {
        let mut s = TimeState::default();
        let l = LadderState::default();
        let me = [PlayerSlot(0)];
        request_rate(&mut s, PlayerSlot(0), TimeRate::X4);
        assert!(update(&mut s, GameMode::Campaign, &me, Ok(()), &l));
        assert_eq!((s.effective, s.blocked), (TimeRate::X4, None));
        assert!(!update(&mut s, GameMode::Campaign, &me, Ok(()), &l), "no change, no event");
        assert!(update(&mut s, GameMode::Campaign, &me, Err(CompressionBlock::WeaponInbound), &l));
        assert_eq!((s.effective, s.blocked), (TimeRate::X1, Some(CompressionBlock::WeaponInbound)));
        update(&mut s, GameMode::Campaign, &me, Ok(()), &l);
        assert_eq!(s.effective, TimeRate::X4, "resumes while the request stands");
    }

    #[test]
    fn coop_takes_the_lowest_request_and_never_pauses() {
        let mut s = TimeState { paused: true, ..Default::default() };
        let l = LadderState::default();
        let both = [PlayerSlot(0), PlayerSlot(1)];
        request_rate(&mut s, PlayerSlot(0), TimeRate::X4);
        update(&mut s, GameMode::Campaign, &both, Ok(()), &l);
        assert!(!s.paused);
        assert_eq!((s.effective, s.blocked), (TimeRate::X1, Some(CompressionBlock::NotAllPlayers)));
        request_rate(&mut s, PlayerSlot(1), TimeRate::X2);
        update(&mut s, GameMode::Campaign, &both, Ok(()), &l);
        assert_eq!((s.effective, s.blocked), (TimeRate::X2, None));
        // A disconnected player no longer holds the rate down.
        request_rate(&mut s, PlayerSlot(1), TimeRate::X1);
        update(&mut s, GameMode::Campaign, &both[..1], Ok(()), &l);
        assert_eq!(s.effective, TimeRate::X4);
    }

    #[test]
    fn pvp_forbids_and_the_ladder_caps() {
        let mut s = TimeState::default();
        let me = [PlayerSlot(0)];
        request_rate(&mut s, PlayerSlot(0), TimeRate::X4);
        update(&mut s, GameMode::PvpElimination, &me, Ok(()), &LadderState::default());
        assert_eq!((s.effective, s.blocked), (TimeRate::X1, Some(CompressionBlock::ModeForbids)));
        let capped = LadderState { max_rate: 2, ..Default::default() };
        update(&mut s, GameMode::Skirmish, &me, Ok(()), &capped);
        assert_eq!((s.effective, s.blocked), (TimeRate::X2, Some(CompressionBlock::HostLoad)));
        let slow = LadderState { rung: 4, sub_step: 0, max_rate: 1, ..Default::default() };
        update(&mut s, GameMode::Skirmish, &me, Ok(()), &slow);
        assert_eq!((s.effective, s.blocked), (TimeRate::X1, Some(CompressionBlock::TimeScaleDegraded)));
    }

    #[test]
    fn safety_uses_player_knowledge_and_envelopes() {
        let hull = [(DVec3::ZERO, 10_000.0)];
        let mut none = |_: &ClassId| 0.0;
        // Friend and Neutral never block; a Suspect outside every envelope does not either.
        let ts = [track(1, DVec3::new(500.0, 0.0, 0.0), Stance::Friend, Domain::Surface), track(2, DVec3::new(800.0, 0.0, 0.0), Stance::Neutral, Domain::Surface), track(3, DVec3::new(20_000.0, 0.0, 0.0), Stance::Suspect, Domain::Surface)];
        assert_eq!(safety_core(&hull, ts.iter(), &mut none, 0), Ok(()));
        // A Hostile inside our envelope.
        let ts = [track(4, DVec3::new(0.0, 0.0, 9_000.0), Stance::Hostile, Domain::Surface)];
        assert_eq!(safety_core(&hull, ts.iter(), &mut none, 0), Err(CompressionBlock::ThreatInOwnEnvelope));
        // We are inside the envelope the plot draws for an identified class at 15 km.
        let mut t = track(5, DVec3::new(15_000.0, 0.0, 0.0), Stance::Suspect, Domain::Surface);
        t.class = Some(ClassId("destroyer".into()));
        let mut env = |_: &ClassId| 18_000.0;
        assert_eq!(safety_core(&hull, [t.clone()].iter(), &mut env, 0), Err(CompressionBlock::InsideThreatEnvelope));
        // An inbound weapon outranks everything, and an override to Hostile on a Friend changes nothing.
        let mut w = track(6, DVec3::new(30_000.0, 50.0, 0.0), Stance::Hostile, Domain::Weapon);
        w.inbound = true;
        let mut f = track(7, DVec3::new(100.0, 0.0, 0.0), Stance::Friend, Domain::Surface);
        f.stance_override = Some(Stance::Hostile);
        assert_eq!(safety_core(&hull, [t, w, f].iter(), &mut env, 0), Err(CompressionBlock::WeaponInbound));
        // A dropped track is a last-known mark, not knowledge of a present threat.
        let mut d = track(8, DVec3::new(10.0, 0.0, 0.0), Stance::Hostile, Domain::Surface);
        d.dropped_tick = Some(5);
        assert_eq!(safety_core(&hull, [d].iter(), &mut none, 10), Ok(()));
    }
}
