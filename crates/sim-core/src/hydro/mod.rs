//! Unit `sim-core-hydro` (hard: numerically subtle physics inside a 1.25 ms share).
//! Buoyancy, drag, propulsion and steering forces, plus the hydrostatics the editor gauges predict with.
//! DESIGN.md L650 (buoyancy blocks), L658 (far-hull fallback), L1114 (parallel per vessel, serial merge),
//! L153-175 (Float and Speed gauges, GZ curve), L1549 (analytic buoyancy for kinematic hulls).
//!
//! Buoyancy uses 2 m blocks (4x4x4 cells) carrying a solid-plus-watertight fill fraction. Water height is
//! sampled once per 2 m column per tick and shared by all blocks above it. Heel, list and capsize emerge from
//! the physics; there is no stability meter. Forces for different vessels are computed in parallel and applied
//! to the physics world serially in ascending vessel slot order; floats are never summed in parallel.
//!
//! # Model (binding for callers)
//! - A cell displaces water when it is a live solid cell, air inside a compartment, or a walled-in void
//!   (`compartments::VOID_CELL`). Air open to the sea displaces nothing. A block's `centre` is the centroid
//!   of its displacing cells and `height_m` their vertical extent, so a keel plate or a deck edge sits where
//!   it really is.
//! - One kernel (`blocks::Kernel`) turns a block and a water height into wetted volume. A block the surface
//!   cuts is taken one 0.5 m cell layer at a time, each layer four prisms at the Gauss points of its plan
//!   extent, wetted linearly over its projected height; a block wholly under or wholly dry costs one test.
//!   The per-tick force, [`equilibrium`], [`gz_curve`] and [`floating_pose`] all use it, so the Float gauge
//!   and the simulation cannot drift apart, and both follow the cells to within a few percent.
//! - [`buoyancy`] returns one resultant: the summed upward force at the centre of buoyancy. It also sets
//!   `HydroForces::submerged`, which [`drag`] and [`depth_keeping`] read, so call it first with the same `out`.
//! - Angles: heel is positive to starboard (starboard side down), trim is positive bow up; [`heel_trim`]
//!   reads both from a rotation. A pose with heel and trim is `heading * Rz(trim) * Rx(heel)`.
//! - Resistance: surge drag `1/2 rho (drag_form * section + drag_skin * wetted skin) v^2` on the mean
//!   submerged section and a box estimate of the wetted skin, with twice the form drag going astern. Lateral
//!   resistance on eight strips along the hull, which is what damps yaw and sets the turning circle: crossflow
//!   drag `1/2 rho drag_lateral * area v^2` plus the slender-body lift `1/2 rho pi T^2 u v` of a hull making
//!   way. Two choices there are the game's, not the textbook's. The strips share the lateral area so that its
//!   centre lies 4% of the length abaft the centre of mass (as far as a 0.1 to 1.9 lean of the shares can put
//!   it, which covers a centre of mass up to a tenth of the length abaft mid-length), so a hull holds its
//!   course with the helm amidships whether or not it has a rudder to act as a fin. And the lateral force acts
//!   halfway up from half draft to the centre of mass, so a hard turn at full speed heels a sound hull out by
//!   2 to 14 degrees, not the 25 to 40 that half draft gives short, fast, tall hulls. Heave damping is 0.35 of
//!   critical, with roll and pitch damping from the same plan area scaled by `drag_angular`. All are taken
//!   relative to the waves' orbital velocity and scale with the displaced volume. Heave damping acts on the
//!   hull's vertical speed relative to the surface under it: the water's own vertical speed plus the hull's
//!   horizontal speed through the water times the surface slope, both read at the two quarter points. Without
//!   the slope term a hull making way over a swell reads as one plunging into calm water, and the damping
//!   becomes a vertical force that grows with speed and swings with the encounter; acting below the centre of
//!   mass of a hull that lies over, it heels the hull further, and an undamaged hull rolled over at speed in
//!   seas it rides out adrift. Each damping force is capped at half the momentum it opposes over the step it
//!   acts for (one tick, or two for a `Full30` hull), so drag can never drive a light hull unstable.
//! - Propulsion reads part states only (the frozen signature gives it no catalogue): a propulsor's
//!   `thrust_n` is its static thrust along the part's +X (negative astern) and `shaft_kw` the power behind
//!   it, as `parts::step_parts` writes them; [`thrust_at_speed`] is the speed law, and an astern order
//!   delivers 0.45 of the thrust ahead. `steer_rad` swings a waterjet's thrust. A rudder's blade area is its
//!   footprint seen from the side (chord by span), because `PartFunction::Rudder::area_m2` cannot reach the
//!   simulation; [`predict_speed_turn`] uses the same figure. A rudder standing in a propulsor's race feels
//!   the race. A propulsor or rudder works in proportion to the share of its height that is under water, so
//!   a stern trimmed or pitched out of the sea loses thrust and helm, in the simulation and in the Speed
//!   gauge alike. Sea water density there is [`SEA_WATER_DENSITY`].
//! - [`kinematic_step`] has no tuning argument and uses the default drag coefficients;
//!   [`kinematic_step_with`] takes the live table and is what the world should call. A kinematic hull rides
//!   level in calm water: it does not carry the static trim or list a full-physics hull settles to.
use crate::compartments::CompartmentSet;
use crate::grid::VesselGrid;
use crate::mass::MassProperties;
use crate::math::{Kinematics, Pose};
use crate::vessel::{Vessel, VesselPrefab};
use crate::waves::WaveSet;
use content::tuning::BuoyancyTuning;
use glam::DVec3;
use serde::{Deserialize, Serialize};

mod blocks;
mod forces;
mod statics;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod sea_trials;
#[cfg(test)]
mod seakeeping;

pub use forces::{thrust_at_speed, PROPULSIVE_EFFICIENCY};

/// Sea water density in kg/m^3 used where no tuning table is in reach (and when the table's value is unusable).
pub const SEA_WATER_DENSITY: f64 = 1025.0;

/// One buoyancy block: centre in vessel-local metres, enclosed volume in m^3 (cells that displace water:
/// solid cells and air sealed inside watertight structure), and its water column.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Block { pub centre: DVec3, pub volume_m3: f64, pub height_m: f64, pub column: u32 }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BuoyancyBlocks {
    pub block_cells: u8,
    /// Ascending (y, z, x).
    pub blocks: Vec<Block>,
    /// Column positions in vessel-local (x, z) metres.
    pub columns: Vec<[f64; 2]>,
    /// Grid size in cells the blocks were built for.
    #[serde(default)]
    pub(crate) size: [u16; 3],
    /// Exact extents of each block's displacing cells, parallel to `blocks`.
    #[serde(default)]
    pub(crate) ext: Vec<blocks::BlockExt>,
    /// Cell layers of every block, bottom layer first, block after block (`ext` holds each block's range).
    #[serde(default)]
    pub(crate) layers: Vec<blocks::Layer>,
    /// Summary of the displacing hull, derived from the blocks.
    #[serde(default)]
    pub(crate) form: blocks::HullForm,
}
impl BuoyancyBlocks {
    /// Total displacing volume in m^3: the most water the hull can displace.
    pub fn total_volume_m3(&self) -> f64 { self.form.total_m3 }
}

/// Build blocks from the live grid. Flooded compartments do not reduce block volume here; flood water is
/// carried as mass (see `mass::with_fluids`). Unit: `sim-core-hydro`.
///
/// `block_cells` is clamped to 1..=8. Blocks with no displacing cell are left out; an inert grid gives an
/// empty set, which floats nothing.
pub fn build_blocks(grid: &VesselGrid, compartments: &CompartmentSet, block_cells: u8) -> BuoyancyBlocks { blocks::build(grid, compartments, block_cells) }
/// Rebuild only blocks overlapping dirty chunks. Unit: `sim-core-hydro`.
///
/// Reads `grid.dirty_chunks` without clearing it. The result equals [`build_blocks`] on the same grid and
/// compartments. A set built for another grid, or by hand, is rebuilt whole.
pub fn rebuild_dirty_blocks(blocks: &mut BuoyancyBlocks, grid: &VesselGrid, compartments: &CompartmentSet) { blocks::rebuild_dirty(blocks, grid, compartments) }

/// A force at a world point, produced in parallel and applied serially.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointForce { pub force: DVec3, pub at: DVec3 }

/// All hydro forces on one vessel for one tick.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HydroForces {
    pub forces: Vec<PointForce>,
    pub torque: DVec3,
    /// Buoyancy samples taken (work units for tracing and Complexity, L973).
    pub samples: u32,
    /// Submerged fraction of total block volume, 0.0..=1.0.
    pub submerged: f64,
}

/// Buoyancy: for each column sample `waves.height_at` once, then for each block above it the submerged height
/// fraction gives an upward force at the block centre. Unit: `sim-core-hydro`.
///
/// Appends one force, the resultant at the centre of buoyancy, adds the water heights sampled to
/// `out.samples` (one per column that can meet the surface) and sets `out.submerged`.
pub fn buoyancy(vessel: &Vessel, waves: &WaveSet, t: f64, tuning: &BuoyancyTuning, out: &mut HydroForces) {
    buoyancy_on(&vessel.blocks, &vessel.kin.pose, vessel.waterline_y, waves, t, statics::water_density(tuning), out)
}

/// Buoyancy of a block set at a pose. `ref_y` is the local height at which each column meets the water (the
/// design waterline); it only decides where the column's sample is taken on a heeled hull.
pub(crate) fn buoyancy_on(blocks: &BuoyancyBlocks, pose: &Pose, ref_y: f64, waves: &WaveSet, t: f64, rho: f64, out: &mut HydroForces) {
    out.submerged = 0.0;
    if blocks.blocks.is_empty() || !(blocks.form.total_m3 > 0.0) || !(pose.pos.is_finite() && pose.rot.is_finite()) { return; }
    let up = (pose.rot.inverse() * DVec3::Y).normalize_or(DVec3::Y);
    let ref_y = if ref_y.is_finite() { ref_y.clamp(blocks.form.lo.y, blocks.form.hi.y) } else { blocks.form.centroid.y };
    // One water height per column, shared by every block above it (L650). No wave stands higher than the sum
    // of the amplitudes, so a column wholly above or below that band needs no sample.
    let band: f64 = waves.waves.iter().take(waves.count as usize).map(|w| w.amplitude.abs()).sum();
    let kernel = blocks::Kernel::new(up);
    let mut span = vec![(f64::INFINITY, f64::NEG_INFINITY); blocks.columns.len()];
    for (i, b) in blocks.blocks.iter().enumerate() {
        let Some(s) = span.get_mut(b.column as usize) else { continue };
        // Heights of the block's lowest and highest points above the origin.
        let (mid, half) = blocks.bounds(i);
        let (mid, reach) = (up.dot(mid), kernel.reach(half));
        *s = (s.0.min(mid - reach), s.1.max(mid + reach));
    }
    let mut plane: Vec<f64> = Vec::with_capacity(blocks.columns.len());
    for (c, s) in blocks.columns.iter().zip(&span) {
        let (low, high) = (pose.pos.y + s.0, pose.pos.y + s.1);
        if !band.is_finite() || (low < band && high > -band) {
            let at = pose.to_world(DVec3::new(c[0], ref_y, c[1]));
            plane.push(waves.height_at(at.x, at.z, t) - pose.pos.y);
            out.samples += 1;
        } else {
            plane.push(if high <= -band { band } else { -band } - pose.pos.y);
        }
    }
    let (mut volume, mut moment) = (0.0, DVec3::ZERO);
    for (i, b) in blocks.blocks.iter().enumerate() {
        let Some(&c) = plane.get(b.column as usize) else { continue };
        blocks.eval_block(i, &kernel, c, &mut |v, at, _, _| { volume += v; moment += at * v; });
    }
    if !(volume > 0.0 && volume.is_finite() && moment.is_finite()) { return; }
    out.submerged = (volume / blocks.form.total_m3).clamp(0.0, 1.0);
    out.forces.push(PointForce { force: DVec3::Y * (rho * crate::physics::GRAVITY * volume), at: pose.to_world(moment / volume) });
}

/// Heel and trim in radians of a rotation: heel positive to starboard (-pi..=pi, beyond a right angle when
/// capsized), trim positive bow up.
pub fn heel_trim(rot: glam::DQuat) -> (f64, f64) {
    if !rot.is_finite() { return (0.0, 0.0); }
    statics::heel_trim_of_up((rot.inverse() * DVec3::Y).normalize_or(DVec3::Y))
}
/// Hull drag: form and skin friction along the bow axis, lateral resistance, angular damping, scaled by the
/// submerged fraction and relative to the water's orbital velocity. Unit: `sim-core-hydro`.
///
/// Reads `out.submerged` as [`buoyancy`] left it; with nothing submerged there is no drag.
pub fn drag(vessel: &Vessel, waves: &WaveSet, t: f64, tuning: &BuoyancyTuning, out: &mut HydroForces) { forces::drag(&hull_state(vessel), waves, t, tuning, out) }

fn hull_state(vessel: &Vessel) -> forces::HullState<'_> {
    let ticks = if vessel.fidelity == crate::vessel::Fidelity::Full30 { (crate::time::TICK_HZ / crate::time::DISTANT_HZ.max(1)).max(1) } else { 1 };
    forces::HullState { kin: &vessel.kin, mass: &vessel.mass, blocks: &vessel.blocks, parts: &vessel.parts, grid: &vessel.grid, step_s: crate::time::DT * ticks as f64 }
}
/// Propulsion and steering: thrust at each working propulsor from its shaft power and the speed-dependent
/// thrust law, zero when the propulsor is out of the water; rudder lift from flow speed and angle; waterjets
/// vector thrust. Reads part states written by `parts::step_parts`. Unit: `sim-core-hydro`.
pub fn propulsion(vessel: &Vessel, waves: &WaveSet, t: f64, out: &mut HydroForces) { forces::propulsion(&hull_state(vessel), waves, t, SEA_WATER_DENSITY, out) }
/// Act 3, AI-only submarines: when `vessel.submersible` and `controls.depth_m` is set, add the ballast and
/// plane forces that hold the ordered depth with neutral buoyancy. Unit: `sim-core-hydro` (Phase::Act3).
///
/// The ballast force cancels whatever net buoyancy the hull has (from `out.submerged`, so call it after
/// [`buoyancy`]; without that it assumes the boat is fully under) and a damped loop with limited authority,
/// greater at speed, drives the keel to the ordered depth while the planes hold the boat level. With no
/// ordered depth the boat is left to its own buoyancy and surfaces.
pub fn depth_keeping(vessel: &Vessel, waves: &WaveSet, t: f64, out: &mut HydroForces) {
    if !vessel.submersible { return; }
    if let Some(depth) = vessel.controls.depth_m { forces::depth_keeping(&hull_state(vessel), depth, SEA_WATER_DENSITY, out); }
}
/// Kinematic hulls: advance pose along the ordered course at a first-order speed response with analytic heave,
/// pitch and roll from the wave field under the hull (L1549). Unit: `sim-core-hydro`.
///
/// Uses the default drag coefficients; see [`kinematic_step_with`].
pub fn kinematic_step(vessel: &Vessel, waves: &WaveSet, t: f64, dt: f64) -> Kinematics { kinematic_step_with(vessel, waves, t, dt, &BuoyancyTuning::default()) }

/// [`kinematic_step`] with the live tuning table. The hull's surge, sway and yaw advance one step under the
/// same thrust, rudder and resistance laws as full physics (from its part states, in calm water), so its
/// speed and turn respond as the real hull's would and a change of fidelity is seamless. It rides the waves
/// at its calm-water draft for its present mass: heave follows the mean of four water heights under the hull
/// and heel and trim follow their slopes, each with a short lag. A hull heavier than all it can displace
/// goes down at 1 m/s.
pub fn kinematic_step_with(vessel: &Vessel, waves: &WaveSet, t: f64, dt: f64, tuning: &BuoyancyTuning) -> Kinematics { forces::kinematic(&hull_state(vessel), waves, t, dt, tuning) }

/// Static equilibrium of a floating body in calm water.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Equilibrium {
    pub displacement_t: f64,
    /// Keel depth below the surface in metres.
    pub draft_m: f64,
    pub heel_rad: f64,
    pub trim_rad: f64,
    /// Reserve buoyancy as a fraction of displacement.
    pub reserve: f64,
    /// Metacentric height in metres and centre of buoyancy in vessel-local metres.
    pub gm_m: f64,
    pub cb: DVec3,
    /// False when the hull cannot float (Float gauge Fail).
    pub floats: bool,
}

/// Solve calm-water equilibrium for blocks and mass, optionally with compartments open to the sea (the
/// Resilience gauge's 1- and 2-compartment cases, L657). Unit: `sim-core-hydro`.
///
/// A flooded compartment's air cells fill to the outside level, cell by cell, and displace nothing there
/// (lost buoyancy, free surface included), which is where the simulation ends up once such a compartment has
/// filled through its breach. `gm_m` is the transverse
/// metacentric height on an even keel at the equilibrium draft and trim (the initial GM, negative for a hull
/// that will not stand upright); `heel_rad` and `trim_rad` are where the hull actually settles, which for
/// such a hull is its angle of loll to starboard. `floats` is false when the hull is heavier than all the
/// water it can displace, or settles past a right angle.
pub fn equilibrium(blocks: &BuoyancyBlocks, mass: &MassProperties, flooded: &[content::ids::CompartmentId], compartments: &CompartmentSet, tuning: &BuoyancyTuning) -> Equilibrium { statics::equilibrium(blocks, mass, flooded, compartments, tuning) }
/// Righting-arm curve: (heel degrees, GZ metres) from 0 to 180 at `step_deg` (2 degrees; 10 while dragging,
/// L187). Unit: `sim-core-hydro`.
///
/// Heel is to starboard at the even-keel trim, the hull free to sink until it carries its weight; GZ is
/// positive when it rights the hull. A step that is not positive and finite reads as 2 degrees, and steps
/// below 0.25 degrees are raised to it. A hull that cannot float gives zeros.
pub fn gz_curve(blocks: &BuoyancyBlocks, mass: &MassProperties, step_deg: f64, tuning: &BuoyancyTuning) -> Vec<(f64, f64)> { statics::gz_curve(blocks, mass, step_deg, tuning) }
/// Pose of the grid origin that floats a prefab at equilibrium at a world position and heading.
/// Unit: `sim-core-hydro`.
///
/// Uses the prefab's dry mass. `(x, z)` is where the grid origin goes. A prefab that cannot float is set
/// level with its deck awash.
pub fn floating_pose(prefab: &VesselPrefab, x: f64, z: f64, heading_rad: f64, tuning: &BuoyancyTuning) -> Pose { floating_pose_of(&prefab.blocks, &prefab.dry_mass, x, z, heading_rad, tuning) }

/// [`floating_pose`] for any block set and mass (a loaded hull, a wreck piece).
pub fn floating_pose_of(blocks: &BuoyancyBlocks, mass: &MassProperties, x: f64, z: f64, heading_rad: f64, tuning: &BuoyancyTuning) -> Pose {
    let heading = if heading_rad.is_finite() { heading_rad } else { 0.0 };
    let (x, z) = (if x.is_finite() { x } else { 0.0 }, if z.is_finite() { z } else { 0.0 });
    let level = Pose::from_heading(DVec3::new(x, 0.0, z), heading);
    let Some((_, free, _)) = statics::float(blocks, &[], mass, statics::water_density(tuning)) else {
        return Pose { pos: DVec3::new(x, -blocks.form.hi.y, z), rot: level.rot };
    };
    let (heel, trim) = statics::heel_trim_of_up(free.plane.u);
    let (sh, ch) = libm::sincos(0.5 * heel);
    let (st, ct) = libm::sincos(0.5 * trim);
    let rot = (level.rot * glam::DQuat::from_xyzw(0.0, 0.0, st, ct) * glam::DQuat::from_xyzw(sh, 0.0, 0.0, ch)).normalize();
    // The body point p sits at world height pos.y + up . p, and the surface is where up . p = c.
    Pose { pos: DVec3::new(x, -free.plane.c, z), rot }
}
/// Analytic top speed (m/s) and steady turning radius (m) at full power from the same drag and thrust laws,
/// for the Speed gauge; the trial battery checks them against simulation within 10% (L229).
/// Unit: `sim-core-hydro`.
///
/// The hull is taken as it spawns: loaded (`mass::with_fluids` on the prefab's own part states, so fuel,
/// ammunition and ballast count) and floating at rest at its calm-water draft and trim, which decides how
/// much of each propeller and rudder is under water. Top speed is where thrust meets surge resistance with
/// every engine at full power. The turning radius is that of the centre of mass with the helm hard over to
/// starboard, found by running the planar (surge, sway, yaw) model on those laws to a steady turn. A hull
/// with no working propulsion gives `(0.0, f64::INFINITY)`, and one that cannot turn gives an infinite
/// radius.
pub fn predict_speed_turn(prefab: &VesselPrefab, catalogue: &content::catalogue::Catalogue, tuning: &BuoyancyTuning) -> (f64, f64) {
    let loaded = crate::mass::with_fluids(&prefab.dry_mass, &prefab.parts, &prefab.compartments, catalogue);
    forces::predict(&prefab.blocks, &loaded, &prefab.parts, &prefab.grid, catalogue, tuning)
}
