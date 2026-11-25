//! Contract-owned types (no work unit; data is authored by the `data-parts-*` units, loading by
//! `content-catalogue`). The part catalogue schema: every stat the simulation, the editor gauges and the
//! renderer need. DESIGN.md L136-151 (parts and kits), L281-295 (networks, priorities), L539-553 (weapons),
//! L801-807 (tiers, counts), L421-456 (sensor reference values).
//!
//! First release carries about 60 parts in 5 tiers: 7 guns (20, 30, 40, 57, 76, 100, 127 mm), 4 missiles,
//! 3 underwater weapons, 5 defence systems, 2 drones, 8 sensors and about 30 hull and system parts (L807).
//! Later-act parts are flagged by [`PartDef::phase`].
pub mod weapons;

use crate::enums::{NetworkKind, Phase, Tier};
use crate::ids::{IconId, ModelId, MunitionId, PartId, TextKey};
use serde::{Deserialize, Serialize};
use weapons::CalibreBand;

/// Tray grouping in the editor and the default power-priority category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PartCategory { Hull, Propulsion, Power, Fuel, Control, Gun, Missile, Underwater, Defence, Offboard, Sensor, Crew, DamageControl, SpecialOps }

/// Where the editor lets a part go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Placement {
    #[default] Anywhere,
    /// Needs open sky above every top cell (mounts, launchers, masts, decks).
    Weather,
    /// Inside the hull envelope.
    Internal,
    /// Touching the outer bottom below the design waterline (propulsors, rudders, hull sonar).
    Underwater,
    /// At the deck edge (davits, mine rails, torpedo tubes).
    DeckEdge,
}

/// What a port does on its network.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PortRole { Supply, Consume, Store, Link }

/// A typed port. Placing a part joins its compartment's network at once; there are no wires (L138).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortDef {
    pub network: NetworkKind,
    pub role: PortRole,
    /// Rated kW (Power) or L/s (Fuel) at full output; 0 for Data.
    #[serde(default)]
    pub rate: f64,
    /// Stored kWh (Power) or litres (Fuel) for `Store` ports.
    #[serde(default)]
    pub capacity: f64,
}

/// Tags that link parts to briefing constraint tags and special-operations actions (L786-801).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PartTag {
    Crane, MineRail, RhibDavit, BoardingKit, RecoveryNet, CameraMast, PassengerBerths, PointDefence, Magazine, FuelStore, Propulsion, Steering, Generation,
    /// Lowers the hull's radar cross-section (enclosed composite masts and the kits built on them): the
    /// Quartermaster's curated list for a briefing's cross-section limit (L803).
    LowSignature,
}

/// Procedural model reference: a hull-mesh generator plus its parameters (L1233). No authored meshes exist.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcModel {
    pub generator: ModelId,
    #[serde(default)]
    pub params: Vec<f32>,
}

/// One catalogue entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartDef {
    pub id: PartId,
    pub name: TextKey,
    pub description: TextKey,
    pub category: PartCategory,
    pub tier: Tier,
    /// Release phase that adds the part to the build. Sandbox, skirmish and PvP offer every part in the build
    /// from first launch; the campaign gates by tier (L140, L807).
    #[serde(default)]
    pub phase: Phase,
    /// Footprint in whole cells along X (bow), Y (up), Z (starboard) at rotation 0 (L132).
    pub size: [u8; 3],
    #[serde(default)]
    pub placement: Placement,
    /// Dry mass in kilograms, spread evenly over the footprint cells.
    pub mass_kg: f64,
    /// Build cost in Salvage. Anchor: a fully fitted 150 t Act 1 hull is about 1,000 Salvage (L811-822).
    pub cost: u32,
    #[serde(default)]
    pub ports: Vec<PortDef>,
    /// Default power priority 1..=5 by category (table at L287-295). 0 = not a power consumer.
    #[serde(default)]
    pub power_priority: u8,
    /// Local battery in seconds at rated draw (default 30 s) and day tank in seconds at rated flow (default 60 s)
    /// used when the part loses its network (L394). `None` takes the tuning-table default.
    #[serde(default)]
    pub buffer_s: Option<f64>,
    /// True when the part's cells are watertight structure (tanks, magazines); false for open equipment.
    #[serde(default)]
    pub watertight: bool,
    /// Cells of this part count as fuel or ammunition for fire ignition (L662).
    #[serde(default)]
    pub flammable: Flammable,
    /// Radar cross-section material factor applied to the part's projected area (L427). 1.0 = bare steel.
    #[serde(default = "one_f32")]
    pub rcs_factor: f32,
    #[serde(default)]
    pub tags: Vec<PartTag>,
    pub function: PartFunction,
    #[serde(default)]
    pub model: ProcModel,
    #[serde(default)]
    pub icon: IconId,
    /// Migration target when a later release removes this part (L2083).
    #[serde(default)]
    pub replaced_by: Option<PartId>,
    /// Maximum copies per vessel under the standard PvP ruleset; `None` = unlimited (L233, L965).
    #[serde(default)]
    pub standard_limit: Option<u16>,
}
fn one_f32() -> f32 { 1.0 }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Flammable { #[default] No, Fuel, Ammunition }

/// What a part does. One variant per behaviour the simulation implements.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PartFunction {
    /// Inert fitting (mast section, funnel, deckhouse block, ballast).
    Structure,
    /// Adjustable ballast in kilograms (Suggest: ballast and trim, L210).
    Ballast { max_kg: f64 },
    Engine(EngineDef),
    /// Electrical generation in kW burning `fuel_lps` at full load.
    Generator { kw: f64, fuel_lps: f64 },
    Battery { kwh: f64, max_kw: f64 },
    FuelTank { litres: f64 },
    Propulsor(PropulsorDef),
    Rudder { area_m2: f64, max_deg: f64, rate_dps: f64 },
    /// Bridge or control console on the Data network (priority 1, L291). Loss forces local control.
    Console,
    Gun(GunDef),
    /// Ammunition stays inside each mount kit: magazine, hoist, gun (L283).
    Magazine { calibre_mm: u16, rounds: u32, ammo_kg: f64 },
    Hoist { rounds_per_min: f64 },
    Launcher(LauncherDef),
    /// Hull or system part, not an underwater weapon (L551). Lays [`MunitionId`] mines.
    MineRail { mine: MunitionId, capacity: u16 },
    Decoy(DecoyLauncherDef),
    /// Self-screening jammer rated by burn-through range `r_bt0_m` (default 10 km, L428).
    Jammer { r_bt0_m: f64 },
    Sensor(SensorDef),
    /// Fire-control director: lets guns in its kit use a fire-control radar's lock.
    Director,
    /// Bilge or damage-control pump: small 0.05, large 0.15 cubic metres per second (L638).
    Pump { m3ps: f64 },
    /// Crew berths. Damage-control teams = floor(berths / 4), min 1, max 6 (L703).
    Berths { count: u8 },
    PassengerBerths { count: u8 },
    /// Drone launch and recharge (2 drone types at first release, L549).
    DroneBay { drone: MunitionId, count: u8, recharge_s: f64 },
    /// Act 2: small deck carrying a stock helicopter flown by an officer (L551). Phase::Act2.
    HeliDeck { helicopter: MunitionId },
    RhibDavit,
    /// RHIB davit plus boarding-team berths (L786).
    BoardingKit { team_berths: u8 },
    RecoveryNet,
    /// Trainable camera: doubles visual range, 10 degree FOV, 6 km Photograph range (L436, L796).
    CameraMast { fov_deg: f64, photo_range_m: f64 },
    Crane,
    /// Watertight door or hatch joining two compartments; closed by default in action.
    Door,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineKind { Diesel, GasTurbine, ElectricMotor }

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineDef {
    pub kind: EngineKind,
    /// Shaft power in kW at full throttle. Passive sonar noise N = propulsion power / 2 MW (L453).
    pub shaft_kw: f64,
    /// Fuel burn at full throttle in L/s (0 for electric motors, which draw `shaft_kw` from the Power network).
    pub fuel_lps: f64,
    /// Seconds from idle to full power.
    pub spool_s: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PropulsorKind { Propeller, Waterjet }

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PropulsorDef {
    pub kind: PropulsorKind,
    /// Largest shaft power it can absorb in kW.
    pub max_shaft_kw: f64,
    /// Static thrust per kW absorbed, N/kW; thrust falls with speed per the hydro unit's law.
    pub thrust_n_per_kw: f64,
    /// Speed above which the propeller cavitates and doubles acoustic noise (default 20 kn, L453).
    pub cavitation_kn: f64,
    /// Waterjets vector thrust; propellers need a rudder.
    pub steer_max_deg: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GunDef {
    pub calibre_mm: u16,
    pub band: CalibreBand,
    pub barrels: u8,
    /// Rounds per minute per barrel.
    pub rof_rpm: f64,
    pub muzzle_mps: f64,
    pub shell_kg: f64,
    pub train_rate_dps: f64,
    pub elevation_rate_dps: f64,
    pub elevation_min_deg: f64,
    pub elevation_max_deg: f64,
    /// 1-sigma dispersion in milliradians on a steady platform.
    pub dispersion_mrad: f64,
    /// Point-defence mount: rounds self-destruct after 4 s and hit small bodies within 1.5 m (L557, L597).
    #[serde(default)]
    pub point_defence: bool,
    /// Ready rounds on the mount before the hoist must feed.
    pub ready_rounds: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LauncherKind { Missile, TorpedoTube, DepthChargeRack }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LauncherDef {
    pub kind: LauncherKind,
    pub munition: MunitionId,
    /// Ready cells or tubes.
    pub cells: u8,
    /// Seconds between launches from adjacent cells.
    pub interval_s: f64,
    /// Seconds to reload one cell from a magazine at sea; 0 = reload only at the tender.
    pub reload_s: f64,
    /// Trainable launcher limits; `None` = fixed (vertical or boxed).
    #[serde(default)]
    pub train_rate_dps: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DecoyKind { Chaff, Flare, Smoke, TowedDecoy }

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecoyLauncherDef {
    pub kind: DecoyKind,
    /// Rounds carried (ignored for towed decoys).
    pub rounds: u16,
    /// Rounds per salvo.
    pub salvo: u8,
    /// Reload between salvos in seconds (10 s default, L486).
    pub reload_s: f64,
}

/// Sensor families with their detection laws in sim-systems (L421-456).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SensorKind { SearchRadar, NavRadar, FireControlRadar, Seeker, Esm, PassiveSonar, ActiveSonar, Visual, Camera }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensorDef {
    pub kind: SensorKind,
    /// Reference range in metres: against 100 m^2 for radar and active sonar, against N = 1.0 for passive sonar
    /// (L426, L453-454). Designers tune only this (L434). Unused for ESM and visual.
    pub r_ref_m: f64,
    /// Evaluation rate in Hz: search and nav radar 5 (2..=10), fire control and seekers 10, others 2 (L412).
    pub rate_hz: f64,
    /// Rotation rate for rotating antennas (24 rpm, L412); `None` = staring.
    #[serde(default)]
    pub rpm: Option<f64>,
    /// Half-angle of the field of view for staring sensors in degrees; 180 = all round.
    #[serde(default = "half_circle")]
    pub fov_half_deg: f64,
    /// Emitter model key matched by ESM libraries (L472-475). Empty for passive sensors.
    #[serde(default)]
    pub emitter: String,
}
fn half_circle() -> f64 { 180.0 }
