//! Unit `content-cell`. The 0.5 m cell model: positions, materials, armour tiers, the shape+rotation byte,
//! the dense grid and its run-length file form. DESIGN.md L128-134, L535, L1923-1931, L2075.
//!
//! The types here are the contract. The unit implements the functions marked `todo!`.
//!
//! # Frames
//! Vessel-local cell frame: +X toward the bow, +Y up, +Z to starboard (right-handed). Cell (0,0,0) is the
//! minimum corner of the blueprint's bounding box. A cell's centre is `(pos + 0.5) * CELL_M` in vessel-local metres.
//! World frame (sim-core and Godot agree): +Y up, sea level at y = 0, +X east, +Z south.
//!
//! # Canonical shapes
//! At orientation 0 in the unit cell: Wedge is `x + y <= 1` (slope normal (1,1,0), full faces X- and Y-);
//! Corner is `x + y + z <= 1` (normal (1,1,1), no full face); InverseCorner is `x + y + z <= 2` (normal (1,1,1),
//! full faces X-, Y-, Z-); SlopeHigh is `y <= 1 - x/2` and SlopeLow is `y <= 0.5 - x/2` (normal (1,2,0); a 1:2
//! slope along +X is SlopeHigh then SlopeLow). Orientation `o` maps a canonical vector `v` to
//! `v.x * M[0] + v.y * M[1] + v.z * M[2]` with `M = orientation_matrix(o)`, about the cell centre. hull-mesh builds
//! its face polygons from these solids; [`shape_corner_mask`] gives each shape's corner vertices.
mod grid;
mod shape;
#[cfg(test)]
mod tests;

pub use shape::{orientation_index, shape_corner_mask, ORIENTATION_COUNT};

use crate::enums::Axis;
use crate::ids::CellIndex;
use serde::{Deserialize, Serialize};

/// Cell edge in metres (L128).
pub const CELL_M: f64 = 0.5;
/// Volume of one cell in cubic metres.
pub const CELL_VOLUME_M3: f64 = 0.125;
/// Area of one cell face in square metres; one destroyed outer face is one breach of this area (L626).
pub const CELL_FACE_M2: f64 = 0.25;
/// Simulation chunk edge in cells (16^3 cells = 8 m, L1114).
pub const SIM_CHUNK: usize = 16;
/// Render chunk edge in cells (32^3 = 16 m = 2x2x2 sim chunks, same grid alignment, L1114).
pub const RENDER_CHUNK: usize = 32;
/// Player hull length limits in metres (L126). Enemy-class mode allows up to [`MAX_ENEMY_HULL_M`].
pub const MIN_PLAYER_HULL_M: f64 = 10.0;
pub const MAX_PLAYER_HULL_M: f64 = 90.0;
pub const MAX_ENEMY_HULL_M: f64 = 150.0;
/// Hard bound on a grid's size in cells on any axis (150 m = 300 cells, plus masts and margin).
pub const MAX_GRID_DIM: u16 = 384;
/// Destroyed-cell damage bucket; health = 1 - bucket / 8 (L561).
pub const BUCKET_DESTROYED: u8 = 8;

/// Integer cell position in the vessel-local cell frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellPos { pub x: i16, pub y: i16, pub z: i16 }
impl CellPos {
    pub const fn new(x: i16, y: i16, z: i16) -> Self { Self { x, y, z } }
    /// Centre of the cell in vessel-local metres.
    pub fn centre_m(self) -> [f64; 3] { [(self.x as f64 + 0.5) * CELL_M, (self.y as f64 + 0.5) * CELL_M, (self.z as f64 + 0.5) * CELL_M] }
}

/// One of the six axis directions; used for faces, openings and conduit steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Face { XNeg, XPos, YNeg, YPos, ZNeg, ZPos }
impl Face {
    pub const ALL: [Face; 6] = [Face::XNeg, Face::XPos, Face::YNeg, Face::YPos, Face::ZNeg, Face::ZPos];
    pub fn offset(self) -> (i16, i16, i16) { match self { Face::XNeg => (-1, 0, 0), Face::XPos => (1, 0, 0), Face::YNeg => (0, -1, 0), Face::YPos => (0, 1, 0), Face::ZNeg => (0, 0, -1), Face::ZPos => (0, 0, 1) } }
    pub fn axis(self) -> Axis { match self { Face::XNeg | Face::XPos => Axis::X, Face::YNeg | Face::YPos => Axis::Y, _ => Axis::Z } }
}

/// Structural material of a cell. Per-material numbers live in [`crate::catalogue::MaterialDef`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Material {
    /// No cell. Only appears in packed and dense forms.
    Empty = 0,
    #[default] Steel = 1,
    Aluminium = 2,
    /// Low-signature composite (26 m insertion craft, L151).
    Composite = 3,
    /// Bridge glazing; render class glass (L1925).
    Glass = 4,
    /// Cells owned by a functional part. The part's own data sets mass and behaviour (L132).
    PartBody = 5,
}

/// Armour tier painted on outer cells: 5, 15, 40, 80 mm (L128, L572).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum ArmourTier { #[default] None = 0, Mm5 = 1, Mm15 = 2, Mm40 = 3, Mm80 = 4 }
impl ArmourTier {
    pub fn thickness_mm(self) -> f64 { [0.0, 5.0, 15.0, 40.0, 80.0][self as usize] }
}

/// Shape family of a cell (L132, L1923).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum ShapeKind {
    #[default] Cube = 0,
    /// 45 degree wedge.
    Wedge = 1,
    Corner = 2,
    InverseCorner = 3,
    /// Lower half of a placeable 1:2 slope.
    SlopeLow = 4,
    /// Upper half of a placeable 1:2 slope.
    SlopeHigh = 5,
}

/// The per-cell shape+rotation byte: bits 7..5 = [`ShapeKind`], bits 4..0 = orientation 0..=23 (L128).
/// The simulation treats every shaped cell as a full cell except for armour obliquity (L535).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CellShape(pub u8);
impl CellShape {
    pub const CUBE: CellShape = CellShape(0);
    pub const fn new(kind: ShapeKind, orientation: u8) -> Self { CellShape(((kind as u8) << 5) | (orientation & 0x1f)) }
    pub fn kind(self) -> ShapeKind { match self.0 >> 5 { 1 => ShapeKind::Wedge, 2 => ShapeKind::Corner, 3 => ShapeKind::InverseCorner, 4 => ShapeKind::SlopeLow, 5 => ShapeKind::SlopeHigh, _ => ShapeKind::Cube } }
    pub fn orientation(self) -> u8 { self.0 & 0x1f }
    pub fn is_cube(self) -> bool { self.kind() == ShapeKind::Cube }
}

/// The 24 proper rotations of the cube. Orientation index `o` maps canonical shape axes to cell axes.
/// Must return a signed permutation matrix (rows = images of canonical X, Y, Z); index 0 is identity.
/// The table is the single source for hull-mesh's face table and for armour obliquity. Unit: `content-cell`.
pub fn orientation_matrix(orientation: u8) -> [[i8; 3]; 3] { shape::orientation_matrix(orientation) }

/// Orientation obtained by applying a 90 degree turn about `axis` to `orientation` (editor R / Shift+R / T, L244)
/// and by mirroring across the centreline for symmetry tools. Unit: `content-cell`.
pub fn rotate_orientation(orientation: u8, axis: Axis, quarter_turns: i8) -> u8 { shape::rotate_orientation(orientation, axis, quarter_turns) }
/// Mirror a shape across the vessel centreline plane (z -> -z). Unit: `content-cell`.
pub fn mirror_shape_z(shape: CellShape) -> CellShape { shape::mirror_shape_z(shape) }

/// Outward unit normal of the shape's sloped surface in the cell frame; cubes return the face normal nearest
/// `incoming` (direction of travel of the shell, cell frame). Used only for armour obliquity (L535, L563).
/// Unit: `content-cell`.
pub fn armour_normal(shape: CellShape, incoming: [f64; 3]) -> [f64; 3] { shape::armour_normal(shape, incoming) }

/// Bit mask (bit per [`Face`] in `Face::ALL` order) of the faces this shape fully covers.
/// hull-mesh uses it for neighbour-cover culling (L1925). Unit: `content-cell`.
pub fn full_face_mask(shape: CellShape) -> u8 { shape::full_face_mask(shape) }

/// The loft voxelizer's 256-entry table: an inside/outside pattern of a boundary cell's 8 corners maps to the
/// nearest shape by Hamming distance, ties to cube (L1923). Bit `i` of `corners` is corner
/// `(i & 1, (i >> 1) & 1, (i >> 2) & 1)`. Returns `None` for the empty pattern. Unit: `content-cell`.
pub fn shape_from_corners(corners: u8) -> Option<CellShape> { shape::shape_from_corners(corners) }

/// One cell of structure as the blueprint stores it (L2075).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cell {
    pub material: Material,
    pub armour: ArmourTier,
    /// Index into the blueprint paint table, 0..=15 (L130).
    pub paint: u8,
    pub shape: CellShape,
}
impl Default for Cell { fn default() -> Self { Cell::EMPTY } }
impl Cell {
    pub const EMPTY: Cell = Cell { material: Material::Empty, armour: ArmourTier::None, paint: 0, shape: CellShape::CUBE };
    pub fn is_empty(&self) -> bool { self.material == Material::Empty }
}

/// A run of identical cells along +X in one row (L2075).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellRun { pub x: i16, pub len: u16, pub cell: Cell }

/// One row of cells at fixed (y, z). Rows are sorted by (y, z) and runs by x, so an edit changes only the
/// touched rows in a diff (L2075).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellRow { pub y: i16, pub z: i16, pub runs: Vec<CellRun> }

/// Run-length cell storage of a blueprint. Empty cells are not stored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CellRows(pub Vec<CellRow>);

/// Dense cell grid: the expanded, bounding-box form every consumer works on.
/// Linear index is `x + size[0] * (z + size[2] * y)` ([`CellGrid::index`]); that index is [`CellIndex`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellGrid {
    /// Extent in cells along X, Y, Z. Each is at most [`MAX_GRID_DIM`].
    pub size: [u16; 3],
    pub cells: Vec<Cell>,
}
impl CellGrid {
    pub fn new(size: [u16; 3]) -> Self { Self { size, cells: vec![Cell::EMPTY; size[0] as usize * size[1] as usize * size[2] as usize] } }
    pub fn len(&self) -> usize { self.cells.len() }
    pub fn is_empty(&self) -> bool { self.cells.is_empty() }
    pub fn in_bounds(&self, p: CellPos) -> bool { p.x >= 0 && p.y >= 0 && p.z >= 0 && (p.x as u16) < self.size[0] && (p.y as u16) < self.size[1] && (p.z as u16) < self.size[2] }
    pub fn index(&self, p: CellPos) -> Option<CellIndex> {
        if !self.in_bounds(p) { return None; }
        Some(CellIndex(p.x as u32 + self.size[0] as u32 * (p.z as u32 + self.size[2] as u32 * p.y as u32)))
    }
    pub fn pos(&self, i: CellIndex) -> CellPos {
        let (sx, sz) = (self.size[0] as u32, self.size[2] as u32);
        CellPos::new((i.0 % sx) as i16, (i.0 / (sx * sz)) as i16, ((i.0 / sx) % sz) as i16)
    }
    pub fn get(&self, p: CellPos) -> Cell { self.index(p).map(|i| self.cells[i.index()]).unwrap_or(Cell::EMPTY) }
    pub fn set(&mut self, p: CellPos, c: Cell) { if let Some(i) = self.index(p) { self.cells[i.index()] = c; } }
    /// Expand run-length rows into a dense grid sized to their bounding box (origin shifted to 0,0,0 must
    /// already hold: rows use non-negative coordinates). Unit: `content-cell`.
    pub fn from_rows(rows: &CellRows) -> CellGrid { grid::from_rows(rows) }
    /// Canonical run-length form: rows ascending by (y, z), maximal runs ascending by x. Unit: `content-cell`.
    pub fn to_rows(&self) -> CellRows { grid::to_rows(self) }
    /// Count of non-empty cells. Unit: `content-cell`.
    pub fn solid_count(&self) -> usize { self.cells.iter().filter(|c| !c.is_empty()).count() }
}

/// Render class of a cell in hull-mesh's class buffer (L1925).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderClass { ExposedAir, Opaque, Glass, Emissive, SealedAir }

/// Plain view of one vessel's cells handed to hull-mesh (which depends on content only) and produced by
/// sim-core for live vessels and by the editor for designs. All slices are indexed by [`CellIndex`].
#[derive(Clone, Copy, Debug)]
pub struct CellGridView<'a> {
    pub size: [u16; 3],
    pub cells: &'a [Cell],
    /// Damage bucket 0..=8 per cell. A cell at 8 is removed and meshes as air (L561). Empty slice = all intact.
    pub damage: &'a [u8],
    /// One bit per cell (LSB first, 64 cells per word): set where an empty cell is air inside a compartment that
    /// is still sealed, so it gets class 0xFFFF and no faces; breached or entered compartments clear it (L1925).
    /// Empty slice = nothing sealed.
    pub sealed_air: &'a [u64],
    /// Per-cell scorch 0..=255 for the volume texture B channel (L1931). Empty slice = none.
    pub scorch: &'a [u8],
}

/// One entry of the replicated, reliable damage stream: cell index + new bucket (L561, L920).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellChange { pub cell: CellIndex, pub bucket: u8 }

/// Coordinates of a 32^3 render chunk in a vessel grid (chunk c covers cells `c * 32 .. c * 32 + 32`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RenderChunkCoord { pub x: u8, pub y: u8, pub z: u8 }
impl RenderChunkCoord {
    pub fn of(p: CellPos) -> Self { Self { x: (p.x as usize / RENDER_CHUNK) as u8, y: (p.y as usize / RENDER_CHUNK) as u8, z: (p.z as usize / RENDER_CHUNK) as u8 } }
}
