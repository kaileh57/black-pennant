//! Unit `sim-core-terrain`. Land and water depth of one sortie map. DESIGN.md L1133 (one world, max 100 km),
//! L762-766 (the 50 m pathing grid stores depth per cell), L786-797 (shore markers).
//!
//! DECISION: terrain is an analytic height field built from `content::campaign::MapDef` islands (smooth
//! radial profiles with seeded noise, libm only). It serves grounding checks, the Rapier terrain collider,
//! the pathing depth grid in sim-systems, radar and visual line of sight, and the renderer's shore heightfield.
//!
//! # Height model
//! Ground height is the maximum of the open-water seabed (`-depth_m`) and one profile per island:
//! - The shoreline sits at `radius_m * f(theta)`, where `f` is `1 +` five low harmonics (orders 2 to 6) with
//!   seeded amplitudes and phases. `f` stays within 0.84 to 1.16, so authored markers placed against
//!   `radius_m` stay on the side of the shore they were placed on.
//! - On land, height is `height_m * (1 - d^2) * (0.8 + 0.2 n)`, where `d` is the distance over the local shore
//!   radius and `n` in [0, 1] is two octaves of hashed value noise. It is 0 at the shore and at most `height_m`.
//! - Offshore, a shelf of width `max(0.5 radius, 300 m)` falls to `-depth_m` along a smoothstep. That leaves a
//!   readable band of shallows where deep-draft hulls ground before small craft do.
//! - An island with `height_m <= 0` is a submerged shoal. Its peak `height_m` is at the centre, and it falls to
//!   the seabed by smoothstep at the shore radius plus the shelf.
//!
//! Seeds: island `i` hashes `(map.seed, island.seed, i)` through [`crate::rng`] (`Stream::Generator`).
//! Every value is a pure function of the map, with no caches. Bad input never panics: non-finite coordinates
//! read as open water; islands with a non-positive or non-finite radius, or a non-finite height, are ignored;
//! a non-positive or non-finite `depth_m` falls back to [`DEFAULT_DEPTH_M`]; and map sizes are clamped to
//! `MAX_MAP_M`.
#[cfg(test)]
mod tests;

use crate::math::DVec3;
use crate::rng::{self, Key, Stream};
use content::campaign::{Island, MapDef, Marker, MarkerKind, MAX_MAP_M};
use serde::{Deserialize, Serialize};

/// Open-water depth used when a map gives none (or a non-positive one).
pub const DEFAULT_DEPTH_M: f64 = 200.0;
/// Terrain must stand this far above a line before it blocks it, so a sensor sitting on the ground does not
/// block itself.
pub const LOS_EPS_M: f64 = 0.25;
/// Largest patch edge in samples (2048 is the renderer's; the cap keeps a bad request bounded).
pub const MAX_PATCH_N: u32 = 4096;
/// Largest depth-grid edge in cells; a finer `cell_m` on a big map is coarsened to fit.
pub const MAX_GRID_CELLS: u32 = 8192;
/// Base harmonic amplitudes of the shoreline (orders 2..=6), scaled per island by a seeded factor in [0.5, 1].
const HARMONIC_AMP: [f64; 5] = [0.07, 0.04, 0.025, 0.015, 0.01];
/// Upper bound of `f(theta)`: 1 + sum of `HARMONIC_AMP`.
const SHORE_MAX: f64 = 1.16;

/// Cell size of the coarse pathing grid in metres (L762).
pub const PATH_CELL_M: f64 = 50.0;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Terrain { pub map: MapDef }

/// Coarse depth grid for A* routing: `depth[x + cols * z]` is water depth in metres (0 or negative = land).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DepthGrid { pub cell_m: f64, pub cols: u32, pub rows: u32, pub origin: [f64; 2], pub depth: Vec<f32> }

impl Terrain {
    pub fn new(map: MapDef) -> Self { Self { map } }
    /// Ground height at world (x, z) in metres: positive above sea level, negative = seabed depth.
    /// Unit: `sim-core-terrain`.
    pub fn height_at(&self, x: f64, z: f64) -> f64 {
        let base = -self.base_depth();
        if !x.is_finite() || !z.is_finite() { return base; }
        let mut h = base;
        for (i, isl) in self.map.islands.iter().enumerate() {
            let Some(reach) = coarse_reach(isl) else { continue };
            let (dx, dz) = (x - isl.centre_m[0], z - isl.centre_m[1]);
            if dx * dx + dz * dz > reach * reach { continue; }
            if let Some(shape) = IslandShape::new(isl, i, self.map.seed, -base) { h = h.max(shape.height(x, z)); }
        }
        h
    }
    /// Water depth (positive down) at world (x, z); 0 on land. Unit: `sim-core-terrain`.
    pub fn depth_at(&self, x: f64, z: f64) -> f64 { (-self.height_at(x, z)).max(0.0) }
    /// True when the straight segment between two world points is blocked by land (line of sight for visual,
    /// camera and radar; shells hitting islands). Unit: `sim-core-terrain`.
    ///
    /// The segment is blocked when terrain stands more than [`LOS_EPS_M`] above it anywhere, the seabed
    /// included. Islands are prefiltered by a 2D segment-circle test and their peak height, then marched at
    /// a step of `radius / 40` (1 to 10 m) inside the overlap. Non-finite input is never blocked.
    pub fn segment_blocked(&self, a: DVec3, b: DVec3) -> bool {
        if !(a.is_finite() && b.is_finite()) { return false; }
        let depth = self.base_depth();
        if a.y.min(b.y) < -depth - LOS_EPS_M { return true; }
        let underwater = a.y.min(b.y) < 0.0;
        let (dx, dz) = (b.x - a.x, b.z - a.z);
        let len2 = dx * dx + dz * dz;
        for (i, isl) in self.map.islands.iter().enumerate() {
            let Some(shape) = IslandShape::new(isl, i, self.map.seed, depth) else { continue };
            let reach = if underwater { shape.reach } else { shape.land_reach };
            if reach <= 0.0 { continue; }
            // Parameter interval of the segment inside the island's reach circle.
            let (fx, fz) = (a.x - shape.cx, a.z - shape.cz);
            let (s0, s1) = if len2 <= 1e-18 {
                if fx * fx + fz * fz > reach * reach { continue; }
                (0.0, 1.0)
            } else {
                let bq = fx * dx + fz * dz;
                let cq = fx * fx + fz * fz - reach * reach;
                let disc = bq * bq - len2 * cq;
                if disc < 0.0 { continue; }
                let sq = disc.sqrt();
                let (t0, t1) = ((-bq - sq) / len2, (-bq + sq) / len2);
                if t1 < 0.0 || t0 > 1.0 { continue; }
                (t0.max(0.0), t1.min(1.0))
            };
            let ya = a.y + (b.y - a.y) * s0;
            let yb = a.y + (b.y - a.y) * s1;
            if ya.min(yb) >= shape.peak + LOS_EPS_M { continue; }
            let span = (s1 - s0) * len2.sqrt();
            let step = (shape.r / 40.0).clamp(1.0, 10.0);
            let n = ((span / step).ceil() as usize).clamp(1, 100_000);
            for k in 0..=n {
                let t = s0 + (s1 - s0) * (k as f64 / n as f64);
                let p = a + (b - a) * t;
                if shape.height(p.x, p.z) > p.y + LOS_EPS_M { return true; }
            }
        }
        false
    }
    /// Build the pathing depth grid at `cell_m` (50 m default, 25 m fallback). Unit: `sim-core-terrain`.
    ///
    /// The grid covers the map (clamped to `MAX_MAP_M`), centred on the origin, from `origin` = the minimum
    /// (x, z) corner, with `cols = ceil(size_x / cell)`. Each cell stores the shallowest of its centre and its
    /// four corners, so a cell that clips the shore never reads as deep water. A zero-size map gives an empty
    /// grid; a non-positive or non-finite `cell_m` uses [`PATH_CELL_M`].
    pub fn depth_grid(&self, cell_m: f64) -> DepthGrid {
        let size = self.map_size();
        let mut cell = if cell_m.is_finite() && cell_m > 0.0 { cell_m } else { PATH_CELL_M };
        cell = cell.max(size[0].max(size[1]) / MAX_GRID_CELLS as f64);
        let cols = (size[0] / cell).ceil() as u32;
        let rows = (size[1] / cell).ceil() as u32;
        let origin = [-size[0] * 0.5, -size[1] * 0.5];
        if cols == 0 || rows == 0 { return DepthGrid { cell_m: cell, cols: 0, rows: 0, origin, depth: Vec::new() }; }
        let centres = self.height_lattice([origin[0] + cell * 0.5, origin[1] + cell * 0.5], cell, cols, rows);
        let corners = self.height_lattice(origin, cell, cols + 1, rows + 1);
        let cw = cols as usize + 1;
        let mut depth = Vec::with_capacity(cols as usize * rows as usize);
        for z in 0..rows as usize {
            for x in 0..cols as usize {
                let h = centres[x + cols as usize * z]
                    .max(corners[x + cw * z]).max(corners[x + 1 + cw * z])
                    .max(corners[x + cw * (z + 1)]).max(corners[x + 1 + cw * (z + 1)]);
                depth.push(-h as f32);
            }
        }
        DepthGrid { cell_m: cell, cols, rows, origin, depth }
    }
    /// Height samples for a square patch, for the physics heightfield collider near a vessel and the renderer's
    /// 2048^2 shore heightfield (L1907). Row-major, `n * n` values. Unit: `sim-core-terrain`.
    ///
    /// Sample `(ix, iz)` is at `centre - size / 2 + (ix, iz) * size / (n - 1)` and is stored at `ix + n * iz`, so
    /// the outer samples lie on the patch edges (heightfield vertices). `n == 1` samples the centre and `n == 0`
    /// returns nothing. `n` is capped at [`MAX_PATCH_N`]; a non-finite or negative size reads as 0.
    pub fn sample_patch(&self, centre: [f64; 2], size_m: f64, n: u32) -> Vec<f32> {
        let n = n.min(MAX_PATCH_N);
        if n == 0 { return Vec::new(); }
        let size = if size_m.is_finite() && size_m > 0.0 { size_m } else { 0.0 };
        let step = if n > 1 { size / (n - 1) as f64 } else { 0.0 };
        let start = if n > 1 { [centre[0] - size * 0.5, centre[1] - size * 0.5] } else { centre };
        self.height_lattice(start, step, n, n).into_iter().map(|h| h as f32).collect()
    }

    /// Open-water depth with the fallback applied.
    fn base_depth(&self) -> f64 { if self.map.depth_m.is_finite() && self.map.depth_m > 0.0 { self.map.depth_m } else { DEFAULT_DEPTH_M } }

    /// Map extent clamped to `[0, MAX_MAP_M]`.
    fn map_size(&self) -> [f64; 2] {
        let c = |v: f64| if v.is_finite() { v.clamp(0.0, MAX_MAP_M) } else { 0.0 };
        [c(self.map.size_m[0]), c(self.map.size_m[1])]
    }

    /// Heights on a regular lattice: sample `(ix, iz)` at `start + (ix, iz) * step`, stored at `ix + cols * iz`.
    /// Each island only touches the samples inside its bounding square, so open water costs one fill.
    fn height_lattice(&self, start: [f64; 2], step: f64, cols: u32, rows: u32) -> Vec<f64> {
        let depth = self.base_depth();
        let (cols, rows) = (cols as usize, rows as usize);
        let mut out = vec![-depth; cols * rows];
        if !(start[0].is_finite() && start[1].is_finite() && step.is_finite()) { return out; }
        for (i, isl) in self.map.islands.iter().enumerate() {
            let Some(shape) = IslandShape::new(isl, i, self.map.seed, depth) else { continue };
            let range = |c: f64, s: f64, n: usize| -> (usize, usize) {
                if n == 0 { return (0, 0); }
                if step <= 0.0 {
                    return if (s - c).abs() <= shape.reach { (0, n) } else { (0, 0) };
                }
                let lo = ((c - shape.reach - s) / step).floor().max(0.0);
                let hi = ((c + shape.reach - s) / step).ceil() + 1.0;
                if hi <= 0.0 || lo >= n as f64 { return (0, 0); }
                (lo as usize, (hi.min(n as f64)) as usize)
            };
            let (x0, x1) = range(shape.cx, start[0], cols);
            let (z0, z1) = range(shape.cz, start[1], rows);
            for iz in z0..z1 {
                let z = start[1] + iz as f64 * step;
                for ix in x0..x1 {
                    let x = start[0] + ix as f64 * step;
                    let v = &mut out[ix + cols * iz];
                    *v = v.max(shape.height(x, z));
                }
            }
        }
        out
    }
    pub fn marker(&self, name: &str) -> Option<&Marker> { self.map.markers.iter().find(|m| m.name == name) }
    pub fn markers_of(&self, kind: MarkerKind) -> impl Iterator<Item = &Marker> { self.map.markers.iter().filter(move |m| m.kind == kind) }
}

/// Smoothstep on [0, 1] (clamped).
fn smoothstep(t: f64) -> f64 { let t = t.clamp(0.0, 1.0); t * t * (3.0 - 2.0 * t) }

/// Shelf width of an island of radius `r`.
fn shelf_width(r: f64) -> f64 { (0.5 * r).max(300.0) }

/// Conservative reach of an island (shore plus shelf) without hashing, for cheap rejection; `None` when the
/// island is invalid.
fn coarse_reach(isl: &Island) -> Option<f64> {
    let r = isl.radius_m;
    if !(r.is_finite() && r > 0.0 && isl.height_m.is_finite() && isl.centre_m[0].is_finite() && isl.centre_m[1].is_finite()) { return None; }
    Some(r * SHORE_MAX + shelf_width(r))
}

/// One island with its seeded shape parameters resolved.
struct IslandShape {
    cx: f64,
    cz: f64,
    r: f64,
    /// Peak height (positive island) or crest height (shoal, <= 0).
    peak: f64,
    depth: f64,
    shelf: f64,
    /// (amplitude, cos phase, sin phase) for harmonic orders 2..=6.
    harmonics: [(f64, f64, f64); 5],
    noise_seed: u64,
    noise_cell: f64,
    /// Furthest distance from the centre at which the island stands above the seabed.
    reach: f64,
    /// Furthest distance from the centre at which the island stands above sea level (0 for shoals).
    land_reach: f64,
}

impl IslandShape {
    fn new(isl: &Island, index: usize, map_seed: u32, depth: f64) -> Option<Self> {
        let reach = coarse_reach(isl)?;
        if isl.height_m <= -depth { return None; }
        let seed = rng::bits(Key::new(map_seed as u64, 0, Stream::Generator, isl.seed as u64, index as u64));
        let mut harmonics = [(0.0, 1.0, 0.0); 5];
        for (k, hm) in harmonics.iter_mut().enumerate() {
            let key = Key::new(seed, 1, Stream::Generator, k as u64, 0);
            let amp = HARMONIC_AMP[k] * (0.5 + 0.5 * rng::uniform(key));
            let phase = core::f64::consts::TAU * rng::uniform(key.draw(1));
            *hm = (amp, libm::cos(phase), libm::sin(phase));
        }
        let r = isl.radius_m;
        let land = isl.height_m > 0.0;
        Some(Self {
            cx: isl.centre_m[0],
            cz: isl.centre_m[1],
            r,
            peak: isl.height_m,
            depth,
            shelf: shelf_width(r),
            harmonics,
            noise_seed: seed,
            noise_cell: (r / 3.0).clamp(40.0, 2000.0),
            reach,
            land_reach: if land { r * SHORE_MAX } else { 0.0 },
        })
    }

    /// Ground height of this island alone at (x, z); `-depth` beyond its reach.
    fn height(&self, x: f64, z: f64) -> f64 {
        let (dx, dz) = (x - self.cx, z - self.cz);
        let r2 = dx * dx + dz * dz;
        if !(r2 <= self.reach * self.reach) { return -self.depth; }
        let dist = r2.sqrt();
        let (c, s) = if dist > 1e-9 { (dx / dist, dz / dist) } else { (1.0, 0.0) };
        // cos(k theta + phi) for k = 2..=6 by repeated complex multiplication (no transcendentals).
        let (mut ck, mut sk) = (c * c - s * s, 2.0 * c * s);
        let mut f = 1.0;
        for &(amp, cp, sp) in &self.harmonics {
            f += amp * (ck * cp - sk * sp);
            let (nc, ns) = (ck * c - sk * s, sk * c + ck * s);
            ck = nc;
            sk = ns;
        }
        let shore = self.r * f;
        if self.peak > 0.0 {
            if dist <= shore {
                let d = dist / shore;
                self.peak * (1.0 - d * d) * (0.8 + 0.2 * self.noise(x, z))
            } else {
                -self.depth * smoothstep((dist - shore) / self.shelf)
            }
        } else {
            let t = smoothstep(dist / (shore + self.shelf));
            self.peak + (-self.depth - self.peak) * t
        }
    }

    /// Two octaves of hashed value noise in [0, 1].
    fn noise(&self, x: f64, z: f64) -> f64 {
        let a = value_noise(self.noise_seed, x / self.noise_cell, z / self.noise_cell);
        let b = value_noise(self.noise_seed ^ 0x6F63_7461_7665_3221, 2.0 * x / self.noise_cell, 2.0 * z / self.noise_cell);
        (a + 0.5 * b) / 1.5
    }
}

/// Bilinear value noise with smoothstep fade on an integer lattice, in [0, 1].
fn value_noise(seed: u64, x: f64, z: f64) -> f64 {
    let (fx, fz) = (x.floor(), z.floor());
    let (tx, tz) = (smoothstep(x - fx), smoothstep(z - fz));
    let (ix, iz) = (fx as i64, fz as i64);
    let corner = |i: i64, k: i64| -> f64 {
        let h = rng::absorb(rng::absorb(rng::mix64(seed), i as u64), k as u64);
        (h >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
    };
    let a = corner(ix, iz) + (corner(ix + 1, iz) - corner(ix, iz)) * tx;
    let b = corner(ix, iz + 1) + (corner(ix + 1, iz + 1) - corner(ix, iz + 1)) * tx;
    a + (b - a) * tz
}
