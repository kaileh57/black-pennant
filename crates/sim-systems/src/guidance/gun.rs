//! Unit `sys-gunnery` (hard: numerically subtle fixed-point iteration, gated at L526).
//! The Gunnery officer's `BallisticSolver` stage and the solver tables behind gauges and doctrine.
//! DESIGN.md L518 (gun solution), L205-208 (auto-set: ballistic solver tables from the projectile drag
//! model), L489 (effective range from single-round Pk), L526-527 (convergence gate and fallback).
//!
//! Iterate predicted target position and shell time of flight until the impact point moves less than 0.25 m
//! (half a cell) between iterations, capped at 6 iterations (`GuidanceTuning`). An unconverged solution fires
//! on the last iterate and logs "Solution unconverged". The flight model is exactly
//! `sim_core::projectiles::ballistic_step` (same drag k per calibre band) and the solution accounts for
//! turret slew time. It aims at the track estimate, never at truth.
//!
//! # Conventions (shared with manual aiming and sim-core's mounts)
//! - Mount frame: the vessel frame (+X bow, +Y up, +Z starboard) turned about +Y by the mount's zero bearing
//!   `zero_rad`. Training is a right-handed turn about +Y from the mount's zero, the same sense as
//!   `DQuat::from_rotation_y` and hull-mesh's Train joint: positive training swings the muzzle from the bow
//!   toward port. Elevation is measured up from the mount's deck plane (the vessel's XZ plane), so it rolls
//!   and pitches with the hull.
//! - Training angles are returned in the representation of the mount's limits: inside `[train_min,
//!   train_max]` for a limited arc (so a slew never has to cross the dead arc), in `(-pi, pi]` for a mount
//!   with a full circle.
//! - Velocity inheritance: a shell leaves the muzzle at `muzzle_mps` along the barrel relative to the firing
//!   hull, so its world velocity is `dir * muzzle_mps + own_vel` ([`shell_velocity`]). sim-core's mount step
//!   must spawn shells the same way.
//!
//! # Method
//! A shell under gravity and isotropic quadratic drag with no wind keeps the horizontal direction of its
//! velocity, so every trajectory lies in one vertical plane. The solver works in that plane and steps
//! `ballistic_step` with `sim_core::time::DT`, exactly as the projectile array does, reading the height where
//! the path crosses the target's horizontal range (linear interpolation inside the crossing step). The
//! low-angle elevation is found by a bracketed Illinois search. The outer loop iterates the total time to
//! impact (slew time plus time of flight): each iterate predicts the target position, solves the elevation for
//! it, derives the new slew and flight times, and stops when the predicted impact point moves less than the
//! convergence distance. The update uses a secant estimate of the fixed point's slope, which is still plain
//! iteration of position and time of flight but converges in two or three iterations against fast aircraft.
//!
//! # The hit model (L489; DECISION, k2)
//! [`predicted_pk`] is the single-round hit probability with the errors the officers' solver really has:
//! gun dispersion and the officer laying error ([`laying_sigma_rad`]); the target estimate's own error, from
//! the track's fire-control motion fit (`crate::tracks::MotionFit`, which the solver also leads with), aged
//! and extrapolated over the time of flight; a manoeuvre allowance (the spec's surface sigma_a of L463, plus
//! what the fit has measured of the target's turning); against the contact's silhouette from its radar
//! cross-section and aspect ([`silhouette`]), the crests of the sea state hiding its foot. The Punch gauge and
//! the default effective range ([`single_round_pk`]) read it for the 40 m reference hull under a steady lock
//! in Beaufort 4; the officers' open-fire test (`crate::engage::hit_chance`) for the contact in front of them.
//! It was fitted to the simulation, never the reverse: `gun_range_trace` with `BP_NOGATE` sums the predicted
//! chance of every round fired against the hits that landed (see [`SILHOUETTE_FILL`]).
use crate::prelude::*;
use crate::sensors::measurement_error;
use content::graph::MountLimits;
use content::part::weapons::BandDef;
use content::part::SensorKind;
use core::f64::consts::{FRAC_PI_2, PI, TAU};
use content::weather::SeaState;
use sim_core::rng::{self, Stream};
use sim_core::time::DT;

pub struct GunSolveIn {
    /// Muzzle position and own velocity in the world frame.
    pub muzzle: DVec3,
    pub own_vel: DVec3,
    /// Vessel pose (to express the answer as mount training and elevation) and the mount's zero bearing.
    pub pose: Pose,
    pub zero_rad: f64,
    pub limits: MountLimits,
    /// Track estimate.
    pub target_pos: DVec3,
    pub target_vel: DVec3,
    pub muzzle_mps: f64,
    pub drag_k: f64,
    /// Current mount angles and slew rates, to add slew time to the lead.
    pub train_rad: f64,
    pub elevation_rad: f64,
    pub train_rate: f64,
    pub elevation_rate: f64,
    pub iteration_cap: u8,
    pub converge_m: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GunSolution {
    /// Ordered mount angles (mount frame, radians), clamped to limits.
    pub train_rad: f64,
    pub elevation_rad: f64,
    pub aim_point: DVec3,
    pub time_of_flight_s: f64,
    pub iterations: u8,
    pub converged: bool,
    /// Movement of the impact point in the last iteration, metres.
    pub residual_m: f64,
    /// The solution needs angles outside the limits: (needed, limit) in degrees for the "Why?" chain (L353).
    pub train_out_of_limits: Option<(f64, f64)>,
    pub elevation_out_of_limits: Option<(f64, f64)>,
    pub in_range: bool,
}

// ---- solver constants (numerical, not gameplay) ----

/// Longest flight the solver integrates, seconds. Far beyond any gun's maximum range.
const MAX_FLIGHT_S: f64 = 180.0;
/// Longest slew time added to the lead, seconds (a mount that slow is effectively not tracking).
const MAX_SLEW_S: f64 = 30.0;
/// Height tolerance of the elevation search at the target's range, metres.
const HEIGHT_TOL_M: f64 = 0.01;
/// Elevation search stops when the bracket is narrower than this, radians.
const ELEV_TOL_RAD: f64 = 1e-10;
/// Highest elevation the search tries, radians (89.95 degrees). A steeper line of sight is shot straight up
/// or down ([`direct_shot`]).
const ELEV_MAX_RAD: f64 = 1.569_923_662_168_899_4;
/// Below this horizontal range the shot is treated as straight up or down the line of sight, metres.
const MIN_RANGE_M: f64 = 0.5;
/// Cap on evaluated trajectories per elevation search (the peak refinement may add 26 more).
const MAX_EVALS: u32 = 48;
/// A shell that falls this far below the target height while descending is reported short without
/// integrating further, metres (plus 5% of range).
const FLOOR_MARGIN_M: f64 = 50.0;
/// First upward step of the elevation scan when the start gives no usable crossing, radians (1 degree).
const SCAN_STEP_RAD: f64 = 0.017_453_292_519_943_295;

/// Default convergence distance when the input carries none (L518).
const DEFAULT_CONVERGE_M: f64 = 0.25;

/// World velocity of a shell fired along `dir` (unit) from a hull moving at `own_vel` (see the module doc).
pub fn shell_velocity(dir: DVec3, muzzle_mps: f64, own_vel: DVec3) -> DVec3 { dir * muzzle_mps + own_vel }

/// Solve one gun. Work units: iterations (returned). Unit: `sys-gunnery`.
///
/// Returns the clamped mount angles for the last iterate. `in_range` is false when no elevation reaches the
/// predicted target position (the mount is then laid at its longest-reaching elevation on the target's
/// bearing, and `converged` is false). Invalid input (non-finite values, no muzzle velocity) returns a
/// solution that holds the current angles with `in_range` and `converged` false and zero iterations.
pub fn solve(input: &GunSolveIn) -> GunSolution { solve_with(input, &sim_core::projectiles::ballistic_step) }

/// Elevation and time of flight to reach a horizontal range and height difference with a band's drag, by
/// stepping `ballistic_step`; `None` beyond maximum range. The inner routine of [`solve`] and of the tables.
/// Unit: `sys-gunnery`.
///
/// The low-angle (direct fire) solution, elevation in radians above the horizontal, for a shell fired from
/// a stationary muzzle.
pub fn elevation_for(range_m: f64, dy_m: f64, muzzle_mps: f64, drag_k: f64) -> Option<(f64, f64)> {
    elevation_for_with(range_m, dy_m, muzzle_mps, drag_k, &sim_core::projectiles::ballistic_step)
}

/// One row of a range table.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RangeRow { pub range_m: f64, pub elevation_rad: f64, pub time_of_flight_s: f64, pub impact_mps: f64, pub fall_angle_rad: f64 }

/// Ballistic solver table for one gun (auto-set value 5, L208): rows every 250 m out to the cap range.
/// Built at design time and cached in `ShipDesign`. Unit: `sys-gunnery`.
///
/// Rows start at 250 m and stop at the cap range or at the gun's maximum range, whichever is shorter; the
/// target is at muzzle height. `fall_angle_rad` is the angle of the shell's path below the horizontal at
/// the range. Empty for a non-positive or non-finite muzzle velocity or cap.
pub fn range_table(muzzle_mps: f64, band: &BandDef) -> Vec<RangeRow> { range_table_with(muzzle_mps, band, &sim_core::projectiles::ballistic_step) }

/// Predicted single-round hit probability against the reference hull of the Punch gauge and the default
/// effective range (L489): `target_len_m` long and `target_height_m` high seen broadside, with a beam of
/// [`REFERENCE_BEAM_FRACTION`] of its length, under a fire-control lock in Beaufort 4. The errors are the ones
/// the officers' solver really has ([`lock_errors`], [`laying_sigma_rad`], [`REFERENCE_MANOEUVRE_MPS2`]); see
/// [`predicted_pk`]. Unit: `sys-gunnery`.
///
/// The result is continuous, never rises with range, and is 0 beyond the table's last row.
pub fn single_round_pk(table: &[RangeRow], range_m: f64, dispersion_mrad: f64, target_len_m: f64, target_height_m: f64) -> f64 {
    let Some(last) = table.last() else { return 0.0 };
    if !range_m.is_finite() || !dispersion_mrad.is_finite() || !target_len_m.is_finite() || !target_height_m.is_finite() { return 0.0; }
    let range = range_m.max(0.0);
    if range > last.range_m + 1e-6 { return 0.0; }
    let (len, height) = (target_len_m.max(0.0), target_height_m.max(0.0));
    let target = TargetBox { width_m: len, height_m: height, depth_m: REFERENCE_BEAM_FRACTION * len, crest_m: crest_m(SeaState::BEAUFORT_4) };
    let angular = laying_sigma_rad(dispersion_mrad, 1.0, SeaState::BEAUFORT_4);
    let at = |r: f64| { let (var_pos, var_vel, cov) = lock_errors(r); predicted_pk(table, r, &ShotErrors { angular_rad: angular, var_pos, var_vel, cov, var_pos_along: var_pos, var_vel_along: var_vel, cov_along: cov, age_s: 0.0, manoeuvre_mps2: REFERENCE_MANOEUVRE_MPS2, manoeuvre_along_mps2: REFERENCE_MANOEUVRE_MPS2 }, &target) };
    let mut p = at(range);
    // Monotone envelope: never more likely to hit farther out than at any tabulated range short of it.
    for row in table.iter().take_while(|r| r.range_m <= range) { p = p.min(at(row.range_m)); }
    p.clamp(0.0, 1.0)
}

/// Aim error an officer or AI gunner adds on top of the solution: normal draws from `rng::Stream::AimError`
/// keyed (vessel, mount), scaled by the difficulty's accuracy multiplier (1.0 = stock officers, L851) and by
/// 3.0 under local control (L394). Unit: `sys-gunnery`.
///
/// Returns `[training, elevation]` errors in radians. The two axes use draws 0 and 2 (each normal deviate
/// consumes two draws). The local-control factor is `NetworkTuning::local_control_error_factor`.
pub fn aim_error(vessel: EntityId, slot: PartSlot, base_sigma_rad: f64, accuracy_mult: f64, local_control: bool, ctx: &SimCtx<'_>) -> [f64; 2] {
    let factor = if local_control { ctx.tuning.networks.local_control_error_factor } else { 1.0 };
    let sigma = aim_sigma(base_sigma_rad, accuracy_mult, factor);
    if sigma == 0.0 { return [0.0, 0.0]; }
    let key = ctx.key(Stream::AimError, vessel.to_u64(), slot.0 as u64);
    [rng::normal(key.draw(0)) * sigma, rng::normal(key.draw(2)) * sigma]
}

/// Mount frame conversion: world aim direction to (training, elevation) for a mount with zero bearing
/// `zero_rad` on a vessel at `pose`. Shared with manual aiming. Unit: `sys-gunnery`.
///
/// Training is in `(-pi, pi]`, elevation in `[-pi/2, pi/2]`; see the module doc for the sense. A zero or
/// non-finite direction gives `(0, 0)`.
pub fn world_dir_to_mount(pose: &Pose, zero_rad: f64, dir_world: DVec3) -> (f64, f64) {
    if !dir_world.is_finite() || dir_world.length_squared() < 1e-24 { return (0.0, 0.0); }
    let local = unit_rot(pose.rot).inverse() * dir_world;
    let z = if zero_rad.is_finite() { zero_rad } else { 0.0 };
    let (s, c) = (libm::sin(z), libm::cos(z));
    // Turn by -zero about +Y.
    let mx = local.x * c - local.z * s;
    let mz = local.x * s + local.z * c;
    let horiz = libm::sqrt(mx * mx + mz * mz);
    let train = if horiz < 1e-15 { 0.0 } else { libm::atan2(-mz, mx) };
    let elev = libm::atan2(local.y, horiz);
    (train, elev)
}

/// Inverse of [`world_dir_to_mount`]: the world unit direction of a barrel at (training, elevation).
pub fn mount_dir_to_world(pose: &Pose, zero_rad: f64, train_rad: f64, elevation_rad: f64) -> DVec3 {
    let (st, ct) = (libm::sin(train_rad), libm::cos(train_rad));
    let (se, ce) = (libm::sin(elevation_rad), libm::cos(elevation_rad));
    let m = DVec3::new(ce * ct, se, -ce * st);
    let (s, c) = (libm::sin(zero_rad), libm::cos(zero_rad));
    // Turn by +zero about +Y.
    let local = DVec3::new(m.x * c + m.z * s, m.y, -m.x * s + m.z * c);
    unit_rot(pose.rot) * local
}

// ---- The hit model (L489): the error the solver really has, against the target's silhouette ----

/// Officer laying error of a stabilised, officer-aimed mount, radians per axis: the base sigma of the aim-error
/// draw ([`aim_error`]) the officers add to every solution, in every sea state, scaled by the difficulty's
/// accuracy multiplier. (The name is historical; the mount's stabilisation takes out the hull's motion.)
pub const PLATFORM_SIGMA_B4_RAD: f64 = 1.0e-3;
/// Beam of the reference hull as a fraction of its length.
pub const REFERENCE_BEAM_FRACTION: f64 = 0.15;
/// Manoeuvre allowance over the time to impact, m/s^2: the spec's assumed manoeuvre acceleration for surface
/// tracks (L463). Doctrine reads `TrackTuning::sigma_a` instead; this is its shipped value for the gauge.
pub const REFERENCE_MANOEUVRE_MPS2: f64 = 0.3;
/// Fire-control radar fix rate the lock estimate is solved for, per second (the 10 Hz track rate, L412).
pub const LOCK_FIX_HZ: f64 = 10.0;

/// One round's error budget at the target, 1 sigma per axis (see [`predicted_pk`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ShotErrors {
    /// Angular error of the round's line: gun dispersion and laying, radians ([`laying_sigma_rad`]).
    pub angular_rad: f64,
    /// Variance of the target estimate across the line of fire (m^2), of its velocity (m^2/s^2) and their
    /// covariance (m^2/s), at the estimate's tick (`crate::tracks::MotionEstimate`); then the same along it.
    pub var_pos: f64,
    pub var_vel: f64,
    pub cov: f64,
    pub var_pos_along: f64,
    pub var_vel_along: f64,
    pub cov_along: f64,
    /// Age of the estimate when the round leaves, seconds: it is extrapolated on its velocity for this long
    /// plus the time of flight.
    pub age_s: f64,
    /// Acceleration the target may hold over that time, m/s^2, across the line of fire and along it.
    pub manoeuvre_mps2: f64,
    pub manoeuvre_along_mps2: f64,
}

/// What a round must pass through: the target's silhouette seen down the line of fire. `width_m` across the
/// line of fire, `height_m` above the waterline, `depth_m` along the line of fire; `crest_m` is the height of
/// the wave crests that hide its foot from a round coming in low ([`crest_m`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TargetBox { pub width_m: f64, pub height_m: f64, pub depth_m: f64, pub crest_m: f64 }
impl TargetBox {
    /// How far above the waterline the solver lays: over the crests that hide the hull's foot, never more than
    /// half the silhouette (a hull rides up with the sea it sits in). 0 in calm water: the waterline itself.
    pub fn floor_m(&self) -> f64 {
        let (c, h) = (if self.crest_m.is_finite() { self.crest_m.max(0.0) } else { 0.0 }, if self.height_m.is_finite() { self.height_m.max(0.0) } else { 0.0 });
        c.min(0.5 * h)
    }
}

/// Height above the waterline a gun solution against a surface contact lays for in a seaway: the top of the
/// crests in front of it ([`TargetBox::floor_m`] of the contact's silhouette, `rcs_m2`, in `sea`). The solver
/// aims at the track estimate (L518), the hull at the sea surface; laid at the waterline itself, a round in
/// Beaufort 4 meets a crest before the hull nearly every time at short range. Unit: `sys-gunnery`.
pub fn wave_lay_m(rcs_m2: f64, sea: SeaState) -> f64 { TargetBox { crest_m: crest_m(sea), ..silhouette(rcs_m2, None) }.floor_m() }

/// Fraction of the significant wave height that the crests in front of a hull hide of its silhouette from a
/// flat-falling round (a shell ends at the wave surface, L557): half, the height of a significant crest above
/// the mean surface. Calibrated with [`SILHOUETTE_FILL`].
pub const CREST_FRACTION: f64 = 0.5;
/// Share of a contact's radar silhouette width that a round can strike: the return counts masts and upperworks
/// that stand well above where the rounds arrive, and a silhouette is narrower at the top than at the
/// waterline. Measured (k2, `gun_range_trace` with `BP_NOGATE`: the 76 mm on the 22 m starter against the
/// Combine patrol boat, gunboat and missile boat at 3, 5 and 7 km in sea states 0, 3 and 4, 27 runs of 240 s,
/// 9,700 rounds, the officers' own predicted chance summed per round fired): 1,005 hits landed where the
/// model predicted 1,087 (0.92); 0.95 in calm water, 0.79 in sea state 3 (Beaufort 4), 1.10 in sea state 4;
/// 0.88, 1.08 and 0.90 at 3, 5 and 7 km. With the full silhouette it had predicted half as many again.
pub const SILHOUETTE_FILL: f64 = 0.55;
/// Height of the crests that hide a hull's foot in a sea state, metres.
pub fn crest_m(sea: SeaState) -> f64 { CREST_FRACTION * sea.significant_height_m() }

/// Angular sigma of an officer-aimed round, radians per axis: the gun's dispersion and the officer laying
/// error (times the accuracy multiplier) in quadrature, plus the sea state's laying error ([`sea_laying_rad`]).
pub fn laying_sigma_rad(dispersion_mrad: f64, accuracy_mult: f64, sea: SeaState) -> f64 {
    let d = if dispersion_mrad.is_finite() { dispersion_mrad.max(0.0) * 1e-3 } else { 0.0 };
    let o = aim_sigma(PLATFORM_SIGMA_B4_RAD, accuracy_mult, 1.0);
    let s = sea_laying_rad(sea);
    libm::sqrt(d * d + o * o + s * s)
}

/// Extra laying error the sea adds to a stabilised mount, radians per axis, by sea state. Measured with
/// `gun_range_trace` against a stationary hull (see the calibration note in the module's tests).
pub fn sea_laying_rad(sea: SeaState) -> f64 { let _ = sea; 0.0 }

/// Steady-state errors of the fire-control fit (`crate::tracks::MotionFit`, long baseline) under a lock at
/// `range_m`: (position variance, velocity variance, covariance) per axis. A fire-control fix is 5 m in range
/// and 0.05 degrees in bearing (L430), fused as the tracker fuses it (an isotropic equivalent), at
/// [`LOCK_FIX_HZ`]; an exponentially weighted line fit with time constant tau then has position variance
/// 1.25 s^2 / (f tau), velocity variance s^2 / (4 f tau^3) and covariance s^2 / (2 f tau^2).
pub fn lock_errors(range_m: f64) -> (f64, f64, f64) {
    let fc = measurement_error(SensorKind::FireControlRadar);
    let sr = fc.range_m.unwrap_or(5.0);
    let cross = range_m.max(0.0) * fc.bearing_deg.to_radians();
    let (a, b) = (sr * sr, cross * cross);
    // The tracker's isotropic equivalent of an anisotropic fix (`tracks::iso_equivalent`).
    let s2 = 0.5 * (a + b) + 0.14 * (a - b).abs();
    let (f, tau) = (LOCK_FIX_HZ, crate::tracks::FIT_LONG_S);
    (1.25 * s2 / (f * tau), s2 / (4.0 * f * tau * tau * tau), s2 / (2.0 * f * tau * tau))
}

/// The silhouette of a contact from what the sensors know of it: its radar cross-section (the projected area
/// above water from this aspect, `Track::rcs_m2`) and its aspect, the angle between its course and the line
/// of fire (`None` when its course is not known). An area-preserving rectangle: a hull of length five beams
/// whose broadside silhouette is seven times as wide as it is high and whose bow-on silhouette is square,
/// which is what the stock Combine and starter hulls measure (`silhouette_probe` in the trial tests: gunboat
/// 26 m, 90 m^2 broadside; frigate 65.5 m, 549 m^2). Between them the area and the width follow the aspect.
/// Its width is [`SILHOUETTE_FILL`] of the rectangle's. A contact with no radar return yet is the 40 m
/// reference hull. Unit: `sys-gunnery`.
pub fn silhouette(rcs_m2: f64, aspect_rad: Option<f64>) -> TargetBox {
    if !(rcs_m2.is_finite() && rcs_m2 > 0.0) {
        let len = crate::doctrine::REFERENCE_HULL_LENGTH_M;
        return TargetBox { width_m: len, height_m: crate::doctrine::REFERENCE_HULL_HEIGHT_M, depth_m: REFERENCE_BEAM_FRACTION * len, crest_m: 0.0 };
    }
    let theta = aspect_rad.filter(|a| a.is_finite()).unwrap_or(core::f64::consts::FRAC_PI_4);
    let (s, c) = (libm::sin(theta).abs(), libm::cos(theta).abs());
    // Width over beam, area over beam squared, depth over beam.
    let w = 5.0 * s + c;
    let area = 3.57 * s + c;
    let depth = 5.0 * c + s;
    let beam = libm::sqrt(rcs_m2 / area);
    TargetBox { width_m: SILHOUETTE_FILL * beam * w, height_m: rcs_m2 / (beam * w), depth_m: beam * depth, crest_m: 0.0 }
}

/// Predicted single-round hit probability of an officer-aimed round at `range_m` with errors `e` against the
/// silhouette `target`. Unit: `sys-gunnery`.
///
/// The solver aims at the track estimate (L518): the target's centre, at the waterline, laid over the crests
/// in a seaway ([`wave_lay_m`]). Across the line of fire the round misses by the angular error times range
/// and the target's predicted cross-range error; it hits when that lands inside the silhouette's width. In the
/// vertical plane through the target the round's height errs by the angular error times range plus the
/// predicted along-range error times the tangent of the fall angle; it hits when it clears the crests before
/// the near face and comes down before the far face: between `-(depth / 2) tan(fall)` and
/// `height - floor + (depth / 2) tan(fall)` about the aim point ([`TargetBox::floor_m`]). The prediction
/// error is the estimate's own error extrapolated over its age plus the time of flight, with the manoeuvre
/// allowance `0.5 a t^2`. The two axes are independent normal errors. 0 beyond the table's last row.
pub fn predicted_pk(table: &[RangeRow], range_m: f64, e: &ShotErrors, target: &TargetBox) -> f64 {
    let Some(last) = table.last() else { return 0.0 };
    if !range_m.is_finite() || range_m > last.range_m + 1e-6 { return 0.0; }
    let range = range_m.max(0.0);
    let (tof, fall) = interpolate(table, range);
    let t = (if e.age_s.is_finite() { e.age_s.max(0.0) } else { 0.0 }) + tof;
    let fin = |v: f64| if v.is_finite() { v.max(0.0) } else { f64::INFINITY };
    let man_c = 0.5 * fin(e.manoeuvre_mps2) * t * t;
    let man_a = 0.5 * fin(e.manoeuvre_along_mps2) * t * t;
    let stat = |p: f64, v: f64, c: f64| (fin(p) + 2.0 * t * (if c.is_finite() { c } else { 0.0 }) + t * t * fin(v)).max(0.0);
    let (stat_c, stat_a) = (stat(e.var_pos, e.var_vel, e.cov), stat(e.var_pos_along, e.var_vel_along, e.cov_along));
    let ang = range * fin(e.angular_rad);
    let tan = libm::tan(fall.clamp(0.0, 1.5));
    // A millimetre floor keeps point blank continuous with a metre out (no error at all is no real shot).
    let sigma_cross = libm::sqrt(ang * ang + stat_c + man_c * man_c).max(1e-3);
    let sigma_vert = libm::sqrt(ang * ang + (stat_a + man_a * man_a) * tan * tan).max(1e-3);
    let (w, h, d) = (fin(target.width_m), fin(target.height_m), fin(target.depth_m));
    if !(w.is_finite() && h.is_finite() && d.is_finite()) { return 0.0; }
    let cross = axis_hit(w, sigma_cross);
    // Laid at the crests' height (`wave_lay_m`): the round hits between the crests and the silhouette's top.
    let floor = target.floor_m();
    let lo = -0.5 * d * tan;
    let hi = h - floor + 0.5 * d * tan;
    let vert = normal_between(lo, hi, sigma_vert);
    (cross * vert).clamp(0.0, 1.0)
}

/// Largest range in `[0, cap_m]` at which [`predicted_pk`] is at least `pk_min`, by the table's rows and a
/// bisection inside the first failing row (the prediction error and the angular error both grow with range,
/// so the probability does not rise with it). 0 when even point-blank fire falls short of `pk_min`.
pub fn pk_range(table: &[RangeRow], e: &ShotErrors, target: &TargetBox, pk_min: f64, cap_m: f64) -> f64 {
    let Some(last) = table.last() else { return 0.0 };
    let cap = if cap_m.is_finite() { cap_m.min(last.range_m).max(0.0) } else { last.range_m };
    let ok = |r: f64| predicted_pk(table, r, e, target) >= pk_min;
    if ok(cap) { return cap; }
    let (mut lo, mut hi) = (0.0, cap);
    for row in table.iter().take_while(|r| r.range_m < cap) {
        if ok(row.range_m) { lo = row.range_m; } else { hi = row.range_m; break; }
    }
    if lo == 0.0 && !ok(0.0) { return 0.0; }
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if ok(mid) { lo = mid; } else { hi = mid; }
        if hi - lo < 1.0 { break; }
    }
    lo
}

/// Probability that a centred normal error with `sigma` lands in `[lo, hi]`.
fn normal_between(lo: f64, hi: f64, sigma: f64) -> f64 {
    if !(hi > lo) { return 0.0; }
    if sigma <= 1e-12 { return if lo <= 0.0 && hi >= 0.0 { 1.0 } else { 0.0 }; }
    let k = 1.0 / (core::f64::consts::SQRT_2 * sigma);
    (0.5 * (libm::erf(hi * k) - libm::erf(lo * k))).clamp(0.0, 1.0)
}

/// Probability that a centred normal error with `sigma` lands inside a window of total width `w`.
fn axis_hit(w: f64, sigma: f64) -> f64 {
    if w <= 0.0 { return 0.0; }
    if sigma <= 1e-12 { return 1.0; }
    libm::erf(w / (2.0 * core::f64::consts::SQRT_2 * sigma)).clamp(0.0, 1.0)
}

/// Time of flight and fall angle at a range by linear interpolation, with a virtual row at 0 m.
fn interpolate(table: &[RangeRow], range: f64) -> (f64, f64) {
    let mut prev = (0.0, 0.0, 0.0);
    for row in table {
        if range <= row.range_m {
            let span = row.range_m - prev.0;
            let f = if span > 1e-9 { ((range - prev.0) / span).clamp(0.0, 1.0) } else { 1.0 };
            return (prev.1 + (row.time_of_flight_s - prev.1) * f, prev.2 + (row.fall_angle_rad - prev.2) * f);
        }
        prev = (row.range_m, row.time_of_flight_s, row.fall_angle_rad);
    }
    (prev.1, prev.2)
}

fn aim_sigma(base_sigma_rad: f64, accuracy_mult: f64, factor: f64) -> f64 {
    let base = if base_sigma_rad.is_finite() { base_sigma_rad.abs() } else { 0.0 };
    let mult = if accuracy_mult.is_finite() { accuracy_mult.clamp(0.0, 10.0) } else { 1.0 };
    let factor = if factor.is_finite() { factor.clamp(0.0, 10.0) } else { 1.0 };
    base * mult * factor
}

fn unit_rot(q: DQuat) -> DQuat {
    let n = q.length_squared();
    if !n.is_finite() || n < 1e-24 { DQuat::IDENTITY } else { q.normalize() }
}

// ---- trajectory in the firing plane ----

/// The firing hull's velocity resolved against the horizontal bearing of the shot.
#[derive(Clone, Copy, Debug, Default)]
struct OwnPlane { along: f64, across: f64, up: f64 }

/// Where a trajectory crosses the target's horizontal range.
#[derive(Clone, Copy, Debug)]
struct Crossing { h: f64, t: f64, vel: DVec2 }

/// Shell velocity in the plane (along the bearing, up) for a barrel elevation; `None` when the barrel cannot
/// point the shell's horizontal path along the bearing (crossing speed above the shell's horizontal speed).
fn plane_velocity(e: f64, mv: f64, own: OwnPlane) -> Option<DVec2> {
    let a = mv * libm::cos(e);
    let a2 = a * a - own.across * own.across;
    if a2 < 0.0 { return None; }
    let vx = libm::sqrt(a2) + own.along;
    if vx <= 1e-6 { return None; }
    Some(DVec2::new(vx, mv * libm::sin(e) + own.up))
}

/// Step a shell from the origin until it crosses horizontal distance `range`; `None` when it falls well
/// below the target height first (short) or flies longer than the solver integrates.
fn trace<S: Fn(DVec3, DVec3, f64, f64) -> (DVec3, DVec3)>(step: &S, v0: DVec2, k: f64, range: f64, dy: f64) -> Option<Crossing> {
    let floor = dy - FLOOR_MARGIN_M - 0.05 * range;
    let mut p = DVec3::ZERO;
    let mut v = DVec3::new(v0.x, v0.y, 0.0);
    let mut t = 0.0;
    for _ in 0..(MAX_FLIGHT_S / DT) as u32 {
        let (np, nv) = step(p, v, k, DT);
        if !np.is_finite() || !nv.is_finite() { return None; }
        if np.x >= range {
            let dx = np.x - p.x;
            let f = if dx > 1e-12 { ((range - p.x) / dx).clamp(0.0, 1.0) } else { 1.0 };
            let vel = v + (nv - v) * f;
            return Some(Crossing { h: p.y + (np.y - p.y) * f, t: t + f * DT, vel: DVec2::new(vel.x, vel.y) });
        }
        if (np.y < floor && nv.y < 0.0) || np.x - p.x <= 1e-9 { return None; }
        p = np;
        v = nv;
        t += DT;
    }
    None
}

/// Result of an elevation search.
#[derive(Clone, Copy, Debug)]
enum Elev {
    Hit { e: f64, c: Crossing },
    /// Out of range: the elevation that reaches highest at the range, and its crossing if it gets there.
    OutOfRange { e: f64, c: Option<Crossing> },
}

type Sample = Option<(f64, Crossing)>;

/// Low-angle elevation for the plane problem: f(e) = height at the range minus the target height rises with
/// elevation up to the maximum-range elevation, and the root on that rising branch is the answer.
/// `guess` warm-starts the search.
#[allow(clippy::too_many_arguments)]
fn search_elevation<S: Fn(DVec3, DVec3, f64, f64) -> (DVec3, DVec3)>(step: &S, range: f64, dy: f64, mv: f64, k: f64, own: OwnPlane, guess: Option<f64>, evals: &mut u32) -> Elev {
    let los = libm::atan2(dy, range);
    let f = |e: f64, evals: &mut u32| -> Sample {
        *evals += 1;
        let v0 = plane_velocity(e, mv, own)?;
        trace(step, v0, k, range, dy).map(|c| (c.h - dy, c))
    };
    // Vacuum flat-fire slope of height at the range against elevation, for step sizes.
    let deriv = |e: f64| { let c = libm::cos(e); (range / (c * c).max(1e-6)).max(1.0) };
    let mut best: (f64, f64, Option<Crossing>) = (los, f64::NEG_INFINITY, None);
    let note = |e: f64, s: &Sample, best: &mut (f64, f64, Option<Crossing>)| { if let Some((fv, c)) = s { if *fv > best.1 { *best = (e, *fv, Some(*c)); } } };

    let start = guess.filter(|g| g.is_finite()).unwrap_or_else(|| vacuum_guess(range, dy, mv, los)).clamp(los.min(ELEV_MAX_RAD), ELEV_MAX_RAD);
    let s0 = f(start, evals);
    note(start, &s0, &mut best);
    // Bracket: lo on the rising branch below the target (f < 0, or short with no magnitude), hi at or above it.
    let (mut lo_e, mut lo_f, mut hi_e, mut hi_f, mut hi_c);
    match s0 {
        Some((fv, c)) if fv.abs() <= HEIGHT_TOL_M => return Elev::Hit { e: start, c },
        Some((fv, c)) if fv > 0.0 => {
            // Above: step down toward the line of sight (f < 0 there under gravity) until below.
            (hi_e, hi_f, hi_c) = (start, fv, c);
            (lo_e, lo_f) = (los, None);
            let mut d = (fv / deriv(start) * 1.5).max(1e-7);
            while *evals < MAX_EVALS {
                let next = hi_e - d;
                if next <= los { break; }
                match f(next, evals) {
                    Some((nf, nc)) if nf.abs() <= HEIGHT_TOL_M => return Elev::Hit { e: next, c: nc },
                    Some((nf, nc)) if nf > 0.0 => { (hi_e, hi_f, hi_c) = (next, nf, nc); d *= 2.0; }
                    Some((nf, _)) => { (lo_e, lo_f) = (next, Some(nf)); break; }
                    None => { (lo_e, lo_f) = (next, None); break; }
                }
            }
        }
        _ => {
            // Below or short: scan upward until above, watching for the peak (out of range). A short start
            // gives no magnitude, so the scan restarts from the line of sight to be sure of the branch.
            let (mut prev_e, mut prev_f, mut cur_e, mut cur_f) = (los, None, start, s0.map(|s| s.0));
            let mut d = match cur_f { Some(fv) => (-fv / deriv(start) * 1.5).max(1e-6), None => { cur_e = los; SCAN_STEP_RAD } };
            let found;
            loop {
                if *evals >= MAX_EVALS || cur_e >= ELEV_MAX_RAD { return Elev::OutOfRange { e: best.0, c: best.2 }; }
                let next = (cur_e + d).min(ELEV_MAX_RAD);
                let s = f(next, evals);
                note(next, &s, &mut best);
                let passed_peak = match (s, cur_f) { (Some((nf, _)), Some(cf)) => nf < cf, (None, Some(_)) => true, _ => false };
                match s {
                    Some((nf, nc)) if nf >= 0.0 => {
                        if nf <= HEIGHT_TOL_M { return Elev::Hit { e: next, c: nc }; }
                        found = Some((next, nf, nc));
                        (lo_e, lo_f) = (cur_e, cur_f);
                        break;
                    }
                    _ if passed_peak => {
                        // The peak lies in [prev_e, next].
                        match refine_peak(&f, prev_e, next, evals) {
                            Some((pe, pf, pc)) if pf >= 0.0 => { found = Some((pe, pf, pc)); (lo_e, lo_f) = (prev_e, prev_f); break; }
                            Some((pe, pf, pc)) => { if pf > best.1 { best = (pe, pf, Some(pc)); } return Elev::OutOfRange { e: best.0, c: best.2 }; }
                            None => return Elev::OutOfRange { e: best.0, c: best.2 },
                        }
                    }
                    _ => {
                        (prev_e, prev_f) = (cur_e, cur_f);
                        (cur_e, cur_f) = (next, s.map(|s| s.0));
                        d *= 2.0;
                    }
                }
            }
            let Some((e, fv, c)) = found else { return Elev::OutOfRange { e: best.0, c: best.2 } };
            (hi_e, hi_f, hi_c) = (e, fv, c);
        }
    }
    // Illinois regula falsi inside [lo, hi]; bisect while lo has no magnitude.
    let mut side = 0i8;
    while *evals < MAX_EVALS && hi_e - lo_e > ELEV_TOL_RAD {
        let mid = match lo_f {
            Some(lf) => { let m = hi_e - hi_f * (hi_e - lo_e) / (hi_f - lf); if m > lo_e && m < hi_e { m } else { 0.5 * (lo_e + hi_e) } }
            None => 0.5 * (lo_e + hi_e),
        };
        match f(mid, evals) {
            Some((mf, mc)) if mf.abs() <= HEIGHT_TOL_M => return Elev::Hit { e: mid, c: mc },
            Some((mf, mc)) if mf > 0.0 => {
                (hi_e, hi_f, hi_c) = (mid, mf, mc);
                if side == 1 { if let Some(lf) = lo_f.as_mut() { *lf *= 0.5; } }
                side = 1;
            }
            Some((mf, _)) => {
                (lo_e, lo_f) = (mid, Some(mf));
                if side == -1 { hi_f *= 0.5; }
                side = -1;
            }
            None => { (lo_e, lo_f) = (mid, None); side = 0; }
        }
    }
    Elev::Hit { e: hi_e, c: hi_c }
}

/// Golden-section search for the elevation that reaches highest at the range, inside [a, b]; stops early
/// once a sample reaches the target height.
fn refine_peak<F: Fn(f64, &mut u32) -> Sample>(f: &F, a: f64, b: f64, evals: &mut u32) -> Option<(f64, f64, Crossing)> {
    const R: f64 = 0.618_033_988_749_894_8;
    let (mut a, mut b) = (a, b);
    let mut best: Option<(f64, f64, Crossing)> = None;
    let keep = |e: f64, v: Sample, best: &mut Option<(f64, f64, Crossing)>| -> f64 {
        match v { Some((fv, c)) => { if best.is_none_or(|b| fv > b.1) { *best = Some((e, fv, c)); } fv } None => f64::NEG_INFINITY }
    };
    let mut x1 = b - R * (b - a);
    let mut x2 = a + R * (b - a);
    let mut f1 = keep(x1, f(x1, evals), &mut best);
    let mut f2 = keep(x2, f(x2, evals), &mut best);
    for _ in 0..24 {
        if best.is_some_and(|b| b.1 >= 0.0) || b - a < 1e-7 { break; }
        if f1 < f2 { a = x1; x1 = x2; f1 = f2; x2 = a + R * (b - a); f2 = keep(x2, f(x2, evals), &mut best); }
        else { b = x2; x2 = x1; f2 = f1; x1 = b - R * (b - a); f1 = keep(x1, f(x1, evals), &mut best); }
    }
    best
}

/// Vacuum low-angle elevation as a starting point; falls back to a little above the line of sight.
fn vacuum_guess(range: f64, dy: f64, mv: f64, los: f64) -> f64 {
    let g = content::part::weapons::GRAVITY;
    let v2 = mv * mv;
    let disc = v2 * v2 - g * (g * range * range + 2.0 * dy * v2);
    if disc >= 0.0 && range > 0.0 { libm::atan((v2 - libm::sqrt(disc)) / (g * range)) } else { (los + 0.35).min(ELEV_MAX_RAD) }
}

fn elevation_for_with<S: Fn(DVec3, DVec3, f64, f64) -> (DVec3, DVec3)>(range_m: f64, dy_m: f64, muzzle_mps: f64, drag_k: f64, step: &S) -> Option<(f64, f64)> {
    if !range_m.is_finite() || !dy_m.is_finite() || !muzzle_mps.is_finite() || muzzle_mps <= 0.0 || range_m < 0.0 { return None; }
    let k = sane_k(drag_k);
    if near_vertical(range_m, dy_m) { return direct_shot(range_m, dy_m, muzzle_mps, k, step); }
    let mut evals = 0;
    match search_elevation(step, range_m, dy_m, muzzle_mps, k, OwnPlane::default(), None, &mut evals) {
        Elev::Hit { e, c } => Some((e, c.t)),
        Elev::OutOfRange { .. } => None,
    }
}

/// Straight up or down: elevation along the line of sight and the climb or fall time; `None` when a shell
/// fired straight up cannot reach the height.
fn direct_shot<S: Fn(DVec3, DVec3, f64, f64) -> (DVec3, DVec3)>(range: f64, dy: f64, mv: f64, k: f64, step: &S) -> Option<(f64, f64)> {
    let e = libm::atan2(dy, range.max(0.0));
    if libm::sqrt(range * range + dy * dy) < 1e-9 { return Some((e, 0.0)); }
    let dir = if dy >= 0.0 { 1.0 } else { -1.0 };
    let mut p = DVec3::ZERO;
    let mut v = DVec3::new(0.0, dir * mv, 0.0);
    let mut t = 0.0;
    for _ in 0..(MAX_FLIGHT_S / DT) as u32 {
        let (np, nv) = step(p, v, k, DT);
        if (np.y - dy) * dir >= 0.0 {
            let span = np.y - p.y;
            let f = if span.abs() > 1e-12 { ((dy - p.y) / span).clamp(0.0, 1.0) } else { 1.0 };
            return Some((e, t + f * DT));
        }
        if !nv.is_finite() || (dir > 0.0 && nv.y <= 0.0) { return None; }
        p = np;
        v = nv;
        t += DT;
    }
    None
}

/// Too short or too steep for the plane search: shoot along the line of sight.
fn near_vertical(range: f64, dy: f64) -> bool { range < MIN_RANGE_M || libm::atan2(dy.abs(), range) >= ELEV_MAX_RAD }

fn sane_k(k: f64) -> f64 { if k.is_finite() { k.max(0.0) } else { 0.0 } }

fn range_table_with<S: Fn(DVec3, DVec3, f64, f64) -> (DVec3, DVec3)>(muzzle_mps: f64, band: &BandDef, step: &S) -> Vec<RangeRow> {
    const ROW_M: f64 = 250.0;
    const MAX_ROWS: usize = 400;
    let mut rows = Vec::new();
    if !muzzle_mps.is_finite() || muzzle_mps <= 0.0 || !band.cap_range_m.is_finite() || band.cap_range_m < ROW_M { return rows; }
    let k = sane_k(band.drag_k);
    let mut guess = None;
    for i in 1..=MAX_ROWS {
        let range = ROW_M * i as f64;
        if range > band.cap_range_m + 1e-6 { break; }
        let mut evals = 0;
        match search_elevation(step, range, 0.0, muzzle_mps, k, OwnPlane::default(), guess, &mut evals) {
            Elev::Hit { e, c } => {
                guess = Some(e);
                rows.push(RangeRow { range_m: range, elevation_rad: e, time_of_flight_s: c.t, impact_mps: c.vel.length(), fall_angle_rad: libm::atan2(-c.vel.y, c.vel.x) });
            }
            Elev::OutOfRange { .. } => break,
        }
    }
    rows
}

// ---- the fixed-point solver ----

/// Limits in radians with a full-circle flag.
#[derive(Clone, Copy, Debug)]
struct Limits { t_min: f64, t_max: f64, full: bool, e_min: f64, e_max: f64 }

fn limits_rad(l: &MountLimits) -> Limits {
    let fin = |v: f64, d: f64| if v.is_finite() { v } else { d };
    let (mut t0, mut t1) = (fin(l.train_min_deg, -180.0), fin(l.train_max_deg, 180.0));
    if t0 > t1 { core::mem::swap(&mut t0, &mut t1); }
    let (mut e0, mut e1) = (fin(l.elevation_min_deg, -90.0), fin(l.elevation_max_deg, 90.0));
    if e0 > e1 { core::mem::swap(&mut e0, &mut e1); }
    Limits { t_min: t0.to_radians(), t_max: t1.to_radians(), full: t1 - t0 >= 360.0 - 1e-9, e_min: e0.to_radians(), e_max: e1.to_radians() }
}

fn wrap_pi(a: f64) -> f64 {
    let mut r = a % TAU;
    if r > PI { r -= TAU; } else if r <= -PI { r += TAU; }
    r
}

/// Training expressed in the limit's representation, and the limit it was clamped to, if any (radians).
fn clamp_train(train: f64, l: &Limits) -> (f64, Option<f64>) {
    if l.full { return (wrap_pi(train), None); }
    let t = l.t_min + (train - l.t_min).rem_euclid(TAU);
    if t <= l.t_max + 1e-12 { return (t.min(l.t_max), None); }
    // In the dead arc: go to the nearer limit.
    if t - l.t_max <= l.t_min + TAU - t { (l.t_max, Some(l.t_max)) } else { (l.t_min, Some(l.t_min)) }
}

fn slew_time(cur: (f64, f64), target: (f64, f64), rates: (f64, f64), l: &Limits) -> f64 {
    let dt = if l.full { wrap_pi(target.0 - cur.0).abs() } else { (target.0 - clamp_train(cur.0, l).0).abs() };
    let de = (target.1 - cur.1.clamp(l.e_min, l.e_max)).abs();
    let mut t: f64 = 0.0;
    if rates.0.is_finite() && rates.0 > 1e-9 { t = t.max(dt / rates.0); }
    if rates.1.is_finite() && rates.1 > 1e-9 { t = t.max(de / rates.1); }
    t.min(MAX_SLEW_S)
}

fn input_finite(i: &GunSolveIn) -> bool {
    i.muzzle.is_finite() && i.own_vel.is_finite() && i.pose.pos.is_finite() && i.pose.rot.is_finite() && i.zero_rad.is_finite()
        && i.target_pos.is_finite() && i.target_vel.is_finite() && i.muzzle_mps.is_finite() && i.muzzle_mps > 0.0
        && i.train_rad.is_finite() && i.elevation_rad.is_finite()
}

/// One elevation solve for an aim point.
struct Shot { dir: DVec3, tof: f64, reach: bool, impact_h_speed: f64, warm: Warm }

/// The previous iterate's plane solution, to warm-start the next: (elevation, line of sight, range).
type Warm = Option<(f64, f64, f64)>;

fn shoot<S: Fn(DVec3, DVec3, f64, f64) -> (DVec3, DVec3)>(step: &S, origin: DVec3, aim: DVec3, inp: &GunSolveIn, k: f64, fallback_bearing: DVec2, warm: Warm) -> Shot {
    let d = aim - origin;
    let horiz = DVec2::new(d.x, d.z);
    let range = horiz.length();
    let h_hat = if range > 1e-9 { horiz / range } else { fallback_bearing };
    let own = own_plane(inp.own_vel, h_hat);
    let mv = inp.muzzle_mps;
    let (e, tof, reach, hs) = if near_vertical(range, d.y) {
        match direct_shot(range, d.y, mv, k, step) { Some((e, t)) => (e, t, true, 0.0), None => (libm::atan2(d.y, range), d.length() / mv, false, 0.0) }
    } else {
        // Keep the previous superelevation, scaled with range.
        let los = libm::atan2(d.y, range);
        let guess = warm.filter(|w| w.2 > 1.0).map(|(e0, l0, r0)| los + (e0 - l0) * (range / r0));
        let mut evals = 0;
        match search_elevation(step, range, d.y, mv, k, own, guess, &mut evals) {
            Elev::Hit { e, c } => (e, c.t, true, c.vel.x),
            Elev::OutOfRange { e, c } => (e, c.map_or(d.length() / mv, |c| c.t), false, c.map_or(0.0, |c| c.vel.x)),
        }
    };
    let dir = barrel_dir(e, mv, own, h_hat, inp.own_vel);
    Shot { dir, tof, reach, impact_h_speed: hs, warm: if near_vertical(range, d.y) { None } else { Some((e, libm::atan2(d.y, range), range)) } }
}

/// Own velocity split along and across a horizontal bearing (unit), and up.
fn own_plane(own_vel: DVec3, h_hat: DVec2) -> OwnPlane {
    let own_h = DVec2::new(own_vel.x, own_vel.z);
    let along = own_h.dot(h_hat);
    OwnPlane { along, across: (own_h - h_hat * along).length(), up: own_vel.y }
}

/// Barrel direction for a plane elevation: the shell's world velocity (horizontal part along the bearing)
/// minus own velocity.
fn barrel_dir(e: f64, mv: f64, own: OwnPlane, h_hat: DVec2, own_vel: DVec3) -> DVec3 {
    let shell = match plane_velocity(e, mv, own) {
        Some(v) => DVec3::new(h_hat.x * v.x, v.y, h_hat.y * v.x),
        None => DVec3::new(h_hat.x * mv * libm::cos(e), mv * libm::sin(e), h_hat.y * mv * libm::cos(e)) + own_vel,
    };
    let rel = shell - own_vel;
    if rel.length_squared() > 1e-18 { rel.normalize() } else { DVec3::new(h_hat.x, 0.0, h_hat.y) }
}

/// Horizontal bearing of the barrel as it lies now (+X when it points straight up or down).
fn current_bearing(inp: &GunSolveIn) -> DVec2 {
    let cur_dir = mount_dir_to_world(&inp.pose, inp.zero_rad, inp.train_rad, inp.elevation_rad);
    let h = DVec2::new(cur_dir.x, cur_dir.z);
    if h.length_squared() > 1e-18 { h.normalize() } else { DVec2::X }
}

/// Lay a mount on a target too far for a firing solution, without solving: train onto the target's lead
/// bearing (straight-line time of flight) and elevate to the vacuum low-angle elevation for
/// `min(range, lay_range_m)`, so the mount stands ready when the target closes. Never in range or converged,
/// zero iterations. Invalid input gets [`solve`]'s hold-the-current-angles answer. Unit: `sys-gunnery`.
///
/// The officer's `BallisticSolver` uses it beyond [`LAY_BEYOND_CAP`] times the band's cap range: effective
/// range never exceeds the cap (DESIGN.md L572), so no order to fire can come there, and a full solution at
/// two or three times the cap integrates trajectories a minute long for every holding mount at 30 Hz.
pub fn lay(input: &GunSolveIn, lay_range_m: f64) -> GunSolution {
    if !input_finite(input) { return solve(input); }
    let lim = limits_rad(&input.limits);
    let mv = input.muzzle_mps;
    let tof = ((input.target_pos - input.muzzle).length() / mv).min(MAX_FLIGHT_S);
    let aim = input.target_pos + input.target_vel * tof;
    let d = aim - input.muzzle;
    let horiz = DVec2::new(d.x, d.z);
    let range = horiz.length();
    let h_hat = if range > 1e-9 { horiz / range } else { current_bearing(input) };
    let los = libm::atan2(d.y, range);
    let reach = if lay_range_m.is_finite() && lay_range_m > 0.0 { range.min(lay_range_m) } else { range };
    let e = vacuum_guess(reach, d.y, mv, los).clamp(-ELEV_MAX_RAD, ELEV_MAX_RAD);
    let dir = barrel_dir(e, mv, own_plane(input.own_vel, h_hat), h_hat, input.own_vel);
    let (train_raw, elev_raw) = world_dir_to_mount(&input.pose, input.zero_rad, dir);
    let (train, train_hit) = clamp_train(train_raw, &lim);
    let elev = elev_raw.clamp(lim.e_min, lim.e_max);
    let elev_hit = if elev_raw < lim.e_min { Some(lim.e_min) } else if elev_raw > lim.e_max { Some(lim.e_max) } else { None };
    GunSolution {
        train_rad: train, elevation_rad: elev, aim_point: aim, time_of_flight_s: tof, iterations: 0, converged: false, residual_m: 0.0,
        train_out_of_limits: train_hit.map(|l| (train_raw.to_degrees(), l.to_degrees())),
        elevation_out_of_limits: elev_hit.map(|l| (elev_raw.to_degrees(), l.to_degrees())),
        in_range: false,
    }
}

/// Targets farther than this multiple of the band's cap range get a [`lay`], not a solution.
pub const LAY_BEYOND_CAP: f64 = 1.25;

fn solve_with<S: Fn(DVec3, DVec3, f64, f64) -> (DVec3, DVec3)>(inp: &GunSolveIn, step: &S) -> GunSolution {
    let lim = limits_rad(&inp.limits);
    if !input_finite(inp) {
        let (t, e) = if inp.train_rad.is_finite() && inp.elevation_rad.is_finite() { (inp.train_rad, inp.elevation_rad) } else { (0.0, 0.0) };
        return GunSolution { train_rad: t, elevation_rad: e, aim_point: if inp.target_pos.is_finite() { inp.target_pos } else { DVec3::ZERO }, ..Default::default() };
    }
    let cap = inp.iteration_cap.max(1);
    let conv = if inp.converge_m.is_finite() && inp.converge_m > 0.0 { inp.converge_m } else { DEFAULT_CONVERGE_M };
    let k = sane_k(inp.drag_k);
    let mv = inp.muzzle_mps;
    let rates = (inp.train_rate, inp.elevation_rate);
    let cur = (inp.train_rad, inp.elevation_rad);
    let fallback = current_bearing(inp);
    let tau_max = MAX_FLIGHT_S + MAX_SLEW_S;

    // tau = slew time + time of flight: the time from now to impact that places the aim point.
    let mut slew = 0.0;
    let mut tau = ((inp.target_pos - inp.muzzle).length() / mv).min(tau_max);
    let mut prev: Option<(f64, f64)> = None;
    let mut warm: Warm = None;
    let mut sol = GunSolution::default();
    for it in 1..=cap {
        let origin = inp.muzzle + inp.own_vel * slew;
        let aim = inp.target_pos + inp.target_vel * tau;
        let shot = shoot(step, origin, aim, inp, k, fallback, warm);
        warm = shot.warm;
        let (train_raw, elev_raw) = world_dir_to_mount(&inp.pose, inp.zero_rad, shot.dir);
        let (train, train_hit) = clamp_train(train_raw, &lim);
        let elev = elev_raw.clamp(lim.e_min, lim.e_max);
        let elev_hit = if elev_raw < lim.e_min { Some(lim.e_min) } else if elev_raw > lim.e_max { Some(lim.e_max) } else { None };
        let new_slew = slew_time(cur, (train, elev), rates, &lim);
        let tau_out = (new_slew + shot.tof).min(tau_max);
        sol = GunSolution {
            train_rad: train,
            elevation_rad: elev,
            aim_point: aim,
            time_of_flight_s: shot.tof,
            iterations: it,
            converged: false,
            residual_m: 0.0,
            train_out_of_limits: train_hit.map(|l| (train_raw.to_degrees(), l.to_degrees())),
            elevation_out_of_limits: elev_hit.map(|l| (elev_raw.to_degrees(), l.to_degrees())),
            in_range: shot.reach,
        };
        if !shot.reach { return sol; }
        // Next tau: substitution, accelerated by a secant estimate of d(tau_out)/d(tau).
        let slope = match prev {
            Some((ti, to)) if (tau - ti).abs() > 1e-9 => (tau_out - to) / (tau - ti),
            _ => {
                // First iterate: the target's range rate over the shell's horizontal speed at impact.
                let h = DVec2::new(aim.x - origin.x, aim.z - origin.z);
                if h.length_squared() > 1e-12 && shot.impact_h_speed > 1.0 { DVec2::new(inp.target_vel.x, inp.target_vel.z).dot(h.normalize()) / shot.impact_h_speed } else { 0.0 }
            }
        };
        let slope = if slope.is_finite() { slope.clamp(-0.9, 0.9) } else { 0.0 };
        let next_tau = (tau + (tau_out - tau) / (1.0 - slope)).clamp(0.0, tau_max);
        // The impact point moves with the target, and the firing geometry with the muzzle at the new slew.
        sol.residual_m = (inp.target_vel * (next_tau - tau)).length() + (inp.own_vel * (new_slew - slew)).length();
        if sol.residual_m < conv { sol.converged = true; return sol; }
        prev = Some((tau, tau_out));
        tau = next_tau;
        slew = new_slew;
    }
    sol
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::part::weapons::{CalibreBand, GRAVITY};

    /// Semi-implicit Euler with gravity and quadratic drag, as `sim_core::projectiles::ballistic_step` is
    /// documented (L557); stands in while that unit is unlanded.
    fn ref_step(pos: DVec3, vel: DVec3, k: f64, dt: f64) -> (DVec3, DVec3) {
        let a = DVec3::new(0.0, -GRAVITY, 0.0) - vel * (k * vel.length());
        let v = vel + a * dt;
        (pos + v * dt, v)
    }

    /// Fly a shell with the reference step and return its closest approach to `point`.
    fn miss(origin: DVec3, vel: DVec3, k: f64, point: DVec3) -> f64 {
        let (mut p, mut v) = (origin, vel);
        let mut best = (p - point).length();
        for _ in 0..(120.0 / DT) as u32 {
            let (np, nv) = ref_step(p, v, k, DT);
            let seg = np - p;
            let t = if seg.length_squared() > 0.0 { ((point - p).dot(seg) / seg.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
            best = best.min((p + seg * t - point).length());
            if np.y < point.y - 500.0 && nv.y < 0.0 { break; }
            p = np;
            v = nv;
        }
        best
    }

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 { self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (self.0 >> 11) as f64 / (1u64 << 53) as f64 }
        fn range(&mut self, a: f64, b: f64) -> f64 { a + (b - a) * self.next() }
    }

    fn open_limits() -> MountLimits { MountLimits { train_min_deg: -180.0, train_max_deg: 180.0, elevation_min_deg: -15.0, elevation_max_deg: 85.0 } }

    fn base_input() -> GunSolveIn {
        GunSolveIn {
            muzzle: DVec3::new(0.0, 6.0, 0.0), own_vel: DVec3::ZERO, pose: Pose::default(), zero_rad: 0.0, limits: open_limits(),
            target_pos: DVec3::new(6_000.0, 2.0, 0.0), target_vel: DVec3::ZERO, muzzle_mps: 925.0, drag_k: 6e-5,
            train_rad: 0.0, elevation_rad: 0.0, train_rate: 60f64.to_radians(), elevation_rate: 35f64.to_radians(), iteration_cap: 6, converge_m: 0.25,
        }
    }

    #[test]
    fn vacuum_elevation_matches_closed_form() {
        let (mv, r) = (800.0, 10_000.0);
        let (e, t) = elevation_for_with(r, 0.0, mv, 0.0, &ref_step).expect("in range");
        let exact = 0.5 * libm::asin(GRAVITY * r / (mv * mv));
        // Euler at 60 Hz differs from the continuous solution by well under a milliradian.
        assert!((e - exact).abs() < 1e-3, "e {e} exact {exact}");
        assert!((t - r / (mv * libm::cos(e))).abs() < 0.05, "t {t}");
        let v = DVec3::new(mv * libm::cos(e), mv * libm::sin(e), 0.0);
        assert!(miss(DVec3::ZERO, v, 0.0, DVec3::new(r, 0.0, 0.0)) < 0.05);
    }

    #[test]
    fn drag_and_heights_hit_and_max_range_is_none() {
        for &(r, dy, k) in &[(3_000.0, -5.0, 1.2e-4), (8_000.0, 0.0, 6e-5), (2_000.0, 1_500.0, 6e-5), (300.0, 3_000.0, 6e-5), (12_000.0, -10.0, 3e-5), (5.0, -3.0, 0.0)] {
            let (e, _t) = elevation_for_with(r, dy, 925.0, k, &ref_step).unwrap_or_else(|| panic!("in range {r} {dy}"));
            let v = DVec3::new(925.0 * libm::cos(e), 925.0 * libm::sin(e), 0.0);
            let m = miss(DVec3::ZERO, v, k, DVec3::new(r, dy, 0.0)); assert!(m < 0.05, "r {r} dy {dy} miss {m} e {e}");
            // Low-angle branch.
            assert!(e < libm::atan2(dy, r) + 0.8, "r {r} dy {dy} e {e}");
        }
        assert!(elevation_for_with(200_000.0, 0.0, 925.0, 6e-5, &ref_step).is_none());
        assert!(elevation_for_with(f64::NAN, 0.0, 925.0, 6e-5, &ref_step).is_none());
        assert!(elevation_for_with(1_000.0, 0.0, 0.0, 6e-5, &ref_step).is_none());
        let (e, t) = elevation_for_with(0.0, 1_000.0, 925.0, 0.0, &ref_step).expect("overhead");
        assert!((e - FRAC_PI_2).abs() < 1e-9 && t > 1.0 && t < 1.2);
        assert!(elevation_for_with(0.0, 100_000.0, 925.0, 0.0, &ref_step).is_none());
    }

    #[test]
    fn mount_frame_round_trip_and_sense() {
        let pose = Pose::from_heading(DVec3::new(10.0, 0.0, 5.0), 0.7);
        for &(zero, tr, el) in &[(0.0, 0.3, 0.1), (PI, -2.0, 0.5), (1.0, 3.0, -0.2), (-0.5, 0.0, 1.2)] {
            let d = mount_dir_to_world(&pose, zero, tr, el);
            let (t2, e2) = world_dir_to_mount(&pose, zero, d);
            assert!(wrap_pi(t2 - tr).abs() < 1e-9 && (e2 - el).abs() < 1e-9, "{zero} {tr} {el} -> {t2} {e2}");
        }
        // Positive training swings from the bow toward port (-Z local).
        let d = mount_dir_to_world(&Pose::default(), 0.0, FRAC_PI_2, 0.0);
        assert!((d - DVec3::new(0.0, 0.0, -1.0)).length() < 1e-12);
        assert_eq!(world_dir_to_mount(&Pose::default(), 0.0, DVec3::ZERO), (0.0, 0.0));
    }

    #[test]
    fn stationary_target_converges_first_iteration_and_hits() {
        let inp = base_input();
        let s = solve_with(&inp, &ref_step);
        assert!(s.in_range && s.converged && s.iterations == 1, "{s:?}");
        let dir = mount_dir_to_world(&inp.pose, 0.0, s.train_rad, s.elevation_rad);
        assert!(miss(inp.muzzle, shell_velocity(dir, inp.muzzle_mps, inp.own_vel), inp.drag_k, inp.target_pos) < 0.1);
    }

    /// A lay trains on the lead bearing at the cap-range elevation, never claims a solution, and costs no
    /// trajectory; out at 20 km it lies close to the solved bearing.
    #[test]
    fn lay_trains_on_the_lead_bearing_without_solving() {
        let mut inp = base_input();
        inp.target_pos = DVec3::new(14_000.0, 2.0, 14_000.0);
        inp.target_vel = DVec3::new(0.0, 0.0, -10.0);
        let l = lay(&inp, 8_000.0);
        assert!(!l.in_range && !l.converged && l.iterations == 0, "{l:?}");
        assert!(l.train_rad.is_finite() && l.elevation_rad.is_finite() && l.aim_point.is_finite());
        let s = solve(&inp);
        assert!((wrap_pi(l.train_rad - s.train_rad)).abs() < 0.01, "lay {} vs solve {}", l.train_rad, s.train_rad);
        let e8 = elevation_for(8_000.0, -4.0, inp.muzzle_mps, 0.0).expect("vacuum reach").0;
        assert!((l.elevation_rad - e8).abs() < 1e-3, "{} vs {e8}", l.elevation_rad);
        let mut bad = base_input();
        bad.target_pos = DVec3::new(f64::NAN, 0.0, 0.0);
        let b = lay(&bad, 8_000.0);
        assert!(!b.in_range && b.train_rad == 0.0 && b.elevation_rad == 0.0);
    }

    #[test]
    fn moving_shooter_and_target_hits_the_lead_point() {
        let mut inp = base_input();
        inp.own_vel = DVec3::new(3.0, 0.0, -14.0);
        inp.pose = Pose::from_heading(DVec3::ZERO, 2.9);
        inp.target_pos = DVec3::new(4_000.0, 2.0, 2_500.0);
        inp.target_vel = DVec3::new(-10.0, 0.0, 12.0);
        inp.train_rad = 1.0;
        let s = solve_with(&inp, &ref_step);
        assert!(s.in_range && s.converged, "{s:?}");
        let slew = slew_time((inp.train_rad, inp.elevation_rad), (s.train_rad, s.elevation_rad), (inp.train_rate, inp.elevation_rate), &limits_rad(&inp.limits));
        assert!(slew > 0.1);
        let dir = mount_dir_to_world(&inp.pose, 0.0, s.train_rad, s.elevation_rad);
        let origin = inp.muzzle + inp.own_vel * slew;
        let target_then = inp.target_pos + inp.target_vel * (slew + s.time_of_flight_s);
        assert!((target_then - s.aim_point).length() < 0.5, "{target_then} {}", s.aim_point);
        assert!(miss(origin, shell_velocity(dir, inp.muzzle_mps, inp.own_vel), inp.drag_k, s.aim_point) < 0.3);
    }

    #[test]
    fn limits_clamp_and_report() {
        let mut inp = base_input();
        inp.limits = MountLimits { train_min_deg: -120.0, train_max_deg: 120.0, elevation_min_deg: -10.0, elevation_max_deg: 2.0 };
        inp.target_pos = DVec3::new(-6_000.0, 2.0, 100.0); // astern
        let s = solve_with(&inp, &ref_step);
        let (needed, limit) = s.train_out_of_limits.expect("astern is outside the arc");
        assert!(needed.abs() > 170.0 && (limit.abs() - 120.0).abs() < 1e-9 && s.train_rad.abs() <= 120f64.to_radians() + 1e-12);
        let (ne, le) = s.elevation_out_of_limits.expect("6 km needs more than 2 degrees");
        assert!(ne > 2.0 && (le - 2.0).abs() < 1e-9 && s.elevation_rad <= 2f64.to_radians() + 1e-12);
        // An aft arc 30..330 keeps training inside it.
        inp.limits = MountLimits { train_min_deg: 30.0, train_max_deg: 330.0, elevation_min_deg: -10.0, elevation_max_deg: 85.0 };
        let s = solve_with(&inp, &ref_step);
        assert!(s.train_rad >= 30f64.to_radians() && s.train_rad <= 330f64.to_radians() && s.train_out_of_limits.is_none());
    }

    #[test]
    fn bad_input_does_not_panic() {
        let mut inp = base_input();
        inp.target_pos = DVec3::new(f64::NAN, 0.0, 0.0);
        let s = solve_with(&inp, &ref_step);
        assert!(!s.in_range && s.iterations == 0);
        let mut inp = base_input();
        inp.target_pos = DVec3::new(500_000.0, 0.0, 0.0);
        let s = solve_with(&inp, &ref_step);
        assert!(!s.in_range && !s.converged);
        let mut inp = base_input();
        inp.target_pos = inp.muzzle;
        inp.iteration_cap = 0;
        inp.drag_k = f64::INFINITY;
        let s = solve_with(&inp, &ref_step);
        assert!(s.elevation_rad.is_finite() && s.train_rad.is_finite());
        let mut inp = base_input();
        inp.target_vel = DVec3::new(1e7, 0.0, 0.0);
        let s = solve_with(&inp, &ref_step);
        assert!(s.train_rad.is_finite());
    }

    #[test]
    fn near_overhead_targets_are_finite() {
        for &dy in &[1_000.0, 2_000.0, 3_000.0, -40.0] {
            for i in 0..=40 {
                let r = 0.5 + i as f64 * 1.25;
                if let Some((e, t)) = elevation_for_with(r, dy, 925.0, 6e-5, &ref_step) { assert!(e.is_finite() && t.is_finite() && t >= 0.0); }
                let mut inp = base_input();
                inp.target_pos = inp.muzzle + DVec3::new(r, dy, 0.3 * r);
                inp.target_vel = DVec3::new(150.0, 0.0, 30.0);
                let s = solve_with(&inp, &ref_step);
                assert!(s.train_rad.is_finite() && s.elevation_rad.is_finite() && s.time_of_flight_s.is_finite(), "r {r} dy {dy} {s:?}");
                if dy > 0.0 { assert!(s.in_range, "r {r} dy {dy} {s:?}"); }
            }
        }
    }

    /// Gate (1), L526: over a seeded battery of surface and air geometries at least 99.9% converge within 6
    /// iterations and the mean aim-point error against a 50-iteration solution is 0.25 m or less.
    #[test]
    fn gate_convergence_battery() {
        let mut rng = Lcg(0x5eed);
        let (mut converged, mut total, mut err_sum, mut iters) = (0u32, 0u32, 0.0, 0u32);
        for i in 0..600 {
            let air = i % 3 == 0;
            let mut inp = base_input();
            inp.drag_k = [0.0, 3e-5, 6e-5, 1.2e-4][i % 4];
            inp.muzzle_mps = rng.range(800.0, 1_100.0);
            let brg = rng.range(-PI, PI);
            let range = if air { rng.range(800.0, 6_000.0) } else { rng.range(1_000.0, 11_000.0) };
            let alt = if air { rng.range(5.0, 3_000.0) } else { rng.range(0.0, 4.0) };
            inp.target_pos = DVec3::new(range * libm::cos(brg), alt, range * libm::sin(brg));
            let speed = if air { rng.range(100.0, 280.0) } else { rng.range(0.0, 18.0) };
            let crs = rng.range(-PI, PI);
            inp.target_vel = DVec3::new(speed * libm::cos(crs), if air { rng.range(-20.0, 20.0) } else { 0.0 }, speed * libm::sin(crs));
            inp.own_vel = DVec3::new(rng.range(-15.0, 15.0), 0.0, rng.range(-15.0, 15.0));
            inp.pose = Pose::from_heading(DVec3::ZERO, rng.range(0.0, TAU));
            inp.train_rad = rng.range(-PI, PI);
            let reference = solve_with(&GunSolveIn { iteration_cap: 50, converge_m: 1e-6, ..inp }, &ref_step);
            let s = solve_with(&inp, &ref_step);
            if !reference.in_range { continue; }
            total += 1;
            iters += s.iterations as u32;
            if s.converged && s.iterations <= 6 { converged += 1; }
            err_sum += (s.aim_point - reference.aim_point).length();
        }
        assert!(total > 400, "total {total}");
        let rate = converged as f64 / total as f64;
        let mean = err_sum / total as f64;
        assert!(rate >= 0.999, "convergence {rate}");
        assert!(mean <= 0.25, "mean aim-point error {mean}");
        assert!((iters as f64 / total as f64) < 4.0, "mean iterations {}", iters as f64 / total as f64);
    }

    #[test]
    fn range_table_rows_and_pk_shape() {
        let mut band = BandDef::spec_default(CalibreBand::Medium);
        band.drag_k = 6e-5;
        let table = range_table_with(925.0, &band, &ref_step);
        assert_eq!(table.len(), 32);
        assert_eq!(table[0].range_m, 250.0);
        assert_eq!(table.last().map(|r| r.range_m), Some(8_000.0));
        for w in table.windows(2) {
            assert!(w[1].elevation_rad > w[0].elevation_rad && w[1].time_of_flight_s > w[0].time_of_flight_s && w[1].impact_mps < w[0].impact_mps && w[1].fall_angle_rad > w[0].fall_angle_rad);
        }
        assert!(range_table_with(0.0, &band, &ref_step).is_empty());
        let mut last = single_round_pk(&table, 0.0, 1.2, 40.0, 6.0);
        let mut r = 0.0;
        while r <= 8_000.0 {
            let p = single_round_pk(&table, r, 1.2, 40.0, 6.0);
            // Close in a falling round gains a little deck to land on faster than it spreads (a fraction of a
            // percent); beyond a kilometre the chance never rises with range.
            assert!((p <= last + 1e-12 || r <= 1_000.0) && (0.0..=1.0).contains(&p), "r {r} p {p}");
            assert!((last - p).abs() < 0.05, "jump at {r}");
            last = p;
            r += 25.0;
        }
        // Point blank, a lock and a 40 m hull in Beaufort 4: laid on the crests' height, every round that does
        // not dip into them hits, about half of them.
        let near = single_round_pk(&table, 50.0, 1.2, 40.0, 6.0);
        assert!(near > 0.45 && near < 0.55, "{near}");
        assert_eq!(single_round_pk(&table, 8_100.0, 1.2, 40.0, 6.0), 0.0);
        assert_eq!(single_round_pk(&[], 100.0, 1.2, 40.0, 6.0), 0.0);
        assert_eq!(single_round_pk(&table, f64::NAN, 1.2, 40.0, 6.0), 0.0);
        // The 76 mm reference gun keeps 10% out to several kilometres (its doctrine cap is 8 km, L489).
        assert!(single_round_pk(&table, 5_000.0, 1.2, 40.0, 6.0) > 0.10);
    }

    #[test]
    fn aim_sigma_scales() {
        assert_eq!(aim_sigma(1e-3, 1.0, 1.0), 1e-3);
        assert!((aim_sigma(1e-3, 1.5, 3.0) - 4.5e-3).abs() < 1e-15);
        assert_eq!(aim_sigma(f64::NAN, 1.0, 1.0), 0.0);
        assert_eq!(aim_sigma(1e-3, f64::NAN, 1.0), 1e-3);
    }
}

#[cfg(test)]
mod pk_probe {
    use super::*;
    /// Hit-model table for the shipped guns: `cargo test -p sim-systems --lib pk_table_probe -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn pk_table_probe() {
        let cat = content::catalogue::catalogue();
        for (id, d) in cat.parts.iter() {
            let content::part::PartFunction::Gun(g) = &d.function else { continue };
            let Some(band) = cat.bands.iter().find(|b| b.band == g.band) else { continue };
            let table = range_table(g.muzzle_mps, band);
            let eff = crate::doctrine::gun_range_from_table(&table, g.dispersion_mrad, 0.10, band.cap_range_m);
            let mut row = format!("{} ({} mm, {:.1} mrad, cap {:.0}): gauge 10% range {eff:.0} m | ref", id.as_str(), g.calibre_mm, g.dispersion_mrad, band.cap_range_m);
            for r in [1_000.0, 2_000.0, 3_000.0, 4_000.0, 5_000.0, 6_000.0, 8_000.0, 10_000.0, 12_000.0] { if r <= band.cap_range_m { row += &format!(" {:.0}:{:.2}", r / 1000.0, single_round_pk(&table, r, g.dispersion_mrad, 40.0, 6.0)); } }
            for (name, rcs, aspect) in [("fac broadside", 93.0, 1.57), ("gunboat bow", 33.0, 0.0), ("corvette beam", 267.0, 1.57)] {
                let b = silhouette(rcs, Some(aspect));
                let (vp, vv, c) = lock_errors(3_000.0);
                let e = ShotErrors { angular_rad: laying_sigma_rad(g.dispersion_mrad, 1.0, SeaState::BEAUFORT_4), var_pos: vp, var_vel: vv, cov: c, var_pos_along: vp, var_vel_along: vv, cov_along: c, age_s: 0.0, manoeuvre_mps2: 0.3, manoeuvre_along_mps2: 0.3 };
                row += &format!(" | {name} {:?} 10% at {:.0}", (b.width_m.round(), b.height_m.round(), b.depth_m.round()), pk_range(&table, &e, &b, 0.10, band.cap_range_m));
            }
            eprintln!("{row}");
            if g.calibre_mm == 76 { for r in table.iter().filter(|r| r.range_m % 1000.0 == 0.0) { eprintln!("    {:.0} m: elev {:.2} deg tof {:.2} s fall {:.2} deg", r.range_m, r.elevation_rad.to_degrees(), r.time_of_flight_s, r.fall_angle_rad.to_degrees()); } }
        }
    }
}
