//! Unit `sys-offboard`. Offboard units flown by officers: launched drones at first release, the stock
//! helicopter from Act 2 (Phase::Act2), and the RHIB used by station actions.
//! DESIGN.md L549-553 (offboard: extra sensor geometry; a bearing-only contact needs a second sensor at an
//! angle to fix range), L551 (helicopter flown by an officer, not player-built), L786-797 (RHIB).
//!
//! Offboard bodies are sim-core small bodies. This module decides where they fly (`Steer::Orbit`,
//! `Steer::Waypoint`), when they return (endurance, recall, damage) and manages bay state. Their sensors are
//! ordinary `SensorInstance`s feeding the side picture over the datalink.
//!
//! Decisions where the spec is silent:
//! - Units are created lazily, one per drone bay and helicopter deck of the vessel, by [`step`] (and by
//!   [`order`] for a bay not seen yet); stock and recharge live in sim-core's `PartState`. RHIB davits are
//!   driven by `crate::specops` (station actions), so no unit is kept for them.
//! - A launched airframe is bound to its unit by scanning the body table for a live body whose launcher and
//!   launcher slot are this vessel and bay. The world spawns the body for a `Launch` before the next step.
//! - The officer launches an idle airframe by itself when a Suspect or Hostile bearing-only track exists, and
//!   tasks a flying one to cross-fix the nearest such track until the track is graded Fine, else to orbit
//!   ahead on the planned route. A player order holds until the player recalls or launches again, or the
//!   shadowed track is gone.
//! - The cross-fix station lies half the estimated range down the bearing and half the range off it on the
//!   side the airframe is already on: a 45 degree cut at the target that keeps the airframe out of reach.
//! - Every station is leashed inside 90% of the type's combat radius (`MunitionDef::range_m`) from the ship.
//!   An airframe returns when its endurance left falls to the flight time home plus 60 s, when it is
//!   damaged, or when it is recalled; within 300 m of the ship it asks to be recovered.
use crate::prelude::*;
use crate::protocol::command::OffboardOrder;
use crate::tracks::SidePicture;
use content::part::weapons::MunitionDef;
use content::part::PartFunction;
use sim_core::munitions::{BodyPhase, Bodies, Steer};
use sim_core::parts::PartState;
use sim_core::vessel::VesselStatus;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum OffboardTask { #[default] Aboard, Launching, Orbit { centre: DVec2, radius_m: f64 }, Shadow(TrackId), Waypoint(DVec2), /// Officer default: hold a station abeam of the nearest bearing-only track to cross-fix it.
    CrossFix(TrackId), Returning, Lost }

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct OffboardUnit { pub bay: PartSlot, pub body: Option<EntityId>, pub task: OffboardTask, pub endurance_left_s: f64, /// True while the player's order is in force (the officer does not re-task it).
    pub player_tasked: bool }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OffboardState { pub units: Vec<OffboardUnit> }

/// What the step asks of the world.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OffboardOut { Launch { bay: PartSlot }, Recover { bay: PartSlot }, Steer { body: EntityId, steer: Steer } }

/// Orbit radius limits for player orders, metres.
pub const MIN_ORBIT_M: f64 = 200.0;
pub const MAX_ORBIT_M: f64 = 20_000.0;
/// Default orbit radius ahead on the route, around a reached waypoint, around a shadowed track and on a
/// cross-fix station, metres.
pub const ROUTE_ORBIT_M: f64 = 2_000.0;
pub const WAYPOINT_ORBIT_M: f64 = 1_000.0;
pub const SHADOW_ORBIT_M: f64 = 3_000.0;
pub const CROSSFIX_ORBIT_M: f64 = 1_000.0;
/// How far ahead on the route the default orbit sits, metres (before the leash).
pub const AHEAD_M: f64 = 8_000.0;
/// Arrival distance at a waypoint and recovery distance from the ship, metres.
pub const ARRIVE_M: f64 = 300.0;
pub const RECOVER_M: f64 = 300.0;
/// Fuel reserve kept for the flight home, seconds.
pub const RESERVE_S: f64 = 60.0;
/// Stations stay inside this fraction of the type's combat radius.
pub const LEASH_FRACTION: f64 = 0.9;

const DT: f64 = 1.0 / 30.0;

fn new_unit(bay: PartSlot) -> OffboardUnit { OffboardUnit { bay, body: None, task: OffboardTask::Aboard, endurance_left_s: 0.0, player_tasked: false } }

fn unit_index(state: &mut OffboardState, bay: PartSlot) -> usize {
    match state.units.binary_search_by_key(&bay, |u| u.bay) {
        Ok(i) => i,
        Err(i) => { state.units.insert(i, new_unit(bay)); i }
    }
}

fn flying_task(t: OffboardTask) -> bool { matches!(t, OffboardTask::Launching | OffboardTask::Orbit { .. } | OffboardTask::Shadow(_) | OffboardTask::Waypoint(_) | OffboardTask::CrossFix(_)) }

/// Apply a player order to a bay. Unit: `sys-offboard`.
///
/// Returns false for an order that cannot apply: a launch while the airframe is out, a non-finite point,
/// a shadow of no track. A recall of an airframe aboard keeps it aboard (the officer will not launch it).
pub fn order(state: &mut OffboardState, bay: PartSlot, order: OffboardOrder) -> bool {
    state.units.sort_by_key(|u| u.bay);
    let valid = match order {
        OffboardOrder::Orbit { centre, .. } | OffboardOrder::Waypoint(centre) => centre.is_finite(),
        OffboardOrder::Shadow(t) => !t.is_none(),
        _ => true,
    };
    if !valid { return false; }
    let i = unit_index(state, bay);
    let u = &mut state.units[i];
    let out = u.body.is_some() || flying_task(u.task);
    match order {
        OffboardOrder::Launch => {
            if out || u.task == OffboardTask::Returning { return false; }
            u.task = OffboardTask::Launching;
            u.player_tasked = false;
        }
        OffboardOrder::Recall => {
            if out { u.task = OffboardTask::Returning; }
            u.player_tasked = true;
        }
        OffboardOrder::Orbit { centre, radius_m } => {
            let r = if radius_m.is_finite() { radius_m.clamp(MIN_ORBIT_M, MAX_ORBIT_M) } else { ROUTE_ORBIT_M };
            u.task = OffboardTask::Orbit { centre, radius_m: r };
            u.player_tasked = true;
        }
        OffboardOrder::Shadow(t) => { u.task = OffboardTask::Shadow(t); u.player_tasked = true; }
        OffboardOrder::Waypoint(p) => { u.task = OffboardTask::Waypoint(p); u.player_tasked = true; }
    }
    true
}

/// One bay as the step sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BayView {
    pub slot: PartSlot,
    pub kind: MunitionIdx,
    /// Airframes aboard ready or recharging, and ticks until the next is ready.
    pub aboard: u8,
    pub recharge_ticks: u32,
    /// Bay works and has power.
    pub works: bool,
}
impl BayView { fn can_launch(&self) -> bool { self.works && self.aboard > 0 && self.recharge_ticks == 0 } }

/// One airframe in the air as the step sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BodyView { pub entity: EntityId, pub pos: DVec3, pub hp: u8, pub remaining: f64, pub spawn_tick: u64 }

/// Everything about the ship the step reads.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ShipView { pub pos: DVec3, pub forward: DVec2, pub alive: bool }

fn body_view(b: &Bodies, row: usize) -> Option<BodyView> {
    let entity = *b.entity.get(row)?;
    if b.phase.get(row).is_none_or(|&p| p == BodyPhase::Dead) { return None; }
    let hp = b.hp.get(row).copied().unwrap_or(0);
    if hp == 0 { return None; }
    Some(BodyView { entity, pos: b.pos.get(row).copied().unwrap_or(DVec3::NAN), hp, remaining: b.remaining.get(row).copied().unwrap_or(f64::NAN), spawn_tick: b.spawn_tick.get(row).copied().unwrap_or(0) })
}

/// One 30 Hz step for a ship's offboard units: officer default tasking (cross-fix the nearest bearing-only
/// Suspect or Hostile track, else orbit ahead on the planned route), endurance and recall, steering.
/// Unit: `sys-offboard`.
pub fn step(state: &mut OffboardState, vessel: EntityId, world: &World, picture: &SidePicture, route: &[DVec2], ctx: &SimCtx<'_>, out: &mut Vec<OffboardOut>) {
    let cat = ctx.catalogue;
    let bodies = world.bodies();
    let Some(v) = world.vessel(vessel) else {
        // The ship is gone: airframes circle where they are; nothing launches or lands.
        for u in &mut state.units {
            match u.body.and_then(|b| bodies.row_of(b)).and_then(|r| body_view(bodies, r)) {
                Some(bv) if bv.pos.is_finite() => out.push(OffboardOut::Steer { body: bv.entity, steer: Steer::Orbit { centre: bv.pos, radius_m: WAYPOINT_ORBIT_M } }),
                _ => { u.body = None; u.task = OffboardTask::Lost; }
            }
        }
        return;
    };
    let mut bays: Vec<BayView> = Vec::new();
    for p in &v.parts {
        let Some((_, def)) = cat.parts.get_index(p.def.index()) else { continue };
        let kind = match &def.function {
            PartFunction::DroneBay { drone, .. } => cat.munition_idx(drone),
            PartFunction::HeliDeck { helicopter } => cat.munition_idx(helicopter),
            _ => continue,
        };
        let Some(kind) = kind else { continue };
        let (aboard, recharge_ticks) = match p.state { PartState::DroneBay { aboard, recharge_ticks } => (aboard, recharge_ticks), PartState::HeliDeck { aboard } => (aboard as u8, 0), _ => (0, 0) };
        let works = crate::datalink::part_command(&p.links, p.works(), &ctx.tuning.networks) != crate::datalink::PartCommand::Dead;
        bays.push(BayView { slot: p.slot, kind, aboard, recharge_ticks, works });
    }
    bays.sort_by_key(|b| b.slot);
    bays.dedup_by_key(|b| b.slot);
    let rot = v.kin.pose.rot;
    let fwd3 = if rot.is_finite() { rot * DVec3::X } else { DVec3::X };
    let fwd = DVec2::new(fwd3.x, fwd3.z);
    let ship = ShipView { pos: v.kin.pose.pos, forward: if fwd.length_squared() > 1e-9 { fwd.normalize() } else { DVec2::X }, alive: matches!(v.status, VesselStatus::Active | VesselStatus::Disabled) };
    // Candidate airframes of this vessel per bay, newest first.
    let mut launched: Vec<(PartSlot, BodyView)> = Vec::new();
    for row in 0..bodies.len() {
        if bodies.launcher.get(row) != Some(&vessel) { continue; }
        let Some(&slot) = bodies.launcher_slot.get(row) else { continue };
        if let Some(bv) = body_view(bodies, row) { launched.push((slot, bv)); }
    }
    let live = |e: EntityId| bodies.row_of(e).and_then(|r| body_view(bodies, r));
    step_core(state, &bays, ship, &launched, &live, picture, route, ctx, &|k| cat.munitions.get_index(k.index()).map(|(_, d)| d), out);
}

/// World-free step over prepared views (see [`step`]).
#[allow(clippy::too_many_arguments)]
pub(crate) fn step_core<'a>(state: &mut OffboardState, bays: &[BayView], ship: ShipView, launched: &[(PartSlot, BodyView)], live: &dyn Fn(EntityId) -> Option<BodyView>,
    picture: &SidePicture, route: &[DVec2], ctx: &SimCtx<'_>, def_of: &dyn Fn(MunitionIdx) -> Option<&'a MunitionDef>, out: &mut Vec<OffboardOut>) {
    let tick = ctx.tick.0;
    // Units follow the bays: one per bay, ascending; units of vanished bays go once nothing is in the air.
    state.units.sort_by_key(|u| u.bay);
    state.units.dedup_by_key(|u| u.bay);
    state.units.retain(|u| bays.iter().any(|b| b.slot == u.bay) || u.body.is_some());
    for b in bays { unit_index(state, b.slot); }
    let bound: BTreeSet<EntityId> = state.units.iter().filter_map(|u| u.body).collect();
    let mut claimed = bound.clone();
    let crossfix = crossfix_target(picture, ship.pos, tick, None);

    for u in state.units.iter_mut() {
        let bay = bays.iter().find(|b| b.slot == u.bay).copied();
        let def = bay.and_then(|b| def_of(b.kind));
        let endurance = def.map(|d| d.endurance_s).filter(|e| e.is_finite() && *e > 0.0).unwrap_or(0.0);

        // Lost airframes: shot down, or recovered while returning.
        let mut body = u.body.and_then(live);
        if u.body.is_some() && body.is_none() {
            u.body = None;
            u.task = if u.task == OffboardTask::Returning { OffboardTask::Aboard } else { OffboardTask::Lost };
            if u.task == OffboardTask::Lost { u.player_tasked = false; }
        }
        // Bind a freshly launched airframe.
        if body.is_none() && flying_task(u.task) {
            let pick = launched.iter().filter(|(s, b)| *s == u.bay && !claimed.contains(&b.entity))
                .max_by(|a, b| a.1.spawn_tick.cmp(&b.1.spawn_tick).then(b.1.entity.cmp(&a.1.entity))).map(|x| x.1);
            if let Some(b) = pick { claimed.insert(b.entity); u.body = Some(b.entity); body = Some(b); }
        }

        let Some(b) = body else {
            // Aboard (or lost, or waiting to launch).
            let airframe = bay.is_some_and(|b| b.aboard > 0);
            if u.task == OffboardTask::Lost && airframe { u.task = OffboardTask::Aboard; }
            if matches!(u.task, OffboardTask::Aboard | OffboardTask::Returning) { u.task = OffboardTask::Aboard; u.endurance_left_s = endurance; }
            let wants = flying_task(u.task) || (u.task == OffboardTask::Aboard && !u.player_tasked && crossfix.is_some());
            if wants && ship.alive {
                match bay {
                    Some(bv) if bv.can_launch() && def.is_some() => {
                        out.push(OffboardOut::Launch { bay: u.bay });
                        if u.task == OffboardTask::Aboard { u.task = OffboardTask::Launching; }
                        u.endurance_left_s = endurance;
                    }
                    Some(bv) if bv.aboard > 0 && bv.works => {} // recharging: wait
                    _ => if flying_task(u.task) { u.task = if airframe { OffboardTask::Aboard } else { OffboardTask::Lost }; u.player_tasked = false; },
                }
            }
            continue;
        };

        let Some(def) = def else {
            // Bay destroyed or unknown type: bring the airframe home.
            u.task = OffboardTask::Returning;
            steer_home(u, b, ship, 0.0, out);
            continue;
        };
        u.endurance_left_s = if b.remaining.is_finite() && b.remaining >= 0.0 && endurance > 0.0 { b.remaining.min(endurance) } else { (u.endurance_left_s - DT).max(0.0) };
        let alt = if def.cruise_alt_m.is_finite() { def.cruise_alt_m } else { 0.0 };
        let speed = if def.speed_mps.is_finite() && def.speed_mps > 1.0 { def.speed_mps } else { 1.0 };
        let leash = if def.range_m.is_finite() && def.range_m > 0.0 { LEASH_FRACTION * def.range_m } else { f64::INFINITY };
        let pos2 = DVec2::new(b.pos.x, b.pos.z);
        let home2 = DVec2::new(ship.pos.x, ship.pos.z);
        let dist_home = if pos2.is_finite() && home2.is_finite() { pos2.distance(home2) } else { 0.0 };

        // Return: endurance, damage, a lost ship.
        let low = endurance > 0.0 && u.endurance_left_s <= dist_home / speed + RESERVE_S;
        if u.task != OffboardTask::Returning && (low || b.hp < def.hp || !ship.alive) { u.task = OffboardTask::Returning; }

        // Player tasks that no longer apply, and officer tasking.
        if let OffboardTask::Shadow(t) = u.task { if picture.get(t).is_none_or(|t| !t.is_live()) { u.player_tasked = false; u.task = OffboardTask::Launching; } }
        if let OffboardTask::Waypoint(p) = u.task { if pos2.is_finite() && pos2.distance(p) <= ARRIVE_M { u.task = OffboardTask::Orbit { centre: p, radius_m: WAYPOINT_ORBIT_M }; } }
        if u.task != OffboardTask::Returning && !u.player_tasked {
            let keep = match u.task { OffboardTask::CrossFix(t) => crossfix_target(picture, ship.pos, tick, Some(t)) == Some(t), _ => false };
            if !keep {
                u.task = match crossfix_target(picture, ship.pos, tick, None) {
                    Some(t) => OffboardTask::CrossFix(t),
                    None => OffboardTask::Orbit { centre: ahead_point(ship, route, AHEAD_M.min(0.6 * leash)), radius_m: ROUTE_ORBIT_M },
                };
            }
        }

        let at = |p: DVec2| DVec3::new(p.x, alt, p.y);
        let steer = match u.task {
            OffboardTask::Orbit { centre, radius_m } => Steer::Orbit { centre: at(leash_point(centre, home2, leash)), radius_m },
            OffboardTask::Waypoint(p) => Steer::Waypoint(at(leash_point(p, home2, leash))),
            OffboardTask::Shadow(t) => {
                let c = picture.get(t).map(|t| { let p = t.predict(tick); DVec2::new(p.x, p.z) }).unwrap_or(home2);
                Steer::Orbit { centre: at(leash_point(c, home2, leash)), radius_m: SHADOW_ORBIT_M }
            }
            OffboardTask::CrossFix(t) => {
                let c = picture.get(t).map(|t| crossfix_station(t, home2, pos2, tick)).unwrap_or(home2);
                Steer::Orbit { centre: at(leash_point(c, home2, leash)), radius_m: CROSSFIX_ORBIT_M }
            }
            _ => { u.task = OffboardTask::Returning; steer_home(u, b, ship, alt, out); continue; }
        };
        out.push(OffboardOut::Steer { body: b.entity, steer });
    }
}

fn steer_home(u: &OffboardUnit, b: BodyView, ship: ShipView, alt: f64, out: &mut Vec<OffboardOut>) {
    let home = if ship.pos.is_finite() { DVec3::new(ship.pos.x, alt, ship.pos.z) } else { DVec3::new(0.0, alt, 0.0) };
    out.push(OffboardOut::Steer { body: b.entity, steer: Steer::Waypoint(home) });
    let d = DVec2::new(b.pos.x - home.x, b.pos.z - home.z).length();
    if ship.alive && d.is_finite() && d <= RECOVER_M { out.push(OffboardOut::Recover { bay: u.bay }); }
}

/// The nearest live Suspect or Hostile bearing-only track (not a weapon) to the ship. With `keep`, that track
/// stays the target while it is live, hostile-looking and not yet graded Fine.
fn crossfix_target(picture: &SidePicture, own: DVec3, tick: u64, keep: Option<TrackId>) -> Option<TrackId> {
    if let Some(k) = keep {
        return picture.get(k).filter(|t| t.is_live() && t.stance >= Stance::Suspect && t.grade < TrackGrade::Fine).map(|t| t.id);
    }
    let own2 = DVec2::new(own.x, own.z);
    let mut best: Option<(f64, TrackId)> = None;
    for t in &picture.tracks {
        if !t.is_live() || t.bearing_only.is_none() || t.stance < Stance::Suspect || t.domain == Domain::Weapon { continue; }
        let p = t.predict(tick);
        let d = DVec2::new(p.x, p.z).distance(own2);
        if !d.is_finite() { continue; }
        if best.is_none_or(|(bd, bid)| d < bd || (d == bd && t.id < bid)) { best = Some((d, t.id)); }
    }
    best.map(|b| b.1)
}

/// Cross-fix station for a track (see the module notes).
pub(crate) fn crossfix_station(t: &crate::tracks::Track, own: DVec2, airframe: DVec2, tick: u64) -> DVec2 {
    let p = t.predict(tick);
    let target = DVec2::new(p.x, p.z);
    let d = target - own;
    let r = d.length();
    if !(r.is_finite() && r > 1.0) { return own; }
    let u = d / r;
    let n = DVec2::new(-u.y, u.x);
    let side = if airframe.is_finite() && (airframe - own).dot(n) < 0.0 { -1.0 } else { 1.0 };
    own + u * (0.5 * r) + n * (0.5 * r * side)
}

/// Keep a station inside the leash around the ship.
fn leash_point(p: DVec2, home: DVec2, leash: f64) -> DVec2 {
    if !p.is_finite() { return home; }
    let d = p - home;
    let r = d.length();
    if r <= leash || !r.is_finite() || r == 0.0 { p } else { home + d * (leash / r) }
}

/// A point `ahead_m` along the planned route from the leg nearest the ship, or along the bow without a route.
pub(crate) fn ahead_point(ship: ShipView, route: &[DVec2], ahead_m: f64) -> DVec2 {
    let own = DVec2::new(ship.pos.x, ship.pos.z);
    let own = if own.is_finite() { own } else { DVec2::ZERO };
    let ahead = if ahead_m.is_finite() && ahead_m > 0.0 { ahead_m } else { 0.0 };
    let pts: Vec<DVec2> = route.iter().copied().filter(|p| p.is_finite()).collect();
    if pts.is_empty() { return own + ship.forward * ahead; }
    // Projection on the nearest leg (the first leg runs from the ship to the first point).
    let mut legs: Vec<(DVec2, DVec2)> = vec![(own, pts[0])];
    legs.extend(pts.windows(2).map(|w| (w[0], w[1])));
    let (mut best_leg, mut best_t, mut best_d) = (0usize, 0.0f64, f64::INFINITY);
    for (i, (a, b)) in legs.iter().enumerate() {
        let ab = *b - *a;
        let l2 = ab.length_squared();
        let t = if l2 > 0.0 { ((own - *a).dot(ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
        let d = (*a + ab * t).distance(own);
        if d < best_d { best_d = d; best_leg = i; best_t = t; }
    }
    let mut left = ahead;
    let (a, b) = legs[best_leg];
    let mut at = a + (b - a) * best_t;
    let mut i = best_leg;
    loop {
        let end = legs[i].1;
        let seg = end.distance(at);
        if seg >= left { return if seg > 0.0 { at + (end - at) * (left / seg) } else { at }; }
        left -= seg;
        at = end;
        i += 1;
        if i >= legs.len() { return at; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::snapshot::SourceMask;
    use crate::tracks::{BearingState, Track};
    use content::campaign::DifficultySliders;
    use content::enums::TimeOfDay;
    use content::part::weapons::{Fuse, MunitionKind, SeekerKind};
    use sim_core::Tick;

    struct Env { cat: Catalogue, tuning: TuningTable, weather: WeatherState, diff: DifficultySliders }
    impl Env {
        fn new() -> Self { Env { cat: Catalogue::default(), tuning: TuningTable::default(), weather: WeatherState::default(), diff: DifficultySliders::NORMAL } }
        fn ctx(&self, tick: u64) -> SimCtx<'_> {
            SimCtx { seed: 1, tick: Tick(tick), catalogue: &self.cat, tuning: &self.tuning, weather: &self.weather, time_of_day: TimeOfDay::Day, layer_depth_m: 50.0, difficulty: &self.diff }
        }
    }

    fn drone() -> MunitionDef {
        MunitionDef {
            id: content::ids::MunitionId::new("drone"), name: content::ids::TextKey::new("d"), kind: MunitionKind::Drone, phase: Default::default(), domain: Domain::Air,
            speed_mps: 40.0, range_m: 30_000.0, max_accel: 5.0, cruise_alt_m: 300.0, fuse: Fuse::None, he_b: 0, he_r_cells: 0, seeker: SeekerKind::None,
            seeker_r_ref_m: 0.0, seeker_fov_half_deg: 0.0, pn_n: 0.0, wire_guided: false, radius_m: 1.0, hp: 2, rcs_m2: 0.5, noise: 0.0, mass_kg: 50.0, cost: 10,
            sensors: vec![], payload: vec![], endurance_s: 1_800.0, model: Default::default(), icon: Default::default(),
        }
    }
    fn bay(aboard: u8) -> BayView { BayView { slot: PartSlot(4), kind: MunitionIdx(0), aboard, recharge_ticks: 0, works: true } }
    fn ship() -> ShipView { ShipView { pos: DVec3::ZERO, forward: DVec2::X, alive: true } }
    fn e(i: u32) -> EntityId { EntityId { index: i, generation: 0 } }
    fn bearing_track(id: u32, at: DVec2) -> Track {
        Track {
            id: TrackId(id), side: SideId(0), domain: Domain::Surface, truth: EntityId::NONE, pos: DVec3::new(at.x, 0.0, at.y), vel: DVec3::ZERO, r0_m: 2_000.0, sigma_v: 5.0,
            bearing_only: Some(BearingState { origin: DVec3::ZERO, bearing_rad: 0.0, bearing_rate: 0.0, sigma_bearing_rad: 0.03, range_m: at.length(), sigma_range_m: 5_000.0, sensors: 1 }),
            created_tick: 0, last_update_tick: 0, last_lock_tick: None, dropped_tick: None, grade: TrackGrade::Bearing, sources: SourceMask::ESM, trend: Default::default(),
            rung: IdRung::Unknown, rung_reason: RungReason::None, candidates: Default::default(), class: None, history: vec![], rcs_m2: 0.0, consistent_ticks: 0,
            emitters: vec![], id_override: None, stance: Stance::Suspect, stance_reason: StanceReason::Default, stance_override: None, fired_on_us: false, inbound: false, engaged_by: 0, emitting: false, fit: None, out_of_action: false, struck: false,
        }
    }

    fn run(state: &mut OffboardState, bays: &[BayView], launched: &[(PartSlot, BodyView)], live: &[BodyView], pic: &SidePicture, route: &[DVec2], tick: u64) -> Vec<OffboardOut> {
        let env = Env::new();
        let d = drone();
        let mut out = Vec::new();
        let lv = |x: EntityId| live.iter().find(|b| b.entity == x).copied();
        step_core(state, bays, ship(), launched, &lv, pic, route, &env.ctx(tick), &|_| Some(&d), &mut out);
        out
    }

    #[test]
    fn orders_validate() {
        let mut s = OffboardState::default();
        assert!(!order(&mut s, PartSlot(4), OffboardOrder::Waypoint(DVec2::NAN)));
        assert!(!order(&mut s, PartSlot(4), OffboardOrder::Shadow(TrackId::NONE)));
        assert!(order(&mut s, PartSlot(4), OffboardOrder::Launch));
        assert_eq!(s.units[0].task, OffboardTask::Launching);
        assert!(!order(&mut s, PartSlot(4), OffboardOrder::Launch), "already launching");
        assert!(order(&mut s, PartSlot(4), OffboardOrder::Orbit { centre: DVec2::new(1.0, 2.0), radius_m: 1.0e9 }));
        assert_eq!(s.units[0].task, OffboardTask::Orbit { centre: DVec2::new(1.0, 2.0), radius_m: MAX_ORBIT_M });
        assert!(s.units[0].player_tasked);
        assert!(order(&mut s, PartSlot(4), OffboardOrder::Recall));
        assert_eq!(s.units[0].task, OffboardTask::Returning);
    }

    #[test]
    fn officer_launches_for_a_bearing_only_contact_and_cross_fixes() {
        let mut s = OffboardState::default();
        let pic_empty = SidePicture::default();
        // Nothing to cross-fix: stays aboard.
        assert!(run(&mut s, &[bay(2)], &[], &[], &pic_empty, &[], 0).is_empty());
        assert_eq!(s.units.len(), 1);
        assert_eq!(s.units[0].endurance_left_s, 1_800.0);
        let pic = SidePicture { side: SideId(0), tracks: vec![bearing_track(5, DVec2::new(20_000.0, 0.0))], next_id: 6 };
        assert_eq!(run(&mut s, &[bay(2)], &[], &[], &pic, &[], 1), vec![OffboardOut::Launch { bay: PartSlot(4) }]);
        assert_eq!(s.units[0].task, OffboardTask::Launching);
        // The world spawned the airframe: bind it and fly to the cross-fix station (10 km down, 10 km off).
        let b = BodyView { entity: e(30), pos: DVec3::new(0.0, 300.0, -10.0), hp: 2, remaining: 1_790.0, spawn_tick: 1 };
        let out = run(&mut s, &[bay(1)], &[(PartSlot(4), b)], &[b], &pic, &[], 2);
        assert_eq!(s.units[0].body, Some(e(30)));
        assert_eq!(s.units[0].task, OffboardTask::CrossFix(TrackId(5)));
        let OffboardOut::Steer { body, steer: Steer::Orbit { centre, radius_m } } = out[0] else { panic!("{out:?}") };
        assert_eq!((body, radius_m), (e(30), CROSSFIX_ORBIT_M));
        assert!((centre - DVec3::new(10_000.0, 300.0, -10_000.0)).length() < 1e-6, "{centre}");
        assert_eq!(s.units[0].endurance_left_s, 1_790.0);
    }

    #[test]
    fn returns_on_low_endurance_and_damage_then_recovers() {
        let mut s = OffboardState { units: vec![OffboardUnit { bay: PartSlot(4), body: Some(e(30)), task: OffboardTask::Orbit { centre: DVec2::ZERO, radius_m: 1_000.0 }, endurance_left_s: 100.0, player_tasked: true }] };
        let pic = SidePicture::default();
        // 4 km out at 40 m/s: 100 s home + 60 s reserve > 120 s left.
        let b = BodyView { entity: e(30), pos: DVec3::new(4_000.0, 300.0, 0.0), hp: 2, remaining: 120.0, spawn_tick: 1 };
        let out = run(&mut s, &[bay(0)], &[], &[b], &pic, &[], 5);
        assert_eq!(s.units[0].task, OffboardTask::Returning);
        assert_eq!(out, vec![OffboardOut::Steer { body: e(30), steer: Steer::Waypoint(DVec3::new(0.0, 300.0, 0.0)) }]);
        let near = BodyView { pos: DVec3::new(100.0, 300.0, 0.0), ..b };
        let out = run(&mut s, &[bay(0)], &[], &[near], &pic, &[], 6);
        assert!(out.contains(&OffboardOut::Recover { bay: PartSlot(4) }));
        // Recovered: the body is gone while returning, the unit is aboard again.
        run(&mut s, &[bay(1)], &[], &[], &pic, &[], 7);
        assert_eq!((s.units[0].task, s.units[0].body), (OffboardTask::Aboard, None));
        // Damage sends it home; a body lost while on task is Lost (no airframe left).
        let mut d = OffboardState { units: vec![OffboardUnit { bay: PartSlot(4), body: Some(e(31)), task: OffboardTask::Waypoint(DVec2::new(5_000.0, 0.0)), endurance_left_s: 1_000.0, player_tasked: true }] };
        let hit = BodyView { entity: e(31), pos: DVec3::new(1_000.0, 300.0, 0.0), hp: 1, remaining: 1_000.0, spawn_tick: 1 };
        run(&mut d, &[bay(0)], &[], &[hit], &pic, &[], 8);
        assert_eq!(d.units[0].task, OffboardTask::Returning);
        d.units[0].task = OffboardTask::Orbit { centre: DVec2::ZERO, radius_m: 500.0 };
        run(&mut d, &[bay(0)], &[], &[], &pic, &[], 9);
        assert_eq!(d.units[0].task, OffboardTask::Lost);
    }

    #[test]
    fn default_orbit_ahead_on_route_and_leash() {
        let sh = ship();
        assert_eq!(ahead_point(sh, &[], 8_000.0), DVec2::new(8_000.0, 0.0));
        let route = [DVec2::new(0.0, 5_000.0), DVec2::new(10_000.0, 5_000.0)];
        let p = ahead_point(sh, &route, 8_000.0);
        assert!((p - DVec2::new(3_000.0, 5_000.0)).length() < 1e-9, "{p}");
        assert_eq!(ahead_point(sh, &route, 1.0e9), DVec2::new(10_000.0, 5_000.0));
        assert_eq!(leash_point(DVec2::new(50_000.0, 0.0), DVec2::ZERO, 27_000.0), DVec2::new(27_000.0, 0.0));
        assert_eq!(leash_point(DVec2::NAN, DVec2::ONE, 10.0), DVec2::ONE);
        // A flying officer-tasked airframe with no contacts orbits ahead.
        let mut s = OffboardState { units: vec![OffboardUnit { bay: PartSlot(4), body: Some(e(30)), task: OffboardTask::Launching, endurance_left_s: 1_000.0, player_tasked: false }] };
        let b = BodyView { entity: e(30), pos: DVec3::new(100.0, 300.0, 0.0), hp: 2, remaining: 1_000.0, spawn_tick: 1 };
        let out = run(&mut s, &[bay(0)], &[], &[b], &SidePicture::default(), &[], 3);
        assert_eq!(out, vec![OffboardOut::Steer { body: e(30), steer: Steer::Orbit { centre: DVec3::new(8_000.0, 300.0, 0.0), radius_m: ROUTE_ORBIT_M } }]);
        // A recalled airframe aboard is never launched by the officer.
        let mut r = OffboardState::default();
        order(&mut r, PartSlot(4), OffboardOrder::Recall);
        let pic = SidePicture { side: SideId(0), tracks: vec![bearing_track(5, DVec2::new(20_000.0, 0.0))], next_id: 6 };
        assert!(run(&mut r, &[bay(2)], &[], &[], &pic, &[], 1).is_empty());
    }
}
