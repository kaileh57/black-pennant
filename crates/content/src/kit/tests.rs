use super::*;
use crate::blueprint::{Blueprint, PartPlacement};
use crate::catalogue::Catalogue;
use crate::cell::{orientation_matrix, Cell, CellGrid, Face, Material};
use crate::error::ContentError;
use crate::part::PartDef;
use std::collections::BTreeSet;

const KIT: &str = include_str!("../../../../blueprints/examples/minimal.kit.ron");
const BP: &str = include_str!("../../../../blueprints/examples/minimal.bp.ron");

fn all_rots() -> impl Iterator<Item = PartRot> { (0..TILT_COUNT).flat_map(|tilt| (0..YAW_COUNT).map(move |yaw| PartRot { yaw, tilt })) }

fn part(id: &str, size: [u8; 3], replaced_by: Option<&str>) -> PartDef {
    let rb = replaced_by.map(|r| format!("Some(\"{r}\")")).unwrap_or("None".into());
    ron::from_str(&format!(
        "(id: \"{id}\", name: \"n\", description: \"d\", category: Hull, tier: T1, size: ({}, {}, {}), mass_kg: 1.0, cost: 1, function: Structure, replaced_by: {rb})",
        size[0], size[1], size[2]
    ))
    .unwrap()
}

fn catalogue(parts: Vec<PartDef>) -> Catalogue {
    let mut c = Catalogue::default();
    for p in parts {
        c.parts.insert(p.id.clone(), p);
    }
    c
}

#[test]
fn rotations_are_the_24_cube_rotations_once_each() {
    let orients: BTreeSet<u8> = all_rots().map(part_rot_orientation).collect();
    assert_eq!(orients.len(), 24);
    for r in all_rots() {
        assert_eq!(part_rot_from_matrix(part_rot_matrix(r)), Some(r));
    }
    assert_eq!(part_rot_orientation(PartRot::default()), 0);
    // Values past the reduced range wrap.
    assert_eq!(part_rot_matrix(PartRot { yaw: 5, tilt: 7 }), part_rot_matrix(PartRot { yaw: 1, tilt: 1 }));
    // One yaw turn carries the bow (+X) to port (-Z), as the cell shapes do.
    assert_eq!(part_rot_matrix(PartRot { yaw: 1, tilt: 0 })[0], [0, 0, -1]);
    // Tilt 4 stands the part nose up.
    assert_eq!(part_rot_matrix(PartRot { yaw: 0, tilt: 4 })[0], [0, 1, 0]);
    assert_eq!(part_rot_from_matrix([[1, 0, 0], [0, 1, 0], [0, 0, -1]]), None);
}

#[test]
fn rotate_offset_is_a_bijection_onto_the_rotated_box() {
    let size = [4u8, 3, 2];
    for r in all_rots() {
        let rs = rotated_size(size, r);
        let mut seen = BTreeSet::new();
        for x in 0..4 {
            for y in 0..3 {
                for z in 0..2 {
                    let o = rotate_offset(CellPos::new(x, y, z), size, r);
                    assert!(o.x >= 0 && o.y >= 0 && o.z >= 0, "{r:?}");
                    assert!((o.x as u8) < rs[0] && (o.y as u8) < rs[1] && (o.z as u8) < rs[2], "{r:?} {o:?}");
                    seen.insert(o);
                }
            }
        }
        assert_eq!(seen.len(), 24);
        let mut sorted = rs;
        sorted.sort();
        assert_eq!(sorted, [2, 3, 4]);
    }
    // Bow end of a 4-long part ends at the most negative z after one yaw turn.
    assert_eq!(rotate_offset(CellPos::new(3, 0, 0), [4, 1, 1], PartRot { yaw: 1, tilt: 0 }), CellPos::new(0, 0, 0));
    assert_eq!(rotate_offset(CellPos::new(0, 0, 0), [4, 1, 1], PartRot { yaw: 1, tilt: 0 }), CellPos::new(0, 0, 3));
    // Matches the cell orientation matrix: the offset of the +X neighbour moves along the image of +X.
    for r in all_rots() {
        let m = orientation_matrix(part_rot_orientation(r));
        let a = rotate_offset(CellPos::new(0, 0, 0), [2, 1, 1], r);
        let b = rotate_offset(CellPos::new(1, 0, 0), [2, 1, 1], r);
        assert_eq!([b.x - a.x, b.y - a.y, b.z - a.z], [m[0][0] as i16, m[0][1] as i16, m[0][2] as i16]);
    }
}

fn test_kit() -> KitDef {
    let mut k = parse_kit(KIT, "kit").unwrap();
    k.parts = vec![
        KitPart { part: PartId::new("a"), offset: CellPos::new(0, 0, 0), rot: PartRot::default() },
        KitPart { part: PartId::new("b"), offset: CellPos::new(2, 0, 1), rot: PartRot { yaw: 1, tilt: 0 } },
        KitPart { part: PartId::new("unknown"), offset: CellPos::new(9, 9, 9), rot: PartRot::default() },
    ];
    k.sockets = vec![Socket { name: "deck".into(), offset: CellPos::new(0, -1, 0), face: Face::YNeg }, Socket { name: "side".into(), offset: CellPos::new(0, 0, 0), face: Face::ZNeg }];
    k
}

#[test]
fn kit_placement_agrees_with_part_footprints() {
    let cat = catalogue(vec![part("a", [2, 1, 3], None), part("b", [3, 2, 1], None)]);
    let kit = test_kit();
    let base = CellPos::new(10, 5, 20);
    for r in all_rots() {
        for mirrored in [false, true] {
            let placed = kit_part_placements(&kit, &cat, base, r, mirrored);
            assert_eq!(placed.len(), 2, "unknown parts are skipped");
            for p in &placed {
                let size = cat.part(&p.part).unwrap().size;
                let mut cells = BTreeSet::new();
                for x in 0..size[0] {
                    for y in 0..size[1] {
                        for z in 0..size[2] {
                            let o = rotate_offset(CellPos::new(x as i16, y as i16, z as i16), size, p.rot);
                            cells.insert(CellPos::new(p.pos.x + o.x, p.pos.y + o.y, p.pos.z + o.z));
                        }
                    }
                }
                let b: BTreeSet<_> = p.cells.iter().copied().collect();
                assert_eq!(cells, b, "{r:?} mirrored {mirrored}");
            }
            let fp = kit_footprint(&kit, &cat, base, r, mirrored);
            assert_eq!(fp.len(), 6 + 6);
            let min = fp.iter().fold(CellPos::new(i16::MAX, i16::MAX, i16::MAX), |m, c| CellPos::new(m.x.min(c.x), m.y.min(c.y), m.z.min(c.z)));
            assert_eq!(min, base, "pos is the minimum corner");
        }
    }
}

#[test]
fn mirrored_kit_reflects_across_its_own_centre_plane() {
    let cat = catalogue(vec![part("a", [2, 1, 3], None), part("b", [3, 2, 1], None)]);
    let kit = test_kit();
    let plain: BTreeSet<_> = kit_footprint(&kit, &cat, CellPos::default(), PartRot::default(), false).into_iter().collect();
    let mirror: BTreeSet<_> = kit_footprint(&kit, &cat, CellPos::default(), PartRot::default(), true).into_iter().collect();
    let max_z = plain.iter().map(|c| c.z).max().unwrap();
    let reflected: BTreeSet<_> = plain.iter().map(|c| CellPos::new(c.x, c.y, max_z - c.z)).collect();
    assert_eq!(mirror, reflected);
    let socks = kit_sockets_at(&kit, &cat, CellPos::default(), PartRot::default(), true);
    assert_eq!(socks[0].face, Face::YNeg);
    assert_eq!(socks[0].cell, CellPos::new(0, -1, max_z));
    assert_eq!(socks[1].face, Face::ZPos);
    // A yaw turn turns socket faces too: -Z (port) goes to -X (aft).
    let socks = kit_sockets_at(&kit, &cat, CellPos::default(), PartRot { yaw: 1, tilt: 0 }, false);
    assert_eq!(socks[1].face, Face::XNeg);
}

#[test]
fn officer_kit_has_no_footprint() {
    let cat = catalogue(vec![]);
    let mut kit = test_kit();
    kit.kind = KitKind::Officer;
    kit.parts.clear();
    assert!(kit_footprint(&kit, &cat, CellPos::new(1, 1, 1), PartRot::default(), true).is_empty());
    assert_eq!(kit_sockets_at(&kit, &cat, CellPos::new(1, 1, 1), PartRot::default(), false)[1].cell, CellPos::new(1, 1, 1));
}

#[test]
fn kit_text_round_trips_canonically() {
    let kit = parse_kit(KIT, "minimal.kit.ron").unwrap();
    assert_eq!(kit.hash, kit_hash(&kit));
    assert!(!kit.hash.is_empty());
    let text = kit_to_file_text(&kit);
    assert!(!text.contains('\r'));
    assert!(text.contains(&format!("hash: \"{}\"", kit.hash.to_hex())));
    assert!(kit_to_canonical_ron(&kit).contains("hash: \"\""));
    // Each part sits on one line.
    assert!(text.lines().any(|l| l.trim_start().starts_with("(part: \"gun.76mm.single\"") && l.trim_end().ends_with("),")), "{text}");
    let again = parse_kit(&text, "again").unwrap();
    assert_eq!(again, kit);
    assert_eq!(kit_to_file_text(&again), text);
    assert_eq!(text::verify_text_hash(&text), Some(kit.hash));
    assert_eq!(text::verify_text_hash(&text.replace("76 mm gun", "77 mm gun")), None);
    assert_eq!(text::verify_text_hash(KIT), None, "an empty stored hash is not a verified one");
}

#[test]
fn parse_kit_normalises_and_refuses_bad_input() {
    let mut kit = parse_kit(KIT, "k").unwrap();
    kit.lineage = (0..40).map(|i| LineageEntry { hash: ContentHash([i as u8; 32]), author: format!("a{i}") }).collect();
    kit.parts[0].rot = PartRot { yaw: 6, tilt: 8 };
    let back = parse_kit(&kit_to_file_text(&kit), "k").unwrap();
    assert_eq!(back.lineage.len(), MAX_LINEAGE);
    assert_eq!(back.lineage[0].author, "a8", "the oldest entries go");
    assert_eq!(back.parts[0].rot, PartRot { yaw: 2, tilt: 2 });

    let unknown = KIT.replace("tray_order: 0,", "tray_order: 0, colour: 3,");
    assert!(matches!(parse_kit(&unknown, "k"), Err(ContentError::Parse { .. })));
    let newer = KIT.replacen("format: 1", "format: 9", 1);
    assert!(matches!(parse_kit(&newer, "k"), Err(ContentError::FormatTooNew { found: 9, .. })));
    let deep = format!("(format: 1, x: {}{})", "[".repeat(200), "]".repeat(200));
    assert!(matches!(parse_kit(&deep, "k"), Err(ContentError::Limit(_))));
    assert!(parse_kit("", "k").is_err());
    assert!(parse_kit("(format: 1", "k").is_err());
}

#[test]
fn text_scanner_ignores_strings_and_comments() {
    assert_eq!(text::max_depth("( a: \"(((((\", b: r#\"[[[[\"#, c: '(', /* ((( /* ((( */ */ d: [()] ) // ((("), 3);
    assert_eq!(text::doc_kind(BP), Some(text::DocKind::Ship));
    assert_eq!(text::doc_kind(KIT), Some(text::DocKind::Kit));
    assert_eq!(text::doc_kind("(name: \"x\")"), None);
    // A `hash:` inside a string or a nested struct is not the field.
    let t = "(format: 1, note: \"hash: \\\"ab\\\"\", graph: (hash: \"cd\"), hash: \"\")";
    let (span, h) = text::stored_hash(t).unwrap();
    assert_eq!(&t[span], "\"\"");
    assert_eq!(h, Some(ContentHash::EMPTY));
    let b = "(header: (format: 1, game_version: \"hash: \", hash: \"\"), embedded_kits: {\"k\": (hash: \"x\")})";
    let (span, _) = text::stored_hash(b).unwrap();
    assert_eq!(span.start, b.rfind("hash: \"\"").unwrap() + 6);
}

/// The removed-part migration's known-footprint path calls [`rotate_offset`]: a removed multi-cell part whose
/// stored part-body cells sit next to a surviving part loses exactly its own cells.
#[test]
fn removed_part_clears_only_its_rotated_footprint() {
    // A catalogue built by hand: "old" names a replacement that no longer exists, so its footprint is known but
    // it has nothing to become (`from_lists` would refuse such a catalogue; old saves can still meet one).
    let cat = catalogue(vec![part("keep", [1, 1, 2], None), part("old", [2, 1, 3], Some("missing"))]);
    let mut bp: Blueprint = crate::data::parse_ron(BP, "bp").unwrap();
    let body = Cell { material: Material::PartBody, ..Cell::EMPTY };
    let src = CellGrid::from_rows(&bp.cells);
    let mut grid = CellGrid::new([8, 2, 4]);
    for (i, c) in src.cells.iter().enumerate() {
        grid.set(src.pos(crate::ids::CellIndex(i as u32)), *c);
    }
    // "old" at (2, 1, 0) turned one yaw step covers x 2..5, z 0..2; "keep" at (5, 1, 0) covers z 0..2.
    let old_rot = PartRot { yaw: 1, tilt: 0 };
    let mut old_cells = BTreeSet::new();
    for x in 0..2 {
        for z in 0..3 {
            let o = rotate_offset(CellPos::new(x, 0, z), [2, 1, 3], old_rot);
            old_cells.insert(CellPos::new(2 + o.x, 1 + o.y, o.z));
        }
    }
    assert_eq!(old_cells.len(), 6);
    let keep_cells = [CellPos::new(5, 1, 0), CellPos::new(5, 1, 1)];
    for c in old_cells.iter().chain(keep_cells.iter()) {
        grid.set(*c, body);
    }
    bp.cells = grid.to_rows();
    bp.parts = vec![
        PartPlacement { part: PartId::new("old"), pos: CellPos::new(2, 1, 0), rot: old_rot, kit: None, label: None, load: None },
        PartPlacement { part: PartId::new("keep"), pos: CellPos::new(5, 1, 0), rot: PartRot::default(), kit: None, label: None, load: None },
    ];
    let removed = crate::migrate::replace_removed_parts(&mut bp, &cat);
    assert_eq!(removed.len(), 1);
    assert_eq!(bp.parts.len(), 1);
    let after = CellGrid::from_rows(&bp.cells);
    for c in &old_cells {
        assert_ne!(after.get(*c).material, Material::PartBody, "{c:?}");
    }
    for c in &keep_cells {
        assert_eq!(after.get(*c).material, Material::PartBody);
    }
    assert_eq!(after.get(CellPos::new(0, 0, 0)).material, Material::Steel);
}
