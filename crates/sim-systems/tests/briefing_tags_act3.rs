//! Unit `data-act3`: the Act 3 briefings' constraint tags (DESIGN.md L780, L800-803) against the ships a player
//! has in the act, measured the way the editor measures them: the float, speed and endurance gauges and
//! sim-core's projected areas at the design draft.
//!
//! The 60 m raiding corvette starter passes every draft limit, speed minimum and range minimum as built. Every
//! cross-section limit fails it, because a stealth insertion wants a specialised ship, and passes the same hull
//! rebuilt with composite topsides. If a hull, part or material change moves these numbers, the tags in
//! crates/content/data/operations/act3.ron need recalibrating, and so does their header.
use content::blueprint::Blueprint;
use content::campaign::load::stock_blueprint;
use content::campaign::{CampaignData, ConstraintTag, OperationDef};
use content::catalogue::Catalogue;
use content::cell::Material;
use content::enums::Act;
use sim_systems::editor::gauges;

const STARTER: &str = "starters/corvette60.bp.ron";

#[derive(Debug)]
struct Measured { draft_m: f64, top_speed_kn: f64, range_km: f64, worst_rcs_m2: f64 }

fn measure(bp: &Blueprint, cat: &Catalogue) -> Measured {
    let prefab = sim_core::vessel::build_prefab(bp, cat);
    let float = gauges::float_gauge(&prefab, Act::Act3.hull_cap_t(), 2.0, cat);
    assert!(float.floats, "{}: floats", bp.header.name);
    let speed = gauges::speed_gauge(&prefab, cat);
    let range = gauges::endurance_gauge(&prefab, cat);
    let worst_rcs_m2 = sim_core::vessel::projected_areas_prefab(&prefab, cat).rcs_m2.iter().map(|&a| a as f64).fold(0.0, f64::max);
    Measured { draft_m: float.draft_m, top_speed_kn: speed.top_speed_kn, range_km: range.range_km, worst_rcs_m2 }
}

/// The starter rebuilt in the editor with every metal cell at or above the design waterline in composite: the
/// stealth refit an RCS-limited briefing asks for. Parts and the hull shape are unchanged.
fn composite_topsides(bp: &Blueprint) -> Blueprint {
    let mut out = bp.clone();
    let wl = bp.hull.design_waterline_y as i32;
    for row in out.cells.0.iter_mut().filter(|r| r.y as i32 >= wl) {
        for run in &mut row.runs {
            if matches!(run.cell.material, Material::Steel | Material::Aluminium) { run.cell.material = Material::Composite; }
        }
    }
    out
}

fn load() -> (Catalogue, CampaignData) {
    let cat = Catalogue::load_embedded().expect("catalogue");
    let campaign = content::campaign::load::load_embedded(&cat).expect("campaign data");
    (cat, campaign)
}

fn act3_ops(c: &CampaignData) -> impl Iterator<Item = &OperationDef> { c.operations.values().filter(|o| o.act == Act::Act3) }

#[test]
fn the_act3_starter_measures_what_the_briefings_assume() {
    let (cat, _) = load();
    let bp = stock_blueprint(STARTER).unwrap();
    let m = measure(&bp, &cat);
    if std::env::var_os("PRINT_TAGS").is_some() { println!("stock {m:?}\ncomposite topsides {:?}", measure(&composite_topsides(&bp), &cat)); }
    // The operations file's header quotes these numbers.
    // The header once quoted 2.7 m, read off 2 m buoyancy blocks with one fill fraction each; taken layer by
    // layer the hull draws what its cells do, about 2.2 m for the retuned starter. Every draft tag (4.0 m and
    // up) passes it either way.
    assert!((2.0..=3.1).contains(&m.draft_m), "draft {} m", m.draft_m);
    assert!((27.0..=32.0).contains(&m.top_speed_kn), "top speed {} kn", m.top_speed_kn);
    assert!((1_800.0..=2_700.0).contains(&m.range_km), "range {} km", m.range_km);
    assert!((360.0..=480.0).contains(&m.worst_rcs_m2), "worst-aspect RCS {} m^2", m.worst_rcs_m2);
}

#[test]
fn every_act3_tag_is_met_by_the_starter_or_by_a_specialised_ship() {
    let (cat, c) = load();
    let starter_bp = stock_blueprint(STARTER).unwrap();
    let starter = measure(&starter_bp, &cat);
    let stealth = measure(&composite_topsides(&starter_bp), &cat);
    let mut rcs_tags = 0;
    for o in act3_ops(&c) {
        let id = o.id.as_str();
        for t in &o.tags {
            match *t {
                ConstraintTag::DraftLimit { max_m } => assert!(max_m >= starter.draft_m + 0.5, "{id}: the starter floats at {:.2} m", starter.draft_m),
                ConstraintTag::MinSpeed { kn } => assert!(starter.top_speed_kn >= kn + 1.0, "{id}: the starter makes {:.1} kn", starter.top_speed_kn),
                ConstraintTag::Range { min_km } => assert!(starter.range_km >= 2.0 * min_km, "{id}: the starter runs {:.0} km", starter.range_km),
                ConstraintTag::RcsLimit { max_m2 } => {
                    rcs_tags += 1;
                    assert!(starter.worst_rcs_m2 > max_m2, "{id}: the stock starter ({:.0} m^2) should need a stealth refit for {max_m2} m^2", starter.worst_rcs_m2);
                    assert!(stealth.worst_rcs_m2 <= 0.8 * max_m2, "{id}: composite topsides ({:.0} m^2) should pass {max_m2} m^2 with margin", stealth.worst_rcs_m2);
                }
                ConstraintTag::Passengers { .. } | ConstraintTag::Requires(_) => {}
            }
        }
    }
    // Stealth work is part of the act, not all of it.
    assert!((4..=8).contains(&rcs_tags), "{rcs_tags} cross-section tags");
    // The composite refit still makes the speed the briefings ask for.
    assert!(stealth.top_speed_kn >= starter.top_speed_kn - 0.5, "{} kn", stealth.top_speed_kn);
}

/// The Liquidator duel's reference setup (L318) is the starter with officer settings only, so it meets the duel's
/// own tags.
#[test]
fn the_duel_reference_setup_meets_its_briefing() {
    let (cat, c) = load();
    let duel = act3_ops(&c).find(|o| o.reference_setup.is_some()).expect("the Act 3 reference duel");
    let m = measure(&stock_blueprint(duel.reference_setup.as_deref().unwrap()).unwrap(), &cat);
    for t in &duel.tags {
        match *t {
            ConstraintTag::DraftLimit { max_m } => assert!(m.draft_m <= max_m),
            ConstraintTag::MinSpeed { kn } => assert!(m.top_speed_kn >= kn, "{} kn", m.top_speed_kn),
            ConstraintTag::Range { min_km } => assert!(m.range_km >= min_km, "{} km", m.range_km),
            ConstraintTag::RcsLimit { max_m2 } => assert!(m.worst_rcs_m2 <= max_m2, "{} m^2", m.worst_rcs_m2),
            ConstraintTag::Passengers { .. } | ConstraintTag::Requires(_) => panic!("{}: the reference setup carries no special-operations kit", duel.id),
        }
    }
}
