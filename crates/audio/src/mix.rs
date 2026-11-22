//! Unit `audio-mix`. Runtime rules gdclient applies when playing the rendered files: which file an event uses,
//! and pitch and volume from sim state for engines, sea and weather (L1246). Godot-free and pure, so it tests
//! headless. Alert repetition timings come from L1680-1685.
use crate::{AlertSound, AmbienceSfx, ImpactSfx, SoundId, WeaponSfx};

/// Warning chime repeats after this many seconds while unacknowledged (L1683).
pub const WARNING_REPEAT_S: f32 = 120.0;
/// "Silence all" lasts 30 s (L1687).
pub const SILENCE_ALL_S: f32 = 30.0;
/// Speed of sound in air for report delays, m/s.
pub const SOUND_SPEED_MPS: f32 = 343.0;
/// Gain the client treats as silent (Godot `volume_db` floor); never negative infinity.
pub const SILENT_DB: f32 = -80.0;
/// Inside this listener distance a gun uses its near report; beyond it the distant file.
pub const NEAR_REPORT_M: f32 = 1_500.0;
/// Reference distance of [`distance_gain_db`]: the gain is 0 dB at the source and about -3 dB here.
pub const DISTANCE_REF_M: f32 = 50.0;
/// Rolloff of [`distance_gain_db`]: the fraction of the physical inverse-distance law applied (0.5 is 3 dB
/// per doubling of distance far out, against 6 dB in free air).
pub const ROLLOFF: f32 = 0.5;
/// Air absorption beyond [`ABSORPTION_START_M`], dB per km (the rendered files' low-weighted content).
pub const ABSORPTION_DB_PER_KM: f32 = 0.5;
pub const ABSORPTION_START_M: f32 = 2_000.0;

/// Calibre band upper bounds (mm) used to pick files. The bands are 20-40, 57-76, 100-127 and 130-155 mm
/// (L565-570); calibres between bands go to the nearer band (split at the midpoints 48, 88 and 128 mm).
const AUTO_MAX_MM: u16 = 48;
const MEDIUM_MAX_MM: u16 = 88;
const LARGE_MAX_MM: u16 = 128;

/// NaN and infinities to a finite fallback; then clamp.
fn finite(x: f32, fallback: f32) -> f32 {
    if x.is_finite() { x } else { fallback }
}

/// Linear amplitude to dB, floored at [`SILENT_DB`].
fn amp_db(a: f32) -> f32 {
    if !(a > 1e-4) { SILENT_DB } else { (20.0 * libm::log10f(a)).max(SILENT_DB) }
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Equal-power pair for a crossfade position 0..=1: (outgoing, incoming) linear gains.
fn equal_power(x: f32) -> (f32, f32) {
    let x = x.clamp(0.0, 1.0) * core::f32::consts::FRAC_PI_2;
    (libm::cosf(x), libm::sinf(x))
}

fn lp(pitch: f32, amp: f32) -> LoopParams {
    LoopParams { pitch, gain_db: amp_db(amp) }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Band { Auto, Medium, Large, Heavy }

fn band(calibre_mm: u16) -> Band {
    if calibre_mm <= AUTO_MAX_MM {
        Band::Auto
    } else if calibre_mm <= MEDIUM_MAX_MM {
        Band::Medium
    } else if calibre_mm <= LARGE_MAX_MM {
        Band::Large
    } else {
        Band::Heavy
    }
}

/// Propulsion family, mirroring `content::part::EngineKind` (this crate has no workspace dependencies).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EngineClass { Diesel, GasTurbine, Electric }

/// Pitch scale (1.0 = as rendered) and gain in dB for a looping sound this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoopParams { pub pitch: f32, pub gain_db: f32 }

/// The loop file of an engine class.
pub fn engine_loop(class: EngineClass) -> SoundId {
    SoundId::Ambience(match class { EngineClass::Diesel => AmbienceSfx::EngineDiesel, EngineClass::GasTurbine => AmbienceSfx::EngineGasTurbine, EngineClass::Electric => AmbienceSfx::EngineElectric })
}

/// Engine pitch and gain from shaft power fraction 0..=1 (`VesselView.power`) and spool state. Pitch spans
/// about 0.6x idle to 1.6x full. Unit: `audio-mix`.
///
/// Shaft speed follows the propeller law (power grows with the cube of rpm), so pitch rises with the cube
/// root of power: it climbs quickly off idle and flattens towards full. Gas turbines idle higher (the gas
/// generator never drops far) and electric drives sit quieter. Gain rises with the square root of power.
pub fn engine_params(class: EngineClass, power: f32) -> LoopParams {
    let p = finite(power, 0.0).clamp(0.0, 1.0);
    let rpm = libm::cbrtf(p);
    let (idle_pitch, full_pitch, idle_db, full_db) = match class {
        EngineClass::Diesel => (0.6, 1.6, -14.0, 0.0),
        EngineClass::GasTurbine => (0.75, 1.6, -10.0, 0.0),
        EngineClass::Electric => (0.6, 1.5, -20.0, -4.0),
    };
    LoopParams { pitch: idle_pitch + (full_pitch - idle_pitch) * rpm, gain_db: idle_db + (full_db - idle_db) * libm::sqrtf(p) }
}

/// Hull wash and cavitation loops from speed in knots and the propulsor's cavitation speed (default 20 kn, L453).
/// Returns gains for (slow wash, fast wash, cavitation). Unit: `audio-mix`.
///
/// Slow wash fades in from 1 kn and is full by 6 kn; between 10 and 22 kn it crossfades (equal power) into
/// fast wash, whose level keeps growing towards 35 kn. Cavitation is silent below the cavitation speed and
/// fades in over the next 4 kn. Pitch rises gently with speed. Negative speed (astern) uses its magnitude; a
/// non-finite or non-positive cavitation speed falls back to 20 kn.
pub fn wash_params(speed_kn: f32, cavitation_kn: f32) -> [LoopParams; 3] {
    let v = finite(speed_kn, 0.0).abs().min(80.0);
    let cav = { let c = finite(cavitation_kn, 20.0); if c > 0.0 { c } else { 20.0 } };
    let presence = smoothstep(1.0, 6.0, v);
    let (slow, fast) = equal_power((v - 10.0) / 12.0);
    let fast_level = 0.6 + 0.4 * smoothstep(15.0, 35.0, v);
    let wash_pitch = 0.85 + 0.3 * (v / 35.0).min(1.0);
    let cav_amt = smoothstep(cav, cav + 4.0, v);
    let cav_pitch = 0.9 + 0.25 * ((v - cav).max(0.0) / 15.0).min(1.0);
    [
        lp(wash_pitch, presence * slow),
        lp(wash_pitch, presence * fast * fast_level),
        lp(cav_pitch, cav_amt),
    ]
}

/// Crossfaded sea beds for a WMO sea-state code 0..=8 and wind speed: gains for calm, moderate, rough and storm
/// (equal-power crossfades between neighbouring beds). Unit: `audio-mix`.
///
/// The sea state places the listener on a continuous scale; wind against the state's typical wind (WMO
/// table, about 0, 2, 4, 6.5, 9, 12, 15.5, 20 and 25 m/s) nudges it by up to half a state either way. Beds
/// sit at their ranges' centres (calm 1, moderate 3.5, rough 5.5, storm 7.5) and neighbouring beds
/// crossfade with equal power, so the squared gains always sum to 1. Below the calm centre a glassy sea
/// drops up to 6 dB. Codes above 8 clamp to 8.
pub fn sea_params(sea_state: u8, wind_mps: f32) -> [LoopParams; 4] {
    const TYPICAL_WIND: [f32; 9] = [0.0, 2.0, 4.0, 6.5, 9.0, 12.0, 15.5, 20.0, 25.0];
    const CENTRES: [f32; 4] = [1.0, 3.5, 5.5, 7.5];
    let code = sea_state.min(8) as usize;
    let wind = finite(wind_mps, TYPICAL_WIND[code]).max(0.0);
    let span = if code < 8 { TYPICAL_WIND[code + 1] - TYPICAL_WIND[code] } else { TYPICAL_WIND[8] - TYPICAL_WIND[7] };
    let nudge = ((wind - TYPICAL_WIND[code]) / span.max(1.0)).clamp(-0.5, 0.5);
    let s = (code as f32 + nudge).clamp(0.0, 8.0);
    let mut amps = [0.0f32; 4];
    if s <= CENTRES[0] {
        amps[0] = 1.0;
    } else if s >= CENTRES[3] {
        amps[3] = 1.0;
    } else {
        for i in 0..3 {
            if s >= CENTRES[i] && s <= CENTRES[i + 1] {
                let (a, b) = equal_power((s - CENTRES[i]) / (CENTRES[i + 1] - CENTRES[i]));
                amps[i] = a;
                amps[i + 1] = b;
                break;
            }
        }
    }
    let glassy_db = -6.0 * (1.0 - s.min(1.0));
    let mut out = [LoopParams { pitch: 1.0, gain_db: SILENT_DB }; 4];
    for (o, &a) in out.iter_mut().zip(amps.iter()) {
        o.gain_db = if a > 1e-4 { (amp_db(a) + glassy_db).max(SILENT_DB) } else { SILENT_DB };
    }
    out
}

/// Wind (normal, gale) and rain (light, heavy) gains from wind speed and rain 0..=1. Unit: `audio-mix`.
///
/// Wind is silent below 2 m/s and reaches full level at 10 m/s; it crossfades (equal power) into the gale
/// bed between 14 and 22 m/s, with pitch rising slightly with speed. Rain fades in from 0.02 and crossfades
/// into heavy rain between 0.45 and 0.85. Returns [wind, gale, rain, heavy rain].
pub fn weather_params(wind_mps: f32, rain: f32) -> [LoopParams; 4] {
    let w = finite(wind_mps, 0.0).clamp(0.0, 60.0);
    let r = finite(rain, 0.0).clamp(0.0, 1.0);
    let wind_presence = smoothstep(2.0, 10.0, w);
    let (calm, gale) = equal_power((w - 14.0) / 8.0);
    let gale_level = 0.7 + 0.3 * smoothstep(22.0, 32.0, w);
    let wind_pitch = 0.9 + 0.25 * (w / 30.0).min(1.0);
    let rain_presence = smoothstep(0.02, 0.25, r);
    let (light, heavy) = equal_power((r - 0.45) / 0.4);
    [
        lp(wind_pitch, wind_presence * calm),
        lp(wind_pitch, wind_presence * gale * gale_level),
        lp(1.0, rain_presence * light),
        lp(1.0, rain_presence * heavy),
    ]
}

/// Gun report for a calibre at a listener distance: near band file inside about 1.5 km, distant file beyond.
/// Unit: `audio-mix`.
/// Calibres between bands use the nearer band; a non-finite distance counts as near (NaN) or distant (+inf).
pub fn gun_report(calibre_mm: u16, distance_m: f32) -> SoundId {
    let b = band(calibre_mm);
    let near = if distance_m.is_nan() { true } else { distance_m < NEAR_REPORT_M };
    SoundId::Weapon(match (near, b) {
        (true, Band::Auto) => WeaponSfx::GunAuto,
        (true, Band::Medium) => WeaponSfx::GunMedium,
        (true, Band::Large) => WeaponSfx::GunLarge,
        (true, Band::Heavy) => WeaponSfx::GunHeavy,
        (false, Band::Auto | Band::Medium) => WeaponSfx::GunDistantLight,
        (false, Band::Large | Band::Heavy) => WeaponSfx::GunDistantHeavy,
    })
}

/// Gain in dB to add to [`gun_report`]'s file for a calibre at a distance: 0 for a near report; for the
/// distant file (rendered as loud as the largest near report), the step down to the calibre band's near
/// level, so a report never gets louder as it crosses [`NEAR_REPORT_M`]. Unit: `audio-mix`.
pub fn gun_report_trim_db(calibre_mm: u16, distance_m: f32) -> f32 {
    let near = if distance_m.is_nan() { true } else { distance_m < NEAR_REPORT_M };
    if near {
        return 0.0;
    }
    match band(calibre_mm) {
        Band::Auto => -4.0,
        Band::Medium => -2.0,
        Band::Large | Band::Heavy => 0.0,
    }
}

/// Splash file by calibre (autocannon small, 57-76 medium, 100 and up large). Unit: `audio-mix`.
/// Missiles and torpedoes broaching use the large file (pass their warhead as a large calibre, e.g. 127).
pub fn splash_sound(calibre_mm: u16) -> SoundId {
    SoundId::Impact(match band(calibre_mm) {
        Band::Auto => ImpactSfx::SplashSmall,
        Band::Medium => ImpactSfx::SplashMedium,
        Band::Large | Band::Heavy => ImpactSfx::SplashLarge,
    })
}

/// Burst file by HE B buckets (L580): small below 6, large from 6. Unit: `audio-mix`.
/// Any underwater burst (torpedo, mine, depth charge) uses the underwater file.
pub fn explosion_sound(he_b: u8, underwater: bool) -> SoundId {
    SoundId::Impact(if underwater {
        ImpactSfx::UnderwaterBurst
    } else if he_b < 6 {
        ImpactSfx::ExplosionSmall
    } else {
        ImpactSfx::ExplosionLarge
    })
}

/// Distance gain in dB for positional sounds, never above 0 dB. Unit: `audio-mix`.
///
/// `-20 * ROLLOFF * log10(1 + d / DISTANCE_REF_M)`, plus [`ABSORPTION_DB_PER_KM`] beyond
/// [`ABSORPTION_START_M`], floored at [`SILENT_DB`]: about -5 dB at 100 m, -13 dB at 1 km, -21 dB at 5 km and
/// -27 dB at 10 km. NaN and negative distances count as 0 m; +inf is silent.
///
/// DECISION (the spec sets no law): half the free-air rolloff. Inverse distance in full puts an enemy gun at
/// 5 km 40 dB down, under the sea bed, and a gun at range is the cue a captain listens for; games compress
/// distance the same way. The `1 +` keeps the curve smooth through the reference instead of a flat 0 dB
/// plateau that made everything inside 50 m equally loud.
pub fn distance_gain_db(distance_m: f32) -> f32 {
    let d = if distance_m.is_nan() { 0.0 } else { distance_m.max(0.0) };
    if d.is_infinite() {
        return SILENT_DB;
    }
    let spread = -20.0 * ROLLOFF * libm::log10f(1.0 + d / DISTANCE_REF_M);
    let absorb = -ABSORPTION_DB_PER_KM * (d - ABSORPTION_START_M).max(0.0) / 1_000.0;
    (spread + absorb).clamp(SILENT_DB, 0.0)
}

/// Start of the fade to silence of [`range_gain_db`], as a fraction of the range.
pub const RANGE_FADE_FROM: f32 = 0.6;

/// [`distance_gain_db`] faded smoothly to silence between `RANGE_FADE_FROM * range_m` and `range_m`, the
/// distance past which a family is not played, so a sound never cuts off at a hard edge. A non-positive or
/// non-finite range means no fade. Unit: `audio-mix`.
pub fn range_gain_db(distance_m: f32, range_m: f32) -> f32 {
    let g = distance_gain_db(distance_m);
    if !(range_m.is_finite() && range_m > 0.0) {
        return g;
    }
    let d = if distance_m.is_nan() { 0.0 } else { distance_m.max(0.0) };
    let fade = 1.0 - smoothstep(RANGE_FADE_FROM * range_m, range_m, d);
    if fade <= 1e-4 { SILENT_DB } else { (g + amp_db(fade)).max(SILENT_DB) }
}

/// Arrival delay of a sound from `distance_m` (gun flashes precede their reports). Thunder after lightning uses
/// the same rule.
pub fn arrival_delay_s(distance_m: f32) -> f32 { distance_m.max(0.0) / SOUND_SPEED_MPS }

/// Alert file for each sounded priority, ascending urgency.
pub fn alert_sound(priority_index: u8) -> SoundId {
    SoundId::Alert(match priority_index { 0 => AlertSound::Caution, 1 => AlertSound::Warning, 2 => AlertSound::Alarm, _ => AlertSound::Emergency })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_pitch_and_gain_span() {
        for class in [EngineClass::Diesel, EngineClass::GasTurbine, EngineClass::Electric] {
            let idle = engine_params(class, 0.0);
            let full = engine_params(class, 1.0);
            assert!(idle.pitch >= 0.6 - 1e-6 && idle.pitch < 0.8);
            assert!(full.pitch >= 1.5 - 1e-6 && full.pitch <= 1.6 + 1e-6);
            assert!(idle.gain_db < full.gain_db && full.gain_db <= 0.0);
            let mut last = engine_params(class, 0.0);
            for i in 1..=20 {
                let p = engine_params(class, i as f32 / 20.0);
                assert!(p.pitch > last.pitch && p.gain_db > last.gain_db);
                last = p;
            }
            assert_eq!(engine_params(class, f32::NAN), idle);
            assert_eq!(engine_params(class, 7.0), full);
            assert_eq!(engine_params(class, -1.0), idle);
        }
        assert!((engine_params(EngineClass::Diesel, 0.0).pitch - 0.6).abs() < 1e-6);
        assert!((engine_params(EngineClass::Diesel, 1.0).pitch - 1.6).abs() < 1e-6);
    }

    #[test]
    fn wash_and_cavitation() {
        let still = wash_params(0.0, 20.0);
        assert!(still.iter().all(|p| p.gain_db == SILENT_DB && p.pitch > 0.0));
        let slow = wash_params(6.0, 20.0);
        assert!(slow[0].gain_db > -0.5 && slow[1].gain_db == SILENT_DB);
        let fast = wash_params(30.0, 20.0);
        assert!(fast[0].gain_db == SILENT_DB && fast[1].gain_db > -6.0);
        assert_eq!(wash_params(19.9, 20.0)[2].gain_db, SILENT_DB);
        assert!(wash_params(25.0, 20.0)[2].gain_db > -0.5);
        // Fallbacks.
        assert_eq!(wash_params(25.0, f32::NAN), wash_params(25.0, 20.0));
        assert_eq!(wash_params(-10.0, 20.0), wash_params(10.0, 20.0));
        assert!(wash_params(f32::INFINITY, 20.0).iter().all(|p| p.gain_db.is_finite() && p.pitch.is_finite()));
    }

    fn power(p: &[LoopParams]) -> f32 {
        p.iter().map(|x| if x.gain_db <= SILENT_DB { 0.0 } else { libm::powf(10.0, x.gain_db / 10.0) }).sum()
    }

    #[test]
    fn sea_beds_are_equal_power() {
        for code in 1..=8u8 {
            for wind in [0.0f32, 3.0, 9.0, 15.0, 30.0] {
                let p = sea_params(code, wind);
                let pw = power(&p);
                // Code 1 with no wind sits below the calm centre and takes the glassy trim.
                if code == 1 && wind < 2.0 { assert!(pw <= 1.0 + 1e-3); continue; }
                assert!((pw - 1.0).abs() < 1e-3, "code {code} wind {wind} power {pw}");
                assert!(p.iter().all(|x| x.pitch == 1.0));
            }
        }
        assert!(sea_params(0, 0.0)[0].gain_db < -5.0);
        assert_eq!(sea_params(1, 2.0)[0].gain_db, 0.0);
        assert_eq!(sea_params(8, 25.0)[3].gain_db, 0.0);
        assert_eq!(sea_params(200, 25.0), sea_params(8, 25.0));
        // Moderate sea state 4 has moderate as the dominant bed.
        let m = sea_params(4, 9.0);
        assert!(m[1].gain_db > m[2].gain_db && m[0].gain_db == SILENT_DB);
        assert!(sea_params(5, f32::NAN).iter().all(|x| x.gain_db.is_finite()));
    }

    #[test]
    fn weather_beds() {
        assert!(weather_params(0.0, 0.0).iter().all(|p| p.gain_db == SILENT_DB && p.pitch > 0.0));
        let w = weather_params(10.0, 0.0);
        assert!(w[0].gain_db > -0.5 && w[1].gain_db == SILENT_DB);
        let g = weather_params(26.0, 0.0);
        assert!(g[0].gain_db == SILENT_DB && g[1].gain_db > -3.0);
        let r = weather_params(0.0, 0.3);
        assert!(r[2].gain_db > -1.0 && r[3].gain_db == SILENT_DB);
        let h = weather_params(0.0, 1.0);
        assert!(h[2].gain_db == SILENT_DB && h[3].gain_db > -0.5);
        assert_eq!(weather_params(f32::NAN, f32::NAN), weather_params(0.0, 0.0));
    }

    #[test]
    fn file_choice() {
        use WeaponSfx::*;
        assert_eq!(gun_report(20, 100.0), SoundId::Weapon(GunAuto));
        assert_eq!(gun_report(40, 1_499.0), SoundId::Weapon(GunAuto));
        assert_eq!(gun_report(76, 500.0), SoundId::Weapon(GunMedium));
        assert_eq!(gun_report(57, 5_000.0), SoundId::Weapon(GunDistantLight));
        assert_eq!(gun_report(127, 10.0), SoundId::Weapon(GunLarge));
        assert_eq!(gun_report(100, 2_000.0), SoundId::Weapon(GunDistantHeavy));
        assert_eq!(gun_report(155, 10.0), SoundId::Weapon(GunHeavy));
        assert_eq!(gun_report(155, f32::INFINITY), SoundId::Weapon(GunDistantHeavy));
        assert_eq!(gun_report(155, f32::NAN), SoundId::Weapon(GunHeavy));
        assert_eq!(gun_report(0, 0.0), SoundId::Weapon(GunAuto));
        assert_eq!(gun_report(u16::MAX, 0.0), SoundId::Weapon(GunHeavy));
        assert_eq!(splash_sound(30), SoundId::Impact(ImpactSfx::SplashSmall));
        assert_eq!(splash_sound(76), SoundId::Impact(ImpactSfx::SplashMedium));
        assert_eq!(splash_sound(100), SoundId::Impact(ImpactSfx::SplashLarge));
        assert_eq!(explosion_sound(4, false), SoundId::Impact(ImpactSfx::ExplosionSmall));
        assert_eq!(explosion_sound(5, false), SoundId::Impact(ImpactSfx::ExplosionSmall));
        assert_eq!(explosion_sound(6, false), SoundId::Impact(ImpactSfx::ExplosionLarge));
        assert_eq!(explosion_sound(8, true), SoundId::Impact(ImpactSfx::UnderwaterBurst));
    }

    #[test]
    fn distance_gain_law() {
        assert_eq!(distance_gain_db(0.0), 0.0);
        assert!((distance_gain_db(DISTANCE_REF_M) + 10.0 * ROLLOFF * 0.60206).abs() < 0.01);
        assert!((distance_gain_db(100.0) + 10.0 * libm::log10f(3.0)).abs() < 0.01);
        assert!((distance_gain_db(1_000.0) + 10.0 * libm::log10f(21.0)).abs() < 0.01);
        let below = distance_gain_db(1_999.999);
        let above = distance_gain_db(2_000.001);
        assert!((below - above).abs() < 0.01);
        assert!((distance_gain_db(4_000.0) - (-10.0 * libm::log10f(81.0) - 1.0)).abs() < 0.01);
        // A gun at 5 km stays well above the player's -60 dB floor.
        assert!(distance_gain_db(5_000.0) > -25.0 && distance_gain_db(5_000.0) < -18.0);
        let mut last = 0.0;
        let mut d = 1.0;
        while d < 1.0e6 {
            let g = distance_gain_db(d);
            assert!(g <= last + 1e-6 && g <= 0.0 && g >= SILENT_DB);
            last = g;
            d *= 1.3;
        }
        assert_eq!(distance_gain_db(f32::NAN), 0.0);
        assert_eq!(distance_gain_db(-5.0), 0.0);
        assert_eq!(distance_gain_db(f32::INFINITY), SILENT_DB);
        assert!((arrival_delay_s(343.0) - 1.0).abs() < 1e-6);
        assert_eq!(arrival_delay_s(-1.0), 0.0);
    }

    #[test]
    fn range_fades_to_silence() {
        assert_eq!(range_gain_db(1_000.0, 6_000.0), distance_gain_db(1_000.0));
        assert!(range_gain_db(4_000.0, 6_000.0) < distance_gain_db(4_000.0));
        assert_eq!(range_gain_db(6_000.0, 6_000.0), SILENT_DB);
        assert_eq!(range_gain_db(9_000.0, 6_000.0), SILENT_DB);
        // Falls steadily, and the fade itself never steps more than 3 dB per 1% of the range until it is
        // 20 dB down.
        let mut last = range_gain_db(0.0, 6_000.0);
        let mut last_fade = 0.0;
        for i in 1..=99 {
            let d = i as f32 * 60.0;
            let g = range_gain_db(d, 6_000.0);
            let fade = g - distance_gain_db(d);
            assert!(g <= last + 1e-6, "{i}: {last} -> {g}");
            if last_fade > -20.0 { assert!(last_fade - fade < 3.0, "{i}: fade {last_fade} -> {fade}"); }
            last = g;
            last_fade = fade;
        }
        assert_eq!(range_gain_db(500.0, 0.0), distance_gain_db(500.0));
        assert_eq!(range_gain_db(500.0, f32::NAN), distance_gain_db(500.0));
    }

    #[test]
    fn alerts_ascend() {
        assert_eq!(alert_sound(0), SoundId::Alert(AlertSound::Caution));
        assert_eq!(alert_sound(3), SoundId::Alert(AlertSound::Emergency));
        assert_eq!(alert_sound(9), SoundId::Alert(AlertSound::Emergency));
    }
}
