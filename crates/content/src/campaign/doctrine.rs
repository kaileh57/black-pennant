//! Doctrine cards and the four kill tallies. About 30 authored cards. DESIGN.md L772.
use crate::enums::{Emcon, TimeOfDay};
use crate::ids::{CardId, FleetTemplateId, TextKey};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum KillMethod { Gunfire, Missile, Torpedo, Mine, DepthCharge, Fire, Capture, Collision }

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RangeBand { Close, Medium, Long, BeyondHorizon }

/// One of the four tallies each kill feeds (L772).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TallyKey { Method(KillMethod), Range(RangeBand), Time(TimeOfDay), Emission(Emcon) }

/// What an active card changes in its region. Fields at default change nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CardEffect {
    /// Extra escorts or pickets added to each operation's force.
    pub add_fleet: Option<FleetTemplateId>,
    /// Hulls swap to their heavier class variants.
    pub use_variants: bool,
    pub enemy_emcon: Option<Emcon>,
    pub patrol_density_factor: Option<f64>,
    pub engagement_range_factor: Option<f64>,
    /// Behaviour flags read by enemy AI, e.g. `zigzag`, `darken_ship`, `sweep_mines`.
    pub behaviour_flags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctrineCardDef {
    pub id: CardId,
    pub name: TextKey,
    pub description: TextKey,
    /// The tally that queues this card when it reaches the threshold.
    pub counters: TallyKey,
    pub effect: CardEffect,
}
