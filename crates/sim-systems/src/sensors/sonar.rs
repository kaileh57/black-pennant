//! Unit `sys-detect-sonar`. Hull sonar, passive and active, with the layer-and-shadow model.
//! DESIGN.md L449-456. Detects torpedoes, mines and surface noise at first release; AI submarines and depth
//! charges use the same model from Act 3. Convergence zones are never simulated.
//!
//! Passive: R = R_ref sqrt(N / 1.0); own speed over 15 kn halves the range, over 25 kn hears nothing; bearing
//! only (3 degrees). Any passive sonar also hears active pings out to 2.0 x the pinging sonar's R_ref.
//! Active: R = R_ref (A_sub(theta) / 100 m^2)^(1/4); 20 m and 1 degree; it is an emitter under EMCON.
//! Layer: per region depth 30 to 80 m; water shallower than the layer has none. Hull sonars sit above the
//! layer. Sensor and target on opposite sides: range x 0.3, and a target over 5 km away is in the shadow zone
//! and undetectable.
use super::radar::{bearing_only, in_view, measured_height, pair_ok, range_bearing};
use super::{Detection, Evidence, SenseEnv, SensorInstance};
use crate::signature::{compass_bearing, EmitterKind, SigKind, Signature};
use content::enums::Domain;
use content::part::SensorKind;
use content::tuning::SensorTuning;

/// Passive sonar reference noise N_ref (L453).
pub const N_REF: f64 = 1.0;
/// Reference submerged area of the active law, m^2 (L454).
pub const ACTIVE_REF_AREA_M2: f64 = 100.0;
/// Own speed above which passive range halves, and above which the hull sonar hears nothing, knots (L453).
pub const PASSIVE_HALF_KN: f64 = 15.0;
pub const PASSIVE_DEAF_KN: f64 = 25.0;
/// Any passive sonar hears an active ping out to this multiple of the pinging sonar's R_ref (L454).
pub const PING_INTERCEPT_FACTOR: f64 = 2.0;
/// Height above the sea over which a small body counts as airborne and out of the water for sonar, metres.
const AIRBORNE_M: f64 = 0.5;

fn pos(x: f64) -> f64 { if x.is_finite() && x > 0.0 { x } else { 0.0 } }

/// Own-speed factor of passive listening: 1, 0.5 above 15 kn, 0 above 25 kn.
fn own_speed_factor(own_speed_kn: f64) -> f64 {
    let v = if own_speed_kn.is_finite() { own_speed_kn.abs() } else { return 0.0 };
    if v > PASSIVE_DEAF_KN { 0.0 } else if v > PASSIVE_HALF_KN { 0.5 } else { 1.0 }
}

/// True when the target is somewhere sound can reach it: not an air contact and not a body flying above the sea.
fn in_water(target: &Signature) -> bool {
    if target.domain == Domain::Air { return false; }
    match target.kind {
        SigKind::Body(_) | SigKind::Decoy(_) => target.pos.y <= AIRBORNE_M,
        SigKind::Vessel | SigKind::Wreck => true,
    }
}

/// Layer factor for a pair: sensor depth from its (negative) height, target depth from its signature, water
/// depth at the target from `water` (asked only when the two sit on opposite sides of the layer depth).
fn pair_layer(sensor: &SensorInstance, target: &Signature, range: f64, layer_depth_m: f64, tuning: &SensorTuning, water: &dyn Fn() -> f64) -> f64 {
    let sensor_depth = if sensor.height_m.is_finite() { (-sensor.height_m).max(0.0) } else { 0.0 };
    if !(layer_depth_m.is_finite() && layer_depth_m > 0.0) || (sensor_depth > layer_depth_m) == (target.depth_m > layer_depth_m) { return 1.0; }
    layer_factor(sensor_depth, target.depth_m, range, water(), layer_depth_m, tuning)
}

/// Sound path clear of land between the transducer and the target.
fn water_path_clear(sensor: &SensorInstance, target: &Signature, env: &SenseEnv<'_>) -> bool { !env.world.terrain().segment_blocked(sensor.pos, target.pos) }

/// Range multiplier from the layer for a sensor at `sensor_depth_m` and a target at `target_depth_m` separated
/// by `range_m` in water `water_depth_m` deep: 1.0 same side or no layer, 0.3 across, 0.0 in the shadow zone.
/// Unit: `sys-detect-sonar`.
///
/// A point is below the layer when its depth is greater than the layer depth. A layer depth that is not a
/// positive finite number means no layer.
pub fn layer_factor(sensor_depth_m: f64, target_depth_m: f64, range_m: f64, water_depth_m: f64, layer_depth_m: f64, tuning: &content::tuning::SensorTuning) -> f64 {
    if !(layer_depth_m.is_finite() && layer_depth_m > 0.0) { return 1.0; }
    if !(water_depth_m.is_finite() && water_depth_m > layer_depth_m) { return 1.0; }
    let below = |d: f64| d.is_finite() && d > layer_depth_m;
    if below(sensor_depth_m) == below(target_depth_m) { return 1.0; }
    if !range_m.is_finite() || range_m > tuning.shadow_zone_m { return 0.0; }
    pos(tuning.layer_cross_factor)
}

/// Passive range in metres before the layer factor. Unit: `sys-detect-sonar`.
pub fn passive_range_m(r_ref_m: f64, noise: f64, own_speed_kn: f64) -> f64 { pos(r_ref_m) * (pos(noise) / N_REF).sqrt() * own_speed_factor(own_speed_kn) }

/// Active range in metres before the layer factor (`libm::pow(x, 0.25)`). Unit: `sys-detect-sonar`.
pub fn active_range_m(r_ref_m: f64, submerged_area_m2: f64) -> f64 { pos(r_ref_m) * libm::pow(pos(submerged_area_m2) / ACTIVE_REF_AREA_M2, 0.25) }

/// Passive detection: propulsion noise, running torpedoes, and other units' active pings. Sets
/// `Evidence::ACOUSTIC`. Unit: `sys-detect-sonar`.
///
/// Noise is heard to `passive_range_m` and pings to 2.0 x the pinging sonar's R_ref (own-speed rule applied
/// to both), each times the layer factor and the damage output. Noise sets `ACOUSTIC`; a heard ping sets
/// `EMITTER` with the sonar's emitter id. The acoustic signature tells the domain (hull, submarine, torpedo).
pub fn detect_passive(sensor: &SensorInstance, target: &Signature, env: &SenseEnv<'_>) -> Option<Detection> {
    let terrain = env.world.terrain();
    let det = passive_core(sensor, target, env.ctx.tick.0, env.ctx.layer_depth_m, &env.ctx.tuning.sensors, &|| terrain.depth_at(target.pos.x, target.pos.z))?;
    if !water_path_clear(sensor, target, env) { return None; }
    Some(det)
}

/// Passive sonar without the land test. Cull order: working, target in the water, own speed, the largest
/// un-layered reach, field of view, then the layer.
pub(crate) fn passive_core(sensor: &SensorInstance, target: &Signature, tick: u64, layer_depth_m: f64, tuning: &SensorTuning, water: &dyn Fn() -> f64) -> Option<Detection> {
    if !sensor.active || !pair_ok(sensor, target) || !in_water(target) { return None; }
    let speed = own_speed_factor(sensor.own_speed_kn);
    if speed <= 0.0 { return None; }
    let d = target.pos - sensor.pos;
    let range = d.length();
    let noise_reach = passive_range_m(sensor.r_ref_m, target.noise, sensor.own_speed_kn) * sensor.output;
    let ping_reach = |e: &crate::signature::Emission| PING_INTERCEPT_FACTOR * pos(e.r_ref_m) * speed * sensor.output;
    let max_reach = target.emissions.iter().filter(|e| e.kind == EmitterKind::ActiveSonar).map(ping_reach).fold(noise_reach, f64::max);
    if !range.is_finite() || range > max_reach || max_reach <= 0.0 { return None; }
    let bearing = compass_bearing(d);
    if !in_view(sensor, bearing, tick) { return None; }
    let layer = pair_layer(sensor, target, range, layer_depth_m, tuning, water);
    if layer <= 0.0 { return None; }
    let heard_noise = range <= noise_reach * layer;
    let mut ping: Option<(f64, u16)> = None;
    for e in target.emissions.iter().filter(|e| e.kind == EmitterKind::ActiveSonar) {
        if range <= ping_reach(e) * layer && ping.is_none_or(|(r, _)| e.r_ref_m > r) { ping = Some((e.r_ref_m, e.emitter)); }
    }
    if !heard_noise && ping.is_none() { return None; }
    let mut evidence = Evidence::empty();
    if heard_noise { evidence |= Evidence::ACOUSTIC; }
    let emitter = match ping { Some((_, id)) => { evidence |= Evidence::EMITTER; id } None => u16::MAX };
    Some(bearing_only(sensor, target, tick, bearing, evidence, emitter, Some(target.domain)))
}

/// Active detection (only while the sonar radiates): hulls, submarines, torpedoes, moored mines. Reports
/// depth, so sets `Evidence::DEPTH` for submerged targets. Unit: `sys-detect-sonar`.
///
/// Also the law of active acoustic torpedo seekers (routed here from `radar::detect_radar`), which use their
/// own R_ref and field of view.
pub fn detect_active(sensor: &SensorInstance, target: &Signature, env: &SenseEnv<'_>) -> Option<Detection> {
    let terrain = env.world.terrain();
    let det = active_core(sensor, target, env.ctx.tick.0, env.ctx.layer_depth_m, &env.ctx.tuning.sensors, &|| terrain.depth_at(target.pos.x, target.pos.z))?;
    if !water_path_clear(sensor, target, env) { return None; }
    Some(det)
}

/// Active sonar without the land test. Cull order: working and pinging, target in the water, un-layered
/// reach, field of view, then the layer.
pub(crate) fn active_core(sensor: &SensorInstance, target: &Signature, tick: u64, layer_depth_m: f64, tuning: &SensorTuning, water: &dyn Fn() -> f64) -> Option<Detection> {
    if !matches!(sensor.kind, SensorKind::ActiveSonar | SensorKind::Seeker) { return None; }
    if !sensor.active || !sensor.radiating || !pair_ok(sensor, target) || !in_water(target) { return None; }
    let d = target.pos - sensor.pos;
    let range = d.length();
    let reach = active_range_m(sensor.r_ref_m, target.submerged_area_toward(sensor.pos)) * sensor.output;
    if !range.is_finite() || range > reach || reach <= 0.0 { return None; }
    let bearing = compass_bearing(d);
    if !in_view(sensor, bearing, tick) { return None; }
    if range > reach * pair_layer(sensor, target, range, layer_depth_m, tuning, water) { return None; }
    let h = measured_height(target);
    let evidence = if target.depth_m > 0.0 { Evidence::DEPTH } else { Evidence::empty() };
    let hint = if target.depth_m > 0.0 && matches!(target.kind, SigKind::Vessel | SigKind::Wreck) { Some(Domain::Subsurface) } else { None };
    Some(range_bearing(sensor, target, tick, range, bearing, Some(h), evidence, 0.0, hint))
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::tuning::SensorTuning;

    #[test]
    fn passive_law_and_own_speed() {
        assert!((passive_range_m(8_000.0, 1.0, 12.0) - 8_000.0).abs() < 1e-9);
        assert!((passive_range_m(8_000.0, 4.0, 12.0) - 16_000.0).abs() < 1e-9);
        assert!((passive_range_m(8_000.0, 1.0, 15.0) - 8_000.0).abs() < 1e-9);
        assert!((passive_range_m(8_000.0, 1.0, 16.0) - 4_000.0).abs() < 1e-9);
        assert_eq!(passive_range_m(8_000.0, 1.0, 25.5), 0.0);
        assert_eq!(passive_range_m(8_000.0, 0.0, 5.0), 0.0);
        assert_eq!(passive_range_m(8_000.0, f64::NAN, 5.0), 0.0);
        // L456 gate shape: a 40 kn torpedo (N = 1) heard at 12 kn is more than 60 s out.
        assert!(passive_range_m(8_000.0, 1.0, 12.0) / (40.0 * sim_core::time::KN) >= 60.0);
    }

    #[test]
    fn active_law_finds_a_mine_at_1_3_km() {
        let r = active_range_m(4_000.0, 1.0);
        assert!((r - 1_264.9).abs() < 0.1, "{r}");
        assert!(r >= 800.0);
        assert!((active_range_m(4_000.0, 1_600.0) - 8_000.0).abs() < 1e-9);
        assert_eq!(active_range_m(4_000.0, -1.0), 0.0);
    }

    #[test]
    fn layer_and_shadow() {
        let t = SensorTuning::default();
        // Hull sonar at 4 m, target at 100 m, 50 m layer in 200 m of water.
        assert_eq!(layer_factor(4.0, 20.0, 3_000.0, 200.0, 50.0, &t), 1.0);
        assert!((layer_factor(4.0, 100.0, 3_000.0, 200.0, 50.0, &t) - 0.3).abs() < 1e-12);
        assert_eq!(layer_factor(4.0, 100.0, 5_000.1, 200.0, 50.0, &t), 0.0);
        assert!((layer_factor(4.0, 100.0, 5_000.0, 200.0, 50.0, &t) - 0.3).abs() < 1e-12);
        // Shallow water has no layer; nor does a missing layer depth.
        assert_eq!(layer_factor(4.0, 40.0, 9_000.0, 45.0, 50.0, &t), 1.0);
        assert_eq!(layer_factor(4.0, 100.0, 9_000.0, 200.0, f64::NAN, &t), 1.0);
        // Both below: same side.
        assert_eq!(layer_factor(60.0, 100.0, 9_000.0, 200.0, 50.0, &t), 1.0);
    }

    fn sonar(kind: SensorKind, r_ref_m: f64) -> SensorInstance {
        let mut s = crate::sensors::radar::tests::sensor(kind);
        s.r_ref_m = r_ref_m;
        s.pos.y = -4.0;
        s.height_m = -4.0;
        s
    }

    fn torpedo_at(range: f64, depth: f64) -> Signature {
        let mut t = crate::signature::tests::sig(SigKind::Body(content::ids::MunitionIdx(2)));
        t.domain = Domain::Weapon;
        t.pos = glam::DVec3::new(0.0, -depth, -range);
        t.top_m = 0.0;
        t.depth_m = depth;
        t.noise = 1.0;
        let disc = core::f64::consts::PI as f32;
        t.areas = Some(std::sync::Arc::new(sim_core::vessel::ProjectedAreas { rcs_m2: [0.0; 36], above_m2: [disc; 36], submerged_m2: [disc; 36], top_height_m: 0.0 }));
        t
    }

    #[test]
    fn passive_core_noise_pings_speed_and_layer() {
        use crate::signature::Emission;
        let t = SensorTuning::default();
        let deep = || 200.0;
        let mut s = sonar(SensorKind::PassiveSonar, 8_000.0);
        s.own_speed_kn = 12.0;
        let d = passive_core(&s, &torpedo_at(7_500.0, 10.0), 1, 50.0, &t, &deep).unwrap();
        assert_eq!(d.evidence, Evidence::ACOUSTIC);
        assert_eq!(d.domain_hint, Some(Domain::Weapon));
        assert!((d.sigma_bearing_rad - 3f64.to_radians()).abs() < 1e-12);
        assert!(passive_core(&s, &torpedo_at(8_100.0, 10.0), 1, 50.0, &t, &deep).is_none());
        s.own_speed_kn = 18.0;
        assert!(passive_core(&s, &torpedo_at(5_000.0, 10.0), 1, 50.0, &t, &deep).is_none());
        assert!(passive_core(&s, &torpedo_at(3_900.0, 10.0), 1, 50.0, &t, &deep).is_some());
        s.own_speed_kn = 26.0;
        assert!(passive_core(&s, &torpedo_at(100.0, 10.0), 1, 50.0, &t, &deep).is_none());
        s.own_speed_kn = 5.0;
        // A silent target pinging with a 4 km active sonar is heard to 8 km, as an emitter.
        let mut pinger = torpedo_at(7_900.0, 5.0);
        pinger.noise = 0.0;
        pinger.emissions = vec![Emission { source: Default::default(), kind: EmitterKind::ActiveSonar, emitter: 4, r_ref_m: 4_000.0, height_m: -5.0 }];
        let d = passive_core(&s, &pinger, 1, 50.0, &t, &deep).unwrap();
        assert_eq!(d.evidence, Evidence::EMITTER);
        assert_eq!(d.emitter, 4);
        // Across the layer: x 0.3; the water depth is looked up only then.
        let asked = std::cell::Cell::new(0);
        let counted = || { asked.set(asked.get() + 1); 200.0 };
        assert!(passive_core(&s, &torpedo_at(2_000.0, 20.0), 1, 50.0, &t, &counted).is_some());
        assert_eq!(asked.get(), 0);
        assert!(passive_core(&s, &torpedo_at(2_300.0, 100.0), 1, 50.0, &t, &counted).is_some());
        assert!(passive_core(&s, &torpedo_at(2_500.0, 100.0), 1, 50.0, &t, &counted).is_none());
        assert!(asked.get() >= 2);
        // Aircraft and missiles in flight are not heard.
        let mut air = torpedo_at(500.0, 0.0);
        air.domain = Domain::Air;
        assert!(passive_core(&s, &air, 1, 50.0, &t, &deep).is_none());
        let mut skimmer = torpedo_at(500.0, 0.0);
        skimmer.pos.y = 5.0;
        assert!(passive_core(&s, &skimmer, 1, 50.0, &t, &deep).is_none());
    }

    #[test]
    fn active_core_finds_mines_and_reports_depth() {
        let t = SensorTuning::default();
        let deep = || 200.0;
        let s = sonar(SensorKind::ActiveSonar, 4_000.0);
        // A moored mine with a 1 m^2 disc (r = 0.564 m) at 10 m.
        let mut mine = torpedo_at(1_200.0, 10.0);
        mine.noise = 0.0;
        let one = 1.0f32;
        mine.areas = Some(std::sync::Arc::new(sim_core::vessel::ProjectedAreas { rcs_m2: [0.0; 36], above_m2: [one; 36], submerged_m2: [one; 36], top_height_m: 0.0 }));
        let d = active_core(&s, &mine, 1, 50.0, &t, &deep).unwrap();
        assert_eq!(d.evidence, Evidence::DEPTH);
        assert!(matches!(d.measurement, crate::sensors::Measurement::RangeBearing { height_m: Some(h), .. } if h == -10.0));
        assert_eq!(d.sigma_range_m, 20.0);
        mine.pos.z = -1_300.0;
        assert!(active_core(&s, &mine, 1, 50.0, &t, &deep).is_none());
        let mut quiet = s.clone();
        quiet.radiating = false;
        mine.pos.z = -500.0;
        assert!(active_core(&quiet, &mine, 1, 50.0, &t, &deep).is_none());
        assert!(active_core(&sonar(SensorKind::PassiveSonar, 4_000.0), &mine, 1, 50.0, &t, &deep).is_none());
        // A surface hull: submerged area from its table, no depth evidence.
        let hull = crate::sensors::radar::tests::hull_at(3_000.0, 0.0, 0.0, 120.0, 8.0);
        let d = active_core(&s, &hull, 1, 50.0, &t, &deep).unwrap();
        assert_eq!(d.evidence, Evidence::empty());
    }
}
