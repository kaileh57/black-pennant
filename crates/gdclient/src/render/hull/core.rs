//! Part of unit `gd-render-hull`. Everything the hull renderer decides, with no Godot in it: the client-side
//! cells of every drawn vessel, what damage does to them, which meshes are wanted and in what order, when a
//! finished build may be shown, and when an old RID may be freed. The engine is reached only through
//! [`Device`], so the whole state machine runs headless against a counting fake.
//!
//! # Epochs
//! Each vessel counts geometry changes in `epoch` (0 = the intact design). A chunk remembers the epoch its
//! cells last changed at (`dirty_epoch`) and the epoch its uploaded mesh was built from (`built_epoch`); a
//! whole-vessel LOD mesh compares against the vessel's `epoch`. Two rules follow:
//! - a LOD the vessel is already drawn at keeps showing its old mesh until the rebuild arrives, then swaps
//!   (L1929);
//! - a LOD the vessel is not drawn at is switched to only once every mesh of it is current, so a ship holed
//!   at LOD2 never shows its unholed LOD0 chunks when the camera closes in. Until then the previous LOD
//!   stays (L1943: no fades).
//!
//! # Timing
//! Reveals, hides and despawns are applied the frame they arrive. Events that change cells at a moment wait
//! in [`super::hold::EventHold`] until playback is [`super::hold::MESH_LEAD_TICKS`] short of their tick, so
//! the rebuilt mesh reaches the screen with the impact it belongs to.
//!
//! # Requests
//! There is no queue to go stale. Each frame every posed vessel lists what it is missing for its picked LOD,
//! the list is sorted by screen area (x4 own hull and dock, x2 the own hull's designated target, L1945) and
//! sent while fewer than [`IN_FLIGHT_PRIORITY`] pieces are being built ([`IN_FLIGHT_OTHER`] for other hulls,
//! which keeps room for the player's). Each request carries a renderer-wide generation; a result whose
//! generation is not the one its slot is waiting for is dropped.
//!
//! # Sharing
//! Meshes built from an intact design (`dirty_epoch == 0`) go into a cache keyed by `hull_mesh::ChunkKey`
//! and are reference-counted; the second ship of a class attaches the first one's chunk meshes and asks only
//! for its own volumes (`MeshWork::Volumes`). An unreferenced entry is kept [`CACHE_IDLE_FRAMES`] in case
//! the class comes back into view. The dock design is never shared: it changes with every edit.
//!
//! # Memory
//! LOD0 chunks of a vessel that has not been drawn at LOD0 for [`CHUNK_EVICT_S`] are released (a chunk holds
//! a 157 KB volume). Retired RIDs wait [`RETIRE_FRAMES`] frames and are freed at most [`FREES_PER_FRAME`]
//! per frame; past [`RETIRE_BACKLOG`] waiting RIDs the per-frame count rises with the backlog so a long
//! battle cannot grow it without bound.
// Client rendering, not simulation (clippy.toml): a hashed map and std floats are allowed here. The cache is
// never iterated for anything that reaches the screen in order, and nothing here feeds back into the sim.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
use super::design::{waterline_row, DrawDesign};
use super::{HullCells, MeshDone, MeshJob, MeshOutput, MeshWork};
use crate::interp::ViewFrame;
use crate::seams::{Aabb, ChunkKey, ChunkMesh, Lod, QualityTier, SurfaceBytes, VolumeTexture, WireEvent};
use content::blueprint::MarkingKind;
use content::cell::{CellChange, RenderChunkCoord, BUCKET_DESTROYED};
use content::ids::{CellIndex, ContentHash};
use glam::{Affine3A, Vec3};
use godot::prelude::Rid;
use hull_mesh::mask::HullMask;
use hull_mesh::pipeline::MarkingQuad;
use sim_core::damage::DamageCause;
use sim_core::events::PhysEvent;
use sim_core::EntityId;
use sim_systems::protocol::SimEvent;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;

/// The id the dry dock's design is drawn under. No simulation entity has it (indices stop far below).
pub(crate) const DOCK: EntityId = EntityId { index: u32::MAX - 1, generation: 0 };
/// Pieces being built at once for the own hull, its target and the dock (L1945: up to 8 remeshes per frame).
pub(crate) const IN_FLIGHT_PRIORITY: u32 = 8;
pub(crate) const IN_FLIGHT_OTHER: u32 = 4;
pub(crate) const RETIRE_FRAMES: u64 = 2;
pub(crate) const FREES_PER_FRAME: usize = 4;
pub(crate) const RETIRE_BACKLOG: usize = 256;
/// Scorch-only volume updates per vessel: at most 4 Hz (L1931).
pub(crate) const VOLUME_PERIOD_S: f64 = 0.25;
pub(crate) const CHUNK_EVICT_S: f64 = 20.0;
pub(crate) const CACHE_IDLE_FRAMES: u64 = 600;
/// Scorch a burning compartment's walls gain per volume period: black in about five seconds.
const FIRE_SCORCH_STEP: u8 = 13;
/// Events kept for a vessel whose design has not arrived.
const PENDING_OPS_MAX: usize = 4_096;
/// Markings are a cell tall: past LOD1 they are under a pixel.
const MARKINGS_MAX_LOD: Lod = Lod::L1;

/// The per-vessel inputs of a chunk material (`hull.gdshader`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChunkLook { pub palette: [[f32; 3]; 16], pub waterline_m: f32, pub deck_m: Option<f32>, pub weathering: f32, pub view_mode: i32 }

/// The engine calls the hull renderer makes. `gpu::GodotDevice` is the real one.
pub(crate) trait Device {
    /// `mesh_create_from_surfaces`.
    fn mesh(&mut self, surface: &SurfaceBytes) -> Rid;
    /// Create the welded shadow mesh and bind it to `mesh` (`mesh_set_shadow_mesh`).
    fn shadow_mesh(&mut self, mesh: Rid, shadow: &SurfaceBytes) -> Rid;
    /// The chunk's volume: `texture_3d_update` when `existing` is valid, else `texture_3d_create`.
    fn volume(&mut self, existing: Rid, volume: &VolumeTexture) -> Rid;
    /// A per-chunk material on the shared hull shader, volume path.
    fn chunk_material(&mut self, volume: Rid, origin_m: [f32; 3], look: &ChunkLook) -> Rid;
    fn set_heat(&mut self, material: Rid, heat: f32);
    /// Re-set the per-vessel inputs of a chunk material in place (palette, waterline, deck, view mode).
    fn set_look(&mut self, material: Rid, look: &ChunkLook);
    /// The one material of the vertex-colour path (LOD1 to LOD4 and markings).
    fn lod_material(&mut self) -> Rid;
    fn instance(&mut self, mesh: Rid, material: Rid, xf: &Affine3A, visible: bool, shadows: bool) -> Rid;
    fn set_base(&mut self, instance: Rid, mesh: Rid);
    fn set_transform(&mut self, instance: Rid, xf: &Affine3A);
    fn set_visible(&mut self, instance: Rid, visible: bool);
    fn set_shadows(&mut self, instance: Rid, on: bool);
    fn free(&mut self, rid: Rid);
    /// The 8-layer mask array, created on first use.
    fn mask_texture(&mut self) -> Rid;
    fn mask_layer(&mut self, layer: u32, mask: &HullMask);
    fn warn(&mut self, message: &str);
}

/// Something that changes a vessel's cells.
#[derive(Clone, Debug)]
pub(crate) enum CellOp {
    /// The replicated damage stream, with what caused it (scorch).
    Damage(Vec<CellChange>, Option<DamageCause>),
    /// Cells gone: split off or shed.
    Remove(Vec<u32>),
    /// A wreck piece: every cell but these is gone.
    Keep(Vec<u32>),
    /// A compartment opened to the sea: its air is no longer sealed.
    Breach(u16),
}

/// Scorch a cell takes at bucket 8 from this cause, 0..=255. Blast and fire blacken; a collision does not.
pub(crate) fn scorch_strength(cause: DamageCause) -> u8 {
    match cause {
        DamageCause::Fire | DamageCause::CookOff => 255,
        DamageCause::Missile | DamageCause::Torpedo | DamageCause::Mine | DamageCause::DepthCharge => 200,
        DamageCause::Shell => 140,
        DamageCause::Collision | DamageCause::Grounding => 0,
    }
}

/// Mesh RIDs a slot draws: its own, or a reference into the cache.
#[derive(Clone, Copy, Debug)]
struct MeshRef { mesh: Rid, shadow: Rid, shared: Option<ChunkKey> }
impl Default for MeshRef { fn default() -> Self { Self::NONE } }
impl MeshRef {
    const NONE: MeshRef = MeshRef { mesh: Rid::Invalid, shadow: Rid::Invalid, shared: None };
}

/// An outstanding build of one slot.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Asked { generation: u32, epoch: u32, volume_rev: u32, volume_only: bool }

#[derive(Clone, Debug)]
pub(crate) struct ChunkSlot {
    pub coord: RenderChunkCoord,
    /// Vessel epoch at which this chunk's cells (padding included) last changed.
    pub dirty_epoch: u32,
    /// Epoch of the uploaded mesh; `None` before the first build and after eviction.
    pub built_epoch: Option<u32>,
    /// Counts scorch changes; `volume_built` is the count the uploaded volume holds.
    volume_rev: u32,
    volume_built: u32,
    asked: Option<Asked>,
    /// The chunk has no face at `built_epoch`.
    pub empty: bool,
    mesh: MeshRef,
    pub instance: Rid,
    pub volume: Rid,
    pub material: Rid,
    heat: f32,
}
impl ChunkSlot {
    fn new(coord: RenderChunkCoord) -> Self {
        Self { coord, dirty_epoch: 0, built_epoch: None, volume_rev: 0, volume_built: 0, asked: None, empty: false, mesh: MeshRef::NONE, instance: Rid::Invalid, volume: Rid::Invalid, material: Rid::Invalid, heat: 0.0 }
    }
    /// The mesh on the GPU is the one the current cells give, with a volume to shade it.
    pub fn current(&self) -> bool { self.built_epoch == Some(self.dirty_epoch) && (self.empty || self.volume.is_valid()) }
}

#[derive(Clone, Debug)]
pub(crate) struct LodSlot { pub built_epoch: Option<u32>, asked: Option<Asked>, pub empty: bool, mesh: MeshRef, pub instance: Rid }
impl Default for LodSlot { fn default() -> Self { Self { built_epoch: None, asked: None, empty: false, mesh: MeshRef::NONE, instance: Rid::Invalid } } }

#[derive(Default)]
struct MaskState {
    /// The mask reads the grid with part cells solid, so it has its own copy, refreshed per rebuild.
    cells: Option<Arc<HullCells>>,
    built_rev: Option<u32>,
    asked: Option<(u32, u32)>,
    asked_at_s: f64,
    data: Option<Box<HullMask>>,
    layer: Option<u8>,
    uploaded: bool,
}

/// A compartment on fire: the wall cells around it and how black they are so far.
struct Burn { cells: Vec<CellIndex>, chunks: Vec<RenderChunkCoord>, level: u8 }

pub(crate) struct Vessel {
    pub entity: EntityId,
    pub design: Arc<DrawDesign>,
    pub cells: Arc<HullCells>,
    /// Geometry epoch (module docs) and a count of every cell-state change (the mask reads more than the
    /// mesh does).
    pub epoch: u32,
    cells_rev: u32,
    pub chunks: Vec<ChunkSlot>,
    pub lods: [LodSlot; 4],
    /// The LOD picked this frame and the one whose instances are on screen.
    pub target: Lod,
    pub drawn: Option<Lod>,
    pub pose: Affine3A,
    /// In this frame's view. An unposed vessel keeps its meshes and is hidden.
    pub posed: bool,
    shown: bool,
    xf_set: Option<Affine3A>,
    pub own: bool,
    targeted: bool,
    shadows: bool,
    shadows_set: bool,
    dist_m: f32,
    /// All placed markings with their anchor cells, and those still standing.
    all_markings: Vec<(u32, MarkingQuad)>,
    pub markings: Vec<MarkingQuad>,
    markings_dirty: bool,
    marking_mesh: Rid,
    marking_instance: Rid,
    mask: MaskState,
    waterline_known: bool,
    look: ChunkLook,
    burning: BTreeMap<u16, Burn>,
    scorch_due_s: f64,
    volume_due_s: f64,
    last_lod0_s: f64,
    failed: bool,
    pub dock: bool,
    /// Hull box in vessel-local metres.
    pub centre: Vec3,
    pub radius: f32,
}

fn lod_index(lod: Lod) -> Option<usize> { match lod { Lod::L0 => None, Lod::L1 => Some(0), Lod::L2 => Some(1), Lod::L3 => Some(2), Lod::L4 => Some(3) } }
const LODS: [Lod; 4] = [Lod::L1, Lod::L2, Lod::L3, Lod::L4];

fn wear_seed(entity: EntityId) -> u64 { (entity.to_u64() ^ 0x5bd1_e995_9e37_79b9).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1 }

impl Vessel {
    pub(crate) fn new(entity: EntityId, design: Arc<DrawDesign>, dock: bool, view_mode: i32) -> Self {
        let size = design.grid.size;
        let waterline_m = design.waterline_m.unwrap_or(size[1] as f32 * hull_mesh::CELL_M_F32 / 3.0);
        let cells = HullCells {
            size,
            cells: design.mesh_cells.clone(),
            damage: Vec::new(),
            sealed_air: design.sealed_air.clone(),
            scorch: Vec::new(),
            palette: design.palette.clone(),
            wear_seed: if dock { 0 } else { wear_seed(entity) },
            design_waterline_y: waterline_row(waterline_m, size[1]),
        };
        let mut palette = [[0.0f32; 3]; 16];
        for (slot, rgb) in palette.iter_mut().zip(&design.palette) { *slot = *rgb; }
        let look = ChunkLook { palette, waterline_m, deck_m: design.deck_m, weathering: if dock { 0.0 } else { 1.0 }, view_mode };
        let chunks = if design.meshable() { hull_mesh::chunk::all_chunks(size).into_iter().map(ChunkSlot::new).collect() } else { Vec::new() };
        // Place each marking by itself, so the quad can be dropped when its own cell goes.
        let all_markings: Vec<(u32, MarkingQuad)> = design.markings.iter().filter_map(|m| {
            let cell = design.grid.index(m.cell)?.0;
            hull_mesh::pipeline::marking_quads(std::slice::from_ref(m), &design.grid).into_iter().next().map(|q| (cell, q))
        }).collect();
        let half = 0.5 * Vec3::from(design.size_m());
        Self {
            entity, cells: Arc::new(cells), epoch: 0, cells_rev: 0, chunks, lods: Default::default(), target: if dock { Lod::L0 } else { Lod::L4 }, drawn: None,
            pose: Affine3A::IDENTITY, posed: dock, shown: false, xf_set: None, own: dock, targeted: false, shadows: true, shadows_set: true, dist_m: 0.0,
            markings: all_markings.iter().map(|(_, q)| q.clone()).collect(), markings_dirty: !all_markings.is_empty(), all_markings, marking_mesh: Rid::Invalid, marking_instance: Rid::Invalid,
            mask: MaskState::default(), waterline_known: design.waterline_m.is_some() || dock, look, burning: BTreeMap::new(), scorch_due_s: 0.0, volume_due_s: 0.0, last_lod0_s: f64::NEG_INFINITY,
            failed: !design.meshable(), dock, centre: half, radius: half.length(), design,
        }
    }

    fn chunk_index(&self, coord: RenderChunkCoord) -> Option<usize> { self.chunks.binary_search_by_key(&coord, |c| c.coord).ok() }

    /// A cell is solid in the design grid and not removed (picking, markings: part cells count).
    fn standing(&self, cell: usize) -> bool {
        self.design.grid.cells.get(cell).is_some_and(|c| !c.is_empty()) && self.cells.damage.get(cell).is_none_or(|b| *b < BUCKET_DESTROYED)
    }

    /// Apply one change to the cells and mark what it dirties.
    pub(crate) fn apply(&mut self, op: &CellOp) {
        let n = self.cells.cells.len();
        if n == 0 { return; }
        let size = self.cells.size;
        // Cells whose removal changes the mesh, cells that changed at all, cells whose scorch rose.
        let (mut holed, mut any, mut scorched): (Vec<CellIndex>, bool, Vec<CellIndex>) = (Vec::new(), false, Vec::new());
        let mut everything = false;
        // Every cell removed by this change, meshed or not (a part cell's removal opens air too).
        let mut gone: Vec<u32> = Vec::new();
        {
            let cells = Arc::make_mut(&mut self.cells);
            let mut remove = |cells: &mut HullCells, i: usize, holed: &mut Vec<CellIndex>| -> bool {
                if cells.damage.is_empty() { cells.damage = vec![0; n]; }
                if cells.damage[i] >= BUCKET_DESTROYED { return false; }
                cells.damage[i] = BUCKET_DESTROYED;
                gone.push(i as u32);
                if !cells.cells[i].is_empty() { holed.push(CellIndex(i as u32)); }
                true
            };
            match op {
                CellOp::Damage(changes, cause) => {
                    let strength = cause.map_or(0, scorch_strength) as u32;
                    for change in changes {
                        let i = change.cell.index();
                        if i >= n { continue; }
                        let bucket = change.bucket.min(BUCKET_DESTROYED);
                        if bucket == BUCKET_DESTROYED {
                            any |= remove(cells, i, &mut holed);
                        } else if !cells.damage.is_empty() || bucket > 0 {
                            if cells.damage.is_empty() { cells.damage = vec![0; n]; }
                            // A removed cell stays removed: only a tender rebuild restores it, as a new reveal.
                            if cells.damage[i] < BUCKET_DESTROYED && cells.damage[i] != bucket { cells.damage[i] = bucket; any = true; }
                        }
                        if strength > 0 && bucket > 0 {
                            let level = (strength * bucket as u32 / BUCKET_DESTROYED as u32) as u8;
                            scorch_around(cells, i, level, &mut scorched);
                        }
                    }
                }
                CellOp::Remove(list) => for &i in list { if (i as usize) < n { any |= remove(cells, i as usize, &mut holed); } },
                CellOp::Keep(list) => {
                    let mut kept = vec![0u64; n.div_ceil(64)];
                    for &i in list { if (i as usize) < n { kept[i as usize / 64] |= 1 << (i % 64); } }
                    let mut scratch = Vec::new();
                    for i in 0..n {
                        if kept[i / 64] >> (i % 64) & 1 == 0 && remove(cells, i, &mut scratch) { any = true; }
                    }
                    everything = !scratch.is_empty();
                }
                CellOp::Breach(id) => {
                    if let Ok(k) = self.design.compartments.binary_search_by_key(id, |c| c.0) {
                        for &i in &self.design.compartments[k].1 {
                            let (word, bit) = (i as usize / 64, 1u64 << (i % 64));
                            if cells.sealed_air.get(word).is_some_and(|w| w & bit != 0) {
                                cells.sealed_air[word] &= !bit;
                                holed.push(CellIndex(i));
                                any = true;
                            }
                        }
                    }
                }
            }
        }
        // A hole lets the air behind it out. The simulation says so with a `Breach` event while the vessel is
        // watched, but a reveal that arrives already holed and a piece derived from a split carry none, and a
        // hole beside air still classed as sealed would mesh no faces behind it: one could see through the
        // hull. So sealed air that touches a removed cell is opened here, through the whole connected pocket.
        if !gone.is_empty() || everything {
            let cells = Arc::make_mut(&mut self.cells);
            if everything || gone.len() * 2 > n {
                // A wreck piece is broken open everywhere.
                if cells.sealed_air.iter().any(|w| *w != 0) { cells.sealed_air.iter_mut().for_each(|w| *w = 0); everything = true; }
            } else {
                unseal_beside(cells, &gone, &mut holed);
            }
        }
        if any { self.cells_rev = self.cells_rev.wrapping_add(1); }
        if everything || !holed.is_empty() {
            self.epoch = self.epoch.wrapping_add(1).max(1);
            if everything {
                for c in &mut self.chunks { c.dirty_epoch = self.epoch; }
            } else {
                for coord in hull_mesh::chunk::dirty_chunks_for_cells(size, &holed) {
                    if let Some(i) = self.chunk_index(coord) { self.chunks[i].dirty_epoch = self.epoch; }
                }
            }
        }
        if any && !self.all_markings.is_empty() {
            let standing: Vec<MarkingQuad> = self.all_markings.iter().filter(|(cell, _)| self.standing(*cell as usize)).map(|(_, q)| q.clone()).collect();
            if standing.len() != self.markings.len() { self.markings = standing; self.markings_dirty = true; }
        }
        self.mark_volumes(&scorched);
    }

    fn mark_volumes(&mut self, cells: &[CellIndex]) {
        if cells.is_empty() { return; }
        for coord in hull_mesh::chunk::dirty_chunks_for_cells(self.cells.size, cells) {
            if let Some(i) = self.chunk_index(coord) { self.chunks[i].volume_rev = self.chunks[i].volume_rev.wrapping_add(1); }
        }
    }

    /// The wall cells of a compartment: solid cells that touch its air.
    fn compartment_walls(&self, id: u16) -> Vec<CellIndex> {
        let Ok(k) = self.design.compartments.binary_search_by_key(&id, |c| c.0) else { return Vec::new() };
        let [sx, sy, sz] = self.cells.size.map(|s| s as usize);
        let cells = &self.cells.cells;
        let mut walls = Vec::new();
        for &i in &self.design.compartments[k].1 {
            let i = i as usize;
            if i >= cells.len() { continue; }
            let (x, y, z) = (i % sx, i / (sx * sz), (i / sx) % sz);
            let mut look = |ok: bool, j: usize| if ok && !cells[j].is_empty() { walls.push(CellIndex(j as u32)); };
            look(x > 0, i.wrapping_sub(1));
            look(x + 1 < sx, i + 1);
            look(z > 0, i.wrapping_sub(sx));
            look(z + 1 < sz, i + sx);
            look(y > 0, i.wrapping_sub(sx * sz));
            look(y + 1 < sy, i + sx * sz);
        }
        walls.sort_unstable();
        walls.dedup();
        walls
    }

    /// Blacken the walls of burning compartments one step. Returns whether anything changed.
    fn burn_step(&mut self) {
        if self.burning.values().all(|b| b.level == u8::MAX) { return; }
        let mut touched: Vec<CellIndex> = Vec::new();
        {
            let cells = Arc::make_mut(&mut self.cells);
            let n = cells.cells.len();
            for burn in self.burning.values_mut() {
                if burn.level == u8::MAX { continue; }
                burn.level = burn.level.saturating_add(FIRE_SCORCH_STEP);
                if cells.scorch.is_empty() { cells.scorch = vec![0; n]; }
                for c in &burn.cells {
                    if cells.scorch[c.index()] < burn.level { cells.scorch[c.index()] = burn.level; touched.push(*c); }
                }
            }
        }
        self.mark_volumes(&touched);
    }

    /// Every chunk of LOD0, or the LOD's one mesh, is current.
    fn lod_current(&self, lod: Lod) -> bool {
        match lod_index(lod) {
            None => !self.chunks.is_empty() && self.chunks.iter().all(ChunkSlot::current),
            Some(i) => self.lods[i].built_epoch == Some(self.epoch),
        }
    }

    fn set_lod_visible(&self, dev: &mut dyn Device, lod: Lod, visible: bool) {
        match lod_index(lod) {
            None => for c in &self.chunks { if c.instance.is_valid() { dev.set_visible(c.instance, visible && !c.empty); } },
            Some(i) => if self.lods[i].instance.is_valid() { dev.set_visible(self.lods[i].instance, visible && !self.lods[i].empty); },
        }
    }
}

/// Open every pocket of sealed air that touches one of the `gone` cells, appending the cells opened to
/// `opened` (their chunks must remesh).
fn unseal_beside(cells: &mut HullCells, gone: &[u32], opened: &mut Vec<CellIndex>) {
    if cells.sealed_air.iter().all(|w| *w == 0) { return; }
    let n = cells.cells.len();
    let [sx, sy, sz] = cells.size.map(|s| s as usize);
    let neighbours = |i: usize| {
        let (x, y, z) = (i % sx, i / (sx * sz), (i / sx) % sz);
        [(x > 0, i.wrapping_sub(1)), (x + 1 < sx, i + 1), (z > 0, i.wrapping_sub(sx)), (z + 1 < sz, i + sx), (y > 0, i.wrapping_sub(sx * sz)), (y + 1 < sy, i + sx * sz)]
    };
    let mut open: Vec<usize> = gone.iter().map(|g| *g as usize).filter(|g| *g < n).collect();
    while let Some(i) = open.pop() {
        for (ok, j) in neighbours(i) {
            if !ok || j >= n { continue; }
            let (word, bit) = (j / 64, 1u64 << (j % 64));
            if cells.sealed_air.get(word).is_some_and(|w| w & bit != 0) {
                cells.sealed_air[word] &= !bit;
                opened.push(CellIndex(j as u32));
                open.push(j);
            }
        }
    }
}

/// Raise the scorch of cell `i` to `level` and of its six neighbours to half of it.
fn scorch_around(cells: &mut HullCells, i: usize, level: u8, touched: &mut Vec<CellIndex>) {
    let n = cells.cells.len();
    if level == 0 || i >= n { return; }
    if cells.scorch.is_empty() { cells.scorch = vec![0; n]; }
    let [sx, sy, sz] = cells.size.map(|s| s as usize);
    let (x, y, z) = (i % sx, i / (sx * sz), (i / sx) % sz);
    let mut raise = |j: usize, to: u8| if cells.scorch[j] < to { cells.scorch[j] = to; touched.push(CellIndex(j as u32)); };
    raise(i, level);
    let half = level / 2;
    if x > 0 { raise(i - 1, half); }
    if x + 1 < sx { raise(i + 1, half); }
    if z > 0 { raise(i - sx, half); }
    if z + 1 < sz { raise(i + sx, half); }
    if y > 0 { raise(i - sx * sz, half); }
    if y + 1 < sy { raise(i + sx * sz, half); }
}

/// A reveal waiting for its design.
struct Pending { hash: ContentHash, ops: Vec<CellOp> }

enum CacheEntry {
    /// Being built for `owner` by the request with this generation.
    Building { owner: EntityId, generation: u32 },
    Ready { mesh: Rid, shadow: Rid, empty: bool, refs: u32, idle_frame: u64 },
}

enum Cached { Ready { mesh: Rid, shadow: Rid, empty: bool }, Building, Absent }

/// What the frame needs of the camera, the clock and the quality profile.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HullLens {
    pub camera: Vec3,
    /// Vertical field of view in radians and the rendered viewport height; NaN when there is no camera, which
    /// keeps every vessel at its current LOD.
    pub fov_y_rad: f64,
    pub viewport_h_px: f64,
    pub tier: QualityTier,
    pub degrade_step: u8,
    pub delta_s: f64,
}

/// One wanted build, before it is sent.
struct Want { priority: f32, boosted: bool, vessel: EntityId, what: What }
#[derive(Clone, Copy, Debug, PartialEq)]
enum What { Chunk(usize), Volume(usize), Lod(Lod), Mask }

/// The hull renderer's state (module docs).
pub(crate) struct HullCore {
    pub(crate) vessels: BTreeMap<EntityId, Vessel>,
    pending: BTreeMap<EntityId, Pending>,
    cache: HashMap<ChunkKey, CacheEntry>,
    retired: VecDeque<(u64, Rid)>,
    /// Finished builds not yet uploaded, in arrival order.
    ready: VecDeque<MeshDone>,
    /// Jobs to send to the meshing thread this frame.
    pub(crate) outbox: Vec<MeshJob>,
    in_flight: u32,
    generation: u32,
    frame: u64,
    clock_s: f64,
    sim_time_s: f64,
    mask_layers: [Option<EntityId>; hull_mesh::mask::MAX_MASKED_HULLS],
    wants: Vec<Want>,
    /// Damage events waiting for playback to reach them.
    hold: super::hold::EventHold,
    pub(crate) chunks_drawn: u32,
}

/// Screen-area weight of a hull `dist_m` from the camera.
fn screen_area(radius_m: f32, dist_m: f32) -> f32 { let r = radius_m / dist_m.max(1.0); r * r }

impl HullCore {
    pub(crate) fn new() -> Self {
        Self { vessels: BTreeMap::new(), pending: BTreeMap::new(), cache: HashMap::new(), retired: VecDeque::new(), ready: VecDeque::new(), outbox: Vec::new(), in_flight: 0, generation: 0, frame: 0, clock_s: 0.0, sim_time_s: 0.0, mask_layers: [None; hull_mesh::mask::MAX_MASKED_HULLS], wants: Vec::new(), hold: Default::default(), chunks_drawn: 0 }
    }

    pub(crate) fn pending_uploads(&self) -> u32 { self.ready.len() as u32 + self.in_flight }
    pub(crate) fn retired_len(&self) -> usize { self.retired.len() }

    /// A finished piece from the meshing thread.
    pub(crate) fn receive(&mut self, done: MeshDone) {
        self.in_flight = self.in_flight.saturating_sub(1);
        self.ready.push_back(done);
    }

    fn retire(retired: &mut VecDeque<(u64, Rid)>, frame: u64, rid: &mut Rid) {
        if rid.is_valid() { retired.push_back((frame, *rid)); }
        *rid = Rid::Invalid;
    }

    /// Look a shared mesh up and, when it is ready, take a reference to it.
    fn cache_take(cache: &mut HashMap<ChunkKey, CacheEntry>, key: &ChunkKey) -> Cached {
        match cache.get_mut(key) {
            Some(CacheEntry::Ready { mesh, shadow, empty, refs, .. }) => { *refs += 1; Cached::Ready { mesh: *mesh, shadow: *shadow, empty: *empty } }
            Some(CacheEntry::Building { .. }) => Cached::Building,
            None => Cached::Absent,
        }
    }

    /// Let go of a slot's mesh: a cache reference is counted down, an own mesh is retired.
    fn release(cache: &mut HashMap<ChunkKey, CacheEntry>, retired: &mut VecDeque<(u64, Rid)>, frame: u64, mesh: &mut MeshRef) {
        match mesh.shared.take() {
            Some(key) => if let Some(CacheEntry::Ready { refs, idle_frame, .. }) = cache.get_mut(&key) {
                *refs = refs.saturating_sub(1);
                if *refs == 0 { *idle_frame = frame; }
            },
            None => { Self::retire(retired, frame, &mut mesh.mesh); Self::retire(retired, frame, &mut mesh.shadow); }
        }
        *mesh = MeshRef::NONE;
    }

    fn release_chunk(cache: &mut HashMap<ChunkKey, CacheEntry>, retired: &mut VecDeque<(u64, Rid)>, frame: u64, c: &mut ChunkSlot) {
        Self::release(cache, retired, frame, &mut c.mesh);
        for rid in [&mut c.instance, &mut c.volume, &mut c.material] { Self::retire(retired, frame, rid); }
        c.built_epoch = None;
        c.asked = None;
        c.empty = false;
        c.heat = 0.0;
    }

    /// Take a vessel off the screen and queue everything it holds for freeing.
    fn drop_vessel(&mut self, dev: &mut dyn Device, entity: EntityId) {
        self.pending.remove(&entity);
        let Some(mut v) = self.vessels.remove(&entity) else { return };
        if v.shown { if let Some(lod) = v.drawn { v.set_lod_visible(dev, lod, false); } }
        if v.marking_instance.is_valid() { dev.set_visible(v.marking_instance, false); }
        for c in &mut v.chunks { Self::release_chunk(&mut self.cache, &mut self.retired, self.frame, c); }
        for l in &mut v.lods {
            Self::release(&mut self.cache, &mut self.retired, self.frame, &mut l.mesh);
            Self::retire(&mut self.retired, self.frame, &mut l.instance);
        }
        Self::retire(&mut self.retired, self.frame, &mut v.marking_instance);
        Self::retire(&mut self.retired, self.frame, &mut v.marking_mesh);
        // Shared builds it had started may now never be attached: let another hull start them.
        self.cache.retain(|_, e| !matches!(e, CacheEntry::Building { owner, .. } if *owner == entity));
        for layer in &mut self.mask_layers { if *layer == Some(entity) { *layer = None; } }
    }

    /// Forget every sortie vessel (a new sortie). The dock design stays.
    pub(crate) fn clear_sortie(&mut self, dev: &mut dyn Device) {
        let ids: Vec<EntityId> = self.vessels.keys().copied().filter(|e| *e != DOCK).collect();
        for id in ids { self.drop_vessel(dev, id); }
        self.pending.clear();
    }

    /// Draw `design` as the dock vessel, replacing any previous one. `cut` removes every cell whose centre is
    /// past the plane (metres along the axis); `xray` dims the hull for the overlays.
    pub(crate) fn show_design(&mut self, dev: &mut dyn Device, design: Arc<DrawDesign>, xray: bool, cut: Option<(content::enums::Axis, f32)>) {
        let view_mode = if xray { 1 } else { 0 };
        let mut next = Vessel::new(DOCK, design, true, view_mode);
        if let Some((axis, at_m)) = cut.filter(|c| c.1.is_finite()) {
            let grid = &next.design.grid;
            let [sx, _, sz] = grid.size.map(|s| s as usize);
            let hidden: Vec<u32> = (0..grid.cells.len()).filter(|&i| {
                let cell = match axis { content::enums::Axis::X => i % sx, content::enums::Axis::Y => i / (sx * sz), content::enums::Axis::Z => (i / sx) % sz };
                (cell as f32 + 0.5) * hull_mesh::CELL_M_F32 > at_m
            }).map(|i| i as u32).collect();
            next.apply(&CellOp::Remove(hidden));
        }
        // Keep what the previous dock vessel has on screen for every chunk the edit did not touch.
        match self.vessels.get_mut(&DOCK) {
            Some(old) if old.cells.size == next.cells.size && !old.failed && !next.failed => {
                let changed: Vec<CellIndex> = {
                    let (a, b) = (&old.cells, &next.cells);
                    let removed = |cells: &HullCells, i: usize| cells.damage.get(i).is_some_and(|d| *d >= BUCKET_DESTROYED);
                    (0..a.cells.len()).filter(|&i| a.cells[i] != b.cells[i] || removed(a, i) != removed(b, i)).map(|i| CellIndex(i as u32)).collect()
                };
                if !changed.is_empty() {
                    old.epoch = old.epoch.wrapping_add(1).max(1);
                    old.cells_rev = old.cells_rev.wrapping_add(1);
                    for coord in hull_mesh::chunk::dirty_chunks_for_cells(next.cells.size, &changed) {
                        if let Some(i) = old.chunk_index(coord) { old.chunks[i].dirty_epoch = old.epoch; }
                    }
                }
                // The palette, waterline and view mode live in each chunk's material: an edit that only moves
                // those (a repaint of a swatch, a heavier fit that sinks the waterline) re-sets them in place.
                if old.look != next.look {
                    for c in &old.chunks { if c.material.is_valid() { dev.set_look(c.material, &next.look); } }
                }
                // The volume's wet and salt rules key off the waterline row: a new row means new volumes.
                if old.cells.design_waterline_y != next.cells.design_waterline_y {
                    for c in &mut old.chunks { c.volume_rev = c.volume_rev.wrapping_add(1); }
                }
                old.look = next.look;
                old.cells = next.cells;
                old.design = next.design;
                old.all_markings = next.all_markings;
                old.markings = next.markings;
                old.markings_dirty = true;
            }
            _ => {
                self.drop_vessel(dev, DOCK);
                self.vessels.insert(DOCK, next);
            }
        }
    }

    fn on_event(&mut self, dev: &mut dyn Device, ev: &WireEvent) {
        let raw = |list: &[CellIndex]| list.iter().map(|c| c.0).collect::<Vec<u32>>();
        let (vessel, op) = match &ev.event {
            SimEvent::SortieStarted { .. } => { self.clear_sortie(dev); return; }
            SimEvent::EntityRevealed(intro) => {
                self.drop_vessel(dev, intro.entity);
                let mut ops = Vec::new();
                if !intro.removed.is_empty() { ops.push(CellOp::Remove(raw(&intro.removed))); }
                if !intro.damage.is_empty() { ops.push(CellOp::Damage(intro.damage.clone(), None)); }
                self.pending.insert(intro.entity, Pending { hash: intro.blueprint, ops });
                return;
            }
            SimEvent::EntityHidden { entity } => { self.drop_vessel(dev, *entity); return; }
            SimEvent::Phys(phys) => match phys {
                PhysEvent::VesselDespawned { vessel } => { self.drop_vessel(dev, *vessel); return; }
                PhysEvent::WreckDespawned { wreck } => { self.drop_vessel(dev, *wreck); return; }
                PhysEvent::CellsDamaged { vessel, changes, cause, .. } => (*vessel, CellOp::Damage(changes.clone(), Some(*cause))),
                PhysEvent::Debris { vessel, cells, .. } => (*vessel, CellOp::Remove(raw(cells))),
                PhysEvent::Breach { vessel, compartment, .. } => (*vessel, CellOp::Breach(compartment.0)),
                PhysEvent::Split { vessel, piece, cells } => {
                    let taken = raw(cells);
                    // Until the piece's own reveal arrives it is the parent as it stands, keeping these cells.
                    if !self.vessels.contains_key(piece) && !self.pending.contains_key(piece) {
                        if let Some(parent) = self.vessels.get(vessel) {
                            let mut child = Vessel::new(*piece, parent.design.clone(), false, 0);
                            {
                                let cells = Arc::make_mut(&mut child.cells);
                                cells.damage = parent.cells.damage.clone();
                                cells.sealed_air = parent.cells.sealed_air.clone();
                                cells.scorch = parent.cells.scorch.clone();
                                cells.design_waterline_y = parent.cells.design_waterline_y;
                            }
                            child.look.waterline_m = parent.look.waterline_m;
                            child.waterline_known = true;
                            child.apply(&CellOp::Keep(taken.clone()));
                            self.vessels.insert(*piece, child);
                        } else if let Some(parent) = self.pending.get(vessel) {
                            let mut ops = parent.ops.clone();
                            ops.push(CellOp::Keep(taken.clone()));
                            self.pending.insert(*piece, Pending { hash: parent.hash, ops });
                        }
                    }
                    (*vessel, CellOp::Remove(taken))
                }
                PhysEvent::FireStarted { vessel, compartment, .. } => {
                    if let Some(v) = self.vessels.get_mut(vessel) {
                        let walls = v.compartment_walls(compartment.0);
                        if !walls.is_empty() {
                            let chunks = hull_mesh::chunk::dirty_chunks_for_cells(v.cells.size, &walls);
                            v.burning.insert(compartment.0, Burn { cells: walls, chunks, level: 0 });
                        }
                    }
                    return;
                }
                PhysEvent::FireOut { vessel, compartment, .. } => {
                    if let Some(v) = self.vessels.get_mut(vessel) { v.burning.remove(&compartment.0); }
                    return;
                }
                _ => return,
            },
            _ => return,
        };
        if let Some(v) = self.vessels.get_mut(&vessel) {
            v.apply(&op);
        } else if let Some(p) = self.pending.get_mut(&vessel) {
            if p.ops.len() < PENDING_OPS_MAX { p.ops.push(op); }
        }
    }

    /// One frame. `spent` is the upload budget's timer (L2008): it is asked before each upload and each
    /// batch of frees, and work it refuses waits for the next frame.
    pub(crate) fn frame(&mut self, dev: &mut dyn Device, view: &ViewFrame, events: &[WireEvent], lens: HullLens, spent: &mut dyn FnMut() -> bool, lookup: &dyn Fn(&ContentHash) -> Option<Arc<DrawDesign>>) {
        self.frame += 1;
        self.clock_s += if lens.delta_s.is_finite() { lens.delta_s.clamp(0.0, 1.0) } else { 0.0 };
        self.sim_time_s = view.sim_time_s;
        // Reveals and hides now; damage when playback is a remesh short of it (`hold`).
        let mut hold = std::mem::take(&mut self.hold);
        hold.step(events, view.render_tick, super::hold::MESH_LEAD_TICKS, &mut |ev| self.on_event(dev, ev));
        self.hold = hold;

        // Designs that have arrived since their reveal.
        if !self.pending.is_empty() {
            let arrived: Vec<(EntityId, Arc<DrawDesign>)> = self.pending.iter().filter_map(|(e, p)| lookup(&p.hash).map(|d| (*e, d))).collect();
            for (entity, design) in arrived {
                let Some(p) = self.pending.remove(&entity) else { continue };
                let mut v = Vessel::new(entity, design, false, 0);
                for op in &p.ops { v.apply(op); }
                self.vessels.insert(entity, v);
            }
        }

        self.pose(dev, view, &lens);
        self.burn();
        self.upload(dev, spent);
        self.request();
        self.present(dev);
        self.sweep(dev, spent);
    }

    /// Poses, LOD picks and shadow flags for this frame.
    fn pose(&mut self, dev: &mut dyn Device, view: &ViewFrame, lens: &HullLens) {
        let designated = view.snapshot.as_ref().and_then(|s| s.ship.as_ref()).map(|s| s.designated).filter(|t| !t.is_none());
        for v in self.vessels.values_mut() { v.posed = v.dock; }
        for p in &view.vessels {
            let Some(v) = self.vessels.get_mut(&p.entity) else { continue };
            v.pose = super::gpu::pose_affine(p.pos, p.rot);
            v.posed = true;
            v.own = p.own;
            v.targeted = designated.is_some_and(|t| t == p.track);
            if !v.waterline_known {
                // An exterior form does not say where its waterline is: the hull's first pose does (the sea is
                // y = 0, and the hull's up axis leans by its heel).
                let up = v.pose.matrix3.y_axis.y;
                let waterline_m = if up.abs() > 0.2 { (-v.pose.translation.y / up).clamp(0.0, v.cells.size[1] as f32 * hull_mesh::CELL_M_F32) } else { v.look.waterline_m };
                v.look.waterline_m = waterline_m;
                Arc::make_mut(&mut v.cells).design_waterline_y = waterline_row(waterline_m, v.cells.size[1]);
                v.waterline_known = true;
            }
        }
        let sim_time_s = self.sim_time_s;
        for v in self.vessels.values_mut() {
            if !v.posed { continue; }
            let centre = v.pose.transform_point3(v.centre);
            v.dist_m = ((centre - lens.camera).length() - v.radius).max(0.0);
            v.target = if v.dock { Lod::L0 } else { hull_mesh::lod::pick_lod(v.dist_m as f64, lens.fov_y_rad, lens.viewport_h_px, lens.tier, v.target) };
            if v.target == Lod::L0 || v.drawn == Some(Lod::L0) { v.last_lod0_s = sim_time_s; }
            // L2030: from degrade step 4 only the own hull and hulls within 300 m cast shadows.
            v.shadows = lens.degrade_step < super::super::parts::SHADOW_DEGRADE_STEP || v.own || v.dist_m < super::super::parts::DEGRADED_SHADOW_M;
            if v.shadows != v.shadows_set {
                for c in &v.chunks { if c.instance.is_valid() { dev.set_shadows(c.instance, v.shadows); } }
                for l in &v.lods { if l.instance.is_valid() { dev.set_shadows(l.instance, v.shadows); } }
                v.shadows_set = v.shadows;
            }
        }
    }

    fn burn(&mut self) {
        let now = self.sim_time_s;
        for v in self.vessels.values_mut() {
            if v.burning.is_empty() { continue; }
            // Simulation time: a pause holds the soot where it is. A rewind restarts the clock.
            if now < v.scorch_due_s - VOLUME_PERIOD_S { v.scorch_due_s = now; }
            if now >= v.scorch_due_s { v.scorch_due_s = now + VOLUME_PERIOD_S; v.burn_step(); }
        }
    }

    /// Upload finished builds while the budget lasts.
    fn upload(&mut self, dev: &mut dyn Device, spent: &mut dyn FnMut() -> bool) {
        while let Some(done) = self.ready.front() {
            // A result nobody waits for costs nothing: drop it without asking the timer.
            let Some(v) = self.vessels.get_mut(&done.vessel) else { self.ready.pop_front(); continue };
            if spent() { break; }
            let Some(done) = self.ready.pop_front() else { break };
            let generation = done.generation;
            match done.output {
                MeshOutput::Chunk(mesh) => Self::upload_chunk(dev, &mut self.cache, &mut self.retired, self.frame, v, generation, *mesh),
                MeshOutput::Volume { coord, volume } => {
                    let Some(i) = v.chunk_index(coord) else { continue };
                    let c = &mut v.chunks[i];
                    let Some(asked) = c.asked.filter(|a| a.generation == generation && a.volume_only) else { continue };
                    c.asked = None;
                    // The mesh this volume was asked for has since been replaced or evicted.
                    if c.built_epoch != Some(asked.epoch) || c.empty { continue; }
                    Self::set_volume(dev, v.pose, v.shown && v.drawn == Some(Lod::L0), v.shadows, &v.look, c, &volume);
                    c.volume_built = asked.volume_rev;
                }
                MeshOutput::Lod { lod, surface } => {
                    let Some(i) = lod_index(lod) else { continue };
                    let key = ChunkKey { blueprint: v.design.hash, coord: RenderChunkCoord::default(), lod, damage_epoch: 0 };
                    // The build may be owed to the cache even if this vessel has stopped waiting for it.
                    let mine = matches!(self.cache.get(&key), Some(CacheEntry::Building { generation: g, .. }) if *g == generation);
                    let asked = v.lods[i].asked.filter(|a| a.generation == generation);
                    if asked.is_none() && !mine { continue; }
                    let mesh = if surface.is_empty() { Rid::Invalid } else { dev.mesh(&surface) };
                    let empty = !mesh.is_valid();
                    if mine { self.cache.insert(key, CacheEntry::Ready { mesh, shadow: Rid::Invalid, empty, refs: asked.is_some() as u32, idle_frame: self.frame }); }
                    let Some(asked) = asked else { continue };
                    v.lods[i].asked = None;
                    let fresh = MeshRef { mesh, shadow: Rid::Invalid, shared: if mine { Some(key) } else { None } };
                    let visible = v.shown && v.drawn == Some(lod);
                    Self::swap_lod(dev, &mut self.cache, &mut self.retired, self.frame, v, i, fresh, empty, visible);
                    v.lods[i].built_epoch = Some(asked.epoch);
                }
                MeshOutput::Mask(mask) => {
                    let Some((_, rev)) = v.mask.asked.filter(|a| a.0 == generation) else { continue };
                    v.mask.asked = None;
                    v.mask.built_rev = Some(rev);
                    v.mask.data = Some(mask);
                    v.mask.uploaded = false;
                }
                MeshOutput::Failed(error) => {
                    // A grid hull-mesh refuses will be refused again: stop asking for this vessel.
                    if !v.failed { dev.warn(&format!("hull meshing refused vessel {:?}: {error}", v.entity)); }
                    v.failed = true;
                    for c in &mut v.chunks { if c.asked.is_some_and(|a| a.generation == generation) { c.asked = None; } }
                    for l in &mut v.lods { if l.asked.is_some_and(|a| a.generation == generation) { l.asked = None; } }
                    if v.mask.asked.is_some_and(|a| a.0 == generation) { v.mask.asked = None; }
                    let entity = v.entity;
                    self.cache.retain(|_, e| !matches!(e, CacheEntry::Building { owner, .. } if *owner == entity));
                }
            }
        }
    }

    /// Create or update a chunk's volume, and with the first one its material and instance.
    fn set_volume(dev: &mut dyn Device, pose: Affine3A, visible: bool, shadows: bool, look: &ChunkLook, c: &mut ChunkSlot, volume: &VolumeTexture) {
        c.volume = dev.volume(c.volume, volume);
        if !c.material.is_valid() && c.volume.is_valid() {
            let origin = [c.coord.x as f32, c.coord.y as f32, c.coord.z as f32].map(|v| v * super::CHUNK_M);
            c.material = dev.chunk_material(c.volume, origin, look);
            if c.heat > 0.0 { dev.set_heat(c.material, c.heat); }
        }
        if !c.instance.is_valid() && c.mesh.mesh.is_valid() {
            c.instance = dev.instance(c.mesh.mesh, c.material, &pose, visible, shadows);
        }
    }

    fn upload_chunk(dev: &mut dyn Device, cache: &mut HashMap<ChunkKey, CacheEntry>, retired: &mut VecDeque<(u64, Rid)>, frame: u64, v: &mut Vessel, generation: u32, mesh: ChunkMesh) {
        let Some(i) = v.chunk_index(mesh.coord) else { return };
        let key = ChunkKey { blueprint: v.design.hash, coord: mesh.coord, lod: Lod::L0, damage_epoch: 0 };
        // The build may be owed to the cache even if this vessel has stopped waiting for it (its chunk was
        // evicted or holed meanwhile): other hulls of the design are waiting on the cache entry.
        let mine = matches!(cache.get(&key), Some(CacheEntry::Building { generation: g, .. }) if *g == generation);
        let asked = v.chunks[i].asked.filter(|a| a.generation == generation && !a.volume_only);
        if asked.is_none() && !mine { return; }
        let mut fresh = MeshRef::NONE;
        if !mesh.empty {
            fresh.mesh = dev.mesh(&mesh.surface);
            if fresh.mesh.is_valid() && !mesh.shadow.is_empty() { fresh.shadow = dev.shadow_mesh(fresh.mesh, &mesh.shadow); }
        }
        let empty = !fresh.mesh.is_valid();
        if mine {
            cache.insert(key, CacheEntry::Ready { mesh: fresh.mesh, shadow: fresh.shadow, empty, refs: asked.is_some() as u32, idle_frame: frame });
            fresh.shared = Some(key);
        }
        let Some(asked) = asked else { return };
        let visible = v.shown && v.drawn == Some(Lod::L0);
        let (pose, shadows) = (v.pose, v.shadows);
        let c = &mut v.chunks[i];
        c.asked = None;
        // Swap: the new mesh goes onto the instance before the old one is let go (L1929).
        let mut old = std::mem::replace(&mut c.mesh, fresh);
        c.empty = empty;
        c.built_epoch = Some(asked.epoch);
        if empty {
            if c.instance.is_valid() { dev.set_visible(c.instance, false); }
        } else {
            if c.instance.is_valid() { dev.set_base(c.instance, c.mesh.mesh); dev.set_visible(c.instance, visible); }
            Self::set_volume(dev, pose, visible, shadows, &v.look, c, &mesh.volume);
            c.volume_built = asked.volume_rev;
        }
        Self::release(cache, retired, frame, &mut old);
    }

    #[allow(clippy::too_many_arguments)]
    fn swap_lod(dev: &mut dyn Device, cache: &mut HashMap<ChunkKey, CacheEntry>, retired: &mut VecDeque<(u64, Rid)>, frame: u64, v: &mut Vessel, i: usize, fresh: MeshRef, empty: bool, visible: bool) {
        let (pose, shadows) = (v.pose, v.shadows);
        let l = &mut v.lods[i];
        let mut old = std::mem::replace(&mut l.mesh, fresh);
        l.empty = empty;
        if empty {
            if l.instance.is_valid() { dev.set_visible(l.instance, false); }
        } else if l.instance.is_valid() {
            dev.set_base(l.instance, l.mesh.mesh);
            dev.set_visible(l.instance, visible);
        } else {
            let material = dev.lod_material();
            l.instance = dev.instance(l.mesh.mesh, material, &pose, visible, shadows);
        }
        Self::release(cache, retired, frame, &mut old);
    }

    /// List what every posed vessel is missing for its picked LOD and send the most visible first.
    fn request(&mut self) {
        self.wants.clear();
        let now = self.sim_time_s;
        for v in self.vessels.values_mut() {
            if !v.posed || v.failed { continue; }
            let boosted = v.own || v.targeted || v.dock;
            let priority = screen_area(v.radius, v.dist_m) * if v.own || v.dock { 4.0 } else if v.targeted { 2.0 } else { 1.0 };
            let mut want = |what: What| self.wants.push(Want { priority, boosted, vessel: v.entity, what });
            match lod_index(v.target) {
                None => {
                    let volumes_due = now >= v.volume_due_s || now < v.volume_due_s - VOLUME_PERIOD_S;
                    let mut asked_volume = false;
                    for (i, c) in v.chunks.iter().enumerate() {
                        if c.asked.is_some() { continue; }
                        if c.built_epoch != Some(c.dirty_epoch) { want(What::Chunk(i)); }
                        else if !c.empty && !c.volume.is_valid() { want(What::Volume(i)); }
                        else if !c.empty && c.volume_built != c.volume_rev && volumes_due { want(What::Volume(i)); asked_volume = true; }
                    }
                    if asked_volume { v.volume_due_s = now + VOLUME_PERIOD_S; }
                }
                Some(i) => if v.lods[i].asked.is_none() && v.lods[i].built_epoch != Some(v.epoch) { want(What::Lod(v.target)); },
            }
            // A masked hull (it has a layer) keeps its mask in step with its cells, at most every 100 ms.
            if v.mask.layer.is_some() && v.mask.asked.is_none() && v.mask.built_rev != Some(v.cells_rev) && self.clock_s - v.mask.asked_at_s >= hull_mesh::mask::MASK_REBUILD_MIN_S {
                want(What::Mask);
            }
        }
        self.wants.sort_by(|a, b| b.priority.total_cmp(&a.priority).then(a.vessel.cmp(&b.vessel)));
        let wants = std::mem::take(&mut self.wants);
        for w in &wants {
            if self.in_flight >= if w.boosted { IN_FLIGHT_PRIORITY } else { IN_FLIGHT_OTHER } {
                // Ordered by priority: once a boosted want is refused nothing later fits either.
                if w.boosted { break; }
                continue;
            }
            self.dispatch(w);
        }
        self.wants = wants;
    }

    fn dispatch(&mut self, w: &Want) {
        let Some(v) = self.vessels.get_mut(&w.vessel) else { return };
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let send = |outbox: &mut Vec<MeshJob>, in_flight: &mut u32, cells: Arc<HullCells>, work: MeshWork| {
            outbox.push(MeshJob { vessel: w.vessel, blueprint: v.design.hash, generation, priority: w.priority, cells, work });
            *in_flight += 1;
        };
        match w.what {
            What::Chunk(i) => {
                let c = &mut v.chunks[i];
                let key = ChunkKey { blueprint: v.design.hash, coord: c.coord, lod: Lod::L0, damage_epoch: 0 };
                let shareable = c.dirty_epoch == 0 && !v.dock;
                match if shareable { Self::cache_take(&mut self.cache, &key) } else { Cached::Absent } {
                    // Another hull of this design has the mesh: take it. Our own volume is asked for on the
                    // next pass (`What::Volume`); nothing is in flight for this.
                    Cached::Ready { mesh, shadow, empty } => {
                        let mut old = std::mem::replace(&mut c.mesh, MeshRef { mesh, shadow, shared: Some(key) });
                        c.empty = empty;
                        c.built_epoch = Some(0);
                        Self::release(&mut self.cache, &mut self.retired, self.frame, &mut old);
                    }
                    // Being built for another hull: wait for it.
                    Cached::Building => {}
                    Cached::Absent => {
                        if shareable { self.cache.insert(key, CacheEntry::Building { owner: w.vessel, generation }); }
                        c.asked = Some(Asked { generation, epoch: c.dirty_epoch, volume_rev: c.volume_rev, volume_only: false });
                        send(&mut self.outbox, &mut self.in_flight, v.cells.clone(), MeshWork::Chunks(vec![c.coord]));
                    }
                }
            }
            What::Volume(i) => {
                let c = &mut v.chunks[i];
                c.asked = Some(Asked { generation, epoch: c.dirty_epoch, volume_rev: c.volume_rev, volume_only: true });
                send(&mut self.outbox, &mut self.in_flight, v.cells.clone(), MeshWork::Volumes(vec![c.coord]));
            }
            What::Lod(lod) => {
                let Some(i) = lod_index(lod) else { return };
                let key = ChunkKey { blueprint: v.design.hash, coord: RenderChunkCoord::default(), lod, damage_epoch: 0 };
                let shareable = v.epoch == 0 && !v.dock;
                match if shareable { Self::cache_take(&mut self.cache, &key) } else { Cached::Absent } {
                    // The instance is made by `present`, which owns the engine calls of showing.
                    Cached::Ready { mesh, empty, .. } => {
                        let mut old = std::mem::replace(&mut v.lods[i].mesh, MeshRef { mesh, shadow: Rid::Invalid, shared: Some(key) });
                        v.lods[i].empty = empty;
                        v.lods[i].built_epoch = Some(0);
                        Self::release(&mut self.cache, &mut self.retired, self.frame, &mut old);
                    }
                    Cached::Building => {}
                    Cached::Absent => {
                        if shareable { self.cache.insert(key, CacheEntry::Building { owner: w.vessel, generation }); }
                        v.lods[i].asked = Some(Asked { generation, epoch: v.epoch, volume_rev: 0, volume_only: false });
                        send(&mut self.outbox, &mut self.in_flight, v.cells.clone(), MeshWork::Lod(lod));
                    }
                }
            }
            What::Mask => {
                // The mask reads the whole grid, part cells included, with this vessel's damage and seals.
                let mut cells = v.mask.cells.take().unwrap_or_else(|| Arc::new(HullCells { size: v.cells.size, cells: v.design.grid.cells.clone(), ..HullCells::default() }));
                {
                    let m = Arc::make_mut(&mut cells);
                    m.damage.clone_from(&v.cells.damage);
                    m.sealed_air.clone_from(&v.cells.sealed_air);
                    m.design_waterline_y = v.cells.design_waterline_y;
                }
                v.mask.cells = Some(cells.clone());
                v.mask.asked = Some((generation, v.cells_rev));
                v.mask.asked_at_s = self.clock_s;
                send(&mut self.outbox, &mut self.in_flight, cells, MeshWork::Mask);
            }
        }
    }

    /// Show the picked LOD once it is current, hide what is not posed, and place what moved.
    fn present(&mut self, dev: &mut dyn Device) {
        self.chunks_drawn = 0;
        for v in self.vessels.values_mut() {
            if !v.posed {
                if v.shown {
                    if let Some(lod) = v.drawn { v.set_lod_visible(dev, lod, false); }
                    if v.marking_instance.is_valid() { dev.set_visible(v.marking_instance, false); }
                    v.shown = false;
                }
                continue;
            }
            // A LOD mesh attached from the cache has no instance yet.
            for i in 0..v.lods.len() {
                if !v.lods[i].instance.is_valid() && v.lods[i].mesh.mesh.is_valid() {
                    let material = dev.lod_material();
                    v.lods[i].instance = dev.instance(v.lods[i].mesh.mesh, material, &v.pose, false, v.shadows);
                }
            }
            let mut switched = false;
            if v.drawn != Some(v.target) && v.lod_current(v.target) {
                if let Some(old) = v.drawn.filter(|_| v.shown) { v.set_lod_visible(dev, old, false); }
                v.drawn = Some(v.target);
                switched = true;
            }
            if let Some(lod) = v.drawn {
                if switched || !v.shown { v.set_lod_visible(dev, lod, true); v.shown = true; v.xf_set = None; }
            }
            Self::markings(dev, &mut self.retired, self.frame, v);
            let Some(lod) = v.drawn else { continue };
            if v.xf_set != Some(v.pose) {
                match lod_index(lod) {
                    None => for c in &v.chunks { if c.instance.is_valid() && !c.empty { dev.set_transform(c.instance, &v.pose); } },
                    Some(i) => if v.lods[i].instance.is_valid() { dev.set_transform(v.lods[i].instance, &v.pose); },
                }
                if v.marking_instance.is_valid() { dev.set_transform(v.marking_instance, &v.pose); }
                v.xf_set = Some(v.pose);
            }
            // Fire glows on the soot of the chunks it burns in.
            if lod == Lod::L0 {
                for c in v.chunks.iter_mut() {
                    let heat = if v.burning.values().any(|b| b.chunks.binary_search(&c.coord).is_ok()) { 1.0 } else { 0.0 };
                    if heat != c.heat { c.heat = heat; if c.material.is_valid() { dev.set_heat(c.material, heat); } }
                }
            }
            self.chunks_drawn += match lod_index(lod) {
                None => v.chunks.iter().filter(|c| c.instance.is_valid() && !c.empty).count() as u32,
                Some(i) => (v.lods[i].instance.is_valid() && !v.lods[i].empty) as u32,
            };
        }
    }

    /// Rebuild the stripe and flag quads when the standing set changed, and show them at LOD0 and LOD1.
    fn markings(dev: &mut dyn Device, retired: &mut VecDeque<(u64, Rid)>, frame: u64, v: &mut Vessel) {
        let visible = v.shown && v.drawn.is_some_and(|l| l <= MARKINGS_MAX_LOD);
        if v.markings_dirty && visible {
            v.markings_dirty = false;
            let surface = hull_mesh::surface::encode_colored_surface(&marking_mesh(&v.markings, &v.design.palette));
            let mut old = v.marking_mesh;
            v.marking_mesh = if surface.is_empty() { Rid::Invalid } else { dev.mesh(&surface) };
            match (v.marking_mesh.is_valid(), v.marking_instance.is_valid()) {
                (true, true) => dev.set_base(v.marking_instance, v.marking_mesh),
                (true, false) => { let material = dev.lod_material(); v.marking_instance = dev.instance(v.marking_mesh, material, &v.pose, true, false); }
                (false, true) => { dev.set_visible(v.marking_instance, false); Self::retire(retired, frame, &mut v.marking_instance); }
                (false, false) => {}
            }
            Self::retire(retired, frame, &mut old);
        }
        if v.marking_instance.is_valid() { dev.set_visible(v.marking_instance, visible); }
    }

    /// Evict LOD0 chunks long unseen, drop idle cache entries, free retired RIDs.
    fn sweep(&mut self, dev: &mut dyn Device, spent: &mut dyn FnMut() -> bool) {
        let now = self.sim_time_s;
        for v in self.vessels.values_mut() {
            // A rewind (replay seek) must not leave a timestamp in the future keeping chunks forever.
            if v.last_lod0_s > now { v.last_lod0_s = now; }
            if v.dock || (v.posed && (v.drawn == Some(Lod::L0) || v.target == Lod::L0)) || now - v.last_lod0_s < CHUNK_EVICT_S { continue; }
            if !v.chunks.iter().any(|c| c.built_epoch.is_some() || c.instance.is_valid() || c.asked.is_some()) { continue; }
            for c in &mut v.chunks { Self::release_chunk(&mut self.cache, &mut self.retired, self.frame, c); }
            // An unposed hull last drawn at LOD0 has nothing to show now: it starts again when it returns.
            if v.drawn == Some(Lod::L0) { v.drawn = None; v.shown = false; }
        }
        if self.frame % 64 == 0 {
            let (frame, retired) = (self.frame, &mut self.retired);
            self.cache.retain(|_, e| match e {
                CacheEntry::Ready { mesh, shadow, refs: 0, idle_frame, .. } if frame.saturating_sub(*idle_frame) >= CACHE_IDLE_FRAMES => {
                    Self::retire(retired, frame, mesh);
                    Self::retire(retired, frame, shadow);
                    false
                }
                _ => true,
            });
        }
        if spent() { return; }
        let quota = FREES_PER_FRAME.max(self.retired.len().saturating_sub(RETIRE_BACKLOG) / 16);
        for _ in 0..quota {
            match self.retired.front() {
                Some((at, _)) if at + RETIRE_FRAMES <= self.frame => { if let Some((_, rid)) = self.retired.pop_front() { dev.free(rid); } }
                _ => break,
            }
        }
    }

    /// Choose the masked hulls (nearest 8 within range of the camera), keep their layers stable, upload at
    /// most one changed layer, and return the matrices: layer `i` maps render space to mask metres
    /// (`x = 0.5 m x texel u`, `z = 0.5 m x (texel v - 64)`, `y` = vessel-local height), column-major. An
    /// unused or not-yet-uploaded layer maps everything far outside the mask.
    pub(crate) fn masks(&mut self, dev: &mut dyn Device, camera: Vec3, out: &mut super::HullMasks) {
        out.count = 0;
        out.world_to_hull.clear();
        out.dirty_layers.clear();
        let mut near: Vec<(f32, EntityId)> = self.vessels.values().filter(|v| v.posed && !v.failed && v.dist_from(camera) <= hull_mesh::mask::MASK_RANGE_M as f32).map(|v| (v.dist_from(camera), v.entity)).collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        near.truncate(hull_mesh::mask::MAX_MASKED_HULLS);
        // Give up the layers of hulls that left the set, then seat the newcomers in the free ones.
        for layer in 0..self.mask_layers.len() {
            let Some(entity) = self.mask_layers[layer] else { continue };
            if !near.iter().any(|n| n.1 == entity) {
                self.mask_layers[layer] = None;
                if let Some(v) = self.vessels.get_mut(&entity) { v.mask.layer = None; v.mask.uploaded = false; }
            }
        }
        for (_, entity) in &near {
            let Some(v) = self.vessels.get_mut(entity) else { continue };
            if v.mask.layer.is_some() { continue; }
            if let Some(free) = self.mask_layers.iter().position(Option::is_none) {
                self.mask_layers[free] = Some(*entity);
                v.mask.layer = Some(free as u8);
                v.mask.uploaded = false;
            }
        }
        if near.is_empty() { return; }
        out.texture = dev.mask_texture();
        // Far outside every mask: the water shader's bounds test fails and it moves on.
        const NOWHERE: [f32; 16] = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0e6, 0.0, -1.0e6, 1.0];
        let mut uploaded_one = false;
        for layer in 0..self.mask_layers.len() {
            let mut matrix = NOWHERE;
            if let Some(v) = self.mask_layers[layer].and_then(|e| self.vessels.get_mut(&e)) {
                if let Some(data) = &v.mask.data {
                    if !v.mask.uploaded && !uploaded_one {
                        dev.mask_layer(layer as u32, data);
                        v.mask.uploaded = true;
                        uploaded_one = true;
                        out.dirty_layers.push(layer as u32);
                    }
                    if v.mask.uploaded {
                        let to_mask = Affine3A::from_translation(Vec3::new(0.0, 0.0, -data.offset_z_m)) * v.pose.inverse();
                        matrix = glam::Mat4::from(to_mask).to_cols_array();
                        out.count = layer as u32 + 1;
                    }
                }
            }
            out.world_to_hull.push(matrix);
        }
        out.world_to_hull.truncate(out.count as usize);
    }

    /// Free everything now (shutdown): the engine is going away or the scene is, so nothing waits two frames.
    pub(crate) fn free_all(&mut self, dev: &mut dyn Device) {
        let ids: Vec<EntityId> = self.vessels.keys().copied().collect();
        for id in ids { self.drop_vessel(dev, id); }
        for (_, e) in self.cache.drain() {
            if let CacheEntry::Ready { mesh, shadow, .. } = e { for rid in [mesh, shadow] { if rid.is_valid() { dev.free(rid); } } }
        }
        while let Some((_, rid)) = self.retired.pop_front() { dev.free(rid); }
        self.pending.clear();
        self.ready.clear();
        self.outbox.clear();
        self.in_flight = 0;
        self.hold = Default::default();
    }

    /// Bounding box of a drawn vessel in render space.
    pub(crate) fn bounds(&self, vessel: EntityId) -> Option<([f32; 3], [f32; 3])> {
        let v = self.vessels.get(&vessel)?;
        let size = Vec3::from(v.design.size_m());
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for corner in 0..8 {
            let p = v.pose.transform_point3(Vec3::new(if corner & 1 == 0 { 0.0 } else { size.x }, if corner & 2 == 0 { 0.0 } else { size.y }, if corner & 4 == 0 { 0.0 } else { size.z }));
            lo = lo.min(p);
            hi = hi.max(p);
        }
        Some((lo.into(), hi.into()))
    }

    /// Ray against a drawn vessel's standing cells (part cells included; shaped cells count as full, as
    /// the simulation treats them): the first cell hit, the face entered (`Face::ALL` index) and the
    /// distance in metres.
    pub(crate) fn raycast(&self, vessel: EntityId, origin: [f32; 3], dir: [f32; 3]) -> Option<([i16; 3], u8, f32)> {
        let v = self.vessels.get(&vessel)?;
        let inv = v.pose.inverse();
        let (o, d) = (inv.transform_point3(Vec3::from(origin)), inv.transform_vector3(Vec3::from(dir)));
        if !o.is_finite() || !d.is_finite() || d.length_squared() < 1e-12 { return None; }
        let d = d.normalize();
        let cell = hull_mesh::CELL_M_F32;
        let size = v.design.grid.size.map(|s| s as i32);
        let hi = Vec3::new(size[0] as f32, size[1] as f32, size[2] as f32) * cell;
        // Clip to the grid box.
        let (mut t0, mut t1, mut face) = (0.0f32, f32::MAX, 0u8);
        for a in 0..3 {
            if d[a].abs() < 1e-9 {
                if o[a] < 0.0 || o[a] > hi[a] { return None; }
                continue;
            }
            let (near, far) = if d[a] > 0.0 { (-o[a] / d[a], (hi[a] - o[a]) / d[a]) } else { ((hi[a] - o[a]) / d[a], -o[a] / d[a]) };
            if near > t0 { t0 = near; face = (2 * a) as u8 + if d[a] > 0.0 { 0 } else { 1 }; }
            t1 = t1.min(far);
        }
        if t0 > t1 { return None; }
        // Walk the cells along the ray (Amanatides and Woo).
        let start = o + d * (t0 + 1e-4);
        let mut at = [0i32; 3];
        let mut step = [0i32; 3];
        let mut t_next = [f32::MAX; 3];
        let mut t_step = [f32::MAX; 3];
        for a in 0..3 {
            at[a] = ((start[a] / cell).floor() as i32).clamp(0, size[a] - 1);
            if d[a].abs() >= 1e-9 {
                step[a] = if d[a] > 0.0 { 1 } else { -1 };
                let edge = (at[a] + if d[a] > 0.0 { 1 } else { 0 }) as f32 * cell;
                t_next[a] = (edge - o[a]) / d[a];
                t_step[a] = cell / d[a].abs();
            }
        }
        let mut t = t0;
        for _ in 0..(size[0] + size[1] + size[2] + 3) {
            let index = at[0] as usize + size[0] as usize * (at[2] as usize + size[2] as usize * at[1] as usize);
            if v.standing(index) { return Some(([at[0] as i16, at[1] as i16, at[2] as i16], face, t.max(0.0))); }
            let a = if t_next[0] <= t_next[1] && t_next[0] <= t_next[2] { 0 } else if t_next[1] <= t_next[2] { 1 } else { 2 };
            t = t_next[a];
            if t > t1 || step[a] == 0 { return None; }
            at[a] += step[a];
            if at[a] < 0 || at[a] >= size[a] { return None; }
            t_next[a] += t_step[a];
            face = (2 * a) as u8 + if step[a] > 0 { 0 } else { 1 };
        }
        None
    }
}

impl Vessel {
    fn dist_from(&self, camera: Vec3) -> f32 { ((self.pose.transform_point3(self.centre) - camera).length() - self.radius).max(0.0) }
}

/// Stripes and flags as flat quads in their paint (hull numbers are text, drawn by the view node), wound
/// clockwise seen from outside.
pub(crate) fn marking_mesh(quads: &[MarkingQuad], palette: &[[f32; 3]]) -> crate::seams::MeshData {
    let mut mesh = crate::seams::MeshData::new();
    for q in quads {
        if q.kind == MarkingKind::HullNumber { continue; }
        let (c, n, r) = (Vec3::from(q.centre), Vec3::from(q.normal), Vec3::from(q.right));
        if !(c.is_finite() && n.is_finite() && r.is_finite()) || !(q.size_m[0] > 0.0 && q.size_m[1] > 0.0) { continue; }
        // Seen from outside, right x up = normal.
        let (r, u) = (r * (0.5 * q.size_m[0]), n.cross(r) * (0.5 * q.size_m[1]));
        let rgb = palette.get(q.paint as usize).copied().unwrap_or(super::design::FALLBACK_PAINT);
        let base = mesh.positions.len() as u32;
        for p in [c - r - u, c + r - u, c + r + u, c - r + u] {
            mesh.positions.push(p.into());
            mesh.normals.push(n.into());
            mesh.colors.push([rgb[0], rgb[1], rgb[2], 1.0]);
        }
        mesh.indices.extend([base, base + 2, base + 1, base, base + 3, base + 2]);
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::super::design::tests::{box_design, STEEL};
    use super::super::thread;
    use super::*;
    use crate::interp::VesselPose;
    use content::cell::{Cell, CellPos};
    use hull_mesh::pipeline::ChunkMesher;
    use sim_systems::protocol::EntityIntro;
    use std::collections::BTreeSet;

    /// Counts engine calls and tracks which RIDs are alive, what is visible and what each instance draws.
    #[derive(Default)]
    struct Fake {
        next: u64,
        live: BTreeMap<u64, &'static str>,
        visible: BTreeSet<u64>,
        base: BTreeMap<u64, u64>,
        transforms: u32,
        volume_updates: u32,
        heat: BTreeMap<u64, f32>,
        mask_layers: Vec<u32>,
        warnings: Vec<String>,
        freed_this_frame: u32,
        looks: u32,
    }
    impl Fake {
        fn make(&mut self, kind: &'static str) -> Rid { self.next += 1; self.live.insert(self.next, kind); Rid::new(self.next) }
        fn count(&self, kind: &str) -> usize { self.live.values().filter(|k| **k == kind).count() }
        fn shown(&self) -> usize { self.visible.len() }
    }
    impl Device for Fake {
        fn mesh(&mut self, surface: &SurfaceBytes) -> Rid { assert!(!surface.is_empty()); self.make("mesh") }
        fn shadow_mesh(&mut self, mesh: Rid, shadow: &SurfaceBytes) -> Rid { assert!(self.live.contains_key(&mesh.to_u64()) && !shadow.is_empty()); self.make("shadow") }
        fn volume(&mut self, existing: Rid, volume: &VolumeTexture) -> Rid {
            assert_eq!(volume.bytes.len(), VolumeTexture::BYTES);
            if existing.is_valid() { assert_eq!(self.live.get(&existing.to_u64()), Some(&"volume")); self.volume_updates += 1; existing } else { self.make("volume") }
        }
        fn chunk_material(&mut self, volume: Rid, _origin: [f32; 3], _look: &ChunkLook) -> Rid { assert!(volume.is_valid()); self.make("material") }
        fn set_heat(&mut self, material: Rid, heat: f32) { self.heat.insert(material.to_u64(), heat); }
        fn set_look(&mut self, material: Rid, _look: &ChunkLook) { assert!(self.live.contains_key(&material.to_u64())); self.looks += 1; }
        fn lod_material(&mut self) -> Rid { Rid::new(1_000_000) }
        fn instance(&mut self, mesh: Rid, _material: Rid, _xf: &Affine3A, visible: bool, _shadows: bool) -> Rid {
            assert!(self.live.contains_key(&mesh.to_u64()), "instance of a freed mesh");
            let rid = self.make("instance");
            self.base.insert(rid.to_u64(), mesh.to_u64());
            if visible { self.visible.insert(rid.to_u64()); }
            rid
        }
        fn set_base(&mut self, instance: Rid, mesh: Rid) { assert!(self.live.contains_key(&mesh.to_u64())); self.base.insert(instance.to_u64(), mesh.to_u64()); }
        fn set_transform(&mut self, instance: Rid, _xf: &Affine3A) { assert!(self.live.contains_key(&instance.to_u64())); self.transforms += 1; }
        fn set_visible(&mut self, instance: Rid, visible: bool) {
            assert!(self.live.contains_key(&instance.to_u64()));
            if visible { self.visible.insert(instance.to_u64()); } else { self.visible.remove(&instance.to_u64()); }
        }
        fn set_shadows(&mut self, _instance: Rid, _on: bool) {}
        fn free(&mut self, rid: Rid) {
            let id = rid.to_u64();
            assert!(self.live.remove(&id).is_some(), "double free of {id}");
            assert!(!self.visible.contains(&id), "freed a visible instance");
            assert!(!self.base.iter().any(|(i, m)| *m == id && self.visible.contains(i)), "freed a mesh a visible instance draws");
            self.base.remove(&id);
            self.freed_this_frame += 1;
        }
        fn mask_texture(&mut self) -> Rid { Rid::new(2_000_000) }
        fn mask_layer(&mut self, layer: u32, mask: &HullMask) { assert_eq!(mask.texels.len(), hull_mesh::mask::MASK_W * hull_mesh::mask::MASK_H); self.mask_layers.push(layer); }
        fn warn(&mut self, message: &str) { self.warnings.push(message.to_string()); }
    }

    struct Rig { core: HullCore, dev: Fake, mesher: ChunkMesher, designs: BTreeMap<ContentHash, Arc<DrawDesign>>, time_s: f64, built: Vec<MeshWork> }
    impl Rig {
        fn new() -> Self { Self { core: HullCore::new(), dev: Fake::default(), mesher: ChunkMesher::new(), designs: BTreeMap::new(), time_s: 0.0, built: Vec::new() } }
        fn add(&mut self, design: DrawDesign) -> Arc<DrawDesign> { let d = Arc::new(design); self.designs.insert(d.hash, d.clone()); d }
        /// One frame at 60 Hz with every upload allowed, the camera at the origin looking at 1080p.
        fn frame(&mut self, poses: &[(u64, f32)], events: &[WireEvent]) { self.frame_with(poses, events, &mut || false) }
        fn frame_with(&mut self, poses: &[(u64, f32)], events: &[WireEvent], spent: &mut dyn FnMut() -> bool) {
            self.time_s += 1.0 / 60.0;
            self.dev.freed_this_frame = 0;
            let vessels = poses.iter().map(|&(id, x)| VesselPose { entity: EntityId::from_u64(id), pos: [x, 0.0, 0.0], rot: [0.0, 0.0, 0.0, 1.0], own: id == 1, ..Default::default() }).collect();
            let view = ViewFrame { sim_time_s: self.time_s, render_tick: self.time_s * 60.0, vessels, ..Default::default() };
            let lens = HullLens { camera: Vec3::new(0.0, 10.0, -30.0), fov_y_rad: 60f64.to_radians(), viewport_h_px: 1080.0, tier: QualityTier::Medium, degrade_step: 0, delta_s: 1.0 / 60.0 };
            let designs = &self.designs;
            self.core.frame(&mut self.dev, &view, events, lens, spent, &|h| designs.get(h).cloned());
        }
        /// Run the meshing thread's work for everything sent so far.
        fn build(&mut self) -> usize {
            let jobs: Vec<MeshJob> = self.core.outbox.drain(..).collect();
            for job in &jobs {
                self.built.push(job.work.clone());
                let mut out = Vec::new();
                thread::process(&mut self.mesher, job, &mut |d| out.push(d));
                assert_eq!(out.len(), thread::outputs(&job.work));
                for d in out { self.core.receive(d); }
            }
            jobs.len()
        }
        /// Frames and builds until two frames in a row ask for nothing and have nothing left to upload.
        fn settle(&mut self, poses: &[(u64, f32)]) {
            let mut idle = 0;
            for _ in 0..60 {
                self.frame(poses, &[]);
                idle = if self.build() == 0 && self.core.pending_uploads() == 0 { idle + 1 } else { 0 };
                if idle == 2 { return; }
            }
            panic!("the renderer never settled");
        }
        fn vessel(&self, id: u64) -> &Vessel { &self.core.vessels[&EntityId::from_u64(id)] }
        fn chunk_builds(&self) -> usize { self.built.iter().filter(|w| matches!(w, MeshWork::Chunks(_))).count() }
    }

    fn ev(event: SimEvent) -> WireEvent { WireEvent { tick: 0, event } }
    fn reveal(entity: u64, hash: ContentHash) -> WireEvent { reveal_with(entity, hash, Vec::new(), Vec::new()) }
    fn reveal_with(entity: u64, hash: ContentHash, damage: Vec<CellChange>, removed: Vec<CellIndex>) -> WireEvent {
        ev(SimEvent::EntityRevealed(Box::new(EntityIntro { entity: EntityId::from_u64(entity), side: Default::default(), blueprint: hash, exterior_only: false, class: None, name: String::new(), player: None, status: Default::default(), damage, removed, parent: None, track: Default::default() })))
    }
    fn damaged(vessel: u64, cells: &[u32], bucket: u8, cause: DamageCause) -> WireEvent {
        ev(SimEvent::Phys(PhysEvent::CellsDamaged { vessel: EntityId::from_u64(vessel), changes: cells.iter().map(|c| CellChange { cell: CellIndex(*c), bucket }).collect(), cause, attacker: EntityId::NONE }))
    }
    /// 40 x 8 x 12 cells: two chunks along X.
    fn hull() -> DrawDesign { box_design([40, 8, 12]) }
    fn cell(d: &DrawDesign, x: i16, y: i16, z: i16) -> u32 { d.grid.index(CellPos::new(x, y, z)).unwrap().0 }

    #[test]
    fn a_revealed_hull_waits_for_its_design_then_meshes_and_shows() {
        let mut r = Rig::new();
        let d = hull();
        let hash = d.hash;
        // Revealed and posed, but the design has not arrived: nothing drawn, nothing asked, no panic.
        r.frame(&[(1, 0.0)], &[reveal(1, hash)]);
        assert!(r.core.vessels.is_empty() && r.core.outbox.is_empty());
        // Damage that arrives meanwhile is not lost.
        let hit = cell(&d, 39, 7, 0);
        r.frame(&[(1, 0.0)], &[damaged(1, &[hit], 8, DamageCause::Shell)]);
        r.add(d);
        r.frame(&[(1, 0.0)], &[]);
        let v = r.vessel(1);
        assert_eq!((v.target, v.drawn, v.epoch), (Lod::L0, None, 1));
        assert_eq!(v.chunks.len(), 2);
        assert_eq!(r.core.outbox.len(), 2, "both chunks asked for at once");
        assert_eq!(r.dev.shown(), 0);
        // The thread builds; the next frame uploads and shows both chunks together.
        r.build();
        r.frame(&[(1, 0.0)], &[]);
        let v = r.vessel(1);
        assert_eq!(v.drawn, Some(Lod::L0));
        assert!(v.chunks.iter().all(ChunkSlot::current));
        assert_eq!((r.dev.count("mesh"), r.dev.count("shadow"), r.dev.count("volume"), r.dev.count("material"), r.dev.count("instance")), (2, 2, 2, 2, 2));
        assert_eq!((r.dev.shown(), r.core.chunks_drawn), (2, 2));
        // One transform per chunk when it moves, none while it sits still (L1088).
        let before = r.dev.transforms;
        r.frame(&[(1, 0.0)], &[]);
        assert_eq!(r.dev.transforms, before);
        r.frame(&[(1, 5.0)], &[]);
        assert_eq!(r.dev.transforms, before + 2);
        // Not in the view: hidden, kept. Back in the view: shown again with no rebuild.
        r.frame(&[], &[]);
        assert_eq!(r.dev.shown(), 0);
        r.frame(&[(1, 5.0)], &[]);
        assert_eq!((r.dev.shown(), r.core.outbox.len()), (2, 0));
    }

    #[test]
    fn uploads_stop_when_the_budget_is_spent() {
        let mut r = Rig::new();
        let d = r.add(hull());
        r.frame(&[(1, 0.0)], &[reveal(1, d.hash)]);
        r.frame(&[(1, 0.0)], &[]);
        r.build();
        // The timer has run out: nothing uploads or frees, and the work waits.
        r.frame_with(&[(1, 0.0)], &[], &mut || true);
        assert_eq!((r.dev.count("mesh"), r.core.pending_uploads()), (0, 2));
        // Room for exactly one upload this frame: the hull is not shown half built.
        let mut asked = 0;
        r.frame_with(&[(1, 0.0)], &[], &mut || { asked += 1; asked > 1 });
        assert_eq!((r.dev.count("mesh"), r.core.pending_uploads(), r.dev.shown()), (1, 1, 0));
        r.frame(&[(1, 0.0)], &[]);
        assert_eq!((r.dev.count("mesh"), r.dev.shown()), (2, 2));
    }

    #[test]
    fn damage_swaps_one_chunk_and_frees_the_old_mesh_two_frames_later() {
        let mut r = Rig::new();
        let d = r.add(hull());
        r.frame(&[(1, 0.0)], &[reveal(1, d.hash)]);
        r.settle(&[(1, 0.0)]);
        let first: Vec<u64> = r.vessel(1).chunks.iter().map(|c| r.dev.base[&c.instance.to_u64()]).collect();
        let builds = r.chunk_builds();
        // A hole at x = 39 touches only the second chunk (cells 32..40).
        r.frame(&[(1, 0.0)], &[damaged(1, &[cell(&d, 39, 7, 6)], 8, DamageCause::Shell)]);
        let v = r.vessel(1);
        assert_eq!((v.epoch, v.chunks[0].dirty_epoch, v.chunks[1].dirty_epoch), (1, 0, 1));
        assert_eq!(v.drawn, Some(Lod::L0), "the old mesh stays up until the new one is ready");
        assert_eq!(r.dev.shown(), 2);
        assert_eq!(r.build(), 1);
        r.frame(&[(1, 0.0)], &[]);
        assert_eq!(r.chunk_builds(), builds + 1);
        let v = r.vessel(1);
        let now: Vec<u64> = v.chunks.iter().map(|c| r.dev.base[&c.instance.to_u64()]).collect();
        assert_eq!(now[0], first[0]);
        assert_ne!(now[1], first[1], "swapped with instance_set_base");
        assert_eq!(r.dev.count("instance"), 2, "the instance is reused");
        assert_eq!(r.dev.volume_updates, 1, "the volume is updated in place with its mesh");
        // The old chunk mesh was the cache's (an intact design's), so it is kept for the next hull of the
        // class, not freed. A second hole retires the damaged mesh: gone two frames later, not before.
        assert!(r.dev.live.contains_key(&first[1]));
        r.frame(&[(1, 0.0)], &[damaged(1, &[cell(&d, 38, 7, 6)], 8, DamageCause::Shell)]);
        r.build();
        r.frame(&[(1, 0.0)], &[]);
        assert_eq!(r.core.retired_len(), 2, "mesh and shadow mesh");
        assert!(r.dev.live.contains_key(&now[1]));
        r.frame(&[(1, 0.0)], &[]);
        assert!(r.dev.live.contains_key(&now[1]), "one frame later it is still alive");
        r.frame(&[(1, 0.0)], &[]);
        assert!(!r.dev.live.contains_key(&now[1]));
        assert_eq!(r.core.retired_len(), 0);
        // Damage below bucket 8, bad indices and a repeat of the same hole change no geometry.
        let epoch = r.vessel(1).epoch;
        r.frame(&[(1, 0.0)], &[damaged(1, &[cell(&d, 2, 2, 2), u32::MAX, cell(&d, 38, 7, 6)], 5, DamageCause::Collision), damaged(1, &[cell(&d, 38, 7, 6)], 8, DamageCause::Collision)]);
        assert_eq!((r.vessel(1).epoch, r.core.outbox.len()), (epoch, 0));
    }

    #[test]
    fn a_hole_opens_when_playback_reaches_the_hit() {
        let mut r = Rig::new();
        let d = r.add(hull());
        r.frame(&[(1, 0.0)], &[reveal(1, d.hash)]);
        r.settle(&[(1, 0.0)]);
        // The event is 8 ticks ahead of playback, as over a live link (the rig plays one tick per frame).
        let tick = (r.time_s * 60.0).round() as u64 + 9;
        let mut hit = damaged(1, &[cell(&d, 39, 7, 6)], 8, DamageCause::Shell);
        hit.tick = tick;
        let mut frames = 0;
        r.frame(&[(1, 0.0)], &[hit]);
        while r.vessel(1).epoch == 0 {
            assert_eq!(r.core.outbox.len(), 0, "nothing is remeshed before the cells change");
            r.frame(&[(1, 0.0)], &[]);
            frames += 1;
            assert!(frames < 20, "the hit was never applied");
        }
        // Applied the remesh lead ahead of the hit, so the new mesh is up when the shell lands.
        let ahead = tick as f64 - r.time_s * 60.0;
        assert!(ahead <= super::super::hold::MESH_LEAD_TICKS + 1e-6 && ahead > super::super::hold::MESH_LEAD_TICKS - 1.0 - 1e-6, "applied {ahead} ticks ahead");
        assert_eq!(r.build(), 1);
        r.frame(&[(1, 0.0)], &[]);
        assert!(r.vessel(1).chunks[1].current());
        assert!(r.time_s * 60.0 <= tick as f64 + 1e-6, "the hole is on screen by the tick of the hit");
        // A vessel hidden while damage for it is held: the damage goes with it and touches nothing later.
        let mut late = damaged(1, &[cell(&d, 2, 7, 6)], 8, DamageCause::Shell);
        late.tick = (r.time_s * 60.0).round() as u64 + 9;
        let at = late.tick;
        r.frame(&[(1, 0.0)], &[late]);
        r.frame(&[], &[WireEvent { tick: at, event: SimEvent::EntityHidden { entity: EntityId::from_u64(1) } }]);
        r.frame(&[(1, 0.0)], &[WireEvent { tick: at + 1, ..reveal(1, d.hash) }]);
        for _ in 0..12 { r.frame(&[(1, 0.0)], &[]); r.build(); }
        assert_eq!(r.vessel(1).epoch, 0, "the new reveal is the intact design");
    }

    #[test]
    fn frees_are_rate_limited() {
        let mut r = Rig::new();
        let d = r.add(hull());
        r.frame(&[(1, 0.0)], &[reveal(1, d.hash)]);
        r.settle(&[(1, 0.0)]);
        // Holing both chunks first makes their meshes the vessel's own, so hiding it retires them all.
        r.frame(&[(1, 0.0)], &[damaged(1, &[cell(&d, 1, 7, 6), cell(&d, 39, 7, 6)], 8, DamageCause::Shell)]);
        r.settle(&[(1, 0.0)]);
        for _ in 0..(CACHE_IDLE_FRAMES + 70) { r.frame(&[(1, 0.0)], &[]); }
        r.frame(&[(1, 0.0)], &[ev(SimEvent::EntityHidden { entity: EntityId::from_u64(1) })]);
        assert_eq!(r.dev.shown(), 0, "hidden at once");
        assert_eq!(r.core.retired_len(), 10, "two chunks of mesh, shadow, volume, material and instance");
        let mut most = 0;
        for _ in 0..8 { r.frame(&[], &[]); most = most.max(r.dev.freed_this_frame); }
        assert_eq!(most as usize, FREES_PER_FRAME);
        assert!(r.dev.live.is_empty(), "everything freed in the end: {:?}", r.dev.live);
    }

    #[test]
    fn a_new_lod_shows_only_when_it_is_current() {
        let mut r = Rig::new();
        let d = r.add(hull());
        r.frame(&[(1, 0.0)], &[reveal(1, d.hash)]);
        r.settle(&[(1, 0.0)]);
        assert_eq!(r.vessel(1).drawn, Some(Lod::L0));
        // 1,500 m away the pixel rule picks LOD2 (2 m cells from 1,247 m at 1080p and 60 degrees).
        r.frame(&[(1, 1_500.0)], &[]);
        assert_eq!((r.vessel(1).target, r.vessel(1).drawn), (Lod::L2, Some(Lod::L0)), "LOD0 stays until LOD2 exists");
        assert_eq!(r.dev.shown(), 2);
        r.build();
        r.frame(&[(1, 1_500.0)], &[]);
        assert_eq!(r.vessel(1).drawn, Some(Lod::L2));
        assert_eq!((r.dev.shown(), r.core.chunks_drawn), (1, 1), "one instance for the whole vessel, same frame");
        // Holed while at LOD2: its LOD mesh rebuilds and swaps.
        r.frame(&[(1, 1_500.0)], &[damaged(1, &[cell(&d, 39, 7, 6)], 8, DamageCause::Missile)]);
        r.build();
        r.frame(&[(1, 1_500.0)], &[]);
        assert_eq!(r.vessel(1).lods[1].built_epoch, Some(1));
        // The camera closes in. LOD0's second chunk was built before the hole: LOD2 stays up until it is
        // rebuilt, so the unholed chunk is never shown.
        r.frame(&[(1, 0.0)], &[]);
        let v = r.vessel(1);
        assert_eq!((v.target, v.drawn), (Lod::L0, Some(Lod::L2)));
        assert!(v.chunks[0].current() && !v.chunks[1].current());
        assert_eq!(r.dev.shown(), 1);
        r.build();
        r.frame(&[(1, 0.0)], &[]);
        assert_eq!((r.vessel(1).drawn, r.dev.shown()), (Some(Lod::L0), 2));
        // Hysteresis: at a distance just inside the LOD1 band the vessel does not flip back and forth.
        r.settle(&[(1, 640.0)]);
        let at = r.vessel(1).drawn;
        r.settle(&[(1, 620.0)]);
        assert_eq!(r.vessel(1).drawn, at);
    }

    #[test]
    fn hulls_of_one_design_share_meshes_but_not_volumes() {
        let mut r = Rig::new();
        let d = r.add(hull());
        r.frame(&[(1, 0.0)], &[reveal(1, d.hash)]);
        r.settle(&[(1, 0.0)]);
        let builds = r.chunk_builds();
        r.frame(&[(1, 0.0), (2, 30.0)], &[reveal(2, d.hash)]);
        r.settle(&[(1, 0.0), (2, 30.0)]);
        assert_eq!(r.chunk_builds(), builds, "the second hull meshes nothing");
        assert_eq!(r.built.iter().filter(|w| matches!(w, MeshWork::Volumes(_))).count(), 2);
        assert_eq!((r.dev.count("mesh"), r.dev.count("volume"), r.dev.count("instance")), (2, 4, 4));
        assert_eq!(r.dev.shown(), 4);
        let (a, b) = (r.vessel(1), r.vessel(2));
        assert_eq!(r.dev.base[&a.chunks[0].instance.to_u64()], r.dev.base[&b.chunks[0].instance.to_u64()]);
        assert_ne!(a.cells.wear_seed, b.cells.wear_seed);
        // Holing one leaves the other on the shared mesh.
        r.frame(&[(1, 0.0), (2, 30.0)], &[damaged(2, &[cell(&d, 1, 7, 6)], 8, DamageCause::Shell)]);
        r.settle(&[(1, 0.0), (2, 30.0)]);
        assert_eq!(r.dev.count("mesh"), 3);
        // Both leave: the shared meshes outlive them for a while, then go.
        let gone = [ev(SimEvent::EntityHidden { entity: EntityId::from_u64(1) }), ev(SimEvent::Phys(PhysEvent::VesselDespawned { vessel: EntityId::from_u64(2) }))];
        r.frame(&[], &gone);
        for _ in 0..30 { r.frame(&[], &[]); }
        assert_eq!((r.dev.count("mesh"), r.dev.count("instance"), r.dev.count("volume")), (2, 0, 0));
        // A third hull of the class arriving now still finds them.
        r.frame(&[(3, 0.0)], &[reveal(3, d.hash)]);
        r.settle(&[(3, 0.0)]);
        assert_eq!(r.chunk_builds(), builds + 1, "only the holed chunk of hull 2 was ever built again");
        r.frame(&[], &[ev(SimEvent::EntityHidden { entity: EntityId::from_u64(3) })]);
        for _ in 0..(CACHE_IDLE_FRAMES + 80) { r.frame(&[], &[]); }
        assert!(r.dev.live.is_empty(), "{:?}", r.dev.live);
    }

    #[test]
    fn a_shared_build_survives_its_first_owner() {
        let mut r = Rig::new();
        let d = r.add(hull());
        // Hull 1 asks for the chunks; hull 2 waits on the cache; hull 1 is hidden before the build lands.
        r.frame(&[(1, 0.0), (2, 30.0)], &[reveal(1, d.hash), reveal(2, d.hash)]);
        r.frame(&[(1, 0.0), (2, 30.0)], &[]);
        assert_eq!(r.core.outbox.len(), 2);
        r.frame(&[(2, 30.0)], &[ev(SimEvent::EntityHidden { entity: EntityId::from_u64(1) })]);
        r.settle(&[(2, 30.0)]);
        assert_eq!(r.vessel(2).drawn, Some(Lod::L0));
        assert_eq!(r.dev.shown(), 2);
        r.core.free_all(&mut r.dev);
        assert!(r.dev.live.is_empty(), "{:?}", r.dev.live);
    }

    #[test]
    fn splits_wrecks_and_reveals_with_damage() {
        let mut r = Rig::new();
        let d = r.add(hull());
        r.frame(&[(1, 0.0)], &[reveal(1, d.hash)]);
        r.settle(&[(1, 0.0)]);
        // The bow (x >= 32) splits off as wreck piece 9.
        let bow: Vec<CellIndex> = (0..d.grid.cells.len() as u32).filter(|i| i % 40 >= 32).map(CellIndex).collect();
        let split = ev(SimEvent::Phys(PhysEvent::Split { vessel: EntityId::from_u64(1), piece: EntityId::from_u64(9), cells: bow.clone() }));
        r.frame(&[(1, 0.0), (9, 20.0)], &[split]);
        r.settle(&[(1, 0.0), (9, 20.0)]);
        let (parent, piece) = (r.vessel(1), r.vessel(9));
        assert!(parent.chunks[1].empty, "the parent's second chunk is empty now");
        assert!(piece.chunks[0].empty && !piece.chunks[1].empty);
        assert_eq!(r.dev.shown(), 2, "one chunk each");
        // The piece's own reveal replaces the derived one and draws the same thing.
        let removed: Vec<CellIndex> = (0..d.grid.cells.len() as u32).filter(|i| i % 40 < 32).map(CellIndex).collect();
        r.frame(&[(1, 0.0), (9, 20.0)], &[reveal_with(9, d.hash, vec![CellChange { cell: bow[0], bucket: 3 }, CellChange { cell: CellIndex(u32::MAX), bucket: 200 }], removed)]);
        r.settle(&[(1, 0.0), (9, 20.0)]);
        let piece = r.vessel(9);
        assert!(piece.chunks[0].empty && !piece.chunks[1].empty && piece.drawn == Some(Lod::L0));
        assert_eq!(r.dev.shown(), 2);
        // A wreck that is despawned goes; a new sortie clears the rest.
        r.frame(&[(1, 0.0)], &[ev(SimEvent::Phys(PhysEvent::WreckDespawned { wreck: EntityId::from_u64(9) }))]);
        assert_eq!(r.core.vessels.len(), 1);
        r.frame(&[], &[ev(SimEvent::SortieStarted { seed: 1, tick_hz: 60 })]);
        assert!(r.core.vessels.is_empty());
        r.core.free_all(&mut r.dev);
        assert!(r.dev.live.is_empty());
    }

    #[test]
    fn breaches_fire_and_scorch() {
        let mut r = Rig::new();
        // A hollow box: one sealed compartment of air inside steel.
        let mut d = hull();
        let grid = Arc::get_mut(&mut d.grid).unwrap();
        let mut air = Vec::new();
        for y in 1..7 { for z in 1..11 { for x in 1..39 { let p = CellPos::new(x, y, z); grid.set(p, Cell::EMPTY); air.push(grid.index(p).unwrap().0); } } }
        air.sort_unstable();
        d.mesh_cells = d.grid.cells.clone();
        d.sealed_air = vec![0u64; d.grid.cells.len().div_ceil(64)];
        for &i in &air { d.sealed_air[i as usize / 64] |= 1 << (i % 64); }
        d.compartments = vec![(4, air.clone())];
        let d = r.add(d);
        r.frame(&[(1, 0.0)], &[reveal(1, d.hash)]);
        r.settle(&[(1, 0.0)]);
        let triangles = |r: &mut Rig| -> u32 {
            let cells = r.core.vessels[&EntityId::from_u64(1)].cells.clone();
            hull_mesh::chunk::all_chunks(cells.size).into_iter().map(|c| r.mesher.mesh_chunk(&cells.input(), c).unwrap().triangles).sum()
        };
        let sealed = triangles(&mut r);
        let builds = r.chunk_builds();
        // A breach opens the compartment: its air is exposed and both chunks gain their inner faces.
        let breach = ev(SimEvent::Phys(PhysEvent::Breach { vessel: EntityId::from_u64(1), compartment: content::ids::CompartmentId(4), below_waterline: true }));
        r.frame(&[(1, 0.0)], &[breach.clone()]);
        assert_eq!(r.vessel(1).epoch, 1);
        assert!(r.vessel(1).cells.sealed_air.iter().all(|w| *w == 0));
        r.settle(&[(1, 0.0)]);
        assert_eq!(r.chunk_builds(), builds + 2);
        assert!(triangles(&mut r) > sealed, "sealed air has no faces; opened air does");
        let breached = triangles(&mut r);
        // A hull revealed already holed gets no breach event: the hole itself opens the air behind it, so
        // the inside is meshed as it is after a breach plus the faces around the hole.
        let wall = cell(&d, 20, 7, 6);
        r.frame(&[(1, 0.0), (2, 100.0)], &[reveal_with(2, d.hash, Vec::new(), vec![CellIndex(wall)])]);
        let holed = r.core.vessels[&EntityId::from_u64(2)].cells.clone();
        assert!(holed.sealed_air.iter().all(|w| *w == 0), "the pocket behind the hole is open");
        let holed_triangles: u32 = hull_mesh::chunk::all_chunks(holed.size).into_iter().map(|c| r.mesher.mesh_chunk(&holed.input(), c).unwrap().triangles).sum();
        assert!(holed_triangles >= breached, "{holed_triangles} against {breached} after a breach");
        // A piece split off keeps its cells and is open everywhere.
        let bow: Vec<CellIndex> = (0..d.grid.cells.len() as u32).filter(|i| i % 40 >= 32).map(CellIndex).collect();
        let mut fresh = Vessel::new(EntityId::from_u64(3), d.clone(), false, 0);
        fresh.apply(&CellOp::Keep(bow.iter().map(|c| c.0).collect()));
        assert!(fresh.cells.sealed_air.iter().all(|w| *w == 0) && fresh.epoch == 1);
        // Hull 2 goes; what was being built for it is dropped when it lands.
        r.frame(&[(1, 0.0)], &[ev(SimEvent::EntityHidden { entity: EntityId::from_u64(2) })]);
        r.build();
        r.settle(&[(1, 0.0)]);
        // A second breach of the same compartment, or of one that does not exist, changes nothing.
        let other = ev(SimEvent::Phys(PhysEvent::Breach { vessel: EntityId::from_u64(1), compartment: content::ids::CompartmentId(77), below_waterline: false }));
        r.frame(&[(1, 0.0)], &[breach, other]);
        assert_eq!((r.vessel(1).epoch, r.core.outbox.len()), (1, 0));
        // Fire in the compartment: its walls blacken through volume-only updates, at most 4 a second, and
        // the burning chunks glow.
        let fire = ev(SimEvent::Phys(PhysEvent::FireStarted { vessel: EntityId::from_u64(1), compartment: content::ids::CompartmentId(4), spread: false }));
        let volumes = |r: &Rig| r.built.iter().filter(|w| matches!(w, MeshWork::Volumes(_))).count();
        let before = (volumes(&r), r.chunk_builds());
        r.frame(&[(1, 0.0)], &[fire]);
        for _ in 0..60 { r.frame(&[(1, 0.0)], &[]); r.build(); }
        let asked = volumes(&r) - before.0;
        assert!((4..=10).contains(&asked), "two chunks at up to 4 Hz for a second: {asked} volume jobs");
        assert_eq!(r.chunk_builds(), before.1, "soot never remeshes");
        assert!(r.vessel(1).cells.scorch.iter().any(|s| *s > 0));
        assert!(r.dev.heat.values().filter(|h| **h == 1.0).count() == 2);
        let out = ev(SimEvent::Phys(PhysEvent::FireOut { vessel: EntityId::from_u64(1), compartment: content::ids::CompartmentId(4), flooded: false }));
        r.frame(&[(1, 0.0)], &[out]);
        assert!(r.dev.heat.values().all(|h| *h == 0.0));
        // A shell hit scorches the cell and, at half strength, its neighbours; a grounding does not.
        let mut r = Rig::new();
        let d = r.add(hull());
        let c = cell(&d, 20, 7, 6);
        r.frame(&[(1, 0.0)], &[reveal(1, d.hash)]);
        r.frame(&[(1, 0.0)], &[damaged(1, &[c], 4, DamageCause::Shell), damaged(1, &[cell(&d, 5, 0, 5)], 6, DamageCause::Grounding)]);
        let s = &r.vessel(1).cells.scorch;
        assert_eq!((s[c as usize], s[cell(&d, 21, 7, 6) as usize], s[cell(&d, 5, 0, 5) as usize]), (70, 35, 0), "140 at bucket 8: 70 at bucket 4, half beside it");
        assert_eq!(r.vessel(1).epoch, 0, "no cell was removed");
        assert_eq!(scorch_strength(DamageCause::Collision), 0);
    }

    #[test]
    fn the_dock_design_is_drawn_cut_and_edited() {
        let mut r = Rig::new();
        let d = Arc::new(hull());
        r.core.show_design(&mut r.dev, d.clone(), false, None);
        r.settle(&[]);
        let v = &r.core.vessels[&DOCK];
        assert_eq!((v.drawn, v.cells.wear_seed, v.look.weathering), (Some(Lod::L0), 0, 0.0));
        assert_eq!(r.dev.shown(), 2);
        let builds = r.chunk_builds();
        // An edit in the bow remeshes one chunk, and nothing is shared with sortie hulls of that hash.
        let mut edited = hull();
        Arc::get_mut(&mut edited.grid).unwrap().set(CellPos::new(39, 7, 6), Cell::EMPTY);
        edited.mesh_cells = edited.grid.cells.clone();
        r.core.show_design(&mut r.dev, Arc::new(edited), false, None);
        r.settle(&[]);
        assert_eq!(r.chunk_builds(), builds + 1);
        // A deck slice at 2 m hides the cells above it; both chunks change. Picking then sees the cut.
        r.core.show_design(&mut r.dev, d.clone(), true, Some((content::enums::Axis::Y, 2.0)));
        r.settle(&[]);
        assert_eq!(r.chunk_builds(), builds + 3);
        let hit = r.core.raycast(DOCK, [5.25, 10.0, 3.25], [0.0, -1.0, 0.0]).unwrap();
        assert_eq!((hit.0, hit.1), ([10, 3, 6], 3), "the first cell under the cut, entered through its top face");
        assert!((hit.2 - 8.0).abs() < 1e-3);
        // A repainted swatch and a lower waterline with the same cells: materials are re-set in place and
        // the volumes rebuilt for the new waterline row, with no remesh and nothing freed while visible.
        let builds = r.chunk_builds();
        r.core.show_design(&mut r.dev, Arc::new(DrawDesign { palette: vec![[0.1, 0.1, 0.1], [0.3, 0.3, 0.3]], waterline_m: Some(0.3), ..hull() }), false, None);
        r.settle(&[]);
        assert!(r.dev.looks >= 2, "both chunk materials re-set");
        assert_eq!(r.chunk_builds(), builds + 2, "the cut is gone, so both chunks remesh once");
        let (builds, volumes) = (r.chunk_builds(), r.dev.volume_updates);
        r.core.show_design(&mut r.dev, Arc::new(DrawDesign { palette: vec![[0.2, 0.1, 0.1], [0.3, 0.3, 0.3]], waterline_m: Some(1.8), ..hull() }), false, None);
        r.settle(&[]);
        assert_eq!(r.chunk_builds(), builds, "a repaint and a new waterline remesh nothing");
        assert_eq!(r.dev.volume_updates, volumes + 2);
        assert_eq!(r.dev.shown(), 2);
        // A different size replaces the vessel outright.
        r.core.show_design(&mut r.dev, Arc::new(box_design([8, 4, 4])), false, None);
        r.settle(&[]);
        assert_eq!(r.core.vessels[&DOCK].chunks.len(), 1);
        // An empty design draws nothing and asks for nothing.
        r.core.show_design(&mut r.dev, Arc::new(box_design([0, 0, 0])), false, None);
        r.settle(&[]);
        assert_eq!(r.dev.shown(), 0);
        r.core.free_all(&mut r.dev);
        assert!(r.dev.live.is_empty());
    }

    #[test]
    fn picking_and_bounds() {
        let mut r = Rig::new();
        let d = r.add(hull());
        r.frame(&[(1, 100.0)], &[reveal(1, d.hash)]);
        r.frame(&[(1, 100.0)], &[]);
        let e = EntityId::from_u64(1);
        assert_eq!(r.core.bounds(e), Some(([100.0, 0.0, 0.0], [120.0, 4.0, 6.0])));
        // From the port side, level, into the hull's side: the cell at the surface, through its Z- face.
        let hit = r.core.raycast(e, [110.25, 1.25, -50.0], [0.0, 0.0, 1.0]).unwrap();
        assert_eq!((hit.0, hit.1), ([20, 2, 0], 4));
        assert!((hit.2 - 50.0).abs() < 1e-3);
        // A removed cell is passed through to the one behind it.
        r.frame(&[(1, 100.0)], &[damaged(1, &[cell(&d, 20, 2, 0)], 8, DamageCause::Shell)]);
        assert_eq!(r.core.raycast(e, [110.25, 1.25, -50.0], [0.0, 0.0, 1.0]).unwrap().0, [20, 2, 1]);
        // From inside the box, misses, and bad input.
        assert_eq!(r.core.raycast(e, [110.25, 1.25, 3.25], [1.0, 0.0, 0.0]).unwrap().2, 0.0);
        assert!(r.core.raycast(e, [110.0, 50.0, 3.0], [1.0, 0.0, 0.0]).is_none());
        assert!(r.core.raycast(e, [110.0, 50.0, 3.0], [0.0, 1.0, 0.0]).is_none());
        assert!(r.core.raycast(e, [f32::NAN, 0.0, 0.0], [0.0, 0.0, 0.0]).is_none());
        assert!(r.core.raycast(EntityId::from_u64(2), [0.0; 3], [1.0, 0.0, 0.0]).is_none());
        assert!(r.core.bounds(EntityId::from_u64(2)).is_none());
    }

    #[test]
    fn markings_stand_until_their_cell_goes() {
        use content::blueprint::Marking;
        use content::cell::Face;
        let mut d = hull();
        d.markings = vec![
            Marking { kind: MarkingKind::Stripe, cell: CellPos::new(10, 4, 11), face: Face::ZPos, paint: 1, size_cells: 4, text: String::new() },
            Marking { kind: MarkingKind::HullNumber, cell: CellPos::new(30, 4, 11), face: Face::ZPos, paint: 0, size_cells: 2, text: "42".into() },
        ];
        let mut r = Rig::new();
        let d = r.add(d);
        r.frame(&[(1, 0.0)], &[reveal(1, d.hash)]);
        r.settle(&[(1, 0.0)]);
        let v = r.vessel(1);
        assert_eq!(v.markings.len(), 2);
        assert_eq!(r.dev.shown(), 3, "two chunks and the stripe's mesh");
        // The stripe is one quad in its paint, facing out of the starboard side, clockwise seen from outside.
        let mesh = marking_mesh(&v.markings, &d.palette);
        assert_eq!((mesh.positions.len(), mesh.indices.len()), (4, 6), "the hull number is text, not a quad");
        assert_eq!(mesh.colors[0], [0.2, 0.25, 0.3, 1.0]);
        let p = |i: u32| Vec3::from(mesh.positions[i as usize]);
        let winding = (p(mesh.indices[1]) - p(mesh.indices[0])).cross(p(mesh.indices[2]) - p(mesh.indices[0]));
        assert!(winding.dot(Vec3::from(mesh.normals[0])) < 0.0, "clockwise from outside is counter to the normal");
        assert!((mesh.normals[0][2] - 1.0).abs() < 1e-6);
        // The stripe's cell is destroyed: only the number is left, and the stripe's mesh goes.
        r.frame(&[(1, 0.0)], &[damaged(1, &[cell(&d, 10, 4, 11)], 8, DamageCause::Shell)]);
        r.settle(&[(1, 0.0)]);
        let v = r.vessel(1);
        assert_eq!(v.markings.len(), 1);
        assert_eq!(v.markings[0].kind, MarkingKind::HullNumber);
        assert_eq!(r.dev.shown(), 2);
        assert!(marking_mesh(&v.markings, &d.palette).is_empty());
    }

    #[test]
    fn masks_take_stable_layers_and_follow_the_hull() {
        let mut r = Rig::new();
        let d = r.add(hull());
        let events: Vec<WireEvent> = (1..=10).map(|i| reveal(i, d.hash)).collect();
        // Ten hulls 40 m apart; the camera sits by the first. Only the nearest eight are masked.
        let poses: Vec<(u64, f32)> = (1..=10).map(|i| (i, (i - 1) as f32 * 40.0)).collect();
        r.frame(&poses, &events);
        r.settle(&poses);
        let mut out = super::super::HullMasks::default();
        let camera = Vec3::new(0.0, 10.0, -30.0);
        r.core.masks(&mut r.dev, camera, &mut out);
        assert_eq!(out.count, 0, "layers are seated but no mask is built yet");
        assert!(r.core.mask_layers.iter().all(Option::is_some));
        assert!(!r.core.mask_layers.contains(&Some(EntityId::from_u64(10))));
        // Masks build on the thread, at most one layer uploads per call.
        for _ in 0..40 { r.frame(&poses, &[]); r.build(); r.core.masks(&mut r.dev, camera, &mut out); }
        assert_eq!(out.count, 8);
        assert_eq!(r.dev.mask_layers.len(), 8);
        assert_eq!(out.texture, Rid::new(2_000_000));
        // Layer of hull 2 (40 m along X): a point 3 m in from its origin and 5 m to starboard maps to mask
        // metres with the centreline offset folded in (12 cells of beam: offset 3 m).
        let layer = r.core.mask_layers.iter().position(|l| *l == Some(EntityId::from_u64(2))).unwrap();
        let m = glam::Mat4::from_cols_array(&out.world_to_hull[layer]);
        let p = m.transform_point3(Vec3::new(43.0, 1.5, 5.0));
        assert!((p - Vec3::new(3.0, 1.5, 2.0)).length() < 1e-4, "{p}");
        // Damage rebuilds that hull's layer only, and not within 100 ms of the last build.
        let uploads = r.dev.mask_layers.len();
        r.frame(&poses, &[damaged(2, &[cell(&d, 5, 0, 5)], 8, DamageCause::Torpedo)]);
        for _ in 0..20 { r.frame(&poses, &[]); r.build(); r.core.masks(&mut r.dev, camera, &mut out); }
        assert_eq!(r.dev.mask_layers.len(), uploads + 1);
        assert_eq!(*r.dev.mask_layers.last().unwrap() as usize, layer);
        // Hull 1 leaves: its layer frees and hull 9 takes it; the others keep theirs.
        let before = r.core.mask_layers;
        let poses: Vec<(u64, f32)> = poses[1..].to_vec();
        r.frame(&poses, &[ev(SimEvent::EntityHidden { entity: EntityId::from_u64(1) })]);
        r.core.masks(&mut r.dev, camera, &mut out);
        let after = r.core.mask_layers;
        let freed = before.iter().position(|l| *l == Some(EntityId::from_u64(1))).unwrap();
        assert_eq!(after[freed], Some(EntityId::from_u64(9)));
        assert!((0..8).all(|i| i == freed || before[i] == after[i]));
        // Nobody near: no layers, no matrices.
        r.core.masks(&mut r.dev, Vec3::new(0.0, 0.0, 50_000.0), &mut out);
        assert_eq!((out.count, out.world_to_hull.len()), (0, 0));
    }

    #[test]
    fn far_chunks_are_evicted_and_come_back() {
        let mut r = Rig::new();
        let d = r.add(hull());
        r.frame(&[(1, 0.0)], &[reveal(1, d.hash)]);
        r.settle(&[(1, 0.0)]);
        r.settle(&[(1, 3_000.0)]);
        assert_eq!(r.vessel(1).drawn, Some(Lod::L3));
        assert_eq!(r.dev.count("volume"), 2, "LOD0 is kept for a while after the hull leaves it");
        r.time_s += CHUNK_EVICT_S + 1.0;
        for _ in 0..10 { r.frame(&[(1, 3_000.0)], &[]); }
        assert_eq!((r.dev.count("volume"), r.dev.count("material")), (0, 0));
        assert_eq!(r.dev.shown(), 1);
        // Back in close: the shared meshes are still cached, so only the volumes are built again.
        let builds = r.chunk_builds();
        r.settle(&[(1, 0.0)]);
        assert_eq!((r.vessel(1).drawn, r.chunk_builds()), (Some(Lod::L0), builds));
        assert_eq!(r.dev.shown(), 2);
    }

    #[test]
    fn a_refused_grid_fails_once_and_quietly() {
        let mut r = Rig::new();
        // A palette of 17 entries is refused by hull-mesh; the design view never produces one, so force it.
        let mut bad = hull();
        bad.palette = vec![[0.1; 3]; 17];
        let d = r.add(bad);
        r.frame(&[(1, 0.0)], &[reveal(1, d.hash)]);
        for _ in 0..5 { r.frame(&[(1, 0.0)], &[]); r.build(); }
        assert_eq!(r.dev.warnings.len(), 1, "{:?}", r.dev.warnings);
        assert!(r.vessel(1).failed);
        assert_eq!((r.dev.shown(), r.core.outbox.len(), r.core.pending_uploads()), (0, 0, 0));
        // Events for entities nobody revealed are ignored.
        r.frame(&[(1, 0.0)], &[damaged(55, &[1], 8, DamageCause::Shell), ev(SimEvent::EntityHidden { entity: EntityId::from_u64(56) })]);
    }

    #[test]
    fn the_own_hull_is_built_first() {
        let mut r = Rig::new();
        let d = r.add(box_design([200, 8, 12]));
        let other = r.add(DrawDesign { hash: ContentHash([9; 32]), ..box_design([200, 8, 12]) });
        // Two hulls of different designs at the same distance: the viewer's own (x4) has its seven chunks
        // sent first, and the other waits, because the pieces in flight already exceed its share.
        r.frame(&[(1, 300.0), (2, 300.0)], &[reveal(1, d.hash), reveal(2, other.hash)]);
        r.frame(&[(1, 300.0), (2, 300.0)], &[]);
        let own = EntityId::from_u64(1);
        assert_eq!(r.core.outbox.len(), 7);
        assert!(r.core.outbox.iter().all(|j| j.vessel == own));
        assert!(r.core.outbox.len() <= IN_FLIGHT_PRIORITY as usize);
        // Once those are built the other hull gets its share of the thread, and no more.
        r.build();
        r.frame(&[(1, 300.0), (2, 300.0)], &[]);
        assert_eq!(r.core.outbox.len(), IN_FLIGHT_OTHER as usize);
        assert!(r.core.outbox.iter().all(|j| j.vessel == EntityId::from_u64(2)));
        // Screen area: nearer is larger; the own hull's factor is 4.
        assert!(screen_area(10.0, 100.0) > screen_area(10.0, 200.0));
        assert_eq!(screen_area(10.0, 0.0), 100.0);
    }
}
