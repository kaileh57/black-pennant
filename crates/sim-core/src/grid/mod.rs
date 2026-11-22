//! Unit `sim-core-grid`. Per-vessel cell storage: 16^3 simulation chunks of u16 ids with derived bitsets, plus
//! dense damage and scorch arrays. DESIGN.md L1114 (hot data), L535 (shaped cells), L561 (buckets).
//!
//! Layout decisions (contract):
//! - The design cells (material, armour, paint, shape) are immutable for the life of a vessel and shared as
//!   `Arc<CellGrid>`; damage lives beside them as one `u8` bucket per cell. That pair is exactly the
//!   `content::cell::CellGridView` hull-mesh consumes, so remeshing needs no conversion.
//! - Each chunk stores one u16 per cell: 0 = empty or destroyed, otherwise an index into [`VesselGrid::types`]
//!   (interned (material, armour, shape, owning part) combinations).
//! - Derived bitsets per chunk: `solid` (a cell exists), `watertight` (solid and holds water out), `air`
//!   (empty cell inside the hull envelope). They are rebuilt for a chunk whenever one of its cells changes.
//! - [`CellIndex`] is the dense linear index of `CellGrid`; chunk c covers cells `c * 16 .. c * 16 + 16`.
//! - `air` is the candidate interior air the sim keeps cheaply. It is an empty cell, never designed or now
//!   destroyed, with a solid cell somewhere on both sides along each of the three axes, so a six-ray
//!   enclosure test. The bits are maintained incrementally from per-line solid extents: a removal only re-tests
//!   the X row, Y column and Z line through the removed cell, and only when it was an end of that line. The
//!   compartments unit's flood fill from outside the box stays authoritative for what is sealed. A removed
//!   outer plate is not air, and neither is any cell that now sees out through the hole along an axis, so both
//!   read as open to the sea.
//! - A chunk is `None` only while it holds no solid cell and no air bit.
//! - Removed cells, whether destroyed or moved to a split-off piece, hold bucket 8. The view therefore shows
//!   them gone, which is the only signal hull-mesh reads.
//! - Destroyed cells are never restored here. They come back only through a tender rebuild (L714), so
//!   `set_bucket` below 8 on a removed cell does nothing.
//! - A malformed design (any axis over `MAX_GRID_DIM`, or `cells.len()` not matching `size`) builds an inert
//!   grid with no solid cells. No call panics on it.
#[cfg(test)]
mod tests;

use crate::bits::BitSet;
use content::cell::{ArmourTier, Cell, CellChange, CellGrid, CellGridView, CellPos, CellShape, Face, Material, RenderChunkCoord, BUCKET_DESTROYED, MAX_GRID_DIM, RENDER_CHUNK};
use content::ids::{CellIndex, PartSlot};
use content::part::Flammable;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

pub const CHUNK: usize = content::cell::SIM_CHUNK;
pub const CHUNK_CELLS: usize = CHUNK * CHUNK * CHUNK;
pub const CHUNK_WORDS: usize = CHUNK_CELLS / 64;

/// Interned cell type; `VesselGrid::types[0]` is the empty type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CellType {
    pub material: Material,
    pub armour: ArmourTier,
    pub shape: CellShape,
    /// The part whose footprint owns this cell, if any (L132).
    pub part: Option<PartSlot>,
    pub watertight: bool,
    pub flammable: Flammable,
}

/// One 16^3 chunk (8 m). Index inside a chunk: `x + 16 * (z + 16 * y)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimChunk {
    pub ids: Vec<u16>,
    pub solid: Vec<u64>,
    pub watertight: Vec<u64>,
    pub air: Vec<u64>,
    pub solid_count: u16,
}

/// Not serialisable as a whole: keyframes store the blueprint hash plus `damage`, `scorch` and the pending
/// queues, and rebuild the rest from the prefab.
#[derive(Clone, Debug, PartialEq)]
pub struct VesselGrid {
    pub design: Arc<CellGrid>,
    /// Chunk counts along X, Y, Z.
    pub chunk_dims: [u8; 3],
    /// Chunks in ascending (y, z, x) order; `None` = wholly empty.
    pub chunks: Vec<Option<Box<SimChunk>>>,
    pub types: Vec<CellType>,
    /// Damage bucket 0..=8 per cell, indexed by [`CellIndex`].
    pub damage: Vec<u8>,
    /// Scorch 0..=255 per cell for the renderer; empty until first fire or burst.
    pub scorch: Vec<u8>,
    /// Sealed-air bits per cell, maintained by the compartments unit (see `CellGridView::sealed_air`).
    pub sealed_air: Vec<u64>,
    /// Live solid cell count.
    pub solid_count: u32,
    /// Solid cell count at spawn, for "share of own cells lost" (L227).
    pub original_count: u32,
    /// Cell changes since the last drain, in application order: the replicated damage stream (L561).
    pub changes: Vec<CellChange>,
    /// Render chunks touched since the last drain, ascending and deduplicated at drain time.
    pub dirty_render: Vec<RenderChunkCoord>,
    /// Sim chunks whose colliders and buoyancy blocks need a rebuild.
    pub dirty_chunks: BitSet,
    /// Solid extent `[min, max]` along X of each row, indexed `z + sz * y`; `[u16::MAX, 0]` when the row is empty.
    pub(crate) xline: Vec<[u16; 2]>,
    /// Solid extent along Y of each column, indexed `x + sx * z`.
    pub(crate) yline: Vec<[u16; 2]>,
    /// Solid extent along Z of each line, indexed `x + sx * y`.
    pub(crate) zline: Vec<[u16; 2]>,
}

/// Line extent of an empty line.
const EMPTY_LINE: [u16; 2] = [u16::MAX, 0];

/// The empty cell type, `types[0]`.
const EMPTY_TYPE: CellType = CellType { material: Material::Empty, armour: ArmourTier::None, shape: CellShape::CUBE, part: None, watertight: false, flammable: Flammable::No };

impl SimChunk {
    fn empty() -> Self { Self { ids: vec![0; CHUNK_CELLS], solid: vec![0; CHUNK_WORDS], watertight: vec![0; CHUNK_WORDS], air: vec![0; CHUNK_WORDS], solid_count: 0 } }
    fn has_air(&self) -> bool { self.air.iter().any(|&w| w != 0) }
}

#[inline]
fn bit(words: &[u64], i: usize) -> bool { (words[i / 64] >> (i % 64)) & 1 == 1 }
#[inline]
fn set_bit(words: &mut [u64], i: usize, v: bool) { if v { words[i / 64] |= 1 << (i % 64); } else { words[i / 64] &= !(1 << (i % 64)); } }

/// Packed interning key of a cell type (CellType has no `Ord`).
fn type_key(t: &CellType) -> u64 {
    let fl = match t.flammable { Flammable::No => 0u64, Flammable::Fuel => 1, Flammable::Ammunition => 2 };
    (t.material as u64) | (t.armour as u64) << 8 | (t.shape.0 as u64) << 16 | (t.part.map_or(0, |p| p.0 as u64 + 1)) << 24 | (t.watertight as u64) << 41 | fl << 42
}

/// Most interned types (ids are u16 and 0 is the empty type).
const MAX_TYPES: usize = u16::MAX as usize + 1;

impl VesselGrid {
    /// Build chunks, interned types and bitsets from a dense design grid in which part footprints are already
    /// stamped (`content::blueprint::io::build_grid`). `part_of(cell)` names the owning part of a `PartBody`
    /// cell. Unit: `sim-core-grid`.
    ///
    /// Structural cells (steel, aluminium, composite, glass) are watertight and not flammable. A `PartBody`
    /// cell takes `part_flags` of its owning part; one no part claims is not watertight and not flammable.
    /// When two parts list the same cell, the lower slot owns it. Out-of-range indices are ignored. Types are
    /// interned first-seen in ascending [`CellIndex`].
    pub fn build(design: Arc<CellGrid>, part_cells: &[(PartSlot, Vec<CellIndex>)], part_flags: &dyn Fn(PartSlot) -> (bool, Flammable)) -> VesselGrid {
        let size = design.size;
        let [sx, sy, sz] = [size[0] as usize, size[1] as usize, size[2] as usize];
        let n = sx * sy * sz;
        let len = design.cells.len();
        let valid = size.iter().all(|&d| d <= MAX_GRID_DIM) && n == len;
        let cd = |d: usize| d.div_ceil(CHUNK).min(u8::MAX as usize) as u8;
        let chunk_dims = if valid { [cd(sx), cd(sy), cd(sz)] } else { [0; 3] };
        let chunk_count = chunk_dims.iter().map(|&d| d as usize).product::<usize>();
        let mut g = VesselGrid {
            design: design.clone(),
            chunk_dims,
            chunks: vec![None; chunk_count],
            types: vec![EMPTY_TYPE],
            damage: vec![0; len],
            scorch: Vec::new(),
            sealed_air: Vec::new(),
            solid_count: 0,
            original_count: 0,
            changes: Vec::new(),
            dirty_render: Vec::new(),
            dirty_chunks: BitSet::new(chunk_count),
            xline: if valid { vec![EMPTY_LINE; sy * sz] } else { Vec::new() },
            yline: if valid { vec![EMPTY_LINE; sx * sz] } else { Vec::new() },
            zline: if valid { vec![EMPTY_LINE; sx * sy] } else { Vec::new() },
        };
        if !valid || n == 0 { return g; }

        let mut owner: Vec<Option<PartSlot>> = Vec::new();
        if !part_cells.is_empty() {
            owner = vec![None; n];
            for (slot, cells) in part_cells {
                for c in cells {
                    if let Some(o) = owner.get_mut(c.index()) {
                        if o.is_none_or(|e| *slot < e) { *o = Some(*slot); }
                    }
                }
            }
        }
        let mut flags: BTreeMap<PartSlot, (bool, Flammable)> = BTreeMap::new();
        let mut interned: BTreeMap<u64, u16> = BTreeMap::new();
        for y in 0..sy {
            for z in 0..sz {
                for x in 0..sx {
                    let i = x + sx * (z + sz * y);
                    let cell = design.cells[i];
                    if cell.is_empty() { continue; }
                    let part = owner.get(i).copied().flatten();
                    let (watertight, flammable) = match cell.material {
                        Material::PartBody => match part {
                            Some(p) => *flags.entry(p).or_insert_with(|| part_flags(p)),
                            None => (false, Flammable::No),
                        },
                        _ => (true, Flammable::No),
                    };
                    let t = CellType { material: cell.material, armour: cell.armour, shape: cell.shape, part, watertight, flammable };
                    let id = g.intern(&mut interned, t);
                    g.put_solid(x, y, z, id);
                }
            }
        }
        g.original_count = g.solid_count;
        // Line extents, then air for every empty cell.
        for y in 0..sy { for z in 0..sz { g.xline[z + sz * y] = g.scan_line(0, [0, y, z]); } }
        for z in 0..sz { for x in 0..sx { g.yline[x + sx * z] = g.scan_line(1, [x, 0, z]); } }
        for y in 0..sy { for x in 0..sx { g.zline[x + sx * y] = g.scan_line(2, [x, y, 0]); } }
        for y in 0..sy {
            for z in 0..sz {
                for x in 0..sx {
                    if g.enclosed(x, y, z) && !g.solid_at(x, y, z) { g.set_air(x, y, z, true); }
                }
            }
        }
        g
    }
    pub fn size(&self) -> [u16; 3] { self.design.size }
    pub fn index(&self, p: CellPos) -> Option<CellIndex> { self.design.index(p) }
    pub fn pos(&self, i: CellIndex) -> CellPos { self.design.pos(i) }
    /// Design cell regardless of damage; the empty cell when out of range.
    pub fn design_cell(&self, i: CellIndex) -> Cell { self.design.cells.get(i.index()).copied().unwrap_or(Cell::EMPTY) }
    /// Interned type of a live cell; the empty type when out of bounds, empty or destroyed. Unit: `sim-core-grid`.
    pub fn cell_type(&self, p: CellPos) -> CellType {
        let Some([x, y, z]) = self.coords(p) else { return EMPTY_TYPE };
        let (ci, li) = self.chunk_loc(x, y, z);
        match self.chunks.get(ci).and_then(|c| c.as_deref()) {
            Some(c) => self.types.get(c.ids[li] as usize).copied().unwrap_or(EMPTY_TYPE),
            None => EMPTY_TYPE,
        }
    }
    pub fn is_solid(&self, p: CellPos) -> bool { self.coords(p).is_some_and(|[x, y, z]| self.solid_at(x, y, z)) }
    pub fn is_watertight(&self, p: CellPos) -> bool { self.chunk_bit(p, |c| &c.watertight) }
    /// True when an empty cell is candidate interior air (see the module docs).
    pub fn is_air(&self, p: CellPos) -> bool { self.chunk_bit(p, |c| &c.air) }
    /// Damage bucket of a cell; 0 when out of range.
    pub fn bucket(&self, i: CellIndex) -> u8 { self.damage.get(i.index()).copied().unwrap_or(0) }
    /// Add damage buckets to a cell (saturating at 8). On reaching 8 the cell is removed from its chunk, bitsets
    /// update, and the chunk is marked dirty. Records a [`CellChange`]. Returns the new bucket.
    /// Unit: `sim-core-grid`.
    ///
    /// Out-of-range, never-designed and already-removed cells are left alone and return their stored bucket
    /// (0 or 8).
    pub fn add_damage(&mut self, i: CellIndex, buckets: u8) -> u8 {
        let Some([x, y, z]) = self.index_coords(i) else { return self.damage.get(i.index()).copied().unwrap_or(0) };
        let cur = self.damage[i.index()];
        if buckets == 0 || !self.solid_at(x, y, z) { return cur; }
        let new = cur.saturating_add(buckets).min(BUCKET_DESTROYED);
        if new == cur { return cur; }
        self.damage[i.index()] = new;
        self.changes.push(CellChange { cell: i, bucket: new });
        if new == BUCKET_DESTROYED { self.remove_batch(&[[x, y, z]]); }
        new
    }
    /// Set a bucket directly (repairs lower it; persistent damage applies it at spawn). Unit: `sim-core-grid`.
    ///
    /// Values above 8 clamp to 8. A change records a [`CellChange`]; reaching 8 removes the cell. Removed
    /// cells stay removed (see the module docs).
    pub fn set_bucket(&mut self, i: CellIndex, bucket: u8) {
        let Some([x, y, z]) = self.index_coords(i) else { return };
        let bucket = bucket.min(BUCKET_DESTROYED);
        if !self.solid_at(x, y, z) || self.damage[i.index()] == bucket { return; }
        self.damage[i.index()] = bucket;
        self.changes.push(CellChange { cell: i, bucket });
        if bucket == BUCKET_DESTROYED { self.remove_batch(&[[x, y, z]]); }
    }
    /// Remove a set of cells without damage events (they moved to a split-off piece). Unit: `sim-core-grid`.
    ///
    /// The cells get bucket 8, so the view shows them gone, and their chunks are marked dirty. No
    /// [`CellChange`] is recorded. Invalid, empty and already-removed cells are skipped.
    pub fn remove_cells(&mut self, cells: &[CellIndex]) {
        let mut batch = Vec::with_capacity(cells.len());
        for &i in cells {
            let Some([x, y, z]) = self.index_coords(i) else { continue };
            if !self.solid_at(x, y, z) || self.damage[i.index()] == BUCKET_DESTROYED { continue; }
            self.damage[i.index()] = BUCKET_DESTROYED;
            batch.push([x, y, z]);
        }
        self.remove_batch(&batch);
    }
    /// Live solid cells in ascending [`CellIndex`]. Unit: `sim-core-grid`.
    pub fn solid_cells(&self) -> Vec<CellIndex> {
        let mut out = Vec::with_capacity(self.solid_count as usize);
        let Some([sx, sy, sz]) = self.dims() else { return out };
        for y in 0..sy {
            for z in 0..sz {
                for cx in 0..self.chunk_dims[0] as usize {
                    let (ci, li0) = self.chunk_loc(cx * CHUNK, y, z);
                    let Some(c) = self.chunks[ci].as_deref() else { continue };
                    if c.solid_count == 0 { continue; }
                    for lx in 0..CHUNK.min(sx - cx * CHUNK) {
                        if bit(&c.solid, li0 + lx) { out.push(CellIndex((cx * CHUNK + lx + sx * (z + sz * y)) as u32)); }
                    }
                }
            }
        }
        out
    }
    /// Face-adjacent live solid neighbours of a cell, in `Face::ALL` order. Unit: `sim-core-grid`.
    pub fn solid_neighbours(&self, i: CellIndex) -> [Option<CellIndex>; 6] {
        let mut out = [None; 6];
        let Some([x, y, z]) = self.index_coords(i) else { return out };
        for (k, f) in Face::ALL.iter().enumerate() {
            let (dx, dy, dz) = f.offset();
            let q = CellPos::new(x as i16 + dx, y as i16 + dy, z as i16 + dz);
            if self.is_solid(q) { out[k] = self.design.index(q); }
        }
        out
    }
    /// Amanatides-Woo traversal of a segment in the vessel's cell frame (cell units), visiting cells in order
    /// until `visit` returns false (L559). Shared by shell stepping, HE line-of-sight and mount arc sweeps.
    /// Unit: `sim-core-grid`.
    ///
    /// The segment is clipped to the grid box `[0, size]`, and only in-bounds cells are visited (solid or not).
    /// Each visit passes the face through which the segment enters that cell. If the segment starts outside,
    /// the first cell's face is the box face it crossed. If it starts inside, the face is the one opposite the
    /// dominant direction (X, then Y, then Z on ties); a zero-length segment gets `XNeg`. When the segment
    /// crosses an edge or corner exactly, axes step in X, Y, Z order, so the cells visited are always face
    /// connected. Non-finite input visits nothing, and the step count is capped at `sx + sy + sz + 3`.
    pub fn traverse(&self, from: [f64; 3], to: [f64; 3], visit: &mut dyn FnMut(CellPos, content::cell::Face) -> bool) {
        let Some(dims) = self.dims() else { return };
        if dims.iter().any(|&d| d == 0) || !from.iter().chain(to.iter()).all(|v| v.is_finite()) { return; }
        let size = [dims[0] as f64, dims[1] as f64, dims[2] as f64];
        let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
        let entry_face = |axis: usize, positive_step: bool| -> Face {
            match (axis, positive_step) { (0, true) => Face::XNeg, (0, false) => Face::XPos, (1, true) => Face::YNeg, (1, false) => Face::YPos, (2, true) => Face::ZNeg, _ => Face::ZPos }
        };
        // Slab clip.
        let (mut t0, mut t1) = (0.0f64, 1.0f64);
        let mut entry_axis = None;
        for a in 0..3 {
            if d[a] == 0.0 {
                if from[a] < 0.0 || from[a] >= size[a] { return; }
            } else {
                let inv = 1.0 / d[a];
                let (mut ta, mut tb) = ((0.0 - from[a]) * inv, (size[a] - from[a]) * inv);
                if ta > tb { core::mem::swap(&mut ta, &mut tb); }
                if ta > t0 { t0 = ta; entry_axis = Some(a); }
                if tb < t1 { t1 = tb; }
            }
        }
        if t0 > t1 { return; }
        let start = [from[0] + d[0] * t0, from[1] + d[1] * t0, from[2] + d[2] * t0];
        let mut cell = [0i64; 3];
        for a in 0..3 { cell[a] = (start[a].floor() as i64).clamp(0, dims[a] as i64 - 1); }
        let mut face = match entry_axis {
            Some(a) => entry_face(a, d[a] > 0.0),
            None => {
                let mut best = 0;
                for a in 1..3 { if d[a].abs() > d[best].abs() { best = a; } }
                if d[best] == 0.0 { Face::XNeg } else { entry_face(best, d[best] > 0.0) }
            }
        };
        let mut step = [0i64; 3];
        let mut t_max = [f64::INFINITY; 3];
        let mut t_delta = [f64::INFINITY; 3];
        for a in 0..3 {
            if d[a] > 0.0 {
                step[a] = 1;
                t_max[a] = ((cell[a] + 1) as f64 - from[a]) / d[a];
                t_delta[a] = 1.0 / d[a];
            } else if d[a] < 0.0 {
                step[a] = -1;
                t_max[a] = (cell[a] as f64 - from[a]) / d[a];
                t_delta[a] = -1.0 / d[a];
            }
        }
        let cap = dims[0] + dims[1] + dims[2] + 3;
        for _ in 0..cap {
            if !visit(CellPos::new(cell[0] as i16, cell[1] as i16, cell[2] as i16), face) { return; }
            let mut a = 0;
            if t_max[1] < t_max[a] { a = 1; }
            if t_max[2] < t_max[a] { a = 2; }
            if !(t_max[a] <= t1) { return; }
            cell[a] += step[a];
            if cell[a] < 0 || cell[a] >= dims[a] as i64 { return; }
            face = entry_face(a, step[a] > 0);
            t_max[a] += t_delta[a];
        }
    }
    /// Take the pending damage stream. Unit: `sim-core-grid`.
    pub fn drain_changes(&mut self) -> Vec<CellChange> { core::mem::take(&mut self.changes) }
    /// Take the dirty render chunks, ascending and deduplicated. Unit: `sim-core-grid`.
    pub fn drain_dirty_render(&mut self) -> Vec<RenderChunkCoord> {
        let mut v = core::mem::take(&mut self.dirty_render);
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Take the sim chunks marked dirty, ascending chunk index, and clear the marks.
    pub fn drain_dirty_chunks(&mut self) -> Vec<usize> {
        let v: Vec<usize> = self.dirty_chunks.iter_ones().collect();
        self.dirty_chunks.clear();
        v
    }

    /// Chunk index (ascending (y, z, x) order) of a cell position, if in bounds.
    pub fn chunk_index_of(&self, p: CellPos) -> Option<usize> { self.coords(p).map(|[x, y, z]| self.chunk_loc(x, y, z).0) }

    // ---- private helpers ----

    /// Grid extent when the grid is valid (built with chunks); `None` for an inert grid.
    fn dims(&self) -> Option<[usize; 3]> {
        let s = self.design.size;
        let d = [s[0] as usize, s[1] as usize, s[2] as usize];
        let ok = self.xline.len() == d[1] * d[2] && self.yline.len() == d[0] * d[2] && self.zline.len() == d[0] * d[1] && d[0] * d[1] * d[2] == self.damage.len() && !self.chunks.is_empty();
        ok.then_some(d)
    }

    fn coords(&self, p: CellPos) -> Option<[usize; 3]> {
        let d = self.dims()?;
        if p.x < 0 || p.y < 0 || p.z < 0 { return None; }
        let c = [p.x as usize, p.y as usize, p.z as usize];
        (c[0] < d[0] && c[1] < d[1] && c[2] < d[2]).then_some(c)
    }

    fn index_coords(&self, i: CellIndex) -> Option<[usize; 3]> {
        let [sx, _, sz] = self.dims()?;
        let i = i.index();
        if i >= self.damage.len() { return None; }
        Some([i % sx, i / (sx * sz), (i / sx) % sz])
    }

    #[inline]
    fn chunk_loc(&self, x: usize, y: usize, z: usize) -> (usize, usize) {
        let [cdx, _, cdz] = [self.chunk_dims[0] as usize, self.chunk_dims[1] as usize, self.chunk_dims[2] as usize];
        let ci = x / CHUNK + cdx * (z / CHUNK + cdz * (y / CHUNK));
        let li = x % CHUNK + CHUNK * (z % CHUNK + CHUNK * (y % CHUNK));
        (ci, li)
    }

    /// In-bounds coordinates assumed.
    #[inline]
    fn solid_at(&self, x: usize, y: usize, z: usize) -> bool {
        let (ci, li) = self.chunk_loc(x, y, z);
        self.chunks.get(ci).and_then(|c| c.as_deref()).is_some_and(|c| bit(&c.solid, li))
    }

    fn chunk_bit(&self, p: CellPos, which: impl Fn(&SimChunk) -> &Vec<u64>) -> bool {
        let Some([x, y, z]) = self.coords(p) else { return false };
        let (ci, li) = self.chunk_loc(x, y, z);
        self.chunks.get(ci).and_then(|c| c.as_deref()).is_some_and(|c| bit(which(c), li))
    }

    fn chunk_alloc(&mut self, ci: usize) -> &mut SimChunk { self.chunks[ci].get_or_insert_with(|| Box::new(SimChunk::empty())) }

    fn intern(&mut self, map: &mut BTreeMap<u64, u16>, t: CellType) -> u16 {
        let key = type_key(&t);
        if let Some(&id) = map.get(&key) { return id; }
        if self.types.len() < MAX_TYPES {
            let id = self.types.len() as u16;
            self.types.push(t);
            map.insert(key, id);
            return id;
        }
        // Table full (needs tens of thousands of distinct combinations): fall back to a cube of the same
        // material, part and flags, then to any type of the same material.
        let cube = CellType { shape: CellShape::CUBE, armour: ArmourTier::None, ..t };
        if let Some(&id) = map.get(&type_key(&cube)) { return id; }
        self.types.iter().position(|e| e.material == t.material).unwrap_or(1) as u16
    }

    fn put_solid(&mut self, x: usize, y: usize, z: usize, id: u16) {
        let (ci, li) = self.chunk_loc(x, y, z);
        let wt = self.types[id as usize].watertight;
        let c = self.chunk_alloc(ci);
        c.ids[li] = id;
        set_bit(&mut c.solid, li, true);
        set_bit(&mut c.watertight, li, wt);
        set_bit(&mut c.air, li, false);
        c.solid_count += 1;
        self.solid_count += 1;
    }

    fn set_air(&mut self, x: usize, y: usize, z: usize, v: bool) {
        let (ci, li) = self.chunk_loc(x, y, z);
        if v {
            set_bit(&mut self.chunk_alloc(ci).air, li, true);
        } else if let Some(c) = self.chunks[ci].as_deref_mut() {
            set_bit(&mut c.air, li, false);
        }
    }

    /// Solid extent of the line along `axis` through `at` (the `axis` coordinate of `at` is ignored).
    fn scan_line(&self, axis: usize, at: [usize; 3]) -> [u16; 2] {
        let Some(d) = self.dims() else { return EMPTY_LINE };
        let mut ext = EMPTY_LINE;
        let mut p = at;
        for k in 0..d[axis] {
            p[axis] = k;
            if self.solid_at(p[0], p[1], p[2]) {
                if ext[0] == u16::MAX { ext[0] = k as u16; }
                ext[1] = k as u16;
            }
        }
        ext
    }

    fn line_slot(&self, axis: usize, p: [usize; 3]) -> usize {
        let s = self.design.size;
        match axis { 0 => p[2] + s[2] as usize * p[1], 1 => p[0] + s[0] as usize * p[2], _ => p[0] + s[0] as usize * p[1] }
    }

    fn line_ext(&self, axis: usize, p: [usize; 3]) -> [u16; 2] {
        let slot = self.line_slot(axis, p);
        let v = match axis { 0 => &self.xline, 1 => &self.yline, _ => &self.zline };
        v.get(slot).copied().unwrap_or(EMPTY_LINE)
    }

    /// Solid on both sides along all three axes.
    fn enclosed(&self, x: usize, y: usize, z: usize) -> bool {
        let p = [x, y, z];
        (0..3).all(|a| {
            let [lo, hi] = self.line_ext(a, p);
            lo != u16::MAX && (lo as usize) < p[a] && p[a] < hi as usize
        })
    }

    /// Mark the render chunk of a removed cell plus every neighbour whose one-cell padding includes it.
    fn mark_render(&mut self, x: usize, y: usize, z: usize) {
        let s = self.design.size;
        let rc = |v: usize, n: u16| -> (usize, usize, usize) {
            let c = v / RENDER_CHUNK;
            let last = (n as usize).saturating_sub(1) / RENDER_CHUNK;
            let lo = if v % RENDER_CHUNK == 0 && c > 0 { c - 1 } else { c };
            let hi = if v % RENDER_CHUNK == RENDER_CHUNK - 1 && c < last { c + 1 } else { c };
            (lo, hi, c)
        };
        let (x0, x1, _) = rc(x, s[0]);
        let (y0, y1, _) = rc(y, s[1]);
        let (z0, z1, _) = rc(z, s[2]);
        for cy in y0..=y1 { for cz in z0..=z1 { for cx in x0..=x1 {
            self.dirty_render.push(RenderChunkCoord { x: cx as u8, y: cy as u8, z: cz as u8 });
        } } }
    }

    /// Remove live solid cells (damage already set by the caller), then refresh line extents and air.
    fn remove_batch(&mut self, cells: &[[usize; 3]]) {
        if cells.is_empty() { return; }
        let mut touched_chunks: Vec<usize> = Vec::with_capacity(cells.len());
        for &[x, y, z] in cells {
            let (ci, li) = self.chunk_loc(x, y, z);
            let Some(c) = self.chunks.get_mut(ci).and_then(|c| c.as_deref_mut()) else { continue };
            if !bit(&c.solid, li) { continue; }
            c.ids[li] = 0;
            set_bit(&mut c.solid, li, false);
            set_bit(&mut c.watertight, li, false);
            c.solid_count -= 1;
            self.solid_count -= 1;
            self.dirty_chunks.set(ci, true);
            touched_chunks.push(ci);
            self.mark_render(x, y, z);
        }
        // Lines whose extent may have shrunk: only those where a removed cell was an end.
        let mut lines: [Vec<usize>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for &p in cells {
            for a in 0..3 {
                let [lo, hi] = self.line_ext(a, p);
                if p[a] == lo as usize || p[a] == hi as usize { lines[a].push(self.line_slot(a, p)); }
            }
        }
        let Some([sx, sy, sz]) = self.dims() else { return };
        for a in 0..3 {
            let mut v = core::mem::take(&mut lines[a]);
            v.sort_unstable();
            v.dedup();
            for slot in v {
                // Representative point of the line.
                let at = match a { 0 => [0, slot / sz, slot % sz], 1 => [slot % sx, 0, slot / sx], _ => [slot % sx, slot / sx, 0] };
                let ext = self.scan_line(a, at);
                let old = match a { 0 => &mut self.xline, 1 => &mut self.yline, _ => &mut self.zline };
                if old[slot] == ext { continue; }
                old[slot] = ext;
                let n = [sx, sy, sz][a];
                let mut p = at;
                for k in 0..n {
                    p[a] = k;
                    if !self.solid_at(p[0], p[1], p[2]) {
                        let air = self.enclosed(p[0], p[1], p[2]);
                        self.set_air(p[0], p[1], p[2], air);
                        let ci = self.chunk_loc(p[0], p[1], p[2]).0;
                        touched_chunks.push(ci);
                    }
                }
            }
        }
        for &[x, y, z] in cells {
            let air = self.enclosed(x, y, z);
            self.set_air(x, y, z, air);
        }
        touched_chunks.sort_unstable();
        touched_chunks.dedup();
        for ci in touched_chunks {
            if self.chunks[ci].as_deref().is_some_and(|c| c.solid_count == 0 && !c.has_air()) { self.chunks[ci] = None; }
        }
    }
    /// Zero-copy view for hull-mesh.
    pub fn view(&self) -> CellGridView<'_> { CellGridView { size: self.design.size, cells: &self.design.cells, damage: &self.damage, sealed_air: &self.sealed_air, scorch: &self.scorch } }
}
