//! Part of unit `sys-ident`. Stance rules, in order (DESIGN.md L496-503):
//! 1. Own-side and co-op hulls, drones and missiles are Friend over the datalink and can never be engaged,
//!    even by hand. Friendly fire from ballistic spread still exists.
//! 2. Hostile when it fires on your side, its emitter matches a Combine radar, the briefing marks its class
//!    hostile, or (PvP) the lobby puts it on the opposing side.
//! 3. Defectors, decoy convoys and civilian traffic start Neutral when the briefing declares them; everything
//!    else starts Suspect.
//! 4. The player or the graph may override stance per track; the track card always shows the reason.
//!
//! Decisions where the spec is silent:
//! - Rule 1 tracks are made by `crate::tracks::fuse` (Friend, reason Datalink, datalink source); the rules here
//!   never write that pair and never move such a track.
//! - An override outranks rules 2 and 3 (it is the player's or graph's call); a graph override never replaces
//!   or clears a player override.
//! - A briefing declaration of Neutral for a hull (a defector, a declared convoy) outranks the Combine-radar
//!   rule, since a defecting Combine hull still radiates Combine radar; it never outranks having fired on us.
//!   A declaration of Friend for a hull not on the datalink is shown as Neutral: it is never auto-engaged,
//!   and only the link makes a Friend.
//! - A track that has shown BriefingNeutral keeps it until it fires on us or an override moves it (DECISION,
//!   r2c): a defector's track that briefly associated an ESM bearing of a Combine hull turned Hostile by the
//!   Combine-radar rule for 10 s, and the escort's own guns sank the defector it came for. The latch holds
//!   only while `StanceFacts::declared` for the track's current truth is Neutral or Friend (DECISION, r3):
//!   a track whose truth moves to a hull the briefing does not declare Neutral (a pursuer) falls through to
//!   the other rules.
//! - The Combine-radar rule and the briefing's hostile and civilian class lists are player-side briefing
//!   knowledge: they apply to pictures of `Faction::Players` sides. An AI side's briefing reaches it through
//!   `StanceFacts::declared` (the caller passes Hostile for a truth entity on a side in `hostile_to`).
//! - PvP: a track has no truth side, so when the viewer's side carries a lobby team and any side of the
//!   sortie carries the other team, every track not declared Neutral is on the opposing side (in PvP every
//!   non-allied unit belongs to the other team; allies are rule 1).
//! - Firing on your side: shell hits, body hits and cell damage on own-side hulls, and splashes within
//!   `crate::sim::events::NEAR_MISS_M` (150 m, horizontal) of one, as the event fold in `crate::sim::events`
//!   collects them. A near miss counts because the shooter meant it.
use super::IdentLibrary;
use crate::prelude::*;
use crate::protocol::config::{Faction, SideDef, SortieIntel};
use crate::protocol::snapshot::SourceMask;
use crate::tracks::Track;

/// Facts the rules read besides the track itself.
#[derive(Clone, Copy)]
pub struct StanceFacts<'a> {
    pub viewer: &'a SideDef,
    pub sides: &'a [SideDef],
    pub intel: &'a SortieIntel,
    pub lib: &'a IdentLibrary,
    /// Stance the briefing declared for the truth entity behind the track, applied once the track's rung
    /// reaches Class or the declaration is by position (declared convoys and civilians on their lane).
    pub declared: Option<Stance>,
}

fn is_datalink_friend(t: &Track) -> bool { super::is_datalink_friend(t) }

/// Rules 2 and 3 (and the override of rule 4) for a non-datalink track.
fn decide(track: &Track, facts: &StanceFacts<'_>) -> (Stance, StanceReason) {
    if let Some(s) = track.stance_override {
        let r = if matches!(track.stance_reason, StanceReason::PlayerOverride | StanceReason::GraphOverride) { track.stance_reason } else { StanceReason::PlayerOverride };
        return (s, r);
    }
    if track.fired_on_us { return (Stance::Hostile, StanceReason::FiredOnOwnSide); }
    // A track the briefing made Neutral stays Neutral while the briefing still declares its current truth
    // Neutral or Friend: a passing association with another hull's detection (an ESM bearing) must not turn
    // a defector Hostile, but a track whose truth moved to a declared-hostile hull falls through.
    let declared_neutral = matches!(facts.declared, Some(Stance::Neutral | Stance::Friend));
    if declared_neutral && track.stance == Stance::Neutral && track.stance_reason == StanceReason::BriefingNeutral { return (Stance::Neutral, StanceReason::BriefingNeutral); }
    if facts.declared == Some(Stance::Hostile) { return (Stance::Hostile, StanceReason::BriefingHostile); }
    let players = facts.viewer.faction == Faction::Players;
    let classed = track.rung >= IdRung::Class;
    if players && !declared_neutral && track.emitters.iter().any(|e| facts.lib.combine_emitters.binary_search(e).is_ok()) {
        return (Stance::Hostile, StanceReason::CombineEmitter);
    }
    if players && !declared_neutral && classed && track.class.as_ref().is_some_and(|c| facts.intel.declared_hostile.contains(c)) {
        return (Stance::Hostile, StanceReason::BriefingHostile);
    }
    if !declared_neutral {
        if let Some(team) = facts.viewer.pvp_team {
            if facts.sides.iter().any(|s| s.pvp_team.is_some_and(|t| t != team)) { return (Stance::Hostile, StanceReason::LobbyOpposingSide); }
        }
    }
    if declared_neutral { return (Stance::Neutral, StanceReason::BriefingNeutral); }
    if players && classed && track.class.as_ref().is_some_and(|c| facts.intel.civilian.contains(c)) {
        return (Stance::Neutral, StanceReason::BriefingNeutral);
    }
    (Stance::Suspect, StanceReason::Default)
}

/// Evaluate the rules for one track and write `stance` and `stance_reason`. Returns true when it changed.
/// Unit: `sys-ident`.
pub fn evaluate(track: &mut Track, facts: &StanceFacts<'_>) -> bool {
    if is_datalink_friend(track) { return false; }
    let (s, mut r) = decide(track, facts);
    // A Hostile track keeps the briefing or emitter reason it first showed while it stays Hostile for either:
    // a track that associates with several hulls of a group swaps between a briefed class and a radiating
    // escort, and the card's reason (and the log) flickered several times a second (DECISION, q5). Firing on
    // us still replaces it.
    let sticky = |x: StanceReason| matches!(x, StanceReason::CombineEmitter | StanceReason::BriefingHostile);
    if s == Stance::Hostile && track.stance == Stance::Hostile && sticky(r) && sticky(track.stance_reason) { r = track.stance_reason; }
    let changed = track.stance != s || track.stance_reason != r;
    track.stance = s;
    track.stance_reason = r;
    changed
}

/// Apply or clear a manual or graph override. Refuses to move a datalink Friend (rule 1) and returns false.
/// Unit: `sys-ident`.
///
/// A graph override may not replace or clear a player override (returns false). Clearing applies the
/// fired-on rule at once and otherwise shows Suspect until the next `evaluate`.
pub fn set_override(track: &mut Track, stance: Option<Stance>, by_graph: bool) -> bool {
    if is_datalink_friend(track) { return false; }
    if by_graph && track.stance_override.is_some() && track.stance_reason == StanceReason::PlayerOverride { return false; }
    match stance {
        Some(s) => {
            track.stance_override = Some(s);
            track.stance = s;
            track.stance_reason = if by_graph { StanceReason::GraphOverride } else { StanceReason::PlayerOverride };
        }
        None => {
            if track.stance_override.take().is_none() { return true; }
            (track.stance, track.stance_reason) = if track.fired_on_us { (Stance::Hostile, StanceReason::FiredOnOwnSide) } else { (Stance::Suspect, StanceReason::Default) };
        }
    }
    true
}

/// Mark every track of `victim_side` associated with `shooter` as having fired on that side (rule 2).
/// Called from the physical event stream (shell and body impacts on own-side hulls; whether a near miss also
/// counts, and at what distance, is this unit's choice to make and document, since the spec says only "fires
/// on your side").
/// Unit: `sys-ident`.
///
/// Near misses count within 150 m (see the module notes). Datalink friends are never marked; a marked track
/// turns Hostile at once unless an override holds it.
pub fn note_fired_on(pictures: &mut crate::tracks::Pictures, victim_side: SideId, shooter: EntityId) {
    if shooter.is_none() { return; }
    let Some(p) = pictures.side_mut(victim_side) else { return };
    for t in p.tracks.iter_mut().filter(|t| t.truth == shooter && t.is_live()) {
        if t.stance == Stance::Friend && t.stance_reason == StanceReason::Datalink && t.sources.contains(SourceMask::DATALINK) { continue; }
        t.fired_on_us = true;
        if t.stance_override.is_none() {
            t.stance = Stance::Hostile;
            t.stance_reason = StanceReason::FiredOnOwnSide;
        }
    }
}
