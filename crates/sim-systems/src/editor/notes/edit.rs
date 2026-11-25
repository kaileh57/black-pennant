//! `apply_edit`: every [`BlueprintEdit`] applied to a copy of a blueprint, with slot and kit renumbering on
//! removal and cell regeneration on lofted-hull edits. Untrusted input: every index, id and number is checked,
//! and an edit that no longer applies returns `None` instead of a partial result.
use crate::editor::BlueprintEdit;
use crate::prelude::*;
use content::blueprint::{Blueprint, KitPlacement, PartPlacement, SocketBinding};
use content::cell::{ArmourTier, Cell, CellGrid, CellPos, Material, MAX_GRID_DIM};
use content::enums::Axis;
use content::graph::{Condition, ControlGraph, NodeKind, Operand, SinkKind, SourceKind};
use content::hull::{Hull, HullSource, InternalPlane};
use content::kit::{KitDef, KitKind, PartRot};
use content::part::{PartDef, PartFunction};

/// Deepest nesting of `Several` accepted.
const MAX_DEPTH: u8 = 8;
/// Burst trigger range limits, metres (the emission controller's parameter range).
const BURST_MIN_M: f64 = 2_000.0;
const BURST_MAX_M: f64 = 60_000.0;

pub(crate) fn apply(bp: &Blueprint, edit: &BlueprintEdit, catalogue: &Catalogue) -> Option<Blueprint> {
    let mut out = bp.clone();
    apply_in(&mut out, edit, catalogue, 0)?;
    content::blueprint::io::canonicalize_blueprint(&mut out);
    out.header.hash = content::blueprint::io::compute_hash(&out);
    Some(out)
}

fn finite(v: f64) -> Option<f64> { if v.is_finite() { Some(v) } else { None } }

fn apply_in(bp: &mut Blueprint, edit: &BlueprintEdit, cat: &Catalogue, depth: u8) -> Option<()> {
    match edit {
        BlueprintEdit::MovePart { slot, to, rot } => {
            let i = slot.index();
            let mut p = bp.parts.get(i)?.clone();
            p.pos = *to;
            p.rot = content::kit::normalize_rot(*rot);
            let cells = part_cells(&p, cat)?;
            let taken = occupied(bp, cat, Some(i));
            if cells.iter().any(|c| taken.contains(c)) { return None; }
            bp.parts[i] = p;
            Some(())
        }
        BlueprintEdit::AddPart(p) => {
            let mut p = p.clone();
            p.rot = content::kit::normalize_rot(p.rot);
            if let Some(k) = p.kit { if k.index() >= bp.kits.len() { return None; } }
            if let Some(l) = p.load { if !l.is_finite() { return None; } }
            let cells = part_cells(&p, cat)?;
            let taken = occupied(bp, cat, None);
            if cells.iter().any(|c| taken.contains(c)) { return None; }
            if bp.parts.len() >= u16::MAX as usize { return None; }
            let slot = PartSlot(bp.parts.len() as u16);
            if let Some(k) = p.kit { bp.kits[k.index()].parts.push(slot); }
            bp.parts.push(p);
            Some(())
        }
        BlueprintEdit::RemovePart(slot) => remove_part(bp, *slot),
        BlueprintEdit::AddKit { kit, pos, rot, mirrored } => add_kit(bp, kit, *pos, *rot, *mirrored, cat),
        BlueprintEdit::RemoveKit(k) => remove_kit(bp, *k),
        BlueprintEdit::SwapKit { slot, kit } => {
            let old = bp.kits.get(slot.index())?.clone();
            remove_kit(bp, *slot)?;
            add_kit(bp, kit, old.pos, old.rot, old.mirrored, cat)
        }
        BlueprintEdit::SetLoad { slot, load } => {
            let load = finite(*load)?;
            let p = bp.parts.get(slot.index())?;
            let def = cat.part(&p.part)?;
            let v = match def.function {
                PartFunction::FuelTank { .. } | PartFunction::Magazine { .. } => load.clamp(0.0, 1.0),
                PartFunction::Ballast { max_kg } => load.clamp(0.0, finite(max_kg)?.max(0.0)),
                _ => return None,
            };
            bp.parts[slot.index()].load = Some(v);
            Some(())
        }
        BlueprintEdit::SetHullHandle { station, handle, value } => {
            let value = finite(*value)?;
            let HullSource::Lofted(shell) = &bp.hull.source else { return None };
            let mut shell = shell.clone();
            set_handle(&mut shell, *station as usize, handle, value)?;
            let max_len = match bp.header.kind { content::blueprint::BlueprintKind::EnemyClass => content::cell::MAX_ENEMY_HULL_M, _ => content::cell::MAX_PLAYER_HULL_M };
            content::hull::validate_shell(&shell, max_len).ok()?;
            let mut hull = bp.hull.clone();
            hull.source = HullSource::Lofted(shell);
            regenerate(bp, hull, cat)
        }
        BlueprintEdit::AddBulkhead { x_cell } => add_bulkhead(bp, *x_cell, cat),
        BlueprintEdit::PaintArmour { cells, tier } => {
            let tier = match tier { 0 => ArmourTier::None, 1 => ArmourTier::Mm5, 2 => ArmourTier::Mm15, 3 => ArmourTier::Mm40, 4 => ArmourTier::Mm80, _ => return None };
            if cells.is_empty() || cells.len() > 1 << 20 { return None; }
            let mut grid = rows_grid(bp)?;
            let mut any = false;
            for &c in cells {
                if !grid.in_bounds(c) { continue; }
                let mut cell = grid.get(c);
                if cell.is_empty() || matches!(cell.material, Material::PartBody | Material::Glass) { continue; }
                cell.armour = tier;
                grid.set(c, cell);
                any = true;
            }
            if !any { return None; }
            bp.cells = grid.to_rows();
            Some(())
        }
        BlueprintEdit::SetSlider { key, value } => {
            let def = crate::officers::tuning::slider(key)?;
            let v = finite(*value)?.clamp(def.min, def.max);
            bp.officers.sliders.insert(key.clone(), v);
            Some(())
        }
        BlueprintEdit::SetBurstTrigger { range_m } => {
            bp.officers.burst_trigger_range_m = Some(finite(*range_m)?.clamp(BURST_MIN_M, BURST_MAX_M));
            Some(())
        }
        BlueprintEdit::SetMountLimits { slot, limits } => {
            let p = bp.parts.get(slot.index())?;
            let def = cat.part(&p.part)?;
            let (emin, emax) = match &def.function {
                PartFunction::Gun(g) => (g.elevation_min_deg, g.elevation_max_deg),
                PartFunction::Launcher(_) => (-90.0, 90.0),
                _ => return None,
            };
            let mut l = *limits;
            for v in [l.train_min_deg, l.train_max_deg, l.elevation_min_deg, l.elevation_max_deg] { finite(v)?; }
            if l.train_max_deg < l.train_min_deg { core::mem::swap(&mut l.train_min_deg, &mut l.train_max_deg); }
            if l.elevation_max_deg < l.elevation_min_deg { core::mem::swap(&mut l.elevation_min_deg, &mut l.elevation_max_deg); }
            l.train_min_deg = l.train_min_deg.clamp(-180.0, 180.0);
            l.train_max_deg = l.train_max_deg.clamp(-180.0, 180.0);
            let (lo, hi) = if emin.is_finite() && emax.is_finite() && emin <= emax { (emin, emax) } else { (-90.0, 90.0) };
            l.elevation_min_deg = l.elevation_min_deg.clamp(lo, hi);
            l.elevation_max_deg = l.elevation_max_deg.clamp(lo, hi);
            bp.officers.mount_limits.insert(*slot, l);
            Some(())
        }
        BlueprintEdit::UnpinNode(id) => {
            let g = &mut bp.graph.pinned;
            let before = g.nodes.len();
            g.nodes.retain(|n| n.id != *id);
            if g.nodes.len() == before { return None; }
            g.edges.retain(|e| e.from.node != *id && e.to.node != *id);
            Some(())
        }
        BlueprintEdit::UnpinRoute { index } => {
            let i = *index as usize;
            if i >= bp.networks.pinned_routes.len() { return None; }
            bp.networks.pinned_routes.remove(i);
            Some(())
        }
        BlueprintEdit::Several(v) => {
            if depth >= MAX_DEPTH || v.is_empty() { return None; }
            for e in v { apply_in(bp, e, cat, depth + 1)?; }
            Some(())
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Footprints.

/// Cells of a placed part's rotated footprint; `None` for an unknown part or a cell outside the grid limits.
pub(crate) fn part_cells(p: &PartPlacement, cat: &Catalogue) -> Option<Vec<CellPos>> { cat.part(&p.part).and_then(|d| footprint(d, p.pos, p.rot)) }

pub(crate) fn footprint(def: &PartDef, pos: CellPos, rot: PartRot) -> Option<Vec<CellPos>> {
    let size = def.size;
    if size.iter().any(|&s| s == 0) { return None; }
    let dim = MAX_GRID_DIM as i32;
    let mut out = Vec::with_capacity(size[0] as usize * size[1] as usize * size[2] as usize);
    for x in 0..size[0] { for y in 0..size[1] { for z in 0..size[2] {
        let o = content::kit::rotate_offset(CellPos::new(x as i16, y as i16, z as i16), size, rot);
        let c = [pos.x as i32 + o.x as i32, pos.y as i32 + o.y as i32, pos.z as i32 + o.z as i32];
        if !c.iter().all(|v| (0..dim).contains(v)) { return None; }
        out.push(CellPos::new(c[0] as i16, c[1] as i16, c[2] as i16));
    }}}
    Some(out)
}

/// Every cell some part occupies, except part `skip`.
pub(crate) fn occupied(bp: &Blueprint, cat: &Catalogue, skip: Option<usize>) -> BTreeSet<CellPos> {
    let mut s = BTreeSet::new();
    for (i, p) in bp.parts.iter().enumerate() {
        if Some(i) == skip { continue; }
        if let Some(cells) = part_cells(p, cat) { s.extend(cells); }
    }
    s
}

fn rows_grid(bp: &Blueprint) -> Option<CellGrid> {
    // Guard the dense expansion: rows outside the grid limits cannot be expanded.
    for r in &bp.cells.0 {
        if r.y < 0 || r.z < 0 || r.y as u16 >= MAX_GRID_DIM || r.z as u16 >= MAX_GRID_DIM { return None; }
        for run in &r.runs { if run.x < 0 || run.x as i32 + run.len as i32 > MAX_GRID_DIM as i32 { return None; } }
    }
    Some(CellGrid::from_rows(&bp.cells))
}

// ---------------------------------------------------------------------------------------------------------
// Kits.

pub(crate) fn kit_def<'a>(bp: &'a Blueprint, cat: &'a Catalogue, id: &KitId) -> Option<&'a KitDef> { bp.embedded_kits.get(id).or_else(|| cat.kit(id)) }

fn add_kit(bp: &mut Blueprint, kit: &KitId, pos: CellPos, rot: PartRot, mirrored: bool, cat: &Catalogue) -> Option<()> {
    let def = kit_def(bp, cat, kit)?.clone();
    if def.kind != KitKind::Assembly || def.parts.is_empty() { return None; }
    let rot = content::kit::normalize_rot(rot);
    let placed = content::kit::kit_part_placements(&def, cat, pos, rot, mirrored);
    if placed.len() != def.parts.len() { return None; }
    let taken = occupied(bp, cat, None);
    let mut mine = BTreeSet::new();
    for p in &placed {
        let d = cat.part(&p.part)?;
        let cells = footprint(d, p.pos, p.rot)?;
        for c in cells { if taken.contains(&c) || !mine.insert(c) { return None; } }
    }
    if bp.kits.len() >= u16::MAX as usize || bp.parts.len() + placed.len() >= u16::MAX as usize { return None; }
    let kslot = KitSlot(bp.kits.len() as u16);
    let mut slots = Vec::with_capacity(placed.len());
    for p in placed {
        slots.push(PartSlot(bp.parts.len() as u16));
        bp.parts.push(PartPlacement { part: p.part, pos: p.pos, rot: p.rot, kit: Some(kslot), label: None, load: None });
    }
    let sockets = content::kit::kit_sockets_at(&def, cat, pos, rot, mirrored).into_iter().map(|s| SocketBinding { socket: s.name, cell: s.cell }).collect();
    let settings = def.settings.iter().map(|s| (s.key.clone(), s.default)).collect();
    bp.kits.push(KitPlacement { kit: def.id.clone(), version: def.version, pos, rot, mirrored, sockets, settings, parts: slots });
    Some(())
}

/// Remove a kit entry only (its parts stay, unowned); part kit references above it shift down.
fn drop_kit_entry(bp: &mut Blueprint, k: KitSlot) {
    if k.index() >= bp.kits.len() { return; }
    bp.kits.remove(k.index());
    for p in &mut bp.parts {
        p.kit = match p.kit { Some(x) if x == k => None, Some(x) if x > k => Some(KitSlot(x.0 - 1)), other => other };
    }
}

fn remove_kit(bp: &mut Blueprint, k: KitSlot) -> Option<()> {
    let kit = bp.kits.get(k.index())?.clone();
    drop_kit_entry(bp, k);
    let mut slots: Vec<PartSlot> = kit.parts.iter().copied().filter(|s| s.index() < bp.parts.len()).collect();
    slots.sort_unstable();
    slots.dedup();
    for s in slots.into_iter().rev() { remove_part(bp, s)?; }
    Some(())
}

// ---------------------------------------------------------------------------------------------------------
// Part removal with renumbering of every stored slot.

fn map_slot(x: PartSlot, gone: PartSlot) -> Option<PartSlot> { if x == gone { None } else if x > gone { Some(PartSlot(x.0 - 1)) } else { Some(x) } }

fn remap_keys<V>(m: &mut BTreeMap<PartSlot, V>, gone: PartSlot) {
    let old = core::mem::take(m);
    for (k, v) in old { if let Some(k) = map_slot(k, gone) { m.insert(k, v); } }
}

fn sink_slot(s: &mut SinkKind) -> Option<&mut PartSlot> {
    match s {
        SinkKind::MountAimPoint(p) | SinkKind::MountFirePermission(p) | SinkKind::MissileLaunch(p) | SinkKind::MissileTarget(p) | SinkKind::EmissionState(p) | SinkKind::PowerPriority(p) | SinkKind::Pump(p) | SinkKind::Door(p) => Some(p),
        _ => None,
    }
}

fn remap_operand(o: &mut Operand, gone: PartSlot) -> bool {
    match o {
        Operand::PartHealth(p) | Operand::AmmoFraction(p) => match map_slot(*p, gone) { Some(n) => { *p = n; true } None => false },
        _ => true,
    }
}

fn remap_condition(c: &mut Condition, gone: PartSlot) -> bool {
    match c {
        Condition::Compare { lhs, rhs, .. } => remap_operand(lhs, gone) & remap_operand(rhs, gone),
        Condition::All(v) | Condition::Any(v) => v.iter_mut().fold(true, |ok, c| remap_condition(c, gone) & ok),
        Condition::Not(c) => remap_condition(c, gone),
    }
}

/// Renumber part references in a graph; nodes that named the removed part go, with their edges.
fn remap_graph(g: &mut ControlGraph, gone: PartSlot) {
    let mut dropped = BTreeSet::new();
    for n in &mut g.nodes {
        let slot = match &mut n.kind {
            NodeKind::Source(SourceKind::PartState(p)) | NodeKind::Source(SourceKind::Detections(p)) => Some(p),
            NodeKind::Sink(s) => sink_slot(s),
            _ => None,
        };
        if let Some(p) = slot { match map_slot(*p, gone) { Some(nw) => *p = nw, None => { dropped.insert(n.id); } } }
    }
    if dropped.is_empty() { return; }
    g.nodes.retain(|n| !dropped.contains(&n.id));
    g.edges.retain(|e| !dropped.contains(&e.from.node) && !dropped.contains(&e.to.node));
}

pub(crate) fn remove_part(bp: &mut Blueprint, gone: PartSlot) -> Option<()> {
    if gone.index() >= bp.parts.len() { return None; }
    bp.parts.remove(gone.index());
    let mut emptied = Vec::new();
    for (i, k) in bp.kits.iter_mut().enumerate() {
        let had = k.parts.contains(&gone);
        k.parts = k.parts.iter().filter_map(|&p| map_slot(p, gone)).collect();
        if had && k.parts.is_empty() { emptied.push(i); }
    }
    // A kit whose last part this was goes too; kits that never held a part stay.
    for k in emptied.into_iter().rev() { drop_kit_entry(bp, KitSlot(k as u16)); }
    let o = &mut bp.officers;
    remap_keys(&mut o.mount_presets, gone);
    remap_keys(&mut o.mount_doctrine, gone);
    remap_keys(&mut o.power_priority, gone);
    remap_keys(&mut o.mount_limits, gone);
    remap_keys(&mut o.radar_hz, gone);
    bp.rules.retain_mut(|r| {
        let sink_ok = match sink_slot(&mut r.sink) { Some(p) => match map_slot(*p, gone) { Some(n) => { *p = n; true } None => false }, None => true };
        remap_condition(&mut r.when, gone) & sink_ok
    });
    let n = &mut bp.networks;
    n.pinned_routes.retain_mut(|r| match (map_slot(r.from, gone), map_slot(r.to, gone)) { (Some(a), Some(b)) => { r.from = a; r.to = b; true } _ => false });
    n.data_links = n.data_links.iter().filter_map(|&(a, b)| Some((map_slot(a, gone)?, map_slot(b, gone)?))).collect();
    remap_graph(&mut bp.graph.pinned, gone);
    for r in &mut bp.graph.replaced { remap_graph(&mut r.subgraph, gone); }
    Some(())
}

// ---------------------------------------------------------------------------------------------------------
// Hull handles, bulkheads and regeneration.

fn set_handle(shell: &mut content::hull::HullShell, station: usize, handle: &str, v: f64) -> Option<()> {
    use content::hull as h;
    match handle {
        "bow_rake" => shell.bow_rake_deg = h::BOW_RAKE_DEG.clamp(v),
        "transom" => shell.transom_rake_deg = h::TRANSOM_RAKE_DEG.clamp(v),
        "transom_width" => shell.transom_width = h::TRANSOM_WIDTH.clamp(v),
        "length" => shell.length_m = v.clamp(content::cell::MIN_PLAYER_HULL_M, content::cell::MAX_ENEMY_HULL_M),
        _ => {
            let s = shell.stations.get_mut(station)?;
            match handle {
                "beam" => s.beam_m = h::STATION_BEAM_M.clamp(v),
                "draft" => s.draft_m = h::DRAFT_M.clamp(v),
                "deadrise" => s.deadrise_deg = h::DEADRISE_DEG.clamp(v),
                "flare" => s.flare_deg = h::FLARE_DEG.clamp(v),
                "sheer" => s.sheer_m = h::SHEER_M.clamp(v),
                _ => return None,
            }
        }
    }
    Some(())
}

fn add_bulkhead(bp: &mut Blueprint, x_cell: i16, cat: &Catalogue) -> Option<()> {
    let grid = content::blueprint::io::build_grid(bp, cat);
    if x_cell < 1 || x_cell as i32 >= grid.size[0] as i32 - 1 { return None; }
    if bp.hull.planes.iter().any(|p| p.axis == Axis::X && p.offset == x_cell) { return None; }
    let plane = InternalPlane { axis: Axis::X, offset: x_cell, watertight: true };
    match &bp.hull.source {
        HullSource::Lofted(_) => {
            let mut hull = bp.hull.clone();
            hull.planes.push(plane);
            regenerate(bp, hull, cat)
        }
        HullSource::Freeform => {
            let mut cells = rows_grid(bp)?;
            let [_, sy, sz] = grid.size;
            let x = x_cell;
            let mut any = false;
            for y in 0..sy as i16 {
                for z in 0..sz as i16 {
                    let p = CellPos::new(x, y, z);
                    if !grid.get(p).is_empty() { continue; }
                    let side_a = (0..z).any(|zz| !grid.get(CellPos::new(x, y, zz)).is_empty());
                    let side_b = (z + 1..sz as i16).any(|zz| !grid.get(CellPos::new(x, y, zz)).is_empty());
                    let below = (0..y).any(|yy| !grid.get(CellPos::new(x, yy, z)).is_empty());
                    if side_a && side_b && below {
                        grow(&mut cells, p);
                        cells.set(p, Cell { material: Material::Steel, armour: ArmourTier::None, paint: content::hull::PAINT_SIDES, shape: content::cell::CellShape::CUBE });
                        any = true;
                    }
                }
            }
            if !any { return None; }
            bp.cells = cells.to_rows();
            bp.hull.planes.push(plane);
            Some(())
        }
    }
}

fn grow(g: &mut CellGrid, p: CellPos) {
    if g.in_bounds(p) { return; }
    let size = [g.size[0].max(p.x as u16 + 1), g.size[1].max(p.y as u16 + 1), g.size[2].max(p.z as u16 + 1)];
    let mut n = CellGrid::new(size);
    for y in 0..g.size[1] as i16 { for z in 0..g.size[2] as i16 { for x in 0..g.size[0] as i16 {
        let c = CellPos::new(x, y, z);
        n.set(c, g.get(c));
    }}}
    *g = n;
}

/// Regenerate a lofted hull's cells for a changed shell or plane list. The old voxelization is diffed against
/// the stored cells so player work survives: cells added on top of the shell (deckhouses), cells cut from it,
/// armour and repainted swatches all carry over, shifted with the frame when the shell's layout moves; parts,
/// kits, markings and pinned routes shift with it.
fn regenerate(bp: &mut Blueprint, mut hull: Hull, cat: &Catalogue) -> Option<()> {
    let (HullSource::Lofted(old_shell), HullSource::Lofted(new_shell)) = (&bp.hull.source, &hull.source) else { return None };
    let (lo, ln) = (content::hull::shell_layout(old_shell), content::hull::shell_layout(new_shell));
    if ln.size.iter().any(|&s| s == 0) { return None; }
    let d = [(lo.x_offset as i32 - ln.x_offset as i32), (ln.waterline_y as i32 - lo.waterline_y as i32), (ln.half_width_cells as i32 - lo.half_width_cells as i32)];
    let shift = |p: CellPos| -> Option<CellPos> {
        let q = [p.x as i32 + d[0], p.y as i32 + d[1], p.z as i32 + d[2]];
        if q.iter().all(|v| (0..MAX_GRID_DIM as i32).contains(v)) { Some(CellPos::new(q[0] as i16, q[1] as i16, q[2] as i16)) } else { None }
    };
    // Planes move with the frame before the new shell is voxelized.
    for p in &mut hull.planes {
        p.offset = (p.offset as i32 + match p.axis { Axis::X => d[0], Axis::Y => d[1], Axis::Z => d[2] }).clamp(0, MAX_GRID_DIM as i32 - 1) as i16;
    }
    hull.design_waterline_y = (bp.hull.design_waterline_y as i32 + d[1]).clamp(0, MAX_GRID_DIM as i32 - 1) as i16;
    let old_vox = content::hull::voxelize(&bp.hull).grid;
    let new_vox = content::hull::voxelize(&hull);
    if new_vox.grid.cells.is_empty() { return None; }
    let stored = rows_grid(bp)?;
    let mut out: BTreeMap<CellPos, Cell> = BTreeMap::new();
    let [nx, ny, nz] = new_vox.grid.size;
    for y in 0..ny as i16 { for z in 0..nz as i16 { for x in 0..nx as i16 {
        let p = CellPos::new(x, y, z);
        let c = new_vox.grid.get(p);
        if !c.is_empty() { out.insert(p, c); }
    }}}
    let generated = [content::hull::PAINT_SIDES, content::hull::PAINT_DECK, content::hull::PAINT_BOOT_TOPPING, content::hull::PAINT_ANTIFOULING];
    let [sx, sy, sz] = stored.size;
    for y in 0..sy as i16 { for z in 0..sz as i16 { for x in 0..sx as i16 {
        let p = CellPos::new(x, y, z);
        let (was, now) = (old_vox.get(p), stored.get(p));
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
    let size = out.keys().fold([0u16; 3], |s, p| [s[0].max(p.x as u16 + 1), s[1].max(p.y as u16 + 1), s[2].max(p.z as u16 + 1)]);
    let mut grid = CellGrid::new(size);
    for (p, c) in out { grid.set(p, c); }
    if d != [0, 0, 0] {
        for p in &mut bp.parts { p.pos = shift(p.pos)?; }
        for k in &mut bp.kits {
            k.pos = shift(k.pos)?;
            for s in &mut k.sockets { s.cell = shift(s.cell)?; }
        }
        for m in &mut bp.markings { m.cell = shift(m.cell)?; }
        for r in &mut bp.networks.pinned_routes { for c in &mut r.cells { *c = shift(*c)?; } }
        for b in &mut bp.networks.breakers { b.cell = shift(b.cell)?; }
        // Every part must still have a footprint inside the limits.
        for p in &bp.parts { part_cells(p, cat)?; }
    }
    hull.design_waterline_y = new_vox.design_waterline_y;
    bp.hull = hull;
    bp.cells = grid.to_rows();
    Some(())
}
