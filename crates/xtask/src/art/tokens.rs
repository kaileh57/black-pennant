//! Unit `xt-art-tokens`. The world token table, colour conversions and the committed JSON export.
//! DESIGN.md L1788-1803 (see `art/mod.rs`), L1803 (hull albedo clamps: Y <= 0.6, C <= 0.06), L1818 (albedo
//! within token Y and C), L1824 (no teal-orange grades), L1868 (preset scope).
//!
//! Colour math (OKLab per Ottosson, sRGB transfer, CIE L*) is f64 with libm, so the export is byte-stable.
//! `content-blueprint`'s `default_paint` must produce the same linear RGB as [`swatch_colour`] for the eight
//! swatches and antifouling; that unit copies the values from the committed JSON.
//!
//! # Authoring rule
//! Every colour is authored as linear luminance Y plus a small OKLCH chroma and hue. [`swatch_colour`] solves
//! OKLCH lightness so the colour's luminance equals Y; when the colour would leave the sRGB gamut, chroma is
//! reduced first (the L1803 clamp order) and the export records the authored chroma as `chroma_clamped_from`.
//!
//! # Presets (L1857-1872)
//! The 18 weather x time-of-day presets derive from two small tables: a per-time base (the clear sky, the key
//! light, light energies, exposure and the base grade) and a per-weather modifier (how much the sun dims and
//! whitens, how far the sky greys toward the horizon, how the fog colour shifts, desaturation). The horizon and
//! fog colours are one value, so distant hulls fog into the sky they sit against (G2). `ambient` is the sky
//! colour by area (65% zenith, 35% horizon, each at unit luminance) with its energy folded in (luminance =
//! energy, exported as `ambient_energy`), never under `ambient_floor` (L1821). `sun_color` peaks at 1 and `sun_energy` carries the intensity.
//! Phases: the 14 presets of 0.1.0 are `Phase::R010`; the four remaining nights are `Phase::R100` (L1868).
//!
//! # LUT grade
//! Applied to AgX output (display-encoded 0..1) per channel: `v = gain * (x + lift * (1 - x))`, then
//! `v^(1 / gamma)`, then saturation about the Rec. 709 luma of the result, clamped to 0..1. Lift and gain casts
//! always share a hue family per preset, so no preset split-tones shadows against highlights (no teal-orange,
//! L1824); [`validate`] rejects any grade whose shadow and highlight casts point more than 90 degrees apart.
//!
//! # Ramps (L1953-1955)
//! Stops are (t, linear RGBA, emission). Emission follows the ladder in `hull_mesh::vfx::emission`: flash up to
//! 16, fire 2 to 4, everything else 1 or less. A ramp either emits (alpha 1 throughout) or carries opacity
//! (emission 0 throughout); `gen/art.rs` packs that one envelope into the texture's alpha. The envelope's
//! first maximum sits in the first 25% of life.
use super::{Preset, Ramp, Swatch, SwatchColour, WorldTokens};
use content::enums::{Phase, TimeOfDay};
use content::weather::WeatherKind;
use hull_mesh::vfx::{emission, RampId};
use std::path::Path;

// ---------------------------------------------------------------------------------------------------------
// Authored tables
// ---------------------------------------------------------------------------------------------------------

/// Clear-sky colours of one time of day; the 6 weather presets of that time derive from them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SkyTokens {
    pub(crate) time: TimeOfDay,
    pub(crate) zenith: Swatch,
    pub(crate) horizon: Swatch,
    /// Sun at dusk and day, the moon at night.
    pub(crate) key_light: Swatch,
}

const fn sw(token: &'static str, citation: &'static str, munsell: &'static str, y: f64, chroma: f64, hue_deg: f64) -> Swatch {
    Swatch { token, citation, munsell, y, chroma, hue_deg }
}

/// The spec's hull table (L1790-1799): token, USN paint, Y, neutral sRGB, L*.
const HULL_TABLE: [(&str, &str, f64, u8, f64); 8] = [
    ("hull.pale", "5-P Pale Gray", 0.55, 196, 79.0),
    ("hull.light", "5-L Light Gray", 0.35, 160, 66.0),
    ("hull.haze", "5-H Haze Gray", 0.28, 144, 60.0),
    ("hull.ocean", "5-O Ocean Gray", 0.17, 115, 48.0),
    ("hull.sea", "5-S Sea Blue", 0.11, 93, 40.0),
    ("hull.navy", "5-N Navy Blue", 0.09, 85, 36.0),
    ("hull.deck", "20-B Deck Blue", 0.05, 63, 27.0),
    ("hull.black", "Dull Black", 0.03, 48, 20.0),
];

/// Hull paint limits (L1803) and marking limits.
const HULL_MAX_Y: f64 = 0.6;
const HULL_MAX_CHROMA: f64 = 0.06;
const MARKING_MAX_Y: f64 = 0.7;
const MARKING_MAX_CHROMA: f64 = 0.12;
/// OKLCH hue of Munsell 5PB (L1788), and the window accepted around it.
const HUE_5PB_DEG: f64 = 265.0;
const HUE_5PB_TOLERANCE_DEG: f64 = 15.0;

fn hull_swatches() -> Vec<Swatch> {
    // Chroma rises as value falls, so every swatch sits near Munsell chroma 0.5 to 2.5 of 5PB: tone over hue.
    vec![
        sw("hull.pale", "USN 5-P Pale Gray (Ships-2 camouflage paints); Y per DESIGN.md L1790-1799", "5PB 7.8/0.5", 0.55, 0.008, HUE_5PB_DEG),
        sw("hull.light", "USN 5-L Light Gray (Ships-2 camouflage paints); Y per DESIGN.md L1790-1799", "5PB 6.5/1", 0.35, 0.012, HUE_5PB_DEG),
        sw("hull.haze", "USN 5-H Haze Gray (Ships-2 camouflage paints); Y per DESIGN.md L1790-1799", "5PB 5.9/1", 0.28, 0.016, HUE_5PB_DEG),
        sw("hull.ocean", "USN 5-O Ocean Gray (Ships-2 camouflage paints); Y per DESIGN.md L1790-1799", "5PB 4.7/1.5", 0.17, 0.022, HUE_5PB_DEG),
        sw("hull.sea", "USN 5-S Sea Blue (Ships-2 camouflage paints); Y per DESIGN.md L1790-1799", "5PB 3.9/2", 0.11, 0.032, HUE_5PB_DEG),
        sw("hull.navy", "USN 5-N Navy Blue (Ships-2 camouflage paints); Y per DESIGN.md L1790-1799", "5PB 3.5/2.5", 0.09, 0.040, HUE_5PB_DEG),
        sw("hull.deck", "USN 20-B Deck Blue (Ships-2 camouflage paints); Y per DESIGN.md L1790-1799", "5PB 2.6/2.5", 0.05, 0.036, HUE_5PB_DEG),
        sw("hull.black", "USN Dull Black (boot-topping); Y per DESIGN.md L1790-1799", "N 2.0/", 0.03, 0.0, 0.0),
    ]
}

fn sky_tokens() -> Vec<SkyTokens> {
    vec![
        SkyTokens {
            time: TimeOfDay::Dusk,
            zenith: sw("sky.dusk.zenith", "Dusk zenith, deepening blue above a low sun", "5PB 2.9/4", 0.06, 0.06, 262.0),
            horizon: sw("sky.dusk.horizon", "Dusk horizon band, warm under a low sun", "5YR 6.3/5", 0.33, 0.075, 62.0),
            key_light: sw("sky.dusk.key_light", "Low warm sun at dusk, the hero light", "5YR 8.7/4.5", 0.70, 0.12, 55.0),
        },
        SkyTokens {
            time: TimeOfDay::Day,
            zenith: sw("sky.day.zenith", "Day zenith", "5B 5.3/4.5", 0.22, 0.07, 245.0),
            horizon: sw("sky.day.horizon", "Day horizon, pale with aerial haze", "5B 7.7/2", 0.52, 0.03, 235.0),
            key_light: sw("sky.day.key_light", "High sun, near white", "5Y 9.6/1.5", 0.90, 0.02, 85.0),
        },
        SkyTokens {
            time: TimeOfDay::Night,
            zenith: sw("sky.night.zenith", "Night zenith, kept above black", "5PB 0.2/2", 0.0025, 0.03, 265.0),
            horizon: sw("sky.night.horizon", "Night horizon glow", "5PB 0.7/1.5", 0.008, 0.02, 258.0),
            key_light: sw("sky.night.key_light", "Moonlight, cool", "5PB 6.1/1.5", 0.30, 0.02, 250.0),
        },
    ]
}

fn sea_swatches() -> Vec<Swatch> {
    vec![
        sw("sea.deep", "Deep water body colour", "5B 1.4/2.5", 0.018, 0.04, 230.0),
        sw("sea.scatter", "Subsurface scattering on wave crests facing the sun", "5BG 2.9/3.5", 0.06, 0.05, 205.0),
        sw("sea.shallow", "Shoal water near islands", "5BG 3.5/3.5", 0.09, 0.05, 185.0),
        sw("sea.foam", "Foam, wakes and whitecaps", "5B 8.7/0.5", 0.70, 0.01, 230.0),
    ]
}

fn island_bands() -> Vec<(f32, Swatch)> {
    vec![
        (3.0, sw("island.shore", "Wet shingle and sand at the waterline; flat-shaded with fog (DESIGN.md L1917)", "5Y 5.1/2.5", 0.20, 0.035, 80.0)),
        (15.0, sw("island.scrub", "Salt-burnt scrub above the shore; flat-shaded with fog (DESIGN.md L1917)", "5GY 3.3/3", 0.08, 0.045, 125.0)),
        (70.0, sw("island.forest", "Dark conifer and maquis; flat-shaded with fog (DESIGN.md L1917)", "5G 2.6/2.5", 0.05, 0.04, 140.0)),
        (220.0, sw("island.rock", "Weathered bare rock; flat-shaded with fog (DESIGN.md L1917)", "5YR 4.0/1.5", 0.12, 0.02, 60.0)),
        (10_000.0, sw("island.summit", "Pale summit stone, cooled by distance; flat-shaded with fog (DESIGN.md L1917)", "5PB 5.3/0.5", 0.22, 0.008, 250.0)),
    ]
}

/// Per-time base of the presets: light energies, exposure and the base grade.
struct TimeBase {
    sun_energy: f64,
    ambient_energy: f64,
    ambient_floor: f64,
    exposure: f64,
    lift: [f64; 3],
    gamma: [f64; 3],
    gain: [f64; 3],
    saturation: f64,
}

fn time_base(t: TimeOfDay) -> TimeBase {
    match t {
        // Warm lift and warm gain: one hue family, no split toning.
        TimeOfDay::Dusk => TimeBase { sun_energy: 1.8, ambient_energy: 0.32, ambient_floor: 0.12, exposure: 1.1, lift: [0.012, 0.008, 0.006], gamma: [1.0; 3], gain: [1.02, 1.0, 0.97], saturation: 1.0 },
        TimeOfDay::Day => TimeBase { sun_energy: 2.2, ambient_energy: 0.40, ambient_floor: 0.15, exposure: 0.95, lift: [0.004, 0.004, 0.006], gamma: [1.0; 3], gain: [1.0; 3], saturation: 0.97 },
        // Moonlight: cool lift and cool gain, mids raised for readability, scotopic desaturation.
        TimeOfDay::Night => TimeBase { sun_energy: 0.22, ambient_energy: 0.07, ambient_floor: 0.035, exposure: 2.6, lift: [0.006, 0.008, 0.014], gamma: [1.06; 3], gain: [0.97, 0.985, 1.02], saturation: 0.82 },
    }
}

/// Per-weather modifier of the presets.
struct WeatherMod {
    /// Sun energy multiplier (cloud and haze take the direct light).
    sun: f64,
    /// Ambient energy multiplier (scattered light fills the shadows).
    ambient: f64,
    /// How far the zenith greys toward the horizon colour, 0..1.
    overcast: f64,
    /// Key-light chroma multiplier (diffused light whitens).
    sun_chroma: f64,
    /// Horizon and fog colour: Y and chroma multipliers on the time's clear horizon.
    fog_y: f64,
    fog_chroma: f64,
    /// Exposure multiplier: dull weather is partly compensated, never fully.
    exposure: f64,
    saturation: f64,
    gamma: f64,
    /// How far the horizon and fog hue turns from the time's clear horizon toward its zenith, 0..1. Under a
    /// cloud deck the low sun no longer colours the air: dusk overcast and storm read as cool steel grey, not
    /// sepia, and the warmth is left to the (weak) sun itself.
    cool: f64,
}

fn weather_mod(w: WeatherKind) -> WeatherMod {
    match w {
        WeatherKind::Clear => WeatherMod { sun: 1.0, ambient: 1.0, overcast: 0.0, sun_chroma: 1.0, fog_y: 1.0, fog_chroma: 1.0, exposure: 1.0, saturation: 1.0, gamma: 1.0, cool: 0.0 },
        WeatherKind::Haze => WeatherMod { sun: 0.7, ambient: 1.1, overcast: 0.3, sun_chroma: 0.8, fog_y: 1.05, fog_chroma: 0.6, exposure: 1.05, saturation: 0.95, gamma: 1.0, cool: 0.0 },
        WeatherKind::Overcast => WeatherMod { sun: 0.3, ambient: 1.3, overcast: 0.8, sun_chroma: 0.4, fog_y: 0.8, fog_chroma: 0.3, exposure: 1.15, saturation: 0.9, gamma: 1.0, cool: 0.85 },
        WeatherKind::Rain => WeatherMod { sun: 0.2, ambient: 1.15, overcast: 0.9, sun_chroma: 0.35, fog_y: 0.6, fog_chroma: 0.25, exposure: 1.2, saturation: 0.85, gamma: 1.0, cool: 0.9 },
        WeatherKind::Storm => WeatherMod { sun: 0.12, ambient: 0.9, overcast: 0.95, sun_chroma: 0.3, fog_y: 0.4, fog_chroma: 0.2, exposure: 1.25, saturation: 0.8, gamma: 0.97, cool: 1.0 },
        WeatherKind::FogBank => WeatherMod { sun: 0.35, ambient: 1.4, overcast: 1.0, sun_chroma: 0.5, fog_y: 1.0, fog_chroma: 0.15, exposure: 1.05, saturation: 0.85, gamma: 1.0, cool: 0.7 },
    }
}

/// Hue `a` turned toward `b` by `t`, the short way round, degrees.
fn mix_hue(a: f64, b: f64, t: f64) -> f64 {
    let d = (b - a + 540.0).rem_euclid(360.0) - 180.0;
    (a + d * t).rem_euclid(360.0)
}

pub(crate) const WEATHERS: [WeatherKind; 6] = [WeatherKind::Clear, WeatherKind::Haze, WeatherKind::Overcast, WeatherKind::Rain, WeatherKind::Storm, WeatherKind::FogBank];
pub(crate) const TIMES: [TimeOfDay; 3] = [TimeOfDay::Dusk, TimeOfDay::Day, TimeOfDay::Night];

/// The 14 presets of 0.1.0: six weathers at dusk and day, plus night clear and night overcast (L1868).
fn preset_phase(w: WeatherKind, t: TimeOfDay) -> Phase {
    match (t, w) {
        (TimeOfDay::Dusk | TimeOfDay::Day, _) | (TimeOfDay::Night, WeatherKind::Clear | WeatherKind::Overcast) => Phase::R010,
        _ => Phase::R100,
    }
}

/// Share of the ambient colour taken from the zenith (the rest from the horizon band).
const AMBIENT_ZENITH_WEIGHT: f64 = 0.65;

fn build_preset(sky: &SkyTokens, w: WeatherKind) -> Preset {
    let base = time_base(sky.time);
    let m = weather_mod(w);
    let horizon = colour_at_y(sky.horizon.y * m.fog_y, sky.horizon.chroma * m.fog_chroma, mix_hue(sky.horizon.hue_deg, sky.zenith.hue_deg, m.cool)).0;
    let zenith_clear = swatch_colour(&sky.zenith).linear_rgb;
    let (zl, hl) = (linear_to_oklab(zenith_clear), linear_to_oklab(horizon));
    let zl = [0, 1, 2].map(|i| zl[i] + (hl[i] - zl[i]) * m.overcast);
    let zenith = clip01(oklab_to_linear(zl));
    let key = &sky.key_light;
    let sun = colour_at_y(key.y, key.chroma * m.sun_chroma, key.hue_deg).0;
    let sun_max = sun.iter().fold(0.0f64, |a, &b| a.max(b)).max(1e-9);
    let sun_color = sun.map(|c| c / sun_max);
    // Ambient takes the sky's colour by area, not by brightness: each colour at unit luminance, the zenith
    // dome weighted over the horizon band, so shadows under a warm low sun stay cool and readable.
    let unit = |c: [f64; 3]| { let y = luminance(c); if y > 1e-9 { c.map(|v| v / y) } else { [1.0; 3] } };
    let (zu, hu) = (unit(zenith), unit(horizon));
    let mix = [0, 1, 2].map(|i| AMBIENT_ZENITH_WEIGHT * zu[i] + (1.0 - AMBIENT_ZENITH_WEIGHT) * hu[i]);
    let ambient_dir = unit(mix);
    let ambient_energy = (base.ambient_energy * m.ambient).max(base.ambient_floor);
    let f = |v: [f64; 3]| v.map(|c| c as f32);
    Preset {
        weather: w,
        time: sky.time,
        phase: preset_phase(w, sky.time),
        sun_color: f(sun_color),
        sun_energy: (base.sun_energy * m.sun) as f32,
        sky_zenith: f(zenith),
        sky_horizon: f(horizon),
        ambient: f(ambient_dir.map(|c| c * ambient_energy)),
        ambient_floor: base.ambient_floor as f32,
        fog_color: f(horizon),
        exposure: (base.exposure * m.exposure) as f32,
        lift: f(base.lift),
        gamma: f(base.gamma.map(|g| g * m.gamma)),
        gain: f(base.gain),
        saturation: (base.saturation * m.saturation) as f32,
    }
}

fn ramp(id: RampId, bands: u8, stops: &[(f32, [f32; 4], f32)]) -> Ramp { Ramp { id, bands, stops: stops.to_vec() } }

fn ramps() -> Vec<Ramp> {
    vec![
        // Tracers, muzzle stars, hit flashes, flares: white-hot to dull red; the envelope ends at half, so a
        // flash runs 16 to 8 and a tracer's 4..8 emission burns down to half (L1955).
        ramp(RampId::Flash, 4, &[(0.0, [1.0, 0.93, 0.80, 1.0], 16.0), (0.25, [1.0, 0.74, 0.40, 1.0], 14.0), (0.6, [0.96, 0.46, 0.16, 1.0], 11.0), (1.0, [0.60, 0.20, 0.06, 1.0], 8.0)]),
        // Splash columns, foam rings, wakes, flooding cues: sea.foam white greying as it thins.
        ramp(RampId::Foam, 5, &[(0.0, [0.70, 0.75, 0.78, 1.0], 0.0), (0.2, [0.66, 0.71, 0.74, 1.0], 0.0), (0.6, [0.52, 0.57, 0.60, 1.0], 0.0), (1.0, [0.38, 0.43, 0.46, 1.0], 0.0)]),
        // Fireballs and flame prisms: yellow to deep red, emission 4 down to 2 (fire 2 to 4).
        ramp(RampId::Fire, 5, &[(0.0, [1.0, 0.82, 0.42, 1.0], 4.0), (0.2, [1.0, 0.56, 0.16, 1.0], 3.6), (0.55, [0.86, 0.30, 0.07, 1.0], 2.8), (1.0, [0.48, 0.11, 0.03, 1.0], 2.0)]),
        // Hit puffs: white to grey over their life (L1963), faint glow only at the start.
        ramp(RampId::HitPuff, 5, &[(0.0, [0.90, 0.89, 0.86, 1.0], 1.0), (0.2, [0.62, 0.62, 0.61, 1.0], 0.4), (0.6, [0.36, 0.36, 0.36, 1.0], 0.0), (1.0, [0.22, 0.22, 0.22, 1.0], 0.0)]),
        // Fire, explosion and muzzle smoke plus muzzle haze: a mid grey the instance tint darkens for soot
        // (0.3 to 0.5) or keeps light for cordite (0.85 to 1.0); opacity falls as it spreads.
        ramp(RampId::Smoke, 5, &[(0.0, [0.42, 0.42, 0.42, 0.9], 0.0), (0.25, [0.46, 0.46, 0.46, 0.8], 0.0), (0.6, [0.52, 0.52, 0.53, 0.6], 0.0), (1.0, [0.58, 0.58, 0.59, 0.35], 0.0)]),
        // Defence smoke: dense white-grey chemical smoke, opaque, the same on every tier (L1965).
        ramp(RampId::DefenceSmoke, 4, &[(0.0, [0.64, 0.65, 0.66, 1.0], 0.0), (0.25, [0.62, 0.63, 0.64, 1.0], 0.0), (0.6, [0.58, 0.59, 0.60, 1.0], 0.0), (1.0, [0.55, 0.56, 0.57, 1.0], 0.0)]),
        // Debris cubes: hot fragments that cool to dark steel.
        ramp(RampId::Debris, 5, &[(0.0, [0.92, 0.42, 0.12, 1.0], 1.0), (0.1, [0.55, 0.24, 0.10, 1.0], 0.5), (0.4, [0.13, 0.13, 0.14, 1.0], 0.0), (1.0, [0.10, 0.10, 0.11, 1.0], 0.0)]),
        // Flak bursts and chaff: an orange core collapsing into black smoke.
        ramp(RampId::Flak, 5, &[(0.0, [1.0, 0.62, 0.26, 1.0], 1.0), (0.12, [0.10, 0.08, 0.07, 1.0], 0.2), (0.5, [0.05, 0.05, 0.05, 1.0], 0.0), (1.0, [0.08, 0.08, 0.08, 1.0], 0.0)]),
        // Missile trails: white exhaust smoke, alpha-blended, thinning over its 12 s (L1966).
        ramp(RampId::Trail, 6, &[(0.0, [0.78, 0.78, 0.78, 0.85], 0.0), (0.25, [0.72, 0.72, 0.73, 0.7], 0.0), (0.6, [0.63, 0.63, 0.64, 0.45], 0.0), (1.0, [0.55, 0.55, 0.56, 0.2], 0.0)]),
    ]
}

/// The token table (8 USN swatches at the Y values of L1790-1799, antifouling, primer, island bands, all 18
/// presets with their phase, and one ramp per `RampId`). Unit: `xt-art-tokens`.
pub fn world_tokens() -> WorldTokens {
    let sky = sky_tokens();
    let mut presets = Vec::with_capacity(18);
    for s in &sky {
        for w in WEATHERS { presets.push(build_preset(s, w)); }
    }
    WorldTokens {
        swatches: hull_swatches(),
        antifouling: sw("hull.antifouling", "Red-brown antifouling below the boot-topping, Y 0.08 (DESIGN.md L1801); chroma held to the hull limit", "5R 3.3/4", 0.08, 0.06, 35.0),
        primer: sw("hull.primer", "Warm primer on exposed interiors, Y 0.25, C <= 0.06 (DESIGN.md L1941)", "5YR 5.6/3.5", 0.25, 0.05, 75.0),
        island_bands: island_bands(),
        presets,
        ramps: ramps(),
        sea: sea_swatches(),
        sky,
    }
}

// ---------------------------------------------------------------------------------------------------------
// Colour math
// ---------------------------------------------------------------------------------------------------------

/// Linear sRGB (D65) relative luminance.
pub(crate) fn luminance(rgb: [f64; 3]) -> f64 { 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2] }

fn clean(v: f64) -> f64 { if v.is_finite() { v } else { 0.0 } }
fn clip01(rgb: [f64; 3]) -> [f64; 3] { rgb.map(|v| clean(v).clamp(0.0, 1.0)) }

pub(crate) fn linear_to_oklab(rgb: [f64; 3]) -> [f64; 3] {
    let [r, g, b] = rgb.map(clean);
    let l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b;
    let m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b;
    let s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b;
    let (l, m, s) = (libm::cbrt(l), libm::cbrt(m), libm::cbrt(s));
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

pub(crate) fn oklab_to_linear(lab: [f64; 3]) -> [f64; 3] {
    let [ll, a, b] = lab.map(clean);
    let l = ll + 0.3963377774 * a + 0.2158037573 * b;
    let m = ll - 0.1055613458 * a - 0.0638541728 * b;
    let s = ll - 0.0894841775 * a - 1.2914855480 * b;
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    [
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    ]
}

/// Linear sRGB to OKLCH (L 0..1, C, h degrees). Unit: `xt-art-tokens`.
pub fn linear_to_oklch(rgb: [f64; 3]) -> [f64; 3] {
    let [l, a, b] = linear_to_oklab(rgb);
    let c = libm::sqrt(a * a + b * b);
    let h = if c < 1e-9 { 0.0 } else {
        let d = libm::atan2(b, a).to_degrees();
        if d < 0.0 { d + 360.0 } else { d }
    };
    [l, c, h]
}

/// OKLCH to linear sRGB. Unit: `xt-art-tokens`.
pub fn oklch_to_linear(lch: [f64; 3]) -> [f64; 3] {
    let [l, c, h] = lch.map(clean);
    let hr = h.to_radians();
    oklab_to_linear([l, c * libm::cos(hr), c * libm::sin(hr)])
}

/// sRGB transfer function, linear 0..1 to encoded 0..1. Unit: `xt-art-tokens`.
pub fn srgb_encode(v: f64) -> f64 {
    let v = clean(v).clamp(0.0, 1.0);
    if v <= 0.0031308 { 12.92 * v } else { 1.055 * libm::pow(v, 1.0 / 2.4) - 0.055 }
}

/// Inverse of [`srgb_encode`].
pub(crate) fn srgb_decode(v: f64) -> f64 {
    let v = clean(v).clamp(0.0, 1.0);
    if v <= 0.04045 { v / 12.92 } else { libm::pow((v + 0.055) / 1.055, 2.4) }
}

/// 0..1 to a byte, rounding to nearest.
pub(crate) fn to_u8(v: f64) -> u8 { libm::round(clean(v).clamp(0.0, 1.0) * 255.0) as u8 }

/// CIE L* of a relative luminance 0..1.
pub(crate) fn lstar(y: f64) -> f64 {
    let y = clean(y).max(0.0);
    let d = 6.0 / 29.0;
    let f = if y > d * d * d { libm::cbrt(y) } else { y / (3.0 * d * d) + 4.0 / 29.0 };
    116.0 * f - 16.0
}

/// Munsell value of a relative luminance per ASTM D1535 (Y in percent of white), solved by bisection.
fn munsell_value(y: f64) -> f64 {
    let yp = clean(y).clamp(0.0, 1.0) * 100.0;
    let f = |v: f64| v * (1.1914 + v * (-0.22533 + v * (0.23352 + v * (-0.020484 + v * 0.00081939))));
    let (mut lo, mut hi) = (0.0, 10.0);
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if f(mid) < yp { lo = mid } else { hi = mid }
    }
    0.5 * (lo + hi)
}

fn in_gamut(rgb: [f64; 3]) -> bool { rgb.iter().all(|&v| v.is_finite() && (-1e-9..=1.0 + 1e-9).contains(&v)) }

/// OKLab lightness whose colour at (a, b) has luminance `y` (bisection; luminance rises with L).
fn solve_lightness(y: f64, a: f64, b: f64) -> f64 {
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..64 {
        let mid = 0.5 * (lo + hi);
        if luminance(oklab_to_linear([mid, a, b])) < y { lo = mid } else { hi = mid }
    }
    0.5 * (lo + hi)
}

/// Linear RGB with luminance `y` at OKLCH (chroma, hue), and the chroma actually used: chroma drops first
/// (L1803) until the colour fits the sRGB gamut.
pub(crate) fn colour_at_y(y: f64, chroma: f64, hue_deg: f64) -> ([f64; 3], f64) {
    let y = clean(y).clamp(0.0, 1.0);
    let c = clean(chroma).clamp(0.0, 0.4);
    let hr = clean(hue_deg).to_radians();
    let (ca, sa) = (libm::cos(hr), libm::sin(hr));
    let at = |c: f64| { let (a, b) = (c * ca, c * sa); oklab_to_linear([solve_lightness(y, a, b), a, b]) };
    let first = at(c);
    if in_gamut(first) { return (clip01(first), c); }
    let (mut lo, mut hi) = (0.0, c);
    for _ in 0..48 {
        let mid = 0.5 * (lo + hi);
        if in_gamut(at(mid)) { lo = mid } else { hi = mid }
    }
    (clip01(at(lo)), lo)
}

/// Linear RGB, sRGB8, OKLCH and L* of a swatch: solve OKLCH lightness so luminance equals `y` at the authored
/// chroma and hue. Unit: `xt-art-tokens`.
pub fn swatch_colour(s: &Swatch) -> SwatchColour {
    let (rgb, c) = colour_at_y(s.y, s.chroma, s.hue_deg);
    let lch = linear_to_oklch(rgb);
    let hue = if c < 1e-9 { 0.0 } else { clean(s.hue_deg).rem_euclid(360.0) };
    SwatchColour { linear_rgb: rgb, srgb8: rgb.map(|v| to_u8(srgb_encode(v))), oklch: [lch[0], c, hue], lstar: lstar(luminance(rgb)) }
}

// ---------------------------------------------------------------------------------------------------------
// Grades and ramps (shared with gen/art.rs)
// ---------------------------------------------------------------------------------------------------------

/// The preset's LUT grade applied to one display-encoded colour (see the module docs).
pub(crate) fn grade(p: &Preset, rgb: [f64; 3]) -> [f64; 3] {
    let mut v = [0.0; 3];
    for i in 0..3 {
        let x = clean(rgb[i]).clamp(0.0, 1.0);
        let (lift, gain) = (clean(p.lift[i] as f64), clean(p.gain[i] as f64));
        let gamma = clean(p.gamma[i] as f64).max(1e-3);
        let lifted = (gain * (x + lift * (1.0 - x))).max(0.0);
        v[i] = libm::pow(lifted, 1.0 / gamma);
    }
    let luma = 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2];
    let s = clean(p.saturation as f64).max(0.0);
    v.map(|c| clean(luma + s * (c - luma)).clamp(0.0, 1.0))
}

/// Ramp colour (linear RGBA) and emission at normalised age `t`, linear between stops, held at the ends.
pub(crate) fn ramp_eval(r: &Ramp, t: f64) -> ([f64; 4], f64) {
    let conv = |s: &(f32, [f32; 4], f32)| (s.1.map(|c| clean(c as f64)), clean(s.2 as f64));
    let Some(first) = r.stops.first() else { return ([0.0, 0.0, 0.0, 1.0], 0.0) };
    let t = clean(t).clamp(0.0, 1.0);
    if t <= first.0 as f64 { return conv(first); }
    for w in r.stops.windows(2) {
        let (t0, t1) = (w[0].0 as f64, w[1].0 as f64);
        if t <= t1 {
            let f = if t1 - t0 > 1e-12 { (t - t0) / (t1 - t0) } else { 1.0 };
            let (a, ea) = conv(&w[0]);
            let (b, eb) = conv(&w[1]);
            return ([0, 1, 2, 3].map(|i| a[i] + (b[i] - a[i]) * f), ea + (eb - ea) * f);
        }
    }
    conv(r.stops.last().unwrap_or(first))
}

/// Highest emission over a ramp's stops (0 for a ramp that carries opacity instead).
pub(crate) fn ramp_peak_emission(r: &Ramp) -> f64 { r.stops.iter().map(|s| clean(s.2 as f64)).fold(0.0, f64::max) }

/// The envelope a ramp texel's alpha carries: emission over the ramp's peak, or opacity on a ramp that does
/// not emit.
pub(crate) fn ramp_strength(r: &Ramp, rgba: [f64; 4], e: f64) -> f64 {
    let peak = ramp_peak_emission(r);
    if peak > 0.0 { (e / peak).clamp(0.0, 1.0) } else { rgba[3].clamp(0.0, 1.0) }
}

// ---------------------------------------------------------------------------------------------------------
// JSON export
// ---------------------------------------------------------------------------------------------------------

enum J { S(String), N(String), A(Vec<J>), O(Vec<(&'static str, J)>) }

fn num(v: f64, dp: usize) -> J {
    let v = if v.is_finite() { v } else { 0.0 };
    let s = format!("{v:.dp$}");
    let negative_zero = s.starts_with('-') && s[1..].chars().all(|c| c == '0' || c == '.');
    J::N(if negative_zero { s[1..].to_string() } else { s })
}
fn int(v: i64) -> J { J::N(v.to_string()) }
fn st(s: &str) -> J { J::S(s.to_string()) }
fn nums(v: &[f64], dp: usize) -> J { J::A(v.iter().map(|&x| num(x, dp)).collect()) }

fn escape(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_json(j: &J, depth: usize, out: &mut String) {
    let pad = |d: usize, out: &mut String| for _ in 0..d { out.push(' ') };
    match j {
        J::S(s) => escape(s, out),
        J::N(n) => out.push_str(n),
        J::A(items) if items.is_empty() => out.push_str("[]"),
        J::A(items) if items.iter().all(|i| matches!(i, J::S(_) | J::N(_))) => {
            out.push('[');
            for (k, i) in items.iter().enumerate() {
                if k > 0 { out.push_str(", "); }
                write_json(i, depth, out);
            }
            out.push(']');
        }
        J::A(items) => {
            out.push_str("[\n");
            for (k, i) in items.iter().enumerate() {
                pad(depth + 1, out);
                write_json(i, depth + 1, out);
                out.push_str(if k + 1 < items.len() { ",\n" } else { "\n" });
            }
            pad(depth, out);
            out.push(']');
        }
        J::O(fields) if fields.is_empty() => out.push_str("{}"),
        J::O(fields) => {
            out.push_str("{\n");
            for (k, (key, v)) in fields.iter().enumerate() {
                pad(depth + 1, out);
                escape(key, out);
                out.push_str(": ");
                write_json(v, depth + 1, out);
                out.push_str(if k + 1 < fields.len() { ",\n" } else { "\n" });
            }
            pad(depth, out);
            out.push('}');
        }
    }
}

fn hex(rgb: [u8; 3]) -> String { format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]) }

/// Derived colour forms of a linear colour (no authored Y, chroma or hue).
fn derived_fields(rgb: [f64; 3]) -> Vec<(&'static str, J)> {
    let rgb = clip01(rgb);
    let lch = linear_to_oklch(rgb);
    let y = luminance(rgb);
    let s8 = rgb.map(|v| to_u8(srgb_encode(v)));
    vec![
        ("y", num(y, 4)),
        ("oklch", J::A(vec![num(lch[0], 4), num(lch[1], 4), num(lch[2], 1)])),
        ("linear_rgb", nums(&rgb, 4)),
        ("srgb8", J::A(s8.iter().map(|&v| int(v as i64)).collect())),
        ("srgb", J::S(hex(s8))),
        ("l_star", num(lstar(y), 2)),
    ]
}

/// Authored and derived fields of a swatch, after `lead` (token, paint and the like).
fn swatch_fields(s: &Swatch, lead: Vec<(&'static str, J)>) -> J {
    let c = swatch_colour(s);
    let mut f = lead;
    f.push(("citation", st(s.citation)));
    f.push(("chroma", num(s.chroma, 4)));
    f.push(("hue_deg", num(s.hue_deg, 1)));
    f.push(("y", num(s.y, 4)));
    f.push(("oklch", J::A(vec![num(c.oklch[0], 4), num(c.oklch[1], 4), num(c.oklch[2], 1)])));
    f.push(("linear_rgb", nums(&c.linear_rgb, 4)));
    f.push(("srgb8", J::A(c.srgb8.iter().map(|&v| int(v as i64)).collect())));
    f.push(("srgb", J::S(hex(c.srgb8))));
    f.push(("l_star", num(c.lstar, 2)));
    f.push(("munsell", st(s.munsell)));
    if c.oklch[1] + 1e-6 < s.chroma { f.push(("chroma_clamped_from", num(s.chroma, 4))); }
    J::O(f)
}

pub(crate) fn weather_name(w: WeatherKind) -> &'static str {
    match w { WeatherKind::Clear => "clear", WeatherKind::Haze => "haze", WeatherKind::Overcast => "overcast", WeatherKind::Rain => "rain", WeatherKind::Storm => "storm", WeatherKind::FogBank => "fog_bank" }
}
pub(crate) fn time_name(t: TimeOfDay) -> &'static str { match t { TimeOfDay::Night => "night", TimeOfDay::Dusk => "dusk", TimeOfDay::Day => "day" } }
fn phase_name(p: Phase) -> &'static str {
    match p { Phase::Slice => "slice", Phase::R010 => "r010", Phase::Act2 => "act2", Phase::Act3 => "act3", Phase::Act4 => "act4", Phase::R100 => "r100", Phase::L4 => "l4" }
}
fn f3(v: [f32; 3]) -> [f64; 3] { v.map(|x| x as f64) }

fn preset_json(p: &Preset) -> J {
    let v = p.weather.visibility_m();
    J::O(vec![
        ("weather", st(weather_name(p.weather))),
        ("time", st(time_name(p.time))),
        ("phase", st(phase_name(p.phase))),
        ("lut", J::S(hull_mesh::gen_files::lut_path(p.weather, p.time, false))),
        ("lut_high", J::S(hull_mesh::gen_files::lut_path(p.weather, p.time, true))),
        ("visibility_m", num(v, 0)),
        ("fog_density", num(3.912 / v, 7)),
        ("sun_color", nums(&f3(p.sun_color), 4)),
        ("sun_energy", num(p.sun_energy as f64, 4)),
        ("sky_zenith", nums(&f3(p.sky_zenith), 4)),
        ("sky_horizon", nums(&f3(p.sky_horizon), 4)),
        ("ambient", nums(&f3(p.ambient), 4)),
        ("ambient_energy", num(luminance(f3(p.ambient)), 4)),
        ("ambient_floor", num(p.ambient_floor as f64, 4)),
        ("fog_color", nums(&f3(p.fog_color), 4)),
        ("exposure", num(p.exposure as f64, 4)),
        ("lift", nums(&f3(p.lift), 4)),
        ("gamma", nums(&f3(p.gamma), 4)),
        ("gain", nums(&f3(p.gain), 4)),
        ("saturation", num(p.saturation as f64, 4)),
    ])
}

fn ramp_json(r: &Ramp) -> J {
    let stops = r.stops.iter().map(|s| J::O(vec![("t", num(s.0 as f64, 3)), ("linear_rgba", nums(&s.1.map(|c| c as f64), 3)), ("emission", num(s.2 as f64, 2))])).collect();
    let peak = ramp_peak_emission(r);
    J::O(vec![
        ("id", st(r.id.name())),
        ("path", J::S(hull_mesh::gen_files::ramp_path(r.id.name()))),
        ("bands", int(r.bands as i64)),
        ("emission_peak", num(peak, 2)),
        ("alpha", st(if peak > 0.0 { "emission / emission_peak" } else { "opacity" })),
        ("stops", J::A(stops)),
    ])
}

/// Deterministic JSON text of the tokens (fixed key order, fixed float formatting, trailing newline).
/// Unit: `xt-art-tokens`.
pub fn to_json(t: &WorldTokens) -> String {
    let about = "World colour tokens. DESIGN.md L1788-1803 (swatches), L1801 (boot-topping, antifouling), L1941 (primer), L1917 (island bands), L1857-1872 (presets: sun, sky, ambient floor, fog, exposure), L1846 and L1824 (LUT grades, no teal-orange), L1953 (VFX ramps). The only place a world colour may be written. Each colour is authored as linear luminance Y plus a small OKLCH chroma and hue; OKLCH lightness is solved so luminance equals Y (chroma drops first if the sRGB gamut clips), then linear RGB, sRGB and CIE L* are derived. Munsell notation is authored and checked against Y by ASTM D1535. Written by `cargo xtask tokens` from crates/xtask/src/art/tokens.rs (unit xt-art-tokens); never edit by hand.";
    let hull = t.swatches.iter().map(|s| {
        let row = HULL_TABLE.iter().find(|r| r.0 == s.token);
        let mut lead = vec![("token", st(s.token))];
        if let Some(r) = row {
            lead.push(("paint", st(r.1)));
            lead.push(("table_srgb_grey", int(r.3 as i64)));
            lead.push(("table_l_star", num(r.4, 0)));
        }
        swatch_fields(s, lead)
    }).collect();
    let islands = t.island_bands.iter().map(|(top, s)| swatch_fields(s, vec![("token", st(s.token)), ("top_m", J::N(format!("{}", if top.is_finite() { *top } else { 0.0 })))])).collect();
    let sea = t.sea.iter().map(|s| swatch_fields(s, vec![("token", st(s.token))])).collect();
    let sky = t.sky.iter().map(|s| J::O(vec![
        ("time", st(time_name(s.time))),
        ("zenith", swatch_fields(&s.zenith, vec![("token", st(s.zenith.token))])),
        ("horizon", swatch_fields(&s.horizon, vec![("token", st(s.horizon.token))])),
        ("key_light", swatch_fields(&s.key_light, vec![("token", st(s.key_light.token))])),
    ])).collect();
    let fog = t.presets.iter().map(|p| {
        let mut f = vec![("weather", st(weather_name(p.weather))), ("time", st(time_name(p.time)))];
        f.extend(derived_fields(f3(p.fog_color)));
        J::O(f)
    }).collect();
    let root = J::O(vec![
        ("_about", st(about)),
        ("sources", J::O(vec![
            ("paint", st("https://en.wikipedia.org/wiki/World_War_II_ship_camouflage_measures_of_the_United_States_Navy")),
            ("oklab", st("https://bottosson.github.io/posts/oklab/")),
            ("munsell_value", st("ASTM D1535")),
        ])),
        ("limits", J::O(vec![
            ("hull", J::O(vec![("max_y", num(HULL_MAX_Y, 2)), ("max_chroma", num(HULL_MAX_CHROMA, 2))])),
            ("marking", J::O(vec![("max_y", num(MARKING_MAX_Y, 2)), ("max_chroma", num(MARKING_MAX_CHROMA, 2))])),
            ("hue", st("5PB or neutral")),
            ("deck_steps_darker", int(3)),
        ])),
        ("hull", J::A(hull)),
        ("antifouling", swatch_fields(&t.antifouling, vec![("token", st(t.antifouling.token))])),
        ("primer", swatch_fields(&t.primer, vec![("token", st(t.primer.token))])),
        ("island_bands", J::A(islands)),
        ("sea", J::A(sea)),
        ("sky", J::A(sky)),
        ("fog", J::A(fog)),
        ("presets", J::A(t.presets.iter().map(preset_json).collect())),
        ("ramps", J::A(t.ramps.iter().map(ramp_json).collect())),
    ]);
    let mut out = String::new();
    write_json(&root, 0, &mut out);
    out.push('\n');
    out
}

// ---------------------------------------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------------------------------------

fn hue_distance(a: f64, b: f64) -> f64 { let d = (a - b).rem_euclid(360.0); d.min(360.0 - d) }

/// The authored Munsell value (the number between the space and the slash).
fn munsell_authored_value(m: &str) -> Option<f64> {
    let (_, rest) = m.split_once(' ')?;
    rest.split('/').next()?.trim().parse::<f64>().ok()
}

fn check_swatch(s: &Swatch, max_y: f64, max_c: f64, errs: &mut Vec<String>) {
    if !(s.y.is_finite() && s.chroma.is_finite() && s.hue_deg.is_finite()) { errs.push(format!("{}: non-finite value", s.token)); return; }
    if !(0.0..=max_y + 1e-12).contains(&s.y) { errs.push(format!("{}: Y {} outside 0..={max_y}", s.token, s.y)); }
    if !(0.0..=max_c + 1e-12).contains(&s.chroma) { errs.push(format!("{}: chroma {} outside 0..={max_c}", s.token, s.chroma)); }
    match munsell_authored_value(s.munsell) {
        Some(v) if (v - munsell_value(s.y)).abs() <= 0.11 => {}
        Some(v) => errs.push(format!("{}: Munsell value {v} does not match Y {} (ASTM D1535 gives {:.2})", s.token, s.y, munsell_value(s.y))),
        None => errs.push(format!("{}: Munsell notation {:?} has no value", s.token, s.munsell)),
    }
    let c = swatch_colour(s);
    if (luminance(c.linear_rgb) - s.y).abs() > 1e-4 { errs.push(format!("{}: solved colour misses Y {} (gets {:.5})", s.token, s.y, luminance(c.linear_rgb))); }
}

/// Opposing shadow and highlight casts (split toning, the teal-orange tell of L1824).
fn split_tone(p: &Preset) -> Option<String> {
    let cast = |x: f64| {
        let g = grade(p, [x; 3]).map(srgb_decode);
        let lab = linear_to_oklab(g);
        (lab[1], lab[2])
    };
    let (sh, hi) = (cast(0.12), cast(0.85));
    let (cs, ch) = (libm::sqrt(sh.0 * sh.0 + sh.1 * sh.1), libm::sqrt(hi.0 * hi.0 + hi.1 * hi.1));
    if cs < 0.004 || ch < 0.004 { return None; }
    let cos = (sh.0 * hi.0 + sh.1 * hi.1) / (cs * ch);
    (cos < 0.0).then(|| format!("shadow cast (a {:.4}, b {:.4}) opposes highlight cast (a {:.4}, b {:.4})", sh.0, sh.1, hi.0, hi.1))
}

fn check_preset(p: &Preset, hull: &[Swatch], errs: &mut Vec<String>) {
    let name = format!("preset {}/{}", time_name(p.time), weather_name(p.weather));
    let colours = [p.sun_color, p.sky_zenith, p.sky_horizon, p.ambient, p.fog_color, p.lift, p.gamma, p.gain];
    let scalars = [p.sun_energy, p.ambient_floor, p.exposure, p.saturation];
    if colours.iter().flatten().chain(scalars.iter()).any(|v| !v.is_finite() || *v < 0.0) { errs.push(format!("{name}: non-finite or negative value")); return; }
    if p.sun_color.iter().any(|&c| c > 1.0 + 1e-6) || p.sun_color.iter().all(|&c| c < 0.999) { errs.push(format!("{name}: sun colour must peak at 1 (energy carries the intensity)")); }
    if p.exposure <= 0.0 { errs.push(format!("{name}: exposure must be positive")); }
    if p.ambient_floor <= 0.0 { errs.push(format!("{name}: ambient floor must be positive (no pure-black shadows, L1821)")); }
    if (luminance(f3(p.ambient)) as f32) < p.ambient_floor - 1e-5 { errs.push(format!("{name}: ambient {:.4} under its floor {}", luminance(f3(p.ambient)), p.ambient_floor)); }
    if p.lift.iter().any(|&l| l > 0.05) { errs.push(format!("{name}: lift over 0.05")); }
    if p.gamma.iter().any(|&g| !(0.8..=1.25).contains(&g)) { errs.push(format!("{name}: gamma outside 0.8..=1.25")); }
    if p.gain.iter().any(|&g| !(0.85..=1.1).contains(&g)) { errs.push(format!("{name}: gain outside 0.85..=1.1")); }
    if !(0.6..=1.05).contains(&p.saturation) { errs.push(format!("{name}: saturation {} outside 0.6..=1.05 (no saturated grades)", p.saturation)); }
    if let Some(why) = split_tone(p) { errs.push(format!("{name}: split-toned grade (no teal-orange, L1824): {why}")); }
    for s in hull {
        let disp = swatch_colour(s).linear_rgb.map(srgb_encode);
        let graded = linear_to_oklch(grade(p, disp).map(srgb_decode));
        if graded[1] > HULL_MAX_CHROMA + 1e-9 { errs.push(format!("{name}: grade pushes {} to chroma {:.4}, over the token limit {HULL_MAX_CHROMA}", s.token, graded[1])); }
    }
}

fn ramp_limit(id: RampId) -> f64 {
    match id { RampId::Flash => emission::FLASH.1 as f64, RampId::Fire => emission::FIRE.1 as f64, _ => emission::OTHER_MAX as f64 }
}

fn check_ramp(r: &Ramp, errs: &mut Vec<String>) {
    let name = format!("ramp {}", r.id.name());
    if !(4..=6).contains(&r.bands) { errs.push(format!("{name}: {} bands, must be 4 to 6 (L1953)", r.bands)); }
    if r.stops.len() < 2 { errs.push(format!("{name}: needs at least 2 stops")); return; }
    if r.stops.iter().any(|s| !s.0.is_finite() || !s.2.is_finite() || s.1.iter().any(|c| !c.is_finite())) { errs.push(format!("{name}: non-finite stop")); return; }
    if r.stops[0].0 != 0.0 || r.stops[r.stops.len() - 1].0 != 1.0 { errs.push(format!("{name}: stops must run from t 0 to t 1")); }
    if r.stops.windows(2).any(|w| w[1].0 <= w[0].0) { errs.push(format!("{name}: stop times must rise strictly")); }
    if r.stops.iter().any(|s| s.1.iter().any(|&c| !(0.0..=1.0).contains(&c))) { errs.push(format!("{name}: colour or alpha outside 0..1")); }
    let peak = ramp_peak_emission(r);
    if r.stops.iter().any(|s| s.2 < 0.0) { errs.push(format!("{name}: negative emission")); }
    if peak > ramp_limit(r.id) + 1e-6 { errs.push(format!("{name}: emission {peak} over the ladder's {} (L1955)", ramp_limit(r.id))); }
    if r.id == RampId::Fire && r.stops.iter().any(|s| (s.2 as f64) < emission::FIRE.0 as f64 - 1e-6) { errs.push(format!("{name}: fire emission under the ladder's {}", emission::FIRE.0)); }
    if peak > 0.0 && r.stops.iter().any(|s| s.1[3] < 1.0) { errs.push(format!("{name}: an emitting ramp must be opaque (alpha carries emission)")); }
    // The envelope's first maximum must fall in the first 25% of life.
    let strength = |s: &(f32, [f32; 4], f32)| ramp_strength(r, s.1.map(|c| c as f64), s.2 as f64);
    let best = r.stops.iter().map(strength).fold(0.0, f64::max);
    if let Some(s) = r.stops.iter().find(|s| strength(s) >= best - 1e-9) {
        if s.0 > 0.25 { errs.push(format!("{name}: envelope peaks at t {} (must be in the first 25%, L1953)", s.0)); }
    }
}

/// Check the table against the spec's constraints: hull swatch chroma <= 0.06 with hue 5PB or neutral, Y
/// matching L1790-1799 within 0.005, ramp bands 4..=6 with the peak in the first 25%, 14 presets in 0.1.0.
/// Unit: `xt-art-tokens`.
pub fn validate(t: &WorldTokens) -> Result<(), String> {
    let mut errs = Vec::new();
    // Hull swatches: order, Y, hue family, limits, the spec table's L* and neutral sRGB.
    if t.swatches.len() != HULL_TABLE.len() { errs.push(format!("{} hull swatches, expected {}", t.swatches.len(), HULL_TABLE.len())); }
    for (s, row) in t.swatches.iter().zip(HULL_TABLE.iter()) {
        if s.token != row.0 { errs.push(format!("hull swatch {} where {} belongs (L130 order)", s.token, row.0)); continue; }
        check_swatch(s, HULL_MAX_Y, HULL_MAX_CHROMA, &mut errs);
        if (s.y - row.2).abs() > 0.005 { errs.push(format!("{}: Y {} differs from the spec's {} (L1790-1799)", s.token, s.y, row.2)); }
        let neutral = s.chroma < 1e-9;
        if neutral != s.munsell.starts_with("N ") { errs.push(format!("{}: Munsell {:?} disagrees with chroma {}", s.token, s.munsell, s.chroma)); }
        if !neutral && (!s.munsell.starts_with("5PB ") || hue_distance(s.hue_deg, HUE_5PB_DEG) > HUE_5PB_TOLERANCE_DEG) { errs.push(format!("{}: hue must be 5PB (about {HUE_5PB_DEG} degrees) or neutral", s.token)); }
        let c = swatch_colour(s);
        if (c.lstar - row.4).abs() > 1.0 { errs.push(format!("{}: L* {:.2} differs from the spec's {}", s.token, c.lstar, row.4)); }
        if (to_u8(srgb_encode(s.y)) as i32 - row.3 as i32).abs() > 1 { errs.push(format!("{}: neutral sRGB of Y {} differs from the spec's {}", s.token, s.y, row.3)); }
    }
    // Antifouling and primer (L1801, L1941).
    check_swatch(&t.antifouling, HULL_MAX_Y, HULL_MAX_CHROMA, &mut errs);
    if (t.antifouling.y - 0.08).abs() > 0.005 { errs.push(format!("{}: Y {} should be 0.08 (L1801)", t.antifouling.token, t.antifouling.y)); }
    if t.antifouling.chroma <= 0.0 || hue_distance(t.antifouling.hue_deg, 37.5) > 22.5 { errs.push(format!("{}: must be red-brown (hue 15 to 60 degrees)", t.antifouling.token)); }
    check_swatch(&t.primer, HULL_MAX_Y, HULL_MAX_CHROMA, &mut errs);
    if (t.primer.y - 0.25).abs() > 0.005 { errs.push(format!("{}: Y {} should be 0.25 (L1941)", t.primer.token, t.primer.y)); }
    if t.primer.chroma <= 0.0 || hue_distance(t.primer.hue_deg, 60.0) > 40.0 { errs.push(format!("{}: must be warm (hue 20 to 100 degrees)", t.primer.token)); }
    // Island bands: rising tops, covering every height, inside the hull limits (same fog, no saturated blocks).
    if t.island_bands.is_empty() { errs.push("no island bands".into()); }
    if t.island_bands.windows(2).any(|w| !(w[1].0 > w[0].0)) { errs.push("island band tops must rise strictly".into()); }
    if t.island_bands.last().is_some_and(|b| !(b.0 >= 1000.0)) { errs.push("the top island band must reach at least 1000 m".into()); }
    for (_, s) in &t.island_bands { check_swatch(s, HULL_MAX_Y, HULL_MAX_CHROMA, &mut errs); }
    for s in &t.sea { check_swatch(s, 1.0, MARKING_MAX_CHROMA, &mut errs); }
    for s in &t.sky {
        for c in [&s.zenith, &s.horizon, &s.key_light] { check_swatch(c, 1.0, 0.2, &mut errs); }
    }
    // Presets: every weather x time once; 14 in 0.1.0, the 4 remaining nights in 1.0.
    for w in WEATHERS {
        for tm in TIMES {
            let n = t.presets.iter().filter(|p| p.weather == w && p.time == tm).count();
            if n != 1 { errs.push(format!("preset {}/{}: {n} entries, expected 1", time_name(tm), weather_name(w))); }
        }
    }
    let r010 = t.presets.iter().filter(|p| p.phase <= Phase::R010).count();
    if r010 != 14 { errs.push(format!("{r010} presets ship in 0.1.0, the spec says 14 (L1868)")); }
    for p in &t.presets {
        if p.phase != preset_phase(p.weather, p.time) { errs.push(format!("preset {}/{}: phase {:?}, the spec gives {:?} (L1868)", time_name(p.time), weather_name(p.weather), p.phase, preset_phase(p.weather, p.time))); }
        check_preset(p, &t.swatches, &mut errs);
    }
    // Ramps: one per RampId.
    for id in RampId::ALL {
        let n = t.ramps.iter().filter(|r| r.id == id).count();
        if n != 1 { errs.push(format!("ramp {}: {n} entries, expected 1", id.name())); }
    }
    for r in &t.ramps { check_ramp(r, &mut errs); }
    if errs.is_empty() { Ok(()) } else { Err(errs.join("\n")) }
}

/// `cargo xtask tokens [--check]`: validate, then write `game/art/tokens/world_tokens.json`, or with `--check`
/// fail if the committed file differs. Implemented.
pub fn run(root: &Path, check: bool) -> Result<(), String> {
    let t = world_tokens();
    validate(&t)?;
    let text = to_json(&t);
    let dest = crate::repo::game_dir(root).join(super::WORLD_TOKENS_JSON);
    let on_disk = std::fs::read_to_string(&dest).ok();
    if check {
        if on_disk.as_deref() != Some(text.as_str()) { return Err(format!("{} is stale; run `cargo xtask tokens`", dest.display())); }
        return Ok(());
    }
    if let Some(p) = dest.parent() { std::fs::create_dir_all(p).map_err(|e| e.to_string())?; }
    if on_disk.as_deref() != Some(text.as_str()) { std::fs::write(&dest, text).map_err(|e| e.to_string())?; }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_validates() { validate(&world_tokens()).unwrap(); }

    #[test]
    fn committed_json_matches_the_table() {
        // The `art.tokens-sync` lint in test form: run `cargo xtask tokens` when this fails.
        run(&crate::repo::root(), true).unwrap();
    }

    #[test]
    fn paint_table_reads_the_same_colours() {
        // content's default paint table reads the committed JSON; it must land on swatch_colour's values.
        let t = world_tokens();
        let paint = content::blueprint::io::default_paint_table();
        for (s, p) in t.swatches.iter().chain(std::iter::once(&t.antifouling)).zip(paint.iter()) {
            let c = swatch_colour(s);
            for i in 0..3 { assert!((p.rgb[i] as f64 - c.linear_rgb[i]).abs() < 1e-4, "{}: {:?} vs {:?}", s.token, p.rgb, c.linear_rgb); }
        }
    }

    #[test]
    fn oklch_round_trip() {
        for rgb in [[0.5, 0.2, 0.1], [0.03, 0.03, 0.03], [0.9, 0.95, 1.0], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.1, 0.6, 0.3]] {
            let back = oklch_to_linear(linear_to_oklch(rgb));
            // Ottosson's published 10-digit matrices invert each other to about 3e-7.
            for i in 0..3 { assert!((back[i] - rgb[i]).abs() < 1e-6, "{rgb:?} -> {back:?}"); }
        }
        // White is L 1, C 0 (Ottosson's matrices are normalised to D65 white).
        let w = linear_to_oklch([1.0; 3]);
        assert!((w[0] - 1.0).abs() < 1e-6 && w[1] < 1e-6);
    }

    #[test]
    fn srgb_transfer_matches_reference_points() {
        assert_eq!(srgb_encode(0.0), 0.0);
        assert!((srgb_encode(1.0) - 1.0).abs() < 1e-12);
        assert_eq!(to_u8(srgb_encode(0.18)), 118);
        assert_eq!(to_u8(srgb_encode(0.55)), 196);
        assert!((srgb_decode(srgb_encode(0.37)) - 0.37).abs() < 1e-12);
        assert_eq!(srgb_encode(f64::NAN), 0.0);
    }

    #[test]
    fn swatches_hit_their_luminance_and_the_spec_table() {
        let t = world_tokens();
        for (s, row) in t.swatches.iter().zip(HULL_TABLE.iter()) {
            let c = swatch_colour(s);
            assert!((luminance(c.linear_rgb) - row.2).abs() < 1e-6, "{}", s.token);
            assert!((c.lstar - row.4).abs() < 0.6, "{} L* {}", s.token, c.lstar);
            assert!(c.oklch[1] <= HULL_MAX_CHROMA);
        }
        // The neutral black is exactly grey.
        let black = swatch_colour(&t.swatches[7]);
        assert!(black.linear_rgb.iter().all(|c| (c - 0.03).abs() < 1e-9), "{:?}", black.linear_rgb);
        assert_eq!(black.srgb8, [48, 48, 48]);
    }

    #[test]
    fn out_of_gamut_chroma_drops_first() {
        // A bright saturated orange cannot exist at Y 0.7 with C 0.3; chroma gives way, Y holds.
        let s = sw("t", "", "N 8.7/", 0.7, 0.3, 55.0);
        let c = swatch_colour(&s);
        assert!(c.oklch[1] < 0.3 && c.oklch[1] > 0.0);
        assert!((luminance(c.linear_rgb) - 0.7).abs() < 1e-4);
        assert!(c.linear_rgb.iter().all(|v| (0.0..=1.0).contains(v)));
    }

    #[test]
    fn munsell_value_matches_astm_points() {
        // The bisection inverts the ASTM D1535 polynomial (V 5 is Y 19.27%, V 2 is Y 3.05% of the MgO white).
        let y = |v: f64| v * (1.1914 + v * (-0.22533 + v * (0.23352 + v * (-0.020484 + v * 0.00081939)))) / 100.0;
        for v in [1.0, 2.0, 5.0, 7.8, 9.5] { assert!((munsell_value(y(v)) - v).abs() < 1e-9, "{v}"); }
        assert!((y(5.0) - 0.1927).abs() < 1e-4);
    }

    #[test]
    fn json_is_stable() {
        let a = to_json(&world_tokens());
        assert_eq!(a, to_json(&world_tokens()));
        assert!(a.ends_with("}\n") && !a.contains("-0.0000") && !a.contains("NaN") && !a.contains('\r'));
        assert!(a.contains("\"hull\": [") && a.contains("\"antifouling\": {") && a.contains("\"presets\": ["));
    }

    #[test]
    fn presets_cover_the_spec() {
        let t = world_tokens();
        assert_eq!(t.presets.len(), 18);
        assert_eq!(t.presets.iter().filter(|p| p.phase == Phase::R010).count(), 14);
        let r100: Vec<_> = t.presets.iter().filter(|p| p.phase == Phase::R100).map(|p| p.time).collect();
        assert!(r100.iter().all(|tm| *tm == TimeOfDay::Night) && r100.len() == 4);
        // Fog matches the horizon, and the dusk sun is warm (the hero light).
        let dusk = t.presets.iter().find(|p| p.time == TimeOfDay::Dusk && p.weather == WeatherKind::Clear).unwrap();
        assert_eq!(dusk.fog_color, dusk.sky_horizon);
        assert!(dusk.sun_color[0] > dusk.sun_color[2]);
        // Weather dims the sun.
        let storm = t.presets.iter().find(|p| p.time == TimeOfDay::Dusk && p.weather == WeatherKind::Storm).unwrap();
        assert!(storm.sun_energy < dusk.sun_energy);
        // Under a cloud deck the dusk air is cool grey, not sepia: the horizon's blue is not under its red.
        for w in [WeatherKind::Overcast, WeatherKind::Rain, WeatherKind::Storm] {
            let p = t.presets.iter().find(|p| p.time == TimeOfDay::Dusk && p.weather == w).unwrap();
            assert!(p.sky_horizon[2] >= p.sky_horizon[0] && p.fog_color[2] >= p.fog_color[0], "{w:?}: {:?}", p.sky_horizon);
        }
        assert!((mix_hue(350.0, 10.0, 0.5) - 0.0).abs() < 1e-9 && (mix_hue(62.0, 262.0, 1.0) - 262.0).abs() < 1e-9);
    }

    #[test]
    fn validate_catches_bad_tables() {
        let mut t = world_tokens();
        t.swatches[2].chroma = 0.09;
        t.swatches[3].hue_deg = 40.0;
        t.ramps[0].bands = 7;
        t.presets[0].phase = Phase::R100;
        let e = validate(&t).unwrap_err();
        assert!(e.contains("hull.haze") && e.contains("hull.ocean") && e.contains("bands") && e.contains("13 presets"), "{e}");
    }

    #[test]
    fn teal_orange_grade_is_rejected() {
        let mut p = world_tokens().presets[0].clone();
        assert!(split_tone(&p).is_none());
        p.lift = [0.0, 0.02, 0.03];
        p.gain = [1.06, 1.0, 0.92];
        assert!(split_tone(&p).is_some());
    }

    #[test]
    fn late_peaking_ramp_is_rejected() {
        let mut t = world_tokens();
        let r = t.ramps.iter_mut().find(|r| r.id == RampId::Fire).unwrap();
        r.stops[0].2 = 2.0;
        r.stops[1].2 = 2.0;
        r.stops[2].2 = 4.0;
        let e = validate(&t).unwrap_err();
        assert!(e.contains("ramp fire") && e.contains("first 25%"), "{e}");
    }
}
