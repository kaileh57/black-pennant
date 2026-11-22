//! Tests of the world units: the tick, queries, keyframes, and the end-to-end gunnery battle.
use super::*;
use crate::hash::{state_hash, subsystem_hashes, SubsystemHashes};
use crate::parts::{MountOrder, PartControl, PartOrder, PartState};
use crate::time::DT;
use crate::vessel::{build_prefab, VesselStatus};
use content::blueprint::{Blueprint, PartPlacement};
use content::cell::{Cell, CellGrid, CellPos, CELL_M};
use content::hull::{Hull, HullSource};
use content::ids::PartId;
use content::weather::SeaState;
use glam::DQuat;

// ---------------------------------------------------------------------------------------------------------
// Fixtures: starter-style hulls built from the default 22 m shell.

const STEEL: Cell = Cell { material: content::cell::Material::Steel, armour: content::cell::ArmourTier::None, paint: 0, shape: content::cell::CellShape::CUBE };
/// Cells the default shell is lifted by, so a propeller and rudder fit under the stern.
const LIFT: i16 = 2;
/// Slots of the parts every fixture hull carries, in placement order.
const ENGINE: u16 = 0;
const GUN: u16 = 8;

/// The default 22 m shell, lifted two cells, with two transverse bulkheads (three compartments).
fn shell() -> CellGrid {
    let src = CellGrid::from_rows(&content::blueprint::io::new_blueprint("shell", 22.0).cells);
    assert_eq!(src.size, [44, 8, 12], "these fixtures place parts by cell in content's default 22 m shell; if that shell changed, move the placements in `hull`");
    let mut g = CellGrid::new([src.size[0], src.size[1] + LIFT as u16, src.size[2]]);
    for y in 0..src.size[1] as i16 { for z in 0..src.size[2] as i16 { for x in 0..src.size[0] as i16 {
        let c = src.get(CellPos::new(x, y, z));
        if !c.is_empty() { g.set(CellPos::new(x, y + LIFT, z), c); }
    } } }
    for x in [14i16, 26] {
        for y in (1 + LIFT)..=(4 + LIFT) {
            let solid: Vec<i16> = (0..g.size[2] as i16).filter(|&z| !g.get(CellPos::new(x, y, z)).is_empty()).collect();
            let (Some(&lo), Some(&hi)) = (solid.first(), solid.last()) else { continue };
            for z in lo..=hi { if g.get(CellPos::new(x, y, z)).is_empty() { g.set(CellPos::new(x, y, z), STEEL); } }
        }
    }
    g
}

fn place(bp: &mut Blueprint, part: &str, pos: [i16; 3]) {
    bp.parts.push(PartPlacement { part: PartId::new(part), pos: CellPos::new(pos[0], pos[1], pos[2]), rot: Default::default(), kit: None, label: None, load: None });
}

/// A fitted hull: diesel, propeller, rudder, generator, fuel, bridge console, berths and a pump, then the gun
/// (slot [`GUN`]) with whatever feeds it.
fn hull(name: &str, gun: &str) -> Blueprint {
    let mut bp = content::blueprint::io::new_blueprint(name, 22.0);
    bp.hull = Hull { source: HullSource::Freeform, planes: Vec::new(), design_waterline_y: 3 + LIFT };
    bp.cells = shell().to_rows();
    bp.header.hash = content::ids::ContentHash::EMPTY;
    place(&mut bp, "engine.diesel.small", [6, 4, 4]);
    place(&mut bp, "prop.small", [2, 0, 5]);
    place(&mut bp, "rudder.small", [0, 0, 6]);
    place(&mut bp, "gen.diesel.small", [6, 4, 7]);
    place(&mut bp, "tank.fuel.small", [10, 4, 3]);
    place(&mut bp, "console.bridge", [16, 4, 7]);
    place(&mut bp, "berths.crew", [16, 4, 3]);
    place(&mut bp, "pump.small", [30, 4, 5]);
    match gun {
        "gun.76mm.single" => {
            place(&mut bp, gun, [19, 8, 3]);
            place(&mut bp, "magazine.76mm", [21, 4, 5]);
            place(&mut bp, "hoist.ammunition", [24, 4, 6]);
        }
        // A missile boat: box launcher, chaff, towed decoy and a mine rail in slots 8 to 11.
        "missiles" => {
            place(&mut bp, "launcher.ashm.light", [3, 8, 2]);
            place(&mut bp, "decoy.chaff", [14, 8, 8]);
            place(&mut bp, "decoy.towed", [1, 8, 7]);
            place(&mut bp, "rail.mine", [14, 8, 2]);
        }
        _ => place(&mut bp, gun, [20, 8, 5]),
    }
    bp.header.hash = content::blueprint::io::compute_hash(&bp);
    bp
}

fn catalogue() -> Arc<Catalogue> { Arc::new(content::catalogue::catalogue().clone()) }

fn sea(state: SeaState) -> WeatherState { WeatherState { sea_state: state, ..WeatherState::default() } }

fn world_on(cat: &Arc<Catalogue>, state: SeaState) -> World {
    World::new(WorldConfig { seed: 0xB1AC_4E77, weather: sea(state), map: MapDef { size_m: [20_000.0, 20_000.0], depth_m: 200.0, ..MapDef::default() }, catalogue: cat.clone() })
}

fn spawn_at(prefab: &Arc<VesselPrefab>, side: u8, x: f64, z: f64, heading_rad: f64) -> VesselSpawn {
    VesselSpawn {
        prefab: prefab.clone(), side: SideId(side), player: None, class: None, pose: crate::math::Pose::from_heading(DVec3::new(x, 0.0, z), heading_rad), speed_mps: 0.0,
        damage: None, fuel_fraction: 1.0, ammo_fraction: 1.0, fidelity: Fidelity::Full, submersible: false,
    }
}

fn assert_finite(w: &World) {
    for (_, v) in w.vessels() {
        assert!(v.kin.pose.pos.is_finite() && v.kin.pose.rot.is_finite() && v.kin.lin_vel.is_finite() && v.kin.ang_vel.is_finite(), "kinematics of {:?}: {:?}", v.id, v.kin);
        assert!(v.mass.mass_kg.is_finite() && v.mass.com.is_finite(), "mass of {:?}", v.id);
        for c in &v.compartments.compartments { assert!(c.water_m3.is_finite() && c.fire.is_finite() && c.water_plane_y.is_finite(), "compartment {:?} of {:?}", c.id, v.id); }
        for p in &v.parts { assert!(p.health.is_finite(), "part {:?} of {:?}", p.slot, v.id); }
    }
    assert!(w.projectiles().pos.iter().chain(&w.projectiles().vel).all(|p| p.is_finite()), "projectiles");
    assert!(w.bodies().pos.iter().chain(&w.bodies().vel).all(|p| p.is_finite()), "bodies");
}

#[test]
fn a_fitted_hull_floats_upright_on_a_beaufort_4_sea() {
    let cat = catalogue();
    let prefab = Arc::new(build_prefab(&hull("Gunboat", "gun.76mm.single"), &cat));
    assert_eq!((prefab.parts.len(), prefab.compartments.compartments.len()), (11, 3));
    let mut w = world_on(&cat, SeaState::BEAUFORT_4);
    let id = w.spawn_vessel(&spawn_at(&prefab, 0, 100.0, -50.0, 0.3));
    assert_eq!(w.drain_events(), vec![PhysEvent::VesselSpawned { vessel: id, side: SideId(0) }]);
    // Spawned at its loaded equilibrium: the keel sits at the gauge's draft, give or take the sea under it.
    let eq = crate::hydro::equilibrium(&prefab.blocks, &w.vessel(id).unwrap().mass, &[], &prefab.compartments, &cat.tuning.buoyancy);
    assert!(eq.floats && (w.vessel(id).unwrap().kin.pose.pos.y + eq.draft_m).abs() < 1.0, "draft {} at y {}", eq.draft_m, w.vessel(id).unwrap().kin.pose.pos.y);
    let mut worst_heel = 0.0f64;
    for _ in 0..600 {
        w.step();
        worst_heel = worst_heel.max(w.vessel(id).unwrap().heel_trim().0.abs());
    }
    assert_finite(&w);
    let v = w.vessel(id).unwrap();
    assert_eq!((v.status, w.tick(), w.time_s()), (VesselStatus::Active, Tick(600), 10.0));
    assert!(worst_heel < 0.35 && v.heel_trim().1.abs() < 0.2, "rolled to {worst_heel} rad");
    assert!((v.kin.pose.pos.y + eq.draft_m).abs() < 1.5 && (v.kin.pose.pos.x - 100.0).abs() < 20.0 && (v.kin.pose.pos.z + 50.0).abs() < 20.0, "ended at {:?}", v.kin.pose.pos);
    assert!(v.compartments.compartments.iter().all(|c| c.water_m3 == 0.0) && v.grid.changes.is_empty());
    // Bounds are the whole grid box around the pose, and the work counters describe the tick.
    assert!(v.bounds.min.x < v.kin.pose.pos.x + 1e-9 && v.bounds.max.y - v.bounds.min.y > 5.0);
    let work = w.work();
    assert!(work.bodies == 1 && work.cuboids as usize == v.cuboids.len() && work.buoyancy_samples > 0 && work.compartments == 3 && work.conduit_segments > 0, "{work:?}");
    // Only link changes while the networks settle: nothing else happens to a hull at rest.
    assert!(w.drain_events().iter().all(|e| matches!(e, PhysEvent::LinkChanged { .. })));
}

// ---------------------------------------------------------------------------------------------------------
// The end-to-end battle: a 76 mm gunboat fires on a patrol boat 250 m off its beam for 60 s.

struct Battle { w: World, shooter: EntityId, target: EntityId, prefabs: Vec<Arc<VesselPrefab>> }

#[derive(Default, Debug)]
struct Tally { fired: u32, spawned: u32, impacts: u32, splashes: u32, cells_changed: u32, breaches: u32, parts_damaged: u32, lost: u32, splits: u32 }

fn battle(cat: &Arc<Catalogue>) -> Battle {
    let gunboat = Arc::new(build_prefab(&hull("Gunboat", "gun.76mm.single"), cat));
    let patrol = Arc::new(build_prefab(&hull("Patrol boat", "gun.20mm.single"), cat));
    let mut w = world_on(cat, SeaState::BEAUFORT_4);
    let shooter = w.spawn_vessel(&spawn_at(&gunboat, 0, 0.0, 0.0, 0.0));
    let target = w.spawn_vessel(&spawn_at(&patrol, 1, 250.0, 0.0, 0.0));
    Battle { w, shooter, target, prefabs: vec![gunboat, patrol] }
}

/// Elevation that carries a shell through a point `range` metres away and `rise` metres up, by bisection over
/// the simulation's own ballistic step.
fn solve_elevation(muzzle_mps: f64, drag_k: f64, range: f64, rise: f64) -> f64 {
    let height_at_range = |elev: f64| {
        let (mut p, mut v) = (DVec3::ZERO, DVec3::new(muzzle_mps * libm::cos(elev), muzzle_mps * libm::sin(elev), 0.0));
        for _ in 0..3_000 {
            let (np, nv) = crate::projectiles::ballistic_step(p, v, drag_k, DT);
            if np.x >= range { return p.y + (np.y - p.y) * (range - p.x) / (np.x - p.x); }
            p = np;
            v = nv;
        }
        f64::NEG_INFINITY
    };
    let (mut lo, mut hi) = (-0.3f64, 0.6f64);
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if height_at_range(mid) < rise { lo = mid } else { hi = mid }
    }
    0.5 * (lo + hi)
}

/// Lay the shooter's gun on the target's waterline amidships and hold fire permission.
fn lay_gun(b: &mut Battle, cat: &Catalogue) {
    let (Some(s), Some(t)) = (b.w.vessel(b.shooter), b.w.vessel(b.target)) else { return };
    let gun = &s.parts[GUN as usize];
    let content::part::PartFunction::Gun(def) = &cat.parts.get_index(gun.def.index()).unwrap().1.function else { panic!("slot {GUN} is not a gun") };
    let muzzle = s.to_world(gun.centre);
    let size = t.grid.size();
    // Walk the point of aim along the hull and up from the waterline with each round, as a gunner spreading
    // fire would: a gun this steady otherwise puts every round through the hole the first ones made.
    let PartState::Mount(mount) = &gun.state else { panic!("slot {GUN} has no mount state") };
    let along = ((mount.fired * 7) % 17) as f64 - 8.0;
    let up = ((mount.fired * 3) % 4) as f64 * 0.3;
    let aim = t.to_world(DVec3::new(size[0] as f64 * 0.5 * CELL_M + along, t.waterline_y + up, size[2] as f64 * 0.5 * CELL_M));
    let flat = DVec3::new(aim.x - muzzle.x, 0.0, aim.z - muzzle.z);
    let elev = solve_elevation(def.muzzle_mps, cat.band(def.band).drag_k, flat.length(), aim.y - muzzle.y);
    let dir = flat.normalize() * libm::cos(elev) + DVec3::Y * libm::sin(elev);
    let local = s.kin.pose.rot.inverse() * dir;
    let order = MountOrder { laid_rot: None, train_rad: libm::atan2(-local.z, local.x), elevation_rad: libm::asin(local.y.clamp(-1.0, 1.0)), fire: true, max_rounds: 0, aim_error_rad: [0.0; 2] };
    let n = s.parts.len();
    let controls = b.w.controls_mut(b.shooter).unwrap();
    controls.parts.resize(n, PartControl::default());
    controls.parts[GUN as usize].order = PartOrder::Mount(order);
}

fn tally(events: &[PhysEvent], target: EntityId, t: &mut Tally) {
    for e in events {
        match e {
            PhysEvent::MountFired { rounds, .. } => t.fired += *rounds as u32,
            PhysEvent::ProjectileSpawned(_) => t.spawned += 1,
            PhysEvent::ShellImpact { vessel, .. } if *vessel == target => t.impacts += 1,
            PhysEvent::Splash { .. } => t.splashes += 1,
            PhysEvent::CellsDamaged { vessel, changes, .. } if *vessel == target => t.cells_changed += changes.len() as u32,
            PhysEvent::Breach { vessel, .. } if *vessel == target => t.breaches += 1,
            PhysEvent::PartDamaged { vessel, .. } if *vessel == target => t.parts_damaged += 1,
            PhysEvent::VesselLost { .. } => t.lost += 1,
            PhysEvent::Split { .. } => t.splits += 1,
            _ => {}
        }
    }
}

/// Run the battle for `ticks`; returns the world, the event tally and the peak flood water in the target.
fn run_battle(ticks: u64) -> (Battle, Tally, f64) {
    let cat = catalogue();
    let mut b = battle(&cat);
    let mut t = Tally::default();
    let mut peak_water = 0.0f64;
    for k in 0..ticks {
        lay_gun(&mut b, &cat);
        b.w.step();
        tally(&b.w.drain_events(), b.target, &mut t);
        if k % 30 == 0 {
            assert_finite(&b.w);
            if let Some(v) = b.w.vessel(b.target) { peak_water = peak_water.max(v.compartments.compartments.iter().map(|c| c.water_m3).sum()); }
        }
    }
    assert_finite(&b.w);
    (b, t, peak_water)
}

fn diff(a: &SubsystemHashes, b: &SubsystemHashes) -> Vec<&'static str> {
    let list = |s: &SubsystemHashes| [s.entities, s.bodies, s.cells, s.compartments, s.networks_parts, s.projectiles, s.munitions, s.crew, s.environment];
    list(a).iter().zip(list(b)).zip(SubsystemHashes::NAMES).filter(|((x, y), _)| **x != *y).map(|(_, n)| n).collect()
}

/// The end-to-end test of the crate: two fitted hulls spawned from blueprints on a Beaufort 4 sea, one firing
/// at the other for 60 s of simulation.
#[test]
fn sixty_seconds_of_gunfire_hits_floods_and_repeats_exactly() {
    let pool = |threads: usize| rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
    let (first, tally_1, water) = pool(1).install(|| run_battle(60 * 60));
    assert_eq!((first.w.tick(), first.w.weather().sea_state), (Tick(3_600), SeaState::BEAUFORT_4));
    // Hits: the gun fired at its rate, the hoist kept it fed from the magazine, and most rounds struck.
    let content::part::PartFunction::Gun(gun) = &first.w.catalogue().parts.get_index(first.prefabs[0].parts[GUN as usize].def.index()).unwrap().1.function else { panic!("slot {GUN} is not a gun") };
    assert!(tally_1.fired as f64 >= 0.9 * gun.rof_rpm * gun.barrels as f64 && tally_1.fired == tally_1.spawned, "{tally_1:?}");
    assert!(tally_1.impacts * 2 >= tally_1.fired && tally_1.impacts + tally_1.splashes <= tally_1.fired, "{tally_1:?}");
    let shooter = first.w.vessel(first.shooter).unwrap();
    assert!(shooter.status == VesselStatus::Active && shooter.grid.solid_count == shooter.grid.original_count, "the shooter is untouched");
    let stowed = |parts: &[crate::parts::PartRuntime]| parts.iter().map(|p| match p.state { PartState::Magazine { rounds, .. } => rounds, _ => 0 }).sum::<u32>();
    assert!(stowed(&shooter.parts) < stowed(&first.prefabs[0].parts), "rounds came up from the magazine");
    // Damage: cells lost, compartments breached, parts hit, and the structure pass kept up.
    let target = first.w.vessel(first.target).unwrap();
    assert!(tally_1.cells_changed > 300 && tally_1.breaches > 5 && tally_1.parts_damaged > 0, "{tally_1:?}");
    assert!(target.cells_lost_fraction() > 0.05 && target.pending_destroyed.is_empty() && first.w.split_cursor == 0, "lost {}", target.cells_lost_fraction());
    assert!(target.compartments.openings.iter().any(|o| o.kind == crate::compartments::OpeningKind::Breach));
    // Flooding: holes at the waterline let the sea in.
    assert!(water > 1.0, "peak flood water {water} m3");
    // The same battle in a pool of another size ends in the same state, subsystem by subsystem.
    let (second, tally_2, _) = pool(3).install(|| run_battle(60 * 60));
    let (h1, h2) = (subsystem_hashes(&first.w), subsystem_hashes(&second.w));
    assert!(h1 == h2, "subsystems that differ between runs: {:?}", diff(&h1, &h2));
    assert_eq!(state_hash(&first.w), state_hash(&second.w));
    assert_eq!((tally_1.fired, tally_1.impacts, tally_1.cells_changed), (tally_2.fired, tally_2.impacts, tally_2.cells_changed));
}

// ---------------------------------------------------------------------------------------------------------
// Keyframes.

fn lookup(prefabs: &[Arc<VesselPrefab>]) -> impl Fn(ContentHash) -> Option<Arc<VesselPrefab>> + '_ { move |h| prefabs.iter().find(|p| p.hash == h).cloned() }

#[test]
fn a_keyframe_taken_mid_battle_restores_the_same_state() {
    let (mut b, tally, water) = run_battle(15 * 60);
    assert!(tally.impacts > 5 && water > 0.0, "the keyframe should hold damage and flood water: {tally:?}, {water} m3");
    let cat = b.w.config.catalogue.clone();
    let config = || WorldConfig { seed: 1, weather: sea(SeaState::CALM), map: b.w.config.map.clone(), catalogue: cat.clone() };
    let bytes = b.w.save_keyframe();
    let mut r = World::restore_keyframe(config(), &bytes, &lookup(&b.prefabs)).expect("restores");
    let (h0, h1) = (subsystem_hashes(&b.w), subsystem_hashes(&r));
    assert!(h0 == h1, "subsystems that differ after a restore: {:?}", diff(&h0, &h1));
    // The keyframe's seed and weather win over the config's.
    assert_eq!((r.seed(), r.weather().sea_state, r.tick()), (b.w.seed(), SeaState::BEAUFORT_4, b.w.tick()));
    // The rebuilt grid is the saved grid, derived parts included.
    let (v0, v1) = (b.w.vessel(b.target).unwrap(), r.vessel(b.target).unwrap());
    assert!(v0.grid.chunks == v1.grid.chunks && v0.grid.types == v1.grid.types && v0.grid.xline == v1.grid.xline && v0.grid.yline == v1.grid.yline && v0.grid.zline == v1.grid.zline);
    assert_eq!(v0.bounds, v1.bounds);
    // A second save of the restored world is byte-identical: nothing was lost or reordered.
    assert_eq!(r.save_keyframe(), bytes);
    // Both go on: the restored world keeps simulating (not bit-identically, by design) without trouble.
    for _ in 0..120 { r.step(); b.w.step(); }
    assert_finite(&r);
    let (p0, p1) = (b.w.vessel(b.target).unwrap().kin.pose.pos, r.vessel(b.target).unwrap().kin.pose.pos);
    assert!((p0 - p1).length() < 1.0, "the restored battle drifted from the original: {p0:?} vs {p1:?}");
    // An undamaged hull costs far less than a damaged one.
    let quiet = battle(&cat).w.save_keyframe();
    assert!(quiet.len() * 2 < bytes.len(), "{} vs {}", quiet.len(), bytes.len());
}

#[test]
fn a_keyframe_refuses_bad_input_without_panicking() {
    let (b, _, _) = run_battle(4 * 60);
    let cat = b.w.config.catalogue.clone();
    let config = || WorldConfig { seed: 1, weather: sea(SeaState::CALM), map: MapDef::default(), catalogue: cat.clone() };
    let bytes = b.w.save_keyframe();
    let find = lookup(&b.prefabs);
    assert!(matches!(World::restore_keyframe(config(), &[], &find), Err(KeyframeError::Corrupt(_))));
    assert!(matches!(World::restore_keyframe(config(), b"not a keyframe at all", &find), Err(KeyframeError::Corrupt(_))));
    let mut other = bytes.clone();
    other[4..8].copy_from_slice(&(KEYFRAME_FORMAT + 1).to_le_bytes());
    assert!(matches!(World::restore_keyframe(config(), &other, &find), Err(KeyframeError::Format { found, expected }) if found == KEYFRAME_FORMAT + 1 && expected == KEYFRAME_FORMAT));
    assert!(matches!(World::restore_keyframe(config(), &bytes, &|_| None), Err(KeyframeError::MissingPrefab(_))));
    // A prefab of another design under the right hash: every array is checked against the grid it is given.
    let raft = Arc::new(build_prefab(&content::blueprint::io::new_blueprint("raft", 12.0), &cat));
    assert!(matches!(World::restore_keyframe(config(), &bytes, &|_| Some(raft.clone())), Err(KeyframeError::Corrupt(_))));
    // Truncations and bit flips either restore or fail cleanly; a restored world must still step.
    for cut in [8usize, 9, 40, bytes.len() / 3, bytes.len() / 2, bytes.len() - 1] {
        assert!(World::restore_keyframe(config(), &bytes[..cut], &find).is_err(), "truncated at {cut}");
    }
    let (mut restored, mut refused) = (0, 0);
    for k in 0..1_500u64 {
        let mut bad = bytes.clone();
        let r = crate::rng::mix64(k);
        let at = 8 + (r as usize) % (bad.len() - 8);
        // Single bit flips, and whole bytes overwritten (0xFF runs make huge lengths and non-finite floats).
        if k % 3 == 0 { for b in bad.iter_mut().skip(at).take(1 + (r >> 40) as usize % 8) { *b = 0xFF; } } else { bad[at] ^= 1 << (k % 8); }
        match World::restore_keyframe(config(), &bad, &find) {
            Ok(mut w) => { restored += 1; for _ in 0..3 { w.step(); } }
            Err(_) => refused += 1,
        }
    }
    assert!(restored > 0 && refused > 0, "{restored} restored, {refused} refused");
}

// ---------------------------------------------------------------------------------------------------------
// Structure, small bodies, spawning and inputs.

fn gunboat(cat: &Arc<Catalogue>) -> Arc<VesselPrefab> { Arc::new(build_prefab(&hull("Gunboat", "gun.76mm.single"), cat)) }

/// Every live cell of a hull with `x` in the given range.
fn slice(v: &Vessel, xs: core::ops::RangeInclusive<i16>) -> Vec<CellIndex> { v.grid.solid_cells().into_iter().filter(|c| xs.contains(&v.grid.pos(*c).x)).collect() }

/// Destroy cells outright through the damage unit, as a blast would between two ticks.
fn smash(w: &mut World, id: EntityId, cells: &[CellIndex]) { batter(w, id, cells, 8) }

/// Put `buckets` of damage on each cell through the damage unit.
fn batter(w: &mut World, id: EntityId, cells: &[CellIndex], buckets: u8) {
    let cat = w.config.catalogue.clone();
    let (waves, weather, terrain) = (w.waves, w.weather, w.terrain.clone());
    let ctx = StepContext { seed: w.config.seed, tick: w.tick, dt: DT, time_s: w.time_s(), catalogue: &cat, tuning: &cat.tuning, waves: &waves, weather: &weather, terrain: &terrain };
    let blast: Vec<crate::armour::CellDamage> = cells.iter().map(|&cell| crate::armour::CellDamage { cell, buckets }).collect();
    crate::damage::apply_cells(w.vessels.get_mut(id).unwrap(), &blast, crate::damage::DamageCause::CookOff, &ctx);
}

#[test]
fn a_hull_cut_in_two_splits_into_a_wreck_that_the_wreck_rules_remove() {
    let mut cat = content::catalogue::catalogue().clone();
    cat.tuning.structure.wreck_ttl_s = 2.0;
    let cat = Arc::new(cat);
    let prefab = gunboat(&cat);
    let mut w = world_on(&cat, SeaState::BEAUFORT_4);
    let id = w.spawn_vessel(&spawn_at(&prefab, 0, 0.0, 0.0, 1.0));
    for _ in 0..30 { w.step(); }
    w.drain_events();
    let cut = slice(w.vessel(id).unwrap(), 28..=29);
    let before = w.vessel(id).unwrap().grid.solid_count;
    smash(&mut w, id, &cut);
    w.step();
    let events = w.drain_events();
    let (piece, cells) = events.iter().find_map(|e| match e { PhysEvent::Split { vessel, piece, cells } if *vessel == id => Some((*piece, cells.clone())), _ => None }).expect("the bow splits off");
    assert_eq!(w.entities().kind(piece), Some(crate::entity::EntityKind::Wreck));
    let (hull, wreck) = (w.vessel(id).unwrap(), w.vessel(piece).unwrap());
    assert_eq!((wreck.status, wreck.parent, wreck.spawn_tick), (VesselStatus::Wreck, Some(id), 30));
    assert_eq!(wreck.grid.solid_count as usize, cells.len());
    assert_eq!(hull.grid.solid_count + wreck.grid.solid_count + cut.len() as u32, before, "every cell is on one body or gone");
    assert!(hull.pending_destroyed.is_empty() && hull.grid.dirty_chunks.count_ones() == 0, "the parent's collider and blocks were rebuilt with the split");
    assert!(hull.mass.mass_kg + wreck.mass.mass_kg < prefab.dry_mass.mass_kg + 5_000.0 && wreck.mass.mass_kg > 1_000.0);
    // A piece's bounds are tight around its cells; the hull's are its whole grid box.
    let (pb, hb) = (wreck.bounds.max - wreck.bounds.min, hull.bounds.max - hull.bounds.min);
    assert!(pb.x.max(pb.z) < 10.0 && hb.x.max(hb.z) > 18.0, "piece {pb:?} hull {hb:?}");
    assert!(w.locate(piece).is_some() && w.work().collider_rebuilds >= 1 && w.work().split_cells > 0);
    // Both halves stay in the simulation; the open ends flood.
    for _ in 0..130 { w.step(); }
    assert_finite(&w);
    assert!(w.vessel(id).unwrap().compartments.compartments.iter().any(|c| c.water_m3 > 0.0));
    // 2 s after the split the wreck rules take the piece: named this tick, gone the next.
    let events = w.drain_events();
    assert!(events.iter().any(|e| matches!(e, PhysEvent::WreckDespawned { wreck } if *wreck == piece)), "wreck despawn event");
    for _ in 0..30 { w.step(); }
    assert!(w.vessel(piece).is_none() && !w.entities().is_alive(piece) && w.vessel(id).is_some());
    assert_eq!(w.physics.counts().0, 1, "the wreck's rigid body left with it");
}

#[test]
fn a_missile_strikes_a_hull_and_its_entity_is_freed_on_the_next_tick() {
    let cat = catalogue();
    let prefab = gunboat(&cat);
    let mut w = world_on(&cat, SeaState::CALM);
    let ship = w.spawn_vessel(&spawn_at(&prefab, 1, 0.0, 0.0, 0.0));
    // Heading north the hull lies along -Z with its port side toward -X. Fly level at the engine room: through
    // the side plating and into the diesel, where the delayed fuse fires (two plates alone would not set it off).
    let kind = cat.munition_idx(&content::ids::MunitionId::new("missile.ashm.light")).unwrap();
    let missile = w.spawn_body(kind, SideId(0), DVec3::new(-200.0, 1.0, -4.0), DVec3::new(270.0, 0.0, 0.0));
    w.steer_body(missile, Steer::Accel(DVec3::ZERO));
    assert_eq!(w.locate(missile).map(|l| l.2), Some(SideId(0)));
    let mut events = Vec::new();
    for _ in 0..60 {
        w.step();
        events.extend(w.drain_events());
        if events.iter().any(|e| matches!(e, PhysEvent::BodyImpact { .. })) { break; }
    }
    let impact = events.iter().find_map(|e| match e { PhysEvent::BodyImpact { body, vessel, cause, world_point, .. } => Some((*body, *vessel, *cause, *world_point)), _ => None }).expect("the missile hits");
    assert_eq!((impact.0, impact.1, impact.2), (missile, ship, crate::damage::DamageCause::Missile));
    assert!(impact.3.x.abs() < 8.0 && (impact.3.z + 4.0).abs() < 3.0, "impact at {:?}", impact.3);
    let changes: Vec<content::cell::CellChange> = events.iter().filter_map(|e| match e { PhysEvent::CellsDamaged { vessel, changes, attacker, .. } if *vessel == ship => { assert!(attacker.is_none()); Some(changes.clone()) } _ => None }).flatten().collect();
    assert!(changes.len() > 30 && changes.iter().any(|c| c.bucket == 8), "an 8-bucket burst with a 2 m radius inside the engine room: {} cells changed", changes.len());
    assert!(events.iter().any(|e| matches!(e, PhysEvent::PartDamaged { vessel, slot, .. } if *vessel == ship && *slot == PartSlot(ENGINE))), "the engine took the burst");
    assert!(events.iter().any(|e| matches!(e, PhysEvent::BodySpawned { body, .. } if *body == missile)));
    // The ended body still resolves for the layers above, and is gone after the next tick's first phase.
    assert!(w.locate(missile).is_some() && w.entities().is_alive(missile));
    w.step();
    assert!(w.locate(missile).is_none() && !w.entities().is_alive(missile) && w.bodies().is_empty());
    assert_finite(&w);
}

#[test]
fn queries_pick_cells_block_sight_and_sweep_mount_arcs() {
    let cat = catalogue();
    let prefab = gunboat(&cat);
    let mut w = world_on(&cat, SeaState::CALM);
    let a = w.spawn_vessel(&spawn_at(&prefab, 0, 0.0, 0.0, 0.0));
    let b = w.spawn_vessel(&spawn_at(&prefab, 1, 200.0, 0.0, 0.0));
    for _ in 0..5 { w.step(); }
    let va = w.vessel(a).unwrap();
    // A level ray from the west meets A's port side; the hit is a live cell at the reported distance.
    let from = DVec3::new(-50.0, 0.5, -11.0);
    let hit = w.pick(from, DVec3::X * 3.0, 1_000.0).expect("hits A");
    assert_eq!(hit.entity, a);
    assert!((hit.point - from).length() - hit.distance < 1e-9 && hit.distance > 48.0 && hit.distance < 53.0, "{hit:?}");
    assert!(va.grid.is_solid(va.grid.pos(hit.cell.unwrap())));
    // Straight down onto the gun: the cell belongs to the mount.
    let gun_top = va.to_world(va.parts[GUN as usize].centre) + DVec3::Y * 20.0;
    assert_eq!(w.pick(gun_top, -DVec3::Y, 100.0).and_then(|h| h.part), Some(PartSlot(GUN)));
    assert!(w.pick(from, DVec3::X, 10.0).is_none() && w.pick(from, DVec3::ZERO, 100.0).is_none() && w.pick(DVec3::NAN, DVec3::X, 100.0).is_none());
    // A sight line through A is blocked unless A is ignored; one over its mast top is clear.
    let (p, q) = (DVec3::new(-50.0, 0.5, -11.0), DVec3::new(60.0, 0.5, -11.0));
    assert!(!w.line_of_sight(p, q, false, [EntityId::NONE; 2]));
    assert!(w.line_of_sight(p, q, false, [a, EntityId::NONE]) && w.line_of_sight(p + DVec3::Y * 30.0, q + DVec3::Y * 30.0, true, [EntityId::NONE; 2]));
    assert!(!w.line_of_sight(p, DVec3::new(f64::NAN, 0.0, 0.0), false, [a, b]));
    // Smoke blocks optical sight and nothing else (L604).
    let smoke = w.spawn_decoy(DecoyKind::Smoke, a, DVec3::new(100.0, 0.0, -11.0));
    for _ in 0..180 { w.step(); }
    let puff = w.decoys().pos[w.decoys().row_of(smoke).unwrap()];
    let (s0, s1) = (DVec3::new(puff.x - 40.0, 2.0, puff.z), DVec3::new(puff.x + 40.0, 2.0, puff.z));
    assert!(!w.line_of_sight(s0, s1, true, [a, b]) && w.line_of_sight(s0, s1, false, [a, b]));
    assert!(w.line_of_sight(s0 + DVec3::Y * 40.0, s1 + DVec3::Y * 40.0, true, [a, b]), "over the top of the cloud");
    assert_eq!(w.locate(smoke).map(|l| l.2), Some(SideId(0)), "a decoy reads as its owner's side");
    // The gun's own path: clear to either beam and upward, blocked through its own deck and by the foredeck.
    let half_pi = core::f64::consts::FRAC_PI_2;
    assert!(World::mount_path_clear(&prefab, PartSlot(GUN), half_pi, 0.1, &cat) && World::mount_path_clear(&prefab, PartSlot(GUN), -half_pi, 0.1, &cat));
    assert!(World::mount_path_clear(&prefab, PartSlot(GUN), 0.0, 1.0, &cat));
    assert!(!World::mount_path_clear(&prefab, PartSlot(GUN), 0.0, -half_pi, &cat) && !World::mount_path_clear(&prefab, PartSlot(GUN), 0.0, -0.2, &cat));
    assert!(!World::mount_path_clear(&prefab, PartSlot(200), 0.0, 0.0, &cat) && !World::mount_path_clear(&prefab, PartSlot(GUN), f64::NAN, 0.0, &cat));
}

#[test]
fn saved_damage_spawns_with_the_hull_and_a_restore_undoes_it() {
    let cat = catalogue();
    let prefab = gunboat(&cat);
    let mut w = world_on(&cat, SeaState::BEAUFORT_4);
    // Two cells of side plating gone at the waterline of the forward compartment, a worn engine, half loads.
    let hole: Vec<CellIndex> = [[28i16, 4, 2], [29, 4, 2]].iter().map(|p| prefab.grid.index(CellPos::new(p[0], p[1], p[2])).unwrap()).collect();
    assert!(hole.iter().all(|c| prefab.grid.is_solid(prefab.grid.pos(*c))));
    let damage = content::save::HullDamage { cells: hole.iter().map(|&cell| content::cell::CellChange { cell, bucket: 8 }).collect(), parts: [(PartSlot(ENGINE), 0.4)].into_iter().collect(), jury_rigged: 0 };
    let spawn = VesselSpawn { damage: Some(damage), fuel_fraction: 0.5, ammo_fraction: 0.5, ..spawn_at(&prefab, 0, 0.0, 0.0, 0.0) };
    let id = w.spawn_vessel(&spawn);
    let events = w.drain_events();
    assert_eq!(events, vec![PhysEvent::VesselSpawned { vessel: id, side: SideId(0) }], "saved damage is starting state, not news");
    let v = w.vessel(id).unwrap();
    assert!(hole.iter().all(|c| v.grid.bucket(*c) == 8) && v.grid.changes.is_empty() && v.pending_destroyed.is_empty());
    assert!(v.compartments.openings.iter().any(|o| o.kind == crate::compartments::OpeningKind::Breach));
    assert_eq!((v.parts[ENGINE as usize].health, v.parts[ENGINE as usize].level), (0.4, content::enums::DamageLevel::Heavy));
    let loads = |parts: &[crate::parts::PartRuntime]| parts.iter().fold((0.0, 0u32), |(l, r), p| match p.state { PartState::FuelTank { litres } => (l + litres, r), PartState::Magazine { rounds, .. } => (l, r + rounds), PartState::Mount(m) => (l, r + m.ready_rounds), _ => (l, r) });
    let full = loads(&prefab.parts);
    assert!(full.0 > 0.0 && full.1 > 2, "the design carries fuel and rounds: {full:?}");
    let half = loads(&v.parts);
    assert!(half.0 == 0.5 * full.0 && half.1.abs_diff(full.1 / 2) <= 1, "half loads {half:?} of {full:?}");
    assert!(w.drain_cell_changes(id).is_empty() && w.drain_dirty_render_chunks(id).is_empty());
    // The hole sits at the calm waterline, most of it above: give the sea ten seconds to wash in.
    for _ in 0..600 { w.step(); }
    assert_finite(&w);
    let flooded: f64 = w.vessel(id).unwrap().compartments.compartments.iter().map(|c| c.water_m3).sum();
    assert!(flooded > 0.02, "the sea washes in through the saved hole: {flooded} m3");
    // Restore: the hull is as designed again, where it was.
    let at = w.vessel(id).unwrap().kin.pose.pos;
    w.restore_vessel(id);
    let v = w.vessel(id).unwrap();
    assert!(v.grid.solid_count == prefab.grid.solid_count && v.compartments == prefab.compartments && v.parts == prefab.parts && v.kin.pose.pos == at);
    assert_eq!(loads(&v.parts), full);
    for _ in 0..120 { w.step(); }
    let v = w.vessel(id).unwrap();
    assert!(v.status == VesselStatus::Active && v.compartments.compartments.iter().all(|c| c.water_m3 == 0.0));
    assert_finite(&w);
}

#[test]
fn fidelity_changes_keep_a_hull_where_it_is_and_inputs_reach_the_stores() {
    let cat = catalogue();
    let prefab = gunboat(&cat);
    let mut w = world_on(&cat, SeaState::BEAUFORT_4);
    let full = w.spawn_vessel(&spawn_at(&prefab, 0, 0.0, 0.0, 0.0));
    let slow = w.spawn_vessel(&VesselSpawn { fidelity: Fidelity::Full30, ..spawn_at(&prefab, 0, 0.0, 400.0, 0.0) });
    let far = w.spawn_vessel(&VesselSpawn { fidelity: Fidelity::Kinematic, speed_mps: 0.0, ..spawn_at(&prefab, 1, 0.0, 800.0, 0.0) });
    for id in [full, slow, far] { w.set_controls(id, VesselControls { throttle: 1.0, ..Default::default() }); }
    for _ in 0..600 { w.step(); }
    assert_finite(&w);
    let run = |w: &World, id: EntityId| { let v = w.vessel(id).unwrap(); (v.forward_speed(), v.kin.pose.pos, v.status) };
    let (v_full, _, _) = run(&w, full);
    let (v_slow, p_slow, _) = run(&w, slow);
    let (v_far, p_far, s_far) = run(&w, far);
    assert!(v_full > 3.0, "ten seconds at full ahead makes way: {v_full} m/s");
    // A 30 Hz hull and a kinematic hull keep pace with the full-rate one under the same orders.
    assert!((v_slow - v_full).abs() < 0.2 * v_full && (v_far - v_full).abs() < 0.35 * v_full, "full {v_full}, 30 Hz {v_slow}, kinematic {v_far}");
    assert!(p_slow.y.abs() < 4.0 && p_far.y.abs() < 4.0 && s_far == VesselStatus::Active);
    // Switching tiers moves nothing: pose within a tick's travel, speed carried over.
    w.set_fidelity(far, Fidelity::Full);
    w.set_fidelity(slow, Fidelity::Kinematic);
    w.step();
    let ((v1, p1, _), (v2, p2, _)) = (run(&w, far), run(&w, slow));
    assert!((p1 - p_far).length() < 0.5 && (v1 - v_far).abs() < 0.5, "kinematic to full: {p_far:?} -> {p1:?}, {v_far} -> {v1}");
    assert!((p2 - p_slow).length() < 0.5 && (v2 - v_slow).abs() < 0.5, "30 Hz to kinematic: {p_slow:?} -> {p2:?}, {v_slow} -> {v2}");
    for _ in 0..300 { w.step(); }
    assert_finite(&w);
    assert!(w.vessels().all(|(_, v)| v.status == VesselStatus::Active && v.kin.pose.pos.y.abs() < 4.0 && v.heel_trim().0.abs() < 0.6));
    // Plain inputs.
    w.set_struck(full, true);
    w.set_side(full, SideId(3));
    w.set_projectile_cap_rung(2);
    w.teleport(full, &Kinematics { pose: crate::math::Pose::from_heading(DVec3::new(5_000.0, -1.8, 5_000.0), 2.0), ..Default::default() });
    w.teleport(full, &Kinematics { pose: crate::math::Pose::new(DVec3::NAN, DQuat::IDENTITY), ..Default::default() });
    w.drain_events();
    w.step();
    let v = w.vessel(full).unwrap();
    assert!(v.status == VesselStatus::Disabled && v.side == SideId(3) && (v.kin.pose.pos - DVec3::new(5_000.0, -1.8, 5_000.0)).length() < 2.0 && v.bounds.min.x > 4_900.0);
    assert!(w.drain_events().iter().any(|e| matches!(e, PhysEvent::StatusChanged { vessel, status: VesselStatus::Disabled } if *vessel == full)));
    assert_eq!(w.projectile_cap, 1_000);
    // Weather: a new sea from the same seed, announced once.
    let calm = w.waves().significant_height();
    w.set_weather(sea(SeaState::BEAUFORT_7));
    w.set_weather(sea(SeaState::BEAUFORT_7));
    let events = w.drain_events();
    assert_eq!(events.iter().filter(|e| matches!(e, PhysEvent::WeatherChanged { .. })).count(), 1);
    // The next tick still runs on the old sea; the new one arrives over the blend (weather.rs).
    assert!(w.wave_blend.is_some() && w.waves().significant_height() == calm);
    // Despawn: gone at once, with one event; stale ids and other kinds are ignored.
    w.despawn(slow);
    w.despawn(slow);
    w.set_controls(slow, VesselControls::default());
    w.set_fidelity(slow, Fidelity::Full);
    assert_eq!(w.drain_events(), vec![PhysEvent::VesselDespawned { vessel: slow }]);
    assert!(w.vessel(slow).is_none() && w.controls_mut(slow).is_none() && w.locate(slow).is_none() && w.vessels().count() == 2);
    for _ in 0..10 { w.step(); }
    assert_finite(&w);
    // A new hull reuses the freed slot under a new generation; the stale id still names nothing.
    let reborn = w.spawn_vessel(&spawn_at(&prefab, 0, 3_000.0, 3_000.0, 0.0));
    assert_eq!(reborn.index, slow.index);
    assert_ne!(reborn.generation, slow.generation);
    assert!(w.vessel(slow).is_none(), "a stale id must not reach the slot's new occupant");
    assert_eq!(w.vessel(reborn).map(|v| v.id), Some(reborn));
}

#[test]
fn launchers_decoys_and_mine_rails_spawn_through_the_tick() {
    use crate::parts::LaunchAim;
    let cat = catalogue();
    let prefab = Arc::new(build_prefab(&hull("Missile boat", "missiles"), &cat));
    let (launcher, chaff, towed, rail) = (8u16, 9u16, 10u16, 11u16);
    let mut w = world_on(&cat, SeaState::CALM);
    let id = w.spawn_vessel(&spawn_at(&prefab, 2, 0.0, 0.0, 0.0));
    let n = prefab.parts.len();
    let order = |w: &mut World, slot: u16, o: PartOrder| {
        let c = w.controls_mut(id).unwrap();
        c.parts.resize(n, PartControl::default());
        c.parts[slot as usize].order = o;
    };
    w.set_controls(id, VesselControls { throttle: 0.5, ..Default::default() });
    for _ in 0..120 { w.step(); }
    w.drain_events();
    order(&mut w, launcher, PartOrder::Launch(LaunchAim::Bearing { heading_rad: 0.0 }));
    order(&mut w, chaff, PartOrder::FireDecoy);
    order(&mut w, towed, PartOrder::StreamDecoy(true));
    order(&mut w, rail, PartOrder::DropMine);
    let mut events = Vec::new();
    for _ in 0..5 { w.step(); events.extend(w.drain_events()); }
    let spawned: Vec<(EntityId, PartSlot, SideId)> = events.iter().filter_map(|e| match e { PhysEvent::BodySpawned { body, launcher, slot, side, .. } if *launcher == id => Some((*body, *slot, *side)), _ => None }).collect();
    assert_eq!(spawned.iter().map(|s| (s.1, s.2)).collect::<Vec<_>>(), vec![(PartSlot(launcher), SideId(2)), (PartSlot(rail), SideId(2))], "one missile, then one mine, in mount order");
    let (missile, mine) = (spawned[0].0, spawned[1].0);
    assert!(events.iter().any(|e| matches!(e, PhysEvent::MineLaid { mine: m, layer, .. } if *m == mine && *layer == id)));
    let decoys: Vec<(EntityId, DecoyKind)> = events.iter().filter_map(|e| match e { PhysEvent::DecoyDeployed { decoy, kind, owner, .. } if *owner == id => Some((*decoy, *kind)), _ => None }).collect();
    assert_eq!(decoys.iter().map(|d| d.1).collect::<Vec<_>>(), vec![DecoyKind::Chaff, DecoyKind::Chaff, DecoyKind::TowedDecoy]);
    assert_eq!((w.bodies().len(), w.decoys().len()), (2, 3));
    // The launch points the missile along the ordered bearing until guidance takes it.
    let row = w.bodies().row_of(missile).unwrap();
    assert!(matches!(w.bodies().steer[row], Steer::Waypoint(p) if p.z < -1_000.0 && p.x.abs() < 50.0), "{:?}", w.bodies().steer[row]);
    // One-shot orders were consumed; the towed decoy's order holds.
    let controls = &w.vessel(id).unwrap().controls;
    assert!(controls.parts[launcher as usize].order == PartOrder::None && controls.parts[rail as usize].order == PartOrder::None && controls.parts[towed as usize].order == PartOrder::StreamDecoy(true));
    // Two seconds on: the missile has flown north, the mine is moored where the rail dropped it, the decoy tows behind.
    for _ in 0..120 { w.step(); }
    assert_finite(&w);
    let ship = w.vessel(id).unwrap().kin.pose.pos;
    let (m, mn, tow) = (w.locate(missile).unwrap().0, w.locate(mine).unwrap().0, w.locate(decoys[2].0).unwrap());
    assert!(m.z < ship.z - 200.0 && m.y > 1.0, "missile at {m:?}, ship at {ship:?}");
    assert!((mn.y + crate::munitions::MINE_DEPTH_M).abs() < 0.5 && (mn - ship).length() < 40.0, "mine at {mn:?}, ship at {ship:?}");
    assert!(tow.0.z > ship.z + 100.0 && tow.2 == SideId(2), "towed decoy at {:?}", tow.0);
    // Hauling in the decoy: announced on the tick the part stops streaming, freed on the next.
    w.drain_events();
    order(&mut w, towed, PartOrder::StreamDecoy(false));
    w.step();
    assert!(w.drain_events().iter().any(|e| matches!(e, PhysEvent::DecoyExpired { decoy } if *decoy == decoys[2].0)));
    assert!(w.entities().is_alive(decoys[2].0));
    w.step();
    assert!(!w.entities().is_alive(decoys[2].0) && w.decoys().len() == 2 && w.drain_events().iter().all(|e| !matches!(e, PhysEvent::DecoyExpired { .. })));
}

#[test]
fn far_hulls_float_on_coarse_blocks_when_the_fallback_is_on() {
    let run = |far: bool| {
        let mut cat = content::catalogue::catalogue().clone();
        cat.tuning.buoyancy.far_blocks_enabled = far;
        let cat = Arc::new(cat);
        let prefab = gunboat(&cat);
        let mut w = world_on(&cat, SeaState::BEAUFORT_4);
        let near = w.spawn_vessel(&VesselSpawn { player: Some(content::ids::PlayerSlot(0)), ..spawn_at(&prefab, 0, 0.0, 0.0, 0.0) });
        let distant = w.spawn_vessel(&spawn_at(&prefab, 1, 3_000.0, 0.0, 0.0));
        for _ in 0..300 { w.step(); }
        assert_finite(&w);
        let coarse = |id: EntityId| w.aux.get(id).and_then(|a| a.far_blocks.as_ref().map(|b| b.block_cells));
        (coarse(near), coarse(distant), w.vessel(distant).unwrap().kin.pose, w.vessel(distant).unwrap().status)
    };
    let (fine, coarse) = (run(false), run(true));
    assert_eq!((fine.0, fine.1), (None, None));
    assert_eq!((coarse.0, coarse.1), (None, Some(8)), "only the hull beyond 2 km of every player hull uses 4 m blocks");
    assert_eq!(coarse.3, VesselStatus::Active);
    // 4 m blocks are a coarse fit to a 5 m deep hull: it still floats upright, within a block's error of its draft.
    let (heel, trim) = crate::hydro::heel_trim(coarse.2.rot);
    assert!((fine.2.pos.y - coarse.2.pos.y).abs() < 1.5 && heel.abs() < 0.3 && trim.abs() < 0.3, "draft on 2 m blocks {} and on 4 m blocks {}, heel {heel}, trim {trim}", fine.2.pos.y, coarse.2.pos.y);
}

#[test]
fn a_magazine_fire_cooks_off_and_a_loss_soon_after_is_put_down_to_it() {
    let mut cat = content::catalogue::catalogue().clone();
    cat.tuning.fire.cookoff_delay_s = 0.5;
    cat.tuning.stability.capsize_hold_s = 0.1;
    cat.tuning.structure.wreck_ttl_s = 1.0;
    let cat = Arc::new(cat);
    let prefab = gunboat(&cat);
    let mut w = world_on(&cat, SeaState::CALM);
    let id = w.spawn_vessel(&spawn_at(&prefab, 0, 0.0, 0.0, 0.0));
    let other = w.spawn_vessel(&spawn_at(&prefab, 1, 500.0, 0.0, 0.0));
    for _ in 0..10 { w.step(); }
    w.drain_events();
    let magazine = prefab.parts.iter().find(|p| matches!(p.state, PartState::Magazine { .. })).unwrap();
    let room = magazine.compartment.expect("the magazine sits in a compartment");
    assert!(crate::fire::ignite(w.vessels.get_mut(id).unwrap(), room, &cat.tuning.fire));
    let mut events = Vec::new();
    for _ in 0..120 { w.step(); events.extend(w.drain_events()); }
    assert_finite(&w);
    let at = |pred: &dyn Fn(&PhysEvent) -> bool| events.iter().position(pred);
    // r = clamp(4 + floor(cbrt(stored ammunition kg)), 4, 24) cells (L666): 16 for the 76 mm magazine's 1.8 t.
    let PartState::Magazine { ammo_kg, .. } = magazine.state else { unreachable!() };
    let radius = crate::fire::cookoff_radius_cells(ammo_kg, cat.tuning.fire.cookoff_r_min_cells, cat.tuning.fire.cookoff_r_max_cells);
    assert!(radius >= 8, "a stocked magazine: {ammo_kg} kg, {radius} cells");
    let blast = at(&|e| matches!(e, PhysEvent::CookOff { vessel, magazine: m, radius_cells, .. } if *vessel == id && *m == magazine.slot && *radius_cells == radius)).expect("the magazine cooks off");
    let cells = at(&|e| matches!(e, PhysEvent::CellsDamaged { vessel, cause: crate::damage::DamageCause::CookOff, changes, .. } if *vessel == id && changes.len() > 1_000)).expect("the blast removes everything within its radius");
    assert!(blast < cells, "the cook-off is announced before its damage");
    // What is left is a stern stub, still one body and still afloat, with its structure work done.
    let v = w.vessel(id).unwrap();
    assert!(!matches!(v.status, VesselStatus::Lost(_)) && v.cells_lost_fraction() > 0.5 && v.pending_destroyed.is_empty(), "{:?}, lost {}", v.status, v.cells_lost_fraction());
    // A checkpoint taken now holds the gutted hull, its recomputed compartments and its dead parts.
    let bytes = w.save_keyframe();
    let r = World::restore_keyframe(WorldConfig { seed: 0, weather: sea(SeaState::BEAUFORT_7), map: w.config.map.clone(), catalogue: cat.clone() }, &bytes, &|h| (h == prefab.hash).then(|| prefab.clone())).expect("a hull gutted by a cook-off restores");
    assert!(subsystem_hashes(&w) == subsystem_hashes(&r) && r.save_keyframe() == bytes);
    // Both hulls go over (held upside down past the capsize time): one loss follows a cook-off, one does not.
    let mut events = Vec::new();
    for _ in 0..12 {
        for hull in [id, other] {
            let kin = w.vessel(hull).unwrap().kin;
            let over = crate::math::Pose::new(kin.pose.pos, DQuat::from_xyzw(1.0, 0.0, 0.0, 0.0));
            w.teleport(hull, &Kinematics { pose: over, ..Default::default() });
        }
        w.step();
        events.extend(w.drain_events());
    }
    let lost = |hull: EntityId| events.iter().find_map(|e| match e { PhysEvent::VesselLost { vessel, cause, by, side, tonnage_t } if *vessel == hull => Some((*cause, *by, *side, *tonnage_t > 40.0)), _ => None });
    assert_eq!(lost(id), Some((crate::vessel::LossCause::CookOff, EntityId::NONE, SideId(0), true)));
    // (Upside down a hull's top deck is under water, so the intact one reads as sunk or capsized, whichever its
    // status test meets first; what matters is that no magazine is blamed.)
    assert!(matches!(lost(other), Some((crate::vessel::LossCause::Capsized | crate::vessel::LossCause::Sunk, by, SideId(1), true)) if by.is_none()), "{:?}", lost(other));
    assert_eq!(w.vessel(id).unwrap().status, VesselStatus::Lost(crate::vessel::LossCause::CookOff));
    // A lost hull follows the wreck rules from the tick it was lost: named, then freed a tick later.
    let mut events = Vec::new();
    for _ in 0..70 { w.step(); events.extend(w.drain_events()); }
    assert!([id, other].iter().all(|h| events.iter().any(|e| matches!(e, PhysEvent::WreckDespawned { wreck } if wreck == h))));
    assert!(w.vessels().count() == 0 && w.entities().is_empty() && w.physics.counts().0 == 0);
    w.step();
    assert_finite(&w);
}

#[test]
fn ramming_and_grounding_crush_cells_and_report_once_per_contact() {
    let cat = catalogue();
    let prefab = gunboat(&cat);
    // One island 300 m across, 600 m to the north of the two hulls.
    let map = MapDef { size_m: [20_000.0, 20_000.0], depth_m: 200.0, islands: vec![content::campaign::Island { centre_m: [0.0, -600.0], radius_m: 150.0, height_m: 20.0, seed: 3 }], ..MapDef::default() };
    let mut w = World::new(WorldConfig { seed: 9, weather: sea(SeaState::CALM), map, catalogue: cat.clone() });
    // The victim lies across the rammer's bow, 40 m ahead of it; the rammer comes on at 10 m/s.
    let victim = w.spawn_vessel(&spawn_at(&prefab, 1, -8.0, -60.0, core::f64::consts::FRAC_PI_2));
    let rammer = w.spawn_vessel(&VesselSpawn { speed_mps: 10.0, ..spawn_at(&prefab, 0, 0.0, 0.0, 0.0) });
    w.set_controls(rammer, VesselControls { throttle: 1.0, ..Default::default() });
    let mut events = Vec::new();
    for _ in 0..(8 * 60) { w.step(); events.extend(w.drain_events()); }
    assert_finite(&w);
    let hits: Vec<f64> = events.iter().filter_map(|e| match e { PhysEvent::Collision { a, b, impulse, .. } if (*a, *b) == (victim, rammer) => Some(*impulse), _ => None }).collect();
    assert!(!hits.is_empty() && hits.len() < 60, "a collision is news when it starts or does damage, not on every tick of contact: {} events", hits.len());
    let crushed = |hull: EntityId, by: EntityId| events.iter().any(|e| matches!(e, PhysEvent::CellsDamaged { vessel, cause: crate::damage::DamageCause::Collision, attacker, .. } if *vessel == hull && *attacker == by));
    assert!(crushed(victim, rammer) && crushed(rammer, victim), "both hulls take crush damage, each credited to the other");
    assert_eq!(w.aux.get(victim).map(|a| a.last_attacker), Some(rammer));
    // A third hull runs straight at the island and takes the ground.
    let runner = w.spawn_vessel(&VesselSpawn { speed_mps: 10.0, ..spawn_at(&prefab, 0, -3.0, -300.0, 0.0) });
    w.set_controls(runner, VesselControls { throttle: 1.0, ..Default::default() });
    let mut events = Vec::new();
    for _ in 0..(40 * 60) {
        w.step();
        events.extend(w.drain_events());
        if events.iter().any(|e| matches!(e, PhysEvent::Grounded { vessel, .. } if *vessel == runner)) { break; }
    }
    let aground = events.iter().find_map(|e| match e { PhysEvent::Grounded { vessel, point } if *vessel == runner => Some(*point), _ => None }).expect("the hull grounds on the shelf");
    assert!(aground.z < -350.0 && aground.y < 1.0 && w.terrain().depth_at(aground.x, aground.z) < 4.0, "grounded at {aground:?}");
    for _ in 0..120 { w.step(); }
    assert_finite(&w);
    let v = w.vessel(runner).unwrap();
    assert!(v.kin.pose.pos.z > -600.0 && v.kin.lin_vel.length() < 10.0, "the shore stops it: {:?}", v.kin);
}

#[test]
fn repairs_join_the_damage_stream_and_a_restored_wreck_is_a_ship_again() {
    let mut cat = content::catalogue::catalogue().clone();
    cat.tuning.crew.repair_job_s = 0.5;
    cat.tuning.stability.capsize_hold_s = 0.1;
    cat.tuning.structure.wreck_ttl_s = 0.0;
    let cat = Arc::new(cat);
    let prefab = gunboat(&cat);
    let generator = PartSlot(3);
    let mut w = world_on(&cat, SeaState::CALM);
    let id = w.spawn_vessel(&spawn_at(&prefab, 0, 0.0, 0.0, 0.0));
    for _ in 0..5 { w.step(); }
    // Knock the generator about (4 buckets a cell: Damaged, half health), then send the team to it.
    let cells = prefab.parts[generator.index()].cells.clone();
    batter(&mut w, id, &cells, 4);
    assert_eq!(w.drain_cell_changes(id).len(), cells.len());
    w.drain_events();
    w.controls_mut(id).unwrap().teams = vec![crate::parts::TeamOrder::Repair(generator)];
    let mut events = Vec::new();
    for _ in 0..60 { w.step(); events.extend(w.drain_events()); }
    assert!(events.iter().any(|e| matches!(e, PhysEvent::JobDone { vessel, job: crate::crew::JobDone::PartRepaired(s) } if *vessel == id && *s == generator)));
    // The repair lowered the cells' buckets; remote clients hear of it through the same stream as damage.
    let stream: Vec<content::cell::CellChange> = events.iter().filter_map(|e| match e { PhysEvent::CellsDamaged { vessel, changes, cause: crate::damage::DamageCause::Fire, attacker } if *vessel == id && attacker.is_none() => Some(changes.clone()), _ => None }).flatten().collect();
    assert!(stream.len() == cells.len() && stream.iter().all(|c| c.bucket == 2 && cells.contains(&c.cell)), "{stream:?}");
    assert_eq!(w.drain_cell_changes(id), stream, "the local feed and the events carry the same changes");
    assert!(w.vessel(id).unwrap().parts[generator.index()].health > 0.7);
    // Lose the hull (rolled over and held); with no wreck time allowed it is named for removal at once.
    let mut events = Vec::new();
    for _ in 0..12 {
        let kin = w.vessel(id).unwrap().kin;
        w.teleport(id, &Kinematics { pose: crate::math::Pose::new(kin.pose.pos, DQuat::from_xyzw(1.0, 0.0, 0.0, 0.0)), ..Default::default() });
        w.step();
        events.extend(w.drain_events());
        if events.iter().any(|e| matches!(e, PhysEvent::WreckDespawned { .. })) { break; }
    }
    assert!(events.iter().any(|e| matches!(e, PhysEvent::VesselLost { vessel, .. } if *vessel == id)) && w.doomed == vec![id]);
    // A restore before the next tick takes it back off the list.
    let kin = w.vessel(id).unwrap().kin;
    w.restore_vessel(id);
    w.teleport(id, &Kinematics { pose: crate::math::Pose::from_heading(kin.pose.pos, 0.0), ..Default::default() });
    for _ in 0..60 { w.step(); }
    assert_finite(&w);
    assert!(w.vessel(id).is_some_and(|v| v.status == VesselStatus::Active && v.grid.solid_count == prefab.grid.solid_count), "{:?}", w.vessel(id).map(|v| v.status));
    assert!(w.entities().is_alive(id) && w.doomed.is_empty());
}

/// A world holding every kind of small thing: a ship, a missile in flight, three decoys and shells in the air.
fn busy_world() -> (World, Vec<Arc<VesselPrefab>>) {
    let (mut b, _, _) = run_battle(2 * 60);
    let cat = b.w.config.catalogue.clone();
    let kind = cat.munition_idx(&content::ids::MunitionId::new("missile.ashm.light")).unwrap();
    let missile = b.w.spawn_body(kind, SideId(0), DVec3::new(-3_000.0, 12.0, 400.0), DVec3::new(250.0, 0.0, 0.0));
    b.w.steer_body(missile, Steer::Waypoint(DVec3::new(5_000.0, 10.0, 400.0)));
    for (kind, x) in [(DecoyKind::Chaff, 60.0), (DecoyKind::Smoke, 90.0), (DecoyKind::Flare, 120.0)] { b.w.spawn_decoy(kind, b.shooter, DVec3::new(x, 15.0, 80.0)); }
    for k in 0..6 {
        b.w.projectiles.spawn(b.w.tick.0, DVec3::new(-2_000.0, 300.0, 50.0 * k as f64), DVec3::new(600.0, 120.0, 0.0), 76, content::part::weapons::CalibreBand::Medium, b.shooter, PartSlot(GUN), false, 2_000);
    }
    for _ in 0..30 { lay_gun(&mut b, &cat); b.w.step(); }
    assert!(b.w.bodies().len() == 1 && b.w.decoys().len() == 3 && b.w.projectiles().len() >= 6, "{} bodies, {} decoys, {} shells", b.w.bodies().len(), b.w.decoys().len(), b.w.projectiles().len());
    (b.w, b.prefabs)
}

#[test]
fn a_keyframe_with_ragged_or_broken_small_stores_is_refused_or_steps() {
    let (w, prefabs) = busy_world();
    let cat = w.config.catalogue.clone();
    let config = || WorldConfig { seed: 1, weather: sea(SeaState::CALM), map: w.config.map.clone(), catalogue: cat.clone() };
    let find = lookup(&prefabs);
    let good = w.save_keyframe();
    let mut r = World::restore_keyframe(config(), &good, &find).expect("restores");
    assert!(subsystem_hashes(&w) == subsystem_hashes(&r) && r.save_keyframe() == good);
    for _ in 0..30 { r.step(); }
    assert_finite(&r);
    // The world is taken apart column by column: a keyframe that arrives with one column short, one long, or a
    // value no simulation produces must be refused or restore to a world that steps.
    let try_it = |what: &str, break_it: &dyn Fn(&mut World)| {
        let mut bad = World::restore_keyframe(config(), &good, &find).expect("restores");
        break_it(&mut bad);
        match World::restore_keyframe(config(), &bad.save_keyframe(), &find) {
            Ok(mut w) => {
                for _ in 0..5 { w.step(); }
                let _ = (w.pick(DVec3::new(-3_000.0, 12.0, 0.0), DVec3::Z, 5_000.0), w.line_of_sight(DVec3::new(0.0, 5.0, 80.0), DVec3::new(200.0, 5.0, 80.0), true, [EntityId::NONE; 2]));
                for id in w.entities().iter().collect::<Vec<_>>() { let _ = w.locate(id); }
                assert_finite(&w);
                false
            }
            Err(KeyframeError::Corrupt(_)) => true,
            Err(e) => panic!("{what}: {e}"),
        }
    };
    macro_rules! columns {
        ($store:ident: $($col:ident),*) => { $(
            assert!(try_it(concat!(stringify!($store), ".", stringify!($col), " short"), &|w| { w.$store.$col.pop(); }), concat!(stringify!($store), ".", stringify!($col), " one short is refused"));
            assert!(try_it(concat!(stringify!($store), ".", stringify!($col), " long"), &|w| { let last = w.$store.$col.last().cloned().unwrap(); w.$store.$col.push(last); }), concat!(stringify!($store), ".", stringify!($col), " one long is refused"));
        )* };
    }
    columns!(bodies: entity, kind, side, pos, vel, hp, phase, steer, launcher, launcher_slot, spawn_tick, remaining, set_depth_m, radiating);
    columns!(decoys: entity, kind, owner, pos, age_s);
    // The private flag columns are padded by the stores themselves: short restores and steps, long is refused.
    assert!(!try_it("bodies.cleared short", &|w| { w.bodies.cleared.pop(); }) && try_it("bodies.cleared long", &|w| w.bodies.cleared.push(true)));
    assert!(!try_it("decoys.lost short", &|w| { w.decoys.lost.pop(); }) && try_it("decoys.lost long", &|w| w.decoys.lost.push(true)));
    columns!(projectiles: id, pos, vel, band, calibre_mm, shooter, mount, spawn_tick, v_1km, path_m, rounds, point_defence);
    // Values no simulation produces.
    assert!(try_it("body position", &|w| w.bodies.pos[0] = DVec3::new(f64::NAN, 0.0, 0.0)));
    assert!(try_it("body velocity", &|w| w.bodies.vel[0] = DVec3::new(0.0, f64::INFINITY, 0.0)));
    assert!(try_it("body range", &|w| w.bodies.remaining[0] = f64::NAN));
    // A kind this catalogue does not have is not refused: the store ends such a body on its first step.
    assert!(!try_it("body kind", &|w| w.bodies.kind[0] = MunitionIdx(u16::MAX)));
    assert!(try_it("decoy position", &|w| w.decoys.pos[1] = DVec3::splat(f64::NEG_INFINITY)));
    assert!(try_it("decoy age", &|w| w.decoys.age_s[0] = f64::NAN));
    assert!(try_it("shell position", &|w| w.projectiles.pos[0] = DVec3::splat(f64::NAN)));
    assert!(try_it("shell velocity", &|w| w.projectiles.vel[2] = DVec3::new(f64::INFINITY, 0.0, 0.0)));
    assert!(try_it("shell ids out of order", &|w| w.projectiles.id.swap(0, 1)));
    assert!(try_it("more shells than any cap", &|w| { let n = crate::projectiles::MAX_LIVE_HARD + 1; let p = &mut w.projectiles; p.id = (0..n as u32).map(crate::projectiles::ProjectileId).collect(); p.pos = vec![DVec3::Y * 50.0; n]; p.vel = vec![DVec3::X; n]; p.band = vec![p.band[0]; n]; p.calibre_mm = vec![76; n]; p.shooter = vec![EntityId::NONE; n]; p.mount = vec![PartSlot(0); n]; p.spawn_tick = vec![0; n]; p.v_1km = vec![0.0; n]; p.path_m = vec![0.0; n]; p.rounds = vec![1; n]; p.point_defence = vec![false; n]; p.next_id = n as u32; }));
    // Wave components, weather and the tick are data too.
    assert!(try_it("wave amplitude", &|w| w.waves.waves[0].amplitude = f64::NAN));
    // Random damage to the bytes of a keyframe that holds all of the above.
    let (mut restored, mut refused) = (0, 0);
    for k in 0..1_500u64 {
        let mut bad = good.clone();
        let r = crate::rng::mix64(k ^ 0x5EED);
        let at = 8 + (r as usize) % (bad.len() - 8);
        if k % 3 == 0 { for b in bad.iter_mut().skip(at).take(1 + (r >> 40) as usize % 8) { *b = 0xFF; } } else { bad[at] ^= 1 << (k % 8); }
        match World::restore_keyframe(config(), &bad, &find) {
            Ok(mut w) => { restored += 1; for _ in 0..3 { w.step(); } }
            Err(_) => refused += 1,
        }
    }
    assert!(restored > 0 && refused > 0, "{restored} restored, {refused} refused");
}

#[test]
fn a_keyframe_of_a_split_and_burning_hull_restores_with_its_wreck() {
    let cat = catalogue();
    let prefab = gunboat(&cat);
    let mut w = world_on(&cat, SeaState::BEAUFORT_4);
    let id = w.spawn_vessel(&spawn_at(&prefab, 0, 0.0, 0.0, 1.0));
    // A 30 Hz hull and a kinematic one under way ride along: every fidelity goes through the keyframe.
    for (fidelity, z) in [(Fidelity::Full30, 300.0), (Fidelity::Kinematic, 600.0)] {
        let hull = w.spawn_vessel(&VesselSpawn { fidelity, speed_mps: 4.0, ..spawn_at(&prefab, 1, 0.0, z, 0.0) });
        w.set_controls(hull, VesselControls { throttle: 0.6, rudder: 0.3, ..Default::default() });
    }
    for _ in 0..30 { w.step(); }
    let cut = slice(w.vessel(id).unwrap(), 28..=29);
    smash(&mut w, id, &cut);
    let room = prefab.parts[ENGINE as usize].compartment.expect("the engine sits in a compartment");
    assert!(crate::fire::ignite(w.vessels.get_mut(id).unwrap(), room, &cat.tuning.fire));
    // Saved on the tick of the split, with the piece just carved, and again once both halves have flooded.
    for ticks in [1, 240] {
        for _ in 0..ticks { w.step(); }
        assert_eq!(w.vessels().count(), 4, "the hull, its wreck piece and the two bystanders");
        assert!(w.vessels().any(|(_, v)| v.parent == Some(id) && v.parts.len() < prefab.parts.len()), "the piece carries only its own parts");
        let bytes = w.save_keyframe();
        let config = WorldConfig { seed: 0, weather: sea(SeaState::CALM), map: w.config.map.clone(), catalogue: cat.clone() };
        let mut r = World::restore_keyframe(config, &bytes, &|h| (h == prefab.hash).then(|| prefab.clone())).expect("a split hull restores");
        let (h0, h1) = (subsystem_hashes(&w), subsystem_hashes(&r));
        assert!(h0 == h1, "subsystems that differ after a restore: {:?}", diff(&h0, &h1));
        assert_eq!(r.save_keyframe(), bytes);
        assert_eq!(r.physics.counts().0, 4, "every body is back in the solver");
        for _ in 0..60 { r.step(); }
        assert_finite(&r);
        // The restored world goes on as the original does (not bit for bit, by design): nobody jumps.
        let mut o = World::restore_keyframe(WorldConfig { seed: 0, weather: sea(SeaState::CALM), map: w.config.map.clone(), catalogue: cat.clone() }, &bytes, &|h| (h == prefab.hash).then(|| prefab.clone())).expect("restores twice");
        for _ in 0..60 { o.step(); }
        assert_eq!(state_hash(&o), state_hash(&r), "two restores of one keyframe run identically");
    }
}

#[test]
fn a_keyframe_restores_on_every_sea_from_calm_to_beaufort_12() {
    let cat = catalogue();
    let prefab = gunboat(&cat);
    for code in 0..=8u8 {
        let mut w = world_on(&cat, SeaState(code));
        let id = w.spawn_vessel(&spawn_at(&prefab, 0, 0.0, 0.0, 0.5));
        w.set_controls(id, VesselControls { throttle: 0.5, ..Default::default() });
        for _ in 0..20 { w.step(); }
        let bytes = w.save_keyframe();
        let config = WorldConfig { seed: 0, weather: sea(SeaState::BEAUFORT_4), map: w.config.map.clone(), catalogue: cat.clone() };
        let mut r = World::restore_keyframe(config, &bytes, &|h| (h == prefab.hash).then(|| prefab.clone())).unwrap_or_else(|e| panic!("sea state {code}: {e}"));
        let (h0, h1) = (subsystem_hashes(&w), subsystem_hashes(&r));
        assert!(h0 == h1, "sea state {code}: subsystems that differ after a restore: {:?}", diff(&h0, &h1));
        assert!(r.save_keyframe() == bytes && r.weather().sea_state == SeaState(code), "sea state {code}");
        for _ in 0..20 { r.step(); }
        assert_finite(&r);
    }
    // Weather the layers above set is carried as it is, whatever it holds.
    let mut w = world_on(&cat, SeaState::CALM);
    w.set_weather(WeatherState { visibility_m: f64::INFINITY, wind_speed_mps: 0.0, ..sea(SeaState::CALM) });
    w.set_weather(WeatherState { visibility_m: f64::INFINITY, wind_speed_mps: 0.0, sea_state: SeaState(8), ..sea(SeaState::CALM) });
    let bytes = w.save_keyframe();
    let r = World::restore_keyframe(WorldConfig { seed: 0, weather: sea(SeaState::CALM), map: w.config.map.clone(), catalogue: cat.clone() }, &bytes, &|_| None).expect("restores");
    assert_eq!(r.weather().visibility_m, f64::INFINITY);
}

// ---------------------------------------------------------------------------------------------------------
// Weather turns (weather.rs): the sea changes state without a step under the hulls.

fn storm_turning_to_rain(wind: f64) -> (WeatherState, WeatherState) {
    use content::weather::WeatherKind;
    let storm = WeatherState { kind: WeatherKind::Storm, sea_state: SeaState(7), wind_dir_rad: wind, wind_speed_mps: 20.0, ..WeatherState::default() };
    let rain = WeatherState { kind: WeatherKind::Rain, sea_state: SeaState(6), wind_dir_rad: wind, wind_speed_mps: 16.0, ..WeatherState::default() };
    (storm, rain)
}

/// Sample points: a 2 km square on a 50 m lattice.
fn lattice() -> Vec<(f64, f64)> { (0..41).flat_map(|i| (0..41).map(move |j| (i as f64 * 50.0 - 1_000.0, j as f64 * 50.0 - 1_000.0))).collect() }

#[test]
fn a_weather_turn_moves_the_sea_without_a_step() {
    let cat = catalogue();
    let (storm, rain) = storm_turning_to_rain(0.4);
    let mut w = World::new(WorldConfig { seed: 1042, weather: storm, map: MapDef { size_m: [20_000.0, 20_000.0], depth_m: 200.0, ..MapDef::default() }, catalogue: cat.clone() });
    for _ in 0..600 { w.step(); }
    let pts = lattice();
    let heights = |w: &World| -> Vec<f64> { pts.iter().map(|&(x, z)| w.water_height(x, z)).collect() };
    let worst_step = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(p, q)| (p - q).abs()).fold(0.0, f64::max);
    // The surface's own motion in one tick on the storm sea, for scale.
    let mut prev = heights(&w);
    let mut natural = 0.0f64;
    for _ in 0..120 { w.step(); let h = heights(&w); natural = natural.max(worst_step(&prev, &h)); prev = h; }
    // What a one-tick swap would do here: the step the old code put under every hull.
    let target = crate::waves::WaveSet::generate(wave_seed(1042), &rain, &cat.tuning.waves);
    let t = w.time_s();
    let swap = pts.iter().map(|&(x, z)| (target.height_at(x, z, t) - w.waves.height_at(x, z, t)).abs()).fold(0.0, f64::max);
    assert!(swap > 3.0, "a one-tick swap would step the sea {swap} m");
    w.set_weather(rain);
    let mut during = 0.0f64;
    for k in 0..crate::world::weather::WEATHER_BLEND_TICKS + 60 {
        w.step();
        let h = heights(&w);
        during = during.max(worst_step(&prev, &h));
        prev = h;
        if k + 1 == crate::world::weather::WEATHER_BLEND_TICKS / 2 { assert!(w.wave_blend.is_some(), "still turning at half time"); }
    }
    // Through the turn no point of the sea moves in one tick by more than its own motion allows, plus a little
    // for the fading components.
    assert!(during < natural + 0.05 && during < 0.25, "the sea stepped {during} m in one tick (its own motion {natural} m, a swap {swap} m)");
    // Once the turn is over the sea is exactly the new weather's.
    assert!(w.wave_blend.is_none());
    assert_eq!(w.waves, target);
}

#[test]
fn a_keyframe_taken_during_a_weather_turn_restores_the_turn() {
    let cat = catalogue();
    let (storm, rain) = storm_turning_to_rain(1.1);
    let config = || WorldConfig { seed: 77, weather: storm, map: MapDef { size_m: [20_000.0, 20_000.0], depth_m: 200.0, ..MapDef::default() }, catalogue: cat.clone() };
    let mut w = World::new(config());
    for _ in 0..30 { w.step(); }
    w.set_weather(rain);
    for _ in 0..600 { w.step(); }
    let bytes = w.save_keyframe();
    let none = |_: ContentHash| -> Option<Arc<VesselPrefab>> { None };
    let mut r = World::restore_keyframe(config(), &bytes, &none).expect("restores");
    assert_eq!((r.wave_blend, r.waves), (w.wave_blend, w.waves));
    for _ in 0..crate::world::weather::WEATHER_BLEND_TICKS { w.step(); r.step(); }
    assert_eq!(r.waves, w.waves);
    assert!(r.wave_blend.is_none());
}

/// Minelaying seed 2 (round 2 of the gate) in miniature: the stock fast attack craft at full power in a storm
/// that turns to rain. Before the turn blended, this hull, dry and undamaged, was lost as sunk on the tick of
/// the turn: the new sea stood 3.7 m higher under it.
#[test]
fn a_dry_hull_rides_out_a_weather_turn() {
    let cat = catalogue();
    let (p, text) = content::data::stock_blueprint_files().find(|(p, _)| p.ends_with("starters/fac22.bp.ron")).expect("the stock fast attack craft");
    let prefab = Arc::new(build_prefab(&content::blueprint::io::load_blueprint(text, p).expect("loads").blueprint, &cat));
    let (storm, rain) = storm_turning_to_rain((42.0 * 0.7) % core::f64::consts::TAU);
    let mut w = World::new(WorldConfig { seed: 1042, weather: storm, map: MapDef { size_m: [40_000.0, 40_000.0], depth_m: 500.0, ..MapDef::default() }, catalogue: cat.clone() });
    let id = w.spawn_vessel(&VesselSpawn { speed_mps: 9.0, ..spawn_at(&prefab, 0, 0.0, 0.0, 94f64.to_radians()) });
    w.set_controls(id, VesselControls { throttle: 1.0, ..Default::default() });
    for _ in 0..3600 { w.step(); }
    assert_eq!(w.vessel(id).unwrap().status, VesselStatus::Active);
    w.set_weather(rain);
    let mut deck_under = 0u32;
    for _ in 0..crate::world::weather::WEATHER_BLEND_TICKS + 600 {
        w.step();
        let v = w.vessel(id).expect("still afloat");
        assert_eq!(v.status, VesselStatus::Active, "at {:.2} s", w.time_s());
        let t = w.time_s() - DT;
        if v.is_sunk(&|x, z| w.waves.height_at(x, z, t)) { deck_under += 1; }
    }
    let v = w.vessel(id).unwrap();
    assert_eq!(deck_under, 0);
    assert!(v.compartments.compartments.iter().all(|c| c.water_m3 == 0.0) && v.grid.solid_count == v.grid.original_count);
}

/// The blend does not hide a real loss: a hull whose whole deck is under the sea in the middle of a weather
/// turn is declared sunk on that tick, as on a steady sea.
#[test]
fn a_hull_under_the_sea_during_a_weather_turn_is_sunk_on_that_tick() {
    let cat = catalogue();
    let prefab = gunboat(&cat);
    let (storm, rain) = storm_turning_to_rain(0.9);
    for turning in [false, true] {
        let mut w = World::new(WorldConfig { seed: 5, weather: storm, map: MapDef { size_m: [20_000.0, 20_000.0], depth_m: 200.0, ..MapDef::default() }, catalogue: cat.clone() });
        let id = w.spawn_vessel(&spawn_at(&prefab, 0, 0.0, 0.0, 0.5));
        for _ in 0..120 { w.step(); }
        if turning { w.set_weather(rain); for _ in 0..crate::world::weather::WEATHER_BLEND_TICKS / 2 { w.step(); } }
        assert_eq!(w.wave_blend.is_some(), turning);
        let v = w.vessel(id).unwrap();
        assert!(matches!(v.status, VesselStatus::Active | VesselStatus::Disabled), "{:?}", v.status);
        // Held 25 m down, far below any trough of this sea, at rest.
        let mut kin = v.kin;
        kin.pose.pos.y = -25.0;
        kin.lin_vel = DVec3::ZERO;
        kin.ang_vel = DVec3::ZERO;
        w.teleport(id, &kin);
        w.step();
        assert_eq!(w.vessel(id).unwrap().status, VesselStatus::Lost(crate::vessel::LossCause::Sunk), "turning {turning}");
    }
}
