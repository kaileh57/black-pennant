//! Contract-owned types of the world tokens: the art-direction values every generated texture derives from.
//! DESIGN.md L1788 (swatches in `res://art/tokens/world_tokens.json` with linear Y, OKLCH, sRGB, Munsell and
//! paint citations; hue 5PB or neutral; OKLCH C <= 0.06; converted by a Rust colour library and committed),
//! L1790-1799 (the eight USN swatches), L1801 (boot-topping hull.black; red-brown antifouling Y 0.08),
//! L1941 (primer Y 0.25, warm, C <= 0.06), L1917 (island token colour bands), L1857-1872 (weather presets: sun
//! colour and energy, sky, ambient above a floor, fog colour, exposure), L1846 (one LUT per preset), L1953 (ramps).
//!
//! The Rust table in [`tokens`] is the source of truth. `cargo xtask tokens` exports it to the committed JSON,
//! which the Godot side reads (environment presets, island bands); generators read the Rust table directly.
//! The UI palette is separate (`res://ui/theme/tokens.json`, owned by the UI theme work).
pub mod tokens;

use content::enums::TimeOfDay;
use content::weather::WeatherKind;
use hull_mesh::vfx::RampId;

/// Committed export of the world tokens, relative to `game/`.
pub const WORLD_TOKENS_JSON: &str = "art/tokens/world_tokens.json";

/// One paint swatch. `y` is the authored value (linear luminance); the other colour forms derive from it plus a
/// small authored OKLCH hue and chroma.
#[derive(Clone, Debug, PartialEq)]
pub struct Swatch {
    /// Token name, e.g. `hull.haze`.
    pub token: &'static str,
    /// Paint citation, e.g. `USN 5-H Haze Gray (Ships-2, 1941)`.
    pub citation: &'static str,
    pub munsell: &'static str,
    /// Linear luminance Y.
    pub y: f64,
    /// Authored OKLCH chroma (<= 0.06 for hull swatches) and hue in degrees (5PB is about 265; neutral uses 0
    /// chroma).
    pub chroma: f64,
    pub hue_deg: f64,
}

/// Colour forms of a swatch for the JSON export.
#[derive(Clone, Debug, PartialEq)]
pub struct SwatchColour { pub linear_rgb: [f64; 3], pub srgb8: [u8; 3], pub oklch: [f64; 3], pub lstar: f64 }

/// Lighting and grade of one weather x time-of-day preset (L1857-1872, L1846). Colours linear RGB.
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub weather: WeatherKind,
    pub time: TimeOfDay,
    /// Shipped in 0.1.0 (14 presets: 6 weathers at dusk and day, plus night clear and night overcast) or 1.0.
    pub phase: content::enums::Phase,
    pub sun_color: [f32; 3],
    pub sun_energy: f32,
    pub sky_zenith: [f32; 3],
    pub sky_horizon: [f32; 3],
    pub ambient: [f32; 3],
    /// Ambient never drops under this energy (no flat or pure-black shadows, L1821).
    pub ambient_floor: f32,
    pub fog_color: [f32; 3],
    /// Fixed exposure (auto exposure is off, L1847).
    pub exposure: f32,
    /// LUT grade applied in AgX output space: lift, gamma, gain per channel and a saturation multiplier.
    pub lift: [f32; 3],
    pub gamma: [f32; 3],
    pub gain: [f32; 3],
    pub saturation: f32,
}

/// One VFX ramp: 4..=6 posterised bands; stops are (t 0..1, linear RGBA, emission) with the peak inside the
/// first 25% of life (L1953-1955).
#[derive(Clone, Debug, PartialEq)]
pub struct Ramp { pub id: RampId, pub bands: u8, pub stops: Vec<(f32, [f32; 4], f32)> }

/// The whole token set.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldTokens {
    /// hull.pale, hull.light, hull.haze, hull.ocean, hull.sea, hull.navy, hull.deck, hull.black, in that order
    /// (the default paint table order, L130).
    pub swatches: Vec<Swatch>,
    /// Red-brown antifouling, Y 0.08 (L1801).
    pub antifouling: Swatch,
    /// Primer for exposed interiors, Y 0.25 (L1941).
    pub primer: Swatch,
    /// Island height bands, lowest first: (top of band in metres, swatch).
    pub island_bands: Vec<(f32, Swatch)>,
    pub presets: Vec<Preset>,
    pub ramps: Vec<Ramp>,
    /// Sea body colours for the water shader: deep, scatter, shallow, foam (exported as `sea`).
    pub(crate) sea: Vec<Swatch>,
    /// Clear-sky colours per time of day (zenith, horizon, key light); the presets derive from them (exported
    /// as `sky`).
    pub(crate) sky: Vec<tokens::SkyTokens>,
}
