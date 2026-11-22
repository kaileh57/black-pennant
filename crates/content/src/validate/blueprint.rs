//! Blueprint validation (DESIGN.md L2081, L945, L126, L151, L313, L2150).
//!
//! Issues come in a fixed order: paint table, shell and grid bounds, length, tonnage and complexity, parts
//! (unknown, unavailable, over limit, out of bounds, overlapping, badly placed), connectivity, paint indices,
//! control layers, graph content, then kit bindings. Ids are reported in ascending order and slots in
//! ascending slot order, so every platform reports the same list.
//!
//! Placement rules, on the dense grid of structure plus part cells (shaped cells count as full, as in the
//! simulation, L535). "Outside" is the empty space reachable from beyond the bounding box through empty
//! cells: open sea and open air. The rules are lenient where a stricter one would refuse sensible ships.
//! - `Weather`: no structure above any of the part's cells, so a director may sit on a mast and a mount may
//!   sit under another part's yard, but never under a deck or deckhouse.
//! - `Internal`: every part cell is inside the hull envelope: structure lies below it in its column and on
//!   both sides of it along X and along Z. The sky may be open (an open cockpit or well deck is still inside).
//! - `Underwater`: some part cell below the design waterline row has a face on the outside.
//! - `DeckEdge`: some part cell at or above the design waterline row has a side face (X or Z) on the outside.
use super::graph::validate_graph;
use super::kit::{needs_graph_layer, part_limit, validate_kit};
use super::{Issue, ValidationContext, ValidationReport};
use crate::blueprint::{Blueprint, MAX_PAINT};
use crate::catalogue::{Catalogue, BUILD_PHASE};
use crate::cell::{CellPos, CELL_M, MAX_ENEMY_HULL_M, MAX_GRID_DIM, MIN_PLAYER_HULL_M, MAX_PLAYER_HULL_M};
use crate::enums::ControlLayer;
use crate::graph::NodeKind;
use crate::hull::{validate_shell, HullSource, ShellError};
use crate::ids::{KitSlot, PartId, PartSlot};
use crate::kit::KitDef;
use crate::part::{PartDef, Placement};
use crate::stats::geom::{footprint, in_frame, Structure};
use std::collections::{BTreeMap, BTreeSet};

const EMPTY: u8 = 0;
const STRUCTURE: u8 = 1;
const PART: u8 = 2;
/// Largest bounding box the dense checks will build, in cells. A 150 m capital ship 40 m in beam and 40 m tall
/// is under 2 million; a box past this is not a ship and is refused as out of bounds instead of costing the
/// host hundreds of megabytes.
const MAX_BOX_CELLS: usize = 8 << 20;

/// Dense occupancy of structure and part cells over their bounding box, plus the outside-space flood.
struct Occupancy {
    size: [usize; 3],
    solid: Vec<u8>,
    /// Outside space, over the box padded by one cell on every side.
    outside: Vec<bool>,
    /// Structure extent along X per (y, z), along Z per (x, y) and lowest structure per (x, z); `(MAX, MIN)`
    /// where there is none.
    x_span: Vec<(u16, u16)>,
    z_span: Vec<(u16, u16)>,
    y_low: Vec<u16>,
}
impl Occupancy {
    fn idx(&self, x: usize, y: usize, z: usize) -> usize { x + self.size[0] * (z + self.size[2] * y) }
    fn pidx(&self, x: usize, y: usize, z: usize) -> usize { x + (self.size[0] + 2) * (z + (self.size[2] + 2) * y) }
    fn at(&self, c: [i32; 3]) -> u8 {
        if c.iter().zip(self.size).any(|(&v, s)| v < 0 || v as usize >= s) {
            return EMPTY;
        }
        self.solid[self.idx(c[0] as usize, c[1] as usize, c[2] as usize)]
    }
    fn solid_at(&self, c: [i32; 3]) -> bool { self.at(c) != EMPTY }
    /// Inside the hull envelope (see the module doc). `c` must be inside the box.
    fn enveloped(&self, c: [i32; 3]) -> bool {
        let (x, y, z) = (c[0] as usize, c[1] as usize, c[2] as usize);
        let xs = self.x_span[y * self.size[2] + z];
        let zs = self.z_span[y * self.size[0] + x];
        let low = self.y_low[z * self.size[0] + x];
        let (xu, yu, zu) = (x as u16, y as u16, z as u16);
        xs.0 < xu && xu < xs.1 && zs.0 < zu && zu < zs.1 && low < yu
    }
    /// Is this cell outside space? Anything beyond the box is.
    fn outside_at(&self, c: [i32; 3]) -> bool {
        if c.iter().zip(self.size).any(|(&v, s)| v < -1 || v > s as i32) {
            return true;
        }
        self.outside[self.pidx((c[0] + 1) as usize, (c[1] + 1) as usize, (c[2] + 1) as usize)]
    }

    fn build(structure: &Structure, part_cells: &[[i32; 3]]) -> Option<Occupancy> {
        let mut size = [0usize; 3];
        for (x, y, z, _) in structure.cells() {
            size = [size[0].max(x as usize + 1), size[1].max(y as usize + 1), size[2].max(z as usize + 1)];
        }
        for c in part_cells {
            size = [size[0].max(c[0] as usize + 1), size[1].max(c[1] as usize + 1), size[2].max(c[2] as usize + 1)];
        }
        if size[0] * size[1] * size[2] > MAX_BOX_CELLS {
            return None;
        }
        let none = (u16::MAX, u16::MIN);
        let mut occ = Occupancy {
            size,
            solid: vec![EMPTY; size[0] * size[1] * size[2]],
            outside: Vec::new(),
            x_span: vec![none; size[1] * size[2]],
            z_span: vec![none; size[0] * size[1]],
            y_low: vec![u16::MAX; size[0] * size[2]],
        };
        let widen = |s: &mut (u16, u16), v: u16| *s = (s.0.min(v), s.1.max(v));
        for (x, y, z, _) in structure.cells() {
            let i = occ.idx(x as usize, y as usize, z as usize);
            occ.solid[i] = STRUCTURE;
            let (xu, yu, zu) = (x as usize, y as usize, z as usize);
            widen(&mut occ.x_span[yu * size[2] + zu], x);
            widen(&mut occ.z_span[yu * size[0] + xu], z);
            let low = &mut occ.y_low[zu * size[0] + xu];
            *low = (*low).min(y);
        }
        for c in part_cells {
            let i = occ.idx(c[0] as usize, c[1] as usize, c[2] as usize);
            occ.solid[i] = PART;
        }
        occ.flood_outside();
        Some(occ)
    }

    fn flood_outside(&mut self) {
        let p = [self.size[0] + 2, self.size[1] + 2, self.size[2] + 2];
        let mut outside = vec![false; p[0] * p[1] * p[2]];
        let mut stack = vec![[0usize; 3]];
        outside[0] = true;
        while let Some(c) = stack.pop() {
            for (axis, d) in [(0, -1i64), (0, 1), (1, -1), (1, 1), (2, -1), (2, 1)] {
                let v = c[axis] as i64 + d;
                if v < 0 || v as usize >= p[axis] {
                    continue;
                }
                let mut n = c;
                n[axis] = v as usize;
                let i = n[0] + p[0] * (n[2] + p[2] * n[1]);
                if outside[i] {
                    continue;
                }
                let inner = n.iter().zip(self.size).all(|(&v, s)| v >= 1 && v <= s);
                if inner && self.solid[self.idx(n[0] - 1, n[1] - 1, n[2] - 1)] != EMPTY {
                    continue;
                }
                outside[i] = true;
                stack.push(n);
            }
        }
        self.outside = outside;
    }

    /// First solid cell (ascending cell index) not face-connected to the first solid cell.
    fn stray_cell(&self) -> Option<CellPos> {
        let first = self.solid.iter().position(|&s| s != EMPTY)?;
        let mut seen = vec![false; self.solid.len()];
        seen[first] = true;
        let mut stack = vec![first];
        let (sx, sz) = (self.size[0], self.size[2]);
        while let Some(i) = stack.pop() {
            let (x, z, y) = (i % sx, (i / sx) % sz, i / (sx * sz));
            let c = [x as i32, y as i32, z as i32];
            for (axis, d) in [(0, -1), (0, 1), (1, -1), (1, 1), (2, -1), (2, 1)] {
                let mut n = c;
                n[axis] += d;
                if !self.solid_at(n) {
                    continue;
                }
                let j = self.idx(n[0] as usize, n[1] as usize, n[2] as usize);
                if !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
        let stray = (0..self.solid.len()).find(|&i| self.solid[i] != EMPTY && !seen[i])?;
        Some(CellPos::new((stray % sx) as i16, (stray / (sx * sz)) as i16, ((stray / sx) % sz) as i16))
    }
}

const FACES: [[i32; 3]; 6] = [[-1, 0, 0], [1, 0, 0], [0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1]];

fn placement_ok(placement: Placement, cells: &[[i32; 3]], occ: &Occupancy, waterline_y: i32) -> bool {
    let own: BTreeSet<[i32; 3]> = cells.iter().copied().collect();
    let add = |c: [i32; 3], d: [i32; 3]| [c[0] + d[0], c[1] + d[1], c[2] + d[2]];
    let open_face = |c: [i32; 3], d: [i32; 3]| {
        let n = add(c, d);
        !own.contains(&n) && occ.outside_at(n)
    };
    match placement {
        Placement::Anywhere => true,
        Placement::Weather => cells.iter().all(|&c| (c[1] + 1..occ.size[1] as i32).all(|y| occ.at([c[0], y, c[2]]) != STRUCTURE)),
        Placement::Internal => cells.iter().all(|&c| occ.enveloped(c)),
        // The waterline row is wetted (the boot-topping band), so it counts as below the water.
        Placement::Underwater => cells.iter().any(|&c| c[1] <= waterline_y && FACES.iter().any(|&d| open_face(c, d))),
        Placement::DeckEdge => cells.iter().any(|&c| c[1] >= waterline_y && FACES.iter().filter(|d| d[1] == 0).any(|&d| open_face(c, d))),
    }
}

/// Longest hull the context allows: 150 m in enemy-class mode, else the ruleset's limit, never above 90 m.
fn max_length(ctx: &ValidationContext<'_>) -> f64 {
    if ctx.enemy_class {
        return MAX_ENEMY_HULL_M;
    }
    let m = ctx.ruleset.max_length_m;
    if m.is_finite() { m.clamp(MIN_PLAYER_HULL_M, MAX_PLAYER_HULL_M) } else { MAX_PLAYER_HULL_M }
}

fn kit_def<'a>(bp: &'a Blueprint, catalogue: &'a Catalogue, id: &crate::ids::KitId) -> Option<&'a KitDef> { bp.embedded_kits.get(id).or_else(|| catalogue.kit(id)) }

pub(super) fn validate_blueprint(bp: &Blueprint, catalogue: &Catalogue, ctx: &ValidationContext<'_>) -> ValidationReport {
    let stats = crate::stats::compute_stats(bp, catalogue);
    let rs = ctx.ruleset;
    let mut issues = Vec::new();

    // Paint table and shell.
    if bp.paint.len() > MAX_PAINT {
        issues.push(Issue::TooManyPaintEntries(bp.paint.len()));
    }
    let max_m = max_length(ctx);
    let mut length = stats.length_m;
    if let HullSource::Lofted(shell) = &bp.hull.source {
        if shell.length_m.is_finite() {
            length = length.max(shell.length_m);
        }
        match validate_shell(shell, max_m) {
            Ok(()) | Err(ShellError::Length { .. }) => {}
            Err(e) => issues.push(Issue::BadShell { detail: e.to_string() }),
        }
    }
    if bp.hull.planes.iter().any(|p| p.offset < 0 || p.offset as i32 >= MAX_GRID_DIM as i32) {
        issues.push(Issue::BadShell { detail: "a deck or bulkhead lies outside the cell frame".into() });
    }
    let structure = Structure::of(&bp.cells);
    if structure.out_of_frame {
        issues.push(Issue::GridOutOfBounds);
    }

    // Size, tonnage, complexity.
    let min_m = if ctx.enemy_class { CELL_M } else { MIN_PLAYER_HULL_M };
    if length > max_m + 1e-9 {
        issues.push(Issue::TooLong { length_m: length, max_m });
    } else if stats.cell_count == 0 || length + CELL_M < min_m - 1e-9 {
        // One cell of tolerance: a 10 m shell voxelizes to 19 or 20 cells depending on its rake.
        issues.push(Issue::TooShort { length_m: length, min_m });
    }
    let cap = rs.tonnage_cap_t;
    if cap > 0.0 && cap.is_finite() && !(stats.tonnage_t <= cap) {
        issues.push(Issue::OverTonnage { tonnage_t: stats.tonnage_t, cap_t: cap });
    }
    if let Some(points) = ctx.complexity {
        let cap = rs.complexity_per_vessel;
        if cap > 0.0 && !(points <= cap) {
            issues.push(Issue::OverComplexity { points, cap });
        }
    }

    // Parts.
    let available: Option<BTreeSet<&PartId>> = ctx.available.map(|a| a.iter().collect());
    let mut unknown = BTreeSet::new();
    let mut unavailable = BTreeSet::new();
    let mut counts: BTreeMap<&PartId, (u16, Option<u16>)> = BTreeMap::new();
    let mut placed: Vec<(usize, &PartDef, Vec<[i32; 3]>)> = Vec::new();
    let mut out_of_bounds = Vec::new();
    let slots = bp.parts.len().min(u16::MAX as usize + 1);
    for (slot, p) in bp.parts.iter().enumerate().take(slots) {
        let Some(def) = catalogue.part(&p.part) else {
            unknown.insert(p.part.clone());
            continue;
        };
        let ok = match &available {
            Some(set) => set.contains(&p.part),
            None => def.phase <= BUILD_PHASE && def.replaced_by.is_none(),
        };
        if !ok {
            unavailable.insert(p.part.clone());
        }
        let e = counts.entry(&p.part).or_insert((0, def.standard_limit));
        e.0 = e.0.saturating_add(1);
        let cells = footprint(p, def);
        if cells.iter().all(|&c| in_frame(c)) {
            placed.push((slot, def, cells));
        } else {
            out_of_bounds.push(slot);
        }
    }
    if bp.parts.len() > slots {
        // Slots past u16::MAX cannot be addressed by mounts, rules or the wire protocol.
        out_of_bounds.push(u16::MAX as usize);
    }
    issues.extend(unknown.into_iter().map(|part| Issue::UnknownPart { part }));
    issues.extend(unavailable.into_iter().map(|part| Issue::PartNotAvailable { part }));
    for (id, (count, std_limit)) in counts {
        if let Some(limit) = part_limit(rs, id, std_limit) {
            if count > limit {
                issues.push(Issue::PartOverLimit { part: id.clone(), count, limit });
            }
        }
    }
    issues.extend(out_of_bounds.into_iter().map(|s| Issue::PartOutOfBounds { slot: PartSlot(s as u16) }));

    // Overlaps: for each later part, the lowest earlier slot it shares a cell with.
    let mut owners: Vec<((i32, i32, i32), usize)> = placed.iter().flat_map(|(slot, _, cells)| cells.iter().map(move |c| ((c[1], c[2], c[0]), *slot))).collect();
    owners.sort_unstable();
    owners.dedup();
    let mut overlaps: BTreeMap<usize, usize> = BTreeMap::new();
    for run in owners.chunk_by(|a, b| a.0 == b.0) {
        if run.len() > 1 {
            let first = run[0].1;
            for &(_, s) in &run[1..] {
                let e = overlaps.entry(s).or_insert(first);
                *e = (*e).min(first);
            }
        }
    }
    issues.extend(overlaps.into_iter().map(|(b, a)| Issue::PartsOverlap { a: PartSlot(a as u16), b: PartSlot(b as u16) }));

    // Placement and connectivity on the dense grid. Only built when every cell is inside the frame, so its size
    // is bounded by MAX_GRID_DIM on each axis.
    let part_cells: Vec<[i32; 3]> = owners.iter().map(|((y, z, x), _)| [*x, *y, *z]).collect();
    if let Some(occ) = Occupancy::build(&structure, &part_cells) {
        let waterline = bp.hull.design_waterline_y as i32;
        for (slot, def, cells) in &placed {
            if !placement_ok(def.placement, cells, &occ, waterline) {
                issues.push(Issue::PartBadPlacement { slot: PartSlot(*slot as u16) });
            }
        }
        if let Some(stray_cell) = occ.stray_cell() {
            issues.push(Issue::Disconnected { stray_cell });
        }
    } else if !issues.contains(&Issue::GridOutOfBounds) {
        issues.push(Issue::GridOutOfBounds);
    }

    // Paint indices: the first bad cell in cell-index order, then the first bad marking.
    let paint_n = bp.paint.len().min(MAX_PAINT);
    if let Some((x, y, z, _)) = structure.cells().find(|(_, _, _, c)| c.paint as usize >= paint_n) {
        issues.push(Issue::BadPaintIndex { cell: CellPos::new(x as i16, y as i16, z as i16) });
    }
    if let Some(m) = bp.markings.iter().find(|m| m.paint as usize >= paint_n) {
        issues.push(Issue::BadPaintIndex { cell: m.cell });
    }

    // Control layers (L313) and scripts (L2119-2121).
    let graph_edits = !bp.graph.pinned.nodes.is_empty() || !bp.graph.pinned.edges.is_empty() || !bp.graph.replaced.is_empty();
    let forked = bp.kits.iter().filter_map(|k| kit_def(bp, catalogue, &k.kit)).any(|d| needs_graph_layer(d, catalogue));
    let graph_allowed = rs.kind.max_layer() >= ControlLayer::L3Graph;
    if !graph_allowed && (graph_edits || forked) {
        issues.push(Issue::LayerNotAllowed);
    }
    let scripted = !bp.graph.scripts.is_empty()
        || bp.graph.pinned.nodes.iter().chain(bp.graph.replaced.iter().flat_map(|r| &r.subgraph.nodes)).any(|n| matches!(n.kind, NodeKind::Script(_)));
    if scripted && !rs.luau {
        issues.push(Issue::ScriptsNotAllowed);
    }
    issues.extend(validate_graph(&bp.graph, &bp.rules));

    // Kits: each placement resolves and owns exactly the parts it lists; embedded kits that are placed are
    // checked as kits.
    let mut claimed: BTreeMap<usize, usize> = BTreeMap::new();
    for (k, kp) in bp.kits.iter().enumerate() {
        let Some(def) = kit_def(bp, catalogue, &kp.kit) else {
            issues.push(Issue::BadKitBinding { detail: format!("kit {k} names unknown kit `{}`", kp.kit) });
            continue;
        };
        if let Some(detail) = kit_binding_problem(bp, k, kp, def) {
            issues.push(Issue::BadKitBinding { detail });
        }
        for s in &kp.parts {
            claimed.entry(s.index()).or_insert(k);
        }
    }
    for (slot, p) in bp.parts.iter().enumerate().take(slots) {
        if let Some(KitSlot(k)) = p.kit {
            if claimed.get(&slot) != Some(&(k as usize)) {
                issues.push(Issue::BadKitBinding { detail: format!("part {slot} claims kit {k}, which does not list it") });
            }
        }
    }
    let mut checked = BTreeSet::new();
    for kp in &bp.kits {
        if let Some((key, def)) = bp.embedded_kits.get_key_value(&kp.kit) {
            if !checked.insert(key) {
                continue;
            }
            if def.id != *key {
                issues.push(Issue::BadKitBinding { detail: format!("embedded kit `{key}` is stored as `{}`", def.id) });
            }
            for issue in validate_kit(def, catalogue, rs) {
                // One layer verdict for the whole design.
                if issue == Issue::LayerNotAllowed && issues.contains(&issue) {
                    continue;
                }
                issues.push(issue);
            }
        }
    }
    ValidationReport { issues, stats }
}

/// First problem with one kit placement's bindings, if any.
fn kit_binding_problem(bp: &Blueprint, k: usize, kp: &crate::blueprint::KitPlacement, def: &KitDef) -> Option<String> {
    if kp.parts.len() != def.parts.len() {
        return Some(format!("kit {k} lists {} parts; `{}` has {}", kp.parts.len(), kp.kit, def.parts.len()));
    }
    let mut seen = BTreeSet::new();
    for (i, s) in kp.parts.iter().enumerate() {
        let Some(p) = bp.parts.get(s.index()) else { return Some(format!("kit {k} part {i} names slot {}, which does not exist", s.0)) };
        if !seen.insert(s.0) {
            return Some(format!("kit {k} lists slot {} twice", s.0));
        }
        if p.part != def.parts[i].part {
            return Some(format!("kit {k} part {i} is `{}`, the kit expects `{}`", p.part, def.parts[i].part));
        }
        if p.kit != Some(KitSlot(k.min(u16::MAX as usize) as u16)) {
            return Some(format!("slot {} does not belong to kit {k}", s.0));
        }
    }
    let mut names = BTreeSet::new();
    for b in &kp.sockets {
        if !def.sockets.iter().any(|s| s.name == b.socket) {
            return Some(format!("kit {k} binds socket `{}`, which `{}` does not have", b.socket, kp.kit));
        }
        if !names.insert(b.socket.as_str()) {
            return Some(format!("kit {k} binds socket `{}` twice", b.socket));
        }
    }
    None
}
