//! Contract-owned (no work unit). Every enumeration that crosses to GDScript, as integer constants on the
//! Godot class `Bp`, and the conversions between those integers and the Rust enums.
//! DESIGN.md L1663 (stance and rung enums shown as words), L1676-1687 (alert priorities), L1725-1744.
//!
//! Rules:
//! - GDScript never writes a bare number: it writes `Bp.STANCE_HOSTILE`. A wrong name is a parse error.
//! - Values are the position in the variant list below and are stable within one build only; nothing stores
//!   them on disk or sends them over the wire.
//! - Words for the player come from `Tx` (`BpText`), never from the constant's name.
//! - Enums with payloads cross as a Dictionary whose `kind` key holds one of the kind constants below.
//!
//! This file is generated from the table in the architect's scratch script and then frozen: edit by hand,
//! keeping the three places per enum in step (the `gd_enum!` list, the constants, the doc line).
use godot::prelude::*;

/// Conversion between a fieldless Rust enum and its `Bp.*` integer.
pub trait GdEnum: Sized + Copy + PartialEq + 'static {
    /// Variants in constant order; index = GDScript value.
    const VARIANTS: &'static [Self];
    fn to_gd(self) -> i64 { Self::VARIANTS.iter().position(|v| *v == self).map_or(-1, |i| i as i64) }
    /// `None` for any integer that is not one of the constants.
    fn from_gd(value: i64) -> Option<Self> { usize::try_from(value).ok().and_then(|i| Self::VARIANTS.get(i).copied()) }
}
macro_rules! gd_enum {
    ($t:ty { $($v:ident),+ $(,)? }) => { impl GdEnum for $t { const VARIANTS: &'static [Self] = &[$(<$t>::$v),+]; } };
}
macro_rules! own_enum {
    ($(#[$m:meta])* $name:ident { $($v:ident),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name { #[default] $($v),+ }
        impl GdEnum for $name { const VARIANTS: &'static [Self] = &[$($name::$v),+]; }
    };
}

// ---- Enums from content, sim-core and sim-systems ----
gd_enum!(content::enums::Act { Act1, Act2, Act3, Act4 });
gd_enum!(content::enums::Tier { T1, T2, T3, T4, T5 });
gd_enum!(content::enums::NetworkKind { Power, Fuel, Data });
gd_enum!(content::enums::Domain { Surface, Subsurface, Air, Weapon });
gd_enum!(content::enums::Stance { Friend, Neutral, Suspect, Hostile });
gd_enum!(content::enums::IdRung { Unknown, Domain, Type, Class, Exact });
gd_enum!(content::enums::TrackGrade { Bearing, Coarse, Fine, Lock });
gd_enum!(content::enums::Emcon { Silent, Burst, Active });
gd_enum!(content::enums::DoctrinePreset { Aggressive, Cautious, SilentRunning });
gd_enum!(content::enums::OfficerRole { Helm, Engineer, Sensors, Gunnery, DamageControl });
gd_enum!(content::enums::ControlLayer { L0Doctrine, L1Tune, L2Rules, L3Graph, L4Script });
gd_enum!(content::enums::DamageLevel { Intact, Damaged, Heavy, Destroyed });
gd_enum!(content::enums::GameMode { Campaign, Skirmish, Sandbox, PvpElimination, PvpEscort, Splash, Tutorial, Trial });
gd_enum!(content::enums::TimeOfDay { Night, Dusk, Day });
gd_enum!(content::weather::WeatherKind { Clear, Haze, Overcast, Rain, Storm, FogBank });
gd_enum!(content::part::PartCategory { Hull, Propulsion, Power, Fuel, Control, Gun, Missile, Underwater, Defence, Offboard, Sensor, Crew, DamageControl, SpecialOps });
gd_enum!(content::part::DecoyKind { Chaff, Flare, Smoke, TowedDecoy });
gd_enum!(content::campaign::MissionType { ConvoyRaid, DepotStrike, RadarDecapitation, AgentExtraction, Minelaying, PrizeCapture, DefectorEscort, Feint, PhotoReconnaissance, HunterGroupSurvival, CuttingOut });
gd_enum!(content::campaign::StationAction { BoardAndCapture, InsertOrExtract, CarryPassengers, RecoverSurvivors, LayMines, Photograph });
gd_enum!(content::campaign::KillMethod { Gunfire, Missile, Torpedo, Mine, DepthCharge, Fire, Capture, Collision });
gd_enum!(content::graph::RepairPriority { MagazineFires, BreachesBelowWaterline, OtherFires, ConduitsToCritical, OtherConduits, PartRepairs });
gd_enum!(content::graph::WeaponClass { Gun, AntiShipMissile, Sam, PointDefenceGun, PointDefenceMissile, Torpedo, DepthCharge, Decoy });
gd_enum!(content::graph::TargetCategory { Surface, Air, InboundWeapon, Subsurface });
gd_enum!(content::graph::PortType { Bool, Scalar, Angle, Vec3, Text, Stance, Emcon, Track, TrackList, DetectionList, PartState, Orders });
gd_enum!(content::graph::OfficerStage { HelmAutopilot, StationKeeping, Evasion, TrackFusion, ThreatRanking, MountAssignment, BallisticSolver, PnGuidance, LoadShedder, DamageControlPlanner, EmissionController });
gd_enum!(content::graph::Cmp { Lt, Le, Gt, Ge, Eq, Ne });
gd_enum!(content::ruleset::RulesetKind { Standard, Open });
gd_enum!(content::settings::SplashTarget { Drones, PatrolBoat });
gd_enum!(content::cell::ArmourTier { None, Mm5, Mm15, Mm40, Mm80 });
gd_enum!(content::blueprint::MarkingKind { Stripe, Flag, HullNumber });
gd_enum!(sim_core::networks::LinkState { Network, Redundant, Buffer, LocalControl, Dead });
gd_enum!(sim_systems::protocol::AlertPriority { Caution, Warning, Alarm, Emergency });
gd_enum!(sim_systems::protocol::AlertState { Unacknowledged, Acknowledged, Cleared, Shelved, LogOnly });
gd_enum!(sim_systems::protocol::Subsystem { Threat, Flooding, Fire, Stability, Power, Fuel, Propulsion, Steering, Weapons, Sensors, Data, Crew, Navigation, Mission, System });
gd_enum!(sim_systems::protocol::ControlHolder { Officer, Player });
gd_enum!(sim_systems::protocol::SeekerPhase { Off, Midcourse, Searching, Locked, Seduced, Lost });
gd_enum!(sim_systems::protocol::ObjectiveState { Active, Done, Failed, Hidden });
gd_enum!(sim_systems::protocol::ActionState { Running, Paused, Aborted, Done });
gd_enum!(sim_systems::protocol::CompressionBlock { WeaponInbound, ThreatInOwnEnvelope, InsideThreatEnvelope, HostLoad, TimeScaleDegraded, NotAllPlayers, ModeForbids });
gd_enum!(sim_systems::protocol::VerticalTrend { Steady, Climbing, Diving });
gd_enum!(sim_systems::protocol::Outcome { Victory, Defeat, Withdrawn, Aborted, Draw });
gd_enum!(sim_systems::protocol::PrizeDecision { SailHome, StripOnSpot });
gd_enum!(sim_systems::editor::Check { Pass, Caution, Fail });
gd_enum!(sim_systems::editor::notes::NoteTier { CannotFloatMoveOrPower, FailsBriefingTag, CannotWork, GaugeCaution, Advisory });
gd_enum!(sim_systems::editor::suggest::SuggestKind { BallastAndTrim, ArmourLayout, EngineAndGeneratorSizing, FuelLoad, SensorPlacement, BulkheadPlacement, Aggression, EmconTriggerRange });
gd_enum!(sim_systems::theatre::Confidence { Possible, Probable, Confirmed });
gd_enum!(sim_systems::theatre::saves::SaveGate { Allowed, InSortie, NotAtCoveOrBetweenOperations });
gd_enum!(sim_systems::theatre::AlertCause { ObservedKill, UnobservedKillNoticed, Sighting, Evidence, QuietDecay, ActStart, SextantSunk });

// ---- Enums from hull-mesh, audio and net (the sibling crates own them; gdclient only numbers them) ----
/// Graphics tier (L1977-1982). The one definition is `hull_mesh::QualityTier`; every gdclient module names it
/// through this re-export. Its Rust default is `Medium`. The `Bp.QUALITY_*` order below (Low first) is the
/// GDScript numbering and is independent of the Rust declaration order (Fallback first).
pub use hull_mesh::QualityTier;
gd_enum!(hull_mesh::QualityTier { Low, Medium, High, Fallback });
gd_enum!(audio::UiSfx { Click, Select, Place, Remove, Invalid, Confirm, Rotate, Snap, TimeRate, Pause, Resume, TutorialStep, ObjectiveDone, Saved });
gd_enum!(net::msg::LinkQuality { Good, Fair, Poor });
gd_enum!(net::msg::DropWarning { HighPing, PacketLoss, FallingBehind });

// ---- Enums owned by gdclient ----
own_enum!(/// Sortie view (L106). Tab switches.
    View { Bridge, Plot });
own_enum!(/// State of `BpSession`.
    SessionState { Idle, Loading, Lobby, InSortie, Ended, Replay, Failed });
own_enum!(/// What the current session is.
    SessionKind { None, Campaign, Skirmish, Sandbox, Pvp, Splash, Tutorial, Replay, Benchmark, Scripted });
own_enum!(/// This process's network role.
    NetRole { Offline, Host, Client });
own_enum!(/// Connection state of one player or of the local link (L1033-1039).
    ConnState { Closed, Connecting, Connected, Warning, Dropped, Refused });
own_enum!(/// Automatic port-mapping result (L994-999).
    PortMapState { Idle, Trying, Mapped, DoubleNat, Failed, RenewFailed });
own_enum!(/// UI palette (L1622). Chart screens always use the chart tokens.
    Palette { Dusk, HighContrast });
own_enum!(/// Active dry-dock tool.
    EditorTool { Select, PlacePart, PlaceKit, HullHandles, Planes, Armour, Paint, Marking, Sculpt, Route });
own_enum!(/// Placement symmetry (L233).
    Symmetry { None, Mirror, Radial });
own_enum!(/// Why the dry dock is open; picks part availability, caps and cost display.
    EditorMode { Sandbox, Campaign, Lobby, EnemyClass, Tutorial });
own_enum!(/// HUD log entry type (rule 15, L1671).
    LogKind { Track, Engagement, Damage, Officer, Control, Alert, Pacing, Objective, Network, Ace, System });
own_enum!(/// Benchmark stages (L1392-1399).
    BenchStage { Idle, ReferenceBattle, CoopReference, MinimumSwarm, Editor, SimCalib, Soak, Done });
own_enum!(/// State of a background-worker job (gauges, trials, Fit Check, Quartermaster, splash pre-build).
    JobState { Idle, Running, Done, Failed, Stale });
own_enum!(/// State of `BpTutorial`.
    TutorialState { Off, Running, Finished });
own_enum!(/// Where the campaign loop stands (L62-73).
    CampaignPhase { None, Chart, Advancing, Briefing, DryDock, Sortie, AfterAction, Over });

/// Time compression rate as GDScript sees it: the factor 1, 2 or 4 (L113).
pub fn time_rate_to_gd(rate: sim_systems::protocol::TimeRate) -> i64 { rate.factor() as i64 }
pub fn time_rate_from_gd(factor: i64) -> Option<sim_systems::protocol::TimeRate> {
    use sim_systems::protocol::TimeRate::*;
    match factor { 1 => Some(X1), 2 => Some(X2), 4 => Some(X4), _ => None }
}

/// Holder of the `Bp.*` constants. Never instantiated; GDScript reads `Bp.NAME`.
#[derive(GodotClass)]
#[class(init, base = Object)]
pub struct Bp { base: Base<Object> }

#[godot_api]
impl Bp {
    // `content::enums::Act`
    #[constant] const ACT_1: i64 = 0;
    #[constant] const ACT_2: i64 = 1;
    #[constant] const ACT_3: i64 = 2;
    #[constant] const ACT_4: i64 = 3;
    // `content::enums::Tier`
    #[constant] const TIER_1: i64 = 0;
    #[constant] const TIER_2: i64 = 1;
    #[constant] const TIER_3: i64 = 2;
    #[constant] const TIER_4: i64 = 3;
    #[constant] const TIER_5: i64 = 4;
    // `content::enums::NetworkKind`
    #[constant] const NETWORK_POWER: i64 = 0;
    #[constant] const NETWORK_FUEL: i64 = 1;
    #[constant] const NETWORK_DATA: i64 = 2;
    // `content::enums::Domain`
    #[constant] const DOMAIN_SURFACE: i64 = 0;
    #[constant] const DOMAIN_SUBSURFACE: i64 = 1;
    #[constant] const DOMAIN_AIR: i64 = 2;
    #[constant] const DOMAIN_WEAPON: i64 = 3;
    // `content::enums::Stance`
    #[constant] const STANCE_FRIEND: i64 = 0;
    #[constant] const STANCE_NEUTRAL: i64 = 1;
    #[constant] const STANCE_SUSPECT: i64 = 2;
    #[constant] const STANCE_HOSTILE: i64 = 3;
    // `content::enums::IdRung`
    #[constant] const RUNG_UNKNOWN: i64 = 0;
    #[constant] const RUNG_DOMAIN: i64 = 1;
    #[constant] const RUNG_TYPE: i64 = 2;
    #[constant] const RUNG_CLASS: i64 = 3;
    #[constant] const RUNG_EXACT: i64 = 4;
    // `content::enums::TrackGrade`
    #[constant] const GRADE_BEARING: i64 = 0;
    #[constant] const GRADE_COARSE: i64 = 1;
    #[constant] const GRADE_FINE: i64 = 2;
    #[constant] const GRADE_LOCK: i64 = 3;
    // `content::enums::Emcon`
    #[constant] const EMCON_SILENT: i64 = 0;
    #[constant] const EMCON_BURST: i64 = 1;
    #[constant] const EMCON_ACTIVE: i64 = 2;
    // `content::enums::DoctrinePreset`
    #[constant] const PRESET_AGGRESSIVE: i64 = 0;
    #[constant] const PRESET_CAUTIOUS: i64 = 1;
    #[constant] const PRESET_SILENT_RUNNING: i64 = 2;
    // `content::enums::OfficerRole`
    #[constant] const OFFICER_HELM: i64 = 0;
    #[constant] const OFFICER_ENGINEER: i64 = 1;
    #[constant] const OFFICER_SENSORS: i64 = 2;
    #[constant] const OFFICER_GUNNERY: i64 = 3;
    #[constant] const OFFICER_DAMAGE_CONTROL: i64 = 4;
    // `content::enums::ControlLayer`
    #[constant] const LAYER_DOCTRINE: i64 = 0;
    #[constant] const LAYER_TUNE: i64 = 1;
    #[constant] const LAYER_RULES: i64 = 2;
    #[constant] const LAYER_GRAPH: i64 = 3;
    #[constant] const LAYER_SCRIPT: i64 = 4;
    // `content::enums::DamageLevel`
    #[constant] const DAMAGE_INTACT: i64 = 0;
    #[constant] const DAMAGE_DAMAGED: i64 = 1;
    #[constant] const DAMAGE_HEAVY: i64 = 2;
    #[constant] const DAMAGE_DESTROYED: i64 = 3;
    // `content::enums::GameMode`
    #[constant] const MODE_CAMPAIGN: i64 = 0;
    #[constant] const MODE_SKIRMISH: i64 = 1;
    #[constant] const MODE_SANDBOX: i64 = 2;
    #[constant] const MODE_PVP_ELIMINATION: i64 = 3;
    #[constant] const MODE_PVP_ESCORT: i64 = 4;
    #[constant] const MODE_SPLASH: i64 = 5;
    #[constant] const MODE_TUTORIAL: i64 = 6;
    #[constant] const MODE_TRIAL: i64 = 7;
    // `content::enums::TimeOfDay`
    #[constant] const TIME_NIGHT: i64 = 0;
    #[constant] const TIME_DUSK: i64 = 1;
    #[constant] const TIME_DAY: i64 = 2;
    // `content::weather::WeatherKind`
    #[constant] const WEATHER_CLEAR: i64 = 0;
    #[constant] const WEATHER_HAZE: i64 = 1;
    #[constant] const WEATHER_OVERCAST: i64 = 2;
    #[constant] const WEATHER_RAIN: i64 = 3;
    #[constant] const WEATHER_STORM: i64 = 4;
    #[constant] const WEATHER_FOG_BANK: i64 = 5;
    // `content::part::PartCategory`
    #[constant] const CATEGORY_HULL: i64 = 0;
    #[constant] const CATEGORY_PROPULSION: i64 = 1;
    #[constant] const CATEGORY_POWER: i64 = 2;
    #[constant] const CATEGORY_FUEL: i64 = 3;
    #[constant] const CATEGORY_CONTROL: i64 = 4;
    #[constant] const CATEGORY_GUN: i64 = 5;
    #[constant] const CATEGORY_MISSILE: i64 = 6;
    #[constant] const CATEGORY_UNDERWATER: i64 = 7;
    #[constant] const CATEGORY_DEFENCE: i64 = 8;
    #[constant] const CATEGORY_OFFBOARD: i64 = 9;
    #[constant] const CATEGORY_SENSOR: i64 = 10;
    #[constant] const CATEGORY_CREW: i64 = 11;
    #[constant] const CATEGORY_DAMAGE_CONTROL: i64 = 12;
    #[constant] const CATEGORY_SPECIAL_OPS: i64 = 13;
    // `content::part::DecoyKind`
    #[constant] const DECOY_CHAFF: i64 = 0;
    #[constant] const DECOY_FLARE: i64 = 1;
    #[constant] const DECOY_SMOKE: i64 = 2;
    #[constant] const DECOY_TOWED_DECOY: i64 = 3;
    // `content::campaign::MissionType`
    #[constant] const MISSION_CONVOY_RAID: i64 = 0;
    #[constant] const MISSION_DEPOT_STRIKE: i64 = 1;
    #[constant] const MISSION_RADAR_DECAPITATION: i64 = 2;
    #[constant] const MISSION_AGENT_EXTRACTION: i64 = 3;
    #[constant] const MISSION_MINELAYING: i64 = 4;
    #[constant] const MISSION_PRIZE_CAPTURE: i64 = 5;
    #[constant] const MISSION_DEFECTOR_ESCORT: i64 = 6;
    #[constant] const MISSION_FEINT: i64 = 7;
    #[constant] const MISSION_PHOTO_RECONNAISSANCE: i64 = 8;
    #[constant] const MISSION_HUNTER_GROUP_SURVIVAL: i64 = 9;
    #[constant] const MISSION_CUTTING_OUT: i64 = 10;
    // `content::campaign::StationAction`
    #[constant] const STATION_BOARD_AND_CAPTURE: i64 = 0;
    #[constant] const STATION_INSERT_OR_EXTRACT: i64 = 1;
    #[constant] const STATION_CARRY_PASSENGERS: i64 = 2;
    #[constant] const STATION_RECOVER_SURVIVORS: i64 = 3;
    #[constant] const STATION_LAY_MINES: i64 = 4;
    #[constant] const STATION_PHOTOGRAPH: i64 = 5;
    // `content::campaign::KillMethod`
    #[constant] const KILL_GUNFIRE: i64 = 0;
    #[constant] const KILL_MISSILE: i64 = 1;
    #[constant] const KILL_TORPEDO: i64 = 2;
    #[constant] const KILL_MINE: i64 = 3;
    #[constant] const KILL_DEPTH_CHARGE: i64 = 4;
    #[constant] const KILL_FIRE: i64 = 5;
    #[constant] const KILL_CAPTURE: i64 = 6;
    #[constant] const KILL_COLLISION: i64 = 7;
    // `content::graph::RepairPriority`
    #[constant] const REPAIR_MAGAZINE_FIRES: i64 = 0;
    #[constant] const REPAIR_BREACHES_BELOW_WATERLINE: i64 = 1;
    #[constant] const REPAIR_OTHER_FIRES: i64 = 2;
    #[constant] const REPAIR_CONDUITS_TO_CRITICAL: i64 = 3;
    #[constant] const REPAIR_OTHER_CONDUITS: i64 = 4;
    #[constant] const REPAIR_PART_REPAIRS: i64 = 5;
    // `content::graph::WeaponClass`
    #[constant] const WEAPON_GUN: i64 = 0;
    #[constant] const WEAPON_ANTI_SHIP_MISSILE: i64 = 1;
    #[constant] const WEAPON_SAM: i64 = 2;
    #[constant] const WEAPON_POINT_DEFENCE_GUN: i64 = 3;
    #[constant] const WEAPON_POINT_DEFENCE_MISSILE: i64 = 4;
    #[constant] const WEAPON_TORPEDO: i64 = 5;
    #[constant] const WEAPON_DEPTH_CHARGE: i64 = 6;
    #[constant] const WEAPON_DECOY: i64 = 7;
    // `content::graph::TargetCategory`
    #[constant] const TARGET_SURFACE: i64 = 0;
    #[constant] const TARGET_AIR: i64 = 1;
    #[constant] const TARGET_INBOUND_WEAPON: i64 = 2;
    #[constant] const TARGET_SUBSURFACE: i64 = 3;
    // `content::graph::PortType`
    #[constant] const PORT_BOOL: i64 = 0;
    #[constant] const PORT_SCALAR: i64 = 1;
    #[constant] const PORT_ANGLE: i64 = 2;
    #[constant] const PORT_VEC3: i64 = 3;
    #[constant] const PORT_TEXT: i64 = 4;
    #[constant] const PORT_STANCE: i64 = 5;
    #[constant] const PORT_EMCON: i64 = 6;
    #[constant] const PORT_TRACK: i64 = 7;
    #[constant] const PORT_TRACK_LIST: i64 = 8;
    #[constant] const PORT_DETECTION_LIST: i64 = 9;
    #[constant] const PORT_PART_STATE: i64 = 10;
    #[constant] const PORT_ORDERS: i64 = 11;
    // `content::graph::OfficerStage`
    #[constant] const STAGE_HELM_AUTOPILOT: i64 = 0;
    #[constant] const STAGE_STATION_KEEPING: i64 = 1;
    #[constant] const STAGE_EVASION: i64 = 2;
    #[constant] const STAGE_TRACK_FUSION: i64 = 3;
    #[constant] const STAGE_THREAT_RANKING: i64 = 4;
    #[constant] const STAGE_MOUNT_ASSIGNMENT: i64 = 5;
    #[constant] const STAGE_BALLISTIC_SOLVER: i64 = 6;
    #[constant] const STAGE_PN_GUIDANCE: i64 = 7;
    #[constant] const STAGE_LOAD_SHEDDER: i64 = 8;
    #[constant] const STAGE_DAMAGE_CONTROL_PLANNER: i64 = 9;
    #[constant] const STAGE_EMISSION_CONTROLLER: i64 = 10;
    // `content::graph::Cmp`
    #[constant] const CMP_LT: i64 = 0;
    #[constant] const CMP_LE: i64 = 1;
    #[constant] const CMP_GT: i64 = 2;
    #[constant] const CMP_GE: i64 = 3;
    #[constant] const CMP_EQ: i64 = 4;
    #[constant] const CMP_NE: i64 = 5;
    // `content::ruleset::RulesetKind`
    #[constant] const RULESET_STANDARD: i64 = 0;
    #[constant] const RULESET_OPEN: i64 = 1;
    // `content::settings::SplashTarget`
    #[constant] const SPLASH_TARGET_DRONES: i64 = 0;
    #[constant] const SPLASH_TARGET_PATROL_BOAT: i64 = 1;
    // `content::cell::ArmourTier`
    #[constant] const ARMOUR_NONE: i64 = 0;
    #[constant] const ARMOUR_5MM: i64 = 1;
    #[constant] const ARMOUR_15MM: i64 = 2;
    #[constant] const ARMOUR_40MM: i64 = 3;
    #[constant] const ARMOUR_80MM: i64 = 4;
    // `content::blueprint::MarkingKind`
    #[constant] const MARKING_STRIPE: i64 = 0;
    #[constant] const MARKING_FLAG: i64 = 1;
    #[constant] const MARKING_HULL_NUMBER: i64 = 2;
    // `sim_core::networks::LinkState`
    #[constant] const LINK_NETWORK: i64 = 0;
    #[constant] const LINK_REDUNDANT: i64 = 1;
    #[constant] const LINK_BUFFER: i64 = 2;
    #[constant] const LINK_LOCAL_CONTROL: i64 = 3;
    #[constant] const LINK_DEAD: i64 = 4;
    // `sim_systems::protocol::AlertPriority`
    #[constant] const PRIORITY_CAUTION: i64 = 0;
    #[constant] const PRIORITY_WARNING: i64 = 1;
    #[constant] const PRIORITY_ALARM: i64 = 2;
    #[constant] const PRIORITY_EMERGENCY: i64 = 3;
    // `sim_systems::protocol::AlertState`
    #[constant] const ALERT_UNACKNOWLEDGED: i64 = 0;
    #[constant] const ALERT_ACKNOWLEDGED: i64 = 1;
    #[constant] const ALERT_CLEARED: i64 = 2;
    #[constant] const ALERT_SHELVED: i64 = 3;
    #[constant] const ALERT_LOG_ONLY: i64 = 4;
    // `sim_systems::protocol::Subsystem`
    #[constant] const SUBSYSTEM_THREAT: i64 = 0;
    #[constant] const SUBSYSTEM_FLOODING: i64 = 1;
    #[constant] const SUBSYSTEM_FIRE: i64 = 2;
    #[constant] const SUBSYSTEM_STABILITY: i64 = 3;
    #[constant] const SUBSYSTEM_POWER: i64 = 4;
    #[constant] const SUBSYSTEM_FUEL: i64 = 5;
    #[constant] const SUBSYSTEM_PROPULSION: i64 = 6;
    #[constant] const SUBSYSTEM_STEERING: i64 = 7;
    #[constant] const SUBSYSTEM_WEAPONS: i64 = 8;
    #[constant] const SUBSYSTEM_SENSORS: i64 = 9;
    #[constant] const SUBSYSTEM_DATA: i64 = 10;
    #[constant] const SUBSYSTEM_CREW: i64 = 11;
    #[constant] const SUBSYSTEM_NAVIGATION: i64 = 12;
    #[constant] const SUBSYSTEM_MISSION: i64 = 13;
    #[constant] const SUBSYSTEM_SYSTEM: i64 = 14;
    // `sim_systems::protocol::ControlHolder`
    #[constant] const HOLDER_OFFICER: i64 = 0;
    #[constant] const HOLDER_PLAYER: i64 = 1;
    // `sim_systems::protocol::SeekerPhase`
    #[constant] const SEEKER_OFF: i64 = 0;
    #[constant] const SEEKER_MIDCOURSE: i64 = 1;
    #[constant] const SEEKER_SEARCHING: i64 = 2;
    #[constant] const SEEKER_LOCKED: i64 = 3;
    #[constant] const SEEKER_SEDUCED: i64 = 4;
    #[constant] const SEEKER_LOST: i64 = 5;
    // `sim_systems::protocol::ObjectiveState`
    #[constant] const OBJECTIVE_ACTIVE: i64 = 0;
    #[constant] const OBJECTIVE_DONE: i64 = 1;
    #[constant] const OBJECTIVE_FAILED: i64 = 2;
    #[constant] const OBJECTIVE_HIDDEN: i64 = 3;
    // `sim_systems::protocol::ActionState`
    #[constant] const ACTION_RUNNING: i64 = 0;
    #[constant] const ACTION_PAUSED: i64 = 1;
    #[constant] const ACTION_ABORTED: i64 = 2;
    #[constant] const ACTION_DONE: i64 = 3;
    // `sim_systems::protocol::CompressionBlock`
    #[constant] const BLOCK_WEAPON_INBOUND: i64 = 0;
    #[constant] const BLOCK_THREAT_IN_OWN_ENVELOPE: i64 = 1;
    #[constant] const BLOCK_INSIDE_THREAT_ENVELOPE: i64 = 2;
    #[constant] const BLOCK_HOST_LOAD: i64 = 3;
    #[constant] const BLOCK_TIME_SCALE_DEGRADED: i64 = 4;
    #[constant] const BLOCK_NOT_ALL_PLAYERS: i64 = 5;
    #[constant] const BLOCK_MODE_FORBIDS: i64 = 6;
    // `sim_systems::protocol::VerticalTrend`
    #[constant] const TREND_STEADY: i64 = 0;
    #[constant] const TREND_CLIMBING: i64 = 1;
    #[constant] const TREND_DIVING: i64 = 2;
    // `sim_systems::protocol::Outcome`
    #[constant] const OUTCOME_VICTORY: i64 = 0;
    #[constant] const OUTCOME_DEFEAT: i64 = 1;
    #[constant] const OUTCOME_WITHDRAWN: i64 = 2;
    #[constant] const OUTCOME_ABORTED: i64 = 3;
    #[constant] const OUTCOME_DRAW: i64 = 4;
    // `sim_systems::protocol::PrizeDecision`
    #[constant] const PRIZE_SAIL_HOME: i64 = 0;
    #[constant] const PRIZE_STRIP_ON_SPOT: i64 = 1;
    // `sim_systems::editor::Check`
    #[constant] const CHECK_PASS: i64 = 0;
    #[constant] const CHECK_CAUTION: i64 = 1;
    #[constant] const CHECK_FAIL: i64 = 2;
    // `sim_systems::editor::notes::NoteTier`
    #[constant] const NOTE_CANNOT_FLOAT_MOVE_OR_POWER: i64 = 0;
    #[constant] const NOTE_FAILS_BRIEFING_TAG: i64 = 1;
    #[constant] const NOTE_CANNOT_WORK: i64 = 2;
    #[constant] const NOTE_GAUGE_CAUTION: i64 = 3;
    #[constant] const NOTE_ADVISORY: i64 = 4;
    // `sim_systems::editor::suggest::SuggestKind`
    #[constant] const SUGGEST_BALLAST_AND_TRIM: i64 = 0;
    #[constant] const SUGGEST_ARMOUR_LAYOUT: i64 = 1;
    #[constant] const SUGGEST_ENGINE_AND_GENERATOR_SIZING: i64 = 2;
    #[constant] const SUGGEST_FUEL_LOAD: i64 = 3;
    #[constant] const SUGGEST_SENSOR_PLACEMENT: i64 = 4;
    #[constant] const SUGGEST_BULKHEAD_PLACEMENT: i64 = 5;
    #[constant] const SUGGEST_AGGRESSION: i64 = 6;
    #[constant] const SUGGEST_EMCON_TRIGGER_RANGE: i64 = 7;
    // `sim_systems::theatre::Confidence`
    #[constant] const CONFIDENCE_POSSIBLE: i64 = 0;
    #[constant] const CONFIDENCE_PROBABLE: i64 = 1;
    #[constant] const CONFIDENCE_CONFIRMED: i64 = 2;
    // `sim_systems::theatre::saves::SaveGate`
    #[constant] const SAVE_ALLOWED: i64 = 0;
    #[constant] const SAVE_IN_SORTIE: i64 = 1;
    #[constant] const SAVE_NOT_AT_COVE: i64 = 2;
    // `sim_systems::theatre::AlertCause`
    #[constant] const ALERT_CAUSE_OBSERVED_KILL: i64 = 0;
    #[constant] const ALERT_CAUSE_UNOBSERVED_KILL_NOTICED: i64 = 1;
    #[constant] const ALERT_CAUSE_SIGHTING: i64 = 2;
    #[constant] const ALERT_CAUSE_EVIDENCE: i64 = 3;
    #[constant] const ALERT_CAUSE_QUIET_DECAY: i64 = 4;
    #[constant] const ALERT_CAUSE_ACT_START: i64 = 5;
    #[constant] const ALERT_CAUSE_SEXTANT_SUNK: i64 = 6;
    // `View`: Sortie view (L106). Tab switches.
    #[constant] const VIEW_BRIDGE: i64 = 0;
    #[constant] const VIEW_PLOT: i64 = 1;
    // `SessionState`: State of `BpSession`.
    #[constant] const SESSION_IDLE: i64 = 0;
    #[constant] const SESSION_LOADING: i64 = 1;
    #[constant] const SESSION_LOBBY: i64 = 2;
    #[constant] const SESSION_IN_SORTIE: i64 = 3;
    #[constant] const SESSION_ENDED: i64 = 4;
    #[constant] const SESSION_REPLAY: i64 = 5;
    #[constant] const SESSION_FAILED: i64 = 6;
    // `SessionKind`: What the current session is.
    #[constant] const KIND_NONE: i64 = 0;
    #[constant] const KIND_CAMPAIGN: i64 = 1;
    #[constant] const KIND_SKIRMISH: i64 = 2;
    #[constant] const KIND_SANDBOX: i64 = 3;
    #[constant] const KIND_PVP: i64 = 4;
    #[constant] const KIND_SPLASH: i64 = 5;
    #[constant] const KIND_TUTORIAL: i64 = 6;
    #[constant] const KIND_REPLAY: i64 = 7;
    #[constant] const KIND_BENCHMARK: i64 = 8;
    #[constant] const KIND_SCRIPTED: i64 = 9;
    // `NetRole`: This process's network role.
    #[constant] const NET_OFFLINE: i64 = 0;
    #[constant] const NET_HOST: i64 = 1;
    #[constant] const NET_CLIENT: i64 = 2;
    // `ConnState`: Connection state of one player or of the local link (L1033-1039).
    #[constant] const CONN_CLOSED: i64 = 0;
    #[constant] const CONN_CONNECTING: i64 = 1;
    #[constant] const CONN_CONNECTED: i64 = 2;
    #[constant] const CONN_WARNING: i64 = 3;
    #[constant] const CONN_DROPPED: i64 = 4;
    #[constant] const CONN_REFUSED: i64 = 5;
    // `PortMapState`: Automatic port-mapping result (L994-999).
    #[constant] const PORTMAP_IDLE: i64 = 0;
    #[constant] const PORTMAP_TRYING: i64 = 1;
    #[constant] const PORTMAP_MAPPED: i64 = 2;
    #[constant] const PORTMAP_DOUBLE_NAT: i64 = 3;
    #[constant] const PORTMAP_FAILED: i64 = 4;
    /// A lease renewal failed after an earlier success; the mapping may lapse (`net::portmap::MappingOutcome::RenewFailed`).
    #[constant] const PORTMAP_RENEW_FAILED: i64 = 5;
    // `hull_mesh::QualityTier`: Graphics tier (L1977-1982).
    #[constant] const QUALITY_LOW: i64 = 0;
    #[constant] const QUALITY_MEDIUM: i64 = 1;
    #[constant] const QUALITY_HIGH: i64 = 2;
    #[constant] const QUALITY_FALLBACK: i64 = 3;
    // `audio::UiSfx`: interface sounds for `AudioFeed.ui_sound` (files `res://audio/gen/ui_<name>.wav`).
    #[constant] const UI_SOUND_CLICK: i64 = 0;
    #[constant] const UI_SOUND_SELECT: i64 = 1;
    /// Part or kit placed in the dry dock.
    #[constant] const UI_SOUND_PLACE: i64 = 2;
    #[constant] const UI_SOUND_REMOVE: i64 = 3;
    /// Placement refused or command rejected.
    #[constant] const UI_SOUND_INVALID: i64 = 4;
    #[constant] const UI_SOUND_CONFIRM: i64 = 5;
    /// Ghost rotated.
    #[constant] const UI_SOUND_ROTATE: i64 = 6;
    /// Kit socket snapped.
    #[constant] const UI_SOUND_SNAP: i64 = 7;
    /// Time compression stepped.
    #[constant] const UI_SOUND_TIME_RATE: i64 = 8;
    #[constant] const UI_SOUND_PAUSE: i64 = 9;
    #[constant] const UI_SOUND_RESUME: i64 = 10;
    #[constant] const UI_SOUND_TUTORIAL_STEP: i64 = 11;
    #[constant] const UI_SOUND_OBJECTIVE_DONE: i64 = 12;
    /// Design saved.
    #[constant] const UI_SOUND_SAVED: i64 = 13;
    // `net::msg::LinkQuality`: bandwidth quality on the connection panel (L1033).
    #[constant] const NET_QUALITY_GOOD: i64 = 0;
    #[constant] const NET_QUALITY_FAIR: i64 = 1;
    #[constant] const NET_QUALITY_POOR: i64 = 2;
    // `net::msg::DropWarning`: a drop-rule condition that has held for 5 s (L1035).
    #[constant] const DROP_WARNING_HIGH_PING: i64 = 0;
    #[constant] const DROP_WARNING_PACKET_LOSS: i64 = 1;
    #[constant] const DROP_WARNING_FALLING_BEHIND: i64 = 2;
    // `Palette`: UI palette (L1622). Chart screens always use the chart tokens.
    #[constant] const PALETTE_DUSK: i64 = 0;
    #[constant] const PALETTE_HIGH_CONTRAST: i64 = 1;
    // `EditorTool`: Active dry-dock tool.
    #[constant] const TOOL_SELECT: i64 = 0;
    #[constant] const TOOL_PLACE_PART: i64 = 1;
    #[constant] const TOOL_PLACE_KIT: i64 = 2;
    #[constant] const TOOL_HULL_HANDLES: i64 = 3;
    #[constant] const TOOL_PLANES: i64 = 4;
    #[constant] const TOOL_ARMOUR: i64 = 5;
    #[constant] const TOOL_PAINT: i64 = 6;
    #[constant] const TOOL_MARKING: i64 = 7;
    #[constant] const TOOL_SCULPT: i64 = 8;
    #[constant] const TOOL_ROUTE: i64 = 9;
    // `Symmetry`: Placement symmetry (L233).
    #[constant] const SYMMETRY_NONE: i64 = 0;
    #[constant] const SYMMETRY_MIRROR: i64 = 1;
    #[constant] const SYMMETRY_RADIAL: i64 = 2;
    // `EditorMode`: Why the dry dock is open; picks part availability, caps and cost display.
    #[constant] const DOCK_SANDBOX: i64 = 0;
    #[constant] const DOCK_CAMPAIGN: i64 = 1;
    #[constant] const DOCK_LOBBY: i64 = 2;
    #[constant] const DOCK_ENEMY_CLASS: i64 = 3;
    #[constant] const DOCK_TUTORIAL: i64 = 4;
    // `LogKind`: HUD log entry type (rule 15, L1671).
    #[constant] const LOG_TRACK: i64 = 0;
    #[constant] const LOG_ENGAGEMENT: i64 = 1;
    #[constant] const LOG_DAMAGE: i64 = 2;
    #[constant] const LOG_OFFICER: i64 = 3;
    #[constant] const LOG_CONTROL: i64 = 4;
    #[constant] const LOG_ALERT: i64 = 5;
    #[constant] const LOG_PACING: i64 = 6;
    #[constant] const LOG_OBJECTIVE: i64 = 7;
    #[constant] const LOG_NETWORK: i64 = 8;
    #[constant] const LOG_ACE: i64 = 9;
    #[constant] const LOG_SYSTEM: i64 = 10;
    // `BenchStage`: Benchmark stages (L1392-1399).
    #[constant] const BENCH_IDLE: i64 = 0;
    #[constant] const BENCH_REFERENCE_BATTLE: i64 = 1;
    #[constant] const BENCH_COOP_REFERENCE: i64 = 2;
    #[constant] const BENCH_MINIMUM_SWARM: i64 = 3;
    #[constant] const BENCH_EDITOR: i64 = 4;
    #[constant] const BENCH_SIM_CALIB: i64 = 5;
    #[constant] const BENCH_SOAK: i64 = 6;
    #[constant] const BENCH_DONE: i64 = 7;
    // `JobState`: State of a background-worker job (gauges, trials, Fit Check, Quartermaster, splash pre-build).
    #[constant] const JOB_IDLE: i64 = 0;
    #[constant] const JOB_RUNNING: i64 = 1;
    #[constant] const JOB_DONE: i64 = 2;
    #[constant] const JOB_FAILED: i64 = 3;
    #[constant] const JOB_STALE: i64 = 4;
    // `TutorialState`: State of `BpTutorial`.
    #[constant] const TUTORIAL_OFF: i64 = 0;
    #[constant] const TUTORIAL_RUNNING: i64 = 1;
    #[constant] const TUTORIAL_FINISHED: i64 = 2;
    // `CampaignPhase`: Where the campaign loop stands (L62-73).
    #[constant] const PHASE_NONE: i64 = 0;
    #[constant] const PHASE_CHART: i64 = 1;
    #[constant] const PHASE_ADVANCING: i64 = 2;
    #[constant] const PHASE_BRIEFING: i64 = 3;
    #[constant] const PHASE_DRY_DOCK: i64 = 4;
    #[constant] const PHASE_SORTIE: i64 = 5;
    #[constant] const PHASE_AFTER_ACTION: i64 = 6;
    #[constant] const PHASE_OVER: i64 = 7;
    // `VesselStatus` without its loss cause (L537).
    #[constant] const VESSEL_ACTIVE: i64 = 0;
    #[constant] const VESSEL_DISABLED: i64 = 1;
    #[constant] const VESSEL_LOST: i64 = 2;
    #[constant] const VESSEL_WRECK: i64 = 3;
    // `EngageStatus` kind per mount (L407).
    #[constant] const ENGAGE_IDLE: i64 = 0;
    #[constant] const ENGAGE_ENGAGING: i64 = 1;
    #[constant] const ENGAGE_HOLDING: i64 = 2;
    // `UncertaintyShape` kind (L1662).
    #[constant] const SHAPE_CIRCLE: i64 = 0;
    #[constant] const SHAPE_ELLIPSE: i64 = 1;
    #[constant] const SHAPE_WEDGE: i64 = 2;
    // `ControlRole` kind; mount and group roles carry an index (L953, L1665).
    #[constant] const ROLE_HELM: i64 = 0;
    #[constant] const ROLE_SENSORS: i64 = 1;
    #[constant] const ROLE_MOUNT: i64 = 2;
    #[constant] const ROLE_WEAPON_GROUP: i64 = 3;
    // `PartReadout` kind.
    #[constant] const READOUT_NONE: i64 = 0;
    #[constant] const READOUT_ROUNDS: i64 = 1;
    #[constant] const READOUT_CELLS: i64 = 2;
    #[constant] const READOUT_OUTPUT: i64 = 3;
    #[constant] const READOUT_FUEL: i64 = 4;
    #[constant] const READOUT_RADIATING: i64 = 5;
    #[constant] const READOUT_RUNNING: i64 = 6;
    #[constant] const READOUT_CLOSED: i64 = 7;
    #[constant] const READOUT_ABOARD: i64 = 8;
    // `TeamOrder` kind (damage-control teams).
    #[constant] const TEAM_IDLE: i64 = 0;
    #[constant] const TEAM_FIGHT_FIRE: i64 = 1;
    #[constant] const TEAM_PATCH: i64 = 2;
    #[constant] const TEAM_JURY_RIG: i64 = 3;
    #[constant] const TEAM_REPAIR: i64 = 4;
    // `OffboardOrder` kind.
    #[constant] const OFFBOARD_LAUNCH: i64 = 0;
    #[constant] const OFFBOARD_RECALL: i64 = 1;
    #[constant] const OFFBOARD_ORBIT: i64 = 2;
    #[constant] const OFFBOARD_SHADOW: i64 = 3;
    #[constant] const OFFBOARD_WAYPOINT: i64 = 4;
    // `Interrupt` kind (why theatre time stopped, L741).
    #[constant] const INTERRUPT_ARRIVED: i64 = 0;
    #[constant] const INTERRUPT_REFIT_DONE: i64 = 1;
    #[constant] const INTERRUPT_WAIT_DONE: i64 = 2;
    #[constant] const INTERRUPT_OPERATION_OFFERED: i64 = 3;
    #[constant] const INTERRUPT_FORCED_OPERATION: i64 = 4;
    #[constant] const INTERRUPT_HUNTER_GROUP_WARNING: i64 = 5;
    #[constant] const INTERRUPT_CARD_ARRIVING: i64 = 6;
    #[constant] const INTERRUPT_CLOCK_LOW: i64 = 7;
    #[constant] const INTERRUPT_ACT_ENDED: i64 = 8;
    #[constant] const INTERRUPT_HULL_READY: i64 = 9;
    #[constant] const INTERRUPT_PAUSED: i64 = 10;
    // `TheatreAction` kind (chart-table proposals).
    #[constant] const THEATRE_ACTION_TRANSIT: i64 = 0;
    #[constant] const THEATRE_ACTION_REFIT: i64 = 1;
    #[constant] const THEATRE_ACTION_WAIT: i64 = 2;
    #[constant] const THEATRE_ACTION_ACCEPT_OPERATION: i64 = 3;
    #[constant] const THEATRE_ACTION_DECLINE_OPERATION: i64 = 4;
    #[constant] const THEATRE_ACTION_SPEND_INTEL: i64 = 5;
    #[constant] const THEATRE_ACTION_REBUILD_HULL: i64 = 6;
    // `IntelSpend` kind (L811-822).
    #[constant] const INTEL_DELAY_CARD: i64 = 0;
    #[constant] const INTEL_CANCEL_CARD: i64 = 1;
    #[constant] const INTEL_REVEAL_CONVOYS: i64 = 2;
    #[constant] const INTEL_REVEAL_ACE: i64 = 3;
    // `OperationKind` kind.
    #[constant] const OPERATION_TEMPLATE: i64 = 0;
    #[constant] const OPERATION_STORY: i64 = 1;
    #[constant] const OPERATION_ACE_DUEL: i64 = 2;
    #[constant] const OPERATION_CAPSTONE: i64 = 3;
    #[constant] const OPERATION_TUTORIAL: i64 = 4;
    // `QmResult` kind (L801-807).
    #[constant] const QM_ALREADY_FITS: i64 = 0;
    #[constant] const QM_REFIT: i64 = 1;
    #[constant] const QM_NO_LEGAL_REFIT: i64 = 2;
    // `NodeKind` group of a control-graph node (L338-349).
    #[constant] const NODE_SOURCE: i64 = 0;
    #[constant] const NODE_MATH: i64 = 1;
    #[constant] const NODE_STAGE: i64 = 2;
    #[constant] const NODE_SINK: i64 = 3;
    #[constant] const NODE_SCRIPT: i64 = 4;
    // `SourceMask` bits: sensor families feeding a track. Combine with `|`.
    #[constant] const SOURCE_SEARCH_RADAR: i64 = 1;
    #[constant] const SOURCE_FIRE_CONTROL: i64 = 2;
    #[constant] const SOURCE_SEEKER: i64 = 4;
    #[constant] const SOURCE_ESM: i64 = 8;
    #[constant] const SOURCE_PASSIVE_SONAR: i64 = 16;
    #[constant] const SOURCE_ACTIVE_SONAR: i64 = 32;
    #[constant] const SOURCE_VISUAL: i64 = 64;
    #[constant] const SOURCE_CAMERA: i64 = 128;
    #[constant] const SOURCE_DATALINK: i64 = 256;
    #[constant] const SOURCE_OFFBOARD: i64 = 512;
    #[constant] const SOURCE_BRIEFING: i64 = 1024;
    // Dry-dock overlay bits (L168). Combine with `|`.
    #[constant] const OVERLAY_WATERLINE: i64 = 1;
    #[constant] const OVERLAY_CG_CB: i64 = 2;
    #[constant] const OVERLAY_ARCS: i64 = 4;
    #[constant] const OVERLAY_BLIND_SECTORS: i64 = 8;
    #[constant] const OVERLAY_CONDUITS: i64 = 16;
    #[constant] const OVERLAY_HEATMAP: i64 = 32;
    #[constant] const OVERLAY_XRAY: i64 = 64;
    #[constant] const OVERLAY_COMPARTMENTS: i64 = 128;
    #[constant] const OVERLAY_SIGNATURE: i64 = 256;
    #[constant] const OVERLAY_GHOST: i64 = 512;
    // Plot-table layer bits (L106, rule 14). Combine with `|`.
    #[constant] const PLOT_TRACKS: i64 = 1;
    #[constant] const PLOT_UNCERTAINTY: i64 = 2;
    #[constant] const PLOT_ENVELOPES: i64 = 4;
    #[constant] const PLOT_EMITTERS: i64 = 8;
    #[constant] const PLOT_ROUTE: i64 = 16;
    #[constant] const PLOT_RINGS: i64 = 32;
    #[constant] const PLOT_GRATICULE: i64 = 64;
    #[constant] const PLOT_HISTORY: i64 = 128;
    #[constant] const PLOT_LAND: i64 = 256;
    #[constant] const PLOT_OBJECTIVES: i64 = 512;
    #[constant] const PLOT_MINES: i64 = 1024;
    #[constant] const PLOT_ALLIES: i64 = 2048;
    #[constant] const PLOT_LABELS: i64 = 4096;
    // Sentinels.
    /// "No track", "no part", "no player": every id getter uses -1 for none.
    #[constant] const NONE: i64 = -1;
    /// Simulation tick rate in Hz (L1413).
    #[constant] const TICK_HZ: i64 = 60;
}
