//! Contract-owned types (no work unit). The stored form of control-graph content that blueprints and kits
//! carry: pinned nodes, replaced stages, L2 rules, L0/L1 officer settings, doctrine overrides and L4 script
//! sources. DESIGN.md L277-397 (automatic systems), L479-494 (doctrine), L2077 (blueprint "rest").
//!
//! sim-systems owns the generator, the bytecode VM and node semantics. It compiles these types; it must not
//! define a second stored form. Generated nodes are never stored: the graph regenerates whenever the design
//! changes and only pinned (player-edited) nodes survive (L320).
use crate::enums::{DoctrinePreset, Emcon, IdRung, OfficerRole, Stance, TrackGrade};
use crate::ids::{CompartmentId, GraphNodeId, PartSlot};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Ids at or above this are player nodes; ids below are assigned by the generator from (officer, stage, part
/// slot) so an edge from a pinned node to a generated node survives regeneration.
pub const USER_NODE_BASE: u32 = 1 << 24;
/// Most rules one ship may carry (L336).
pub const MAX_RULES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PortType { Bool, Scalar, Angle, Vec3, Text, Stance, Emcon, Track, TrackList, DetectionList, PartState, Orders }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value { None, Bool(bool), Scalar(f64), Vec3([f64; 3]), Text(String), Stance(Stance), Emcon(Emcon), Preset(DoctrinePreset), Rung(IdRung) }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cmp { Lt, Le, Gt, Ge, Eq, Ne }

/// Sources: readable, not writable (L344).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SourceKind { OwnShip, PartState(PartSlot), Detections(PartSlot), TrackList, TheatreOrders, Constant(Value) }

/// Math and logic nodes (L345).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MathOp { Add, Sub, Mul, Div, Min, Max, Abs, Neg, Clamp, Compare(Cmp), And, Or, Not, Select, Latch, Timer, Counter, Pid, LowPass, AlphaBeta, VecMake, VecSplit, VecAdd, VecSub, VecScale, VecDot, VecCross, VecLength, VecNormalize, BearingTo, RangeTo }

/// Officer stages; any one may be replaced by a player subgraph while the rest stay automatic (L346, L349).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum OfficerStage { HelmAutopilot, StationKeeping, Evasion, TrackFusion, ThreatRanking, MountAssignment, BallisticSolver, PnGuidance, LoadShedder, DamageControlPlanner, EmissionController }
impl OfficerStage {
    pub fn officer(self) -> OfficerRole {
        match self {
            Self::HelmAutopilot | Self::StationKeeping | Self::Evasion => OfficerRole::Helm,
            Self::TrackFusion | Self::EmissionController => OfficerRole::Sensors,
            Self::ThreatRanking | Self::MountAssignment | Self::BallisticSolver | Self::PnGuidance => OfficerRole::Gunnery,
            Self::LoadShedder => OfficerRole::Engineer,
            Self::DamageControlPlanner => OfficerRole::DamageControl,
        }
    }
}

/// Sinks: readable and writable (L347).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SinkKind {
    HelmHeading, HelmSpeed,
    MountAimPoint(PartSlot), MountFirePermission(PartSlot),
    MissileLaunch(PartSlot), MissileTarget(PartSlot),
    EmissionState(PartSlot),
    PowerPriority(PartSlot),
    Pump(PartSlot), Door(PartSlot), FloodValve(CompartmentId),
    TrackStanceOverride, TrackIdOverride,
    /// Ship-wide settings a rule may switch (L0 preset, EMCON).
    ShipPreset, ShipEmcon,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    Source(SourceKind),
    Math(MathOp),
    Stage(OfficerStage),
    Sink(SinkKind),
    /// Luau code with typed ports, index into [`GraphOverrides::scripts`]. Phase L4 (L332, L355-390).
    Script(u16),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphNode {
    pub id: GraphNodeId,
    pub kind: NodeKind,
    /// Named parameters (gains, limits). Keys are stable identifiers defined by the node library.
    pub params: BTreeMap<String, Value>,
    /// Editor layout position; no meaning to the VM.
    pub pos: [f32; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PortRef { pub node: GraphNodeId, pub port: u8 }

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GraphEdge { pub from: PortRef, pub to: PortRef }

/// A graph or graph fragment: a kit carries one so a placed kit already aims and fires (L140).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ControlGraph { pub nodes: Vec<GraphNode>, pub edges: Vec<GraphEdge> }

/// A stage the player replaced with a subgraph (L346).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageReplacement { pub stage: OfficerStage, pub subgraph: ControlGraph }

/// L4 Luau source for one script node. Arrives disabled in any imported file or share string (L2123).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptSource { pub name: String, pub source: String, pub inputs: Vec<(String, PortType)>, pub outputs: Vec<(String, PortType)>, pub enabled: bool }

/// Everything above L1 that a blueprint stores (L3 pins and replacements, L4 scripts). Banned under the standard
/// PvP ruleset (L313).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GraphOverrides {
    /// Pinned nodes and every edge touching one.
    pub pinned: ControlGraph,
    pub replaced: Vec<StageReplacement>,
    pub scripts: Vec<ScriptSource>,
}
impl GraphOverrides { pub fn is_empty(&self) -> bool { self.pinned.nodes.is_empty() && self.replaced.is_empty() && self.scripts.is_empty() } }

/// Which contacts a rule operand looks at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TrackFilter { pub min_stance: Stance, pub domain: Option<crate::enums::Domain>, pub min_grade: TrackGrade }

/// A number a rule can read.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Operand { Const(f64), OwnSpeedKn, OwnHeelDeg, FuelFraction, PowerMargin, HullHealth, NearestContactRangeM(TrackFilter), ContactCount(TrackFilter), InboundWeapons, PartHealth(PartSlot), AmmoFraction(PartSlot), SortieTimeS }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Condition { Compare { lhs: Operand, cmp: Cmp, rhs: Operand }, All(Vec<Condition>), Any(Vec<Condition>), Not(Box<Condition>) }

/// One L2 when/then rule. Rules evaluate top to bottom at 30 Hz; a matching rule overrides the officer output
/// on its sink for as long as the condition holds; the nearer-top rule wins a contested sink (L336).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule { pub when: Condition, pub sink: SinkKind, pub value: RuleValue, pub enabled: bool }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RuleValue { Set(Value), NearestContact(TrackFilter) }

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum WeaponClass { Gun, AntiShipMissile, Sam, PointDefenceGun, PointDefenceMissile, Torpedo, DepthCharge, Decoy }

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TargetCategory { Surface, Air, InboundWeapon, Subsurface }

/// One doctrine entry per (weapon, target category); inherits fleet, then ship, then mount (L481-487).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctrineEntry {
    pub weapon: WeaponClass,
    pub target: TargetCategory,
    pub min_stance: Stance,
    pub min_id: IdRung,
    pub min_grade: TrackGrade,
    pub salvo: u8,
    /// `None` = the Punch gauge's effective range clamped to the class cap (L489).
    pub effective_range_m: Option<f64>,
}

/// Jobs on the damage-control repair list in default order (L716-725). Players may reorder (L396).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RepairPriority { MagazineFires, BreachesBelowWaterline, OtherFires, ConduitsToCritical, OtherConduits, PartRepairs }
impl RepairPriority { pub const DEFAULT_ORDER: [RepairPriority; 6] = [Self::MagazineFires, Self::BreachesBelowWaterline, Self::OtherFires, Self::ConduitsToCritical, Self::OtherConduits, Self::PartRepairs]; }

/// Mount training and elevation limits (auto-set by ray sweep, L205).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MountLimits { pub train_min_deg: f64, pub train_max_deg: f64, pub elevation_min_deg: f64, pub elevation_max_deg: f64 }

/// L0, L1 and tuning state stored with a design. Every field left at `None` or empty is "auto" and shows the
/// A chip; a stored value shows the hand glyph (L351).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OfficerSettings {
    /// L0 preset for the ship and per-mount overrides (L328).
    pub preset: DoctrinePreset,
    pub mount_presets: BTreeMap<PartSlot, DoctrinePreset>,
    /// L1 sliders by stable key, e.g. `gunnery.aggression`, `helm.evasion` (3 to 6 per behaviour, L329).
    pub sliders: BTreeMap<String, f64>,
    pub emcon: Emcon,
    pub burst_trigger_range_m: Option<f64>,
    pub self_defence_range_m: Option<f64>,
    /// Ship-level and per-mount doctrine overrides.
    pub doctrine: Vec<DoctrineEntry>,
    pub mount_doctrine: BTreeMap<PartSlot, Vec<DoctrineEntry>>,
    /// Empty = [`RepairPriority::DEFAULT_ORDER`].
    pub repair_order: Vec<RepairPriority>,
    /// Power priority overrides 1..=5 per consumer (L287).
    pub power_priority: BTreeMap<PartSlot, u8>,
    /// Hand-set mount limits; absent = auto-set.
    pub mount_limits: BTreeMap<PartSlot, MountLimits>,
    /// Search radar rate overrides, 2..=10 Hz (L412).
    pub radar_hz: BTreeMap<PartSlot, f64>,
}
