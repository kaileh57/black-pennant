//! Unit `data-starter-hulls`: the generator behind `blueprints/starters/*.bp.ron` and
//! `blueprints/tutorial/*.bp.ron` (DESIGN.md L142-151, L252-275, L705, L801-822).
//!
//! Each ship is code in [`ships`]: a lofted shell, explicit decks and bulkheads, deckhouses voxelized with the
//! same corner-pattern rule as the hull, and kits and parts dropped into place against the real cell grid. The
//! [`Builder`] drives the content crate's own loft, voxelizer, kit transform and canonical writer, so a
//! committed file is exactly what the editor would save for the same design.
//!
//! Hydrostatics here are the generator's own cell-exact check, used to set the design waterline: buoyant
//! volume is every live cell (structure and parts) and every empty cell that water cannot reach from outside,
//! by the simulation's rule for what water passes (empty cells and the cells of open equipment; structure and
//! watertight parts such as tanks, magazines and doors stop it), each immersed by the fraction of it under a
//! flat waterline. GM is the
//! righting arm at 2 degrees of heel either way over sin(2 degrees), with G from
//! [`content::stats::compute_stats`]; trim comes from the waterplane row's longitudinal inertia. The Float
//! gauge and the simulation solve the same hull on 2 m buoyancy blocks (L650), so the binding stability checks
//! sail the committed files there (`crates/sim-systems/tests/starter_hulls_sim.rs`).
//!
//! An open part that replaces structure (an ammunition hoist through its deck, a vertical launcher sunk into
//! the deck, a hull sonar through the keel) would open its compartment to the sea or the sky, and the
//! simulation counts an open compartment as outside the hull. [`Builder::finish`] therefore builds a
//! watertight trunk of plating round every such part, as a shipyard does.
#![allow(dead_code)]

pub mod ships;

use content::blueprint::io::{build_grid, canonicalize_blueprint, compute_hash, default_paint_table};
use content::blueprint::{Blueprint, BlueprintKind, Header, KitPlacement, Marking, MarkingKind, PartPlacement, SocketBinding, BLUEPRINT_FORMAT, DEFAULT_LICENCE};
use content::catalogue::Catalogue;
use content::cell::{full_face_mask, shape_from_corners, ArmourTier, Cell, CellGrid, CellPos, CellShape, Face, Material, CELL_M};
use content::enums::Axis;
use content::hull::{voxelize, Hull, HullShell, HullSource, InternalPlane, ShellField, ShellLayout, Station, PAINT_DECK, PAINT_SIDES};
use content::ids::{ContentHash, KitId, KitSlot, PartId, PartSlot, TextKey};
use content::kit::{kit_part_placements, kit_sockets_at, rotated_size, PartRot};
use content::stats::{compute_stats, BlueprintStats};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Rows of headroom above the hull grid for deckhouses.
const HEADROOM: u16 = 40;
/// Paint indices of the default table's swatches beyond the ones the voxelizer uses.
pub const PAINT_PALE: u8 = 0;
pub const PAINT_LIGHT: u8 = 1;
pub const PAINT_OCEAN: u8 = 3;
pub const PAINT_SEA: u8 = 4;
pub const PAINT_NAVY: u8 = 5;
pub const PAINT_BLACK: u8 = 7;

pub const YAW_AFT: PartRot = PartRot { yaw: 2, tilt: 0 };
pub const YAW_0: PartRot = PartRot { yaw: 0, tilt: 0 };
/// Parts allowed to replace structure: a hoist through its deck, a vertical launcher sunk into the deck, a
/// waterjet through the transom, a hull sonar in its keel dome, a door in its bulkhead.
pub const EMBEDDED: [&str; 6] = ["hoist.ammunition", "launcher.sam", "waterjet", "sonar.passive", "sonar.active", "door.watertight"];

/// One cross-section as fractions of the principal dimensions: (t, beam, draft, deadrise deg, flare deg, sheer).
pub type StationRow = (f64, f64, f64, f64, f64, f64);

/// A lofted shell from principal dimensions and a station table of fractions (a beam fraction of 0 is the
/// 0.6 m stem bar).
pub fn shell(length_m: f64, beam_m: f64, draft_m: f64, freeboard_m: f64, rows: &[StationRow], bow_rake_deg: f64, transom_rake_deg: f64, transom_width: f64) -> HullShell {
    let stations = rows
        .iter()
        .map(|&(t, b, d, dr, fl, sh)| Station { t, beam_m: if b <= 0.0 { 0.6 } else { b * beam_m }, draft_m: d * draft_m, deadrise_deg: dr, flare_deg: fl, sheer_m: sh * freeboard_m })
        .collect();
    HullShell { length_m, stations, bow_rake_deg, transom_rake_deg, transom_width, mirrored: true, shell_cells: 1 }
}

/// A deckhouse block in vessel-local metres: a prism standing on the deck whose faces lean in with height.
#[derive(Clone, Debug)]
pub struct House {
    /// Aft and forward ends at the base.
    pub x0: f64,
    pub x1: f64,
    /// Half-width at the base, and the centre's offset from the centreline (starboard positive).
    pub hw: f64,
    pub zc: f64,
    /// Height of the base (vessel-local y, metres) and of the roof above it.
    pub base: f64,
    pub height: f64,
    /// Inward lean of the front, aft and side faces in metres per metre of height.
    pub front: f64,
    pub aft: f64,
    pub side: f64,
    pub mat: Material,
    pub paint: u8,
    /// Window band in metres above the base: front wall cells in it are glazing, and side wall cells within
    /// 2 m of the front (the bridge wings).
    pub glass: Option<(f64, f64)>,
}
impl House {
    pub fn new(x0: f64, x1: f64, hw: f64, base: f64, height: f64) -> House {
        House { x0, x1, hw, zc: 0.0, base, height, front: 0.0, aft: 0.0, side: 0.0, mat: Material::Steel, paint: PAINT_SIDES, glass: None }
    }
    pub fn lean(mut self, front: f64, aft: f64, side: f64) -> House {
        self.front = front;
        self.aft = aft;
        self.side = side;
        self
    }
    pub fn mat(mut self, m: Material) -> House {
        self.mat = m;
        self
    }
    pub fn paint(mut self, p: u8) -> House {
        self.paint = p;
        self
    }
    pub fn glass(mut self, from: f64, to: f64) -> House {
        self.glass = Some((from, to));
        self
    }
    pub fn off(mut self, zc: f64) -> House {
        self.zc = zc;
        self
    }
    pub fn top(&self) -> f64 { self.base + self.height }
    /// Inside the prism (extended 3 m below its base, so it always reaches a sloping deck).
    fn inside(&self, p: [f64; 3], lay: &ShellLayout) -> bool {
        let h = p[1] - self.base;
        if p[1] > self.top() + 1e-9 || h < -3.0 {
            return false;
        }
        let hp = h.max(0.0);
        let zc = lay.half_width_cells as f64 * CELL_M + self.zc;
        p[0] >= self.x0 + self.aft * hp - 1e-9 && p[0] <= self.x1 - self.front * hp + 1e-9 && (p[2] - zc).abs() <= self.hw - self.side * hp + 1e-9
    }
}

/// Where a piece goes; `x` and `z` are the minimum corner of its footprint.
#[derive(Clone, Debug)]
pub enum At {
    Exact(CellPos),
    /// Start with the top of the piece just under row `under`, move down to the first free position, then keep
    /// lowering until it rests on structure or another part.
    Drop { x: i16, z: i16, under: i16 },
    /// Start at the bottom of the frame and rise until the piece touches structure (props and rudders).
    Lift { x: i16, z: i16 },
    /// Put the named socket's cell against `target` across its face.
    Socket { name: &'static str, target: CellPos },
}

/// A piece at the origin: its parts (id, relative pos, rot, cells) and sockets.
struct Piece {
    parts: Vec<(PartId, CellPos, PartRot, Vec<[i32; 3]>)>,
    sockets: Vec<(String, [i32; 3], Face)>,
}
impl Piece {
    fn cells(&self) -> impl Iterator<Item = [i32; 3]> + '_ { self.parts.iter().flat_map(|p| p.3.iter().copied()) }
    fn height(&self) -> i32 { self.cells().map(|c| c[1]).max().unwrap_or(0) + 1 }
}

fn add(a: [i32; 3], b: [i32; 3]) -> [i32; 3] { [a[0] + b[0], a[1] + b[1], a[2] + b[2]] }
fn arr(p: CellPos) -> [i32; 3] { [p.x as i32, p.y as i32, p.z as i32] }
fn cp(a: [i32; 3]) -> CellPos { CellPos::new(a[0] as i16, a[1] as i16, a[2] as i16) }
fn face_dir(f: Face) -> [i32; 3] {
    let (x, y, z) = f.offset();
    [x as i32, y as i32, z as i32]
}

pub struct Builder<'c> {
    pub cat: &'c Catalogue,
    pub shell: HullShell,
    pub field: ShellField,
    pub lay: ShellLayout,
    pub planes: Vec<InternalPlane>,
    /// The voxelized hull at the current design waterline, and the same without decks and bulkheads.
    hull: CellGrid,
    bare: CellGrid,
    /// Deckhouse cells.
    extra: BTreeMap<CellPos, Cell>,
    /// Material and side colour the hull plating is built in, and the material of the plating below the design
    /// waterline when that differs (a metal bottom under composite topsides).
    pub hull_mat: Material,
    pub bottom_mat: Option<Material>,
    pub side_paint: u8,
    /// Armour belts on the side plating: x range and rows relative to the design waterline, tier.
    belts: Vec<(i16, i16, i16, i16, ArmourTier)>,
    pub waterline: i16,
    pub parts: Vec<PartPlacement>,
    pub kits: Vec<KitPlacement>,
    owner: BTreeMap<[i32; 3], usize>,
    pub markings: Vec<Marking>,
    /// Combined structure grid, rebuilt on change.
    grid: CellGrid,
}

impl<'c> Builder<'c> {
    /// A hull from a shell, decks at the given heights above the design waterline (metres) and transverse
    /// bulkheads at the given vessel-local x (metres).
    pub fn new(cat: &'c Catalogue, shell: HullShell, decks_m: &[f64], bulkheads_m: &[f64]) -> Builder<'c> {
        content::hull::validate_shell(&shell, 90.0).unwrap_or_else(|e| panic!("shell: {e}"));
        let field = ShellField::new(&shell).expect("valid shell");
        let lay = field.layout();
        let mut planes = Vec::new();
        for &d in decks_m {
            planes.push(InternalPlane { axis: Axis::Y, offset: lay.waterline_y + (d / CELL_M).round() as i16, watertight: true });
        }
        for &b in bulkheads_m {
            planes.push(InternalPlane { axis: Axis::X, offset: (b / CELL_M).floor() as i16, watertight: true });
        }
        let mut b = Builder {
            cat,
            shell,
            field,
            lay,
            planes,
            hull: CellGrid::default(),
            bare: CellGrid::default(),
            extra: BTreeMap::new(),
            hull_mat: Material::Steel,
            bottom_mat: None,
            side_paint: PAINT_SIDES,
            belts: Vec::new(),
            waterline: lay.waterline_y,
            parts: Vec::new(),
            kits: Vec::new(),
            owner: BTreeMap::new(),
            markings: Vec::new(),
            grid: CellGrid::default(),
        };
        b.revoxelize();
        b
    }

    fn revoxelize(&mut self) {
        let hull = Hull { source: HullSource::Lofted(self.shell.clone()), planes: self.planes.clone(), design_waterline_y: self.waterline };
        let mut v = voxelize(&hull).grid;
        let sxz = v.size[0] as usize * v.size[2] as usize;
        for (i, c) in v.cells.iter_mut().enumerate().filter(|(_, c)| !c.is_empty()) {
            let below = ((i / sxz) as i16) < self.waterline;
            c.material = match self.bottom_mat {
                Some(m) if below => m,
                _ => self.hull_mat,
            };
            if c.paint == PAINT_SIDES {
                c.paint = self.side_paint;
            }
        }
        // Belts: side plating (a cell open to the outside across a z face) inside each belt's box.
        let [sx, sy, sz] = v.size.map(|n| n as i16);
        let open = |g: &CellGrid, x: i16, y: i16, z: i16| z < 0 || z >= sz || g.get(CellPos::new(x, y, z)).is_empty() && self.field.sdf(CellPos::new(x, y, z).centre_m()) > 0.0;
        let mut armoured = Vec::new();
        for &(x0, x1, r0, r1, tier) in &self.belts {
            for y in (self.waterline + r0).max(0)..=(self.waterline + r1).min(sy - 1) {
                for x in x0.max(0)..=x1.min(sx - 1) {
                    for z in 0..sz {
                        let c = CellPos::new(x, y, z);
                        if !v.get(c).is_empty() && (open(&v, x, y, z - 1) || open(&v, x, y, z + 1)) {
                            armoured.push((c, tier));
                        }
                    }
                }
            }
        }
        for (c, tier) in armoured {
            let mut cell = v.get(c);
            cell.armour = tier;
            v.set(c, cell);
        }
        self.hull = v;
        self.bare = voxelize(&Hull { planes: Vec::new(), ..hull }).grid;
        self.compose();
    }

    /// Paint an armour belt on the side plating from vessel-local `x0_m` to `x1_m`, rows `below` under to
    /// `above` over the design waterline (it follows the waterline when that moves).
    pub fn belt(&mut self, x0_m: f64, x1_m: f64, below: i16, above: i16, tier: ArmourTier) {
        self.belts.push((self.x(x0_m), self.x(x1_m), -below, above, tier));
        self.revoxelize();
    }

    /// Re-dress the plating: material and side colour (decks, boot-topping and antifouling keep theirs).
    pub fn dress(&mut self, mat: Material, side_paint: u8) {
        self.hull_mat = mat;
        self.side_paint = side_paint;
        self.revoxelize();
    }

    /// Build the plating below the design waterline in `mat`, whatever the topsides are made of.
    pub fn bottom(&mut self, mat: Material) {
        self.bottom_mat = Some(mat);
        self.revoxelize();
    }

    fn compose(&mut self) {
        let s = self.hull.size;
        let mut g = CellGrid::new([s[0], s[1] + HEADROOM, s[2]]);
        for (i, c) in self.hull.cells.iter().enumerate() {
            if !c.is_empty() {
                g.set(self.hull.pos(content::ids::CellIndex(i as u32)), *c);
            }
        }
        for (p, c) in &self.extra {
            g.set(*p, *c);
        }
        self.grid = g;
    }

    pub fn size(&self) -> [i16; 3] { self.grid.size.map(|v| v as i16) }
    pub fn half(&self) -> i16 { self.lay.half_width_cells as i16 }
    /// Cell column of a vessel-local x in metres.
    pub fn x(&self, m: f64) -> i16 { (m / CELL_M).floor() as i16 }
    /// Minimum z of a piece `w` cells wide centred on the centreline.
    pub fn zc(&self, w: i16) -> i16 { self.half() - w / 2 }
    /// Minimum z of a piece whose inboard face is `off` cells from the centreline, to starboard.
    pub fn zs(&self, off: i16) -> i16 { self.half() + off }
    /// Minimum z of a piece `w` wide whose inboard face is `off` cells from the centreline, to port.
    pub fn zp(&self, off: i16, w: i16) -> i16 { self.half() - off - w }

    pub fn solid(&self, p: [i32; 3]) -> bool { p.iter().all(|&v| v >= 0) && !self.grid.get(cp(p)).is_empty() }
    /// Highest solid cell of the hull in a column.
    pub fn hull_top(&self, x: i16, z: i16) -> Option<i16> { (0..self.hull.size[1] as i16).rev().find(|&y| !self.hull.get(CellPos::new(x, y, z)).is_empty()) }
    /// Highest solid structure cell (hull or deckhouse) in a column.
    pub fn top(&self, x: i16, z: i16) -> Option<i16> { (0..self.grid.size[1] as i16).rev().find(|&y| !self.grid.get(CellPos::new(x, y, z)).is_empty()) }
    /// Weather deck row on the centreline at column `x`.
    pub fn deck(&self, x: i16) -> i16 { self.hull_top(x, self.half()).unwrap_or_else(|| panic!("no hull at x {x}")) }
    /// Vessel-local height in metres of the top of the weather deck at vessel-local x.
    pub fn deck_m(&self, x_m: f64) -> f64 { (self.deck(self.x(x_m)) + 1) as f64 * CELL_M }
    /// Top of the highest structure over a column, as a row.
    pub fn roof(&self, x: i16, z: i16) -> i16 { self.top(x, z).unwrap_or_else(|| panic!("no structure at x {x} z {z}")) }
    /// Outermost plating cell to starboard at (x, y).
    pub fn side_z(&self, x: i16, y: i16) -> Option<i16> { (0..self.hull.size[2] as i16).rev().find(|&z| !self.hull.get(CellPos::new(x, y, z)).is_empty()) }
    /// Sky row: start every topside drop from here.
    pub fn sky(&self) -> i16 { self.size()[1] }

    // ---------------------------------------------------------------------------------------------------
    // Pieces.

    fn part_piece(&self, id: &str, rot: PartRot) -> Piece {
        let def = self.cat.part(&PartId::new(id)).unwrap_or_else(|| panic!("unknown part {id}"));
        let s = rotated_size(def.size, rot);
        let mut cells = Vec::new();
        for x in 0..s[0] as i32 {
            for y in 0..s[1] as i32 {
                for z in 0..s[2] as i32 {
                    cells.push([x, y, z]);
                }
            }
        }
        Piece { parts: vec![(PartId::new(id), CellPos::new(0, 0, 0), rot, cells)], sockets: Vec::new() }
    }

    fn kit_piece(&self, id: &str, rot: PartRot, mirrored: bool) -> Piece {
        let kit = self.cat.kit(&KitId::new(id)).unwrap_or_else(|| panic!("unknown kit {id}"));
        let o = CellPos::new(0, 0, 0);
        let parts = kit_part_placements(kit, self.cat, o, rot, mirrored).into_iter().map(|p| (p.part, p.pos, p.rot, p.cells.into_iter().map(arr).collect())).collect();
        let sockets = kit_sockets_at(kit, self.cat, o, rot, mirrored).into_iter().map(|s| (s.name, arr(s.cell), s.face)).collect();
        Piece { parts, sockets }
    }

    /// Can the piece sit at `pos`: inside the frame, clear of other parts, and clear of structure except for
    /// the parts allowed to replace it.
    fn fits(&self, piece: &Piece, pos: [i32; 3]) -> bool {
        piece.parts.iter().all(|(id, _, _, cells)| {
            let embed = EMBEDDED.contains(&id.as_str());
            cells.iter().all(|&c| {
                let p = add(c, pos);
                p.iter().all(|&v| (0..384).contains(&v)) && !self.owner.contains_key(&p) && (embed || !self.solid(p))
            })
        })
    }

    /// Has the piece at `pos` reached the hull's bottom plating: is any cell at or below the lowest plating
    /// cell of its column?
    fn cuts_plating(&self, piece: &Piece, pos: [i32; 3]) -> bool {
        piece.cells().any(|c| {
            let p = add(c, pos);
            let (x, z) = (p[0] as i16, p[2] as i16);
            (0..self.bare.size[1] as i16).find(|&y| !self.bare.get(CellPos::new(x, y, z)).is_empty()).is_some_and(|b| p[1] <= b as i32)
        })
    }

    /// What blocks a piece at a position, for error messages.
    fn why(&self, piece: &Piece, pos: [i32; 3]) -> String {
        let mut out = Vec::new();
        for (id, _, _, cells) in &piece.parts {
            for &c in cells {
                let p = add(c, pos);
                if let Some(s) = self.owner.get(&p) {
                    out.push(format!("{id} cell {p:?} hits part {}", self.parts[*s].part));
                } else if self.solid(p) && !EMBEDDED.contains(&id.as_str()) {
                    out.push(format!("{id} cell {p:?} hits structure"));
                }
            }
        }
        out.truncate(4);
        out.join("; ")
    }

    fn resolve(&self, piece: &Piece, at: &At, what: &str) -> [i32; 3] { self.try_resolve(piece, at, what).unwrap_or_else(|e| panic!("{e}")) }

    fn try_resolve(&self, piece: &Piece, at: &At, what: &str) -> Result<[i32; 3], String> {
        let pos = match *at {
            At::Exact(p) => arr(p),
            At::Drop { x, z, under } => {
                // Search down for the first free position, through decks and other parts but never through the
                // hull's own plating (a piece that does not fit inside must not land under the keel).
                let mut y = under as i32 - piece.height();
                while y >= 0 && !self.fits(piece, [x as i32, y, z as i32]) {
                    if self.cuts_plating(piece, [x as i32, y, z as i32]) {
                        y = -1;
                        break;
                    }
                    y -= 1;
                }
                if y < 0 {
                    return Err(format!("{what}: no free space under row {under} at x {x} z {z}: {}", self.why(piece, [x as i32, under as i32 - piece.height(), z as i32])));
                }
                while y > 0 && self.fits(piece, [x as i32, y - 1, z as i32]) {
                    y -= 1;
                }
                [x as i32, y, z as i32]
            }
            At::Lift { x, z } => {
                if !self.fits(piece, [x as i32, 0, z as i32]) {
                    return Err(format!("{what}: blocked at the bottom of the frame at x {x} z {z}: {}", self.why(piece, [x as i32, 0, z as i32])));
                }
                let mut y = 0;
                while self.fits(piece, [x as i32, y + 1, z as i32]) {
                    y += 1;
                }
                [x as i32, y, z as i32]
            }
            At::Socket { name, target } => {
                let Some((_, cell, face)) = piece.sockets.iter().find(|s| s.0 == name) else { return Err(format!("{what}: no socket {name}")) };
                let d = face_dir(*face);
                let t = arr(target);
                [t[0] - d[0] - cell[0], t[1] - d[1] - cell[1], t[2] - d[2] - cell[2]]
            }
        };
        if !self.fits(piece, pos) {
            return Err(format!("{what}: does not fit at {pos:?}: {}", self.why(piece, pos)));
        }
        Ok(pos)
    }

    /// May an embedded part replace this structure cell? Doors only replace bulkhead and deck planes; a hoist
    /// or vertical launcher also passes through the weather deck and deckhouses; a waterjet also replaces the
    /// transom and bottom plating it exhausts through. Nothing else opens the hull.
    fn may_replace(&self, id: &str, p: [i32; 3]) -> bool {
        let c = cp(p);
        let plane = !self.hull.get(c).is_empty() && self.bare.get(c).is_empty();
        let weather = self.hull_top(c.x, c.z) == Some(c.y) || self.extra.contains_key(&c);
        match id {
            "door.watertight" => plane,
            "hoist.ammunition" | "launcher.sam" => plane || weather,
            "waterjet" | "sonar.passive" | "sonar.active" => plane || !self.hull.get(c).is_empty(),
            _ => false,
        }
    }

    fn commit(&mut self, piece: &Piece, pos: [i32; 3], kit: Option<KitSlot>) -> Vec<PartSlot> {
        let mut slots = Vec::new();
        for (id, rel, rot, cells) in &piece.parts {
            let slot = self.parts.len();
            for &c in cells {
                let p = add(c, pos);
                assert!(!self.solid(p) || self.may_replace(id.as_str(), p), "{id} at {p:?} would cut through the hull");
                self.owner.insert(p, slot);
            }
            self.parts.push(PartPlacement { part: id.clone(), pos: cp(add(arr(*rel), pos)), rot: *rot, kit, label: None, load: None });
            slots.push(PartSlot(slot as u16));
        }
        slots
    }

    /// Place a loose part; returns its slot.
    pub fn part(&mut self, id: &str, rot: PartRot, at: At) -> PartSlot {
        let piece = self.part_piece(id, rot);
        let pos = self.resolve(&piece, &at, id);
        self.commit(&piece, pos, None)[0]
    }

    /// Place a part or kit (ids starting `kit.`) dropped from the sky onto whatever is below, at vessel-local
    /// `x_m` with minimum z `z`.
    pub fn on(&mut self, id: &str, rot: PartRot, x_m: f64, z: i16) {
        let at = At::Drop { x: self.x(x_m), z, under: self.sky() };
        self.place(id, rot, at);
    }

    /// Place a kit mirrored across its own z centre (the starboard twin of a port kit), dropped from the sky.
    pub fn on_mirrored(&mut self, id: &str, rot: PartRot, x_m: f64, z: i16) {
        let at = At::Drop { x: self.x(x_m), z, under: self.sky() };
        self.kit(id, rot, true, at);
    }

    /// Place a part or kit inside the hull: dropped from under the weather deck to the floor below.
    pub fn below(&mut self, id: &str, rot: PartRot, x_m: f64, z: i16) {
        let x = self.x(x_m);
        let at = At::Drop { x, z, under: self.deck(x) };
        self.place(id, rot, at);
    }

    /// Place a part or kit in the first column from vessel-local `x0_m` forward where it fits wholly inside
    /// `x0_m..x1_m` (a compartment between bulkheads), dropped from under row `under` (`None`: the weather
    /// deck) to the floor. Returns the x it took, in metres.
    pub fn fit_in(&mut self, id: &str, rot: PartRot, x0_m: f64, x1_m: f64, z: i16, under: Option<i16>) -> f64 {
        let len = self.size_of(id, rot)[0];
        let (x0, x1) = (self.x(x0_m), self.x(x1_m));
        let piece = if id.starts_with("kit.") { self.kit_piece(id, rot, false) } else { self.part_piece(id, rot) };
        let mut why = Vec::new();
        for x in x0..=x1 - len {
            let at = At::Drop { x, z, under: under.unwrap_or_else(|| self.deck(x)) };
            match self.try_resolve(&piece, &at, id) {
                // Standing on the floor or a deck, not on another part.
                Ok(pos) if piece.cells().any(|c| self.solid(add(add(c, pos), [0, -1, 0]))) => {
                    self.place(id, rot, At::Exact(cp(pos)));
                    return x as f64 * CELL_M;
                }
                Ok(pos) => why.push(format!("{id}: at {pos:?} it would stand on another part")),
                Err(e) => why.push(e),
            }
        }
        panic!("{id}: no room between {x0_m} m and {x1_m} m at z {z}: {}", why.last().map(String::as_str).unwrap_or("too short"));
    }

    /// Place a part or kit dropped from under row `under`.
    pub fn under(&mut self, id: &str, rot: PartRot, x_m: f64, z: i16, under: i16) {
        let at = At::Drop { x: self.x(x_m), z, under };
        self.place(id, rot, at);
    }

    pub fn place(&mut self, id: &str, rot: PartRot, at: At) {
        if id.starts_with("kit.") {
            self.kit(id, rot, false, at);
        } else {
            self.part(id, rot, at);
        }
    }

    /// A propeller (or rudder) hung under the run as low as the frame allows, so it turns fully immersed: the
    /// first column from vessel-local `x_m` forward where, lifted until it meets the hull, it still sits on the
    /// keel line (row 0).
    pub fn prop(&mut self, id: &str, x_m: f64, z: i16) {
        let piece = self.part_piece(id, YAW_0);
        let mut why = Vec::new();
        for x in self.x(x_m)..self.x(x_m + 6.0) {
            match self.try_resolve(&piece, &At::Lift { x, z }, id) {
                Ok(pos) if pos[1] == 0 => {
                    self.part(id, YAW_0, At::Exact(cp(pos)));
                    return;
                }
                Ok(pos) => why.push(format!("{id}: at {pos:?} it hangs above the keel line")),
                Err(e) => {
                    why.push(e);
                    break;
                }
            }
        }
        panic!("{}", why.join("\n"));
    }

    /// A waterjet (loose part, or a kit whose jet sits at its aft end) through the transom: standing on the
    /// hull's interior floor `probe` cells forward of the stern (or, `sunk`, one row lower with its intake
    /// through the bottom plating), with its aft column replacing the transom plating so its nozzle exhausts
    /// outside, below the waterline.
    pub fn jet(&mut self, id: &str, z: i16, probe: i16, sunk: bool) {
        let zc = z + 1;
        let interior = |b: &Self, x: i16, y: i16| b.hull.get(CellPos::new(x, y, zc)).is_empty() && b.field.sdf(CellPos::new(x, y, zc).centre_m()) <= 0.0;
        let floor = (0..self.hull.size[1] as i16).find(|&y| interior(self, probe, y)).unwrap_or_else(|| panic!("{id}: no interior at x {probe} z {zc}"));
        let floor = floor - sunk as i16;
        let x = (0..self.hull.size[0] as i16).find(|&x| (0..2).any(|dy| !self.hull.get(CellPos::new(x, floor + dy, zc)).is_empty())).unwrap();
        assert!(floor < self.waterline, "{id}: the transom floor (row {floor}) is above the waterline (row {})", self.waterline);
        self.place(id, YAW_0, At::Exact(CellPos::new(x, floor, z)));
    }

    /// A hull sonar kit in a keel dome at vessel-local `x_m`, centred: its lowest row replaces the keel plating
    /// on the centreline so the transducers face the sea.
    pub fn keel(&mut self, id: &str, x_m: f64) {
        let x = self.x(x_m);
        let w = self.size_of(id, YAW_0)[2];
        let z = self.zc(w);
        let y = (0..self.hull.size[1] as i16).find(|&y| !self.hull.get(CellPos::new(x, y, self.half())).is_empty()).unwrap();
        assert!(y < self.waterline, "{id}: the keel at x {x} is above the waterline");
        self.place(id, YAW_0, At::Exact(CellPos::new(x, y, z)));
    }

    /// Place a loose part at the first of several positions where it fits.
    pub fn part_first(&mut self, id: &str, rot: PartRot, ats: &[At]) -> PartSlot {
        let piece = self.part_piece(id, rot);
        let mut why = Vec::new();
        for at in ats {
            match self.try_resolve(&piece, at, id) {
                Ok(pos) => return self.commit(&piece, pos, None)[0],
                Err(e) => why.push(e),
            }
        }
        panic!("{}", why.join("\n"));
    }

    /// Place a stock kit; returns its kit slot. Every socket binds to the first structure cell across its
    /// face, within 4 cells (a floor socket over a vee bottom may hang a cell or two above the plating its
    /// kit rests on); at least one must bind.
    pub fn kit(&mut self, id: &str, rot: PartRot, mirrored: bool, at: At) -> KitSlot {
        let piece = self.kit_piece(id, rot, mirrored);
        let pos = self.resolve(&piece, &at, id);
        let k = KitSlot(self.kits.len() as u16);
        let slots = self.commit(&piece, pos, Some(k));
        let def = self.cat.kit(&KitId::new(id)).unwrap();
        let mut sockets = Vec::new();
        for (name, cell, face) in &piece.sockets {
            let d = face_dir(*face);
            let mut target = add(*cell, pos);
            let mut bound = None;
            for _ in 0..4 {
                target = add(target, d);
                if self.solid(target) {
                    bound = Some(target);
                    break;
                }
                if self.owner.get(&target).is_some_and(|s| !slots.iter().any(|x| x.index() == *s)) {
                    break;
                }
            }
            // A socket on a part that replaces structure (a launcher flush with the deck, a jet through the
            // transom, a sonar in its keel dome) binds to the nearest structure around it.
            let at = add(*cell, pos);
            let bound = bound.or_else(|| {
                (1..=3).find_map(|r: i32| {
                    let mut near = Vec::new();
                    for dx in -r..=r {
                        for dy in -r..=r {
                            for dz in -r..=r {
                                let n = add(at, [dx, dy, dz]);
                                if dx.abs().max(dy.abs()).max(dz.abs()) == r && self.solid(n) && !self.owner.contains_key(&n) {
                                    near.push(n);
                                }
                            }
                        }
                    }
                    near.into_iter().min_by_key(|n| (n[0] - at[0]).abs() + (n[1] - at[1]).abs() + (n[2] - at[2]).abs())
                })
            });
            if let Some(t) = bound {
                sockets.push(SocketBinding { socket: name.clone(), cell: cp(t) });
            }
        }
        assert!(!sockets.is_empty(), "{id} at {pos:?}: no socket touches structure");
        let settings = def.settings.iter().map(|s| (s.key.clone(), s.default)).collect();
        self.kits.push(KitPlacement { kit: def.id.clone(), version: def.version, pos: cp(pos), rot, mirrored, sockets, settings, parts: slots });
        k
    }

    /// A watertight door in the transverse bulkhead at column `x`, on the lowest four rows of bulkhead with
    /// open space on both sides; `z` is its minimum corner.
    pub fn door(&mut self, x: i16, z: i16) -> PartSlot {
        let (xi, zi) = (x as i32, z as i32);
        let ok = |b: &Self, y: i32| (0..4).all(|dy| (0..2).all(|dz| b.solid([xi, y + dy, zi + dz]) && !b.solid([xi - 1, y + dy, zi + dz]) && !b.solid([xi + 1, y + dy, zi + dz])));
        let y = (1..self.size()[1] as i32 - 4).find(|&y| ok(self, y)).unwrap_or_else(|| panic!("no room for a door in bulkhead {x} at z {z}"));
        self.part("door.watertight", YAW_0, At::Exact(CellPos::new(x, y as i16, z)))
    }

    /// Footprint size of a part or kit (ids starting `kit.`) at a rotation, cells along x, y, z.
    pub fn size_of(&self, id: &str, rot: PartRot) -> [i16; 3] {
        let piece = if id.starts_with("kit.") { self.kit_piece(id, rot, false) } else { self.part_piece(id, rot) };
        let mut hi = [0i32; 3];
        for c in piece.cells() {
            for k in 0..3 {
                hi[k] = hi[k].max(c[k] + 1);
            }
        }
        hi.map(|v| v as i16)
    }

    /// A gun kit standing on the weather deck with its magazine and hoist below: the deck socket over the
    /// centreline deck at `x_m`, or the first column aft of it (up to 3 m) where the mount clears a sheer step,
    /// raised a row at a time (at most two) where the deck rises under the mount.
    pub fn gun(&mut self, id: &str, x_m: f64) {
        let h = self.half();
        let piece = self.kit_piece(id, YAW_0, false);
        let mut why = Vec::new();
        for x in (self.x(x_m - 3.0)..=self.x(x_m)).rev() {
            let deck = self.deck(x);
            for dy in 0..3 {
                let at = At::Socket { name: "deck", target: CellPos::new(x, deck + dy, h) };
                match self.try_resolve(&piece, &at, id) {
                    Ok(_) => {
                        self.kit(id, YAW_0, false, at);
                        return;
                    }
                    Err(e) => why.push(e),
                }
            }
        }
        let decks: Vec<(i16, i16)> = (self.x(x_m - 6.0)..=self.x(x_m + 2.0)).map(|x| (x, self.deck(x))).collect();
        panic!("{}\ndeck rows {decks:?}", why.join("\n"));
    }

    /// A vertical launcher kit sunk into the hull at vessel-local `x_m` with minimum z `z`: its top row
    /// replaces the weather deck, so the cell lids sit flush and nothing stands over them.
    pub fn vls(&mut self, id: &str, x_m: f64, z: i16) {
        let x = self.x(x_m);
        let [sx, sy, sz] = self.size_of(id, YAW_0);
        let top = (x..x + sx).flat_map(|x| (z..z + sz).map(move |z| (x, z))).map(|(x, z)| self.hull_top(x, z).unwrap()).max().unwrap();
        self.place(id, YAW_0, At::Exact(CellPos::new(x, top - (sy - 1), z)));
    }

    /// Set a part's load (fuel fraction, ballast kilograms).
    pub fn load(&mut self, slot: PartSlot, load: f64) { self.parts[slot.index()].load = Some(load); }

    // ---------------------------------------------------------------------------------------------------
    // Deckhouses.

    /// Voxelize deckhouse blocks onto the deck as one hollow union with one-cell walls and shaped edges.
    pub fn houses(&mut self, houses: &[House]) {
        let lay = self.lay;
        let in_union = |p: [f64; 3]| houses.iter().any(|h| h.inside(p, &lay));
        let [sx, sy, sz] = self.size().map(|v| v as i32);
        // Fill: centre inside a block, above the hull's own cells in its column, outside the hull envelope.
        let mut fill: BTreeSet<[i32; 3]> = BTreeSet::new();
        for x in 0..sx {
            for z in 0..sz {
                let top = self.hull_top(x as i16, z as i16).map(|v| v as i32);
                for y in 0..sy {
                    let c = CellPos::new(x as i16, y as i16, z as i16).centre_m();
                    if !in_union(c) {
                        continue;
                    }
                    let base_ok = match top {
                        Some(t) => y > t,
                        None => houses.iter().any(|h| h.inside(c, &lay) && c[1] >= h.base),
                    };
                    if base_ok && !self.solid([x, y, z]) && self.field.sdf(c) > 0.0 {
                        fill.insert([x, y, z]);
                    }
                }
            }
        }
        let context = |p: [i32; 3], fill: &BTreeSet<[i32; 3]>| fill.contains(&p) || self.solid(p) || (p.iter().all(|&v| v >= 0) && self.field.sdf(cp(p).centre_m()) <= 0.0);
        let mut wall: BTreeSet<[i32; 3]> = BTreeSet::new();
        for &c in &fill {
            let mut exposed = false;
            'n: for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if (dx, dy, dz) != (0, 0, 0) && !context(add(c, [dx, dy, dz]), &fill) {
                            exposed = true;
                            break 'n;
                        }
                    }
                }
            }
            if exposed {
                wall.insert(c);
            }
        }
        // Shapes from the union's corner pattern (hull envelope counts as inside, so walls meet the deck).
        let corner_in = |p: [f64; 3]| in_union(p) || self.field.sdf(p) <= 0.0;
        let mut shapes: BTreeMap<[i32; 3], CellShape> = BTreeMap::new();
        for &c in &wall {
            let mut pattern = 0u8;
            for bit in 0..8u8 {
                let p = [(c[0] + (bit & 1) as i32) as f64 * CELL_M, (c[1] + ((bit >> 1) & 1) as i32) as f64 * CELL_M, (c[2] + ((bit >> 2) & 1) as i32) as f64 * CELL_M];
                if corner_in(p) {
                    pattern |= 1 << bit;
                }
            }
            let s = if pattern == 0xFF { CellShape::CUBE } else { shape_from_corners(pattern).unwrap_or(CellShape::CUBE) };
            shapes.insert(c, s);
        }
        // No shaped wall may open a partial face onto the enclosed air.
        let mut seal = Vec::new();
        for (&c, &s) in &shapes {
            if s.is_cube() {
                continue;
            }
            let full = full_face_mask(s);
            for (b, f) in Face::ALL.iter().enumerate() {
                let n = add(c, face_dir(*f));
                if full & (1 << b) == 0 && fill.contains(&n) && !wall.contains(&n) {
                    seal.push(n);
                }
            }
        }
        for n in seal {
            wall.insert(n);
            shapes.insert(n, CellShape::CUBE);
        }
        let mut built = Vec::with_capacity(wall.len());
        for &c in &wall {
            let p = cp(c).centre_m();
            let owner = houses.iter().find(|h| h.inside(p, &lay)).unwrap_or(&houses[0]);
            let outside = |d: [i32; 3]| !context(add(c, d), &fill);
            let glazed = owner.glass.is_some_and(|(a, b)| {
                let h = p[1] - owner.base;
                let side = (outside([0, 0, 1]) || outside([0, 0, -1])) && p[0] >= owner.x1 - owner.front * h - 2.0;
                h >= a && h <= b && (outside([1, 0, 0]) || side)
            });
            let roof = outside([0, 1, 0]) && p[1] + CELL_M > owner.top() - 1e-9;
            let shape = shapes[&c];
            let cell = if glazed {
                Cell { material: Material::Glass, armour: ArmourTier::None, paint: PAINT_BLACK, shape }
            } else {
                Cell { material: owner.mat, armour: ArmourTier::None, paint: if roof && shape.is_cube() { PAINT_DECK } else { owner.paint }, shape }
            };
            built.push((cp(c), cell));
        }
        self.extra.extend(built);
        self.compose();
    }

    /// Hull numbers on both bows: pale MSDF text on the plating `rows_below` under the deck edge at `x`.
    pub fn hull_number(&mut self, text: &str, x: i16, rows_below: i16) {
        let y = self.hull_top(x, self.half()).unwrap() - rows_below;
        let zs = self.side_z(x, y).expect("side plating");
        let zp = 2 * self.half() - 1 - zs;
        for (z, face) in [(zs, Face::ZPos), (zp, Face::ZNeg)] {
            self.markings.push(Marking { kind: MarkingKind::HullNumber, cell: CellPos::new(x, y, z), face, paint: PAINT_PALE, size_cells: 3, text: text.to_string() });
        }
    }

    // ---------------------------------------------------------------------------------------------------
    // Output.

    fn blueprint_raw(&self, name: &str, name_key: &str) -> Blueprint {
        Blueprint {
            header: Header {
                format: BLUEPRINT_FORMAT,
                game_version: env!("CARGO_PKG_VERSION").to_string(),
                hash: ContentHash::EMPTY,
                kind: BlueprintKind::Ship,
                name: name.to_string(),
                name_key: Some(TextKey::new(name_key)),
                author: None,
                licence: DEFAULT_LICENCE.to_string(),
                lineage: Vec::new(),
            },
            hull: Hull { source: HullSource::Lofted(self.shell.clone()), planes: self.planes.clone(), design_waterline_y: self.waterline },
            cells: self.grid.to_rows(),
            paint: default_paint_table(),
            markings: self.markings.clone(),
            parts: self.parts.clone(),
            kits: self.kits.clone(),
            embedded_kits: Default::default(),
            officers: Default::default(),
            rules: Vec::new(),
            networks: Default::default(),
            graph: Default::default(),
        }
    }

    /// Watertight trunks: plate every air cell inside the hull that touches an open part replacing structure,
    /// so the part's own cells are the only path from the sea or the sky and its compartment stays sealed.
    /// Panics when another open part sits against such a part inside the hull, where no plating can go.
    fn trunks(&mut self) {
        let g = &self.grid;
        let [sx, sy, sz] = g.size.map(|v| v as i32);
        let inb = |p: [i32; 3]| (0..sx).contains(&p[0]) && (0..sy).contains(&p[1]) && (0..sz).contains(&p[2]);
        let idx = |p: [i32; 3]| (p[0] + sx * (p[2] + sz * p[1])) as usize;
        let watertight = |slot: usize| self.cat.part(&self.parts[slot].part).is_some_and(|d| d.watertight);
        let trunked = |slot: usize| !watertight(slot) && EMBEDDED.contains(&self.parts[slot].part.as_str());
        // Outside when the trunked parts are barriers: the sea, the sky, and the parts that stand in them.
        let owner = &self.owner;
        let beyond = flood_outside(g.size, |i| {
            let p = g.pos(content::ids::CellIndex(i as u32));
            g.cells[i].is_empty() && owner.get(&arr(p)).is_none_or(|&o| !trunked(o))
        });
        let mut plate = BTreeSet::new();
        for (&c, &slot) in &self.owner {
            if !trunked(slot) {
                continue;
            }
            for f in Face::ALL {
                let n = add(c, face_dir(f));
                if !inb(n) || beyond[idx(n)] || !g.cells[idx(n)].is_empty() {
                    continue;
                }
                match self.owner.get(&n) {
                    Some(&o) if o == slot || watertight(o) || trunked(o) => {}
                    Some(&o) => panic!("{} at {c:?} touches open part {} at {n:?} inside the hull: no room for its trunk", self.parts[slot].part, self.parts[o].part),
                    None => {
                        plate.insert(n);
                    }
                }
            }
        }
        let cell = Cell { material: self.hull_mat, armour: ArmourTier::None, paint: PAINT_SIDES, shape: CellShape::CUBE };
        for n in plate {
            self.extra.insert(cp(n), cell);
        }
        self.compose();
    }

    /// Trim heel to zero with a ballast tank: kilograms in `slot` until the centre of mass sits on the
    /// centreline (to the nearest 10 kg, within the tank's capacity).
    pub fn balance_heel(&mut self, slot: PartSlot) {
        let centre = self.half() as f64 * CELL_M;
        self.load(slot, 0.0);
        let st = compute_stats(&self.blueprint_raw("", "x"), self.cat);
        let tank = &self.parts[slot.index()];
        let def = self.cat.part(&tank.part).unwrap();
        let s = rotated_size(def.size, tank.rot);
        let tz = (tank.pos.z as f64 + s[2] as f64 * 0.5) * CELL_M;
        let m = st.tonnage_t * 1000.0;
        // (m zc + b tz) / (m + b) = centre  =>  b = m (centre - zc) / (tz - centre)
        let b = m * (centre - st.com_m[2]) / (tz - centre);
        let max = match def.function {
            content::part::PartFunction::Ballast { max_kg } => max_kg,
            _ => panic!("not a ballast tank"),
        };
        assert!(b >= 0.0, "the ballast tank is on the heavy side ({b:.0} kg)");
        assert!(b <= max, "heel needs {b:.0} kg of ballast, the tank holds {max:.0}");
        self.load(slot, (b / 10.0).round() * 10.0);
    }

    /// Set the design waterline to the flat equilibrium row, re-paint the bands, and return the blueprint as
    /// the editor would save it (canonical, hashed).
    pub fn finish(mut self, name: &str, name_key: &str) -> Blueprint {
        self.trunks();
        for _ in 0..3 {
            let bp = self.blueprint_raw(name, name_key);
            let h = hydrostatics(&bp, self.cat);
            let row = (h.level_m / CELL_M).round() as i16;
            if row == self.waterline {
                break;
            }
            self.waterline = row;
            self.revoxelize();
        }
        let mut bp = self.blueprint_raw(name, name_key);
        canonicalize_blueprint(&mut bp);
        bp.header.hash = compute_hash(&bp);
        bp
    }
}

// -------------------------------------------------------------------------------------------------------
// Checks.

/// Flat-water hydrostatics of a design at full load.
#[derive(Clone, Debug, Default)]
pub struct Hydro {
    pub tonnage_t: f64,
    /// Equilibrium waterline height, vessel-local metres, and its row.
    pub level_m: f64,
    pub row: i16,
    /// Buoyant volume below the design waterline row's bottom face, tonnes of water.
    pub at_design_t: f64,
    /// Buoyant volume above the waterline over displacement.
    pub reserve: f64,
    pub kb: f64,
    pub bm: f64,
    pub kg: f64,
    pub gm: f64,
    pub beam_wl_m: f64,
    pub draft_m: f64,
    pub lcb: f64,
    pub lcg: f64,
    pub trim_deg: f64,
    pub heel_deg: f64,
    pub gm_over_beam: f64,
}

/// Cells of the dense grid reachable from outside its bounding box through empty cells.
pub fn outside(g: &CellGrid) -> Vec<bool> { flood_outside(g.size, |i| g.cells[i].is_empty()) }

/// Cells water can pass, by the simulation's rule (`sim_core::compartments`): empty cells and the cells of
/// open equipment pass; structure and watertight parts (tanks, magazines, doors, ballast) stop it.
pub fn passable(bp: &Blueprint, cat: &Catalogue, g: &CellGrid) -> Vec<bool> {
    let mut pass: Vec<bool> = g.cells.iter().map(|c| c.is_empty()).collect();
    let [sx, sy, sz] = g.size.map(|v| v as i32);
    for p in &bp.parts {
        let Some(def) = cat.part(&p.part) else { continue };
        if def.watertight {
            continue;
        }
        let s = rotated_size(def.size, p.rot);
        for x in 0..s[0] as i32 {
            for y in 0..s[1] as i32 {
                for z in 0..s[2] as i32 {
                    let c = [p.pos.x as i32 + x, p.pos.y as i32 + y, p.pos.z as i32 + z];
                    if (0..sx).contains(&c[0]) && (0..sy).contains(&c[1]) && (0..sz).contains(&c[2]) {
                        pass[(c[0] + sx * (c[2] + sz * c[1])) as usize] = true;
                    }
                }
            }
        }
    }
    pass
}

/// Cells reachable from outside the bounding box of a grid of `size` through cells where `pass(index)`.
pub fn flood_outside(size: [u16; 3], pass: impl Fn(usize) -> bool) -> Vec<bool> {
    let [sx, sy, sz] = size.map(|v| v as usize);
    let idx = |x: usize, y: usize, z: usize| x + sx * (z + sz * y);
    let mut out = vec![false; sx * sy * sz];
    let mut q = VecDeque::new();
    for y in 0..sy {
        for z in 0..sz {
            for x in 0..sx {
                let edge = x == 0 || z == 0 || y == 0 || x == sx - 1 || z == sz - 1 || y == sy - 1;
                if edge && pass(idx(x, y, z)) {
                    out[idx(x, y, z)] = true;
                    q.push_back((x, y, z));
                }
            }
        }
    }
    while let Some((x, y, z)) = q.pop_front() {
        for (a, b, c) in [(x.wrapping_sub(1), y, z), (x + 1, y, z), (x, y.wrapping_sub(1), z), (x, y + 1, z), (x, y, z.wrapping_sub(1)), (x, y, z + 1)] {
            if a < sx && b < sy && c < sz && !out[idx(a, b, c)] && pass(idx(a, b, c)) {
                out[idx(a, b, c)] = true;
                q.push_back((a, b, c));
            }
        }
    }
    out
}

/// Cells inside the hull (enclosed when every part is taken as a barrier) that water reaches from outside by
/// the simulation's rule: a compartment opened by an open part that replaces structure.
pub fn open_cells(bp: &Blueprint, cat: &Catalogue) -> Vec<CellPos> {
    let g = build_grid(bp, cat);
    let sealed = outside(&g);
    let pass = passable(bp, cat, &g);
    let open = flood_outside(g.size, |i| pass[i]);
    (0..g.len()).filter(|&i| open[i] && !sealed[i] && g.cells[i].is_empty()).map(|i| g.pos(content::ids::CellIndex(i as u32))).collect()
}

/// Hydrostatics from the dense grid: every cell not reachable from outside through empty cells is buoyant.
pub fn hydrostatics(bp: &Blueprint, cat: &Catalogue) -> Hydro {
    let st: BlueprintStats = compute_stats(bp, cat);
    let g = build_grid(bp, cat);
    let [sx, sy, sz] = g.size.map(|v| v as usize);
    let idx = |x: usize, y: usize, z: usize| x + sx * (z + sz * y);
    // Water: empty cells reached from outside through empty cells and open equipment. Every live cell
    // displaces, a part outside the hull included, as in the simulation's buoyancy blocks.
    let pass = passable(bp, cat, &g);
    let reached = flood_outside(g.size, |i| pass[i]);
    let out: Vec<bool> = reached.iter().zip(&g.cells).map(|(&r, c)| r && c.is_empty()).collect();
    // Per row: count, sums of x, z, z^2, x^2 of buoyant cell centres.
    let mut rows = vec![[0.0f64; 5]; sy];
    for y in 0..sy {
        for z in 0..sz {
            for x in 0..sx {
                if !out[idx(x, y, z)] {
                    let (xc, zc) = ((x as f64 + 0.5) * CELL_M, (z as f64 + 0.5) * CELL_M);
                    let r = &mut rows[y];
                    r[0] += 1.0;
                    r[1] += xc;
                    r[2] += zc;
                    r[3] += zc * zc;
                    r[4] += xc * xc;
                }
            }
        }
    }
    let rho = cat.tuning.buoyancy.water_density / 1000.0;
    let cv = CELL_M * CELL_M * CELL_M;
    let volume_below = |h: f64| -> f64 { rows.iter().enumerate().map(|(y, r)| r[0] * cv * ((h - y as f64 * CELL_M) / CELL_M).clamp(0.0, 1.0)).sum() };
    let total = volume_below(sy as f64 * CELL_M);
    let need = st.tonnage_t / rho;
    let mut hd = Hydro { tonnage_t: st.tonnage_t, ..Default::default() };
    if !(need > 0.0) || total < need {
        hd.level_m = f64::INFINITY;
        return hd;
    }
    let (mut lo, mut hi) = (0.0, sy as f64 * CELL_M);
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if volume_below(mid) < need {
            lo = mid
        } else {
            hi = mid
        }
    }
    let h = 0.5 * (lo + hi);
    hd.level_m = h;
    hd.row = (h / CELL_M).round() as i16;
    hd.at_design_t = volume_below(bp.hull.design_waterline_y as f64 * CELL_M) * rho;
    hd.reserve = (total - need) / need;
    let (mut m_y, mut m_x) = (0.0, 0.0);
    let mut keel = None;
    for (y, r) in rows.iter().enumerate() {
        let f = ((h - y as f64 * CELL_M) / CELL_M).clamp(0.0, 1.0);
        if f <= 0.0 || r[0] == 0.0 {
            continue;
        }
        keel.get_or_insert(y);
        m_y += r[0] * cv * f * (y as f64 * CELL_M + 0.5 * f * CELL_M);
        m_x += cv * f * r[1];
    }
    hd.kb = m_y / need;
    hd.lcb = m_x / need;
    hd.lcg = st.com_m[0];
    hd.kg = st.com_m[1];
    hd.draft_m = h - keel.unwrap_or(0) as f64 * CELL_M;
    // Trim from the waterplane row's longitudinal inertia.
    let wr = ((h / CELL_M).floor() as usize).min(sy - 1);
    let r = rows[wr];
    let a = CELL_M * CELL_M;
    if r[0] > 0.0 {
        let xbar = r[1] / r[0];
        let il = a * (r[4] - r[0] * xbar * xbar) + r[0] * a * a / 12.0;
        let gml = hd.kb + il / need - hd.kg;
        hd.trim_deg = libm::atan((hd.lcg - hd.lcb) / gml).to_degrees();
        let (mut zmin, mut zmax) = (sz, 0);
        for z in 0..sz {
            for x in 0..sx {
                if !out[idx(x, wr, z)] {
                    zmin = zmin.min(z);
                    zmax = zmax.max(z);
                }
            }
        }
        hd.beam_wl_m = (zmax + 1 - zmin) as f64 * CELL_M;
    }
    // GM from the righting arm at a small heel either way, with cells immersed by their fraction under the
    // tilted water surface: smooth across row boundaries, where a waterplane taken from one row is not.
    let mut n = vec![0.0f64; sy * sz];
    for y in 0..sy {
        for z in 0..sz {
            n[y * sz + z] = (0..sx).filter(|&x| !out[idx(x, y, z)]).count() as f64;
        }
    }
    let zc = sz as f64 * 0.5 * CELL_M;
    let heeled = |phi: f64| -> (f64, f64, f64) {
        let t = libm::tan(phi);
        let vol = |h0: f64| -> (f64, f64, f64) {
            let (mut v, mut mz, mut my) = (0.0, 0.0, 0.0);
            for y in 0..sy {
                for z in 0..sz {
                    let k = n[y * sz + z];
                    if k == 0.0 {
                        continue;
                    }
                    let zm = (z as f64 + 0.5) * CELL_M;
                    let f = ((h0 + (zm - zc) * t - y as f64 * CELL_M) / CELL_M).clamp(0.0, 1.0);
                    let dv = k * cv * f;
                    v += dv;
                    mz += dv * zm;
                    my += dv * (y as f64 * CELL_M + 0.5 * f * CELL_M);
                }
            }
            (v, mz, my)
        };
        let (mut lo, mut hi) = (-(sz as f64), sy as f64 * CELL_M + sz as f64);
        for _ in 0..60 {
            let mid = 0.5 * (lo + hi);
            if vol(mid).0 < need {
                lo = mid
            } else {
                hi = mid
            }
        }
        let (v, mz, my) = vol(0.5 * (lo + hi));
        let (zb, yb) = (mz / v, my / v);
        // Righting arm: horizontal offset of B from G in the earth frame, starboard down positive.
        ((zb - st.com_m[2]) * libm::cos(phi) + (yb - st.com_m[1]) * libm::sin(phi), zb, yb)
    };
    let phi = 2.0f64.to_radians();
    let (gz_p, _, _) = heeled(phi);
    let (gz_n, _, _) = heeled(-phi);
    let (gz_0, _, _) = heeled(0.0);
    hd.gm = (gz_p - gz_n) / (2.0 * libm::sin(phi));
    hd.bm = hd.gm + hd.kg - hd.kb;
    // Upright, B sits under the waterplane's centre; G off the centreline lists the ship by GZ(0) / GM.
    hd.heel_deg = if hd.gm > 0.0 { libm::atan(-gz_0 / hd.gm).to_degrees() } else { 90.0 };
    hd.gm_over_beam = hd.gm / st.beam_m.max(1e-9);
    hd
}

/// Part cells that replace structure, other than the [`EMBEDDED`] parts (a part buried in a deck or plating).
pub fn buried_parts(bp: &Blueprint, cat: &Catalogue) -> Vec<(usize, String)> {
    let g = CellGrid::from_rows(&bp.cells);
    let mut out = Vec::new();
    for (i, p) in bp.parts.iter().enumerate() {
        if EMBEDDED.contains(&p.part.as_str()) {
            continue;
        }
        let def = cat.part(&p.part).unwrap();
        let s = rotated_size(def.size, p.rot);
        'cells: for x in 0..s[0] as i16 {
            for y in 0..s[1] as i16 {
                for z in 0..s[2] as i16 {
                    if !g.get(CellPos::new(p.pos.x + x, p.pos.y + y, p.pos.z + z)).is_empty() {
                        out.push((i, p.part.to_string()));
                        break 'cells;
                    }
                }
            }
        }
    }
    out
}

/// Supply and draw on the power network from the part ports (kW).
pub fn power(bp: &Blueprint, cat: &Catalogue) -> (f64, f64) {
    use content::enums::NetworkKind;
    use content::part::PortRole;
    let (mut supply, mut draw) = (0.0, 0.0);
    for p in &bp.parts {
        for q in &cat.part(&p.part).unwrap().ports {
            if q.network == NetworkKind::Power {
                match q.role {
                    PortRole::Supply => supply += q.rate,
                    PortRole::Consume => draw += q.rate,
                    _ => {}
                }
            }
        }
    }
    (supply, draw)
}

/// Shaft power and what the propulsors can absorb (kW).
pub fn drive(bp: &Blueprint, cat: &Catalogue) -> (f64, f64) {
    use content::part::PartFunction;
    let (mut shaft, mut absorb) = (0.0, 0.0);
    for p in &bp.parts {
        match &cat.part(&p.part).unwrap().function {
            PartFunction::Engine(e) => shaft += e.shaft_kw,
            PartFunction::Propulsor(d) => absorb += d.max_shaft_kw,
            _ => {}
        }
    }
    (shaft, absorb)
}

/// ASCII views for eyeballing a design: profile (from starboard), plan (from above) and one section
/// (`BP_SECTION` picks the column; default amidships).
pub fn views(bp: &Blueprint, cat: &Catalogue) -> String {
    use content::part::PartCategory as C;
    let g = build_grid(bp, cat);
    let s = CellGrid::from_rows(&bp.cells);
    let [sx, sy, sz] = g.size.map(|v| v as i16);
    let mut owner: BTreeMap<(i16, i16, i16), char> = BTreeMap::new();
    for p in &bp.parts {
        let def = cat.part(&p.part).unwrap();
        let ch = match def.category {
            C::Gun => 'G',
            C::Missile => 'M',
            C::Underwater => 'T',
            C::Defence => 'D',
            C::Offboard => 'O',
            C::Sensor => 'R',
            C::Propulsion => 'E',
            C::Power => 'P',
            C::Fuel => 'F',
            C::Control => 'C',
            C::Crew => 'B',
            C::DamageControl => 'p',
            C::Hull => 'H',
            C::SpecialOps => 'S',
        };
        let sz3 = rotated_size(def.size, p.rot);
        for x in 0..sz3[0] as i16 {
            for y in 0..sz3[1] as i16 {
                for z in 0..sz3[2] as i16 {
                    owner.insert((p.pos.x + x, p.pos.y + y, p.pos.z + z), ch);
                }
            }
        }
    }
    let ch = |x: i16, y: i16, z: i16| -> Option<char> {
        if let Some(c) = owner.get(&(x, y, z)) {
            return Some(*c);
        }
        let c = s.get(CellPos::new(x, y, z));
        if c.is_empty() {
            return None;
        }
        Some(match c.material {
            Material::Glass => 'o',
            _ if !c.shape.is_cube() => '/',
            Material::Composite => '%',
            _ => '#',
        })
    };
    let wl = bp.hull.design_waterline_y;
    let mut out = String::new();
    out.push_str("profile (bow right):\n");
    for y in (0..sy).rev() {
        for x in 0..sx {
            out.push((0..sz).find_map(|z| ch(x, y, z)).unwrap_or(if y == wl { '~' } else { ' ' }));
        }
        out.push('\n');
    }
    out.push_str("plan (bow right, starboard down):\n");
    for z in 0..sz {
        for x in 0..sx {
            out.push((0..sy).rev().find_map(|y| ch(x, y, z)).unwrap_or(' '));
        }
        out.push('\n');
    }
    let mid = std::env::var("BP_SECTION").ok().and_then(|v| v.parse().ok()).unwrap_or(sx / 2);
    out.push_str(&format!("section at x {mid} (looking forward):\n"));
    for y in (0..sy).rev() {
        for z in 0..sz {
            out.push(ch(mid, y, z).unwrap_or(if y == wl { '~' } else { ' ' }));
        }
        out.push('\n');
    }
    out
}

/// Horizontal slices of the structure: `#` solid, `.` enclosed air, ` ` outside. For planning interiors.
pub fn slices(bp: &Blueprint, cat: &Catalogue) -> String {
    let g = build_grid(bp, cat);
    let [sx, sy, sz] = g.size.map(|v| v as usize);
    let idx = |x: usize, y: usize, z: usize| x + sx * (z + sz * y);
    let out = outside(&g);
    let mut s = String::new();
    for y in (0..sy).rev() {
        s.push_str(&format!("row {y}:\n"));
        for z in 0..sz {
            for x in 0..sx {
                s.push(if !g.cells[idx(x, y, z)].is_empty() { '#' } else if out[idx(x, y, z)] { ' ' } else { '.' });
            }
            s.push('\n');
        }
    }
    s
}

/// A one-line summary.
pub fn summary(bp: &Blueprint, cat: &Catalogue) -> String {
    let st = compute_stats(bp, cat);
    let h = hydrostatics(bp, cat);
    let (supply, draw) = power(bp, cat);
    let (shaft, absorb) = drive(bp, cat);
    format!(
        "{}: {:.1} t (light {:.1}), {} S, {:.1} x {:.1} x {:.1} m, {} cells, {} parts, {} berths | draft {:.2} m, wl row {} (design {}), GM {:.2} m = {:.1}% of beam, KB {:.2} BM {:.2} KG {:.2}, LCB {:.2} LCG {:.2} trim {:.2} deg, heel {:.2} deg, reserve {:.0}% | power {:.0}/{:.0} kW, shaft {:.0} kW (props {:.0})",
        bp.header.name, st.tonnage_t, st.light_t, st.cost, st.length_m, st.beam_m, st.height_m, st.cell_count, st.part_count, st.berths,
        h.draft_m, h.row, bp.hull.design_waterline_y, h.gm, h.gm_over_beam * 100.0, h.kb, h.bm, h.kg, h.lcb, h.lcg, h.trim_deg, h.heel_deg, h.reserve * 100.0,
        supply, draw, shaft, absorb
    )
}
