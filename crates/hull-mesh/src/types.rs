//! Contract-owned (no work unit). Shared output types of every hull-mesh unit: plain Vecs, no Godot types.
//! DESIGN.md L1919-1945 (ship meshing and materials), L1977-1984 (quality tiers).
//!
//! # Frames and winding
//! - Vessel-local metres, the content cell frame: +X bow, +Y up, +Z starboard; cell (0,0,0) min corner at the
//!   origin, a cell spans `pos * 0.5 .. pos * 0.5 + 0.5` m. Chunk meshes are in vessel-local metres too (not
//!   chunk-local), so every chunk of one vessel shares the vessel transform and a moving ship costs one
//!   transform per chunk (L1921).
//! - Part models are in part-local metres: origin at the footprint's minimum corner at rotation 0, same axes.
//! - Front faces wind clockwise seen from outside, Godot's convention (L1927).
use serde::{Deserialize, Serialize};

/// Render chunk edge in cells (32^3 cells = 16 m, L1925). Equal to `content::cell::RENDER_CHUNK`.
pub const CHUNK: usize = 32;
/// Padded chunk edge: one neighbour cell on every side (34^3, L1925).
pub const CHUNK_P: usize = CHUNK + 2;
/// Cells in one padded chunk buffer.
pub const CHUNK_P3: usize = CHUNK_P * CHUNK_P * CHUNK_P;
/// Cell edge in metres, as f32 for mesh output.
pub const CELL_M_F32: f32 = 0.5;
/// Index buffers stay u16 up to this many vertices, else u32 (L1927).
pub const MAX_U16_VERTICES: usize = 65_536;
/// Meshing budget per chunk on one minimum-spec core, microseconds (spike S5, L1925). Not enforced in code;
/// the S5 benchmark measures it.
pub const CHUNK_BUDGET_US: u32 = 200;

/// Rendering quality tier (L1977-1982). Fallback is the Compatibility renderer: Layer A VFX only, MSAA 2x.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum QualityTier { Fallback, Low, #[default] Medium, High }

/// Axis-aligned box in the mesh's own frame (metres).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Aabb { pub min: [f32; 3], pub max: [f32; 3] }
impl Aabb {
    pub const EMPTY: Aabb = Aabb { min: [f32::MAX; 3], max: [f32::MIN; 3] };
    pub fn is_empty(&self) -> bool { self.min[0] > self.max[0] }
    pub fn include(&mut self, p: [f32; 3]) {
        for i in 0..3 { self.min[i] = self.min[i].min(p[i]); self.max[i] = self.max[i].max(p[i]); }
    }
}

/// Godot-ready surface arrays (triangles). Every Vec except `positions` and `indices` is either empty or the
/// same length as `positions`. The client copies these into `PackedVector3Array` and friends with
/// `ARRAY_VERTEX`, `ARRAY_NORMAL`, `ARRAY_COLOR`, `ARRAY_CUSTOM0` and `ARRAY_INDEX`.
///
/// Flat shading: vertices are never shared between faces with different normals; normals come only from the
/// shape table or primitive faces, never smoothed across cells (L1815, mesher test).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Linear RGBA. Empty at hull LOD0 (L1927); swatch times AO on hull LODs 1-4 (L1943); token albedo on part
    /// models and VFX meshes.
    pub colors: Vec<[f32; 4]>,
    /// `ARRAY_CUSTOM0` as RGBA_FLOAT. Used by: ocean clipmap (morph data), VFX meshes (per-vertex phase or
    /// band index), part models (piece-local emission strength in x). Empty when unused.
    pub custom0: Vec<[f32; 4]>,
    /// Triangle list, clockwise front faces.
    pub indices: Vec<u32>,
}
impl MeshData {
    pub fn new() -> Self { Self::default() }
    pub fn vertex_count(&self) -> usize { self.positions.len() }
    pub fn triangle_count(&self) -> usize { self.indices.len() / 3 }
    pub fn is_empty(&self) -> bool { self.indices.is_empty() }
    /// Bounding box of `positions`.
    pub fn aabb(&self) -> Aabb { let mut b = Aabb::EMPTY; for p in &self.positions { b.include(*p); } b }
    pub fn clear(&mut self) { self.positions.clear(); self.normals.clear(); self.colors.clear(); self.custom0.clear(); self.indices.clear(); }
}

/// Godot `Mesh.ARRAY_FORMAT_*` bits used by the packed surfaces (Godot 4.7 `RenderingServer` values).
pub mod format {
    pub const VERTEX: u64 = 1;
    pub const NORMAL: u64 = 1 << 1;
    pub const COLOR: u64 = 1 << 3;
    pub const INDEX: u64 = 1 << 12;
    /// `ARRAY_FLAG_FORMAT_CURRENT_VERSION` (version 2 in bits 35+).
    pub const FORMAT_CURRENT_VERSION: u64 = 1 << 35;
    /// LOD0 chunk surface: `VERTEX | NORMAL | INDEX | FORMAT_CURRENT_VERSION` = 34359742467 (L1927).
    pub const CHUNK_SURFACE: u64 = VERTEX | NORMAL | INDEX | FORMAT_CURRENT_VERSION;
    /// Hull LOD1-4 and part meshes: adds vertex colour in the attribute stream (L1943).
    pub const COLORED_SURFACE: u64 = VERTEX | NORMAL | COLOR | INDEX | FORMAT_CURRENT_VERSION;
    /// Welded position-only shadow mesh (L1929): `VERTEX | INDEX | FORMAT_CURRENT_VERSION`.
    pub const SHADOW_SURFACE: u64 = VERTEX | INDEX | FORMAT_CURRENT_VERSION;
    /// `RenderingServer.PRIMITIVE_TRIANGLES`.
    pub const PRIMITIVE_TRIANGLES: u32 = 3;
    const _CHECK: () = assert!(CHUNK_SURFACE == 34_359_742_467);
}

/// Raw surface for `RenderingServer.mesh_create_from_surfaces` (L1927-1929): the client builds the Godot
/// Dictionary (`format`, `primitive`, `vertex_data`, `attribute_data`, `vertex_count`, `index_data`,
/// `index_count`, `aabb`) from these fields with no per-vertex work on the main thread. Values verified against
/// the 4.7 `extension_api.json` (`ARRAY_FORMAT_*`, `ARRAY_FLAG_FORMAT_CURRENT_VERSION`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SurfaceBytes {
    /// One of [`format::CHUNK_SURFACE`], [`format::COLORED_SURFACE`] or [`format::SHADOW_SURFACE`].
    pub format: u64,
    pub primitive: u32,
    /// Chunk surface: N x 12 B little-endian f32 positions, then N x 4 B octahedral normals (2 x u16 UNORM).
    /// Shadow surface: N x 12 B positions only.
    pub vertex_data: Vec<u8>,
    /// Attribute stream: empty for chunk and shadow surfaces; N x 4 B RGBA8 UNORM colours for
    /// `COLORED_SURFACE` (the implementer confirms Godot 4.7's colour element layout in a byte-layout test).
    pub attribute_data: Vec<u8>,
    pub vertex_count: u32,
    /// Little-endian u16 indices when `vertex_count <= MAX_U16_VERTICES`, else u32.
    pub index_data: Vec<u8>,
    pub index_count: u32,
    pub aabb: Aabb,
}
impl SurfaceBytes {
    pub fn index_is_u32(&self) -> bool { self.vertex_count as usize > MAX_U16_VERTICES }
    pub fn is_empty(&self) -> bool { self.index_count == 0 }
}

/// Mipless 34^3 RGBA8 Texture3D of one chunk (157,216 bytes, L1931). Texel order: x fastest, then y, then z
/// (Godot `Image` slices are z layers of x-by-y images), padded coordinates (chunk cell c at c + 1).
/// Channels:
/// - R occupancy: 255 solid, 0 air (sealed air counts as air here; AO samples it linearly).
/// - G low nibble: blueprint palette index 0..=15 (L130); bits 4-5: [`MaterialClass`]; bits 6-7 zero.
/// - B scorch 0..=255 (L1931).
/// - A bits 0-2: armour tier 0..=4; bits 3-7: per-cell wear seed 0..=31 from (vessel seed, cell index).
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeTexture { pub bytes: Vec<u8> }
impl VolumeTexture {
    pub const EDGE: usize = CHUNK_P;
    pub const BYTES: usize = CHUNK_P3 * 4;
    pub fn zeroed() -> Self { Self { bytes: vec![0; Self::BYTES] } }
    pub fn texel_offset(px: usize, py: usize, pz: usize) -> usize { 4 * (px + CHUNK_P * (py + CHUNK_P * pz)) }
}

/// Material class in volume G bits 4-5 (L1931). The hull shader picks paint, glass, emissive or bare from it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum MaterialClass {
    /// Painted steel, aluminium, composite and part bodies: albedo from the palette swatch.
    #[default] Paint = 0,
    /// Bridge glazing: opaque, dark, glossy (L1933).
    Glass = 1,
    /// Lit fittings. Reserved: no cell material maps here yet (parts carry their own emission).
    Emissive = 2,
    /// An interior cell exposed by damage: it was not exterior in the intact design and now faces exposed air.
    /// The shader draws primer (Y 0.25, warm, C <= 0.06) instead of paint (L1941).
    Bare = 3,
}

/// Hull level of detail (L1943). LOD0 is 0.5 m cells; LODn cells are 0.5 * 2^n m.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Lod { #[default] L0, L1, L2, L3, L4 }
impl Lod {
    pub const ALL: [Lod; 5] = [Lod::L0, Lod::L1, Lod::L2, Lod::L3, Lod::L4];
    /// Cell edge at this LOD in metres: 0.5, 1, 2, 4, 8.
    pub fn cell_m(self) -> f64 { 0.5 * (1u32 << self as u32) as f64 }
    /// Cells per LOD cell along one axis: 1, 2, 4, 8, 16.
    pub fn factor(self) -> usize { 1 << self as usize }
    /// Distance where this LOD starts at 1080p, 60 degree vertical FOV, Medium (1.5 px per cell), metres:
    /// 0, 623, 1,247, 2,494, 4,988 (L1943). Runtime picks use the live FOV through `lod::pick_lod`.
    pub fn reference_start_m(self) -> f64 { [0.0, 623.0, 1_247.0, 2_494.0, 4_988.0][self as usize] }
}

/// Cache key for shared chunk meshes: undamaged same-blueprint ships share chunk meshes (L1945).
/// `damage_epoch` is 0 for an intact design and the vessel's own counter once any cell in the chunk changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkKey {
    pub blueprint: content::ids::ContentHash,
    pub coord: content::cell::RenderChunkCoord,
    pub lod: Lod,
    pub damage_epoch: u32,
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum MeshError {
    #[error("grid size {0:?} exceeds the meshable bound")]
    GridTooLarge([u16; 3]),
    #[error("chunk {0:?} outside the grid")]
    ChunkOutOfRange(content::cell::RenderChunkCoord),
    #[error("palette has {0} entries; at most 16")]
    Palette(usize),
    #[error("view slices do not match the grid length")]
    ViewLength,
}
