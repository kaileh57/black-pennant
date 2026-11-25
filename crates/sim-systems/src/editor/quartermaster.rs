//! Unit `sys-quartermaster`. The Quartermaster: proposes a legal refit or says none exists.
//! DESIGN.md L67, L801-807, L892 (gate), L893 (time gate).
//!
//! It never changes the hull shell. For each failing tag it swaps kits from the curated tag-linked list
//! (`Catalogue::kits_for_tag`), checks candidates with Fit Check on the background worker pool, and offers
//! the cheapest legal result as a diff of every changed part. Limits: 300 ms wall per candidate and 5 s in
//! total across 3 workers (about 50 candidates), enforced through `Deadline`. If none passes it answers
//! "No legal refit found" and names the blocking tag. It never returns an illegal refit: the winning
//! candidate is validated (`content::validate::validate_blueprint`) and re-checked before it is offered.
//! Candidate order is deterministic (ascending Salvage cost, then kit id), so the same inputs under
//! `NoDeadline` always give the same answer.
//!
//! # Candidates (choices the spec leaves open)
//! - Curated lists per tag: a draft limit and a minimum speed draw on `Propulsion` kits (the waterjet kit is
//!   the shallow-draft one), a cross-section limit on `LowSignature` kits, a range on `FuelStore` and then
//!   `Propulsion` kits, passengers on `PassengerBerths` kits, a required part on kits carrying that tag.
//! - A kit from the list is either swapped in for a fitted kit that does the same job (they share a tag, or a
//!   machinery, weapon or sensor category: an engine kit for an engine kit, a radar mast for a radar
//!   mast), one instance or every instance of that kit at once, or added where all its parts fit (the editor's
//!   placement search: weather-deck kits on the deck amidships, others low in the hull, propulsion aft). A
//!   swapped-in kit takes the old kit's place and turn when it fits there, else the nearest place within
//!   4 m where it does.
//!   Berths are added as many times as the passenger count needs, fuel tanks one to three times.
//! - With several failing tags, every single change and every combination of one change per tag are
//!   candidates. Each must leave the hull's shell and cells untouched, use only parts the player can field
//!   (`available`, or held in the parts store), and is priced with `content::stats::refit_cost` against the
//!   parts store. Candidates are sorted by Salvage, then the ids of the kits they fit, then theatre hours.
//! - Checking is serial in candidate order, so the first candidate that passes is the cheapest and the answer
//!   does not depend on timing. Before the simulated Fit Check a candidate must validate under the ruleset,
//!   stay within the briefing's tonnage cap, and pass every tag the screen scores exactly as the simulation
//!   does (all but `MinSpeed`, where a gauge top speed more than 10% short rules it out). At most
//!   [`MAX_SIMULATED`] candidates are simulated (the L893 fallback); one whose hull is lost in its run
//!   (capsized, sunk) is rejected. The passing candidate is re-checked before it is offered: shell unchanged, validated again, and every tag
//!   re-scored on the exact blueprint offered.
//! - "No legal refit found" names the first failing tag (briefing order) that no candidate made pass; when
//!   each failing tag passed on some candidate but none passed them all, the first failing tag of the
//!   candidate that came closest.
use super::fitcheck::{FitBrief, FitResult};
use super::Deadline;
use crate::prelude::*;
use crate::sim::SimDeps;
use content::blueprint::Blueprint;
use content::campaign::ConstraintTag;
use content::kit::KitDef;
use content::part::{PartCategory, PartFunction, PartTag, Placement};
use content::stats::{RefitCost, RefitDiff};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum QmResult {
    /// The design already passes every tag.
    AlreadyFits(FitResult),
    Refit { blueprint: Box<Blueprint>, diff: RefitDiff, cost: RefitCost, fit: FitResult },
    NoLegalRefit { blocking: ConstraintTag },
}

/// One candidate refit before checking.
#[derive(Clone, Debug, PartialEq)]
pub struct QmCandidate { pub blueprint: Blueprint, pub diff: RefitDiff, pub cost: RefitCost }

/// About 50 candidates fit in the 5 s search (L805).
pub const MAX_CANDIDATES: usize = 50;
/// Seconds simulated per Fit Check.
pub const CHECK_SECONDS: f64 = super::fitcheck::DEFAULT_SECONDS;
/// Most changes kept per failing tag before combining, and most combinations enumerated.
const MOVES_PER_TAG: usize = 16;
const MAX_ENUMERATED: usize = 1_024;
/// Most copies of one kit added by one change.
const MAX_COPIES: u8 = 4;
/// Most candidates simulated per search: the L893 fallback (screen with the gauges, simulate the 3 cheapest
/// that pass), taken up front because a 30 s check measured 185 to 630 ms per starter hull on the owner's
/// Mac without the throttle profile.
pub const MAX_SIMULATED: usize = 3;
/// A candidate whose gauge top speed is under this share of a `MinSpeed` tag is not simulated.
const SPEED_SCREEN: f64 = 0.9;
const FUEL_COPIES: u8 = 3;
/// How far from a swapped kit's place its replacement may go, cells (4 m).
const SWAP_RADIUS: i32 = 8;

/// One change to the design toward a tag.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Move {
    /// Replace the kit instances at these (original) kit slots with `kit`.
    Swap { slots: Vec<u16>, kit: KitId },
    /// Fit `count` copies of `kit` where they fit.
    Add { kit: KitId, count: u8 },
}
impl Move {
    fn kit(&self) -> &KitId { match self { Move::Swap { kit, .. } | Move::Add { kit, .. } => kit } }
}

/// Part tags whose kits can help a constraint tag, most direct first.
pub(crate) fn kit_tags_for(tag: &ConstraintTag) -> Vec<PartTag> {
    match tag {
        ConstraintTag::DraftLimit { .. } | ConstraintTag::MinSpeed { .. } => vec![PartTag::Propulsion],
        ConstraintTag::RcsLimit { .. } => vec![PartTag::LowSignature],
        ConstraintTag::Range { .. } => vec![PartTag::FuelStore, PartTag::Propulsion],
        ConstraintTag::Passengers { .. } => vec![PartTag::PassengerBerths],
        ConstraintTag::Requires(t) => vec![*t],
    }
}

fn kit_def<'a>(bp: &'a Blueprint, catalogue: &'a Catalogue, id: &KitId) -> Option<&'a KitDef> { bp.embedded_kits.get(id).or_else(|| catalogue.kit(id)) }

/// Whether the player can fit every part of a kit: known, in this build, and unlocked or in the store.
fn kit_usable(kit: &KitDef, catalogue: &Catalogue, available: Option<&[PartId]>, store: &BTreeMap<PartId, u16>) -> bool {
    !kit.parts.is_empty() && kit.parts.iter().all(|kp| {
        let Some(d) = catalogue.part(&kp.part) else { return false };
        d.phase <= content::catalogue::BUILD_PHASE && (available.is_none_or(|a| a.contains(&kp.part)) || store.get(&kp.part).copied().unwrap_or(0) > 0)
    })
}

/// Part categories that name a kit's job: machinery, weapons and sensors. Hull, control, crew, damage
/// control, offboard and special-operations parts do not (a crane and passenger berths are both special
/// operations but never stand in for each other; such kits match only by tag).
fn job_categories(kit: &KitDef, catalogue: &Catalogue) -> BTreeSet<u8> {
    kit.parts.iter().filter_map(|kp| catalogue.part(&kp.part)).map(|d| d.category)
        .filter(|c| matches!(c, PartCategory::Propulsion | PartCategory::Power | PartCategory::Fuel | PartCategory::Gun | PartCategory::Missile | PartCategory::Underwater | PartCategory::Defence | PartCategory::Sensor))
        .map(|c| c as u8).collect()
}

fn same_job(a: &KitDef, b: &KitDef, catalogue: &Catalogue) -> bool {
    a.id != b.id && (a.tags.iter().any(|t| b.tags.contains(t)) || !job_categories(a, catalogue).is_disjoint(&job_categories(b, catalogue)))
}

/// Berths one kit adds.
fn kit_berths(kit: &KitDef, catalogue: &Catalogue) -> u32 {
    kit.parts.iter().filter_map(|kp| catalogue.part(&kp.part)).map(|d| match d.function { PartFunction::PassengerBerths { count } => count as u32, _ => 0 }).sum()
}

/// Changes toward one failing tag, in a fixed order.
fn moves_for(bp: &Blueprint, tag: &ConstraintTag, catalogue: &Catalogue, available: Option<&[PartId]>, store: &BTreeMap<PartId, u16>) -> Vec<Move> {
    let mut out: Vec<Move> = Vec::new();
    let mut seen: BTreeSet<KitId> = BTreeSet::new();
    for pt in kit_tags_for(tag) {
        for kit in catalogue.kits_for_tag(pt) {
            if !seen.insert(kit.id.clone()) || !kit_usable(kit, catalogue, available, store) { continue; }
            // Swaps for fitted kits doing the same job, grouped by fitted kit type.
            let mut by_type: BTreeMap<KitId, Vec<u16>> = BTreeMap::new();
            for (i, kp) in bp.kits.iter().enumerate() {
                let Some(fitted) = kit_def(bp, catalogue, &kp.kit) else { continue };
                if same_job(fitted, kit, catalogue) { by_type.entry(kp.kit.clone()).or_default().push(i.min(u16::MAX as usize) as u16); }
            }
            for slots in by_type.values() {
                if slots.len() > 1 { out.push(Move::Swap { slots: slots.clone(), kit: kit.id.clone() }); }
                for s in slots { out.push(Move::Swap { slots: vec![*s], kit: kit.id.clone() }); }
            }
            // Additions.
            let copies: Vec<u8> = match (tag, pt) {
                (ConstraintTag::Passengers { count, .. }, PartTag::PassengerBerths) => {
                    let have = super::fitcheck::passenger_berths(bp, catalogue);
                    let per = kit_berths(kit, catalogue).max(1);
                    let need = (*count as u32).saturating_sub(have);
                    vec![need.div_ceil(per).clamp(1, MAX_COPIES as u32) as u8]
                }
                (ConstraintTag::Range { .. }, PartTag::FuelStore) => (1..=FUEL_COPIES).collect(),
                _ => vec![1],
            };
            for count in copies { out.push(Move::Add { kit: kit.id.clone(), count }); }
        }
    }
    out.truncate(MOVES_PER_TAG);
    out
}

/// Where to start the placement search for a kit, as (length fraction, on the weather deck).
fn add_target(kit: &KitDef, catalogue: &Catalogue) -> (f64, bool) {
    let defs: Vec<_> = kit.parts.iter().filter_map(|kp| catalogue.part(&kp.part)).collect();
    let deck = defs.iter().any(|d| matches!(d.placement, Placement::Weather | Placement::DeckEdge));
    let aft = defs.iter().any(|d| d.category == PartCategory::Propulsion);
    (if aft { 0.25 } else { 0.5 }, deck)
}

/// The design with a set of changes made, or `None` when a change does not apply. Swapped kits come out
/// first (highest kit slot down, so lower slots keep their meaning); each replacement then goes in at the old
/// kit's place and turn when it fits there, else at the nearest place within [`SWAP_RADIUS`] cells where all
/// its parts fit; additions come last.
fn apply_moves(bp: &Blueprint, moves: &[&Move], catalogue: &Catalogue) -> Option<Blueprint> {
    use super::BlueprintEdit as E;
    use super::suggest::place::{Space, MAX_RADIUS};
    let mut swaps: Vec<(u16, KitId)> = Vec::new();
    for m in moves { if let Move::Swap { slots, kit } = m { for s in slots { swaps.push((*s, kit.clone())); } } }
    swaps.sort_by_key(|a| core::cmp::Reverse(a.0));
    if swaps.windows(2).any(|w| w[0].0 == w[1].0) { return None; }
    let mut out = bp.clone();
    let mut refit: Vec<(u16, KitId, content::cell::CellPos, content::kit::PartRot, bool)> = Vec::with_capacity(swaps.len());
    for (slot, kit) in swaps {
        let old = out.kits.get(slot as usize)?.clone();
        out = super::notes::apply_edit(&out, &E::RemoveKit(KitSlot(slot)), catalogue)?;
        refit.push((slot, kit, old.pos, old.rot, old.mirrored));
    }
    refit.sort_by_key(|r| r.0);
    for (_, kit, pos, rot, mirrored) in refit {
        if let Some(b) = super::notes::apply_edit(&out, &E::AddKit { kit: kit.clone(), pos, rot, mirrored }, catalogue) { out = b; continue; }
        let def = kit_def(&out, catalogue, &kit)?.clone();
        let at = find_kit(&Space::new(&out, catalogue), &def, catalogue, rot, mirrored, pos, SWAP_RADIUS)?;
        out = super::notes::apply_edit(&out, &E::AddKit { kit, pos: at, rot, mirrored }, catalogue)?;
    }
    for m in moves {
        let Move::Add { kit, count } = m else { continue };
        let def = kit_def(&out, catalogue, kit)?.clone();
        let (frac, deck) = add_target(&def, catalogue);
        for _ in 0..*count {
            let space = Space::new(&out, catalogue);
            let target = if deck { space.deck_top(frac) } else { space.low_centre(frac) };
            let rot = super::suggest::place::upright();
            let pos = find_kit(&space, &def, catalogue, rot, false, target, MAX_RADIUS)?;
            out = super::notes::apply_edit(&out, &E::AddKit { kit: kit.clone(), pos, rot, mirrored: false }, catalogue)?;
        }
    }
    Some(out)
}

/// The nearest kit position where every part fits its placement rule: the editor's search
/// (`suggest::place::Space::find_kit`, same visiting order: Chebyshev shells around `target`, then y, x, z),
/// with a cheap test first that every footprint cell is empty (inside the hull for internal parts), so a
/// hull with no room is ruled out fast.
fn find_kit(space: &super::suggest::place::Space, kit: &KitDef, catalogue: &Catalogue, rot: content::kit::PartRot, mirrored: bool, target: content::cell::CellPos, max_r: i32) -> Option<content::cell::CellPos> {
    use content::cell::{CellPos, MAX_GRID_DIM};
    let base = content::kit::kit_part_placements(kit, catalogue, CellPos::new(0, 0, 0), rot, mirrored);
    if base.is_empty() || base.len() != kit.parts.len() { return None; }
    let mut parts: Vec<(&content::part::PartDef, Vec<CellPos>)> = Vec::with_capacity(base.len());
    for p in &base {
        let d = catalogue.part(&p.part)?;
        parts.push((d, super::notes::edit::footprint(d, p.pos, p.rot)?));
    }
    let [sx, sy, sz] = [space.grid.size[0] as i32, space.grid.size[1] as i32, space.grid.size[2] as i32];
    let lim = MAX_GRID_DIM as i32;
    let shifted = |pos: CellPos| -> Option<Vec<(&content::part::PartDef, Vec<CellPos>)>> {
        let mut g = Vec::with_capacity(parts.len());
        for (d, cells) in &parts {
            let mut v = Vec::with_capacity(cells.len());
            for c in cells {
                let (x, y, z) = (c.x as i32 + pos.x as i32, c.y as i32 + pos.y as i32, c.z as i32 + pos.z as i32);
                if x < 0 || y < 0 || z < 0 || x >= lim || y >= lim || z >= lim { return None; }
                let q = CellPos::new(x as i16, y as i16, z as i16);
                let ok = if d.placement == Placement::Internal { space.inside(q) } else { space.empty(q) };
                if !ok { return None; }
                v.push(q);
            }
            g.push((*d, v));
        }
        Some(g)
    };
    for r in 0..=max_r.max(0) {
        for dy in -r..=r { for dx in -r..=r { for dz in -r..=r {
            if dx.abs().max(dy.abs()).max(dz.abs()) != r { continue; }
            let (x, y, z) = (target.x as i32 + dx, target.y as i32 + dy, target.z as i32 + dz);
            if x < 0 || y < 0 || z < 0 || x >= lim || y >= lim || z >= lim || x > sx + 4 || y > sy + 8 || z > sz + 4 { continue; }
            let pos = CellPos::new(x as i16, y as i16, z as i16);
            if let Some(g) = shifted(pos) { if space.fits_group(&g) { return Some(pos); } }
        }}}
    }
    None
}

/// Every part an added change brings in must be fieldable.
fn diff_usable(diff: &RefitDiff, available: Option<&[PartId]>, store: &BTreeMap<PartId, u16>) -> bool {
    let Some(a) = available else { return true };
    diff.changes.iter().all(|c| match c {
        content::stats::PartChange::Added(p) => a.contains(&p.part) || store.get(&p.part).copied().unwrap_or(0) > 0,
        _ => true,
    })
}

/// Enumerate candidates for the failing tags in checking order (cheapest first), at most `max`.
/// Unit: `sys-quartermaster`.
pub fn candidates(bp: &Blueprint, failing: &[ConstraintTag], parts_store: &BTreeMap<PartId, u16>, available: Option<&[PartId]>, catalogue: &Catalogue, max: usize) -> Vec<QmCandidate> {
    if failing.is_empty() || max == 0 { return Vec::new(); }
    let bp = super::splash::canonical(bp);
    let per_tag: Vec<Vec<Move>> = failing.iter().map(|t| moves_for(&bp, t, catalogue, available, parts_store)).collect();
    // Sets of changes: every single change, then one change per failing tag.
    let mut sets: BTreeSet<Vec<Move>> = BTreeSet::new();
    for moves in &per_tag { for m in moves { sets.insert(vec![m.clone()]); } }
    if per_tag.len() > 1 && per_tag.iter().all(|m| !m.is_empty()) {
        let mut combos: Vec<Vec<Move>> = vec![Vec::new()];
        for moves in &per_tag {
            let mut next = Vec::new();
            'outer: for c in &combos {
                for m in moves {
                    let mut v = c.clone();
                    if !v.contains(m) { v.push(m.clone()); }
                    next.push(v);
                    if next.len() >= MAX_ENUMERATED { break 'outer; }
                }
            }
            combos = next;
        }
        for mut c in combos { c.sort(); c.dedup(); sets.insert(c); if sets.len() >= MAX_ENUMERATED { break; } }
    }
    let mut out: Vec<(QmCandidate, String)> = Vec::new();
    let mut hashes: BTreeSet<ContentHash> = BTreeSet::new();
    for set in sets {
        let refs: Vec<&Move> = set.iter().collect();
        let Some(cand) = apply_moves(&bp, &refs, catalogue) else { continue };
        if cand.hull != bp.hull || cand.cells != bp.cells || cand.header.hash == bp.header.hash || !hashes.insert(cand.header.hash) { continue; }
        let diff = content::stats::diff_blueprints(&bp, &cand, catalogue);
        if diff.changes.is_empty() || !diff_usable(&diff, available, parts_store) { continue; }
        let cost = content::stats::refit_cost(&diff, catalogue, parts_store);
        let mut kits: Vec<&str> = set.iter().map(|m| m.kit().as_str()).collect();
        kits.sort_unstable();
        out.push((QmCandidate { blueprint: cand, diff, cost }, kits.join(",")));
    }
    out.sort_by(|(a, ka), (b, kb)| a.cost.salvage.cmp(&b.cost.salvage).then_with(|| ka.cmp(kb)).then_with(|| a.cost.hours.cmp(&b.cost.hours)).then_with(|| a.blueprint.header.hash.cmp(&b.blueprint.header.hash)));
    out.truncate(max);
    out.into_iter().map(|(c, _)| c).collect()
}

/// Legal under the ruleset and the briefing's cap: no validation issue, full-load tonnage within `cap_t`.
fn legal(bp: &Blueprint, catalogue: &Catalogue, ruleset: &content::ruleset::Ruleset, available: Option<&[PartId]>, cap_t: f64) -> bool {
    let ctx = content::validate::ValidationContext { ruleset, available, enemy_class: false, complexity: None };
    let report = content::validate::validate_blueprint(bp, catalogue, &ctx);
    report.is_ok() && (!(cap_t.is_finite() && cap_t > 0.0) || report.stats.tonnage_t <= cap_t)
}

/// A deadline that only stops on the overall limit (the design's own check is not a candidate).
struct OverallOnly<'a>(&'a mut dyn Deadline);
impl Deadline for OverallOnly<'_> { fn expired(&mut self) -> bool { self.0.expired() } }

/// Run the Quartermaster. Candidates may be checked in parallel on the caller's pool; the answer is the
/// cheapest passing candidate in candidate order regardless of completion order.
/// Unit: `sys-quartermaster`.
pub fn quartermaster(bp: &Blueprint, brief: &FitBrief, parts_store: &BTreeMap<PartId, u16>, available: Option<&[PartId]>, ruleset: &content::ruleset::Ruleset, make_deps: &mut dyn FnMut() -> SimDeps, deadline: &mut dyn Deadline) -> QmResult {
    let bp = super::splash::canonical(bp);
    let deps = make_deps();
    let catalogue = deps.catalogue.clone();
    let campaign = deps.campaign.clone();
    // The tag run alone: the seakeeping sweep belongs to the design's own Fit Check, not to this 5 s search.
    let base = super::fitcheck::fit_check_tags(&bp, brief, deps, CHECK_SECONDS, &mut OverallOnly(deadline));
    if base.passes() { return QmResult::AlreadyFits(base); }
    let failing: Vec<ConstraintTag> = base.tags.iter().filter(|t| !t.pass).map(|t| t.tag.clone()).collect();
    let Some(first_failing) = failing.first().cloned() else { return QmResult::AlreadyFits(base) };
    // Parts held in the store count as unlocked.
    let fieldable: Option<Vec<PartId>> = available.map(|a| {
        let mut v: Vec<PartId> = a.to_vec();
        v.extend(parts_store.iter().filter(|(_, n)| **n > 0).map(|(p, _)| p.clone()));
        v.sort();
        v.dedup();
        v
    });
    let fieldable = fieldable.as_deref();
    let list = candidates(&bp, &failing, parts_store, available, &catalogue, MAX_CANDIDATES);
    let mut fixed = vec![false; failing.len()];
    let mut closest: Option<(usize, ConstraintTag)> = None;
    let mut simulated = 0usize;
    let note = |fit: &FitResult, fixed: &mut Vec<bool>, closest: &mut Option<(usize, ConstraintTag)>| {
        for (i, tag) in failing.iter().enumerate() {
            if fit.tags.iter().any(|t| t.tag == *tag && t.pass) { fixed[i] = true; }
        }
        let fails = fit.tags.iter().filter(|t| !t.pass).count();
        if let Some(first) = fit.first_failing() {
            if closest.as_ref().is_none_or(|(n, _)| fails < *n) { *closest = Some((fails, first.clone())); }
        }
    };
    for cand in list {
        if deadline.expired() { break; }
        deadline.start_candidate();
        if !legal(&cand.blueprint, &catalogue, ruleset, fieldable, brief.cap_t) { continue; }
        // Every tag but speed scores the same analytically as simulated: screen first.
        let screen = super::fitcheck::fit_screen_with(&cand.blueprint, brief, &catalogue, &campaign);
        // Speed needs the run, but a hull the gauge (held within 10% of the run) puts more than 10% short
        // cannot make it.
        let screen_ok = screen.tags.iter().all(|t| t.pass || matches!(t.tag, ConstraintTag::MinSpeed { kn } if screen.top_speed_kn >= SPEED_SCREEN * kn));
        if !screen_ok { note(&screen, &mut fixed, &mut closest); continue; }
        if simulated >= MAX_SIMULATED { break; }
        simulated += 1;
        let (fit, lost) = super::fitcheck::fit_check_run(&cand.blueprint, brief, make_deps(), CHECK_SECONDS, deadline);
        note(&fit, &mut fixed, &mut closest);
        // A refit whose hull is lost in its own check is never offered.
        if lost || !(fit.simulated && fit.passes()) { continue; }
        // Re-check the exact design offered: shell untouched, legal, and every tag re-scored on it (speed as
        // the run measured it).
        if cand.blueprint.hull != bp.hull || cand.blueprint.cells != bp.cells || !legal(&cand.blueprint, &catalogue, ruleset, fieldable, brief.cap_t) { continue; }
        let again = super::fitcheck::fit_screen_with(&cand.blueprint, brief, &catalogue, &campaign);
        let rescored: Vec<bool> = brief.tags.iter().map(|t| match t {
            ConstraintTag::MinSpeed { kn } => fit.top_speed_kn.is_finite() && fit.top_speed_kn >= *kn,
            _ => again.tags.iter().any(|r| r.tag == *t && r.pass),
        }).collect();
        if !rescored.iter().all(|p| *p) { continue; }
        return QmResult::Refit { blueprint: Box::new(cand.blueprint), diff: cand.diff, cost: cand.cost, fit };
    }
    let blocking = failing.iter().zip(&fixed).find(|(_, f)| !**f).map(|(t, _)| t.clone())
        .or_else(|| closest.map(|(_, t)| t))
        .unwrap_or(first_failing);
    QmResult::NoLegalRefit { blocking }
}

#[cfg(test)]
mod tests;
