//! Unit `data-starter-hulls`: the six starter hulls and the tutorial's 22 m variants sailed in the simulation
//! (DESIGN.md L142-151 starters, L166 GM bands, L252-275 the first ten minutes, L650 buoyancy blocks).
//!
//! `crates/content/tests/starter_hulls.rs` checks the committed files against their generator, the rulesets
//! and the generator's cell-exact hydrostatics. This file checks the same embedded files the way the editor and
//! a sortie see them: built into a prefab by sim-core, gauged by the editor's own gauges, spawned into a world
//! and stepped. The Float gauge and the simulation solve buoyancy on 2 m blocks (L650); `report` prints the
//! one-cell solve beside it, and the starters are tuned so the gauge the player sees reads Pass with margin.
//!
//! `cargo test -p sim-systems --test starter_hulls_sim report -- --ignored --nocapture` prints every number
//! these checks read, for retuning a hull.
use content::blueprint::Blueprint;
use content::campaign::operation::ConstraintTag;
use content::campaign::MapDef;
use content::catalogue::Catalogue;
use content::cell::CELL_M;
use content::enums::Act;
use content::ids::SideId;
use content::part::{PartFunction, PropulsorKind};
use content::weather::{SeaState, WeatherState};
use glam::DVec3;
use sim_core::parts::VesselControls;
use sim_core::vessel::{build_prefab, Fidelity, Vessel, VesselPrefab, VesselSpawn, VesselStatus};
use sim_core::{EntityId, Pose, World, WorldConfig};
use sim_systems::editor::gauges::{endurance_gauge, float_gauge, power_gauge, resilience_gauge, speed_gauge, FloatGauge};
use sim_systems::editor::Check;
use std::sync::Arc;

const STARTERS: [(&str, Act); 6] = [
    ("starters/patrol15.bp.ron", Act::Act1),
    ("starters/fac22.bp.ron", Act::Act1),
    ("starters/insertion26.bp.ron", Act::Act1),
    ("starters/corvette40.bp.ron", Act::Act2),
    ("starters/corvette60.bp.ron", Act::Act3),
    ("starters/destroyer90.bp.ron", Act::Act4),
];
const TUTORIAL: [&str; 4] = ["tutorial/fac22_tutorial.bp.ron", "tutorial/fac22_tutorial_float.bp.ron", "tutorial/fac22_tutorial_drive.bp.ron", "tutorial/fac22_tutorial_nogun.bp.ron"];

/// Ticks per second of simulated time.
const HZ: usize = sim_core::TICK_HZ as usize;
/// Knots per metre per second.
const KN: f64 = 1.943_844;

fn catalogue() -> Arc<Catalogue> { Arc::new(Catalogue::load_embedded().expect("embedded catalogue")) }

fn stock(path: &str) -> Blueprint { content::campaign::load::stock_blueprint(path).unwrap_or_else(|e| panic!("{path}: {e}")) }

fn world(cat: &Arc<Catalogue>) -> World { world_in(cat, SeaState::CALM) }

/// A world with the wind (and the sea it raises) running east, on the beam of a hull heading north.
fn world_in(cat: &Arc<Catalogue>, sea: SeaState) -> World {
    let weather = WeatherState { sea_state: sea, wind_dir_rad: core::f64::consts::FRAC_PI_2, ..WeatherState::default() };
    World::new(WorldConfig { seed: 0x5743_5254, weather, map: MapDef { size_m: [40_000.0, 40_000.0], depth_m: 500.0, ..MapDef::default() }, catalogue: cat.clone() })
}

/// Worst heel in degrees over `seconds` of a Beaufort 7 beam sea at full load, and the status at the end.
fn beam_sea(cat: &Arc<Catalogue>, prefab: &Arc<VesselPrefab>, seconds: usize) -> (f64, VesselStatus) {
    let mut w = world_in(cat, SeaState::BEAUFORT_7);
    let id = spawn(&mut w, prefab, 1.0);
    let mut worst = 0.0f64;
    for _ in 0..seconds * HZ {
        w.step();
        let Some(v) = w.vessel(id) else { break };
        worst = worst.max(degrees(v).0.abs());
    }
    (worst, w.vessel(id).map_or(VesselStatus::Disabled, |v| v.status))
}

/// Spawn at the origin heading north, with fuel and ammunition at the given fractions.
fn spawn(w: &mut World, prefab: &Arc<VesselPrefab>, load: f64) -> EntityId {
    w.spawn_vessel(&VesselSpawn {
        prefab: prefab.clone(), side: SideId(0), player: None, class: None, pose: Pose::from_heading(DVec3::ZERO, 0.0), speed_mps: 0.0,
        damage: None, fuel_fraction: load, ammo_fraction: load, fidelity: Fidelity::Full, submersible: false,
    })
}

fn run(w: &mut World, seconds: usize) {
    for _ in 0..seconds * HZ {
        w.step();
    }
}

/// Vessel-local top-centre points of every propeller, in metres.
fn propeller_tops(bp: &Blueprint, cat: &Catalogue) -> Vec<DVec3> {
    bp.parts
        .iter()
        .filter_map(|p| {
            let def = cat.part(&p.part)?;
            let PartFunction::Propulsor(d) = &def.function else { return None };
            if d.kind != PropulsorKind::Propeller {
                return None;
            }
            let s = content::kit::rotated_size(def.size, p.rot);
            Some(DVec3::new((p.pos.x as f64 + 0.5 * s[0] as f64) * CELL_M, (p.pos.y as f64 + s[1] as f64) * CELL_M, (p.pos.z as f64 + 0.5 * s[2] as f64) * CELL_M))
        })
        .collect()
}

/// Depth under the surface of the shallowest propeller tip, metres (infinite for a jet boat).
fn prop_depth(v: &Vessel, tops: &[DVec3]) -> f64 { tops.iter().map(|p| -v.to_world(*p).y).fold(f64::INFINITY, f64::min) }

fn degrees(v: &Vessel) -> (f64, f64) {
    let (h, t) = v.heel_trim();
    (h.to_degrees(), t.to_degrees())
}

/// The Float gauge at the act's tonnage cap.
fn float(prefab: &VesselPrefab, act: Act, cat: &Catalogue) -> FloatGauge { float_gauge(prefab, act.hull_cap_t(), 2.0, cat) }

/// Every starter reads Pass on the editor's fast-path and background gauges, settles upright and level with
/// its screws well under water, runs straight at full power, rides out a Beaufort 7 beam sea, and stays upright
/// with its tanks and magazines nearly empty.
#[test]
fn starters_sail_upright_and_straight() {
    let cat = catalogue();
    let st = &cat.tuning.stability;
    let mut speeds = Vec::new();
    for (path, act) in STARTERS {
        let bp = stock(path);
        let prefab = Arc::new(build_prefab(&bp, &cat));
        let f = float(&prefab, act, &cat);
        assert!(f.floats && f.check == Check::Pass, "{path}: Float {:?}, GM {:.2} m = {:.1}% of beam", f.check, f.gm_m, 100.0 * f.gm_over_beam);
        // Margin inside the band, so a refit or a burnt-off tank does not tip the gauge over.
        assert!((st.gm_caution_low + 0.005..=st.gm_caution_high - 0.005).contains(&f.gm_over_beam), "{path}: GM {:.1}% of beam", 100.0 * f.gm_over_beam);
        assert!(f.heel_deg.abs() <= 1.0 && f.trim_deg.abs() <= 1.5, "{path}: gauge heel {:.2} trim {:.2}", f.heel_deg, f.trim_deg);
        // Righting arm to well past the 30 degrees of the tender test (L170).
        assert!(f.range_of_stability_deg >= 45.0, "{path}: range of stability {:.0} deg", f.range_of_stability_deg);
        let s = speed_gauge(&prefab, &cat);
        let p = power_gauge(&prefab, &cat);
        let e = endurance_gauge(&prefab, &cat);
        let r = resilience_gauge(&prefab, &cat);
        for (name, check) in [("Speed", s.check), ("Power", p.check), ("Endurance", e.check), ("Resilience", r.check)] {
            assert_eq!(check, Check::Pass, "{path}: {name} gauge");
        }
        assert!(r.one_flooded_survival >= 1.0, "{path}: survives every single flooded compartment ({:.2})", r.one_flooded_survival);

        // Calm water at full load: settled, upright, dry, screws immersed.
        let tops = propeller_tops(&bp, &cat);
        let mut w = world(&cat);
        let id = spawn(&mut w, &prefab, 1.0);
        run(&mut w, 10);
        let v = w.vessel(id).unwrap();
        let (heel, trim) = degrees(v);
        assert_eq!(v.status, VesselStatus::Active, "{path}");
        assert!(heel.abs() <= 1.0 && trim.abs() <= 1.5, "{path}: lies at heel {heel:.2} trim {trim:.2} deg");
        assert!(v.compartments.compartments.iter().all(|c| c.water_m3 == 0.0), "{path}: takes water at rest");
        assert!(prop_depth(v, &tops) >= 0.25, "{path}: a propeller tip is {:.2} m under", prop_depth(v, &tops));

        // Full ahead with the helm amidships for a minute: no yaw away from the course, no list, screws still
        // biting, and a warship's speed.
        w.set_controls(id, VesselControls { throttle: 1.0, ..Default::default() });
        run(&mut w, 60);
        let v = w.vessel(id).unwrap();
        let (heel, trim) = degrees(v);
        let kn = v.forward_speed() * KN;
        let at = v.kin.pose.pos;
        let run_m = (at.x * at.x + at.z * at.z).sqrt();
        assert!(kn >= 20.0, "{path}: {kn:.1} kn at full power");
        assert!(at.x.abs() <= 0.05 * run_m && v.kin.ang_vel.y.abs() < 0.005, "{path}: drifted {:.0} m off a {run_m:.0} m run, yaw rate {:.3}", at.x, v.kin.ang_vel.y);
        assert!(heel.abs() <= 3.0 && trim.abs() <= 2.0, "{path}: at speed heel {heel:.2} trim {trim:.2}");
        assert!(prop_depth(v, &tops) >= 0.25, "{path}: at speed a propeller tip is {:.2} m under", prop_depth(v, &tops));
        speeds.push((path, act, kn));

        // Two minutes of a Beaufort 7 sea on the beam: she rolls hard and comes back.
        let (worst, status) = beam_sea(&cat, &prefab, 120);
        assert!(status == VesselStatus::Active && worst < 50.0, "{path}: Beaufort 7 beam sea, worst heel {worst:.1} deg, {status:?}");

        // Light load, a tenth of the fuel and ammunition: still upright.
        let mut w = world(&cat);
        let id = spawn(&mut w, &prefab, 0.1);
        run(&mut w, 10);
        let v = w.vessel(id).unwrap();
        let (heel, trim) = degrees(v);
        assert!(v.status == VesselStatus::Active && heel.abs() <= 3.0 && trim.abs() <= 3.0, "{path}: light, {:?} at heel {heel:.2} trim {trim:.2}", v.status);
    }
    // The 22 m fast attack craft outruns the 15 m patrol boat it learns to beat (L261).
    let kn = |p: &str| speeds.iter().find(|s| s.0 == p).unwrap().2;
    assert!(kn("starters/fac22.bp.ron") > kn("starters/patrol15.bp.ron") + 2.0, "{speeds:?}");
}

/// The insertion craft answers every Act 1 stealth-insertion tag without a redesign (L151): its strongest
/// radar aspect is under the tightest cross-section cap an Act 1 operation sets, while the fast attack craft,
/// the first starter a player meets, is not.
#[test]
fn insertion_craft_fits_under_every_act1_cross_section_cap() {
    let cat = catalogue();
    let campaign = content::campaign::load::load_embedded(&cat).expect("campaign data");
    let cap = campaign
        .operations
        .values()
        .filter(|o| o.act == Act::Act1)
        .flat_map(|o| o.tags.iter())
        .filter_map(|t| match t {
            ConstraintTag::RcsLimit { max_m2 } => Some(*max_m2),
            _ => None,
        })
        .fold(f64::INFINITY, f64::min);
    assert!(cap.is_finite(), "Act 1 has stealth-insertion operations with a cross-section cap");
    let max_rcs = |path: &str| {
        let prefab = Arc::new(build_prefab(&stock(path), &cat));
        let mut w = world(&cat);
        let id = spawn(&mut w, &prefab, 1.0);
        run(&mut w, 5);
        let a = sim_core::vessel::projected_areas(w.vessel(id).unwrap(), &cat);
        a.rcs_m2.iter().fold(0.0f64, |m, &x| m.max(x as f64))
    };
    let ins = max_rcs("starters/insertion26.bp.ron");
    assert!(ins <= 0.8 * cap, "insertion craft: {ins:.0} m^2 against the {cap:.0} m^2 cap");
    let fac = max_rcs("starters/fac22.bp.ron");
    assert!(fac > ins * 1.5, "the fast attack craft ({fac:.0} m^2) is no stealth answer next to the insertion craft ({ins:.0} m^2)");
}

/// The tutorial's hulls as the editor shows them (L252-275): level 0 reads Fail on Float until the beam handle
/// is dragged, levels 1 to 4 read Pass, and the levels that can splash settle upright under way.
#[test]
fn tutorial_hulls_read_as_the_lesson_needs() {
    let cat = catalogue();
    for (i, path) in TUTORIAL.into_iter().enumerate() {
        let bp = stock(path);
        let prefab = Arc::new(build_prefab(&bp, &cat));
        let f = float(&prefab, Act::Act1, &cat);
        if i == 0 {
            assert!(f.check == Check::Fail && f.gm_m <= 0.0, "{path}: Float {:?}, GM {:.2} m must fail before the beam step", f.check, f.gm_m);
            continue;
        }
        assert_eq!(f.check, Check::Pass, "{path}: Float GM {:.1}% of beam", 100.0 * f.gm_over_beam);
        let mut w = world(&cat);
        let id = spawn(&mut w, &prefab, 1.0);
        run(&mut w, 10);
        let v = w.vessel(id).unwrap();
        let (heel, trim) = degrees(v);
        assert!(heel.abs() <= 1.0 && trim.abs() <= 4.5, "{path}: lies at heel {heel:.2} trim {trim:.2}");
        // Levels 2 to 4 have their engine and fuel; level 1 has no drive until the kits go in.
        if i >= 2 {
            w.set_controls(id, VesselControls { throttle: 1.0, ..Default::default() });
            run(&mut w, 30);
            let v = w.vessel(id).unwrap();
            let (heel, _) = degrees(v);
            assert!(v.status == VesselStatus::Active && v.forward_speed() * KN >= 20.0 && heel.abs() <= 3.0, "{path}: {:.1} kn at heel {heel:.2}", v.forward_speed() * KN);
        }
    }
}

/// Every number the checks above read, per hull, with the cell-exact (one-cell block) solve beside the
/// shipping 2 m blocks for comparison.
#[test]
#[ignore]
fn report() {
    let cat = catalogue();
    for path in STARTERS.iter().map(|s| s.0).chain(TUTORIAL) {
        let bp = stock(path);
        let prefab = Arc::new(build_prefab(&bp, &cat));
        let f = float(&prefab, Act::Act4, &cat);
        let s = speed_gauge(&prefab, &cat);
        let r = resilience_gauge(&prefab, &cat);
        eprintln!("== {path}\n   Float {:?}: {:.1} t, GM {:.2} m = {:.1}% of beam, heel {:.2} trim {:.2}, range {:.0} deg | Speed {:?} {:.1} kn, turn {:.0} m | Resilience {:?} {:.2}/{:.2}", f.check, f.displacement_t, f.gm_m, 100.0 * f.gm_over_beam, f.heel_deg, f.trim_deg, f.range_of_stability_deg, s.check, s.top_speed_kn, s.turning_circle_m, r.check, r.one_flooded_survival, r.two_flooded_survival);
        let gz: Vec<String> = f.gz.iter().step_by(5).map(|(a, g)| format!("{a:.0}:{g:.2}")).collect();
        eprintln!("   GZ {}", gz.join(" "));
        let mut w = world(&cat);
        let id = spawn(&mut w, &prefab, 1.0);
        let v = w.vessel(id).unwrap();
        let fine = sim_core::hydro::build_blocks(&prefab.grid, &prefab.compartments, 1);
        let e = sim_core::hydro::equilibrium(&fine, &v.mass, &[], &v.compartments, &cat.tuning.buoyancy);
        let gzf: Vec<String> = sim_core::hydro::gz_curve(&fine, &v.mass, 2.0, &cat.tuning.buoyancy).iter().step_by(5).map(|(a, g)| format!("{a:.0}:{g:.2}")).collect();
        eprintln!("   GZ one-cell {}", gzf.join(" "));
        eprintln!("   one-cell blocks: GM {:.2} m = {:.1}%, heel {:.2} trim {:.2}; waterline {:.2} m above the keel row", e.gm_m, 100.0 * e.gm_m / prefab.dims[2], e.heel_rad.to_degrees(), e.trim_rad.to_degrees(), prefab.waterline_y);
        let a = sim_core::vessel::projected_areas(v, &cat);
        eprintln!("   radar cross-section max {:.0} m^2, top {:.1} m", a.rcs_m2.iter().fold(0.0f32, |m, &x| m.max(x)), a.top_height_m);
        let tops = propeller_tops(&bp, &cat);
        run(&mut w, 10);
        let v = w.vessel(id).unwrap();
        let (heel, trim) = degrees(v);
        eprintln!("   at rest: {:?}, heel {heel:.2} trim {trim:.2}, propeller depth {:.2} m", v.status, prop_depth(v, &tops));
        if v.status != VesselStatus::Active {
            continue;
        }
        let (worst, status) = beam_sea(&cat, &prefab, 120);
        eprintln!("   Beaufort 7 beam sea 120 s: worst heel {worst:.1} deg, {status:?}");
        w.set_controls(id, VesselControls { throttle: 1.0, ..Default::default() });
        run(&mut w, 60);
        let v = w.vessel(id).unwrap();
        let (heel, trim) = degrees(v);
        eprintln!("   full ahead 60 s: {:.1} kn, heel {heel:.2} trim {trim:.2}, x drift {:.0} m, propeller depth {:.2} m", v.forward_speed() * KN, v.kin.pose.pos.x, prop_depth(v, &tops));
    }
}
