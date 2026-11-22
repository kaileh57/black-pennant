//! Unit `gd-editor-hull`. Hull editing on `BpEditor`: lofted shell handles, decks and bulkheads, freeform
//! sculpting after a bake, armour, paint and markings. DESIGN.md L122-134 (hull and scale: 4 to 16 stations,
//! handles beam, draft, deadrise, flare, sheer, bow rake, transom, each with a numeric field; 10 to 90 m for
//! players, 150 m in enemy-class mode; 0.5 m cells with wedge, corner and inverse-corner shapes; armour tiers
//! 5, 15, 40, 80 mm; 16-entry paint table with clamps; decks 2.5 m apart; one-way bake), L1801-1803 (paint).
//! Voxelizing and clamping are `content::hull` and `content::blueprint::io`; this unit edits the values.
//!
//! # How edits regenerate cells
//! A handle or length edit goes through `sim_systems::editor::notes::apply_edit` (`SetHullHandle`), the same
//! path a Ship's Note Fix takes. Station and plane edits have no `BlueprintEdit`, so [`regenerate`] here does the
//! same diff: the old voxelization is compared with the stored cells, and the player's own work (cells added
//! on top, cells cut away, armour, repainted swatches) carries over to the new shell, shifted with the frame
//! when the shell's layout moves, together with parts, kits, markings and network edits.
//!
//! # Choices the spec leaves open
//! - Handle ids are the ones `BlueprintEdit::SetHullHandle` uses: `transom` is the transom rake in degrees and
//!   `transom_width` (also listed, as a ninth numeric field) the transom's share of the aftmost beam.
//!   `HullDict.transom` is the rake; `HullDict.transom_width` is added beside it.
//! - A handle's `min` and `max` are the furthest values the shell stays valid at with every other parameter as
//!   it is (`content::hull::validate_shell` has cross-parameter limits), so a field never offers a value the
//!   hull would refuse; `set_handle` clamps to them.
//! - Mirrored edits: on a lofted hull `HullShell::mirrored` mirrors sculpting, armour, paint and markings across
//!   the centreline; a baked hull follows the dock's mirror symmetry.
//! - Before a bake, sculpting adds superstructure anywhere but removes only cells the shell did not generate
//!   (the shell's own cells change through its handles); after the bake every cell is the player's.
//! - Interior decks are painted the deck swatch, bulkheads the side swatch. Armour is painted on outer cells
//!   only (cells with a face open to the outside).
//! - `compartments()` reads the design the worker last built for this revision; it is empty until then.
use super::BpEditor;
use crate::convert::{d, msg_dict};
use crate::enums::GdEnum;
use content::blueprint::{Blueprint, Marking, MarkingKind, PaintEntry, PaintUse};
use content::cell::{ArmourTier, Cell, CellGrid, CellPos, CellShape, Face, Material, ShapeKind, CELL_M, MAX_GRID_DIM};
use content::enums::Axis;
use content::hull::{Hull, HullShell, HullSource, InternalPlane, ParamRange, Station};
use godot::prelude::*;
use sim_systems::protocol::{Msg, MsgArg};
use std::collections::{BTreeMap, BTreeSet};

/// Text keys this unit emits.
pub(crate) const HULL_KEYS: &[&str] = &[
    "dock.undo.handle", "dock.undo.length", "dock.undo.station_add", "dock.undo.station_remove", "dock.undo.mirror",
    "dock.undo.plane_add", "dock.undo.plane_move", "dock.undo.plane_remove", "dock.undo.bake", "dock.undo.sculpt",
    "dock.undo.armour", "dock.undo.paint", "dock.undo.palette", "dock.undo.marking_add", "dock.undo.marking_remove",
    "dock.compartment",
];

/// Most cells one sculpt, armour or paint call changes (a 90 m hull has about 100,000).
const MAX_BRUSH_CELLS: usize = 1 << 18;
/// Most markings on one design, and the longest marking text.
const MAX_MARKINGS: usize = 256;
const MAX_MARKING_TEXT: usize = 16;
const MAX_MARKING_CELLS: i64 = 64;
/// Stations closer than this share of the hull cannot both exist (they would fold the loft).
const MIN_STATION_GAP_T: f64 = 0.02;
/// Bisection steps when finding a handle's valid range.
const RANGE_STEPS: usize = 40;

/// Per-revision caches.
#[derive(Default)]
pub(crate) struct HullCache {
    handles: std::cell::RefCell<Option<(u64, Vec<HandleInfo>)>>,
    exterior: std::cell::RefCell<Option<(u64, std::rc::Rc<Exterior>)>>,
}
impl HullCache {
    pub(crate) fn invalidate(&mut self) {
        *self.handles.get_mut() = None;
        *self.exterior.get_mut() = None;
    }
}

/// One numeric field and drag handle.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HandleInfo { pub id: &'static str, pub station: i64, pub value: f64, pub min: f64, pub max: f64, pub step: f64, pub unit: &'static str, pub pos: [f64; 3] }

/// The station handles in field order, with their ranges, steps and units.
const STATION_HANDLES: [(&str, ParamRange, f64, &str); 5] = [
    ("beam", content::hull::STATION_BEAM_M, 0.1, "m"),
    ("draft", content::hull::DRAFT_M, 0.1, "m"),
    ("deadrise", content::hull::DEADRISE_DEG, 1.0, "deg"),
    ("flare", content::hull::FLARE_DEG, 1.0, "deg"),
    ("sheer", content::hull::SHEER_M, 0.1, "m"),
];
const HULL_HANDLES: [(&str, ParamRange, f64, &str); 3] = [
    ("bow_rake", content::hull::BOW_RAKE_DEG, 1.0, "deg"),
    ("transom", content::hull::TRANSOM_RAKE_DEG, 1.0, "deg"),
    ("transom_width", content::hull::TRANSOM_WIDTH, 0.01, "none"),
];

fn get_handle(shell: &HullShell, id: &str, station: usize) -> Option<f64> {
    match id {
        "bow_rake" => Some(shell.bow_rake_deg),
        "transom" => Some(shell.transom_rake_deg),
        "transom_width" => Some(shell.transom_width),
        "length" => Some(shell.length_m),
        _ => {
            let s = shell.stations.get(station)?;
            match id { "beam" => Some(s.beam_m), "draft" => Some(s.draft_m), "deadrise" => Some(s.deadrise_deg), "flare" => Some(s.flare_deg), "sheer" => Some(s.sheer_m), _ => None }
        }
    }
}
fn put_handle(shell: &mut HullShell, id: &str, station: usize, v: f64) -> bool {
    match id {
        "bow_rake" => shell.bow_rake_deg = v,
        "transom" => shell.transom_rake_deg = v,
        "transom_width" => shell.transom_width = v,
        "length" => shell.length_m = v,
        _ => {
            let Some(s) = shell.stations.get_mut(station) else { return false };
            match id { "beam" => s.beam_m = v, "draft" => s.draft_m = v, "deadrise" => s.deadrise_deg = v, "flare" => s.flare_deg = v, "sheer" => s.sheer_m = v, _ => return false }
        }
    }
    true
}
fn handle_range(id: &str) -> Option<ParamRange> {
    if id == "length" { return Some(ParamRange::new(content::cell::MIN_PLAYER_HULL_M, content::cell::MAX_ENEMY_HULL_M)); }
    STATION_HANDLES.iter().chain(HULL_HANDLES.iter()).find(|(h, ..)| *h == id).map(|(_, r, ..)| *r)
}

/// The furthest values from the current one, inside the parameter's own range, at which the shell stays valid
/// with every other parameter unchanged. A shell that is already invalid gets the plain range.
pub(crate) fn valid_range(shell: &HullShell, id: &str, station: usize, max_len: f64) -> Option<(f64, f64)> {
    let range = handle_range(id)?;
    let (lo_lim, hi_lim) = if id == "length" { (range.min, max_len.min(range.max)) } else { (range.min, range.max) };
    let cur = get_handle(shell, id, station)?;
    let ok = |v: f64| { let mut s = shell.clone(); put_handle(&mut s, id, station, v) && content::hull::validate_shell(&s, max_len).is_ok() };
    if !cur.is_finite() || !ok(cur) { return Some((lo_lim, hi_lim)); }
    let reach = |limit: f64| -> f64 {
        if ok(limit) { return limit; }
        let (mut good, mut bad) = (cur, limit);
        for _ in 0..RANGE_STEPS { let mid = 0.5 * (good + bad); if ok(mid) { good = mid } else { bad = mid } }
        good
    };
    Some((reach(lo_lim), reach(hi_lim)))
}

/// Fore and aft ends of the hull at the design waterline, design-frame metres along x.
fn waterline_ends(field: &content::hull::ShellField, shell: &HullShell) -> (f64, f64) {
    let y = -0.01;
    let l = shell.length_m;
    let mut first = None;
    let mut last = None;
    let n = ((l / 0.25).ceil() as usize).clamp(4, 2_000);
    for i in 0..=n {
        let x = l * i as f64 / n as f64;
        if field.sdf_design([x, y, 0.0]) <= 0.0 {
            first.get_or_insert(x);
            last = Some(x);
        }
    }
    (first.unwrap_or(0.0), last.unwrap_or(l))
}

/// Where the shell surface crosses the segment from `out` (outside the hull) to `inside`, along the one axis
/// they differ on (design-frame metres); the inside end's coordinate when the segment never crosses.
fn surface(field: &content::hull::ShellField, out: [f64; 3], inside: [f64; 3]) -> f64 {
    let axis = (0..3).find(|&k| out[k] != inside[k]).unwrap_or(1);
    if field.sdf_design(out) <= 0.0 || field.sdf_design(inside) > 0.0 { return inside[axis]; }
    let (mut a, mut b) = (out, inside);
    for _ in 0..24 {
        let mut mid = a;
        mid[axis] = 0.5 * (a[axis] + b[axis]);
        if field.sdf_design(mid) > 0.0 { a = mid } else { b = mid }
    }
    b[axis]
}

/// Every handle of a lofted shell with its valid range and gizmo position (editor frame, metres).
pub(crate) fn shell_handles(shell: &HullShell, max_len: f64) -> Vec<HandleInfo> {
    let Some(field) = content::hull::ShellField::new(shell) else { return Vec::new() };
    let layout = field.layout();
    let (xa, xb) = waterline_ends(&field, shell);
    let to_v = |d: [f64; 3]| layout.to_vessel(d);
    let mut out = Vec::new();
    for (i, st) in shell.stations.iter().enumerate() {
        let x = xa + st.t * (xb - xa);
        let half = 0.5 * st.beam_m;
        for (id, _, step, unit) in STATION_HANDLES {
            let Some(value) = get_handle(shell, id, i) else { continue };
            let Some((min, max)) = valid_range(shell, id, i, max_len) else { continue };
            let pos = match id {
                "beam" => [x, st.sheer_m, half],
                "draft" => [x, -st.draft_m, 0.0],
                // On the surface itself: the bottom halfway out, the topside halfway up.
                "deadrise" => [x, surface(&field, [x, -st.draft_m - 1.0, 0.5 * half], [x, st.sheer_m, 0.5 * half]), 0.5 * half],
                "flare" => [x, 0.5 * st.sheer_m, surface(&field, [x, 0.5 * st.sheer_m, half + 0.5 * st.sheer_m + 1.0], [x, 0.5 * st.sheer_m, 0.0])],
                _ => [x, st.sheer_m, 0.0],
            };
            out.push(HandleInfo { id, station: i as i64, value, min, max, step, unit, pos: to_v(pos) });
        }
    }
    let (first, last) = (shell.stations.first(), shell.stations.last());
    for (id, _, step, unit) in HULL_HANDLES {
        let Some(value) = get_handle(shell, id, 0) else { continue };
        let Some((min, max)) = valid_range(shell, id, 0, max_len) else { continue };
        let pos = match id {
            "bow_rake" => [xb, last.map_or(0.0, |s| s.sheer_m), 0.0],
            "transom" => [xa, 0.0, 0.0],
            _ => [xa, first.map_or(0.0, |s| s.sheer_m), 0.5 * shell.transom_width * first.map_or(0.0, |s| s.beam_m)],
        };
        out.push(HandleInfo { id, station: -1, value, min, max, step, unit, pos: to_v(pos) });
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Cells.

fn in_frame(c: [i32; 3]) -> bool { c.iter().all(|v| (0..MAX_GRID_DIM as i32).contains(v)) }
fn pos3(c: [i32; 3]) -> CellPos { CellPos::new(c[0] as i16, c[1] as i16, c[2] as i16) }

/// The blueprint's structure as a dense grid, when every stored run lies inside the frame.
pub(crate) fn rows_grid(bp: &Blueprint) -> Option<CellGrid> {
    for r in &bp.cells.0 {
        if r.y < 0 || r.z < 0 || r.y as u16 >= MAX_GRID_DIM || r.z as u16 >= MAX_GRID_DIM { return None; }
        for run in &r.runs { if run.x < 0 || run.x as i32 + run.len as i32 > MAX_GRID_DIM as i32 { return None; } }
    }
    Some(CellGrid::from_rows(&bp.cells))
}

/// Cells as a sparse map, for edits that may grow the grid.
fn cell_map(grid: &CellGrid) -> BTreeMap<CellPos, Cell> {
    let mut out = BTreeMap::new();
    let [sx, sy, sz] = grid.size;
    for y in 0..sy as i16 { for z in 0..sz as i16 { for x in 0..sx as i16 {
        let p = CellPos::new(x, y, z);
        let c = grid.get(p);
        if !c.is_empty() { out.insert(p, c); }
    }}}
    out
}
fn map_grid(map: &BTreeMap<CellPos, Cell>) -> CellGrid {
    let size = map.keys().fold([0u16; 3], |s, p| [s[0].max(p.x as u16 + 1), s[1].max(p.y as u16 + 1), s[2].max(p.z as u16 + 1)]);
    let mut g = CellGrid::new(size);
    for (p, c) in map { g.set(*p, *c); }
    g
}

/// Regenerate a lofted hull's cells for a changed shell or plane list, keeping the player's own work (see the
/// module doc). The same rule as `sim_systems::editor::notes::apply_edit` uses for handle edits. `None` when
/// the new shell has no cells, the stored cells lie outside the frame, or a part would leave the frame.
pub(crate) fn regenerate(bp: &Blueprint, mut hull: Hull, cat: &content::catalogue::Catalogue) -> Option<Blueprint> {
    let (HullSource::Lofted(old_shell), HullSource::Lofted(new_shell)) = (&bp.hull.source, &hull.source) else { return None };
    let (lo, ln) = (content::hull::shell_layout(old_shell), content::hull::shell_layout(new_shell));
    if ln.size.contains(&0) { return None; }
    let d = [lo.x_offset as i32 - ln.x_offset as i32, ln.waterline_y as i32 - lo.waterline_y as i32, ln.half_width_cells as i32 - lo.half_width_cells as i32];
    let shift = |p: CellPos| -> Option<CellPos> { let q = [p.x as i32 + d[0], p.y as i32 + d[1], p.z as i32 + d[2]]; in_frame(q).then(|| pos3(q)) };
    for p in &mut hull.planes {
        p.offset = (p.offset as i32 + match p.axis { Axis::X => d[0], Axis::Y => d[1], Axis::Z => d[2] }).clamp(0, MAX_GRID_DIM as i32 - 1) as i16;
    }
    hull.design_waterline_y = (hull.design_waterline_y as i32 + d[1]).clamp(0, MAX_GRID_DIM as i32 - 1) as i16;
    let old_vox = content::hull::voxelize(&bp.hull).grid;
    let new_vox = content::hull::voxelize(&hull);
    if new_vox.grid.cells.is_empty() { return None; }
    let stored = rows_grid(bp)?;
    let mut out = cell_map(&new_vox.grid);
    let generated = [content::hull::PAINT_SIDES, content::hull::PAINT_DECK, content::hull::PAINT_BOOT_TOPPING, content::hull::PAINT_ANTIFOULING];
    let size = [stored.size[0].max(old_vox.size[0]), stored.size[1].max(old_vox.size[1]), stored.size[2].max(old_vox.size[2])];
    for y in 0..size[1] as i16 { for z in 0..size[2] as i16 { for x in 0..size[0] as i16 {
        let p = CellPos::new(x, y, z);
        let (was, now) = (old_vox.get(p), stored.get(p));
        if was.is_empty() && now.is_empty() { continue; }
        let Some(q) = shift(p) else { continue };
        match (was.is_empty(), now.is_empty()) {
            (true, false) => { out.insert(q, now); }
            (false, true) => { out.remove(&q); }
            (false, false) => {
                if let Some(c) = out.get_mut(&q) {
                    c.armour = now.armour;
                    if !generated.contains(&now.paint) { c.paint = now.paint; }
                    if now.material != was.material { c.material = now.material; }
                }
            }
            (true, true) => {}
        }
    }}}
    let mut next = bp.clone();
    if d != [0, 0, 0] {
        for p in &mut next.parts { p.pos = shift(p.pos)?; }
        for k in &mut next.kits {
            k.pos = shift(k.pos)?;
            for s in &mut k.sockets { s.cell = shift(s.cell)?; }
        }
        for m in &mut next.markings { m.cell = shift(m.cell)?; }
        for r in &mut next.networks.pinned_routes { for c in &mut r.cells { *c = shift(*c)?; } }
        for b in &mut next.networks.breakers { b.cell = shift(b.cell)?; }
        for p in &next.parts {
            let def = cat.part(&p.part)?;
            super::parts::footprint_in_frame(def, p.pos, p.rot)?;
        }
    }
    hull.design_waterline_y = new_vox.design_waterline_y;
    next.hull = hull;
    next.cells = map_grid(&out).to_rows();
    Some(next)
}

/// The design with its waterline row moved to `row`: the boot-topping band and antifouling follow (L130).
/// A lofted hull regenerates; a baked hull repaints the outer cells that carry a generated hull swatch.
pub(crate) fn with_waterline(bp: &Blueprint, row: i16, cat: &content::catalogue::Catalogue) -> Option<Blueprint> {
    if row < 0 || row as u16 >= MAX_GRID_DIM { return None; }
    match &bp.hull.source {
        HullSource::Lofted(_) => {
            let mut hull = bp.hull.clone();
            hull.design_waterline_y = row;
            regenerate(bp, hull, cat)
        }
        HullSource::Freeform => {
            let mut grid = rows_grid(bp)?;
            let ext = Exterior::of(&grid);
            let (sides, band, anti) = (content::hull::PAINT_SIDES, content::hull::PAINT_BOOT_TOPPING, content::hull::PAINT_ANTIFOULING);
            let [sx, sy, sz] = grid.size;
            for y in 0..sy as i16 { for z in 0..sz as i16 { for x in 0..sx as i16 {
                let p = CellPos::new(x, y, z);
                let mut c = grid.get(p);
                if c.is_empty() || c.material == Material::PartBody || c.material == Material::Glass || !ext.outer(p) { continue; }
                if ![sides, band, anti].contains(&c.paint) { continue; }
                let want = if y == row { band } else if y < row { anti } else { sides };
                if c.paint != want { c.paint = want; grid.set(p, c); }
            }}}
            let mut next = bp.clone();
            next.cells = grid.to_rows();
            next.hull.design_waterline_y = row;
            Some(next)
        }
    }
}

/// Outside air reached from beyond the grid through empty cells (shaped cells count as solid, as in the
/// simulation): which solid cells have a face on it.
pub(crate) struct Exterior { size: [usize; 3], outside: Vec<bool> }
impl Exterior {
    pub(crate) fn of(grid: &CellGrid) -> Exterior {
        let s = [grid.size[0] as usize, grid.size[1] as usize, grid.size[2] as usize];
        let p = [s[0] + 2, s[1] + 2, s[2] + 2];
        let idx = |x: usize, y: usize, z: usize| x + p[0] * (z + p[2] * y);
        let mut outside = vec![false; p[0] * p[1] * p[2]];
        let mut stack = vec![[0usize; 3]];
        outside[0] = true;
        while let Some(c) = stack.pop() {
            for (axis, dlt) in [(0, -1i64), (0, 1), (1, -1), (1, 1), (2, -1), (2, 1)] {
                let v = c[axis] as i64 + dlt;
                if v < 0 || v as usize >= p[axis] { continue; }
                let mut n = c;
                n[axis] = v as usize;
                let i = idx(n[0], n[1], n[2]);
                if outside[i] { continue; }
                let inner = n.iter().zip(s).all(|(&v, s)| v >= 1 && v <= s);
                if inner && !grid.get(CellPos::new(n[0] as i16 - 1, n[1] as i16 - 1, n[2] as i16 - 1)).is_empty() { continue; }
                outside[i] = true;
                stack.push(n);
            }
        }
        Exterior { size: s, outside }
    }
    fn outside_at(&self, c: [i32; 3]) -> bool {
        if c.iter().zip(self.size).any(|(&v, s)| v < 0 || v as usize >= s) { return true; }
        let p = [self.size[0] + 2, self.size[2] + 2];
        self.outside[(c[0] as usize + 1) + p[0] * ((c[2] as usize + 1) + p[1] * (c[1] as usize + 1))]
    }
    /// A cell with a face on outside air.
    pub(crate) fn outer(&self, c: CellPos) -> bool {
        Face::ALL.iter().any(|f| { let (dx, dy, dz) = f.offset(); self.outside_at([c.x as i32 + dx as i32, c.y as i32 + dy as i32, c.z as i32 + dz as i32]) })
    }
}

/// A brush's cells: the requested ones inside the frame, and their mirror images when edits mirror. The flag
/// marks a mirror image (a requested cell wins where both meet).
fn brush(cells: &PackedVector3Array, mirror: Option<i32>) -> Vec<(CellPos, bool)> {
    let mut out: BTreeMap<CellPos, bool> = BTreeMap::new();
    for v in cells.as_slice().iter().take(MAX_BRUSH_CELLS) {
        if !(v.x.is_finite() && v.y.is_finite() && v.z.is_finite()) { continue; }
        let c = [v.x.round() as i32, v.y.round() as i32, v.z.round() as i32];
        if !in_frame(c) { continue; }
        out.insert(pos3(c), false);
        if let Some(s) = mirror {
            let m = [c[0], c[1], s - c[2]];
            if in_frame(m) { out.entry(pos3(m)).or_insert(true); }
        }
    }
    out.into_iter().collect()
}

/// The shape byte for `shape` 0..4 and orientation 0..23 (`None` for anything else). Shape 4 is a 1:2 slope;
/// its upper half goes in the named cell.
fn shape_of(shape: i64, orientation: i64) -> Option<CellShape> {
    let o = u8::try_from(orientation).ok().filter(|o| (*o as usize) < content::cell::ORIENTATION_COUNT as usize)?;
    let kind = match shape { 0 => ShapeKind::Cube, 1 => ShapeKind::Wedge, 2 => ShapeKind::Corner, 3 => ShapeKind::InverseCorner, 4 => ShapeKind::SlopeHigh, _ => return None };
    Some(CellShape::new(kind, if kind == ShapeKind::Cube { 0 } else { o }))
}

/// The neighbour that holds a 1:2 slope's lower half: one cell along the orientation's image of +X.
fn slope_low_cell(c: CellPos, orientation: u8) -> Option<CellPos> {
    let m = content::cell::orientation_matrix(orientation);
    let q = [c.x as i32 + m[0][0] as i32, c.y as i32 + m[0][1] as i32, c.z as i32 + m[0][2] as i32];
    in_frame(q).then(|| pos3(q))
}

/// Mirror of a shape across the centreline.
fn mirrored_shape(shape: CellShape) -> CellShape { content::cell::mirror_shape_z(shape) }

/// Every cell some part occupies.
fn part_cells(bp: &Blueprint, cat: &content::catalogue::Catalogue) -> BTreeSet<CellPos> {
    let mut s = BTreeSet::new();
    for p in &bp.parts {
        if let Some(def) = cat.part(&p.part) { s.extend(super::parts::footprint_cells(def, p.pos, p.rot)); }
    }
    s
}

/// Planes of one axis in ascending offset, with their indices in `hull.planes`.
fn planes_of(hull: &Hull, axis: Axis) -> Vec<(usize, InternalPlane)> {
    let mut v: Vec<(usize, InternalPlane)> = hull.planes.iter().copied().enumerate().filter(|(_, p)| p.axis == axis).collect();
    v.sort_by_key(|(_, p)| p.offset);
    v
}
fn axis_of(name: &str) -> Option<Axis> { match name { "deck" => Some(Axis::Y), "bulkhead" => Some(Axis::X), _ => None } }
/// Canonical plane order: by axis, then offset.
fn sort_planes(planes: &mut [InternalPlane]) { planes.sort_by_key(|p| (p.axis as u8, p.offset, p.watertight)); }

/// Lowest structure row (the keel).
fn keel_y(bp: &Blueprint) -> i16 { bp.cells.0.iter().filter(|r| r.runs.iter().any(|run| !run.cell.is_empty() && run.len > 0)).map(|r| r.y).min().unwrap_or(0) }

/// A plane position in metres (deck height above the keel, bulkhead distance from the stern) to its cell offset.
fn plane_offset(bp: &Blueprint, axis: Axis, position_m: f64) -> Option<i16> {
    if !position_m.is_finite() { return None; }
    let base = if axis == Axis::Y { keel_y(bp) as f64 } else { 0.0 };
    let o = (position_m / CELL_M).round() + base;
    (o >= 1.0 && o < MAX_GRID_DIM as f64 - 1.0).then_some(o as i16)
}
fn plane_position_m(bp: &Blueprint, axis: Axis, offset: i16) -> f64 {
    let base = if axis == Axis::Y { keel_y(bp) } else { 0 };
    (offset - base) as f64 * CELL_M
}

/// Is a cell on a plane inside the hull (so a deck or bulkhead belongs there)? X planes: hull on both sides
/// across the beam and below; Y planes: hull on both sides across the beam and fore and aft; Z planes: fore
/// and aft and below.
fn enclosed(grid: &CellGrid, axis: Axis, p: CellPos) -> bool {
    let solid = |x: i32, y: i32, z: i32| in_frame([x, y, z]) && !grid.get(pos3([x, y, z])).is_empty();
    let (x, y, z) = (p.x as i32, p.y as i32, p.z as i32);
    let [sx, _, sz] = [grid.size[0] as i32, grid.size[1] as i32, grid.size[2] as i32];
    let z_both = (0..z).any(|zz| solid(x, y, zz)) && (z + 1..sz).any(|zz| solid(x, y, zz));
    let x_both = (0..x).any(|xx| solid(xx, y, z)) && (x + 1..sx).any(|xx| solid(xx, y, z));
    let below = (0..y).any(|yy| solid(x, yy, z));
    match axis { Axis::X => z_both && below, Axis::Y => z_both && x_both, Axis::Z => x_both && below }
}

/// Insert or take out one plane's cells on a baked hull. Taking out removes the plane's own sheet: plain steel
/// cubes inside the hull (the same enclosure rule as inserting, so the skin stays) that are not structure
/// crossing the plane (solid on both sides across it, like a deck through a bulkhead).
fn freeform_plane(grid: &mut BTreeMap<CellPos, Cell>, axis: Axis, offset: i16, add: bool) -> bool {
    let dense = map_grid(grid);
    let [sx, sy, sz] = dense.size;
    let mut changed = false;
    let paint = if axis == Axis::Y { content::hull::PAINT_DECK } else { content::hull::PAINT_SIDES };
    let (ra, rb) = match axis { Axis::X => (sy, sz), Axis::Y => (sx, sz), Axis::Z => (sx, sy) };
    for a in 0..ra as i16 {
        for b in 0..rb as i16 {
            let p = match axis { Axis::X => CellPos::new(offset, a, b), Axis::Y => CellPos::new(a, offset, b), Axis::Z => CellPos::new(a, b, offset) };
            let c = dense.get(p);
            if add {
                if c.is_empty() && enclosed(&dense, axis, p) {
                    grid.insert(p, Cell { material: Material::Steel, armour: ArmourTier::None, paint, shape: CellShape::CUBE });
                    changed = true;
                }
            } else if c.material == Material::Steel && c.shape.is_cube() && c.armour == ArmourTier::None && c.paint == paint {
                let (n1, n2) = match axis {
                    Axis::X => (CellPos::new(p.x - 1, p.y, p.z), CellPos::new(p.x + 1, p.y, p.z)),
                    Axis::Y => (CellPos::new(p.x, p.y - 1, p.z), CellPos::new(p.x, p.y + 1, p.z)),
                    Axis::Z => (CellPos::new(p.x, p.y, p.z - 1), CellPos::new(p.x, p.y, p.z + 1)),
                };
                // Structure crossing the plane (a deck through a bulkhead) is solid on both sides: it stays.
                if !(!dense.get(n1).is_empty() && !dense.get(n2).is_empty()) && enclosed(&dense, axis, p) {
                    grid.remove(&p);
                    changed = true;
                }
            }
        }
    }
    changed
}

/// The design with its plane list replaced: regenerated on a lofted hull, cut and filled on a baked one.
fn with_planes(bp: &Blueprint, planes: Vec<InternalPlane>, cat: &content::catalogue::Catalogue) -> Option<Blueprint> {
    let mut planes = planes;
    sort_planes(&mut planes);
    match &bp.hull.source {
        HullSource::Lofted(_) => {
            let mut hull = bp.hull.clone();
            hull.planes = planes;
            regenerate(bp, hull, cat)
        }
        HullSource::Freeform => {
            let mut map = cell_map(&rows_grid(bp)?);
            let old: BTreeSet<(u8, i16)> = bp.hull.planes.iter().map(|p| (p.axis as u8, p.offset)).collect();
            let new: BTreeSet<(u8, i16)> = planes.iter().map(|p| (p.axis as u8, p.offset)).collect();
            let axis = |a: u8| match a { 0 => Axis::X, 1 => Axis::Y, _ => Axis::Z };
            for &(a, o) in old.difference(&new) { freeform_plane(&mut map, axis(a), o, false); }
            for &(a, o) in new.difference(&old) { freeform_plane(&mut map, axis(a), o, true); }
            let mut next = bp.clone();
            next.cells = map_grid(&map).to_rows();
            next.hull.planes = planes;
            Some(next)
        }
    }
}

/// `CompartmentStaticDict` from a built compartment.
fn compartment_dict(c: &sim_core::compartments::Compartment, set: &sim_core::compartments::CompartmentSet, deck_offsets: &[i16], waterline_y: i16) -> VarDictionary {
    let min = c.min.map(|v| v as f32 * CELL_M as f32);
    let ext = [(c.max[0] - c.min[0] + 1) as f32 * CELL_M as f32, (c.max[1] - c.min[1] + 1) as f32 * CELL_M as f32, (c.max[2] - c.min[2] + 1) as f32 * CELL_M as f32];
    let deck = deck_offsets.iter().filter(|&&o| o <= c.min[1]).count() as i64;
    let neighbours: PackedInt32Array = set.adjacency.iter().filter_map(|a| if a.a == c.id { Some(a.b.0 as i32) } else if a.b == c.id { Some(a.a.0 as i32) } else { None }).collect();
    d! {
        "id" => c.id.0 as i64,
        "name" => msg_dict(&Msg::new("dock.compartment").arg(MsgArg::Int(c.id.0 as i64 + 1))),
        "deck" => deck,
        "rect" => Rect2::new(Vector2::new(min[0], min[2]), Vector2::new(ext[0], ext[2])),
        "side_rect" => Rect2::new(Vector2::new(min[0], min[1]), Vector2::new(ext[0], ext[1])),
        "volume_m3" => if c.volume_m3.is_finite() { c.volume_m3 } else { 0.0 },
        "has_magazine" => c.magazine.is_some(),
        "below_waterline" => c.below_waterline || c.min[1] < waterline_y,
        "neighbours" => neighbours,
    }
}

impl BpEditor {
    /// The mirror plane sum when hull edits mirror (see the module doc), else `None`.
    fn hull_mirror(&self) -> Option<i32> {
        let m = self.model.as_ref()?;
        let on = match &m.blueprint.hull.source { HullSource::Lofted(s) => s.mirrored, HullSource::Freeform => m.symmetry == crate::enums::Symmetry::Mirror };
        if on { super::mirror_sum(&m.blueprint) } else { None }
    }
    fn hull_max_len(&self) -> f64 { self.model.as_ref().map_or(content::cell::MAX_PLAYER_HULL_M, |m| super::max_length_m(m.context.mode, &m.context.ruleset)) }
    fn hull_exterior(&self) -> Option<std::rc::Rc<Exterior>> {
        let m = self.model.as_ref()?;
        if let Some((r, e)) = self.state.hull.exterior.borrow().as_ref() {
            if *r == m.rev { return Some(e.clone()); }
        }
        let cat = crate::services::services().catalogue();
        let e = std::rc::Rc::new(Exterior::of(&content::blueprint::io::build_grid(&m.blueprint, &cat)));
        *self.state.hull.exterior.borrow_mut() = Some((m.rev, e.clone()));
        Some(e)
    }
    pub(crate) fn handle_list(&self) -> Vec<HandleInfo> {
        let Some(m) = self.model.as_ref() else { return Vec::new() };
        let HullSource::Lofted(shell) = &m.blueprint.hull.source else { return Vec::new() };
        if let Some((r, h)) = self.state.hull.handles.borrow().as_ref() {
            if *r == m.rev { return h.clone(); }
        }
        let h = shell_handles(shell, self.hull_max_len());
        *self.state.hull.handles.borrow_mut() = Some((m.rev, h.clone()));
        h
    }
    /// Commit a cell-grid change of the structure.
    fn commit_cells(&mut self, grid: &CellGrid, label: &str) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        let mut next = m.blueprint.clone();
        next.cells = grid.to_rows();
        self.commit(next, label)
    }
}

#[godot_api(secondary)]
impl BpEditor {
    /// Returns `HullDict`.
    #[func]
    fn hull(&self) -> VarDictionary {
        let Some(m) = self.model.as_ref() else { return VarDictionary::new() };
        let bp = &m.blueprint;
        let size = CellGrid::from_rows(&bp.cells).size;
        let decks: Vec<f32> = planes_of(&bp.hull, Axis::Y).iter().map(|(_, p)| plane_position_m(bp, Axis::Y, p.offset) as f32).collect();
        let bulkheads: Vec<f32> = planes_of(&bp.hull, Axis::X).iter().map(|(_, p)| plane_position_m(bp, Axis::X, p.offset) as f32).collect();
        let spacing = if decks.len() >= 2 { let mut gaps: Vec<f32> = decks.windows(2).map(|w| w[1] - w[0]).collect(); gaps.sort_by(|a, b| a.total_cmp(b)); gaps[gaps.len() / 2] as f64 } else { content::hull::DEFAULT_DECK_SPACING_M };
        let (stations, length, bow, transom, width, mirrored, baked) = match &bp.hull.source {
            HullSource::Lofted(shell) => {
                let field = content::hull::ShellField::new(shell);
                let (xa, xb) = field.as_ref().map_or((0.0, shell.length_m), |f| waterline_ends(f, shell));
                let layout = content::hull::shell_layout(shell);
                let list: Array<VarDictionary> = shell.stations.iter().enumerate().map(|(i, s)| {
                    let x = layout.to_vessel([xa + s.t * (xb - xa), 0.0, 0.0])[0];
                    d! { "index" => i as i64, "x_m" => x, "beam_m" => s.beam_m, "draft_m" => s.draft_m, "deadrise_deg" => s.deadrise_deg, "flare_deg" => s.flare_deg, "sheer_m" => s.sheer_m }
                }).collect();
                (list, shell.length_m, shell.bow_rake_deg, shell.transom_rake_deg, shell.transom_width, shell.mirrored, false)
            }
            HullSource::Freeform => (Array::new(), super::design_length_m(bp), 0.0, 0.0, 0.0, m.symmetry == crate::enums::Symmetry::Mirror, true),
        };
        d! {
            "length_m" => length,
            "stations" => stations,
            "bow_rake_deg" => bow,
            "transom" => transom,
            "transom_width" => width,
            "mirrored" => mirrored,
            "baked" => baked,
            "deck_spacing_m" => spacing,
            "decks" => PackedFloat32Array::from(decks.as_slice()),
            "bulkheads" => PackedFloat32Array::from(bulkheads.as_slice()),
            "size_cells" => Vector3i::new(size[0] as i32, size[1] as i32, size[2] as i32),
            "waterline_m" => bp.hull.design_waterline_y as f64 * CELL_M,
        }
    }
    /// Every handle with its value, range and gizmo position: `Array[HandleDict]`. Empty after a bake.
    #[func]
    fn handles(&self) -> Array<VarDictionary> {
        self.handle_list().into_iter().map(|h| {
            let pos = Vector3::new(h.pos[0] as f32, h.pos[1] as f32, h.pos[2] as f32);
            d! { "id" => GString::from(h.id), "station" => h.station, "value" => h.value, "min" => h.min, "max" => h.max, "step" => h.step, "unit" => StringName::from(h.unit), "pos" => pos }
        }).collect()
    }
    /// Set one handle (`HandleDict.id`, station index) to a value, clamped to its range. Returns the value
    /// applied. Wrap a drag in `begin_group` and `end_group`; `dragging` true asks for the coarse GZ curve
    /// while the handle moves (L187).
    #[func]
    fn set_handle(&mut self, id: GString, station: i64, value: f64, dragging: bool) -> f64 {
        let id = id.to_string();
        let max_len = self.hull_max_len();
        let Some(m) = self.model.as_ref() else { return 0.0 };
        let HullSource::Lofted(shell) = &m.blueprint.hull.source else { return 0.0 };
        let st = if handle_range(&id).is_some() && STATION_HANDLES.iter().any(|(h, ..)| *h == id) { usize::try_from(station).ok().filter(|s| *s < shell.stations.len()) } else { Some(0) };
        let (Some(st), Some(cur)) = (st, st.and_then(|s| get_handle(shell, &id, s))) else { return 0.0 };
        let Some((lo, hi)) = valid_range(shell, &id, st, max_len) else { return cur };
        let v = if value.is_finite() { value.clamp(lo, hi) } else { cur };
        if (v - cur).abs() < 1e-9 { return cur; }
        let Ok(st8) = u8::try_from(st) else { return cur };
        let label = if id == "length" { "dock.undo.length" } else { "dock.undo.handle" };
        self.state.dragging = dragging && self.model.as_ref().is_some_and(|m| m.group.is_some());
        let edit = sim_systems::editor::BlueprintEdit::SetHullHandle { station: st8, handle: id.clone(), value: v };
        if !self.commit_edit(&edit, label) { return cur; }
        crate::services::services().tutorial_signal(sim_systems::tutorial::Signal::HandleChanged { handle: id.clone() });
        match self.model.as_ref().map(|m| &m.blueprint.hull.source) { Some(HullSource::Lofted(s)) => get_handle(s, &id, st).unwrap_or(v), _ => v }
    }
    /// Hull length in metres (10 to 90; 150 in enemy-class mode). Returns the length applied.
    #[func]
    fn set_length(&mut self, length_m: f64) -> f64 { self.set_handle(GString::from("length"), 0, length_m, false) }
    /// Insert a station at `x_m` from the stern (4 to 16 stations). Returns its index or `Bp.NONE`.
    ///
    /// `x_m` is measured along the design waterline in the editor frame. The new station takes values
    /// interpolated linearly between its neighbours, so the shell changes shape only slightly (the loft
    /// between stations is a monotone cubic) until it is edited.
    #[func]
    fn add_station(&mut self, x_m: f64) -> i64 {
        let cat = crate::services::services().catalogue();
        let max_len = self.hull_max_len();
        let Some(m) = self.model.as_ref() else { return -1 };
        let HullSource::Lofted(shell) = &m.blueprint.hull.source else { return -1 };
        if shell.stations.len() >= content::hull::MAX_STATIONS || !x_m.is_finite() { return -1; }
        let Some(field) = content::hull::ShellField::new(shell) else { return -1 };
        let layout = field.layout();
        let (xa, xb) = waterline_ends(&field, shell);
        if xb - xa <= 0.0 { return -1; }
        let xd = layout.to_design([x_m, 0.0, 0.0])[0];
        let t = (xd - xa) / (xb - xa);
        if !(t > 0.0 && t < 1.0) || shell.stations.iter().any(|s| (s.t - t).abs() < MIN_STATION_GAP_T) { return -1; }
        let i = shell.stations.partition_point(|s| s.t < t);
        let (a, b) = (shell.stations[i.saturating_sub(1)], shell.stations[i.min(shell.stations.len() - 1)]);
        let f = if (b.t - a.t).abs() > 1e-9 { ((t - a.t) / (b.t - a.t)).clamp(0.0, 1.0) } else { 0.5 };
        let lerp = |x: f64, y: f64| x + (y - x) * f;
        let st = Station { t, beam_m: lerp(a.beam_m, b.beam_m), draft_m: lerp(a.draft_m, b.draft_m), deadrise_deg: lerp(a.deadrise_deg, b.deadrise_deg), flare_deg: lerp(a.flare_deg, b.flare_deg), sheer_m: lerp(a.sheer_m, b.sheer_m) };
        let mut s2 = shell.clone();
        s2.stations.insert(i, st);
        if content::hull::validate_shell(&s2, max_len).is_err() { return -1; }
        let mut hull = m.blueprint.hull.clone();
        hull.source = HullSource::Lofted(s2);
        let Some(next) = regenerate(&m.blueprint, hull, &cat) else { return -1 };
        if !self.commit(next, "dock.undo.station_add") { return -1; }
        i as i64
    }
    #[func]
    fn remove_station(&mut self, index: i64) -> bool {
        let cat = crate::services::services().catalogue();
        let max_len = self.hull_max_len();
        let Some(m) = self.model.as_ref() else { return false };
        let HullSource::Lofted(shell) = &m.blueprint.hull.source else { return false };
        let Some(i) = usize::try_from(index).ok().filter(|i| *i < shell.stations.len()) else { return false };
        if shell.stations.len() <= content::hull::MIN_STATIONS { return false; }
        let mut s2 = shell.clone();
        s2.stations.remove(i);
        if content::hull::validate_shell(&s2, max_len).is_err() { return false; }
        let mut hull = m.blueprint.hull.clone();
        hull.source = HullSource::Lofted(s2);
        let Some(next) = regenerate(&m.blueprint, hull, &cat) else { return false };
        self.commit(next, "dock.undo.station_remove")
    }
    /// Mirror the shell port to starboard (default on).
    #[func]
    fn set_mirrored(&mut self, on: bool) {
        let Some(m) = self.model.as_ref() else { return };
        let HullSource::Lofted(shell) = &m.blueprint.hull.source else { return };
        if shell.mirrored == on { return; }
        let mut next = m.blueprint.clone();
        if let HullSource::Lofted(s) = &mut next.hull.source { s.mirrored = on; }
        self.commit(next, "dock.undo.mirror");
    }
    /// Decks and bulkheads are planes through the hull that cut it into compartments (L132).
    /// `axis` is `&"deck"` (height in metres above the keel) or `&"bulkhead"` (x in metres from the stern).
    /// Returns the plane's index or `Bp.NONE`.
    #[func]
    fn add_plane(&mut self, axis: StringName, position_m: f64) -> i64 {
        let cat = crate::services::services().catalogue();
        let Some(ax) = axis_of(&axis.to_string()) else { return -1 };
        let Some(m) = self.model.as_ref() else { return -1 };
        let bp = &m.blueprint;
        let Some(offset) = plane_offset(bp, ax, position_m) else { return -1 };
        let size = CellGrid::from_rows(&bp.cells).size;
        let limit = match ax { Axis::X => size[0], Axis::Y => size[1], Axis::Z => size[2] } as i16;
        if offset >= limit - 1 || bp.hull.planes.iter().any(|p| p.axis == ax && p.offset == offset) { return -1; }
        let mut planes = bp.hull.planes.clone();
        planes.push(InternalPlane { axis: ax, offset, watertight: true });
        let Some(next) = with_planes(bp, planes, &cat) else { return -1 };
        if !self.commit(next, "dock.undo.plane_add") { return -1; }
        self.model.as_ref().map_or(-1, |m| planes_of(&m.blueprint.hull, ax).iter().position(|(_, p)| p.offset == offset).map_or(-1, |i| i as i64))
    }
    /// Move a plane; returns the position applied (snapped to 0.5 m).
    #[func]
    fn move_plane(&mut self, axis: StringName, index: i64, position_m: f64) -> f64 {
        let cat = crate::services::services().catalogue();
        let Some(ax) = axis_of(&axis.to_string()) else { return 0.0 };
        let Some(m) = self.model.as_ref() else { return 0.0 };
        let bp = &m.blueprint;
        let list = planes_of(&bp.hull, ax);
        let Some(&(at, plane)) = usize::try_from(index).ok().and_then(|i| list.get(i)) else { return 0.0 };
        let cur = plane_position_m(bp, ax, plane.offset);
        let Some(offset) = plane_offset(bp, ax, position_m) else { return cur };
        let size = CellGrid::from_rows(&bp.cells).size;
        let limit = match ax { Axis::X => size[0], Axis::Y => size[1], Axis::Z => size[2] } as i16;
        if offset == plane.offset || offset >= limit - 1 || bp.hull.planes.iter().any(|p| p.axis == ax && p.offset == offset) { return cur; }
        let mut planes = bp.hull.planes.clone();
        planes[at].offset = offset;
        let Some(next) = with_planes(bp, planes, &cat) else { return cur };
        if !self.commit(next, "dock.undo.plane_move") { return cur; }
        self.model.as_ref().map_or(cur, |m| plane_position_m(&m.blueprint, ax, offset))
    }
    #[func]
    fn remove_plane(&mut self, axis: StringName, index: i64) -> bool {
        let cat = crate::services::services().catalogue();
        let Some(ax) = axis_of(&axis.to_string()) else { return false };
        let Some(m) = self.model.as_ref() else { return false };
        let bp = &m.blueprint;
        let list = planes_of(&bp.hull, ax);
        let Some(&(at, _)) = usize::try_from(index).ok().and_then(|i| list.get(i)) else { return false };
        let mut planes = bp.hull.planes.clone();
        planes.remove(at);
        let Some(next) = with_planes(bp, planes, &cat) else { return false };
        self.commit(next, "dock.undo.plane_remove")
    }
    /// Compartments the planes produce: `Array[CompartmentStaticDict]`.
    #[func]
    fn compartments(&self) -> Array<VarDictionary> {
        let Some(m) = self.model.as_ref() else { return Array::new() };
        let Some(design) = self.current_design() else { return Array::new() };
        let set = &design.prefab.compartments;
        let decks: Vec<i16> = planes_of(&m.blueprint.hull, Axis::Y).iter().map(|(_, p)| p.offset).collect();
        set.compartments.iter().filter(|c| !c.is_absorbed()).map(|c| compartment_dict(c, set, &decks, m.blueprint.hull.design_waterline_y)).collect()
    }
    /// Bake the shell into freeform cells. One-way: handles are gone afterwards (L134). The UI confirms first.
    #[func]
    fn bake_hull(&mut self) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        if !matches!(m.blueprint.hull.source, HullSource::Lofted(_)) { return false; }
        let mut next = m.blueprint.clone();
        next.hull.source = HullSource::Freeform;
        self.commit(next, "dock.undo.bake")
    }
    /// Freeform sculpting after a bake, and superstructure at any time: add (`add` true) or remove cells.
    /// `cells` are cell coordinates as `Vector3` of integers; `shape` is 0 cube, 1 wedge, 2 corner,
    /// 3 inverse corner, 4 slope 1:2; `orientation` is 0..23. Returns how many cells changed.
    #[func]
    fn sculpt(&mut self, cells: PackedVector3Array, add: bool, shape: i64, orientation: i64) -> i64 {
        let cat = crate::services::services().catalogue();
        let mirror = self.hull_mirror();
        let Some(m) = self.model.as_ref() else { return 0 };
        let bp = &m.blueprint;
        let Some(grid) = rows_grid(bp) else { return 0 };
        let Some(shape) = (if add { shape_of(shape, orientation) } else { Some(CellShape::CUBE) }) else { return 0 };
        let targets = brush(&cells, mirror);
        if targets.is_empty() { return 0; }
        let mut map = cell_map(&grid);
        let taken = part_cells(bp, &cat);
        let generated = match &bp.hull.source { HullSource::Lofted(_) => Some(content::hull::voxelize(&bp.hull).grid), HullSource::Freeform => None };
        let mut changed = 0i64;
        let neighbour_paint = |map: &BTreeMap<CellPos, Cell>, c: CellPos| Face::ALL.iter().find_map(|f| { let (dx, dy, dz) = f.offset(); map.get(&CellPos::new(c.x + dx, c.y + dy, c.z + dz)).filter(|n| n.material != Material::PartBody).map(|n| n.paint) }).unwrap_or(content::hull::PAINT_SIDES);
        for (c, image) in targets {
            if add {
                if taken.contains(&c) { continue; }
                // The mirror image of a shaped cell is the mirrored shape.
                let here = if image { mirrored_shape(shape) } else { shape };
                let put = |p: CellPos, sh: CellShape, map: &mut BTreeMap<CellPos, Cell>| -> bool {
                    if taken.contains(&p) { return false; }
                    let paint = map.get(&p).map(|c| c.paint).unwrap_or_else(|| neighbour_paint(map, p));
                    let armour = map.get(&p).map_or(ArmourTier::None, |c| c.armour);
                    let cell = Cell { material: map.get(&p).map_or(Material::Steel, |c| c.material), armour, paint, shape: sh };
                    if map.get(&p) == Some(&cell) { return false; }
                    map.insert(p, cell);
                    true
                };
                if put(c, here, &mut map) { changed += 1; }
                if here.kind() == ShapeKind::SlopeHigh {
                    if let Some(low) = slope_low_cell(c, here.orientation()) {
                        if put(low, CellShape::new(ShapeKind::SlopeLow, here.orientation()), &mut map) { changed += 1; }
                    }
                }
            } else {
                if let Some(g) = &generated { if !g.get(c).is_empty() { continue; } }
                if map.remove(&c).is_some() { changed += 1; }
            }
        }
        if changed == 0 { return 0; }
        let grid = map_grid(&map);
        if !self.commit_cells(&grid, "dock.undo.sculpt") { return 0; }
        changed
    }
    /// Paint armour on outer cells; `tier` is `Bp.ARMOUR_*`. Returns `{ "cells": int, "mass_delta_kg":
    /// float, "cost_delta": int }`.
    #[func]
    fn paint_armour(&mut self, cells: PackedVector3Array, tier: i64) -> VarDictionary {
        let none = || d! { "cells" => 0i64, "mass_delta_kg" => 0.0, "cost_delta" => 0i64 };
        let Some(tier) = ArmourTier::from_gd(tier) else { return none() };
        let cat = crate::services::services().catalogue();
        let mirror = self.hull_mirror();
        let Some(ext) = self.hull_exterior() else { return none() };
        let Some(m) = self.model.as_ref() else { return none() };
        let Some(grid) = rows_grid(&m.blueprint) else { return none() };
        let taken = part_cells(&m.blueprint, &cat);
        let (mut n, mut mass, mut cost) = (0i64, 0.0f64, 0.0f64);
        let mut picked = Vec::new();
        for (c, _) in brush(&cells, mirror) {
            let cell = grid.get(c);
            if cell.is_empty() || matches!(cell.material, Material::PartBody | Material::Glass) || taken.contains(&c) || cell.armour == tier || !ext.outer(c) { continue; }
            let Some(def) = cat.materials.iter().find(|d| d.material == cell.material) else { continue };
            let at = |t: ArmourTier| -> (f64, f64) { if t == ArmourTier::None { (0.0, 0.0) } else { let i = t as usize - 1; (def.armour_mass_kg[i], def.armour_cost[i]) } };
            let (m0, c0) = at(cell.armour);
            let (m1, c1) = at(tier);
            mass += m1 - m0;
            cost += c1 - c0;
            n += 1;
            picked.push(c);
        }
        if picked.is_empty() { return none(); }
        let edit = sim_systems::editor::BlueprintEdit::PaintArmour { cells: picked, tier: tier as u8 };
        if !self.commit_edit(&edit, "dock.undo.armour") { return none(); }
        d! { "cells" => n, "mass_delta_kg" => if mass.is_finite() { mass } else { 0.0 }, "cost_delta" => if cost.is_finite() { cost.round() as i64 } else { 0 } }
    }
    /// Paint cells with palette entry `index` (0..15). Returns how many cells changed.
    #[func]
    fn paint_colour(&mut self, cells: PackedVector3Array, index: i64) -> i64 {
        let mirror = self.hull_mirror();
        let Some(m) = self.model.as_ref() else { return 0 };
        let Some(paint) = u8::try_from(index).ok().filter(|i| (*i as usize) < m.blueprint.paint.len().min(content::blueprint::MAX_PAINT)) else { return 0 };
        let Some(mut grid) = rows_grid(&m.blueprint) else { return 0 };
        let mut n = 0i64;
        for (c, _) in brush(&cells, mirror) {
            let mut cell = grid.get(c);
            if cell.is_empty() || cell.material == Material::PartBody || cell.paint == paint { continue; }
            cell.paint = paint;
            grid.set(c, cell);
            n += 1;
        }
        if n == 0 || !self.commit_cells(&grid, "dock.undo.paint") { return 0; }
        n
    }
    /// The ship palette: `Array[PaintDict]`, 16 entries (8 naval swatches, antifouling, 7 custom).
    ///
    /// Custom entries not yet picked come back with `empty: true` (an extra key) and black.
    #[func]
    fn palette(&self) -> Array<VarDictionary> {
        let Some(m) = self.model.as_ref() else { return Array::new() };
        (0..content::blueprint::MAX_PAINT).map(|i| match m.blueprint.paint.get(i) {
            Some(p) => paint_dict(i, p, false),
            None => paint_dict(i, &PaintEntry { token: None, rgb: [0.0; 3], usage: PaintUse::Hull }, true),
        }).collect()
    }
    /// Set a custom entry (9..15) from a linear RGB pick. The value is clamped in OKLCH, chroma first, to the
    /// hull or marking limits (L1803); returns the `PaintDict` actually stored.
    ///
    /// An index past the entries in use stores at the next free entry (the table has no gaps); the result's
    /// `index` says where.
    #[func]
    fn set_palette_entry(&mut self, index: i64, rgb: Vector3, marking: bool) -> VarDictionary {
        let Some(m) = self.model.as_ref() else { return VarDictionary::new() };
        let first = content::blueprint::io::DEFAULT_PAINT_COUNT as i64;
        if index < first || index >= content::blueprint::MAX_PAINT as i64 { return VarDictionary::new(); }
        let len = m.blueprint.paint.len();
        if len < content::blueprint::io::DEFAULT_PAINT_COUNT { return VarDictionary::new(); }
        let at = (index as usize).min(len);
        if at >= content::blueprint::MAX_PAINT { return VarDictionary::new(); }
        let raw = PaintEntry { token: None, rgb: [rgb.x, rgb.y, rgb.z], usage: if marking { PaintUse::Marking } else { PaintUse::Hull } };
        let entry = content::blueprint::io::clamp_paint(&raw);
        let mut next = m.blueprint.clone();
        if at == len { next.paint.push(entry.clone()); } else { next.paint[at] = entry.clone(); }
        self.commit(next, "dock.undo.palette");
        paint_dict(at, &entry, false)
    }
    /// Markings: `Array[Dictionary]` with `index: int`, `kind: int` (`Bp.MARKING_*`), `cell: Vector3i`,
    /// `face: int` (0..5: -x, +x, -y, +y, -z, +z), `paint: int`, `size_cells: int`, `text: String`.
    #[func]
    fn markings(&self) -> Array<VarDictionary> {
        let Some(m) = self.model.as_ref() else { return Array::new() };
        m.blueprint.markings.iter().enumerate().map(|(i, k)| d! {
            "index" => i as i64, "kind" => k.kind.to_gd(), "cell" => Vector3i::new(k.cell.x as i32, k.cell.y as i32, k.cell.z as i32),
            "face" => Face::ALL.iter().position(|f| *f == k.face).unwrap_or(0) as i64, "paint" => k.paint as i64, "size_cells" => k.size_cells as i64, "text" => GString::from(k.text.as_str()),
        }).collect()
    }
    /// Add a stripe, flag or hull number. Returns its index or `Bp.NONE`.
    ///
    /// On a mirrored hull a marking on a side face gets its mirror image on the other side too.
    #[func]
    fn add_marking(&mut self, kind: i64, cell: Vector3i, face: i64, paint: i64, size_cells: i64, text: GString) -> i64 {
        let mirror = self.hull_mirror();
        let Some(m) = self.model.as_ref() else { return -1 };
        let Some(kind) = MarkingKind::from_gd(kind) else { return -1 };
        let Some(face) = usize::try_from(face).ok().and_then(|f| Face::ALL.get(f).copied()) else { return -1 };
        let c = [cell.x, cell.y, cell.z];
        if !in_frame(c) || !(1..=MAX_MARKING_CELLS).contains(&size_cells) { return -1; }
        let Some(paint) = u8::try_from(paint).ok().filter(|p| (*p as usize) < m.blueprint.paint.len()) else { return -1 };
        if m.blueprint.markings.len() >= MAX_MARKINGS { return -1; }
        let text: String = text.to_string().chars().filter(|c| !c.is_control()).take(MAX_MARKING_TEXT).collect();
        let mark = Marking { kind, cell: pos3(c), face, paint, size_cells: size_cells as u8, text };
        let mut next = m.blueprint.clone();
        let index = next.markings.len() as i64;
        if let Some(s) = mirror.filter(|_| matches!(face, Face::ZNeg | Face::ZPos)) {
            let mc = [c[0], c[1], s - c[2]];
            let mut twin = mark.clone();
            twin.cell = pos3(mc);
            twin.face = if face == Face::ZNeg { Face::ZPos } else { Face::ZNeg };
            next.markings.push(mark);
            if in_frame(mc) && twin.cell != next.markings[index as usize].cell && next.markings.len() < MAX_MARKINGS { next.markings.push(twin); }
        } else {
            next.markings.push(mark);
        }
        if !self.commit(next, "dock.undo.marking_add") { return -1; }
        index
    }
    #[func]
    fn remove_marking(&mut self, index: i64) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        let Some(i) = usize::try_from(index).ok().filter(|i| *i < m.blueprint.markings.len()) else { return false };
        let mut next = m.blueprint.clone();
        next.markings.remove(i);
        self.commit(next, "dock.undo.marking_remove")
    }
}

/// `PaintDict`.
fn paint_dict(index: usize, p: &PaintEntry, empty: bool) -> VarDictionary {
    d! {
        "index" => index as i64,
        "token" => StringName::from(p.token.as_deref().unwrap_or("")),
        "rgb" => Vector3::new(p.rgb[0], p.rgb[1], p.rgb[2]),
        "marking" => p.usage == PaintUse::Marking,
        "locked" => index < content::blueprint::io::DEFAULT_PAINT_COUNT,
        "empty" => empty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fac() -> Blueprint { content::campaign::load::stock_blueprint("starters/fac22.bp.ron").unwrap() }
    fn shell(bp: &Blueprint) -> &HullShell { match &bp.hull.source { HullSource::Lofted(s) => s, _ => panic!("lofted starter") } }

    #[test]
    fn regeneration_matches_the_fix_path() {
        let cat = content::catalogue::catalogue();
        let bp = fac();
        let edit = sim_systems::editor::BlueprintEdit::SetHullHandle { station: 1, handle: "beam".into(), value: shell(&bp).stations[1].beam_m + 0.8 };
        let via_fix = sim_systems::editor::notes::apply_edit(&bp, &edit, cat).unwrap();
        let mut hull = bp.hull.clone();
        if let HullSource::Lofted(s) = &mut hull.source { s.stations[1].beam_m += 0.8; }
        let mine = regenerate(&bp, hull, cat).unwrap();
        assert_eq!(mine.cells, via_fix.cells);
        assert_eq!(mine.parts, via_fix.parts);
        assert_eq!(mine.hull, via_fix.hull);
    }

    #[test]
    fn handle_ranges_stay_valid() {
        let bp = fac();
        let s = shell(&bp);
        for h in shell_handles(s, 90.0) {
            assert!(h.min <= h.value + 1e-9 && h.value <= h.max + 1e-9, "{} out of its own range", h.id);
            for v in [h.min, h.max] {
                let mut t = s.clone();
                assert!(put_handle(&mut t, h.id, h.station.max(0) as usize, v));
                assert!(content::hull::validate_shell(&t, 90.0).is_ok(), "{} at {v} is refused", h.id);
            }
        }
        let (lo, hi) = valid_range(s, "length", 0, 90.0).unwrap();
        assert!(lo >= 10.0 && hi <= 90.0 && lo <= s.length_m && s.length_m <= hi);
    }

    #[test]
    fn planes_cut_and_fill_on_both_hull_kinds() {
        let cat = content::catalogue::catalogue();
        let bp = fac();
        let size = CellGrid::from_rows(&bp.cells).size;
        let x = (size[0] / 2) as i16;
        let x = (x..size[0] as i16 - 2).find(|x| !bp.hull.planes.iter().any(|p| p.axis == Axis::X && p.offset == *x)).unwrap();
        let mut planes = bp.hull.planes.clone();
        planes.push(InternalPlane { axis: Axis::X, offset: x, watertight: true });
        let lofted = with_planes(&bp, planes.clone(), cat).unwrap();
        assert!(lofted.hull.planes.iter().any(|p| p.axis == Axis::X && p.offset == x));
        assert!(CellGrid::from_rows(&lofted.cells).solid_count() > CellGrid::from_rows(&bp.cells).solid_count());
        // The same on a baked hull, and back out again.
        let mut baked = bp.clone();
        baked.hull.source = HullSource::Freeform;
        let with = with_planes(&baked, planes, cat).unwrap();
        let added = CellGrid::from_rows(&with.cells).solid_count() - CellGrid::from_rows(&baked.cells).solid_count();
        assert!(added > 0);
        let back = with_planes(&with, baked.hull.planes.clone(), cat).unwrap();
        let left = CellGrid::from_rows(&back.cells).solid_count() as i64 - CellGrid::from_rows(&baked.cells).solid_count() as i64;
        assert!(left.abs() * 10 <= added as i64, "taking the bulkhead out removes its sheet ({left} of {added} left)");
    }

    #[test]
    fn the_waterline_band_follows() {
        let cat = content::catalogue::catalogue();
        let bp = fac();
        let row = bp.hull.design_waterline_y + 1;
        let next = with_waterline(&bp, row, cat).unwrap();
        assert_eq!(next.hull.design_waterline_y, row);
        let g = CellGrid::from_rows(&next.cells);
        let band = g.cells.iter().enumerate().filter(|(_, c)| c.paint == content::hull::PAINT_BOOT_TOPPING && !c.is_empty()).map(|(i, _)| g.pos(content::ids::CellIndex(i as u32)).y).collect::<BTreeSet<_>>();
        assert!(band.contains(&row), "the boot-topping band sits on the new row");
        let mut baked = bp.clone();
        baked.hull.source = HullSource::Freeform;
        let next = with_waterline(&baked, row, cat).unwrap();
        let g = CellGrid::from_rows(&next.cells);
        assert!(g.cells.iter().enumerate().any(|(i, c)| c.paint == content::hull::PAINT_BOOT_TOPPING && g.pos(content::ids::CellIndex(i as u32)).y == row));
    }

    #[test]
    fn shapes_and_slopes() {
        assert_eq!(shape_of(0, 7), Some(CellShape::CUBE));
        assert_eq!(shape_of(1, 3).map(|s| (s.kind(), s.orientation())), Some((ShapeKind::Wedge, 3)));
        assert!(shape_of(5, 0).is_none() && shape_of(1, 24).is_none() && shape_of(1, -1).is_none());
        assert_eq!(slope_low_cell(CellPos::new(3, 3, 3), 0), Some(CellPos::new(4, 3, 3)));
    }
}
