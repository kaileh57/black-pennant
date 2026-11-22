//! Contract-owned (no work unit). Sortie configuration, the side and player model, static entity facts and the
//! sortie result. DESIGN.md L92-102 (commitment), L496-503 (stance sources), L778-797 (operation parts),
//! L957-971 (modes), L832-845 (co-op), L749 and L772 (what the theatre needs back), L811-822 (payouts).
//!
//! [`SortieConfig`] is plain serde data: it is stored inside `content::save::SortieCheckpoint::config` and sent
//! to a re-hosting member (L843). It never holds prefabs or other derived data; `Sim::new` builds those.
//! [`SortieResult`] is the only channel from a sortie into `crate::theatre::Theatre`.
use crate::ids::{GroupId, ObjectiveId, SideMask, TrackId};
use content::blueprint::Blueprint;
use content::campaign::{CardEffect, ConstraintTag, FleetKind, Formation, GhostCondition, KillMethod, MapDef, Objective, RangeBand, StationAction};
use content::cell::CellChange;
use content::enums::{Act, Emcon, GameMode, Stance, TimeOfDay};
use content::ids::{AceId, CardId, ClassId, ContentHash, MunitionId, OperationId, PartId, PartSlot, PlayerSlot, RegionId, SideId, TextKey};
use content::save::HullDamage;
use content::settings::{SimSettings, SplashOptions};
use content::weather::WeatherState;
use glam::{DVec2, DVec3};
use serde::{Deserialize, Serialize};
use sim_core::vessel::VesselStatus;
use sim_core::EntityId;
use std::collections::BTreeMap;

/// Who a side is; picks default behaviour and stance rules (L496-503).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Faction {
    /// LANTERN and co-op partners, or a PvP team.
    #[default] Players,
    /// Meridian Combine: AI, hostile to Players.
    Combine,
    /// Civilian traffic: AI, never fires, starts Neutral when the briefing declares it (L502).
    Civilian,
    /// Defectors and escorted ships: Neutral until rendezvous, then join the player side (L797).
    Protected,
    /// Splash and trial targets (drones, the patrol boat that fights back).
    Target,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SideDef {
    pub id: SideId,
    pub faction: Faction,
    pub name: TextKey,
    /// Sides this side's AI treats as enemies (truth for AI rules of engagement and for "fires on your side").
    pub hostile_to: SideMask,
    /// Sides whose units share this side's datalink picture and are Friend (L500). Always includes itself.
    pub allied: SideMask,
    /// PvP team index (0 or 1) when the lobby placed this side opposite another (L501).
    pub pvp_team: Option<u8>,
}

/// Where a vessel's design comes from. Stock designs are in every build; player designs travel inline.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BlueprintRef {
    /// Path under `blueprints/` (`content::campaign::load::stock_blueprint`).
    Stock(String),
    /// Hash of an entry in [`SortieConfig::blueprints`].
    Inline(ContentHash),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerInit {
    pub slot: PlayerSlot,
    /// Random 64-bit per-install id; holds a dropped player's slot for 5 minutes (L1003, L838).
    pub client_id: u64,
    pub name: String,
    pub side: SideId,
    /// Index into [`SortieConfig::vessels`] of this player's one hull; `None` = spectator.
    pub vessel: Option<u16>,
    /// False for a bot or an empty slot whose hull the officers run (L838).
    pub human: bool,
}

/// One vessel to spawn. Order in [`SortieConfig::vessels`] is spawn order and therefore entity slot order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VesselInit {
    pub blueprint: BlueprintRef,
    pub side: SideId,
    pub class: Option<ClassId>,
    pub group: Option<GroupId>,
    /// Position on the sea surface (y ignored) and compass heading.
    pub pos: DVec3,
    pub heading_rad: f64,
    pub speed_kn: f64,
    /// Seconds after sortie start; 0 = present at start. Scheduled reinforcements count as committed (L94).
    pub arrives_s: u32,
    pub damage: Option<HullDamage>,
    pub fuel_fraction: f64,
    pub ammo_fraction: f64,
    /// Rounds per magazine or launcher slot carried in from the save; overrides `ammo_fraction` where present.
    pub ammo: BTreeMap<PartSlot, u32>,
    /// Persistent Combine hull record this hull is (`content::save::HullRecord::id`), so results map back.
    pub hull_record: Option<u32>,
    pub veterancy: f64,
    /// Tag objectives and force entries name (`"convoy"`, `"defector"`, `"prize"`).
    pub tag: String,
    /// Stance the briefing declares for it, if any (L501-502). `Exact` naming is in `SortieIntel::named`.
    pub declared: Option<Stance>,
    pub emcon: Option<Emcon>,
    pub passengers: u16,
    /// Tender, depot ship or other hull whose loss has theatre consequences (SEXTANT, L827).
    pub role: VesselRole,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VesselRole { #[default] Combatant, PlayerHull, Tender, Convoy, Site, Prize, Target }

/// A free small body present at start or arriving later: pre-laid mines, enemy aircraft (Act 2), survivors.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BodyInit { pub munition: MunitionId, pub side: SideId, pub group: Option<GroupId>, pub pos: DVec3, pub vel: DVec3, pub arrives_s: u32 }

/// Standing order of an AI group at sortie start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GroupOrders { Patrol { markers: Vec<String> }, Transit { from: String, to: String }, Guard { marker: String, radius_m: f64 }, Escort { tag: String }, Hunt, Anchor, AirPatrol { marker: String, radius_m: f64 }, Flee { exit: String },
    /// Commander tasking (not used in briefing data): search the water within `radius_m` of `at`, world (x, z)
    /// metres, a peak of the side's belief map (L760). Issued by `ai::commander::update`.
    Search { at: DVec2, radius_m: f64 } }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GroupInit {
    pub id: GroupId,
    pub side: SideId,
    pub kind: FleetKind,
    pub formation: Formation,
    pub orders: GroupOrders,
    /// Ace leading the group, if any (L774); loads the ace's lines and doctrine.
    pub ace: Option<AceId>,
    /// Persistent task group id in the theatre ledger, so belief and losses map back.
    pub theatre_group: Option<u32>,
    pub tag: String,
}

/// What the briefing lets the player side know before contact (L470-475, L501-502, L510).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SortieIntel {
    /// Theatre order of battle and civilian traffic list: the identification candidate pool (L470).
    pub order_of_battle: Vec<ClassId>,
    pub civilian: Vec<ClassId>,
    /// Classes the briefing marks hostile (L501).
    pub declared_hostile: Vec<ClassId>,
    /// Vessel indices the briefing names exactly (ID rung Exact on first association, L475).
    pub named: Vec<u16>,
    /// Briefing-declared ESM sites in world metres (L510).
    pub esm_sites: Vec<[f64; 2]>,
    pub recommended_emcon: Option<Emcon>,
}

/// Extra rules of even-sided modes (L957-971).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ModeRules {
    /// Campaign, skirmish, tutorial: the objective decides.
    Objective,
    /// Free play, join any time, no end condition.
    Sandbox,
    /// Last team afloat.
    Elimination,
    /// 1.0 item: `convoy_side` runs a convoy from `entry` to `exit` across a 20 to 40 km map (Phase::R100).
    Escort { convoy_side: SideId, entry: String, exit: String, required_fraction: f64 },
    /// Test basin: never ends by itself.
    Splash,
    /// Headless trial of fixed length in ticks.
    Trial { ticks: u64 },
}

/// Everything needed to start one sortie.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SortieConfig {
    pub protocol: u32,
    pub seed: u64,
    pub mode: GameMode,
    pub rules: ModeRules,
    pub settings: SimSettings,
    pub act: Act,
    pub weather: WeatherState,
    /// Scheduled weather turns: (seconds after start, new state).
    pub weather_changes: Vec<(u32, WeatherState)>,
    pub time_of_day: TimeOfDay,
    pub map: MapDef,
    /// Sonar layer depth of the region in metres (30..=80, L455).
    pub layer_depth_m: f64,
    pub sides: Vec<SideDef>,
    pub players: Vec<PlayerInit>,
    /// Inline designs (player hulls; exterior forms are derived by the server).
    pub blueprints: Vec<Blueprint>,
    pub vessels: Vec<VesselInit>,
    pub bodies: Vec<BodyInit>,
    pub groups: Vec<GroupInit>,
    pub objective: Objective,
    pub ghosts: Vec<GhostCondition>,
    pub tags: Vec<ConstraintTag>,
    pub actions: Vec<StationAction>,
    pub intel: SortieIntel,
    /// Active doctrine cards of the region with their effects resolved (L772).
    pub cards: Vec<(CardId, CardEffect)>,
    pub operation: Option<OperationId>,
    pub region: Option<RegionId>,
    pub theatre_hour: u64,
    pub splash: Option<SplashOptions>,
    /// Tutorial script key (`crate::tutorial`), if this basin runs one.
    pub tutorial: Option<String>,
    /// Hard time limit in seconds; `None` = none. The proxy gate fails any run over 45 simulated minutes (L81).
    pub time_limit_s: Option<u32>,
}

/// Static facts about an entity, sent once when it enters a side's drawable set (and to the replay).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntityIntro {
    pub entity: EntityId,
    pub side: SideId,
    /// Design to draw: the full blueprint hash for own-side hulls; in PvP the exterior-only form's hash for
    /// the other team (L945). Stock classes resolve locally; others stream on the blueprint lane.
    pub blueprint: ContentHash,
    pub exterior_only: bool,
    pub class: Option<ClassId>,
    pub name: String,
    pub player: Option<PlayerSlot>,
    pub status: VesselStatus,
    /// Damage accumulated before the reveal, in application order.
    pub damage: Vec<CellChange>,
    /// Cells already split off or shed (the client removes them before drawing).
    pub removed: Vec<content::ids::CellIndex>,
    /// For wreck pieces: the vessel they split from.
    pub parent: Option<EntityId>,
    pub track: TrackId,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome { Victory, Defeat, #[default] Withdrawn, /// Suspended or host quit: no result applies.
    Aborted, /// PvP and skirmish draws, sandbox end.
    Draw }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KillFate { Lost, Disabled, Captured }

/// One enemy hull taken out of the fight. The theatre derives Alert, doctrine tallies and Salvage from these
/// (L749, L772, L811-822).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KillRecord {
    pub tick: u64,
    pub hull_record: Option<u32>,
    pub class: Option<ClassId>,
    pub side: SideId,
    pub tonnage_t: f64,
    pub build_cost: u32,
    pub fate: KillFate,
    /// From the per-vessel last-damage ledger (`crate::objectives::DamageLedger`).
    pub method: KillMethod,
    pub range: RangeBand,
    pub time: TimeOfDay,
    /// Killer's emission state at the kill.
    pub emcon: Emcon,
    /// A surviving Combine unit observed or reported it (+1.0 Alert) or not (+0.5 after 12 h), L749.
    pub observed: bool,
    pub by_player: Option<PlayerSlot>,
    /// True when the wreck stayed afloat or grounded where it can be salvaged.
    pub salvageable: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PrizeRecord { pub hull_record: Option<u32>, pub class: Option<ClassId>, pub decision: super::command::PrizeDecision, pub tonnage_t: f64, pub build_cost: u32, pub parts: Vec<PartId>, pub reached_exit: bool, pub crew_captured: u16 }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AceFate { Escaped, Beaten, Captured }

/// State of a surviving enemy hull, written back into its persistent record (crippled groups return weakened).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurvivorRecord { pub hull_record: u32, pub damage: f64, pub fuel: f64, pub ammunition: f64 }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerResult {
    pub player: PlayerSlot,
    pub hull_lost: bool,
    pub damage: HullDamage,
    pub fuel_fraction: f64,
    pub ammo: BTreeMap<PartSlot, u32>,
    /// Munitions expended by key, for restocking from SEXTANT.
    pub expended: BTreeMap<MunitionId, u16>,
    pub ammo_kg_expended: BTreeMap<u16, f64>,
    pub fuel_l_used: f64,
    pub passengers_lost: u16,
    pub dropped: bool,
}

/// Counters for the report card, the after-action report and the local operation log (L70, L80, L216).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SortieStats {
    pub rounds_fired: u32,
    pub rounds_hit: u32,
    pub missiles_fired: u16,
    pub missiles_hit: u16,
    pub torpedoes_fired: u16,
    pub torpedoes_hit: u16,
    pub decoys_fired: u16,
    pub missiles_seduced: u16,
    pub fires: u16,
    pub first_detection_s: Option<f32>,
    /// Seconds any own emitter radiated.
    pub radiating_s: f32,
    pub enemy_tonnage_disabled: f64,
    pub enemy_tonnage_total: f64,
    pub own_cells_lost_fraction: f64,
    pub alerts_raised: u16,
    pub kills_by_method: BTreeMap<KillMethod, u16>,
    /// Enemy hulls that struck their colours under the Combine surrender rule (L537); counted in the disabled
    /// tonnage too, and shown on their own in the after-action report.
    #[serde(default)]
    pub enemy_struck: u16,
}

/// The full result, host side.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SortieResult {
    pub operation: Option<OperationId>,
    pub region: Option<RegionId>,
    pub seed: u64,
    pub outcome: Outcome,
    /// PvP and skirmish: winning side.
    pub winner: Option<SideId>,
    pub duration_ticks: u64,
    pub objective_done: bool,
    /// Ghost condition results in briefing order.
    pub ghosts: Vec<bool>,
    pub kills: Vec<KillRecord>,
    pub survivors: Vec<SurvivorRecord>,
    pub prizes: Vec<PrizeRecord>,
    pub aces: Vec<(AceId, AceFate)>,
    pub players: Vec<PlayerResult>,
    /// Alert inputs before per-operation caps (L749): contacts that held a player hull as Suspect or Hostile,
    /// and evidence items (wreck found, intercepted emission, detected RHIB).
    pub sightings: u16,
    pub evidence: u16,
    pub photos: u8,
    pub survivors_recovered: u16,
    pub crew_captured: u16,
    pub passengers_delivered: u16,
    pub mines_laid: u16,
    /// Depot, radar site or other site objective destroyed.
    pub site_destroyed: bool,
    pub tender_lost: bool,
    /// Highest overload rung the host reached (L837).
    pub highest_rung: u8,
    /// Enemy commander's final belief about the player position, for the after-action overlay (L70, L760):
    /// (world x, world z, weight) samples.
    pub belief: Vec<[f32; 3]>,
    pub stats: SortieStats,
}

/// What a client may show when the sortie ends.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SortieSummary {
    pub outcome: Outcome,
    pub winner: Option<SideId>,
    pub duration_ticks: u64,
    pub objectives: Vec<(ObjectiveId, bool)>,
    pub stats: SortieStats,
    /// One tip for the splash report card (L216).
    pub tip: Option<super::text::Msg>,
}
