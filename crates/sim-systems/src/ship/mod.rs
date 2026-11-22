//! Contract-owned types (no work unit). One vessel's logical side: its compiled design, standing orders,
//! officer and graph state. `design.rs` is unit `sys-ship-design`; `controls.rs` is unit `sys-controls`.
//! DESIGN.md L277-279 (one control graph per vessel), L301-311 (officers), L758-762 (enemy units run the same
//! pipeline as the player's ship), L953 (roles and takeover).
//!
//! The physical vessel lives in `sim_core::World`. A [`Ship`] exists for every vessel with a crew (player
//! hulls, AI hulls, prizes), not for wreck pieces. Player commands and AI captains both act by writing the
//! same [`ShipOrders`]; officers and the graph turn orders into `sim_core::parts::VesselControls`.
pub mod controls;
pub mod design;

use crate::ai::unit::CaptainState;
use crate::datalink::PartCommand;
use crate::doctrine::{DoctrineTable, MountInfo};
use crate::emcon::{EmconState, EmitterInfo};
use crate::engage::EngageState;
use crate::graph::{CompiledGraph, GraphState, SinkWrites};
use crate::guidance::gun::RangeRow;
use crate::input::ControlState;
use crate::officers::damage_control::DcState;
use crate::officers::engineer::EngineerState;
use crate::officers::helm::HelmState;
use crate::offboard::OffboardState;
use crate::prelude::*;
use crate::protocol::snapshot::BridgeBoard;
use crate::sensors::Detection;
use crate::tracks::SidePicture;
use content::blueprint::Blueprint;
use content::graph::{DoctrineEntry, GraphOverrides, OfficerSettings, Rule};
use content::part::DecoyKind;
use sim_core::vessel::{Vessel, VesselPrefab};

/// Manoeuvring numbers of a design, from `sim_core::hydro::predict_speed_turn` and the trial battery; used by
/// the Helm officer, pathing (Dubins radius) and formation keeping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnModel { pub top_speed_mps: f64, pub turn_radius_m: f64, pub accel_mps2: f64, pub stop_distance_m: f64, pub beam_m: f64, pub draft_m: f64, pub length_m: f64 }

/// Design facts the graph generator and node library read (borrowed view of a [`ShipDesign`] under
/// construction).
pub struct DesignFacts<'a> {
    pub blueprint: &'a Blueprint,
    pub prefab: &'a VesselPrefab,
    pub catalogue: &'a Catalogue,
    pub mounts: &'a [MountInfo],
    pub emitters: &'a [EmitterInfo],
    pub sensor_slots: &'a [PartSlot],
    pub consumer_slots: &'a [PartSlot],
    pub decoys: &'a [(PartSlot, DecoyKind)],
    pub pumps: &'a [PartSlot],
    pub doors: &'a [PartSlot],
}

/// Everything derived from one blueprint that the logical side needs. Built once per design save on the
/// background worker next to the `VesselPrefab` and shared by `Arc`, so Splash only swaps worlds (L218).
#[derive(Clone, Debug)]
pub struct ShipDesign {
    pub hash: ContentHash,
    pub prefab: Arc<VesselPrefab>,
    /// Stored settings, rules and overrides of the design (the "default" side of every A chip diff).
    pub settings: OfficerSettings,
    pub rules: Vec<Rule>,
    pub overrides: GraphOverrides,
    pub graph: Arc<CompiledGraph>,
    /// Weapon mounts, launchers and decoy launchers, ascending slot.
    pub mounts: Vec<MountInfo>,
    pub emitters: Vec<EmitterInfo>,
    pub sensor_slots: Vec<PartSlot>,
    pub consumer_slots: Vec<PartSlot>,
    pub decoys: Vec<(PartSlot, DecoyKind)>,
    pub pumps: Vec<PartSlot>,
    pub doors: Vec<PartSlot>,
    /// Offboard bays (drones; helicopter deck from Act 2).
    pub bays: Vec<PartSlot>,
    /// Ballistic solver table per gun mount, same order as the gun entries of `mounts` (auto-set, L208).
    pub range_tables: BTreeMap<PartSlot, Arc<Vec<RangeRow>>>,
    pub turn: TurnModel,
    /// Build cost in Salvage and tonnage, for payouts and commitment checks.
    pub cost: u32,
    pub tonnage_t: f64,
    /// The blueprint the design was built from (`recompile` needs it for `DesignFacts`). Added by
    /// `sys-ship-design` (additive; read it through `ShipDesign::blueprint`).
    pub(crate) blueprint: Arc<Blueprint>,
    /// Ruleset layer and Luau switch `graph` was compiled under; a design reused under other rules is
    /// recompiled. Added by `sys-ship-design`.
    pub(crate) compiled_under: (content::ruleset::RulesetKind, bool),
    /// Auto-set mount limits from the ray sweep with their blocked fraction, for mounts without hand-set
    /// limits; reused by a rebuild whose geometry is unchanged. Added by `sys-ship-design`.
    pub(crate) auto_limits: BTreeMap<PartSlot, (content::graph::MountLimits, f64)>,
}

/// Standing orders of one ship: what the player's clicks or an AI captain want. The graph reads them through
/// the `TheatreOrders` source (L344). Persist until replaced.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ShipOrders {
    /// Planned route in world (x, z) metres; empty = hold heading.
    pub route: Vec<DVec2>,
    /// Ordered speed in knots; `None` = cruise.
    pub speed_kn: Option<f64>,
    /// Hold station at a point with an optional heading (formation slot, berth, station action hold).
    pub station: Option<(DVec2, Option<f64>)>,
    /// Keep this offset from a leader (formation keeping; AI groups, prize following home).
    pub formation: Option<FormationSlot>,
    /// Track the player or captain designated; officers handle the rest (L108).
    pub designated: TrackId,
    /// Weapons hold: officers track and solve but do not fire except point defence.
    pub weapons_tight: bool,
    /// Evasion pattern requested (zigzag, comb a torpedo track).
    pub evade: bool,
    /// Engagement range the captain wants to keep, metres (doctrine-set, L762).
    pub keep_range_m: Option<f64>,
    /// Act 3 AI submarines: ordered depth in metres.
    pub depth_m: Option<f64>,
    /// Manual launch requests not yet executed.
    pub launches: Vec<LaunchRequest>,
    /// Decoy launchers to fire once.
    pub decoys: Vec<PartSlot>,
    /// An AI captain has this hull lie still (at anchor, struck, or stopped by its tree): the Helm's own
    /// evasion stage does not get it under way for an inbound weapon; ordered evasion (`evade`) still does.
    /// Player orders never set it, so a player's hull at all stop still beams a missile. DECISION (r4, from
    /// s2): the anchored prize in cutting_out seed 0 had orders of 0 kn and no route yet made 21.5 kn from 30
    /// to 90 s, beaming missiles on the `helm.evasion` slider's default.
    #[serde(default)]
    pub at_rest: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FormationSlot { pub leader: EntityId, pub offset_m: DVec2 }

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LaunchRequest { pub slot: PartSlot, pub track: TrackId, pub bearing_rad: Option<f64>, pub depth_m: Option<f64> }

/// How well this ship's gunners and reactions perform (difficulty sliders for AI hulls; 1.0 and 0 for stock
/// player officers, L851-852).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Proficiency { pub aim_error_mult: f64, pub reaction_s: f64, pub veterancy: f64 }
impl Default for Proficiency { fn default() -> Self { Self { aim_error_mult: 1.0, reaction_s: 0.0, veterancy: 0.0 } } }

/// Officer stage memory of one ship. Each field is owned by the unit named in its module.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StageStates { pub emcon: EmconState, pub engage: EngageState, pub helm: HelmState, pub engineer: EngineerState, pub dc: DcState,
    /// Held results of the 10 Hz stages and per-launcher edge memory (`crate::officers::stages`, unit
    /// `sys-officer-stages`). Added by `sys-officer-stages` (additive, serde default).
    #[serde(default)]
    pub hold: crate::officers::stages::StageHold }

pub struct Ship {
    pub vessel: EntityId,
    pub side: SideId,
    pub player: Option<PlayerSlot>,
    pub design: Arc<ShipDesign>,
    /// Live settings and rules (start as the design's; L0-L2 commands change them in a sortie).
    pub settings: OfficerSettings,
    pub rules: Vec<Rule>,
    /// Compiled graph in force (the design's, or recompiled after a rules change or a splash edit).
    pub graph: Arc<CompiledGraph>,
    pub graph_state: GraphState,
    /// Fleet-level doctrine entries (AI groups, doctrine cards); empty for player hulls.
    pub fleet_doctrine: Vec<DoctrineEntry>,
    pub doctrine: DoctrineTable,
    pub orders: ShipOrders,
    pub control: ControlState,
    pub stages: StageStates,
    pub offboard: OffboardState,
    pub proficiency: Proficiency,
    /// AI captain; `None` for a hull a connected player commands.
    pub captain: Option<CaptainState>,
    /// Sinks written by the last graph tick.
    pub sinks: SinkWrites,
    /// Command mode per part (index = slot), refreshed each tick from link state.
    pub part_command: Vec<PartCommand>,
    pub board: BridgeBoard,
    /// This ship's own detections of the last second, ascending (sensor, tick): the "raw detections per
    /// sensor" source and local-control input.
    pub detections: Vec<Detection>,
    /// Passengers aboard and the tag objectives refer to this hull by.
    pub tag: String,
}

/// Every ship in the sortie, ascending vessel entity.
#[derive(Default)]
pub struct ShipTable { pub ships: Vec<Ship> }
impl ShipTable {
    pub fn get(&self, vessel: EntityId) -> Option<&Ship> { self.ships.binary_search_by(|s| s.vessel.cmp(&vessel)).ok().map(|i| &self.ships[i]) }
    pub fn get_mut(&mut self, vessel: EntityId) -> Option<&mut Ship> { match self.ships.binary_search_by(|s| s.vessel.cmp(&vessel)) { Ok(i) => Some(&mut self.ships[i]), Err(_) => None } }
    pub fn of_player(&self, player: PlayerSlot) -> Option<&Ship> { self.ships.iter().find(|s| s.player == Some(player)) }
    pub fn of_player_mut(&mut self, player: PlayerSlot) -> Option<&mut Ship> { self.ships.iter_mut().find(|s| s.player == Some(player)) }
    /// Insert keeping ascending order. Real code.
    pub fn insert(&mut self, ship: Ship) { let i = self.ships.partition_point(|s| s.vessel < ship.vessel); self.ships.insert(i, ship); }
    pub fn remove(&mut self, vessel: EntityId) -> Option<Ship> { self.ships.binary_search_by(|s| s.vessel.cmp(&vessel)).ok().map(|i| self.ships.remove(i)) }
}

/// Read-only bundle a ship's sources and stages see during its graph tick. Per-vessel graph ticks run in
/// parallel on the sim pool over shared immutable views and merge serially in ascending vessel slot.
#[derive(Clone, Copy)]
pub struct ShipView<'a> {
    pub ctx: &'a SimCtx<'a>,
    pub world: &'a World,
    pub vessel: &'a Vessel,
    pub design: &'a ShipDesign,
    pub settings: &'a OfficerSettings,
    pub orders: &'a ShipOrders,
    pub doctrine: &'a DoctrineTable,
    /// The side picture this ship reads over the datalink (empty slice view when isolated).
    pub picture: &'a SidePicture,
    pub detections: &'a [Detection],
    pub part_command: &'a [PartCommand],
    pub control: &'a ControlState,
    /// Positions and velocities of own-side hulls (inbound test).
    pub own_side_hulls: &'a [(DVec3, DVec3)],
    pub esm_sites: &'a [[f64; 2]],
    pub proficiency: Proficiency,
}
