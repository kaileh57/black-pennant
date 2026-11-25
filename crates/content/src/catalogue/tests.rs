use super::*;
use crate::enums::Phase;
use crate::error::ContentError;
use crate::part::PartTag;

fn part(id: &str, tier: &str, phase: &str, function: &str, extra: &str) -> PartDef {
    ron::from_str(&format!("(id: \"{id}\", name: \"n\", description: \"d\", category: Hull, tier: {tier}, phase: {phase}, size: (1, 1, 1), mass_kg: 1.0, cost: 1, function: {function}{extra})")).unwrap()
}
fn simple(id: &str) -> PartDef { part(id, "T1", "Slice", "Structure", "") }

fn munition(id: &str, payload: &str) -> MunitionDef {
    ron::from_str(&format!(
        "(id: \"{id}\", name: \"n\", kind: AntiShipMissile, domain: Weapon, speed_mps: 250.0, range_m: 30000.0, max_accel: 50.0, cruise_alt_m: 5.0, fuse: Contact, he_b: 6, he_r_cells: 2, seeker: ActiveRadar, radius_m: 0.5, hp: 1, rcs_m2: 0.1, mass_kg: 600.0, cost: 50, payload: [{payload}])"
    ))
    .unwrap()
}

fn materials() -> Vec<MaterialDef> {
    crate::data::parse_ron(crate::data::MATERIALS, "materials").unwrap()
}
fn bands() -> Vec<BandDef> { crate::part::weapons::CalibreBand::ALL.into_iter().map(BandDef::spec_default).collect() }

fn kit(id: &str, parts: &[&str], tags: &[PartTag], tray: u16) -> KitDef {
    let mut k = crate::kit::parse_kit(include_str!("../../../../blueprints/examples/minimal.kit.ron"), "k").unwrap();
    k.id = KitId::new(id);
    k.parts = parts.iter().map(|p| crate::kit::KitPart { part: PartId::new(*p), offset: Default::default(), rot: Default::default() }).collect();
    k.tags = tags.to_vec();
    k.tray_order = tray;
    k
}

fn build(parts: Vec<PartDef>, kits: Vec<KitDef>, munitions: Vec<MunitionDef>) -> ContentResult<Catalogue> { Catalogue::from_lists(parts, kits, munitions, materials(), bands(), TuningTable::default()) }

#[test]
fn embedded_catalogue_loads() {
    let c = catalogue();
    assert!(!c.parts.is_empty());
    assert!(c.parts.keys().zip(c.parts.keys().skip(1)).all(|(a, b)| a < b), "parts ascend by key");
    for k in c.kits.values() {
        assert_eq!(k.hash, crate::kit::kit_hash(k));
    }
    for band in crate::part::weapons::CalibreBand::ALL {
        assert_eq!(c.band(band).band, band);
    }
    assert_eq!(c.material(crate::cell::Material::Steel).material, crate::cell::Material::Steel);
}

#[test]
fn example_kits_stay_out_of_the_catalogue() {
    assert!(crate::data::stock_kit_files().any(|(p, _)| p.starts_with("examples/")), "the example kit is still committed");
    let c = catalogue();
    assert!(!c.kits.is_empty());
    for k in c.kits.values() {
        assert!(!k.id.as_str().starts_with("kit.example."), "{} is a format example, not a player kit", k.id.as_str());
    }
}

#[test]
fn lists_are_sorted_and_indexed() {
    let c = build(vec![simple("z"), simple("a"), simple("m")], vec![kit("k2", &["a"], &[], 0), kit("k1", &["z"], &[], 0)], vec![munition("w", "")]).unwrap();
    assert_eq!(c.parts.keys().map(|k| k.as_str()).collect::<Vec<_>>(), ["a", "m", "z"]);
    assert_eq!(c.part_idx(&PartId::new("m")), Some(PartIdx(1)));
    assert_eq!(c.part_id_at(PartIdx(2)).as_str(), "z");
    assert_eq!(c.kits.keys().map(|k| k.as_str()).collect::<Vec<_>>(), ["k1", "k2"]);
}

#[test]
fn bad_references_are_refused() {
    let dup = build(vec![simple("a"), simple("a")], vec![], vec![]);
    assert!(matches!(dup, Err(ContentError::Parse { .. })));
    let kit_ref = build(vec![simple("a")], vec![kit("k", &["nope"], &[], 0)], vec![]);
    assert!(matches!(kit_ref, Err(ContentError::UnknownRef { .. })));
    let launcher = part("l", "T1", "Slice", "Launcher((kind: Missile, munition: \"asm\", cells: 4, interval_s: 2.0, reload_s: 0.0))", "");
    assert!(matches!(build(vec![launcher.clone()], vec![], vec![]), Err(ContentError::UnknownRef { .. })));
    assert!(build(vec![launcher], vec![], vec![munition("asm", "")]).is_ok());
    let rail = part("r", "T1", "Slice", "MineRail(mine: \"mine\", capacity: 4)", "");
    assert!(matches!(build(vec![rail], vec![], vec![]), Err(ContentError::UnknownRef { .. })));
    assert!(matches!(build(vec![], vec![], vec![munition("a", "(\"b\", 2)")]), Err(ContentError::UnknownRef { .. })));
    let replaced = part("old", "T1", "Slice", "Structure", ", replaced_by: Some(\"gone\")");
    assert!(matches!(build(vec![replaced], vec![], vec![]), Err(ContentError::UnknownRef { .. })));
    let cyc = vec![part("a", "T1", "Slice", "Structure", ", replaced_by: Some(\"b\")"), part("b", "T1", "Slice", "Structure", ", replaced_by: Some(\"a\")")];
    assert!(matches!(build(cyc, vec![], vec![]), Err(ContentError::Parse { .. })));
    let mut no_band = bands();
    no_band.pop();
    assert!(Catalogue::from_lists(vec![], vec![], vec![], materials(), no_band, TuningTable::default()).is_err());
    let mut two_band = bands();
    two_band.push(BandDef::spec_default(crate::part::weapons::CalibreBand::Auto));
    assert!(Catalogue::from_lists(vec![], vec![], vec![], materials(), two_band, TuningTable::default()).is_err());
    let mut no_glass = materials();
    no_glass.retain(|m| m.material != crate::cell::Material::Glass);
    assert!(Catalogue::from_lists(vec![], vec![], vec![], no_glass, bands(), TuningTable::default()).is_err());
}

#[test]
fn availability_by_mode_act_and_tier() {
    let parts = vec![
        part("t1", "T1", "Slice", "Structure", ""),
        part("t3", "T3", "R010", "Structure", ""),
        part("act2", "T2", "Act2", "Structure", ""),
        part("retired", "T1", "Slice", "Structure", ", replaced_by: Some(\"t1\")"),
        part("late", "T1", "R100", "Structure", ""),
    ];
    let c = build(parts, vec![], vec![]).unwrap();
    let ids = |v: Vec<PartIdx>| v.into_iter().map(|i| c.part_id_at(i).as_str().to_string()).collect::<Vec<_>>();
    let all: Vec<PartId> = c.parts.keys().cloned().collect();
    assert_eq!(ids(c.available_parts(GameMode::Sandbox, Act::Act1, Tier::T1, &[])), ["act2", "late", "t1", "t3"]);
    assert_eq!(ids(c.available_parts(GameMode::PvpElimination, Act::Act1, Tier::T1, &[])).len(), 4);
    assert_eq!(ids(c.available_parts(GameMode::Campaign, Act::Act1, Tier::T5, &all)), ["late", "t1", "t3"]);
    assert_eq!(ids(c.available_parts(GameMode::Campaign, Act::Act2, Tier::T2, &all)), ["act2", "late", "t1"]);
    assert_eq!(ids(c.available_parts(GameMode::Campaign, Act::Act4, Tier::T5, &[PartId::new("t3")])), ["t3"]);
    assert!(Phase::L4 <= BUILD_PHASE);
}

#[test]
fn kits_for_tag_are_assemblies_in_tray_order() {
    let mut officer = kit("k.officer", &[], &[PartTag::Propulsion], 0);
    officer.kind = crate::kit::KitKind::Officer;
    let kits = vec![kit("k.b", &["a"], &[PartTag::Propulsion], 2), kit("k.a", &["a"], &[PartTag::Propulsion, PartTag::Crane], 2), kit("k.c", &["a"], &[PartTag::Propulsion], 1), kit("k.d", &["a"], &[PartTag::Crane], 0), officer];
    let c = build(vec![simple("a")], kits, vec![]).unwrap();
    let ids: Vec<&str> = c.kits_for_tag(PartTag::Propulsion).iter().map(|k| k.id.as_str()).collect();
    assert_eq!(ids, ["k.c", "k.a", "k.b"]);
    assert!(c.kits_for_tag(PartTag::MineRail).is_empty());
}
