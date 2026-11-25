//! Part and kit rotations: the [`PartRot`] convention, footprint offsets, and the placement transform that
//! carries a kit's parts and sockets into the vessel frame.
//!
//! # Convention
//! A rotation is a tilt followed by a yaw, both about the part's minimum-corner box.
//! - `tilt` (T, read modulo [`TILT_COUNT`]) picks which way the part stands: 0 upright, 1 rolled +90 degrees about
//!   the bow axis (+Y toward +Z), 2 upside down, 3 rolled -90 degrees, 4 nose up (+X toward +Y, a +90 degree turn
//!   about +Z), 5 nose down. Each of the six sends the part's +Y to a different axis direction.
//! - `yaw` (R / Shift+R, read modulo 4) is then a number of +90 degree right-handed quarter turns about the
//!   vertical: one turn carries the part's +X (bow) to -Z (port).
//!
//! Together the 6 x 4 values name each of the 24 cube rotations exactly once, so every orientation a cell shape
//! can take is also reachable by a part. Rotations use the same signed-permutation table as cell shapes
//! ([`crate::cell::orientation_matrix`]): rows are the images of the part's X, Y and Z axes.
use super::{KitDef, PartRot};
use crate::catalogue::Catalogue;
use crate::cell::{orientation_matrix, rotate_orientation, CellPos, Face};
use crate::enums::Axis;
use crate::ids::PartId;

/// Number of distinct tilt states; `PartRot::tilt` is read modulo this.
pub const TILT_COUNT: u8 = 6;
/// Number of distinct yaw states; `PartRot::yaw` is read modulo this.
pub const YAW_COUNT: u8 = 4;

type Mat = [[i8; 3]; 3];

/// The quarter turns each tilt state applies, in order.
const TILTS: [Option<(Axis, i8)>; 6] = [None, Some((Axis::X, 1)), Some((Axis::X, 2)), Some((Axis::X, -1)), Some((Axis::Z, 1)), Some((Axis::Z, -1))];

/// Orientation index (into [`crate::cell::orientation_matrix`]) of a part rotation. Hull-mesh and the
/// simulation orient part models and muzzle axes with it.
pub fn part_rot_orientation(rot: PartRot) -> u8 {
    let mut o = 0u8;
    if let Some((axis, turns)) = TILTS[(rot.tilt % TILT_COUNT) as usize] {
        o = rotate_orientation(o, axis, turns);
    }
    rotate_orientation(o, Axis::Y, (rot.yaw % YAW_COUNT) as i8)
}

/// Signed permutation matrix of a part rotation: rows are the images of the part's X, Y and Z axes.
pub fn part_rot_matrix(rot: PartRot) -> [[i8; 3]; 3] { orientation_matrix(part_rot_orientation(rot)) }

/// The reduced [`PartRot`] (yaw below 4, tilt below 6) whose matrix is `m`; `None` for an improper or invalid
/// matrix.
pub fn part_rot_from_matrix(m: [[i8; 3]; 3]) -> Option<PartRot> {
    (0..TILT_COUNT).flat_map(|tilt| (0..YAW_COUNT).map(move |yaw| PartRot { yaw, tilt })).find(|r| part_rot_matrix(*r) == m)
}

/// The same rotation with yaw and tilt reduced to their canonical ranges.
pub fn normalize_rot(rot: PartRot) -> PartRot { PartRot { yaw: rot.yaw % YAW_COUNT, tilt: rot.tilt % TILT_COUNT } }

/// Footprint extent in cells after rotation.
pub fn rotated_size(size: [u8; 3], rot: PartRot) -> [u8; 3] {
    let m = part_rot_matrix(rot);
    let mut out = [0u8; 3];
    for (k, row) in m.iter().enumerate() {
        for (c, &s) in row.iter().enumerate() {
            if s != 0 {
                out[c] = size[k];
            }
        }
    }
    out
}

/// Map a box-local integer position through `m`, keeping the rotated box at the origin. `ext` is the box extent;
/// positions outside the box map affinely (sockets may sit just outside a kit's parts).
fn map_box(p: [i32; 3], ext: [i32; 3], m: &Mat) -> [i32; 3] {
    let mut out = [0i32; 3];
    for k in 0..3 {
        for c in 0..3 {
            match m[k][c] {
                1 => out[c] = p[k],
                -1 => out[c] = ext[k] - 1 - p[k],
                _ => {}
            }
        }
    }
    out
}

fn sat(v: i32) -> i16 { v.clamp(i16::MIN as i32, i16::MAX as i32) as i16 }
fn arr(p: CellPos) -> [i32; 3] { [p.x as i32, p.y as i32, p.z as i32] }
fn pos(a: [i32; 3]) -> CellPos { CellPos::new(sat(a[0]), sat(a[1]), sat(a[2])) }

pub(super) fn rotate_offset(offset: CellPos, size: [u8; 3], rot: PartRot) -> CellPos {
    let ext = [size[0] as i32, size[1] as i32, size[2] as i32];
    pos(map_box(arr(offset), ext, &part_rot_matrix(rot)))
}

/// Matrix product `a * b` in the row-vector convention: apply `a`, then `b`.
fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut out = [[0i8; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            out[r][c] = (0..3).map(|k| a[r][k] * b[k][c]).sum();
        }
    }
    out
}

const MIRROR_Z: Mat = [[1, 0, 0], [0, 1, 0], [0, 0, -1]];

fn face_dir(f: Face) -> [i8; 3] {
    let (x, y, z) = f.offset();
    [x as i8, y as i8, z as i8]
}
fn dir_face(d: [i8; 3]) -> Face {
    Face::ALL.into_iter().find(|f| face_dir(*f) == d).unwrap_or(Face::YNeg)
}
fn apply_dir(d: [i8; 3], m: &Mat) -> [i8; 3] {
    let mut out = [0i8; 3];
    for k in 0..3 {
        for c in 0..3 {
            out[c] += d[k] * m[k][c];
        }
    }
    out
}

/// One of a kit's parts carried into the vessel frame by a kit placement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedKitPart {
    /// Index into [`KitDef::parts`].
    pub index: usize,
    pub part: PartId,
    /// Minimum-corner cell of the placed footprint (what [`crate::blueprint::PartPlacement::pos`] stores).
    pub pos: CellPos,
    /// Reduced rotation that draws the placed footprint from the part's own box.
    pub rot: PartRot,
    /// Footprint cells, in the part's own x, y, z order.
    pub cells: Vec<CellPos>,
}

/// A kit socket carried into the vessel frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedSocket { pub name: String, pub cell: CellPos, pub face: Face }

/// The affine map from kit-local cells to vessel cells for one placement.
struct KitTransform { min: [i32; 3], ext: [i32; 3], m: Mat, mirrored: bool, origin: [i32; 3] }

impl KitTransform {
    fn cell(&self, p: [i32; 3]) -> [i32; 3] {
        let mut n = [p[0] - self.min[0], p[1] - self.min[1], p[2] - self.min[2]];
        if self.mirrored {
            n[2] = self.ext[2] - 1 - n[2];
        }
        let r = map_box(n, self.ext, &self.m);
        [r[0] + self.origin[0], r[1] + self.origin[1], r[2] + self.origin[2]]
    }
    fn dir(&self, d: [i8; 3]) -> [i8; 3] {
        let d = if self.mirrored { apply_dir(d, &MIRROR_Z) } else { d };
        apply_dir(d, &self.m)
    }
}

/// Kit-local cells of each part the catalogue knows, by kit part index.
fn local_parts(kit: &KitDef, catalogue: &Catalogue) -> Vec<(usize, Vec<[i32; 3]>)> {
    let mut out = Vec::with_capacity(kit.parts.len());
    for (i, kp) in kit.parts.iter().enumerate() {
        let Some(def) = catalogue.part(&kp.part) else { continue };
        let size = def.size;
        let mut cells = Vec::with_capacity(size[0] as usize * size[1] as usize * size[2] as usize);
        for x in 0..size[0] {
            for y in 0..size[1] {
                for z in 0..size[2] {
                    let o = arr(rotate_offset(CellPos::new(x as i16, y as i16, z as i16), size, kp.rot));
                    cells.push([kp.offset.x as i32 + o[0], kp.offset.y as i32 + o[1], kp.offset.z as i32 + o[2]]);
                }
            }
        }
        out.push((i, cells));
    }
    out
}

fn transform(local: &[(usize, Vec<[i32; 3]>)], pos: CellPos, rot: PartRot, mirrored: bool) -> KitTransform {
    let mut lo = [i32::MAX; 3];
    let mut hi = [i32::MIN; 3];
    for c in local.iter().flat_map(|(_, cells)| cells) {
        for k in 0..3 {
            lo[k] = lo[k].min(c[k]);
            hi[k] = hi[k].max(c[k]);
        }
    }
    // A kit with no known parts (an officer kit) is a point at its local origin.
    let (min, ext) = if lo[0] == i32::MAX { ([0; 3], [1; 3]) } else { (lo, [hi[0] - lo[0] + 1, hi[1] - lo[1] + 1, hi[2] - lo[2] + 1]) };
    KitTransform { min, ext, m: part_rot_matrix(rot), mirrored, origin: arr(pos) }
}

/// Every part of `kit` placed at `pos` (the minimum corner of the placed kit's part footprint) with `rot` and,
/// when `mirrored`, reflected across the kit's own z centre plane before rotating. Parts the catalogue does not
/// know are skipped. A mirrored part keeps a proper rotation: its box is symmetric, so the reflection only
/// flips the part's starboard axis, and its bow and up axes land where the mirror image puts them.
///
/// Kit-local cells are each part's `offset` plus its rotated footprint; the kit's box is the bounding box of
/// those cells. The editor ghost, the placement command and collision checks all use this one transform.
pub fn kit_part_placements(kit: &KitDef, catalogue: &Catalogue, pos: CellPos, rot: PartRot, mirrored: bool) -> Vec<PlacedKitPart> {
    let local = local_parts(kit, catalogue);
    let t = transform(&local, pos, rot, mirrored);
    local
        .into_iter()
        .map(|(index, cells)| {
            let kp = &kit.parts[index];
            let cells: Vec<CellPos> = cells.into_iter().map(|c| self::pos(t.cell(c))).collect();
            let mut min = CellPos::new(i16::MAX, i16::MAX, i16::MAX);
            for c in &cells {
                min = CellPos::new(min.x.min(c.x), min.y.min(c.y), min.z.min(c.z));
            }
            if cells.is_empty() {
                min = self::pos(t.cell(arr(kp.offset)));
            }
            let p = part_rot_matrix(kp.rot);
            let q = if mirrored { mul(&mul(&mul(&MIRROR_Z, &p), &MIRROR_Z), &t.m) } else { mul(&p, &t.m) };
            let rot = part_rot_from_matrix(q).unwrap_or_default();
            PlacedKitPart { index, part: kp.part.clone(), pos: min, rot, cells }
        })
        .collect()
}

/// Every socket of `kit` under the same placement transform as [`kit_part_placements`].
pub fn kit_sockets_at(kit: &KitDef, catalogue: &Catalogue, pos: CellPos, rot: PartRot, mirrored: bool) -> Vec<PlacedSocket> {
    let local = local_parts(kit, catalogue);
    let t = transform(&local, pos, rot, mirrored);
    kit.sockets
        .iter()
        .map(|s| PlacedSocket { name: s.name.clone(), cell: self::pos(t.cell(arr(s.offset))), face: dir_face(t.dir(face_dir(s.face))) })
        .collect()
}

pub(super) fn kit_footprint(kit: &KitDef, catalogue: &Catalogue, pos: CellPos, rot: PartRot, mirrored: bool) -> Vec<CellPos> {
    let mut cells: Vec<CellPos> = kit_part_placements(kit, catalogue, pos, rot, mirrored).into_iter().flat_map(|p| p.cells).collect();
    cells.sort();
    cells.dedup();
    cells
}
