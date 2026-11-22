//! Detaching a piece from its vessel and assembling the wreck it becomes.
//!
//! "Parent" is what keeps the vessel id, "wreck" is the piece. Every compartment of the parent is classified by
//! the live walls its air cells touch: only parent walls (it stays), only piece walls (it moves to the wreck),
//! or both. For both, each air cell votes with six axis rays to the first live cell: it goes to the wreck when
//! more rays end on piece cells than on parent cells, so a plate falling off a compartment takes no air with it
//! while a fracture through a compartment shares the air out by enclosure. A compartment left with air on both
//! sides is cut: its water is shared by volume and every air face that now looks at the other body is a breach.
//! The parent then treats the cells it lost exactly as destroyed cells (`update_after_damage`), so a lost plate
//! between two of its compartments merges them and a lost wall onto the sea becomes breaches. On the wreck every
//! parent cell is gone and open to the sea.
use super::split::Solid;
use super::Piece;
use crate::bits::BitSet;
use crate::compartments::{opposite, Compartment, CompartmentSet, Dims, Opening, OpeningKind, MAX_COMPARTMENTS, NO_COMPARTMENT, OPEN_CELL, VOID_CELL};
use crate::crew::CrewState;
use crate::entity::EntityId;
use crate::grid::VesselGrid;
use crate::math::{Aabb, Kinematics, Pose};
use crate::networks::Networks;
use crate::parts::{PartRuntime, PartState, VesselControls};
use crate::physics::BodyHandle;
use crate::vessel::{Vessel, VesselStatus};
use content::catalogue::Catalogue;
use content::cell::{Face, CELL_FACE_M2, CELL_M, CELL_VOLUME_M3};
use content::enums::DamageLevel;
use content::ids::{CellIndex, CompartmentId, PartSlot};
use glam::DVec3;
use std::collections::{BTreeMap, BTreeSet};

/// The wreck side of a detach, before mass and geometry are derived.
pub(crate) struct WreckCore {
    pub grid: VesselGrid,
    pub parts: Vec<PartRuntime>,
    pub compartments: CompartmentSet,
    pub pending: Vec<CellIndex>,
    /// Inclusive cell bounds of the piece; `None` for an empty piece.
    pub bounds: Option<([usize; 3], [usize; 3])>,
}

#[derive(Clone, Debug)]
enum Class {
    Stay,
    /// `walled` is true when some of its air also touches parent walls (a ray vote sent all of it over).
    Move { walled: bool },
    /// Per air cell of the compartment (in its `cells` order): true on the wreck side.
    Cut(Vec<bool>),
}
impl Class {
    fn in_parent(&self) -> bool { !matches!(self, Class::Move { .. }) }
    fn in_wreck(&self) -> bool { !matches!(self, Class::Stay) }
}

type OpeningKey = (u16, u32, u8, u32, u8);

fn is_comp(c: u16) -> bool { (c as usize) < MAX_COMPARTMENTS }

/// Same order as the compartments unit's opening order: (a, cell, face, b, kind).
fn opening_key(o: &Opening) -> OpeningKey {
    let kind = match o.kind { OpeningKind::Breach => 0, OpeningKind::Hole => 1, OpeningKind::Door(_) => 2, OpeningKind::FloodValve => 3 };
    (o.a.0, o.cell.0, o.face as u8, o.b.map_or(u32::MAX, |b| b.0 as u32), kind)
}

fn shell(id: CompartmentId) -> Compartment {
    Compartment {
        id, cells: Vec::new(), volume_m3: 0.0, min: [0; 3], max: [-1; 3], centroid: DVec3::ZERO, water_m3: 0.0, water_plane_y: 0.0,
        water_centroid: DVec3::ZERO, free_surface_m2: 0.0, fire: 0.0, fire_ticks: 0, parts: Vec::new(), magazine: None, berths: 0,
        below_waterline: false, flood_valve_open: false, sealed: false,
    }
}

/// Fluid and charge loads of a part emptied (they left with the other body or with the debris).
fn empty_loads(state: &mut PartState) {
    match state {
        PartState::Battery { kwh } => *kwh = 0.0,
        PartState::FuelTank { litres } => *litres = 0.0,
        PartState::Ballast { kg } => *kg = 0.0,
        PartState::Mount(m) => m.ready_rounds = 0,
        PartState::Magazine { rounds, ammo_kg, cookoff_ticks, .. } => { *rounds = 0; *ammo_kg = 0.0; *cookoff_ticks = 0; }
        PartState::Launcher { ready, .. } => *ready = 0,
        PartState::MineRail { mines, .. } => *mines = 0,
        PartState::Decoy { rounds, .. } => *rounds = 0,
        PartState::DroneBay { aboard, .. } => *aboard = 0,
        PartState::HeliDeck { aboard } => *aboard = false,
        PartState::Davit { rhib_aboard } => *rhib_aboard = false,
        _ => {}
    }
}

/// Stop everything a part does on its own (a wreck's parts run nothing).
fn stop_activity(state: &mut PartState) {
    match state {
        PartState::Engine { throttle, output_kw } => { *throttle = 0.0; *output_kw = 0.0; }
        PartState::Generator { output_kw } => *output_kw = 0.0,
        PartState::Propulsor { shaft_kw, thrust_n, .. } => { *shaft_kw = 0.0; *thrust_n = 0.0; }
        PartState::Sensor { radiating, .. } => *radiating = false,
        PartState::Jammer { radiating } => *radiating = false,
        PartState::Pump { running, target } => { *running = false; *target = None; }
        _ => {}
    }
}

/// A destroyed, emptied copy of a part (it keeps its footprint, so dry mass still splits by live cells).
fn wrecked(p: &PartRuntime) -> PartRuntime {
    let mut q = p.clone();
    q.health = 0.0;
    q.level = DamageLevel::Destroyed;
    q.compartment = None;
    empty_loads(&mut q.state);
    stop_activity(&mut q.state);
    q
}

/// Six-ray enclosure vote per air cell: true when more rays end on a live piece cell than on a live parent cell.
fn ray_sides(c: &Compartment, s: &Solid, in_piece: &BitSet) -> Vec<bool> {
    let layer = s.sx * s.sz;
    c.cells.iter().map(|a| {
        let i = a.index();
        if i >= s.n { return false; }
        let [x, y, z] = s.xyz(i);
        let (mut w, mut p) = (0u32, 0u32);
        // (steps available, stride, forward)
        let rays = [(x, 1usize, false), (s.sx - 1 - x, 1, true), (y, layer, false), (s.sy - 1 - y, layer, true), (z, s.sx, false), (s.sz - 1 - z, s.sx, true)];
        for (steps, stride, fwd) in rays {
            let mut q = i;
            for _ in 0..steps {
                q = if fwd { q + stride } else { q - stride };
                if s.solid(q) { if in_piece.get(q) { w += 1 } else { p += 1 } break; }
            }
        }
        w > p
    }).collect()
}

/// A compartment rebuilt on a subset of its air cells (ascending) holding `water` m^3; the water is laid in
/// from the lowest cells up until the flooding unit levels it at the current heel.
fn reshape(base: &Compartment, cells: Vec<CellIndex>, water: f64, d: Dims, pose: &Pose) -> Compartment {
    let m = cells.len();
    if m == 0 { return Compartment { parts: base.parts.clone(), ..shell(base.id) }; }
    let mut c = base.clone();
    c.cells = cells;
    c.volume_m3 = m as f64 * CELL_VOLUME_M3;
    let mut min = [i16::MAX; 3];
    let mut max = [i16::MIN; 3];
    let mut sum = DVec3::ZERO;
    for x in &c.cells {
        let p = d.xyz(x.index());
        for k in 0..3 { min[k] = min[k].min(p[k] as i16); max[k] = max[k].max(p[k] as i16); }
        sum += d.centre(x.index());
    }
    c.min = min;
    c.max = max;
    c.centroid = sum / m as f64;
    let water = if water.is_finite() { water.clamp(0.0, c.volume_m3) } else { 0.0 };
    c.water_m3 = water;
    let filled = if water > 0.0 { (libm::ceil(water / CELL_VOLUME_M3) as usize).clamp(1, m) } else { 0 };
    // Ascending index is ascending y, so the first cells are the lowest.
    let layer_y = d.xyz(c.cells[filled.saturating_sub(1)].index())[1];
    if filled > 0 {
        let mut s = DVec3::ZERO;
        for x in &c.cells[..filled] { s += d.centre(x.index()); }
        c.water_centroid = s / filled as f64;
        c.water_plane_y = pose.to_world(DVec3::new(c.centroid.x, (layer_y as f64 + 1.0) * CELL_M, c.centroid.z)).y;
    } else {
        c.water_centroid = c.centroid;
        c.water_plane_y = pose.to_world(DVec3::new(c.centroid.x, min[1] as f64 * CELL_M, c.centroid.z)).y;
    }
    c.free_surface_m2 = c.cells.iter().filter(|x| d.xyz(x.index())[1] == layer_y).count() as f64 * CELL_FACE_M2;
    c.sealed = false;
    c
}

/// A grid copied for a new body starts with no history: no replicated changes, nothing dirty, its own count.
fn clean_new_grid(g: &mut VesselGrid) {
    g.changes.clear();
    g.dirty_render.clear();
    g.dirty_chunks.clear();
    g.original_count = g.solid_count;
}

/// Removed cells walled in (`VOID_CELL`) that the given open cells now reach are open to the sea too.
fn open_voids(c2c: &mut [u16], starts: &[usize], d: Dims) {
    let mut queue: Vec<usize> = starts.to_vec();
    while let Some(i) = queue.pop() {
        for f in Face::ALL {
            let Some(q) = d.step(i, f) else { continue };
            if c2c.get(q) == Some(&VOID_CELL) { c2c[q] = OPEN_CELL; queue.push(q); }
        }
    }
}

fn empty_core(parent: &Vessel) -> WreckCore {
    let mut grid = parent.grid.clone();
    let all = grid.solid_cells();
    grid.remove_cells(&all);
    clean_new_grid(&mut grid);
    let compartments = CompartmentSet {
        compartments: parent.compartments.compartments.iter().map(|c| shell(c.id)).collect(),
        cell_to_compartment: vec![NO_COMPARTMENT; parent.compartments.cell_to_compartment.len()],
        adjacency: Vec::new(), openings: Vec::new(), top_deck_y: parent.compartments.top_deck_y,
    };
    grid.sealed_air = compartments.sealed_air_bits(grid.damage.len()).words;
    WreckCore { grid, parts: Vec::new(), compartments, pending: Vec::new(), bounds: None }
}

/// Detach `cells` (the live ones) from `parent`: the parent loses the cells, the parts wholly inside (left as
/// wrecked, emptied entries so part slots stay put), the compartments inside and its share of cut ones, and the
/// conduits through them; the fracture opens to the sea. With `build` the wreck side is assembled and returned.
/// Returns the core and the work done (cells).
pub(crate) fn detach(parent: &mut Vessel, cells: &[CellIndex], build: bool) -> (Option<WreckCore>, u32) {
    let s = Solid::new(&parent.grid);
    let n = s.n;
    let mut piece: Vec<CellIndex> = cells.iter().copied().filter(|c| s.solid(c.index())).collect();
    piece.sort_unstable();
    piece.dedup();
    if piece.is_empty() { return (build.then(|| empty_core(parent)), 0); }
    let mut work = piece.len() as u32;
    let mut in_piece = BitSet::new(n);
    let mut lo = [usize::MAX; 3];
    let mut hi = [0usize; 3];
    for c in &piece {
        in_piece.set(c.index(), true);
        let p = s.xyz(c.index());
        for k in 0..3 { lo[k] = lo[k].min(p[k]); hi[k] = hi[k].max(p[k]); }
    }

    // Parts: wholly inside the piece (moved) or cut across the fracture, by live cells.
    let np = parent.parts.len();
    let mut moved = vec![false; np];
    let mut cut = vec![false; np];
    for (i, p) in parent.parts.iter().enumerate() {
        let (mut a, mut b) = (0u32, 0u32);
        for c in &p.cells { if s.solid(c.index()) { if in_piece.get(c.index()) { a += 1 } else { b += 1 } } }
        moved[i] = a > 0 && b == 0;
        cut[i] = a > 0 && b > 0;
    }
    let moved_slots: BTreeSet<PartSlot> = parent.parts.iter().zip(&moved).filter(|(_, &m)| m).map(|(p, _)| p.slot).collect();
    let on_wreck = |slot: PartSlot| moved_slots.contains(&slot);

    // Compartments.
    let old = &parent.compartments;
    let c2c_ok = old.cell_to_compartment.len() == n && n > 0;
    let d = Dims::new(parent.grid.size());
    // Only compartments with air next to a piece cell can move or be cut.
    let mut touched = vec![false; old.compartments.len()];
    if c2c_ok {
        let mut nb = [0usize; 6];
        for c in &piece {
            let kn = s.neighbours(c.index(), &mut nb);
            for &q in &nb[..kn] {
                if s.solid(q) { continue; }
                if let Some(t) = old.cell_to_compartment.get(q).and_then(|&l| touched.get_mut(l as usize)) { *t = true; }
            }
        }
    }
    let classes: Vec<Class> = old.compartments.iter().enumerate().map(|(k, c)| {
        if !c2c_ok || !touched[k] || c.is_absorbed() || c.cells.is_empty() || k >= MAX_COMPARTMENTS { return Class::Stay; }
        let (mut w, mut p) = (0u32, 0u32);
        let mut nb = [0usize; 6];
        // Live = designed and below bucket 8 (every removal sets bucket 8): no chunk lookup per neighbour.
        let live = |q: usize| parent.grid.design.cells.get(q).is_some_and(|x| !x.is_empty()) && parent.grid.damage.get(q).is_some_and(|&b| b < content::cell::BUCKET_DESTROYED);
        for a in &c.cells {
            let kn = s.neighbours(a.index(), &mut nb);
            for &q in &nb[..kn] { if live(q) { if in_piece.get(q) { w += 1 } else { p += 1 } } }
        }
        if w == 0 { return Class::Stay; }
        if p == 0 { return Class::Move { walled: false }; }
        let sides = ray_sides(c, &s, &in_piece);
        if sides.iter().all(|&x| !x) { Class::Stay } else if sides.iter().all(|&x| x) { Class::Move { walled: true } } else { Class::Cut(sides) }
    }).collect();
    let side_of_cell = |k: usize, cell: usize| -> Option<bool> {
        let c = old.compartments.get(k)?;
        let j = c.cells.binary_search(&CellIndex(cell as u32)).ok()?;
        match classes.get(k)? { Class::Stay => Some(false), Class::Move { .. } => Some(true), Class::Cut(sides) => Some(sides[j]) }
    };
    let pose = parent.kin.pose;
    let filter_parts = |list: &[PartSlot], wreck: bool| -> Vec<PartSlot> { list.iter().copied().filter(|&s| on_wreck(s) == wreck).collect() };
    let magazine_of = |list: &[PartSlot]| -> Option<PartSlot> {
        list.iter().copied().find(|s| crate::compartments::find_part(&parent.parts, *s).is_some_and(|p| matches!(p.state, PartState::Magazine { .. })))
    };
    let mut p_comps: Vec<Compartment> = Vec::with_capacity(old.compartments.len());
    let mut w_comps: Vec<Compartment> = Vec::new();
    for (k, c) in old.compartments.iter().enumerate() {
        let (mut pc, wc) = match &classes[k] {
            Class::Stay => (c.clone(), None),
            Class::Move { .. } => (Compartment { berths: 0, ..shell(c.id) }, build.then(|| c.clone())),
            Class::Cut(sides) => {
                let (mut pcells, mut wcells) = (Vec::new(), Vec::new());
                for (j, &x) in c.cells.iter().enumerate() { if sides[j] { wcells.push(x) } else { pcells.push(x) } }
                let total = if c.water_m3.is_finite() { c.water_m3.max(0.0) } else { 0.0 };
                let wp = total * pcells.len() as f64 / c.cells.len() as f64;
                let wc = build.then(|| reshape(c, wcells, total - wp, d, &pose));
                (reshape(c, pcells, wp, d, &pose), wc)
            }
        };
        pc.parts = filter_parts(&pc.parts, false);
        pc.magazine = magazine_of(&pc.parts);
        p_comps.push(pc);
        if build {
            let mut wc = wc.unwrap_or_else(|| shell(c.id));
            wc.parts = filter_parts(&wc.parts, true);
            wc.magazine = magazine_of(&wc.parts);
            w_comps.push(wc);
        }
    }

    // Openings go to the side of the air they open into; a far side now on the other body becomes the sea. A
    // door whose part left with the other body is gone: the hole it leaves is found below.
    let step = |cell: usize, f: Face| if cell < n { d.step(cell, f) } else { None };
    let mut p_open: BTreeMap<OpeningKey, Opening> = BTreeMap::new();
    let mut w_open: BTreeMap<OpeningKey, Opening> = BTreeMap::new();
    let mut emit = |wreck: bool, o: Opening| {
        if wreck && !build { return; }
        let map = if wreck { &mut w_open } else { &mut p_open };
        map.entry(opening_key(&o)).or_insert(o);
    };
    for o in &old.openings {
        let ka = o.a.index();
        let Some(cls_a) = classes.get(ka) else { emit(false, *o); continue };
        let sa = match cls_a {
            Class::Stay => false,
            Class::Move { .. } => true,
            Class::Cut(_) => {
                let c = o.cell.index();
                match (step(c, o.face).and_then(|x| side_of_cell(ka, x)).or_else(|| side_of_cell(ka, c)), o.kind) {
                    (Some(x), _) => x,
                    (None, OpeningKind::Door(s)) => on_wreck(s),
                    _ => false,
                }
            }
        };
        let sb = o.b.and_then(|b| {
            let kb = b.index();
            Some(match classes.get(kb)? {
                Class::Stay => false,
                Class::Move { .. } => true,
                Class::Cut(_) => step(o.cell.index(), opposite(o.face)).and_then(|x| side_of_cell(kb, x)).or_else(|| side_of_cell(kb, o.cell.index())).unwrap_or(sa),
            })
        });
        let toward = |side: bool, mut x: Opening, to_sea: bool| -> Option<Opening> {
            if let OpeningKind::Door(slot) = x.kind { if on_wreck(slot) != side { return None; } }
            if to_sea {
                x.b = None;
                if x.kind == OpeningKind::Hole { x.kind = OpeningKind::Breach; x.patched = false; x.open = true; }
            }
            Some(x)
        };
        if let Some(x) = toward(sa, *o, o.b.is_some() && sb != Some(sa)) { emit(sa, x); }
        if let (Some(b), Some(sb)) = (o.b, sb) {
            if sb != sa {
                let flipped = Opening { a: b, b: None, cell: o.cell, face: opposite(o.face), area_m2: o.area_m2, kind: o.kind, patched: false, open: o.open };
                if let Some(x) = toward(sb, flipped, true) { emit(sb, x); }
            }
        }
    }
    let mut opened: BTreeSet<(bool, u16)> = BTreeSet::new();
    // Fracture faces: on the parent, air of a cut compartment facing the wreck's share of the air (faces on the
    // piece's cells are found by the recompute below); on the wreck, air of a moved or cut compartment facing
    // parent cells or the parent's share of the air.
    for (k, cls) in classes.iter().enumerate() {
        let sides: Option<&[bool]> = match cls { Class::Cut(s) => Some(s), Class::Move { walled: true } => None, _ => continue };
        let c = &old.compartments[k];
        for (j, a) in c.cells.iter().enumerate() {
            let side = sides.is_none_or(|s| s[j]);
            if side && !build { continue; }
            for f in Face::ALL {
                let Some(q) = step(a.index(), f) else { continue };
                let other_air = !s.solid(q) && old.cell_to_compartment.get(q) == Some(&(k as u16)) && side_of_cell(k, q).is_some_and(|x| x != side);
                let parent_wall = side && s.solid(q) && !in_piece.get(q);
                if !(other_air || parent_wall) { continue; }
                opened.insert((side, k as u16));
                emit(side, Opening { a: CompartmentId(k as u16), b: None, cell: CellIndex(q as u32), face: opposite(f), area_m2: CELL_FACE_M2, kind: OpeningKind::Breach, patched: false, open: true });
            }
        }
    }
    drop(emit);

    // Wreck cell labels: parent cells are gone and open to the sea.
    let mut wreck_c2c: Vec<u16> = Vec::new();
    if build && c2c_ok {
        wreck_c2c = vec![NO_COMPARTMENT; n];
        let mut starts = Vec::new();
        for (i, (slot, &l)) in wreck_c2c.iter_mut().zip(&old.cell_to_compartment).enumerate() {
            let live = s.solid(i);
            *slot = if live && !in_piece.get(i) {
                starts.push(i);
                OPEN_CELL
            } else if is_comp(l) {
                match classes.get(l as usize) {
                    Some(Class::Move { .. }) => l,
                    Some(Class::Cut(_)) if live || side_of_cell(l as usize, i) == Some(true) => l,
                    _ => NO_COMPARTMENT,
                }
            } else { l };
        }
        open_voids(&mut wreck_c2c, &starts, d);
    }
    // Parent labels: air that left is exterior, and the lost cells are unprocessed removed cells for the
    // recompute (a compartment label on one would make the recompute treat it as destroyed equipment inside).
    let mut parent_c2c = old.cell_to_compartment.clone();
    if c2c_ok {
        for (k, cls) in classes.iter().enumerate() {
            let c = &old.compartments[k];
            match cls {
                Class::Move { .. } => for a in &c.cells { if let Some(x) = parent_c2c.get_mut(a.index()) { *x = NO_COMPARTMENT; } },
                Class::Cut(sides) => for (j, a) in c.cells.iter().enumerate() { if sides[j] { if let Some(x) = parent_c2c.get_mut(a.index()) { *x = NO_COMPARTMENT; } } },
                Class::Stay => {}
            }
        }
        for c in &piece { parent_c2c[c.index()] = NO_COMPARTMENT; }
    }
    let p_adj: Vec<_> = old.adjacency.iter().copied().filter(|a| classes.get(a.a.index()).is_none_or(Class::in_parent) && classes.get(a.b.index()).is_none_or(Class::in_parent)).collect();
    let w_adj: Vec<_> = if build { old.adjacency.iter().copied().filter(|a| classes.get(a.a.index()).is_some_and(Class::in_wreck) && classes.get(a.b.index()).is_some_and(Class::in_wreck)).collect() } else { Vec::new() };
    let top_deck_y = old.top_deck_y;
    let pending: Vec<CellIndex> = if build {
        parent.pending_destroyed.iter().copied().filter(|c| {
            let i = c.index();
            i < n && Face::ALL.iter().any(|&f| d.step(i, f).is_some_and(|q| in_piece.get(q)))
        }).collect()
    } else { Vec::new() };

    // Wreck grid from the parent's while the piece is still in it.
    let wreck_grid = build.then(|| {
        let mut g = parent.grid.clone();
        let others: Vec<CellIndex> = g.solid_cells().into_iter().filter(|c| !in_piece.get(c.index())).collect();
        g.remove_cells(&others);
        clean_new_grid(&mut g);
        g
    });

    // Parts of both sides.
    let mut wreck_parts = Vec::new();
    for i in 0..np {
        if build && (moved[i] || cut[i]) {
            let p = &parent.parts[i];
            let mut q = if moved[i] { p.clone() } else { wrecked(p) };
            q.compartment = None;
            stop_activity(&mut q.state);
            wreck_parts.push(q);
        }
        if moved[i] { parent.parts[i] = wrecked(&parent.parts[i]); }
    }

    // Parent side.
    parent.grid.remove_cells(&piece);
    let p_openings: Vec<Opening> = p_open.into_values().collect();
    for &(side, k) in &opened {
        let list = if side { &mut w_comps } else { &mut p_comps };
        if let Some(c) = list.get_mut(k as usize) { c.sealed = false; }
    }
    parent.compartments = CompartmentSet { compartments: p_comps, cell_to_compartment: parent_c2c, adjacency: p_adj, openings: p_openings, top_deck_y };
    if c2c_ok {
        let (v, _) = parent.compartments.update_after_damage(&mut parent.grid, &piece, u32::MAX);
        work = work.saturating_add(v);
    }
    parent.grid.sealed_air = parent.compartments.sealed_air_bits(n).words;
    // Parts cut across the fracture lose the cells that left (health becomes the lower of stored and cell mean).
    if cut.iter().any(|&c| c) {
        let lost: Vec<CellIndex> = parent.parts.iter().zip(&cut).filter(|(_, &c)| c).flat_map(|(p, _)| p.cells.iter().copied().filter(|c| in_piece.get(c.index()))).collect();
        crate::parts::refresh_health(&mut parent.parts, &parent.grid, &lost);
    }
    for p in parent.parts.iter_mut() {
        if let Some(c) = p.compartment { if parent.compartments.get(c).is_none_or(Compartment::is_absorbed) { p.compartment = None; } }
    }
    parent.compartments.sync_part_compartments(&mut parent.parts);
    crate::networks::cut_conduits(&mut parent.networks, &piece);

    let Some(mut grid) = wreck_grid else { return (None, work) };
    let compartments = if c2c_ok {
        CompartmentSet { compartments: w_comps, cell_to_compartment: wreck_c2c, adjacency: w_adj, openings: w_open.into_values().collect(), top_deck_y }
    } else {
        CompartmentSet { top_deck_y, ..CompartmentSet::default() }
    };
    grid.sealed_air = compartments.sealed_air_bits(n).words;
    compartments.link_parts(&mut wreck_parts);
    (Some(WreckCore { grid, parts: wreck_parts, compartments, pending, bounds: Some((lo, hi)) }), work)
}

/// Build the wreck vessel from a detached core: everything except the collider cuboids and buoyancy blocks,
/// which `carve_piece` derives last. Also refreshes the parent's dry and wet mass. `pre_mass` and `pre_kin` are
/// the parent's before the detach.
pub(crate) fn assemble(parent: &mut Vessel, core: WreckCore, new_id: EntityId, catalogue: &Catalogue, pre_mass: &crate::mass::MassProperties, pre_kin: &Kinematics) -> Vessel {
    parent.dry_mass = crate::mass::dry_mass(&parent.grid, &parent.parts, catalogue);
    parent.mass = crate::mass::with_fluids(&parent.dry_mass, &parent.parts, &parent.compartments, catalogue);
    let dry = crate::mass::dry_mass(&core.grid, &core.parts, catalogue);
    let mass = crate::mass::with_fluids(&dry, &core.parts, &core.compartments, catalogue);
    let pose = pre_kin.pose;
    // Rigid split: the wreck's centre of mass keeps the velocity it had as part of the parent (body velocities
    // are centre-of-mass velocities, as Rapier reports them).
    let lin = pre_kin.lin_vel + pre_kin.ang_vel.cross(pose.rot * (mass.com - pre_mass.com));
    let kin = Kinematics { pose, lin_vel: if lin.is_finite() { lin } else { pre_kin.lin_vel }, ang_vel: pre_kin.ang_vel };
    // The parent's centre of mass moved too; its centre-of-mass velocity follows the same rigid-body rule.
    let plin = pre_kin.lin_vel + pre_kin.ang_vel.cross(pose.rot * (parent.mass.com - pre_mass.com));
    if plin.is_finite() { parent.kin.lin_vel = plin; }
    let (dims, bounds) = match core.bounds {
        Some((lo, hi)) => {
            let a = DVec3::new(lo[0] as f64, lo[1] as f64, lo[2] as f64) * CELL_M;
            let b = DVec3::new(hi[0] as f64 + 1.0, hi[1] as f64 + 1.0, hi[2] as f64 + 1.0) * CELL_M;
            let mut mn = DVec3::splat(f64::INFINITY);
            let mut mx = DVec3::splat(f64::NEG_INFINITY);
            for k in 0..8 {
                let w = pose.to_world(DVec3::new(if k & 1 == 0 { a.x } else { b.x }, if k & 2 == 0 { a.y } else { b.y }, if k & 4 == 0 { a.z } else { b.z }));
                mn = mn.min(w);
                mx = mx.max(w);
            }
            let e = b - a;
            ([e.x, e.y, e.z], Aabb { min: mn, max: mx })
        }
        None => ([0.0; 3], Aabb { min: pose.pos, max: pose.pos }),
    };
    let root_cell = crate::structure::pick_root_cell(&core.grid, dry.com);
    Vessel {
        id: new_id,
        side: parent.side,
        player: None,
        class: None,
        blueprint: parent.blueprint,
        name: parent.name.clone(),
        status: VesselStatus::Wreck,
        fidelity: parent.fidelity,
        grid: core.grid,
        parts: core.parts,
        compartments: core.compartments,
        networks: Networks::default(),
        crew: CrewState::default(),
        controls: VesselControls::default(),
        tonnage_t: dry.mass_kg / 1000.0,
        dry_mass: dry,
        mass,
        body: BodyHandle::NONE,
        cuboids: Vec::new(),
        blocks: crate::hydro::BuoyancyBlocks { block_cells: parent.blocks.block_cells, ..Default::default() },
        kin,
        bounds,
        root_cell,
        dims,
        waterline_y: parent.waterline_y,
        last_fired_tick: 0,
        capsize_ticks: 0,
        struck: false,
        pending_destroyed: core.pending,
        parent: Some(parent.id),
        spawn_tick: parent.spawn_tick,
        submersible: false,
    }
}

pub(crate) fn carve_piece(parent: &mut Vessel, piece: &Piece, new_id: EntityId, catalogue: &Catalogue) -> Vessel {
    let pre_mass = parent.mass;
    let pre_kin = parent.kin;
    let core = detach(parent, &piece.cells, true).0.unwrap_or_else(|| empty_core(parent));
    let mut wreck = assemble(parent, core, new_id, catalogue, &pre_mass, &pre_kin);
    let block_cells = if parent.blocks.block_cells > 0 { parent.blocks.block_cells } else { catalogue.tuning.buoyancy.block_cells };
    wreck.cuboids = crate::physics::greedy_cuboids(&wreck.grid);
    wreck.blocks = crate::hydro::build_blocks(&wreck.grid, &wreck.compartments, block_cells);
    crate::physics::regreedy_dirty(&parent.grid, &mut parent.cuboids);
    crate::hydro::rebuild_dirty_blocks(&mut parent.blocks, &parent.grid, &parent.compartments);
    wreck
}
