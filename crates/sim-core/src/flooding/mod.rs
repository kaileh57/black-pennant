//! Unit `sim-core-flooding`. Flooding through openings, water planes, pumps and flood valves.
//! DESIGN.md L622-648, L652-658 (gates).
//!
//! Orifice law: Q = Cd * A * sqrt(2 g dh) with Cd = 0.6, scaled by `flood_rate_scale`. dh is the net head:
//! water surface height above the opening centre (floored at 0) on one side minus the other; the sea side uses
//! the wave height sampled under that cell. Flow runs from high to low head and stops when both sides are
//! below the opening. Per tick, move Q * dt clamped to the levelling volume: dh * A_i * A_j / (A_i + A_j)
//! between compartments (free-surface areas), or dh * A_i against the sea. Compartments are vented.
//! Each compartment's water surface is a level world-space plane found by 16 bisection steps over its cells
//! at the current heel and trim; water weight acts at the centroid of the filled volume.
//! Flows are computed per vessel in parallel; openings are applied in ascending compartment id order.
//! Pumps drain their own compartment or one adjacent compartment chosen by Damage Control, deepest water
//! first. Magazines flood from the fire main in 10 s. sqrt is IEEE-exact; no libm call is needed here.
//!
//! # Model (binding for the units that read compartment water)
//! - Cell fill law: at heel and trim a 0.5 m cell fills linearly over its projected height
//!   `h = 0.5 m * (|u.x| + |u.y| + |u.z|)`, where `u` is world up in the vessel frame. This is the law
//!   `hydro::equilibrium` uses for cells open to the sea, so the Resilience gauge ([`flooded_volumes`]) and the
//!   simulation agree by construction. The plane is bisected (`level_bisection_steps`, 16) between the lowest
//!   and highest cell of the compartment; `free_surface_m2` is the plan area of the cells the plane cuts, never
//!   below one cell face (0.25 m^2), so a dry or brim-full compartment still passes water.
//! - Tick order, after door and valve sync: every wet compartment is levelled at the current pose; openings are
//!   applied in stored order (ascending compartment id) against running water levels, so several breach faces
//!   of one compartment share the levelling volume instead of each taking all of it (a compartment dry at the
//!   start of the tick takes its first water over the plan area of its lowest cells); pumps run in ascending
//!   slot; magazines on the fire main fill; every compartment whose water changed is levelled again, so the
//!   plane, centroid and free surface always match `water_m3` at the pose of this tick.
//! - Each transfer is clamped to the water the source holds and the room left in the destination. Openings
//!   that are closed or patched pass nothing, and so do absorbed (merged-away) compartments.
//! - Flood valves: `controls.open_valves` is the whole truth each tick. A listed compartment that has a valve
//!   gets `flood_valve_open` and its valve opening opens; every other valve closes.
//! - Pumps: a running pump (parts decide running from orders, damage and power) moves its `m3ps` times its
//!   output factor and power supply fraction overboard, from its target when that compartment holds water,
//!   else from its own compartment. Damage Control picks targets deepest water first (sim-systems).
//! - Magazines listed in `controls.flood_magazines` take fire-main water for `magazine_flood_s` (10 s) and are
//!   then flooded: the fire there goes out (fire unit) and the rounds are lost until rearmed. The fire main is
//!   always available while the magazine works; the water inside a flooded magazine is not tracked as mass or
//!   as compartment water.
//! - `inflow_m3` is sea water in through openings, minus water out through openings and pumps.
use crate::compartments::{Compartment, CompartmentSet, Dims, OpeningKind};
use crate::math::{DQuat, DVec3, Pose};
use crate::parts::{PartRuntime, PartState, VesselControls};
use crate::vessel::Vessel;
use crate::world::StepContext;
use content::catalogue::Catalogue;
use content::cell::{Face, CELL_FACE_M2, CELL_M, CELL_VOLUME_M3};
use content::ids::{CompartmentId, PartSlot};
use content::part::PartFunction;
use content::tuning::FloodingTuning;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FloodOutcome {
    /// Compartments that crossed the 50% fill line this tick (fires go out, berths stop counting), ascending.
    pub crossed_half: Vec<CompartmentId>,
    /// Magazines newly flooded (their fire is out and their ammunition is lost until rearmed, L667).
    pub magazines_flooded: Vec<content::ids::PartSlot>,
    /// Net water taken on this tick in m^3.
    pub inflow_m3: f64,
    pub work_units: u32,
}

/// One tick of flooding for one vessel. `sea_height(x, z)` samples the wave field. Unit: `sim-core-flooding`.
pub fn step_flooding(vessel: &mut Vessel, sea_height: &dyn Fn(f64, f64) -> f64, ctx: &StepContext<'_>) -> FloodOutcome {
    // No wave stands higher than the sum of the amplitudes: an opening above that band with no water inside
    // needs no sea sample.
    let band: f64 = ctx.waves.waves.iter().take(ctx.waves.count as usize).map(|w| w.amplitude.abs()).sum();
    let io = FloodIo { comps: &mut vessel.compartments, parts: &mut vessel.parts, size: vessel.grid.size(), controls: &vessel.controls, pose: vessel.kin.pose };
    let env = FloodEnv { sea: sea_height, band: if band.is_finite() { band } else { f64::INFINITY }, catalogue: ctx.catalogue, tuning: &ctx.tuning.flooding, dt: ctx.dt };
    flood_tick(io, &env)
}

/// Solve one compartment's water plane and filled centroid for its current volume and the vessel's pose
/// (16 bisection steps). Unit: `sim-core-flooding`.
///
/// Also clamps `water_m3` into `0..=volume_m3` and refreshes `free_surface_m2`. `steps == 0` uses 16. An
/// unknown or absorbed compartment is left alone.
pub fn level_water(vessel: &mut Vessel, compartment: CompartmentId, steps: u8) {
    let fr = Frame::of(&vessel.kin.pose);
    let d = Dims::new(vessel.grid.size());
    let Some(c) = vessel.compartments.get_mut(compartment) else { return };
    let mut scratch = Vec::new();
    level(c, d, &fr, steps, &mut scratch);
}

/// Equilibrium water volumes with the listed compartments open to the sea and pumps off, for the Resilience
/// gauge's 1- and 2-compartment cases (L657). Unit: `sim-core-flooding`.
///
/// The hull settles where `hydro::equilibrium` puts it with those compartments open (prefab dry mass with the
/// fuel, ballast and magazine loads its part states hold, default buoyancy tuning, since the signature carries
/// neither a catalogue nor a table), and each open compartment
/// fills to the outside water plane by the cell fill law above. A hull that does not float with them open
/// has them full. Unknown and absorbed ids are skipped; the result is ascending and deduplicated.
pub fn flooded_volumes(prefab: &crate::vessel::VesselPrefab, open: &[CompartmentId]) -> Vec<(CompartmentId, f64)> {
    let mut ids: Vec<CompartmentId> = open.iter().copied().filter(|&c| prefab.compartments.get(c).is_some_and(|k| !k.is_absorbed() && !k.cells.is_empty())).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() { return Vec::new(); }
    let tuning = content::tuning::BuoyancyTuning::default();
    let mass = stores_mass(prefab);
    let eq = crate::hydro::equilibrium(&prefab.blocks, &mass, &ids, &prefab.compartments, &tuning);
    let plane = if eq.floats { equilibrium_plane(&prefab.blocks, &eq) } else { None };
    let d = Dims::new(prefab.grid.size());
    ids.into_iter().filter_map(|id| prefab.compartments.get(id).map(|c| (id, c))).map(|(id, c)| {
        let v = match plane {
            None => c.cells.len() as f64 * CELL_VOLUME_M3,
            Some((u, k)) => {
                let h = cell_height(u);
                let mut sum = 0.0;
                for cell in &c.cells {
                    let i = cell.index();
                    if i >= d.len() { continue; }
                    sum += fill(k, u.dot(d.centre(i)), h);
                }
                sum * CELL_VOLUME_M3
            }
        };
        (id, v)
    }).collect()
}

/// Prefab dry mass plus the loads its part states carry without a catalogue lookup: fuel, ballast water and
/// magazine ammunition, each at its part centre. Ballast and fuel left out of the gauge would float the hull
/// high and understate the flooded volumes. Ready rounds on mounts, launcher cells and mines need the catalogue
/// and are left out; they are small against the hull.
fn stores_mass(prefab: &crate::vessel::VesselPrefab) -> crate::mass::MassProperties {
    use crate::mass::{combine, MassProperties, FUEL_DENSITY_KG_PER_L};
    let mut m = prefab.dry_mass;
    for p in &prefab.parts {
        let kg = match p.state {
            PartState::FuelTank { litres } => litres * FUEL_DENSITY_KG_PER_L,
            PartState::Ballast { kg } => kg,
            PartState::Magazine { ammo_kg, .. } => ammo_kg,
            _ => continue,
        };
        if !(kg.is_finite() && kg > 0.0 && p.centre.is_finite()) { continue; }
        m = combine(&m, &MassProperties { mass_kg: kg, com: p.centre, inertia: crate::math::DMat3::ZERO });
    }
    m
}

// ---------------------------------------------------------------------------------------------------------
// Geometry shared with the vessel unit.

/// Body-frame up direction for a heel (positive to starboard) and trim (positive bow up), the inverse of
/// `hydro::heel_trim`.
pub(crate) fn up_of(heel: f64, trim: f64) -> DVec3 {
    let (sh, ch) = libm::sincos(heel);
    let (st, ct) = libm::sincos(trim);
    DVec3::new(st, ct * ch, -ct * sh)
}

/// The calm-water plane of a solved equilibrium in the body frame: `(u, c)` with the surface at `u . p = c`.
/// Recovered from heel, trim and keel draft, which `hydro::equilibrium` measures as the deepest block keel
/// below the plane. `None` when the equilibrium is unusable.
pub(crate) fn equilibrium_plane(blocks: &crate::hydro::BuoyancyBlocks, eq: &crate::hydro::Equilibrium) -> Option<(DVec3, f64)> {
    if blocks.blocks.is_empty() || !(eq.heel_rad.is_finite() && eq.trim_rad.is_finite() && eq.draft_m.is_finite()) { return None; }
    let u = up_of(eq.heel_rad, eq.trim_rad);
    let c = eq.draft_m - blocks.keel_reach(u);
    (c.is_finite() && u.is_finite()).then_some((u, c))
}

/// Projected vertical extent of a cell for a body-frame up direction.
#[inline]
fn cell_height(u: DVec3) -> f64 { (CELL_M * (u.x.abs() + u.y.abs() + u.z.abs())).max(CELL_M) }

/// Fill fraction of a cell whose centre stands at `k` along up, under a plane at `c`.
#[inline]
fn fill(c: f64, k: f64, h: f64) -> f64 { ((c - k) / h + 0.5).clamp(0.0, 1.0) }

/// Pose-derived frame: world up in the body frame, the world height of the body origin and the cell height.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Frame { pub up: DVec3, pub base_y: f64, pub h: f64 }
impl Frame {
    pub(crate) fn of(pose: &Pose) -> Frame {
        let rot = if pose.rot.is_finite() && pose.rot.length_squared() > 1e-12 { pose.rot.normalize() } else { DQuat::IDENTITY };
        let up = (rot.inverse() * DVec3::Y).normalize_or(DVec3::Y);
        let base_y = if pose.pos.y.is_finite() { pose.pos.y } else { 0.0 };
        Frame { up, base_y, h: cell_height(up) }
    }
    /// World height of a body-frame point.
    #[inline]
    fn world_y(&self, p: DVec3) -> f64 { self.base_y + self.up.dot(p) }
}

/// Lowest world height of a compartment's cell box.
fn box_bottom(c: &Compartment, fr: &Frame) -> f64 {
    let lo = DVec3::new(c.min[0] as f64, c.min[1] as f64, c.min[2] as f64) * CELL_M;
    let hi = DVec3::new(c.max[0] as f64 + 1.0, c.max[1] as f64 + 1.0, c.max[2] as f64 + 1.0) * CELL_M;
    let u = fr.up;
    let pick = |a: f64, l: f64, h: f64| if a >= 0.0 { a * l } else { a * h };
    fr.base_y + pick(u.x, lo.x, hi.x) + pick(u.y, lo.y, hi.y) + pick(u.z, lo.z, hi.z)
}

/// A run of consecutive cells along +X in one row: the first cell's centre, its height along up, and length.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CellRun { p0: DVec3, k0: f64, len: u32 }

/// Split a compartment's ascending cells into runs along X (consecutive indices in one row).
fn runs_of(c: &Compartment, d: Dims, up: DVec3, out: &mut Vec<CellRun>) {
    out.clear();
    let n = d.len();
    let mut j = 0;
    let cells = &c.cells;
    while j < cells.len() {
        let i = cells[j].index();
        if i >= n { j += 1; continue; }
        let x = i % d.sx.max(1);
        let mut len = 1usize;
        while j + len < cells.len() && cells[j + len].index() == i + len && x + len < d.sx { len += 1; }
        let p0 = d.centre(i);
        out.push(CellRun { p0, k0: up.dot(p0), len: len as u32 });
        j += len;
    }
}

/// Sum of cell fill fractions along one run under the plane `c` (closed form: full, linear and dry parts).
fn run_fill(r: &CellRun, dk: f64, c: f64, h: f64) -> f64 {
    let len = r.len as f64;
    let a = (c - r.k0) / h + 0.5;
    let b = dk / h;
    if b.abs() < 1e-12 { return len * a.clamp(0.0, 1.0); }
    // t_j = a - b j; mirror an increasing run so that t falls along j.
    let (a, b) = if b > 0.0 { (a, b) } else { (a - b * (len - 1.0), -b) };
    let full = (libm::floor((a - 1.0) / b) + 1.0).clamp(0.0, len);
    let dry = libm::ceil(a / b).clamp(full, len);
    let m = dry - full;
    let partial = m * a - b * (full + dry - 1.0) * m * 0.5;
    (full + partial.clamp(0.0, m)).clamp(0.0, len)
}

/// Level one compartment at a frame (see the module docs). Returns work units: cells for the runs and the
/// final pass, plus runs per bisection step.
pub(crate) fn level(c: &mut Compartment, d: Dims, fr: &Frame, steps: u8, scratch: &mut Vec<CellRun>) -> u32 {
    if c.is_absorbed() || c.cells.is_empty() {
        c.water_m3 = 0.0;
        c.free_surface_m2 = 0.0;
        return 0;
    }
    let vol = c.cells.len() as f64 * CELL_VOLUME_M3;
    let water = if c.water_m3.is_finite() { c.water_m3.clamp(0.0, vol) } else { 0.0 };
    c.water_m3 = water;
    let (u, h) = (fr.up, fr.h);
    let half = 0.5 * h;
    let dk = u.x * CELL_M;
    runs_of(c, d, u, scratch);
    let (mut kmin, mut kmax) = (f64::INFINITY, f64::NEG_INFINITY);
    for r in scratch.iter() {
        let k1 = r.k0 + dk * (r.len as f64 - 1.0);
        kmin = kmin.min(r.k0.min(k1));
        kmax = kmax.max(r.k0.max(k1));
    }
    if !(kmin.is_finite() && kmax.is_finite()) { c.free_surface_m2 = CELL_FACE_M2; return c.cells.len() as u32; }
    let steps = if steps == 0 { 16 } else { steps };
    let full = water >= vol;
    let plane = if water <= 0.0 {
        kmin - half
    } else if full {
        kmax + half
    } else {
        let target = water / CELL_VOLUME_M3;
        let (mut lo, mut hi) = (kmin - half, kmax + half);
        for _ in 0..steps {
            let mid = 0.5 * (lo + hi);
            let mut sum = 0.0;
            for r in scratch.iter() { sum += run_fill(r, dk, mid, h); }
            if sum < target { lo = mid; } else { hi = mid; }
        }
        0.5 * (lo + hi)
    };
    let (mut sum_f, mut moment, mut cut) = (0.0, DVec3::ZERO, 0usize);
    let step = DVec3::new(CELL_M, 0.0, 0.0);
    for r in scratch.iter() {
        let mut p = r.p0;
        let mut k = r.k0;
        for _ in 0..r.len {
            let t = (plane - k) / h + 0.5;
            let f = t.clamp(0.0, 1.0);
            if f > 0.0 {
                // The wet part of a cell sits at the bottom of its projected height.
                sum_f += f;
                moment += (p - u * (half * (1.0 - f))) * f;
            }
            let surface = if full { t > 0.0 && t <= 1.0 } else { (0.0..1.0).contains(&t) };
            if surface { cut += 1; }
            p += step;
            k += dk;
        }
    }
    c.water_plane_y = fr.base_y + plane;
    c.water_centroid = if sum_f > 0.0 && moment.is_finite() { moment / sum_f } else { c.centroid };
    c.free_surface_m2 = (cut as f64 * CELL_VOLUME_M3 / h).max(CELL_FACE_M2);
    let runs = scratch.len() as u32;
    let bisect = if water > 0.0 && !full { runs.saturating_mul(steps as u32) } else { 0 };
    (c.cells.len() as u32).saturating_mul(2).saturating_add(bisect)
}

// ---------------------------------------------------------------------------------------------------------
// The tick.

/// The parts of a vessel one flooding tick touches.
pub(crate) struct FloodIo<'a> {
    pub comps: &'a mut CompartmentSet,
    pub parts: &'a mut [PartRuntime],
    pub size: [u16; 3],
    pub controls: &'a VesselControls,
    pub pose: Pose,
}

/// Everything else a flooding tick reads.
pub(crate) struct FloodEnv<'a> {
    pub sea: &'a dyn Fn(f64, f64) -> f64,
    /// Highest the sea can stand above or below y = 0 (infinite when unknown).
    pub band: f64,
    pub catalogue: &'a Catalogue,
    pub tuning: &'a FloodingTuning,
    pub dt: f64,
}

fn pos_or(v: f64, d: f64) -> f64 { if v.is_finite() && v > 0.0 { v } else { d } }

fn face_dir(f: Face) -> DVec3 { let (x, y, z) = f.offset(); DVec3::new(x as f64, y as f64, z as f64) }

/// Running water state of one compartment during a tick.
#[derive(Clone, Copy)]
struct Run {
    /// False for absorbed compartments, which take no part.
    live: bool,
    /// World height of the water surface; meaningful while `water > 0`.
    plane: f64,
    /// Free-surface area used for running levels and the levelling clamp.
    area: f64,
    /// False while `area` is only the one-face floor of a compartment that started the tick dry; the first
    /// opening that moves water through it measures the film that arriving water would spread over.
    area_known: bool,
    /// World height of the compartment floor, where water arriving in a dry compartment starts.
    bottom: f64,
    volume: f64,
}

/// Index of a part by slot (parts are normally stored at their slot index).
fn part_index(parts: &[PartRuntime], s: PartSlot) -> Option<usize> {
    if parts.get(s.index()).is_some_and(|p| p.slot == s) { Some(s.index()) } else { parts.iter().position(|p| p.slot == s) }
}

pub(crate) fn flood_tick(io: FloodIo<'_>, env: &FloodEnv<'_>) -> FloodOutcome {
    let FloodIo { comps, parts, size, controls, pose } = io;
    let mut out = FloodOutcome::default();
    let dt = if env.dt.is_finite() && env.dt > 0.0 { env.dt } else { 0.0 };
    let t = env.tuning;
    let flooded = if t.flooded_fraction.is_finite() { t.flooded_fraction } else { 0.5 };
    let d = Dims::new(size);
    let n = d.len();
    let fr = Frame::of(&pose);
    let mut scratch: Vec<CellRun> = Vec::new();
    let mut work: u32 = 0;

    // ---- Doors and valves ----
    comps.sync_doors(parts);
    let want: BTreeSet<CompartmentId> = controls.open_valves.iter().copied().collect();
    let has_valve: BTreeSet<CompartmentId> = comps.openings.iter().filter(|o| o.kind == OpeningKind::FloodValve).map(|o| o.a).collect();
    for c in &mut comps.compartments { c.flood_valve_open = want.contains(&c.id) && has_valve.contains(&c.id); }
    for o in &mut comps.openings {
        if o.kind == OpeningKind::FloodValve { o.open = comps.compartments.get(o.a.index()).is_some_and(|c| c.flood_valve_open); }
    }

    // ---- Start of tick: level every wet compartment at this pose ----
    let before: Vec<f64> = comps.compartments.iter().map(|c| c.fill_fraction()).collect();
    let mut run: Vec<Run> = Vec::with_capacity(comps.compartments.len());
    for c in &mut comps.compartments {
        if c.is_absorbed() || c.cells.is_empty() {
            c.water_m3 = 0.0;
            run.push(Run { live: false, plane: 0.0, area: 0.0, bottom: 0.0, volume: 0.0, area_known: true });
            continue;
        }
        let volume = c.cells.len() as f64 * CELL_VOLUME_M3;
        if !c.water_m3.is_finite() || c.water_m3 < 0.0 { c.water_m3 = 0.0; }
        if c.water_m3 > volume { c.water_m3 = volume; }
        let wet = c.water_m3 > 0.0;
        if wet { work = work.saturating_add(level(c, d, &fr, t.level_bisection_steps, &mut scratch)); }
        let area = if wet && c.free_surface_m2.is_finite() { c.free_surface_m2.max(CELL_FACE_M2) } else { CELL_FACE_M2 };
        run.push(Run { live: true, plane: c.water_plane_y, area, bottom: box_bottom(c, &fr), volume, area_known: wet });
    }
    let start: Vec<f64> = comps.compartments.iter().map(|c| c.water_m3).collect();

    // ---- Openings, in stored order (ascending compartment id) ----
    if dt > 0.0 {
        let cd = pos_or(t.cd, 0.6);
        let scale = if t.flood_rate_scale.is_finite() && t.flood_rate_scale >= 0.0 { t.flood_rate_scale } else { 1.0 };
        let g = crate::physics::GRAVITY;
        let mut sea_cache: Option<(u32, f64)> = None;
        let head = |c: &Compartment, r: &Run, y: f64| if c.water_m3 > 0.0 && r.plane.is_finite() { (r.plane - y).max(0.0) } else { 0.0 };
        for oi in 0..comps.openings.len() {
            let o = comps.openings[oi];
            if !o.open || o.patched || !(o.area_m2.is_finite() && o.area_m2 > 0.0) { continue; }
            let a = o.a.index();
            if !run.get(a).is_some_and(|r| r.live) { continue; }
            let b = match o.b {
                Some(b) if b == o.a => continue,
                Some(b) => { if !run.get(b.index()).is_some_and(|r| r.live) { continue; } Some(b.index()) }
                None => None,
            };
            let ci = o.cell.index();
            if ci >= n { continue; }
            work = work.saturating_add(1);
            let at = d.centre(ci) + face_dir(o.face) * (0.5 * CELL_M);
            let y = fr.world_y(at);
            if !y.is_finite() { continue; }
            let ha = head(&comps.compartments[a], &run[a], y);
            let hb = match b {
                Some(b) => head(&comps.compartments[b], &run[b], y),
                None => {
                    if ha <= 0.0 && y > env.band { continue; }
                    let s = match sea_cache {
                        Some((cell, s)) if cell == o.cell.0 => s,
                        _ => {
                            let w = pose.to_world(at);
                            let s = (env.sea)(w.x, w.z);
                            let s = if s.is_finite() { s } else { 0.0 };
                            sea_cache = Some((o.cell.0, s));
                            s
                        }
                    };
                    (s - y).max(0.0)
                }
            };
            if ha <= 0.0 && hb <= 0.0 { continue; }
            let dh = ha - hb;
            if dh == 0.0 { continue; }
            for k in [Some(a), b].into_iter().flatten() {
                if !run[k].area_known { work = work.saturating_add(measure_film(&comps.compartments[k], &mut run[k], d, &fr)); }
            }
            let q = cd * o.area_m2 * (2.0 * g * dh.abs()).sqrt() * scale;
            let level_v = match b {
                Some(b) => { let (x, z) = (run[a].area, run[b].area); dh.abs() * x * z / (x + z) }
                None => dh.abs() * run[a].area,
            };
            let mut v = (q * dt).min(level_v);
            // Source water and destination room.
            let (src, dst) = if dh > 0.0 { (Some(a), b) } else { (b, Some(a)) };
            if let Some(s) = src { v = v.min(comps.compartments[s].water_m3); }
            if let Some(t) = dst { v = v.min((run[t].volume - comps.compartments[t].water_m3).max(0.0)); }
            if !(v > 0.0) || !v.is_finite() { continue; }
            if let Some(s) = src { take(&mut comps.compartments[s], &mut run[s], v); }
            if let Some(t) = dst { give(&mut comps.compartments[t], &mut run[t], v); }
            if b.is_none() { out.inflow_m3 += if dh > 0.0 { -v } else { v }; }
        }
    }

    // ---- Pumps, ascending slot ----
    if dt > 0.0 {
        for p in parts.iter() {
            let PartState::Pump { running: true, target } = p.state else { continue };
            if !p.works() { continue; }
            let Some(PartFunction::Pump { m3ps }) = env.catalogue.parts.get_index(p.def.index()).map(|(_, d)| &d.function) else { continue };
            let supply = if p.links.power_supplied.is_finite() { p.links.power_supplied.clamp(0.0, 1.0) } else { 0.0 };
            let rate = if m3ps.is_finite() && *m3ps > 0.0 { m3ps * p.output_factor() * supply * dt } else { 0.0 };
            if !(rate > 0.0) { continue; }
            work = work.saturating_add(1);
            let wet = |c: Option<CompartmentId>| c.filter(|c| run.get(c.index()).is_some_and(|r| r.live) && comps.compartments[c.index()].water_m3 > 0.0);
            let Some(from) = wet(target).or_else(|| wet(p.compartment)) else { continue };
            let i = from.index();
            let v = rate.min(comps.compartments[i].water_m3);
            if !(v > 0.0) { continue; }
            take(&mut comps.compartments[i], &mut run[i], v);
            out.inflow_m3 -= v;
        }
    }

    // ---- Magazines on the fire main ----
    if dt > 0.0 && !controls.flood_magazines.is_empty() {
        let fill_s = pos_or(t.magazine_flood_s, 10.0);
        let mut slots: Vec<PartSlot> = controls.flood_magazines.clone();
        slots.sort_unstable();
        slots.dedup();
        for s in slots {
            let Some(i) = part_index(parts, s) else { continue };
            let p = &mut parts[i];
            if !p.works() { continue; }
            let PartState::Magazine { rounds, flooded: done @ false, .. } = &mut p.state else { continue };
            let m = &mut p.links.mech;
            m.flood_s = if m.flood_s.is_finite() { (m.flood_s + dt).max(0.0) } else { dt };
            work = work.saturating_add(1);
            if m.flood_s + 1e-9 >= fill_s {
                *done = true;
                *rounds = 0;
                m.flood_s = fill_s;
                out.magazines_flooded.push(s);
            }
        }
    }

    // ---- End of tick: re-level what changed, report the half line ----
    for (i, c) in comps.compartments.iter_mut().enumerate() {
        if !run[i].live { continue; }
        if c.water_m3 < 1e-12 { c.water_m3 = 0.0; }
        if c.water_m3 != start[i] { work = work.saturating_add(level(c, d, &fr, t.level_bisection_steps, &mut scratch)); }
        if before[i] <= flooded && c.fill_fraction() > flooded { out.crossed_half.push(c.id); }
    }
    out.work_units = work;
    out
}

/// For a compartment that started the tick dry: its true floor (the bottom of its lowest cell, which the cell
/// box can undercut on a shaped hull) and the free surface of the first water to arrive, the plan area of the
/// cells within half a cell height of that floor. Without it, the first tick's running level would rise as if
/// all inflow stood on one cell face. Returns work units (one per cell).
fn measure_film(c: &Compartment, r: &mut Run, d: Dims, fr: &Frame) -> u32 {
    r.area_known = true;
    let n = d.len();
    let mut kmin = f64::INFINITY;
    for cell in &c.cells {
        let i = cell.index();
        if i < n { kmin = kmin.min(fr.up.dot(d.centre(i))); }
    }
    if !kmin.is_finite() { return c.cells.len() as u32; }
    let edge = kmin + 0.5 * fr.h;
    let count = c.cells.iter().filter(|cell| cell.index() < n && fr.up.dot(d.centre(cell.index())) < edge).count();
    r.area = (count as f64 * CELL_VOLUME_M3 / fr.h).max(CELL_FACE_M2);
    if !(c.water_m3 > 0.0) { r.bottom = fr.base_y + kmin - 0.5 * fr.h; }
    (c.cells.len() as u32).saturating_mul(2)
}

/// Remove water from a compartment, lowering its running level.
fn take(c: &mut Compartment, r: &mut Run, v: f64) {
    c.water_m3 = (c.water_m3 - v).max(0.0);
    r.plane -= v / r.area;
    if c.water_m3 <= 0.0 { r.plane = r.bottom; }
}

/// Add water to a compartment, raising its running level (from the floor when it was dry).
fn give(c: &mut Compartment, r: &mut Run, v: f64) {
    if !(c.water_m3 > 0.0) || !r.plane.is_finite() { r.plane = r.bottom; }
    c.water_m3 = (c.water_m3 + v).min(r.volume);
    r.plane += v / r.area;
}
