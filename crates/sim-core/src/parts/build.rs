//! Building the runtime part list from a blueprint, feed wiring, and health refresh.
use super::{FeedLinks, HandArc, MountState, PartRuntime, PartState, MAX_FEED};
use crate::networks::PartLinks;
use content::blueprint::{Blueprint, PartPlacement};
use content::catalogue::Catalogue;
use content::cell::{CellGrid, CellPos, Face, BUCKET_DESTROYED, CELL_M};
use content::enums::{DamageLevel, NetworkKind};
use content::ids::{CellIndex, PartIdx, PartSlot};
use content::kit::{normalize_rot, rotate_offset, rotated_size, PartRot};
use content::part::{PartDef, PartFunction, PortRole};
use glam::DVec3;

/// Catalogue index marking a placement whose part the catalogue does not know.
pub(super) const UNKNOWN_DEF: PartIdx = PartIdx(u16::MAX);
/// Slots are u16 and `u16::MAX` is never a valid slot.
const MAX_SLOTS: usize = u16::MAX as usize;
/// Footprint iteration bound (a 32 x 32 x 32 part); larger catalogue sizes are clipped.
const MAX_FOOTPRINT: usize = 32 * 32 * 32;

/// Finite value or the fallback.
pub(super) fn fin(v: f64, d: f64) -> f64 { if v.is_finite() { v } else { d } }
/// Finite and non-negative, else 0.
pub(super) fn nonneg(v: f64) -> f64 { if v.is_finite() && v > 0.0 { v } else { 0.0 } }

pub(super) fn def_of<'c>(cat: &'c Catalogue, p: &PartRuntime) -> Option<&'c PartDef> { cat.parts.get_index(p.def.index()).map(|(_, d)| d) }

pub(super) fn build_parts(bp: &Blueprint, grid: &CellGrid, cat: &Catalogue) -> Vec<PartRuntime> {
    let count = bp.parts.len().min(MAX_SLOTS);
    let mut parts: Vec<PartRuntime> = bp.parts.iter().take(count).enumerate().map(|(i, pl)| build_one(PartSlot(i as u16), pl, grid, cat)).collect();
    let kit_of = kit_membership(bp, count);
    wire_feeds(&mut parts, &kit_of, grid, cat);
    for p in parts.iter_mut() {
        let Some(l) = bp.officers.mount_limits.get(&p.slot) else { continue };
        let Some(PartFunction::Gun(g)) = def_of(cat, p).map(|d| &d.function) else { continue };
        let arc = hand_arc(l);
        p.links.mech.arc = arc;
        if let PartState::Mount(m) = &mut p.state {
            // Start inside the arc, in its representation.
            let (lo, hi) = gun_elev_limits(g);
            let (t, e, _) = super::step::mount_targets(m.train_rad, m.elevation_rad, lo, hi, arc);
            m.train_rad = t;
            m.elevation_rad = e;
        }
    }
    parts
}

fn build_one(slot: PartSlot, pl: &PartPlacement, grid: &CellGrid, cat: &Catalogue) -> PartRuntime {
    let rot = normalize_rot(pl.rot);
    let known = cat.part_idx(&pl.part).and_then(|i| cat.parts.get_index(i.index()).map(|(_, d)| (i, d)));
    let Some((idx, def)) = known else {
        return PartRuntime {
            slot, def: UNKNOWN_DEF, pos: pl.pos, rot, cells: Vec::new(), centre: DVec3::from(pl.pos.centre_m()), compartment: None,
            health: 0.0, level: DamageLevel::Destroyed, links: PartLinks::default(), priority: 0, state: PartState::Inert,
        };
    };
    let cells = footprint(def.size, pl.pos, rot, grid);
    let centre = if cells.is_empty() {
        let s = rotated_size(def.size, rot);
        DVec3::new(pl.pos.x as f64 + s[0] as f64 * 0.5, pl.pos.y as f64 + s[1] as f64 * 0.5, pl.pos.z as f64 + s[2] as f64 * 0.5) * CELL_M
    } else {
        // Cell centres are multiples of 0.25 m, so this serial sum is exact.
        let mut sum = DVec3::ZERO;
        for c in &cells { sum += DVec3::from(grid.pos(*c).centre_m()); }
        sum / cells.len() as f64
    };
    let ghost = cells.is_empty();
    let state = if ghost { PartState::Inert } else { initial_state(def, pl.load) };
    PartRuntime {
        slot, def: idx, pos: pl.pos, rot, cells, centre, compartment: None,
        health: if ghost { 0.0 } else { 1.0 },
        level: if ghost { DamageLevel::Destroyed } else { DamageLevel::Intact },
        links: initial_links(def, cat),
        priority: default_priority(def),
        state,
    }
}

/// In-bounds footprint cells, ascending and unique.
fn footprint(size: [u8; 3], pos: CellPos, rot: PartRot, grid: &CellGrid) -> Vec<CellIndex> {
    let mut out = Vec::new();
    let mut n = 0usize;
    'outer: for x in 0..size[0] {
        for y in 0..size[1] {
            for z in 0..size[2] {
                n += 1;
                if n > MAX_FOOTPRINT { break 'outer; }
                let o = rotate_offset(CellPos::new(x as i16, y as i16, z as i16), size, rot);
                let c = [pos.x as i32 + o.x as i32, pos.y as i32 + o.y as i32, pos.z as i32 + o.z as i32];
                if c.iter().any(|v| *v < 0 || *v > i16::MAX as i32) { continue; }
                if let Some(i) = grid.index(CellPos::new(c[0] as i16, c[1] as i16, c[2] as i16)) { out.push(i); }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn has_port(def: &PartDef, net: NetworkKind, role: PortRole) -> bool { def.ports.iter().any(|p| p.network == net && p.role == role) }

/// Default power priority from the part's data (L287): its `power_priority` when it consumes power, 5 when the
/// data leaves it unset (the network solve's own fallback), 0 for parts that draw no power.
pub(super) fn default_priority(def: &PartDef) -> u8 {
    if !has_port(def, NetworkKind::Power, PortRole::Consume) { return 0; }
    if (1..=5).contains(&def.power_priority) { def.power_priority } else { 5 }
}

/// Links before the first network solve: fully served, buffers full.
fn initial_links(def: &PartDef, cat: &Catalogue) -> PartLinks {
    let t = &cat.tuning.networks;
    let own = def.buffer_s.filter(|v| v.is_finite() && *v >= 0.0);
    let power = has_port(def, NetworkKind::Power, PortRole::Consume);
    let fuel = has_port(def, NetworkKind::Fuel, PortRole::Consume);
    PartLinks {
        battery_s: if power { own.unwrap_or(nonneg(t.battery_s)) } else { 0.0 },
        day_tank_s: if fuel { own.unwrap_or(nonneg(t.day_tank_s)) } else { 0.0 },
        power_supplied: 1.0,
        fuel_supplied: 1.0,
        ..PartLinks::default()
    }
}

/// Fraction load from a placement: missing or invalid means full.
fn fraction(load: Option<f64>) -> f64 { match load { Some(v) if v.is_finite() => v.clamp(0.0, 1.0), _ => 1.0 } }

/// Whole rounds for a fraction of a capacity (the epsilon keeps 0.5 * 80 at 40 after rounding error).
fn rounds_of(capacity: u32, frac: f64) -> u32 { libm::floor(capacity as f64 * frac + 1e-9).clamp(0.0, capacity as f64) as u32 }

/// Elevation range of a gun in radians, ordered and finite.
pub(super) fn gun_elev_limits(g: &content::part::GunDef) -> (f64, f64) {
    let (a, b) = (fin(g.elevation_min_deg, -10.0).to_radians(), fin(g.elevation_max_deg, 85.0).to_radians());
    if a <= b { (a, b) } else { (b, a) }
}

pub(super) fn initial_state(def: &PartDef, load: Option<f64>) -> PartState {
    use PartFunction as F;
    match &def.function {
        F::Engine(_) => PartState::Engine { throttle: 0.0, output_kw: 0.0 },
        F::Generator { .. } => PartState::Generator { output_kw: 0.0 },
        F::Battery { kwh, .. } => PartState::Battery { kwh: nonneg(*kwh) },
        F::FuelTank { litres } => PartState::FuelTank { litres: nonneg(*litres) * fraction(load) },
        F::Propulsor(_) => PartState::Propulsor { shaft_kw: 0.0, thrust_n: 0.0, steer_rad: 0.0 },
        F::Rudder { .. } => PartState::Rudder { angle_rad: 0.0 },
        // An unset ballast load is full, as content's stats (the editor's mass and Float figures) and the editor's
        // Suggest and Notes read it; reading it as empty here launched hulls lighter and more tender than designed.
        F::Ballast { max_kg } => PartState::Ballast { kg: match load { Some(v) if v.is_finite() => v.clamp(0.0, nonneg(*max_kg)), _ => nonneg(*max_kg) } },
        F::Gun(g) => {
            let (lo, hi) = gun_elev_limits(g);
            PartState::Mount(MountState { elevation_rad: 0.0f64.clamp(lo, hi), ready_rounds: g.ready_rounds, ..MountState::default() })
        }
        F::Magazine { rounds, ammo_kg, .. } => {
            let f = fraction(load);
            let r = rounds_of(*rounds, f);
            let kg = if *rounds > 0 { nonneg(*ammo_kg) * r as f64 / *rounds as f64 } else { nonneg(*ammo_kg) * f };
            PartState::Magazine { rounds: r, ammo_kg: kg, flooded: false, cookoff_ticks: 0 }
        }
        F::Launcher(l) => PartState::Launcher { ready: l.cells, reload_ticks: 0, interval_ticks: 0, train_rad: 0.0 },
        F::MineRail { capacity, .. } => PartState::MineRail { mines: *capacity, since_last_m: f64::MAX },
        F::Decoy(d) => PartState::Decoy { rounds: if d.kind == content::part::DecoyKind::TowedDecoy { 0 } else { d.rounds }, reload_ticks: 0, streamed: false },
        F::Sensor(_) | F::Director | F::CameraMast { .. } => PartState::Sensor { bearing_rad: 0.0, swept_from_rad: 0.0, radiating: false, train_rad: 0.0 },
        F::Jammer { .. } => PartState::Jammer { radiating: false },
        F::Pump { .. } => PartState::Pump { running: false, target: None },
        F::Door => PartState::Door { closed: true },
        F::DroneBay { count, .. } => PartState::DroneBay { aboard: *count, recharge_ticks: 0 },
        F::HeliDeck { .. } => PartState::HeliDeck { aboard: true },
        F::RhibDavit | F::BoardingKit { .. } => PartState::Davit { rhib_aboard: true },
        F::Structure | F::Console | F::Hoist { .. } | F::Berths { .. } | F::PassengerBerths { .. } | F::RecoveryNet | F::Crane => PartState::Inert,
    }
}

/// Hand-set limits in radians; `None` when any value is not finite.
fn hand_arc(l: &content::graph::MountLimits) -> Option<HandArc> {
    let v = [l.train_min_deg, l.train_max_deg, l.elevation_min_deg, l.elevation_max_deg];
    if v.iter().any(|x| !x.is_finite()) { return None; }
    let (t0, t1) = if v[0] <= v[1] { (v[0], v[1]) } else { (v[1], v[0]) };
    let (e0, e1) = if v[2] <= v[3] { (v[2], v[3]) } else { (v[3], v[2]) };
    Some(HandArc { t_min: t0.to_radians(), t_max: t1.to_radians(), e_min: e0.to_radians(), e_max: e1.to_radians(), full: t1 - t0 >= 360.0 - 1e-9 })
}

/// Kit instance of each slot: the placement's own `kit` when it names a placed kit, else the first kit that
/// lists the slot.
fn kit_membership(bp: &Blueprint, count: usize) -> Vec<Option<u16>> {
    let mut out: Vec<Option<u16>> = bp.parts.iter().take(count).map(|p| p.kit.filter(|k| k.index() < bp.kits.len()).map(|k| k.0)).collect();
    for (k, kp) in bp.kits.iter().enumerate().take(u16::MAX as usize) {
        for s in &kp.parts {
            if let Some(slot) = out.get_mut(s.index()) { if slot.is_none() { *slot = Some(k as u16); } }
        }
    }
    out
}

fn calibre_of_gun(def: &PartDef) -> Option<u16> { if let PartFunction::Gun(g) = &def.function { Some(g.calibre_mm) } else { None } }
fn magazine_calibre(def: &PartDef) -> Option<u16> { if let PartFunction::Magazine { calibre_mm, .. } = &def.function { Some(*calibre_mm) } else { None } }
fn is_hoist(def: &PartDef) -> bool { matches!(def.function, PartFunction::Hoist { .. }) }

/// Up to `MAX_FEED` slots from `cands`, nearest to `from` first (ties by slot).
fn nearest(parts: &[PartRuntime], from: DVec3, mut cands: Vec<usize>) -> Vec<usize> {
    cands.sort_by(|&a, &b| {
        let (da, db) = (parts[a].centre.distance_squared(from), parts[b].centre.distance_squared(from));
        da.total_cmp(&db).then(a.cmp(&b))
    });
    cands.truncate(MAX_FEED);
    cands
}

fn pack(slots: &[usize]) -> ([u16; MAX_FEED], u8) {
    let mut a = [0u16; MAX_FEED];
    let n = slots.len().min(MAX_FEED);
    for (k, &s) in slots.iter().take(n).enumerate() { a[k] = s as u16; }
    (a, n as u8)
}

/// True when any cell of `b` shares a face with a cell of `a` (`a` ascending).
fn touching(a: &[CellIndex], b: &[CellIndex], grid: &CellGrid) -> bool {
    b.iter().any(|&c| {
        let p = grid.pos(c);
        Face::ALL.iter().any(|f| {
            let (dx, dy, dz) = f.offset();
            let q = CellPos::new(p.x.saturating_add(dx), p.y.saturating_add(dy), p.z.saturating_add(dz));
            grid.index(q).is_some_and(|qi| a.binary_search(&qi).is_ok())
        })
    })
}

/// Kit wiring of magazines and hoists to guns and reloading launchers (L207, L283; see the module docs).
fn wire_feeds(parts: &mut [PartRuntime], kit_of: &[Option<u16>], grid: &CellGrid, cat: &Catalogue) {
    let n = parts.len();
    let defs: Vec<Option<&PartDef>> = parts.iter().map(|p| def_of(cat, p)).collect();
    let kit = |i: usize| kit_of.get(i).copied().flatten();
    let ok: Vec<bool> = (0..n).map(|i| defs[i].is_some() && parts[i].works()).collect();
    let usable = |i: usize| ok[i];
    let mut claimed = vec![false; n];
    let mut feeds: Vec<Option<FeedLinks>> = vec![None; n];
    for g in 0..n {
        let Some(cal) = defs[g].and_then(calibre_of_gun) else { continue };
        if !usable(g) { continue; }
        let mag_ok = |j: usize| usable(j) && defs[j].and_then(magazine_calibre) == Some(cal);
        let hoist_ok = |j: usize| usable(j) && defs[j].is_some_and(is_hoist);
        let in_kit = |j: usize| kit(g).is_some() && kit(j) == kit(g);
        let loose = |j: usize| kit(j).is_none();
        let mut mags: Vec<usize> = (0..n).filter(|&j| mag_ok(j) && in_kit(j)).take(MAX_FEED).collect();
        if mags.is_empty() { mags = nearest(parts, parts[g].centre, (0..n).filter(|&j| mag_ok(j) && loose(j)).collect()); }
        let hoists: Vec<usize> = if kit(g).is_some() {
            (0..n).filter(|&j| hoist_ok(j) && in_kit(j)).take(MAX_FEED).collect()
        } else {
            nearest(parts, parts[g].centre, (0..n).filter(|&j| hoist_ok(j) && loose(j)).collect())
        };
        let direct = mags.iter().any(|&m| touching(&parts[g].cells, &parts[m].cells, grid));
        for &m in &mags { claimed[m] = true; }
        let (ma, mn) = pack(&mags);
        let (ha, hn) = pack(&hoists);
        feeds[g] = Some(FeedLinks { mags: ma, n_mags: mn, hoists: ha, n_hoists: hn, direct });
    }
    for l in 0..n {
        let reloads = matches!(defs[l].map(|d| &d.function), Some(PartFunction::Launcher(ld)) if ld.reload_s.is_finite() && ld.reload_s > 0.0);
        if !reloads || !usable(l) { continue; }
        let mag_ok = |j: usize| usable(j) && defs[j].and_then(magazine_calibre).is_some();
        let mut mags: Vec<usize> = (0..n).filter(|&j| mag_ok(j) && !claimed[j] && kit(l).is_some() && kit(j) == kit(l)).take(MAX_FEED).collect();
        if mags.is_empty() { mags = nearest(parts, parts[l].centre, (0..n).filter(|&j| mag_ok(j) && kit(j).is_none() && !claimed[j]).collect()); }
        let (ma, mn) = pack(&mags);
        feeds[l] = Some(FeedLinks { mags: ma, n_mags: mn, ..FeedLinks::default() });
    }
    for (p, f) in parts.iter_mut().zip(feeds) { if let Some(f) = f { p.links.mech.feed = f; } }
}

pub(super) fn refresh_health(parts: &mut [PartRuntime], grid: &crate::grid::VesselGrid, changed: &[CellIndex]) -> Vec<PartSlot> {
    let mut changed: Vec<CellIndex> = changed.to_vec();
    changed.sort_unstable();
    changed.dedup();
    let mut out = Vec::new();
    if changed.is_empty() { return out; }
    for p in parts.iter_mut() {
        if p.cells.is_empty() { continue; }
        let hit = if p.cells.len() <= changed.len() {
            p.cells.iter().any(|c| changed.binary_search(c).is_ok())
        } else {
            changed.iter().any(|c| p.cells.binary_search(c).is_ok())
        };
        if !hit { continue; }
        let mut sum = 0.0;
        for &c in &p.cells { sum += 1.0 - grid.bucket(c).min(BUCKET_DESTROYED) as f64 / BUCKET_DESTROYED as f64; }
        let mean = sum / p.cells.len() as f64;
        let stored = if p.health.is_finite() { p.health.clamp(0.0, 1.0) } else { 0.0 };
        let health = if p.level == DamageLevel::Destroyed { 0.0 } else { stored.min(mean) };
        let level = crate::crew::level_from_health(health).max(p.level);
        p.health = health;
        if level != p.level { p.level = level; out.push(p.slot); }
    }
    out.sort_unstable();
    out
}
