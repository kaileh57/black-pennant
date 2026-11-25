//! Unit `data-act2`: the committed reference setup for Act 2's "defaults fail, sliders pass" operation (DESIGN.md
//! L317-318, L782): the Underwriter duel, `op.act2.duel.underwriter`.
//!
//! The setup is a stock blueprint, `blueprints/reference/act2/duel_underwriter.bp.ron`: the 40 m missile corvette
//! starter, hull and parts untouched, with an L0 to L2 setup stored the way the editor stores one (officer
//! settings and rules; no pinned nodes or scripts). The winnability harness applies its officers and rules over
//! each player hull's stored settings (`PlayerSetup::reference_setup`).
//!
//! Why the defaults fail: Indemnity runs silent and closes at speed, and the Cautious preset holds the
//! missiles for a Type identification and a Coarse track that a silent contact in haze gives too late. The
//! reference goes Aggressive (Domain identification and a bearing track suffice, one more missile a salvo),
//! raises gunnery aggression, bursts the search radar out to 30 km, and two rules switch the ship to active
//! emission while a surface contact of Suspect stance or worse is inside 25 km or a weapon is inbound.
//! Regenerate with `cargo test -p content --test reference_act2 -- --ignored write_reference_act2`.
use content::blueprint::io::{canonicalize_blueprint, to_file_text};
use content::blueprint::Blueprint;
use content::enums::{Act, DoctrinePreset, Domain, Emcon, Stance, TrackGrade};
use content::graph::{Cmp, Condition, Operand, Rule, RuleValue, SinkKind, TrackFilter, Value};
use content::kit::LineageEntry;

const STARTER: &str = "starters/corvette40.bp.ron";
const REFERENCE: &str = "reference/act2/duel_underwriter.bp.ron";
const OPERATION: &str = "op.act2.duel.underwriter";

fn reference() -> Blueprint {
    let starter = content::campaign::load::stock_blueprint(STARTER).expect("the 40 m starter");
    let mut bp = starter.clone();
    bp.header.name = "Missile corvette, reference setup".into();
    bp.header.lineage = vec![LineageEntry { hash: starter.header.hash, author: "Black Pennant".into() }];
    let o = &mut bp.officers;
    o.preset = DoctrinePreset::Aggressive;
    o.emcon = Emcon::Burst;
    o.burst_trigger_range_m = Some(30_000.0);
    o.sliders.insert("gunnery.aggression".into(), 0.9);
    o.sliders.insert("helm.evasion".into(), 0.7);
    o.sliders.insert("sensors.burst_repeat_s".into(), 15.0);
    let surface = TrackFilter { min_stance: Stance::Suspect, domain: Some(Domain::Surface), min_grade: TrackGrade::Bearing };
    bp.rules = vec![
        Rule { when: Condition::Compare { lhs: Operand::InboundWeapons, cmp: Cmp::Gt, rhs: Operand::Const(0.0) }, sink: SinkKind::ShipEmcon, value: RuleValue::Set(Value::Emcon(Emcon::Active)), enabled: true },
        Rule { when: Condition::Compare { lhs: Operand::NearestContactRangeM(surface), cmp: Cmp::Lt, rhs: Operand::Const(25_000.0) }, sink: SinkKind::ShipEmcon, value: RuleValue::Set(Value::Emcon(Emcon::Active)), enabled: true },
    ];
    canonicalize_blueprint(&mut bp);
    bp
}

fn path() -> std::path::PathBuf { std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../blueprints").join(REFERENCE) }

#[test]
fn committed_reference_matches_its_generator() {
    let text = std::fs::read_to_string(path()).unwrap_or_else(|e| panic!("{REFERENCE}: {e}")).replace("\r\n", "\n");
    assert_eq!(to_file_text(&reference()), text, "stale: run `cargo test -p content --test reference_act2 -- --ignored write_reference_act2`");
}

#[test]
fn the_reference_is_an_l0_to_l2_setup_on_the_stock_starter() {
    let cat = content::catalogue::catalogue();
    let c = content::campaign::load::campaign();
    let op = &c.operations[&content::ids::OperationId::new(OPERATION)];
    assert_eq!(op.reference_setup.as_deref(), Some(REFERENCE));
    let act2: Vec<_> = c.operations.values().filter(|o| o.act == Act::Act2 && o.reference_setup.is_some()).collect();
    assert_eq!(act2.len(), 1, "one reference operation in Act 2 (L318)");
    let r = content::campaign::load::stock_blueprint(REFERENCE).unwrap();
    let s = content::campaign::load::stock_blueprint(STARTER).unwrap();
    // Same ship: only the stored setup differs.
    assert_eq!((&r.hull, &r.cells, &r.parts, &r.kits), (&s.hull, &s.cells, &s.parts, &s.kits));
    assert_ne!((&r.officers, &r.rules), (&s.officers, &s.rules));
    // L0 to L2 only: no pinned nodes, replaced stages or scripts (L318: no graph edits or scripts).
    assert!(r.graph.is_empty());
    assert!(r.rules.len() <= content::graph::MAX_RULES && r.rules.iter().all(|x| x.enabled));
    // It is a legal campaign design for the act it plays in.
    let rs = content::ruleset::Ruleset::campaign(Act::Act2);
    let ctx = content::validate::ValidationContext { ruleset: &rs, available: None, enemy_class: false, complexity: None };
    let report = content::validate::validate_blueprint(&r, cat, &ctx);
    assert!(report.issues.is_empty(), "{:?}", report.issues);
}

#[test]
#[ignore = "writes blueprints/reference/act2; run by hand after editing the setup"]
fn write_reference_act2() {
    let p = path();
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, to_file_text(&reference())).unwrap();
}
