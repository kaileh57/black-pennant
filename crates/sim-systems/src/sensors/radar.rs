//! Unit `sys-detect-radar`. Radar, seeker and ESM detection laws, closed form. DESIGN.md L421-434, L438-439.
//!
//! Radar: (1) horizon cull d_km = 4.12 (sqrt(h_a) + sqrt(h_t)); (2) R = R_ref (sigma_eff / 100 m^2)^(1/4);
//! (3) sigma_eff = sigma(theta) / (1 + C) with clutter at full strength for a target top at or under 5 m and
//! fading linearly to none at 15 m; (4) a self-screening jammer limits detection to min(R, R_bt) with
//! R_bt = R_bt0 (R_ref / 40 km)^2 sqrt(sigma_eff / 100 m^2). A radar in Silent, or in Burst between sweeps,
//! makes no detections and has no ESM signature. ESM: passive, bearing only, range = min(radar horizon from
//! the emitter's height, 2.0 x the emitter's R_ref). A radiating jammer is an emitter every ESM receiver inside
//! the radar horizon gets a bearing on.
use super::{measurement_error, Detection, Evidence, Measurement, SenseEnv, SensorInstance};
use crate::ids::SensorRef;
use crate::signature::{compass_bearing, EmitterKind, SigKind, Signature};
use content::enums::Domain;
use content::part::weapons::SeekerKind;
use content::part::SensorKind;
use content::tuning::SensorTuning;
use glam::DVec3;

/// Reference radar of the jammer rating: R_bt0 is quoted against a radar with R_ref 40 km (L428).
pub const JAMMER_REF_RADAR_M: f64 = 40_000.0;
/// Reference infrared power for infrared seekers: R_ref is quoted against a 1 MW source and scales with the
/// square root of power (one-way spreading, like passive sonar). The spec gives no infrared law.
pub const IR_REF_MW: f64 = 1.0;
/// Measured height above which a radar return reads as airborne for the domain hint, metres. Sea-skimmers fly
/// at 5 m and cannot be told from hulls by height alone.
pub const AIR_HEIGHT_M: f64 = 30.0;

/// Radar horizon in metres for antenna and target heights in metres (4/3 Earth, L425). Also the Sight gauge
/// formula (L164). Real code: spec formula with exact operations.
pub fn horizon_m(h_antenna_m: f64, h_target_m: f64) -> f64 { 4_120.0 * (h_antenna_m.max(0.0).sqrt() + h_target_m.max(0.0).sqrt()) }

/// Ranges that decide one radar pair, for the expert view and the gauge-vs-simulation gate (L438).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RadarRanges { pub horizon_m: f64, pub sigma_eff_m2: f64, pub r_m: f64, pub r_burn_through_m: Option<f64>, pub final_m: f64 }

/// Closed-form ranges for a radar against a target of `sigma_m2` whose top is `top_m` above the sea.
/// Uses `libm::pow(x, 0.25)` for the fourth root. Unit: `sys-detect-radar`.
///
/// `final_m` is the smallest of the horizon, the range law and the burn-through range. Bad input (negative,
/// NaN) reads as zero, so the pair is simply not detected.
pub fn radar_ranges(r_ref_m: f64, h_antenna_m: f64, sigma_m2: f64, top_m: f64, clutter: f64, jammer_r_bt0_m: Option<f64>, tuning: &content::tuning::SensorTuning) -> RadarRanges {
    let r_ref = nonneg(r_ref_m);
    let h_a = nonneg(h_antenna_m);
    let top = if top_m.is_finite() { top_m } else { 0.0 };
    let horizon = horizon_m(h_a, top);
    let sigma_ref = if tuning.sigma_ref_m2.is_finite() && tuning.sigma_ref_m2 > 0.0 { tuning.sigma_ref_m2 } else { 100.0 };
    let c = nonneg(clutter) * clutter_weight(top, tuning);
    let sigma_eff = nonneg(sigma_m2) / (1.0 + c);
    let r = r_ref * libm::pow(sigma_eff / sigma_ref, 0.25);
    let r_bt = jammer_r_bt0_m.map(|bt0| {
        let q = r_ref / JAMMER_REF_RADAR_M;
        nonneg(bt0) * q * q * (sigma_eff / sigma_ref).sqrt()
    });
    let final_m = nonneg(horizon.min(r).min(r_bt.unwrap_or(f64::INFINITY)));
    RadarRanges { horizon_m: horizon, sigma_eff_m2: sigma_eff, r_m: nonneg(r), r_burn_through_m: r_bt.map(nonneg), final_m }
}

/// Share of the sea-state clutter that applies to a target whose top is `top_m` above the sea: 1 at or under
/// `clutter_full_below_m` (5 m), 0 at or above `clutter_zero_above_m` (15 m), linear between (L427).
pub fn clutter_weight(top_m: f64, tuning: &SensorTuning) -> f64 {
    let (lo, hi) = (tuning.clutter_full_below_m, tuning.clutter_zero_above_m);
    if !top_m.is_finite() || top_m <= lo { return 1.0; }
    if top_m >= hi || hi <= lo { return 0.0; }
    (hi - top_m) / (hi - lo)
}

fn nonneg(x: f64) -> f64 { if x.is_finite() && x > 0.0 { x } else { 0.0 } }

/// Wrap an angle into `(-pi, pi]`.
pub(crate) fn wrap_pi(a: f64) -> f64 {
    let w = crate::signature::wrap_tau(a);
    if w > core::f64::consts::PI { w - core::f64::consts::TAU } else { w }
}

/// True when compass `bearing` lies where the sensor looks this evaluation. Rotating antennas
/// (`rot_per_tick != 0`) see the half-open sector swept since the previous evaluation, `[swept_from, bearing)`
/// in the direction of rotation, or all round when a whole turn or more has passed. Staring sensors see
/// within `fov_half_rad` of their boresight; a half-angle of 180 degrees or more is all round.
pub(crate) fn in_view(sensor: &SensorInstance, bearing: f64, tick: u64) -> bool {
    let tau = core::f64::consts::TAU;
    if sensor.rot_per_tick != 0.0 && sensor.rot_per_tick.is_finite() {
        let elapsed = tick.saturating_sub(sensor.last_eval_tick) as f64;
        if sensor.rot_per_tick.abs() * elapsed >= tau { return true; }
        let (sweep, rel) = if sensor.rot_per_tick > 0.0 {
            (crate::signature::wrap_tau(sensor.bearing_rad - sensor.swept_from_rad), crate::signature::wrap_tau(bearing - sensor.swept_from_rad))
        } else {
            (crate::signature::wrap_tau(sensor.swept_from_rad - sensor.bearing_rad), crate::signature::wrap_tau(sensor.swept_from_rad - bearing))
        };
        return rel < sweep;
    }
    let half = sensor.fov_half_rad;
    if !half.is_finite() || half >= core::f64::consts::PI - 1e-9 { return true; }
    if half <= 0.0 { return false; }
    wrap_pi(bearing - sensor.bearing_rad).abs() <= half
}

/// The point a line-of-sight test aims at: the target's highest point for hulls, its position otherwise.
pub(crate) fn sight_point(target: &Signature) -> DVec3 {
    match target.kind {
        SigKind::Vessel | SigKind::Wreck => DVec3::new(target.pos.x, target.pos.y.max(target.top_m), target.pos.z),
        _ => target.pos,
    }
}

/// Altitude (positive) or depth (negative) a height-measuring sensor reads for the target.
pub(crate) fn measured_height(target: &Signature) -> f64 {
    match target.kind {
        SigKind::Vessel | SigKind::Wreck => -target.depth_m,
        _ => if target.depth_m > 0.0 { -target.depth_m } else { target.pos.y },
    }
}

/// Common guards: a sensor never senses its own carrier, and every number must be finite.
pub(crate) fn pair_ok(sensor: &SensorInstance, target: &Signature) -> bool {
    target.entity != sensor.id.carrier && sensor.pos.is_finite() && target.pos.is_finite() && sensor.output.is_finite() && sensor.output > 0.0
}

pub(crate) fn error_factor(sensor: &SensorInstance) -> f64 {
    if sensor.error_factor.is_finite() && sensor.error_factor > 0.0 { sensor.error_factor } else { 1.0 }
}

/// Seeker type of a small body's seeker sensor, read from the catalogue; `None` for any other sensor.
fn seeker_kind(sensor: &SensorInstance, env: &SenseEnv<'_>) -> Option<SeekerKind> {
    if sensor.kind != SensorKind::Seeker || sensor.id.index != SensorRef::SEEKER { return None; }
    let bodies = env.world.bodies();
    let row = bodies.row_of(sensor.id.carrier)?;
    let kind = *bodies.kind.get(row)?;
    env.ctx.catalogue.munitions.get_index(kind.index()).map(|(_, d)| d.seeker)
}

/// Build a range-and-bearing detection with the spec's 1-sigma errors for the sensor kind (L430). The
/// scheduler adds the noise; laws report the true measurement.
#[allow(clippy::too_many_arguments)]
pub(crate) fn range_bearing(sensor: &SensorInstance, target: &Signature, tick: u64, range_m: f64, bearing: f64, height: Option<f64>, evidence: Evidence, rcs_m2: f64, domain_hint: Option<Domain>) -> Detection {
    let me = measurement_error(sensor.kind);
    let ef = error_factor(sensor);
    let sigma_r = me.range_m.unwrap_or(0.0) + me.range_fraction.unwrap_or(0.0) * range_m;
    Detection {
        tick, sensor: sensor.id, side: sensor.side, kind: sensor.kind, sensor_pos: sensor.pos, target: target.entity,
        measurement: Measurement::RangeBearing { range_m, bearing_rad: bearing, height_m: height },
        sigma_range_m: sigma_r * ef, sigma_bearing_rad: me.bearing_deg.to_radians() * ef, evidence, emitter: u16::MAX,
        rcs_m2: rcs_m2 as f32, domain_hint,
    }
}

/// Build a bearing-only detection (ESM, passive sonar).
pub(crate) fn bearing_only(sensor: &SensorInstance, target: &Signature, tick: u64, bearing: f64, evidence: Evidence, emitter: u16, domain_hint: Option<Domain>) -> Detection {
    let me = measurement_error(sensor.kind);
    Detection {
        tick, sensor: sensor.id, side: sensor.side, kind: sensor.kind, sensor_pos: sensor.pos, target: target.entity,
        measurement: Measurement::BearingOnly { bearing_rad: bearing }, sigma_range_m: 0.0,
        sigma_bearing_rad: me.bearing_deg.to_radians() * error_factor(sensor), evidence, emitter, rcs_m2: 0.0, domain_hint,
    }
}

/// Height reading of a fire-control radar or seeker: (height, evidence, domain hint).
fn height_reading(target: &Signature) -> (Option<f64>, Evidence, Option<Domain>) {
    let h = measured_height(target);
    // A measured altitude tells the domain (L471). Below `AIR_HEIGHT_M` only a target that is airborne in truth
    // (a sea-skimming missile or a low drone) reads as in the air: hulls read at or below the sea, and a decoy
    // keeps reading as the ship it imitates. Without this
    // hint a 5 m sea-skimmer started a surface track (8 m/s prior), which the next return 0.2 s and 50 m later
    // could not gate, so each lock pulse left a new stationary surface ghost that guns then engaged.
    let hint = if h >= AIR_HEIGHT_M { Some(Domain::Air) } else if h > 0.0 && matches!(target.kind, SigKind::Body(_)) && matches!(target.domain, Domain::Air | Domain::Weapon) { Some(target.domain) } else { None };
    (Some(h), Evidence::ALTITUDE | Evidence::FIRE_CONTROL, hint)
}

/// Radar or seeker detection of one target, or `None`: field of view or swept sector, horizon, line of sight
/// over terrain, final range. Fire-control radars and seekers report height. Unit: `sys-detect-radar`.
///
/// Seekers follow their munition's seeker: active radar uses this law, an infrared seeker the passive
/// square-root law against `ir_mw` (blocked by smoke), and an active acoustic seeker the active sonar law.
/// Every geometric and range test runs before the line-of-sight query, the only costly step.
pub fn detect_radar(sensor: &SensorInstance, target: &Signature, env: &SenseEnv<'_>) -> Option<Detection> {
    let infrared = match seeker_kind(sensor, env) {
        Some(SeekerKind::None) => return None,
        Some(SeekerKind::ActiveAcoustic) => return super::sonar::detect_active(sensor, target, env),
        Some(SeekerKind::Infrared) => true,
        Some(SeekerKind::ActiveRadar) | None => false,
    };
    let det = if infrared { infrared_core(sensor, target, env.ctx.tick.0) } else { radar_core(sensor, target, env.ctx.tick.0, env.clutter, &env.ctx.tuning.sensors) }?;
    if !env.world.line_of_sight(sensor.pos, sight_point(target), infrared, [sensor.id.carrier, target.entity]) { return None; }
    Some(det)
}

/// The radar law without line of sight. Cull order: working and radiating, target above water, horizon,
/// field of view or swept sector, then the aspect RCS and the range law.
pub(crate) fn radar_core(sensor: &SensorInstance, target: &Signature, tick: u64, clutter: f64, tuning: &SensorTuning) -> Option<Detection> {
    if !sensor.active || !sensor.radiating || !pair_ok(sensor, target) || target.is_submerged() { return None; }
    let d = target.pos - sensor.pos;
    let range = d.length();
    if !range.is_finite() || range > horizon_m(sensor.height_m, target.top_m) { return None; }
    let bearing = compass_bearing(d);
    if !in_view(sensor, bearing, tick) { return None; }
    let sigma = target.rcs_toward(sensor.pos);
    let rr = radar_ranges(sensor.r_ref_m, sensor.height_m, sigma, target.top_m, clutter, target.jammer_r_bt0_m, tuning);
    let reach = rr.horizon_m.min(rr.r_m.min(rr.r_burn_through_m.unwrap_or(f64::INFINITY)) * sensor.output);
    if range > reach || reach <= 0.0 { return None; }
    let (height, evidence, hint) = if matches!(sensor.kind, SensorKind::FireControlRadar | SensorKind::Seeker) { height_reading(target) } else { (None, Evidence::empty(), None) };
    Some(range_bearing(sensor, target, tick, range, bearing, height, evidence, sigma, hint))
}

/// Infrared seeker without line of sight: passive, R = R_ref sqrt(IR / 1 MW), capped by the horizon.
pub(crate) fn infrared_core(sensor: &SensorInstance, target: &Signature, tick: u64) -> Option<Detection> {
    if !sensor.active || !pair_ok(sensor, target) || target.is_submerged() { return None; }
    let d = target.pos - sensor.pos;
    let range = d.length();
    let reach = horizon_m(sensor.height_m, target.top_m).min(nonneg(sensor.r_ref_m) * (nonneg(target.ir_mw) / IR_REF_MW).sqrt() * sensor.output);
    if !range.is_finite() || range > reach || reach <= 0.0 { return None; }
    let bearing = compass_bearing(d);
    if !in_view(sensor, bearing, tick) { return None; }
    let (height, evidence, hint) = height_reading(target);
    Some(range_bearing(sensor, target, tick, range, bearing, height, evidence, 0.0, hint))
}

/// ESM bearing on the strongest emission of the target, or `None` (L434). Sets `Evidence::EMITTER` and the
/// emitter index. Unit: `sys-detect-radar`.
///
/// Radar-band emissions only (search, navigation and fire-control radars, radar seekers, jammers); sonar pings
/// are heard by passive sonar instead. "Strongest" is the radar with the largest reference range that the
/// receiver can hear; a jammer is chosen only when no radar of the target is heard, since it carries no
/// emitter model. Jammers are heard to the radar horizon.
pub fn detect_esm(sensor: &SensorInstance, target: &Signature, env: &SenseEnv<'_>) -> Option<Detection> {
    let (det, src) = esm_core(sensor, target, env.ctx.tick.0, &env.ctx.tuning.sensors)?;
    if !env.world.line_of_sight(sensor.pos, src, false, [sensor.id.carrier, target.entity]) { return None; }
    Some(det)
}

/// ESM without line of sight: the detection and the emitter point the line of sight must reach.
pub(crate) fn esm_core(sensor: &SensorInstance, target: &Signature, tick: u64, tuning: &SensorTuning) -> Option<(Detection, DVec3)> {
    if !sensor.active || !pair_ok(sensor, target) || target.emissions.is_empty() { return None; }
    let mut best: Option<(u8, f64, usize)> = None;
    for (i, e) in target.emissions.iter().enumerate() {
        let class = match e.kind {
            EmitterKind::SearchRadar | EmitterKind::NavRadar | EmitterKind::FireControlRadar | EmitterKind::Seeker => 1u8,
            EmitterKind::Jammer => 0u8,
            EmitterKind::ActiveSonar => continue,
        };
        if !e.height_m.is_finite() || e.height_m < 0.0 { continue; }
        let src = DVec3::new(target.pos.x, e.height_m, target.pos.z);
        let dist = (src - sensor.pos).length();
        let reach = if e.kind == EmitterKind::Jammer { horizon_m(e.height_m, sensor.height_m) } else { esm_range_m(e.r_ref_m, e.height_m, sensor.height_m, tuning) * sensor.output };
        if !dist.is_finite() || dist > reach { continue; }
        let strength = if e.kind == EmitterKind::Jammer { 0.0 } else { nonneg(e.r_ref_m) };
        let better = match best { None => true, Some((c, s, _)) => class > c || (class == c && strength > s) };
        if better { best = Some((class, strength, i)); }
    }
    let (_, _, i) = best?;
    let bearing = compass_bearing(DVec3::new(target.pos.x - sensor.pos.x, 0.0, target.pos.z - sensor.pos.z));
    if !in_view(sensor, bearing, tick) { return None; }
    let e = target.emissions[i];
    let hint = if e.kind == EmitterKind::Seeker { Some(Domain::Weapon) } else { None };
    Some((bearing_only(sensor, target, tick, bearing, Evidence::EMITTER, e.emitter, hint), DVec3::new(target.pos.x, e.height_m, target.pos.z)))
}

/// Range at which an ESM receiver at `h_receiver_m` hears an emitter (L434, and the ESM warning at L510).
/// Unit: `sys-detect-radar`.
pub fn esm_range_m(emitter_r_ref_m: f64, h_emitter_m: f64, h_receiver_m: f64, tuning: &content::tuning::SensorTuning) -> f64 {
    let h_e = if h_emitter_m.is_finite() { h_emitter_m } else { 0.0 };
    let h_r = if h_receiver_m.is_finite() { h_receiver_m } else { 0.0 };
    nonneg(horizon_m(h_e, h_r).min(nonneg(tuning.esm_range_factor) * nonneg(emitter_r_ref_m)))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::signature::SigKind;
    use sim_core::EntityId;

    fn t() -> SensorTuning { SensorTuning::default() }

    pub(crate) fn sensor(kind: SensorKind) -> SensorInstance {
        SensorInstance {
            id: SensorRef { carrier: EntityId { index: 1, generation: 0 }, index: 0 }, side: Default::default(), kind, r_ref_m: 40_000.0,
            period_ticks: 12, phase: 0, rot_per_tick: 0.0, fov_half_rad: core::f64::consts::PI, emitter: u16::MAX,
            pos: DVec3::new(0.0, 20.0, 0.0), height_m: 20.0, bearing_rad: 0.0, swept_from_rad: 0.0, active: true, radiating: true,
            output: 1.0, error_factor: 1.0, own_speed_kn: 0.0, last_eval_tick: 0,
        }
    }

    #[test]
    fn horizon_matches_sight_gauge() {
        // 4.12 km x (sqrt(16) + sqrt(25)) = 37.08 km.
        assert!((horizon_m(16.0, 25.0) - 37_080.0).abs() < 1e-6);
        assert_eq!(horizon_m(-3.0, f64::NAN), 0.0);
    }

    #[test]
    fn beaufort7_sea_skimmer_at_about_5_4_km() {
        // L438: frigate search radar (R_ref 40 km) against the 0.1 m^2 sea-skimmer at 5 m in Beaufort 7 (C = 2).
        let r = radar_ranges(40_000.0, 20.0, 0.1, 5.0, 2.0, None, &t());
        assert!(r.final_m >= 5_000.0, "{r:?}");
        assert!((r.final_m - 5_404.0).abs() < 10.0, "{r:?}");
        assert!((r.sigma_eff_m2 - 0.1 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn fourth_root_law_and_sixteen_times_doubles() {
        let a = radar_ranges(40_000.0, 400.0, 100.0, 100.0, 0.0, None, &t());
        let b = radar_ranges(40_000.0, 400.0, 1_600.0, 100.0, 0.0, None, &t());
        assert!((a.r_m - 40_000.0).abs() < 1e-6);
        assert!((b.r_m / a.r_m - 2.0).abs() < 1e-12);
    }

    #[test]
    fn jammer_example_from_spec() {
        // L428: a jamming 150 t craft (~150 m^2) is seen by a frigate-class search radar at 12.2 km instead of 44 km.
        let r = radar_ranges(40_000.0, 25.0, 150.0, 30.0, 0.0, Some(10_000.0), &t());
        assert!((r.r_m - 44_267.0).abs() < 5.0, "{r:?}");
        assert!((r.r_burn_through_m.unwrap() - 12_247.0).abs() < 5.0, "{r:?}");
        assert!((r.final_m - 12_247.0).abs() < 5.0);
        // Jamming floor (L438): still detectable at 8 km or more in Beaufort 4 with the mast top at 8 m.
        let b4 = radar_ranges(40_000.0, 25.0, 150.0, 8.0, 0.5, Some(10_000.0), &t());
        assert!(b4.final_m >= 8_000.0, "{b4:?}");
        // Burn-through scales with the square of R_ref.
        let small = radar_ranges(20_000.0, 25.0, 150.0, 30.0, 0.0, Some(10_000.0), &t());
        assert!((small.r_burn_through_m.unwrap() * 4.0 - r.r_burn_through_m.unwrap()).abs() < 1e-6);
    }

    #[test]
    fn clutter_fades_between_5_and_15_m() {
        let tn = t();
        assert_eq!(clutter_weight(2.0, &tn), 1.0);
        assert_eq!(clutter_weight(5.0, &tn), 1.0);
        assert!((clutter_weight(10.0, &tn) - 0.5).abs() < 1e-12);
        assert_eq!(clutter_weight(15.0, &tn), 0.0);
        assert_eq!(clutter_weight(1_000.0, &tn), 0.0);
        let r = radar_ranges(40_000.0, 20.0, 100.0, 10.0, 2.0, None, &tn);
        assert!((r.sigma_eff_m2 - 50.0).abs() < 1e-9);
    }

    #[test]
    fn horizon_caps_final_range() {
        let r = radar_ranges(200_000.0, 9.0, 7_000.0, 16.0, 0.0, None, &t());
        assert!((r.final_m - horizon_m(9.0, 16.0)).abs() < 1e-9);
    }

    #[test]
    fn bad_input_never_detects_or_panics() {
        for &x in &[f64::NAN, f64::INFINITY, -1.0, 0.0] {
            let r = radar_ranges(x, x, x, x, x, Some(x), &t());
            assert!(r.final_m.is_finite() && r.final_m >= 0.0, "{r:?}");
            assert!(esm_range_m(x, x, x, &t()).is_finite());
        }
    }

    #[test]
    fn esm_range_is_horizon_or_twice_r_ref() {
        let tn = t();
        // Seeker at 5 m, receiver at 20 m: horizon 27.6 km, 2 x 12 km = 24 km.
        assert!((esm_range_m(12_000.0, 5.0, 20.0, &tn) - 24_000.0).abs() < 1e-9);
        // Search radar at 25 m: 2 x 40 km = 80 km, horizon to a 20 m mast = 39.0 km.
        assert!((esm_range_m(40_000.0, 25.0, 20.0, &tn) - horizon_m(25.0, 20.0)).abs() < 1e-9);
    }

    #[test]
    fn rotating_sector_is_half_open_and_staring_fov() {
        let mut s = sensor(SensorKind::SearchRadar);
        s.rot_per_tick = core::f64::consts::TAU / 150.0;
        s.swept_from_rad = 350f64.to_radians();
        s.bearing_rad = 20f64.to_radians();
        s.last_eval_tick = 100;
        let tick = 112;
        assert!(in_view(&s, 355f64.to_radians(), tick));
        assert!(in_view(&s, 5f64.to_radians(), tick));
        assert!(in_view(&s, 350f64.to_radians(), tick));
        assert!(!in_view(&s, 20f64.to_radians(), tick));
        assert!(!in_view(&s, 90f64.to_radians(), tick));
        // Consecutive sectors tile the circle: every bearing is seen exactly once per turn.
        let mut seen = vec![0u32; 360];
        let step = core::f64::consts::TAU / 150.0 * 12.0;
        let mut from = 0.0f64;
        for k in 0..13 {
            s.swept_from_rad = crate::signature::wrap_tau(from);
            s.bearing_rad = crate::signature::wrap_tau(from + step);
            s.last_eval_tick = 1_000 + 12 * k;
            for (deg, n) in seen.iter_mut().enumerate() {
                if in_view(&s, (deg as f64 + 0.5).to_radians(), 1_000 + 12 * k + 12) { *n += 1; }
            }
            from += step;
        }
        // 13 sweeps of 28.8 degrees = 374.4 degrees: each bearing once, the first 14.4 degrees twice.
        assert!(seen.iter().all(|&n| n >= 1), "{seen:?}");
        assert!(seen.iter().enumerate().all(|(d, &n)| n == if d < 14 { 2 } else { 1 }), "{seen:?}");
        // A full turn since the last look sees everything.
        s.last_eval_tick = 0;
        assert!(in_view(&s, 3.0, 1_000));
        // Staring sensor with a 30 degree half-angle about 090.
        let mut f = sensor(SensorKind::FireControlRadar);
        f.fov_half_rad = 30f64.to_radians();
        f.bearing_rad = 90f64.to_radians();
        assert!(in_view(&f, 100f64.to_radians(), 5));
        assert!(!in_view(&f, 130f64.to_radians(), 5));
        f.bearing_rad = 10f64.to_radians();
        assert!(in_view(&f, 350f64.to_radians(), 5));
    }

    #[test]
    fn measured_height_reads_altitude_or_depth() {
        let mut sig = crate::signature::tests::sig(SigKind::Vessel);
        assert_eq!(measured_height(&sig), 0.0);
        sig.depth_m = 40.0;
        assert_eq!(measured_height(&sig), -40.0);
        let mut b = crate::signature::tests::sig(SigKind::Body(content::ids::MunitionIdx(0)));
        b.pos.y = 500.0;
        b.depth_m = 0.0;
        assert_eq!(measured_height(&b), 500.0);
    }

    /// A vessel signature with a uniform aspect table at compass bearing 000 and `range` metres.
    pub(crate) fn hull_at(range: f64, rcs: f32, above: f32, submerged: f32, top: f64) -> Signature {
        let mut s = crate::signature::tests::sig(SigKind::Vessel);
        s.pos = DVec3::new(0.0, 0.0, -range);
        s.top_m = top;
        s.areas = Some(std::sync::Arc::new(sim_core::vessel::ProjectedAreas { rcs_m2: [rcs; 36], above_m2: [above; 36], submerged_m2: [submerged; 36], top_height_m: top as f32 }));
        s
    }

    #[test]
    fn radar_core_gates_and_reports() {
        let tn = t();
        let s = sensor(SensorKind::SearchRadar);
        // 150 m^2 craft, top 10 m: R = 44.3 km, horizon from 20 m = 31.45 km.
        let target = hull_at(30_000.0, 150.0, 0.0, 0.0, 10.0);
        let det = radar_core(&s, &target, 5, 0.0, &tn).expect("inside horizon and range");
        match det.measurement { Measurement::RangeBearing { range_m, bearing_rad, height_m } => {
            assert!((range_m - 30_000.0).abs() < 0.1);
            assert!(bearing_rad.abs() < 1e-12);
            assert_eq!(height_m, None);
        } _ => panic!("range and bearing") }
        assert_eq!(det.evidence, Evidence::empty());
        assert_eq!(det.sigma_range_m, 50.0);
        assert!((det.rcs_m2 - 150.0).abs() < 1e-3);
        assert!(radar_core(&s, &hull_at(32_000.0, 150.0, 0.0, 0.0, 10.0), 5, 0.0, &tn).is_none(), "beyond the horizon");
        let mut quiet = s.clone();
        quiet.radiating = false;
        assert!(radar_core(&quiet, &target, 5, 0.0, &tn).is_none(), "a silent radar sees nothing");
        let mut dived = target.clone();
        dived.top_m = -3.0;
        dived.depth_m = 20.0;
        assert!(radar_core(&s, &dived, 5, 0.0, &tn).is_none());
        // Damage output halves the law range; local control triples the error.
        let mut hurt = s.clone();
        hurt.output = 0.5;
        hurt.error_factor = 3.0;
        assert!(radar_core(&hurt, &hull_at(23_000.0, 150.0, 0.0, 0.0, 10.0), 5, 0.0, &tn).is_none());
        let d = radar_core(&hurt, &hull_at(21_000.0, 150.0, 0.0, 0.0, 10.0), 5, 0.0, &tn).unwrap();
        assert_eq!(d.sigma_range_m, 150.0);
        // Self-screening jammer: burn-through 12.25 km.
        let mut jam = hull_at(12_500.0, 150.0, 0.0, 0.0, 10.0);
        jam.jammer_r_bt0_m = Some(10_000.0);
        assert!(radar_core(&s, &jam, 5, 0.0, &tn).is_none());
        jam.pos.z = -12_000.0;
        assert!(radar_core(&s, &jam, 5, 0.0, &tn).is_some());
        // Fire control measures height and flags the lock.
        let mut fc = sensor(SensorKind::FireControlRadar);
        fc.r_ref_m = 25_000.0;
        let d = radar_core(&fc, &hull_at(10_000.0, 150.0, 0.0, 0.0, 10.0), 5, 0.0, &tn).unwrap();
        assert_eq!(d.evidence, Evidence::ALTITUDE | Evidence::FIRE_CONTROL);
        assert!(matches!(d.measurement, Measurement::RangeBearing { height_m: Some(h), .. } if h == 0.0));
        // Never its own carrier.
        let mut own = target.clone();
        own.entity = s.id.carrier;
        assert!(radar_core(&s, &own, 5, 0.0, &tn).is_none());
    }

    #[test]
    fn esm_core_picks_strongest_radar_then_jammer() {
        use crate::signature::Emission;
        let tn = t();
        let esm = sensor(SensorKind::Esm);
        let mut target = hull_at(30_000.0, 150.0, 0.0, 0.0, 25.0);
        let em = |kind, emitter, r_ref_m, height_m| Emission { source: SensorRef::default(), kind, emitter, r_ref_m, height_m };
        target.emissions = vec![
            em(EmitterKind::FireControlRadar, 3, 25_000.0, 20.0),
            em(EmitterKind::Jammer, u16::MAX, crate::signature::JAMMER_ESM_R_REF_M, 10.0),
            em(EmitterKind::SearchRadar, 7, 40_000.0, 25.0),
            em(EmitterKind::ActiveSonar, 9, 4_000.0, -4.0),
        ];
        let (d, src) = esm_core(&esm, &target, 1, &tn).unwrap();
        assert_eq!(d.emitter, 7);
        assert_eq!(d.evidence, Evidence::EMITTER);
        assert!(matches!(d.measurement, Measurement::BearingOnly { .. }));
        assert!((d.sigma_bearing_rad - 2f64.to_radians()).abs() < 1e-12);
        assert_eq!(src.y, 25.0);
        // Only the jammer: still a bearing, no emitter model; heard to the horizon only.
        target.emissions.retain(|e| e.kind == EmitterKind::Jammer || e.kind == EmitterKind::ActiveSonar);
        assert_eq!(esm_core(&esm, &target, 1, &tn).unwrap().0.emitter, u16::MAX);
        target.pos.z = -32_000.0;
        assert!(esm_core(&esm, &target, 1, &tn).is_none(), "jammer beyond the horizon");
        // Sonar pings are not radar-band.
        target.pos.z = -1_000.0;
        target.emissions.retain(|e| e.kind == EmitterKind::ActiveSonar);
        assert!(esm_core(&esm, &target, 1, &tn).is_none());
    }
}
