//! Unit `sys-ai-air` (Phase::Act2: schedule with Act 2). Enemy strike aircraft and air patrols.
//! DESIGN.md L52 (aircraft exist only as enemies, missiles and drones), L749 (air patrols appear at Alert 3),
//! L866-875 (Act 2 adds enemy strike aircraft), L1447 (minimum swarm carries 6 enemy aircraft).
//!
//! Aircraft are sim-core small bodies (`MunitionKind::Aircraft`): 5 m sphere, 6 hit points, reference RCS
//! 5 m^2, with sensors and a payload from `MunitionDef`. This unit flies them (`Steer::Orbit`,
//! `Steer::Waypoint`) and decides weapon release against tracks in their side's picture. Released weapons
//! are ordinary guided bodies handed to `crate::guidance`.
//!
//! Each aircraft runs a small state machine every half second (jittered), in ascending body order:
//! - `Patrol`: orbits its station. It takes on the nearest Hostile surface or subsurface track (not
//!   bearing-only, perceived for at least the difficulty's reaction time) within 25 km of the station's
//!   circle that it carries a weapon for, preferring a track a group mate is already attacking.
//! - `Ingress(track)`: flies at its cruise altitude to a launch point on the line from the target, 90% of
//!   release range out. Release range is the weapon's kinematic range times
//!   `DoctrineTuning::missile_range_fraction` (L584); depth charges are dropped from 400 m. It does not
//!   release on a track coarser than `Coarse`: it closes to half release range and circles there while its
//!   sensors refine the track. It gives up after 10 minutes.
//! - `Attack(track)`: nose on the target, it releases a salvo (2 anti-ship missiles, 1 torpedo or a pattern of
//!   3 depth charges) one weapon a second, then egresses.
//! - `Egress`: flies directly away from the attack point for 45 s and at least 15 km (2 minutes at most), then
//!   patrols again if it has a weapon left, else returns to base.
//! - `ReturnToBase`: flies to where it was first seen and circles there, out of the fight. Bingo fuel (less
//!   endurance left than 1.25 times the flight home plus 60 s) or half its hit points gone sends any aircraft
//!   home.
//!
//! Over all of these, a weapon track in the side's picture that will pass within 500 m of the aircraft in the
//! next 20 s triggers a 4 s break turn at full lateral acceleration across the line of sight.
//!
//! Releases are requests: the caller (`sys-ai`) spawns each weapon at the aircraft and registers it with
//! guidance against `Release::track`; `payload_left` is already decremented when the request is made. Steering
//! demands are written on decision ticks only (the body keeps its last `Steer`).
use crate::prelude::*;
use crate::tracks::SidePicture;
use content::part::weapons::MunitionKind;
use sim_core::munitions::{BodyPhase, Steer};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum AirTask { #[default] Patrol, Ingress(TrackId), Attack(TrackId), Egress, ReturnToBase }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Aircraft {
    pub body: EntityId, pub side: SideId, pub group: Option<GroupId>, pub task: AirTask, pub station: DVec2, pub radius_m: f64, pub payload_left: Vec<(MunitionIdx, u8)>, pub next_decision_tick: u64,
    /// State machine memory (unit `sys-ai-air`).
    #[serde(default)]
    pub(crate) mem: AirMemory,
}

/// What an aircraft remembers between decisions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AirMemory {
    /// Where the aircraft was first seen (x, z); it returns here.
    pub(crate) home: Option<DVec2>,
    /// `add` got no usable station: the first position seen becomes the station.
    pub(crate) station_unset: bool,
    /// Tick the current task began.
    pub(crate) task_tick: u64,
    pub(crate) last_release_tick: Option<u64>,
    pub(crate) salvo_left: u8,
    /// Where the attack was pressed from, for the egress leg.
    pub(crate) egress_from: Option<DVec2>,
    pub(crate) break_until_tick: u64,
    pub(crate) break_sign: i8,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AirState { pub aircraft: Vec<Aircraft> }

/// A weapon release request: the Sim spawns the body at the aircraft and registers it with guidance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Release { pub aircraft: EntityId, pub munition: MunitionIdx, pub track: TrackId }

/// One step for every aircraft, ascending body entity. Unit: `sys-ai-air`.
///
/// Aircraft whose body is gone or dead are dropped. Returns the number of aircraft decisions made.
pub fn step(state: &mut AirState, world: &World, pictures: &crate::tracks::Pictures, ctx: &SimCtx<'_>, steer: &mut Vec<(EntityId, Steer)>, releases: &mut Vec<Release>) -> u32 {
    let bodies = world.bodies();
    state.aircraft.retain(|a| bodies.row_of(a.body).is_some_and(|r| bodies.phase.get(r).is_some_and(|p| *p != BodyPhase::Dead) && bodies.hp.get(r).is_some_and(|h| *h > 0)));
    state.aircraft.sort_by_key(|a| a.body);
    let now = ctx.tick.0;
    let reaction = (ctx.difficulty.clamped().ai_reaction_s.clamp(0.0, 10.0) * TICKS_PER_S as f64).round() as u64;
    let fraction = pos_or(ctx.tuning.doctrine.missile_range_fraction, 0.8).min(1.0);
    let weapon = |idx: MunitionIdx| weapon_spec(ctx.catalogue, idx, fraction);
    // Who is attacking what, as of the start of this step (order-independent).
    let mates: Vec<(GroupId, TrackId)> = state.aircraft.iter().filter_map(|a| a.group.zip(attack_track(a.task))).collect();
    let mut work = 0;
    for ac in &mut state.aircraft {
        if now < ac.next_decision_tick { continue; }
        let Some(row) = bodies.row_of(ac.body) else { continue };
        let Some((_, def)) = bodies.kind.get(row).and_then(|k| ctx.catalogue.munitions.get_index(k.index())) else { continue };
        let me = Me { pos: bodies.pos[row], vel: bodies.vel.get(row).copied().unwrap_or(DVec3::ZERO), hp: bodies.hp[row], remaining: bodies.remaining.get(row).copied().unwrap_or(f64::INFINITY) };
        let spec = AirSpec::of(def);
        let body = ac.body.to_u64();
        let draw = |code: u64| sim_core::rng::uniform(ctx.key(sim_core::rng::Stream::AiDecision, body, code));
        let env = Env { now, reaction, weapon: &weapon, mates: &mates, break_draw: draw(DRAW_BREAK) };
        let s = decide(ac, &me, &spec, pictures.side(ac.side), &env, releases);
        steer.push((ac.body, s));
        ac.next_decision_tick = now + ((DECISION_TICKS as f64 * (0.85 + 0.3 * clamp01(draw(DRAW_JITTER)))).round() as u64).max(1);
        work += 1;
    }
    work
}

/// Register an aircraft that spawned (arrival of an air patrol or strike). Unit: `sys-ai-air`.
///
/// Keeps `AirState::aircraft` ascending by body and ignores a body already registered. The payload comes from
/// the aircraft's `MunitionDef::payload`; entries that name no catalogue munition or carry nothing are
/// dropped, duplicates are merged. A station that is not finite is replaced by the first position seen; the
/// orbit radius is kept at least 1.2 turn radii (speed^2 / max lateral acceleration) and at most 50 km.
pub fn add(state: &mut AirState, body: EntityId, side: SideId, group: Option<GroupId>, station: DVec2, radius_m: f64, catalogue: &Catalogue, kind: MunitionIdx) {
    let i = match state.aircraft.binary_search_by_key(&body, |a| a.body) { Ok(_) => return, Err(i) => i };
    let def = catalogue.munitions.get_index(kind.index()).map(|(_, d)| d);
    let mut payload: Vec<(MunitionIdx, u8)> = Vec::new();
    if let Some(def) = def {
        for (id, n) in &def.payload {
            let Some(idx) = catalogue.munition_idx(id) else { continue };
            if *n == 0 { continue; }
            match payload.iter_mut().find(|(m, _)| *m == idx) { Some((_, c)) => *c = c.saturating_add(*n), None => payload.push((idx, *n)) }
        }
    }
    let spec = def.map(AirSpec::of).unwrap_or_default();
    let min_r = 1.2 * spec.turn_radius_m();
    let default_r = def.map_or(5_000.0, |d| if d.range_m.is_finite() && d.range_m > 0.0 { (0.5 * d.range_m).clamp(1_000.0, 20_000.0) } else { 5_000.0 });
    let radius_m = if radius_m.is_finite() && radius_m > 0.0 { radius_m } else { default_r }.max(min_r).min(MAX_ORBIT_M);
    let ok = station.x.is_finite() && station.y.is_finite();
    let mem = AirMemory { station_unset: !ok, ..Default::default() };
    state.aircraft.insert(i, Aircraft { body, side, group, task: AirTask::Patrol, station: if ok { station } else { DVec2::ZERO }, radius_m, payload_left: payload, next_decision_tick: 0, mem });
}

// ---------------------------------------------------------------------------------------------------------
// Constants (play choices; the spec fixes only the body's sphere, hit points and RCS)
// ---------------------------------------------------------------------------------------------------------

const TICKS_PER_S: u64 = sim_core::time::TICK_HZ as u64;
/// Decision cadence: 0.5 s, jittered +-15%.
const DECISION_TICKS: u64 = TICKS_PER_S / 2;
const DRAW_JITTER: u64 = 1;
const DRAW_BREAK: u64 = 2;
/// Tracks within this distance of the station's circle are prosecuted, metres; 1.5x for a task under way.
const PROSECUTE_M: f64 = 25_000.0;
const MAX_ORBIT_M: f64 = 50_000.0;
/// Depth charges are dropped within this distance of the target, metres.
const DEPTH_CHARGE_RELEASE_M: f64 = 400.0;
/// Launch point as a fraction of release range.
const LAUNCH_POINT_FRACTION: f64 = 0.9;
const RELEASE_GAP_TICKS: u64 = TICKS_PER_S;
const INGRESS_MAX_S: u64 = 600;
const EGRESS_MIN_S: u64 = 45;
const EGRESS_MAX_S: u64 = 120;
const EGRESS_CLEAR_M: f64 = 15_000.0;
const EGRESS_LEG_M: f64 = 10_000.0;
/// Home: closer than this, the aircraft circles; circle radius floor, metres.
const HOME_ARRIVE_M: f64 = 1_500.0;
const HOME_ORBIT_M: f64 = 2_000.0;
/// Bingo: endurance left below `BINGO_FACTOR` x flight time home + `BINGO_RESERVE_S`.
const BINGO_FACTOR: f64 = 1.25;
const BINGO_RESERVE_S: f64 = 60.0;
/// Break turn: a weapon track predicted to pass within `BREAK_CPA_M` in the next `BREAK_HORIZON_S`.
const BREAK_CPA_M: f64 = 500.0;
const BREAK_HORIZON_S: f64 = 20.0;
const BREAK_S: u64 = 4;
/// Dead reckoning limit for a track, s.
const DR_MAX_S: f64 = 60.0;

// ---------------------------------------------------------------------------------------------------------
// Core
// ---------------------------------------------------------------------------------------------------------

/// The body as the aircraft knows itself.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Me { pos: DVec3, vel: DVec3, hp: u8, remaining: f64 }

#[derive(Clone, Copy, Debug, PartialEq)]
struct AirSpec { speed_mps: f64, max_accel: f64, alt_m: f64, endurance_s: f64, hp_max: u8 }
impl Default for AirSpec { fn default() -> Self { AirSpec { speed_mps: 150.0, max_accel: 30.0, alt_m: 1_000.0, endurance_s: 0.0, hp_max: 6 } } }
impl AirSpec {
    fn of(d: &content::part::weapons::MunitionDef) -> AirSpec {
        let base = AirSpec::default();
        AirSpec {
            speed_mps: pos_or(d.speed_mps, base.speed_mps),
            max_accel: pos_or(d.max_accel, base.max_accel),
            alt_m: if d.cruise_alt_m.is_finite() && d.cruise_alt_m > 0.0 { d.cruise_alt_m } else { base.alt_m },
            endurance_s: if d.endurance_s.is_finite() && d.endurance_s > 0.0 { d.endurance_s } else { 0.0 },
            hp_max: d.hp,
        }
    }
    fn turn_radius_m(&self) -> f64 { self.speed_mps * self.speed_mps / self.max_accel }
}

/// A weapon an aircraft can release.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Weapon { kind: MunitionKind, release_m: f64, salvo: u8 }

fn weapon_spec(catalogue: &Catalogue, idx: MunitionIdx, fraction: f64) -> Option<Weapon> {
    let (_, d) = catalogue.munitions.get_index(idx.index())?;
    let (release_m, salvo) = match d.kind {
        MunitionKind::AntiShipMissile => (d.range_m * fraction, 2),
        MunitionKind::Torpedo => (d.range_m * fraction, 1),
        MunitionKind::DepthCharge => (DEPTH_CHARGE_RELEASE_M, 3),
        _ => return None,
    };
    (release_m.is_finite() && release_m > 0.0).then_some(Weapon { kind: d.kind, release_m, salvo })
}

fn suits(kind: MunitionKind, domain: Domain) -> bool {
    match domain {
        Domain::Surface => matches!(kind, MunitionKind::AntiShipMissile | MunitionKind::Torpedo),
        Domain::Subsurface => matches!(kind, MunitionKind::Torpedo | MunitionKind::DepthCharge),
        Domain::Air | Domain::Weapon => false,
    }
}

struct Env<'a> {
    now: u64,
    reaction: u64,
    weapon: &'a dyn Fn(MunitionIdx) -> Option<Weapon>,
    mates: &'a [(GroupId, TrackId)],
    break_draw: f64,
}

fn pos_or(x: f64, d: f64) -> f64 { if x.is_finite() && x > 0.0 { x } else { d } }
fn clamp01(x: f64) -> f64 { if x.is_finite() { x.clamp(0.0, 1.0) } else { 0.5 } }
fn xz(v: DVec3) -> DVec2 { DVec2::new(v.x, v.z) }
fn finite2(v: DVec2) -> bool { v.x.is_finite() && v.y.is_finite() }
fn at_alt(p: DVec2, alt: f64) -> DVec3 { DVec3::new(p.x, alt, p.y) }
fn attack_track(t: AirTask) -> Option<TrackId> { match t { AirTask::Ingress(id) | AirTask::Attack(id) => Some(id), _ => None } }

/// Where a track is now: dead-reckoned from its last update at constant velocity, at most 60 s.
fn track_pos(t: &crate::tracks::Track, now: u64) -> Option<DVec2> {
    let vel = if finite2(xz(t.vel)) { xz(t.vel) } else { DVec2::ZERO };
    let dt = (now.saturating_sub(t.last_update_tick) as f64 / TICKS_PER_S as f64).min(DR_MAX_S);
    let p = xz(t.pos) + vel * dt;
    finite2(p).then_some(p)
}

/// The best weapon left for a domain (longest release range, then lowest index) and its payload row.
fn best_weapon(ac: &Aircraft, domain: Domain, env: &Env<'_>) -> Option<(usize, MunitionIdx, Weapon)> {
    let mut best: Option<(usize, MunitionIdx, Weapon)> = None;
    for (i, (idx, n)) in ac.payload_left.iter().enumerate() {
        if *n == 0 { continue; }
        let Some(w) = (env.weapon)(*idx) else { continue };
        if !suits(w.kind, domain) { continue; }
        if best.is_none_or(|(_, bi, bw)| w.release_m > bw.release_m || (w.release_m == bw.release_m && *idx < bi)) { best = Some((i, *idx, w)); }
    }
    best
}

fn has_any_weapon(ac: &Aircraft, env: &Env<'_>) -> bool { best_weapon(ac, Domain::Surface, env).is_some() || best_weapon(ac, Domain::Subsurface, env).is_some() }

/// A track the aircraft may keep prosecuting: live, Hostile, a surface or subsurface contact, within the leash.
fn valid_target<'p>(ac: &Aircraft, picture: Option<&'p SidePicture>, id: TrackId, leash: f64, env: &Env<'_>) -> Option<(&'p crate::tracks::Track, DVec2)> {
    let t = picture?.get(id)?;
    if !t.is_live() || t.stance != Stance::Hostile || !matches!(t.domain, Domain::Surface | Domain::Subsurface) { return None; }
    let p = track_pos(t, env.now)?;
    (p.distance(ac.station) <= ac.radius_m + leash).then_some((t, p))
}

/// Pick a new target from the picture.
fn pick_target(ac: &Aircraft, me: &Me, picture: Option<&SidePicture>, env: &Env<'_>) -> Option<TrackId> {
    let pic = picture?;
    let mut best: Option<(f64, TrackId)> = None;
    for t in &pic.tracks {
        if t.bearing_only.is_some() || t.created_tick.saturating_add(env.reaction) > env.now { continue; }
        let Some((t, p)) = valid_target(ac, picture, t.id, PROSECUTE_M, env) else { continue };
        if best_weapon(ac, t.domain, env).is_none() { continue; }
        let mates = ac.group.map_or(0, |g| env.mates.iter().filter(|(mg, mt)| *mg == g && *mt == t.id).count());
        let score = p.distance(xz(me.pos)) / (1.0 + mates as f64);
        if best.is_none_or(|(bs, bid)| score < bs || (score == bs && t.id < bid)) { best = Some((score, t.id)); }
    }
    best.map(|(_, id)| id)
}

/// A weapon track that will pass close to the aircraft soon, from the side picture.
fn break_threat(me: &Me, picture: Option<&SidePicture>, now: u64) -> Option<DVec2> {
    let pic = picture?;
    let here = xz(me.pos);
    let my_v = xz(me.vel);
    let mut best: Option<(f64, DVec2)> = None;
    for t in &pic.tracks {
        if !t.is_live() || t.domain != Domain::Weapon || matches!(t.stance, Stance::Friend | Stance::Neutral) { continue; }
        let Some(p) = track_pos(t, now) else { continue };
        let rel = p - here;
        let rv = xz(t.vel) - if finite2(my_v) { my_v } else { DVec2::ZERO };
        let vv = rv.length_squared();
        if !(vv.is_finite() && vv > 1.0) { continue; }
        let tc = -rel.dot(rv) / vv;
        if !(0.0..=BREAK_HORIZON_S).contains(&tc) { continue; }
        let miss = (rel + rv * tc).length();
        if miss <= BREAK_CPA_M && best.is_none_or(|(bt, _)| tc < bt) { best = Some((tc, p)); }
    }
    best.map(|(_, p)| p)
}

fn set_task(ac: &mut Aircraft, task: AirTask, now: u64) { if ac.task != task { ac.task = task; ac.mem.task_tick = now; } }

fn orbit(centre: DVec2, radius_m: f64, alt: f64) -> Steer { Steer::Orbit { centre: at_alt(centre, alt), radius_m } }

/// One decision for one aircraft: update the task and return its steering demand.
fn decide(ac: &mut Aircraft, me: &Me, spec: &AirSpec, picture: Option<&SidePicture>, env: &Env<'_>, releases: &mut Vec<Release>) -> Steer {
    let now = env.now;
    let here = if finite2(xz(me.pos)) { xz(me.pos) } else { ac.station };
    if ac.mem.station_unset { ac.station = here; ac.mem.station_unset = false; }
    let home = *ac.mem.home.get_or_insert(here);
    let alt = spec.alt_m;

    // Defensive break turn across the threat's line of sight.
    let threat = break_threat(me, picture, now);
    if threat.is_some() && now >= ac.mem.break_until_tick {
        ac.mem.break_until_tick = now + BREAK_S * TICKS_PER_S;
        ac.mem.break_sign = if env.break_draw < 0.5 { 1 } else { -1 };
    }
    if now < ac.mem.break_until_tick {
        // Across the line of sight to the threat; with the threat gone from the picture, across the velocity.
        let fwd = xz(me.vel);
        let los = match threat { Some(p) => (p - here).normalize_or_zero(), None if finite2(fwd) => fwd.normalize_or_zero(), None => DVec2::ZERO };
        let sign = if ac.mem.break_sign < 0 { -1.0 } else { 1.0 };
        let perp = DVec2::new(-los.y, los.x) * sign;
        let perp = if perp.length_squared() > 0.0 { perp } else { DVec2::new(sign, 0.0) };
        return Steer::Accel(DVec3::new(perp.x, 0.0, perp.y) * spec.max_accel);
    }

    // Bingo fuel or badly hurt: home.
    let bingo = spec.endurance_s > 0.0 && me.remaining.is_finite() && me.remaining < BINGO_FACTOR * here.distance(home) / spec.speed_mps + BINGO_RESERVE_S;
    let hurt = spec.hp_max > 1 && (me.hp as u16) * 2 <= spec.hp_max as u16;
    if bingo || hurt { set_task(ac, AirTask::ReturnToBase, now); }

    // Up to three task transitions in one decision (patrol -> ingress -> attack).
    for _ in 0..3 {
        match ac.task {
            AirTask::ReturnToBase => {
                return if here.distance(home) > HOME_ARRIVE_M { Steer::Waypoint(at_alt(home, alt)) } else { orbit(home, HOME_ORBIT_M.max(1.2 * spec.turn_radius_m()), alt) };
            }
            AirTask::Patrol => {
                // An aircraft with nothing it can release still patrols: its sensors feed the side picture.
                if let Some(id) = pick_target(ac, me, picture, env) { set_task(ac, AirTask::Ingress(id), now); continue; }
                return orbit(ac.station, ac.radius_m, alt);
            }
            AirTask::Ingress(id) => {
                let Some((t, tp)) = valid_target(ac, picture, id, 1.5 * PROSECUTE_M, env) else { set_task(ac, AirTask::Patrol, now); continue };
                let Some((_, _, w)) = best_weapon(ac, t.domain, env) else { set_task(ac, AirTask::Patrol, now); continue };
                if now.saturating_sub(ac.mem.task_tick) > INGRESS_MAX_S * TICKS_PER_S { set_task(ac, AirTask::Patrol, now); return orbit(ac.station, ac.radius_m, alt); }
                let d = here.distance(tp);
                let good = t.grade >= TrackGrade::Coarse;
                if d <= w.release_m && good {
                    ac.mem.salvo_left = w.salvo;
                    set_task(ac, AirTask::Attack(id), now);
                    continue;
                }
                if !good && d <= 0.5 * w.release_m {
                    // Too coarse to shoot: circle at half release range while the sensors refine the track.
                    return orbit(tp, (0.5 * w.release_m).max(1.2 * spec.turn_radius_m()), alt);
                }
                let from = (here - tp).normalize_or_zero();
                let from = if from.length_squared() > 0.0 { from } else { DVec2::new(0.0, 1.0) };
                return Steer::Waypoint(at_alt(tp + from * (LAUNCH_POINT_FRACTION * w.release_m), alt));
            }
            AirTask::Attack(id) => {
                let target = valid_target(ac, picture, id, 1.5 * PROSECUTE_M, env);
                let Some((t, tp)) = target else { begin_egress(ac, None, now); continue };
                let Some((row, idx, w)) = best_weapon(ac, t.domain, env) else { begin_egress(ac, Some(tp), now); continue };
                let d = here.distance(tp);
                if d > 1.3 * w.release_m { set_task(ac, AirTask::Ingress(id), now); continue; }
                let ready = ac.mem.last_release_tick.is_none_or(|l| now >= l + RELEASE_GAP_TICKS);
                if ready && ac.mem.salvo_left > 0 && d <= 1.1 * w.release_m && t.grade >= TrackGrade::Coarse {
                    releases.push(Release { aircraft: ac.body, munition: idx, track: id });
                    ac.payload_left[row].1 -= 1;
                    ac.mem.salvo_left -= 1;
                    ac.mem.last_release_tick = Some(now);
                }
                if ac.mem.salvo_left == 0 || best_weapon(ac, t.domain, env).is_none() { begin_egress(ac, Some(tp), now); continue; }
                return Steer::Waypoint(at_alt(tp, alt));
            }
            AirTask::Egress => {
                let from = ac.mem.egress_from.unwrap_or(here);
                let elapsed = now.saturating_sub(ac.mem.task_tick);
                let clear = elapsed >= EGRESS_MIN_S * TICKS_PER_S && here.distance(from) >= EGRESS_CLEAR_M;
                if clear || elapsed >= EGRESS_MAX_S * TICKS_PER_S {
                    ac.mem.egress_from = None;
                    set_task(ac, if has_any_weapon(ac, env) { AirTask::Patrol } else { AirTask::ReturnToBase }, now);
                    continue;
                }
                let mut away = (here - from).normalize_or_zero();
                if away.length_squared() == 0.0 { away = (home - here).normalize_or_zero(); }
                if away.length_squared() == 0.0 { away = DVec2::new(0.0, 1.0); }
                return Steer::Waypoint(at_alt(here + away * EGRESS_LEG_M, alt));
            }
        }
    }
    orbit(ac.station, ac.radius_m, alt)
}

fn begin_egress(ac: &mut Aircraft, from: Option<DVec2>, now: u64) {
    ac.mem.salvo_left = 0;
    ac.mem.egress_from = from.or(ac.mem.egress_from);
    set_task(ac, AirTask::Egress, now);
}

// ---------------------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::commander::tests::track;

    const ASM: MunitionIdx = MunitionIdx(1);
    const TORP: MunitionIdx = MunitionIdx(2);
    const SAM: MunitionIdx = MunitionIdx(3);

    fn weapons(idx: MunitionIdx) -> Option<Weapon> {
        match idx { ASM => Some(Weapon { kind: MunitionKind::AntiShipMissile, release_m: 24_000.0, salvo: 2 }), TORP => Some(Weapon { kind: MunitionKind::Torpedo, release_m: 8_000.0, salvo: 1 }), _ => None }
    }
    fn aircraft(payload: Vec<(MunitionIdx, u8)>) -> Aircraft {
        Aircraft { body: EntityId::from_u64(40), side: SideId(1), group: Some(GroupId(3)), task: AirTask::Patrol, station: DVec2::ZERO, radius_m: 5_000.0, payload_left: payload, next_decision_tick: 0, mem: AirMemory::default() }
    }
    fn spec() -> AirSpec { AirSpec { speed_mps: 200.0, max_accel: 40.0, alt_m: 1_000.0, endurance_s: 3_600.0, hp_max: 6 } }
    fn me_at(p: DVec2) -> Me { Me { pos: DVec3::new(p.x, 1_000.0, p.y), vel: DVec3::new(0.0, 0.0, -200.0), hp: 6, remaining: 3_000.0 } }
    fn picture(tracks: Vec<crate::tracks::Track>) -> SidePicture { SidePicture { side: SideId(1), tracks, next_id: 100 } }
    fn run(ac: &mut Aircraft, me: &Me, pic: &SidePicture, now: u64, rel: &mut Vec<Release>) -> Steer {
        let env = Env { now, reaction: 120, weapon: &weapons, mates: &[], break_draw: 0.3 };
        decide(ac, me, &spec(), Some(pic), &env, rel)
    }

    #[test]
    fn patrols_then_strikes_a_hostile_and_egresses() {
        let mut ac = aircraft(vec![(SAM, 2), (ASM, 2)]);
        let mut rel = Vec::new();
        let ship = track(5, DVec2::new(0.0, -28_000.0), Stance::Hostile, Domain::Surface);
        let pic = picture(vec![ship]);
        // Too soon after the track appeared (reaction 2 s): keeps orbiting.
        let s = run(&mut ac, &me_at(DVec2::ZERO), &pic, 60, &mut rel);
        assert!(matches!(s, Steer::Orbit { .. }));
        assert_eq!(ac.task, AirTask::Patrol);
        // Ingress to a launch point 90% of release range from the target, on the line toward the aircraft.
        let s = run(&mut ac, &me_at(DVec2::ZERO), &pic, 200, &mut rel);
        assert_eq!(ac.task, AirTask::Ingress(TrackId(5)));
        match s { Steer::Waypoint(p) => assert!((DVec2::new(p.x, p.z) - DVec2::new(0.0, -28_000.0 + 0.9 * 24_000.0)).length() < 1e-6), o => panic!("{o:?}") }
        assert!(rel.is_empty());
        // Inside release range: one missile now, the second a second later, then egress.
        let pos = DVec2::new(0.0, -17_000.0);
        run(&mut ac, &me_at(pos), &pic, 300, &mut rel);
        assert_eq!(ac.task, AirTask::Attack(TrackId(5)));
        assert_eq!(rel, vec![Release { aircraft: ac.body, munition: ASM, track: TrackId(5) }]);
        run(&mut ac, &me_at(pos), &pic, 330, &mut rel);
        assert_eq!(rel.len(), 1, "one release a second");
        let s = run(&mut ac, &me_at(pos), &pic, 360, &mut rel);
        assert_eq!(rel.len(), 2);
        assert_eq!(ac.task, AirTask::Egress);
        assert_eq!(ac.payload_left, vec![(SAM, 2), (ASM, 0)]);
        match s { Steer::Waypoint(p) => assert!(p.z > pos.y, "flies away from the target"), o => panic!("{o:?}") }
        // Out of anti-ship weapons: home after the egress leg.
        let away = DVec2::new(0.0, 0.0);
        let s = run(&mut ac, &me_at(away), &pic, 360 + 50 * TICKS_PER_S, &mut rel);
        assert_eq!(ac.task, AirTask::ReturnToBase);
        assert!(matches!(s, Steer::Orbit { .. }), "home is where it was first seen");
    }

    #[test]
    fn holds_fire_on_coarse_tracks_and_drops_lost_ones() {
        let mut ac = aircraft(vec![(ASM, 2)]);
        let mut rel = Vec::new();
        let mut ship = track(5, DVec2::new(0.0, -10_000.0), Stance::Hostile, Domain::Surface);
        ship.grade = TrackGrade::Bearing;
        let pic = picture(vec![ship.clone()]);
        let s = run(&mut ac, &me_at(DVec2::ZERO), &pic, 200, &mut rel);
        assert!(rel.is_empty());
        assert_eq!(ac.task, AirTask::Ingress(TrackId(5)));
        assert!(matches!(s, Steer::Orbit { radius_m, .. } if (radius_m - 12_000.0).abs() < 1e-6));
        // Track turns neutral: back to patrol.
        ship.stance = Stance::Neutral;
        let pic = picture(vec![ship]);
        run(&mut ac, &me_at(DVec2::ZERO), &pic, 260, &mut rel);
        assert_eq!(ac.task, AirTask::Patrol);
        // No weapon for a surface target (SAMs only): never ingresses.
        let mut ac = aircraft(vec![(SAM, 4)]);
        let pic = picture(vec![track(5, DVec2::new(0.0, -10_000.0), Stance::Hostile, Domain::Surface)]);
        run(&mut ac, &me_at(DVec2::ZERO), &pic, 200, &mut rel);
        assert_eq!(ac.task, AirTask::Patrol);
        // Beyond the leash: ignored.
        let mut ac = aircraft(vec![(ASM, 2)]);
        let pic = picture(vec![track(5, DVec2::new(0.0, -40_000.0), Stance::Hostile, Domain::Surface)]);
        run(&mut ac, &me_at(DVec2::ZERO), &pic, 200, &mut rel);
        assert_eq!(ac.task, AirTask::Patrol);
    }

    #[test]
    fn breaks_against_an_inbound_weapon_and_goes_home_at_bingo() {
        let mut ac = aircraft(vec![(ASM, 2)]);
        let mut rel = Vec::new();
        let mut sam = track(9, DVec2::new(0.0, -3_000.0), Stance::Hostile, Domain::Weapon);
        sam.vel = DVec3::new(0.0, 0.0, 600.0);
        let pic = picture(vec![sam]);
        let s = run(&mut ac, &me_at(DVec2::ZERO), &pic, 200, &mut rel);
        match s { Steer::Accel(a) => { assert!((a.length() - 40.0).abs() < 1e-9); assert!(a.z.abs() < 1e-9, "across the line of sight") } o => panic!("{o:?}") }
        // The break lasts 4 s even after the track goes.
        let empty = picture(vec![]);
        assert!(matches!(run(&mut ac, &me_at(DVec2::ZERO), &empty, 200 + 3 * TICKS_PER_S, &mut rel), Steer::Accel(_)));
        assert!(matches!(run(&mut ac, &me_at(DVec2::ZERO), &empty, 200 + 5 * TICKS_PER_S, &mut rel), Steer::Orbit { .. }));
        // Bingo: 30 km from home with 100 s left at 200 m/s needs 1.25 * 150 + 60 s.
        let mut low = me_at(DVec2::new(30_000.0, 0.0));
        low.remaining = 100.0;
        let s = run(&mut ac, &low, &empty, 1_000, &mut rel);
        assert_eq!(ac.task, AirTask::ReturnToBase);
        assert!(matches!(s, Steer::Waypoint(p) if p.x.abs() < 1e-9 && p.z.abs() < 1e-9));
    }

    #[test]
    fn add_keeps_order_merges_payload_and_sanitises() {
        let cat_text = |id: &str, kind: &str, payload: &str| format!("(id: \"{id}\", name: \"n\", kind: {kind}, domain: Air, speed_mps: 200.0, range_m: 30000.0, max_accel: 40.0, cruise_alt_m: 1000.0, fuse: Contact, he_b: 6, he_r_cells: 2, seeker: None, radius_m: 5.0, hp: 6, rcs_m2: 5.0, mass_kg: 9000.0, cost: 50, payload: [{payload}], endurance_s: 3600.0)");
        let mut cat = Catalogue::default();
        for (id, kind, payload) in [("a.asm", "AntiShipMissile", ""), ("b.strike", "Aircraft", "(\"a.asm\", 2), (\"x.none\", 3), (\"a.asm\", 1), (\"a.asm\", 0)")] {
            let d: content::part::weapons::MunitionDef = content::data::parse_ron(&cat_text(id, kind, payload), "t").unwrap();
            cat.munitions.insert(d.id.clone(), d);
        }
        let kind = cat.munition_idx(&MunitionId::new("b.strike")).unwrap();
        let mut st = AirState::default();
        add(&mut st, EntityId::from_u64(9), SideId(1), None, DVec2::new(1.0, 2.0), 300.0, &cat, kind);
        add(&mut st, EntityId::from_u64(4), SideId(1), None, DVec2::new(f64::NAN, 0.0), f64::NAN, &cat, kind);
        add(&mut st, EntityId::from_u64(9), SideId(1), None, DVec2::ZERO, 1.0, &cat, kind);
        add(&mut st, EntityId::from_u64(6), SideId(1), None, DVec2::ZERO, 1.0, &cat, MunitionIdx(77));
        assert_eq!(st.aircraft.iter().map(|a| a.body.to_u64()).collect::<Vec<_>>(), vec![4, 6, 9]);
        let a9 = &st.aircraft[2];
        assert_eq!(a9.payload_left, vec![(MunitionIdx(0), 3)]);
        assert!((a9.radius_m - 1.2 * 200.0 * 200.0 / 40.0).abs() < 1e-9, "at least 1.2 turn radii");
        let a4 = &st.aircraft[0];
        assert!(a4.mem.station_unset && a4.radius_m == 15_000.0);
        assert!(st.aircraft[1].payload_left.is_empty());
        // The unset station becomes the first position seen.
        let mut a = st.aircraft[0].clone();
        let env = Env { now: 0, reaction: 0, weapon: &weapons, mates: &[], break_draw: 0.3 };
        decide(&mut a, &me_at(DVec2::new(500.0, 700.0)), &spec(), None, &env, &mut Vec::new());
        assert_eq!(a.station, DVec2::new(500.0, 700.0));
    }

    #[test]
    fn group_mates_concentrate_on_one_target() {
        let mut ac = aircraft(vec![(ASM, 2)]);
        let pic = picture(vec![track(5, DVec2::new(0.0, -10_000.0), Stance::Hostile, Domain::Surface), track(6, DVec2::new(0.0, 14_000.0), Stance::Hostile, Domain::Surface)]);
        let mates = [(GroupId(3), TrackId(6))];
        let env = Env { now: 300, reaction: 120, weapon: &weapons, mates: &mates, break_draw: 0.3 };
        decide(&mut ac, &me_at(DVec2::ZERO), &spec(), Some(&pic), &env, &mut Vec::new());
        assert_eq!(attack_track(ac.task), Some(TrackId(6)));
    }
}
