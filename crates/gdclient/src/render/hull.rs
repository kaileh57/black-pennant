//! Unit `gd-render-hull` (hard: threading, RID lifetime, upload budget). Hull meshes for every drawn vessel
//! and wreck, and the meshing thread that builds them.
//! DESIGN.md L1919-1945 (meshed 16 m chunks, shapes, class buffer, surface bytes, rebuild by swap, per-chunk
//! volume, LOD choice, shared meshes, meshing priority), L1909 (hull masks for the water), L945 (received
//! blueprints upload in slices of at most 0.5 ms per frame), L2008 (0.3 ms hard timer), L2043 (S5 gate:
//! 200 us per chunk).
//!
//! All meshing is the `hull-mesh` crate's; this unit owns the client-side cell copies, the thread, the RIDs
//! and the upload budget. It defines no mesh type of its own.
//!
//! # Two ways a hull is drawn (L1943)
//! - LOD0 (0.5 m cells): one mesh instance per 16 m render chunk, each a
//!   [`ChunkMesh`](crate::seams::ChunkMesh): a `format::CHUNK_SURFACE` surface (position, normal, index; no
//!   vertex colour), a welded shadow mesh and a 34 cubed RGBA8 volume in a per-chunk `ShaderMaterial` on the
//!   one shared hull shader (L1931). All chunks of a vessel are in vessel-local metres and share its
//!   transform, so a moving ship costs one `instance_set_transform` per chunk (L1088).
//! - LOD1 to LOD4 (1, 2, 4, 8 m cells): one mesh per vessel per LOD, not per chunk:
//!   `hull_mesh::lod::mesh_lod(input, lod)` packed with `hull_mesh::surface::encode_colored_surface`
//!   (`format::COLORED_SURFACE`, vertex colour = swatch times AO). One instance and one transform per
//!   vessel; no volume and no per-chunk material. There is no separate shadow mesh: the LOD surface casts
//!   with its own triangles, which only matters where a LOD is inside the shadow distance (High, 800 m,
//!   against LOD1 from 623 m at 1080p). Drawn with the hull shader's vertex-colour path (material flag
//!   `lod_vertex_colour`).
//!
//! Per vessel per frame the unit asks `hull_mesh::lod::pick_lod(distance, fov_y_rad, viewport_h_px, tier,
//! current)`, which holds the 1.5 px rule (3.0 on Low) and the 15% hysteresis. On a change it shows the
//! instances of the new LOD and hides the others in the same frame: no fades (L1943). A LOD mesh is built the
//! first time its LOD is picked and again after damage; until it arrives the previous LOD stays visible.
//!
//! # Flow
//! `SimEvent::EntityRevealed` gives the blueprint hash, damage so far and removed cells; the unit keeps one
//! [`HullCells`] per drawn vessel, applies `PhysEvent` cell changes, splits and compartment breaches to it,
//! finds what to rebuild with `hull_mesh::chunk::dirty_chunks` and `dirty_chunks_for_cells`, and queues
//! [`MeshJob`]s (priority by screen area, x4 own hull, x2 its target; up to 8 remeshes per frame for those).
//! The thread runs one `hull_mesh::pipeline::ChunkMesher` and returns [`MeshDone`]s. The main thread uploads
//! them under the budget: a new mesh RID from `mesh_create_from_surfaces` (`convert::surface_dict`), the
//! shadow mesh, the volume (`texture_3d_create` or `texture_3d_update`), then `instance_set_base`; old RIDs
//! are freed 2 frames later, at most 4 per frame. `mesh_clear` is never called. An empty `ChunkMesh`
//! (`empty == true`) frees the chunk's RIDs and draws nothing. Holes show within 2 frames inside 1 km and
//! within 10 beyond.
//!
//! Markings (L130, L1801): `hull_mesh::pipeline::marking_quads(markings, grid)` places each stripe, flag and
//! hull number on its cell face and drops those whose cell is gone. Stripes and flags are drawn here as flat
//! quads in the marking's paint on the vertex-colour path; hull numbers are in-engine MSDF text, which needs
//! a font, so their quads are handed out through [`HullRenderer::marking_quads`] for the view node to place.
//!
//! Undamaged ships of one blueprint share meshes through a cache keyed by `hull_mesh::ChunkKey`
//! (`damage_epoch == 0`); a vessel LOD mesh uses the key with `coord` (0, 0, 0) and its `lod`. Volumes are
//! per vessel (wear seed) and never shared.
//!
//! # How the unit is built
//! - [`core`] holds every decision with no Godot in it (cells, damage, epochs, requests, the shared cache,
//!   the retire queue, masks, picking) and reaches the engine only through `core::Device`; its tests run
//!   the whole state machine against a counting fake. [`gpu`] is that device on `RenderingServer`, plus
//!   the MultiMesh and mesh helpers the part and projectile units share. [`thread`] is the meshing thread.
//!   [`design`] is the one shape a design is read in, whatever it came from.
//! - A reveal names its design by hash. The unit looks the hash up in `Services::designs` every frame until
//!   it is there (`design::lookup`), so a design that streams in after the reveal is drawn when it lands,
//!   with the damage that arrived meanwhile.
//! - The cells meshed are the design grid without the part cells that stand in the open (`design`): parts
//!   are drawn by `gd-render-parts`, and a footprint meshed as a box would hide its own model. Picking,
//!   markings and the hull mask read the unchanged grid.
//! - A LOD the vessel is not yet drawn at is switched to only when all of it is built from the current
//!   cells; a LOD already on screen swaps piece by piece as rebuilds arrive.
//! - Scorch is the client's own: cells blacken by what damaged them (blast and fire, not collisions), and
//!   the walls of a burning compartment blacken while it burns, through volume-only updates
//!   ([`MeshWork::Volumes`]) at up to 4 Hz. An exterior-only form has no compartments, so it scorches from
//!   damage alone.
//! - An exterior-only form does not say where its waterline is; the hull's first pose does.
//! - LOD0 chunks of a hull not drawn at LOD0 for 20 s are released; meshes of an intact design stay cached
//!   for 10 s after the last hull of the class leaves.
//! - Damage is drawn at the playback tick ([`hold`]): cell damage, debris, breaches, splits and fires wait
//!   until playback is two ticks short of the event, which is the time a remesh takes to reach the screen,
//!   so a hole opens as the shell lands and the burst shows, not the interpolation delay before.
//! - If the meshing thread cannot start or dies, jobs are built on the main thread, one per frame.
//! - `res://test/tools/render_check.gd` runs this unit and its two siblings inside the engine
//!   ([`check`]).
pub(crate) mod check;
pub(crate) mod core;
pub(crate) mod design;
pub(crate) mod gpu;
pub(crate) mod hold;
pub(crate) mod thread;

use super::{FrameContext, RenderStats, UploadBudget};
use crate::seams::{ChunkMesh, Lod, MeshError, SurfaceBytes};
use content::cell::{Cell, CellGridView, RenderChunkCoord};
use content::ids::ContentHash;
use hull_mesh::chunk::MeshInput;
use hull_mesh::mask::HullMask;
use sim_core::EntityId;
use std::sync::Arc;

/// Render chunk edge in cells and metres (L1114, L1925): `hull_mesh::CHUNK` cells of `hull_mesh::CELL_M_F32`.
pub const CHUNK_CELLS: usize = hull_mesh::CHUNK;
pub const CHUNK_M: f32 = hull_mesh::CHUNK as f32 * hull_mesh::CELL_M_F32;

/// The client's copy of one drawn vessel's cells, in the form `hull-mesh` reads. The main thread owns the
/// current copy and hands the meshing thread an `Arc` per job; applying damage clones on write
/// (`Arc::make_mut`), so a job always meshes one consistent state.
#[derive(Clone, Debug, Default)]
pub struct HullCells {
    pub size: [u16; 3],
    pub cells: Vec<Cell>,
    /// Damage bucket 0..=8 per cell (8 = removed); empty = intact.
    pub damage: Vec<u8>,
    /// Sealed-air bits (`CellGridView::sealed_air`), cleared as compartments are breached or entered.
    /// Empty for editor designs and for exterior-only PvP forms.
    pub sealed_air: Vec<u64>,
    /// Scorch 0..=255 per cell for the volume's B channel; empty = none.
    pub scorch: Vec<u8>,
    /// Blueprint paint table in linear RGB, at most 16 entries.
    pub palette: Vec<[f32; 3]>,
    /// Per-vessel weathering seed; 0 in the dry dock.
    pub wear_seed: u64,
    pub design_waterline_y: i16,
}
impl HullCells {
    /// The view `hull-mesh` takes. Borrowing only: no copy.
    pub fn input(&self) -> MeshInput<'_> {
        MeshInput {
            view: CellGridView { size: self.size, cells: &self.cells, damage: &self.damage, sealed_air: &self.sealed_air, scorch: &self.scorch },
            palette: &self.palette,
            wear_seed: self.wear_seed,
            design_waterline_y: self.design_waterline_y,
        }
    }
}

/// What a job builds.
#[derive(Clone, Debug, PartialEq)]
pub enum MeshWork {
    /// LOD0 chunks: `ChunkMesher::mesh_chunk` for each coordinate, one [`MeshOutput::Chunk`] each.
    Chunks(Vec<RenderChunkCoord>),
    /// One whole-vessel LOD, `Lod::L1` to `Lod::L4`: `lod::mesh_lod` then `surface::encode_colored_surface`.
    /// `Lod::L0` is a caller error.
    Lod(Lod),
    /// `mask::build_hull_mask`, for the water's hull masking.
    Mask,
    /// Volumes only, `chunk::build_volume` for each coordinate, one [`MeshOutput::Volume`] each: a chunk
    /// whose mesh comes from the shared cache still needs its own volume (wear seed), and a scorch-only
    /// change re-uploads the volume without remeshing (L1931, up to 4 Hz).
    Volumes(Vec<RenderChunkCoord>),
}

/// A request to the meshing thread. Jobs for one vessel with an older `generation` than the newest queued
/// one for the same work are dropped unbuilt.
pub struct MeshJob { pub vessel: EntityId, pub blueprint: ContentHash, pub generation: u32, pub priority: f32, pub cells: Arc<HullCells>, pub work: MeshWork }

/// One finished piece of a job.
pub enum MeshOutput {
    /// Upload the surface, shadow mesh and volume of `ChunkMesh::coord` together (L1931).
    Chunk(Box<ChunkMesh>),
    /// The whole vessel at `lod` as one coloured surface.
    Lod { lod: Lod, surface: SurfaceBytes },
    Mask(Box<HullMask>),
    /// One chunk's volume alone; the chunk keeps its mesh.
    Volume { coord: RenderChunkCoord, volume: Box<crate::seams::VolumeTexture> },
    /// The grid was rejected by `hull-mesh` (a caller error; logged once, the vessel keeps its old meshes).
    Failed(MeshError),
}

pub struct MeshDone { pub vessel: EntityId, pub generation: u32, pub output: MeshOutput, pub micros: u32 }

/// The id the dry dock's design is drawn under ([`HullRenderer::show_design`]). `EntityId::NONE` is accepted
/// in its place by the queries below.
pub const DOCK_VESSEL: EntityId = core::DOCK;

pub struct HullRenderer {
    pub(crate) jobs: Option<std::sync::mpsc::Sender<MeshJob>>,
    pub(crate) done: Option<std::sync::mpsc::Receiver<MeshDone>>,
    pub(crate) thread: Option<std::thread::JoinHandle<()>>,
    pub(crate) stats: RenderStats,
    pub(crate) core: core::HullCore,
    pub(crate) device: Option<gpu::GodotDevice>,
    /// Without a meshing thread (it could not start, or died) jobs are built here, one per frame.
    pub(crate) backlog: std::collections::VecDeque<MeshJob>,
    pub(crate) fallback: Option<hull_mesh::pipeline::ChunkMesher>,
    pub(crate) masks: HullMasks,
}

fn key(vessel: EntityId) -> EntityId { if vessel.is_none() { DOCK_VESSEL } else { vessel } }

impl HullRenderer {
    /// Create the two hull materials' shared shader hookup (volume path and vertex-colour path), load
    /// `gen::NOISE_WEATHERING`, and start the meshing thread (named `bp-mesh`) with its `ChunkMesher`.
    pub fn new(scenario: godot::prelude::Rid) -> Self {
        let (jobs, done, thread) = match thread::spawn() {
            Some((jobs, done, handle)) => (Some(jobs), Some(done), Some(handle)),
            None => { godot::global::godot_warn!("hull renderer: the meshing thread could not start; meshing on the main thread"); (None, None, None) }
        };
        Self { jobs, done, thread, stats: RenderStats::default(), core: core::HullCore::new(), device: Some(gpu::GodotDevice::new(scenario)), backlog: Default::default(), fallback: None, masks: HullMasks::default() }
    }
    /// Per frame: handle reveals, hides, damage and splits; pick each vessel's LOD; set one transform per
    /// visible chunk instance (LOD0) or one per vessel (LOD1 to LOD4); upload finished builds within
    /// `budget`; free retired RIDs.
    pub fn update(&mut self, ctx: &FrameContext, budget: &mut UploadBudget) {
        let Some(device) = &mut self.device else { return };
        if let Some(done) = &self.done { while let Ok(piece) = done.try_recv() { self.core.receive(piece); } }
        // A new sortie brings new designs: converted forms of the last one are dropped with its vessels.
        if ctx.events.iter().any(|e| matches!(e.event, sim_systems::protocol::SimEvent::SortieStarted { .. })) { design::forget_all(); }
        let camera = ctx.camera;
        let lens = core::HullLens {
            camera: gpu::camera_render_pos(ctx),
            fov_y_rad: if camera.valid { (camera.fov_y_deg as f64).to_radians() } else { f64::NAN },
            // The pixel rule (L1943) counts rendered pixels: the 3D resolution scale, not the dynamic step,
            // which moves too often to hang a LOD on.
            viewport_h_px: camera.viewport_px[1] as f64 * (ctx.quality.base_scale as f64).clamp(0.25, 2.0),
            tier: ctx.quality.tier,
            degrade_step: ctx.degrade_step,
            delta_s: ctx.delta_s,
        };
        self.core.frame(device, ctx.view, ctx.events, lens, &mut || budget.spent(), &|hash| design::lookup(hash));
        let mut lost = false;
        for job in self.core.outbox.drain(..) {
            // Without a thread (it never started, or its end of the channel is gone) the job is built here.
            let unsent = match &self.jobs {
                Some(tx) if !lost => tx.send(job).err().map(|e| e.0),
                _ => Some(job),
            };
            if let Some(job) = unsent { lost = true; self.backlog.push_back(job); }
        }
        if lost { self.jobs = None; }
        if self.jobs.is_none() {
            if let Some(job) = self.backlog.pop_front() {
                let mesher = self.fallback.get_or_insert_with(hull_mesh::pipeline::ChunkMesher::new);
                let core = &mut self.core;
                thread::process(mesher, &job, &mut |piece| core.receive(piece));
            }
        }
        self.stats = RenderStats { chunks_drawn: self.core.chunks_drawn, chunk_uploads_pending: self.core.pending_uploads(), ..RenderStats::default() };
    }
    /// Up to `hull_mesh::mask::MAX_MASKED_HULLS` hulls within `MASK_RANGE_M` of the camera for the water's
    /// hull mask: world-to-hull matrices and the 512 x 128 RGBA16F layers (`HullMask::texels` are raw half
    /// bits: dry-column bottom, top, waterline SDF, presence), rebuilt at most every 100 ms (L1909).
    ///
    /// `world_to_hull[i]` belongs to layer `i` and is column-major: it maps a render-space position to mask
    /// metres, `x = 0.5 m x texel u`, `z = 0.5 m x (texel v - 64)` (the centreline offset of
    /// `HullMask::offset_z_m` is folded in) and `y` = vessel-local height, which is what the R and G
    /// channels hold. `count` is one past the highest layer in use; a layer below it that holds no hull has
    /// a matrix that maps every point outside the mask. At most one layer is uploaded per call
    /// (`dirty_layers`); a hull's layer counts only once its mask is on the GPU.
    pub fn hull_masks(&mut self, ctx: &FrameContext) -> HullMasks {
        let Some(device) = &mut self.device else { return HullMasks::default() };
        self.core.masks(device, gpu::camera_render_pos(ctx), &mut self.masks);
        self.masks.clone()
    }
    /// Draw one design at the origin for the dry dock or a thumbnail, replacing any previous one, always at
    /// LOD0 with wear seed 0. `xray` and `cut` drive the editor's X-ray and deck-slice views.
    ///
    /// `cut` hides every cell whose centre lies past the plane, in vessel-local metres along the axis (for
    /// `Axis::Y`, everything above a deck height), and the cut faces show primer. `xray` dims the hull so
    /// the overlays read on top. The design is drawn by the following `update` calls under [`DOCK_VESSEL`];
    /// a call for an edited design of the same size remeshes only the chunks whose cells changed.
    pub fn show_design(&mut self, design: &sim_systems::ship::ShipDesign, blueprint: &content::blueprint::Blueprint, xray: bool, cut: Option<(content::enums::Axis, f32)>) {
        let Some(device) = &mut self.device else { return };
        let drawn = design::DrawDesign::from_blueprint(blueprint, Some(design), content::catalogue::catalogue(), design.hash, true);
        self.core.show_design(device, Arc::new(drawn), xray, cut);
    }
    /// Bounding box of a drawn vessel in render space, for picking and framing.
    pub fn bounds(&self, vessel: EntityId) -> Option<([f32; 3], [f32; 3])> { self.core.bounds(key(vessel)) }
    /// Ray against the client-side cell grid of a drawn vessel: (cell, face index, distance).
    ///
    /// `origin` and `dir` are in render space. The face is the one the ray enters through, as an index into
    /// `content::cell::Face::ALL`; the distance is in metres along the ray. Part cells count (the caller maps
    /// a cell to its part), removed cells do not, and shaped cells are tested as full cells.
    pub fn raycast(&self, vessel: EntityId, origin: [f32; 3], dir: [f32; 3]) -> Option<([i16; 3], u8, f32)> { self.core.raycast(key(vessel), origin, dir) }
    /// Current marking quads of a drawn vessel in vessel-local metres (recomputed when its cells change);
    /// the caller draws the `MarkingKind::HullNumber` ones as text and applies the vessel transform.
    pub fn marking_quads(&self, vessel: EntityId) -> &[hull_mesh::pipeline::MarkingQuad] { self.core.vessels.get(&key(vessel)).map_or(&[], |v| v.markings.as_slice()) }
    /// The LOD a drawn vessel is shown at this frame (performance overlay, tests).
    pub fn lod(&self, vessel: EntityId) -> Option<Lod> { self.core.vessels.get(&key(vessel)).filter(|v| v.posed).and_then(|v| v.drawn) }
    pub fn stats(&self) -> RenderStats { self.stats }
    /// Stop the meshing thread and free every RID. Idempotent.
    pub fn free(&mut self) {
        // Dropping the sender ends the thread's loop; it finishes the job in hand first.
        self.jobs = None;
        if let Some(handle) = self.thread.take() { let _ = handle.join(); }
        self.done = None;
        self.backlog.clear();
        self.fallback = None;
        let Some(mut device) = self.device.take() else { return };
        self.core.free_all(&mut device);
        device.free();
        self.masks = HullMasks::default();
        self.stats = RenderStats::default();
    }
}

/// Inputs of the water shader's hull masking (L1909).
#[derive(Clone, Debug)]
pub struct HullMasks { pub count: u32, pub world_to_hull: Vec<[f32; 16]>, pub texture: godot::prelude::Rid, pub dirty_layers: Vec<u32> }
impl Default for HullMasks { fn default() -> Self { Self { count: 0, world_to_hull: Vec::new(), texture: godot::prelude::Rid::Invalid, dirty_layers: Vec::new() } } }
