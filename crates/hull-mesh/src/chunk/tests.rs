use super::*;
use content::cell::{ArmourTier, Cell, CellGrid, CellPos, CellShape, Material, ShapeKind};
use content::ids::CellIndex;
use std::collections::BTreeSet;

fn steel() -> Cell { Cell { material: Material::Steel, armour: ArmourTier::None, paint: 0, shape: CellShape::CUBE } }

fn input<'a>(g: &'a CellGrid, damage: &'a [u8], sealed: &'a [u64]) -> MeshInput<'a> {
    MeshInput {
        view: CellGridView { size: g.size, cells: &g.cells, damage, sealed_air: sealed, scorch: &[] },
        palette: &[],
        wear_seed: 7,
        design_waterline_y: 0,
    }
}

fn idx(g: &CellGrid, x: i16, y: i16, z: i16) -> usize { g.index(CellPos::new(x, y, z)).unwrap().index() }

/// Closed box hull: walls one cell thick around an interior compartment.
fn box_hull(n: i16) -> CellGrid {
    let mut g = CellGrid::new([n as u16; 3]);
    for x in 0..n {
        for y in 0..n {
            for z in 0..n {
                let edge = x == 0 || y == 0 || z == 0 || x == n - 1 || y == n - 1 || z == n - 1;
                if edge {
                    g.set(CellPos::new(x, y, z), steel());
                }
            }
        }
    }
    g
}

fn sealed_bits(g: &CellGrid) -> Vec<u64> {
    let mut bits = vec![0u64; g.len().div_ceil(64)];
    for (i, c) in g.cells.iter().enumerate() {
        if c.is_empty() {
            bits[i / 64] |= 1 << (i % 64);
        }
    }
    bits
}

#[test]
fn chunk_enumeration_is_ascending() {
    let c = all_chunks([33, 32, 65]);
    assert_eq!(c.len(), 2 * 1 * 3);
    assert_eq!(c[0], RenderChunkCoord { x: 0, y: 0, z: 0 });
    assert_eq!(c[1], RenderChunkCoord { x: 0, y: 0, z: 1 });
    assert!(c.windows(2).all(|w| w[0] < w[1]));
    assert!(all_chunks([0, 10, 10]).is_empty());
    // A 90 m hull is 180 cells: 6 chunks along x.
    assert_eq!(all_chunks([180, 32, 32]).len(), 6);
    // Oversized sizes clamp instead of wrapping.
    assert_eq!(all_chunks([u16::MAX, 1, 1]).len(), 12);
}

#[test]
fn validation_reports_each_error() {
    let g = CellGrid::new([4, 4, 4]);
    assert!(validate_input(&input(&g, &[], &[])).is_ok());
    assert_eq!(validate_input(&input(&g, &[0; 3], &[])), Err(MeshError::ViewLength));
    assert_eq!(validate_input(&input(&g, &[], &[0; 2])), Err(MeshError::ViewLength));
    assert!(validate_input(&input(&g, &[0; 64], &[0; 1])).is_ok());
    let pal = [[0.0f32; 3]; 17];
    let mut i = input(&g, &[], &[]);
    i.palette = &pal;
    assert_eq!(validate_input(&i), Err(MeshError::Palette(17)));
    let big = CellGrid { size: [400, 1, 1], cells: vec![Cell::EMPTY; 400] };
    assert_eq!(validate_input(&input(&big, &[], &[])), Err(MeshError::GridTooLarge([400, 1, 1])));
    let mut buf = ClassBuffer::new(RenderChunkCoord::default());
    assert_eq!(fill_class_buffer(&input(&g, &[], &[]), RenderChunkCoord { x: 1, y: 0, z: 0 }, &mut buf), Err(MeshError::ChunkOutOfRange(RenderChunkCoord { x: 1, y: 0, z: 0 })));
    // Malformed views never panic the volume builder.
    let bad = CellGridView { size: [4, 4, 4], cells: &g.cells[..10], damage: &[], sealed_air: &[], scorch: &[] };
    let bi = MeshInput { view: bad, palette: &[], wear_seed: 0, design_waterline_y: 0 };
    assert_eq!(build_volume(&bi, RenderChunkCoord::default()), VolumeTexture::zeroed());
    assert_eq!(material_class(&bi, CellIndex(3)), MaterialClass::Paint);
}

#[test]
fn class_mapping_and_padding() {
    let mut g = CellGrid::new([40, 3, 3]);
    g.set(CellPos::new(0, 0, 0), steel());
    g.set(CellPos::new(1, 0, 0), Cell { material: Material::Glass, ..steel() });
    g.set(CellPos::new(2, 0, 0), Cell { shape: CellShape::new(ShapeKind::Wedge, 5), ..steel() });
    g.set(CellPos::new(3, 0, 0), Cell { shape: CellShape(0x20 | 30), ..steel() });
    g.set(CellPos::new(4, 0, 0), steel());
    // x = 32 is in chunk 1, and in chunk 0's padding.
    g.set(CellPos::new(32, 1, 1), steel());
    let mut damage = vec![0u8; g.len()];
    damage[idx(&g, 4, 0, 0)] = 8;
    let mut sealed = vec![0u64; g.len().div_ceil(64)];
    let s = idx(&g, 5, 0, 0);
    sealed[s / 64] |= 1 << (s % 64);
    let inp = input(&g, &damage, &sealed);
    let mut buf = ClassBuffer::new(RenderChunkCoord::default());
    fill_class_buffer(&inp, RenderChunkCoord::default(), &mut buf).unwrap();
    let at = |x, y, z| buf.voxels[ClassBuffer::pad_index(x, y, z)];
    assert_eq!(at(0, 0, 0), class::OPAQUE);
    assert_eq!(at(1, 0, 0), class::GLASS);
    assert_eq!(at(2, 0, 0), class::SHAPED_BASE | CellShape::new(ShapeKind::Wedge, 5).0 as u16);
    assert_eq!(at(3, 0, 0), class::SHAPED_BASE | 0x20, "invalid orientation canonicalised");
    assert_eq!(at(4, 0, 0), class::EXPOSED_AIR, "destroyed");
    assert_eq!(at(5, 0, 0), class::SEALED_AIR);
    assert_eq!(at(32, 1, 1), class::OPAQUE, "padding from the next chunk");
    assert_eq!(at(-1, 0, 0), class::EXPOSED_AIR);
    assert!(!buf.empty && buf.has_shaped);
    // Masks match bgm's own mask builder.
    let reference = binary_greedy_meshing::compute_opaque_mask::<32>(&buf.voxels, &BTreeSet::new());
    assert_eq!(&buf.opaque_mask[..], &reference[..]);
    assert!(buf.trans_mask.iter().all(|&m| m == 0));

    // Chunk 1 sees the steel cell and has no shaped cell anywhere.
    fill_class_buffer(&inp, RenderChunkCoord { x: 1, y: 0, z: 0 }, &mut buf).unwrap();
    assert!(!buf.empty && !buf.has_shaped);
    assert_eq!(buf.voxels[ClassBuffer::pad_index(0, 1, 1)], class::OPAQUE);
}

#[test]
fn shaped_padding_sets_has_shaped_and_sealed_only_is_empty() {
    let mut g = CellGrid::new([33, 1, 1]);
    g.set(CellPos::new(32, 0, 0), Cell { shape: CellShape::new(ShapeKind::Corner, 0), ..steel() });
    let mut buf = ClassBuffer::new(RenderChunkCoord::default());
    fill_class_buffer(&input(&g, &[], &[]), RenderChunkCoord::default(), &mut buf).unwrap();
    assert!(buf.empty && buf.has_shaped);
    let g = CellGrid::new([2, 2, 2]);
    let sealed = vec![u64::MAX; 1];
    fill_class_buffer(&input(&g, &[], &sealed), RenderChunkCoord::default(), &mut buf).unwrap();
    assert!(buf.empty, "sealed air alone has no faces");
    // Sealed air beside a shaped cell in the padding gets a face from the shaped pass.
    let mut g = CellGrid::new([33, 1, 1]);
    g.set(CellPos::new(32, 0, 0), Cell { shape: CellShape::new(ShapeKind::Wedge, 0), ..steel() });
    let sealed = vec![1u64 << 31];
    fill_class_buffer(&input(&g, &[], &sealed), RenderChunkCoord::default(), &mut buf).unwrap();
    assert!(!buf.empty && buf.has_shaped);
}

/// A hull with open air round it: plating, a deck under open sky, an overhang, a sealed compartment.
fn open_hull() -> (CellGrid, Vec<u64>) {
    let mut g = CellGrid::new([16, 14, 12]);
    for x in 2..14 {
        for y in 1..7 {
            for z in 2..10 {
                let shell = x == 2 || x == 13 || y == 1 || y == 6 || z == 2 || z == 9;
                if shell {
                    g.set(CellPos::new(x, y, z), steel());
                }
            }
        }
    }
    // A column and an overhanging slab above the deck: the air under it is open sideways.
    for y in 7..10 {
        g.set(CellPos::new(4, y, 5), steel());
    }
    for x in 3..9 {
        for z in 3..8 {
            g.set(CellPos::new(x, 10, z), steel());
        }
    }
    let mut sealed = vec![0u64; g.len().div_ceil(64)];
    for x in 3..13 {
        for y in 2..6 {
            for z in 3..9 {
                let i = idx(&g, x, y, z);
                sealed[i / 64] |= 1 << (i % 64);
            }
        }
    }
    (g, sealed)
}

#[test]
fn bare_rays_on_an_open_hull() {
    let (g, sealed) = open_hull();
    let damage = vec![0u8; g.len()];
    // Editor and intact live views: no primer anywhere, overhang and deck included.
    assert_eq!(bare_count(&input(&g, &[], &[])), 0);
    assert_eq!(bare_count(&input(&g, &damage, &sealed)), 0);
    let v = build_volume(&input(&g, &damage, &sealed), RenderChunkCoord::default());
    assert!(v.bytes.chunks_exact(4).all(|t| t[1] >> 4 != MaterialClass::Bare as u8));
    // Breach the side plating and unseal the compartment.
    let mut damage = damage;
    damage[idx(&g, 7, 3, 2)] = 8;
    damage[idx(&g, 8, 3, 2)] = 8;
    let unsealed = vec![0u64; sealed.len()];
    let inp = input(&g, &damage, &unsealed);
    let mc = |x, y, z| material_class(&inp, CellIndex(idx(&g, x, y, z) as u32));
    // Hull plating faces the breached room but keeps its outside face: paint.
    assert_eq!(mc(7, 3, 9), MaterialClass::Paint, "far wall has an outside face");
    assert_eq!(mc(6, 3, 2), MaterialClass::Paint, "plating round the hole keeps its outside face");
    // The deck plating has open sky above, so it stays paint.
    assert_eq!(mc(7, 6, 5), MaterialClass::Paint);
    // Interior structure exposed to the breached room is bare: put a pillar inside and check it.
    let mut g2 = g.clone();
    for y in 2..6 {
        g2.set(CellPos::new(8, y, 6), steel());
    }
    let inp2 = input(&g2, &damage, &unsealed);
    assert_eq!(material_class(&inp2, CellIndex(idx(&g2, 8, 3, 6) as u32)), MaterialClass::Bare);
    // Still sealed: the same pillar is paint.
    let mut sealed2 = sealed.clone();
    for (i, c) in g2.cells.iter().enumerate() {
        if !c.is_empty() {
            sealed2[i / 64] &= !(1 << (i % 64));
        }
    }
    let zero = vec![0u8; g2.len()];
    let inp3 = input(&g2, &zero, &sealed2);
    assert_eq!(material_class(&inp3, CellIndex(idx(&g2, 8, 3, 6) as u32)), MaterialClass::Paint);
    assert_eq!(bare_count(&inp3), 0);
}

#[test]
fn volume_texel_layout() {
    let mut g = CellGrid::new([3, 3, 3]);
    g.set(CellPos::new(1, 2, 0), Cell { material: Material::Aluminium, armour: ArmourTier::Mm40, paint: 9, shape: CellShape::CUBE });
    g.set(CellPos::new(0, 0, 0), Cell { material: Material::Glass, ..steel() });
    let mut scorch = vec![0u8; g.len()];
    scorch[idx(&g, 1, 2, 0)] = 200;
    let mut inp = input(&g, &[], &[]);
    inp.view.scorch = &scorch;
    let v = build_volume(&inp, RenderChunkCoord::default());
    assert_eq!(v.bytes.len(), 157_216);
    let t = VolumeTexture::texel_offset(2, 3, 1);
    assert_eq!(v.bytes[t], 255);
    assert_eq!(v.bytes[t + 1], 9);
    assert_eq!(v.bytes[t + 2], 200);
    assert_eq!(v.bytes[t + 3] & 7, 3);
    assert_eq!(v.bytes[t + 3] >> 3, view::wear_bits(7, idx(&g, 1, 2, 0)));
    let t = VolumeTexture::texel_offset(1, 1, 1);
    assert_eq!(v.bytes[t + 1] >> 4, MaterialClass::Glass as u8);
    assert_eq!(v.bytes[VolumeTexture::texel_offset(2, 2, 2)], 0, "air");
    assert_eq!(v.bytes[VolumeTexture::texel_offset(0, 1, 1)], 0, "padding outside the grid");
    assert_eq!(build_volume(&inp, RenderChunkCoord::default()), v, "deterministic");
    // Wear seeds vary across cells and ships.
    let seeds: BTreeSet<u8> = (0..64).map(|i| view::wear_bits(1, i)).collect();
    assert!(seeds.len() > 16);
    assert_ne!((0..32).map(|i| view::wear_bits(1, i)).collect::<Vec<_>>(), (0..32).map(|i| view::wear_bits(2, i)).collect::<Vec<_>>());
}

fn bare_count(inp: &MeshInput<'_>) -> usize {
    (0..inp.view.cells.len()).filter(|&i| material_class(inp, CellIndex(i as u32)) == MaterialClass::Bare).count()
}

#[test]
fn bare_marks_only_damage_exposed_interiors() {
    let n = 7;
    let mut g = box_hull(n);
    // An inner bulkhead at x = 3 splits the compartment.
    for y in 1..n - 1 {
        for z in 1..n - 1 {
            g.set(CellPos::new(3, y, z), steel());
        }
    }
    // Editor view: nothing is bare.
    assert_eq!(bare_count(&input(&g, &[], &[])), 0);
    // Intact live view, compartments sealed: nothing is bare.
    let sealed = sealed_bits(&g);
    assert_eq!(bare_count(&input(&g, &vec![0; g.len()], &sealed)), 0);
    // Destroy a plating cell on the x = 6 wall and unseal the x = 4..5 compartment.
    let mut damage = vec![0u8; g.len()];
    damage[idx(&g, 6, 3, 3)] = 8;
    let mut sealed = sealed;
    for x in 4..6 {
        for y in 1..n - 1 {
            for z in 1..n - 1 {
                let i = idx(&g, x, y, z);
                sealed[i / 64] &= !(1 << (i % 64));
            }
        }
    }
    let inp = input(&g, &damage, &sealed);
    // The bulkhead face toward the breached compartment shows primer.
    assert_eq!(material_class(&inp, CellIndex(idx(&g, 3, 3, 3) as u32)), MaterialClass::Bare);
    // Plating round the hole still has its outside face: paint.
    assert_eq!(material_class(&inp, CellIndex(idx(&g, 6, 2, 3) as u32)), MaterialClass::Paint);
    // The still-sealed compartment's walls do not.
    assert_eq!(material_class(&inp, CellIndex(idx(&g, 0, 3, 3) as u32)), MaterialClass::Paint);
    // Volume agrees with the per-cell query.
    let v = build_volume(&inp, RenderChunkCoord::default());
    let t = VolumeTexture::texel_offset(4, 4, 4);
    assert_eq!(v.bytes[t + 1] >> 4, MaterialClass::Bare as u8);
    let t = VolumeTexture::texel_offset(7, 3, 4);
    assert_eq!(v.bytes[t + 1] >> 4, MaterialClass::Paint as u8);
    // A destroyed cell exposes the solid core behind it even without compartment data.
    let mut g3 = CellGrid::new([5, 5, 5]);
    for c in g3.cells.iter_mut() {
        *c = steel();
    }
    let mut d3 = vec![0u8; 125];
    d3[idx(&g3, 2, 2, 1)] = 8;
    let inp3 = input(&g3, &d3, &[]);
    assert_eq!(material_class(&inp3, CellIndex(idx(&g3, 2, 2, 2) as u32)), MaterialClass::Bare);
    assert_eq!(material_class(&inp3, CellIndex(idx(&g3, 2, 2, 0) as u32)), MaterialClass::Paint);
}

#[test]
fn dirty_sets_cover_padding_neighbours() {
    let size = [96, 64, 40];
    let g = CellGrid::new(size);
    let ch = |x, y, z| CellChange { cell: g.index(CellPos::new(x, y, z)).unwrap(), bucket: 8 };
    // Interior cell: only its own chunk.
    assert_eq!(dirty_chunks(size, &[ch(40, 10, 10)]), vec![RenderChunkCoord { x: 1, y: 0, z: 0 }]);
    // Corner cell at (32, 32, 31): chunks x 0..=1, y 0..=1, z 0..=1.
    let d = dirty_chunks(size, &[ch(32, 32, 31)]);
    assert_eq!(d.len(), 8);
    assert!(d.windows(2).all(|w| w[0] < w[1]));
    // Grid edge: no chunk below zero or past the last chunk.
    assert_eq!(dirty_chunks(size, &[ch(0, 0, 0)]), vec![RenderChunkCoord::default()]);
    assert_eq!(dirty_chunks(size, &[ch(95, 63, 39)]), vec![RenderChunkCoord { x: 2, y: 1, z: 1 }]);
    // Duplicates collapse; bad indices are ignored.
    let d = dirty_chunks(size, &[ch(40, 10, 10), ch(41, 10, 10), CellChange { cell: CellIndex(u32::MAX), bucket: 8 }]);
    assert_eq!(d.len(), 1);
    let cells = [g.index(CellPos::new(63, 5, 5)).unwrap(), g.index(CellPos::new(64, 5, 5)).unwrap()];
    assert_eq!(dirty_chunks_for_cells(size, &cells), vec![RenderChunkCoord { x: 1, y: 0, z: 0 }, RenderChunkCoord { x: 2, y: 0, z: 0 }]);
}
