use super::*;

const KINDS: [ShapeKind; 6] = [ShapeKind::Cube, ShapeKind::Wedge, ShapeKind::Corner, ShapeKind::InverseCorner, ShapeKind::SlopeLow, ShapeKind::SlopeHigh];

fn det(m: [[i8; 3]; 3]) -> i32 {
    let m = m.map(|r| r.map(|v| v as i32));
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

/// Geometry fingerprint of a shape: corners, full faces and the slope normal. Equal fingerprints mean the same
/// solid for every kind (slopes included, since the normal fixes the fall direction).
fn geometry(s: CellShape) -> (ShapeKind, u8, u8, [i64; 3]) {
    let n = if s.is_cube() { [0.0; 3] } else { armour_normal(s, [f64::NAN; 3]) };
    (s.kind(), shape_corner_mask(s), full_face_mask(s), n.map(|v| (v * 1e6).round() as i64))
}

#[test]
fn orientation_table_is_the_rotation_group() {
    assert_eq!(orientation_matrix(0), [[1, 0, 0], [0, 1, 0], [0, 0, 1]]);
    let mut seen = Vec::new();
    for o in 0..24 {
        let m = orientation_matrix(o);
        assert_eq!(det(m), 1);
        for r in m {
            assert_eq!(r.iter().map(|v| v.abs() as i32).sum::<i32>(), 1);
        }
        assert!(!seen.contains(&m));
        seen.push(m);
        assert_eq!(orientation_index(m), Some(o));
    }
    assert_eq!(orientation_matrix(31), orientation_matrix(0));
    assert_eq!(orientation_index([[1, 0, 0], [0, 1, 0], [0, 0, -1]]), None);
}

#[test]
fn rotations_compose() {
    for o in 0..24 {
        for axis in [Axis::X, Axis::Y, Axis::Z] {
            assert_eq!(rotate_orientation(o, axis, 4), o);
            assert_eq!(rotate_orientation(rotate_orientation(o, axis, 1), axis, -1), o);
            assert_eq!(rotate_orientation(o, axis, 3), rotate_orientation(o, axis, -1));
            assert_ne!(rotate_orientation(o, axis, 1), o);
        }
    }
    // A +90 degree yaw (about +Y) carries canonical +X to -Z.
    assert_eq!(orientation_matrix(rotate_orientation(0, Axis::Y, 1))[0], [0, 0, -1]);
    // Yaw and tilt reach all 24 orientations from identity.
    let mut reach = vec![0u8];
    let mut i = 0;
    while i < reach.len() {
        for axis in [Axis::X, Axis::Y] {
            let n = rotate_orientation(reach[i], axis, 1);
            if !reach.contains(&n) {
                reach.push(n);
            }
        }
        i += 1;
    }
    assert_eq!(reach.len(), 24);
}

#[test]
fn canonical_faces_and_corners() {
    assert_eq!(full_face_mask(CellShape::CUBE), 0x3F);
    assert_eq!(full_face_mask(CellShape::new(ShapeKind::Wedge, 0)), 0b000101);
    assert_eq!(full_face_mask(CellShape::new(ShapeKind::Corner, 7)), 0);
    assert_eq!(full_face_mask(CellShape::new(ShapeKind::InverseCorner, 0)), 0b010101);
    assert_eq!(full_face_mask(CellShape::new(ShapeKind::SlopeLow, 0)), 0b000100);
    for o in 0..24 {
        for (kind, faces, corners) in [(ShapeKind::Wedge, 2, 6), (ShapeKind::Corner, 0, 4), (ShapeKind::InverseCorner, 3, 7), (ShapeKind::SlopeHigh, 2, 6), (ShapeKind::SlopeLow, 1, 4)] {
            let s = CellShape::new(kind, o);
            assert_eq!(full_face_mask(s).count_ones(), faces);
            assert_eq!(shape_corner_mask(s).count_ones(), corners);
        }
    }
    // 12 distinct wedges, 8 corners, 8 inverse corners.
    for (kind, n) in [(ShapeKind::Wedge, 12), (ShapeKind::Corner, 8), (ShapeKind::InverseCorner, 8)] {
        let mut masks: Vec<u8> = (0..24).map(|o| shape_corner_mask(CellShape::new(kind, o))).collect();
        masks.sort();
        masks.dedup();
        assert_eq!(masks.len(), n);
    }
}

#[test]
fn full_faces_agree_with_corners() {
    // A face is full exactly when the solid covers all four of its corners and it is not a slope's half face.
    for kind in [ShapeKind::Cube, ShapeKind::Wedge, ShapeKind::Corner, ShapeKind::InverseCorner] {
        for o in 0..24 {
            let s = CellShape::new(kind, o);
            let corners = shape_corner_mask(s);
            for (fi, f) in Face::ALL.iter().enumerate() {
                let (dx, dy, dz) = f.offset();
                let on_face = (0..8u8).filter(|i| {
                    let c = [(i & 1) as i16, ((i >> 1) & 1) as i16, ((i >> 2) & 1) as i16];
                    (dx == 0 || c[0] == (dx + 1) / 2) && (dy == 0 || c[1] == (dy + 1) / 2) && (dz == 0 || c[2] == (dz + 1) / 2)
                });
                let all = on_face.clone().all(|i| corners & (1 << i) != 0);
                assert_eq!(full_face_mask(s) & (1 << fi) != 0, all, "{kind:?} o{o} {f:?}");
            }
        }
    }
}

#[test]
fn mirror_reflects_geometry() {
    for kind in KINDS {
        for o in 0..24 {
            let s = CellShape::new(kind, o);
            let m = mirror_shape_z(s);
            assert_eq!(m.kind(), kind);
            assert_eq!(geometry(mirror_shape_z(m)), geometry(s));
            let flip_corner = |mask: u8| (0..8u8).filter(|i| mask & (1 << i) != 0).fold(0u8, |acc, i| acc | 1 << (i ^ 4));
            assert_eq!(shape_corner_mask(m), flip_corner(shape_corner_mask(s)));
            let flip_face = |mask: u8| (mask & 0x0F) | ((mask >> 1) & 0x10) | ((mask << 1) & 0x20);
            assert_eq!(full_face_mask(m), flip_face(full_face_mask(s)));
            if kind != ShapeKind::Cube {
                let (a, b) = (armour_normal(s, [f64::NAN; 3]), armour_normal(m, [f64::NAN; 3]));
                assert!((a[0] - b[0]).abs() < 1e-12 && (a[1] - b[1]).abs() < 1e-12 && (a[2] + b[2]).abs() < 1e-12);
            }
        }
    }
}

#[test]
fn armour_normals() {
    let c = CellShape::CUBE;
    assert_eq!(armour_normal(c, [1.0, 0.2, -0.1]), [-1.0, 0.0, 0.0]);
    assert_eq!(armour_normal(c, [0.0, 0.0, -3.0]), [0.0, 0.0, 1.0]);
    assert_eq!(armour_normal(c, [0.0, 0.0, 0.0]), [0.0, 1.0, 0.0]);
    let w = CellShape::new(ShapeKind::Wedge, 0);
    let n = armour_normal(w, [-1.0, -0.2, 0.0]);
    assert!((n[0] - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-12 && (n[1] - n[0]).abs() < 1e-12);
    // From behind the shell meets the flat back face.
    assert_eq!(armour_normal(w, [1.0, 0.0, 0.0]), [-1.0, 0.0, 0.0]);
    for kind in KINDS {
        for o in 0..24 {
            let n = armour_normal(CellShape::new(kind, o), [0.3, -0.8, 0.5]);
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-12);
            assert!(n[0] * 0.3 - n[1] * 0.8 + n[2] * 0.5 < 0.0);
        }
    }
}

#[test]
fn corner_table() {
    assert_eq!(shape_from_corners(0), None);
    assert_eq!(shape_from_corners(0xFF), Some(CellShape::CUBE));
    for kind in [ShapeKind::Wedge, ShapeKind::Corner, ShapeKind::InverseCorner] {
        for o in 0..24 {
            let s = CellShape::new(kind, o);
            let got = shape_from_corners(shape_corner_mask(s)).unwrap();
            assert_eq!(got.kind(), kind);
            assert_eq!(geometry(got), geometry(s));
        }
    }
    // A single inside corner gets the corner solid whose apex is that corner.
    for i in 0..8u8 {
        let s = shape_from_corners(1 << i).unwrap();
        assert_eq!(s.kind(), ShapeKind::Corner);
        let apex = (0..8u8).find(|j| { let m = shape_corner_mask(s); (0..3).all(|b| m & (1 << (j ^ (1 << b))) != 0) && m & (1 << j) != 0 });
        assert_eq!(apex, Some(i));
    }
    // One corner missing is an inverse corner. A full face of four is equally near four wedges and four corner
    // solids, a symmetric tie that goes to cube.
    assert_eq!(shape_from_corners(0x7F).unwrap().kind(), ShapeKind::InverseCorner);
    assert_eq!(shape_from_corners(0x0F), Some(CellShape::CUBE));
    for p in 1..=255u8 {
        let s = shape_from_corners(p).unwrap();
        // Nearest by Hamming distance, and cube whenever it is among the nearest.
        let dist = |s: CellShape| (shape_corner_mask(s) ^ p).count_ones();
        let best = [ShapeKind::Wedge, ShapeKind::Corner, ShapeKind::InverseCorner].iter().flat_map(|k| (0..24).map(|o| dist(CellShape::new(*k, o)))).min().unwrap();
        let cube = dist(CellShape::CUBE);
        if cube <= best {
            assert!(s.is_cube(), "pattern {p:#x}");
        } else {
            assert!(dist(s) == best || s.is_cube(), "pattern {p:#x}");
        }
        assert!(matches!(s.kind(), ShapeKind::Cube | ShapeKind::Wedge | ShapeKind::Corner | ShapeKind::InverseCorner));
        assert!(s.orientation() < 24);
    }
}

fn steel(paint: u8) -> Cell { Cell { material: Material::Steel, armour: ArmourTier::None, paint, shape: CellShape::CUBE } }

#[test]
fn rows_round_trip() {
    let mut g = CellGrid::new([7, 3, 4]);
    for (i, c) in g.cells.iter_mut().enumerate() {
        if i % 3 != 0 {
            *c = steel((i % 2) as u8);
        }
    }
    g.set(CellPos::new(6, 2, 3), Cell { material: Material::Glass, armour: ArmourTier::Mm40, paint: 3, shape: CellShape::new(ShapeKind::Wedge, 5) });
    let rows = g.to_rows();
    assert_eq!(CellGrid::from_rows(&rows), g);
    assert_eq!(rows.0.iter().flat_map(|r| &r.runs).map(|r| r.len as usize).sum::<usize>(), g.solid_count());
    let keys: Vec<_> = rows.0.iter().map(|r| (r.y, r.z)).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}

#[test]
fn rows_merge_and_clip() {
    let rows = CellRows(vec![
        CellRow { y: 1, z: 0, runs: vec![CellRun { x: 2, len: 3, cell: steel(0) }, CellRun { x: 5, len: 2, cell: steel(0) }] },
        CellRow { y: 0, z: 0, runs: vec![CellRun { x: -4, len: 5, cell: steel(1) }, CellRun { x: 3, len: 0, cell: steel(1) }] },
        CellRow { y: -1, z: 0, runs: vec![CellRun { x: 0, len: 9, cell: steel(1) }] },
        CellRow { y: 0, z: 2, runs: vec![CellRun { x: 0, len: 4, cell: Cell::EMPTY }] },
    ]);
    let g = CellGrid::from_rows(&rows);
    assert_eq!(g.size, [7, 2, 1]);
    assert_eq!(g.solid_count(), 6);
    let canon = g.to_rows();
    assert_eq!(canon.0.len(), 2);
    assert_eq!((canon.0[0].y, canon.0[0].runs[0]), (0, CellRun { x: 0, len: 1, cell: steel(1) }));
    assert_eq!(canon.0[1].runs, vec![CellRun { x: 2, len: 5, cell: steel(0) }]);
    assert_eq!(CellGrid::from_rows(&CellRows::default()).size, [0, 0, 0]);
    let far = CellRows(vec![CellRow { y: 0, z: 0, runs: vec![CellRun { x: i16::MAX, len: u16::MAX, cell: steel(0) }] }]);
    assert_eq!(CellGrid::from_rows(&far).size, [0, 0, 0]);
    // A grid with a short cell vector does not panic.
    let broken = CellGrid { size: [4, 4, 4], cells: vec![steel(0); 10] };
    assert_eq!(broken.to_rows().0.len(), 2);
}

/// Bit permutation of corner patterns under a +90 degree turn about `axis` (same map as the orientation rows).
fn rotate_pattern(p: u8, axis: Axis) -> u8 {
    let o = rotate_orientation(0, axis, 1);
    let m = orientation_matrix(o);
    let mut out = 0u8;
    for i in 0..8u8 {
        if p & (1 << i) == 0 {
            continue;
        }
        let v = [((i & 1) * 2) as i8 - 1, (((i >> 1) & 1) * 2) as i8 - 1, (((i >> 2) & 1) * 2) as i8 - 1];
        let mut w = [0i8; 3];
        for k in 0..3 {
            for c in 0..3 {
                w[c] += v[k] * m[k][c];
            }
        }
        out |= 1 << (((w[0] + 1) / 2) as u8 | ((((w[1] + 1) / 2) as u8) << 1) | ((((w[2] + 1) / 2) as u8) << 2));
    }
    out
}

#[test]
fn corner_table_is_symmetric() {
    for p in 1..=255u8 {
        let s = shape_from_corners(p).unwrap();
        let mirrored: u8 = (0..8u8).filter(|i| p & (1 << i) != 0).fold(0, |acc, i| acc | 1 << (i ^ 4));
        assert_eq!(geometry(shape_from_corners(mirrored).unwrap()), geometry(mirror_shape_z(s)), "mirror {p:#x}");
        for axis in [Axis::X, Axis::Y, Axis::Z] {
            let r = shape_from_corners(rotate_pattern(p, axis)).unwrap();
            let expect = CellShape::new(s.kind(), rotate_orientation(s.orientation(), axis, 1));
            assert_eq!(geometry(r), geometry(expect), "rotate {p:#x} {axis:?}");
        }
    }
}
