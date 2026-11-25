//! Unit `data-act3`: the Combine's Act 3 classes float the way the simulation will float them. Every class row
//! in `enemy_classes/act3.ron` is measured on the editor's float gauge (sim-core's calm-water equilibrium and GZ
//! curve) as loaded: it floats, sits upright and nearly level, is not tender, and keeps a usable range of
//! stability, so no Combine ship lists at anchor or rolls over in a turn.
//!
//! The two submarine classes are held to the same rules on the surface, where they transit and where a
//! depth-charging escort first meets them; submerged, depth keeping holds them level (`submarine_act3.rs`).
//!
//! The build sheets that make these hulls are `crates/content/tests/combine/classes_act3.rs`; regenerate with
//! `cargo test -p content --test combine_act3 -- --ignored write_combine_act3` after changing one.
use content::catalogue::Catalogue;
use content::enums::Act;
use sim_systems::editor::{gauges, Check};

/// Largest heel at rest, degrees.
const MAX_HEEL_DEG: f64 = 1.0;
/// Largest trim at rest, degrees.
const MAX_TRIM_DEG: f64 = 1.5;
/// Smallest angle at which the righting arm vanishes, degrees.
const MIN_RANGE_OF_STABILITY_DEG: f64 = 35.0;

#[test]
fn every_act3_class_floats_upright_and_stable() {
    let cat = Catalogue::load_embedded().expect("catalogue");
    let campaign = content::campaign::load::load_embedded(&cat).expect("campaign data");
    let rows: Vec<_> = campaign.classes.values().filter(|c| c.act == Act::Act3).collect();
    assert!(rows.len() >= 12, "{} Act 3 classes", rows.len());
    assert!(rows.iter().filter(|c| c.submersible).count() >= 2, "both submarine classes are measured");
    let mut failures = Vec::new();
    let mut table = String::new();
    for row in rows {
        let bp = content::campaign::load::stock_blueprint(&row.blueprint).unwrap_or_else(|e| panic!("{}: {e}", row.id));
        let prefab = sim_core::vessel::build_prefab(&bp, &cat);
        let f = gauges::float_gauge(&prefab, 0.0, 2.0, &cat);
        let id = row.id.as_str();
        table += &format!("{id:28} {:7.1} t draft {:5.2} heel {:+6.2} trim {:+6.2} GM {:5.2} GM/B {:.3} range {:4.0} {:?}\n", f.displacement_t, f.draft_m, f.heel_deg, f.trim_deg, f.gm_m, f.gm_over_beam, f.range_of_stability_deg, f.check);
        if !f.floats { failures.push(format!("{id}: does not float")); continue; }
        if f.heel_deg.abs() > MAX_HEEL_DEG { failures.push(format!("{id}: lists {:+.2} deg", f.heel_deg)); }
        if f.trim_deg.abs() > MAX_TRIM_DEG { failures.push(format!("{id}: trims {:+.2} deg", f.trim_deg)); }
        // Caution on the stiff side is a merchant's high GM; on the tender side it is a warship that rolls over.
        let tender = f.check == Check::Fail || (f.check == Check::Caution && f.gm_over_beam < cat.tuning.stability.gm_caution_low);
        if tender { failures.push(format!("{id}: tender, GM {:.2} m ({:.3} of the beam)", f.gm_m, f.gm_over_beam)); }
        if f.range_of_stability_deg < MIN_RANGE_OF_STABILITY_DEG { failures.push(format!("{id}: range of stability {:.0} deg", f.range_of_stability_deg)); }
    }
    if std::env::var_os("PRINT_FLOAT").is_some() { println!("{table}"); }
    assert!(failures.is_empty(), "{failures:#?}\n{table}");
}
