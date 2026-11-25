//! Aces: 7 named Combine captains with a signature ship, doctrine and radio callsign. Returns are authored and
//! fixed; no enemy remembers individual player actions. Lines are text with radio effects, never voiced.
//! DESIGN.md L774.
use crate::enums::Act;
use crate::ids::{AceId, CardId, ClassId, OperationId, TextKey};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AceCue { Arrives, SightsPlayer, FiresFirst, TakesDamage, PlayerDamaged, Withdraws, Defeated, Captured }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AceLine { pub cue: AceCue, pub text: TextKey }

/// One authored appearance arc: 2 per act in Acts 1-3, 1 in Act 4.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AceArc { pub act: Act, pub operations: Vec<OperationId>, pub lines: Vec<AceLine> }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AceDef {
    pub id: AceId,
    pub name: TextKey,
    pub callsign: TextKey,
    pub ship: ClassId,
    /// Doctrine cards always active when the ace leads a group.
    #[serde(default)]
    pub doctrine: Vec<CardId>,
    pub arcs: Vec<AceArc>,
}
