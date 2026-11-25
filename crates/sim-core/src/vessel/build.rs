//! Building a prefab from a blueprint (see the module docs for the order and the decisions).
use super::VesselPrefab;
use crate::compartments::CompartmentSet;
use crate::grid::VesselGrid;
use crate::hydro::BuoyancyBlocks;
use crate::mass::MassProperties;
use crate::parts::PartRuntime;
use content::blueprint::Blueprint;
use content::catalogue::Catalogue;
use content::cell::CELL_M;
use content::ids::{CellIndex, PartSlot};
use content::part::{Flammable, PartFunction};
use glam::DVec3;
use std::sync::Arc;

pub(super) fn build(bp: &Blueprint, cat: &Catalogue) -> VesselPrefab {
    let design = content::blueprint::io::build_grid(bp, cat);
    let mut parts = crate::parts::build_parts(bp, &design, cat);
    let part_cells: Vec<(PartSlot, Vec<CellIndex>)> = parts.iter().filter(|p| !p.cells.is_empty()).map(|p| (p.slot, p.cells.clone())).collect();
    let flags: Vec<(bool, Flammable)> = parts.iter().map(|p| def_of(cat, p).map_or((false, Flammable::No), |d| (d.watertight, d.flammable))).collect();
    let mut grid = VesselGrid::build(Arc::new(design), &part_cells, &|s: PartSlot| flags.get(s.index()).copied().unwrap_or((false, Flammable::No)));
    let (mut compartments, _) = CompartmentSet::derive(&grid, &parts);
    compartments.link_parts(&mut parts);
    let networks = crate::networks::route(&grid, &compartments, &parts, &bp.networks, cat);
    let dry_mass = crate::mass::dry_mass(&grid, &parts, cat);
    let cuboids = crate::physics::greedy_cuboids(&grid);
    let blocks = crate::hydro::build_blocks(&grid, &compartments, cat.tuning.buoyancy.block_cells);
    let loaded = crate::mass::with_fluids(&dry_mass, &parts, &compartments, cat);
    let waterline_y = waterline(&blocks, &loaded, &compartments, bp.hull.design_waterline_y, grid.size(), &cat.tuning.buoyancy);
    compartments.set_waterline(waterline_y, cat.tuning.flooding.valve_m2);
    let cell_count = grid.design.cells.len();
    let mut sealed = compartments.sealed_air_bits(cell_count).words;
    sealed.resize(cell_count.div_ceil(64), 0);
    grid.sealed_air = sealed;
    let root_cell = crate::structure::pick_root_cell(&grid, dry_mass.com);
    let berths = parts.iter().fold(0u16, |acc, p| match def_of(cat, p).map(|d| &d.function) {
        Some(PartFunction::Berths { count }) => acc.saturating_add(*count as u16),
        _ => acc,
    });
    let dims = solid_extent(&grid);
    let live_compartments = compartments.compartments.iter().filter(|c| !c.is_absorbed()).count();
    let clamp32 = |v: usize| v.min(u32::MAX as usize) as u32;
    let work_counts = [clamp32(cuboids.len()), clamp32(blocks.columns.len()), clamp32(networks.conduits.len()), clamp32(live_compartments)];
    let hash = if bp.header.hash.is_empty() { content::blueprint::io::compute_hash(bp) } else { bp.header.hash };
    VesselPrefab {
        hash,
        name: bp.header.name.clone(),
        grid,
        parts,
        compartments,
        networks,
        dry_mass,
        cuboids,
        blocks,
        root_cell,
        berths,
        waterline_y,
        dims,
        work_counts,
    }
}

fn def_of<'c>(cat: &'c Catalogue, p: &PartRuntime) -> Option<&'c content::part::PartDef> { cat.parts.get_index(p.def.index()).map(|(_, d)| d) }

/// Local height at which the loaded hull floats, at its centre-of-mass station.
fn waterline(blocks: &BuoyancyBlocks, mass: &MassProperties, comps: &CompartmentSet, design_row: i16, size: [u16; 3], tuning: &content::tuning::BuoyancyTuning) -> f64 {
    let top = size[1] as f64 * CELL_M;
    let fallback = ((design_row.max(0) as f64 + 0.5) * CELL_M).clamp(0.0, top);
    if blocks.blocks.is_empty() { return fallback; }
    let eq = crate::hydro::equilibrium(blocks, mass, &[], comps, tuning);
    if !eq.floats { return fallback; }
    let Some((u, c)) = crate::flooding::equilibrium_plane(blocks, &eq) else { return fallback };
    if !(u.y > 1e-6) { return fallback; }
    let com = if mass.com.is_finite() { mass.com } else { DVec3::new(size[0] as f64, 0.0, size[2] as f64) * (0.5 * CELL_M) };
    let y = (c - u.x * com.x - u.z * com.z) / u.y;
    if y.is_finite() { y.clamp(0.0, top) } else { fallback }
}

/// Length, height and beam of the live solid cells in metres.
fn solid_extent(grid: &VesselGrid) -> [f64; 3] {
    let mut lo = [i32::MAX; 3];
    let mut hi = [i32::MIN; 3];
    for c in grid.solid_cells() {
        let p = grid.pos(c);
        let q = [p.x as i32, p.y as i32, p.z as i32];
        for k in 0..3 { lo[k] = lo[k].min(q[k]); hi[k] = hi[k].max(q[k]); }
    }
    if lo[0] > hi[0] { return [0.0; 3]; }
    [0, 1, 2].map(|k| (hi[k] - lo[k] + 1) as f64 * CELL_M)
}
