//! Unit `hm-lod`. Hull LODs 1-4 and the per-frame LOD pick.
//! DESIGN.md L1943: Rust picks per vessel per frame the coarsest LOD whose cell is at most 1.5 px (3.0 on Low),
//! 15% hysteresis; no visibility-range fades; at 60 degrees LOD1 (1 m) starts at 623 m, LOD2 1,247 m, LOD3
//! 2,494 m, LOD4 4,988 m. LODs downsample any-solid into one mesh each, with swatch and AO in vertex colour.
//! No impostors. L1786 (pixel math: 1 m at d m covers 935/d px at 1080p, 60 degrees; runtime uses live FOV).
//!
//! Downsampling is any-solid (a LOD cell is solid if any source cell in its block is solid), not bgm's
//! `mesh_lod` (which takes the topmost voxel). Shaped cells count as solid. Sealed air counts as air.
//! Vertex colour = majority palette swatch of the block's exterior source cells (ties to the lower index)
//! times 0fps-style vertex AO from the downsampled occupancy; alpha 1. This is also the S6 fallback's AO law.
//!
//! # Mesh construction
//! - Blocks are aligned to the grid origin; the last block on an axis is clipped to the grid's extent, so a LOD
//!   mesh never reaches past the hull's bounding box.
//! - An exterior source cell is a solid cell with a face neighbour that is exposed air (outside the grid, an
//!   unsealed empty cell or a removed cell). A block with no exterior cell takes the majority of all its cells.
//!   A swatch index past the palette draws as [`MISSING_SWATCH`].
//! - Vertex AO: at each face corner the three LOD cells around it in the layer outside the face (two sides and
//!   the diagonal; two solid sides count as three) give an occupancy `k / 4`, the value a linear fetch half a
//!   cell outside that corner would read. The colour factor is the volume AO law of L1937,
//!   `1 - 0.45 * clamp(1.33 * k / 4, 0, 1)`. Quads split along the diagonal joining the brighter corner pair, so
//!   a dark corner stays in one triangle.
//! - Faces greedy-merge per slice only when their swatch matches and their four AO levels are equal, so a merged
//!   quad is shaded exactly like the faces it replaces. Merged quads then gain every mesh vertex lying inside
//!   their edges (a fan around the quad centre), so the LOD mesh is crack-free (L1945) without the chunk-seam
//!   splitting the LOD0 path needs.
use crate::chunk::{class, validate_input, Cells, MeshInput};
use crate::types::{Lod, MeshData, QualityTier, CELL_M_F32};

/// Hull LOD threshold in pixels per cell (L1943).
pub const LOD_CELL_PX: f64 = 1.5;
/// Threshold on `QualityTier::Low` (L1943). The Compatibility fallback uses it too, as gdclient's quality
/// profile does (`lod_cell_px = 3.0` for both).
pub const LOD_CELL_PX_LOW: f64 = 3.0;
/// Hysteresis band either side of the threshold (L1943).
pub const LOD_HYSTERESIS: f64 = 0.15;
/// Linear grey drawn for a swatch index the palette does not hold (a caller error; never a panic).
pub const MISSING_SWATCH: [f32; 3] = [0.2, 0.2, 0.2];

/// Mesh one whole vessel at `lod` (L1..=L4) as a single mesh with vertex colours. `Lod::L0` is a caller error
/// (LOD0 uses chunk meshes). Deterministic. Unit: `hm-lod`.
///
/// `Lod::L0` and a malformed input (see `chunk::validate_input`) give an empty mesh.
pub fn mesh_lod(input: &MeshInput<'_>, lod: Lod) -> MeshData {
    let mut out = MeshData::new();
    if lod == Lod::L0 || validate_input(input).is_err() {
        return out;
    }
    let grid = Downsampled::build(input, lod.factor());
    if grid.d.contains(&0) {
        return out;
    }
    let quads = grid.faces();
    emit(&quads, input.palette, &mut out);
    out
}

/// Pixel size of one cell of `lod` at `distance_m` for a viewport `viewport_h_px` tall with vertical FOV
/// `fov_y_rad` (libm tan). Unit: `hm-lod`.
///
/// A distance of zero or less is infinitely close (`f64::INFINITY`). An FOV outside `(0, pi)`, a viewport that is
/// not a positive finite height or a NaN distance has no meaningful size and returns NaN, which [`pick_lod`]
/// treats as "keep the current LOD".
pub fn cell_px(lod: Lod, distance_m: f64, fov_y_rad: f64, viewport_h_px: f64) -> f64 {
    let fov_ok = fov_y_rad > 0.0 && fov_y_rad < core::f64::consts::PI;
    let view_ok = viewport_h_px > 0.0 && viewport_h_px.is_finite();
    if !fov_ok || !view_ok || distance_m.is_nan() {
        return f64::NAN;
    }
    if distance_m <= 0.0 {
        return f64::INFINITY;
    }
    let half = libm::tan(0.5 * fov_y_rad);
    lod.cell_m() * viewport_h_px / (2.0 * half * distance_m)
}

/// Coarsest LOD whose cell is at most 1.5 px (3.0 px on `QualityTier::Low`), with 15% hysteresis against
/// `current`: moving to a finer LOD requires the coarser cell to exceed the threshold by 15%, and to a coarser
/// LOD requires it to be under the threshold by 15%. Unit: `hm-lod`.
///
/// `QualityTier::Fallback` uses the Low threshold. A jump over several LODs (a camera cut) lands on the LOD the
/// hysteresis band holds, so the next frame at the same distance does not move again. Inputs `cell_px` rejects
/// keep `current`.
pub fn pick_lod(distance_m: f64, fov_y_rad: f64, viewport_h_px: f64, tier: QualityTier, current: Lod) -> Lod {
    let t = match tier {
        QualityTier::Low | QualityTier::Fallback => LOD_CELL_PX_LOW,
        QualityTier::Medium | QualityTier::High => LOD_CELL_PX,
    };
    let px = |l: Lod| cell_px(l, distance_m, fov_y_rad, viewport_h_px);
    if px(Lod::L0).is_nan() {
        return current;
    }
    let coarsest = |limit: f64, keep: &dyn Fn(Lod) -> bool| Lod::ALL.into_iter().filter(|&l| keep(l) && px(l) <= limit).max();
    let plain = coarsest(t, &|_| true).unwrap_or(Lod::L0);
    if plain > current {
        // Coarser only once the coarser cell is 15% under the threshold.
        coarsest(t * (1.0 - LOD_HYSTERESIS), &|l| l > current).unwrap_or(current)
    } else if plain < current {
        // Finer only once the current cell is 15% over the threshold; then the coarsest LOD inside the band.
        if px(current) <= t * (1.0 + LOD_HYSTERESIS) {
            current
        } else {
            coarsest(t * (1.0 + LOD_HYSTERESIS), &|l| l < current).unwrap_or(Lod::L0)
        }
    } else {
        current
    }
}

/// Colour factor at a face corner with `k` of 4 outer cells occupied (volume AO law, L1937).
pub(crate) fn ao_factor(k: u8) -> f32 {
    let occ = k.min(4) as f32 / 4.0;
    1.0 - 0.45 * (1.33 * occ).clamp(0.0, 1.0)
}

/// In-plane axes (u, v) of a face direction with u x v = outward normal. `dir` is `2 * axis + (sign > 0)`, the
/// `content::cell::Face::ALL` order.
fn face_uv(dir: usize) -> (usize, usize) {
    match dir {
        0 => (2, 1), // -X
        1 => (1, 2), // +X
        2 => (0, 2), // -Y
        3 => (2, 0), // +Y
        4 => (1, 0), // -Z
        _ => (0, 1), // +Z
    }
}

/// The downsampled occupancy and per-block swatch.
struct Downsampled {
    /// LOD grid size in blocks.
    d: [usize; 3],
    /// Source grid size in cells.
    s: [usize; 3],
    f: usize,
    occ: Vec<bool>,
    swatch: Vec<u8>,
}

impl Downsampled {
    fn build(input: &MeshInput<'_>, f: usize) -> Self {
        let g = Cells::new(input.view);
        let s = [g.s[0] as usize, g.s[1] as usize, g.s[2] as usize];
        let d = [s[0].div_ceil(f), s[1].div_ceil(f), s[2].div_ceil(f)];
        let n = d[0] * d[1] * d[2];
        let mut occ = vec![false; n];
        let mut swatch = vec![0u8; n];
        let exposed = |x: i32, y: i32, z: i32| g.class(g.index(x, y, z)) == class::EXPOSED_AIR;
        for by in 0..d[1] {
            for bz in 0..d[2] {
                for bx in 0..d[0] {
                    // At most 16^3 = 4096 cells per block: u16 counts cannot overflow.
                    let mut ext = [0u16; 16];
                    let mut all = [0u16; 16];
                    let mut solid = false;
                    let x0 = bx * f;
                    let x1 = (x0 + f).min(s[0]);
                    for y in by * f..((by + 1) * f).min(s[1]) {
                        for z in bz * f..((bz + 1) * f).min(s[2]) {
                            let Some(row) = g.index(x0 as i32, y as i32, z as i32) else { continue };
                            for x in x0..x1 {
                                let i = row + (x - x0);
                                if !g.solid(i) {
                                    continue;
                                }
                                solid = true;
                                let p = (input.view.cells[i].paint & 0x0F) as usize;
                                all[p] += 1;
                                let (xi, yi, zi) = (x as i32, y as i32, z as i32);
                                if exposed(xi - 1, yi, zi) || exposed(xi + 1, yi, zi) || exposed(xi, yi - 1, zi) || exposed(xi, yi + 1, zi) || exposed(xi, yi, zi - 1) || exposed(xi, yi, zi + 1) {
                                    ext[p] += 1;
                                }
                            }
                        }
                    }
                    if !solid {
                        continue;
                    }
                    let li = bx + d[0] * (bz + d[2] * by);
                    occ[li] = true;
                    swatch[li] = majority(if ext.iter().any(|&c| c > 0) { &ext } else { &all });
                }
            }
        }
        Self { d, s, f, occ, swatch }
    }

    #[inline]
    fn index(&self, c: [i32; 3]) -> Option<usize> {
        if (0..3).any(|k| c[k] < 0 || c[k] >= self.d[k] as i32) {
            return None;
        }
        Some(c[0] as usize + self.d[0] * (c[2] as usize + self.d[2] * c[1] as usize))
    }

    #[inline]
    fn solid(&self, c: [i32; 3]) -> bool { self.index(c).is_some_and(|i| self.occ[i]) }

    /// LOD lattice coordinate along `axis` in source cells, clipped to the grid.
    #[inline]
    fn src(&self, axis: usize, lc: usize) -> u32 { (lc * self.f).min(self.s[axis]) as u32 }

    /// Occupancy levels (0..=3) at the four corners of the face of cell `c` towards `dir`, in corner order
    /// (u0 v0), (u1 v0), (u0 v1), (u1 v1).
    fn corner_ao(&self, c: [i32; 3], dir: usize) -> [u8; 4] {
        let a = dir / 2;
        let (u, v) = face_uv(dir);
        let mut o = c;
        o[a] += if dir & 1 == 1 { 1 } else { -1 };
        let mut out = [0u8; 4];
        for (k, slot) in out.iter_mut().enumerate() {
            let du = if k & 1 == 1 { 1 } else { -1 };
            let dv = if k & 2 == 2 { 1 } else { -1 };
            let mut su = o;
            su[u] += du;
            let mut sv = o;
            sv[v] += dv;
            let mut cn = su;
            cn[v] += dv;
            let (s1, s2) = (self.solid(su), self.solid(sv));
            *slot = if s1 && s2 { 3 } else { s1 as u8 + s2 as u8 + self.solid(cn) as u8 };
        }
        out
    }

    /// Every exposed face, greedy-merged where swatch and uniform AO match.
    fn faces(&self) -> Vec<Quad> {
        let mut quads = Vec::new();
        let mut mask: Vec<u32> = Vec::new();
        for dir in 0..6 {
            let a = dir / 2;
            let (u, v) = face_uv(dir);
            let (du, dv) = (self.d[u], self.d[v]);
            let step = if dir & 1 == 1 { 1 } else { -1 };
            mask.clear();
            mask.resize(du * dv, 0);
            for t in 0..self.d[a] {
                // Face keys of this slice: 0 none, else 1 << 24 | swatch << 8 | four 2-bit AO levels.
                for iv in 0..dv {
                    for iu in 0..du {
                        let mut c = [0i32; 3];
                        c[a] = t as i32;
                        c[u] = iu as i32;
                        c[v] = iv as i32;
                        let mut nb = c;
                        nb[a] += step;
                        mask[iu + du * iv] = match self.index(c) {
                            Some(i) if self.occ[i] && !self.solid(nb) => {
                                let ao = self.corner_ao(c, dir);
                                let bits = ao[0] as u32 | (ao[1] as u32) << 2 | (ao[2] as u32) << 4 | (ao[3] as u32) << 6;
                                1 << 24 | (self.swatch[i] as u32) << 8 | bits
                            }
                            _ => 0,
                        };
                    }
                }
                let plane = self.src(a, if dir & 1 == 1 { t + 1 } else { t });
                for iv in 0..dv {
                    let mut iu = 0;
                    while iu < du {
                        let key = mask[iu + du * iv];
                        if key == 0 {
                            iu += 1;
                            continue;
                        }
                        let ao = [0, 2, 4, 6].map(|sh| ((key >> sh) & 3) as u8);
                        let (mut w, mut h) = (1, 1);
                        if ao.iter().all(|&l| l == ao[0]) {
                            while iu + w < du && mask[iu + w + du * iv] == key {
                                w += 1;
                            }
                            'grow: while iv + h < dv {
                                for k in 0..w {
                                    if mask[iu + k + du * (iv + h)] != key {
                                        break 'grow;
                                    }
                                }
                                h += 1;
                            }
                        }
                        for jv in 0..h {
                            for k in 0..w {
                                mask[iu + k + du * (iv + jv)] = 0;
                            }
                        }
                        quads.push(Quad {
                            dir: dir as u8,
                            plane,
                            lo: [self.src(u, iu), self.src(v, iv)],
                            hi: [self.src(u, iu + w), self.src(v, iv + h)],
                            swatch: ((key >> 8) & 0xFF) as u8,
                            ao,
                        });
                        iu += w;
                    }
                }
            }
        }
        quads
    }
}

/// Swatch with the most votes; ties go to the lower index.
fn majority(votes: &[u16; 16]) -> u8 {
    let mut best = 0usize;
    for (i, &c) in votes.iter().enumerate() {
        if c > votes[best] {
            best = i;
        }
    }
    best as u8
}

/// One axis-aligned face rectangle in source-cell lattice units.
struct Quad {
    dir: u8,
    /// Plane coordinate along the face's normal axis.
    plane: u32,
    /// Minimum and maximum (u, v) corner.
    lo: [u32; 2],
    hi: [u32; 2],
    swatch: u8,
    /// Occupancy levels at corners (u0 v0), (u1 v0), (u0 v1), (u1 v1).
    ao: [u8; 4],
}

impl Quad {
    fn corner(&self, cu: u32, cv: u32) -> [u32; 3] {
        let dir = self.dir as usize;
        let (u, v) = face_uv(dir);
        let mut p = [0u32; 3];
        p[dir / 2] = self.plane;
        p[u] = cu;
        p[v] = cv;
        p
    }
}

/// Every quad corner, grouped by axis-parallel line, to find the vertices strictly inside an edge.
struct LinePoints {
    /// (axis, coordinate on axis + 1, coordinate on axis + 2, coordinate along the axis), sorted, unique.
    pts: Vec<[u32; 4]>,
}

impl LinePoints {
    fn build(quads: &[Quad]) -> Self {
        let mut pts = Vec::with_capacity(quads.len() * 12);
        for q in quads {
            for (cu, cv) in [(q.lo[0], q.lo[1]), (q.hi[0], q.lo[1]), (q.lo[0], q.hi[1]), (q.hi[0], q.hi[1])] {
                let p = q.corner(cu, cv);
                for axis in 0..3 {
                    pts.push([axis as u32, p[(axis + 1) % 3], p[(axis + 2) % 3], p[axis]]);
                }
            }
        }
        pts.sort_unstable();
        pts.dedup();
        Self { pts }
    }

    /// Points strictly between coordinates `a` and `b` on the line along `axis` through `p`, ascending.
    fn inside(&self, axis: usize, p: [u32; 3], a: u32, b: u32) -> &[[u32; 4]] {
        let line = [axis as u32, p[(axis + 1) % 3], p[(axis + 2) % 3]];
        let (lo, hi) = (a.min(b), a.max(b));
        let start = self.pts.partition_point(|e| (e[0], e[1], e[2], e[3]) <= (line[0], line[1], line[2], lo));
        let end = self.pts.partition_point(|e| (e[0], e[1], e[2], e[3]) < (line[0], line[1], line[2], hi));
        &self.pts[start..end.max(start)]
    }
}

fn to_m(p: [u32; 3]) -> [f32; 3] { p.map(|c| c as f32 * CELL_M_F32) }

fn swatch_rgb(palette: &[[f32; 3]], i: u8) -> [f32; 3] {
    let c = palette.get(i as usize).copied().unwrap_or(MISSING_SWATCH);
    c.map(|x| if x.is_finite() { x.clamp(0.0, 1.0) } else { 0.0 })
}

fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] { [0, 1, 2, 3].map(|k| a[k] + (b[k] - a[k]) * t) }

/// Write the quads as triangles with vertex colours, splitting quads at the vertices inside their edges.
fn emit(quads: &[Quad], palette: &[[f32; 3]], out: &mut MeshData) {
    let lines = LinePoints::build(quads);
    let mut ring: Vec<([u32; 3], [f32; 4])> = Vec::new();
    for q in quads {
        let dir = q.dir as usize;
        let (u, v) = face_uv(dir);
        let mut n = [0.0f32; 3];
        n[dir / 2] = if dir & 1 == 1 { 1.0 } else { -1.0 };
        let rgb = swatch_rgb(palette, q.swatch);
        let col = |k: usize| {
            let f = ao_factor(q.ao[k]);
            [rgb[0] * f, rgb[1] * f, rgb[2] * f, 1.0]
        };
        // Corners clockwise from outside: (u0 v0), (u0 v1), (u1 v1), (u1 v0), with their AO corner slots.
        let corners = [((q.lo[0], q.lo[1]), 0usize), ((q.lo[0], q.hi[1]), 2), ((q.hi[0], q.hi[1]), 3), ((q.hi[0], q.lo[1]), 1)];
        ring.clear();
        let mut split = false;
        for e in 0..4 {
            let ((au, av), ka) = corners[e];
            let ((bu, bv), kb) = corners[(e + 1) % 4];
            let (pa, pb) = (q.corner(au, av), q.corner(bu, bv));
            let (ca, cb) = (col(ka), col(kb));
            ring.push((pa, ca));
            let axis = if au != bu { u } else { v };
            let (a, b) = (pa[axis], pb[axis]);
            let mids = lines.inside(axis, pa, a, b);
            if mids.is_empty() {
                continue;
            }
            split = true;
            let span = b as f32 - a as f32;
            let at = |m: &[u32; 4]| {
                let mut p = pa;
                p[axis] = m[3];
                (p, lerp4(ca, cb, (m[3] as f32 - a as f32) / span))
            };
            if b > a {
                ring.extend(mids.iter().map(at));
            } else {
                ring.extend(mids.iter().rev().map(at));
            }
        }
        let base = out.positions.len() as u32;
        if !split {
            for (p, c) in &ring {
                out.positions.push(to_m(*p));
                out.normals.push(n);
                out.colors.push(*c);
            }
            // Ring order is c00, c01, c11, c10 (AO slots 0, 2, 3, 1). Split along the brighter diagonal.
            let b = |k: usize| 3 - q.ao[k].min(3) as i32;
            if b(0) + b(3) >= b(1) + b(2) {
                out.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            } else {
                out.indices.extend_from_slice(&[base + 1, base + 2, base + 3, base + 1, base + 3, base]);
            }
            continue;
        }
        // Fan around the quad centre (bilinear colour there), so no triangle is degenerate.
        let (lo_m, hi_m) = (to_m(q.corner(q.lo[0], q.lo[1])), to_m(q.corner(q.hi[0], q.hi[1])));
        out.positions.push([0, 1, 2].map(|k| 0.5 * (lo_m[k] + hi_m[k])));
        out.normals.push(n);
        out.colors.push(lerp4(lerp4(col(0), col(1), 0.5), lerp4(col(2), col(3), 0.5), 0.5));
        for (p, c) in &ring {
            out.positions.push(to_m(*p));
            out.normals.push(n);
            out.colors.push(*c);
        }
        let m = ring.len() as u32;
        for i in 0..m {
            out.indices.extend_from_slice(&[base, base + 1 + i, base + 1 + (i + 1) % m]);
        }
    }
}

#[cfg(test)]
mod tests;
