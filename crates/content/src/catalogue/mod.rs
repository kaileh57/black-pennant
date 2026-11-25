//! Unit `content-catalogue`. The loaded, indexed catalogue of parts, kits, munitions, materials and bands.
//! DESIGN.md L136-151, L801-807, L811-822 (cost anchor).
use crate::cell::Material;
use crate::enums::{Act, GameMode, Tier};
use crate::error::ContentResult;
use crate::ids::{KitId, MunitionId, MunitionIdx, PartId, PartIdx};
use crate::kit::KitDef;
use crate::part::weapons::{BandDef, CalibreBand, MunitionDef};
use crate::part::PartDef;
use crate::tuning::TuningTable;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

mod load;
#[cfg(test)]
mod tests;

/// Latest release phase this build ships. Sandbox, skirmish and PvP offer every part whose
/// [`PartDef::phase`] is at or before it (L140, L807). The whole game is in this build, so it is the last phase;
/// a cut-down build lowers it and every mode follows.
pub const BUILD_PHASE: crate::enums::Phase = crate::enums::Phase::L4;

/// Per-material numbers (L427 material factors, mass for the Float gauge).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialDef {
    pub material: Material,
    /// Mass of one unarmoured 0.5 m structural cell in kilograms (plating and framing, not a solid block).
    pub cell_mass_kg: f64,
    /// Extra mass per cell for each armour tier 5, 15, 40, 80 mm, kilograms.
    pub armour_mass_kg: [f64; 4],
    /// Salvage cost per cell and per armoured cell by tier.
    pub cell_cost: f64,
    pub armour_cost: [f64; 4],
    /// Radar cross-section factor for projected area (L427).
    pub rcs_factor: f32,
    /// Infrared and visual factors for signature (L404).
    pub ir_factor: f32,
}

/// Everything the game knows about parts. Maps are in ascending key order so dense indices are deterministic.
#[derive(Clone, Debug, Default)]
pub struct Catalogue {
    pub parts: IndexMap<PartId, PartDef>,
    pub kits: IndexMap<KitId, KitDef>,
    pub munitions: IndexMap<MunitionId, MunitionDef>,
    pub materials: Vec<MaterialDef>,
    pub bands: Vec<BandDef>,
    pub tuning: TuningTable,
}

impl Catalogue {
    /// Build the catalogue from the embedded RON in [`crate::data`]: parse every file, sort by key, check that
    /// every reference resolves (kit parts, launcher munitions, replaced_by), that every band has a row and
    /// that ids are unique. Unit: `content-catalogue`.
    pub fn load_embedded() -> ContentResult<Catalogue> { load::load_embedded() }
    /// Build from already parsed lists (tests, part packs after 1.0). Unit: `content-catalogue`.
    pub fn from_lists(parts: Vec<PartDef>, kits: Vec<KitDef>, munitions: Vec<MunitionDef>, materials: Vec<MaterialDef>, bands: Vec<BandDef>, tuning: TuningTable) -> ContentResult<Catalogue> { load::from_lists(parts, kits, munitions, materials, bands, tuning) }

    pub fn part(&self, id: &PartId) -> Option<&PartDef> { self.parts.get(id) }
    pub fn part_idx(&self, id: &PartId) -> Option<PartIdx> { self.parts.get_index_of(id).map(|i| PartIdx(i as u16)) }
    pub fn part_at(&self, idx: PartIdx) -> &PartDef { &self.parts[idx.index()] }
    pub fn part_id_at(&self, idx: PartIdx) -> &PartId { self.parts.get_index(idx.index()).expect("part index").0 }
    pub fn kit(&self, id: &KitId) -> Option<&KitDef> { self.kits.get(id) }
    pub fn munition(&self, id: &MunitionId) -> Option<&MunitionDef> { self.munitions.get(id) }
    pub fn munition_idx(&self, id: &MunitionId) -> Option<MunitionIdx> { self.munitions.get_index_of(id).map(|i| MunitionIdx(i as u16)) }
    pub fn munition_at(&self, idx: MunitionIdx) -> &MunitionDef { &self.munitions[idx.index()] }
    pub fn band(&self, band: CalibreBand) -> &BandDef { self.bands.iter().find(|b| b.band == band).expect("band row") }
    pub fn material(&self, m: Material) -> &MaterialDef { self.materials.iter().find(|d| d.material == m).expect("material row") }

    /// Parts a player may place: in campaign, parts of an unlocked tier that the save's unlock list holds; in
    /// every other mode, every part whose phase is in the build (L140, L807). Unit: `content-catalogue`.
    ///
    /// Parts a later release retired (`replaced_by` set) are never offered: loading migrates them away. In
    /// campaign a part added by a later act (phase `Act2`..`Act4`) also waits for that act. Ascending index.
    pub fn available_parts(&self, mode: GameMode, act: Act, max_tier: Tier, unlocked: &[PartId]) -> Vec<PartIdx> { load::available_parts(self, mode, act, max_tier, unlocked) }
    /// The curated, tag-linked kit list the Quartermaster swaps from for one constraint tag (L801-807).
    /// Unit: `content-catalogue`.
    ///
    /// Assembly kits whose `tags` hold `tag`, in tray order then id, so the Quartermaster tries candidates in
    /// the same order on every machine. Officer kits carry no parts and are never refit candidates.
    pub fn kits_for_tag(&self, tag: crate::part::PartTag) -> Vec<&KitDef> {
        let mut out: Vec<&KitDef> = self.kits.values().filter(|k| k.kind == crate::kit::KitKind::Assembly && k.tags.contains(&tag)).collect();
        out.sort_by(|a, b| (a.tray_order, &a.id).cmp(&(b.tray_order, &b.id)));
        out
    }
}

/// Process-wide catalogue built once from embedded data. Panics with the load error if the embedded data is
/// invalid (CI guards that). Unit: `content-catalogue`.
pub fn catalogue() -> &'static Catalogue {
    static CATALOGUE: std::sync::OnceLock<Catalogue> = std::sync::OnceLock::new();
    CATALOGUE.get_or_init(|| Catalogue::load_embedded().unwrap_or_else(|e| panic!("embedded catalogue is invalid: {e}")))
}
