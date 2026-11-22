//! Contract-owned (no work unit). Reliable, sequenced events from the server, each with the audience that may
//! receive it. DESIGN.md L920-926 (damage stream, splits, weather, projectile spawn and impact events),
//! L924 (interest management), L945 (exterior-only damage for the other team in PvP), L951 (shot log),
//! L1043-1050 (replays record every event), L1661-1665 (track and control changes get a log entry).
//!
//! `Sim::drain_events` returns [`OutEvent`]s in emission order. net delivers an event to a client only when
//! [`Audience::admits`] that client; the replay recorder keeps `Audience` so PvP replays can be cut per side.
//! Where two audiences must see different content for the same fact (PvP cell damage), the simulation emits
//! two events with disjoint audiences; the filter is `crate::sim::interest::exterior_changes`.
use super::alert::{AlertState, AlertView};
use super::command::{ControlRole, Query, ShotDesignation};
use super::config::{EntityIntro, SortieSummary};
use super::snapshot::{ActionView, ObjectiveState, Pacing, SeekerPhase};
use super::text::{ControlHolder, FireReason, HoldReason, Msg, RungReason, StanceReason};
use crate::ids::{AlertId, MountRef, ObjectiveId, PlayerMask, SideMask, TrackId};
use content::campaign::AceCue;
use content::enums::{Domain, IdRung, OfficerRole, Stance};
use content::graph::PortRef;
use content::ids::GraphNodeId;
use content::ids::{AceId, ClassId, PartSlot, PlayerSlot, SideId};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use sim_core::events::PhysEvent;
use sim_core::EntityId;

/// Who may receive an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Audience {
    All,
    /// Every player on any of these sides (the sides that have detected or can see the subject).
    Sides(SideMask),
    Players(PlayerMask),
    /// Host only: never leaves the server (shot log rows, belief-map debug, script backstop trips).
    Host,
}
impl Audience {
    pub fn side(side: SideId) -> Self { Audience::Sides(SideMask::only(side)) }
    pub fn player(p: PlayerSlot) -> Self { Audience::Players(PlayerMask(1 << (p.0 & 7))) }
    /// True when a player on `side` in slot `player` may receive the event.
    pub fn admits(self, player: PlayerSlot, side: SideId) -> bool {
        match self { Audience::All => true, Audience::Sides(m) => m.has(side), Audience::Players(m) => m.has(player), Audience::Host => false }
    }
}

/// Delivery lane hint for net: `Critical` events are never dropped or delayed behind bulk lanes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lane { #[default] Reliable, /// Projectile spawns and impacts: reliable, but capped at 25% of client bandwidth by batching (L929, L935).
    Projectile, /// Cell damage and splits: reliable and sequenced per vessel (L920).
    Damage, /// Log-only lines the HUD files silently (L1671).
    Log }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutEvent { pub tick: u64, pub audience: Audience, pub lane: Lane, pub event: SimEvent }

/// Why a script node went grey (L363-371).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScriptFault { Error, Memory, Budget, Backstop }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason { NotAllowedInMode, NotAllowedByRuleset, OnlyInDryDockOrSplash, NoSuchPart, NoSuchTrack, FriendCannotBeEngaged, RoleNotHeld, HullLost, TooManyRules, InvalidValue, SinglePlayerOnly, ConditionsNotMet, Duplicate }

/// One link of a "Why?" chain (L353); `node` opens the responsible graph node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WhyLink { pub text: Msg, pub node: Option<GraphNodeId>, pub part: Option<PartSlot> }

/// One identification candidate on a track card (L470-477).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidateView { pub class: Option<ClassId>, pub domain: Domain, pub civilian: bool, pub own_side: bool }

/// One overridden value in the Bridge Board diff (L351).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OverrideView { pub key: super::command::OverrideKey, pub label: Msg, pub default: content::graph::Value, pub current: content::graph::Value, pub layer: content::enums::ControlLayer }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum QueryReply {
    Why { slot: PartSlot, chain: Vec<WhyLink> },
    Scope { port: PortRef, value: content::graph::Value },
    TrackCard { track: TrackId, candidates: Vec<CandidateView>, evidence: Vec<(IdRung, RungReason, u64)>, stance_reason: StanceReason },
    Overrides { role: OfficerRole, values: Vec<OverrideView> },
}

/// One row of the shot log (L951): manual shots only.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShotLogEntry {
    pub tick: u64,
    pub player: PlayerSlot,
    pub mount: PartSlot,
    pub shot: ShotDesignation,
    pub rtt_ms: u16,
    pub client_render_tick: u64,
    pub target_speed_mps: f32,
    /// Filled when the round resolves; `None` while in flight.
    pub hit: Option<bool>,
    /// Which projectiles the row's round group became, so `input::resolve_shots` can match impacts and
    /// splashes to it. Added by `sys-input` (additive; older logs read as unbound).
    #[serde(default)]
    pub binding: ShotBinding,
}

/// Link from a shot-log row to the projectiles its round group spawned (server bookkeeping).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShotBinding {
    /// Vessel that fired.
    pub shooter: EntityId,
    /// First projectile id of the group and how many were spawned (ids of one mount's group are contiguous).
    pub first_projectile: u32,
    pub projectiles: u8,
    /// Projectiles of the group not yet closed by an impact or splash.
    pub open: u8,
}
impl Default for ShotBinding { fn default() -> Self { Self { shooter: EntityId::NONE, first_projectile: 0, projectiles: 0, open: 0 } } }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SimEvent {
    // ---- Session ----
    /// First event of a sortie for each client: wave seed and weather arrive inside
    /// `PhysEvent::WeatherChanged` right after it.
    SortieStarted { seed: u64, tick_hz: u32 },
    /// Pause, compression, time scale or ladder rung changed (every change shows in HUD and log, L1025).
    PacingChanged(Pacing),
    /// Overload ladder step (L1019-1029).
    LadderStep { rung: u8, sub_step: u8, up: bool },
    /// The sortie is over; the summary is what a client may show. The host reads `Sim::result()` for the full
    /// `SortieResult`.
    SortieEnded(Box<SortieSummary>),
    PlayerJoined { player: PlayerSlot, name: String, side: SideId, vessel: EntityId },
    /// Officers keep running a dropped player's hull (L838).
    PlayerDropped { player: PlayerSlot, officers_in_command: bool },
    CommandRejected { player: PlayerSlot, seq: u32, reason: RejectReason },
    QueryReply { player: PlayerSlot, reply: QueryReply },

    // ---- Interest management ----
    /// An entity entered the viewer side's drawable set: static facts plus accumulated damage so far.
    EntityRevealed(Box<EntityIntro>),
    /// It left the drawable set (out of visual range, despawned). Tracks may persist.
    EntityHidden { entity: EntityId },

    // ---- Physical events passed through after audience filtering ----
    /// Projectile spawns and impacts, body spawns and bursts, decoys, cell damage, fires, splits, losses,
    /// weather. In PvP the other team's `CellsDamaged` lists exterior cells only (L945).
    Phys(PhysEvent),

    // ---- Tracks (L458-477, L496-503, L1661) ----
    TrackCreated { side: SideId, track: TrackId, domain: Domain },
    TrackDropped { side: SideId, track: TrackId },
    /// Two tracks proved to be one object: `from` is retired, `into` continues. Numbers are never reused.
    TrackMerged { side: SideId, from: TrackId, into: TrackId },
    TrackSplit { side: SideId, from: TrackId, new: TrackId },
    RungChanged { side: SideId, track: TrackId, rung: IdRung, reason: RungReason, class: Option<ClassId> },
    StanceChanged { side: SideId, track: TrackId, stance: Stance, reason: StanceReason },
    /// ESM warning turned on or off for a vessel (L510).
    EsmWarning { vessel: EntityId, on: bool },
    /// An emitter started or stopped radiating (Bridge Board, L507).
    Radiating { vessel: EntityId, slot: PartSlot, on: bool },

    // ---- Engagement and guidance (L407-408, L353, L518-524) ----
    Engaging { mount: MountRef, track: TrackId, reason: FireReason },
    Holding { mount: MountRef, track: TrackId, reason: HoldReason },
    /// "Solution unconverged": fired on the last iterate (L518).
    SolutionUnconverged { mount: MountRef, track: TrackId, error_m: f32 },
    Seeker { body: EntityId, phase: SeekerPhase, target: TrackId },
    /// A decoy took a seeker's lock (L524).
    Seduced { body: EntityId, decoy: EntityId },
    /// Manual shot resolved; host only until the replay is released.
    ShotLogged(ShotLogEntry),

    // ---- Officers, aces, control (L351, L774, L953, L1665) ----
    /// An officer reports: text with radio effect, two 40-character lines at most (L1706).
    OfficerMessage { vessel: EntityId, role: OfficerRole, text: Msg },
    AceMessage { ace: AceId, cue: AceCue, text: Msg, from: Option<TrackId> },
    ControlChanged { vessel: EntityId, role: ControlRole, holder: ControlHolder },
    /// A value was overridden or reverted (A chip to hand glyph).
    OverrideChanged { vessel: EntityId, key: super::command::OverrideKey, overridden: bool },
    ScriptGreyedOut { vessel: EntityId, node: GraphNodeId, fault: ScriptFault },
    /// A script was stopped only by the wall-clock backstop: this tick is not reproducible (L367).
    TickNotReproducible { vessel: EntityId, node: GraphNodeId },

    // ---- Alerts (L1676-1687) ----
    AlertRaised(AlertView),
    AlertChanged { id: AlertId, state: AlertState },
    /// A Warning crossed its threshold and became an Alarm or Emergency.
    AlertEscalated(AlertView),
    AlertsSilenced { vessel: EntityId, until_tick: u64 },
    /// Flood mode on or off (more than 10 alerts in 10 minutes).
    AlertFlood { vessel: EntityId, on: bool },

    // ---- Objectives and special operations (L778-797) ----
    ObjectiveChanged { id: ObjectiveId, state: ObjectiveState, progress: f32 },
    ActionChanged(ActionView),
    PhotoTaken { by: EntityId, track: TrackId },
    PrizeTaken { prize: EntityId, by: PlayerSlot, class: Option<ClassId> },
    Reinforcements { side: SideId, at: DVec3 },
    /// The enemy side's in-sortie alert state moved (after-action and replay; host only during the sortie).
    EnemyAlert { side: SideId, level: u8 },

    // ---- Kill cam (L1049) ----
    /// What killed a player's hull: in PvP limited to what that player's side detected plus the exterior of
    /// the vessel that fired the killing shot.
    KillCam { victim: EntityId, killer: Option<EntityId>, from_tick: u64, to_tick: u64 },
}
