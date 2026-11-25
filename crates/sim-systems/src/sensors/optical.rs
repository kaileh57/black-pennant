//! Unit `sys-detect-optical`. Visual lookouts and camera masts. DESIGN.md L436.
//!
//! R_vis = 10 km x sqrt(A(theta) / 100 m^2) with A the projected above-water area at that aspect; a camera
//! mast doubles it. Capped by the radar-horizon cull and by weather visibility V. At night R_vis x 0.3 unless
//! the target fired in the last 10 s, is burning or shows lights. A smoke screen blocks every visual or camera
//! line of sight that crosses it (`World::line_of_sight` with `optical = true`).
//! CI invariant: over 10^5 sampled pairs per weather preset, no visual or camera detection beyond V or the
//! horizon.
use super::radar::{horizon_m, in_view, pair_ok, range_bearing, sight_point};
use super::{Detection, Evidence, SenseEnv, SensorInstance};
use crate::signature::{compass_bearing, Signature};
use content::part::SensorKind;
use content::tuning::SensorTuning;

/// Reference above-water area of the visual law, m^2 (L436).
pub const VISUAL_REF_AREA_M2: f64 = 100.0;

/// R_vis before the visibility and horizon caps: the law, the camera factor and the night factor. This is
/// the R_vis the identification thresholds (50% and 25%) are measured against.
pub fn visual_law_m(area_m2: f64, camera: bool, night: bool, night_exempt: bool, tuning: &SensorTuning) -> f64 {
    let a = if area_m2.is_finite() && area_m2 > 0.0 { area_m2 } else { return 0.0 };
    let mut r = pos(tuning.visual_ref_range_m) * (a / VISUAL_REF_AREA_M2).sqrt();
    if camera { r *= pos(tuning.camera_range_factor); }
    if night && !night_exempt { r *= pos(tuning.night_visual_factor); }
    pos(r)
}

fn pos(x: f64) -> f64 { if x.is_finite() && x > 0.0 { x } else { 0.0 } }

/// The gates shared by [`detect_visual`] and [`in_visual_range`], short of line of sight. Returns (range,
/// bearing, R_vis uncapped). Cull order: working lookout or camera, target above water, visibility and
/// horizon caps, field of view, then the aspect area and the law.
pub(crate) fn visual_core(sensor: &SensorInstance, target: &Signature, tick: u64, night: bool, visibility_m: f64, tuning: &SensorTuning) -> Option<(f64, f64, f64)> {
    if !matches!(sensor.kind, SensorKind::Visual | SensorKind::Camera) || !sensor.active || !pair_ok(sensor, target) { return None; }
    if target.is_submerged() { return None; }
    let d = target.pos - sensor.pos;
    let range = d.length();
    let cap = pos(visibility_m).min(horizon_m(sensor.height_m, target.top_m));
    if !range.is_finite() || range > cap { return None; }
    let bearing = compass_bearing(d);
    if !in_view(sensor, bearing, tick) { return None; }
    let exempt = target.fired_recently || target.burning || target.lights;
    let camera = sensor.kind == SensorKind::Camera;
    let law = visual_law_m(target.visual_area_toward(sensor.pos), camera, night, exempt, tuning) * sensor.output;
    let reach = law.min(cap);
    if range > reach || reach <= 0.0 { return None; }
    Some((range, bearing, law))
}

/// The detection a sighting at `range` makes, with the identification evidence against R_vis `law`.
pub(crate) fn sighting(sensor: &SensorInstance, target: &Signature, tick: u64, range: f64, bearing: f64, law: f64) -> Detection {
    let mut evidence = Evidence::empty();
    if range <= 0.5 * law { evidence |= Evidence::VISUAL_HALF; }
    if range <= 0.25 * law { evidence |= Evidence::VISUAL_QUARTER; }
    range_bearing(sensor, target, tick, range, bearing, None, evidence, 0.0, Some(target.domain))
}

fn visual_pair(sensor: &SensorInstance, target: &Signature, env: &SenseEnv<'_>) -> Option<(f64, f64, f64)> {
    let r = visual_core(sensor, target, env.ctx.tick.0, env.night, env.ctx.weather.visibility_m, &env.ctx.tuning.sensors)?;
    if !env.world.line_of_sight(sensor.pos, sight_point(target), true, [sensor.id.carrier, target.entity]) { return None; }
    Some(r)
}

/// Visual range in metres before line-of-sight tests. Unit: `sys-detect-optical`.
pub fn visual_range_m(area_m2: f64, camera: bool, night: bool, night_exempt: bool, visibility_m: f64, horizon_m: f64, tuning: &content::tuning::SensorTuning) -> f64 {
    visual_law_m(area_m2, camera, night, night_exempt, tuning).min(pos(visibility_m)).min(pos(horizon_m))
}

/// Visual or camera detection of one target, or `None`. Range error is 10% of range, bearing 0.2 degrees.
/// Sets `Evidence::VISUAL_HALF` inside 50% of R_vis and `VISUAL_QUARTER` inside 25% (L474-475).
/// Unit: `sys-detect-optical`.
///
/// The 50% and 25% thresholds are fractions of R_vis as the law gives it (camera and night factors and damage
/// output included, visibility and horizon caps not): a hull seen at 300 m in fog is identified even though
/// fog caps sighting at 400 m. A sighting also reports the target's domain, which the eye can tell.
pub fn detect_visual(sensor: &SensorInstance, target: &Signature, env: &SenseEnv<'_>) -> Option<Detection> {
    let (range, bearing, law) = visual_pair(sensor, target, env)?;
    Some(sighting(sensor, target, env.ctx.tick.0, range, bearing, law))
}

/// True when an entity at `target_pos` is inside the visual range of the given sensor (used by interest
/// management to decide which true poses a side may draw, L529). Unit: `sys-detect-optical`.
///
/// Exactly the gates of [`detect_visual`] (working lookout or camera, field of view, night rule, visibility and
/// horizon caps, land, hull and smoke line of sight), so a side draws precisely what its eyes could report.
pub fn in_visual_range(sensor: &SensorInstance, target: &Signature, env: &SenseEnv<'_>) -> bool { visual_pair(sensor, target, env).is_some() }

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> SensorTuning { SensorTuning::default() }

    #[test]
    fn visual_law_and_factors() {
        let tn = t();
        // 100 m^2 is 10 km; a 150 t craft (~150 m^2) about 12.2 km; a camera doubles; night x 0.3.
        assert!((visual_law_m(100.0, false, false, false, &tn) - 10_000.0).abs() < 1e-9);
        assert!((visual_law_m(150.0, false, false, false, &tn) - 12_247.4).abs() < 0.1);
        assert!((visual_law_m(100.0, true, false, false, &tn) - 20_000.0).abs() < 1e-9);
        assert!((visual_law_m(100.0, false, true, false, &tn) - 3_000.0).abs() < 1e-9);
        assert!((visual_law_m(100.0, false, true, true, &tn) - 10_000.0).abs() < 1e-9);
        assert_eq!(visual_law_m(0.0, true, false, false, &tn), 0.0);
        assert_eq!(visual_law_m(f64::NAN, true, false, false, &tn), 0.0);
        // Caps: fog bank and horizon.
        assert_eq!(visual_range_m(7_000.0, false, false, false, 400.0, 30_000.0, &tn), 400.0);
        assert_eq!(visual_range_m(7_000.0, true, false, false, 30_000.0, 18_000.0, &tn), 18_000.0);
    }

    #[test]
    fn never_beyond_visibility_or_horizon() {
        // The L436 invariant on the range function, over 10^5 deterministic samples per weather preset.
        use content::weather::WeatherKind;
        let tn = t();
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || { x ^= x << 13; x ^= x >> 7; x ^= x << 17; (x >> 11) as f64 / (1u64 << 53) as f64 };
        for kind in [WeatherKind::Clear, WeatherKind::Haze, WeatherKind::Overcast, WeatherKind::Rain, WeatherKind::Storm, WeatherKind::FogBank] {
            let v = kind.visibility_m();
            for _ in 0..100_000 {
                let area = next() * 20_000.0;
                let (ha, ht) = (next() * 40.0, next() * 1_200.0);
                let h = crate::sensors::radar::horizon_m(ha, ht);
                let r = visual_range_m(area, next() < 0.5, next() < 0.5, next() < 0.5, v, h, &tn);
                assert!(r <= v && r <= h && r >= 0.0);
            }
        }
    }

    #[test]
    fn sighting_gates_night_fog_and_evidence() {
        use crate::sensors::radar::tests::{hull_at, sensor};
        let tn = t();
        let eye = sensor(SensorKind::Visual);
        // 150 m^2 above water: R_vis 12.25 km; horizon from 20 m to a 10 m top is 31.45 km.
        let far = hull_at(12_000.0, 0.0, 150.0, 0.0, 10.0);
        let (r, _, law) = visual_core(&eye, &far, 1, false, 30_000.0, &tn).unwrap();
        assert!((law - 12_247.4).abs() < 0.1);
        assert_eq!(sighting(&eye, &far, 1, r, 0.0, law).evidence, Evidence::empty());
        assert!(visual_core(&eye, &hull_at(12_500.0, 0.0, 150.0, 0.0, 10.0), 1, false, 30_000.0, &tn).is_none());
        let mid = hull_at(5_000.0, 0.0, 150.0, 0.0, 10.0);
        let (r, b, law) = visual_core(&eye, &mid, 1, false, 30_000.0, &tn).unwrap();
        let d = sighting(&eye, &mid, 1, r, b, law);
        assert_eq!(d.evidence, Evidence::VISUAL_HALF);
        assert!((d.sigma_range_m - 500.0).abs() < 0.01, "10% of range");
        assert_eq!(d.domain_hint, Some(content::enums::Domain::Surface));
        let near = hull_at(3_000.0, 0.0, 150.0, 0.0, 10.0);
        let (r, b, law) = visual_core(&eye, &near, 1, false, 30_000.0, &tn).unwrap();
        assert_eq!(sighting(&eye, &near, 1, r, b, law).evidence, Evidence::VISUAL_HALF | Evidence::VISUAL_QUARTER);
        // Night: x 0.3 (3.67 km) unless lit, burning or recently fired.
        assert!(visual_core(&eye, &mid, 1, true, 30_000.0, &tn).is_none());
        let mut lit = mid.clone();
        lit.lights = true;
        assert!(visual_core(&eye, &lit, 1, true, 30_000.0, &tn).is_some());
        let mut fired = mid.clone();
        fired.fired_recently = true;
        assert!(visual_core(&eye, &fired, 1, true, 30_000.0, &tn).is_some());
        // Fog: nothing past V = 400 m, but what is seen inside it is seen well.
        assert!(visual_core(&eye, &hull_at(450.0, 0.0, 150.0, 0.0, 10.0), 1, false, 400.0, &tn).is_none());
        let fog = hull_at(300.0, 0.0, 150.0, 0.0, 10.0);
        let (r, b, law) = visual_core(&eye, &fog, 1, false, 400.0, &tn).unwrap();
        assert!(sighting(&eye, &fog, 1, r, b, law).evidence.contains(Evidence::VISUAL_QUARTER));
        // Camera: doubles range but only inside its narrow field of view.
        let mut cam = sensor(SensorKind::Camera);
        cam.fov_half_rad = 5f64.to_radians();
        cam.bearing_rad = 0.0;
        assert!(visual_core(&cam, &hull_at(20_000.0, 0.0, 150.0, 0.0, 10.0), 1, false, 30_000.0, &tn).is_some());
        cam.bearing_rad = 90f64.to_radians();
        assert!(visual_core(&cam, &mid, 1, false, 30_000.0, &tn).is_none());
        // A dived hull or a radar-only sensor never sights.
        let mut dived = mid.clone();
        dived.top_m = -2.0;
        dived.depth_m = 15.0;
        assert!(visual_core(&eye, &dived, 1, false, 30_000.0, &tn).is_none());
        assert!(visual_core(&sensor(SensorKind::SearchRadar), &mid, 1, false, 30_000.0, &tn).is_none());
    }
}
