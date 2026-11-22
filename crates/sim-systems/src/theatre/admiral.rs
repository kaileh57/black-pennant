//! Unit `sys-theatre-admiral`. The Admiral (a utility planner that replans every six theatre hours) and the
//! task-group commanders' theatre-level belief maps. DESIGN.md L756-760.
//!
//! About 300 persistent hull records on Normal (200 to 450 by slider), each with fuel, ammunition, damage,
//! veterancy and refits, so crippled groups return weakened. Each task-group commander keeps a belief map of
//! where LANTERN probably is (probability per theatre node), fed by sightings, radar, radio emissions, ESM and
//! missing convoys, and searches its peaks. Decoys and emission control fool a real model. Players can view
//! the overlay after a mission. No enemy remembers individual player actions (L56).
//!
//! # Model (decisions where the spec is silent)
//! - Cadence: [`step_hour`] runs [`replan`] itself whenever `admiral_replan_h` hours have passed since
//!   `TheatreState::last_admiral_plan_hour`; callers need not call `replan` on the cadence. A second `replan`
//!   in the same hour changes nothing material.
//! - Groups move a whole leg at a time: `node` stays the origin until `arrive_hour`, then becomes the
//!   destination. The hunter group (`TheatreState::hunter_group`) is moved by `alert` and never re-ordered here.
//! - Utility: every replan scores each group against the open tasks (refit, intercept and search of its own
//!   belief peak, patrol slots on each region's busiest nodes, escorts for convoys bound to Alert-3 regions,
//!   garrison at the nearest base), with a distance penalty and a small bonus for the current task, and
//!   assigns greedily from the highest score. Patrol slots per region = round(patrol factor x active-card
//!   patrol multiplier x base patrols / task-group size), at least 1. A group with average damage of 40% or
//!   more, fuel at 30% or less, or ammunition at 30% or less returns to the nearest base and refits there.
//! - Upkeep per hour: groups under way burn 1/120 of their fuel, on station 1/240. At a Combine base a hull
//!   repairs 1/72 of full damage, refuels 1/12 and rearms 1/24 per hour, each times the region's patrol factor
//!   (Supply scales Combine repair and refit speeds, L747). `refit_until_hour` carries the estimated finish for
//!   hulls under a `Refit` order (those are unavailable to operations) and is `None` otherwise.
//! - Groups: below 2 live hulls a group dissolves into its region's pool; the Admiral forms pooled combat
//!   hulls into groups of `task_group_hulls` (at least 2) at the region's forward base, spending
//!   [`FORM_SUPPLY_PER_HULL`] tonnes per hull from the region's depots (a region without depots pays nothing;
//!   short stock means no group this replan). Ace hulls stay with their group and an ace's group never
//!   dissolves while the ace lives.
//! - Open Theatre: each shipyard rebuilds at most one sunk record per replan from its built tonnage (the hull
//!   cap of the class's act, as a tonnage proxy). Aces never return.
//! - Beliefs: the support is the nodes of regions open to SEXTANT; the prior is uniform on it. Each hour a
//!   belief diffuses 4% of each node's mass equally along its lanes (LANTERN moves; cuts do not stop it),
//!   loses 40% at a node a group holds on station with nothing found (coves are hidden, so this is honest
//!   negative evidence even where LANTERN is), relaxes 0.5% toward the prior and is renormalised. Evidence
//!   comes only from sortie results and missing convoys, never from where SEXTANT is.
use super::logistics::{self, Net};
use super::{TheatreData, TheatreEvent};
use crate::prelude::*;
use crate::protocol::config::{Outcome, SortieResult};
use content::campaign::{MissionType, NodeKind, OperationKind};
use content::save::{GroupOrder, HullRecord, TaskGroupState, TheatreState};

/// Tonnes of regional supply spent per hull when the Admiral launches a new task group.
pub const FORM_SUPPLY_PER_HULL: f64 = 5.0;
/// Smallest group the Admiral keeps or forms.
const MIN_GROUP: usize = 2;
const DIFFUSION: f64 = 0.04;
const SEARCH_MISS: f64 = 0.6;
const RELAX: f64 = 0.005;

/// Admiral replan (every `CampaignRules::admiral_replan_h`): score and assign group orders (garrison,
/// patrol, escort, search, intercept, return, refit) by utility from supply, alert, losses and beliefs; form
/// new task groups at forward bases that have supply; rebuild from shipyards in Open Theatre.
/// Unit: `sys-theatre-admiral`.
pub fn replan(state: &mut TheatreState, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    let _ = events;
    let repeat = state.hour != 0 && state.last_admiral_plan_hour == state.hour;
    state.last_admiral_plan_hour = state.hour;
    let net = Net::new(data);
    prune_groups(state);
    if net.len() == 0 { return; }
    if state.open_theatre && !repeat { rebuild_from_shipyards(state, data, &net); }
    form_groups(state, data, &net);
    assign_orders(state, data, &net);
}

/// One hour of group movement along lanes, refit and repair progress (scaled by regional supply), and belief
/// diffusion. Runs the replan first when it is due. Unit: `sys-theatre-admiral`.
pub fn step_hour(state: &mut TheatreState, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    let every = data.campaign.rules.admiral_replan_h.max(1) as u64;
    if state.hour >= state.last_admiral_plan_hour.saturating_add(every) { replan(state, data, events); }
    let net = Net::new(data);
    move_groups(state, &net);
    upkeep(state, data, &net);
    diffuse_beliefs(state, data, &net);
}

/// Feed a sortie's outcome into the beliefs: sightings sharpen belief at the operation's node, missing
/// convoys and unobserved losses add weaker evidence, a feint drags belief to its node.
/// Unit: `sys-theatre-admiral`.
///
/// Weight of the evidence at the node: 0.25 per sighting, 0.2 per observed kill, 0.1 per evidence item, 0.08
/// per unobserved kill, capped at 0.85; a feint pulls with at least 0.8. Groups in the operation's region and
/// the hunter group take the full weight, others half. A sortie nobody saw moves no belief.
pub fn observe_result(state: &mut TheatreState, node: &TheatreNodeId, result: &SortieResult, data: &TheatreData) {
    if result.outcome == Outcome::Aborted { return; }
    let net = Net::new(data);
    let Some(at) = net.idx(node) else { return };
    let observed = result.kills.iter().filter(|k| k.observed).count() as f64;
    let unobserved = result.kills.len() as f64 - observed;
    let mut w = 0.25 * result.sightings as f64 + 0.2 * observed + 0.1 * result.evidence as f64 + 0.08 * unobserved;
    w = w.min(0.85);
    let feint = result.operation.as_ref().and_then(|o| data.campaign.operations.get(o)).is_some_and(|o| o.kind == OperationKind::Template(MissionType::Feint));
    if feint { w = w.max(0.8); }
    if w <= 0.0 { return; }
    let region = net.region_of(at).clone();
    let prior = prior(state, data, &net);
    let hunter = state.hunter_group;
    for g in state.groups.iter_mut() {
        ensure_belief(g, &prior);
        let local = Some(g.id) == hunter || net.idx(&g.node).is_some_and(|i| *net.region_of(i) == region);
        add_evidence(&mut g.belief, &net, at, if local { w } else { w * 0.5 });
    }
}

/// Seed the order of battle at campaign or act start: hull records per region scaled by the hull-count
/// slider, grouped into task groups of 5 and convoys. Unit: `sys-theatre-admiral`.
///
/// Idempotent: seeds every region open by `act` (all in Open Theatre) that has no hull record yet, then gives
/// each of the act's aces a hull if it never had one. Counts scale by hull_count / 300, rounded to nearest.
/// Combat hulls are dealt by tier, heaviest first, round-robin into groups at the region's bases; freighters
/// and tankers stay pooled for convoys (the logistics unit sails them).
pub fn seed_order_of_battle(state: &mut TheatreState, data: &TheatreData, act: Act) {
    let net = Net::new(data);
    if net.len() == 0 { return; }
    let prior = prior(state, data, &net);
    let scale = state.difficulty.clamped().hull_count as f64 / 300.0;
    let size = (data.campaign.rules.task_group_hulls as usize).max(1);
    for def in &data.campaign.theatre.regions {
        if !(state.open_theatre || def.act <= act) { continue; }
        if state.hulls.iter().any(|h| h.region == def.id) { continue; }
        let bases = region_bases(&net, &def.id);
        let mut combat: Vec<(core::cmp::Reverse<Tier>, u32)> = Vec::new();
        let mut pooled: Vec<u32> = Vec::new();
        for (class, count) in &def.order_of_battle {
            let n = libm::round(*count as f64 * scale).max(0.0) as u32;
            let role = logistics::class_role(data, class);
            let tier = data.campaign.classes.get(class).map_or(Tier::T1, |c| c.tier);
            for _ in 0..n.min(10_000) {
                let id = state.next_id;
                state.next_id = state.next_id.wrapping_add(1);
                let node = net.nodes[bases[(id as usize) % bases.len()]].id.clone();
                state.hulls.push(HullRecord { id, class: class.clone(), variant: None, region: def.id.clone(), node, group: None, fuel: 1.0, ammunition: 1.0, damage: 0.0, veterancy: 0.0, refit_until_hour: None, alive: true, ace: None });
                if logistics::is_noncombat_role(role) { pooled.push(id); } else { combat.push((core::cmp::Reverse(tier), id)); }
            }
        }
        combat.sort();
        let groups = combat.len().div_ceil(size);
        let mut members: Vec<Vec<u32>> = vec![Vec::new(); groups];
        for (i, (_, id)) in combat.iter().enumerate() { members[i % groups].push(*id); }
        for (k, hulls) in members.into_iter().enumerate() {
            let gid = state.next_id;
            state.next_id = state.next_id.wrapping_add(1);
            let node = net.nodes[bases[k % bases.len()]].id.clone();
            for h in state.hulls.iter_mut().filter(|h| hulls.contains(&h.id)) { h.group = Some(gid); h.node = node.clone(); }
            state.groups.push(TaskGroupState { id: gid, hulls, node, destination: None, order: GroupOrder::Garrison, arrive_hour: None, belief: prior.clone() });
        }
    }
    seed_aces(state, data, &net, act);
}

/// A convoy went missing (wholly or `fraction` of it): weak evidence along its route for the commanders of
/// the regions it sailed through. Called by `logistics`.
pub(crate) fn note_missing_convoy(state: &mut TheatreState, data: &TheatreData, from: &TheatreNodeId, to: &TheatreNodeId, fraction: f64) {
    let net = Net::new(data);
    let (Some(a), Some(b)) = (net.idx(from), net.idx(to)) else { return };
    let w = 0.12 * fraction.clamp(0.0, 1.0);
    if w <= 0.0 { return; }
    let prior = prior(state, data, &net);
    let (ra, rb) = (net.region_of(a).clone(), net.region_of(b).clone());
    for g in state.groups.iter_mut() {
        ensure_belief(g, &prior);
        let local = net.idx(&g.node).is_some_and(|i| *net.region_of(i) == ra || *net.region_of(i) == rb);
        let wg = if local { w } else { w * 0.5 };
        add_evidence(&mut g.belief, &net, a, wg * 0.5);
        add_evidence(&mut g.belief, &net, b, wg * 0.5);
    }
}

// ---------------------------------------------------------------------------------------------------------
// Beliefs.

/// Uniform prior over the nodes of regions open to SEXTANT (every node when none is open).
fn prior(state: &TheatreState, data: &TheatreData, net: &Net) -> Vec<f32> {
    let n = net.len();
    let mask: Vec<bool> = (0..n).map(|i| logistics::region_open(state, data, net.region_of(i))).collect();
    let k = mask.iter().filter(|m| **m).count();
    if k == 0 { return vec![if n == 0 { 0.0 } else { 1.0 / n as f32 }; n]; }
    mask.iter().map(|&m| if m { 1.0 / k as f32 } else { 0.0 }).collect()
}

fn ensure_belief(g: &mut TaskGroupState, prior: &[f32]) {
    let ok = g.belief.len() == prior.len() && g.belief.iter().all(|v| v.is_finite() && *v >= 0.0) && g.belief.iter().map(|v| *v as f64).sum::<f64>() > 1e-6;
    if !ok { g.belief = prior.to_vec(); }
}

fn normalise(b: &mut [f64]) {
    let s: f64 = b.iter().sum();
    if s > 0.0 && s.is_finite() { for v in b.iter_mut() { *v /= s; } }
}

/// Mix `w` of a node-centred bump (70% at the node, 30% on its neighbours) into a belief.
fn add_evidence(belief: &mut [f32], net: &Net, at: usize, w: f64) {
    if belief.len() != net.len() || at >= belief.len() { return; }
    let w = w.clamp(0.0, 1.0);
    let mut bump = vec![0.0f64; belief.len()];
    let nb = &net.adj[at];
    if nb.is_empty() { bump[at] = 1.0; } else {
        bump[at] = 0.7;
        for e in nb { bump[e.to] += 0.3 / nb.len() as f64; }
    }
    let mut b: Vec<f64> = belief.iter().map(|v| *v as f64).collect();
    for (v, x) in b.iter_mut().zip(bump) { *v = (1.0 - w) * *v + w * x; }
    normalise(&mut b);
    for (dst, v) in belief.iter_mut().zip(b) { *dst = v as f32; }
}

fn diffuse_beliefs(state: &mut TheatreState, data: &TheatreData, net: &Net) {
    let n = net.len();
    if n == 0 { return; }
    let prior = prior(state, data, net);
    let support: Vec<bool> = prior.iter().map(|p| *p > 0.0).collect();
    for g in state.groups.iter_mut() {
        ensure_belief(g, &prior);
        let b: Vec<f64> = g.belief.iter().map(|v| *v as f64).collect();
        let mut out = vec![0.0f64; n];
        for i in 0..n {
            let nbs: Vec<usize> = net.adj[i].iter().map(|e| e.to).filter(|&j| support[j]).collect();
            if nbs.is_empty() { out[i] += b[i]; continue; }
            out[i] += b[i] * (1.0 - DIFFUSION);
            for j in &nbs { out[*j] += b[i] * DIFFUSION / nbs.len() as f64; }
        }
        if g.destination.is_none() && matches!(g.order, GroupOrder::Patrol | GroupOrder::Search | GroupOrder::Intercept | GroupOrder::Escort) {
            if let Some(i) = net.idx(&g.node) { out[i] *= SEARCH_MISS; }
        }
        for i in 0..n {
            let p = prior[i] as f64;
            out[i] = if support[i] { (1.0 - RELAX) * out[i] + RELAX * p } else { 0.0 };
        }
        normalise(&mut out);
        g.belief = out.into_iter().map(|v| v as f32).collect();
    }
}

// ---------------------------------------------------------------------------------------------------------
// Groups.

/// Combine bases of a region (forward bases first), else its non-cove nodes, else any of its nodes, else
/// every node. Never empty while the chart has nodes.
fn region_bases(net: &Net, region: &RegionId) -> Vec<usize> {
    let in_region: Vec<usize> = (0..net.len()).filter(|&i| net.region_of(i) == region).collect();
    let mut fwd: Vec<usize> = in_region.iter().copied().filter(|&i| matches!(net.nodes[i].kind, NodeKind::ForwardBase)).collect();
    fwd.extend(in_region.iter().copied().filter(|&i| net.is_base(i) && !matches!(net.nodes[i].kind, NodeKind::ForwardBase)));
    if !fwd.is_empty() { return fwd; }
    let open: Vec<usize> = in_region.iter().copied().filter(|&i| !matches!(net.nodes[i].kind, NodeKind::Cove)).collect();
    if !open.is_empty() { return open; }
    if !in_region.is_empty() { return in_region; }
    (0..net.len()).collect()
}

fn seed_aces(state: &mut TheatreState, data: &TheatreData, net: &Net, act: Act) {
    let Some(def) = data.campaign.acts.iter().find(|a| a.act == act) else { return };
    for ace in &def.aces {
        if state.hulls.iter().any(|h| h.ace.as_ref() == Some(ace)) { continue; }
        let Some(ace_def) = data.campaign.aces.get(ace) else { continue };
        // The ace's home region: the node of its first operation this act, else the act's first region.
        let region = ace_def.arcs.iter().filter(|a| a.act == act).flat_map(|a| a.operations.iter())
            .filter_map(|o| data.campaign.operations.get(o).and_then(|o| o.node.as_ref()))
            .filter_map(|n| net.idx(n)).map(|i| net.region_of(i).clone()).next()
            .or_else(|| def.regions.first().cloned())
            .or_else(|| data.campaign.theatre.regions.first().map(|r| r.id.clone()));
        let Some(region) = region else { continue };
        let bases = region_bases(net, &region);
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        // Lead the smallest group of the region, if it has one.
        let group = state.groups.iter_mut().filter(|g| net.idx(&g.node).is_some_and(|i| *net.region_of(i) == region) && Some(g.id) != state.hunter_group).min_by_key(|g| (g.hulls.len(), g.id));
        let (node, gid) = match group {
            Some(g) => { g.hulls.insert(0, id); (g.node.clone(), Some(g.id)) }
            None => (net.nodes[bases[0]].id.clone(), None),
        };
        state.hulls.push(HullRecord { id, class: ace_def.ship.clone(), variant: None, region, node, group: gid, fuel: 1.0, ammunition: 1.0, damage: 0.0, veterancy: 0.0, refit_until_hour: None, alive: true, ace: Some(ace.clone()) });
    }
}

fn prune_groups(state: &mut TheatreState) {
    let hunter = state.hunter_group;
    let alive: BTreeMap<u32, bool> = state.hulls.iter().map(|h| (h.id, h.alive)).collect();
    let has_ace: BTreeSet<u32> = state.hulls.iter().filter(|h| h.alive && h.ace.is_some()).map(|h| h.id).collect();
    let mut freed: Vec<u32> = Vec::new();
    let mut dissolved: Vec<u32> = Vec::new();
    for g in state.groups.iter_mut() {
        let gid = g.id;
        g.hulls.retain(|h| alive.get(h).copied().unwrap_or(false));
        g.hulls.dedup();
        if Some(gid) == hunter { continue; }
        let ace_led = g.hulls.iter().any(|h| has_ace.contains(h));
        if g.hulls.len() < MIN_GROUP && !ace_led {
            freed.extend(g.hulls.iter().copied());
            dissolved.push(gid);
        }
    }
    state.groups.retain(|g| !dissolved.contains(&g.id) && (!g.hulls.is_empty() || Some(g.id) == hunter));
    let live_groups: BTreeSet<u32> = state.groups.iter().map(|g| g.id).collect();
    for h in state.hulls.iter_mut() {
        if freed.contains(&h.id) || h.group.is_some_and(|g| !live_groups.contains(&g)) || !h.alive { h.group = None; }
    }
    // Hull lists are the truth for membership.
    let membership: BTreeMap<u32, u32> = state.groups.iter().flat_map(|g| g.hulls.iter().map(move |h| (*h, g.id))).collect();
    for h in state.hulls.iter_mut() { if h.alive { h.group = membership.get(&h.id).copied(); } }
}

fn rebuild_from_shipyards(state: &mut TheatreState, data: &TheatreData, net: &Net) {
    for i in 0..net.len() {
        if !matches!(net.nodes[i].kind, NodeKind::Shipyard { .. }) { continue; }
        let yard = net.nodes[i].id.clone();
        let stock = state.depots.get(&yard).copied().unwrap_or(0.0);
        let Some(rec) = state.hulls.iter_mut().filter(|h| !h.alive && h.ace.is_none() && data.campaign.classes.contains_key(&h.class)).min_by_key(|h| h.id) else { return };
        let cost = data.campaign.classes.get(&rec.class).map_or(150.0, |c| c.act.hull_cap_t());
        if stock + 1e-9 < cost { continue; }
        *rec = HullRecord { id: rec.id, class: rec.class.clone(), variant: rec.variant.clone(), region: net.region_of(i).clone(), node: yard.clone(), group: None, fuel: 1.0, ammunition: 1.0, damage: 0.0, veterancy: 0.0, refit_until_hour: None, alive: true, ace: None };
        state.depots.insert(yard, stock - cost);
    }
}

fn form_groups(state: &mut TheatreState, data: &TheatreData, net: &Net) {
    let size = (data.campaign.rules.task_group_hulls as usize).max(MIN_GROUP);
    let busy: BTreeSet<u32> = state.convoys.iter().flat_map(|c| c.hulls.iter().copied()).collect();
    let prior = prior(state, data, net);
    for def in &data.campaign.theatre.regions {
        let mut pool: Vec<u32> = state.hulls.iter()
            .filter(|h| h.alive && h.group.is_none() && h.region == def.id && h.refit_until_hour.is_none() && !busy.contains(&h.id) && !logistics::is_noncombat_role(logistics::class_role(data, &h.class)))
            .map(|h| h.id).collect();
        pool.sort();
        let base = net.nodes[region_bases(net, &def.id)[0]].id.clone();
        for chunk in pool.chunks(size) {
            if chunk.len() < MIN_GROUP { break; }
            if !logistics::spend_region_supply(state, data, &def.id, FORM_SUPPLY_PER_HULL * chunk.len() as f64) { break; }
            let gid = state.next_id;
            state.next_id = state.next_id.wrapping_add(1);
            for h in state.hulls.iter_mut().filter(|h| chunk.contains(&h.id)) { h.group = Some(gid); h.node = base.clone(); }
            state.groups.push(TaskGroupState { id: gid, hulls: chunk.to_vec(), node: base.clone(), destination: None, order: GroupOrder::Garrison, arrive_hour: None, belief: prior.clone() });
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TaskKind { Patrol, Escort, Search, Intercept, Garrison }

struct Task { kind: TaskKind, node: usize, cap: u32 }

/// Status of one group for the planner.
struct GroupInfo { gi: usize, node: usize, region: RegionId, damage: f64, fuel: f64, ammo: f64 }

fn group_info(state: &TheatreState, net: &Net, gi: usize) -> Option<GroupInfo> {
    let g = &state.groups[gi];
    let node = net.idx(&g.node)?;
    let recs: Vec<&HullRecord> = g.hulls.iter().filter_map(|h| state.hulls.iter().find(|r| r.id == *h && r.alive)).collect();
    if recs.is_empty() { return None; }
    let n = recs.len() as f64;
    Some(GroupInfo { gi, node, region: net.region_of(node).clone(), damage: recs.iter().map(|r| r.damage).sum::<f64>() / n, fuel: recs.iter().map(|r| r.fuel).fold(f64::INFINITY, f64::min), ammo: recs.iter().map(|r| r.ammunition).sum::<f64>() / n })
}

fn needs_refit(i: &GroupInfo) -> bool { i.damage >= 0.4 || i.fuel <= 0.3 || i.ammo <= 0.3 }

fn repaired(i: &GroupInfo) -> bool { i.damage <= 0.05 && i.fuel >= 0.95 && i.ammo >= 0.95 }

fn alert_of(state: &TheatreState, region: &RegionId) -> f64 { state.regions.get(region).map_or(0.0, |r| if r.alert.is_finite() { r.alert.clamp(0.0, 5.0) } else { 0.0 }) }

fn assign_orders(state: &mut TheatreState, data: &TheatreData, net: &Net) {
    let n = net.len();
    if n == 0 { return; }
    let rules = &data.campaign.rules;
    let floor = rules.patrol_floor;
    let size = (rules.task_group_hulls as f64).max(1.0);
    let hunter = state.hunter_group;
    let prior = prior(state, data, net);
    let support = prior.iter().filter(|p| **p > 0.0).count().max(1) as f64;
    let uniform = 1.0 / support;

    // Tasks.
    let mut tasks: Vec<Task> = Vec::new();
    for def in &data.campaign.theatre.regions {
        let supply = state.regions.get(&def.id).map_or(100.0, |r| r.supply);
        let mut factor = logistics::patrol_factor(supply, floor);
        if let Some(r) = state.regions.get(&def.id) {
            for c in &r.active_cards {
                if let Some(f) = data.campaign.cards.get(c).and_then(|c| c.effect.patrol_density_factor) { if f.is_finite() { factor *= f.clamp(0.0, 4.0); } }
            }
        }
        if def.base_patrols == 0 || !logistics::region_open(state, data, &def.id) { continue; }
        let slots = (libm::round(factor * def.base_patrols as f64 / size) as usize).max(1);
        let mut nodes: Vec<(core::cmp::Reverse<u64>, usize)> = (0..n)
            .filter(|&i| net.region_of(i) == &def.id && !matches!(net.nodes[i].kind, NodeKind::Cove))
            .map(|i| (core::cmp::Reverse((net.adj[i].iter().map(|e| net.lanes[e.lane].traffic).sum::<f64>() * 1000.0) as u64), i))
            .collect();
        nodes.sort();
        if nodes.is_empty() { continue; }
        for k in 0..slots { tasks.push(Task { kind: TaskKind::Patrol, node: nodes[k % nodes.len()].1, cap: 1 }); }
    }
    let esc = rules.alert_escalation;
    for c in &state.convoys {
        if c.decoy { continue; }
        let Some(to) = net.idx(&c.to) else { continue };
        if !logistics::region_open(state, data, net.region_of(to)) { continue; }
        if super::alert::alert_floor(alert_of(state, net.region_of(to))) < esc { continue; }
        match tasks.iter_mut().find(|t| t.kind == TaskKind::Escort && t.node == to) { Some(t) => t.cap += 1, None => tasks.push(Task { kind: TaskKind::Escort, node: to, cap: 1 }) }
    }
    let infos: Vec<GroupInfo> = (0..state.groups.len()).filter(|&gi| Some(state.groups[gi].id) != hunter).filter_map(|gi| group_info(state, net, gi)).collect();
    for info in &infos {
        let g = &state.groups[info.gi];
        if g.belief.len() != n { continue; }
        let (peak, p) = g.belief.iter().enumerate().fold((0usize, -1.0f64), |(bi, bp), (i, v)| if (*v as f64) > bp { (i, *v as f64) } else { (bi, bp) });
        if p >= 2.0 * uniform {
            for kind in [TaskKind::Search, TaskKind::Intercept] {
                if !tasks.iter().any(|t| t.kind == kind && t.node == peak) { tasks.push(Task { kind, node: peak, cap: 2 }); }
            }
        }
    }
    let base_nodes: Vec<usize> = (0..n).filter(|&i| net.is_base(i) && logistics::region_open(state, data, net.region_of(i))).collect();
    for &b in &base_nodes { tasks.push(Task { kind: TaskKind::Garrison, node: b, cap: u32::MAX }); }

    // Forced refits and running refits first.
    let mut assigned: Vec<Option<(GroupOrder, usize)>> = vec![None; state.groups.len()];
    let hours_from: Vec<Vec<Option<u32>>> = infos.iter().map(|i| net.combine_hours_from(state, i.node)).collect();
    for (k, info) in infos.iter().enumerate() {
        let g = &state.groups[info.gi];
        let refitting = matches!(g.order, GroupOrder::Refit | GroupOrder::Return) && !repaired(info);
        if !(needs_refit(info) || refitting) { continue; }
        if g.order == GroupOrder::Return && g.destination.is_some() { assigned[info.gi] = Some((GroupOrder::Return, net.idx(g.destination.as_ref().unwrap_or(&g.node)).unwrap_or(info.node))); continue; }
        let target = if net.is_base(info.node) { Some(info.node) } else {
            base_nodes.iter().copied().filter(|&b| hours_from[k][b].is_some()).min_by_key(|&b| (net.region_of(b) != &info.region, hours_from[k][b], b))
        };
        if let Some(t) = target { assigned[info.gi] = Some((if t == info.node { GroupOrder::Refit } else { GroupOrder::Return }, t)); }
    }

    // Greedy utility assignment.
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (k, info) in infos.iter().enumerate() {
        if assigned[info.gi].is_some() { continue; }
        let g = &state.groups[info.gi];
        let current_target = g.destination.as_ref().and_then(|d| net.idx(d)).unwrap_or(info.node);
        for (ti, t) in tasks.iter().enumerate() {
            let Some(h) = hours_from[k][t.node] else { continue };
            let h = h as f64;
            let alert = alert_of(state, net.region_of(t.node));
            let b = g.belief.get(t.node).map_or(0.0, |v| *v as f64);
            let mut score = match t.kind {
                TaskKind::Patrol => 0.5 + 0.05 * alert - 0.004 * h,
                TaskKind::Escort => 0.6 + 0.1 * alert - 0.004 * h,
                TaskKind::Search => if b >= 2.0 * uniform { 0.45 + 0.4 * (b / (4.0 * uniform)).min(1.0) - 0.005 * h } else { continue },
                TaskKind::Intercept => if b >= 0.3 && h <= 48.0 { 0.9 + b - 0.008 * h } else { continue },
                TaskKind::Garrison => 0.2 - 0.004 * h,
            };
            if t.node == current_target && order_of(t.kind) == g.order { score += 0.1; }
            pairs.push((score, info.gi, ti));
        }
    }
    pairs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(core::cmp::Ordering::Equal).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut used = vec![0u32; tasks.len()];
    for (_, gi, ti) in pairs {
        if assigned[gi].is_some() || used[ti] >= tasks[ti].cap { continue; }
        used[ti] += 1;
        assigned[gi] = Some((order_of(tasks[ti].kind), tasks[ti].node));
    }

    // Apply.
    let hour = state.hour;
    for (k, info) in infos.iter().enumerate() {
        let Some((order, target)) = assigned[info.gi] else { continue };
        let g = &mut state.groups[info.gi];
        g.order = order;
        if target == info.node {
            g.destination = None;
            g.arrive_hour = None;
        } else if g.destination.as_ref().and_then(|d| net.idx(d)) != Some(target) {
            let Some(h) = hours_from[k][target] else { continue };
            g.destination = Some(net.nodes[target].id.clone());
            g.arrive_hour = Some(hour + (h.max(1)) as u64);
        }
    }
}

fn order_of(kind: TaskKind) -> GroupOrder {
    match kind { TaskKind::Patrol => GroupOrder::Patrol, TaskKind::Escort => GroupOrder::Escort, TaskKind::Search => GroupOrder::Search, TaskKind::Intercept => GroupOrder::Intercept, TaskKind::Garrison => GroupOrder::Garrison }
}

fn move_groups(state: &mut TheatreState, net: &Net) {
    let hour = state.hour;
    let hunter = state.hunter_group;
    let mut moved: Vec<(Vec<u32>, TheatreNodeId)> = Vec::new();
    for g in state.groups.iter_mut() {
        if Some(g.id) == hunter { continue; }
        let Some(dest) = g.destination.clone() else { continue };
        if g.arrive_hour.is_some_and(|a| a > hour) { continue; }
        g.node = dest.clone();
        g.destination = None;
        g.arrive_hour = None;
        if g.order == GroupOrder::Return && net.idx(&dest).is_some_and(|i| net.is_base(i)) { g.order = GroupOrder::Refit; }
        moved.push((g.hulls.clone(), dest));
    }
    for (hulls, dest) in moved {
        let region = net.idx(&dest).map(|i| net.region_of(i).clone());
        for h in state.hulls.iter_mut().filter(|h| hulls.contains(&h.id)) {
            h.node = dest.clone();
            if let Some(r) = &region { h.region = r.clone(); }
        }
    }
}

fn upkeep(state: &mut TheatreState, data: &TheatreData, net: &Net) {
    let hour = state.hour;
    let floor = data.campaign.rules.patrol_floor;
    let group_state: BTreeMap<u32, (bool, GroupOrder)> = state.groups.iter().map(|g| (g.id, (g.destination.is_some(), g.order))).collect();
    let factors: BTreeMap<RegionId, f64> = state.regions.iter().map(|(k, r)| (k.clone(), logistics::patrol_factor(r.supply, floor))).collect();
    let mut finished: BTreeSet<u32> = BTreeSet::new();
    let mut still: BTreeSet<u32> = BTreeSet::new();
    for h in state.hulls.iter_mut().filter(|h| h.alive) {
        for v in [&mut h.fuel, &mut h.ammunition, &mut h.damage] { if !v.is_finite() { *v = 0.0; } *v = v.clamp(0.0, 1.0); }
        let (moving, order) = h.group.and_then(|g| group_state.get(&g).copied()).unwrap_or((false, GroupOrder::Garrison));
        let at_base = !moving && net.idx(&h.node).is_some_and(|i| net.is_base(i));
        if at_base {
            let f = factors.get(&h.region).copied().unwrap_or(1.0).max(0.01);
            h.damage = (h.damage - f / 72.0).max(0.0);
            h.fuel = (h.fuel + f / 12.0).min(1.0);
            h.ammunition = (h.ammunition + f / 24.0).min(1.0);
            if order == GroupOrder::Refit {
                let left = (h.damage * 72.0).max((1.0 - h.fuel) * 12.0).max((1.0 - h.ammunition) * 24.0) / f;
                h.refit_until_hour = Some(hour + libm::ceil(left).max(0.0) as u64);
                if let Some(g) = h.group { if h.damage <= 0.05 && h.fuel >= 0.95 && h.ammunition >= 0.95 { finished.insert(g); } else { still.insert(g); } }
            } else {
                h.refit_until_hour = None;
            }
        } else {
            h.fuel = (h.fuel - if moving { 1.0 / 120.0 } else if h.group.is_some() { 1.0 / 240.0 } else { 0.0 }).max(0.0);
            if order != GroupOrder::Refit { h.refit_until_hour = None; }
        }
    }
    // A group whose every hull is repaired leaves refit and garrisons until the next replan.
    for g in state.groups.iter_mut() {
        if g.order == GroupOrder::Refit && finished.contains(&g.id) && !still.contains(&g.id) {
            g.order = GroupOrder::Garrison;
            let members = g.hulls.clone();
            for h in state.hulls.iter_mut().filter(|h| members.contains(&h.id)) { h.refit_until_hour = None; }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::alert::tests::{kill, result};
    use super::super::logistics::testkit::*;
    use super::*;

    #[test]
    fn seeding_scales_and_is_idempotent() {
        let (s, d) = started();
        // r1: 20 + 5 + 6 records on Normal plus the ace; r2 opens in Act 2.
        assert_eq!(s.hulls.iter().filter(|h| h.region == rid("r1")).count(), 32);
        assert!(s.hulls.iter().all(|h| h.region != rid("r2")));
        let mut s2 = s.clone();
        seed_order_of_battle(&mut s2, &d, Act::Act1);
        assert_eq!(s2.hulls.len(), s.hulls.len());
        // Freighters stay pooled; combat hulls are in groups of at most 5 plus the ace.
        for h in &s.hulls {
            let role = logistics::class_role(&d, &h.class);
            assert_eq!(h.group.is_none(), logistics::is_convoy_role(role), "hull {} {:?}", h.id, h.class);
        }
        assert!(s.groups.iter().all(|g| g.hulls.len() <= 6));
        let mut hard = state();
        hard.difficulty.hull_count = 450;
        seed_order_of_battle(&mut hard, &d, Act::Act2);
        assert_eq!(hard.hulls.iter().filter(|h| h.region == rid("r1")).count(), 30 + 8 + 9);
        assert_eq!(hard.hulls.iter().filter(|h| h.region == rid("r2")).count(), 15);
    }

    #[test]
    fn beliefs_follow_evidence_not_truth() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        // SEXTANT is at C1; a sortie seen at S2 pulls belief to S2.
        observe_result(&mut s, &id("S2"), &result(vec![kill(true)], 2, 0), &d);
        let net = Net::new(&d);
        let s2 = net.idx(&id("S2")).unwrap();
        for g in &s.groups {
            let peak = g.belief.iter().enumerate().max_by(|a, b| a.1.partial_cmp(b.1).unwrap()).unwrap().0;
            assert_eq!(peak, s2);
            assert!((g.belief.iter().map(|v| *v as f64).sum::<f64>() - 1.0).abs() < 1e-4);
            // Closed regions carry no belief.
            assert_eq!(g.belief[net.idx(&id("F")).unwrap()], 0.0);
        }
        // A sortie nobody saw moves nothing.
        let before: Vec<Vec<f32>> = s.groups.iter().map(|g| g.belief.clone()).collect();
        let mut quiet = result(vec![kill(false)], 0, 0);
        quiet.kills.clear();
        observe_result(&mut s, &id("D"), &quiet, &d);
        assert_eq!(before, s.groups.iter().map(|g| g.belief.clone()).collect::<Vec<_>>());
        // The replan sends groups to search the peak.
        replan(&mut s, &d, &mut ev);
        assert!(s.groups.iter().any(|g| matches!(g.order, GroupOrder::Search | GroupOrder::Intercept) && (g.destination == Some(id("S2")) || g.node == id("S2"))));
    }

    #[test]
    fn crippled_groups_return_and_refit() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        let g = s.groups[0].clone();
        s.groups[0].node = id("S1");
        for h in s.hulls.iter_mut().filter(|h| g.hulls.contains(&h.id)) { h.damage = 0.8; h.node = id("S1"); }
        replan(&mut s, &d, &mut ev);
        let g0 = &s.groups[0];
        assert_eq!(g0.order, GroupOrder::Return);
        assert!(g0.destination.is_some());
        let mut hours = 0;
        while s.groups[0].order != GroupOrder::Garrison && hours < 400 { s.hour += 1; step_hour(&mut s, &d, &mut ev); hours += 1; }
        assert!(hours < 400, "refit never finished");
        for h in s.hulls.iter().filter(|h| g.hulls.contains(&h.id)) { assert!(h.damage <= 0.05); }
    }

    #[test]
    fn dissolved_hulls_reform_with_supply() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        // Sink all but one hull of two groups without an ace: both dissolve, the survivors form a new group.
        let ace_hulls: Vec<u32> = s.hulls.iter().filter(|h| h.ace.is_some()).map(|h| h.id).collect();
        let plain: Vec<Vec<u32>> = s.groups.iter().filter(|g| !g.hulls.iter().any(|h| ace_hulls.contains(h))).take(2).map(|g| g.hulls.clone()).collect();
        assert_eq!(plain.len(), 2);
        for h in s.hulls.iter_mut() { for g in &plain { if g.contains(&h.id) && h.id != g[g.len() - 1] { h.alive = false; } } }
        let groups_before = s.groups.len();
        let stock = s.depots[&id("D")];
        replan(&mut s, &d, &mut ev);
        assert!(s.groups.len() <= groups_before);
        assert!((s.depots[&id("D")] - (stock - 2.0 * FORM_SUPPLY_PER_HULL)).abs() < 1e-9);
        for h in s.hulls.iter().filter(|h| h.alive) { if let Some(g) = h.group { assert!(s.groups.iter().any(|x| x.id == g && x.hulls.contains(&h.id))); } }
    }
}
