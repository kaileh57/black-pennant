//! Tests for `apply_edit`, note collection and ranking, and the notes CSV.
use super::*;
use crate::editor::gauges::testkit::*;
use crate::editor::gauges::{FloatGauge, GaugeStrip, PowerGauge};
use crate::editor::Check;
use crate::text::csv_check::{check_msg, lint, parse, GLOSSARY};
use content::blueprint::{KitPlacement, PartPlacement};
use content::cell::CellPos;
use content::graph::{Cmp, Condition, ControlGraph, GraphEdge, GraphNode, MountLimits, NodeKind, Operand, PortRef, Rule, RuleValue, SinkKind, SourceKind, Value};
use content::kit::{KitDef, KitKind, KitPart, PartRot};
use content::network::PinnedRoute;
use content::part::{PartDef, PartFunction};

const CSV: &str = include_str!("../../../../../game/i18n/notes.csv");

fn kit(id: &str, parts: &[(&str, CellPos)], tags: Vec<content::part::PartTag>) -> KitDef {
    KitDef {
        format: 1, id: KitId::new(id), version: 1, hash: ContentHash::default(), name: id.into(), name_key: None, description_key: None, author: None,
        licence: "CC-BY-4.0".into(), lineage: vec![], kind: KitKind::Assembly,
        parts: parts.iter().map(|(p, o)| KitPart { part: PartId::new(*p), offset: *o, rot: PartRot::default() }).collect(),
        sockets: vec![], settings: vec![], graph: ControlGraph::default(), officer: None, tags, tray_order: 0,
    }
}

fn with_kits(mut cat: Catalogue, kits: Vec<KitDef>) -> Catalogue {
    for k in kits { cat.kits.insert(k.id.clone(), k); }
    cat.kits.sort_keys();
    cat
}

#[test]
fn removing_a_part_renumbers_every_stored_slot() {
    let (mut bp, cat) = fitted();
    // Slots: 0 gen, 1 diesel, 2 radar, 3 gun, 4 tank, 5 hoist. Kit 0 holds the gun and its hoist.
    bp.kits.push(KitPlacement { kit: KitId::new("k"), version: 1, pos: CellPos::new(30, 6, 6), rot: PartRot::default(), mirrored: false, sockets: vec![], settings: Default::default(), parts: vec![PartSlot(3), PartSlot(5)] });
    bp.parts[3].kit = Some(KitSlot(0));
    bp.parts[5].kit = Some(KitSlot(0));
    bp.officers.mount_limits.insert(PartSlot(3), MountLimits { train_min_deg: -90.0, train_max_deg: 90.0, elevation_min_deg: 0.0, elevation_max_deg: 60.0 });
    bp.officers.radar_hz.insert(PartSlot(2), 5.0);
    bp.rules.push(Rule { when: Condition::Compare { lhs: Operand::PartHealth(PartSlot(2)), cmp: Cmp::Lt, rhs: Operand::Const(0.5) }, sink: SinkKind::HelmSpeed, value: RuleValue::Set(Value::Scalar(1.0)), enabled: true });
    bp.rules.push(Rule { when: Condition::Compare { lhs: Operand::AmmoFraction(PartSlot(3)), cmp: Cmp::Lt, rhs: Operand::Const(0.5) }, sink: SinkKind::MountFirePermission(PartSlot(3)), value: RuleValue::Set(Value::Bool(false)), enabled: true });
    bp.networks.data_links.push((PartSlot(2), PartSlot(3)));
    bp.networks.data_links.push((PartSlot(0), PartSlot(5)));
    bp.networks.pinned_routes.push(PinnedRoute { network: NetworkKind::Power, from: PartSlot(0), to: PartSlot(3), cells: vec![] });
    let node = |id: u32, kind: NodeKind| GraphNode { id: GraphNodeId(id), kind, params: Default::default(), pos: [0.0, 0.0] };
    bp.graph.pinned.nodes.push(node(1 << 24, NodeKind::Source(SourceKind::PartState(PartSlot(2)))));
    bp.graph.pinned.nodes.push(node((1 << 24) + 1, NodeKind::Sink(SinkKind::MountAimPoint(PartSlot(3)))));
    bp.graph.pinned.edges.push(GraphEdge { from: PortRef { node: GraphNodeId(1 << 24), port: 0 }, to: PortRef { node: GraphNodeId((1 << 24) + 1), port: 0 } });

    let out = apply_edit(&bp, &BlueprintEdit::RemovePart(PartSlot(2)), &cat).expect("applies");
    assert_eq!(out.parts.len(), 5);
    assert_eq!(out.parts[2].part, PartId::new("gun"));
    assert_eq!(out.kits[0].parts, vec![PartSlot(2), PartSlot(4)]);
    assert!(out.officers.mount_limits.contains_key(&PartSlot(2)) && !out.officers.mount_limits.contains_key(&PartSlot(3)));
    assert!(out.officers.radar_hz.is_empty(), "the radar's own setting goes with it");
    assert_eq!(out.rules.len(), 1, "the rule reading the removed radar goes");
    assert_eq!(out.rules[0].sink, SinkKind::MountFirePermission(PartSlot(2)));
    assert!(matches!(out.rules[0].when, Condition::Compare { lhs: Operand::AmmoFraction(PartSlot(2)), .. }));
    assert_eq!(out.networks.data_links, vec![(PartSlot(0), PartSlot(4))]);
    assert_eq!((out.networks.pinned_routes[0].from, out.networks.pinned_routes[0].to), (PartSlot(0), PartSlot(2)));
    assert_eq!(out.graph.pinned.nodes.len(), 1);
    assert_eq!(out.graph.pinned.nodes[0].kind, NodeKind::Sink(SinkKind::MountAimPoint(PartSlot(2))));
    assert!(out.graph.pinned.edges.is_empty());
    assert_eq!(out.header.hash, content::blueprint::io::compute_hash(&out), "rehashed");

    // An officer kit (no parts) is not swept away by an unrelated removal.
    let mut with_officer = bp.clone();
    with_officer.kits.push(KitPlacement { kit: KitId::new("officer"), version: 1, pos: CellPos::new(0, 0, 0), rot: PartRot::default(), mirrored: false, sockets: vec![], settings: Default::default(), parts: vec![] });
    let out = apply_edit(&with_officer, &BlueprintEdit::RemovePart(PartSlot(0)), &cat).expect("applies");
    assert_eq!(out.kits.len(), 2);
    // Removing both kit parts removes the kit and shifts kit references.
    let out = apply_edit(&bp, &BlueprintEdit::RemoveKit(KitSlot(0)), &cat).expect("applies");
    assert!(out.kits.is_empty());
    assert_eq!(out.parts.len(), 4);
    assert!(out.parts.iter().all(|p| p.kit.is_none()));
    assert!(apply_edit(&bp, &BlueprintEdit::RemovePart(PartSlot(99)), &cat).is_none());
    assert!(apply_edit(&bp, &BlueprintEdit::RemoveKit(KitSlot(3)), &cat).is_none());
}

#[test]
fn parts_cannot_overlap_and_several_is_atomic() {
    let (bp, cat) = fitted();
    let onto_gen = PartPlacement { part: PartId::new("tank"), pos: CellPos::new(5, 1, 5), rot: PartRot::default(), kit: None, label: None, load: None };
    assert!(apply_edit(&bp, &BlueprintEdit::AddPart(onto_gen.clone()), &cat).is_none(), "overlaps the generator");
    let free = PartPlacement { pos: CellPos::new(25, 1, 5), ..onto_gen.clone() };
    let out = apply_edit(&bp, &BlueprintEdit::AddPart(free.clone()), &cat).expect("free space");
    assert_eq!(out.parts.len(), bp.parts.len() + 1);
    let unknown = PartPlacement { part: PartId::new("nope"), ..free.clone() };
    assert!(apply_edit(&bp, &BlueprintEdit::AddPart(unknown), &cat).is_none());
    let far = PartPlacement { pos: CellPos::new(-3, 1, 5), ..free.clone() };
    assert!(apply_edit(&bp, &BlueprintEdit::AddPart(far), &cat).is_none(), "outside the grid limits");
    // Moving a part onto itself is fine; onto another part is not.
    assert!(apply_edit(&bp, &BlueprintEdit::MovePart { slot: PartSlot(0), to: CellPos::new(5, 1, 5), rot: PartRot { yaw: 5, tilt: 0 } }, &cat).is_some());
    assert!(apply_edit(&bp, &BlueprintEdit::MovePart { slot: PartSlot(0), to: CellPos::new(8, 1, 5), rot: PartRot::default() }, &cat).is_none());
    // One failing step fails the whole edit, and the input is untouched.
    let e = BlueprintEdit::Several(vec![BlueprintEdit::AddPart(free.clone()), BlueprintEdit::AddPart(free)]);
    assert!(apply_edit(&bp, &e, &cat).is_none());
    assert!(apply_edit(&bp, &BlueprintEdit::Several(vec![]), &cat).is_none());
    let mut deep = BlueprintEdit::SetLoad { slot: PartSlot(4), load: 0.5 };
    for _ in 0..12 { deep = BlueprintEdit::Several(vec![deep]); }
    assert!(apply_edit(&bp, &deep, &cat).is_none(), "nesting is bounded");
}

#[test]
fn values_are_checked_and_clamped() {
    let (mut bp, cat) = fitted();
    let b = place(&mut bp, "ballast", CellPos::new(25, 1, 5));
    let out = apply_edit(&bp, &BlueprintEdit::SetLoad { slot: PartSlot(4), load: 3.0 }, &cat).unwrap();
    assert_eq!(out.parts[4].load, Some(1.0));
    let out = apply_edit(&bp, &BlueprintEdit::SetLoad { slot: b, load: 9_000.0 }, &cat).unwrap();
    assert_eq!(out.parts[b.index()].load, Some(4_000.0));
    assert!(apply_edit(&bp, &BlueprintEdit::SetLoad { slot: PartSlot(0), load: 0.5 }, &cat).is_none(), "a generator has no load");
    assert!(apply_edit(&bp, &BlueprintEdit::SetLoad { slot: PartSlot(4), load: f64::NAN }, &cat).is_none());
    let out = apply_edit(&bp, &BlueprintEdit::SetSlider { key: "gunnery.aggression".into(), value: 4.0 }, &cat).unwrap();
    assert_eq!(out.officers.sliders.get("gunnery.aggression"), Some(&1.0));
    assert!(apply_edit(&bp, &BlueprintEdit::SetSlider { key: "no.such".into(), value: 0.5 }, &cat).is_none());
    let out = apply_edit(&bp, &BlueprintEdit::SetBurstTrigger { range_m: 1.0e9 }, &cat).unwrap();
    assert_eq!(out.officers.burst_trigger_range_m, Some(60_000.0));
    let lim = MountLimits { train_min_deg: 120.0, train_max_deg: -400.0, elevation_min_deg: -50.0, elevation_max_deg: 99.0 };
    let out = apply_edit(&bp, &BlueprintEdit::SetMountLimits { slot: PartSlot(3), limits: lim }, &cat).unwrap();
    assert_eq!(out.officers.mount_limits[&PartSlot(3)], MountLimits { train_min_deg: -180.0, train_max_deg: 120.0, elevation_min_deg: -10.0, elevation_max_deg: 85.0 });
    assert!(apply_edit(&bp, &BlueprintEdit::SetMountLimits { slot: PartSlot(0), limits: lim }, &cat).is_none(), "not a mount");
    assert!(apply_edit(&bp, &BlueprintEdit::UnpinNode(GraphNodeId(5)), &cat).is_none());
    assert!(apply_edit(&bp, &BlueprintEdit::UnpinRoute { index: 0 }, &cat).is_none());
    assert!(apply_edit(&bp, &BlueprintEdit::SetHullHandle { station: 0, handle: "beam".into(), value: 4.0 }, &cat).is_none(), "freeform hull has no handles");
}

#[test]
fn armour_and_bulkheads_change_cells() {
    let (bp, cat) = fitted();
    let side = CellPos::new(10, 3, 0);
    let out = apply_edit(&bp, &BlueprintEdit::PaintArmour { cells: vec![side, CellPos::new(10, 3, 5)], tier: 3 }, &cat).unwrap();
    assert_eq!(content::cell::CellGrid::from_rows(&out.cells).get(side).armour, content::cell::ArmourTier::Mm40);
    assert!(apply_edit(&bp, &BlueprintEdit::PaintArmour { cells: vec![CellPos::new(10, 3, 5)], tier: 3 }, &cat).is_none(), "air takes no armour");
    assert!(apply_edit(&bp, &BlueprintEdit::PaintArmour { cells: vec![side], tier: 9 }, &cat).is_none());
    // A freeform bulkhead fills the enclosed slice around the parts.
    let out = apply_edit(&bp, &BlueprintEdit::AddBulkhead { x_cell: 18 }, &cat).unwrap();
    let g = content::cell::CellGrid::from_rows(&out.cells);
    assert!(!g.get(CellPos::new(18, 3, 5)).is_empty());
    assert!(g.get(CellPos::new(18, 8, 5)).is_empty(), "nothing above the deck");
    assert_eq!(out.hull.planes.len(), 1);
    assert!(apply_edit(&out, &BlueprintEdit::AddBulkhead { x_cell: 18 }, &cat).is_none(), "already there");
    assert!(apply_edit(&bp, &BlueprintEdit::AddBulkhead { x_cell: 0 }, &cat).is_none());
    let pf = prefab(&out, &cat);
    assert_eq!(pf.compartments.compartments.iter().filter(|c| !c.is_absorbed()).count(), 2);
}

#[test]
fn kits_add_swap_and_remove() {
    let (bp, cat) = fitted();
    let cat = with_kits(cat, vec![kit("kit.gen", &[("gen", CellPos::new(0, 0, 0)), ("tank", CellPos::new(2, 0, 0))], vec![content::part::PartTag::Generation]), kit("kit.tank", &[("tank", CellPos::new(0, 0, 0))], vec![])]);
    let add = BlueprintEdit::AddKit { kit: KitId::new("kit.gen"), pos: CellPos::new(22, 1, 5), rot: PartRot::default(), mirrored: false };
    let out = apply_edit(&bp, &add, &cat).expect("room for the kit");
    assert_eq!(out.kits.len(), 1);
    assert_eq!(out.kits[0].parts, vec![PartSlot(6), PartSlot(7)]);
    assert!(out.parts[6..].iter().all(|p| p.kit == Some(KitSlot(0))));
    let swapped = apply_edit(&out, &BlueprintEdit::SwapKit { slot: KitSlot(0), kit: KitId::new("kit.tank") }, &cat).unwrap();
    assert_eq!(swapped.parts.len(), 7);
    assert_eq!(swapped.kits[0].kit, KitId::new("kit.tank"));
    assert!(apply_edit(&bp, &BlueprintEdit::AddKit { kit: KitId::new("kit.gen"), pos: CellPos::new(5, 1, 5), rot: PartRot::default(), mirrored: false }, &cat).is_none(), "overlaps");
    assert!(apply_edit(&bp, &BlueprintEdit::AddKit { kit: KitId::new("kit.none"), pos: CellPos::new(22, 1, 5), rot: PartRot::default(), mirrored: false }, &cat).is_none());
    // The placement finder puts a tagged kit inside the hull.
    let (edit, _) = collect::add_kit_for_tag(&bp, content::part::PartTag::Generation, (0.4, false), &cat).expect("a spot");
    assert!(apply_edit(&bp, &edit, &cat).is_some());
}

#[test]
fn lofted_handles_regenerate_cells() {
    let cat = crate::editor::gauges::testkit::power_catalogue();
    let mut bp = content::blueprint::io::new_blueprint("Lofted", 20.0);
    let content::hull::HullSource::Lofted(shell) = &bp.hull.source else { panic!("lofted") };
    let beam = shell.stations[1].beam_m;
    let space = crate::editor::suggest::place::Space::new(&bp, &cat);
    let spot = space.low_centre(0.5);
    place(&mut bp, "tank", spot);
    let wide = apply_edit(&bp, &BlueprintEdit::SetHullHandle { station: 1, handle: "beam".into(), value: beam + 1.0 }, &cat).expect("valid");
    assert_ne!(wide.cells, bp.cells);
    let content::hull::HullSource::Lofted(s2) = &wide.hull.source else { panic!("lofted") };
    assert!((s2.stations[1].beam_m - (beam + 1.0)).abs() < 1e-9);
    assert!(apply_edit(&bp, &BlueprintEdit::SetHullHandle { station: 1, handle: "colour".into(), value: 1.0 }, &cat).is_none());
    assert!(apply_edit(&bp, &BlueprintEdit::SetHullHandle { station: 40, handle: "beam".into(), value: 3.0 }, &cat).is_none());
    // A bulkhead on a lofted hull goes in through the voxelizer.
    let sx = content::cell::CellGrid::from_rows(&bp.cells).size[0] as i16;
    let out = apply_edit(&bp, &BlueprintEdit::AddBulkhead { x_cell: sx / 3 }, &cat).expect("bulkhead");
    assert_eq!(out.hull.planes.len(), bp.hull.planes.len() + 1);
}

fn note(tier: NoteTier, affected: u16, part: Option<u16>, flips: bool) -> ShipNote {
    ShipNote { tier, kind: NoteKind::Advisory("t".into()), part: part.map(PartSlot), affected_parts: affected, text: Msg::new("notes.gauge.punch"), fix: None, flips_tag: flips, score_effect: None }
}

#[test]
fn ranking_is_tier_then_flip_then_reach_then_slot() {
    let mut v = vec![note(NoteTier::Advisory, 9, Some(0), false), note(NoteTier::CannotWork, 1, Some(4), false), note(NoteTier::CannotWork, 1, Some(2), false), note(NoteTier::CannotWork, 3, Some(9), false), note(NoteTier::CannotWork, 0, Some(7), true)];
    rank(&mut v, false);
    let order: Vec<(NoteTier, Option<PartSlot>)> = v.iter().map(|n| (n.tier, n.part)).collect();
    assert_eq!(order, vec![(NoteTier::CannotWork, Some(PartSlot(7))), (NoteTier::CannotWork, Some(PartSlot(9))), (NoteTier::CannotWork, Some(PartSlot(2))), (NoteTier::CannotWork, Some(PartSlot(4))), (NoteTier::Advisory, Some(PartSlot(0)))]);
    v[3].score_effect = Some(5.0);
    rank(&mut v, true);
    assert_eq!(v[0].part, Some(PartSlot(4)), "fallback: measured effect first within the tier");
}

fn healthy_strip() -> GaugeStrip {
    GaugeStrip { float: FloatGauge { floats: true, gm_m: 0.6, gm_over_beam: 0.1, displacement_t: 50.0, check: Check::Pass, ..Default::default() }, ..Default::default() }
}

#[test]
fn notes_come_ranked_with_rows_and_working_fixes() {
    let (bp, cat) = fitted();
    let cat = with_kits(cat, vec![kit("kit.crane", &[("bridge", CellPos::new(0, 0, 0))], vec![content::part::PartTag::Crane])]);
    let design = design(&bp, prefab(&bp, &cat));
    let mut g = healthy_strip();
    g.power = PowerGauge { generation_kw: 0.0, cruise_demand_kw: 20.0, combat_demand_kw: 50.0, battery_kwh: 0.0, check: Check::Fail };
    g.punch.check = Check::Caution;
    let fit = FitResult { tags: vec![crate::editor::fitcheck::TagResult { tag: content::campaign::ConstraintTag::Requires(content::part::PartTag::Crane), pass: false, value: 0.0, limit: 1.0 }], ..Default::default() };
    let removed = [content::migrate::RemovedPart { part: PartId::new("old.gun"), pos: CellPos::new(1, 2, 3) }];
    let notes = ship_notes(&bp, &design, &g, Some(&fit), &removed, &cat);
    assert!(notes.windows(2).all(|w| w[0].tier <= w[1].tier), "ranked by tier");
    let kinds: Vec<&NoteKind> = notes.iter().map(|n| &n.kind).collect();
    assert!(kinds.contains(&&NoteKind::NoPropulsion) && kinds.contains(&&NoteKind::NoSteering) && kinds.contains(&&NoteKind::NoGeneration));
    assert!(!kinds.contains(&&NoteKind::FloatFail));
    let tag = notes.iter().find(|n| matches!(n.kind, NoteKind::TagFails(_))).expect("tag note");
    assert_eq!(tag.tier, NoteTier::FailsBriefingTag);
    assert!(tag.flips_tag && tag.fix.is_some(), "the crane kit fits on deck and flips the tag");
    assert!(notes.iter().any(|n| n.kind == NoteKind::PartRemovedByMigration(PartId::new("old.gun")) && n.tier == NoteTier::Advisory));
    assert!(notes.iter().any(|n| n.kind == NoteKind::GaugeCaution("punch".into())));
    let rows = parse(CSV);
    for n in &notes {
        check_msg(&rows, &n.text);
        if let Some(f) = &n.fix { assert!(apply_edit(&bp, f, &cat).is_some(), "{:?}", n.kind); }
    }
    // Deterministic.
    assert_eq!(notes, ship_notes(&bp, &design, &g, Some(&fit), &removed, &cat));
    // Outside campaign there are no tag notes.
    assert!(ship_notes(&bp, &design, &g, None, &[], &cat).iter().all(|n| n.tier != NoteTier::FailsBriefingTag));
}

#[test]
fn csv_rows_match_keys_and_copy_rules() {
    let rows = parse(CSV);
    let keys = text_keys();
    assert_eq!(rows.len(), keys.len(), "CSV rows vs text_keys()");
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), keys.len(), "unique keys");
    let mut glossary = GLOSSARY.to_vec();
    glossary.push("GM");
    for k in &keys {
        let (_, en) = rows.get(*k).unwrap_or_else(|| panic!("no row for {k}"));
        assert!(k.starts_with("notes."));
        lint(k, en, 80, &glossary);
    }
}

#[test]
fn a_buoyant_hull_that_will_not_stand_capsizes_rather_than_sinks() {
    let (bp, cat) = fitted();
    let design = design(&bp, prefab(&bp, &cat));
    let float_note = |g: &GaugeStrip| ship_notes(&bp, &design, g, None, &[], &cat).into_iter().find(|n| n.kind == NoteKind::FloatFail).map(|n| n.text.key);
    let mut g = healthy_strip();
    g.float = FloatGauge { floats: false, reserve: 0.0, displacement_t: 900.0, check: Check::Fail, ..Default::default() };
    assert_eq!(float_note(&g), Some(TextKey::new("notes.float_fail.sinks")));
    g.float = FloatGauge { floats: false, reserve: 1.5, gm_m: -0.1, heel_deg: 93.0, displacement_t: 80.0, check: Check::Fail, ..Default::default() };
    assert_eq!(float_note(&g), Some(TextKey::new("notes.float_fail.gm")), "lolls past a right angle: the GM note");
    g.float = FloatGauge { floats: true, reserve: 1.5, gm_m: -0.02, displacement_t: 80.0, check: Check::Fail, ..Default::default() };
    assert_eq!(float_note(&g), Some(TextKey::new("notes.float_fail.gm")));
    assert_eq!(float_note(&healthy_strip()), None);
}

#[test]
fn magazine_notes_follow_the_feed_wiring_and_the_catalogue() {
    let (bp, cat) = fitted();
    let gun = PartSlot(3);
    let mount = crate::doctrine::MountInfo {
        slot: gun, part: cat.part_idx(&PartId::new("gun")).expect("gun"), weapon: content::graph::WeaponClass::Gun, munition: None,
        pos_local: DVec3::new(15.25, 4.25, 3.25), zero_rad: 0.0,
        limits: MountLimits { train_min_deg: -150.0, train_max_deg: 150.0, elevation_min_deg: -10.0, elevation_max_deg: 85.0 },
        effective_range_m: 8_000.0, director: None, kit: None, group: 0, muzzle_mps: 900.0, point_defence: false,
    };
    let notes_with = |cat: &Catalogue| {
        let mut d = design(&bp, prefab(&bp, cat));
        d.mounts = vec![mount.clone()];
        ship_notes(&bp, &d, &healthy_strip(), None, &[], cat).into_iter().filter(|n| n.kind == NoteKind::NoMagazine).collect::<Vec<_>>()
    };
    // No 76 mm magazine exists: the gun fires from its ready rounds by design.
    assert!(notes_with(&cat).is_empty());
    // Once one exists, 10 ready rounds against 120 a minute is a gun out of the fight: tier 3.
    let mut defs: Vec<PartDef> = cat.parts.values().cloned().collect();
    defs.push(def("mag76", content::part::PartCategory::Gun, [2, 1, 2], content::part::Placement::Internal, PartFunction::Magazine { calibre_mm: 76, rounds: 100, ammo_kg: 600.0 }, vec![]));
    let with_mag = catalogue(defs.clone());
    let n = notes_with(&with_mag);
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!((n[0].tier, n[0].part), (NoteTier::CannotWork, Some(gun)));
    // A deep ready-use locker lasts the first minute: advisory only.
    for d in &mut defs { if let PartFunction::Gun(g) = &mut d.function { g.ready_rounds = 400; } }
    let n = notes_with(&catalogue(defs));
    assert_eq!(n.len(), 1);
    assert_eq!(n[0].tier, NoteTier::Advisory);
}

/// On a new hull with only the tray's engine kit fitted, the "cannot move" Fix adds a kit that brings a propeller
/// or waterjet, so applying it answers the note (an engine-only kit would leave the ship where it was).
#[test]
fn no_propulsion_fix_adds_a_propulsor() {
    let cat = content::catalogue::catalogue().clone();
    let mut bp = content::blueprint::io::new_blueprint("t", 22.0);
    let design = design(&bp, prefab(&bp, &cat));
    let notes = ship_notes(&bp, &design, &healthy_strip(), None, &[], &cat);
    let note = notes.iter().find(|n| n.kind == NoteKind::NoPropulsion).expect("an empty hull cannot move");
    let fix = note.fix.as_ref().expect("a Fix is offered");
    bp = apply_edit(&bp, fix, &cat).expect("the Fix applies");
    assert!(bp.parts.iter().filter_map(|p| cat.part(&p.part)).any(|d| matches!(d.function, PartFunction::Propulsor(_))), "{fix:?} brings a propulsor");
}

/// With no console aboard, the weapons and sensors on Data get one note that adds the bridge, not one note each.
#[test]
fn no_console_is_one_note_with_a_bridge_fix() {
    let cat = content::catalogue::catalogue().clone();
    let mut bp = content::blueprint::io::new_blueprint("t", 22.0);
    let esm = cat.part(&PartId::new("sensor.esm")).expect("ESM").clone();
    let space = crate::editor::suggest::place::Space::new(&bp, &cat);
    let up = crate::editor::suggest::place::upright();
    let pos = space.find_part(&esm, up, space.deck_top(0.5), 12).expect("the ESM fits on deck");
    bp = apply_edit(&bp, &crate::editor::BlueprintEdit::AddPart(PartPlacement { part: esm.id.clone(), pos, rot: up, kit: None, label: None, load: None }), &cat).expect("ESM placed");
    let design = design(&bp, prefab(&bp, &cat));
    let notes = ship_notes(&bp, &design, &healthy_strip(), None, &[], &cat);
    let unconnected: Vec<&ShipNote> = notes.iter().filter(|n| n.kind == NoteKind::Unconnected).collect();
    assert_eq!(unconnected.len(), 1, "{unconnected:?}");
    assert_eq!(unconnected[0].text.key.as_str(), "notes.no_console");
    let fix = unconnected[0].fix.as_ref().expect("a Fix adds the bridge");
    let next = apply_edit(&bp, fix, &cat).expect("the Fix applies");
    assert!(next.parts.iter().filter_map(|p| cat.part(&p.part)).any(|d| matches!(d.function, PartFunction::Console)));
}
