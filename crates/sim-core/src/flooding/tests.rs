use super::*;
use crate::compartments::testkit::box_hull;
use crate::compartments::OpeningKind;
use crate::math::{DQuat, DVec3, Pose};
use crate::vessel::testkit::{blueprint, place, vessel};
use crate::vessel::{build_prefab, VesselPrefab};
use content::blueprint::Blueprint;
use content::cell::{CellPos, CELL_VOLUME_M3};
use content::ids::CellIndex;

fn cat() -> &'static Catalogue { content::catalogue::catalogue() }

/// 22 m x 5 m x 6 m steel box with bulkheads at x = 11, 22, 33, a deck at y = 5 and a 40 mm bottom. Lower bays
/// are compartments 0..=3 (25 m^2 of floor each, floor at 0.5 m), upper bays 4..=7.
fn box_bp() -> Blueprint {
    let mut g = box_hull([44, 10, 12], &[11, 22, 33], &[5]);
    for c in g.cells.iter_mut().take(44 * 12) { c.armour = content::cell::ArmourTier::Mm40; }
    blueprint(&g, 2)
}

fn at(y: f64) -> Pose { Pose { pos: DVec3::new(0.0, y, 0.0), rot: DQuat::IDENTITY } }

fn tick(v: &mut Vessel, tuning: &FloodingTuning) -> FloodOutcome {
    let calm = |_: f64, _: f64| 0.0;
    let env = FloodEnv { sea: &calm, band: f64::INFINITY, catalogue: cat(), tuning, dt: crate::time::DT };
    let io = FloodIo { comps: &mut v.compartments, parts: &mut v.parts, size: v.grid.size(), controls: &v.controls, pose: v.kin.pose };
    flood_tick(io, &env)
}

fn idx(v: &Vessel, x: i16, y: i16, z: i16) -> CellIndex { v.grid.index(CellPos::new(x, y, z)).unwrap() }

/// Shoot away one cell and let the compartments unit turn it into openings.
fn destroy(v: &mut Vessel, c: CellIndex) {
    v.grid.add_damage(c, 8);
    let (_, more) = v.compartments.update_after_damage(&mut v.grid, &[c], 1_000_000);
    assert!(!more);
}

fn prefab() -> VesselPrefab { build_prefab(&box_bp(), cat()) }

#[test]
fn orifice_matches_the_spec_numbers() {
    let p = prefab();
    for (depth, q) in [(2.0, 0.94), (1.0, 0.66)] {
        // Side shell cell (16, 1, 0): its breach face looks into bay 2 at local height 0.75 m.
        let mut v = vessel(&p, at(-0.75 - depth));
        let c = idx(&v, 16, 1, 0);
        destroy(&mut v, c);
        let breaches: Vec<_> = v.compartments.openings.iter().filter(|o| o.kind == OpeningKind::Breach).collect();
        assert_eq!(breaches.len(), 1);
        assert_eq!(breaches[0].a, CompartmentId(1));
        let out = tick(&mut v, &FloodingTuning::default());
        let exact = 0.6 * 0.25 * (2.0 * 9.81 * depth).sqrt();
        assert!((out.inflow_m3 / crate::time::DT - exact).abs() < 1e-9, "{} vs {exact}", out.inflow_m3 / crate::time::DT);
        assert!((exact - q).abs() < 0.005);
        assert!((v.compartments.compartments[1].water_m3 - out.inflow_m3).abs() < 1e-12);
    }
}

#[test]
fn first_water_in_a_dry_bay_spreads_over_its_floor() {
    let p = prefab();
    let mut v = vessel(&p, at(-1.75));
    for x in [13, 15, 17, 19] { let c = idx(&v, x, 1, 0); destroy(&mut v, c); }
    let out = tick(&mut v, &FloodingTuning::default());
    // Four faces 1 m down into a dry bay with 25 m^2 of floor: the running level rises by millimetres between
    // them, so each takes almost the full first-tick flow.
    let single = 0.6 * 0.25 * (2.0 * 9.81 * 1.0f64).sqrt() * crate::time::DT;
    assert!(out.inflow_m3 > 4.0 * single * 0.999 && out.inflow_m3 <= 4.0 * single + 1e-12, "{} vs {}", out.inflow_m3, 4.0 * single);
    assert!((v.compartments.compartments[1].free_surface_m2 - 25.0).abs() < 1e-9);
}

#[test]
fn breach_fills_to_the_outside_level_without_overshoot() {
    let p = prefab();
    let mut v = vessel(&p, at(-1.75));
    for z in [0] { for x in [14, 16, 18] { let c = idx(&v, x, 1, z); destroy(&mut v, c); } }
    let t = FloodingTuning { flood_rate_scale: 20.0, ..Default::default() };
    let mut total = 0.0;
    for _ in 0..3_000 {
        let out = tick(&mut v, &t);
        total += out.inflow_m3;
        let c = &v.compartments.compartments[1];
        assert!(c.water_plane_y <= 1e-4 && c.water_m3 <= 31.25 + 2e-3, "overshoot to {}", c.water_plane_y);
    }
    let c = &v.compartments.compartments[1];
    // Floor at world -1.25 m, sea at 0: 25 m^2 x 1.25 m.
    assert!(c.water_plane_y.abs() < 1e-3, "plane {}", c.water_plane_y);
    assert!((c.water_m3 - 31.25).abs() < 0.05, "water {}", c.water_m3);
    assert!((total - c.water_m3).abs() < 1e-9);
    assert!((c.free_surface_m2 - 25.0).abs() < 1e-9);
    // Raised above the sea, the same compartment drains back out through the breach.
    v.kin.pose.pos.y = 0.0;
    for _ in 0..3_000 { tick(&mut v, &t); }
    let c = &v.compartments.compartments[1];
    // The breach face centre is at 0.75 m: water below it stays.
    assert!(c.water_plane_y > 0.75 - 1e-3 && c.water_plane_y < 0.76, "plane {}", c.water_plane_y);
}

#[test]
fn open_door_levels_two_compartments_and_conserves_water() {
    let mut bp = box_bp();
    place(&mut bp, "door.watertight", [11, 1, 5]);
    let p = build_prefab(&bp, cat());
    let mut v = vessel(&p, at(-0.87));
    let door = v.compartments.openings.iter().find(|o| matches!(o.kind, OpeningKind::Door(_))).copied().expect("door opening");
    assert_eq!((door.a, door.b), (CompartmentId(0), Some(CompartmentId(1))));
    v.parts[0].state = PartState::Door { closed: true };
    v.compartments.compartments[0].water_m3 = 20.0;
    let t = FloodingTuning::default();
    for _ in 0..10 { tick(&mut v, &t); }
    assert_eq!(v.compartments.compartments[1].water_m3, 0.0);
    v.parts[0].state = PartState::Door { closed: false };
    let mut last_gap = f64::INFINITY;
    for _ in 0..3_000 {
        tick(&mut v, &t);
        let (a, b) = (&v.compartments.compartments[0], &v.compartments.compartments[1]);
        assert!((a.water_m3 + b.water_m3 - 20.0).abs() < 1e-9);
        let gap = a.water_plane_y - b.water_plane_y;
        assert!(gap >= -1e-9 && gap <= last_gap + 1e-9, "levels oscillate: {gap}");
        last_gap = gap;
    }
    assert!(last_gap < 1e-3);
    assert!((v.compartments.compartments[1].water_m3 - 10.0).abs() < 0.05);
}

#[test]
fn flood_valves_follow_the_controls() {
    let p = prefab();
    let mut v = vessel(&p, at(-p.waterline_y));
    let t = FloodingTuning::default();
    let out = tick(&mut v, &t);
    assert_eq!(out.inflow_m3, 0.0);
    v.controls.open_valves = vec![CompartmentId(2), CompartmentId(6)];
    let out = tick(&mut v, &t);
    assert!(v.compartments.compartments[2].flood_valve_open);
    // Compartment 6 is above the waterline and has no valve.
    assert!(!v.compartments.compartments[6].flood_valve_open);
    let head = p.waterline_y - 0.5;
    let exact = 0.6 * 0.1 * (2.0 * 9.81 * head).sqrt() * crate::time::DT;
    assert!((out.inflow_m3 - exact).abs() < 1e-12);
    v.controls.open_valves.clear();
    let out = tick(&mut v, &t);
    assert_eq!(out.inflow_m3, 0.0);
    assert!(!v.compartments.compartments[2].flood_valve_open);
    assert!(v.compartments.openings.iter().filter(|o| o.kind == OpeningKind::FloodValve).all(|o| !o.open));
}

#[test]
fn pumps_drain_their_target_or_their_own_compartment() {
    let mut bp = box_bp();
    place(&mut bp, "pump.small", [5, 1, 5]);
    let p = build_prefab(&bp, cat());
    let mut v = vessel(&p, at(-0.87));
    assert_eq!(v.parts[0].compartment, Some(CompartmentId(0)));
    v.parts[0].links.power_supplied = 1.0;
    v.parts[0].state = PartState::Pump { running: true, target: Some(CompartmentId(1)) };
    v.compartments.compartments[0].water_m3 = 5.0;
    let t = FloodingTuning::default();
    // The target is dry, so the pump drains its own compartment.
    let out = tick(&mut v, &t);
    assert!((v.compartments.compartments[0].water_m3 - (5.0 - 0.05 / 60.0)).abs() < 1e-12);
    assert!((out.inflow_m3 + 0.05 / 60.0).abs() < 1e-12);
    v.compartments.compartments[1].water_m3 = 1.0;
    tick(&mut v, &t);
    assert!((v.compartments.compartments[1].water_m3 - (1.0 - 0.05 / 60.0)).abs() < 1e-12);
    // Half power, half rate; stopped pumps move nothing.
    v.parts[0].links.power_supplied = 0.5;
    tick(&mut v, &t);
    assert!((v.compartments.compartments[1].water_m3 - (1.0 - 0.075 / 60.0)).abs() < 1e-12);
    v.parts[0].state = PartState::Pump { running: false, target: None };
    let before = v.compartments.compartments[0].water_m3;
    tick(&mut v, &t);
    assert_eq!(v.compartments.compartments[0].water_m3, before);
}

#[test]
fn magazine_floods_in_ten_seconds() {
    let mut bp = box_bp();
    place(&mut bp, "magazine.57mm", [3, 1, 3]);
    let p = build_prefab(&bp, cat());
    let mut v = vessel(&p, at(-0.87));
    v.controls.flood_magazines = vec![PartSlot(0), PartSlot(0), PartSlot(7)];
    let t = FloodingTuning::default();
    for _ in 0..599 { assert!(tick(&mut v, &t).magazines_flooded.is_empty()); }
    assert!(matches!(v.parts[0].state, PartState::Magazine { flooded: false, .. }));
    assert_eq!(tick(&mut v, &t).magazines_flooded, vec![PartSlot(0)]);
    assert!(matches!(v.parts[0].state, PartState::Magazine { flooded: true, rounds: 0, .. }));
    assert!(tick(&mut v, &t).magazines_flooded.is_empty());
}

#[test]
fn levelling_conserves_volume_and_shifts_water_to_the_low_side() {
    let p = prefab();
    let mut v = vessel(&p, at(-0.87));
    v.compartments.compartments[0].water_m3 = 10.0;
    level_water(&mut v, CompartmentId(0), 16);
    let c = &v.compartments.compartments[0];
    assert!((c.water_plane_y - (-0.87 + 0.5 + 0.4)).abs() < 1e-3);
    assert!((c.water_centroid.y - 0.7).abs() < 1e-3);
    let level_z = c.water_centroid.z;
    // Heel 20 degrees to starboard.
    let h = 20f64.to_radians();
    v.kin.pose.rot = DQuat::from_xyzw(libm::sin(0.5 * h), 0.0, 0.0, libm::cos(0.5 * h));
    level_water(&mut v, CompartmentId(0), 16);
    let c = v.compartments.compartments[0].clone();
    let fr = Frame::of(&v.kin.pose);
    let d = Dims::new(v.grid.size());
    let local = c.water_plane_y - fr.base_y;
    let vol: f64 = c.cells.iter().map(|k| fill(local, fr.up.dot(d.centre(k.index())), fr.h)).sum::<f64>() * CELL_VOLUME_M3;
    assert!((vol - 10.0).abs() < 0.01, "volume {vol}");
    assert!(c.water_centroid.z > level_z + 0.3, "water should run to starboard");
    // A wedge against the starboard side: 2 m^2 of section over 5 m gives a 3.3 m wetted floor, and the surface
    // is 3.3 / cos 20 = 3.53 m wide over the 5 m length.
    let w = (4.0 / libm::tan(h)).sqrt();
    let want = w / libm::cos(h) * 5.0;
    assert!((c.free_surface_m2 - want).abs() < 0.1 * want, "free surface {} vs {want}", c.free_surface_m2);
    // Bad input is clamped, never a panic.
    v.compartments.compartments[0].water_m3 = f64::NAN;
    level_water(&mut v, CompartmentId(0), 0);
    assert_eq!(v.compartments.compartments[0].water_m3, 0.0);
    level_water(&mut v, CompartmentId(99), 16);
}

#[test]
fn crossing_the_half_line_is_reported_once() {
    let p = prefab();
    let mut v = vessel(&p, at(-3.0));
    let c = idx(&v, 5, 1, 0);
    destroy(&mut v, c);
    let t = FloodingTuning { flood_rate_scale: 20.0, ..Default::default() };
    let mut crossed = Vec::new();
    for _ in 0..2_000 { crossed.extend(tick(&mut v, &t).crossed_half); }
    assert_eq!(crossed, vec![CompartmentId(0)]);
    assert!(v.compartments.compartments[0].fill_fraction() > 0.5);
}

#[test]
fn gauge_volumes_match_where_the_simulation_settles() {
    let p = prefab();
    let open = [CompartmentId(1)];
    let vols = flooded_volumes(&p, &[CompartmentId(1), CompartmentId(1), CompartmentId(77)]);
    assert_eq!(vols.len(), 1);
    let (id, want) = vols[0];
    assert_eq!(id, CompartmentId(1));
    assert!(want > 1.0 && want < p.compartments.compartments[1].volume_m3);
    // Put the hull at the gauge's equilibrium and flood the bay through its valve.
    let eq = crate::hydro::equilibrium(&p.blocks, &p.dry_mass, &open, &p.compartments, &content::tuning::BuoyancyTuning::default());
    assert!(eq.floats);
    let (u, c) = equilibrium_plane(&p.blocks, &eq).unwrap();
    let (sh, ch) = libm::sincos(0.5 * eq.heel_rad);
    let (st, ct) = libm::sincos(0.5 * eq.trim_rad);
    let rot = (DQuat::from_xyzw(0.0, 0.0, st, ct) * DQuat::from_xyzw(sh, 0.0, 0.0, ch)).normalize();
    let mut v = vessel(&p, Pose { pos: DVec3::new(0.0, -c, 0.0), rot });
    assert!((Frame::of(&v.kin.pose).up - u).length() < 1e-9);
    let (h, t) = v.heel_trim();
    assert!((h - eq.heel_rad).abs() < 1e-9 && (t - eq.trim_rad).abs() < 1e-9);
    v.controls.open_valves = vec![CompartmentId(1)];
    let tun = FloodingTuning { flood_rate_scale: 50.0, ..Default::default() };
    for _ in 0..3_000 { tick(&mut v, &tun); }
    let got = v.compartments.compartments[1].water_m3;
    assert!((got - want).abs() < 0.01 * want, "{got} vs {want}");
}

#[test]
fn flooding_is_deterministic() {
    let run = || {
        let p = prefab();
        let mut v = vessel(&p, at(-2.0));
        for x in [3, 15, 27] { let c = idx(&v, x, 1, 11); destroy(&mut v, c); }
        let t = FloodingTuning::default();
        let mut log = Vec::new();
        for k in 0..300 {
            let h = (k as f64 * 0.01).to_radians();
            v.kin.pose.rot = DQuat::from_xyzw(libm::sin(h), 0.0, 0.0, libm::cos(h));
            log.push(tick(&mut v, &t).inflow_m3.to_bits());
        }
        (log, v.compartments)
    };
    assert_eq!(run(), run());
}

#[test]
fn run_sums_match_cell_sums() {
    for &dk in &[0.0, 1e-14, 0.1, -0.1, 0.37, -0.49] {
        for len in [1u32, 2, 7, 40] {
            let r = CellRun { p0: DVec3::ZERO, k0: 1.3, len };
            for step in 0..60 {
                let c = -2.0 + step as f64 * 0.17;
                let h = 0.61;
                let brute: f64 = (0..len).map(|j| fill(c, 1.3 + dk * j as f64, h)).sum();
                let fast = run_fill(&r, dk, c, h);
                assert!((brute - fast).abs() < 1e-9, "dk {dk} len {len} c {c}: {brute} vs {fast}");
            }
        }
    }
}

#[test]
fn gauge_mass_carries_the_loads_the_build_floats_on() {
    let mut bp = box_bp();
    place(&mut bp, "tank.fuel.small", [3, 1, 3]);
    place(&mut bp, "ballast.tank", [25, 1, 6]);
    if let Some(b) = bp.parts.last_mut() { b.load = Some(2_000.0); }
    place(&mut bp, "magazine.57mm", [16, 1, 3]);
    let p = build_prefab(&bp, cat());
    let loads = p.parts.iter().filter(|q| matches!(q.state, PartState::FuelTank { litres } if litres > 0.0) || matches!(q.state, PartState::Ballast { kg } if kg > 0.0) || matches!(q.state, PartState::Magazine { ammo_kg, .. } if ammo_kg > 0.0)).count();
    assert_eq!(loads, 3, "fixture should carry fuel, ballast and ammunition");
    let got = stores_mass(&p);
    let want = crate::mass::with_fluids(&p.dry_mass, &p.parts, &p.compartments, cat());
    assert!(got.mass_kg > p.dry_mass.mass_kg + 2_000.0);
    assert!((got.mass_kg - want.mass_kg).abs() < 1e-6 * want.mass_kg, "{} vs {}", got.mass_kg, want.mass_kg);
    assert!((got.com - want.com).length() < 1e-6, "{} vs {}", got.com, want.com);
}
