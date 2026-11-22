//! Unit `sys-theatre-alert`. Regional Alert and the hunter-killer group. DESIGN.md L749, L885 (gate).
//!
//! Alert is a real number 0..=5 per region, displayed and compared as its floor, decaying by 1 per 48 quiet
//! hours. Increments in the region where the event happens: a kill observed or reported by a surviving
//! Combine unit +1.0; an unobserved kill +0.5, applied when the loss is noticed 12 theatre hours later; a
//! sighting (any Combine unit holds LANTERN as Suspect or Hostile) +0.5 per contact, capped at 1.0 per
//! operation; evidence (wreck found, intercepted emission, detected RHIB) +0.25 each, capped at 1.0 per
//! operation. At Alert 3 and above escorts, air patrols and decoy convoys appear. At Alert 5 a hunter-killer
//! group sorties against SEXTANT: one task group of the act's top enemy tier, led by an ace not yet beaten in
//! the act if any; the briefing warns at least 24 hours ahead; if it reaches SEXTANT's cove the next
//! operation is a forced "surviving a hunter group" that cannot be declined.
//!
//! # Rules where the spec is silent
//! - Every kill record counts (lost, disabled or captured). Increments raise Alert up to 5 and reset the
//!   region's quiet-hour count; decay runs only while Alert is above 0.
//! - The hunter group launches when a region's Alert *reaches* 5 (its floor rises to 5 from below) and no
//!   hunter group is at sea. Alert held at 5 does not launch another: it must fall below 5 and climb again.
//!   This is the re-arm rule that keeps a surviving player from facing back-to-back hunter groups; the
//!   result that resolves a hunt never launches a new one, even if its kills carry Alert across 5.
//! - Composition: alive, available combat hulls of the act's top tier, nearest to SEXTANT first; any
//!   shortfall is filled by new records of the act's top-tier class (the Combine sends its hunter-killers from
//!   the rear). The ace's own hull record leads it, created if the ace has none.
//! - It forms at the Combine base nearest SEXTANT and arrives after the lane transit or
//!   `CampaignRules::hunter_warning_h`, whichever is longer. It chases SEXTANT: if SEXTANT moves, the group
//!   re-routes from its last node and the arrival can only move later, so moving buys time.
//! - Forced operation pending = the hunter group sits at `sextant_node` with no destination
//!   ([`hunter_at_cove`]); opgen offers the forced operation from that state. When a hunter-group survival
//!   operation resolves while the group is at the cove, the group is released to the Admiral (order
//!   `Return`) and `TheatreState::hunter_group` clears. A hunter group whose hulls are all sunk also clears.
use super::logistics::{self, Net};
use super::{TheatreData, TheatreEvent};
use crate::prelude::*;
use crate::protocol::config::{Outcome, SortieResult};
use content::campaign::{MissionType, OperationKind};
use content::save::{GroupOrder, HullRecord, RegionState, TaskGroupState, TheatreState};
use super::AlertCause;

/// Apply a sortie's kills, sightings and evidence to its region (with the per-operation caps and the delayed
/// unobserved kills). Unit: `sys-theatre-alert`.
pub fn apply_result(state: &mut TheatreState, region: &RegionId, result: &SortieResult, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    if result.outcome == Outcome::Aborted { return; }
    // A hunter-group survival operation fought at the cove ends that hunt.
    let hunt_op = result.operation.as_ref().and_then(|op| data.campaign.operations.get(op)).is_some_and(|op| op.kind == OperationKind::Template(MissionType::HunterGroupSurvival));
    // The call that resolves a hunt never launches the next one, whatever its kills do to Alert.
    let resolved = hunt_op && hunter_at_cove(state);
    if resolved { release_hunter(state); }

    if !data.campaign.theatre.regions.iter().any(|r| &r.id == region) { return; }
    let rules = &data.campaign.rules;
    let notice = state.hour.saturating_add(rules.alert_unobserved_delay_h as u64);
    let mut observed = 0.0;
    let mut pending = Vec::new();
    for k in &result.kills {
        if k.observed { observed += rules.alert_observed_kill; } else { pending.push((notice, rules.alert_unobserved_kill)); }
    }
    let sighting = (result.sightings as f64 * rules.alert_sighting).min(rules.alert_sighting_cap);
    let evidence = (result.evidence as f64 * rules.alert_evidence).min(rules.alert_evidence_cap);
    state.regions.entry(region.clone()).or_default().pending_alert.extend(pending);
    raise(state, region, observed, AlertCause::ObservedKill, !resolved, data, events);
    raise(state, region, sighting, AlertCause::Sighting, !resolved, data, events);
    raise(state, region, evidence, AlertCause::Evidence, !resolved, data, events);
}

/// One hour: quiet-hour decay, pending unobserved kills that are now noticed, escalation at 3, hunter group
/// launch at 5 and its progress toward SEXTANT. Unit: `sys-theatre-alert`.
pub fn step_hour(state: &mut TheatreState, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    let hour = state.hour;
    let rules = &data.campaign.rules;
    let ids: Vec<RegionId> = state.regions.keys().cloned().collect();
    for region in ids {
        let Some(r) = state.regions.get_mut(&region) else { continue };
        let mut noticed = 0.0;
        r.pending_alert.retain(|&(at, amount)| {
            if at <= hour { if amount.is_finite() { noticed += amount.max(0.0); } false } else { true }
        });
        if noticed > 0.0 {
            raise(state, &region, noticed, AlertCause::UnobservedKillNoticed, true, data, events);
            continue;
        }
        let Some(r) = state.regions.get_mut(&region) else { continue };
        if !r.alert.is_finite() { r.alert = 0.0; }
        if r.alert <= 0.0 { r.alert = 0.0; r.quiet_hours = 0; continue; }
        r.quiet_hours = r.quiet_hours.saturating_add(1);
        if r.quiet_hours >= rules.alert_decay_quiet_h.max(1) {
            r.quiet_hours = 0;
            r.alert = (r.alert - 1.0).max(0.0);
            events.push(TheatreEvent::AlertChanged { region: region.clone(), alert: r.alert, cause: AlertCause::QuietDecay });
        }
    }
    step_hunter(state, data, events);
}

/// Displayed alert level.
pub fn alert_floor(alert: f64) -> u8 { alert.clamp(0.0, 5.0).floor() as u8 }

/// Hours until the hunter group arrives, if one is at sea. Unit: `sys-theatre-alert`.
pub fn hunter_eta_hours(state: &TheatreState) -> Option<u32> {
    let g = hunter(state)?;
    g.destination.as_ref()?;
    let at = g.arrive_hour?;
    Some(at.saturating_sub(state.hour).min(u32::MAX as u64) as u32)
}

/// True when the hunter group has reached SEXTANT's cove: the next operation is the forced hunter-group
/// survival operation, which cannot be declined.
pub fn hunter_at_cove(state: &TheatreState) -> bool {
    hunter(state).is_some_and(|g| g.destination.is_none() && g.node == state.sextant_node)
}

// ---------------------------------------------------------------------------------------------------------

fn hunter(state: &TheatreState) -> Option<&TaskGroupState> {
    let id = state.hunter_group?;
    state.groups.iter().find(|g| g.id == id)
}

/// Add `amount` to a region's Alert (capped at 5); escalation and hunter launch on crossing 3 and 5.
fn raise(state: &mut TheatreState, region: &RegionId, amount: f64, cause: AlertCause, may_launch: bool, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    if amount.is_nan() || amount <= 0.0 || !amount.is_finite() { return; }
    let r: &mut RegionState = state.regions.entry(region.clone()).or_default();
    if !r.alert.is_finite() { r.alert = 0.0; }
    let before = alert_floor(r.alert);
    r.alert = (r.alert + amount).clamp(0.0, 5.0);
    r.quiet_hours = 0;
    let after = alert_floor(r.alert);
    events.push(TheatreEvent::AlertChanged { region: region.clone(), alert: r.alert, cause });
    let esc = data.campaign.rules.alert_escalation.min(5);
    if before < esc && after >= esc { events.push(TheatreEvent::AlertEscalated { region: region.clone() }); }
    if may_launch && before < 5 && after >= 5 { launch_hunter(state, data, events); }
}

/// The ace who leads the hunter group: the first of the act's aces not yet beaten.
fn hunter_ace(state: &TheatreState, data: &TheatreData) -> Option<AceId> {
    let act = data.campaign.acts.iter().find(|a| a.act == state.act)?;
    act.aces.iter().find(|a| !state.ops.aces_beaten.contains(a) && data.campaign.aces.contains_key(*a)).cloned()
}

fn launch_hunter(state: &mut TheatreState, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    if let Some(id) = state.hunter_group {
        if state.groups.iter().any(|g| g.id == id) { return; }
        state.hunter_group = None;
    }
    let net = Net::new(data);
    let Some(sextant) = net.idx(&state.sextant_node) else { return };
    let size = (data.campaign.rules.task_group_hulls as usize).max(1);
    let classes = &data.campaign.classes;
    let combat = |c: &content::campaign::EnemyClassDef| !logistics::is_noncombat_role(Some(c.role)) && !c.submersible;
    let top_tier = classes.values().filter(|c| c.act == state.act && combat(c)).map(|c| c.tier).max()
        .or_else(|| classes.values().filter(|c| c.act <= state.act && combat(c)).map(|c| c.tier).max());
    let top_class: Option<ClassId> = top_tier.and_then(|t| classes.values().find(|c| c.tier == t && c.act == state.act && combat(c)).or_else(|| classes.values().find(|c| c.tier == t && combat(c))).map(|c| c.id.clone()));

    // Assembly point: the Combine base nearest SEXTANT (by Combine transit from SEXTANT's node).
    let from_sextant = net.combine_hours_from(state, sextant);
    let start = (0..net.len()).filter(|&i| net.is_base(i) && logistics::region_open(state, data, net.region_of(i))).min_by_key(|&i| (from_sextant[i].unwrap_or(u32::MAX), i));

    let ace = hunter_ace(state, data);
    let mut chosen: Vec<u32> = Vec::new();
    if let Some(ace) = &ace {
        if let Some(h) = state.hulls.iter().find(|h| h.alive && h.ace.as_ref() == Some(ace)) { chosen.push(h.id); }
    }
    let busy: BTreeSet<u32> = state.convoys.iter().flat_map(|c| c.hulls.iter().copied()).collect();
    let mut candidates: Vec<(core::cmp::Reverse<Tier>, u32, u32)> = state.hulls.iter()
        .filter(|h| h.alive && h.refit_until_hour.is_none() && h.ace.is_none() && !busy.contains(&h.id) && !chosen.contains(&h.id))
        .filter_map(|h| {
            let c = classes.get(&h.class)?;
            if !combat(c) || top_tier.is_some_and(|t| c.tier != t) { return None; }
            let hours = net.idx(&h.node).and_then(|i| from_sextant[i]).unwrap_or(u32::MAX);
            Some((core::cmp::Reverse(c.tier), hours, h.id))
        })
        .collect();
    candidates.sort();
    for (_, _, id) in candidates { if chosen.len() >= size { break; } chosen.push(id); }

    let start_node: TheatreNodeId = match start {
        Some(i) => net.nodes[i].id.clone(),
        None => chosen.first().and_then(|id| state.hulls.iter().find(|h| h.id == *id)).map_or_else(|| state.sextant_node.clone(), |h| h.node.clone()),
    };
    let start_region = net.idx(&start_node).map_or_else(|| net.region_of(sextant).clone(), |i| net.region_of(i).clone());

    // The ace's hull, if the ace has none at sea.
    let mut new_records: Vec<(ClassId, Option<AceId>)> = Vec::new();
    if let Some(ace) = &ace {
        if !state.hulls.iter().any(|h| h.alive && h.ace.as_ref() == Some(ace)) {
            if let Some(def) = data.campaign.aces.get(ace) { new_records.push((def.ship.clone(), Some(ace.clone()))); }
        }
    }
    if let Some(class) = &top_class {
        while chosen.len() + new_records.len() < size { new_records.push((class.clone(), None)); }
    }
    let gid = state.next_id;
    state.next_id = state.next_id.wrapping_add(1);
    for (class, ace) in new_records {
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.hulls.push(HullRecord { id, class, variant: None, region: start_region.clone(), node: start_node.clone(), group: None, fuel: 1.0, ammunition: 1.0, damage: 0.0, veterancy: 0.0, refit_until_hour: None, alive: true, ace });
        chosen.push(id);
    }
    if chosen.is_empty() { return; }
    // Pull the chosen hulls out of their groups; groups left empty dissolve.
    for g in state.groups.iter_mut() { g.hulls.retain(|h| !chosen.contains(h)); }
    state.groups.retain(|g| !g.hulls.is_empty());
    for h in state.hulls.iter_mut().filter(|h| chosen.contains(&h.id)) {
        h.group = Some(gid);
        h.node = start_node.clone();
        h.region = start_region.clone();
    }
    let transit = logistics::transit_hours(state, data, &start_node, &state.sextant_node, true).unwrap_or_else(|| direct_hours(&net, &start_node, &state.sextant_node));
    let arrives = state.hour + (transit.max(data.campaign.rules.hunter_warning_h).max(1)) as u64;
    let belief = vec![0.0; net.len()];
    state.groups.push(TaskGroupState { id: gid, hulls: chosen, node: start_node, destination: Some(state.sextant_node.clone()), order: GroupOrder::HuntSextant, arrive_hour: Some(arrives), belief });
    state.hunter_group = Some(gid);
    let ace = ace.filter(|a| state.hulls.iter().any(|h| h.alive && h.group == Some(gid) && h.ace.as_ref() == Some(a)));
    events.push(TheatreEvent::HunterGroupSortied { group: gid, arrives_hour: arrives, ace });
}

/// Straight-line Combine hours when no lane route exists (the hunter group sails open water).
fn direct_hours(net: &Net, a: &TheatreNodeId, b: &TheatreNodeId) -> u32 {
    match (net.idx(a), net.idx(b)) {
        (Some(i), Some(j)) => logistics::hours_for(net.direct_km(i, j), logistics::COMBINE_KMH).unwrap_or(1),
        _ => 1,
    }
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

fn step_hunter(state: &mut TheatreState, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    let Some(gid) = state.hunter_group else { return };
    let Some(gi) = state.groups.iter().position(|g| g.id == gid) else { state.hunter_group = None; return };
    let alive = state.groups[gi].hulls.iter().any(|h| state.hulls.iter().any(|r| r.id == *h && r.alive));
    if !alive {
        state.groups.remove(gi);
        state.hunter_group = None;
        return;
    }
    let hour = state.hour;
    let sextant = state.sextant_node.clone();
    let g = &state.groups[gi];
    // Chase SEXTANT if it moved.
    let target_moved = match &g.destination { Some(d) => *d != sextant, None => g.node != sextant };
    if target_moved {
        let net = Net::new(data);
        let from = g.node.clone();
        let transit = logistics::transit_hours(state, data, &from, &sextant, true).unwrap_or_else(|| direct_hours(&net, &from, &sextant)).max(1) as u64;
        let g = &mut state.groups[gi];
        let at = (hour + transit).max(g.arrive_hour.unwrap_or(0));
        g.destination = Some(sextant.clone());
        g.arrive_hour = Some(at);
    }
    let g = &state.groups[gi];
    if g.destination.is_some() && g.arrive_hour.is_some_and(|a| a <= hour) {
        let net = Net::new(data);
        let region = net.idx(&sextant).map(|i| net.region_of(i).clone());
        let g = &mut state.groups[gi];
        g.node = sextant.clone();
        g.destination = None;
        g.arrive_hour = None;
        let members = g.hulls.clone();
        for h in state.hulls.iter_mut().filter(|h| members.contains(&h.id)) {
            h.node = sextant.clone();
            if let Some(r) = &region { h.region = r.clone(); }
        }
        events.push(TheatreEvent::HunterGroupArrived { group: gid });
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::logistics::testkit::*;
    use super::*;
    use crate::protocol::config::{KillFate, KillRecord, SortieStats};
    use content::campaign::{KillMethod, RangeBand};

    pub fn kill(observed: bool) -> KillRecord {
        KillRecord { tick: 0, hull_record: None, class: None, side: SideId(1), tonnage_t: 50.0, build_cost: 100, fate: KillFate::Lost, method: KillMethod::Gunfire, range: RangeBand::Close, time: TimeOfDay::Night, emcon: Emcon::Silent, observed, by_player: None, salvageable: true }
    }

    pub fn result(kills: Vec<KillRecord>, sightings: u16, evidence: u16) -> SortieResult {
        SortieResult { operation: None, region: Some(rid("r1")), seed: 1, outcome: Outcome::Victory, winner: None, duration_ticks: 0, objective_done: true, ghosts: vec![], kills, survivors: vec![], prizes: vec![], aces: vec![], players: vec![], sightings, evidence, photos: 0, survivors_recovered: 0, crew_captured: 0, passengers_delivered: 0, mines_laid: 0, site_destroyed: false, tender_lost: false, highest_rung: 0, belief: vec![], stats: SortieStats::default() }
    }

    fn alert(s: &TheatreState) -> f64 { s.regions[&rid("r1")].alert }

    #[test]
    fn increments_and_caps() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        apply_result(&mut s, &rid("r1"), &result(vec![kill(true)], 5, 9), &d, &mut ev);
        // 1.0 observed + 0.5 x 5 sightings capped at 1.0 + 0.25 x 9 evidence capped at 1.0.
        assert!((alert(&s) - 3.0).abs() < 1e-12);
        assert!(ev.iter().any(|e| matches!(e, TheatreEvent::AlertEscalated { .. })));
        assert_eq!(alert_floor(2.99), 2);
        let mut ev = Vec::new();
        apply_result(&mut s, &rid("r1"), &result(vec![], 1, 1), &d, &mut ev);
        assert!((alert(&s) - 3.75).abs() < 1e-12);
        // Aborted changes nothing.
        let mut r = result(vec![kill(true)], 0, 0);
        r.outcome = Outcome::Aborted;
        apply_result(&mut s, &rid("r1"), &r, &d, &mut ev);
        assert!((alert(&s) - 3.75).abs() < 1e-12);
        // Unknown region: ignored.
        apply_result(&mut s, &rid("nowhere"), &result(vec![kill(true)], 0, 0), &d, &mut ev);
        assert!(!s.regions.contains_key(&rid("nowhere")));
    }

    #[test]
    fn unobserved_kill_noticed_after_12_hours() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        apply_result(&mut s, &rid("r1"), &result(vec![kill(false), kill(false)], 0, 0), &d, &mut ev);
        assert_eq!(alert(&s), 0.0);
        for _ in 0..11 { s.hour += 1; step_hour(&mut s, &d, &mut ev); }
        assert_eq!(alert(&s), 0.0);
        s.hour += 1;
        step_hour(&mut s, &d, &mut ev);
        assert!((alert(&s) - 1.0).abs() < 1e-12);
        assert!(ev.iter().any(|e| matches!(e, TheatreEvent::AlertChanged { cause: AlertCause::UnobservedKillNoticed, .. })));
    }

    #[test]
    fn decays_one_per_48_quiet_hours() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        s.regions.get_mut(&rid("r1")).unwrap().alert = 2.5;
        for _ in 0..47 { s.hour += 1; step_hour(&mut s, &d, &mut ev); }
        assert!((alert(&s) - 2.5).abs() < 1e-12);
        s.hour += 1;
        step_hour(&mut s, &d, &mut ev);
        assert!((alert(&s) - 1.5).abs() < 1e-12);
        for _ in 0..96 { s.hour += 1; step_hour(&mut s, &d, &mut ev); }
        assert_eq!(alert(&s), 0.0);
    }

    #[test]
    fn hunter_group_launch_arrival_and_rearm() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        s.regions.get_mut(&rid("r1")).unwrap().alert = 4.0;
        apply_result(&mut s, &rid("r1"), &result(vec![kill(true)], 0, 0), &d, &mut ev);
        let gid = s.hunter_group.expect("hunter group launched");
        let g = s.groups.iter().find(|g| g.id == gid).unwrap();
        assert_eq!(g.hulls.len(), 5);
        assert_eq!(g.order, GroupOrder::HuntSextant);
        // Led by the act's unbeaten ace; the rest are the act's top tier (frigates).
        assert!(ev.iter().any(|e| matches!(e, TheatreEvent::HunterGroupSortied { ace: Some(_), .. })));
        for h in &g.hulls {
            let rec = s.hulls.iter().find(|r| r.id == *h).unwrap();
            assert!(rec.ace.is_some() || rec.class == ClassId::new("frig"));
        }
        let eta = hunter_eta_hours(&s).unwrap();
        assert!(eta >= 24);
        for _ in 0..eta { s.hour += 1; step_hour(&mut s, &d, &mut ev); }
        assert!(hunter_at_cove(&s));
        assert_eq!(hunter_eta_hours(&s), None);
        // Surviving the forced operation releases the group; Alert still at 5 does not relaunch.
        let mut r = result(vec![kill(true)], 0, 0);
        r.operation = Some(OperationId::new("op.hunt"));
        apply_result(&mut s, &rid("r1"), &r, &d, &mut ev);
        assert!(s.hunter_group.is_none());
        assert_eq!(alert_floor(alert(&s)), 5);
        s.hour += 1;
        step_hour(&mut s, &d, &mut ev);
        assert!(s.hunter_group.is_none());
    }

    #[test]
    fn resolving_fight_never_relaunches() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        s.regions.get_mut(&rid("r1")).unwrap().alert = 4.0;
        apply_result(&mut s, &rid("r1"), &result(vec![kill(true)], 0, 0), &d, &mut ev);
        let eta = hunter_eta_hours(&s).unwrap();
        for _ in 0..eta { s.hour += 1; step_hour(&mut s, &d, &mut ev); }
        assert!(hunter_at_cove(&s));
        // SEXTANT waited: Alert decayed below 5 before the forced fight.
        s.regions.get_mut(&rid("r1")).unwrap().alert = 4.2;
        let mut r = result(vec![kill(true)], 0, 0);
        r.operation = Some(OperationId::new("op.hunt"));
        apply_result(&mut s, &rid("r1"), &r, &d, &mut ev);
        assert_eq!(alert_floor(alert(&s)), 5);
        assert!(s.hunter_group.is_none());
        // A later rise from below 5 arms it again.
        s.regions.get_mut(&rid("r1")).unwrap().alert = 4.5;
        apply_result(&mut s, &rid("r1"), &result(vec![kill(true)], 0, 0), &d, &mut ev);
        assert!(s.hunter_group.is_some());
    }

    #[test]
    fn hunter_chases_a_moving_sextant() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        s.regions.get_mut(&rid("r1")).unwrap().alert = 4.5;
        apply_result(&mut s, &rid("r1"), &result(vec![kill(true)], 0, 0), &d, &mut ev);
        let before = hunter_eta_hours(&s).unwrap();
        s.sextant_node = id("S2");
        s.hour += 1;
        step_hour(&mut s, &d, &mut ev);
        let g = s.groups.iter().find(|g| Some(g.id) == s.hunter_group).unwrap();
        assert_eq!(g.destination, Some(id("S2")));
        assert!(hunter_eta_hours(&s).unwrap() + 1 >= before);
        // All hulls sunk: the hunt ends.
        let members = g.hulls.clone();
        for h in s.hulls.iter_mut().filter(|h| members.contains(&h.id)) { h.alive = false; }
        s.hour += 1;
        step_hour(&mut s, &d, &mut ev);
        assert!(s.hunter_group.is_none());
    }
}
