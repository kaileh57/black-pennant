use super::*;
use content::campaign::Island;

fn map() -> MapDef {
    MapDef {
        size_m: [30_000.0, 30_000.0],
        depth_m: 80.0,
        islands: vec![Island { centre_m: [4000.0, -2000.0], radius_m: 1500.0, height_m: 120.0, seed: 0 }],
        markers: vec![],
        seed: 7,
    }
}

#[test]
fn island_profile() {
    let t = Terrain::new(map());
    let peak = t.height_at(4000.0, -2000.0);
    assert!(peak > 0.8 * 120.0 - 1e-9 && peak <= 120.0, "{peak}");
    // Far water is the open depth.
    assert_eq!(t.height_at(-12_000.0, 10_000.0), -80.0);
    assert_eq!(t.depth_at(-12_000.0, 10_000.0), 80.0);
    assert_eq!(t.depth_at(4000.0, -2000.0), 0.0);
    // Shore stays within 16% of the authored radius in every direction.
    for k in 0..36 {
        let a = k as f64 * core::f64::consts::TAU / 36.0;
        let (c, s) = (libm::cos(a), libm::sin(a));
        let at = |r: f64| t.height_at(4000.0 + c * r, -2000.0 + s * r);
        assert!(at(1500.0 * 0.83) > 0.0, "land inside at {k}");
        assert!(at(1500.0 * 1.17) < 0.0, "water outside at {k}");
        // Shelf deepens monotonically outward.
        assert!(at(1500.0 * 1.17 + 100.0) <= at(1500.0 * 1.17));
    }
    // The example depot marker (3500, 0) is in water.
    assert!(t.depth_at(3500.0, 0.0) > 0.0);
}

#[test]
fn deterministic_and_seeded() {
    let a = Terrain::new(map());
    let b = Terrain::new(map());
    let mut m = map();
    m.seed = 8;
    let c = Terrain::new(m);
    let p = (4000.0 + 1400.0, -2000.0 + 300.0);
    assert_eq!(a.height_at(p.0, p.1).to_bits(), b.height_at(p.0, p.1).to_bits());
    let differs = (0..20).any(|k| {
        let x = 4000.0 + 1450.0 * libm::cos(k as f64 * 0.3);
        let z = -2000.0 + 1450.0 * libm::sin(k as f64 * 0.3);
        a.height_at(x, z) != c.height_at(x, z)
    });
    assert!(differs);
}

#[test]
fn bad_input_is_safe() {
    let mut m = map();
    m.depth_m = f64::NAN;
    m.islands.push(Island { centre_m: [0.0, 0.0], radius_m: -5.0, height_m: 50.0, seed: 1 });
    m.islands.push(Island { centre_m: [f64::NAN, 0.0], radius_m: 100.0, height_m: 50.0, seed: 1 });
    m.islands.push(Island { centre_m: [0.0, 0.0], radius_m: 100.0, height_m: f64::INFINITY, seed: 1 });
    let t = Terrain::new(m);
    assert_eq!(t.height_at(0.0, 0.0), -DEFAULT_DEPTH_M);
    assert_eq!(t.height_at(f64::NAN, 0.0), -DEFAULT_DEPTH_M);
    assert!(!t.segment_blocked(DVec3::new(f64::NAN, 0.0, 0.0), DVec3::ZERO));
    let d = Terrain::default();
    assert_eq!(d.depth_grid(50.0).depth.len(), 0);
    assert_eq!(d.depth_at(1.0, 2.0), DEFAULT_DEPTH_M);
    assert!(d.sample_patch([0.0, 0.0], 100.0, 0).is_empty());
    assert_eq!(d.sample_patch([0.0, 0.0], f64::NAN, 3).len(), 9);
    let g = Terrain::new(map()).depth_grid(-1.0);
    assert_eq!(g.cell_m, PATH_CELL_M);
}

#[test]
fn shoal_stays_under_water() {
    let mut m = map();
    m.islands = vec![Island { centre_m: [0.0, 0.0], radius_m: 500.0, height_m: -3.0, seed: 2 }];
    let t = Terrain::new(m);
    assert!((t.height_at(0.0, 0.0) + 3.0).abs() < 1e-9);
    assert!(t.height_at(300.0, 0.0) < -3.0);
    assert_eq!(t.height_at(5000.0, 0.0), -80.0);
}

#[test]
fn depth_grid_matches_point_queries() {
    let t = Terrain::new(map());
    let g = t.depth_grid(PATH_CELL_M);
    assert_eq!((g.cols, g.rows), (600, 600));
    assert_eq!(g.origin, [-15_000.0, -15_000.0]);
    assert_eq!(g.depth.len(), 360_000);
    let cell = |x: f64, z: f64| {
        let cx = ((x - g.origin[0]) / g.cell_m) as usize;
        let cz = ((z - g.origin[1]) / g.cell_m) as usize;
        g.depth[cx + g.cols as usize * cz]
    };
    assert!(cell(4010.0, -1990.0) < 0.0);
    assert_eq!(cell(-12_000.0, 10_000.0), 80.0);
    // Each cell is no deeper than the depth at its centre.
    for &(x, z) in &[(5400.0, -2000.0), (4000.0, -500.0), (2600.0, -2200.0)] {
        let cx = ((x - g.origin[0]) / g.cell_m).floor();
        let cz = ((z - g.origin[1]) / g.cell_m).floor();
        let centre = (g.origin[0] + (cx + 0.5) * g.cell_m, g.origin[1] + (cz + 0.5) * g.cell_m);
        assert!(cell(x, z) as f64 <= -t.height_at(centre.0, centre.1) + 1e-3);
    }
}

#[test]
fn patch_layout() {
    let t = Terrain::new(map());
    let n = 9;
    let p = t.sample_patch([4000.0, -2000.0], 4000.0, n);
    assert_eq!(p.len(), 81);
    // Centre sample is the island top; corners are on the patch edges.
    assert_eq!(p[4 + 9 * 4], t.height_at(4000.0, -2000.0) as f32);
    assert_eq!(p[0], t.height_at(2000.0, -4000.0) as f32);
    assert_eq!(p[8], t.height_at(6000.0, -4000.0) as f32);
    assert_eq!(p[9 * 8], t.height_at(2000.0, 0.0) as f32);
    assert_eq!(t.sample_patch([1.0, 2.0], 10.0, 1), vec![t.height_at(1.0, 2.0) as f32]);
}

#[test]
fn line_of_sight() {
    let t = Terrain::new(map());
    let over_island = |y: f64| t.segment_blocked(DVec3::new(0.0, y, -2000.0), DVec3::new(8000.0, y, -2000.0));
    assert!(over_island(10.0));
    assert!(!over_island(200.0));
    // Open water.
    assert!(!t.segment_blocked(DVec3::new(-10_000.0, 5.0, 5000.0), DVec3::new(10_000.0, 5.0, 5000.0)));
    // A sensor sitting on the ground does not block itself.
    let g = t.height_at(4000.0, -2000.0);
    assert!(!t.segment_blocked(DVec3::new(4000.0, g, -2000.0), DVec3::new(4000.0, g + 300.0, -2000.0)));
    // Underwater: below the seabed is blocked; mid-water in the open is not.
    assert!(t.segment_blocked(DVec3::new(-10_000.0, -90.0, 0.0), DVec3::new(-9000.0, -10.0, 0.0)));
    assert!(!t.segment_blocked(DVec3::new(-10_000.0, -20.0, 0.0), DVec3::new(-9000.0, -20.0, 0.0)));
    // Underwater into the island's shelf is blocked.
    assert!(t.segment_blocked(DVec3::new(0.0, -30.0, -2000.0), DVec3::new(4000.0, -30.0, -2000.0)));
    // Zero-length segment.
    assert!(!t.segment_blocked(DVec3::new(0.0, 5.0, 0.0), DVec3::new(0.0, 5.0, 0.0)));
}
