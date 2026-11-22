use super::*;
use crate::blueprint::{Blueprint, KitPlacement, PartPlacement};
use crate::catalogue::Catalogue;
use crate::cell::{Cell, CellGrid, CellShape, Material};
use crate::graph::{Cmp, Condition, GraphEdge, GraphNode, NodeKind, Operand, PortRef, Rule, RuleValue, SinkKind, SourceKind, Value};
use crate::ids::{GraphNodeId, KitId, PartSlot};
use crate::part::PartDef;

const BP: &str = include_str!("../../../../blueprints/examples/minimal.bp.ron");
const KIT: &str = include_str!("../../../../blueprints/examples/minimal.kit.ron");

#[test]
fn peeks_blueprint_and_kit_formats() {
    assert_eq!(peek_format(BP).unwrap(), 1);
    assert_eq!(peek_format(KIT).unwrap(), 1);
}

#[test]
fn peek_ignores_decoys() {
    let text = r##"#![enable(implicit_some)]
    // format: 7
    Blueprint( /* format: 8 /* nested format: 9 */ */
        note: "format: 10",
        raw: r#"format: 11 ")"#,
        ch: ')',
        r#header: Header(
            name: "x",
            inner: (format: 12),
            format: 0x_0003,
        ),
        embedded_kits: { "k": (format: 13) },
    )"##;
    assert_eq!(peek_format(text).unwrap(), 3);
    // Top-level kit field after nested structures.
    assert_eq!(peek_format("(id: \"k\", graph: (format: 9), format: 4)").unwrap(), 4);
    // Embedded kit formats are not the document's.
    assert!(peek_format("(header: (name: \"a\"), embedded_kits: {\"k\": (format: 2)})").is_err());
    assert!(peek_format("(format: -1)").is_err());
    assert!(peek_format("(format: 99999999999)").is_err());
    assert!(peek_format("(format: \"1\")").is_err());
    assert!(peek_format("(name: \"unterminated").is_err());
    assert!(peek_format("").is_err());
    assert!(peek_format(")))(format: 1)").is_err());
}

#[test]
fn set_format_rewrites_only_the_number() {
    let out = set_format(BP, 42).unwrap();
    assert_eq!(peek_format(&out).unwrap(), 42);
    assert_eq!(out.replacen("format: 42", "format: 1", 1), BP);
}

fn bump(text: &str) -> ContentResult<String> {
    let v = peek_format(text)?;
    set_format(&text.replace("old_name", "new_name"), v + 1)
}
fn no_bump(text: &str) -> ContentResult<String> { Ok(text.to_owned()) }
fn fails(_: &str) -> ContentResult<String> { Err(ContentError::Limit("boom")) }

#[test]
fn runs_steps_in_order() {
    let steps = [Migration { from: 2, apply: bump }, Migration { from: 1, apply: bump }];
    let text = "(format: 1, old_name: 3)";
    assert_eq!(migrate_text(text, &steps, 1, 3).unwrap(), "(format: 3, new_name: 3)");
    assert_eq!(migrate_text(text, &steps, 1, 1).unwrap(), text);
    match migrate_text(text, &steps, 1, 4) {
        Err(ContentError::Migration { from: 3, to: 4, .. }) => {}
        other => panic!("{other:?}"),
    }
    match migrate_text("(format: 5)", &steps, 5, 3) {
        Err(ContentError::FormatTooNew { found: 5, supported: 3, needs_release }) => assert!(needs_release.contains("later than")),
        other => panic!("{other:?}"),
    }
    assert!(matches!(migrate_text(text, &[Migration { from: 1, apply: no_bump }], 1, 2), Err(ContentError::Migration { from: 1, to: 2, .. })));
    assert!(matches!(migrate_text(text, &[Migration { from: 1, apply: fails }], 1, 2), Err(ContentError::Migration { from: 1, .. })));
    assert!(migrate_text(text, BLUEPRINT_MIGRATIONS, 1, crate::blueprint::BLUEPRINT_FORMAT).is_ok());
}

#[test]
fn release_names() {
    assert_eq!(release_for_format(1), "0.1.0");
    assert!(release_for_format(2).starts_with("later than"));
}

fn example() -> Blueprint {
    ron::Options::default().with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME).from_str(BP).unwrap()
}

fn part(id: &str, replaced_by: Option<&str>) -> PartDef {
    let rb = replaced_by.map(|r| format!("Some(\"{r}\")")).unwrap_or("None".into());
    ron::from_str(&format!("(id: \"{id}\", name: \"n\", description: \"d\", category: Hull, tier: T1, size: (1, 1, 1), mass_kg: 1.0, cost: 1, function: Structure, replaced_by: {rb})")).unwrap()
}

fn catalogue(parts: Vec<PartDef>) -> Catalogue {
    let mut c = Catalogue::default();
    for p in parts {
        c.parts.insert(p.id.clone(), p);
    }
    c
}

fn place(bp: &mut Blueprint, id: &str, x: i16) {
    bp.parts.push(PartPlacement { part: PartId::new(id), pos: CellPos::new(x, 0, 0), rot: Default::default(), kit: None, label: None, load: None });
}

fn node(id: u32, kind: NodeKind) -> GraphNode { GraphNode { id: GraphNodeId(id), kind, params: Default::default(), pos: [0.0; 2] } }

#[test]
fn replaces_and_removes_parts_keeping_slots_consistent() {
    let mut bp = example();
    let cat = catalogue(vec![part("keep", None), part("old", Some("mid")), part("mid", Some("new")), part("new", None), part("dead", Some("missing")), part("loop", Some("loop"))]);
    for (id, x) in [("keep", 0), ("gone", 1), ("old", 2), ("dead", 3), ("keep", 4), ("loop", 5)] {
        place(&mut bp, id, x);
    }
    let kit = |parts: Vec<u16>| KitPlacement { kit: KitId::new("k"), version: 1, pos: CellPos::default(), rot: Default::default(), mirrored: false, sockets: vec![], settings: Default::default(), parts: parts.into_iter().map(PartSlot).collect() };
    // Kit 0 loses slot 1 and dissolves; kit 1 keeps both its parts (one replaced) and becomes kit 0.
    bp.kits = vec![kit(vec![1, 4]), kit(vec![0, 2])];
    for (slot, k) in [(1, 0), (4, 0), (0, 1), (2, 1)] {
        bp.parts[slot].kit = Some(crate::ids::KitSlot(k));
    }
    bp.officers.radar_hz.insert(PartSlot(4), 5.0);
    bp.officers.radar_hz.insert(PartSlot(3), 6.0);
    bp.networks.data_links = vec![(PartSlot(0), PartSlot(4)), (PartSlot(0), PartSlot(1))];
    let rule = |sink, slot| Rule { when: Condition::Compare { lhs: Operand::PartHealth(PartSlot(slot)), cmp: Cmp::Lt, rhs: Operand::Const(0.5) }, sink, value: RuleValue::Set(Value::Bool(true)), enabled: true };
    bp.rules = vec![rule(SinkKind::Pump(PartSlot(4)), 2), rule(SinkKind::HelmSpeed, 1), rule(SinkKind::Door(PartSlot(5)), 0)];
    let base = crate::graph::USER_NODE_BASE;
    bp.graph.pinned.nodes = vec![node(base, NodeKind::Source(SourceKind::PartState(PartSlot(4)))), node(base + 1, NodeKind::Sink(SinkKind::MountAimPoint(PartSlot(3))))];
    bp.graph.pinned.edges = vec![GraphEdge { from: PortRef { node: GraphNodeId(base), port: 0 }, to: PortRef { node: GraphNodeId(base + 1), port: 0 } }];

    let removed = replace_removed_parts(&mut bp, &cat);
    let ids: Vec<_> = removed.iter().map(|r| (r.part.as_str(), r.pos.x)).collect();
    assert_eq!(ids, vec![("gone", 1), ("dead", 3), ("loop", 5)]);
    let left: Vec<_> = bp.parts.iter().map(|p| p.part.as_str()).collect();
    assert_eq!(left, vec!["keep", "new", "keep"]);
    assert_eq!(bp.kits.len(), 1);
    assert_eq!(bp.kits[0].parts, vec![PartSlot(0), PartSlot(1)]);
    let kits: Vec<_> = bp.parts.iter().map(|p| p.kit.map(|k| k.0)).collect();
    assert_eq!(kits, vec![Some(0), Some(0), None]);
    assert_eq!(bp.officers.radar_hz.clone().into_iter().collect::<Vec<_>>(), vec![(PartSlot(2), 5.0)]);
    assert_eq!(bp.networks.data_links, vec![(PartSlot(0), PartSlot(2))]);
    assert_eq!(bp.rules.len(), 1);
    assert_eq!(bp.rules[0].sink, SinkKind::Pump(PartSlot(2)));
    assert!(matches!(bp.rules[0].when, Condition::Compare { lhs: Operand::PartHealth(PartSlot(1)), .. }));
    assert_eq!(bp.graph.pinned.nodes.len(), 1);
    assert!(matches!(bp.graph.pinned.nodes[0].kind, NodeKind::Source(SourceKind::PartState(PartSlot(2)))));
    assert!(bp.graph.pinned.edges.is_empty());

    // Idempotent: a second pass changes nothing.
    let before = bp.clone();
    assert!(replace_removed_parts(&mut bp, &cat).is_empty());
    assert_eq!(bp, before);
}

#[test]
fn removed_part_body_cells_are_emptied() {
    let mut bp = example();
    let mut grid = CellGrid::from_rows(&bp.cells);
    let body = Cell { material: Material::PartBody, armour: Default::default(), paint: 0, shape: CellShape::CUBE };
    let steel_before = grid.solid_count();
    grid.set(CellPos::new(2, 0, 0), body);
    grid.set(CellPos::new(3, 0, 0), body);
    bp.cells = grid.to_rows();
    place(&mut bp, "gone", 2);
    let removed = replace_removed_parts(&mut bp, &Catalogue::default());
    assert_eq!(removed.len(), 1);
    assert!(bp.parts.is_empty());
    let after = CellGrid::from_rows(&bp.cells);
    assert_eq!(after.solid_count(), steel_before - 2);
    assert!(after.cells.iter().all(|c| c.material != Material::PartBody));
}
