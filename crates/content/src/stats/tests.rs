use super::testkit::{blueprint, catalogue, place, rows, shell_box, steel};
use super::*;
use crate::cell::{ArmourTier, CellRow, CellRows, CellRun};

fn close(a: f64, b: f64) -> bool { (a - b).abs() < 1e-9 }

#[test]
fn structure_mass_cost_and_extent() {
    let cat = catalogue();
    let cells = rows(&[(0, 0, 0, steel(ArmourTier::None)), (1, 0, 0, steel(ArmourTier::None)), (2, 0, 0, steel(ArmourTier::None)), (3, 0, 0, steel(ArmourTier::Mm5))]);
    let st = compute_stats(&blueprint(cells, 0), &cat);
    assert!(close(st.light_t, 0.410) && close(st.tonnage_t, 0.410));
    assert_eq!(st.cost, 5); // 4 cells + 0.5 armour = 4.5, rounded half away from zero
    assert_eq!((st.length_m, st.beam_m, st.height_m), (2.0, 0.5, 0.5));
    assert_eq!(st.cell_count, 4);
    // Centre of mass: 100 kg at x = 0.25, 0.75, 1.25 and 110 kg at 1.75.
    assert!(close(st.com_m[0], (100.0 * (0.25 + 0.75 + 1.25) + 110.0 * 1.75) / 410.0));
}

#[test]
fn part_replaces_structure_but_keeps_armour_and_loads_count() {
    let cat = catalogue();
    let cells = rows(&[(0, 0, 0, steel(ArmourTier::None)), (1, 0, 0, steel(ArmourTier::None)), (2, 0, 0, steel(ArmourTier::None)), (3, 0, 0, steel(ArmourTier::Mm5))]);
    let mut bp = blueprint(cells, 0);
    bp.parts.push(place("box", 3, 0, 0)); // over the armoured cell: 300 + 10 + 1000 kg
    let mut tank = place("tank", 0, 1, 0);
    tank.load = Some(0.5); // 500 L of fuel
    bp.parts.push(tank);
    let mut ballast = place("ballast", 3, 1, 0);
    ballast.load = Some(f64::NAN); // bad data counts as full
    bp.parts.push(ballast);
    bp.parts.push(place("mag", 2, 1, 0));
    bp.parts.push(place("berth", 1, 2, 0));
    bp.parts.push(place("nonesuch", 9, 9, 9));
    let st = compute_stats(&bp, &cat);
    let light = 300.0 + 10.0 + 1000.0 + 200.0 + 50.0 + 300.0 + 100.0;
    assert!(close(st.light_t, light / 1000.0), "{}", st.light_t);
    assert!(close(st.tonnage_t, (light + 500.0 * FUEL_KG_PER_L + 2000.0 + 700.0) / 1000.0), "{}", st.tonnage_t);
    assert!(close(st.fuel_l, 500.0));
    assert_eq!(st.cost, 4 + 10 + 5 + 1 + 30 + 5); // round(3 + 0.5) + parts
    assert_eq!(st.part_count, 6);
    assert_eq!(st.berths, 4);
    assert_eq!(st.parts_by_category.get(&crate::part::PartCategory::Hull), Some(&5));
    // 4 structure cells plus 6 part cells, one of which sits on structure.
    assert_eq!(st.cell_count, 4 + 6 - 1);
    assert_eq!(st.height_m, 1.5);
}

#[test]
fn hostile_runs_are_clipped_and_cheap() {
    let cat = catalogue();
    let run = |x: i16, len: u16| CellRun { x, len, cell: steel(ArmourTier::None) };
    let cells = CellRows(vec![CellRow { y: 0, z: 0, runs: vec![run(-100, u16::MAX), run(i16::MAX, u16::MAX)] }, CellRow { y: -4, z: 0, runs: vec![run(0, 10)] }, CellRow { y: i16::MAX, z: i16::MAX, runs: vec![run(0, 10)] }]);
    let mut bp = blueprint(cells, 0);
    bp.parts.push(place("box", i16::MAX, i16::MAX, i16::MAX));
    let st = compute_stats(&bp, &cat);
    assert!(st.tonnage_t.is_finite() && st.length_m.is_finite());
    // The clipped run covers x = 0..384 at row (0, 0).
    assert!(st.cell_count >= 384);
}

#[test]
fn deterministic_and_order_independent_of_storage() {
    let cat = catalogue();
    let a = blueprint(shell_box(12, 4, 4, 0), 1);
    let mut b = a.clone();
    b.cells.0.reverse();
    assert_eq!(compute_stats(&a, &cat), compute_stats(&b, &cat));
}

#[test]
fn refit_matching_and_pricing() {
    let cat = catalogue();
    let base = blueprint(shell_box(12, 4, 4, 0), 1);
    let mut before = base.clone();
    before.parts = vec![place("box", 2, 1, 1), place("gun", 5, 4, 1), place("mag", 3, 1, 1)];
    let mut after = base.clone();
    let mut relabelled = place("box", 2, 1, 1);
    relabelled.label = Some("Store".into());
    // box unchanged (label only), gun moved, mag removed, tank and berth added.
    after.parts = vec![place("gun", 6, 4, 1), relabelled, place("tank", 3, 2, 1), place("berth", 4, 1, 1)];
    let diff = diff_blueprints(&before, &after, &cat);
    assert_eq!(diff.changes.len(), 4);
    assert!(matches!(&diff.changes[0], PartChange::Moved { before, after } if before.pos.x == 5 && after.pos.x == 6));
    assert!(matches!(&diff.changes[1], PartChange::Removed(p) if p.part.as_str() == "mag"));
    assert!(matches!(&diff.changes[2], PartChange::Added(p) if p.part.as_str() == "tank"));
    assert_eq!(diff.structure_delta_t, 0.0);
    let mut store = BTreeMap::new();
    store.insert(crate::ids::PartId::new("tank"), 1u16);
    let cost = refit_cost(&diff, &cat, &store);
    // Swaps: 1 moved + max(2 added, 1 removed) = 3, at 2 h. Tank from the store, berth at full price.
    assert_eq!((cost.salvage, cost.hours), (5, 6));
    assert_eq!(cost.from_store.get(&crate::ids::PartId::new("tank")), Some(&1));
}

#[test]
fn refit_structure_hours_and_salvage() {
    let cat = catalogue();
    let before = blueprint(rows(&[(0, 0, 0, steel(ArmourTier::None))]), 0);
    // 100 new cells: exactly 10 t of work, one hour.
    let mut cells: Vec<_> = (0..101).map(|x| (x as i16, 0, 0, steel(ArmourTier::None))).collect();
    let after = blueprint(rows(&cells), 0);
    let diff = diff_blueprints(&before, &after, &cat);
    assert!(close(diff.structure_delta_t, 10.0));
    assert_eq!(diff.structure_salvage, 100);
    let cost = refit_cost(&diff, &cat, &BTreeMap::new());
    assert_eq!((cost.salvage, cost.hours), (100, 1));
    // One more cell tips it to a second hour.
    cells.push((101, 0, 0, steel(ArmourTier::None)));
    let diff = diff_blueprints(&before, &blueprint(rows(&cells), 0), &cat);
    assert_eq!(refit_cost(&diff, &cat, &BTreeMap::new()).hours, 2);
    // Up-armouring 10 cells from 5 to 15 mm: 100 kg of work, 5 S; removing armour is free.
    let thin: Vec<_> = (0..10).map(|x| (x as i16, 0, 0, steel(ArmourTier::Mm5))).collect();
    let thick: Vec<_> = (0..10).map(|x| (x as i16, 0, 0, steel(ArmourTier::Mm15))).collect();
    let up = diff_blueprints(&blueprint(rows(&thin), 0), &blueprint(rows(&thick), 0), &cat);
    assert!(close(up.structure_delta_t, 0.1));
    assert_eq!(up.structure_salvage, 5);
    let down = diff_blueprints(&blueprint(rows(&thick), 0), &blueprint(rows(&thin), 0), &cat);
    assert_eq!(down.structure_salvage, 0);
    // Paint alone is free.
    let mut painted = thin.clone();
    painted[0].3.paint = 3;
    let p = diff_blueprints(&blueprint(rows(&thin), 0), &blueprint(rows(&painted), 0), &cat);
    assert_eq!((p.structure_delta_t, p.structure_salvage), (0.0, 0));
}

#[test]
fn starter_scale_hull_is_quick() {
    let cat = crate::catalogue::catalogue();
    let bp = crate::blueprint::io::new_blueprint("x", 90.0);
    let t = std::time::Instant::now();
    let st = compute_stats(&bp, cat);
    assert!(st.tonnage_t > 0.0 && st.length_m <= 90.0);
    assert!(t.elapsed().as_millis() < 500);
}
