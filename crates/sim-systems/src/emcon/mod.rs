//! Unit `sys-emcon`. Emission control: the Sensors officer's `EmissionController` stage and the ESM warning.
//! DESIGN.md L505-514.
//!
//! One L1 setting per ship: Silent, Burst (default), Active. All radars, jammers and active sonar follow it.
//! - Silent: every emitter off. Exception: point-defence fire-control radars radiate while an inbound weapon is
//!   inside 8 km (`DoctrineTuning::silent_pd_radar_m`), shown on the Bridge Board.
//! - Burst: passive until a Suspect or Hostile track from a passive source (ESM, sonar, drone, visual) is
//!   inside the trigger range (20 km default; the editor suggests one per design) or a weapon is inbound; then
//!   bursts of 3 antenna rotations (7.5 s), repeated every 30 s while the trigger holds, every 10 s while a
//!   track with a weapon assigned is graded below Lock. Fire-control lock is held only while own weapons are
//!   in flight or point defence is engaging.
//! - Active: radiates continuously. Ship lights burn only in Active (L436).
//!
//! ESM warning (player knowledge only, never ground truth): on when any own emitter radiated in the last
//! 60 s and any Suspect or Hostile track or briefing-declared ESM site lies within
//! min(radar horizon from the emitter's height to an assumed 20 m mast, 2.0 x the emitter's R_ref).
//!
//! Decisions where the spec is silent:
//! - A burst radiates every emitter (search, navigation and fire-control radars, jammers, active sonar).
//!   Between bursts only the fire-control radars radiate, and only while the lock is held (own weapons in
//!   flight or point defence engaging). Point-defence radars keep the Silent exception in Burst too.
//! - A burst ends early when its trigger drops: Burst exists to keep emissions short.
//! - A weapon inbound repeats bursts at the engaged interval (10 s), the same as a weapon-assigned track below
//!   Lock; a weapon-assigned track below Lock is itself a trigger, so a missile engagement beyond the trigger
//!   range still gets its refresh sweeps.
//! - "Passive source" means the track's sources include ESM, passive sonar, visual, camera, datalink or an
//!   offboard sensor; tracks of the weapon domain trigger through `inbound_weapon_m` only.
//! - The ESM warning ignores weapon-domain tracks (weapons carry no ESM receiver that reports back).
//! - Shellfire is a weapon inbound (DECISION, s4, 22 November 2026, on the ruling that a ship under fire defends
//!   itself with its guns at stock officer settings). L507 and L509 write the inbound-weapon rule for weapon
//!   tracks, and a shell has no track. A hull is under fire while an enemy shell is about to pass within the
//!   near-miss distance of it (`EmconIn::under_fire`, read by the stage runner) and for
//!   [`SELF_DEFENCE_S`] after the last one (60 s, the "under fire" window of L2540).
//!   - Burst: under fire is the weapon-inbound trigger, so bursts start and repeat at the engaged cadence. Before
//!     this a hull in Burst with no passive contact (attackers that do not radiate, in visibility under their
//!     range) never radiated and was shot to a stop with no track to fire at.
//!   - Silent: under fire, every fire-control radar radiates, as the point-defence ones do for an inbound weapon.
//!     This departs from L507, which names one exception; it is the `under_fire && e.kind == FireControlRadar`
//!     condition in the Silent arm below, and removing it leaves the Burst trigger as it is. `EmitterInfo` does
//!     not say which director serves which gun, so all of them light. Search and navigation radars, jammers
//!     and active sonar stay off. Before this a Silent hull in fog or at night held only ESM bearings of the
//!     hulls shelling it and its guns held on the grade to the end. Cost: a "no radar emissions" ghost
//!     condition fails once the hull is shelled.
use crate::prelude::*;
use crate::protocol::snapshot::SourceMask;
use crate::sensors::radar::horizon_m;
use crate::signature::EmitterKind;
use crate::tracks::Track;
use content::part::{PartFunction, SensorKind};

/// One emitter the controller can switch.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EmitterInfo { pub slot: PartSlot, pub kind: EmitterKind, pub r_ref_m: f64, pub height_m: f64, /// Seconds per antenna rotation (2.5 s at 24 rpm); 0 for staring emitters.
    pub rotation_s: f64, /// Fire-control radar that serves point-defence mounts (Silent exception).
    pub point_defence: bool }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmconPhase { #[default] Quiet, Bursting, Continuous, PointDefenceOnly }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EmconState {
    pub phase: EmconPhase,
    pub burst_end_tick: u64,
    pub next_burst_tick: u64,
    /// Emitters ordered to radiate this tick, ascending slot.
    pub radiating: Vec<PartSlot>,
    pub last_radiated_tick: Option<u64>,
    pub esm_warning: bool,
    /// Last tick each emitter radiated, ascending slot (the per-emitter reach of the ESM warning).
    #[serde(default)]
    pub last_radiated: Vec<(PartSlot, u64)>,
    /// Last tick an enemy shell was inbound on this hull (the self-defence window).
    #[serde(default)]
    pub under_fire_tick: Option<u64>,
    /// Silent, and only the under-fire window lit a radar this tick (no point-defence window): the Bridge
    /// Board's reason is [`EmconReason::SelfDefenceUnderFire`].
    #[serde(default)]
    pub self_defence_only: bool,
}

pub struct EmconIn<'a> {
    pub setting: Emcon,
    pub trigger_range_m: f64,
    pub own_pos: DVec3,
    pub emitters: &'a [EmitterInfo],
    /// The side picture (live tracks).
    pub tracks: &'a [Track],
    /// Range to the nearest inbound weapon track, if any.
    pub inbound_weapon_m: Option<f64>,
    pub own_weapons_in_flight: bool,
    pub point_defence_engaging: bool,
    /// A track with a weapon assigned is graded below Lock.
    pub assigned_below_lock: bool,
    /// An enemy shell is about to hit or near-miss this hull (the stage runner's reading of the shells in
    /// flight; see the module notes).
    pub under_fire: bool,
    pub esm_sites: &'a [[f64; 2]],
    pub tick: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EmconOut {
    /// Per emitter in input order: radiate this tick.
    pub radiate: Vec<bool>,
    pub lights: bool,
    pub esm_warning: bool,
    /// One-line reason for the Bridge Board.
    pub reason: EmconReason,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum EmconReason { #[default] SilentOrdered, PassiveNoTrigger, BurstTrackInside { track: TrackId, range_m: f64 }, BurstWeaponInbound, BurstRefreshForWeapon, HoldingLockWeaponsInFlight, ActiveOrdered, PointDefenceException,
    /// Silent, under fire: the fire-control radars radiate for the guns (the under-fire window), and no
    /// point-defence window is open.
    SelfDefenceUnderFire }

const HZ: f64 = sim_core::TICK_HZ as f64;
/// Seconds per rotation when no rotating emitter is fitted (24 rpm, L412).
const DEFAULT_ROTATION_S: f64 = 2.5;
/// ESM warning memory: an emitter that radiated in the last 60 s (L510).
const ESM_MEMORY_S: f64 = 60.0;
/// A hull stays under fire this long after the last shell inbound on it (L2540: "under fire in the last 60 s").
pub const SELF_DEFENCE_S: f64 = 60.0;

fn ticks(s: f64) -> u64 { if s.is_finite() && s > 0.0 { (s * HZ).round().min(1.0e12) as u64 } else { 0 } }
fn pos_or(x: f64, default: f64) -> f64 { if x.is_finite() && x > 0.0 { x } else { default } }

/// Sources that count as passive for the Burst trigger.
fn passive_sources() -> SourceMask { SourceMask::ESM | SourceMask::PASSIVE_SONAR | SourceMask::VISUAL | SourceMask::CAMERA | SourceMask::DATALINK | SourceMask::OFFBOARD }

fn horizontal(a: DVec3, b: DVec3) -> f64 { let (dx, dz) = (a.x - b.x, a.z - b.z); (dx * dx + dz * dz).sqrt() }

/// Nearest live Suspect or Hostile non-weapon track from a passive source inside the trigger range.
fn trigger_track(input: &EmconIn<'_>, range_m: f64) -> Option<(TrackId, f64)> {
    let passive = passive_sources();
    let mut best: Option<(TrackId, f64)> = None;
    for t in input.tracks {
        if !t.is_live() || t.domain == Domain::Weapon || t.stance < Stance::Suspect || !t.sources.intersects(passive) { continue; }
        let r = horizontal(t.predict(input.tick), input.own_pos);
        if !(r.is_finite() && r <= range_m) { continue; }
        if best.is_none_or(|(bid, br)| r < br || (r == br && t.id < bid)) { best = Some((t.id, r)); }
    }
    best
}

/// Burst length in ticks: `burst_rotations` turns of the slowest rotating emitter.
fn burst_len_ticks(emitters: &[EmitterInfo], tuning: &TuningTable) -> u64 {
    let rot = emitters.iter().map(|e| e.rotation_s).filter(|r| r.is_finite() && *r > 0.0).fold(0.0f64, f64::max);
    let rot = if rot > 0.0 { rot.min(60.0) } else { DEFAULT_ROTATION_S };
    ticks(rot * tuning.doctrine.burst_rotations.max(1) as f64).max(1)
}

/// The `EmissionController` stage: decide which emitters radiate this graph tick and update the burst timers.
/// Experts may replace it with their own graph (L514). Unit: `sys-emcon`.
pub fn emission_controller(input: &EmconIn<'_>, state: &mut EmconState, tuning: &TuningTable) -> EmconOut {
    let d = &tuning.doctrine;
    let tick = input.tick;
    // A restored or rewound state ahead of the clock: forget timers rather than wait on them.
    if state.last_radiated_tick.is_some_and(|l| l > tick) || state.burst_end_tick > tick.saturating_add(ticks(600.0)) {
        *state = EmconState { esm_warning: state.esm_warning, ..Default::default() };
    }
    state.last_radiated.retain(|&(_, t)| t <= tick);
    if input.under_fire { state.under_fire_tick = Some(tick); }
    if state.under_fire_tick.is_some_and(|t| t > tick) { state.under_fire_tick = None; }
    let under_fire = state.under_fire_tick.is_some_and(|t| tick - t <= ticks(SELF_DEFENCE_S));

    let n = input.emitters.len();
    let mut radiate = vec![false; n];
    let inbound = input.inbound_weapon_m.filter(|r| r.is_finite() && *r >= 0.0);
    let pd_window = inbound.is_some_and(|r| r <= pos_or(d.silent_pd_radar_m, 8_000.0));
    let is_pd = |e: &EmitterInfo| e.point_defence && e.kind == EmitterKind::FireControlRadar;
    let mut lights = false;
    let (phase, reason) = match input.setting {
        Emcon::Active => {
            radiate.iter_mut().for_each(|r| *r = true);
            lights = true;
            state.burst_end_tick = 0;
            (EmconPhase::Continuous, EmconReason::ActiveOrdered)
        }
        Emcon::Silent => {
            state.burst_end_tick = 0;
            let (mut pd_lit, mut fire_lit) = (false, false);
            for (i, e) in input.emitters.iter().enumerate() {
                if pd_window && is_pd(e) { radiate[i] = true; pd_lit = true; }
                else if under_fire && e.kind == EmitterKind::FireControlRadar { radiate[i] = true; fire_lit = true; }
            }
            // The Bridge Board names the exception that lit the radar (DECISION, r4, from s4).
            if pd_lit { (EmconPhase::PointDefenceOnly, EmconReason::PointDefenceException) }
            else if fire_lit { (EmconPhase::PointDefenceOnly, EmconReason::SelfDefenceUnderFire) }
            else { (EmconPhase::Quiet, EmconReason::SilentOrdered) }
        }
        Emcon::Burst => {
            let range = pos_or(input.trigger_range_m, pos_or(d.burst_trigger_m, 20_000.0));
            let near = trigger_track(input, range);
            let weapon = inbound.is_some() || under_fire;
            let refresh = input.assigned_below_lock;
            let trigger = near.is_some() || weapon || refresh;
            let len = burst_len_ticks(input.emitters, tuning);
            let repeat = ticks(if weapon || refresh { pos_or(d.burst_repeat_engaged_s, 10.0) } else { pos_or(d.burst_repeat_s, 30.0) }).max(len);
            let mut in_burst = tick < state.burst_end_tick;
            if trigger {
                let last_start = if state.burst_end_tick > 0 { Some(state.burst_end_tick.saturating_sub(len)) } else { None };
                // The engaged cadence may shorten a wait set at the slower one.
                if let Some(s) = last_start { state.next_burst_tick = state.next_burst_tick.min(s.saturating_add(repeat)); }
                if !in_burst && tick >= state.next_burst_tick {
                    state.burst_end_tick = tick.saturating_add(len);
                    state.next_burst_tick = tick.saturating_add(repeat);
                    in_burst = true;
                }
            } else if in_burst {
                state.burst_end_tick = tick;
                in_burst = false;
            }
            let hold_lock = input.own_weapons_in_flight || input.point_defence_engaging;
            for (i, e) in input.emitters.iter().enumerate() {
                let fc = e.kind == EmitterKind::FireControlRadar;
                radiate[i] = in_burst || (hold_lock && fc) || (is_pd(e) && (pd_window || input.point_defence_engaging));
            }
            let any = radiate.iter().any(|&r| r);
            let phase = if in_burst || trigger { EmconPhase::Bursting } else if hold_lock { EmconPhase::Continuous } else if any { EmconPhase::PointDefenceOnly } else { EmconPhase::Quiet };
            let reason = if in_burst || trigger {
                if weapon { EmconReason::BurstWeaponInbound }
                else if let Some((track, range_m)) = near { EmconReason::BurstTrackInside { track, range_m } }
                else if hold_lock { EmconReason::HoldingLockWeaponsInFlight }
                else { EmconReason::BurstRefreshForWeapon }
            } else if hold_lock { EmconReason::HoldingLockWeaponsInFlight }
            else if any { EmconReason::PointDefenceException }
            else { EmconReason::PassiveNoTrigger };
            (phase, reason)
        }
    };

    // Record what radiates before the warning test: the gate allows the warning at most 1 s late.
    let mut slots: Vec<PartSlot> = input.emitters.iter().zip(&radiate).filter(|(_, r)| **r).map(|(e, _)| e.slot).collect();
    slots.sort_unstable();
    slots.dedup();
    for &s in &slots {
        match state.last_radiated.binary_search_by_key(&s, |x| x.0) {
            Ok(i) => state.last_radiated[i].1 = tick,
            Err(i) => state.last_radiated.insert(i, (s, tick)),
        }
    }
    if !slots.is_empty() { state.last_radiated_tick = Some(tick); }
    let memory = ticks(ESM_MEMORY_S);
    state.last_radiated.retain(|&(_, t)| tick - t <= memory);
    state.radiating = slots;
    state.phase = phase;
    state.self_defence_only = reason == EmconReason::SelfDefenceUnderFire;
    let warn = esm_warning(input, state, tuning);
    state.esm_warning = warn;
    EmconOut { radiate, lights, esm_warning: warn, reason }
}

/// The ESM warning test alone (L510). Unit: `sys-emcon`.
pub fn esm_warning(input: &EmconIn<'_>, state: &EmconState, tuning: &TuningTable) -> bool {
    let tick = input.tick;
    let memory = ticks(ESM_MEMORY_S);
    let mast = pos_or(tuning.sensors.assumed_enemy_mast_m, 20.0);
    let factor = pos_or(tuning.sensors.esm_range_factor, 2.0);
    // Widest reach among emitters that radiated inside the memory window.
    let mut reach = 0.0f64;
    for e in input.emitters {
        let last = state.last_radiated.binary_search_by_key(&e.slot, |x| x.0).ok().map(|i| state.last_radiated[i].1)
            .or_else(|| if state.radiating.contains(&e.slot) { Some(tick) } else { None });
        let Some(last) = last else { continue };
        if last > tick || tick - last > memory { continue; }
        let h = if e.height_m.is_finite() { e.height_m } else { 0.0 };
        let r = horizon_m(h, mast).min(factor * pos_or(e.r_ref_m, 0.0));
        if r.is_finite() { reach = reach.max(r); }
    }
    if reach <= 0.0 { return false; }
    let own = input.own_pos;
    let site = input.esm_sites.iter().any(|s| { let (dx, dz) = (s[0] - own.x, s[1] - own.z); let r = (dx * dx + dz * dz).sqrt(); r.is_finite() && r <= reach });
    site || input.tracks.iter().any(|t| {
        t.is_live() && t.domain != Domain::Weapon && t.stance >= Stance::Suspect && { let r = horizontal(t.predict(tick), own); r.is_finite() && r <= reach }
    })
}

/// The reference threat of the Suggest value: an Act 1 Combine patrol craft of about 150 t (about 150 m^2,
/// L432) with its search radar (R_ref 20 km, small mast) at the Sight gauge's 20 m mast height and an
/// acoustic noise of N = 1 (2 MW of propulsion, L453).
const REF_TOP_M: f64 = 20.0;
const REF_AREA_M2: f64 = 150.0;
const REF_RADAR_R_REF_M: f64 = 20_000.0;
const REF_NOISE: f64 = 1.0;
/// Clamp of the suggested trigger range, metres, rounded to 500 m.
const SUGGEST_MIN_M: f64 = 4_000.0;
const SUGGEST_MAX_M: f64 = 60_000.0;

/// First-detection range of the reference threat by one passive sensor at a height above the sea, or `None`
/// for active sensors.
pub(crate) fn passive_reach_m(kind: SensorKind, r_ref_m: f64, height_m: f64, tuning: &TuningTable) -> Option<f64> {
    let s = &tuning.sensors;
    let h = if height_m.is_finite() { height_m } else { 0.0 };
    let visual = || pos_or(s.visual_ref_range_m, 10_000.0) * (REF_AREA_M2 / pos_or(s.sigma_ref_m2, 100.0)).sqrt();
    let r = match kind {
        SensorKind::Esm => horizon_m(h, REF_TOP_M).min(pos_or(s.esm_range_factor, 2.0) * REF_RADAR_R_REF_M),
        SensorKind::Visual => visual().min(horizon_m(h, REF_TOP_M)),
        SensorKind::Camera => (visual() * pos_or(s.camera_range_factor, 2.0)).min(horizon_m(h, REF_TOP_M)),
        SensorKind::PassiveSonar => pos_or(r_ref_m, pos_or(s.passive_sonar_r_ref_m, 8_000.0)) * REF_NOISE.sqrt(),
        _ => return None,
    };
    if r.is_finite() && r > 0.0 { Some(r) } else { None }
}

fn suggest_from(reaches: impl Iterator<Item = f64>, tuning: &TuningTable) -> f64 {
    let best = reaches.filter(|r| r.is_finite() && *r > 0.0).fold(0.0f64, f64::max);
    let r = if best > 0.0 { best } else { pos_or(tuning.doctrine.burst_trigger_m, 20_000.0) };
    ((r / 500.0).round() * 500.0).clamp(SUGGEST_MIN_M, SUGGEST_MAX_M)
}

/// Suggested burst trigger range for a design (Suggest button, L210): the range at which the design's best
/// passive sensor first detects the act's reference threat. Unit: `sys-emcon`.
///
/// The reference threat is the Act 1 patrol craft described at [`REF_TOP_M`]; the result is rounded to
/// 500 m and clamped to 4..=60 km. A design with no passive sensor gets the default trigger range.
pub fn suggest_trigger_range_m(design: &crate::ship::ShipDesign, catalogue: &Catalogue) -> f64 {
    let prefab = &design.prefab;
    let tuning = &catalogue.tuning;
    let waterline = if prefab.waterline_y.is_finite() { prefab.waterline_y } else { 0.0 };
    let reaches = design.sensor_slots.iter().filter_map(|slot| {
        let p = prefab.parts.get(slot.index()).filter(|p| p.slot == *slot).or_else(|| prefab.parts.iter().find(|p| p.slot == *slot))?;
        let (_, def) = catalogue.parts.get_index(p.def.index())?;
        let (kind, r_ref) = match &def.function {
            PartFunction::Sensor(s) => (s.kind, s.r_ref_m),
            PartFunction::CameraMast { .. } => (SensorKind::Camera, 0.0),
            _ => return None,
        };
        passive_reach_m(kind, r_ref, p.centre.y - waterline, tuning)
    });
    suggest_from(reaches, tuning)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracks::Track;

    fn em(slot: u16, kind: EmitterKind, pd: bool) -> EmitterInfo { EmitterInfo { slot: PartSlot(slot), kind, r_ref_m: 20_000.0, height_m: 16.0, rotation_s: 2.5, point_defence: pd } }
    fn emitters() -> Vec<EmitterInfo> { vec![em(3, EmitterKind::SearchRadar, false), em(5, EmitterKind::FireControlRadar, true)] }

    fn track(id: u32, x: f64, stance: Stance, sources: SourceMask) -> Track {
        let mut t = blank();
        t.id = TrackId(id);
        t.pos = DVec3::new(x, 0.0, 0.0);
        t.stance = stance;
        t.sources = sources;
        t
    }
    /// A blank surface track without going through `tracks` internals.
    fn blank() -> Track {
        Track {
            id: TrackId(1), side: SideId(0), domain: Domain::Surface, truth: EntityId::NONE, pos: DVec3::ZERO, vel: DVec3::ZERO, r0_m: 10.0, sigma_v: 1.0,
            bearing_only: None, created_tick: 0, last_update_tick: 0, last_lock_tick: None, dropped_tick: None, grade: TrackGrade::Coarse,
            sources: SourceMask::empty(), trend: Default::default(), rung: IdRung::Unknown, rung_reason: RungReason::None, candidates: Default::default(),
            class: None, history: Vec::new(), rcs_m2: 0.0, consistent_ticks: 0, emitters: Vec::new(), id_override: None, stance: Stance::Suspect,
            stance_reason: StanceReason::Default, stance_override: None, fired_on_us: false, inbound: false, engaged_by: 0, emitting: false, fit: None, out_of_action: false, struck: false,
        }
    }

    fn input<'a>(setting: Emcon, ems: &'a [EmitterInfo], tracks: &'a [Track], tick: u64) -> EmconIn<'a> {
        EmconIn { setting, trigger_range_m: 20_000.0, own_pos: DVec3::ZERO, emitters: ems, tracks, inbound_weapon_m: None, own_weapons_in_flight: false,
            point_defence_engaging: false, assigned_below_lock: false, under_fire: false, esm_sites: &[], tick }
    }

    #[test]
    fn a_hull_under_fire_lights_what_its_guns_need() {
        let t = TuningTable::default();
        // A search radar, a gun director and a point-defence director.
        let ems = vec![em(3, EmitterKind::SearchRadar, false), em(4, EmitterKind::FireControlRadar, false), em(5, EmitterKind::FireControlRadar, true)];
        let window = ticks(SELF_DEFENCE_S);
        // Silent: the fire-control radars radiate from the first shell to 60 s after the last; nothing else does.
        let mut s = EmconState::default();
        assert_eq!(emission_controller(&input(Emcon::Silent, &ems, &[], 100), &mut s, &t).radiate, vec![false, false, false]);
        let mut i = input(Emcon::Silent, &ems, &[], 101);
        i.under_fire = true;
        let out = emission_controller(&i, &mut s, &t);
        assert_eq!((out.radiate, out.reason, out.lights, s.phase), (vec![false, true, true], EmconReason::SelfDefenceUnderFire, false, EmconPhase::PointDefenceOnly));
        assert_eq!(s.radiating, vec![PartSlot(4), PartSlot(5)]);
        assert_eq!(emission_controller(&input(Emcon::Silent, &ems, &[], 101 + window), &mut s, &t).radiate, vec![false, true, true], "still inside the window");
        let out = emission_controller(&input(Emcon::Silent, &ems, &[], 102 + window), &mut s, &t);
        assert_eq!((out.radiate, out.reason, s.phase), (vec![false, false, false], EmconReason::SilentOrdered, EmconPhase::Quiet));
        // Burst with no contact at all: shellfire is the weapon-inbound trigger, at the engaged cadence.
        let mut s = EmconState::default();
        assert_eq!(emission_controller(&input(Emcon::Burst, &ems, &[], 100), &mut s, &t).reason, EmconReason::PassiveNoTrigger);
        let mut i = input(Emcon::Burst, &ems, &[], 101);
        i.under_fire = true;
        let out = emission_controller(&i, &mut s, &t);
        assert_eq!((out.radiate, out.reason, s.phase), (vec![true, true, true], EmconReason::BurstWeaponInbound, EmconPhase::Bursting));
        let len = burst_len_ticks(&ems, &t);
        assert_eq!(emission_controller(&input(Emcon::Burst, &ems, &[], 101 + len), &mut s, &t).radiate[0], false, "between bursts");
        let again = 101 + ticks(t.doctrine.burst_repeat_engaged_s);
        assert_eq!(emission_controller(&input(Emcon::Burst, &ems, &[], again), &mut s, &t).radiate[0], true, "the next burst, 10 s on");
        assert_eq!(emission_controller(&input(Emcon::Burst, &ems, &[], 102 + window), &mut s, &t).reason, EmconReason::PassiveNoTrigger);
        // A state restored ahead of the clock forgets the window.
        let mut s = EmconState { under_fire_tick: Some(500), ..Default::default() };
        assert_eq!(emission_controller(&input(Emcon::Silent, &ems, &[], 100), &mut s, &t).radiate, vec![false, false, false]);
        assert_eq!(s.under_fire_tick, None);
    }

    #[test]
    fn silent_and_active() {
        let t = TuningTable::default();
        let ems = emitters();
        let mut s = EmconState::default();
        let out = emission_controller(&input(Emcon::Silent, &ems, &[], 10), &mut s, &t);
        assert_eq!((out.radiate.clone(), out.reason, out.lights), (vec![false, false], EmconReason::SilentOrdered, false));
        let mut i = input(Emcon::Silent, &ems, &[], 11);
        i.inbound_weapon_m = Some(7_000.0);
        let out = emission_controller(&i, &mut s, &t);
        assert_eq!((out.radiate.clone(), out.reason, s.phase), (vec![false, true], EmconReason::PointDefenceException, EmconPhase::PointDefenceOnly));
        i.inbound_weapon_m = Some(9_000.0);
        i.tick = 12;
        assert_eq!(emission_controller(&i, &mut s, &t).radiate, vec![false, false]);
        let out = emission_controller(&input(Emcon::Active, &ems, &[], 13), &mut s, &t);
        assert_eq!((out.radiate, out.lights, out.reason), (vec![true, true], true, EmconReason::ActiveOrdered));
        assert_eq!(s.radiating, vec![PartSlot(3), PartSlot(5)]);
    }

    #[test]
    fn burst_cycle_follows_trigger() {
        let t = TuningTable::default();
        let ems = emitters();
        let mut s = EmconState::default();
        // A radar-only track does not trigger; an ESM one inside 20 km does.
        let radar_only = [track(1, 15_000.0, Stance::Hostile, SourceMask::SEARCH_RADAR)];
        let out = emission_controller(&input(Emcon::Burst, &ems, &radar_only, 100), &mut s, &t);
        assert_eq!((out.reason, s.phase), (EmconReason::PassiveNoTrigger, EmconPhase::Quiet));
        let far = [track(2, 25_000.0, Stance::Hostile, SourceMask::ESM)];
        assert!(!emission_controller(&input(Emcon::Burst, &ems, &far, 101), &mut s, &t).radiate.iter().any(|&r| r));
        let neutral = [track(2, 15_000.0, Stance::Neutral, SourceMask::ESM)];
        assert!(!emission_controller(&input(Emcon::Burst, &ems, &neutral, 102), &mut s, &t).radiate.iter().any(|&r| r));
        let near = [track(2, 15_000.0, Stance::Suspect, SourceMask::ESM)];
        let start = 200u64;
        let out = emission_controller(&input(Emcon::Burst, &ems, &near, start), &mut s, &t);
        assert_eq!(out.radiate, vec![true, true]);
        assert_eq!(out.reason, EmconReason::BurstTrackInside { track: TrackId(2), range_m: 15_000.0 });
        // 3 rotations of 2.5 s = 450 ticks; then quiet until 30 s after the start.
        assert_eq!(s.burst_end_tick, start + 450);
        assert_eq!(s.next_burst_tick, start + 1_800);
        assert!(emission_controller(&input(Emcon::Burst, &ems, &near, start + 449), &mut s, &t).radiate[0]);
        let out = emission_controller(&input(Emcon::Burst, &ems, &near, start + 450), &mut s, &t);
        assert_eq!((out.radiate.clone(), s.phase), (vec![false, false], EmconPhase::Bursting));
        assert!(!emission_controller(&input(Emcon::Burst, &ems, &near, start + 1_799), &mut s, &t).radiate[0]);
        assert!(emission_controller(&input(Emcon::Burst, &ems, &near, start + 1_800), &mut s, &t).radiate[0]);
        // A weapon-assigned track below Lock shortens the cadence to 10 s.
        // The last burst started at start + 1,800, so the 10 s cadence makes one due at start + 2,400.
        let mut i = input(Emcon::Burst, &ems, &near, start + 2_400);
        i.assigned_below_lock = true;
        assert!(emission_controller(&i, &mut s, &t).radiate[0]);
        assert_eq!((s.burst_end_tick, s.next_burst_tick), (start + 2_850, start + 3_000));
        // Trigger drops: the burst stops at once and the phase goes quiet.
        let out = emission_controller(&input(Emcon::Burst, &ems, &[], start + 2_500), &mut s, &t);
        assert_eq!((out.radiate, s.phase, out.reason), (vec![false, false], EmconPhase::Quiet, EmconReason::PassiveNoTrigger));
    }

    #[test]
    fn engaged_cadence_and_lock_hold() {
        let t = TuningTable::default();
        let ems = emitters();
        let mut s = EmconState::default();
        let near = [track(2, 15_000.0, Stance::Hostile, SourceMask::VISUAL)];
        let mut i = input(Emcon::Burst, &ems, &near, 1_000);
        i.assigned_below_lock = true;
        assert!(emission_controller(&i, &mut s, &t).radiate[0]);
        assert_eq!(s.next_burst_tick, 1_600);
        i.tick = 1_599;
        assert!(!emission_controller(&i, &mut s, &t).radiate[0]);
        i.tick = 1_600;
        assert!(emission_controller(&i, &mut s, &t).radiate[0]);
        // No trigger, own weapons in flight: fire control only, Continuous.
        let mut j = input(Emcon::Burst, &ems, &[], 3_000);
        j.own_weapons_in_flight = true;
        let out = emission_controller(&j, &mut s, &t);
        assert_eq!((out.radiate, s.phase, out.reason), (vec![false, true], EmconPhase::Continuous, EmconReason::HoldingLockWeaponsInFlight));
        // Weapon inbound alone triggers a burst.
        let mut k = input(Emcon::Burst, &ems, &[], 9_000);
        k.inbound_weapon_m = Some(12_000.0);
        let out = emission_controller(&k, &mut s, &t);
        assert_eq!((out.radiate, out.reason), (vec![true, true], EmconReason::BurstWeaponInbound));
    }

    #[test]
    fn esm_warning_uses_reach_and_memory() {
        let t = TuningTable::default();
        let ems = emitters();
        // Reach: min(4.12 km x (4 + sqrt 20), 40 km) = 34.9 km.
        let tracks = [track(1, 30_000.0, Stance::Hostile, SourceMask::ESM)];
        let mut s = EmconState::default();
        let out = emission_controller(&input(Emcon::Silent, &ems, &tracks, 10), &mut s, &t);
        assert!(!out.esm_warning, "nothing radiated");
        let out = emission_controller(&input(Emcon::Active, &ems, &tracks, 20), &mut s, &t);
        assert!(out.esm_warning, "radiating with a hostile inside reach warns on the same tick");
        assert!(emission_controller(&input(Emcon::Silent, &ems, &tracks, 20 + 3_600), &mut s, &t).esm_warning);
        assert!(!emission_controller(&input(Emcon::Silent, &ems, &tracks, 20 + 3_601), &mut s, &t).esm_warning);
        let far = [track(1, 36_000.0, Stance::Hostile, SourceMask::ESM)];
        let mut s = EmconState::default();
        assert!(!emission_controller(&input(Emcon::Active, &ems, &far, 5), &mut s, &t).esm_warning);
        let mut i = input(Emcon::Active, &ems, &far, 6);
        let sites = [[0.0, 30_000.0]];
        i.esm_sites = &sites;
        assert!(emission_controller(&i, &mut s, &t).esm_warning);
    }

    #[test]
    fn robust_to_bad_input() {
        let t = TuningTable::default();
        let mut ems = emitters();
        ems[0].rotation_s = f64::NAN;
        ems[0].height_m = f64::NAN;
        // A far track with a non-finite trigger range uses the 20 km default.
        let tracks = [track(1, 25_000.0, Stance::Hostile, SourceMask::ESM)];
        let mut s = EmconState { last_radiated_tick: Some(1_000_000), burst_end_tick: 999_999_999, ..Default::default() };
        let mut i = input(Emcon::Burst, &ems, &tracks, 50);
        i.trigger_range_m = f64::NAN;
        i.inbound_weapon_m = Some(f64::NAN);
        let out = emission_controller(&i, &mut s, &t);
        assert_eq!(out.radiate, vec![false, false]);
        assert!(s.last_radiated_tick.is_none());
    }

    #[test]
    fn suggest_reaches() {
        let t = TuningTable::default();
        // ESM at 16 m: min(4.12 x (4 + 4.47) km, 40 km) = 34.9 km.
        let esm = passive_reach_m(SensorKind::Esm, 0.0, 16.0, &t).unwrap();
        assert!((esm - 34_904.0).abs() < 5.0, "{esm}");
        // Visual: 10 km x sqrt(1.5) = 12.25 km; camera doubles it.
        let vis = passive_reach_m(SensorKind::Visual, 0.0, 16.0, &t).unwrap();
        assert!((vis - 12_247.4).abs() < 1.0);
        assert!((passive_reach_m(SensorKind::Camera, 0.0, 16.0, &t).unwrap() - 2.0 * vis).abs() < 1.0);
        assert_eq!(passive_reach_m(SensorKind::PassiveSonar, 8_000.0, -4.0, &t), Some(8_000.0));
        assert_eq!(passive_reach_m(SensorKind::SearchRadar, 40_000.0, 16.0, &t), None);
        assert_eq!(suggest_from([esm, vis].into_iter(), &t), 35_000.0);
        assert_eq!(suggest_from(std::iter::empty(), &t), 20_000.0);
        assert_eq!(suggest_from([f64::NAN, 1_000.0].into_iter(), &t), 4_000.0);
    }
}
