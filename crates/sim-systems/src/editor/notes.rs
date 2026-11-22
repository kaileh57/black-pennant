//! Unit `sys-notes`. Ship's Notes: the ranked problem list with Fix buttons. DESIGN.md L188-198, L1699
//! (note fields: part, cause, value, limit, fix, link to part).
//!
//! Open notes add a Caution triangle and a count to the Launch button and never block Launch. Severity
//! tiers, highest first: (1) cannot float, move or make power; (2) fails a constraint tag in the current
//! briefing's Fit Check (campaign only); (3) a weapon, sensor or consumer cannot work: arc blocked by own
//! structure over 50% of its sweep, unpowered, unconnected, or a pinned node or route lost its input;
//! (4) a gauge at Caution; (5) advisory. Within a tier, deterministically: a Fix that flips a Fit Check tag
//! from Fail to Pass first; then more affected parts; then ascending part index.
//! Gate fallback (L198): within-tier order by each note's measured effect on the trial battery's 1-vs-3
//! score, computed by the background worker after each save.
//!
//! # Sources and fixes (choices the spec leaves open)
//! Notes read the gauge strip, the built design (mounts, compiled-graph issues, routed networks), the
//! briefing's Fit Check and the migration report; nothing is recomputed that the strip already holds.
//! Fixes are the mechanical edits a designer would make: add the missing kit (or part) at the nearest place
//! it fits, raise a masked gun until its arc clears, add a magazine or director to the mount's kit, unpin a
//! broken pin, or take the matching Suggest (ballast and trim for GM, generator sizing for power, fuel load
//! for range, sensor placement for blind radar, bulkhead placement for flooding). A note has no Fix when no
//! mechanical edit answers it: an unconnected consumer, a radar cross-section limit, a complexity overrun.
//! "Unconnected" is a consumer whose compartment shares no network zone with a source
//! (`sim_core::networks::unconnected_consumers`); "Unpowered" is a consumer the Engineer sheds at combat load
//! when generation falls short (priority 5 first, L287). Missing directors are tier 3 for point defence and surface-to-air weapons (they need a lock) and advisory
//! for other guns. "No magazine" follows the simulation's feed wiring (`PartRuntime::feed`); it is tier 3
//! when the gun's ready rounds last under a minute at full rate and advisory otherwise, and calibres the
//! catalogue has no magazine for fire from their ready rounds by design and get no note. A hull that does
//! not float but still has reserve buoyancy capsizes rather than sinks, so its tier 1 note is the GM one with
//! the ballast and trim Fix. Graph issues other than a pinned node's lost input are advisory.
use super::fitcheck::FitResult;
use super::gauges::GaugeStrip;
use super::BlueprintEdit;
use crate::prelude::*;
use crate::ship::ShipDesign;
use content::blueprint::Blueprint;

mod collect;
#[cfg(test)]
mod stock;
pub(crate) mod edit;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum NoteTier { CannotFloatMoveOrPower, FailsBriefingTag, CannotWork, GaugeCaution, Advisory }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NoteKind {
    FloatFail, NoPropulsion, NoGeneration, NoSteering,
    TagFails(content::campaign::ConstraintTag),
    /// Fit Check's seakeeping sweep rolled the hull over in the briefing's forecast sea, with the sea from
    /// `from_deg` off the bow (the worst direction).
    RollsOver { sea_state: u8, from_deg: f64 },
    ArcBlocked { from_deg: f64, to_deg: f64, blocked_fraction: f64 },
    Unpowered, Unconnected, NoMagazine, NoDirector,
    PinnedNodeLostInput(GraphNodeId), PinnedRouteBroken(u16),
    GaugeCaution(String),
    AutoSetDropped(String),
    PartRemovedByMigration(PartId),
    OverTonnage, OverComplexity,
    Advisory(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShipNote {
    pub tier: NoteTier,
    pub kind: NoteKind,
    pub part: Option<PartSlot>,
    pub affected_parts: u16,
    /// Plain sentence, for example "Gun 2 cannot reach targets ahead: the bridge blocks 340 to 020 degrees."
    pub text: Msg,
    pub fix: Option<BlueprintEdit>,
    /// The Fix flips a briefing tag from Fail to Pass.
    pub flips_tag: bool,
    /// Measured effect on the 1-vs-3 score, when the fallback ordering is on.
    pub score_effect: Option<f64>,
}

/// All notes for a design, ranked. `fit` is the current briefing's Fit Check in campaign, `None` elsewhere.
/// Unit: `sys-notes`.
///
/// `design` and `gauges` must come from `bp`. Every Fix is checked with [`apply_edit`]; one that would not
/// apply is dropped rather than offered.
pub fn ship_notes(bp: &Blueprint, design: &ShipDesign, gauges: &GaugeStrip, fit: Option<&FitResult>, removed_by_migration: &[content::migrate::RemovedPart], catalogue: &Catalogue) -> Vec<ShipNote> {
    let mut notes = collect::collect(bp, design, gauges, fit, removed_by_migration, catalogue);
    for n in &mut notes {
        if let Some(f) = &n.fix { if apply_edit(bp, f, catalogue).is_none() { n.fix = None; n.flips_tag = false; } }
    }
    rank(&mut notes, false);
    notes
}

/// Apply an edit to a copy of the blueprint. The result is what the ghost preview shows and what accepting
/// the Fix commits (one undo step). `None` when the edit no longer applies. Unit: `sys-notes`.
///
/// Removing a part or kit renumbers every stored slot (kit part lists, officer settings, rules, pinned
/// routes, data links, pinned graph nodes); rules and pinned nodes that named the removed part go with it.
/// Hull handle and bulkhead edits on a lofted hull regenerate its cells, keeping the player's own cells,
/// cuts, armour and paint. The result is canonical with a fresh hash.
pub fn apply_edit(bp: &Blueprint, edit: &BlueprintEdit, catalogue: &Catalogue) -> Option<Blueprint> { edit::apply(bp, edit, catalogue) }

/// Rank notes by the rules above. Real ordering key, exposed for the fallback re-rank.
pub fn rank(notes: &mut [ShipNote], by_score_effect: bool) {
    notes.sort_by(|a, b| {
        a.tier.cmp(&b.tier)
            .then_with(|| if by_score_effect { b.score_effect.unwrap_or(0.0).partial_cmp(&a.score_effect.unwrap_or(0.0)).unwrap_or(core::cmp::Ordering::Equal) } else { core::cmp::Ordering::Equal })
            .then_with(|| b.flips_tag.cmp(&a.flips_tag))
            .then_with(|| b.affected_parts.cmp(&a.affected_parts))
            .then_with(|| a.part.cmp(&b.part))
    });
}

/// Every `notes.*` key this unit can emit (`game/i18n/notes.csv`). Unit: `sys-notes`.
pub fn text_keys() -> Vec<&'static str> { collect::KEYS.to_vec() }
