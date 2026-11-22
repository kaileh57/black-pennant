//! Contract-owned (no work unit). Replicated weather state (DESIGN.md L1116, L1857-1868).
//! sim-core drives waves from it, sim-systems reads visibility and clutter, the renderer picks presets.
use serde::{Deserialize, Serialize};

/// Weather family. Each has a visibility V and an allowed sea-state range (table at L1859-1866).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum WeatherKind { #[default] Clear, Haze, Overcast, Rain, Storm, FogBank }
impl WeatherKind {
    /// Visibility V in metres (L1859-1866). Renderer fog density is 3.912 / V.
    pub fn visibility_m(self) -> f64 { match self { Self::Clear => 30_000.0, Self::Haze => 8_000.0, Self::Overcast => 12_000.0, Self::Rain => 2_000.0, Self::Storm => 1_500.0, Self::FogBank => 400.0 } }
    /// Inclusive WMO sea-state code range this weather may carry (L1859-1866).
    pub fn sea_state_range(self) -> (u8, u8) { match self { Self::Clear => (0, 4), Self::Haze => (2, 4), Self::Overcast => (3, 5), Self::Rain => (4, 6), Self::Storm => (7, 8), Self::FogBank => (0, 3) } }
}

/// WMO sea-state code 0..=8. Significant wave height per code 2..=8 is fixed by the spec (L1887).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SeaState(pub u8);
impl SeaState {
    pub const CALM: SeaState = SeaState(0);
    /// Splash basin and gate presets name Beaufort 4 and Beaufort 7 (L216, L172). DECISION (spec gives no
    /// Beaufort-to-code table): Beaufort 4 maps to code 3 (Hs 0.9 m), Beaufort 7 to code 6 (Hs 5.0 m).
    pub const BEAUFORT_4: SeaState = SeaState(3);
    pub const BEAUFORT_7: SeaState = SeaState(6);
    /// Significant wave height in metres. Codes 2..=8 are the spec's Pierson-Moskowitz targets (L1887);
    /// codes 0 and 1 (0 m, 0.1 m) are this contract's choice.
    pub fn significant_height_m(self) -> f64 { [0.0, 0.1, 0.3, 0.9, 1.9, 3.2, 5.0, 7.5, 11.0][self.0.min(8) as usize] }
}

/// Replicated weather state. Sent reliably on change together with the wave seed (L920).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WeatherState {
    pub kind: WeatherKind,
    pub sea_state: SeaState,
    /// Direction the wind blows toward, radians, compass convention (0 = north = world -Z, clockwise).
    pub wind_dir_rad: f64,
    pub wind_speed_mps: f64,
    /// 0.0..=1.0.
    pub cloud_cover: f64,
    /// Visibility V in metres. Caps every visual and camera detection (L436).
    pub visibility_m: f64,
    /// 0.0..=1.0 rain intensity.
    pub rain: f64,
    /// Sun elevation in degrees, fixed per sortie at the theatre hour (L1868).
    pub sun_elevation_deg: f64,
    /// Sun azimuth in degrees, compass convention.
    pub sun_azimuth_deg: f64,
}
impl Default for WeatherState {
    fn default() -> Self {
        Self { kind: WeatherKind::Clear, sea_state: SeaState::BEAUFORT_4, wind_dir_rad: 0.0, wind_speed_mps: 6.0, cloud_cover: 0.3, visibility_m: 30_000.0, rain: 0.0, sun_elevation_deg: 12.0, sun_azimuth_deg: 250.0 }
    }
}
