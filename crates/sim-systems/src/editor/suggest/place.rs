//! Finding room for a part or kit a Fix or Suggest proposes. The placement rules follow
//! `content::part::Placement`: inside the hull envelope, on the weather deck with open sky above, touching the
//! bottom below the waterline, or at the deck edge. The search walks outward from a target cell in a fixed
//! order, so the same design always gets the same proposal.
use crate::editor::notes::edit::footprint;
use crate::prelude::*;
use content::blueprint::Blueprint;
use content::cell::{CellGrid, CellPos, MAX_GRID_DIM};
use content::kit::{KitDef, PartRot};
use content::part::{PartDef, Placement};

/// Largest search radius in cells (24 m).
pub(crate) const MAX_RADIUS: i32 = 48;
/// Most candidate spots [`Space::low_spots`] returns.
pub(crate) const MAX_SPOTS: usize = 8_192;

/// The design's cells with parts stamped, plus the hull envelope.
pub(crate) struct Space {
    pub grid: CellGrid,
    /// Per (x, y) row: lowest and highest solid z; per (x, z) column: lowest and highest solid y.
    row_z: Vec<(i32, i32)>,
    col_y: Vec<(i32, i32)>,
    pub waterline_y: i32,
}

impl Space {
    pub fn new(bp: &Blueprint, catalogue: &Catalogue) -> Space {
        let grid = content::blueprint::io::build_grid(bp, catalogue);
        let [sx, sy, sz] = [grid.size[0] as i32, grid.size[1] as i32, grid.size[2] as i32];
        let mut row_z = vec![(i32::MAX, i32::MIN); (sx * sy).max(0) as usize];
        let mut col_y = vec![(i32::MAX, i32::MIN); (sx * sz).max(0) as usize];
        for y in 0..sy { for z in 0..sz { for x in 0..sx {
            if grid.get(CellPos::new(x as i16, y as i16, z as i16)).is_empty() { continue; }
            let r = &mut row_z[(x + sx * y) as usize];
            r.0 = r.0.min(z); r.1 = r.1.max(z);
            let c = &mut col_y[(x + sx * z) as usize];
            c.0 = c.0.min(y); c.1 = c.1.max(y);
        }}}
        Space { grid, row_z, col_y, waterline_y: bp.hull.design_waterline_y as i32 }
    }

    fn size(&self) -> [i32; 3] { [self.grid.size[0] as i32, self.grid.size[1] as i32, self.grid.size[2] as i32] }

    pub fn empty(&self, p: CellPos) -> bool { self.grid.get(p).is_empty() }

    /// An empty cell enclosed by the hull: solid to port, to starboard and below.
    pub fn inside(&self, p: CellPos) -> bool {
        let [sx, sy, sz] = self.size();
        let (x, y, z) = (p.x as i32, p.y as i32, p.z as i32);
        if x < 0 || y < 0 || z < 0 || x >= sx || y >= sy || z >= sz || !self.empty(p) { return false; }
        let r = self.row_z[(x + sx * y) as usize];
        let c = self.col_y[(x + sx * z) as usize];
        r.0 < z && z < r.1 && c.0 < y
    }

    /// No solid cell above `p` in its column (outside `ignore`).
    fn open_sky(&self, p: CellPos) -> bool {
        let [sx, _, sz] = self.size();
        let (x, z) = (p.x as i32, p.z as i32);
        if x < 0 || z < 0 || x >= sx || z >= sz { return true; }
        self.col_y[(x + sx * z) as usize].1 < p.y as i32
    }

    fn solid(&self, p: CellPos) -> bool { !self.grid.get(p).is_empty() }

    /// Whether a part's cells satisfy its placement rule here.
    pub fn fits(&self, def: &PartDef, cells: &[CellPos]) -> bool { self.fits_in(def, cells, &BTreeSet::new()) }

    /// Same, with `support` the cells of the other parts placed together with it (a kit): they hold the part
    /// up like structure but never count as structure above it.
    pub fn fits_in(&self, def: &PartDef, cells: &[CellPos], support: &BTreeSet<CellPos>) -> bool {
        if cells.is_empty() || !cells.iter().all(|&c| self.empty(c)) { return false; }
        let mine: BTreeSet<CellPos> = cells.iter().copied().collect();
        let n = |c: CellPos, d: (i16, i16, i16)| CellPos::new(c.x + d.0, c.y + d.1, c.z + d.2);
        let solid = |p: CellPos| !mine.contains(&p) && (self.solid(p) || support.contains(&p));
        let on_deck = || cells.iter().any(|&c| c.y > 0 && solid(n(c, (0, -1, 0))));
        let weather = || cells.iter().all(|&c| !self.inside(c) && self.open_sky(c)) && on_deck();
        match def.placement {
            Placement::Anywhere => true,
            Placement::Internal => cells.iter().all(|&c| self.inside(c)),
            Placement::Weather => weather(),
            Placement::Underwater => {
                cells.iter().all(|&c| !self.inside(c) && (c.y as i32) <= self.waterline_y)
                    && cells.iter().any(|&c| [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)].iter().any(|&d| solid(n(c, d))))
            }
            Placement::DeckEdge => {
                let [_, _, sz] = self.size();
                weather() && cells.iter().any(|&c| {
                    [-1i16, 1].iter().any(|&dz| {
                        let q = n(c, (0, 0, dz));
                        q.z < 0 || q.z as i32 >= sz || (!mine.contains(&q) && self.empty(q) && !self.inside(q) && (0..c.y).all(|y| self.empty(CellPos::new(q.x, y, q.z))))
                    })
                })
            }
        }
    }

    /// Whether every part of a group (a kit's parts at their cells) fits, each supported by the others.
    pub fn fits_group(&self, parts: &[(&PartDef, Vec<CellPos>)]) -> bool {
        let all: BTreeSet<CellPos> = parts.iter().flat_map(|(_, c)| c.iter().copied()).collect();
        if all.len() != parts.iter().map(|(_, c)| c.len()).sum::<usize>() { return false; }
        parts.iter().all(|(d, c)| self.fits_in(d, c, &all))
    }

    /// Candidate minimum corners in search order: Chebyshev shells around `target`, lowest y first, then
    /// x, then z.
    fn candidates(&self, target: CellPos, max_r: i32, visit: &mut dyn FnMut(CellPos) -> bool) {
        let [sx, sy, sz] = self.size();
        let lim = MAX_GRID_DIM as i32;
        for r in 0..=max_r.max(0) {
            for dy in -r..=r { for dx in -r..=r { for dz in -r..=r {
                if dx.abs().max(dy.abs()).max(dz.abs()) != r { continue; }
                let (x, y, z) = (target.x as i32 + dx, target.y as i32 + dy, target.z as i32 + dz);
                if x < 0 || y < 0 || z < 0 || x >= lim || y >= lim || z >= lim || x > sx + 4 || y > sy + 8 || z > sz + 4 { continue; }
                if !visit(CellPos::new(x as i16, y as i16, z as i16)) { return; }
            }}}
        }
    }

    /// Nearest minimum corner where one part fits.
    pub fn find_part(&self, def: &PartDef, rot: PartRot, target: CellPos, max_r: i32) -> Option<CellPos> {
        let mut found = None;
        self.candidates(target, max_r, &mut |pos| {
            if let Some(cells) = footprint(def, pos, rot) { if self.fits(def, &cells) { found = Some(pos); return false; } }
            true
        });
        found
    }

    /// Nearest kit position where every part of the kit fits its own placement rule.
    pub fn find_kit(&self, kit: &KitDef, catalogue: &Catalogue, rot: PartRot, mirrored: bool, target: CellPos, max_r: i32) -> Option<CellPos> {
        let base = content::kit::kit_part_placements(kit, catalogue, CellPos::new(0, 0, 0), rot, mirrored);
        if base.is_empty() || base.len() != kit.parts.len() { return None; }
        let parts: Vec<(&PartDef, CellPos, PartRot)> = base.iter().filter_map(|p| catalogue.part(&p.part).map(|d| (d, p.pos, p.rot))).collect();
        if parts.len() != base.len() { return None; }
        let mut found = None;
        self.candidates(target, max_r, &mut |pos| {
            let group: Option<Vec<(&PartDef, Vec<CellPos>)>> = parts.iter().map(|(d, off, r)| footprint(d, CellPos::new(pos.x + off.x, pos.y + off.y, pos.z + off.z), *r).map(|c| (*d, c))).collect();
            if group.is_some_and(|g| self.fits_group(&g)) { found = Some(pos); return false; }
            true
        });
        found
    }

    /// Minimum corners where one part fits, low in the hull all along it: for each x station, every z in the
    /// lowest `layers` cell layers that hold a fit there, in scan order (x, then y, then z), at most
    /// [`MAX_SPOTS`]. Ballast wants height above all, but trim and heel need a choice of station and side, so
    /// this replaces the nearest-first search for it.
    pub fn low_spots(&self, def: &PartDef, rot: PartRot, layers: usize) -> Vec<CellPos> {
        let [sx, sy, sz] = self.size();
        let mut out = Vec::new();
        for x in 0..sx {
            let mut layers_left = layers.max(1);
            for y in 0..sy {
                let before = out.len();
                for z in 0..sz {
                    if out.len() >= MAX_SPOTS { return out; }
                    let pos = CellPos::new(x as i16, y as i16, z as i16);
                    if !self.empty(pos) { continue; }
                    if let Some(cells) = footprint(def, pos, rot) { if self.fits(def, &cells) { out.push(pos); } }
                }
                if out.len() > before { layers_left -= 1; if layers_left == 0 { break; } }
            }
        }
        out
    }

    /// Lowest enclosed cell of the hull near length fraction `x_frac` on the centreline (engine rooms,
    /// ballast), or the grid centre when the hull encloses nothing there.
    pub fn low_centre(&self, x_frac: f64) -> CellPos {
        let [sx, sy, sz] = self.size();
        let x = ((x_frac.clamp(0.0, 1.0) * (sx - 1).max(0) as f64) as i32).clamp(0, (sx - 1).max(0));
        let z = sz / 2;
        for y in 0..sy { let p = CellPos::new(x as i16, y as i16, z as i16); if self.inside(p) { return p; } }
        CellPos::new(x as i16, (sy / 2) as i16, z as i16)
    }

    /// Top of the weather deck on the centreline at length fraction `x_frac`.
    pub fn deck_top(&self, x_frac: f64) -> CellPos {
        let [sx, _, sz] = self.size();
        let x = ((x_frac.clamp(0.0, 1.0) * (sx - 1).max(0) as f64) as i32).clamp(0, (sx - 1).max(0));
        let z = sz / 2;
        let top = self.col_y.get((x + sx * z) as usize).map(|c| c.1).filter(|y| *y != i32::MIN).unwrap_or(0);
        CellPos::new(x as i16, (top + 1).max(0) as i16, z as i16)
    }
}

/// A part's default rotation for a proposal.
pub(crate) fn upright() -> PartRot { PartRot::default() }
