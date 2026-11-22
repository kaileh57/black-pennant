//! Forces that move a hull through the water: resistance, damping, thrust and steering, plus the planar
//! model built from the same laws that predicts speed and turning circle and moves kinematic hulls.
//!
//! Every law is a function of body-frame quantities, called both by the per-tick 3D code (velocities taken
//! at world points, relative to the waves) and by the planar model (velocities from surge, sway and yaw rate
//! in calm water). The Speed gauge therefore tracks the simulation by construction (L229).
use super::blocks::{HullForm, Plane};
use super::statics::{heel_trim_of_up, water_density};
use super::{BuoyancyBlocks, HydroForces, PointForce};
use crate::grid::VesselGrid;
use crate::mass::MassProperties;
use crate::math::{Kinematics, Pose};
use crate::parts::{PartRuntime, PartState};
use crate::physics::GRAVITY;
use crate::time::DT;
use crate::waves::WaveSet;
use content::catalogue::Catalogue;
use content::cell::CELL_M;
use content::kit::part_rot_matrix;
use content::part::PartFunction;
use content::tuning::BuoyancyTuning;
use glam::{DQuat, DVec3};

/// Strips along the hull that carry the lateral (crossflow) resistance. Their spread is what damps yaw and
/// gives a turning circle that scales with length.
pub(crate) const STRIPS: usize = 8;
/// Heave damping as a fraction of critical: waves a hull makes as it bobs carry its energy away.
const HEAVE_DAMPING: f64 = 0.35;
/// Share of the buoyant volume still above water below which wave-making damping fades to nothing.
const SURFACE_FADE: f64 = 0.1;
/// Roll takes this share of the plan-area damping that pitch takes in full; hulls roll far more freely than
/// they pitch.
const ROLL_SHARE: f64 = 0.4;
/// Speeds added to the quadratic laws so a coasting hull comes to rest instead of creeping for ever.
const SURGE_FLOOR_MPS: f64 = 0.05;
const SWAY_FLOOR_MPS: f64 = 0.25;
/// Mean draft at which the bottom counts as fully wetted.
const WET_DRAFT_M: f64 = 0.25;
/// How far up from half draft toward the centre of mass the hull's lateral resistance acts. At half draft
/// (0.0) a hull that turns as tightly as these do heels out by 25 to 40 degrees, which the short, fast, tall
/// hulls of this game reach at a sixth of a g; real hulls of that speed carry chines and flare that bank
/// them the other way. Halfway up, a hard turn at full speed heels a sound hull out by 2 to 14 degrees with
/// a peak near 20 as it goes in, and a tender one still goes over.
const LATERAL_LIFT: f64 = 0.5;
/// The centre of the hull's lateral resistance lies this share of its length abaft the centre of mass, as
/// far as leaning the strips' shares of the lateral area toward one end can put it ([`MAX_STRIP_LEAN`]). A
/// hull whose resistance centre leads its centre of mass will not hold a course: any sway swings the bow
/// further off, and a jet boat with no rudder to act as a fin spins out and rolls over at full speed. With
/// the margin every sanely loaded hull tracks, as its deadwood and skeg make a real one do, and the helm
/// still decides the turn.
const TRACKING_MARGIN: f64 = 0.04;
/// Share of the slender-body lift a hull's side makes when it moves ahead with some leeway.
const HULL_LIFT: f64 = 1.0;
const MAX_STRIP_LEAN: f64 = 0.9;
/// Thrust a propulsor delivers astern, as a share of its thrust ahead at the same speed, and how much more
/// form drag a hull makes going stern first. Together they give about six tenths of the ahead speed astern
/// and a crash stop from full ahead in three to eight lengths.
const ASTERN_THRUST: f64 = 0.45;
const ASTERN_FORM: f64 = 2.0;
/// Share of shaft power a propulsor turns into thrust power at speed.
pub const PROPULSIVE_EFFICIENCY: f64 = 0.65;
/// Corner speed of the thrust law when a propulsor reports thrust but no shaft power.
const DEFAULT_CORNER_MPS: f64 = 6.0;
/// Normal-force slope of a rudder blade per unit sine of incidence (aspect ratio about 1.3), and the
/// coefficient at which the blade stalls.
const FOIL_SLOPE: f64 = 2.2;
const FOIL_MAX: f64 = 1.3;
/// Share of a propeller race's added speed that a rudder standing in it feels.
const WASH_COVER: f64 = 0.7;
/// A rudder within this distance of a race's edge is in the race.
const WASH_MARGIN_M: f64 = 0.75;
const MIN_DISC_M2: f64 = 0.2;

/// What the force functions read from a hull. Built from a [`crate::vessel::Vessel`] by the public
/// wrappers; tests build it by hand.
#[derive(Clone, Copy)]
pub(crate) struct HullState<'a> {
    pub kin: &'a Kinematics,
    pub mass: &'a MassProperties,
    pub blocks: &'a BuoyancyBlocks,
    pub parts: &'a [PartRuntime],
    pub grid: &'a VesselGrid,
    /// Time the forces computed now act for, seconds: one tick, or two for a hull stepped at 30 Hz (the
    /// physics world applies its forces for the tick it sat out as well).
    pub step_s: f64,
}

/// Drag coefficients and density, sanitised.
#[derive(Clone, Copy)]
pub(crate) struct Laws { pub rho: f64, pub form: f64, pub skin: f64, pub lateral: f64, pub angular: f64 }
impl Laws {
    pub(crate) fn new(t: &BuoyancyTuning) -> Laws {
        let ok = |v: f64| if v.is_finite() && v > 0.0 { v } else { 0.0 };
        Laws { rho: water_density(t), form: ok(t.drag_form), skin: ok(t.drag_skin), lateral: ok(t.drag_lateral), angular: ok(t.drag_angular) }
    }
}

/// Reference dimensions of the displacing hull.
#[derive(Clone, Copy)]
pub(crate) struct HullRef {
    pub length: f64,
    /// Mean beam: plan area over length.
    pub beam: f64,
    pub plan: f64,
    /// Mid-length, keel height and mid-beam in vessel-local metres.
    pub mid: DVec3,
    pub depth: f64,
    pub total: f64,
    pub lo_x: f64,
}
impl HullRef {
    pub(crate) fn of(form: &HullForm) -> Option<HullRef> {
        let length = form.hi.x - form.lo.x;
        if !(form.total_m3 > 0.0 && form.plan_m2 > 0.0 && length > 0.0 && form.lo.is_finite() && form.hi.is_finite()) { return None; }
        Some(HullRef { length, beam: form.plan_m2 / length, plan: form.plan_m2, mid: DVec3::new(0.5 * (form.lo.x + form.hi.x), form.lo.y, 0.5 * (form.lo.z + form.hi.z)), depth: (form.hi.y - form.lo.y).max(CELL_M), total: form.total_m3, lo_x: form.lo.x })
    }
}

/// Wetted reference areas for a displaced volume: the hull is treated as a prism of its plan area sunk to its
/// mean draft.
#[derive(Clone, Copy)]
pub(crate) struct Wetted { pub volume: f64, pub draft: f64, pub section_m2: f64, pub lateral_m2: f64, pub skin_m2: f64, pub wet: f64 }
pub(crate) fn wetted(h: &HullRef, volume: f64) -> Wetted {
    let volume = if volume.is_finite() { volume.clamp(0.0, h.total) } else { 0.0 };
    let draft = (volume / h.plan).min(h.depth);
    let wet = (draft / WET_DRAFT_M).min(1.0);
    Wetted { volume, draft, section_m2: volume / h.length, lateral_m2: h.length * draft, skin_m2: h.plan * wet + 2.0 * draft * (h.length + h.beam), wet }
}

/// Keep an explicit damping force from doing more than halving the velocity it opposes within the step it
/// acts for, so no hull, however light, can be driven unstable by its own drag: not at 60 Hz, not at the
/// 30 Hz distant rate, and not at any time compression (which runs more ticks, never longer ones).
#[inline]
fn limit(force: f64, inertia: f64, velocity: f64, step_s: f64) -> f64 {
    let cap = 0.5 * inertia * velocity.abs() / step_s;
    if cap.is_finite() { force.clamp(-cap, cap) } else { 0.0 }
}

/// A usable step length: `step_s` when positive and finite, else one tick.
#[inline]
fn step_or_tick(step_s: f64) -> f64 { if step_s.is_finite() && step_s > 0.0 { step_s } else { DT } }

/// Share of a device under water: `depth_m` is how far its centre is below the surface and `half_m` half its
/// vertical extent. A propeller breaking the surface races and a rudder half out of the water has half a
/// blade; both are dry with their lowest point at the surface.
#[inline]
pub(crate) fn immersion(depth_m: f64, half_m: f64) -> f64 {
    let half = half_m.max(0.5 * CELL_M);
    let f = (depth_m + half) / (2.0 * half);
    if f.is_finite() { f.clamp(0.0, 1.0) } else { 0.0 }
}

/// Thrust in newtons of a propulsor at an advance speed, from its static thrust and absorbed shaft power.
///
/// `static_thrust_n` is the bollard thrust ahead (negative for an astern order) and `advance_mps` the speed
/// of the hull through the water in the direction the propulsor pushes it. At rest the thrust is the static
/// thrust; at speed it falls toward `PROPULSIVE_EFFICIENCY * power / speed`, the two blended as
/// `T0 / sqrt(1 + (v / v_c)^2)` with corner speed `v_c = PROPULSIVE_EFFICIENCY * power / T0`. Going
/// backwards against the thrust (a crash stop) gives the static thrust. An astern order delivers 0.45 of
/// what the same order ahead would at every speed: blades and buckets work badly backwards.
pub fn thrust_at_speed(static_thrust_n: f64, shaft_kw: f64, advance_mps: f64) -> f64 {
    if !static_thrust_n.is_finite() || static_thrust_n == 0.0 { return 0.0; }
    let power_w = if shaft_kw.is_finite() && shaft_kw > 0.0 { shaft_kw * 1000.0 } else { 0.0 };
    let corner = if power_w > 0.0 { PROPULSIVE_EFFICIENCY * power_w / static_thrust_n.abs() } else { DEFAULT_CORNER_MPS };
    let a = if advance_mps.is_finite() { advance_mps.max(0.0) / corner } else { 0.0 };
    if !a.is_finite() { return 0.0; }
    let thrust = static_thrust_n / (1.0 + a * a).sqrt();
    if thrust < 0.0 { thrust * ASTERN_THRUST } else { thrust }
}

/// Hull resistance in the horizontal plane. `rel(p)` is the hull's velocity at the local point `p` relative
/// to the water, as (ahead, up, to starboard) components in the hull's sea frame: the horizontal direction
/// of the bow, the world vertical and the horizontal across them. `emit(p, f)` receives each force in the
/// same frame and where it acts. One surge force at mid-length and half draft (twice the form drag going
/// astern), then [`STRIPS`] lateral forces along the hull, raised from half draft toward the height of the
/// centre of mass `com` by [`LATERAL_LIFT`] and sharing the lateral area so that its centre lies
/// [`TRACKING_MARGIN`] abaft `com`. Working in the sea frame keeps a hull on its beam ends, or upside down,
/// from sliding over the water unresisted.
#[allow(clippy::too_many_arguments)]
pub(crate) fn hull_resistance(h: &HullRef, w: &Wetted, laws: &Laws, mass: f64, com: DVec3, step_s: f64, mut rel: impl FnMut(DVec3) -> DVec3, mut emit: impl FnMut(DVec3, DVec3)) {
    if !(w.volume > 0.0) { return; }
    let y = h.mid.y + 0.5 * w.draft;
    let centre = DVec3::new(h.mid.x, y, h.mid.z);
    let u = rel(centre).x;
    let form = if u < 0.0 { laws.form * ASTERN_FORM } else { laws.form };
    let surge = -0.5 * laws.rho * (form * w.section_m2 + laws.skin * w.skin_m2) * (u.abs() + SURGE_FLOOR_MPS) * u;
    emit(centre, DVec3::X * limit(surge, mass, u, step_s));
    let n = STRIPS as f64;
    let k = 0.5 * laws.rho * laws.lateral * w.lateral_m2 / n;
    // A hull making way is a wing of very low aspect ratio: slender-body lift 1/2 rho pi T^2 u v. It is what
    // makes a moving hull hold its line against a small push that crossflow drag alone hardly resists.
    let lift = 0.5 * laws.rho * core::f64::consts::PI * w.draft * w.draft * u.abs() / n * HULL_LIFT;
    let y = if com.y.is_finite() { y + LATERAL_LIFT * (com.y - y) } else { y };
    // Strip `i` stands at `xi` (-1 at the stern, 1 at the bow) and takes the share `1 - lean * xi` of the
    // area, which puts the centre of the shares `lean * (n^2 - 1) / (3 n^2)` half-lengths abaft mid-length.
    let aft = (h.mid.x - com.x) / h.length + TRACKING_MARGIN;
    let lean = if aft.is_finite() { (2.0 * aft * 3.0 * n * n / (n * n - 1.0)).clamp(-MAX_STRIP_LEAN, MAX_STRIP_LEAN) } else { 0.0 };
    for i in 0..STRIPS {
        let xi = (2.0 * i as f64 + 1.0) / n - 1.0;
        let share = 1.0 - lean * xi;
        let p = DVec3::new(h.mid.x + 0.5 * h.length * xi, y, h.mid.z);
        let v = rel(p).z;
        emit(p, DVec3::Z * limit(-share * (k * (v.abs() + SWAY_FLOOR_MPS) + lift) * v, mass * share / n, v, step_s));
    }
}

/// A working propulsor: where it sits, which way it pushes the hull (steering included), how hard, its disc
/// area and the disc radius.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Thruster { pub pos: DVec3, pub dir: DVec3, pub static_n: f64, pub shaft_kw: f64, pub disc_m2: f64, pub half_m: f64 }
/// A rudder blade: position, unit normal at its present angle, span axis, area, damage factor and half its
/// span.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Foil { pub pos: DVec3, pub normal: DVec3, pub span: DVec3, pub area_m2: f64, pub factor: f64, pub half_m: f64 }
#[derive(Clone, Debug, Default)]
pub(crate) struct Devices { pub thrusters: Vec<Thruster>, pub foils: Vec<Foil> }

/// Extent of a part's footprint in metres along the vessel axes (one cell when it has no valid cell).
fn footprint_m(grid: &VesselGrid, p: &PartRuntime) -> DVec3 {
    let (mut lo, mut hi) = ([i16::MAX; 3], [i16::MIN; 3]);
    for c in &p.cells {
        if c.index() >= grid.design.cells.len() { continue; }
        let q = grid.pos(*c);
        for (k, v) in [q.x, q.y, q.z].into_iter().enumerate() { lo[k] = lo[k].min(v); hi[k] = hi[k].max(v); }
    }
    if lo[0] > hi[0] { return DVec3::splat(CELL_M); }
    DVec3::new((hi[0] - lo[0] + 1) as f64, (hi[1] - lo[1] + 1) as f64, (hi[2] - lo[2] + 1) as f64) * CELL_M
}

/// Forward, up and starboard axes of a placed part in the vessel frame.
fn part_axes(p: &PartRuntime) -> (DVec3, DVec3, DVec3) {
    let m = part_rot_matrix(p.rot);
    let v = |r: [i8; 3]| DVec3::new(r[0] as f64, r[1] as f64, r[2] as f64);
    let (axis, up) = (v(m[0]), v(m[1]));
    (axis, up, axis.cross(up))
}

fn angle_or_zero(a: f64) -> f64 { if a.is_finite() { a.clamp(-core::f64::consts::FRAC_PI_2, core::f64::consts::FRAC_PI_2) } else { 0.0 } }

impl Devices {
    fn push_thruster(&mut self, grid: &VesselGrid, p: &PartRuntime, static_n: f64, shaft_kw: f64, steer_rad: f64) {
        if !(static_n.is_finite() && static_n != 0.0 && p.centre.is_finite()) { return; }
        let (axis, up, lat) = part_axes(p);
        let (s, c) = libm::sincos(angle_or_zero(steer_rad));
        let ext = footprint_m(grid, p);
        let d = ext.dot(up.abs()).min(ext.dot(lat.abs()));
        let disc = (core::f64::consts::FRAC_PI_4 * d * d).max(MIN_DISC_M2);
        // Positive deflection swings the bow to starboard: the jet pushes the stern to port.
        self.thrusters.push(Thruster { pos: p.centre, dir: axis * c + up.cross(axis) * s, static_n, shaft_kw: if shaft_kw.is_finite() { shaft_kw.abs() } else { 0.0 }, disc_m2: disc, half_m: 0.5 * d });
    }
    fn push_foil(&mut self, grid: &VesselGrid, p: &PartRuntime, angle_rad: f64) {
        if !p.centre.is_finite() { return; }
        let (axis, up, lat) = part_axes(p);
        let (s, c) = libm::sincos(angle_or_zero(angle_rad));
        let ext = footprint_m(grid, p);
        // The blade is the footprint seen from the side: chord along the part's length, span along its height.
        // The catalogue's `area_m2` cannot reach the simulation through the frozen signatures, so the
        // predictor uses the same figure.
        let area = ext.dot(axis.abs()) * ext.dot(up.abs());
        self.foils.push(Foil { pos: p.centre, normal: axis * s + lat * c, span: up, area_m2: area, factor: p.output_factor(), half_m: 0.5 * ext.dot(up.abs()) });
    }

    /// Devices as the part states stand this tick.
    pub(crate) fn from_state(parts: &[PartRuntime], grid: &VesselGrid) -> Devices {
        let mut d = Devices::default();
        for p in parts {
            if !p.works() { continue; }
            match p.state {
                PartState::Propulsor { shaft_kw, thrust_n, steer_rad } => d.push_thruster(grid, p, thrust_n, shaft_kw, steer_rad),
                PartState::Rudder { angle_rad } => d.push_foil(grid, p, angle_rad),
                _ => {}
            }
        }
        d
    }

    /// Devices at full power ahead with the helm at `helm` (-1 port ..= 1 starboard), from the catalogue:
    /// every working engine's shaft power is pooled and shared by the working propulsors in proportion to
    /// what each can absorb, as `parts::step_parts` does.
    pub(crate) fn from_defs(parts: &[PartRuntime], grid: &VesselGrid, catalogue: &Catalogue, helm: f64) -> Devices {
        let def = |p: &PartRuntime| catalogue.parts.get_index(p.def.index()).map(|(_, d)| &d.function);
        let pos = |v: f64| if v.is_finite() && v > 0.0 { v } else { 0.0 };
        let (mut engine_kw, mut cap_kw) = (0.0, 0.0);
        for p in parts.iter().filter(|p| p.works()) {
            match def(p) {
                Some(PartFunction::Engine(e)) => engine_kw += pos(e.shaft_kw) * p.output_factor(),
                Some(PartFunction::Propulsor(pd)) => cap_kw += pos(pd.max_shaft_kw) * p.output_factor(),
                _ => {}
            }
        }
        let mut d = Devices::default();
        for p in parts.iter().filter(|p| p.works()) {
            match def(p) {
                Some(PartFunction::Propulsor(pd)) if cap_kw > 0.0 => {
                    let cap = pos(pd.max_shaft_kw) * p.output_factor();
                    let kw = (engine_kw * cap / cap_kw).min(cap);
                    d.push_thruster(grid, p, pos(pd.thrust_n_per_kw) * kw, kw, helm * pos(pd.steer_max_deg).to_radians());
                }
                Some(PartFunction::Rudder { max_deg, .. }) => d.push_foil(grid, p, helm * pos(*max_deg).to_radians()),
                _ => {}
            }
        }
        d
    }
}

/// Thrust and rudder forces. `flow(p)` gives the water's velocity relative to the hull at the local point
/// `p` (body frame) and how far that point is below the surface in metres (negative above it); `up` is the
/// world vertical in the body frame; `inertia_at(p)` the mass a sideways push at `p` has to move; `step_s`
/// the time the forces act for; `emit(p, f)` receives the forces.
///
/// Each device works in proportion to the share of its height that is under water ([`immersion`]), and a
/// part-wetted rudder's force acts at the middle of its wet span.
pub(crate) fn device_forces(d: &Devices, rho: f64, up: DVec3, step_s: f64, inertia_at: impl Fn(DVec3) -> f64, mut flow: impl FnMut(DVec3) -> (DVec3, f64), mut emit: impl FnMut(DVec3, DVec3)) {
    // Each race: origin, direction it blows, added speed, radius.
    let mut races: Vec<(DVec3, DVec3, f64, f64)> = Vec::with_capacity(if d.foils.is_empty() { 0 } else { d.thrusters.len() });
    for t in &d.thrusters {
        let (v, depth) = flow(t.pos);
        let sign = t.static_n.signum();
        let advance = -v.dot(t.dir) * sign;
        let thrust = thrust_at_speed(t.static_n, t.shaft_kw, advance) * immersion(depth, t.half_m);
        if !(thrust.is_finite() && thrust != 0.0) { continue; }
        emit(t.pos, t.dir * thrust);
        if !d.foils.is_empty() {
            // Momentum theory: the far race moves at sqrt(v^2 + 2 T / (rho A)).
            let adv = advance.max(0.0);
            let added = (adv * adv + 2.0 * thrust.abs() / (rho * t.disc_m2)).sqrt() - adv;
            races.push((t.pos, -t.dir * sign, added, (t.disc_m2 / core::f64::consts::PI).sqrt()));
        }
    }
    for f in &d.foils {
        let (mut v, depth) = flow(f.pos);
        // The blade's vertical reach shrinks as the hull lies over.
        let rise = f.span.dot(up);
        let half = f.half_m * rise.abs();
        let wet = immersion(depth, half);
        if !(wet > 0.0 && f.area_m2 > 0.0 && f.factor > 0.0) { continue; }
        // Middle of the wet part of the span, which hangs from the blade's lowest point.
        let at = f.pos - f.span * (f.half_m * (1.0 - wet) * if rise >= 0.0 { 1.0 } else { -1.0 });
        let mut best = 0.0;
        let mut wash = DVec3::ZERO;
        for &(origin, dir, added, radius) in &races {
            let r = f.pos - origin;
            let along = r.dot(dir);
            if !(along > 0.0 && (r - dir * along).length() <= radius + WASH_MARGIN_M) { continue; }
            // The race is no wider than the disc that made it: a blade larger than the disc is only partly in it.
            let felt = added * WASH_COVER * (core::f64::consts::PI * radius * radius / f.area_m2).min(1.0);
            if felt > best { best = felt; wash = dir * felt; }
        }
        v += wash;
        let vp = v - f.span * v.dot(f.span);
        let speed = vp.length();
        if !(speed.is_finite() && speed > 1e-9) { continue; }
        let vn = vp.dot(f.normal);
        let cn = (FOIL_SLOPE * vn.abs() / speed).min(FOIL_MAX);
        let force = 0.5 * rho * f.area_m2 * cn * speed * speed * vn.signum() * wet * f.factor;
        emit(at, f.normal * limit(force, inertia_at(at), vn, step_s));
    }
}

/// Yaw inertia about the centre of mass, floored so a degenerate tensor cannot divide by zero.
fn yaw_inertia(mass: &MassProperties) -> f64 {
    let i = mass.inertia.col(1)[1];
    let floor = mass.mass_kg * CELL_M * CELL_M / 6.0;
    if i.is_finite() && i > floor { i } else { floor }
}

/// Mass a sideways push at the local point `p` has to move: the body's mass reduced by the lever about its
/// centre of mass.
fn inertia_at(mass: &MassProperties, yaw_i: f64, p: DVec3) -> f64 {
    let d = p - mass.com;
    1.0 / (1.0 / mass.mass_kg + (d.x * d.x + d.z * d.z) / yaw_i)
}

fn usable(mass: &MassProperties, kin: &Kinematics) -> bool {
    mass.mass_kg.is_finite() && mass.mass_kg > 0.0 && mass.com.is_finite() && kin.pose.pos.is_finite() && kin.pose.rot.is_finite() && kin.lin_vel.is_finite() && kin.ang_vel.is_finite()
}

pub(crate) fn drag(hull: &HullState, waves: &WaveSet, t: f64, tuning: &BuoyancyTuning, out: &mut HydroForces) {
    let Some(h) = HullRef::of(&hull.blocks.form) else { return };
    if !usable(hull.mass, hull.kin) { return; }
    let w = wetted(&h, out.submerged.clamp(0.0, 1.0) * h.total);
    if !(w.volume > 0.0) { return; }
    let laws = Laws::new(tuning);
    let (pose, kin, m) = (hull.kin.pose, hull.kin, hull.mass.mass_kg);
    let com = pose.to_world(hull.mass.com);
    // Sea frame: the bow's horizontal direction (the keel-to-deck line if the bow points straight up or down),
    // the vertical, and the horizontal to starboard.
    let level = |v: DVec3| DVec3::new(v.x, 0.0, v.z);
    let bow = level(pose.rot * DVec3::X);
    let ahead = if bow.length_squared() > 1e-6 { bow.normalize() } else { level(pose.rot * DVec3::Y).normalize_or(DVec3::X) };
    let stbd = ahead.cross(DVec3::Y);
    let to_sea = |v: DVec3| DVec3::new(v.dot(ahead), v.y, v.dot(stbd));
    let from_sea = |f: DVec3| ahead * f.x + DVec3::Y * f.y + stbd * f.z;
    // Orbital velocity of the water and the slope of the surface at the quarter points, interpolated along the
    // hull.
    let (xa, xf) = (h.mid.x - 0.25 * h.length, h.mid.x + 0.25 * h.length);
    let y_wl = h.mid.y + w.draft;
    let orbit = |x: f64| { let p = pose.to_world(DVec3::new(x, y_wl, h.mid.z)); waves.sample(p.x, p.z, t) };
    let slope_of = |n: DVec3| if n.y > 1e-6 && n.is_finite() { DVec3::new(-n.x / n.y, 0.0, -n.z / n.y) } else { DVec3::ZERO };
    let ((va, ga), (vf, gf)) = if waves.count == 0 { ((DVec3::ZERO, DVec3::ZERO), (DVec3::ZERO, DVec3::ZERO)) } else {
        let (a, f) = (orbit(xa), orbit(xf));
        ((a.velocity, slope_of(a.normal)), (f.velocity, slope_of(f.normal)))
    };
    let along = |x: f64| (x - xa) / (xf - xa);
    let water = |x: f64| va + (vf - va) * along(x);
    let through = |p: DVec3| { let at = pose.to_world(p); kin.lin_vel + kin.ang_vel.cross(at - com) - water(p.x) };
    let step = step_or_tick(hull.step_s);
    hull_resistance(&h, &w, &laws, m, hull.mass.com, step, |p| to_sea(through(p)), |p, f| out.forces.push(PointForce { force: from_sea(f), at: pose.to_world(p) }));

    // Heave: wave-making damping plus plate drag on the bottom, along the vertical at mid-hull. They act on the
    // hull's vertical speed relative to the surface it floats on. Under a hull that moves over the water the
    // surface rises at the water's own vertical speed plus the hull's horizontal speed through the water times
    // the slope it runs up (the Gerstner surface moves with its particles), so a hull making way over a swell
    // is not taken for one plunging into calm water, and one riding a wave at the wave's speed meets a surface
    // that keeps its height. In calm water the slope is zero and this is the hull's own vertical speed.
    let centre = DVec3::new(h.mid.x, h.mid.y + 0.5 * w.draft, h.mid.z);
    let rel = through(centre);
    let slope = ga + (gf - ga) * along(centre.x);
    let rise = rel.y - (rel.x * slope.x + rel.z * slope.z);
    // A hull wholly under water makes no waves: the wave-making part fades out over the last tenth.
    let surface = ((1.0 - w.volume / h.total) / SURFACE_FADE).clamp(0.0, 1.0);
    let linear = 2.0 * HEAVE_DAMPING * laws.rho * h.plan * (GRAVITY * w.draft).sqrt() * surface;
    let plate = 0.5 * laws.rho * laws.lateral * h.plan * w.wet;
    out.forces.push(PointForce { force: DVec3::Y * limit(-(linear + plate * rise.abs()) * rise, m, rise, step), at: pose.to_world(centre) });

    // Roll and pitch: the same damping spread over the plan area, about the sea frame's horizontal axes.
    let rot_inv = pose.rot.inverse();
    let floor = m * CELL_M * CELL_M / 6.0;
    let about = |axis: DVec3| { let a = rot_inv * axis; let i = a.dot(hull.mass.inertia * a); if i.is_finite() && i > floor { i } else { floor } };
    let (roll_rate, pitch_rate) = (kin.ang_vel.dot(ahead), kin.ang_vel.dot(stbd));
    let (b, l) = (h.beam, h.length);
    let roll = limit(-(laws.angular * ROLL_SHARE * linear * b * b / 12.0 + plate * b * b * b / 32.0 * roll_rate.abs()) * roll_rate, about(ahead), roll_rate, step);
    let pitch = limit(-(laws.angular * linear * l * l / 12.0 + plate * l * l * l / 32.0 * pitch_rate.abs()) * pitch_rate, about(stbd), pitch_rate, step);
    out.torque += ahead * roll + stbd * pitch;
}

pub(crate) fn propulsion(hull: &HullState, waves: &WaveSet, t: f64, rho: f64, out: &mut HydroForces) {
    if !usable(hull.mass, hull.kin) { return; }
    let devices = Devices::from_state(hull.parts, hull.grid);
    if devices.thrusters.is_empty() && devices.foils.is_empty() { return; }
    let (pose, kin) = (hull.kin.pose, hull.kin);
    let rot_inv = pose.rot.inverse();
    let com = pose.to_world(hull.mass.com);
    let yaw_i = yaw_inertia(hull.mass);
    let flow = |p: DVec3| {
        let at = pose.to_world(p);
        let s = waves.sample(at.x, at.z, t);
        (rot_inv * (s.velocity - kin.lin_vel - kin.ang_vel.cross(at - com)), s.height - at.y)
    };
    let up = (rot_inv * DVec3::Y).normalize_or(DVec3::Y);
    device_forces(&devices, rho, up, step_or_tick(hull.step_s), |p| inertia_at(hull.mass, yaw_i, p), flow, |p, f| out.forces.push(PointForce { force: pose.rot * f, at: pose.to_world(p) }));
}

/// Surge, sway (positive to starboard) and yaw rate about +Y (positive swings the bow to port) of a hull in
/// calm water, at its centre of mass in the body frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Planar { pub u: f64, pub v: f64, pub r: f64 }

pub(crate) struct PlanarBody<'a> {
    pub mass: &'a MassProperties,
    pub hull: HullRef,
    pub wetted: Wetted,
    pub laws: Laws,
    pub devices: &'a Devices,
    /// Calm-water plane the hull floats at, which decides how much of each propeller and rudder is wet.
    pub plane: Plane,
    /// Step the damping caps are sized for, seconds.
    pub step_s: f64,
}

impl PlanarBody<'_> {
    /// Net surge force, sway force and yaw moment about +Y at a planar state.
    pub(crate) fn forces(&self, s: Planar) -> (f64, f64, f64) {
        let com = self.mass.com;
        let yaw_i = yaw_inertia(self.mass);
        let hull_vel = |p: DVec3| { let d = p - com; DVec3::new(s.u + s.r * d.z, 0.0, s.v - s.r * d.x) };
        let mut sum = (0.0, 0.0, 0.0);
        let mut add = |p: DVec3, f: DVec3| { let d = p - com; sum.0 += f.x; sum.1 += f.z; sum.2 += d.z * f.x - d.x * f.z; };
        hull_resistance(&self.hull, &self.wetted, &self.laws, self.mass.mass_kg, com, self.step_s, hull_vel, &mut add);
        // Devices use the fixed sea-water density, as `propulsion` must (it is given no tuning table).
        let plane = self.plane;
        device_forces(self.devices, super::SEA_WATER_DENSITY, plane.u, self.step_s, |p| inertia_at(self.mass, yaw_i, p), |p| (-hull_vel(p), plane.c - plane.u.dot(p)), &mut add);
        sum
    }
    /// One explicit step of the planar equations of motion in the rotating body frame.
    pub(crate) fn step(&self, s: Planar, dt: f64) -> Planar {
        let (x, z, n) = self.forces(s);
        let m = self.mass.mass_kg;
        Planar { u: s.u + (x / m - s.r * s.v) * dt, v: s.v + (z / m + s.r * s.u) * dt, r: s.r + n / yaw_inertia(self.mass) * dt }
    }
}

pub(crate) fn predict(blocks: &BuoyancyBlocks, mass: &MassProperties, parts: &[PartRuntime], grid: &VesselGrid, catalogue: &Catalogue, tuning: &BuoyancyTuning) -> (f64, f64) {
    let none = (0.0, f64::INFINITY);
    let Some(hull) = HullRef::of(&blocks.form) else { return none };
    if !(mass.mass_kg.is_finite() && mass.mass_kg > 0.0 && mass.com.is_finite()) { return none; }
    let laws = Laws::new(tuning);
    let wet = wetted(&hull, mass.mass_kg / laws.rho);
    let ahead = Devices::from_defs(parts, grid, catalogue, 0.0);
    let dt = 0.05;
    // The hull as it floats at rest, sunk and trimmed but upright: a stern trimmed out of the water lifts the
    // propellers and rudders with it. A hull that cannot float is taken with everything immersed.
    let plane = super::statics::upright_plane(blocks, mass, laws.rho).unwrap_or(Plane { u: DVec3::Y, c: blocks.form.hi.y + 1.0e3 });
    let body = PlanarBody { mass, hull, wetted: wet, laws, devices: &ahead, plane, step_s: dt };
    let net = |u: f64| body.forces(Planar { u, v: 0.0, r: 0.0 }).0;
    if ahead.thrusters.is_empty() || !(net(0.0) > 0.0) { return none; }
    // Thrust falls and resistance rises with speed, so the balance is a single crossing.
    let (mut lo, mut hi) = (0.0, 200.0);
    for _ in 0..60 { let mid = 0.5 * (lo + hi); if net(mid) > 0.0 { lo = mid; } else { hi = mid; } }
    let top = 0.5 * (lo + hi);
    // Hard over to starboard at full power: run the planar model until the turn is steady.
    let turning = Devices::from_defs(parts, grid, catalogue, 1.0);
    let body = PlanarBody { devices: &turning, ..body };
    let mut s = Planar { u: top, v: 0.0, r: 0.0 };
    let mut mark = s;
    for i in 1..=24_000 {
        s = body.step(s, dt);
        if !(s.u.is_finite() && s.v.is_finite() && s.r.is_finite()) { return (top, f64::INFINITY); }
        if i % 100 == 0 {
            let settled = (s.r - mark.r).abs() <= 1e-7 * s.r.abs().max(1e-9) && (s.u - mark.u).abs() <= 1e-7 * s.u.abs().max(1e-9) && (s.v - mark.v).abs() <= 1e-7 * s.u.abs().max(1e-9);
            if settled { break; }
            mark = s;
        }
    }
    let speed = libm::hypot(s.u, s.v);
    (top, if s.r.abs() > 1e-6 && speed > 1e-6 { speed / s.r.abs() } else { f64::INFINITY })
}

/// Time constants of the kinematic wave ride, seconds.
const RIDE_HEAVE_S: f64 = 0.7;
const RIDE_TILT_S: f64 = 2.0;
/// Largest wave-following tilt of a kinematic hull, radians.
const RIDE_MAX_TILT: f64 = 0.35;
/// Rate a kinematic hull with no buoyancy left goes down, m/s.
const KINEMATIC_SINK_MPS: f64 = 1.0;

fn quat_axis(axis: usize, angle: f64) -> DQuat {
    let (s, c) = libm::sincos(0.5 * angle);
    match axis { 0 => DQuat::from_xyzw(s, 0.0, 0.0, c), 1 => DQuat::from_xyzw(0.0, s, 0.0, c), _ => DQuat::from_xyzw(0.0, 0.0, s, c) }
}

pub(crate) fn kinematic(hull: &HullState, waves: &WaveSet, t: f64, dt: f64, tuning: &BuoyancyTuning) -> Kinematics {
    let kin = *hull.kin;
    if !(dt.is_finite() && dt > 0.0 && t.is_finite()) || !usable(hull.mass, &kin) { return kin; }
    let coast = Kinematics { pose: Pose { pos: kin.pose.pos + kin.lin_vel * dt, rot: kin.pose.rot }, ..kin };
    let Some(h) = HullRef::of(&hull.blocks.form) else { return coast };
    let laws = Laws::new(tuning);
    let (m, com) = (hull.mass.mass_kg, hull.mass.com);
    let volume = m / laws.rho;
    let rot_inv = kin.pose.rot.inverse();
    // Planar state from the present motion, read in the level frame of the heading (so the lean and heave of
    // the wave ride never leak into surge, sway or yaw), advanced one step by the same laws as full physics.
    let bow = kin.pose.rot * DVec3::X;
    let yaw_now = libm::atan2(-bow.z, bow.x);
    let (sn, cn) = libm::sincos(yaw_now);
    let now = Planar { u: kin.lin_vel.x * cn - kin.lin_vel.z * sn, v: kin.lin_vel.x * sn + kin.lin_vel.z * cn, r: kin.ang_vel.y };
    let devices = Devices::from_state(hull.parts, hull.grid);
    // Devices are wetted as on the level calm-water line for the present mass (all under when it cannot float).
    let calm = hull.blocks.form.upright_waterline(volume);
    let plane = Plane { u: DVec3::Y, c: calm.unwrap_or(hull.blocks.form.hi.y + 1.0e3) };
    let body = PlanarBody { mass: hull.mass, hull: h, wetted: wetted(&h, volume), laws, devices: &devices, plane, step_s: dt };
    let s = body.step(now, dt);
    if !(s.u.is_finite() && s.v.is_finite() && s.r.is_finite()) { return coast; }
    let yaw = yaw_now + s.r * dt;
    let (sy, cy) = libm::sincos(yaw);
    let vel = DVec3::new(s.u * cy + s.v * sy, 0.0, -s.u * sy + s.v * cy);
    let com_w = kin.pose.to_world(com);
    let com_xz = com_w + vel * dt;
    let (heel, trim) = heel_trim_of_up(rot_inv * DVec3::Y);
    let Some(y_wl) = calm else {
        // Heavier than everything it can displace: it goes down as it lies.
        let rot = (quat_axis(1, s.r * dt) * kin.pose.rot).normalize();
        let down = DVec3::new(vel.x, -KINEMATIC_SINK_MPS, vel.z);
        let pos = com_w + down * dt - rot * com;
        return Kinematics { pose: Pose { pos, rot }, lin_vel: down, ang_vel: DVec3::Y * s.r };
    };
    // Ride the waves: heave to the mean of four heights under the hull, lean to their slopes, each with a lag.
    let ride = DVec3::new(h.mid.x, y_wl, h.mid.z);
    let level = quat_axis(1, yaw);
    let at = com_xz + level * (ride - com);
    let (fore, side) = (DVec3::new(cy, 0.0, -sy) * (h.length / 3.0), DVec3::new(sy, 0.0, cy) * (h.beam / 3.0));
    let height = |p: DVec3| waves.height_at(p.x, p.z, t + dt);
    let (hb, hs, hp, hr) = (height(at + fore), height(at - fore), height(at - side), height(at + side));
    let lean = |rise: f64, run: f64| libm::atan2(rise, run).clamp(-RIDE_MAX_TILT, RIDE_MAX_TILT);
    let blend = |now: f64, want: f64, tau: f64| now + (want - now) * (dt / (tau + dt));
    let trim = blend(trim, lean(hb - hs, 2.0 * h.length / 3.0), RIDE_TILT_S);
    let heel = blend(heel, lean(hp - hr, 2.0 * h.beam / 3.0), RIDE_TILT_S);
    let ride_y = blend(kin.pose.to_world(ride).y, 0.25 * (hb + hs + hp + hr), RIDE_HEAVE_S);
    let rot = (level * quat_axis(2, trim) * quat_axis(0, heel)).normalize();
    let pos = DVec3::new(com_xz.x - (rot * com).x, ride_y - (rot * ride).y, com_xz.z - (rot * com).z);
    let pose = Pose { pos, rot };
    let lin_vel = DVec3::new(vel.x, (pose.to_world(com).y - com_w.y) / dt, vel.z);
    if !(pos.is_finite() && rot.is_finite() && lin_vel.is_finite()) { return coast; }
    Kinematics { pose, lin_vel, ang_vel: DVec3::Y * s.r }
}

/// Depth-keeping response: natural frequency of the depth loop and of the levelling loop, rad/s.
const DEPTH_RATE: f64 = 0.25;
const LEVEL_RATE: f64 = 0.5;
/// Vertical acceleration the ballast alone can command, and what the planes add per (m/s)^2 of speed.
const BALLAST_ACCEL: f64 = 0.4;
const PLANE_ACCEL_PER_V2: f64 = 0.02;
const MAX_DEPTH_ACCEL: f64 = 1.5;
const MAX_LEVEL_ACCEL: f64 = 0.3;

pub(crate) fn depth_keeping(hull: &HullState, depth_m: f64, rho: f64, out: &mut HydroForces) {
    let Some(h) = HullRef::of(&hull.blocks.form) else { return };
    if !usable(hull.mass, hull.kin) || !depth_m.is_finite() { return; }
    let (kin, m) = (hull.kin, hull.mass.mass_kg);
    let pose = kin.pose;
    // Ballast trims out whatever buoyancy the hull has at this instant, so it hangs neutral at any depth.
    let submerged = if out.submerged > 0.0 || out.samples > 0 { out.submerged.clamp(0.0, 1.0) } else { 1.0 };
    let surplus = rho * GRAVITY * h.total * submerged;
    let keel = pose.to_world(DVec3::new(h.mid.x, h.mid.y, h.mid.z)).y;
    let error = -depth_m.max(0.0) - keel;
    let rot_inv = pose.rot.inverse();
    let speed = (rot_inv * kin.lin_vel).x;
    let authority = (BALLAST_ACCEL + PLANE_ACCEL_PER_V2 * speed * speed).min(MAX_DEPTH_ACCEL);
    let accel = (DEPTH_RATE * DEPTH_RATE * error - 2.0 * DEPTH_RATE * kin.lin_vel.y).clamp(-authority, authority);
    // The ballast's weight acts at the centre of mass, and the buoyancy it trims out is taken where that buoyancy
    // acts: the resultant `buoyancy` pushed first into `out` (the documented call order). Cancelling it at the
    // centre of mass instead would leave the full buoyancy moment about it, which pitches a real hull whose centre
    // of buoyancy submerged is not over its centre of mass.
    let com = pose.to_world(hull.mass.com);
    let cob = out.forces.first().map(|f| f.at).filter(|p| out.submerged > 0.0 && p.is_finite()).unwrap_or(com);
    out.forces.push(PointForce { force: DVec3::Y * (m * GRAVITY + m * accel), at: com });
    out.forces.push(PointForce { force: DVec3::Y * -surplus, at: cob });
    // Planes and trim tanks hold the boat level.
    let (heel, trim) = heel_trim_of_up(rot_inv * DVec3::Y);
    let spin = rot_inv * kin.ang_vel;
    let floor = m * CELL_M * CELL_M / 6.0;
    let moment = |k: usize| { let i = hull.mass.inertia.col(k)[k]; if i.is_finite() && i > floor { i } else { floor } };
    let level = |angle: f64, rate: f64| (-LEVEL_RATE * LEVEL_RATE * angle - 2.0 * LEVEL_RATE * rate).clamp(-MAX_LEVEL_ACCEL, MAX_LEVEL_ACCEL);
    out.torque += pose.rot * DVec3::new(moment(0) * level(heel, spin.x), 0.0, moment(2) * level(trim, spin.z));
}
