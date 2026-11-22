//! Unit `content-validate`. Server-side validation of blueprints and kits: schema, part legality, ruleset
//! limits, bounds and connectivity. The authoritative host re-validates everything a client sends and
//! recomputes every stat itself. DESIGN.md L2081, L945, L126, L151, L2150.
use crate::blueprint::Blueprint;
use crate::catalogue::Catalogue;
use crate::cell::CellPos;
use crate::ids::{PartId, PartSlot, TextKey};
use crate::ruleset::Ruleset;
use crate::stats::BlueprintStats;
use serde::{Deserialize, Serialize};

mod blueprint;
mod graph;
mod kit;
#[cfg(test)]
mod tests;

/// One reason a blueprint is refused. Each maps to a translation key with arguments for the UI.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Issue {
    /// Longer than the mode allows; player modes refuse over 90 m and show the reason (L126).
    TooLong { length_m: f64, max_m: f64 },
    TooShort { length_m: f64, min_m: f64 },
    GridOutOfBounds,
    OverTonnage { tonnage_t: f64, cap_t: f64 },
    OverComplexity { points: f64, cap: f64 },
    UnknownPart { part: PartId },
    PartNotAvailable { part: PartId },
    PartOverLimit { part: PartId, count: u16, limit: u16 },
    PartOutOfBounds { slot: PartSlot },
    PartsOverlap { a: PartSlot, b: PartSlot },
    PartBadPlacement { slot: PartSlot },
    /// Structure is not one face-connected piece.
    Disconnected { stray_cell: CellPos },
    BadPaintIndex { cell: CellPos },
    TooManyPaintEntries(usize),
    TooManyRules(usize),
    /// Graph edits or forked officers under the standard ruleset (L313).
    LayerNotAllowed,
    ScriptsNotAllowed,
    BadGraph { detail: String },
    BadKitBinding { detail: String },
    BadShell { detail: String },
}
impl Issue {
    /// Translation key for the message, e.g. `validate.too_long`. Unit: `content-validate`.
    pub fn text_key(&self) -> TextKey {
        TextKey::new(match self {
            Issue::TooLong { .. } => "validate.too_long",
            Issue::TooShort { .. } => "validate.too_short",
            Issue::GridOutOfBounds => "validate.grid_out_of_bounds",
            Issue::OverTonnage { .. } => "validate.over_tonnage",
            Issue::OverComplexity { .. } => "validate.over_complexity",
            Issue::UnknownPart { .. } => "validate.unknown_part",
            Issue::PartNotAvailable { .. } => "validate.part_not_available",
            Issue::PartOverLimit { .. } => "validate.part_over_limit",
            Issue::PartOutOfBounds { .. } => "validate.part_out_of_bounds",
            Issue::PartsOverlap { .. } => "validate.parts_overlap",
            Issue::PartBadPlacement { .. } => "validate.part_bad_placement",
            Issue::Disconnected { .. } => "validate.disconnected",
            Issue::BadPaintIndex { .. } => "validate.bad_paint_index",
            Issue::TooManyPaintEntries(_) => "validate.too_many_paint_entries",
            Issue::TooManyRules(_) => "validate.too_many_rules",
            Issue::LayerNotAllowed => "validate.layer_not_allowed",
            Issue::ScriptsNotAllowed => "validate.scripts_not_allowed",
            Issue::BadGraph { .. } => "validate.bad_graph",
            Issue::BadKitBinding { .. } => "validate.bad_kit_binding",
            Issue::BadShell { .. } => "validate.bad_shell",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidationReport {
    pub issues: Vec<Issue>,
    /// Stats recomputed locally; never trusted from the file (L2081).
    pub stats: BlueprintStats,
}
impl ValidationReport { pub fn is_ok(&self) -> bool { self.issues.is_empty() } }

/// What the validator needs to know about the session.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidationContext<'a> {
    pub ruleset: &'a Ruleset,
    /// Parts the player may field (campaign unlocks); `None` = every part in the build.
    pub available: Option<&'a [PartId]>,
    /// Enemy-class mode raises the length limit to 150 m (L126).
    pub enemy_class: bool,
    /// Complexity points from the last trial, if known; `None` skips the complexity cap.
    pub complexity: Option<f64>,
}

/// Validate a parsed blueprint against a ruleset. Deterministic; same result on every platform.
/// Unit: `content-validate`.
pub fn validate_blueprint(bp: &Blueprint, catalogue: &Catalogue, ctx: &ValidationContext<'_>) -> ValidationReport { blueprint::validate_blueprint(bp, catalogue, ctx) }

/// Validate a kit definition (parts exist, sockets inside footprint, graph fragment well-formed).
/// Unit: `content-validate`.
pub fn validate_kit(kit: &crate::kit::KitDef, catalogue: &Catalogue, ruleset: &Ruleset) -> Vec<Issue> { kit::validate_kit(kit, catalogue, ruleset) }

/// Structural check of stored graph content: node ids unique, edges reference existing ports of matching
/// type, no cycles outside latch/timer feedback, at most 32 rules. Unit: `content-validate`.
pub fn validate_graph(overrides: &crate::graph::GraphOverrides, rules: &[crate::graph::Rule]) -> Vec<Issue> { graph::validate_graph(overrides, rules) }
