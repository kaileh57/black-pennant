//! Contract-owned (no work unit). The per-viewer state the server builds at 30 Hz.
//! DESIGN.md L529 and L924 (interest management: only what the viewer's side has detected), L920 (per-vessel
//! wire content), L926 (missile state at 5 Hz), L640 (compartment water at 2 Hz as u8), L1423 (distant tracks
//! at 1 Hz, 2 Hz for weapons and fast closers), L1664-1665 (Bridge Board), L106 (plot table content).
//!
//! `Sim::snapshot(viewer)` fills every list in ascending id order. net decides what fits in a packet with its
//! priority accumulator using each item's [`Relevance`] and its own per-client history; items that miss keep
//! their priority and go first next time (L924). Lists marked "slow" change at most at the stated rate and net
//! may send them on change.
use super::alert::AlertView;
use super::command::{ControlRole, TimeRate};
use super::text::{ControlHolder, EngageStatus, Msg, RungReason, StanceReason};
use crate::ids::{ActionId, ObjectiveId, TrackId};
use content::campaign::StationAction;
use content::enums::{ControlLayer, DamageLevel, Domain, Emcon, IdRung, OfficerRole, Stance, TrackGrade};
use content::graph::RepairPriority;
use content::ids::{ClassId, CompartmentId, MunitionIdx, PartSlot, PlayerSlot, SideId};
use content::part::DecoyKind;
use glam::{DVec2, DVec3};
use serde::{Deserialize, Serialize};
use sim_core::math::Kinematics;
use sim_core::munitions::BodyPhase;
use sim_core::networks::LinkState;
use sim_core::parts::TeamOrder;
use sim_core::vessel::VesselStatus;
use sim_core::EntityId;

/// Who a snapshot or event stream is built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Viewer {
    /// A player: their side's picture plus their own hull's detail.
    Player(PlayerSlot),
    /// A whole side with no own hull (dropped player's slot, post-loss spectating in co-op Ironman, L828).
    Side(SideId),
    /// Ground truth. Only the host uses it: replay recording, trials, bots, golden captures. Never sent to a
    /// client during a PvP match (L1049).
    Omniscient,
}

/// How much a viewer cares about an item; drives net's priority and update rate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Relevance {
    /// Beyond 10 km of the viewer's hull and not a weapon: 1 Hz (L1423).
    #[default] Distant,
    /// Beyond 10 km but a weapon in flight or closing faster than 30 kn: 2 Hz.
    DistantFast,
    /// Within 10 km of the viewer's hull.
    Near,
    /// Targets, or is targeted by, an own-side unit; or an inbound weapon.
    Engaged,
    /// Own-side unit other than the viewer's hull.
    OwnSide,
    /// The viewer's own hull: every snapshot, and corrections stay under 0.5 m (L931).
    Own,
}

/// Simulation pacing every player sees in the HUD (L110-116, L1019-1029).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pacing {
    pub paused: bool,
    /// Requested rate per player slot (co-op: effective = minimum, L114).
    pub requested: Vec<(PlayerSlot, TimeRate)>,
    pub effective: TimeRate,
    /// Why compression is unavailable right now, if it is.
    pub blocked: Option<CompressionBlock>,
    /// Overload time scale, 1.0 down to 0.5 in 0.1 steps (rung 4).
    pub time_scale: f32,
    /// Overload ladder rung 0..=4 and sub-step (L1500-1517).
    pub rung: u8,
    pub sub_step: u8,
    /// Host tick time p95 over the last 2 s in milliseconds, as measured and supplied by the driver.
    pub host_tick_ms: f32,
}

/// Compression safety test results (L113): uses only player-side knowledge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompressionBlock { WeaponInbound, ThreatInOwnEnvelope, InsideThreatEnvelope, HostLoad, TimeScaleDegraded, NotAllPlayers, ModeForbids }

bitflags::bitflags! {
    /// Visible state bits of a vessel for VFX and audio.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct VesselFlags: u16 {
        const REST = 1;
        const BURNING = 1 << 1;
        const LIGHTS = 1 << 2;
        const FIRED_RECENTLY = 1 << 3;
        const KINEMATIC = 1 << 4;
        const STRUCK_COLOURS = 1 << 5;
        const TOWING_DECOY = 1 << 6;
        const SUBMERGED = 1 << 7;
        const WRECK = 1 << 8;
    }
}

/// Training and elevation of one trainable part (L920: turret angles replicate).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TurretView { pub slot: PartSlot, pub train_rad: f32, pub elevation_rad: f32 }

/// True pose of one vessel or wreck the viewer may draw: own-side hulls always, others only inside an
/// own-side unit's visual range. Static facts (blueprint, class, name) arrive once in
/// `SimEvent::EntityRevealed`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VesselView {
    pub entity: EntityId,
    pub kin: Kinematics,
    pub status: VesselStatus,
    pub flags: VesselFlags,
    pub turrets: Vec<TurretView>,
    /// Rudder -1..=1 and shaft power fraction, for wake and engine audio.
    pub rudder: f32,
    pub power: f32,
    /// Track this vessel is held as in the viewer's picture, if any (pairs the symbol with the 3D model).
    pub track: TrackId,
    pub relevance: Relevance,
}

/// Seeker state shown to experts and used for trails (L408, L523).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SeekerPhase { #[default] Off, Midcourse, Searching, Locked, Seduced, Lost }

/// A small body the viewer may draw: own-side bodies always, others inside visual range. Guided missile
/// state goes out at 5 Hz and the client interpolates (L926).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BodyView {
    pub entity: EntityId,
    pub kind: MunitionIdx,
    pub side: SideId,
    pub pos: DVec3,
    pub vel: DVec3,
    pub phase: BodyPhase,
    pub seeker: SeekerPhase,
    pub track: TrackId,
    pub relevance: Relevance,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecoyView { pub entity: EntityId, pub kind: DecoyKind, pub owner: EntityId, pub pos: DVec3, pub age_s: f32 }

/// The drawn uncertainty shape: the 95% contour (2.45 sigma in 2D). Sensor shapes are kept: a wedge for
/// bearing-only tracks, a small circle under fire-control lock (L460, L1662).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum UncertaintyShape {
    Circle { radius_m: f32 },
    Ellipse { semi_major_m: f32, semi_minor_m: f32, bearing_rad: f32 },
    /// Bearing-only: apex at the observing sensor.
    Wedge { apex: DVec2, bearing_rad: f32, half_angle_rad: f32, near_m: f32, far_m: f32 },
}

bitflags::bitflags! {
    /// Which sensor families currently feed a track.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct SourceMask: u16 {
        const SEARCH_RADAR = 1;
        const FIRE_CONTROL = 1 << 1;
        const SEEKER = 1 << 2;
        const ESM = 1 << 3;
        const PASSIVE_SONAR = 1 << 4;
        const ACTIVE_SONAR = 1 << 5;
        const VISUAL = 1 << 6;
        const CAMERA = 1 << 7;
        const DATALINK = 1 << 8;
        const OFFBOARD = 1 << 9;
        const BRIEFING = 1 << 10;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerticalTrend { #[default] Steady, Climbing, Diving }

/// One track of the viewer's side picture (L458-477, L496-503, L1661-1663).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackView {
    pub id: TrackId,
    pub domain: Domain,
    pub stance: Stance,
    pub stance_reason: StanceReason,
    pub rung: IdRung,
    pub rung_reason: RungReason,
    pub grade: TrackGrade,
    /// Estimated position and velocity (world metres). For a bearing-only track `pos` is the best range guess.
    pub pos: DVec3,
    pub vel: DVec3,
    pub shape: UncertaintyShape,
    pub last_update_tick: u64,
    /// True once dropped: the plot keeps a last-known mark for 10 minutes (L465).
    pub last_known: bool,
    /// Class when the rung is Class or Exact; the single remaining candidate otherwise `None`.
    pub class: Option<ClassId>,
    /// Remaining candidate count (the list itself comes by `Query::TrackCard`).
    pub candidates: u16,
    pub sources: SourceMask,
    pub trend: VerticalTrend,
    /// Predicted closest approach to any own-side hull is within 500 m in the next 60 s (L486).
    pub inbound: bool,
    /// Own-side mounts assigned to it.
    pub engaged_by: u8,
    /// Weapon envelope radius the plot draws for this track, metres; 0 = none (L113, L106).
    pub envelope_m: f32,
    /// Emitter radiating on this track now (plot shows enemy emitters, L106).
    pub emitting: bool,
    pub relevance: Relevance,
    /// The side has seen the hull behind it strike its colours (ruling 1: the track card shows Struck).
    #[serde(default)]
    pub struck: bool,
}

/// One line group of the Bridge Board (L351, L1664): no spinners, always an evidence-citing reason.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OfficerReport {
    pub role: Option<OfficerRole>,
    pub task: Msg,
    pub reason: Msg,
    pub next: Msg,
    pub holder: ControlHolder,
    /// Layer in use for this role (highest layer with any override, else L0).
    pub layer: ControlLayer,
    /// Count of overridden values; clicking opens a diff against default with one-click revert.
    pub overrides: u16,
    /// When the player holds the role: the officer's suggested solution, advisory only (L953).
    pub advisory: Option<Msg>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BridgeBoard {
    /// In `OfficerRole::ALL` order.
    pub officers: Vec<OfficerReport>,
    /// Emitters radiating now, ascending slot (L507).
    pub radiating: Vec<PartSlot>,
    /// "Enemy ESM has probably detected you" (L510). Player knowledge only.
    pub esm_warning: bool,
    pub emcon: Emcon,
    /// Server-side radio bookkeeping of `crate::officers::board` (unit `sys-officer-board`): per officer in
    /// `OfficerRole::ALL` order, the tick + 1 of its last radio message (0 = never), carried from board to
    /// board so `board::messages` can rate-limit without extra state; serialised so keyframes keep the limit.
    #[serde(default)]
    pub radio: [u64; 5],
}

/// Kind-specific numbers a part shows in the HUD.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum PartReadout { #[default] None, Rounds { ready: u32, magazine: u32 }, Cells { ready: u8, reload_s: f32 }, Output { kw: f32 }, Fuel { litres: f32 }, Radiating(bool), Running(bool), Closed(bool), Aboard(u8) }

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartView {
    pub slot: PartSlot,
    pub level: DamageLevel,
    /// Health 0..=255.
    pub health: u8,
    pub power: LinkState,
    pub fuel: LinkState,
    pub data: LinkState,
    pub shed: bool,
    pub priority: u8,
    pub readout: PartReadout,
}

/// Gun solution shown to experts (L408, L518).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SolutionView { pub aim_point: DVec3, pub time_of_flight_s: f32, pub iterations: u8, pub converged: bool, pub error_m: f32 }

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MountView {
    pub slot: PartSlot,
    pub status: EngageStatus,
    pub holder: ControlHolder,
    pub solution: Option<SolutionView>,
    /// Effective range in force (doctrine, L489) in metres, for the envelope ring.
    pub effective_range_m: f32,
    /// Local control after losing the Data link: own sensor, 3x error; the HUD says so (L394).
    pub local_control: bool,
}

/// Compartment state at 2 Hz (L640): fill and fire as u8 fractions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompartmentView { pub id: CompartmentId, pub fill: u8, pub fire: u8, pub breached: bool, pub valve_open: bool, pub sealed: bool }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CrewView {
    pub original_berths: u16,
    pub surviving_berths: u16,
    pub passengers: u16,
    pub passengers_lost: u16,
    /// (order, progress 0..=255, pinned by the player) per team.
    pub teams: Vec<(TeamOrder, u8, bool)>,
    pub repair_order: Vec<RepairPriority>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PowerView { pub generation_kw: f32, pub demand_kw: f32, pub supplied_kw: f32, pub fuel_l: f32, pub fuel_fraction: f32, pub fuel_flow_lps: f32 }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionState { #[default] Running, Paused, Aborted, Done }

/// Why a station action is paused or aborted (L786-797).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionBlock { TooFast, TooFar, UnderFire, TargetUnderWay, NoLineOfSight, OutsideFieldOfView, BeyondVisibility, OffLine, TooSlow, NoMinesLeft, PartLost, Cancelled, TargetLost }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionView {
    pub id: ActionId,
    pub action: StationAction,
    pub actor: EntityId,
    pub state: ActionState,
    /// 0.0..=1.0 of the hold time, or of the mine line laid.
    pub progress: f32,
    pub block: Option<ActionBlock>,
    pub at: Option<DVec2>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct OffboardView { pub slot: PartSlot, pub body: Option<EntityId>, pub aboard: u8, pub endurance_s: f32, pub recharging_s: f32 }

/// The viewer's own hull in detail. "Slow" lists: `parts` and `compartments` at 2 Hz, the rest on change.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ShipStatus {
    pub vessel: EntityId,
    pub speed_kn: f32,
    pub heading_rad: f32,
    pub heel_deg: f32,
    pub trim_deg: f32,
    /// Ordered values after officers and takeovers.
    pub ordered_speed_kn: f32,
    pub ordered_heading_rad: f32,
    /// Planned route (plot table) and the designated track.
    pub route: Vec<DVec2>,
    pub designated: TrackId,
    pub power: PowerView,
    pub parts: Vec<PartView>,
    pub mounts: Vec<MountView>,
    pub compartments: Vec<CompartmentView>,
    pub crew: CrewView,
    pub board: BridgeBoard,
    /// Roles the player holds now.
    pub held: Vec<ControlRole>,
    pub offboard: Vec<OffboardView>,
    pub actions: Vec<ActionView>,
    /// Longest own weapon envelope in metres (compression safety test and plot ring, L113).
    pub longest_envelope_m: f32,
    /// Fraction of original cells lost, 0..=1.
    pub cells_lost: f32,
}

/// Lighter view of an own-side hull that is not the viewer's (co-op partner, SEXTANT, prize under way).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AllyStatus { pub vessel: EntityId, pub player: Option<PlayerSlot>, pub speed_kn: f32, pub cells_lost: f32, pub burning: bool, pub flooding: bool, pub emcon: Emcon, pub officers_in_command: bool }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectiveState { #[default] Active, Done, Failed, Hidden }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObjectiveView {
    pub id: ObjectiveId,
    /// True for ghost conditions (optional, pay Intel or blueprints, L780).
    pub ghost: bool,
    pub state: ObjectiveState,
    pub text: Msg,
    /// 0.0..=1.0 where it makes sense (tonnage sunk, mines laid, seconds survived).
    pub progress: f32,
    pub marker: Option<DVec2>,
}

/// Everything one viewer may know at one tick.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub tick: u64,
    pub pacing: Pacing,
    /// Own-side vessels, then other vessels and wrecks inside own-side visual range; ascending entity slot.
    pub vessels: Vec<VesselView>,
    pub bodies: Vec<BodyView>,
    pub decoys: Vec<DecoyView>,
    /// The side picture, ascending track id. Includes last-known marks.
    pub tracks: Vec<TrackView>,
    /// `None` for `Viewer::Side` and while the viewer's hull is lost.
    pub ship: Option<ShipStatus>,
    pub allies: Vec<AllyStatus>,
    pub alerts: Vec<AlertView>,
    pub objectives: Vec<ObjectiveView>,
}
