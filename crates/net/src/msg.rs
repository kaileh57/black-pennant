//! Contract-owned (no work unit). Every session-level message on the wire, in one place: this file plus
//! `schema/mod.rs` (snapshot wire items) is the one replication schema (L920-922). Encoding is in
//! `schema/codec.rs`; snapshot frames are raw bytes from `delta`.
//!
//! Channel use (see [`crate::channel::Channel`]):
//! - `Session`: [`Hello`] (client, first message, frozen header), then [`ClientMsg`] / [`ServerMsg`].
//! - `Events`, `Entity`, `Projectile`: [`EventBatch`] (server to client).
//! - `Unreliable`: [`InputPacket`] (client to server); snapshot frames from `delta` (server to client).
//! - `Bulk`: [`BulkMsg`] both ways.
//!
//! Clients never stamp their own player slot or RTT: the server builds `CommandEnvelope` from a
//! [`WireCommand`] with the slot it assigned and the RTT it measured (L951).
use crate::error::{DropReason, JoinRefusal, SessionEnd};
use content::enums::GameMode;
use content::ids::{ContentHash, PlayerSlot, SideId};
use content::ruleset::Ruleset;
use serde::{Deserialize, Serialize};
use sim_systems::modes::{LobbyIssue, LobbySetup};
use sim_systems::protocol::command::{Command, PlayerInput};
use sim_systems::protocol::config::SortieSummary;
use sim_systems::protocol::event::SimEvent;
use sim_systems::protocol::snapshot::Viewer;
use sim_systems::theatre::{AfterAction, Briefing, ChartView, TheatreCommand, TheatreEvent, TheatreReject};
use std::net::SocketAddr;

/// First four bytes of every hello. The hello header (magic, then protocol version as u32 little-endian) is
/// frozen forever so any two builds can tell each other their versions (L1003).
pub const HELLO_MAGIC: [u8; 4] = *b"BPNH";

/// Where a build came from; GitHub and Steam builds with one protocol version interoperate over UDP (L1003).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuildKind { #[default] GitHub, Steam, Dev }

/// Client to server, first message on `Session`. Encoded as `HELLO_MAGIC`, `protocol` (u32 LE), then the
/// rest with bincode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub game_version: String,
    pub build: BuildKind,
    /// Random 64-bit per-install id (L1003); also the renet2 client id on UDP.
    pub client_id: u64,
    /// Display name chosen locally; no accounts. The host trims it to 24 characters.
    pub name: String,
}

/// What a client learns on admission.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    pub protocol: u32,
    pub slot: PlayerSlot,
    pub mode: GameMode,
    pub lobby_name: String,
    pub host_game_version: String,
    /// True when this join reclaimed a slot held for a dropped player (5 minutes, L834).
    pub rejoined: bool,
    /// Session phase at admission.
    pub phase: crate::session::SessionPhase,
}

/// A command as the client sends it: no player slot, no RTT (the server stamps both).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireCommand { pub seq: u32, pub client_render_tick: u64, pub command: Command }

/// Snapshot acknowledgement carried in every input packet: the newest frame sequence received and a bit mask
/// of the 32 before it (bit n = `newest - 1 - n`). The server picks delta baselines only from acked frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotAck { pub newest: u32, pub mask: u32 }

/// Client to server on `Unreliable`, 30 Hz (L1424): the last up to 4 inputs, oldest first, plus the ack.
/// Sent even with no hull (spectators), with `inputs` empty, so acks and "frames behind" keep flowing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InputPacket { pub ack: SnapshotAck, pub inputs: Vec<PlayerInput> }

/// Server to client on `Events`, `Entity` or `Projectile`: events in emission order with audience stripped.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EventBatch { pub events: Vec<WireEvent> }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireEvent { pub tick: u64, pub event: SimEvent }

/// Bandwidth quality shown on the connection panel (L1033).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkQuality { #[default] Good, Fair, Poor }

/// A drop-rule condition that has held for at least 5 s (warning) on its way to the 15 s drop (L1035).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DropWarning { HighPing, PacketLoss, FallingBehind }

/// One row of the connection panel everyone sees.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlayerLink {
    pub slot: PlayerSlot,
    pub name: String,
    pub host: bool,
    pub ping_ms: u16,
    /// Packet loss 0..=1 over the monitor window.
    pub loss: f32,
    /// Snapshots behind the newest one sent (L1033).
    pub frames_behind: u16,
    pub quality: LinkQuality,
    /// Current adaptive cap in kbps (128 to 768, L937).
    pub cap_kbps: u16,
    pub warning: Option<DropWarning>,
    /// Dropped mid-operation and held for rejoin: seconds left of the 5-minute hold.
    pub held_s: Option<u16>,
}

/// Connection panel, server to every client at 1 Hz and on change (L1033).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConnectionPanel {
    pub players: Vec<PlayerLink>,
    /// Host simulation tick rate actually achieved over the last second, Hz (every player sees it, L1025).
    pub host_tick_hz: f32,
    pub host_tick_p95_ms: f32,
    /// Upload saturation in force: caps cut, joins refused (L1031).
    pub saturated: bool,
}

/// One lobby member as everyone sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LobbyMember {
    pub slot: PlayerSlot,
    pub name: String,
    pub host: bool,
    pub team: u8,
    pub ready: bool,
    /// Chosen design's hash (exterior hash for the other PvP team, L945), if any.
    pub blueprint: Option<ContentHash>,
    pub complexity: f64,
    /// Blueprint upload to the host in progress: received fraction.
    pub upload: Option<f32>,
}

/// Lobby state, server to every client on change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LobbyView {
    pub mode: GameMode,
    pub name: String,
    pub ruleset: Ruleset,
    pub members: Vec<LobbyMember>,
    /// Upload-derived player limit including the host (L1031).
    pub max_players: u8,
    /// Problems that block the start (`sim_systems::modes::validate_lobby`).
    pub issues: Vec<LobbyIssue>,
    pub can_start: bool,
    pub saturated: bool,
}

/// Client to server lobby requests. The host applies them to the `LobbySetup` it owns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LobbyRequest {
    SetReady(bool),
    SetTeam(u8),
    /// Choose a design by hash; the client uploads it on `Bulk` if the host does not have it.
    SetBlueprint(ContentHash),
    /// Host-only settings from a remote admin are not supported; this is the host's own client in a listen
    /// session (slot 0 over the in-memory transport).
    SetSetup(Box<LobbySetup>),
    Start,
}

/// What a client must prepare before the first snapshot of a sortie.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SortieIntro {
    pub mode: GameMode,
    pub seed: u64,
    pub viewer: Viewer,
    pub side: SideId,
    pub tick: u64,
    /// Blueprints the client will need for its own side, so it can fetch them during loading.
    pub own_side_blueprints: Vec<ContentHash>,
    /// Operation key for campaign and skirmish, empty otherwise.
    pub operation: String,
}

/// Co-op host succession (L1005): members whose own port mapping worked, in join order, with their external
/// endpoints, and a fresh secret the next host will use. Sent to every member on change.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SuccessionList { pub entries: Vec<SuccessionEntry>, pub next_secret: [u8; 10], pub campaign_id: u64 }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SuccessionEntry { pub client_id: u64, pub slot: PlayerSlot, pub external: SocketAddr }

/// A member's background port-mapping result, reported to the host for the succession list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MappingReport { pub external: Option<SocketAddr>, pub double_nat: bool }

/// Server to client on `Session`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ServerMsg {
    Welcome(Welcome),
    /// Sent just before the host disconnects the client.
    Refused(JoinRefusal),
    Dropped(DropReason),
    Lobby(Box<LobbyView>),
    SortieLoading(Box<SortieIntro>),
    /// The sortie runs; snapshots follow.
    SortieRunning { tick: u64 },
    /// The sortie is over and the session returns to the lobby or chart table.
    SortieClosed { summary: Box<SortieSummary> },
    Connection(Box<ConnectionPanel>),
    Saturation { on: bool },
    // Campaign co-op (L832-845); the host app owns the `Theatre` and passes these through the session.
    Theatre(TheatreEvent),
    TheatreRejected(TheatreReject),
    Chart(Box<ChartView>),
    Briefing(Box<Briefing>),
    AfterAction(Box<AfterAction>),
    Succession(Box<SuccessionList>),
    /// A transfer is starting on `Bulk` (checkpoint, save copy, replay, blueprint).
    TransferAnnounce(TransferStart),
    /// A requested blueprint will not be sent.
    BlueprintRefused { hash: ContentHash, reason: BlueprintRefusal },
    SessionEnded(SessionEnd),
}

/// Client to server on `Session` (after the hello).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClientMsg {
    Command(WireCommand),
    Lobby(LobbyRequest),
    Theatre(TheatreCommand),
    /// Ask for a blueprint by hash (from `EntityIntro::blueprint` or the lobby). The server checks
    /// entitlement: a PvP opponent only ever receives exterior hashes (L945).
    RequestBlueprint(ContentHash),
    Mapping(MappingReport),
    /// Ask for the released PvP replay after the match (L1049).
    RequestReplay,
    Leave,
}

/// Why a blueprint transfer was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlueprintRefusal { NotEntitled, Unknown, TooLarge, Invalid }

/// What a bulk transfer carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TransferKind {
    /// `content::share::PackedBlueprint::compressed` of this hash.
    Blueprint(ContentHash),
    /// Co-op operation checkpoint at this tick: bincode of [`CheckpointBundle`] (L843).
    Checkpoint(u64),
    /// Co-op save copy with this sequence number: bincode of `content::save::SaveGame` (L834, L843).
    SaveCopy(u64),
    /// The released PvP replay, already cut for the receiver's side (L1049).
    Replay,
}

/// Start of a chunked transfer on `Bulk`; chunks are 16 KiB (`content::share::BLUEPRINT_CHUNK_BYTES`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferStart { pub id: u32, pub kind: TransferKind, pub total_bytes: u32, pub chunks: u32, pub hash: ContentHash }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BulkMsg {
    Start(TransferStart),
    Chunk { id: u32, index: u32, bytes: Vec<u8> },
    Cancel { id: u32 },
}

/// What a co-op checkpoint transfer carries: the Sim keyframe record plus the theatre save (L843).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckpointBundle { pub checkpoint: content::save::SortieCheckpoint, pub save: content::save::SaveGame }
