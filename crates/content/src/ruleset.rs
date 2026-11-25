//! Contract-owned types (no work unit). Lobby rulesets and mode limits.
//! DESIGN.md L73, L313, L957-976 (modes, rulesets, complexity caps), L1031 (player limits).
use crate::enums::{Act, ControlLayer, GameMode};
use crate::ids::PartId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// PvP ruleset enum: `standard` is the default for public lobbies (L313, L969-971).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RulesetKind {
    /// Doctrine presets, sliders and the rules table allowed; forked officers and graph edits banned.
    #[default] Standard,
    /// Forked officers and graphs allowed. Luau still never runs in a PvP lobby.
    Open,
}
impl RulesetKind {
    /// Highest control layer a vessel may use under this ruleset.
    pub fn max_layer(self) -> ControlLayer { match self { Self::Standard => ControlLayer::L2Rules, Self::Open => ControlLayer::L3Graph } }
}

/// Limits a lobby applies to every fielded blueprint. Campaign uses the act's cap and no part limits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Ruleset {
    pub kind: RulesetKind,
    /// Per-hull tonnage cap: one of 150, 400, 900, 1,800 t; PvP default 400 t (L971).
    pub tonnage_cap_t: f64,
    /// Complexity caps in points: 600 per vessel, 3,600 per lobby (L976). 0 = informational only (campaign).
    pub complexity_per_vessel: f64,
    pub complexity_per_lobby: f64,
    /// Per-part copy limits; missing parts fall back to `PartDef::standard_limit`.
    pub part_limits: BTreeMap<PartId, u16>,
    /// Longest hull in metres (90 for players, L126).
    pub max_length_m: f64,
    /// Whether the host lets Luau run in this lobby; never true in PvP (L2119-2121).
    pub luau: bool,
}
impl Default for Ruleset {
    fn default() -> Self { Self { kind: RulesetKind::Standard, tonnage_cap_t: 400.0, complexity_per_vessel: 600.0, complexity_per_lobby: 3_600.0, part_limits: BTreeMap::new(), max_length_m: 90.0, luau: false } }
}
impl Ruleset {
    /// Campaign and co-op: the act's tonnage cap, every layer allowed, complexity informational.
    pub fn campaign(act: Act) -> Self { Self { kind: RulesetKind::Open, tonnage_cap_t: act.hull_cap_t(), complexity_per_vessel: 0.0, complexity_per_lobby: 0.0, part_limits: BTreeMap::new(), max_length_m: 90.0, luau: true } }
}

/// Player-count limits per mode: (first release, 1.0). L957-963.
pub fn max_players(mode: GameMode, at_1_0: bool) -> u8 {
    match mode {
        GameMode::Campaign => if at_1_0 { 4 } else { 2 },
        GameMode::Skirmish => 4,
        GameMode::Sandbox => 8,
        GameMode::PvpElimination => if at_1_0 { 8 } else { 4 },
        GameMode::PvpEscort => 8,
        GameMode::Splash | GameMode::Tutorial | GameMode::Trial => 1,
    }
}
