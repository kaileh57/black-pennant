//! Unit `hm-chunk`. Builds the per-chunk inputs from a plain cell-grid view: the padded 34^3 class buffer and
//! bgm masks, the 34^3 RGBA8 volume texture, chunk enumeration and dirty-chunk sets from the damage stream.
//! DESIGN.md L1925 (class buffer, 32^3 padded to 34^3, grid-aligned), L1931 (volume channels), L1941 (damage:
//! removed cells vanish, exposed interiors primer), L561 (bucket 8 = removed), L1945 (holes within 2 frames).
//!
//! The same code serves editor designs (empty damage, no sealed air) and damaged live vessels (sim-core's
//! `VesselGrid::view()` or the client's replica of it), so meshing never converts formats.
use crate::types::{MaterialClass, MeshError, VolumeTexture, CHUNK, CHUNK_P, CHUNK_P3};
use content::cell::{CellChange, CellGridView, RenderChunkCoord};

/// Class-buffer values (L1925). Greedy input is `u16` per padded cell in binary-greedy-meshing's ZXY order
/// (`binary_greedy_meshing::pad_linearize::<32>(x, y, z) = z + 1 + (x + 1) * 34 + (y + 1) * 34^2`, with chunk
/// cell (x, y, z) in content's frame: x bow, y up, z starboard).
pub mod class {
    /// Exposed air: faces are emitted against it. Removed cells (bucket 8) and air of a breached or entered
    /// compartment are exposed air.
    pub const EXPOSED_AIR: u16 = 0;
    pub const OPAQUE: u16 = 1;
    pub const GLASS: u16 = 2;
    /// Reserved (see `MaterialClass::Emissive`).
    pub const EMISSIVE: u16 = 3;
    /// Shaped cells: `SHAPED_BASE | shape byte` (kind and orientation, `content::cell::CellShape`). Distinct
    /// per shape and orientation so greedy merging never joins them; the shaped pass owns their faces.
    pub const SHAPED_BASE: u16 = 0x0100;
    /// Air inside a still-sealed compartment: bgm treats any non-zero value as solid, so solids next to it emit
    /// no faces. Quads bgm emits for this value itself must be dropped (L1925: sealed air has no faces).
    pub const SEALED_AIR: u16 = 0xFFFF;
    pub fn is_shaped(v: u16) -> bool { v != SEALED_AIR && v & 0xFF00 == SHAPED_BASE }
    pub fn shape_byte(v: u16) -> u8 { (v & 0xFF) as u8 }
}

/// Everything hull-mesh needs about one vessel besides its cells.
#[derive(Clone, Copy, Debug)]
pub struct MeshInput<'a> {
    pub view: CellGridView<'a>,
    /// Blueprint paint table in linear RGB, at most 16 entries (L130). Used by LOD vertex colours; LOD0 colour
    /// comes from the volume texture's palette index in the shader.
    pub palette: &'a [[f32; 3]],
    /// Per-vessel wear seed: weathering is seeded per ship and cell (L1940). Editor designs pass 0.
    pub wear_seed: u64,
    /// Y cell row of the design waterline (salt band, mask and wet-cell rules key off it).
    pub design_waterline_y: i16,
}

/// The padded class buffer of one chunk plus the two bgm column masks.
/// `opaque_mask` and `trans_mask` follow bgm's `fast_mesh` layout: one u64 per (x, y) padded column of 34
/// z-bits, index `x + y * 34` padded coordinates; bit z set where the voxel is opaque / transparent.
/// No class is transparent in v1 (glass renders opaque, L1933), so `trans_mask` is all zero.
#[derive(Clone, Debug)]
pub struct ClassBuffer {
    pub coord: RenderChunkCoord,
    pub voxels: Vec<u16>,
    pub opaque_mask: Vec<u64>,
    pub trans_mask: Vec<u64>,
    /// True when the 32^3 interior holds no solid cell (the chunk has no mesh). An interior of only sealed air
    /// is empty unless a shaped cell sits in the buffer: the shaped pass then gives sealed air next to the shaped
    /// cell's open side a face, so the chunk is not empty.
    pub empty: bool,
    /// True when the chunk interior or its one-cell padding contains at least one shaped cell (skip the shaped
    /// pass otherwise). A shaped cell in the padding counts: bgm suppresses the interior cube face against it, and
    /// the shaped pass must emit what of that face the shaped cell leaves uncovered.
    pub has_shaped: bool,
}
impl ClassBuffer {
    pub fn new(coord: RenderChunkCoord) -> Self {
        Self { coord, voxels: vec![class::EXPOSED_AIR; CHUNK_P3], opaque_mask: vec![0; CHUNK_P * CHUNK_P], trans_mask: vec![0; CHUNK_P * CHUNK_P], empty: true, has_shaped: false }
    }
    /// Padded linear index of chunk cell (x, y, z) in -1..=32 (bgm ZXY order).
    pub fn pad_index(x: i32, y: i32, z: i32) -> usize { ((z + 1) + (x + 1) * CHUNK_P as i32 + (y + 1) * (CHUNK_P * CHUNK_P) as i32) as usize }
}

/// Number of render chunks along each axis for a grid of `size` cells (ceil(size / 32)).
pub fn chunk_dims(size: [u16; 3]) -> [u8; 3] {
    let d = |s: u16| ((s as usize + CHUNK - 1) / CHUNK) as u8;
    [d(size[0]), d(size[1]), d(size[2])]
}


mod view;
#[cfg(test)]
mod tests;

pub(crate) use view::Cells;
use content::cell::MAX_GRID_DIM;
use view::{classify, wear_bits, EnclosureCache};

/// `chunk_dims` with the size clamped to the meshable bound, so oversized (invalid) sizes cannot wrap the u8.
fn clamped_dims(size: [u16; 3]) -> [u8; 3] { chunk_dims([size[0].min(MAX_GRID_DIM), size[1].min(MAX_GRID_DIM), size[2].min(MAX_GRID_DIM)]) }

fn coord_in_range(size: [u16; 3], c: RenderChunkCoord) -> bool {
    let d = clamped_dims(size);
    c.x < d[0] && c.y < d[1] && c.z < d[2]
}

/// Minimum cell of a chunk minus the one-cell padding.
fn padded_origin(c: RenderChunkCoord) -> [i32; 3] { [c.x as i32 * CHUNK as i32 - 1, c.y as i32 * CHUNK as i32 - 1, c.z as i32 * CHUNK as i32 - 1] }

/// Every chunk coordinate of the grid in ascending (x, y, z) order. Unit: `hm-chunk`.
pub fn all_chunks(size: [u16; 3]) -> Vec<RenderChunkCoord> {
    let d = clamped_dims(size);
    let mut out = Vec::with_capacity(d[0] as usize * d[1] as usize * d[2] as usize);
    for x in 0..d[0] {
        for y in 0..d[1] {
            for z in 0..d[2] {
                out.push(RenderChunkCoord { x, y, z });
            }
        }
    }
    out
}

/// Check that the view's slices match its size (cells, damage and scorch empty or `len` long, sealed bits
/// empty or `ceil(len / 64)` words) and that the palette has at most 16 entries. Unit: `hm-chunk`.
pub fn validate_input(input: &MeshInput<'_>) -> Result<(), MeshError> {
    let v = &input.view;
    if v.size.iter().any(|&d| d > MAX_GRID_DIM) {
        return Err(MeshError::GridTooLarge(v.size));
    }
    if !Cells::new(*v).well_formed() {
        return Err(MeshError::ViewLength);
    }
    if input.palette.len() > 16 {
        return Err(MeshError::Palette(input.palette.len()));
    }
    Ok(())
}

/// Fill `out` for chunk `coord` including the one-cell padding from neighbouring chunks (cells outside the
/// grid are exposed air). Mapping per cell: empty material or bucket 8 -> exposed air; empty and sealed bit
/// set -> sealed air; glass -> GLASS; shaped (non-cube) -> `SHAPED_BASE | shape byte`; else OPAQUE.
/// Also fills both masks, `empty` and `has_shaped`. Reuses `out`'s allocations. Unit: `hm-chunk`.
///
/// A shaped glass cell takes the shaped class, so sloped glazing keeps its slope; the volume texture still
/// marks it glass. Shape bytes with an invalid orientation (24..=31) are stored as orientation 0.
pub fn fill_class_buffer(input: &MeshInput<'_>, coord: RenderChunkCoord, out: &mut ClassBuffer) -> Result<(), MeshError> {
    validate_input(input)?;
    if !coord_in_range(input.view.size, coord) {
        return Err(MeshError::ChunkOutOfRange(coord));
    }
    let g = Cells::new(input.view);
    out.coord = coord;
    out.voxels.clear();
    out.voxels.resize(CHUNK_P3, class::EXPOSED_AIR);
    out.opaque_mask.clear();
    out.opaque_mask.resize(CHUNK_P * CHUNK_P, 0);
    out.trans_mask.clear();
    out.trans_mask.resize(CHUNK_P * CHUNK_P, 0);
    let o = padded_origin(coord);
    let mut empty = true;
    let mut has_shaped = false;
    let mut interior_sealed = false;
    // Walk view rows (x contiguous) and scatter into bgm's z-fastest layout.
    let x_lo = (-o[0]).max(0) as usize;
    let x_hi = (g.s[0] - o[0]).clamp(0, CHUNK_P as i32) as usize;
    for py in 0..CHUNK_P {
        let y = o[1] + py as i32;
        if y < 0 || y >= g.s[1] {
            continue;
        }
        for pz in 0..CHUNK_P {
            let z = o[2] + pz as i32;
            if z < 0 || z >= g.s[2] {
                continue;
            }
            let interior_yz = (1..=CHUNK).contains(&py) && (1..=CHUNK).contains(&pz);
            let Some(row) = g.index(o[0] + x_lo as i32, y, z) else { continue };
            for px in x_lo..x_hi {
                let v = g.class(Some(row + (px - x_lo)));
                if v == class::EXPOSED_AIR {
                    continue;
                }
                out.voxels[pz + CHUNK_P * (px + CHUNK_P * py)] = v;
                out.opaque_mask[px + CHUNK_P * py] |= 1 << pz;
                has_shaped |= class::is_shaped(v);
                if interior_yz && (1..=CHUNK).contains(&px) {
                    if v == class::SEALED_AIR { interior_sealed = true; } else { empty = false; }
                }
            }
        }
    }
    out.empty = empty && !(has_shaped && interior_sealed);
    out.has_shaped = has_shaped;
    Ok(())
}

/// Build the chunk's 34^3 volume texture (layout on [`VolumeTexture`]): occupancy, palette index and
/// [`MaterialClass`], scorch, armour tier and wear seed. A solid cell is `Bare` when it had no exterior face in
/// the intact design but now touches exposed air through damage or a breached compartment. Wear seed bits come
/// from a stateless hash of (wear_seed, cell index). Unit: `hm-chunk`.
///
/// Air texels are zero except for scorch. A malformed view (see [`validate_input`]) or a chunk outside the grid
/// yields an all-zero volume rather than a panic.
pub fn build_volume(input: &MeshInput<'_>, coord: RenderChunkCoord) -> VolumeTexture {
    let g = Cells::new(input.view);
    let mut vol = VolumeTexture::zeroed();
    if !g.well_formed() || !coord_in_range(input.view.size, coord) {
        return vol;
    }
    let o = padded_origin(coord);
    // Texels cover the padded box; their face neighbours reach one cell further.
    let mut cache = EnclosureCache::new([o[0] - 1, o[1] - 1, o[2] - 1], CHUNK_P as i32 + 2);
    for pz in 0..CHUNK_P {
        for py in 0..CHUNK_P {
            for px in 0..CHUNK_P {
                let p = [o[0] + px as i32, o[1] + py as i32, o[2] + pz as i32];
                let Some(i) = g.index(p[0], p[1], p[2]) else { continue };
                let t = VolumeTexture::texel_offset(px, py, pz);
                vol.bytes[t + 2] = g.scorch(i);
                if !g.solid(i) {
                    continue;
                }
                let c = input.view.cells[i];
                let mc = classify(&g, p, i, Some(&mut cache));
                vol.bytes[t] = 255;
                vol.bytes[t + 1] = (c.paint & 0x0F) | ((mc as u8) << 4);
                vol.bytes[t + 3] = (c.armour as u8 & 0x07) | (wear_bits(input.wear_seed, i) << 3);
            }
        }
    }
    vol
}

/// Material class of one cell for the volume texture (see [`build_volume`] for `Bare`). Unit: `hm-chunk`.
///
/// Air, an index past the grid or a malformed view read as `Paint`.
pub fn material_class(input: &MeshInput<'_>, cell: content::ids::CellIndex) -> MaterialClass {
    let g = Cells::new(input.view);
    if !g.well_formed() {
        return MaterialClass::Paint;
    }
    match g.pos(cell.index()) {
        Some(p) => classify(&g, p, cell.index(), None),
        None => MaterialClass::Paint,
    }
}

/// Push every chunk whose padded box (cells `-1..=32` of the chunk) holds cell `p`.
fn chunks_padding(dims: [u8; 3], p: [i32; 3], out: &mut Vec<RenderChunkCoord>) {
    let mut range = [(0u8, 0u8); 3];
    for k in 0..3 {
        let c = p[k] / CHUNK as i32;
        let r = p[k] % CHUNK as i32;
        let lo = if r == 0 && c > 0 { c - 1 } else { c };
        let hi = if r == CHUNK as i32 - 1 && c + 1 < dims[k] as i32 { c + 1 } else { c };
        if c >= dims[k] as i32 {
            return;
        }
        range[k] = (lo as u8, hi as u8);
    }
    for x in range[0].0..=range[0].1 {
        for y in range[1].0..=range[1].1 {
            for z in range[2].0..=range[2].1 {
                out.push(RenderChunkCoord { x, y, z });
            }
        }
    }
}

fn dirty_from(size: [u16; 3], cells: impl Iterator<Item = usize>) -> Vec<RenderChunkCoord> {
    let size = [size[0].min(MAX_GRID_DIM), size[1].min(MAX_GRID_DIM), size[2].min(MAX_GRID_DIM)];
    let dims = chunk_dims(size);
    let len = size[0] as usize * size[1] as usize * size[2] as usize;
    let (sx, sz) = (size[0] as usize, size[2] as usize);
    let mut out = Vec::new();
    for i in cells {
        // Indices past the grid come from bad network data: ignore them.
        if i >= len {
            continue;
        }
        let p = [(i % sx) as i32, (i / (sx * sz)) as i32, ((i / sx) % sz) as i32];
        chunks_padding(dims, p, &mut out);
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Chunks whose mesh or volume must rebuild after these damage changes: the chunk of each changed cell plus
/// every neighbouring chunk whose padding includes it (cells on a chunk face, edge or corner). Sorted ascending,
/// no duplicates. Unit: `hm-chunk`.
pub fn dirty_chunks(size: [u16; 3], changes: &[CellChange]) -> Vec<RenderChunkCoord> { dirty_from(size, changes.iter().map(|c| c.cell.index())) }

/// Chunks whose class buffer changes when a compartment's sealed state flips (breached or entered, L1925):
/// every chunk containing or padding any of `cells`. Sorted ascending, no duplicates. Unit: `hm-chunk`.
pub fn dirty_chunks_for_cells(size: [u16; 3], cells: &[content::ids::CellIndex]) -> Vec<RenderChunkCoord> { dirty_from(size, cells.iter().map(|c| c.index())) }
