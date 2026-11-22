use super::*;
use content::cell::{ArmourTier, Cell, CellGrid, CellGridView, CellPos, CellShape, Material};

/// Reference binary16 decoder.
fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0f64 } else { 1.0 };
    let e = ((h >> 10) & 0x1F) as i32;
    let m = (h & 0x3FF) as f64;
    let v = match e {
        0 => libm::ldexp(m, -24),
        31 => if m == 0.0 { f64::INFINITY } else { f64::NAN },
        _ => libm::ldexp(1.0 + m / 1024.0, e - 15),
    };
    (sign * v) as f32
}

#[test]
fn f16_round_trips_every_pattern() {
    for h in 0..=u16::MAX {
        let v = f16_to_f32(h);
        let back = f32_to_f16_bits(v);
        if v.is_nan() {
            assert!(f16_to_f32(back).is_nan(), "{h:#06x}");
            assert_eq!(back & 0x8000, h & 0x8000);
        } else {
            assert_eq!(back, h, "{h:#06x} {v}");
        }
    }
}

#[test]
fn f16_rounds_to_nearest_even() {
    assert_eq!(f32_to_f16_bits(1.0), 0x3C00);
    assert_eq!(f32_to_f16_bits(-2.0), 0xC000);
    assert_eq!(f32_to_f16_bits(-0.0), 0x8000);
    assert_eq!(f32_to_f16_bits(65_504.0), 0x7BFF);
    assert_eq!(f32_to_f16_bits(65_519.0), 0x7BFF);
    assert_eq!(f32_to_f16_bits(65_520.0), 0x7C00);
    assert_eq!(f32_to_f16_bits(-1e9), 0xFC00);
    assert_eq!(f32_to_f16_bits(f32::INFINITY), 0x7C00);
    // Halfway between 1 and the next half: ties to even (down), then up from an odd mantissa.
    let ulp = 1.0 / 1024.0;
    assert_eq!(f32_to_f16_bits(1.0 + ulp / 2.0), 0x3C00);
    assert_eq!(f32_to_f16_bits(1.0 + 1.5 * ulp), 0x3C02);
    assert_eq!(f32_to_f16_bits(1.0 + ulp / 2.0 + 1e-7), 0x3C01);
    // The mantissa carry rolls into the exponent.
    assert_eq!(f32_to_f16_bits(2.0 - ulp / 4.0), 0x4000);
    // Subnormals and underflow.
    let tiny = libm::ldexpf(1.0, -24);
    assert_eq!(f32_to_f16_bits(tiny), 0x0001);
    assert_eq!(f32_to_f16_bits(tiny / 2.0), 0x0000);
    assert_eq!(f32_to_f16_bits(tiny * 1.5), 0x0002);
    assert_eq!(f32_to_f16_bits(tiny * 0.51), 0x0001);
    assert_eq!(f32_to_f16_bits(-tiny / 4.0), 0x8000);
    assert_eq!(f32_to_f16_bits(f32::from_bits(1)), 0x0000);
    assert_eq!(f32_to_f16_bits(libm::ldexpf(1.0, -14) * (1.0 - 1.0 / 4096.0)), 0x0400);
    assert!(f16_to_f32(f32_to_f16_bits(f32::NAN)).is_nan());
}

fn steel() -> Cell { Cell { material: Material::Steel, armour: ArmourTier::None, paint: 0, shape: CellShape::CUBE } }

/// A closed box hull: one-cell walls around an interior, `n` long, `h` high and `w` wide.
fn box_hull(n: i16, h: i16, w: i16) -> CellGrid {
    let mut g = CellGrid::new([n as u16, h as u16, w as u16]);
    for x in 0..n {
        for y in 0..h {
            for z in 0..w {
                if x == 0 || y == 0 || z == 0 || x == n - 1 || y == h - 1 || z == w - 1 {
                    g.set(CellPos::new(x, y, z), steel());
                }
            }
        }
    }
    g
}

fn sealed_interior(g: &CellGrid) -> Vec<u64> {
    let mut bits = vec![0u64; g.len().div_ceil(64)];
    for (i, c) in g.cells.iter().enumerate() {
        if c.is_empty() {
            bits[i / 64] |= 1 << (i % 64);
        }
    }
    bits
}

fn input<'a>(g: &'a CellGrid, damage: &'a [u8], sealed: &'a [u64], wl: i16) -> MeshInput<'a> {
    MeshInput { view: CellGridView { size: g.size, cells: &g.cells, damage, sealed_air: sealed, scorch: &[] }, palette: &[], wear_seed: 0, design_waterline_y: wl }
}

/// Texel of cell column (x, z) for a grid `w` cells wide.
fn texel(m: &HullMask, x: usize, z: i32, w: i32) -> [f32; 4] {
    let v = (z - w / 2 + 64) as usize;
    m.texels[x + MASK_W * v].map(f16_to_f32)
}

#[test]
fn intact_hull_columns_are_dry_from_keel_to_deck() {
    let g = box_hull(40, 10, 13);
    let sealed = sealed_interior(&g);
    for m in [build_hull_mask(&input(&g, &[], &sealed, 4)), build_hull_mask(&input(&g, &[], &[], 4))] {
        assert_eq!(m.texels.len(), MASK_W * MASK_H);
        assert_eq!(m.offset_z_m, 3.0);
        for (x, z) in [(0, 0), (20, 6), (39, 12), (5, 3)] {
            let t = texel(&m, x, z, 13);
            assert_eq!(t, [0.0, 5.0, t[2], 1.0], "column {x} {z}");
            assert!(t[2] < 0.0);
        }
        // Outside the hull: no column, positive distance.
        let out = texel(&m, 45, 6, 13);
        assert_eq!([out[0], out[1], out[3]], [MASK_LARGE, -MASK_LARGE, 0.0]);
        // Texel centre 6 cells past the last hull column: 5.5 texels = 2.75 m.
        assert!((out[2] - 2.75).abs() < 1e-3, "{}", out[2]);
        let beyond = texel(&m, 10, -3, 13);
        assert!((beyond[2] - 1.25).abs() < 1e-3);
        // Deep inside: distance to the nearest side (centre at z = 6, sides at 0 and 13 cells).
        let mid = texel(&m, 20, 6, 13);
        assert!((mid[2] + 3.25).abs() < 1e-3, "{}", mid[2]);
    }
}

#[test]
fn breaches_open_the_column() {
    let g = box_hull(40, 10, 13);
    let mut sealed = sealed_interior(&g);
    // Breach the compartment: its air is no longer sealed.
    // A zeroed, non-empty slice: a live vessel whose compartment is open.
    sealed.iter_mut().for_each(|w| *w = 0);
    let m = build_hull_mask(&input(&g, &[], &sealed, 4));
    let t = texel(&m, 20, 6, 13);
    // The runs are the bottom plate (row 0) and the deck (row 9); the bottom is nearer the waterline row 4.
    assert_eq!([t[0], t[1], t[3]], [0.0, 0.5, 1.0]);
    // The side wall column stays dry all the way.
    assert_eq!(texel(&m, 20, 0, 13)[..2], [0.0, 5.0]);
    // A removed cell in the side wall at the waterline splits that column; the outline still encloses the hull
    // except through the hole.
    let mut damage = vec![0u8; g.len()];
    damage[g.index(CellPos::new(20, 4, 0)).unwrap().index()] = 8;
    let sealed = sealed_interior(&g);
    let m = build_hull_mask(&input(&g, &damage, &sealed, 4));
    let wall = texel(&m, 20, 0, 13);
    assert_eq!(wall[..2], [2.5, 5.0]);
    assert!(texel(&m, 20, 6, 13)[2] > 0.0, "the hole floods the waterline slice");
    // With the compartment open and the waterline at row 1, the bottom plate is the nearest run.
    let m = build_hull_mask(&input(&g, &damage, &vec![0; sealed.len()], 1));
    assert_eq!(texel(&m, 20, 6, 13)[..2], [0.0, 0.5]);
    let mut col = vec![Dry::Yes, Dry::No, Dry::No, Dry::Yes];
    assert_eq!(dry_run(&mut col, 1), Some((0, 0)));
    let mut col = vec![Dry::Yes, Dry::No, Dry::No, Dry::Yes];
    assert_eq!(dry_run(&mut col, 2), Some((3, 3)));
    // Equally near runs: the upper one wins.
    let mut col = vec![Dry::Yes, Dry::No, Dry::No, Dry::No, Dry::Yes];
    assert_eq!(dry_run(&mut col, 2), Some((4, 4)));
}

#[test]
fn open_topped_hull_without_compartments() {
    // A U-section boat: floor and walls, no deck. Without sealed state the open interior is not dry.
    let mut g = box_hull(20, 6, 9);
    for x in 1..19 {
        for z in 1..8 {
            g.set(CellPos::new(x, 5, z), Cell::EMPTY);
        }
    }
    let m = build_hull_mask(&input(&g, &[], &[], 2));
    let t = texel(&m, 10, 4, 9);
    assert_eq!([t[0], t[1], t[3]], [0.0, 0.5, 1.0]);
    // The waterline outline still encloses the interior air.
    assert!(t[2] < 0.0);
}

#[test]
fn texel_mapping_and_edge_cases() {
    // Odd beam: offset is the cell boundary just below the centreline.
    let g = box_hull(10, 4, 7);
    let m = build_hull_mask(&input(&g, &[], &[], 1));
    assert_eq!(m.offset_z_m, 1.5);
    assert_eq!(m.texels[MASK_W * (64 - 3)][3], f32_to_f16_bits(1.0));
    assert_eq!(m.texels[MASK_W * (64 - 4)][3], 0);
    assert_eq!(m.texels[MASK_W * (64 + 3)][3], f32_to_f16_bits(1.0));
    assert_eq!(m.texels[MASK_W * (64 + 4)][3], 0);
    // Waterline outside the grid: no outline anywhere.
    let m = build_hull_mask(&input(&g, &[], &[], 40));
    assert!(m.texels.iter().all(|t| f16_to_f32(t[2]) == MASK_LARGE));
    assert_eq!(texel(&m, 3, 3, 7)[3], 1.0);
    // Malformed input: no hull at all, never a panic.
    let bad = MeshInput { view: CellGridView { size: g.size, cells: &g.cells[..3], damage: &[], sealed_air: &[], scorch: &[] }, palette: &[], wear_seed: 0, design_waterline_y: 1 };
    let m = build_hull_mask(&bad);
    assert!(m.texels.iter().all(|t| t[3] == 0 && f16_to_f32(t[0]) == MASK_LARGE && f16_to_f32(t[1]) == -MASK_LARGE));
    // A beam wider than the texture is cropped, a long hull fills x up to its length.
    let wide = box_hull(300, 3, 150);
    let m = build_hull_mask(&input(&wide, &[], &[], 1));
    assert_eq!(m.texels.len(), MASK_W * MASK_H);
    assert_eq!(f16_to_f32(m.texels[299 + MASK_W * 10][3]), 1.0);
    assert_eq!(m.texels[300 + MASK_W * 10][3], 0);
    // Empty grid.
    let none = CellGrid::new([0, 0, 0]);
    assert!(build_hull_mask(&input(&none, &[], &[], 0)).texels.iter().all(|t| t[3] == 0));
}

#[test]
fn sdf_matches_a_brute_force_distance() {
    // An L-shaped waterline footprint.
    let mut g = CellGrid::new([30, 3, 20]);
    for x in 0..30 {
        for z in 0..20 {
            if x < 10 || z < 6 {
                g.set(CellPos::new(x, 1, z), steel());
            }
        }
    }
    let m = build_hull_mask(&input(&g, &[], &[], 1));
    let inside = |u: i32, v: i32| -> bool {
        let z = v - 64 + 10;
        (0..30).contains(&u) && (0..20).contains(&z) && (u < 10 || z < 6)
    };
    for &(u, v) in &[(15i32, 70i32), (40, 60), (5, 64), (20, 56), (2, 90), (12, 62), (29, 54)] {
        let me = inside(u, v);
        let mut best = f64::MAX;
        for bu in 0..MASK_W as i32 {
            for bv in 0..MASK_H as i32 {
                if inside(bu, bv) != me {
                    let d = (((bu - u) * (bu - u) + (bv - v) * (bv - v)) as f64).sqrt();
                    best = best.min(d);
                }
            }
        }
        let want = if me { -(best - 0.5) * 0.5 } else { (best - 0.5) * 0.5 };
        let got = f16_to_f32(m.texels[u as usize + MASK_W * v as usize][2]) as f64;
        assert!((got - want).abs() < 0.01 + want.abs() * 1e-3, "({u}, {v}) got {got} want {want}");
    }
}
