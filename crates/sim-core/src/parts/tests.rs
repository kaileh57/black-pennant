use super::step::{self, Keys, PartsIo};
use super::*;
use crate::compartments::testkit::{block, box_hull, build as build_grid_kit, catalogue as test_catalogue, def as test_def, idx, port};
use crate::compartments::CompartmentSet;
use crate::math::{Kinematics, Pose};
use crate::time::DT;
use content::blueprint::{KitPlacement, PartPlacement};
use content::catalogue::Catalogue;
use content::cell::{CellGrid, CellPos};
use content::enums::{Domain, NetworkKind};
use content::ids::{KitId, KitSlot, MunitionId, PartId, TextKey};
use content::part::weapons::{CalibreBand, Fuse, MunitionDef, MunitionKind, SeekerKind};
use content::part::{DecoyKind, DecoyLauncherDef, EngineDef, EngineKind, Flammable, GunDef, LauncherDef, LauncherKind, PartFunction, PortRole, PropulsorDef, PropulsorKind, SensorDef, SensorKind};
use glam::DVec3;

fn gun(rof: f64, barrels: u8, ready: u32) -> PartFunction {
    PartFunction::Gun(GunDef {
        calibre_mm: 76, band: CalibreBand::Medium, barrels, rof_rpm: rof, muzzle_mps: 900.0, shell_kg: 6.0, train_rate_dps: 60.0,
        elevation_rate_dps: 30.0, elevation_min_deg: -10.0, elevation_max_deg: 80.0, dispersion_mrad: 1.0, point_defence: false, ready_rounds: ready,
    })
}

fn munition(id: &str, kind: MunitionKind) -> MunitionDef {
    MunitionDef {
        id: MunitionId::new(id), name: TextKey::new("m"), kind, phase: Default::default(), domain: Domain::Weapon, speed_mps: 270.0, range_m: 30_000.0,
        max_accel: 100.0, cruise_alt_m: 5.0, fuse: Fuse::Contact, he_b: 8, he_r_cells: 4, seeker: SeekerKind::None, seeker_r_ref_m: 0.0,
        seeker_fov_half_deg: 0.0, pn_n: 3.0, wire_guided: false, radius_m: 0.5, hp: 1, rcs_m2: 0.1, noise: 0.0, mass_kg: 500.0, cost: 1,
        sensors: vec![], payload: vec![], endurance_s: 0.0, model: Default::default(), icon: Default::default(),
    }
}

/// Test catalogue, index order:
/// 0 gun, 1 magazine, 2 hoist, 3 diesel, 4 propeller, 5 rudder, 6 launcher, 7 search radar, 8 decoy, 9 pump,
/// 10 drone bay, 11 fuel tank, 12 big box (2 x 1 x 3), 13 fast gun, 14 esm, 15 door, 16 reloading torpedo tube,
/// 17 mine rail, 18 towed decoy.
fn cat() -> Catalogue {
    let power = vec![port(NetworkKind::Power, PortRole::Consume, 10.0, 0.0)];
    let mut defs = vec![
        test_def("a.gun", gun(80.0, 1, 10), power.clone(), 1000.0),
        test_def("b.mag", PartFunction::Magazine { calibre_mm: 76, rounds: 5, ammo_kg: 50.0 }, vec![], 100.0),
        test_def("c.hoist", PartFunction::Hoist { rounds_per_min: 60.0 }, power.clone(), 100.0),
        test_def("d.diesel", PartFunction::Engine(EngineDef { kind: EngineKind::Diesel, shaft_kw: 1000.0, fuel_lps: 0.1, spool_s: 10.0 }), vec![], 100.0),
        test_def("e.prop", PartFunction::Propulsor(PropulsorDef { kind: PropulsorKind::Propeller, max_shaft_kw: 800.0, thrust_n_per_kw: 100.0, cavitation_kn: 20.0, steer_max_deg: 0.0 }), vec![], 100.0),
        test_def("f.rudder", PartFunction::Rudder { area_m2: 1.0, max_deg: 35.0, rate_dps: 5.0 }, power.clone(), 100.0),
        test_def("g.launcher", PartFunction::Launcher(LauncherDef { kind: LauncherKind::Missile, munition: MunitionId::new("asm"), cells: 2, interval_s: 2.0, reload_s: 0.0, train_rate_dps: None }), power.clone(), 100.0),
        test_def("h.radar", PartFunction::Sensor(SensorDef { kind: SensorKind::SearchRadar, r_ref_m: 20_000.0, rate_hz: 5.0, rpm: Some(24.0), fov_half_deg: 180.0, emitter: "r".into() }), power.clone(), 100.0),
        test_def("i.chaff", PartFunction::Decoy(DecoyLauncherDef { kind: DecoyKind::Chaff, rounds: 5, salvo: 2, reload_s: 10.0 }), power.clone(), 100.0),
        test_def("j.pump", PartFunction::Pump { m3ps: 0.05 }, power.clone(), 100.0),
        test_def("k.bay", PartFunction::DroneBay { drone: MunitionId::new("drone"), count: 2, recharge_s: 5.0 }, power.clone(), 100.0),
        test_def("l.tank", PartFunction::FuelTank { litres: 1000.0 }, vec![], 100.0),
        test_def("m.box", PartFunction::Structure, vec![], 100.0),
        test_def("n.fastgun", gun(4500.0, 1, 1000), power.clone(), 100.0),
        test_def("o.esm", PartFunction::Sensor(SensorDef { kind: SensorKind::Esm, r_ref_m: 0.0, rate_hz: 2.0, rpm: None, fov_half_deg: 180.0, emitter: String::new() }), power.clone(), 100.0),
        test_def("p.door", PartFunction::Door, vec![], 100.0),
        test_def("q.tube", PartFunction::Launcher(LauncherDef { kind: LauncherKind::TorpedoTube, munition: MunitionId::new("asm"), cells: 1, interval_s: 1.0, reload_s: 10.0, train_rate_dps: None }), power.clone(), 100.0),
        test_def("r.rail", PartFunction::MineRail { mine: MunitionId::new("mine"), capacity: 2 }, power.clone(), 100.0),
        test_def("s.towed", PartFunction::Decoy(DecoyLauncherDef { kind: DecoyKind::TowedDecoy, rounds: 0, salvo: 1, reload_s: 0.0 }), power.clone(), 100.0),
        test_def("t.ballast", PartFunction::Ballast { max_kg: 4000.0 }, vec![], 100.0),
    ];
    defs[0].power_priority = 3;
    defs[12].size = [2, 1, 3];
    let mut c = test_catalogue(defs);
    for m in [munition("asm", MunitionKind::AntiShipMissile), munition("drone", MunitionKind::Drone), munition("mine", MunitionKind::Mine)] { c.munitions.insert(m.id.clone(), m); }
    c
}

/// A runtime part of catalogue entry `def` at slot `slot`, centre at x metres, fully served by the networks.
fn rt(slot: u16, def: u16, x: f64, c: &Catalogue) -> PartRuntime {
    let d = c.parts.get_index(def as usize).unwrap().1;
    let links = crate::networks::PartLinks { power_supplied: 1.0, fuel_supplied: 1.0, ..Default::default() };
    PartRuntime {
        slot: PartSlot(slot), def: PartIdx(def), pos: CellPos::default(), rot: Default::default(), cells: vec![], centre: DVec3::new(x, 1.0, 1.0),
        compartment: None, health: 1.0, level: DamageLevel::Intact, links, priority: 0, state: super::build::initial_state(d, None),
    }
}

struct Fx { parts: Vec<PartRuntime>, controls: VesselControls, kin: Kinematics, comps: CompartmentSet, active: bool, last_fired: u64, cat: Catalogue }

impl Fx {
    fn new(parts: Vec<PartRuntime>, cat: Catalogue) -> Self {
        let n = parts.len();
        Fx { parts, controls: VesselControls { parts: vec![PartControl::default(); n], ..Default::default() }, kin: Kinematics::default(), comps: CompartmentSet::default(), active: true, last_fired: 0, cat }
    }
    fn step(&mut self, seed: u64, tick: u64) -> Vec<SpawnRequest> { self.step_dt(seed, tick, DT) }
    fn step_dt(&mut self, seed: u64, tick: u64, dt: f64) -> Vec<SpawnRequest> {
        let mut out = Vec::new();
        let io = PartsIo { parts: &mut self.parts, controls: &mut self.controls, kin: self.kin, compartments: &self.comps, active: self.active, last_fired_tick: &mut self.last_fired };
        step::step(io, Keys { seed, tick, vessel: 9 }, &self.cat, dt, &mut out);
        out
    }
    fn run(&mut self, from: u64, ticks: u64) -> Vec<SpawnRequest> { (from..from + ticks).flat_map(|t| self.step(1, t)).collect() }
    fn order(&mut self, slot: usize, o: PartOrder) { self.controls.parts[slot].order = o; }
}

fn mount(p: &PartRuntime) -> MountState { match p.state { PartState::Mount(m) => m, _ => panic!("not a mount") } }

// ---- build ----

fn placement(part: &str, pos: CellPos, kit: Option<u16>) -> PartPlacement {
    PartPlacement { part: PartId::new(part), pos, rot: Default::default(), kit: kit.map(KitSlot), label: None, load: None }
}

/// Cross-unit: an unset ballast load is a full tank, as content's stats and the editor's Suggest read it, so
/// the hull that sails is the hull the dry dock weighed. A set load is kilograms, clamped to the tank.
#[test]
fn unset_ballast_load_is_full() {
    let c = cat();
    let mut bp = content::blueprint::io::new_blueprint("t", 10.0);
    bp.cells = box_hull([12, 6, 8], &[], &[]).to_rows();
    bp.parts = vec![
        placement("t.ballast", CellPos::new(2, 1, 2), None),
        PartPlacement { load: Some(1500.0), ..placement("t.ballast", CellPos::new(4, 1, 2), None) },
        PartPlacement { load: Some(9e9), ..placement("t.ballast", CellPos::new(6, 1, 2), None) },
    ];
    let grid = content::blueprint::io::build_grid(&bp, &c);
    let kg: Vec<f64> = build_parts(&bp, &grid, &c).iter().map(|p| match p.state { PartState::Ballast { kg } => kg, _ => f64::NAN }).collect();
    assert_eq!(kg, vec![4000.0, 1500.0, 4000.0]);
}

#[test]
fn build_footprint_loads_and_unknown_parts() {
    let c = cat();
    let mut bp = content::blueprint::io::new_blueprint("t", 10.0);
    bp.cells = box_hull([12, 6, 8], &[], &[]).to_rows();
    bp.parts = vec![
        PartPlacement { rot: PartRot { yaw: 1, tilt: 0 }, ..placement("m.box", CellPos::new(2, 1, 2), None) },
        PartPlacement { load: Some(0.5), ..placement("l.tank", CellPos::new(6, 1, 2), None) },
        PartPlacement { load: Some(0.5), ..placement("b.mag", CellPos::new(7, 1, 2), None) },
        placement("zz.unknown", CellPos::new(8, 1, 2), None),
        PartPlacement { load: Some(f64::NAN), ..placement("j.pump", CellPos::new(9, 1, 2), None) },
    ];
    let grid = content::blueprint::io::build_grid(&bp, &c);
    let parts = build_parts(&bp, &grid, &c);
    assert_eq!(parts.len(), 5);
    assert!(parts.iter().enumerate().all(|(i, p)| p.slot == PartSlot(i as u16)));
    // A 2 x 1 x 3 box turned a quarter is 3 x 1 x 2 from its minimum corner.
    let b = &parts[0];
    assert_eq!(b.cells.len(), 6);
    assert!(b.cells.windows(2).all(|w| w[0] < w[1]));
    let xs: Vec<i16> = b.cells.iter().map(|c| grid.pos(*c).x).collect();
    assert_eq!((*xs.iter().min().unwrap(), *xs.iter().max().unwrap()), (2, 4));
    assert!((b.centre - DVec3::new(1.75, 0.75, 1.5)).length() < 1e-12, "{:?}", b.centre);
    assert!(matches!(parts[1].state, PartState::FuelTank { litres } if (litres - 500.0).abs() < 1e-9));
    // 5 rounds at half load is 2 rounds with their share of the mass.
    assert!(matches!(parts[2].state, PartState::Magazine { rounds: 2, ammo_kg, .. } if (ammo_kg - 20.0).abs() < 1e-9));
    assert_eq!((parts[3].def, parts[3].level, parts[3].works()), (super::build::UNKNOWN_DEF, DamageLevel::Destroyed, false));
    assert!(matches!(parts[4].state, PartState::Pump { running: false, target: None }));
    assert_eq!(parts[4].priority, 5, "a consumer with no priority in its data takes 5");
    assert_eq!(parts[1].priority, 0, "a tank draws no power");
    assert_eq!(parts[4].links.power_supplied, 1.0);
    // Same blueprint, same parts.
    assert_eq!(parts, build_parts(&bp, &grid, &c));
}

#[test]
fn kit_wiring_feeds_guns_from_their_own_kit() {
    let c = cat();
    let mut bp = content::blueprint::io::new_blueprint("t", 10.0);
    bp.cells = box_hull([24, 8, 8], &[], &[]).to_rows();
    bp.parts = vec![
        placement("a.gun", CellPos::new(2, 7, 3), Some(0)),
        placement("b.mag", CellPos::new(2, 2, 3), Some(0)),
        placement("c.hoist", CellPos::new(2, 4, 3), Some(0)),
        placement("a.gun", CellPos::new(14, 7, 3), None),
        placement("b.mag", CellPos::new(14, 6, 3), None),
        placement("b.mag", CellPos::new(20, 2, 3), None),
    ];
    bp.kits = vec![KitPlacement { kit: KitId::new("k"), version: 1, pos: CellPos::new(2, 2, 3), rot: Default::default(), mirrored: false, sockets: vec![], settings: Default::default(), parts: vec![PartSlot(0), PartSlot(1), PartSlot(2)] }];
    let grid = content::blueprint::io::build_grid(&bp, &c);
    let parts = build_parts(&bp, &grid, &c);
    let kit_gun = parts[0].feed();
    assert_eq!(kit_gun.magazines().collect::<Vec<_>>(), vec![PartSlot(1)]);
    assert_eq!(kit_gun.hoists().collect::<Vec<_>>(), vec![PartSlot(2)]);
    assert!(!kit_gun.direct());
    let loose = parts[3].feed();
    assert_eq!(loose.magazines().collect::<Vec<_>>(), vec![PartSlot(4), PartSlot(5)], "nearest loose magazine first");
    assert!(loose.direct(), "the magazine under the gun touches it");
    assert_eq!(loose.hoists().count(), 0);
    assert_eq!(parts[1].feed().magazines().count(), 0);
}

#[test]
fn refresh_health_takes_the_cell_mean_and_keeps_burn_down() {
    let g = CellGrid::new([4, 2, 2]);
    let cells = block(&g, [0, 0, 0], [3, 0, 0]);
    let mut grid = build_grid_kit(g.clone(), &[(PartSlot(0), cells.clone(), false, Flammable::No)]);
    let c = cat();
    let mut parts = vec![rt(0, 12, 0.0, &c)];
    parts[0].cells = cells.clone();
    grid.add_damage(cells[0], 8);
    assert_eq!(refresh_health(&mut parts, &grid, &[cells[0]]), vec![PartSlot(0)]);
    assert!((parts[0].health - 0.75).abs() < 1e-12);
    assert_eq!(parts[0].level, DamageLevel::Damaged);
    // Burn-down already took it to 0.5; a light hit does not raise it back to the cell mean.
    parts[0].health = 0.5;
    grid.add_damage(cells[1], 1);
    assert!(refresh_health(&mut parts, &grid, &[cells[1]]).is_empty());
    assert_eq!(parts[0].health, 0.5);
    // Unrelated cells change nothing; all cells gone is Destroyed.
    assert!(refresh_health(&mut parts, &grid, &[idx(&g, 0, 1, 1)]).is_empty());
    for &cc in &cells { grid.add_damage(cc, 8); }
    assert_eq!(refresh_health(&mut parts, &grid, &cells), vec![PartSlot(0)]);
    assert_eq!((parts[0].health, parts[0].level), (0.0, DamageLevel::Destroyed));
}

// ---- step ----

#[test]
fn engines_spool_and_share_power_over_propellers() {
    let c = cat();
    let mut f = Fx::new(vec![rt(0, 3, 0.0, &c), rt(1, 4, 0.0, &c), rt(2, 4, 0.0, &c)], c);
    f.controls.throttle = 1.0;
    f.run(0, 300);
    let PartState::Engine { throttle, output_kw } = f.parts[0].state else { panic!() };
    assert!((throttle - 0.5).abs() < 1e-9, "10 s spool, 5 s in: {throttle}");
    assert!((output_kw - 500.0).abs() < 1e-6);
    for p in &f.parts[1..] {
        assert!(matches!(p.state, PartState::Propulsor { shaft_kw, thrust_n, .. } if (shaft_kw - 250.0).abs() < 1e-6 && (thrust_n - 25_000.0).abs() < 1e-3));
    }
    // Half fuel halves output; one propeller at Heavy absorbs half its rating.
    f.parts[0].links.fuel_supplied = 0.5;
    f.parts[2].level = DamageLevel::Heavy;
    f.run(300, 600);
    let PartState::Engine { output_kw, .. } = f.parts[0].state else { panic!() };
    assert!((output_kw - 500.0).abs() < 1e-6);
    let shaft = |p: &PartRuntime| match p.state { PartState::Propulsor { shaft_kw, .. } => shaft_kw, _ => panic!() };
    assert!((shaft(&f.parts[1]) - 500.0 * 800.0 / 1200.0).abs() < 1e-6);
    assert!((shaft(&f.parts[2]) - 500.0 * 400.0 / 1200.0).abs() < 1e-6);
    // Astern: thrust reverses, shaft power stays a magnitude.
    f.parts[0].links.fuel_supplied = 1.0;
    f.controls.throttle = -1.0;
    f.run(900, 1200);
    assert!(matches!(f.parts[1].state, PartState::Propulsor { shaft_kw, thrust_n, .. } if shaft_kw > 0.0 && thrust_n < 0.0));
}

#[test]
fn rudder_slews_at_its_rate_and_jams_without_power() {
    let c = cat();
    let mut f = Fx::new(vec![rt(0, 5, 0.0, &c)], c);
    f.controls.rudder = 1.0;
    f.run(0, 60);
    let angle = |f: &Fx| match f.parts[0].state { PartState::Rudder { angle_rad } => angle_rad, _ => panic!() };
    assert!((angle(&f) - 5f64.to_radians()).abs() < 1e-9);
    f.parts[0].links.power_supplied = 0.0;
    f.controls.rudder = -1.0;
    f.run(60, 60);
    assert!((angle(&f) - 5f64.to_radians()).abs() < 1e-9, "steering gear without power holds");
    f.parts[0].links.power_supplied = 1.0;
    f.controls.rudder = f64::NAN;
    f.run(120, 120);
    assert!(angle(&f).abs() < 1e-9, "a non-finite order centres the rudder");
}

#[test]
fn mount_slews_then_fires_at_its_rate_with_seeded_dispersion() {
    let c = cat();
    let mut f = Fx::new(vec![rt(0, 0, 0.0, &c)], c);
    let o = MountOrder { laid_rot: None, train_rad: 90f64.to_radians(), elevation_rad: 0.0, fire: true, max_rounds: 0, aim_error_rad: [0.0; 2] };
    f.order(0, PartOrder::Mount(o));
    // 90 degrees at 60 deg/s: on target after 90 ticks.
    let early = f.run(0, 89);
    assert!(early.is_empty() && !mount(&f.parts[0]).on_target);
    let shots = f.run(89, 1);
    assert_eq!(shots.len(), 1, "a loaded gun fires as soon as it is on target");
    assert_eq!(f.last_fired, 89);
    // 80 rpm: one round per 45 ticks.
    let more = f.run(90, 135);
    assert_eq!(more.len(), 3);
    let m = mount(&f.parts[0]);
    assert_eq!((m.ready_rounds, m.fired), (6, 4));
    let SpawnRequest::Shell { velocity, origin, rounds, slot } = shots[0] else { panic!() };
    assert_eq!((rounds, slot, origin), (1, PartSlot(0), DVec3::new(0.0, 1.0, 1.0)));
    // Positive training swings the muzzle to port (-Z); dispersion is about 1 mrad.
    let dir = velocity / velocity.length();
    assert!((dir - DVec3::NEG_Z).length() < 0.01, "{dir:?}");
    // Same seed same rounds; another seed differs.
    let again = |seed: u64| {
        let c = cat();
        let mut g = Fx::new(vec![rt(0, 0, 0.0, &c)], c);
        g.order(0, PartOrder::Mount(o));
        (0..90).flat_map(|t| g.step(seed, t)).collect::<Vec<_>>()
    };
    assert_eq!(again(1), shots);
    assert_ne!(again(2), shots);
}

#[test]
fn mount_respects_limits_power_and_order_validity() {
    let c = cat();
    let mut f = Fx::new(vec![rt(0, 0, 0.0, &c)], c);
    // Elevation beyond the gun's 80 degrees: it lays at 80 but never fires.
    f.order(0, PartOrder::Mount(MountOrder { laid_rot: None, train_rad: 0.0, elevation_rad: 85f64.to_radians(), fire: true, max_rounds: 0, aim_error_rad: [0.0; 2] }));
    assert!(f.run(0, 300).is_empty());
    assert!((mount(&f.parts[0]).elevation_rad - 80f64.to_radians()).abs() < 1e-9);
    // Shortest way round: from 170 to -170 degrees is 20 degrees.
    if let PartState::Mount(m) = &mut f.parts[0].state { m.train_rad = 170f64.to_radians(); m.elevation_rad = 0.0; }
    f.order(0, PartOrder::Mount(MountOrder { laid_rot: None, train_rad: -170f64.to_radians(), elevation_rad: 0.0, fire: false, max_rounds: 0, aim_error_rad: [0.0; 2] }));
    f.run(300, 20);
    assert!(mount(&f.parts[0]).on_target);
    // A hand-set arc [-150, 150]: the slew stays inside it and goes the long way.
    f.parts[0].links.mech.arc = Some(HandArc { t_min: -150f64.to_radians(), t_max: 150f64.to_radians(), e_min: -0.2, e_max: 1.4, full: false });
    if let PartState::Mount(m) = &mut f.parts[0].state { m.train_rad = 140f64.to_radians(); }
    f.order(0, PartOrder::Mount(MountOrder { laid_rot: None, train_rad: -140f64.to_radians(), elevation_rad: 0.0, fire: false, max_rounds: 0, aim_error_rad: [0.0; 2] }));
    f.run(320, 60);
    assert!((mount(&f.parts[0]).train_rad - 80f64.to_radians()).abs() < 1e-9, "turned 60 degrees toward the bow");
    // No power: no slew, no fire.
    f.parts[0].links.power_supplied = 0.0;
    let before = mount(&f.parts[0]).train_rad;
    f.order(0, PartOrder::Mount(MountOrder { laid_rot: None, train_rad: before, elevation_rad: 0.0, fire: true, max_rounds: 0, aim_error_rad: [0.0; 2] }));
    assert!(f.run(380, 60).is_empty());
    // Garbage orders hold the mount.
    f.parts[0].links.power_supplied = 1.0;
    f.order(0, PartOrder::Mount(MountOrder { laid_rot: None, train_rad: f64::NAN, elevation_rad: f64::INFINITY, fire: true, max_rounds: 0, aim_error_rad: [f64::NAN; 2] }));
    assert!(f.run(440, 10).is_empty());
    assert_eq!(mount(&f.parts[0]).train_rad, before);
}

#[test]
fn fast_guns_keep_their_rate_above_one_round_per_tick() {
    let c = cat();
    let mut f = Fx::new(vec![rt(0, 13, 0.0, &c)], c);
    f.order(0, PartOrder::Mount(MountOrder { laid_rot: None, train_rad: 0.0, elevation_rad: 0.0, fire: true, max_rounds: 0, aim_error_rad: [0.0; 2] }));
    let n = f.run(0, 60).len();
    assert!((74..=75).contains(&n), "4,500 rpm is 75 rounds a second: {n}");
    f.order(0, PartOrder::Mount(MountOrder { laid_rot: None, train_rad: 0.0, elevation_rad: 0.0, fire: true, max_rounds: 1, aim_error_rad: [0.0; 2] }));
    assert_eq!(f.run(60, 60).len(), 60, "max_rounds caps each tick");
}

#[test]
fn hoist_feeds_ready_rounds_from_the_kit_magazine() {
    let c = cat();
    let mut parts = vec![rt(0, 0, 0.0, &c), rt(1, 1, 0.0, &c), rt(2, 2, 0.0, &c)];
    parts[0].links.mech.feed = FeedLinks { mags: [1, 0, 0, 0], n_mags: 1, hoists: [2, 0, 0, 0], n_hoists: 1, direct: false };
    if let PartState::Mount(m) = &mut parts[0].state { m.ready_rounds = 0; }
    let mut f = Fx::new(parts, c);
    f.run(0, 180);
    assert_eq!(mount(&f.parts[0]).ready_rounds, 3, "60 rounds per minute");
    assert!(matches!(f.parts[1].state, PartState::Magazine { rounds: 2, ammo_kg, .. } if (ammo_kg - 20.0).abs() < 1e-9));
    // An unpowered hoist stops the feed; a dry magazine ends it.
    f.parts[2].links.power_supplied = 0.0;
    f.run(180, 120);
    assert_eq!(mount(&f.parts[0]).ready_rounds, 3);
    f.parts[2].links.power_supplied = 1.0;
    f.run(300, 600);
    assert_eq!(mount(&f.parts[0]).ready_rounds, 5);
    assert!(matches!(f.parts[1].state, PartState::Magazine { rounds: 0, ammo_kg, .. } if ammo_kg == 0.0));
}

#[test]
fn launch_orders_are_one_shot_and_wait_for_the_interval() {
    let c = cat();
    let mut f = Fx::new(vec![rt(0, 6, 0.0, &c)], c);
    let aim = LaunchAim::Bearing { heading_rad: 1.0 };
    f.order(0, PartOrder::Launch(aim));
    let out = f.step(1, 0);
    assert_eq!(out.len(), 1);
    let SpawnRequest::Munition { munition, velocity, aim: a, .. } = out[0] else { panic!() };
    assert_eq!((munition, a), (MunitionIdx(0), aim));
    assert!((velocity.length() - 25.0).abs() < 1e-9 && velocity.y > 0.0, "ejected upward along the facing");
    assert_eq!(f.controls.parts[0].order, PartOrder::None, "the order is consumed");
    assert_eq!(f.last_fired, 0);
    // A second order waits for the 2 s interval, then launches the last cell.
    f.order(0, PartOrder::Launch(aim));
    assert!(f.run(1, 118).is_empty());
    assert_eq!(f.controls.parts[0].order, PartOrder::Launch(aim), "still pending");
    assert_eq!(f.run(119, 2).len(), 1);
    // Empty and no reload at sea: the order is dropped.
    f.order(0, PartOrder::Launch(aim));
    assert!(f.step(1, 500).is_empty());
    assert_eq!(f.controls.parts[0].order, PartOrder::None);
    assert!(matches!(f.parts[0].state, PartState::Launcher { ready: 0, .. }));
}

#[test]
fn search_radar_turns_at_24_rpm_and_radiates_only_when_told_and_powered() {
    let c = cat();
    let mut f = Fx::new(vec![rt(0, 7, 0.0, &c), rt(1, 14, 0.0, &c)], c);
    f.step(1, 0);
    let PartState::Sensor { bearing_rad, swept_from_rad, radiating, .. } = f.parts[0].state else { panic!() };
    assert!((bearing_rad - TAU_PER_TICK).abs() < 1e-12 && swept_from_rad == 0.0 && !radiating);
    f.run(1, 149);
    let PartState::Sensor { bearing_rad, .. } = f.parts[0].state else { panic!() };
    assert!(bearing_rad < 1e-9 || (core::f64::consts::TAU - bearing_rad) < 1e-9, "one turn in 2.5 s: {bearing_rad}");
    for s in 0..2 { f.order(s, PartOrder::Emit { radiating: true, train_rad: 0.0 }); }
    f.step(1, 150);
    assert!(matches!(f.parts[0].state, PartState::Sensor { radiating: true, .. }));
    assert!(matches!(f.parts[1].state, PartState::Sensor { radiating: false, .. }), "ESM never radiates");
    f.parts[0].links.power_supplied = 0.0;
    f.step(1, 151);
    assert!(matches!(f.parts[0].state, PartState::Sensor { radiating: false, .. }));
}
const TAU_PER_TICK: f64 = 24.0 * core::f64::consts::TAU / 60.0 / 60.0;

#[test]
fn decoy_salvo_then_reload() {
    let c = cat();
    let mut f = Fx::new(vec![rt(0, 8, 0.0, &c)], c);
    f.order(0, PartOrder::FireDecoy);
    let out = f.step(1, 0);
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|s| matches!(s, SpawnRequest::Decoy { kind: DecoyKind::Chaff, origin, .. } if (origin.y - 16.0).abs() < 1e-9)));
    assert!(matches!(f.parts[0].state, PartState::Decoy { rounds: 3, reload_ticks: 600, .. }));
    f.order(0, PartOrder::FireDecoy);
    assert!(f.run(1, 599).is_empty(), "10 s reload");
    assert_eq!(f.run(600, 1).len(), 2);
    f.order(0, PartOrder::FireDecoy);
    assert_eq!(f.run(601, 700).len(), 1, "the last round");
    f.order(0, PartOrder::FireDecoy);
    assert!(f.step(1, 2000).is_empty());
    assert_eq!(f.controls.parts[0].order, PartOrder::None, "no rounds: dropped");
}

#[test]
fn pumps_doors_and_shutdown() {
    let c = cat();
    let mut f = Fx::new(vec![rt(0, 9, 0.0, &c), rt(1, 15, 0.0, &c), rt(2, 3, 0.0, &c)], c);
    f.parts[0].compartment = Some(CompartmentId(0));
    f.comps.adjacency.push(crate::compartments::Adjacency { a: CompartmentId(0), b: CompartmentId(1), shared_faces: 4, vertical: false, has_door: false });
    f.comps.compartments = vec![compartment(0), compartment(1), compartment(2)];
    f.order(0, PartOrder::Pump { running: true, target: Some(CompartmentId(1)) });
    f.order(1, PartOrder::Door { closed: false });
    f.controls.throttle = 1.0;
    f.run(0, 60);
    assert!(matches!(f.parts[0].state, PartState::Pump { running: true, target: Some(CompartmentId(1)) }));
    assert!(matches!(f.parts[1].state, PartState::Door { closed: false }));
    f.order(0, PartOrder::Pump { running: true, target: Some(CompartmentId(2)) });
    f.step(1, 60);
    assert!(matches!(f.parts[0].state, PartState::Pump { running: true, target: None }), "not adjacent: drains its own");
    f.active = false;
    f.step(1, 61);
    assert!(matches!(f.parts[0].state, PartState::Pump { running: false, .. }));
    assert!(matches!(f.parts[2].state, PartState::Engine { throttle, output_kw } if throttle == 0.0 && output_kw == 0.0));
}

fn compartment(id: u16) -> crate::compartments::Compartment {
    crate::compartments::Compartment {
        id: CompartmentId(id), cells: vec![], volume_m3: 1.0, min: [0; 3], max: [0; 3], centroid: DVec3::ZERO, water_m3: 0.0, water_plane_y: 0.0,
        water_centroid: DVec3::ZERO, free_surface_m2: 0.0, fire: 0.0, fire_ticks: 0, parts: vec![], magazine: None, berths: 0, below_waterline: false,
        flood_valve_open: false, sealed: true,
    }
}

#[test]
fn drones_launch_and_recover_with_recharge() {
    let c = cat();
    let mut f = Fx::new(vec![rt(0, 10, 0.0, &c)], c);
    f.order(0, PartOrder::LaunchOffboard);
    let out = f.step(1, 0);
    assert!(matches!(out[..], [SpawnRequest::Munition { munition: MunitionIdx(1), .. }]));
    assert!(matches!(f.parts[0].state, PartState::DroneBay { aboard: 1, .. }));
    let cat = f.cat.clone();
    assert_eq!(recover_offboard(&mut f.parts, PartSlot(0), MunitionIdx(1), &cat), Some(PartSlot(0)));
    assert!(matches!(f.parts[0].state, PartState::DroneBay { aboard: 2, recharge_ticks: 300 }));
    assert_eq!(recover_offboard(&mut f.parts, PartSlot(0), MunitionIdx(1), &cat), None, "bay full");
    f.order(0, PartOrder::LaunchOffboard);
    assert!(f.run(1, 299).is_empty(), "recharging");
    assert_eq!(f.run(300, 1).len(), 1);
}

#[test]
fn tube_reloads_at_sea_from_its_magazine_slower_when_heavy() {
    let c = cat();
    let mut parts = vec![rt(0, 16, 0.0, &c), rt(1, 1, 0.0, &c)];
    parts[0].links.mech.feed = FeedLinks { mags: [1, 0, 0, 0], n_mags: 1, ..FeedLinks::default() };
    let mut f = Fx::new(parts, c);
    let aim = LaunchAim::Bearing { heading_rad: 0.0 };
    f.order(0, PartOrder::Launch(aim));
    let out = f.step(1, 0);
    let SpawnRequest::Munition { velocity, .. } = out[0] else { panic!() };
    assert!((velocity - DVec3::X * 12.0).length() < 1e-9, "a fixed tube ejects level along its facing");
    // The reload starts on the next tick and takes 10 s of base ticks.
    f.run(1, 600);
    assert!(matches!(f.parts[0].state, PartState::Launcher { ready: 0, .. }));
    f.run(601, 1);
    assert!(matches!(f.parts[0].state, PartState::Launcher { ready: 1, reload_ticks: 0, .. }));
    assert!(matches!(f.parts[1].state, PartState::Magazine { rounds: 4, ammo_kg, .. } if (ammo_kg - 40.0).abs() < 1e-9));
    // At Heavy damage the timer runs at half rate.
    f.order(0, PartOrder::Launch(aim));
    assert_eq!(f.step(1, 602).len(), 1);
    f.parts[0].level = DamageLevel::Heavy;
    f.run(603, 1100);
    assert!(matches!(f.parts[0].state, PartState::Launcher { ready: 0, .. }), "not yet at half rate");
    f.run(1703, 110);
    assert!(matches!(f.parts[0].state, PartState::Launcher { ready: 1, .. }));
}

#[test]
fn mine_rail_drops_astern_and_measures_the_run() {
    let c = cat();
    let mut f = Fx::new(vec![rt(0, 17, 0.0, &c)], c);
    f.kin.lin_vel = DVec3::new(5.0, 0.0, 0.0);
    f.run(0, 60);
    assert!(matches!(f.parts[0].state, PartState::MineRail { mines: 2, since_last_m } if since_last_m == f64::MAX));
    f.order(0, PartOrder::DropMine);
    let out = f.step(1, 60);
    assert!(matches!(out[..], [SpawnRequest::Munition { munition: MunitionIdx(2), velocity, .. }] if (velocity - DVec3::new(4.0, 0.0, 0.0)).length() < 1e-9));
    assert_eq!(f.controls.parts[0].order, PartOrder::None);
    f.run(61, 60);
    assert!(matches!(f.parts[0].state, PartState::MineRail { mines: 1, since_last_m } if (since_last_m - 5.0).abs() < 1e-9));
    // No power: the drop is skipped, not saved for later.
    f.parts[0].links.power_supplied = 0.0;
    f.order(0, PartOrder::DropMine);
    assert!(f.step(1, 121).is_empty());
    assert_eq!(f.controls.parts[0].order, PartOrder::None);
    assert!(matches!(f.parts[0].state, PartState::MineRail { mines: 1, .. }));
}

#[test]
fn towed_decoy_spawns_once_per_stream() {
    let c = cat();
    let mut f = Fx::new(vec![rt(0, 18, 0.0, &c)], c);
    f.order(0, PartOrder::StreamDecoy(true));
    assert_eq!(f.run(0, 30).len(), 1, "the persistent order streams one decoy");
    assert!(matches!(f.parts[0].state, PartState::Decoy { streamed: true, .. }));
    f.order(0, PartOrder::StreamDecoy(false));
    f.step(1, 30);
    assert!(matches!(f.parts[0].state, PartState::Decoy { streamed: false, .. }));
    f.order(0, PartOrder::StreamDecoy(true));
    assert_eq!(f.run(31, 5).len(), 1);
    f.parts[0].level = DamageLevel::Destroyed;
    f.step(1, 36);
    assert!(matches!(f.parts[0].state, PartState::Decoy { streamed: false, .. }), "a destroyed winch loses the decoy");
}

#[test]
fn half_rate_hulls_count_the_same_base_ticks() {
    let c = cat();
    let mut f = Fx::new(vec![rt(0, 8, 0.0, &c)], c);
    f.order(0, PartOrder::FireDecoy);
    assert_eq!(f.step_dt(1, 0, 2.0 * DT).len(), 2);
    f.order(0, PartOrder::FireDecoy);
    let early: usize = (1..300).map(|k| f.step_dt(1, 2 * k, 2.0 * DT).len()).sum();
    assert_eq!(early, 0, "600 base ticks are 300 calls at 30 Hz");
    assert_eq!(f.step_dt(1, 600, 2.0 * DT).len(), 2);
}

#[test]
fn mount_zero_follows_the_rotated_muzzle_axis() {
    use content::kit::{part_rot_matrix, PartRot, TILT_COUNT, YAW_COUNT};
    for yaw in 0..YAW_COUNT {
        // Untilted parts keep the plain yaw quarter turns.
        assert_eq!(mount_zero_rad(PartRot { yaw, tilt: 0 }), yaw as f64 * core::f64::consts::FRAC_PI_2);
        for tilt in 0..TILT_COUNT {
            let rot = PartRot { yaw, tilt };
            let m = part_rot_matrix(rot);
            let axis = DVec3::new(m[0][0] as f64, m[0][1] as f64, m[0][2] as f64);
            let barrel = mount_dir_local(mount_zero_rad(rot), 0.0, 0.0);
            if axis.y == 0.0 {
                assert!(barrel.distance(axis) < 1e-12, "rot {rot:?}: barrel {barrel} axis {axis}");
            } else {
                assert_eq!(mount_zero_rad(rot), yaw as f64 * core::f64::consts::FRAC_PI_2);
            }
        }
    }
}

#[test]
fn stabilised_laying_holds_the_world_line_as_the_hull_rolls() {
    use crate::math::DQuat;
    let zero = 0.3;
    let laid = DQuat::from_rotation_y(0.4);
    let o = MountOrder { laid_rot: Some(laid), train_rad: 1.2, elevation_rad: 0.05, fire: true, max_rounds: 0, aim_error_rad: [0.0; 2] };
    let line = laid * mount_dir_local(zero, o.train_rad, o.elevation_rad);
    // Rolled 6 degrees and pitched 2 since the order: the new angles point the barrel down the same line.
    let now = laid * DQuat::from_rotation_x(6f64.to_radians()) * DQuat::from_rotation_z(2f64.to_radians());
    let s = super::step::stabilised(o, zero, now);
    assert!((now * mount_dir_local(zero, s.train_rad, s.elevation_rad) - line).length() < 1e-9);
    assert!((s.elevation_rad - o.elevation_rad).abs() > 1e-3, "the roll moved the laid angles");
    // Same attitude, or no laid attitude: the order is unchanged.
    let same = super::step::stabilised(o, zero, laid);
    assert!((same.train_rad - o.train_rad).abs() < 1e-12 && (same.elevation_rad - o.elevation_rad).abs() < 1e-12);
    assert_eq!(super::step::stabilised(MountOrder { laid_rot: None, ..o }, zero, now), MountOrder { laid_rot: None, ..o });
}
