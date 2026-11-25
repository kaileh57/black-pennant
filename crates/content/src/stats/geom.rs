//! Shared geometry for stats, refit diffs and validation: the structure cells of a blueprint in canonical
//! order without building a dense grid, and the footprint cells of placed parts.
//!
//! Every routine here is bounded by the cell frame (`0..MAX_GRID_DIM` on each axis), so a tiny hostile file
//! that describes runs of 65,535 cells cannot make a validator do unbounded work.
use crate::blueprint::PartPlacement;
use crate::catalogue::{Catalogue, MaterialDef};
use crate::cell::{ArmourTier, Cell, CellPos, CellRows, Material, MAX_GRID_DIM};
use crate::kit::rotate_offset;
use crate::part::PartDef;
use std::collections::BTreeMap;

/// A maximal run of identical solid cells at row (y, z), x in `x0..x1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub x0: u16,
    pub x1: u16,
    pub cell: Cell,
}

/// Structure cells in canonical form: rows ascending by (y, z), runs ascending by x and maximal. Later runs
/// win where stored runs overlap, empty cells are dropped, and anything outside the cell frame is clipped.
#[derive(Clone, Debug, Default)]
pub(crate) struct Structure {
    pub rows: Vec<((u16, u16), Vec<Run>)>,
    /// True when some solid stored cell lay outside the cell frame and was clipped.
    pub out_of_frame: bool,
}

impl Structure {
    pub fn of(rows: &CellRows) -> Structure {
        let dim = MAX_GRID_DIM as i32;
        let mut out_of_frame = false;
        let mut by_row: BTreeMap<(u16, u16), Vec<(u16, u16, Cell)>> = BTreeMap::new();
        for row in &rows.0 {
            for run in &row.runs {
                if run.cell.is_empty() || run.len == 0 {
                    continue;
                }
                let (x0, x1) = (run.x as i32, run.x as i32 + run.len as i32);
                if row.y < 0 || row.z < 0 || row.y as i32 >= dim || row.z as i32 >= dim || x0 < 0 || x1 > dim {
                    out_of_frame = true;
                }
                if row.y < 0 || row.z < 0 || row.y as i32 >= dim || row.z as i32 >= dim {
                    continue;
                }
                let (c0, c1) = (x0.clamp(0, dim), x1.clamp(0, dim));
                if c0 >= c1 {
                    continue;
                }
                by_row.entry((row.y as u16, row.z as u16)).or_default().push((c0 as u16, c1 as u16, run.cell));
            }
        }
        let mut line = vec![Cell::EMPTY; MAX_GRID_DIM as usize];
        let mut out = Vec::with_capacity(by_row.len());
        for (key, runs) in by_row {
            let mut hi = 0usize;
            for &(x0, x1, cell) in &runs {
                line[x0 as usize..x1 as usize].fill(cell);
                hi = hi.max(x1 as usize);
            }
            let mut merged = Vec::new();
            let mut x = 0usize;
            while x < hi {
                let cell = line[x];
                let start = x;
                while x < hi && line[x] == cell {
                    x += 1;
                }
                if !cell.is_empty() {
                    merged.push(Run { x0: start as u16, x1: x as u16, cell });
                }
            }
            line[..hi].fill(Cell::EMPTY);
            if !merged.is_empty() {
                out.push((key, merged));
            }
        }
        Structure { rows: out, out_of_frame }
    }

    /// Every solid cell in ascending cell-index order (y, then z, then x): `(x, y, z, cell)`.
    pub fn cells(&self) -> impl Iterator<Item = (u16, u16, u16, Cell)> + '_ {
        self.rows.iter().flat_map(|&((y, z), ref runs)| runs.iter().flat_map(move |r| (r.x0..r.x1).map(move |x| (x, y, z, r.cell))))
    }

    pub fn cell_count(&self) -> usize { self.rows.iter().flat_map(|(_, r)| r).map(|r| (r.x1 - r.x0) as usize).sum() }

    /// The row's runs, if the row holds any solid cell.
    pub fn row(&self, y: u16, z: u16) -> Option<&[Run]> { self.rows.binary_search_by_key(&(y, z), |(k, _)| *k).ok().map(|i| self.rows[i].1.as_slice()) }
}

/// Footprint cells of a placed part in vessel-local cell coordinates (i32, so nothing wraps), in the part's
/// own box order. Empty for a part the catalogue does not know.
pub(crate) fn footprint(p: &PartPlacement, def: &PartDef) -> Vec<[i32; 3]> {
    let size = def.size;
    let mut out = Vec::with_capacity(size[0] as usize * size[1] as usize * size[2] as usize);
    for x in 0..size[0] {
        for y in 0..size[1] {
            for z in 0..size[2] {
                let o = rotate_offset(CellPos::new(x as i16, y as i16, z as i16), size, p.rot);
                out.push([p.pos.x as i32 + o.x as i32, p.pos.y as i32 + o.y as i32, p.pos.z as i32 + o.z as i32]);
            }
        }
    }
    out
}

/// True when the cell lies inside the cell frame.
pub(crate) fn in_frame(c: [i32; 3]) -> bool { c.iter().all(|v| (0..MAX_GRID_DIM as i32).contains(v)) }

/// Material row, if the catalogue has one (`PartBody` and `Empty` never do).
pub(crate) fn material<'a>(catalogue: &'a Catalogue, m: Material) -> Option<&'a MaterialDef> { catalogue.materials.iter().find(|d| d.material == m) }

/// Per-cell mass and cost of the bare structure and of its armour.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct CellPrice {
    pub structure_kg: f64,
    pub armour_kg: f64,
    pub structure_cost: f64,
    pub armour_cost: f64,
}

/// Cached material lookups: one row per `Material` discriminant.
pub(crate) struct Prices {
    rows: [Option<MaterialDef>; 6],
}
impl Prices {
    pub fn new(catalogue: &Catalogue) -> Prices {
        let all = [Material::Empty, Material::Steel, Material::Aluminium, Material::Composite, Material::Glass, Material::PartBody];
        Prices { rows: all.map(|m| material(catalogue, m).copied()) }
    }
    pub fn of(&self, cell: Cell) -> CellPrice {
        let Some(def) = self.rows[cell.material as usize].as_ref() else { return CellPrice::default() };
        let tier = cell.armour as usize;
        let (armour_kg, armour_cost) = if cell.armour == ArmourTier::None || tier > 4 { (0.0, 0.0) } else { (sane(def.armour_mass_kg[tier - 1]), sane(def.armour_cost[tier - 1])) };
        CellPrice { structure_kg: sane(def.cell_mass_kg), armour_kg, structure_cost: sane(def.cell_cost), armour_cost }
    }
}

/// A non-negative finite number, else 0.
pub(crate) fn sane(v: f64) -> f64 { if v.is_finite() && v > 0.0 { v } else { 0.0 } }
