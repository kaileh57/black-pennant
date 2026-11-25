use super::*;
use crate::chunk::{all_chunks, fill_class_buffer, MeshInput};
use crate::greedy::tests::{append, assert_clockwise, Rng};
use crate::greedy::{fix_t_junctions, GreedyMesher};
use crate::surface::is_watertight;
use content::cell::{armour_normal, shape_corner_mask, ArmourTier, Cell, CellGrid, CellGridView, CellPos, Material, RenderChunkCoord};

const KINDS: [ShapeKind; 6] = [ShapeKind::Cube, ShapeKind::Wedge, ShapeKind::Corner, ShapeKind::InverseCorner, ShapeKind::SlopeLow, ShapeKind::SlopeHigh];

fn cell(shape: CellShape) -> Cell { Cell { material: Material::Steel, armour: ArmourTier::None, paint: 0, shape } }

/// The full LOD0 path per chunk (class buffer, greedy, shaped, T-junction fix), concatenated over the grid.
fn mesh_grid(g: &CellGrid, sealed: &[u64]) -> MeshData {
    let inp = MeshInput { view: CellGridView { size: g.size, cells: &g.cells, damage: &[], sealed_air: sealed, scorch: &[] }, palette: &[], wear_seed: 0, design_waterline_y: 0 };
    let table = ShapeFaceTable::build();
    let mut gm = GreedyMesher::new();
    let mut buf = ClassBuffer::new(RenderChunkCoord::default());
    let mut all = MeshData::new();
    for c in all_chunks(g.size) {
        fill_class_buffer(&inp, c, &mut buf).unwrap();
        let mut m = MeshData::new();
        gm.mesh_cubes(&buf, &mut m);
        mesh_shaped(&table, &buf, &mut m);
        fix_t_junctions(&mut m);
        append(&mut all, &m);
    }
    all
}

fn poly_area(c: &[[f32; 3]]) -> f64 {
    let mut n = [0.0f64; 3];
    for i in 0..c.len() {
        let (a, b) = (c[i].map(|v| v as f64), c[(i + 1) % c.len()].map(|v| v as f64));
        n[0] += a[1] * b[2] - a[2] * b[1];
        n[1] += a[2] * b[0] - a[0] * b[2];
        n[2] += a[0] * b[1] - a[1] * b[0];
    }
    0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()
}

#[test]
fn table_matches_content_shapes_in_every_orientation() {
    let t = ShapeFaceTable::build();
    assert_eq!(t.polygons.len(), 256);
    for kind in KINDS {
        for o in 0..24u8 {
            let s = CellShape::new(kind, o);
            let polys = t.polygons(s);
            // Full faces: area-1 polygons on exactly the faces content's mask names.
            let mut mask = 0u8;
            let mut corners = 0u8;
            for p in polys {
                assert!(p.corners.len() >= 3 && p.corners.len() <= 5, "{kind:?} {o}");
                for c in &p.corners {
                    for v in c {
                        assert!(*v == 0.0 || *v == 0.5 || *v == 1.0, "{kind:?} {o}: {c:?}");
                    }
                    if c.iter().all(|v| *v != 0.5) {
                        corners |= 1 << ((c[0] as u8) | ((c[1] as u8) << 1) | ((c[2] as u8) << 2));
                    }
                }
                // Clockwise about the stated normal.
                let (a, b, c) = (p.corners[0], p.corners[1], p.corners[2]);
                let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                let cr = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
                assert!(cr[0] * p.normal[0] + cr[1] * p.normal[1] + cr[2] * p.normal[2] < 0.0, "{kind:?} {o} winding");
                if let Some(f) = p.on_face {
                    if (poly_area(&p.corners) - 1.0).abs() < 1e-9 {
                        mask |= 1 << (f as u8);
                    }
                } else {
                    // The slope normal is the one content uses for armour obliquity.
                    let n = p.normal.map(|v| v as f64);
                    let an = armour_normal(s, [-n[0], -n[1], -n[2]]);
                    for k in 0..3 {
                        assert!((an[k] - n[k]).abs() < 1e-6, "{kind:?} {o}: {n:?} vs {an:?}");
                    }
                }
            }
            assert_eq!(mask, full_face_mask(s), "{kind:?} {o}");
            assert_eq!(t.full_mask[s.0 as usize], full_face_mask(s));
            assert_eq!(corners, shape_corner_mask(s), "{kind:?} {o}");
        }
    }
    // Volumes from the polygons (divergence theorem) match the solids: 1, 1/2, 1/6, 5/6, 1/4, 3/4.
    for (kind, vol) in [(ShapeKind::Cube, 1.0), (ShapeKind::Wedge, 0.5), (ShapeKind::Corner, 1.0 / 6.0), (ShapeKind::InverseCorner, 5.0 / 6.0), (ShapeKind::SlopeLow, 0.25), (ShapeKind::SlopeHigh, 0.75)] {
        for o in [0u8, 7, 13, 23] {
            let mut v = 0.0;
            for p in t.polygons(CellShape::new(kind, o)) {
                let c0 = p.corners[0].map(|x| x as f64);
                for k in 1..p.corners.len() - 1 {
                    let (b, c) = (p.corners[k].map(|x| x as f64), p.corners[k + 1].map(|x| x as f64));
                    // Clockwise winding: the signed volume is negated.
                    v -= (c0[0] * (b[1] * c[2] - b[2] * c[1]) - c0[1] * (b[0] * c[2] - b[2] * c[0]) + c0[2] * (b[0] * c[1] - b[1] * c[0])) / 6.0;
                }
            }
            assert!((v - vol).abs() < 1e-9, "{kind:?} {o}: {v}");
        }
    }
}

#[test]
fn single_shaped_cells_are_closed() {
    for kind in KINDS {
        for o in 0..24u8 {
            let mut g = CellGrid::new([3, 3, 3]);
            g.set(CellPos::new(1, 1, 1), cell(CellShape::new(kind, o)));
            let m = mesh_grid(&g, &[]);
            assert_clockwise(&m);
            assert!(is_watertight(&m), "{kind:?} {o}");
        }
    }
}

#[test]
fn matching_faces_cancel() {
    // A run of identical wedges along z: the shared triangles vanish, leaving 2 end triangles, 1 slope strip
    // and 2 full strips (greedy and shaped faces are per cell here).
    let mut g = CellGrid::new([1, 1, 4]);
    for z in 0..4 {
        g.set(CellPos::new(0, 0, z), cell(CellShape::new(ShapeKind::Wedge, 0)));
    }
    let m = mesh_grid(&g, &[]);
    assert!(is_watertight(&m));
    let tri_faces = m.normals.iter().filter(|n| n[2] != 0.0).count();
    assert_eq!(tri_faces, 6, "only the two end triangles face along z");
    // A cube against a wedge's full face emits nothing between them; against its triangle, the other half.
    let mut g = CellGrid::new([2, 1, 2]);
    g.set(CellPos::new(0, 0, 0), cell(CellShape::new(ShapeKind::Wedge, 0)));
    g.set(CellPos::new(0, 0, 1), cell(CellShape::CUBE));
    g.set(CellPos::new(1, 0, 0), cell(CellShape::CUBE));
    let m = mesh_grid(&g, &[]);
    assert_clockwise(&m);
    assert!(is_watertight(&m));
}

fn random_shape(rng: &mut Rng) -> CellShape {
    let kind = KINDS[rng.below(6) as usize];
    CellShape::new(kind, rng.below(24) as u8)
}

#[test]
fn random_shapes_across_chunk_seams_are_watertight() {
    for seed in 0..4u64 {
        let mut rng = Rng(seed * 7 + 1);
        let mut g = CellGrid::new([42, 40, 42]);
        let density = 40 + rng.below(40);
        for x in 26..40 {
            for y in 26..38 {
                for z in 26..40 {
                    if rng.below(100) < density {
                        let shape = if rng.below(3) == 0 { CellShape::CUBE } else { random_shape(&mut rng) };
                        let material = if rng.below(8) == 0 { Material::Glass } else { Material::Steel };
                        g.set(CellPos::new(x, y, z), Cell { material, ..cell(shape) });
                    }
                }
            }
        }
        let m = mesh_grid(&g, &[]);
        assert_clockwise(&m);
        assert!(is_watertight(&m), "seed {seed}");
        assert_eq!(m, mesh_grid(&g, &[]), "deterministic");
    }
}

#[test]
fn every_shape_pair_is_watertight() {
    // Every ordered pair of the 121 distinct shape bytes (cube plus 5 kinds x 24 orientations) adjacent along X,
    // and every pair of a fixed third of them (all kinds, 8 orientations each) along Y and Z. Pairs are packed
    // into one grid per axis with an air gap between them, so the disjoint closed surfaces must sum to a closed
    // one; some pairs straddle a 16 m seam.
    let all: Vec<CellShape> = std::iter::once(CellShape::CUBE).chain(KINDS[1..].iter().flat_map(|k| (0..24u8).map(move |o| CellShape::new(*k, o)))).collect();
    for axis in 0..3 {
        let shapes: Vec<CellShape> = all.iter().enumerate().filter(|(i, _)| axis == 0 || i % 3 == axis % 3).map(|(_, s)| *s).collect();
        let n = shapes.len();
        let (b, c) = ((axis + 1) % 3, (axis + 2) % 3);
        let mut size = [1u16; 3];
        size[axis] = 3 * n as u16;
        size[b] = 2 * n as u16;
        let mut g = CellGrid::new(size);
        let at = |i: usize, j: usize, second: bool| {
            let mut p = [0i16; 3];
            p[axis] = (3 * i + second as usize) as i16;
            p[b] = (2 * j) as i16;
            p[c] = 0;
            CellPos::new(p[0], p[1], p[2])
        };
        for i in 0..n {
            for j in 0..n {
                g.set(at(i, j, false), cell(shapes[i]));
                g.set(at(i, j, true), cell(shapes[j]));
            }
        }
        let m = mesh_grid(&g, &[]);
        if !is_watertight(&m) {
            // Find the culprit pair for the report.
            for i in 0..n {
                for j in 0..n {
                    let mut size = [1u16; 3];
                    size[axis] = 2;
                    let mut h = CellGrid::new(size);
                    let mut p = [0i16; 3];
                    h.set(CellPos::new(p[0], p[1], p[2]), cell(shapes[i]));
                    p[axis] = 1;
                    h.set(CellPos::new(p[0], p[1], p[2]), cell(shapes[j]));
                    assert!(is_watertight(&mesh_grid(&h, &[])), "{:?} then {:?} along axis {axis}", shapes[i], shapes[j]);
                }
            }
            panic!("axis {axis}: open surface only across a seam");
        }
        assert_clockwise(&m);
    }
}

#[test]
fn slope_against_seam_with_cubes() {
    // A 1:2 slope (high then low) ends at the x = 32 seam, cubes continue the deck below it on both sides.
    let mut g = CellGrid::new([36, 4, 3]);
    for z in 0..3 {
        for x in 28..36 {
            g.set(CellPos::new(x, 0, z), cell(CellShape::CUBE));
        }
        g.set(CellPos::new(30, 1, z), cell(CellShape::new(ShapeKind::SlopeHigh, 0)));
        g.set(CellPos::new(31, 1, z), cell(CellShape::new(ShapeKind::SlopeLow, 0)));
        g.set(CellPos::new(32, 1, z), cell(CellShape::new(ShapeKind::SlopeLow, 0)));
        g.set(CellPos::new(33, 1, z), cell(CellShape::CUBE));
    }
    // A lone slope cell beside the seam, its half-height vertex on the seam line.
    g.set(CellPos::new(32, 2, 1), cell(CellShape::new(ShapeKind::SlopeLow, 0)));
    let m = mesh_grid(&g, &[]);
    assert_clockwise(&m);
    assert!(is_watertight(&m));
}

#[test]
fn shaped_cell_open_to_sealed_air_is_closed() {
    // A closed steel box with a sealed interior; one wall cell is a wedge whose open corner faces the inside.
    let n = 6i16;
    let mut g = CellGrid::new([n as u16; 3]);
    for x in 0..n {
        for y in 0..n {
            for z in 0..n {
                if x == 0 || y == 0 || z == 0 || x == n - 1 || y == n - 1 || z == n - 1 {
                    g.set(CellPos::new(x, y, z), cell(CellShape::CUBE));
                }
            }
        }
    }
    // Wedge orientation 0 fills x + y <= 1: its open side faces +X +Y, into the box from the x = 0 wall.
    g.set(CellPos::new(0, 2, 2), cell(CellShape::new(ShapeKind::Wedge, 0)));
    let mut sealed = vec![0u64; g.len().div_ceil(64)];
    for (i, c) in g.cells.iter().enumerate() {
        if c.is_empty() {
            sealed[i / 64] |= 1 << (i % 64);
        }
    }
    let m = mesh_grid(&g, &sealed);
    assert_clockwise(&m);
    assert!(is_watertight(&m));
    // Everything is inside the 3 m box.
    let b = m.aabb();
    assert_eq!(b.min, [0.0; 3]);
    assert_eq!(b.max, [3.0; 3]);
}

/// Triangle area summed per exact normal: equal for two meshes that cover the same surface, however they
/// triangulate it.
pub(crate) fn area_by_normal(m: &MeshData) -> std::collections::BTreeMap<[u32; 3], f64> {
    let mut out = std::collections::BTreeMap::new();
    for t in m.indices.chunks_exact(3) {
        let n = m.normals[t[0] as usize].map(|v| (v + 0.0).to_bits());
        let c = [m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]];
        *out.entry(n).or_insert(0.0) += poly_area(&c);
    }
    out
}

pub(crate) fn assert_same_area(a: &MeshData, b: &MeshData, what: &str) {
    let (x, y) = (area_by_normal(a), area_by_normal(b));
    assert_eq!(x.keys().collect::<Vec<_>>(), y.keys().collect::<Vec<_>>(), "{what}: normals");
    for (k, v) in &x {
        assert!((v - y[k]).abs() < 1e-6, "{what}: area {v} vs {} for normal {k:?}", y[k]);
    }
}

#[test]
fn pair_table_matches_per_cell_clipping() {
    let table = ShapeFaceTable::build();
    assert!(table.region_list.len() < 64, "{} regions", table.region_list.len());
    for seed in 0..3u64 {
        let mut rng = Rng(seed * 13 + 5);
        let mut g = CellGrid::new([40, 36, 40]);
        for x in 24..38 {
            for y in 24..36 {
                for z in 24..38 {
                    if rng.below(100) < 60 {
                        let shape = if rng.below(4) == 0 { CellShape::CUBE } else { random_shape(&mut rng) };
                        g.set(CellPos::new(x, y, z), cell(shape));
                    }
                }
            }
        }
        let sealed = vec![u64::MAX; g.len().div_ceil(64)];
        for sealed in [&[][..], &sealed[..]] {
            let inp = MeshInput { view: CellGridView { size: g.size, cells: &g.cells, damage: &[], sealed_air: sealed, scorch: &[] }, palette: &[], wear_seed: 0, design_waterline_y: 0 };
            let mut buf = ClassBuffer::new(RenderChunkCoord::default());
            for c in all_chunks(g.size) {
                fill_class_buffer(&inp, c, &mut buf).unwrap();
                let (mut fast, mut slow) = (MeshData::new(), MeshData::new());
                mesh_shaped(&table, &buf, &mut fast);
                mesh_shaped_reference(&table, &buf, &mut slow);
                assert_clockwise(&fast);
                assert_eq!(fast.positions.len() == 0, slow.positions.len() == 0);
                assert_same_area(&fast, &slow, &format!("seed {seed} chunk {c:?}"));
            }
        }
    }
}

#[test]
fn t_junction_fast_path_matches_general_index_on_shaped_chunks() {
    let table = ShapeFaceTable::build();
    let mut gm = GreedyMesher::new();
    let mut saw_off_lattice = false;
    for seed in 0..3u64 {
        let mut rng = Rng(seed * 31 + 3);
        let mut g = CellGrid::new([44, 40, 44]);
        for x in 24..42 {
            for y in 24..38 {
                for z in 24..42 {
                    if rng.below(100) < 55 {
                        let shape = if rng.below(3) == 0 { CellShape::CUBE } else { random_shape(&mut rng) };
                        g.set(CellPos::new(x, y, z), cell(shape));
                    }
                }
            }
        }
        let inp = MeshInput { view: CellGridView { size: g.size, cells: &g.cells, damage: &[], sealed_air: &[], scorch: &[] }, palette: &[], wear_seed: 0, design_waterline_y: 0 };
        let mut buf = ClassBuffer::new(RenderChunkCoord::default());
        for c in all_chunks(g.size) {
            fill_class_buffer(&inp, c, &mut buf).unwrap();
            let mut raw = MeshData::new();
            gm.mesh_cubes(&buf, &mut raw);
            mesh_shaped(&table, &buf, &mut raw);
            assert!(crate::greedy::takes_fast_path(&raw), "chunk meshes take the lattice path");
            saw_off_lattice |= raw.positions.iter().flatten().any(|c| (c * 4.0).fract() != 0.0);
            assert_eq!(crate::greedy::vertices_inside_shared_edges(&raw), 0, "seed {seed} chunk {c:?}");
            let (mut fast, mut general) = (raw.clone(), raw.clone());
            fix_t_junctions(&mut fast);
            crate::greedy::fix_with_general_index(&mut general);
            assert_eq!(fast, general, "seed {seed} chunk {c:?}");
            assert_same_area(&raw, &fast, "split keeps the surface");
        }
    }
    assert!(saw_off_lattice, "the side list (cut-line crossings) is exercised");
}

/// A lofted hull section: the corner-pattern voxelizer puts cubes, wedges, corners and inverse corners on the
/// boundary of an elliptic cross-section (the shape mix of a real hull).
fn lofted_section(size: [u16; 3]) -> CellGrid {
    let mut g = CellGrid::new(size);
    let (sx, sy, sz) = (size[0] as f64, size[1] as f64, size[2] as f64);
    let inside = |x: f64, y: f64, z: f64| {
        let a = (z - sz / 2.0) / (sz / 2.0 - 2.0);
        let b = (y - sy * 0.62) / (sy * 0.5);
        a * a + b * b <= 1.0 && y <= sy * 0.8 && x >= 2.0 && x <= sx - 2.0
    };
    for x in 0..size[0] as i16 {
        for y in 0..size[1] as i16 {
            for z in 0..size[2] as i16 {
                let mut mask = 0u8;
                for i in 0..8u8 {
                    if inside(x as f64 + (i & 1) as f64, y as f64 + ((i >> 1) & 1) as f64, z as f64 + ((i >> 2) & 1) as f64) {
                        mask |= 1 << i;
                    }
                }
                if let Some(s) = content::cell::shape_from_corners(mask) {
                    g.set(CellPos::new(x, y, z), cell(s));
                }
            }
        }
    }
    g
}

#[test]
fn t_junction_coarse_path_matches_general_index_on_a_lofted_hull() {
    let g = lofted_section([40, 36, 40]);
    let inp = MeshInput { view: CellGridView { size: g.size, cells: &g.cells, damage: &[], sealed_air: &[], scorch: &[] }, palette: &[], wear_seed: 0, design_waterline_y: 0 };
    let table = ShapeFaceTable::build();
    let mut gm = GreedyMesher::new();
    let mut buf = ClassBuffer::new(RenderChunkCoord::default());
    let mut all = MeshData::new();
    for c in all_chunks(g.size) {
        fill_class_buffer(&inp, c, &mut buf).unwrap();
        let mut raw = MeshData::new();
        gm.mesh_cubes(&buf, &mut raw);
        mesh_shaped(&table, &buf, &mut raw);
        if raw.is_empty() {
            continue;
        }
        assert!(crate::greedy::takes_coarse_path(&raw), "wedges and corners keep every vertex on the cell grid");
        assert_eq!(crate::greedy::vertices_inside_shared_edges(&raw), 0);
        let (mut fast, mut general) = (raw.clone(), raw.clone());
        fix_t_junctions(&mut fast);
        crate::greedy::fix_with_general_index(&mut general);
        assert_eq!(fast, general, "chunk {c:?}");
        append(&mut all, &fast);
    }
    assert!(is_watertight(&all));
}

#[test]
fn t_junction_fix_on_a_mesh_wider_than_the_lattice_box() {
    // Two shaped grids 40 m apart in one mesh: the general index takes it and the result stays closed.
    let mut g = CellGrid::new([100, 4, 4]);
    let mut rng = Rng(77);
    for x in (0..6).chain(90..96) {
        for y in 0..3 {
            for z in 0..3 {
                let shape = if rng.below(2) == 0 { CellShape::CUBE } else { random_shape(&mut rng) };
                g.set(CellPos::new(x, y, z), cell(shape));
            }
        }
    }
    let inp = MeshInput { view: CellGridView { size: g.size, cells: &g.cells, damage: &[], sealed_air: &[], scorch: &[] }, palette: &[], wear_seed: 0, design_waterline_y: 0 };
    let table = ShapeFaceTable::build();
    let mut gm = GreedyMesher::new();
    let mut buf = ClassBuffer::new(RenderChunkCoord::default());
    let mut all = MeshData::new();
    for c in all_chunks(g.size) {
        fill_class_buffer(&inp, c, &mut buf).unwrap();
        let mut m = MeshData::new();
        gm.mesh_cubes(&buf, &mut m);
        mesh_shaped(&table, &buf, &mut m);
        append(&mut all, &m);
    }
    assert!(!crate::greedy::takes_fast_path(&all));
    let raw = all.clone();
    fix_t_junctions(&mut all);
    assert!(is_watertight(&all));
    assert_same_area(&raw, &all, "wide mesh");
}

#[test]
fn no_shaped_cells_is_a_no_op() {
    let mut buf = ClassBuffer::new(RenderChunkCoord::default());
    buf.voxels[ClassBuffer::pad_index(0, 0, 0)] = class::SHAPED_BASE | 0x20;
    buf.opaque_mask[1 + CHUNK_P] |= 1 << 1;
    let mut m = MeshData::new();
    mesh_shaped(&ShapeFaceTable::build(), &buf, &mut m);
    assert!(m.is_empty(), "has_shaped false");
    buf.has_shaped = true;
    mesh_shaped(&ShapeFaceTable::build(), &buf, &mut m);
    assert!(!m.is_empty());
}


/// Spike S5 helper: `cargo test --release -p hull-mesh -- --ignored bench_hull_chunk --nocapture` prints per-chunk
/// timings of each LOD0 stage on a voxelised hull section (budget 200 us per chunk on a minimum-spec core).
#[test]
#[ignore]
fn bench_hull_chunk() {
    // Hull-ish: an elliptic section 32 long, voxelised with shapes on the boundary from corner patterns.
    let mut g = CellGrid::new([64, 32, 40]);
    let inside = |x: f64, y: f64, z: f64| { let a = (z - 20.0) / 18.0; let b = (y - 20.0) / 16.0; a * a + b * b <= 1.0 && y <= 26.0 && x >= 2.0 && x <= 62.0 };
    for x in 0..64i16 { for y in 0..32i16 { for z in 0..40i16 {
        let mut mask = 0u8;
        for i in 0..8u8 { if inside(x as f64 + (i & 1) as f64, y as f64 + ((i >> 1) & 1) as f64, z as f64 + ((i >> 2) & 1) as f64) { mask |= 1 << i; } }
        if let Some(s) = content::cell::shape_from_corners(mask) { g.set(CellPos::new(x, y, z), cell(s)); }
    }}}
    let sealed = vec![0u64; g.len().div_ceil(64)];
    let inp = MeshInput { view: CellGridView { size: g.size, cells: &g.cells, damage: &[], sealed_air: &sealed, scorch: &[] }, palette: &[], wear_seed: 0, design_waterline_y: 0 };
    let table = ShapeFaceTable::build();
    let mut gm = GreedyMesher::new();
    let mut buf = ClassBuffer::new(RenderChunkCoord::default());
    let chunks = all_chunks(g.size);
    // Per chunk and stage, the fastest of the repetitions (machine noise only ever adds time), plus the p95 of
    // every whole-chunk sample.
    let mut best = vec![[u128::MAX; 4]; chunks.len()];
    let mut m = MeshData::new();
    let mut totals = Vec::new();
    let mut tris = 0;
    let mut all = MeshData::new();
    for rep in 0..40 {
        all.clear();
        for (ci, c) in chunks.iter().enumerate() {
            let a = std::time::Instant::now();
            fill_class_buffer(&inp, *c, &mut buf).unwrap();
            let b = std::time::Instant::now();
            // Reused across chunks, as `ChunkMesher::scratch` is.
            m.clear();
            gm.mesh_cubes(&buf, &mut m);
            let c2 = std::time::Instant::now();
            mesh_shaped(&table, &buf, &mut m);
            let d = std::time::Instant::now();
            fix_t_junctions(&mut m);
            let e = std::time::Instant::now();
            for (k, t) in [(0, b - a), (1, c2 - b), (2, d - c2), (3, e - d)] {
                best[ci][k] = best[ci][k].min(t.as_nanos());
            }
            totals.push((e - a).as_nanos());
            if rep == 0 {
                tris += m.triangle_count();
            }
            append(&mut all, &m);
        }
    }
    let vol = std::time::Instant::now();
    for c in &chunks {
        let _ = crate::chunk::build_volume(&inp, *c);
    }
    let vt = vol.elapsed();
    assert!(is_watertight(&all));
    totals.sort_unstable();
    let n = chunks.len() as u128;
    let stage = |k: usize| best.iter().map(|b| b[k]).sum::<u128>() / n / 1000;
    let heaviest = best.iter().map(|b| b.iter().sum::<u128>()).max().unwrap_or(0) / 1000;
    eprintln!(
        "per chunk best: fill {} us greedy {} us shaped {} us tj {} us; heaviest chunk best {} us; whole chunk p50 {} us p95 {} us; tris/chunk {}; volume/chunk {:?}",
        stage(0), stage(1), stage(2), stage(3), heaviest, totals[totals.len() / 2] / 1000, totals[totals.len() * 95 / 100] / 1000, tris / chunks.len(), vt / chunks.len() as u32
    );
}
