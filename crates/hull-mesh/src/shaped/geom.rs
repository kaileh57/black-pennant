//! Shape solids and exact face-region arithmetic for the shaped pass.
use super::{axis, positive, Piece, ShapePolygon};
use crate::greedy::{face_normal, push_fan};
use crate::types::MeshData;
use content::cell::{Face, ShapeKind};

pub(super) type P2 = [f64; 2];

/// Common denominator of every crossing of two face cut lines, in cell units (lcm of 1..=16).
const SNAP: f64 = 720_720.0;
const EPS: f64 = 1e-9;

#[inline]
fn snap(v: f64) -> f64 { (v * SNAP).round() / SNAP }

/// Cut half-space of each canonical solid: the solid is the unit cell intersected with `n . p <= d`.
fn cut(kind: ShapeKind) -> Option<([f64; 3], f64)> {
    match kind {
        ShapeKind::Cube => None,
        ShapeKind::Wedge => Some(([1.0, 1.0, 0.0], 1.0)),
        ShapeKind::Corner => Some(([1.0, 1.0, 1.0], 1.0)),
        ShapeKind::InverseCorner => Some(([1.0, 1.0, 1.0], 2.0)),
        ShapeKind::SlopeHigh => Some(([1.0, 2.0, 0.0], 2.0)),
        ShapeKind::SlopeLow => Some(([1.0, 2.0, 0.0], 1.0)),
    }
}

fn sub3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] { [a[0] - b[0], a[1] - b[1], a[2] - b[2]] }
fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 { a[0] * b[0] + a[1] * b[1] + a[2] * b[2] }
fn cross3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] { [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]] }

/// Newell normal (twice the vector area); points toward the viewer for counter-clockwise order.
fn newell(pts: &[[f64; 3]]) -> [f64; 3] {
    let mut n = [0.0; 3];
    let o = pts.first().copied().unwrap_or([0.0; 3]);
    for i in 0..pts.len() {
        let c = cross3(sub3(pts[i], o), sub3(pts[(i + 1) % pts.len()], o));
        n = [n[0] + c[0], n[1] + c[1], n[2] + c[2]];
    }
    n
}

/// Order `pts` clockwise seen from the side `n` points to.
fn make_clockwise(pts: &mut [[f64; 3]], n: [f64; 3]) {
    if dot3(newell(pts), n) > 0.0 {
        pts.reverse();
    }
}

/// Unit square of cell face `f` (corners in 0..=1).
fn face_square(f: Face) -> Vec<[f64; 3]> {
    let a = axis(f);
    let w = if positive(f) { 1.0 } else { 0.0 };
    let (u, v) = ((a + 1) % 3, (a + 2) % 3);
    [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
        .iter()
        .map(|&(su, sv)| {
            let mut p = [0.0; 3];
            p[a] = w;
            p[u] = su;
            p[v] = sv;
            p
        })
        .collect()
}

/// Sutherland-Hodgman clip of a 3D polygon to `n . p <= d`.
fn clip3(poly: &[[f64; 3]], n: [f64; 3], d: f64) -> Vec<[f64; 3]> {
    let mut out = Vec::new();
    for i in 0..poly.len() {
        let (p, q) = (poly[i], poly[(i + 1) % poly.len()]);
        let (sp, sq) = (dot3(n, p) - d, dot3(n, q) - d);
        if sp <= EPS {
            out.push(p);
        }
        if (sp < -EPS && sq > EPS) || (sp > EPS && sq < -EPS) {
            let t = sp / (sp - sq);
            out.push([p[0] + t * (q[0] - p[0]), p[1] + t * (q[1] - p[1]), p[2] + t * (q[2] - p[2])]);
        }
    }
    dedup3(out)
}

fn dedup3(mut v: Vec<[f64; 3]>) -> Vec<[f64; 3]> {
    v.dedup_by(|a, b| sub3(*a, *b).iter().all(|c| c.abs() < EPS));
    while v.len() > 1 && sub3(v[0], v[v.len() - 1]).iter().all(|c| c.abs() < EPS) {
        v.pop();
    }
    v
}

fn to_f32(p: [f64; 3]) -> [f32; 3] { [snap(p[0]) as f32, snap(p[1]) as f32, snap(p[2]) as f32] }

fn normalised(n: [f64; 3]) -> [f32; 3] {
    let l = dot3(n, n).sqrt();
    [(n[0] / l) as f32, (n[1] / l) as f32, (n[2] / l) as f32]
}

/// Polygons of a canonical (orientation 0) solid, clockwise from outside: the cell-face polygons with positive
/// area, then the sloped polygon.
pub(super) fn canonical_polygons(kind: ShapeKind) -> Vec<ShapePolygon> {
    let c = cut(kind);
    let mut out = Vec::new();
    let mut on_cut: Vec<[f64; 3]> = Vec::new();
    for f in Face::ALL {
        let mut sq = face_square(f);
        let fnorm = face_normal(f).map(|v| v as f64);
        make_clockwise(&mut sq, fnorm);
        let poly = match c { Some((n, d)) => clip3(&sq, n, d), None => sq };
        if let Some((n, d)) = c {
            on_cut.extend(poly.iter().filter(|p| (dot3(n, **p) - d).abs() < EPS));
        }
        if poly.len() >= 3 && dot3(newell(&poly), newell(&poly)).sqrt() > EPS {
            out.push(ShapePolygon { corners: poly.into_iter().map(to_f32).collect(), normal: face_normal(f), on_face: Some(f) });
        }
    }
    if let Some((n, _)) = c {
        // The cut polygon: the cut-plane points of the clipped faces, sorted around the normal.
        let mut pts = dedup_all(on_cut);
        let k = pts.len() as f64;
        let ctr = pts.iter().fold([0.0; 3], |a, p| [a[0] + p[0] / k, a[1] + p[1] / k, a[2] + p[2] / k]);
        let e1 = sub3(pts[0], ctr);
        let e2 = cross3(n, e1);
        pts.sort_by(|a, b| {
            let ang = |p: &[f64; 3]| { let w = sub3(*p, ctr); libm::atan2(dot3(w, e2), dot3(w, e1)) };
            ang(a).total_cmp(&ang(b))
        });
        make_clockwise(&mut pts, n);
        out.push(ShapePolygon { corners: pts.into_iter().map(to_f32).collect(), normal: normalised(n), on_face: None });
    }
    out
}

fn dedup_all(v: Vec<[f64; 3]>) -> Vec<[f64; 3]> {
    let mut out: Vec<[f64; 3]> = Vec::new();
    for p in v {
        if !out.iter().any(|q| sub3(p, *q).iter().all(|c| c.abs() < EPS)) {
            out.push(p);
        }
    }
    out
}

fn face_of(v: [i8; 3]) -> Face {
    match v {
        [-1, 0, 0] => Face::XNeg,
        [1, 0, 0] => Face::XPos,
        [0, -1, 0] => Face::YNeg,
        [0, 1, 0] => Face::YPos,
        [0, 0, -1] => Face::ZNeg,
        _ => Face::ZPos,
    }
}

/// Rotate a canonical polygon by an orientation matrix about the cell centre (`v.x * M[0] + v.y * M[1] +
/// v.z * M[2]`, content's convention). Proper rotations keep the winding.
pub(super) fn rotate_polygon(p: &ShapePolygon, m: &[[i8; 3]; 3]) -> ShapePolygon {
    let rot = |v: [f32; 3]| {
        let mut r = [0.0f32; 3];
        for k in 0..3 {
            for c in 0..3 {
                r[c] += v[k] * m[k][c] as f32;
            }
        }
        r
    };
    let corners = p.corners.iter().map(|c| { let r = rot([c[0] - 0.5, c[1] - 0.5, c[2] - 0.5]); [r[0] + 0.5, r[1] + 0.5, r[2] + 0.5] }).collect();
    let on_face = p.on_face.map(|f| {
        let (x, y, z) = f.offset();
        let r = rot([x as f32, y as f32, z as f32]);
        face_of([r[0] as i8, r[1] as i8, r[2] as i8])
    });
    ShapePolygon { corners, normal: rot(p.normal), on_face }
}

/// 2D frame of a face along axis `a`: (p[a + 1], p[a + 2]) in cell units, the same for both cells of a pair.
pub(super) fn project(corners: &[[f32; 3]], a: usize) -> Vec<P2> {
    corners.iter().map(|c| [c[(a + 1) % 3] as f64, c[(a + 2) % 3] as f64]).collect()
}

pub(super) fn unit_square() -> Vec<P2> { vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]] }

/// Twice the signed area of a 2D polygon (positive counter-clockwise).
pub(super) fn signed_area2(p: &[P2]) -> f64 { area2(p) }

fn area2(p: &[P2]) -> f64 {
    let mut a = 0.0;
    for i in 0..p.len() {
        let (u, v) = (p[i], p[(i + 1) % p.len()]);
        a += u[0] * v[1] - v[0] * u[1];
    }
    a
}

#[inline]
fn side(a: P2, b: P2, p: P2) -> f64 { (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]) }

/// Keep the part of `poly` with `sign * side(a, b, p) >= 0`. Crossing points snap to the shared grid.
fn clip2(poly: &[P2], a: P2, b: P2, sign: f64) -> Vec<P2> {
    let mut out = Vec::with_capacity(poly.len() + 1);
    for i in 0..poly.len() {
        let (p, q) = (poly[i], poly[(i + 1) % poly.len()]);
        let (sp, sq) = (sign * side(a, b, p), sign * side(a, b, q));
        if sp >= -EPS {
            out.push(p);
        }
        if (sp < -EPS && sq > EPS) || (sp > EPS && sq < -EPS) {
            let t = sp / (sp - sq);
            out.push([snap(p[0] + t * (q[0] - p[0])), snap(p[1] + t * (q[1] - p[1]))]);
        }
    }
    cleanup(out)
}

/// Drop repeated and collinear vertices so the polygon is strictly convex (or empty).
fn cleanup(mut p: Vec<P2>) -> Vec<P2> {
    p.dedup_by(|a, b| (a[0] - b[0]).abs() < EPS && (a[1] - b[1]).abs() < EPS);
    while p.len() > 1 && (p[0][0] - p[p.len() - 1][0]).abs() < EPS && (p[0][1] - p[p.len() - 1][1]).abs() < EPS {
        p.pop();
    }
    let mut changed = true;
    while changed && p.len() >= 3 {
        changed = false;
        for i in 0..p.len() {
            let n = p.len();
            if side(p[(i + n - 1) % n], p[i], p[(i + 1) % n]).abs() < EPS {
                p.remove(i);
                changed = true;
                break;
            }
        }
    }
    if p.len() < 3 || area2(&p).abs() < EPS { Vec::new() } else { p }
}

/// Convex `a` minus convex `b`, as convex pieces appended to `out`.
pub(super) fn subtract(a: &[P2], b: &[P2], out: &mut Vec<Vec<P2>>) {
    let b = cleanup(b.to_vec());
    let mut rest = cleanup(a.to_vec());
    if b.is_empty() {
        if !rest.is_empty() {
            out.push(rest);
        }
        return;
    }
    // Inside of b is to the left of its edges when counter-clockwise.
    let inside = if area2(&b) > 0.0 { 1.0 } else { -1.0 };
    for i in 0..b.len() {
        if rest.is_empty() {
            return;
        }
        let (p, q) = (b[i], b[(i + 1) % b.len()]);
        let outside = clip2(&rest, p, q, -inside);
        if !outside.is_empty() {
            out.push(outside);
        }
        rest = clip2(&rest, p, q, inside);
    }
}

fn to_metres(c: f64) -> f32 { (c * 0.5) as f32 }

/// Largest polygon the shaped pass emits: a unit square clipped by one line has at most 5 corners, a table
/// polygon at most 6 (the hexagonal cut of a corner shape is 3).
const MAX_CORNERS: usize = 8;

/// Emit a table polygon of the cell at vessel cell `cell`.
pub(super) fn emit_unit_polygon(out: &mut MeshData, cell: [i64; 3], poly: &ShapePolygon) {
    let at = |c: &[f32; 3]| [to_metres(cell[0] as f64 + c[0] as f64), to_metres(cell[1] as f64 + c[1] as f64), to_metres(cell[2] as f64 + c[2] as f64)];
    if poly.corners.len() > MAX_CORNERS {
        let pts: Vec<[f32; 3]> = poly.corners.iter().map(at).collect();
        return push_fan(out, &pts, poly.normal);
    }
    let mut pts = [[0.0f32; 3]; MAX_CORNERS];
    for (dst, c) in pts.iter_mut().zip(&poly.corners) {
        *dst = at(c);
    }
    push_fan(out, &pts[..poly.corners.len()], poly.normal);
}

/// Emit a 2D piece lying on face `f` of the cell at vessel cell `cell`, clockwise about the face normal.
///
/// The face frame (p[a + 1], p[a + 2]) is right-handed about +a, so a counter-clockwise piece is clockwise seen
/// from the -a side: the corners are reversed when that disagrees with the face's side.
pub(super) fn emit_face_piece(out: &mut MeshData, cell: [i64; 3], f: Face, piece: &Piece) {
    let a = axis(f);
    let (u, v) = ((a + 1) % 3, (a + 2) % 3);
    let w = to_metres(cell[a] as f64 + if positive(f) { 1.0 } else { 0.0 });
    let at = |q: &P2| {
        let mut p = [0.0f32; 3];
        p[a] = w;
        p[u] = to_metres(cell[u] as f64 + q[0]);
        p[v] = to_metres(cell[v] as f64 + q[1]);
        p
    };
    let flip = piece.ccw == positive(f);
    if piece.pts.len() > MAX_CORNERS {
        let mut pts: Vec<[f32; 3]> = piece.pts.iter().map(at).collect();
        if flip {
            pts.reverse();
        }
        return push_fan(out, &pts, face_normal(f));
    }
    let mut buf = [[0.0f32; 3]; MAX_CORNERS];
    for (dst, q) in buf.iter_mut().zip(&piece.pts) {
        *dst = at(q);
    }
    let pts = &mut buf[..piece.pts.len()];
    if flip {
        pts.reverse();
    }
    push_fan(out, pts, face_normal(f));
}

/// Same vertex set (the regions of two solids on one face are equal).
pub(super) fn same_polygon(a: &[P2], b: &[P2]) -> bool {
    a.len() == b.len() && a.iter().all(|p| b.iter().any(|q| (p[0] - q[0]).abs() < EPS && (p[1] - q[1]).abs() < EPS))
}
