//! Unit `sim-core-projectiles` (hard: hot loop, tunnelling-free collision, exact ordering).
//! Ballistic projectiles as flat arrays. DESIGN.md L557-559, L563 (drag fit), L926 (client integration).
//!
//! Integration: fixed 60 Hz, semi-implicit Euler, gravity 9.81 m/s^2, quadratic drag a = -k |v| v with one k
//! per calibre band; no wind. Ids are assigned in spawn order; spawns within a tick are ordered by
//! (vessel id, mount id). Live cap 2,000 (then 1,500, 1,000 by ladder rung); above the cap one autocannon
//! projectile carries 2 rounds and applies its hit twice. A shell reaching the water surface ends there with a
//! splash event. Point-defence rounds self-destruct 4 s after firing.
//! Broadphase: the segment old -> new position is tested against vessel bounding boxes in a uniform hash grid
//! of 128 m cells. Narrowphase: a segment entering a box is transformed into the vessel's cell frame and
//! stepped with Amanatides-Woo traversal. Hits are sorted by (tick, projectile id) before they apply.
//!
//! # Implementation decisions
//! - One tick's segment ends at the earliest of: the first live solid cell of any hull, the first contact
//!   with a small-body sphere, the water surface, or land. Ties go to hulls (ascending target order), then
//!   spheres, then the water or land end. A shell plunging onto a deck that sits below the local wave crest
//!   therefore still hits the deck when the deck comes first along the segment.
//! - A shell never collides with the hull that fired it: shells spawn at the mount's footprint centre (L2489),
//!   inside the shooter's own grid, and mount limits already exclude arcs that strike own structure (L206).
//! - Any sphere contact (a direct pass, or a point-defence round inside the proximity margin) is a hit: the
//!   shell is spent and a [`BodyGraze`] is reported once. The sweep is relative: the body's centre from the
//!   previous tick (kept in `sphere_prev`) moves linearly to its current centre over the tick.
//! - Land strikes are reported as [`SplashAt`] at the strike point, so clients close the replicated shell
//!   at the right place; a splash whose point stands above the local water level is a land strike.
//! - Above the live cap, merging is tracked per (shooter, mount): the first autocannon round carries 2 rounds
//!   and that mount's next round within one second is dropped. Non-autocannon shells always spawn, up to the
//!   hard safety ceiling [`MAX_LIVE_HARD`].
//! - `v_1km` for a hit closer than 1 km of path is the drag model continued to 1 km:
//!   `v * exp(-k * (1000 - path))` (gravity's share of deceleration is negligible over that distance).
//! - Rows with a non-finite position or velocity, and shells older than [`MAX_FLIGHT_TICKS`], are dropped
//!   without an event.
use crate::entity::EntityId;
use crate::grid::VesselGrid;
use crate::math::{Aabb, Pose};
use crate::time::{DT, TICK_HZ};
use content::cell::{CellPos, CELL_M};
use content::ids::{CellIndex, PartSlot};
use content::part::weapons::{BandDef, CalibreBand, GRAVITY, PEN_EXPONENT};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[cfg(test)]
mod tests;

/// Hard ceiling on live rows, whatever the cap: past it every spawn is refused (protects memory from a
/// runaway spawner; never reached in play, where the cap keeps autocannon fire merged).
pub const MAX_LIVE_HARD: usize = 16_384;
/// Shells older than this are dropped (5 minutes; no gun shell flies that long).
pub const MAX_FLIGHT_TICKS: u64 = 60 * 300;
/// Path length at which `v_1km` is recorded, metres (L563).
pub const V_1KM_PATH_M: f64 = 1_000.0;
/// The wave field is sampled only when a segment dips below this height, metres. Crests of the highest
/// sea state stay far below it.
const WAVE_CEILING_M: f64 = 50.0;
/// A merged-round drop is honoured only within this many ticks of the merge (1 s).
const MERGE_WINDOW_TICKS: u64 = 60;
/// Targets spanning more broadphase cells than this along an axis are tested against every segment.
const MAX_SPAN_CELLS: i64 = 32;
/// Margin added to bounds by [`Projectiles::threatens`], metres.
const THREAT_MARGIN_M: f64 = 1.0;
/// Bisection steps for the land strike point.
const LAND_BISECT_STEPS: u32 = 16;

/// Spawn-order projectile id, unique within a sortie.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectileId(pub u32);

/// The replicated spawn event: clients integrate the shell themselves with the same drag model (L926).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectileSpawn {
    pub id: ProjectileId,
    pub tick: u64,
    pub origin: DVec3,
    pub velocity: DVec3,
    pub calibre_mm: u16,
    pub band: CalibreBand,
    pub shooter: EntityId,
    pub mount: PartSlot,
    /// 1, or 2 when the live cap forced round merging.
    pub rounds: u8,
    pub point_defence: bool,
}

/// Structure-of-arrays storage; row i of every vector is one live projectile, rows ascending by id.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Projectiles {
    pub id: Vec<ProjectileId>,
    pub pos: Vec<DVec3>,
    pub vel: Vec<DVec3>,
    pub band: Vec<CalibreBand>,
    pub calibre_mm: Vec<u16>,
    pub shooter: Vec<EntityId>,
    pub mount: Vec<PartSlot>,
    pub spawn_tick: Vec<u64>,
    /// Speed when 1 km of path had been flown (v_1km of the penetration law, L563); 0 until then.
    pub v_1km: Vec<f64>,
    pub path_m: Vec<f64>,
    pub rounds: Vec<u8>,
    pub point_defence: Vec<bool>,
    pub next_id: u32,
    /// Mounts whose next autocannon round is owed to a merged 2-round projectile: (shooter, mount, merge tick).
    #[serde(default)]
    pub(crate) merge_owed: Vec<(EntityId, PartSlot, u64)>,
    /// Small-body centres seen last tick, ascending by entity, for the relative sphere sweep.
    #[serde(default)]
    pub(crate) sphere_prev: Vec<(EntityId, DVec3)>,
}

/// A shell entering a vessel's grid.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShellHit {
    pub tick: u64,
    pub projectile: ProjectileId,
    pub vessel: EntityId,
    /// Entry point and direction of travel in the vessel's cell frame (cell units).
    pub entry: [f64; 3],
    pub dir: [f64; 3],
    pub first_cell: CellIndex,
    pub speed_mps: f64,
    pub v_1km: f64,
    pub band: CalibreBand,
    pub calibre_mm: u16,
    pub shooter: EntityId,
    pub rounds: u8,
    pub world_point: DVec3,
}

/// A shell passing a small body's sphere (L597).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BodyGraze { pub tick: u64, pub projectile: ProjectileId, pub body: EntityId, pub shooter: EntityId }

/// A shell ending in the sea.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SplashAt { pub projectile: ProjectileId, pub pos: DVec3, pub calibre_mm: u16, pub shooter: EntityId }

/// Broadphase target: one per vessel, ascending vessel slot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HullTarget { pub vessel: EntityId, pub bounds: Aabb, pub pose: crate::math::Pose }
/// Small-body target sphere.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SphereTarget { pub body: EntityId, pub centre: DVec3, pub radius: f64 }

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectileStepOutput { pub hits: Vec<ShellHit>, pub grazes: Vec<BodyGraze>, pub splashes: Vec<SplashAt>, pub expired: Vec<ProjectileId>, pub work_units: u32 }

/// One semi-implicit Euler step of a ballistic shell. This exact function is shared by the server, by clients
/// integrating replicated spawns, and by the gun solver in sim-systems (L518, L926). Unit: `sim-core-projectiles`.
///
/// `v' = v + (g - k |v| v) dt`, then `p' = p + v' dt`. A negative or non-finite `drag_k` is treated as 0.
pub fn ballistic_step(pos: DVec3, vel: DVec3, drag_k: f64, dt: f64) -> (DVec3, DVec3) {
    let k = if drag_k.is_finite() && drag_k > 0.0 { drag_k } else { 0.0 };
    let speed = vel.length();
    let acc = DVec3::new(-k * speed * vel.x, -GRAVITY - k * speed * vel.y, -k * speed * vel.z);
    let v = vel + acc * dt;
    (pos + v * dt, v)
}

/// Penetration in mm at speed v: P_1km * (v / v_1km)^1.4 with libm `pow` (L563). Unit: `sim-core-projectiles`.
///
/// A non-positive or non-finite speed gives 0; a non-positive or non-finite `v_1km` (unknown) gives `P_1km`.
pub fn penetration_mm(band: &BandDef, v: f64, v_1km: f64) -> f64 {
    let p1 = band.p_1km_mm;
    if !(p1.is_finite() && p1 > 0.0) || !(v.is_finite() && v > 0.0) { return 0.0; }
    if !(v_1km.is_finite() && v_1km > 0.0) { return p1; }
    p1 * libm::pow(v / v_1km, PEN_EXPONENT)
}

/// Fit a band's drag k so the ballistic impact velocity at the cap range yields `p_cap_mm` from `p_1km_mm`
/// (bisection on k with the reference muzzle velocity at the best-range elevation). `cargo xtask fit-drag`
/// writes the result into `data/weapons.ron`; CI fails if the committed k misses by over 5% (L563).
/// Unit: `sim-core-projectiles`.
///
/// The shell leaves sea level at `ref_muzzle_mps` on the low elevation that lands it back at sea level at the
/// cap range, integrated with [`ballistic_step`] at the sim tick exactly as in play; `v_1km` is read the same
/// way the runtime records it. The ratio `v_cap / v_1km` falls as k rises, so k is bisected (in log space,
/// 1e-9 to 0.05 per metre) until `(v_cap / v_1km)^1.4 = p_cap / p_1km`; a k at which the cap range cannot
/// be reached counts as too much drag. A degenerate row (non-finite or non-positive inputs, or a far-range
/// target that is not below `p_1km`) returns 0.
pub fn fit_drag_k(band: &BandDef) -> f64 {
    let (p1, pc, cap, v0) = (band.p_1km_mm, band.p_cap_mm, band.cap_range_m, band.ref_muzzle_mps);
    let ok = |x: f64| x.is_finite() && x > 0.0;
    if !(ok(p1) && ok(pc) && ok(cap) && ok(v0)) || pc >= p1 { return 0.0; }
    let target = libm::pow(pc / p1, 1.0 / PEN_EXPONENT);
    let ratio = |k: f64| landing_at_range(v0, k, cap).map(|l| l.v_land / l.v_1km);
    let (mut lo, mut hi) = (1e-9f64, 0.05f64);
    match ratio(lo) {
        Some(r) if r >= target => {}
        _ => return 0.0,
    }
    for _ in 0..48 {
        let mid = libm::sqrt(lo * hi);
        match ratio(mid) {
            Some(r) if r >= target => lo = mid,
            _ => hi = mid,
        }
    }
    libm::sqrt(lo * hi)
}

/// One sea-level-to-sea-level flight.
#[derive(Clone, Copy, Debug)]
struct Landing { range_m: f64, v_land: f64, v_1km: f64 }

/// Speed at the path crossing inside a step, linear in path length (the runtime's `v_1km` rule).
fn speed_at_fraction(s0: f64, s1: f64, f: f64) -> f64 { s0 + (s1 - s0) * f.clamp(0.0, 1.0) }

/// `v_1km` predicted for a shell that has flown less than 1 km of path.
fn predict_v_1km(speed: f64, path_m: f64, k: f64) -> f64 {
    let k = if k.is_finite() && k > 0.0 { k } else { 0.0 };
    speed * libm::exp(-k * (V_1KM_PATH_M - path_m).max(0.0))
}

/// Fly from the origin at sea level with elevation `elev` until the shell falls back through y = 0.
fn fly(v0: f64, elev: f64, k: f64) -> Option<Landing> {
    let mut pos = DVec3::ZERO;
    let mut vel = DVec3::new(v0 * libm::cos(elev), v0 * libm::sin(elev), 0.0);
    let (mut path, mut v1) = (0.0f64, 0.0f64);
    for _ in 0..MAX_FLIGHT_TICKS {
        let (p, v) = ballistic_step(pos, vel, k, DT);
        let seg = (p - pos).length();
        let (s0, s1) = (vel.length(), v.length());
        if v1 == 0.0 && path + seg >= V_1KM_PATH_M && seg > 0.0 { v1 = speed_at_fraction(s0, s1, (V_1KM_PATH_M - path) / seg); }
        if p.y < 0.0 {
            let f = if pos.y - p.y > 0.0 { pos.y / (pos.y - p.y) } else { 0.0 };
            let v_land = speed_at_fraction(s0, s1, f);
            let path_land = path + seg * f;
            if v1 == 0.0 { v1 = predict_v_1km(v_land, path_land, k); }
            if v1.is_nan() || v1 <= 0.0 { return None; }
            return Some(Landing { range_m: pos.x + (p.x - pos.x) * f, v_land, v_1km: v1 });
        }
        path += seg;
        pos = p;
        vel = v;
    }
    None
}

/// The low-elevation flight that lands at `range` (within bisection precision), or `None` if out of reach.
fn landing_at_range(v0: f64, k: f64, range: f64) -> Option<Landing> {
    const STEP: f64 = 0.5 * core::f64::consts::PI / 180.0;
    let first = fly(v0, 0.0, k)?;
    if first.range_m >= range { return Some(first); }
    let (mut prev_e, mut prev_r) = (0.0, first.range_m);
    for i in 1..=90 {
        let e = i as f64 * STEP;
        let l = fly(v0, e, k)?;
        if l.range_m >= range {
            let (mut lo, mut hi, mut best) = (prev_e, e, l);
            for _ in 0..30 {
                let mid = 0.5 * (lo + hi);
                let m = fly(v0, mid, k)?;
                if m.range_m >= range { hi = mid; best = m; } else { lo = mid; }
            }
            return Some(best);
        }
        // Past the maximum-range elevation without reaching the range.
        if l.range_m < prev_r { return None; }
        prev_e = e;
        prev_r = l.range_m;
    }
    None
}

/// Drag k for a band: the table row's committed value, or (row missing) the fit of the spec's starting row.
fn band_drag_k(bands: &[BandDef], band: CalibreBand) -> f64 {
    match bands.iter().find(|b| b.band == band) {
        Some(b) if b.drag_k.is_finite() && b.drag_k > 0.0 => b.drag_k,
        Some(_) => 0.0,
        None => {
            static DEFAULT_K: OnceLock<[f64; 4]> = OnceLock::new();
            let ks = DEFAULT_K.get_or_init(|| CalibreBand::ALL.map(|b| fit_drag_k(&BandDef::spec_default(b))));
            ks[CalibreBand::ALL.iter().position(|b| *b == band).unwrap_or(0)]
        }
    }
}

/// Segment-AABB slab test; returns the entry parameter in [0, 1] if the segment touches the box.
fn segment_aabb(a: DVec3, b: DVec3, min: DVec3, max: DVec3) -> Option<f64> {
    let d = b - a;
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for ax in 0..3 {
        let (o, dd, lo, hi) = (a[ax], d[ax], min[ax], max[ax]);
        if lo.is_nan() || hi.is_nan() || lo > hi { return None; }
        if dd == 0.0 {
            if o < lo || o > hi { return None; }
        } else {
            let inv = 1.0 / dd;
            let (mut ta, mut tb) = ((lo - o) * inv, (hi - o) * inv);
            if ta > tb { core::mem::swap(&mut ta, &mut tb); }
            t0 = t0.max(ta);
            t1 = t1.min(tb);
            if t0 > t1 { return None; }
        }
    }
    Some(t0)
}

fn aabb_finite(b: &Aabb) -> bool { b.min.is_finite() && b.max.is_finite() }

/// Uniform x-z hash grid over targets, rebuilt every tick. Sorted (key, target) pairs, no hashing.
struct Broadphase { cell: f64, entries: Vec<(i64, u32)>, always: Vec<u32> }

impl Broadphase {
    fn cell_of(&self, v: f64) -> i64 { libm::floor(v / self.cell).clamp(i32::MIN as f64, i32::MAX as f64) as i64 }
    fn key(ix: i64, iz: i64) -> i64 { (ix << 32) | (iz & 0xffff_ffff) }
    fn build(cell: f64, boxes: &[(DVec3, DVec3)]) -> Self {
        let mut bp = Broadphase { cell, entries: Vec::new(), always: Vec::new() };
        for (t, (min, max)) in boxes.iter().enumerate() {
            if !(min.is_finite() && max.is_finite()) { continue; }
            let (x0, x1, z0, z1) = (bp.cell_of(min.x), bp.cell_of(max.x), bp.cell_of(min.z), bp.cell_of(max.z));
            if x1 - x0 >= MAX_SPAN_CELLS || z1 - z0 >= MAX_SPAN_CELLS || x1 < x0 || z1 < z0 {
                bp.always.push(t as u32);
                continue;
            }
            for ix in x0..=x1 { for iz in z0..=z1 { bp.entries.push((Self::key(ix, iz), t as u32)); } }
        }
        bp.entries.sort_unstable();
        bp
    }
    /// Candidate targets whose cells overlap the x-z box of a segment, ascending and unique.
    fn query(&self, a: DVec3, b: DVec3, out: &mut Vec<u32>) {
        out.clear();
        out.extend_from_slice(&self.always);
        if !self.entries.is_empty() {
            let (x0, x1) = (self.cell_of(a.x.min(b.x)), self.cell_of(a.x.max(b.x)));
            let (z0, z1) = (self.cell_of(a.z.min(b.z)), self.cell_of(a.z.max(b.z)));
            if x1 - x0 >= MAX_SPAN_CELLS || z1 - z0 >= MAX_SPAN_CELLS {
                out.extend(self.entries.iter().map(|e| e.1));
            } else {
                for ix in x0..=x1 {
                    for iz in z0..=z1 {
                        let k = Self::key(ix, iz);
                        let start = self.entries.partition_point(|e| e.0 < k);
                        out.extend(self.entries[start..].iter().take_while(|e| e.0 == k).map(|e| e.1));
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
    }
}

/// First live solid cell along a world segment through a hull: (parameter on the segment, cell, index,
/// entry point and unit direction in cell units). Also returns cells visited.
/// Narrowphase hit: (parameter on the segment, cell, entry and unit direction in cell units).
type CellHit = (f64, CellIndex, [f64; 3], [f64; 3]);

fn narrowphase(grid: &VesselGrid, pose: &Pose, a: DVec3, b: DVec3) -> (Option<CellHit>, u32) {
    let la = pose.to_local(a) / CELL_M;
    let lb = pose.to_local(b) / CELL_M;
    if !(la.is_finite() && lb.is_finite()) { return (None, 0); }
    let mut work = 0u32;
    let mut found: Option<CellPos> = None;
    grid.traverse(la.to_array(), lb.to_array(), &mut |p, _| {
        work = work.saturating_add(1);
        if grid.is_solid(p) { found = Some(p); false } else { true }
    });
    let Some(p) = found else { return (None, work) };
    let Some(index) = grid.index(p) else { return (None, work) };
    let d = lb - la;
    let cell = [p.x as f64, p.y as f64, p.z as f64];
    let mut t = 0.0f64;
    for ax in 0..3 {
        if d[ax] != 0.0 {
            let (ta, tb) = ((cell[ax] - la[ax]) / d[ax], (cell[ax] + 1.0 - la[ax]) / d[ax]);
            t = t.max(ta.min(tb));
        }
    }
    let t = t.clamp(0.0, 1.0);
    let entry = la + d * t;
    let len = d.length();
    let dir = if len > 0.0 { d / len } else { (pose.rot.inverse() * (b - a)).normalize_or_zero() };
    (Some((t, index, entry.to_array(), dir.to_array())), work)
}

/// Earliest parameter s in [0, s_max] at which the shell (old + seg s) is within `r` of a centre moving
/// linearly from `c0` to `c1` over the tick.
fn sphere_sweep(old: DVec3, seg: DVec3, c0: DVec3, c1: DVec3, r: f64, s_max: f64) -> Option<f64> {
    if !(r.is_finite() && r >= 0.0 && c0.is_finite() && c1.is_finite()) { return None; }
    let p0 = old - c0;
    let w = seg - (c1 - c0);
    let c = p0.dot(p0) - r * r;
    if c <= 0.0 { return Some(0.0); }
    let a = w.dot(w);
    if a <= 0.0 { return None; }
    let bh = p0.dot(w);
    if bh >= 0.0 { return None; }
    let disc = bh * bh - a * c;
    if disc < 0.0 { return None; }
    let s = (-bh - libm::sqrt(disc)) / a;
    if s >= 0.0 && s <= s_max { Some(s) } else { None }
}

/// Conservative test for land along a segment before calling the terrain's exact (and costlier) check.
fn land_possible(terrain: &crate::terrain::Terrain, a: DVec3, b: DVec3) -> bool {
    let ymin = a.y.min(b.y);
    let (dx, dz) = (b.x - a.x, b.z - a.z);
    let len2 = dx * dx + dz * dz;
    terrain.map.islands.iter().any(|isl| {
        let (cx, cz, r, h) = (isl.centre_m[0], isl.centre_m[1], isl.radius_m, isl.height_m);
        if !(cx.is_finite() && cz.is_finite() && r.is_finite() && r > 0.0 && h.is_finite()) { return false; }
        if ymin > h.max(0.0) + 1.0 { return false; }
        let reach = r * 1.2 + 1.0;
        let (fx, fz) = (cx - a.x, cz - a.z);
        let t = if len2 > 0.0 { ((fx * dx + fz * dz) / len2).clamp(0.0, 1.0) } else { 0.0 };
        let (ex, ez) = (fx - dx * t, fz - dz * t);
        ex * ex + ez * ez <= reach * reach
    })
}

/// Keep rows whose mask entry is true, preserving order.
fn retain_rows<T>(v: &mut Vec<T>, keep: &[bool]) {
    let mut i = 0;
    v.retain(|_| { let k = keep.get(i).copied().unwrap_or(false); i += 1; k });
}

/// What ended a shell's segment this tick.
#[derive(Clone, Copy)]
enum End { Hull { target: usize, cell: CellIndex, entry: [f64; 3], dir: [f64; 3] }, Sphere { target: usize }, Water, Land }

impl Projectiles {
    pub fn len(&self) -> usize { self.id.len() }
    pub fn is_empty(&self) -> bool { self.id.is_empty() }
    /// Append a shell and return its spawn event. When `len() >= cap` and the band is `Auto`, merge: every
    /// second autocannon round is dropped and the other carries 2 rounds (L557). Callers spawn in
    /// (vessel id, mount id) order. Unit: `sim-core-projectiles`.
    ///
    /// Merging is per (shooter, mount): over the cap, a mount's round spawns with 2 rounds and that mount's
    /// next round within one second returns `None` (it is the second of the pair). A non-finite origin or
    /// velocity, or a store at [`MAX_LIVE_HARD`], returns `None`.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(&mut self, tick: u64, origin: DVec3, velocity: DVec3, calibre_mm: u16, band: CalibreBand, shooter: EntityId, mount: PartSlot, point_defence: bool, cap: usize) -> Option<ProjectileSpawn> {
        if !(origin.is_finite() && velocity.is_finite()) { return None; }
        self.normalise();
        if self.len() >= MAX_LIVE_HARD { return None; }
        self.merge_owed.retain(|e| tick.saturating_sub(e.2) <= MERGE_WINDOW_TICKS && e.2 <= tick);
        let mut rounds = 1u8;
        if band == CalibreBand::Auto {
            if let Some(i) = self.merge_owed.iter().position(|e| e.0 == shooter && e.1 == mount) {
                self.merge_owed.remove(i);
                return None;
            }
            if self.len() >= cap {
                self.merge_owed.push((shooter, mount, tick));
                rounds = 2;
            }
        }
        let id = ProjectileId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        self.id.push(id);
        self.pos.push(origin);
        self.vel.push(velocity);
        self.band.push(band);
        self.calibre_mm.push(calibre_mm);
        self.shooter.push(shooter);
        self.mount.push(mount);
        self.spawn_tick.push(tick);
        self.v_1km.push(0.0);
        self.path_m.push(0.0);
        self.rounds.push(rounds);
        self.point_defence.push(point_defence);
        Some(ProjectileSpawn { id, tick, origin, velocity, calibre_mm, band, shooter, mount, rounds, point_defence })
    }

    /// Truncate every column to the shortest one, so a malformed store (e.g. a bad keyframe) never panics.
    fn normalise(&mut self) {
        let n = [self.id.len(), self.pos.len(), self.vel.len(), self.band.len(), self.calibre_mm.len(), self.shooter.len(), self.mount.len(), self.spawn_tick.len(), self.v_1km.len(), self.path_m.len(), self.rounds.len(), self.point_defence.len()]
            .into_iter()
            .min()
            .unwrap_or(0);
        self.id.truncate(n);
        self.pos.truncate(n);
        self.vel.truncate(n);
        self.band.truncate(n);
        self.calibre_mm.truncate(n);
        self.shooter.truncate(n);
        self.mount.truncate(n);
        self.spawn_tick.truncate(n);
        self.v_1km.truncate(n);
        self.path_m.truncate(n);
        self.rounds.truncate(n);
        self.point_defence.truncate(n);
    }

    /// Integrate every projectile one tick, run broadphase and narrowphase against the targets, end shells at
    /// the water surface (`water_height` samples the wave field) or on land, expire point-defence rounds, and
    /// remove finished rows keeping id order. `vessels` gives the narrowphase read access to each hit hull's grid. Output hits are sorted by (tick, projectile id).
    /// Unit: `sim-core-projectiles`.
    ///
    /// `out` is cleared first and then holds exactly this tick's results (keep last tick's grazes elsewhere
    /// before calling, for the munitions step). A hull whose `vessels` entry is missing (or whose id does not
    /// match) is skipped.
    #[allow(clippy::too_many_arguments)]
    pub fn step(&mut self, tick: u64, bands: &[BandDef], hulls: &[HullTarget], spheres: &[SphereTarget], vessels: &crate::entity::Column<crate::vessel::Vessel>, water_height: &dyn Fn(f64, f64) -> f64, terrain: &crate::terrain::Terrain, tuning: &content::tuning::ProjectileTuning, out: &mut ProjectileStepOutput) {
        let grid_of = |id: EntityId| vessels.get(id).filter(|v| v.id == id).map(|v| &v.grid);
        self.step_with(tick, bands, hulls, spheres, &grid_of, water_height, terrain, tuning, out);
    }

    /// [`Projectiles::step`] with the grid lookup abstracted (tests use hand-built grids).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn step_with<'g>(&mut self, tick: u64, bands: &[BandDef], hulls: &[HullTarget], spheres: &[SphereTarget], grid_of: &dyn Fn(EntityId) -> Option<&'g VesselGrid>, water_height: &dyn Fn(f64, f64) -> f64, terrain: &crate::terrain::Terrain, tuning: &content::tuning::ProjectileTuning, out: &mut ProjectileStepOutput) {
        out.hits.clear();
        out.grazes.clear();
        out.splashes.clear();
        out.expired.clear();
        out.work_units = 0;
        self.normalise();
        let n = self.len();
        let ks = CalibreBand::ALL.map(|b| band_drag_k(bands, b));
        let k_of = |b: CalibreBand| ks[CalibreBand::ALL.iter().position(|x| *x == b).unwrap_or(0)];
        let pd_ticks = {
            let s = tuning.pd_self_destruct_s;
            if s.is_finite() && s > 0.0 { libm::round(s * TICK_HZ as f64).clamp(1.0, MAX_FLIGHT_TICKS as f64) as u64 } else { 4 * TICK_HZ as u64 }
        };
        let pd_margin = if tuning.pd_proximity_m.is_finite() && tuning.pd_proximity_m >= 0.0 { tuning.pd_proximity_m.min(50.0) } else { 1.5 };
        let cell_m = if tuning.broadphase_cell_m.is_finite() && tuning.broadphase_cell_m >= 1.0 { tuning.broadphase_cell_m } else { 128.0 };

        // Sphere motion over the tick: previous centre (if seen last tick) to the current one.
        let sphere_c0: Vec<DVec3> = spheres
            .iter()
            .map(|s| match self.sphere_prev.binary_search_by(|e| e.0.cmp(&s.body)) {
                Ok(i) if self.sphere_prev[i].1.is_finite() => self.sphere_prev[i].1,
                _ => s.centre,
            })
            .collect();
        let mut boxes: Vec<(DVec3, DVec3)> = Vec::with_capacity(hulls.len() + spheres.len());
        for h in hulls {
            if aabb_finite(&h.bounds) { boxes.push((h.bounds.min, h.bounds.max)); } else { boxes.push((DVec3::NAN, DVec3::NAN)); }
        }
        for (s, c0) in spheres.iter().zip(&sphere_c0) {
            let r = if s.radius.is_finite() && s.radius >= 0.0 { s.radius + pd_margin } else { f64::NAN };
            boxes.push((s.centre.min(*c0) - DVec3::splat(r), s.centre.max(*c0) + DVec3::splat(r)));
        }
        let bp = Broadphase::build(cell_m, &boxes);
        let mut cand: Vec<u32> = Vec::new();
        let mut keep = vec![true; n];
        let mut work: u32 = 0;

        #[allow(clippy::needless_range_loop)]
        for i in 0..n {
            work = work.saturating_add(1);
            let (old, vel_old) = (self.pos[i], self.vel[i]);
            let age = tick.saturating_sub(self.spawn_tick[i]).saturating_add(1);
            if !(old.is_finite() && vel_old.is_finite()) || age > MAX_FLIGHT_TICKS {
                keep[i] = false;
                continue;
            }
            let k = k_of(self.band[i]);
            let (new, vel_new) = ballistic_step(old, vel_old, k, DT);
            if !(new.is_finite() && vel_new.is_finite()) { keep[i] = false; continue; }
            let seg = new - old;

            // Water and land bound the segment.
            let mut t_end = 1.0f64;
            let mut end: Option<End> = None;
            if old.y.min(new.y) < WAVE_CEILING_M {
                // One wave sample per shell per tick; the start is sampled only when the end is under water
                // or on the shell's first tick (it may have spawned below the surface).
                let a1 = new.y - water_height(new.x, new.z);
                if a1 < 0.0 || age == 1 {
                    let a0 = old.y - water_height(old.x, old.z);
                    if a0 <= 0.0 && a0.is_finite() {
                        t_end = 0.0;
                        end = Some(End::Water);
                    } else if a1 < 0.0 && a0.is_finite() {
                        t_end = (a0 / (a0 - a1)).clamp(0.0, 1.0);
                        end = Some(End::Water);
                    }
                }
            }
            let at = |t: f64| old + seg * t;
            if land_possible(terrain, old, at(t_end)) && terrain.segment_blocked(old, at(t_end)) {
                let (mut lo, mut hi) = (0.0f64, t_end);
                for _ in 0..LAND_BISECT_STEPS {
                    let mid = 0.5 * (lo + hi);
                    if terrain.segment_blocked(old, at(mid)) { hi = mid; } else { lo = mid; }
                }
                t_end = hi;
                end = Some(End::Land);
            }

            // Hulls and spheres inside [0, t_end].
            let b = at(t_end);
            bp.query(old, b, &mut cand);
            let mut best: Option<(f64, End)> = None;
            let pd = self.point_defence[i];
            for &c in &cand {
                let c = c as usize;
                if c < hulls.len() {
                    let h = &hulls[c];
                    if h.vessel == self.shooter[i] || h.vessel.is_none() { continue; }
                    let Some(t_box) = segment_aabb(old, b, h.bounds.min, h.bounds.max) else { continue };
                    if best.is_some_and(|(bt, _)| t_box * t_end > bt) { continue; }
                    let Some(grid) = grid_of(h.vessel) else { continue };
                    let (hit, w) = narrowphase(grid, &h.pose, old, b);
                    work = work.saturating_add(w);
                    if let Some((t, cell, entry, dir)) = hit {
                        let tg = t * t_end;
                        if best.is_none_or(|(bt, _)| tg < bt) { best = Some((tg, End::Hull { target: c, cell, entry, dir })); }
                    }
                } else {
                    let si = c - hulls.len();
                    let Some(s) = spheres.get(si) else { continue };
                    if s.body.is_none() { continue; }
                    let r = s.radius + if pd { pd_margin } else { 0.0 };
                    if let Some(t) = sphere_sweep(old, seg, sphere_c0[si], s.centre, r, t_end) {
                        if best.is_none_or(|(bt, _)| t < bt) { best = Some((t, End::Sphere { target: si })); }
                    }
                }
            }
            let (t_hit, end) = match best {
                Some((t, e)) => (t, Some(e)),
                None => (t_end, end),
            };

            // Path and v_1km up to the end point.
            let seg_len = seg.length();
            let (s0, s1) = (vel_old.length(), vel_new.length());
            let path0 = self.path_m[i];
            let path_at = path0 + seg_len * t_hit;
            let mut v1 = self.v_1km[i];
            if v1 <= 0.0 && path_at >= V_1KM_PATH_M && seg_len > 0.0 { v1 = speed_at_fraction(s0, s1, (V_1KM_PATH_M - path0) / (seg_len * 1.0)); }

            match end {
                Some(End::Hull { target, cell, entry, dir }) => {
                    let h = &hulls[target];
                    let speed = speed_at_fraction(s0, s1, t_hit);
                    let v_1km = if v1 > 0.0 { v1 } else { predict_v_1km(speed, path_at, k) };
                    let world_point = h.pose.to_world(DVec3::from_array(entry) * CELL_M);
                    out.hits.push(ShellHit { tick, projectile: self.id[i], vessel: h.vessel, entry, dir, first_cell: cell, speed_mps: speed, v_1km, band: self.band[i], calibre_mm: self.calibre_mm[i], shooter: self.shooter[i], rounds: self.rounds[i], world_point });
                    keep[i] = false;
                }
                Some(End::Sphere { target }) => {
                    out.grazes.push(BodyGraze { tick, projectile: self.id[i], body: spheres[target].body, shooter: self.shooter[i] });
                    keep[i] = false;
                }
                Some(End::Water) | Some(End::Land) => {
                    out.splashes.push(SplashAt { projectile: self.id[i], pos: at(t_hit), calibre_mm: self.calibre_mm[i], shooter: self.shooter[i] });
                    keep[i] = false;
                }
                None => {
                    self.pos[i] = new;
                    self.vel[i] = vel_new;
                    self.path_m[i] = path0 + seg_len;
                    self.v_1km[i] = v1.max(0.0);
                    if self.point_defence[i] && age >= pd_ticks {
                        out.expired.push(self.id[i]);
                        keep[i] = false;
                    }
                }
            }
        }

        if keep.iter().any(|k| !k) {
            retain_rows(&mut self.id, &keep);
            retain_rows(&mut self.pos, &keep);
            retain_rows(&mut self.vel, &keep);
            retain_rows(&mut self.band, &keep);
            retain_rows(&mut self.calibre_mm, &keep);
            retain_rows(&mut self.shooter, &keep);
            retain_rows(&mut self.mount, &keep);
            retain_rows(&mut self.spawn_tick, &keep);
            retain_rows(&mut self.v_1km, &keep);
            retain_rows(&mut self.path_m, &keep);
            retain_rows(&mut self.rounds, &keep);
            retain_rows(&mut self.point_defence, &keep);
        }
        out.hits.sort_by_key(|h| (h.tick, h.projectile));
        out.work_units = out.work_units.saturating_add(work);

        let mut prev: Vec<(EntityId, DVec3)> = spheres.iter().filter(|s| s.centre.is_finite()).map(|s| (s.body, s.centre)).collect();
        prev.sort_by_key(|e| e.0);
        prev.dedup_by(|a, b| a.0 == b.0);
        self.sphere_prev = prev;
    }

    /// True when any live projectile's next-tick segment can reach `bounds` (a hull that must run full physics,
    /// L1547). Unit: `sim-core-projectiles`.
    ///
    /// Conservative: the drag-free segment `pos -> pos + v dt - g dt^2 / 2` against the box grown by 1 m.
    pub fn threatens(&self, bounds: &Aabb) -> bool {
        if !aabb_finite(bounds) { return false; }
        let (min, max) = (bounds.min - DVec3::splat(THREAT_MARGIN_M), bounds.max + DVec3::splat(THREAT_MARGIN_M));
        let drop = DVec3::new(0.0, -0.5 * GRAVITY * DT * DT, 0.0);
        self.pos.iter().zip(&self.vel).any(|(&p, &v)| {
            if !(p.is_finite() && v.is_finite()) { return false; }
            segment_aabb(p, p + v * DT + drop, min, max).is_some()
        })
    }
}
