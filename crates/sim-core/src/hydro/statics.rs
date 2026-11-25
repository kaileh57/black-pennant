//! Calm-water hydrostatics on the buoyancy blocks: the floating equilibrium, the righting-arm curve and the
//! spawn pose. Uses the same [`Kernel`] as the per-tick buoyancy force.
use super::blocks::{up_of, Kernel, Plane};
use super::{BuoyancyBlocks, Equilibrium};
use crate::compartments::CompartmentSet;
use crate::mass::MassProperties;
use content::ids::CompartmentId;
use content::cell::{CELL_M, CELL_VOLUME_M3};
use glam::DVec3;

const MAX_ITERS: usize = 120;
/// Largest plane rotation per iteration, radians.
const MAX_TURN: f64 = 0.12;
/// Heel given to an upright hull with no righting moment so the solver finds where it really settles.
const KICK: f64 = 0.03;

/// Waterplane sums about the centre of mass in plane coordinates (x along `e1`, z along `e2`).
#[derive(Clone, Copy, Default)]
struct Sums { vol: f64, moment: DVec3, area: f64, sx: f64, sz: f64, ixx: f64, ixz: f64, izz: f64 }

/// Two unit axes spanning the water plane: `e1` as near the bow as the plane allows, `e2 = u x e1`.
fn plane_axes(u: DVec3) -> (DVec3, DVec3) {
    let along = DVec3::X - u * u.x;
    let e1 = if along.length_squared() > 1e-12 { along.normalize() } else { (DVec3::Y - u * u.y).normalize_or(DVec3::Y) };
    (e1, u.cross(e1))
}

fn measure(blocks: &BuoyancyBlocks, holes: &[DVec3], plane: Plane, g: DVec3) -> Sums {
    let (e1, e2) = plane_axes(plane.u);
    let k = Kernel::with_waterplane(plane.u);
    let mut s = Sums::default();
    let mut add = |wet: f64, at: DVec3, area: f64, p: DVec3| {
        s.vol += wet;
        s.moment += at * wet;
        if area != 0.0 {
            let (x, z) = ((p - g).dot(e1), (p - g).dot(e2));
            s.area += area;
            s.sx += area * x;
            s.sz += area * z;
            s.ixx += area * x * x;
            s.ixz += area * x * z;
            s.izz += area * z * z;
        }
    };
    for i in 0..blocks.blocks.len() { blocks.eval_block(i, &k, plane.c, &mut add); }
    // A cell open to the sea fills to the outside level: its wetted part displaces nothing, and its free
    // surface comes off the waterplane.
    let h = CELL_M * (plane.u.x.abs() + plane.u.y.abs() + plane.u.z.abs());
    for &p in holes {
        let f = ((plane.c - plane.u.dot(p)) / h + 0.5).clamp(0.0, 1.0);
        if f <= 0.0 { continue; }
        let area = if f < 1.0 { CELL_VOLUME_M3 / h } else { 0.0 };
        add(-CELL_VOLUME_M3 * f, p - plane.u * (0.5 * h * (1.0 - f)), -area, p);
    }
    s
}

/// A solved floating state.
#[derive(Clone, Copy)]
pub(crate) struct Floating {
    pub plane: Plane,
    pub vol: f64,
    pub cb: DVec3,
    /// Transverse stiffness over volume at this state: the metacentric height in metres.
    pub gm: f64,
    pub converged: bool,
}

/// Iterate a plane until the hull displaces `target` and its centre of buoyancy stands under the centre of
/// mass. With `free_heel` false the plane only sinks and trims. The iteration is Newton on the hydrostatic
/// stiffness where that is positive definite, and plain descent on the potential energy where it is not, so
/// it settles on a stable equilibrium (the angle of loll of a hull with negative initial stability), never
/// on an unstable one.
fn settle(blocks: &BuoyancyBlocks, holes: &[DVec3], target: f64, g: DVec3, mut plane: Plane, free_heel: bool) -> Floating {
    let form = &blocks.form;
    let span = (form.hi - form.lo).max_element().max(1.0);
    let depth = (form.hi - form.lo).length().max(1.0);
    let mut last = Floating { plane, vol: 0.0, cb: g, gm: 0.0, converged: false };
    for iter in 0..MAX_ITERS {
        let s = measure(blocks, holes, plane, g);
        let (e1, e2) = plane_axes(plane.u);
        let arm = s.moment - g * s.vol;
        let (r0, m1, m2, bgu) = (s.vol - target, arm.dot(e1), arm.dot(e2), arm.dot(plane.u));
        // Stiffness with heave eliminated.
        let wet = s.area > 1e-9;
        let (h11, h22, h12, q1, q2) = if wet {
            (s.ixx + bgu - s.sx * s.sx / s.area, s.izz + bgu - s.sz * s.sz / s.area, s.ixz - s.sx * s.sz / s.area, m1 - s.sx * r0 / s.area, m2 - s.sz * r0 / s.area)
        } else { (bgu, bgu, 0.0, m1, m2) };
        last = Floating { plane, vol: s.vol, cb: if s.vol > 0.0 { s.moment / s.vol } else { g }, gm: if s.vol > 0.0 { h22 / s.vol } else { 0.0 }, converged: false };
        let tol = 1e-10 * target.max(CELL_VOLUME_M3);
        let balanced = r0.abs() <= tol && m1.abs() <= tol * span && (!free_heel || m2.abs() <= tol * span);
        let stable = h11 > 0.0 && (!free_heel || (h22 > 0.0 && h11 * h22 - h12 * h12 > 0.0));
        if balanced && stable { last.converged = true; return last; }
        // Rotation step.
        let floor = 1e-7 * target.max(CELL_VOLUME_M3) * span;
        let (mut da, mut db);
        if stable && free_heel {
            let det = h11 * h22 - h12 * h12;
            da = -(q1 * h22 - q2 * h12) / det;
            db = -(q2 * h11 - q1 * h12) / det;
        } else {
            da = if h11 > 0.0 { -q1 / h11 } else if q1.abs() > tol * span { -q1 / h11.abs().max(floor) } else { KICK };
            db = if !free_heel { 0.0 } else if h22 > 0.0 { -q2 / h22 } else if q2.abs() > tol * span { -q2 / h22.abs().max(floor) } else { -KICK };
        }
        // Late iterations take shorter steps so a plane hopping between two block faces still settles.
        let relax = if iter > 60 { 0.25 } else if iter > 30 { 0.5 } else { 1.0 };
        da = (da * relax).clamp(-MAX_TURN, MAX_TURN);
        db = (db * relax).clamp(-MAX_TURN, MAX_TURN);
        let dc = if wet { (-(r0 + s.sx * da + s.sz * db) / s.area * relax).clamp(-0.5 * depth, 0.5 * depth) } else if r0 < 0.0 { 0.25 * depth } else { -0.25 * depth };
        // s'(p) = s(p) + dc + da x + db z, with x and z measured from the centre of mass.
        let u = plane.u - e1 * da - e2 * db;
        let c = plane.c + dc - da * g.dot(e1) - db * g.dot(e2);
        let len = u.length();
        if !(len.is_finite() && len > 1e-9 && c.is_finite()) { return last; }
        plane = Plane { u: u / len, c: c / len };
    }
    last
}

/// Centres of the air cells of the flooded compartments, vessel-local metres.
fn flooded_cells(blocks: &BuoyancyBlocks, flooded: &[CompartmentId], compartments: &CompartmentSet) -> Vec<DVec3> {
    let (sx, sz) = (blocks.size[0] as usize, blocks.size[2] as usize);
    let mut out = Vec::new();
    if sx == 0 || sz == 0 { return out; }
    let mut ids: Vec<CompartmentId> = flooded.to_vec();
    ids.sort();
    ids.dedup();
    for id in ids {
        let Some(c) = compartments.get(id) else { continue };
        for cell in &c.cells {
            let i = cell.index();
            out.push(DVec3::new((i % sx) as f64 + 0.5, (i / (sx * sz)) as f64 + 0.5, ((i / sx) % sz) as f64 + 0.5) * CELL_M);
        }
    }
    out
}

/// Upright start plane for a target volume, from the hull's even-keel volume table. The solve corrects it for
/// any open cells in a step or two.
fn start_plane(blocks: &BuoyancyBlocks, target: f64) -> Plane {
    Plane { u: DVec3::Y, c: blocks.form.upright_waterline(target).unwrap_or(0.5 * (blocks.form.lo.y + blocks.form.hi.y)) }
}

/// The two-phase solve shared by every caller: on an even keel first (for the initial metacentric height and
/// the trim), then with heel free. `holes` are cells open to the sea. Returns the upright state, the final
/// state and the buoyant volume in hand.
pub(crate) fn float(blocks: &BuoyancyBlocks, holes: &[DVec3], mass: &MassProperties, rho: f64) -> Option<(Floating, Floating, f64)> {
    let total = blocks.form.total_m3 - holes.len() as f64 * CELL_VOLUME_M3;
    let target = mass.mass_kg / rho;
    if !(target.is_finite() && target > 0.0 && mass.com.is_finite() && total > 0.0) || target >= total { return None; }
    let upright = settle(blocks, holes, target, mass.com, start_plane(blocks, target), false);
    let free = settle(blocks, holes, target, mass.com, upright.plane, true);
    Some((upright, free, total))
}

/// The plane a hull floats at when held upright: sunk and trimmed to carry its weight, heel not solved.
/// `None` when it cannot float.
pub(crate) fn upright_plane(blocks: &BuoyancyBlocks, mass: &MassProperties, rho: f64) -> Option<Plane> {
    let target = mass.mass_kg / rho;
    if !(target.is_finite() && target > 0.0 && mass.com.is_finite() && blocks.form.total_m3 > 0.0) || target >= blocks.form.total_m3 { return None; }
    Some(settle(blocks, &[], target, mass.com, start_plane(blocks, target), false).plane)
}

pub(crate) fn water_density(tuning: &content::tuning::BuoyancyTuning) -> f64 {
    if tuning.water_density.is_finite() && tuning.water_density > 0.0 { tuning.water_density } else { super::SEA_WATER_DENSITY }
}

/// Heel (positive to starboard) and trim (positive bow up) of a body-frame up direction.
pub(crate) fn heel_trim_of_up(u: DVec3) -> (f64, f64) { (libm::atan2(-u.z, u.y), libm::asin(u.x.clamp(-1.0, 1.0))) }

pub(crate) fn equilibrium(blocks: &BuoyancyBlocks, mass: &MassProperties, flooded: &[CompartmentId], compartments: &CompartmentSet, tuning: &content::tuning::BuoyancyTuning) -> Equilibrium {
    let rho = water_density(tuning);
    let holes = if flooded.is_empty() { Vec::new() } else { flooded_cells(blocks, flooded, compartments) };
    let displacement_t = if mass.mass_kg.is_finite() && mass.mass_kg > 0.0 { mass.mass_kg / 1000.0 } else { 0.0 };
    let Some((upright, free, total)) = float(blocks, &holes, mass, rho) else {
        // Too heavy for its buoyant volume (or no volume at all): it goes down on an even keel.
        return Equilibrium { displacement_t, draft_m: blocks.form.hi.y - blocks.form.lo.y, cb: blocks.form.centroid, floats: false, ..Equilibrium::default() };
    };
    let (heel, trim) = heel_trim_of_up(free.plane.u);
    let draft = (free.plane.c + blocks.keel_reach(free.plane.u)).max(0.0);
    Equilibrium {
        displacement_t,
        draft_m: draft,
        heel_rad: heel,
        trim_rad: trim,
        reserve: if free.vol > 0.0 { ((total - free.vol) / free.vol).max(0.0) } else { 0.0 },
        gm_m: upright.gm,
        cb: free.cb,
        floats: free.converged && heel.abs() < core::f64::consts::FRAC_PI_2,
    }
}

pub(crate) fn gz_curve(blocks: &BuoyancyBlocks, mass: &MassProperties, step_deg: f64, tuning: &content::tuning::BuoyancyTuning) -> Vec<(f64, f64)> {
    let step = if step_deg.is_finite() && step_deg > 0.0 { step_deg.clamp(0.25, 180.0) } else { 2.0 };
    let n = libm::floor(180.0 / step + 1e-9) as usize;
    let angles = (0..=n).map(|k| k as f64 * step);
    let rho = water_density(tuning);
    let Some((upright, _, _)) = float(blocks, &[], mass, rho) else { return angles.map(|a| (a, 0.0)).collect() };
    let target = mass.mass_kg / rho;
    let trim = heel_trim_of_up(upright.plane.u).1;
    let g = mass.com;
    let reach = (blocks.form.hi - blocks.form.lo).length() + 1.0;
    let mid = 0.5 * (blocks.form.hi + blocks.form.lo);
    let mut c = upright.plane.c;
    let mut out = Vec::with_capacity(n + 1);
    for a in angles {
        let u = up_of(a.to_radians(), trim);
        // Sink the heeled hull until it carries its weight: Newton on the waterplane area inside a bracket.
        let (mut lo, mut hi) = (u.dot(mid) - reach, u.dot(mid) + reach);
        c = c.clamp(lo, hi);
        let mut s = measure(blocks, &[], Plane { u, c }, g);
        for _ in 0..60 {
            let r = s.vol - target;
            if r.abs() <= 1e-10 * target { break; }
            if r < 0.0 { lo = c; } else { hi = c; }
            let newton = if s.area > 1e-9 { c - r / s.area } else { f64::NAN };
            c = if newton > lo && newton < hi { newton } else { 0.5 * (lo + hi) };
            s = measure(blocks, &[], Plane { u, c }, g);
        }
        let gz = if s.vol > 0.0 { -(s.moment / s.vol - g).cross(u).x } else { 0.0 };
        out.push((a, gz));
    }
    out
}
