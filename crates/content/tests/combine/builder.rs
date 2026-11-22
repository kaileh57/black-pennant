//! The ship builder behind the Combine's stock classes: the editor's steps, scripted.
//!
//! A class is built the way a player builds a ship: loft a shell (stations, beam, draft, sheer, rake), cut its
//! decks and bulkheads, bake it to freeform cells, raise deckhouses on the weather deck, then drop stock kits and
//! single parts where the editor would let them sit. The builder then floats the result: it finds the waterline
//! at which the hull envelope displaces the full-load tonnage, repaints the boot-topping band there, and reports
//! the hydrostatics the class table is tuned against (GM, trim lever, freeboard, power balance).
//!
//! Frames: vessel-local cells, +X bow, +Y up, +Z starboard, origin at the grid's minimum corner. The baked hull is
//! raised [`KEEL_ROOM`] cells so propellers, rudders and sonar domes can hang under the keel inside the cell frame.
use content::blueprint::io::{canonicalize_blueprint, compute_hash, default_paint_table};
use content::blueprint::{Blueprint, BlueprintKind, Header, KitPlacement, PartPlacement, SocketBinding, DEFAULT_LICENCE};
use content::catalogue::Catalogue;
use content::cell::{ArmourTier, Cell, CellGrid, CellPos, CellShape, Material, CELL_M};
use content::enums::Axis;
use content::hull::{voxelize, Hull, HullShell, HullSource, InternalPlane, ShellField, Station, PAINT_DECK, PAINT_SIDES};
use content::ids::{ContentHash, KitId, KitSlot, PartId, PartSlot, TextKey};
use content::kit::{kit_part_placements, kit_sockets_at, rotated_size, PartRot};
use content::enums::NetworkKind;
use content::part::{PartFunction, Placement, PortRole};
use content::stats::{compute_stats, BlueprintStats};

/// Cells of room left under the keel for underwater appendages (a propeller is 3 cells tall).
pub const KEEL_ROOM: i16 = 3;
/// Headroom above the weather deck for houses and masts, in cells.
const HEADROOM: usize = 64;
/// Sea water, tonnes per cubic metre (the tuning table's buoyancy density).
pub const SEA_T_PER_M3: f64 = 1.025;

// ---------------------------------------------------------------------------------------------------------
// Specs

/// Lofted shell parameters, in the editor's terms.
#[derive(Clone, Copy, Debug)]
pub struct HullSpec {
    pub length: f64,
    /// Beam at the deck edge amidships, metres.
    pub beam: f64,
    /// Keel depth below the design datum amidships, metres.
    pub draft: f64,
    /// Deck edge above the datum amidships, metres.
    pub freeboard: f64,
    /// Bottom deadrise amidships, degrees.
    pub deadrise: f64,
    /// Extra deadrise at the transom fading to zero at the stem (planing hulls), degrees.
    pub planing: f64,
    /// Topside flare amidships, degrees (negative is tumblehome).
    pub flare: f64,
    pub bow_rake: f64,
    /// How far the sheer rises toward the stem, as a multiple of the default rise.
    pub sheer_rise: f64,
    pub material: Material,
}

impl HullSpec {
    pub const fn new(length: f64, beam: f64, draft: f64, freeboard: f64) -> Self {
        HullSpec { length, beam, draft, freeboard, deadrise: 10.0, planing: 0.0, flare: 8.0, bow_rake: 25.0, sheer_rise: 1.0, material: Material::Steel }
    }
    pub const fn deadrise(mut self, mid: f64, planing: f64) -> Self { self.deadrise = mid; self.planing = planing; self }
    pub const fn flare(mut self, deg: f64) -> Self { self.flare = deg; self }
    pub const fn rake(mut self, deg: f64) -> Self { self.bow_rake = deg; self }
    pub const fn sheer(mut self, rise: f64) -> Self { self.sheer_rise = rise; self }
    pub const fn material(mut self, m: Material) -> Self { self.material = m; self }
}

/// A deckhouse: an enclosed steel box on whatever stands below it (the weather deck or an earlier house).
#[derive(Clone, Copy, Debug)]
pub struct House {
    /// Aft and forward ends as fractions of the hull length.
    pub aft: f64,
    pub fwd: f64,
    /// Width in cells, centred on the centreline (even widths centre exactly).
    pub width: i16,
    /// Wall height in cells above the highest deck cell under it; the roof is one more.
    pub height: i16,
    /// Glazed band in the top wall row at the forward end (a bridge).
    pub bridge: bool,
    /// Plating, when it differs from the hull's (a light alloy superstructure on a steel hull).
    pub material: Option<Material>,
}
impl House {
    /// Plate the house in `m` instead of the hull's material. Added for the Act 1 sheets; the later acts'
    /// sheets never use it.
    #[allow(dead_code)]
    pub const fn material(mut self, m: Material) -> Self { self.material = Some(m); self }
}
pub const fn house(aft: f64, fwd: f64, width: i16, height: i16) -> House { House { aft, fwd, width, height, bridge: false, material: None } }
pub const fn bridge(aft: f64, fwd: f64, width: i16, height: i16) -> House { House { aft, fwd, width, height, bridge: true, material: None } }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side { Port, Starboard }

/// Where a fit goes. `t` is the station along the hull (0 transom, 1 stem) of the fit's socket.
#[derive(Clone, Copy, Debug)]
pub enum At {
    /// On the topmost structure: weather deck or house roof.
    Top(f64),
    /// On the floor of the hull below the weather deck (bottom plating or a platform deck).
    Hold(f64),
    /// On the floor of a deckhouse, inside it.
    InHouse(f64),
    /// Hung under the bottom (sonar domes).
    Keel(f64),
    /// Hung under the bottom at the stern, `x` cells forward of the transom.
    Stern(i16),
    /// On the weather deck against the side.
    Edge(f64, Side),
    /// Sunk into the hull with its top row flush in the weather deck, taking over the deck plating it covers (a
    /// vertical launcher). Added for the Act 2 sheets; the Act 1 sheets never use it.
    #[allow(dead_code)]
    Flush(f64),
}

#[derive(Clone, Copy, Debug)]
pub enum Item { Kit(&'static str), Part(&'static str) }

#[derive(Clone, Copy, Debug)]
pub struct Fit { pub item: Item, pub at: At, pub dz: i16, pub yaw: u8, pub load: Option<f64> }
impl Fit {
    /// Offset from the centreline in cells, positive to starboard.
    pub const fn dz(mut self, dz: i16) -> Self { self.dz = dz; self }
    /// Turn to face aft.
    pub const fn aft(mut self) -> Self { self.yaw = 2; self }
}
pub const fn kit(id: &'static str, at: At) -> Fit { Fit { item: Item::Kit(id), at, dz: 0, yaw: 0, load: None } }
pub const fn part(id: &'static str, at: At) -> Fit { Fit { item: Item::Part(id), at, dz: 0, yaw: 0, load: None } }

/// Armour painted on the outer plating of a band of the hull and houses.
#[derive(Clone, Copy, Debug)]
pub struct Armour { pub aft: f64, pub fwd: f64, pub tier: ArmourTier, pub houses: bool }

/// Paint scheme: indices into the default paint table.
#[derive(Clone, Copy, Debug)]
pub struct Scheme { pub sides: u8, pub house: u8 }
/// Combine warships: ocean grey hull, haze grey superstructure, deck blue decks.
pub const WARSHIP: Scheme = Scheme { sides: 3, house: 2 };
/// Combine auxiliaries and merchants: haze grey hull, pale grey superstructure.
pub const AUXILIARY: Scheme = Scheme { sides: 2, house: 0 };
/// Civilian craft: navy grey hull, light grey house.
pub const CIVILIAN: Scheme = Scheme { sides: 5, house: 1 };

pub struct ShipSpec {
    /// File stem under `blueprints/combine/act1/`.
    pub file: &'static str,
    /// English class name as the header's ship name.
    pub name: &'static str,
    /// Translation key of the class name.
    pub key: &'static str,
    pub hull: HullSpec,
    /// Watertight transverse bulkheads as fractions of the length.
    pub bulkheads: &'static [f64],
    /// Platform deck height above the datum in metres, if any.
    pub platform_m: Option<f64>,
    pub houses: &'static [House],
    pub fits: &'static [Fit],
    pub armour: &'static [Armour],
    pub scheme: Scheme,
}

// ---------------------------------------------------------------------------------------------------------
// Report

/// Hydrostatics and outfit of one built class, for the class table and its tests.
#[derive(Clone, Debug)]
pub struct Report {
    pub stats: BlueprintStats,
    /// Floating waterline above the keel, metres.
    pub draft_m: f64,
    /// Deepest appendage below the waterline, metres.
    pub draft_max_m: f64,
    pub displacement_at_deck_t: f64,
    pub freeboard_m: f64,
    pub gm_m: f64,
    /// LCG minus LCB as a fraction of the length (positive: bow down).
    pub trim_lever: f64,
    /// Static trim the lever gives against the longitudinal metacentric height, degrees (positive: bow down).
    pub trim_deg: f64,
    pub power_kw: f64,
    pub load_kw: f64,
    pub shaft_kw: f64,
    /// Fits that found no room (a build sheet to fix).
    pub errors: Vec<String>,
}

pub struct Built { pub blueprint: Blueprint, pub report: Report }

// ---------------------------------------------------------------------------------------------------------
// Working volume

#[derive(Clone)]
struct Vol {
    n: [usize; 3],
    cells: Vec<Cell>,
    /// Part slot + 1 per cell, 0 = no part.
    parts: Vec<u16>,
    /// True for hull structure (before houses): displacement and envelope come from it.
    hull: Vec<bool>,
}
impl Vol {
    fn idx(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        if x < 0 || y < 0 || z < 0 || x as usize >= self.n[0] || y as usize >= self.n[1] || z as usize >= self.n[2] { return None; }
        Some(x as usize + self.n[0] * (z as usize + self.n[2] * y as usize))
    }
    fn cell(&self, x: i32, y: i32, z: i32) -> Cell { self.idx(x, y, z).map_or(Cell::EMPTY, |i| self.cells[i]) }
    fn solid(&self, x: i32, y: i32, z: i32) -> bool { !self.cell(x, y, z).is_empty() }
    fn part_at(&self, x: i32, y: i32, z: i32) -> bool { self.idx(x, y, z).is_some_and(|i| self.parts[i] != 0) }
    fn set(&mut self, x: i32, y: i32, z: i32, c: Cell) { if let Some(i) = self.idx(x, y, z) { self.cells[i] = c; } }
    /// Highest structure cell in a column.
    fn top(&self, x: i32, z: i32) -> Option<i32> { (0..self.n[1] as i32).rev().find(|&y| self.solid(x, y, z)) }
    /// Highest hull cell in a column.
    fn hull_top(&self, x: i32, z: i32) -> Option<i32> { (0..self.n[1] as i32).rev().find(|&y| self.idx(x, y, z).is_some_and(|i| self.hull[i])) }
    /// Lowest structure cell in a column.
    fn bottom(&self, x: i32, z: i32) -> Option<i32> { (0..self.n[1] as i32).find(|&y| self.solid(x, y, z)) }
}

/// Empty cells enclosed by structure on all six sides along the axes (the validator's envelope, closed above).
struct Envelope { n: [usize; 3], inside: Vec<bool> }
impl Envelope {
    fn of(v: &Vol, only_hull: bool) -> Envelope {
        let [nx, ny, nz] = v.n;
        let s = |x: usize, y: usize, z: usize| { let i = x + nx * (z + nz * y); if only_hull { v.hull[i] } else { !v.cells[i].is_empty() } };
        let mut inside = vec![false; nx * ny * nz];
        // Spans per row along each axis.
        let mut xs = vec![(usize::MAX, 0usize); ny * nz];
        let mut zs = vec![(usize::MAX, 0usize); ny * nx];
        let mut ys = vec![(usize::MAX, 0usize); nx * nz];
        for y in 0..ny { for z in 0..nz { for x in 0..nx {
            if s(x, y, z) {
                let a = &mut xs[y * nz + z]; *a = (a.0.min(x), a.1.max(x));
                let b = &mut zs[y * nx + x]; *b = (b.0.min(z), b.1.max(z));
                let c = &mut ys[z * nx + x]; *c = (c.0.min(y), c.1.max(y));
            }
        }}}
        for y in 0..ny { for z in 0..nz { for x in 0..nx {
            let i = x + nx * (z + nz * y);
            if s(x, y, z) { continue; }
            let a = xs[y * nz + z];
            let b = zs[y * nx + x];
            let c = ys[z * nx + x];
            inside[i] = a.0 < x && x < a.1 && b.0 < z && z < b.1 && c.0 < y && y < c.1;
        }}}
        Envelope { n: v.n, inside }
    }
    fn at(&self, x: i32, y: i32, z: i32) -> bool {
        if x < 0 || y < 0 || z < 0 || x as usize >= self.n[0] || y as usize >= self.n[1] || z as usize >= self.n[2] { return false; }
        self.inside[x as usize + self.n[0] * (z as usize + self.n[2] * y as usize)]
    }
}

// ---------------------------------------------------------------------------------------------------------
// Hull

/// Station table of the Combine yards: (t, beam, draft, deadrise, flare, sheer) as fractions of the amidships
/// values (deadrise and flare in multiples of the amidships angle). The stem station's beam is the stem bar.
const STATIONS: [(f64, f64, f64, f64, f64, f64); 8] = [
    (0.00, 0.80, 0.55, 0.50, 0.50, 0.96),
    (0.12, 0.92, 0.80, 0.70, 0.70, 0.94),
    (0.30, 1.00, 0.98, 0.90, 0.90, 0.94),
    (0.50, 1.00, 1.00, 1.00, 1.00, 1.00),
    (0.68, 0.96, 1.00, 1.30, 1.20, 1.08),
    (0.82, 0.80, 0.94, 1.80, 1.70, 1.20),
    (0.93, 0.50, 0.82, 2.40, 2.20, 1.34),
    (1.00, 0.00, 0.60, 3.00, 2.60, 1.44),
];
const STEM_BAR_M: f64 = 0.6;

pub fn shell(h: &HullSpec) -> HullShell {
    let stations = STATIONS
        .iter()
        .map(|&(t, b, d, dr, fl, sh)| Station {
            t,
            beam_m: if t >= 1.0 { STEM_BAR_M } else { b * h.beam },
            draft_m: d * h.draft,
            deadrise_deg: (dr * h.deadrise + h.planing * (1.0 - t)).clamp(0.0, 40.0),
            flare_deg: (fl * h.flare).clamp(-25.0, 35.0).min(85.0 - (dr * h.deadrise + h.planing * (1.0 - t)).clamp(0.0, 40.0)),
            sheer_m: h.freeboard * (1.0 + (sh - 1.0) * if sh > 1.0 { h.sheer_rise } else { 1.0 }),
        })
        .collect();
    HullShell { length_m: h.length, stations, bow_rake_deg: h.bow_rake, transom_rake_deg: 8.0, transom_width: 1.0, mirrored: true, shell_cells: 1 }
}

fn planes(spec: &ShipSpec, shell: &HullShell) -> Vec<InternalPlane> {
    let lay = ShellField::new(shell).expect("valid shell").layout();
    let mut out = Vec::new();
    if let Some(m) = spec.platform_m {
        out.push(InternalPlane { axis: Axis::Y, offset: lay.waterline_y + (m / CELL_M).floor() as i16, watertight: true });
    }
    for &f in spec.bulkheads {
        let x = (f * shell.length_m / CELL_M).floor() as i16 - lay.x_offset;
        out.push(InternalPlane { axis: Axis::X, offset: x, watertight: true });
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Building

struct Ship<'a> {
    spec: &'a ShipSpec,
    cat: &'a Catalogue,
    vol: Vol,
    len_cells: i32,
    /// Centreline plane in cells (`z = half` separates port from starboard).
    half: i32,
    parts: Vec<PartPlacement>,
    kits: Vec<KitPlacement>,
    /// Enclosed air of the hull alone, and of hull plus houses; refreshed whenever structure is added.
    env_hull: Envelope,
    env_all: Envelope,
}

/// Build one class at a design waterline row (shifted frame), or at the shell's datum when `None`.
fn build_at(spec: &ShipSpec, cat: &Catalogue, waterline: Option<i16>, fits: &[Fit], extra: &[Fit]) -> (Blueprint, Vol, Vec<String>) {
    let shell = shell(&spec.hull);
    if let Err(e) = content::hull::validate_shell(&shell, 150.0) { panic!("{}: bad shell: {e}", spec.file); }
    let lay = ShellField::new(&shell).expect("valid shell").layout();
    let planes = planes(spec, &shell);
    let wl_unshifted = waterline.map_or(lay.waterline_y, |w| w - KEEL_ROOM);
    let hull = Hull { source: HullSource::Lofted(shell.clone()), planes: planes.clone(), design_waterline_y: wl_unshifted };
    let vox = voxelize(&hull);
    let g = &vox.grid;
    let n = [g.size[0] as usize, g.size[1] as usize + KEEL_ROOM as usize + HEADROOM, g.size[2] as usize];
    let mut vol = Vol { n, cells: vec![Cell::EMPTY; n[0] * n[1] * n[2]], parts: vec![0; n[0] * n[1] * n[2]], hull: vec![false; n[0] * n[1] * n[2]] };
    for y in 0..g.size[1] as i32 {
        for z in 0..g.size[2] as i32 {
            for x in 0..g.size[0] as i32 {
                let mut c = g.get(CellPos::new(x as i16, y as i16, z as i16));
                if c.is_empty() { continue; }
                if c.material == Material::Steel { c.material = spec.hull.material; }
                if c.paint == PAINT_SIDES { c.paint = spec.scheme.sides; }
                let yy = y + KEEL_ROOM as i32;
                vol.set(x, yy, z, c);
                let i = vol.idx(x, yy, z).unwrap();
                vol.hull[i] = true;
            }
        }
    }
    let env_hull = Envelope::of(&vol, true);
    let env_all = Envelope::of(&vol, false);
    let mut ship = Ship { spec, cat, vol, len_cells: g.size[0] as i32, half: lay.half_width_cells as i32, parts: Vec::new(), kits: Vec::new(), env_hull, env_all };
    for h in spec.houses { ship.house(h); }
    ship.env_all = Envelope::of(&ship.vol, false);
    for a in spec.armour { ship.armour(a); }
    let mut errors = Vec::new();
    for (i, f) in fits.iter().chain(extra).enumerate() {
        if let Err(e) = ship.fit(f) { errors.push(format!("{}: fit {i} ({:?} at {:?}): {e}", spec.file, f.item, f.at)); }
    }
    ship.trunks();
    let wl = vox.design_waterline_y + KEEL_ROOM;
    let bp = ship.blueprint(Hull {
        source: HullSource::Freeform,
        planes: planes.iter().map(|p| InternalPlane { offset: if p.axis == Axis::Y { p.offset + KEEL_ROOM } else { p.offset }, ..*p }).collect(),
        design_waterline_y: wl,
    });
    (bp, ship.vol, errors)
}

impl Ship<'_> {
    fn x_at(&self, t: f64) -> i32 { ((t * self.len_cells as f64).round() as i32).clamp(0, self.len_cells - 1) }

    fn house(&mut self, h: &House) {
        let x0 = self.x_at(h.aft);
        let x1 = self.x_at(h.fwd);
        let z0 = self.half - h.width as i32 / 2;
        let z1 = z0 + h.width as i32 - 1;
        let mut base = i32::MIN;
        let mut cols = Vec::new();
        for x in x0..=x1 {
            for z in z0..=z1 {
                let Some(top) = self.vol.top(x, z) else { continue };
                cols.push((x, z, top));
                base = base.max(top);
            }
        }
        assert!(!cols.is_empty(), "{}: house {h:?} has no deck under it", self.spec.file);
        let roof = base + h.height as i32 + 1;
        // Clip the house to the columns that have deck under them, then wall its outline.
        let has = |x: i32, z: i32| cols.iter().any(|c| c.0 == x && c.1 == z);
        let m = h.material.unwrap_or(self.spec.hull.material);
        for &(x, z, top) in &cols {
            let edge = !has(x - 1, z) || !has(x + 1, z) || !has(x, z - 1) || !has(x, z + 1);
            for y in top + 1..=roof {
                let wall = edge || y == roof;
                if !wall { continue; }
                let glass = h.bridge && y == roof - 1 && edge && x >= x1 - 2 && (x == x1 || z == z0 || z == z1);
                let paint = if y == roof { PAINT_DECK } else { self.spec.scheme.house };
                self.vol.set(x, y, z, Cell { material: if glass { Material::Glass } else { m }, armour: ArmourTier::None, paint, shape: CellShape::CUBE });
            }
        }
    }

    fn armour(&mut self, a: &Armour) {
        let x0 = self.x_at(a.aft);
        let x1 = self.x_at(a.fwd);
        let [_, ny, nz] = self.vol.n;
        for y in 0..ny as i32 {
            for z in 0..nz as i32 {
                for x in x0..=x1 {
                    let Some(i) = self.vol.idx(x, y, z) else { continue };
                    let c = self.vol.cells[i];
                    if c.is_empty() || c.material == Material::Glass || (!a.houses && !self.vol.hull[i]) { continue; }
                    // Outer plating: a side face open to the sea or the air.
                    let open = [(0, 0, -1), (0, 0, 1)].iter().any(|&(dx, dy, dz)| !self.vol.solid(x + dx, y + dy, z + dz) && self.outside_column(x, y, z + dz));
                    if open && y >= KEEL_ROOM as i32 + 1 { self.vol.cells[i].armour = a.tier; }
                }
            }
        }
    }

    /// True when the cell is beyond the plating along Z (open sea or air beside the hull).
    fn outside_column(&self, x: i32, y: i32, z: i32) -> bool {
        let lo = (0..=z).rev().any(|k| self.vol.solid(x, y, k));
        let hi = (z..self.vol.n[2] as i32).any(|k| self.vol.solid(x, y, k));
        !(lo && hi)
    }

    /// Placed cells of an item at `pos`, per part: (part id, pos, rot, cells, kit part index).
    fn layout(&self, item: Item, pos: CellPos, rot: PartRot) -> Result<(Vec<(PartId, CellPos, PartRot, Vec<CellPos>)>, Vec<SocketBinding>, Option<KitId>), String> {
        match item {
            Item::Kit(id) => {
                let kid = KitId::new(id);
                let def = self.cat.kit(&kid).ok_or_else(|| format!("unknown kit {id}"))?;
                let parts = kit_part_placements(def, self.cat, pos, rot, false).into_iter().map(|p| (p.part, p.pos, p.rot, p.cells)).collect();
                let sockets = kit_sockets_at(def, self.cat, pos, rot, false)
                    .into_iter()
                    .map(|s| { let (dx, dy, dz) = s.face.offset(); SocketBinding { socket: s.name, cell: CellPos::new(s.cell.x + dx, s.cell.y + dy, s.cell.z + dz) } })
                    .collect();
                Ok((parts, sockets, Some(kid)))
            }
            Item::Part(id) => {
                let pid = PartId::new(id);
                let def = self.cat.part(&pid).ok_or_else(|| format!("unknown part {id}"))?;
                let s = rotated_size(def.size, rot);
                let mut cells = Vec::new();
                for y in 0..s[1] as i16 { for z in 0..s[2] as i16 { for x in 0..s[0] as i16 { cells.push(CellPos::new(pos.x + x, pos.y + y, pos.z + z)); } } }
                Ok((vec![(pid, pos, rot, cells)], Vec::new(), None))
            }
        }
    }

    /// The cell the item binds to, relative to `pos`, and the item's box size. Kits bind by their `keel` socket
    /// when hung under the hull, else by their `deck`, `floor` or `edge` socket; single parts by the cell under
    /// (or, hung, over) the middle of their footprint.
    fn anchor(&self, item: Item, rot: PartRot, hung: bool) -> Result<([i32; 3], [i32; 3]), String> {
        let (parts, sockets, _) = self.layout(item, CellPos::new(0, 0, 0), rot)?;
        let mut hi = [0i32; 3];
        for c in parts.iter().flat_map(|p| &p.3) { hi = [hi[0].max(c.x as i32 + 1), hi[1].max(c.y as i32 + 1), hi[2].max(c.z as i32 + 1)]; }
        let names: &[&str] = if hung { &["keel"] } else { &["deck", "floor", "edge"] };
        let a = match sockets.iter().find(|s| names.contains(&s.socket.as_str())) {
            Some(s) => [s.cell.x as i32, s.cell.y as i32, s.cell.z as i32],
            None if hung => [hi[0] / 2, hi[1], hi[2] / 2],
            None => [hi[0] / 2, -1, hi[2] / 2],
        };
        Ok((a, hi))
    }

    fn fit(&mut self, f: &Fit) -> Result<(), String> {
        let rot = PartRot { yaw: f.yaw, tilt: 0 };
        let hung = matches!(f.at, At::Keel(_) | At::Stern(_));
        let (a, size) = self.anchor(f.item, rot, hung)?;
        let (tx, tz) = match f.at {
            At::Top(t) | At::Hold(t) | At::InHouse(t) | At::Keel(t) | At::Edge(t, _) | At::Flush(t) => (self.x_at(t), self.half + f.dz as i32),
            At::Stern(x) => (x as i32 + a[0], self.half + f.dz as i32),
        };
        let mut tried = Vec::new();
        let shifts: Vec<i32> = (0..=12).flat_map(|k| if k == 0 { vec![0] } else { vec![k, -k] }).collect();
        for &dx in &shifts {
            if matches!(f.at, At::Stern(_)) && dx != 0 { break; }
            let x = tx + dx - a[0];
            let z = match f.at {
                At::Edge(_, side) => {
                    // Flush with the deck edge on that side, at the deck row under the item.
                    let xs = x..x + size[0];
                    let mut edge = None::<i32>;
                    for xx in xs {
                        let Some(top) = (0..self.vol.n[2] as i32).filter_map(|zz| self.vol.hull_top(xx, zz)).max() else { continue };
                        let row: Vec<i32> = (0..self.vol.n[2] as i32).filter(|&zz| self.vol.solid(xx, top, zz) && self.vol.hull[self.vol.idx(xx, top, zz).unwrap()]).collect();
                        let e = match side { Side::Starboard => row.iter().max().map(|m| m - size[2] + 1), Side::Port => row.iter().min().copied() };
                        if let Some(e) = e { edge = Some(match (edge, side) { (None, _) => e, (Some(o), Side::Starboard) => o.min(e), (Some(o), Side::Port) => o.max(e) }); }
                    }
                    match edge { Some(e) => e, None => continue }
                }
                _ => tz - a[2],
            };
            let cols: Vec<(i32, i32)> = (x..x + size[0]).flat_map(|xx| (z..z + size[2]).map(move |zz| (xx, zz))).collect();
            let ys: Vec<i32> = match f.at {
                At::Top(_) | At::Edge(..) => {
                    // The deck under the footprint: the highest structure in any of its columns.
                    let Some(deck) = cols.iter().filter_map(|&(xx, zz)| self.vol.top(xx, zz)).max() else { continue };
                    vec![deck - a[1]]
                }
                At::Hold(_) => {
                    // Lowest enclosed air in the socket column, then up a few cells if the hull is too narrow.
                    let Some(low) = (0..self.vol.n[1] as i32).find(|&y| self.env_hull.at(tx + dx, y, tz)) else { continue };
                    (0..8).map(|k| low - 1 + k - a[1]).collect()
                }
                At::InHouse(_) => {
                    let Some(deck) = self.vol.hull_top(tx + dx, tz) else { continue };
                    vec![deck - a[1]]
                }
                At::Keel(_) | At::Stern(_) => {
                    let Some(low) = cols.iter().filter_map(|&(xx, zz)| self.vol.bottom(xx, zz)).min() else { continue };
                    vec![low - a[1]]
                }
                At::Flush(_) => {
                    // The item's top row in the weather deck: the highest hull cell under its footprint.
                    let Some(deck) = cols.iter().filter_map(|&(xx, zz)| self.vol.hull_top(xx, zz)).max() else { continue };
                    vec![deck - (size[1] - 1)]
                }
            };
            for y in ys {
                let pos = CellPos::new(x as i16, y as i16, z as i16);
                match self.try_place(f, pos, rot) {
                    Ok(()) => return Ok(()),
                    Err(e) => tried.push(format!("({x}, {y}, {z}): {e}")),
                }
            }
        }
        Err(format!("no room; tried {}", tried.into_iter().take(4).collect::<Vec<_>>().join("; ")))
    }

    fn try_place(&mut self, f: &Fit, pos: CellPos, rot: PartRot) -> Result<(), String> {
        let (parts, sockets, kit) = self.layout(f.item, pos, rot)?;
        let (env_hull, env_all) = (&self.env_hull, &self.env_all);
        let mut pedestal: Vec<(i32, i32, i32)> = Vec::new();
        // A flush item covers hull plating and its own cells stand under it, so neither counts as structure over it.
        let flush = matches!(f.at, At::Flush(_));
        let own: std::collections::BTreeSet<(i32, i32, i32)> = if flush { parts.iter().flat_map(|p| p.3.iter().map(|c| (c.x as i32, c.y as i32, c.z as i32))).collect() } else { Default::default() };
        for (pid, _, _, cells) in &parts {
            let def = self.cat.part(pid).ok_or_else(|| format!("unknown part {pid}"))?;
            let through = matches!(def.function, PartFunction::Hoist { .. });
            for c in cells {
                let (x, y, z) = (c.x as i32, c.y as i32, c.z as i32);
                let Some(i) = self.vol.idx(x, y, z) else { return Err(format!("{pid} leaves the frame")) };
                if self.vol.part_at(x, y, z) { return Err(format!("{pid} overlaps a part")); }
                let solid = self.vol.solid(x, y, z);
                // A hold item may rise through the weather deck into a house above it (an engine casing): the
                // part takes over those deck cells, as the editor lets a part cover structure.
                let hold = matches!(f.at, At::Hold(_));
                let casing = hold && ((self.vol.hull[i] && Some(y) == self.vol.hull_top(x, z) && env_all.at(x, y + 1, z)) || (env_all.at(x, y, z) && !env_hull.at(x, y, z)));
                let covers = solid && (through || casing || (flush && self.vol.hull[i]));
                if solid && !covers { return Err(format!("{pid} overlaps structure")); }
                match def.placement {
                    Placement::Internal if !(env_all.at(x, y, z) || covers) => return Err(format!("{pid} is not enclosed")),
                    Placement::Weather if ((y + 1)..self.vol.n[1] as i32).any(|yy| self.vol.solid(x, yy, z) && !self.vol.part_at(x, yy, z) && !own.contains(&(x, yy, z))) => return Err(format!("{pid} has structure over it")),
                    _ => {}
                }
                if hold && !(env_hull.at(x, y, z) || casing || covers) { return Err(format!("{pid} is outside the hold")); }
                if flush && !(env_hull.at(x, y, z) || covers) { return Err(format!("{pid} is outside the hull")); }
            }
            // Weather-deck items stand on the deck: fill at most two cells of plinth under any floating cell.
            if matches!(f.at, At::Top(_) | At::Edge(..)) && matches!(def.placement, Placement::Weather | Placement::DeckEdge | Placement::Anywhere) {
                let min_y = cells.iter().map(|c| c.y).min().unwrap_or(0) as i32;
                for c in cells.iter().filter(|c| c.y as i32 == min_y) {
                    let (x, z) = (c.x as i32, c.z as i32);
                    let below = (1..=3).find(|&k| self.vol.solid(x, min_y - k, z) || self.vol.part_at(x, min_y - k, z));
                    match below {
                        Some(1) => {}
                        Some(k) => { for j in 1..k { pedestal.push((x, min_y - j, z)); } }
                        None if parts.len() > 1 => {}
                        None => return Err(format!("{pid} overhangs the deck")),
                    }
                }
            }
        }
        // Kits whose bottom parts sit in the hull (magazines, sonar) are checked per part above; a kit with a deck
        // socket must have the deck under it.
        for s in sockets.iter().filter(|_| !flush) {
            if s.socket == "deck" || s.socket == "edge" {
                if !self.vol.solid(s.cell.x as i32, s.cell.y as i32, s.cell.z as i32) && !pedestal.contains(&(s.cell.x as i32, s.cell.y as i32, s.cell.z as i32)) {
                    return Err(format!("socket {} has no deck", s.socket));
                }
            }
        }
        // Commit.
        let m = self.spec.hull.material;
        // Placing parts never changes structure; only plinths and beds do, and only then can the enclosed air change.
        let mut structure_changed = false;
        for &(x, y, z) in &pedestal {
            if !self.vol.solid(x, y, z) { self.vol.set(x, y, z, Cell { material: m, armour: ArmourTier::None, paint: self.spec.scheme.house, shape: CellShape::CUBE }); structure_changed = true; }
        }
        let kit_slot = kit.as_ref().map(|_| KitSlot(self.kits.len() as u16));
        let mut slots = Vec::new();
        for (pid, ppos, prot, cells) in parts {
            let slot = self.parts.len();
            for c in &cells { let i = self.vol.idx(c.x as i32, c.y as i32, c.z as i32).unwrap(); self.vol.parts[i] = slot as u16 + 1; }
            self.parts.push(PartPlacement { part: pid, pos: ppos, rot: prot, kit: kit_slot, label: None, load: if kit_slot.is_none() { f.load } else { None } });
            slots.push(PartSlot(slot as u16));
        }
        // Hold items stand on beds: fill under floating bottom cells down to structure.
        if matches!(f.at, At::Hold(_)) {
            let placed: Vec<CellPos> = self.parts[slots[0].0 as usize..].iter().flat_map(|p| self.cells_of(p)).collect();
            let min_y = placed.iter().map(|c| c.y).min().unwrap_or(0) as i32;
            for c in placed.iter().filter(|c| c.y as i32 == min_y) {
                let (x, z) = (c.x as i32, c.z as i32);
                let mut y = min_y - 1;
                while y >= 0 && !self.vol.solid(x, y, z) && !self.vol.part_at(x, y, z) {
                    self.vol.set(x, y, z, Cell { material: m, armour: ArmourTier::None, paint: PAINT_DECK, shape: CellShape::CUBE });
                    structure_changed = true;
                    y -= 1;
                }
            }
        }
        if structure_changed { self.env_all = Envelope::of(&self.vol, false); }
        if let Some(kid) = kit {
            let def = self.cat.kit(&kid).unwrap();
            self.kits.push(KitPlacement { kit: kid, version: def.version, pos, rot, mirrored: false, sockets, settings: Default::default(), parts: slots });
        }
        Ok(())
    }

    /// Watertight trunks, as the starter yards build them (`tests/starters`): an open part that took over
    /// structure (an ammunition hoist through the weather deck, under a deck gun) opens its compartment to the
    /// sky, and the simulation counts an open compartment as outside the hull, so the gun over it joined no
    /// network and never fired. Plate every air cell inside the hull next to such a part so its own cells are
    /// the only path through the deck.
    fn trunks(&mut self) {
        let [nx, ny, nz] = self.vol.n;
        let n = nx * ny * nz;
        let watertight = |slot: usize| self.cat.part(&self.parts[slot].part).is_some_and(|d| d.watertight);
        let owner = |i: usize| (self.vol.parts[i] as usize).checked_sub(1);
        // What water and air pass in the simulation: empty cells and the cells of open equipment.
        let passable = |i: usize| match owner(i) { Some(o) => !watertight(o), None => self.vol.cells[i].is_empty() };
        let flood = |through: &dyn Fn(usize) -> bool| -> Vec<bool> {
            let mut seen = vec![false; n];
            let mut stack: Vec<usize> = Vec::new();
            for y in 0..ny { for z in 0..nz { for x in 0..nx {
                if x == 0 || y == 0 || z == 0 || x == nx - 1 || y == ny - 1 || z == nz - 1 {
                    let i = x + nx * (z + nz * y);
                    if through(i) && !seen[i] { seen[i] = true; stack.push(i); }
                }
            }}}
            while let Some(i) = stack.pop() {
                let (x, rest) = (i % nx, i / nx);
                let (z, y) = (rest % nz, rest / nz);
                let mut push = |j: usize| if through(j) && !seen[j] { seen[j] = true; stack.push(j); };
                if x > 0 { push(i - 1); } if x + 1 < nx { push(i + 1); }
                if z > 0 { push(i - nx); } if z + 1 < nz { push(i + nx); }
                if y > 0 { push(i - nx * nz); } if y + 1 < ny { push(i + nx * nz); }
            }
            seen
        };
        let outside = flood(&passable);
        // Open parts over structure that the sky or the sea reaches.
        let mut trunked = vec![false; self.parts.len()];
        for i in 0..n {
            if let Some(o) = owner(i) { if !watertight(o) && !self.vol.cells[i].is_empty() && outside[i] { trunked[o] = true; } }
        }
        if !trunked.iter().any(|t| *t) { return; }
        let beyond = flood(&|i: usize| passable(i) && owner(i).is_none_or(|o| !trunked[o]));
        let mut plate = Vec::new();
        for i in 0..n {
            let Some(o) = owner(i) else { continue };
            if !trunked[o] { continue; }
            let (x, rest) = (i % nx, i / nx);
            let (z, y) = (rest % nz, rest / nz);
            let mut near = Vec::with_capacity(6);
            if x > 0 { near.push(i - 1); } if x + 1 < nx { near.push(i + 1); }
            if z > 0 { near.push(i - nx); } if z + 1 < nz { near.push(i + nx); }
            if y > 0 { near.push(i - nx * nz); } if y + 1 < ny { near.push(i + nx * nz); }
            for j in near {
                if beyond[j] || !self.vol.cells[j].is_empty() { continue; }
                match owner(j) {
                    None => plate.push(j),
                    Some(p) if p == o || watertight(p) || trunked[p] => {}
                    Some(p) => eprintln!("{}: {} touches open part {} inside the hull: no room for its trunk", self.spec.file, self.parts[o].part, self.parts[p].part),
                }
            }
        }
        // Plate the mirror cells too, where they are free, so the trunk adds no list.
        let mirrored: Vec<usize> = plate.iter().filter_map(|&j| {
            let (x, rest) = (j % nx, j / nx);
            let (z, y) = (rest % nz, rest / nz);
            let mz = 2 * self.half - 1 - z as i32;
            if mz < 0 || mz as usize >= nz { return None; }
            let k = x + nx * (mz as usize + nz * y);
            (!beyond[k] && self.vol.cells[k].is_empty() && owner(k).is_none()).then_some(k)
        }).collect();
        let cell = Cell { material: self.spec.hull.material, armour: ArmourTier::None, paint: self.spec.scheme.sides, shape: CellShape::CUBE };
        for j in plate.into_iter().chain(mirrored) { self.vol.cells[j] = cell; }
    }

    fn cells_of(&self, p: &PartPlacement) -> Vec<CellPos> {
        let def = self.cat.part(&p.part).unwrap();
        let s = rotated_size(def.size, p.rot);
        let mut v = Vec::new();
        for y in 0..s[1] as i16 { for z in 0..s[2] as i16 { for x in 0..s[0] as i16 { v.push(CellPos::new(p.pos.x + x, p.pos.y + y, p.pos.z + z)); } } }
        v
    }

    fn blueprint(&self, hull: Hull) -> Blueprint {
        let mut lo = [usize::MAX; 3];
        let mut hi = [0usize; 3];
        let [nx, ny, nz] = self.vol.n;
        for y in 0..ny { for z in 0..nz { for x in 0..nx {
            if !self.vol.cells[x + nx * (z + nz * y)].is_empty() { lo = [lo[0].min(x), lo[1].min(y), lo[2].min(z)]; hi = [hi[0].max(x), hi[1].max(y), hi[2].max(z)]; }
        }}}
        let mut grid = CellGrid::new([(hi[0] + 1) as u16, (hi[1] + 1) as u16, (hi[2] + 1) as u16]);
        for y in 0..=hi[1] { for z in 0..=hi[2] { for x in 0..=hi[0] {
            let c = self.vol.cells[x + nx * (z + nz * y)];
            if !c.is_empty() { grid.set(CellPos::new(x as i16, y as i16, z as i16), c); }
        }}}
        let mut bp = Blueprint {
            header: Header {
                format: content::blueprint::BLUEPRINT_FORMAT,
                game_version: "0.1.0".into(),
                hash: ContentHash::EMPTY,
                kind: BlueprintKind::EnemyClass,
                name: self.spec.name.into(),
                name_key: Some(TextKey::new(self.spec.key)),
                author: None,
                licence: DEFAULT_LICENCE.into(),
                lineage: Vec::new(),
            },
            hull,
            cells: grid.to_rows(),
            paint: default_paint_table(),
            markings: Vec::new(),
            parts: self.parts.clone(),
            kits: self.kits.clone(),
            embedded_kits: Default::default(),
            officers: Default::default(),
            rules: Vec::new(),
            networks: Default::default(),
            graph: Default::default(),
        };
        canonicalize_blueprint(&mut bp);
        bp.header.hash = compute_hash(&bp);
        bp
    }
}

// ---------------------------------------------------------------------------------------------------------
// Floating

/// Hull envelope volume per cell layer (structure plus enclosed air), cubic metres.
fn layers(v: &Vol) -> Vec<(f64, f64, f64, f64, f64, f64, f64)> {
    // Per layer: (volume, sum x, sum z^2 about the centreline, sum x for waterplane... ) kept simple below.
    let env = Envelope::of(v, true);
    let [nx, ny, nz] = v.n;
    let half = nz as f64 * 0.5;
    let mut out = Vec::with_capacity(ny);
    for y in 0..ny {
        let (mut n, mut sx, mut iz, mut sxx) = (0.0, 0.0, 0.0, 0.0);
        for z in 0..nz { for x in 0..nx {
            let i = x + nx * (z + nz * y);
            if v.hull[i] || env.inside[i] {
                n += 1.0;
                sx += (x as f64 + 0.5) * CELL_M;
                sxx += ((x as f64 + 0.5) * CELL_M) * ((x as f64 + 0.5) * CELL_M);
                let dz = (z as f64 + 0.5 - half) * CELL_M;
                iz += dz * dz;
            }
        }}
        let vol = n * CELL_M * CELL_M * CELL_M;
        let area = CELL_M * CELL_M;
        out.push((vol, sx * CELL_M * CELL_M * CELL_M, iz * area, (y as f64 + 0.5) * CELL_M * vol, n * area, sx * area, sxx * area));
    }
    out
}

fn power(bp: &Blueprint, cat: &Catalogue) -> (f64, f64, f64) {
    let (mut supply, mut load, mut shaft) = (0.0, 0.0, 0.0);
    for p in &bp.parts {
        let Some(d) = cat.part(&p.part) else { continue };
        for q in &d.ports {
            if q.network != NetworkKind::Power { continue; }
            match q.role { PortRole::Supply => supply += q.rate, PortRole::Consume => load += q.rate, _ => {} }
        }
        if let PartFunction::Engine(e) = &d.function { shaft += e.shaft_kw; }
    }
    (supply, load, shaft)
}

/// Least metacentric height a class may sail with: 5% of the beam, at least 0.3 m.
pub fn gm_floor(spec: &ShipSpec) -> f64 { (0.05 * spec.hull.beam).max(0.3) }
/// Largest static trim a class may sail with, degrees.
pub const TRIM_LIMIT_DEG: f64 = 0.5;
/// Kilograms one ballast tank holds.
const BALLAST_KG: f64 = 4000.0;
/// Largest transverse offset of the centre of mass from the centreline a class may sail with, metres. A
/// centred item of odd width lands half a cell to starboard on an even-width hull; left there, the heavy ones
/// list a small warship several degrees on sim-core's hydrostatics. The Act 2 to 4 sheets trim that out by
/// hand (`.dz(-1)`) to this bar; [`build`] does the same for a sheet that misses it.
pub const LIST_LIMIT_M: f64 = 0.015;

/// Transverse offset of the centre of mass from the hull's centreline, metres (positive: to starboard).
pub fn list_offset_m(bp: &Blueprint, stats: &BlueprintStats) -> f64 {
    let (z0, z1) = bp.cells.0.iter().fold((i16::MAX, i16::MIN), |(a, z), row| (a.min(row.z), z.max(row.z)));
    stats.com_m[2] - (z0 as f64 + z1 as f64 + 1.0) * 0.5 * CELL_M
}

/// Build a class and trim it as a designer would: weigh it, then pump trim ballast into tanks at the light end
/// until the static trim is under [`TRIM_LIMIT_DEG`], and into tanks low amidships while the metacentric height
/// is under [`gm_floor`]. Candidate tank stations are tried in order until one has room. The final pass paints
/// the boot-topping band at the waterline the trimmed ship floats to.
///
/// A ship whose centre of mass still sits more than [`LIST_LIMIT_M`] off the centreline has its list trimmed
/// out the way the Act 2 sheets do it by hand: centred items move one cell to port or starboard, heaviest
/// correction first, while each move brings the centre of mass closer to the centreline. A sheet that already
/// meets the bar builds exactly as written.
pub fn build(spec: &ShipSpec, cat: &Catalogue) -> Built {
    let (mut best, extra) = build_with(spec, cat, spec.fits);
    let mut off = list_offset_m(&best.blueprint, &best.report.stats).abs();
    if off <= LIST_LIMIT_M { return best; }
    let (mut fits, mut extra) = (spec.fits.to_vec(), extra);
    // Ballast follows the outfit, so trim against the last build's tanks and rebuild, a few rounds at most.
    for _ in 0..3 {
        fits = trim_list(spec, cat, &fits, &extra);
        let (b, e) = build_with(spec, cat, &fits);
        let o = list_offset_m(&b.blueprint, &b.report.stats).abs();
        if o >= off { break; }
        (best, off, extra) = (b, o, e);
        if off <= LIST_LIMIT_M { break; }
    }
    best
}

/// `fits` with centred items shifted one cell sideways to cancel the list, measured with the ballast `extra`
/// in place: each candidate shift is built once and its change of transverse moment kept.
fn trim_list(spec: &ShipSpec, cat: &Catalogue, fits: &[Fit], extra: &[Fit]) -> Vec<Fit> {
    let moment = |fits: &[Fit]| -> Option<f64> {
        let (bp, _, errors) = build_at(spec, cat, None, fits, extra);
        if !errors.is_empty() { return None; }
        let st = compute_stats(&bp, cat);
        Some(list_offset_m(&bp, &st) * st.tonnage_t)
    };
    let mut fits = fits.to_vec();
    let Some(mut m) = moment(&fits) else { return fits };
    let mut moves: Vec<(usize, i16, f64)> = Vec::new();
    for i in 0..fits.len() {
        if fits[i].dz != 0 || matches!(fits[i].at, At::Edge(..) | At::Stern(_)) || spec.fits.get(i).is_some_and(|f| f.dz != 0) { continue; }
        for d in [-1i16, 1] {
            let mut trial = fits.clone();
            trial[i].dz = d;
            if let Some(mi) = moment(&trial) { moves.push((i, d, mi - m)); }
        }
    }
    // Largest corrections first; ties by fit order, then port before starboard.
    moves.sort_by(|a, b| b.2.abs().total_cmp(&a.2.abs()).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
    let mut moved = vec![false; fits.len()];
    for (i, d, dm) in moves {
        if moved[i] || (m + dm).abs() >= m.abs() - 1e-6 { continue; }
        let mut trial = fits.clone();
        trial[i].dz = d;
        // Moves interact through placement (a shifted item can push a later one); keep the measured result.
        let Some(mt) = moment(&trial) else { continue };
        if mt.abs() < m.abs() { fits = trial; m = mt; moved[i] = true; }
    }
    fits
}

fn build_with(spec: &ShipSpec, cat: &Catalogue, fits: &[Fit]) -> (Built, Vec<Fit>) {
    let mut extra: Vec<Fit> = Vec::new();
    // Candidate stations, each tried on the centreline and either side.
    let sides = |ts: Vec<f64>| -> Vec<(f64, i16)> { ts.into_iter().flat_map(|t| [(t, 0), (t, -4), (t, 4)]).collect() };
    let aft = sides((0..10).map(|k| 0.05 + 0.04 * k as f64).collect());
    let fwd = sides((0..10).map(|k| 0.82 - 0.04 * k as f64).collect());
    let mid = sides((0..12).map(|k| 0.48 + 0.04 * if k % 2 == 0 { k as f64 / 2.0 } else { -(k as f64 + 1.0) / 2.0 }).collect());
    let (mut ia, mut iff, mut im) = (0usize, 0usize, 0usize);
    let mut gm_tanks;
    for _ in 0..120 {
        let (bp, vol, errors) = build_at(spec, cat, None, fits, &extra);
        if !errors.is_empty() {
            // The last tank found no room; drop it and try the next station.
            extra.pop();
            continue;
        }
        let r = report(spec, cat, &bp, &vol, errors);
        let w = r.stats.tonnage_t * 1000.0;
        let lcb = r.stats.com_m[0] - r.trim_lever * spec.hull.length;
        if r.trim_deg.abs() > TRIM_LIMIT_DEG {
            let t = if r.trim_lever > 0.0 { ia += 1; aft.get(ia - 1) } else { iff += 1; fwd.get(iff - 1) };
            let Some(&(t, dz)) = t else { break };
            let x_b = t * spec.hull.length;
            let m = (w * (r.stats.com_m[0] - lcb) / (lcb - x_b)).abs();
            let kg = (m.min(BALLAST_KG) / 50.0).round() * 50.0;
            if kg > 0.0 { extra.push(Fit { load: Some(kg), ..part("ballast.tank", At::Hold(t)).dz(dz) }); continue; }
        }
        gm_tanks = extra.iter().filter(|f| f.load == Some(BALLAST_KG)).count();
        if r.gm_m < gm_floor(spec) && gm_tanks < 4 {
            im += 1;
            let Some(&(t, dz)) = mid.get(im - 1) else { break };
            extra.push(Fit { load: Some(BALLAST_KG), ..part("ballast.tank", At::Hold(t)).dz(dz) });
            continue;
        }
        break;
    }
    // Drop a trailing tank that never fitted.
    if !build_at(spec, cat, None, fits, &extra).2.is_empty() { extra.pop(); }
    let (bp0, vol0, _) = build_at(spec, cat, None, fits, &extra);
    let t0 = compute_stats(&bp0, cat).tonnage_t;
    let row = (waterline_cells(&vol0, t0) + 0.5).floor() as i16;
    let (bp, vol, errors) = build_at(spec, cat, Some(row), fits, &extra);
    let report = report(spec, cat, &bp, &vol, errors);
    (Built { blueprint: bp, report }, extra)
}

fn report(spec: &ShipSpec, cat: &Catalogue, bp: &Blueprint, vol: &Vol, errors: Vec<String>) -> Report {
    let stats = compute_stats(bp, cat);
    let lay = layers(vol);
    let h = waterline_cells(vol, stats.tonnage_t);
    // Integrate the submerged envelope up to h.
    let (mut v, mut mx, mut my, mut it, mut il) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (y, l) in lay.iter().enumerate() {
        let f = (h - y as f64).clamp(0.0, 1.0);
        if f <= 0.0 { break; }
        v += l.0 * f;
        mx += l.1 * f;
        let yc = (y as f64 + 0.5 * f) * CELL_M;
        my += yc * l.0 * f;
        if (y as f64) <= h && h < y as f64 + 1.0 {
            it = l.2;
            // Longitudinal second moment of the waterplane about its centre of flotation.
            let xf = if l.4 > 0.0 { l.5 / l.4 } else { 0.0 };
            il = l.6 - l.4 * xf * xf;
        }
    }
    let kb = my / v;
    let bm = it / v;
    let kg = stats.com_m[1];
    let lcb = mx / v;
    let gml = kb + il / v - kg;
    let keel = (0..vol.n[1]).find(|&y| lay[y].0 > 0.0).unwrap_or(0) as f64;
    let mid = vol.n[0] as i32 / 2;
    let deck = (0..vol.n[2] as i32).filter_map(|z| vol.hull_top(mid, z)).max().unwrap_or(0) as f64 + 1.0;
    let full: f64 = lay.iter().map(|l| l.0).sum();
    let (supply, load, shaft) = power(bp, cat);
    let lowest = (0..vol.n[1]).find(|&y| (0..vol.n[0] * vol.n[2]).any(|k| { let i = k + vol.n[0] * vol.n[2] * y; !vol.cells[i].is_empty() || vol.parts[i] != 0 })).unwrap_or(0) as f64;
    Report {
        draft_m: (h - keel) * CELL_M,
        draft_max_m: (h - lowest) * CELL_M,
        displacement_at_deck_t: full * SEA_T_PER_M3,
        freeboard_m: (deck - h) * CELL_M,
        gm_m: kb + bm - kg,
        trim_lever: (stats.com_m[0] - lcb) / spec.hull.length,
        trim_deg: if gml > 0.0 { libm::atan((stats.com_m[0] - lcb) / gml) * (180.0 / core::f64::consts::PI) } else { 90.0 },
        power_kw: supply,
        load_kw: load,
        shaft_kw: shaft,
        stats,
        errors,
    }
}

/// Waterline height in cells above the grid floor at which the hull envelope displaces `tonnage_t`.
fn waterline_cells(v: &Vol, tonnage_t: f64) -> f64 {
    let need = tonnage_t / SEA_T_PER_M3;
    let mut acc = 0.0;
    for (y, l) in layers(v).iter().enumerate() {
        if l.0 > 0.0 && acc + l.0 >= need { return y as f64 + (need - acc) / l.0; }
        acc += l.0;
    }
    v.n[1] as f64
}

/// ASCII cross-sections of the bare hull at the given stations (debugging the build sheets).
pub fn sections(spec: &ShipSpec, ts: &[f64]) -> String {
    let shell = shell(&spec.hull);
    let lay = ShellField::new(&shell).expect("valid shell").layout();
    let hull = Hull { source: HullSource::Lofted(shell), planes: planes(spec, &shell_of(spec)), design_waterline_y: lay.waterline_y };
    let g = voxelize(&hull).grid;
    let mut s = format!("{} grid {:?} waterline row {}\n", spec.file, g.size, lay.waterline_y);
    for &t in ts {
        let x = ((t * g.size[0] as f64).round() as i16).clamp(0, g.size[0] as i16 - 1);
        s += &format!("t {t} x {x}\n");
        for y in (0..g.size[1] as i16).rev() {
            let row: String = (0..g.size[2] as i16).map(|z| if g.get(CellPos::new(x, y, z)).is_empty() { '.' } else { '#' }).collect();
            s += &format!("{y:3} {row}\n");
        }
    }
    s
}
fn shell_of(spec: &ShipSpec) -> HullShell { shell(&spec.hull) }
