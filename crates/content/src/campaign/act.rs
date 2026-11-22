//! Scope of each act. DESIGN.md L142-151 (starter hulls), L801-807 (tiers), L866-875 (scope per act).
use crate::enums::{Act, Phase, Tier};
use crate::ids::{AceId, OperationId, RegionId, TextKey, TheatreNodeId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActDef {
    pub act: Act,
    pub phase: Phase,
    pub name: TextKey,
    /// Stock blueprint paths under `blueprints/` offered as starter hulls from this act on (L144-149).
    pub starters: Vec<String>,
    /// Highest part tier open at act start, and tiers opened inside the act by named operations.
    pub start_tier: Tier,
    #[serde(default)]
    pub tier_unlocks: Vec<(OperationId, Tier)>,
    pub regions: Vec<RegionId>,
    pub story_ops: Vec<OperationId>,
    pub ace_duels: Vec<OperationId>,
    pub capstone: OperationId,
    pub aces: Vec<AceId>,
    /// Coves open at a normal start, and the one closed in the retreat configuration (L750).
    pub coves: Vec<TheatreNodeId>,
    pub retreat_closed_cove: Option<TheatreNodeId>,
    /// True for Act 4: finishing its capstone opens Open Theatre in the same save (L875).
    #[serde(default)]
    pub opens_open_theatre: bool,
}
