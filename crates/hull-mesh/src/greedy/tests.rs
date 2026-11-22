use super::*;
use crate::chunk::{all_chunks, fill_class_buffer, MeshInput};
use crate::surface::is_watertight;
use content::cell::{ArmourTier, Cell, CellGrid, CellGridView, CellPos, CellShape, Material, RenderChunkCoord};
use std::collections::BTreeSet;

pub(crate) struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 { self.next() % n }
}

fn steel() -> Cell { Cell { material: Material::Steel, armour: ArmourTier::None, paint: 0, shape: CellShape::CUBE } }

fn view(g: &CellGrid) -> MeshInput<'_> {
    MeshInput { view: CellGridView { size: g.size, cells: &g.cells, damage: &[], sealed_air: &[], scorch: &[] }, palette: &[], wear_seed: 0, design_waterline_y: 0 }
}

/// Every triangle winds clockwise about its vertex normal and has positive area.
pub(crate) fn assert_clockwise(m: &MeshData) {
    for t in m.indices.chunks_exact(3) {
        let p = |i: u32| m.positions[i as usize];
        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
        let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let cr = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
        let n = m.normals[t[0] as usize];
        let d = cr[0] * n[0] + cr[1] * n[1] + cr[2] * n[2];
        let len = (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]).sqrt();
        assert!(len > 1e-7, "zero-area triangle {a:?} {b:?} {c:?}");
        assert!(d < 0.0 && (-d / len) > 0.999, "triangle {a:?} {b:?} {c:?} not clockwise about {n:?}");
    }
}

pub(crate) fn append(all: &mut MeshData, m: &MeshData) {
    let base = all.positions.len() as u32;
    all.positions.extend_from_slice(&m.positions);
    all.normals.extend_from_slice(&m.normals);
    all.indices.extend(m.indices.iter().map(|i| i + base));
}

/// `mesh_cube_grid(g, true)` through the general point index.
fn mesh_cube_grid_general(g: &CellGrid) -> MeshData {
    let inp = view(g);
    let mut gm = GreedyMesher::new();
    let mut buf = ClassBuffer::new(RenderChunkCoord::default());
    let mut all = MeshData::new();
    for c in all_chunks(g.size) {
        fill_class_buffer(&inp, c, &mut buf).unwrap();
        let mut m = MeshData::new();
        gm.mesh_cubes(&buf, &mut m);
        assert!(super::takes_coarse_path(&m) || m.is_empty());
        super::fix_with_general_index(&mut m);
        append(&mut all, &m);
    }
    all
}

/// Cubes only: mesh every chunk, fix T-junctions per chunk, concatenate.
fn mesh_cube_grid(g: &CellGrid, fix: bool) -> MeshData {
    let inp = view(g);
    let mut gm = GreedyMesher::new();
    let mut buf = ClassBuffer::new(RenderChunkCoord::default());
    let mut all = MeshData::new();
    for c in all_chunks(g.size) {
        fill_class_buffer(&inp, c, &mut buf).unwrap();
        let mut m = MeshData::new();
        gm.mesh_cubes(&buf, &mut m);
        if fix {
            fix_t_junctions(&mut m);
        }
        append(&mut all, &m);
    }
    all
}

#[test]
fn bgm_faces_map_to_content_frame() {
    let mut g = CellGrid::new([3, 3, 3]);
    g.set(CellPos::new(1, 1, 1), steel());
    let m = mesh_cube_grid(&g, false);
    assert_eq!(m.triangle_count(), 12);
    assert_clockwise(&m);
    // Each face sits on the plane its normal names: +X face at x = 1.0 m, -X at 0.5 m, and so on.
    for (i, p) in m.positions.iter().enumerate() {
        let n = m.normals[i];
        for k in 0..3 {
            if n[k] != 0.0 {
                assert_eq!(p[k], if n[k] > 0.0 { 1.0 } else { 0.5 }, "normal {n:?} at {p:?}");
            }
        }
    }
    let normals: BTreeSet<[i32; 3]> = m.normals.iter().map(|n| [n[0] as i32, n[1] as i32, n[2] as i32]).collect();
    assert_eq!(normals.len(), 6);
    assert!(is_watertight(&m));
}

#[test]
fn solid_block_is_six_quads() {
    let mut g = CellGrid::new([10, 10, 10]);
    for x in 1..9 {
        for y in 1..9 {
            for z in 1..9 {
                g.set(CellPos::new(x, y, z), steel());
            }
        }
    }
    let m = mesh_cube_grid(&g, true);
    assert_eq!(m.triangle_count(), 12, "8^3 block: 3,072 faces greedy to 6 quads (L1921)");
    assert!(is_watertight(&m));
    let b = m.aabb();
    assert_eq!(b.min, [0.5; 3]);
    assert_eq!(b.max, [4.5; 3]);
}

/// Unit faces (face index, cell) a naive per-face mesher emits for one class buffer.
fn naive_faces(buf: &ClassBuffer) -> BTreeSet<(usize, [i32; 3])> {
    let mut out = BTreeSet::new();
    for x in 0..32 {
        for y in 0..32 {
            for z in 0..32 {
                let v = buf.voxels[ClassBuffer::pad_index(x, y, z)];
                if v == 0 || v == class::SEALED_AIR || class::is_shaped(v) {
                    continue;
                }
                for (k, f) in Face::ALL.iter().enumerate() {
                    let (dx, dy, dz) = f.offset();
                    if buf.voxels[ClassBuffer::pad_index(x + dx as i32, y + dy as i32, z + dz as i32)] == 0 {
                        out.insert((k, [x, y, z]));
                    }
                }
            }
        }
    }
    out
}

/// Rasterise mesh_cubes output back to unit faces; panics on overlap.
fn greedy_faces(m: &MeshData, origin: [f32; 3]) -> BTreeSet<(usize, [i32; 3])> {
    let mut out = BTreeSet::new();
    for q in 0..m.positions.len() / 4 {
        let ps = &m.positions[q * 4..q * 4 + 4];
        let n = m.normals[q * 4];
        let f = face_of_normal([n[0] as i32, n[1] as i32, n[2] as i32]).unwrap();
        let k = Face::ALL.iter().position(|g| *g == f).unwrap();
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for p in ps {
            for a in 0..3 {
                lo[a] = lo[a].min((p[a] - origin[a]) * 2.0);
                hi[a] = hi[a].max((p[a] - origin[a]) * 2.0);
            }
        }
        let a = f as usize / 2;
        let mut r = [(0i32, 0i32); 3];
        for b in 0..3 {
            r[b] = if b == a {
                let plane = lo[b] as i32;
                let cell = if f as usize % 2 == 1 { plane - 1 } else { plane };
                (cell, cell + 1)
            } else {
                (lo[b] as i32, hi[b] as i32)
            };
        }
        for x in r[0].0..r[0].1 {
            for y in r[1].0..r[1].1 {
                for z in r[2].0..r[2].1 {
                    assert!(out.insert((k, [x, y, z])), "overlapping quads");
                }
            }
        }
    }
    out
}

#[test]
fn greedy_matches_naive_mesher_on_32_random_chunks() {
    let mut rng = Rng(0xB1AC);
    let mut gm = GreedyMesher::new();
    for i in 0..32 {
        let coord = RenderChunkCoord { x: (i % 3) as u8, y: (i / 3 % 2) as u8, z: (i % 5) as u8 };
        let mut buf = ClassBuffer::new(coord);
        let density = 1 + rng.below(9);
        for (j, v) in buf.voxels.iter_mut().enumerate() {
            let r = rng.below(10);
            *v = if r >= density {
                0
            } else {
                match rng.below(8) {
                    0 => class::GLASS,
                    1 => class::SEALED_AIR,
                    2 => class::SHAPED_BASE | (0x20 + (j % 24) as u16),
                    3 => class::EMISSIVE,
                    _ => class::OPAQUE,
                }
            };
        }
        // Some chunks are mostly solid with carved holes to exercise large merges.
        if i % 4 == 0 {
            for (j, v) in buf.voxels.iter_mut().enumerate() {
                *v = if (j * 7919) % 97 == 0 { 0 } else { class::OPAQUE };
            }
        }
        for py in 0..34 {
            for px in 0..34 {
                let mut col = 0u64;
                for pz in 0..34 {
                    if buf.voxels[pz + 34 * (px + 34 * py)] != 0 {
                        col |= 1 << pz;
                    }
                }
                buf.opaque_mask[px + 34 * py] = col;
            }
        }
        buf.empty = false;
        let mut m = MeshData::new();
        gm.mesh_cubes(&buf, &mut m);
        assert_clockwise(&m);
        let origin = [coord.x as f32 * 16.0, coord.y as f32 * 16.0, coord.z as f32 * 16.0];
        assert_eq!(greedy_faces(&m, origin), naive_faces(&buf), "chunk {i}");
    }
}

fn random_cube_grid(seed: u64, size: [u16; 3], lo: i16, hi: i16) -> CellGrid {
    let mut rng = Rng(seed);
    let mut g = CellGrid::new(size);
    for x in lo..hi.min(size[0] as i16) {
        for y in lo..hi.min(size[1] as i16) {
            for z in lo..hi.min(size[2] as i16) {
                if rng.below(100) < 55 {
                    let mat = if rng.below(6) == 0 { Material::Glass } else { Material::Steel };
                    g.set(CellPos::new(x, y, z), Cell { material: mat, ..steel() });
                }
            }
        }
    }
    g
}

#[test]
fn t_junction_fix_makes_random_multi_chunk_grids_watertight() {
    let mut saw_crack = false;
    for seed in 0..4 {
        // Cells 24..44 straddle the 16 m seams on every axis.
        let g = random_cube_grid(seed, [44, 44, 44], 24, 44);
        let raw = mesh_cube_grid(&g, false);
        saw_crack |= !is_watertight(&raw);
        let m = mesh_cube_grid(&g, true);
        assert_clockwise(&m);
        assert!(is_watertight(&m), "seed {seed}");
        assert_eq!(m, mesh_cube_grid_general(&g), "lattice path equals the general index, seed {seed}");
        // Same area before and after.
        let area = |m: &MeshData| -> f64 {
            m.indices.chunks_exact(3).map(|t| {
                let p = |i: u32| m.positions[i as usize].map(|v| v as f64);
                let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                let cr = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
                0.5 * (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]).sqrt()
            }).sum()
        };
        assert!((area(&raw) - area(&m)).abs() < 1e-6);
    }
    assert!(saw_crack, "greedy output should have had T-junctions to fix");
}

#[test]
fn t_junction_fix_is_deterministic_and_idempotent() {
    let g = random_cube_grid(9, [36, 12, 12], 26, 36);
    let a = mesh_cube_grid(&g, true);
    let b = mesh_cube_grid(&g, true);
    assert_eq!(a, b);
    let mut c = a.clone();
    fix_t_junctions(&mut c);
    assert_eq!(c.triangle_count(), a.triangle_count());
    // Bad input does not panic.
    let mut bad = MeshData { positions: vec![[0.0; 3], [f32::NAN, 0.0, 0.0]], normals: vec![], colors: vec![], custom0: vec![], indices: vec![0, 1, 7, 0] };
    fix_t_junctions(&mut bad);
}

#[test]
fn malformed_buffer_is_skipped() {
    let mut buf = ClassBuffer::new(RenderChunkCoord::default());
    buf.voxels.truncate(10);
    buf.empty = false;
    let mut m = MeshData::new();
    GreedyMesher::new().mesh_cubes(&buf, &mut m);
    assert!(m.is_empty());
}
