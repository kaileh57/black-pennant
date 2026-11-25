//! Sea trials of the stock starter hulls through the whole tick: what the gauges predict against what the
//! simulation does, and the block kernel against the cells it stands for. The assertions are about the model
//! (level floating, tracking, gauge agreement, sane stopping), not about any one blueprint's numbers, so a
//! redesigned starter keeps passing as long as it is a sound hull.
use super::*;
use crate::parts::VesselControls;
use crate::time::DT;
use crate::vessel::{build_prefab, Fidelity, VesselPrefab, VesselSpawn};
use crate::world::{World, WorldConfig};
use content::campaign::MapDef;
use content::catalogue::Catalogue;
use content::ids::SideId;
use content::weather::{SeaState, WeatherState};
use std::sync::Arc;

fn starters(cat: &Catalogue) -> Vec<(&'static str, Arc<VesselPrefab>)> {
    let mut out: Vec<_> = content::data::stock_blueprint_files()
        .filter(|(p, _)| p.contains("starters/"))
        .map(|(p, text)| (p, Arc::new(build_prefab(&content::blueprint::io::load_blueprint(text, p).expect("starter loads").blueprint, cat))))
        .collect();
    out.sort_by_key(|(p, _)| *p);
    assert!(out.len() >= 6, "the six starter hulls are stock blueprints");
    out
}

fn loaded(prefab: &VesselPrefab, cat: &Catalogue) -> MassProperties { crate::mass::with_fluids(&prefab.dry_mass, &prefab.parts, &prefab.compartments, cat) }

/// Heel in degrees at which the righting arm first turns negative (180 when it never does).
fn range_of_stability(prefab: &VesselPrefab, mass: &MassProperties, cat: &Catalogue) -> f64 {
    gz_curve(&prefab.blocks, mass, 5.0, &cat.tuning.buoyancy).iter().skip(1).find(|(_, gz)| *gz <= 0.0).map_or(180.0, |(a, _)| *a)
}

#[test]
fn two_metre_blocks_float_a_real_hull_as_its_cells_do() {
    let cat = content::catalogue::catalogue();
    let tuning = &cat.tuning.buoyancy;
    for (path, prefab) in starters(cat) {
        let mass = loaded(&prefab, cat);
        // One-cell blocks are the cells themselves; the 2 m blocks and the 4 m far-hull blocks must agree.
        let exact = build_blocks(&prefab.grid, &prefab.compartments, 1);
        let truth = equilibrium(&exact, &mass, &[], &prefab.compartments, tuning);
        let truth_gz = gz_curve(&exact, &mass, 10.0, tuning);
        for (cells, tol) in [(4u8, 1.0), (8, 2.0)] {
            let blocks = build_blocks(&prefab.grid, &prefab.compartments, cells);
            assert!((blocks.total_volume_m3() - exact.total_volume_m3()).abs() < 1e-6, "{path}");
            let eq = equilibrium(&blocks, &mass, &[], &prefab.compartments, tuning);
            assert_eq!(eq.floats, truth.floats, "{path} at {cells} cells");
            assert!((eq.heel_rad - truth.heel_rad).abs() < tol * 0.006 && (eq.trim_rad - truth.trim_rad).abs() < tol * 0.003, "{path} at {cells} cells: {eq:?} vs {truth:?}");
            assert!((eq.gm_m - truth.gm_m).abs() < tol * (0.1 * truth.gm_m.abs() + 0.015), "{path} at {cells} cells: GM {} vs {}", eq.gm_m, truth.gm_m);
            assert!((eq.cb - truth.cb).length() < tol * 0.01 && (eq.draft_m - truth.draft_m).abs() < 0.15, "{path} at {cells} cells: {eq:?} vs {truth:?}");
            let gz = gz_curve(&blocks, &mass, 10.0, tuning);
            for (a, b) in gz.iter().zip(&truth_gz).take(10) { assert!((a.1 - b.1).abs() < tol * 0.015, "{path} at {cells} cells, {} degrees: GZ {} vs {}", a.0, a.1, b.1); }
        }
        // The flooding unit recovers the solver's plane from the reported heel, trim and draft.
        let eq = equilibrium(&prefab.blocks, &mass, &[], &prefab.compartments, tuning);
        let solved = statics::float(&prefab.blocks, &[], &mass, statics::water_density(tuning)).expect("floats").1.plane;
        let (u, c) = crate::flooding::equilibrium_plane(&prefab.blocks, &eq).expect("a plane");
        assert!((u - solved.u).length() < 1e-9 && (c - solved.c).abs() < 1e-9, "{path}: {u:?} {c} vs {solved:?}");
    }
}

fn calm_world(cat: &Arc<Catalogue>) -> World {
    World::new(WorldConfig { seed: 7, weather: WeatherState { sea_state: SeaState(0), ..WeatherState::default() }, map: MapDef { size_m: [40_000.0, 40_000.0], depth_m: 500.0, ..MapDef::default() }, catalogue: cat.clone() })
}

#[test]
fn starters_float_level_track_turn_and_stop_as_the_gauges_say() {
    let cat = Arc::new(content::catalogue::catalogue().clone());
    for (path, prefab) in starters(&cat) {
        let tuning = &cat.tuning.buoyancy;
        let mass = loaded(&prefab, &cat);
        let length = prefab.dims[0];
        let eq = equilibrium(&prefab.blocks, &mass, &[], &prefab.compartments, tuning);
        let (top, radius) = predict_speed_turn(&prefab, &cat, tuning);
        assert!(eq.floats && eq.gm_m > 0.0, "{path}: {eq:?}");
        assert!(top > 5.0 && top < 25.0 && radius > 1.5 * length && radius < 6.0 * length, "{path}: {top} m/s, turning radius {radius} m on {length} m");
        let mut w = calm_world(&cat);
        let id = w.spawn_vessel(&VesselSpawn { prefab: prefab.clone(), side: SideId(0), player: None, class: None, pose: crate::math::Pose::from_heading(DVec3::ZERO, 0.0), speed_mps: 0.0, damage: None, fuel_fraction: 1.0, ammo_fraction: 1.0, fidelity: Fidelity::Full, submersible: false });
        let hull = |w: &World| { let v = w.vessel(id).unwrap_or_else(|| panic!("{path} is afloat")); (v.kin, v.heel_trim(), v.forward_speed()) };
        // At rest it stays where the Float gauge put it: level, still, at the gauge's heel and trim.
        for _ in 0..300 { w.step(); }
        let (kin, (heel, trim), _) = hull(&w);
        assert!(kin.lin_vel.length() < 0.01 && kin.ang_vel.length() < 0.002, "{path} lies still: {kin:?}");
        assert!(heel.abs() < 0.05 && (heel - eq.heel_rad).abs() < 0.002 && (trim - eq.trim_rad).abs() < 0.002, "{path}: heel {heel} trim {trim} against {eq:?}");
        // Full ahead, helm amidships: it holds its course upright and makes the Speed gauge's top speed.
        w.set_controls(id, VesselControls { throttle: 1.0, ..Default::default() });
        for _ in 0..2100 { w.step(); }
        let earlier = hull(&w).0.ang_vel.y;
        for _ in 0..600 { w.step(); }
        let (kin, (heel, _), speed) = hull(&w);
        assert!((speed - top).abs() < 0.05 * top, "{path}: {speed} m/s against the gauge's {top}");
        // A list of a degree, as two of these hulls carry, walks the bow off by a few degrees a minute. What
        // must not happen is a swing that feeds itself: the yaw rate is small and has stopped growing.
        let yaw = kin.ang_vel.y;
        assert!(heel.abs() < 0.08 && yaw.abs() < 0.012 && yaw.abs() < 1.1 * earlier.abs() + 2e-4, "{path} tracks: heel {heel}, yaw rate {earlier} then {yaw}");
        // Helm hard a-starboard. A hull whose righting arm is gone by 45 degrees is not asked to survive it.
        if range_of_stability(&prefab, &mass, &cat) >= 45.0 {
            w.set_controls(id, VesselControls { throttle: 1.0, rudder: 1.0, ..Default::default() });
            let mut worst = 0.0f64;
            for _ in 0..2700 { w.step(); worst = worst.max(hull(&w).1 .0.abs()); }
            let (kin, (heel, _), _) = hull(&w);
            let turn = DVec3::new(kin.lin_vel.x, 0.0, kin.lin_vel.z).length() / kin.ang_vel.y.abs();
            assert!(kin.ang_vel.y < 0.0 && (turn - radius).abs() < 0.1 * radius, "{path}: turning radius {turn} m against the gauge's {radius}");
            // A sound hull (GM of 4% of beam or more, the Float gauge's Caution line) leans out by well under
            // 30 degrees even as it goes in; a tender one only has to stay up.
            let limit = if eq.gm_m >= 0.04 * prefab.dims[2] { 0.5 } else { 1.2 };
            assert!(heel < 0.0 && worst < limit, "{path} heels out of the turn and stays on its feet: {heel} rad, worst {worst}");
            w.set_controls(id, VesselControls { throttle: 1.0, ..Default::default() });
            for _ in 0..1800 { w.step(); }
        }
        // Crash stop, then astern: it stops in a few lengths and backs at well under its ahead speed.
        let (from, ahead) = (hull(&w).0.pose.pos, hull(&w).2);
        w.set_controls(id, VesselControls { throttle: -1.0, ..Default::default() });
        let mut ticks = 0;
        while hull(&w).2 > 0.0 && ticks < 7200 { w.step(); ticks += 1; }
        let run = (hull(&w).0.pose.pos - from).length();
        assert!(ticks < 7200 && run > 1.5 * length && run < 10.0 * length, "{path}: stops from {ahead} m/s in {run} m, {} s", ticks as f64 * DT);
        for _ in 0..2400 { w.step(); }
        let astern = -hull(&w).2;
        assert!(astern > 0.3 * top && astern < 0.75 * top, "{path}: {astern} m/s astern against {top} ahead");
    }
}
