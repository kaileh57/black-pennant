//! Unit `gd-editor-parts`. Parts and kits on `BpEditor`: the tray, ghost placement, moving, kit settings and
//! kit updates. DESIGN.md L136-151 (parts have typed ports and no wires; a placed part joins its
//! compartment's network at once; kits are versioned sub-assemblies with sockets and a settings sheet;
//! updating a kit offers to migrate every placed copy), L244 (a picked kit follows the cursor as a ghost,
//! snaps to cells, mirrors; click places and keeps placing until Esc; R and Shift+R rotate 90 degrees about
//! the vertical, T tilts), L233 (click-to-place as the non-drag alternative; per-ruleset part limits).
//! Placement legality is `content::validate`; footprints are `content::kit::kit_footprint`.
//!
//! # Placement verdicts
//! A ghost's verdict is `content::validate::validate_blueprint` itself: the candidate design (the placement
//! and its symmetry copies added through `apply_edit`) is validated against the dock's ruleset and
//! availability, and the placement is refused when it adds a problem the current design does not have (bad
//! placement, out of bounds, unavailable part, over a ruleset limit, structure no longer in one piece). The
//! host runs the same check, so a ghost that shows valid is a part the host accepts. Verdicts are cached per
//! revision and ghost pose, so moving the cursor across one cell validates once.
//!
//! # Choices the spec leaves open
//! - The ghost's `cell` is the cell under the cursor (the empty cell against the surface). A kit with a socket
//!   that faces down (`deck`, `floor`, `edge`; `deck` first) stands that socket's cell on it, so a gun mount's
//!   magazine and hoist go below the deck its gun stands on. Anything else is centred on it across X and Z and
//!   stands on it.
//! - A body that belongs inside the hull (an internal part, or a kit of internal parts) and cannot go where the
//!   cursor points settles down the column under it to the lowest pose that is valid: pointing at the deck
//!   puts an engine or a fuel tank in the hull below, with no deck slice needed. The ghost then reports the
//!   pose it settled to.
//! - Mirror copies reflect across the centreline (the structure's mid-plane), radial copies turn about the
//!   hull's vertical centre line; a copy that would land on the original or on another copy is dropped.
//! - Pasted parts are placed loose (no kit), keeping their relative layout; R and Shift+R turn the paste.
//! - Moving one part of a kit moves the whole kit; its `yaw` and `tilt` turn the kit by the same steps as
//!   they turn that part, keeping every slot (so rules and labels keep pointing at the same parts).
//! - Migrating a kit keeps each copy's pose and the settings whose keys still exist; parts the new version
//!   adds or drops change slots, as a swap does.
//! - `set_load` takes a fill fraction for every loadable part; ballast stores it as kilograms of its maximum.
//! - Routes come from the design the worker built (`routes()` is empty until it is in). A pinned conduit names
//!   the lowest-slot part on its network at each end; a breaker sits on the middle cell of the conduit between
//!   the two compartments.
use super::{BpEditor, Ghost};
use crate::convert::{d, empty_msg, get_bool, get_i64, get_text, msg_dict};
use crate::enums::{EditorMode, EditorTool, GdEnum, Symmetry};
use content::blueprint::{Blueprint, PartPlacement};
use content::catalogue::Catalogue;
use content::cell::{CellPos, MAX_GRID_DIM};
use content::enums::NetworkKind;
use content::ids::{KitId, KitSlot, PartId, PartSlot};
use content::kit::{KitDef, KitKind, PartRot};
use content::part::{PartDef, Placement};
use content::validate::Issue;
use godot::prelude::*;
use sim_systems::editor::BlueprintEdit;
use sim_systems::protocol::{Msg, MsgArg};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

/// Text keys this unit emits.
pub(crate) const PARTS_KEYS: &[&str] = &[
    "dock.undo.place_part", "dock.undo.place_kit", "dock.undo.paste", "dock.undo.move", "dock.undo.remove_part", "dock.undo.remove_kit",
    "dock.undo.label", "dock.undo.load", "dock.undo.kit_setting", "dock.undo.kit_migrate", "dock.undo.route_pin", "dock.undo.data_link",
    "dock.undo.breaker", "dock.ghost.overlap", "dock.ghost.outside", "dock.ghost.weather", "dock.ghost.internal", "dock.ghost.underwater",
    "dock.ghost.deck_edge", "dock.ghost.limit", "dock.ghost.floating", "dock.ghost.unknown", "dock.ghost.no_parts", "dock.ghost.no_room",
];

/// Where player kits live (`capture_prefab` writes them, the tray lists them).
pub(crate) const PLAYER_KIT_DIR: &str = "user://blueprints/kits";
/// Most player kit files read.
const MAX_PLAYER_KITS: usize = 512;
/// Most characters of a part label.
const MAX_LABEL_CHARS: usize = 32;

/// Per-revision caches and the paste pose.
#[derive(Default)]
pub(crate) struct PartsCache {
    /// Turn of the pasted clipboard (R, Shift+R, T).
    pub(crate) paste_rot: PartRot,
    /// Validation of the design under edit, by revision.
    base: std::cell::RefCell<Option<(u64, Rc<Vec<Issue>>)>>,
    /// Ghost verdicts by (revision, ghost key).
    verdicts: std::cell::RefCell<BTreeMap<(u64, String), Rc<Verdict>>>,
    /// Player kits from [`PLAYER_KIT_DIR`], read on demand.
    player: std::cell::RefCell<Option<Rc<Vec<KitDef>>>>,
    /// The hull envelope of the design under edit, by revision (the settling ghost's cheap first check).
    envelope: std::cell::RefCell<Option<(u64, Rc<Envelope>)>>,
    /// Networks the last placement joined and the revision its routes arrive with (`conduits_routed`).
    pub(crate) routed: Option<(u64, Vec<NetworkKind>)>,
    /// Tutorial kit spots resolved to a valid ghost cell, by (revision, kit id); `None` = nothing fits near it.
    spots: std::cell::RefCell<BTreeMap<(u64, String), Option<CellPos>>>,
}
impl PartsCache {
    pub(crate) fn invalidate(&mut self) {
        *self.base.get_mut() = None;
        self.verdicts.get_mut().clear();
        *self.envelope.get_mut() = None;
        self.spots.get_mut().clear();
    }
    pub(crate) fn forget_player_kits(&mut self) { *self.player.get_mut() = None; }
}

// ---------------------------------------------------------------------------------------------------------
// Footprints and poses.

/// Footprint cells of a part at a pose; cells outside the cell frame are left out.
pub(crate) fn footprint_cells(def: &PartDef, pos: CellPos, rot: PartRot) -> Vec<CellPos> {
    let size = def.size;
    let mut out = Vec::with_capacity(size[0] as usize * size[1] as usize * size[2] as usize);
    for x in 0..size[0] { for y in 0..size[1] { for z in 0..size[2] {
        let o = content::kit::rotate_offset(CellPos::new(x as i16, y as i16, z as i16), size, rot);
        let c = [pos.x as i32 + o.x as i32, pos.y as i32 + o.y as i32, pos.z as i32 + o.z as i32];
        if c.iter().all(|v| (0..MAX_GRID_DIM as i32).contains(v)) { out.push(CellPos::new(c[0] as i16, c[1] as i16, c[2] as i16)); }
    }}}
    out
}
/// The footprint when all of it lies inside the frame.
pub(crate) fn footprint_in_frame(def: &PartDef, pos: CellPos, rot: PartRot) -> Option<Vec<CellPos>> {
    let n = def.size.iter().map(|&s| s as usize).product::<usize>();
    let cells = footprint_cells(def, pos, rot);
    (n > 0 && cells.len() == n).then_some(cells)
}

/// Where a new ghost starts: on the weather deck amidships.
pub(crate) fn default_ghost_cell(bp: &Blueprint) -> CellPos {
    let g = content::cell::CellGrid::from_rows(&bp.cells);
    let (x, z) = (g.size[0] as i16 / 2, g.size[2] as i16 / 2);
    let top = (0..g.size[1] as i16).rev().find(|&y| !g.get(CellPos::new(x, y, z)).is_empty()).map_or(0, |y| y + 1);
    CellPos::new(x, top, z)
}

type Mat = [[i8; 3]; 3];
const MIRROR_Z: Mat = [[1, 0, 0], [0, 1, 0], [0, 0, -1]];
fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut o = [[0i8; 3]; 3];
    for i in 0..3 { for j in 0..3 { o[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum(); } }
    o
}
/// The rotation a part takes in its mirror image across the centreline.
fn mirrored_rot(rot: PartRot) -> PartRot {
    let m = content::kit::part_rot_matrix(rot);
    content::kit::part_rot_from_matrix(mul(&mul(&MIRROR_Z, &m), &MIRROR_Z)).unwrap_or(rot)
}
fn turned(rot: PartRot, quarter_turns: i64) -> PartRot { PartRot { yaw: ((rot.yaw as i64 + quarter_turns).rem_euclid(4)) as u8, tilt: rot.tilt % 6 } }

/// Bounding box of cells.
fn bounds(cells: &[CellPos]) -> Option<([i32; 3], [i32; 3])> {
    let mut lo = [i32::MAX; 3];
    let mut hi = [i32::MIN; 3];
    for c in cells {
        for (k, v) in [c.x, c.y, c.z].into_iter().enumerate() { lo[k] = lo[k].min(v as i32); hi[k] = hi[k].max(v as i32); }
    }
    (lo[0] != i32::MAX).then_some((lo, hi))
}
fn cell_of(c: [i32; 3]) -> Option<CellPos> { c.iter().all(|v| (0..MAX_GRID_DIM as i32).contains(v)).then(|| CellPos::new(c[0] as i16, c[1] as i16, c[2] as i16)) }

/// Something the ghost places: one part, a kit, or the clipboard.
#[derive(Clone, Debug)]
pub(crate) enum Body { Part(PartDef), Kit(KitDef), Paste(KitDef) }

/// One placed copy of a body.
#[derive(Clone, Debug, PartialEq)]
struct Copy { pos: CellPos, rot: PartRot, mirrored: bool, cells: Vec<CellPos> }

fn body_cells(body: &Body, cat: &Catalogue, pos: CellPos, rot: PartRot, mirrored: bool) -> Vec<CellPos> {
    match body {
        Body::Part(def) => {
            let mut c = footprint_cells(def, pos, rot);
            c.sort();
            c
        }
        Body::Kit(k) | Body::Paste(k) => content::kit::kit_footprint(k, cat, pos, rot, mirrored),
    }
}

/// The kit-frame cell (placed at the origin with `rot`) of the socket a kit stands on: the first socket facing
/// down after the turn, `deck` before the others.
fn standing_socket(body: &Body, cat: &Catalogue, rot: PartRot) -> Option<CellPos> {
    let (Body::Kit(k) | Body::Paste(k)) = body else { return None };
    let sockets = content::kit::kit_sockets_at(k, cat, CellPos::new(0, 0, 0), rot, false);
    let down = |s: &&content::kit::PlacedSocket| s.face == content::cell::Face::YNeg;
    sockets.iter().filter(down).find(|s| s.name == "deck").or_else(|| sockets.iter().find(down)).map(|s| s.cell)
}

/// The pose for a placement at `cell`: a kit's standing socket on `cell`, or else the footprint centred on it
/// across X and Z and standing on it.
fn anchored(body: &Body, cat: &Catalogue, cell: CellPos, rot: PartRot) -> Option<Copy> {
    let probe = body_cells(body, cat, CellPos::new(0, 0, 0), rot, false);
    let (lo, hi) = bounds(&probe)?;
    let pos = match standing_socket(body, cat, rot) {
        Some(s) => cell_of([cell.x as i32 - s.x as i32, cell.y as i32 - s.y as i32, cell.z as i32 - s.z as i32])?,
        None => {
            let ext = [hi[0] - lo[0] + 1, hi[2] - lo[2] + 1];
            cell_of([cell.x as i32 - ext[0] / 2, cell.y as i32, cell.z as i32 - ext[1] / 2])?
        }
    };
    let cells = body_cells(body, cat, pos, rot, false);
    (cells.len() == probe.len()).then_some(Copy { pos, rot, mirrored: false, cells })
}

/// Does this body belong in the hull: an internal or underwater part, or a kit whose parts are internal,
/// underwater or go anywhere, with at least one internal or underwater part (an engine, a fuel tank, a
/// waterjet drive). Mounts and masts, which need the weather deck, stand where they are pointed.
fn settles_inside(body: &Body, cat: &Catalogue) -> bool {
    let low = |p: &Placement| matches!(p, Placement::Internal | Placement::Underwater);
    match body {
        Body::Part(d) => low(&d.placement),
        Body::Kit(k) | Body::Paste(k) => {
            let places: Vec<Placement> = k.parts.iter().filter_map(|p| cat.part(&p.part)).map(|d| d.placement).collect();
            places.iter().any(low) && places.iter().all(|p| low(p) || *p == Placement::Anywhere)
        }
    }
}

/// The cells a settling body tries below `cell`, lowest first: the rows from the bottom of the grid up to the
/// row under `cell`.
fn settle_rows(cell: CellPos) -> impl Iterator<Item = CellPos> {
    (0..cell.y.max(0)).map(move |y| CellPos::new(cell.x, y, cell.z))
}

/// Where the hull's cells are, row by row: the loose form of `content::validate`'s envelope (any stored cell
/// counts, so it is never tighter). A pose with a cell outside it cannot be internal, which rules most of a
/// settling column out without validating the whole design for each row (13 ms a row on a 90 m hull).
pub(crate) struct Envelope {
    size: [usize; 3],
    /// Per (y, z): lowest and highest x with a cell.
    x_span: Vec<(i32, i32)>,
    /// Per (y, x): lowest and highest z with a cell.
    z_span: Vec<(i32, i32)>,
    /// Per (z, x): lowest y with a cell.
    y_low: Vec<i32>,
    /// Cells placed parts take.
    taken: BTreeSet<CellPos>,
}
impl Envelope {
    pub(crate) fn of(bp: &Blueprint) -> Envelope {
        let g = content::cell::CellGrid::from_rows(&bp.cells);
        let size = g.size.map(|v| v as usize);
        let [sx, sy, sz] = size;
        let (mut x_span, mut z_span, mut y_low) = (vec![(i32::MAX, i32::MIN); sy * sz], vec![(i32::MAX, i32::MIN); sy * sx], vec![i32::MAX; sz * sx]);
        for y in 0..sy {
            for z in 0..sz {
                for x in 0..sx {
                    if g.get(CellPos::new(x as i16, y as i16, z as i16)).is_empty() { continue; }
                    let (xi, yi, zi) = (x as i32, y as i32, z as i32);
                    let a = &mut x_span[y * sz + z];
                    *a = (a.0.min(xi), a.1.max(xi));
                    let b = &mut z_span[y * sx + x];
                    *b = (b.0.min(zi), b.1.max(zi));
                    let c = &mut y_low[z * sx + x];
                    *c = (*c).min(yi);
                }
            }
        }
        let cat = crate::services::services().catalogue();
        let taken = bp.parts.iter().filter_map(|p| cat.part(&p.part).and_then(|d| footprint_in_frame(d, p.pos, p.rot))).flatten().collect();
        Envelope { size, x_span, z_span, y_low, taken }
    }
    /// Can this cell be inside the hull: cells on both sides of it along X and Z on its row, and below it.
    fn may_hold(&self, c: CellPos) -> bool {
        let [sx, sy, sz] = self.size;
        let (x, y, z) = (c.x as i32, c.y as i32, c.z as i32);
        if x < 0 || y < 0 || z < 0 || x as usize >= sx || y as usize >= sy || z as usize >= sz { return false; }
        let (xs, zs) = (self.x_span[y as usize * sz + z as usize], self.z_span[y as usize * sx + x as usize]);
        xs.0 < x && x < xs.1 && zs.0 < z && z < zs.1 && self.y_low[z as usize * sx + x as usize] < y
    }
}

/// Could `body` be placed at `cell`: no placed part in the way, and its internal parts' cells all within the
/// envelope (an underwater part may stand proud of the plating, a rudder below it).
fn may_settle(body: &Body, cat: &Catalogue, env: &Envelope, cell: CellPos, rot: PartRot) -> bool {
    let Some(first) = anchored(body, cat, cell, rot) else { return false };
    if first.cells.iter().any(|c| env.taken.contains(c)) { return false; }
    match body {
        Body::Part(d) => d.placement != Placement::Internal || first.cells.iter().all(|c| env.may_hold(*c)),
        Body::Kit(k) | Body::Paste(k) => content::kit::kit_part_placements(k, cat, first.pos, first.rot, false)
            .iter()
            .filter(|p| cat.part(&p.part).is_some_and(|d| d.placement == Placement::Internal))
            .all(|p| p.cells.iter().all(|c| env.may_hold(*c))),
    }
}

/// The symmetry copies of a placed copy (not including it), in placing order.
fn symmetry_copies(body: &Body, cat: &Catalogue, base: &Copy, symmetry: Symmetry, radial: u8, bp: &Blueprint) -> Vec<Copy> {
    let mut out = Vec::new();
    match symmetry {
        Symmetry::None => {}
        Symmetry::Mirror => {
            let Some(s) = super::mirror_sum(bp) else { return out };
            let target: Vec<CellPos> = { let mut v: Vec<CellPos> = base.cells.iter().filter_map(|c| cell_of([c.x as i32, c.y as i32, s - c.z as i32])).collect(); v.sort(); v };
            if target.len() != base.cells.len() { return out; }
            let Some((lo, _)) = bounds(&target) else { return out };
            let Some(pos) = cell_of(lo) else { return out };
            let (rot, mirrored) = match body { Body::Part(_) => (mirrored_rot(base.rot), false), _ => (mirrored_rot(base.rot), !base.mirrored) };
            let cells = body_cells(body, cat, pos, rot, mirrored);
            if cells == target { out.push(Copy { pos, rot, mirrored, cells }); }
        }
        Symmetry::Radial => {
            let (Some(sx), Some(sz)) = (super::x_sum(bp), super::mirror_sum(bp)) else { return out };
            let turns: &[i64] = if radial >= 4 { &[1, 2, 3] } else { &[2] };
            for &k in turns {
                // Turn every cell centre about the hull's vertical centre line by k quarter turns (+X to -Z).
                let turn = |c: &CellPos| -> Option<CellPos> {
                    let (u, v) = (2 * c.x as i32 - sx, 2 * c.z as i32 - sz);
                    let (mut u2, mut v2) = (u, v);
                    for _ in 0..k { let (a, b) = (v2, -u2); u2 = a; v2 = b; }
                    // Back to cells; an odd sum on one axis only leaves a half-cell, rounded down.
                    cell_of([(u2 + sx).div_euclid(2), c.y as i32, (v2 + sz).div_euclid(2)])
                };
                let target: Vec<CellPos> = { let mut v: Vec<CellPos> = base.cells.iter().filter_map(turn).collect(); v.sort(); v.dedup(); v };
                if target.len() != base.cells.len() { continue; }
                let Some((lo, _)) = bounds(&target) else { continue };
                let Some(pos) = cell_of(lo) else { continue };
                let rot = turned(base.rot, k);
                let cells = body_cells(body, cat, pos, rot, base.mirrored);
                if cells == target { out.push(Copy { pos, rot, mirrored: base.mirrored, cells }); }
            }
        }
    }
    out
}

/// The edits that place a body at its copies.
fn place_edits(body: &Body, cat: &Catalogue, copies: &[Copy]) -> Vec<BlueprintEdit> {
    let mut out = Vec::new();
    for c in copies {
        match body {
            Body::Part(def) => out.push(BlueprintEdit::AddPart(PartPlacement { part: def.id.clone(), pos: c.pos, rot: c.rot, kit: None, label: None, load: None })),
            Body::Kit(k) => out.push(BlueprintEdit::AddKit { kit: k.id.clone(), pos: c.pos, rot: c.rot, mirrored: c.mirrored }),
            Body::Paste(k) => {
                for p in content::kit::kit_part_placements(k, cat, c.pos, c.rot, c.mirrored) {
                    out.push(BlueprintEdit::AddPart(PartPlacement { part: p.part, pos: p.pos, rot: p.rot, kit: None, label: None, load: None }));
                }
            }
        }
    }
    out
}

/// A placement check: the candidate design, or why there is none.
#[derive(Clone, Debug)]
pub(crate) struct Verdict {
    pub valid: bool,
    pub reason: Msg,
    pub cells: Vec<CellPos>,
    pub cost: i64,
    /// The design after placing, when valid.
    pub next: Option<Blueprint>,
    /// Part slots the placement adds.
    pub new_slots: std::ops::Range<usize>,
}

/// The ruleset, availability and mode a verdict is checked under.
pub(crate) struct Rules<'a> { pub ruleset: &'a content::ruleset::Ruleset, pub available: Option<&'a [PartId]>, pub enemy_class: bool }

fn validate(bp: &Blueprint, cat: &Catalogue, rules: &Rules<'_>) -> Vec<Issue> {
    let ctx = content::validate::ValidationContext { ruleset: rules.ruleset, available: rules.available, enemy_class: rules.enemy_class, complexity: None };
    content::validate::validate_blueprint(bp, cat, &ctx).issues
}

/// Problems the candidate has that the base does not, touching the placed slots, as the refusal message.
pub(crate) fn new_problem(base: &[Issue], cand: &[Issue], new: &std::ops::Range<usize>, next: &Blueprint, cat: &Catalogue) -> Option<Msg> {
    let mine = |s: &PartSlot| new.contains(&s.index());
    let name = |part: &PartId| cat.part(part).map_or(MsgArg::Name(part.as_str().to_string()), |d| MsgArg::Key(d.name.clone()));
    for i in cand {
        if base.contains(i) { continue; }
        let msg = match i {
            Issue::PartOutOfBounds { slot } if mine(slot) => Msg::new("dock.ghost.outside"),
            Issue::PartsOverlap { a, b } if mine(a) || mine(b) => Msg::new("dock.ghost.overlap"),
            Issue::PartBadPlacement { slot } if mine(slot) => {
                let place = next.parts.get(slot.index()).and_then(|p| cat.part(&p.part)).map_or(Placement::Anywhere, |d| d.placement);
                Msg::new(match place { Placement::Weather => "dock.ghost.weather", Placement::Internal => "dock.ghost.internal", Placement::Underwater => "dock.ghost.underwater", Placement::DeckEdge => "dock.ghost.deck_edge", Placement::Anywhere => "dock.ghost.overlap" })
            }
            Issue::UnknownPart { .. } => Msg::new("dock.ghost.unknown"),
            Issue::PartNotAvailable { part } if new.clone().any(|s| next.parts.get(s).is_some_and(|p| &p.part == part)) => {
                cat.part(part).map(|d| crate::content_api::model::part_access(d, false, content::enums::Act::Act4, None).err().unwrap_or_else(|| Msg::new("content.locked.blueprint"))).unwrap_or_else(|| Msg::new("dock.ghost.unknown"))
            }
            Issue::PartOverLimit { part, count, limit } if new.clone().any(|s| next.parts.get(s).is_some_and(|p| &p.part == part)) => Msg::new("dock.ghost.limit").arg(name(part)).arg(MsgArg::Int(*count as i64)).arg(MsgArg::Int(*limit as i64)),
            Issue::Disconnected { .. } if !base.iter().any(|b| matches!(b, Issue::Disconnected { .. })) => Msg::new("dock.ghost.floating"),
            _ => continue,
        };
        return Some(msg);
    }
    None
}

/// Check placing `body` at `cell` with `rot` and the dock's symmetry. Pure: used by the ghost and by tests.
#[allow(clippy::too_many_arguments)]
pub(crate) fn verdict(bp: &Blueprint, base: &[Issue], body: &Body, cell: CellPos, rot: PartRot, symmetry: Symmetry, radial: u8, cat: &Catalogue, rules: &Rules<'_>) -> Verdict {
    let refuse = |reason: Msg, cells: Vec<CellPos>, cost: i64| Verdict { valid: false, reason, cells, cost, next: None, new_slots: 0..0 };
    let cost_of = |n: usize| -> i64 {
        let one: i64 = match body {
            Body::Part(d) => d.cost as i64,
            Body::Kit(k) | Body::Paste(k) => k.parts.iter().filter_map(|p| cat.part(&p.part)).map(|d| d.cost as i64).sum(),
        };
        one * n as i64
    };
    if let Body::Kit(k) | Body::Paste(k) = body {
        if k.kind != KitKind::Assembly || k.parts.is_empty() { return refuse(Msg::new("dock.ghost.no_parts"), Vec::new(), 0); }
    }
    let Some(first) = anchored(body, cat, cell, rot) else { return refuse(Msg::new("dock.ghost.outside"), Vec::new(), cost_of(1)) };
    let mut copies = vec![first.clone()];
    for c in symmetry_copies(body, cat, &first, symmetry, radial, bp) {
        // A copy on top of one already placed (a centreline part, a centred radial part) is dropped.
        if copies.iter().any(|o| o.cells.iter().any(|x| c.cells.binary_search(x).is_ok())) { continue; }
        copies.push(c);
    }
    let cells: Vec<CellPos> = copies.iter().flat_map(|c| c.cells.iter().copied()).collect();
    let cost = cost_of(copies.len());
    let mut start = bp.clone();
    if let Body::Kit(k) = body {
        // A player kit travels with the design that uses it (`Blueprint::embedded_kits`).
        if cat.kit(&k.id) != Some(k) { start.embedded_kits.insert(k.id.clone(), k.clone()); }
    }
    let edits = place_edits(body, cat, &copies);
    let edit = if edits.len() == 1 { edits.into_iter().next().expect("one edit") } else { BlueprintEdit::Several(edits) };
    let Some(next) = sim_systems::editor::notes::apply_edit(&start, &edit, cat) else {
        // apply_edit refuses overlaps with placed parts and footprints leaving the frame.
        let outside = match body { Body::Part(d) => copies.iter().any(|c| footprint_in_frame(d, c.pos, c.rot).is_none()), _ => false };
        return refuse(Msg::new(if outside { "dock.ghost.outside" } else { "dock.ghost.overlap" }), cells, cost);
    };
    let new_slots = bp.parts.len()..next.parts.len();
    let cand = validate(&next, cat, rules);
    match new_problem(base, &cand, &new_slots, &next, cat) {
        Some(reason) => refuse(reason, cells, cost),
        None => Verdict { valid: true, reason: Msg::default(), cells, cost, next: Some(next), new_slots },
    }
}

fn ghost_dict(v: &Verdict, cell: CellPos, rot: PartRot) -> VarDictionary {
    let cells: PackedVector3Array = v.cells.iter().map(|c| Vector3::new(c.x as f32, c.y as f32, c.z as f32)).collect();
    d! {
        "valid" => v.valid,
        "reason" => if v.valid { empty_msg() } else { msg_dict(&v.reason) },
        "cell" => Vector3i::new(cell.x as i32, cell.y as i32, cell.z as i32),
        "yaw" => rot.yaw as i64,
        "tilt" => rot.tilt as i64,
        "cells" => cells,
        "cost" => v.cost,
    }
}

fn v3i(c: CellPos) -> Vector3i { Vector3i::new(c.x as i32, c.y as i32, c.z as i32) }
fn cell_from(v: Vector3i) -> Option<CellPos> { cell_of([v.x, v.y, v.z]) }
fn rot_from(yaw: i64, tilt: i64) -> PartRot { PartRot { yaw: yaw.rem_euclid(4) as u8, tilt: tilt.rem_euclid(6) as u8 } }

/// Load fraction of a placed part, or -1 when it carries none.
fn load_fraction(p: &PartPlacement, def: &PartDef) -> f64 {
    use content::part::PartFunction as F;
    match &def.function {
        F::FuelTank { .. } | F::Magazine { .. } => p.load.filter(|v| v.is_finite()).map_or(1.0, |v| v.clamp(0.0, 1.0)),
        F::Ballast { max_kg } => {
            let max = if max_kg.is_finite() && *max_kg > 0.0 { *max_kg } else { return 0.0 };
            p.load.filter(|v| v.is_finite()).map_or(1.0, |kg| (kg / max).clamp(0.0, 1.0))
        }
        _ => -1.0,
    }
}

/// For each part, the slot of the part it mirrors across the centreline, if one does.
fn mirror_pairs(bp: &Blueprint, cat: &Catalogue) -> Vec<i64> {
    let Some(s) = super::mirror_sum(bp) else { return vec![-1; bp.parts.len()] };
    let mut by_cells: BTreeMap<(PartId, Vec<CellPos>), usize> = BTreeMap::new();
    let mut fps = Vec::with_capacity(bp.parts.len());
    for (i, p) in bp.parts.iter().enumerate() {
        let mut c = cat.part(&p.part).map(|d| footprint_cells(d, p.pos, p.rot)).unwrap_or_default();
        c.sort();
        by_cells.entry((p.part.clone(), c.clone())).or_insert(i);
        fps.push(c);
    }
    bp.parts.iter().enumerate().map(|(i, p)| {
        let mut m: Vec<CellPos> = fps[i].iter().filter_map(|c| cell_of([c.x as i32, c.y as i32, s - c.z as i32])).collect();
        m.sort();
        if m.is_empty() || m == fps[i] { return -1; }
        by_cells.get(&(p.part.clone(), m)).filter(|j| **j != i).map_or(-1, |j| *j as i64)
    }).collect()
}

/// The newest definition of a kit: the catalogue's for a stock kit, else the player's file, else the copy the
/// design carries.
fn latest_kit(id: &KitId, bp: &Blueprint, cat: &Catalogue, player: &[KitDef]) -> Option<KitDef> {
    cat.kit(id).cloned().or_else(|| player.iter().find(|k| &k.id == id).cloned()).or_else(|| bp.embedded_kits.get(id).cloned())
}

/// Read the player's kits.
fn read_player_kits() -> Vec<KitDef> {
    use godot::classes::{DirAccess, FileAccess};
    if !DirAccess::dir_exists_absolute(PLAYER_KIT_DIR) { return Vec::new(); }
    let mut files: Vec<String> = DirAccess::get_files_at(PLAYER_KIT_DIR).as_slice().iter().map(|f| f.to_string()).filter(|f| f.ends_with(".kit.ron")).collect();
    files.sort();
    files.truncate(MAX_PLAYER_KITS);
    let mut out = Vec::new();
    for f in files {
        let path = format!("{PLAYER_KIT_DIR}/{f}");
        let text = FileAccess::get_file_as_string(&GString::from(path.as_str())).to_string();
        if text.is_empty() || text.len() > content::blueprint::MAX_RON_BYTES { continue; }
        if let Ok(k) = content::kit::parse_kit(&text, &f) { out.push(k); }
    }
    out
}

/// How far from a tutorial kit spot (cells along, up or down, and across) a valid pose is looked for, and the
/// most poses checked; each check validates the design once.
const SPOT_REACH: i32 = 10;
const SPOT_DEPTH: i32 = 6;
const SPOT_SIDE: i32 = 5;
const MAX_SPOT_CHECKS: usize = 400;

impl BpEditor {
    /// The tutorial's marked place for a lesson kit (DESIGN.md L257: drop four kits from the tray and watch the
    /// conduits route themselves). In a tutorial dock, while the running step waits on kits, a kit whose family
    /// has a spot (`Services::tutorial_spots`) settles on the valid pose nearest that spot wherever it is moved
    /// over the hull, so a newcomer never has to find the one column inside the hull where an engine fits.
    /// `None` outside that case, or when nothing near the spot fits (the ghost then follows the cursor as usual).
    fn tutorial_spot(&self, kit: &KitId) -> Option<CellPos> {
        let m = self.model.as_ref()?;
        if m.context.mode != EditorMode::Tutorial { return None; }
        let spot = crate::services::services().tutorial_spots.borrow().iter().find(|s| sim_systems::tutorial::kit_matches(&s.kit, kit)).cloned()?;
        let key = (m.rev, kit.as_str().to_string());
        if let Some(hit) = self.state.parts.spots.borrow().get(&key) { return *hit; }
        let body = Body::Kit(self.kit_def(kit)?);
        let size = content::cell::CellGrid::from_rows(&m.blueprint.cells).size;
        let want = [spot.x as i32, spot.y as i32, size[2] as i32 / 2 + spot.dz as i32];
        let mut cands: Vec<[i32; 3]> = Vec::new();
        for dx in -SPOT_REACH..=SPOT_REACH {
            for dy in -SPOT_DEPTH..=SPOT_DEPTH {
                for dz in -SPOT_SIDE..=SPOT_SIDE { cands.push([want[0] + dx, want[1] + dy, want[2] + dz]); }
            }
        }
        let d2 = |c: &[i32; 3]| (0..3).map(|k| (c[k] - want[k]) * (c[k] - want[k])).sum::<i32>();
        cands.sort_by_key(|c| (d2(c), *c));
        let found = cands.into_iter().filter_map(cell_of).take(MAX_SPOT_CHECKS).find(|c| self.check(&body, *c, PartRot::default()).is_some_and(|v| v.valid));
        self.state.parts.spots.borrow_mut().insert(key, found);
        found
    }
    fn player_kits(&self) -> Rc<Vec<KitDef>> {
        if let Some(k) = self.state.parts.player.borrow().as_ref() { return k.clone(); }
        let k = Rc::new(read_player_kits());
        *self.state.parts.player.borrow_mut() = Some(k.clone());
        k
    }
    fn kit_def(&self, id: &KitId) -> Option<KitDef> {
        let m = self.model.as_ref()?;
        let cat = crate::services::services().catalogue();
        m.blueprint.embedded_kits.get(id).cloned().or_else(|| cat.kit(id).cloned()).or_else(|| self.player_kits().iter().find(|k| &k.id == id).cloned())
    }
    fn base_issues(&self) -> Rc<Vec<Issue>> {
        let Some(m) = self.model.as_ref() else { return Rc::new(Vec::new()) };
        if let Some((r, v)) = self.state.parts.base.borrow().as_ref() {
            if *r == m.rev { return v.clone(); }
        }
        let cat = crate::services::services().catalogue();
        let rules = Rules { ruleset: &m.context.ruleset, available: m.context.available.as_deref().map(|v| v.as_slice()), enemy_class: m.context.mode == EditorMode::EnemyClass };
        let v = Rc::new(validate(&m.blueprint, &cat, &rules));
        *self.state.parts.base.borrow_mut() = Some((m.rev, v.clone()));
        v
    }
    /// The body and pose a ghost places.
    fn ghost_body(&self) -> Option<(Body, CellPos, PartRot)> {
        let m = self.model.as_ref()?;
        let cat = crate::services::services().catalogue();
        match m.ghost.as_ref()? {
            Ghost::Part { part, cell, rot } => Some((Body::Part(cat.part(part)?.clone()), *cell, *rot)),
            Ghost::Kit { kit, cell, rot } => Some((Body::Kit(self.kit_def(kit)?), *cell, *rot)),
            Ghost::Paste { offset } => Some((Body::Paste((**m.clipboard.as_ref()?).clone()), *offset, self.state.parts.paste_rot)),
            Ghost::Edit(_) => None,
        }
    }
    /// The verdict for a body at a pose, cached per revision.
    fn check(&self, body: &Body, cell: CellPos, rot: PartRot) -> Option<Rc<Verdict>> {
        let m = self.model.as_ref()?;
        let id = match body { Body::Part(d) => format!("p:{}", d.id), Body::Kit(k) => format!("k:{}:{}", k.id, k.version), Body::Paste(k) => format!("c:{}", content::kit::kit_hash(k).to_hex()) };
        let key = (m.rev, format!("{id}@{},{},{}/{},{}/{:?}{}", cell.x, cell.y, cell.z, rot.yaw, rot.tilt, m.symmetry, m.radial_count));
        if let Some(v) = self.state.parts.verdicts.borrow().get(&key) { return Some(v.clone()); }
        let cat = crate::services::services().catalogue();
        let base = self.base_issues();
        let rules = Rules { ruleset: &m.context.ruleset, available: m.context.available.as_deref().map(|v| v.as_slice()), enemy_class: m.context.mode == EditorMode::EnemyClass };
        let v = Rc::new(verdict(&m.blueprint, &base, body, cell, rot, m.symmetry, m.radial_count, &cat, &rules));
        let mut cache = self.state.parts.verdicts.borrow_mut();
        if cache.len() > 256 { cache.clear(); }
        cache.insert(key, v.clone());
        Some(v)
    }
    /// The ghost's verdict and the pose it shows: where the cursor points, or for a body that belongs inside
    /// the hull and cannot go there, the lowest valid pose in the column below (module doc).
    pub(crate) fn ghost_verdict(&self) -> Option<(Rc<Verdict>, CellPos, PartRot)> {
        let (body, cell, rot) = self.ghost_body()?;
        let here = self.check(&body, cell, rot)?;
        let cat = crate::services::services().catalogue();
        if here.valid || !settles_inside(&body, &cat) {
            return Some((here, cell, rot));
        }
        let env = self.envelope()?;
        for below in settle_rows(cell).filter(|c| may_settle(&body, &cat, &env, *c, rot)) {
            let v = self.check(&body, below, rot)?;
            if v.valid { return Some((v, below, rot)); }
        }
        // Nothing in the column takes it: the hull is too narrow or too shallow here, or full.
        if matches!(here.reason.key.as_str(), "dock.ghost.internal" | "dock.ghost.underwater") {
            let mut v = (*here).clone();
            v.reason = Msg::new("dock.ghost.no_room");
            return Some((Rc::new(v), cell, rot));
        }
        Some((here, cell, rot))
    }
    fn envelope(&self) -> Option<Rc<Envelope>> {
        let m = self.model.as_ref()?;
        if let Some((rev, e)) = self.state.parts.envelope.borrow().as_ref() {
            if *rev == m.rev { return Some(e.clone()); }
        }
        let e = Rc::new(Envelope::of(&m.blueprint));
        *self.state.parts.envelope.borrow_mut() = Some((m.rev, e.clone()));
        Some(e)
    }
    /// Can a part be offered here at all: in the build, available to this dock.
    fn part_offered(&self, def: &PartDef) -> Result<(), Msg> {
        let m = self.model.as_ref().ok_or_else(|| Msg::new("content.locked.no_campaign"))?;
        if let Some(av) = m.context.available.as_ref() {
            let unlocks = crate::content_api::model::Unlocks { max_tier: content::enums::Tier::T5, unlocked: av.iter().cloned().collect(), store: (*m.context.parts_store).clone() };
            crate::content_api::model::part_access(def, true, m.context.act, Some(&unlocks))
        } else {
            crate::content_api::model::part_access(def, false, m.context.act, None)
        }
    }
    fn kit_offered(&self, kit: &KitDef, cat: &Catalogue) -> Result<(), Msg> {
        if kit.kind == KitKind::Officer || kit.parts.is_empty() { return Err(Msg::new("dock.ghost.no_parts")); }
        for kp in &kit.parts {
            let Some(def) = cat.part(&kp.part) else { return Err(Msg::new("content.locked.unknown_part")) };
            self.part_offered(def)?;
        }
        Ok(())
    }
    fn start_ghost(&mut self, ghost: Ghost, tool: EditorTool) {
        let Some(m) = self.model.as_mut() else { return };
        m.ghost = Some(ghost);
        let changed_tool = m.tool != tool;
        m.tool = tool;
        if changed_tool {
            let t = tool.to_gd();
            self.base_mut().emit_signal("tool_changed", &[t.to_variant()]);
        }
        self.base_mut().emit_signal("ghost_changed", &[]);
    }
    /// Commit a moved design after checking the moved slots still validate.
    fn commit_moved(&mut self, next: Blueprint, moved: &BTreeSet<usize>) -> bool {
        let cat = crate::services::services().catalogue();
        let Some(m) = self.model.as_ref() else { return false };
        // Overlaps and the frame first, cheaply.
        let mut seen: BTreeMap<CellPos, usize> = BTreeMap::new();
        for (i, p) in next.parts.iter().enumerate() {
            let Some(def) = cat.part(&p.part) else { continue };
            let Some(cells) = footprint_in_frame(def, p.pos, p.rot) else { if moved.contains(&i) { return false; } continue };
            for c in cells { if let Some(j) = seen.insert(c, i) { if moved.contains(&i) || moved.contains(&j) { return false; } } }
        }
        let rules = Rules { ruleset: &m.context.ruleset, available: m.context.available.as_deref().map(|v| v.as_slice()), enemy_class: m.context.mode == EditorMode::EnemyClass };
        let base = self.base_issues();
        let cand = validate(&next, &cat, &rules);
        for i in &cand {
            if base.contains(i) { continue; }
            let bad = match i {
                Issue::PartOutOfBounds { slot } | Issue::PartBadPlacement { slot } => moved.contains(&slot.index()),
                Issue::PartsOverlap { a, b } => moved.contains(&a.index()) || moved.contains(&b.index()),
                Issue::Disconnected { .. } => !base.iter().any(|b| matches!(b, Issue::Disconnected { .. })),
                _ => false,
            };
            if bad { return false; }
        }
        self.commit(next, "dock.undo.move")
    }
    /// After a placement: remember which networks it joins, so `conduits_routed` fires when the rebuilt design
    /// (with its routes) arrives.
    fn remember_routes(&mut self, slots: std::ops::Range<usize>) {
        let cat = crate::services::services().catalogue();
        let Some(m) = self.model.as_ref() else { return };
        let mut nets: BTreeSet<u8> = BTreeSet::new();
        for i in slots {
            let Some(def) = m.blueprint.parts.get(i).and_then(|p| cat.part(&p.part)) else { continue };
            for port in &def.ports { nets.insert(port.network as u8); }
        }
        let kinds: Vec<NetworkKind> = [NetworkKind::Power, NetworkKind::Fuel, NetworkKind::Data].into_iter().filter(|k| nets.contains(&(*k as u8))).collect();
        if !kinds.is_empty() { self.state.parts.routed = Some((m.rev, kinds)); }
    }
    /// Called by the core when the design for `rev` arrives: the networks whose conduits a placement routed,
    /// as `Bp.NETWORK_*`, for `conduits_routed` (L138).
    pub(crate) fn parts_design_arrived(&mut self, rev: u64) -> Vec<i64> {
        let Some((r, kinds)) = self.state.parts.routed.take() else { return Vec::new() };
        if r != rev {
            if r > rev { self.state.parts.routed = Some((r, kinds)); }
            return Vec::new();
        }
        kinds.into_iter().map(|k| k.to_gd()).collect()
    }
    /// After a design is installed: offer kit updates (L140).
    pub(crate) fn parts_design_installed(&mut self) {
        let offers: Vec<(String, i64)> = self.kit_update_list().into_iter().map(|(k, _, _, n)| (k, n)).collect();
        for (kit, copies) in offers {
            self.base_mut().emit_signal("kit_update_offered", &[StringName::from(kit.as_str()).to_variant(), copies.to_variant()]);
        }
    }
    fn kit_update_list(&self) -> Vec<(String, u32, u32, i64)> {
        let Some(m) = self.model.as_ref() else { return Vec::new() };
        let cat = crate::services::services().catalogue();
        let player = self.player_kits();
        let mut by_kit: BTreeMap<KitId, (u32, i64)> = BTreeMap::new();
        for k in &m.blueprint.kits {
            let e = by_kit.entry(k.kit.clone()).or_insert((u32::MAX, 0));
            e.0 = e.0.min(k.version);
            e.1 += 1;
        }
        by_kit.into_iter().filter_map(|(id, (placed, _))| {
            let latest = latest_kit(&id, &m.blueprint, &cat, &player)?;
            let copies = m.blueprint.kits.iter().filter(|k| k.kit == id && k.version < latest.version).count() as i64;
            (latest.version > placed && copies > 0).then(|| (id.0.clone(), placed, latest.version, copies))
        }).collect()
    }
    fn network_of(n: i64) -> Option<NetworkKind> { NetworkKind::from_gd(n) }
}

#[godot_api(secondary)]
impl BpEditor {
    /// The tray: parts and kits this dock may place. `filter` keys (optional): `category: int`
    /// (`Bp.CATEGORY_*`), `kits_only: bool`, `parts_only: bool`, `search: String`, `tier: int`.
    /// Returns `Array[Dictionary]`: each a `PartDefDict` or `KitDefDict` with an extra `kind: StringName`
    /// (`&"part"` or `&"kit"`) and `placed: int` (copies in the design).
    ///
    /// Parts first (by category, tier, name), then stock kits in tray order, then the player's kits and the
    /// kits the design carries from elsewhere, by name.
    /// Parts the dock may not place are listed with `available` false and their reason.
    #[func]
    fn tray(&self, filter: VarDictionary) -> Array<VarDictionary> {
        use crate::content_api::{dicts, model};
        let Some(m) = self.model.as_ref() else { return Array::new() };
        let cat = crate::services::services().catalogue();
        let lk = crate::text::EngineLookup::new();
        let f = model::Filter {
            mode: None,
            act: None,
            category: get_i64(&filter, "category").and_then(content::part::PartCategory::from_gd),
            tier: get_i64(&filter, "tier").and_then(content::enums::Tier::from_gd),
            tag: None,
            search: get_text(&filter, "search").map(|s| s.trim().to_lowercase()).unwrap_or_default(),
            ruleset: None,
        };
        let (kits_only, parts_only) = (get_bool(&filter, "kits_only").unwrap_or(false), get_bool(&filter, "parts_only").unwrap_or(false));
        let mut placed_parts: BTreeMap<&PartId, i64> = BTreeMap::new();
        for p in &m.blueprint.parts { *placed_parts.entry(&p.part).or_default() += 1; }
        let mut placed_kits: BTreeMap<&KitId, i64> = BTreeMap::new();
        for k in &m.blueprint.kits { *placed_kits.entry(&k.kit).or_default() += 1; }
        let mut out = Array::new();
        if !kits_only {
            for def in model::sorted_parts(&cat, &f, &lk) {
                let mut dict = dicts::part_def_dict(def, &cat);
                dicts::set_access(&mut dict, &self.part_offered(def));
                dict.set("limit", model::part_limit(Some(&m.context.ruleset), def) as i64);
                dict.set("in_store", m.context.parts_store.get(&def.id).map_or(0, |n| *n as i64));
                dict.set("kind", &StringName::from("part"));
                dict.set("placed", placed_parts.get(&def.id).copied().unwrap_or(0));
                out.push(&dict);
            }
        }
        if !parts_only {
            // `blueprints/examples/` holds the file-format sample kit, not a stock kit: in the tray it read as a
            // second, unnamed 76 mm gun above the real one, and the tutorial's gun step does not count it.
            let mut stock: Vec<&KitDef> = cat.kits.values().filter(|k| k.kind == KitKind::Assembly && !k.id.as_str().starts_with("kit.example.")).collect();
            stock.sort_by(|a, b| (a.tray_order, &a.id).cmp(&(b.tray_order, &b.id)));
            let player = self.player_kits();
            // The player's own kits, then kits only the design carries (from a shared ship), each once.
            let mut seen: BTreeSet<&KitId> = BTreeSet::new();
            let mut named: Vec<(String, &KitDef)> = player.iter().chain(m.blueprint.embedded_kits.values()).filter(|k| cat.kit(&k.id).is_none() && k.kind == KitKind::Assembly && seen.insert(&k.id)).map(|k| (model::kit_display_name(k, &lk).to_lowercase(), k)).collect();
            named.sort_by(|(an, a), (bn, b)| (an, &a.id).cmp(&(bn, &b.id)));
            let all = stock.into_iter().map(|k| (k, true)).chain(named.into_iter().map(|(_, k)| (k, false)));
            for (kit, is_stock) in all {
                let summary = model::kit_summary(kit, &cat);
                if !model::kit_matches(kit, &summary, &f, &lk) { continue; }
                let mut dict = dicts::kit_def_dict(kit, &summary, is_stock);
                dicts::set_access(&mut dict, &self.kit_offered(kit, &cat));
                dict.set("kind", &StringName::from("kit"));
                dict.set("placed", placed_kits.get(&kit.id).copied().unwrap_or(0));
                out.push(&dict);
            }
        }
        out
    }
    /// Start placing a part or kit by id: it becomes the ghost. Returns false when it may not be placed here.
    #[func]
    fn pick_part(&mut self, part: StringName) -> bool {
        let cat = crate::services::services().catalogue();
        let Some(m) = self.model.as_ref() else { return false };
        let id = PartId::new(part.to_string());
        let Some(def) = cat.part(&id) else { return false };
        if self.part_offered(def).is_err() { return false; }
        let (cell, rot) = match &m.ghost { Some(Ghost::Part { cell, rot, .. } | Ghost::Kit { cell, rot, .. }) => (*cell, *rot), Some(Ghost::Paste { offset }) => (*offset, PartRot::default()), _ => (default_ghost_cell(&m.blueprint), PartRot::default()) };
        self.start_ghost(Ghost::Part { part: id, cell, rot }, EditorTool::PlacePart);
        true
    }
    #[func]
    fn pick_kit(&mut self, kit: StringName) -> bool {
        let cat = crate::services::services().catalogue();
        let id = KitId::new(kit.to_string());
        let Some(def) = self.kit_def(&id) else { return false };
        if self.kit_offered(&def, &cat).is_err() { return false; }
        let Some(m) = self.model.as_ref() else { return false };
        let (cell, rot) = match &m.ghost { Some(Ghost::Part { cell, rot, .. } | Ghost::Kit { cell, rot, .. }) => (*cell, *rot), Some(Ghost::Paste { offset }) => (*offset, PartRot::default()), _ => (default_ghost_cell(&m.blueprint), PartRot::default()) };
        // A tutorial lesson kit shows on its marked spot from the moment it is picked.
        let (cell, rot) = self.tutorial_spot(&id).map_or((cell, rot), |c| (c, PartRot::default()));
        self.start_ghost(Ghost::Kit { kit: id, cell, rot }, EditorTool::PlaceKit);
        true
    }
    /// The current ghost as a `GhostDict`, or `null` when nothing is being placed.
    #[func]
    fn ghost(&self) -> Variant {
        match self.ghost_verdict() {
            Some((v, cell, rot)) => ghost_dict(&v, cell, rot).to_variant(),
            None => Variant::nil(),
        }
    }
    /// Move the ghost to a cell (from `BpEditorView.cell_at`). Returns the updated `GhostDict`.
    #[func]
    fn ghost_move(&mut self, cell: Vector3i) -> VarDictionary {
        let Some(c) = cell_from(cell) else { return self.ghost().try_to::<VarDictionary>().unwrap_or_default() };
        // In the tutorial a lesson kit settles on its marked spot wherever it is moved over the hull.
        let lesson_kit = match self.model.as_ref().and_then(|m| m.ghost.as_ref()) { Some(Ghost::Kit { kit, .. }) => Some(kit.clone()), _ => None };
        let snap = lesson_kit.and_then(|k| self.tutorial_spot(&k));
        let c = snap.unwrap_or(c);
        let Some(m) = self.model.as_mut() else { return VarDictionary::new() };
        if snap.is_some() {
            if let Some(Ghost::Kit { rot, .. }) = &mut m.ghost { *rot = PartRot::default(); }
        }
        let moved = match &mut m.ghost {
            Some(Ghost::Part { cell, .. } | Ghost::Kit { cell, .. }) if *cell != c => { *cell = c; true }
            Some(Ghost::Paste { offset }) if *offset != c => { *offset = c; true }
            _ => false,
        };
        let out = self.ghost().try_to::<VarDictionary>().unwrap_or_default();
        if moved { self.base_mut().emit_signal("ghost_changed", &[]); }
        out
    }
    /// Rotate the ghost by quarter turns about the vertical (+1 for R, -1 for Shift+R).
    #[func]
    fn ghost_rotate(&mut self, quarter_turns: i64) {
        let q = quarter_turns.rem_euclid(4);
        if q == 0 { return; }
        let Some(m) = self.model.as_mut() else { return };
        match &mut m.ghost {
            Some(Ghost::Part { rot, .. } | Ghost::Kit { rot, .. }) => *rot = turned(*rot, q),
            Some(Ghost::Paste { .. }) => self.state.parts.paste_rot = turned(self.state.parts.paste_rot, q),
            _ => return,
        }
        ui_sfx(UiSfx::Rotate);
        self.base_mut().emit_signal("ghost_changed", &[]);
    }
    /// Cycle the ghost's tilt (T).
    #[func]
    fn ghost_tilt(&mut self) {
        let Some(m) = self.model.as_mut() else { return };
        let tilt = |r: &mut PartRot| r.tilt = (r.tilt + 1) % content::kit::TILT_COUNT;
        match &mut m.ghost {
            Some(Ghost::Part { rot, .. } | Ghost::Kit { rot, .. }) => tilt(rot),
            Some(Ghost::Paste { .. }) => tilt(&mut self.state.parts.paste_rot),
            _ => return,
        }
        ui_sfx(UiSfx::Rotate);
        self.base_mut().emit_signal("ghost_changed", &[]);
    }
    /// Place the ghost where it is, with symmetry copies. The ghost stays for the next placement. Returns the
    /// new part slots (empty when the placement is not valid).
    #[func]
    fn ghost_place(&mut self) -> PackedInt32Array {
        let Some((v, _, _)) = self.ghost_verdict() else { return PackedInt32Array::new() };
        let Some(next) = v.next.clone().filter(|_| v.valid) else { ui_sfx(UiSfx::Invalid); return PackedInt32Array::new() };
        let Some(m) = self.model.as_ref() else { return PackedInt32Array::new() };
        let (label, signal) = match &m.ghost {
            Some(Ghost::Kit { kit, .. }) => ("dock.undo.place_kit", Some(sim_systems::tutorial::Signal::KitPlaced { kit: kit.clone() })),
            Some(Ghost::Part { part, .. }) => ("dock.undo.place_part", Some(sim_systems::tutorial::Signal::PartPlaced { part: part.clone() })),
            _ => ("dock.undo.paste", None),
        };
        // A tutorial lesson kit is placed once: placing stops after it, so a second click cannot drop a copy.
        let lesson = match &m.ghost { Some(Ghost::Kit { kit, .. }) => { let k = kit.clone(); self.tutorial_spot(&k).is_some() } _ => false };
        let slots = v.new_slots.clone();
        if !self.commit(next, label) { ui_sfx(UiSfx::Invalid); return PackedInt32Array::new(); }
        ui_sfx(UiSfx::Place);
        if let Some(s) = signal { crate::services::services().tutorial_signal(s); }
        self.remember_routes(slots.clone());
        if lesson { self.ghost_cancel(); }
        slots.map(|s| s as i32).collect()
    }
    /// Stop placing (Esc).
    #[func]
    fn ghost_cancel(&mut self) {
        let Some(m) = self.model.as_mut() else { return };
        if !matches!(m.ghost, Some(Ghost::Part { .. } | Ghost::Kit { .. } | Ghost::Paste { .. })) { return; }
        m.ghost = None;
        let tool_changed = matches!(m.tool, EditorTool::PlacePart | EditorTool::PlaceKit);
        if tool_changed { m.tool = EditorTool::Select; }
        if tool_changed { self.base_mut().emit_signal("tool_changed", &[EditorTool::Select.to_gd().to_variant()]); }
        self.base_mut().emit_signal("ghost_changed", &[]);
    }
    /// Check a placement without a ghost: returns a `GhostDict`.
    #[func]
    fn can_place(&self, id: StringName, is_kit: bool, cell: Vector3i, yaw: i64, tilt: i64) -> VarDictionary {
        let cat = crate::services::services().catalogue();
        let rot = rot_from(yaw, tilt);
        let refuse = |reason: Msg| d! { "valid" => false, "reason" => msg_dict(&reason), "cell" => cell, "yaw" => rot.yaw as i64, "tilt" => rot.tilt as i64, "cells" => PackedVector3Array::new(), "cost" => 0i64 };
        let Some(c) = cell_from(cell) else { return refuse(Msg::new("dock.ghost.outside")) };
        let body = if is_kit {
            match self.kit_def(&KitId::new(id.to_string())) { Some(k) => { if let Err(e) = self.kit_offered(&k, &cat) { return refuse(e); } Body::Kit(k) } None => return refuse(Msg::new("dock.ghost.unknown")) }
        } else {
            match cat.part(&PartId::new(id.to_string())) { Some(d) => { if let Err(e) = self.part_offered(d) { return refuse(e); } Body::Part(d.clone()) } None => return refuse(Msg::new("dock.ghost.unknown")) }
        };
        match self.check(&body, c, rot) { Some(v) => ghost_dict(&v, c, rot), None => refuse(Msg::new("dock.ghost.unknown")) }
    }
    /// Placed parts, ascending slot: `Array[PlacedPartDict]`.
    #[func]
    fn parts(&self) -> Array<VarDictionary> {
        let Some(m) = self.model.as_ref() else { return Array::new() };
        let cat = crate::services::services().catalogue();
        let issues = self.base_issues();
        let mut bad: BTreeSet<usize> = BTreeSet::new();
        for i in issues.iter() {
            match i {
                Issue::PartOutOfBounds { slot } | Issue::PartBadPlacement { slot } => { bad.insert(slot.index()); }
                Issue::PartsOverlap { a, b } => { bad.insert(a.index()); bad.insert(b.index()); }
                Issue::PartNotAvailable { part } | Issue::PartOverLimit { part, .. } | Issue::UnknownPart { part } => { bad.extend(m.blueprint.parts.iter().enumerate().filter(|(_, p)| &p.part == part).map(|(i, _)| i)); }
                _ => {}
            }
        }
        let mirrors = mirror_pairs(&m.blueprint, &cat);
        let selected: BTreeSet<PartSlot> = super::selected_slots(&m.blueprint, &m.selection).into_iter().collect();
        m.blueprint.parts.iter().enumerate().map(|(i, p)| {
            let load = cat.part(&p.part).map_or(-1.0, |d| load_fraction(p, d));
            d! {
                "slot" => i as i64, "part" => StringName::from(p.part.as_str()), "cell" => v3i(p.pos), "yaw" => p.rot.yaw as i64, "tilt" => p.rot.tilt as i64,
                "kit" => p.kit.map_or(-1, |k| k.0 as i64), "label" => GString::from(p.label.as_deref().unwrap_or("")), "load" => load,
                "selected" => selected.contains(&PartSlot(i as u16)), "valid" => !bad.contains(&i), "mirror_of" => mirrors.get(i).copied().unwrap_or(-1),
            }
        }).collect()
    }
    /// Placed kits, ascending kit slot: `Array[PlacedKitDict]`.
    #[func]
    fn kits(&self) -> Array<VarDictionary> {
        let Some(m) = self.model.as_ref() else { return Array::new() };
        let cat = crate::services::services().catalogue();
        let player = self.player_kits();
        m.blueprint.kits.iter().enumerate().map(|(i, k)| {
            let latest = latest_kit(&k.kit, &m.blueprint, &cat, &player).map_or(k.version, |d| d.version);
            let mut settings = VarDictionary::new();
            for (key, v) in &k.settings { settings.set(&GString::from(key.as_str()).to_variant(), &(if v.is_finite() { *v } else { 0.0 }).to_variant()); }
            let parts: PackedInt32Array = k.parts.iter().map(|s| s.0 as i32).collect();
            d! { "slot" => i as i64, "kit" => StringName::from(k.kit.as_str()), "version" => k.version as i64, "latest_version" => latest as i64, "cell" => v3i(k.pos), "yaw" => k.rot.yaw as i64, "tilt" => k.rot.tilt as i64, "mirrored" => k.mirrored, "parts" => parts, "settings" => settings }
        }).collect()
    }
    /// Move a placed part (and its kit, when it belongs to one). Returns false when the target is not valid.
    #[func]
    fn move_part(&mut self, slot: i64, cell: Vector3i, yaw: i64, tilt: i64) -> bool {
        let cat = crate::services::services().catalogue();
        let Some(to) = cell_from(cell) else { return false };
        let rot = rot_from(yaw, tilt);
        let Some(m) = self.model.as_ref() else { return false };
        let Some(i) = usize::try_from(slot).ok().filter(|i| *i < m.blueprint.parts.len()) else { return false };
        let p = m.blueprint.parts[i].clone();
        if p.pos == to && content::kit::normalize_rot(p.rot) == rot { return true; }
        let mut next = m.blueprint.clone();
        let mut moved = BTreeSet::new();
        match p.kit.and_then(|k| m.blueprint.kits.get(k.index()).map(|kp| (k, kp.clone()))) {
            Some((k, kp)) => {
                let d = [to.x as i32 - p.pos.x as i32, to.y as i32 - p.pos.y as i32, to.z as i32 - p.pos.z as i32];
                let Some(kpos) = cell_of([kp.pos.x as i32 + d[0], kp.pos.y as i32 + d[1], kp.pos.z as i32 + d[2]]) else { return false };
                let krot = PartRot { yaw: ((kp.rot.yaw as i64 + yaw - p.rot.yaw as i64).rem_euclid(4)) as u8, tilt: ((kp.rot.tilt as i64 + tilt - p.rot.tilt as i64).rem_euclid(6)) as u8 };
                let def = next.embedded_kits.get(&kp.kit).cloned().or_else(|| cat.kit(&kp.kit).cloned()).or_else(|| self.player_kits().iter().find(|x| x.id == kp.kit).cloned());
                let placed = def.as_ref().map(|d| content::kit::kit_part_placements(d, &cat, kpos, krot, kp.mirrored));
                match placed.filter(|pl| pl.len() == kp.parts.len()) {
                    Some(pl) => {
                        for (slot, pp) in kp.parts.iter().zip(pl) {
                            let Some(t) = next.parts.get_mut(slot.index()) else { return false };
                            t.pos = pp.pos;
                            t.rot = pp.rot;
                            moved.insert(slot.index());
                        }
                        let sockets = def.as_ref().map(|d| content::kit::kit_sockets_at(d, &cat, kpos, krot, kp.mirrored)).unwrap_or_default();
                        let kk = &mut next.kits[k.index()];
                        kk.pos = kpos;
                        kk.rot = krot;
                        for s in &mut kk.sockets { if let Some(n) = sockets.iter().find(|x| x.name == s.socket) { s.cell = n.cell; } }
                    }
                    None => {
                        // The definition is gone or changed shape: translate the parts as they are.
                        if krot != kp.rot { return false; }
                        for slot in &kp.parts {
                            let Some(t) = next.parts.get_mut(slot.index()) else { return false };
                            let Some(np) = cell_of([t.pos.x as i32 + d[0], t.pos.y as i32 + d[1], t.pos.z as i32 + d[2]]) else { return false };
                            t.pos = np;
                            moved.insert(slot.index());
                        }
                        let kk = &mut next.kits[k.index()];
                        kk.pos = kpos;
                        for s in &mut kk.sockets { if let Some(c) = cell_of([s.cell.x as i32 + d[0], s.cell.y as i32 + d[1], s.cell.z as i32 + d[2]]) { s.cell = c; } }
                    }
                }
            }
            None => {
                next.parts[i].pos = to;
                next.parts[i].rot = rot;
                moved.insert(i);
            }
        }
        self.commit_moved(next, &moved)
    }
    /// Move the whole selection by a cell delta. Returns false, changing nothing, when any part would collide.
    #[func]
    fn move_selection(&mut self, delta: Vector3i) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        if delta == Vector3i::ZERO { return true; }
        let d = [delta.x, delta.y, delta.z];
        if d.iter().any(|v| v.abs() > MAX_GRID_DIM as i32) { return false; }
        let slots = super::selected_slots(&m.blueprint, &m.selection);
        if slots.is_empty() { return false; }
        let set: BTreeSet<usize> = slots.iter().map(|s| s.index()).collect();
        let mut next = m.blueprint.clone();
        for &i in &set {
            let t = &mut next.parts[i];
            let Some(np) = cell_of([t.pos.x as i32 + d[0], t.pos.y as i32 + d[1], t.pos.z as i32 + d[2]]) else { return false };
            t.pos = np;
        }
        // A kit moves with its parts when all of them move.
        for k in &mut next.kits {
            if k.parts.is_empty() || !k.parts.iter().all(|s| set.contains(&s.index())) { continue; }
            let Some(np) = cell_of([k.pos.x as i32 + d[0], k.pos.y as i32 + d[1], k.pos.z as i32 + d[2]]) else { return false };
            k.pos = np;
            for s in &mut k.sockets { if let Some(c) = cell_of([s.cell.x as i32 + d[0], s.cell.y as i32 + d[1], s.cell.z as i32 + d[2]]) { s.cell = c; } }
        }
        self.commit_moved(next, &set)
    }
    #[func]
    fn remove_part(&mut self, slot: i64) -> bool {
        let Some(s) = u16::try_from(slot).ok().map(PartSlot) else { return false };
        if self.model.as_ref().is_none_or(|m| s.index() >= m.blueprint.parts.len()) { return false; }
        let ok = self.commit_edit(&BlueprintEdit::RemovePart(s), "dock.undo.remove_part");
        if ok { ui_sfx(UiSfx::Remove); }
        ok
    }
    #[func]
    fn remove_kit(&mut self, kit_slot: i64) -> bool {
        let Some(k) = u16::try_from(kit_slot).ok().map(KitSlot) else { return false };
        if self.model.as_ref().is_none_or(|m| k.index() >= m.blueprint.kits.len()) { return false; }
        let ok = self.commit_edit(&BlueprintEdit::RemoveKit(k), "dock.undo.remove_kit");
        if ok { ui_sfx(UiSfx::Remove); }
        ok
    }
    /// Player label of a part ("Gun 2"); empty restores the generated name.
    #[func]
    fn set_label(&mut self, slot: i64, label: GString) {
        let Some(m) = self.model.as_ref() else { return };
        let Some(i) = usize::try_from(slot).ok().filter(|i| *i < m.blueprint.parts.len()) else { return };
        let l: String = super::clean_name(&label.to_string()).chars().take(MAX_LABEL_CHARS).collect();
        let mut next = m.blueprint.clone();
        next.parts[i].label = (!l.is_empty()).then_some(l);
        self.commit(next, "dock.undo.label");
    }
    /// Fill of a tank or magazine, 0.0 to 1.0.
    #[func]
    fn set_load(&mut self, slot: i64, load: f64) -> bool {
        use content::part::PartFunction as F;
        let cat = crate::services::services().catalogue();
        let Some(m) = self.model.as_ref() else { return false };
        let Some(s) = u16::try_from(slot).ok().map(PartSlot).filter(|s| s.index() < m.blueprint.parts.len()) else { return false };
        if !load.is_finite() { return false; }
        let Some(def) = cat.part(&m.blueprint.parts[s.index()].part) else { return false };
        let value = match &def.function {
            F::FuelTank { .. } | F::Magazine { .. } => load.clamp(0.0, 1.0),
            F::Ballast { max_kg } if max_kg.is_finite() && *max_kg > 0.0 => load.clamp(0.0, 1.0) * max_kg,
            _ => return false,
        };
        if m.blueprint.parts[s.index()].load == Some(value) { return true; }
        self.commit_edit(&BlueprintEdit::SetLoad { slot: s, load: value }, "dock.undo.load")
    }
    /// One value of a placed kit's settings sheet.
    #[func]
    fn set_kit_setting(&mut self, kit_slot: i64, key: GString, value: f64) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        let Some(k) = usize::try_from(kit_slot).ok().filter(|k| *k < m.blueprint.kits.len()) else { return false };
        if !value.is_finite() { return false; }
        let key = key.to_string();
        let kid = m.blueprint.kits[k].kit.clone();
        let Some(def) = self.kit_def(&kid) else { return false };
        let Some(row) = def.settings.iter().find(|s| s.key == key) else { return false };
        let (lo, hi) = if row.min <= row.max { (row.min, row.max) } else { (row.max, row.min) };
        let v = if lo.is_finite() && hi.is_finite() { value.clamp(lo, hi) } else { value };
        let Some(m) = self.model.as_ref() else { return false };
        let mut next = m.blueprint.clone();
        next.kits[k].settings.insert(key, v);
        self.commit(next, "dock.undo.kit_setting");
        true
    }
    /// Kits with a newer version available: `Array[Dictionary]` with `kit: StringName`, `placed_version: int`,
    /// `latest_version: int`, `copies: int`.
    #[func]
    fn kit_updates(&self) -> Array<VarDictionary> {
        self.kit_update_list().into_iter().map(|(kit, placed, latest, copies)| d! { "kit" => StringName::from(kit.as_str()), "placed_version" => placed as i64, "latest_version" => latest as i64, "copies" => copies }).collect()
    }
    /// Migrate every placed copy of a kit to its latest version (one undo step). Returns copies migrated.
    #[func]
    fn migrate_kit(&mut self, kit: StringName) -> i64 {
        let cat = crate::services::services().catalogue();
        let player = self.player_kits();
        let Some(m) = self.model.as_ref() else { return 0 };
        let id = KitId::new(kit.to_string());
        let Some(latest) = latest_kit(&id, &m.blueprint, &cat, &player) else { return 0 };
        let mut bp = m.blueprint.clone();
        // The design carries the new definition unless it is the catalogue's own.
        if cat.kit(&id) == Some(&latest) { bp.embedded_kits.remove(&id); } else { bp.embedded_kits.insert(id.clone(), latest.clone()); }
        let mut n = 0i64;
        // Highest kit slot first: a swap re-adds its kit at the end, so lower slots are not disturbed.
        let old: Vec<usize> = (0..bp.kits.len()).rev().filter(|&k| bp.kits[k].kit == id && bp.kits[k].version < latest.version).collect();
        for k in old {
            let settings = bp.kits[k].settings.clone();
            let Some(next) = sim_systems::editor::notes::apply_edit(&bp, &BlueprintEdit::SwapKit { slot: KitSlot(k as u16), kit: id.clone() }, &cat) else { continue };
            bp = next;
            if let Some(newest) = bp.kits.last_mut() {
                for (key, v) in settings {
                    if let Some(row) = latest.settings.iter().find(|s| s.key == key) {
                        let (lo, hi) = if row.min <= row.max { (row.min, row.max) } else { (row.max, row.min) };
                        newest.settings.insert(key, v.clamp(lo, hi));
                    }
                }
            }
            n += 1;
        }
        if n == 0 { return 0; }
        content::blueprint::io::canonicalize_blueprint(&mut bp);
        if !self.commit(bp, "dock.undo.kit_migrate") { return 0; }
        n
    }
    /// Conduit routes for the X-ray overlay: `Array[NetworkDict]`. Routes are auto-set and deterministic (L285).
    #[func]
    fn routes(&self) -> Array<VarDictionary> {
        let cat = crate::services::services().catalogue();
        let Some(design) = self.latest_design() else { return Array::new() };
        let net = &design.prefab.networks;
        let dead = sim_core::networks::unconnected_consumers(net, &design.prefab.parts, &design.prefab.compartments, &cat);
        [NetworkKind::Power, NetworkKind::Fuel, NetworkKind::Data].into_iter().map(|kind| {
            let conduits: VarArray = net.conduits.iter().enumerate().filter(|(_, c)| c.network == kind).map(|(i, c)| d! {
                "id" => i as i64, "from" => c.from.0 as i64, "to" => c.to.0 as i64, "cut" => false, "jury_rigged" => false, "redundant" => c.redundant, "pinned" => c.pinned,
            }.to_variant()).collect();
            let consumers_dead = dead.iter().filter(|s| design.prefab.parts.iter().find(|p| p.slot == **s).and_then(|p| cat.parts.get_index(p.def.index())).is_some_and(|(_, d)| d.ports.iter().any(|port| port.network == kind))).count() as i64;
            d! { "kind" => kind.to_gd(), "conduits" => conduits, "consumers_dead" => consumers_dead, "consumers_on_buffer" => 0i64 }
        }).collect()
    }
    /// Expert network edits (L299): pin or unpin a conduit so it survives regeneration.
    #[func]
    fn pin_route(&mut self, network: i64, conduit: i64, pinned: bool) -> bool {
        let cat = crate::services::services().catalogue();
        let Some(kind) = Self::network_of(network) else { return false };
        let Some(design) = self.current_design() else { return false };
        let Some(c) = usize::try_from(conduit).ok().and_then(|i| design.prefab.networks.conduits.get(i)).filter(|c| c.network == kind) else { return false };
        let grid = &design.prefab.grid.design;
        let cells: Vec<CellPos> = c.cells.iter().filter(|i| i.index() < grid.cells.len()).map(|i| grid.pos(*i)).collect();
        let Some(m) = self.model.as_ref() else { return false };
        let mut next = m.blueprint.clone();
        let existing = next.networks.pinned_routes.iter().position(|r| r.network == kind && r.cells == cells);
        if pinned {
            if existing.is_some() || c.pinned { return true; }
            // The lowest-slot part on this network in each end compartment.
            let end = |comp: content::ids::CompartmentId| -> Option<PartSlot> {
                let cm = design.prefab.compartments.compartments.get(comp.index())?;
                cm.parts.iter().copied().find(|s| m.blueprint.parts.get(s.index()).and_then(|p| cat.part(&p.part)).is_some_and(|d| d.ports.iter().any(|port| port.network == kind)))
            };
            let (Some(from), Some(to)) = (end(c.from), end(c.to)) else { return false };
            if cells.is_empty() { return false; }
            next.networks.pinned_routes.push(content::network::PinnedRoute { network: kind, from, to, cells });
        } else {
            let Some(i) = existing else { return !c.pinned };
            next.networks.pinned_routes.remove(i);
        }
        self.commit(next, "dock.undo.route_pin")
    }
    /// Add an explicit Data link between two parts. Returns false when either has no Data port.
    #[func]
    fn add_data_link(&mut self, from_slot: i64, to_slot: i64) -> bool {
        let cat = crate::services::services().catalogue();
        let Some(m) = self.model.as_ref() else { return false };
        let slot = |v: i64| u16::try_from(v).ok().map(PartSlot).filter(|s| s.index() < m.blueprint.parts.len());
        let (Some(a), Some(b)) = (slot(from_slot), slot(to_slot)) else { return false };
        if a == b { return false; }
        let data = |s: PartSlot| cat.part(&m.blueprint.parts[s.index()].part).is_some_and(|d| d.ports.iter().any(|p| p.network == NetworkKind::Data));
        if !data(a) || !data(b) { return false; }
        if m.blueprint.networks.data_links.iter().any(|&(x, y)| (x == a && y == b) || (x == b && y == a)) { return true; }
        let mut next = m.blueprint.clone();
        next.networks.data_links.push((a, b));
        self.commit(next, "dock.undo.data_link")
    }
    /// Split a network into zones with a breaker at a compartment boundary.
    #[func]
    fn add_breaker(&mut self, network: i64, compartment_a: i64, compartment_b: i64) -> bool {
        let Some(kind) = Self::network_of(network) else { return false };
        let Some(design) = self.current_design() else { return false };
        let (Ok(a), Ok(b)) = (u16::try_from(compartment_a), u16::try_from(compartment_b)) else { return false };
        let c = design.prefab.networks.conduits.iter().find(|c| c.network == kind && ((c.from.0 == a && c.to.0 == b) || (c.from.0 == b && c.to.0 == a)) && !c.cells.is_empty());
        let Some(c) = c else { return false };
        let grid = &design.prefab.grid.design;
        let mid = c.cells[c.cells.len() / 2];
        if mid.index() >= grid.cells.len() { return false; }
        let cell = grid.pos(mid);
        let Some(m) = self.model.as_ref() else { return false };
        if m.blueprint.networks.breakers.iter().any(|x| x.network == kind && x.cell == cell) { return true; }
        let mut next = m.blueprint.clone();
        next.networks.breakers.push(content::network::Breaker { network: kind, cell, closed: false });
        self.commit(next, "dock.undo.breaker")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fac() -> Blueprint { content::campaign::load::stock_blueprint("starters/fac22.bp.ron").unwrap() }
    fn rules(r: &content::ruleset::Ruleset) -> Rules<'_> { Rules { ruleset: r, available: None, enemy_class: false } }

    /// The first ten minutes drop a gun, a radar mast, an engine and a fuel tank (L257) by pointing at the deck
    /// of a new hull. The mounts stand on the deck with their magazines below it, and the internal kits settle
    /// into the hull under the cursor.
    #[test]
    fn the_first_four_kits_go_where_the_deck_is_pointed_at() {
        let cat = content::catalogue::catalogue();
        let r = content::ruleset::Ruleset::campaign(content::enums::Act::Act4);
        let tutorial = content::campaign::load::stock_blueprint("tutorial/fac22_tutorial.bp.ron").unwrap();
        for (bp, extra) in [(content::blueprint::io::new_blueprint("", 22.0), true), (tutorial, false)] {
            let base = validate(&bp, cat, &rules(&r));
            let grid = content::cell::CellGrid::from_rows(&bp.cells);
            let z = grid.size[2] as i16 / 2;
            // The empty cell on the weather deck along the centreline, as `BpEditorView.cell_at` gives it.
            let surface = |x: i16| (0..grid.size[1] as i16).rev().find(|&y| !grid.get(CellPos::new(x, y, z)).is_empty()).map(|y| CellPos::new(x, y + 1, z));
            // A new hull also takes the transom waterjet and a generator; the tutorial hull's stern is already fitted.
            let more: &[(&str, bool)] = if extra { &[("kit.engine.waterjet", true), ("kit.generator", true)] } else { &[] };
            for &(kit, inside) in [("kit.gun", false), ("kit.radar_mast", false), ("kit.engine", true), ("kit.fuel_tank", true)].iter().chain(more) {
                let body = Body::Kit(cat.kit(&KitId::new(kit)).unwrap().clone());
                assert_eq!(settles_inside(&body, cat), inside, "{kit}");
                let mut found = None;
                for x in (0..grid.size[0] as i16 - 4).step_by(2) {
                    let Some(at) = surface(x) else { continue };
                    let v = verdict(&bp, &base, &body, at, PartRot::default(), Symmetry::Mirror, 2, cat, &rules(&r));
                    let v = if v.valid || !inside { v } else {
                        match settle_rows(at).map(|c| verdict(&bp, &base, &body, c, PartRot::default(), Symmetry::Mirror, 2, cat, &rules(&r))).find(|v| v.valid) { Some(v) => v, None => v }
                    };
                    if v.valid { found = Some((at, v)); break; }
                }
                let Some((at, v)) = found else { panic!("{kit}: no deck spot on {} takes it", bp.header.name) };
                let low = v.cells.iter().map(|c| c.y).min().unwrap();
                if kit == "kit.gun" { assert!(low < at.y, "the gun's magazine goes below the deck"); }
                if inside { assert!(v.cells.iter().any(|c| c.y < at.y), "{kit} settles into the hull"); }
            }
        }
    }

    /// The ghost's verdict agrees with validating the candidate: every valid placement validates clean of new
    /// problems, every refusal names one, across a sweep of positions.
    #[test]
    fn ghost_verdicts_agree_with_validation() {
        let cat = content::catalogue::catalogue();
        let bp = fac();
        let r = content::ruleset::Ruleset::campaign(content::enums::Act::Act4);
        let base = validate(&bp, cat, &rules(&r));
        let grid = content::cell::CellGrid::from_rows(&bp.cells);
        let gun = cat.parts.values().find(|d| d.placement == Placement::Weather && matches!(d.function, content::part::PartFunction::Gun(_))).unwrap().clone();
        let tank = cat.parts.values().find(|d| d.placement == Placement::Internal).unwrap().clone();
        let (mut valid, mut refused) = (0, 0);
        for body in [Body::Part(gun), Body::Part(tank)] {
            for x in (2..grid.size[0] as i16 - 2).step_by(5) {
                for y in (0..grid.size[1] as i16 + 2).step_by(2) {
                    let v = verdict(&bp, &base, &body, CellPos::new(x, y, grid.size[2] as i16 / 2), PartRot::default(), Symmetry::None, 2, cat, &rules(&r));
                    if v.valid {
                        valid += 1;
                        let next = v.next.as_ref().unwrap();
                        let cand = validate(next, cat, &rules(&r));
                        assert!(new_problem(&base, &cand, &v.new_slots, next, cat).is_none());
                        assert_eq!(v.new_slots.len(), 1);
                    } else {
                        refused += 1;
                        assert!(!v.reason.is_empty());
                    }
                }
            }
        }
        assert!(valid > 0 && refused > 0, "the sweep saw both verdicts ({valid} valid, {refused} refused)");
    }

    #[test]
    fn mirror_copies_land_across_the_centreline() {
        let cat = content::catalogue::catalogue();
        let bp = fac();
        let s = super::super::mirror_sum(&bp).unwrap();
        let def = cat.parts.values().find(|d| d.size[2] == 1 && d.size[0] >= 1 && d.placement == Placement::Anywhere).or_else(|| cat.parts.values().find(|d| d.size[2] == 1)).unwrap().clone();
        let body = Body::Part(def.clone());
        let base = anchored(&body, cat, CellPos::new(10, 12, 2), PartRot { yaw: 1, tilt: 0 }).unwrap();
        let copies = symmetry_copies(&body, cat, &base, Symmetry::Mirror, 2, &bp);
        assert_eq!(copies.len(), 1);
        let mut want: Vec<CellPos> = base.cells.iter().map(|c| CellPos::new(c.x, c.y, (s - c.z as i32) as i16)).collect();
        want.sort();
        assert_eq!(copies[0].cells, want);
        // Radial half turns map the footprint through the centre.
        let r = symmetry_copies(&body, cat, &base, Symmetry::Radial, 2, &bp);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].cells.len(), base.cells.len());
    }

    #[test]
    fn a_kit_ghost_places_the_whole_kit_and_its_mirror() {
        let cat = content::catalogue::catalogue();
        let bp = fac();
        let r = content::ruleset::Ruleset::campaign(content::enums::Act::Act4);
        let base = validate(&bp, cat, &rules(&r));
        let kit = cat.kits.values().find(|k| k.kind == KitKind::Assembly && !k.parts.is_empty() && k.parts.iter().all(|p| cat.part(&p.part).is_some_and(|d| d.placement == Placement::Weather || d.placement == Placement::Anywhere))).cloned();
        let Some(kit) = kit else { return };
        let grid = content::cell::CellGrid::from_rows(&bp.cells);
        let mut placed = None;
        'outer: for x in (2..grid.size[0] as i16 - 2).step_by(2) {
            for y in 0..grid.size[1] as i16 + 2 {
                for z in [1i16, 2, 3] {
                    let v = verdict(&bp, &base, &Body::Kit(kit.clone()), CellPos::new(x, y, z), PartRot::default(), Symmetry::Mirror, 2, cat, &rules(&r));
                    if v.valid { placed = Some(v); break 'outer; }
                }
            }
        }
        let Some(v) = placed else { return };
        let next = v.next.unwrap();
        assert_eq!(next.kits.len(), bp.kits.len() + 2, "the kit and its mirror image");
        assert!(next.kits[bp.kits.len() + 1].mirrored);
        assert_eq!(v.new_slots.len(), 2 * kit.parts.len());
    }

    /// A breaker as `add_breaker` stores it (open) splits the network at that conduit (L299: zones joined by
    /// breakers); the router ignores a closed one.
    #[test]
    fn an_open_breaker_splits_zones() {
        let cat = content::catalogue::catalogue();
        let bp = fac();
        let prefab = sim_core::vessel::build_prefab(&bp, cat);
        let net = &prefab.networks;
        let Some(c) = net.conduits.iter().find(|c| c.network == NetworkKind::Power && !c.cells.is_empty() && !c.redundant && c.from != c.to) else { return };
        let k = 0usize;
        let cell = prefab.grid.design.pos(c.cells[c.cells.len() / 2]);
        let zone = |n: &sim_core::networks::Networks, comp: content::ids::CompartmentId| n.zones.get(comp.index()).map(|z| z[k]);
        let mut closed = bp.clone();
        closed.networks.breakers.push(content::network::Breaker { network: NetworkKind::Power, cell, closed: true });
        let shut = sim_core::vessel::build_prefab(&closed, cat).networks;
        assert_eq!(zone(&shut, c.from) == zone(&shut, c.to), zone(net, c.from) == zone(net, c.to), "a closed breaker changes nothing");
        // Open breakers on every power conduit of one compartment cut it off into its own zone.
        let mut open = bp.clone();
        for o in net.conduits.iter().filter(|o| o.network == NetworkKind::Power && (o.from == c.from || o.to == c.from) && o.from != o.to && !o.cells.is_empty()) {
            open.networks.breakers.push(content::network::Breaker { network: NetworkKind::Power, cell: prefab.grid.design.pos(o.cells[o.cells.len() / 2]), closed: false });
        }
        let after = sim_core::vessel::build_prefab(&open, cat).networks;
        assert_eq!(zone(net, c.from), zone(net, c.to), "joined before");
        assert_ne!(zone(&after, c.from), zone(&after, c.to), "open breakers split the zone");
    }

    #[test]
    fn rotations_and_loads() {
        assert_eq!(turned(PartRot { yaw: 3, tilt: 0 }, 1).yaw, 0);
        assert_eq!(turned(PartRot { yaw: 0, tilt: 0 }, -1).yaw, 3);
        assert_eq!(rot_from(-1, 7), PartRot { yaw: 3, tilt: 1 });
        for yaw in 0..4 { for tilt in 0..6 {
            let r = PartRot { yaw, tilt };
            assert_eq!(mirrored_rot(mirrored_rot(r)), r);
        }}
    }
}

/// Queue an interface sound for a dry-dock action (placed, removed, turned, refused), so the dock reports
/// each edit by ear as well as on screen. The buttons that start these actions carry no click of their own.
fn ui_sfx(s: crate::seams::sound::UiSfx) { crate::services::services().audio.borrow_mut().ui(s); }
use crate::seams::sound::UiSfx;
