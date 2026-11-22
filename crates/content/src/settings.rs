//! Contract-owned types (no work unit). Settings the simulation and the headless server read.
//! Client-only settings (graphics tier, key remaps, trackpad switches) live in gdclient, not here.
use crate::campaign::DifficultySliders;
use crate::enums::GameMode;
use crate::ruleset::Ruleset;
use crate::weather::SeaState;
use serde::{Deserialize, Serialize};

/// Sim-affecting session settings chosen before a sortie.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SimSettings {
    pub difficulty: DifficultySliders,
    pub ironman: bool,
    pub ruleset: Ruleset,
    /// Host lets guest Luau scripts run; only honoured after the fuzz gate and never in PvP (L2119-2121, L2144).
    pub guest_scripts: bool,
    /// Fixed worker count of the sim pool (3, L1084). Tests may use 1.
    pub sim_threads: u8,
}

/// Splash basin options (L216).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplashTarget { #[default] Drones, PatrolBoat }
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SplashOptions { pub water: SeaState, pub target: SplashTarget }

/// Dedicated server RON config: port, mode, ruleset, lobby size, upload figure (L1015).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    pub port: u16,
    pub mode: GameMode,
    pub ruleset: Ruleset,
    pub lobby_size: u8,
    /// Upload speed in kbps (default 1 Mbps, L1031).
    pub upload_kbps: u32,
    pub settings: SimSettings,
    pub lobby_name: String,
}
/// Default UDP port (L994).
pub const DEFAULT_PORT: u16 = 47610;
impl Default for ServerConfig {
    fn default() -> Self { Self { port: DEFAULT_PORT, mode: GameMode::Sandbox, ruleset: Ruleset::default(), lobby_size: 4, upload_kbps: 1_000, settings: SimSettings::default(), lobby_name: String::new() } }
}
