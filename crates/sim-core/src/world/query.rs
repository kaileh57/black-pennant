//! Unit `sim-core-world-query`. Read-only spatial queries on the world and the rendering feeds.
//! DESIGN.md L1097 (UI picking by Rust queries), L436 and L604 (line of sight, smoke), L205 and L188 (mount
//! arcs), L1088 (hull chunk meshes rebuilt only when cells change), L949 (direction-only manual shots).
//!
//! Decisions:
//! - Cells are tested as whole 0.5 m cubes: a shaped cell (wedge, corner) picks and blocks as its cube.
//! - A vessel's position for [`World::locate`] is its centre of mass, and its velocity the velocity there.
//!   A decoy reports no velocity and its owner's side.
//! - Wreck pieces pick and block like hulls. Decoys cannot be picked.
//! - [`World::line_of_sight`] tests land, hull cells and smoke only. The sea surface and the horizon are the
//!   sensor laws' business (L425).
//! - [`World::mount_path_clear`] follows the path of the shell itself: one ray from the mount's footprint
//!   centre (where shells leave, L2489) along the barrel to the edge of the grid, ignoring the mount's own
//!   cells. A slot that is not on the prefab has no clear path.
use super::{PickHit, World};
use crate::entity::EntityId;
use crate::grid::VesselGrid;
use crate::math::{Aabb, Pose};
use crate::vessel::VesselPrefab;
use content::catalogue::Catalogue;
use content::cell::{CellChange, CellPos, RenderChunkCoord, CELL_M};
use content::ids::{PartSlot, SideId};
use glam::DVec3;

/// Longest pick ray in metres (the sortie world is at most 100 km across, L1133).
const MAX_PICK_M: f64 = 150_000.0;
/// Smallest sphere a body is picked with, metres.
const MIN_PICK_RADIUS_M: f64 = 0.25;

/// Parameters `[t0, t1]` in 0..=1 over which the segment `a -> b` lies inside a box, if it touches it.
fn segment_box(a: DVec3, b: DVec3, min: DVec3, max: DVec3) -> Option<(f64, f64)> {
    if !(a.is_finite() && b.is_finite() && min.is_finite() && max.is_finite()) { return None; }
    let d = b - a;
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for k in 0..3 {
        let (o, dk, lo, hi) = (a[k], d[k], min[k], max[k]);
        if lo > hi { return None; }
        if dk == 0.0 {
            if o < lo || o > hi { return None; }
        } else {
            let (mut ta, mut tb) = ((lo - o) / dk, (hi - o) / dk);
            if ta > tb { core::mem::swap(&mut ta, &mut tb); }
            t0 = t0.max(ta);
            t1 = t1.min(tb);
            if t0 > t1 { return None; }
        }
    }
    Some((t0, t1))
}

/// First live solid cell of a grid along the world segment `a -> b`: the cell and the parameter in 0..=1 at
/// which the segment enters it.
fn first_solid(grid: &VesselGrid, pose: &Pose, a: DVec3, b: DVec3) -> Option<(CellPos, f64)> {
    let (la, lb) = (pose.to_local(a) / CELL_M, pose.to_local(b) / CELL_M);
    if !(la.is_finite() && lb.is_finite()) { return None; }
    let mut hit: Option<CellPos> = None;
    grid.traverse(la.to_array(), lb.to_array(), &mut |p, _| {
        if grid.is_solid(p) { hit = Some(p); false } else { true }
    });
    let p = hit?;
    let lo = DVec3::new(p.x as f64, p.y as f64, p.z as f64);
    // The traversal found the cell, so the segment meets its cube; rounding at a grazing edge reads as entry at 0.
    let t = segment_box(la, lb, lo, lo + DVec3::ONE).map_or(0.0, |(t0, _)| t0);
    Some((p, t.clamp(0.0, 1.0)))
}

fn box_hit(a: DVec3, b: DVec3, bounds: &Aabb) -> bool { segment_box(a, b, bounds.min, bounds.max).is_some() }

/// Nearest parameter in 0..=1 at which the segment `a -> b` is within `r` of `c`.
fn segment_sphere(a: DVec3, b: DVec3, c: DVec3, r: f64) -> Option<f64> {
    let d = b - a;
    let m = a - c;
    let (qa, qb, qc) = (d.length_squared(), m.dot(d), m.length_squared() - r * r);
    if qc <= 0.0 { return Some(0.0); }
    if qa <= 0.0 { return None; }
    let disc = qb * qb - qa * qc;
    if disc < 0.0 { return None; }
    let t = (-qb - disc.sqrt()) / qa;
    (0.0..=1.0).contains(&t).then_some(t)
}

/// True when the segment `a -> b` passes through an upright cylinder (geometric centre, radius, height).
fn segment_cylinder(a: DVec3, b: DVec3, centre: DVec3, radius: f64, height: f64) -> bool {
    let d = b - a;
    // Clip to the slab between the cylinder's ends.
    let (lo, hi) = (centre.y - 0.5 * height, centre.y + 0.5 * height);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    if d.y == 0.0 {
        if a.y < lo || a.y > hi { return false; }
    } else {
        let (mut ta, mut tb) = ((lo - a.y) / d.y, (hi - a.y) / d.y);
        if ta > tb { core::mem::swap(&mut ta, &mut tb); }
        t0 = t0.max(ta);
        t1 = t1.min(tb);
        if t0 > t1 { return false; }
    }
    // Within the slab, the nearest approach to the axis in plan.
    let (mx, mz) = (a.x - centre.x, a.z - centre.z);
    let qa = d.x * d.x + d.z * d.z;
    let t = if qa > 0.0 { (-(mx * d.x + mz * d.z) / qa).clamp(t0, t1) } else { t0 };
    let (px, pz) = (mx + d.x * t, mz + d.z * t);
    px * px + pz * pz <= radius * radius
}

impl World {
    /// Position, velocity and side of any entity (vessel centre of mass, body or decoy position).
    /// Unit: `sim-core-world-query`.
    ///
    /// A body or decoy that ended this tick still resolves until the next tick frees it.
    pub fn locate(&self, id: EntityId) -> Option<(DVec3, DVec3, SideId)> { super::step::locate_in(&self.entities, &self.vessels, &self.bodies, &self.decoys, id) }

    /// Nearest hit of a world ray against vessels (cell-exact) and bodies (spheres), for UI picking and the
    /// direction-only manual shot (L949). Unit: `sim-core-world-query`.
    ///
    /// `dir` need not be unit length. `max_m` is clamped to 150 km; a non-finite ray or a zero direction hits
    /// nothing. On equal distance a vessel wins over a body and the lower slot over the higher.
    pub fn pick(&self, origin: DVec3, dir: DVec3, max_m: f64) -> Option<PickHit> {
        if !(origin.is_finite() && dir.is_finite() && max_m > 0.0) { return None; }
        let dir = dir.try_normalize()?;
        let reach = max_m.min(MAX_PICK_M);
        let end = origin + dir * reach;
        let mut best: Option<PickHit> = None;
        for (_, v) in self.vessels.iter() {
            if !box_hit(origin, end, &v.bounds) { continue; }
            let Some((p, t)) = first_solid(&v.grid, &v.kin.pose, origin, end) else { continue };
            let distance = t * reach;
            if best.is_none_or(|b| distance < b.distance) {
                best = Some(PickHit { entity: v.id, cell: v.grid.index(p), part: v.grid.cell_type(p).part, point: origin + dir * distance, distance });
            }
        }
        let b = &self.bodies;
        let rows = b.len().min(b.pos.len()).min(b.kind.len()).min(b.phase.len());
        for r in 0..rows {
            if b.phase[r] == crate::munitions::BodyPhase::Dead || !b.pos[r].is_finite() { continue; }
            let radius = self.config.catalogue.munitions.get_index(b.kind[r].index()).map_or(MIN_PICK_RADIUS_M, |(_, d)| if d.radius_m.is_finite() { d.radius_m.max(MIN_PICK_RADIUS_M) } else { MIN_PICK_RADIUS_M });
            let Some(t) = segment_sphere(origin, end, b.pos[r], radius) else { continue };
            let distance = t * reach;
            if best.is_none_or(|h| distance < h.distance) {
                best = Some(PickHit { entity: b.entity[r], cell: None, part: None, point: origin + dir * distance, distance });
            }
        }
        best
    }

    /// True when the straight line between two world points is clear of land, hull cells of vessels other than
    /// `ignore`, and smoke cylinders when `optical` (L436, L604). Unit: `sim-core-world-query`.
    ///
    /// Non-finite points are never in sight of each other.
    pub fn line_of_sight(&self, a: DVec3, b: DVec3, optical: bool, ignore: [EntityId; 2]) -> bool {
        if !(a.is_finite() && b.is_finite()) { return false; }
        if self.terrain.segment_blocked(a, b) { return false; }
        for (_, v) in self.vessels.iter() {
            if ignore.contains(&v.id) || !box_hit(a, b, &v.bounds) { continue; }
            if first_solid(&v.grid, &v.kin.pose, a, b).is_some() { return false; }
        }
        if optical {
            let tuning = &self.config.catalogue.tuning.decoys;
            for r in 0..self.decoys.len() {
                if let Some((centre, radius, height)) = self.decoys.smoke_cylinder(r, tuning) {
                    if segment_cylinder(a, b, centre, radius, height) { return false; }
                }
            }
        }
        true
    }

    /// Sweep a mount's shell path against its own vessel's structure at a training and elevation, for auto-set
    /// limits and blocked-arc notes (L205, L188). Returns true when clear. Unit: `sim-core-world-query`.
    ///
    /// Angles are in the mount frame of `parts::mount_dir_local`. Non-finite angles are not clear.
    pub fn mount_path_clear(prefab: &VesselPrefab, slot: PartSlot, train_rad: f64, elevation_rad: f64, catalogue: &Catalogue) -> bool {
        let _ = catalogue;
        let Some(part) = prefab.parts.get(slot.index()).filter(|p| p.slot == slot) else { return false };
        if !(train_rad.is_finite() && elevation_rad.is_finite() && part.centre.is_finite()) { return false; }
        let dir = crate::parts::mount_dir_local(crate::parts::mount_zero_rad(part.rot), train_rad, elevation_rad);
        let grid = &prefab.grid;
        let size = grid.size();
        let reach = size[0] as f64 + size[1] as f64 + size[2] as f64 + 2.0;
        let from = part.centre / CELL_M;
        let to = from + dir * reach;
        let mut clear = true;
        grid.traverse(from.to_array(), to.to_array(), &mut |p, _| {
            if !grid.is_solid(p) { return true; }
            // The shell starts inside the mount: its own footprint does not block it.
            let own = grid.index(p).is_some_and(|i| part.cells.binary_search(&i).is_ok());
            if !own { clear = false; }
            own
        });
        clear
    }

    /// Take a vessel's pending cell changes (also emitted as `CellsDamaged` events; this feed is for local
    /// remeshing when the client hosts the sim). Unit: `sim-core-world-query`.
    pub fn drain_cell_changes(&mut self, vessel: EntityId) -> Vec<CellChange> { self.vessel_mut(vessel).map_or(Vec::new(), |v| v.grid.drain_changes()) }
    /// Take the render chunks of a vessel that need remeshing, ascending. Unit: `sim-core-world-query`.
    pub fn drain_dirty_render_chunks(&mut self, vessel: EntityId) -> Vec<RenderChunkCoord> { self.vessel_mut(vessel).map_or(Vec::new(), |v| v.grid.drain_dirty_render()) }
}
