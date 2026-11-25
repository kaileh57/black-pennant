//! Contract-owned types (no work unit). Replay file header. DESIGN.md L1043-1050.
//! A replay is the server's authoritative output stream: poses at 15 Hz, every event, the shot log and a
//! replicated-state keyframe every 60 s, compressed. A build opens only its own replay format version.
//! The body encoding belongs to `sim_systems::protocol` and `net`.
use crate::enums::GameMode;
use crate::ids::ContentHash;
use crate::weather::WeatherState;
use serde::{Deserialize, Serialize};

pub const REPLAY_FORMAT: u32 = 1;
pub const REPLAY_MAGIC: [u8; 4] = *b"BPRP";
pub const REPLAY_POSE_HZ: u32 = 15;
pub const REPLAY_KEYFRAME_S: u32 = 60;
/// Replays kept besides pinned ones (L1047).
pub const REPLAY_KEEP: usize = 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplayHeader {
    pub magic: [u8; 4],
    pub format: u32,
    pub game_version: String,
    pub protocol_version: u32,
    pub tuning_version: u32,
    pub mode: GameMode,
    pub seed: u64,
    pub weather: WeatherState,
    pub duration_ticks: u64,
    /// Blueprints referenced, by hash; in PvP the other team's are exterior-only (L1049).
    pub blueprints: Vec<ContentHash>,
    pub player_names: Vec<String>,
    /// Operation key for campaign and skirmish sorties, empty otherwise.
    pub operation: String,
    pub pinned: bool,
    /// Byte offsets of each 60 s keyframe in the body, for seeking.
    pub keyframe_offsets: Vec<u64>,
}
