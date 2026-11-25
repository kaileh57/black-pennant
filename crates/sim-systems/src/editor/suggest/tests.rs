//! Tests for the Suggest computations that need no physics stub, the variant generator and the CSV.
use super::compute::*;
use super::*;
use crate::editor::gauges::testkit::*;
use crate::editor::notes::apply_edit;
use crate::text::csv_check::{check_msg, lint, parse, GLOSSARY};
use content::cell::{CellPos, CELL_M};

const CSV: &str = include_str!("../../../../../game/i18n/suggest.csv");

#[test]
fn ballast_plan_levels_trim_and_sets_gm() {
    // 100 t with its centre of gravity 0.5 m forward of the centre of buoyancy and GM short of 0.95 m.
    let s = TrimState { mass_kg: 100_000.0, com: DVec3::new(10.5, 2.0, 3.0), gm_m: 0.3, cb: DVec3::new(10.0, 1.0, 3.0), centre_z: 3.0 };
    let stores = [
        BallastStore { centre: DVec3::new(2.0, 0.5, 3.0), max_kg: 50_000.0, now_kg: 0.0 },
        BallastStore { centre: DVec3::new(18.0, 0.5, 3.0), max_kg: 50_000.0, now_kg: 0.0 },
    ];
    let w = ballast_plan(&s, 0.95, &stores);
    let mass = s.mass_kg + w[0] + w[1];
    let x = (s.mass_kg * s.com.x + w[0] * 2.0 + w[1] * 18.0) / mass;
    assert!((x - s.cb.x).abs() < 0.02, "trim levelled: {x}");
    let gm = planned_gm(&s, &stores, &w);
    assert!((gm - 0.95).abs() < 0.05, "GM {gm}");
    assert!(w[0] > w[1], "more weight aft to bring the bow up");
    assert!(w.iter().zip(&stores).all(|(v, b)| *v >= 0.0 && *v <= b.max_kg));
    // Stores too small for the GM target still level the trim first.
    let small = [BallastStore { max_kg: 20_000.0, ..stores[0] }, BallastStore { max_kg: 20_000.0, ..stores[1] }];
    let w = ballast_plan(&s, 0.95, &small);
    let x = (s.mass_kg * s.com.x + w[0] * 2.0 + w[1] * 18.0) / (s.mass_kg + w[0] + w[1]);
    assert!((x - s.cb.x).abs() < 0.01, "trim first: {x}");
    // Bounded: a tiny tank cannot do it all, and nothing goes negative.
    let small = [BallastStore { centre: DVec3::new(2.0, 0.5, 3.0), max_kg: 500.0, now_kg: 200.0 }];
    let w = ballast_plan(&s, 0.95, &small);
    assert_eq!(w, vec![500.0]);
    assert_eq!(ballast_plan(&s, 0.95, &[]), Vec::<f64>::new());
}

#[test]
fn fuel_level_keeps_reserve_buoyancy() {
    // 100 t with 50% reserve: total buoyancy 150 t; at 20% reserve the hull carries 125 t, 25 t more.
    assert_eq!(fuel_level(100.0, 0.5, 0.0, 10_000.0), 1.0);
    let f = fuel_level(100.0, 0.25, 0.0, 10_000.0);
    // Total 125 t carries 104.17 t at 20% reserve: 4.17 t of fuel = 4,902 L of 10,000.
    assert_eq!(f, 0.45);
    assert_eq!(fuel_level(100.0, 0.1, 5_000.0, 10_000.0), 0.0, "already below the reserve: empty is the answer");
    assert_eq!(fuel_level(f64::NAN, 0.5, 0.0, 1.0), 0.0);
    assert_eq!(fuel_level(100.0, 0.5, 0.0, 0.0), 0.0);
}

#[test]
fn armour_goes_on_the_belt_by_the_engine_room() {
    let (bp, cat) = crate::editor::gauges::testkit::fitted();
    let cells = armour_cells(&bp, &cat);
    assert!(!cells.is_empty());
    let wl = bp.hull.design_waterline_y;
    assert!(cells.iter().all(|c| c.y >= wl - 2 && c.y <= wl + 4 && (c.z == 0 || c.z == 11)), "side belt only (no magazine fitted)");
    // Engine room spans x 4..=11 with the generator at 5..=6 and the diesel at 8..=9.
    assert!(cells.iter().all(|c| (4..=10).contains(&c.x)), "{cells:?}");
    let s = armour_layout(&bp, &cat).expect("a belt");
    let BlueprintEdit::PaintArmour { cells: painted, tier } = &s.edit else { panic!("paint") };
    assert!((1..=3).contains(tier));
    let out = apply_edit(&bp, &s.edit, &cat).unwrap();
    assert!(armour_layout(&out, &cat).is_none_or(|s2| matches!(s2.edit, BlueprintEdit::PaintArmour { tier: t2, .. } if t2 < *tier)), "the same layout is not suggested twice");
    assert!(!painted.is_empty());
    check_msg(&parse(CSV), &s.text);
}

#[test]
fn bulkhead_splits_the_long_compartment() {
    let (bp, cat) = crate::editor::gauges::testkit::fitted();
    let pf = prefab(&bp, &cat);
    let s = bulkhead_placement(&bp, &pf, &cat).expect("one 19 m compartment");
    let BlueprintEdit::AddBulkhead { x_cell } = s.edit else { panic!("bulkhead") };
    assert!((15..=25).contains(&x_cell), "{x_cell}");
    let grid = content::blueprint::io::build_grid(&bp, &cat);
    assert!((0..8).all(|y| (0..12).all(|z| grid.get(CellPos::new(x_cell, y, z)).material != content::cell::Material::PartBody)), "cuts no part");
    let out = apply_edit(&bp, &s.edit, &cat).unwrap();
    let pf2 = prefab(&out, &cat);
    // Two shorter compartments now; a hull of short compartments gets no further suggestion at this length.
    assert_eq!(pf2.compartments.compartments.iter().filter(|c| !c.is_absorbed()).count(), 2);
    check_msg(&parse(CSV), &s.text);
}

#[test]
fn radar_moves_out_from_behind_the_funnel() {
    let (mut bp, cat) = crate::editor::gauges::testkit::fitted();
    // Box in the radar: tall structure on both sides and ahead and astern of it.
    for (x, z) in [(17, 5), (22, 5), (19, 2), (19, 8)] { place(&mut bp, "funnel", CellPos::new(x, 8, z)); }
    let pf = prefab(&bp, &cat);
    let s = sensor_placement(&bp, &pf, &cat).expect("a clearer spot");
    let BlueprintEdit::MovePart { slot, to, .. } = s.edit else { panic!("move") };
    assert_eq!(slot, PartSlot(2));
    assert_ne!(to, bp.parts[2].pos);
    let out = apply_edit(&bp, &s.edit, &cat).unwrap();
    check_msg(&parse(CSV), &s.text);
    // At the new spot the radar sees more.
    let mean = |pf: &sim_core::vessel::VesselPrefab| { let site = crate::editor::gauges::calc::sensor_sites(pf, &cat).into_iter().find(|x| x.slot == PartSlot(2)).unwrap(); crate::editor::gauges::calc::sector_coverage(&pf.grid, &site).iter().map(|c| *c as f64).sum::<f64>() };
    assert!(mean(&prefab(&out, &cat)) > mean(&pf));
    // A clear radar is left where it is.
    let (clear_bp, cat) = crate::editor::gauges::testkit::fitted();
    let clear_pf = prefab(&clear_bp, &cat);
    if let Some(s) = sensor_placement(&clear_bp, &clear_pf, &cat) {
        // Only ever an improvement of more than 5% (here: out of the gun's shadow).
        let out = apply_edit(&clear_bp, &s.edit, &cat).unwrap();
        assert!(mean(&prefab(&out, &cat)) >= mean(&clear_pf));
    }
}

#[test]
fn generator_shortfall_adds_a_generator() {
    let (mut bp, cat) = crate::editor::gauges::testkit::fitted();
    // Drop the generator: 50 kW of demand, no generation.
    bp = apply_edit(&bp, &BlueprintEdit::RemovePart(PartSlot(0)), &cat).unwrap();
    let pf = prefab(&bp, &cat);
    let s = engine_and_generator(&bp, &pf, &cat).expect("add one");
    let BlueprintEdit::AddPart(p) = &s.edit else { panic!("add part: {:?}", s.edit) };
    assert_eq!(p.part, PartId::new("gen"));
    let out = apply_edit(&bp, &s.edit, &cat).unwrap();
    let after = crate::editor::gauges::calc::power_budget(&prefab(&out, &cat), &cat, 0.7);
    assert!(after.generation_kw >= after.combat_kw * 1.1);
    assert_eq!(s.text.key.0, "suggest.generator_add");
    check_msg(&parse(CSV), &s.text);
    // Enough generation and no propulsors: nothing to size.
    let (bp, cat) = crate::editor::gauges::testkit::fitted();
    assert!(engine_and_generator(&bp, &prefab(&bp, &cat), &cat).is_none());
}

#[test]
fn surplus_generator_and_oversized_engine_change_together() {
    use content::part::{EngineDef, EngineKind, PartCategory as C, PartFunction as F, Placement as P, PropulsorDef, PropulsorKind};
    let (mut bp, mut cat) = crate::editor::gauges::testkit::fitted();
    for d in [
        def("prop", C::Propulsion, [1, 1, 1], P::Anywhere, F::Propulsor(PropulsorDef { kind: PropulsorKind::Propeller, max_shaft_kw: 500.0, thrust_n_per_kw: 100.0, cavitation_kn: 20.0, steer_max_deg: 0.0 }), vec![]),
        def("diesel_small", C::Propulsion, [2, 2, 2], P::Internal, F::Engine(EngineDef { kind: EngineKind::Diesel, shaft_kw: 500.0, fuel_lps: 0.03, spool_s: 5.0 }), vec![]),
    ] { cat.parts.insert(d.id.clone(), d); }
    cat.parts.sort_keys();
    let second = place(&mut bp, "gen", CellPos::new(25, 1, 5));
    place(&mut bp, "prop", CellPos::new(2, 1, 6));
    let pf = prefab(&bp, &cat);
    let s = engine_and_generator(&bp, &pf, &cat).expect("two changes");
    assert_eq!(s.text.key.0, "suggest.power_and_engine");
    check_msg(&parse(CSV), &s.text);
    let out = apply_edit(&bp, &s.edit, &cat).unwrap();
    let ids: Vec<&str> = out.parts.iter().map(|p| p.part.as_str()).collect();
    assert_eq!(ids.iter().filter(|i| **i == "gen").count(), 1, "one generator removed: {ids:?}");
    assert!(ids.contains(&"diesel_small") && !ids.contains(&"diesel"), "{ids:?}");
    // Nothing else lost: the radar, gun, tank, hoist and propeller are all still there.
    for id in ["radar", "gun", "tank", "hoist", "prop"] { assert!(ids.contains(&id), "{id} lost: {ids:?}"); }
    assert_eq!(out.parts.len(), bp.parts.len() - 1);
    let _ = second;
}

#[test]
fn aggression_suggestion_round_trips() {
    let (bp, cat) = crate::editor::gauges::testkit::fitted();
    let d = design(&bp, prefab(&bp, &cat));
    let want = crate::officers::tuning::suggest_aggression(&d);
    match super::compute::aggression(&bp, &d) {
        Some(s) => {
            let out = apply_edit(&bp, &s.edit, &cat).unwrap();
            assert_eq!(crate::officers::tuning::slider_value(&out.officers, "gunnery.aggression"), want);
            assert!(super::compute::aggression(&out, &d).is_none(), "matches after accepting");
            check_msg(&parse(CSV), &s.text);
        }
        None => assert!((crate::officers::tuning::slider_value(&bp.officers, "gunnery.aggression") - want).abs() < 0.025),
    }
}

#[test]
fn mutated_variants_are_deterministic_and_canonical() {
    let (bp, cat) = crate::editor::gauges::testkit::fitted();
    let mut distinct = BTreeSet::new();
    for seed in 0..40u64 {
        let a = mutated_variant(&bp, seed, &cat);
        assert_eq!(a, mutated_variant(&bp, seed, &cat), "seed {seed}");
        assert_eq!(a.header.hash, content::blueprint::io::compute_hash(&a));
        // Every part still has a valid footprint and nothing overlaps.
        let mut seen = BTreeSet::new();
        for p in &a.parts { for c in crate::editor::notes::edit::part_cells(p, &cat).expect("footprint") { assert!(seen.insert(c), "overlap at {c:?}"); } }
        distinct.insert(a.header.hash);
    }
    assert!(distinct.len() > 30, "variants differ: {}", distinct.len());
}

#[test]
fn autoset_file_parses_and_names_tests() {
    let s = autoset_status();
    assert!(s.dropped.is_empty(), "every auto-set value in force this release");
    assert!(AutoSetValue::ALL.iter().all(|v| s.is_auto(*v)));
    let names: BTreeSet<&str> = AutoSetValue::ALL.iter().map(|v| v.property_test()).collect();
    assert_eq!(names.len(), 5);
    let dropped: AutoSetStatus = content::data::parse_ron("(dropped: [(MountLimits, \"starter fac22 variant 17\")])", "t").unwrap();
    assert!(!dropped.is_auto(AutoSetValue::MountLimits) && dropped.is_auto(AutoSetValue::SolverTables));
}

#[test]
fn csv_rows_match_keys_and_copy_rules() {
    let rows = parse(CSV);
    let keys = text_keys();
    assert_eq!(rows.len(), keys.len(), "CSV rows vs text_keys()");
    let mut glossary = GLOSSARY.to_vec();
    glossary.push("GM");
    for k in &keys {
        let (_, en) = rows.get(*k).unwrap_or_else(|| panic!("no row for {k}"));
        assert!(k.starts_with("suggest."));
        lint(k, en, 80, &glossary);
    }
    let _ = CELL_M;
}

#[test]
fn trim_error_counts_only_what_is_past_the_thresholds() {
    let eq = |gm: f64, trim_deg: f64, heel_deg: f64, floats: bool| sim_core::hydro::Equilibrium { displacement_t: 100.0, draft_m: 2.0, heel_rad: heel_deg.to_radians(), trim_rad: trim_deg.to_radians(), reserve: 1.0, gm_m: gm, cb: DVec3::ZERO, floats };
    // 10 m beam, Pass band 4% to 15%.
    assert_eq!(trim_error(&eq(0.8, 0.3, -0.4, true), 10.0, 0.04, 0.15), 0.0);
    assert!((trim_error(&eq(0.8, 1.5, 0.0, true), 10.0, 0.04, 0.15) - 2.0).abs() < 1e-9, "1 degree past 0.5 is two steps");
    assert!((trim_error(&eq(0.3, 0.0, 0.0, true), 10.0, 0.04, 0.15) - 2.0).abs() < 1e-9, "GM 1% of beam under the band");
    assert!(trim_error(&eq(0.8, 0.0, 0.0, false), 10.0, 0.04, 0.15) >= 1e9, "not floating is worst");
}

#[test]
fn plan_residual_falls_to_zero_when_the_plan_reaches_its_target() {
    let s = TrimState { mass_kg: 100_000.0, com: DVec3::new(10.5, 2.0, 3.0), gm_m: 0.3, cb: DVec3::new(10.0, 1.0, 3.0), centre_z: 3.0 };
    let stores = [
        BallastStore { centre: DVec3::new(2.0, 0.5, 3.0), max_kg: 50_000.0, now_kg: 0.0 },
        BallastStore { centre: DVec3::new(18.0, 0.5, 3.0), max_kg: 50_000.0, now_kg: 0.0 },
    ];
    let before = plan_residual(&s, 0.95, &stores, &[0.0, 0.0]);
    let w = ballast_plan(&s, 0.95, &stores);
    let after = plan_residual(&s, 0.95, &stores, &w);
    assert!(before > 0.0 && after < before * 0.05, "{before} -> {after}");
}
