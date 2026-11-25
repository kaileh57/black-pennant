//! Shape geometry: the 24-rotation table, rotation and mirroring of shape bytes, armour normals, full-face masks
//! and the voxelizer's corner-pattern table.
//!
//! # Canonical shapes (orientation 0, unit cell `0..=1` on every axis)
//! The solid of each [`ShapeKind`] at orientation 0 is:
//!
//! | Kind | Solid | Sloped face normal | Full faces | Corner mask |
//! |---|---|---|---|---|
//! | Cube | whole cell | none | all six | `0xFF` |
//! | Wedge | `x + y <= 1` | `(1, 1, 0) / sqrt 2` | X-, Y- | `0x77` |
//! | Corner | `x + y + z <= 1` | `(1, 1, 1) / sqrt 3` | none | `0x17` |
//! | InverseCorner | `x + y + z <= 2` | `(1, 1, 1) / sqrt 3` | X-, Y-, Z- | `0x7F` |
//! | SlopeHigh | `y <= 1 - x / 2` | `(1, 2, 0) / sqrt 5` | X-, Y- | `0x77` |
//! | SlopeLow | `y <= 0.5 - x / 2` | `(1, 2, 0) / sqrt 5` | Y- | `0x33` |
//!
//! A 1:2 slope placed along +X is a SlopeHigh cell followed by a SlopeLow cell: the surface falls from the top
//! of the high cell's X- face to the bottom of the low cell's X+ edge. Corner-mask bit `i` is corner
//! `(i & 1, (i >> 1) & 1, (i >> 2) & 1)`; a corner is set when it is a vertex of the solid.
//!
//! An orientation `o` maps canonical vectors into the cell frame: `v_cell = v.x * M[0] + v.y * M[1] + v.z * M[2]`
//! with `M = orientation_matrix(o)`, rotating about the cell centre. Orientation bytes 24..=31 are invalid and
//! read as orientation 0; shape-kind bits 6 and 7 read as a cube (see [`CellShape::kind`]).
use super::{CellShape, Face, ShapeKind};
use crate::enums::Axis;
use std::sync::OnceLock;

/// Number of proper cube rotations; valid orientations are `0..ORIENTATION_COUNT`.
pub const ORIENTATION_COUNT: u8 = 24;

type Mat = [[i8; 3]; 3];

const fn det(m: &Mat) -> i32 {
    let a = m[0][0] as i32 * (m[1][1] as i32 * m[2][2] as i32 - m[1][2] as i32 * m[2][1] as i32);
    let b = m[0][1] as i32 * (m[1][0] as i32 * m[2][2] as i32 - m[1][2] as i32 * m[2][0] as i32);
    let c = m[0][2] as i32 * (m[1][0] as i32 * m[2][1] as i32 - m[1][1] as i32 * m[2][0] as i32);
    a - b + c
}

/// All 24 proper signed permutation matrices, in a fixed order: axis permutations in lexicographic order, then
/// sign patterns in binary order (bit k set = row k negated). Index 0 is the identity.
const fn build_table() -> [Mat; 24] {
    const PERMS: [[usize; 3]; 6] = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
    let mut out = [[[0i8; 3]; 3]; 24];
    let mut n = 0;
    let mut p = 0;
    while p < 6 {
        let mut signs = 0;
        while signs < 8 {
            let mut m = [[0i8; 3]; 3];
            let mut k = 0;
            while k < 3 {
                m[k][PERMS[p][k]] = if signs & (1 << k) != 0 { -1 } else { 1 };
                k += 1;
            }
            if det(&m) == 1 {
                out[n] = m;
                n += 1;
            }
            signs += 1;
        }
        p += 1;
    }
    out
}

static TABLE: [Mat; 24] = build_table();

pub fn orientation_matrix(orientation: u8) -> Mat {
    TABLE[if orientation < ORIENTATION_COUNT { orientation as usize } else { 0 }]
}

/// Inverse of [`orientation_matrix`]: the index of a proper signed permutation matrix, `None` for anything else.
pub fn orientation_index(m: [[i8; 3]; 3]) -> Option<u8> {
    TABLE.iter().position(|t| *t == m).map(|i| i as u8)
}

/// Apply `f` to every row of an orientation matrix and look the result up.
fn map_rows(o: u8, f: impl Fn([i8; 3]) -> [i8; 3]) -> u8 {
    let m = orientation_matrix(o);
    let r = [f(m[0]), f(m[1]), f(m[2])];
    // Every caller passes a proper rotation, so the product is always in the table.
    orientation_index(r).unwrap_or(0)
}

/// One +90 degree (right-handed) turn of a vector about `axis`.
fn quarter(axis: Axis, v: [i8; 3]) -> [i8; 3] {
    match axis {
        Axis::X => [v[0], -v[2], v[1]],
        Axis::Y => [v[2], v[1], -v[0]],
        Axis::Z => [-v[1], v[0], v[2]],
    }
}

pub fn rotate_orientation(orientation: u8, axis: Axis, quarter_turns: i8) -> u8 {
    let turns = (quarter_turns as i32).rem_euclid(4);
    map_rows(orientation, |mut v| {
        for _ in 0..turns {
            v = quarter(axis, v);
        }
        v
    })
}

pub fn mirror_shape_z(shape: CellShape) -> CellShape {
    let kind = shape.kind();
    // F (z -> -z) is improper, so compose with an improper symmetry P of the canonical solid: F * M * P is a
    // proper rotation that draws the mirrored solid. P is (axis it sends canonical X, Y, Z to, sign).
    let p: [(usize, i8); 3] = match kind {
        ShapeKind::Cube => return shape,
        // Solids that do not depend on z: reflect z.
        ShapeKind::Wedge | ShapeKind::SlopeHigh | ShapeKind::SlopeLow => [(0, 1), (1, 1), (2, -1)],
        // Solids symmetric in x and y: swap them.
        ShapeKind::Corner | ShapeKind::InverseCorner => [(1, 1), (0, 1), (2, 1)],
    };
    let m = orientation_matrix(shape.orientation());
    let mut r = [[0i8; 3]; 3];
    for k in 0..3 {
        let (j, s) = p[k];
        r[k] = [s * m[j][0], s * m[j][1], -s * m[j][2]];
    }
    CellShape::new(kind, orientation_index(r).unwrap_or(0))
}

/// Map a canonical vector into the cell frame.
fn apply_f(m: &Mat, v: [f64; 3]) -> [f64; 3] {
    let mut out = [0.0; 3];
    for k in 0..3 {
        for c in 0..3 {
            out[c] += v[k] * m[k][c] as f64;
        }
    }
    out
}

fn apply_i(m: &Mat, v: [i8; 3]) -> [i8; 3] {
    let mut out = [0i8; 3];
    for k in 0..3 {
        for c in 0..3 {
            out[c] += v[k] * m[k][c];
        }
    }
    out
}

fn face_vec(f: Face) -> [i8; 3] {
    let (x, y, z) = f.offset();
    [x as i8, y as i8, z as i8]
}

fn vec_face(v: [i8; 3]) -> Face {
    match v {
        [-1, 0, 0] => Face::XNeg,
        [1, 0, 0] => Face::XPos,
        [0, -1, 0] => Face::YNeg,
        [0, 1, 0] => Face::YPos,
        [0, 0, -1] => Face::ZNeg,
        _ => Face::ZPos,
    }
}

const XN: u8 = 1 << 0;
const YN: u8 = 1 << 2;
const ZN: u8 = 1 << 4;

fn canonical_full_faces(kind: ShapeKind) -> u8 {
    match kind {
        ShapeKind::Cube => 0x3F,
        ShapeKind::Wedge | ShapeKind::SlopeHigh => XN | YN,
        ShapeKind::Corner => 0,
        ShapeKind::InverseCorner => XN | YN | ZN,
        ShapeKind::SlopeLow => YN,
    }
}

pub fn full_face_mask(shape: CellShape) -> u8 {
    let canon = canonical_full_faces(shape.kind());
    if canon == 0x3F || canon == 0 {
        return canon;
    }
    let m = orientation_matrix(shape.orientation());
    let mut out = 0u8;
    for (i, f) in Face::ALL.iter().enumerate() {
        if canon & (1 << i) != 0 {
            let g = vec_face(apply_i(&m, face_vec(*f)));
            out |= 1 << (g as u8);
        }
    }
    out
}

fn canonical_corner_mask(kind: ShapeKind) -> u8 {
    match kind {
        ShapeKind::Cube => 0xFF,
        ShapeKind::Wedge | ShapeKind::SlopeHigh => 0x77,
        ShapeKind::Corner => 0x17,
        ShapeKind::InverseCorner => 0x7F,
        ShapeKind::SlopeLow => 0x33,
    }
}

/// Which of the cell's 8 corners are vertices of the shape's solid (bit layout as [`super::shape_from_corners`]).
/// Slopes also have vertices at edge midpoints, which the mask does not show.
pub fn shape_corner_mask(shape: CellShape) -> u8 {
    let canon = canonical_corner_mask(shape.kind());
    if canon == 0xFF {
        return canon;
    }
    let m = orientation_matrix(shape.orientation());
    let mut out = 0u8;
    for i in 0..8u8 {
        if canon & (1 << i) == 0 {
            continue;
        }
        // Corner relative to the cell centre, scaled to +-1, rotated, then back to a bit index.
        let v = [((i & 1) * 2) as i8 - 1, (((i >> 1) & 1) * 2) as i8 - 1, (((i >> 2) & 1) * 2) as i8 - 1];
        let w = apply_i(&m, v);
        let j = ((w[0] + 1) / 2) as u8 | ((((w[1] + 1) / 2) as u8) << 1) | ((((w[2] + 1) / 2) as u8) << 2);
        out |= 1 << j;
    }
    out
}

const INV_SQRT2: f64 = std::f64::consts::FRAC_1_SQRT_2;
const INV_SQRT3: f64 = 0.577_350_269_189_625_8;
const INV_SQRT5: f64 = 0.447_213_595_499_957_9;
const SQRT2: f64 = std::f64::consts::SQRT_2;
const SQRT3_2: f64 = 0.866_025_403_784_438_6;
const SQRT5_2: f64 = 1.118_033_988_749_895;

/// Every face of the canonical solid with positive area: (outward unit normal, area in cell-face units).
/// The sloped face is first.
fn canonical_surfaces(kind: ShapeKind) -> &'static [([f64; 3], f64)] {
    const X_N: [f64; 3] = [-1.0, 0.0, 0.0];
    const X_P: [f64; 3] = [1.0, 0.0, 0.0];
    const Y_N: [f64; 3] = [0.0, -1.0, 0.0];
    const Y_P: [f64; 3] = [0.0, 1.0, 0.0];
    const Z_N: [f64; 3] = [0.0, 0.0, -1.0];
    const Z_P: [f64; 3] = [0.0, 0.0, 1.0];
    const D3: [f64; 3] = [INV_SQRT3, INV_SQRT3, INV_SQRT3];
    const S5: [f64; 3] = [INV_SQRT5, 2.0 * INV_SQRT5, 0.0];
    match kind {
        ShapeKind::Cube => &[(X_N, 1.0), (X_P, 1.0), (Y_N, 1.0), (Y_P, 1.0), (Z_N, 1.0), (Z_P, 1.0)],
        ShapeKind::Wedge => &[([INV_SQRT2, INV_SQRT2, 0.0], SQRT2), (X_N, 1.0), (Y_N, 1.0), (Z_N, 0.5), (Z_P, 0.5)],
        ShapeKind::Corner => &[(D3, SQRT3_2), (X_N, 0.5), (Y_N, 0.5), (Z_N, 0.5)],
        ShapeKind::InverseCorner => &[(D3, SQRT3_2), (X_N, 1.0), (Y_N, 1.0), (Z_N, 1.0), (X_P, 0.5), (Y_P, 0.5), (Z_P, 0.5)],
        ShapeKind::SlopeHigh => &[(S5, SQRT5_2), (X_N, 1.0), (X_P, 0.5), (Y_N, 1.0), (Z_N, 0.75), (Z_P, 0.75)],
        ShapeKind::SlopeLow => &[(S5, SQRT5_2), (X_N, 0.5), (Y_N, 1.0), (Z_N, 0.25), (Z_P, 0.25)],
    }
}

/// The surface a shell travelling along `incoming` strikes: the face with the largest area projected against the
/// direction of travel. A shell meeting the sloped side gets the slope normal; one arriving from behind meets a
/// flat face instead, so obliquity is never computed against a face the shell cannot reach. With no usable
/// direction (zero, NaN, or nothing faces it) a shape returns its slope normal and a cube +Y.
pub fn armour_normal(shape: CellShape, incoming: [f64; 3]) -> [f64; 3] {
    let kind = shape.kind();
    let m = orientation_matrix(shape.orientation());
    let surfaces = canonical_surfaces(kind);
    let mut best: Option<([f64; 3], f64)> = None;
    for (n, area) in surfaces {
        let nc = apply_f(&m, *n);
        let facing = -(nc[0] * incoming[0] + nc[1] * incoming[1] + nc[2] * incoming[2]) * area;
        // NaN compares false, so a non-finite direction selects nothing.
        if facing > 0.0 && best.is_none_or(|(_, b)| facing > b) {
            best = Some((nc, facing));
        }
    }
    match best {
        Some((n, _)) => n,
        None if kind == ShapeKind::Cube => [0.0, 1.0, 0.0],
        None => apply_f(&m, surfaces[0].0),
    }
}

/// Solid volume of each loft kind in 1/24ths of a cell, used to break Hamming ties.
fn volume_24(kind: ShapeKind) -> i64 {
    match kind {
        ShapeKind::Cube => 24,
        ShapeKind::Wedge => 12,
        ShapeKind::Corner => 4,
        ShapeKind::InverseCorner => 20,
        ShapeKind::SlopeHigh => 18,
        ShapeKind::SlopeLow => 6,
    }
}

/// Sum of corner positions (centred, +-1 per axis) and the corner count of a mask.
fn corner_sum(mask: u8) -> ([i64; 3], i64) {
    let mut sum = [0i64; 3];
    for i in 0..8u8 {
        if mask & (1 << i) != 0 {
            sum[0] += ((i & 1) * 2) as i64 - 1;
            sum[1] += (((i >> 1) & 1) * 2) as i64 - 1;
            sum[2] += (((i >> 2) & 1) * 2) as i64 - 1;
        }
    }
    (sum, mask.count_ones() as i64)
}

/// Squared distance between the corner centroids of two masks, as an exact fraction (numerator, denominator).
fn centroid_gap(a: u8, b: u8) -> (i64, i64) {
    let ((sa, na), (sb, nb)) = (corner_sum(a), corner_sum(b));
    let num: i64 = (0..3).map(|k| (sa[k] * nb - sb[k] * na).pow(2)).sum();
    (num, (na * nb).pow(2))
}

/// Build the voxelizer table. Every rule is invariant under the cube's rotations and reflections, so a pattern
/// and its rotated or mirrored copy always get the same shape, rotated or mirrored (port and starboard facet
/// alike). For a pattern, the candidates are the cube and every distinct wedge, corner and inverse corner:
/// 1. least Hamming distance between the pattern and the candidate's corner vertices;
/// 2. the cube wins any tie it is part of (L1923, "ties to cube");
/// 3. then the candidate whose corner centroid is nearest the pattern's (puts a corner solid on the right apex);
/// 4. then the candidate whose volume is nearest the inside-corner fraction (a full face of four picks a wedge,
///    not a sliver);
/// 5. a tie that survives all of that is symmetric, and goes to the cube.
fn build_corner_table() -> [Option<CellShape>; 256] {
    // The voxelizer picks from the loft shapes only; 1:2 slopes are placed by hand.
    let mut candidates: Vec<(u8, CellShape)> = Vec::new();
    for kind in [ShapeKind::Wedge, ShapeKind::Corner, ShapeKind::InverseCorner] {
        for o in 0..ORIENTATION_COUNT {
            let s = CellShape::new(kind, o);
            let mask = shape_corner_mask(s);
            // Keep the lowest orientation that draws each distinct solid (masks identify solids for these kinds).
            if !candidates.iter().any(|(m, _)| *m == mask) {
                candidates.push((mask, s));
            }
        }
    }
    let mut table = [None; 256];
    for (pattern, slot) in table.iter_mut().enumerate().skip(1) {
        let pattern = pattern as u8;
        let dist = |mask: u8| (mask ^ pattern).count_ones();
        let best_d = candidates.iter().map(|(m, _)| dist(*m)).min().unwrap_or(u32::MAX);
        if dist(0xFF) <= best_d {
            *slot = Some(CellShape::CUBE);
            continue;
        }
        let fill_24 = pattern.count_ones() as i64 * 3;
        let mut best: Vec<((i64, i64), i64, CellShape)> = Vec::new();
        for (mask, shape) in candidates.iter().filter(|(m, _)| dist(*m) == best_d) {
            let gap = centroid_gap(*mask, pattern);
            let vol = (volume_24(shape.kind()) - fill_24).abs();
            let order = match best.first() {
                None => std::cmp::Ordering::Less,
                // Compare fractions by cross-multiplying, then volume.
                Some(((bn, bd), bv, _)) => (gap.0 * bd).cmp(&(bn * gap.1)).then(vol.cmp(bv)),
            };
            match order {
                std::cmp::Ordering::Less => best = vec![(gap, vol, *shape)],
                std::cmp::Ordering::Equal => best.push((gap, vol, *shape)),
                std::cmp::Ordering::Greater => {}
            }
        }
        *slot = Some(if best.len() == 1 { best[0].2 } else { CellShape::CUBE });
    }
    table
}

pub fn shape_from_corners(corners: u8) -> Option<CellShape> {
    static TABLE: OnceLock<[Option<CellShape>; 256]> = OnceLock::new();
    TABLE.get_or_init(build_corner_table)[corners as usize]
}
