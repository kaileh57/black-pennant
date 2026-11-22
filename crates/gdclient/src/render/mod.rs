//! Contract-owned (no work unit). Shared types of the 3D renderer and the list of its units.
//! DESIGN.md "Graphics and rendering" (L1768-2061), L1088 (one transform per hull chunk; one
//! `multimesh_set_buffer` per part type, projectile type and VFX mesh type per frame), L2002-2010 (the 2 ms
//! main-thread split), L2012 (adapter fallbacks).
//!
//! Each unit below is a plain Rust struct owned by `BpWorldView` (or `BpEditorView`). It creates its
//! `RenderingServer` RIDs in `new`, frees them in `free`, and is driven once per frame through `update`.
//! Units never hold Godot nodes, never call each other, and read only the [`FrameContext`] they are given.
//! All of this runs on the main thread; only [`hull`] has a thread of its own, for meshing.
//!
//! | Unit | File | Draws | Budget per frame (L2004-2009) |
//! | `gd-render-hull` | `hull.rs` | hull chunk meshes, volumes, LODs, hull masks | transforms 0.15 ms; uploads and frees 0.3 ms (hard timer) |
//! | `gd-render-parts` | `parts.rs` | part MultiMeshes, turrets | 0.3 ms |
//! | `gd-render-projectiles` | `projectiles.rs` | shells, tracers, missiles, decoys | 0.4 ms shared with VFX layer A |
//! | `gd-render-vfx` | `vfx.rs` | VFX layer A MultiMeshes, wakes, trails, VFX lights | shared 0.4 ms |
//! | `gd-render-particles` | `particles.rs` | VFX layer B GPU particles | within the 0.1 ms "emits" share |
//! | `gd-render-ocean` | `ocean.rs` | clipmap, wave uniforms, foam inputs | 0.1 ms with LUT and emits |
//! | `gd-render-scenery` | `scenery.rs` | islands, clouds, environment state, LUT | on change |
//!
//! Render layers (set in `project.godot` layer names): 1 hulls, 2 parts, 3 ocean, 4 VFX (excluded from the
//! shadow caster mask, L1883), 5 scenery, 6 dry-dock overlays.
//!
//! # What comes from `hull-mesh`
//! Every mesh, every instance layout and every cap is produced by the Godot-free `hull-mesh` crate; the units
//! here only upload and place. Nothing in this module re-implements or mirrors a `hull-mesh` type.
//!
//! | Unit | Calls | Uploads through |
//! | `gd-render-hull` | `pipeline::ChunkMesher`, `chunk::dirty_chunks`, `lod::{mesh_lod, pick_lod}`, `surface::encode_colored_surface`, `mask::build_hull_mask`, `pipeline::marking_quads` | `convert::surface_dict` |
//! | `gd-render-parts` | `parts::build_part_model` | `convert::mesh_arrays` |
//! | `gd-render-projectiles` | `parts::build_body_model`, `vfx::runtime::{RingPool, tracer_instance}` | `convert::mesh_arrays` |
//! | `gd-render-vfx` | `vfx::meshes::base_mesh`, `vfx::runtime::*`, `vfx::{VfxKind, CapGroup, RampId, emission}` | `convert::mesh_arrays`, `convert::tex_blob_images` |
//! | `gd-render-particles` | `vfx::layer_b_particle_cap` | none |
//! | `gd-render-ocean` | `env::ocean::{ClipmapSpec, level_mesh, skirt_mesh, snap_origins}` | `convert::mesh_arrays`, `convert::tex_blob_images` |
//! | `gd-render-scenery` | `env::scenery::{cloud_layout, cloud_cluster, island_mesh}` | `convert::mesh_arrays`, `convert::tex_blob_images` |
//!
//! # Vertex formats (what each shader may read)
//! | Shader | Mesh | `VERTEX` | `NORMAL` | `COLOR` | `CUSTOM0` | Per instance |
//! | `hull.gdshader`, volume path | LOD0 chunk (`format::CHUNK_SURFACE`) | vessel-local metres | flat, octahedral | absent | absent | per-chunk material: the 34 cubed RGBA8 volume, chunk origin, blueprint palette (16 linear RGB), wear and weathering |
//! | `hull.gdshader`, vertex-colour path | LOD1 to LOD4, one mesh per vessel (`format::COLORED_SURFACE`) | vessel-local metres | flat, octahedral | RGB = swatch times AO, linear; A = 1 | absent | none: no volume is bound and no texture is fetched |
//! | none (depth only) | LOD0 shadow mesh (`format::SHADOW_SURFACE`) | welded positions | absent | absent | absent | none |
//! | `part.gdshader` | part and body pieces (`MeshData` arrays) | piece-local metres about the pivot | flat | RGB = linear albedo (`parts::Tone`), A = paint mask | x = emission strength (lamps at most 1), y = 1 on glass | MultiMesh colour = white (1, 1, 1, 1); custom.x = scorch 0..=1 (1 on a destroyed part, L1651), custom.yzw = the ship paint the part wears (linear RGB; see `parts`), multiplied in where the paint mask is 1. Godot multiplies the instance colour into `COLOR` before the shader runs, so paint carried in the colour would also tint the fixed-albedo (mask 0) surfaces; with custom.yzw all zero the shader falls back to vertex colour times instance colour |
//! | `vfx_opaque.gdshader`, `vfx_blend.gdshader` | `vfx::meshes::base_mesh` | unit-sized | flat | white | per-vertex motion term | the 80 B `vfx::VfxInstance`: colour, then custom.x = age s, y = lifetime s, z = seed, w = kind-specific |
//! | `water.gdshader` | `env::ocean::level_mesh`, `skirt_mesh` | level-local metres, y = 0 | up | absent | x = level spacing, y = morph weight, z = level, w = 1 on the skirt | none |
//! | `cloud.gdshader` | `env::scenery::cloud_cluster` | cluster-local metres | flat | white | x = wrap-lighting term | none |
//! | `island.gdshader` | `env::scenery::island_mesh` | metres from the mesh origin | flat | height-band colour, linear | absent | none |
//!
//! Hull LOD0 has no vertex colour by design (L1927): a shader that reads `COLOR` on the volume path reads
//! white. The two hull paths are one shader file with a material flag (`lod_vertex_colour`), so the pick
//! between them is a material swap, never a shader compile.
//!
//! # Generated textures
//! Paths are the constants of [`crate::seams::gen`]; files are `TexBlob` `.bin`, read with
//! `convert::load_tex_blob`.
//! | Unit | Files | Becomes |
//! | `gd-render-ocean` | `OCEAN_DETAIL_NORMALS` (BC5), `OCEAN_DETAIL_TURBULENCE` (BC4): 64 layers of 256 squared with mips | two `Texture2DArray` parameters of the water material |
//! | `gd-render-ocean` | `NOISE_CLOUD_SHADOW` (R8 512 squared) | the global shader parameter `bp_cloud_shadow` (water and hull shaders, L1905) |
//! | `gd-render-hull` | `NOISE_WEATHERING` (R8 256 x 32) | a parameter of the hull shader (rust bleed, L1940) |
//! | `gd-render-vfx` | `ramp_path(RampId::name())` for each of `RampId::ALL` (RGBA8 256 x 1) | one ramp parameter per VFX material |
//! | `gd-render-scenery` | `lut_path(weather, time, high)` (RGB8 17 cubed, 33 cubed on High) | the blended `ImageTexture3D` of [`scenery::SceneryRenderer::lut`] |
pub mod hull;
pub mod ocean;
pub mod particles;
pub mod parts;
pub mod projectiles;
pub mod scenery;
pub mod vfx;

use crate::enums::QualityTier;
use crate::interp::{CameraState, ViewFrame};
use crate::seams::WireEvent;
use godot::prelude::Rid;

pub const LAYER_HULL: u32 = 1;
pub const LAYER_PARTS: u32 = 1 << 1;
pub const LAYER_OCEAN: u32 = 1 << 2;
pub const LAYER_VFX: u32 = 1 << 3;
pub const LAYER_SCENERY: u32 = 1 << 4;
pub const LAYER_DOCK_OVERLAY: u32 = 1 << 5;

/// Bytes and floats per MultiMesh instance with colour and custom data: 12 floats transform, 4 colour, 4
/// custom (L1949). The layout is `hull_mesh::vfx::VfxInstance`; part and projectile buffers use the same one.
pub use hull_mesh::vfx::{INSTANCE_BYTES, INSTANCE_FLOATS};

/// Live caps are not a gdclient table: ask `hull_mesh::vfx::CapGroup::cap(tier)` per cap group,
/// `hull_mesh::vfx::layer_b_particle_cap(tier)` for layer B, `hull_mesh::vfx::vfx_light_budget(tier)` for
/// lights (total, of which fires) and `hull_mesh::vfx::runtime::trail_length_s(tier)` for trails (L1972).
/// High uses the Medium caps and Fallback the Low caps. The splash cap counts instances at 5 per splash.
pub use hull_mesh::vfx::{layer_b_particle_cap, vfx_light_budget, CapGroup};

/// What every render unit may read in one frame.
pub struct FrameContext<'a> {
    pub view: &'a ViewFrame,
    /// Events that arrived this frame (entity reveals, physical events, weather), in order
    /// (`FrameFeed::events`). Client-side events carry no audience: they are already this viewer's.
    pub events: &'a [WireEvent],
    pub camera: CameraState,
    pub delta_s: f64,
    /// `World3D` scenario RID the instances live in.
    pub scenario: Rid,
    pub quality: &'a crate::quality::QualityProfile,
    /// Degrade step 0..=6 of rung 1 (L2026-2031) chosen by `BpQuality`.
    pub degrade_step: u8,
    pub reduce_flashing: bool,
}

/// Main-thread time left for uploads this frame; units with a hard timer stop and defer when it runs out
/// (mesh and volume uploads and frees: 0.3 ms, L2008).
pub struct UploadBudget { pub(crate) deadline: std::time::Instant }
impl UploadBudget {
    pub fn new(micros: u64) -> Self { Self { deadline: std::time::Instant::now() + std::time::Duration::from_micros(micros) } }
    pub fn spent(&self) -> bool { std::time::Instant::now() >= self.deadline }
}

/// Counters for the performance HUD, the benchmark and the nightly primitive check (L2039).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RenderStats { pub chunks_drawn: u32, pub chunk_uploads_pending: u32, pub part_instances: u32, pub projectile_instances: u32, pub vfx_instances: u32, pub particles: u32, pub lights: u32, pub adapter_us: u32 }
