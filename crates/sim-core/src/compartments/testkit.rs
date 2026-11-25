//! Hand-built fixtures for the tests of the compartments, mass, networks, crew and fire units.
use crate::grid::VesselGrid;
use crate::networks::PartLinks;
use crate::parts::{PartRuntime, PartState};
use content::catalogue::{Catalogue, MaterialDef};
use content::cell::{ArmourTier, Cell, CellGrid, CellPos, CellShape, Material};
use content::enums::{DamageLevel, NetworkKind};
use content::ids::{CellIndex, PartId, PartIdx, PartSlot, TextKey};
use content::part::{Flammable, PartCategory, PartDef, PartFunction, PortDef, PortRole, ProcModel};
use std::sync::Arc;

pub const STEEL: Cell = Cell { material: Material::Steel, armour: ArmourTier::None, paint: 0, shape: CellShape::CUBE };
pub const BODY: Cell = Cell { material: Material::PartBody, armour: ArmourTier::None, paint: 0, shape: CellShape::CUBE };

/// Hollow box (shell one cell thick) with transverse bulkheads at the given x and decks at the given y.
pub fn box_hull(size: [u16; 3], bulkheads: &[i16], decks: &[i16]) -> CellGrid {
    let mut g = CellGrid::new(size);
    for y in 0..size[1] as i16 {
        for z in 0..size[2] as i16 {
            for x in 0..size[0] as i16 {
                let shell = x == 0 || y == 0 || z == 0 || x == size[0] as i16 - 1 || y == size[1] as i16 - 1 || z == size[2] as i16 - 1;
                if shell || bulkheads.contains(&x) || decks.contains(&y) { g.set(CellPos::new(x, y, z), STEEL); }
            }
        }
    }
    g
}

/// A part placed on a set of cells; `def` indexes the test catalogue.
pub fn part(slot: u16, def: u16, cells: Vec<CellIndex>, grid: &CellGrid, state: PartState) -> PartRuntime {
    let mut centre = glam::DVec3::ZERO;
    for c in &cells { centre += glam::DVec3::from(grid.pos(*c).centre_m()); }
    if !cells.is_empty() { centre /= cells.len() as f64; }
    PartRuntime {
        slot: PartSlot(slot), def: PartIdx(def), pos: cells.first().map(|c| grid.pos(*c)).unwrap_or_default(), rot: Default::default(), cells, centre,
        compartment: None, health: 1.0, level: DamageLevel::Intact, links: PartLinks::default(), priority: 0, state,
    }
}

/// Stamp part-body cells into the design and build the vessel grid with per-part flags.
pub fn build(mut g: CellGrid, parts: &[(PartSlot, Vec<CellIndex>, bool, Flammable)]) -> VesselGrid {
    for (_, cells, _, _) in parts { for c in cells { g.cells[c.index()] = BODY; } }
    let pc: Vec<(PartSlot, Vec<CellIndex>)> = parts.iter().map(|(s, c, _, _)| (*s, c.clone())).collect();
    let flags: Vec<(PartSlot, bool, Flammable)> = parts.iter().map(|(s, _, w, f)| (*s, *w, *f)).collect();
    VesselGrid::build(Arc::new(g), &pc, &move |s| flags.iter().find(|f| f.0 == s).map(|f| (f.1, f.2)).unwrap_or((false, Flammable::No)))
}

pub fn idx(g: &CellGrid, x: i16, y: i16, z: i16) -> CellIndex { g.index(CellPos::new(x, y, z)).expect("in bounds") }

/// Cells of an axis-aligned block, ascending.
pub fn block(g: &CellGrid, min: [i16; 3], max: [i16; 3]) -> Vec<CellIndex> {
    let mut v = Vec::new();
    for y in min[1]..=max[1] { for z in min[2]..=max[2] { for x in min[0]..=max[0] { v.push(idx(g, x, y, z)); } } }
    v.sort();
    v
}

pub fn def(id: &str, function: PartFunction, ports: Vec<PortDef>, mass_kg: f64) -> PartDef {
    PartDef {
        id: PartId::new(id), name: TextKey::new("n"), description: TextKey::new("d"), category: PartCategory::Hull, tier: Default::default(),
        phase: Default::default(), size: [1, 1, 1], placement: Default::default(), mass_kg, cost: 1, ports, power_priority: 0, buffer_s: None,
        watertight: false, flammable: Flammable::No, rcs_factor: 1.0, tags: vec![], function, model: ProcModel::default(), icon: Default::default(),
        replaced_by: None, standard_limit: None,
    }
}

pub fn port(network: NetworkKind, role: PortRole, rate: f64, capacity: f64) -> PortDef { PortDef { network, role, rate, capacity } }

/// Catalogue from part defs in index order, with steel at 60 kg per cell.
pub fn catalogue(defs: Vec<PartDef>) -> Catalogue {
    let mut c = Catalogue::default();
    for d in defs { c.parts.insert(d.id.clone(), d); }
    c.materials.push(MaterialDef { material: Material::Steel, cell_mass_kg: 60.0, armour_mass_kg: [10.0, 30.0, 80.0, 160.0], cell_cost: 0.0, armour_cost: [0.0; 4], rcs_factor: 1.0, ir_factor: 1.0 });
    c
}
