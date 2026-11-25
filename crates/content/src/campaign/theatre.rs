//! Theatre graph, regions, sun table and sortie maps. DESIGN.md L735-751, L1131-1146, L455 (sonar layer).
use crate::enums::Act;
use crate::ids::{ClassId, RegionId, TextKey, TheatreNodeId};
use crate::weather::WeatherKind;
use serde::{Deserialize, Serialize};

/// What stands at a theatre node (L741-748).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    Sea,
    /// Hidden cove: full save point and dry dock for SEXTANT (L737).
    Cove,
    /// Produces Combine tonnage per theatre day.
    Shipyard { tonnes_per_day: f64 },
    /// Stores fuel and munitions; a strike empties it and cuts every lane it feeds for 10 days.
    Depot { capacity: f64 },
    /// Spends supply to launch task groups.
    ForwardBase,
    Harbour,
    RadarSite,
    /// Briefing-declared ESM site (L510).
    EsmSite,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeDef {
    pub id: TheatreNodeId,
    pub name: TextKey,
    pub region: RegionId,
    /// Position on the 600 x 400 km chart in kilometres, origin at the north-west corner.
    pub pos_km: [f64; 2],
    pub kind: NodeKind,
    /// Sortie map used for operations at this node.
    pub map: MapDef,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneDef { pub a: TheatreNodeId, pub b: TheatreNodeId, #[serde(default = "one")] pub traffic: f64 }
fn one() -> f64 { 1.0 }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionDef {
    pub id: RegionId,
    pub name: TextKey,
    /// Act in which the region opens.
    pub act: Act,
    /// Base patrol count; density = (0.25 + 0.75 * Supply / 100) * this (L747-748).
    pub base_patrols: u16,
    /// Sonar layer depth in metres, 30..=80, default 50 (L455).
    #[serde(default = "layer_default")]
    pub layer_depth_m: f64,
    /// Weather mix: weights per weather kind for the seeded hourly weather.
    pub weather: Vec<(WeatherKind, f64)>,
    /// Starting order of battle: class and hull count on Normal (scaled by the hull-count slider).
    pub order_of_battle: Vec<(ClassId, u16)>,
    /// Civilian traffic classes (start Neutral when a briefing declares them, L502).
    #[serde(default)]
    pub civilian: Vec<ClassId>,
}
fn layer_default() -> f64 { 50.0 }

/// Sun elevation and azimuth by theatre hour of day, shared by doctrine tallies and the renderer (L743).
/// OWNER DECISION PENDING: the table values. The example data is a placeholder; do not tune against it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SunTable { pub elevation_deg: [f64; 24], pub azimuth_deg: [f64; 24] }
impl Default for SunTable { fn default() -> Self { Self { elevation_deg: [0.0; 24], azimuth_deg: [0.0; 24] } } }

/// One island or shoal of a sortie map. Height field = smooth cone-with-plateau; the terrain unit in sim-core
/// defines the exact profile.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Island { pub centre_m: [f64; 2], pub radius_m: f64, pub height_m: f64, #[serde(default)] pub seed: u32 }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarkerKind { PlayerSpawn, EnemySpawn, Objective, ShoreLanding, Survivors, Berth, ConvoyEntry, ConvoyExit, Exit, RadarSite, DepotSite }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Marker { pub kind: MarkerKind, pub name: String, pub pos_m: [f64; 2], #[serde(default)] pub heading_deg: f64 }

/// A sortie map: at most 100 km across, centred on the engagement (L1133). World X east, Z south, in metres
/// from the map centre.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MapDef {
    pub size_m: [f64; 2],
    /// Open-water depth in metres (positive down).
    pub depth_m: f64,
    pub islands: Vec<Island>,
    pub markers: Vec<Marker>,
    pub seed: u32,
}
/// Largest sortie world edge in metres (L1133).
pub const MAX_MAP_M: f64 = 100_000.0;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TheatreDef {
    pub name: TextKey,
    /// 600 x 400 km (L735).
    pub size_km: [f64; 2],
    pub regions: Vec<RegionDef>,
    /// 48 nodes at full scope (L741).
    pub nodes: Vec<NodeDef>,
    pub lanes: Vec<LaneDef>,
    pub sun: SunTable,
    /// Test basin map used by Splash, the tutorial and trials.
    pub basin: MapDef,
}
