//! Unit `sim-core-compartments`. Compartments derived by flood fill over air cells bounded by watertight cells,
//! their adjacency graph and their openings. DESIGN.md L132 (decks and bulkheads cut compartments), L626
//! (openings), L684 (recompute after damage; merged compartments sum water), L285 (router uses adjacency).
//!
//! Model (implementation notes, binding for the units that read this set):
//! - A cell is a barrier when it is live and watertight (structure, or a watertight part such as a tank,
//!   magazine or door). Every other cell is passable: empty cells, destroyed cells and the cells of open
//!   equipment. The flood fill from outside the box marks the exterior; each remaining passable component is
//!   one compartment, numbered by its lowest cell index.
//! - `Compartment::cells` holds only the empty (air) cells, so equipment displaces water and `volume_m3` is the
//!   floodable volume. `cell_to_compartment` also maps the passable equipment cells of a compartment.
//! - A part is inside the compartment holding most of its passable cells. A part made of barrier cells (tank,
//!   magazine) sits in the compartment its cells touch most. Exterior parts (deck guns, masts) are in none.
//! - Adjacency is found across barrier walls one or two cells thick, so armoured double plating still joins
//!   neighbours. `shared_faces` counts wall crossings; `vertical` is true when most crossings run along Y.
//! - Openings: `cell` is the cell the water passes and `face` is the face of that cell that looks into `a`.
//!   A flood valve sits on the bottom face (`YNeg`) of the lowest air cell of its compartment.
//! - After damage ([`CompartmentSet::update_after_damage`]) a removed cell that touches a compartment and the
//!   sea becomes an opening cell ([`OPEN_CELL`]) with one 0.25 m^2 breach per face it shares with that
//!   compartment. A removed cell touching two or more compartments merges them into the lowest id and becomes
//!   air of the merged compartment, which sums water (L684). The absorbed compartment keeps its index as an
//!   empty shell (no cells, `max < min`) so ids held elsewhere stay valid; its parts and openings move over.
//!   A removed cell walled in on every side waits as [`VOID_CELL`] until a neighbour opens.
//! - The set never needs a waterline to be valid. [`CompartmentSet::set_waterline`] adds the flood valves and
//!   `below_waterline` flags once the prefab builder knows the design waterline.
use crate::bits::BitSet;
use crate::grid::VesselGrid;
use crate::math::DVec3;
use crate::parts::{PartRuntime, PartState};
use content::cell::{CellPos, Face, CELL_FACE_M2, CELL_M, CELL_VOLUME_M3};
use content::ids::{CellIndex, CompartmentId, PartSlot};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod testkit;

/// `cell_to_compartment` value of a solid or outside cell. Every value at or above [`MAX_COMPARTMENTS`] is a
/// label, not an id: read the map through [`CompartmentSet::of_cell`] or `compartments.get`, never index with it.
pub const NO_COMPARTMENT: u16 = u16::MAX;
/// `cell_to_compartment` value of a removed cell that opens to the sea (set by `update_after_damage`, and by
/// `derive` for removed cells outside the hull envelope).
pub const OPEN_CELL: u16 = u16::MAX - 1;
/// `cell_to_compartment` value of a removed cell enclosed by intact walls, re-tested when a neighbour opens.
pub const VOID_CELL: u16 = u16::MAX - 2;
/// Compartment ids are below this; components past the limit stay unassigned (deterministically the highest
/// lowest-cell-index ones).
pub const MAX_COMPARTMENTS: usize = VOID_CELL as usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpeningKind {
    /// Destroyed outer cell face: sea to compartment, 0.25 m^2 each (L626).
    Breach,
    /// Destroyed bulkhead or deck cell between two compartments.
    Hole,
    /// Door or hatch part; passes water only while open.
    Door(PartSlot),
    /// 0.1 m^2 flood valve of a below-waterline compartment (L648).
    FloodValve,
}

/// One opening. `b == None` means the sea.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Opening {
    pub a: CompartmentId,
    pub b: Option<CompartmentId>,
    pub cell: CellIndex,
    pub face: Face,
    pub area_m2: f64,
    pub kind: OpeningKind,
    /// Patched by a damage-control team; a patch has 1 bucket of health and any further damage to the cell
    /// reopens it (L642-646).
    pub patched: bool,
    pub open: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Compartment {
    pub id: CompartmentId,
    /// Air cells of the compartment in ascending index.
    pub cells: Vec<CellIndex>,
    pub volume_m3: f64,
    /// Bounding box in cells (min, max inclusive).
    pub min: [i16; 3],
    pub max: [i16; 3],
    /// Geometric centroid in vessel-local metres.
    pub centroid: DVec3,
    /// Water volume now, m^3.
    pub water_m3: f64,
    /// World-space height of the level water plane, from 16 bisection steps over the cells at the current heel
    /// and trim (L634). Meaningful while `water_m3 > 0`.
    pub water_plane_y: f64,
    /// Centroid of the filled volume in vessel-local metres.
    pub water_centroid: DVec3,
    /// Free-surface area in m^2 at the current level (for the levelling clamp, L630).
    pub free_surface_m2: f64,
    /// Fire intensity 0..=100; 0 = not burning (L664).
    pub fire: f64,
    /// Ticks the fire has burned, for cook-off timing.
    pub fire_ticks: u32,
    /// Parts inside, ascending slot.
    pub parts: Vec<PartSlot>,
    /// Magazine part in this compartment, if any.
    pub magazine: Option<PartSlot>,
    pub berths: u16,
    pub below_waterline: bool,
    pub flood_valve_open: bool,
    /// False once breached or entered; drives the sealed-air render class (L1925).
    pub sealed: bool,
}
impl Compartment {
    pub fn fill_fraction(&self) -> f64 { if self.volume_m3 > 0.0 { (self.water_m3 / self.volume_m3).clamp(0.0, 1.0) } else { 0.0 } }
    pub fn is_burning(&self) -> bool { self.fire > 0.0 }
    /// True for a compartment absorbed by a merge: it keeps its index but has no cells and an empty box.
    pub fn is_absorbed(&self) -> bool { self.max[0] < self.min[0] }
}

/// Adjacency between two compartments across a deck or bulkhead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adjacency { pub a: CompartmentId, pub b: CompartmentId, pub shared_faces: u32, pub vertical: bool, pub has_door: bool }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompartmentSet {
    /// Ascending id; processing order everywhere (L630).
    pub compartments: Vec<Compartment>,
    /// Compartment of each cell by [`CellIndex`], [`NO_COMPARTMENT`] for solid or outside cells. Removed cells
    /// processed after damage hold [`OPEN_CELL`] or [`VOID_CELL`]; use [`CompartmentSet::of_cell`] to read it.
    pub cell_to_compartment: Vec<u16>,
    /// Ascending (a, b).
    pub adjacency: Vec<Adjacency>,
    /// Ascending (a, cell, face).
    pub openings: Vec<Opening>,
    /// Y cell coordinate of the top deck, for the sunk test: lost when no top-deck cell is above water (L537).
    pub top_deck_y: i16,
}

// ---------------------------------------------------------------------------------------------------------
// Grid helpers shared with the networks router.

/// Dense box dimensions with the `CellIndex` layout `x + sx * (z + sz * y)`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Dims { pub sx: usize, pub sy: usize, pub sz: usize }
impl Dims {
    pub fn new(size: [u16; 3]) -> Self { Self { sx: size[0] as usize, sy: size[1] as usize, sz: size[2] as usize } }
    pub fn len(&self) -> usize { self.sx * self.sy * self.sz }
    #[inline]
    pub fn xyz(&self, i: usize) -> [usize; 3] { [i % self.sx, i / (self.sx * self.sz), (i / self.sx) % self.sz] }
    #[inline]
    pub fn idx(&self, x: usize, y: usize, z: usize) -> usize { x + self.sx * (z + self.sz * y) }
    pub fn pos(&self, i: usize) -> CellPos { let [x, y, z] = self.xyz(i); CellPos::new(x as i16, y as i16, z as i16) }
    /// Face neighbour index, `None` outside the box.
    #[inline]
    pub fn step(&self, i: usize, f: Face) -> Option<usize> {
        let [x, y, z] = self.xyz(i);
        match f {
            Face::XNeg => (x > 0).then(|| i - 1),
            Face::XPos => (x + 1 < self.sx).then(|| i + 1),
            Face::YNeg => (y > 0).then(|| i - self.sx * self.sz),
            Face::YPos => (y + 1 < self.sy).then(|| i + self.sx * self.sz),
            Face::ZNeg => (z > 0).then(|| i - self.sx),
            Face::ZPos => (z + 1 < self.sz).then(|| i + self.sx),
        }
    }
    pub fn centre(&self, i: usize) -> DVec3 { let [x, y, z] = self.xyz(i); DVec3::new((x as f64 + 0.5) * CELL_M, (y as f64 + 0.5) * CELL_M, (z as f64 + 0.5) * CELL_M) }
}

pub(crate) fn opposite(f: Face) -> Face {
    match f { Face::XNeg => Face::XPos, Face::XPos => Face::XNeg, Face::YNeg => Face::YPos, Face::YPos => Face::YNeg, Face::ZNeg => Face::ZPos, Face::ZPos => Face::ZNeg }
}
fn face_key(f: Face) -> u8 { f as u8 }

/// Cell classes for the flood fill.
pub(crate) const AIR: u8 = 0;
pub(crate) const OPEN_SOLID: u8 = 1;
pub(crate) const BARRIER: u8 = 2;

/// Classify every cell: empty or removed (air), live but not watertight (open equipment), live watertight.
/// Empty when the grid's arrays do not match its size.
pub(crate) fn classify(grid: &VesselGrid) -> Vec<u8> {
    let d = Dims::new(grid.size());
    let n = d.len();
    if n == 0 || grid.design.cells.len() != n || grid.damage.len() != n { return Vec::new(); }
    let mut cls = vec![AIR; n];
    for (i, c) in cls.iter_mut().enumerate() {
        let p = d.pos(i);
        if grid.is_watertight(p) { *c = BARRIER } else if grid.is_solid(p) { *c = OPEN_SOLID }
    }
    cls
}

/// One crossing of a barrier wall between two compartments: the wall cells in order from `a` to `b`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Crossing { pub a: u16, pub b: u16, pub wall: Vec<u32>, pub axis: u8 }

/// Every wall crossing (walls one or two barrier cells thick) between different compartments, in ascending
/// (lower cell index, axis) scan order. `a` is the compartment on the lower-coordinate side.
pub(crate) fn wall_crossings(d: Dims, cls: &[u8], c2c: &[u16]) -> Vec<Crossing> {
    let mut out = Vec::new();
    let n = d.len();
    if cls.len() != n || c2c.len() != n { return out; }
    let comp = |i: usize| -> Option<u16> { let c = c2c[i]; (c < MAX_COMPARTMENTS as u16).then_some(c) };
    for i in 0..n {
        let Some(ca) = comp(i) else { continue };
        for (axis, f) in [(0u8, Face::XPos), (1, Face::YPos), (2, Face::ZPos)] {
            let Some(w1) = d.step(i, f) else { continue };
            if cls[w1] != BARRIER { continue; }
            let Some(n2) = d.step(w1, f) else { continue };
            if let Some(cb) = comp(n2) {
                if cb != ca { out.push(Crossing { a: ca, b: cb, wall: vec![w1 as u32], axis }); }
                continue;
            }
            if cls[n2] != BARRIER { continue; }
            let Some(n3) = d.step(n2, f) else { continue };
            if let Some(cb) = comp(n3) { if cb != ca { out.push(Crossing { a: ca, b: cb, wall: vec![w1 as u32, n2 as u32], axis }); } }
        }
    }
    out
}

fn is_comp(c: u16) -> bool { (c as usize) < MAX_COMPARTMENTS }

fn opening_order(o: &Opening) -> (u16, u32, u8, u32, u8) {
    let kind = match o.kind { OpeningKind::Breach => 0, OpeningKind::Hole => 1, OpeningKind::Door(_) => 2, OpeningKind::FloodValve => 3 };
    (o.a.0, o.cell.0, face_key(o.face), o.b.map_or(u32::MAX, |b| b.0 as u32), kind)
}

impl CompartmentSet {
    /// Derive everything from an intact or damaged grid. Flood fill from outside the bounding box marks exterior
    /// air; remaining air components become compartments numbered by lowest cell index. Counts cells visited.
    /// Unit: `sim-core-compartments`.
    ///
    /// Removed cells are passable here (a damaged grid derived from scratch opens its breached compartments to
    /// the sea); removed cells outside every compartment are marked [`OPEN_CELL`]. To keep breached
    /// compartments as compartments that flood through their breaches (persistent damage at spawn), derive on
    /// the intact grid and pass the removed cells to [`CompartmentSet::update_after_damage`], as the tick does. Doors become openings; flood
    /// valves wait for [`CompartmentSet::set_waterline`]. Every compartment starts dry, cold and sealed, with
    /// `free_surface_m2` the area of its lowest air layer. Part containment is listed in `Compartment::parts`;
    /// [`CompartmentSet::link_parts`] copies it onto the parts.
    pub fn derive(grid: &VesselGrid, parts: &[PartRuntime]) -> (CompartmentSet, u32) {
        let d = Dims::new(grid.size());
        let n = d.len();
        let mut set = CompartmentSet { compartments: Vec::new(), cell_to_compartment: vec![NO_COMPARTMENT; n], adjacency: Vec::new(), openings: Vec::new(), top_deck_y: 0 };
        let cls = classify(grid);
        if cls.is_empty() { return (set, 0); }
        let mut visited: u32 = 0;
        // Exterior flood from every passable cell on the box surface.
        let mut ext = vec![false; n];
        let mut queue: Vec<u32> = Vec::new();
        for i in 0..n {
            if cls[i] == BARRIER { continue; }
            let [x, y, z] = d.xyz(i);
            if x == 0 || y == 0 || z == 0 || x + 1 == d.sx || y + 1 == d.sy || z + 1 == d.sz { ext[i] = true; queue.push(i as u32); }
        }
        while let Some(i) = queue.pop() {
            visited = visited.saturating_add(1);
            for f in Face::ALL {
                if let Some(j) = d.step(i as usize, f) { if !ext[j] && cls[j] != BARRIER { ext[j] = true; queue.push(j as u32); } }
            }
        }
        // Interior components in ascending lowest cell index.
        let mut comp_cells: Vec<Vec<u32>> = Vec::new();
        let mut seen = ext.clone();
        for i in 0..n {
            if seen[i] || cls[i] == BARRIER { continue; }
            let id = comp_cells.len();
            let assign = id < MAX_COMPARTMENTS;
            let mut cells = Vec::new();
            seen[i] = true;
            queue.push(i as u32);
            while let Some(c) = queue.pop() {
                visited = visited.saturating_add(1);
                cells.push(c);
                for f in Face::ALL {
                    if let Some(j) = d.step(c as usize, f) { if !seen[j] && cls[j] != BARRIER { seen[j] = true; queue.push(j as u32); } }
                }
            }
            if assign {
                for &c in &cells { set.cell_to_compartment[c as usize] = id as u16; }
                comp_cells.push(cells);
            }
        }
        // Removed design cells in the exterior are open to the sea.
        for i in 0..n {
            if ext[i] && cls[i] == AIR && !grid.design.cells[i].is_empty() { set.cell_to_compartment[i] = OPEN_CELL; }
        }
        for (id, mut cells) in comp_cells.into_iter().enumerate() {
            cells.sort_unstable();
            set.compartments.push(build_compartment(id as u16, &cells, &cls, d));
        }
        // Parts.
        for p in parts {
            let Some(c) = containment(p, &set.cell_to_compartment, &cls, d) else { continue };
            set.compartments[c as usize].parts.push(p.slot);
        }
        for c in &mut set.compartments {
            c.parts.sort_unstable();
            c.parts.dedup();
            c.magazine = c.parts.iter().copied().find(|s| find_part(parts, *s).is_some_and(|p| matches!(p.state, PartState::Magazine { .. })));
        }
        // Adjacency across walls.
        let mut adj: BTreeMap<(u16, u16), (u32, u32)> = BTreeMap::new();
        for x in wall_crossings(d, &cls, &set.cell_to_compartment) {
            let e = adj.entry((x.a.min(x.b), x.a.max(x.b))).or_insert((0, 0));
            e.0 = e.0.saturating_add(1);
            if x.axis == 1 { e.1 = e.1.saturating_add(1); }
        }
        visited = visited.saturating_add(n as u32);
        // Doors.
        for p in parts {
            let PartState::Door { closed } = p.state else { continue };
            if let Some((a, b, cell, face, count)) = door_link(p, &set.cell_to_compartment, &cls, &ext, d) {
                set.openings.push(Opening { a: CompartmentId(a), b: b.map(CompartmentId), cell: CellIndex(cell), face, area_m2: count as f64 * CELL_FACE_M2, kind: OpeningKind::Door(p.slot), patched: false, open: !closed });
                if let Some(b) = b {
                    let e = adj.entry((a.min(b), a.max(b))).or_insert((0, 0));
                    if e.0 == 0 { e.0 = count; }
                }
            }
        }
        set.adjacency = adj.into_iter().map(|((a, b), (faces, vert))| Adjacency { a: CompartmentId(a), b: CompartmentId(b), shared_faces: faces, vertical: vert * 2 > faces, has_door: false }).collect();
        set.refresh_door_flags();
        set.sort_openings();
        set.top_deck_y = top_deck(&cls, &set.cell_to_compartment, d);
        (set, visited)
    }

    /// Incremental update after cells were destroyed: add breaches and holes, merge compartments joined by a
    /// destroyed boundary (summing water), within `budget` cells visited; returns cells visited and whether work
    /// remains. Also refreshes `grid.sealed_air`. Unit: `sim-core-compartments`.
    ///
    /// Cells are taken in ascending index. Passing a cell again is cheap and changes nothing, so a caller that
    /// ran out of budget passes the same list (or a superset) next tick; at least one unprocessed cell is always
    /// handled, so repeated calls finish. Patched openings at or next to a newly removed cell reopen.
    /// A single merge or enclosed-void re-test finishes once started, so one call may run past `budget` by the
    /// size of the absorbed compartment. Breaches the damage unit already added are not duplicated, and holes
    /// between compartments that merge are dropped.
    pub fn update_after_damage(&mut self, grid: &mut VesselGrid, destroyed: &[CellIndex], budget: u32) -> (u32, bool) {
        let d = Dims::new(grid.size());
        let n = d.len();
        if n == 0 || self.cell_to_compartment.len() != n || grid.design.cells.len() != n || grid.damage.len() != n { return (0, false); }
        let words = n.div_ceil(64);
        let mut touched: BTreeSet<u16> = BTreeSet::new();
        if grid.sealed_air.len() != words {
            grid.sealed_air = self.sealed_air_bits(n).words;
            grid.sealed_air.resize(words, 0);
        }
        let mut list: Vec<u32> = destroyed.iter().map(|c| c.0).filter(|&c| (c as usize) < n).collect();
        list.sort_unstable();
        list.dedup();
        let mut keys: BTreeSet<(u16, u32, u8, u32, u8)> = self.openings.iter().map(opening_order).collect();
        let mut ctx = Upd { d, visited: 0, processed: BTreeSet::new(), voids: Vec::new() };
        let mut remains = false;
        let mut did_work = false;
        for (k, &x) in list.iter().enumerate() {
            if did_work && ctx.visited >= budget { remains = list[k..].iter().any(|&c| !self.is_processed(grid, d, c as usize)); break; }
            ctx.visited = ctx.visited.saturating_add(1);
            if self.is_processed(grid, d, x as usize) { continue; }
            did_work = true;
            self.process_cell(grid, &mut ctx, &mut keys, &mut touched, x as usize);
            while let Some(v) = ctx.voids.pop() {
                if self.cell_to_compartment[v] == VOID_CELL { self.process_cell(grid, &mut ctx, &mut keys, &mut touched, v); }
            }
        }
        if did_work {
            // Reopen patches at or next to the cells removed this call.
            for o in &mut self.openings {
                if !o.patched { continue; }
                let c = o.cell.0 as usize;
                let near = ctx.processed.contains(&o.cell.0) || (c < n && Face::ALL.iter().any(|&f| d.step(c, f).is_some_and(|j| ctx.processed.contains(&(j as u32)))));
                if near { o.patched = false; }
            }
            self.sort_openings();
            for id in touched {
                let Some(c) = self.compartments.get(id as usize) else { continue };
                let sealed = c.sealed;
                for &cell in &c.cells {
                    let i = cell.index();
                    if i < n { let (w, b) = (i / 64, i % 64); if sealed { grid.sealed_air[w] |= 1 << b } else { grid.sealed_air[w] &= !(1 << b) } }
                }
            }
        }
        (ctx.visited, remains)
    }

    pub fn get(&self, id: CompartmentId) -> Option<&Compartment> { self.compartments.get(id.index()) }
    pub fn get_mut(&mut self, id: CompartmentId) -> Option<&mut Compartment> { self.compartments.get_mut(id.index()) }
    pub fn of_cell(&self, cell: CellIndex) -> Option<CompartmentId> { match self.cell_to_compartment.get(cell.index()) { Some(&c) if is_comp(c) => Some(CompartmentId(c)), _ => None } }
    /// Compartments adjacent to `id`, ascending. Unit: `sim-core-compartments`.
    pub fn neighbours(&self, id: CompartmentId) -> Vec<CompartmentId> {
        let mut out: Vec<CompartmentId> = self.adjacency.iter().filter_map(|a| if a.a == id && a.b != id { Some(a.b) } else if a.b == id && a.a != id { Some(a.a) } else { None }).collect();
        out.sort_unstable();
        out.dedup();
        out
    }
    /// Bitset of air cells that are sealed (see `VesselGrid::sealed_air`). Unit: `sim-core-compartments`.
    pub fn sealed_air_bits(&self, cell_count: usize) -> BitSet {
        let mut bits = BitSet::new(cell_count);
        for c in self.compartments.iter().filter(|c| c.sealed) {
            for &cell in &c.cells { if cell.index() < cell_count { bits.set(cell.index(), true); } }
        }
        bits
    }

    /// Set `below_waterline` from the design waterline (vessel-local metres) and give every compartment whose
    /// lowest air cell lies below it a flood valve of `valve_m2` (L648), replacing any earlier valves. Called by
    /// the prefab builder once the waterline is known; a set without it simply has no valves.
    pub fn set_waterline(&mut self, waterline_y: f64, valve_m2: f64) {
        self.openings.retain(|o| o.kind != OpeningKind::FloodValve);
        let area = if valve_m2.is_finite() && valve_m2 > 0.0 { valve_m2 } else { 0.0 };
        let mut valves = Vec::new();
        for c in &mut self.compartments {
            let Some(&lowest) = c.cells.first() else { c.below_waterline = false; continue };
            let bottom = (c.min[1] as f64 + 0.5) * CELL_M;
            c.below_waterline = waterline_y.is_finite() && bottom < waterline_y;
            if c.below_waterline && area > 0.0 {
                valves.push(Opening { a: c.id, b: None, cell: lowest, face: Face::YNeg, area_m2: area, kind: OpeningKind::FloodValve, patched: false, open: c.flood_valve_open });
            }
        }
        self.openings.extend(valves);
        self.sort_openings();
    }

    /// Write each part's `compartment` from the containment lists (`None` for parts no compartment holds).
    pub fn link_parts(&self, parts: &mut [PartRuntime]) {
        for p in parts.iter_mut() { p.compartment = None; }
        self.sync_part_compartments(parts);
    }

    /// Point every part listed in a compartment at that compartment (after merges moved them); parts not
    /// listed anywhere are left as they are.
    pub(crate) fn sync_part_compartments(&self, parts: &mut [PartRuntime]) {
        for c in &self.compartments {
            for &s in &c.parts {
                let i = if parts.get(s.index()).is_some_and(|p| p.slot == s) { Some(s.index()) } else { parts.iter().position(|p| p.slot == s) };
                if let Some(i) = i { parts[i].compartment = Some(c.id); }
            }
        }
    }

    /// Copy door part state onto door openings and the adjacency door flags.
    pub(crate) fn sync_doors(&mut self, parts: &[PartRuntime]) {
        for o in &mut self.openings {
            if let OpeningKind::Door(s) = o.kind {
                if let Some(p) = find_part(parts, s) {
                    o.open = match p.state { PartState::Door { closed } => !closed && p.works(), _ => false };
                }
            }
        }
    }

    /// True when an open door joins `a` and `b`.
    pub(crate) fn open_door_between(&self, a: CompartmentId, b: CompartmentId) -> bool {
        self.openings.iter().any(|o| matches!(o.kind, OpeningKind::Door(_)) && o.open && ((o.a == a && o.b == Some(b)) || (o.a == b && o.b == Some(a))))
    }

    fn refresh_door_flags(&mut self) {
        let doors: BTreeSet<(u16, u16)> = self.openings.iter().filter(|o| matches!(o.kind, OpeningKind::Door(_))).filter_map(|o| o.b.map(|b| (o.a.0.min(b.0), o.a.0.max(b.0)))).collect();
        for a in &mut self.adjacency { a.has_door = doors.contains(&(a.a.0, a.b.0)); }
    }

    fn sort_openings(&mut self) { self.openings.sort_by_key(opening_order); }

    fn is_processed(&self, grid: &VesselGrid, d: Dims, i: usize) -> bool {
        let p = d.pos(i);
        if grid.is_solid(p) || grid.design.cells[i].is_empty() { return true; }
        match self.cell_to_compartment[i] {
            OPEN_CELL => true,
            VOID_CELL => false,
            c if is_comp(c) => self.compartments.get(c as usize).is_none_or(|k| k.cells.binary_search(&CellIndex(i as u32)).is_ok()),
            _ => false,
        }
    }

    /// Classify one removed cell from its neighbours and apply the result.
    fn process_cell(&mut self, grid: &VesselGrid, ctx: &mut Upd, keys: &mut BTreeSet<(u16, u32, u8, u32, u8)>, touched: &mut BTreeSet<u16>, x: usize) {
        let d = ctx.d;
        ctx.visited = ctx.visited.saturating_add(6);
        ctx.processed.insert(x as u32);
        let own = self.cell_to_compartment[x];
        if is_comp(own) {
            // Open equipment inside a compartment was destroyed: its space is now air of that compartment.
            self.add_air(own, x, d);
            touched.insert(own);
            return;
        }
        let mut comps: Vec<(u16, Face)> = Vec::new();
        let mut sea = false;
        let mut open_nb: Vec<(usize, Face)> = Vec::new();
        for f in Face::ALL {
            let Some(j) = d.step(x, f) else { sea = true; continue };
            let lab = self.cell_to_compartment[j];
            if is_comp(lab) { comps.push((lab, f)); continue; }
            match lab {
                OPEN_CELL => open_nb.push((j, f)),
                VOID_CELL => {}
                _ => {
                    let p = d.pos(j);
                    if grid.is_solid(p) { if !grid.is_watertight(p) { sea = true; } }
                    else if grid.design.cells[j].is_empty() { sea = true; }
                    // An unprocessed removed neighbour counts as wall until it is processed itself.
                }
            }
        }
        if comps.is_empty() {
            if sea || !open_nb.is_empty() { self.set_label(x, OPEN_CELL, ctx); } else { self.cell_to_compartment[x] = VOID_CELL; }
            return;
        }
        // Merge every compartment this cell joins into the lowest id.
        let mut ids: Vec<u16> = comps.iter().map(|c| c.0).collect();
        ids.sort_unstable();
        ids.dedup();
        let target = ids[0];
        for &b in &ids[1..] { ctx.visited = ctx.visited.saturating_add(self.merge(target, b, d)); }
        touched.insert(target);
        if sea {
            self.set_label(x, OPEN_CELL, ctx);
            for &(_, f) in &comps { self.add_breach(target, x as u32, f, keys); }
        } else {
            self.add_air(target, x, d);
            self.set_label(x, target, ctx);
            for &(j, f) in &open_nb { self.add_breach(target, j as u32, opposite(f), keys); }
            if !open_nb.is_empty() { self.compartments[target as usize].sealed = false; }
        }
        if sea { self.compartments[target as usize].sealed = false; }
    }

    fn set_label(&mut self, x: usize, lab: u16, ctx: &mut Upd) { self.cell_to_compartment[x] = lab; self.queue_voids(x, ctx); }
    fn queue_voids(&self, x: usize, ctx: &mut Upd) {
        for f in Face::ALL { if let Some(j) = ctx.d.step(x, f) { if self.cell_to_compartment[j] == VOID_CELL { ctx.voids.push(j); } } }
    }

    fn add_breach(&mut self, a: u16, cell: u32, face: Face, keys: &mut BTreeSet<(u16, u32, u8, u32, u8)>) {
        let o = Opening { a: CompartmentId(a), b: None, cell: CellIndex(cell), face, area_m2: CELL_FACE_M2, kind: OpeningKind::Breach, patched: false, open: true };
        if keys.insert(opening_order(&o)) { self.openings.push(o); }
    }

    /// Add a removed cell to a compartment's air.
    fn add_air(&mut self, id: u16, x: usize, d: Dims) {
        self.cell_to_compartment[x] = id;
        let Some(c) = self.compartments.get_mut(id as usize) else { return };
        let cell = CellIndex(x as u32);
        let Err(at) = c.cells.binary_search(&cell) else { return };
        let k = c.cells.len() as f64;
        c.centroid = if k > 0.0 { (c.centroid * k + d.centre(x)) / (k + 1.0) } else { d.centre(x) };
        c.cells.insert(at, cell);
        c.volume_m3 = c.cells.len() as f64 * CELL_VOLUME_M3;
        let [px, py, pz] = d.xyz(x);
        let p = [px as i16, py as i16, pz as i16];
        if c.is_absorbed() { c.min = p; c.max = p; } else { for k in 0..3 { c.min[k] = c.min[k].min(p[k]); c.max[k] = c.max[k].max(p[k]); } }
    }

    /// Fold compartment `b` into `a` (a < b). Returns cells visited.
    fn merge(&mut self, a: u16, b: u16, d: Dims) -> u32 {
        if a == b || b as usize >= self.compartments.len() || a as usize >= self.compartments.len() { return 0; }
        let mut visited = 0u32;
        let src = std::mem::replace(&mut self.compartments[b as usize], absorbed_shell(CompartmentId(b)));
        if !src.is_absorbed() {
            // Relabel every cell of `b` (air and open equipment) inside its box.
            for y in src.min[1].max(0) as usize..=(src.max[1].max(0) as usize).min(d.sy.saturating_sub(1)) {
                for z in src.min[2].max(0) as usize..=(src.max[2].max(0) as usize).min(d.sz.saturating_sub(1)) {
                    for x in src.min[0].max(0) as usize..=(src.max[0].max(0) as usize).min(d.sx.saturating_sub(1)) {
                        let i = d.idx(x, y, z);
                        visited = visited.saturating_add(1);
                        if self.cell_to_compartment[i] == b { self.cell_to_compartment[i] = a; }
                    }
                }
            }
        }
        let dst = &mut self.compartments[a as usize];
        let (va, vb) = (dst.volume_m3, src.volume_m3);
        if va + vb > 0.0 { dst.centroid = (dst.centroid * va + src.centroid * vb) / (va + vb); }
        let mut cells = std::mem::take(&mut dst.cells);
        cells.extend_from_slice(&src.cells);
        cells.sort_unstable();
        cells.dedup();
        dst.cells = cells;
        dst.volume_m3 = dst.cells.len() as f64 * CELL_VOLUME_M3;
        if !src.is_absorbed() {
            if dst.is_absorbed() { dst.min = src.min; dst.max = src.max; } else { for k in 0..3 { dst.min[k] = dst.min[k].min(src.min[k]); dst.max[k] = dst.max[k].max(src.max[k]); } }
        }
        let (wa, wb) = (dst.water_m3.max(0.0), src.water_m3.max(0.0));
        if wa + wb > 0.0 {
            dst.water_centroid = (dst.water_centroid * wa + src.water_centroid * wb) / (wa + wb);
            if wb > wa { dst.water_plane_y = src.water_plane_y; }
        }
        dst.water_m3 = wa + wb;
        dst.free_surface_m2 += src.free_surface_m2.max(0.0);
        dst.fire = dst.fire.max(src.fire);
        dst.fire_ticks = dst.fire_ticks.max(src.fire_ticks);
        dst.parts.extend_from_slice(&src.parts);
        dst.parts.sort_unstable();
        dst.parts.dedup();
        dst.magazine = match (dst.magazine, src.magazine) { (Some(x), Some(y)) => Some(x.min(y)), (x, y) => x.or(y) };
        dst.berths = dst.berths.saturating_add(src.berths);
        dst.below_waterline |= src.below_waterline;
        dst.flood_valve_open |= src.flood_valve_open;
        dst.sealed &= src.sealed;
        // Openings: retarget, drop the ones now inside, keep one flood valve.
        let (ia, ib) = (CompartmentId(a), CompartmentId(b));
        for o in &mut self.openings {
            if o.a == ib { o.a = ia; }
            if o.b == Some(ib) { o.b = Some(ia); }
        }
        self.openings.retain(|o| o.b != Some(o.a));
        self.sort_openings();
        let mut have_valve = BTreeSet::new();
        let mut seen = BTreeSet::new();
        self.openings.retain(|o| {
            if o.kind == OpeningKind::FloodValve && !have_valve.insert(o.a) { return false; }
            seen.insert(opening_order(o))
        });
        // Adjacency: retarget and fold duplicates.
        let mut adj: BTreeMap<(u16, u16), Adjacency> = BTreeMap::new();
        for mut e in std::mem::take(&mut self.adjacency) {
            if e.a == ib { e.a = ia; }
            if e.b == ib { e.b = ia; }
            if e.a == e.b { continue; }
            let key = (e.a.0.min(e.b.0), e.a.0.max(e.b.0));
            e.a = CompartmentId(key.0);
            e.b = CompartmentId(key.1);
            match adj.get_mut(&key) {
                Some(x) => { if e.shared_faces > x.shared_faces { x.vertical = e.vertical; } x.shared_faces = x.shared_faces.saturating_add(e.shared_faces); x.has_door |= e.has_door; }
                None => { adj.insert(key, e); }
            }
        }
        self.adjacency = adj.into_values().collect();
        visited.saturating_add(src.cells.len() as u32)
    }
}

/// Per-call scratch of `update_after_damage`.
struct Upd { d: Dims, visited: u32, processed: BTreeSet<u32>, voids: Vec<usize> }

fn absorbed_shell(id: CompartmentId) -> Compartment {
    Compartment {
        id, cells: Vec::new(), volume_m3: 0.0, min: [0; 3], max: [-1; 3], centroid: DVec3::ZERO, water_m3: 0.0, water_plane_y: 0.0,
        water_centroid: DVec3::ZERO, free_surface_m2: 0.0, fire: 0.0, fire_ticks: 0, parts: Vec::new(), magazine: None, berths: 0,
        below_waterline: false, flood_valve_open: false, sealed: false,
    }
}

pub(crate) fn find_part(parts: &[PartRuntime], s: PartSlot) -> Option<&PartRuntime> {
    match parts.get(s.index()) { Some(p) if p.slot == s => Some(p), _ => parts.iter().find(|p| p.slot == s) }
}

fn build_compartment(id: u16, cells: &[u32], cls: &[u8], d: Dims) -> Compartment {
    let mut min = [i16::MAX; 3];
    let mut max = [i16::MIN; 3];
    let mut air: Vec<CellIndex> = Vec::new();
    let mut sum_air = DVec3::ZERO;
    let mut sum_all = DVec3::ZERO;
    for &c in cells {
        let [x, y, z] = d.xyz(c as usize);
        let p = [x as i16, y as i16, z as i16];
        for k in 0..3 { min[k] = min[k].min(p[k]); max[k] = max[k].max(p[k]); }
        let centre = d.centre(c as usize);
        sum_all += centre;
        if cls[c as usize] == AIR { air.push(CellIndex(c)); sum_air += centre; }
    }
    let centroid = if !air.is_empty() { sum_air / air.len() as f64 } else if !cells.is_empty() { sum_all / cells.len() as f64 } else { DVec3::ZERO };
    // Cells are ascending, so the first air cell is on the lowest air layer.
    let low_y = air.first().map(|c| d.xyz(c.index())[1]);
    let floor = low_y.map_or(0, |y| air.iter().take_while(|c| d.xyz(c.index())[1] == y).count());
    Compartment {
        id: CompartmentId(id), volume_m3: air.len() as f64 * CELL_VOLUME_M3, cells: air, min, max, centroid, water_m3: 0.0,
        water_plane_y: min[1] as f64 * CELL_M, water_centroid: centroid, free_surface_m2: floor as f64 * CELL_FACE_M2, fire: 0.0, fire_ticks: 0,
        parts: Vec::new(), magazine: None, berths: 0, below_waterline: false, flood_valve_open: false, sealed: true,
    }
}

/// Containing compartment of a part: most of its passable cells, else the compartment its barrier cells touch
/// most. Ties go to the lowest id.
pub(crate) fn containment(p: &PartRuntime, c2c: &[u16], cls: &[u8], d: Dims) -> Option<u16> {
    let n = c2c.len();
    let mut inside: BTreeMap<u16, u32> = BTreeMap::new();
    let mut touch: BTreeMap<u16, u32> = BTreeMap::new();
    for c in &p.cells {
        let i = c.index();
        if i >= n || i >= cls.len() { continue; }
        if is_comp(c2c[i]) { *inside.entry(c2c[i]).or_insert(0) += 1; continue; }
        if cls[i] != BARRIER { continue; }
        for f in Face::ALL {
            if let Some(j) = d.step(i, f) { if is_comp(c2c[j]) { *touch.entry(c2c[j]).or_insert(0) += 1; } }
        }
    }
    let best = |m: &BTreeMap<u16, u32>| m.iter().fold(None, |acc: Option<(u16, u32)>, (&k, &v)| match acc { Some((_, bv)) if bv >= v => acc, _ => Some((k, v)) }).map(|x| x.0);
    best(&inside).or_else(|| best(&touch))
}

/// The two regions a door part joins: (a, b, lowest door cell, face of that cell toward `a`, door cells).
/// `b == None` when the far side is the exterior.
fn door_link(p: &PartRuntime, c2c: &[u16], cls: &[u8], ext: &[bool], d: Dims) -> Option<(u16, Option<u16>, u32, Face, u32)> {
    let n = c2c.len();
    let own: BTreeSet<u32> = p.cells.iter().map(|c| c.0).filter(|&c| (c as usize) < n).collect();
    // Region past the door along a direction: compartment id, u16::MAX for the exterior, None for a wall.
    let region = |start: usize, f: Face| -> Option<u16> {
        let mut i = start;
        for _ in 0..4 {
            let j = match d.step(i, f) { Some(j) => j, None => return Some(u16::MAX) };
            if own.contains(&(j as u32)) { i = j; continue; }
            if is_comp(c2c[j]) { return Some(c2c[j]); }
            if ext[j] && cls[j] != BARRIER { return Some(u16::MAX); }
            return None;
        }
        None
    };
    let mut pairs: BTreeMap<(u16, u16), (u32, u32, Face)> = BTreeMap::new();
    for &c in &own {
        for (neg, pos) in [(Face::XNeg, Face::XPos), (Face::YNeg, Face::YPos), (Face::ZNeg, Face::ZPos)] {
            let (Some(r1), Some(r2)) = (region(c as usize, neg), region(c as usize, pos)) else { continue };
            if r1 == r2 || (r1 == u16::MAX && r2 == u16::MAX) { continue; }
            let (lo, hi, face_to_lo) = if r1 < r2 { (r1, r2, neg) } else { (r2, r1, pos) };
            let e = pairs.entry((lo, hi)).or_insert((0, c, face_to_lo));
            e.0 += 1;
            if c < e.1 { e.1 = c; e.2 = face_to_lo; }
        }
    }
    let best = pairs.iter().fold(None, |acc: Option<((u16, u16), (u32, u32, Face))>, (&k, &v)| match acc { Some((_, bv)) if bv.0 >= v.0 => acc, _ => Some((k, v)) })?;
    let ((lo, hi), (count, cell, face)) = best;
    Some((lo, (hi != u16::MAX).then_some(hi), cell, face, count))
}

/// The top deck: the highest layer whose barrier cells roof at least half as much compartment area as the
/// most-roofing layer. Falls back to the highest layer with any barrier cell.
fn top_deck(cls: &[u8], c2c: &[u16], d: Dims) -> i16 {
    let mut counts = vec![0u32; d.sy];
    let mut highest_solid = 0usize;
    for y in 0..d.sy {
        for z in 0..d.sz {
            for x in 0..d.sx {
                let i = d.idx(x, y, z);
                if cls[i] != BARRIER { continue; }
                highest_solid = y;
                if y > 0 && is_comp(c2c[d.idx(x, y - 1, z)]) { counts[y] += 1; }
            }
        }
    }
    let m = counts.iter().copied().max().unwrap_or(0);
    if m == 0 { return highest_solid as i16; }
    (0..d.sy).rev().find(|&y| counts[y] * 2 >= m).unwrap_or(highest_solid) as i16
}
