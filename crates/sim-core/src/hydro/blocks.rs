//! Buoyancy blocks: building them from the grid, the hull summary derived from them, and the one kernel that
//! turns a block and a water plane into displaced volume. Everything that floats a hull (the per-tick force,
//! the equilibrium solver, the GZ curve) goes through [`BuoyancyBlocks::eval_block`], so the gauges predict
//! the simulation by construction.
//!
//! A block's fill is kept per cell layer ([`Layer`]). One fill fraction for a whole 2 m cube smears a keel, a
//! turn of bilge or a flared side over the cube: on the starter hulls that moved the metacentric height by up
//! to a factor of three against the cells themselves, listed symmetric hulls (blocks are aligned to the grid,
//! not the centreline) and capsized one of them at the quay. Layers cost nothing where a block is wholly
//! under or wholly dry, which is nearly all of them; the blocks, the columns and the water samples are as the
//! spec gives them (L650).
use super::{Block, BuoyancyBlocks};
use crate::compartments::{CompartmentSet, MAX_COMPARTMENTS, VOID_CELL};
use crate::grid::{VesselGrid, CHUNK};
use content::cell::{CellPos, CELL_M, CELL_VOLUME_M3};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Largest block edge in cells (the far-hull fallback uses 8).
pub(crate) const MAX_BLOCK_CELLS: u8 = 8;
/// Samples of the upright volume table.
pub(crate) const UPRIGHT_STEPS: usize = 17;
/// Offset of the two-point Gauss rule on a unit interval: 1 / (2 sqrt 3). Two points per axis at this offset
/// carry a block's own waterplane inertia exactly, so a hull one or two blocks wide still has the right
/// metacentric height.
const GAUSS: f64 = 0.288_675_134_594_812_9;

/// Exact extent of a block's displacing cells, kept beside the public [`Block`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct BlockExt {
    /// Bounds of the displacing cells in vessel-local metres.
    pub lo: [f32; 3],
    pub hi: [f32; 3],
    /// Plan (XZ) area the displacing cells cover, m^2.
    pub foot_m2: f32,
    /// The block's cell layers in [`BuoyancyBlocks::layers`]: first index and count, bottom layer first.
    pub first: u32,
    pub layers: u8,
}

/// One cell layer (0.5 m) of a block's displacing cells.
///
/// The layer is a box one cell thick, uniformly filled and centred on its cells' centroid. Its plan half
/// extents reach from the centroid to the nearer edge of the cells' bounds, so the box never leaves those
/// bounds: a full rectangle of cells is exact, and a staircase (a bow in plan) keeps its centroid and gets a
/// narrower box, as its own spread is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Layer {
    /// Centroid of the layer's displacing cells, vessel-local metres.
    pub centre: DVec3,
    /// Plan half extents along X and Z, metres.
    pub half: [f32; 2],
    /// Displacing volume, m^3.
    pub volume_m3: f32,
}

/// Summary of the displacing hull, derived from the blocks after every build.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct HullForm {
    pub total_m3: f64,
    /// Bounds of every displacing cell, vessel-local metres.
    pub lo: DVec3,
    pub hi: DVec3,
    /// Plan area of the displacing hull, m^2.
    pub plan_m2: f64,
    /// Volume centroid of the displacing hull.
    pub centroid: DVec3,
    /// Displaced volume on an even keel with the waterline at `lo.y + k (hi.y - lo.y) / 16`.
    pub upright_m3: [f64; UPRIGHT_STEPS],
}

impl HullForm {
    /// Local height of the upright waterline that displaces `volume`, or `None` when the hull cannot.
    pub(crate) fn upright_waterline(&self, volume: f64) -> Option<f64> {
        if !(volume.is_finite() && volume >= 0.0 && self.total_m3 > 0.0) || volume >= self.total_m3 { return None; }
        let step = (self.hi.y - self.lo.y) / (UPRIGHT_STEPS - 1) as f64;
        for k in 1..UPRIGHT_STEPS {
            let (a, b) = (self.upright_m3[k - 1], self.upright_m3[k]);
            if volume <= b {
                let f = if b > a { (volume - a) / (b - a) } else { 0.0 };
                return Some(self.lo.y + (k as f64 - 1.0 + f) * step);
            }
        }
        Some(self.hi.y)
    }
}

/// A water plane in the body frame: a body point `p` lies `c - u . p` below the surface, `u` being the world
/// up direction seen from the body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Plane { pub u: DVec3, pub c: f64 }

/// Up direction in the body frame for a heel (positive to starboard) and a trim (positive bow up).
pub(crate) fn up_of(heel: f64, trim: f64) -> DVec3 {
    let (sh, ch) = libm::sincos(heel);
    let (st, ct) = libm::sincos(trim);
    DVec3::new(st, ct * ch, -ct * sh)
}

/// The block-submersion kernel for one orientation.
///
/// It works on a uniformly filled box (a cell layer of a block, or a whole block when a set carries no
/// layers). The wetted volume is integrated along lines parallel to one box axis: each line is wet from its
/// low end up to the surface. Across the face the lines stand on, the wet fraction is integrated in closed
/// form along the axis the surface slopes most over ([`strip`]) and by a two-point Gauss rule along the
/// other. A heeled box is therefore exact at any angle, own waterplane inertia included, and trim on top of
/// heel costs only the Gauss rule's error over a slope that is small. The three line axes are blended by the
/// eighth power of their share of the box's projected height, so a box standing on end hands over smoothly
/// to lines that still cross the surface and the force never jumps as a hull rolls over.
#[derive(Clone, Copy)]
pub(crate) struct Kernel { u: DVec3, au: DVec3, plane: bool }

/// Blend weights below this are dropped (the rest are renormalised).
const MIN_AXIS_WEIGHT: f64 = 1e-4;

/// Integrals over `t` in -1/2..=1/2 of the wet fraction `f(t) = clamp(a + s t, 0, 1)` of a row of lines,
/// for `s >= 0`: `[int f, int t f, int f^2 / 2]`, and the span of `t` over which the surface cuts the lines.
/// Each piece is integrated directly, so nothing cancels when the slope is small.
#[inline]
fn strip(a: f64, s: f64) -> ([f64; 3], f64, f64) {
    // The lines are dry below t0 and wholly wet above t1.
    let (t0, t1) = if s > 1e-12 { (-a / s, (1.0 - a) / s) } else {
        (if a > 0.0 { f64::NEG_INFINITY } else { f64::INFINITY }, if a >= 1.0 { f64::NEG_INFINITY } else { f64::INFINITY })
    };
    let (lo, hi) = (t0.clamp(-0.5, 0.5), t1.clamp(-0.5, 0.5));
    let (d1, d2, d3) = (hi - lo, hi * hi - lo * lo, hi * hi * hi - lo * lo * lo);
    let full = 0.5 - hi;
    let i0 = a * d1 + 0.5 * s * d2 + full;
    let i1 = 0.5 * a * d2 + s * d3 / 3.0 + 0.5 * (0.25 - hi * hi);
    let i2 = 0.5 * (a * a * d1 + a * s * d2 + s * s * d3 / 3.0) + 0.5 * full;
    ([i0, i1, i2], lo, hi)
}

impl Kernel {
    /// Kernel for the forces: wet volumes and their centroids only.
    pub(crate) fn new(u: DVec3) -> Kernel { Kernel { u, au: u.abs(), plane: false } }
    /// Kernel for hydrostatics: also visits the waterplane.
    pub(crate) fn with_waterplane(u: DVec3) -> Kernel { Kernel { plane: true, ..Kernel::new(u) } }

    /// Half the height a box of half extents `half` stands over, measured along up.
    #[inline]
    pub(crate) fn reach(&self, half: [f64; 3]) -> f64 { self.au.x * half[0] + self.au.y * half[1] + self.au.z * half[2] }

    /// Visit the wet pieces of a box: `visit(wet volume, its centroid, waterplane area, waterplane point)`,
    /// all in the body frame. `centre` and `half` give the box, `volume` its displacing volume and `c` the
    /// plane offset of its water column. A dry box is not visited; a wholly wet one once. Waterplane pieces
    /// (zero volume, an area and the point it lies at) come only from [`Kernel::with_waterplane`].
    #[inline]
    pub(crate) fn eval(&self, centre: DVec3, half: [f64; 3], volume: f64, c: f64, visit: &mut impl FnMut(f64, DVec3, f64, DVec3)) {
        if !(volume > 0.0) { return; }
        let ext = [2.0 * half[0], 2.0 * half[1], 2.0 * half[2]];
        let proj = [self.au.x * ext[0], self.au.y * ext[1], self.au.z * ext[2]];
        let reach = 0.5 * (proj[0] + proj[1] + proj[2]);
        let d0 = c - self.u.dot(centre);
        if !(reach > 0.0) || d0 + reach <= 0.0 { return; }
        if d0 - reach >= 0.0 { visit(volume, centre, 0.0, centre); return; }
        let p8 = |v: f64| { let q = v * v; let q = q * q; q * q };
        let sq = [p8(proj[0]), p8(proj[1]), p8(proj[2])];
        let all = sq[0] + sq[1] + sq[2];
        let kept: f64 = sq.iter().filter(|&&q| q >= MIN_AXIS_WEIGHT * all).sum();
        for k in 0..3 {
            if sq[k] < MIN_AXIS_WEIGHT * all { continue; }
            // `j`: the axis the surface slopes most over, integrated in closed form; `i`: the Gauss axis.
            let (p, q) = ((k + 1) % 3, (k + 2) % 3);
            let (i, j) = if proj[p] > proj[q] { (q, p) } else { (p, q) };
            let share = 0.5 * volume * sq[k] / kept;
            let low = if self.u[k] >= 0.0 { 1.0 } else { -1.0 };
            let a0 = d0 / proj[k] + 0.5;
            let slope = -self.u[j] * ext[j] / proj[k];
            let (flip, slope) = if slope < 0.0 { (-1.0, -slope) } else { (1.0, slope) };
            let gi = ext[i] * GAUSS;
            let di = self.u[i] * gi / proj[k];
            for si in [-1.0, 1.0] {
                let a = a0 - si * di;
                let (m, lo, hi) = strip(a, slope);
                if !(m[0] > 0.0) { continue; }
                let mut at = centre;
                at[i] += si * gi;
                at[j] += flip * ext[j] * (m[1] / m[0]);
                at[k] += low * ext[k] * (m[2] / m[0] - 0.5);
                visit(share * m[0], at, 0.0, at);
                if self.plane && hi > lo {
                    // The waterplane of this row: two Gauss points along its cut span carry its own inertia.
                    let area = 0.5 * share * (hi - lo) / proj[k];
                    for g in [-GAUSS, GAUSS] {
                        let t = 0.5 * (lo + hi) + g * (hi - lo);
                        let mut cut = centre;
                        cut[i] += si * gi;
                        cut[j] += flip * ext[j] * t;
                        cut[k] += low * ext[k] * (a + slope * t - 0.5);
                        visit(0.0, cut, area, cut);
                    }
                }
            }
        }
    }
}

impl BuoyancyBlocks {
    /// Half extents of the box of block `i`'s displacing cells (half the block edge in plan when unknown).
    #[inline]
    pub(crate) fn half(&self, i: usize) -> [f64; 3] {
        let hy = 0.5 * self.blocks[i].height_m;
        match self.ext.get(i) {
            Some(e) => [0.5 * (e.hi[0] - e.lo[0]) as f64, hy, 0.5 * (e.hi[2] - e.lo[2]) as f64],
            None => { let h = 0.5 * self.edge_m(); [h, hy, h] }
        }
    }
    /// Middle and half extents of the bounds of block `i`'s displacing cells. A set without extents (built
    /// by hand) is taken as boxes about the block centres.
    #[inline]
    pub(crate) fn bounds(&self, i: usize) -> (DVec3, [f64; 3]) {
        match self.ext.get(i) {
            Some(e) => {
                let (lo, hi) = (DVec3::new(e.lo[0] as f64, e.lo[1] as f64, e.lo[2] as f64), DVec3::new(e.hi[0] as f64, e.hi[1] as f64, e.hi[2] as f64));
                (0.5 * (lo + hi), (0.5 * (hi - lo)).to_array())
            }
            None => (self.blocks[i].centre, self.half(i)),
        }
    }
    /// Cell layers of block `i`, or `None` when the set carries none for it (built by hand, or restored
    /// from bytes that do not add up).
    #[inline]
    pub(crate) fn layers_of(&self, i: usize) -> Option<&[Layer]> {
        let e = self.ext.get(i)?;
        if e.layers == 0 { return None; }
        self.layers.get(e.first as usize..e.first as usize + e.layers as usize)
    }
    /// Visit the wet pieces of block `i` under the plane offset `c` of its water column (see
    /// [`Kernel::eval`]). A block wholly under or wholly dry is settled from its bounds; one the surface
    /// cuts is taken layer by layer.
    #[inline]
    pub(crate) fn eval_block(&self, i: usize, k: &Kernel, c: f64, visit: &mut impl FnMut(f64, DVec3, f64, DVec3)) {
        let b = &self.blocks[i];
        let Some(layers) = self.layers_of(i) else { k.eval(b.centre, self.half(i), b.volume_m3, c, visit); return };
        if !(b.volume_m3 > 0.0) { return; }
        let (mid, half) = self.bounds(i);
        let (d0, reach) = (c - k.u.dot(mid), k.reach(half));
        if !(d0 + reach > 0.0) { return; }
        if d0 - reach >= 0.0 { visit(b.volume_m3, b.centre, 0.0, b.centre); return; }
        for l in layers { k.eval(l.centre, [l.half[0] as f64, 0.5 * CELL_M, l.half[1] as f64], l.volume_m3 as f64, c, visit); }
    }
    /// How far the hull's lowest point lies below the body origin along `u` (the world up seen from the
    /// body): under a plane of offset `c` the keel draws `c + keel_reach(u)`. Zero for an empty set.
    pub(crate) fn keel_reach(&self, u: DVec3) -> f64 {
        let k = Kernel::new(u);
        let mut deepest = f64::NEG_INFINITY;
        for i in 0..self.blocks.len() {
            let (mid, half) = self.bounds(i);
            deepest = deepest.max(k.reach(half) - u.dot(mid));
        }
        if deepest.is_finite() { deepest } else { 0.0 }
    }
    /// Block edge in metres.
    pub(crate) fn edge_m(&self) -> f64 { self.block_cells.clamp(1, MAX_BLOCK_CELLS) as f64 * CELL_M }
    /// Lattice coordinates (x, y, z) of a block from its centroid.
    pub(crate) fn lattice_of(&self, b: &Block) -> [u16; 3] {
        let e = self.edge_m();
        let q = |v: f64| (v / e).floor().clamp(0.0, u16::MAX as f64) as u16;
        [q(b.centre.x), q(b.centre.y), q(b.centre.z)]
    }
    /// Displaced volume and its first moment (body frame) under one plane for the whole hull.
    pub(crate) fn displaced(&self, plane: Plane) -> (f64, DVec3) {
        let k = Kernel::new(plane.u);
        let (mut vol, mut moment) = (0.0, DVec3::ZERO);
        for i in 0..self.blocks.len() {
            self.eval_block(i, &k, plane.c, &mut |v, at, _, _| { vol += v; moment += at * v; });
        }
        (vol, moment)
    }

    /// Recompute `columns`, every block's `column`, and `form` from `blocks` and `ext`.
    pub(crate) fn finish(&mut self) {
        let e = self.edge_m();
        let mut cols: BTreeMap<(u16, u16), (u32, f32)> = BTreeMap::new();
        for (i, b) in self.blocks.iter().enumerate() {
            let l = self.lattice_of(b);
            let foot = self.ext.get(i).map_or((e * e) as f32, |x| x.foot_m2);
            let c = cols.entry((l[2], l[0])).or_insert((0, 0.0));
            if foot > c.1 { c.1 = foot; }
        }
        self.columns.clear();
        let mut plan = 0.0;
        for (n, (key, v)) in cols.iter_mut().enumerate() {
            v.0 = n as u32;
            plan += v.1 as f64;
            self.columns.push([(key.1 as f64 + 0.5) * e, (key.0 as f64 + 0.5) * e]);
        }
        let mut form = HullForm { plan_m2: plan, lo: DVec3::splat(f64::INFINITY), hi: DVec3::splat(f64::NEG_INFINITY), ..HullForm::default() };
        let mut first = DVec3::ZERO;
        for i in 0..self.blocks.len() {
            let l = self.lattice_of(&self.blocks[i]);
            let half = self.half(i);
            let b = &mut self.blocks[i];
            b.column = cols.get(&(l[2], l[0])).map_or(0, |c| c.0);
            form.total_m3 += b.volume_m3;
            first += b.centre * b.volume_m3;
            let (lo, hi) = match self.ext.get(i) {
                Some(x) => (DVec3::new(x.lo[0] as f64, x.lo[1] as f64, x.lo[2] as f64), DVec3::new(x.hi[0] as f64, x.hi[1] as f64, x.hi[2] as f64)),
                None => { let r = DVec3::from(half); (b.centre - r, b.centre + r) }
            };
            form.lo = form.lo.min(lo);
            form.hi = form.hi.max(hi);
        }
        if form.total_m3 > 0.0 {
            form.centroid = first / form.total_m3;
            let step = (form.hi.y - form.lo.y) / (UPRIGHT_STEPS - 1) as f64;
            for k in 0..UPRIGHT_STEPS {
                form.upright_m3[k] = self.displaced(Plane { u: DVec3::Y, c: form.lo.y + k as f64 * step }).0;
            }
            form.upright_m3[UPRIGHT_STEPS - 1] = form.total_m3;
        } else {
            form.lo = DVec3::ZERO;
            form.hi = DVec3::ZERO;
        }
        self.form = form;
    }
}

/// True when a cell displaces water: a live solid cell, air inside a compartment, or a walled-in void.
fn displaces(grid: &VesselGrid, compartments: &CompartmentSet, x: usize, y: usize, z: usize, size: [usize; 3]) -> bool {
    let i = x + size[0] * (z + size[2] * y);
    match compartments.cell_to_compartment.get(i) {
        Some(&label) if (label as usize) < MAX_COMPARTMENTS || label == VOID_CELL => true,
        _ => grid.is_solid(CellPos::new(x as i16, y as i16, z as i16)),
    }
}

/// A scanned block: the public block, its extent (layer range not yet set) and its cell layers, bottom first.
type Scanned = (Block, BlockExt, [Layer; MAX_BLOCK_CELLS as usize], usize);

/// Block, extent and layers of the lattice cell `l`, or `None` when none of its cells displaces.
fn scan_block(grid: &VesselGrid, compartments: &CompartmentSet, bc: usize, l: [usize; 3], size: [usize; 3]) -> Option<Scanned> {
    let (mut n, mut sum) = (0u32, [0u64; 3]);
    let (mut lo, mut hi) = ([usize::MAX; 3], [0usize; 3]);
    let mut foot = 0u64;
    let mut layers = [Layer::default(); MAX_BLOCK_CELLS as usize];
    let mut count = 0;
    let end = |k: usize| ((l[k] + 1) * bc).min(size[k]);
    let m = |v: usize| (v as f64 * CELL_M) as f32;
    for y in l[1] * bc..end(1) {
        // This layer: cell count, coordinate sums and plan bounds.
        let (mut ln, mut lsum) = (0u32, [0u64; 2]);
        let (mut llo, mut lhi) = ([usize::MAX; 2], [0usize; 2]);
        for z in l[2] * bc..end(2) { for x in l[0] * bc..end(0) {
            if !displaces(grid, compartments, x, y, z, size) { continue; }
            ln += 1;
            for (k, v) in [x, z].into_iter().enumerate() {
                lsum[k] += 2 * v as u64 + 1;
                llo[k] = llo[k].min(v);
                lhi[k] = lhi[k].max(v);
            }
            foot |= 1 << ((x - l[0] * bc) + bc * (z - l[2] * bc));
        } }
        if ln == 0 { continue; }
        n += ln;
        sum = [sum[0] + lsum[0], sum[1] + ln as u64 * (2 * y as u64 + 1), sum[2] + lsum[1]];
        lo = [lo[0].min(llo[0]), lo[1].min(y), lo[2].min(llo[1])];
        hi = [hi[0].max(lhi[0]), hi[1].max(y), hi[2].max(lhi[1])];
        // Cell centres are odd half-cells, so each centroid is an exact integer sum.
        let c = |k: usize| lsum[k] as f64 / (2.0 * ln as f64) * CELL_M;
        let centre = DVec3::new(c(0), (y as f64 + 0.5) * CELL_M, c(1));
        let reach = |k: usize, at: f64| (at - llo[k] as f64 * CELL_M).min((lhi[k] + 1) as f64 * CELL_M - at) as f32;
        layers[count] = Layer { centre, half: [reach(0, centre.x), reach(1, centre.z)], volume_m3: (ln as f64 * CELL_VOLUME_M3) as f32 };
        count += 1;
    }
    if n == 0 { return None; }
    let c = |k: usize| sum[k] as f64 / (2.0 * n as f64) * CELL_M;
    let block = Block { centre: DVec3::new(c(0), c(1), c(2)), volume_m3: n as f64 * CELL_VOLUME_M3, height_m: (hi[1] - lo[1] + 1) as f64 * CELL_M, column: 0 };
    let ext = BlockExt { lo: [m(lo[0]), m(lo[1]), m(lo[2])], hi: [m(hi[0] + 1), m(hi[1] + 1), m(hi[2] + 1)], foot_m2: (foot.count_ones() as f64 * CELL_M * CELL_M) as f32, first: 0, layers: 0 };
    Some((block, ext, layers, count))
}

impl BuoyancyBlocks {
    /// Append a scanned block, its extent and its layers.
    fn push_scanned(&mut self, (block, mut ext, layers, count): Scanned) {
        ext.first = self.layers.len() as u32;
        ext.layers = count as u8;
        self.layers.extend_from_slice(&layers[..count]);
        self.blocks.push(block);
        self.ext.push(ext);
    }
}

fn grid_size(grid: &VesselGrid) -> Option<[usize; 3]> {
    let s = grid.size();
    let size = [s[0] as usize, s[1] as usize, s[2] as usize];
    // An inert grid (malformed design) has no chunks and floats nothing.
    if grid.chunks.is_empty() || size.contains(&0) || grid.design.cells.len() != size[0] * size[1] * size[2] { None } else { Some(size) }
}

pub(crate) fn build(grid: &VesselGrid, compartments: &CompartmentSet, block_cells: u8) -> BuoyancyBlocks {
    let bc = block_cells.clamp(1, MAX_BLOCK_CELLS);
    let mut out = BuoyancyBlocks { block_cells: bc, size: grid.size(), ..BuoyancyBlocks::default() };
    let Some(size) = grid_size(grid) else { out.finish(); return out };
    let bc = bc as usize;
    let nb = [size[0].div_ceil(bc), size[1].div_ceil(bc), size[2].div_ceil(bc)];
    for by in 0..nb[1] { for bz in 0..nb[2] { for bx in 0..nb[0] {
        if let Some(scanned) = scan_block(grid, compartments, bc, [bx, by, bz], size) { out.push_scanned(scanned); }
    } } }
    out.finish();
    out
}

pub(crate) fn rebuild_dirty(blocks: &mut BuoyancyBlocks, grid: &VesselGrid, compartments: &CompartmentSet) {
    let bc = blocks.block_cells.clamp(1, MAX_BLOCK_CELLS);
    // Blocks from another grid, a hand-built set, a bad block size or layers that do not add up: start over.
    let whole = blocks.ext.len() == blocks.blocks.len() && (0..blocks.blocks.len()).all(|i| blocks.layers_of(i).is_some());
    if blocks.block_cells != bc || blocks.size != grid.size() || !whole { *blocks = build(grid, compartments, bc); return; }
    let Some(size) = grid_size(grid) else { *blocks = build(grid, compartments, bc); return };
    let bc = bc as usize;
    let (cdx, cdz) = (grid.chunk_dims[0] as usize, grid.chunk_dims[2] as usize);
    if cdx == 0 || cdz == 0 { return; }
    let mut touched: BTreeSet<(usize, usize, usize)> = BTreeSet::new();
    for ci in grid.dirty_chunks.iter_ones().filter(|&c| c < grid.chunks.len()) {
        let origin = [(ci % cdx) * CHUNK, (ci / (cdx * cdz)) * CHUNK, ((ci / cdx) % cdz) * CHUNK];
        let range = |k: usize| origin[k] / bc..=((origin[k] + CHUNK - 1).min(size[k].saturating_sub(1))) / bc;
        if origin.iter().zip(size).any(|(o, s)| *o >= s) { continue; }
        for by in range(1) { for bz in range(2) { for bx in range(0) { touched.insert((by, bz, bx)); } } }
    }
    if touched.is_empty() { return; }
    // Merge the rescanned blocks into the (y, z, x)-ordered list: untouched blocks keep their old entry.
    enum Entry { Kept(usize), Rescanned(Box<Scanned>) }
    let mut merged: BTreeMap<(usize, usize, usize), Entry> = BTreeMap::new();
    for (i, b) in blocks.blocks.iter().enumerate() {
        let l = blocks.lattice_of(b);
        let k = (l[1] as usize, l[2] as usize, l[0] as usize);
        if !touched.contains(&k) { merged.insert(k, Entry::Kept(i)); }
    }
    for &(by, bz, bx) in &touched {
        if let Some(scanned) = scan_block(grid, compartments, bc, [bx, by, bz], size) { merged.insert((by, bz, bx), Entry::Rescanned(Box::new(scanned))); }
    }
    let old = core::mem::take(blocks);
    *blocks = BuoyancyBlocks { block_cells: old.block_cells, size: old.size, ..BuoyancyBlocks::default() };
    for (_, entry) in merged {
        match entry {
            Entry::Kept(i) => {
                let mut layers = [Layer::default(); MAX_BLOCK_CELLS as usize];
                let kept = old.layers_of(i).unwrap_or(&[]);
                let count = kept.len().min(layers.len());
                layers[..count].copy_from_slice(&kept[..count]);
                blocks.push_scanned((old.blocks[i], old.ext[i], layers, count));
            }
            Entry::Rescanned(scanned) => blocks.push_scanned(*scanned),
        }
    }
    blocks.finish();
}
