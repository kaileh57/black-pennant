//! Unit `data-act3`: the committed reference setup for Act 3's "defaults fail, sliders pass" operation (DESIGN.md
//! L317-318, L782): the Liquidator duel, `op.act3.duel.liquidator`.
//!
//! The setup is a stock blueprint, `blueprints/reference/act3/duel_liquidator.bp.ron`: the 60 m raiding corvette
//! starter, hull and parts untouched, with an L0 to L2 setup stored the way the editor stores one (officer
//! settings and rules; no pinned nodes or scripts). The winnability harness applies its officers and rules over
//! each player hull's stored settings (`PlayerSetup::reference_setup`).
//!
//! Why the defaults fail: Foreclosure keeps 16 km, runs silent in rain, and her 155 mm turrets reach 18 km against
//! the starter's 100 mm at 12 km. The Cautious preset holds the heavy missiles for a Type identification and a
//! Coarse track that a silent contact in rain gives only once the corvette has closed into her gun range, where
//! a steady course meets her salvoes. The reference goes Aggressive (Domain identification and a bearing track
//! suffice), bursts the search radar out to 40 km, raises evasion to the top of the slider so the Helm zigzags
//! through her long-range salvoes, and two rules switch the ship to active emission while a surface contact of
//! Suspect stance or worse is inside 35 km or a weapon is inbound, so the missiles get their track from outside
//! her gun range.
//! Regenerate with `cargo test -p content --test reference_act3 -- --ignored write_reference_act3`.
use content::blueprint::io::{canonicalize_blueprint, to_file_text};
use content::blueprint::Blueprint;
use content::enums::{Act, DoctrinePreset, Domain, Emcon, Stance, TrackGrade};
use content::graph::{Cmp, Condition, Operand, Rule, RuleValue, SinkKind, TrackFilter, Value};
use content::kit::LineageEntry;

const STARTER: &str = "starters/corvette60.bp.ron";
const REFERENCE: &str = "reference/act3/duel_liquidator.bp.ron";
const OPERATION: &str = "op.act3.duel.liquidator";

fn reference() -> Blueprint {
    let starter = content::campaign::load::stock_blueprint(STARTER).expect("the 60 m starter");
    let mut bp = starter.clone();
    bp.header.name = "Raiding corvette, reference setup".into();
    bp.header.lineage = vec![LineageEntry { hash: starter.header.hash, author: "Black Pennant".into() }];
    let o = &mut bp.officers;
    o.preset = DoctrinePreset::Aggressive;
    o.emcon = Emcon::Burst;
    o.burst_trigger_range_m = Some(40_000.0);
    o.sliders.insert("gunnery.aggression".into(), 0.9);
    o.sliders.insert("helm.evasion".into(), 1.0);
    o.sliders.insert("helm.cruise_speed".into(), 1.0);
    o.sliders.insert("sensors.burst_repeat_s".into(), 15.0);
    let surface = TrackFilter { min_stance: Stance::Suspect, domain: Some(Domain::Surface), min_grade: TrackGrade::Bearing };
    bp.rules = vec![
        Rule { when: Condition::Compare { lhs: Operand::InboundWeapons, cmp: Cmp::Gt, rhs: Operand::Const(0.0) }, sink: SinkKind::ShipEmcon, value: RuleValue::Set(Value::Emcon(Emcon::Active)), enabled: true },
        Rule { when: Condition::Compare { lhs: Operand::NearestContactRangeM(surface), cmp: Cmp::Lt, rhs: Operand::Const(35_000.0) }, sink: SinkKind::ShipEmcon, value: RuleValue::Set(Value::Emcon(Emcon::Active)), enabled: true },
    ];
    canonicalize_blueprint(&mut bp);
    bp
}

fn path() -> std::path::PathBuf { std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../blueprints").join(REFERENCE) }

#[test]
fn committed_reference_matches_its_generator() {
    let text = std::fs::read_to_string(path()).unwrap_or_else(|e| panic!("{REFERENCE}: {e}")).replace("\r\n", "\n");
    assert_eq!(to_file_text(&reference()), text, "stale: run `cargo test -p content --test reference_act3 -- --ignored write_reference_act3`");
}

#[test]
fn the_reference_is_an_l0_to_l2_setup_on_the_stock_starter() {
    let cat = content::catalogue::catalogue();
    let c = content::campaign::load::campaign();
    let op = &c.operations[&content::ids::OperationId::new(OPERATION)];
    assert_eq!(op.reference_setup.as_deref(), Some(REFERENCE));
    let act3: Vec<_> = c.operations.values().filter(|o| o.act == Act::Act3 && o.reference_setup.is_some()).collect();
    assert_eq!(act3.len(), 1, "one reference operation in Act 3 (L318)");
    let r = content::campaign::load::stock_blueprint(REFERENCE).unwrap();
    let s = content::campaign::load::stock_blueprint(STARTER).unwrap();
    // Same ship: only the stored setup differs.
    assert_eq!((&r.hull, &r.cells, &r.parts, &r.kits), (&s.hull, &s.cells, &s.parts, &s.kits));
    assert_ne!((&r.officers, &r.rules), (&s.officers, &s.rules));
    // L0 to L2 only: no pinned nodes, replaced stages or scripts (L318: no graph edits or scripts).
    assert!(r.graph.is_empty());
    assert!(r.rules.len() <= content::graph::MAX_RULES && r.rules.iter().all(|x| x.enabled));
    // It is a legal campaign design for the act it plays in, buildable from the tiers open at the act start.
    let rs = content::ruleset::Ruleset::campaign(Act::Act3);
    let ctx = content::validate::ValidationContext { ruleset: &rs, available: None, enemy_class: false, complexity: None };
    let report = content::validate::validate_blueprint(&r, cat, &ctx);
    assert!(report.issues.is_empty(), "{:?}", report.issues);
    let start = c.acts.iter().find(|a| a.act == Act::Act3).unwrap().start_tier;
    assert!(r.parts.iter().all(|p| cat.part(&p.part).unwrap().tier <= start));
}

#[test]
#[ignore = "writes blueprints/reference/act3; run by hand after editing the setup"]
fn write_reference_act3() {
    let p = path();
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, to_file_text(&reference())).unwrap();
}
