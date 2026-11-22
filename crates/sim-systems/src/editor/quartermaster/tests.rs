use super::*;
use crate::editor::NoDeadline;
use content::weather::SeaState;

fn real_deps() -> SimDeps { crate::editor::splash::tests::real_deps() }
fn starter(name: &str) -> Blueprint { crate::editor::splash::canonical(&content::campaign::load::stock_blueprint(&format!("starters/{name}.bp.ron")).unwrap()) }
fn pool3() -> rayon::ThreadPool { rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap() }
fn brief(tags: Vec<ConstraintTag>) -> FitBrief {
    FitBrief { tags, weather: crate::editor::splash::basin_weather(SeaState::BEAUFORT_4, 3), approach_km: 40.0, threats: vec![ClassId::new("class.patrol_boat")], cap_t: 1_800.0 }
}
fn ruleset() -> content::ruleset::Ruleset { crate::editor::splash::basin_ruleset() }
fn crane() -> ConstraintTag { ConstraintTag::Requires(PartTag::Crane) }

#[test]
fn each_tag_has_its_curated_list() {
    assert_eq!(kit_tags_for(&ConstraintTag::DraftLimit { max_m: 1.0 }), vec![PartTag::Propulsion]);
    assert_eq!(kit_tags_for(&ConstraintTag::RcsLimit { max_m2: 1.0 }), vec![PartTag::LowSignature]);
    assert_eq!(kit_tags_for(&ConstraintTag::Range { min_km: 1.0 }), vec![PartTag::FuelStore, PartTag::Propulsion]);
    assert_eq!(kit_tags_for(&ConstraintTag::Passengers { count: 2, allowed_losses: 0 }), vec![PartTag::PassengerBerths]);
    assert_eq!(kit_tags_for(&crane()), vec![PartTag::Crane]);
    assert_eq!(kit_tags_for(&ConstraintTag::MinSpeed { kn: 1.0 }), vec![PartTag::Propulsion]);
}

#[test]
fn candidates_keep_the_shell_come_cheapest_first_and_respect_unlocks() {
    let cat = content::catalogue::catalogue();
    let bp = starter("corvette60");
    let failing = [ConstraintTag::Range { min_km: 5_000.0 }, ConstraintTag::Passengers { count: 6, allowed_losses: 0 }];
    let list = candidates(&bp, &failing, &BTreeMap::new(), None, cat, 50);
    assert!(!list.is_empty());
    assert_eq!(list, candidates(&bp, &failing, &BTreeMap::new(), None, cat, 50), "deterministic");
    assert!(list.windows(2).all(|w| w[0].cost.salvage <= w[1].cost.salvage), "cheapest first");
    for c in &list {
        assert_eq!((&c.blueprint.hull, &c.blueprint.cells), (&bp.hull, &bp.cells), "the shell never changes");
        assert!(!c.diff.changes.is_empty());
        assert_eq!(c.cost, content::stats::refit_cost(&c.diff, cat, &BTreeMap::new()));
    }
    // Some candidate fixes both tags at once (a combination).
    assert!(list.iter().any(|c| crate::editor::fitcheck::passenger_berths(&c.blueprint, cat) >= 6 && c.blueprint.kits.len() > bp.kits.len() + 1));
    assert!(candidates(&bp, &failing, &BTreeMap::new(), None, cat, 3).len() <= 3);
    assert!(candidates(&bp, &[], &BTreeMap::new(), None, cat, 50).is_empty());
    // Without passenger berths unlocked no candidate adds them, unless the store holds some.
    let unlocked: Vec<PartId> = cat.parts.keys().filter(|p| p.as_str() != "berths.passenger").cloned().collect();
    let berths = |l: &[QmCandidate]| l.iter().filter(|c| crate::editor::fitcheck::passenger_berths(&c.blueprint, cat) > 0).count();
    assert_eq!(berths(&candidates(&bp, &failing[1..], &BTreeMap::new(), Some(&unlocked), cat, 50)), 0);
    let store: BTreeMap<PartId, u16> = [(PartId::new("berths.passenger"), 1)].into();
    let from_store = candidates(&bp, &failing[1..], &store, Some(&unlocked), cat, 50);
    assert!(berths(&from_store) > 0);
    assert!(from_store.iter().any(|c| c.cost.from_store.contains_key(&PartId::new("berths.passenger"))));
}

#[test]
fn a_required_crane_is_fitted_and_offered_legal() {
    pool3().install(|| {
        let cat = content::catalogue::catalogue();
        let bp = starter("fac22");
        let b = brief(vec![ConstraintTag::DraftLimit { max_m: 5.0 }, crane()]);
        let r = quartermaster(&bp, &b, &BTreeMap::new(), None, &ruleset(), &mut real_deps, &mut NoDeadline);
        let QmResult::Refit { blueprint, diff, cost, fit } = &r else { panic!("{r:?}") };
        assert!(fit.passes() && fit.simulated, "{fit:?}");
        assert_eq!(fit.tags.len(), 2, "the tag that already passed is still checked");
        assert!(crate::editor::fitcheck::parts_with_tag(blueprint, PartTag::Crane, cat) >= 1);
        assert!(diff.changes.iter().any(|c| matches!(c, content::stats::PartChange::Added(p) if p.part.as_str() == "crane")));
        assert!(cost.salvage > 0);
        assert_eq!((&blueprint.hull, &blueprint.cells), (&bp.hull, &bp.cells));
        let ctx = content::validate::ValidationContext { ruleset: &ruleset(), available: None, enemy_class: false, complexity: None };
        assert!(content::validate::validate_blueprint(blueprint, cat, &ctx).is_ok());
        assert_eq!(r, quartermaster(&bp, &b, &BTreeMap::new(), None, &ruleset(), &mut real_deps, &mut NoDeadline), "same inputs, same answer");
    });
}

#[test]
fn it_says_when_the_design_fits_or_names_the_blocking_tag() {
    pool3().install(|| {
        let bp = starter("fac22");
        let r = quartermaster(&bp, &brief(vec![ConstraintTag::MinSpeed { kn: 10.0 }]), &BTreeMap::new(), None, &ruleset(), &mut real_deps, &mut NoDeadline);
        assert!(matches!(r, QmResult::AlreadyFits(ref f) if f.passes()), "{r:?}");
        // No kit makes a 22 m boat do 60 kn; the crane is fixable, so the speed tag blocks.
        let r = quartermaster(&bp, &brief(vec![crane(), ConstraintTag::MinSpeed { kn: 60.0 }]), &BTreeMap::new(), None, &ruleset(), &mut real_deps, &mut NoDeadline);
        assert_eq!(r, QmResult::NoLegalRefit { blocking: ConstraintTag::MinSpeed { kn: 60.0 } });
        // A cross-section no fitted kit can reach.
        let r = quartermaster(&bp, &brief(vec![ConstraintTag::RcsLimit { max_m2: 1.0 }]), &BTreeMap::new(), None, &ruleset(), &mut real_deps, &mut NoDeadline);
        assert_eq!(r, QmResult::NoLegalRefit { blocking: ConstraintTag::RcsLimit { max_m2: 1.0 } });
        // A tonnage cap the refit would break makes it illegal.
        let mut tight = brief(vec![crane()]);
        tight.cap_t = content::stats::compute_stats(&bp, content::catalogue::catalogue()).tonnage_t + 0.01;
        let r = quartermaster(&bp, &tight, &BTreeMap::new(), None, &ruleset(), &mut real_deps, &mut NoDeadline);
        assert_eq!(r, QmResult::NoLegalRefit { blocking: crane() });
    });
}

/// Quartermaster wall time on starters and briefs (L893: 5 s total).
#[test]
#[ignore]
fn qm_probe() {
    pool3().install(|| {
        for name in ["fac22", "corvette60", "destroyer90"] {
            let bp = starter(name);
            let cat = content::catalogue::catalogue();
            let screen = crate::editor::fitcheck::fit_screen(&bp, &brief(vec![ConstraintTag::DraftLimit { max_m: 0.0 }]), cat);
            let tags = [
                ConstraintTag::DraftLimit { max_m: screen.tags[0].value - 0.05 }, crane(), ConstraintTag::Passengers { count: 6, allowed_losses: 0 },
                ConstraintTag::RcsLimit { max_m2: 50.0 }, ConstraintTag::MinSpeed { kn: 60.0 }, ConstraintTag::Range { min_km: 3000.0 },
            ];
            for t in tags {
                let t0 = std::time::Instant::now();
                let r = quartermaster(&bp, &brief(vec![t.clone()]), &BTreeMap::new(), None, &ruleset(), &mut real_deps, &mut NoDeadline);
                let summary = match &r { QmResult::AlreadyFits(_) => "already".to_string(), QmResult::Refit { diff, cost, .. } => format!("refit {} changes, {} salvage, {} h", diff.changes.len(), cost.salvage, cost.hours), QmResult::NoLegalRefit { blocking } => format!("none: {blocking:?}") };
                eprintln!("{name} {t:?}: {:.0} ms -> {summary}", t0.elapsed().as_secs_f64() * 1e3);
            }
        }
    });
}
