use super::testkit::*;
use super::*;
use content::part::Flammable;

/// 32 x 12 x 12 box, bulkheads at x = 10 and 21, deck at y = 6: three compartments below and three above.
fn six() -> (CellGrid, VesselGrid) {
    let g = box_hull([32, 12, 12], &[10, 21], &[6]);
    let v = build(g.clone(), &[]);
    (g, v)
}
use content::cell::CellGrid;

#[test]
fn derive_box_hull() {
    let (g, v) = six();
    let (set, visited) = CompartmentSet::derive(&v, &[]);
    assert!(visited > 0);
    assert_eq!(set.compartments.len(), 6);
    let vols: Vec<f64> = set.compartments.iter().map(|c| c.volume_m3).collect();
    assert_eq!(vols, vec![450.0 * 0.125, 500.0 * 0.125, 450.0 * 0.125, 360.0 * 0.125, 400.0 * 0.125, 360.0 * 0.125]);
    assert!(set.compartments.iter().all(|c| c.sealed && c.water_m3 == 0.0 && c.free_surface_m2 > 0.0));
    assert_eq!(set.compartments[0].free_surface_m2, 90.0 * 0.25);
    assert_eq!(set.of_cell(idx(&g, 5, 3, 5)), Some(CompartmentId(0)));
    assert_eq!(set.of_cell(idx(&g, 15, 8, 5)), Some(CompartmentId(4)));
    assert_eq!(set.of_cell(idx(&g, 10, 3, 5)), None);
    let pairs: Vec<(u16, u16, bool)> = set.adjacency.iter().map(|a| (a.a.0, a.b.0, a.vertical)).collect();
    assert_eq!(pairs, vec![(0, 1, false), (0, 3, true), (1, 2, false), (1, 4, true), (2, 5, true), (3, 4, false), (4, 5, false)]);
    assert_eq!(set.neighbours(CompartmentId(1)), vec![CompartmentId(0), CompartmentId(2), CompartmentId(4)]);
    assert_eq!(set.top_deck_y, 11);
    let bits = set.sealed_air_bits(g.len());
    assert_eq!(bits.count_ones(), set.compartments.iter().map(|c| c.cells.len()).sum::<usize>());
    // Same grid, same set.
    assert_eq!(CompartmentSet::derive(&v, &[]).0, set);
}

#[test]
fn breach_opens_and_is_idempotent() {
    let (g, mut v) = six();
    let (mut set, _) = CompartmentSet::derive(&v, &[]);
    v.sealed_air = set.sealed_air_bits(g.len()).words;
    let hole = idx(&g, 5, 0, 5);
    v.add_damage(hole, 8);
    let (visited, more) = set.update_after_damage(&mut v, &[hole], 1_000);
    assert!(visited > 0 && !more);
    let breaches: Vec<&Opening> = set.openings.iter().filter(|o| o.kind == OpeningKind::Breach).collect();
    assert_eq!(breaches.len(), 1);
    assert_eq!((breaches[0].a, breaches[0].b, breaches[0].cell, breaches[0].face, breaches[0].area_m2), (CompartmentId(0), None, hole, Face::YPos, 0.25));
    assert!(!set.compartments[0].sealed && set.compartments[1].sealed);
    assert_eq!(set.cell_to_compartment[hole.index()], OPEN_CELL);
    assert_eq!(set.of_cell(hole), None);
    let inside = idx(&g, 5, 3, 5);
    assert_eq!(v.sealed_air[inside.index() / 64] >> (inside.index() % 64) & 1, 0);
    let other = idx(&g, 15, 3, 5);
    assert_eq!(v.sealed_air[other.index() / 64] >> (other.index() % 64) & 1, 1);
    // Passing the same cell again changes nothing; a breach the damage unit already added is not doubled.
    let before = set.clone();
    set.update_after_damage(&mut v, &[hole], 1_000);
    assert_eq!(set, before);
    // A patched face reopens when the next cell goes.
    set.openings.iter_mut().for_each(|o| o.patched = true);
    let next = idx(&g, 6, 0, 5);
    v.add_damage(next, 8);
    set.update_after_damage(&mut v, &[next], 1_000);
    let b: Vec<&Opening> = set.openings.iter().filter(|o| o.kind == OpeningKind::Breach).collect();
    assert_eq!(b.len(), 2);
    assert!(b.iter().all(|o| !o.patched));
}

#[test]
fn destroyed_bulkhead_merges_and_sums_water() {
    let (g, mut v) = six();
    let (mut set, _) = CompartmentSet::derive(&v, &[]);
    set.compartments[0].water_m3 = 10.0;
    set.compartments[1].water_m3 = 4.0;
    set.compartments[1].fire = 60.0;
    let cell = idx(&g, 10, 2, 5);
    v.add_damage(cell, 8);
    set.update_after_damage(&mut v, &[cell], 50_000);
    assert_eq!(set.compartments.len(), 6, "ids stay stable");
    let c0 = &set.compartments[0];
    assert_eq!(c0.water_m3, 14.0);
    assert_eq!(c0.fire, 60.0);
    assert_eq!(c0.cells.len(), 450 + 500 + 1);
    assert!(set.compartments[1].is_absorbed() && set.compartments[1].cells.is_empty() && set.compartments[1].water_m3 == 0.0);
    assert_eq!(set.of_cell(idx(&g, 15, 3, 5)), Some(CompartmentId(0)));
    assert_eq!(set.of_cell(cell), Some(CompartmentId(0)));
    let pairs: Vec<(u16, u16)> = set.adjacency.iter().map(|a| (a.a.0, a.b.0)).collect();
    assert_eq!(pairs, vec![(0, 2), (0, 3), (0, 4), (2, 5), (3, 4), (4, 5)]);
    assert!(set.compartments[0].sealed, "an internal hole does not open the hull");
    assert_eq!(set.neighbours(CompartmentId(1)), Vec::<CompartmentId>::new());
}

#[test]
fn enclosed_void_waits_for_a_neighbour() {
    // Double bottom: the floor is three cells thick (y = 0..=2).
    let mut g = box_hull([12, 10, 10], &[], &[1, 2]);
    for z in 0..10 { for x in 0..12 { for y in 0..3 { g.set(CellPos::new(x, y, z), STEEL); } } }
    let mut v = build(g.clone(), &[]);
    let (mut set, _) = CompartmentSet::derive(&v, &[]);
    assert_eq!(set.compartments.len(), 1);
    let mid = idx(&g, 5, 1, 5);
    v.add_damage(mid, 8);
    set.update_after_damage(&mut v, &[mid], 1_000);
    assert_eq!(set.cell_to_compartment[mid.index()], VOID_CELL);
    assert!(set.openings.is_empty());
    // The outer plate goes: the void now opens to the sea, but no compartment touches it yet.
    let outer = idx(&g, 5, 0, 5);
    v.add_damage(outer, 8);
    set.update_after_damage(&mut v, &[outer], 1_000);
    assert_eq!(set.cell_to_compartment[mid.index()], OPEN_CELL);
    assert!(set.openings.is_empty() && set.compartments[0].sealed);
    // The inner plate goes: its space joins the compartment, and the breach sits at the narrowest point,
    // the open cell beyond it.
    let inner = idx(&g, 5, 2, 5);
    v.add_damage(inner, 8);
    set.update_after_damage(&mut v, &[inner], 1_000);
    let b: Vec<&Opening> = set.openings.iter().filter(|o| o.kind == OpeningKind::Breach).collect();
    assert_eq!(b.len(), 1);
    assert_eq!((b[0].a, b[0].cell, b[0].face), (CompartmentId(0), mid, Face::YPos));
    assert_eq!(set.of_cell(inner), Some(CompartmentId(0)));
    assert!(!set.compartments[0].sealed);
}

#[test]
fn budget_carries_over_and_finishes() {
    let (g, mut v) = six();
    let (mut set, _) = CompartmentSet::derive(&v, &[]);
    let cells: Vec<CellIndex> = (1..9).map(|x| idx(&g, x, 0, 5)).collect();
    for &c in &cells { v.add_damage(c, 8); }
    let (_, more) = set.update_after_damage(&mut v, &cells, 1);
    assert!(more);
    let mut rounds = 1;
    loop {
        let (_, more) = set.update_after_damage(&mut v, &cells, 1);
        rounds += 1;
        if !more { break; }
        assert!(rounds < 20);
    }
    assert_eq!(set.openings.iter().filter(|o| o.kind == OpeningKind::Breach).count(), 8);
    // Same result as one unbudgeted call.
    let (mut one, _) = CompartmentSet::derive(&build(g.clone(), &[]), &[]);
    let mut v2 = build(g.clone(), &[]);
    for &c in &cells { v2.add_damage(c, 8); }
    one.update_after_damage(&mut v2, &cells, 50_000);
    assert_eq!(one.openings, set.openings);
}

#[test]
fn doors_valves_and_parts() {
    let g = box_hull([32, 12, 12], &[10, 21], &[6]);
    // A watertight door in the first bulkhead below deck, a watertight tank against the floor of
    // compartment 1, and open equipment in compartment 2.
    let door_cells = block(&g, [10, 1, 4], [10, 3, 5]);
    let tank_cells = block(&g, [12, 1, 2], [13, 2, 3]);
    let gear_cells = block(&g, [24, 1, 4], [25, 2, 5]);
    let v = build(g.clone(), &[(PartSlot(0), door_cells.clone(), true, Flammable::No), (PartSlot(1), tank_cells.clone(), true, Flammable::Fuel), (PartSlot(2), gear_cells.clone(), false, Flammable::No)]);
    let parts = vec![
        part(0, 0, door_cells.clone(), &g, PartState::Door { closed: true }),
        part(1, 0, tank_cells, &g, PartState::Magazine { rounds: 10, ammo_kg: 100.0, flooded: false, cookoff_ticks: 0 }),
        part(2, 0, gear_cells.clone(), &g, PartState::Inert),
    ];
    let (mut set, _) = CompartmentSet::derive(&v, &parts);
    assert_eq!(set.compartments.len(), 6);
    let door: Vec<&Opening> = set.openings.iter().filter(|o| matches!(o.kind, OpeningKind::Door(_))).collect();
    assert_eq!(door.len(), 1);
    assert_eq!((door[0].a, door[0].b, door[0].area_m2, door[0].open), (CompartmentId(0), Some(CompartmentId(1)), 6.0 * 0.25, false));
    assert!(set.adjacency.iter().any(|a| a.a.0 == 0 && a.b.0 == 1 && a.has_door));
    assert_eq!(set.compartments[1].parts, vec![PartSlot(1)]);
    assert_eq!(set.compartments[1].magazine, Some(PartSlot(1)));
    assert_eq!(set.compartments[2].parts, vec![PartSlot(2)]);
    // Equipment displaces water but maps to its compartment.
    assert_eq!(set.compartments[2].volume_m3, (450.0 - 8.0) * 0.125);
    assert_eq!(set.of_cell(gear_cells[0]), Some(CompartmentId(2)));
    let mut ps = parts.clone();
    set.link_parts(&mut ps);
    assert_eq!(ps.iter().map(|p| p.compartment).collect::<Vec<_>>(), vec![Some(CompartmentId(0)), Some(CompartmentId(1)), Some(CompartmentId(2))]);
    // Opening the door shows on the opening.
    ps[0].state = PartState::Door { closed: false };
    set.sync_doors(&ps);
    assert!(set.open_door_between(CompartmentId(1), CompartmentId(0)));
    // Waterline at 2 m: the three lower compartments get valves.
    set.set_waterline(2.0, 0.1);
    let valves: Vec<u16> = set.openings.iter().filter(|o| o.kind == OpeningKind::FloodValve).map(|o| o.a.0).collect();
    assert_eq!(valves, vec![0, 1, 2]);
    assert!(set.compartments[0].below_waterline && !set.compartments[3].below_waterline);
    set.set_waterline(2.0, 0.1);
    assert_eq!(set.openings.iter().filter(|o| o.kind == OpeningKind::FloodValve).count(), 3);
}

#[test]
fn bad_input_does_not_panic() {
    let (g, mut v) = six();
    let (mut set, _) = CompartmentSet::derive(&v, &[]);
    let junk = [CellIndex(u32::MAX), CellIndex(g.len() as u32), idx(&g, 5, 3, 5), idx(&g, 0, 0, 0)];
    let before = set.clone();
    set.update_after_damage(&mut v, &junk, 10);
    assert_eq!(set, before, "cells that were not removed change nothing");
    let mut other = CompartmentSet::default();
    assert_eq!(other.update_after_damage(&mut v, &junk, 10), (0, false));
    let empty = build(CellGrid::new([0, 0, 0]), &[]);
    let (e, _) = CompartmentSet::derive(&empty, &[]);
    assert!(e.compartments.is_empty());
}
