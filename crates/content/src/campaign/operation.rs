//! Operations: the 11 parametric templates, story operations, ace duels and capstones, plus constraint tags,
//! ghost conditions and special-operations actions. DESIGN.md L778-807, L92-102 (commitment), L85-90 (approach).
use crate::enums::{Act, Emcon, Stance};
use crate::ids::{AceId, ClassId, FleetTemplateId, OperationId, PartId, RegionId, TextKey};
use crate::part::PartTag;
use crate::weather::{SeaState, WeatherKind};
use serde::{Deserialize, Serialize};

/// The 11 parametric templates (L778).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MissionType { ConvoyRaid, DepotStrike, RadarDecapitation, AgentExtraction, Minelaying, PrizeCapture, DefectorEscort, Feint, PhotoReconnaissance, HunterGroupSurvival, CuttingOut }
impl MissionType { pub const ALL: [MissionType; 11] = [Self::ConvoyRaid, Self::DepotStrike, Self::RadarDecapitation, Self::AgentExtraction, Self::Minelaying, Self::PrizeCapture, Self::DefectorEscort, Self::Feint, Self::PhotoReconnaissance, Self::HunterGroupSurvival, Self::CuttingOut]; }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum OperationKind { Template(MissionType), Story, AceDuel(AceId), Capstone, Tutorial }

/// Briefing constraint tags; each briefing carries 2 to 4 and Fit Check scores each pass or fail (L801).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ConstraintTag { DraftLimit { max_m: f64 }, RcsLimit { max_m2: f64 }, Range { min_km: f64 }, Passengers { count: u8, allowed_losses: u8 }, Requires(PartTag), MinSpeed { kn: f64 } }

/// Timed station actions the player starts and the enemy can interrupt (L786-797).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum StationAction { BoardAndCapture, InsertOrExtract, CarryPassengers, RecoverSurvivors, LayMines, Photograph }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Reward { Intel(u8), Blueprint(PartId), Salvage(u32) }

/// Optional ghost conditions, 2 or 3 per operation (L780).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GhostKind { NoRadarEmissions, NoLosses, NeverDetected, CaptureTheAce, NoKills, UnderTime { seconds: u32 }, NoHullDamage }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GhostCondition { pub kind: GhostKind, pub reward: Reward }

/// Primary objective. `marker` and `tag` names refer to the operation's map markers and force slots.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Objective {
    DestroyAll,
    /// Sink or disable at least `fraction` of the tonnage of the tagged group (convoy raid).
    DestroyGroup { tag: String, fraction: f64 },
    DestroySite { marker: String },
    /// Hold near a shore marker (insert, extract) and leave by an exit marker.
    Extract { marker: String, passengers: u8 },
    LayMines { marker: String, count: u16 },
    Capture { tag: String },
    Escort { tag: String, exit: String },
    /// Be detected, hold the enemy's attention for `hold_s`, then escape.
    Feint { hold_s: u32 },
    Photograph { tag: String, count: u8 },
    Survive { seconds: u32, exit: String },
    CutOut { tag: String, exit: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Complication { None, Reinforcements { fleet: FleetTemplateId, at_s: u32 }, WeatherTurns { to: WeatherKind, at_s: u32 }, AirPatrol, Minefield, DecoyConvoy, CivilianTraffic, AceArrives(AceId) }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Unlock { Always, SupplyBelow { region: RegionId, value: f64 }, AlertAtLeast { region: RegionId, value: u8 }, Completed(Vec<OperationId>), StoryCount(u8), AceBeaten(AceId), All(Vec<Unlock>), Any(Vec<Unlock>) }

/// One force element: a fleet template placed at a marker with a stance and a tag objectives can name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForceEntry {
    pub fleet: FleetTemplateId,
    pub marker: String,
    #[serde(default)]
    pub tag: String,
    /// Stance the briefing declares (defectors, decoy convoys and civilians start Neutral, L502).
    #[serde(default = "hostile")]
    pub stance: Stance,
    /// Seconds after start; 0 = present at start. Scheduled reinforcements count as committed (L94).
    #[serde(default)]
    pub arrives_s: u32,
    #[serde(default)]
    pub emcon: Option<Emcon>,
}
fn hostile() -> Stance { Stance::Hostile }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationDef {
    pub id: OperationId,
    pub kind: OperationKind,
    pub act: Act,
    pub name: TextKey,
    pub briefing: TextKey,
    #[serde(default = "always")]
    pub unlock: Unlock,
    /// Theatre node an authored operation (story, ace duel, capstone) takes place at; `None` for templates,
    /// which the generator places from theatre state (L778).
    #[serde(default)]
    pub node: Option<crate::ids::TheatreNodeId>,
    /// Authored sortie map overriding the node's map.
    #[serde(default)]
    pub map: Option<crate::campaign::MapDef>,
    pub objective: Objective,
    pub tags: Vec<ConstraintTag>,
    #[serde(default)]
    pub ghosts: Vec<GhostCondition>,
    pub force: Vec<ForceEntry>,
    /// Task group template added once per extra co-op player (L834).
    #[serde(default)]
    pub per_extra_player: Option<FleetTemplateId>,
    /// Complications the generator may draw one from.
    #[serde(default)]
    pub complications: Vec<Complication>,
    /// Authored approach distance from player spawn to first objective in metres; the length fallback scales it
    /// within 50%..=150% (L85-90).
    pub approach_m: f64,
    /// Sea states the generator may use; empty = the region's weather at the commit hour.
    #[serde(default)]
    pub sea_states: Vec<SeaState>,
    #[serde(default)]
    pub actions: Vec<StationAction>,
    /// Hostile classes the briefing marks hostile by name (L501) and named hulls (ID rung Exact, L475).
    #[serde(default)]
    pub declared_hostile: Vec<ClassId>,
    /// Recommended emission setting when ghost conditions forbid emissions (L507).
    #[serde(default)]
    pub recommended_emcon: Option<Emcon>,
    /// Operation-clock pushback in theatre days on success: template 1, depot strike or radar decapitation 2,
    /// ace duel 2, story 3 (L750). `None` = rule default for the kind.
    #[serde(default)]
    pub pushback_days: Option<f64>,
    /// Forced operations cannot be declined (hunter group reaches the cove, L749).
    #[serde(default)]
    pub forced: bool,
    /// The reference L0-L2 setup committed for the "defaults fail, sliders pass" gate in Acts 2-4 (L318).
    #[serde(default)]
    pub reference_setup: Option<String>,
}
fn always() -> Unlock { Unlock::Always }
