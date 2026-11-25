//! Unit `hm-greedy` (hard: T-junction splitting and watertightness). Cube faces of one chunk through
//! binary-greedy-meshing 0.6.2. DESIGN.md L1921 (greedy cuts a solid 8^3 block from 3,072 quads to 6),
//! L1925 (`Mesher::<32>`, vendored if a 32-chunk equality test fails; class buffer; tests cover triangle count,
//! watertightness and byte layout), L1945 (T-junction cracks are fixed by splitting quads in the mesher, never
//! in the vertex shader).
//!
//! bgm seam (checked against the 0.6.2 source): `Mesher::<32>::new()`, `clear()`, `fast_mesh(&voxels,
//! &opaque_mask, &trans_mask)` with `voxels.len() == 34^3` in ZXY order; output `quads: [Vec<Quad>; 6]` in
//! face order Up, Down, Right, Left, Front, Back; `Quad` packs `(voxel_id << 32) | (h << 24) | (w << 18) |
//! (z << 12) | (y << 6) | x` and `Face::vertices_packed(quad)` gives the 4 corners. The implementer pins the
//! mapping of bgm's Right/Left/Front/Back to content's +X bow, +Z starboard in a unit test.
//!
//! Rules:
//! - Drop every quad whose voxel id is `class::SEALED_AIR` or a shaped class (the shaped pass owns those).
//! - Emit vessel-local metres: chunk origin `coord * 16 m` plus cell coordinates times 0.5 m.
//! - Normals are the exact axis normal of the face.
//! - Split merged quads wherever a vertex of an adjacent coplanar or perpendicular quad (including shaped-pass
//!   faces and the neighbouring chunk's boundary quads) lies on their edge, so the mesh is crack-free.
//! - If the 32-chunk equality test against a naive per-face mesher fails, vendor bgm under this directory.
use crate::chunk::{class, ClassBuffer};
use crate::types::{MeshData, CELL_M_F32, CHUNK, CHUNK_P, CHUNK_P3};
use binary_greedy_meshing::{Face as BgmFace, Mesher};
use content::cell::Face;

mod tjunction;
#[cfg(test)]
pub(crate) mod tests;

pub use tjunction::{ON_EDGE_EPS_M, SEAM_STEP_M};
#[cfg(test)]
pub(crate) use tjunction::{fix_with_general_index, takes_coarse_path, takes_fast_path, vertices_inside_shared_edges};

/// Cell lattice coordinate to metres (exact in f32 for any grid coordinate).
#[inline]
pub(crate) fn cell_m(c: u32) -> f32 { c as f32 * CELL_M_F32 }

/// Content face of an exact axis normal. bgm's faces map straight onto content's frame (pinned by a test):
/// Up +Y, Down -Y, Right +X (bow), Left -X, Front +Z (starboard), Back -Z.
pub(crate) fn face_of_normal(n: [i32; 3]) -> Option<Face> {
    Some(match n {
        [1, 0, 0] => Face::XPos,
        [-1, 0, 0] => Face::XNeg,
        [0, 1, 0] => Face::YPos,
        [0, -1, 0] => Face::YNeg,
        [0, 0, 1] => Face::ZPos,
        [0, 0, -1] => Face::ZNeg,
        _ => return None,
    })
}

/// Unit normal of a face as f32.
pub(crate) fn face_normal(f: Face) -> [f32; 3] {
    let (x, y, z) = f.offset();
    [x as f32, y as f32, z as f32]
}

/// In-plane axes (u, v) of a face with u x v = outward normal.
fn face_uv(f: Face) -> (usize, usize) {
    match f {
        Face::XPos => (1, 2),
        Face::XNeg => (2, 1),
        Face::YPos => (2, 0),
        Face::YNeg => (0, 2),
        Face::ZPos => (0, 1),
        Face::ZNeg => (1, 0),
    }
}

/// Append an axis-aligned rectangle on face direction `f` spanning `lo..hi` (one axis flat), clockwise from
/// outside: two triangles, four vertices.
pub(crate) fn push_rect(out: &mut MeshData, f: Face, lo: [f32; 3], hi: [f32; 3]) {
    let (u, v) = face_uv(f);
    let p0 = lo;
    let mut pu = lo;
    pu[u] = hi[u];
    let mut pv = lo;
    pv[v] = hi[v];
    let mut puv = pu;
    puv[v] = hi[v];
    // Counter-clockwise seen from outside is p0, pu, puv, pv; Godot wants clockwise.
    push_fan(out, &[p0, pv, puv, pu], face_normal(f));
}

/// Append a convex polygon already ordered clockwise from outside as a triangle fan with one flat normal.
pub(crate) fn push_fan(out: &mut MeshData, pts: &[[f32; 3]], n: [f32; 3]) {
    if pts.len() < 3 {
        return;
    }
    let base = out.positions.len() as u32;
    for p in pts {
        out.positions.push(*p);
        out.normals.push(n);
    }
    for k in 1..pts.len() as u32 - 1 {
        out.indices.extend_from_slice(&[base, base + k, base + k + 1]);
    }
}

/// Reusable greedy mesher state for one meshing thread. Holds bgm's buffers so a chunk remesh allocates nothing.
pub struct GreedyMesher {
    pub mesher: Mesher<32>,
}
impl GreedyMesher {
    pub fn new() -> Self { Self { mesher: Mesher::<32>::new() } }
    /// Append the cube faces of `buf` to `out` (positions, normals, indices; no colours) in vessel-local metres.
    /// Unit: `hm-greedy`.
    ///
    /// A buffer whose slices are not the padded sizes is skipped (bgm would index out of bounds).
    pub fn mesh_cubes(&mut self, buf: &ClassBuffer, out: &mut MeshData) {
        if buf.empty || buf.voxels.len() != CHUNK_P3 || buf.opaque_mask.len() != CHUNK_P * CHUNK_P || buf.trans_mask.len() != CHUNK_P * CHUNK_P {
            return;
        }
        self.mesher.clear();
        self.mesher.fast_mesh(&buf.voxels, &buf.opaque_mask, &buf.trans_mask);
        let origin = [buf.coord.x as u32 * CHUNK as u32, buf.coord.y as u32 * CHUNK as u32, buf.coord.z as u32 * CHUNK as u32];
        for (fi, quads) in self.mesher.quads.iter().enumerate() {
            let face = BgmFace::from(fi as u8);
            let n = face.n();
            let Some(dir) = face_of_normal(n) else { continue };
            for q in quads {
                let id = q.voxel_id();
                if id == 0 || id > u16::MAX as u64 {
                    continue;
                }
                let id = id as u16;
                if id == class::SEALED_AIR || class::is_shaped(id) {
                    continue;
                }
                let vs = face.vertices_packed(*q);
                let mut lo = [u32::MAX; 3];
                let mut hi = [0u32; 3];
                for v in vs {
                    let p = v.xyz();
                    for k in 0..3 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
                let to_m = |p: [u32; 3]| [cell_m(origin[0] + p[0]), cell_m(origin[1] + p[1]), cell_m(origin[2] + p[2])];
                push_rect(out, dir, to_m(lo), to_m(hi));
            }
        }
    }
}
impl Default for GreedyMesher { fn default() -> Self { Self::new() } }

/// Split quads so no vertex lies on another triangle's edge (L1945). Operates on the finished chunk mesh
/// (greedy plus shaped faces) and keeps flat normals. Unit: `hm-greedy`.
///
/// Each triangle edge gains every mesh vertex lying strictly inside it, and the triangle is re-fanned from the
/// corners opposite the split edges, so no zero-area triangle and no new interior vertex appears. Inner fan
/// diagonals (an edge the neighbouring triangle in the index list shares by vertex index) are left alone: no
/// vertex of a voxel surface lies inside a face's own polygon. Axis-aligned
/// edges match points exactly (cell-lattice coordinates are exact in f32); sloped edges of shaped faces match
/// within [`ON_EDGE_EPS_M`], since 1:2 slopes can cross diagonals at thirds and fifths of a cell.
///
/// Chunk seams: the neighbouring chunk's vertices are not in this mesh, so every axis-aligned edge lying in a
/// chunk boundary plane (a coordinate that is a positive multiple of 16 m) is also split at every
/// [`SEAM_STEP_M`] lattice point. Both chunks apply the same rule, so their boundary vertices coincide. Plane 0
/// is the grid's own minimum face with no chunk beyond it and is not split.
///
/// Chunk meshes take a bitset fast path over the quarter-metre lattice; a mesh spanning more than 32 m on an
/// axis uses the general point index, with the same result.
pub fn fix_t_junctions(mesh: &mut MeshData) { tjunction::fix(mesh) }
