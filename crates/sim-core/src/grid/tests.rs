use super::*;
use crate::rng::{self, Key, Stream};
use content::cell::{ShapeKind, BUCKET_DESTROYED};

const STEEL: Cell = Cell { material: Material::Steel, armour: ArmourTier::None, paint: 0, shape: CellShape::CUBE };

/// Hollow box hull (shell one cell thick) with a transverse bulkhead at x = 20 and a deck at y = 6.
fn hull(size: [u16; 3]) -> CellGrid {
    let mut g = CellGrid::new(size);
    for y in 0..size[1] as i16 {
        for z in 0..size[2] as i16 {
            for x in 0..size[0] as i16 {
                let shell = x == 0 || y == 0 || z == 0 || x == size[0] as i16 - 1 || y == size[1] as i16 - 1 || z == size[2] as i16 - 1;
                if shell || x == 20 || y == 6 { g.set(CellPos::new(x, y, z), STEEL); }
            }
        }
    }
    g
}

fn no_flags(_: PartSlot) -> (bool, Flammable) { (false, Flammable::No) }

/// Brute-force recompute of every derived bit from (design, damage), compared with the chunk storage.
fn check_consistent(g: &VesselGrid) {
    let [sx, sy, sz] = [g.size()[0] as i16, g.size()[1] as i16, g.size()[2] as i16];
    let live = |p: CellPos| g.design.index(p).is_some_and(|i| !g.design.cells[i.index()].is_empty() && g.damage[i.index()] < BUCKET_DESTROYED);
    let mut count = 0;
    let mut solid = Vec::new();
    for y in 0..sy { for z in 0..sz { for x in 0..sx {
        let p = CellPos::new(x, y, z);
        let l = live(p);
        assert_eq!(g.is_solid(p), l, "solid {p:?}");
        if l {
            count += 1;
            solid.push(g.design.index(p).unwrap());
            assert_eq!(g.is_watertight(p), g.cell_type(p).watertight);
            assert!(!g.is_air(p));
        } else {
            let ray = |dx: i16, dy: i16, dz: i16| {
                let mut q = p;
                loop {
                    q = CellPos::new(q.x + dx, q.y + dy, q.z + dz);
                    if !g.design.in_bounds(q) { return false; }
                    if live(q) { return true; }
                }
            };
            let enclosed = ray(1, 0, 0) && ray(-1, 0, 0) && ray(0, 1, 0) && ray(0, -1, 0) && ray(0, 0, 1) && ray(0, 0, -1);
            assert_eq!(g.is_air(p), enclosed, "air {p:?}");
            assert!(!g.is_watertight(p));
            assert_eq!(g.cell_type(p).material, Material::Empty);
        }
    } } }
    assert_eq!(g.solid_count, count);
    assert_eq!(g.solid_cells(), solid);
    for (ci, c) in g.chunks.iter().enumerate() {
        if let Some(c) = c {
            assert_eq!(c.solid_count as usize, c.solid.iter().map(|w| w.count_ones() as usize).sum::<usize>(), "chunk {ci}");
            assert!(c.solid_count > 0 || c.has_air(), "chunk {ci} should be None");
        }
    }
}

#[test]
fn build_hollow_hull() {
    let g = VesselGrid::build(Arc::new(hull([40, 12, 34])), &[], &no_flags);
    assert_eq!(g.chunk_dims, [3, 1, 3]);
    assert_eq!(g.chunks.len(), 9);
    assert_eq!(g.solid_count, g.original_count);
    assert_eq!(g.solid_count as usize, g.design.solid_count());
    assert!(g.is_air(CellPos::new(5, 3, 5)));
    assert!(!g.is_air(CellPos::new(-1, 3, 5)));
    assert_eq!(g.types.len(), 2);
    assert_eq!(g.types[0].material, Material::Empty);
    assert_eq!(g.damage.len(), g.design.len());
    check_consistent(&g);
}

#[test]
fn destroying_an_outer_plate() {
    let mut g = VesselGrid::build(Arc::new(hull([40, 12, 34])), &[], &no_flags);
    let p = CellPos::new(10, 3, 0);
    let i = g.index(p).unwrap();
    assert_eq!(g.add_damage(i, 3), 3);
    assert!(g.is_solid(p));
    assert_eq!(g.add_damage(i, 200), BUCKET_DESTROYED);
    assert!(!g.is_solid(p));
    assert!(!g.is_air(p), "a removed outer plate is open to the sea");
    // Cells behind the hole now see out along -Z; their neighbours are still enclosed.
    assert!(!g.is_air(CellPos::new(10, 3, 1)));
    assert!(g.is_air(CellPos::new(11, 3, 1)));
    assert_eq!(g.add_damage(i, 1), BUCKET_DESTROYED);
    assert_eq!(g.drain_changes(), vec![CellChange { cell: i, bucket: 3 }, CellChange { cell: i, bucket: 8 }]);
    assert!(g.drain_changes().is_empty());
    assert_eq!(g.drain_dirty_render(), vec![RenderChunkCoord { x: 0, y: 0, z: 0 }]);
    assert_eq!(g.drain_dirty_chunks(), vec![g.chunk_index_of(p).unwrap()]);
    assert_eq!(g.solid_count, g.original_count - 1);
    // Destroyed cells are not restored in a sortie.
    g.set_bucket(i, 0);
    assert!(!g.is_solid(p));
    assert_eq!(g.bucket(i), BUCKET_DESTROYED);
    check_consistent(&g);
}

#[test]
fn render_dirty_includes_padding_neighbours() {
    let mut g = VesselGrid::build(Arc::new(hull([40, 12, 34])), &[], &no_flags);
    let i = g.index(CellPos::new(31, 0, 32)).unwrap();
    g.add_damage(i, 8);
    let d = g.drain_dirty_render();
    assert_eq!(d, vec![
        RenderChunkCoord { x: 0, y: 0, z: 0 }, RenderChunkCoord { x: 0, y: 0, z: 1 },
        RenderChunkCoord { x: 1, y: 0, z: 0 }, RenderChunkCoord { x: 1, y: 0, z: 1 },
    ]);
}

#[test]
fn random_damage_stays_consistent() {
    let mut g = VesselGrid::build(Arc::new(hull([40, 12, 34])), &[], &no_flags);
    let n = g.design.len() as u32;
    for round in 0..6u64 {
        for k in 0..120u64 {
            let i = CellIndex(rng::below(Key::new(3, round, Stream::Misc, k, 0), n));
            let b = rng::below(Key::new(3, round, Stream::Misc, k, 1), 9) as u8;
            g.add_damage(i, b);
        }
        let cut: Vec<CellIndex> = (0..60u64).map(|k| CellIndex(rng::below(Key::new(4, round, Stream::Misc, k, 0), n))).collect();
        g.remove_cells(&cut);
        g.set_bucket(CellIndex(rng::below(Key::new(5, round, Stream::Misc, 0, 0), n)), 8);
        check_consistent(&g);
    }
    // Every change in the stream names a cell whose final bucket is at least what it recorded.
    for c in g.drain_changes() { assert!(g.bucket(c.cell) >= c.bucket); }
}

#[test]
fn removing_everything_frees_chunks() {
    let mut g = VesselGrid::build(Arc::new(hull([18, 8, 18])), &[], &no_flags);
    let all = g.solid_cells();
    g.remove_cells(&all);
    assert_eq!(g.solid_count, 0);
    assert!(g.chunks.iter().all(|c| c.is_none()));
    assert!(g.drain_changes().is_empty());
    assert!(g.damage.iter().zip(&g.design.cells).all(|(&d, c)| c.is_empty() || d == BUCKET_DESTROYED));
    check_consistent(&g);
}

#[test]
fn parts_and_types() {
    let mut d = CellGrid::new([4, 2, 2]);
    let body = Cell { material: Material::PartBody, ..STEEL };
    let wedge = Cell { shape: CellShape::new(ShapeKind::Wedge, 3), armour: ArmourTier::Mm15, ..STEEL };
    d.set(CellPos::new(0, 0, 0), body);
    d.set(CellPos::new(1, 0, 0), body);
    d.set(CellPos::new(2, 0, 0), body); // unowned
    d.set(CellPos::new(3, 0, 0), wedge);
    let parts = vec![(PartSlot(4), vec![CellIndex(0), CellIndex(1), CellIndex(999)]), (PartSlot(2), vec![CellIndex(1)])];
    let flags = |p: PartSlot| if p == PartSlot(4) { (true, Flammable::Fuel) } else { (false, Flammable::Ammunition) };
    let g = VesselGrid::build(Arc::new(d), &parts, &flags);
    let t0 = g.cell_type(CellPos::new(0, 0, 0));
    assert_eq!((t0.part, t0.watertight, t0.flammable), (Some(PartSlot(4)), true, Flammable::Fuel));
    let t1 = g.cell_type(CellPos::new(1, 0, 0));
    assert_eq!((t1.part, t1.watertight, t1.flammable), (Some(PartSlot(2)), false, Flammable::Ammunition));
    let t2 = g.cell_type(CellPos::new(2, 0, 0));
    assert_eq!((t2.part, t2.watertight), (None, false));
    let t3 = g.cell_type(CellPos::new(3, 0, 0));
    assert_eq!((t3.shape, t3.armour, t3.watertight), (wedge.shape, ArmourTier::Mm15, true));
    assert!(g.is_watertight(CellPos::new(0, 0, 0)));
    assert!(!g.is_watertight(CellPos::new(1, 0, 0)));
    assert_eq!(g.types.len(), 5);
    assert_eq!(g.cell_type(CellPos::new(9, 0, 0)), EMPTY_TYPE);
    assert_eq!(g.solid_neighbours(CellIndex(1)), [Some(CellIndex(0)), Some(CellIndex(2)), None, None, None, None]);
}

#[test]
fn malformed_design_is_inert() {
    let bad = CellGrid { size: [4, 4, 4], cells: vec![STEEL; 10] };
    let mut g = VesselGrid::build(Arc::new(bad), &[(PartSlot(0), vec![CellIndex(3)])], &no_flags);
    assert_eq!(g.solid_count, 0);
    assert!(!g.is_solid(CellPos::new(0, 0, 0)));
    assert_eq!(g.add_damage(CellIndex(3), 8), 0);
    g.set_bucket(CellIndex(50), 3);
    g.remove_cells(&[CellIndex(1), CellIndex(1 << 30)]);
    assert!(g.solid_cells().is_empty());
    assert_eq!(g.solid_neighbours(CellIndex(0)), [None; 6]);
    let mut n = 0;
    g.traverse([0.0; 3], [4.0; 3], &mut |_, _| { n += 1; true });
    assert_eq!(n, 0);
    let empty = VesselGrid::build(Arc::new(CellGrid::new([0, 0, 0])), &[], &no_flags);
    assert!(empty.solid_cells().is_empty());
    assert_eq!(g.view().cells.len(), 10);
    assert_eq!(g.bucket(CellIndex(1 << 30)), 0);
    assert_eq!(g.design_cell(CellIndex(1 << 30)), Cell::EMPTY);
}

fn traverse_list(g: &VesselGrid, a: [f64; 3], b: [f64; 3]) -> Vec<(CellPos, Face)> {
    let mut v = Vec::new();
    g.traverse(a, b, &mut |p, f| { v.push((p, f)); true });
    v
}

#[test]
fn traversal_matches_brute_force() {
    let g = VesselGrid::build(Arc::new(hull([40, 12, 34])), &[], &no_flags);
    let size = [40.0, 12.0, 34.0];
    for k in 0..300u64 {
        let r = |j: u64| rng::uniform(Key::new(9, k, Stream::Misc, j, 0));
        let a = [r(0) * 60.0 - 10.0, r(1) * 24.0 - 6.0, r(2) * 54.0 - 10.0];
        let b = [r(3) * 60.0 - 10.0, r(4) * 24.0 - 6.0, r(5) * 54.0 - 10.0];
        let cells = traverse_list(&g, a, b);
        // Consecutive cells are face neighbours, and the face given is the one entered through.
        for w in cells.windows(2) {
            let (p, q) = (w[0].0, w[1].0);
            let (dx, dy, dz) = (q.x - p.x, q.y - p.y, q.z - p.z);
            assert_eq!(dx.abs() + dy.abs() + dz.abs(), 1, "{a:?} {b:?}");
            let (ox, oy, oz) = w[1].1.offset();
            assert_eq!((ox, oy, oz), (-dx, -dy, -dz));
        }
        // Every point sampled on the segment inside the box lies in a visited cell.
        for s in 0..=400 {
            let t = s as f64 / 400.0;
            let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
            if (0..3).all(|i| p[i] > 1e-6 && p[i] < size[i] - 1e-6) {
                let c = CellPos::new(p[0].floor() as i16, p[1].floor() as i16, p[2].floor() as i16);
                assert!(cells.iter().any(|&(q, _)| q == c), "{a:?} {b:?} missing {c:?}");
            }
        }
        // No cell twice.
        let mut sorted: Vec<CellPos> = cells.iter().map(|c| c.0).collect();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), cells.len());
    }
}

#[test]
fn traversal_edge_cases() {
    let g = VesselGrid::build(Arc::new(hull([40, 12, 34])), &[], &no_flags);
    // Entering from outside along -X gets the XPos face.
    let v = traverse_list(&g, [50.0, 3.5, 3.5], [37.5, 3.5, 3.5]);
    assert_eq!(v.first(), Some(&(CellPos::new(39, 3, 3), Face::XPos)));
    assert_eq!(v.len(), 3);
    // Starting inside along +Y: first face is YNeg.
    let v = traverse_list(&g, [1.5, 1.5, 1.5], [1.5, 4.5, 1.5]);
    assert_eq!(v.iter().map(|c| c.0.y).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
    assert_eq!(v[0].1, Face::YNeg);
    // Zero length.
    assert_eq!(traverse_list(&g, [2.5, 2.5, 2.5], [2.5, 2.5, 2.5]), vec![(CellPos::new(2, 2, 2), Face::XNeg)]);
    // Misses and NaN.
    assert!(traverse_list(&g, [-5.0, -5.0, -5.0], [-1.0, 20.0, -1.0]).is_empty());
    assert!(traverse_list(&g, [f64::NAN, 0.0, 0.0], [1.0, 1.0, 1.0]).is_empty());
    // Early stop.
    let mut n = 0;
    g.traverse([0.5, 0.5, 0.5], [39.5, 0.5, 0.5], &mut |_, _| { n += 1; n < 5 });
    assert_eq!(n, 5);
}
