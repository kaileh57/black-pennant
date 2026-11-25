//! Unit `sys-theatre-progress`. The operation clock, act flow, unlocks and Open Theatre.
//! DESIGN.md L750 (clock, retreat), L778-782 (story operations, capstone), L801-807 (tiers), L866-875 (scope
//! per act, Open Theatre), L887 (gate).
//!
//! The clock runs per act until a Combine objective completes: 40 theatre days on Normal (slider). A success
//! pushes it back: template +1 day; depot strike or radar decapitation +2; ace duel won +2; story operation
//! +3. When it runs out the act is lost and the next act starts in the retreat configuration (the campaign
//! does not end); Act 4 restarts itself in retreat. Normal act start: every region Alert 0, 3 coves. Retreat
//! start: every region Alert 2, 2 coves (the act script names the closed cove), same clock length. Each act
//! has 4 story operations unlocked by theatre conditions, 1 or 2 ace duels and a capstone that unlocks when
//! its 4 story operations are done. Open Theatre opens in the same save after the Act 4 capstone: no clock,
//! all 4 regions, template operations only, the Combine rebuilds from shipyards, 1,800 t cap, template repeat
//! cap per 40 theatre days.
//!
//! # Rules where the spec is silent
//! - Success = `Outcome::Victory`, or a withdrawal after the primary objective was done. Completed and failed
//!   lists hold each operation once. The capstone pushes back like a story operation; the tutorial not at all.
//! - Pushback never lifts the clock above the act's full length (`clock_days` x 24 hours).
//! - Ghost-condition rewards, Intel and blueprints are paid by `economy`, never here.
//! - An act that has no `ActDef` cannot start: winning the last shipped act's capstone stops the clock
//!   (the campaign waits for the next act's update), and losing it restarts that act in retreat.
//! - Act start (normal or retreat) clears every region's Alert history, active and queued doctrine cards, the
//!   hunter group, the template offer counts (`OpsLedger::offered`, `last_offered`) and the offer list, seeds
//!   the order of battle of newly opened regions and fills missing logistics state. A retreat start also
//!   restores every cut lane and refills every region's Supply window (the Combine has pushed back).
//! - Coves: a normal start opens the act's coves (at most `normal_coves`); a retreat start drops the act's
//!   named closed cove (else its last) and keeps at most `retreat_coves`. SEXTANT moves to the first open cove
//!   if its node is no longer open. Open Theatre opens every cove on the chart.
//! - Starter hulls are read by the dry dock from `ActDef::starters` of the acts reached; no state changes.
//! - `StoryCount(n)` counts the current act's `story_ops` that are completed. [`unlocked`] has no campaign data
//!   and resolves it through the process-wide `content::campaign::campaign()`; callers that hold a
//!   [`TheatreData`] should use [`unlocked_in`], which is identical otherwise.
use super::{admiral, logistics, AlertCause, TheatreData, TheatreEvent};
use crate::prelude::*;
use crate::protocol::config::{Outcome, SortieResult};
use crate::protocol::config::AceFate;
use content::campaign::{CampaignData, MissionType, NodeKind, OperationDef, OperationKind, Unlock};
use content::save::{GroupOrder, TheatreState};

/// Deepest `Unlock` nesting evaluated; deeper branches read as locked.
const MAX_UNLOCK_DEPTH: u32 = 32;

/// Clock pushback in days for a won operation (data override, else the rule for its kind).
/// Unit: `sys-theatre-progress`.
pub fn pushback_days(op: &OperationDef, data: &TheatreData) -> f64 {
    if let Some(d) = op.pushback_days { if d.is_finite() { return d.max(0.0); } }
    let r = &data.campaign.rules;
    match &op.kind {
        OperationKind::Template(MissionType::DepotStrike | MissionType::RadarDecapitation) => r.pushback_strike,
        OperationKind::Template(_) => r.pushback_template,
        OperationKind::AceDuel(_) => r.pushback_ace,
        OperationKind::Story | OperationKind::Capstone => r.pushback_story,
        OperationKind::Tutorial => 0.0,
    }
}

/// Record a finished operation: completed or failed lists, clock pushback, aces beaten, tier and part
/// unlocks, capstone and act end. Unit: `sys-theatre-progress`.
///
/// Part unlocks come only from tiers here (a tier opened by the operation, `TierOpened`); blueprint parts are
/// economy's payout.
pub fn apply_result(state: &mut TheatreState, op: &OperationDef, result: &SortieResult, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    if result.outcome == Outcome::Aborted { return; }
    let success = result.outcome == Outcome::Victory || (result.outcome == Outcome::Withdrawn && result.objective_done);
    let tier_before = open_tier(state, data);

    for (ace, fate) in &result.aces {
        if matches!(fate, AceFate::Beaten | AceFate::Captured) && !state.ops.aces_beaten.contains(ace) { state.ops.aces_beaten.push(ace.clone()); }
    }
    if success {
        if let OperationKind::AceDuel(ace) = &op.kind { if !state.ops.aces_beaten.contains(ace) { state.ops.aces_beaten.push(ace.clone()); } }
        if !state.ops.completed.contains(&op.id) { state.ops.completed.push(op.id.clone()); }
        if let Some(left) = state.clock_hours_left {
            let days = pushback_days(op, data);
            let full = full_clock_hours(state);
            let new = (left + days * 24.0).min(full.max(left));
            state.clock_hours_left = Some(new);
            if days > 0.0 { events.push(TheatreEvent::ClockPushedBack { days, hours_left: new }); }
        }
    } else if !state.ops.failed.contains(&op.id) {
        state.ops.failed.push(op.id.clone());
    }

    let tier_after = open_tier(state, data);
    if tier_after > tier_before { events.push(TheatreEvent::TierOpened(tier_after)); }

    if success && op.kind == OperationKind::Capstone {
        let act = state.act;
        events.push(TheatreEvent::ActWon { act });
        let opens = data.campaign.acts.iter().find(|a| a.act == act).is_some_and(|a| a.opens_open_theatre) || act == Act::Act4;
        if opens {
            open_open_theatre(state, data, events);
        } else if let Some(next) = next_act(act).filter(|n| act_def(data, *n).is_some()) {
            start_act(state, next, false, data, events);
        } else {
            // The last shipped act is won: the clock stops until the next act's update.
            state.clock_hours_left = None;
        }
    }
}

/// One hour of the clock; on expiry lose the act and start the next in retreat.
/// Unit: `sys-theatre-progress`.
pub fn step_hour(state: &mut TheatreState, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    if state.open_theatre { state.clock_hours_left = None; return; }
    let Some(left) = state.clock_hours_left else { return };
    let left = if left.is_finite() { left - 1.0 } else { 0.0 };
    if left > 0.0 { state.clock_hours_left = Some(left); return; }
    state.clock_hours_left = Some(0.0);
    let act = state.act;
    events.push(TheatreEvent::ClockExpired);
    events.push(TheatreEvent::ActLost { act });
    let next = next_act(act).filter(|n| act_def(data, *n).is_some()).unwrap_or(act);
    start_act(state, next, true, data, events);
}

/// Start an act normally or in retreat: alerts, coves, clock, regions opened, starter hulls offered.
/// Unit: `sys-theatre-progress`.
pub fn start_act(state: &mut TheatreState, act: Act, retreat: bool, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    let rules = &data.campaign.rules;
    let tier_before = open_tier(state, data);
    state.act = act;
    state.retreat = retreat;
    state.open_theatre = false;
    state.clock_hours_left = Some(full_clock_hours(state));

    // Alert.
    let alert = if retreat { rules.retreat_alert.clamp(0.0, 5.0) } else { 0.0 };
    for def in &data.campaign.theatre.regions { state.regions.entry(def.id.clone()).or_default(); }
    for (id, r) in state.regions.iter_mut() {
        r.alert = alert;
        r.quiet_hours = 0;
        r.pending_alert.clear();
        r.active_cards.clear();
        r.queued_cards.clear();
        r.combine_control = false;
        events.push(TheatreEvent::AlertChanged { region: id.clone(), alert, cause: AlertCause::ActStart });
    }

    // Coves.
    let def = act_def(data, act);
    let mut coves: Vec<TheatreNodeId> = def.map(|d| d.coves.clone()).unwrap_or_default();
    coves.retain(|c| data.campaign.theatre.nodes.iter().any(|n| &n.id == c));
    coves.dedup();
    if coves.is_empty() { coves = all_coves(data); }
    if retreat {
        let closed = def.and_then(|d| d.retreat_closed_cove.clone()).filter(|c| coves.contains(c)).or_else(|| if coves.len() > 1 { coves.last().cloned() } else { None });
        if let Some(c) = closed { coves.retain(|x| *x != c); }
        coves.truncate((rules.retreat_coves as usize).max(1));
    } else {
        coves.truncate((rules.normal_coves as usize).max(1));
    }
    if !coves.is_empty() { state.open_coves = coves; }
    relocate_sextant(state, events);

    // Hunter group and offers.
    release_hunter(state);
    state.ops.offered.clear();
    state.ops.last_offered = None;
    state.offered.clear();

    // Order of battle and logistics.
    admiral::seed_order_of_battle(state, data, act);
    logistics::init_logistics(state, data, retreat);
    if retreat {
        for lane in state.lanes.values_mut() { lane.cut_until_hour = None; lane.mines = 0; }
        state.lanes.clear();
    }

    events.push(TheatreEvent::ActStarted { act, retreat });
    let tier_after = open_tier(state, data);
    if tier_after > tier_before { events.push(TheatreEvent::TierOpened(tier_after)); }
}

/// Evaluate an unlock condition against the ledger. Unit: `sys-theatre-progress`.
///
/// `StoryCount` needs campaign data and reads the process-wide `content::campaign::campaign()`; prefer
/// [`unlocked_in`] where a [`TheatreData`] is at hand.
pub fn unlocked(state: &TheatreState, unlock: &Unlock) -> bool {
    eval(state, unlock, &|| content::campaign::load::campaign(), 0)
}

/// [`unlocked`] against the given campaign data (no process-wide lookup).
pub fn unlocked_in(state: &TheatreState, unlock: &Unlock, data: &TheatreData) -> bool {
    let c: &CampaignData = &data.campaign;
    eval(state, unlock, &|| c, 0)
}

/// Highest part tier open now. Unit: `sys-theatre-progress`.
///
/// The highest start tier of every act reached (all acts in Open Theatre), raised by any `tier_unlocks`
/// operation completed. Tiers never close.
pub fn open_tier(state: &TheatreState, data: &TheatreData) -> Tier {
    let mut tier = Tier::T1;
    for a in &data.campaign.acts {
        if state.open_theatre || a.act <= state.act { tier = tier.max(a.start_tier); }
        for (op, t) in &a.tier_unlocks { if state.ops.completed.contains(op) { tier = tier.max(*t); } }
    }
    tier
}

/// Time-of-day band at a theatre hour from the sun table (L743). The table values await an owner decision;
/// this reads whatever `TheatreDef::sun` holds. Real code.
pub fn time_of_day(hour: u64, data: &TheatreData) -> TimeOfDay { TimeOfDay::from_sun_elevation_deg(data.campaign.theatre.sun.elevation_deg[(hour % 24) as usize]) }

// ---------------------------------------------------------------------------------------------------------

fn eval<'c>(state: &TheatreState, unlock: &Unlock, data: &dyn Fn() -> &'c CampaignData, depth: u32) -> bool {
    if depth > MAX_UNLOCK_DEPTH { return false; }
    match unlock {
        Unlock::Always => true,
        Unlock::SupplyBelow { region, value } => state.regions.get(region).is_some_and(|r| r.supply < *value),
        Unlock::AlertAtLeast { region, value } => state.regions.get(region).is_some_and(|r| super::alert::alert_floor(r.alert) >= *value),
        Unlock::Completed(ops) => ops.iter().all(|o| state.ops.completed.contains(o)),
        Unlock::StoryCount(n) => story_count(state, data()) >= *n as usize,
        Unlock::AceBeaten(ace) => state.ops.aces_beaten.contains(ace),
        Unlock::All(v) => v.iter().all(|u| eval(state, u, data, depth + 1)),
        Unlock::Any(v) => v.iter().any(|u| eval(state, u, data, depth + 1)),
    }
}

/// Completed story operations of the current act (its `story_ops`; without an act row, every completed
/// story operation of that act).
fn story_count(state: &TheatreState, c: &CampaignData) -> usize {
    match c.acts.iter().find(|a| a.act == state.act) {
        Some(a) => {
            let mut seen: Vec<&OperationId> = Vec::new();
            for o in &a.story_ops { if state.ops.completed.contains(o) && !seen.contains(&o) { seen.push(o); } }
            seen.len()
        }
        None => state.ops.completed.iter().filter(|o| c.operations.get(*o).is_some_and(|d| d.kind == OperationKind::Story && d.act == state.act)).count(),
    }
}

fn act_def(data: &TheatreData, act: Act) -> Option<&content::campaign::ActDef> { data.campaign.acts.iter().find(|a| a.act == act) }

fn next_act(act: Act) -> Option<Act> {
    match act { Act::Act1 => Some(Act::Act2), Act::Act2 => Some(Act::Act3), Act::Act3 => Some(Act::Act4), Act::Act4 => None }
}

fn full_clock_hours(state: &TheatreState) -> f64 { state.difficulty.clamped().clock_days as f64 * 24.0 }

fn all_coves(data: &TheatreData) -> Vec<TheatreNodeId> {
    data.campaign.theatre.nodes.iter().filter(|n| matches!(n.kind, NodeKind::Cove)).map(|n| n.id.clone()).collect()
}

fn relocate_sextant(state: &mut TheatreState, events: &mut Vec<TheatreEvent>) {
    if state.open_coves.is_empty() || state.open_coves.contains(&state.sextant_node) { return; }
    let to = state.open_coves[0].clone();
    let from = core::mem::replace(&mut state.sextant_node, to.clone());
    events.push(TheatreEvent::SextantMoved { from, to });
}

fn release_hunter(state: &mut TheatreState) {
    if let Some(id) = state.hunter_group.take() {
        if let Some(g) = state.groups.iter_mut().find(|g| g.id == id) {
            g.order = GroupOrder::Return;
            g.destination = None;
            g.arrive_hour = None;
        }
    }
}

fn open_open_theatre(state: &mut TheatreState, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    let tier_before = open_tier(state, data);
    state.open_theatre = true;
    state.retreat = false;
    state.clock_hours_left = None;
    let coves = all_coves(data);
    if !coves.is_empty() { state.open_coves = coves; }
    relocate_sextant(state, events);
    state.ops.offered.clear();
    state.ops.last_offered = None;
    state.offered.clear();
    admiral::seed_order_of_battle(state, data, state.act);
    logistics::init_logistics(state, data, false);
    events.push(TheatreEvent::OpenTheatreOpened);
    let tier_after = open_tier(state, data);
    if tier_after > tier_before { events.push(TheatreEvent::TierOpened(tier_after)); }
}

#[cfg(test)]
mod tests {
    use super::super::alert::tests::result;
    use super::super::logistics::testkit::*;
    use super::*;

    fn op<'a>(d: &'a TheatreData, id: &str) -> &'a OperationDef { &d.campaign.operations[&OperationId::new(id)] }

    #[test]
    fn pushback_by_kind() {
        let d = data();
        assert_eq!(pushback_days(op(&d, "op.raid"), &d), 1.0);
        assert_eq!(pushback_days(op(&d, "op.depot"), &d), 2.0);
        assert_eq!(pushback_days(op(&d, "op.duel"), &d), 2.0);
        assert_eq!(pushback_days(op(&d, "op.s1"), &d), 3.0);
        let mut o = op(&d, "op.raid").clone();
        o.pushback_days = Some(0.5);
        assert_eq!(pushback_days(&o, &d), 0.5);
    }

    #[test]
    fn clock_pushback_capped_and_failures_recorded() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        assert_eq!(s.clock_hours_left, Some(960.0));
        s.clock_hours_left = Some(500.0);
        apply_result(&mut s, op(&d, "op.depot"), &result(vec![], 0, 0), &d, &mut ev);
        assert_eq!(s.clock_hours_left, Some(548.0));
        s.clock_hours_left = Some(950.0);
        apply_result(&mut s, op(&d, "op.s1"), &result(vec![], 0, 0), &d, &mut ev);
        assert_eq!(s.clock_hours_left, Some(960.0));
        let mut lost = result(vec![], 0, 0);
        lost.outcome = Outcome::Defeat;
        apply_result(&mut s, op(&d, "op.raid"), &lost, &d, &mut ev);
        assert_eq!(s.ops.failed, vec![OperationId::new("op.raid")]);
        assert_eq!(s.clock_hours_left, Some(960.0));
        // Story op s1 opened tier 2.
        assert_eq!(open_tier(&s, &d), Tier::T2);
        assert!(ev.iter().any(|e| matches!(e, TheatreEvent::TierOpened(Tier::T2))));
    }

    #[test]
    fn clock_expiry_starts_next_act_in_retreat() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        s.clock_hours_left = Some(1.0);
        s.regions.get_mut(&rid("r1")).unwrap().alert = 4.2;
        step_hour(&mut s, &d, &mut ev);
        assert!(ev.iter().any(|e| matches!(e, TheatreEvent::ActLost { act: Act::Act1 })));
        assert_eq!(s.act, Act::Act2);
        assert!(s.retreat);
        assert_eq!(s.clock_hours_left, Some(960.0));
        assert!(s.regions.values().all(|r| r.alert == 2.0));
        // Retreat: Act 2 names no closed cove, so its last cove (C2) closes.
        assert_eq!(s.open_coves, vec![id("C1")]);
        // Region 2 opened in Act 2 and was seeded; Act 2 opens tier 4.
        assert!(s.hulls.iter().any(|h| h.region == rid("r2")));
        assert_eq!(open_tier(&s, &d), Tier::T4);
    }

    #[test]
    fn closed_regions_wait_for_their_act() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        for _ in 0..72 { hour(&mut s, &d, &mut ev); }
        assert!(s.hulls.iter().all(|h| h.region != rid("r2")));
        assert!(s.convoys.iter().all(|c| c.to != id("F")));
        start_act(&mut s, Act::Act2, false, &d, &mut ev);
        let pb = s.hulls.iter().filter(|h| h.region == rid("r2") && h.class == ClassId::new("pb")).count();
        assert_eq!(pb, 10);
        assert_eq!(s.regions[&rid("r2")].supply, 100.0);
    }

    #[test]
    fn last_act_restarts_itself_in_retreat() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        start_act(&mut s, Act::Act2, false, &d, &mut ev);
        assert_eq!(s.open_coves, vec![id("C1"), id("C2")]);
        s.clock_hours_left = Some(0.5);
        step_hour(&mut s, &d, &mut ev);
        // No Act 3 data: Act 2 restarts in retreat.
        assert_eq!(s.act, Act::Act2);
        assert!(s.retreat);
    }

    #[test]
    fn capstones_advance_act_and_open_theatre() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        s.ops.offered.insert(OperationId::new("op.raid"), 3);
        apply_result(&mut s, op(&d, "op.cap1"), &result(vec![], 0, 0), &d, &mut ev);
        assert!(ev.iter().any(|e| matches!(e, TheatreEvent::ActWon { act: Act::Act1 })));
        assert_eq!(s.act, Act::Act2);
        assert!(!s.retreat);
        assert!(s.ops.offered.is_empty());
        // The Act 2 row opens Open Theatre.
        apply_result(&mut s, op(&d, "op.cap2"), &result(vec![], 0, 0), &d, &mut ev);
        assert!(s.open_theatre);
        assert_eq!(s.clock_hours_left, None);
        assert!(ev.iter().any(|e| matches!(e, TheatreEvent::OpenTheatreOpened)));
        step_hour(&mut s, &d, &mut ev);
        assert_eq!(s.clock_hours_left, None);
    }

    #[test]
    fn unlock_conditions() {
        let (mut s, d) = started();
        let r1 = rid("r1");
        assert!(unlocked_in(&s, &Unlock::Always, &d));
        assert!(!unlocked_in(&s, &Unlock::SupplyBelow { region: r1.clone(), value: 40.0 }, &d));
        s.regions.get_mut(&r1).unwrap().supply = 39.0;
        assert!(unlocked_in(&s, &Unlock::SupplyBelow { region: r1.clone(), value: 40.0 }, &d));
        s.regions.get_mut(&r1).unwrap().alert = 2.9;
        assert!(!unlocked_in(&s, &Unlock::AlertAtLeast { region: r1.clone(), value: 3 }, &d));
        assert!(!unlocked_in(&s, &Unlock::AlertAtLeast { region: rid("nowhere"), value: 0 }, &d));
        let cap = Unlock::StoryCount(2);
        assert!(!unlocked_in(&s, &cap, &d));
        s.ops.completed = vec![OperationId::new("op.s1"), OperationId::new("op.raid"), OperationId::new("op.s1")];
        assert!(!unlocked_in(&s, &cap, &d));
        s.ops.completed.push(OperationId::new("op.s2"));
        assert!(unlocked_in(&s, &cap, &d));
        assert!(unlocked_in(&s, &Unlock::Completed(vec![OperationId::new("op.s1"), OperationId::new("op.s2")]), &d));
        assert!(!unlocked_in(&s, &Unlock::AceBeaten(AceId::new("ace.one")), &d));
        assert!(unlocked_in(&s, &Unlock::Any(vec![Unlock::AceBeaten(AceId::new("ace.one")), Unlock::Always]), &d));
        assert!(!unlocked_in(&s, &Unlock::All(vec![Unlock::AceBeaten(AceId::new("ace.one")), Unlock::Always]), &d));
        assert!(!unlocked_in(&s, &Unlock::Any(vec![]), &d));
        // The no-data form agrees on conditions that need no campaign data.
        assert!(unlocked(&s, &Unlock::Completed(vec![OperationId::new("op.s2")])));
        // An ace duel won beats the ace.
        let mut ev = Vec::new();
        apply_result(&mut s, op(&d, "op.duel"), &result(vec![], 0, 0), &d, &mut ev);
        assert!(unlocked_in(&s, &Unlock::AceBeaten(AceId::new("ace.one")), &d));
    }

    /// Five hundred hours of the whole group of units, twice, from the same start: identical states.
    #[test]
    fn long_run_is_deterministic() {
        let run = || {
            let (mut s, d) = started();
            let mut ev = Vec::new();
            for h in 0..500u64 {
                if h % 50 == 10 {
                    let mut r = result(vec![super::super::alert::tests::kill(h % 100 == 10)], 1, 2);
                    r.operation = Some(OperationId::new("op.raid"));
                    super::super::alert::apply_result(&mut s, &rid("r1"), &r, &d, &mut ev);
                    super::super::adaptation::record_kills(&mut s, &rid("r1"), &r.kills, &d, &mut ev);
                    admiral::observe_result(&mut s, &id("S1"), &r, &d);
                    apply_result(&mut s, op(&d, "op.raid"), &r, &d, &mut ev);
                    if h == 110 { logistics::cut_lane(&mut s, &id("Y"), &id("S1"), &d, &mut ev); }
                    if h == 210 { logistics::strike_depot(&mut s, &id("D"), &d, &mut ev); }
                }
                hour(&mut s, &d, &mut ev);
            }
            (s, ev)
        };
        let (a, ea) = run();
        let (b, eb) = run();
        assert_eq!(a, b);
        assert_eq!(ea, eb);
        assert!(a.clock_hours_left.is_some());
    }

    /// Bad input never panics: an empty chart, unknown ids and a garbage state.
    #[test]
    fn robust_to_empty_and_unknown() {
        let d = TheatreData { campaign: Arc::new(CampaignData::default()), catalogue: Arc::new(Catalogue::default()) };
        let mut s = state();
        let mut ev = Vec::new();
        start_act(&mut s, Act::Act3, true, &d, &mut ev);
        s.sextant_node = id("nowhere");
        s.hunter_group = Some(999);
        s.groups.push(content::save::TaskGroupState { id: 5, hulls: vec![1, 2], node: id("x"), destination: Some(id("y")), order: GroupOrder::Patrol, arrive_hour: Some(0), belief: vec![f32::NAN; 3] });
        for _ in 0..30 { hour(&mut s, &d, &mut ev); }
        let r = result(vec![super::super::alert::tests::kill(true)], 3, 3);
        super::super::alert::apply_result(&mut s, &rid("r9"), &r, &d, &mut ev);
        admiral::observe_result(&mut s, &id("zz"), &r, &d);
        logistics::cut_lane(&mut s, &id("a"), &id("b"), &d, &mut ev);
        logistics::strike_depot(&mut s, &id("a"), &d, &mut ev);
        assert_eq!(logistics::transit_hours(&s, &d, &id("a"), &id("b"), true), None);
        let _ = super::super::alert::hunter_eta_hours(&s);
        // The test chart, but with nonsense state.
        let (mut s, d) = started();
        s.regions.get_mut(&rid("r1")).unwrap().alert = f64::NAN;
        s.regions.get_mut(&rid("r1")).unwrap().delivered = vec![f64::NAN; 9];
        s.clock_hours_left = Some(f64::INFINITY);
        for h in s.hulls.iter_mut() { h.fuel = f64::NAN; h.node = id("void"); }
        for _ in 0..30 { hour(&mut s, &d, &mut ev); }
    }
}
