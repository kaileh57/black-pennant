//! Unit `content-stats`. Stats recomputed from blueprint data alone: tonnage, cost, dimensions, counts.
//! These are the numbers the migration gate diffs between releases (L2085) and that validation, refit cost
//! and the economy use (L801-822). Physics-derived gauges (GM, speed, range) live with the editor gauges in
//! sim-systems; this module is static bookkeeping only.
use crate::blueprint::Blueprint;
use crate::catalogue::Catalogue;
use crate::part::PartCategory;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod calc;
pub(crate) mod geom;
mod refit;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod testkit;

/// Fuel density in kilograms per litre (marine diesel), used for full-load displacement.
pub const FUEL_KG_PER_L: f64 = 0.85;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BlueprintStats {
    /// Full-load displacement in tonnes: structure, armour, parts, fuel, ammunition, ballast.
    pub tonnage_t: f64,
    /// Light-ship mass in tonnes (no fuel, ammunition or ballast).
    pub light_t: f64,
    /// Build cost in Salvage (structure + armour + parts).
    pub cost: u32,
    pub length_m: f64,
    pub beam_m: f64,
    /// Keel to highest cell, metres.
    pub height_m: f64,
    pub cell_count: u32,
    pub part_count: u16,
    pub berths: u16,
    pub passenger_berths: u16,
    pub fuel_l: f64,
    pub parts_by_category: BTreeMap<PartCategory, u16>,
    /// Centre of mass at full load in vessel-local metres.
    pub com_m: [f64; 3],
}

/// Compute static stats. Deterministic; sums run in ascending cell index then ascending part slot.
/// Unit: `content-stats`.
pub fn compute_stats(bp: &Blueprint, catalogue: &Catalogue) -> BlueprintStats { calc::compute_stats(bp, catalogue) }

/// One changed part in a refit diff (Quartermaster output and refit pricing, L801-807).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PartChange { Added(crate::blueprint::PartPlacement), Removed(crate::blueprint::PartPlacement), Moved { before: crate::blueprint::PartPlacement, after: crate::blueprint::PartPlacement } }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RefitDiff {
    pub changes: Vec<PartChange>,
    /// Structural mass change in tonnes (cells and armour): tonnes of structure built, removed or re-armoured,
    /// never negative (removing 5 t and adding 5 t elsewhere is 10 t of work).
    pub structure_delta_t: f64,
    /// Salvage for the structure and armour the refit adds (new cells and up-armouring at build cost). Removed
    /// structure returns nothing, as removed parts never sell (L805).
    #[serde(default)]
    pub structure_salvage: u32,
}

/// Diff two versions of a design part by part. Unit: `content-stats`.
pub fn diff_blueprints(before: &Blueprint, after: &Blueprint, catalogue: &Catalogue) -> RefitDiff { refit::diff_blueprints(before, after, catalogue) }

/// Price of a refit: added parts at full price unless drawn from the parts store (free), removed parts go to
/// the store and never sell; time is 2 h per module swap plus 1 h per 10 t of structural change (L801-807).
/// Returns (Salvage, theatre hours, parts taken from store). Unit: `content-stats`.
pub fn refit_cost(diff: &RefitDiff, catalogue: &Catalogue, parts_store: &BTreeMap<crate::ids::PartId, u16>) -> RefitCost { refit::refit_cost(diff, catalogue, parts_store) }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RefitCost { pub salvage: u32, pub hours: u32, pub from_store: BTreeMap<crate::ids::PartId, u16> }
