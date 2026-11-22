//! Unit `sys-tracks` (hard: filter numerics and association gates are CI-gated).
//! Stage 3 of the pipeline: fused, filtered tracks per side at 10 Hz. DESIGN.md L458-468, L1661.
//! The Sensors officer's `TrackFusion` stage is this module ("fuse detections into tracks with a simple gate;
//! no multi-hypothesis tracking", L309).
//!
//! - Alpha-beta filter per track; beta = alpha^2 / (2 - alpha). Alpha by updating sensor: fire-control radar and
//!   seekers 0.5, search radar 0.4, ESM, sonar and visual 0.3.
//! - Uncertainty grows between updates: r(t) = sqrt(r0^2 + (sigma_v t)^2 + (0.5 sigma_a t^2)^2), with sigma_a
//!   by domain from `TrackTuning::sigma_a` (surface and subsurface 0.3, aircraft 20, missiles 50 m/s^2).
//! - Bearing-only tracks carry process noise sigma_a = 0.5 m/s^2 and a single-sensor range uncertainty never
//!   below 10% of the estimated range.
//! - Fusion across sensors is inverse-variance weighting. Own-side units share detections over the datalink
//!   at 10 Hz into one picture per side (allied sides included, `SideDef::allied`).
//! - Association: one gate, no multi-hypothesis. A detection joins the nearest track whose 99% contour
//!   (Mahalanobis `TrackTuning::gate_sigma` = 3.03) contains it, else it starts a new track.
//! - Drop after no update for 120 s (surface, subsurface) or 20 s (air, weapon); the plot keeps a last-known
//!   mark for 10 minutes. Track numbers are never reused.
//! - Grade: Lock = updated by a fire-control radar or seeker within the last 1 s; Fine = 95% radius at most
//!   250 m; Coarse = at most 2 km; Bearing = bearing-only or wider.
//! - The drawn shape is the 95% contour (2.45 sigma in 2D).
use crate::ident::CandidateSet;
use crate::prelude::*;
mod fit;
pub use fit::{FixShape, LineFit, MotionEstimate, MotionFit, FIT_LONG_S, FIT_SHORT_S};
use crate::protocol::snapshot::{SourceMask, UncertaintyShape, VerticalTrend};
use crate::sensors::Detection;
use content::part::SensorKind;

/// 95% contour factor in 2D (L460).
pub const SIGMA_95: f64 = 2.45;
/// Grade thresholds in metres of 95% radius, and the lock freshness in ticks (L466).
pub const FINE_RADIUS_M: f64 = 250.0;
pub const COARSE_RADIUS_M: f64 = 2_000.0;
pub const LOCK_FRESH_TICKS: u64 = 60;

/// Filter gain alpha for the updating sensor (L462). Real code: spec constants.
pub fn alpha_for(kind: SensorKind) -> f64 {
    match kind { SensorKind::FireControlRadar | SensorKind::Seeker => 0.5, SensorKind::SearchRadar | SensorKind::NavRadar => 0.4, _ => 0.3 }
}
/// beta = alpha^2 / (2 - alpha) (L462).
pub fn beta_for(alpha: f64) -> f64 { alpha * alpha / (2.0 - alpha) }

/// Bearing-only state: a ray from the observing sensor with a range guess.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BearingState { pub origin: DVec3, pub bearing_rad: f64, pub bearing_rate: f64, pub sigma_bearing_rad: f64, pub range_m: f64, pub sigma_range_m: f64, pub sensors: u8 }

/// One identification step kept for the track card (L477).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RungStep { pub rung: IdRung, pub reason: RungReason, pub tick: u64 }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub side: SideId,
    pub domain: Domain,
    /// Ground-truth entity most recently associated. Server bookkeeping only (pairs the symbol with the drawn
    /// model, gate statistics); association and doctrine never read it.
    pub truth: EntityId,
    pub pos: DVec3,
    pub vel: DVec3,
    /// 1-sigma position radius at the last update and velocity sigma; see the growth law above.
    pub r0_m: f64,
    pub sigma_v: f64,
    pub bearing_only: Option<BearingState>,
    pub created_tick: u64,
    pub last_update_tick: u64,
    /// Last update by a fire-control radar or seeker.
    pub last_lock_tick: Option<u64>,
    /// Set when dropped; the row stays as a last-known mark until `TrackTuning::last_known_s` passes.
    pub dropped_tick: Option<u64>,
    pub grade: TrackGrade,
    pub sources: SourceMask,
    pub trend: VerticalTrend,
    // ---- identification (owned by `crate::ident`) ----
    pub rung: IdRung,
    pub rung_reason: RungReason,
    pub candidates: CandidateSet,
    pub class: Option<ClassId>,
    pub history: Vec<RungStep>,
    /// Apparent RCS (running mean) and the ticks it and the speed have been consistent with one type (L473).
    pub rcs_m2: f32,
    pub consistent_ticks: u32,
    /// Emitter library indices heard on this track.
    pub emitters: Vec<u16>,
    pub id_override: Option<(IdRung, Option<ClassId>)>,
    // ---- stance (owned by `crate::ident::stance`) ----
    pub stance: Stance,
    pub stance_reason: StanceReason,
    pub stance_override: Option<Stance>,
    pub fired_on_us: bool,
    // ---- engagement bookkeeping (written by `crate::engage`) ----
    /// Predicted closest approach to any own-side hull within 500 m in the next 60 s (L486).
    pub inbound: bool,
    pub engaged_by: u8,
    pub emitting: bool,
    // ---- fire control (owned by `fit`) ----
    /// Fire-control motion fit of the ranged fixes (surface and subsurface tracks), see [`MotionFit`].
    #[serde(default)]
    pub fit: Option<MotionFit>,
    /// The side has seen this contact strike its colours or lie dead in the water with no working propulsion
    /// (`VesselStatus::Disabled`) at its latest association, so the officers no longer engage it unless the
    /// player designates it. Written by the Sim's track phase.
    #[serde(default)]
    pub out_of_action: bool,
    /// The side has seen this contact with its colours struck at its latest association (the HUD track card's
    /// Struck line). Written by the Sim's track phase with `out_of_action`.
    #[serde(default)]
    pub struck: bool,
}
impl Track {
    /// 1-sigma radius now (growth law, L463). Unit: `sys-tracks`.
    ///
    /// sqrt(r0^2 + (sigma_v t)^2 + (0.5 sigma_a t^2)^2) with t the time since the last update. Bearing-only
    /// tracks use sigma_a = 0.5 m/s^2 and add the velocity diffusion of an unmodelled target course
    /// (sigma_v^2 t x 30 s): their velocity is not estimated, so position uncertainty must grow with it.
    pub fn sigma_m(&self, tick: u64, tuning: &content::tuning::TrackTuning) -> f64 {
        let r0 = fin(self.r0_m);
        (r0 * r0 + self.growth2(self.age_s(tick), tuning)).sqrt()
    }
    /// 95% radius now in metres. Unit: `sys-tracks`.
    pub fn radius95_m(&self, tick: u64, tuning: &content::tuning::TrackTuning) -> f64 { SIGMA_95 * self.sigma_m(tick, tuning) }
    /// Predicted position at a tick (constant velocity). Unit: `sys-tracks`.
    pub fn predict(&self, tick: u64) -> DVec3 {
        let p = self.pos + self.vel * self.age_s(tick);
        if p.is_finite() { p } else if self.pos.is_finite() { self.pos } else { DVec3::ZERO }
    }
    /// The shape the plot draws now. Unit: `sys-tracks`.
    ///
    /// Bearing-only tracks draw a wedge from the observing sensor over the 95% bearing and range interval;
    /// every other track a circle of its 95% radius (a small one under a fire-control lock).
    pub fn shape(&self, tick: u64, tuning: &content::tuning::TrackTuning) -> UncertaintyShape {
        if let Some(bs) = &self.bearing_only {
            let g2 = self.growth2(self.age_s(tick), tuning);
            let p = self.predict(tick);
            let (qx, qz) = (p.x - bs.origin.x, p.z - bs.origin.z);
            let range = (qx * qx + qz * qz).sqrt().max(1.0);
            let bearing = if qx == 0.0 && qz == 0.0 { fin(bs.bearing_rad) } else { compass_bearing(DVec3::new(qx, 0.0, qz)) };
            let sc = (sq(bs.sigma_bearing_rad * bs.range_m) + g2).sqrt();
            let sr = (sq(bs.sigma_range_m) + g2).sqrt();
            let half = libm::atan2(SIGMA_95 * sc, range).clamp(0.0, core::f64::consts::FRAC_PI_2);
            return UncertaintyShape::Wedge {
                apex: DVec2::new(bs.origin.x, bs.origin.z), bearing_rad: bearing as f32, half_angle_rad: half as f32,
                near_m: (range - SIGMA_95 * sr).max(0.0) as f32, far_m: (range + SIGMA_95 * sr) as f32,
            };
        }
        UncertaintyShape::Circle { radius_m: self.radius95_m(tick, tuning) as f32 }
    }
    pub fn is_live(&self) -> bool { self.dropped_tick.is_none() }
    /// The fire-control motion estimate of a live ranged surface or subsurface track (see [`MotionFit`]), or
    /// `None` while it has too few fixes. Unit: `sys-tracks`.
    pub fn fire_control(&self) -> Option<MotionEstimate> {
        if !self.is_live() || self.bearing_only.is_some() || !matches!(self.domain, Domain::Surface | Domain::Subsurface) { return None; }
        self.fit.as_ref().and_then(MotionFit::estimate)
    }
    /// The position at `tick` and the velocity a gun solution leads with: the fire-control estimate's when the
    /// track has one, else the filter's ([`Track::predict`], `vel`). Height and vertical velocity are always
    /// the filter's (a surface track sits on the sea). Unit: `sys-tracks`.
    pub fn gun_target(&self, tick: u64) -> (DVec3, DVec3) {
        let (p, v) = (self.predict(tick), if self.vel.is_finite() { self.vel } else { DVec3::ZERO });
        match self.fire_control() {
            Some(e) => { let q = e.predict(tick); if q.is_finite() && e.vel.is_finite() { (DVec3::new(q.x, p.y, q.y), DVec3::new(e.vel.x, v.y, e.vel.y)) } else { (p, v) } }
            None => (p, v),
        }
    }
}

/// What changed in a picture this update; the Sim turns these into `SimEvent`s.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum TrackChange { Created(TrackId), Dropped(TrackId), Expired(TrackId), Merged { from: TrackId, into: TrackId }, Split { from: TrackId, new: TrackId }, Grade(TrackId, TrackGrade) }

/// One side's fused picture. Tracks ascending by id; ids start at 1 and never repeat.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SidePicture { pub side: SideId, pub tracks: Vec<Track>, pub next_id: u32 }
impl SidePicture {
    pub fn get(&self, id: TrackId) -> Option<&Track> { self.tracks.binary_search_by(|t| t.id.cmp(&id)).ok().map(|i| &self.tracks[i]) }
    pub fn get_mut(&mut self, id: TrackId) -> Option<&mut Track> { match self.tracks.binary_search_by(|t| t.id.cmp(&id)) { Ok(i) => Some(&mut self.tracks[i]), Err(_) => None } }
    /// Live track currently associated with a truth entity, if any (server bookkeeping).
    pub fn track_of(&self, entity: EntityId) -> Option<&Track> { self.tracks.iter().find(|t| t.truth == entity && t.is_live()) }
}

/// Every side's picture, indexed by `SideId`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pictures { pub sides: Vec<SidePicture> }
impl Pictures {
    pub fn side(&self, side: SideId) -> Option<&SidePicture> { self.sides.get(side.index()) }
    pub fn side_mut(&mut self, side: SideId) -> Option<&mut SidePicture> { self.sides.get_mut(side.index()) }
}

/// The `TrackFusion` stage for one side at one 10 Hz update: associate each detection (ascending sensor,
/// target order) to the nearest gated track or start a new one, apply the alpha-beta update with the
/// sensor's gain and inverse-variance fusion, fold allied sides' detections in as datalink, refresh grade,
/// trend and sources, merge duplicate tracks (the association gate at L467: no truth object in detection range
/// may hold two live tracks for more than 10 s), drop stale tracks and expire old last-known marks. Own-side entities never form tracks from detections: they are Friend tracks fed by
/// datalink positions (L500). Identification and stance are updated by the caller right after with
/// `crate::ident`. Unit: `sys-tracks`.
///
/// Returns the number of track updates (detections applied plus datalink friend refreshes), the work count of
/// the track share of the sensor budget.
pub fn fuse(picture: &mut SidePicture, detections: &[Detection], own_side: &[(EntityId, Domain, DVec3, DVec3)], ctx: &SimCtx<'_>, changes: &mut Vec<TrackChange>) -> u32 {
    fuse_core(picture, detections, own_side, ctx.tick.0, ctx.tuning, changes)
}

/// Local-control track for one mount that lost its Data link: built from that part's own sensor alone with 3x
/// error (L394). Returns a transient track not stored in the picture. Unit: `sys-tracks`.
///
/// The detections already carry the 3x local-control error (the scheduler applied the factor), so it is not
/// applied again. With no previous track the nearest return is taken; afterwards the return that best fits the
/// gate. A track with no fitting return coasts until its drop time, then gives way to the nearest new return.
pub fn local_track(detections: &[Detection], sensor: SensorRef, previous: Option<&Track>, ctx: &SimCtx<'_>) -> Option<Track> {
    local_track_core(detections, sensor, previous, ctx.tick.0, ctx.tuning)
}


// ------------------------------------------------------------------------------------------------------------
// Filter internals
// ------------------------------------------------------------------------------------------------------------

use crate::sensors::radar::{horizon_m, wrap_pi, AIR_HEIGHT_M};
use crate::sensors::Measurement;
use crate::signature::compass_bearing;
use content::tuning::{TrackTuning, TuningTable};

const TICK_S: f64 = 1.0 / sim_core::TICK_HZ as f64;
/// Position accuracy of a datalink friend report, 1 sigma metres, and its velocity sigma.
pub const DATALINK_SIGMA_M: f64 = 5.0;
pub const DATALINK_SIGMA_V: f64 = 0.5;
/// Two bearings from origins farther apart than this, within [`CROSS_FIX_WINDOW_TICKS`] of each other,
/// triangulate (a real cross-fix). Bearings from one place, or far apart in time, only refine the bearing.
pub const CROSS_FIX_BASELINE_M: f64 = 100.0;
pub const CROSS_FIX_WINDOW_TICKS: u64 = 120;
/// Velocity correlation time of the diffusion term bearing-only tracks add to their growth, seconds.
pub const BEARING_DIFFUSION_S: f64 = 30.0;
/// A track with no update for this long starts its source mask afresh at its next update (10 s).
pub const SOURCE_STALE_TICKS: u64 = 600;
/// Speeds above which a surface-domain track, confidently (speed minus its 95% velocity error), must be
/// airborne or a weapon, m/s.
pub const AIR_SPEED_MPS: f64 = 35.0;
pub const WEAPON_SPEED_MPS: f64 = 100.0;
/// Vertical speed for the climb and dive glyph, m/s; it must also exceed the velocity sigma.
pub const TREND_MIN_MPS: f64 = 5.0;
/// Re-acquisition: when the predicted sigma exceeds this multiple of the measurement sigma, the update uses
/// the inverse-variance weight instead of the fixed alpha (a coasting or fresh track snaps to a good fix).
pub const REACQUIRE_RATIO: f64 = 4.0;
/// Merge distance, in combined sigma, for a track born this batch from a single detection next to an
/// established track. The association gate (3.03 sigma, 99%) leaves 1 detection in 100 outside it by
/// chance; such a detection starts a track (L467) that is folded back at once instead of showing as a
/// duplicate. 4.5 sigma passes 1 in 25,000 real neighbours; a neighbour reported by the same sensor at the
/// same instant is never folded.
pub const FRESH_MERGE_SIGMA: f64 = 4.5;
/// A track not updated for this long (5 s, two search-radar looks) while a neighbour inside
/// [`FRESH_MERGE_SIGMA`] is being updated has lost its object to that neighbour and is merged into it.
pub const GHOST_TICKS: u64 = 300;
const MIN_SIGMA_M: f64 = 0.5;
const MIN_SIGMA_V: f64 = 0.05;
const MAX_SIGMA_M: f64 = 1.0e6;
const MAX_BEARING_RATE: f64 = 0.5;

fn fin(x: f64) -> f64 { if x.is_finite() { x } else { 0.0 } }
fn sq(x: f64) -> f64 { let x = fin(x); x * x }
fn pos_f(x: f64) -> f64 { if x.is_finite() && x > 0.0 { x } else { 0.0 } }
/// (sin, cos) of a compass bearing. Along the bearing the world (x, z) direction is (sin, -cos); the
/// perpendicular toward increasing bearing is (cos, sin).
fn dir(b: f64) -> (f64, f64) { let b = fin(b); (libm::sin(b), libm::cos(b)) }

fn air_like(d: Domain) -> bool { matches!(d, Domain::Air | Domain::Weapon) }
fn compatible(hint: Option<Domain>, track: Domain) -> bool { hint.is_none_or(|h| air_like(h) == air_like(track)) }

fn sigma_a(t: &TrackTuning, domain: Domain, bearing_only: bool) -> f64 {
    let i = if bearing_only { 3 } else { match domain { Domain::Surface | Domain::Subsurface => 0, Domain::Air => 1, Domain::Weapon => 2 } };
    pos_f(t.sigma_a[i])
}
/// 1-sigma velocity uncertainty of a fresh track, m/s, by domain.
fn prior_speed(d: Domain) -> f64 { match d { Domain::Surface => 8.0, Domain::Subsurface => 6.0, Domain::Air => 150.0, Domain::Weapon => 250.0 } }
/// Largest plausible speed, m/s, by domain (velocity estimates are clamped to it).
fn max_speed(d: Domain) -> f64 { match d { Domain::Surface => 30.0, Domain::Subsurface => 25.0, Domain::Air => 400.0, Domain::Weapon => 1_200.0 } }
/// The update interval the sensor's gains were set for, seconds: one search-radar look per 2.5 s rotation,
/// 10 Hz fire control and seekers, 2 Hz for the rest (L412). The velocity gain divides by at least this, so
/// two sensors hitting one track a moment apart cannot fling its velocity.
fn nominal_interval_s(kind: SensorKind) -> f64 {
    match kind { SensorKind::SearchRadar | SensorKind::NavRadar => 2.5, SensorKind::FireControlRadar | SensorKind::Seeker => 0.1, _ => 0.5 }
}
fn drop_ticks(d: Domain, t: &TrackTuning) -> u64 {
    let s = match d { Domain::Surface | Domain::Subsurface => t.drop_surface_s, Domain::Air | Domain::Weapon => t.drop_air_s };
    (pos_f(s) * sim_core::TICK_HZ as f64).min(u64::MAX as f64 / 2.0) as u64
}
fn source_of(kind: SensorKind) -> SourceMask {
    match kind {
        SensorKind::SearchRadar | SensorKind::NavRadar => SourceMask::SEARCH_RADAR,
        SensorKind::FireControlRadar => SourceMask::FIRE_CONTROL,
        SensorKind::Seeker => SourceMask::SEEKER,
        SensorKind::Esm => SourceMask::ESM,
        SensorKind::PassiveSonar => SourceMask::PASSIVE_SONAR,
        SensorKind::ActiveSonar => SourceMask::ACTIVE_SONAR,
        SensorKind::Visual => SourceMask::VISUAL,
        SensorKind::Camera => SourceMask::CAMERA,
    }
}
fn is_lock_kind(kind: SensorKind) -> bool { matches!(kind, SensorKind::FireControlRadar | SensorKind::Seeker) }

/// Horizontal covariance (x, z), m^2.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cov2 { xx: f64, xz: f64, zz: f64 }
impl Cov2 {
    fn iso(v: f64) -> Self { Cov2 { xx: v, xz: 0.0, zz: v } }
    /// Diagonal in the frame of compass bearing `b`: `along` variance on the line of sight, `cross` across it.
    fn los(b: f64, along: f64, cross: f64) -> Self { let (s, c) = dir(b); Self::los_sc(s, c, along, cross) }
    /// [`Cov2::los`] from the bearing's `dir` (sine, cosine), computed once by the caller.
    fn los_sc(s: f64, c: f64, along: f64, cross: f64) -> Self {
        let (ux, uz, nx, nz) = (s, -c, c, s);
        Cov2 { xx: along * ux * ux + cross * nx * nx, xz: along * ux * uz + cross * nx * nz, zz: along * uz * uz + cross * nz * nz }
    }
    fn add(self, o: Cov2) -> Self { Cov2 { xx: self.xx + o.xx, xz: self.xz + o.xz, zz: self.zz + o.zz } }
    fn quad(self, x: f64, z: f64) -> f64 { self.xx * x * x + 2.0 * self.xz * x * z + self.zz * z * z }
    fn trace(self) -> f64 { self.xx + self.zz }
    /// d' C^-1 d, or `None` when the matrix is singular or bad.
    fn inv_quad(self, x: f64, z: f64) -> Option<f64> {
        let det = self.xx * self.zz - self.xz * self.xz;
        if !(det.is_finite() && det > 1e-12) { return None; }
        let q = (self.zz * x * x - 2.0 * self.xz * x * z + self.xx * z * z) / det;
        if q.is_finite() { Some(q.max(0.0)) } else { None }
    }
    /// Variances on and across the line of sight `b`, inflated by the dropped correlation so the diagonal
    /// stays a conservative bound of the full matrix.
    fn to_los(self, b: f64) -> (f64, f64) {
        let (s, c) = dir(b);
        let along = self.quad(s, -c);
        let cross = self.quad(c, s);
        let off = (self.xx * s * c + self.xz * (s * s - c * c) - self.zz * s * c).abs();
        (pos_f(along) + off, pos_f(cross) + off)
    }
}

impl Track {
    fn age_s(&self, tick: u64) -> f64 { tick.saturating_sub(self.last_update_tick) as f64 * TICK_S }
    fn growth2(&self, t: f64, tuning: &TrackTuning) -> f64 {
        let t = pos_f(t);
        let sv = fin(self.sigma_v).abs();
        let sa = sigma_a(tuning, self.domain, self.bearing_only.is_some());
        let mut g = sq(sv * t) + sq(0.5 * sa * t * t);
        if self.bearing_only.is_some() { g += sv * sv * t * BEARING_DIFFUSION_S; }
        if g.is_finite() { g } else { MAX_SIGMA_M * MAX_SIGMA_M }
    }
    /// Horizontal covariance now.
    fn cov_h(&self, tick: u64, tuning: &TrackTuning) -> Cov2 {
        let g = self.growth2(self.age_s(tick), tuning);
        match &self.bearing_only {
            None => Cov2::iso(sq(self.r0_m) + g),
            Some(bs) => Cov2::los(bs.bearing_rad, sq(bs.sigma_range_m), sq(bs.sigma_bearing_rad * bs.range_m)).add(Cov2::iso(g)),
        }
    }
    /// Trace of [`Track::cov_h`] without trigonometry, for quick rejection.
    fn cov_trace(&self, tick: u64, tuning: &TrackTuning) -> f64 {
        let g = self.growth2(self.age_s(tick), tuning);
        match &self.bearing_only {
            None => 2.0 * (sq(self.r0_m) + g),
            Some(bs) => sq(bs.sigma_range_m) + sq(bs.sigma_bearing_rad * bs.range_m) + 2.0 * g,
        }
    }
    /// Grade now (L466).
    fn grade_at(&self, tick: u64, tuning: &TrackTuning) -> TrackGrade {
        if self.last_lock_tick.is_some_and(|l| l <= tick && tick - l <= LOCK_FRESH_TICKS) { return TrackGrade::Lock; }
        if self.bearing_only.is_some() { return TrackGrade::Bearing; }
        let r = self.radius95_m(tick, tuning);
        if r <= FINE_RADIUS_M { TrackGrade::Fine } else if r <= COARSE_RADIUS_M { TrackGrade::Coarse } else { TrackGrade::Bearing }
    }
    fn is_friend(&self) -> bool { self.stance == Stance::Friend && self.stance_reason == StanceReason::Datalink && self.sources.contains(SourceMask::DATALINK) }
}

/// One detection prepared for association: unit vectors computed once.
#[derive(Clone, Copy)]
struct Meas<'a> {
    det: &'a Detection,
    ranged: bool,
    range: f64,
    bearing: f64,
    sin: f64,
    cos: f64,
    height: Option<f64>,
    sr: f64,
    sb: f64,
}
impl<'a> Meas<'a> {
    fn new(det: &'a Detection) -> Option<Self> {
        if !det.sensor_pos.is_finite() { return None; }
        let (ranged, range, bearing, height) = match det.measurement {
            Measurement::RangeBearing { range_m, bearing_rad, height_m } => (true, range_m, bearing_rad, height_m.filter(|h| h.is_finite())),
            Measurement::BearingOnly { bearing_rad } => (false, 0.0, bearing_rad, None),
        };
        if !bearing.is_finite() || (ranged && !(range.is_finite() && range >= 0.0)) { return None; }
        let (sin, cos) = dir(bearing);
        let sb = pos_f(det.sigma_bearing_rad).max(1e-5);
        let sr = pos_f(det.sigma_range_m).max(0.5);
        Some(Meas { det, ranged, range, bearing, sin, cos, height, sr, sb })
    }
    /// Measured point for a target height `y`, and its horizontal covariance.
    fn point(&self, y: f64) -> (DVec3, Cov2, f64) {
        let o = self.det.sensor_pos;
        let dy = fin(y) - o.y;
        let h = (self.range * self.range - dy * dy).max(0.0).sqrt();
        let p = DVec3::new(o.x + h * self.sin, fin(y), o.z - h * self.cos);
        let cross = sq(h * self.sb);
        // `sin` and `cos` are `dir(bearing)`, so this is `Cov2::los(bearing, ..)` without the trigonometry.
        (p, Cov2::los_sc(self.sin, self.cos, self.sr * self.sr, cross), h)
    }
    /// Isotropic variance of a ranged fix at horizontal range `h` whose 2.45-sigma circle holds about 95% of
    /// the anisotropic range-and-bearing error: the per-axis mean plus 0.14 of the axis difference (exact at
    /// both ends: equal axes, and one dominant axis where the 95% radius is 1.96 sigma).
    fn iso_var(&self, h: f64) -> f64 { iso_equivalent(self.sr * self.sr, sq(h * self.sb)) }
    fn height_var(&self) -> f64 { sq(self.range * self.sb).max(1.0) }
    /// The fix's error shape at horizontal range `h`: the line of sight and the variances along and across it.
    fn shape(&self, h: f64) -> FixShape { FixShape { los: DVec2::new(self.sin, -self.cos), along_var: self.sr * self.sr, cross_var: sq(h * self.sb) } }
}

/// See [`Meas::iso_var`].
fn iso_equivalent(a: f64, b: f64) -> f64 { let (a, b) = (pos_f(a), pos_f(b)); 0.5 * (a + b) + ANISOTROPY_WEIGHT * (a - b).abs() }
/// Weight of the axis difference in [`iso_equivalent`]: (1.96 / 2.45)^2 - 0.5.
const ANISOTROPY_WEIGHT: f64 = 0.14;

/// Store a fixed-gain posterior (pp, pv, vv) as the growth law's (r0, sigma_v), which has no cross term:
/// sigma_v keeps the velocity variance (the recursion needs it unbiased) and r0 takes the cross term's
/// contribution at the sensor's next nominal look T, pp + 2 pv T, so the law is exact where the track is next
/// updated and conservative before it.
const FOLD_K: f64 = 1.0;
fn fold(pp: f64, pv: f64, vv: f64, t_nominal: f64, vmax: f64) -> (f64, f64) {
    let (pp, vv) = (pos_f(pp), pos_f(vv));
    let t = if t_nominal.is_finite() && t_nominal > 0.0 { t_nominal } else { 1.0 };
    let r2 = if pv.is_finite() && pv > 0.0 { pp + FOLD_K * pv * t } else { pp };
    (r2.sqrt().clamp(MIN_SIGMA_M, MAX_SIGMA_M), vv.sqrt().clamp(MIN_SIGMA_V, vmax))
}

/// Joseph-form posterior of one axis for gains (a, kv) from prior (pp, pv, vv) and measurement variance r.
fn joseph(a: f64, kv: f64, pp: f64, pv: f64, vv: f64, r: f64) -> (f64, f64, f64) {
    let ppp = sq(1.0 - a) * pp + a * a * r;
    let ppv = (1.0 - a) * (pv - kv * pp) + a * kv * r;
    let pvv = vv - 2.0 * kv * pv + kv * kv * pp + kv * kv * r;
    (ppp, ppv, pvv)
}

/// Squared Mahalanobis distance of a detection from a track's prediction, or `None` when it cannot belong.
fn gate_d2(m: &Meas<'_>, t: &Track, tuning: &TrackTuning, gate2: f64) -> Option<f64> {
    if !compatible(m.det.domain_hint, t.domain) { return None; }
    // A fire-control radar or seeker measures height and hints the air; a fix it reads with no hint is at or
    // below the sea (`sensors::radar::height_reading`), so it is a hull's and never an air or weapon track's.
    // Without this a missile's track, born at its launcher, took the launcher's next lock returns and followed
    // the hull for the rest of the battle as a second, Weapon-domain track (L467 duplicates; DECISION, k2).
    if m.height.is_some() && m.det.domain_hint.is_none() && air_like(t.domain) && is_lock_kind(m.det.kind) { return None; }
    let tk = m.det.tick;
    let p = t.predict(tk);
    // d' S^-1 d <= g^2 needs |d|^2 <= g^2 lambda_max(S) <= g^2 trace(S): reject cheaply before any trig.
    let trace = t.cov_trace(tk, tuning);
    if m.ranged {
        let (z, mc, _) = m.point(m.height.unwrap_or(p.y));
        let (dx, dz) = (z.x - p.x, z.z - p.z);
        if dx * dx + dz * dz > gate2 * (trace + mc.trace()) { return None; }
        t.cov_h(tk, tuning).add(mc).inv_quad(dx, dz)
    } else {
        let o = m.det.sensor_pos;
        let (rx, rz) = (p.x - o.x, p.z - o.z);
        let along = rx * m.sin - rz * m.cos;
        if !(along > 0.0) { return None; }
        let e = rx * m.cos + rz * m.sin;
        if e * e > gate2 * (trace + sq(along * m.sb)) { return None; }
        let var = t.cov_h(tk, tuning).quad(m.cos, m.sin) + sq(along * m.sb);
        if !(var.is_finite() && var > 0.0) { return None; }
        Some(e * e / var)
    }
}

fn blank_track(id: TrackId, side: SideId, truth: EntityId, domain: Domain, tick: u64) -> Track {
    Track {
        id, side, domain, truth, pos: DVec3::ZERO, vel: DVec3::ZERO, r0_m: MIN_SIGMA_M, sigma_v: prior_speed(domain), bearing_only: None,
        created_tick: tick, last_update_tick: tick, last_lock_tick: None, dropped_tick: None, grade: TrackGrade::Bearing,
        sources: SourceMask::empty(), trend: VerticalTrend::Steady, rung: IdRung::Unknown, rung_reason: RungReason::None,
        candidates: CandidateSet::default(), class: None, history: Vec::new(), rcs_m2: 0.0, consistent_ticks: 0, emitters: Vec::new(),
        id_override: None, stance: Stance::Suspect, stance_reason: StanceReason::Default, stance_override: None, fired_on_us: false,
        inbound: false, engaged_by: 0, emitting: false, fit: None, out_of_action: false, struck: false,
    }
}

/// Domain a fresh track takes from its first detection.
fn initial_domain(m: &Meas<'_>) -> Domain {
    match m.det.domain_hint {
        Some(d) => d,
        None => if m.height.is_some_and(|h| h >= AIR_HEIGHT_M) { Domain::Air } else { Domain::Surface },
    }
}

/// Range guess and its 1-sigma for a fresh bearing-only track: a uniform prior over the sensor's plausible
/// reach. ESM: the radar horizon to an assumed enemy mast (L510); passive sonar: twice the reference range
/// (a loud hull, N = 4). Mean = half the reach, sigma = reach / sqrt(12).
fn bearing_prior(m: &Meas<'_>, tuning: &TuningTable) -> (f64, f64) {
    let reach = match m.det.kind {
        SensorKind::Esm => horizon_m(m.det.sensor_pos.y, tuning.sensors.assumed_enemy_mast_m),
        SensorKind::PassiveSonar => 2.0 * pos_f(tuning.sensors.passive_sonar_r_ref_m),
        _ => 20_000.0,
    };
    let reach = if reach.is_finite() && reach > 100.0 { reach } else { 20_000.0 };
    (0.5 * reach, reach / 12f64.sqrt())
}

fn new_track(id: TrackId, side: SideId, m: &Meas<'_>, tuning: &TuningTable) -> Track {
    let det = m.det;
    let domain = initial_domain(m);
    let mut t = blank_track(id, side, det.target, domain, det.tick);
    if m.ranged {
        let y = m.height.unwrap_or(0.0);
        let (p, _, h) = m.point(y);
        t.pos = if domain == Domain::Surface { DVec3::new(p.x, 0.0, p.z) } else { p };
        t.r0_m = m.iso_var(h).sqrt().clamp(MIN_SIGMA_M, MAX_SIGMA_M);
        if fits(domain) { t.fit = MotionFit::start(p.x, p.z, m.iso_var(h).max(MIN_SIGMA_M * MIN_SIGMA_M), det.tick, Some(m.shape(h))); }
    } else {
        let (range, sr) = bearing_prior(m, tuning);
        let o = det.sensor_pos;
        t.pos = DVec3::new(o.x + range * m.sin, 0.0, o.z - range * m.cos);
        let sc = range * m.sb;
        t.bearing_only = Some(BearingState { origin: o, bearing_rad: m.bearing, bearing_rate: 0.0, sigma_bearing_rad: m.sb, range_m: range, sigma_range_m: sr, sensors: 1 });
        t.r0_m = sr.max(sc);
    }
    t.sigma_v = prior_speed(domain);
    hold_to_surface(&mut t);
    if is_lock_kind(det.kind) { t.last_lock_tick = Some(det.tick); }
    t.sources = source_of(det.kind) | if det.side != side { SourceMask::DATALINK } else { SourceMask::empty() };
    t.rcs_m2 = if det.rcs_m2.is_finite() && det.rcs_m2 > 0.0 { det.rcs_m2 } else { 0.0 };
    t.grade = t.grade_at(det.tick, &tuning.tracks);
    t
}

/// Domain evidence of one detection (the track's `domain`; identification rungs are `crate::ident`'s).
fn apply_hint(t: &mut Track, m: &Meas<'_>) {
    let Some(h) = m.det.domain_hint else { return };
    match m.det.kind {
        // The eye and the ear tell the domain outright.
        SensorKind::Visual | SensorKind::Camera | SensorKind::PassiveSonar | SensorKind::ActiveSonar => t.domain = h,
        _ => match h {
            Domain::Air => if t.domain != Domain::Weapon { t.domain = Domain::Air },
            d => t.domain = d,
        },
    }
}

/// Alpha-beta update of a ranged (or wedge-converting) track from the ranged detections of one batch, fused
/// by inverse-variance weighting.
fn ranged_update(t: &mut Track, rs: &[Meas<'_>], tuning: &TrackTuning) {
    let tu = rs.iter().map(|m| m.det.tick).max().unwrap_or(t.last_update_tick).max(t.last_update_tick);
    let dt = t.age_s(tu);
    let p = t.predict(tu);
    let was_bearing = t.bearing_only.is_some();
    let sp2 = sq(t.sigma_m(tu, tuning));
    // Inverse-variance fusion of the batch, each fix carried to the update tick on the track's velocity.
    let (mut wx, mut wz, mut w) = (0.0, 0.0, 0.0);
    let (mut hy, mut hw) = (0.0, 0.0);
    let mut best: Option<(f64, SensorKind)> = None;
    let mut best_shape: Option<(f64, FixShape)> = None;
    for m in rs {
        let y = m.height.unwrap_or(p.y);
        let (mut z, _, h) = m.point(y);
        if !was_bearing { z += t.vel * (tu.saturating_sub(m.det.tick) as f64 * TICK_S); }
        let var = m.iso_var(h).max(MIN_SIGMA_M * MIN_SIGMA_M);
        let wi = 1.0 / var;
        if !(z.is_finite() && wi.is_finite()) { continue; }
        wx += wi * z.x;
        wz += wi * z.z;
        w += wi;
        if let Some(hh) = m.height { let wv = 1.0 / m.height_var(); hy += wv * hh; hw += wv; }
        if best.is_none_or(|(bw, _)| wi > bw) { best = Some((wi, m.det.kind)); }
        if best_shape.is_none_or(|(bw, _)| wi > bw) { best_shape = Some((wi, m.shape(h))); }
    }
    // The fused fix's shape: the heaviest fix's (the others add little to a fix they weigh far less than).
    let shape = best_shape.map(|(_, s)| s);
    let Some((_, kind)) = best else { return };
    if !(w > 0.0) { return; }
    let (zx, zz) = (wx / w, wz / w);
    let vf = 1.0 / w;
    let zy = if hw > 0.0 { Some(hy / hw) } else { None };

    if was_bearing {
        // A wedge's range is a guess: the first fix replaces it.
        t.bearing_only = None;
        t.pos = DVec3::new(zx, if t.domain == Domain::Surface { 0.0 } else { zy.unwrap_or(0.0) }, zz);
        t.vel = DVec3::ZERO;
        t.r0_m = vf.sqrt().clamp(MIN_SIGMA_M, MAX_SIGMA_M);
        t.sigma_v = prior_speed(t.domain);
        t.last_update_tick = tu;
        t.fit = if fits(t.domain) { MotionFit::start(zx, zz, vf, tu, shape) } else { None };
        hold_to_surface(t);
        return;
    }
    let alpha = alpha_for(kind);
    let beta = beta_for(alpha);
    let a = if sp2 > sq(REACQUIRE_RATIO) * vf { sp2 / (sp2 + vf) } else { alpha };
    let k = coarse_fix_scale(alpha, sp2, vf);
    let (a, kv) = (a * k, k * beta / dt.max(nominal_interval_s(kind)));
    let r = DVec3::new(zx - p.x, zy.map_or(0.0, |y| y - p.y), zz - p.z);
    let mut pos = p + r * a;
    let mut vel = t.vel + r * kv;
    // A surface contact floats: it has no vertical motion and sits at the sea surface. Height noise (the
    // bearing error times range) would otherwise become a climb rate, and a gun solution leading a phantom
    // 10 m/s climb lays several hundred metres long or short at medium range.
    if t.domain == Domain::Surface { pos.y = 0.0; vel.y = 0.0; }
    let vmax = max_speed(t.domain);
    let sp = vel.length();
    if !(sp.is_finite()) { vel = t.vel; } else if sp > vmax { vel *= vmax / sp; }
    // Fixed-gain covariance (Joseph form per axis) from the growth-law prior, folded back into the law.
    let sa = sigma_a(tuning, t.domain, false);
    let sv2 = sq(t.sigma_v);
    let ppv = sv2 * dt + 0.5 * sa * sa * dt * dt * dt;
    let pvv = sv2 + sq(sa * dt);
    let (pp, pv, vv) = joseph(a, kv, sp2, ppv, pvv, vf);
    if pos.is_finite() { t.pos = pos; }
    t.vel = vel;
    (t.r0_m, t.sigma_v) = fold(pp, pv, vv, nominal_interval_s(kind), vmax);
    t.last_update_tick = tu;
    // The fire-control fit takes the fused fix itself, never the filtered position (L462's gains stay the
    // filter's; the fit is the consumer's estimate).
    t.fit = if fits(t.domain) { MotionFit::update(t.fit, zx, zz, vf, tu, shape) } else { None };
    hold_to_surface(t);
}

/// Domains the fire-control fit runs for: hulls, whose courses guns lead. Air and weapon tracks keep the
/// filter's estimate (point defence and air gunnery are tuned and gated on it, L606).
fn fits(d: Domain) -> bool { matches!(d, Domain::Surface | Domain::Subsurface) }

/// Gain scale for a fix far coarser than the track (L460: sensors fuse by inverse-variance weighting). When the
/// measurement variance is more than `REACQUIRE_RATIO`^2 times the track's predicted variance, the fixed
/// Benedict-Bordner gains give way to the inverse-variance weight `prior / (prior + meas)`, and the velocity
/// gain shrinks in step; otherwise 1 (the sensor's own gains, untouched). Without it a lookout's 10% range
/// guess or a search radar's 50 m fix landed on a fire-control lock with the full fixed gain: the lock's
/// velocity jumped by 10 to 30 m/s each time, and guns led a 9 m/s target by up to 100 m.
fn coarse_fix_scale(alpha: f64, prior_var: f64, meas_var: f64) -> f64 {
    if !(alpha > 0.0 && prior_var > 0.0 && meas_var.is_finite() && meas_var > sq(REACQUIRE_RATIO) * prior_var) { return 1.0; }
    (prior_var / (prior_var + meas_var) / alpha).min(1.0)
}

/// A surface track sits on the sea: its height is sea level and it has no vertical velocity. Height readings
/// carry the bearing error as an elevation error (several metres at gun ranges), and the beta gain would turn
/// that noise into a vertical velocity; carried over a shell's time of flight it put the predicted aim point
/// tens of metres above or below the water, and the gun laid for that point overshot or fell short by up to
/// 2 km against a stationary hull. Air, weapon and subsurface tracks keep their measured height and trend.
fn hold_to_surface(t: &mut Track) {
    if t.domain != Domain::Surface { return; }
    t.pos.y = 0.0;
    t.vel.y = 0.0;
}

/// Turn a ranged track whose 95% radius has grown past Coarse into a wedge on the observing sensor.
fn to_wedge(t: &mut Track, origin: DVec3, tick: u64, tuning: &TrackTuning) {
    let p = t.predict(tick);
    let c = t.cov_h(tick, tuning);
    let (qx, qz) = (p.x - origin.x, p.z - origin.z);
    let range = (qx * qx + qz * qz).sqrt().max(1.0);
    let b = compass_bearing(DVec3::new(qx, 0.0, qz));
    let (along, cross) = c.to_los(b);
    t.pos = p;
    t.vel = DVec3::ZERO;
    t.fit = None;
    t.sigma_v = t.sigma_v.max(prior_speed(t.domain));
    t.bearing_only = Some(BearingState { origin, bearing_rad: b, bearing_rate: 0.0, sigma_bearing_rad: cross.sqrt() / range, range_m: range, sigma_range_m: along.sqrt(), sensors: 1 });
    t.r0_m = along.max(cross).sqrt().clamp(MIN_SIGMA_M, MAX_SIGMA_M);
    t.last_update_tick = tick;
}

/// One bearing detection applied to a track.
fn bearing_update(t: &mut Track, m: &Meas<'_>, tuning: &TrackTuning) {
    let tk = m.det.tick.max(t.last_update_tick);
    let o = m.det.sensor_pos;
    if t.bearing_only.is_none() && t.radius95_m(tk, tuning) > COARSE_RADIUS_M { to_wedge(t, o, tk, tuning); }
    let dt = t.age_s(tk);
    let p = t.predict(tk);
    let c = t.cov_h(tk, tuning);
    let (rx, rz) = (p.x - o.x, p.z - o.z);
    let along = rx * m.sin - rz * m.cos;
    if !(along > 1.0) { return; }
    let e = rx * m.cos + rz * m.sin;
    let mv = sq(along * m.sb);
    let alpha = alpha_for(m.det.kind);
    let beta = beta_for(alpha);
    let kv = beta / dt.max(nominal_interval_s(m.det.kind));
    let (nx, nz) = (m.cos, m.sin);
    match t.bearing_only {
        None => {
            // Ranged track: a bearing pulls it across the line of sight only.
            let cpv = c.quad(nx, nz);
            let k = coarse_fix_scale(alpha, cpv, mv);
            let (alpha, kv) = (alpha * k, kv * k);
            let pos = DVec3::new(p.x - alpha * e * nx, p.y, p.z - alpha * e * nz);
            let mut vel = t.vel - DVec3::new(nx, 0.0, nz) * (kv * e);
            let vmax = max_speed(t.domain);
            let sp = vel.length();
            if !sp.is_finite() { vel = t.vel; } else if sp > vmax { vel *= vmax / sp; }
            let sa = sigma_a(tuning, t.domain, false);
            let sv2 = sq(t.sigma_v);
            let ppv = sv2 * dt + 0.5 * sa * sa * dt * dt * dt;
            let pvv = sv2 + sq(sa * dt);
            let (cp, cpv_post, cv) = joseph(alpha, kv, cpv, ppv, pvv, mv);
            if pos.is_finite() { t.pos = pos; }
            t.vel = vel;
            // Along the line of sight nothing was measured: that axis keeps its prior.
            let (r_cross, v_cross) = fold(cp, cpv_post, cv, nominal_interval_s(m.det.kind), vmax);
            let (r_along, v_along) = fold(cpv, ppv, pvv, nominal_interval_s(m.det.kind), vmax);
            t.r0_m = iso_equivalent(sq(r_along), sq(r_cross)).sqrt().clamp(MIN_SIGMA_M, MAX_SIGMA_M);
            t.sigma_v = (0.5 * (sq(v_along) + sq(v_cross))).sqrt().clamp(MIN_SIGMA_V, vmax);
            t.last_update_tick = tk;
        }
        Some(bs) => {
            let dx = o.x - bs.origin.x;
            let dz = o.z - bs.origin.z;
            let cross_fix = (dx * dx + dz * dz).sqrt() > CROSS_FIX_BASELINE_M && tk.saturating_sub(t.last_update_tick) <= CROSS_FIX_WINDOW_TICKS;
            let (q, cpost, sensors) = if cross_fix {
                // Triangulation: Kalman update on the offset from the new ray (inverse-variance fusion).
                let cn = (c.xx * nx + c.xz * nz, c.xz * nx + c.zz * nz);
                let s = c.quad(nx, nz) + mv;
                if !(s.is_finite() && s > 0.0) { return; }
                let k = (cn.0 / s, cn.1 / s);
                let q = DVec3::new(p.x - k.0 * e, 0.0, p.z - k.1 * e);
                let cp = Cov2 { xx: c.xx - k.0 * k.0 * s, xz: c.xz - k.0 * k.1 * s, zz: c.zz - k.1 * k.1 * s };
                (q, cp, 2u8)
            } else {
                // One sensor: refine the bearing with the fixed gain; range stays as uncertain as it was.
                let (av, cv) = c.to_los(m.bearing);
                let q = DVec3::new(p.x - alpha * e * nx, 0.0, p.z - alpha * e * nz);
                let cross = sq(1.0 - alpha) * cv + alpha * alpha * mv;
                (q, Cov2::los_sc(m.sin, m.cos, av, cross), 1u8)
            };
            if !q.is_finite() { return; }
            let (qx, qz) = (q.x - o.x, q.z - o.z);
            let range = (qx * qx + qz * qz).sqrt().max(1.0);
            let b = compass_bearing(DVec3::new(qx, 0.0, qz));
            let (mut ar, cr) = cpost.to_los(b);
            if sensors == 1 { ar = ar.max(sq(pos_f(tuning.bearing_only_min_range_fraction) * range)); }
            let sr = ar.sqrt().clamp(MIN_SIGMA_M, MAX_SIGMA_M);
            let sc = cr.sqrt().clamp(MIN_SIGMA_M, MAX_SIGMA_M);
            let rate = if cross_fix || (dx * dx + dz * dz).sqrt() > CROSS_FIX_BASELINE_M {
                bs.bearing_rate
            } else {
                let pred = bs.bearing_rad + bs.bearing_rate * dt;
                (bs.bearing_rate + kv * wrap_pi(b - pred)).clamp(-MAX_BEARING_RATE, MAX_BEARING_RATE)
            };
            t.pos = DVec3::new(q.x, t.pos.y, q.z);
            t.bearing_only = Some(BearingState { origin: o, bearing_rad: b, bearing_rate: fin(rate), sigma_bearing_rad: sc / range, range_m: range, sigma_range_m: sr, sensors });
            t.r0_m = sr.max(sc);
            t.last_update_tick = tk;
        }
    }
}

/// Apply one track's detections of this batch (ascending tick): ranged fixes fused, then bearings.
fn update_track(t: &mut Track, ms: &[Meas<'_>], side: SideId, tuning: &TrackTuning) {
    if ms.is_empty() { return; }
    let first_tick = ms.iter().map(|m| m.det.tick).min().unwrap_or(t.last_update_tick);
    let mut batch = SourceMask::empty();
    for m in ms {
        batch |= source_of(m.det.kind);
        if m.det.side != side { batch |= SourceMask::DATALINK; }
    }
    t.sources = if first_tick.saturating_sub(t.last_update_tick) > SOURCE_STALE_TICKS { batch } else { t.sources | batch };
    for m in ms { apply_hint(t, m); }
    let ranged: Vec<Meas<'_>> = ms.iter().filter(|m| m.ranged).copied().collect();
    if !ranged.is_empty() { ranged_update(t, &ranged, tuning); }
    for m in ms.iter().filter(|m| !m.ranged) { bearing_update(t, m, tuning); }
    // The hull behind a track follows its positional fixes: a bearing-only intercept that associated on the same
    // line (an ESM bearing of a radar further out behind the contact) refines the bearing but does not make the
    // track that other hull. A briefing-declared Neutral (a defector) otherwise became the pursuing Combine
    // hull's track, turned Hostile on that hull's emitter, and drew the player's own guns.
    // A track with a position fix keeps its hull through batches that hold only bearings.
    let key = |m: &&Meas<'_>| (m.det.tick, m.det.sensor);
    match ms.iter().filter(|m| m.ranged).max_by_key(key) {
        Some(last) => t.truth = last.det.target,
        None if t.bearing_only.is_some() || t.truth.is_none() => { if let Some(last) = ms.iter().max_by_key(key) { t.truth = last.det.target; } }
        None => {}
    }
    if let Some(l) = ms.iter().filter(|m| is_lock_kind(m.det.kind)).map(|m| m.det.tick).max() { t.last_lock_tick = Some(t.last_lock_tick.map_or(l, |x| x.max(l))); }
    // A surface-domain fix moving faster than any hull is airborne or a weapon.
    if t.domain == Domain::Surface && t.bearing_only.is_none() {
        let v = (t.vel.x * t.vel.x + t.vel.z * t.vel.z).sqrt() - SIGMA_95 * t.sigma_v;
        if v > WEAPON_SPEED_MPS { t.domain = Domain::Weapon; } else if v > AIR_SPEED_MPS { t.domain = Domain::Air; }
    }
}

/// Grade, trend and source bookkeeping of a live track at `tick`.
fn refresh_state(t: &mut Track, tick: u64, tuning: &TrackTuning) {
    if !t.last_lock_tick.is_some_and(|l| l <= tick && tick - l <= LOCK_FRESH_TICKS) { t.sources.remove(SourceMask::FIRE_CONTROL | SourceMask::SEEKER); }
    t.grade = t.grade_at(tick, tuning);
    let vy = fin(t.vel.y);
    let thr = TREND_MIN_MPS.max(fin(t.sigma_v));
    t.trend = if t.bearing_only.is_some() { VerticalTrend::Steady } else if vy > thr { VerticalTrend::Climbing } else if vy < -thr { VerticalTrend::Diving } else { VerticalTrend::Steady };
}

/// Fold track `b` into track `a` (a keeps its number): the better kinematic estimate, the higher rung and the
/// union of the evidence.
fn absorb(a: &mut Track, b: &Track, tick: u64, tuning: &TrackTuning) {
    // The older track keeps its filter unless the other is ranged where it is a wedge, or is an established
    // track (more than one update) with less than half its uncertainty.
    let b_established = b.created_tick < b.last_update_tick;
    let b_fresher = b.last_update_tick > a.last_update_tick.saturating_add(GHOST_TICKS);
    let take = (a.bearing_only.is_some() && b.bearing_only.is_none())
        || (b_established && b.bearing_only.is_none() && (b_fresher || b.sigma_m(tick, tuning) < a.sigma_m(tick, tuning)));
    if take {
        a.pos = b.pos;
        a.vel = b.vel;
        a.r0_m = b.r0_m;
        a.sigma_v = b.sigma_v;
        a.bearing_only = b.bearing_only;
        a.last_update_tick = b.last_update_tick;
        a.domain = b.domain;
        a.fit = b.fit;
    } else {
        a.last_update_tick = a.last_update_tick.max(b.last_update_tick);
    }
    if b.last_update_tick > a.last_update_tick || b.last_update_tick == tick { a.truth = b.truth; }
    a.last_lock_tick = match (a.last_lock_tick, b.last_lock_tick) { (Some(x), Some(y)) => Some(x.max(y)), (x, y) => x.or(y) };
    a.created_tick = a.created_tick.min(b.created_tick);
    a.sources |= b.sources;
    if b.rung > a.rung {
        a.rung = b.rung;
        a.rung_reason = b.rung_reason;
        a.candidates = b.candidates.clone();
        a.class = b.class.clone();
        a.history = b.history.clone();
        a.rcs_m2 = b.rcs_m2;
    }
    a.consistent_ticks = a.consistent_ticks.max(b.consistent_ticks);
    for &e in &b.emitters { if let Err(i) = a.emitters.binary_search(&e) { a.emitters.insert(i, e); } }
    if a.id_override.is_none() { a.id_override = b.id_override.clone(); }
    if a.stance_override.is_none() { a.stance_override = b.stance_override; }
    if b.stance == Stance::Hostile && a.stance != Stance::Hostile && a.stance_override.is_none() { a.stance = b.stance; a.stance_reason = b.stance_reason; }
    a.fired_on_us |= b.fired_on_us;
    a.inbound |= b.inbound;
    a.engaged_by = a.engaged_by.max(b.engaged_by);
    a.emitting |= b.emitting;
}

/// Spatial index of a picture's live tracks for association: compact tracks in a uniform grid by predicted
/// position, wide ones (wedges, or a 1-sigma radius over 1 km) in a list every detection checks.
/// `all` holds every indexed track, ascending (tracks are inserted in ascending index order).
struct TrackIndex { cell: f64, grid: BTreeMap<(i32, i32), GridCell>, wide: Vec<usize>, all: Vec<usize>, max_trace: f64, max_speed: f64, max_y: f64 }
/// One grid cell's tracks with the largest covariance trace and horizontal speed among them, so a query can
/// bound its reach per cell instead of by the worst track in the picture.
#[derive(Default)]
struct GridCell { ids: Vec<usize>, max_trace: f64, max_speed: f64 }
/// Trace above which a track is "wide" (two axes of 1 km sigma), and the grid cell, metres.
const WIDE_TRACE_M2: f64 = 2.0e6;
const INDEX_CELL_M: f64 = 4_000.0;
impl TrackIndex {
    fn coord(&self, v: f64) -> i32 { let c = (v / self.cell).floor(); if c.is_finite() { c.clamp(-1.0e9, 1.0e9) as i32 } else { 0 } }
    fn build(tracks: &[Track], tick: u64, tuning: &TrackTuning) -> Self {
        let mut ix = TrackIndex { cell: INDEX_CELL_M, grid: BTreeMap::new(), wide: Vec::new(), all: Vec::new(), max_trace: 0.0, max_speed: 0.0, max_y: 0.0 };
        for (ti, t) in tracks.iter().enumerate() {
            if t.is_live() && !t.is_friend() { ix.insert(ti, t, tick, tuning); }
        }
        ix
    }
    fn insert(&mut self, ti: usize, t: &Track, tick: u64, tuning: &TrackTuning) {
        // Keep `all` ascending even if a caller ever inserts out of order.
        match self.all.last() { Some(&l) if l >= ti => { if let Err(at) = self.all.binary_search(&ti) { self.all.insert(at, ti); } } _ => self.all.push(ti) }
        let tr = t.cov_trace(tick, tuning);
        let p = t.predict(tick);
        self.max_y = self.max_y.max(fin(p.y).abs());
        if t.bearing_only.is_some() || !(tr <= WIDE_TRACE_M2) || !p.is_finite() { self.wide.push(ti); return; }
        let speed = (t.vel.x * t.vel.x + t.vel.z * t.vel.z).sqrt().min(2_000.0);
        self.max_trace = self.max_trace.max(tr);
        self.max_speed = self.max_speed.max(speed);
        let key = (self.coord(p.x), self.coord(p.z));
        let c = self.grid.entry(key).or_default();
        c.ids.push(ti);
        c.max_trace = c.max_trace.max(tr);
        c.max_speed = c.max_speed.max(speed);
    }
    /// Track indices that may gate a detection, ascending.
    fn candidates(&self, m: &Meas<'_>, tick: u64, gate2: f64, out: &mut Vec<usize>) {
        out.clear();
        #[cfg(test)]
        if tests::BRUTE.with(|b| b.get()) { out.extend_from_slice(&self.wide); out.extend(self.grid.values().flat_map(|c| c.ids.iter().copied())); out.sort_unstable(); return; }
        // Every track (wide or gridded) is in `all`, ascending: the answer whenever the whole set is wanted.
        if !m.ranged { out.extend_from_slice(&self.all); return; }
        out.extend_from_slice(&self.wide);
        {
            // The fix lies on its bearing between the horizontal ranges of the lowest and highest targets.
            let o = m.det.sensor_pos;
            let dy = self.max_y + o.y.abs();
            let h_min = (m.range * m.range - dy * dy).max(0.0).sqrt();
            let mid = 0.5 * (h_min + m.range);
            let (cx, cz) = (o.x + mid * m.sin, o.z - mid * m.cos);
            let meas_tr = m.sr * m.sr + sq(m.range * m.sb);
            let age = tick.saturating_sub(m.det.tick) as f64 * TICK_S;
            let r = (gate2 * (self.max_trace + meas_tr)).sqrt() + 0.5 * (m.range - h_min) + self.max_speed * age + 1.0;
            if !(r.is_finite() && cx.is_finite() && cz.is_finite()) { out.clear(); out.extend_from_slice(&self.all); return; }
            else {
                let (x0, x1, z0, z1) = (self.coord(cx - r), self.coord(cx + r), self.coord(cz - r), self.coord(cz + r));
                let span = (x1 as i64 - x0 as i64 + 1) * (z1 as i64 - z0 as i64 + 1);
                // The same reach bound per cell, from the cell's own largest trace and speed: a cell whose
                // rectangle lies farther than that from the fix holds no track the fix can gate.
                let fixed = 0.5 * (m.range - h_min) + 1.0;
                let reach = |key: (i32, i32), c: &GridCell| -> bool {
                    let rc = (gate2 * (c.max_trace + meas_tr)).sqrt() + fixed + c.max_speed * age;
                    let (lx, lz) = (key.0 as f64 * self.cell, key.1 as f64 * self.cell);
                    let ex = (lx - cx).max(cx - (lx + self.cell)).max(0.0);
                    let ez = (lz - cz).max(cz - (lz + self.cell)).max(0.0);
                    !(ex * ex + ez * ez > rc * rc)
                };
                if span > self.grid.len() as i64 {
                    for (&(gx, gz), c) in &self.grid { if gx >= x0 && gx <= x1 && gz >= z0 && gz <= z1 && reach((gx, gz), c) { out.extend_from_slice(&c.ids); } }
                } else {
                    for gx in x0..=x1 { for gz in z0..=z1 { if let Some(c) = self.grid.get(&(gx, gz)) { if reach((gx, gz), c) { out.extend_from_slice(&c.ids); } } } }
                }
            }
        }
        out.sort_unstable();
    }
}

/// Order key of a detection for association: ranged fixes first, then bearings, each by (tick, sensor, target).
fn assoc_key(m: &Meas<'_>) -> (bool, u64, SensorRef, EntityId) { (!m.ranged, m.det.tick, m.det.sensor, m.det.target) }

/// The fusion pass without a `SimCtx`. See [`fuse`].
pub(crate) fn fuse_core(picture: &mut SidePicture, detections: &[Detection], own_side: &[(EntityId, Domain, DVec3, DVec3)], tick: u64, tuning: &TuningTable, changes: &mut Vec<TrackChange>) -> u32 {
    let tt = &tuning.tracks;
    let side = picture.side;
    if picture.next_id == 0 { picture.next_id = 1; }
    let mut work: u32 = 0;
    let mut created: Vec<TrackId> = Vec::new();

    // ---- 1. Datalink friends (L500): one Friend track per own-side unit, exact from the link ----
    let mut own: Vec<(EntityId, Domain, DVec3, DVec3)> = own_side.iter().copied().filter(|o| o.2.is_finite()).collect();
    own.sort_by_key(|o| o.0);
    own.dedup_by_key(|o| o.0);
    let own_ids: Vec<EntityId> = own.iter().map(|o| o.0).collect();
    for &(e, domain, pos, vel) in &own {
        let vel = if vel.is_finite() { vel } else { DVec3::ZERO };
        let idx = picture.tracks.iter().position(|t| t.truth == e && t.is_live());
        let t = match idx {
            Some(i) => &mut picture.tracks[i],
            None => {
                let id = TrackId(picture.next_id);
                picture.next_id = picture.next_id.saturating_add(1);
                picture.tracks.push(blank_track(id, side, e, domain, tick));
                created.push(id);
                changes.push(TrackChange::Created(id));
                picture.tracks.last_mut().expect("just pushed")
            }
        };
        t.domain = domain;
        t.pos = pos;
        t.vel = vel;
        t.r0_m = DATALINK_SIGMA_M;
        t.sigma_v = DATALINK_SIGMA_V;
        t.bearing_only = None;
        t.last_update_tick = tick;
        t.sources = SourceMask::DATALINK;
        if t.stance != Stance::Friend || t.stance_reason != StanceReason::Datalink { t.stance = Stance::Friend; t.stance_reason = StanceReason::Datalink; t.stance_override = None; }
        if t.rung != IdRung::Exact {
            t.rung = IdRung::Exact;
            t.rung_reason = RungReason::Datalink;
            t.history.push(RungStep { rung: IdRung::Exact, reason: RungReason::Datalink, tick });
        }
        work = work.saturating_add(1);
    }
    for t in picture.tracks.iter_mut() {
        if t.is_live() && t.is_friend() && own_ids.binary_search(&t.truth).is_err() {
            t.dropped_tick = Some(tick);
            changes.push(TrackChange::Dropped(t.id));
        }
    }

    // ---- 2. Detections: own-side returns are the datalink's (never tracks), the rest associate ----
    let mut meas: Vec<Meas<'_>> = detections.iter().filter(|d| own_ids.binary_search(&d.target).is_err()).filter_map(Meas::new).collect();
    meas.sort_by_key(assoc_key);
    let gate = if tt.gate_sigma.is_finite() && tt.gate_sigma > 0.0 { tt.gate_sigma } else { 3.03 };
    let gate2 = gate * gate;
    let fresh_gate2 = sq(FRESH_MERGE_SIGMA.max(gate));
    // Assigned detections per track index, and the (sensor, tick) pairs each track took this batch.
    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); picture.tracks.len()];
    let mut taken: Vec<Vec<(SensorRef, u64)>> = vec![Vec::new(); picture.tracks.len()];
    let mut fresh: Vec<bool> = vec![false; picture.tracks.len()];
    // For each track born this batch, the detection that started it, and the established track nearest to that
    // detection beyond the gate (its outlier candidate), with the squared distance.
    let mut born_of: Vec<Option<(usize, Option<(f64, usize)>)>> = vec![None; picture.tracks.len()];
    let mut index = TrackIndex::build(&picture.tracks, tick, tt);
    let mut cand: Vec<usize> = Vec::new();
    for (mi, m) in meas.iter().enumerate() {
        let key = (m.det.sensor, m.det.tick);
        let mut best: Option<(f64, usize)> = None;
        let mut blocked: Option<(f64, usize)> = None;
        let mut near: Option<(f64, usize)> = None;
        index.candidates(m, tick, fresh_gate2, &mut cand);
        for &ti in &cand {
            let t = &picture.tracks[ti];
            if !t.is_live() || t.is_friend() { continue; }
            let Some(d2) = gate_d2(m, t, tt, fresh_gate2) else { continue };
            if d2 > gate2 {
                if d2 <= fresh_gate2 && !fresh[ti] && !taken[ti].contains(&key) && near.is_none_or(|(b, _)| d2 < b) { near = Some((d2, ti)); }
                continue;
            }
            let slot = if taken[ti].contains(&key) { &mut blocked } else { &mut best };
            if slot.is_none_or(|(b, _)| d2 < b) { *slot = Some((d2, ti)); }
        }
        match best {
            Some((_, ti)) => { groups[ti].push(mi); taken[ti].push(key); }
            None => {
                let id = TrackId(picture.next_id);
                picture.next_id = picture.next_id.saturating_add(1);
                picture.tracks.push(new_track(id, side, m, tuning));
                let ti = picture.tracks.len() - 1;
                index.insert(ti, &picture.tracks[ti], tick, tt);
                groups.push(Vec::new());
                taken.push(vec![key]);
                fresh.push(true);
                born_of.push(Some((mi, near)));
                created.push(id);
                changes.push(TrackChange::Created(id));
                if let Some((_, from)) = blocked { changes.push(TrackChange::Split { from: picture.tracks[from].id, new: id }); }
                work = work.saturating_add(1);
            }
        }
    }

    // ---- 3. Filter updates ----
    let mut updated: Vec<bool> = fresh.clone();
    for (ti, g) in groups.iter().enumerate() {
        if g.is_empty() { continue; }
        let mut ms: Vec<Meas<'_>> = g.iter().map(|&i| meas[i]).collect();
        ms.sort_by_key(|m| (m.det.tick, m.det.sensor, m.det.target));
        update_track(&mut picture.tracks[ti], &ms, side, tt);
        updated[ti] = true;
        work = work.saturating_add(ms.len().min(u32::MAX as usize) as u32);
    }

    // ---- 4. Merge duplicates: no object may hold two live tracks (L467) ----
    let n = picture.tracks.len();
    let mut gone = vec![false; n];
    let mut unseen: Vec<TrackId> = Vec::new();
    // Outliers first: a track born of one detection that lay within 4.5 sigma of an established track (and
    // took no further detection) is folded back into it, unseen.
    for j in 0..n {
        let Some((_, Some((_, i)))) = born_of[j] else { continue };
        if !groups[j].is_empty() || gone[i] || !picture.tracks[i].is_live() { continue; }
        if taken[i].iter().any(|k| taken[j].contains(k)) { continue; }
        let b = picture.tracks[j].clone();
        absorb(&mut picture.tracks[i], &b, tick, tt);
        gone[j] = true;
        let absorbed = taken[j].clone();
        taken[i].extend(absorbed);
        changes.retain(|c| !matches!(c, TrackChange::Created(id) | TrackChange::Split { new: id, .. } if *id == b.id));
        created.retain(|&id| id != b.id);
        unseen.push(b.id);
    }
    // Predicted positions and covariance traces once per track (refreshed for a track that absorbs another), so
    // the pair scan below is a few flops per pair before any set or matrix work.
    let mut pred: Vec<DVec3> = picture.tracks.iter().map(|t| t.predict(tick)).collect();
    let mut trace: Vec<f64> = picture.tracks.iter().map(|t| t.cov_trace(tick, tt)).collect();
    for i in 0..n {
        if gone[i] || !picture.tracks[i].is_live() || picture.tracks[i].is_friend() { continue; }
        for j in (i + 1)..n {
            if gone[j] || !(updated[i] || updated[j]) { continue; }
            // A track left coasting beside one that is being updated has lost its object to it.
            let (ua, ub) = (picture.tracks[i].last_update_tick, picture.tracks[j].last_update_tick);
            let ghost = (updated[i] && tick.saturating_sub(ub) > GHOST_TICKS) || (updated[j] && tick.saturating_sub(ua) > GHOST_TICKS);
            let limit2 = if ghost { fresh_gate2 } else { gate2 };
            let (pa, pb) = (pred[i], pred[j]);
            let (dx, dz) = (pa.x - pb.x, pa.z - pb.z);
            // d' S^-1 d <= g^2 needs |d|^2 <= g^2 trace(S): cheap rejection first.
            if !(dx * dx + dz * dz <= limit2 * (trace[i] + trace[j])) { continue; }
            let (a, b) = (&picture.tracks[i], &picture.tracks[j]);
            if !b.is_live() || b.is_friend() || air_like(a.domain) != air_like(b.domain) { continue; }
            if matches!((&a.class, &b.class), (Some(x), Some(y)) if x != y) { continue; }
            // One sensor reporting both at one instant saw two objects.
            if taken[i].iter().any(|k| taken[j].contains(k)) { continue; }
            let s = a.cov_h(tick, tt).add(b.cov_h(tick, tt));
            let Some(d2) = s.inv_quad(dx, dz) else { continue };
            if d2 > limit2 { continue; }
            let b = picture.tracks[j].clone();
            absorb(&mut picture.tracks[i], &b, tick, tt);
            pred[i] = picture.tracks[i].predict(tick);
            trace[i] = picture.tracks[i].cov_trace(tick, tt);
            let into = picture.tracks[i].id;
            gone[j] = true;
            updated[i] = true;
            let absorbed = taken[j].clone();
            taken[i].extend(absorbed);
            if fresh[j] {
                // Never shown: it was created and folded back within this update, so it leaves no event.
                changes.retain(|c| !matches!(c, TrackChange::Created(id) | TrackChange::Split { new: id, .. } if *id == b.id));
                created.retain(|&id| id != b.id);
                unseen.push(b.id);
            } else {
                changes.push(TrackChange::Merged { from: b.id, into });
            }
        }
    }
    if gone.iter().any(|&g| g) {
        let mut k = 0;
        picture.tracks.retain(|_| { let keep = !gone[k]; k += 1; keep });
    }
    // A number that no event or snapshot ever carried was never used: hand back the trailing ones.
    unseen.sort_unstable();
    while let Some(&last) = unseen.last() {
        if last.0.saturating_add(1) != picture.next_id { break; }
        picture.next_id = last.0;
        unseen.pop();
    }

    // ---- 5. Drop stale tracks, expire old last-known marks, refresh grades ----
    let last_known = (pos_f(tt.last_known_s) * sim_core::TICK_HZ as f64) as u64;
    let mut expired: Vec<TrackId> = Vec::new();
    for t in picture.tracks.iter_mut() {
        match t.dropped_tick {
            Some(d) => { if tick.saturating_sub(d) > last_known { expired.push(t.id); } }
            None => {
                if !t.is_friend() && tick.saturating_sub(t.last_update_tick) > drop_ticks(t.domain, tt) {
                    t.dropped_tick = Some(tick);
                    t.fit = None;
                    changes.push(TrackChange::Dropped(t.id));
                    continue;
                }
                let old = t.grade;
                refresh_state(t, tick, tt);
                let is_new = created.contains(&t.id);
                if t.grade != old && !is_new { changes.push(TrackChange::Grade(t.id, t.grade)); }
            }
        }
    }
    if !expired.is_empty() {
        picture.tracks.retain(|t| !expired.contains(&t.id));
        for id in expired { changes.push(TrackChange::Expired(id)); }
    }
    picture.tracks.sort_by_key(|t| t.id);
    work
}

/// [`local_track`] without a `SimCtx`.
pub(crate) fn local_track_core(detections: &[Detection], sensor: SensorRef, previous: Option<&Track>, tick: u64, tuning: &TuningTable) -> Option<Track> {
    let tt = &tuning.tracks;
    let mut mine: Vec<Meas<'_>> = detections.iter().filter(|d| d.sensor == sensor && d.tick <= tick).filter_map(Meas::new).collect();
    mine.sort_by_key(|m| (m.det.tick, m.det.target));
    let gate = if tt.gate_sigma.is_finite() && tt.gate_sigma > 0.0 { tt.gate_sigma } else { 3.03 };
    let gate2 = gate * gate;
    let mut track: Option<Track> = previous.filter(|t| t.is_live()).cloned();
    let mut i = 0;
    while i < mine.len() {
        let tk = mine[i].det.tick;
        let mut j = i;
        while j < mine.len() && mine[j].det.tick == tk { j += 1; }
        let group = &mine[i..j];
        // A track past its drop time gives way to whatever the sensor sees now.
        if track.as_ref().is_some_and(|t| tk.saturating_sub(t.last_update_tick) > drop_ticks(t.domain, tt)) { track = None; }
        match track.as_mut() {
            None => {
                let nearest = group.iter().min_by(|a, b| a.range.partial_cmp(&b.range).unwrap_or(core::cmp::Ordering::Equal));
                if let Some(m) = nearest {
                    let id = previous.map_or(TrackId::NONE, |p| p.id);
                    track = Some(new_track(id, m.det.side, m, tuning));
                }
            }
            Some(t) => {
                let best = group.iter().filter_map(|m| gate_d2(m, t, tt, gate2).filter(|&d| d <= gate2).map(|d| (d, *m))).min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal));
                if let Some((_, m)) = best { let side = t.side; update_track(t, &[m], side, tt); }
            }
        }
        i = j;
    }
    let mut t = track?;
    if tick.saturating_sub(t.last_update_tick) > drop_ticks(t.domain, tt) { return None; }
    refresh_state(&mut t, tick, tt);
    Some(t)
}

#[cfg(test)]
mod probe;
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensors::{measurement_error, Evidence};

    fn eid(i: u32) -> EntityId { EntityId { index: i, generation: 0 } }
    fn tun() -> TuningTable { TuningTable::default() }

    /// Test-local counter RNG (sim_core::rng is another unit's): SplitMix64 and Box-Muller through libm.
    struct Rng(u64);
    impl Rng {
        fn u(&mut self) -> f64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            ((z >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        }
        fn n(&mut self) -> f64 { let (a, b) = (self.u(), self.u()); (-2.0 * libm::log(a)).sqrt() * libm::cos(core::f64::consts::TAU * b) }
    }

    fn sref(c: u32) -> SensorRef { SensorRef { carrier: eid(c), index: 0 } }

    /// A noisy detection of `truth` by a sensor of `kind` at `from`, errors per the L430 table.
    fn det(rng: &mut Rng, tick: u64, sensor: SensorRef, kind: SensorKind, from: DVec3, target: EntityId, truth: DVec3) -> Detection {
        let me = measurement_error(kind);
        let d = truth - from;
        let range = d.length();
        let bearing = compass_bearing(d);
        let sb = me.bearing_deg.to_radians();
        let sr = me.range_m.unwrap_or(0.0) + me.range_fraction.unwrap_or(0.0) * range;
        let ranged = me.range_m.is_some() || me.range_fraction.is_some();
        let measurement = if ranged {
            let h = if matches!(kind, SensorKind::FireControlRadar | SensorKind::Seeker | SensorKind::ActiveSonar) { Some(truth.y + range * sb * rng.n()) } else { None };
            Measurement::RangeBearing { range_m: (range + sr * rng.n()).max(0.0), bearing_rad: crate::signature::wrap_tau(bearing + sb * rng.n()), height_m: h }
        } else {
            Measurement::BearingOnly { bearing_rad: crate::signature::wrap_tau(bearing + sb * rng.n()) }
        };
        Detection {
            tick, sensor, side: SideId(0), kind, sensor_pos: from, target, measurement, sigma_range_m: sr, sigma_bearing_rad: sb,
            evidence: Evidence::empty(), emitter: u16::MAX, rcs_m2: 150.0, domain_hint: None,
        }
    }

    fn picture() -> SidePicture { SidePicture { side: SideId(0), tracks: Vec::new(), next_id: 1 } }

    /// Horizontal distance check against the drawn shape.
    fn inside(t: &Track, truth: DVec3, tick: u64, tt: &TrackTuning) -> bool {
        match t.shape(tick, tt) {
            UncertaintyShape::Circle { radius_m } => {
                let p = t.predict(tick);
                { let (dx, dz) = (truth.x - p.x, truth.z - p.z); (dx * dx + dz * dz).sqrt() <= radius_m as f64 }
            }
            UncertaintyShape::Wedge { apex, bearing_rad, half_angle_rad, near_m, far_m } => {
                let q = DVec3::new(truth.x - apex.x, 0.0, truth.z - apex.y);
                let r = (q.x * q.x + q.z * q.z).sqrt();
                wrap_pi(compass_bearing(q) - bearing_rad as f64).abs() <= half_angle_rad as f64 && r >= near_m as f64 && r <= far_m as f64
            }
            UncertaintyShape::Ellipse { .. } => panic!("no ellipses drawn"),
        }
    }

    #[test]
    fn growth_law_radius_and_prediction() {
        let tt = TrackTuning::default();
        let mut t = blank_track(TrackId(1), SideId(0), eid(1), Domain::Surface, 0);
        t.r0_m = 10.0;
        t.sigma_v = 2.0;
        t.vel = DVec3::new(3.0, 0.0, -4.0);
        // t = 10 s: sqrt(100 + 400 + (0.5 x 0.3 x 100)^2) = sqrt(725).
        assert!((t.sigma_m(600, &tt) - 725f64.sqrt()).abs() < 1e-9);
        assert!((t.radius95_m(600, &tt) - SIGMA_95 * 725f64.sqrt()).abs() < 1e-9);
        assert_eq!(t.predict(600), DVec3::new(30.0, 0.0, -40.0));
        // Missiles grow with sigma_a = 50.
        t.domain = Domain::Weapon;
        assert!((t.sigma_m(60, &tt) - (100.0f64 + 4.0 + 625.0).sqrt()).abs() < 1e-9);
        assert!((beta_for(0.5) - 0.1667).abs() < 1e-3 && (beta_for(0.4) - 0.1).abs() < 1e-12 && (beta_for(0.3) - 0.0529).abs() < 1e-3);
        // Garbage state never panics or goes non-finite.
        t.r0_m = f64::NAN;
        t.sigma_v = f64::INFINITY;
        assert!(t.sigma_m(6_000, &tt).is_finite());
        assert!(t.predict(u64::MAX).is_finite());
    }

    /// Cross-unit (sensors -> tracks): a fire-control radar on a 5 m sea-skimmer closing at 250 m/s, one lock
    /// pulse every 6 ticks, must hold one air-like track. It used to read as a surface contact with an 8 m/s
    /// prior, and every pulse left a new stationary surface ghost behind. A hull stays one surface track on the sea.
    /// A fire-control lock on a 9 m/s hull 3 km off, with a lookout (10% range), a search radar (50 m) and ESM
    /// (2 degrees) reporting the same hull in between lock pulses: the coarse fixes may not throw the lock's
    /// velocity about. With the fixed gains a lookout fix moved it by tens of m/s, and guns led by 100 m.
    #[test]
    fn coarse_fixes_do_not_shake_a_fire_control_lock() {
        let tun = tun();
        let mut rng = Rng(11);
        let mut pic = picture();
        let mut changes = Vec::new();
        let from = DVec3::new(0.0, 15.0, 0.0);
        let vel = DVec3::new(9.0, 0.0, 0.0);
        let mut worst: f64 = 0.0;
        let mut errs = Vec::new();
        for k in 0..600u64 {
            let tick = k * 6;
            let truth = DVec3::new(-1_500.0, 0.0, -3_000.0) + vel * (tick as f64 * TICK_S);
            let mut dets = vec![det(&mut rng, tick, sref(1), SensorKind::FireControlRadar, from, eid(5), truth)];
            if k % 5 == 2 { dets.push(det(&mut rng, tick, sref(2), SensorKind::Visual, from, eid(5), truth)); }
            if k % 25 == 3 { dets.push(det(&mut rng, tick, sref(3), SensorKind::SearchRadar, from, eid(5), truth)); }
            if k % 5 == 4 { dets.push(det(&mut rng, tick, sref(4), SensorKind::Esm, from, eid(5), truth)); }
            fuse_core(&mut pic, &dets, &[], tick, &tun, &mut changes);
            if k >= 100 {
                let t = pic.tracks.iter().find(|t| t.is_live() && t.truth == eid(5)).expect("one track");
                let e = (t.vel - vel).length();
                worst = worst.max(e);
                errs.push(e * e);
            }
        }
        let rms = (errs.iter().sum::<f64>() / errs.len() as f64).sqrt();
        assert_eq!(pic.tracks.iter().filter(|t| t.is_live()).count(), 1);
        // The lock alone runs at about 11 m/s rms (the L462 gains at 10 Hz on 5 m range noise); the coarse fixes
        // took it to 21 m/s with the fixed gains, and add next to nothing now.
        assert!(rms < 14.0, "velocity error rms {rms:.1} m/s, worst {worst:.1}");
    }

    #[test]
    fn fire_control_on_sea_skimmer_holds_one_weapon_track() {
        use crate::sensors::radar::{radar_core, tests::sensor};
        use crate::signature::{tests::sig, SigKind};
        let tun = tun();
        let mut fc = sensor(SensorKind::FireControlRadar);
        fc.r_ref_m = 25_000.0;
        fc.period_ticks = 6;
        let mut m = sig(SigKind::Body(content::ids::MunitionIdx(0)));
        m.domain = Domain::Weapon;
        m.rcs_flat_m2 = 0.1;
        m.top_m = 5.0;
        let mut hull = crate::sensors::radar::tests::hull_at(3_000.0, 500.0, 0.0, 0.0, 10.0);
        hull.entity = eid(9);
        hull.pos.x = 2_000.0;
        let (mut pic, mut changes) = (picture(), Vec::new());
        for k in 0..40u64 {
            let tick = k * 6;
            m.pos = DVec3::new(0.0, 5.0, -(4_000.0 - 250.0 * tick as f64 / sim_core::TICK_HZ as f64));
            let dets: Vec<Detection> = [&m, &hull].into_iter().filter_map(|t| radar_core(&fc, t, tick, 0.0, &tun.sensors)).collect();
            assert_eq!(dets.len(), 2, "both in reach at tick {tick}");
            fuse_core(&mut pic, &dets, &[], tick, &tun, &mut changes);
        }
        let live: Vec<&Track> = pic.tracks.iter().filter(|t| t.is_live()).collect();
        assert_eq!(live.len(), 2, "one track per object: {:?}", live.iter().map(|t| (t.domain, t.truth)).collect::<Vec<_>>());
        let missile = live.iter().find(|t| t.truth == m.entity).expect("missile track");
        assert!(air_like(missile.domain), "{:?}", missile.domain);
        assert!(missile.vel.length() > 150.0, "closing speed learned: {}", missile.vel.length());
        let ship = live.iter().find(|t| t.truth == hull.entity).expect("hull track");
        assert_eq!(ship.domain, Domain::Surface);
        assert_eq!((ship.pos.y, ship.vel.y), (0.0, 0.0), "a surface track sits on the sea");
    }

    #[test]
    fn grades() {
        let tt = TrackTuning::default();
        let mut t = blank_track(TrackId(1), SideId(0), eid(1), Domain::Surface, 1_000);
        t.sigma_v = 0.0;
        t.r0_m = 100.0;
        assert_eq!(t.grade_at(1_000, &tt), TrackGrade::Fine);
        t.r0_m = 300.0;
        assert_eq!(t.grade_at(1_000, &tt), TrackGrade::Coarse);
        t.r0_m = 900.0;
        assert_eq!(t.grade_at(1_000, &tt), TrackGrade::Bearing);
        t.last_lock_tick = Some(1_000);
        assert_eq!(t.grade_at(1_060, &tt), TrackGrade::Lock);
        assert_eq!(t.grade_at(1_061, &tt), TrackGrade::Bearing);
        t.last_lock_tick = None;
        t.r0_m = 10.0;
        t.bearing_only = Some(BearingState { origin: DVec3::ZERO, bearing_rad: 0.0, bearing_rate: 0.0, sigma_bearing_rad: 0.001, range_m: 1_000.0, sigma_range_m: 10.0, sensors: 2 });
        assert_eq!(t.grade_at(1_000, &tt), TrackGrade::Bearing);
        assert!(matches!(t.shape(1_000, &tt), UncertaintyShape::Wedge { .. }));
    }

    /// One straight-running target tracked by one sensor; returns (inside, samples, ids burned, longest
    /// duplicate span in ticks) after a warm-up.
    fn single_sensor_run(seed: u64, kind: SensorKind, range: f64, period: u64, minutes: u64, target_alt: f64) -> (u32, u32, u32, u64) {
        let tun = tun();
        let tt = tun.tracks;
        let mut rng = Rng(seed);
        let mut pic = picture();
        let from = DVec3::new(0.0, 20.0, 0.0);
        let h0 = rng.u() * core::f64::consts::TAU;
        let start = DVec3::new(range * libm::sin(h0), target_alt, -range * libm::cos(h0));
        let course = rng.u() * core::f64::consts::TAU;
        let vel = DVec3::new(8.0 * libm::sin(course), 0.0, -8.0 * libm::cos(course));
        let (mut inn, mut n) = (0u32, 0u32);
        let mut dup_since: Option<u64> = None;
        let mut longest = 0u64;
        let phase = (seed % period) as u64;
        let mut dets = Vec::new();
        for tick in 1..=(minutes * 3_600) {
            let truth = start + vel * (tick as f64 / 60.0);
            if (tick + phase) % period == 0 { dets.push(det(&mut rng, tick, sref(1), kind, from, eid(7), truth)); }
            if tick % 6 == 0 {
                let mut ch = Vec::new();
                fuse_core(&mut pic, &dets, &[], tick, &tun, &mut ch);
                dets.clear();
                let live: Vec<&Track> = pic.tracks.iter().filter(|t| t.is_live()).collect();
                if live.len() > 1 { let s = *dup_since.get_or_insert(tick); longest = longest.max(tick - s); } else { dup_since = None; }
                if tick > 3_600 {
                    for t in &live { n += 1; if inside(t, truth, tick, &tt) { inn += 1; } }
                }
            }
        }
        (inn, n, pic.next_id - 1, longest)
    }

    #[test]
    fn drawn_shape_coverage_by_sensor_family() {
        // L467 gate (1): truth inside the drawn shape in 90% to 98% of samples.
        for (kind, range, period, alt) in [
            (SensorKind::FireControlRadar, 5_000.0, 6, 0.0),
            (SensorKind::FireControlRadar, 20_000.0, 6, 0.0),
            (SensorKind::SearchRadar, 5_000.0, 150, 0.0),
            (SensorKind::SearchRadar, 30_000.0, 150, 0.0),
            (SensorKind::Visual, 3_000.0, 30, 0.0),
            (SensorKind::ActiveSonar, 2_000.0, 30, -20.0),
        ] {
            let (mut inn, mut n) = (0u32, 0u32);
            for seed in 0..12u64 {
                let (a, b, _, _) = single_sensor_run(1_000 + seed, kind, range, period, 4, alt);
                inn += a;
                n += b;
            }
            let f = inn as f64 / n as f64;
            assert!((0.90..=0.98).contains(&f), "{kind:?} at {range} m: {f:.3} of {n}");
        }
    }

    #[test]
    fn no_lasting_duplicates() {
        // L467 association gate: no truth object holds two live tracks for more than 10 s.
        for (kind, range, period) in [(SensorKind::FireControlRadar, 10_000.0, 6), (SensorKind::SearchRadar, 20_000.0, 150)] {
            for seed in 0..8u64 {
                let (_, _, ids, longest) = single_sensor_run(77 + seed, kind, range, period, 10, 0.0);
                assert!(longest <= 600, "{kind:?} seed {seed}: duplicate lived {longest} ticks");
                assert!(ids <= 40, "{kind:?} seed {seed}: {ids} track numbers in 10 minutes");
            }
        }
    }

    #[test]
    fn two_targets_hold_their_tracks() {
        let tun = tun();
        let mut rng = Rng(5);
        let mut pic = picture();
        let from = DVec3::new(0.0, 20.0, 0.0);
        let a0 = DVec3::new(-3_000.0, 0.0, -12_000.0);
        let b0 = DVec3::new(3_000.0, 0.0, -12_000.0);
        let va = DVec3::new(6.0, 0.0, 0.0);
        let vb = DVec3::new(-6.0, 0.0, 1.0);
        let mut owner: BTreeMap<TrackId, EntityId> = BTreeMap::new();
        let mut dets = Vec::new();
        for tick in 1..=(3 * 3_600u64) {
            let (a, b) = (a0 + va * (tick as f64 / 60.0), b0 + vb * (tick as f64 / 60.0));
            if tick % 6 == 0 {
                dets.push(det(&mut rng, tick, sref(1), SensorKind::FireControlRadar, from, eid(10), a));
                dets.push(det(&mut rng, tick, sref(1), SensorKind::FireControlRadar, from, eid(11), b));
                let mut ch = Vec::new();
                fuse_core(&mut pic, &dets, &[], tick, &tun, &mut ch);
                dets.clear();
                for t in pic.tracks.iter().filter(|t| t.is_live()) {
                    let o = *owner.entry(t.id).or_insert(t.truth);
                    assert_eq!(o, t.truth, "track {:?} swapped at {tick}", t.id);
                }
            }
        }
        assert_eq!(pic.tracks.iter().filter(|t| t.is_live()).count(), 2);
        assert!(pic.tracks.iter().all(|t| t.grade == TrackGrade::Lock));
    }

    /// L467 gate (2): one ESM, a 15 kn target at 20 km, one own-ship two-leg manoeuvre over 10 minutes.
    #[test]
    fn bearing_only_single_sensor_keeps_honest_range() {
        let tun = tun();
        let tt = tun.tracks;
        let runs = 30u32;
        let mut inside_runs = 0u32;
        for seed in 0..runs as u64 {
            let mut rng = Rng(9_000 + seed);
            let mut pic = picture();
            let b = rng.u() * core::f64::consts::TAU;
            let t0 = DVec3::new(20_000.0 * libm::sin(b), 0.0, -20_000.0 * libm::cos(b));
            let c = rng.u() * core::f64::consts::TAU;
            let vt = DVec3::new(7.72 * libm::sin(c), 0.0, -7.72 * libm::cos(c));
            let own_leg = |tick: u64| -> DVec3 {
                let s = tick as f64 / 60.0;
                let v1 = DVec3::new(5.14, 0.0, 0.0);
                let v2 = DVec3::new(0.0, 0.0, -5.14);
                if s <= 300.0 { v1 * s } else { v1 * 300.0 + v2 * (s - 300.0) }
            };
            let mut dets = Vec::new();
            let mut ok = true;
            for tick in 1..=36_000u64 {
                let truth = t0 + vt * (tick as f64 / 60.0);
                let own = own_leg(tick) + DVec3::new(0.0, 20.0, 0.0);
                if (tick + seed) % 30 == 0 { dets.push(det(&mut rng, tick, sref(1), SensorKind::Esm, own, eid(9), truth)); }
                if tick % 6 == 0 {
                    let mut ch = Vec::new();
                    fuse_core(&mut pic, &dets, &[], tick, &tun, &mut ch);
                    dets.clear();
                    let live: Vec<&Track> = pic.tracks.iter().filter(|t| t.is_live()).collect();
                    if live.is_empty() && tick <= 36 { continue; }
                    assert_eq!(live.len(), 1, "seed {seed} tick {tick}");
                    let t = live[0];
                    let bs = t.bearing_only.expect("stays bearing-only");
                    assert_eq!(bs.sensors, 1);
                    assert!(bs.sigma_range_m >= 0.1 * bs.range_m - 1e-6, "range sigma floor");
                    if tick % 600 == 0 && !inside(t, truth, tick, &tt) { ok = false; }
                }
            }
            if ok { inside_runs += 1; }
        }
        assert!(inside_runs as f64 >= 0.9 * runs as f64, "{inside_runs} of {runs}");
    }

    #[test]
    fn cross_fix_from_two_platforms() {
        let tun = tun();
        let tt = tun.tracks;
        let mut rng = Rng(31);
        let mut pic = picture();
        let a = DVec3::new(-8_000.0, 20.0, 0.0);
        let b = DVec3::new(8_000.0, 20.0, 0.0);
        let t0 = DVec3::new(1_000.0, 0.0, -20_000.0);
        let vt = DVec3::new(0.0, 0.0, 5.0);
        let mut dets = Vec::new();
        let mut inn = 0;
        let mut n = 0;
        for tick in 1..=(5 * 3_600u64) {
            let truth = t0 + vt * (tick as f64 / 60.0);
            if tick % 30 == 0 { dets.push(det(&mut rng, tick, sref(1), SensorKind::Esm, a, eid(9), truth)); }
            if tick % 30 == 15 { dets.push(det(&mut rng, tick, sref(2), SensorKind::Esm, b, eid(9), truth)); }
            if tick % 6 == 0 {
                let mut ch = Vec::new();
                fuse_core(&mut pic, &dets, &[], tick, &tun, &mut ch);
                dets.clear();
                let live: Vec<&Track> = pic.tracks.iter().filter(|t| t.is_live()).collect();
                if live.is_empty() && tick < 30 { continue; }
                assert_eq!(live.len(), 1, "tick {tick}: {}", live.len());
                if tick > 3_600 { n += 1; if inside(live[0], truth, tick, &tt) { inn += 1; } }
            }
        }
        let t = pic.tracks.iter().find(|t| t.is_live()).unwrap();
        let bs = t.bearing_only.unwrap();
        assert_eq!(bs.sensors, 2);
        assert!(bs.sigma_range_m < 0.1 * bs.range_m, "cross-fix tightens range: {} of {}", bs.sigma_range_m, bs.range_m);
        assert!(inn as f64 >= 0.9 * n as f64, "{inn} of {n}");
    }

    #[test]
    fn esm_bearing_joins_radar_track_and_radar_converts_wedges() {
        let tun = tun();
        let mut rng = Rng(3);
        let mut pic = picture();
        let from = DVec3::new(0.0, 20.0, 0.0);
        let truth = DVec3::new(2_000.0, 0.0, -15_000.0);
        // ESM first: a wedge.
        let mut ch = Vec::new();
        fuse_core(&mut pic, &[det(&mut rng, 6, sref(2), SensorKind::Esm, from, eid(9), truth)], &[], 6, &tun, &mut ch);
        assert_eq!(ch, vec![TrackChange::Created(TrackId(1))]);
        assert!(pic.tracks[0].bearing_only.is_some());
        // Then the search radar: the wedge becomes a ranged track, same number.
        ch.clear();
        fuse_core(&mut pic, &[det(&mut rng, 12, sref(1), SensorKind::SearchRadar, from, eid(9), truth)], &[], 12, &tun, &mut ch);
        assert_eq!(pic.tracks.len(), 1);
        let t = &pic.tracks[0];
        assert!(t.bearing_only.is_none());
        assert!(((t.pos - truth).length()) < 1_000.0);
        // More ESM bearings keep feeding the same track.
        for k in 0..20u64 {
            let tick = 18 + 30 * k;
            ch.clear();
            fuse_core(&mut pic, &[det(&mut rng, tick, sref(2), SensorKind::Esm, from, eid(9), truth)], &[], tick, &tun, &mut ch);
            assert!(!ch.iter().any(|c| matches!(c, TrackChange::Created(_))), "{ch:?}");
        }
        assert_eq!(pic.tracks.iter().filter(|t| t.is_live()).count(), 1);
        assert!(pic.tracks[0].sources.contains(SourceMask::ESM));
    }

    #[test]
    fn friends_by_datalink() {
        let tun = tun();
        let mut rng = Rng(4);
        let mut pic = picture();
        let friend = (eid(3), Domain::Surface, DVec3::new(500.0, 0.0, -4_000.0), DVec3::new(1.0, 0.0, 0.0));
        let from = DVec3::new(0.0, 20.0, 0.0);
        let mut ch = Vec::new();
        let d = det(&mut rng, 6, sref(1), SensorKind::SearchRadar, from, eid(3), friend.2);
        fuse_core(&mut pic, &[d], &[friend], 6, &tun, &mut ch);
        assert_eq!(pic.tracks.len(), 1, "a return from a friend never makes a track");
        let t = &pic.tracks[0];
        assert_eq!((t.stance, t.stance_reason, t.rung, t.rung_reason), (Stance::Friend, StanceReason::Datalink, IdRung::Exact, RungReason::Datalink));
        assert!(t.sources.contains(SourceMask::DATALINK));
        assert_eq!(t.pos, friend.2);
        // Next update moves it; leaving the link drops it.
        let moved = (friend.0, friend.1, DVec3::new(600.0, 0.0, -4_000.0), friend.3);
        fuse_core(&mut pic, &[], &[moved], 12, &tun, &mut ch);
        assert_eq!(pic.tracks[0].pos, moved.2);
        ch.clear();
        fuse_core(&mut pic, &[], &[], 18, &tun, &mut ch);
        assert_eq!(ch, vec![TrackChange::Dropped(TrackId(1))]);
    }

    #[test]
    fn drop_expire_and_numbers_never_reused() {
        let tun = tun();
        let mut rng = Rng(8);
        let mut pic = picture();
        let from = DVec3::new(0.0, 20.0, 0.0);
        let truth = DVec3::new(0.0, 0.0, -10_000.0);
        let mut ch = Vec::new();
        fuse_core(&mut pic, &[det(&mut rng, 6, sref(1), SensorKind::SearchRadar, from, eid(9), truth)], &[], 6, &tun, &mut ch);
        // 120 s without update: dropped (surface). The grade coarsens as it coasts.
        ch.clear();
        fuse_core(&mut pic, &[], &[], 6 + 7_200, &tun, &mut ch);
        assert!(ch.iter().all(|c| matches!(c, TrackChange::Grade(..))), "{ch:?}");
        ch.clear();
        fuse_core(&mut pic, &[], &[], 6 + 7_206, &tun, &mut ch);
        assert_eq!(ch, vec![TrackChange::Dropped(TrackId(1))]);
        assert!(!pic.tracks[0].is_live());
        // A last-known mark stays 10 minutes, then expires.
        ch.clear();
        fuse_core(&mut pic, &[], &[], 7_212 + 36_000, &tun, &mut ch);
        assert!(ch.is_empty());
        fuse_core(&mut pic, &[], &[], 7_218 + 36_000, &tun, &mut ch);
        assert_eq!(ch, vec![TrackChange::Expired(TrackId(1))]);
        assert!(pic.tracks.is_empty());
        // Seen again: a new number.
        ch.clear();
        let tick = 50_000;
        fuse_core(&mut pic, &[det(&mut rng, tick, sref(1), SensorKind::SearchRadar, from, eid(9), truth)], &[], tick, &tun, &mut ch);
        assert_eq!(ch, vec![TrackChange::Created(TrackId(2))]);
        // Air tracks drop after 20 s.
        let mut t = pic.tracks[0].clone();
        t.domain = Domain::Air;
        pic.tracks[0] = t;
        ch.clear();
        fuse_core(&mut pic, &[], &[], tick + 1_206, &tun, &mut ch);
        assert_eq!(ch, vec![TrackChange::Dropped(TrackId(2))]);
    }

    #[test]
    fn close_updates_do_not_fling_velocity() {
        // Two datalinked search radars hit one target 0.2 s apart, every rotation.
        let tun = tun();
        let mut rng = Rng(12);
        let mut pic = picture();
        let a = DVec3::new(0.0, 20.0, 0.0);
        let b = DVec3::new(2_000.0, 20.0, 1_000.0);
        let t0 = DVec3::new(0.0, 0.0, -25_000.0);
        let v = DVec3::new(7.0, 0.0, 0.0);
        let mut dets = Vec::new();
        let (mut se, mut n) = (0.0, 0.0);
        for tick in 1..=(6 * 3_600u64) {
            let truth = t0 + v * (tick as f64 / 60.0);
            if tick % 150 == 0 { dets.push(det(&mut rng, tick, sref(1), SensorKind::SearchRadar, a, eid(9), truth)); }
            if tick % 150 == 12 { dets.push(det(&mut rng, tick, sref(2), SensorKind::SearchRadar, b, eid(9), truth)); }
            if tick % 6 == 0 {
                let mut ch = Vec::new();
                fuse_core(&mut pic, &dets, &[], tick, &tun, &mut ch);
                dets.clear();
                if tick > 3_600 { for t in pic.tracks.iter().filter(|t| t.is_live()) { se += (t.vel - v).length_squared(); n += 1.0; } }
            }
        }
        // Without the dt floor the 0.2 s gap gives a velocity gain of beta / 0.2 s, five times the design's, and the
        // estimate rides the 30 m/s clamp. With it the error is the spec gains' own noise at 25 km.
        let rms = (se / n).sqrt();
        assert!(rms < 15.0, "velocity error rms {rms} m/s");
        assert_eq!(pic.tracks.iter().filter(|t| t.is_live()).count(), 1);
    }

    #[test]
    fn local_control_track() {
        let tun = tun();
        let mut rng = Rng(2);
        let from = DVec3::new(0.0, 15.0, 0.0);
        let near = DVec3::new(0.0, 0.0, -3_000.0);
        let far = DVec3::new(500.0, 0.0, -6_000.0);
        let s = sref(1);
        let mut prev: Option<Track> = None;
        for k in 1..=30u64 {
            let tick = 6 * k;
            let mut ds = vec![det(&mut rng, tick, s, SensorKind::FireControlRadar, from, eid(20), far), det(&mut rng, tick, s, SensorKind::FireControlRadar, from, eid(21), near)];
            // Another sensor's returns are ignored.
            ds.push(det(&mut rng, tick, sref(2), SensorKind::FireControlRadar, from, eid(22), DVec3::new(0.0, 0.0, -1_000.0)));
            let t = local_track_core(&ds, s, prev.as_ref(), tick, &tun).expect("a track");
            assert_eq!(t.truth, eid(21), "locks the nearest return and holds it");
            assert_eq!(t.grade, TrackGrade::Lock);
            prev = Some(t);
        }
        // Nothing seen for longer than the drop time: gone.
        assert!(local_track_core(&[], s, prev.as_ref(), 180 + 7_300, &tun).is_none());
        assert!(local_track_core(&[], s, None, 10, &tun).is_none());
    }

    #[test]
    fn bad_input_is_ignored() {
        let tun = tun();
        let mut rng = Rng(1);
        let mut pic = SidePicture { side: SideId(0), tracks: Vec::new(), next_id: 0 };
        let from = DVec3::new(0.0, 20.0, 0.0);
        let mut bad = det(&mut rng, 6, sref(1), SensorKind::SearchRadar, from, eid(9), DVec3::new(0.0, 0.0, -5_000.0));
        bad.measurement = Measurement::RangeBearing { range_m: f64::NAN, bearing_rad: 0.0, height_m: None };
        let mut bad2 = bad;
        bad2.sensor_pos = DVec3::new(f64::INFINITY, 0.0, 0.0);
        let mut bad3 = bad;
        bad3.measurement = Measurement::BearingOnly { bearing_rad: f64::NAN };
        let mut ch = Vec::new();
        fuse_core(&mut pic, &[bad, bad2, bad3], &[(eid(1), Domain::Surface, DVec3::NAN, DVec3::ZERO)], 6, &tun, &mut ch);
        assert!(pic.tracks.is_empty() && ch.is_empty());
        // Ids start at 1 even from a zeroed picture.
        let ok = det(&mut rng, 12, sref(1), SensorKind::SearchRadar, from, eid(9), DVec3::new(0.0, 0.0, -5_000.0));
        fuse_core(&mut pic, &[ok], &[], 12, &tun, &mut ch);
        assert_eq!(pic.tracks[0].id, TrackId(1));
    }

    #[test]
    fn outlier_is_folded_back_unseen_and_resolution_splits() {
        let tun = tun();
        let tt = tun.tracks;
        let mut rng = Rng(21);
        let mut pic = picture();
        let from = DVec3::new(0.0, 20.0, 0.0);
        let truth = DVec3::new(0.0, 0.0, -8_000.0);
        let mut ch = Vec::new();
        for k in 1..=50u64 { fuse_core(&mut pic, &[det(&mut rng, 6 * k, sref(1), SensorKind::FireControlRadar, from, eid(9), truth)], &[], 6 * k, &tun, &mut ch); }
        assert_eq!(pic.tracks.len(), 1);
        let next = pic.next_id;
        // A detection 3.6 sigma off (outside the 3.03 gate, inside 4.5): no new track is ever shown.
        let t = &pic.tracks[0];
        let tick = 306;
        let p = t.predict(tick);
        let probe = det(&mut rng, tick, sref(1), SensorKind::FireControlRadar, from, eid(9), p);
        let m = Meas::new(&probe).unwrap();
        let s = (t.cov_h(tick, &tt).add(m.point(p.y).1)).xx.sqrt();
        let mut off = det(&mut Rng(0), tick, sref(1), SensorKind::FireControlRadar, from, eid(9), p + DVec3::new(3.6 * s, 0.0, 0.0));
        off.measurement = match off.measurement { Measurement::RangeBearing { height_m, .. } => {
            let d = p + DVec3::new(3.6 * s, 0.0, 0.0) - from;
            Measurement::RangeBearing { range_m: d.length(), bearing_rad: compass_bearing(d), height_m }
        } other => other };
        ch.clear();
        fuse_core(&mut pic, &[off], &[], tick, &tun, &mut ch);
        assert!(ch.iter().all(|c| matches!(c, TrackChange::Grade(..))), "{ch:?}");
        assert_eq!(pic.tracks.len(), 1);
        assert_eq!(pic.next_id, next, "the outlier never showed, so its number is handed back");
        // Two returns from one sensor at one instant near one track: the second object splits off.
        let near = pic.tracks[0].predict(312);
        ch.clear();
        let exact = |d: &mut Detection, at: DVec3| {
            let v = at - from;
            d.measurement = Measurement::RangeBearing { range_m: v.length(), bearing_rad: compass_bearing(v), height_m: Some(at.y) };
        };
        let mut a = det(&mut Rng(1), 312, sref(1), SensorKind::FireControlRadar, from, eid(9), near);
        exact(&mut a, near);
        // Inside the gate, but that sensor's instant already went to the nearer return.
        let mut b = det(&mut Rng(2), 312, sref(1), SensorKind::FireControlRadar, from, eid(10), near);
        exact(&mut b, near + DVec3::new(2.0 * s, 0.0, 0.0));
        fuse_core(&mut pic, &[a, b], &[], 312, &tun, &mut ch);
        assert!(ch.contains(&TrackChange::Created(TrackId(next))), "{ch:?}");
        assert!(ch.contains(&TrackChange::Split { from: TrackId(1), new: TrackId(next) }), "{ch:?}");
        assert_eq!(pic.tracks.iter().filter(|t| t.is_live()).count(), 2);
    }

    thread_local! { pub(super) static BRUTE: core::cell::Cell<bool> = const { core::cell::Cell::new(false) }; }

    #[test]
    fn track_index_matches_brute_force() {
        let run = |brute: bool| -> (SidePicture, Vec<TrackChange>) {
            BRUTE.with(|b| b.set(brute));
            let tun = tun();
            let mut rng = Rng(66);
            let mut pic = picture();
            let mut all = Vec::new();
            let targets: Vec<(DVec3, DVec3, f64)> = (0..120).map(|_| {
                let p = DVec3::new((rng.u() - 0.5) * 60_000.0, 0.0, (rng.u() - 0.5) * 60_000.0);
                let c = rng.u() * core::f64::consts::TAU;
                let alt = if rng.u() < 0.2 { 3_000.0 } else { 0.0 };
                (p, DVec3::new(9.0 * libm::sin(c), 0.0, -9.0 * libm::cos(c)), alt)
            }).collect();
            let sensors = [DVec3::new(0.0, 20.0, 0.0), DVec3::new(15_000.0, 20.0, 5_000.0), DVec3::new(-10_000.0, 20.0, -20_000.0)];
            let mut dets = Vec::new();
            for tick in 1..=1_800u64 {
                for (k, (p0, v, alt)) in targets.iter().enumerate() {
                    let truth = *p0 + *v * (tick as f64 / 60.0) + DVec3::new(0.0, *alt, 0.0);
                    for (si, o) in sensors.iter().enumerate() {
                        if (tick + 7 * k as u64 + 50 * si as u64) % 150 == 0 { dets.push(det(&mut rng, tick, sref(si as u32 + 1), SensorKind::SearchRadar, *o, eid(100 + k as u32), truth)); }
                        if si == 0 && k % 10 == 0 && tick % 6 == 0 { dets.push(det(&mut rng, tick, sref(9), SensorKind::FireControlRadar, *o, eid(100 + k as u32), truth)); }
                        if si == 1 && k % 7 == 0 && (tick + k as u64) % 30 == 0 { dets.push(det(&mut rng, tick, sref(8), SensorKind::Esm, *o, eid(100 + k as u32), truth)); }
                    }
                }
                if tick % 6 == 0 {
                    dets.sort_by(|a, b| (a.sensor, a.target).cmp(&(b.sensor, b.target)));
                    fuse_core(&mut pic, &dets, &[], tick, &tun, &mut all);
                    dets.clear();
                }
            }
            BRUTE.with(|b| b.set(false));
            (pic, all)
        };
        let (a, ca) = run(true);
        let (b, cb) = run(false);
        assert!(a.tracks.iter().filter(|t| t.is_live()).count() >= 100);
        assert_eq!(ca, cb);
        assert_eq!(a, b);
    }
}
