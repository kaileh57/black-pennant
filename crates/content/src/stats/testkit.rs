//! Test fixtures shared by the stats, validate and campaign tests: a small catalogue with round numbers and
//! hand-built hulls.
use crate::blueprint::{Blueprint, PartPlacement};
use crate::catalogue::{Catalogue, MaterialDef};
use crate::cell::{ArmourTier, Cell, CellPos, CellRow, CellRows, CellRun, CellShape, Material};
use crate::hull::{Hull, HullSource};
use crate::ids::PartId;
use crate::part::PartDef;

pub fn part(id: &str, size: (u8, u8, u8), placement: &str, mass: f64, cost: u32, function: &str, extra: &str) -> PartDef {
    ron::from_str(&format!(
        "(id: \"{id}\", name: \"n\", description: \"d\", category: Hull, tier: T1, size: ({}, {}, {}), placement: {placement}, mass_kg: {mass:?}, cost: {cost}, function: {function}{extra})",
        size.0, size.1, size.2
    ))
    .unwrap()
}

/// Steel cells 100 kg and 1 S; armour tiers +10/20/30/40 kg and +0.5/1/2/4 S.
pub fn catalogue() -> Catalogue {
    let steel = MaterialDef { material: Material::Steel, cell_mass_kg: 100.0, armour_mass_kg: [10.0, 20.0, 30.0, 40.0], cell_cost: 1.0, armour_cost: [0.5, 1.0, 2.0, 4.0], rcs_factor: 1.0, ir_factor: 1.0 };
    let parts = vec![
        part("box", (1, 1, 1), "Anywhere", 1000.0, 10, "Structure", ""),
        part("tank", (2, 1, 1), "Internal", 200.0, 5, "FuelTank(litres: 1000.0)", ""),
        part("gun", (1, 2, 1), "Weather", 500.0, 50, "Structure", ", standard_limit: Some(1)"),
        part("prop", (1, 1, 1), "Underwater", 100.0, 20, "Structure", ""),
        part("davit", (1, 1, 1), "DeckEdge", 100.0, 20, "RhibDavit", ""),
        part("ballast", (1, 1, 1), "Internal", 50.0, 1, "Ballast(max_kg: 2000.0)", ""),
        part("mag", (1, 1, 1), "Internal", 300.0, 30, "Magazine(calibre_mm: 76, rounds: 100, ammo_kg: 700.0)", ""),
        part("berth", (1, 1, 1), "Internal", 100.0, 5, "Berths(count: 4)", ""),
    ];
    Catalogue { parts: parts.into_iter().map(|p| (p.id.clone(), p)).collect(), materials: vec![steel], ..Default::default() }
}

pub fn steel(armour: ArmourTier) -> Cell { Cell { material: Material::Steel, armour, paint: 0, shape: CellShape::CUBE } }

/// Rows from a list of solid cells.
pub fn rows(cells: &[(i16, i16, i16, Cell)]) -> CellRows {
    let mut v: Vec<CellRow> = Vec::new();
    for &(x, y, z, c) in cells {
        match v.iter_mut().find(|r| r.y == y && r.z == z) {
            Some(r) => r.runs.push(CellRun { x, len: 1, cell: c }),
            None => v.push(CellRow { y, z, runs: vec![CellRun { x, len: 1, cell: c }] }),
        }
    }
    v.sort_by_key(|r| (r.y, r.z));
    CellRows(v)
}

/// A closed steel box shell of `l x h x w` cells starting at y = `y0`: every cell on the box surface.
pub fn shell_box(l: i16, h: i16, w: i16, y0: i16) -> CellRows {
    let mut cells = Vec::new();
    for y in y0..y0 + h {
        for z in 0..w {
            for x in 0..l {
                if x == 0 || x == l - 1 || y == y0 || y == y0 + h - 1 || z == 0 || z == w - 1 {
                    cells.push((x, y, z, steel(ArmourTier::None)));
                }
            }
        }
    }
    rows(&cells)
}

/// A freeform design with the given cells, one paint entry and the waterline at `waterline_y`.
pub fn blueprint(cells: CellRows, waterline_y: i16) -> Blueprint {
    let mut bp = crate::blueprint::io::new_blueprint("test", 20.0);
    bp.hull = Hull { source: HullSource::Freeform, planes: Vec::new(), design_waterline_y: waterline_y };
    bp.cells = cells;
    bp.paint.truncate(1);
    bp
}

pub fn place(part: &str, x: i16, y: i16, z: i16) -> PartPlacement { PartPlacement { part: PartId::new(part), pos: CellPos::new(x, y, z), rot: Default::default(), kit: None, label: None, load: None } }
