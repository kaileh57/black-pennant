use super::*;
use crate::math::Pose;
use crate::projectiles::ProjectileId;
use crate::terrain::Terrain;
use crate::time::{Tick, DT};
use crate::waves::WaveSet;
use crate::world::StepContext;
use content::campaign::MapDef;
use content::catalogue::Catalogue;
use content::enums::Domain;
use content::ids::MunitionId;
use content::part::weapons::SeekerKind;
use content::tuning::TuningTable;
use content::weather::WeatherState;

fn def(id: &str, kind: MunitionKind, speed: f64, range: f64, accel: f64, alt: f64, fuse: Fuse, radius: f64, hp: u8, endurance: f64) -> MunitionDef {
    MunitionDef {
        id: MunitionId::new(id), name: Default::default(), kind, phase: Default::default(), domain: Domain::Weapon,
        speed_mps: speed, range_m: range, max_accel: accel, cruise_alt_m: alt, fuse, he_b: 8, he_r_cells: 4,
        seeker: SeekerKind::None, seeker_r_ref_m: 0.0, seeker_fov_half_deg: 0.0, pn_n: 3.0, radius_m: radius, hp,
        rcs_m2: 0.1, noise: 0.0, mass_kg: 100.0, cost: 1, sensors: Vec::new(), payload: Vec::new(), endurance_s: endurance, wire_guided: false,
        model: Default::default(), icon: Default::default(),
    }
}

const ASM: MunitionIdx = MunitionIdx(0);
const SAM: MunitionIdx = MunitionIdx(1);
const MINE: MunitionIdx = MunitionIdx(2);
const DC: MunitionIdx = MunitionIdx(3);
const DRONE: MunitionIdx = MunitionIdx(4);
const TORP: MunitionIdx = MunitionIdx(5);
const RHIB: MunitionIdx = MunitionIdx(6);

fn catalogue() -> Catalogue {
    let mut c = Catalogue::default();
    for d in [
        def("asm", MunitionKind::AntiShipMissile, 270.0, 30_000.0, 150.0, 5.0, Fuse::Penetrate { armour_mm: 40.0, delay_cells: 2 }, 0.5, 1, 0.0),
        def("sam", MunitionKind::Sam, 600.0, 25_000.0, 300.0, 0.0, Fuse::Proximity { radius_m: 8.0 }, 0.5, 1, 0.0),
        def("mine", MunitionKind::Mine, 0.0, 0.0, 0.0, 0.0, Fuse::Influence { radius_m: 10.0 }, 0.5, 1, 0.0),
        def("dc", MunitionKind::DepthCharge, 0.0, 0.0, 0.0, -30.0, Fuse::Depth { sink_mps: 3.0 }, 0.5, 1, 0.0),
        def("drone", MunitionKind::Drone, 40.0, 50_000.0, 20.0, 300.0, Fuse::None, 1.0, 2, 600.0),
        def("torp", MunitionKind::Torpedo, 20.6, 8_000.0, 10.0, -3.0, Fuse::Contact, 0.5, 1, 0.0),
        def("rhib", MunitionKind::Rhib, 15.0, 0.0, 5.0, 0.0, Fuse::None, 2.0, 2, 1_800.0),
    ] {
        c.munitions.insert(d.id.clone(), d);
    }
    c
}

struct Env { cat: Catalogue, tuning: TuningTable, waves: WaveSet, weather: WeatherState, terrain: Terrain }
impl Env {
    fn new() -> Self {
        Self { cat: catalogue(), tuning: TuningTable::default(), waves: WaveSet::calm(), weather: WeatherState { wind_speed_mps: 0.0, ..WeatherState::default() }, terrain: Terrain::new(MapDef { depth_m: 200.0, ..MapDef::default() }) }
    }
    fn ctx(&self, tick: u64) -> StepContext<'_> {
        StepContext { seed: 1, tick: Tick(tick), dt: DT, time_s: tick as f64 * DT, catalogue: &self.cat, tuning: &self.tuning, waves: &self.waves, weather: &self.weather, terrain: &self.terrain }
    }
}

fn eid(i: u32) -> EntityId { EntityId { index: i, generation: 0 } }

/// A hull whose grid origin sits at `pos`, bow along compass `heading`, box `dims` (length, height, beam).
fn hull(id: u32, pos: DVec3, heading: f64, dims: DVec3) -> HullTarget {
    let pose = Pose::from_heading(pos, heading);
    let mut min = DVec3::splat(f64::INFINITY);
    let mut max = DVec3::splat(f64::NEG_INFINITY);
    for c in 0..8 {
        let l = DVec3::new(if c & 1 != 0 { dims.x } else { 0.0 }, if c & 2 != 0 { dims.y } else { 0.0 }, if c & 4 != 0 { dims.z } else { 0.0 });
        let w = pose.to_world(l);
        min = min.min(w);
        max = max.max(w);
    }
    HullTarget { vessel: eid(id), bounds: Aabb { min, max }, pose }
}

fn run(b: &mut Bodies, d: &mut Decoys, env: &Env, tick: &mut u64, hulls: &[HullTarget], ticks: u32) -> Vec<BodyOutcome> {
    let mut all = Vec::new();
    for _ in 0..ticks {
        let mut out = Vec::new();
        b.step(d, &env.ctx(*tick), hulls, &[], &mut out);
        *tick += 1;
        all.extend(out);
    }
    all
}

#[test]
fn insert_order_replace_and_remove() {
    let env = Env::new();
    let mut b = Bodies::default();
    for i in [5u32, 1, 3] { b.insert(eid(i), ASM, SideId(0), DVec3::ZERO, DVec3::X, EntityId::NONE, PartSlot(0), 0, &env.cat); }
    assert_eq!(b.entity, vec![eid(1), eid(3), eid(5)]);
    assert_eq!(b.remaining[0], 30_000.0);
    b.insert(eid(3), DRONE, SideId(1), DVec3::ONE, DVec3::X, EntityId::NONE, PartSlot(0), 0, &env.cat);
    assert_eq!(b.len(), 3);
    assert_eq!((b.kind[1], b.hp[1], b.remaining[1]), (DRONE, 2, 600.0));
    b.remove(eid(3));
    b.remove(eid(3));
    assert_eq!(b.entity, vec![eid(1), eid(5)]);
    // Unknown kind: inserted with 0 hit points and expired on the next step, swept on the one after.
    b.insert(eid(9), MunitionIdx(77), SideId(0), DVec3::ZERO, DVec3::X, EntityId::NONE, PartSlot(0), 0, &env.cat);
    let mut d = Decoys::default();
    let mut out = Vec::new();
    b.step(&mut d, &env.ctx(0), &[], &[], &mut out);
    assert!(out.iter().any(|o| matches!(o, BodyOutcome::Expired { body, .. } if *body == eid(9))));
    assert_eq!(b.phase[b.row_of(eid(9)).unwrap()], BodyPhase::Dead);
    out.clear();
    b.step(&mut d, &env.ctx(1), &[], &[], &mut out);
    assert!(b.row_of(eid(9)).is_none());
    assert!(out.iter().all(|o| o.body() != eid(9)));
}

#[test]
fn unequal_columns_do_not_panic() {
    let env = Env::new();
    let mut b = Bodies::default();
    b.insert(eid(1), ASM, SideId(0), DVec3::new(0.0, 5.0, 0.0), DVec3::X * 270.0, EntityId::NONE, PartSlot(0), 0, &env.cat);
    b.entity.push(eid(2));
    b.pos.push(DVec3::ZERO);
    let mut d = Decoys::default();
    d.entity.push(eid(7));
    let mut out = Vec::new();
    b.step(&mut d, &env.ctx(0), &[], &[BodyGraze { tick: 0, projectile: ProjectileId(0), body: eid(40), shooter: eid(0) }], &mut out);
    assert_eq!(b.len(), 1);
    assert_eq!(d.len(), 0);
    assert_eq!(d.rcs_m2(5, 10.0, &env.tuning.decoys), 0.0);
    assert!(d.smoke_cylinder(5, &env.tuning.decoys).is_none());
}

#[test]
fn sea_skimmer_holds_altitude_and_hits_hull_with_cell_frame_entry() {
    let env = Env::new();
    let mut b = Bodies::default();
    let mut d = Decoys::default();
    // Launcher behind the missile, target 2 km east lying north-south (heading 0: bow toward -Z).
    let launcher = hull(1, DVec3::new(-30.0, -3.0, -5.0), core::f64::consts::FRAC_PI_2, DVec3::new(40.0, 12.0, 10.0));
    let target = hull(2, DVec3::new(2_000.0, -3.0, 40.0), 0.0, DVec3::new(80.0, 14.0, 12.0));
    b.insert(eid(10), ASM, SideId(0), DVec3::new(0.0, 6.0, 0.0), DVec3::new(60.0, 10.0, 0.0), eid(1), PartSlot(2), 0, &env.cat);
    b.steer[0] = Steer::Waypoint(DVec3::new(2_006.0, 0.0, 0.0));
    let mut tick = 0;
    let mut hit = None;
    for _ in 0..1_200 {
        let out = run(&mut b, &mut d, &env, &mut tick, &[launcher, target], 1);
        if let Some(o) = out.into_iter().next() { hit = Some(o); break; }
        if tick > 360 {
            let y = b.pos[0].y;
            assert!((y - 5.0).abs() < 0.5, "altitude {y}");
            assert!((b.vel[0].length() - 270.0).abs() < 1e-6);
        }
    }
    let Some(BodyOutcome::HullHit { vessel, entry, dir, world_point, .. }) = hit else { panic!("no hit: {hit:?}") };
    assert_eq!(vessel, eid(2));
    assert!((world_point.x - 2_000.0).abs() < 1.0, "{world_point:?}");
    // Entering the target's port side: local +Z is starboard, so the port face is z = 0 in the cell frame and
    // travel is along local +Z.
    let pose = target.pose;
    let local = pose.to_local(world_point) / CELL_M;
    for k in 0..3 { assert!((local[k] - entry[k]).abs() < 1e-6); }
    assert!(entry[2].abs() < 1e-6, "{entry:?}");
    assert!(dir[2] > 0.99, "{dir:?}");
}

#[test]
fn launcher_is_ignored_until_cleared() {
    let env = Env::new();
    let mut b = Bodies::default();
    let mut d = Decoys::default();
    let launcher = hull(1, DVec3::new(-20.0, -3.0, -5.0), core::f64::consts::FRAC_PI_2, DVec3::new(40.0, 12.0, 10.0));
    // Spawned inside the launcher's box flying out of it: no hit on the launcher.
    b.insert(eid(10), ASM, SideId(0), DVec3::new(0.0, 6.0, 0.0), DVec3::new(30.0, 0.0, 0.0), eid(1), PartSlot(0), 0, &env.cat);
    let out = run(&mut b, &mut d, &env, &mut 0, &[launcher], 120);
    assert!(out.is_empty(), "{out:?}");
    assert!(b.cleared[0]);
    // Flying back into it now counts.
    b.steer[0] = Steer::Accel(DVec3::ZERO);
    b.vel[0] = DVec3::new(-270.0, 0.0, 0.0);
    let mut tick = 120;
    let out = run(&mut b, &mut d, &env, &mut tick, &[launcher], 400);
    assert!(matches!(out.first(), Some(BodyOutcome::HullHit { vessel, .. }) if *vessel == eid(1)), "{out:?}");
}

#[test]
fn proximity_fuse_bursts_on_hostile_air_body_and_spares_own_side() {
    let env = Env::new();
    let mut b = Bodies::default();
    let mut d = Decoys::default();
    // Head-on: SAM at 600 m/s, ASM at 270 m/s, 3 m lateral miss; closing 14.5 m per tick.
    b.insert(eid(1), SAM, SideId(0), DVec3::new(0.0, 6.0, 0.0), DVec3::new(600.0, 0.0, 0.0), EntityId::NONE, PartSlot(0), 0, &env.cat);
    b.insert(eid(2), ASM, SideId(1), DVec3::new(1_000.0, 6.0, 3.0), DVec3::new(-270.0, 0.0, 0.0), EntityId::NONE, PartSlot(0), 0, &env.cat);
    b.insert(eid(3), ASM, SideId(0), DVec3::new(1_000.0, 6.0, -3.0), DVec3::new(-270.0, 0.0, 0.0), EntityId::NONE, PartSlot(0), 0, &env.cat);
    for i in 0..3 { b.phase[i] = BodyPhase::Cruise; }
    b.steer[0] = Steer::Accel(DVec3::ZERO);
    let out = run(&mut b, &mut d, &env, &mut 0, &[], 120);
    assert_eq!(out.len(), 1, "{out:?}");
    let BodyOutcome::AirBurst { body, killed, .. } = &out[0] else { panic!("{out:?}") };
    assert_eq!(*body, eid(1));
    assert_eq!(killed, &vec![eid(2)]);
    assert!(b.row_of(eid(2)).is_none());
    assert!(b.row_of(eid(3)).is_some());
}

#[test]
fn grazes_remove_hit_points() {
    let env = Env::new();
    let mut b = Bodies::default();
    let mut d = Decoys::default();
    b.insert(eid(4), DRONE, SideId(1), DVec3::new(0.0, 300.0, 0.0), DVec3::new(40.0, 0.0, 0.0), EntityId::NONE, PartSlot(0), 0, &env.cat);
    let g = BodyGraze { tick: 0, projectile: ProjectileId(1), body: eid(4), shooter: eid(9) };
    let mut out = Vec::new();
    b.step(&mut d, &env.ctx(0), &[], &[g], &mut out);
    assert!(out.is_empty());
    assert_eq!(b.hp[0], 1);
    b.step(&mut d, &env.ctx(1), &[], &[g], &mut out);
    assert!(matches!(out.as_slice(), [BodyOutcome::Expired { body, shot_down: true, .. }] if *body == eid(4)), "{out:?}");
}

#[test]
fn mine_arms_ignores_layer_and_fires_on_another_hull() {
    let env = Env::new();
    let mut b = Bodies::default();
    let mut d = Decoys::default();
    let layer = hull(1, DVec3::new(-45.0, -3.0, -5.0), core::f64::consts::FRAC_PI_2, DVec3::new(40.0, 10.0, 10.0));
    b.insert(eid(20), MINE, SideId(0), DVec3::new(0.0, 2.0, 0.0), DVec3::ZERO, eid(1), PartSlot(0), 0, &env.cat);
    let mut tick = 0;
    let out = run(&mut b, &mut d, &env, &mut tick, &[layer], 60 * 40);
    assert!(out.is_empty(), "{out:?}");
    assert!((b.pos[0].y + MINE_DEPTH_M).abs() < 1e-9 && b.vel[0] == DVec3::ZERO);
    // Another hull passing 8 m away triggers it.
    let other = hull(2, DVec3::new(-20.0, -3.0, 8.0), core::f64::consts::FRAC_PI_2, DVec3::new(40.0, 10.0, 10.0));
    let out = run(&mut b, &mut d, &env, &mut tick, &[layer, other], 1);
    assert!(matches!(out.as_slice(), [BodyOutcome::UnderwaterBurst { vessel: Some(v), .. }] if *v == eid(2)), "{out:?}");
    // Not armed yet: a fresh mine next to a hull does nothing.
    let mut b = Bodies::default();
    b.insert(eid(21), MINE, SideId(0), DVec3::new(0.0, -2.0, 0.0), DVec3::ZERO, EntityId::NONE, PartSlot(0), 0, &env.cat);
    let out = run(&mut b, &mut d, &env, &mut 0, &[other], 60);
    assert!(out.is_empty());
}

#[test]
fn depth_charge_sinks_to_set_depth() {
    let env = Env::new();
    let mut b = Bodies::default();
    let mut d = Decoys::default();
    b.insert(eid(30), DC, SideId(0), DVec3::new(0.0, 3.0, 0.0), DVec3::new(5.0, 2.0, 0.0), EntityId::NONE, PartSlot(0), 0, &env.cat);
    assert_eq!(b.set_depth_m[0], 30.0);
    let sub = hull(3, DVec3::new(0.0, -36.0, 10.0), 0.0, DVec3::new(20.0, 5.0, 6.0));
    let mut tick = 0;
    let mut got = None;
    for _ in 0..60 * 20 {
        let out = run(&mut b, &mut d, &env, &mut tick, &[sub], 1);
        if !out.is_empty() { got = Some(out); break; }
    }
    let out = got.expect("depth charge never burst");
    let BodyOutcome::UnderwaterBurst { at, vessel, .. } = &out[0] else { panic!("{out:?}") };
    assert!((at.y + 30.0).abs() < 0.06, "{at:?}");
    assert_eq!(*vessel, Some(eid(3)));
    // About 10 s of sinking at 3 m/s.
    assert!((tick as f64 * DT - 10.5).abs() < 1.5, "{tick}");
}

#[test]
fn torpedo_runs_at_depth_and_strikes_below_the_waterline() {
    let env = Env::new();
    let mut b = Bodies::default();
    let mut d = Decoys::default();
    let target = hull(2, DVec3::new(500.0, -5.0, 30.0), 0.0, DVec3::new(80.0, 14.0, 12.0));
    b.insert(eid(11), TORP, SideId(0), DVec3::new(0.0, 2.0, 0.0), DVec3::new(15.0, 0.0, 0.0), EntityId::NONE, PartSlot(0), 0, &env.cat);
    let mut tick = 0;
    let mut hit = None;
    for _ in 0..60 * 40 {
        let out = run(&mut b, &mut d, &env, &mut tick, &[target], 1);
        if let Some(o) = out.into_iter().next() { hit = Some(o); break; }
        if tick > 600 { assert!((b.pos[0].y + 3.0).abs() < 0.3, "{:?}", b.pos[0]); }
    }
    let Some(BodyOutcome::HullHit { vessel, world_point, .. }) = hit else { panic!("{hit:?}") };
    assert_eq!(vessel, eid(2));
    assert!(world_point.y < 0.0);
}

#[test]
fn range_runs_out() {
    let env = Env::new();
    let mut b = Bodies::default();
    let mut d = Decoys::default();
    b.insert(eid(1), ASM, SideId(0), DVec3::new(0.0, 5.0, 0.0), DVec3::new(270.0, 0.0, 0.0), EntityId::NONE, PartSlot(0), 0, &env.cat);
    b.remaining[0] = 45.0;
    b.phase[0] = BodyPhase::Cruise;
    let out = run(&mut b, &mut d, &env, &mut 0, &[], 20);
    assert!(matches!(out.as_slice(), [BodyOutcome::Expired { shot_down: false, .. }]), "{out:?}");
}

#[test]
fn a_ditched_weapon_ends_its_flight_without_a_burst() {
    // Guidance with nothing left to steer by (a seeker that kept a decoy now gone) orders `Steer::Ditch`: the
    // body ends at once as an expiry, with most of its range unflown and a hostile air body beside it unharmed.
    let env = Env::new();
    let mut b = Bodies::default();
    let mut d = Decoys::default();
    b.insert(eid(1), ASM, SideId(0), DVec3::new(0.0, 5.0, 0.0), DVec3::new(270.0, 0.0, 0.0), EntityId::NONE, PartSlot(0), 0, &env.cat);
    b.insert(eid(2), DRONE, SideId(1), DVec3::new(3.0, 5.0, 0.0), DVec3::new(270.0, 0.0, 0.0), EntityId::NONE, PartSlot(0), 0, &env.cat);
    for r in 0..2 { b.phase[r] = BodyPhase::Cruise; }
    let mut tick = 0;
    assert!(run(&mut b, &mut d, &env, &mut tick, &[], 30).is_empty(), "both fly on under Hold");
    assert!(b.remaining[0] > 29_000.0);
    b.steer[0] = Steer::Ditch;
    let out = run(&mut b, &mut d, &env, &mut tick, &[], 1);
    assert!(matches!(out.as_slice(), [BodyOutcome::Expired { body, shot_down: false, .. }] if *body == eid(1)), "{out:?}");
    assert_eq!(b.phase[0], BodyPhase::Dead);
    assert_ne!(b.phase[1], BodyPhase::Dead, "a ditch is not a burst");
    // The row is gone at the next step and produces nothing more.
    assert!(run(&mut b, &mut d, &env, &mut tick, &[], 5).is_empty());
    assert_eq!(b.len(), 1);
}

#[test]
fn drone_returns_at_bingo_and_is_recovered() {
    let env = Env::new();
    let mut b = Bodies::default();
    let mut d = Decoys::default();
    let ship = hull(1, DVec3::new(-20.0, -3.0, -5.0), core::f64::consts::FRAC_PI_2, DVec3::new(40.0, 10.0, 10.0));
    b.insert(eid(5), DRONE, SideId(0), DVec3::new(4_000.0, 300.0, 0.0), DVec3::new(40.0, 0.0, 0.0), eid(1), PartSlot(3), 0, &env.cat);
    b.cleared[0] = true;
    b.phase[0] = BodyPhase::Cruise;
    b.remaining[0] = 130.0;
    b.steer[0] = Steer::Orbit { centre: DVec3::new(4_000.0, 0.0, 1_000.0), radius_m: 500.0 };
    let mut tick = 0;
    let out = run(&mut b, &mut d, &env, &mut tick, &[ship], 60);
    assert!(out.is_empty());
    assert_eq!(b.phase[0], BodyPhase::Returning);
    let out = run(&mut b, &mut d, &env, &mut tick, &[ship], 60 * 200);
    assert!(matches!(out.as_slice(), [BodyOutcome::Recovered { body, vessel }] if *body == eid(5) && *vessel == eid(1)), "{out:?}");
}

#[test]
fn outcomes_are_in_ascending_entity_order() {
    let env = Env::new();
    let mut b = Bodies::default();
    let mut d = Decoys::default();
    for i in [8u32, 2, 5] {
        b.insert(eid(i), ASM, SideId(0), DVec3::new(i as f64 * 10.0, 5.0, 0.0), DVec3::new(270.0, 0.0, 0.0), EntityId::NONE, PartSlot(0), 0, &env.cat);
    }
    for r in 0..3 { b.remaining[r] = 1.0; b.phase[r] = BodyPhase::Cruise; }
    b.steer[2] = Steer::Detonate;
    let mut out = Vec::new();
    b.step(&mut d, &env.ctx(0), &[], &[], &mut out);
    let ids: Vec<EntityId> = out.iter().map(BodyOutcome::body).collect();
    assert_eq!(ids, vec![eid(2), eid(5), eid(8)]);
}

#[test]
fn decoy_signatures_and_lifetimes() {
    let env = Env::new();
    let t = &env.tuning.decoys;
    let mut d = Decoys::default();
    d.insert(eid(1), DecoyKind::Chaff, eid(9), DVec3::ZERO);
    d.insert(eid(2), DecoyKind::Flare, eid(9), DVec3::ZERO);
    d.insert(eid(3), DecoyKind::Smoke, eid(9), DVec3::new(10.0, 4.0, 20.0));
    d.insert(eid(4), DecoyKind::TowedDecoy, eid(9), DVec3::ZERO);
    let set_age = |d: &mut Decoys, a: f64| for x in d.age_s.iter_mut() { *x = a; };
    set_age(&mut d, 1.5);
    assert!((d.rcs_m2(0, 0.0, t) - 500.0).abs() < 1e-9);
    assert_eq!(d.ir_mw(1, t), 20.0);
    let (c, r, h) = d.smoke_cylinder(2, t).unwrap();
    assert!((r - 9.0).abs() < 1e-9 && (h - 4.5).abs() < 1e-9 && (c - DVec3::new(10.0, 2.25, 20.0)).length() < 1e-9);
    assert_eq!(d.rcs_m2(3, 300.0, t), 600.0);
    assert_eq!(d.rcs_m2(1, 300.0, t), 0.0);
    set_age(&mut d, 3.0 + 30.0);
    assert!((d.rcs_m2(0, 0.0, t) - 500.0).abs() < 1e-9);
    assert_eq!(d.ir_mw(1, t), 0.0);
    assert!(d.is_expired(1, t) && !d.is_expired(0, t));
    let (_, r, h) = d.smoke_cylinder(2, t).unwrap();
    assert_eq!((r, h), (60.0, 30.0));
    set_age(&mut d, 130.0);
    assert!(d.smoke_cylinder(2, t).is_none());
    assert_eq!(d.rcs_m2(0, 0.0, t), 0.0);
    assert_eq!(d.expired(t), vec![eid(1), eid(2), eid(3)]);
}

#[test]
fn decoys_drift_tow_and_expire() {
    let mut env = Env::new();
    env.weather.wind_dir_rad = core::f64::consts::FRAC_PI_2; // toward east
    env.weather.wind_speed_mps = 10.0;
    let mut b = Bodies::default();
    let mut d = Decoys::default();
    let ship = hull(9, DVec3::new(100.0, -3.0, 0.0), core::f64::consts::FRAC_PI_2, DVec3::new(40.0, 10.0, 10.0));
    d.insert(eid(1), DecoyKind::Chaff, eid(9), DVec3::ZERO);
    d.insert(eid(2), DecoyKind::TowedDecoy, eid(9), DVec3::ZERO);
    let mut tick = 0;
    run(&mut b, &mut d, &env, &mut tick, &[ship], 60);
    assert!((d.pos[0] - DVec3::new(10.0, 0.0, 0.0)).length() < 1e-6, "{:?}", d.pos[0]);
    // Ship bow east, stern at x = 100: the towed body streams 200 m astern (west).
    assert!((d.pos[1].x - (100.0 - 200.0)).abs() < 1e-6 && (d.pos[1].z - 5.0).abs() < 1e-6, "{:?}", d.pos[1]);
    // Owner gone: the towed decoy expires, and the next step drops it.
    run(&mut b, &mut d, &env, &mut tick, &[], 1);
    assert_eq!(d.expired(&env.tuning.decoys), vec![eid(2)]);
    run(&mut b, &mut d, &env, &mut tick, &[], 1);
    assert_eq!(d.entity, vec![eid(1)]);
    // Chaff gone after bloom plus life.
    run(&mut b, &mut d, &env, &mut tick, &[], 60 * 64);
    assert!(d.is_empty());
}

/// Rough seas: a sea-skimmer cruising 8 km never flies into a wave, and keeps the floor above the local surface.
#[test]
fn sea_skimmer_survives_rough_seas() {
    for code in [6u8, 7, 8] {
        let mut env = Env::new();
        env.weather = WeatherState { sea_state: content::weather::SeaState(code), wind_speed_mps: 0.0, ..WeatherState::default() };
        env.waves = WaveSet::generate(3, &env.weather, &env.tuning.waves);
        let mut b = Bodies::default();
        let mut d = Decoys::default();
        b.insert(eid(1), ASM, SideId(0), DVec3::new(0.0, 12.0, 0.0), DVec3::new(270.0, 0.0, 0.0), EntityId::NONE, PartSlot(0), 0, &env.cat);
        b.phase[0] = BodyPhase::Cruise;
        b.steer[0] = Steer::Waypoint(DVec3::new(50_000.0, 0.0, 0.0));
        let mut tick = 0;
        let mut worst = f64::INFINITY;
        for _ in 0..60 * 30 {
            let out = run(&mut b, &mut d, &env, &mut tick, &[], 1);
            assert!(out.is_empty(), "sea state {code}: {out:?} at tick {tick}");
            let p = b.pos[0];
            worst = worst.min(p.y - env.waves.height_at(p.x, p.z, tick as f64 * DT));
        }
        assert!(worst > 0.5, "sea state {code}: worst clearance {worst}");
    }
}

/// Terminal homing follows the vertical demand: a skimmer cruising at 5 m dives onto a craft whose top is at
/// 2.5 m, and in a rough sea a demand aimed into the water is held at the floor above the waves.
#[test]
fn terminal_homing_dives_onto_low_craft_and_keeps_the_wave_floor() {
    let env = Env::new();
    let mut b = Bodies::default();
    let mut d = Decoys::default();
    let craft = hull(2, DVec3::new(1_500.0, -1.5, 20.0), 0.0, DVec3::new(30.0, 4.0, 6.0));
    b.insert(eid(1), ASM, SideId(0), DVec3::new(0.0, 5.0, 0.0), DVec3::new(270.0, 0.0, 0.0), EntityId::NONE, PartSlot(0), 0, &env.cat);
    b.phase[0] = BodyPhase::Cruise;
    let aim = DVec3::new(1_503.0, 1.25, 0.0);
    let mut tick = 0;
    let mut hit = None;
    for _ in 0..60 * 10 {
        let los = (aim - b.pos[0]).normalize();
        b.steer[0] = Steer::Accel((los * 270.0 - b.vel[0]) * 3.0);
        let out = run(&mut b, &mut d, &env, &mut tick, &[craft], 1);
        if let Some(o) = out.into_iter().next() { hit = Some(o); break; }
    }
    assert!(matches!(hit, Some(BodyOutcome::HullHit { vessel, .. }) if vessel == eid(2)), "{hit:?}");

    let mut env = Env::new();
    env.weather = WeatherState { sea_state: content::weather::SeaState(7), wind_speed_mps: 0.0, ..WeatherState::default() };
    env.waves = WaveSet::generate(5, &env.weather, &env.tuning.waves);
    let mut b = Bodies::default();
    b.insert(eid(1), ASM, SideId(0), DVec3::new(0.0, 8.0, 0.0), DVec3::new(270.0, 0.0, 0.0), EntityId::NONE, PartSlot(0), 0, &env.cat);
    b.phase[0] = BodyPhase::Cruise;
    let mut tick = 0;
    for _ in 0..60 * 10 {
        let aim = b.pos[0] + DVec3::new(500.0, -20.0, 0.0);
        let los = (aim - b.pos[0]).normalize();
        b.steer[0] = Steer::Accel((los * 270.0 - b.vel[0]) * 3.0);
        let out = run(&mut b, &mut d, &env, &mut tick, &[], 1);
        assert!(out.is_empty(), "{out:?} at tick {tick}");
        let p = b.pos[0];
        assert!(p.y - env.waves.height_at(p.x, p.z, tick as f64 * DT) > 0.5);
    }
}

/// An air burst kills hostile air bodies only: a torpedo of the same side as the target, inside the lethal
/// radius but under the sea, is untouched.
#[test]
fn air_burst_spares_bodies_that_are_not_air_targets() {
    let env = Env::new();
    let mut b = Bodies::default();
    let mut d = Decoys::default();
    b.insert(eid(1), SAM, SideId(0), DVec3::new(0.0, 3.0, 0.0), DVec3::new(600.0, 0.0, 0.0), EntityId::NONE, PartSlot(0), 0, &env.cat);
    b.insert(eid(2), ASM, SideId(1), DVec3::new(0.0, 4.0, 2.0), DVec3::new(270.0, 0.0, 0.0), EntityId::NONE, PartSlot(0), 0, &env.cat);
    b.insert(eid(3), TORP, SideId(1), DVec3::new(0.0, -3.0, 0.0), DVec3::new(20.0, 0.0, 0.0), EntityId::NONE, PartSlot(0), 0, &env.cat);
    for r in 0..3 { b.phase[r] = BodyPhase::Cruise; }
    b.steer[0] = Steer::Detonate;
    let mut out = Vec::new();
    b.step(&mut d, &env.ctx(0), &[], &[], &mut out);
    let [BodyOutcome::AirBurst { body, killed, .. }] = out.as_slice() else { panic!("{out:?}") };
    assert_eq!((*body, killed.as_slice()), (eid(1), [eid(2)].as_slice()));
    assert_ne!(b.phase[b.row_of(eid(3)).unwrap()], BodyPhase::Dead);
}

/// A RHIB at rest sets off toward a waypoint due north (its default heading), stops near it, and comes home.
#[test]
fn rhib_starts_from_rest_holds_station_and_is_recovered() {
    let env = Env::new();
    let mut b = Bodies::default();
    let mut d = Decoys::default();
    let ship = hull(1, DVec3::new(-20.0, -3.0, -5.0), core::f64::consts::FRAC_PI_2, DVec3::new(40.0, 10.0, 10.0));
    b.insert(eid(6), RHIB, SideId(0), DVec3::new(0.0, 0.0, -100.0), DVec3::ZERO, eid(1), PartSlot(1), 0, &env.cat);
    b.cleared[0] = true;
    b.steer[0] = Steer::Waypoint(DVec3::new(0.0, 0.0, -600.0));
    let mut tick = 0;
    let out = run(&mut b, &mut d, &env, &mut tick, &[ship], 60 * 90);
    assert!(out.is_empty(), "{out:?}");
    assert!((b.pos[0] - DVec3::new(0.0, 0.0, -600.0)).length() < 5.0, "{:?}", b.pos[0]);
    assert!(b.vel[0].length() < 1.5);
    b.steer[0] = Steer::Waypoint(DVec3::new(0.0, 0.0, 0.0));
    let out = run(&mut b, &mut d, &env, &mut tick, &[ship], 60 * 120);
    assert!(matches!(out.as_slice(), [BodyOutcome::Recovered { body, .. }] if *body == eid(6)), "{out:?}");
}

#[test]
fn range_that_ends_short_of_a_hull_expires_and_range_that_reaches_it_hits() {
    // L584: missile ranges are kinematic. A missile whose range ends 200 m short of a hull expires there with
    // no hit; with 200 m to spare it hits. (Verifier kb1's probe.)
    let env = Env::new();
    let target = hull(2, DVec3::new(2_000.0, -3.0, 40.0), 0.0, DVec3::new(80.0, 14.0, 12.0));
    for (left, want_hit) in [(1_800.0, false), (2_200.0, true)] {
        let mut b = Bodies::default();
        let mut d = Decoys::default();
        b.insert(eid(10), ASM, SideId(0), DVec3::new(0.0, 6.0, 0.0), DVec3::new(270.0, 0.0, 0.0), EntityId::NONE, PartSlot(2), 0, &env.cat);
        b.phase[0] = BodyPhase::Cruise;
        b.remaining[0] = left;
        let mut tick = 0;
        let out = run(&mut b, &mut d, &env, &mut tick, &[target], 900);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(matches!(out[0], BodyOutcome::HullHit { .. }), want_hit, "range left {left}: {out:?}");
        if !want_hit { assert!(matches!(out[0], BodyOutcome::Expired { shot_down: false, .. }), "{out:?}"); }
    }
}

#[test]
fn a_torpedo_holding_its_course_past_a_decoy_strikes_the_hull_200_m_on() {
    // Guidance holds a torpedo whose seeker is Lost on its course for 1,500 m before it orders `Steer::Ditch`
    // (`guidance::missile::LOST_CARRY_M`). Under `Steer::Hold` the torpedo runs the 200 m from a towed decoy to
    // the towing hull and strikes it; the same torpedo ordered to ditch at the decoy ends there with no hit.
    let env = Env::new();
    let target = hull(2, DVec3::new(200.0, -5.0, 30.0), 0.0, DVec3::new(80.0, 14.0, 12.0));
    for (ditch, want_hit) in [(false, true), (true, false)] {
        let mut b = Bodies::default();
        let mut d = Decoys::default();
        b.insert(eid(11), TORP, SideId(0), DVec3::new(0.0, -3.0, 0.0), DVec3::new(15.0, 0.0, 0.0), EntityId::NONE, PartSlot(0), 0, &env.cat);
        b.phase[0] = BodyPhase::Cruise;
        b.steer[0] = if ditch { Steer::Ditch } else { Steer::Hold };
        let mut tick = 0;
        let out = run(&mut b, &mut d, &env, &mut tick, &[target], 60 * 20);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(matches!(out[0], BodyOutcome::HullHit { .. }), want_hit, "ditch {ditch}: {out:?}");
    }
}
