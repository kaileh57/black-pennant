//! Unit `data-act4`: the committed reference setup for Act 4's "defaults fail, sliders pass" operation (DESIGN.md
//! L317-318, L782): the Chairman duel, `op.act4.duel.chairman`.
//!
//! The setup is a stock blueprint, `blueprints/reference/act4/duel_chairman.bp.ron`: the 90 m destroyer starter,
//! hull and parts untouched, with an L0 to L2 setup stored the way the editor stores one (a preset, sliders,
//! officer settings and rules; no doctrine-table edits, pinned nodes or scripts). The winnability harness applies
//! its officers and rules over each player hull's stored settings (`PlayerSetup::reference_setup`).
//!
//! Why the defaults fail: the Charter keeps 17 km behind an 80 mm belt, her 155 mm turrets reach 18 km against the
//! starter's 127 mm at 12 km, and she carries 32 surface-to-air cells, two point-defence launchers and four
//! close-in guns. The Cautious preset fires the destroyer's eight heavy missiles two at a time, on a Type
//! identification and a Coarse track, so her air defence meets one pair after another; then the stock Helm closes
//! into her gun range on a steady course. The reference goes Aggressive, bursts the search radar out to 60 km so a
//! track comes from outside her gun range, raises evasion to the top of the slider so the Helm zigzags through her
//! salvoes, and its rules table (L2) does the rest: one rule per heavy-missile launcher fires it at the nearest
//! hostile surface track graded Coarse or better inside 45 km, so all eight missiles leave in one ripple that
//! saturates her defences, and two rules switch the ship to active emission while a surface contact of Suspect
//! stance or worse is inside 40 km or a weapon is inbound.
//! Regenerate with `cargo test -p content --test reference_act4 -- --ignored write_reference_act4`.
use content::blueprint::io::{canonicalize_blueprint, to_file_text};
use content::blueprint::Blueprint;
use content::enums::{Act, DoctrinePreset, Domain, Emcon, Stance, TrackGrade};
use content::ids::PartSlot;
use content::graph::{Cmp, Condition, Operand, Rule, RuleValue, SinkKind, TrackFilter, Value};
use content::kit::LineageEntry;
use content::part::{LauncherKind, PartFunction};

const STARTER: &str = "starters/destroyer90.bp.ron";
const REFERENCE: &str = "reference/act4/duel_chairman.bp.ron";
const OPERATION: &str = "op.act4.duel.chairman";

/// The starter's heavy anti-ship missile launchers: (slot, cells).
fn heavy_launchers(bp: &Blueprint) -> Vec<(PartSlot, u8)> {
    let cat = content::catalogue::catalogue();
    bp.parts.iter().enumerate().filter_map(|(i, p)| match cat.part(&p.part).map(|d| &d.function) {
        Some(PartFunction::Launcher(l)) if l.kind == LauncherKind::Missile && l.munition.as_str() == "missile.ashm.heavy" => Some((PartSlot(i as u16), l.cells)),
        _ => None,
    }).collect()
}

/// Hostile surface tracks good enough to fire a heavy missile at.
const TARGET: TrackFilter = TrackFilter { min_stance: Stance::Hostile, domain: Some(Domain::Surface), min_grade: TrackGrade::Coarse };
/// Range inside which the launcher rules fire, metres (the heavy missile flies 70 km).
const RIPPLE_M: f64 = 45_000.0;

fn reference() -> Blueprint {
    let starter = content::campaign::load::stock_blueprint(STARTER).expect("the 90 m starter");
    let mut bp = starter.clone();
    bp.header.name = "Destroyer, reference setup".into();
    bp.header.lineage = vec![LineageEntry { hash: starter.header.hash, author: "Black Pennant".into() }];
    let o = &mut bp.officers;
    o.preset = DoctrinePreset::Aggressive;
    o.emcon = Emcon::Burst;
    o.burst_trigger_range_m = Some(60_000.0);
    o.sliders.insert("gunnery.aggression".into(), 0.9);
    o.sliders.insert("helm.evasion".into(), 1.0);
    o.sliders.insert("helm.cruise_speed".into(), 1.0);
    o.sliders.insert("sensors.burst_repeat_s".into(), 15.0);
    let close = TrackFilter { min_stance: Stance::Suspect, domain: Some(Domain::Surface), min_grade: TrackGrade::Bearing };
    let mut rules = vec![
        Rule { when: Condition::Compare { lhs: Operand::InboundWeapons, cmp: Cmp::Gt, rhs: Operand::Const(0.0) }, sink: SinkKind::ShipEmcon, value: RuleValue::Set(Value::Emcon(Emcon::Active)), enabled: true },
        Rule { when: Condition::Compare { lhs: Operand::NearestContactRangeM(close), cmp: Cmp::Lt, rhs: Operand::Const(40_000.0) }, sink: SinkKind::ShipEmcon, value: RuleValue::Set(Value::Emcon(Emcon::Active)), enabled: true },
    ];
    for (slot, _) in heavy_launchers(&starter) {
        rules.push(Rule { when: Condition::Compare { lhs: Operand::NearestContactRangeM(TARGET), cmp: Cmp::Lt, rhs: Operand::Const(RIPPLE_M) }, sink: SinkKind::MissileLaunch(slot), value: RuleValue::NearestContact(TARGET), enabled: true });
    }
    bp.rules = rules;
    canonicalize_blueprint(&mut bp);
    bp
}

fn path() -> std::path::PathBuf { std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../blueprints").join(REFERENCE) }

#[test]
fn committed_reference_matches_its_generator() {
    let text = std::fs::read_to_string(path()).unwrap_or_else(|e| panic!("{REFERENCE}: {e}")).replace("\r\n", "\n");
    assert_eq!(to_file_text(&reference()), text, "stale: run `cargo test -p content --test reference_act4 -- --ignored write_reference_act4`");
}

#[test]
fn the_reference_is_an_l0_to_l2_setup_on_the_stock_starter() {
    let cat = content::catalogue::catalogue();
    let c = content::campaign::load::campaign();
    let op = &c.operations[&content::ids::OperationId::new(OPERATION)];
    assert_eq!(op.reference_setup.as_deref(), Some(REFERENCE));
    let act4: Vec<_> = c.operations.values().filter(|o| o.act == Act::Act4 && o.reference_setup.is_some()).collect();
    assert_eq!(act4.len(), 1, "one reference operation in Act 4 (L318)");
    let r = content::campaign::load::stock_blueprint(REFERENCE).unwrap();
    let s = content::campaign::load::stock_blueprint(STARTER).unwrap();
    // Same ship: only the stored setup differs.
    assert_eq!((&r.hull, &r.cells, &r.parts, &r.kits), (&s.hull, &s.cells, &s.parts, &s.kits));
    assert_ne!((&r.officers, &r.rules), (&s.officers, &s.rules));
    // L0 to L2 only: no pinned nodes, replaced stages or scripts (L318: no graph edits or scripts).
    assert!(r.graph.is_empty());
    assert!(r.rules.len() <= content::graph::MAX_RULES && r.rules.iter().all(|x| x.enabled));
    // No doctrine-table edit: the defaults hold their pairs, and the rules table is the lever (L318).
    assert!(r.officers.doctrine.is_empty() && r.officers.mount_doctrine.is_empty());
    // One launch rule per heavy-missile launcher, so the whole load leaves together: more than one Cautious pair.
    let launchers = heavy_launchers(&s);
    let cells: u32 = launchers.iter().map(|(_, n)| *n as u32).sum();
    assert!(cells > 2, "the starter carries more heavy missiles than one Cautious salvo");
    for (slot, _) in &launchers {
        assert!(r.rules.iter().any(|x| x.sink == SinkKind::MissileLaunch(*slot) && x.value == RuleValue::NearestContact(TARGET)), "launcher {slot:?} has a launch rule");
    }
    // It is a legal campaign design for the act it plays in, buildable from the tiers open at the act start.
    let rs = content::ruleset::Ruleset::campaign(Act::Act4);
    let ctx = content::validate::ValidationContext { ruleset: &rs, available: None, enemy_class: false, complexity: None };
    let report = content::validate::validate_blueprint(&r, cat, &ctx);
    assert!(report.issues.is_empty(), "{:?}", report.issues);
    let start = c.acts.iter().find(|a| a.act == Act::Act4).unwrap().start_tier;
    assert!(r.parts.iter().all(|p| cat.part(&p.part).unwrap().tier <= start));
}

#[test]
#[ignore = "writes blueprints/reference/act4; run by hand after editing the setup"]
fn write_reference_act4() {
    let p = path();
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, to_file_text(&reference())).unwrap();
}
