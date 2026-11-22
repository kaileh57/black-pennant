//! Unit `sys-suggest`. Suggest buttons and the auto-set qualification record. DESIGN.md L200-212.
//!
//! Rule: the editor auto-sets a value only when a headless test proves it correct; otherwise it shows a
//! manual control plus a Suggest button. Auto-set at launch, each behind a named property test: conduit
//! routes; default power priorities; mount training and elevation limits; wiring inside a kit; ballistic
//! solver tables. Suggest at launch: ballast and trim, armour layout, engine and generator sizing, fuel load,
//! sensor placement, bulkhead placement, the L1 Aggression slider and the EMCON trigger range.
//! A failing auto-set value drops to Suggest for that release (manual control, Suggest, and a Ship's Note
//! naming the failing case) and is promoted back only after 30 consecutive clean CI days.
//!
//! # What each Suggest proposes (choices the spec leaves open)
//! - Ballast and trim: loads on the fitted ballast parts, then (when they cannot do it) up to four ballast
//!   parts added one at a time, each at the low spot along the hull where it does most good with its own
//!   empty mass counted, that bring GM to the middle of the Pass band (9.5% of beam with the default 4% and
//!   15% thresholds) when it is outside it, and the centre of gravity to where the hull floats with no trim
//!   and no heel (measured by probing the hydrostatics, not read off the trimmed centre of buoyancy). Solved
//!   as a bounded least-squares problem on the three moments, then each stage of the plan is checked on the
//!   real hydrostatics and the model's aim corrected, for three rounds; the stage that floats best is
//!   offered, and only when it removes a quarter of the error (trim and heel past 0.5 degrees, GM outside
//!   the band), so the button never proposes a token or harmful change.
//! - Armour layout: a belt from 1 m below to 2 m above the design waterline along the vital spaces
//!   (magazines, engines, generators, consoles) plus the deck over each magazine, at the heaviest of 40, 15
//!   or 5 mm that adds no more than 8% of light-ship mass.
//! - Engine and generator sizing: generation at least 110% of combat demand (adding the smallest generator
//!   that covers the shortfall, or removing a surplus one when one fewer still leaves 110%), and engine shaft
//!   power matched to what the propulsors can absorb (between 80% and 105%) by swapping one engine.
//! - Fuel load: every tank to the fullest level, in 5% steps, that keeps reserve buoyancy at 20% or more.
//! - Sensor placement: the main search radar (or its mast kit) moved to the weather-deck spot on the
//!   centreline that maximises its horizon against a sea-skimmer weighted by sector coverage, when that beats
//!   the current spot by 5%.
//! - Bulkhead placement: a watertight transverse bulkhead splitting the longest below-waterline compartment
//!   when it is longer than 20% of the hull and 3 m, at the slice nearest its middle that cuts no part.
//! - Aggression: `officers::tuning::suggest_aggression`. EMCON trigger range:
//!   `emcon::suggest_trigger_range_m`.
use super::BlueprintEdit;
use crate::prelude::*;
use crate::ship::ShipDesign;
use content::blueprint::Blueprint;

pub(crate) mod compute;
mod mutate;
pub(crate) mod place;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SuggestKind { BallastAndTrim, ArmourLayout, EngineAndGeneratorSizing, FuelLoad, SensorPlacement, BulkheadPlacement, Aggression, EmconTriggerRange }
impl SuggestKind {
    /// Every kind, in button order.
    pub const ALL: [SuggestKind; 8] = [Self::BallastAndTrim, Self::ArmourLayout, Self::EngineAndGeneratorSizing, Self::FuelLoad, Self::SensorPlacement, Self::BulkheadPlacement, Self::Aggression, Self::EmconTriggerRange];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AutoSetValue { ConduitRoutes, PowerPriorities, MountLimits, KitWiring, SolverTables }
impl AutoSetValue {
    pub const ALL: [AutoSetValue; 5] = [Self::ConduitRoutes, Self::PowerPriorities, Self::MountLimits, Self::KitWiring, Self::SolverTables];
    /// Name of the property test that guards it in CI (L202).
    pub fn property_test(self) -> &'static str { match self { Self::ConduitRoutes => "autoset_conduit_routes", Self::PowerPriorities => "autoset_power_priorities", Self::MountLimits => "autoset_mount_limits", Self::KitWiring => "autoset_kit_wiring", Self::SolverTables => "autoset_solver_tables" } }
}

/// Which auto-set values are in force this release. Committed data (`crates/sim-systems/data/autoset.ron`)
/// rewritten by the CI fallback; a value listed as dropped behaves as Suggest.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AutoSetStatus { pub dropped: Vec<(AutoSetValue, String)> }
impl AutoSetStatus { pub fn is_auto(&self, v: AutoSetValue) -> bool { !self.dropped.iter().any(|(d, _)| *d == v) } }

/// A suggestion: the edit to preview and one line saying why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Suggestion { pub kind: SuggestKind, pub edit: BlueprintEdit, pub text: Msg }

/// Compute one suggestion; `None` when the design already matches it. Unit: `sys-suggest`.
///
/// `design` must be built from `bp`. The edit is checked with [`super::notes::apply_edit`] before it is
/// returned, so accepting it always applies.
pub fn suggest(kind: SuggestKind, bp: &Blueprint, design: &ShipDesign, catalogue: &Catalogue) -> Option<Suggestion> {
    let s = match kind {
        SuggestKind::BallastAndTrim => compute::ballast_and_trim(bp, design, catalogue),
        SuggestKind::ArmourLayout => compute::armour_layout(bp, catalogue),
        SuggestKind::EngineAndGeneratorSizing => compute::engine_and_generator(bp, &design.prefab, catalogue),
        SuggestKind::FuelLoad => compute::fuel_load(bp, design, catalogue),
        SuggestKind::SensorPlacement => compute::sensor_placement(bp, &design.prefab, catalogue),
        SuggestKind::BulkheadPlacement => compute::bulkhead_placement(bp, &design.prefab, catalogue),
        SuggestKind::Aggression => compute::aggression(bp, design),
        SuggestKind::EmconTriggerRange => compute::emcon_trigger(bp, design, catalogue),
    }?;
    super::notes::apply_edit(bp, &s.edit, catalogue)?;
    Some(s)
}

/// The 200 seeded mutated variants of a design used by the auto-set qualifying corpus (6 starters, every
/// enemy class, 200 variants; L212). Unit: `sys-suggest`.
///
/// One to four edits drawn from the counter-based RNG keyed on `seed`: nudge or turn a loose part, move a
/// kit, change a tank, magazine or ballast load, nudge a hull handle, add a bulkhead, remove a loose part, or
/// paint an armour patch. Edits that no longer apply are skipped. Deterministic; the result is canonical and
/// rehashed.
pub fn mutated_variant(bp: &Blueprint, seed: u64, catalogue: &Catalogue) -> Blueprint { mutate::mutated_variant(bp, seed, catalogue) }

/// The committed auto-set status file.
const AUTOSET_RON: &str = include_str!("../../data/autoset.ron");

/// Parse the embedded auto-set status file. Unit: `sys-suggest`.
///
/// The file is committed data checked by a test, so a parse failure cannot ship; should it happen anyway,
/// every value stays auto-set rather than stopping the editor.
pub fn autoset_status() -> AutoSetStatus { content::data::parse_ron::<AutoSetStatus>(AUTOSET_RON, "autoset.ron").unwrap_or_default() }

/// Every `suggest.*` key this unit can emit (`game/i18n/suggest.csv`). Unit: `sys-suggest`.
pub fn text_keys() -> Vec<&'static str> { compute::KEYS.to_vec() }
