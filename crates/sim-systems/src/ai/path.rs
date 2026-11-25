//! Unit `sys-ai-path` (hard: bitwise-identical paths across runs and between macOS and Windows).
//! Naval pathing in three layers. DESIGN.md L764-770.
//!
//! 1. Route: A* (`pathfinding::directed::astar`, crate 4.16) on a coarse grid of the sea map with 50 m cells
//!    (`sim_core::terrain::PATH_CELL_M`): 8-connected, octile heuristic, u32 costs of 10 per straight step and
//!    14 per diagonal, plus 20 for a cell next to land, ties broken by cell index. The grid stores water depth
//!    per cell. Each hull class has a clearance mask that blocks cells within half its beam plus 10 m of land,
//!    or shallower than its draft plus 2 m.
//! 2. Smoothing: line-of-sight string-pulling over the clearance mask, then a Dubins turn at the hull's
//!    turning radius for its planned speed at each remaining corner (`dubins_paths` 3.3, f64, libm). A corner
//!    whose arc leaves clear water keeps its unsmoothed waypoint. Final approaches to a pose (formation slot,
//!    station, berth) use `DubinsPath::shortest_from`.
//! 3. Steering: turning-circle-aware pure pursuit with a lookahead of at least one turning radius.
//!
//! No reversing paths: backing off is a scripted manoeuvre. Budget: 2,000 A* node expansions per 60 Hz tick
//! across all hulls, first in first out (`Budgets::astar_expansions`; the fallback is 4,000). The pathfinding
//! crate's search is not resumable, so a search runs to completion when it reaches the head of the queue, its
//! expansion count is charged as debt, and neither its result nor the next search is released until the debt
//! is repaid at the per-tick budget. That keeps timing a pure function of work units.
//! Fallbacks noted by the spec: 25 m cells if the harbour leg fails, then hand-written hybrid A* for confined
//! water only.
//!
//! Implementation notes (choices where the spec is silent):
//! - Grid frame: `origin` is the world (x, z) of the minimum corner of cell 0; cell `(cx, cz)` has index
//!   `cx + cols * cz` and covers `[origin + c * cell, origin + (c + 1) * cell)`.
//! - Land is a cell whose depth is 0 or less (or not a number). "Within half the beam plus 10 m of land" is
//!   measured from the cell centre to the nearest land cell's square, so for most hulls the cells next to
//!   land stay open and carry the +20 near-land cost. `near_land` marks open cells with a blocked 8-neighbour
//!   (the hull's own coastline: land, shoal or clearance).
//! - Diagonal steps need both orthogonal neighbours open (no corner cutting).
//! - Ties are broken by cell index exactly: the search runs on u64 costs scaled by the cell count, and the
//!   heuristic adds the cell index, so equal-f entries pop lowest index first. The added term is below one
//!   cost unit, so the route stays optimal.
//! - Open cells are labelled into connected components when a mask is built, so an unreachable goal fails
//!   without flooding the grid. A blocked or off-grid start or goal snaps to the nearest open cell (the goal
//!   within the start's component) inside 2 km. A goal that is navigable but inside the clearance zone (a
//!   berth against a quay) is kept as a final leg from the snapped cell.
//! - String-pulling only shortcuts across near-land cells when the A* span it replaces touched them too, so
//!   the +20 preference survives smoothing.
//! - Dubins corners are tangent fillets `DubinsPath::shortest_from(a, b, r)` between the points one tangent
//!   length either side of the corner; a fillet that does not fit its legs keeps the corner waypoint, as does
//!   one whose arc (checked at half-cell spacing) touches a blocked cell. Arcs become waypoints every 15
//!   degrees of turn.
//! - Pure pursuit: lookahead `max(turn radius, 3 s of travel, 30 m)` measured along the route from the
//!   ship's projection onto it; the last leg is extended past the final waypoint so a ship that finishes a
//!   route holds the last leg's course.
use crate::prelude::*;
use core::f64::consts::{FRAC_PI_2, PI, TAU};
use dubins_paths::f64::{DubinsPath, PosRot};
use dubins_paths::SegmentType;
use pathfinding::directed::astar::astar;
use sim_core::bits::BitSet;
use sim_core::terrain::{DepthGrid, Terrain, PATH_CELL_M};

/// Straight step cost (L764).
pub const COST_STRAIGHT: u64 = 10;
/// Diagonal step cost (L764).
pub const COST_DIAGONAL: u64 = 14;
/// Extra cost for entering a cell next to land (L764).
pub const COST_NEAR_LAND: u64 = 20;
/// Clearance from land beyond half the beam, metres (L764).
pub const LAND_CLEARANCE_M: f64 = 10.0;
/// Water under the keel required beyond the draft, metres (L764).
pub const KEEL_CLEARANCE_M: f64 = 2.0;
/// Snap radius for a blocked start or goal, in cells (2 km at 50 m).
const SNAP_RADIUS_CELLS: i64 = 40;
/// Largest grid the planner accepts (8192 x 8192 cells); keeps u64 search costs far from overflow.
const MAX_CELLS: u64 = 1 << 26;
/// Turn radius used when a request carries none.
const DEFAULT_TURN_RADIUS_M: f64 = 100.0;
/// Arc waypoint spacing in radians of turn.
pub(crate) const ARC_STEP_RAD: f64 = PI / 12.0;
/// Pure-pursuit lookahead floor and time ahead.
const MIN_LOOKAHEAD_M: f64 = 30.0;
const LOOKAHEAD_S: f64 = 3.0;
/// A single-point route counts as reached inside this radius.
const ARRIVE_M: f64 = 25.0;

/// Derived data rebuilt on demand: never serialised and never part of equality.
#[derive(Clone, Debug, Default)]
pub(crate) struct Derived<T>(pub(crate) T);
impl<T> PartialEq for Derived<T> {
    fn eq(&self, _: &Self) -> bool { true }
}

/// Per-class blocked cells over the depth grid.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClearanceMask {
    pub beam_m: f64,
    pub draft_m: f64,
    pub cols: u32,
    pub rows: u32,
    pub blocked: BitSet,
    pub near_land: BitSet,
    /// Connected-component label per cell (0 = blocked); rebuilt after a keyframe restore.
    #[serde(skip)]
    pub(crate) components: Derived<Vec<u32>>,
}

/// The shared navigation grid of a sortie and the masks built so far, keyed by (beam, draft) in decimetres.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NavGrid { pub depth: Option<DepthGridData>, pub masks: BTreeMap<(u32, u32), ClearanceMask> }
/// Serialisable copy of `sim_core::terrain::DepthGrid`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DepthGridData { pub cell_m: f64, pub cols: u32, pub rows: u32, pub origin: [f64; 2], pub depth: Vec<f32> }
impl From<&DepthGrid> for DepthGridData {
    fn from(g: &DepthGrid) -> Self { Self { cell_m: g.cell_m, cols: g.cols, rows: g.rows, origin: g.origin, depth: g.depth.clone() } }
}

/// A planned route: smoothed waypoints with the turn radius used and the pose to finish at, if any.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Route { pub waypoints: Vec<DVec2>, pub turn_radius_m: f64, pub final_heading_rad: Option<f64>, pub length_m: f64, pub valid: bool }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PathRequest { pub id: u32, pub requester: EntityId, pub from: DVec2, pub from_heading_rad: f64, pub to: DVec2, pub to_heading_rad: Option<f64>, pub beam_m: f64, pub draft_m: f64, pub turn_radius_m: f64, pub issued_tick: u64 }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PathPlanner {
    pub grid: NavGrid,
    /// First in, first out.
    pub queue: Vec<PathRequest>,
    /// Finished routes not yet collected: (request id, requester, route).
    pub done: Vec<(u32, EntityId, Route)>,
    /// Expansions still owed before the head result is released.
    pub debt: u32,
    pub next_id: u32,
    /// The searched head result waiting for its debt to be repaid.
    #[serde(default)]
    pub(crate) pending: Option<(u32, EntityId, Route)>,
    /// Latest request per requester: (requester, id, goal key), so an unchanged re-request does not throw away
    /// a result that is still being paid for or not yet collected.
    #[serde(default)]
    pub(crate) latest: Vec<(EntityId, u32, [u64; 6])>,
}
impl PathPlanner {
    /// Build the grid from the terrain. Unit: `sys-ai-path`.
    pub fn new(terrain: &Terrain) -> PathPlanner { Self::with_grid(DepthGridData::from(&terrain.depth_grid(PATH_CELL_M))) }
    /// Planner over an existing depth grid (also the 25 m fallback grid).
    pub fn with_grid(depth: DepthGridData) -> PathPlanner { PathPlanner { grid: NavGrid { depth: Some(depth), masks: BTreeMap::new() }, ..Default::default() } }
    /// Build (once) the clearance mask a hull class uses, so the first request does not pay for it.
    pub fn ensure_mask(&mut self, beam_m: f64, draft_m: f64) {
        let key = mask_key(beam_m, draft_m);
        let Some(depth) = self.grid.depth.as_ref() else { return };
        let Some(g) = GridView::of_data(depth) else { return };
        let mask = self.grid.masks.entry(key).or_insert_with(|| build_mask(&g, key.0 as f64 / 10.0, key.1 as f64 / 10.0));
        if mask.components.0.len() != g.n() { mask.components.0 = label_components(&g, mask); }
    }
    /// Queue a request; returns its id. A newer request from the same requester replaces its queued one.
    /// Unit: `sys-ai-path`.
    pub fn request(&mut self, req: PathRequest) -> u32 {
        let who = req.requester;
        let key = request_key(&req);
        if let Some(&(_, old_id, old_key)) = self.latest.iter().find(|l| l.0 == who) {
            let in_flight = self.pending.as_ref().is_some_and(|p| p.0 == old_id) || self.done.iter().any(|d| d.0 == old_id);
            if old_key == key && in_flight { return old_id; }
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let mut req = req;
        req.id = id;
        match self.latest.iter_mut().find(|l| l.0 == who) { Some(l) => *l = (who, id, key), None => self.latest.push((who, id, key)) }
        self.done.retain(|d| d.1 != who);
        if self.pending.as_ref().is_some_and(|p| p.1 == who) { self.pending = None; }
        if let Some(q) = self.queue.iter_mut().find(|q| q.requester == who) { *q = req; } else { self.queue.push(req); }
        id
    }
    /// Spend up to `budget` expansions on the queue. Returns expansions charged this tick.
    /// Unit: `sys-ai-path`.
    pub fn step(&mut self, budget: u32, tick: u64) -> u32 {
        let _ = tick;
        let mut avail = budget;
        let mut spent = 0u32;
        loop {
            if self.debt > 0 {
                let pay = self.debt.min(avail);
                self.debt -= pay;
                avail -= pay;
                spent += pay;
                if self.debt > 0 { break; }
            }
            if let Some(p) = self.pending.take() { self.done.push(p); }
            if avail == 0 || self.queue.is_empty() { break; }
            let req = self.queue.remove(0);
            let (route, expansions) = self.plan(&req);
            self.pending = Some((req.id, req.requester, route));
            self.debt = expansions.max(1);
        }
        spent
    }
    /// Take a finished route for a requester, if one is ready. Unit: `sys-ai-path`.
    pub fn take(&mut self, requester: EntityId) -> Option<Route> {
        let i = self.done.iter().position(|d| d.1 == requester)?;
        Some(self.done.remove(i).2)
    }

    /// Run one request to completion: (route, A* expansions).
    fn plan(&mut self, req: &PathRequest) -> (Route, u32) {
        let r = sane_radius(req.turn_radius_m);
        let final_heading = req.to_heading_rad.filter(|h| h.is_finite()).map(wrap_2pi);
        if !req.from.is_finite() || !req.to.is_finite() { return (invalid_route(r, final_heading), 0); }
        let Some(depth) = self.grid.depth.as_ref() else { return (straight_route(req, r, final_heading), 0) };
        let Some(g) = GridView::of_data(depth) else { return (straight_route(req, r, final_heading), 0) };
        let key = mask_key(req.beam_m, req.draft_m);
        let mask = self.grid.masks.entry(key).or_insert_with(|| build_mask(&g, key.0 as f64 / 10.0, key.1 as f64 / 10.0));
        if mask.components.0.len() != g.n() { mask.components.0 = label_components(&g, mask); }
        let (cells, expansions) = astar_route(depth, mask, req.from, req.to);
        let route = match cells { Some(c) => smooth(depth, mask, &c, req), None => invalid_route(r, final_heading) };
        (route, expansions)
    }
}

/// What makes two requests the same plan apart from the start: goal, final heading, hull numbers.
fn request_key(r: &PathRequest) -> [u64; 6] {
    let (b, d) = mask_key(r.beam_m, r.draft_m);
    [r.to.x.to_bits(), r.to.y.to_bits(), r.to_heading_rad.map_or(u64::MAX, f64::to_bits), b as u64, d as u64, r.turn_radius_m.to_bits()]
}

/// Mask key: beam and draft in decimetres, rounded up (bad values count as 0).
fn mask_key(beam_m: f64, draft_m: f64) -> (u32, u32) {
    let dm = |v: f64| if v.is_finite() && v > 0.0 { (v * 10.0).ceil().min(100_000.0) as u32 } else { 0 };
    (dm(beam_m), dm(draft_m))
}

fn sane_radius(r: f64) -> f64 { if r.is_finite() && r >= 1.0 { r.min(20_000.0) } else { DEFAULT_TURN_RADIUS_M } }

fn invalid_route(r: f64, final_heading: Option<f64>) -> Route { Route { waypoints: Vec::new(), turn_radius_m: r, final_heading_rad: final_heading, length_m: 0.0, valid: false } }

/// Open water with no grid: a straight leg, with a Dubins final approach when a pose is wanted.
fn straight_route(req: &PathRequest, r: f64, final_heading: Option<f64>) -> Route {
    let mut w = vec![req.from, req.to];
    if let Some(hf) = final_heading {
        let qh = if req.from_heading_rad.is_finite() { req.from_heading_rad } else { bearing(req.to - req.from) };
        if let Ok(p) = DubinsPath::shortest_from(posrot_heading(req.from, qh), posrot_heading(req.to, hf), r) {
            let mut pts = Vec::new();
            dubins_points(&p, r * ARC_STEP_RAD, &mut pts);
            w.truncate(1);
            w.extend(pts);
            if let Some(last) = w.last_mut() { *last = req.to; }
        }
    }
    finish_route(w, r, final_heading)
}

fn finish_route(w: Vec<DVec2>, r: f64, final_heading: Option<f64>) -> Route {
    let mut out: Vec<DVec2> = Vec::with_capacity(w.len());
    let last = w.len().saturating_sub(1);
    for (i, p) in w.into_iter().enumerate() {
        match out.last_mut() {
            Some(q) if q.distance_squared(p) < 0.25 => { if i == last && out.len() > 1 { *out.last_mut().unwrap() = p; } }
            _ => out.push(p),
        }
    }
    let length_m = out.windows(2).map(|s| s[0].distance(s[1])).fold(0.0, |a, b| a + b);
    Route { waypoints: out, turn_radius_m: r, final_heading_rad: final_heading, length_m, valid: true }
}

// ---------------------------------------------------------------------------------------------------------
// Grid view

#[derive(Clone, Copy)]
struct GridView<'a> { cell: f64, cols: i64, rows: i64, ox: f64, oz: f64, depth: &'a [f32] }
impl<'a> GridView<'a> {
    fn new(cell_m: f64, cols: u32, rows: u32, origin: [f64; 2], depth: &'a [f32]) -> Option<Self> {
        let n = cols as u64 * rows as u64;
        if !(cell_m.is_finite() && cell_m > 0.0) || n == 0 || n > MAX_CELLS || depth.len() as u64 != n || !origin[0].is_finite() || !origin[1].is_finite() { return None; }
        Some(Self { cell: cell_m, cols: cols as i64, rows: rows as i64, ox: origin[0], oz: origin[1], depth })
    }
    fn of_data(d: &'a DepthGridData) -> Option<Self> { Self::new(d.cell_m, d.cols, d.rows, d.origin, &d.depth) }
    fn n(&self) -> usize { (self.cols * self.rows) as usize }
    fn inside(&self, x: i64, y: i64) -> bool { x >= 0 && y >= 0 && x < self.cols && y < self.rows }
    fn idx(&self, x: i64, y: i64) -> u32 { (x + self.cols * y) as u32 }
    fn xy(&self, i: u32) -> (i64, i64) { let i = i as i64; (i % self.cols, i / self.cols) }
    fn centre(&self, i: u32) -> DVec2 { let (x, y) = self.xy(i); DVec2::new(self.ox + (x as f64 + 0.5) * self.cell, self.oz + (y as f64 + 0.5) * self.cell) }
    fn grid_coords(&self, p: DVec2) -> (f64, f64) { ((p.x - self.ox) / self.cell, (p.y - self.oz) / self.cell) }
    fn cell_of(&self, p: DVec2) -> Option<u32> {
        if !p.is_finite() { return None; }
        let (gx, gy) = self.grid_coords(p);
        let (x, y) = (gx.floor(), gy.floor());
        if x < 0.0 || y < 0.0 || x >= self.cols as f64 || y >= self.rows as f64 { return None; }
        Some(self.idx(x as i64, y as i64))
    }
    /// Nearest in-grid cell coordinates of a finite point.
    fn clamp_xy(&self, p: DVec2) -> (i64, i64) {
        let (gx, gy) = self.grid_coords(p);
        let x = gx.floor().clamp(0.0, (self.cols - 1) as f64) as i64;
        let y = gy.floor().clamp(0.0, (self.rows - 1) as f64) as i64;
        (x, y)
    }
    fn depth(&self, i: u32) -> f64 { let d = self.depth[i as usize] as f64; if d.is_nan() { f64::NEG_INFINITY } else { d } }
    fn is_land(&self, i: u32) -> bool { !(self.depth(i) > 0.0) }
}

/// Neighbour offsets in ascending index order: (dx, dy, diagonal).
const NEIGHBOURS: [(i64, i64, bool); 8] = [(-1, -1, true), (0, -1, false), (1, -1, true), (-1, 0, false), (1, 0, false), (-1, 1, true), (0, 1, false), (1, 1, true)];

fn open(mask: &ClearanceMask, i: u32) -> bool { !mask.blocked.get(i as usize) }

/// Open neighbours of an open cell under the move rules, ascending index.
fn neighbours(g: &GridView<'_>, mask: &ClearanceMask, i: u32, mut f: impl FnMut(u32, bool)) {
    let (x, y) = g.xy(i);
    for &(dx, dy, diag) in &NEIGHBOURS {
        let (nx, ny) = (x + dx, y + dy);
        if !g.inside(nx, ny) { continue; }
        let j = g.idx(nx, ny);
        if !open(mask, j) { continue; }
        if diag && !(open(mask, g.idx(x + dx, y)) && open(mask, g.idx(x, y + dy))) { continue; }
        f(j, diag);
    }
}

fn build_mask(g: &GridView<'_>, beam_m: f64, draft_m: f64) -> ClearanceMask {
    let beam = if beam_m.is_finite() && beam_m > 0.0 { beam_m } else { 0.0 };
    let draft = if draft_m.is_finite() && draft_m > 0.0 { draft_m } else { 0.0 };
    let n = g.n();
    let mut blocked = BitSet::new(n);
    let need = draft + KEEL_CLEARANCE_M;
    for i in 0..n as u32 {
        if !(g.depth(i) >= need) { blocked.set(i as usize, true); }
    }
    // Clearance from land: stamp around coastal land cells (every land cell nearest to water is coastal).
    let r = beam * 0.5 + LAND_CLEARANCE_M;
    let half = g.cell * 0.5;
    let w = ((r + half) / g.cell).ceil() as i64;
    let mut offsets = Vec::new();
    for dy in -w..=w {
        for dx in -w..=w {
            let gx = (dx.abs() as f64 * g.cell - half).max(0.0);
            let gy = (dy.abs() as f64 * g.cell - half).max(0.0);
            if gx * gx + gy * gy < r * r { offsets.push((dx, dy)); }
        }
    }
    for i in 0..n as u32 {
        if !g.is_land(i) { continue; }
        let (x, y) = g.xy(i);
        let coastal = NEIGHBOURS.iter().any(|&(dx, dy, _)| g.inside(x + dx, y + dy) && !g.is_land(g.idx(x + dx, y + dy)));
        if !coastal { continue; }
        for &(dx, dy) in &offsets {
            if g.inside(x + dx, y + dy) { blocked.set(g.idx(x + dx, y + dy) as usize, true); }
        }
    }
    let mut near_land = BitSet::new(n);
    for i in 0..n as u32 {
        if blocked.get(i as usize) { continue; }
        let (x, y) = g.xy(i);
        if NEIGHBOURS.iter().any(|&(dx, dy, _)| g.inside(x + dx, y + dy) && blocked.get(g.idx(x + dx, y + dy) as usize)) { near_land.set(i as usize, true); }
    }
    let mut mask = ClearanceMask { beam_m: beam, draft_m: draft, cols: g.cols as u32, rows: g.rows as u32, blocked, near_land, components: Derived(Vec::new()) };
    mask.components.0 = label_components(g, &mask);
    mask
}

/// Connected components of open cells under the move rules; labels from 1 in ascending seed index.
fn label_components(g: &GridView<'_>, mask: &ClearanceMask) -> Vec<u32> {
    let n = g.n();
    let mut label = vec![0u32; n];
    let mut next = 1u32;
    let mut queue: Vec<u32> = Vec::new();
    for seed in 0..n as u32 {
        if label[seed as usize] != 0 || !open(mask, seed) { continue; }
        label[seed as usize] = next;
        queue.clear();
        queue.push(seed);
        let mut head = 0;
        while head < queue.len() {
            let c = queue[head];
            head += 1;
            neighbours(g, mask, c, |j, _| {
                if label[j as usize] == 0 { label[j as usize] = next; queue.push(j); }
            });
        }
        next = next.wrapping_add(1).max(1);
    }
    label
}

fn mask_fits(g: &GridView<'_>, mask: &ClearanceMask) -> bool {
    mask.cols as i64 == g.cols && mask.rows as i64 == g.rows && mask.blocked.len >= g.n() && mask.blocked.words.len() * 64 >= g.n()
}

/// Nearest open cell to `p` (lowest (distance, index)), optionally inside one component.
fn snap(g: &GridView<'_>, mask: &ClearanceMask, p: DVec2, comp: Option<(&[u32], u32)>) -> Option<u32> {
    if !p.is_finite() { return None; }
    let ok = |i: u32| open(mask, i) && comp.is_none_or(|(c, l)| c[i as usize] == l);
    let (cx, cy) = g.clamp_xy(p);
    let c0 = g.idx(cx, cy);
    if ok(c0) && g.cell_of(p) == Some(c0) { return Some(c0); }
    let mut best: Option<(f64, u32)> = None;
    let mut found_at: Option<i64> = None;
    let consider = |x: i64, y: i64, best: &mut Option<(f64, u32)>| {
        if !g.inside(x, y) { return; }
        let i = g.idx(x, y);
        if !ok(i) { return; }
        let d2 = g.centre(i).distance_squared(p);
        if best.is_none_or(|(bd, bi)| d2 < bd || (d2 == bd && i < bi)) { *best = Some((d2, i)); }
    };
    consider(cx, cy, &mut best);
    if best.is_some() { found_at = Some(0); }
    for r in 1..=SNAP_RADIUS_CELLS {
        if found_at.is_some_and(|f| r > f + 1) { break; }
        for y in (cy - r)..=(cy + r) {
            if y == cy - r || y == cy + r {
                for x in (cx - r)..=(cx + r) { consider(x, y, &mut best); }
            } else {
                consider(cx - r, y, &mut best);
                consider(cx + r, y, &mut best);
            }
        }
        if best.is_some() && found_at.is_none() { found_at = Some(r); }
    }
    best.map(|b| b.1)
}

/// Clearance mask for a hull (L764). Unit: `sys-ai-path`.
pub fn clearance_mask(depth: &DepthGrid, beam_m: f64, draft_m: f64) -> ClearanceMask {
    match GridView::new(depth.cell_m, depth.cols, depth.rows, depth.origin, &depth.depth) {
        Some(g) => build_mask(&g, beam_m, draft_m),
        None => ClearanceMask { beam_m, draft_m, cols: depth.cols, rows: depth.rows, ..Default::default() },
    }
}

/// Layer 1: A* over the mask; returns cell indices from start to goal and the expansion count, or `None` when
/// no route exists. Unit: `sys-ai-path`.
pub fn astar_route(grid: &DepthGridData, mask: &ClearanceMask, from: DVec2, to: DVec2) -> (Option<Vec<u32>>, u32) {
    let Some(g) = GridView::of_data(grid) else { return (None, 0) };
    if !mask_fits(&g, mask) { return (None, 0); }
    let comps: Option<&[u32]> = (mask.components.0.len() == g.n()).then_some(&mask.components.0[..]);
    let Some(start) = snap(&g, mask, from, None) else { return (None, 0) };
    // An open goal is used as given (and fails when it lies in another component); a blocked or off-grid
    // goal snaps to the nearest open cell the start can reach.
    let goal = match g.cell_of(to) {
        Some(c) if open(mask, c) => c,
        _ => match snap(&g, mask, to, comps.map(|c| (c, c[start as usize]))) { Some(c) => c, None => return (None, 0) },
    };
    if let Some(c) = comps { if c[start as usize] != c[goal as usize] { return (None, 0); } }
    if start == goal { return (Some(vec![start]), 0); }
    let k = g.n() as u64;
    let (gx, gy) = g.xy(goal);
    let mut expansions = 0u32;
    let result = astar(
        &start,
        |&i: &u32| {
            expansions = expansions.saturating_add(1);
            let mut out = [(0u32, 0u64); 8];
            let mut cnt = 0;
            neighbours(&g, mask, i, |j, diag| {
                let base = if diag { COST_DIAGONAL } else { COST_STRAIGHT } + if mask.near_land.get(j as usize) { COST_NEAR_LAND } else { 0 };
                out[cnt] = (j, base * k);
                cnt += 1;
            });
            out.into_iter().take(cnt)
        },
        |&i: &u32| {
            let (x, y) = g.xy(i);
            let (dx, dy) = ((x - gx).unsigned_abs(), (y - gy).unsigned_abs());
            let (lo, hi) = if dx < dy { (dx, dy) } else { (dy, dx) };
            (COST_STRAIGHT * hi + (COST_DIAGONAL - COST_STRAIGHT) * lo) * k + i as u64
        },
        |&i: &u32| i == goal,
    );
    (result.map(|(path, _)| path), expansions)
}

// ---------------------------------------------------------------------------------------------------------
// Line of sight

/// Visit every cell the segment a-b touches (supercover; both side cells at an exact corner). Returns false
/// when the segment leaves the grid or `visit` rejects a cell.
fn segment_cells(g: &GridView<'_>, a: DVec2, b: DVec2, mut visit: impl FnMut(u32) -> bool) -> bool {
    if !a.is_finite() || !b.is_finite() { return false; }
    let (ax, ay) = g.grid_coords(a);
    let (bx, by) = g.grid_coords(b);
    let (mut x, mut y) = (ax.floor() as i64, ay.floor() as i64);
    let (ex, ey) = (bx.floor() as i64, by.floor() as i64);
    let mut nx = (ex - x).unsigned_abs();
    let mut ny = (ey - y).unsigned_abs();
    if nx + ny > 4 * (g.cols + g.rows) as u64 { return false; }
    let mut check = |x: i64, y: i64| g.inside(x, y) && visit(g.idx(x, y));
    if !check(x, y) { return false; }
    let (dx, dy) = (bx - ax, by - ay);
    let sx = if ex > x { 1 } else { -1 };
    let sy = if ey > y { 1 } else { -1 };
    let tdx = if dx != 0.0 { 1.0 / dx.abs() } else { f64::INFINITY };
    let tdy = if dy != 0.0 { 1.0 / dy.abs() } else { f64::INFINITY };
    let mut tmx = if dx > 0.0 { (x as f64 + 1.0 - ax) * tdx } else if dx < 0.0 { (ax - x as f64) * tdx } else { f64::INFINITY };
    let mut tmy = if dy > 0.0 { (y as f64 + 1.0 - ay) * tdy } else if dy < 0.0 { (ay - y as f64) * tdy } else { f64::INFINITY };
    while nx > 0 || ny > 0 {
        if ny == 0 || (nx > 0 && tmx < tmy) {
            x += sx; tmx += tdx; nx -= 1;
        } else if nx == 0 || tmy < tmx {
            y += sy; tmy += tdy; ny -= 1;
        } else {
            if !check(x + sx, y) || !check(x, y + sy) { return false; }
            x += sx; y += sy; tmx += tdx; tmy += tdy; nx -= 1; ny -= 1;
        }
        if !check(x, y) { return false; }
    }
    true
}

fn polyline_clear(g: &GridView<'_>, pts: &[DVec2], accept: &impl Fn(u32) -> bool) -> bool {
    pts.windows(2).all(|s| segment_cells(g, s[0], s[1], |c| accept(c)))
}

/// Greedy farthest-visible string pull (galloping then bisection; every kept shortcut is verified).
fn string_pull(g: &GridView<'_>, mask: &ClearanceMask, pts: &[(DVec2, u32)]) -> Vec<DVec2> {
    let n = pts.len();
    if n <= 2 { return pts.iter().map(|p| p.0).collect(); }
    let mut pre = vec![0u32; n + 1];
    for (i, p) in pts.iter().enumerate() { pre[i + 1] = pre[i] + mask.near_land.get(p.1 as usize) as u32; }
    let los = |a: usize, b: usize| {
        let coast_ok = pre[b + 1] - pre[a] > 0;
        segment_cells(g, pts[a].0, pts[b].0, |c| open(mask, c) && (coast_ok || !mask.near_land.get(c as usize)))
    };
    let mut out = vec![pts[0].0];
    let mut a = 0;
    while a < n - 1 {
        let mut lo = a + 1;
        let mut hi = n;
        let mut step = 1;
        loop {
            let j = (lo + step).min(n - 1);
            if j == lo { break; }
            if los(a, j) { lo = j; step *= 2; } else { hi = j; break; }
        }
        while hi - lo > 1 {
            let m = (lo + hi) / 2;
            if los(a, m) { lo = m; } else { hi = m; }
        }
        out.push(pts[lo].0);
        a = lo;
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Dubins helpers. The Dubins plane is world (x, z); theta is the angle from +x towards +z, so a compass
// heading h maps to theta = h - pi/2.

pub(crate) fn posrot_heading(p: DVec2, heading: f64) -> PosRot { PosRot::from_floats(p.x, p.y, heading - FRAC_PI_2) }
fn posrot_dir(p: DVec2, d: DVec2) -> PosRot { PosRot::from_floats(p.x, p.y, libm::atan2(d.y, d.x)) }

/// Points along a Dubins path after its start, ending at its end: turns every `step` metres, straights once.
pub(crate) fn dubins_points(path: &DubinsPath, step: f64, out: &mut Vec<DVec2>) {
    let types = path.path_type.to_segment_types();
    let step = if step.is_finite() && step > 0.1 { step } else { 0.1 };
    let mut t0 = 0.0;
    for (i, ty) in types.iter().enumerate() {
        let len = path.segment_length(i);
        if !(len > 1e-9) { continue; }
        let steps = if *ty == SegmentType::S { 1 } else { ((len / step).ceil() as usize).clamp(1, 4096) };
        for s in 1..=steps {
            let q = path.sample(t0 + len * s as f64 / steps as f64);
            out.push(DVec2::new(q.x(), q.y()));
        }
        t0 += len;
    }
}

/// Sample a Dubins path densely (for the clearance check) and coarsely (for waypoints).
fn dubins_clear_points(g: &GridView<'_>, path: &DubinsPath, start: DVec2, accept: &impl Fn(u32) -> bool) -> Option<Vec<DVec2>> {
    if !path.length().is_finite() { return None; }
    let mut dense = vec![start];
    dubins_points(path, (g.cell * 0.5).min(path.rho * ARC_STEP_RAD), &mut dense);
    if !polyline_clear(g, &dense, accept) { return None; }
    let mut coarse = Vec::new();
    dubins_points(path, path.rho * ARC_STEP_RAD, &mut coarse);
    Some(coarse)
}

/// Replace each corner with a tangent fillet at radius `r` where it fits its legs and stays in clear water.
fn smooth_corners(g: &GridView<'_>, mask: &ClearanceMask, w: &[DVec2], r: f64, reserve_last: bool) -> Vec<DVec2> {
    let m = w.len();
    if m < 3 { return w.to_vec(); }
    let accept = |c: u32| open(mask, c);
    let mut out = vec![w[0]];
    let mut used_in = 0.0;
    for k in 1..m - 1 {
        let (p0, p1, p2) = (w[k - 1], w[k], w[k + 1]);
        let (a, b) = (p1 - p0, p2 - p1);
        let (la, lb) = (a.length(), b.length());
        let keep = |out: &mut Vec<DVec2>, used_in: &mut f64| { out.push(p1); *used_in = 0.0; };
        if !(la > 1e-6 && lb > 1e-6) { keep(&mut out, &mut used_in); continue; }
        let (da, db) = (a / la, b / lb);
        let dot = da.dot(db);
        let cross = da.perp_dot(db).abs();
        if dot > 0.999_999 || 1.0 + dot < 1e-6 { keep(&mut out, &mut used_in); continue; }
        let s = r * cross / (1.0 + dot);
        let avail_in = la - used_in;
        let avail_out = if k == m - 2 && !reserve_last { lb } else { lb * 0.5 };
        if !(s <= avail_in && s <= avail_out) { keep(&mut out, &mut used_in); continue; }
        let (pa, pb) = (p1 - da * s, p1 + db * s);
        let fillet = DubinsPath::shortest_from(posrot_dir(pa, da), posrot_dir(pb, db), r).ok().filter(|p| p.length() <= 2.0 * s + 1e-3);
        match fillet.and_then(|p| dubins_clear_points(g, &p, pa, &accept)) {
            Some(mut pts) => {
                out.push(pa);
                if let Some(l) = pts.last_mut() { *l = pb; }
                out.extend(pts);
                used_in = s;
            }
            None => keep(&mut out, &mut used_in),
        }
    }
    out.push(w[m - 1]);
    out
}

/// Layer 2: string-pull and add Dubins turns. Unit: `sys-ai-path`.
pub fn smooth(grid: &DepthGridData, mask: &ClearanceMask, cells: &[u32], req: &PathRequest) -> Route {
    let r = sane_radius(req.turn_radius_m);
    let final_heading = req.to_heading_rad.filter(|h| h.is_finite()).map(wrap_2pi);
    let Some(g) = GridView::of_data(grid) else { return invalid_route(r, final_heading) };
    if cells.is_empty() || !mask_fits(&g, mask) || cells.iter().any(|&c| c as usize >= g.n()) || !req.from.is_finite() || !req.to.is_finite() {
        return invalid_route(r, final_heading);
    }
    let mut pts: Vec<(DVec2, u32)> = cells.iter().map(|&c| (g.centre(c), c)).collect();
    let mut prefix = None;
    if g.cell_of(req.from) == Some(cells[0]) { pts[0].0 = req.from; } else { prefix = Some(req.from); }
    let last_cell = cells[cells.len() - 1];
    let mut suffix = None;
    let mut relax_radius = 0.0;
    if g.cell_of(req.to) == Some(last_cell) {
        if pts.len() == 1 && prefix.is_none() { pts.push((req.to, last_cell)); } else { pts.last_mut().unwrap().0 = req.to; }
    } else if let Some(tc) = g.cell_of(req.to) {
        // A goal in the clearance zone but deep enough (a berth): finish with a short leg from the snapped cell,
        // only when every cell on that leg is water at least as deep as the draft.
        let wet = |c: u32| !g.is_land(c) && g.depth(c) >= mask.draft_m;
        if wet(tc) && segment_cells(&g, g.centre(last_cell), req.to, wet) {
            relax_radius = g.centre(last_cell).distance(req.to) + g.cell;
            suffix = Some(req.to);
        }
    }
    let pulled = string_pull(&g, mask, &pts);
    let mut w: Vec<DVec2> = Vec::with_capacity(pulled.len() + 2);
    w.extend(prefix);
    w.extend(pulled);
    w.extend(suffix);
    w.dedup_by(|b, a| a.distance_squared(*b) < 0.25);
    let mut out = smooth_corners(&g, mask, &w, r, final_heading.is_some());
    if let Some(hf) = final_heading {
        let n = out.len();
        if n >= 2 {
            let (q, goal) = (out[n - 2], out[n - 1]);
            if q.distance(goal) > 1e-3 {
                let qh = if n == 2 && req.from_heading_rad.is_finite() { req.from_heading_rad } else { bearing(goal - q) };
                let accept = |c: u32| open(mask, c) || (suffix.is_some() && !g.is_land(c) && g.depth(c) >= mask.draft_m && g.centre(c).distance(goal) <= relax_radius);
                if let Ok(p) = DubinsPath::shortest_from(posrot_heading(q, qh), posrot_heading(goal, hf), r) {
                    if let Some(mut pts) = dubins_clear_points(&g, &p, q, &accept) {
                        if let Some(l) = pts.last_mut() { *l = goal; }
                        out.truncate(n - 1);
                        out.extend(pts);
                    }
                }
            }
        }
    }
    finish_route(out, r, final_heading)
}

// ---------------------------------------------------------------------------------------------------------
// Steering

/// Compass bearing of a world (x, z) direction, radians in [0, 2 pi).
pub(crate) fn bearing(d: DVec2) -> f64 { wrap_2pi(libm::atan2(d.x, -d.y)) }
/// Unit world (x, z) direction of a compass heading.
pub(crate) fn heading_dir(h: f64) -> DVec2 { DVec2::new(libm::sin(h), -libm::cos(h)) }
pub(crate) fn wrap_2pi(a: f64) -> f64 {
    if !a.is_finite() { return 0.0; }
    let r = a % TAU;
    let r = if r < 0.0 { r + TAU } else { r };
    if r >= TAU { 0.0 } else { r }
}
/// Angle wrapped into (-pi, pi].
pub(crate) fn wrap_pi(a: f64) -> f64 { let r = wrap_2pi(a); if r > PI { r - TAU } else { r } }

/// Full pure-pursuit result; `pure_pursuit` is the contract's view of it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pursuit {
    pub heading_rad: f64,
    pub speed_factor: f64,
    pub waypoint: usize,
    /// The ship has passed the final waypoint (or reached a single-point route).
    pub finished: bool,
    /// Course to hold once finished: the last leg's direction, else the current heading.
    pub final_heading_rad: f64,
}

/// Parameter of the projection of `p` on segment a-b (unclamped; 1 for a zero-length segment).
pub(crate) fn proj_t(a: DVec2, b: DVec2, p: DVec2) -> f64 {
    let ab = b - a;
    let l2 = ab.length_squared();
    if !(l2 > 1e-12) { return 1.0; }
    (p - a).dot(ab) / l2
}

pub(crate) fn pursue(route: &[DVec2], pos: DVec2, heading_rad: f64, speed_mps: f64, turn_radius_m: f64, waypoint: usize) -> Pursuit {
    let h = wrap_2pi(heading_rad);
    let n = route.len();
    if n == 0 || !pos.is_finite() || route.iter().any(|p| !p.is_finite()) {
        return Pursuit { heading_rad: h, speed_factor: 0.0, waypoint: waypoint.min(n.saturating_sub(1)), finished: n == 0, final_heading_rad: h };
    }
    let r = sane_radius(turn_radius_m);
    let spd = if speed_mps.is_finite() { speed_mps.abs() } else { 0.0 };
    let look = r.max(spd * LOOKAHEAD_S).max(MIN_LOOKAHEAD_M);
    let fwd = heading_dir(h);
    // Course of the final leg (last non-degenerate segment), for holding after the route.
    let last_dir = (1..n).rev().map(|i| route[i] - route[i - 1]).find(|d| d.length_squared() > 1e-12).map(|d| d.normalize());
    let final_heading = last_dir.map(bearing).unwrap_or(h);
    let speed_factor = |target: DVec2| {
        let d = target - pos;
        let l = d.length();
        if !(l > 1e-9) { return 1.0; }
        let c = fwd.dot(d / l);
        if c >= 0.5 { 1.0 } else { 0.6 + 0.4 * (c + 1.0) / 1.5 }
    };
    let mut w = waypoint.min(n - 1);
    if w == 0 {
        let d = route[0] - pos;
        let dist = d.length();
        if n > 1 && dist < look {
            w = 1;
        } else {
            let passed = n == 1 && fwd.dot(d) <= 0.0 && dist < 2.0 * r;
            let finished = n == 1 && (dist < ARRIVE_M || passed);
            let hd = if dist > 1e-6 { bearing(d) } else { h };
            return Pursuit { heading_rad: if finished { h } else { hd }, speed_factor: speed_factor(route[0]), waypoint: 0, finished, final_heading_rad: if n == 1 { h } else { final_heading } };
        }
    }
    // Nearest segment from the current one forward, within three lookaheads of path length.
    let seg_d2 = |s: usize| {
        let t = proj_t(route[s - 1], route[s], pos).clamp(0.0, 1.0);
        route[s - 1].lerp(route[s], t).distance_squared(pos)
    };
    let mut best = w;
    let mut best_d2 = seg_d2(w);
    let mut acc = 0.0;
    for s in w + 1..n {
        acc += route[s - 1].distance(route[s - 2]);
        if acc > 3.0 * look { break; }
        let d2 = seg_d2(s);
        if d2 < best_d2 { best = s; best_d2 = d2; }
    }
    w = best;
    while w < n - 1 && proj_t(route[w - 1], route[w], pos) >= 1.0 { w += 1; }
    let t_raw = proj_t(route[w - 1], route[w], pos);
    let finished = w == n - 1 && t_raw >= 1.0;
    // Walk the lookahead distance along the route from the projection; past the end, along the last leg.
    let mut p = route[w - 1].lerp(route[w], t_raw.clamp(0.0, 1.0));
    let mut remaining = look;
    let mut seg = w;
    loop {
        let end = route[seg];
        let d = end.distance(p);
        if remaining <= d && d > 1e-12 { p += (end - p) * (remaining / d); break; }
        remaining -= d;
        p = end;
        if seg == n - 1 {
            if let Some(dir) = last_dir { p += dir * remaining; }
            break;
        }
        seg += 1;
    }
    let to = p - pos;
    let hd = if to.length_squared() > 1e-6 { bearing(to) } else { final_heading };
    Pursuit { heading_rad: hd, speed_factor: speed_factor(p), waypoint: w, finished, final_heading_rad: final_heading }
}

/// Layer 3: pure pursuit. Returns (ordered compass heading in radians, speed factor 0..=1, index of the
/// waypoint now being chased); the lookahead is at least one turning radius. Shared with the Helm officer.
/// Unit: `sys-ai-path`.
pub fn pure_pursuit(route: &[DVec2], pos: DVec2, heading_rad: f64, speed_mps: f64, turn_radius_m: f64, waypoint: usize) -> (f64, f64, usize) {
    let p = pursue(route, pos, heading_rad, speed_mps, turn_radius_m, waypoint);
    (p.heading_rad, p.speed_factor, p.waypoint)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: f64 = 50.0;

    fn grid(cols: u32, rows: u32, f: impl Fn(i64, i64) -> f32) -> DepthGridData {
        let mut depth = Vec::with_capacity((cols * rows) as usize);
        for y in 0..rows as i64 { for x in 0..cols as i64 { depth.push(f(x, y)); } }
        DepthGridData { cell_m: CELL, cols, rows, origin: [0.0, 0.0], depth }
    }
    fn centre(x: i64, y: i64) -> DVec2 { DVec2::new((x as f64 + 0.5) * CELL, (y as f64 + 0.5) * CELL) }
    fn mask_of(g: &DepthGridData, beam: f64, draft: f64) -> ClearanceMask { build_mask(&GridView::of_data(g).unwrap(), beam, draft) }
    fn req(from: DVec2, to: DVec2, to_heading: Option<f64>, r: f64) -> PathRequest {
        PathRequest { id: 0, requester: EntityId { index: 1, generation: 0 }, from, from_heading_rad: 0.0, to, to_heading_rad: to_heading, beam_m: 8.0, draft_m: 3.0, turn_radius_m: r, issued_tick: 0 }
    }
    /// Every cell a route's legs touch, excluding a final berth leg, must be open.
    fn route_clear(g: &DepthGridData, m: &ClearanceMask, w: &[DVec2]) -> bool {
        let v = GridView::of_data(g).unwrap();
        polyline_clear(&v, w, &|c| open(m, c))
    }
    /// 5 km x 5 km sea with an island in the middle and a walled harbour on the east side whose mouth faces
    /// north. Returns (grid, berth point inside the harbour).
    fn island_and_harbour() -> (DepthGridData, DVec2) {
        let g = grid(100, 100, |x, y| {
            let (dx, dy) = (x - 45, y - 50);
            if dx * dx + dy * dy <= 12 * 12 { return -5.0; }
            // Harbour walls: x 80..=94, y 60..=80, open at the north (y = 60) between x 84 and 90.
            let wall = (x == 80 || x == 94) && (60..=80).contains(&y) || y == 80 && (80..=94).contains(&x) || y == 60 && (80..=94).contains(&x) && !(84..=90).contains(&x);
            if wall { -2.0 } else if x > 80 && x < 94 && y > 72 { 6.0 } else { 40.0 }
        });
        (g, centre(87, 75))
    }

    #[test]
    fn clearance_mask_follows_beam_draft_and_shoals() {
        let g = grid(20, 20, |x, y| if (8..=11).contains(&x) && (8..=11).contains(&y) { 0.0 } else if x == 2 && y == 2 { 4.5 } else { 50.0 });
        let small = mask_of(&g, 8.0, 3.0);
        let v = GridView::of_data(&g).unwrap();
        assert!(small.blocked.get(v.idx(9, 9) as usize), "land");
        assert!(!small.blocked.get(v.idx(7, 9) as usize), "adjacent cell centre is 25 m from land, clearance 14 m");
        assert!(small.near_land.get(v.idx(7, 9) as usize));
        assert!(!small.near_land.get(v.idx(5, 9) as usize));
        assert!(small.blocked.get(v.idx(2, 2) as usize), "4.5 m of water is under draft + 2 m");
        let wide = mask_of(&g, 40.0, 3.0);
        assert!(wide.blocked.get(v.idx(7, 9) as usize), "clearance 30 m reaches the orthogonal neighbour");
        assert!(!wide.blocked.get(v.idx(7, 7) as usize), "diagonal neighbour is 35 m away");
        let shallow_ok = mask_of(&g, 8.0, 2.0);
        assert!(!shallow_ok.blocked.get(v.idx(2, 2) as usize));
        assert_eq!(mask_key(8.01, 3.0), (81, 30));
    }

    #[test]
    fn astar_open_water_is_straight_and_deterministic() {
        let g = grid(30, 30, |_, _| 40.0);
        let m = mask_of(&g, 8.0, 3.0);
        let (p, n) = astar_route(&g, &m, centre(2, 5), centre(12, 5));
        let p = p.unwrap();
        assert_eq!(p.len(), 11);
        let v = GridView::of_data(&g).unwrap();
        assert!(p.iter().all(|&c| v.xy(c).1 == 5));
        assert!(n >= 10 && n < 40, "octile heuristic keeps the search on the line, got {n}");
        let (p2, n2) = astar_route(&g, &m, centre(2, 5), centre(12, 5));
        assert_eq!((Some(p), n), (p2, n2));
        // Diagonal ties: of the equal-cost routes the search settles them by cell index, the same every run.
        let a = astar_route(&g, &m, centre(1, 1), centre(9, 4));
        let b = astar_route(&g, &m, centre(1, 1), centre(9, 4));
        assert_eq!(a, b);
        let cost: u64 = a.0.unwrap().windows(2).map(|s| { let (x0, y0) = v.xy(s[0]); let (x1, y1) = v.xy(s[1]); if x0 != x1 && y0 != y1 { 14 } else { 10 } }).sum();
        assert_eq!(cost, 8 * 10 + 3 * 4);
    }

    #[test]
    fn astar_detours_and_fails_fast_when_cut_off() {
        let wall = |gap: bool| grid(30, 30, move |x, y| if x == 15 && !(gap && y == 25) { -1.0 } else { 40.0 });
        let g = wall(true);
        let m = mask_of(&g, 8.0, 3.0);
        let v = GridView::of_data(&g).unwrap();
        let (p, _) = astar_route(&g, &m, centre(5, 5), centre(25, 5));
        let p = p.unwrap();
        assert!(p.iter().all(|&c| open(&m, c)));
        assert!(p.contains(&v.idx(15, 25)), "must use the gap");
        for s in p.windows(2) {
            let ((x0, y0), (x1, y1)) = (v.xy(s[0]), v.xy(s[1]));
            if x0 != x1 && y0 != y1 { assert!(open(&m, v.idx(x1, y0)) && open(&m, v.idx(x0, y1)), "no corner cutting"); }
        }
        let g = wall(false);
        let m = mask_of(&g, 8.0, 3.0);
        assert_eq!(astar_route(&g, &m, centre(5, 5), centre(25, 5)), (None, 0));
        // A goal on land snaps to the nearest open cell on the start's side.
        let (p, _) = astar_route(&g, &m, centre(5, 5), centre(15, 5));
        assert_eq!(*p.unwrap().last().unwrap(), GridView::of_data(&g).unwrap().idx(14, 5));
    }

    #[test]
    fn dubins_frame_matches_compass() {
        // North is -z: a straight Dubins leg from heading 0 runs up the -z axis.
        let p = DubinsPath::shortest_from(posrot_heading(DVec2::ZERO, 0.0), posrot_heading(DVec2::new(0.0, -1000.0), 0.0), 100.0).unwrap();
        let q = p.sample(500.0);
        assert!(q.x().abs() < 1e-6 && (q.y() + 500.0).abs() < 1e-6);
        let p = DubinsPath::shortest_from(posrot_heading(DVec2::ZERO, FRAC_PI_2), posrot_heading(DVec2::new(1000.0, 0.0), FRAC_PI_2), 100.0).unwrap();
        let q = p.sample(500.0);
        assert!((q.x() - 500.0).abs() < 1e-6 && q.y().abs() < 1e-6);
        assert!(bearing(DVec2::new(0.0, -1.0)).abs() < 1e-12);
        assert!((bearing(DVec2::new(1.0, 0.0)) - FRAC_PI_2).abs() < 1e-12);
        assert!((bearing(DVec2::new(0.0, 1.0)) - PI).abs() < 1e-12);
    }

    #[test]
    fn smoothing_open_water_and_island() {
        let (g, _) = island_and_harbour();
        let m = mask_of(&g, 8.0, 3.0);
        let r = req(centre(5, 5), centre(20, 8), None, 150.0);
        let (cells, _) = astar_route(&g, &m, r.from, r.to);
        let route = smooth(&g, &m, &cells.unwrap(), &r);
        assert!(route.valid);
        assert_eq!(route.waypoints, vec![r.from, r.to]);
        // Across the island: every leg stays in open water and the turns are filleted.
        let r = req(centre(20, 50), centre(70, 52), None, 150.0);
        let (cells, _) = astar_route(&g, &m, r.from, r.to);
        let route = smooth(&g, &m, &cells.unwrap(), &r);
        assert!(route.valid && route.waypoints.len() >= 3);
        assert!(route_clear(&g, &m, &route.waypoints));
        assert_eq!(route.waypoints[0], r.from);
        assert_eq!(*route.waypoints.last().unwrap(), r.to);
        let straight = r.from.distance(r.to);
        assert!(route.length_m > straight && route.length_m < straight * 1.6);
    }

    #[test]
    fn final_approach_reaches_the_berth_pose() {
        let (g, berth) = island_and_harbour();
        let mut p = PathPlanner::with_grid(g.clone());
        let r = req(centre(5, 95), berth, Some(PI), 120.0);
        p.request(r.clone());
        let mut route = None;
        for t in 0..10_000 { p.step(2_000, t); if let Some(x) = p.take(r.requester) { route = Some(x); break; } }
        let route = route.expect("route released");
        assert!(route.valid);
        let w = &route.waypoints;
        assert_eq!(*w.last().unwrap(), berth);
        let last = w[w.len() - 1] - w[w.len() - 2];
        assert!(wrap_pi(bearing(last) - PI).abs() < 0.3, "arrives heading south");
        let m = p.grid.masks.values().next().unwrap();
        assert!(route_clear(&g, m, w));
        // Bitwise identical on a second planner.
        let mut p2 = PathPlanner::with_grid(g);
        p2.request(r.clone());
        let mut again = None;
        for t in 0..10_000 { p2.step(2_000, t); if let Some(x) = p2.take(r.requester) { again = Some(x); break; } }
        let again = again.unwrap();
        assert_eq!(route.waypoints.len(), again.waypoints.len());
        assert!(route.waypoints.iter().zip(&again.waypoints).all(|(a, b)| a.x.to_bits() == b.x.to_bits() && a.y.to_bits() == b.y.to_bits()));
    }

    #[test]
    fn planner_charges_debt_fifo_and_replaces_in_place() {
        let g = grid(200, 200, |x, y| if x == 100 && y > 2 { -1.0 } else { 40.0 });
        let mut p = PathPlanner::with_grid(g.clone());
        let a = EntityId { index: 1, generation: 0 };
        let b = EntityId { index: 2, generation: 0 };
        let mut ra = req(centre(90, 190), centre(110, 190), None, 100.0);
        ra.requester = a;
        let mut rb = req(centre(5, 5), centre(6, 5), None, 100.0);
        rb.requester = b;
        let id_a = p.request(ra.clone());
        let id_b = p.request(rb.clone());
        assert_ne!(id_a, id_b);
        // A newer request from `a` keeps its place at the head.
        ra.to = centre(111, 190);
        let id_a2 = p.request(ra.clone());
        assert_eq!(p.queue[0].requester, a);
        assert_eq!(p.queue[0].id, id_a2);
        assert_eq!(p.queue.len(), 2);
        let m = mask_of(&g, 8.0, 3.0);
        let (_, cost) = astar_route(&g, &m, ra.from, ra.to);
        assert!(cost > 4_000, "the detour around the wall is a big search ({cost})");
        let mut ticks = 0;
        loop {
            let spent = p.step(2_000, ticks);
            ticks += 1;
            if p.take(a).is_some() { assert!(spent <= 2_000); break; }
            assert_eq!(spent, 2_000);
            assert!(p.take(b).is_none(), "FIFO: b waits behind a");
        }
        assert_eq!(ticks, cost.div_ceil(2_000) as u64);
        p.step(2_000, ticks);
        assert!(p.take(b).unwrap().valid);
        // Empty planner and default planner never panic.
        let mut d = PathPlanner::default();
        d.request(rb.clone());
        d.step(2_000, 0);
        let r = d.take(b).unwrap();
        assert!(r.valid && r.waypoints.len() == 2);
        assert_eq!(d.step(0, 1), 0);
    }

    #[test]
    fn berth_leg_only_through_water() {
        // A one-cell land strip across the map at x = 15; a 40 m beam blocks the cells beside it.
        let g = grid(30, 30, |x, _| if x == 15 { -1.0 } else { 30.0 });
        let m = mask_of(&g, 40.0, 3.0);
        let v = GridView::of_data(&g).unwrap();
        assert!(!open(&m, v.idx(14, 10)) && !open(&m, v.idx(16, 10)));
        // Berth beside the strip on the near side: deep water, inside the clearance zone, kept as a final leg.
        let mut r = req(centre(5, 10), DVec2::new(14.5 * CELL, 10.5 * CELL), None, 100.0);
        r.beam_m = 40.0;
        let (cells, _) = astar_route(&g, &m, r.from, r.to);
        let route = smooth(&g, &m, &cells.unwrap(), &r);
        assert!(route.valid && *route.waypoints.last().unwrap() == r.to);
        // The same spot on the far side: the snapped cell is on the near side and the leg would cross land.
        r.to = DVec2::new(16.5 * CELL, 10.5 * CELL);
        let (cells, _) = astar_route(&g, &m, r.from, r.to);
        let route = smooth(&g, &m, &cells.unwrap(), &r);
        assert!(route.valid && route.waypoints.iter().all(|p| p.x < 15.0 * CELL), "never crosses the strip");
    }

    #[test]
    fn unchanged_rerequest_does_not_starve() {
        let g = grid(200, 200, |x, y| if x == 100 && y > 2 { -1.0 } else { 40.0 });
        let mut p = PathPlanner::with_grid(g);
        let mut r = req(centre(90, 190), centre(110, 190), None, 100.0);
        p.request(r.clone());
        let mut got = None;
        let mut ids = BTreeSet::new();
        for t in 0..100 {
            // The caller re-requests from its new position every tick; the goal is unchanged.
            r.from.y -= 1.0;
            ids.insert(p.request(r.clone()));
            p.step(2_000, t);
            if let Some(route) = p.take(r.requester) { got = Some(route); break; }
        }
        assert!(got.unwrap().valid);
        assert_eq!(ids.len(), 1, "once searched, unchanged re-requests return the in-flight id");
        // A changed goal does replace a pending result.
        let mut p2 = PathPlanner::with_grid(grid(200, 200, |x, y| if x == 100 && y > 2 { -1.0 } else { 40.0 }));
        let a = p2.request(r.clone());
        p2.step(2_000, 0);
        r.to = centre(120, 190);
        let b = p2.request(r.clone());
        assert_ne!(a, b);
        assert!(p2.pending.is_none() && p2.queue.len() == 1);
    }

    /// Full-size map timing (100 km at 50 m): run with `--ignored --nocapture`.
    #[test]
    #[ignore]
    fn full_size_map_timing() {
        let g = grid(2000, 2000, |x, y| { let (dx, dy) = (x - 1000, y - 1000); if dx * dx + dy * dy < 300 * 300 { -10.0 } else { 60.0 } });
        let t0 = std::time::Instant::now();
        let m = mask_of(&g, 12.0, 4.0);
        let t1 = std::time::Instant::now();
        let r = req(centre(1000, 50), centre(1000, 1950), Some(PI), 300.0);
        let (cells, n) = astar_route(&g, &m, r.from, r.to);
        let t2 = std::time::Instant::now();
        let route = smooth(&g, &m, &cells.unwrap(), &r);
        let t3 = std::time::Instant::now();
        eprintln!("mask {:?}, astar {:?} ({n} expansions), smooth {:?}, {} waypoints", t1 - t0, t2 - t1, t3 - t2, route.waypoints.len());
        assert!(route.valid && route_clear(&g, &m, &route.waypoints[..route.waypoints.len()]));
    }

    #[test]
    fn pure_pursuit_tracks_and_finishes() {
        let route = vec![DVec2::new(0.0, 0.0), DVec2::new(2000.0, 0.0)];
        // On the line heading east.
        let (h, f, w) = pure_pursuit(&route, DVec2::new(100.0, 0.0), FRAC_PI_2, 10.0, 200.0, 0);
        assert!((h - FRAC_PI_2).abs() < 1e-9 && f == 1.0 && w == 1);
        // 100 m north of the line (z = -100): steer back with the lookahead at one turning radius along it.
        let (h, _, _) = pure_pursuit(&route, DVec2::new(100.0, -100.0), FRAC_PI_2, 10.0, 200.0, 1);
        assert!((h - bearing(DVec2::new(200.0, 100.0))).abs() < 1e-9);
        // Index never goes back; past the end the ship holds the last leg's course.
        let p = pursue(&route, DVec2::new(2100.0, 30.0), FRAC_PI_2, 10.0, 200.0, 1);
        assert!(p.finished && p.waypoint == 1 && (p.final_heading_rad - FRAC_PI_2).abs() < 1e-9);
        let (_, _, w) = pure_pursuit(&route, DVec2::new(0.0, 0.0), FRAC_PI_2, 10.0, 200.0, 1);
        assert_eq!(w, 1);
        // Degenerate input.
        assert_eq!(pure_pursuit(&[], DVec2::ZERO, 1.0, 5.0, 0.0, 9), (1.0, 0.0, 0));
        let (h, _, w) = pure_pursuit(&route, DVec2::new(f64::NAN, 0.0), 1.0, 5.0, 0.0, 9);
        assert!(h == 1.0 && w == 1);
        // A point behind the beam inside two turning radii counts as reached, so the ship cannot orbit it.
        let one = [DVec2::new(100.0, 50.0)];
        assert!(pursue(&one, DVec2::new(150.0, 0.0), FRAC_PI_2, 10.0, 200.0, 0).finished);
        assert!(!pursue(&one, DVec2::new(-500.0, 0.0), FRAC_PI_2, 10.0, 200.0, 0).finished);
        // Sharp heading error slows the ship.
        let (_, f, _) = pure_pursuit(&route, DVec2::new(100.0, 0.0), 3.0 * FRAC_PI_2, 10.0, 200.0, 1);
        assert!(f < 1.0 && f >= 0.6);
    }
}
