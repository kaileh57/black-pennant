//! Unit `data-act2`: the Combine's Act 2 classes float the way the simulation will float them. Every class row
//! in `enemy_classes/act2.ron` is measured on the editor's float gauge (sim-core's calm-water equilibrium and GZ
//! curve) as loaded: it floats, sits upright and nearly level, is not tender, and keeps a usable range of
//! stability, so no Combine ship lists at anchor or rolls over in a turn.
//!
//! The build sheets that make these hulls are `crates/content/tests/combine/classes_act2.rs`; regenerate with
//! `cargo test -p content --test combine_act2 -- --ignored write_combine_act2` after changing one.
use content::catalogue::Catalogue;
use content::enums::Act;
use sim_systems::editor::{gauges, Check};

/// Largest heel at rest, degrees.
const MAX_HEEL_DEG: f64 = 1.0;
/// Largest trim at rest, degrees.
const MAX_TRIM_DEG: f64 = 1.5;
/// Smallest angle at which the righting arm vanishes, degrees.
const MIN_RANGE_OF_STABILITY_DEG: f64 = 35.0;
/// Classes known to be tender and waiting for ballast in their build sheets. Remove an entry when its sheet is
/// fixed: the test fails on an entry that is no longer tender. `class.premium` left the list when its SAM cells
/// became watertight and sealed the space under them (it read 3.6% of its beam while that space stood open).
const TENDER_PENDING_BALLAST: [&str; 0] = [];

#[test]
fn every_act2_class_floats_upright_and_stable() {
    let cat = Catalogue::load_embedded().expect("catalogue");
    let campaign = content::campaign::load::load_embedded(&cat).expect("campaign data");
    let rows: Vec<_> = campaign.classes.values().filter(|c| c.act == Act::Act2).collect();
    assert!(rows.len() >= 10, "{} Act 2 classes", rows.len());
    let mut failures = Vec::new();
    for row in rows {
        let bp = content::campaign::load::stock_blueprint(&row.blueprint).unwrap_or_else(|e| panic!("{}: {e}", row.id));
        let prefab = sim_core::vessel::build_prefab(&bp, &cat);
        let f = gauges::float_gauge(&prefab, 0.0, 2.0, &cat);
        let id = row.id.as_str();
        if !f.floats { failures.push(format!("{id}: does not float")); continue; }
        if f.heel_deg.abs() > MAX_HEEL_DEG { failures.push(format!("{id}: lists {:+.2} deg", f.heel_deg)); }
        if f.trim_deg.abs() > MAX_TRIM_DEG { failures.push(format!("{id}: trims {:+.2} deg", f.trim_deg)); }
        // Caution on the stiff side is a merchant's high GM; on the tender side it is a warship that rolls over.
        let tender = f.check == Check::Fail || (f.check == Check::Caution && f.gm_over_beam < cat.tuning.stability.gm_caution_low);
        let pending = TENDER_PENDING_BALLAST.contains(&id);
        if tender && !pending { failures.push(format!("{id}: tender, GM {:.2} m ({:.3} of the beam)", f.gm_m, f.gm_over_beam)); }
        if pending && !tender { failures.push(format!("{id}: no longer tender, take it off TENDER_PENDING_BALLAST")); }
        if pending && f.gm_m <= 0.0 { failures.push(format!("{id}: GM {:.2} m, it will not stand upright", f.gm_m)); }
        if f.range_of_stability_deg < MIN_RANGE_OF_STABILITY_DEG { failures.push(format!("{id}: range of stability {:.0} deg", f.range_of_stability_deg)); }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
