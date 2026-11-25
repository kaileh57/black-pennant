//! Gathering Ship's Notes from their sources, with text and Fix for each.
use super::{NoteKind, NoteTier, ShipNote};
use crate::doctrine::MountInfo;
use crate::editor::fitcheck::FitResult;
use crate::editor::gauges::{calc, GaugeStrip};
use crate::editor::suggest::compute as sc;
use crate::editor::suggest::place::{upright, Space, MAX_RADIUS};
use crate::editor::suggest::{SuggestKind, AutoSetValue};
use crate::editor::{BlueprintEdit, Check};
use crate::graph::GraphIssue;
use crate::prelude::*;
use crate::ship::ShipDesign;
use content::blueprint::{Blueprint, PartPlacement};
use content::campaign::ConstraintTag;
use content::cell::{CellPos, CELL_M};
use content::graph::WeaponClass;
use content::kit::KitDef;
use content::part::{PartDef, PartFunction, PartTag};
use sim_core::vessel::VesselPrefab;

pub(crate) const KEYS: &[&str] = &[
    "notes.arc_blocked",
    "notes.autoset.conduit_routes",
    "notes.autoset.kit_wiring",
    "notes.autoset.mount_limits",
    "notes.autoset.power_priorities",
    "notes.autoset.solver_tables",
    "notes.autoset_dropped",
    "notes.float_fail.gm",
    "notes.float_fail.sinks",
    "notes.gauge.complexity",
    "notes.gauge.endurance",
    "notes.gauge.flood_one",
    "notes.gauge.flood_two",
    "notes.gauge.gm_high",
    "notes.gauge.gm_low",
    "notes.gauge.over_tonnage",
    "notes.gauge.power",
    "notes.gauge.punch",
    "notes.gauge.sight_blind",
    "notes.gauge.sight_none",
    "notes.gauge.turning",
    "notes.graph.cycle",
    "notes.graph.layer",
    "notes.graph.rule_sink",
    "notes.graph.script_disabled",
    "notes.graph.too_many_rules",
    "notes.graph.type_mismatch",
    "notes.no_director",
    "notes.no_generation",
    "notes.no_magazine",
    "notes.no_propulsion",
    "notes.no_steering",
    "notes.part_removed",
    "notes.pinned_node",
    "notes.pinned_route",
    "notes.sea.rolls_over",
    "notes.tag.draft",
    "notes.tag.passengers",
    "notes.tag.range",
    "notes.tag.rcs",
    "notes.tag.requires",
    "notes.tag.speed",
    "notes.tag_word.boarding_kit",
    "notes.tag_word.camera_mast",
    "notes.tag_word.crane",
    "notes.tag_word.fuel_store",
    "notes.tag_word.generation",
    "notes.tag_word.low_signature",
    "notes.tag_word.magazine",
    "notes.tag_word.mine_rail",
    "notes.tag_word.passenger_berths",
    "notes.tag_word.point_defence",
    "notes.tag_word.propulsion",
    "notes.tag_word.recovery_net",
    "notes.tag_word.rhib_davit",
    "notes.tag_word.steering",
    "notes.unconnected",
    "notes.no_console",
    "notes.unpowered",
    "notes.word.structure",
];

/// Arc blocked over more than this share of the sweep is a tier 3 note (L194).
const ARC_BLOCKED_NOTE: f64 = 0.5;
/// Highest raise in cells a masked-gun Fix tries.
const MAX_RAISE: i16 = 6;

fn m(key: &str) -> Msg { Msg::new(key) }
fn fin(x: f64) -> f64 { if x.is_finite() { x } else { 0.0 } }
fn pos(x: f64) -> f64 { if x.is_finite() && x > 0.0 { x } else { 0.0 } }
fn num(msg: Msg, v: f64, u: Unit) -> Msg { msg.num(fin(v), u) }
fn count(n: usize) -> u16 { n.min(u16::MAX as usize) as u16 }

fn note(tier: NoteTier, kind: NoteKind, part: Option<PartSlot>, affected: usize, text: Msg, fix: Option<BlueprintEdit>) -> ShipNote {
    ShipNote { tier, kind, part, affected_parts: count(affected), text, fix, flips_tag: false, score_effect: None }
}

pub(crate) fn tag_word(t: PartTag) -> &'static str {
    match t {
        PartTag::Crane => "notes.tag_word.crane",
        PartTag::MineRail => "notes.tag_word.mine_rail",
        PartTag::RhibDavit => "notes.tag_word.rhib_davit",
        PartTag::BoardingKit => "notes.tag_word.boarding_kit",
        PartTag::RecoveryNet => "notes.tag_word.recovery_net",
        PartTag::CameraMast => "notes.tag_word.camera_mast",
        PartTag::PassengerBerths => "notes.tag_word.passenger_berths",
        PartTag::PointDefence => "notes.tag_word.point_defence",
        PartTag::Magazine => "notes.tag_word.magazine",
        PartTag::FuelStore => "notes.tag_word.fuel_store",
        PartTag::Propulsion => "notes.tag_word.propulsion",
        PartTag::Steering => "notes.tag_word.steering",
        PartTag::Generation => "notes.tag_word.generation",
        PartTag::LowSignature => "notes.tag_word.low_signature",
    }
}

fn autoset_key(v: AutoSetValue) -> &'static str {
    match v {
        AutoSetValue::ConduitRoutes => "notes.autoset.conduit_routes",
        AutoSetValue::PowerPriorities => "notes.autoset.power_priorities",
        AutoSetValue::MountLimits => "notes.autoset.mount_limits",
        AutoSetValue::KitWiring => "notes.autoset.kit_wiring",
        AutoSetValue::SolverTables => "notes.autoset.solver_tables",
    }
}

/// The edit of one Suggest, as a Fix.
fn suggest_fix(kind: SuggestKind, bp: &Blueprint, design: &ShipDesign, cat: &Catalogue) -> Option<BlueprintEdit> { crate::editor::suggest::suggest(kind, bp, design, cat).map(|s| s.edit) }

/// Add the first curated kit for a tag at the nearest place all its parts fit, searching from `target`.
pub(crate) fn add_kit_for_tag(bp: &Blueprint, tag: PartTag, target_frac: (f64, bool), cat: &Catalogue) -> Option<(BlueprintEdit, KitDef)> {
    add_kit_for_tag_where(bp, tag, target_frac, cat, &|_| true)
}

/// [`add_kit_for_tag`] among the kits that `keep` accepts, so a Fix adds the part a note says is missing.
fn add_kit_for_tag_where(bp: &Blueprint, tag: PartTag, target_frac: (f64, bool), cat: &Catalogue, keep: &dyn Fn(&KitDef) -> bool) -> Option<(BlueprintEdit, KitDef)> {
    let space = Space::new(bp, cat);
    let target = if target_frac.1 { space.deck_top(target_frac.0) } else { space.low_centre(target_frac.0) };
    for kit in cat.kits_for_tag(tag).into_iter().filter(|k| keep(k)) {
        if let Some(pos) = space.find_kit(kit, cat, upright(), false, target, MAX_RADIUS) {
            return Some((BlueprintEdit::AddKit { kit: kit.id.clone(), pos, rot: upright(), mirrored: false }, kit.clone()));
        }
    }
    None
}

/// Add one part of the catalogue chosen by `want` near `target`, in kit `kit`.
fn add_part_near(bp: &Blueprint, cat: &Catalogue, want: &dyn Fn(&PartDef) -> bool, target: CellPos, kit: Option<KitSlot>) -> Option<BlueprintEdit> {
    let space = Space::new(bp, cat);
    let mut defs: Vec<&PartDef> = cat.parts.values().filter(|d| d.replaced_by.is_none() && d.phase <= content::catalogue::BUILD_PHASE && want(d)).collect();
    defs.sort_by(|a, b| (a.mass_kg, &a.id).partial_cmp(&(b.mass_kg, &b.id)).unwrap_or(core::cmp::Ordering::Equal));
    for d in defs {
        if let Some(pos) = space.find_part(d, upright(), target, MAX_RADIUS) {
            return Some(BlueprintEdit::AddPart(PartPlacement { part: d.id.clone(), pos, rot: upright(), kit, label: None, load: None }));
        }
    }
    None
}

/// Ballast aboard, kilograms; an unset load is full, as the stats unit reads it.
fn ballast_kg(p: &PartPlacement, cat: &Catalogue) -> f64 {
    match cat.part(&p.part).map(|d| &d.function) {
        Some(PartFunction::Ballast { max_kg }) => { let max = if max_kg.is_finite() { max_kg.max(0.0) } else { 0.0 }; p.load.filter(|v| v.is_finite()).map_or(max, |v| v.clamp(0.0, max)) }
        _ => 0.0,
    }
}

fn cell_of(local_m: DVec3) -> CellPos {
    let c = |v: f64| (libm::floor(fin(v) / CELL_M) as i64).clamp(0, i16::MAX as i64) as i16;
    CellPos::new(c(local_m.x), c(local_m.y), c(local_m.z))
}

struct Facts<'a> {
    bp: &'a Blueprint,
    design: &'a ShipDesign,
    prefab: &'a VesselPrefab,
    cat: &'a Catalogue,
    g: &'a GaugeStrip,
}

impl<'a> Facts<'a> {
    fn defs(&self) -> impl Iterator<Item = (&'a sim_core::parts::PartRuntime, &'a PartDef)> + '_ {
        let cat = self.cat;
        self.prefab.parts.iter().filter_map(move |p| calc::def_of(cat, p).map(|d| (p, d)))
    }
    fn has(&self, f: &dyn Fn(&PartDef) -> bool) -> bool { self.defs().any(|(p, d)| p.works() && f(d)) }
}

pub(crate) fn collect(bp: &Blueprint, design: &ShipDesign, g: &GaugeStrip, fit: Option<&FitResult>, removed: &[content::migrate::RemovedPart], cat: &Catalogue) -> Vec<ShipNote> {
    let f = Facts { bp, design, prefab: &design.prefab, cat, g };
    let mut out = Vec::new();
    tier_one(&f, &mut out);
    if let Some(fit) = fit { tier_two(&f, fit, &mut out); }
    tier_three(&f, &mut out);
    tier_four(&f, &mut out);
    tier_five(&f, removed, &mut out);
    out
}

// ---------------------------------------------------------------------------------------------------------
// Tier 1: cannot float, move or make power.

fn tier_one(f: &Facts<'_>, out: &mut Vec<ShipNote>) {
    let all = f.prefab.parts.len();
    let fl = &f.g.float;
    if !fl.floats && fl.reserve <= 0.0 {
        // Heavier than all the water it can displace. Shedding loaded ballast is the one mechanical answer.
        let fix = sc::ballast_unload(f.bp, f.cat);
        out.push(note(NoteTier::CannotFloatMoveOrPower, NoteKind::FloatFail, None, all, num(m("notes.float_fail.sinks"), fl.displacement_t, Unit::Tonnes), fix));
    } else if !fl.floats || fl.check == Check::Fail {
        // Buoyant but it will not stand upright (it lolls past a right angle, or GM is zero or less): weight
        // low is the answer, which is the ballast and trim Suggest.
        out.push(note(NoteTier::CannotFloatMoveOrPower, NoteKind::FloatFail, None, all, num(m("notes.float_fail.gm"), fl.gm_m, Unit::Metres), suggest_fix(SuggestKind::BallastAndTrim, f.bp, f.design, f.cat)));
    }
    let propulsor = f.has(&|d| matches!(d.function, PartFunction::Propulsor(_)));
    let engine = f.has(&|d| matches!(d.function, PartFunction::Engine(_)));
    if !propulsor || !engine || f.g.speed.check == Check::Fail {
        // The Fix adds what is missing: a kit with a propeller or waterjet when there is none (an engine kit
        // alone leaves the ship as it was), else one with an engine.
        let supplies = |k: &KitDef, want: &dyn Fn(&PartFunction) -> bool| k.parts.iter().any(|p| f.cat.part(&p.part).is_some_and(|d| want(&d.function)));
        let keep = |k: &KitDef| if !propulsor { supplies(k, &|x| matches!(x, PartFunction::Propulsor(_))) } else if !engine { supplies(k, &|x| matches!(x, PartFunction::Engine(_))) } else { true };
        // A propulsion kit sits partly in the plating, where the search does not place; the propeller alone
        // hangs under the stern, as the rudder does for steering.
        let fix = add_kit_for_tag_where(f.bp, PartTag::Propulsion, (0.15, false), f.cat, &keep).map(|k| k.0).or_else(|| {
            if propulsor { return None; }
            let space = Space::new(f.bp, f.cat);
            add_part_near(f.bp, f.cat, &|d| matches!(d.function, PartFunction::Propulsor(_)), space.low_centre(0.02), None)
        });
        out.push(note(NoteTier::CannotFloatMoveOrPower, NoteKind::NoPropulsion, None, all, m("notes.no_propulsion"), fix));
    }
    if f.g.power.check == Check::Fail {
        let consumers = calc::power_budget(f.prefab, f.cat, calc::cruise_fraction()).consumers.len();
        let fix = add_kit_for_tag(f.bp, PartTag::Generation, (0.4, false), f.cat).map(|k| k.0).or_else(|| suggest_fix(SuggestKind::EngineAndGeneratorSizing, f.bp, f.design, f.cat));
        out.push(note(NoteTier::CannotFloatMoveOrPower, NoteKind::NoGeneration, None, consumers, num(m("notes.no_generation"), f.g.power.combat_demand_kw, Unit::Kilowatts), fix));
    }
    let steering = f.has(&|d| matches!(&d.function, PartFunction::Rudder { .. }) || matches!(&d.function, PartFunction::Propulsor(p) if p.steer_max_deg.is_finite() && p.steer_max_deg > 0.0));
    if !steering {
        let fix = add_kit_for_tag(f.bp, PartTag::Steering, (0.05, false), f.cat).map(|k| k.0)
            .or_else(|| { let space = Space::new(f.bp, f.cat); add_part_near(f.bp, f.cat, &|d| matches!(d.function, PartFunction::Rudder { .. }), space.low_centre(0.02), None) });
        out.push(note(NoteTier::CannotFloatMoveOrPower, NoteKind::NoSteering, None, all, m("notes.no_steering"), fix));
    }
}

// ---------------------------------------------------------------------------------------------------------
// Tier 2: briefing tags.

fn tier_two(f: &Facts<'_>, fit: &FitResult, out: &mut Vec<ShipNote>) {
    for t in fit.tags.iter().filter(|t| !t.pass) {
        let (value, limit) = (fin(t.value), fin(t.limit));
        let (text, fix, flips) = match &t.tag {
            ConstraintTag::DraftLimit { .. } => {
                let fix = sc::ballast_unload(f.bp, f.cat);
                // Draft falls by the unloaded mass over the waterplane (about 0.7 L B for these hulls).
                let shed_t: f64 = f.bp.parts.iter().filter(|p| f.cat.part(&p.part).is_some_and(|d| matches!(d.function, PartFunction::Ballast { .. }))).map(|p| ballast_kg(p, f.cat) / 1_000.0).sum();
                let wp = 0.7 * fin(f.prefab.dims[0]) * fin(f.prefab.dims[2]);
                let rho = f.cat.tuning.buoyancy.water_density / 1_000.0;
                let flips = fix.is_some() && wp > 0.0 && rho > 0.0 && value - shed_t / (rho * wp) <= limit;
                (num(num(m("notes.tag.draft"), value, Unit::Metres), limit, Unit::Metres), fix, flips)
            }
            ConstraintTag::RcsLimit { .. } => (num(num(m("notes.tag.rcs"), value, Unit::None), limit, Unit::None), None, false),
            ConstraintTag::Range { .. } => {
                match sc::fuel_plan(f.bp, f.design, f.cat) {
                    Some((edit, level, fuel)) => {
                        let after = if fuel.fuel_l > 0.0 { value * level * fuel.capacity_l / fuel.fuel_l } else { 0.0 };
                        (num(num(m("notes.tag.range"), value, Unit::Kilometres), limit, Unit::Kilometres), Some(edit), after >= limit)
                    }
                    None => (num(num(m("notes.tag.range"), value, Unit::Kilometres), limit, Unit::Kilometres), None, false),
                }
            }
            ConstraintTag::Passengers { count: need, .. } => {
                let kit = add_kit_for_tag(f.bp, PartTag::PassengerBerths, (0.5, false), f.cat);
                let added: f64 = kit.as_ref().map(|(_, k)| k.parts.iter().filter_map(|p| f.cat.part(&p.part)).map(|d| match d.function { PartFunction::PassengerBerths { count } => count as f64, _ => 0.0 }).sum()).unwrap_or(0.0);
                let flips = kit.is_some() && value + added >= *need as f64;
                (m("notes.tag.passengers").arg(MsgArg::Int(value.max(0.0) as i64)).arg(MsgArg::Int(*need as i64)), kit.map(|k| k.0), flips)
            }
            ConstraintTag::Requires(tag) => {
                let deck = !matches!(tag, PartTag::Magazine | PartTag::FuelStore | PartTag::Generation | PartTag::Propulsion | PartTag::PassengerBerths);
                let kit = add_kit_for_tag(f.bp, *tag, (0.5, deck), f.cat);
                let flips = kit.is_some();
                (m("notes.tag.requires").arg(MsgArg::Key(TextKey::new(tag_word(*tag)))), kit.map(|k| k.0), flips)
            }
            ConstraintTag::MinSpeed { .. } => (num(num(m("notes.tag.speed"), value, Unit::Knots), limit, Unit::Knots), suggest_fix(SuggestKind::EngineAndGeneratorSizing, f.bp, f.design, f.cat), false),
        };
        let mut n = note(NoteTier::FailsBriefingTag, NoteKind::TagFails(t.tag.clone()), None, 0, text, fix);
        n.flips_tag = flips && n.fix.is_some();
        out.push(n);
    }
    // DECISION: a hull that rolls over in the briefing's own sea fails the briefing as surely as a tag does, so
    // the seakeeping verdict is a tier 2 note, after the tags. It names the worst direction and has no Fix.
    if let Some(run) = fit.seakeeping.as_ref().filter(|s| s.rolls_over()).and_then(|s| s.worst().map(|r| (s.sea_state, *r))) {
        let (sea, r) = run;
        let text = m("notes.sea.rolls_over").arg(MsgArg::Int(sea.0 as i64)).arg(MsgArg::Num { value: fin(r.from_deg), unit: Unit::Degrees });
        out.push(note(NoteTier::FailsBriefingTag, NoteKind::RollsOver { sea_state: sea.0, from_deg: fin(r.from_deg) }, None, 0, text, None));
    }
}

// ---------------------------------------------------------------------------------------------------------
// Tier 3: a weapon, sensor or consumer cannot work.

/// Vessel-local direction of flat fire at a training angle (training positive to port, as the gunnery unit).
fn mount_dir(zero_rad: f64, train_rad: f64) -> DVec3 {
    let (st, ct) = (libm::sin(train_rad), libm::cos(train_rad));
    let (s, c) = (libm::sin(fin(zero_rad)), libm::cos(fin(zero_rad)));
    let (mx, mz) = (ct, -st);
    DVec3::new(mx * c + mz * s, 0.0, -mx * s + mz * c)
}

/// Raise a masked gun until its flat-fire arc clears to at most half blocked, tested by rays over the
/// design's cells from the raised pivot; the first height that works is the Fix.
fn raise_fix(f: &Facts<'_>, mount: &MountInfo) -> Option<BlueprintEdit> {
    let placed = f.bp.parts.get(mount.slot.index())?;
    let def = f.cat.part(&placed.part)?;
    let space = Space::new(f.bp, f.cat);
    let own: BTreeSet<CellPos> = crate::editor::notes::edit::part_cells(placed, f.cat)?.into_iter().collect();
    for dy in 1..=MAX_RAISE {
        let to = CellPos::new(placed.pos.x, placed.pos.y + dy, placed.pos.z);
        let cells = crate::editor::notes::edit::footprint(def, to, placed.rot)?;
        if !cells.iter().all(|c| own.contains(c) || space.empty(*c)) { break; }
        let pivot = mount.pos_local + DVec3::new(0.0, dy as f64 * CELL_M, 0.0);
        let mut clear = |t: f64, _e: f64| calc::ray_clear(&f.prefab.grid, Some(mount.slot), pivot, mount_dir(mount.zero_rad, t));
        let sweep = calc::sweep_arcs(&mount.limits, calc::trainable(mount, f.cat), mount.zero_rad, mount.effective_range_m, &mut clear);
        if sweep.blocked_fraction <= ARC_BLOCKED_NOTE { return Some(BlueprintEdit::MovePart { slot: mount.slot, to, rot: placed.rot }); }
    }
    None
}

/// The first solid cell met along a ray: its owning part, or own structure.
fn blocker(f: &Facts<'_>, mount: &MountInfo, bearing_deg: f64) -> MsgArg {
    let grid = &f.prefab.grid;
    let dir = calc::bearing_dir(bearing_deg);
    let s = grid.size();
    let len = DVec3::new(s[0] as f64, s[1] as f64, s[2] as f64).length() + 2.0;
    let a = mount.pos_local / CELL_M;
    let b = a + dir * len;
    let mut hit: Option<Option<PartSlot>> = None;
    grid.traverse([a.x, a.y, a.z], [b.x, b.y, b.z], &mut |p, _| {
        if grid.is_solid(p) {
            let owner = grid.cell_type(p).part;
            if owner != Some(mount.slot) { hit = Some(owner); return false; }
        }
        true
    });
    match hit { Some(Some(slot)) => MsgArg::Part(slot), _ => MsgArg::Key(TextKey::new("notes.word.structure")) }
}

fn tier_three(f: &Facts<'_>, out: &mut Vec<ShipNote>) {
    // Masked arcs: the blocked share of the auto-set sweep decides (L194); the sector named comes from an
    // all-round sweep of the same shell paths.
    for mount in f.design.mounts.iter().filter(|m| m.weapon != WeaponClass::Decoy) {
        // The design keeps the sweep of every auto-limited mount; a hand-limited mount is swept here.
        let blocked_fraction = match f.design.auto_limits.get(&mount.slot) { Some(&(_, b)) => b, None => crate::ship::design::auto_mount_limits(f.prefab, mount.slot, f.cat).1 };
        if !(blocked_fraction.is_finite() && blocked_fraction > ARC_BLOCKED_NOTE) { continue; }
        let prefab = f.prefab;
        let cat = f.cat;
        let mut clear = |t: f64, e: f64| sim_core::World::mount_path_clear(prefab, mount.slot, t, e, cat);
        let sweep = calc::sweep_arcs(&mount.limits, calc::trainable(mount, cat), mount.zero_rad, mount.effective_range_m, &mut clear);
        let (from, to) = sweep.widest_blocked.unwrap_or((0.0, 360.0));
        let by = blocker(f, mount, sweep.widest_mid_deg.unwrap_or(0.0));
        let text = num(num(num(m("notes.arc_blocked").arg(MsgArg::Part(mount.slot)), blocked_fraction * 100.0, Unit::Percent).arg(by), from, Unit::Degrees), to, Unit::Degrees);
        out.push(note(NoteTier::CannotWork, NoteKind::ArcBlocked { from_deg: from, to_deg: to, blocked_fraction }, Some(mount.slot), 1, text, raise_fix(f, mount)));
    }
    // Ammunition and fire control.
    for mount in &f.design.mounts {
        let Some((_, def)) = f.cat.parts.get_index(mount.part.index()) else { continue };
        if let PartFunction::Gun(gun) = &def.function {
            let cal = gun.calibre_mm;
            let is_mag = move |d: &PartDef| matches!(d.function, PartFunction::Magazine { calibre_mm, .. } if calibre_mm == cal);
            // The simulation's own feed wiring decides (kit magazines first, else the nearest loose ones).
            let fed = f.prefab.parts.iter().find(|p| p.slot == mount.slot).is_some_and(|p| p.feed().magazines().next().is_some());
            // Calibres with no magazine in the catalogue fire from their ready rounds by design.
            let magazine_exists = f.cat.parts.values().any(|d| d.replaced_by.is_none() && d.phase <= content::catalogue::BUILD_PHASE && is_mag(d));
            let ready = gun.ready_rounds as f64;
            if !fed && (magazine_exists || ready <= 0.0) {
                let below = cell_of(mount.pos_local - DVec3::new(0.0, 2.0, 0.0));
                let fix = add_part_near(f.bp, f.cat, &is_mag, below, mount.kit);
                // Ready rounds for under a minute at full rate: the gun is out of the fight in its first minute.
                let minute = pos(gun.rof_rpm) * gun.barrels.max(1) as f64;
                let tier = if ready < minute { NoteTier::CannotWork } else { NoteTier::Advisory };
                let text = m("notes.no_magazine").arg(MsgArg::Part(mount.slot)).num(ready, Unit::Rounds);
                out.push(note(tier, NoteKind::NoMagazine, Some(mount.slot), 1, text, fix));
            }
        }
        // Only weapons that need a lock of their own. A surface gun aims at the side's picture, Lock grade
        // included, whether or not its kit carries a director; its director matters only under local control
        // after a cut Data link (L394), a damage state. Noting it put an untrue Advisory on every stock gun.
        let needs_lock = matches!(mount.weapon, WeaponClass::PointDefenceGun | WeaponClass::PointDefenceMissile | WeaponClass::Sam);
        if needs_lock && mount.director.is_none() {
            let fix = add_part_near(f.bp, f.cat, &|d| matches!(d.function, PartFunction::Director), cell_of(mount.pos_local), mount.kit);
            out.push(note(NoteTier::CannotWork, NoteKind::NoDirector, Some(mount.slot), 1, m("notes.no_director").arg(MsgArg::Part(mount.slot)), fix));
        }
    }
    // Consumers without a conduit to a source. With no generation at all the tier 1 note already covers every
    // power-only consumer.
    let budget = calc::power_budget(f.prefab, f.cat, calc::cruise_fraction());
    let no_power = budget.generation_kw <= 0.0;
    // With no console aboard the Data network has no source at all: one note with a Fix that adds the
    // bridge says so, in place of a note per weapon and sensor.
    let no_console = !f.has(&|d| matches!(d.function, PartFunction::Console));
    let mut data_cut = 0usize;
    let cut_on = |k: NetworkKind| -> BTreeSet<PartSlot> { sim_core::networks::unconnected_consumers_on(&f.prefab.networks, &f.prefab.parts, &f.prefab.compartments, f.cat, k).into_iter().collect() };
    let (cut_power, cut_fuel, cut_data) = (cut_on(NetworkKind::Power), cut_on(NetworkKind::Fuel), cut_on(NetworkKind::Data));
    for s in sim_core::networks::unconnected_consumers(&f.prefab.networks, &f.prefab.parts, &f.prefab.compartments, f.cat) {
        let def = f.prefab.parts.iter().find(|p| p.slot == s).and_then(|p| calc::def_of(f.cat, p));
        let power_only = def.is_some_and(|d| d.ports.iter().all(|pt| pt.network == NetworkKind::Power) && !d.ports.is_empty());
        if no_power && power_only { continue; }
        // Cut only from Data (and from Power when nothing generates): the console note covers it.
        if no_console && cut_data.contains(&s) && !cut_fuel.contains(&s) && (no_power || !cut_power.contains(&s)) {
            data_cut += 1;
            continue;
        }
        out.push(note(NoteTier::CannotWork, NoteKind::Unconnected, Some(s), 1, m("notes.unconnected").arg(MsgArg::Part(s)), None));
    }
    if data_cut > 0 {
        let space = Space::new(f.bp, f.cat);
        let fix = add_part_near(f.bp, f.cat, &|d| matches!(d.function, PartFunction::Console), space.low_centre(0.55), None);
        out.push(note(NoteTier::CannotWork, NoteKind::Unconnected, None, data_cut, m("notes.no_console"), fix));
    }
    // Consumers the Engineer sheds at combat load: priority 5 first, then 4 and so on (L287), highest slot
    // first within a priority, until demand fits generation.
    if !no_power && budget.combat_kw > budget.generation_kw {
        let mut cons: Vec<(u8, PartSlot, f64)> = f.defs().filter(|(p, _)| p.works()).filter_map(|(p, d)| {
            let draw = calc::rated_draw_kw(d);
            if draw <= 0.0 { return None; }
            let pri = f.bp.officers.power_priority.get(&p.slot).copied().filter(|v| (1..=5).contains(v)).or(Some(p.priority).filter(|v| (1..=5).contains(v))).or(Some(d.power_priority).filter(|v| (1..=5).contains(v))).unwrap_or(5);
            Some((pri, p.slot, draw))
        }).collect();
        cons.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        let mut demand = budget.combat_kw;
        let short = budget.combat_kw - budget.generation_kw;
        let fix = suggest_fix(SuggestKind::EngineAndGeneratorSizing, f.bp, f.design, f.cat);
        for (_, slot, draw) in cons {
            if demand <= budget.generation_kw { break; }
            demand -= draw;
            out.push(note(NoteTier::CannotWork, NoteKind::Unpowered, Some(slot), 1, num(m("notes.unpowered").arg(MsgArg::Part(slot)), short, Unit::Kilowatts), fix.clone()));
        }
    }
    // Pins.
    for issue in &f.design.graph.issues {
        if let GraphIssue::PinnedLostInput { node, port } = issue {
            let text = m("notes.pinned_node").arg(MsgArg::Int(node.0 as i64)).arg(MsgArg::Int(*port as i64));
            out.push(note(NoteTier::CannotWork, NoteKind::PinnedNodeLostInput(*node), None, 1, text, Some(BlueprintEdit::UnpinNode(*node))));
        }
    }
    let mut broken: Vec<u16> = f.prefab.networks.broken_pins.clone();
    broken.sort_unstable();
    broken.dedup();
    for i in broken {
        out.push(note(NoteTier::CannotWork, NoteKind::PinnedRouteBroken(i), None, 1, m("notes.pinned_route").arg(MsgArg::Int(i as i64 + 1)), Some(BlueprintEdit::UnpinRoute { index: i })));
    }
}

// ---------------------------------------------------------------------------------------------------------
// Tier 4: gauges at Caution.

fn tier_four(f: &Facts<'_>, out: &mut Vec<ShipNote>) {
    let g = f.g;
    let st = &f.cat.tuning.stability;
    let beam = fin(f.prefab.dims[2]);
    let all = f.prefab.parts.len();
    if g.float.floats && g.float.check != Check::Fail && beam > 0.0 {
        let r = g.float.gm_over_beam;
        if r < st.gm_caution_low {
            let text = num(num(m("notes.gauge.gm_low"), g.float.gm_m, Unit::Metres), st.gm_caution_low * beam, Unit::Metres);
            out.push(note(NoteTier::GaugeCaution, NoteKind::GaugeCaution("float".into()), None, all, text, suggest_fix(SuggestKind::BallastAndTrim, f.bp, f.design, f.cat)));
        } else if r > st.gm_caution_high {
            let text = num(num(m("notes.gauge.gm_high"), g.float.gm_m, Unit::Metres), st.gm_caution_high * beam, Unit::Metres);
            out.push(note(NoteTier::GaugeCaution, NoteKind::GaugeCaution("float".into()), None, all, text, suggest_fix(SuggestKind::BallastAndTrim, f.bp, f.design, f.cat)));
        }
    }
    if g.float.cap_t > 0.0 && g.float.displacement_t > g.float.cap_t {
        let text = num(num(m("notes.gauge.over_tonnage"), g.float.displacement_t, Unit::Tonnes), g.float.cap_t, Unit::Tonnes);
        out.push(note(NoteTier::GaugeCaution, NoteKind::OverTonnage, None, all, text, None));
    }
    if g.speed.check == Check::Caution && g.speed.turning_circle_m > 0.0 {
        let limit = calc::TURN_CAUTION_LENGTHS * fin(f.prefab.dims[0]);
        let text = num(num(m("notes.gauge.turning"), g.speed.turning_circle_m, Unit::Metres), limit, Unit::Metres);
        out.push(note(NoteTier::GaugeCaution, NoteKind::GaugeCaution("speed".into()), None, 0, text, None));
    }
    if g.power.check == Check::Caution {
        let consumers = calc::power_budget(f.prefab, f.cat, calc::cruise_fraction()).consumers.len();
        let text = num(num(m("notes.gauge.power"), g.power.combat_demand_kw, Unit::Kilowatts), g.power.generation_kw, Unit::Kilowatts);
        out.push(note(NoteTier::GaugeCaution, NoteKind::GaugeCaution("power".into()), None, consumers, text, suggest_fix(SuggestKind::EngineAndGeneratorSizing, f.bp, f.design, f.cat)));
    }
    if g.endurance.check == Check::Caution && g.speed.check != Check::Fail {
        let text = num(num(m("notes.gauge.endurance"), g.endurance.range_km, Unit::Kilometres), calc::ENDURANCE_CAUTION_KM, Unit::Kilometres);
        out.push(note(NoteTier::GaugeCaution, NoteKind::GaugeCaution("endurance".into()), None, 0, text, suggest_fix(SuggestKind::FuelLoad, f.bp, f.design, f.cat)));
    }
    if g.punch.check == Check::Caution {
        out.push(note(NoteTier::GaugeCaution, NoteKind::GaugeCaution("punch".into()), None, 0, m("notes.gauge.punch"), None));
    }
    if g.sight.check == Check::Caution {
        let radars = calc::sensor_sites(f.prefab, f.cat).into_iter().filter(|s| matches!(s.kind, content::part::SensorKind::SearchRadar | content::part::SensorKind::NavRadar)).count();
        if radars == 0 {
            out.push(note(NoteTier::GaugeCaution, NoteKind::GaugeCaution("sight".into()), None, 0, m("notes.gauge.sight_none"), None));
        } else {
            let blind = g.sight.coverage.iter().filter(|c| **c < 0.5).count();
            out.push(note(NoteTier::GaugeCaution, NoteKind::GaugeCaution("sight".into()), None, radars, m("notes.gauge.sight_blind").arg(MsgArg::Int(blind as i64)), suggest_fix(SuggestKind::SensorPlacement, f.bp, f.design, f.cat)));
        }
    }
    if g.resilience.check == Check::Caution {
        let fix = suggest_fix(SuggestKind::BulkheadPlacement, f.bp, f.design, f.cat);
        if g.resilience.one_flooded_survival < 1.0 {
            let worst = g.resilience.worst.iter().find(|c| !c.survives && c.b.is_none()).map(|c| c.a);
            let text = match worst { Some(c) => m("notes.gauge.flood_one").arg(MsgArg::Compartment(c)), None => num(m("notes.gauge.flood_two"), g.resilience.two_flooded_survival * 100.0, Unit::Percent) };
            out.push(note(NoteTier::GaugeCaution, NoteKind::GaugeCaution("resilience".into()), None, all, text, fix));
        } else {
            out.push(note(NoteTier::GaugeCaution, NoteKind::GaugeCaution("resilience".into()), None, all, num(m("notes.gauge.flood_two"), g.resilience.two_flooded_survival * 100.0, Unit::Percent), fix));
        }
    }
    if g.complexity.check == Check::Caution {
        let text = num(num(m("notes.gauge.complexity"), g.complexity.points, Unit::None), g.complexity.caution_at, Unit::None);
        out.push(note(NoteTier::GaugeCaution, NoteKind::OverComplexity, None, 0, text, None));
    }
}

// ---------------------------------------------------------------------------------------------------------
// Tier 5: advisories.

fn tier_five(f: &Facts<'_>, removed: &[content::migrate::RemovedPart], out: &mut Vec<ShipNote>) {
    for (v, case) in &crate::editor::suggest::autoset_status().dropped {
        let text = m("notes.autoset_dropped").arg(MsgArg::Key(TextKey::new(autoset_key(*v)))).arg(MsgArg::Name(v.property_test().to_string())).arg(MsgArg::Name(case.clone()));
        out.push(note(NoteTier::Advisory, NoteKind::AutoSetDropped(v.property_test().to_string()), None, 0, text, None));
    }
    for r in removed {
        let at = format!("{}, {}, {}", r.pos.x, r.pos.y, r.pos.z);
        out.push(note(NoteTier::Advisory, NoteKind::PartRemovedByMigration(r.part.clone()), None, 1, m("notes.part_removed").arg(MsgArg::Name(r.part.0.clone())).arg(MsgArg::Name(at)), None));
    }
    for issue in &f.design.graph.issues {
        let (key, arg) = match issue {
            GraphIssue::PinnedLostInput { .. } => continue,
            GraphIssue::TypeMismatch { node, .. } => ("notes.graph.type_mismatch", MsgArg::Int(node.0 as i64)),
            GraphIssue::Cycle { node } => ("notes.graph.cycle", MsgArg::Int(node.0 as i64)),
            GraphIssue::RuleSinkMissing { rule } => ("notes.graph.rule_sink", MsgArg::Int(*rule as i64 + 1)),
            GraphIssue::TooManyRules(n) => ("notes.graph.too_many_rules", MsgArg::Int(*n as i64)),
            GraphIssue::ScriptDisabled { script } => ("notes.graph.script_disabled", MsgArg::Int(*script as i64 + 1)),
            GraphIssue::LayerNotAllowed { node } => ("notes.graph.layer", MsgArg::Int(node.0 as i64)),
        };
        let mut text = m(key).arg(arg);
        if let GraphIssue::TooManyRules(_) = issue { text = text.arg(MsgArg::Int(content::graph::MAX_RULES as i64)); }
        let fix = match issue { GraphIssue::TypeMismatch { node, .. } | GraphIssue::Cycle { node } | GraphIssue::LayerNotAllowed { node } if f.bp.graph.pinned.nodes.iter().any(|n| n.id == *node) => Some(BlueprintEdit::UnpinNode(*node)), _ => None };
        out.push(note(NoteTier::Advisory, NoteKind::Advisory(key.trim_start_matches("notes.graph.").to_string()), None, 1, text, fix));
    }
}
