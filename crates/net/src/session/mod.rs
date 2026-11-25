//! Contract-owned (no work unit). Session phases, options and the events sessions hand to their app.
//! `server.rs` and `local.rs` are unit `net-server-session`; `client.rs` is unit `net-client-session`.
//! DESIGN.md L899 (single-player is the same program over the in-memory transport), L832-845 (co-op session
//! rules), L957-965 (modes), L1015 (dedicated server), L1019-1039 (overload, upstream, quality).
//!
//! Ownership: the host app (gdclient's sim thread, or the server binary) owns the `ServerSession`, the
//! campaign `Theatre` (never inside net), the rayon pool and the clock. It builds each `Sim` (it has the
//! `SimDeps`) and hands it to `ServerSession::start_sortie`; it gets it back with `end_sortie` to read the
//! `SortieResult`. Theatre traffic passes through the session: clients' `TheatreCommand`s surface as
//! [`HostEvent::Theatre`]; the app answers with `ServerSession::send_to` / `broadcast` carrying `ServerMsg`
//! chart, briefing, theatre event and after-action variants.
pub mod client;
pub mod local;
pub mod server;

// Compile-time contract: gdclient moves sessions to its sim and network threads.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<server::ServerSession>();
    assert_send::<client::ClientSession>();
};

use crate::blueprints::Completed;
use crate::error::{DropReason, JoinRefusal, SessionEnd};
use crate::events::RouterOptions;
use crate::interp::InterpTuning;
use crate::join::ClientIdentity;
use crate::msg::{BuildKind, CheckpointBundle, ConnectionPanel, LobbyView, MappingReport, SortieIntro, SuccessionList, Welcome, WireEvent};
use crate::priority::RateTuning;
use crate::quality::QualityTuning;
use content::enums::GameMode;
use content::ids::{ContentHash, PlayerSlot};
use serde::{Deserialize, Serialize};
use sim_systems::protocol::config::SortieSummary;
use sim_systems::theatre::{AfterAction, Briefing, ChartView, TheatreCommand, TheatreEvent, TheatreReject};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionPhase {
    /// Lobby: skirmish, sandbox and PvP setup; co-op before the campaign starts.
    #[default] Lobby,
    /// Campaign chart table, briefing and dry dock between operations (co-op joins happen here, L834).
    Theatre,
    /// A sortie is being built; clients fetch blueprints.
    Loading,
    /// The sortie runs: the session steps the Sim and sends snapshots.
    Sortie,
    /// Debrief after a sortie, before the app returns to `Lobby` or `Theatre`.
    AfterAction,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerOptions {
    pub game_version: String,
    pub lobby_name: String,
    pub mode: GameMode,
    /// Host upload figure in kbps (default 1 Mbps, L1031).
    pub upload_kbps: u32,
    /// Dedicated server: no host member, slot 0 is an ordinary player.
    pub dedicated: bool,
    pub quality: QualityTuning,
    /// Replication rates. The default is fallback 1 of L935 (vessels other than the client's own at 20 Hz):
    /// the per-client total measured over 256 kbps at 30 Hz, which is the condition L935 names.
    pub rates: RateTuning,
    pub router: RouterOptions,
    /// Record a replay of every sortie (L1043).
    pub record_replays: bool,
    /// Synthetic tick cost added to every measured tick, for the ladder controller gate (L1027).
    pub synthetic_tick_ms: f32,
    /// Most ticks one `update` may run to catch up before it reports the backlog instead (keeps the host
    /// responsive; the ladder sees the backlog through `HostLoad`).
    pub max_catch_up_ticks: u32,
}
impl ServerOptions {
    pub fn new(game_version: &str, mode: GameMode) -> Self {
        Self { game_version: game_version.into(), lobby_name: String::new(), mode, upload_kbps: 1_000, dedicated: false, quality: QualityTuning::default(), rates: RateTuning::fallback_20hz(), router: RouterOptions::default(), record_replays: true, synthetic_tick_ms: 0.0, max_catch_up_ticks: 8 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClientOptions {
    pub identity: ClientIdentity,
    pub game_version: String,
    pub build: BuildKind,
    pub interp: InterpTuning,
}

/// What the host app learns from `ServerSession::update`.
#[derive(Clone, Debug, PartialEq)]
pub enum HostEvent {
    PlayerJoined { slot: PlayerSlot, client_id: u64, name: String, rejoined: bool },
    /// Mid-sortie drops are already passed to `Sim::drop_player` by the session; `held` says the slot is held.
    PlayerLeft { slot: PlayerSlot, client_id: u64, reason: DropReason, held: bool },
    JoinRefused { client_id: u64, reason: JoinRefusal },
    /// A held slot expired: co-op officers keep the hull to the end of the operation (L834).
    HoldExpired { slot: PlayerSlot, client_id: u64 },
    LobbyChanged,
    /// A member asked to start (the host app decides and builds the Sim).
    StartRequested { by: PlayerSlot },
    Theatre { slot: PlayerSlot, command: TheatreCommand },
    /// A validated blueprint arrived from a member.
    BlueprintAccepted { slot: PlayerSlot, hash: ContentHash },
    BlueprintRejected { slot: PlayerSlot, hash: ContentHash },
    MappingReported { slot: PlayerSlot, report: MappingReport },
    /// The Sim reported the sortie over (`Sim::is_over`); the app calls `end_sortie`.
    SortieOver,
    /// Overload ladder or saturation changed (already logged and broadcast).
    Saturation { on: bool },
    /// A member asked for the released PvP replay.
    ReplayRequested { slot: PlayerSlot },
    /// A member's bulk transfer finished (save copy sync in co-op).
    BulkReceived { slot: PlayerSlot, completed: Completed },
}

/// What the client app learns from `ClientSession::update`.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientEvent {
    Welcomed(Welcome),
    Refused(JoinRefusal),
    Lobby(Box<LobbyView>),
    SortieLoading(Box<SortieIntro>),
    SortieRunning { tick: u64 },
    SortieClosed(Box<SortieSummary>),
    /// A simulation event for the HUD, log, audio and VFX (already applied to the snapshot buffer and shells).
    Sim(WireEvent),
    Connection(Box<ConnectionPanel>),
    Saturation { on: bool },
    Theatre(TheatreEvent),
    TheatreRejected(TheatreReject),
    Chart(Box<ChartView>),
    Briefing(Box<Briefing>),
    AfterAction(Box<AfterAction>),
    Succession(Box<SuccessionList>),
    Checkpoint(Box<CheckpointBundle>),
    /// A blueprint is now in the local store.
    BlueprintReady(ContentHash),
    BlueprintRefused(ContentHash, crate::msg::BlueprintRefusal),
    /// A bulk transfer finished (replay, save copy).
    BulkReceived(Completed),
    /// No packet from the host for 10 s (co-op re-host, L1005).
    HostSilent,
    Ended(SessionEnd),
}
