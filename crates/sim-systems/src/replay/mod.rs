//! Replays. `record.rs` is unit `sys-replay-record`, `play.rs` is unit `sys-replay-play`; the frame types
//! here are contract-owned. DESIGN.md L1043-1050.
//!
//! A replay is recorded from the server's authoritative output stream: vessel poses, projectile spawn and
//! impact events, damage events, track changes and weather changes. There is no re-simulation, so it plays
//! identically on every platform. The kill cam, the graphics golden captures and the frame-time CI scripted
//! replays use the same format. Format: `content::replay::ReplayHeader`, then frames: poses at 15 Hz (every
//! other snapshot; playback interpolates), every event, the shot log, and a replicated-state keyframe every
//! 60 s for seeking; compressed; stamped with a format version. A build opens only its own format version.
//! Size gate: a 30-minute reference-battle replay is at most 30 MB; fallback: hulls over 5 km from every
//! player hull record at 10 Hz.
//! PvP: replays are released only after the match ends and carry exterior-only blueprints for the other
//! team; the in-match kill cam shows only what the killed player's side detected plus the exterior of the
//! vessel that fired the killing shot.
//! Compression goes through `content::share::compress` / `decompress_bounded`, the same codec as share
//! strings and saves; the open owner decision on that codec (zstd level 19 against the pure-Rust workspace)
//! covers replays too.
pub mod play;
pub mod record;

use crate::prelude::*;
use crate::protocol::config::EntityIntro;
use crate::protocol::event::{OutEvent, ShotLogEntry};
use crate::protocol::snapshot::{BodyView, DecoyView, ObjectiveView, Pacing, TrackView, VesselView};
use content::weather::WeatherState;

/// One recorded frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ReplayFrame {
    /// True poses of everything at one tick (15 Hz; distant hulls at 10 Hz under the size fallback).
    Poses { tick: u64, vessels: Vec<VesselView>, bodies: Vec<BodyView>, decoys: Vec<DecoyView> },
    /// Each side's picture at 1 Hz, so the plot can be replayed as that side saw it.
    Picture { tick: u64, side: SideId, tracks: Vec<TrackView> },
    Event(OutEvent),
    Shot(ShotLogEntry),
    Keyframe(Box<ReplicatedKeyframe>),
    /// Every live entity with the sides that may draw it, ascending entity (`InterestState::sides_seeing`).
    /// Written after each keyframe and whenever any side's drawable set changes; it is what `Viewer::Side`
    /// playback and the PvP cut filter poses by, so neither has to reconstruct interest from events.
    Drawable { tick: u64, seen: Vec<(EntityId, SideMask)> },
}

/// One side's facts at a keyframe: who is on it, who it is allied with, and its own introductions of the
/// entities it may draw (exterior-only for the other team in PvP), so a side's view can start here.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SideCut {
    pub side: SideId,
    /// Allied mask, always including the side itself.
    pub allied: SideMask,
    /// Player slots on the side with their hull, if any (resolves `Audience::Players`).
    pub players: Vec<(PlayerSlot, Option<EntityId>)>,
    /// `EntityRevealed` content for every entity the side may draw now, ascending entity.
    pub intros: Vec<EntityIntro>,
}

/// Replicated state at a keyframe, enough to start playback from there: every live entity's static facts and
/// damage so far, weather and wave seed, pacing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplicatedKeyframe {
    pub tick: u64,
    pub weather: WeatherState,
    pub wave_seed: u64,
    pub entities: Vec<EntityIntro>,
    pub vessels: Vec<VesselView>,
    pub bodies: Vec<BodyView>,
    pub decoys: Vec<DecoyView>,
    pub pictures: Vec<(SideId, Vec<TrackView>)>,
    #[serde(default)]
    pub pacing: Pacing,
    #[serde(default)]
    pub objectives: Vec<ObjectiveView>,
    /// Per side, ascending side id.
    #[serde(default)]
    pub sides: Vec<SideCut>,
    /// PvP only: each vessel's weather-deck part slots (`ExteriorIndex::parts`), ascending entity. The other
    /// team's turrets are drawn only for these slots, as in live play.
    #[serde(default)]
    pub exterior_parts: Vec<(EntityId, Vec<PartSlot>)>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("not a replay file")]
    Magic,
    #[error("replay format {found} cannot be opened by this build (format {expected})")]
    Format { found: u32, expected: u32 },
    #[error("replay is damaged: {0}")]
    Corrupt(String),
}
