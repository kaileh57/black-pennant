//! The eight Suggest computations. Each returns `None` when the design already matches its proposal.
use super::place::{upright, Space, MAX_RADIUS};
use super::{SuggestKind, Suggestion};
use crate::editor::gauges::calc;
use crate::editor::notes::edit::{footprint, part_cells};
use crate::editor::BlueprintEdit;
use crate::prelude::*;
use crate::ship::ShipDesign;
use content::blueprint::{Blueprint, PartPlacement};
use content::cell::{ArmourTier, CellGrid, CellPos, Material, CELL_M};
use content::part::{EngineKind, PartDef, PartFunction, SensorKind};
use sim_core::vessel::VesselPrefab;

pub(crate) const KEYS: &[&str] = &[
    "suggest.aggression",
    "suggest.armour",
    "suggest.ballast",
    "suggest.ballast_add",
    "suggest.bulkhead",
    "suggest.emcon_trigger",
    "suggest.engine_swap",
    "suggest.fuel",
    "suggest.generator_add",
    "suggest.generator_remove",
    "suggest.power_and_engine",
    "suggest.sensor",
];

fn m(key: &str) -> Msg { Msg::new(key) }
fn fin(x: f64) -> f64 { if x.is_finite() { x } else { 0.0 } }
fn pos(x: f64) -> f64 { if x.is_finite() && x > 0.0 { x } else { 0.0 } }
fn num(msg: Msg, v: f64, u: Unit) -> Msg { msg.num(fin(v), u) }

/// Parts a proposal may add: in the build and not retired.
fn offered<'c>(cat: &'c Catalogue) -> impl Iterator<Item = &'c PartDef> + 'c {
    cat.parts.values().filter(|d| d.replaced_by.is_none() && d.phase <= content::catalogue::BUILD_PHASE)
}

fn suggestion(kind: SuggestKind, edit: BlueprintEdit, text: Msg) -> Option<Suggestion> { Some(Suggestion { kind, edit, text }) }

/// Several edits, or the single one.
fn several(mut v: Vec<BlueprintEdit>) -> Option<BlueprintEdit> {
    match v.len() { 0 => None, 1 => v.pop(), _ => Some(BlueprintEdit::Several(v)) }
}

// ---------------------------------------------------------------------------------------------------------
// Ballast and trim.

/// One ballast store the solver may load.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BallastStore { pub centre: DVec3, pub max_kg: f64, pub now_kg: f64 }

/// The state the solver corrects: loaded mass and centre of gravity, GM, centre of buoyancy and centreline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrimState { pub mass_kg: f64, pub com: DVec3, pub gm_m: f64, pub cb: DVec3, pub centre_z: f64 }

/// Trim and heel weigh three times GM in the residual, so when the stores cannot do everything the ship still
/// comes level first.
const MOMENT_WEIGHTS: [f64; 3] = [3.0, 1.0, 3.0];
/// A ballast proposal must remove at least this share of the weighted moment residual; smaller changes are
/// noise the player would only undo.
pub(crate) const MIN_IMPROVEMENT: f64 = 0.25;
/// Lowest cell layers searched at each station for a new ballast part's spot.
const BALLAST_LAYERS: usize = 1;
/// Most ballast parts one proposal adds.
const MAX_ADDED_BALLAST: usize = 4;
/// The plan stops adding parts once the residual is down to this share of where it started.
const DONE_RESIDUAL: f64 = 0.05;
/// An added part must remove at least this share of the starting residual.
const PART_EARNS: f64 = 0.1;
/// Rounds of plan, check on the real hydrostatics, and correct.
const BALLAST_ROUNDS: usize = 3;

/// Where the centre of gravity should go: over the centre of buoyancy, on the centreline, at the height that
/// gives `target_gm` with the metacentre held.
fn plan_target(s: &TrimState, target_gm: f64) -> [f64; 3] { [s.cb.x, s.com.y + s.gm_m - target_gm, s.centre_z] }

/// Weighted moment residual (kg m) of a plan: zero when the centre of gravity sits on the target.
pub(crate) fn plan_residual(s: &TrimState, target_gm: f64, stores: &[BallastStore], w: &[f64]) -> f64 {
    let t = plan_target(s, target_gm);
    let com = [s.com.x, s.com.y, s.com.z];
    let mut res = [0.0; 3];
    for k in 0..3 { res[k] = s.mass_kg * (com[k] - t[k]) * MOMENT_WEIGHTS[k]; }
    for (b, &nw) in stores.iter().zip(w) {
        let c = [b.centre.x, b.centre.y, b.centre.z];
        let d = nw - b.now_kg.clamp(0.0, pos(b.max_kg));
        for k in 0..3 { res[k] += d * (c[k] - t[k]) * MOMENT_WEIGHTS[k]; }
    }
    fin(libm::sqrt(res.iter().map(|r| r * r).sum::<f64>()))
}

/// Bounded least squares on the longitudinal, vertical and transverse moments: loads that put the centre of
/// gravity over the centre of buoyancy, on the centreline, and at the height that gives `target_gm` (the
/// metacentre held where it is). Projected coordinate descent, fixed sweep count, deterministic.
pub(crate) fn ballast_plan(s: &TrimState, target_gm: f64, stores: &[BallastStore]) -> Vec<f64> {
    let t = plan_target(s, target_gm);
    let wt = MOMENT_WEIGHTS;
    let r = |v: DVec3| [v.x, v.y, v.z];
    let com = r(s.com);
    let mut w: Vec<f64> = stores.iter().map(|b| b.now_kg.clamp(0.0, pos(b.max_kg))).collect();
    let a: Vec<[f64; 3]> = stores.iter().map(|b| { let c = r(b.centre); [(c[0] - t[0]) * wt[0], (c[1] - t[1]) * wt[1], (c[2] - t[2]) * wt[2]] }).collect();
    // Residuals: m (com - t) + sum (w - w0) a.
    let mut res = [0.0; 3];
    for k in 0..3 { res[k] = s.mass_kg * (com[k] - t[k]) * wt[k]; }
    for _ in 0..5_000 {
        let mut moved = 0.0;
        for i in 0..stores.len() {
            let aa: f64 = a[i].iter().map(|x| x * x).sum();
            if !(aa > 1e-12) { continue; }
            let g: f64 = (0..3).map(|k| a[i][k] * res[k]).sum();
            let nw = (w[i] - g / aa).clamp(0.0, pos(stores[i].max_kg));
            let d = nw - w[i];
            if d != 0.0 { for k in 0..3 { res[k] += d * a[i][k]; } w[i] = nw; moved += d.abs(); }
        }
        if moved < 1e-4 { break; }
    }
    w
}

/// The trim state after stores take loads `w` (the metacentre held). Exactly the linear model the plan
/// minimises: the residual of the result with no stores equals the plan's residual.
fn with_loads(s: &TrimState, stores: &[BallastStore], w: &[f64]) -> TrimState {
    let mut mass = s.mass_kg;
    let mut first = s.com * s.mass_kg;
    for (b, &nw) in stores.iter().zip(w) {
        let d = nw - b.now_kg.clamp(0.0, pos(b.max_kg));
        mass += d;
        first += b.centre * d;
    }
    if !(mass > 0.0) { return *s; }
    let com = first / mass;
    TrimState { mass_kg: mass, com, gm_m: s.gm_m + (s.com.y - com.y), ..*s }
}

/// The trim state with an empty part of `dry_kg` added at `centre` (the metacentre held).
fn with_dry_part(s: &TrimState, centre: DVec3, dry_kg: f64) -> TrimState {
    let m = pos(dry_kg);
    if m <= 0.0 || !centre.is_finite() { return *s; }
    let mass = s.mass_kg + m;
    let com = (s.com * s.mass_kg + centre * m) / mass;
    // KM = KG + GM is held, so GM moves by the change in KG.
    TrimState { mass_kg: mass, com, gm_m: s.gm_m + (s.com.y - com.y), ..*s }
}

/// Where the centre of gravity must sit, along and across the hull, for the ship to float with no trim and
/// no heel, measured on the simulation's own hydrostatics: one secant step from a probe with G shifted 1% of
/// the length (or beam). The centre of buoyancy of the trimmed attitude is not the target, because there G
/// and B already share a vertical. Without a usable response (a hull that does not float, or one with no
/// positive GM for heel) it falls back to that centre of buoyancy and to the centreline.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LevelTargets { x: f64, z: f64, /// Trim and heel response, radians per metre of G shift.
    k_trim: Option<f64>, k_heel: Option<f64> }

fn level_targets(prefab: &VesselPrefab, mass: &sim_core::mass::MassProperties, eq: &sim_core::hydro::Equilibrium, cat: &Catalogue) -> LevelTargets {
    let centre_z = prefab.grid.size()[2] as f64 * CELL_M * 0.5;
    let fallback_x = if eq.cb.is_finite() { eq.cb.x } else { mass.com.x };
    let mut out = LevelTargets { x: fallback_x, z: centre_z, k_trim: None, k_heel: None };
    if !eq.floats || !mass.com.is_finite() { return out; }
    let tuning = &cat.tuning.buoyancy;
    let probe = |shift: DVec3| {
        let m = sim_core::mass::MassProperties { com: mass.com + shift, ..*mass };
        sim_core::hydro::equilibrium(&prefab.blocks, &m, &[], &prefab.compartments, tuning)
    };
    let slope = |a: f64, b: f64, step: f64| { let k = (fin(b) - fin(a)) / step; (k.is_finite() && k.abs() > 1e-9).then_some(k) };
    let dx = (0.01 * pos(prefab.dims[0])).max(CELL_M);
    let p = probe(DVec3::new(dx, 0.0, 0.0));
    if p.floats {
        out.k_trim = slope(eq.trim_rad, p.trim_rad, dx);
        if let Some(k) = out.k_trim { out.x = (mass.com.x - fin(eq.trim_rad) / k).clamp(0.0, pos(prefab.dims[0])); }
    }
    if eq.gm_m > 0.0 {
        let dz = (0.01 * pos(prefab.dims[2])).max(CELL_M * 0.25);
        let p = probe(DVec3::new(0.0, 0.0, dz));
        if p.floats {
            out.k_heel = slope(eq.heel_rad, p.heel_rad, dz);
            if let Some(k) = out.k_heel { out.z = (mass.com.z - fin(eq.heel_rad) / k).clamp(0.0, pos(prefab.dims[2]).max(centre_z * 2.0)); }
        }
    }
    out
}

/// GM after a plan, with the metacentre held: KM - KG'.
pub(crate) fn planned_gm(s: &TrimState, stores: &[BallastStore], w: &[f64]) -> f64 {
    let km = s.com.y + s.gm_m;
    let mut mass = s.mass_kg;
    let mut my = s.mass_kg * s.com.y;
    for (b, &nw) in stores.iter().zip(w) { let d = nw - b.now_kg; mass += d; my += d * b.centre.y; }
    if mass > 0.0 { km - my / mass } else { s.gm_m }
}

/// How far a floating state is from what the ballast Suggest aims for, in units of the thresholds: degrees of
/// trim and heel past 0.5, plus GM outside the Pass band in steps of 0.5% of beam. Zero means nothing to do;
/// a hull that does not float scores as badly as any.
pub(crate) fn trim_error(eq: &sim_core::hydro::Equilibrium, beam_m: f64, lo: f64, hi: f64) -> f64 {
    if !eq.floats || !(beam_m > 0.0) { return 1e9; }
    let over = |deg: f64| (fin(deg).abs() - 0.5).max(0.0) / 0.5;
    let r = fin(eq.gm_m) / beam_m;
    let band = if r < lo { lo - r } else if r > hi { r - hi } else { 0.0 };
    over(eq.trim_rad.to_degrees()) + over(eq.heel_rad.to_degrees()) + band / 0.005
}

/// Mass properties with point masses added (negative to remove); inertia is left as it was, which the static
/// equilibrium does not read.
fn with_masses(m: &sim_core::mass::MassProperties, add: &[(DVec3, f64)]) -> sim_core::mass::MassProperties {
    let mut mass = m.mass_kg;
    let mut first = m.com * m.mass_kg;
    for &(c, kg) in add { if c.is_finite() && kg.is_finite() { mass += kg; first += c * kg; } }
    if !(mass > 0.0) { return *m; }
    sim_core::mass::MassProperties { mass_kg: mass, com: first / mass, ..*m }
}

/// One ballast plan: the fitted-load changes, then the parts added in order.
#[derive(Clone, Debug, Default)]
struct BallastPlan {
    fitted_edits: Vec<BlueprintEdit>,
    /// Point masses of the fitted-load changes.
    fitted_masses: Vec<(DVec3, f64)>,
    /// Ballast in the fitted stores after the plan, kg.
    fitted_aboard: f64,
    /// (edit, point mass of the part with its load, ballast load in kg)
    adds: Vec<(BlueprintEdit, (DVec3, f64), f64)>,
}

/// Plan loads and added parts on the moment model for G targets `state.cb.x`, `state.centre_z` and
/// `target_gm`. Parts are added only when `may_add`.
fn plan_ballast(bp: &Blueprint, cat: &Catalogue, state: &TrimState, target_gm: f64, stores: &[(PartSlot, BallastStore)], may_add: bool) -> BallastPlan {
    let before = plan_residual(state, target_gm, &[], &[]);
    let mut plan = BallastPlan { fitted_aboard: stores.iter().map(|(_, b)| b.now_kg).sum(), ..Default::default() };
    let mut cur = *state;
    if !stores.is_empty() {
        let only: Vec<BallastStore> = stores.iter().map(|(_, b)| *b).collect();
        let w: Vec<f64> = ballast_plan(state, target_gm, &only).iter().map(|v| libm::round(*v)).collect();
        for ((slot, b), &nw) in stores.iter().zip(&w) {
            if (nw - b.now_kg).abs() < 1.0 { continue; }
            plan.fitted_edits.push(BlueprintEdit::SetLoad { slot: *slot, load: nw });
            plan.fitted_masses.push((b.centre, nw - b.now_kg));
        }
        cur = with_loads(state, &only, &w);
        plan.fitted_aboard = w.iter().sum();
    }
    if !may_add { return plan; }
    let mut defs: Vec<&PartDef> = offered(cat).filter(|d| matches!(d.function, PartFunction::Ballast { .. })).collect();
    // Largest part first, then id.
    defs.sort_by(|a, b| ballast_max(b).partial_cmp(&ballast_max(a)).unwrap_or(core::cmp::Ordering::Equal).then_with(|| a.id.cmp(&b.id)));
    let mut work: Option<Blueprint> = None;
    for _ in 0..MAX_ADDED_BALLAST {
        let now = plan_residual(&cur, target_gm, &[], &[]);
        if now <= before * DONE_RESIDUAL { break; }
        let space = Space::new(work.as_ref().unwrap_or(bp), cat);
        // (residual after, part, minimum corner, part centre, load, state after)
        let mut best: Option<(f64, &PartDef, CellPos, DVec3, f64, TrimState)> = None;
        for def in &defs {
            for at in space.low_spots(def, upright(), BALLAST_LAYERS) {
                let Some(cells) = footprint(def, at, upright()) else { continue };
                let centre = cells.iter().fold(DVec3::ZERO, |s, c| s + DVec3::from_array(c.centre_m())) / cells.len().max(1) as f64;
                let st = with_dry_part(&cur, centre, def.mass_kg);
                let store = [BallastStore { centre, max_kg: ballast_max(def), now_kg: 0.0 }];
                let kg = libm::round(ballast_plan(&st, target_gm, &store)[0]);
                if kg < 1.0 { continue; }
                let after = with_loads(&st, &store, &[kg]);
                let r = plan_residual(&after, target_gm, &[], &[]);
                if best.is_none_or(|b| r < b.0) { best = Some((r, *def, at, centre, kg, after)); }
            }
            if best.is_some() { break; }
        }
        // Each added part must earn its place.
        let Some((r, def, at, centre, kg, after)) = best else { break };
        if now - r < before * PART_EARNS { break; }
        let add = BlueprintEdit::AddPart(PartPlacement { part: def.id.clone(), pos: at, rot: upright(), kit: None, label: None, load: Some(kg) });
        let Some(next) = crate::editor::notes::apply_edit(work.as_ref().unwrap_or(bp), &add, cat) else { break };
        work = Some(next);
        plan.adds.push((add, (centre, pos(def.mass_kg) + kg), kg));
        cur = after;
    }
    plan
}

pub(crate) fn ballast_and_trim(bp: &Blueprint, design: &ShipDesign, cat: &Catalogue) -> Option<Suggestion> {
    let prefab = &design.prefab;
    let tuning = &cat.tuning.buoyancy;
    let mass = calc::loaded_mass(prefab, cat);
    let solve = |m: &sim_core::mass::MassProperties| sim_core::hydro::equilibrium(&prefab.blocks, m, &[], &prefab.compartments, tuning);
    let eq = solve(&mass);
    let beam = pos(prefab.dims[2]);
    if beam <= 0.0 || !(mass.mass_kg > 0.0) { return None; }
    let st = &cat.tuning.stability;
    let (lo, hi) = (pos(st.gm_caution_low), pos(st.gm_caution_high).max(pos(st.gm_caution_low)));
    let ratio = fin(eq.gm_m) / beam;
    let gm_ok = eq.floats && ratio >= lo && ratio <= hi;
    let level = fin(eq.trim_rad).to_degrees().abs() < 0.5 && fin(eq.heel_rad).to_degrees().abs() < 0.5;
    if gm_ok && level { return None; }
    let want_gm = if gm_ok { fin(eq.gm_m) } else { 0.5 * (lo + hi) * beam };
    let lt = level_targets(prefab, &mass, &eq, cat);
    let mut stores = Vec::new();
    for p in &prefab.parts {
        let Some(def) = calc::def_of(cat, p) else { continue };
        let PartFunction::Ballast { max_kg } = def.function else { continue };
        // An unset ballast load is full, as the stats unit reads it.
        let now = bp.parts.get(p.slot.index()).and_then(|b| b.load).filter(|v| v.is_finite()).unwrap_or(pos(max_kg));
        stores.push((p.slot, BallastStore { centre: p.centre, max_kg: pos(max_kg), now_kg: now.clamp(0.0, pos(max_kg)) }));
    }
    // A stiff hull (GM too high) is not helped by weight low, so parts are added only for a tender hull or to
    // level trim and heel.
    let may_add = gm_ok || ratio <= hi;
    let e0 = trim_error(&eq, beam, lo, hi);
    // The moment model holds the metacentre still; the hull does not. Each round checks every stage of the
    // plan (the fitted loads, then each added part) on the real hydrostatics and corrects the model's targets
    // by what it missed; the stage that floats best over all rounds is offered (fewest edits on a tie), and
    // only when it removes a quarter of the error.
    let (mut gm_aim, mut x_aim, mut z_aim) = (want_gm, lt.x, lt.z);
    // (error, plan, stage, equilibrium)
    let mut best: Option<(f64, BallastPlan, usize, sim_core::hydro::Equilibrium)> = None;
    for _ in 0..BALLAST_ROUNDS {
        let state = TrimState { mass_kg: mass.mass_kg, com: mass.com, gm_m: fin(eq.gm_m), cb: DVec3::new(x_aim, if eq.cb.is_finite() { eq.cb.y } else { mass.com.y }, z_aim), centre_z: z_aim };
        let plan = plan_ballast(bp, cat, &state, gm_aim, &stores, may_add);
        let first = if plan.fitted_edits.is_empty() { 1 } else { 0 };
        let mut full: Option<sim_core::hydro::Equilibrium> = None;
        for k in first..=plan.adds.len() {
            let mut pts = plan.fitted_masses.clone();
            pts.extend(plan.adds[..k].iter().map(|a| a.1));
            let e = solve(&with_masses(&mass, &pts));
            let err = trim_error(&e, beam, lo, hi);
            let edits = plan.fitted_edits.len() + k;
            let better = best.as_ref().is_none_or(|b| err < b.0 - 1e-9 || ((err - b.0).abs() <= 1e-9 && edits < b.1.fitted_edits.len() + b.2));
            if better { best = Some((err, plan.clone(), k, e)); }
            full = Some(e);
        }
        let Some(full) = full else { break };
        if best.as_ref().is_some_and(|b| b.0 <= 0.0) || !full.floats { break; }
        // Aim past what the real hull missed.
        gm_aim += want_gm - fin(full.gm_m);
        if let Some(k) = lt.k_trim { x_aim -= fin(full.trim_rad) / k; }
        if let Some(k) = lt.k_heel { z_aim -= fin(full.heel_rad) / k; }
    }
    let (err, plan, k, after_eq) = best?;
    if err > e0 * (1.0 - MIN_IMPROVEMENT) { return None; }
    let added_kg: f64 = plan.adds[..k].iter().map(|a| a.2).sum();
    let no_fitted = plan.fitted_edits.is_empty();
    let mut edits = plan.fitted_edits;
    edits.extend(plan.adds.into_iter().take(k).map(|a| a.0));
    let edit = several(edits)?;
    let text = if no_fitted {
        num(num(m("suggest.ballast_add"), added_kg / 1_000.0, Unit::Tonnes), after_eq.gm_m, Unit::Metres)
    } else {
        num(num(m("suggest.ballast"), (plan.fitted_aboard + added_kg) / 1_000.0, Unit::Tonnes), after_eq.gm_m, Unit::Metres)
    };
    suggestion(SuggestKind::BallastAndTrim, edit, text)
}

/// Every loaded ballast part emptied: the one edit that sheds weight without touching the design.
pub(crate) fn ballast_unload(bp: &Blueprint, cat: &Catalogue) -> Option<BlueprintEdit> {
    let edits: Vec<BlueprintEdit> = bp.parts.iter().enumerate().filter(|(_, p)| cat.part(&p.part).is_some_and(|d| matches!(d.function, PartFunction::Ballast { .. })) && p.load.is_none_or(|l| !l.is_finite() || l > 0.0)).map(|(i, _)| BlueprintEdit::SetLoad { slot: PartSlot(i as u16), load: 0.0 }).collect();
    several(edits)
}

fn ballast_max(d: &PartDef) -> f64 { match d.function { PartFunction::Ballast { max_kg } => pos(max_kg), _ => 0.0 } }

// ---------------------------------------------------------------------------------------------------------
// Armour layout.

/// Share of light-ship mass the armour suggestion may add.
const ARMOUR_BUDGET: f64 = 0.08;

fn armour_mass(cat: &Catalogue, material: Material, tier: ArmourTier) -> f64 {
    if tier == ArmourTier::None { return 0.0; }
    let row = cat.materials.iter().find(|m| m.material == material).or_else(|| cat.materials.iter().find(|m| m.material == Material::Steel));
    match row {
        Some(r) => pos(r.armour_mass_kg[tier as usize - 1]),
        // Steel plate over one 0.25 m^2 cell face at 7,850 kg/m^3.
        None => 0.25 * tier.thickness_mm() / 1_000.0 * 7_850.0,
    }
}

/// Outer structural cells of the belt and magazine crowns over the vital spaces.
pub(crate) fn armour_cells(bp: &Blueprint, cat: &Catalogue) -> Vec<CellPos> {
    let stored = CellGrid::from_rows(&bp.cells);
    let [sx, sy, sz] = [stored.size[0] as i32, stored.size[1] as i32, stored.size[2] as i32];
    let structural = |p: CellPos| { let c = stored.get(p); !c.is_empty() && !matches!(c.material, Material::PartBody | Material::Glass) };
    let mut xs = BTreeSet::new();
    let mut crowns = BTreeSet::new();
    for p in &bp.parts {
        let Some(def) = cat.part(&p.part) else { continue };
        let vital = matches!(def.function, PartFunction::Magazine { .. } | PartFunction::Engine(_) | PartFunction::Generator { .. } | PartFunction::Console);
        if !vital { continue; }
        let Some(cells) = part_cells(p, cat) else { continue };
        for c in &cells {
            for dx in -1..=1 { let x = c.x as i32 + dx; if (0..sx).contains(&x) { xs.insert(x); } }
            if matches!(def.function, PartFunction::Magazine { .. }) { crowns.insert((c.x as i32, c.z as i32, c.y as i32)); }
        }
    }
    let wl = bp.hull.design_waterline_y as i32;
    let mut out = BTreeSet::new();
    for &x in &xs {
        for y in (wl - 2).max(0)..=(wl + 4).min(sy - 1) {
            if let Some(z) = (0..sz).find(|&z| !stored.get(CellPos::new(x as i16, y as i16, z as i16)).is_empty()) {
                let p = CellPos::new(x as i16, y as i16, z as i16);
                if structural(p) { out.insert(p); }
            }
            if let Some(z) = (0..sz).rev().find(|&z| !stored.get(CellPos::new(x as i16, y as i16, z as i16)).is_empty()) {
                let p = CellPos::new(x as i16, y as i16, z as i16);
                if structural(p) { out.insert(p); }
            }
        }
    }
    for &(x, z, y0) in &crowns {
        if let Some(y) = (y0 + 1..sy).rev().find(|&y| !stored.get(CellPos::new(x as i16, y as i16, z as i16)).is_empty()) {
            let p = CellPos::new(x as i16, y as i16, z as i16);
            if structural(p) { out.insert(p); }
        }
    }
    out.into_iter().collect()
}

pub(crate) fn armour_layout(bp: &Blueprint, cat: &Catalogue) -> Option<Suggestion> {
    let cells = armour_cells(bp, cat);
    if cells.is_empty() { return None; }
    let stored = CellGrid::from_rows(&bp.cells);
    // The budget is a share of the light ship without the layout's own armour, so accepting a layout never
    // raises the budget for the next tier.
    let layout_now: f64 = cells.iter().map(|&c| { let cell = stored.get(c); armour_mass(cat, cell.material, cell.armour) }).sum();
    let base = (pos(content::stats::compute_stats(bp, cat).light_t) * 1_000.0 - layout_now).max(0.0);
    let budget = base * ARMOUR_BUDGET;
    for tier in [ArmourTier::Mm40, ArmourTier::Mm15, ArmourTier::Mm5] {
        let total: f64 = cells.iter().map(|&c| { let cell = stored.get(c); armour_mass(cat, cell.material, cell.armour.max(tier)) }).sum();
        if total > budget { continue; }
        let need: Vec<CellPos> = cells.iter().copied().filter(|&c| stored.get(c).armour < tier).collect();
        if need.is_empty() { return None; }
        let text = num(m("suggest.armour").arg(MsgArg::Int(need.len() as i64)), tier.thickness_mm(), Unit::Millimetres);
        return suggestion(SuggestKind::ArmourLayout, BlueprintEdit::PaintArmour { cells: need, tier: tier as u8 }, text);
    }
    None
}

// ---------------------------------------------------------------------------------------------------------
// Engine and generator sizing.

/// Generation margin over combat demand.
const GENERATION_MARGIN: f64 = 1.1;
/// Engine shaft power is matched when it lies in this share of what the propulsors absorb.
const SHAFT_MATCH: (f64, f64) = (0.8, 1.05);

fn generator_kw(d: &PartDef) -> f64 { match d.function { PartFunction::Generator { kw, .. } => pos(kw), _ => 0.0 } }

pub(crate) fn engine_and_generator(bp: &Blueprint, prefab: &VesselPrefab, cat: &Catalogue) -> Option<Suggestion> {
    let budget = calc::power_budget(prefab, cat, calc::cruise_fraction());
    let need = budget.combat_kw * GENERATION_MARGIN;
    let space = Space::new(bp, cat);
    let mut edits = Vec::new();
    let mut gen_after = budget.generation_kw;
    let mut gen_key = None;
    if budget.combat_kw > 0.0 && budget.generation_kw < need {
        let deficit = need - budget.generation_kw;
        let mut defs: Vec<&PartDef> = offered(cat).filter(|d| generator_kw(d) > 0.0).collect();
        defs.sort_by(|a, b| generator_kw(a).partial_cmp(&generator_kw(b)).unwrap_or(core::cmp::Ordering::Equal).then_with(|| a.id.cmp(&b.id)));
        let pick: Vec<&PartDef> = match defs.iter().position(|d| generator_kw(d) >= deficit) { Some(i) => defs[i..].to_vec(), None => defs.iter().rev().copied().collect() };
        // Next to an existing generator or engine, else low amidships.
        let near = bp.parts.iter().find(|p| cat.part(&p.part).is_some_and(|d| matches!(d.function, PartFunction::Generator { .. } | PartFunction::Engine(_)))).map(|p| p.pos).unwrap_or_else(|| space.low_centre(0.4));
        for d in pick {
            if let Some(at) = space.find_part(d, upright(), near, MAX_RADIUS) {
                edits.push(BlueprintEdit::AddPart(PartPlacement { part: d.id.clone(), pos: at, rot: upright(), kit: None, label: None, load: None }));
                gen_after += generator_kw(d);
                gen_key = Some("suggest.generator_add");
                break;
            }
        }
    } else if budget.generators.len() > 1 && budget.generation_kw > need * 1.5 {
        // A surplus generator: the smallest loose one whose removal still leaves the margin.
        let spare = prefab.parts.iter().filter_map(|p| {
            let d = calc::def_of(cat, p)?;
            let kw = generator_kw(d);
            let loose = bp.parts.get(p.slot.index()).is_some_and(|b| b.kit.is_none());
            (kw > 0.0 && loose && budget.generation_kw - kw >= need).then_some((kw, p.slot))
        }).min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal).then_with(|| a.1.cmp(&b.1)));
        if let Some((kw, slot)) = spare {
            edits.push(BlueprintEdit::RemovePart(slot));
            gen_after -= kw;
            gen_key = Some("suggest.generator_remove");
        }
    }
    // Engines against propulsor absorption.
    let absorb: f64 = prefab.parts.iter().filter(|p| p.works()).filter_map(|p| match calc::def_of(cat, p).map(|d| &d.function) { Some(PartFunction::Propulsor(pr)) => Some(pos(pr.max_shaft_kw)), _ => None }).sum();
    let engines: Vec<(PartSlot, EngineKind, f64)> = prefab.parts.iter().filter_map(|p| match calc::def_of(cat, p).map(|d| &d.function) { Some(PartFunction::Engine(e)) => Some((p.slot, e.kind, pos(e.shaft_kw))), _ => None }).collect();
    let mut engine_change: Option<(PartSlot, f64)> = None;
    if absorb > 0.0 && !engines.is_empty() && (budget.shaft_kw < absorb * SHAFT_MATCH.0 || budget.shaft_kw > absorb * SHAFT_MATCH.1) {
        let per = absorb / engines.len() as f64;
        // The engine furthest from its share.
        let (slot, kind, kw) = engines.iter().copied().max_by(|a, b| (a.2 - per).abs().partial_cmp(&(b.2 - per).abs()).unwrap_or(core::cmp::Ordering::Equal).then_with(|| b.0.cmp(&a.0)))?;
        let mut cands: Vec<&PartDef> = offered(cat).filter(|d| matches!(&d.function, PartFunction::Engine(e) if e.kind == kind && pos(e.shaft_kw) > 0.0)).collect();
        let shaft = |d: &PartDef| match &d.function { PartFunction::Engine(e) => pos(e.shaft_kw), _ => 0.0 };
        cands.sort_by(|a, b| (shaft(a) - per).abs().partial_cmp(&(shaft(b) - per).abs()).unwrap_or(core::cmp::Ordering::Equal).then_with(|| a.id.cmp(&b.id)));
        let old = bp.parts.get(slot.index())?.clone();
        for d in cands.into_iter().take(4) {
            let new_kw = shaft(d);
            if (new_kw - per).abs() >= (kw - per).abs() || d.id == old.part { continue; }
            let swap = BlueprintEdit::Several(vec![BlueprintEdit::RemovePart(slot), BlueprintEdit::AddPart(PartPlacement { part: d.id.clone(), pos: old.pos, rot: old.rot, kit: None, label: old.label.clone(), load: None })]);
            if crate::editor::notes::apply_edit(bp, &swap, cat).is_some() {
                // The generator edit goes first; a generator removal below the engine renumbers it.
                let after = match edits.first() { Some(BlueprintEdit::RemovePart(g)) if *g < slot => PartSlot(slot.0 - 1), _ => slot };
                let swap = BlueprintEdit::Several(vec![BlueprintEdit::RemovePart(after), BlueprintEdit::AddPart(PartPlacement { part: d.id.clone(), pos: old.pos, rot: old.rot, kit: None, label: old.label.clone(), load: None })]);
                edits.push(swap);
                engine_change = Some((slot, new_kw));
                break;
            }
        }
    }
    // Everything at once if it still applies together, else the most useful change alone.
    let mut edit = several(edits.clone())?;
    if crate::editor::notes::apply_edit(bp, &edit, cat).is_none() {
        // The changes collide (the new generator's spot under the new engine): offer the first alone.
        edit = edits.into_iter().next()?;
        if gen_key.is_some() { engine_change = None; } else { gen_key = None; }
    }
    let text = match (gen_key, engine_change) {
        (Some(_), Some((_, kw))) => num(num(num(m("suggest.power_and_engine"), gen_after, Unit::Kilowatts), budget.combat_kw, Unit::Kilowatts), kw, Unit::Kilowatts),
        (Some(k), None) => num(num(m(k), gen_after, Unit::Kilowatts), budget.combat_kw, Unit::Kilowatts),
        (None, Some((slot, kw))) => num(m("suggest.engine_swap").arg(MsgArg::Part(slot)), kw, Unit::Kilowatts),
        (None, None) => return None,
    };
    suggestion(SuggestKind::EngineAndGeneratorSizing, edit, text)
}

// ---------------------------------------------------------------------------------------------------------
// Fuel load.

/// Reserve buoyancy the fuel suggestion keeps, as a share of displacement.
pub(crate) const MIN_RESERVE: f64 = 0.2;

/// Fullest uniform tank level in 5% steps that keeps reserve buoyancy at `MIN_RESERVE`. Reserve buoyancy is
/// the enclosed volume beyond displacement: total = displacement x (1 + reserve), unchanged by loading.
pub(crate) fn fuel_level(displacement_t: f64, reserve: f64, fuel_l: f64, capacity_l: f64) -> f64 {
    if !(capacity_l > 0.0) || !(displacement_t > 0.0) || !reserve.is_finite() { return 0.0; }
    let total = displacement_t * (1.0 + reserve);
    // Largest added mass with (total - d') / d' >= r, d' = displacement + added.
    let max_added_t = total / (1.0 + MIN_RESERVE) - displacement_t;
    let max_fuel_l = pos(fuel_l) + max_added_t * 1_000.0 / sim_core::mass::FUEL_DENSITY_KG_PER_L;
    let f = (max_fuel_l / capacity_l).clamp(0.0, 1.0);
    libm::floor(f * 20.0 + 1e-9) / 20.0
}

/// The fuel proposal: the edit, the tank level it sets and the fuel account it started from.
pub(crate) fn fuel_plan(bp: &Blueprint, design: &ShipDesign, cat: &Catalogue) -> Option<(BlueprintEdit, f64, calc::FuelAccount)> {
    let prefab = &design.prefab;
    let fuel = calc::fuel_account(prefab, cat);
    if !(fuel.capacity_l > 0.0) { return None; }
    let mass = calc::loaded_mass(prefab, cat);
    let eq = sim_core::hydro::equilibrium(&prefab.blocks, &mass, &[], &prefab.compartments, &cat.tuning.buoyancy);
    if !eq.floats { return None; }
    let f = fuel_level(eq.displacement_t, eq.reserve, fuel.fuel_l, fuel.capacity_l);
    let edits: Vec<BlueprintEdit> = bp.parts.iter().enumerate().filter(|(_, p)| cat.part(&p.part).is_some_and(|d| matches!(d.function, PartFunction::FuelTank { .. }))).filter(|(_, p)| (p.load.filter(|v| v.is_finite()).unwrap_or(1.0) - f).abs() > 0.01).map(|(i, _)| BlueprintEdit::SetLoad { slot: PartSlot(i as u16), load: f }).collect();
    Some((several(edits)?, f, fuel))
}

pub(crate) fn fuel_load(bp: &Blueprint, design: &ShipDesign, cat: &Catalogue) -> Option<Suggestion> {
    let (edit, f, fuel) = fuel_plan(bp, design, cat)?;
    let text = num(num(m("suggest.fuel"), f * 100.0, Unit::Percent), f * fuel.capacity_l, Unit::Litres);
    suggestion(SuggestKind::FuelLoad, edit, text)
}

// ---------------------------------------------------------------------------------------------------------
// Sensor placement.

/// Score of a radar spot: horizon against a sea-skimmer weighted by mean sector coverage.
pub(crate) fn spot_score(height_m: f64, coverage: &[f32]) -> f64 {
    let mean = if coverage.is_empty() { 0.0 } else { coverage.iter().map(|c| *c as f64).sum::<f64>() / coverage.len() as f64 };
    crate::sensors::radar::horizon_m(height_m.max(0.0), calc::REF_HEIGHT_M[0]) * (0.5 + 0.5 * mean)
}

pub(crate) fn sensor_placement(bp: &Blueprint, prefab: &VesselPrefab, cat: &Catalogue) -> Option<Suggestion> {
    // The main search radar: longest reference range, then lowest slot.
    let site = calc::sensor_sites(prefab, cat).into_iter().filter(|s| matches!(s.kind, SensorKind::SearchRadar | SensorKind::NavRadar)).max_by(|a, b| a.r_ref_m.partial_cmp(&b.r_ref_m).unwrap_or(core::cmp::Ordering::Equal).then_with(|| b.slot.cmp(&a.slot)))?;
    let placed = bp.parts.get(site.slot.index())?.clone();
    let def = cat.part(&placed.part)?;
    // The design without the radar (or its whole mast kit), which moves as one.
    let kit = placed.kit.and_then(|k| bp.kits.get(k.index()).map(|kp| (k, kp.clone())));
    let lift = match &kit { Some((k, _)) => BlueprintEdit::RemoveKit(*k), None => BlueprintEdit::RemovePart(site.slot) };
    let without = crate::editor::notes::apply_edit(bp, &lift, cat)?;
    let space = Space::new(&without, cat);
    let vgrid = sim_core::grid::VesselGrid::build(Arc::new(space.grid.clone()), &[], &|_| (false, content::part::Flammable::No));
    let cells = part_cells(&placed, cat)?;
    let (min, max) = cells.iter().fold(([i16::MAX; 3], [i16::MIN; 3]), |(lo, hi), c| ([lo[0].min(c.x), lo[1].min(c.y), lo[2].min(c.z)], [hi[0].max(c.x), hi[1].max(c.y), hi[2].max(c.z)]));
    let ext = [max[0] - min[0] + 1, max[1] - min[1] + 1, max[2] - min[2] + 1];
    let off = [placed.pos.x - min[0], placed.pos.y - min[1], placed.pos.z - min[2]];
    let wl_m = bp.hull.design_waterline_y as f64 * CELL_M;
    // The current spot is scored exactly as the candidates are (same grid without the radar or its kit, same
    // waterline), so a radar already in the best spot is never offered a move to an equal one.
    let now_score = spot_score(site.centre.y - wl_m, &calc::sector_coverage(&vgrid, &calc::SensorSite { slot: PartSlot(u16::MAX), ..site }));
    let sx = space.grid.size[0] as i32;
    let mut best: Option<(f64, CellPos, f64, f64)> = None; // (score, new pos, height, coverage)
    let mut kit_defs: Vec<(&PartDef, CellPos, content::kit::PartRot)> = Vec::new();
    if let Some((_, kp)) = &kit {
        for &s in &kp.parts { let p = bp.parts.get(s.index())?; kit_defs.push((cat.part(&p.part)?, p.pos, p.rot)); }
    }
    for x in 0..(sx - ext[0] as i32 + 1).max(0) {
        let frac = if sx > 1 { (x as f64 + ext[0] as f64 * 0.5) / sx as f64 } else { 0.5 };
        let deck = space.deck_top(frac);
        // The radar's lowest cell at the deck top, or the kit's lowest part there.
        let lowest = CellPos::new(x as i16, deck.y, deck.z - ext[2] / 2);
        let delta = match &kit {
            Some(_) => {
                let kit_min_y = kit_defs.iter().filter_map(|(d, p, r)| footprint(d, *p, *r)).flatten().map(|c| c.y).min()?;
                CellPos::new(lowest.x - min[0], deck.y - kit_min_y, lowest.z - min[2])
            }
            None => CellPos::new(lowest.x - min[0], lowest.y - min[1], lowest.z - min[2]),
        };
        if delta == CellPos::new(0, 0, 0) { continue; }
        let fits = match &kit {
            Some(_) => {
                let group: Option<Vec<(&PartDef, Vec<CellPos>)>> = kit_defs.iter().map(|(d, p, r)| footprint(d, CellPos::new(p.x + delta.x, p.y + delta.y, p.z + delta.z), *r).map(|c| (*d, c))).collect();
                group.is_some_and(|g| space.fits_group(&g))
            }
            None => footprint(def, CellPos::new(lowest.x + off[0], lowest.y + off[1], lowest.z + off[2]), placed.rot).is_some_and(|c| space.fits(def, &c)),
        };
        if !fits { continue; }
        let centre = site.centre + DVec3::new(delta.x as f64, delta.y as f64, delta.z as f64) * CELL_M;
        let moved = calc::SensorSite { centre, height_m: centre.y - wl_m, ..site };
        let cov = calc::sector_coverage(&vgrid, &calc::SensorSite { slot: PartSlot(u16::MAX), ..moved });
        let score = spot_score(moved.height_m, &cov);
        let mean = cov.iter().map(|c| *c as f64).sum::<f64>() / cov.len().max(1) as f64;
        if best.is_none_or(|b| score > b.0) { best = Some((score, delta, moved.height_m, mean)); }
    }
    let (score, delta, height, mean) = best?;
    if !(score > now_score * 1.05) { return None; }
    let edit = match &kit {
        Some((k, kp)) => BlueprintEdit::Several(vec![BlueprintEdit::RemoveKit(*k), BlueprintEdit::AddKit { kit: kp.kit.clone(), pos: CellPos::new(kp.pos.x + delta.x, kp.pos.y + delta.y, kp.pos.z + delta.z), rot: kp.rot, mirrored: kp.mirrored }]),
        None => BlueprintEdit::MovePart { slot: site.slot, to: CellPos::new(placed.pos.x + delta.x, placed.pos.y + delta.y, placed.pos.z + delta.z), rot: placed.rot },
    };
    let text = num(num(m("suggest.sensor").arg(MsgArg::Part(site.slot)), height, Unit::Metres), mean * 100.0, Unit::Percent);
    suggestion(SuggestKind::SensorPlacement, edit, text)
}

// ---------------------------------------------------------------------------------------------------------
// Bulkhead placement.

/// A compartment is split when longer than this share of the hull and this many metres.
const SPLIT_SHARE: f64 = 0.2;
const SPLIT_MIN_M: f64 = 3.0;

pub(crate) fn bulkhead_placement(bp: &Blueprint, prefab: &VesselPrefab, cat: &Catalogue) -> Option<Suggestion> {
    let length = pos(prefab.dims[0]);
    // Below the waterline: the compartment's lowest cell row starts under the design waterline.
    let longest = prefab.compartments.compartments.iter().filter(|c| !c.is_absorbed() && !c.cells.is_empty() && (c.min[1] as f64) * CELL_M < prefab.waterline_y).max_by(|a, b| (a.max[0] - a.min[0]).cmp(&(b.max[0] - b.min[0])).then_with(|| b.id.cmp(&a.id)))?;
    let cells_long = (longest.max[0] - longest.min[0] + 1) as f64;
    let len_m = cells_long * CELL_M;
    if len_m <= SPLIT_MIN_M || len_m <= SPLIT_SHARE * length { return None; }
    let grid = content::blueprint::io::build_grid(bp, cat);
    let cuts_part = |x: i16| -> bool {
        (longest.min[1]..=longest.max[1]).any(|y| (longest.min[2]..=longest.max[2]).any(|z| grid.get(CellPos::new(x, y, z)).material == Material::PartBody))
    };
    let taken: BTreeSet<i16> = bp.hull.planes.iter().filter(|p| p.axis == content::enums::Axis::X).map(|p| p.offset).collect();
    let mid = (longest.min[0] as i32 + longest.max[0] as i32) / 2;
    let quarter = ((longest.max[0] - longest.min[0]) / 4).max(1) as i32;
    let x = (0..=quarter).flat_map(|d| [mid - d, mid + d]).map(|x| x as i16).find(|&x| x > longest.min[0] && x < longest.max[0] && !taken.contains(&x) && !cuts_part(x))?;
    let after = ((x - longest.min[0]).max(longest.max[0] - x)) as f64 * CELL_M;
    let text = num(num(m("suggest.bulkhead"), x as f64 * CELL_M, Unit::Metres), after, Unit::Metres);
    suggestion(SuggestKind::BulkheadPlacement, BlueprintEdit::AddBulkhead { x_cell: x }, text)
}

// ---------------------------------------------------------------------------------------------------------
// Aggression and the EMCON trigger.

pub(crate) fn aggression(bp: &Blueprint, design: &ShipDesign) -> Option<Suggestion> {
    let want = crate::officers::tuning::suggest_aggression(design);
    let now = crate::officers::tuning::slider_value(&bp.officers, "gunnery.aggression");
    if !want.is_finite() || (want - now).abs() < 0.025 { return None; }
    let edit = BlueprintEdit::SetSlider { key: "gunnery.aggression".into(), value: want };
    suggestion(SuggestKind::Aggression, edit, num(m("suggest.aggression"), want * 100.0, Unit::Percent))
}

pub(crate) fn emcon_trigger(bp: &Blueprint, design: &ShipDesign, cat: &Catalogue) -> Option<Suggestion> {
    let want = crate::emcon::suggest_trigger_range_m(design, cat);
    if !(want.is_finite() && want > 0.0) { return None; }
    let want = want.clamp(2_000.0, 60_000.0);
    let now = bp.officers.burst_trigger_range_m.filter(|r| r.is_finite() && *r > 0.0).unwrap_or(cat.tuning.doctrine.burst_trigger_m);
    if (want - now).abs() < 500.0 { return None; }
    let want = libm::round(want / 100.0) * 100.0;
    suggestion(SuggestKind::EmconTriggerRange, BlueprintEdit::SetBurstTrigger { range_m: want }, num(m("suggest.emcon_trigger"), want / 1_000.0, Unit::Kilometres))
}
