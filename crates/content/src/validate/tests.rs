use super::*;
use crate::blueprint::{KitPlacement, MAX_PAINT};
use crate::cell::{ArmourTier, CellPos, CellRow, CellRun};
use crate::graph::{ControlGraph, GraphEdge, GraphNode, GraphOverrides, MathOp, NodeKind, OfficerStage, PortRef, ScriptSource, SinkKind, SourceKind, StageReplacement, Value, USER_NODE_BASE};
use crate::ids::{GraphNodeId, KitId, KitSlot};
use crate::kit::{KitDef, KitKind, KitPart};
use crate::ruleset::RulesetKind;
use crate::stats::testkit::{blueprint, catalogue, place, rows, shell_box, steel};

fn standard() -> Ruleset { Ruleset { tonnage_cap_t: 400.0, ..Ruleset::default() } }
fn open() -> Ruleset { Ruleset { kind: RulesetKind::Open, luau: true, ..standard() } }
fn ctx(rs: &Ruleset) -> ValidationContext<'_> { ValidationContext { ruleset: rs, available: None, enemy_class: false, complexity: None } }

/// A 20 m closed box hull (40 x 4 x 6 cells from y = 1) with a gun on deck, a tank inside, a propeller under
/// the bottom and a davit on the side. Waterline row 2.
fn good() -> Blueprint {
    let mut bp = blueprint(shell_box(40, 4, 6, 1), 2);
    bp.parts = vec![place("gun", 10, 5, 2), place("tank", 10, 2, 2), place("prop", 2, 0, 2), place("davit", 20, 3, 6)];
    bp
}

fn issues(bp: &Blueprint, rs: &Ruleset) -> Vec<Issue> { validate_blueprint(bp, &catalogue(), &ctx(rs)).issues }
fn node(id: u32, kind: NodeKind) -> GraphNode { GraphNode { id: GraphNodeId(id), kind, params: Default::default(), pos: [0.0, 0.0] } }
fn edge(a: u32, ap: u8, b: u32, bp: u8) -> GraphEdge { GraphEdge { from: PortRef { node: GraphNodeId(a), port: ap }, to: PortRef { node: GraphNodeId(b), port: bp } } }

#[test]
fn legal_design_passes() {
    let r = validate_blueprint(&good(), &catalogue(), &ctx(&standard()));
    assert!(r.is_ok(), "{:?}", r.issues);
    assert!(r.stats.tonnage_t > 0.0);
}

#[test]
fn length_limits_follow_the_mode() {
    let line = |n: i16| blueprint(rows(&(0..n).map(|x| (x, 0, 0, steel(ArmourTier::None))).collect::<Vec<_>>()), 0);
    let long = line(200);
    assert!(matches!(issues(&long, &standard())[..], [Issue::TooLong { length_m, max_m }] if length_m == 100.0 && max_m == 90.0));
    let rs = standard();
    let enemy = ValidationContext { enemy_class: true, ..ctx(&rs) };
    assert!(validate_blueprint(&long, &catalogue(), &enemy).is_ok());
    assert!(matches!(issues(&line(18), &standard())[..], [Issue::TooShort { .. }]));
    // A 10 m shell can voxelize to 19 cells; one cell of tolerance.
    assert!(issues(&line(19), &standard()).is_empty());
    assert!(matches!(issues(&blueprint(Default::default(), 0), &standard())[..], [Issue::TooShort { .. }]));
}

#[test]
fn caps_and_bounds() {
    let mut bp = good();
    let tight = Ruleset { tonnage_cap_t: 1.0, ..standard() };
    assert!(matches!(issues(&bp, &tight)[..], [Issue::OverTonnage { cap_t, .. }] if cap_t == 1.0));
    let rs = standard();
    let c = ValidationContext { complexity: Some(700.0), ..ctx(&rs) };
    assert!(matches!(validate_blueprint(&bp, &catalogue(), &c).issues[..], [Issue::OverComplexity { cap, .. }] if cap == 600.0));
    let c = ValidationContext { complexity: Some(f64::NAN), ..ctx(&rs) };
    assert!(matches!(validate_blueprint(&bp, &catalogue(), &c).issues[..], [Issue::OverComplexity { .. }]));
    bp.cells.0.push(CellRow { y: 1, z: 0, runs: vec![CellRun { x: -1, len: 1, cell: steel(ArmourTier::None) }] });
    assert!(issues(&bp, &standard()).contains(&Issue::GridOutOfBounds));
}

#[test]
fn part_legality() {
    let mut bp = good();
    bp.parts.push(place("nonesuch", 1, 1, 1));
    bp.parts.push(place("gun", 30, 5, 2));
    bp.parts.push(place("box", -3, 2, 2));
    let got = issues(&bp, &standard());
    assert_eq!(got, vec![Issue::UnknownPart { part: PartId::new("nonesuch") }, Issue::PartOverLimit { part: PartId::new("gun"), count: 2, limit: 1 }, Issue::PartOutOfBounds { slot: PartSlot(6) }]);
    // Open rulesets only apply explicit limits.
    bp.parts.truncate(4);
    bp.parts.push(place("gun", 30, 5, 2));
    assert!(issues(&bp, &open()).is_empty());
    let rs = standard();
    let only = [PartId::new("tank"), PartId::new("prop"), PartId::new("davit")];
    let c = ValidationContext { available: Some(&only), ..ctx(&rs) };
    assert_eq!(validate_blueprint(&good(), &catalogue(), &c).issues, vec![Issue::PartNotAvailable { part: PartId::new("gun") }]);
}

#[test]
fn overlaps_and_placement() {
    let mut bp = good();
    bp.parts.push(place("box", 10, 5, 2)); // on the gun
    bp.parts.push(place("gun", 12, 1, 2)); // in the bottom plating, under the deck
    bp.parts.push(place("tank", 12, 5, 2)); // on deck: outside the envelope
    bp.parts.push(place("prop", 30, 5, 2)); // above water
    bp.parts.push(place("davit", 20, 2, 3)); // inside, not at the edge
    let got = issues(&bp, &standard());
    assert_eq!(
        got,
        vec![
            Issue::PartOverLimit { part: PartId::new("gun"), count: 2, limit: 1 },
            Issue::PartsOverlap { a: PartSlot(0), b: PartSlot(4) },
            Issue::PartBadPlacement { slot: PartSlot(5) },
            Issue::PartBadPlacement { slot: PartSlot(6) },
            Issue::PartBadPlacement { slot: PartSlot(7) },
            Issue::PartBadPlacement { slot: PartSlot(8) },
        ]
    );
    // A director on top of a mast is fine: only structure blocks the sky.
    let mut stacked = good();
    stacked.parts.push(place("gun", 20, 7, 2));
    stacked.parts.push(place("gun", 20, 5, 2));
    assert!(issues(&stacked, &open()).is_empty());
}

#[test]
fn connectivity_and_paint() {
    let mut bp = good();
    bp.cells.0.push(CellRow { y: 20, z: 20, runs: vec![CellRun { x: 5, len: 1, cell: steel(ArmourTier::None) }] });
    assert_eq!(issues(&bp, &standard()), vec![Issue::Disconnected { stray_cell: CellPos::new(5, 20, 20) }]);
    let mut bp = good();
    bp.cells.0[0].runs[0].cell.paint = 3;
    let first = CellPos::new(bp.cells.0[0].runs[0].x, bp.cells.0[0].y, bp.cells.0[0].z);
    assert_eq!(issues(&bp, &standard()), vec![Issue::BadPaintIndex { cell: first }]);
    let mut bp = good();
    bp.paint = vec![bp.paint[0].clone(); MAX_PAINT + 1];
    assert_eq!(issues(&bp, &standard()), vec![Issue::TooManyPaintEntries(17)]);
}

#[test]
fn shell_parameters() {
    let mut bp = crate::blueprint::io::new_blueprint("x", 30.0);
    let crate::hull::HullSource::Lofted(shell) = &mut bp.hull.source else { panic!("lofted") };
    shell.stations.truncate(2);
    bp.paint.truncate(9);
    let got: Vec<_> = issues(&bp, &open()).into_iter().filter(|i| matches!(i, Issue::BadShell { .. })).collect();
    assert_eq!(got.len(), 1);
}

#[test]
fn layers_and_scripts() {
    let mut bp = good();
    bp.graph.pinned.nodes.push(node(USER_NODE_BASE, NodeKind::Math(MathOp::Add)));
    assert_eq!(issues(&bp, &standard()), vec![Issue::LayerNotAllowed]);
    assert!(issues(&bp, &open()).is_empty());
    bp.graph.scripts.push(ScriptSource { name: "s".into(), source: "".into(), inputs: vec![], outputs: vec![], enabled: false });
    let no_luau = Ruleset { luau: false, ..open() };
    assert_eq!(issues(&bp, &no_luau), vec![Issue::ScriptsNotAllowed]);
}

#[test]
fn graph_structure() {
    let u = USER_NODE_BASE;
    let mut g = GraphOverrides::default();
    g.pinned.nodes = vec![node(u, NodeKind::Math(MathOp::Add)), node(u, NodeKind::Math(MathOp::Neg))];
    assert!(matches!(validate_graph(&g, &[])[..], [Issue::BadGraph { .. }]));

    // A missing player node, an edge out of a sink and an edge into a source.
    g.pinned.nodes = vec![node(u, NodeKind::Sink(SinkKind::HelmSpeed)), node(u + 1, NodeKind::Source(SourceKind::OwnShip)), node(u + 2, NodeKind::Math(MathOp::Neg))];
    g.pinned.edges = vec![edge(u + 9, 0, u + 2, 0), edge(u, 0, u + 2, 0), edge(u + 2, 0, u + 1, 0)];
    assert_eq!(validate_graph(&g, &[]).len(), 3);

    // A cycle through plain math is refused; through a latch it is feedback.
    g.pinned.nodes = vec![node(u, NodeKind::Math(MathOp::Add)), node(u + 1, NodeKind::Math(MathOp::Neg))];
    g.pinned.edges = vec![edge(u, 0, u + 1, 0), edge(u + 1, 0, u, 0)];
    assert!(matches!(validate_graph(&g, &[])[..], [Issue::BadGraph { .. }]));
    g.pinned.nodes[1].kind = NodeKind::Math(MathOp::Latch);
    assert!(validate_graph(&g, &[]).is_empty());

    // Two drivers into one input; a generated id below the base may be absent.
    g.pinned.nodes = vec![node(u, NodeKind::Math(MathOp::Neg)), node(u + 1, NodeKind::Source(SourceKind::Constant(Value::Scalar(1.0))))];
    g.pinned.edges = vec![edge(5, 0, u, 0), edge(u + 1, 0, u, 0)];
    assert!(matches!(validate_graph(&g, &[])[..], [Issue::BadGraph { .. }]));
    g.pinned.edges.pop();
    assert!(validate_graph(&g, &[]).is_empty());
    // An edge with neither end stored is refused in a pinned graph.
    g.pinned.edges.push(edge(5, 0, 6, 0));
    assert_eq!(validate_graph(&g, &[]).len(), 1);

    // Script ports and types.
    let script = ScriptSource { name: "s".into(), source: "".into(), inputs: vec![("a".into(), crate::graph::PortType::Bool)], outputs: vec![], enabled: true };
    g.scripts = vec![script];
    g.pinned.nodes = vec![node(u, NodeKind::Script(0)), node(u + 1, NodeKind::Source(SourceKind::Constant(Value::Text("x".into())))), node(u + 2, NodeKind::Script(4))];
    g.pinned.edges = vec![edge(u + 1, 0, u, 0), edge(u + 1, 0, u, 1)];
    assert_eq!(validate_graph(&g, &[]).len(), 3, "{:?}", validate_graph(&g, &[])); // type mismatch, missing input, missing script

    // A replacement subgraph may wire generated ids straight to the stage it replaces.
    let sub = ControlGraph { nodes: vec![node(u + 10, NodeKind::Math(MathOp::Neg))], edges: vec![edge(7, 0, 8, 0), edge(7, 1, u + 10, 0), edge(u + 10, 0, 8, 1)] };
    let ok = GraphOverrides { replaced: vec![StageReplacement { stage: OfficerStage::Evasion, subgraph: sub.clone() }], ..Default::default() };
    assert!(validate_graph(&ok, &[]).is_empty(), "{:?}", validate_graph(&ok, &[]));
    let twice = GraphOverrides { replaced: vec![StageReplacement { stage: OfficerStage::Evasion, subgraph: sub.clone() }, StageReplacement { stage: OfficerStage::Evasion, subgraph: sub }], ..Default::default() };
    assert_eq!(validate_graph(&twice, &[]).len(), 1);

    let rule = crate::graph::Rule { when: crate::graph::Condition::All(vec![]), sink: SinkKind::HelmSpeed, value: crate::graph::RuleValue::Set(Value::Scalar(1.0)), enabled: true };
    assert_eq!(validate_graph(&GraphOverrides::default(), &vec![rule; 33]), vec![Issue::TooManyRules(33)]);
}

fn kit(parts: &[&str]) -> KitDef {
    let mut k = crate::kit::parse_kit(include_str!("../../../../blueprints/examples/minimal.kit.ron"), "k").unwrap();
    k.id = KitId::new("kit.test");
    k.parts = parts.iter().enumerate().map(|(i, p)| KitPart { part: PartId::new(*p), offset: CellPos::new(i as i16, 0, 0), rot: Default::default() }).collect();
    k.sockets[0].offset = CellPos::new(0, 0, 0);
    k
}

#[test]
fn kits_and_bindings() {
    let cat = catalogue();
    assert!(validate_kit(&kit(&["box", "mag"]), &cat, &standard()).is_empty());
    let mut k = kit(&["box", "nonesuch"]);
    k.parts.push(KitPart { part: PartId::new("box"), offset: CellPos::new(0, 0, 0), rot: Default::default() });
    k.sockets[0].offset = CellPos::new(9, 9, 9);
    let got = validate_kit(&k, &cat, &standard());
    assert_eq!(got.len(), 3, "{got:?}"); // unknown part, overlap, socket outside
    let mut officer = kit(&[]);
    officer.kind = KitKind::Officer;
    assert_eq!(validate_kit(&officer, &cat, &standard()), vec![Issue::LayerNotAllowed]);
    assert!(validate_kit(&officer, &cat, &open()).is_empty());

    // A placed embedded kit that owns its parts passes; a broken binding is reported.
    let mut bp = good();
    let def = kit(&["box"]);
    bp.embedded_kits.insert(def.id.clone(), def.clone());
    let mut p = place("box", 30, 5, 2);
    p.kit = Some(KitSlot(0));
    bp.parts.push(p);
    bp.kits.push(KitPlacement { kit: def.id.clone(), version: 1, pos: CellPos::new(30, 5, 2), rot: Default::default(), mirrored: false, sockets: vec![], settings: Default::default(), parts: vec![PartSlot(4)] });
    assert!(issues(&bp, &standard()).is_empty(), "{:?}", issues(&bp, &standard()));
    bp.kits[0].parts = vec![PartSlot(0)];
    assert_eq!(issues(&bp, &standard()).len(), 2); // wrong part type, and slot 4 claims a kit that does not list it
    bp.kits[0].kit = KitId::new("kit.missing");
    assert!(issues(&bp, &standard()).iter().any(|i| matches!(i, Issue::BadKitBinding { .. })));
}

#[test]
fn hostile_input_does_not_panic() {
    let mut bp = good();
    bp.cells.0.push(CellRow { y: i16::MAX, z: i16::MIN, runs: vec![CellRun { x: i16::MIN, len: u16::MAX, cell: steel(ArmourTier::Mm80) }] });
    bp.cells.0.push(CellRow { y: 0, z: 0, runs: vec![CellRun { x: 0, len: u16::MAX, cell: steel(ArmourTier::Mm80) }] });
    bp.parts.push(place("gun", i16::MAX, i16::MAX, i16::MAX));
    bp.parts.push(place("tank", i16::MIN, 0, 0));
    bp.hull.design_waterline_y = i16::MIN;
    let got = issues(&bp, &standard());
    assert!(got.contains(&Issue::GridOutOfBounds));
    // Two cells at opposite corners of the frame: too big a box to build, refused as out of bounds.
    let far = blueprint(rows(&[(0, 0, 0, steel(ArmourTier::None)), (383, 383, 383, steel(ArmourTier::None))]), 0);
    assert!(issues(&far, &open()).contains(&Issue::GridOutOfBounds));
}

#[test]
fn text_keys_are_distinct_and_translated() {
    let all = [
        Issue::TooLong { length_m: 0.0, max_m: 0.0 },
        Issue::TooShort { length_m: 0.0, min_m: 0.0 },
        Issue::GridOutOfBounds,
        Issue::OverTonnage { tonnage_t: 0.0, cap_t: 0.0 },
        Issue::OverComplexity { points: 0.0, cap: 0.0 },
        Issue::UnknownPart { part: PartId::default() },
        Issue::PartNotAvailable { part: PartId::default() },
        Issue::PartOverLimit { part: PartId::default(), count: 0, limit: 0 },
        Issue::PartOutOfBounds { slot: PartSlot(0) },
        Issue::PartsOverlap { a: PartSlot(0), b: PartSlot(1) },
        Issue::PartBadPlacement { slot: PartSlot(0) },
        Issue::Disconnected { stray_cell: CellPos::default() },
        Issue::BadPaintIndex { cell: CellPos::default() },
        Issue::TooManyPaintEntries(0),
        Issue::TooManyRules(0),
        Issue::LayerNotAllowed,
        Issue::ScriptsNotAllowed,
        Issue::BadGraph { detail: String::new() },
        Issue::BadKitBinding { detail: String::new() },
        Issue::BadShell { detail: String::new() },
    ];
    let csv = include_str!("../../../../game/i18n/validate.csv");
    let keys: std::collections::BTreeSet<String> = all.iter().map(|i| i.text_key().0).collect();
    assert_eq!(keys.len(), all.len());
    for k in keys {
        assert!(csv.lines().any(|l| l.starts_with(&format!("{k},"))), "{k} has no CSV row");
    }
}

#[test]
fn stock_files_are_structurally_sound() {
    let cat = crate::catalogue::catalogue();
    let rs = Ruleset::campaign(crate::enums::Act::Act4);
    for (path, text) in crate::data::stock_blueprint_files() {
        let bp = crate::blueprint::io::load_blueprint_with(text, path, cat).unwrap().blueprint;
        let c = ValidationContext { ruleset: &rs, available: None, enemy_class: bp.header.kind == crate::blueprint::BlueprintKind::EnemyClass, complexity: None };
        let r = validate_blueprint(&bp, cat, &c);
        let structural: Vec<_> = r.issues.iter().filter(|i| matches!(i, Issue::GridOutOfBounds | Issue::UnknownPart { .. } | Issue::BadGraph { .. } | Issue::BadKitBinding { .. } | Issue::Disconnected { .. } | Issue::PartsOverlap { .. } | Issue::BadPaintIndex { .. })).collect();
        assert!(structural.is_empty(), "{path}: {structural:?}");
    }
    for (path, text) in crate::data::stock_kit_files() {
        let k = crate::kit::parse_kit(text, path).unwrap();
        let bad: Vec<_> = validate_kit(&k, cat, &rs).into_iter().filter(|i| !matches!(i, Issue::LayerNotAllowed)).collect();
        assert!(bad.is_empty(), "{path}: {bad:?}");
    }
}

#[test]
fn new_designs_validate_quickly() {
    let cat = crate::catalogue::catalogue();
    for len in [10.0, 22.0, 90.0] {
        let bp = crate::blueprint::io::new_blueprint("x", len);
        let rs = Ruleset::campaign(crate::enums::Act::Act4);
        let t = std::time::Instant::now();
        let r = validate_blueprint(&bp, cat, &ctx(&rs));
        assert!(t.elapsed().as_millis() < 1000);
        assert!(r.is_ok(), "{len} m: {:?}", r.issues);
    }
}

#[test]
fn placement_on_a_voxelized_hull() {
    let cat = catalogue();
    let hull = crate::hull::default_hull(40.0);
    let vox = crate::hull::voxelize(&hull);
    let mut bp = blueprint(vox.grid.to_rows(), vox.design_waterline_y);
    bp.hull.design_waterline_y = vox.design_waterline_y;
    bp.paint = crate::blueprint::io::default_paint_table();
    let size = vox.grid.size;
    let wl = vox.design_waterline_y;
    let z = size[2] as i16 / 2;
    let solid = |x: i16, y: i16| !vox.grid.get(CellPos::new(x, y, z)).is_empty();
    // Amidships, away from any bulkhead: a column with interior air at or above the waterline row.
    let (x, inner) = (size[0] as i16 / 3..size[0] as i16 * 2 / 3)
        .find_map(|x| {
            let top = (0..size[1] as i16).rev().find(|&y| solid(x, y))?;
            (wl..top).find(|&y| !solid(x, y)).map(|y| (x, y))
        })
        .expect("interior air amidships");
    // Weather part: the first empty cell above the topmost solid cell in that column.
    let top = (0..size[1] as i16).rev().find(|&y| solid(x, y)).expect("deck");
    bp.parts = vec![place("ballast", x, inner, z), place("gun", x, top + 1, z)];
    let rs = Ruleset { tonnage_cap_t: 0.0, ..open() };
    let r = validate_blueprint(&bp, &cat, &ctx(&rs));
    assert!(r.is_ok(), "{:?}", r.issues);
}
