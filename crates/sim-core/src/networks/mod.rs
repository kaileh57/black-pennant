//! Unit `sim-core-networks`. Physical state of the three ship-wide networks: the deterministic conduit router,
//! per-tick supply solving, conduit damage and the per-part fallback chain.
//! DESIGN.md L281-299 (networks, router, priorities), L392-396 (when things break), L205 (auto-set routes).
//!
//! Router: lays conduits between compartments along the compartment adjacency graph, two paths where the graph
//! allows, avoiding magazines and the waterline, ties broken by cell index; the same design always routes the
//! same way. Pinned routes survive; a pinned route broken by a design change is reported.
//! Per-part fallback order: redundant path (within one tick), local buffer (battery 30 s, day tank 60 s),
//! then local control. A part that loses only its Data link skips the buffer step.
//!
//! Implementation notes:
//! - Every part joins the network of the compartment it sits in (`attach`); an exterior part (deck gun, mast)
//!   joins the compartment below it, or the nearest one. A network's terminals are the compartments its parts
//!   join. The router grows a tree from the root terminal (the lowest-slot generator or battery for Power, fuel
//!   tank for Fuel, console for Data), each time connecting the terminal cheapest to reach from the tree by
//!   Dijkstra over the compartment graph. Each tree link is one primary conduit plus, where the graph allows, a
//!   redundant conduit (`from` = tree side, `to` = new terminal).
//! - Conduits are fixed to structure: inside each compartment a conduit runs over the watertight cells of the
//!   compartment's shell (deckhead first), and it passes bulkheads and decks at the highest wall cell of the
//!   crossing. The redundant conduit avoids the primary's intermediate compartments and every primary cell,
//!   and keeps clear of the cells next to it where it can, so one destroyed cell never cuts both.
//! - Costs: a step on the shell costs 1, plus 6 below the estimated design waterline (from dry mass and the
//!   hull envelope) plus 20 next to the primary. Passing through a magazine compartment costs 10,000 and a
//!   compartment or crossing below the waterline 200, so both are used only when nothing else connects.
//! - Supply: Fuel solves first (tanks feed engines and generators pooled per zone, shared pro rata on
//!   shortage), then Power (generation at `output_factor` times the fuel fraction, batteries cover the rest,
//!   consumers served in ascending priority then slot, partial service to the first one that does not fit and
//!   everything after it shed, surplus charges batteries), then Data (a working, powered console).
//! - A compartment reached only through a jury-rigged conduit gets at most `jury_rig_capacity` (50%) of its
//!   demand; the consumer's supplied fraction is capped there.
//! - Buffers start full on the first solve and refill one second per second while the part is served.
//!   Engines and generators burn at least 10% of their rated fuel flow while they work (idle).
use crate::compartments::{classify, containment, wall_crossings, Crossing, Dims, BARRIER, MAX_COMPARTMENTS};
use crate::compartments::CompartmentSet;
use crate::grid::VesselGrid;
use crate::parts::{PartRuntime, PartState};
use content::catalogue::Catalogue;
use content::cell::{Face, CELL_M};
use content::enums::{DamageLevel, NetworkKind};
use content::ids::{CellIndex, CompartmentId, PartSlot};
use content::network::NetworkEdits;
use content::part::{EngineKind, PartDef, PartFunction, PortRole};
use content::tuning::NetworkTuning;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConduitState { #[default] Intact, Cut, /// 50% capacity until repaired at the tender (L396).
    JuryRigged }

/// One routed conduit between a part and its network trunk or between two compartments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Conduit {
    pub network: NetworkKind,
    pub from: CompartmentId,
    pub to: CompartmentId,
    /// Cells the conduit runs through, in order; destroying any of them cuts it.
    pub cells: Vec<CellIndex>,
    /// True for the second (redundant) path of a pair.
    pub redundant: bool,
    pub pinned: bool,
    pub state: ConduitState,
}

/// Where a part currently gets a network's service from (L394).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LinkState { #[default] Network, Redundant, Buffer, LocalControl, Dead }

/// Per-part connection state for the three networks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PartLinks {
    pub power: LinkState,
    pub fuel: LinkState,
    pub data: LinkState,
    /// Seconds of local battery and day tank left.
    pub battery_s: f64,
    pub day_tank_s: f64,
    /// Fraction of rated power and fuel supplied this tick, 0.0..=1.0.
    pub power_supplied: f64,
    pub fuel_supplied: f64,
    /// True while shed by priority (L287).
    pub shed: bool,
    /// Private mechanics of the parts unit: ammunition feed wiring inside the part's kit (magazines and
    /// hoists, L207, L283), the mount firing clock and hand-set arcs. Set by `parts::build_parts`; the default
    /// is unwired. Feed wiring is read through [`crate::parts::PartRuntime::feed`]. Public only so that
    /// `PartLinks { .., ..Default::default() }` builds outside this crate; its contents are private.
    #[doc(hidden)]
    #[serde(default)]
    pub mech: crate::parts::PartMech,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Networks {
    /// Ascending (network, from, to, redundant).
    pub conduits: Vec<Conduit>,
    /// Zone id per compartment per network after breakers and cuts (connected components), ascending.
    pub zones: Vec<[u16; 3]>,
    /// Totals this tick, per zone-agnostic summary for the Power gauge and Bridge Board.
    pub generation_kw: f64,
    pub demand_kw: f64,
    pub supplied_kw: f64,
    pub fuel_flow_lps: f64,
    pub fuel_l: f64,
    /// Pinned routes the router could not honour (each becomes a Ship's Note, L299).
    pub broken_pins: Vec<u16>,
    /// Compartment each part joins, by position in the part list the router saw; `u16::MAX` = none.
    #[serde(default)]
    pub(crate) attach: Vec<u16>,
    /// Open breakers as (network index, cell): conduits through them do not conduct.
    #[serde(default)]
    pub(crate) open_breakers: Vec<(u8, u32)>,
    /// Set once the first solve has filled every part's local buffers.
    #[serde(default)]
    pub(crate) primed: bool,
    /// Connectivity labels reused while the conduit states are unchanged (not serialised; rebuilt on demand).
    #[serde(skip)]
    pub(crate) cache: ConnCache,
}

/// Component labels per network for (all conducting, primaries only, intact only), keyed by a fingerprint of
/// the conduit states, node count and breakers. Equality ignores it: it is derived data.
#[derive(Clone, Debug, Default)]
pub(crate) struct ConnCache { key: Option<(u64, usize)>, labels: Vec<[Vec<u16>; 3]> }
impl PartialEq for ConnCache { fn eq(&self, _: &Self) -> bool { true } }

fn fingerprint(net: &Networks) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ net.conduits.len() as u64;
    let mut mix = |v: u64| { h ^= v; h = h.wrapping_mul(0x0100_0000_01b3); };
    for c in &net.conduits {
        mix(kind_index(c.network) as u64 | (c.from.0 as u64) << 8 | (c.to.0 as u64) << 24 | (c.redundant as u64) << 40 | (match c.state { ConduitState::Intact => 0u64, ConduitState::Cut => 1, ConduitState::JuryRigged => 2 }) << 41);
        mix(c.cells.len() as u64 ^ c.cells.first().map_or(0, |x| x.0 as u64) << 20);
    }
    for &(k, cell) in &net.open_breakers { mix((k as u64) << 32 | cell as u64); }
    h
}

/// Labels of network `k` for (all, primaries, intact), from the cache when the conduits are unchanged.
fn labels(net: &mut Networks, n: usize, k: usize) -> [Vec<u16>; 3] {
    let key = (fingerprint(net), n);
    if net.cache.key != Some(key) || net.cache.labels.len() != 3 {
        let l: Vec<[Vec<u16>; 3]> = (0..3).map(|k| [
            components(net, n, k, &|_| true),
            components(net, n, k, &|c| !c.redundant),
            components(net, n, k, &|c| c.state == ConduitState::Intact),
        ]).collect();
        net.cache = ConnCache { key: Some(key), labels: l };
    }
    net.cache.labels[k].clone()
}

const NONE: u16 = u16::MAX;
const KINDS: [NetworkKind; 3] = [NetworkKind::Power, NetworkKind::Fuel, NetworkKind::Data];
fn kind_index(k: NetworkKind) -> usize { match k { NetworkKind::Power => 0, NetworkKind::Fuel => 1, NetworkKind::Data => 2 } }
/// Most conduits a vessel holds (team orders address them by u16).
const MAX_CONDUITS: usize = u16::MAX as usize;
const IDLE_FUEL: f64 = 0.1;
const MAGAZINE_COST: u64 = 10_000;
const WATERLINE_COST: u64 = 200;
const CELL_LOW_COST: u32 = 6;
const NEAR_PRIMARY_COST: u32 = 20;
const SEGMENT_EXPANSION_CAP: usize = 60_000;

// ---------------------------------------------------------------------------------------------------------
// Roles.

/// What a part does on each network.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Roles {
    pub supply_kw: f64,
    pub store_kwh: f64,
    pub store_kw: f64,
    pub draw_kw: f64,
    pub electric_motor: bool,
    pub fuel_store_l: f64,
    pub fuel_lps: f64,
    pub data: bool,
    pub console: bool,
}
impl Roles {
    pub fn on(&self, k: usize) -> bool {
        match k { 0 => self.supply_kw > 0.0 || self.store_kwh > 0.0 || self.draw_kw > 0.0, 1 => self.fuel_store_l > 0.0 || self.fuel_lps > 0.0, _ => self.data || self.console }
    }
    pub fn source(&self, k: usize) -> bool { match k { 0 => self.supply_kw > 0.0 || self.store_kwh > 0.0, 1 => self.fuel_store_l > 0.0, _ => self.console } }
    pub fn consumer(&self, k: usize) -> bool { match k { 0 => self.draw_kw > 0.0, 1 => self.fuel_lps > 0.0, _ => self.data && !self.console } }
}

fn pos(v: f64) -> f64 { if v.is_finite() && v > 0.0 { v } else { 0.0 } }

pub(crate) fn roles(def: &PartDef) -> Roles {
    let mut r = Roles::default();
    let mut has = [[false; 4]; 3];
    for p in &def.ports {
        let k = kind_index(p.network);
        let role = match p.role { PortRole::Supply => 0, PortRole::Consume => 1, PortRole::Store => 2, PortRole::Link => 3 };
        has[k][role] = true;
        match (p.network, p.role) {
            (NetworkKind::Power, PortRole::Supply) => r.supply_kw += pos(p.rate),
            (NetworkKind::Power, PortRole::Consume) => r.draw_kw += pos(p.rate),
            (NetworkKind::Power, PortRole::Store) => { r.store_kwh += pos(p.capacity); r.store_kw += pos(p.rate); }
            (NetworkKind::Fuel, PortRole::Store | PortRole::Supply) => r.fuel_store_l += pos(p.capacity),
            (NetworkKind::Fuel, PortRole::Consume) => r.fuel_lps += pos(p.rate),
            (NetworkKind::Data, _) => r.data = true,
            _ => {}
        }
    }
    match &def.function {
        PartFunction::Generator { kw, fuel_lps } => {
            if !has[0][0] { r.supply_kw += pos(*kw); }
            if !has[1][1] { r.fuel_lps += pos(*fuel_lps); }
        }
        PartFunction::Engine(e) => {
            if e.kind == EngineKind::ElectricMotor { r.electric_motor = true; if !has[0][1] { r.draw_kw += pos(e.shaft_kw); } }
            else if !has[1][1] { r.fuel_lps += pos(e.fuel_lps); }
        }
        PartFunction::Battery { kwh, max_kw } => { if !has[0][2] { r.store_kwh += pos(*kwh); r.store_kw += pos(*max_kw); } }
        PartFunction::FuelTank { litres } => { if !has[1][2] && !has[1][0] { r.fuel_store_l += pos(*litres); } }
        PartFunction::Console => { r.console = true; r.data = true; }
        _ => {}
    }
    r
}

fn def_of<'c>(catalogue: &'c Catalogue, p: &PartRuntime) -> Option<&'c PartDef> { catalogue.parts.get_index(p.def.index()).map(|(_, d)| d) }

// ---------------------------------------------------------------------------------------------------------
// Union-find over compartments.

struct Uf(Vec<u32>);
impl Uf {
    fn new(n: usize) -> Self { Uf((0..n as u32).collect()) }
    fn find(&mut self, mut x: usize) -> usize {
        while self.0[x] as usize != x { let p = self.0[x] as usize; self.0[x] = self.0[p]; x = p; }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        if a >= self.0.len() || b >= self.0.len() { return; }
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb { let (lo, hi) = (ra.min(rb), ra.max(rb)); self.0[hi] = lo as u32; }
    }
    /// Component label per node: the lowest member index.
    fn labels(mut self) -> Vec<u16> {
        let n = self.0.len();
        let mut low = vec![u32::MAX; n];
        for i in 0..n { let r = self.find(i); if low[r] == u32::MAX { low[r] = i as u32; } }
        (0..n).map(|i| { let r = self.find(i); low[r].min(u16::MAX as u32 - 1) as u16 }).collect()
    }
}

fn breaker_blocks(net: &Networks, k: usize, c: &Conduit) -> bool {
    net.open_breakers.iter().any(|&(bk, cell)| bk as usize == k && c.cells.iter().any(|x| x.0 == cell))
}

/// Component labels over compartments for network `k`, using the conduits `keep` accepts.
fn components(net: &Networks, n: usize, k: usize, keep: &dyn Fn(&Conduit) -> bool) -> Vec<u16> {
    let mut uf = Uf::new(n);
    for c in &net.conduits {
        if kind_index(c.network) != k || c.state == ConduitState::Cut || !keep(c) || breaker_blocks(net, k, c) { continue; }
        uf.union(c.from.index(), c.to.index());
    }
    uf.labels()
}

fn node_count(net: &Networks) -> usize {
    let mut n = net.zones.len();
    for c in &net.conduits { n = n.max(c.from.index() + 1).max(c.to.index() + 1); }
    for &a in &net.attach { if a != NONE { n = n.max(a as usize + 1); } }
    n.min(MAX_COMPARTMENTS)
}

/// Recompute `zones` from the conducting conduits.
pub(crate) fn recompute_zones(net: &mut Networks) {
    let n = node_count(net);
    let labels: Vec<Vec<u16>> = (0..3).map(|k| components(net, n, k, &|_| true)).collect();
    net.zones = (0..n).map(|i| [labels[0][i], labels[1][i], labels[2][i]]).collect();
}

/// Restore a cut conduit at jury-rig capacity and recompute zones. Returns whether it changed.
pub(crate) fn jury_rig(net: &mut Networks, conduit: u16) -> bool {
    match net.conduits.get_mut(conduit as usize) {
        Some(c) if c.state == ConduitState::Cut => { c.state = ConduitState::JuryRigged; recompute_zones(net); true }
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------------------------
// Router.

/// Route every network for a design. Deterministic. Also the editor's animated conduit source and the X-ray
/// overlay data. Unit: `sim-core-networks`.
pub fn route(grid: &VesselGrid, compartments: &CompartmentSet, parts: &[PartRuntime], edits: &NetworkEdits, catalogue: &Catalogue) -> Networks {
    let mut net = Networks { zones: Vec::new(), ..Default::default() };
    let d = Dims::new(grid.size());
    let cls = classify(grid);
    let c2c = &compartments.cell_to_compartment;
    let m = compartments.compartments.len().min(MAX_COMPARTMENTS);
    net.zones = (0..m).map(|i| [i as u16; 3]).collect();
    let all_roles: Vec<Roles> = parts.iter().map(|p| def_of(catalogue, p).map(roles).unwrap_or_default()).collect();
    if cls.is_empty() || c2c.len() != d.len() || m == 0 {
        net.attach = vec![NONE; parts.len()];
        net.broken_pins = (0..edits.pinned_routes.len().min(u16::MAX as usize) as u16).collect();
        return net;
    }
    net.attach = parts.iter().map(|p| attach_of(p, compartments, &cls, d)).collect();
    let r = Router::new(grid, compartments, parts, catalogue, &cls, d);
    let mut conduits: Vec<Conduit> = Vec::new();
    let slot_of = |s: PartSlot| parts.iter().position(|p| p.slot == s);

    // Pinned routes first: they survive regeneration and pre-join their terminals.
    let mut pinned_links: Vec<(usize, u16, u16, Vec<CellIndex>)> = Vec::new();
    for (pi, pin) in edits.pinned_routes.iter().enumerate() {
        let k = kind_index(pin.network);
        let ok = (|| {
            let (ia, ib) = (slot_of(pin.from)?, slot_of(pin.to)?);
            if !all_roles[ia].on(k) || !all_roles[ib].on(k) { return None; }
            let (ca, cb) = (net.attach[ia], net.attach[ib]);
            if ca == NONE || cb == NONE || pin.cells.is_empty() { return None; }
            let mut cells = Vec::with_capacity(pin.cells.len());
            for (i, &p) in pin.cells.iter().enumerate() {
                let idx = grid.index(p)?;
                let inside = !grid.design.cells[idx.index()].is_empty() || (c2c[idx.index()] as usize) < m;
                if !inside { return None; }
                if i > 0 {
                    let q = pin.cells[i - 1];
                    let step = (p.x as i32 - q.x as i32).abs() + (p.y as i32 - q.y as i32).abs() + (p.z as i32 - q.z as i32).abs();
                    if step != 1 { return None; }
                }
                cells.push(idx);
            }
            Some((ca, cb, cells))
        })();
        match ok {
            Some((ca, cb, cells)) => {
                pinned_links.push((k, ca, cb, cells.clone()));
                conduits.push(Conduit { network: pin.network, from: CompartmentId(ca), to: CompartmentId(cb), cells, redundant: false, pinned: true, state: ConduitState::Intact });
            }
            None => net.broken_pins.push(pi.min(u16::MAX as usize) as u16),
        }
    }

    for k in 0..3 {
        let net_kind = KINDS[k];
        let mut terminals: BTreeSet<u16> = BTreeSet::new();
        let mut root: Option<(PartSlot, u16)> = None;
        for (i, p) in parts.iter().enumerate() {
            let a = net.attach[i];
            if a == NONE || !all_roles[i].on(k) { continue; }
            terminals.insert(a);
            let is_root = match k { 0 => all_roles[i].supply_kw > 0.0, 1 => all_roles[i].fuel_store_l > 0.0, _ => all_roles[i].console };
            if is_root && root.is_none_or(|(s, _)| p.slot < s) { root = Some((p.slot, a)); }
        }
        if root.is_none() && k == 0 {
            for (i, p) in parts.iter().enumerate() {
                if net.attach[i] != NONE && all_roles[i].store_kwh > 0.0 && root.is_none_or(|(s, _)| p.slot < s) { root = Some((p.slot, net.attach[i])); }
            }
        }
        let Some(&first) = terminals.iter().next() else { continue };
        let root = root.map_or(first, |r| r.1);
        // Pins and explicit data links pre-join terminals.
        let mut joined = Uf::new(m);
        for (pk, a, b, _) in &pinned_links { if *pk == k { joined.union(*a as usize, *b as usize); } }
        let mut connected: BTreeSet<u16> = BTreeSet::new();
        connected.insert(root);
        let close = |connected: &mut BTreeSet<u16>, joined: &mut Uf| {
            let roots: BTreeSet<usize> = connected.iter().map(|&c| joined.find(c as usize)).collect();
            for &t in terminals.iter() { if roots.contains(&joined.find(t as usize)) { connected.insert(t); } }
        };
        close(&mut connected, &mut joined);
        let mut links: Vec<(u16, u16, Vec<u16>)> = Vec::new();
        loop {
            if terminals.iter().all(|t| connected.contains(t)) { break; }
            let sources: Vec<u16> = connected.iter().copied().collect();
            let (dist, prev, origin) = r.dijkstra(&sources, &BTreeSet::new(), &BTreeSet::new());
            let best = terminals.iter().copied().filter(|t| !connected.contains(t)).filter(|&t| dist[t as usize] != u64::MAX).min_by_key(|&t| (dist[t as usize], t));
            let Some(t) = best else { break };
            let path = reconstruct(&prev, t);
            links.push((origin[t as usize], t, path));
            connected.insert(t);
            joined.union(t as usize, origin[t as usize] as usize);
            close(&mut connected, &mut joined);
        }
        if k == 2 {
            for &(a, b) in &edits.data_links {
                let (Some(ia), Some(ib)) = (slot_of(a), slot_of(b)) else { continue };
                let (ca, cb) = (net.attach[ia], net.attach[ib]);
                if ca == NONE || cb == NONE || ca == cb { continue; }
                let (dist, prev, _) = r.dijkstra(&[ca], &BTreeSet::new(), &BTreeSet::new());
                if dist[cb as usize] == u64::MAX { continue; }
                links.push((ca, cb, reconstruct(&prev, cb)));
            }
        }
        for (pk, a, b, pin_cells) in &pinned_links {
            let (a, b) = (*a, *b);
            if *pk != k || a == b { continue; }
            let (dist, prev, _) = r.dijkstra(&[a], &BTreeSet::new(), &BTreeSet::new());
            if dist[b as usize] == u64::MAX { continue; }
            // The pin is the primary; only the redundant path is generated.
            let path = reconstruct(&prev, b);
            if let Some(cells) = r.redundant(&path, pin_cells) {
                conduits.push(Conduit { network: net_kind, from: CompartmentId(a), to: CompartmentId(b), cells, redundant: true, pinned: false, state: ConduitState::Intact });
            }
        }
        for (s, t, path) in links {
            let Some(primary) = r.lay(&path, None) else { continue };
            if let Some(redundant) = r.redundant(&path, &primary) {
                conduits.push(Conduit { network: net_kind, from: CompartmentId(s), to: CompartmentId(t), cells: redundant, redundant: true, pinned: false, state: ConduitState::Intact });
            }
            conduits.push(Conduit { network: net_kind, from: CompartmentId(s), to: CompartmentId(t), cells: primary, redundant: false, pinned: false, state: ConduitState::Intact });
        }
    }
    conduits.sort_by(|a, b| (kind_index(a.network), a.from, a.to, a.redundant, a.pinned, &a.cells).cmp(&(kind_index(b.network), b.from, b.to, b.redundant, b.pinned, &b.cells)));
    conduits.truncate(MAX_CONDUITS);
    net.conduits = conduits;
    for b in &edits.breakers {
        if b.closed { continue; }
        if let Some(i) = grid.index(b.cell) { net.open_breakers.push((kind_index(b.network) as u8, i.0)); }
    }
    net.open_breakers.sort_unstable();
    net.open_breakers.dedup();
    recompute_zones(&mut net);
    net
}

fn reconstruct(prev: &[u16], t: u16) -> Vec<u16> {
    let mut path = vec![t];
    let mut c = t;
    while prev[c as usize] != NONE { c = prev[c as usize]; path.push(c); if path.len() > prev.len() { break; } }
    path.reverse();
    path
}

/// The compartment a part joins: its containing compartment, else the first compartment below its cells
/// (within 8 cells), else the one whose centroid is nearest its centre.
pub(crate) fn attach_of(p: &PartRuntime, set: &CompartmentSet, cls: &[u8], d: Dims) -> u16 {
    let c2c = &set.cell_to_compartment;
    let valid = |c: u16| (c as usize) < set.compartments.len().min(MAX_COMPARTMENTS) && !set.compartments[c as usize].is_absorbed();
    if let Some(c) = containment(p, c2c, cls, d) { if valid(c) { return c; } }
    let mut below: BTreeMap<u16, u32> = BTreeMap::new();
    for c in &p.cells {
        let mut i = c.index();
        if i >= c2c.len() { continue; }
        for _ in 0..8 {
            let Some(j) = d.step(i, Face::YNeg) else { break };
            if valid(c2c[j]) { *below.entry(c2c[j]).or_insert(0) += 1; break; }
            i = j;
        }
    }
    if let Some((&c, _)) = below.iter().fold(None, |acc: Option<(&u16, &u32)>, (k, v)| match acc { Some((_, bv)) if bv >= v => acc, _ => Some((k, v)) }) { return c; }
    let centre = if p.centre.is_finite() { p.centre } else { glam::DVec3::ZERO };
    set.compartments.iter().filter(|c| valid(c.id.0)).min_by(|a, b| a.centroid.distance_squared(centre).total_cmp(&b.centroid.distance_squared(centre)).then(a.id.cmp(&b.id))).map_or(NONE, |c| c.id.0)
}

/// Routing context for one design.
struct Router<'a> {
    d: Dims,
    cls: &'a [u8],
    c2c: &'a [u16],
    set: &'a CompartmentSet,
    m: usize,
    /// Graph edges per compartment: (neighbour, crossings index list into `crossings`).
    edges: Vec<BTreeMap<u16, Vec<usize>>>,
    crossings: Vec<Crossing>,
    /// Shell cells (watertight, within one cell of the compartment) per compartment, ascending.
    shells: Vec<Vec<u32>>,
    magazine: Vec<bool>,
    waterline_y: f64,
    /// Cells of magazine parts: never used by a conduit.
    banned: BTreeSet<u32>,
    hubs: Vec<Option<u32>>,
    /// Best crossing and its cost per adjacent pair (lower id first), computed once.
    best: BTreeMap<(u16, u16), (usize, u64)>,
    /// Dense per-cell Dijkstra scratch for shell runs, reset by generation stamp.
    scratch: std::cell::RefCell<Scratch>,
}

struct Scratch { gen: u32, stamp: Vec<u32>, dist: Vec<u32>, prev: Vec<u32>, shell_gen: u32, shell: Vec<u32> }

impl<'a> Router<'a> {
    fn new(grid: &VesselGrid, set: &'a CompartmentSet, parts: &[PartRuntime], catalogue: &Catalogue, cls: &'a [u8], d: Dims) -> Self {
        let c2c = &set.cell_to_compartment;
        let m = set.compartments.len().min(MAX_COMPARTMENTS);
        let mut crossings = wall_crossings(d, cls, c2c);
        let mut banned = BTreeSet::new();
        for p in parts {
            if matches!(p.state, PartState::Magazine { .. }) { for c in &p.cells { banned.insert(c.0); } }
        }
        crossings.extend(thick_crossings(d, cls, c2c, m, &crossings, &banned));
        let mut edges: Vec<BTreeMap<u16, Vec<usize>>> = vec![BTreeMap::new(); m];
        for (i, x) in crossings.iter().enumerate() {
            if (x.a as usize) < m && (x.b as usize) < m {
                edges[x.a as usize].entry(x.b).or_default().push(i);
                edges[x.b as usize].entry(x.a).or_default().push(i);
            }
        }
        // Shells.
        let mut shells: Vec<Vec<u32>> = vec![Vec::new(); m];
        for i in 0..d.len() {
            if cls[i] != BARRIER { continue; }
            let [x, y, z] = d.xyz(i);
            let mut near: [u16; 26] = [NONE; 26];
            let mut k = 0;
            for dy in -1i32..=1 { for dz in -1i32..=1 { for dx in -1i32..=1 {
                if dx == 0 && dy == 0 && dz == 0 { continue; }
                let (nx, ny, nz) = (x as i32 + dx, y as i32 + dy, z as i32 + dz);
                if nx < 0 || ny < 0 || nz < 0 || nx as usize >= d.sx || ny as usize >= d.sy || nz as usize >= d.sz { continue; }
                let c = c2c[d.idx(nx as usize, ny as usize, nz as usize)];
                if (c as usize) < m && !near[..k].contains(&c) { near[k] = c; k += 1; }
            } } }
            for &c in &near[..k] { shells[c as usize].push(i as u32); }
        }
        let magazine: Vec<bool> = set.compartments.iter().take(m).map(|c| c.magazine.is_some()).collect();
        let waterline_y = estimate_waterline(grid, set, parts, catalogue, cls, d);
        let hubs = (0..m).map(|c| hub(set, c as u16, cls, d)).collect();
        let n = d.len();
        let scratch = std::cell::RefCell::new(Scratch { gen: 0, stamp: vec![0; n], dist: vec![0; n], prev: vec![0; n], shell_gen: 0, shell: vec![0; n] });
        let mut r = Router { d, cls, c2c, set, m, edges, crossings, shells, magazine, waterline_y, banned, hubs, best: BTreeMap::new(), scratch };
        let empty = BTreeSet::new();
        let mut best = BTreeMap::new();
        for a in 0..m as u16 {
            for (&b, list) in &r.edges[a as usize] {
                if b < a { continue; }
                if let Some(x) = r.crossing(a, b, &empty).or_else(|| list.first().copied()) { best.insert((a, b), (x, r.edge_cost(a, b, x))); }
            }
        }
        r.best = best;
        r
    }

    fn low(&self, cell: u32) -> bool { self.d.centre(cell as usize).y < self.waterline_y + 0.25 * CELL_M }
    fn deck_low(&self, c: u16) -> bool { self.set.compartments.get(c as usize).is_some_and(|k| (k.max[1] as f64 + 1.5) * CELL_M < self.waterline_y) }

    /// The crossing a link uses between two adjacent compartments: highest wall, then nearest the midpoint of
    /// the two centroids, then lowest cell index. A crossing with a wall cell in `avoid` (the primary and the
    /// cells next to it) is ruled out.
    fn crossing(&self, a: u16, b: u16, avoid: &BTreeSet<u32>) -> Option<usize> {
        let list = self.edges.get(a as usize)?.get(&b)?;
        let (ca, cb) = (self.set.compartments[a as usize].centroid, self.set.compartments[b as usize].centroid);
        let mid = (ca + cb) * 0.5;
        let near = |w: u32| avoid.contains(&w);
        list.iter().copied().filter(|&i| !self.crossings[i].wall.iter().any(|&w| self.banned.contains(&w) || near(w))).min_by(|&i, &j| {
            let key = |x: usize| {
                let w = &self.crossings[x].wall;
                let top = w.iter().map(|&c| self.d.xyz(c as usize)[1]).max().unwrap_or(0);
                (Reverse(top), self.d.centre(w[0] as usize).distance_squared(mid), w[0])
            };
            let (ki, kj) = (key(i), key(j));
            ki.0.cmp(&kj.0).then(ki.1.total_cmp(&kj.1)).then(ki.2.cmp(&kj.2))
        })
    }

    fn edge_cost(&self, a: u16, b: u16, x: usize) -> u64 {
        let w = &self.crossings[x].wall;
        let ca = self.set.compartments[a as usize].centroid;
        let cb = self.set.compartments[b as usize].centroid;
        let p = self.d.centre(w[0] as usize);
        let man = |u: glam::DVec3, v: glam::DVec3| ((u - v).abs().element_sum() / CELL_M) as u64;
        let mut cost = 1 + man(ca, p) + man(p, cb) + w.len() as u64;
        if w.iter().any(|&c| self.low(c)) { cost += WATERLINE_COST; }
        cost
    }

    /// Dijkstra over compartments from `sources`. `banned` compartments are never entered and `banned_pairs`
    /// never crossed. Returns distance, predecessor and the source each node was reached from.
    fn dijkstra(&self, sources: &[u16], banned: &BTreeSet<u16>, banned_pairs: &BTreeSet<(u16, u16)>) -> (Vec<u64>, Vec<u16>, Vec<u16>) {
        let m = self.m;
        let mut dist = vec![u64::MAX; m];
        let mut prev = vec![NONE; m];
        let mut origin = vec![NONE; m];
        let mut heap = BinaryHeap::new();
        for &s in sources {
            if (s as usize) < m { dist[s as usize] = 0; origin[s as usize] = s; heap.push(Reverse((0u64, s))); }
        }
        while let Some(Reverse((dc, c))) = heap.pop() {
            if dc > dist[c as usize] { continue; }
            for (&nb, list) in &self.edges[c as usize] {
                if banned.contains(&nb) || banned_pairs.contains(&(c.min(nb), c.max(nb))) { continue; }
                let Some(&(_, base)) = self.best.get(&(c.min(nb), c.max(nb))) else { continue };
                let _ = list;
                let mut cost = base;
                // Entering a magazine or a low compartment is a last resort. A terminal that is one pays the
                // same on every path to it, so the choice of route is unchanged.
                if self.magazine[nb as usize] { cost += MAGAZINE_COST; }
                if self.deck_low(nb) { cost += WATERLINE_COST; }
                let nd = dc.saturating_add(cost);
                if nd < dist[nb as usize] {
                    dist[nb as usize] = nd;
                    prev[nb as usize] = c;
                    origin[nb as usize] = origin[c as usize];
                    heap.push(Reverse((nd, nb)));
                }
            }
        }
        (dist, prev, origin)
    }

    /// Lay a conduit along a compartment path. `avoid` (the primary's cells) is never used and its neighbours
    /// cost extra; the redundant conduit also uses the far hub of each end compartment.
    fn lay(&self, path: &[u16], avoid: Option<&BTreeSet<u32>>) -> Option<Vec<CellIndex>> {
        let mut cells: Vec<u32> = Vec::new();
        if path.is_empty() { return None; }
        let near = avoid.map(|a| dilate(self.d, a));
        let avoid_list: BTreeSet<u32> = match (avoid, near.as_ref()) { (Some(a), Some(n)) => a.union(n).copied().collect(), _ => BTreeSet::new() };
        let start_hub = |c: u16| if avoid.is_some() { far_hub(self.set, c, self.hubs[c as usize], self.cls, self.d) } else { self.hubs[c as usize] };
        let mut at = start_hub(path[0])?;
        cells.push(at);
        for w in path.windows(2) {
            let (a, b) = (w[0], w[1]);
            let x = if avoid.is_none() { self.best.get(&(a.min(b), a.max(b))).map(|e| e.0) } else { self.crossing(a, b, &avoid_list) }?;
            let cr = &self.crossings[x];
            let wall: Vec<u32> = if cr.a == a { cr.wall.clone() } else { cr.wall.iter().rev().copied().collect() };
            let seg = self.segment(a, at, wall[0], avoid, near.as_ref());
            cells.extend(seg.into_iter().skip(1));
            for &c in &wall[1..] { cells.push(c); }
            at = *wall.last().unwrap_or(&at);
        }
        let last = *path.last().unwrap_or(&path[0]);
        let end = start_hub(last)?;
        let seg = self.segment(last, at, end, avoid, near.as_ref());
        cells.extend(seg.into_iter().skip(1));
        cells.dedup();
        Some(cells.into_iter().map(CellIndex).collect())
    }

    /// The redundant conduit for a primary compartment path: a path avoiding the primary's intermediate
    /// compartments, using a crossing clear of the primary on any pair they share, laid clear of every primary
    /// cell. `None` when the graph allows no second path.
    fn redundant(&self, path: &[u16], primary: &[CellIndex]) -> Option<Vec<CellIndex>> {
        if path.len() < 2 { return None; }
        let (s, t) = (path[0], *path.last()?);
        let banned: BTreeSet<u16> = path[1..path.len() - 1].iter().copied().collect();
        let prim: BTreeSet<u32> = primary.iter().map(|c| c.0).collect();
        let prim_list: BTreeSet<u32> = prim.union(&dilate(self.d, &prim)).copied().collect();
        // A pair the primary crosses is usable only through a crossing clear of the primary.
        let mut banned_pairs = BTreeSet::new();
        for w in path.windows(2) {
            if self.crossing(w[0], w[1], &prim_list).is_none() { banned_pairs.insert((w[0].min(w[1]), w[0].max(w[1]))); }
        }
        let (dist, prev, _) = self.dijkstra(&[s], &banned, &banned_pairs);
        if dist.get(t as usize).copied().unwrap_or(u64::MAX) == u64::MAX { return None; }
        let alt = reconstruct(&prev, t);
        let cells = self.lay(&alt, Some(&prim))?;
        if cells.len() < 2 || cells.iter().any(|c| prim.contains(&c.0)) { return None; }
        Some(cells)
    }

    fn in_shell(&self, c: u16, cell: u32) -> bool { self.shells.get(c as usize).is_some_and(|s| s.binary_search(&cell).is_ok()) }

    /// Shortest run over a compartment's shell from `from` to `to` (both included), deterministic. Falls back
    /// to a straight Manhattan run when the shell does not connect them.
    fn segment(&self, c: u16, from: u32, to: u32, avoid: Option<&BTreeSet<u32>>, near: Option<&BTreeSet<u32>>) -> Vec<u32> {
        if from == to { return vec![from]; }
        let mut sc = self.scratch.borrow_mut();
        let sc = &mut *sc;
        let n = sc.stamp.len();
        if from as usize >= n || to as usize >= n { return vec![from, to]; }
        // Stamp this compartment's shell.
        sc.shell_gen = sc.shell_gen.wrapping_add(1);
        if sc.shell_gen == 0 { sc.shell.iter_mut().for_each(|x| *x = 0); sc.shell_gen = 1; }
        let sg = sc.shell_gen;
        if let Some(sh) = self.shells.get(c as usize) { for &x in sh { sc.shell[x as usize] = sg; } }
        sc.gen = sc.gen.wrapping_add(1);
        if sc.gen == 0 { sc.stamp.iter_mut().for_each(|x| *x = 0); sc.gen = 1; }
        let g = sc.gen;
        sc.stamp[from as usize] = g;
        sc.dist[from as usize] = 0;
        sc.prev[from as usize] = u32::MAX;
        let mut heap = BinaryHeap::new();
        heap.push(Reverse((0u32, from)));
        let mut expanded = 0usize;
        let mut found = false;
        while let Some(Reverse((dc, cell))) = heap.pop() {
            if sc.stamp[cell as usize] == g && dc > sc.dist[cell as usize] { continue; }
            if cell == to { found = true; break; }
            expanded += 1;
            if expanded > SEGMENT_EXPANSION_CAP { break; }
            for f in Face::ALL {
                let Some(j) = self.d.step(cell as usize, f) else { continue };
                if j != to as usize {
                    if sc.shell[j] != sg || self.banned.contains(&(j as u32)) { continue; }
                    if avoid.is_some_and(|a| a.contains(&(j as u32))) { continue; }
                }
                let mut step = 1u32;
                if self.low(j as u32) { step += CELL_LOW_COST; }
                if near.is_some_and(|nb| nb.contains(&(j as u32))) { step += NEAR_PRIMARY_COST; }
                let nd = dc.saturating_add(step);
                if sc.stamp[j] != g || nd < sc.dist[j] {
                    sc.stamp[j] = g;
                    sc.dist[j] = nd;
                    sc.prev[j] = cell;
                    heap.push(Reverse((nd, j as u32)));
                }
            }
        }
        if found {
            let mut out = vec![to];
            let mut c = to;
            while sc.prev[c as usize] != u32::MAX { c = sc.prev[c as usize]; out.push(c); if out.len() > n { break; } }
            out.reverse();
            return out;
        }
        manhattan(self.d, from, to)
    }
}

/// Crossings through walls the thin-wall scan cannot see, used only to join compartments it leaves apart.
///
/// [`wall_crossings`] links compartments through walls of one or two watertight cells. A designer can still
/// separate two compartments by a thicker block of structure or equipment (a tank flat, packed machinery, a
/// stern void behind a three-cell bulkhead), and the parts that join the far side then sit on an island the
/// conduits can never reach. Cables run through solid structure, so this scan adds straight runs of up to
/// [`MAX_THICK_WALL`] solid (watertight or open-equipment) cells between two compartments, but only where the
/// thin-wall graph puts them in different components and never through a magazine. A design whose thin graph is
/// already connected gains nothing and routes exactly as before. Scan order is ascending (cell index, axis).
fn thick_crossings(d: Dims, cls: &[u8], c2c: &[u16], m: usize, thin: &[Crossing], banned: &BTreeSet<u32>) -> Vec<Crossing> {
    let n = d.len();
    if cls.len() != n || c2c.len() != n || m == 0 { return Vec::new(); }
    let mut uf = Uf::new(m);
    for x in thin { uf.union(x.a as usize, x.b as usize); }
    let comp = |i: usize| -> Option<u16> { let c = c2c[i]; ((c as usize) < m).then_some(c) };
    // Nothing to join: every compartment already shares one component.
    let root = uf.find(0);
    if (1..m).all(|c| uf.find(c) == root) { return Vec::new(); }
    let mut out = Vec::new();
    for i in 0..n {
        let Some(ca) = comp(i) else { continue };
        for (axis, f) in [(0u8, Face::XPos), (1, Face::YPos), (2, Face::ZPos)] {
            let mut wall: Vec<u32> = Vec::new();
            let mut at = i;
            let far = loop {
                let Some(j) = d.step(at, f) else { break None };
                if cls[j] == crate::compartments::AIR { break comp(j); }
                if banned.contains(&(j as u32)) || wall.len() >= MAX_THICK_WALL { break None; }
                wall.push(j as u32);
                at = j;
            };
            let Some(cb) = far else { continue };
            if wall.is_empty() || cb == ca || uf.find(ca as usize) == uf.find(cb as usize) { continue; }
            out.push(Crossing { a: ca, b: cb, wall, axis });
        }
    }
    out
}

/// Longest straight run of solid cells [`thick_crossings`] lays a conduit through.
const MAX_THICK_WALL: usize = 16;

/// Chebyshev distance between two cells.
fn cheb(d: Dims, a: u32, b: u32) -> usize {
    let (p, q) = (d.xyz(a as usize), d.xyz(b as usize));
    (0..3).map(|k| p[k].abs_diff(q[k])).max().unwrap_or(0)
}

/// Every cell within one cell (26-neighbourhood) of the set, excluding the set.
fn dilate(d: Dims, set: &BTreeSet<u32>) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    for &c in set {
        let [x, y, z] = d.xyz(c as usize);
        for dy in -1i32..=1 { for dz in -1i32..=1 { for dx in -1i32..=1 {
            let (nx, ny, nz) = (x as i32 + dx, y as i32 + dy, z as i32 + dz);
            if nx < 0 || ny < 0 || nz < 0 || nx as usize >= d.sx || ny as usize >= d.sy || nz as usize >= d.sz { continue; }
            let j = d.idx(nx as usize, ny as usize, nz as usize) as u32;
            if !set.contains(&j) { out.insert(j); }
        } } }
    }
    out
}

/// Straight run Y, then X, then Z.
fn manhattan(d: Dims, from: u32, to: u32) -> Vec<u32> {
    let (mut p, q) = (d.xyz(from as usize), d.xyz(to as usize));
    let mut out = vec![from];
    for k in [1usize, 0, 2] {
        while p[k] != q[k] {
            if p[k] < q[k] { p[k] += 1 } else { p[k] -= 1 }
            out.push(d.idx(p[0], p[1], p[2]) as u32);
        }
    }
    out
}

/// Deckhead cell above the compartment air cell nearest its centroid.
fn hub(set: &CompartmentSet, c: u16, cls: &[u8], d: Dims) -> Option<u32> {
    let k = set.compartments.get(c as usize)?;
    if k.cells.is_empty() { return equipment_hub(set, c, cls, d); }
    let start = k.cells.iter().min_by(|a, b| d.centre(a.index()).distance_squared(k.centroid).total_cmp(&d.centre(b.index()).distance_squared(k.centroid)).then(a.cmp(b)))?.index();
    ceiling(start, cls, d)
}

/// Hub of a compartment with no air cells: a space wholly filled by open equipment (a pump boxed into the
/// double bottom). Its cells are the equipment's own; the lowest-index one stands in for the air.
fn equipment_hub(set: &CompartmentSet, c: u16, cls: &[u8], d: Dims) -> Option<u32> {
    let k = set.compartments.get(c as usize)?;
    if k.is_absorbed() { return None; }
    let c2c = &set.cell_to_compartment;
    let (lo, hi) = (k.min.map(|v| v.max(0) as usize), k.max.map(|v| v.max(0) as usize));
    for y in lo[1]..=hi[1].min(d.sy.saturating_sub(1)) {
        for z in lo[2]..=hi[2].min(d.sz.saturating_sub(1)) {
            for x in lo[0]..=hi[0].min(d.sx.saturating_sub(1)) {
                let i = d.idx(x, y, z);
                if c2c.get(i) == Some(&c) { return ceiling(i, cls, d).or(Some(i as u32)); }
            }
        }
    }
    None
}

/// Deckhead cell above the top-layer air cell farthest (in plan) from the primary hub.
fn far_hub(set: &CompartmentSet, c: u16, primary: Option<u32>, cls: &[u8], d: Dims) -> Option<u32> {
    let k = set.compartments.get(c as usize)?;
    let Some(h) = primary else { return hub(set, c, cls, d) };
    let [hx, _, hz] = d.xyz(h as usize);
    let top = k.cells.iter().map(|c| d.xyz(c.index())[1]).max()?;
    let start = k.cells.iter().filter(|c| d.xyz(c.index())[1] == top).max_by(|a, b| {
        let da = { let [x, _, z] = d.xyz(a.index()); x.abs_diff(hx) + z.abs_diff(hz) };
        let db = { let [x, _, z] = d.xyz(b.index()); x.abs_diff(hx) + z.abs_diff(hz) };
        da.cmp(&db).then(b.cmp(a))
    })?.index();
    ceiling(start, cls, d)
}

fn ceiling(mut i: usize, cls: &[u8], d: Dims) -> Option<u32> {
    loop {
        let j = d.step(i, Face::YPos)?;
        if cls[j] == BARRIER { return Some(j as u32); }
        i = j;
    }
}

/// Design waterline estimate for routing: the height at which the hull envelope (solid and compartment cells)
/// displaces the vessel's mass with fluids at their current load.
fn estimate_waterline(grid: &VesselGrid, set: &CompartmentSet, parts: &[PartRuntime], catalogue: &Catalogue, cls: &[u8], d: Dims) -> f64 {
    let dry = crate::mass::dry_mass(grid, parts, catalogue);
    let full = crate::mass::with_fluids(&dry, parts, set, catalogue);
    let rho = catalogue.tuning.buoyancy.water_density;
    let rho = if rho.is_finite() && rho > 0.0 { rho } else { 1025.0 };
    let need = full.mass_kg / rho;
    let mut layer = vec![0u64; d.sy];
    for i in 0..d.len() {
        if cls[i] != 0 || (set.cell_to_compartment[i] as usize) < MAX_COMPARTMENTS { layer[d.xyz(i)[1]] += 1; }
    }
    let mut acc = 0.0;
    for (y, &n) in layer.iter().enumerate() {
        let v = n as f64 * content::cell::CELL_VOLUME_M3;
        if v > 0.0 && acc + v >= need { return (y as f64 + (need - acc) / v) * CELL_M; }
        acc += v;
    }
    d.sy as f64 * CELL_M
}

/// Property the CI test asserts for the auto-set rule (L205): every consumer connected, two paths where the
/// adjacency graph allows. Returns the slots of unconnected consumers. Unit: `sim-core-networks`.
///
/// A consumer is a part that draws power or fuel or sits on Data (other than a console). It is connected when
/// its compartment shares a zone (conduits not cut, breakers honoured) with a source of that network: a
/// generator or battery, a fuel tank, a console. A network with no source leaves its consumers unconnected.
pub fn unconnected_consumers(net: &Networks, parts: &[PartRuntime], compartments: &CompartmentSet, catalogue: &Catalogue) -> Vec<PartSlot> {
    unconnected_on(net, parts, compartments, catalogue, &[0, 1, 2])
}

/// [`unconnected_consumers`] on one network only (Power, Fuel or Data). Unit: `sim-core-networks`.
pub fn unconnected_consumers_on(net: &Networks, parts: &[PartRuntime], compartments: &CompartmentSet, catalogue: &Catalogue, kind: NetworkKind) -> Vec<PartSlot> {
    let k = match kind { NetworkKind::Power => 0, NetworkKind::Fuel => 1, NetworkKind::Data => 2 };
    unconnected_on(net, parts, compartments, catalogue, &[k])
}

fn unconnected_on(net: &Networks, parts: &[PartRuntime], compartments: &CompartmentSet, catalogue: &Catalogue, kinds: &[usize]) -> Vec<PartSlot> {
    let n = node_count(net).max(compartments.compartments.len().min(MAX_COMPARTMENTS));
    let attach: Vec<u16> = attach_list(net, parts, compartments);
    let rl: Vec<Roles> = parts.iter().map(|p| def_of(catalogue, p).map(roles).unwrap_or_default()).collect();
    let mut out = Vec::new();
    for &k in kinds {
        let labels = components(net, n, k, &|_| true);
        let sources: BTreeSet<u16> = parts.iter().enumerate().filter(|(i, _)| rl[*i].source(k) && attach[*i] != NONE && (attach[*i] as usize) < n).map(|(i, _)| labels[attach[i] as usize]).collect();
        for (i, p) in parts.iter().enumerate() {
            if !rl[i].consumer(k) { continue; }
            let a = attach[i];
            if a == NONE || a as usize >= n || !sources.contains(&labels[a as usize]) { out.push(p.slot); }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn attach_list(net: &Networks, parts: &[PartRuntime], compartments: &CompartmentSet) -> Vec<u16> {
    if net.attach.len() == parts.len() { return net.attach.clone(); }
    parts.iter().map(|p| p.compartment.map_or(NONE, |c| c.0)).map(|c| if (c as usize) < compartments.compartments.len().min(MAX_COMPARTMENTS) { c } else { NONE }).collect()
}

/// Cut every conduit passing through any of the destroyed cells and recompute zones. Returns indices of
/// conduits newly cut. Unit: `sim-core-networks`.
///
/// A jury-rigged conduit hit again is cut again.
pub fn cut_conduits(net: &mut Networks, destroyed: &[CellIndex]) -> Vec<u16> {
    if destroyed.is_empty() { return Vec::new(); }
    let mut hit: Vec<u32> = destroyed.iter().map(|c| c.0).collect();
    hit.sort_unstable();
    hit.dedup();
    let (lo, hi) = (hit[0], *hit.last().unwrap_or(&hit[0]));
    let mut out = Vec::new();
    for (i, c) in net.conduits.iter_mut().enumerate() {
        if c.state == ConduitState::Cut { continue; }
        if c.cells.iter().any(|x| x.0 >= lo && x.0 <= hi && hit.binary_search(&x.0).is_ok()) {
            c.state = ConduitState::Cut;
            out.push(i.min(u16::MAX as usize) as u16);
        }
    }
    if !out.is_empty() { recompute_zones(net); }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Supply.

/// Service of one part on one network this tick.
#[derive(Clone, Copy, Debug)]
struct Reach { connected: bool, primary: bool, cap: f64 }

/// Reachability of each compartment to a source of network `k`.
fn reach(net: &mut Networks, n: usize, k: usize, sources: &BTreeSet<u16>, cap: f64) -> Vec<Reach> {
    let [all, prim, intact] = labels(net, n, k);
    let hit = |labels: &[u16]| -> BTreeSet<u16> { sources.iter().filter(|&&s| (s as usize) < n).map(|&s| labels[s as usize]).collect() };
    let (sa, sp, si) = (hit(&all), hit(&prim), hit(&intact));
    (0..n).map(|c| {
        let connected = sa.contains(&all[c]);
        Reach { connected, primary: sp.contains(&prim[c]), cap: if connected && !si.contains(&intact[c]) { cap } else { 1.0 } }
    }).collect()
}

fn link_of(r: Option<Reach>) -> LinkState { match r { Some(r) if r.primary => LinkState::Network, Some(r) if r.connected => LinkState::Redundant, _ => LinkState::Buffer } }

/// One tick of supply: per zone sum generation and storage, serve consumers in ascending priority number then
/// ascending slot, shed from priority 5 upward on shortage (L287), draw fuel for engines and generators, run
/// the fallback chain and buffers, and write each part's [`PartLinks`]. Heavy-damage parts output 50% (L561).
/// Returns slots whose link state or shed flag changed. Unit: `sim-core-networks`.
pub fn solve_tick(net: &mut Networks, parts: &mut [PartRuntime], catalogue: &Catalogue, tuning: &NetworkTuning, dt: f64) -> Vec<PartSlot> {
    let dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
    let attach = if net.attach.len() == parts.len() { net.attach.clone() } else { parts.iter().map(|p| p.compartment.map_or(NONE, |c| c.0)).collect() };
    let n = node_count(net).max(attach.iter().filter(|&&a| a != NONE).map(|&a| a as usize + 1).max().unwrap_or(0)).min(MAX_COMPARTMENTS);
    let rl: Vec<Roles> = parts.iter().map(|p| def_of(catalogue, p).map(roles).unwrap_or_default()).collect();
    let caps: Vec<[f64; 2]> = parts.iter().map(|p| {
        let own = def_of(catalogue, p).and_then(|d| d.buffer_s);
        [pos(own.unwrap_or(tuning.battery_s)), pos(own.unwrap_or(tuning.day_tank_s))]
    }).collect();
    let buffer_cap = |i: usize, which: usize| -> f64 { caps[i][which.min(1)] };
    let jury = catalogue.tuning.crew.jury_rig_capacity;
    let jury = if jury.is_finite() { jury.clamp(0.0, 1.0) } else { 0.5 };
    let before: Vec<(LinkState, LinkState, LinkState, bool)> = parts.iter().map(|p| (p.links.power, p.links.fuel, p.links.data, p.links.shed)).collect();
    if !net.primed {
        for i in 0..parts.len() {
            if rl[i].consumer(0) { parts[i].links.battery_s = buffer_cap(i, 0); }
            if rl[i].consumer(1) { parts[i].links.day_tank_s = buffer_cap(i, 1); }
        }
        net.primed = true;
    }
    let works = |p: &PartRuntime| p.level != DamageLevel::Destroyed;
    let node = |i: usize| -> Option<usize> { let a = attach[i]; (a != NONE && (a as usize) < n).then_some(a as usize) };
    let zone_all: Vec<Vec<u16>> = (0..3).map(|k| { let [a, _, _] = labels(net, n, k); a }).collect();

    // ---- Fuel ----
    let fuel_sources: BTreeSet<u16> = (0..parts.len()).filter(|&i| rl[i].fuel_store_l > 0.0 && works(&parts[i]) && matches!(parts[i].state, PartState::FuelTank { litres } if litres > 0.0)).filter_map(|i| node(i).map(|c| c as u16)).collect();
    let fuel_reach = reach(net, n, 1, &fuel_sources, jury);
    let mut pool: BTreeMap<u16, f64> = BTreeMap::new();
    for i in 0..parts.len() {
        if rl[i].fuel_store_l <= 0.0 || !works(&parts[i]) { continue; }
        let (Some(c), PartState::FuelTank { litres }) = (node(i), &parts[i].state) else { continue };
        *pool.entry(zone_all[1][c]).or_insert(0.0) += pos(*litres);
    }
    // Demand per connected consumer in litres this tick.
    let mut fuel_demand = vec![0.0f64; parts.len()];
    let mut zone_demand: BTreeMap<u16, f64> = BTreeMap::new();
    for i in 0..parts.len() {
        if !rl[i].consumer(1) || !works(&parts[i]) { continue; }
        let load = match (&parts[i].state, def_of(catalogue, &parts[i]).map(|d| &d.function)) {
            (PartState::Engine { throttle, .. }, _) => throttle.abs().clamp(IDLE_FUEL, 1.0),
            (PartState::Generator { output_kw }, Some(PartFunction::Generator { kw, .. })) if *kw > 0.0 => (output_kw / kw).clamp(IDLE_FUEL, 1.0),
            (PartState::Generator { .. }, _) => IDLE_FUEL,
            _ => 1.0,
        };
        let litres = rl[i].fuel_lps * load * dt;
        fuel_demand[i] = if litres.is_finite() { litres } else { 0.0 };
        if let Some(c) = node(i) { if fuel_reach[c].connected { *zone_demand.entry(zone_all[1][c]).or_insert(0.0) += fuel_demand[i] * fuel_reach[c].cap; } }
    }
    let mut drawn: BTreeMap<u16, f64> = BTreeMap::new();
    let mut fuel_flow = 0.0;
    for i in 0..parts.len() {
        let p = &mut parts[i];
        if !works(p) { continue; }
        if !rl[i].on(1) { p.links.fuel = LinkState::Network; p.links.fuel_supplied = 1.0; continue; }
        if !rl[i].consumer(1) {
            // A tank: on the network where it sits.
            p.links.fuel = link_of(node(i).map(|c| fuel_reach[c]).or(Some(Reach { connected: true, primary: true, cap: 1.0 })));
            p.links.fuel_supplied = 1.0;
            continue;
        }
        let r = node(i).map(|c| fuel_reach[c]);
        let cap_s = buffer_cap(i, 1);
        let mut supplied = 0.0;
        if let Some(r) = r.filter(|r| r.connected) {
            let z = zone_all[1][node(i).unwrap_or(0)];
            let want = zone_demand.get(&z).copied().unwrap_or(0.0);
            let have = pool.get(&z).copied().unwrap_or(0.0);
            let frac = if want > 0.0 { (have / want).min(1.0) } else { 1.0 };
            supplied = frac * r.cap;
            *drawn.entry(z).or_insert(0.0) += fuel_demand[i] * supplied;
            p.links.fuel = link_of(Some(r));
        } else {
            p.links.fuel = LinkState::Buffer;
        }
        if p.links.fuel == LinkState::Buffer {
            // Cut off from every tank: the day tank runs the part at rated flow until it is dry.
            if p.links.day_tank_s > 0.0 && dt > 0.0 {
                let take = dt.min(p.links.day_tank_s);
                p.links.day_tank_s -= take;
                supplied = take / dt;
            }
        } else if supplied >= 1.0 - 1e-9 {
            p.links.day_tank_s = (p.links.day_tank_s + dt).min(cap_s);
        }
        if supplied <= 0.0 && p.links.fuel == LinkState::Buffer { p.links.fuel = LinkState::Dead; }
        p.links.fuel_supplied = supplied.clamp(0.0, 1.0);
        if dt > 0.0 { fuel_flow += fuel_demand[i] * p.links.fuel_supplied / dt; }
    }
    // Draw from tanks in ascending slot per zone.
    let mut tank_order: Vec<usize> = (0..parts.len()).filter(|&i| rl[i].fuel_store_l > 0.0).collect();
    tank_order.sort_by_key(|&i| parts[i].slot);
    let mut fuel_l = 0.0;
    for i in tank_order {
        let c = node(i);
        let ok = works(&parts[i]);
        if let PartState::FuelTank { litres } = &mut parts[i].state {
            if ok { if let Some(c) = c {
                let z = zone_all[1][c];
                if let Some(need) = drawn.get_mut(&z) { let take = need.min(pos(*litres)); *litres = pos(*litres - take); *need -= take; }
            } }
            fuel_l += pos(*litres);
        }
    }
    net.fuel_flow_lps = fuel_flow;
    net.fuel_l = fuel_l;

    // ---- Power ----
    let power_sources: BTreeSet<u16> = (0..parts.len()).filter(|&i| works(&parts[i]) && (rl[i].supply_kw > 0.0 || (rl[i].store_kwh > 0.0 && matches!(parts[i].state, PartState::Battery { kwh } if kwh > 0.0)))).filter_map(|i| node(i).map(|c| c as u16)).collect();
    let power_reach = reach(net, n, 0, &power_sources, jury);
    let mut gen_avail = vec![0.0f64; parts.len()];
    let mut zone_gen: BTreeMap<u16, f64> = BTreeMap::new();
    let mut zone_bat: BTreeMap<u16, f64> = BTreeMap::new();
    let mut bat_avail = vec![0.0f64; parts.len()];
    for i in 0..parts.len() {
        let p = &parts[i];
        if !works(p) { continue; }
        let Some(c) = node(i) else { continue };
        let z = zone_all[0][c];
        if rl[i].supply_kw > 0.0 {
            let fuel = if rl[i].consumer(1) { p.links.fuel_supplied } else { 1.0 };
            gen_avail[i] = rl[i].supply_kw * p.output_factor() * fuel;
            *zone_gen.entry(z).or_insert(0.0) += gen_avail[i];
        }
        if let (true, PartState::Battery { kwh }) = (rl[i].store_kwh > 0.0, &p.state) {
            let rate = if rl[i].store_kw > 0.0 { rl[i].store_kw } else { rl[i].store_kwh };
            let by_charge = if dt > 0.0 { pos(*kwh) * 3600.0 / dt } else { 0.0 };
            bat_avail[i] = (rate * p.output_factor()).min(by_charge);
            *zone_bat.entry(z).or_insert(0.0) += bat_avail[i];
        }
    }
    // Consumers by zone in (priority, slot) order.
    let mut demand = vec![0.0f64; parts.len()];
    let mut by_zone: BTreeMap<u16, Vec<(u8, PartSlot, usize)>> = BTreeMap::new();
    let mut total_demand = 0.0;
    for i in 0..parts.len() {
        let p = &parts[i];
        if !rl[i].consumer(0) || !works(p) { continue; }
        let mut d = rl[i].draw_kw;
        if rl[i].electric_motor { if let PartState::Engine { throttle, .. } = p.state { d *= throttle.abs().min(1.0); } }
        demand[i] = if d.is_finite() { d.max(0.0) } else { 0.0 };
        total_demand += demand[i];
        let Some(c) = node(i) else { continue };
        if !power_reach[c].connected { continue; }
        let pr = if (1..=5).contains(&p.priority) { p.priority } else { def_of(catalogue, p).map(|d| d.power_priority).filter(|x| (1..=5).contains(x)).unwrap_or(5) };
        by_zone.entry(zone_all[0][c]).or_default().push((pr, p.slot, i));
    }
    let mut served = vec![0.0f64; parts.len()];
    let mut shed = vec![false; parts.len()];
    let mut zone_used: BTreeMap<u16, f64> = BTreeMap::new();
    for (z, list) in by_zone.iter_mut() {
        list.sort();
        let mut left = zone_gen.get(z).copied().unwrap_or(0.0) + zone_bat.get(z).copied().unwrap_or(0.0);
        let mut short = false;
        let mut used = 0.0;
        let mut idx = 0;
        while idx < list.len() {
            let pr = list[idx].0;
            let end = list[idx..].iter().position(|e| e.0 != pr).map_or(list.len(), |x| idx + x);
            for &(_, _, i) in &list[idx..end] {
                let c = node(i).unwrap_or(0);
                let want = demand[i] * power_reach[c].cap;
                if short { shed[i] = demand[i] > 0.0; continue; }
                if want <= left + 1e-9 { served[i] = want; left -= want; used += want; }
                else { served[i] = left.max(0.0); used += served[i]; left = 0.0; short = true; if served[i] <= 0.0 { shed[i] = true; } }
            }
            idx = end;
        }
        zone_used.insert(*z, used);
    }
    // Share the load over generators, then batteries; charge batteries from surplus.
    let mut gen_used_frac: BTreeMap<u16, f64> = BTreeMap::new();
    let mut bat_draw: BTreeMap<u16, f64> = BTreeMap::new();
    let mut bat_charge: BTreeMap<u16, f64> = BTreeMap::new();
    let zones: BTreeSet<u16> = zone_gen.keys().chain(zone_bat.keys()).copied().collect();
    for z in zones {
        let g = zone_gen.get(&z).copied().unwrap_or(0.0);
        let used = zone_used.get(&z).copied().unwrap_or(0.0);
        let from_gen = used.min(g);
        gen_used_frac.insert(z, if g > 0.0 { from_gen / g } else { 0.0 });
        bat_draw.insert(z, (used - from_gen).max(0.0));
        bat_charge.insert(z, (g - from_gen).max(0.0));
    }
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by_key(|&i| parts[i].slot);
    let mut generation = 0.0;
    for &i in &order {
        let Some(c) = node(i) else { continue };
        let z = zone_all[0][c];
        let ok = works(&parts[i]);
        if rl[i].supply_kw > 0.0 {
            generation += gen_avail[i];
            if let PartState::Generator { output_kw } = &mut parts[i].state { *output_kw = if ok { gen_avail[i] * gen_used_frac.get(&z).copied().unwrap_or(0.0) } else { 0.0 }; }
        }
        if rl[i].store_kwh > 0.0 && ok {
            let cap = rl[i].store_kwh;
            if let PartState::Battery { kwh } = &mut parts[i].state {
                if let Some(need) = bat_draw.get_mut(&z) {
                    let take = need.min(bat_avail[i]);
                    *kwh = pos(*kwh - take * dt / 3600.0);
                    *need -= take;
                }
                if let Some(spare) = bat_charge.get_mut(&z) {
                    let rate = if rl[i].store_kw > 0.0 { rl[i].store_kw } else { cap };
                    let room = if dt > 0.0 { pos(cap - *kwh) * 3600.0 / dt } else { 0.0 };
                    let put = spare.min(rate).min(room);
                    *kwh = (*kwh + put * dt / 3600.0).min(cap);
                    *spare -= put;
                }
            }
        }
    }
    let mut supplied_total = 0.0;
    for i in 0..parts.len() {
        let p = &mut parts[i];
        if !works(p) { continue; }
        if !rl[i].on(0) { p.links.power = LinkState::Network; p.links.power_supplied = 1.0; p.links.shed = false; continue; }
        let r = node(i).map(|c| power_reach[c]);
        if !rl[i].consumer(0) {
            p.links.power = link_of(r.or(Some(Reach { connected: true, primary: true, cap: 1.0 })));
            p.links.power_supplied = 1.0;
            p.links.shed = false;
            continue;
        }
        let cap_s = buffer_cap(i, 0);
        if r.is_some_and(|r| r.connected) {
            p.links.power = link_of(r);
            p.links.shed = shed[i];
            let frac = if demand[i] > 0.0 { served[i] / demand[i] } else if shed[i] { 0.0 } else { 1.0 };
            p.links.power_supplied = frac.clamp(0.0, 1.0);
            supplied_total += served[i];
            if !shed[i] && p.links.power_supplied >= 1.0 - 1e-9 { p.links.battery_s = (p.links.battery_s + dt).min(cap_s); }
        } else {
            p.links.shed = false;
            if p.links.battery_s > 0.0 {
                p.links.battery_s = (p.links.battery_s - dt).max(0.0);
                p.links.power = LinkState::Buffer;
                p.links.power_supplied = 1.0;
            } else {
                p.links.power = LinkState::Dead;
                p.links.power_supplied = 0.0;
            }
        }
    }
    net.generation_kw = generation;
    net.demand_kw = total_demand;
    net.supplied_kw = supplied_total;

    // ---- Data ----
    let consoles: BTreeSet<u16> = (0..parts.len()).filter(|&i| {
        let p = &parts[i];
        rl[i].console && works(p) && (!rl[i].consumer(0) || (p.links.power != LinkState::Dead && p.links.power_supplied > 0.0 && !p.links.shed))
    }).filter_map(|i| node(i).map(|c| c as u16)).collect();
    let data_reach = reach(net, n, 2, &consoles, 1.0);
    for i in 0..parts.len() {
        let p = &mut parts[i];
        if !works(p) { continue; }
        if !rl[i].on(2) { p.links.data = LinkState::Network; continue; }
        p.links.data = match node(i).map(|c| data_reach[c]) {
            Some(r) if r.primary => LinkState::Network,
            Some(r) if r.connected => LinkState::Redundant,
            _ => LinkState::LocalControl,
        };
    }

    // Destroyed parts are dead on every network.
    for p in parts.iter_mut().filter(|p| !works(p)) {
        p.links.power = LinkState::Dead;
        p.links.fuel = LinkState::Dead;
        p.links.data = LinkState::Dead;
        p.links.power_supplied = 0.0;
        p.links.fuel_supplied = 0.0;
        p.links.shed = false;
        if let PartState::Generator { output_kw } = &mut p.state { *output_kw = 0.0; }
    }
    let mut changed: Vec<PartSlot> = parts.iter().zip(before.iter()).filter(|(p, b)| (p.links.power, p.links.fuel, p.links.data, p.links.shed) != **b).map(|(p, _)| p.slot).collect();
    changed.sort_unstable();
    changed
}
