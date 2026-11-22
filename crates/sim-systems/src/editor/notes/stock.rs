//! The gauges, Ship's Notes and every Suggest on the six stock starter hulls with the embedded catalogue:
//! nothing panics, every value is finite, every offered Fix and Suggest applies, results repeat, and a
//! Suggest once accepted is not offered again. Facts about the starters themselves belong to their owners and
//! are not asserted here.
use crate::editor::gauges::{all_gauges, blind_sectors, vulnerability_heatmap, weapon_arcs, GaugeStrip};
use crate::editor::notes::{apply_edit, ship_notes};
use crate::editor::suggest::{mutated_variant, suggest, SuggestKind};
use crate::prelude::*;
use crate::ship::ShipDesign;
use content::blueprint::Blueprint;

const STARTERS: [&str; 6] = ["starters/patrol15.bp.ron", "starters/fac22.bp.ron", "starters/insertion26.bp.ron", "starters/corvette40.bp.ron", "starters/corvette60.bp.ron", "starters/destroyer90.bp.ron"];

/// Suggestions that settle in one press. Bulkheads may split again and a surplus generator may be followed by
/// another; both are legitimate repeat offers.
const SETTLES: [SuggestKind; 6] = [SuggestKind::BallastAndTrim, SuggestKind::ArmourLayout, SuggestKind::FuelLoad, SuggestKind::SensorPlacement, SuggestKind::Aggression, SuggestKind::EmconTriggerRange];

fn design(bp: &Blueprint, cat: &Catalogue) -> ShipDesign {
    let prefab = Arc::new(sim_core::vessel::build_prefab(bp, cat));
    ShipDesign::build(bp, prefab, cat, &content::ruleset::Ruleset::default())
}

fn finite_strip(g: &GaugeStrip) -> bool {
    let f = &g.float;
    [f.displacement_t, f.reserve, f.draft_m, f.gm_m, f.range_of_stability_deg, g.speed.top_speed_kn, g.speed.turning_circle_m, g.power.generation_kw, g.power.combat_demand_kw, g.endurance.range_km, g.punch.weight_of_fire_kg_per_min, g.sight.antenna_m, g.resilience.systems_surviving_hit]
        .iter().all(|v| v.is_finite())
        && g.sight.horizon_km.iter().chain(&g.sight.radar_km).chain(&g.sight.visual_km).all(|v| v.is_finite())
        && g.sight.coverage.len() == 36
}

#[test]
fn starters_gauge_note_and_suggest_cleanly() {
    let cat = content::catalogue::catalogue();
    for path in STARTERS {
        let bp = content::campaign::load::stock_blueprint(path).unwrap_or_else(|e| panic!("{path}: {e:?}"));
        let d = design(&bp, cat);
        let g = all_gauges(&bp, &d, 400.0, 120.0, cat);
        assert!(finite_strip(&g), "{path}: {g:?}");
        for m in &d.mounts {
            for s in weapon_arcs(&d, m.slot, cat) { assert!(s.from_deg.is_finite() && s.to_deg.is_finite(), "{path}: {s:?}"); }
        }
        for p in &d.prefab.parts { let _ = blind_sectors(&d, p.slot, cat); }
        assert!(!vulnerability_heatmap(&d, cat).is_empty(), "{path}: heatmap");
        let notes = ship_notes(&bp, &d, &g, None, &[], cat);
        for n in &notes {
            if let Some(f) = &n.fix { assert!(apply_edit(&bp, f, cat).is_some(), "{path}: fix of {:?} applies", n.kind); }
        }
        assert_eq!(notes, ship_notes(&bp, &d, &g, None, &[], cat), "{path}: notes repeat");
        for k in SuggestKind::ALL {
            let Some(s) = suggest(k, &bp, &d, cat) else { continue };
            let after = apply_edit(&bp, &s.edit, cat).unwrap_or_else(|| panic!("{path}: {k:?} applies"));
            let d2 = design(&after, cat);
            assert!(finite_strip(&all_gauges(&after, &d2, 400.0, 120.0, cat)), "{path}: after {k:?}");
            if SETTLES.contains(&k) {
                let again = suggest(k, &after, &d2, cat);
                assert!(again.is_none(), "{path}: {k:?} settles after one press, was {:?}, then {:?}", s.text, again.map(|a| a.text));
            }
        }
    }
}

#[test]
fn mutated_starters_never_panic_the_editor() {
    let cat = content::catalogue::catalogue();
    for path in ["starters/fac22.bp.ron", "starters/corvette40.bp.ron"] {
        let bp = content::campaign::load::stock_blueprint(path).unwrap_or_else(|e| panic!("{path}: {e:?}"));
        for seed in 0..6u64 {
            let v = mutated_variant(&bp, seed, cat);
            assert_eq!(v, mutated_variant(&bp, seed, cat), "{path} seed {seed}: deterministic");
            let d = design(&v, cat);
            let g = all_gauges(&v, &d, 400.0, 0.0, cat);
            assert!(finite_strip(&g), "{path} seed {seed}");
            for n in ship_notes(&v, &d, &g, None, &[], cat) {
                if let Some(f) = &n.fix { assert!(apply_edit(&v, f, cat).is_some(), "{path} seed {seed}: {:?}", n.kind); }
            }
            for k in SuggestKind::ALL { let _ = suggest(k, &v, &d, cat); }
        }
    }
}
