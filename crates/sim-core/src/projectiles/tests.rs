use super::*;
use crate::terrain::Terrain;
use content::campaign::{Island, MapDef};
use content::cell::{Cell, CellGrid, Material};
use content::part::Flammable;
use content::tuning::ProjectileTuning;
use std::sync::Arc;

fn e(i: u32) -> EntityId { EntityId { index: i, generation: 0 } }
fn calm(_: f64, _: f64) -> f64 { 0.0 }

fn bands() -> Vec<BandDef> {
    CalibreBand::ALL.iter().map(|&b| { let mut d = BandDef::spec_default(b); d.drag_k = 1e-4; d }).collect()
}

/// Solid steel block of the given size.
fn block(size: [u16; 3]) -> VesselGrid {
    let mut g = CellGrid::new(size);
    for c in g.cells.iter_mut() { *c = Cell { material: Material::Steel, ..Cell::EMPTY }; }
    VesselGrid::build(Arc::new(g), &[], &|_| (false, Flammable::No))
}

fn hull_at(id: EntityId, origin: DVec3, size: [u16; 3]) -> HullTarget {
    let max = origin + DVec3::new(size[0] as f64, size[1] as f64, size[2] as f64) * CELL_M;
    HullTarget { vessel: id, bounds: Aabb { min: origin, max }, pose: Pose::new(origin, glam::DQuat::IDENTITY) }
}

fn run(p: &mut Projectiles, tick: u64, hulls: &[HullTarget], spheres: &[SphereTarget], grids: &[(EntityId, &VesselGrid)], terrain: &Terrain) -> ProjectileStepOutput {
    let mut out = ProjectileStepOutput::default();
    let lookup = |id: EntityId| grids.iter().find(|g| g.0 == id).map(|g| g.1);
    p.step_with(tick, &bands(), hulls, spheres, &lookup, &calm, terrain, &ProjectileTuning::default(), &mut out);
    out
}

#[test]
fn ballistic_step_is_semi_implicit() {
    let (p, v) = ballistic_step(DVec3::ZERO, DVec3::new(100.0, 0.0, 0.0), 0.0, 0.5);
    assert_eq!(v, DVec3::new(100.0, -GRAVITY * 0.5, 0.0));
    assert_eq!(p, v * 0.5);
    let (_, vd) = ballistic_step(DVec3::ZERO, DVec3::new(100.0, 0.0, 0.0), 1e-3, 0.1);
    assert!((vd.x - (100.0 - 1e-3 * 100.0 * 100.0 * 0.1)).abs() < 1e-12);
    let (_, vn) = ballistic_step(DVec3::ZERO, DVec3::X, f64::NAN, 0.1);
    assert_eq!(vn.x, 1.0);
}

#[test]
fn penetration_law() {
    let b = BandDef::spec_default(CalibreBand::Large);
    assert!((penetration_mm(&b, 800.0, 800.0) - 90.0).abs() < 1e-9);
    assert!((penetration_mm(&b, 400.0, 800.0) - 90.0 * libm::pow(0.5, 1.4)).abs() < 1e-9);
    assert_eq!(penetration_mm(&b, 500.0, 0.0), 90.0);
    assert_eq!(penetration_mm(&b, 0.0, 800.0), 0.0);
    assert_eq!(penetration_mm(&b, f64::NAN, 800.0), 0.0);
}

#[test]
fn drag_fit_meets_every_spec_row_within_five_percent() {
    for band in CalibreBand::ALL {
        let row = BandDef::spec_default(band);
        let k = fit_drag_k(&row);
        assert!(k > 1e-7 && k < 1e-2, "{band:?} k = {k}");
        let l = landing_at_range(row.ref_muzzle_mps, k, row.cap_range_m).expect("cap range reachable");
        assert!((l.range_m - row.cap_range_m).abs() < 1.0, "{band:?} landed at {}", l.range_m);
        let p = penetration_mm(&row, l.v_land, l.v_1km);
        assert!((p - row.p_cap_mm).abs() / row.p_cap_mm < 0.05, "{band:?}: {p} vs {}", row.p_cap_mm);
    }
    let mut bad = BandDef::spec_default(CalibreBand::Auto);
    bad.p_cap_mm = 30.0;
    assert_eq!(fit_drag_k(&bad), 0.0);
    bad.ref_muzzle_mps = f64::NAN;
    assert_eq!(fit_drag_k(&bad), 0.0);
}

#[test]
fn spawn_assigns_ids_and_merges_autocannon_per_mount_over_cap() {
    let mut p = Projectiles::default();
    let v = DVec3::new(500.0, 10.0, 0.0);
    let a = p.spawn(0, DVec3::ZERO, v, 30, CalibreBand::Auto, e(1), PartSlot(0), false, 2).unwrap();
    let b = p.spawn(0, DVec3::ZERO, v, 30, CalibreBand::Auto, e(1), PartSlot(1), false, 2).unwrap();
    assert_eq!((a.id, b.id, a.rounds), (ProjectileId(0), ProjectileId(1), 1));
    // Over the cap: mount 0 carries 2, mount 1 carries 2, then each mount's next round is dropped.
    let c = p.spawn(1, DVec3::ZERO, v, 30, CalibreBand::Auto, e(1), PartSlot(0), false, 2).unwrap();
    let d = p.spawn(1, DVec3::ZERO, v, 30, CalibreBand::Auto, e(1), PartSlot(1), false, 2).unwrap();
    assert_eq!((c.rounds, d.rounds, c.id, d.id), (2, 2, ProjectileId(2), ProjectileId(3)));
    assert!(p.spawn(2, DVec3::ZERO, v, 30, CalibreBand::Auto, e(1), PartSlot(0), false, 2).is_none());
    assert!(p.spawn(2, DVec3::ZERO, v, 30, CalibreBand::Auto, e(1), PartSlot(1), false, 2).is_none());
    assert_eq!(p.spawn(3, DVec3::ZERO, v, 30, CalibreBand::Auto, e(1), PartSlot(0), false, 2).unwrap().rounds, 2);
    // Big guns are never merged.
    assert_eq!(p.spawn(3, DVec3::ZERO, v, 127, CalibreBand::Large, e(1), PartSlot(5), false, 2).unwrap().rounds, 1);
    assert!(p.spawn(3, DVec3::NAN, v, 127, CalibreBand::Large, e(1), PartSlot(5), false, 2).is_none());
    assert_eq!(p.len(), 6);
    assert!(p.id.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn shell_falls_into_the_sea_and_splashes_at_the_surface() {
    let mut p = Projectiles::default();
    p.spawn(0, DVec3::new(0.0, 0.5, 0.0), DVec3::new(300.0, -60.0, 0.0), 76, CalibreBand::Medium, e(1), PartSlot(0), false, 2000).unwrap();
    let out = run(&mut p, 0, &[], &[], &[], &Terrain::default());
    assert_eq!(out.splashes.len(), 1);
    assert!(out.splashes[0].pos.y.abs() < 1e-6);
    assert!(p.is_empty());
}

#[test]
fn point_defence_rounds_expire_after_four_seconds() {
    let mut p = Projectiles::default();
    p.spawn(0, DVec3::new(0.0, 100.0, 0.0), DVec3::new(0.0, 1000.0, 0.0), 30, CalibreBand::Auto, e(1), PartSlot(0), true, 2000).unwrap();
    let mut expired_at = None;
    for t in 0..300 {
        let out = run(&mut p, t, &[], &[], &[], &Terrain::default());
        if !out.expired.is_empty() { expired_at = Some(t); break; }
    }
    assert_eq!(expired_at, Some(239));
}

#[test]
fn shell_hits_the_first_solid_cell_and_skips_its_own_hull() {
    let grid = block([20, 8, 10]);
    let target = hull_at(e(2), DVec3::new(10.0, -2.0, -2.5), [20, 8, 10]);
    let own = hull_at(e(1), DVec3::new(-5.0, -2.0, -2.5), [20, 8, 10]);
    let mut p = Projectiles::default();
    // Fired from inside its own hull towards the target, 2 m above the sea.
    p.spawn(0, DVec3::new(0.0, 1.0, 0.0), DVec3::new(900.0, 0.0, 0.0), 76, CalibreBand::Medium, e(1), PartSlot(0), false, 2000).unwrap();
    let out = run(&mut p, 0, &[own, target], &[], &[(e(1), &grid), (e(2), &grid)], &Terrain::default());
    assert_eq!(out.hits.len(), 1);
    let h = out.hits[0];
    assert_eq!(h.vessel, e(2));
    assert!((h.world_point.x - 10.0).abs() < 1e-6, "{:?}", h.world_point);
    assert_eq!(grid.pos(h.first_cell).x, 0);
    assert!(h.entry[0].abs() < 1e-6 && (h.dir[0] - 1.0).abs() < 1e-3);
    assert!(h.v_1km > h.speed_mps * 0.5 && h.v_1km < h.speed_mps);
    assert!(p.is_empty());
}

#[test]
fn rotated_hull_maps_the_hit_into_its_cell_frame() {
    // Grid 2 x 4 x 20 cells, yawed 90 degrees: local +Z points along world +X, local +X along world -Z.
    let grid = block([2, 4, 20]);
    let origin = DVec3::new(10.0, -1.0, 0.0);
    let rot = glam::DQuat::from_rotation_y(core::f64::consts::FRAC_PI_2);
    let pose = Pose::new(origin, rot);
    let corners = [DVec3::ZERO, DVec3::new(2.0, 4.0, 20.0) * CELL_M];
    let (a, b) = (pose.to_world(corners[0]), pose.to_world(corners[1]));
    let target = HullTarget { vessel: e(2), bounds: Aabb { min: a.min(b), max: a.max(b) }, pose };
    let mut p = Projectiles::default();
    p.spawn(0, DVec3::new(0.0, 0.6, -0.75), DVec3::new(900.0, 0.0, 0.0), 76, CalibreBand::Medium, e(1), PartSlot(0), false, 2000).unwrap();
    let out = run(&mut p, 0, &[target], &[], &[(e(2), &grid)], &Terrain::default());
    assert_eq!(out.hits.len(), 1);
    let h = out.hits[0];
    let c = grid.pos(h.first_cell);
    // World z = -0.75 is local x = 1.5 cells; world y = 0.6 is local y = 3.2 cells; the shell enters at local z = 0.
    assert_eq!((c.x, c.y, c.z), (1, 3, 0));
    assert!(h.entry[2].abs() < 1e-3 && (h.dir[2] - 1.0).abs() < 1e-3, "{h:?}");
    assert!((h.world_point.x - 10.0).abs() < 1e-3, "{:?}", h.world_point);
}

/// The runtime's `v_1km` and impact speed must reproduce the fit: a shell fired with the fitted k at the
/// elevation that lands at cap range, stopped by a wall just short of it, yields `p_cap` within 5%.
#[test]
fn runtime_penetration_at_cap_range_matches_the_fit() {
    for band in CalibreBand::ALL {
        let mut row = BandDef::spec_default(band);
        row.drag_k = fit_drag_k(&row);
        let (v0, k, cap) = (row.ref_muzzle_mps, row.drag_k, row.cap_range_m);
        let (mut lo, mut hi) = (0.0f64, 0.7f64);
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            if fly(v0, mid, k).is_some_and(|l| l.range_m >= cap) { hi = mid; } else { lo = mid; }
        }
        let grid = block([1, 120, 4]);
        let wall_x = cap - 2.0;
        let target = hull_at(e(2), DVec3::new(wall_x, -10.0, -1.0), [1, 120, 4]);
        let bands: Vec<BandDef> = CalibreBand::ALL.iter().map(|&b| if b == band { row } else { BandDef::spec_default(b) }).collect();
        let mut p = Projectiles::default();
        p.spawn(0, DVec3::ZERO, DVec3::new(v0 * libm::cos(hi), v0 * libm::sin(hi), 0.0), 100, band, e(1), PartSlot(0), false, 2000).unwrap();
        let lookup = |id: EntityId| if id == e(2) { Some(&grid) } else { None };
        // Spawned at the surface: give the first tick's start a hair of freeboard below the wave sample.
        let water = |_: f64, _: f64| -0.01;
        let mut hit = None;
        for t in 0..MAX_FLIGHT_TICKS {
            let mut out = ProjectileStepOutput::default();
            p.step_with(t, &bands, &[target], &[], &lookup, &water, &Terrain::default(), &ProjectileTuning::default(), &mut out);
            if let Some(h) = out.hits.first() { hit = Some(*h); break; }
            assert!(out.splashes.is_empty(), "{band:?} splashed before the wall");
        }
        let h = hit.expect("shell reached the wall");
        let pen = penetration_mm(&row, h.speed_mps, h.v_1km);
        assert!((pen - row.p_cap_mm).abs() / row.p_cap_mm < 0.05, "{band:?}: {pen} vs {}", row.p_cap_mm);
    }
}

#[test]
fn fast_shell_does_not_tunnel_through_a_thin_hull() {
    // One cell thick hull, 1,000 m/s shell (about 33 cells per tick).
    let mut g = CellGrid::new([1, 4, 4]);
    for c in g.cells.iter_mut() { *c = Cell { material: Material::Steel, ..Cell::EMPTY }; }
    let grid = VesselGrid::build(Arc::new(g), &[], &|_| (false, Flammable::No));
    let target = hull_at(e(2), DVec3::new(8.3, 0.0, -1.0), [1, 4, 4]);
    let mut p = Projectiles::default();
    p.spawn(0, DVec3::new(0.0, 1.0, 0.0), DVec3::new(1000.0, 0.0, 0.0), 30, CalibreBand::Auto, e(1), PartSlot(0), false, 2000).unwrap();
    let out = run(&mut p, 0, &[target], &[], &[(e(2), &grid)], &Terrain::default());
    assert_eq!(out.hits.len(), 1);
}

#[test]
fn hits_are_sorted_and_spheres_are_grazed_with_the_pd_margin() {
    let grid = block([4, 4, 4]);
    let target = hull_at(e(9), DVec3::new(10.0, 0.0, -1.0), [4, 4, 4]);
    let mut p = Projectiles::default();
    for k in 0..3 {
        p.spawn(0, DVec3::new(0.0, 1.0 + 0.2 * k as f64, 0.0), DVec3::new(900.0, 0.0, 0.0), 30, CalibreBand::Auto, e(1 + k), PartSlot(0), false, 2000).unwrap();
    }
    // A point-defence round passing 1.2 m from a 0.5 m sphere's surface, and a plain round passing as close.
    p.spawn(0, DVec3::new(0.0, 40.0, 0.0), DVec3::new(900.0, 0.0, 0.0), 30, CalibreBand::Auto, e(1), PartSlot(1), true, 2000).unwrap();
    p.spawn(0, DVec3::new(0.0, 40.0, 30.0), DVec3::new(900.0, 0.0, 0.0), 30, CalibreBand::Auto, e(1), PartSlot(1), false, 2000).unwrap();
    let spheres = [
        SphereTarget { body: e(20), centre: DVec3::new(8.0, 41.7, 0.0), radius: 0.5 },
        SphereTarget { body: e(21), centre: DVec3::new(8.0, 41.7, 30.0), radius: 0.5 },
    ];
    let out = run(&mut p, 0, &[target], &spheres, &[(e(9), &grid)], &Terrain::default());
    assert_eq!(out.hits.len(), 3);
    assert!(out.hits.windows(2).all(|w| w[0].projectile < w[1].projectile));
    assert_eq!(out.grazes.len(), 1);
    assert_eq!(out.grazes[0].body, e(20));
    assert_eq!(p.len(), 1);
}

#[test]
fn moving_sphere_is_swept_relative_to_the_shell() {
    let mut p = Projectiles::default();
    // A crossing missile: last tick at z = -4, now at z = +4; the shell passes x = 7.5 mid-tick at z = 0.
    p.spawn(0, DVec3::new(0.0, 40.0, 0.0), DVec3::new(900.0, 0.0, 0.0), 30, CalibreBand::Auto, e(1), PartSlot(0), false, 2000).unwrap();
    p.sphere_prev = vec![(e(20), DVec3::new(7.5, 40.0, -4.0))];
    let spheres = [SphereTarget { body: e(20), centre: DVec3::new(7.5, 40.0, 4.0), radius: 0.5 }];
    let out = run(&mut p, 0, &[], &spheres, &[], &Terrain::default());
    assert_eq!(out.grazes.len(), 1);
}

#[test]
fn land_ends_the_shell() {
    let map = MapDef { size_m: [10_000.0, 10_000.0], depth_m: 100.0, islands: vec![Island { centre_m: [30.0, 0.0], radius_m: 200.0, height_m: 60.0, seed: 1 }], markers: vec![], seed: 3 };
    let terrain = Terrain::new(map);
    let mut p = Projectiles::default();
    p.spawn(0, DVec3::new(-400.0, 5.0, 0.0), DVec3::new(900.0, 0.0, 0.0), 76, CalibreBand::Medium, e(1), PartSlot(0), false, 2000).unwrap();
    let mut ended = None;
    for t in 0..60 {
        let out = run(&mut p, t, &[], &[], &[], &terrain);
        if let Some(s) = out.splashes.first() { ended = Some(s.pos); break; }
    }
    let pos = ended.expect("shell struck land");
    assert!(terrain.height_at(pos.x, pos.z) > pos.y - 1.0, "{pos:?}");
    assert!(pos.x < 30.0);
}

#[test]
fn threatens_sees_next_tick_segments() {
    let mut p = Projectiles::default();
    p.spawn(0, DVec3::new(0.0, 5.0, 0.0), DVec3::new(600.0, 0.0, 0.0), 76, CalibreBand::Medium, e(1), PartSlot(0), false, 2000).unwrap();
    assert!(p.threatens(&Aabb { min: DVec3::new(8.0, 0.0, -2.0), max: DVec3::new(20.0, 10.0, 2.0) }));
    assert!(!p.threatens(&Aabb { min: DVec3::new(30.0, 0.0, -2.0), max: DVec3::new(40.0, 10.0, 2.0) }));
}

#[test]
fn malformed_store_does_not_panic() {
    let mut p = Projectiles::default();
    p.spawn(0, DVec3::new(0.0, 100.0, 0.0), DVec3::new(100.0, 0.0, 0.0), 30, CalibreBand::Auto, e(1), PartSlot(0), false, 2000).unwrap();
    p.vel.clear();
    let out = run(&mut p, 0, &[], &[], &[], &Terrain::default());
    assert!(p.is_empty() && out.hits.is_empty());
    let mut q = Projectiles::default();
    q.spawn(0, DVec3::new(0.0, 100.0, 0.0), DVec3::new(100.0, 0.0, 0.0), 30, CalibreBand::Auto, e(1), PartSlot(0), false, 2000).unwrap();
    let bad = HullTarget { vessel: e(3), bounds: Aabb { min: DVec3::NAN, max: DVec3::INFINITY }, pose: Pose::default() };
    let _ = run(&mut q, 0, &[bad], &[SphereTarget { body: e(4), centre: DVec3::NAN, radius: -1.0 }], &[], &Terrain::default());
    assert_eq!(q.len(), 1);
}

#[test]
fn step_is_deterministic() {
    let grid = block([20, 8, 10]);
    let target = hull_at(e(2), DVec3::new(300.0, -2.0, -2.5), [20, 8, 10]);
    let mk = || {
        let mut p = Projectiles::default();
        for k in 0..50u32 {
            let v = DVec3::new(600.0 + k as f64, 5.0 + (k % 7) as f64, (k % 5) as f64 - 2.0);
            p.spawn(0, DVec3::new(0.0, 3.0, 0.0), v, 30, CalibreBand::Auto, e(1), PartSlot(k as u16), false, 2000);
        }
        p
    };
    let (mut a, mut b) = (mk(), mk());
    for t in 0..120 {
        let oa = run(&mut a, t, &[target], &[], &[(e(2), &grid)], &Terrain::default());
        let ob = run(&mut b, t, &[target], &[], &[(e(2), &grid)], &Terrain::default());
        assert_eq!(oa, ob);
    }
    assert_eq!(a, b);
}
