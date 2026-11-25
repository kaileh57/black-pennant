use super::carve;
use super::*;
use crate::compartments::testkit::{box_hull, build, catalogue, def, idx, part, BODY, STEEL};
use crate::compartments::{CompartmentSet, OpeningKind, OPEN_CELL};
use crate::crew::CrewState;
use crate::entity::EntityId;
use crate::hydro::BuoyancyBlocks;
use crate::mass;
use crate::math::{Aabb, Kinematics};
use crate::networks::Networks;
use crate::parts::{PartRuntime, PartState, VesselControls};
use crate::physics::BodyHandle;
use crate::vessel::{Fidelity, Vessel, VesselStatus};
use content::catalogue::Catalogue;
use content::cell::CellGrid;
use content::enums::DamageLevel;
use content::ids::{CellIndex, CompartmentId, ContentHash, PartSlot, SideId};
use content::part::{Flammable, PartFunction};
use content::tuning::StructureTuning;
use glam::DVec3;

fn vessel(design: CellGrid, spec: &[(PartSlot, Vec<CellIndex>, bool, Flammable)], mut parts: Vec<PartRuntime>, cat: &Catalogue) -> Vessel {
    let mut grid = build(design, spec);
    let (compartments, _) = CompartmentSet::derive(&grid, &parts);
    compartments.link_parts(&mut parts);
    grid.sealed_air = compartments.sealed_air_bits(grid.damage.len()).words;
    let dry = mass::dry_mass(&grid, &parts, cat);
    let wet = mass::with_fluids(&dry, &parts, &compartments, cat);
    let root_cell = pick_root_cell(&grid, dry.com);
    Vessel {
        id: EntityId { index: 3, generation: 1 }, side: SideId(0), player: None, class: None, blueprint: ContentHash::EMPTY, name: "test".into(),
        status: VesselStatus::Active, fidelity: Fidelity::Full, grid, parts, compartments, networks: Networks::default(), crew: CrewState::default(),
        controls: VesselControls::default(), dry_mass: dry, mass: wet, body: BodyHandle::NONE, cuboids: Vec::new(), blocks: BuoyancyBlocks::default(),
        kin: Kinematics::default(), bounds: Aabb::default(), root_cell, dims: [0.0; 3], waterline_y: 1.5, last_fired_tick: 0, capsize_ticks: 0,
        struck: false, pending_destroyed: Vec::new(), parent: None, spawn_tick: 0, submersible: false, tonnage_t: 0.0,
    }
}

fn destroy(v: &mut Vessel, cells: &[CellIndex]) {
    for &c in cells { v.grid.add_damage(c, 8); v.pending_destroyed.push(c); }
    v.pending_destroyed.sort_unstable();
    v.pending_destroyed.dedup();
}

/// Shell cells of the transverse ring at `x` of a box hull.
fn ring(g: &CellGrid, x: i16) -> Vec<CellIndex> {
    let [_, sy, sz] = g.size;
    let mut v = Vec::new();
    for y in 0..sy as i16 { for z in 0..sz as i16 { if y == 0 || z == 0 || y == sy as i16 - 1 || z == sz as i16 - 1 { v.push(idx(g, x, y, z)); } } }
    v.sort_unstable();
    v
}

/// Run splitting to the end in slices of `budget`, collecting pieces (wreck pieces are not carved).
fn run(v: &mut Vessel, job: &mut SplitJob, budget: u32) -> (Vec<Piece>, Option<CellIndex>, u32) {
    let mut pieces = Vec::new();
    let mut reroot = None;
    let mut calls = 0;
    loop {
        let r = step_split(v, job, budget);
        calls += 1;
        pieces.extend(r.pieces);
        if r.rerooted.is_some() { reroot = r.rerooted; }
        if !r.unfinished { break; }
        assert!(calls < 200_000, "split never finished");
    }
    (pieces, reroot, calls)
}

/// 32 x 8 x 8 hollow box, bulkheads at x = 10 and 21: three compartments.
fn three() -> (CellGrid, Vessel) {
    let g = box_hull([32, 8, 8], &[10, 21], &[]);
    let v = vessel(g.clone(), &[], Vec::new(), &catalogue(Vec::new()));
    (g, v)
}

/// 31 x 8 x 8 hollow box without bulkheads: a cut at x = 15 leaves two equal halves.
fn tie_hull() -> (CellGrid, Vessel) {
    let g = box_hull([31, 8, 8], &[], &[]);
    let v = vessel(g.clone(), &[], Vec::new(), &catalogue(Vec::new()));
    (g, v)
}

#[test]
fn root_is_keel_cell_under_centre_of_mass() {
    let (g, v) = three();
    // Centre of mass at x = 8.0 m, z = 2.0 m: columns 15/16 and 3/4 tie, the lower index wins.
    assert_eq!(v.root_cell, idx(&g, 15, 0, 3));
    assert_eq!(pick_root_cell(&v.grid, DVec3::splat(f64::NAN)), idx(&g, 15, 0, 3));
    // A column whose bottom is equipment is not keel.
    let dome = vec![idx(&g, 15, 0, 3)];
    let mut g2 = g.clone();
    g2.cells[dome[0].index()] = BODY;
    let v2 = vessel(g2.clone(), &[(PartSlot(0), dome, true, Flammable::No)], Vec::new(), &catalogue(Vec::new()));
    assert_eq!(pick_root_cell(&v2.grid, v.dry_mass.com), idx(&g, 16, 0, 3));
    // An empty grid has no root.
    g2.cells.iter_mut().for_each(|c| *c = content::cell::Cell::EMPTY);
    let v3 = vessel(g2, &[], Vec::new(), &catalogue(Vec::new()));
    assert_eq!(pick_root_cell(&v3.grid, DVec3::ZERO), CellIndex(0));
}

#[test]
fn single_hole_is_local_and_recomputes_compartments() {
    let (g, mut v) = three();
    destroy(&mut v, &[idx(&g, 15, 3, 0)]);
    let mut job = SplitJob::default();
    let r = step_split(&mut v, &mut job, crate::time::SPLIT_WORK_BUDGET);
    assert!(r.pieces.is_empty() && r.rerooted.is_none() && !r.unfinished);
    assert!(v.pending_destroyed.is_empty() && !job.in_progress);
    assert!(r.cells_visited < 200, "a single hole should only fill locally, visited {}", r.cells_visited);
    // The compartment recompute ran: the middle compartment has its breach.
    assert!(v.compartments.openings.iter().any(|o| o.a == CompartmentId(1) && o.kind == OpeningKind::Breach));
    assert!(!v.compartments.compartments[1].sealed);
    // Nothing left to do.
    let r2 = step_split(&mut v, &mut job, 100);
    assert_eq!((r2.cells_visited, r2.unfinished), (0, false));
}

#[test]
fn cut_through_hull_splits_stern_as_wreck() {
    let (g, mut v) = three();
    let before = v.grid.solid_count;
    destroy(&mut v, &ring(&g, 25));
    let (pieces, reroot, _) = run(&mut v, &mut SplitJob::default(), crate::time::SPLIT_WORK_BUDGET);
    assert_eq!(reroot, None);
    assert_eq!(pieces.len(), 1);
    let p = &pieces[0];
    assert_eq!(p.cells.len(), 5 * 28 + 64);
    assert!(!p.debris);
    assert!(p.cells.windows(2).all(|w| w[0] < w[1]));
    assert!(p.cells.iter().all(|c| v.grid.pos(*c).x >= 26));
    // Wreck pieces stay in the grid until carved.
    assert_eq!(v.grid.solid_count, before - 28);
    assert_eq!(v.root_cell, idx(&g, 15, 0, 3));
}

#[test]
fn debris_threshold_is_64_cells_and_debris_is_detached() {
    // 64 cells: a wreck.
    let (g, mut v) = three();
    destroy(&mut v, &ring(&g, 30));
    let (pieces, _, _) = run(&mut v, &mut SplitJob::default(), 50_000);
    assert_eq!(pieces.len(), 1);
    assert_eq!((pieces[0].cells.len(), pieces[0].debris), (64, false));
    // 63 cells: debris, removed from the vessel at once, and the compartment behind it opens to the sea.
    let (g, mut v) = three();
    let before = v.grid.solid_count;
    let mut cells = ring(&g, 30);
    cells.push(idx(&g, 31, 3, 3));
    destroy(&mut v, &cells);
    let changes = v.grid.changes.len();
    let r = step_split(&mut v, &mut SplitJob::default(), 50_000);
    assert_eq!(r.pieces.len(), 1);
    assert_eq!((r.pieces[0].cells.len(), r.pieces[0].debris), (63, true));
    assert!(!r.unfinished);
    assert_eq!(v.grid.solid_count, before - 29 - 63);
    assert!(r.pieces[0].cells.iter().all(|c| !v.grid.is_solid(v.grid.pos(*c))));
    assert_eq!(v.grid.changes.len(), changes, "debris leaves without damage events");
    let stern = &v.compartments.compartments[2];
    assert_eq!(stern.cells.len(), 9 * 36, "the stern compartment keeps its air");
    assert!(v.compartments.openings.iter().any(|o| o.a == CompartmentId(2) && o.kind == OpeningKind::Breach && v.grid.pos(o.cell).x == 31));
    assert!(r.pieces[0].cells.iter().all(|c| v.compartments.cell_to_compartment[c.index()] == OPEN_CELL));
}

#[test]
fn main_body_under_64_cells_keeps_the_id() {
    let mut g = CellGrid::new([10, 3, 3]);
    g.cells.iter_mut().for_each(|c| *c = STEEL);
    let mut v = vessel(g.clone(), &[], Vec::new(), &catalogue(Vec::new()));
    assert_eq!(v.root_cell, idx(&g, 4, 0, 1));
    let slab: Vec<CellIndex> = (0..3).flat_map(|y| (0..3).map(move |z| (y, z))).map(|(y, z)| idx(&g, 7, y, z)).collect();
    destroy(&mut v, &slab);
    let r = step_split(&mut v, &mut SplitJob::default(), 50_000);
    assert_eq!(r.pieces.len(), 1);
    assert_eq!((r.pieces[0].cells.len(), r.pieces[0].debris), (18, true));
    assert_eq!(v.grid.solid_count, 63);
    assert_eq!(v.root_cell, idx(&g, 4, 0, 1));
}

#[test]
fn lost_root_goes_to_largest_piece_with_ties_to_lowest_index() {
    let (g, mut v) = tie_hull();
    assert_eq!(v.root_cell, idx(&g, 15, 0, 3));
    destroy(&mut v, &ring(&g, 15));
    let (pieces, reroot, _) = run(&mut v, &mut SplitJob::default(), 50_000);
    assert_eq!(pieces.len(), 1);
    assert_eq!(pieces[0].cells.len(), 456);
    assert!(pieces[0].cells.iter().all(|c| v.grid.pos(*c).x > 15), "the bow half holds cell 0 and keeps the id");
    assert_eq!(reroot, Some(idx(&g, 14, 0, 3)));
    assert_eq!(v.root_cell, idx(&g, 14, 0, 3));
    // A larger stern keeps the id instead.
    let (g, mut v) = tie_hull();
    let mut cells = ring(&g, 15);
    cells.extend(ring(&g, 3));
    destroy(&mut v, &cells);
    let (pieces, reroot, _) = run(&mut v, &mut SplitJob::default(), 50_000);
    let sizes: Vec<(usize, bool)> = pieces.iter().map(|p| (p.cells.len(), p.debris)).collect();
    assert_eq!(sizes, vec![(64 + 2 * 28, false), (11 * 28, false)]);
    assert!(pieces[1].cells.iter().all(|c| (4..15).contains(&v.grid.pos(*c).x)));
    assert!(reroot.is_some_and(|r| v.grid.pos(r).x > 15));
}

fn final_state(v: &Vessel) -> (Vec<u8>, CompartmentSet, CellIndex, u32) { (v.grid.damage.clone(), v.compartments.clone(), v.root_cell, v.grid.solid_count) }

#[test]
fn budget_slicing_gives_identical_results() {
    for scenario in 0..3 {
        let make = || {
            let (g, mut v) = if scenario == 2 { tie_hull() } else { three() };
            let mut cells = ring(&g, if scenario == 2 { 15 } else { 25 });
            if scenario == 1 { cells.extend(ring(&g, 5)); cells.push(idx(&g, 15, 3, 0)); }
            destroy(&mut v, &cells);
            v
        };
        let mut a = make();
        let ra = run(&mut a, &mut SplitJob::default(), 50_000);
        let mut b = make();
        let rb = run(&mut b, &mut SplitJob::default(), 7);
        assert!(rb.2 > 10, "the small budget really sliced the work");
        assert_eq!((ra.0, ra.1), (rb.0, rb.1), "scenario {scenario}");
        assert!(final_state(&a) == final_state(&b), "scenario {scenario}");
    }
}

#[test]
fn job_survives_a_keyframe_round_trip_mid_fill() {
    let (g, mut a) = tie_hull();
    destroy(&mut a, &ring(&g, 15));
    let mut b = a.clone();
    let full = run(&mut a, &mut SplitJob::default(), 50_000);
    let mut job = SplitJob::default();
    let r = step_split(&mut b, &mut job, 300);
    assert!(r.unfinished && job.in_progress && job.phase == 2);
    let bytes = bincode::serialize(&job).expect("serialise");
    let back: SplitJob = bincode::deserialize(&bytes).expect("deserialise");
    assert_eq!(back, job);
    assert!(bytes.len() < 16 * 1024, "labels serialise sparsely ({} bytes)", bytes.len());
    let mut job = back;
    let rest = run(&mut b, &mut job, 50_000);
    assert_eq!((full.0, full.1), (rest.0, rest.1));
    assert!(final_state(&a) == final_state(&b));
}

/// Live cells the root cannot reach over face-adjacent live cells.
fn unreachable_from_root(v: &Vessel) -> usize {
    let g = &v.grid;
    let [sx, sy, sz] = g.size().map(|d| d as usize);
    let live = |i: usize| !g.design.cells[i].is_empty() && g.damage[i] < content::cell::BUCKET_DESTROYED;
    let mut seen = vec![false; sx * sy * sz];
    let mut stack = vec![v.root_cell.index()];
    if !live(stack[0]) { return g.solid_count as usize; }
    seen[stack[0]] = true;
    let mut reached = 0;
    while let Some(i) = stack.pop() {
        reached += 1;
        let (x, y, z) = (i % sx, i / (sx * sz), (i / sx) % sz);
        let mut nb = Vec::new();
        if x > 0 { nb.push(i - 1) } if x + 1 < sx { nb.push(i + 1) }
        if y > 0 { nb.push(i - sx * sz) } if y + 1 < sy { nb.push(i + sx * sz) }
        if z > 0 { nb.push(i - sx) } if z + 1 < sz { nb.push(i + sx) }
        for q in nb { if !seen[q] && live(q) { seen[q] = true; stack.push(q); } }
    }
    g.solid_count as usize - reached
}

#[test]
fn a_cut_landing_mid_job_is_never_half_seen() {
    for kill_root in [false, true] {
        let (g, mut v) = three();
        destroy(&mut v, &ring(&g, 5));
        let mut job = SplitJob::default();
        // Pause the job early in its fill, before the stern fill has grown past x = 6.
        while !(job.in_progress && job.phase == 2 && job.head > 0) { let _ = step_split(&mut v, &mut job, 1); }
        // Another cut lands while the job is paused: it isolates the x = 6 slice from the stern.
        let mut cut = ring(&g, 7);
        if kill_root { cut.push(v.root_cell); }
        destroy(&mut v, &cut);
        let mut pieces = Vec::new();
        loop {
            let r = step_split(&mut v, &mut job, 50_000);
            for p in &r.pieces { if !p.debris { v.grid.remove_cells(&p.cells); } }
            pieces.extend(r.pieces);
            if !r.unfinished { break; }
        }
        // The bow (x 0..4) is a wreck, the x = 6 slice is debris, and the main body is one body again.
        let sizes: Vec<(usize, bool)> = pieces.iter().map(|p| (p.cells.len(), p.debris)).collect();
        assert_eq!(sizes, vec![(64 + 4 * 28, false), (28, true)], "kill_root {kill_root}");
        assert!(pieces[0].cells.iter().all(|c| v.grid.pos(*c).x < 5) && pieces[1].cells.iter().all(|c| v.grid.pos(*c).x == 6));
        assert_eq!(unreachable_from_root(&v), 0, "kill_root {kill_root}: the vessel kept a body it is not joined to");
        assert!(v.grid.pos(v.root_cell).x > 7);
    }
}

#[test]
fn corrupt_job_recovers_without_panic() {
    let (g, mut v) = three();
    destroy(&mut v, &ring(&g, 25));
    let mut job = SplitJob { in_progress: true, phase: 2, labels: vec![5; 3], frontier: vec![CellIndex(9_999_999)], head: 0, ..SplitJob::default() };
    let (pieces, _, _) = run(&mut v, &mut job, 50_000);
    assert_eq!(pieces.len(), 1);
    // A consistent-looking job whose labels point at fills that do not exist.
    let (g, mut v) = three();
    destroy(&mut v, &ring(&g, 25));
    let mut job = SplitJob::default();
    let _ = step_split(&mut v, &mut job, 200);
    assert!(job.in_progress);
    for l in job.labels.iter_mut().filter(|l| **l != 0) { *l = 1_000_000; }
    let (pieces, _, _) = run(&mut v, &mut job, 50_000);
    assert_eq!(pieces.len(), 1);
}

#[test]
fn carve_conserves_mass_water_and_fuel() {
    let g = box_hull([32, 8, 8], &[10, 21], &[]);
    let cat = catalogue(vec![def("tank", PartFunction::FuelTank { litres: 800.0 }, vec![], 100.0), def("shaft", PartFunction::Ballast { max_kg: 0.0 }, vec![], 30.0)]);
    let tank_cells = vec![idx(&g, 28, 1, 2), idx(&g, 28, 1, 3)];
    let shaft_cells = vec![idx(&g, 24, 1, 3), idx(&g, 25, 1, 3), idx(&g, 26, 1, 3)];
    let spec = [(PartSlot(0), tank_cells.clone(), false, Flammable::Fuel), (PartSlot(1), shaft_cells.clone(), false, Flammable::No)];
    let mut design = g.clone();
    for c in tank_cells.iter().chain(&shaft_cells) { design.cells[c.index()] = BODY; }
    let parts = vec![part(0, 0, tank_cells, &design, PartState::FuelTank { litres: 500.0 }), part(1, 1, shaft_cells, &design, PartState::Inert)];
    let mut v = vessel(g.clone(), &spec, parts, &cat);
    for (k, w) in [(1usize, 2.0), (2, 9.0)] {
        let c = &mut v.compartments.compartments[k];
        c.water_m3 = w;
        c.water_centroid = c.centroid;
    }
    v.kin.ang_vel = DVec3::new(0.0, 0.1, 0.0);
    v.kin.lin_vel = DVec3::new(3.0, 0.0, 0.0);
    let mut cells = ring(&g, 25);
    cells.push(idx(&g, 25, 1, 3));
    destroy(&mut v, &cells);
    let (pieces, _, _) = run(&mut v, &mut SplitJob::default(), 50_000);
    assert_eq!(pieces.len(), 1);
    let piece = &pieces[0];
    // Stand-in for the mass refresh the world does each tick.
    v.dry_mass = mass::dry_mass(&v.grid, &v.parts, &cat);
    v.mass = mass::with_fluids(&v.dry_mass, &v.parts, &v.compartments, &cat);
    let (dry0, wet0) = (v.dry_mass, v.mass);
    let water0: f64 = v.compartments.compartments.iter().map(|c| c.water_m3).sum();
    let solid0 = v.grid.solid_count;
    let (pre_mass, pre_kin) = (v.mass, v.kin);
    let (core, _) = carve::detach(&mut v, &piece.cells, true);
    let w = carve::assemble(&mut v, core.expect("core"), EntityId { index: 9, generation: 1 }, &cat, &pre_mass, &pre_kin);

    assert_eq!(w.status, VesselStatus::Wreck);
    assert_eq!(w.parent, Some(v.id));
    assert_eq!(w.grid.solid_count as usize, piece.cells.len());
    assert_eq!(v.grid.solid_count as usize, solid0 as usize - piece.cells.len());
    assert!(w.grid.changes.is_empty() && w.grid.original_count == w.grid.solid_count);
    let both = mass::combine(&v.dry_mass, &w.dry_mass);
    assert!((both.mass_kg - dry0.mass_kg).abs() < 1e-6, "{} vs {}", both.mass_kg, dry0.mass_kg);
    assert!((both.com - dry0.com).length() < 1e-9);
    let wet = mass::combine(&v.mass, &w.mass);
    assert!((wet.mass_kg - wet0.mass_kg).abs() < 1e-6, "{} vs {}", wet.mass_kg, wet0.mass_kg);
    let water1: f64 = v.compartments.compartments.iter().chain(&w.compartments.compartments).map(|c| c.water_m3).sum();
    assert!((water1 - water0).abs() < 1e-9);
    // The tank moved whole; the parent keeps an empty, destroyed entry in its slot.
    assert!(matches!(w.parts.iter().find(|p| p.slot == PartSlot(0)).map(|p| &p.state), Some(PartState::FuelTank { litres }) if *litres == 500.0));
    assert!(matches!(v.parts[0].state, PartState::FuelTank { litres } if litres == 0.0) && v.parts[0].level == DamageLevel::Destroyed);
    // The shaft was cut: the parent keeps it at a third of its cells, the wreck carries a wrecked copy.
    assert!((v.parts[1].health - 1.0 / 3.0).abs() < 1e-9 && v.parts[1].level == DamageLevel::Heavy, "{:?}", (v.parts[1].health, v.parts[1].level));
    assert!(w.parts.iter().any(|p| p.slot == PartSlot(1) && p.level == DamageLevel::Destroyed));
    // The stern compartment was cut: x 22..25 stays, x 26..30 goes, water by volume, both open along the cut.
    assert_eq!(w.compartments.compartments.len(), v.compartments.compartments.len());
    let (ps, ws) = (&v.compartments.compartments[2], &w.compartments.compartments[2]);
    assert!(ps.cells.iter().all(|c| v.grid.pos(*c).x <= 25) && ws.cells.iter().all(|c| w.grid.pos(*c).x >= 26));
    // 9 layers of 36, less the tank and the two live shaft cells (the destroyed one became air).
    assert_eq!(ps.cells.len() + ws.cells.len(), 9 * 36 - 4, "every air cell of the stern lands on one side");
    assert!((ps.water_m3 + ws.water_m3 - 9.0).abs() < 1e-9 && ws.water_m3 > ps.water_m3);
    assert!(!ps.sealed && !ws.sealed);
    let cut_faces = |set: &CompartmentSet| set.openings.iter().filter(|o| o.a == CompartmentId(2) && o.kind == OpeningKind::Breach && o.b.is_none()).count();
    assert!(cut_faces(&v.compartments) >= 24 + 30 && cut_faces(&w.compartments) >= 30, "{} {}", cut_faces(&v.compartments), cut_faces(&w.compartments));
    // The other compartments stay with the parent; the wreck holds shells for them.
    assert!(w.compartments.compartments[0].is_absorbed() && w.compartments.compartments[1].is_absorbed());
    assert_eq!(v.compartments.compartments[1].water_m3, 2.0);
    assert_eq!(w.parts.iter().find(|p| p.slot == PartSlot(0)).and_then(|p| p.compartment), Some(CompartmentId(2)));
    // Labels: the parent's cells are gone from the wreck; the wreck's air is outside the parent.
    assert!(v.grid.solid_cells().iter().all(|c| w.compartments.cell_to_compartment[c.index()] == OPEN_CELL));
    assert!(ws.cells.iter().all(|c| v.compartments.of_cell(*c).is_none()));
    // Kinematics, root and frame.
    assert_eq!(w.kin.pose, v.kin.pose);
    let expect = pre_kin.lin_vel + pre_kin.ang_vel.cross(w.mass.com - pre_mass.com);
    assert!((w.kin.lin_vel - expect).length() < 1e-12);
    assert!(w.grid.is_solid(w.grid.pos(w.root_cell)) && w.grid.pos(w.root_cell).y == 0);
    assert_eq!(w.dims, [3.0, 4.0, 4.0]);
}

#[test]
fn wreck_despawn_rules() {
    let t = StructureTuning::default();
    let id = |i: u32| EntityId { index: i, generation: 1 };
    let tick = 100_000;
    // Old (exactly 600 s), deep, NaN depth, young and shallow; a duplicate id counts once.
    let wrecks = vec![(id(1), tick - 36_000, 0.0), (id(2), tick - 10, 20.5), (id(3), tick - 10, f64::NAN), (id(4), tick - 35_999, 19.9), (id(2), tick, 0.0)];
    assert_eq!(wrecks_to_despawn(&wrecks, tick, &t), vec![id(1), id(2)]);
    // Cap of 16: the oldest go first, ties by id.
    let mut many: Vec<(EntityId, u64, f64)> = (0..18).map(|i| (id(100 - i), tick - 1000 + (i as u64 / 2), 1.0)).collect();
    many.push((id(500), tick + 50, 0.0));
    // Spawn ticks: ids 100 and 99 oldest, then 98 and 97 (the lower id goes first).
    assert_eq!(wrecks_to_despawn(&many, tick, &t), vec![id(97), id(99), id(100)]);
    // A future spawn tick is age zero; broken tuning disables a rule instead of panicking.
    let off = StructureTuning { wreck_ttl_s: f64::NAN, wreck_despawn_depth_m: f64::INFINITY, ..t };
    assert!(wrecks_to_despawn(&[(id(1), 0, 1e9)], u64::MAX, &off).is_empty());
}

#[test]
fn falling_bulkhead_plate_merges_neighbours_and_takes_no_air() {
    let (g, mut v) = three();
    // Cutting the shell ring at the forward bulkhead frees the bow (a wreck) and the bulkhead's inner plate
    // (36 cells of debris).
    destroy(&mut v, &ring(&g, 10));
    let r = step_split(&mut v, &mut SplitJob::default(), 50_000);
    let sizes: Vec<(usize, bool)> = r.pieces.iter().map(|p| (p.cells.len(), p.debris)).collect();
    assert_eq!(sizes, vec![(64 + 9 * 28, false), (36, true)]);
    // The plate is gone: the two compartments it separated are one, with all their air and the plate's space.
    let set = &v.compartments;
    assert!(set.compartments[1].is_absorbed());
    assert_eq!(set.compartments[0].cells.len(), 9 * 36 + 10 * 36 + 36);
    assert!(!set.compartments[0].sealed);
    // Carving the bow now splits the merged compartment at the fracture.
    let cat = catalogue(Vec::new());
    let (pre_mass, pre_kin) = (v.mass, v.kin);
    let (core, _) = carve::detach(&mut v, &r.pieces[0].cells, true);
    let w = carve::assemble(&mut v, core.expect("core"), EntityId { index: 8, generation: 1 }, &cat, &pre_mass, &pre_kin);
    let (pc, wc) = (&v.compartments.compartments[0], &w.compartments.compartments[0]);
    assert!(pc.cells.iter().all(|c| v.grid.pos(*c).x >= 10) && wc.cells.iter().all(|c| w.grid.pos(*c).x <= 9));
    assert_eq!(pc.cells.len() + wc.cells.len(), 720);
    assert!(w.compartments.compartments[2].is_absorbed() && !v.compartments.compartments[2].is_absorbed());
    assert!(v.grid.solid_cells().iter().all(|c| v.grid.pos(*c).x >= 10));
    assert!(w.grid.solid_cells().iter().all(|c| w.grid.pos(*c).x <= 9));
}

#[test]
fn debris_takes_its_parts_with_it() {
    let g = box_hull([32, 8, 8], &[10, 21], &[]);
    let cat = catalogue(vec![def("mag", PartFunction::FuelTank { litres: 100.0 }, vec![], 50.0)]);
    let cells = vec![idx(&g, 31, 4, 4)];
    let mut design = g.clone();
    design.cells[cells[0].index()] = BODY;
    let parts = vec![part(0, 0, cells.clone(), &design, PartState::FuelTank { litres: 80.0 })];
    let mut v = vessel(g.clone(), &[(PartSlot(0), cells, true, Flammable::Fuel)], parts, &cat);
    let mut cut = ring(&g, 30);
    cut.push(idx(&g, 31, 3, 3));
    destroy(&mut v, &cut);
    let r = step_split(&mut v, &mut SplitJob::default(), 50_000);
    assert_eq!((r.pieces.len(), r.pieces[0].debris), (1, true));
    let p = &v.parts[0];
    assert_eq!(p.level, DamageLevel::Destroyed);
    assert!(matches!(p.state, PartState::FuelTank { litres } if litres == 0.0));
    assert_eq!(p.compartment, None);
    assert!(v.compartments.compartments.iter().all(|c| !c.parts.contains(&PartSlot(0))));
}
