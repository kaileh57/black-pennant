//! Enemy classes and fleet templates. DESIGN.md L756-770, L832-845 (co-op variants).
//! Every enemy class is a blueprint built in the player's editor and validated in CI; designs are never
//! generated at runtime. About 20 classes in 5 tiers at first release, about 60 by 1.0.
use crate::enums::{Act, Tier};
use crate::ids::{ClassId, FleetTemplateId, MunitionId, TextKey};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ClassRole { Submarine, PatrolBoat, FastAttack, Corvette, Frigate, Destroyer, Capital, Picket, Minesweeper, Freighter, Tanker, Tender, Civilian, Drone }

/// When a Combine hull strikes its colours (counts as Disabled, L537). DECISION (orchestrator ruling 1, 20 November
/// 2026, L537 "the Combine surrender rule in its behaviour tree"): a hull under fire, or hit in the last 60 s,
/// strikes when for a 5 s confirm time (a) it has lost a set fraction of its original cells, (b) no weapon of it
/// works (destroyed, unpowered or out of ammunition; an unarmed hull has none) while a hostile is inside that
/// hostile's weapon range, or (c) it cannot make way and is on fire or flooding. Player hulls never strike; aces
/// never strike. Evaluated by the captain (`sim_systems::ai::unit`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum SurrenderRule {
    /// The hull never strikes (aces, and any class the data flags).
    Never,
    /// The Combine rule with the campaign's cells-lost fraction (`CampaignRules::strike_cells_lost`).
    #[default]
    Combine,
    /// The Combine rule with this class's own cells-lost fraction (0 to 1).
    Strike { cells_lost: f64 },
}
impl SurrenderRule {
    /// The rule's cells-lost fraction, `None` for a hull that never strikes. `default` is the campaign's value for
    /// `Combine`; a malformed fraction never strikes.
    pub fn cells_lost(self, default: f64) -> Option<f64> {
        let f = match self { SurrenderRule::Never => return None, SurrenderRule::Combine => default, SurrenderRule::Strike { cells_lost } => cells_lost };
        (f.is_finite() && f > 0.0).then(|| f.min(1.0))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnemyClassDef {
    pub id: ClassId,
    pub name: TextKey,
    /// Path of the class blueprint under `blueprints/`, e.g. `combine/act1/patrol_boat_a.bp.ron`.
    pub blueprint: String,
    pub role: ClassRole,
    pub tier: Tier,
    pub act: Act,
    /// Behaviour tree key resolved by sim-systems' enemy AI.
    pub behaviour: String,
    /// Doctrine-set engagement range in metres; `None` = weapon effective range.
    #[serde(default)]
    pub engagement_range_m: Option<f64>,
    /// Heavier variants used for co-op scaling and doctrine upgrades (armour tiers, loadouts), L834.
    #[serde(default)]
    pub variants: Vec<ClassId>,
    #[serde(default)]
    pub surrender: SurrenderRule,
    /// Aircraft, helicopters or drones the class can launch (Act 2 onward for aircraft).
    #[serde(default)]
    pub air_wing: Vec<(MunitionId, u8)>,
    /// Act 3: an AI-only submarine. It is an ordinary cell hull built in enemy-class mode that keeps an ordered
    /// depth (see `sim_core::parts::VesselControls::depth_m`); depth charges damage it by the cell rules, which
    /// is what the depth-charge gate's "disables" and whole-cell radius steps assume (L451, L618).
    #[serde(default)]
    pub submersible: bool,
    /// True for the up to 4 hulls over 90 m an operation may field (L126).
    #[serde(default)]
    pub capital: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FleetKind { TaskGroup, Patrol, Convoy, Escort, HunterKiller, DecoyConvoy, Garrison, AirPatrol, SubmarinePatrol }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Formation { #[default] Column, LineAbreast, Wedge, Screen, Box }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FleetSlot {
    /// Classes the generator may draw for this slot (seeded).
    pub classes: Vec<ClassId>,
    pub min: u8,
    pub max: u8,
}

/// A task group is 5 hulls whose combined displacement meets the operation tonnage multiple (L747).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FleetTemplate {
    pub id: FleetTemplateId,
    pub kind: FleetKind,
    pub act: Act,
    pub slots: Vec<FleetSlot>,
    #[serde(default)]
    pub formation: Formation,
    /// Small bodies that accompany the group: aircraft (Act 2).
    #[serde(default)]
    pub bodies: Vec<(MunitionId, u8)>,
}
