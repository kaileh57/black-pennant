//! Contract-owned (no work unit). The wire and render protocol: everything that crosses between the
//! authoritative simulation and anything outside it (net, gdclient, the headless server, replays, bots).
//! DESIGN.md L897-953 (multiplayer architecture, wire content, aiming), L529 (visibility), L1043-1050
//! (replays), L1676-1687 (alerts), L104-116 (in-battle commands and time rules).
//!
//! Rules every type here follows:
//! - plain serde data, no references, no `Arc`, no trait objects; net encodes these with its own schema and
//!   may quantise (pose to about 2 mm, smallest-three quaternion, L920), but the fields are the contract;
//! - a client never receives truth its side has not sensed (L529, L924): a [`snapshot::Snapshot`] carries
//!   own-side vessels in full, other entities only as [`snapshot::TrackView`]s, plus the true pose of
//!   entities inside an own-side unit's visual range (needed to draw them);
//! - every player-facing sentence is a [`text::Msg`] (translation key plus typed arguments) or a typed reason
//!   enum that `crate::text` turns into one; no English text is ever produced here;
//! - projectiles are never in snapshots: one spawn event, client-side integration with
//!   `sim_core::projectiles::ballistic_step`, then a reliable impact event (L926).
//!
//! Files: [`text`] (messages and reasons), [`command`] (client to server), [`snapshot`] (30 Hz per-viewer
//! state), [`event`] (reliable events with an audience), [`alert`] (alert model), [`config`] (sortie config,
//! sides, players, result).
pub mod alert;
pub mod command;
pub mod config;
pub mod event;
pub mod snapshot;
pub mod text;

pub use alert::*;
pub use command::*;
pub use config::*;
pub use event::*;
pub use snapshot::*;
pub use text::*;

/// Protocol version. A join with a different version is refused with a message naming both (L1003).
/// Also stamps keyframes (`content::save::SortieCheckpoint::keyframe_format`) and replays.
pub const PROTOCOL_VERSION: u32 = 1;
