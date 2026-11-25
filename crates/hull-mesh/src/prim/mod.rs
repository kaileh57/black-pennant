//! Unit `hm-prim`. Faceted primitive builder shared by part models, VFX meshes and scenery.
//! DESIGN.md L1233 (bespoke models built procedurally from parameters), L1953 (VFX: faceted flat-normal meshes
//! of 20 to 80 triangles), L1815 (no smoothed normals), L1915 (faceted icosphere clouds).
//!
//! Every primitive is flat shaded (one normal per face, unshared vertices), clockwise from outside, and appends
//! to a [`MeshBuilder`] with the builder's current colour and transform. libm only for trig (deterministic
//! output across platforms is required because `cargo xtask gen` may bake these).
//!
//! # Conventions
//! - A front face winds clockwise seen from outside (Godot). Its stored normal is the unit vector
//!   `(c - a) x (b - a)` of any of its triangles `(a, b, c)`.
//! - Normals are computed from the transformed corners, so any affine [`Xform`] (rotation, non-uniform scale,
//!   mirror) keeps normals correct; a mirroring transform (negative determinant) reverses the corner order so
//!   faces stay front-facing outward.
//! - Degenerate faces (fewer than three distinct corners, zero area) and faces with non-finite corners are
//!   dropped, never emitted with a NaN normal. No primitive panics on bad sizes: negative extents are swapped
//!   or taken by magnitude and facet counts are clamped.
//! - `colors` is always written; `custom0` is written while [`MeshBuilder::custom`] is `Some` and zero-padded
//!   for vertices written while it was `None`, so every channel stays the length of `positions`.
use crate::types::MeshData;

pub(crate) mod kit;

/// Smallest squared doubled-area a face may have before it is dropped as degenerate (about 1e-6 m edges).
const MIN_AREA2: f32 = 1e-14;
/// Corners closer than this (metres) are merged when a face is emitted.
const MERGE_EPS: f32 = 1e-6;

/// Column-major 3x4 affine transform (rotation/scale columns then translation), f32.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Xform { pub basis: [[f32; 3]; 3], pub origin: [f32; 3] }
impl Xform {
    pub const IDENTITY: Xform = Xform { basis: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], origin: [0.0; 3] };
    pub fn translation(t: [f32; 3]) -> Xform { Xform { origin: t, ..Xform::IDENTITY } }
    pub fn apply(&self, p: [f32; 3]) -> [f32; 3] {
        let b = &self.basis;
        [b[0][0] * p[0] + b[1][0] * p[1] + b[2][0] * p[2] + self.origin[0],
         b[0][1] * p[0] + b[1][1] * p[1] + b[2][1] * p[2] + self.origin[1],
         b[0][2] * p[0] + b[1][2] * p[1] + b[2][2] * p[2] + self.origin[2]]
    }
    /// Rotation about +Y by `rad` (libm). Unit: `hm-prim`.
    ///
    /// Right-handed: +X turns toward -Z for positive `rad` (+Z starboard, so positive is toward port seen from
    /// above with +X forward).
    pub fn rot_y(rad: f32) -> Xform {
        let (s, c) = (libm::sinf(rad), libm::cosf(rad));
        Xform { basis: [[c, 0.0, -s], [0.0, 1.0, 0.0], [s, 0.0, c]], origin: [0.0; 3] }
    }
    /// Rotation about +Z (elevation for a bow-pointing barrel) by `rad`. Unit: `hm-prim`.
    ///
    /// Positive `rad` lifts +X toward +Y.
    pub fn rot_z(rad: f32) -> Xform {
        let (s, c) = (libm::sinf(rad), libm::cosf(rad));
        Xform { basis: [[c, s, 0.0], [-s, c, 0.0], [0.0, 0.0, 1.0]], origin: [0.0; 3] }
    }
    /// Rotation about +X by `rad`. Unit: `hm-prim`.
    ///
    /// Positive `rad` turns +Y toward +Z.
    pub fn rot_x(rad: f32) -> Xform {
        let (s, c) = (libm::sinf(rad), libm::cosf(rad));
        Xform { basis: [[1.0, 0.0, 0.0], [0.0, c, s], [0.0, -s, c]], origin: [0.0; 3] }
    }
    /// `self` after `other` (apply `other` first). Unit: `hm-prim`.
    pub fn then(&self, other: &Xform) -> Xform {
        Xform {
            basis: [self.apply_vec(other.basis[0]), self.apply_vec(other.basis[1]), self.apply_vec(other.basis[2])],
            origin: self.apply(other.origin),
        }
    }
    /// Non-uniform scale about the origin.
    pub(crate) fn scale(s: [f32; 3]) -> Xform {
        Xform { basis: [[s[0], 0.0, 0.0], [0.0, s[1], 0.0], [0.0, 0.0, s[2]]], origin: [0.0; 3] }
    }
    /// The basis applied to a direction (no translation).
    pub(crate) fn apply_vec(&self, v: [f32; 3]) -> [f32; 3] {
        let b = &self.basis;
        [b[0][0] * v[0] + b[1][0] * v[1] + b[2][0] * v[2],
         b[0][1] * v[0] + b[1][1] * v[1] + b[2][1] * v[2],
         b[0][2] * v[0] + b[1][2] * v[1] + b[2][2] * v[2]]
    }
    fn det(&self) -> f32 { dot(self.basis[0], cross(self.basis[1], self.basis[2])) }
}

// Small vector helpers (exact IEEE operations only, plus sqrt).
pub(crate) fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] { [a[0] + b[0], a[1] + b[1], a[2] + b[2]] }
pub(crate) fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] { [a[0] - b[0], a[1] - b[1], a[2] - b[2]] }
pub(crate) fn mul(a: [f32; 3], s: f32) -> [f32; 3] { [a[0] * s, a[1] * s, a[2] * s] }
pub(crate) fn dot(a: [f32; 3], b: [f32; 3]) -> f32 { a[0] * b[0] + a[1] * b[1] + a[2] * b[2] }
pub(crate) fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
pub(crate) fn length(a: [f32; 3]) -> f32 { dot(a, a).sqrt() }
/// Unit vector, or `fallback` for a zero or non-finite input.
pub(crate) fn normalize_or(a: [f32; 3], fallback: [f32; 3]) -> [f32; 3] {
    let l = length(a);
    if l > 1e-12 && l.is_finite() { mul(a, 1.0 / l) } else { fallback }
}
fn finite3(p: [f32; 3]) -> bool { p[0].is_finite() && p[1].is_finite() && p[2].is_finite() }
fn centroid(pts: &[[f32; 3]]) -> [f32; 3] {
    let mut c = [0.0f32; 3];
    for p in pts { c = add(c, *p); }
    if pts.is_empty() { c } else { mul(c, 1.0 / pts.len() as f32) }
}
/// Newell's polygon normal (unnormalised, length = twice the area) for counter-clockwise winding. The front
/// normal of a clockwise face is its negation.
fn newell(pts: &[[f32; 3]]) -> [f32; 3] {
    let mut n = [0.0f32; 3];
    for i in 0..pts.len() {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        n[0] += (a[1] - b[1]) * (a[2] + b[2]);
        n[1] += (a[2] - b[2]) * (a[0] + b[0]);
        n[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    n
}
/// Front normal (unnormalised) of a clockwise face.
fn front_normal(pts: &[[f32; 3]]) -> [f32; 3] { mul(newell(pts), -1.0) }
/// Unit-circle point `i` of `n` starting at angle `start` (libm trig).
fn ring_point(i: usize, n: usize, start: f32) -> (f32, f32) {
    let a = start + core::f32::consts::TAU * i as f32 / n as f32;
    (libm::cosf(a), libm::sinf(a))
}

/// Appends flat-shaded primitives to a mesh.
pub struct MeshBuilder {
    pub mesh: MeshData,
    /// Linear RGBA written to `colors` for every new vertex.
    pub color: [f32; 4],
    /// Written to `custom0` for every new vertex when `Some`.
    pub custom: Option<[f32; 4]>,
    pub xform: Xform,
}
impl MeshBuilder {
    pub fn new() -> Self { Self { mesh: MeshData::default(), color: [1.0; 4], custom: None, xform: Xform::IDENTITY } }
    pub fn finish(self) -> MeshData { self.mesh }

    /// Emit one face given in the builder's local frame, clockwise from the front. Applies the transform,
    /// keeps winding under mirrors, merges coincident corners and drops degenerate or non-finite faces.
    fn emit(&mut self, local: &[[f32; 3]]) {
        if local.len() < 3 { return; }
        let mut pts: Vec<[f32; 3]> = Vec::with_capacity(local.len());
        for p in local {
            let q = self.xform.apply(*p);
            if !finite3(q) { return; }
            if let Some(last) = pts.last() { if length(sub(q, *last)) <= MERGE_EPS { continue; } }
            pts.push(q);
        }
        while pts.len() > 1 && length(sub(pts[0], pts[pts.len() - 1])) <= MERGE_EPS { pts.pop(); }
        if pts.len() < 3 { return; }
        if self.xform.det() < 0.0 { pts.reverse(); }
        let n = front_normal(&pts);
        let l2 = dot(n, n);
        if l2.is_nan() || l2 <= MIN_AREA2 || !l2.is_finite() { return; }
        let n = mul(n, 1.0 / l2.sqrt());
        if pts.len() > 3 && !self.fan_is_planar(&pts, n) {
            // A non-planar face (a twisted loft quad) is split into its fan triangles, each with its own normal,
            // so no triangle carries a normal that disagrees with its winding.
            for i in 1..pts.len() - 1 {
                let tri = [pts[0], pts[i], pts[i + 1]];
                let t = front_normal(&tri);
                let l2 = dot(t, t);
                if l2 > MIN_AREA2 && l2.is_finite() { self.push_face(&tri, mul(t, 1.0 / l2.sqrt())); }
            }
            return;
        }
        self.push_face(&pts, n);
    }

    /// True when every fan triangle of `pts` lies within about 1.8 degrees of the face normal `n`.
    fn fan_is_planar(&self, pts: &[[f32; 3]], n: [f32; 3]) -> bool {
        (1..pts.len() - 1).all(|i| {
            let t = front_normal(&[pts[0], pts[i], pts[i + 1]]);
            let l = length(t);
            l <= 1e-7 || dot(t, n) >= 0.9995 * l
        })
    }

    fn push_face(&mut self, pts: &[[f32; 3]], n: [f32; 3]) {
        let m = &mut self.mesh;
        let base = m.positions.len();
        if self.custom.is_some() && m.custom0.len() < base { m.custom0.resize(base, [0.0; 4]); }
        let write_custom = self.custom.is_some() || !m.custom0.is_empty();
        for p in pts {
            m.positions.push(*p);
            m.normals.push(n);
            m.colors.push(self.color);
            if write_custom { m.custom0.push(self.custom.unwrap_or([0.0; 4])); }
        }
        let base = base as u32;
        for i in 1..(pts.len() as u32 - 1) {
            m.indices.extend_from_slice(&[base, base + i, base + i + 1]);
        }
    }

    /// Emit a convex face (local frame) whose front faces along `outward`; the corner order is flipped when it
    /// winds the other way.
    pub(crate) fn face(&mut self, corners: &[[f32; 3]], outward: [f32; 3]) {
        if corners.len() < 3 { return; }
        if dot(front_normal(corners), outward) < 0.0 {
            let rev: Vec<[f32; 3]> = corners.iter().rev().copied().collect();
            self.emit(&rev);
        } else {
            self.emit(corners);
        }
    }

    /// Run `f` with `x` applied inside the current transform, then restore the transform.
    pub(crate) fn with<F: FnOnce(&mut MeshBuilder)>(&mut self, x: Xform, f: F) {
        let saved = self.xform;
        self.xform = saved.then(&x);
        f(self);
        self.xform = saved;
    }

    /// One planar polygon (3+ coplanar corners, clockwise from the front), fan-triangulated, normal from the
    /// first three corners. Unit: `hm-prim`.
    ///
    /// The normal is Newell's normal over all corners, which equals the first-three-corner normal for a
    /// planar polygon and stays defined when the first three corners happen to be collinear.
    pub fn polygon(&mut self, corners: &[[f32; 3]]) { self.emit(corners); }

    /// Axis-aligned box from `min` to `max`. Unit: `hm-prim`.
    pub fn cuboid(&mut self, min: [f32; 3], max: [f32; 3]) {
        let (x0, x1) = (min[0].min(max[0]), min[0].max(max[0]));
        let (y0, y1) = (min[1].min(max[1]), min[1].max(max[1]));
        let (z0, z1) = (min[2].min(max[2]), min[2].max(max[2]));
        self.face(&[[x1, y0, z0], [x1, y1, z0], [x1, y1, z1], [x1, y0, z1]], [1.0, 0.0, 0.0]);
        self.face(&[[x0, y0, z0], [x0, y1, z0], [x0, y1, z1], [x0, y0, z1]], [-1.0, 0.0, 0.0]);
        self.face(&[[x0, y1, z0], [x1, y1, z0], [x1, y1, z1], [x0, y1, z1]], [0.0, 1.0, 0.0]);
        self.face(&[[x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]], [0.0, -1.0, 0.0]);
        self.face(&[[x0, y0, z1], [x1, y0, z1], [x1, y1, z1], [x0, y1, z1]], [0.0, 0.0, 1.0]);
        self.face(&[[x0, y0, z0], [x1, y0, z0], [x1, y1, z0], [x0, y1, z0]], [0.0, 0.0, -1.0]);
    }

    /// Prism with a regular `sides`-gon cross-section (3..=16) along +Y from `y0` to `y1`, centred on (cx, cz),
    /// circumradius `r`; caps optional. Hexagonal splash columns use `sides = 6`. Unit: `hm-prim`.
    ///
    /// The first edge faces +X (corners at angles pi/n + 2 pi k/n), so a 4-sided prism is an axis-aligned box.
    #[allow(clippy::too_many_arguments)]
    pub fn prism_y(&mut self, cx: f32, cz: f32, r: f32, y0: f32, y1: f32, sides: u8, caps: bool) {
        let n = sides.clamp(3, 16) as usize;
        let r = r.abs();
        let (y0, y1) = (y0.min(y1), y0.max(y1));
        let start = core::f32::consts::PI / n as f32;
        let ring: Vec<(f32, f32)> = (0..n).map(|i| ring_point(i, n, start)).collect();
        for i in 0..n {
            let (c0, s0) = ring[i];
            let (c1, s1) = ring[(i + 1) % n];
            let out = [c0 + c1, 0.0, s0 + s1];
            self.face(&[[cx + r * c0, y0, cz + r * s0], [cx + r * c1, y0, cz + r * s1],
                        [cx + r * c1, y1, cz + r * s1], [cx + r * c0, y1, cz + r * s0]], out);
        }
        if caps {
            let top: Vec<[f32; 3]> = ring.iter().map(|&(c, s)| [cx + r * c, y1, cz + r * s]).collect();
            let bot: Vec<[f32; 3]> = ring.iter().map(|&(c, s)| [cx + r * c, y0, cz + r * s]).collect();
            self.face(&top, [0.0, 1.0, 0.0]);
            self.face(&bot, [0.0, -1.0, 0.0]);
        }
    }

    /// Frustum along an arbitrary axis from `a` (radius `ra`) to `b` (radius `rb`), `sides` facets, caps
    /// optional. Barrels, funnels, missile bodies, masts. Unit: `hm-prim`.
    ///
    /// `sides` is clamped to 3..=32. A flat facet faces "up" (the world +Y projected off the axis, or +X for a
    /// near-vertical axis), so horizontal barrels show a flat top. A zero radius makes a cone point (that end
    /// gets no cap).
    pub fn frustum(&mut self, a: [f32; 3], b: [f32; 3], ra: f32, rb: f32, sides: u8, caps: bool) {
        let n = sides.clamp(3, 32) as usize;
        let (ra, rb) = (ra.abs(), rb.abs());
        if !finite3(a) || !finite3(b) || !ra.is_finite() || !rb.is_finite() { return; }
        let axis = sub(b, a);
        let len = length(axis);
        if len.is_nan() || len <= 1e-9 { return; }
        let d = mul(axis, 1.0 / len);
        let reference = if d[1].abs() > 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
        let v = normalize_or(sub(reference, mul(d, dot(d, reference))), [0.0, 0.0, 1.0]);
        let u = cross(v, d);
        let start = core::f32::consts::FRAC_PI_2 + core::f32::consts::PI / n as f32;
        let ring: Vec<[f32; 3]> = (0..n).map(|i| { let (c, s) = ring_point(i, n, start); add(mul(u, c), mul(v, s)) }).collect();
        let at = |o: [f32; 3], r: f32, k: usize| add(o, mul(ring[k], r));
        for i in 0..n {
            let j = (i + 1) % n;
            let out = add(ring[i], ring[j]);
            self.face(&[at(a, ra, i), at(a, ra, j), at(b, rb, j), at(b, rb, i)], out);
        }
        if caps {
            if ra > 1e-6 { let cap: Vec<[f32; 3]> = (0..n).map(|k| at(a, ra, k)).collect(); self.face(&cap, mul(d, -1.0)); }
            if rb > 1e-6 { let cap: Vec<[f32; 3]> = (0..n).map(|k| at(b, rb, k)).collect(); self.face(&cap, d); }
        }
    }

    /// Icosphere of `subdiv` 0..=2 subdivisions (20, 80, 320 faces) at `c` with radius `r`. Unit: `hm-prim`.
    pub fn icosphere(&mut self, c: [f32; 3], r: f32, subdiv: u8) {
        let r = r.abs();
        let (verts, faces) = icosphere_unit(subdiv.min(2));
        for f in &faces {
            let tri = [add(c, mul(verts[f[0]], r)), add(c, mul(verts[f[1]], r)), add(c, mul(verts[f[2]], r))];
            let out = add(add(verts[f[0]], verts[f[1]]), verts[f[2]]);
            self.face(&tri, out);
        }
    }

    /// Flat ring (annulus) in the XZ plane at height `y`, facing +Y, `sides` segments. Unit: `hm-prim`.
    ///
    /// `sides` is clamped to 3..=64; an inner radius of 0 makes a disc of triangles.
    pub fn ring_y(&mut self, cx: f32, cz: f32, r_in: f32, r_out: f32, y: f32, sides: u8) {
        let n = sides.clamp(3, 64) as usize;
        let (a, b) = (r_in.abs(), r_out.abs());
        let (ri, ro) = (a.min(b), a.max(b));
        let start = core::f32::consts::PI / n as f32;
        for i in 0..n {
            let (c0, s0) = ring_point(i, n, start);
            let (c1, s1) = ring_point(i + 1, n, start);
            self.face(&[[cx + ri * c0, y, cz + ri * s0], [cx + ro * c0, y, cz + ro * s0],
                        [cx + ro * c1, y, cz + ro * s1], [cx + ri * c1, y, cz + ri * s1]], [0.0, 1.0, 0.0]);
        }
    }

    /// Extrude a closed convex or simple polygon (XY plane, clockwise seen from +Z) along +Z by `depth`.
    /// Fins, shields, rudders, radar faces. Unit: `hm-prim`.
    ///
    /// Either winding is accepted (the outline's orientation is measured). Concave simple outlines are
    /// ear-clipped; a negative `depth` extrudes toward -Z. Caps sit at z = 0 and z = `depth`.
    pub fn extrude_z(&mut self, outline: &[[f32; 2]], depth: f32) {
        let mut pts: Vec<[f32; 2]> = Vec::with_capacity(outline.len());
        for p in outline {
            if !p[0].is_finite() || !p[1].is_finite() { return; }
            if let Some(l) = pts.last() { if (p[0] - l[0]).abs() <= MERGE_EPS && (p[1] - l[1]).abs() <= MERGE_EPS { continue; } }
            pts.push(*p);
        }
        while pts.len() > 1 && (pts[0][0] - pts[pts.len() - 1][0]).abs() <= MERGE_EPS && (pts[0][1] - pts[pts.len() - 1][1]).abs() <= MERGE_EPS { pts.pop(); }
        if pts.len() < 3 || !depth.is_finite() { return; }
        let area = signed_area(&pts);
        if area.is_nan() || area.abs() <= 1e-12 { return; }
        if area < 0.0 { pts.reverse(); } // now counter-clockwise in the XY plane seen from +Z
        let (z0, z1) = (depth.min(0.0), depth.max(0.0));
        let n = pts.len();
        for i in 0..n {
            let a = pts[i];
            let b = pts[(i + 1) % n];
            let out = [b[1] - a[1], a[0] - b[0], 0.0];
            self.face(&[[a[0], a[1], z0], [b[0], b[1], z0], [b[0], b[1], z1], [a[0], a[1], z1]], out);
        }
        if is_convex(&pts) {
            let top: Vec<[f32; 3]> = pts.iter().map(|p| [p[0], p[1], z1]).collect();
            let bot: Vec<[f32; 3]> = pts.iter().map(|p| [p[0], p[1], z0]).collect();
            self.face(&top, [0.0, 0.0, 1.0]);
            self.face(&bot, [0.0, 0.0, -1.0]);
        } else {
            for t in ear_clip(&pts) {
                let (a, b, c) = (pts[t[0]], pts[t[1]], pts[t[2]]);
                self.face(&[[a[0], a[1], z1], [b[0], b[1], z1], [c[0], c[1], z1]], [0.0, 0.0, 1.0]);
                self.face(&[[a[0], a[1], z0], [b[0], b[1], z0], [c[0], c[1], z0]], [0.0, 0.0, -1.0]);
            }
        }
    }

    /// Two crossed vertical quads (missile trail segments, L1966). Unit: `hm-prim`.
    ///
    /// Both quads run along the local X axis from x = 0 to x = 1 (the trail-segment convention of
    /// `vfx::meshes`: an instance transform stretches the segment between two trail points) and cross on it:
    /// one in the XY plane spanning y = -`half_h`..`half_h`, one in the XZ plane spanning z = -`half_w`..`half_w`.
    /// Each quad is emitted front and back (8 triangles) so a blended trail reads from either side with
    /// back-face culling on.
    pub fn cross_quads(&mut self, half_w: f32, half_h: f32) {
        let (w, h) = (half_w.abs(), half_h.abs());
        let xy = [[0.0, -h, 0.0], [1.0, -h, 0.0], [1.0, h, 0.0], [0.0, h, 0.0]];
        let xz = [[0.0, 0.0, -w], [1.0, 0.0, -w], [1.0, 0.0, w], [0.0, 0.0, w]];
        self.face(&xy, [0.0, 0.0, 1.0]);
        self.face(&xy, [0.0, 0.0, -1.0]);
        self.face(&xz, [0.0, 1.0, 0.0]);
        self.face(&xz, [0.0, -1.0, 0.0]);
    }

    /// Lattice beam between two points: a thin square-section box, for lattice masts and cranes. Unit: `hm-prim`.
    ///
    /// The section is `2 * half_w` square with one face toward "up" (see [`MeshBuilder::frustum`]); 12 triangles.
    pub fn beam(&mut self, a: [f32; 3], b: [f32; 3], half_w: f32) {
        self.frustum(a, b, half_w.abs() * core::f32::consts::SQRT_2, half_w.abs() * core::f32::consts::SQRT_2, 4, true);
    }

    /// Open-ended square-section member (8 triangles) for lattice bracing whose ends are buried in other members.
    pub(crate) fn strut(&mut self, a: [f32; 3], b: [f32; 3], half_w: f32) {
        self.frustum(a, b, half_w.abs() * core::f32::consts::SQRT_2, half_w.abs() * core::f32::consts::SQRT_2, 4, false);
    }

    /// Loft between consecutive rings of corresponding corners (each ring a closed, star-shaped loop around its
    /// centroid, all rings the same length >= 3). Side faces are oriented away from the axis between the two
    /// ring centroids; the end caps (optional) away from the neighbouring ring. A ring collapsed to one point
    /// makes a cone tip. Gunhouses, domes, sonar domes, faceted housings.
    pub(crate) fn loft(&mut self, rings: &[Vec<[f32; 3]>], cap_start: bool, cap_end: bool) {
        if rings.len() < 2 { return; }
        let n = rings[0].len();
        if n < 3 || rings.iter().any(|r| r.len() != n) { return; }
        let cents: Vec<[f32; 3]> = rings.iter().map(|r| centroid(r)).collect();
        for k in 0..rings.len() - 1 {
            let (r0, r1) = (&rings[k], &rings[k + 1]);
            let inside = mul(add(cents[k], cents[k + 1]), 0.5);
            for i in 0..n {
                let j = (i + 1) % n;
                let quad = [r0[i], r0[j], r1[j], r1[i]];
                let out = sub(centroid(&quad), inside);
                self.face(&quad, out);
            }
        }
        let last = rings.len() - 1;
        if cap_start { self.face(&rings[0], sub(cents[0], cents[1])); }
        if cap_end { self.face(&rings[last], sub(cents[last], cents[last - 1])); }
    }
}
impl Default for MeshBuilder { fn default() -> Self { Self::new() } }

/// Twice the signed area of a 2D polygon (positive = counter-clockwise).
fn signed_area(p: &[[f32; 2]]) -> f32 {
    let mut s = 0.0f32;
    for i in 0..p.len() {
        let a = p[i];
        let b = p[(i + 1) % p.len()];
        s += a[0] * b[1] - b[0] * a[1];
    }
    s
}

fn cross2(o: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 { (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]) }

/// Convex test for a counter-clockwise polygon (collinear corners allowed).
fn is_convex(p: &[[f32; 2]]) -> bool {
    let n = p.len();
    (0..n).all(|i| cross2(p[i], p[(i + 1) % n], p[(i + 2) % n]) >= -1e-9)
}

/// Ear-clipping triangulation of a simple counter-clockwise polygon; falls back to a fan for the remainder if
/// the outline self-intersects.
fn ear_clip(p: &[[f32; 2]]) -> Vec<[usize; 3]> {
    let mut idx: Vec<usize> = (0..p.len()).collect();
    let mut tris = Vec::with_capacity(p.len().saturating_sub(2));
    while idx.len() > 3 {
        let m = idx.len();
        let mut clipped = false;
        for k in 0..m {
            let (ia, ib, ic) = (idx[(k + m - 1) % m], idx[k], idx[(k + 1) % m]);
            let (a, b, c) = (p[ia], p[ib], p[ic]);
            if cross2(a, b, c) <= 1e-12 { continue; }
            let blocked = idx.iter().any(|&q| {
                if q == ia || q == ib || q == ic { return false; }
                let t = p[q];
                cross2(a, b, t) >= 0.0 && cross2(b, c, t) >= 0.0 && cross2(c, a, t) >= 0.0
            });
            if blocked { continue; }
            tris.push([ia, ib, ic]);
            idx.remove(k);
            clipped = true;
            break;
        }
        if !clipped { break; }
    }
    for k in 1..idx.len().saturating_sub(1) { tris.push([idx[0], idx[k], idx[k + 1]]); }
    tris
}

/// Unit icosphere: vertex positions and triangle corner indices.
fn icosphere_unit(subdiv: u8) -> (Vec<[f32; 3]>, Vec<[usize; 3]>) {
    let t = (1.0 + 5.0f32.sqrt()) * 0.5;
    let mut verts: Vec<[f32; 3]> = [
        [-1.0, t, 0.0], [1.0, t, 0.0], [-1.0, -t, 0.0], [1.0, -t, 0.0],
        [0.0, -1.0, t], [0.0, 1.0, t], [0.0, -1.0, -t], [0.0, 1.0, -t],
        [t, 0.0, -1.0], [t, 0.0, 1.0], [-t, 0.0, -1.0], [-t, 0.0, 1.0],
    ].iter().map(|v| normalize_or(*v, [0.0, 1.0, 0.0])).collect();
    let mut faces: Vec<[usize; 3]> = vec![
        [0, 11, 5], [0, 5, 1], [0, 1, 7], [0, 7, 10], [0, 10, 11],
        [1, 5, 9], [5, 11, 4], [11, 10, 2], [10, 7, 6], [7, 1, 8],
        [3, 9, 4], [3, 4, 2], [3, 2, 6], [3, 6, 8], [3, 8, 9],
        [4, 9, 5], [2, 4, 11], [6, 2, 10], [8, 6, 7], [9, 8, 1],
    ];
    for _ in 0..subdiv {
        // Midpoints keyed by the sorted edge, looked up linearly in insertion order (deterministic, small).
        let mut mids: Vec<((usize, usize), usize)> = Vec::new();
        let mut mid = |a: usize, b: usize, verts: &mut Vec<[f32; 3]>| -> usize {
            let key = (a.min(b), a.max(b));
            if let Some(&(_, i)) = mids.iter().find(|(k, _)| *k == key) { return i; }
            let m = normalize_or(mul(add(verts[a], verts[b]), 0.5), [0.0, 1.0, 0.0]);
            verts.push(m);
            mids.push((key, verts.len() - 1));
            verts.len() - 1
        };
        let mut next = Vec::with_capacity(faces.len() * 4);
        for f in &faces {
            let ab = mid(f[0], f[1], &mut verts);
            let bc = mid(f[1], f[2], &mut verts);
            let ca = mid(f[2], f[0], &mut verts);
            next.extend_from_slice(&[[f[0], ab, ca], [f[1], bc, ab], [f[2], ca, bc], [ab, bc, ca]]);
        }
        faces = next;
    }
    (verts, faces)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3], eps: f32) -> bool { length(sub(a, b)) <= eps }

    /// Signed volume by the divergence theorem with the clockwise-front convention.
    pub(crate) fn volume(m: &MeshData) -> f32 {
        let mut v = 0.0f32;
        for t in m.indices.chunks(3) {
            let (a, b, c) = (m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]);
            v += dot(a, cross(c, b)) / 6.0;
        }
        v
    }

    /// Every triangle's stored normal is unit, matches its clockwise winding, and channels line up.
    pub(crate) fn check_consistent(m: &MeshData) {
        assert_eq!(m.normals.len(), m.positions.len());
        assert_eq!(m.colors.len(), m.positions.len());
        assert!(m.custom0.is_empty() || m.custom0.len() == m.positions.len());
        assert_eq!(m.indices.len() % 3, 0);
        assert!(m.positions.iter().chain(m.normals.iter()).all(|p| finite3(*p)), "non-finite vertex data");
        for t in m.indices.chunks(3) {
            assert!(t.iter().all(|&i| (i as usize) < m.positions.len()));
            let (a, b, c) = (m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]);
            let n = m.normals[t[0] as usize];
            assert!((length(n) - 1.0).abs() < 1e-4, "normal not unit: {n:?}");
            assert_eq!(n, m.normals[t[1] as usize]);
            assert_eq!(n, m.normals[t[2] as usize]);
            let w = cross(sub(c, a), sub(b, a));
            if length(w) > 1e-6 { assert!(dot(normalize_or(w, n), n) > 0.99, "winding disagrees with normal"); }
        }
    }

    /// Every face of a convex closed solid faces away from `inside`.
    fn check_outward(m: &MeshData, inside: [f32; 3]) {
        for t in m.indices.chunks(3) {
            let c = centroid(&[m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]]);
            assert!(dot(sub(c, inside), m.normals[t[0] as usize]) > 0.0, "inward face at {c:?}");
        }
    }

    #[test]
    fn rotations_follow_documented_handedness() {
        assert!(close(Xform::rot_y(core::f32::consts::FRAC_PI_2).apply([1.0, 0.0, 0.0]), [0.0, 0.0, -1.0], 1e-6));
        assert!(close(Xform::rot_z(core::f32::consts::FRAC_PI_2).apply([1.0, 0.0, 0.0]), [0.0, 1.0, 0.0], 1e-6));
        assert!(close(Xform::rot_x(core::f32::consts::FRAC_PI_2).apply([0.0, 1.0, 0.0]), [0.0, 0.0, 1.0], 1e-6));
        let t = Xform::translation([1.0, 2.0, 3.0]).then(&Xform::rot_z(core::f32::consts::FRAC_PI_2));
        assert!(close(t.apply([1.0, 0.0, 0.0]), [1.0, 3.0, 3.0], 1e-6), "then applies the right-hand side first");
        let r = Xform::rot_y(0.3).then(&Xform::rot_y(0.4));
        assert!(close(r.apply([1.0, 0.0, 0.0]), Xform::rot_y(0.7).apply([1.0, 0.0, 0.0]), 1e-6));
        assert!((Xform::rot_x(0.7).det() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cuboid_is_closed_outward_and_has_the_right_volume() {
        let mut b = MeshBuilder::new();
        b.cuboid([2.0, 1.0, 3.0], [0.0, 0.0, 0.0]);
        let m = b.finish();
        assert_eq!(m.triangle_count(), 12);
        assert_eq!(m.vertex_count(), 24);
        check_consistent(&m);
        check_outward(&m, [1.0, 0.5, 1.5]);
        assert!((volume(&m) - 6.0).abs() < 1e-4);
    }

    #[test]
    fn prism_and_frustum_counts_orientation_and_volume() {
        let mut b = MeshBuilder::new();
        b.prism_y(1.0, -1.0, 2.0, 0.0, 3.0, 6, true);
        let m = b.finish();
        assert_eq!(m.triangle_count(), 6 * 2 + 2 * 4);
        check_consistent(&m);
        check_outward(&m, [1.0, 1.5, -1.0]);
        let hex_area = 1.5 * 3.0f32.sqrt() * 4.0;
        assert!((volume(&m) - hex_area * 3.0).abs() < 1e-3);

        // A 4-sided prism is an axis-aligned box.
        let mut b = MeshBuilder::new();
        b.prism_y(0.0, 0.0, core::f32::consts::SQRT_2, 0.0, 1.0, 4, true);
        let bb = b.finish().aabb();
        assert!(close(bb.min, [-1.0, 0.0, -1.0], 1e-5) && close(bb.max, [1.0, 1.0, 1.0], 1e-5));

        for (a, bpt) in [([0.0, 0.0, 0.0], [3.0, 0.0, 0.0]), ([0.0, 0.0, 0.0], [0.0, 2.0, 0.0]), ([1.0, 2.0, 3.0], [-1.0, 0.5, 4.0])] {
            let mut b = MeshBuilder::new();
            b.frustum(a, bpt, 0.5, 0.5, 8, true);
            let m = b.finish();
            assert_eq!(m.triangle_count(), 8 * 2 + 2 * 6);
            check_consistent(&m);
            check_outward(&m, mul(add(a, bpt), 0.5));
            let oct_area = 2.0 * core::f32::consts::SQRT_2 * 0.25;
            assert!((volume(&m) - oct_area * length(sub(bpt, a))).abs() < 1e-3);
        }
        // Cone: zero tip radius gives triangles and no tip cap.
        let mut b = MeshBuilder::new();
        b.frustum([0.0; 3], [0.0, 0.0, 2.0], 1.0, 0.0, 6, true);
        let m = b.finish();
        assert_eq!(m.triangle_count(), 6 + 4);
        check_consistent(&m);
        check_outward(&m, [0.0, 0.0, 0.5]);
    }

    #[test]
    fn horizontal_barrels_have_a_flat_top() {
        let mut b = MeshBuilder::new();
        b.frustum([0.0; 3], [2.0, 0.0, 0.0], 1.0, 1.0, 8, false);
        let m = b.finish();
        assert!(m.normals.iter().any(|n| close(*n, [0.0, 1.0, 0.0], 1e-5)));
    }

    #[test]
    fn icosphere_face_counts_and_outward_normals() {
        for (s, faces) in [(0u8, 20usize), (1, 80), (2, 320), (9, 320)] {
            let mut b = MeshBuilder::new();
            b.icosphere([1.0, 2.0, 3.0], 2.0, s);
            let m = b.finish();
            assert_eq!(m.triangle_count(), faces);
            check_consistent(&m);
            check_outward(&m, [1.0, 2.0, 3.0]);
            let sphere = 4.0 / 3.0 * core::f32::consts::PI * 8.0;
            let v = volume(&m);
            assert!(v > 0.5 * sphere && v < sphere);
            for p in &m.positions { assert!((length(sub(*p, [1.0, 2.0, 3.0])) - 2.0).abs() < 1e-4); }
        }
    }

    #[test]
    fn ring_faces_up_and_disc_degenerates_cleanly() {
        let mut b = MeshBuilder::new();
        b.ring_y(0.0, 0.0, 0.5, 1.0, 0.2, 12);
        let m = b.finish();
        assert_eq!(m.triangle_count(), 24);
        check_consistent(&m);
        assert!(m.normals.iter().all(|n| close(*n, [0.0, 1.0, 0.0], 1e-5)));
        let mut b = MeshBuilder::new();
        b.ring_y(0.0, 0.0, 0.0, 1.0, 0.0, 8);
        let m = b.finish();
        assert_eq!(m.triangle_count(), 8);
        check_consistent(&m);
    }

    #[test]
    fn extrusion_handles_both_windings_and_concave_outlines() {
        let square_cw = [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];
        let square_ccw = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        for o in [&square_cw[..], &square_ccw[..]] {
            let mut b = MeshBuilder::new();
            b.extrude_z(o, 2.0);
            let m = b.finish();
            assert_eq!(m.triangle_count(), 12);
            check_consistent(&m);
            check_outward(&m, [0.5, 0.5, 1.0]);
            assert!((volume(&m) - 2.0).abs() < 1e-4);
        }
        // L-shape (area 3) extruded toward -Z.
        let l = [[0.0, 0.0], [2.0, 0.0], [2.0, 1.0], [1.0, 1.0], [1.0, 2.0], [0.0, 2.0]];
        let mut b = MeshBuilder::new();
        b.extrude_z(&l, -0.5);
        let m = b.finish();
        check_consistent(&m);
        assert!((volume(&m) - 1.5).abs() < 1e-4);
        assert!(m.aabb().min[2] < -0.49 && m.aabb().max[2] < 1e-6);
    }

    #[test]
    fn cross_quads_are_double_sided_along_x() {
        let mut b = MeshBuilder::new();
        b.cross_quads(0.2, 0.3);
        let m = b.finish();
        assert_eq!(m.triangle_count(), 8);
        check_consistent(&m);
        let bb = m.aabb();
        assert!(close(bb.min, [0.0, -0.3, -0.2], 1e-6) && close(bb.max, [1.0, 0.3, 0.2], 1e-6));
    }

    #[test]
    fn beam_is_a_square_box_of_the_given_width() {
        let mut b = MeshBuilder::new();
        b.beam([0.0, 0.0, 0.0], [0.0, 0.0, 4.0], 0.1);
        let m = b.finish();
        assert_eq!(m.triangle_count(), 12);
        check_consistent(&m);
        assert!((volume(&m) - 0.2 * 0.2 * 4.0).abs() < 1e-4);
        let bb = m.aabb();
        assert!(close(bb.min, [-0.1, -0.1, 0.0], 1e-5) && close(bb.max, [0.1, 0.1, 4.0], 1e-5));
    }

    #[test]
    fn transforms_apply_and_mirrors_keep_faces_outward() {
        let mut b = MeshBuilder::new();
        b.xform = Xform::translation([5.0, 0.0, 0.0]).then(&Xform::rot_y(0.6)).then(&Xform::scale([2.0, 1.0, 0.5]));
        b.cuboid([-1.0; 3], [1.0; 3]);
        let m = b.finish();
        check_consistent(&m);
        check_outward(&m, [5.0, 0.0, 0.0]);
        assert!((volume(&m) - 8.0).abs() < 1e-3);

        let mut b = MeshBuilder::new();
        b.xform = Xform::scale([-1.0, 1.0, 1.0]);
        b.prism_y(2.0, 0.0, 1.0, 0.0, 1.0, 5, true);
        let m = b.finish();
        check_consistent(&m);
        check_outward(&m, [-2.0, 0.5, 0.0]);
        assert!(volume(&m) > 0.0);
    }

    #[test]
    fn with_restores_the_transform() {
        let mut b = MeshBuilder::new();
        b.with(Xform::translation([1.0, 0.0, 0.0]), |b| b.cuboid([0.0; 3], [1.0; 3]));
        b.cuboid([0.0; 3], [1.0; 3]);
        let m = b.finish();
        assert_eq!(m.aabb().max[0], 2.0);
        assert_eq!(m.aabb().min[0], 0.0);
    }

    #[test]
    fn colour_and_custom_channels_stay_aligned() {
        let mut b = MeshBuilder::new();
        b.color = [0.5, 0.5, 0.5, 0.0];
        b.cuboid([0.0; 3], [1.0; 3]);
        b.custom = Some([1.0, 0.0, 0.0, 0.0]);
        b.cuboid([2.0; 3], [3.0; 3]);
        b.custom = None;
        b.cuboid([4.0; 3], [5.0; 3]);
        let m = b.finish();
        check_consistent(&m);
        assert_eq!(m.custom0.len(), 72);
        assert_eq!(m.custom0[0], [0.0; 4]);
        assert_eq!(m.custom0[30], [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(m.custom0[71], [0.0; 4]);
        assert_eq!(m.colors[0], [0.5, 0.5, 0.5, 0.0]);
    }

    #[test]
    fn bad_input_never_panics_or_makes_nan() {
        let mut b = MeshBuilder::new();
        b.polygon(&[[0.0; 3], [1.0, 0.0, 0.0]]);
        b.polygon(&[[0.0; 3], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]]);
        b.polygon(&[[f32::NAN, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
        b.cuboid([0.0; 3], [0.0; 3]);
        b.prism_y(0.0, 0.0, -1.0, 2.0, 1.0, 0, true);
        b.prism_y(0.0, 0.0, 1.0, 0.0, 1.0, 255, false);
        b.frustum([0.0; 3], [0.0; 3], 1.0, 1.0, 8, true);
        b.frustum([0.0; 3], [1.0, 0.0, 0.0], f32::INFINITY, 1.0, 8, true);
        b.frustum([0.0; 3], [1.0, 0.0, 0.0], 0.0, 0.0, 1, true);
        b.icosphere([0.0; 3], 0.0, 1);
        b.ring_y(0.0, 0.0, 1.0, 1.0, 0.0, 0);
        b.extrude_z(&[], 1.0);
        b.extrude_z(&[[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]], 1.0);
        b.extrude_z(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]], f32::NAN);
        // Self-intersecting outline: triangulated without panicking.
        b.extrude_z(&[[0.0, 0.0], [1.0, 1.0], [1.0, 0.0], [0.0, 1.0], [0.5, 2.0]], 1.0);
        b.beam([0.0; 3], [0.0, 1.0, 0.0], 0.0);
        b.loft(&[vec![[0.0; 3]; 4]], true, true);
        b.loft(&[vec![[0.0; 3]; 4], vec![[0.0; 3]; 3]], true, true);
        let m = b.finish();
        check_consistent(&m);
        assert!(m.triangle_count() > 0);
    }

    #[test]
    fn loft_builds_a_closed_outward_dome() {
        let ring = |y: f32, r: f32| -> Vec<[f32; 3]> { (0..8).map(|i| { let (c, s) = ring_point(i, 8, 0.0); [r * c, y, r * s] }).collect() };
        let rings = vec![ring(0.0, 1.0), ring(0.5, 0.9), ring(0.9, 0.5), ring(1.0, 0.0)];
        let mut b = MeshBuilder::new();
        b.loft(&rings, true, true);
        let m = b.finish();
        check_consistent(&m);
        check_outward(&m, [0.0, 0.3, 0.0]);
        assert!(volume(&m) > 0.0);
        assert_eq!(m.triangle_count(), 8 * 2 * 2 + 8 + 6);
    }

    #[test]
    fn output_is_deterministic() {
        let build = || { let mut b = MeshBuilder::new(); b.icosphere([0.1, 0.2, 0.3], 1.3, 2); b.frustum([0.0; 3], [1.0, 2.0, 3.0], 0.4, 0.1, 9, true); b.finish() };
        assert_eq!(build(), build());
    }
}
