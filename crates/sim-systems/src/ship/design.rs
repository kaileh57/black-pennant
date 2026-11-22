//! Unit `sys-ship-design`. Build the logical design of a blueprint: mounts, emitters, slot lists, solver
//! tables, turn model and the compiled graph. DESIGN.md L218 (cached build for Splash), L205-208 (auto-set
//! mount limits and solver tables).
//!
//! # Auto-set mount limits (auto-set value 3, L205)
//! A ray sweep at 1 degree steps over each mount's mechanical arc: every training bearing from -179 to 180
//! degrees, and at each bearing every elevation from the mechanical minimum to the maximum, is tested with
//! `World::mount_path_clear`. The limits are one training arc and one elevation band. A value qualifies only
//! if no sampled shell path inside it hits own structure, so the chosen rectangle is always clear; within that
//! rule the sweep keeps as much of the clear water as one arc can hold:
//! - the elevation floor may rise from the mechanical depression up to level fire, and the ceiling may drop
//!   from the mechanical maximum down to 30 degrees, when that opens training bearings (a gun firing over a
//!   low deckhouse, or under a mast at high angle);
//! - the longest contiguous run of clear bearings is the training arc; among floor and ceiling pairs whose arc
//!   is within 2 degrees of the longest, the widest elevation band wins, then the lower floor.
//!
//! When the clear water falls into two separate sectors (a gun amidships between a bridge and a funnel) one
//! arc cannot hold both without firing through the ship; the sweep keeps the larger sector and the blocked
//! fraction counts the other as lost, so the Ship's Note (over 50% blocked, L194) names it. A design whose
//! auto-set value was dropped to Suggest for this release (`editor::suggest::autoset_status`) gets the
//! mechanical arc instead, and the editor shows the manual control. Hand-set limits
//! (`OfficerSettings::mount_limits`) always win and skip the sweep.
//!
//! # Turn model
//! Top speed and steady turning radius come from `sim_core::hydro::predict_speed_turn`. The rest is derived
//! from the drive train as `sim_core` runs it: `accel_mps2` is full-power acceleration from rest (static
//! propulsor thrust over full-load mass), and `stop_distance_m` is the crash-stop distance from top speed. For
//! the stop the throttle runs from full ahead to full astern at each engine's spool rate (`1 / spool_s` per
//! second, so the reversal takes twice the spool time), each propulsor gives
//! `sim_core::hydro::thrust_at_speed` of its share of the pooled power (astern against headway is the full
//! static astern thrust, 0.45 of the static thrust ahead, as in the physics), and hull resistance is `k v^2` with `k` fitted so it equals the ahead
//! thrust at top speed. The surge equation is integrated at a fixed 0.05 s step on the full-load mass.
use super::{DesignFacts, ShipDesign, TurnModel};
use crate::doctrine::MountInfo;
use crate::emcon::EmitterInfo;
use crate::guidance::gun::RangeRow;
use crate::prelude::*;
use crate::signature::EmitterKind;
use content::blueprint::Blueprint;
use content::enums::NetworkKind;
use content::graph::{MountLimits, WeaponClass};
use content::part::weapons::MunitionKind;
use content::part::{DecoyKind, LauncherKind, PartDef, PartFunction, PortRole, SensorKind};
use content::ruleset::{Ruleset, RulesetKind};
use rayon::prelude::*;
use sim_core::parts::PartRuntime;
use sim_core::vessel::VesselPrefab;

impl ShipDesign {
    /// Build from a blueprint and its prefab: list mounts (with auto-set or hand-set limits), emitters,
    /// sensors, consumers, decoys, pumps, doors and bays in ascending slot; build range tables
    /// (`guidance::gun::range_table`); predict the turn model; resolve effective ranges; then generate and
    /// compile the graph under `ruleset`. Deterministic. Unit: `sys-ship-design`.
    pub fn build(bp: &Blueprint, prefab: Arc<VesselPrefab>, catalogue: &Catalogue, ruleset: &Ruleset) -> ShipDesign {
        let auto = mount_limits_auto_set();
        build_design(bp, prefab, catalogue, ruleset, None, auto, &|p: &VesselPrefab, slot| auto_mount_limits(p, slot, catalogue))
    }
    /// Recompile only the graph after a rules, overrides or settings change (live splash edit, in-sortie
    /// `SetRules`). Unit: `sys-ship-design`.
    pub fn recompile(&self, settings: &content::graph::OfficerSettings, rules: &[content::graph::Rule], overrides: &content::graph::GraphOverrides, catalogue: &Catalogue, ruleset: &Ruleset) -> Arc<crate::graph::CompiledGraph> {
        let facts = self.facts(catalogue);
        Arc::new(crate::graph::compile::compile_with(&facts, settings, overrides, rules, ruleset))
    }

    /// The borrowed design facts the generator and node library read.
    pub(crate) fn facts<'a>(&'a self, catalogue: &'a Catalogue) -> DesignFacts<'a> {
        DesignFacts {
            blueprint: &self.blueprint, prefab: &self.prefab, catalogue, mounts: &self.mounts, emitters: &self.emitters, sensor_slots: &self.sensor_slots,
            consumer_slots: &self.consumer_slots, decoys: &self.decoys, pumps: &self.pumps, doors: &self.doors,
        }
    }

    /// Whether `graph` was compiled under rules that give the same graph as `ruleset`.
    pub(crate) fn compiled_for(&self, ruleset: &Ruleset) -> bool { self.compiled_under == compile_key(ruleset) }

    /// The blueprint this design was built from.
    pub fn blueprint(&self) -> &Blueprint { &self.blueprint }

    /// Auto-set limits and blocked fraction of a mount's sweep, when the design ran one.
    pub fn auto_limits(&self, slot: PartSlot) -> Option<(MountLimits, f64)> { self.auto_limits.get(&slot).copied() }
}

/// What a compile depends on in a ruleset: the highest layer and whether Luau may run.
pub(crate) fn compile_key(ruleset: &Ruleset) -> (RulesetKind, bool) { (ruleset.kind, ruleset.luau) }

/// Auto-set training and elevation limits of one mount (auto-set value 3, L205): ray sweep at 1 degree steps
/// over the arc with `World::mount_path_clear`; a value qualifies only if no sampled shell path hits own
/// structure and no clear sector is excluded. Returns the limits and the blocked fraction of the sweep (a
/// Ship's Note fires above 50%, L194). Unit: `sys-ship-design`.
///
/// The returned fraction is the share of the 360 one-degree training bearings that lie outside the chosen
/// arc: bearings own structure blocks, plus a second clear sector a single arc cannot also hold (see the
/// module doc). Fixed launchers and decoy launchers sweep nothing (no bearing refused, fraction 0); a slot
/// that is not a mount gets unrestricted limits and 0.
pub fn auto_mount_limits(prefab: &VesselPrefab, slot: PartSlot, catalogue: &Catalogue) -> (MountLimits, f64) {
    let Some(part) = prefab.parts.iter().find(|p| p.slot == slot) else { return (OPEN_LIMITS, 0.0) };
    let Some((_, def)) = catalogue.parts.get_index(part.def.index()) else { return (OPEN_LIMITS, 0.0) };
    match mechanism(def) {
        None => (OPEN_LIMITS, 0.0),
        Some(Mechanism::Fixed) => (FIXED_LIMITS, 0.0),
        Some(Mechanism::Trainable { elevation_min_deg, elevation_max_deg }) => {
            let clear = |train_rad: f64, elevation_rad: f64| World::mount_path_clear(prefab, slot, train_rad, elevation_rad, catalogue);
            sweep_limits(elevation_min_deg, elevation_max_deg, &clear)
        }
    }
}

/// List the mounts of a design. Unit: `sys-ship-design`.
pub fn list_mounts(bp: &Blueprint, prefab: &VesselPrefab, catalogue: &Catalogue) -> Vec<MountInfo> {
    let auto = mount_limits_auto_set();
    let parts = PartsView::new(bp, prefab, catalogue);
    let (mounts, _, _) = mounts_from(&parts, auto, &mut |slot| auto_mount_limits(prefab, slot, catalogue));
    mounts
}

/// Turn model from the hydro prediction. Unit: `sys-ship-design`.
pub fn turn_model(prefab: &VesselPrefab, catalogue: &Catalogue) -> TurnModel {
    let (top, radius) = sim_core::hydro::predict_speed_turn(prefab, catalogue, &catalogue.tuning.buoyancy);
    turn_model_from(prefab, catalogue, top, radius)
}

/// A cache of built designs keyed by blueprint hash (sorties spawn many hulls of few classes; splash rebuilds
/// only on change). Real code.
#[derive(Default)]
pub struct DesignCache { pub designs: BTreeMap<ContentHash, Arc<ShipDesign>> }
impl DesignCache {
    pub fn get(&self, hash: ContentHash) -> Option<Arc<ShipDesign>> { self.designs.get(&hash).cloned() }
    pub fn insert(&mut self, design: Arc<ShipDesign>) { self.designs.insert(design.hash, design); }
    pub fn prefab(&self, hash: ContentHash) -> Option<Arc<VesselPrefab>> { self.designs.get(&hash).map(|d| d.prefab.clone()) }
}

// ------------------------------------------------------------------------------------------------------------
// Constants (the spec names the method, not these numbers; listed in the unit's decisions)
// ------------------------------------------------------------------------------------------------------------

/// Training samples of the sweep: 1 degree steps over the full circle (L205).
const TRAIN_SAMPLES: usize = 360;
/// The elevation floor may rise up to level fire to open bearings, never above it: a gun that cannot fire
/// level cannot hit a surface target at short and medium range.
const FLOOR_MAX_DEG: f64 = 0.0;
/// The ceiling may drop to open bearings, never below this (or the mechanical maximum when that is lower).
const CEILING_MIN_DEG: f64 = 30.0;
/// Arcs within this many 1-degree samples of the longest count as equally long; the widest elevation band
/// among them wins.
const ARC_TOLERANCE: usize = 2;
/// Diesel fuel density for full-load mass, kg per litre.
const FUEL_KG_PER_L: f64 = 0.85;

/// Limits of a mount nothing restricts.
const OPEN_LIMITS: MountLimits = MountLimits { train_min_deg: -180.0, train_max_deg: 180.0, elevation_min_deg: -90.0, elevation_max_deg: 90.0 };
/// Fixed launchers (vertical cells, boxes, racks) and decoy launchers: the munition leaves on its own course,
/// so no bearing is refused.
const FIXED_LIMITS: MountLimits = MountLimits { train_min_deg: -180.0, train_max_deg: 180.0, elevation_min_deg: 0.0, elevation_max_deg: 90.0 };

// ------------------------------------------------------------------------------------------------------------
// The sweep
// ------------------------------------------------------------------------------------------------------------

/// Mechanical movement of a mount before own structure is considered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Mechanism {
    /// Trains over the full circle and elevates between these angles (degrees). Trainable launchers sweep
    /// level only (0..0).
    Trainable { elevation_min_deg: f64, elevation_max_deg: f64 },
    /// Vertical or boxed launchers, racks and decoy launchers: nothing to sweep.
    Fixed,
}

/// Movement of a part, `None` for parts that are not mounts.
pub(crate) fn mechanism(def: &PartDef) -> Option<Mechanism> {
    match &def.function {
        PartFunction::Gun(g) => {
            let lo = if g.elevation_min_deg.is_finite() { g.elevation_min_deg.clamp(-90.0, 90.0) } else { -10.0 };
            let hi = if g.elevation_max_deg.is_finite() { g.elevation_max_deg.clamp(-90.0, 90.0) } else { 85.0 };
            Some(Mechanism::Trainable { elevation_min_deg: lo.min(hi), elevation_max_deg: lo.max(hi) })
        }
        PartFunction::Launcher(l) => match l.train_rate_dps {
            Some(r) if r.is_finite() && r > 0.0 => Some(Mechanism::Trainable { elevation_min_deg: 0.0, elevation_max_deg: 0.0 }),
            _ => Some(Mechanism::Fixed),
        },
        PartFunction::Decoy(_) => Some(Mechanism::Fixed),
        _ => None,
    }
}

/// The mechanical limits, used when the auto-set value is dropped to Suggest.
pub(crate) fn mechanical_limits(m: Mechanism) -> MountLimits {
    match m {
        Mechanism::Trainable { elevation_min_deg, elevation_max_deg } => MountLimits { train_min_deg: -180.0, train_max_deg: 180.0, elevation_min_deg, elevation_max_deg },
        Mechanism::Fixed => FIXED_LIMITS,
    }
}

/// Training bearing of sample `i`, degrees in (-180, 180].
fn bearing_deg(i: usize) -> f64 { i as f64 - (TRAIN_SAMPLES as f64 / 2.0 - 1.0) }

/// Elevation samples at 1 degree steps from `lo` to `hi`, both included.
fn elevation_samples(lo: f64, hi: f64) -> Vec<f64> {
    let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
    let steps = (hi - lo).floor().clamp(0.0, 180.0) as usize;
    let mut v: Vec<f64> = (0..=steps).map(|k| lo + k as f64).collect();
    if v.last().is_some_and(|l| hi - *l > 1e-9) { v.push(hi); }
    v
}

/// Sweep a mechanism with a path test `clear(train_rad, elevation_rad)` and pick the limits. Bearings are
/// tested in parallel; the result is merged in bearing order, so it is the same on any number of threads.
pub(crate) fn sweep_limits(elevation_min_deg: f64, elevation_max_deg: f64, clear: &(dyn Fn(f64, f64) -> bool + Sync)) -> (MountLimits, f64) {
    let elevs = elevation_samples(elevation_min_deg, elevation_max_deg);
    let blocked: Vec<Vec<bool>> = (0..TRAIN_SAMPLES).into_par_iter()
        .map(|i| { let t = bearing_deg(i).to_radians(); elevs.iter().map(|e| !clear(t, e.to_radians())).collect() })
        .collect();
    choose_limits(&elevs, &blocked)
}

/// Longest circular run of `true`: (start index, length).
fn longest_run(clear: &[bool]) -> (usize, usize) {
    let n = clear.len();
    if n == 0 { return (0, 0); }
    let Some(z) = clear.iter().position(|c| !c) else { return (0, n) };
    let (mut best, mut cur, mut start) = ((0usize, 0usize), 0usize, 0usize);
    for k in 1..=n {
        let i = (z + k) % n;
        if clear[i] {
            if cur == 0 { start = i; }
            cur += 1;
            if cur > best.1 { best = (start, cur); }
        } else { cur = 0; }
    }
    best
}

/// Pick limits from a sweep: `blocked[bearing][elevation sample]`.
pub(crate) fn choose_limits(elevs: &[f64], blocked: &[Vec<bool>]) -> (MountLimits, f64) {
    let ne = elevs.len();
    let nb = blocked.len();
    if ne == 0 || nb == 0 { return (OPEN_LIMITS, 0.0); }
    let (e_lo, e_hi) = (elevs[0], elevs[ne - 1]);
    // Blocked samples below elevation index k, per bearing.
    let prefix: Vec<Vec<u16>> = blocked.iter().map(|row| {
        let mut p = Vec::with_capacity(ne + 1);
        p.push(0u16);
        for k in 0..ne { let last = p[k]; p.push(last + u16::from(row.get(k).copied().unwrap_or(true))); }
        p
    }).collect();
    let floor_cap = FLOOR_MAX_DEG.max(e_lo);
    // (arc length, start bearing, floor index, ceiling index)
    let mut cands: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut clear = vec![false; nb];
    for kf in (0..ne).filter(|&k| elevs[k] <= floor_cap + 1e-9) {
        let ceil_min = CEILING_MIN_DEG.min(e_hi).max(elevs[kf]);
        for kc in (kf..ne).filter(|&k| elevs[k] >= ceil_min - 1e-9) {
            for (b, p) in prefix.iter().enumerate() { clear[b] = p[kc + 1] == p[kf]; }
            let (start, len) = longest_run(&clear);
            cands.push((len, start, kf, kc));
        }
    }
    let best_len = cands.iter().map(|c| c.0).max().unwrap_or(0);
    if best_len == 0 {
        // Every bearing is blocked in every band: point the mount at its least blocked bearing.
        let b = (0..nb).min_by_key(|&b| (prefix[b][ne], b)).unwrap_or(0);
        let t = bearing_deg(b);
        return (MountLimits { train_min_deg: t, train_max_deg: t, elevation_min_deg: e_lo, elevation_max_deg: e_hi }, 1.0);
    }
    let span = |c: &(usize, usize, usize, usize)| elevs[c.3] - elevs[c.2];
    let pick = cands.iter().filter(|c| c.0 + ARC_TOLERANCE >= best_len)
        .max_by(|a, b| span(a).total_cmp(&span(b)).then(b.2.cmp(&a.2)).then(a.3.cmp(&b.3)).then(a.0.cmp(&b.0)).then(b.1.cmp(&a.1)))
        .copied().unwrap_or((best_len, 0, 0, ne - 1));
    let (len, start, kf, kc) = pick;
    let (t0, t1) = if len >= nb { (-180.0, 180.0) } else { let t0 = bearing_deg(start); (t0, t0 + (len - 1) as f64) };
    let limits = MountLimits { train_min_deg: t0, train_max_deg: t1, elevation_min_deg: elevs[kf], elevation_max_deg: elevs[kc] };
    (limits, (nb - len.min(nb)) as f64 / nb as f64)
}

/// Whether auto-set mount limits are in force this release (L212). A value dropped to Suggest uses the
/// mechanical arc.
fn mount_limits_auto_set() -> bool { crate::editor::suggest::autoset_status().is_auto(crate::editor::suggest::AutoSetValue::MountLimits) }

/// Hand-set limits, sanitised; `None` when absent or unusable (non-finite values fall back to auto-set).
fn hand_set(limits: Option<&MountLimits>) -> Option<MountLimits> {
    let l = limits?;
    let ok = [l.train_min_deg, l.train_max_deg, l.elevation_min_deg, l.elevation_max_deg].iter().all(|v| v.is_finite());
    if !ok { return None; }
    let (t0, t1) = (l.train_min_deg.min(l.train_max_deg), l.train_min_deg.max(l.train_max_deg));
    let (t0, t1) = if t1 - t0 >= 360.0 { (-180.0, 180.0) } else { (t0, t1) };
    let (e0, e1) = (l.elevation_min_deg.clamp(-90.0, 90.0), l.elevation_max_deg.clamp(-90.0, 90.0));
    Some(MountLimits { train_min_deg: t0, train_max_deg: t1, elevation_min_deg: e0.min(e1), elevation_max_deg: e0.max(e1) })
}

// ------------------------------------------------------------------------------------------------------------
// Part classification
// ------------------------------------------------------------------------------------------------------------

/// Parts of a design with their definitions and placements, ascending slot.
pub(crate) struct PartsView<'a> {
    pub bp: &'a Blueprint,
    pub prefab: &'a VesselPrefab,
    pub catalogue: &'a Catalogue,
    /// (runtime, definition) for every part whose definition is in the catalogue, ascending slot.
    pub parts: Vec<(&'a PartRuntime, &'a PartDef)>,
}
impl<'a> PartsView<'a> {
    pub fn new(bp: &'a Blueprint, prefab: &'a VesselPrefab, catalogue: &'a Catalogue) -> Self {
        let mut parts: Vec<(&PartRuntime, &PartDef)> = prefab.parts.iter().filter_map(|p| catalogue.parts.get_index(p.def.index()).map(|(_, d)| (p, d))).collect();
        parts.sort_by_key(|(p, _)| p.slot);
        parts.dedup_by_key(|(p, _)| p.slot);
        Self { bp, prefab, catalogue, parts }
    }
    fn kit_of(&self, slot: PartSlot) -> Option<KitSlot> { self.bp.parts.get(slot.index()).and_then(|p| p.kit).filter(|k| k.index() < self.bp.kits.len()) }
    fn kit_parts(&self, kit: KitSlot) -> &'a [PartSlot] { self.bp.kits.get(kit.index()).map(|k| k.parts.as_slice()).unwrap_or(&[]) }
    fn def_of(&self, slot: PartSlot) -> Option<&'a PartDef> { self.parts.binary_search_by(|(p, _)| p.slot.cmp(&slot)).ok().map(|i| self.parts[i].1) }
    fn is_fire_control(&self, slot: PartSlot) -> bool { matches!(self.def_of(slot).map(|d| &d.function), Some(PartFunction::Sensor(s)) if s.kind == SensorKind::FireControlRadar) }
}

/// Zero bearing of a part from its rotation: where its +X (muzzle) axis points, about +Y, in the training
/// sense of `guidance::gun` (a quarter turn of yaw carries the bow to port, +90 degrees).
fn zero_rad(rot: content::kit::PartRot) -> f64 {
    let m = content::kit::part_rot_matrix(rot);
    let (x, z) = (m[0][0] as f64, m[0][2] as f64);
    if x == 0.0 && z == 0.0 { (rot.yaw % content::kit::YAW_COUNT) as f64 * core::f64::consts::FRAC_PI_2 } else { libm::atan2(-z, x) }
}

/// Weapon class of a launcher from its munition (or its kind when the munition is unknown); `None` for
/// launchers of non-weapons (drones, aircraft, boats, mines).
fn launcher_class(kind: LauncherKind, munition: Option<MunitionKind>) -> Option<WeaponClass> {
    match munition {
        Some(MunitionKind::AntiShipMissile) => Some(WeaponClass::AntiShipMissile),
        Some(MunitionKind::Sam) => Some(WeaponClass::Sam),
        Some(MunitionKind::PdMissile) => Some(WeaponClass::PointDefenceMissile),
        Some(MunitionKind::Torpedo) => Some(WeaponClass::Torpedo),
        Some(MunitionKind::DepthCharge) => Some(WeaponClass::DepthCharge),
        Some(_) => None,
        None => Some(match kind { LauncherKind::Missile => WeaponClass::AntiShipMissile, LauncherKind::TorpedoTube => WeaponClass::Torpedo, LauncherKind::DepthChargeRack => WeaponClass::DepthCharge }),
    }
}

/// Mounts, range tables and the auto-set limits computed, from a parts view. `auto_limits(slot)` is the sweep
/// (cached by the caller where possible); it runs only for trainable mounts without hand-set limits while
/// auto-set is in force.
pub(crate) type MountsOut = (Vec<MountInfo>, BTreeMap<PartSlot, Arc<Vec<RangeRow>>>, BTreeMap<PartSlot, (MountLimits, f64)>);
pub(crate) fn mounts_from(view: &PartsView<'_>, auto_set: bool, auto_limits: &mut dyn FnMut(PartSlot) -> (MountLimits, f64)) -> MountsOut {
    let cat = view.catalogue;
    let tuning = &cat.tuning;
    let first_fc = view.parts.iter().find(|(p, _)| view.is_fire_control(p.slot)).map(|(p, _)| p.slot);
    let mut groups: Vec<String> = Vec::new();
    let mut mounts = Vec::new();
    let mut tables = BTreeMap::new();
    let mut swept = BTreeMap::new();
    for (p, def) in &view.parts {
        let Some(mech) = mechanism(def) else { continue };
        let (weapon, munition, muzzle, pd) = match &def.function {
            PartFunction::Gun(g) => (if g.point_defence { WeaponClass::PointDefenceGun } else { WeaponClass::Gun }, None, g.muzzle_mps, g.point_defence),
            PartFunction::Launcher(l) => {
                let midx = cat.munition_idx(&l.munition);
                let mdef = midx.map(|i| cat.munition_at(i));
                let Some(w) = launcher_class(l.kind, mdef.map(|m| m.kind)) else { continue };
                let speed = mdef.map(|m| m.speed_mps).filter(|s| s.is_finite() && *s > 0.0).unwrap_or(0.0);
                (w, midx, speed, w == WeaponClass::PointDefenceMissile)
            }
            PartFunction::Decoy(_) => (WeaponClass::Decoy, None, 0.0, false),
            _ => continue,
        };
        let kit = view.kit_of(p.slot);
        let limits = match hand_set(view.bp.officers.mount_limits.get(&p.slot)) {
            Some(l) => l,
            None if auto_set && matches!(mech, Mechanism::Trainable { .. }) => { let r = auto_limits(p.slot); swept.insert(p.slot, r); r.0 }
            None => mechanical_limits(mech),
        };
        // Director: a fire-control radar in the mount's kit, else the ship's first fire-control radar when the
        // kit carries a director (which lets its guns use any fire-control radar's lock).
        let director = kit.and_then(|k| {
            let in_kit = view.kit_parts(k);
            in_kit.iter().copied().find(|s| view.is_fire_control(*s))
                .or_else(|| if in_kit.iter().any(|s| matches!(view.def_of(*s).map(|d| &d.function), Some(PartFunction::Director))) { first_fc } else { None })
        });
        let group_key = match kit.and_then(|k| view.bp.kits.get(k.index())) { Some(k) => format!("kit:{}", k.kit.0), None => format!("part:{}", def.id.0) };
        let group = match groups.iter().position(|g| *g == group_key) { Some(i) => i, None => { groups.push(group_key); groups.len() - 1 } };
        let mut m = MountInfo {
            slot: p.slot, part: p.def, weapon, munition, pos_local: p.centre, zero_rad: zero_rad(p.rot), limits, effective_range_m: 0.0, director, kit,
            group: group.min(u8::MAX as usize) as u8, muzzle_mps: if muzzle.is_finite() && muzzle > 0.0 { muzzle } else { 0.0 }, point_defence: pd,
        };
        m.effective_range_m = match &def.function {
            PartFunction::Gun(g) => match cat.bands.iter().find(|b| b.band == g.band) {
                Some(band) if m.muzzle_mps > 0.0 => {
                    let table = Arc::new(crate::guidance::gun::range_table(m.muzzle_mps, band));
                    let cap = crate::doctrine::range_cap_m(&m, cat, tuning);
                    let r = if cap > 0.0 { crate::doctrine::gun_range_from_table(&table, g.dispersion_mrad, tuning.doctrine.effective_range_pk, cap) } else { 0.0 };
                    tables.insert(p.slot, table);
                    r
                }
                _ => 0.0,
            },
            _ => crate::doctrine::effective_range_m(&m, cat, tuning),
        };
        if !m.effective_range_m.is_finite() || m.effective_range_m < 0.0 { m.effective_range_m = 0.0; }
        mounts.push(m);
    }
    (mounts, tables, swept)
}

/// The slot lists other than mounts, ascending slot.
pub(crate) struct SlotLists { pub emitters: Vec<EmitterInfo>, pub sensors: Vec<PartSlot>, pub consumers: Vec<PartSlot>, pub decoys: Vec<(PartSlot, DecoyKind)>, pub pumps: Vec<PartSlot>, pub doors: Vec<PartSlot>, pub bays: Vec<PartSlot> }

/// Emitters (radars, active sonar, jammers), sensors (every sensor part and camera mast), power consumers,
/// decoy launchers, pumps, doors and offboard bays.
pub(crate) fn slot_lists(view: &PartsView<'_>, mounts: &[MountInfo]) -> SlotLists {
    let tuning = &view.catalogue.tuning.sensors;
    let waterline = if view.prefab.waterline_y.is_finite() { view.prefab.waterline_y } else { 0.0 };
    let mut out = SlotLists { emitters: Vec::new(), sensors: Vec::new(), consumers: Vec::new(), decoys: Vec::new(), pumps: Vec::new(), doors: Vec::new(), bays: Vec::new() };
    for (p, def) in &view.parts {
        let height = if p.centre.y.is_finite() { (p.centre.y - waterline).max(0.0) } else { 0.0 };
        let powered = def.ports.iter().any(|q| q.network == NetworkKind::Power && q.role == PortRole::Consume) || def.power_priority > 0;
        if powered { out.consumers.push(p.slot); }
        match &def.function {
            PartFunction::Sensor(s) => {
                out.sensors.push(p.slot);
                let kind = match s.kind {
                    SensorKind::SearchRadar => Some(EmitterKind::SearchRadar),
                    SensorKind::NavRadar => Some(EmitterKind::NavRadar),
                    SensorKind::FireControlRadar => Some(EmitterKind::FireControlRadar),
                    SensorKind::ActiveSonar => Some(EmitterKind::ActiveSonar),
                    _ => None,
                };
                if let Some(kind) = kind {
                    let r_ref = if s.r_ref_m.is_finite() && s.r_ref_m > 0.0 { s.r_ref_m } else {
                        match s.kind {
                            SensorKind::FireControlRadar => crate::sensors::DEFAULT_FIRE_CONTROL_R_REF_M,
                            SensorKind::ActiveSonar => tuning.active_sonar_r_ref_m,
                            _ => crate::sensors::DEFAULT_SEARCH_R_REF_M,
                        }
                    };
                    let rotation_s = match s.rpm { Some(rpm) if rpm.is_finite() && rpm > 0.0 => 60.0 / rpm, _ => 0.0 };
                    let pd = kind == EmitterKind::FireControlRadar && mounts.iter().any(|m| m.point_defence && (m.director == Some(p.slot) || m.kit.is_some_and(|k| view.kit_parts(k).contains(&p.slot))));
                    out.emitters.push(EmitterInfo { slot: p.slot, kind, r_ref_m: r_ref, height_m: height, rotation_s, point_defence: pd });
                }
            }
            PartFunction::CameraMast { .. } => out.sensors.push(p.slot),
            PartFunction::Jammer { r_bt0_m } => {
                let r = if r_bt0_m.is_finite() && *r_bt0_m > 0.0 { *r_bt0_m } else { 10_000.0 };
                out.emitters.push(EmitterInfo { slot: p.slot, kind: EmitterKind::Jammer, r_ref_m: r, height_m: height, rotation_s: 0.0, point_defence: false });
            }
            PartFunction::Decoy(d) => out.decoys.push((p.slot, d.kind)),
            PartFunction::Pump { .. } => out.pumps.push(p.slot),
            PartFunction::Door => out.doors.push(p.slot),
            PartFunction::DroneBay { .. } | PartFunction::HeliDeck { .. } => out.bays.push(p.slot),
            _ => {}
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------------------
// Turn model
// ------------------------------------------------------------------------------------------------------------

/// The drive train as `sim_core::parts` runs it: engines (shaft kW, spool seconds) sharing one throttle, and
/// propulsors (most shaft kW absorbed, static N per kW) sharing the pooled power in proportion to what each
/// can absorb.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Drive { pub engines: Vec<(f64, f64)>, pub props: Vec<(f64, f64)> }
impl Drive {
    pub fn of(prefab: &VesselPrefab, catalogue: &Catalogue) -> Drive {
        let mut d = Drive::default();
        for p in &prefab.parts {
            let Some((_, def)) = catalogue.parts.get_index(p.def.index()) else { continue };
            match &def.function {
                PartFunction::Engine(e) if e.shaft_kw.is_finite() && e.shaft_kw > 0.0 => d.engines.push((e.shaft_kw, if e.spool_s.is_finite() { e.spool_s.max(0.0) } else { 0.0 })),
                PartFunction::Propulsor(q) if q.max_shaft_kw.is_finite() && q.max_shaft_kw > 0.0 && q.thrust_n_per_kw.is_finite() && q.thrust_n_per_kw > 0.0 => d.props.push((q.max_shaft_kw, q.thrust_n_per_kw)),
                _ => {}
            }
        }
        d
    }
    /// Signed shaft power the engines deliver, kW, with each engine at signed throttle `throttle(spool_s)`.
    fn pooled_kw(&self, throttle: &dyn Fn(f64) -> f64) -> f64 { self.engines.iter().map(|(kw, spool)| kw * throttle(*spool).clamp(-1.0, 1.0)).sum() }
    /// Thrust ahead in newtons with `net_kw` of signed pooled power and the hull moving ahead at `v`
    /// (`sim_core::hydro::thrust_at_speed` per propulsor; astern thrust against headway is the full astern
    /// static, which that law puts at 0.45 of the static thrust ahead).
    fn thrust_n(&self, net_kw: f64, v: f64) -> f64 {
        let cap: f64 = self.props.iter().map(|p| p.0).sum();
        if cap <= 0.0 || cap.is_nan() || net_kw == 0.0 || !net_kw.is_finite() { return 0.0; }
        let sign = net_kw.signum();
        self.props.iter().map(|(max_kw, n_per_kw)| {
            let kw = (net_kw.abs() * max_kw / cap).min(*max_kw);
            sim_core::hydro::thrust_at_speed(sign * n_per_kw * kw, kw, sign * v)
        }).sum()
    }
    fn full_kw(&self) -> f64 { self.pooled_kw(&|_| 1.0) }
}

/// Static thrust at full power in newtons: engine shaft power shared over the propulsors in proportion to
/// what each can absorb.
pub(crate) fn static_thrust_n(prefab: &VesselPrefab, catalogue: &Catalogue) -> f64 {
    let d = Drive::of(prefab, catalogue);
    d.thrust_n(d.full_kw(), 0.0).max(0.0)
}

/// Full-load mass in kilograms: dry mass plus full tanks and magazines and the design's ballast load.
pub(crate) fn full_load_kg(prefab: &VesselPrefab, catalogue: &Catalogue) -> f64 {
    let mut m = if prefab.dry_mass.mass_kg.is_finite() { prefab.dry_mass.mass_kg.max(0.0) } else { 0.0 };
    for p in &prefab.parts {
        let Some((_, def)) = catalogue.parts.get_index(p.def.index()) else { continue };
        let add = match (&def.function, &p.state) {
            (PartFunction::FuelTank { litres }, _) => litres * FUEL_KG_PER_L,
            (PartFunction::Magazine { ammo_kg, .. }, _) => *ammo_kg,
            // Ballast water is a load in sim-core's mass model (not dry mass): the design's trim load counts.
            (PartFunction::Ballast { max_kg }, sim_core::parts::PartState::Ballast { kg }) if kg.is_finite() => kg.clamp(0.0, max_kg.max(0.0)),
            _ => 0.0,
        };
        if add.is_finite() && add > 0.0 { m += add; }
    }
    m.max(1.0)
}

pub(crate) fn turn_model_from(prefab: &VesselPrefab, catalogue: &Catalogue, top_speed_mps: f64, turn_radius_m: f64) -> TurnModel {
    let fin = |v: f64| if v.is_finite() { v.max(0.0) } else { 0.0 };
    let [length, _height, beam] = prefab.dims.map(fin);
    let draft = fin(prefab.waterline_y);
    let top = fin(top_speed_mps);
    let radius = if turn_radius_m.is_finite() && turn_radius_m > 0.0 { turn_radius_m } else { (3.0 * length).max(50.0) };
    let mass = full_load_kg(prefab, catalogue);
    let drive = Drive::of(prefab, catalogue);
    let thrust = drive.thrust_n(drive.full_kw(), 0.0).max(0.0);
    let accel = if top > 0.0 { fin(thrust / mass) } else { 0.0 };
    let stop = stop_distance_m(mass, &drive, top);
    TurnModel { top_speed_mps: top, turn_radius_m: radius, accel_mps2: accel, stop_distance_m: stop, beam_m: beam, draft_m: draft, length_m: length }
}

/// Step of the crash-stop integration, seconds.
const STOP_DT_S: f64 = 0.05;
/// Longest crash stop integrated (20 minutes); a hull still moving then reports the distance so far.
const STOP_MAX_STEPS: usize = 24_000;

/// Crash-stop distance from steady speed `v` (see the module doc): the throttle runs from full ahead to full
/// astern at each engine's spool rate, as `sim_core::parts` spools it, while hull resistance, quadratic and
/// fitted so it equals the ahead thrust at `v`, and the propeller thrust at the moment's speed act on the
/// full-load mass. Fixed-step and bounded, so it is the same on every platform.
pub(crate) fn stop_distance_m(mass_kg: f64, drive: &Drive, v: f64) -> f64 {
    if !(mass_kg.is_finite() && mass_kg > 0.0 && v.is_finite() && v > 0.0) { return 0.0; }
    let ahead = drive.thrust_n(drive.full_kw(), v);
    if !(ahead.is_finite() && ahead > 0.0) { return 0.0; }
    let k = ahead / (v * v);
    let (mut u, mut x) = (v, 0.0);
    for i in 0..STOP_MAX_STEPS {
        let t = i as f64 * STOP_DT_S;
        let net_kw = drive.pooled_kw(&|spool: f64| if spool > 0.0 { 1.0 - t / spool } else { -1.0 });
        let thrust = drive.thrust_n(net_kw, u);
        let a = (thrust - k * u * u) / mass_kg;
        let next = u + a * STOP_DT_S;
        if !(next.is_finite() && a.is_finite()) { return 0.0; }
        if next <= 0.0 {
            // Stopped inside this step: add the part of the step travelled before the speed reached zero.
            x += if a < 0.0 { u * u / (-2.0 * a) } else { 0.0 };
            return x.max(0.0);
        }
        x += 0.5 * (u + next) * STOP_DT_S;
        u = next;
    }
    x.max(0.0)
}


// ------------------------------------------------------------------------------------------------------------
// Building
// ------------------------------------------------------------------------------------------------------------

/// Same geometry (hull, cells, parts and kits): the sweep of a previous build is still valid.
pub(crate) fn same_geometry(a: &Blueprint, b: &Blueprint) -> bool { a.hull == b.hull && a.cells == b.cells && a.parts == b.parts && a.kits == b.kits && a.embedded_kits == b.embedded_kits }

/// Build a design. `previous` lends its sweep results when the geometry is unchanged (live splash edits of
/// settings, rules or graphs). `sweep(prefab, slot)` is the auto-set sweep.
pub(crate) fn build_design(bp: &Blueprint, prefab: Arc<VesselPrefab>, catalogue: &Catalogue, ruleset: &Ruleset, previous: Option<&ShipDesign>, auto_set: bool, sweep: &dyn Fn(&VesselPrefab, PartSlot) -> (MountLimits, f64)) -> ShipDesign {
    let reuse = previous.filter(|p| same_geometry(&p.blueprint, bp)).map(|p| &p.auto_limits);
    let view = PartsView::new(bp, &prefab, catalogue);
    let (mounts, range_tables, auto_limits) = mounts_from(&view, auto_set, &mut |slot| {
        if let Some(r) = reuse.and_then(|m| m.get(&slot)) { return *r; }
        sweep(&prefab, slot)
    });
    let lists = slot_lists(&view, &mounts);
    let (top, radius) = sim_core::hydro::predict_speed_turn(&prefab, catalogue, &catalogue.tuning.buoyancy);
    let turn = turn_model_from(&prefab, catalogue, top, radius);
    let stats = content::stats::compute_stats(bp, catalogue);
    let blueprint = Arc::new(bp.clone());
    let graph = {
        let facts = DesignFacts {
            blueprint: &blueprint, prefab: &prefab, catalogue, mounts: &mounts, emitters: &lists.emitters, sensor_slots: &lists.sensors,
            consumer_slots: &lists.consumers, decoys: &lists.decoys, pumps: &lists.pumps, doors: &lists.doors,
        };
        Arc::new(crate::graph::compile::compile(&facts, &bp.graph, &bp.rules, ruleset))
    };
    ShipDesign {
        hash: content::blueprint::io::compute_hash(bp),
        prefab: prefab.clone(),
        settings: bp.officers.clone(),
        rules: bp.rules.clone(),
        overrides: bp.graph.clone(),
        graph,
        mounts,
        emitters: lists.emitters,
        sensor_slots: lists.sensors,
        consumer_slots: lists.consumers,
        decoys: lists.decoys,
        pumps: lists.pumps,
        doors: lists.doors,
        bays: lists.bays,
        range_tables,
        turn,
        cost: stats.cost,
        tonnage_t: if stats.tonnage_t.is_finite() { stats.tonnage_t.max(0.0) } else { 0.0 },
        blueprint,
        compiled_under: compile_key(ruleset),
        auto_limits,
    }
}

/// [`ShipDesign::build`] reusing a previous design's sweep when the geometry is unchanged.
pub(crate) fn rebuild(bp: &Blueprint, prefab: Arc<VesselPrefab>, catalogue: &Catalogue, ruleset: &Ruleset, previous: &ShipDesign) -> ShipDesign {
    let auto = mount_limits_auto_set();
    build_design(bp, prefab, catalogue, ruleset, Some(previous), auto, &|p: &VesselPrefab, slot| auto_mount_limits(p, slot, catalogue))
}

/// A copy of a design whose graph is compiled under `ruleset` (a cached design reused under other rules).
pub(crate) fn under_rules(design: &Arc<ShipDesign>, catalogue: &Catalogue, ruleset: &Ruleset) -> Arc<ShipDesign> {
    if design.compiled_for(ruleset) { return design.clone(); }
    let mut d = (**design).clone();
    d.graph = design.recompile(&design.settings, &design.rules, &design.overrides, catalogue, ruleset);
    d.compiled_under = compile_key(ruleset);
    Arc::new(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deg(v: f64) -> f64 { v.to_radians() }

    #[test]
    fn elevation_samples_cover_both_ends() {
        let s = elevation_samples(-15.0, 85.0);
        assert_eq!(s.len(), 101);
        assert_eq!(s[0], -15.0);
        assert_eq!(*s.last().unwrap(), 85.0);
        let s = elevation_samples(-10.5, 2.0);
        assert_eq!(s.first().copied(), Some(-10.5));
        assert_eq!(s.last().copied(), Some(2.0));
        assert_eq!(elevation_samples(0.0, 0.0), vec![0.0]);
    }

    #[test]
    fn bearings_span_the_circle_once() {
        assert_eq!(bearing_deg(0), -179.0);
        assert_eq!(bearing_deg(TRAIN_SAMPLES - 1), 180.0);
    }

    #[test]
    fn open_deck_gives_the_full_circle() {
        let (l, blocked) = sweep_limits(-15.0, 85.0, &|_, _| true);
        assert_eq!((l.train_min_deg, l.train_max_deg), (-180.0, 180.0));
        assert_eq!((l.elevation_min_deg, l.elevation_max_deg), (-15.0, 85.0));
        assert_eq!(blocked, 0.0);
    }

    #[test]
    fn structure_astern_is_cut_out_when_it_blocks_every_elevation() {
        let clear = |t: f64, _e: f64| t.abs() <= deg(150.0) + 1e-9;
        let (l, blocked) = sweep_limits(-10.0, 80.0, &clear);
        assert_eq!((l.train_min_deg, l.train_max_deg), (-150.0, 150.0));
        assert_eq!((l.elevation_min_deg, l.elevation_max_deg), (-10.0, 80.0));
        assert!((blocked - 59.0 / 360.0).abs() < 1e-12, "{blocked}");
    }

    #[test]
    fn chosen_limits_never_contain_a_blocked_sample() {
        // An irregular obstruction pattern: whatever the sweep picks, every sample inside is clear.
        let clear = |t: f64, e: f64| { let (td, ed) = (t.to_degrees(), e.to_degrees()); !((td > 100.0 && td < 130.0 && ed < 20.0) || (td < -60.0 && td > -75.0) || (td.abs() < 8.0 && ed < -3.0)) };
        let (l, _) = sweep_limits(-10.0, 85.0, &clear);
        let elevs = elevation_samples(-10.0, 85.0);
        for i in 0..TRAIN_SAMPLES {
            let b = bearing_deg(i);
            let inside = l.train_min_deg + (b - l.train_min_deg).rem_euclid(360.0) <= l.train_max_deg + 1e-9;
            if !inside { continue; }
            for e in elevs.iter().filter(|e| **e >= l.elevation_min_deg - 1e-9 && **e <= l.elevation_max_deg + 1e-9) {
                assert!(clear(deg(b), deg(*e)), "blocked sample inside limits at {b}, {e}: {l:?}");
            }
        }
    }

    #[test]
    fn low_obstruction_raises_the_floor_instead_of_losing_the_arc() {
        // A deckhouse ahead blocks depression over 59 bearings; level fire and above is clear.
        let clear = |t: f64, e: f64| !(t.abs() < deg(30.0) && e < -1e-9);
        let (l, blocked) = sweep_limits(-10.0, 85.0, &clear);
        assert_eq!((l.train_min_deg, l.train_max_deg), (-180.0, 180.0));
        assert_eq!((l.elevation_min_deg, l.elevation_max_deg), (0.0, 85.0));
        assert_eq!(blocked, 0.0);
    }

    #[test]
    fn floor_never_rises_above_level_fire() {
        // A tall bridge ahead blocks everything below 10 degrees: the bearings are cut out instead.
        let clear = |t: f64, e: f64| !(t.abs() < deg(20.0) && e < deg(10.0));
        let (l, blocked) = sweep_limits(-10.0, 85.0, &clear);
        assert_eq!(l.elevation_min_deg, -10.0);
        assert!((blocked - 39.0 / 360.0).abs() < 1e-12, "{blocked}");
        // The arc runs from 20 to 340 (port round to starboard, the long way).
        assert_eq!((l.train_min_deg, l.train_max_deg), (20.0, 340.0));
    }

    #[test]
    fn mast_overhead_lowers_the_ceiling() {
        let clear = |t: f64, e: f64| !(t.abs() < deg(5.0) && e > deg(40.0) + 1e-9 && e < deg(70.0));
        let (l, blocked) = sweep_limits(-10.0, 85.0, &clear);
        assert_eq!((l.train_min_deg, l.train_max_deg), (-180.0, 180.0));
        assert_eq!((l.elevation_min_deg, l.elevation_max_deg), (-10.0, 40.0));
        assert_eq!(blocked, 0.0);
    }

    #[test]
    fn two_sectors_keep_the_larger_and_count_the_other_as_blocked() {
        // Bridge ahead and funnel astern at every elevation; the port sector (positive training) is larger.
        let clear = |t: f64, _e: f64| { let d = t.to_degrees().round(); !(d.abs() < 30.0 || d.abs() > 160.0 || (-40.0..=-30.0).contains(&d)) };
        let (l, blocked) = sweep_limits(-10.0, 85.0, &clear);
        assert_eq!((l.train_min_deg, l.train_max_deg), (30.0, 160.0));
        assert!((blocked - 229.0 / 360.0).abs() < 1e-12, "{blocked}");
    }

    #[test]
    fn fully_enclosed_mount_reports_everything_blocked() {
        let (l, blocked) = sweep_limits(-10.0, 85.0, &|_, _| false);
        assert_eq!(blocked, 1.0);
        assert_eq!(l.train_min_deg, l.train_max_deg);
    }

    #[test]
    fn trainable_launcher_sweeps_level_only() {
        // A deck-edge tube: inboard (starboard side, negative training) crosses the deck.
        let clear = |t: f64, e: f64| { assert_eq!(e, 0.0); t >= 0.0 };
        let (l, blocked) = sweep_limits(0.0, 0.0, &clear);
        assert_eq!((l.train_min_deg, l.train_max_deg), (0.0, 180.0));
        assert_eq!((l.elevation_min_deg, l.elevation_max_deg), (0.0, 0.0));
        assert!((blocked - 179.0 / 360.0).abs() < 1e-12);
    }

    #[test]
    fn sweep_is_deterministic() {
        let clear = |t: f64, e: f64| (libm::sin(3.0 * t) + libm::cos(2.0 * e)) > -0.2;
        let a = sweep_limits(-15.0, 85.0, &clear);
        let b = sweep_limits(-15.0, 85.0, &clear);
        assert_eq!(a.0, b.0);
        assert_eq!(a.1.to_bits(), b.1.to_bits());
    }

    #[test]
    fn longest_run_wraps_around() {
        let mut v = vec![true; 10];
        v[4] = false;
        assert_eq!(longest_run(&v), (5, 9));
        assert_eq!(longest_run(&[false, false]), (0, 0));
        assert_eq!(longest_run(&[true, true]), (0, 2));
        assert_eq!(longest_run(&[true, false, true, true, false, true]).1, 2);
    }

    #[test]
    fn hand_set_limits_are_sanitised() {
        assert_eq!(hand_set(None), None);
        let bad = MountLimits { train_min_deg: f64::NAN, train_max_deg: 10.0, elevation_min_deg: 0.0, elevation_max_deg: 10.0 };
        assert_eq!(hand_set(Some(&bad)), None);
        let swapped = MountLimits { train_min_deg: 90.0, train_max_deg: -90.0, elevation_min_deg: 120.0, elevation_max_deg: -5.0 };
        let l = hand_set(Some(&swapped)).unwrap();
        assert_eq!((l.train_min_deg, l.train_max_deg, l.elevation_min_deg, l.elevation_max_deg), (-90.0, 90.0, -5.0, 90.0));
    }

    #[test]
    fn zero_bearing_follows_yaw() {
        use content::kit::PartRot;
        assert!(zero_rad(PartRot { yaw: 0, tilt: 0 }).abs() < 1e-12);
        assert!((zero_rad(PartRot { yaw: 1, tilt: 0 }) - core::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!((zero_rad(PartRot { yaw: 2, tilt: 0 }).abs() - core::f64::consts::PI).abs() < 1e-12);
    }

    #[test]
    fn stop_distance_follows_the_drive_train() {
        // Instant engine reversal: full static thrust astern against k v^2 from the start, the closed form
        // d = m / (2k) ln(1 + k v^2 / T0).
        let instant = Drive { engines: vec![(1_000.0, 0.0)], props: vec![(1_000.0, 100.0)] };
        let (m, v) = (100_000.0, 10.0);
        // The hydro law gives 0.45 of the 100 kN static thrust astern.
        let t0 = -instant.thrust_n(-1_000.0, 0.0);
        assert!((t0 - 45_000.0).abs() < 1e-6, "{t0}");
        let k = instant.thrust_n(1_000.0, v) / (v * v);
        let expect = m / (2.0 * k) * libm::log1p(k * v * v / t0);
        let d = stop_distance_m(m, &instant, v);
        assert!((d - expect).abs() < 0.01 * expect, "{d} vs {expect}");
        // Thrust falls with speed, so the drag fit is far below static thrust and the stop is longer than a
        // static-thrust fit would say.
        let ahead_static = 100_000.0;
        let static_fit = m / (2.0 * ahead_static / (v * v)) * libm::log1p(ahead_static / t0);
        assert!(d > 1.1 * static_fit, "{d} vs {static_fit}");
        // A 10 s spool coasts on for about 10 s before the astern thrust builds: roughly v * 10 m further.
        let spooled = Drive { engines: vec![(1_000.0, 10.0)], ..instant.clone() };
        let ds = stop_distance_m(m, &spooled, v);
        assert!(ds > d + 0.6 * v * 10.0 && ds < d + 1.5 * v * 10.0, "{ds} vs {d}");
        // Bad input gives zero, never a panic or NaN.
        assert_eq!(stop_distance_m(m, &Drive::default(), v), 0.0);
        assert_eq!(stop_distance_m(f64::NAN, &instant, v), 0.0);
        assert_eq!(stop_distance_m(m, &instant, 0.0), 0.0);
    }

    #[test]
    fn predicted_crash_stop_matches_the_physics() {
        // The 22 m starter at its predicted top speed: run full ahead to steady, then full astern, in a calm
        // World, and compare the distance run with the turn model's.
        let cat = Arc::new(content::catalogue::catalogue().clone());
        let bp = content::campaign::load::stock_blueprint("starters/fac22.bp.ron").unwrap();
        let prefab = Arc::new(sim_core::vessel::build_prefab(&bp, &cat));
        let tm = turn_model(&prefab, &cat);
        assert!(tm.top_speed_mps > 5.0 && tm.stop_distance_m > 0.0, "{tm:?}");
        let weather = WeatherState { sea_state: content::weather::SeaState::CALM, ..Default::default() };
        let map = content::campaign::MapDef { size_m: [40_000.0, 40_000.0], depth_m: 200.0, ..Default::default() };
        let mut w = World::new(sim_core::WorldConfig { seed: 1, weather, map, catalogue: cat.clone() });
        let spawn = sim_core::vessel::VesselSpawn {
            prefab: prefab.clone(), side: SideId(0), player: None, class: None, pose: Pose::from_heading(DVec3::ZERO, 0.0), speed_mps: tm.top_speed_mps,
            damage: None, fuel_fraction: 1.0, ammo_fraction: 1.0, fidelity: sim_core::vessel::Fidelity::Full, submersible: false,
        };
        let id = w.spawn_vessel(&spawn);
        w.set_controls(id, sim_core::parts::VesselControls { throttle: 1.0, ..Default::default() });
        for _ in 0..30 * sim_core::time::TICK_HZ as usize { w.step(); }
        let p0 = w.vessel(id).unwrap().kin.pose.pos;
        w.set_controls(id, sim_core::parts::VesselControls { throttle: -1.0, ..Default::default() });
        let mut n = 0;
        while w.vessel(id).unwrap().forward_speed() > 0.05 && n < 120 * sim_core::time::TICK_HZ as usize { w.step(); n += 1; }
        let run = (w.vessel(id).unwrap().kin.pose.pos - p0).length();
        let ratio = run / tm.stop_distance_m;
        assert!((0.75..=1.25).contains(&ratio), "physics stops in {run} m, turn model says {} m", tm.stop_distance_m);
    }

    #[test]
    fn power_is_shared_over_propulsors_by_what_each_absorbs() {
        // 3 MW of engines on two shafts that absorb 1 MW and 3 MW: shares 0.75 MW and 2.25 MW.
        let d = Drive { engines: vec![(2_000.0, 5.0), (1_000.0, 5.0)], props: vec![(1_000.0, 100.0), (3_000.0, 50.0)] };
        assert!((d.thrust_n(d.full_kw(), 0.0) - (750.0 * 100.0 + 2_250.0 * 50.0)).abs() < 1e-6);
        // Astern at rest is 0.45 of that thrust, reversed (the hydro law); astern against headway stays there.
        assert!((d.thrust_n(-d.full_kw(), 0.0) + 0.45 * d.thrust_n(d.full_kw(), 0.0)).abs() < 1e-6);
        assert!((d.thrust_n(-d.full_kw(), 8.0) - d.thrust_n(-d.full_kw(), 0.0)).abs() < 1e-6);
        assert!(d.thrust_n(d.full_kw(), 8.0) < d.thrust_n(d.full_kw(), 0.0));
    }

    #[test]
    fn launcher_classes() {
        assert_eq!(launcher_class(LauncherKind::Missile, Some(MunitionKind::Sam)), Some(WeaponClass::Sam));
        assert_eq!(launcher_class(LauncherKind::Missile, Some(MunitionKind::Drone)), None);
        assert_eq!(launcher_class(LauncherKind::TorpedoTube, None), Some(WeaponClass::Torpedo));
    }
}
