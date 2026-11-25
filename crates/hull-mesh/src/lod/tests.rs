use super::*;
use crate::greedy::tests::{assert_clockwise, Rng};
use crate::surface::{encode_colored_surface, is_watertight};
use content::cell::{ArmourTier, Cell, CellGrid, CellGridView, CellPos, CellShape, Material};

const FOV60: f64 = core::f64::consts::PI / 3.0;
const PALETTE: [[f32; 3]; 4] = [[0.5, 0.5, 0.5], [0.1, 0.2, 0.3], [0.4, 0.3, 0.2], [0.6, 0.6, 0.6]];

fn steel(paint: u8) -> Cell { Cell { material: Material::Steel, armour: ArmourTier::None, paint, shape: CellShape::CUBE } }

fn input<'a>(g: &'a CellGrid, damage: &'a [u8], sealed: &'a [u64]) -> MeshInput<'a> {
    MeshInput { view: CellGridView { size: g.size, cells: &g.cells, damage, sealed_air: sealed, scorch: &[] }, palette: &PALETTE, wear_seed: 0, design_waterline_y: 0 }
}

fn fill(g: &mut CellGrid, lo: [i16; 3], hi: [i16; 3], c: Cell) {
    for x in lo[0]..hi[0] {
        for y in lo[1]..hi[1] {
            for z in lo[2]..hi[2] {
                g.set(CellPos::new(x, y, z), c);
            }
        }
    }
}

#[test]
fn cell_px_matches_the_reference_starts() {
    // L1786: 1 m at d m covers 935/d px at 1080p and 60 degrees.
    assert!((cell_px(Lod::L1, 935.3, FOV60, 1080.0) - 1.0).abs() < 1e-3);
    for lod in [Lod::L1, Lod::L2, Lod::L3, Lod::L4] {
        let px = cell_px(lod, lod.reference_start_m(), FOV60, 1080.0);
        assert!((px - 1.5).abs() < 0.005, "{lod:?} {px}");
    }
    // A 0.5 m cell is 4 px at 117 m and 1 px at 467 m (L1786).
    assert!((cell_px(Lod::L0, 117.0, FOV60, 1080.0) - 4.0).abs() < 0.01);
    assert!((cell_px(Lod::L0, 467.0, FOV60, 1080.0) - 1.0).abs() < 0.01);
    assert_eq!(cell_px(Lod::L2, 0.0, FOV60, 1080.0), f64::INFINITY);
    assert!(cell_px(Lod::L2, 10.0, 0.0, 1080.0).is_nan());
    assert!(cell_px(Lod::L2, 10.0, FOV60, 0.0).is_nan());
    assert!(cell_px(Lod::L2, f64::NAN, FOV60, 1080.0).is_nan());
}

#[test]
fn pick_lod_follows_the_threshold_with_hysteresis() {
    let pick = |d: f64, cur: Lod| pick_lod(d, FOV60, 1080.0, QualityTier::Medium, cur);
    assert_eq!(pick(100.0, Lod::L0), Lod::L0);
    // Approaching from LOD0: LOD1's cell must fall 15% under 1.5 px (about 734 m) before switching.
    assert_eq!(pick(700.0, Lod::L0), Lod::L0);
    assert_eq!(pick(740.0, Lod::L0), Lod::L1);
    // Coming back: LOD1's cell must exceed 1.5 px by 15% (about 542 m).
    assert_eq!(pick(560.0, Lod::L1), Lod::L1);
    assert_eq!(pick(540.0, Lod::L1), Lod::L0);
    // A camera cut jumps straight to the right LOD.
    assert_eq!(pick(20_000.0, Lod::L0), Lod::L4);
    assert_eq!(pick(50.0, Lod::L4), Lod::L0);
    assert_eq!(pick(0.0, Lod::L3), Lod::L0);
    // Low (and the fallback) use 3 px: twice the distance per LOD step.
    let low = pick_lod(1_000.0, FOV60, 1080.0, QualityTier::Low, Lod::L0);
    assert_eq!(low, Lod::L2);
    assert_eq!(pick_lod(1_000.0, FOV60, 1080.0, QualityTier::Fallback, Lod::L0), Lod::L2);
    assert_eq!(pick_lod(1_000.0, FOV60, 1080.0, QualityTier::High, Lod::L0), Lod::L1);
    // Garbage keeps the current LOD.
    assert_eq!(pick(f64::NAN, Lod::L2), Lod::L2);
    assert_eq!(pick_lod(500.0, 0.0, 1080.0, QualityTier::Medium, Lod::L3), Lod::L3);
    assert_eq!(pick_lod(500.0, FOV60, f64::INFINITY, QualityTier::Medium, Lod::L1), Lod::L1);
}

#[test]
fn pick_lod_is_a_fixed_point_and_monotonic() {
    for tier in [QualityTier::Fallback, QualityTier::Low, QualityTier::Medium, QualityTier::High] {
        for fov in [0.6, FOV60, 1.4] {
            for h in [720.0, 1080.0, 2160.0] {
                let mut prev_from_l0 = Lod::L0;
                let mut d = 1.0;
                while d < 40_000.0 {
                    for cur in Lod::ALL {
                        let p = pick_lod(d, fov, h, tier, cur);
                        assert_eq!(pick_lod(d, fov, h, tier, p), p, "d {d} {tier:?} {cur:?}");
                        // The picked cell never exceeds the threshold by more than the band, unless at LOD0.
                        let t = if matches!(tier, QualityTier::Low | QualityTier::Fallback) { 3.0 } else { 1.5 };
                        assert!(p == Lod::L0 || cell_px(p, d, fov, h) <= t * 1.15 + 1e-9);
                    }
                    let p = pick_lod(d, fov, h, tier, Lod::L0);
                    assert!(p >= prev_from_l0);
                    prev_from_l0 = p;
                    d *= 1.07;
                }
            }
        }
    }
}

#[test]
fn solid_block_is_six_bright_quads() {
    let mut g = CellGrid::new([16, 16, 16]);
    fill(&mut g, [0; 3], [16; 3], steel(1));
    let m = mesh_lod(&input(&g, &[], &[]), Lod::L1);
    assert_eq!(m.triangle_count(), 12);
    assert_eq!(m.colors.len(), m.positions.len());
    for c in &m.colors {
        assert_eq!(*c, [0.1, 0.2, 0.3, 1.0]);
    }
    assert_eq!(m.aabb(), crate::types::Aabb { min: [0.0; 3], max: [8.0; 3] });
    assert!(is_watertight(&m));
    assert_clockwise(&m);
    let s = encode_colored_surface(&m);
    assert_eq!(s.attribute_data.len(), 4 * m.vertex_count());
}

#[test]
fn any_solid_and_clipped_blocks() {
    let mut g = CellGrid::new([3, 3, 3]);
    g.set(CellPos::new(2, 2, 2), steel(0));
    let m = mesh_lod(&input(&g, &[], &[]), Lod::L1);
    // The block holding cell (2, 2, 2) spans cells 2..4 but is clipped to the grid edge at 3.
    assert_eq!(m.aabb(), crate::types::Aabb { min: [1.0; 3], max: [1.5; 3] });
    let m4 = mesh_lod(&input(&g, &[], &[]), Lod::L4);
    assert_eq!(m4.aabb(), crate::types::Aabb { min: [0.0; 3], max: [1.5; 3] });
    assert_eq!(m4.triangle_count(), 12);
    // Shaped cells count as solid.
    let mut s = CellGrid::new([4, 4, 4]);
    s.set(CellPos::new(0, 0, 0), Cell { shape: CellShape::new(content::cell::ShapeKind::Corner, 5), ..steel(0) });
    assert_eq!(mesh_lod(&input(&s, &[], &[]), Lod::L2).triangle_count(), 12);
}

#[test]
fn removed_cells_and_sealed_air_are_air() {
    let mut g = CellGrid::new([8, 8, 8]);
    g.set(CellPos::new(1, 1, 1), steel(0));
    let mut damage = vec![0u8; g.len()];
    damage[g.index(CellPos::new(1, 1, 1)).unwrap().index()] = 8;
    assert!(mesh_lod(&input(&g, &damage, &[]), Lod::L1).is_empty());
    let mut e = CellGrid::new([8, 8, 8]);
    e.set(CellPos::new(6, 6, 6), steel(0));
    let sealed = vec![u64::MAX; e.len().div_ceil(64)];
    let m = mesh_lod(&input(&e, &[], &sealed), Lod::L1);
    // Only the one solid block: all the sealed air is air.
    assert_eq!(m.triangle_count(), 12);
}

#[test]
fn swatch_is_the_exterior_majority_with_ties_to_the_lower_index() {
    // One 4^3 block at LOD2: the 56 surface cells vote, the 8 interior cells do not.
    let mut g = CellGrid::new([4, 4, 4]);
    fill(&mut g, [0; 3], [4; 3], steel(3));
    fill(&mut g, [1; 3], [3; 3], steel(2));
    let m = mesh_lod(&input(&g, &[], &[]), Lod::L2);
    assert!(m.colors.iter().all(|c| *c == [0.6, 0.6, 0.6, 1.0]));
    // Surface cells split 28 / 28 between swatches 3 and 1 (by x parity): the tie goes to 1.
    for x in 0..4 {
        fill(&mut g, [x, 0, 0], [x + 1, 4, 4], steel(if x % 2 == 0 { 3 } else { 1 }));
    }
    fill(&mut g, [1; 3], [3; 3], steel(2));
    let m = mesh_lod(&input(&g, &[], &[]), Lod::L2);
    assert!(m.colors.iter().all(|c| *c == [0.1, 0.2, 0.3, 1.0]));
    // A swatch index past the palette draws the fallback grey, not a panic.
    let mut h = CellGrid::new([2, 2, 2]);
    fill(&mut h, [0; 3], [2; 3], steel(12));
    let m = mesh_lod(&input(&h, &[], &[]), Lod::L1);
    assert!(m.colors.iter().all(|c| c[..3] == MISSING_SWATCH));
}

#[test]
fn concave_corners_darken_by_the_volume_ao_law() {
    assert_eq!(ao_factor(0), 1.0);
    assert!((ao_factor(2) - (1.0 - 0.45 * 0.665)).abs() < 1e-6);
    assert!((ao_factor(3) - (1.0 - 0.45 * 0.9975)).abs() < 1e-6);
    // A floor with a wall standing on it, at LOD1 (1 m cells).
    let mut g = CellGrid::new([8, 8, 8]);
    fill(&mut g, [0, 0, 0], [8, 2, 8], steel(0));
    fill(&mut g, [0, 2, 0], [2, 8, 8], steel(0));
    let m = mesh_lod(&input(&g, &[], &[]), Lod::L1);
    assert!(is_watertight(&m));
    assert_clockwise(&m);
    // Floor-top vertices at the foot of the wall (x = 1 m) are darker than those away from it.
    let up: Vec<(usize, [f32; 3])> = m.normals.iter().enumerate().filter(|(_, n)| **n == [0.0, 1.0, 0.0]).map(|(i, _)| (i, m.positions[i])).filter(|(_, p)| p[1] == 1.0).collect();
    let at_wall = up.iter().filter(|(_, p)| p[0] == 1.0 && p[2] > 0.0 && p[2] < 4.0).map(|(i, _)| m.colors[*i][0]).fold(f32::MAX, f32::min);
    let open = up.iter().filter(|(_, p)| p[0] >= 3.0).map(|(i, _)| m.colors[*i][0]).fold(f32::MIN, f32::max);
    assert!((at_wall - 0.5 * ao_factor(2)).abs() < 1e-6, "{at_wall}");
    assert!((open - 0.5).abs() < 1e-6);
}

fn random_grid(seed: u64, size: [u16; 3]) -> (CellGrid, Vec<u8>) {
    let mut r = Rng(seed);
    let mut g = CellGrid::new(size);
    let kinds = [content::cell::ShapeKind::Cube, content::cell::ShapeKind::Wedge, content::cell::ShapeKind::SlopeLow];
    for c in g.cells.iter_mut() {
        if r.below(100) < 45 {
            let k = kinds[r.below(3) as usize];
            *c = Cell { shape: CellShape::new(k, r.below(24) as u8), ..steel(r.below(4) as u8) };
        }
    }
    let damage = (0..g.len()).map(|_| if r.below(10) == 0 { 8 } else { 0 }).collect();
    (g, damage)
}

#[test]
fn lod_meshes_are_watertight_clockwise_and_deterministic() {
    for (seed, size) in [(1u64, [37u16, 21, 19]), (2, [50, 9, 33]), (3, [17, 30, 23])] {
        let (g, damage) = random_grid(seed, size);
        for lod in [Lod::L1, Lod::L2, Lod::L3, Lod::L4] {
            let inp = input(&g, &damage, &[]);
            let m = mesh_lod(&inp, lod);
            assert!(!m.is_empty());
            assert_eq!(m.normals.len(), m.positions.len());
            assert_eq!(m.colors.len(), m.positions.len());
            assert!(is_watertight(&m), "seed {seed} {lod:?}");
            assert_clockwise(&m);
            let b = m.aabb();
            for k in 0..3 {
                assert!(b.min[k] >= 0.0 && b.max[k] <= size[k] as f32 * 0.5);
            }
            assert!(m.colors.iter().all(|c| c[3] == 1.0 && c[..3].iter().all(|v| (0.0..=1.0).contains(v))));
            assert_eq!(m, mesh_lod(&inp, lod));
        }
    }
}

#[test]
fn lofted_hull_lods_are_watertight_and_shrink() {
    let mut g = CellGrid::new([150, 34, 41]);
    let inside = |x: f64, y: f64, z: f64| { let a = (z - 20.5) / 19.0; let b = (y - 26.0) / 24.0; a * a + b * b <= 1.0 && y <= 26.0 && x >= 2.0 && x <= 148.0 - (z - 20.5).abs() };
    for x in 0..150i16 {
        for y in 0..34i16 {
            for z in 0..41i16 {
                let mut mask = 0u8;
                for i in 0..8u8 {
                    if inside(x as f64 + (i & 1) as f64, y as f64 + ((i >> 1) & 1) as f64, z as f64 + ((i >> 2) & 1) as f64) {
                        mask |= 1 << i;
                    }
                }
                if let Some(s) = content::cell::shape_from_corners(mask) {
                    g.set(CellPos::new(x, y, z), Cell { shape: s, ..steel(if y > 20 { 1 } else { 2 }) });
                }
            }
        }
    }
    let inp = input(&g, &[], &[]);
    let mut prev = usize::MAX;
    for lod in [Lod::L1, Lod::L2, Lod::L3, Lod::L4] {
        let m = mesh_lod(&inp, lod);
        assert!(is_watertight(&m), "{lod:?}");
        assert_clockwise(&m);
        assert!(m.triangle_count() < prev, "{lod:?} {}", m.triangle_count());
        prev = m.triangle_count();
    }
}

#[test]
fn l0_and_bad_input_give_empty_meshes() {
    let mut g = CellGrid::new([4, 4, 4]);
    fill(&mut g, [0; 3], [4; 3], steel(0));
    assert!(mesh_lod(&input(&g, &[], &[]), Lod::L0).is_empty());
    let short = MeshInput { view: CellGridView { size: g.size, cells: &g.cells[..5], damage: &[], sealed_air: &[], scorch: &[] }, palette: &PALETTE, wear_seed: 0, design_waterline_y: 0 };
    assert!(mesh_lod(&short, Lod::L2).is_empty());
    let pal = [[0.5f32; 3]; 17];
    let big = MeshInput { palette: &pal, ..input(&g, &[], &[]) };
    assert!(mesh_lod(&big, Lod::L2).is_empty());
    let none = CellGrid::new([0, 4, 4]);
    assert!(mesh_lod(&input(&none, &[], &[]), Lod::L1).is_empty());
}

/// Timing helper: `cargo test --release -p hull-mesh -- --ignored bench_lod_and_mask --nocapture` prints the
/// cost of each LOD mesh and of one hull mask for a 90 m lofted hull (both run on the meshing thread).
#[test]
#[ignore]
fn bench_lod_and_mask() {
    let mut g = CellGrid::new([180, 40, 36]);
    let inside = |x: f64, y: f64, z: f64| { let a = (z - 18.0) / 17.0; let b = (y - 30.0) / 28.0; a * a + b * b <= 1.0 && y <= 30.0 && x >= 2.0 && x <= 178.0 - (z - 18.0).abs() };
    for x in 0..180i16 {
        for y in 0..40i16 {
            for z in 0..36i16 {
                let mut mask = 0u8;
                for i in 0..8u8 {
                    if inside(x as f64 + (i & 1) as f64, y as f64 + ((i >> 1) & 1) as f64, z as f64 + ((i >> 2) & 1) as f64) {
                        mask |= 1 << i;
                    }
                }
                if let Some(s) = content::cell::shape_from_corners(mask) {
                    g.set(CellPos::new(x, y, z), Cell { shape: s, ..steel(if y > 24 { 1 } else { 2 }) });
                }
            }
        }
    }
    let inp = MeshInput { design_waterline_y: 12, ..input(&g, &[], &[]) };
    for lod in [Lod::L1, Lod::L2, Lod::L3, Lod::L4] {
        let t = std::time::Instant::now();
        let m = mesh_lod(&inp, lod);
        eprintln!("{lod:?}: {:?}, {} triangles, {} vertices", t.elapsed(), m.triangle_count(), m.vertex_count());
    }
    let t = std::time::Instant::now();
    let _ = crate::mask::build_hull_mask(&inp);
    eprintln!("mask: {:?}", t.elapsed());
}
