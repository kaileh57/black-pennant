//! `World::step`: the 13 phases of the tick, in the order the module docs fix (unit `sim-core-world`).
//!
//! Decisions the phase list leaves open:
//! - Rates. A `Full` hull runs every system every tick. A `Full30` or `Kinematic` hull runs parts, networks,
//!   mass, forces, crew, flooding and fire on every second tick (staggered by slot) with a step of two ticks;
//!   between them a `Full30` body sits out the physics step, which `physics` makes up on its next one. A
//!   kinematic hull's pose still advances every tick. Status, damage and structure run every tick for all.
//! - Lost hulls and wrecks keep flooding, burning and floating; their networks and crew stop.
//! - Ended bodies, expired decoys and wrecks past their rules are named in this tick's events and freed in
//!   phase 1 of the next tick, so the layers above can still read them while they handle the events.
//! - Magazine cook-offs found by the fire step blast in the same tick (the serial merge of phase 10), ahead
//!   of the structure pass.
//! - Structure work resumes at `split_cursor` and then runs in ascending slot order, wrapping, so a hull
//!   whose job ran out of budget goes first next tick and no hull starves. A job that yields a wreck piece
//!   ends the pass for the tick: carving is the one unbudgeted cost.
//! - Colliders, buoyancy blocks and dry mass rebuild for a dirty hull once its structure work is done and at
//!   most every [`REBUILD_TICKS`]; a split rebuilds at once.
//! - `Collision` and `Grounded` are queued when a contact starts and whenever it does damage.
//! - A lost hull follows the wreck rules from the tick it was lost (20 m under, 600 s, 16 kept), because deep
//!   water has no seabed collider to stop it.
//! - A loss within [`COOKOFF_LOSS_TICKS`] of the hull's own magazine cook-off is reported as
//!   `LossCause::CookOff`, whichever of the two end states it met. `VesselLost::by` is the last entity that
//!   damaged the hull from outside.
//! - Bucket changes that are not hits (a damage-control repair lowering the buckets of a part's cells) go out
//!   as `CellsDamaged` with `DamageCause::Fire` and no attacker: the stream must carry every change, the cause
//!   list has no entry for a repair, and `Fire` is the cause no weapon and no cell damage otherwise uses.
//! - Persistent damage applied at spawn queues no `CellsDamaged`: it is part of the state a spawned hull
//!   starts with, and its feeds start empty.
use super::*;
use crate::armour::{CellDamage, HitOutcome, COOKOFF_B};
use crate::crew::JobDone;
use crate::damage::{DamageCause, DamageOutcome};
use crate::entity::EntityKind;
use crate::fire::FireOutcome;
use crate::flooding::FloodOutcome;
use crate::hydro::{BuoyancyBlocks, HydroForces};
use crate::munitions::{BodyOutcome, BodyPhase};
use crate::parts::{LaunchAim, PartState, SpawnRequest};
use crate::physics::BodyHandle;
use crate::projectiles::{HullTarget, ProjectileStepOutput, SphereTarget};
use crate::time::{DISTANT_HZ, DT, SPLIT_WORK_BUDGET, TICK_HZ};
use crate::vessel::{LossCause, VesselStatus};
use content::cell::CELL_M;
use content::part::weapons::{BandDef, CalibreBand, MunitionKind};
use content::part::PartFunction;
use rayon::prelude::*;

/// Depth-charge set depth limits in metres (L595).
const SET_DEPTH_M: (f64, f64) = (10.0, 150.0);

fn at<T>(c: &Column<T>, slot: u32) -> Option<&T> { c.slots.get(slot as usize).and_then(Option::as_ref) }
fn at_mut<T>(c: &mut Column<T>, slot: u32) -> Option<&mut T> { c.slots.get_mut(slot as usize).and_then(Option::as_mut) }

/// The live vessel behind an id (columns are indexed by slot only, so the generation is checked here).
fn hull(vessels: &mut Column<Vessel>, id: EntityId) -> Option<&mut Vessel> { vessels.get_mut(id).filter(|v| v.id == id) }

/// Step of a hull's systems this tick, or `None` on a tick a 30 Hz hull sits out.
fn due_dt(fidelity: Fidelity, slot: u32, tick: Tick) -> Option<f64> {
    match fidelity {
        Fidelity::Full => Some(DT),
        Fidelity::Full30 | Fidelity::Kinematic => tick.is_due(DISTANT_HZ, slot as u64).then_some(DT * (TICK_HZ / DISTANT_HZ.max(1)) as f64),
    }
}

/// A hull's part by slot. A hull's list is indexed by slot; a wreck piece keeps only the parts it carried,
/// under their original slots and in that order, so its list is searched.
fn part_of(v: &Vessel, slot: PartSlot) -> Option<&crate::parts::PartRuntime> {
    match v.parts.get(slot.index()) {
        Some(p) if p.slot == slot => Some(p),
        _ => v.parts.binary_search_by(|p| p.slot.0.cmp(&slot.0)).ok().and_then(|i| v.parts.get(i)),
    }
}

fn afloat(status: VesselStatus) -> bool { matches!(status, VesselStatus::Active | VesselStatus::Disabled) }

fn band_of(cat: &Catalogue, band: CalibreBand) -> BandDef { cat.bands.iter().find(|b| b.band == band).copied().unwrap_or_else(|| BandDef::spec_default(band)) }

fn cause_of(kind: MunitionKind) -> DamageCause {
    match kind {
        MunitionKind::Torpedo => DamageCause::Torpedo,
        MunitionKind::Mine => DamageCause::Mine,
        MunitionKind::DepthCharge => DamageCause::DepthCharge,
        MunitionKind::Rhib => DamageCause::Collision,
        _ => DamageCause::Missile,
    }
}

/// Queue everything one damage application changed, in a fixed order: the cell stream, then parts, breaches,
/// conduits and the fire it lit.
fn emit_damage(events: &mut Vec<PhysEvent>, v: &Vessel, from: usize, outcome: &DamageOutcome, cause: DamageCause, attacker: EntityId) {
    if let Some(changes) = v.grid.changes.get(from..).filter(|c| !c.is_empty()) {
        events.push(PhysEvent::CellsDamaged { vessel: v.id, changes: changes.to_vec(), cause, attacker });
    }
    for &slot in &outcome.parts_changed {
        if let Some(p) = part_of(v, slot) { events.push(PhysEvent::PartDamaged { vessel: v.id, slot, level: p.level }); }
    }
    for &c in &outcome.breached {
        events.push(PhysEvent::Breach { vessel: v.id, compartment: c, below_waterline: v.compartments.get(c).is_some_and(|k| k.below_waterline) });
    }
    for &conduit in &outcome.conduits_cut { events.push(PhysEvent::ConduitCut { vessel: v.id, conduit }); }
    if let Some(c) = outcome.ignited { events.push(PhysEvent::FireStarted { vessel: v.id, compartment: c, spread: false }); }
}

fn doom(doomed: &mut Vec<EntityId>, id: EntityId) { if !id.is_none() && !doomed.contains(&id) { doomed.push(id); } }

#[derive(Default)]
struct PartsOut { ran: bool, spawns: Vec<SpawnRequest>, links: Vec<PartSlot> }

enum Motion { Idle, Forces(HydroForces), Kinematic(Kinematics) }

#[derive(Default)]
struct SystemsOut { ran: bool, jobs: Vec<JobDone>, flood: FloodOutcome, fire: FireOutcome, changes_from: usize }

pub(super) fn step(w: &mut World) {
    let cat_arc = w.config.catalogue.clone();
    let cat: &Catalogue = &cat_arc;
    let tuning = &cat.tuning;
    let tick = w.tick;
    let time_s = tick.seconds();
    w.work = WorkCounters::default();
    let mut lap = crate::spans::Lap::start();

    // ---- 1. Queued despawns, ascending slot ----
    let mut doomed = core::mem::take(&mut w.doomed);
    doomed.sort_unstable();
    doomed.dedup();
    for id in doomed { w.free_entity(id); }

    // ---- 2. Time and the sea of this tick ----
    if let Some(blend) = w.wave_blend {
        let (set, done) = blend.at(tick.0, &tuning.waves);
        w.waves = set;
        if done { w.wave_blend = None; }
    }
    // The context owns nothing of the world: waves and weather are copies and the terrain is lent for the tick.
    let waves = w.waves;
    let weather = w.weather;
    let terrain = core::mem::take(&mut w.terrain);
    let ctx = StepContext { seed: w.config.seed, tick, dt: DT, time_s, catalogue: cat, tuning, waves: &waves, weather: &weather, terrain: &terrain };
    let sea = |x: f64, z: f64| waves.height_at(x, z, time_s);

    // ---- 3. Parts and networks, per vessel ----
    let parts_out: Vec<(u32, PartsOut)> = {
        let run = |(slot, v): (u32, &mut Vessel)| -> (u32, PartsOut) {
            let Some(dt) = due_dt(v.fidelity, slot, tick) else { return (slot, PartsOut::default()) };
            let c = StepContext { dt, ..ctx };
            let links = if afloat(v.status) { crate::networks::solve_tick(&mut v.networks, &mut v.parts, cat, &tuning.networks, dt) } else { Vec::new() };
            let mut spawns = Vec::new();
            crate::parts::step_parts(v, &c, &mut spawns);
            (slot, PartsOut { ran: true, spawns, links })
        };
        let hulls = w.vessels.values_mut();
        if hulls.len() > 1 { hulls.into_par_iter().map(run).collect() } else { hulls.into_iter().map(run).collect() }
    };

    // ---- 4. Spawns in (vessel id, mount id) order ----
    let towed_out = w.decoys.kind.contains(&DecoyKind::TowedDecoy);
    for (slot, out) in parts_out {
        let Some(v) = at(&w.vessels, slot) else { continue };
        let (id, side) = (v.id, v.side);
        if out.ran { w.work.conduit_segments += v.networks.conduits.len() as u32; }
        for ps in out.links {
            if let Some(p) = part_of(v, ps) {
                w.events.push(PhysEvent::LinkChanged { vessel: id, slot: ps, power: p.links.power, fuel: p.links.fuel, data: p.links.data, shed: p.links.shed });
            }
        }
        let mut i = 0;
        while i < out.spawns.len() {
            match out.spawns[i] {
                SpawnRequest::Shell { slot: ps, .. } => {
                    // One run of rounds from one mount.
                    let mut j = i;
                    let mut fired = 0u32;
                    while let Some(SpawnRequest::Shell { slot: s, rounds, .. }) = out.spawns.get(j) {
                        if *s != ps { break; }
                        fired += (*rounds).max(1) as u32;
                        j += 1;
                    }
                    w.events.push(PhysEvent::MountFired { vessel: id, slot: ps, rounds: fired.min(u8::MAX as u32) as u8 });
                    let gun = part_of(v, ps).and_then(|p| cat.parts.get_index(p.def.index())).and_then(|(_, d)| match &d.function {
                        PartFunction::Gun(g) => Some((g.calibre_mm, g.band, g.point_defence)),
                        _ => None,
                    });
                    if let Some((calibre, band, pd)) = gun {
                        for req in &out.spawns[i..j] {
                            let SpawnRequest::Shell { origin, velocity, .. } = *req else { continue };
                            if let Some(spawn) = w.projectiles.spawn(tick.0, origin, velocity, calibre, band, id, ps, pd, w.projectile_cap) {
                                w.events.push(PhysEvent::ProjectileSpawned(spawn));
                            }
                        }
                    }
                    i = j;
                }
                SpawnRequest::Munition { slot: ps, munition, origin, velocity, aim } => {
                    i += 1;
                    if !(origin.is_finite() && velocity.is_finite()) { continue; }
                    let def = cat.munitions.get_index(munition.index()).map(|(_, d)| d);
                    // Where the launch points the body until guidance takes it (sim-systems, 30 Hz).
                    let steer = match aim {
                        LaunchAim::Point(p) if p.is_finite() && (p - origin).length_squared() > 1.0 => Some(Steer::Waypoint(p)),
                        LaunchAim::Entity(e) => locate_in(&w.entities, &w.vessels, &w.bodies, &w.decoys, e).map(|l| Steer::Waypoint(l.0)),
                        LaunchAim::Bearing { heading_rad: h } if h.is_finite() => {
                            let reach = def.map_or(10_000.0, |d| if d.range_m.is_finite() { d.range_m.max(1_000.0) } else { 10_000.0 });
                            Some(Steer::Waypoint(origin + DVec3::new(libm::sin(h), 0.0, -libm::cos(h)) * reach))
                        }
                        _ => None,
                    };
                    let body = w.entities.spawn(EntityKind::Body);
                    if body.is_none() { continue; }
                    w.bodies.insert(body, munition, side, origin, velocity, id, ps, tick.0, cat);
                    if let Some(r) = w.bodies.row_of(body) {
                        if let (Some(s), Some(cell)) = (steer, w.bodies.steer.get_mut(r)) { *cell = s; }
                        if let (LaunchAim::DepthM(d), Some(cell)) = (aim, w.bodies.set_depth_m.get_mut(r)) {
                            if d.is_finite() { *cell = d.clamp(SET_DEPTH_M.0, SET_DEPTH_M.1); }
                        }
                    }
                    w.events.push(PhysEvent::BodySpawned { body, kind: munition, side, launcher: id, slot: ps, pos: origin, vel: velocity });
                    if def.is_some_and(|d| d.kind == MunitionKind::Mine) { w.events.push(PhysEvent::MineLaid { mine: body, layer: id, pos: origin }); }
                }
                SpawnRequest::Decoy { kind, origin, .. } => {
                    i += 1;
                    if !origin.is_finite() { continue; }
                    let decoy = w.entities.spawn(EntityKind::Decoy);
                    if decoy.is_none() { continue; }
                    w.decoys.insert(decoy, kind, id, origin);
                    w.events.push(PhysEvent::DecoyDeployed { decoy, kind, owner: id, pos: origin });
                }
            }
        }
        // A towed decoy whose part stopped streaming (order, damage, power) is hauled in.
        if towed_out || w.decoys.kind.contains(&DecoyKind::TowedDecoy) {
            let streaming = v.parts.iter().filter(|p| matches!(p.state, PartState::Decoy { streamed: true, .. })).count();
            let n = w.decoys.len().min(w.decoys.kind.len()).min(w.decoys.owner.len());
            let out_now: Vec<EntityId> = (0..n).filter(|&r| w.decoys.kind[r] == DecoyKind::TowedDecoy && w.decoys.owner[r] == id && !w.doomed.contains(&w.decoys.entity[r])).map(|r| w.decoys.entity[r]).collect();
            for &decoy in out_now.iter().skip(streaming) {
                w.events.push(PhysEvent::DecoyExpired { decoy });
                doom(&mut w.doomed, decoy);
            }
        }
    }

    lap.mark(crate::spans::Span::Parts);
    // ---- 5. Forces, per vessel in parallel; applied serially ----
    // Far-hull fallback (L658): hulls beyond `far_distance_m` of every player hull sample coarser blocks.
    let far_cells = tuning.buoyancy.far_block_cells;
    if tuning.buoyancy.far_blocks_enabled && far_cells != tuning.buoyancy.block_cells && tuning.buoyancy.far_distance_m.is_finite() {
        let players: Vec<DVec3> = w.vessels.iter().filter(|(_, v)| v.player.is_some()).map(|(_, v)| v.kin.pose.pos).collect();
        let far2 = tuning.buoyancy.far_distance_m * tuning.buoyancy.far_distance_m;
        for (slot, v) in w.vessels.iter() {
            let Some(a) = at_mut(&mut w.aux, slot) else { continue };
            let far = !players.is_empty() && v.player.is_none() && players.iter().all(|p| (*p - v.kin.pose.pos).length_squared() > far2);
            if !far { a.far_blocks = None; } else if a.far_blocks.is_none() { a.far_blocks = Some(Arc::new(crate::hydro::build_blocks(&v.grid, &v.compartments, far_cells))); }
        }
    } else {
        for a in w.aux.slots.iter_mut().flatten() { a.far_blocks = None; }
    }
    let rho = if tuning.buoyancy.water_density.is_finite() && tuning.buoyancy.water_density > 0.0 { tuning.buoyancy.water_density } else { crate::hydro::SEA_WATER_DENSITY };
    let motions: Vec<(u32, Motion)> = {
        let aux = &w.aux;
        let run = |(slot, v): (u32, &mut Vessel)| -> (u32, Motion) {
            let due = due_dt(v.fidelity, slot, tick);
            if due.is_some() { v.mass = crate::mass::with_fluids(&v.dry_mass, &v.parts, &v.compartments, cat); }
            if v.fidelity == Fidelity::Kinematic { return (slot, Motion::Kinematic(crate::hydro::kinematic_step_with(v, &waves, time_s, DT, &tuning.buoyancy))); }
            if due.is_none() { return (slot, Motion::Idle); }
            let mut out = HydroForces::default();
            let far: Option<&BuoyancyBlocks> = at(aux, slot).and_then(|a| a.far_blocks.as_deref());
            match far {
                Some(blocks) => crate::hydro::buoyancy_on(blocks, &v.kin.pose, v.waterline_y, &waves, time_s, rho, &mut out),
                None => crate::hydro::buoyancy(v, &waves, time_s, &tuning.buoyancy, &mut out),
            }
            crate::hydro::drag(v, &waves, time_s, &tuning.buoyancy, &mut out);
            crate::hydro::propulsion(v, &waves, time_s, &mut out);
            crate::hydro::depth_keeping(v, &waves, time_s, &mut out);
            (slot, Motion::Forces(out))
        };
        let hulls = w.vessels.values_mut();
        if hulls.len() > 1 { hulls.into_par_iter().map(run).collect() } else { hulls.into_iter().map(run).collect() }
    };
    for (slot, motion) in motions {
        let Some(v) = at(&w.vessels, slot) else { continue };
        match motion {
            Motion::Idle => w.physics.set_enabled(v.body, false),
            Motion::Forces(h) => {
                w.physics.set_enabled(v.body, true);
                if let Some(a) = at_mut(&mut w.aux, slot) {
                    if a.pushed_mass != Some(v.mass) { w.physics.set_mass(v.body, &v.mass); a.pushed_mass = Some(v.mass); }
                }
                for f in &h.forces { w.physics.add_force_at(v.body, f.force, f.at); }
                w.physics.add_torque(v.body, h.torque);
                w.work.buoyancy_samples += h.samples;
            }
            Motion::Kinematic(kin) => {
                w.physics.set_enabled(v.body, true);
                w.physics.set_kinematics(v.body, &kin);
            }
        }
    }

    lap.mark(crate::spans::Span::Forces);
    // ---- 6. Rigid bodies; kinematics and bounds back; contacts ----
    let around: Vec<DVec3> = w.vessels.iter().map(|(_, v)| v.kin.pose.pos).collect();
    w.physics.set_terrain(&terrain, &around);
    let mut contacts = Vec::new();
    w.physics.step(DT, &mut contacts);
    let mut reset: Vec<(BodyHandle, Kinematics)> = Vec::new();
    let mut by_body: Vec<(BodyHandle, EntityId)> = Vec::with_capacity(around.len());
    for (slot, v) in w.vessels.values_mut() {
        let kin = w.physics.kinematics(v.body);
        if kin.pose.pos.is_finite() && kin.pose.rot.is_finite() && kin.pose.rot.length_squared() > 1e-12 && kin.lin_vel.is_finite() && kin.ang_vel.is_finite() {
            v.kin = Kinematics { pose: crate::math::Pose::new(kin.pose.pos, kin.pose.rot.normalize()), lin_vel: kin.lin_vel, ang_vel: kin.ang_vel };
        } else {
            // A body the solver lost: hold it where it last made sense, at rest, so no NaN reaches state.
            v.kin = Kinematics { pose: v.kin.pose, lin_vel: DVec3::ZERO, ang_vel: DVec3::ZERO };
            reset.push((v.body, v.kin));
        }
        refresh_bounds(v, at(&w.aux, slot).and_then(|a| a.extent));
        by_body.push((v.body, v.id));
        w.work.bodies += 1;
        w.work.cuboids += v.cuboids.len() as u32;
    }
    for (body, kin) in reset { w.physics.set_kinematics(body, &kin); }
    by_body.sort_unstable();
    let entity_of = |h: BodyHandle| by_body.binary_search_by(|p| p.0.cmp(&h)).ok().map(|i| by_body[i].1);
    let mut touching: Vec<(EntityId, EntityId)> = Vec::with_capacity(contacts.len());
    for c in &contacts {
        let Some(a) = entity_of(c.a) else { continue };
        let b = match c.b { Some(h) => match entity_of(h) { Some(e) => e, None => continue }, None => EntityId::NONE };
        let pair = if b.is_none() || a <= b { (a, b) } else { (b, a) };
        let fresh = w.touching.binary_search(&pair).is_err();
        touching.push(pair);
        // What each party takes is worked out before either is changed.
        let crush = |id: EntityId| -> Vec<CellDamage> { if id.is_none() { Vec::new() } else { w.vessels.get(id).map_or(Vec::new(), |v| crate::damage::collision_damage(v, c.point, c.impulse)) } };
        let (da, db) = (crush(pair.0), crush(pair.1));
        if fresh || !da.is_empty() || !db.is_empty() {
            w.events.push(if pair.1.is_none() { PhysEvent::Grounded { vessel: pair.0, point: c.point } } else { PhysEvent::Collision { a: pair.0, b: pair.1, point: c.point, impulse: c.impulse } });
        }
        for (me, other, cells) in [(pair.0, pair.1, da), (pair.1, pair.0, db)] {
            if cells.is_empty() { continue; }
            let Some(v) = hull(&mut w.vessels, me) else { continue };
            let cause = if other.is_none() { DamageCause::Grounding } else { DamageCause::Collision };
            let from = v.grid.changes.len();
            let outcome = crate::damage::apply_cells(v, &cells, cause, &ctx);
            w.work.damage_cells += cells.len() as u32;
            emit_damage(&mut w.events, v, from, &outcome, cause, other);
            if !other.is_none() { if let Some(a) = w.aux.get_mut(me) { a.last_attacker = other; } }
        }
    }
    touching.sort_unstable();
    touching.dedup();
    w.touching = touching;

    lap.mark(crate::spans::Span::Physics);
    // ---- 7. Small bodies and decoys ----
    let hulls: Vec<HullTarget> = w.vessels.iter().map(|(_, v)| HullTarget { vessel: v.id, bounds: v.bounds, pose: v.kin.pose }).collect();
    let grazes = core::mem::take(&mut w.grazes);
    let mut outcomes = Vec::new();
    w.bodies.step(&mut w.decoys, &ctx, &hulls, &grazes, &mut outcomes);
    let auto_band = band_of(cat, CalibreBand::Auto);
    for o in outcomes { resolve_body(w, &ctx, o, &grazes, &auto_band); }
    for decoy in w.decoys.expired(&tuning.decoys) {
        if !w.doomed.contains(&decoy) {
            w.events.push(PhysEvent::DecoyExpired { decoy });
            w.doomed.push(decoy);
        }
    }

    lap.mark(crate::spans::Span::Bodies);
    // ---- 8. Projectiles ----
    let rows = w.bodies.len().min(w.bodies.pos.len()).min(w.bodies.kind.len()).min(w.bodies.phase.len()).min(w.bodies.hp.len());
    let spheres: Vec<SphereTarget> = (0..rows).filter(|&r| w.bodies.phase[r] != BodyPhase::Dead && w.bodies.hp[r] > 0).filter_map(|r| {
        let def = cat.munitions.get_index(w.bodies.kind[r].index())?.1;
        (def.radius_m.is_finite() && def.radius_m > 0.0 && w.bodies.pos[r].is_finite()).then(|| SphereTarget { body: w.bodies.entity[r], centre: w.bodies.pos[r], radius: def.radius_m })
    }).collect();
    let mut shots = ProjectileStepOutput::default();
    w.projectiles.step(tick.0, &cat.bands, &hulls, &spheres, &w.vessels, &sea, &terrain, &tuning.projectiles, &mut shots);
    w.work.projectile_cells = shots.work_units;
    w.work.live_projectiles = w.projectiles.len() as u32;
    for s in &shots.splashes { w.events.push(PhysEvent::Splash { projectile: s.projectile, pos: s.pos, calibre_mm: s.calibre_mm, shooter: s.shooter }); }

    lap.mark(crate::spans::Span::Projectiles);
    // ---- 9. Shell hits in (tick, projectile id) order ----
    for hit in &shots.hits {
        let band = band_of(cat, hit.band);
        let Some(v) = hull(&mut w.vessels, hit.vessel) else { continue };
        let result = crate::armour::resolve_shell(&v.grid, hit, &band);
        w.work.damage_cells += result.work_units;
        w.events.push(PhysEvent::ShellImpact {
            projectile: hit.projectile, vessel: hit.vessel, cell: hit.first_cell, outcome: result.outcome.unwrap_or(HitOutcome::PassedThrough),
            world_point: hit.world_point, shooter: hit.shooter, burst: result.burst.is_some(),
        });
        let from = v.grid.changes.len();
        let outcome = crate::damage::apply_hit(v, &result, DamageCause::Shell, hit.shooter, &ctx);
        emit_damage(&mut w.events, v, from, &outcome, DamageCause::Shell, hit.shooter);
        if !hit.shooter.is_none() { if let Some(a) = w.aux.get_mut(hit.vessel) { a.last_attacker = hit.shooter; } }
    }
    w.grazes = core::mem::take(&mut shots.grazes);

    lap.mark(crate::spans::Span::Hits);
    // ---- 10. Crew, flooding and fire, per vessel ----
    let systems: Vec<(u32, SystemsOut)> = {
        let run = |(slot, v): (u32, &mut Vessel)| -> (u32, SystemsOut) {
            let Some(dt) = due_dt(v.fidelity, slot, tick) else { return (slot, SystemsOut::default()) };
            let c = StepContext { dt, ..ctx };
            let mut jobs = Vec::new();
            let changes_from = v.grid.changes.len();
            let teams = if afloat(v.status) {
                crate::crew::step_crew(v, &c, &mut jobs)
            } else {
                // No crew works a lost hull; its doors stay as they were left.
                let Vessel { compartments, parts, .. } = &mut *v;
                compartments.sync_doors(parts);
                Vec::new()
            };
            let flood = crate::flooding::step_flooding(v, &|x, z| waves.height_at(x, z, time_s), &c);
            let fire = crate::fire::step_fire(v, &teams, &c);
            (slot, SystemsOut { ran: true, jobs, flood, fire, changes_from })
        };
        let hulls = w.vessels.values_mut();
        if hulls.len() > 1 { hulls.into_par_iter().map(run).collect() } else { hulls.into_iter().map(run).collect() }
    };
    for (slot, out) in systems {
        if !out.ran { continue; }
        let Some(v) = at_mut(&mut w.vessels, slot) else { continue };
        let id = v.id;
        let attacker = at(&w.aux, slot).map_or(EntityId::NONE, |a| a.last_attacker);
        w.work.flood_openings += out.flood.work_units;
        w.work.compartments += v.compartments.compartments.len() as u32;
        for job in out.jobs { w.events.push(PhysEvent::JobDone { vessel: id, job }); }
        // A repair lowers the buckets of the part's cells: those changes join the replicated stream too.
        if let Some(changes) = v.grid.changes.get(out.changes_from..).filter(|c| !c.is_empty()) {
            w.events.push(PhysEvent::CellsDamaged { vessel: id, changes: changes.to_vec(), cause: DamageCause::Fire, attacker: EntityId::NONE });
        }
        for &c in &out.flood.crossed_half { w.events.push(PhysEvent::CompartmentHalfFlooded { vessel: id, compartment: c }); }
        for &m in &out.flood.magazines_flooded { w.events.push(PhysEvent::MagazineFlooded { vessel: id, magazine: m }); }
        if let Some(a) = at_mut(&mut w.aux, slot) {
            if v.crew.passengers_lost > a.passengers_reported {
                w.events.push(PhysEvent::PassengersLost { vessel: id, count: v.crew.passengers_lost - a.passengers_reported });
            }
            a.passengers_reported = v.crew.passengers_lost;
        }
        for &c in &out.fire.ignited { w.events.push(PhysEvent::FireStarted { vessel: id, compartment: c, spread: true }); }
        for &c in &out.fire.extinguished {
            let flooded = v.compartments.get(c).is_some_and(|k| k.fill_fraction() > tuning.flooding.flooded_fraction);
            w.events.push(PhysEvent::FireOut { vessel: id, compartment: c, flooded });
        }
        for &ps in &out.fire.parts_changed {
            if let Some(p) = part_of(v, ps) { w.events.push(PhysEvent::PartDamaged { vessel: id, slot: ps, level: p.level }); }
        }
        for &(magazine, radius) in &out.fire.cooked_off {
            let Some(centre_m) = part_of(v, magazine).map(|p| p.centre).filter(|c| c.is_finite()) else { continue };
            w.events.push(PhysEvent::CookOff { vessel: id, magazine, radius_cells: radius, pos: v.to_world(centre_m) });
            if let Some(a) = at_mut(&mut w.aux, slot) { a.cookoff_tick = Some(tick.0); }
            let size = v.grid.size();
            let cell = |m: f64, n: u16| (libm::floor(m / CELL_M).max(0.0) as i64).min(n.max(1) as i64 - 1) as i16;
            let Some(centre) = v.grid.index(content::cell::CellPos::new(cell(centre_m.x, size[0]), cell(centre_m.y, size[1]), cell(centre_m.z, size[2]))) else { continue };
            let mut blast = Vec::new();
            w.work.damage_cells += crate::armour::he_burst(&v.grid, centre, COOKOFF_B, radius, &mut blast);
            let from = v.grid.changes.len();
            let outcome = crate::damage::apply_cells(v, &blast, DamageCause::CookOff, &ctx);
            emit_damage(&mut w.events, v, from, &outcome, DamageCause::CookOff, attacker);
        }
    }

    lap.mark(crate::spans::Span::Systems);
    // ---- 11. Structure under the shared budget; geometry of dirty hulls ----
    let slots: Vec<u32> = w.vessels.iter().map(|(s, _)| s).collect();
    let start = slots.partition_point(|&s| s < w.split_cursor);
    let mut budget = SPLIT_WORK_BUDGET;
    let mut cursor = 0u32;
    for k in 0..slots.len() {
        let slot = slots[(start + k) % slots.len()];
        let next = slots[(start + k + 1) % slots.len()];
        let (unfinished, carved) = structure_for(w, slot, &mut budget, 1);
        if unfinished { cursor = slot; break; }
        if carved { cursor = next; break; }
        if budget == 0 { cursor = next; break; }
    }
    w.split_cursor = cursor;
    for &slot in &slots {
        let Some(v) = at(&w.vessels, slot) else { continue };
        if v.grid.dirty_chunks.count_ones() == 0 || !v.pending_destroyed.is_empty() || w.split_jobs.get(v.id).is_some_and(|j| j.in_progress) { continue; }
        if tick.0.saturating_sub(at(&w.aux, slot).map_or(0, |a| a.rebuilt_tick)) >= REBUILD_TICKS { rebuild_geometry(w, slot, false); }
    }

    lap.mark(crate::spans::Span::Structure);
    // ---- 12. Status, losses and wreck rules ----
    let mut changed: Vec<(u32, VesselStatus)> = Vec::new();
    for (slot, v) in w.vessels.values_mut() {
        if let Some(status) = v.evaluate_status(&sea, &tuning.stability) { changed.push((slot, status)); }
    }
    for (slot, status) in changed {
        let (Some(v), Some(a)) = (at_mut(&mut w.vessels, slot), at_mut(&mut w.aux, slot)) else { continue };
        let mut status = status;
        if let VesselStatus::Lost(_) = status {
            // A hull that goes down within a minute of its magazine exploding was lost to the explosion.
            if a.cookoff_tick.is_some_and(|t| tick.0.saturating_sub(t) <= COOKOFF_LOSS_TICKS) { status = VesselStatus::Lost(LossCause::CookOff); v.status = status; }
            a.lost_tick = Some(tick.0);
        }
        w.events.push(PhysEvent::StatusChanged { vessel: v.id, status });
        if let VesselStatus::Lost(cause) = status {
            w.events.push(PhysEvent::VesselLost { vessel: v.id, side: v.side, cause, by: a.last_attacker, tonnage_t: v.tonnage_t });
        }
    }
    let wrecks: Vec<(EntityId, u64, f64)> = w.vessels.iter().filter(|(_, v)| !afloat(v.status) && !w.doomed.contains(&v.id)).map(|(slot, v)| {
        let since = match v.status { VesselStatus::Lost(_) => at(&w.aux, slot).and_then(|a| a.lost_tick).unwrap_or(v.spawn_tick), _ => v.spawn_tick };
        let depth = if v.grid.solid_count == 0 { f64::INFINITY } else {
            let c = v.to_world(v.mass.com);
            let surface = if c.is_finite() { sea(c.x, c.z) } else { 0.0 };
            surface - v.top_height_m()
        };
        (v.id, since, depth)
    }).collect();
    for wreck in crate::structure::wrecks_to_despawn(&wrecks, tick.0, &tuning.structure) {
        w.events.push(PhysEvent::WreckDespawned { wreck });
        doom(&mut w.doomed, wreck);
    }

    // The local remesh feeds are bounded when no client drains them.
    for v in w.vessels.slots.iter_mut().flatten() {
        if v.grid.changes.len() > CHANGE_FEED_CAP { v.grid.changes.clear(); }
        if v.grid.dirty_render.len() > DIRTY_RENDER_COMPACT { v.grid.dirty_render.sort_unstable(); v.grid.dirty_render.dedup(); }
    }
    w.doomed.sort_unstable();
    w.doomed.dedup();

    lap.mark(crate::spans::Span::Status);
    // ---- 13. Tick counter ----
    w.terrain = terrain;
    w.tick = tick.next();
}

/// One ended body: its damage, its events, and its place in the despawn queue.
fn resolve_body(w: &mut World, ctx: &StepContext<'_>, outcome: BodyOutcome, grazes: &[crate::projectiles::BodyGraze], auto_band: &BandDef) {
    let cat = ctx.catalogue;
    let body = outcome.body();
    let row = w.bodies.row_of(body);
    let kind = row.and_then(|r| w.bodies.kind.get(r).copied());
    let def = kind.and_then(|k| cat.munitions.get_index(k.index())).map(|(_, d)| d);
    let launcher = row.and_then(|r| w.bodies.launcher.get(r).copied()).unwrap_or(EntityId::NONE);
    let slot = row.and_then(|r| w.bodies.launcher_slot.get(r).copied()).unwrap_or(PartSlot(0));
    let pos_of = |bodies: &Bodies, id: EntityId, or: DVec3| bodies.row_of(id).and_then(|r| bodies.pos.get(r).copied()).filter(|p| p.is_finite()).unwrap_or(or);
    doom(&mut w.doomed, body);
    match outcome {
        BodyOutcome::HullHit { vessel, entry, dir, world_point, .. } => {
            let hit = match (def, hull(&mut w.vessels, vessel)) {
                (Some(def), Some(v)) => {
                    let result = crate::armour::resolve_warhead(&v.grid, entry, dir, def.fuse, def.he_b, def.he_r_cells, auto_band);
                    w.work.damage_cells += result.work_units;
                    match result.burst.or(result.damage.first().map(|d| d.cell)) {
                        Some(cell) => {
                            let cause = cause_of(def.kind);
                            w.events.push(PhysEvent::BodyImpact { body, vessel, cell, world_point, cause });
                            let from = v.grid.changes.len();
                            let outcome = crate::damage::apply_hit(v, &result, cause, launcher, ctx);
                            emit_damage(&mut w.events, v, from, &outcome, cause, launcher);
                            if !launcher.is_none() { if let Some(a) = w.aux.get_mut(vessel) { a.last_attacker = launcher; } }
                            true
                        }
                        None => false,
                    }
                }
                _ => false,
            };
            // Through the hull's box without meeting a cell: the body is spent and nothing was struck.
            if !hit { w.events.push(PhysEvent::BodyDestroyed { body, pos: world_point, shot_down: false, by: EntityId::NONE }); }
        }
        BodyOutcome::AirBurst { at, killed, .. } => {
            w.events.push(PhysEvent::BodyBurst { body, pos: at, underwater: false });
            let by = if launcher.is_none() { body } else { launcher };
            for k in killed {
                w.events.push(PhysEvent::BodyDestroyed { body: k, pos: pos_of(&w.bodies, k, at), shot_down: true, by });
                doom(&mut w.doomed, k);
            }
        }
        BodyOutcome::UnderwaterBurst { at, vessel, .. } => {
            w.events.push(PhysEvent::BodyBurst { body, pos: at, underwater: true });
            if let (Some(def), Some(v)) = (def, vessel.and_then(|id| hull(&mut w.vessels, id))) {
                let vessel = v.id;
                let entry = v.kin.pose.to_local(at) / CELL_M;
                let size = v.grid.size();
                let centre = DVec3::new(size[0] as f64, size[1] as f64, size[2] as f64) * 0.5;
                let dir = (centre - entry).normalize_or(DVec3::Y);
                if entry.is_finite() {
                    let result = crate::armour::resolve_warhead(&v.grid, entry.to_array(), dir.to_array(), def.fuse, def.he_b, def.he_r_cells, auto_band);
                    w.work.damage_cells += result.work_units;
                    if let Some(cell) = result.burst.or(result.damage.first().map(|d| d.cell)) {
                        let cause = cause_of(def.kind);
                        w.events.push(PhysEvent::BodyImpact { body, vessel, cell, world_point: at, cause });
                        let from = v.grid.changes.len();
                        let outcome = crate::damage::apply_hit(v, &result, cause, launcher, ctx);
                        emit_damage(&mut w.events, v, from, &outcome, cause, launcher);
                        if !launcher.is_none() { if let Some(a) = w.aux.get_mut(vessel) { a.last_attacker = launcher; } }
                    }
                }
            }
        }
        BodyOutcome::Expired { at, shot_down, .. } => {
            let by = if shot_down { grazes.iter().rev().find(|g| g.body == body).map_or(EntityId::NONE, |g| g.shooter) } else { EntityId::NONE };
            w.events.push(PhysEvent::BodyDestroyed { body, pos: at, shot_down, by });
        }
        BodyOutcome::Recovered { vessel, .. } => {
            if let (Some(kind), Some(v)) = (kind, hull(&mut w.vessels, vessel)) { crate::parts::recover_offboard(&mut v.parts, slot, kind, cat); }
            w.events.push(PhysEvent::BodyRecovered { body, vessel });
        }
    }
}

/// Position, velocity and side of any entity from the stores (the body of `World::locate`).
pub(super) fn locate_in(entities: &EntityTable, vessels: &Column<Vessel>, bodies: &Bodies, decoys: &Decoys, id: EntityId) -> Option<(DVec3, DVec3, SideId)> {
    match entities.kind(id)? {
        EntityKind::Vessel | EntityKind::Wreck => {
            let v = vessels.get(id).filter(|v| v.id == id)?;
            let at = v.to_world(v.mass.com);
            Some((if at.is_finite() { at } else { v.kin.pose.pos }, v.kin.lin_vel, v.side))
        }
        EntityKind::Body => {
            let r = bodies.row_of(id)?;
            Some((*bodies.pos.get(r)?, *bodies.vel.get(r)?, *bodies.side.get(r)?))
        }
        EntityKind::Decoy => {
            let r = decoys.row_of(id)?;
            let owner = *decoys.owner.get(r)?;
            // A decoy has no side of its own; it reads as its owner's.
            let side = match entities.kind(owner) {
                Some(EntityKind::Vessel | EntityKind::Wreck) => vessels.get(owner).map(|v| v.side),
                Some(EntityKind::Body) => bodies.row_of(owner).and_then(|r| bodies.side.get(r).copied()),
                _ => None,
            };
            Some((*decoys.pos.get(r)?, DVec3::ZERO, side.unwrap_or_default()))
        }
    }
}

/// Structure work for one hull within `budget` cells: the split job, debris, and wreck pieces carved into
/// new bodies. Stops after `max_jobs_with_wrecks` jobs that yielded a wreck. Returns (work remains, a wreck
/// was carved).
pub(super) fn structure_for(w: &mut World, slot: u32, budget: &mut u32, max_jobs_with_wrecks: usize) -> (bool, bool) {
    let cat_arc = w.config.catalogue.clone();
    let cat: &Catalogue = &cat_arc;
    let tick = w.tick.0;
    let mut carved_jobs = 0usize;
    loop {
        let Some(v) = at_mut(&mut w.vessels, slot) else { return (false, carved_jobs > 0) };
        let id = v.id;
        if v.pending_destroyed.is_empty() && !w.split_jobs.get(id).is_some_and(|j| j.in_progress) { return (false, carved_jobs > 0); }
        if *budget == 0 { return (true, carved_jobs > 0); }
        if !w.split_jobs.contains(id) { w.split_jobs.insert(id, SplitJob::default()); }
        let Some(job) = w.split_jobs.get_mut(id) else { return (false, carved_jobs > 0) };
        let res = crate::structure::step_split(v, job, *budget);
        w.work.split_cells = w.work.split_cells.saturating_add(res.cells_visited);
        *budget = budget.saturating_sub(res.cells_visited);
        let mut wrecks: Vec<Vessel> = Vec::new();
        for piece in res.pieces {
            let centroid = |v: &Vessel, cells: &[CellIndex]| {
                let mut sum = DVec3::ZERO;
                for c in cells { sum += DVec3::from(v.grid.pos(*c).centre_m()); }
                v.to_world(if cells.is_empty() { v.mass.com } else { sum / cells.len() as f64 })
            };
            if piece.debris {
                w.events.push(PhysEvent::Debris { vessel: id, pos: centroid(v, &piece.cells), cells: piece.cells });
                continue;
            }
            let new_id = w.entities.spawn(EntityKind::Wreck);
            if new_id.is_none() {
                // No slot left for a new body: the piece leaves as debris instead.
                v.grid.remove_cells(&piece.cells);
                w.events.push(PhysEvent::Debris { vessel: id, pos: centroid(v, &piece.cells), cells: piece.cells });
                continue;
            }
            let mut wreck = crate::structure::carve_piece(v, &piece, new_id, cat);
            wreck.spawn_tick = tick;
            wreck.grid.drain_dirty_chunks();
            // The client builds the piece from the split event; its feeds start empty.
            wreck.grid.changes.clear();
            wreck.grid.dirty_render.clear();
            let extent = cuboid_extent(&wreck.cuboids);
            refresh_bounds(&mut wreck, extent);
            wreck.body = w.physics.add_vessel(wreck.kin.pose, wreck.kin.lin_vel, &wreck.mass, &wreck.cuboids);
            w.physics.set_kinematics(wreck.body, &wreck.kin);
            if wreck.fidelity == Fidelity::Kinematic {
                w.physics.set_kinematic(wreck.body, true);
                w.physics.set_kinematics(wreck.body, &wreck.kin);
            }
            let parent_aux = w.aux.get(id);
            let aux = VesselAux { last_attacker: parent_aux.map_or(EntityId::NONE, |a| a.last_attacker), rebuilt_tick: tick, extent, pushed_mass: Some(wreck.mass), ..Default::default() };
            if let Some(prefab) = w.prefabs.get(id).cloned() { w.prefabs.insert(new_id, prefab); }
            w.aux.insert(new_id, aux);
            w.split_jobs.remove(new_id);
            w.events.push(PhysEvent::Split { vessel: id, piece: new_id, cells: piece.cells });
            wrecks.push(wreck);
        }
        let carved = !wrecks.is_empty();
        for wreck in wrecks { w.vessels.insert(wreck.id, wreck); }
        if carved {
            carved_jobs += 1;
            // The parent lost cells, mass and a share of its momentum: push all three now.
            rebuild_geometry(w, slot, true);
            if let Some(v) = at(&w.vessels, slot) { w.physics.set_kinematics(v.body, &v.kin); }
            if carved_jobs >= max_jobs_with_wrecks { return (res.unfinished, true); }
        }
        if !res.unfinished { return (false, carved_jobs > 0); }
        // No progress and nothing carved: leave the rest to the next tick instead of spinning.
        if res.cells_visited == 0 && !carved { return (true, carved_jobs > 0); }
    }
}

/// Rebuild a hull's collider, buoyancy blocks and dry mass from its cells and push them to the physics body.
/// Unless `force`d, a hull with no dirty chunk is left alone.
pub(super) fn rebuild_geometry(w: &mut World, slot: u32, force: bool) {
    let cat_arc = w.config.catalogue.clone();
    let cat: &Catalogue = &cat_arc;
    let tick = w.tick.0;
    let Some(v) = at_mut(&mut w.vessels, slot) else { return };
    if !force && v.grid.dirty_chunks.count_ones() == 0 { return; }
    crate::physics::regreedy_dirty(&v.grid, &mut v.cuboids);
    crate::hydro::rebuild_dirty_blocks(&mut v.blocks, &v.grid, &v.compartments);
    v.grid.drain_dirty_chunks();
    v.dry_mass = crate::mass::dry_mass(&v.grid, &v.parts, cat);
    v.mass = crate::mass::with_fluids(&v.dry_mass, &v.parts, &v.compartments, cat);
    w.physics.set_collider(v.body, &v.cuboids);
    w.physics.set_mass(v.body, &v.mass);
    w.work.collider_rebuilds += 1;
    if let Some(a) = at_mut(&mut w.aux, slot) {
        a.rebuilt_tick = tick;
        a.far_blocks = None;
        a.pushed_mass = Some(v.mass);
        if a.extent.is_some() { a.extent = cuboid_extent(&v.cuboids); }
    }
}
