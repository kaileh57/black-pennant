//! Removed-part migration: follow `replaced_by` chains, drop parts with no replacement, and keep every
//! part-slot reference in the blueprint pointing at the same physical part afterwards.
use super::RemovedPart;
use crate::blueprint::Blueprint;
use crate::catalogue::Catalogue;
use crate::cell::{Cell, CellGrid, CellPos, Face, Material};
use crate::graph::{Condition, ControlGraph, NodeKind, Operand, SinkKind, SourceKind};
use crate::ids::{KitSlot, PartId, PartSlot};
use crate::kit::PartRot;
use std::collections::{BTreeMap, BTreeSet};

enum Fate {
    Keep,
    Replace(PartId),
    /// Removed; the footprint size is known when the catalogue still carries the old entry.
    Remove(Option<[u8; 3]>),
}

/// Follow a `replaced_by` chain. A part absent from the catalogue, a chain ending at a missing part, or a cycle
/// all mean the part is gone.
fn fate(id: &PartId, catalogue: &Catalogue) -> Fate {
    let Some(first) = catalogue.part(id) else { return Fate::Remove(None) };
    let mut cur = first;
    for _ in 0..=catalogue.parts.len() {
        match &cur.replaced_by {
            None if cur.id == *id => return Fate::Keep,
            None => return Fate::Replace(cur.id.clone()),
            Some(next) => match catalogue.part(next) {
                Some(def) => cur = def,
                None => return Fate::Remove(Some(first.size)),
            },
        }
    }
    Fate::Remove(Some(first.size))
}

/// Resolve a part id for an embedded kit definition: replaced ids follow their chain, others stay as they are.
fn replacement(id: &PartId, catalogue: &Catalogue) -> Option<PartId> {
    match fate(id, catalogue) {
        Fate::Replace(new) => Some(new),
        _ => None,
    }
}

pub fn replace_removed_parts(bp: &mut Blueprint, catalogue: &Catalogue) -> Vec<RemovedPart> {
    let mut removed = Vec::new();
    // Footprints of removed parts whose size is known; `None` size means flood from the anchor cell.
    let mut gone: Vec<(CellPos, PartRot, Option<[u8; 3]>)> = Vec::new();
    // Old slot -> new slot, `None` for removed slots.
    let mut map: Vec<Option<PartSlot>> = Vec::with_capacity(bp.parts.len());
    let mut kept = Vec::with_capacity(bp.parts.len());
    for mut placement in std::mem::take(&mut bp.parts) {
        match fate(&placement.part, catalogue) {
            Fate::Keep => {}
            Fate::Replace(new) => placement.part = new,
            Fate::Remove(size) => {
                removed.push(RemovedPart { part: placement.part.clone(), pos: placement.pos });
                gone.push((placement.pos, placement.rot, size));
                map.push(None);
                continue;
            }
        }
        map.push(Some(PartSlot(kept.len() as u16)));
        kept.push(placement);
    }
    bp.parts = kept;

    for kit in bp.embedded_kits.values_mut() {
        for part in &mut kit.parts {
            if let Some(new) = replacement(&part.part, catalogue) {
                part.part = new;
            }
        }
    }

    if removed.is_empty() {
        return removed;
    }
    remap_slots(bp, &map);
    clear_part_cells(bp, catalogue, &gone);
    removed
}

fn remap_slots(bp: &mut Blueprint, map: &[Option<PartSlot>]) {
    let m = |s: PartSlot| map.get(s.index()).copied().flatten();

    // `KitPlacement::parts` is positional (kit part i is parts[i], and the kit's graph indexes by i), so a kit
    // instance that lost a part cannot stay a kit: it dissolves and its surviving parts become loose parts.
    let mut kit_map: Vec<Option<KitSlot>> = Vec::with_capacity(bp.kits.len());
    let mut kits = Vec::with_capacity(bp.kits.len());
    for mut kit in std::mem::take(&mut bp.kits) {
        let parts: Option<Vec<PartSlot>> = kit.parts.iter().map(|s| m(*s)).collect();
        match parts {
            Some(parts) => {
                kit.parts = parts;
                kit_map.push(Some(KitSlot(kits.len() as u16)));
                kits.push(kit);
            }
            None => kit_map.push(None),
        }
    }
    bp.kits = kits;
    for p in &mut bp.parts {
        if let Some(k) = p.kit {
            if let Some(new) = kit_map.get(k.index()) {
                p.kit = *new;
            }
        }
    }

    let net = &mut bp.networks;
    net.pinned_routes.retain_mut(|r| match (m(r.from), m(r.to)) {
        (Some(a), Some(b)) => { r.from = a; r.to = b; true }
        _ => false,
    });
    net.data_links = net.data_links.iter().filter_map(|(a, b)| Some((m(*a)?, m(*b)?))).collect();

    fn remap_map<V>(map: &mut BTreeMap<PartSlot, V>, m: &impl Fn(PartSlot) -> Option<PartSlot>) {
        *map = std::mem::take(map).into_iter().filter_map(|(k, v)| Some((m(k)?, v))).collect();
    }
    let o = &mut bp.officers;
    remap_map(&mut o.mount_presets, &m);
    remap_map(&mut o.mount_doctrine, &m);
    remap_map(&mut o.power_priority, &m);
    remap_map(&mut o.mount_limits, &m);
    remap_map(&mut o.radar_hz, &m);

    // A rule that reads or drives a removed part cannot work any more, so it goes with the part.
    bp.rules.retain_mut(|rule| remap_sink(&mut rule.sink, &m) && remap_condition(&mut rule.when, &m));

    remap_graph(&mut bp.graph.pinned, &m);
    for rep in &mut bp.graph.replaced {
        remap_graph(&mut rep.subgraph, &m);
    }
}

/// Remap the slot inside a sink; false when it names a removed part.
fn remap_sink(sink: &mut SinkKind, m: &impl Fn(PartSlot) -> Option<PartSlot>) -> bool {
    let slot = match sink {
        SinkKind::MountAimPoint(s) | SinkKind::MountFirePermission(s) | SinkKind::MissileLaunch(s) | SinkKind::MissileTarget(s)
        | SinkKind::EmissionState(s) | SinkKind::PowerPriority(s) | SinkKind::Pump(s) | SinkKind::Door(s) => s,
        _ => return true,
    };
    match m(*slot) {
        Some(n) => { *slot = n; true }
        None => false,
    }
}

fn remap_operand(op: &mut Operand, m: &impl Fn(PartSlot) -> Option<PartSlot>) -> bool {
    let slot = match op {
        Operand::PartHealth(s) | Operand::AmmoFraction(s) => s,
        _ => return true,
    };
    match m(*slot) {
        Some(n) => { *slot = n; true }
        None => false,
    }
}

fn remap_condition(c: &mut Condition, m: &impl Fn(PartSlot) -> Option<PartSlot>) -> bool {
    match c {
        Condition::Compare { lhs, rhs, .. } => {
            let a = remap_operand(lhs, m);
            let b = remap_operand(rhs, m);
            a && b
        }
        Condition::All(v) | Condition::Any(v) => v.iter_mut().all(|c| remap_condition(c, m)),
        Condition::Not(inner) => remap_condition(inner, m),
    }
}

/// Drop nodes bound to removed parts (and every edge touching them); remap the rest.
fn remap_graph(g: &mut ControlGraph, m: &impl Fn(PartSlot) -> Option<PartSlot>) {
    let mut dead = BTreeSet::new();
    g.nodes.retain_mut(|node| {
        let ok = match &mut node.kind {
            NodeKind::Source(SourceKind::PartState(s)) | NodeKind::Source(SourceKind::Detections(s)) => match m(*s) {
                Some(n) => { *s = n; true }
                None => false,
            },
            NodeKind::Sink(sink) => remap_sink(sink, m),
            _ => true,
        };
        if !ok {
            dead.insert(node.id);
        }
        ok
    });
    if !dead.is_empty() {
        g.edges.retain(|e| !dead.contains(&e.from.node) && !dead.contains(&e.to.node));
    }
}

/// Cells of a placed part's footprint, or nothing when the position would leave the cell frame.
fn footprint(pos: CellPos, rot: PartRot, size: [u8; 3], out: &mut Vec<CellPos>) {
    for x in 0..size[0] {
        for y in 0..size[1] {
            for z in 0..size[2] {
                let o = crate::kit::rotate_offset(CellPos::new(x as i16, y as i16, z as i16), size, rot);
                if let (Some(px), Some(py), Some(pz)) = (pos.x.checked_add(o.x), pos.y.checked_add(o.y), pos.z.checked_add(o.z)) {
                    out.push(CellPos::new(px, py, pz));
                }
            }
        }
    }
}

/// Empty the stored part-body cells of removed parts. Structure cells (steel, glass and so on) are never
/// touched, and neither are cells inside a surviving part's footprint. Most blueprints store no part-body cells
/// at all (the dense grid stamps them from placements), so this usually returns at once.
fn clear_part_cells(bp: &mut Blueprint, catalogue: &Catalogue, gone: &[(CellPos, PartRot, Option<[u8; 3]>)]) {
    let has_body = bp.cells.0.iter().flat_map(|r| &r.runs).any(|r| r.cell.material == Material::PartBody && r.len > 0);
    if !has_body || gone.is_empty() {
        return;
    }
    let mut grid = CellGrid::from_rows(&bp.cells);
    let mut cells = Vec::new();
    for p in &bp.parts {
        if let Some(def) = catalogue.part(&p.part) {
            footprint(p.pos, p.rot, def.size, &mut cells);
        }
    }
    let covered: BTreeSet<CellPos> = cells.drain(..).collect();
    let is_free_body = |g: &CellGrid, c: CellPos| g.get(c).material == Material::PartBody && !covered.contains(&c);
    let mut changed = false;
    for (pos, rot, size) in gone {
        match size {
            Some(size) => {
                footprint(*pos, *rot, *size, &mut cells);
                for c in cells.drain(..) {
                    if is_free_body(&grid, c) {
                        grid.set(c, Cell::EMPTY);
                        changed = true;
                    }
                }
            }
            None => {
                // Footprint unknown: clear the connected part-body region the part was anchored in.
                let mut stack = vec![*pos];
                while let Some(c) = stack.pop() {
                    if !is_free_body(&grid, c) {
                        continue;
                    }
                    grid.set(c, Cell::EMPTY);
                    changed = true;
                    for f in Face::ALL {
                        let (dx, dy, dz) = f.offset();
                        if let (Some(x), Some(y), Some(z)) = (c.x.checked_add(dx), c.y.checked_add(dy), c.z.checked_add(dz)) {
                            stack.push(CellPos::new(x, y, z));
                        }
                    }
                }
            }
        }
    }
    if changed {
        bp.cells = grid.to_rows();
    }
}
