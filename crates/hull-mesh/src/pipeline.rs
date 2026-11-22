//! Unit `hm-pipeline`. The top-level chunk mesher the client's meshing thread calls, and marking placement.
//! DESIGN.md L1921-1931 (chunk meshes, volume upload with its mesh), L1945 (one meshing thread outside the
//! sim's 3-core pool; shared meshes for undamaged same-blueprint ships), L130 and L1801 (markings; hull numbers
//! are in-engine MSDF text).
//!
//! Order per chunk: `chunk::fill_class_buffer` -> skip if empty -> `GreedyMesher::mesh_cubes` ->
//! `shaped::mesh_shaped` -> `greedy::fix_t_junctions` -> `surface::encode_chunk_surface` and
//! `encode_shadow_surface` -> `chunk::build_volume`.
//!
//! # Markings
//! A marking is centred on its anchor cell's face. Sizes, with `s = max(size_cells, 1)` cells of 0.5 m:
//! - stripe: a band `s` cells long and one cell tall;
//! - flag: `s` cells tall at 3:2 (width 1.5 times height);
//! - hull number: `s` cells tall, [`HULL_NUMBER_ADVANCE_EM`] of the height per character of `text`; a hull
//!   number with blank text is skipped.
//!
//! The face is the anchor cell's polygon on that cell face (its centroid, so a half face of a wedge carries the
//! mark on the half that exists). A shaped cell with nothing on that face carries the mark on its sloped face
//! when the slope faces that way (a curved bow plate), with the slope's normal. A face is covered when the
//! neighbouring cell fully covers the shared face; a slope is covered when every face it looks towards is.
//! `right` is the reading direction seen from outside: text stands upright (+Y up) on sides and ends, and on
//! decks and bottoms its top points to the bow.
use crate::chunk::{all_chunks, build_volume, fill_class_buffer, validate_input, ClassBuffer, MeshInput};
use crate::greedy::{fix_t_junctions, GreedyMesher};
use crate::shaped::{mesh_shaped, ShapeFaceTable};
use crate::surface::{encode_chunk_surface, encode_shadow_surface};
use crate::types::{Aabb, MeshData, MeshError, SurfaceBytes, VolumeTexture};
use content::blueprint::{Marking, MarkingKind};
use content::cell::{full_face_mask, CellGrid, CellPos, Face, RenderChunkCoord};
use std::sync::OnceLock;

/// Everything the client uploads for one LOD0 chunk: the surface and shadow mesh for
/// `mesh_create_from_surfaces` and the volume for `texture_3d_update` (L1929-1931).
#[derive(Clone, Debug)]
pub struct ChunkMesh {
    pub coord: RenderChunkCoord,
    /// No solid cells: the client frees any old RIDs and draws nothing.
    ///
    /// Also set when the chunk has solid cells but no face (buried under its neighbours), so the client never
    /// uploads a surface with no triangles. An empty chunk's volume is all zero and its surfaces are empty.
    pub empty: bool,
    pub surface: SurfaceBytes,
    pub shadow: SurfaceBytes,
    pub volume: VolumeTexture,
    pub aabb: Aabb,
    pub triangles: u32,
}

/// One meshing thread's reusable state. Not shared between threads; the client runs one per meshing thread.
pub struct ChunkMesher {
    pub greedy: GreedyMesher,
    pub shapes: ShapeFaceTable,
    pub class_buf: ClassBuffer,
    pub scratch: MeshData,
}
impl Default for ChunkMesher {
    fn default() -> Self { Self::new() }
}
impl ChunkMesher {
    /// Allocate buffers and build the shape table. Unit: `hm-pipeline`.
    pub fn new() -> Self {
        Self { greedy: GreedyMesher::new(), shapes: ShapeFaceTable::build(), class_buf: ClassBuffer::new(RenderChunkCoord::default()), scratch: MeshData::new() }
    }

    /// Mesh one LOD0 chunk. Unit: `hm-pipeline`.
    ///
    /// Errors: the input fails `chunk::validate_input`, or `coord` is outside the grid.
    pub fn mesh_chunk(&mut self, input: &MeshInput<'_>, coord: RenderChunkCoord) -> Result<ChunkMesh, MeshError> {
        let has_faces = self.mesh_into_scratch(input, coord)?;
        if !has_faces {
            let none = encode_chunk_surface(&MeshData::new());
            let shadow = encode_shadow_surface(&MeshData::new());
            return Ok(ChunkMesh { coord, empty: true, aabb: none.aabb, surface: none, shadow, volume: VolumeTexture::zeroed(), triangles: 0 });
        }
        let surface = encode_chunk_surface(&self.scratch);
        let shadow = encode_shadow_surface(&self.scratch);
        let volume = build_volume(input, coord);
        let triangles = surface.index_count / 3;
        // The encoder drops triangles with bad indices; if that left nothing, there is nothing to draw.
        let empty = triangles == 0;
        Ok(ChunkMesh { coord, empty, aabb: surface.aabb, surface, shadow, volume, triangles })
    }

    /// Same as [`Self::mesh_chunk`] but returns the unpacked mesh (tests, editor cutaway tools). Unit: `hm-pipeline`.
    pub fn mesh_chunk_data(&mut self, input: &MeshInput<'_>, coord: RenderChunkCoord) -> Result<MeshData, MeshError> {
        self.mesh_into_scratch(input, coord)?;
        Ok(self.scratch.clone())
    }

    /// Mesh every chunk of a vessel in ascending coordinate order (editor full rebuilds, spawn). Unit: `hm-pipeline`.
    ///
    /// Empty chunks are included (`empty == true`), so a full rebuild also retires chunks that lost all cells.
    pub fn mesh_all(&mut self, input: &MeshInput<'_>) -> Result<Vec<ChunkMesh>, MeshError> {
        validate_input(input)?;
        all_chunks(input.view.size).into_iter().map(|c| self.mesh_chunk(input, c)).collect()
    }

    /// The LOD0 stages into `self.scratch`; returns whether the mesh has any triangle.
    fn mesh_into_scratch(&mut self, input: &MeshInput<'_>, coord: RenderChunkCoord) -> Result<bool, MeshError> {
        self.scratch.clear();
        fill_class_buffer(input, coord, &mut self.class_buf)?;
        if self.class_buf.empty {
            return Ok(false);
        }
        self.greedy.mesh_cubes(&self.class_buf, &mut self.scratch);
        mesh_shaped(&self.shapes, &self.class_buf, &mut self.scratch);
        fix_t_junctions(&mut self.scratch);
        Ok(!self.scratch.is_empty())
    }
}

/// Where the client draws one marking: a quad on a hull face in vessel-local metres. Stripes and flags draw as
/// flat quads in the marking's paint; hull numbers draw `text` as in-engine MSDF text on this quad (L1801).
#[derive(Clone, Debug, PartialEq)]
pub struct MarkingQuad {
    pub centre: [f32; 3],
    /// Outward unit normal of the face; the quad sits 2 mm off the surface along it.
    pub normal: [f32; 3],
    /// Unit vector along the quad's width (text baseline).
    pub right: [f32; 3],
    pub size_m: [f32; 2],
    pub paint: u8,
    pub kind: content::blueprint::MarkingKind,
    pub text: String,
}

/// Offset of a marking quad off its surface, metres.
pub const MARKING_LIFT_M: f64 = 0.002;
/// Width of one hull-number character as a fraction of the text height (tabular digits in the MSDF font).
pub const HULL_NUMBER_ADVANCE_EM: f64 = 0.6;
/// Width over height of a flag.
pub const FLAG_ASPECT: f64 = 1.5;

/// The shape table for marking placement (meshing threads keep their own in `ChunkMesher`).
fn shape_table() -> &'static ShapeFaceTable {
    static TABLE: OnceLock<ShapeFaceTable> = OnceLock::new();
    TABLE.get_or_init(ShapeFaceTable::build)
}

fn opposite(f: Face) -> Face {
    match f {
        Face::XNeg => Face::XPos,
        Face::XPos => Face::XNeg,
        Face::YNeg => Face::YPos,
        Face::YPos => Face::YNeg,
        Face::ZNeg => Face::ZPos,
        Face::ZPos => Face::ZNeg,
    }
}

fn face_vec(f: Face) -> [f64; 3] {
    let (x, y, z) = f.offset();
    [x as f64, y as f64, z as f64]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] { [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]] }
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 { a[0] * b[0] + a[1] * b[1] + a[2] * b[2] }
fn normalise(a: [f64; 3]) -> Option<[f64; 3]> {
    let l = libm::sqrt(dot(a, a));
    (l > 1e-9 && l.is_finite()).then(|| a.map(|c| c / l))
}

/// The neighbour across face `f` of `p` fully covers the shared face.
fn covered(grid: &CellGrid, p: CellPos, f: Face) -> bool {
    let (dx, dy, dz) = f.offset();
    let q = CellPos::new(p.x.saturating_add(dx), p.y.saturating_add(dy), p.z.saturating_add(dz));
    if !grid.in_bounds(q) || q == p {
        return false;
    }
    let n = grid.get(q);
    !n.is_empty() && full_face_mask(n.shape) & (1 << opposite(f) as u8) != 0
}

/// Reading direction on a surface with outward normal `n`, seen from outside.
fn reading_right(n: [f64; 3]) -> [f64; 3] {
    // Upright on walls; on near-horizontal surfaces the top of the text points to the bow.
    let up = if n[1].abs() > 0.75 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
    let r = cross([-n[0], -n[1], -n[2]], up);
    normalise(r).unwrap_or([1.0, 0.0, 0.0])
}

/// Place each marking on its cell face; markings whose cell is now empty or whose face is covered are skipped.
/// Unit: `hm-pipeline`.
pub fn marking_quads(markings: &[Marking], grid: &CellGrid) -> Vec<MarkingQuad> {
    let mut out = Vec::with_capacity(markings.len());
    if grid.cells.len() != grid.size.iter().map(|&s| s as usize).product::<usize>() {
        return out;
    }
    let table = shape_table();
    for m in markings {
        if !grid.in_bounds(m.cell) {
            continue;
        }
        let cell = grid.get(m.cell);
        if cell.is_empty() {
            continue;
        }
        let s = m.size_cells.max(1) as f64 * content::cell::CELL_M;
        let size = match m.kind {
            MarkingKind::Stripe => [s, content::cell::CELL_M],
            MarkingKind::Flag => [s * FLAG_ASPECT, s],
            MarkingKind::HullNumber => {
                let chars = m.text.trim().chars().count();
                if chars == 0 {
                    continue;
                }
                [s * HULL_NUMBER_ADVANCE_EM * chars as f64, s]
            }
        };
        let fv = face_vec(m.face);
        let polys = table.polygons(cell.shape);
        // The cell's own polygon on this face, else a slope looking this way.
        let placed = if let Some(poly) = polys.iter().find(|p| p.on_face == Some(m.face)) {
            if covered(grid, m.cell, m.face) {
                continue;
            }
            Some((poly, fv))
        } else {
            polys.iter().filter(|p| p.on_face.is_none()).find_map(|p| {
                let n = normalise(p.normal.map(|c| c as f64))?;
                (dot(n, fv) > 1e-6).then_some((p, n))
            })
        };
        let Some((poly, n)) = placed else { continue };
        if poly.on_face.is_none() {
            let faces_seen: Vec<Face> = Face::ALL.into_iter().filter(|f| dot(n, face_vec(*f)) > 1e-6).collect();
            if faces_seen.iter().all(|f| covered(grid, m.cell, *f)) {
                continue;
            }
        }
        if poly.corners.is_empty() {
            continue;
        }
        let k = poly.corners.len() as f64;
        let mut c = [0.0f64; 3];
        for p in &poly.corners {
            for a in 0..3 {
                c[a] += p[a] as f64 / k;
            }
        }
        let origin = [m.cell.x as f64, m.cell.y as f64, m.cell.z as f64];
        let centre: [f64; 3] = [0, 1, 2].map(|a| (origin[a] + c[a]) * content::cell::CELL_M + n[a] * MARKING_LIFT_M);
        out.push(MarkingQuad {
            centre: centre.map(|v| v as f32),
            normal: n.map(|v| v as f32),
            right: reading_right(n).map(|v| v as f32),
            size_m: size.map(|v| v as f32),
            paint: m.paint,
            kind: m.kind,
            text: m.text.clone(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::greedy::tests::{append, assert_clockwise};
    use crate::surface::is_watertight;
    use content::cell::{ArmourTier, Cell, CellGridView, CellShape, Material, ShapeKind};

    fn steel(paint: u8) -> Cell { Cell { material: Material::Steel, armour: ArmourTier::None, paint, shape: CellShape::CUBE } }

    fn input<'a>(g: &'a CellGrid, damage: &'a [u8], sealed: &'a [u64]) -> MeshInput<'a> {
        MeshInput { view: CellGridView { size: g.size, cells: &g.cells, damage, sealed_air: sealed, scorch: &[] }, palette: &[], wear_seed: 3, design_waterline_y: 4 }
    }

    /// A lofted hull section across several chunks, shaped on its boundary, with a closed interior.
    fn hull(size: [u16; 3]) -> CellGrid {
        let mut g = CellGrid::new(size);
        let (sx, sy, sz) = (size[0] as f64, size[1] as f64, size[2] as f64);
        let inside = |x: f64, y: f64, z: f64| {
            let a = (z - sz / 2.0) / (sz / 2.0 - 1.0);
            let b = (y - sy * 0.8) / (sy * 0.7);
            a * a + b * b <= 1.0 && y <= sy * 0.8 && x >= 2.0 && x <= sx - 2.0
        };
        for x in 0..size[0] as i16 {
            for y in 0..size[1] as i16 {
                for z in 0..size[2] as i16 {
                    let mut mask = 0u8;
                    for i in 0..8u8 {
                        if inside(x as f64 + (i & 1) as f64, y as f64 + ((i >> 1) & 1) as f64, z as f64 + ((i >> 2) & 1) as f64) {
                            mask |= 1 << i;
                        }
                    }
                    if let Some(s) = content::cell::shape_from_corners(mask) {
                        g.set(CellPos::new(x, y, z), Cell { shape: s, ..steel(0) });
                    }
                }
            }
        }
        g
    }

    #[test]
    fn all_chunks_mesh_watertight_and_match_the_hand_wired_stages() {
        let g = hull([70, 36, 40]);
        let inp = input(&g, &[], &[]);
        let mut m = ChunkMesher::new();
        let mut all = MeshData::new();
        // Reference: the stages wired by hand with fresh state per chunk.
        let table = ShapeFaceTable::build();
        let mut reference = MeshData::new();
        for c in all_chunks(g.size) {
            let d = m.mesh_chunk_data(&inp, c).unwrap();
            assert_clockwise(&d);
            append(&mut all, &d);
            let mut buf = ClassBuffer::new(c);
            fill_class_buffer(&inp, c, &mut buf).unwrap();
            let mut r = MeshData::new();
            if !buf.empty {
                GreedyMesher::new().mesh_cubes(&buf, &mut r);
                mesh_shaped(&table, &buf, &mut r);
                fix_t_junctions(&mut r);
            }
            assert_eq!(d, r, "chunk {c:?}");
            append(&mut reference, &r);
        }
        assert!(!all.is_empty());
        assert!(is_watertight(&all));
        let meshes = m.mesh_all(&inp).unwrap();
        assert_eq!(meshes.len(), all_chunks(g.size).len());
        let tris: u32 = meshes.iter().map(|c| c.triangles).sum();
        assert_eq!(tris as usize, all.triangle_count());
        assert!(meshes.windows(2).all(|w| w[0].coord < w[1].coord));
        for c in &meshes {
            assert_eq!(c.volume.bytes.len(), VolumeTexture::BYTES);
            assert_eq!(c.empty, c.triangles == 0);
            if !c.empty {
                assert_eq!(c.surface.format, crate::types::format::CHUNK_SURFACE);
                assert_eq!(c.shadow.format, crate::types::format::SHADOW_SURFACE);
                assert_eq!(c.aabb, c.surface.aabb);
                assert!(c.shadow.vertex_count <= c.surface.vertex_count);
            }
        }
    }

    #[test]
    fn no_state_leaks_between_chunks() {
        let g = hull([70, 36, 40]);
        let other = hull([40, 20, 24]);
        let inp = input(&g, &[], &[]);
        let inp2 = input(&other, &[], &[]);
        let a = RenderChunkCoord { x: 1, y: 0, z: 0 };
        let mut m = ChunkMesher::new();
        let first = m.mesh_chunk(&inp, a).unwrap();
        m.mesh_chunk(&inp, RenderChunkCoord { x: 0, y: 1, z: 1 }).unwrap();
        m.mesh_chunk(&inp2, RenderChunkCoord::default()).unwrap();
        let again = m.mesh_chunk(&inp, a).unwrap();
        assert_eq!(first.surface, again.surface);
        assert_eq!(first.shadow, again.shadow);
        assert_eq!(first.volume, again.volume);
        let fresh = ChunkMesher::new().mesh_chunk(&inp, a).unwrap();
        assert_eq!(first.surface, fresh.surface);
    }

    #[test]
    fn empty_and_buried_chunks_are_empty() {
        // A solid 96^3 grid: chunk (1, 1, 1) is buried on every side, so it has cells but no face.
        let mut g = CellGrid::new([96, 96, 96]);
        g.cells.iter_mut().for_each(|c| *c = steel(0));
        let inp = input(&g, &[], &[]);
        let mut m = ChunkMesher::new();
        let buried = m.mesh_chunk(&inp, RenderChunkCoord { x: 1, y: 1, z: 1 }).unwrap();
        assert!(buried.empty);
        assert_eq!(buried.triangles, 0);
        assert!(buried.surface.is_empty());
        let edge = m.mesh_chunk(&inp, RenderChunkCoord { x: 0, y: 1, z: 1 }).unwrap();
        assert!(!edge.empty);
        assert!(edge.triangles >= 2);
        let air = CellGrid::new([40, 10, 10]);
        let e = m.mesh_chunk(&input(&air, &[], &[]), RenderChunkCoord { x: 1, y: 0, z: 0 }).unwrap();
        assert!(e.empty && e.triangles == 0 && e.volume.bytes.iter().all(|&b| b == 0));
    }

    #[test]
    fn bad_input_is_an_error_not_a_panic() {
        let g = hull([40, 20, 24]);
        let mut m = ChunkMesher::new();
        let short = MeshInput { view: CellGridView { size: g.size, cells: &g.cells[..10], damage: &[], sealed_air: &[], scorch: &[] }, palette: &[], wear_seed: 0, design_waterline_y: 0 };
        assert_eq!(m.mesh_chunk(&short, RenderChunkCoord::default()).unwrap_err(), MeshError::ViewLength);
        assert_eq!(m.mesh_all(&short).unwrap_err(), MeshError::ViewLength);
        let inp = input(&g, &[], &[]);
        let far = RenderChunkCoord { x: 9, y: 0, z: 0 };
        assert_eq!(m.mesh_chunk(&inp, far).unwrap_err(), MeshError::ChunkOutOfRange(far));
        assert!(m.mesh_chunk_data(&inp, far).is_err());
        let bad_damage = [0u8; 3];
        assert!(m.mesh_chunk(&input(&g, &bad_damage, &[]), RenderChunkCoord::default()).is_err());
        // The mesher still works afterwards.
        assert!(!m.mesh_chunk(&inp, RenderChunkCoord::default()).unwrap().empty);
    }

    #[test]
    fn damage_opens_holes_and_stays_watertight() {
        let g = hull([70, 36, 40]);
        let mut damage = vec![0u8; g.len()];
        for x in 30..36 {
            for y in 0..36 {
                damage[g.index(CellPos::new(x, y, 20)).unwrap().index()] = 8;
            }
        }
        let inp = input(&g, &damage, &[]);
        let mut m = ChunkMesher::new();
        let mut all = MeshData::new();
        let mut intact = MeshData::new();
        for c in all_chunks(g.size) {
            append(&mut all, &m.mesh_chunk_data(&inp, c).unwrap());
            append(&mut intact, &m.mesh_chunk_data(&input(&g, &[], &[]), c).unwrap());
        }
        assert!(is_watertight(&all));
        assert_ne!(all.triangle_count(), intact.triangle_count());
    }

    fn marking(kind: MarkingKind, cell: CellPos, face: Face, size: u8, text: &str) -> Marking { Marking { kind, cell, face, paint: 9, size_cells: size, text: text.into() } }

    #[test]
    fn markings_sit_on_faces_and_read_from_outside() {
        let mut g = CellGrid::new([10, 6, 6]);
        for x in 0..10 {
            for y in 0..4 {
                for z in 1..5 {
                    g.set(CellPos::new(x, y, z), steel(0));
                }
            }
        }
        let ms = [
            marking(MarkingKind::HullNumber, CellPos::new(5, 2, 4), Face::ZPos, 2, "DD 21"),
            marking(MarkingKind::Stripe, CellPos::new(5, 2, 1), Face::ZNeg, 6, ""),
            marking(MarkingKind::Flag, CellPos::new(5, 3, 2), Face::YPos, 2, ""),
            // Covered: the face is inside the hull.
            marking(MarkingKind::Flag, CellPos::new(5, 2, 2), Face::ZPos, 2, ""),
            // Empty cell and out of bounds.
            marking(MarkingKind::Flag, CellPos::new(5, 5, 2), Face::YPos, 2, ""),
            marking(MarkingKind::Flag, CellPos::new(-1, 2, 2), Face::YPos, 2, ""),
            // Blank hull number.
            marking(MarkingKind::HullNumber, CellPos::new(5, 2, 4), Face::ZPos, 2, "  "),
        ];
        let q = marking_quads(&ms, &g);
        assert_eq!(q.len(), 3);
        // Starboard side: +Z normal, reads towards the bow, 2 mm off the face at z = 2.5 m.
        assert_eq!(q[0].normal, [0.0, 0.0, 1.0]);
        assert_eq!(q[0].right, [1.0, 0.0, 0.0]);
        assert!((q[0].centre[2] - 2.502).abs() < 1e-6);
        assert!((q[0].centre[0] - 2.75).abs() < 1e-6 && (q[0].centre[1] - 1.25).abs() < 1e-6);
        assert!((q[0].size_m[1] - 1.0).abs() < 1e-6 && (q[0].size_m[0] - 3.0).abs() < 1e-6);
        assert_eq!(q[0].text, "DD 21");
        assert_eq!(q[0].paint, 9);
        // Port side reads towards the stern.
        assert_eq!(q[1].normal, [0.0, 0.0, -1.0]);
        assert_eq!(q[1].right, [-1.0, 0.0, 0.0]);
        assert_eq!(q[1].size_m, [3.0, 0.5]);
        // Deck: top of the flag to the bow, so the baseline runs to starboard.
        assert_eq!(q[2].normal, [0.0, 1.0, 0.0]);
        assert_eq!(q[2].right, [0.0, 0.0, 1.0]);
        assert_eq!(q[2].size_m, [1.5, 1.0]);
        for m in &q {
            // right is perpendicular to the normal, and (right, up, normal) is right-handed seen from outside.
            let n = m.normal.map(|v| v as f64);
            let r = m.right.map(|v| v as f64);
            assert!(dot(n, r).abs() < 1e-6);
        }
    }

    #[test]
    fn markings_on_shaped_cells_use_the_existing_surface() {
        let table = ShapeFaceTable::build();
        for o in 0..24u8 {
            for kind in [ShapeKind::Wedge, ShapeKind::Corner, ShapeKind::InverseCorner, ShapeKind::SlopeHigh, ShapeKind::SlopeLow] {
                let shape = CellShape::new(kind, o);
                let mut g = CellGrid::new([3, 3, 3]);
                g.set(CellPos::new(1, 1, 1), Cell { shape, ..steel(0) });
                for f in Face::ALL {
                    let q = marking_quads(&[marking(MarkingKind::Flag, CellPos::new(1, 1, 1), f, 1, "")], &g);
                    // An isolated cell always shows something towards every face: its polygon there or its slope.
                    assert_eq!(q.len(), 1, "{kind:?} o{o} {f:?}");
                    let q = &q[0];
                    let n = q.normal.map(|v| v as f64);
                    assert!(dot(n, face_vec(f)) > 0.0);
                    // The centre lies on one of the shape's polygons (in its plane), lifted 2 mm.
                    let on_plane = table.polygons(shape).iter().any(|p| {
                        let pn = p.normal.map(|v| v as f64);
                        let c0 = p.corners[0].map(|v| (v as f64 + 1.0) * 0.5);
                        let d = (0..3).map(|a| (q.centre[a] as f64 - c0[a]) * pn[a]).sum::<f64>();
                        (d - MARKING_LIFT_M).abs() < 1e-5 && dot(pn, n) > 0.999
                    });
                    assert!(on_plane, "{kind:?} o{o} {f:?}");
                }
            }
        }
        // A slope buried on every side it faces is skipped.
        let mut g = CellGrid::new([3, 3, 3]);
        g.cells.iter_mut().for_each(|c| *c = steel(0));
        g.set(CellPos::new(1, 1, 1), Cell { shape: CellShape::new(ShapeKind::Wedge, 0), ..steel(0) });
        assert!(marking_quads(&[marking(MarkingKind::Flag, CellPos::new(1, 1, 1), Face::XPos, 1, "")], &g).is_empty());
        // A malformed grid gives nothing rather than a panic.
        let bad = CellGrid { size: [4, 4, 4], cells: vec![steel(0); 3] };
        assert!(marking_quads(&[marking(MarkingKind::Flag, CellPos::new(1, 1, 1), Face::XPos, 1, "")], &bad).is_empty());
    }
}
