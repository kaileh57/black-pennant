//! Tests for the gauge cores on hand-built data (no physics stubs are reached).
use super::calc::*;
use super::testkit::*;
use super::*;
use crate::doctrine::MountInfo;
use content::cell::CellPos;
use content::graph::{MountLimits, WeaponClass};
use content::part::weapons::CalibreBand;
use content::part::{EngineDef, EngineKind, GunDef, PartCategory as C, PartFunction as F, Placement as P, PortRole, SensorDef, SensorKind};
use content::tuning::StabilityTuning;
use sim_core::hydro::Equilibrium;

fn eq(floats: bool, gm: f64, disp: f64) -> Equilibrium { Equilibrium { displacement_t: disp, draft_m: 1.5, heel_rad: 0.0, trim_rad: 0.0, reserve: 0.6, gm_m: gm, cb: DVec3::ZERO, floats } }

#[test]
fn range_of_stability_interpolates_the_zero_crossing() {
    let gz: Vec<(f64, f64)> = (0..=90).map(|i| { let h = i as f64 * 2.0; (h, libm::sin((2.0 * h).to_radians())) }).collect();
    let r = range_of_stability(&gz);
    assert!((r - 90.0).abs() < 1e-6, "{r}");
    assert_eq!(range_of_stability(&[(0.0, 0.0), (2.0, -0.1), (4.0, -0.2)]), 0.0, "never positive");
    assert_eq!(range_of_stability(&[(0.0, 0.0), (90.0, 0.5), (180.0, 0.1)]), 180.0, "never falls back");
    assert_eq!(range_of_stability(&[(0.0, 0.0), (10.0, 1.0), (20.0, -1.0)]), 15.0);
    assert_eq!(range_of_stability(&[]), 0.0);
}

#[test]
fn gm_states_follow_the_beam_thresholds() {
    let t = StabilityTuning::default();
    let beam = 10.0;
    assert_eq!(gm_check(0.0, beam, &t), Check::Fail);
    assert_eq!(gm_check(-0.2, beam, &t), Check::Fail);
    assert_eq!(gm_check(f64::NAN, beam, &t), Check::Fail);
    assert_eq!(gm_check(0.39, beam, &t), Check::Caution, "under 4% of beam");
    assert_eq!(gm_check(0.40, beam, &t), Check::Pass);
    assert_eq!(gm_check(1.0, beam, &t), Check::Pass);
    assert_eq!(gm_check(1.5, beam, &t), Check::Pass);
    assert_eq!(gm_check(1.51, beam, &t), Check::Caution, "over 15% of beam");
}

#[test]
fn float_gauge_reports_cap_and_sinking() {
    let t = StabilityTuning::default();
    let g = float_from(&eq(true, 0.8, 120.0), vec![(0.0, 0.0), (30.0, 0.3), (70.0, -0.1)], 10.0, 150.0, &t);
    assert_eq!(g.check, Check::Pass);
    assert!((g.gm_over_beam - 0.08).abs() < 1e-12);
    assert!((g.range_of_stability_deg - 60.0).abs() < 1e-9, "{}", g.range_of_stability_deg);
    assert_eq!(float_from(&eq(true, 0.8, 160.0), vec![], 10.0, 150.0, &t).check, Check::Caution, "over the tonnage cap");
    assert_eq!(float_from(&eq(true, 0.8, 160.0), vec![], 10.0, 0.0, &t).check, Check::Pass, "no cap");
    let sunk = float_from(&eq(false, 0.8, 120.0), vec![(0.0, 0.0), (10.0, f64::NAN)], 10.0, 150.0, &t);
    assert_eq!(sunk.check, Check::Fail);
    assert_eq!(sunk.range_of_stability_deg, 0.0);
    assert_eq!(sunk.gz.len(), 1, "non-finite samples dropped");
}

#[test]
fn speed_checks() {
    let g = speed_from(10.0 * sim_core::time::KN, 100.0, 40.0);
    assert!((g.top_speed_kn - 10.0).abs() < 1e-9);
    assert_eq!(g.turning_circle_m, 200.0);
    assert_eq!(g.check, Check::Pass);
    assert_eq!(speed_from(0.0, 100.0, 40.0).check, Check::Fail);
    assert_eq!(speed_from(5.0, f64::INFINITY, 40.0).check, Check::Caution, "cannot turn");
    assert_eq!(speed_from(5.0, f64::INFINITY, 40.0).turning_circle_m, 0.0);
    assert_eq!(speed_from(5.0, 200.0, 40.0).check, Check::Caution, "400 m circle is over 8 lengths");
}

#[test]
fn power_and_endurance_accounting() {
    let (bp, cat) = fitted();
    let pf = prefab(&bp, &cat);
    let b = power_budget(&pf, &cat, 0.7);
    assert_eq!(b.generation_kw, 100.0);
    assert_eq!(b.combat_kw, 50.0);
    assert_eq!(b.cruise_kw, 20.0, "the gun idles at cruise, the radar runs");
    assert_eq!(b.consumers.len(), 2);
    assert_eq!(b.gauge().check, Check::Pass);
    let fuel = fuel_account(&pf, &cat);
    assert_eq!((fuel.fuel_l, fuel.capacity_l), (1_000.0, 2_000.0));
    let e = endurance_from(10.0, &fuel, &b, 0.7);
    let burn = 0.06 * 0.7 * 0.7 * 0.7 + 0.01 * 0.2;
    let want = 7.0 * 1_000.0 / burn / 1_000.0;
    assert!((e.range_km - want).abs() < 1e-6, "{} vs {want}", e.range_km);
    assert_eq!(e.check, Check::Pass);
    assert_eq!(endurance_from(0.0, &fuel, &b, 0.7).range_km, 0.0);
    assert_eq!(endurance_from(0.0, &fuel, &b, 0.7).check, Check::Caution);
    let short = PowerBudget { generation_kw: 40.0, combat_kw: 50.0, ..b.clone() };
    assert_eq!(short.gauge().check, Check::Caution);
    let none = PowerBudget { generation_kw: 0.0, ..b };
    assert_eq!(none.gauge().check, Check::Fail);
    assert!((cruise_fraction() - 0.7).abs() < 1e-12, "the Helm's default cruise speed");
}

#[test]
fn punch_counts_ready_rounds_and_hoist_feed() {
    let (bp, cat) = fitted();
    let pf = prefab(&bp, &cat);
    let gun = cat.part_idx(&PartId::new("gun")).unwrap();
    let centre = pf.parts[3].centre;
    let mount = |slot: u16, weapon: WeaponClass, pos_local: DVec3| MountInfo { slot: PartSlot(slot), part: gun, weapon, munition: None, pos_local, zero_rad: 0.0, limits: MountLimits { train_min_deg: -180.0, train_max_deg: 180.0, elevation_min_deg: -10.0, elevation_max_deg: 85.0 }, effective_range_m: 8_000.0, director: None, kit: None, group: 0, muzzle_mps: 900.0, point_defence: false };
    let g = punch_from(&[mount(3, WeaponClass::Gun, centre), mount(9, WeaponClass::Decoy, centre)], &pf, &cat);
    // 2 barrels x 60 rpm = 120 rpm, fed by 10 ready + 40 from the hoist below: 50 rounds of 6 kg.
    assert_eq!(g.weight_of_fire_kg_per_min, 300.0);
    assert_eq!(g.effective_range_m, vec![(PartSlot(3), 8_000.0)]);
    assert_eq!(g.check, Check::Pass);
    let far = punch_from(&[mount(3, WeaponClass::Gun, centre + DVec3::new(10.0, 0.0, 0.0))], &pf, &cat);
    assert_eq!(far.weight_of_fire_kg_per_min, 60.0, "no hoist in reach: ready rounds only");
    assert_eq!(punch_from(&[], &pf, &cat).check, Check::Caution);
}

#[test]
fn arc_sweep_bearings_and_fractions() {
    let full = MountLimits { train_min_deg: -180.0, train_max_deg: 180.0, elevation_min_deg: -5.0, elevation_max_deg: 80.0 };
    // Structure dead ahead: training within 20 degrees of the bow is masked.
    let mut clear = |t: f64, e: f64| { assert_eq!(e, 0.0); libm::round(t.to_degrees()).abs() > 20.0 };
    let s = sweep_arcs(&full, true, 0.0, 8_000.0, &mut clear);
    assert!((s.blocked_fraction - 41.0 / 360.0).abs() < 1e-12, "{}", s.blocked_fraction);
    let blocked: Vec<&ArcSector> = s.sectors.iter().filter(|a| a.blocked).collect();
    assert_eq!(blocked.len(), 1);
    assert!((blocked[0].from_deg - 339.5).abs() < 1e-9 && (blocked[0].to_deg - 20.5).abs() < 1e-9, "{:?}", blocked[0]);
    assert_eq!(s.widest_blocked, Some((blocked[0].from_deg, blocked[0].to_deg)));
    let mid = s.widest_mid_deg.unwrap();
    assert!(mid < 1.0 || mid > 359.0, "{mid}");
    assert!(s.sectors.iter().all(|a| a.range_m == 8_000.0));
    // A limited port-side arc (training is positive to port), masked between 40 and 60 degrees of training.
    let port = MountLimits { train_min_deg: 0.0, train_max_deg: 180.0, elevation_min_deg: 5.0, elevation_max_deg: 80.0 };
    let mut clear = |t: f64, e: f64| { assert!((e - 5f64.to_radians()).abs() < 1e-12); !(40.0..=60.0).contains(&libm::round(t.to_degrees())) };
    let s = sweep_arcs(&port, false, 0.0, 1.0, &mut clear);
    assert!((s.blocked_fraction - 21.0 / 181.0).abs() < 1e-12);
    let b = s.sectors.iter().find(|a| a.blocked).unwrap();
    assert!((b.from_deg - 299.5).abs() < 1e-9 && (b.to_deg - 320.5).abs() < 1e-9, "{b:?}");
    // Nothing masked: one clear ring.
    let s = sweep_arcs(&full, true, 1.0, 1.0, &mut |_, _| true);
    assert_eq!(s.sectors, vec![ArcSector { from_deg: 0.0, to_deg: 360.0, blocked: false, range_m: 1.0 }]);
    assert_eq!(s.widest_blocked, None);
    // Bad limits read as all round.
    let bad = MountLimits { train_min_deg: f64::NAN, train_max_deg: 0.0, elevation_min_deg: f64::NAN, elevation_max_deg: 0.0 };
    assert_eq!(sweep_arcs(&bad, false, 0.0, 1.0, &mut |_, _| false).blocked_fraction, 1.0);
    // Auto-set limits leave out the masked bow (training 20 to 340 the long way round); an all-round sweep of a
    // trainable mount still reports the mask, and draws nothing where the limits stop a clear path.
    let auto = MountLimits { train_min_deg: 20.0, train_max_deg: 340.0, elevation_min_deg: 0.0, elevation_max_deg: 80.0 };
    let mut clear = |t: f64, _e: f64| libm::round(t.to_degrees()).abs() > 20.0;
    let s = sweep_arcs(&auto, true, 0.0, 1.0, &mut clear);
    assert!((s.blocked_fraction - 41.0 / 360.0).abs() < 1e-12);
    assert!(s.sectors.iter().any(|a| a.blocked && (a.from_deg - 339.5).abs() < 1e-9 && (a.to_deg - 20.5).abs() < 1e-9), "{:?}", s.sectors);
    // Hand-set limits narrower than the structure allows: the clear bearings outside them are not drawn.
    let narrow = MountLimits { train_min_deg: -90.0, train_max_deg: -30.0, elevation_min_deg: 0.0, elevation_max_deg: 80.0 };
    let s = sweep_arcs(&narrow, true, 0.0, 1.0, &mut clear);
    let clear_arcs: Vec<&ArcSector> = s.sectors.iter().filter(|a| !a.blocked).collect();
    assert_eq!(clear_arcs.len(), 1);
    assert!((clear_arcs[0].from_deg - 29.5).abs() < 1e-9 && (clear_arcs[0].to_deg - 90.5).abs() < 1e-9, "{clear_arcs:?}");
}

#[test]
fn sectors_merge_across_the_bow() {
    let mut b = vec![false; 36];
    b[35] = true;
    b[0] = true;
    b[1] = true;
    let arcs = merge_sectors(&b, 5.0);
    assert_eq!(arcs.len(), 2);
    let blocked = arcs.iter().find(|a| a.blocked).unwrap();
    assert_eq!((blocked.from_deg, blocked.to_deg), (350.0, 20.0));
    let clear = arcs.iter().find(|a| !a.blocked).unwrap();
    assert_eq!((clear.from_deg, clear.to_deg), (20.0, 350.0));
    assert_eq!(merge_sectors(&[true; 36], 1.0).len(), 1);
}

#[test]
fn radar_coverage_sees_the_funnel_astern() {
    let (mut bp, cat) = fitted();
    // A tall funnel just aft of the radar (lower x is aft).
    place(&mut bp, "funnel", CellPos::new(16, 8, 5));
    let pf = prefab(&bp, &cat);
    let site = sensor_sites(&pf, &cat).into_iter().find(|s| s.kind == SensorKind::SearchRadar).unwrap();
    assert_eq!(site.height_m, 8.5 * 0.5 - 1.5);
    let cov = sector_coverage(&pf.grid, &site);
    assert_eq!(cov.len(), 36);
    assert!(cov[0] < 1.0, "the gun ahead masks part of the bow sector");
    assert_eq!(cov[4], 1.0, "clear on the starboard bow");
    assert_eq!(cov[18], 0.0, "funnel astern");
    assert_eq!(cov[9], 1.0, "clear to starboard");
    let blind = blind_sectors_of(&pf, PartSlot(2), &cat);
    assert!(blind.iter().any(|a| a.blocked && a.from_deg < 180.0 && a.to_deg > 180.0), "{blind:?}");
    assert!(blind.iter().all(|a| a.range_m > 0.0));
    let sight = sight_from(&pf, &cat);
    assert_eq!(sight.coverage, cov);
    assert!(sight.radar_km[0] > 0.0 && sight.radar_km[1] > sight.radar_km[0], "{:?}", sight.radar_km);
    assert!((sight.horizon_km[0] - 4.12 * (site.height_m.sqrt() + 5f64.sqrt())).abs() < 1e-9);
    assert_eq!(sight.antenna_m, site.height_m);
    assert_eq!(sight.visual_km, [0.0; 3], "no crew, no lookout");
    assert_eq!(blind_sectors_of(&pf, PartSlot(0), &cat), Vec::new(), "a generator is not a sensor");
}

#[test]
fn sight_numbers_follow_the_laws() {
    let t = content::tuning::SensorTuning::default();
    let r = SensorSite { slot: PartSlot(0), kind: SensorKind::SearchRadar, r_ref_m: 40_000.0, centre: DVec3::ZERO, height_m: 16.0, fov_half_deg: 180.0, boresight_deg: 0.0 };
    let (h, radar, visual, antenna) = sight_numbers(&[r], Some(9.0), 0.5, 30_000.0, &t);
    assert_eq!(antenna, 16.0);
    assert!((h[2] - 4.12 * (4.0 + 1_000f64.sqrt())).abs() < 1e-9);
    // Sea-skimmer at 5 m: full clutter, 40 km x (0.1 / 100 / 1.5)^(1/4), under the horizon.
    let want = 40.0 * libm::pow(0.1 / 100.0 / 1.5, 0.25);
    assert!((radar[0] - want.min(h[0])).abs() < 1e-9, "{} vs {want}", radar[0]);
    assert!(visual[1] > 0.0 && visual[1] <= 30.0);
    assert_eq!(sight_check(0, &[1.0; 36]), Check::Caution);
    let mut c = vec![1.0f32; 36];
    c[0] = 0.0;
    c[1] = 0.0;
    c[2] = 0.0;
    assert_eq!(sight_check(1, &c), Check::Pass);
    c[3] = 0.2;
    assert_eq!(sight_check(1, &c), Check::Caution);
}

#[test]
fn flood_cases_and_shares() {
    let cat = power_catalogue();
    let mut bp = box_blueprint(40, 8, 12, 3);
    // A transverse bulkhead at x = 20 makes two compartments.
    let mut g = content::cell::CellGrid::from_rows(&bp.cells);
    for y in 0..8 { for z in 0..12 { g.set(CellPos::new(20, y, z), content::cell::Cell { material: content::cell::Material::Steel, armour: Default::default(), paint: 2, shape: content::cell::CellShape::CUBE }); } }
    bp.cells = g.to_rows();
    let pf = prefab(&bp, &cat);
    let (singles, pairs) = flood_cases(&pf.compartments);
    assert_eq!(singles.len(), 2);
    assert_eq!(pairs.len(), 1);
    let case = |a: u16, b: Option<u16>, heel: f64, survives: bool| FloodCase { a: CompartmentId(a), b: b.map(CompartmentId), heel_deg: heel, trim_deg: 0.0, survives };
    let c = flood_case(CompartmentId(0), None, &Equilibrium { heel_rad: 35f64.to_radians(), ..eq(true, 0.5, 100.0) });
    assert!(!c.survives, "heeled past 30 degrees");
    assert!(flood_case(CompartmentId(0), None, &eq(true, 0.5, 100.0)).survives);
    assert!(!flood_case(CompartmentId(0), None, &eq(false, 0.5, 100.0)).survives);
    let r = resilience_from(vec![case(0, None, 5.0, true), case(1, None, 40.0, false)], vec![case(0, Some(1), 50.0, false)], 0.8);
    assert_eq!(r.one_flooded_survival, 0.5);
    assert_eq!(r.two_flooded_survival, 0.0);
    assert_eq!(r.check, Check::Caution);
    assert_eq!(r.worst[0].b, Some(CompartmentId(1)), "worst first: lost, then largest heel");
    assert_eq!(r.worst.len(), 3);
    let ok = resilience_from(vec![case(0, None, 5.0, true)], vec![case(0, Some(1), 10.0, true)], 2.0);
    assert_eq!((ok.check, ok.systems_surviving_hit), (Check::Pass, 1.0));
}

#[test]
fn hits_and_heatmap_are_deterministic() {
    let (bp, cat) = fitted();
    let pf = prefab(&bp, &cat);
    let hits = HitModel::new(&pf, &cat);
    assert!(!hits.outer().is_empty());
    let a = hits.systems_surviving(7, 64);
    assert_eq!(a, hits.systems_surviving(7, 64));
    assert!((0.0..=1.0).contains(&a));
    let h1 = hits.heatmap(11);
    assert_eq!(h1, hits.heatmap(11));
    assert!(!h1.is_empty());
    assert!(h1.windows(2).all(|w| w[0].0 < w[1].0), "ascending cells");
    assert!(h1.iter().any(|(_, v)| *v > 0), "some outer cells cover systems");
    // A shell plunging through the gun and the deck bursts at the third solid cell, the hoist, and takes
    // both out (the gun is inside the 2-cell HE radius).
    let lost = hits.hit(CellPos::new(30, 8, 6), -DVec3::Y);
    assert!(lost.contains(&PartSlot(5)) && lost.contains(&PartSlot(3)), "{lost:?}");
    // Through the thin side and out of the far side there are too few solid cells to arm the fuse.
    assert!(hits.hit(CellPos::new(2, 4, 0), DVec3::Z).is_empty());
    // An empty design has no systems to lose.
    let empty = box_blueprint(10, 4, 6, 2);
    let pe = prefab(&empty, &cat);
    assert_eq!(HitModel::new(&pe, &cat).systems_surviving(1, 32), 1.0);
}

#[test]
fn complexity_marks_caution_and_cap() {
    let g = complexity_from(450.0, 400.0, 600.0);
    assert_eq!((g.check, g.pvp_cap, g.caution_at), (Check::Caution, 600.0, 400.0));
    assert_eq!(complexity_from(400.0, 400.0, 600.0).check, Check::Pass);
    assert_eq!(complexity_from(f64::NAN, f64::NAN, 600.0), ComplexityGauge { points: 0.0, caution_at: 400.0, pvp_cap: 600.0, check: Check::Pass });
    let cat = Catalogue::default();
    let g = complexity_gauge(10.0, &cat);
    assert_eq!((g.caution_at, g.pvp_cap), (400.0, 600.0));
}

#[test]
fn facing_and_bearings() {
    assert_eq!(wrap360(-20.0), 340.0);
    assert_eq!(wrap360(720.0), 0.0);
    assert_eq!(wrap360(f64::NAN), 0.0);
    assert_eq!(facing_deg(content::kit::PartRot::default()), 0.0);
    assert_eq!(train_to_bearing(0.0, 90.0), 270.0, "training to port is bearing 270");
    assert!(bearing_dir(90.0).z > 0.99, "bearing 90 is starboard (+Z)");
}

#[test]
fn wrappers_run_on_the_real_hydrostatics() {
    let (bp, cat) = fitted();
    let mut pf = prefab(&bp, &cat);
    pf.blocks = sim_core::hydro::build_blocks(&pf.grid, &pf.compartments, cat.tuning.buoyancy.block_cells);
    pf.dry_mass = sim_core::mass::dry_mass(&pf.grid, &pf.parts, &cat);
    let f = float_gauge(&pf, 150.0, 2.0, &cat);
    assert!(f.floats, "{f:?}");
    assert!(f.displacement_t > 0.0 && f.gm_m.is_finite());
    assert!(!f.gz.is_empty() && f.gz.iter().all(|(h, g)| h.is_finite() && g.is_finite()));
    let coarse = float_gauge(&pf, 150.0, 10.0, &cat);
    assert!(coarse.gz.len() < f.gz.len(), "10 degree steps while dragging");
    let r = resilience_gauge(&pf, &cat);
    assert!((0.0..=1.0).contains(&r.one_flooded_survival) && (0.0..=1.0).contains(&r.two_flooded_survival));
    assert!((0.0..=1.0).contains(&r.systems_surviving_hit));
    let s = speed_gauge(&pf, &cat);
    assert!(s.top_speed_kn.is_finite() && s.turning_circle_m.is_finite());
    let e = endurance_gauge(&pf, &cat);
    assert!(e.range_km.is_finite() && e.range_km >= 0.0);
}
