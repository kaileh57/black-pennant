//! Unit `sys-guidance` (hard: proportional navigation, seeker logic and decoy seduction, gated at L526).
//! The `PnGuidance` stage for every guided body. DESIGN.md L520-527, L599-604.
//!
//! - Proportional navigation: N = 3 anti-ship (and torpedoes), N = 4 SAM and point-defence missiles; tunable
//!   at L1 and L3.
//! - Midcourse: 1 Hz datalink updates while the launcher or any own-side unit sharing the track holds it; no
//!   horizon limit on the datalink. When updates stop the missile flies to the last predicted position.
//! - Terminal: the seeker turns on when range to the predicted position is under its detection range against
//!   the track's estimated RCS plus the track's 95% radius. An active radar seeker is then an emitter (ESM
//!   hears it at 2.0 x 12 km, horizon-capped). It acquires a return inside its field of view (+/-30 degrees)
//!   and its own detection range (radar law, clutter, jamming). It rejects own-side returns by datalink, not
//!   Neutral ones.
//! - Decoys: when a decoy enters the seeker's resolution cell (inside the field of view and within 300 m of
//!   the locked return), one draw from `rng::Stream::DecoySeduction` keyed (missile id, decoy id) moves the
//!   lock with P = sigma_decoy / (sigma_decoy + sigma_target); the result is kept. Chaff 1,000 m^2 at bloom
//!   falling to 0 over 60 s; flares against infrared seekers (hull IR = propulsion MW, flare 20 MW for 5 s);
//!   towed decoys against torpedo seekers N = 4.0, against radar seekers 2x ship RCS.
//! - Torpedoes run straight to the predicted position (1 Hz wire updates on wire-guided designs), then home
//!   with an active acoustic seeker under the active sonar law, N = 3. Depth charges and mines are unguided.
//!
//! # Rules this module settles where the spec is silent
//! - Navigation constant and field of view: the effective value is the larger of the munition's data value
//!   and the tuning table's class value (`GuidanceTuning::pn_antiship` for anti-ship missiles and torpedoes,
//!   `pn_sam` for SAM and point-defence missiles, `seeker_fov_half_deg` for every seeker), so a gate fallback
//!   committed to `tuning.ron` always takes effect. An N set at L1 or L3 at launch replaces it (1..=10).
//! - "Holds the track" means the track is live in the side picture (which carries every own-side and allied
//!   unit's detections). At an update with no live track the prediction is frozen where it stands and the
//!   body flies to that point.
//! - Seeker boresight is the body's velocity. Acquisition takes the in-view, in-range return nearest the
//!   predicted position (bearing-only launches: nearest the boresight); ties go to the lower entity id. Anti-ship
//!   seekers see surface hulls and wrecks, torpedo seekers surface and submerged hulls, SAM and point-defence
//!   seekers air bodies and weapons. Decoys are never acquired directly, only by seduction.
//! - A return is own-side when it belongs to the missile's side or is a live Friend track in its picture
//!   (allies arrive as Friend tracks over the datalink).
//! - A chaff cloud enters the resolution cell only once it has bloomed (age at least `chaff_bloom_s`): a cloud
//!   still blooming is not yet a resolvable return, so its roll is taken at full bloom strength. A decoy that
//!   is not yet in the cell is not marked tested.
//! - A seduced seeker never re-acquires a hull: when its decoy leaves the field of view or fades, the phase
//!   becomes `Lost` and the body flies straight on. A seeker that loses a real lock searches again.
//! - An infrared seeker's range is `seeker_r_ref_m * sqrt(IR MW / 1 MW)` capped by the horizon and blocked by
//!   smoke; its turn-on range is its reference range plus the track's 95% radius.
//! - Terminal homing on a surface hull aims at half the hull's height above the sea so a sea-skimmer does not
//!   fly into the water short of the target. Torpedo homing steers only in the horizontal plane and keeps its
//!   running depth.
//! - Proximity-fused bodies request `Steer::Detonate` only inside the fuse radius of the locked return;
//!   sim-core's own fuse covers the swept case.
use crate::prelude::*;
use crate::protocol::snapshot::SeekerPhase;
use crate::signature::{SigKind, Signature, SignatureTable};
use crate::tracks::Pictures;
use content::part::weapons::{Fuse, MunitionDef, MunitionKind, SeekerKind};
use content::part::DecoyKind;
use sim_core::munitions::{BodyPhase, Steer};
use sim_core::rng::{self, Stream};
use sim_core::time::DT;

/// Guidance memory of one guided body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Guided {
    pub body: EntityId,
    pub side: SideId,
    pub launcher: EntityId,
    pub kind: MunitionIdx,
    /// Track it was launched at (in its side's picture); `TrackId::NONE` for bearing-only launches.
    pub track: TrackId,
    pub phase: SeekerPhase,
    /// Entity the seeker is locked on (truth; hull, body or decoy).
    pub lock: Option<EntityId>,
    /// Last midcourse prediction and when it was refreshed.
    pub predicted: DVec3,
    pub predicted_vel: DVec3,
    pub last_update_tick: u64,
    pub pn_n: f64,
    /// Decoys already rolled against (each decoy is tested once per missile; the result is kept).
    pub tested_decoys: Vec<EntityId>,
    /// Previous line-of-sight unit vector for the PN rate estimate.
    pub last_los: DVec3,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GuidanceTable {
    /// Ascending body entity.
    pub guided: Vec<Guided>,
}

/// What guidance decided for one body this step; the Sim applies it with `World::steer_body` and
/// `World::set_body_radiating`, and turns phase changes into `SimEvent::Seeker` / `Seduced`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GuidanceOut { pub body: EntityId, pub steer: Steer, pub radiating: bool, pub phase: SeekerPhase, pub lock: Option<EntityId>, pub seduced_by: Option<EntityId> }

impl GuidanceTable {
    /// Register a newly spawned guided body (from `PhysEvent::BodySpawned`) with its launch track.
    /// Unit: `sys-guidance`.
    ///
    /// Kinds that are not guided (mines, depth charges, drones, aircraft, helicopters, RHIBs) and unknown
    /// munition indices are ignored. Registering a body twice replaces its record. A launch whose track is
    /// not live in the side picture starts in `SeekerPhase::Off` and is set up on its first step.
    #[allow(clippy::too_many_arguments)]
    pub fn launch(&mut self, body: EntityId, side: SideId, launcher: EntityId, kind: MunitionIdx, track: TrackId, pn_n: Option<f64>, pictures: &Pictures, ctx: &SimCtx<'_>) {
        let Some((_, def)) = ctx.catalogue.munitions.get_index(kind.index()) else { return };
        if !is_guided(def.kind) { return; }
        let tick = ctx.tick.0;
        let held = if track.is_none() { None } else { pictures.side(side).and_then(|p| p.get(track)).filter(|t| t.is_live()) };
        let (predicted, predicted_vel, phase) = match held { Some(t) => (t.predict(tick), t.vel, SeekerPhase::Midcourse), None => (DVec3::ZERO, DVec3::ZERO, SeekerPhase::Off) };
        let n = match pn_n { Some(n) if n.is_finite() && n > 0.0 => n.clamp(1.0, 10.0), _ => class_n(def, ctx.tuning) };
        let g = Guided { body, side, launcher, kind, track, phase, lock: None, predicted, predicted_vel, last_update_tick: tick, pn_n: n, tested_decoys: Vec::new(), last_los: DVec3::ZERO };
        match self.guided.binary_search_by(|x| x.body.cmp(&body)) { Ok(i) => self.guided[i] = g, Err(i) => self.guided.insert(i, g) }
    }
    pub fn remove(&mut self, body: EntityId) {
        if let Ok(i) = self.guided.binary_search_by(|x| x.body.cmp(&body)) { self.guided.remove(i); }
    }
    /// One 30 Hz guidance step for every guided body, ascending entity: midcourse refresh at 1 Hz from the side
    /// picture, seeker turn-on, acquisition, decoy rolls, PN steering, torpedo run-out and homing, proximity
    /// detonation requests. Returns work units (bodies stepped). Unit: `sys-guidance`.
    ///
    /// Bodies missing from the world or already dead produce no output (the Sim removes them on their
    /// outcome event).
    pub fn step(&mut self, world: &World, pictures: &Pictures, sigs: &SignatureTable, ctx: &SimCtx<'_>, out: &mut Vec<GuidanceOut>) -> u32 {
        if self.guided.is_empty() { return 0; }
        let env = WorldEnv { world, pictures, sigs, ctx, clutter: crate::sensors::clutter_for(ctx.weather, &ctx.tuning.sensors) };
        let bodies = world.bodies();
        let mut work = 0;
        for g in &mut self.guided {
            let Some(row) = bodies.row_of(g.body) else { continue };
            if bodies.phase.get(row) == Some(&BodyPhase::Dead) { continue; }
            let (Some(&pos), Some(&vel)) = (bodies.pos.get(row), bodies.vel.get(row)) else { continue };
            let Some((_, def)) = ctx.catalogue.munitions.get_index(g.kind.index()) else { continue };
            work += 1;
            out.push(step_body(g, def, pos, vel, &env));
        }
        work
    }
}

/// Proportional-navigation lateral acceleration demand in world m/s^2: a = N * Vc * (LOS rate), perpendicular
/// to the line of sight. Pure function shared with the graph node `PnGuidance`. Unit: `sys-guidance`.
///
/// In vector form a = N Vc (omega x r_hat) with omega = r x v_rel / |r|^2, which is N Vc v_perp / |r| where
/// v_perp is the relative velocity across the line of sight. Zero when opening (Vc <= 0), at zero range or on
/// non-finite input.
pub fn pn_accel(pos: DVec3, vel: DVec3, target_pos: DVec3, target_vel: DVec3, n: f64) -> DVec3 {
    let r = target_pos - pos;
    let r2 = r.length_squared();
    if !r2.is_finite() || r2 < 1e-6 || !vel.is_finite() || !target_vel.is_finite() || !n.is_finite() { return DVec3::ZERO; }
    let dist = libm::sqrt(r2);
    let r_hat = r / dist;
    let v_rel = target_vel - vel;
    let vc = -r_hat.dot(v_rel);
    if vc <= 0.0 { return DVec3::ZERO; }
    let v_perp = v_rel - r_hat * r_hat.dot(v_rel);
    v_perp * (n * vc / dist)
}

/// Seduction probability sigma_decoy / (sigma_decoy + sigma_target) (L524). Real code: spec formula.
pub fn seduction_probability(decoy_strength: f64, target_strength: f64) -> f64 { let d = decoy_strength.max(0.0); let t = target_strength.max(0.0); if d + t <= 0.0 { 0.0 } else { d / (d + t) } }

/// Range at which the seeker turns on: its detection range against the track's estimated RCS plus the track's
/// 95% radius (L523). Unit: `sys-guidance`.
///
/// Detection range is the radar law R = R_ref (sigma / sigma_ref)^(1/4) (`SensorTuning::sigma_ref_m2`).
pub fn seeker_turn_on_range_m(seeker_r_ref_m: f64, track_rcs_m2: f64, track_radius95_m: f64, tuning: &TuningTable) -> f64 {
    let fin = |v: f64| if v.is_finite() { v.max(0.0) } else { 0.0 };
    let sigma_ref = if tuning.sensors.sigma_ref_m2.is_finite() && tuning.sensors.sigma_ref_m2 > 0.0 { tuning.sensors.sigma_ref_m2 } else { 100.0 };
    fin(seeker_r_ref_m) * libm::pow(fin(track_rcs_m2) / sigma_ref, 0.25) + fin(track_radius95_m)
}

// ---- rules ----

/// Reference range when a munition's data carries none: 12 km for missile seekers (L432), the active sonar
/// reference for torpedo seekers.
const MISSILE_SEEKER_R_REF_M: f64 = 12_000.0;
/// Reference infrared power of the seeker law, MW.
const IR_REF_MW: f64 = 1.0;
/// Lowest terminal aim height on a surface hull, metres above the sea.
const MIN_HULL_AIM_M: f64 = 1.0;

fn is_guided(kind: MunitionKind) -> bool { matches!(kind, MunitionKind::AntiShipMissile | MunitionKind::Sam | MunitionKind::PdMissile | MunitionKind::Torpedo) }

fn class_n(def: &MunitionDef, tuning: &TuningTable) -> f64 {
    let data = if def.pn_n.is_finite() { def.pn_n.max(0.0) } else { 0.0 };
    let class = match def.kind {
        MunitionKind::AntiShipMissile | MunitionKind::Torpedo => tuning.guidance.pn_antiship,
        MunitionKind::Sam | MunitionKind::PdMissile => tuning.guidance.pn_sam,
        _ => 0.0,
    };
    let class = if class.is_finite() { class.max(0.0) } else { 0.0 };
    let n = data.max(class);
    if n > 0.0 { n.clamp(1.0, 10.0) } else { 3.0 }
}

fn fov_cos(def: &MunitionDef, tuning: &TuningTable) -> f64 {
    let fin = |v: f64| if v.is_finite() { v.clamp(0.0, 180.0) } else { 0.0 };
    let deg = fin(def.seeker_fov_half_deg).max(fin(tuning.guidance.seeker_fov_half_deg));
    let deg = if deg > 0.0 { deg } else { 30.0 };
    libm::cos(deg.to_radians())
}

fn seeker_r_ref(def: &MunitionDef, tuning: &TuningTable) -> f64 {
    if def.seeker_r_ref_m.is_finite() && def.seeker_r_ref_m > 0.0 { return def.seeker_r_ref_m; }
    match def.seeker { SeekerKind::ActiveAcoustic => tuning.sensors.active_sonar_r_ref_m.max(0.0), _ => MISSILE_SEEKER_R_REF_M }
}

fn midcourse_period_ticks(hz: f64) -> u64 {
    let hz = if hz.is_finite() && hz > 0.0 { hz } else { 1.0 };
    ((sim_core::time::TICK_HZ as f64 / hz) as u64).max(1)
}

/// Returns this seeker type can see (never decoys; those only by seduction).
fn sees_kind(def: &MunitionDef, s: &Signature) -> bool {
    match def.kind {
        MunitionKind::AntiShipMissile => matches!(s.kind, SigKind::Vessel | SigKind::Wreck) && s.domain == Domain::Surface,
        MunitionKind::Torpedo => matches!(s.kind, SigKind::Vessel | SigKind::Wreck) && matches!(s.domain, Domain::Surface | Domain::Subsurface),
        MunitionKind::Sam | MunitionKind::PdMissile => matches!(s.kind, SigKind::Body(_)) && matches!(s.domain, Domain::Air | Domain::Weapon),
        _ => false,
    }
}

/// Decoy kinds that compete for this seeker.
fn decoy_competes(seeker: SeekerKind, kind: DecoyKind) -> bool {
    match seeker {
        SeekerKind::ActiveRadar => matches!(kind, DecoyKind::Chaff | DecoyKind::TowedDecoy),
        SeekerKind::Infrared => kind == DecoyKind::Flare,
        SeekerKind::ActiveAcoustic => kind == DecoyKind::TowedDecoy,
        SeekerKind::None => false,
    }
}

// ---- environment seam (world, pictures, signatures, sensor laws, RNG) ----

#[derive(Clone, Copy, Debug)]
struct TrackEst { pos: DVec3, vel: DVec3, rcs_m2: f64, r95_m: f64 }

trait SeekerEnv {
    fn tick(&self) -> u64;
    fn tuning(&self) -> &TuningTable;
    /// The side's live track now (predicted to this tick), or `None`.
    fn track(&self, side: SideId, track: TrackId) -> Option<TrackEst>;
    fn signatures(&self) -> &[Signature];
    fn signature(&self, id: EntityId) -> Option<&Signature>;
    /// Position (vessel centre of mass) and velocity of an entity.
    fn state(&self, id: EntityId) -> Option<(DVec3, DVec3)>;
    /// The return is own-side for a seeker launched by `side`.
    fn own_side(&self, side: SideId, s: &Signature) -> bool;
    fn rcs_toward(&self, s: &Signature, from: DVec3) -> f64;
    /// Radar law range for this seeker against `s` at cross-section `sigma` (horizon, clutter, jamming).
    fn radar_range(&self, r_ref: f64, from: DVec3, s: &Signature, sigma: f64) -> f64;
    /// Active sonar law range for this seeker against `s` (submerged area, layer).
    fn sonar_range(&self, r_ref: f64, from: DVec3, s: &Signature) -> f64;
    fn clear(&self, a: DVec3, b: DVec3, optical: bool, ignore: [EntityId; 2]) -> bool;
    fn decoy_age(&self, id: EntityId) -> Option<f64>;
    /// The keyed seduction draw.
    fn seduce(&self, missile: EntityId, decoy: EntityId, p: f64) -> bool;
}

struct WorldEnv<'a> { world: &'a World, pictures: &'a Pictures, sigs: &'a SignatureTable, ctx: &'a SimCtx<'a>, clutter: f64 }

impl SeekerEnv for WorldEnv<'_> {
    fn tick(&self) -> u64 { self.ctx.tick.0 }
    fn tuning(&self) -> &TuningTable { self.ctx.tuning }
    fn track(&self, side: SideId, track: TrackId) -> Option<TrackEst> {
        if track.is_none() { return None; }
        let t = self.pictures.side(side)?.get(track).filter(|t| t.is_live())?;
        let tick = self.ctx.tick.0;
        Some(TrackEst { pos: t.predict(tick), vel: t.vel, rcs_m2: t.rcs_m2 as f64, r95_m: t.radius95_m(tick, &self.ctx.tuning.tracks) })
    }
    fn signatures(&self) -> &[Signature] { &self.sigs.rows }
    fn signature(&self, id: EntityId) -> Option<&Signature> { self.sigs.get(id) }
    fn state(&self, id: EntityId) -> Option<(DVec3, DVec3)> { self.world.locate(id).map(|(p, v, _)| (p, v)) }
    fn own_side(&self, side: SideId, s: &Signature) -> bool {
        s.side == side || self.pictures.side(side).and_then(|p| p.track_of(s.entity)).is_some_and(|t| t.stance == Stance::Friend)
    }
    fn rcs_toward(&self, s: &Signature, from: DVec3) -> f64 { s.rcs_toward(from) }
    fn radar_range(&self, r_ref: f64, from: DVec3, s: &Signature, sigma: f64) -> f64 {
        crate::sensors::radar::radar_ranges(r_ref, from.y.max(0.0), sigma, s.top_m, self.clutter, s.jammer_r_bt0_m, &self.ctx.tuning.sensors).final_m
    }
    fn sonar_range(&self, r_ref: f64, from: DVec3, s: &Signature) -> f64 {
        use crate::sensors::sonar;
        let range = (s.pos - from).length();
        let water = self.world.terrain().depth_at(from.x, from.z);
        sonar::active_range_m(r_ref, s.submerged_area_toward(from)) * sonar::layer_factor((-from.y).max(0.0), s.depth_m, range, water, self.ctx.layer_depth_m, &self.ctx.tuning.sensors)
    }
    fn clear(&self, a: DVec3, b: DVec3, optical: bool, ignore: [EntityId; 2]) -> bool { self.world.line_of_sight(a, b, optical, ignore) }
    fn decoy_age(&self, id: EntityId) -> Option<f64> {
        let d = self.world.decoys();
        d.entity.iter().position(|e| *e == id).and_then(|i| d.age_s.get(i).copied())
    }
    fn seduce(&self, missile: EntityId, decoy: EntityId, p: f64) -> bool { rng::chance(self.ctx.key(Stream::DecoySeduction, missile.to_u64(), decoy.to_u64()), p) }
}

// ---- per-body logic ----

/// What the seeker measures of a return: its strength for seduction and whether it is detectable now.
fn strength<E: SeekerEnv>(def: &MunitionDef, s: &Signature, from: DVec3, env: &E) -> f64 {
    let v = match def.seeker {
        SeekerKind::ActiveRadar => env.rcs_toward(s, from),
        SeekerKind::Infrared => s.ir_mw,
        SeekerKind::ActiveAcoustic => if matches!(s.kind, SigKind::Decoy(DecoyKind::TowedDecoy)) && s.noise <= 0.0 { env.tuning().decoys.towed_noise } else { s.noise },
        SeekerKind::None => 0.0,
    };
    if v.is_finite() { v.max(0.0) } else { 0.0 }
}

fn in_view(pos: DVec3, vel: DVec3, target: DVec3, cos_fov: f64) -> bool {
    let d = target - pos;
    let (dl, vl) = (d.length(), vel.length());
    if dl < 1e-6 { return true; }
    if vl < 1e-9 { return false; }
    d.dot(vel) >= cos_fov * dl * vl
}

/// Detectable by this seeker now: in view, in range under its law, line of sight clear.
fn detectable<E: SeekerEnv>(g: &Guided, def: &MunitionDef, pos: DVec3, vel: DVec3, s: &Signature, cos_fov: f64, env: &E) -> bool {
    if !in_view(pos, vel, s.pos, cos_fov) { return false; }
    let dist = (s.pos - pos).length();
    let r_ref = seeker_r_ref(def, env.tuning());
    let (range, optical) = match def.seeker {
        SeekerKind::ActiveRadar => (env.radar_range(r_ref, pos, s, strength(def, s, pos, env)), false),
        SeekerKind::Infrared => {
            let r = r_ref * libm::sqrt(strength(def, s, pos, env) / IR_REF_MW);
            (r.min(crate::sensors::radar::horizon_m(pos.y.max(0.0), s.top_m)), true)
        }
        SeekerKind::ActiveAcoustic => (env.sonar_range(r_ref, pos, s), false),
        SeekerKind::None => (0.0, false),
    };
    range.is_finite() && dist <= range && env.clear(pos, s.pos, optical, [g.body, s.entity])
}

fn acquire<E: SeekerEnv>(g: &Guided, def: &MunitionDef, pos: DVec3, vel: DVec3, est: DVec3, cos_fov: f64, env: &E) -> Option<EntityId> {
    let bearing_only = g.track.is_none();
    let boresight = vel.normalize_or_zero();
    let mut best: Option<(f64, EntityId)> = None;
    for s in env.signatures() {
        if s.entity == g.body || !sees_kind(def, s) { continue; }
        if !detectable(g, def, pos, vel, s, cos_fov, env) || env.own_side(g.side, s) { continue; }
        let score = if bearing_only { -boresight.dot((s.pos - pos).normalize_or_zero()) } else { (s.pos - est).length() };
        if best.is_none_or(|(b, _)| score < b) { best = Some((score, s.entity)); }
    }
    best.map(|(_, e)| e)
}

/// Intercept point for a body at `pos` flying at `speed` against a point moving at `vel`.
fn intercept(pos: DVec3, speed: f64, target: DVec3, vel: DVec3) -> DVec3 {
    let d = target - pos;
    let dist = d.length();
    if speed <= 1e-6 || dist < 1e-6 { return target; }
    let a = vel.length_squared() - speed * speed;
    let b = 2.0 * d.dot(vel);
    let c = dist * dist;
    let t = if a.abs() < 1e-9 { if b < 0.0 { -c / b } else { dist / speed } } else {
        let disc = b * b - 4.0 * a * c;
        if disc < 0.0 { dist / speed } else {
            let sq = libm::sqrt(disc);
            let (t1, t2) = ((-b - sq) / (2.0 * a), (-b + sq) / (2.0 * a));
            let lo = t1.min(t2);
            let hi = t1.max(t2);
            if lo > 0.0 { lo } else if hi > 0.0 { hi } else { dist / speed }
        }
    };
    target + vel * t.clamp(0.0, 3_600.0)
}

/// Arrived at or past a point: it is behind the body or within one tick of travel.
fn passed(pos: DVec3, vel: DVec3, point: DVec3) -> bool {
    let d = point - pos;
    d.dot(vel) <= 0.0 || d.length() <= vel.length() * DT * 2.0
}

fn step_body<E: SeekerEnv>(g: &mut Guided, def: &MunitionDef, pos: DVec3, vel: DVec3, env: &E) -> GuidanceOut {
    let tick = env.tick();
    let tuning = env.tuning();
    let torpedo = def.kind == MunitionKind::Torpedo;
    let has_seeker = def.seeker != SeekerKind::None;
    let cos_fov = fov_cos(def, tuning);
    let mut seduced_by = None;

    // Launch set-up for bearing-only launches and launches whose track was not held.
    if g.phase == SeekerPhase::Off {
        if let Some(t) = env.track(g.side, g.track) {
            (g.predicted, g.predicted_vel) = (t.pos, t.vel);
            g.phase = SeekerPhase::Midcourse;
        } else {
            let h = DVec3::new(vel.x, if def.domain == Domain::Air || def.kind == MunitionKind::Sam || def.kind == MunitionKind::PdMissile { vel.y } else { 0.0 }, vel.z).normalize_or_zero();
            let reach = if def.range_m.is_finite() && def.range_m > 0.0 { def.range_m } else { MISSILE_SEEKER_R_REF_M };
            (g.predicted, g.predicted_vel) = (pos + h * reach, DVec3::ZERO);
            g.phase = if has_seeker { SeekerPhase::Searching } else { SeekerPhase::Midcourse };
        }
        g.last_update_tick = tick;
    }

    // Midcourse updates over the datalink (torpedoes only by wire), until the seeker locks.
    let linked = !torpedo || def.wire_guided;
    let since = tick.saturating_sub(g.last_update_tick);
    if linked && matches!(g.phase, SeekerPhase::Midcourse | SeekerPhase::Searching) && since >= midcourse_period_ticks(tuning.guidance.midcourse_hz) {
        match env.track(g.side, g.track) {
            Some(t) => (g.predicted, g.predicted_vel) = (t.pos, t.vel),
            None => (g.predicted, g.predicted_vel) = (g.predicted + g.predicted_vel * (since as f64 * DT), DVec3::ZERO),
        }
        g.last_update_tick = tick;
    }
    let est = g.predicted + g.predicted_vel * (tick.saturating_sub(g.last_update_tick) as f64 * DT);

    // Seeker turn-on.
    if g.phase == SeekerPhase::Midcourse && has_seeker {
        let track = env.track(g.side, g.track);
        let r_ref = seeker_r_ref(def, tuning);
        let r95 = track.map_or(0.0, |t| t.r95_m);
        let on_range = match def.seeker {
            SeekerKind::ActiveRadar => {
                let rcs = track.map_or(0.0, |t| t.rcs_m2);
                let rcs = if rcs.is_finite() && rcs > 0.0 { rcs } else { tuning.sensors.sigma_ref_m2 };
                seeker_turn_on_range_m(r_ref, rcs, r95, tuning)
            }
            _ => r_ref + r95.max(0.0),
        };
        if (est - pos).length() <= on_range || passed(pos, vel, est) { g.phase = SeekerPhase::Searching; }
    }

    // Lock upkeep.
    if let (SeekerPhase::Locked | SeekerPhase::Seduced, Some(lock)) = (g.phase, g.lock) {
        let ok = match env.signature(lock) {
            Some(s) if matches!(s.kind, SigKind::Decoy(_)) => in_view(pos, vel, s.pos, cos_fov) && strength(def, s, pos, env) > 0.0,
            Some(s) => detectable(g, def, pos, vel, s, cos_fov, env),
            None => false,
        };
        if !ok {
            g.phase = if g.phase == SeekerPhase::Seduced { SeekerPhase::Lost } else { SeekerPhase::Searching };
            g.lock = None;
        }
    } else if matches!(g.phase, SeekerPhase::Locked | SeekerPhase::Seduced) {
        g.phase = SeekerPhase::Searching;
    }

    // Acquisition.
    if g.phase == SeekerPhase::Searching {
        if let Some(t) = acquire(g, def, pos, vel, est, cos_fov, env) { g.lock = Some(t); g.phase = SeekerPhase::Locked; }
    }

    // Decoys entering the resolution cell of the locked return.
    if let (SeekerPhase::Locked | SeekerPhase::Seduced, Some(lock)) = (g.phase, g.lock) {
        if let Some(lock_sig) = env.signature(lock) {
            let lock_pos = lock_sig.pos;
            let lock_strength = strength(def, lock_sig, pos, env);
            let cell = tuning.decoys.resolution_cell_m;
            for s in env.signatures() {
                let SigKind::Decoy(kind) = s.kind else { continue };
                if s.entity == lock || !decoy_competes(def.seeker, kind) || g.tested_decoys.binary_search(&s.entity).is_ok() { continue; }
                if kind == DecoyKind::Chaff && env.decoy_age(s.entity).is_none_or(|a| a < tuning.decoys.chaff_bloom_s) { continue; }
                if (s.pos - lock_pos).length() > cell || !in_view(pos, vel, s.pos, cos_fov) || env.own_side(g.side, s) { continue; }
                let d = strength(def, s, pos, env);
                if d <= 0.0 { continue; }
                if let Err(i) = g.tested_decoys.binary_search(&s.entity) { g.tested_decoys.insert(i, s.entity); }
                if env.seduce(g.body, s.entity, seduction_probability(d, lock_strength)) {
                    g.lock = Some(s.entity);
                    g.phase = SeekerPhase::Seduced;
                    seduced_by = Some(s.entity);
                    break;
                }
            }
        }
    }

    // Steering.
    let speed = vel.length().max(if def.speed_mps.is_finite() { def.speed_mps } else { 0.0 });
    let mut aim = est;
    let steer = match (g.phase, g.lock) {
        (SeekerPhase::Locked | SeekerPhase::Seduced, Some(lock)) => {
            let sig = env.signature(lock);
            let (mut tp, tv) = env.state(lock).or_else(|| sig.map(|s| (s.pos, s.vel))).unwrap_or((est, DVec3::ZERO));
            if let Some(s) = sig {
                if !torpedo && matches!(s.kind, SigKind::Vessel | SigKind::Wreck) && s.domain == Domain::Surface { tp.y = (0.5 * s.top_m).max(MIN_HULL_AIM_M); }
            }
            aim = tp;
            let fuse_r = match def.fuse { Fuse::Proximity { radius_m } => radius_m, _ => 0.0 };
            if fuse_r > 0.0 && (tp - pos).length() <= fuse_r { Steer::Detonate } else {
                let mut a = pn_accel(pos, vel, tp, tv, g.pn_n);
                if torpedo { a.y = 0.0; }
                Steer::Accel(a)
            }
        }
        (SeekerPhase::Midcourse | SeekerPhase::Searching, _) => {
            if passed(pos, vel, est) { Steer::Hold } else { let p = intercept(pos, speed, est, g.predicted_vel); aim = p; Steer::Waypoint(p) }
        }
        _ => Steer::Hold,
    };
    let los = (aim - pos).normalize_or_zero();
    if los != DVec3::ZERO { g.last_los = los; }
    let radiating = matches!(def.seeker, SeekerKind::ActiveRadar | SeekerKind::ActiveAcoustic) && matches!(g.phase, SeekerPhase::Searching | SeekerPhase::Locked | SeekerPhase::Seduced | SeekerPhase::Lost);
    GuidanceOut { body: g.body, steer, radiating, phase: g.phase, lock: g.lock, seduced_by }
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::ids::{MunitionId, TextKey};

    fn ashm() -> MunitionDef {
        MunitionDef {
            id: MunitionId::new("missile.ashm.light"), name: TextKey::new("x"), kind: MunitionKind::AntiShipMissile, phase: Default::default(), domain: Domain::Weapon,
            speed_mps: 270.0, range_m: 40_000.0, max_accel: 150.0, cruise_alt_m: 5.0, fuse: Fuse::Penetrate { armour_mm: 40.0, delay_cells: 2 }, he_b: 8, he_r_cells: 4,
            seeker: SeekerKind::ActiveRadar, seeker_r_ref_m: 12_000.0, seeker_fov_half_deg: 30.0, pn_n: 3.0, wire_guided: false, radius_m: 0.5, hp: 1, rcs_m2: 0.1,
            noise: 0.0, mass_kg: 350.0, cost: 40, sensors: vec![], payload: vec![], endurance_s: 0.0, model: Default::default(), icon: Default::default(),
        }
    }
    fn sam() -> MunitionDef {
        MunitionDef { kind: MunitionKind::Sam, speed_mps: 900.0, range_m: 30_000.0, max_accel: 400.0, cruise_alt_m: 500.0, fuse: Fuse::Proximity { radius_m: 8.0 }, pn_n: 4.0, seeker_r_ref_m: 15_000.0, ..ashm() }
    }

    #[allow(clippy::too_many_arguments)]
    fn sig(entity: EntityId, side: u8, kind: SigKind, domain: Domain, pos: DVec3, vel: DVec3, rcs: f64, top: f64) -> Signature {
        Signature { entity, side: SideId(side), kind, domain, class: None, pos, vel, heading_rad: 0.0, areas: None, rcs_flat_m2: rcs, top_m: top, depth_m: 0.0, noise: 1.0, ir_mw: 5.0, emissions: vec![], jammer_r_bt0_m: None, fired_recently: false, burning: false, lights: false }
    }
    fn id(i: u32) -> EntityId { EntityId { index: i, generation: 1 } }

    /// Stand-in world: flat RCS, radar law without clutter, clear lines of sight, a hash RNG.
    struct TestEnv { tick: u64, tuning: TuningTable, sigs: Vec<Signature>, track: Option<TrackEst>, ages: Vec<(EntityId, f64)>, seed: u64 }
    impl SeekerEnv for TestEnv {
        fn tick(&self) -> u64 { self.tick }
        fn tuning(&self) -> &TuningTable { &self.tuning }
        fn track(&self, _side: SideId, track: TrackId) -> Option<TrackEst> { if track.is_none() { None } else { self.track } }
        fn signatures(&self) -> &[Signature] { &self.sigs }
        fn signature(&self, id: EntityId) -> Option<&Signature> { self.sigs.iter().find(|s| s.entity == id) }
        fn state(&self, id: EntityId) -> Option<(DVec3, DVec3)> { self.signature(id).map(|s| (s.pos, s.vel)) }
        fn own_side(&self, side: SideId, s: &Signature) -> bool { s.side == side }
        fn rcs_toward(&self, s: &Signature, _from: DVec3) -> f64 { s.rcs_flat_m2 }
        fn radar_range(&self, r_ref: f64, _from: DVec3, _s: &Signature, sigma: f64) -> f64 { r_ref * libm::pow(sigma / 100.0, 0.25) }
        fn sonar_range(&self, r_ref: f64, _from: DVec3, _s: &Signature) -> f64 { r_ref }
        fn clear(&self, _a: DVec3, _b: DVec3, _optical: bool, _ignore: [EntityId; 2]) -> bool { true }
        fn decoy_age(&self, id: EntityId) -> Option<f64> { self.ages.iter().find(|(e, _)| *e == id).map(|(_, a)| *a) }
        fn seduce(&self, missile: EntityId, decoy: EntityId, p: f64) -> bool {
            let mut x = self.seed ^ self.tick.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ missile.to_u64().rotate_left(17) ^ decoy.to_u64().rotate_left(41);
            x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            x ^= x >> 31;
            ((x >> 11) as f64 / (1u64 << 53) as f64) < p
        }
    }

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 { self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (self.0 >> 11) as f64 / (1u64 << 53) as f64 }
        fn range(&mut self, a: f64, b: f64) -> f64 { a + (b - a) * self.next() }
    }

    /// Stand-in for sim-core's body integration: constant speed, lateral demand clamped to the type's limit,
    /// waypoints flown with a lateral demand toward the point at cruise altitude.
    fn fly(pos: &mut DVec3, vel: &mut DVec3, steer: Steer, def: &MunitionDef) {
        let speed = vel.length().max(1.0);
        let lateral = |a: DVec3, v: DVec3| { let vh = v.normalize(); let a = a - vh * a.dot(vh); if a.length() > def.max_accel { a.normalize() * def.max_accel } else { a } };
        let a = match steer {
            Steer::Accel(a) => lateral(a, *vel),
            Steer::Waypoint(p) => { let mut p = p; p.y = def.cruise_alt_m; let want = (p - *pos).normalize_or_zero() * speed; lateral((want - *vel) * 2.0, *vel) }
            _ => DVec3::ZERO,
        };
        *vel = (*vel + a * DT).normalize() * speed;
        *pos += *vel * DT;
    }

    fn guided(body: EntityId, track: TrackId, n: f64, predicted: DVec3, predicted_vel: DVec3) -> Guided {
        Guided { body, side: SideId(0), launcher: id(1), kind: MunitionIdx(0), track, phase: SeekerPhase::Midcourse, lock: None, predicted, predicted_vel, last_update_tick: 0, pn_n: n, tested_decoys: vec![], last_los: DVec3::ZERO }
    }

    #[test]
    fn pn_law() {
        // Head-on with a crossing target: demand points toward the target's crossing direction.
        let a = pn_accel(DVec3::ZERO, DVec3::new(300.0, 0.0, 0.0), DVec3::new(3_000.0, 0.0, 0.0), DVec3::new(0.0, 0.0, 20.0), 3.0);
        assert!(a.z > 0.0 && a.x.abs() < 1e-9 && a.y.abs() < 1e-9);
        // N * Vc * lambda_dot: Vc = 300, lambda_dot = 20 / 3000.
        assert!((a.z - 3.0 * 300.0 * 20.0 / 3_000.0).abs() < 1e-9);
        // Collision course: zero demand. Opening: zero demand. Degenerate input: zero.
        assert_eq!(pn_accel(DVec3::ZERO, DVec3::new(100.0, 0.0, 0.0), DVec3::new(1_000.0, 0.0, 0.0), DVec3::new(-50.0, 0.0, 0.0), 4.0), DVec3::ZERO);
        assert_eq!(pn_accel(DVec3::ZERO, DVec3::new(-100.0, 0.0, 0.0), DVec3::new(1_000.0, 0.0, 0.0), DVec3::ZERO, 4.0), DVec3::ZERO);
        assert_eq!(pn_accel(DVec3::ZERO, DVec3::X, DVec3::ZERO, DVec3::ZERO, 4.0), DVec3::ZERO);
        assert_eq!(pn_accel(DVec3::ZERO, DVec3::X, DVec3::new(f64::NAN, 0.0, 0.0), DVec3::ZERO, 4.0), DVec3::ZERO);
    }

    #[test]
    fn turn_on_range_and_seduction_numbers() {
        let t = TuningTable::default();
        assert!((seeker_turn_on_range_m(12_000.0, 100.0, 0.0, &t) - 12_000.0).abs() < 1e-9);
        assert!((seeker_turn_on_range_m(12_000.0, 1_600.0, 500.0, &t) - 24_500.0).abs() < 1e-6);
        assert_eq!(seeker_turn_on_range_m(12_000.0, f64::NAN, -5.0, &t), 0.0);
        // L524: about 87% on a 150 t craft (~150 m^2) and about 13% on an 1,800 t hull (~6,700 m^2).
        assert!((seduction_probability(1_000.0, 150.0) - 0.87).abs() < 0.005);
        assert!((seduction_probability(1_000.0, 6_700.0) - 0.13).abs() < 0.005);
    }

    #[test]
    fn tuning_fallback_raises_n_and_fov() {
        let mut t = TuningTable::default();
        assert_eq!(class_n(&ashm(), &t), 3.0);
        assert_eq!(class_n(&sam(), &t), 4.0);
        t.guidance.pn_sam = 5.0;
        t.guidance.pn_antiship = 4.0;
        t.guidance.seeker_fov_half_deg = 45.0;
        assert_eq!(class_n(&sam(), &t), 5.0);
        assert_eq!(class_n(&ashm(), &t), 4.0);
        assert!((fov_cos(&ashm(), &t) - libm::cos(45f64.to_radians())).abs() < 1e-12);
    }

    #[test]
    fn table_keeps_order_and_replaces() {
        let mut table = GuidanceTable::default();
        for i in [5, 2, 9, 2] { let g = guided(id(i), TrackId(1), 3.0, DVec3::ZERO, DVec3::ZERO); match table.guided.binary_search_by(|x| x.body.cmp(&g.body)) { Ok(j) => table.guided[j] = g, Err(j) => table.guided.insert(j, g) } }
        assert_eq!(table.guided.iter().map(|g| g.body.index).collect::<Vec<_>>(), vec![2, 5, 9]);
        table.remove(id(5));
        table.remove(id(77));
        assert_eq!(table.guided.len(), 2);
    }

    #[test]
    fn rejects_own_side_and_takes_neutral() {
        let def = ashm();
        let pos = DVec3::new(0.0, 5.0, 0.0);
        let vel = DVec3::new(270.0, 0.0, 0.0);
        let own = sig(id(10), 0, SigKind::Vessel, Domain::Surface, DVec3::new(5_000.0, 0.0, 0.0), DVec3::ZERO, 2_000.0, 10.0);
        let neutral = sig(id(11), 2, SigKind::Vessel, Domain::Surface, DVec3::new(6_000.0, 0.0, 300.0), DVec3::ZERO, 2_000.0, 10.0);
        let env = TestEnv { tick: 0, tuning: TuningTable::default(), sigs: vec![own, neutral], track: None, ages: vec![], seed: 1 };
        let mut g = guided(id(3), TrackId(1), 3.0, DVec3::new(5_000.0, 0.0, 0.0), DVec3::ZERO);
        g.phase = SeekerPhase::Searching;
        let out = step_body(&mut g, &def, pos, vel, &env);
        assert_eq!(out.lock, Some(id(11)));
        assert!(out.radiating && out.phase == SeekerPhase::Locked && matches!(out.steer, Steer::Accel(_)));
    }

    /// One closed-loop anti-ship run at a Coarse track 30 km out. Returns (acquired, seduced, miss inside box).
    fn ashm_run(seed: u64, rcs: f64, chaff: bool) -> (bool, bool, bool) {
        let mut rng = Lcg(seed.wrapping_mul(7919) ^ 0xA5A5);
        let def = ashm();
        let tuning = TuningTable::default();
        let ship_id = id(20);
        let chaff_id = id(30);
        let speed = 30.0 * sim_core::time::KN;
        let mut heading = rng.range(0.0, core::f64::consts::TAU);
        let turn_sign = if rng.next() < 0.5 { 1.0 } else { -1.0 };
        let brg = rng.range(-0.5, 0.5);
        let mut ship = DVec3::new(30_000.0 * libm::cos(brg), 0.0, 30_000.0 * libm::sin(brg));
        // Coarse track: up to 1 km position error and 2 m/s velocity error, 95% radius 2 km.
        let err = DVec3::new(rng.range(-700.0, 700.0), 0.0, rng.range(-700.0, 700.0));
        let verr = DVec3::new(rng.range(-2.0, 2.0), 0.0, rng.range(-2.0, 2.0));
        let mut pos = DVec3::new(0.0, 5.0, 0.0);
        let mut vel = (ship + err - pos).normalize() * def.speed_mps;
        let ship_vel = |h: f64| DVec3::new(libm::sin(h), 0.0, -libm::cos(h)) * speed;
        let mut g = guided(id(3), TrackId(7), class_n(&def, &tuning), ship + err, ship_vel(heading) + verr);
        let mut env = TestEnv { tick: 0, tuning, sigs: vec![], track: None, ages: vec![], seed };
        let mut chaff_at: Option<(DVec3, u64)> = None;
        let (mut acquired, mut seduced, mut best) = (false, false, f64::INFINITY);
        let mut best_local = DVec3::ZERO;
        let mut steer = Steer::Hold;
        for tick in 0..(200.0 / DT) as u64 {
            let dist = (ship - pos).length();
            // Hard turn when the missile is inside 6 km; chaff fired at the same moment (doctrine decoy trigger).
            if dist < 6_000.0 {
                heading += turn_sign * 3f64.to_radians() * DT;
                if chaff && chaff_at.is_none() { chaff_at = Some((ship + DVec3::new(rng.range(-60.0, 60.0), 0.0, rng.range(-60.0, 60.0)), tick)); }
            }
            ship += ship_vel(heading) * DT;
            if tick % 2 == 0 {
                env.tick = tick;
                env.track = Some(TrackEst { pos: ship + err, vel: ship_vel(heading) + verr, rcs_m2: rcs, r95_m: 2_000.0 });
                if env.sigs.is_empty() { env.sigs.push(sig(ship_id, 1, SigKind::Vessel, Domain::Surface, ship, ship_vel(heading), rcs, 8.0)); }
                env.sigs.truncate(1);
                (env.sigs[0].pos, env.sigs[0].vel) = (ship, ship_vel(heading));
                env.ages.clear();
                if let Some((cp, t0)) = chaff_at {
                    let age = (tick - t0) as f64 * DT;
                    let t = &env.tuning.decoys;
                    let r = if age < t.chaff_bloom_s { t.chaff_rcs_m2 * age / t.chaff_bloom_s } else { (t.chaff_rcs_m2 * (1.0 - (age - t.chaff_bloom_s) / t.chaff_life_s)).max(0.0) };
                    env.sigs.push(sig(chaff_id, 1, SigKind::Decoy(DecoyKind::Chaff), Domain::Surface, cp + DVec3::new(age * 3.0, 20.0, 0.0), DVec3::ZERO, r, 40.0));
                    env.ages.push((chaff_id, age));
                }
                let out = step_body(&mut g, &def, pos, vel, &env);
                if out.lock == Some(ship_id) { acquired = true; }
                if out.seduced_by.is_some() { seduced = true; }
                steer = out.steer;
            }
            if chaff && !g.tested_decoys.is_empty() { return (acquired, seduced, false); }
            let prev = pos;
            fly(&mut pos, &mut vel, steer, &def);
            // Closest approach to the hull centre at half height, in the hull frame.
            let seg = pos - prev;
            let c = ship + DVec3::new(0.0, 4.0, 0.0);
            let s = if seg.length_squared() > 0.0 { ((c - prev).dot(seg) / seg.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
            let p = prev + seg * s;
            if (p - c).length() < best { best = (p - c).length(); let f = ship_vel(heading).normalize(); let side = DVec3::new(-f.z, 0.0, f.x); best_local = DVec3::new((p - c).dot(f), p.y - c.y, (p - c).dot(side)); }
            if (pos - ship).length() > best + 2_000.0 && best < 1_000.0 { break; }
        }
        // Reference hull box 40 m x 7 m x 8 m.
        let inside = best_local.x.abs() <= 20.0 && best_local.z.abs() <= 3.5 && best_local.y.abs() <= 4.0;
        (acquired, seduced, inside)
    }

    /// Gate (3) shape at L526 on stand-in kinematics: acquisition at least 90% from a Coarse track at 30 km;
    /// one chaff salvo seduces 60-90% against a 150 t craft and 10-40% against an 1,800 t hull. Gate (2) shape:
    /// the median anti-ship miss against a 30 kn hull turning hard is inside the box.
    #[test]
    fn gate_shape_antiship_acquisition_seduction_and_box() {
        let runs = 100;
        let (mut acq, mut inside) = (0, 0);
        for s in 0..runs { let (a, _, i) = ashm_run(s, 150.0, false); acq += a as u32; inside += i as u32; }
        assert!(acq as f64 / runs as f64 >= 0.90, "acquired {acq}/{runs}");
        assert!(inside as f64 / runs as f64 > 0.5, "inside box {inside}/{runs}");
        let small = (0..runs).filter(|&s| ashm_run(1_000 + s, 150.0, true).1).count() as f64 / runs as f64;
        let large = (0..runs).filter(|&s| ashm_run(2_000 + s, 6_700.0, true).1).count() as f64 / runs as f64;
        assert!((0.60..=0.90).contains(&small), "small craft seduced {small}");
        assert!((0.10..=0.40).contains(&large), "large hull seduced {large}");
    }

    /// Gate (2) shape: median SAM miss against an aircraft weaving at 3 g is 5 m or less (stand-in kinematics).
    #[test]
    fn gate_shape_sam_against_weaver() {
        let def = sam();
        let mut misses = Vec::new();
        for seed in 0..100u64 {
            let mut rng = Lcg(seed ^ 0x3C3C);
            let air = id(40);
            let range = rng.range(6_000.0, 12_000.0);
            let brg = rng.range(-0.4, 0.4);
            let alt = rng.range(300.0, 2_000.0);
            let mut tgt = DVec3::new(range * libm::cos(brg), alt, range * libm::sin(brg));
            let crs = rng.range(0.0, core::f64::consts::TAU);
            let base = DVec3::new(libm::cos(crs), 0.0, libm::sin(crs)) * 250.0;
            let side = DVec3::new(-base.z, 0.0, base.x).normalize();
            let (w, phase) = (rng.range(0.3, 0.8), rng.range(0.0, core::f64::consts::TAU));
            let mut pos = DVec3::new(0.0, 10.0, 0.0);
            let mut vel = (tgt - pos).normalize() * def.speed_mps;
            let mut g = guided(id(3), TrackId(9), class_n(&def, &TuningTable::default()), tgt, base);
            let mut env = TestEnv { tick: 0, tuning: TuningTable::default(), sigs: vec![], track: None, ages: vec![], seed };
            let mut steer = Steer::Hold;
            let mut best = f64::INFINITY;
            for tick in 0..(40.0 / DT) as u64 {
                let t = tick as f64 * DT;
                // 3 g lateral weave: v = base + side * (3g / w) sin(w t + phase).
                let tv = base + side * (3.0 * 9.81 / w) * libm::sin(w * t + phase);
                tgt += tv * DT;
                if tick % 2 == 0 {
                    env.tick = tick;
                    env.track = Some(TrackEst { pos: tgt, vel: tv, rcs_m2: 5.0, r95_m: 100.0 });
                    env.sigs = vec![sig(air, 1, SigKind::Body(MunitionIdx(1)), Domain::Air, tgt, tv, 5.0, alt)];
                    steer = step_body(&mut g, &def, pos, vel, &env).steer;
                }
                if steer == Steer::Detonate { best = best.min((tgt - pos).length()); break; }
                let prev = pos;
                fly(&mut pos, &mut vel, steer, &def);
                let seg = pos - prev;
                let s = if seg.length_squared() > 0.0 { ((tgt - prev).dot(seg) / seg.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
                best = best.min((prev + seg * s - tgt).length());
                if (pos - tgt).length() > best + 500.0 && best < 200.0 { break; }
            }
            misses.push(best);
        }
        misses.sort_by(|a, b| a.total_cmp(b));
        let median = misses[misses.len() / 2];
        assert!(median <= 5.0, "median miss {median}");
    }

    #[test]
    fn bearing_only_launch_searches_and_lost_after_decoy_fades() {
        let def = ashm();
        let mut env = TestEnv { tick: 0, tuning: TuningTable::default(), sigs: vec![], track: None, ages: vec![], seed: 3 };
        let mut g = guided(id(3), TrackId::NONE, 3.0, DVec3::ZERO, DVec3::ZERO);
        g.phase = SeekerPhase::Off;
        let pos = DVec3::new(0.0, 5.0, 0.0);
        let vel = DVec3::new(270.0, 0.0, 0.0);
        let out = step_body(&mut g, &def, pos, vel, &env);
        assert_eq!(out.phase, SeekerPhase::Searching);
        assert!(out.radiating && matches!(out.steer, Steer::Waypoint(p) if p.x > 30_000.0));
        // Seduced onto a decoy that then fades: Lost, never back to the hull.
        g.phase = SeekerPhase::Seduced;
        g.lock = Some(id(30));
        env.sigs = vec![sig(id(20), 1, SigKind::Vessel, Domain::Surface, DVec3::new(3_000.0, 0.0, 0.0), DVec3::ZERO, 500.0, 8.0), sig(id(30), 1, SigKind::Decoy(DecoyKind::Chaff), Domain::Surface, DVec3::new(2_900.0, 20.0, 50.0), DVec3::ZERO, 0.0, 40.0)];
        let out = step_body(&mut g, &def, pos, vel, &env);
        assert_eq!(out.phase, SeekerPhase::Lost);
        assert_eq!(out.lock, None);
        assert_eq!(out.steer, Steer::Hold);
    }

    #[test]
    fn midcourse_freezes_when_track_dropped_and_torpedo_needs_wire() {
        let def = ashm();
        let mut env = TestEnv { tick: 0, tuning: TuningTable::default(), sigs: vec![], track: Some(TrackEst { pos: DVec3::new(30_000.0, 0.0, 0.0), vel: DVec3::new(0.0, 0.0, 10.0), rcs_m2: 500.0, r95_m: 1_000.0 }), ages: vec![], seed: 3 };
        let mut g = guided(id(3), TrackId(4), 3.0, DVec3::new(30_000.0, 0.0, 0.0), DVec3::new(0.0, 0.0, 10.0));
        let (pos, vel) = (DVec3::new(0.0, 5.0, 0.0), DVec3::new(270.0, 0.0, 0.0));
        env.tick = 60;
        env.track = Some(TrackEst { pos: DVec3::new(30_000.0, 0.0, 620.0), vel: DVec3::new(0.0, 0.0, 10.0), rcs_m2: 500.0, r95_m: 1_000.0 });
        step_body(&mut g, &def, pos, vel, &env);
        assert_eq!(g.predicted.z, 620.0);
        env.tick = 120;
        env.track = None;
        step_body(&mut g, &def, pos, vel, &env);
        assert!((g.predicted.z - 630.0).abs() < 1e-9 && g.predicted_vel == DVec3::ZERO);
        // A plain torpedo ignores picture updates; a wire-guided one takes them.
        let torp = MunitionDef { kind: MunitionKind::Torpedo, seeker: SeekerKind::ActiveAcoustic, seeker_r_ref_m: 1_500.0, speed_mps: 20.6, cruise_alt_m: -8.0, ..ashm() };
        let mut t = guided(id(4), TrackId(4), 3.0, DVec3::new(5_000.0, 0.0, 0.0), DVec3::ZERO);
        env.tick = 180;
        env.track = Some(TrackEst { pos: DVec3::new(5_000.0, 0.0, 400.0), vel: DVec3::ZERO, rcs_m2: 0.0, r95_m: 50.0 });
        step_body(&mut t, &torp, DVec3::new(0.0, -8.0, 0.0), DVec3::new(20.6, 0.0, 0.0), &env);
        assert_eq!(t.predicted.z, 0.0);
        let wired = MunitionDef { wire_guided: true, ..torp };
        step_body(&mut t, &wired, DVec3::new(0.0, -8.0, 0.0), DVec3::new(20.6, 0.0, 0.0), &env);
        assert_eq!(t.predicted.z, 400.0);
    }
}
