//! Building and checking the catalogue, and part availability.
use super::{Catalogue, MaterialDef, BUILD_PHASE};
use crate::cell::Material;
use crate::data::{self, parse_ron};
use crate::enums::{Act, GameMode, Phase, Tier};
use crate::error::{ContentError, ContentResult};
use crate::ids::{MunitionId, PartId, PartIdx};
use crate::kit::KitDef;
use crate::part::weapons::{BandDef, CalibreBand, MunitionDef, WeaponTables};
use crate::part::{PartDef, PartFunction};
use crate::tuning::TuningTable;
use indexmap::IndexMap;
use std::collections::BTreeSet;

/// Materials a structure cell can be made of; each needs a [`MaterialDef`] row. `PartBody` takes its mass and
/// behaviour from the part, and `Empty` is no cell.
const STRUCTURAL: [Material; 4] = [Material::Steel, Material::Aluminium, Material::Composite, Material::Glass];

fn dup(what: &str, key: &str) -> ContentError { ContentError::Parse { file: "catalogue".into(), message: format!("duplicate {what} `{key}`") } }

pub(super) fn load_embedded() -> ContentResult<Catalogue> {
    let mut parts = Vec::new();
    for (i, text) in data::PARTS.iter().enumerate() {
        parts.extend(parse_ron::<Vec<PartDef>>(text, &format!("crates/content/data/parts/act{}.ron", i + 1))?);
    }
    let weapons: WeaponTables = parse_ron(data::WEAPONS, "crates/content/data/weapons.ron")?;
    let materials: Vec<MaterialDef> = parse_ron(data::MATERIALS, "crates/content/data/materials.ron")?;
    let tuning: TuningTable = parse_ron(data::TUNING, "crates/content/data/tuning.ron")?;
    // `parse_kit` never touches the process-wide catalogue, so this cannot re-enter `catalogue()`.
    // `blueprints/examples/` holds format samples for modders and tests, not kits a player places: keep them out
    // of the catalogue so the dry-dock tray never lists `kit.example.*` beside the real parts.
    let kits = data::stock_kit_files().filter(|(path, _)| !path.starts_with("examples/")).map(|(path, text)| crate::kit::parse_kit(text, &format!("blueprints/{path}"))).collect::<ContentResult<Vec<_>>>()?;
    from_lists(parts, kits, weapons.munitions, materials, weapons.bands, tuning)
}

/// Munitions a part names.
fn part_munitions(p: &PartDef) -> Vec<&MunitionId> {
    match &p.function {
        PartFunction::Launcher(l) => vec![&l.munition],
        PartFunction::MineRail { mine, .. } => vec![mine],
        PartFunction::DroneBay { drone, .. } => vec![drone],
        PartFunction::HeliDeck { helicopter } => vec![helicopter],
        _ => Vec::new(),
    }
}

pub(super) fn from_lists(mut parts: Vec<PartDef>, mut kits: Vec<KitDef>, mut munitions: Vec<MunitionDef>, materials: Vec<MaterialDef>, bands: Vec<BandDef>, tuning: TuningTable) -> ContentResult<Catalogue> {
    // Dense indices are u16 on the wire and in the simulation.
    if parts.len() > u16::MAX as usize {
        return Err(ContentError::Limit("more than 65535 parts"));
    }
    if munitions.len() > u16::MAX as usize {
        return Err(ContentError::Limit("more than 65535 munitions"));
    }
    parts.sort_by(|a, b| a.id.cmp(&b.id));
    kits.sort_by(|a, b| a.id.cmp(&b.id));
    munitions.sort_by(|a, b| a.id.cmp(&b.id));

    let mut part_map = IndexMap::with_capacity(parts.len());
    for p in parts {
        if part_map.contains_key(&p.id) {
            return Err(dup("part id", p.id.as_str()));
        }
        part_map.insert(p.id.clone(), p);
    }
    let mut munition_map = IndexMap::with_capacity(munitions.len());
    for m in munitions {
        if munition_map.contains_key(&m.id) {
            return Err(dup("munition id", m.id.as_str()));
        }
        munition_map.insert(m.id.clone(), m);
    }
    let mut kit_map = IndexMap::with_capacity(kits.len());
    for k in kits {
        if kit_map.contains_key(&k.id) {
            return Err(dup("kit id", k.id.as_str()));
        }
        kit_map.insert(k.id.clone(), k);
    }

    let unknown = |kind: &'static str, key: &str| ContentError::UnknownRef { kind, key: key.to_string() };
    for p in part_map.values() {
        for m in part_munitions(p) {
            if !munition_map.contains_key(m) {
                return Err(unknown("munition", m.as_str()));
            }
        }
        if let Some(next) = &p.replaced_by {
            if !part_map.contains_key(next) {
                return Err(unknown("part (replaced_by)", next.as_str()));
            }
        }
    }
    // A replacement chain must end: walk each one at most `len` steps.
    for p in part_map.values() {
        let mut cur = p;
        let mut steps = 0;
        while let Some(next) = &cur.replaced_by {
            steps += 1;
            if steps > part_map.len() {
                return Err(ContentError::Parse { file: "catalogue".into(), message: format!("replaced_by cycle through part `{}`", p.id) });
            }
            cur = &part_map[next];
        }
    }
    for m in munition_map.values() {
        for (payload, _) in &m.payload {
            if !munition_map.contains_key(payload) {
                return Err(unknown("munition (payload)", payload.as_str()));
            }
        }
    }
    for k in kit_map.values() {
        for kp in &k.parts {
            if !part_map.contains_key(&kp.part) {
                return Err(unknown("part (in kit)", &format!("{} in {}", kp.part, k.id)));
            }
        }
    }

    for band in CalibreBand::ALL {
        match bands.iter().filter(|b| b.band == band).count() {
            0 => return Err(unknown("calibre band row", &format!("{band:?}"))),
            1 => {}
            _ => return Err(dup("calibre band row", &format!("{band:?}"))),
        }
    }
    let mut seen = BTreeSet::new();
    for m in &materials {
        if !seen.insert(m.material) {
            return Err(dup("material row", &format!("{:?}", m.material)));
        }
    }
    for m in STRUCTURAL {
        if !seen.contains(&m) {
            return Err(unknown("material row", &format!("{m:?}")));
        }
    }

    Ok(Catalogue { parts: part_map, kits: kit_map, munitions: munition_map, materials, bands, tuning })
}

/// The campaign act that first offers a part of this phase; `None` for phases not tied to an act.
fn phase_act(phase: Phase) -> Option<Act> {
    match phase {
        Phase::Act2 => Some(Act::Act2),
        Phase::Act3 => Some(Act::Act3),
        Phase::Act4 => Some(Act::Act4),
        Phase::Slice | Phase::R010 | Phase::R100 | Phase::L4 => None,
    }
}

pub(super) fn available_parts(cat: &Catalogue, mode: GameMode, act: Act, max_tier: Tier, unlocked: &[PartId]) -> Vec<PartIdx> {
    let unlocked: BTreeSet<&PartId> = unlocked.iter().collect();
    cat.parts
        .values()
        .enumerate()
        .filter(|(_, p)| p.phase <= BUILD_PHASE && p.replaced_by.is_none())
        .filter(|(_, p)| match mode {
            GameMode::Campaign => p.tier <= max_tier && unlocked.contains(&p.id) && phase_act(p.phase).is_none_or(|a| a <= act),
            _ => true,
        })
        .map(|(i, _)| PartIdx(i as u16))
        .collect()
}
