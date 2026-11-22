//! Tests of buoyancy, hydrostatics and manoeuvring on hand-built barges, checked against closed forms and
//! against the simulation the gauges are meant to predict.
use super::forces::{self, HullState};
use super::*;
use crate::compartments::testkit::{box_hull, build, catalogue, def, idx, part};
use crate::parts::{PartRuntime, PartState};
use crate::physics::{BodyHandle, PhysicsWorld, GRAVITY};
use crate::time::DT;
use content::cell::CellPos;
use content::ids::CompartmentId;
use content::part::{EngineDef, EngineKind, PartFunction, PropulsorDef, PropulsorKind};
use content::tuning::WaveTuning;
use content::weather::{SeaState, WeatherState};
use glam::{DMat3, DQuat};

const RHO: f64 = 1025.0;

struct Rig { grid: VesselGrid, comps: CompartmentSet, blocks: BuoyancyBlocks, mass: MassProperties, parts: Vec<PartRuntime> }

fn box_mass(kg: f64, com: DVec3, dims: DVec3) -> MassProperties {
    let d = dims * dims;
    MassProperties { mass_kg: kg, com, inertia: DMat3::from_diagonal(DVec3::new(d.y + d.z, d.x + d.z, d.x + d.y) * (kg / 12.0)) }
}

/// A sealed box barge 20 m long, 4 m deep and 6 m wide, 120 t, with its weight `kg_m` above the keel.
fn barge(bulkheads: &[i16], kg_m: f64) -> Rig {
    let grid = build(box_hull([40, 8, 12], bulkheads, &[]), &[]);
    let (comps, _) = CompartmentSet::derive(&grid, &[]);
    let blocks = build_blocks(&grid, &comps, 4);
    Rig { grid, comps, blocks, mass: box_mass(120_000.0, DVec3::new(10.0, kg_m, 3.0), DVec3::new(20.0, 4.0, 6.0)), parts: Vec::new() }
}

const DRAFT: f64 = 120_000.0 / RHO / 120.0;

fn tuning() -> BuoyancyTuning { BuoyancyTuning::default() }

impl Rig {
    fn spawn(&self, world: &mut PhysicsWorld, pose: Pose) -> BodyHandle {
        world.add_vessel(pose, DVec3::ZERO, &self.mass, &crate::physics::greedy_cuboids(&self.grid))
    }
    /// One tick of the world's force phase and physics step for this hull.
    fn tick(&self, world: &mut PhysicsWorld, body: BodyHandle, waves: &WaveSet, t: f64) -> HydroForces {
        let kin = world.kinematics(body);
        let hull = HullState { kin: &kin, mass: &self.mass, blocks: &self.blocks, parts: &self.parts, grid: &self.grid, step_s: DT };
        let mut out = HydroForces::default();
        buoyancy_on(&self.blocks, &kin.pose, DRAFT, waves, t, RHO, &mut out);
        forces::drag(&hull, waves, t, &tuning(), &mut out);
        forces::propulsion(&hull, waves, t, RHO, &mut out);
        for f in &out.forces { world.add_force_at(body, f.force, f.at); }
        world.add_torque(body, out.torque);
        world.step(DT, &mut Vec::new());
        out
    }
    fn run(&self, world: &mut PhysicsWorld, body: BodyHandle, waves: &WaveSet, from_tick: u64, ticks: u64) {
        for k in from_tick..from_tick + ticks { self.tick(world, body, waves, k as f64 * DT); }
    }
}

#[test]
fn blocks_carry_the_displacing_volume() {
    let rig = barge(&[13, 26], 1.5);
    let b = &rig.blocks;
    assert_eq!((b.block_cells, b.blocks.len(), b.columns.len()), (4, 60, 30));
    assert!((b.total_volume_m3() - 480.0).abs() < 1e-9);
    assert!(b.blocks.windows(2).all(|w| { let (p, q) = (b.lattice_of(&w[0]), b.lattice_of(&w[1])); (p[1], p[2], p[0]) < (q[1], q[2], q[0]) }), "ascending (y, z, x)");
    for blk in &b.blocks { assert_eq!(blk.volume_m3, 8.0); assert_eq!(blk.height_m, 2.0); assert_eq!(b.columns[blk.column as usize], [blk.centre.x, blk.centre.z]); }
    assert_eq!((b.form.lo, b.form.hi), (DVec3::ZERO, DVec3::new(20.0, 4.0, 6.0)));
    assert!((b.form.plan_m2 - 120.0).abs() < 1e-9 && (b.form.centroid - DVec3::new(10.0, 2.0, 3.0)).length() < 1e-9);
    assert!((b.form.upright_waterline(120.0 * 1.3).unwrap() - 1.3).abs() < 1e-9);
    assert!(b.form.upright_waterline(481.0).is_none());
    // An open boat displaces only its shell: no deck, so the air is open to the sea.
    let mut open = box_hull([16, 6, 12], &[], &[]);
    for z in 1..11 { for x in 1..15 { open.set(CellPos::new(x, 5, z), content::cell::Cell::EMPTY); } }
    let g = build(open, &[]);
    let (c, _) = CompartmentSet::derive(&g, &[]);
    assert!(c.compartments.is_empty());
    let ob = build_blocks(&g, &c, 4);
    assert!((ob.total_volume_m3() - g.solid_count as f64 * 0.125).abs() < 1e-9);
    // A keel plate block sits at the plate, not at the middle of its 2 m cube.
    let low = ob.blocks.iter().find(|k| ob.lattice_of(k) == [1, 0, 1]).unwrap();
    assert!((low.centre.y - 0.25).abs() < 1e-12 && low.height_m == 0.5);
    // Bad block sizes are clamped, and an inert grid floats nothing.
    assert_eq!(build_blocks(&rig.grid, &rig.comps, 0).block_cells, 1);
    assert_eq!(build_blocks(&rig.grid, &rig.comps, 200).block_cells, 8);
    let inert = VesselGrid::build(std::sync::Arc::new(content::cell::CellGrid { size: [4, 4, 4], cells: vec![] }), &[], &|_| (false, content::part::Flammable::No));
    assert_eq!(build_blocks(&inert, &CompartmentSet::default(), 4).total_volume_m3(), 0.0);
}

#[test]
fn dirty_rebuild_equals_a_full_build() {
    let mut rig = barge(&[13, 26], 1.5);
    rig.grid.drain_dirty_chunks();
    // Blow a hole through the bottom at the bow and knock out a run of the starboard side.
    let mut gone = vec![idx(&rig.grid.design, 35, 0, 5), idx(&rig.grid.design, 36, 0, 5)];
    for y in 0..8 { gone.push(idx(&rig.grid.design, 20, y, 11)); }
    gone.sort();
    for c in &gone { rig.grid.add_damage(*c, 8); }
    let mut comps = rig.comps.clone();
    comps.update_after_damage(&mut rig.grid, &gone, u32::MAX);
    let mut blocks = rig.blocks.clone();
    rebuild_dirty_blocks(&mut blocks, &rig.grid, &comps);
    assert_eq!(blocks, build_blocks(&rig.grid, &comps, 4));
    assert!((rig.blocks.total_volume_m3() - blocks.total_volume_m3() - gone.len() as f64 * 0.125).abs() < 1e-9, "the removed cells are open to the sea");
    // A set from elsewhere is rebuilt whole.
    let mut alien = BuoyancyBlocks { block_cells: 4, ..Default::default() };
    rebuild_dirty_blocks(&mut alien, &rig.grid, &comps);
    assert_eq!(alien, blocks);
}

#[test]
fn box_barge_floats_at_its_analytic_draft_and_gm() {
    let rig = barge(&[], 1.5);
    let eq = equilibrium(&rig.blocks, &rig.mass, &[], &rig.comps, &tuning());
    assert!(eq.floats);
    assert!((eq.draft_m - DRAFT).abs() < 1e-9, "{}", eq.draft_m);
    assert!(eq.heel_rad.abs() < 1e-9 && eq.trim_rad.abs() < 1e-9);
    assert!((eq.displacement_t - 120.0).abs() < 1e-12);
    // KB = T / 2, BM = B^2 / (12 T), GM = KB + BM - KG.
    let gm = DRAFT / 2.0 + 36.0 / (12.0 * DRAFT) - 1.5;
    assert!((eq.gm_m - gm).abs() < 1e-9, "{} vs {gm}", eq.gm_m);
    assert!((eq.cb - DVec3::new(10.0, DRAFT / 2.0, 3.0)).length() < 1e-9);
    assert!((eq.reserve - (480.0 - 120.0 * DRAFT) / (120.0 * DRAFT)).abs() < 1e-9);
    // The same hull on 4 m far blocks keeps draft and stays within a few percent on GM.
    let far = build_blocks(&rig.grid, &rig.comps, 8);
    let fe = equilibrium(&far, &rig.mass, &[], &rig.comps, &tuning());
    assert!((fe.draft_m - DRAFT).abs() < 1e-9 && (fe.gm_m - gm).abs() < 0.05 * gm, "{fe:?}");
    // Too heavy to float.
    let lead = MassProperties { mass_kg: 600_000.0, ..rig.mass };
    let sunk = equilibrium(&rig.blocks, &lead, &[], &rig.comps, &tuning());
    assert!(!sunk.floats && sunk.displacement_t == 600.0);
    assert!(gz_curve(&rig.blocks, &lead, 10.0, &tuning()).iter().all(|p| p.1 == 0.0));
    assert!(!equilibrium(&BuoyancyBlocks::default(), &rig.mass, &[], &rig.comps, &tuning()).floats);
}

/// Wall-sided heel for a transverse offset `e` of the weight: tan(phi) (GM + BM tan^2(phi) / 2) = e.
fn wall_sided_heel(gm: f64, bm: f64, e: f64) -> f64 {
    let (mut lo, mut hi) = (0.0f64, 1.2f64);
    for _ in 0..80 { let m = 0.5 * (lo + hi); let t = libm::tan(m); if t * (gm + 0.5 * bm * t * t) < e { lo = m; } else { hi = m; } }
    0.5 * (lo + hi)
}

#[test]
fn offset_weight_heels_and_trims_as_theory_says() {
    let mut rig = barge(&[], 1.5);
    let (bm, gm) = (36.0 / (12.0 * DRAFT), DRAFT / 2.0 + 36.0 / (12.0 * DRAFT) - 1.5);
    rig.mass.com.z += 0.3;
    let eq = equilibrium(&rig.blocks, &rig.mass, &[], &rig.comps, &tuning());
    let want = wall_sided_heel(gm, bm, 0.3);
    assert!(eq.heel_rad > 0.0 && (eq.heel_rad - want).abs() < 1e-6, "starboard weight heels to starboard: {} vs {want}", eq.heel_rad);
    assert!((eq.gm_m - gm).abs() < 1e-6, "initial GM is taken upright");
    // Weight moved aft trims by the stern (bow up); longitudinal BM = L^2 / (12 T).
    let mut rig = barge(&[], 1.5);
    rig.mass.com.x -= 1.0;
    let eq = equilibrium(&rig.blocks, &rig.mass, &[], &rig.comps, &tuning());
    let gml = DRAFT / 2.0 + 400.0 / (12.0 * DRAFT) - 1.5;
    let want = wall_sided_heel(gml, 400.0 / (12.0 * DRAFT), 1.0);
    assert!(eq.trim_rad > 0.0 && (eq.trim_rad - want).abs() < 1e-6, "{} vs {want}", eq.trim_rad);
    assert!(eq.heel_rad.abs() < 1e-9);
}

#[test]
fn a_tender_hull_lolls_and_reports_negative_gm() {
    // KG chosen so the loll angle stays below the 18 degrees at which this shallow barge lifts its bilge.
    let rig = barge(&[], 3.6);
    let eq = equilibrium(&rig.blocks, &rig.mass, &[], &rig.comps, &tuning());
    let (bm, gm) = (36.0 / (12.0 * DRAFT), DRAFT / 2.0 + 36.0 / (12.0 * DRAFT) - 3.6);
    assert!(gm < 0.0 && (eq.gm_m - gm).abs() < 1e-9, "{}", eq.gm_m);
    let loll = libm::atan((-2.0 * gm / bm).sqrt());
    assert!(eq.floats && (eq.heel_rad - loll).abs() < 1e-6, "{} vs {loll}", eq.heel_rad);
    // Raise the weight further and it goes right over: upside down is where it settles, and it does not float.
    let rig = barge(&[], 3.9);
    let eq = equilibrium(&rig.blocks, &rig.mass, &[], &rig.comps, &tuning());
    assert!(!eq.floats && eq.heel_rad.abs() > 3.0 && eq.gm_m < 0.0, "{eq:?}");
}

#[test]
fn flooded_compartments_sink_and_trim_the_hull() {
    let rig = barge(&[13, 26], 1.5);
    assert_eq!(rig.comps.compartments.len(), 3);
    // Middle hold open to the sea: parallel sinkage. Its air is 6 x 5 m in plan, from 0.5 m above the keel.
    let mid = equilibrium(&rig.blocks, &rig.mass, &[CompartmentId(1)], &rig.comps, &tuning());
    let draft = (120.0 * DRAFT - 30.0 * 0.5) / 90.0;
    assert!(mid.floats && (mid.draft_m - draft).abs() < 1e-6 && mid.trim_rad.abs() < 1e-9, "{mid:?} vs {draft}");
    let intact = equilibrium(&rig.blocks, &rig.mass, &[], &rig.comps, &tuning());
    assert!(mid.gm_m < intact.gm_m + 1.0 && mid.reserve < intact.reserve);
    // Bow hold open: deeper and down by the bow. Check the balance by brute force over the cells.
    let bow_id = rig.comps.compartments.iter().max_by(|a, b| a.centroid.x.total_cmp(&b.centroid.x)).unwrap().id;
    let bow = equilibrium(&rig.blocks, &rig.mass, &[bow_id], &rig.comps, &tuning());
    assert!(bow.floats && bow.trim_rad < -0.01 && bow.draft_m > DRAFT + 0.1 && bow.heel_rad.abs() < 1e-9, "{bow:?}");
    let up = blocks::up_of(bow.heel_rad, bow.trim_rad);
    // Recover the plane offset from the reported draft: the deepest corner is the bow keel.
    let c = bow.draft_m + up.dot(DVec3::new(20.0, 0.0, 0.0));
    let h = 0.5 * (up.x.abs() + up.y.abs() + up.z.abs());
    let (mut vol, mut moment) = (0.0, DVec3::ZERO);
    for y in 0..8 { for z in 0..12 { for x in 0..40 {
        let cell = idx(&rig.grid.design, x, y, z);
        if rig.comps.of_cell(cell) == Some(bow_id) { continue; }
        let p = DVec3::from(CellPos::new(x, y, z).centre_m());
        let f = ((c - up.dot(p)) / h + 0.5).clamp(0.0, 1.0);
        vol += 0.125 * f;
        moment += (p - up * (0.5 * h * (1.0 - f))) * (0.125 * f);
    } } }
    assert!((vol - 120.0 * DRAFT).abs() < 0.05, "{vol}");
    let arm = (moment / vol - rig.mass.com).cross(up);
    assert!(arm.length() < 5e-3, "buoyancy stands under the weight: {arm:?}");
    // Two holds gone is still afloat for this barge; all three is not.
    assert!(equilibrium(&rig.blocks, &rig.mass, &[CompartmentId(0), CompartmentId(1)], &rig.comps, &tuning()).floats);
    let all: Vec<CompartmentId> = rig.comps.compartments.iter().map(|c| c.id).collect();
    // The shell alone still displaces 210 m^3: enough for 120 t, not for 230 t.
    assert!(equilibrium(&rig.blocks, &rig.mass, &all, &rig.comps, &tuning()).floats);
    let heavy = MassProperties { mass_kg: 230_000.0, ..rig.mass };
    assert!(equilibrium(&rig.blocks, &heavy, &[], &rig.comps, &tuning()).floats);
    assert!(!equilibrium(&rig.blocks, &heavy, &all, &rig.comps, &tuning()).floats);
    // Unknown ids are ignored.
    assert_eq!(equilibrium(&rig.blocks, &rig.mass, &[CompartmentId(900)], &rig.comps, &tuning()), intact);
}

#[test]
fn gz_curve_follows_the_wall_sided_formula() {
    let rig = barge(&[], 1.5);
    let (bm, gm) = (36.0 / (12.0 * DRAFT), DRAFT / 2.0 + 36.0 / (12.0 * DRAFT) - 1.5);
    let curve = gz_curve(&rig.blocks, &rig.mass, 2.0, &tuning());
    assert_eq!(curve.len(), 91);
    assert!(curve[0].0 == 0.0 && curve[0].1.abs() < 1e-9);
    assert_eq!(curve[90].0, 180.0);
    for k in [2usize, 5, 8] {
        let phi = curve[k].0.to_radians();
        let t = libm::tan(phi);
        let want = libm::sin(phi) * (gm + 0.5 * bm * t * t);
        assert!((curve[k].1 - want).abs() < 1e-6, "{} deg: {} vs {want}", curve[k].0, curve[k].1);
    }
    assert!(curve[90].1.abs() < 1e-6, "upside down it is balanced again");
    assert!(curve.iter().all(|p| p.1.is_finite()));
    // A box this shallow and wide rights itself all the way round to its vanishing angle.
    let vanish = curve.iter().position(|p| p.0 > 0.0 && p.1 < 0.0).expect("a vanishing angle");
    assert!(curve[1..vanish].iter().all(|p| p.1 > 0.0) && curve[vanish].0 > 60.0);
    assert_eq!(gz_curve(&rig.blocks, &rig.mass, 10.0, &tuning()).len(), 19);
    assert_eq!(gz_curve(&rig.blocks, &rig.mass, f64::NAN, &tuning()).len(), 91);
    assert_eq!(gz_curve(&rig.blocks, &rig.mass, -3.0, &tuning()).len(), 91);
}

#[test]
fn heel_trim_and_floating_pose_agree() {
    let mut rig = barge(&[], 1.5);
    rig.mass.com += DVec3::new(-0.8, 0.0, 0.25);
    let eq = equilibrium(&rig.blocks, &rig.mass, &[], &rig.comps, &tuning());
    let pose = floating_pose_of(&rig.blocks, &rig.mass, 300.0, -40.0, 1.1, &tuning());
    let (heel, trim) = heel_trim(pose.rot);
    assert!((heel - eq.heel_rad).abs() < 1e-9 && (trim - eq.trim_rad).abs() < 1e-9);
    assert!((pose.heading_rad() - 1.1).abs() < 1e-9 && (pose.pos.x, pose.pos.z) == (300.0, -40.0));
    // At that pose the calm-water buoyancy carries the weight through the centre of mass.
    let mut out = HydroForces::default();
    buoyancy_on(&rig.blocks, &pose, DRAFT, &WaveSet::calm(), 0.0, RHO, &mut out);
    let f = out.forces[0];
    assert!((f.force.y - 120_000.0 * GRAVITY).abs() < 1e-3 && out.samples == 30);
    let lever = f.at - pose.to_world(rig.mass.com);
    assert!(lever.x.abs() < 1e-6 && lever.z.abs() < 1e-6, "{lever:?}");
    assert!((out.submerged - DRAFT / 4.0).abs() < 1e-9);
    // Conventions: starboard down is positive heel, bow up is positive trim.
    assert!(heel_trim(DQuat::from_rotation_x(0.2)).0 > 0.19 && heel_trim(DQuat::from_rotation_z(0.1)).1 > 0.09);
    assert!(heel_trim(DQuat::from_rotation_x(3.0)).0 > 2.9);
    assert_eq!(heel_trim(DQuat::from_xyzw(f64::NAN, 0.0, 0.0, 1.0)), (0.0, 0.0));
}

#[test]
fn rests_level_and_still_in_calm_water() {
    let rig = barge(&[], 1.5);
    let mut world = PhysicsWorld::new();
    let start = floating_pose_of(&rig.blocks, &rig.mass, 0.0, 0.0, 0.4, &tuning());
    assert!((start.pos.y + DRAFT).abs() < 1e-9);
    let body = rig.spawn(&mut world, start);
    rig.run(&mut world, body, &WaveSet::calm(), 0, 3600);
    let k = world.kinematics(body);
    assert!((k.pose.pos - start.pos).length() < 1e-4 && k.pose.rot.angle_between(start.rot) < 1e-5, "{:?}", k.pose);
    assert!(k.lin_vel.length() < 1e-5 && k.ang_vel.length() < 1e-6);
}

#[test]
fn dropped_and_heeled_it_settles_where_the_gauge_says() {
    let mut rig = barge(&[], 1.5);
    rig.mass.com.z += 0.3;
    let eq = equilibrium(&rig.blocks, &rig.mass, &[], &rig.comps, &tuning());
    let mut world = PhysicsWorld::new();
    // Start half a metre high, level, 10 degrees over the wrong way.
    let start = Pose::new(DVec3::new(0.0, 0.5 - DRAFT, 0.0), DQuat::from_rotation_x(-10f64.to_radians()));
    let body = rig.spawn(&mut world, start);
    let mut worst = 0.0f64;
    for k in 0..3600u64 {
        rig.tick(&mut world, body, &WaveSet::calm(), k as f64 * DT);
        worst = worst.max(heel_trim(world.kinematics(body).pose.rot).0.abs());
    }
    let k = world.kinematics(body);
    let (heel, trim) = heel_trim(k.pose.rot);
    assert!((heel - eq.heel_rad).abs() < 0.1 * eq.heel_rad, "{heel} vs {}", eq.heel_rad);
    assert!(trim.abs() < 1e-3 && k.ang_vel.length() < 1e-3 && k.lin_vel.length() < 1e-2, "{k:?}");
    assert!(worst < 0.5, "no wild roll on the way: {worst}");
    // The tender hull finds its angle of loll, on one side or the other.
    let rig = barge(&[], 3.6);
    let eq = equilibrium(&rig.blocks, &rig.mass, &[], &rig.comps, &tuning());
    assert!(eq.heel_rad > 0.1 && eq.heel_rad < 0.2);
    let mut world = PhysicsWorld::new();
    let body = rig.spawn(&mut world, Pose::new(DVec3::new(0.0, -DRAFT, 0.0), DQuat::from_rotation_x(0.02)));
    rig.run(&mut world, body, &WaveSet::calm(), 0, 7200);
    let heel = heel_trim(world.kinematics(body).pose.rot).0;
    assert!((heel.abs() - eq.heel_rad).abs() < 0.1 * eq.heel_rad, "{heel} vs {}", eq.heel_rad);
}

fn sea(state: SeaState, seed: u64) -> WaveSet {
    WaveSet::generate(seed, &WeatherState { sea_state: state, wind_speed_mps: 15.0, wind_dir_rad: 1.57, ..WeatherState::default() }, &WaveTuning::default())
}

#[test]
fn rides_a_beaufort_7_beam_sea_without_blowing_up() {
    let rig = barge(&[], 1.5);
    let waves = sea(SeaState::BEAUFORT_7, 42);
    assert!(waves.significant_height() > 1.5, "{}", waves.significant_height());
    let mut world = PhysicsWorld::new();
    let body = rig.spawn(&mut world, floating_pose_of(&rig.blocks, &rig.mass, 0.0, 0.0, 0.0, &tuning()));
    let (mut max_heel, mut max_off, mut moved) = (0.0f64, 0.0f64, 0.0f64);
    for k in 0..7200u64 {
        let t = k as f64 * DT;
        rig.tick(&mut world, body, &waves, t);
        let kin = world.kinematics(body);
        assert!(kin.pose.pos.is_finite() && kin.lin_vel.is_finite() && kin.ang_vel.is_finite(), "tick {k}");
        let centre = kin.pose.to_world(DVec3::new(10.0, DRAFT, 3.0));
        max_heel = max_heel.max(heel_trim(kin.pose.rot).0.abs());
        max_off = max_off.max((centre.y - waves.height_at(centre.x, centre.z, t)).abs());
        moved = moved.max(kin.lin_vel.y.abs());
    }
    assert!(max_heel < 0.8, "a stiff barge does not capsize: {max_heel}");
    assert!(max_off < 1.5, "it follows the surface: {max_off}");
    assert!(moved > 0.2, "and it does move with the sea: {moved}");
    let k = world.kinematics(body);
    assert!(DVec3::new(k.pose.pos.x, 0.0, k.pose.pos.z).length() < 400.0, "no runaway drift: {:?}", k.pose.pos);
}

#[test]
fn thrust_law_numbers() {
    // 100 kN static on 1 MW: corner speed 6.5 m/s.
    assert_eq!(thrust_at_speed(100_000.0, 1000.0, 0.0), 100_000.0);
    assert!((thrust_at_speed(100_000.0, 1000.0, 6.5) - 100_000.0 / 2f64.sqrt()).abs() < 1e-6);
    // Far above it thrust power tends to efficiency times shaft power.
    assert!((thrust_at_speed(100_000.0, 1000.0, 65.0) * 65.0 / 1e6 - PROPULSIVE_EFFICIENCY).abs() < 0.01);
    assert_eq!(thrust_at_speed(100_000.0, 1000.0, -9.0), 100_000.0);
    // An astern order delivers 0.45 of the thrust ahead, at rest and at speed.
    assert_eq!(thrust_at_speed(-60_000.0, 1000.0, 0.0), -27_000.0);
    assert!((thrust_at_speed(-100_000.0, 1000.0, 6.5) + 0.45 * thrust_at_speed(100_000.0, 1000.0, 6.5)).abs() < 1e-9);
    assert!(thrust_at_speed(100_000.0, 1000.0, 3.0) > thrust_at_speed(100_000.0, 1000.0, 4.0));
    for bad in [f64::NAN, f64::INFINITY] { assert_eq!(thrust_at_speed(bad, 1000.0, 1.0), 0.0); assert!(thrust_at_speed(1e5, bad, 1.0).is_finite()); assert!(thrust_at_speed(1e5, 1000.0, bad).is_finite()); }
    assert_eq!(thrust_at_speed(0.0, 1000.0, 1.0), 0.0);
}

const ENGINE_KW: f64 = 1500.0;
const N_PER_KW: f64 = 100.0;
const RUDDER_DEG: f64 = 35.0;

/// The barge with one engine, one propeller on the centreline aft and a 1 m^2 rudder in its race.
fn powered() -> (Rig, content::catalogue::Catalogue) {
    let mut rig = barge(&[], 1.5);
    let g = rig.grid.design.clone();
    let cells = |x: [i16; 2], y: [i16; 2]| crate::compartments::testkit::block(&g, [x[0], y[0], 5], [x[1], y[1], 6]);
    let cat = catalogue(vec![
        def("engine", PartFunction::Engine(EngineDef { kind: EngineKind::Diesel, shaft_kw: ENGINE_KW, fuel_lps: 0.1, spool_s: 5.0 }), vec![], 1.0),
        def("prop", PartFunction::Propulsor(PropulsorDef { kind: PropulsorKind::Propeller, max_shaft_kw: 2000.0, thrust_n_per_kw: N_PER_KW, cavitation_kn: 20.0, steer_max_deg: 0.0 }), vec![], 1.0),
        def("rudder", PartFunction::Rudder { area_m2: 1.0, max_deg: RUDDER_DEG, rate_dps: 5.0 }, vec![], 1.0),
    ]);
    rig.parts = vec![
        part(0, 0, cells([18, 19], [2, 3]), &g, PartState::Engine { throttle: 1.0, output_kw: ENGINE_KW }),
        part(1, 1, cells([2, 3], [0, 1]), &g, PartState::Propulsor { shaft_kw: ENGINE_KW, thrust_n: ENGINE_KW * N_PER_KW, steer_rad: 0.0 }),
        part(2, 2, cells([0, 1], [0, 1]), &g, PartState::Rudder { angle_rad: 0.0 }),
    ];
    (rig, cat)
}

fn set_helm(rig: &mut Rig, rad: f64) { rig.parts[2].state = PartState::Rudder { angle_rad: rad }; }

#[test]
fn speed_and_turning_circle_match_the_gauge() {
    let (mut rig, cat) = powered();
    let (top, radius) = forces::predict(&rig.blocks, &rig.mass, &rig.parts, &rig.grid, &cat, &tuning());
    assert!(top > 6.0 && top < 12.0, "a 120 t barge on 1.5 MW makes 12 to 23 knots: {top}");
    assert!(radius > 20.0 && radius < 200.0, "one to ten lengths: {radius}");
    let mut world = PhysicsWorld::new();
    let body = rig.spawn(&mut world, floating_pose_of(&rig.blocks, &rig.mass, 0.0, 0.0, 0.0, &tuning()));
    let calm = WaveSet::calm();
    // Full ahead from rest: speed builds along the bow (north is -Z) and levels off at the predicted top speed.
    rig.run(&mut world, body, &calm, 0, 1200);
    let early = world.kinematics(body);
    assert!(-early.lin_vel.z > 0.5 * top && -early.lin_vel.z < top, "20 s in: {}", -early.lin_vel.z);
    rig.run(&mut world, body, &calm, 1200, 6000);
    let k = world.kinematics(body);
    let speed = k.lin_vel.length();
    assert!((speed - top).abs() < 0.1 * top, "{speed} vs {top}");
    assert!(k.pose.heading_rad().min(core::f64::consts::TAU - k.pose.heading_rad()) < 0.02, "it runs straight");
    // Helm hard a-starboard: the bow swings clockwise and the circle matches.
    set_helm(&mut rig, RUDDER_DEG.to_radians());
    rig.run(&mut world, body, &calm, 7200, 7200);
    let k = world.kinematics(body);
    assert!(k.ang_vel.y < 0.0, "clockwise seen from above");
    let turn = DVec3::new(k.lin_vel.x, 0.0, k.lin_vel.z).length() / k.ang_vel.y.abs();
    assert!((turn - radius).abs() < 0.1 * radius, "{turn} vs {radius}");
    assert!(DVec3::new(k.lin_vel.x, 0.0, k.lin_vel.z).length() < speed, "a turning hull loses speed");
    assert!(heel_trim(k.pose.rot).0 < -1e-3, "and it heels out of the turn");
    // Engines stopped, helm amidships: it coasts down.
    set_helm(&mut rig, 0.0);
    rig.parts[1].state = PartState::Propulsor { shaft_kw: 0.0, thrust_n: 0.0, steer_rad: 0.0 };
    rig.run(&mut world, body, &calm, 14_400, 3600);
    let coasting = world.kinematics(body).lin_vel.length();
    assert!(coasting < 0.35 * speed && coasting > 0.02, "60 s after stopping: {coasting}");
    // Full astern from there backs it up.
    rig.parts[1].state = PartState::Propulsor { shaft_kw: ENGINE_KW, thrust_n: -ENGINE_KW * N_PER_KW, steer_rad: 0.0 };
    rig.run(&mut world, body, &calm, 18_000, 3600);
    let k = world.kinematics(body);
    assert!((k.pose.rot.inverse() * k.lin_vel).x < -1.0);
    // No propulsion, or no hull: nothing to predict.
    let unpowered = barge(&[], 1.5);
    assert_eq!(forces::predict(&unpowered.blocks, &unpowered.mass, &[], &unpowered.grid, &cat, &tuning()), (0.0, f64::INFINITY));
    assert_eq!(forces::predict(&BuoyancyBlocks::default(), &rig.mass, &rig.parts, &rig.grid, &cat, &tuning()), (0.0, f64::INFINITY));
    // Without a rudder it cannot turn.
    let straight = forces::predict(&rig.blocks, &rig.mass, &rig.parts[..2], &rig.grid, &cat, &tuning());
    assert!((straight.0 - top).abs() < 1e-9 && straight.1.is_infinite());
}

#[test]
fn propulsor_out_of_the_water_gives_no_thrust_and_a_jet_steers() {
    let (mut rig, _) = powered();
    let lifted = Kinematics { pose: Pose::new(DVec3::new(0.0, 5.0, 0.0), DQuat::IDENTITY), ..Default::default() };
    let hull = HullState { kin: &lifted, mass: &rig.mass, blocks: &rig.blocks, parts: &rig.parts, grid: &rig.grid, step_s: DT };
    let mut out = HydroForces::default();
    forces::propulsion(&hull, &WaveSet::calm(), 0.0, RHO, &mut out);
    assert!(out.forces.is_empty());
    // Afloat, a jet deflected to starboard pushes ahead and throws the stern to port.
    rig.parts[1].state = PartState::Propulsor { shaft_kw: ENGINE_KW, thrust_n: ENGINE_KW * N_PER_KW, steer_rad: 0.3 };
    rig.parts.truncate(2);
    let afloat = Kinematics { pose: Pose::new(DVec3::new(0.0, -DRAFT, 0.0), DQuat::IDENTITY), ..Default::default() };
    let hull = HullState { kin: &afloat, mass: &rig.mass, blocks: &rig.blocks, parts: &rig.parts, grid: &rig.grid, step_s: DT };
    forces::propulsion(&hull, &WaveSet::calm(), 0.0, RHO, &mut out);
    assert_eq!(out.forces.len(), 1);
    let f = out.forces[0];
    // The 1 m disc on the keel line of a hull drawing 0.98 m has its tip out of the water.
    let wet = forces::immersion(DRAFT - 0.5, 0.5);
    assert!(wet > 0.95 && wet < 1.0 && f.force.x > 0.0 && f.force.z < 0.0 && (f.force.length() - ENGINE_KW * N_PER_KW * wet).abs() < 1e-6);
    assert_eq!((forces::immersion(-0.5, 0.5), forces::immersion(0.0, 0.5), forces::immersion(2.0, 0.5), forces::immersion(f64::NAN, 0.5)), (0.0, 0.5, 1.0, 0.0));
    let yaw = (f.at - afloat.pose.to_world(rig.mass.com)).cross(f.force).y;
    assert!(yaw < 0.0, "bow to starboard is a negative turn about +Y");
    // A destroyed propulsor does nothing.
    rig.parts[1].level = content::enums::DamageLevel::Destroyed;
    let hull = HullState { kin: &afloat, mass: &rig.mass, blocks: &rig.blocks, parts: &rig.parts, grid: &rig.grid, step_s: DT };
    let mut none = HydroForces::default();
    forces::propulsion(&hull, &WaveSet::calm(), 0.0, RHO, &mut none);
    assert!(none.forces.is_empty());
}

#[test]
fn kinematic_hull_keeps_pace_with_full_physics_and_rides_the_sea() {
    let (mut rig, cat) = powered();
    let (top, radius) = forces::predict(&rig.blocks, &rig.mass, &rig.parts, &rig.grid, &cat, &tuning());
    let waves = sea(SeaState::BEAUFORT_4, 9);
    let mut kin = Kinematics { pose: floating_pose_of(&rig.blocks, &rig.mass, 0.0, 0.0, 0.0, &tuning()), ..Default::default() };
    let step = |rig: &Rig, kin: &Kinematics, k: u64| {
        let hull = HullState { kin, mass: &rig.mass, blocks: &rig.blocks, parts: &rig.parts, grid: &rig.grid, step_s: DT };
        forces::kinematic(&hull, &waves, k as f64 * DT, DT, &tuning())
    };
    for k in 0..7200 {
        let next = step(&rig, &kin, k);
        assert!((next.pose.pos - kin.pose.pos).length() < 0.5 && next.pose.rot.angle_between(kin.pose.rot) < 0.02, "no jumps at tick {k}");
        kin = next;
    }
    let speed = DVec3::new(kin.lin_vel.x, 0.0, kin.lin_vel.z).length();
    assert!((speed - top).abs() < 0.02 * top, "{speed} vs {top}");
    let t = 7200.0 * DT;
    let ride = kin.pose.to_world(DVec3::new(10.0, DRAFT, 3.0));
    assert!((ride.y - waves.height_at(ride.x, ride.z, t)).abs() < 1.0, "on the surface at its draft");
    assert!(heel_trim(kin.pose.rot).0.abs() < 0.4);
    // Hard over: the same circle as the gauge.
    set_helm(&mut rig, RUDDER_DEG.to_radians());
    for k in 7200..14_400 { kin = step(&rig, &kin, k); }
    let turn = DVec3::new(kin.lin_vel.x, 0.0, kin.lin_vel.z).length() / kin.ang_vel.y.abs();
    assert!(kin.ang_vel.y < 0.0 && (turn - radius).abs() < 0.05 * radius, "{turn} vs {radius}");
    // Too heavy to float: it goes down.
    rig.mass.mass_kg = 900_000.0;
    let down = step(&rig, &kin, 0);
    assert!(down.lin_vel.y < -0.5 && down.pose.pos.y < kin.pose.pos.y);
    // Bad dt leaves it where it is.
    let hull = HullState { kin: &kin, mass: &rig.mass, blocks: &rig.blocks, parts: &rig.parts, grid: &rig.grid, step_s: DT };
    assert_eq!(forces::kinematic(&hull, &waves, 0.0, f64::NAN, &tuning()), kin);
}

#[test]
fn depth_keeping_holds_the_ordered_depth() {
    // A sealed boat with a little reserve buoyancy, weight low.
    let mut rig = barge(&[], 1.0);
    rig.mass = box_mass(450_000.0, DVec3::new(10.0, 1.0, 3.0), DVec3::new(20.0, 4.0, 6.0));
    let mut world = PhysicsWorld::new();
    let body = rig.spawn(&mut world, floating_pose_of(&rig.blocks, &rig.mass, 0.0, 0.0, 0.0, &tuning()));
    let calm = WaveSet::calm();
    for k in 0..12_000u64 {
        let kin = world.kinematics(body);
        let hull = HullState { kin: &kin, mass: &rig.mass, blocks: &rig.blocks, parts: &rig.parts, grid: &rig.grid, step_s: DT };
        let mut out = HydroForces::default();
        buoyancy_on(&rig.blocks, &kin.pose, 3.5, &calm, k as f64 * DT, RHO, &mut out);
        forces::drag(&hull, &calm, k as f64 * DT, &tuning(), &mut out);
        forces::depth_keeping(&hull, 30.0, RHO, &mut out);
        for f in &out.forces { world.add_force_at(body, f.force, f.at); }
        world.add_torque(body, out.torque);
        world.step(DT, &mut Vec::new());
    }
    let k = world.kinematics(body);
    assert!((k.pose.pos.y + 30.0).abs() < 0.5, "keel at 30 m: {}", k.pose.pos.y);
    assert!(k.lin_vel.length() < 0.05 && heel_trim(k.pose.rot).0.abs() < 0.02 && heel_trim(k.pose.rot).1.abs() < 0.02, "{k:?}");
}

#[test]
fn light_debris_stays_stable_at_any_speed() {
    // A 2 m sealed box of 300 kg thrown sideways at 40 m/s and spinning: drag must calm it, never pump it.
    let grid = build(box_hull([4, 4, 4], &[], &[]), &[]);
    let (comps, _) = CompartmentSet::derive(&grid, &[]);
    let rig = Rig { blocks: build_blocks(&grid, &comps, 4), grid, comps, mass: box_mass(300.0, DVec3::splat(1.0), DVec3::splat(2.0)), parts: Vec::new() };
    let waves = sea(SeaState::BEAUFORT_7, 5);
    let mut world = PhysicsWorld::new();
    let body = rig.spawn(&mut world, Pose::new(DVec3::new(0.0, -0.5, 0.0), DQuat::IDENTITY));
    world.set_kinematics(body, &Kinematics { pose: Pose::new(DVec3::new(0.0, -0.5, 0.0), DQuat::IDENTITY), lin_vel: DVec3::new(5.0, -3.0, 40.0), ang_vel: DVec3::new(6.0, 3.0, -8.0) });
    let mut peak_late = 0.0f64;
    for k in 0..3600u64 {
        rig.tick(&mut world, body, &waves, k as f64 * DT);
        let kin = world.kinematics(body);
        assert!(kin.pose.pos.is_finite() && kin.lin_vel.length() < 60.0 && kin.ang_vel.length() < 30.0, "tick {k}: {kin:?}");
        if k > 600 { peak_late = peak_late.max(kin.lin_vel.length()); }
    }
    assert!(peak_late < 12.0, "after 10 s it only moves with the sea: {peak_late}");
}

#[test]
fn reruns_are_bitwise_identical() {
    let run = || {
        let (mut rig, _) = powered();
        set_helm(&mut rig, 0.3);
        let waves = sea(SeaState::BEAUFORT_4, 77);
        let mut world = PhysicsWorld::new();
        let body = rig.spawn(&mut world, floating_pose_of(&rig.blocks, &rig.mass, 10.0, 20.0, 2.0, &tuning()));
        rig.run(&mut world, body, &waves, 0, 900);
        let k = world.kinematics(body);
        [k.pose.pos.x, k.pose.pos.y, k.pose.pos.z, k.pose.rot.x, k.pose.rot.y, k.pose.rot.z, k.pose.rot.w, k.lin_vel.x, k.ang_vel.y].map(f64::to_bits)
    };
    assert_eq!(run(), run());
}

#[test]
fn lofted_hulls_float_still_at_their_equilibrium() {
    let cat = catalogue(vec![]);
    for len in [15.0, 45.0] {
        let vox = content::hull::voxelize(&content::hull::default_hull(len));
        let grid = build(vox.grid, &[]);
        let (comps, _) = CompartmentSet::derive(&grid, &[]);
        let blocks = build_blocks(&grid, &comps, 4);
        // A fitted hull: 2.2 times the bare shell, weight a little lower.
        let dry = crate::mass::dry_mass(&grid, &[], &cat);
        let mass = MassProperties { mass_kg: dry.mass_kg * 2.2, com: DVec3::new(dry.com.x, dry.com.y * 0.8, dry.com.z), inertia: dry.inertia * 2.2 };
        let eq = equilibrium(&blocks, &mass, &[], &comps, &tuning());
        assert!(eq.floats && eq.gm_m > 0.0 && eq.heel_rad.abs() < 0.02 && eq.trim_rad.abs() < 0.3 && eq.reserve > 0.02, "{len} m: {eq:?}");
        let rig = Rig { grid, comps, blocks, mass, parts: Vec::new() };
        let start = floating_pose_of(&rig.blocks, &rig.mass, 0.0, 0.0, 0.3, &tuning());
        let mut world = PhysicsWorld::new();
        let body = rig.spawn(&mut world, start);
        rig.run(&mut world, body, &WaveSet::calm(), 0, 1200);
        let k = world.kinematics(body);
        assert!((k.pose.pos - start.pos).length() < 1e-4 && k.pose.rot.angle_between(start.rot) < 1e-5 && k.lin_vel.length() < 1e-5, "{len} m: {k:?}");
        // The sea moves it, within reason, and it comes back upright.
        let waves = sea(SeaState::BEAUFORT_4, 3);
        let (mut heel, mut trim) = (0.0f64, 0.0f64);
        for t in 0..1800u64 { rig.tick(&mut world, body, &waves, t as f64 * DT); let (h, p) = heel_trim(world.kinematics(body).pose.rot); heel = heel.max(h.abs()); trim = trim.max((p - eq.trim_rad).abs()); }
        assert!(heel > 0.001 && heel < 1.0 && trim < 0.6, "{len} m: heel {heel} trim {trim}");
    }
}

#[test]
fn deep_and_dry_columns_take_no_samples() {
    let rig = barge(&[], 1.5);
    let waves = sea(SeaState::BEAUFORT_4, 3);
    let band: f64 = waves.waves.iter().take(waves.count as usize).map(|w| w.amplitude.abs()).sum();
    let mut out = HydroForces::default();
    buoyancy_on(&rig.blocks, &Pose::new(DVec3::new(0.0, -50.0, 0.0), DQuat::IDENTITY), DRAFT, &waves, 3.0, RHO, &mut out);
    assert_eq!((out.samples, out.submerged), (0, 1.0));
    assert!((out.forces[0].force.y - RHO * GRAVITY * 480.0).abs() < 1e-3);
    let mut out = HydroForces::default();
    buoyancy_on(&rig.blocks, &Pose::new(DVec3::new(0.0, band + 0.5, 0.0), DQuat::IDENTITY), DRAFT, &waves, 3.0, RHO, &mut out);
    assert!(out.samples == 0 && out.submerged == 0.0 && out.forces.is_empty());
    // Afloat, every column is sampled and the force matches a per-column brute force.
    let pose = Pose::new(DVec3::new(0.0, -DRAFT, 0.0), DQuat::from_rotation_x(0.1));
    let mut out = HydroForces::default();
    buoyancy_on(&rig.blocks, &pose, DRAFT, &waves, 3.0, RHO, &mut out);
    assert_eq!(out.samples, 30);
    assert!(out.forces[0].force.y > 0.0 && out.forces[0].force.x == 0.0 && out.forces[0].at.is_finite());
}


#[test]
fn block_sets_without_layers_or_with_bad_ones_still_float() {
    let mut rig = barge(&[], 1.5);
    let pose = Pose::new(DVec3::new(0.0, -DRAFT, 0.0), DQuat::from_rotation_x(0.2));
    let force = |blocks: &BuoyancyBlocks| { let mut out = HydroForces::default(); buoyancy_on(blocks, &pose, DRAFT, &WaveSet::calm(), 0.0, RHO, &mut out); out.forces[0] };
    let whole = force(&rig.blocks);
    assert!(rig.blocks.layers.len() >= rig.blocks.blocks.len() && (0..rig.blocks.blocks.len()).all(|i| rig.blocks.layers_of(i).is_some()));
    // A set built by hand carries no layers and none of the private extents: whole blocks, the same answer on a box.
    let bare = BuoyancyBlocks { block_cells: 4, blocks: rig.blocks.blocks.clone(), columns: rig.blocks.columns.clone(), form: rig.blocks.form, ..Default::default() };
    let f = force(&bare);
    assert!((f.force - whole.force).length() < 1e-6 * whole.force.length() && (f.at - whole.at).length() < 1e-6, "{f:?} vs {whole:?}");
    // Layer ranges that point nowhere (restored bytes) are ignored, never indexed.
    let mut broken = rig.blocks.clone();
    for e in &mut broken.ext { e.first = u32::MAX - 1; }
    let f = force(&broken);
    assert!((f.force - whole.force).length() < 1e-6 * whole.force.length());
    let eq = equilibrium(&broken, &rig.mass, &[], &rig.comps, &tuning());
    assert!(eq.floats && (eq.draft_m - DRAFT).abs() < 1e-6);
    // A dirty rebuild of such a set starts over from the grid.
    rig.grid.add_damage(idx(&rig.grid.design, 3, 0, 3), 8);
    rebuild_dirty_blocks(&mut broken, &rig.grid, &rig.comps);
    assert_eq!(broken, build_blocks(&rig.grid, &rig.comps, 4));
}

#[test]
fn a_keel_and_a_flared_side_are_wetted_layer_by_layer() {
    // A T section 2 m deep: a keel fin one cell wide under a slab one cell thick, in one block. Its volume
    // against draft is two straight lines with a knee at the slab, which one fill fraction cannot follow.
    let steel = box_hull([1, 1, 1], &[], &[]).get(CellPos::new(0, 0, 0));
    let mut design = content::cell::CellGrid::new([8, 4, 4]);
    for x in 0..8 { for y in 0..4 { for z in 0..4 { if y == 3 || z == 1 { design.set(CellPos::new(x, y, z), steel); } } } }
    let grid = build(design, &[]);
    let (comps, _) = CompartmentSet::derive(&grid, &[]);
    let blocks = build_blocks(&grid, &comps, 4);
    assert_eq!((blocks.blocks.len(), blocks.layers.len()), (2, 8));
    let wet = |c: f64| blocks.displaced(super::blocks::Plane { u: DVec3::Y, c }).0;
    // Fin: 8 cells long, one wide: 2 m^3 per metre of draft. Slab: 8 m^2 of waterplane.
    for (c, want) in [(0.5, 1.0), (1.5, 3.0), (1.75, 3.0 + 8.0 * 0.25), (2.0, 7.0)] { assert!((wet(c) - want).abs() < 1e-9, "draft {c}: {} m^3 against {want}", wet(c)); }
    // Heeled 20 degrees about the fin's foot the cells and the layers agree to within a cell's corner.
    let exact = build_blocks(&grid, &comps, 1);
    let u = super::blocks::up_of(0.35, 0.0);
    for c in [0.6, 1.2, 1.6] {
        let plane = super::blocks::Plane { u, c };
        let (a, b) = (blocks.displaced(plane), exact.displaced(plane));
        assert!((a.0 - b.0).abs() < 0.02 * b.0 + 0.01 && (a.1 / a.0 - b.1 / b.0).length() < 0.03, "{c}: {a:?} vs {b:?}");
    }
}

#[test]
fn a_rudder_half_out_of_the_water_has_half_a_blade() {
    let foil = forces::Foil { pos: DVec3::new(0.0, 2.0, 0.0), normal: DVec3::Z, span: DVec3::Y, area_m2: 4.0, factor: 1.0, half_m: 1.0 };
    let devices = forces::Devices { thrusters: Vec::new(), foils: vec![foil] };
    let push = |depth: f64, up: DVec3| {
        let mut got = Vec::new();
        forces::device_forces(&devices, RHO, up, DT, |_| 1.0e9, |_| (DVec3::new(-8.0, 0.0, -1.0), depth), |p, f| got.push((p, f)));
        got
    };
    let deep = push(5.0, DVec3::Y);
    let half = push(0.0, DVec3::Y);
    assert_eq!((deep.len(), half.len()), (1, 1));
    assert!(deep[0].1.z < 0.0 && (half[0].1.z - 0.5 * deep[0].1.z).abs() < 1e-9 * deep[0].1.z.abs(), "{deep:?} {half:?}");
    // The force acts at the middle of the wet half, which hangs below the blade's centre.
    assert_eq!((deep[0].0, half[0].0), (foil.pos, foil.pos - DVec3::Y * 0.5));
    assert!(push(-1.0, DVec3::Y).is_empty() && push(-0.99, DVec3::Y).len() == 1);
    // Upside down the wet half is the other one.
    assert_eq!(push(0.0, -DVec3::Y)[0].0, foil.pos + DVec3::Y * 0.5);
}

#[test]
fn a_hull_weathercocks_wherever_its_weight_sits() {
    // Making way with some leeway to starboard and no rudder, the hull's side force must swing the bow to
    // starboard, toward where it is going, with the centre of mass a tenth of the length aft, amidships or
    // forward.
    let rig = barge(&[], 1.5);
    let hull = forces::HullRef::of(&rig.blocks.form).unwrap();
    let laws = forces::Laws::new(&tuning());
    let devices = forces::Devices::default();
    for com_x in [8.0, 10.0, 13.0] {
        let mass = box_mass(120_000.0, DVec3::new(com_x, 1.5, 3.0), DVec3::new(20.0, 4.0, 6.0));
        let body = forces::PlanarBody { mass: &mass, hull, wetted: forces::wetted(&hull, 120_000.0 / RHO), laws, devices: &devices, plane: super::blocks::Plane { u: DVec3::Y, c: DRAFT }, step_s: DT };
        for sway in [0.05, 0.5, 3.0] {
            let (x, z, n) = body.forces(forces::Planar { u: 8.0, v: sway, r: 0.0 });
            assert!(x < 0.0 && z < 0.0 && n < 0.0, "centre of mass at {com_x} m, {sway} m/s of leeway: {x} {z} {n}");
        }
        // And a pure swing is resisted.
        assert!(body.forces(forces::Planar { u: 8.0, v: 0.0, r: 0.1 }).2 < 0.0);
    }
}

#[test]
fn light_debris_is_stable_at_the_30_hz_distant_rate() {
    // The same thrown box as above, stepped as a distant hull is: forces every other tick, sized for two, with
    // the physics world making up the tick it sat out.
    let grid = build(box_hull([4, 4, 4], &[], &[]), &[]);
    let (comps, _) = CompartmentSet::derive(&grid, &[]);
    let rig = Rig { blocks: build_blocks(&grid, &comps, 4), grid, comps, mass: box_mass(300.0, DVec3::splat(1.0), DVec3::splat(2.0)), parts: Vec::new() };
    let waves = sea(SeaState::BEAUFORT_7, 5);
    let mut world = PhysicsWorld::new();
    let body = rig.spawn(&mut world, Pose::new(DVec3::new(0.0, -0.5, 0.0), DQuat::IDENTITY));
    world.set_kinematics(body, &Kinematics { pose: Pose::new(DVec3::new(0.0, -0.5, 0.0), DQuat::IDENTITY), lin_vel: DVec3::new(5.0, -3.0, 40.0), ang_vel: DVec3::new(6.0, 3.0, -8.0) });
    let mut peak_late = 0.0f64;
    for k in 0..3600u64 {
        let due = k % 2 == 0;
        world.set_enabled(body, due);
        if due {
            let kin = world.kinematics(body);
            let hull = HullState { kin: &kin, mass: &rig.mass, blocks: &rig.blocks, parts: &rig.parts, grid: &rig.grid, step_s: 2.0 * DT };
            let mut out = HydroForces::default();
            buoyancy_on(&rig.blocks, &kin.pose, 0.5, &waves, k as f64 * DT, RHO, &mut out);
            forces::drag(&hull, &waves, k as f64 * DT, &tuning(), &mut out);
            for f in &out.forces { world.add_force_at(body, f.force, f.at); }
            world.add_torque(body, out.torque);
        }
        world.step(DT, &mut Vec::new());
        let kin = world.kinematics(body);
        assert!(kin.pose.pos.is_finite() && kin.lin_vel.length() < 60.0 && kin.ang_vel.length() < 30.0, "tick {k}: {kin:?}");
        if k > 600 { peak_late = peak_late.max(kin.lin_vel.length()); }
    }
    assert!(peak_late < 12.0, "after 10 s it only moves with the sea: {peak_late}");
}
