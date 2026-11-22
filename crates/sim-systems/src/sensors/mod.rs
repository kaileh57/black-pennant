//! Unit `sys-sensors` (hard: staggering, sector sweep, spatial cull and the 1.0 ms sensor budget).
//! Stage 2 of the pipeline: the sensor scheduler. The detection laws live in the sibling files:
//! `radar.rs` (unit `sys-detect-radar`), `optical.rs` (unit `sys-detect-optical`), `sonar.rs`
//! (unit `sys-detect-sonar`). The types in this file are the contract all four share.
//! DESIGN.md L400-419 (pipeline, rates, stagger, budget and its fallbacks), L430 (determinism of detection,
//! measurement errors), L441 (noise keyed on (seed, tick, sensor id, target id)).
//!
//! Scheduling rules (L412): period p = 60 / rate ticks; a sensor evaluates on ticks where
//! `(tick + rng::stagger(sensor key)) mod p == 0`. Rotating search and navigation radars (24 rpm) check only
//! targets inside the sector swept since their last evaluation, so one detection per target per rotation.
//! Fire-control radars and seekers stare and update every evaluation. Pairs are culled by a uniform spatial
//! grid (cell size `SensorTuning::spatial_grid_cell_m`) before any law runs. Detection is deterministic: a
//! pair inside the final range is detected every evaluation; only the measurement error is random.
pub mod optical;
pub mod radar;
pub mod sonar;

use crate::prelude::*;
use crate::ship::ShipTable;
use crate::signature::{Signature, SignatureTable};
use content::part::SensorKind;

/// Measurement errors, 1 sigma (table at L430). Fixed; designers never tune them (L434).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeasurementError { pub range_m: Option<f64>, pub range_fraction: Option<f64>, pub bearing_deg: f64 }
/// The table at L430. Real code: these are spec constants.
pub fn measurement_error(kind: SensorKind) -> MeasurementError {
    match kind {
        SensorKind::SearchRadar | SensorKind::NavRadar => MeasurementError { range_m: Some(50.0), range_fraction: None, bearing_deg: 0.5 },
        SensorKind::FireControlRadar => MeasurementError { range_m: Some(5.0), range_fraction: None, bearing_deg: 0.05 },
        SensorKind::Seeker => MeasurementError { range_m: Some(2.0), range_fraction: None, bearing_deg: 0.1 },
        SensorKind::Esm => MeasurementError { range_m: None, range_fraction: None, bearing_deg: 2.0 },
        SensorKind::PassiveSonar => MeasurementError { range_m: None, range_fraction: None, bearing_deg: 3.0 },
        SensorKind::ActiveSonar => MeasurementError { range_m: Some(20.0), range_fraction: None, bearing_deg: 1.0 },
        SensorKind::Visual | SensorKind::Camera => MeasurementError { range_m: None, range_fraction: Some(0.10), bearing_deg: 0.2 },
    }
}

/// One sensor as the scheduler sees it: static facts from the catalogue plus state refreshed each tick.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SensorInstance {
    pub id: SensorRef,
    pub side: SideId,
    pub kind: SensorKind,
    /// Reference range in metres (vs 100 m^2 for radar and active sonar, vs N = 1.0 for passive sonar).
    pub r_ref_m: f64,
    /// Evaluation period in ticks (60 / rate) and stagger phase.
    pub period_ticks: u32,
    pub phase: u64,
    /// Rotation in radians per tick for rotating antennas; 0 = staring.
    pub rot_per_tick: f64,
    pub fov_half_rad: f64,
    /// Emitter library index, `u16::MAX` for passive sensors.
    pub emitter: u16,
    // ---- refreshed each tick ----
    /// World position of the antenna, lens or transducer.
    pub pos: DVec3,
    /// Height above the sea surface in metres (horizon cull), negative for hull sonar.
    pub height_m: f64,
    /// Boresight compass bearing for staring sensors; current antenna bearing for rotating ones.
    pub bearing_rad: f64,
    /// Bearing at the previous evaluation (start of the swept sector).
    pub swept_from_rad: f64,
    /// Powered, working and allowed to operate (active emitters also need EMCON permission).
    pub active: bool,
    pub radiating: bool,
    /// Range factor from damage: 0.5 at Heavy (L561).
    pub output: f64,
    /// Error multiplier: 3.0 under local control (L394), else 1.0.
    pub error_factor: f64,
    /// Carrier speed in knots (passive sonar: over 15 kn halves range, over 25 kn hears nothing, L453).
    pub own_speed_kn: f64,
    pub last_eval_tick: u64,
}

/// What a detection measured.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Measurement {
    /// Range and compass bearing from the sensor, with altitude (positive) or depth (negative) when the sensor
    /// measures it (fire-control radar, seeker, active sonar).
    RangeBearing { range_m: f64, bearing_rad: f64, height_m: Option<f64> },
    BearingOnly { bearing_rad: f64 },
}

bitflags::bitflags! {
    /// Identification evidence a detection carries (L472-475).
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct Evidence: u16 {
        const ALTITUDE = 1;
        const DEPTH = 1 << 1;
        const EMITTER = 1 << 2;
        const ACOUSTIC = 1 << 3;
        /// Visual or camera sighting inside 50% of R_vis (Class) and inside 25% (Exact).
        const VISUAL_HALF = 1 << 4;
        const VISUAL_QUARTER = 1 << 5;
        const FIRE_CONTROL = 1 << 6;
    }
}

/// One raw detection with error (expert view, L405). `target` is ground truth kept for bookkeeping and gate
/// statistics only: association never reads it, and it never leaves the server.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    pub tick: u64,
    pub sensor: SensorRef,
    pub side: SideId,
    pub kind: SensorKind,
    pub sensor_pos: DVec3,
    pub target: EntityId,
    pub measurement: Measurement,
    /// 1-sigma errors actually applied (after `error_factor`).
    pub sigma_range_m: f64,
    pub sigma_bearing_rad: f64,
    pub evidence: Evidence,
    /// Emitter library index heard (ESM) or `u16::MAX`.
    pub emitter: u16,
    /// Apparent radar cross-section in m^2 (radar) for the "RCS and speed consistent with one type" rule.
    pub rcs_m2: f32,
    /// Target's domain as far as this sensor can tell.
    pub domain_hint: Option<Domain>,
}

/// Environment facts the laws need besides the pair.
#[derive(Clone, Copy)]
pub struct SenseEnv<'a> {
    pub ctx: &'a SimCtx<'a>,
    pub world: &'a World,
    /// Clutter C for the current sea state, interpolated from `SensorTuning::clutter` (L427).
    pub clutter: f64,
    /// Night visual factor applies (time-of-day band Night, L436).
    pub night: bool,
}

/// Uniform spatial grid over entity positions for pair culling (L414, L419 fallback 2).
#[derive(Clone, Debug, Default)]
pub struct SpatialGrid { pub cell_m: f64, pub cells: BTreeMap<(i32, i32), Vec<u32>> }

#[derive(Clone, Debug, Default)]
pub struct SensorScheduler {
    /// Every sensor in the sortie, ascending [`SensorRef`].
    pub sensors: Vec<SensorInstance>,
    pub grid: SpatialGrid,
    /// Pair checks done last tick (work units for the sensor share of the tick, L416-418).
    pub pairs_last_tick: u32,
}
impl SensorScheduler {
    /// Add the sensors of a newly spawned vessel (sensor parts, lookouts: every crewed hull has one Visual
    /// sensor at bridge height) or body (`MunitionDef::sensors`, seeker). Unit: `sys-sensors`.
    pub fn add_carrier(&mut self, world: &World, carrier: EntityId, ctx: &SimCtx<'_>) {
        self.remove_carrier(carrier);
        let mut added = carrier_sensors(world, carrier, ctx, &sim_core::rng::stagger);
        if added.is_empty() { return; }
        self.sensors.append(&mut added);
        self.sensors.sort_by_key(|s| s.id);
        self.sensors.dedup_by_key(|s| s.id);
    }
    pub fn remove_carrier(&mut self, carrier: EntityId) { self.sensors.retain(|s| s.id.carrier != carrier); }
    /// Refresh the per-tick fields of every sensor from part state, link state (local control = 3x error),
    /// damage level and each ship's emission permissions. Unit: `sys-sensors`.
    pub fn refresh(&mut self, world: &World, ships: &ShipTable, ctx: &SimCtx<'_>) {
        let tick = ctx.tick.0;
        let bodies = world.bodies();
        for s in &mut self.sensors {
            if let Some(v) = world.vessel(s.id.carrier) {
                refresh_on_vessel(s, v, ships, ctx, tick);
            } else if let Some(row) = bodies.row_of(s.id.carrier) {
                refresh_on_body(s, bodies, row, ctx, tick);
            } else {
                s.active = false;
                s.radiating = false;
            }
        }
    }
    /// Run every sensor due this tick against the culled candidate targets and append detections in ascending
    /// (sensor, target) order. Pairs on the same side are skipped. Applies measurement noise from
    /// `rng::Stream::SensorNoise` keyed (sensor key, target id). Honours `budget_pairs`: sensors that do not fit
    /// run first next tick. When `kinematic_pairs_2hz` is set (fallback 3 at L419), pairs whose carrier and
    /// target are both kinematic hulls run at 2 Hz. Unit: `sys-sensors`.
    pub fn step(&mut self, sigs: &SignatureTable, env: &SenseEnv<'_>, budget_pairs: u32, kinematic_pairs_2hz: bool, out: &mut Vec<Detection>) {
        let kinematic: Vec<EntityId> = if kinematic_pairs_2hz {
            let mut k: Vec<EntityId> = env.world.vessels().filter(|(_, v)| v.fidelity == sim_core::vessel::Fidelity::Kinematic).map(|(_, v)| v.id).collect();
            k.sort_unstable();
            k
        } else { Vec::new() };
        let ctx = env.ctx;
        let pass = StepPass {
            tick: ctx.tick.0,
            tuning: &ctx.tuning.sensors,
            visibility_m: ctx.weather.visibility_m,
            budget_pairs,
            kinematic: &kinematic,
            kinematic_2hz: kinematic_pairs_2hz,
        };
        self.step_core(sigs, &pass, &|s, t| detect(s, t, env), &|s, t, draw| sim_core::rng::normal(ctx.key(sim_core::rng::Stream::SensorNoise, s.key(), t.to_u64()).draw(draw)), out);
    }
}

// ------------------------------------------------------------------------------------------------------------
// Scheduling constants and rules
// ------------------------------------------------------------------------------------------------------------

/// `SensorRef::index` of the implicit lookout every crewed hull carries. No part slot reaches it.
pub const LOOKOUT_INDEX: u16 = u16::MAX;
/// Eye height of the lookout above the bridge console's centre, metres.
pub const LOOKOUT_EYE_M: f64 = 1.5;
/// Rotation rate of search and navigation radars when the part names none (L412).
pub const ROTATING_RPM: f64 = 24.0;
/// The stock search and navigation radar rate the 5 Hz default refers to (L412).
pub const DEFAULT_SEARCH_HZ: f64 = sim_core::time::DEFAULT_SENSOR_HZ as f64;
/// Fire-control radars and seekers (L412).
pub const FIRE_CONTROL_HZ: f64 = 10.0;
/// ESM, sonar, visual lookouts and camera masts (L412).
pub const PASSIVE_HZ: f64 = 2.0;
/// Designer range for search and navigation radar rates (L412).
pub const SEARCH_HZ_RANGE: (f64, f64) = (2.0, 10.0);
/// Period in ticks of the 2 Hz rate applied to kinematic pairs under fallback 3 (L419).
pub const KINEMATIC_PAIR_PERIOD: u64 = 30;
/// Starting reference ranges used when a part leaves `r_ref_m` unset (L432, L453-454): navigation and search
/// radar on a small mast 20 km, fire-control radar 25 km, missile seeker 12 km.
pub const DEFAULT_SEARCH_R_REF_M: f64 = 20_000.0;
pub const DEFAULT_FIRE_CONTROL_R_REF_M: f64 = 25_000.0;
pub const DEFAULT_SEEKER_R_REF_M: f64 = 12_000.0;

/// Evaluation rate in Hz of a sensor kind (L412). Search and navigation radars use the part's designer rate,
/// clamped to 2..=10 Hz; a part left at the 5 Hz default (or with no valid rate) follows
/// `SensorTuning::search_radar_hz`, which fallback 1 at L419 lowers to 3 Hz. Every other kind has a fixed
/// rate: fire control and seekers 10 Hz, the rest 2 Hz.
pub fn rate_hz(kind: SensorKind, part_rate_hz: f64, tuning: &content::tuning::SensorTuning) -> f64 {
    match kind {
        SensorKind::SearchRadar | SensorKind::NavRadar => {
            let tuned = if tuning.search_radar_hz.is_finite() && tuning.search_radar_hz > 0.0 { tuning.search_radar_hz } else { DEFAULT_SEARCH_HZ };
            let r = if part_rate_hz.is_finite() && part_rate_hz > 0.0 && part_rate_hz != DEFAULT_SEARCH_HZ { part_rate_hz } else { tuned };
            r.clamp(SEARCH_HZ_RANGE.0, SEARCH_HZ_RANGE.1)
        }
        SensorKind::FireControlRadar | SensorKind::Seeker => FIRE_CONTROL_HZ,
        SensorKind::Esm | SensorKind::PassiveSonar | SensorKind::ActiveSonar | SensorKind::Visual | SensorKind::Camera => PASSIVE_HZ,
    }
}

/// Period in ticks (60 / rate), at least 1 (L412).
pub fn period_ticks(rate_hz: f64) -> u32 {
    if !(rate_hz.is_finite() && rate_hz > 0.0) { return sim_core::TICK_HZ; }
    let p = (sim_core::TICK_HZ as f64 / rate_hz).round();
    if p.is_finite() && p >= 1.0 { (p as u32).min(sim_core::TICK_HZ * 60) } else { 1 }
}

/// Phase residue of a sensor: `(tick + phase) mod p`, computed without overflow.
fn residue(tick: u64, phase: u64, p: u64) -> u64 { let p = p.max(1); ((tick % p) + (phase % p)) % p }

/// The most recent scheduled evaluation tick at or before `tick`, if any: the last t <= tick with
/// `(t + phase) mod p == 0` (L412).
pub fn last_due_tick(tick: u64, phase: u64, period: u32) -> Option<u64> { tick.checked_sub(residue(tick, phase, period as u64)) }

/// Rotation in radians per tick for a rotating antenna; 0 for staring sensors. Search and navigation radars
/// rotate at the part's rpm, 24 rpm when it names none (L412); every other kind stares.
pub fn rotation_per_tick(kind: SensorKind, rpm: Option<f64>) -> f64 {
    match kind {
        SensorKind::SearchRadar | SensorKind::NavRadar => {
            let rpm = match rpm { Some(r) if r.is_finite() && r > 0.0 => r, _ => ROTATING_RPM };
            rpm * core::f64::consts::TAU / 60.0 / sim_core::TICK_HZ as f64
        }
        _ => 0.0,
    }
}

/// Antenna bearing of a rotating sensor from the tick alone: a fixed start angle from its stagger phase plus
/// the rotation since tick 0. Used for body sensors and whenever a hull's part state does not turn.
pub fn formula_bearing(phase: u64, rot_per_tick: f64, tick: u64) -> f64 {
    let start = (phase % 3_600) as f64 * (core::f64::consts::TAU / 3_600.0);
    if !rot_per_tick.is_finite() { return wrap_tau(start); }
    // Reduce the tick modulo whole turns first so large ticks keep full precision.
    let ticks_per_turn = if rot_per_tick != 0.0 { (core::f64::consts::TAU / rot_per_tick.abs()).round() } else { 0.0 };
    let t = if ticks_per_turn.is_finite() && ticks_per_turn >= 1.0 { (tick % ticks_per_turn as u64) as f64 } else { tick as f64 };
    wrap_tau(start + rot_per_tick * t)
}

fn wrap_tau(a: f64) -> f64 { crate::signature::wrap_tau(a) }

fn pos_f(x: f64) -> f64 { if x.is_finite() && x > 0.0 { x } else { 0.0 } }

fn emitter_kind(kind: SensorKind) -> bool { matches!(kind, SensorKind::SearchRadar | SensorKind::NavRadar | SensorKind::FireControlRadar | SensorKind::ActiveSonar) }

/// A new instance with its static facts; the per-tick fields are filled by `refresh`.
#[allow(clippy::too_many_arguments)]
fn new_instance(id: SensorRef, side: SideId, kind: SensorKind, r_ref_m: f64, rate: f64, rpm: Option<f64>, fov_half_deg: f64, emitter: u16, tick: u64, stagger: &dyn Fn(u64) -> u64) -> SensorInstance {
    let period = period_ticks(rate);
    let phase = stagger(id.key());
    let rot = rotation_per_tick(kind, rpm);
    let fov = if fov_half_deg.is_finite() && fov_half_deg > 0.0 { fov_half_deg.min(180.0).to_radians() } else { core::f64::consts::PI };
    let bearing = if rot != 0.0 { formula_bearing(phase, rot, tick) } else { 0.0 };
    SensorInstance {
        id, side, kind, r_ref_m: pos_f(r_ref_m), period_ticks: period, phase, rot_per_tick: rot, fov_half_rad: fov, emitter,
        pos: DVec3::ZERO, height_m: 0.0, bearing_rad: bearing, swept_from_rad: bearing, active: false, radiating: false,
        output: 1.0, error_factor: 1.0, own_speed_kn: 0.0, last_eval_tick: tick,
    }
}

/// Reference range of a sensor part, the spec's starting value when the part leaves it unset.
fn part_r_ref(kind: SensorKind, r_ref_m: f64, tuning: &content::tuning::SensorTuning) -> f64 {
    if r_ref_m.is_finite() && r_ref_m > 0.0 { return r_ref_m; }
    match kind {
        SensorKind::SearchRadar | SensorKind::NavRadar => DEFAULT_SEARCH_R_REF_M,
        SensorKind::FireControlRadar => DEFAULT_FIRE_CONTROL_R_REF_M,
        SensorKind::Seeker => DEFAULT_SEEKER_R_REF_M,
        SensorKind::PassiveSonar => tuning.passive_sonar_r_ref_m,
        SensorKind::ActiveSonar => tuning.active_sonar_r_ref_m,
        SensorKind::Visual | SensorKind::Camera => tuning.visual_ref_range_m,
        SensorKind::Esm => 0.0,
    }
}

/// Every sensor a carrier brings, ascending index. `stagger` is `sim_core::rng::stagger` (injected for tests).
fn carrier_sensors(world: &World, carrier: EntityId, ctx: &SimCtx<'_>, stagger: &dyn Fn(u64) -> u64) -> Vec<SensorInstance> {
    use content::part::PartFunction;
    use content::part::weapons::SeekerKind;
    let cat = ctx.catalogue;
    let tuning = &ctx.tuning.sensors;
    let tick = ctx.tick.0;
    let keys = crate::signature::emitter_keys(cat);
    let emitter_of = |key: &str| -> u16 {
        if key.is_empty() { return u16::MAX; }
        match keys.binary_search_by(|k| k.as_str().cmp(key)) { Ok(i) if i < u16::MAX as usize => i as u16, _ => u16::MAX }
    };
    let mut out = Vec::new();
    if let Some(v) = world.vessel(carrier) {
        for part in &v.parts {
            let Some((_, def)) = cat.parts.get_index(part.def.index()) else { continue };
            let id = SensorRef::part(v.id, part.slot);
            if id.index == LOOKOUT_INDEX { continue; }
            match &def.function {
                PartFunction::Sensor(s) => {
                    let emitter = if emitter_kind(s.kind) { emitter_of(&s.emitter) } else { u16::MAX };
                    out.push(new_instance(id, v.side, s.kind, part_r_ref(s.kind, s.r_ref_m, tuning), rate_hz(s.kind, s.rate_hz, tuning), s.rpm, s.fov_half_deg, emitter, tick, stagger));
                }
                PartFunction::CameraMast { fov_deg, .. } => {
                    out.push(new_instance(id, v.side, SensorKind::Camera, tuning.visual_ref_range_m, PASSIVE_HZ, None, fov_deg * 0.5, u16::MAX, tick, stagger));
                }
                _ => {}
            }
        }
        if v.crew.original_berths > 0 {
            out.push(new_instance(SensorRef { carrier: v.id, index: LOOKOUT_INDEX }, v.side, SensorKind::Visual, tuning.visual_ref_range_m, PASSIVE_HZ, None, 180.0, u16::MAX, tick, stagger));
        }
    } else {
        let b = world.bodies();
        if let Some(row) = b.row_of(carrier) {
            let side = b.side.get(row).copied().unwrap_or_default();
            if let Some((_, def)) = b.kind.get(row).and_then(|k| cat.munitions.get_index(k.index())) {
                for (i, s) in def.sensors.iter().enumerate() {
                    if i >= SensorRef::SEEKER as usize { break; }
                    let emitter = if emitter_kind(s.kind) { emitter_of(&s.emitter) } else { u16::MAX };
                    out.push(new_instance(SensorRef { carrier, index: i as u16 }, side, s.kind, part_r_ref(s.kind, s.r_ref_m, tuning), rate_hz(s.kind, s.rate_hz, tuning), s.rpm, s.fov_half_deg, emitter, tick, stagger));
                }
                if def.seeker != SeekerKind::None {
                    let fov = if def.seeker_fov_half_deg.is_finite() && def.seeker_fov_half_deg > 0.0 { def.seeker_fov_half_deg } else { ctx.tuning.guidance.seeker_fov_half_deg };
                    out.push(new_instance(SensorRef::seeker(carrier), side, SensorKind::Seeker, part_r_ref(SensorKind::Seeker, def.seeker_r_ref_m, tuning), FIRE_CONTROL_HZ, None, fov, u16::MAX, tick, stagger));
                }
            }
        }
    }
    out.sort_by_key(|s| s.id);
    out
}

/// Horizontal speed of a velocity in knots.
fn speed_kn(vel: DVec3) -> f64 {
    let v = (vel.x * vel.x + vel.z * vel.z).sqrt();
    if v.is_finite() { v / sim_core::time::KN } else { 0.0 }
}

/// Compass heading of a horizontal velocity, or `fallback` when still or invalid.
fn vel_heading(vel: DVec3, fallback: f64) -> f64 {
    if !(vel.x.is_finite() && vel.z.is_finite()) || (vel.x == 0.0 && vel.z == 0.0) { return fallback; }
    crate::signature::compass_bearing(vel)
}

/// Bearing of a rotating antenna this tick. A hull's part state carries the antenna phase relative to the
/// bow (sim-core turns it at 24 rpm); when it does not turn at about that rate (or the part state is missing)
/// the bearing advances from last tick's value by the nominal rotation so the sweep never stalls. Called once
/// per tick by `refresh`.
fn rotating_bearing(s: &SensorInstance, part_rel: Option<f64>, heading: f64, tick: u64) -> f64 {
    let fallback = if s.last_eval_tick <= tick { wrap_tau(s.bearing_rad + s.rot_per_tick) } else { formula_bearing(s.phase, s.rot_per_tick, tick) };
    let Some(rel) = part_rel.filter(|r| r.is_finite()) else { return fallback };
    let cand = wrap_tau(heading + rel);
    // Advance since last tick, in the direction of rotation.
    let adv = if s.rot_per_tick > 0.0 { wrap_tau(cand - s.bearing_rad) } else { wrap_tau(s.bearing_rad - cand) };
    let nominal = s.rot_per_tick.abs();
    // Accept the part's angle while it turns at roughly the nominal rate (a turning hull shifts it a little).
    if adv < 0.25 * nominal || adv > 4.0 * nominal { fallback } else { cand }
}

/// Compass-sense (clockwise from the bow) facing of a part. One yaw quarter turn carries the part's +X from the
/// bow to port (content::kit::rot), so it faces 270 degrees clockwise, not 90. Same axis rule as
/// `sim_core::parts::mount_zero_rad`, which is counter-clockwise.
pub(crate) fn part_facing_cw(rot: content::kit::PartRot) -> f64 { wrap_tau(-sim_core::parts::mount_zero_rad(rot)) }

/// Whether a hull's implicit lookout (L2446) keeps watch: the hull is afloat and was built with a crew.
///
/// DECISION (s4, 22 November 2026; the spec is silent on what stops a lookout): the watch does not stand down
/// with the berth count. L704 takes a berth out of the count while its compartment burns, floods past 50% or
/// is destroyed, and gives that count one effect, the time of damage-control jobs; L700 keeps crew as capacity
/// and damage about systems. The lookout stood down at `surviving_berths == 0`, so the first fire in a small
/// craft's one berth compartment blinded it for the rest of the fight: in op.act1.agent_extraction seed 0 the
/// insertion craft, sailing Silent, lost its 4 berths to a fire 9 s after the first hit, its tracks fell from
/// Fine to Bearing in 4 s, and its gun held on the grade after 22 rounds while 400 more hit it.
pub(crate) fn lookout_active(alive: bool, crew: &sim_core::crew::CrewState) -> bool { alive && crew.original_berths > 0 }

fn refresh_on_vessel(s: &mut SensorInstance, v: &sim_core::vessel::Vessel, ships: &ShipTable, ctx: &SimCtx<'_>, tick: u64) {
    use sim_core::parts::PartState;
    use sim_core::vessel::VesselStatus;
    let alive = matches!(v.status, VesselStatus::Active | VesselStatus::Disabled);
    let heading = v.kin.pose.heading_rad();
    let heading = if heading.is_finite() { heading } else { 0.0 };
    s.side = v.side;
    s.own_speed_kn = speed_kn(v.kin.lin_vel);
    if s.id.index == LOOKOUT_INDEX {
        // Lookout: on the highest console (the bridge), else the hull's top at mid-length.
        let cat = ctx.catalogue;
        let mut eye: Option<DVec3> = None;
        for p in &v.parts {
            if !matches!(cat.parts.get_index(p.def.index()).map(|(_, d)| &d.function), Some(content::part::PartFunction::Console)) { continue; }
            let w = v.to_world(p.centre);
            if w.is_finite() && eye.is_none_or(|e| w.y > e.y) { eye = Some(w); }
        }
        let pos = match eye {
            Some(e) => e + DVec3::new(0.0, LOOKOUT_EYE_M, 0.0),
            None => v.to_world(DVec3::new(v.dims[0] * 0.5, v.dims[1], v.dims[2] * 0.5)),
        };
        s.pos = if pos.is_finite() { pos } else { v.kin.pose.pos };
        s.height_m = if s.pos.y.is_finite() { s.pos.y } else { 0.0 };
        s.bearing_rad = heading;
        s.active = lookout_active(alive, &v.crew);
        s.radiating = false;
        s.output = 1.0;
        s.error_factor = 1.0;
        return;
    }
    let part = v.parts.get(s.id.index as usize).filter(|p| p.slot.0 == s.id.index).or_else(|| v.parts.iter().find(|p| p.slot.0 == s.id.index));
    let Some(part) = part else { s.active = false; s.radiating = false; return };
    let pos = v.to_world(part.centre);
    s.pos = if pos.is_finite() { pos } else { v.kin.pose.pos };
    s.height_m = if s.pos.y.is_finite() { s.pos.y } else { 0.0 };
    let (part_rel, train, radiating) = match part.state {
        PartState::Sensor { bearing_rad, radiating, train_rad, .. } => (Some(bearing_rad), train_rad, radiating),
        _ => (None, 0.0, false),
    };
    let facing = part_facing_cw(part.rot);
    s.bearing_rad = if s.rot_per_tick != 0.0 { rotating_bearing(s, part_rel, heading, tick) } else { wrap_tau(heading + facing + if train.is_finite() { train } else { 0.0 }) };
    let command = ships.get(v.id).and_then(|sh| sh.part_command.get(part.slot.0 as usize).copied());
    let (powered, error) = match command {
        Some(crate::datalink::PartCommand::Networked) => (true, 1.0),
        Some(crate::datalink::PartCommand::Local { error_factor }) => (true, error_factor),
        Some(crate::datalink::PartCommand::Dead) => (false, 1.0),
        None => {
            // No ship record (a hull outside the ship table): read the links directly.
            use sim_core::networks::LinkState;
            let powered = part.links.power != LinkState::Dead;
            let local = matches!(part.links.data, LinkState::LocalControl | LinkState::Dead);
            (powered, if local { ctx.tuning.networks.local_control_error_factor } else { 1.0 })
        }
    };
    let out = part.output_factor();
    s.output = if out.is_finite() && out > 0.0 { out } else { 0.0 };
    s.error_factor = if error.is_finite() && error >= 1.0 { error } else { 1.0 };
    let working = alive && part.works() && s.output > 0.0 && powered;
    s.radiating = working && emitter_kind(s.kind) && radiating;
    s.active = working && (!emitter_kind(s.kind) || s.radiating);
}

fn refresh_on_body(s: &mut SensorInstance, b: &sim_core::munitions::Bodies, row: usize, ctx: &SimCtx<'_>, tick: u64) {
    use content::part::weapons::SeekerKind;
    use sim_core::munitions::BodyPhase;
    let phase = b.phase.get(row).copied().unwrap_or(BodyPhase::Dead);
    let pos = b.pos.get(row).copied().unwrap_or(DVec3::ZERO);
    let vel = b.vel.get(row).copied().unwrap_or(DVec3::ZERO);
    let flying = !matches!(phase, BodyPhase::Passive | BodyPhase::Dead) && pos.is_finite();
    let body_radiating = b.radiating.get(row).copied().unwrap_or(false);
    s.side = b.side.get(row).copied().unwrap_or(s.side);
    s.pos = if pos.is_finite() { pos } else { DVec3::ZERO };
    s.height_m = s.pos.y;
    s.own_speed_kn = speed_kn(vel);
    s.output = 1.0;
    s.error_factor = 1.0;
    s.bearing_rad = if s.rot_per_tick != 0.0 { formula_bearing(s.phase, s.rot_per_tick, tick) } else { vel_heading(vel, s.bearing_rad) };
    if s.id.index == SensorRef::SEEKER {
        let seeker = b.kind.get(row).and_then(|k| ctx.catalogue.munitions.get_index(k.index())).map(|(_, d)| d.seeker).unwrap_or(SeekerKind::None);
        let (active, radiating) = match seeker {
            SeekerKind::ActiveRadar | SeekerKind::ActiveAcoustic => (flying && body_radiating, flying && body_radiating),
            SeekerKind::Infrared => (flying && phase == BodyPhase::Terminal, false),
            SeekerKind::None => (false, false),
        };
        s.active = active;
        s.radiating = radiating;
    } else {
        s.radiating = flying && emitter_kind(s.kind) && body_radiating;
        s.active = flying && (!emitter_kind(s.kind) || s.radiating);
    }
}

// ------------------------------------------------------------------------------------------------------------
// The step: due sensors, spatial cull, budget, laws, noise
// ------------------------------------------------------------------------------------------------------------

/// Inputs of one scheduler pass that do not come from the world.
pub(crate) struct StepPass<'a> {
    pub tick: u64,
    pub tuning: &'a content::tuning::SensorTuning,
    pub visibility_m: f64,
    pub budget_pairs: u32,
    /// Kinematic hulls, ascending (fallback 3).
    pub kinematic: &'a [EntityId],
    pub kinematic_2hz: bool,
}

/// Largest target values this tick: the upper bound of every law's reach, so the grid cull never drops a pair
/// a law would detect.
#[derive(Clone, Copy, Debug, Default)]
struct TargetMaxima { rcs: f64, top: f64, sub_area: f64, noise: f64, ir: f64, emit_h: f64, ping_r_ref: f64 }
impl TargetMaxima {
    fn of(rows: &[Signature]) -> Self {
        let mut m = TargetMaxima::default();
        for r in rows {
            if !r.pos.is_finite() { continue; }
            let (rcs, sub) = match (&r.kind, &r.areas) {
                (crate::signature::SigKind::Vessel | crate::signature::SigKind::Wreck, Some(a)) => (max36(&a.rcs_m2), max36(&a.submerged_m2)),
                (crate::signature::SigKind::Body(_), Some(a)) => (pos_f(r.rcs_flat_m2), max36(&a.submerged_m2)),
                _ => (pos_f(r.rcs_flat_m2), 0.0),
            };
            m.rcs = m.rcs.max(rcs);
            m.sub_area = m.sub_area.max(sub);
            m.top = m.top.max(pos_f(r.top_m));
            m.noise = m.noise.max(pos_f(r.noise));
            m.ir = m.ir.max(pos_f(r.ir_mw));
            for e in &r.emissions {
                m.emit_h = m.emit_h.max(pos_f(e.height_m));
                if e.kind == crate::signature::EmitterKind::ActiveSonar { m.ping_r_ref = m.ping_r_ref.max(pos_f(e.r_ref_m)); }
            }
        }
        m
    }
}
fn max36(a: &[f32; 36]) -> f64 { a.iter().fold(0.0f64, |m, &x| if (x as f64).is_finite() { m.max(x as f64) } else { m }) }

/// Upper bound on the horizontal distance at which this sensor can detect anything this tick, metres.
fn reach_bound(s: &SensorInstance, m: &TargetMaxima, tuning: &content::tuning::SensorTuning, visibility_m: f64) -> f64 {
    use radar::horizon_m;
    let out = if s.output.is_finite() { s.output.max(1.0) } else { 1.0 };
    let sigma_ref = if tuning.sigma_ref_m2.is_finite() && tuning.sigma_ref_m2 > 0.0 { tuning.sigma_ref_m2 } else { 100.0 };
    let horizon = horizon_m(s.height_m, m.top);
    let radar = horizon.min(s.r_ref_m * libm::pow(m.rcs / sigma_ref, 0.25) * out);
    let active = s.r_ref_m * libm::pow(m.sub_area / sonar::ACTIVE_REF_AREA_M2, 0.25) * out;
    let b = match s.kind {
        SensorKind::SearchRadar | SensorKind::NavRadar | SensorKind::FireControlRadar => radar,
        SensorKind::Seeker => radar.max(horizon.min(s.r_ref_m * (m.ir / radar::IR_REF_MW).sqrt() * out)).max(active),
        SensorKind::Esm => horizon_m(s.height_m, m.emit_h),
        SensorKind::Visual | SensorKind::Camera => pos_f(visibility_m).min(horizon),
        SensorKind::PassiveSonar => (s.r_ref_m * (m.noise / sonar::N_REF).sqrt() * out).max(sonar::PING_INTERCEPT_FACTOR * m.ping_r_ref * out),
        SensorKind::ActiveSonar => active,
    };
    let b = pos_f(b);
    // A hair of slack so rounding never culls a pair the law keeps.
    b * (1.0 + 1e-9) + 1.0
}

impl SpatialGrid {
    /// Rebuild from signature rows (indices into `rows`), cell size from tuning (at least 100 m).
    pub(crate) fn rebuild(&mut self, rows: &[Signature], cell_m: f64) {
        self.cell_m = if cell_m.is_finite() && cell_m >= 100.0 { cell_m } else { 5_000.0 };
        self.cells.clear();
        for (i, r) in rows.iter().enumerate() {
            if !r.pos.is_finite() || i > u32::MAX as usize { continue; }
            self.cells.entry(self.cell_of(r.pos.x, r.pos.z)).or_default().push(i as u32);
        }
    }
    fn coord(&self, v: f64) -> i32 { let c = (v / self.cell_m).floor(); if c.is_finite() { c.clamp(i32::MIN as f64, i32::MAX as f64) as i32 } else { 0 } }
    fn cell_of(&self, x: f64, z: f64) -> (i32, i32) { (self.coord(x), self.coord(z)) }
    /// Row indices whose cell overlaps the square of half-side `r` around (x, z), ascending. Walks the occupied
    /// cells instead of the square when the square spans more cells than are occupied.
    pub(crate) fn query(&self, x: f64, z: f64, r: f64, out: &mut Vec<u32>) {
        out.clear();
        if !(x.is_finite() && z.is_finite()) || !(r >= 0.0) { return; }
        let r = if r.is_finite() { r } else { f64::MAX / 4.0 };
        let (x0, x1, z0, z1) = (self.coord(x - r), self.coord(x + r), self.coord(z - r), self.coord(z + r));
        let span = (x1 as i64 - x0 as i64 + 1).saturating_mul(z1 as i64 - z0 as i64 + 1);
        if span > self.cells.len() as i64 {
            for (&(cx, cz), v) in &self.cells {
                if cx >= x0 && cx <= x1 && cz >= z0 && cz <= z1 { out.extend_from_slice(v); }
            }
        } else {
            for cx in x0..=x1 {
                for cz in z0..=z1 {
                    if let Some(v) = self.cells.get(&(cx, cz)) { out.extend_from_slice(v); }
                }
            }
        }
        out.sort_unstable();
    }
}

/// Add the measurement noise of a detection in place: 1-sigma range and bearing errors as the law set them
/// (spec values times the local-control factor), height with the bearing error as an elevation error.
/// `n(draw)` is a standard normal deviate for the pair; draws 0, 2 and 4 are used (Box-Muller takes pairs).
pub(crate) fn apply_noise(det: &mut Detection, n: &dyn Fn(u32) -> f64) {
    let g = |d: u32| { let v = n(d); if v.is_finite() { v.clamp(-8.0, 8.0) } else { 0.0 } };
    let sr = pos_f(det.sigma_range_m);
    let sb = pos_f(det.sigma_bearing_rad);
    match &mut det.measurement {
        Measurement::RangeBearing { range_m, bearing_rad, height_m } => {
            let r = pos_f(*range_m);
            *range_m = (r + sr * g(0)).max(0.0);
            *bearing_rad = wrap_tau(*bearing_rad + sb * g(2));
            if let Some(h) = height_m.as_mut() { *h = if h.is_finite() { *h + r * sb * g(4) } else { 0.0 }; }
        }
        Measurement::BearingOnly { bearing_rad } => { *bearing_rad = wrap_tau(*bearing_rad + sb * g(0)); }
    }
}

impl SensorScheduler {
    /// The world-free scheduler pass. `detect` is the law dispatch, `normal(sensor, target, draw)` the noise.
    pub(crate) fn step_core(&mut self, sigs: &SignatureTable, pass: &StepPass<'_>, detect: &dyn Fn(&SensorInstance, &Signature) -> Option<Detection>, normal: &dyn Fn(SensorRef, EntityId, u32) -> f64, out: &mut Vec<Detection>) {
        let tick = pass.tick;
        let rows = &sigs.rows;
        let first_out = out.len();
        self.grid.rebuild(rows, pass.tuning.spatial_grid_cell_m);
        let maxima = TargetMaxima::of(rows);

        // Due sensors: overdue (deferred) ones first, oldest slot first, then on-time ones in ascending order.
        let mut due: Vec<(u64, usize)> = Vec::new();
        for (i, s) in self.sensors.iter().enumerate() {
            if let Some(last) = last_due_tick(tick, s.phase, s.period_ticks.max(1)) {
                if last > s.last_eval_tick || (s.last_eval_tick > tick && last == tick) { due.push((last, i)); }
            }
        }
        due.sort_by_key(|&(last, i)| (last >= tick, last, i));

        let mut pairs: u32 = 0;
        let mut ran_any = false;
        let mut cand: Vec<u32> = Vec::new();
        let mut keep: Vec<u32> = Vec::new();
        for &(_, i) in &due {
            let s = &self.sensors[i];
            if s.active && s.pos.is_finite() {
                let bound = reach_bound(s, &maxima, pass.tuning, pass.visibility_m);
                self.grid.query(s.pos.x, s.pos.z, bound, &mut cand);
                let thin_kinematic = pass.kinematic_2hz && s.rot_per_tick == 0.0 && (s.period_ticks as u64) < KINEMATIC_PAIR_PERIOD
                    && residue(tick, s.phase, KINEMATIC_PAIR_PERIOD) >= s.period_ticks as u64
                    && pass.kinematic.binary_search(&s.id.carrier).is_ok();
                keep.clear();
                for &ri in &cand {
                    let Some(t) = rows.get(ri as usize) else { continue };
                    if t.side == s.side || t.entity == s.id.carrier { continue; }
                    let (dx, dz) = (t.pos.x - s.pos.x, t.pos.z - s.pos.z);
                    let dh = (dx * dx + dz * dz).sqrt();
                    if !(dh <= bound) { continue; }
                    if thin_kinematic && pass.kinematic.binary_search(&t.entity).is_ok() { continue; }
                    keep.push(ri);
                }
                let n = keep.len().min(u32::MAX as usize) as u32;
                if ran_any && pairs.saturating_add(n) > pass.budget_pairs { continue; }
                ran_any = true;
                pairs = pairs.saturating_add(n);
                for &ri in &keep {
                    let t = &rows[ri as usize];
                    // DECISION (q5): decoys work on weapon seekers only (L524, L601, L616). A ship's radar, ESM,
                    // lookouts and sonar build no track from chaff, flares, smoke or a towed decoy: chaff laid
                    // against a missile took over the gun's designated surface track for its whole 60 s life,
                    // and the gun shot at an empty patch of sea while the target sailed off.
                    if matches!(t.kind, crate::signature::SigKind::Decoy(_)) && s.kind != SensorKind::Seeker { continue; }
                    if let Some(mut det) = detect(s, t) {
                        let (sid, tid) = (s.id, t.entity);
                        apply_noise(&mut det, &|d| normal(sid, tid, d));
                        out.push(det);
                    }
                }
            }
            // Advance the sector and the evaluation clock (also for sensors that are off, so a radar switched
            // on sees only what it sweeps from then on).
            let s = &mut self.sensors[i];
            s.swept_from_rad = s.bearing_rad;
            s.last_eval_tick = tick;
        }
        out[first_out..].sort_by(|a, b| (a.sensor, a.target).cmp(&(b.sensor, b.target)));
        self.pairs_last_tick = pairs;
    }
}

/// Dispatch one pair to its law. Real code: the routing is contract.
pub fn detect(sensor: &SensorInstance, target: &Signature, env: &SenseEnv<'_>) -> Option<Detection> {
    match sensor.kind {
        SensorKind::SearchRadar | SensorKind::NavRadar | SensorKind::FireControlRadar | SensorKind::Seeker => radar::detect_radar(sensor, target, env),
        SensorKind::Esm => radar::detect_esm(sensor, target, env),
        SensorKind::Visual | SensorKind::Camera => optical::detect_visual(sensor, target, env),
        SensorKind::PassiveSonar => sonar::detect_passive(sensor, target, env),
        SensorKind::ActiveSonar => sonar::detect_active(sensor, target, env),
    }
}

/// Clutter C for a sea state: 0 calm, 0.5 at Beaufort 4, 2.0 at Beaufort 7, linear between the three anchors
/// by significant wave height (L427). Unit: `sys-sensors`.
///
/// Beyond Beaufort 7 the coefficient holds at the Beaufort 7 value: the spec anchors stop there and the
/// L438 gate (sea-skimmer at 5 km or more) is set at Beaufort 7.
pub fn clutter_for(weather: &WeatherState, tuning: &content::tuning::SensorTuning) -> f64 {
    use content::weather::SeaState;
    let hs = weather.sea_state.significant_height_m();
    let anchors = [(SeaState::CALM.significant_height_m(), tuning.clutter[0]), (SeaState::BEAUFORT_4.significant_height_m(), tuning.clutter[1]), (SeaState::BEAUFORT_7.significant_height_m(), tuning.clutter[2])];
    let c = if !hs.is_finite() || hs <= anchors[0].0 {
        anchors[0].1
    } else if hs >= anchors[2].0 {
        anchors[2].1
    } else {
        let (a, b) = if hs <= anchors[1].0 { (anchors[0], anchors[1]) } else { (anchors[1], anchors[2]) };
        if b.0 > a.0 { a.1 + (b.1 - a.1) * (hs - a.0) / (b.0 - a.0) } else { b.1 }
    };
    pos_f(c)
}

#[cfg(test)]
mod tests {
    use super::radar::tests::{hull_at, sensor};
    use super::*;
    use crate::signature::{Emission, EmitterKind, SigKind};
    use content::tuning::SensorTuning;
    use content::weather::SeaState;

    fn tn() -> SensorTuning { SensorTuning::default() }
    fn eid(i: u32) -> EntityId { EntityId { index: i, generation: 0 } }

    /// A part's compass facing follows content's yaw rule: one quarter turn carries its +X from the bow to port.
    #[test]
    fn the_lookout_keeps_watch_with_every_berth_out_of_the_count() {
        use sim_core::crew::CrewState;
        // L704 takes berths out of the damage-control count while their compartment burns; the watch stays.
        let burning = CrewState { original_berths: 4, surviving_berths: 0, ..Default::default() };
        assert!(lookout_active(true, &burning), "a crewed hull afloat keeps its lookout with 0 of 4 berths in the count");
        assert!(lookout_active(true, &CrewState { original_berths: 4, surviving_berths: 4, ..Default::default() }));
        // A hull that is lost, or one built with no crew (a drone boat), has none.
        assert!(!lookout_active(false, &burning));
        assert!(!lookout_active(true, &CrewState::default()));
    }

    #[test]
    fn part_facing_is_compass_sense() {
        use content::kit::PartRot;
        let deg = |yaw: u8| part_facing_cw(PartRot { yaw, tilt: 0 }).to_degrees();
        assert!(deg(0).abs() < 1e-9);
        assert!((deg(1) - 270.0).abs() < 1e-9, "yaw 1 faces port: {}", deg(1));
        assert!((deg(2) - 180.0).abs() < 1e-9);
        assert!((deg(3) - 90.0).abs() < 1e-9, "yaw 3 faces starboard: {}", deg(3));
        // The same rule as content's rotation matrix: the facing points along the rotated +X.
        for yaw in 0..4u8 {
            let m = content::kit::part_rot_matrix(PartRot { yaw, tilt: 0 });
            let (x, z) = (m[0][0] as f64, m[0][2] as f64);
            let a = part_facing_cw(PartRot { yaw, tilt: 0 });
            assert!((libm::cos(a) - x).abs() < 1e-9 && (libm::sin(a) - z).abs() < 1e-9, "yaw {yaw}");
        }
    }

    /// The world-free law dispatch (no line of sight): what `detect` does minus the terrain and smoke tests.
    fn core_detect(s: &SensorInstance, t: &Signature, tick: u64, tuning: &SensorTuning) -> Option<Detection> {
        match s.kind {
            SensorKind::SearchRadar | SensorKind::NavRadar | SensorKind::FireControlRadar | SensorKind::Seeker => radar::radar_core(s, t, tick, 0.5, tuning),
            SensorKind::Esm => radar::esm_core(s, t, tick, tuning).map(|d| d.0),
            SensorKind::Visual | SensorKind::Camera => optical::visual_core(s, t, tick, false, 20_000.0, tuning).map(|(r, b, law)| optical::sighting(s, t, tick, r, b, law)),
            SensorKind::PassiveSonar => sonar::passive_core(s, t, tick, 50.0, tuning, &|| 200.0),
            SensorKind::ActiveSonar => sonar::active_core(s, t, tick, 50.0, tuning, &|| 200.0),
        }
    }

    fn table(rows: Vec<Signature>) -> SignatureTable {
        let mut rows = rows;
        rows.sort_by_key(|r| r.entity);
        SignatureTable { rows, ..Default::default() }
    }

    fn pass<'a>(tick: u64, tuning: &'a SensorTuning, budget: u32, kin: &'a [EntityId], kin2: bool) -> StepPass<'a> {
        StepPass { tick, tuning, visibility_m: 20_000.0, budget_pairs: budget, kinematic: kin, kinematic_2hz: kin2 }
    }

    fn run(sch: &mut SensorScheduler, sigs: &SignatureTable, p: &StepPass<'_>, out: &mut Vec<Detection>) {
        let tuning = p.tuning;
        let tick = p.tick;
        sch.step_core(sigs, p, &|s, t| core_detect(s, t, tick, tuning), &|_, _, _| 0.0, out);
    }

    fn hull(i: u32, x: f64, z: f64, rcs: f32) -> Signature {
        let mut s = hull_at(0.0, rcs, 100.0, 50.0, 10.0);
        s.entity = eid(i);
        s.pos = DVec3::new(x, 0.0, z);
        s
    }

    #[test]
    fn clutter_anchors_and_interpolation() {
        let t = tn();
        let mut w = WeatherState { sea_state: SeaState::CALM, ..Default::default() };
        assert_eq!(clutter_for(&w, &t), 0.0);
        w.sea_state = SeaState::BEAUFORT_4;
        assert!((clutter_for(&w, &t) - 0.5).abs() < 1e-12);
        w.sea_state = SeaState::BEAUFORT_7;
        assert!((clutter_for(&w, &t) - 2.0).abs() < 1e-12);
        // Code 4 (Hs 1.9 m) lies between Beaufort 4 (0.9 m) and Beaufort 7 (5.0 m).
        w.sea_state = SeaState(4);
        assert!((clutter_for(&w, &t) - (0.5 + 1.5 * 1.0 / 4.1)).abs() < 1e-12);
        // Code 1 (0.1 m) between calm and Beaufort 4; above Beaufort 7 holds.
        w.sea_state = SeaState(1);
        assert!((clutter_for(&w, &t) - 0.5 * 0.1 / 0.9).abs() < 1e-12);
        w.sea_state = SeaState(8);
        assert!((clutter_for(&w, &t) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn rates_periods_and_fallback_one() {
        let mut t = tn();
        assert_eq!(period_ticks(rate_hz(SensorKind::SearchRadar, 5.0, &t)), 12);
        assert_eq!(period_ticks(rate_hz(SensorKind::FireControlRadar, 1.0, &t)), 6);
        assert_eq!(period_ticks(rate_hz(SensorKind::Seeker, 0.0, &t)), 6);
        for k in [SensorKind::Esm, SensorKind::PassiveSonar, SensorKind::ActiveSonar, SensorKind::Visual, SensorKind::Camera] {
            assert_eq!(period_ticks(rate_hz(k, 9.0, &t)), 30);
        }
        // Designer rates 2..=10 Hz; out of range clamps.
        assert_eq!(period_ticks(rate_hz(SensorKind::NavRadar, 10.0, &t)), 6);
        assert_eq!(period_ticks(rate_hz(SensorKind::NavRadar, 50.0, &t)), 6);
        assert_eq!(period_ticks(rate_hz(SensorKind::NavRadar, 1.0, &t)), 30);
        // Fallback 1: the default search rate drops to 3 Hz; designer-set rates and fire control keep theirs.
        t.search_radar_hz = 3.0;
        assert_eq!(period_ticks(rate_hz(SensorKind::SearchRadar, 5.0, &t)), 20);
        assert_eq!(period_ticks(rate_hz(SensorKind::SearchRadar, f64::NAN, &t)), 20);
        assert_eq!(period_ticks(rate_hz(SensorKind::SearchRadar, 8.0, &t)), 8);
        assert_eq!(period_ticks(rate_hz(SensorKind::FireControlRadar, 5.0, &t)), 6);
        assert_eq!(period_ticks(f64::NAN), 60);
        // 24 rpm = one turn per 150 ticks.
        let rot = rotation_per_tick(SensorKind::SearchRadar, None);
        assert!((rot * 150.0 - core::f64::consts::TAU).abs() < 1e-12);
        assert_eq!(rotation_per_tick(SensorKind::FireControlRadar, Some(30.0)), 0.0);
    }

    #[test]
    fn staggered_schedule() {
        assert_eq!(last_due_tick(100, 5, 12), Some(91));
        assert_eq!(last_due_tick(7, 5, 12), Some(7));
        assert_eq!(last_due_tick(3, 5, 12), None);
        assert_eq!(last_due_tick(10, u64::MAX, 12), Some(10 - ((10 % 12 + u64::MAX % 12) % 12)));
        let t = tn();
        let mut s = sensor(SensorKind::SearchRadar);
        s.period_ticks = 12;
        s.phase = 5;
        s.last_eval_tick = 0;
        let mut sch = SensorScheduler { sensors: vec![s], ..Default::default() };
        let sigs = table(vec![]);
        let mut evals = Vec::new();
        for tick in 1..=600u64 {
            let before = sch.sensors[0].last_eval_tick;
            run(&mut sch, &sigs, &pass(tick, &t, 1_000, &[], false), &mut Vec::new());
            if sch.sensors[0].last_eval_tick != before { evals.push(tick); }
        }
        assert_eq!(evals.len(), 50);
        assert!(evals.iter().all(|&e| (e + 5) % 12 == 0), "{evals:?}");
    }

    #[test]
    fn budget_defers_and_deferred_run_first() {
        let t = tn();
        let mut sensors = Vec::new();
        for i in 0..3u16 {
            let mut s = sensor(SensorKind::FireControlRadar);
            s.id = SensorRef { carrier: eid(100 + i as u32), index: 0 };
            s.period_ticks = 6;
            s.phase = 0;
            sensors.push(s);
        }
        let mut sch = SensorScheduler { sensors, ..Default::default() };
        let rows: Vec<Signature> = (0..10).map(|i| hull(i, 1_000.0 * i as f64, -5_000.0, 150.0)).collect();
        let sigs = table(rows);
        let mut out = Vec::new();
        // Tick 6: all three due; 10 pairs each against a budget of 15: only the first runs.
        run(&mut sch, &sigs, &pass(6, &t, 15, &[], false), &mut out);
        assert_eq!(sch.pairs_last_tick, 10);
        assert!(out.iter().all(|d| d.sensor.carrier == eid(100)));
        assert_eq!(out.len(), 10);
        // Tick 7: the deferred ones go first, one per tick under this budget.
        out.clear();
        run(&mut sch, &sigs, &pass(7, &t, 15, &[], false), &mut out);
        assert!(out.iter().all(|d| d.sensor.carrier == eid(101)) && out.len() == 10);
        out.clear();
        run(&mut sch, &sigs, &pass(8, &t, 15, &[], false), &mut out);
        assert!(out.iter().all(|d| d.sensor.carrier == eid(102)) && out.len() == 10);
        // Output order is (sensor, target) ascending.
        assert!(out.windows(2).all(|w| (w[0].sensor, w[0].target) < (w[1].sensor, w[1].target)));
        // Tick 12: everyone on time again with a big budget.
        out.clear();
        run(&mut sch, &sigs, &pass(12, &t, 1_000, &[], false), &mut out);
        assert_eq!(out.len(), 30);
        assert_eq!(sch.pairs_last_tick, 30);
    }

    #[test]
    fn rotating_radar_one_detection_per_rotation() {
        let t = tn();
        let mut s = sensor(SensorKind::SearchRadar);
        s.rot_per_tick = rotation_per_tick(SensorKind::SearchRadar, None);
        s.period_ticks = 12;
        s.phase = 3;
        s.bearing_rad = formula_bearing(s.phase, s.rot_per_tick, 0);
        s.swept_from_rad = s.bearing_rad;
        let mut sch = SensorScheduler { sensors: vec![s], ..Default::default() };
        let rows: Vec<Signature> = (0..24).map(|i| {
            let b = (i as f64 * 15.0 + 0.37).to_radians();
            hull(i + 10, 10_000.0 * libm::sin(b), -10_000.0 * libm::cos(b), 150.0)
        }).collect();
        let sigs = table(rows);
        let mut hits: BTreeMap<EntityId, Vec<u64>> = BTreeMap::new();
        for tick in 1..=1_500u64 {
            let sp = &mut sch.sensors[0];
            sp.bearing_rad = formula_bearing(sp.phase, sp.rot_per_tick, tick);
            let mut out = Vec::new();
            run(&mut sch, &sigs, &pass(tick, &t, 10_000, &[], false), &mut out);
            for d in out { hits.entry(d.target).or_default().push(d.tick); }
        }
        assert_eq!(hits.len(), 24);
        for (e, ticks) in &hits {
            assert!(ticks.len() >= 9 && ticks.len() <= 10, "{e:?} {ticks:?}");
            for w in ticks.windows(2) { assert!((w[1] as i64 - w[0] as i64 - 150).abs() <= 12, "{e:?} {ticks:?}"); }
        }
    }

    /// Fill a varied scene: hulls, sea-skimmers, aircraft, a jammer, emitters, a torpedo and a pinging sonar.
    fn scene() -> Vec<Signature> {
        let mut rows = Vec::new();
        let mut seed = 0x1234_5678_9abc_def0u64;
        let mut next = || { seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); ((seed >> 11) as f64) / ((1u64 << 53) as f64) };
        for i in 0..80u32 {
            let (x, z) = ((next() - 0.5) * 120_000.0, (next() - 0.5) * 120_000.0);
            let mut s = hull(i + 1, x, z, (10.0 + next() * 8_000.0) as f32);
            s.side = SideId(1 + (i % 3) as u8);
            match i % 8 {
                1 => { s.kind = SigKind::Body(MunitionIdx(0)); s.rcs_flat_m2 = 0.1; s.top_m = 5.0; s.pos.y = 5.0; s.domain = Domain::Weapon; }
                2 => { s.kind = SigKind::Body(MunitionIdx(1)); s.rcs_flat_m2 = 5.0; s.top_m = 3_000.0; s.pos.y = 3_000.0; s.domain = Domain::Air;
                       s.emissions.push(Emission { source: SensorRef::seeker(s.entity), kind: EmitterKind::SearchRadar, emitter: 2, r_ref_m: 60_000.0, height_m: 3_000.0 }); }
                3 => { s.jammer_r_bt0_m = Some(10_000.0); s.emissions.push(Emission { source: SensorRef { carrier: s.entity, index: 3 }, kind: EmitterKind::Jammer, emitter: u16::MAX, r_ref_m: crate::signature::JAMMER_ESM_R_REF_M, height_m: 15.0 }); }
                4 => { s.emissions.push(Emission { source: SensorRef { carrier: s.entity, index: 1 }, kind: EmitterKind::SearchRadar, emitter: 1, r_ref_m: 40_000.0, height_m: 25.0 }); s.noise = 2.0; }
                5 => { s.kind = SigKind::Body(MunitionIdx(2)); s.domain = Domain::Weapon; s.pos.y = -8.0; s.top_m = 0.0; s.depth_m = 8.0; s.noise = 1.0; }
                6 => { s.emissions.push(Emission { source: SensorRef { carrier: s.entity, index: 2 }, kind: EmitterKind::ActiveSonar, emitter: 4, r_ref_m: 4_000.0, height_m: -4.0 }); s.noise = 0.5; }
                _ => { s.noise = next() * 3.0; }
            }
            s.heading_rad = next() * core::f64::consts::TAU;
            rows.push(s);
        }
        rows
    }

    fn scene_sensors() -> Vec<SensorInstance> {
        let kinds = [SensorKind::SearchRadar, SensorKind::FireControlRadar, SensorKind::Esm, SensorKind::Visual, SensorKind::Camera, SensorKind::PassiveSonar, SensorKind::ActiveSonar, SensorKind::NavRadar];
        let mut v = Vec::new();
        for (i, k) in kinds.iter().enumerate() {
            let mut s = sensor(*k);
            s.id = SensorRef { carrier: eid(500 + i as u32), index: 0 };
            s.side = SideId(1);
            s.period_ticks = 1;
            s.pos = DVec3::new(-20_000.0 + 6_000.0 * i as f64, 20.0, 7_000.0 - 3_000.0 * i as f64);
            s.r_ref_m = match k { SensorKind::PassiveSonar => 8_000.0, SensorKind::ActiveSonar => 4_000.0, SensorKind::FireControlRadar => 25_000.0, _ => 40_000.0 };
            if matches!(k, SensorKind::PassiveSonar | SensorKind::ActiveSonar) { s.pos.y = -4.0; s.height_m = -4.0; }
            v.push(s);
        }
        v
    }

    #[test]
    fn grid_cull_never_changes_results() {
        let rows = scene();
        let sigs = table(rows.clone());
        let tick = 77;
        for cell in [2_000.0, 5_000.0, 10_000.0, 20_000.0, 100.0, 1.0e6] {
            let mut t = tn();
            t.spatial_grid_cell_m = cell;
            let sensors = scene_sensors();
            // Brute force over every pair.
            let mut brute = Vec::new();
            for s in &sensors {
                for r in &sigs.rows {
                    if r.side == s.side { continue; }
                    if let Some(d) = core_detect(s, r, tick, &t) { brute.push((d.sensor, d.target)); }
                }
            }
            brute.sort();
            assert!(brute.len() > 40, "scene too sparse: {}", brute.len());
            let mut sch = SensorScheduler { sensors, ..Default::default() };
            for s in &mut sch.sensors { s.last_eval_tick = tick - 1; }
            let mut out = Vec::new();
            run(&mut sch, &sigs, &pass(tick, &t, u32::MAX, &[], false), &mut out);
            let got: Vec<(SensorRef, EntityId)> = out.iter().map(|d| (d.sensor, d.target)).collect();
            assert_eq!(got, brute, "cell {cell}");
            // Same-side pairs were never checked; the cull checked fewer pairs than all of them.
            assert!(out.iter().all(|d| sigs.get(d.target).unwrap().side != SideId(1)));
            assert!((sch.pairs_last_tick as usize) <= sch.sensors.len() * sigs.rows.len());
        }
    }

    #[test]
    fn noise_is_added_by_the_scheduler() {
        let s = sensor(SensorKind::FireControlRadar);
        let target = hull_at(10_000.0, 150.0, 0.0, 0.0, 10.0);
        let mut det = radar::radar_core(&s, &target, 5, 0.0, &tn()).unwrap();
        let before = det;
        apply_noise(&mut det, &|d| match d { 0 => 1.0, 2 => -2.0, 4 => 0.5, _ => 99.0 });
        match (before.measurement, det.measurement) {
            (Measurement::RangeBearing { range_m: r0, bearing_rad: b0, height_m: Some(h0) }, Measurement::RangeBearing { range_m: r1, bearing_rad: b1, height_m: Some(h1) }) => {
                assert!((r1 - r0 - 5.0).abs() < 1e-9);
                assert!((radar::wrap_pi(b1 - b0) + 2.0 * 0.05f64.to_radians()).abs() < 1e-12);
                assert!((h1 - h0 - 0.5 * r0 * 0.05f64.to_radians()).abs() < 1e-9);
            }
            other => panic!("{other:?}"),
        }
        // Bad deviates are ignored; range never goes negative; bearing wraps.
        let mut d2 = before;
        apply_noise(&mut d2, &|_| f64::NAN);
        assert_eq!(d2.measurement, before.measurement);
        let mut d3 = before;
        d3.sigma_range_m = 1e9;
        apply_noise(&mut d3, &|_| -1.0);
        if let Measurement::RangeBearing { range_m, bearing_rad, .. } = d3.measurement { assert_eq!(range_m, 0.0); assert!((0.0..core::f64::consts::TAU).contains(&bearing_rad)); }
    }

    #[test]
    fn ship_sensors_build_no_track_from_decoys() {
        let t = tn();
        let mut s = sensor(SensorKind::FireControlRadar);
        s.id.carrier = eid(900);
        let mut sch = SensorScheduler { sensors: vec![s], ..Default::default() };
        let mut chaff = hull(2, 100.0, -5_000.0, 1_000.0);
        chaff.kind = crate::signature::SigKind::Decoy(content::part::DecoyKind::Chaff);
        let sigs = table(vec![hull(1, 0.0, -5_000.0, 150.0), chaff]);
        let mut seen: Vec<EntityId> = Vec::new();
        for tick in 1..=30u64 {
            let mut out = Vec::new();
            run(&mut sch, &sigs, &pass(tick, &t, 10_000, &[], false), &mut out);
            seen.extend(out.iter().map(|d| d.target));
        }
        assert!(seen.contains(&eid(1)), "the hull is seen");
        assert!(!seen.contains(&eid(2)), "the chaff cloud is not");
    }

    #[test]
    fn kinematic_pairs_run_at_2hz() {
        let t = tn();
        let mut s = sensor(SensorKind::FireControlRadar);
        s.period_ticks = 6;
        s.phase = 1;
        s.id.carrier = eid(900);
        let mut sch = SensorScheduler { sensors: vec![s], ..Default::default() };
        let sigs = table(vec![hull(1, 0.0, -5_000.0, 150.0), hull(2, 100.0, -6_000.0, 150.0)]);
        let kin = [eid(1), eid(900)];
        let mut n: BTreeMap<EntityId, u32> = BTreeMap::new();
        for tick in 1..=120u64 {
            let mut out = Vec::new();
            run(&mut sch, &sigs, &pass(tick, &t, 10_000, &kin, true), &mut out);
            for d in out { *n.entry(d.target).or_default() += 1; }
        }
        assert_eq!(n[&eid(1)], 4, "kinematic pair at 2 Hz over 2 s");
        assert_eq!(n[&eid(2)], 20, "full-physics target keeps 10 Hz");
    }

    #[test]
    fn switched_off_radar_still_turns() {
        let t = tn();
        let mut s = sensor(SensorKind::SearchRadar);
        s.rot_per_tick = rotation_per_tick(SensorKind::SearchRadar, None);
        s.period_ticks = 12;
        s.active = false;
        s.radiating = false;
        let mut sch = SensorScheduler { sensors: vec![s], ..Default::default() };
        let sigs = table(vec![hull(1, 0.0, -10_000.0, 150.0)]);
        for tick in 1..=600u64 {
            sch.sensors[0].bearing_rad = formula_bearing(0, sch.sensors[0].rot_per_tick, tick);
            run(&mut sch, &sigs, &pass(tick, &t, 10_000, &[], false), &mut Vec::new());
        }
        // Switched on at tick 600: the first evaluation sees only the 12 ticks swept since tick 600.
        let s = &mut sch.sensors[0];
        assert_eq!(s.last_eval_tick, 600);
        s.active = true;
        s.radiating = true;
        let swept = (s.bearing_rad + 12.0 * s.rot_per_tick) - s.swept_from_rad;
        assert!((swept - 12.0 * s.rot_per_tick).abs() < 1e-9);
    }

    #[test]
    fn antenna_follows_part_state_and_never_stalls() {
        let mut s = sensor(SensorKind::SearchRadar);
        s.rot_per_tick = rotation_per_tick(SensorKind::SearchRadar, None);
        s.bearing_rad = 1.0;
        s.last_eval_tick = 0;
        // A turning part state is followed (heading 0.5 plus relative phase).
        let b = rotating_bearing(&s, Some(0.5 + s.rot_per_tick), 0.5, 10);
        assert!((b - (1.0 + s.rot_per_tick)).abs() < 1e-12);
        // A stuck or missing one: the sweep advances at the nominal rate, without jumps.
        let mut prev = s.bearing_rad;
        for tick in 1..=300u64 {
            s.bearing_rad = prev;
            let b = rotating_bearing(&s, Some(0.5), 0.0, tick);
            let adv = crate::signature::wrap_tau(b - prev);
            assert!(adv <= 4.0 * s.rot_per_tick + 1e-12, "tick {tick}: jumped {adv}");
            prev = b;
        }
        s.bearing_rad = prev;
        assert!((rotating_bearing(&s, None, 0.0, 301) - wrap_tau(prev + s.rot_per_tick)).abs() < 1e-12);
    }
}
