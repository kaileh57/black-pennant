//! Contract-owned (no work unit). Client to server: discrete reliable [`Command`]s and the 30 Hz
//! [`PlayerInput`] state. DESIGN.md L104-116 (orders, takeover, time rules), L949-953 (manual shots, role
//! control), L494-514 (overrides), L396 and L648 (damage-control commands), L786-797 (station actions),
//! L1687 (alert commands), L216 (splash), L1422 (inputs at 30 Hz, each packet repeats the last 4 ticks).
//!
//! Each vehicle has exactly one player (L953), so commands address the sender's own hull implicitly.
//! `Sim::apply` sorts a tick's commands by (player slot, seq) before applying; a command the mode or ruleset
//! forbids yields `SimEvent::CommandRejected`, never a panic.
use crate::ids::{ActionId, AlertId, TrackId};
use content::blueprint::Blueprint;
use content::campaign::StationAction;
use content::enums::{DoctrinePreset, Emcon, IdRung, Stance};
use content::graph::{DoctrineEntry, PortRef, RepairPriority, Rule, SinkKind};
use content::ids::{ClassId, CompartmentId, PartSlot, PlayerSlot};
use glam::{DVec2, DVec3};
use serde::{Deserialize, Serialize};
use sim_core::parts::TeamOrder;
use sim_core::EntityId;

/// Time compression rates (L113).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TimeRate { #[default] X1, X2, X4 }
impl TimeRate { pub fn factor(self) -> u32 { match self { Self::X1 => 1, Self::X2 => 2, Self::X4 => 4 } } }

/// A role the player can take from its officer (L953, L1665). The officer turns advisory while held and
/// resumes within one graph tick of release.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ControlRole { Helm, Sensors, Mount(PartSlot), WeaponGroup(u8) }

/// A manual shot is a designation, not a world position (L949): the server fires at its own tick and converts
/// a track designation through its current track estimate. Stamped with the client render tick in the
/// envelope. Direction-only shots are corrected by extrapolating the aimed-at target from that tick.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ShotDesignation {
    Track { track: TrackId, offset_m: DVec3 },
    /// Unit direction in the vessel-local frame (+X bow, +Y up, +Z starboard).
    Direction { dir_local: DVec3 },
}

/// What a station action is aimed at (L786-797).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ActionTarget { None, Track(TrackId), Marker(String), Line { a: DVec2, b: DVec2 } }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrizeDecision { SailHome, StripOnSpot }

/// Orders to an offboard unit (drone at first release, helicopter from Act 2).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum OffboardOrder { Launch, Recall, Orbit { centre: DVec2, radius_m: f64 }, Shadow(TrackId), Waypoint(DVec2) }

/// A read request; the answer comes back as `SimEvent::QueryReply` to the asking player only.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Query {
    /// "Why?" on a part: the chain of causes from graph state (L353).
    Why(PartSlot),
    /// Scope on a graph port during a splash: stream its value until cancelled (L216).
    Scope { port: PortRef, on: bool },
    /// Full candidate list and evidence of one track (track card, L477).
    TrackCard(TrackId),
    /// Diff of overridden values against defaults for one officer (L351).
    Overrides(content::enums::OfficerRole),
}

/// A value the player may override and revert with one click (A chip to hand glyph, L351).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum OverrideKey { Preset, MountPreset(PartSlot), Slider(String), Emcon, BurstTriggerRange, SelfDefenceRange, Doctrine { mount: Option<PartSlot>, index: u8 }, PowerPriority(PartSlot), RepairOrder, Sink(SinkKind), TrackStance(TrackId), TrackIdentity(TrackId) }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Command {
    // ---- Helm and orders (L108) ----
    /// Right-click on the sea: set course. `append` adds to the planned route.
    SetCourse { waypoints: Vec<DVec2>, speed_kn: Option<f64>, append: bool },
    SetSpeed { speed_kn: f64 },
    HoldStation { at: DVec2, heading_rad: Option<f64> },
    /// Click a track: designate. Officers handle the rest. `None` clears.
    Designate { track: Option<TrackId> },
    /// Take or release a role (hold key, or toggle after a press under 250 ms; the client resolves which).
    TakeControl { role: ControlRole, on: bool },
    /// Manual shot from a held mount or weapon group.
    Fire { role: ControlRole, shot: ShotDesignation },
    /// Launch a guided weapon at a track by hand (missile launchers, torpedo tubes, depth-charge racks).
    Launch { slot: PartSlot, track: Option<TrackId>, bearing_rad: Option<f64>, depth_m: Option<f64> },
    FireDecoy { slot: PartSlot },
    // ---- Sensors and tracks (L503, L507) ----
    SetEmcon(Emcon),
    /// Manual stance override per track; `None` reverts to the rules (never lets Friend be engaged, L500).
    SetStance { track: TrackId, stance: Option<Stance> },
    /// Manual identification override per track; `None` reverts.
    SetIdentity { track: TrackId, rung: Option<IdRung>, class: Option<ClassId> },
    // ---- Control ladder L0 to L2 (allowed in a sortie; L3 and L4 edits only in dry dock or a splash) ----
    SetPreset { mount: Option<PartSlot>, preset: DoctrinePreset },
    SetSlider { key: String, value: f64 },
    SetDoctrine { mount: Option<PartSlot>, entry: DoctrineEntry },
    SetRules(Vec<Rule>),
    SetPowerPriority { slot: PartSlot, priority: u8 },
    Revert(OverrideKey),
    RevertAll(content::enums::OfficerRole),
    // ---- Damage control (L396, L639, L648, L667, L716) ----
    ReorderRepairs(Vec<RepairPriority>),
    /// Pin a team to a job, or `None` to hand it back to the officer.
    AssignTeam { team: u8, order: Option<TeamOrder> },
    Pump { slot: PartSlot, running: bool, target: Option<CompartmentId> },
    Door { slot: PartSlot, closed: bool },
    FloodValve { compartment: CompartmentId, open: bool },
    FloodMagazine { slot: PartSlot },
    // ---- Offboard ----
    Offboard { slot: PartSlot, order: OffboardOrder },
    // ---- Special operations (L786-797) ----
    StartAction { action: StationAction, target: ActionTarget },
    CancelAction(ActionId),
    Prize { prize: EntityId, decision: PrizeDecision },
    // ---- Alerts (L1687): only the player acknowledges ----
    /// Acknowledge the highest-priority, oldest alert, or a named one. There is no acknowledge-all.
    Acknowledge(Option<AlertId>),
    /// Silence all sounds for 30 s.
    SilenceAlerts,
    // ---- Time (L110-116) ----
    /// Single-player only. Orders given while paused apply on resume.
    Pause(bool),
    /// Requested compression rate; the effective rate is the minimum over players, subject to the safety test.
    RequestTimeRate(TimeRate),
    // ---- Splash and tutorial basins (L216) ----
    /// Rewind up to 30 s (1,800 ticks).
    SplashRewind { ticks: u32 },
    SplashRestart,
    /// Live edit of kits, tuning, rules, graphs or scripts while the hull shell is unchanged; a changed
    /// shell returns the client to dry dock instead.
    SplashApplyDesign(Box<Blueprint>),
    /// Re-arm a greyed-out script (dry dock or splash only, L371).
    RearmScripts,
    // ---- Queries ----
    Query(Query),
    /// Withdraw from the sortie area (campaign: ends the sortie as withdrawn once clear of contact).
    Withdraw,
}

/// A command with its sender and ordering stamps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub player: PlayerSlot,
    /// Per-player sequence number, ascending; duplicates are dropped.
    pub seq: u32,
    /// Server tick the client was rendering when it issued the command (interpolated past, L949).
    pub client_render_tick: u64,
    /// Round-trip time the server measured for this client, milliseconds; goes into the shot log (L951).
    pub rtt_ms: u16,
    pub command: Command,
}

/// Continuous input sampled at 30 Hz; the latest one per player stays in force until replaced. Only read for
/// roles the player holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlayerInput {
    /// Client input tick (ascending); older inputs are ignored.
    pub input_tick: u64,
    pub client_render_tick: u64,
    /// Helm takeover: -1.0 (full astern) ..= 1.0 and -1.0 (port) ..= 1.0.
    pub throttle: f32,
    pub rudder: f32,
    /// Aim direction for the held mount or group, vessel-local unit vector; zero = none.
    pub aim_local: [f32; 3],
    /// Trigger held: fire continuously with the current aim (autocannon, point defence).
    pub trigger: bool,
    /// Camera position in world metres, for relevance only (never for visibility).
    pub camera_pos: [f64; 3],
}
