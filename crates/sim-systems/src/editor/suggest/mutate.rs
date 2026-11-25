//! Seeded mutated variants for the auto-set qualifying corpus (L212). Every draw comes from the counter-based
//! RNG keyed on (seed, step), so variant `n` of a design is the same on every machine.
use crate::editor::notes::apply_edit;
use crate::editor::BlueprintEdit;
use crate::prelude::*;
use content::blueprint::Blueprint;
use content::cell::{CellGrid, CellPos, Material};
use content::hull::HullSource;
use content::kit::PartRot;
use content::part::PartFunction;
use sim_core::rng::{below, uniform, Key, Stream};

/// Edits tried per variant before giving up on reaching the drawn count.
const MAX_TRIES: u64 = 24;

fn key(seed: u64, step: u64, what: u64) -> Key { Key::new(seed, step, Stream::Generator, what, 0x4d55_5441_5445) }
fn pick(seed: u64, step: u64, what: u64, n: usize) -> usize { if n == 0 { 0 } else { below(key(seed, step, what), n.min(u32::MAX as usize) as u32) as usize } }
fn unit(seed: u64, step: u64, what: u64) -> f64 { uniform(key(seed, step, what)) }
/// A signed nudge of 1 or 2 cells.
fn nudge(seed: u64, step: u64, what: u64) -> i16 { let m = 1 + pick(seed, step, what, 2) as i16; if pick(seed, step, what + 1, 2) == 0 { -m } else { m } }

pub(crate) fn mutated_variant(bp: &Blueprint, seed: u64, cat: &Catalogue) -> Blueprint {
    let count = 1 + pick(seed, 0, 0, 4);
    let mut out = bp.clone();
    let mut done = 0;
    let mut step = 1u64;
    while done < count && step <= MAX_TRIES {
        if let Some(edit) = draw_edit(&out, seed, step, cat) {
            if let Some(next) = apply_edit(&out, &edit, cat) { out = next; done += 1; }
        }
        step += 1;
    }
    if done == 0 {
        // Still canonical and rehashed, as every variant is.
        content::blueprint::io::canonicalize_blueprint(&mut out);
        out.header.hash = content::blueprint::io::compute_hash(&out);
    }
    out
}

fn loose_parts(bp: &Blueprint) -> Vec<usize> { bp.parts.iter().enumerate().filter(|(_, p)| p.kit.is_none()).map(|(i, _)| i).collect() }

fn draw_edit(bp: &Blueprint, seed: u64, step: u64, cat: &Catalogue) -> Option<BlueprintEdit> {
    match pick(seed, step, 1, 8) {
        // Nudge a loose part along the deck.
        0 => {
            let loose = loose_parts(bp);
            let i = *loose.get(pick(seed, step, 2, loose.len()))?;
            let p = &bp.parts[i];
            let d = nudge(seed, step, 3);
            let to = if pick(seed, step, 5, 2) == 0 { CellPos::new(p.pos.x + d, p.pos.y, p.pos.z) } else { CellPos::new(p.pos.x, p.pos.y, p.pos.z + d) };
            Some(BlueprintEdit::MovePart { slot: PartSlot(i as u16), to, rot: p.rot })
        }
        // Turn a loose part a quarter.
        1 => {
            let loose = loose_parts(bp);
            let i = *loose.get(pick(seed, step, 2, loose.len()))?;
            let p = &bp.parts[i];
            Some(BlueprintEdit::MovePart { slot: PartSlot(i as u16), to: p.pos, rot: PartRot { yaw: (p.rot.yaw + 1) % 4, tilt: p.rot.tilt } })
        }
        // Move a whole kit.
        2 => {
            let k = pick(seed, step, 2, bp.kits.len());
            let kp = bp.kits.get(k)?;
            let d = nudge(seed, step, 3);
            let pos = if pick(seed, step, 5, 2) == 0 { CellPos::new(kp.pos.x + d, kp.pos.y, kp.pos.z) } else { CellPos::new(kp.pos.x, kp.pos.y, kp.pos.z + d) };
            Some(BlueprintEdit::Several(vec![BlueprintEdit::RemoveKit(KitSlot(k as u16)), BlueprintEdit::AddKit { kit: kp.kit.clone(), pos, rot: kp.rot, mirrored: kp.mirrored }]))
        }
        // Change a load.
        3 => {
            let loads: Vec<(usize, f64)> = bp.parts.iter().enumerate().filter_map(|(i, p)| match cat.part(&p.part).map(|d| &d.function) {
                Some(PartFunction::FuelTank { .. }) | Some(PartFunction::Magazine { .. }) => Some((i, 1.0)),
                Some(PartFunction::Ballast { max_kg }) if max_kg.is_finite() && *max_kg > 0.0 => Some((i, *max_kg)),
                _ => None,
            }).collect();
            let (i, max) = *loads.get(pick(seed, step, 2, loads.len()))?;
            let v = unit(seed, step, 3) * max;
            Some(BlueprintEdit::SetLoad { slot: PartSlot(i as u16), load: if max > 1.0 { libm::round(v) } else { libm::round(v * 20.0) / 20.0 } })
        }
        // Nudge a hull handle by 2 to 8%.
        4 => {
            let HullSource::Lofted(shell) = &bp.hull.source else { return None };
            let s = pick(seed, step, 2, shell.stations.len());
            let st = shell.stations.get(s)?;
            let (handle, now) = match pick(seed, step, 3, 3) { 0 => ("beam", st.beam_m), 1 => ("draft", st.draft_m), _ => ("sheer", st.sheer_m) };
            let f = 0.02 + 0.06 * unit(seed, step, 4);
            let v = if pick(seed, step, 5, 2) == 0 { now * (1.0 - f) } else { now * (1.0 + f) };
            Some(BlueprintEdit::SetHullHandle { station: s.min(u8::MAX as usize) as u8, handle: handle.into(), value: v })
        }
        // A bulkhead somewhere along the hull.
        5 => {
            let grid = CellGrid::from_rows(&bp.cells);
            let sx = grid.size[0] as usize;
            if sx < 4 { return None; }
            Some(BlueprintEdit::AddBulkhead { x_cell: (1 + pick(seed, step, 2, sx - 2)) as i16 })
        }
        // Remove a loose part.
        6 => {
            let loose = loose_parts(bp);
            let i = *loose.get(pick(seed, step, 2, loose.len()))?;
            Some(BlueprintEdit::RemovePart(PartSlot(i as u16)))
        }
        // Paint a small armour patch on the side.
        _ => {
            let grid = CellGrid::from_rows(&bp.cells);
            let [sx, sy, sz] = grid.size;
            if sx == 0 || sy == 0 || sz == 0 { return None; }
            let x0 = pick(seed, step, 2, sx as usize) as i16;
            let y0 = pick(seed, step, 3, sy as usize) as i16;
            let tier = 1 + pick(seed, step, 4, 4) as u8;
            let mut cells = Vec::new();
            for x in x0..(x0 + 4).min(sx as i16) {
                for y in y0..(y0 + 3).min(sy as i16) {
                    if let Some(z) = (0..sz as i16).find(|&z| { let c = grid.get(CellPos::new(x, y, z)); !c.is_empty() && c.material != Material::Glass }) { cells.push(CellPos::new(x, y, z)); }
                }
            }
            if cells.is_empty() { return None; }
            Some(BlueprintEdit::PaintArmour { cells, tier })
        }
    }
}
