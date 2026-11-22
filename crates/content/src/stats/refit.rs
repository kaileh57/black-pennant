//! Refit diffs and pricing (DESIGN.md L801-807).
//!
//! Matching: a part at the same place and rotation in both designs is unchanged (its label, load and kit
//! membership may differ; those are dock settings, not refit work). Of the rest, a part of the same type in
//! both designs is moved, pairing slots in ascending order; anything left is removed or added.
//!
//! Time: 2 h per module swap plus 1 h per 10 t of structural change (rounded up), both from
//! `data/campaign_rules.ron`. A swap is one module taken out and another fitted, so a replacement (one removed,
//! one added) is one swap: swaps = moved + max(added, removed).
use super::geom::{Prices, Structure};
use super::{PartChange, RefitCost, RefitDiff};
use crate::blueprint::{Blueprint, PartPlacement};
use crate::campaign::CampaignRules;
use crate::catalogue::Catalogue;
use crate::cell::{Cell, CellPos, MAX_GRID_DIM};
use crate::ids::PartId;
use crate::kit::{normalize_rot, PartRot};
use std::collections::{BTreeMap, VecDeque};

/// Campaign rules from the embedded data; the spec's defaults if that file does not parse (the campaign
/// loader reports the parse error).
pub(crate) fn rules() -> &'static CampaignRules {
    static RULES: std::sync::OnceLock<CampaignRules> = std::sync::OnceLock::new();
    RULES.get_or_init(|| crate::data::parse_ron(crate::data::CAMPAIGN_RULES, "crates/content/data/campaign_rules.ron").unwrap_or_default())
}

pub(super) fn diff_blueprints(before: &Blueprint, after: &Blueprint, catalogue: &Catalogue) -> RefitDiff {
    let key = |p: &PartPlacement| -> (PartId, CellPos, PartRot) { (p.part.clone(), p.pos, normalize_rot(p.rot)) };
    // Unchanged parts: same type, cell and rotation.
    let mut exact: BTreeMap<(PartId, CellPos, PartRot), VecDeque<usize>> = BTreeMap::new();
    for (i, p) in after.parts.iter().enumerate() {
        exact.entry(key(p)).or_default().push_back(i);
    }
    let mut after_used = vec![false; after.parts.len()];
    let mut before_left = Vec::new();
    for (i, p) in before.parts.iter().enumerate() {
        match exact.get_mut(&key(p)).and_then(|q| q.pop_front()) {
            Some(j) => after_used[j] = true,
            None => before_left.push(i),
        }
    }
    // Moved parts: same type, paired in ascending slot order.
    let mut by_type: BTreeMap<&PartId, VecDeque<usize>> = BTreeMap::new();
    for (j, p) in after.parts.iter().enumerate() {
        if !after_used[j] {
            by_type.entry(&p.part).or_default().push_back(j);
        }
    }
    let mut changes = Vec::new();
    for i in before_left {
        let b = &before.parts[i];
        match by_type.get_mut(&b.part).and_then(|q| q.pop_front()) {
            Some(j) => {
                after_used[j] = true;
                changes.push(PartChange::Moved { before: b.clone(), after: after.parts[j].clone() });
            }
            None => changes.push(PartChange::Removed(b.clone())),
        }
    }
    for (j, p) in after.parts.iter().enumerate() {
        if !after_used[j] {
            changes.push(PartChange::Added(p.clone()));
        }
    }
    let (kg, salvage) = structure_change(before, after, catalogue);
    RefitDiff { changes, structure_delta_t: kg / 1000.0, structure_salvage: salvage.round().min(u32::MAX as f64) as u32 }
}

/// Kilograms of structural work and the Salvage for what is built, comparing the two designs cell by cell.
///
/// Per cell: same material and shape with only armour changed costs the armour mass difference in work and
/// the armour cost of any added tier; any other change removes the old cell and builds the new one (both
/// masses in work, the new cell's full cost). Paint alone is free.
fn structure_change(before: &Blueprint, after: &Blueprint, catalogue: &Catalogue) -> (f64, f64) {
    let prices = Prices::new(catalogue);
    let (a, b) = (Structure::of(&before.cells), Structure::of(&after.cells));
    let mut keys: Vec<(u16, u16)> = a.rows.iter().chain(&b.rows).map(|(k, _)| *k).collect();
    keys.sort_unstable();
    keys.dedup();
    let dim = MAX_GRID_DIM as usize;
    let (mut la, mut lb) = (vec![Cell::EMPTY; dim], vec![Cell::EMPTY; dim]);
    let (mut kg, mut salvage) = (0.0f64, 0.0f64);
    for (y, z) in keys {
        let mut hi = 0usize;
        for (line, s) in [(&mut la, &a), (&mut lb, &b)] {
            for r in s.row(y, z).unwrap_or(&[]) {
                line[r.x0 as usize..r.x1 as usize].fill(r.cell);
                hi = hi.max(r.x1 as usize);
            }
        }
        for x in 0..hi {
            let (ca, cb) = (la[x], lb[x]);
            if ca == cb {
                continue;
            }
            let (pa, pb) = (prices.of(ca), prices.of(cb));
            if !ca.is_empty() && !cb.is_empty() && ca.material == cb.material && ca.shape == cb.shape {
                kg += (pb.armour_kg - pa.armour_kg).abs();
                salvage += (pb.armour_cost - pa.armour_cost).max(0.0);
            } else {
                kg += pa.structure_kg + pa.armour_kg + pb.structure_kg + pb.armour_kg;
                salvage += pb.structure_cost + pb.armour_cost;
            }
        }
        la[..hi].fill(Cell::EMPTY);
        lb[..hi].fill(Cell::EMPTY);
    }
    (kg, salvage)
}

pub(super) fn refit_cost(diff: &RefitDiff, catalogue: &Catalogue, parts_store: &BTreeMap<PartId, u16>) -> RefitCost {
    let rules = rules();
    let mut store = parts_store.clone();
    let mut from_store: BTreeMap<PartId, u16> = BTreeMap::new();
    let mut salvage = diff.structure_salvage as u64;
    let (mut added, mut removed, mut moved) = (0u64, 0u64, 0u64);
    for change in &diff.changes {
        match change {
            PartChange::Added(p) => {
                added += 1;
                match store.get_mut(&p.part) {
                    Some(n) if *n > 0 => {
                        *n -= 1;
                        let taken = from_store.entry(p.part.clone()).or_insert(0);
                        *taken = taken.saturating_add(1);
                    }
                    _ => salvage += catalogue.part(&p.part).map_or(0, |d| d.cost as u64),
                }
            }
            PartChange::Removed(_) => removed += 1,
            PartChange::Moved { .. } => moved += 1,
        }
    }
    let swaps = moved + added.max(removed);
    let tonnes = if diff.structure_delta_t.is_finite() { diff.structure_delta_t.max(0.0) } else { 0.0 };
    let per_hour = if rules.refit_tonnes_per_hour.is_finite() && rules.refit_tonnes_per_hour > 0.0 { rules.refit_tonnes_per_hour } else { 10.0 };
    // A sliver of float noise from summing cell masses must not cost an extra hour.
    let structure_hours = ((tonnes / per_hour) - 1e-9).ceil().max(0.0);
    let hours = swaps as f64 * rules.refit_hours_per_module as f64 + structure_hours;
    RefitCost { salvage: salvage.min(u32::MAX as u64) as u32, hours: hours.min(u32::MAX as f64) as u32, from_store }
}
