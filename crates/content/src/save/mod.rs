//! Contract-owned types plus unit `content-save` (the functions at the bottom). Save-game and suspend formats.
//! DESIGN.md L112 (suspend), L826-828 (failure and saving), L832-845 (co-op copies, checkpoints), L756 (hull
//! records), L749-750, L772 (ledger state).
//!
//! The persistent theatre state is plain data here. `sim_systems::theatre::Theatre` wraps a [`TheatreState`]
//! with behaviour; it must not keep persistent state outside it. A sortie keyframe is opaque versioned bytes
//! produced and consumed by `sim_systems::Sim`.
use crate::blueprint::Blueprint;
use crate::campaign::{DifficultySliders, KillMethod, TallyKey};
use crate::cell::CellChange;
use crate::enums::Act;
use crate::error::ContentResult;
use crate::ids::{AceId, CardId, ClassId, ContentHash, OperationId, PartId, PartSlot, RegionId, TheatreNodeId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

mod codec;
#[cfg(test)]
mod tests;

pub const SAVE_FORMAT: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SaveHeader {
    pub format: u32,
    pub game_version: String,
    /// Tuning table version the save was last played with.
    pub tuning_version: u32,
    /// Sequence number and hash of the previous save. On disagreement the highest sequence wins and a forked
    /// copy is refused with an explanation. Loading a suspend slot advances the sequence (L845).
    pub sequence: u64,
    pub prev_hash: ContentHash,
    /// Campaign id, random at creation; ties co-op members' copies together.
    pub campaign_id: u64,
    pub slot_name: String,
    pub ironman: bool,
    /// Wall-clock seconds played, excluding pauses over 2 minutes (L79).
    pub played_s: u64,
}

/// Shared pool on SEXTANT (L811-822, L841).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Resources {
    pub salvage: u32,
    pub intel: u32,
    pub fuel_l: f64,
    /// Ammunition stock in kilograms by calibre in mm, and whole rounds by munition for missiles, torpedoes, mines.
    pub ammo_kg: BTreeMap<u16, f64>,
    pub munitions: BTreeMap<crate::ids::MunitionId, u16>,
    /// Parts unlocked for good by blueprints (within open tiers).
    pub unlocked_parts: BTreeSet<PartId>,
    /// SEXTANT parts store: removed parts refit later for free; never sold (L805).
    pub parts_store: BTreeMap<PartId, u16>,
}

/// Persistent damage a hull carries between operations until repaired at the tender.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HullDamage {
    pub cells: Vec<CellChange>,
    /// Part health 0.0..=1.0 for damaged parts.
    pub parts: BTreeMap<PartSlot, f64>,
    /// Conduits left jury-rigged at 50% (L396): count only; the router re-derives which.
    pub jury_rigged: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerRecord {
    /// Random 64-bit per-install client id (L1003).
    pub client_id: u64,
    pub name: String,
    /// The player's one combat hull. A lost hull keeps its design and loses its fitted parts (L826).
    pub blueprint: Blueprint,
    pub damage: HullDamage,
    pub fuel_fraction: f64,
    /// Rounds remaining per magazine or launcher slot.
    pub ammo: BTreeMap<PartSlot, u32>,
    pub hull_lost: bool,
    /// Theatre hour the rebuild or refit completes; `None` = ready.
    pub ready_at_hour: Option<u64>,
    /// 0.0..=0.15 (L811-822). Gain rule pending owner decision.
    pub veterancy: f64,
}

/// One persistent Combine hull record (about 300 on Normal, L756).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HullRecord {
    pub id: u32,
    pub class: ClassId,
    /// Class variant in use (co-op scaling, doctrine upgrades); `None` = base class.
    pub variant: Option<ClassId>,
    pub region: RegionId,
    pub node: TheatreNodeId,
    /// Task group the hull belongs to, if any.
    pub group: Option<u32>,
    pub fuel: f64,
    pub ammunition: f64,
    /// Fraction of hull health lost, 0.0..=1.0; crippled groups return weakened.
    pub damage: f64,
    pub veterancy: f64,
    pub refit_until_hour: Option<u64>,
    pub alive: bool,
    pub ace: Option<AceId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupOrder { Garrison, Patrol, Escort, Search, Intercept, HuntSextant, Return, Refit }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskGroupState {
    pub id: u32,
    pub hulls: Vec<u32>,
    pub node: TheatreNodeId,
    pub destination: Option<TheatreNodeId>,
    pub order: GroupOrder,
    pub arrive_hour: Option<u64>,
    /// Commander's belief map of LANTERN's position: probability per theatre node, same order as
    /// `TheatreDef::nodes` (L760). Players may view it after a mission.
    pub belief: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConvoyState { pub id: u32, pub hulls: Vec<u32>, pub from: TheatreNodeId, pub to: TheatreNodeId, pub depart_hour: u64, pub arrive_hour: u64, pub cargo: f64, pub decoy: bool, pub revealed: bool }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QueuedCard { pub card: CardId, pub arrives_hour: u64 }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RegionState {
    /// Real 0..=5; displayed and compared as floor (L749).
    pub alert: f64,
    pub quiet_hours: u32,
    /// Supply 0..=100 (L747).
    pub supply: f64,
    /// Delivered and demanded supply per theatre day over the window, oldest first.
    pub delivered: Vec<f64>,
    pub demanded: Vec<f64>,
    pub active_cards: Vec<CardId>,
    pub queued_cards: Vec<QueuedCard>,
    /// Kills not yet noticed: (hour the loss is noticed, alert to add), L749.
    pub pending_alert: Vec<(u64, f64)>,
    pub combine_control: bool,
    pub convoys_revealed_until_hour: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LaneState { pub cut_until_hour: Option<u64>, pub mines: u16 }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpsLedger {
    pub completed: Vec<OperationId>,
    pub failed: Vec<OperationId>,
    /// Times each template was offered this act (cap 3, never twice in a row, L782).
    pub offered: BTreeMap<OperationId, u8>,
    pub last_offered: Option<OperationId>,
    pub aces_beaten: Vec<AceId>,
    pub aces_revealed: Vec<AceId>,
    /// Win-method tallies shown in the after-action report (L70).
    pub kills_by_method: BTreeMap<KillMethod, u32>,
}

/// A chart-table proposal in co-op: commits when all accept or the host confirms (L71, L839).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TheatreAction { Transit(TheatreNodeId), Refit, Wait { hours: u32 }, AcceptOperation(OperationId), DeclineOperation(OperationId), SpendIntel(IntelSpend), RebuildHull { player: u8 } }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum IntelSpend { DelayCard { region: RegionId, card: CardId }, CancelCard { region: RegionId, card: CardId }, RevealConvoys(RegionId), RevealAce(AceId) }

/// The whole persistent theatre ledger.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TheatreState {
    pub seed: u64,
    pub act: Act,
    /// Theatre hour since campaign start; time advances in 1-hour steps (L741).
    pub hour: u64,
    /// Hours left on the act's operation clock; `None` in Open Theatre (L750, L875).
    pub clock_hours_left: Option<f64>,
    pub retreat: bool,
    pub open_theatre: bool,
    pub difficulty: DifficultySliders,
    pub resources: Resources,
    pub players: Vec<PlayerRecord>,
    pub sextant_node: TheatreNodeId,
    pub sextant_refit_until_hour: Option<u64>,
    pub open_coves: Vec<TheatreNodeId>,
    pub regions: BTreeMap<RegionId, RegionState>,
    /// Lane state keyed by (a, b) with a < b.
    pub lanes: BTreeMap<(TheatreNodeId, TheatreNodeId), LaneState>,
    pub depots: BTreeMap<TheatreNodeId, f64>,
    pub hulls: Vec<HullRecord>,
    pub groups: Vec<TaskGroupState>,
    pub convoys: Vec<ConvoyState>,
    /// The four decaying tallies (L772).
    pub tallies: BTreeMap<TallyKey, f64>,
    pub ops: OpsLedger,
    /// Operations on offer at the chart table now.
    pub offered: Vec<OperationId>,
    pub hunter_group: Option<u32>,
    pub last_admiral_plan_hour: u64,
    /// Highest degradation rung the host reached last operation (L837).
    pub last_host_rung: u8,
    pub next_id: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SaveGame { pub header: SaveHeader, pub theatre: TheatreState }

/// The 60 s operation checkpoint: full keyframe plus theatre state (L843).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SortieCheckpoint {
    pub tick: u64,
    /// Version of the keyframe encoding (`sim_systems` protocol version).
    pub keyframe_format: u32,
    /// Opaque `SortieConfig` bytes, so a re-host can rebuild the Sim before applying the keyframe.
    pub config: Vec<u8>,
    /// Opaque full keyframe snapshot of the Sim.
    pub keyframe: Vec<u8>,
}

/// The single suspend slot; loading it deletes it (L112, L828).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SuspendSave { pub header: SaveHeader, pub theatre: TheatreState, pub sortie: SortieCheckpoint }

/// Serialise a save: RON text (human-diffable) inside one compressed frame. Unit: `content-save`.
pub fn encode_save(save: &SaveGame) -> Vec<u8> { codec::encode_save(save) }
/// Parse a save with migrations; refuses newer formats naming the release needed. Unit: `content-save`.
pub fn decode_save(bytes: &[u8]) -> ContentResult<SaveGame> { codec::decode_save(bytes) }
pub fn encode_suspend(save: &SuspendSave) -> Vec<u8> { codec::encode_suspend(save) }
pub fn decode_suspend(bytes: &[u8]) -> ContentResult<SuspendSave> { codec::decode_suspend(bytes) }
/// Hash that the next save stores as `prev_hash`. Unit: `content-save`.
pub fn save_hash(save: &SaveGame) -> ContentHash { codec::save_hash(save) }
/// Decide between two copies of one campaign: highest sequence wins; a copy whose chain does not contain the
/// other's hash is a fork and is refused (L845). Unit: `content-save`.
pub fn reconcile(mine: &SaveHeader, mine_hash: ContentHash, theirs: &SaveHeader, theirs_hash: ContentHash) -> Reconcile { codec::reconcile(mine, mine_hash, theirs, theirs_hash) }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconcile { Same, TakeMine, TakeTheirs, Forked }
