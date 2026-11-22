//! `compute_stats`: static bookkeeping from blueprint data alone.
//!
//! Mass model:
//! - A structure cell weighs its material's `cell_mass_kg` plus its armour tier's `armour_mass_kg`. A cell a
//!   part occupies becomes part body: the part's own mass replaces the structure mass, but the armour painted
//!   on that cell stays (as [`crate::blueprint::io::build_grid`] keeps it) and still counts.
//! - Parts weigh `mass_kg`, spread evenly over their footprint.
//! - Loads (full-load only): fuel tanks at their load fraction times [`super::FUEL_KG_PER_L`], magazines at
//!   their rounds fraction of `ammo_kg`, launchers, mine rails, drone bays and helicopter decks full of their
//!   munition, ballast at its set kilograms. An unset load is a full load; a non-finite load counts as full, so
//!   bad data can only overstate tonnage, never hide it under a cap.
//!
//! Sums run serially: structure cells in ascending cell index, then parts in ascending slot.
use super::geom::{footprint, sane, Prices, Structure};
use super::{BlueprintStats, FUEL_KG_PER_L};
use crate::blueprint::{Blueprint, PartPlacement};
use crate::catalogue::Catalogue;
use crate::cell::CELL_M;
use crate::part::{PartDef, PartFunction};

/// Fraction 0..=1 from an optional load; unset or non-finite means full.
pub(crate) fn load_fraction(load: Option<f64>) -> f64 {
    match load {
        Some(v) if v.is_finite() => v.clamp(0.0, 1.0),
        _ => 1.0,
    }
}

/// Loaded masses one part carries at full load, kilograms: (fuel litres, ammunition kg, ballast kg).
pub(crate) fn part_loads(p: &PartPlacement, def: &PartDef, catalogue: &Catalogue) -> (f64, f64, f64) {
    let munition_kg = |id: &crate::ids::MunitionId| catalogue.munition(id).map_or(0.0, |m| sane(m.mass_kg));
    match &def.function {
        PartFunction::FuelTank { litres } => (sane(*litres) * load_fraction(p.load), 0.0, 0.0),
        PartFunction::Magazine { ammo_kg, .. } => (0.0, sane(*ammo_kg) * load_fraction(p.load), 0.0),
        PartFunction::Launcher(l) => (0.0, l.cells as f64 * munition_kg(&l.munition), 0.0),
        PartFunction::MineRail { mine, capacity } => (0.0, *capacity as f64 * munition_kg(mine), 0.0),
        PartFunction::DroneBay { drone, count, .. } => (0.0, *count as f64 * munition_kg(drone), 0.0),
        PartFunction::HeliDeck { helicopter } => (0.0, munition_kg(helicopter), 0.0),
        PartFunction::Ballast { max_kg } => {
            let max = sane(*max_kg);
            let kg = match p.load {
                Some(v) if v.is_finite() => v.clamp(0.0, max),
                _ => max,
            };
            (0.0, 0.0, kg)
        }
        _ => (0.0, 0.0, 0.0),
    }
}

pub(super) fn compute_stats(bp: &Blueprint, catalogue: &Catalogue) -> BlueprintStats {
    let mut st = BlueprintStats::default();
    let prices = Prices::new(catalogue);
    let structure = Structure::of(&bp.cells);

    // Part footprints, resolved once, and the sorted set of part cells (keyed in cell-index order).
    let defs: Vec<Option<&PartDef>> = bp.parts.iter().map(|p| catalogue.part(&p.part)).collect();
    let prints: Vec<Vec<[i32; 3]>> = bp.parts.iter().zip(&defs).map(|(p, d)| d.map(|d| footprint(p, d)).unwrap_or_default()).collect();
    let mut part_cells: Vec<(i32, i32, i32)> = prints.iter().flatten().map(|c| (c[1], c[2], c[0])).collect();
    part_cells.sort_unstable();
    part_cells.dedup();

    let mut lo = [i32::MAX; 3];
    let mut hi = [i32::MIN; 3];
    let mut grow = |c: [i32; 3]| {
        for k in 0..3 {
            lo[k] = lo[k].min(c[k]);
            hi[k] = hi[k].max(c[k]);
        }
    };

    let (mut light_kg, mut cost) = (0.0f64, 0.0f64);
    let mut moment = [0.0f64; 3];
    let mut covered = 0usize;
    for (x, y, z, cell) in structure.cells() {
        let price = prices.of(cell);
        let under_part = part_cells.binary_search(&(y as i32, z as i32, x as i32)).is_ok();
        covered += under_part as usize;
        let (kg, c) = if under_part { (price.armour_kg, price.armour_cost) } else { (price.structure_kg + price.armour_kg, price.structure_cost + price.armour_cost) };
        light_kg += kg;
        cost += c;
        let centre = [(x as f64 + 0.5) * CELL_M, (y as f64 + 0.5) * CELL_M, (z as f64 + 0.5) * CELL_M];
        for k in 0..3 {
            moment[k] += kg * centre[k];
        }
        grow([x as i32, y as i32, z as i32]);
    }
    st.cell_count = (structure.cell_count() + part_cells.len() - covered).min(u32::MAX as usize) as u32;

    let (mut fuel_l, mut ammo_kg, mut ballast_kg) = (0.0f64, 0.0f64, 0.0f64);
    let mut part_cost: u64 = 0;
    for ((p, def), cells) in bp.parts.iter().zip(&defs).zip(&prints) {
        let Some(def) = def else { continue };
        let count = st.parts_by_category.entry(def.category).or_insert(0);
        *count = count.saturating_add(1);
        match def.function {
            PartFunction::Berths { count } => st.berths = st.berths.saturating_add(count as u16),
            PartFunction::PassengerBerths { count } => st.passenger_berths = st.passenger_berths.saturating_add(count as u16),
            _ => {}
        }
        part_cost += def.cost as u64;
        let dry = sane(def.mass_kg);
        let (fuel, ammo, ballast) = part_loads(p, def, catalogue);
        fuel_l += fuel;
        ammo_kg += ammo;
        ballast_kg += ballast;
        light_kg += dry;
        let total = dry + fuel * FUEL_KG_PER_L + ammo + ballast;
        if !cells.is_empty() {
            let mut c = [0.0f64; 3];
            for cell in cells {
                grow(*cell);
                for k in 0..3 {
                    c[k] += (cell[k] as f64 + 0.5) * CELL_M;
                }
            }
            let n = cells.len() as f64;
            for k in 0..3 {
                moment[k] += total * (c[k] / n);
            }
        }
    }

    let full_kg = light_kg + fuel_l * FUEL_KG_PER_L + ammo_kg + ballast_kg;
    st.light_t = light_kg / 1000.0;
    st.tonnage_t = full_kg / 1000.0;
    st.fuel_l = fuel_l;
    // `as` saturates: a cost past u32 reads as u32::MAX, never wraps.
    st.cost = (cost.round() + part_cost as f64).min(u32::MAX as f64) as u32;
    st.part_count = bp.parts.len().min(u16::MAX as usize) as u16;
    if lo[0] <= hi[0] {
        st.length_m = (hi[0] - lo[0] + 1) as f64 * CELL_M;
        st.height_m = (hi[1] - lo[1] + 1) as f64 * CELL_M;
        st.beam_m = (hi[2] - lo[2] + 1) as f64 * CELL_M;
        st.com_m = if full_kg > 0.0 {
            moment.map(|m| m / full_kg)
        } else {
            [0, 1, 2].map(|k| (lo[k] + hi[k] + 1) as f64 * 0.5 * CELL_M)
        };
    }
    st
}
