//! Part of unit `gd-render-hull`, read by `gd-render-parts` too. What the renderer needs of one design, in
//! one shape whatever it came from: a full blueprint with its built `ShipDesign`, the exterior-only form the
//! other PvP team gets (L945), or the dry dock's working design.
//!
//! # Where designs come from
//! `SimEvent::EntityRevealed` names a design by hash only. [`lookup`] resolves it from, in order:
//! 1. `Services::designs` (the session's store: its own designs, stock classes, every
//!    `worker::JobResult::BuiltReceived`);
//! 2. `Services::design` (the design last saved in the dry dock), when its hash matches.
//!
//! A hash neither holds yet is not an error: the caller keeps the vessel pending and asks again next frame,
//! so a design that streams in late appears as soon as it is stored. The converted form is kept per hash for
//! the life of the session (see [`forget_all`]), so hull and parts convert once between them.
//!
//! # Part cells
//! The dense grid stamps every part's footprint as `Material::PartBody` cells
//! (`content::blueprint::io::build_grid`), which is what the damage stream indexes. Meshed as they are, a
//! turret would be a painted box with its model inside. So [`DrawDesign::mesh_cells`] is the grid with the
//! part cells that stand in the open taken out: a part cell is removed when it touches exposed air, or
//! touches a removed part cell. Parts buried in the hull stay solid, so a sealed compartment meshes no
//! hidden faces around its machinery and a breach shows a primer block. The dry dock and exterior forms
//! carry no sealed-air state, so there every part with air beside it is taken out and its model shows in
//! the cutaway. Picking, markings and the water's hull mask read the unchanged grid.
use crate::worker::ReceivedDesign;
use content::blueprint::{Blueprint, ExteriorBlueprint, Marking, PaintEntry, PartPlacement};
use content::catalogue::Catalogue;
use content::cell::{Cell, CellGrid, CellPos, Material};
use content::ids::{ContentHash, PartSlot};
use sim_systems::ship::ShipDesign;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Most palette entries a blueprint may carry (the 4-bit swatch field, L130).
pub(crate) const MAX_PALETTE: usize = 16;
/// Linear grey used when a blueprint carries no paint at all (a caller error; never a panic).
pub(crate) const FALLBACK_PAINT: [f32; 3] = [0.28, 0.28, 0.28];

/// One design as the renderer reads it. Immutable and shared by every vessel of the design.
#[derive(Clone, Debug)]
pub(crate) struct DrawDesign {
    pub hash: ContentHash,
    /// The intact cells, part footprints included: the grid the damage stream indexes.
    pub grid: Arc<CellGrid>,
    /// `grid.cells` without the part cells that stand in the open (module docs): what the hull mesher reads.
    pub mesh_cells: Vec<Cell>,
    /// Sealed-air bits of the intact design; empty for exterior forms and dock designs.
    pub sealed_air: Vec<u64>,
    /// Air cells of each compartment, ascending id; empty when the design carries no compartments.
    pub compartments: Vec<(u16, Vec<u32>)>,
    /// Paint table in linear RGB, at most 16 entries, never empty.
    pub palette: Vec<[f32; 3]>,
    /// The first hull-use swatch: what a part wears when no hull cell sits under it.
    pub hull_paint: [f32; 3],
    pub markings: Vec<Marking>,
    /// Placed parts with their slots, ascending slot.
    pub parts: Vec<(PartSlot, PartPlacement)>,
    /// Design waterline in vessel-local metres, when the design says (exterior forms do not).
    pub waterline_m: Option<f32>,
    /// Top of the main deck in vessel-local metres (faces above it are superstructure), when known.
    pub deck_m: Option<f32>,
}

impl DrawDesign {
    fn palette_of(paint: &[PaintEntry]) -> (Vec<[f32; 3]>, [f32; 3]) {
        let clean = |rgb: [f32; 3]| rgb.map(|c| if c.is_finite() { c.clamp(0.0, 1.0) } else { 0.0 });
        let mut palette: Vec<[f32; 3]> = paint.iter().take(MAX_PALETTE).map(|p| clean(p.rgb)).collect();
        let hull = paint.iter().take(MAX_PALETTE).find(|p| p.usage == content::blueprint::PaintUse::Hull).map(|p| clean(p.rgb));
        if palette.is_empty() { palette.push(FALLBACK_PAINT); }
        let hull = hull.unwrap_or(palette[0]);
        (palette, hull)
    }

    /// From a blueprint and, when it has been built, its design (sealed air, compartments, waterline).
    /// `dock` drops the compartment state: the dry dock draws every interior face (L1940, cutaway views).
    pub fn from_blueprint(blueprint: &Blueprint, design: Option<&ShipDesign>, catalogue: &Catalogue, hash: ContentHash, dock: bool) -> Self {
        let (palette, hull_paint) = Self::palette_of(&blueprint.paint);
        let parts = blueprint.parts.iter().enumerate().take(u16::MAX as usize).map(|(i, p)| (PartSlot(i as u16), p.clone())).collect();
        let (grid, sealed_air, compartments, waterline_m, deck_m) = match design {
            Some(d) => {
                let prefab = &d.prefab;
                let grid = prefab.grid.design.clone();
                let n = grid.cells.len();
                let waterline = Some(prefab.waterline_y as f32).filter(|w| w.is_finite());
                let deck = Some((prefab.compartments.top_deck_y as f32 + 1.0) * hull_mesh::CELL_M_F32).filter(|_| !prefab.compartments.compartments.is_empty());
                // Slices that do not match the grid are left out rather than handed to the mesher, which
                // would refuse the whole vessel.
                if dock || prefab.grid.sealed_air.len() != n.div_ceil(64) {
                    (grid, Vec::new(), Vec::new(), waterline, deck)
                } else {
                    let comps = prefab.compartments.compartments.iter().map(|c| (c.id.0, c.cells.iter().map(|i| i.0).filter(|i| (*i as usize) < n).collect())).collect();
                    (grid, prefab.grid.sealed_air.clone(), comps, waterline, deck)
                }
            }
            None => (Arc::new(content::blueprint::io::build_grid(blueprint, catalogue)), Vec::new(), Vec::new(), None, None),
        };
        let mesh_cells = mesh_cells(&grid, &sealed_air);
        Self { hash, grid, mesh_cells, sealed_air, compartments, palette, hull_paint, markings: blueprint.markings.clone(), parts, waterline_m, deck_m }
    }

    pub fn from_exterior(ext: &ExteriorBlueprint, hash: ContentHash) -> Self {
        let (palette, hull_paint) = Self::palette_of(&ext.paint);
        // The exterior form states its size; rows expand into their own bounding box, which is smaller when
        // the hull does not touch the far faces of the box. Cell indices of the damage stream use the stated
        // size, so the grid must too.
        let rows = CellGrid::from_rows(&ext.cells);
        let grid = if rows.size == ext.size || !size_ok(ext.size) { rows } else {
            let mut g = CellGrid::new(ext.size);
            for y in 0..rows.size[1].min(ext.size[1]) {
                for z in 0..rows.size[2].min(ext.size[2]) {
                    for x in 0..rows.size[0].min(ext.size[0]) {
                        let p = CellPos::new(x as i16, y as i16, z as i16);
                        g.set(p, rows.get(p));
                    }
                }
            }
            g
        };
        let mut parts = ext.parts.clone();
        parts.sort_by_key(|(slot, _)| *slot);
        parts.dedup_by_key(|(slot, _)| *slot);
        let mesh_cells = mesh_cells(&grid, &[]);
        Self { hash, grid: Arc::new(grid), mesh_cells, sealed_air: Vec::new(), compartments: Vec::new(), palette, hull_paint, markings: ext.markings.clone(), parts, waterline_m: None, deck_m: None }
    }

    pub fn from_received(received: &ReceivedDesign, hash: ContentHash) -> Self {
        match received {
            ReceivedDesign::Full { design, blueprint } => Self::from_blueprint(blueprint, Some(design), content::catalogue::catalogue(), hash, false),
            ReceivedDesign::Exterior(ext) => Self::from_exterior(ext, hash),
        }
    }

    /// Whether the mesher will take this grid.
    pub fn meshable(&self) -> bool { size_ok(self.grid.size) && self.grid.cells.len() == self.grid.size.iter().map(|&s| s as usize).product::<usize>() && !self.grid.cells.is_empty() }

    /// Grid extent in metres.
    pub fn size_m(&self) -> [f32; 3] { self.grid.size.map(|s| s as f32 * hull_mesh::CELL_M_F32) }
}

/// `grid.cells` with the part cells that stand in the open emptied (module docs). `sealed_air` is the intact
/// design's sealed-air bits, or empty when the design carries none.
pub(crate) fn mesh_cells(grid: &CellGrid, sealed_air: &[u64]) -> Vec<Cell> {
    let mut cells = grid.cells.clone();
    let [sx, sy, sz] = grid.size.map(|s| s as usize);
    if cells.len() != sx * sy * sz || cells.is_empty() { return cells; }
    let sealed = |i: usize| sealed_air.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 == 1);
    let steps: [(isize, usize, usize); 3] = [(1, 0, sx), (sx as isize * sz as isize, 1, sy), (sx as isize, 2, sz)];
    // Face neighbours of cell `i` at `(x, y, z)`: `None` outside the grid.
    let neighbours = |i: usize| {
        let at = [i % sx, i / (sx * sz), (i / sx) % sz];
        steps.into_iter().flat_map(move |(step, axis, len)| {
            let lo = if at[axis] > 0 { Some((i as isize - step) as usize) } else { None };
            let hi = if at[axis] + 1 < len { Some((i as isize + step) as usize) } else { None };
            [lo, hi]
        })
    };
    let mut open: Vec<usize> = Vec::new();
    for i in 0..cells.len() {
        if grid.cells[i].material != Material::PartBody { continue; }
        if neighbours(i).any(|n| n.is_none_or(|n| grid.cells[n].is_empty() && !sealed(n))) {
            cells[i] = Cell::EMPTY;
            open.push(i);
        }
    }
    while let Some(i) = open.pop() {
        for n in neighbours(i).flatten() {
            if cells[n].material == Material::PartBody {
                cells[n] = Cell::EMPTY;
                open.push(n);
            }
        }
    }
    cells
}

fn size_ok(size: [u16; 3]) -> bool { size.iter().all(|&s| s > 0 && s <= content::cell::MAX_GRID_DIM) }

/// Cell row holding a waterline height in vessel-local metres, inside a grid `rows` cells tall.
pub(crate) fn waterline_row(waterline_m: f32, rows: u16) -> i16 {
    if !waterline_m.is_finite() { return (rows / 3) as i16; }
    (waterline_m / hull_mesh::CELL_M_F32).floor().clamp(0.0, rows.saturating_sub(1) as f32) as i16
}

thread_local! {
    /// Converted designs of this session by hash. Main thread only, like every render unit.
    static DRAWN: RefCell<BTreeMap<ContentHash, Arc<DrawDesign>>> = const { RefCell::new(BTreeMap::new()) };
}

/// The drawable form of a design by hash, or `None` while no store holds it. Converting an exterior form
/// expands its cell rows on the calling thread (about a millisecond for the largest hull), once per hash.
pub(crate) fn lookup(hash: &ContentHash) -> Option<Arc<DrawDesign>> {
    if let Some(found) = DRAWN.with(|d| d.borrow().get(hash).cloned()) { return Some(found); }
    let services = crate::services::services();
    let built = match services.designs.borrow().get(hash) {
        Some(received) => Some(DrawDesign::from_received(received, *hash)),
        None => services.design.borrow().as_ref().filter(|c| c.hash == *hash).map(|c| DrawDesign::from_blueprint(&c.blueprint, c.design.as_deref(), content::catalogue::catalogue(), *hash, false)),
    }?;
    let built = Arc::new(built);
    DRAWN.with(|d| d.borrow_mut().insert(*hash, built.clone()));
    Some(built)
}

/// Drop every converted design (a new sortie: hashes of the last one are unlikely to return, and a design
/// re-saved under the same hash is the same design).
pub(crate) fn forget_all() { DRAWN.with(|d| d.borrow_mut().clear()); }

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use content::blueprint::PaintUse;
    use content::cell::CellRows;

    pub(crate) const STEEL: Cell = Cell { material: Material::Steel, armour: content::cell::ArmourTier::None, paint: 0, shape: content::cell::CellShape::CUBE };

    /// A solid box hull of `size` cells with a two-entry palette, for the units' tests.
    pub(crate) fn box_design(size: [u16; 3]) -> DrawDesign {
        let mut grid = CellGrid::new(size);
        grid.cells.iter_mut().for_each(|c| *c = Cell { paint: 1, ..STEEL });
        let mesh_cells = grid.cells.clone();
        DrawDesign { hash: ContentHash([7; 32]), grid: Arc::new(grid), mesh_cells, sealed_air: Vec::new(), compartments: Vec::new(), palette: vec![[0.5, 0.5, 0.5], [0.2, 0.25, 0.3]], hull_paint: [0.5, 0.5, 0.5], markings: Vec::new(), parts: Vec::new(), waterline_m: Some(1.25), deck_m: None }
    }

    #[test]
    fn palette_is_clamped_and_never_empty() {
        let entry = |rgb, usage| PaintEntry { token: None, rgb, usage };
        let many: Vec<PaintEntry> = (0..20).map(|i| entry([i as f32 * 0.01, f32::NAN, 2.0], if i == 3 { PaintUse::Hull } else { PaintUse::Marking })).collect();
        let (palette, hull) = DrawDesign::palette_of(&many);
        assert_eq!(palette.len(), MAX_PALETTE);
        assert_eq!(palette[2], [0.02, 0.0, 1.0]);
        assert_eq!(hull, [0.03, 0.0, 1.0]);
        let (palette, hull) = DrawDesign::palette_of(&[]);
        assert_eq!((palette.len(), hull), (1, FALLBACK_PAINT));
    }

    #[test]
    fn exterior_form_keeps_its_stated_size() {
        // One cell at (1, 0, 0) in a 4 x 2 x 3 box: the rows alone would expand to 2 x 1 x 1.
        let mut g = CellGrid::new([4, 2, 3]);
        g.set(CellPos::new(1, 0, 0), STEEL);
        let ext = ExteriorBlueprint { source_hash: ContentHash::EMPTY, name: String::new(), size: [4, 2, 3], cells: g.to_rows(), paint: Vec::new(), markings: Vec::new(), parts: vec![(PartSlot(5), sample_placement()), (PartSlot(2), sample_placement())] };
        let d = DrawDesign::from_exterior(&ext, ContentHash([1; 32]));
        assert_eq!(d.grid.size, [4, 2, 3]);
        assert_eq!(d.grid.cells, g.cells);
        assert!(d.meshable());
        assert_eq!(d.parts.iter().map(|p| p.0 .0).collect::<Vec<_>>(), vec![2, 5]);
        assert_eq!(d.size_m(), [2.0, 1.0, 1.5]);
        // An empty form is not meshable and does not panic.
        let empty = ExteriorBlueprint { size: [0, 0, 0], cells: CellRows::default(), parts: Vec::new(), ..ext };
        assert!(!DrawDesign::from_exterior(&empty, ContentHash::EMPTY).meshable());
    }

    pub(crate) fn sample_placement() -> PartPlacement {
        PartPlacement { part: content::ids::PartId::new("none"), pos: CellPos::new(0, 0, 0), rot: Default::default(), kit: None, label: None, load: None }
    }

    #[test]
    fn part_cells_in_the_open_leave_the_mesh() {
        // A 5 x 4 x 3 block of steel with a hollow at (2, 1, 1). Parts: a mount standing on the deck at
        // (1, 3, 1) over its hoist at (1, 2, 1) (inside the steel), and an engine at (2, 1, 1) in the hollow.
        let steel = STEEL;
        let body = Cell { material: Material::PartBody, ..STEEL };
        let mut g = CellGrid::new([5, 4, 3]);
        for y in 0..3 { for z in 0..3 { for x in 0..5 { g.set(CellPos::new(x, y, z), steel); } } }
        g.set(CellPos::new(1, 3, 1), body);
        g.set(CellPos::new(1, 2, 1), body);
        g.set(CellPos::new(3, 1, 1), Cell::EMPTY);
        g.set(CellPos::new(2, 1, 1), body);
        let at = |cells: &[Cell], x, y, z| cells[g.index(CellPos::new(x, y, z)).unwrap().index()];
        // Live vessel: the hollow is sealed air, so the engine stays solid; the mount and the hoist under it go.
        let hollow = g.index(CellPos::new(3, 1, 1)).unwrap().index();
        let mut sealed = vec![0u64; g.cells.len().div_ceil(64)];
        sealed[hollow / 64] |= 1 << (hollow % 64);
        let live = mesh_cells(&g, &sealed);
        assert!(at(&live, 1, 3, 1).is_empty() && at(&live, 1, 2, 1).is_empty());
        assert_eq!(at(&live, 2, 1, 1), body);
        assert_eq!(at(&live, 0, 0, 0), steel);
        // Dry dock: no sealed state, so the engine beside the hollow shows its model too.
        let dock = mesh_cells(&g, &[]);
        assert!(at(&dock, 2, 1, 1).is_empty() && at(&dock, 1, 3, 1).is_empty());
        assert_eq!(dock.iter().filter(|c| c.material == Material::PartBody).count(), 0);
        // A malformed grid comes back unchanged.
        let bad = CellGrid { size: [9, 9, 9], cells: vec![body; 4] };
        assert_eq!(mesh_cells(&bad, &[]), bad.cells);
    }

    #[test]
    fn waterline_rows() {
        assert_eq!(waterline_row(1.6, 20), 3);
        assert_eq!(waterline_row(-4.0, 20), 0);
        assert_eq!(waterline_row(99.0, 20), 19);
        assert_eq!(waterline_row(f32::NAN, 21), 7);
        assert_eq!(waterline_row(1.0, 0), 0);
    }
}
