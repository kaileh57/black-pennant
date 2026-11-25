//! Unit `sys-theatre-logistics`. Combine logistics on the theatre graph. DESIGN.md L747-748, L886 (gate).
//!
//! Shipyards produce tonnage, depots store fuel and munitions, convoys carry them along lanes, forward bases
//! spend supply to launch task groups. Region Supply 0..=100 = 100 x delivered / demanded over the last 4
//! theatre days. Patrol density = (0.25 + 0.75 x Supply / 100) x the region's base patrol count; Combine
//! repair and refit speeds scale by the same factor. A successful convoy raid or a minefield cuts a lane for
//! 10 theatre days; a depot strike empties the depot, which counts as a cut on every lane it feeds for 10
//! days. A cut thins patrols within 2 to 4 theatre days.
//!
//! # Model (decisions where the spec is silent)
//! - Only regions open in the current act (all in Open Theatre) have demand, convoys and Combine group
//!   activity, so a region's hull records stay unseeded until its act opens it.
//! - Demand: every region demands [`DEMAND_T_PER_PATROL_DAY`] x its base patrol count in tonnes per theatre
//!   day, split over its receiving nodes: its depots (by capacity), else its forward bases, else its harbours,
//!   radar and ESM sites. A region with no receiving node, or no base patrols, has no demand and reads Supply 100.
//! - Sources: shipyards are the Combine's supply sources and are not capacity-limited for convoys (harbours
//!   stand in when the chart has no shipyard; a chart with neither has no logistics and every region reads
//!   Supply 100). Shipyard output (`tonnes_per_day`) accumulates as built tonnage in `TheatreState::depots`
//!   under the shipyard's node id (capped at [`SHIPYARD_STOCK_DAYS`] days); the Admiral spends it to rebuild
//!   hulls in Open Theatre. `depots` therefore holds depot stock under depot nodes and built tonnage under
//!   shipyard nodes.
//! - Convoys: once per theatre day per receiving node, at an hour staggered by the node id, a convoy sails
//!   from the receiver's home shipyard (nearest by the uncut lane graph). It takes the shortest open route;
//!   if a cut forces a detour longer than [`DETOUR_LIMIT`] x the home route, or no route exists, the sailing is
//!   cancelled. When the order of battle holds freighters or tankers, a convoy needs [`CONVOY_HULLS`] idle
//!   ones; a convoy whose hulls are all sunk is lost, and a convoy that lost some delivers in proportion.
//!   At Alert 3 and above every real sailing is shadowed by a decoy convoy (no cargo, no hulls).
//! - Accounting: a sailing's demand is booked when it arrives or is lost; a cancelled sailing books its
//!   demand at once with nothing delivered. Steady state therefore reads exactly 100, and every lost or
//!   cancelled sailing lowers Supply for the rest of the window. Buckets are per theatre day, oldest first;
//!   the last element is today and rolls at `hour % 24 == 0`.
//! - Cuts block Combine traffic only; SEXTANT ignores them (they are LANTERN's own cuts and fields) but may
//!   only use lanes whose two ends lie in regions open in the current act. When a cut expires the lane
//!   reopens and its minefield counts as swept (`mines` back to 0).
//! - `RegionState::combine_control` (set by economy when SEXTANT is sunk) is a one-shot handshake: the next
//!   `update_supply` refills that region's window to full, so economy's Supply 100 persists, and clears it.
//! - Speeds: SEXTANT 12 kn, Combine groups and convoys 16 kn, straight-line between node positions; a
//!   transit between distinct nodes takes at least 1 hour.
use super::{admiral, alert, TheatreData, TheatreEvent};
use crate::prelude::*;
use content::campaign::{ClassRole, NodeDef, NodeKind};
use content::save::{ConvoyState, RegionState, TheatreState};

/// Tonnes of supply a region demands per base patrol per theatre day.
pub const DEMAND_T_PER_PATROL_DAY: f64 = 10.0;
/// Days of shipyard output a shipyard can hold as built tonnage.
pub const SHIPYARD_STOCK_DAYS: f64 = 7.0;
/// A rerouted convoy sails only if its route is at most this many times its home route.
pub const DETOUR_LIMIT: f64 = 1.5;
/// Freighters or tankers per convoy, when the order of battle has any.
pub const CONVOY_HULLS: usize = 3;
/// SEXTANT's theatre transit speed in km/h (12 kn).
pub const SEXTANT_KMH: f64 = 12.0 * 1.852;
/// Combine group and convoy transit speed in km/h (16 kn).
pub const COMBINE_KMH: f64 = 16.0 * 1.852;

/// One hour of convoys, shipyards and depots. Unit: `sys-theatre-logistics`.
pub fn step_hour(state: &mut TheatreState, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    let net = Net::new(data);
    let window = window_days(data);
    let hour = state.hour;
    ensure_regions(state, data);
    if hour.is_multiple_of(24) {
        for r in state.regions.values_mut() { roll_day(r, window); }
    }
    produce_and_consume(state, data, &net);
    progress_convoys(state, data, &net);
    dispatch_convoys(state, data, &net, events);
}

/// Recompute Supply and patrol density for every region; reopen lanes whose cut expired.
/// Unit: `sys-theatre-logistics`.
pub fn update_supply(state: &mut TheatreState, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    let hour = state.hour;
    // Lane reopenings, in key order.
    let mut restored = Vec::new();
    for (key, lane) in state.lanes.iter_mut() {
        if let Some(until) = lane.cut_until_hour {
            if until <= hour {
                lane.cut_until_hour = None;
                lane.mines = 0;
                restored.push(key.clone());
            }
        }
    }
    state.lanes.retain(|_, l| l.cut_until_hour.is_some() || l.mines > 0);
    for (a, b) in restored { events.push(TheatreEvent::LaneRestored { a, b }); }

    ensure_regions(state, data);
    let window = window_days(data);
    let floor = data.campaign.rules.patrol_floor;
    let net = Net::new(data);
    for def in &data.campaign.theatre.regions {
        let daily = region_daily_demand(data, &net, &def.id, region_open(state, data, &def.id));
        let Some(r) = state.regions.get_mut(&def.id) else { continue };
        if r.combine_control {
            fill_window(r, daily, window);
            r.combine_control = false;
        }
        if let Some(until) = r.convoys_revealed_until_hour { if until <= hour { r.convoys_revealed_until_hour = None; } }
        trim(r, window);
        let old = r.supply;
        let new = window_supply(r);
        r.supply = new;
        if (new - old).abs() > 1e-6 {
            events.push(TheatreEvent::SupplyChanged { region: def.id.clone(), supply: new });
            let (fo, fnew) = (patrol_factor(old, floor), patrol_factor(new, floor));
            if (fnew - fo).abs() > 1e-6 { events.push(TheatreEvent::PatrolDensityChanged { region: def.id.clone(), factor: fnew }); }
        }
    }
}

/// Patrol density factor of a region now: 0.25 + 0.75 x Supply / 100 (floor from `CampaignRules`).
pub fn patrol_factor(supply: f64, floor: f64) -> f64 { floor + (1.0 - floor) * (supply / 100.0).clamp(0.0, 1.0) }

/// Cut a lane for `CampaignRules::lane_cut_days`. Unit: `sys-theatre-logistics`.
///
/// A pair that is not a lane on the chart is ignored. A lane already cut keeps the later reopening hour.
pub fn cut_lane(state: &mut TheatreState, a: &TheatreNodeId, b: &TheatreNodeId, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    if !is_lane(data, a, b) { return; }
    let until = state.hour.saturating_add(data.campaign.rules.lane_cut_days as u64 * 24);
    let key = lane_key(a, b);
    let lane = state.lanes.entry(key.clone()).or_default();
    let until = lane.cut_until_hour.map_or(until, |u| u.max(until));
    lane.cut_until_hour = Some(until);
    events.push(TheatreEvent::LaneCut { a: key.0, b: key.1, until_hour: until });
}

/// Empty a depot and cut every lane it feeds. Unit: `sys-theatre-logistics`.
///
/// "Feeds" = every lane with the depot at one end. A node that is not a depot is ignored.
pub fn strike_depot(state: &mut TheatreState, node: &TheatreNodeId, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    let net = Net::new(data);
    let Some(i) = net.idx(node) else { return };
    if !matches!(net.nodes[i].kind, NodeKind::Depot { .. }) { return; }
    state.depots.insert(node.clone(), 0.0);
    events.push(TheatreEvent::DepotEmptied { node: node.clone() });
    let neighbours: Vec<usize> = net.adj[i].iter().map(|e| e.to).collect();
    for j in neighbours {
        let other = net.nodes[j].id.clone();
        cut_lane(state, node, &other, data, events);
    }
}

/// Lay a minefield on the lane an operation targeted; the field persists and cuts it (L796).
/// Unit: `sys-theatre-logistics`.
///
/// The field stays on the lane until the cut expires, when the Combine has swept it.
pub fn lay_minefield(state: &mut TheatreState, a: &TheatreNodeId, b: &TheatreNodeId, mines: u16, data: &TheatreData, events: &mut Vec<TheatreEvent>) {
    if mines == 0 || !is_lane(data, a, b) { return; }
    let lane = state.lanes.entry(lane_key(a, b)).or_default();
    lane.mines = lane.mines.saturating_add(mines);
    cut_lane(state, a, b, data, events);
}

/// Shortest travel time in hours between two nodes along open lanes for SEXTANT (and for Combine groups when
/// `combine`), or `None` when unreachable. Unit: `sys-theatre-logistics`.
///
/// Combine traffic may not use cut lanes; SEXTANT may use only lanes whose ends are both in open regions.
pub fn transit_hours(state: &TheatreState, data: &TheatreData, from: &TheatreNodeId, to: &TheatreNodeId, combine: bool) -> Option<u32> {
    let net = Net::new(data);
    let (f, t) = (net.idx(from)?, net.idx(to)?);
    if f == t { return Some(0); }
    let km = if combine { net.dist(f, |l| !lane_cut(state, l)) } else { net.dist(f, |l| sextant_lane_open(state, data, &net, l)) };
    hours_for(km[t], if combine { COMBINE_KMH } else { SEXTANT_KMH })
}

// ---------------------------------------------------------------------------------------------------------
// Shared graph helpers for the theatre units owned alongside this one (alert, admiral, adaptation, progress).

/// One lane of the chart graph.
pub(crate) struct NetLane { pub a: usize, pub b: usize, pub km: f64, pub traffic: f64, pub key: (TheatreNodeId, TheatreNodeId) }
pub(crate) struct NetEdge { pub to: usize, pub lane: usize }

/// The theatre graph built from data: node index, adjacency and lane lengths. Cheap (48 nodes); built per call.
pub(crate) struct Net<'a> {
    pub nodes: &'a [NodeDef],
    index: BTreeMap<&'a str, usize>,
    pub lanes: Vec<NetLane>,
    pub adj: Vec<Vec<NetEdge>>,
}

impl<'a> Net<'a> {
    pub fn new(data: &'a TheatreData) -> Net<'a> {
        let nodes = data.campaign.theatre.nodes.as_slice();
        let mut index = BTreeMap::new();
        for (i, n) in nodes.iter().enumerate() { index.entry(n.id.as_str()).or_insert(i); }
        let mut lanes: Vec<NetLane> = Vec::new();
        let mut adj: Vec<Vec<NetEdge>> = (0..nodes.len()).map(|_| Vec::new()).collect();
        let mut seen = BTreeSet::new();
        for l in &data.campaign.theatre.lanes {
            let (Some(&a), Some(&b)) = (index.get(l.a.as_str()), index.get(l.b.as_str())) else { continue };
            if a == b || !seen.insert((a.min(b), a.max(b))) { continue; }
            let (pa, pb) = (nodes[a].pos_km, nodes[b].pos_km);
            let (dx, dz) = (pa[0] - pb[0], pa[1] - pb[1]);
            let km = libm::sqrt(dx * dx + dz * dz);
            let km = if km.is_finite() { km } else { 0.0 };
            let li = lanes.len();
            lanes.push(NetLane { a, b, km, traffic: if l.traffic.is_finite() { l.traffic.max(0.0) } else { 0.0 }, key: lane_key(&l.a, &l.b) });
            adj[a].push(NetEdge { to: b, lane: li });
            adj[b].push(NetEdge { to: a, lane: li });
        }
        Net { nodes, index, lanes, adj }
    }
    pub fn len(&self) -> usize { self.nodes.len() }
    pub fn idx(&self, id: &TheatreNodeId) -> Option<usize> { self.index.get(id.as_str()).copied() }
    pub fn region_of(&self, i: usize) -> &'a RegionId { &self.nodes[i].region }
    /// Straight-line distance in km between two nodes.
    pub fn direct_km(&self, a: usize, b: usize) -> f64 {
        let (pa, pb) = (self.nodes[a].pos_km, self.nodes[b].pos_km);
        let (dx, dz) = (pa[0] - pb[0], pa[1] - pb[1]);
        let d = libm::sqrt(dx * dx + dz * dz);
        if d.is_finite() { d } else { 0.0 }
    }
    /// Single-source shortest distances in km over lanes `open` admits (`f64::INFINITY` = unreachable).
    /// O(n^2) Dijkstra with ties broken by the lowest node index, so it is deterministic.
    pub fn dist(&self, src: usize, open: impl Fn(&NetLane) -> bool) -> Vec<f64> {
        let n = self.len();
        let mut d = vec![f64::INFINITY; n];
        let mut done = vec![false; n];
        if src >= n { return d; }
        d[src] = 0.0;
        loop {
            let mut best = usize::MAX;
            for i in 0..n { if !done[i] && d[i].is_finite() && (best == usize::MAX || d[i] < d[best]) { best = i; } }
            if best == usize::MAX { break; }
            done[best] = true;
            for e in &self.adj[best] {
                let lane = &self.lanes[e.lane];
                if !open(lane) { continue; }
                let nd = d[best] + lane.km;
                if nd < d[e.to] { d[e.to] = nd; }
            }
        }
        d
    }
    /// Combine transit hours from `src` to every node over uncut lanes.
    pub fn combine_hours_from(&self, state: &TheatreState, src: usize) -> Vec<Option<u32>> {
        self.dist(src, |l| !lane_cut(state, l)).into_iter().map(|km| hours_for(km, COMBINE_KMH)).collect()
    }
    /// True for nodes the Combine uses as bases: forward bases, harbours, shipyards, depots.
    pub fn is_base(&self, i: usize) -> bool { matches!(self.nodes[i].kind, NodeKind::ForwardBase | NodeKind::Harbour | NodeKind::Shipyard { .. } | NodeKind::Depot { .. }) }
}

/// Hours to cover `km` at `kmh`, at least 1 for a non-zero leg; `None` when unreachable.
pub(crate) fn hours_for(km: f64, kmh: f64) -> Option<u32> {
    if !km.is_finite() { return None; }
    if km <= 0.0 { return Some(0); }
    let h = libm::ceil(km / kmh);
    Some((h as u32).max(1))
}

/// Lane-map key with the smaller id first.
pub(crate) fn lane_key(a: &TheatreNodeId, b: &TheatreNodeId) -> (TheatreNodeId, TheatreNodeId) {
    if a <= b { (a.clone(), b.clone()) } else { (b.clone(), a.clone()) }
}

pub(crate) fn lane_cut(state: &TheatreState, lane: &NetLane) -> bool {
    state.lanes.get(&lane.key).and_then(|l| l.cut_until_hour).is_some_and(|u| u > state.hour)
}

fn is_lane(data: &TheatreData, a: &TheatreNodeId, b: &TheatreNodeId) -> bool {
    a != b && data.campaign.theatre.lanes.iter().any(|l| (&l.a == a && &l.b == b) || (&l.a == b && &l.b == a))
}

/// A region is open to SEXTANT when its act has come (all regions in Open Theatre). Unknown regions are open.
pub(crate) fn region_open(state: &TheatreState, data: &TheatreData, region: &RegionId) -> bool {
    state.open_theatre || data.campaign.theatre.regions.iter().find(|r| &r.id == region).is_none_or(|r| r.act <= state.act)
}

fn sextant_lane_open(state: &TheatreState, data: &TheatreData, net: &Net, lane: &NetLane) -> bool {
    region_open(state, data, net.region_of(lane.a)) && region_open(state, data, net.region_of(lane.b))
}

/// Class role of a hull class, if the class is known.
pub(crate) fn class_role(data: &TheatreData, class: &ClassId) -> Option<ClassRole> { data.campaign.classes.get(class).map(|c| c.role) }

/// Freighters and tankers sail in convoys; they never join task groups.
pub(crate) fn is_convoy_role(role: Option<ClassRole>) -> bool { matches!(role, Some(ClassRole::Freighter | ClassRole::Tanker)) }

/// Roles that never fight in task groups.
pub(crate) fn is_noncombat_role(role: Option<ClassRole>) -> bool { matches!(role, Some(ClassRole::Freighter | ClassRole::Tanker | ClassRole::Civilian | ClassRole::Tender)) }

/// FNV-1a over a string, for stable per-id staggering.
pub(crate) fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() { h ^= b as u64; h = h.wrapping_mul(0x0000_0100_0000_01b3); }
    h
}

/// Total depot stock in a region and the depots holding it.
pub(crate) fn region_depots(data: &TheatreData, region: &RegionId) -> Vec<(TheatreNodeId, f64)> {
    data.campaign.theatre.nodes.iter().filter(|n| &n.region == region).filter_map(|n| match n.kind { NodeKind::Depot { capacity } => Some((n.id.clone(), capacity.max(0.0))), _ => None }).collect()
}

/// Draw `amount` of supply from a region's depots (proportionally to stock). Returns false and changes
/// nothing when the region has depots but not enough stock; a region without depots pays nothing.
pub(crate) fn spend_region_supply(state: &mut TheatreState, data: &TheatreData, region: &RegionId, amount: f64) -> bool {
    let depots = region_depots(data, region);
    if depots.is_empty() || amount <= 0.0 { return true; }
    let total: f64 = depots.iter().map(|(id, _)| state.depots.get(id).copied().unwrap_or(0.0).max(0.0)).sum();
    if total + 1e-9 < amount { return false; }
    for (id, _) in &depots {
        if let Some(s) = state.depots.get_mut(id) {
            let share = if total > 0.0 { *s / total * amount } else { 0.0 };
            *s = (*s - share).max(0.0);
        }
    }
    true
}

/// Fill missing depot entries at capacity and give every region with no history a full supply window.
/// Idempotent; called at act start. With `refill_all`, every window is refilled (retreat start).
pub(crate) fn init_logistics(state: &mut TheatreState, data: &TheatreData, refill_all: bool) {
    for n in &data.campaign.theatre.nodes {
        if let NodeKind::Depot { capacity } = n.kind { state.depots.entry(n.id.clone()).or_insert(capacity.max(0.0)); }
    }
    ensure_regions(state, data);
    let window = window_days(data);
    let net = Net::new(data);
    for def in &data.campaign.theatre.regions {
        let daily = region_daily_demand(data, &net, &def.id, region_open(state, data, &def.id));
        let Some(r) = state.regions.get_mut(&def.id) else { continue };
        // A region with no demand on record (new, or closed until now) starts from a full window.
        if refill_all || r.demanded.iter().filter(|v| v.is_finite()).sum::<f64>() <= 1e-9 {
            fill_window(r, daily, window);
            r.supply = window_supply(r);
        }
    }
}

// ---------------------------------------------------------------------------------------------------------

fn window_days(data: &TheatreData) -> usize { (data.campaign.rules.supply_window_days as usize).clamp(1, 60) }

fn ensure_regions(state: &mut TheatreState, data: &TheatreData) {
    for def in &data.campaign.theatre.regions {
        state.regions.entry(def.id.clone()).or_insert_with(|| RegionState { supply: 100.0, ..Default::default() });
    }
}

fn trim(r: &mut RegionState, window: usize) {
    let n = r.delivered.len().max(r.demanded.len());
    r.delivered.resize(n, 0.0);
    r.demanded.resize(n, 0.0);
    if n > window {
        r.delivered.drain(..n - window);
        r.demanded.drain(..n - window);
    }
}

fn roll_day(r: &mut RegionState, window: usize) {
    trim(r, window);
    r.delivered.push(0.0);
    r.demanded.push(0.0);
    trim(r, window);
}

fn fill_window(r: &mut RegionState, daily: f64, window: usize) {
    r.delivered = vec![daily; window];
    r.demanded = vec![daily; window];
    if let (Some(d), Some(m)) = (r.delivered.last_mut(), r.demanded.last_mut()) { *d = 0.0; *m = 0.0; }
}

/// Book demand and delivery into today's bucket.
fn book(r: &mut RegionState, demanded: f64, delivered: f64) {
    if r.demanded.is_empty() || r.delivered.len() != r.demanded.len() {
        let n = r.delivered.len().max(r.demanded.len()).max(1);
        r.delivered.resize(n, 0.0);
        r.demanded.resize(n, 0.0);
    }
    if let Some(m) = r.demanded.last_mut() { *m += demanded.max(0.0); }
    if let Some(d) = r.delivered.last_mut() { *d += delivered.max(0.0); }
}

fn window_supply(r: &RegionState) -> f64 {
    let dem: f64 = r.demanded.iter().filter(|v| v.is_finite()).sum();
    let del: f64 = r.delivered.iter().filter(|v| v.is_finite()).sum();
    if dem <= 1e-9 { 100.0 } else { (100.0 * del / dem).clamp(0.0, 100.0) }
}

/// Receiving nodes of a region with their share of its demand (sums to 1).
fn receivers(net: &Net, region: &RegionId) -> Vec<(usize, f64)> {
    let in_region: Vec<usize> = (0..net.len()).filter(|&i| net.region_of(i) == region).collect();
    let depots: Vec<(usize, f64)> = in_region.iter().filter_map(|&i| match net.nodes[i].kind { NodeKind::Depot { capacity } => Some((i, capacity.max(0.0))), _ => None }).collect();
    let total: f64 = depots.iter().map(|d| d.1).sum();
    if !depots.is_empty() {
        if total > 0.0 { return depots.into_iter().map(|(i, c)| (i, c / total)).collect(); }
        let k = depots.len() as f64;
        return depots.into_iter().map(|(i, _)| (i, 1.0 / k)).collect();
    }
    let pick = |f: &dyn Fn(&NodeKind) -> bool| -> Vec<usize> { in_region.iter().copied().filter(|&i| f(&net.nodes[i].kind)).collect() };
    let mut set = pick(&|k| matches!(k, NodeKind::ForwardBase));
    if set.is_empty() { set = pick(&|k| matches!(k, NodeKind::Harbour | NodeKind::RadarSite | NodeKind::EsmSite)); }
    let k = set.len() as f64;
    set.into_iter().map(|i| (i, 1.0 / k)).collect()
}

/// Supply source nodes: shipyards, else harbours.
fn sources(net: &Net) -> Vec<usize> {
    let yards: Vec<usize> = (0..net.len()).filter(|&i| matches!(net.nodes[i].kind, NodeKind::Shipyard { .. })).collect();
    if !yards.is_empty() { return yards; }
    (0..net.len()).filter(|&i| matches!(net.nodes[i].kind, NodeKind::Harbour)).collect()
}

/// Daily demand of a region; regions not yet open to the campaign demand nothing (the Combine's logistics
/// there are off the chart until the act opens them).
fn region_daily_demand(data: &TheatreData, net: &Net, region: &RegionId, open: bool) -> f64 {
    if !open || sources(net).is_empty() || receivers(net, region).is_empty() { return 0.0; }
    data.campaign.theatre.regions.iter().find(|r| &r.id == region).map_or(0.0, |r| r.base_patrols as f64 * DEMAND_T_PER_PATROL_DAY)
}

fn produce_and_consume(state: &mut TheatreState, data: &TheatreData, net: &Net) {
    for n in net.nodes {
        if let NodeKind::Shipyard { tonnes_per_day } = n.kind {
            let rate = if tonnes_per_day.is_finite() { tonnes_per_day.max(0.0) } else { 0.0 };
            let s = state.depots.entry(n.id.clone()).or_insert(0.0);
            *s = (*s + rate / 24.0).min(rate * SHIPYARD_STOCK_DAYS);
        }
    }
    // Each region burns its daily demand from its depots, hour by hour.
    for def in &data.campaign.theatre.regions {
        let burn = region_daily_demand(data, net, &def.id, region_open(state, data, &def.id)) / 24.0;
        if burn <= 0.0 { continue; }
        let depots = region_depots(data, &def.id);
        let total_cap: f64 = depots.iter().map(|d| d.1).sum();
        for (id, cap) in depots {
            let share = if total_cap > 0.0 { cap / total_cap } else { 0.0 };
            if let Some(s) = state.depots.get_mut(&id) { *s = (*s - burn * share).max(0.0); }
        }
    }
}

fn hull_alive(state: &TheatreState, id: u32) -> bool { state.hulls.iter().any(|h| h.id == id && h.alive) }

fn progress_convoys(state: &mut TheatreState, data: &TheatreData, net: &Net) {
    let hour = state.hour;
    let convoys = core::mem::take(&mut state.convoys);
    let mut keep = Vec::with_capacity(convoys.len());
    for c in convoys {
        let total = c.hulls.len();
        let alive = c.hulls.iter().filter(|&&h| hull_alive(state, h)).count();
        let lost = total > 0 && alive == 0;
        let arrived = c.arrive_hour <= hour;
        if !lost && !arrived { keep.push(c); continue; }
        let dest_region = net.idx(&c.to).map(|i| net.region_of(i).clone());
        if c.decoy {
            continue;
        }
        let frac = if total == 0 { 1.0 } else { alive as f64 / total as f64 };
        let delivered = if lost { 0.0 } else { c.cargo * frac };
        if let Some(region) = &dest_region {
            if let Some(r) = state.regions.get_mut(region) { book(r, c.cargo, delivered); }
        }
        if !lost {
            if let Some(i) = net.idx(&c.to) {
                if let NodeKind::Depot { capacity } = net.nodes[i].kind {
                    let s = state.depots.entry(c.to.clone()).or_insert(0.0);
                    *s = (*s + delivered).min(capacity.max(0.0));
                }
            }
            // Surviving freighters lie at the destination until they are picked for another sailing.
            for h in state.hulls.iter_mut().filter(|h| h.alive && c.hulls.contains(&h.id)) {
                h.node = c.to.clone();
                if let Some(r) = &dest_region { h.region = r.clone(); }
            }
        }
        if frac < 1.0 {
            admiral::note_missing_convoy(state, data, &c.from, &c.to, 1.0 - frac);
        }
    }
    state.convoys = keep;
}

fn dispatch_convoys(state: &mut TheatreState, data: &TheatreData, net: &Net, events: &mut Vec<TheatreEvent>) {
    let srcs = sources(net);
    if srcs.is_empty() { return; }
    let hour = state.hour;
    let escalation = data.campaign.rules.alert_escalation;
    // Distances from every source: home routes over the whole graph, and current routes avoiding cuts.
    let home: Vec<Vec<f64>> = srcs.iter().map(|&s| net.dist(s, |_| true)).collect();
    let open: Vec<Vec<f64>> = srcs.iter().map(|&s| net.dist(s, |l| !lane_cut(state, l))).collect();
    let needs_hulls = state.hulls.iter().any(|h| is_convoy_role(class_role(data, &h.class)));
    for def in &data.campaign.theatre.regions {
        let daily = region_daily_demand(data, net, &def.id, region_open(state, data, &def.id));
        if daily <= 0.0 { continue; }
        for (dest, share) in receivers(net, &def.id) {
            let dest_id = &net.nodes[dest].id;
            if fnv(dest_id.as_str()) % 24 != hour % 24 { continue; }
            let requirement = daily * share;
            // Home shipyard: nearest source over the uncut graph, ties by source order.
            let mut best: Option<usize> = None;
            for (k, d) in home.iter().enumerate() {
                if d[dest].is_finite() && best.is_none_or(|b| d[dest] < home[b][dest]) { best = Some(k); }
            }
            let Some(k) = best else {
                if let Some(r) = state.regions.get_mut(&def.id) { book(r, requirement, 0.0); }
                continue;
            };
            let (home_km, open_km) = (home[k][dest], open[k][dest]);
            let src = srcs[k];
            if src == dest {
                if let Some(r) = state.regions.get_mut(&def.id) { book(r, requirement, requirement); }
                continue;
            }
            if !open_km.is_finite() || open_km > home_km * DETOUR_LIMIT + 1e-9 {
                if let Some(r) = state.regions.get_mut(&def.id) { book(r, requirement, 0.0); }
                continue;
            }
            let hulls = if needs_hulls {
                let picked = pick_freighters(state, data, net, src);
                if picked.is_empty() {
                    if let Some(r) = state.regions.get_mut(&def.id) { book(r, requirement, 0.0); }
                    continue;
                }
                picked
            } else { Vec::new() };
            let hours = hours_for(open_km, COMBINE_KMH).unwrap_or(1).max(1) as u64;
            let src_region = net.region_of(src).clone();
            let revealed = [&def.id, &src_region].iter().any(|r| state.regions.get(*r).and_then(|s| s.convoys_revealed_until_hour).is_some_and(|u| u > hour));
            let decoy = alert::alert_floor(state.regions.get(&def.id).map_or(0.0, |r| r.alert)) >= escalation;
            let mut spawn = |state: &mut TheatreState, hulls: Vec<u32>, cargo: f64, is_decoy: bool| {
                let id = state.next_id;
                state.next_id = state.next_id.wrapping_add(1);
                state.convoys.push(ConvoyState { id, hulls, from: net.nodes[src].id.clone(), to: dest_id.clone(), depart_hour: hour, arrive_hour: hour + hours, cargo, decoy: is_decoy, revealed });
                if revealed { events.push(TheatreEvent::ConvoyRevealed { convoy: id }); }
            };
            spawn(state, hulls, requirement, false);
            if decoy { spawn(state, Vec::new(), 0.0, true); }
        }
    }
}

/// Up to [`CONVOY_HULLS`] idle freighters or tankers: at the source first, then in its region, then anywhere.
fn pick_freighters(state: &TheatreState, data: &TheatreData, net: &Net, src: usize) -> Vec<u32> {
    let busy: BTreeSet<u32> = state.convoys.iter().flat_map(|c| c.hulls.iter().copied()).collect();
    let src_id = &net.nodes[src].id;
    let src_region = net.region_of(src);
    let mut idle: Vec<(u8, u32)> = state.hulls.iter()
        .filter(|h| h.alive && h.group.is_none() && h.refit_until_hour.is_none() && !busy.contains(&h.id) && is_convoy_role(class_role(data, &h.class)))
        .map(|h| (if &h.node == src_id { 0 } else if &h.region == src_region { 1 } else { 2 }, h.id))
        .collect();
    idle.sort();
    idle.into_iter().take(CONVOY_HULLS).map(|(_, id)| id).collect()
}

#[cfg(test)]
pub(crate) mod testkit {
    //! Hand-built campaign data and state shared by the theatre unit tests of this group.
    use super::*;
    use content::campaign::*;
    use content::save::*;

    pub fn node(id: &str, region: &str, x: f64, z: f64, kind: NodeKind) -> NodeDef {
        NodeDef { id: TheatreNodeId::new(id), name: TextKey::new(id), region: RegionId::new(region), pos_km: [x, z], kind, map: MapDef::default() }
    }
    pub fn lane(a: &str, b: &str) -> LaneDef { LaneDef { a: TheatreNodeId::new(a), b: TheatreNodeId::new(b), traffic: 1.0 } }
    pub fn id(s: &str) -> TheatreNodeId { TheatreNodeId::new(s) }
    pub fn rid(s: &str) -> RegionId { RegionId::new(s) }

    fn class(id: &str, role: ClassRole, tier: Tier, act: Act) -> EnemyClassDef {
        EnemyClassDef { id: ClassId::new(id), name: TextKey::new(id), blueprint: String::new(), role, tier, act, behaviour: String::new(), engagement_range_m: None, variants: vec![], surrender: SurrenderRule::Never, air_wing: vec![], submersible: false, capital: false }
    }

    fn op(id: &str, kind: OperationKind, act: Act) -> OperationDef {
        OperationDef { id: OperationId::new(id), kind, act, name: TextKey::new(id), briefing: TextKey::new(id), unlock: Unlock::Always, node: None, map: None, objective: Objective::DestroyAll, tags: vec![], ghosts: vec![], force: vec![], per_extra_player: None, complications: vec![], approach_m: 10_000.0, sea_states: vec![], actions: vec![], declared_hostile: vec![], recommended_emcon: None, pushback_days: None, forced: false, reference_setup: None }
    }

    /// Two regions. Region 1: shipyard Y feeds depot D through sea nodes S1 (busy, short) or S2 (long
    /// detour); cove C1 hangs off S1. Region 2 (Act 2): forward base F and cove C2.
    pub fn data() -> TheatreData {
        let mut c = CampaignData::default();
        c.theatre.regions = vec![
            RegionDef { id: rid("r1"), name: TextKey::new("r1"), act: Act::Act1, base_patrols: 10, layer_depth_m: 50.0, weather: vec![], order_of_battle: vec![(ClassId::new("pb"), 20), (ClassId::new("frig"), 5), (ClassId::new("frt"), 6)], civilian: vec![] },
            RegionDef { id: rid("r2"), name: TextKey::new("r2"), act: Act::Act2, base_patrols: 5, layer_depth_m: 50.0, weather: vec![], order_of_battle: vec![(ClassId::new("pb"), 10)], civilian: vec![] },
        ];
        c.theatre.nodes = vec![
            node("Y", "r1", 0.0, 0.0, NodeKind::Shipyard { tonnes_per_day: 200.0 }),
            node("S1", "r1", 50.0, 0.0, NodeKind::Sea),
            node("S2", "r1", 50.0, 60.0, NodeKind::Sea),
            node("D", "r1", 100.0, 0.0, NodeKind::Depot { capacity: 400.0 }),
            node("C1", "r1", 50.0, -30.0, NodeKind::Cove),
            node("F", "r2", 200.0, 0.0, NodeKind::ForwardBase),
            node("C2", "r2", 200.0, 40.0, NodeKind::Cove),
        ];
        c.theatre.lanes = vec![lane("Y", "S1"), lane("S1", "D"), lane("Y", "S2"), lane("S2", "D"), lane("S1", "C1"), lane("D", "F"), lane("F", "C2")];
        c.theatre.sun.elevation_deg = [30.0; 24];
        c.classes.insert(ClassId::new("pb"), class("pb", ClassRole::PatrolBoat, Tier::T1, Act::Act1));
        c.classes.insert(ClassId::new("frig"), class("frig", ClassRole::Frigate, Tier::T3, Act::Act1));
        c.classes.insert(ClassId::new("frt"), class("frt", ClassRole::Freighter, Tier::T1, Act::Act1));
        c.classes.insert(ClassId::new("ace_ship"), class("ace_ship", ClassRole::Destroyer, Tier::T3, Act::Act1));
        c.aces.insert(AceId::new("ace.one"), AceDef { id: AceId::new("ace.one"), name: TextKey::new("a"), callsign: TextKey::new("a"), ship: ClassId::new("ace_ship"), doctrine: vec![], arcs: vec![] });
        for (k, o) in [
            ("op.raid", op("op.raid", OperationKind::Template(MissionType::ConvoyRaid), Act::Act1)),
            ("op.depot", op("op.depot", OperationKind::Template(MissionType::DepotStrike), Act::Act1)),
            ("op.hunt", op("op.hunt", OperationKind::Template(MissionType::HunterGroupSurvival), Act::Act1)),
            ("op.feint", op("op.feint", OperationKind::Template(MissionType::Feint), Act::Act1)),
            ("op.s1", op("op.s1", OperationKind::Story, Act::Act1)),
            ("op.s2", op("op.s2", OperationKind::Story, Act::Act1)),
            ("op.duel", op("op.duel", OperationKind::AceDuel(AceId::new("ace.one")), Act::Act1)),
            ("op.cap1", op("op.cap1", OperationKind::Capstone, Act::Act1)),
            ("op.cap2", op("op.cap2", OperationKind::Capstone, Act::Act2)),
        ] { c.operations.insert(OperationId::new(k), o); }
        c.acts = vec![
            ActDef { act: Act::Act1, phase: Phase::R010, name: TextKey::new("a1"), starters: vec![], start_tier: Tier::T1, tier_unlocks: vec![(OperationId::new("op.s1"), Tier::T2), (OperationId::new("op.s2"), Tier::T3)], regions: vec![rid("r1")], story_ops: vec![OperationId::new("op.s1"), OperationId::new("op.s2")], ace_duels: vec![OperationId::new("op.duel")], capstone: OperationId::new("op.cap1"), aces: vec![AceId::new("ace.one")], coves: vec![id("C1"), id("C2")], retreat_closed_cove: Some(id("C2")), opens_open_theatre: false },
            ActDef { act: Act::Act2, phase: Phase::Act2, name: TextKey::new("a2"), starters: vec![], start_tier: Tier::T4, tier_unlocks: vec![], regions: vec![rid("r1"), rid("r2")], story_ops: vec![], ace_duels: vec![], capstone: OperationId::new("op.cap2"), aces: vec![], coves: vec![id("C1"), id("C2")], retreat_closed_cove: None, opens_open_theatre: true },
        ];
        c.cards.insert(CardId::new("card.pd"), DoctrineCardDef { id: CardId::new("card.pd"), name: TextKey::new("pd"), description: TextKey::new("pd"), counters: TallyKey::Method(KillMethod::Missile), effect: CardEffect::default() });
        c.cards.insert(CardId::new("card.pickets"), DoctrineCardDef { id: CardId::new("card.pickets"), name: TextKey::new("pk"), description: TextKey::new("pk"), counters: TallyKey::Time(TimeOfDay::Night), effect: CardEffect::default() });
        c.cards.insert(CardId::new("card.pd2"), DoctrineCardDef { id: CardId::new("card.pd2"), name: TextKey::new("pd2"), description: TextKey::new("pd2"), counters: TallyKey::Method(KillMethod::Missile), effect: CardEffect::default() });
        c.cards.insert(CardId::new("card.close"), DoctrineCardDef { id: CardId::new("card.close"), name: TextKey::new("cl"), description: TextKey::new("cl"), counters: TallyKey::Range(RangeBand::Close), effect: CardEffect::default() });
        TheatreData { campaign: Arc::new(c), catalogue: Arc::new(Catalogue::default()) }
    }

    pub fn state() -> TheatreState {
        TheatreState {
            seed: 7, act: Act::Act1, hour: 0, clock_hours_left: Some(960.0), retreat: false, open_theatre: false,
            difficulty: DifficultySliders::NORMAL, resources: Resources::default(), players: vec![],
            sextant_node: id("C1"), sextant_refit_until_hour: None, open_coves: vec![id("C1")],
            regions: BTreeMap::new(), lanes: BTreeMap::new(), depots: BTreeMap::new(), hulls: vec![], groups: vec![], convoys: vec![],
            tallies: BTreeMap::new(), ops: OpsLedger::default(), offered: vec![], hunter_group: None, last_admiral_plan_hour: 0, last_host_rung: 0, next_id: 1,
        }
    }

    /// A freshly started Act 1 campaign.
    pub fn started() -> (TheatreState, TheatreData) {
        let d = data();
        let mut s = state();
        let mut ev = Vec::new();
        crate::theatre::progress::start_act(&mut s, Act::Act1, false, &d, &mut ev);
        (s, d)
    }

    /// Run the hour order of the contract for this group's units.
    pub fn hour(s: &mut TheatreState, d: &TheatreData, ev: &mut Vec<TheatreEvent>) {
        s.hour += 1;
        super::step_hour(s, d, ev);
        super::update_supply(s, d, ev);
        crate::theatre::alert::step_hour(s, d, ev);
        crate::theatre::adaptation::step_hour(s, d, ev);
        crate::theatre::admiral::step_hour(s, d, ev);
        crate::theatre::progress::step_hour(s, d, ev);
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;

    #[test]
    fn patrol_factor_formula() {
        assert!((patrol_factor(100.0, 0.25) - 1.0).abs() < 1e-12);
        assert!((patrol_factor(0.0, 0.25) - 0.25).abs() < 1e-12);
        assert!((patrol_factor(40.0, 0.25) - 0.55).abs() < 1e-12);
        assert!((patrol_factor(250.0, 0.25) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn transit_and_cuts() {
        let (mut s, d) = started();
        // Y-S1-D = 100 km at 16 kn = 3.37 h -> 4 h.
        assert_eq!(transit_hours(&s, &d, &id("Y"), &id("D"), true), Some(4));
        assert_eq!(transit_hours(&s, &d, &id("Y"), &id("Y"), true), Some(0));
        assert_eq!(transit_hours(&s, &d, &id("Y"), &id("nope"), true), None);
        let mut ev = Vec::new();
        cut_lane(&mut s, &id("S1"), &id("D"), &d, &mut ev);
        assert!(matches!(ev[0], TheatreEvent::LaneCut { until_hour: 240, .. }));
        // Detour via S2: 2 x 78.1 km = 156.2 km -> 6 h.
        assert_eq!(transit_hours(&s, &d, &id("Y"), &id("D"), true), Some(6));
        // SEXTANT ignores cuts but not closed regions (r2 opens in Act 2).
        assert_eq!(transit_hours(&s, &d, &id("Y"), &id("D"), false), Some(5));
        assert_eq!(transit_hours(&s, &d, &id("C1"), &id("C2"), false), None);
        assert!(transit_hours(&s, &d, &id("C1"), &id("C2"), true).is_some());
        // Not a lane: ignored.
        let n = ev.len();
        cut_lane(&mut s, &id("Y"), &id("D"), &d, &mut ev);
        assert_eq!(ev.len(), n);
        // Reopens after 10 days with the field swept.
        lay_minefield(&mut s, &id("S1"), &id("D"), 8, &d, &mut ev);
        assert_eq!(s.lanes[&lane_key(&id("S1"), &id("D"))].mines, 8);
        s.hour = 240;
        update_supply(&mut s, &d, &mut ev);
        assert!(s.lanes.is_empty());
        assert!(ev.iter().any(|e| matches!(e, TheatreEvent::LaneRestored { .. })));
    }

    #[test]
    fn steady_state_supply_is_full() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        for _ in 0..24 * 6 { hour(&mut s, &d, &mut ev); }
        assert!((s.regions[&rid("r1")].supply - 100.0).abs() < 1e-9, "supply {}", s.regions[&rid("r1")].supply);
        assert!(s.depots[&id("D")] > 0.0);
    }

    /// Gate shape (L886): cutting the busiest lane lowers patrol density by at least 15% within 4 days.
    #[test]
    fn busiest_lane_cut_thins_patrols() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        for _ in 0..48 { hour(&mut s, &d, &mut ev); }
        let floor = d.campaign.rules.patrol_floor;
        let before = patrol_factor(s.regions[&rid("r1")].supply, floor);
        // The detour via S2 is 1.56x the home route, past the detour limit, so the depot is cut off.
        cut_lane(&mut s, &id("Y"), &id("S1"), &d, &mut ev);
        let mut lowest = before;
        for _ in 0..96 { hour(&mut s, &d, &mut ev); lowest = lowest.min(patrol_factor(s.regions[&rid("r1")].supply, floor)); }
        assert!(lowest <= before * 0.85, "before {before} lowest {lowest}");
    }

    #[test]
    fn depot_strike_empties_and_cuts() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        strike_depot(&mut s, &id("D"), &d, &mut ev);
        assert_eq!(s.depots[&id("D")], 0.0);
        assert_eq!(ev.iter().filter(|e| matches!(e, TheatreEvent::LaneCut { .. })).count(), 3);
        // Not a depot: nothing.
        let n = ev.len();
        strike_depot(&mut s, &id("S1"), &d, &mut ev);
        strike_depot(&mut s, &id("unknown"), &d, &mut ev);
        assert_eq!(ev.len(), n);
    }

    #[test]
    fn sunk_convoy_lowers_supply() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        let mut guard = 0;
        while s.convoys.iter().all(|c| c.decoy) && guard < 48 { hour(&mut s, &d, &mut ev); guard += 1; }
        let c = s.convoys.iter().find(|c| !c.decoy).cloned().expect("a convoy sailed");
        assert_eq!(c.hulls.len(), CONVOY_HULLS);
        for h in s.hulls.iter_mut().filter(|h| c.hulls.contains(&h.id)) { h.alive = false; }
        hour(&mut s, &d, &mut ev);
        assert!(s.convoys.iter().all(|x| x.id != c.id));
        assert!(s.regions[&rid("r1")].supply < 100.0);
    }

    #[test]
    fn combine_control_refills_window() {
        let (mut s, d) = started();
        let mut ev = Vec::new();
        {
            let r = s.regions.get_mut(&rid("r1")).unwrap();
            r.delivered = vec![0.0; 4];
            r.demanded = vec![100.0; 4];
            r.combine_control = true;
            r.supply = 100.0;
        }
        update_supply(&mut s, &d, &mut ev);
        let r = &s.regions[&rid("r1")];
        assert_eq!(r.supply, 100.0);
        assert!(!r.combine_control);
    }
}
