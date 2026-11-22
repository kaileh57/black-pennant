//! Unit `hm-shaped` (hard). Faces of shaped cells: cube, 45 degree wedge, corner and inverse corner in 24
//! orientations plus placeable 1:2 slopes, from a face table with neighbour-cover masks.
//! DESIGN.md L1923 (shapes; curved hulls read as faceted steel, not stairs), L1925 (per-cell pass emits shaped
//! faces from a face table with neighbour-cover masks), L1815 (normals only from the shape table).
//!
//! Geometry comes from `content::cell::orientation_matrix` (the single source of orientations, shared with
//! armour obliquity) and `content::cell::full_face_mask`.
//!
//! Approach the contract expects (the implementer may refine it if the tests pass):
//! - bgm sees shaped cells as opaque, so cube neighbours emit no face toward them; bgm's own quads for shaped
//!   voxel ids are dropped by `hm-greedy`.
//! - For each shaped cell: emit its sloped faces always; emit each of its six cell-face polygons (full square,
//!   triangle or none per the table) unless the neighbour fully covers that face.
//! - For each cube or shaped neighbour of a shaped cell whose touching face is not fully covered by the shaped
//!   cell, emit the neighbour's square face (cube) or polygon (shaped), since bgm suppressed it.
//! - Result: watertight, no hidden faces where two full faces meet.
//!
//! What this pass emits, per chunk:
//! - every sloped polygon of each interior shaped cell;
//! - on each cell face between an interior shaped cell A and its neighbour B, the exact part of A's face polygon
//!   that B's face polygon does not cover (`A - B`), and on the faces of interior cube-class cells next to a
//!   shaped cell (interior or padding), the part of the full square the shaped cell leaves open. Every face
//!   region of these solids is the square cut by at most one line, so each difference is a convex polygon and
//!   the union of both sides is the exact boundary of the two solids: no hole, no hidden face where two full
//!   faces meet.
//! - Exposed air covers nothing, cube classes cover the whole face. Sealed air counts as a full cover, and an
//!   interior sealed-air cell next to a shaped cell gets the face the shaped cell's open side exposes, so the open
//!   corner of a shaped cell never shows into a sealed compartment (sealed air otherwise has no faces).
//!
//! Crossing points of two cut lines that are not binary fractions (1:2 slope against a diagonal meets at 2/3
//! of a cell) are snapped to a grid of 1/720720 of a cell, the common denominator of every possible crossing,
//! so both cells of a pair, possibly in different chunks, compute bit-identical vertices.
use crate::chunk::{class, ClassBuffer};
use crate::types::{MeshData, CHUNK, CHUNK_P, CHUNK_P3};
use content::cell::{full_face_mask, orientation_matrix, CellShape, Face, ShapeKind};

mod geom;
#[cfg(test)]
mod tests;

use geom::P2;

/// One polygon of a canonical (orientation 0) shape in unit-cell coordinates 0..=1, clockwise from outside.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapePolygon {
    /// 3 or 4 corners.
    pub corners: Vec<[f32; 3]>,
    pub normal: [f32; 3],
    /// The cell face this polygon lies on, or `None` for a sloped face inside the cell.
    pub on_face: Option<Face>,
}

/// Face table for every shape byte (kind x orientation), built once.
#[derive(Clone, Debug)]
pub struct ShapeFaceTable {
    /// Indexed by the shape byte (`CellShape.0`); unused bytes hold empty entries.
    pub polygons: Vec<Vec<ShapePolygon>>,
    /// Faces each shape covers fully (bit per `Face::ALL` order); equals `content::cell::full_face_mask`.
    pub full_mask: Vec<u8>,
    /// Per shape byte and face (`Face::ALL` order): the face region in the face's 2D frame.
    pub(crate) regions: Vec<[Region; 6]>,
    /// Per shape byte and face: id of its region in `region_list` (0 none, 1 full).
    pub(crate) region_ids: Vec<[u8; 6]>,
    /// Distinct face regions over every shape and face, equal polygons interned once.
    pub(crate) region_list: Vec<Region>,
    /// `a - b` for every pair of region ids, at `a * region_list.len() + b`, so the per-cell pass is lookup only.
    pub(crate) pair_pieces: Vec<Vec<Piece>>,
}

/// One convex piece of a face-region difference, in the face's 2D frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Piece {
    pub pts: Vec<P2>,
    /// Counter-clockwise in the 2D frame (positive signed area).
    pub ccw: bool,
}

/// Region ids every table starts with.
const REGION_NONE: u8 = 0;
const REGION_FULL: u8 = 1;

/// What a cell puts on one of its faces.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Region {
    None,
    Full,
    /// A convex part of the face in the face's 2D frame ([`geom::project`]).
    Part(Vec<P2>),
}
impl ShapeFaceTable {
    /// Build the table from the canonical shapes and `content::cell::orientation_matrix`. Unit: `hm-shaped`.
    ///
    /// Every byte gets the geometry `CellShape` reads it as: kind bits 6 and 7 read as a cube and orientations
    /// 24..=31 as orientation 0, as in content.
    pub fn build() -> ShapeFaceTable {
        let kinds = [ShapeKind::Cube, ShapeKind::Wedge, ShapeKind::Corner, ShapeKind::InverseCorner, ShapeKind::SlopeLow, ShapeKind::SlopeHigh];
        let canon: Vec<Vec<ShapePolygon>> = kinds.iter().map(|k| geom::canonical_polygons(*k)).collect();
        let mut polygons = Vec::with_capacity(256);
        let mut full_mask = Vec::with_capacity(256);
        let mut regions = Vec::with_capacity(256);
        for b in 0..=255u8 {
            let s = CellShape(b);
            let m = orientation_matrix(s.orientation());
            let polys: Vec<ShapePolygon> = canon[s.kind() as usize].iter().map(|p| geom::rotate_polygon(p, &m)).collect();
            let mask = full_face_mask(s);
            regions.push(Face::ALL.map(|f| {
                if mask & (1 << f as u8) != 0 {
                    return Region::Full;
                }
                match polys.iter().find(|p| p.on_face == Some(f)) {
                    Some(p) => Region::Part(geom::project(&p.corners, axis(f))),
                    None => Region::None,
                }
            }));
            polygons.push(polys);
            full_mask.push(mask);
        }
        let mut region_list = vec![Region::None, Region::Full];
        let region_ids: Vec<[u8; 6]> = regions.iter().map(|rs| rs.clone().map(|r| intern(&mut region_list, r))).collect();
        let n = region_list.len();
        let mut pair_pieces = Vec::with_capacity(n * n);
        let mut scratch = Vec::new();
        for a in &region_list {
            for b in &region_list {
                difference(a, b, &mut scratch);
                pair_pieces.push(scratch.drain(..).map(|pts| { let ccw = geom::signed_area2(&pts) > 0.0; Piece { pts, ccw } }).collect());
            }
        }
        ShapeFaceTable { polygons, full_mask, regions, region_ids, region_list, pair_pieces }
    }

    /// Polygons of one shape, rotated into the cell frame. Unit: `hm-shaped`.
    pub fn polygons(&self, shape: CellShape) -> &[ShapePolygon] { self.polygons.get(shape.0 as usize).map_or(&[], |v| v.as_slice()) }

    /// Region of a shape on face `f`.
    fn region(&self, shape: CellShape, f: Face) -> &Region {
        static NONE: Region = Region::None;
        self.regions.get(shape.0 as usize).map_or(&NONE, |r| &r[f as usize])
    }

    /// Interned region id of a shape on face `f`.
    #[inline]
    fn region_id(&self, shape: CellShape, f: Face) -> u8 { self.region_ids.get(shape.0 as usize).map_or(REGION_NONE, |r| r[f as usize]) }

    /// What a neighbour of class `v` puts on its face `f`, as a region id (see [`cover`]).
    #[inline]
    fn cover_id(&self, v: u16, f: Face) -> u8 {
        if v == class::EXPOSED_AIR {
            REGION_NONE
        } else if class::is_shaped(v) {
            self.region_id(CellShape(class::shape_byte(v)), f)
        } else {
            REGION_FULL
        }
    }

    /// Pieces of region `a` minus region `b`.
    #[inline]
    fn pieces(&self, a: u8, b: u8) -> &[Piece] {
        let n = self.region_list.len();
        self.pair_pieces.get(a as usize * n + b as usize).map_or(&[], |v| v.as_slice())
    }
}

/// Id of `r` in `list`, appending it when no equal region is there yet.
fn intern(list: &mut Vec<Region>, r: Region) -> u8 {
    let found = list.iter().position(|q| match (q, &r) {
        (Region::None, Region::None) | (Region::Full, Region::Full) => true,
        (Region::Part(a), Region::Part(b)) => geom::same_polygon(a, b),
        _ => false,
    });
    match found {
        Some(i) => i as u8,
        None => {
            list.push(r);
            (list.len() - 1) as u8
        }
    }
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

fn axis(f: Face) -> usize { f as usize / 2 }
fn positive(f: Face) -> bool { f as usize % 2 == 1 }

/// What a neighbour of class `v` puts on its face `f`: nothing for exposed air, the shape's region for shaped
/// cells, the whole face for cube classes and sealed air.
fn cover(table: &ShapeFaceTable, v: u16, f: Face) -> &Region {
    static NONE: Region = Region::None;
    static FULL: Region = Region::Full;
    if v == class::EXPOSED_AIR {
        &NONE
    } else if class::is_shaped(v) {
        table.region(CellShape(class::shape_byte(v)), f)
    } else {
        &FULL
    }
}

/// Append `a - b` as convex pieces.
fn difference(a: &Region, b: &Region, out: &mut Vec<Vec<P2>>) {
    out.clear();
    match (a, b) {
        (Region::None, _) | (_, Region::Full) => {}
        (Region::Full, Region::None) => out.push(geom::unit_square()),
        (Region::Part(p), Region::None) => out.push(p.clone()),
        (Region::Full, Region::Part(q)) => geom::subtract(&geom::unit_square(), q, out),
        (Region::Part(p), Region::Part(q)) => {
            if !geom::same_polygon(p, q) {
                geom::subtract(p, q, out);
            }
        }
    }
}

#[inline]
fn voxel(buf: &ClassBuffer, x: i32, y: i32, z: i32) -> u16 { buf.voxels[ClassBuffer::pad_index(x, y, z)] }

fn interior(p: [i32; 3]) -> bool { p.iter().all(|&c| (0..CHUNK as i32).contains(&c)) }

/// Append the faces of shaped cells in `buf`, and the neighbour faces bgm suppressed against them, to `out` in
/// vessel-local metres. No-op when `buf.has_shaped` is false. Unit: `hm-shaped`.
///
/// Columns whose `buf.opaque_mask` word is zero are skipped (every non-air voxel has its bit set, as
/// `chunk::fill_class_buffer` writes it), so air costs nothing.
pub fn mesh_shaped(table: &ShapeFaceTable, buf: &ClassBuffer, out: &mut MeshData) {
    if !buf.has_shaped || buf.voxels.len() != CHUNK_P3 || buf.opaque_mask.len() != CHUNK_P * CHUNK_P {
        return;
    }
    let origin = [buf.coord.x as i64 * CHUNK as i64, buf.coord.y as i64 * CHUNK as i64, buf.coord.z as i64 * CHUNK as i64];
    let cell_of = |p: [i32; 3]| [origin[0] + p[0] as i64, origin[1] + p[1] as i64, origin[2] + p[2] as i64];
    for py in 0..CHUNK_P {
        for px in 0..CHUNK_P {
            if buf.opaque_mask[px + CHUNK_P * py] == 0 {
                continue;
            }
            let row = CHUNK_P * (px + CHUNK_P * py);
            // Branch-free shaped bits of the column, then visit only those.
            let mut bits = 0u64;
            for (z, &v) in buf.voxels[row..row + CHUNK_P].iter().enumerate() {
                bits |= (class::is_shaped(v) as u64) << z;
            }
            while bits != 0 {
                let pz = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let v = buf.voxels[row + pz];
                let p = [px as i32 - 1, py as i32 - 1, pz as i32 - 1];
                let shape = CellShape(class::shape_byte(v));
                let own_cell = interior(p);
                if own_cell {
                    let c = cell_of(p);
                    for poly in table.polygons(shape).iter().filter(|q| q.on_face.is_none()) {
                        geom::emit_unit_polygon(out, c, poly);
                    }
                }
                for f in Face::ALL {
                    let (dx, dy, dz) = f.offset();
                    let q = [p[0] + dx as i32, p[1] + dy as i32, p[2] + dz as i32];
                    if q.iter().any(|&c| !(-1..=CHUNK as i32).contains(&c)) {
                        continue;
                    }
                    let nv = voxel(buf, q[0], q[1], q[2]);
                    let mine = table.region_id(shape, f);
                    if own_cell {
                        // A - B on A's face f.
                        for piece in table.pieces(mine, table.cover_id(nv, opposite(f))) {
                            geom::emit_face_piece(out, cell_of(p), f, piece);
                        }
                    }
                    if interior(q) && nv != class::EXPOSED_AIR && !class::is_shaped(nv) {
                        // Cube-class or sealed-air neighbour: its face toward us minus what we cover.
                        for piece in table.pieces(REGION_FULL, mine) {
                            geom::emit_face_piece(out, cell_of(q), opposite(f), piece);
                        }
                    }
                }
            }
        }
    }
}

/// The pre-table form of [`mesh_shaped`] (polygon clipping per cell face), kept as the test oracle.
#[cfg(test)]
pub(crate) fn mesh_shaped_reference(table: &ShapeFaceTable, buf: &ClassBuffer, out: &mut MeshData) {
    if !buf.has_shaped || buf.voxels.len() != CHUNK_P3 {
        return;
    }
    let origin = [buf.coord.x as i64 * CHUNK as i64, buf.coord.y as i64 * CHUNK as i64, buf.coord.z as i64 * CHUNK as i64];
    let cell_of = |p: [i32; 3]| [origin[0] + p[0] as i64, origin[1] + p[1] as i64, origin[2] + p[2] as i64];
    let mut pieces: Vec<Vec<P2>> = Vec::new();
    let as_piece = |pts: &Vec<P2>| Piece { pts: pts.clone(), ccw: geom::signed_area2(pts) > 0.0 };
    for py in 0..CHUNK_P {
        for px in 0..CHUNK_P {
            for pz in 0..CHUNK_P {
                let v = buf.voxels[pz + CHUNK_P * (px + CHUNK_P * py)];
                if !class::is_shaped(v) {
                    continue;
                }
                let p = [px as i32 - 1, py as i32 - 1, pz as i32 - 1];
                let shape = CellShape(class::shape_byte(v));
                let own_cell = interior(p);
                if own_cell {
                    let c = cell_of(p);
                    for poly in table.polygons(shape).iter().filter(|q| q.on_face.is_none()) {
                        geom::emit_unit_polygon(out, c, poly);
                    }
                }
                for f in Face::ALL {
                    let (dx, dy, dz) = f.offset();
                    let q = [p[0] + dx as i32, p[1] + dy as i32, p[2] + dz as i32];
                    if q.iter().any(|&c| !(-1..=CHUNK as i32).contains(&c)) {
                        continue;
                    }
                    let nv = voxel(buf, q[0], q[1], q[2]);
                    let mine = table.region(shape, f);
                    if own_cell {
                        difference(mine, cover(table, nv, opposite(f)), &mut pieces);
                        for piece in &pieces {
                            geom::emit_face_piece(out, cell_of(p), f, &as_piece(piece));
                        }
                    }
                    if interior(q) && nv != class::EXPOSED_AIR && !class::is_shaped(nv) {
                        difference(&Region::Full, mine, &mut pieces);
                        for piece in &pieces {
                            geom::emit_face_piece(out, cell_of(q), opposite(f), &as_piece(piece));
                        }
                    }
                }
            }
        }
    }
}
