//! Unit `gd-editor-control`. The control ladder's L0 to L2 on `BpEditor`, stored in the blueprint: doctrine
//! presets, tuning sliders, doctrine rows, the rules table, power priorities, mount limits, emission
//! settings and the repair order. DESIGN.md L322-336 (control ladder; at most 32 rules, evaluated top to
//! bottom), L287-295 (power priority 1 to 5 and the default table), L351 (every automatic value carries an
//! "A" chip; overriding swaps it for a hand glyph), L200-212 (auto-set against Suggest), L479-489 (engagement
//! doctrine), L505-514 (emission control), L969-971 (rulesets cap the layer).
//! Slider definitions come from `sim_systems::officers::tuning`; the same shapes serve the in-sortie panel
//! (`Hud.sliders`, `Orders.set_slider`).
//!
//! # The one override rule
//! Every value here is either automatic (nothing stored: the preset, the default table or the auto-set value
//! decides, and the UI shows the A chip) or overridden (stored in `OfficerSettings`, hand glyph). Storing a
//! value equal to what it would be without the override removes the override instead, for sliders, doctrine
//! rows, power priorities, the repair order and the emission ranges alike, so the A chip comes back by itself.
//!
//! # Choices the spec leaves open
//! - The ship's EMCON setting is automatic while it equals its preset's; changing the preset carries an
//!   automatic setting along and leaves an overridden one alone.
//! - Doctrine rows may never lower the stance floor below Suspect (L486: nothing auto-engages a Neutral or
//!   Friend track). A salvo of 0 is the hold encoding the doctrine stage reads.
//! - A rule is accepted only when the design has the sink it names (the generated graph has a node for it),
//!   the parts its operands read exist, and its value fits one of the sink's ports (the "Rule targets" rules
//!   of `graph::compile`). Each rule builder picker offers one value type per sink: retargeting for weapons,
//!   the sink's own type elsewhere.
//! - Mount limits are clamped to the mount's mechanism (full circle in training, the gun's elevation range);
//!   fixed launchers and decoy launchers have nothing to limit.
//! - `reset_control` returns presets, sliders, doctrine, emission, priorities, limits, repair order, radar
//!   rates and rules to stock in one undo step. Graph pins, stage replacements and scripts stay: they have
//!   their own revert controls in the graph view.
use super::BpEditor;
use crate::convert::{d, empty_msg, get_f64, get_i64, msg_dict};
use crate::enums::GdEnum;
use crate::text::Lookup;
use content::blueprint::Blueprint;
use content::catalogue::Catalogue;
use content::enums::{ControlLayer, DoctrinePreset, Emcon, OfficerRole, Stance};
use content::graph::{Condition, DoctrineEntry, MountLimits, OfficerSettings, Operand, RepairPriority, Rule, RuleValue, SinkKind, TargetCategory, Value, WeaponClass, MAX_RULES};
use content::ids::PartSlot;
use content::part::{PartDef, PartFunction};
use godot::prelude::*;
use sim_systems::officers::tuning;
use sim_systems::protocol::{Msg, MsgArg};

/// Text keys this unit emits (`game/i18n/dock.csv`).
pub(crate) const CONTROL_KEYS: &[&str] = &[
    "dock.undo.preset", "dock.undo.slider", "dock.undo.doctrine", "dock.undo.rules", "dock.undo.power_priority", "dock.undo.mount_limits",
    "dock.undo.emission", "dock.undo.repair_order", "dock.undo.reset_control",
    "dock.rule.unreadable", "dock.rule.no_rule", "dock.rule.no_sink", "dock.rule.bad_value", "dock.rule.no_part",
    "dock.name.plain", "dock.autoset.dropped",
    "dock.rule.operand.const", "dock.rule.operand.own_speed_kn", "dock.rule.operand.own_heel_deg", "dock.rule.operand.fuel_fraction",
    "dock.rule.operand.power_margin", "dock.rule.operand.hull_health", "dock.rule.operand.nearest_contact_range_m",
    "dock.rule.operand.contact_count", "dock.rule.operand.inbound_weapons", "dock.rule.operand.part_health",
    "dock.rule.operand.ammo_fraction", "dock.rule.operand.sortie_time_s",
];

/// Rule operands for the builder: (kind, unit, needs).
const OPERANDS: [(&str, &str, &str); 12] = [
    ("const", "none", ""), ("own_speed_kn", "kn", ""), ("own_heel_deg", "deg", ""), ("fuel_fraction", "pct", ""), ("power_margin", "kw", ""),
    ("hull_health", "pct", ""), ("nearest_contact_range_m", "m", "filter"), ("contact_count", "none", "filter"), ("inbound_weapons", "none", ""),
    ("part_health", "pct", "part"), ("ammo_fraction", "pct", "part"), ("sortie_time_s", "s", ""),
];

// ---------------------------------------------------------------------------------------------------------
// Part names and kinds (pure).

/// The display name of a part of the design under edit as a sentence: its label, else its category word and
/// order ("Gun 2"), the same rule as `PartPlacement::label` and the HUD use.
pub(crate) fn part_name_msg(bp: &Blueprint, slot: PartSlot, cat: &Catalogue) -> Msg {
    let Some(p) = bp.parts.get(slot.index()) else { return Msg::new("units.part.numbered").arg(MsgArg::Int(slot.0 as i64 + 1)) };
    if let Some(label) = p.label.as_ref().map(|l| l.trim()).filter(|l| !l.is_empty()) { return Msg::new("dock.name.plain").arg(MsgArg::Name(label.to_string())); }
    let Some(def) = cat.part(&p.part) else { return Msg::new("dock.name.plain").arg(MsgArg::Name(p.part.as_str().to_string())) };
    let order = bp.parts[..=slot.index()].iter().filter(|q| cat.part(&q.part).is_some_and(|d| d.category == def.category)).count();
    let word = crate::text::word_key_of("category", def.category.to_gd()).unwrap_or_default();
    Msg::new("units.part.generated").arg(MsgArg::Key(content::ids::TextKey::new(&word))).arg(MsgArg::Int(order as i64))
}
/// The same, as text in the player's language (for embedding in a sentence as a name argument).
pub(crate) fn part_name_text(lk: &dyn Lookup, bp: &Blueprint, slot: PartSlot, cat: &Catalogue) -> String { crate::text::msg_text(lk, &part_name_msg(bp, slot, cat)) }

fn def_of<'a>(bp: &Blueprint, slot: PartSlot, cat: &'a Catalogue) -> Option<&'a PartDef> { bp.parts.get(slot.index()).and_then(|p| cat.part(&p.part)) }

/// Whether a part trains and elevates (guns, trainable launchers).
fn trainable(def: &PartDef) -> bool {
    match &def.function { PartFunction::Gun(_) => true, PartFunction::Launcher(l) => l.train_rate_dps.is_some_and(|r| r.is_finite() && r > 0.0), _ => false }
}
/// Whether a part is a weapon mount the officers aim (guns, launchers, decoy launchers).
pub(crate) fn is_mount(def: &PartDef) -> bool { matches!(def.function, PartFunction::Gun(_) | PartFunction::Launcher(_) | PartFunction::Decoy(_)) }

/// The mechanical limits of a mount: the full circle in training, the gun's elevation range (launchers train
/// level). `None` for parts with nothing to limit.
pub(crate) fn mechanical_limits(def: &PartDef) -> Option<MountLimits> {
    match &def.function {
        PartFunction::Gun(g) => {
            let lo = if g.elevation_min_deg.is_finite() { g.elevation_min_deg.clamp(-90.0, 90.0) } else { -10.0 };
            let hi = if g.elevation_max_deg.is_finite() { g.elevation_max_deg.clamp(-90.0, 90.0) } else { 85.0 };
            Some(MountLimits { train_min_deg: -180.0, train_max_deg: 180.0, elevation_min_deg: lo.min(hi), elevation_max_deg: lo.max(hi) })
        }
        PartFunction::Launcher(_) if trainable(def) => Some(MountLimits { train_min_deg: -180.0, train_max_deg: 180.0, elevation_min_deg: 0.0, elevation_max_deg: 0.0 }),
        _ => None,
    }
}

/// Hand-set limits read from the caller, clamped to the mechanism; `None` when a value is missing or the
/// ranges are empty.
pub(crate) fn clamp_limits(want: MountLimits, mech: &MountLimits) -> Option<MountLimits> {
    let v = [want.train_min_deg, want.train_max_deg, want.elevation_min_deg, want.elevation_max_deg];
    if !v.iter().all(|x| x.is_finite()) { return None; }
    let t0 = want.train_min_deg.clamp(mech.train_min_deg, mech.train_max_deg);
    let t1 = want.train_max_deg.clamp(mech.train_min_deg, mech.train_max_deg);
    let e0 = want.elevation_min_deg.clamp(mech.elevation_min_deg, mech.elevation_max_deg);
    let e1 = want.elevation_max_deg.clamp(mech.elevation_min_deg, mech.elevation_max_deg);
    if t0 > t1 || e0 > e1 { return None; }
    Some(MountLimits { train_min_deg: t0, train_max_deg: t1, elevation_min_deg: e0, elevation_max_deg: e1 })
}

// ---------------------------------------------------------------------------------------------------------
// Settings rules (pure).

/// Set the ship's preset: an automatic EMCON setting follows it.
pub(crate) fn with_ship_preset(s: &OfficerSettings, preset: DoctrinePreset) -> OfficerSettings {
    let mut n = s.clone();
    let auto_emcon = s.emcon == tuning::preset(s.preset).emcon;
    n.preset = preset;
    if auto_emcon { n.emcon = tuning::preset(preset).emcon; }
    n
}

/// The value a slider has without an override (the preset's value, else its default).
pub(crate) fn slider_base(s: &OfficerSettings, key: &str) -> f64 {
    let mut probe = s.clone();
    probe.sliders.remove(key);
    tuning::slider_value(&probe, key)
}

/// Clamp and step a slider value. `None` for an unknown key or a non-finite value.
pub(crate) fn stepped(key: &str, value: f64) -> Option<f64> {
    let def = tuning::slider(key)?;
    if !value.is_finite() { return None; }
    let (lo, hi) = (def.min.min(def.max), def.min.max(def.max));
    let step = crate::convert::slider_step(lo, hi);
    let v = lo + ((value.clamp(lo, hi) - lo) / step).round() * step;
    Some(((v.clamp(lo, hi)) * 1e9).round() / 1e9)
}

/// Store a slider value under the one override rule. Returns the value applied.
pub(crate) fn with_slider(s: &OfficerSettings, key: &str, value: f64) -> Option<(OfficerSettings, f64)> {
    let v = stepped(key, value)?;
    let mut n = s.clone();
    if (v - slider_base(s, key)).abs() <= 1e-9 { n.sliders.remove(key); } else { n.sliders.insert(key.to_string(), v); }
    Some((n, v))
}

/// The ship-level doctrine rows: the preset's defaults with the ship's own entries over them, sorted by
/// (weapon, target); `true` marks a stored entry.
pub(crate) fn ship_doctrine(s: &OfficerSettings, weapons: Option<&[WeaponClass]>) -> Vec<(DoctrineEntry, bool)> {
    let mut rows: Vec<(DoctrineEntry, bool)> = sim_systems::doctrine::default_entries(s.preset).into_iter().map(|e| (e, false)).collect();
    for e in &s.doctrine {
        match rows.iter_mut().find(|(r, _)| r.weapon == e.weapon && r.target == e.target) { Some(r) => *r = (*e, true), None => rows.push((*e, true)) }
    }
    if let Some(w) = weapons { rows.retain(|(e, _)| w.contains(&e.weapon)); }
    rows.sort_by_key(|(e, _)| (e.weapon, e.target));
    rows
}

/// The rows of one mount, in the order the doctrine stage resolves them (`doctrine::resolve`): the ship's
/// rows of its weapon class (ship preset, then the ship's own entries), then the mount's own preset, then
/// the mount's own entries.
pub(crate) fn mount_doctrine(s: &OfficerSettings, slot: PartSlot, weapon: WeaponClass) -> Vec<(DoctrineEntry, bool)> {
    let mut rows: Vec<(DoctrineEntry, bool)> = ship_doctrine(s, Some(&[weapon])).into_iter().map(|(e, _)| (e, false)).collect();
    let over = |e: DoctrineEntry, own: bool, rows: &mut Vec<(DoctrineEntry, bool)>| match rows.iter_mut().find(|(r, _)| r.target == e.target) { Some(r) => *r = (e, own), None => rows.push((e, own)) };
    if let Some(p) = s.mount_presets.get(&slot) {
        for e in sim_systems::doctrine::default_entries(*p).into_iter().filter(|e| e.weapon == weapon) { over(e, false, &mut rows); }
    }
    for e in s.mount_doctrine.get(&slot).into_iter().flatten().filter(|e| e.weapon == weapon) { over(*e, true, &mut rows); }
    rows.sort_by_key(|(e, _)| (e.weapon, e.target));
    rows
}

/// Whether a doctrine row may be stored: the stance floor stays at Suspect or above (L486).
pub(crate) fn doctrine_row_ok(e: &DoctrineEntry) -> bool { e.min_stance >= Stance::Suspect && e.weapon != WeaponClass::Decoy }

/// Store one row under the one override rule. `mount` is the slot and its weapon class for a mount row.
pub(crate) fn with_doctrine(s: &OfficerSettings, mount: Option<(PartSlot, WeaponClass)>, e: DoctrineEntry) -> Option<OfficerSettings> {
    if !doctrine_row_ok(&e) { return None; }
    let mut n = s.clone();
    match mount {
        None => {
            n.doctrine.retain(|r| !(r.weapon == e.weapon && r.target == e.target));
            let inherited = ship_doctrine(&n, None).into_iter().find(|(r, _)| r.weapon == e.weapon && r.target == e.target).map(|(r, _)| r);
            if inherited != Some(e) { n.doctrine.push(e); n.doctrine.sort_by_key(|r| (r.weapon, r.target)); }
        }
        Some((slot, weapon)) => {
            if e.weapon != weapon { return None; }
            let list = n.mount_doctrine.entry(slot).or_default();
            list.retain(|r| r.target != e.target);
            let inherited = mount_doctrine(&n, slot, weapon).into_iter().find(|(r, _)| r.target == e.target).map(|(r, _)| r);
            let list = n.mount_doctrine.entry(slot).or_default();
            if inherited != Some(e) { list.push(e); list.sort_by_key(|r| (r.weapon, r.target)); }
            if list.is_empty() { n.mount_doctrine.remove(&slot); }
        }
    }
    Some(n)
}

/// The row a doctrine row would have without its own override (the ship's preset and entries for a ship
/// row; the ship's rows, the mount's preset and the ship's entries for a mount row), for one-click revert.
/// `None` for a row that exists only as an override.
pub(crate) fn inherited_doctrine_row(s: &OfficerSettings, mount: Option<(PartSlot, WeaponClass)>, e: &DoctrineEntry) -> Option<DoctrineEntry> {
    let mut probe = s.clone();
    match mount {
        None => {
            probe.doctrine.retain(|r| !(r.weapon == e.weapon && r.target == e.target));
            ship_doctrine(&probe, None).into_iter().find(|(r, _)| r.weapon == e.weapon && r.target == e.target).map(|(r, _)| r)
        }
        Some((slot, weapon)) => {
            if let Some(list) = probe.mount_doctrine.get_mut(&slot) { list.retain(|r| r.target != e.target); }
            mount_doctrine(&probe, slot, weapon).into_iter().find(|(r, _)| r.target == e.target).map(|(r, _)| r)
        }
    }
}

/// The repair order in force, most urgent first.
pub(crate) fn repair_order_of(s: &OfficerSettings) -> Vec<RepairPriority> {
    let ok = s.repair_order.len() == RepairPriority::DEFAULT_ORDER.len() && RepairPriority::DEFAULT_ORDER.iter().all(|p| s.repair_order.contains(p));
    if ok { s.repair_order.clone() } else { RepairPriority::DEFAULT_ORDER.to_vec() }
}
/// Store a repair order: a permutation of every job, the default stored as empty.
pub(crate) fn with_repair_order(s: &OfficerSettings, order: &[RepairPriority]) -> Option<OfficerSettings> {
    if order.len() != RepairPriority::DEFAULT_ORDER.len() || !RepairPriority::DEFAULT_ORDER.iter().all(|p| order.contains(p)) { return None; }
    let mut n = s.clone();
    n.repair_order = if order == RepairPriority::DEFAULT_ORDER { Vec::new() } else { order.to_vec() };
    Some(n)
}

/// Default and range of an emission or self-defence setting, from the stage parameter it bakes into.
fn param_spec(stage: content::graph::OfficerStage, key: &str) -> Option<sim_systems::graph::ParamSpec> { sim_systems::graph::library::stage_params_spec(stage).into_iter().find(|p| p.key == key) }
fn burst_spec(cat: &Catalogue) -> (f64, f64, f64) {
    let p = param_spec(content::graph::OfficerStage::EmissionController, "sensors.burst_trigger_range_m");
    (cat.tuning.doctrine.burst_trigger_m, p.as_ref().map_or(2_000.0, |p| p.min), p.as_ref().map_or(60_000.0, |p| p.max))
}
fn self_defence_spec(cat: &Catalogue) -> (f64, f64, f64) {
    let p = param_spec(content::graph::OfficerStage::ThreatRanking, "gunnery.self_defence_range_m");
    (cat.tuning.doctrine.self_defence_range_m, p.as_ref().map_or(500.0, |p| p.min), p.as_ref().map_or(20_000.0, |p| p.max))
}
/// A range setting under the one override rule: negative = default, else clamped; the default value is stored
/// as automatic.
fn range_setting(v: f64, (default, lo, hi): (f64, f64, f64)) -> Option<f64> {
    if !v.is_finite() || v < 0.0 { return None; }
    let v = v.clamp(lo, hi);
    if (v - default).abs() <= 1e-6 { None } else { Some(v) }
}

// ---------------------------------------------------------------------------------------------------------
// Rules (pure).

/// Whether a rule value can drive a sink, by the compiler's rule-target rules.
pub(crate) fn value_fits_sink(sink: SinkKind, value: &RuleValue) -> bool {
    use sim_systems::graph::library;
    match value {
        RuleValue::NearestContact(_) => matches!(sink, SinkKind::MountFirePermission(_) | SinkKind::MountAimPoint(_) | SinkKind::MissileTarget(_) | SinkKind::MissileLaunch(_)),
        RuleValue::Set(v) => {
            if !value_finite(v) { return false; }
            match v {
                Value::Preset(_) => return sink == SinkKind::ShipPreset,
                Value::Rung(_) => return sink == SinkKind::TrackIdOverride,
                Value::None => return false,
                _ => {}
            }
            if sink == SinkKind::ShipPreset { return matches!(v, Value::Scalar(x) if (0.0..=2.0).contains(&x.round())); }
            if matches!(sink, SinkKind::PowerPriority(_)) { return matches!(v, Value::Scalar(x) if (1.0..=5.0).contains(&x.round())); }
            // The value must be of a port's own type (a number may also set an angle); the compiler's looser
            // Bool-as-Scalar mixing would let "set helm speed to true" through.
            use content::graph::PortType as T;
            let ty = library::value_type(v);
            library::sink_sig(sink).inputs.iter().any(|p| p.ty == ty || (ty == T::Scalar && p.ty == T::Angle))
        }
    }
}
fn value_finite(v: &Value) -> bool { match v { Value::Scalar(x) => x.is_finite(), Value::Vec3(a) => a.iter().all(|x| x.is_finite()), _ => true } }

/// Every part a condition reads.
fn operand_parts(c: &Condition, out: &mut Vec<PartSlot>, depth: usize) {
    if depth > 16 { return; }
    let mut op = |o: &Operand| if let Operand::PartHealth(p) | Operand::AmmoFraction(p) = o { out.push(*p); };
    match c {
        Condition::Compare { lhs, rhs, .. } => { op(lhs); op(rhs); }
        Condition::All(items) | Condition::Any(items) => for i in items { operand_parts(i, out, depth + 1) },
        Condition::Not(i) => operand_parts(i, out, depth + 1),
    }
}

/// Every non-finite constant in a condition makes it unreadable.
fn condition_finite(c: &Condition, depth: usize) -> bool {
    if depth > 16 { return false; }
    match c {
        Condition::Compare { lhs, rhs, .. } => [lhs, rhs].iter().all(|o| !matches!(o, Operand::Const(x) if !x.is_finite())),
        Condition::All(items) | Condition::Any(items) => items.iter().all(|i| condition_finite(i, depth + 1)),
        Condition::Not(i) => condition_finite(i, depth + 1),
    }
}

/// Problems that keep a rule from working on this design, as sentences. `has_sink` says whether the design's
/// generated graph has the sink (`None` when not known yet: the part's function decides).
pub(crate) fn rule_problems(rule: &Rule, bp: &Blueprint, cat: &Catalogue, has_sink: Option<bool>, lk: &dyn Lookup) -> Vec<Msg> {
    let mut out = Vec::new();
    if !condition_finite(&rule.when, 0) { out.push(Msg::new("dock.rule.unreadable")); }
    let sink_ok = has_sink.unwrap_or_else(|| sink_plausible(rule.sink, bp, cat));
    let sink_text = crate::convert::sink_text(lk, &rule.sink);
    let sink_text = match sink_part(rule.sink) { Some(p) if p.index() < bp.parts.len() => crate::text::fill(&lk.tr(&format!("units.sink.{}", sink_kind_name(rule.sink))), &[part_name_text(lk, bp, p, cat)]), _ => sink_text };
    if !sink_ok { out.push(Msg::new("dock.rule.no_sink").arg(MsgArg::Name(sink_text.clone()))); }
    if sink_ok && !value_fits_sink(rule.sink, &rule.value) { out.push(Msg::new("dock.rule.bad_value").arg(MsgArg::Name(sink_text))); }
    let mut parts = Vec::new();
    operand_parts(&rule.when, &mut parts, 0);
    if parts.iter().any(|p| p.index() >= bp.parts.len() || def_of(bp, *p, cat).is_none()) { out.push(Msg::new("dock.rule.no_part")); }
    out
}

/// The part a per-part sink is bound to.
fn sink_part(sink: SinkKind) -> Option<PartSlot> {
    match sink {
        SinkKind::MountAimPoint(s) | SinkKind::MountFirePermission(s) | SinkKind::MissileLaunch(s) | SinkKind::MissileTarget(s) | SinkKind::EmissionState(s) | SinkKind::PowerPriority(s) | SinkKind::Pump(s) | SinkKind::Door(s) => Some(s),
        _ => None,
    }
}
fn sink_kind_name(sink: SinkKind) -> String { sim_systems::graph::library::node_key(&content::graph::NodeKind::Sink(sink)).trim_start_matches("graph.node.").to_string() }

/// Without the generated graph: whether the part a sink names can carry it.
pub(crate) fn sink_plausible(sink: SinkKind, bp: &Blueprint, cat: &Catalogue) -> bool {
    let def = |s: PartSlot| def_of(bp, s, cat);
    match sink {
        SinkKind::MountAimPoint(s) | SinkKind::MountFirePermission(s) => def(s).is_some_and(|d| matches!(d.function, PartFunction::Gun(_))),
        SinkKind::MissileLaunch(s) | SinkKind::MissileTarget(s) => def(s).is_some_and(|d| matches!(d.function, PartFunction::Launcher(_))),
        SinkKind::EmissionState(s) => def(s).is_some_and(|d| matches!(&d.function, PartFunction::Jammer { .. }) || matches!(&d.function, PartFunction::Sensor(x) if matches!(x.kind, content::part::SensorKind::SearchRadar | content::part::SensorKind::NavRadar | content::part::SensorKind::FireControlRadar | content::part::SensorKind::ActiveSonar))),
        SinkKind::PowerPriority(s) => def(s).is_some_and(|d| sim_systems::officers::engineer::default_priority(d) > 0),
        SinkKind::Pump(s) => def(s).is_some_and(|d| matches!(d.function, PartFunction::Pump { .. })),
        SinkKind::Door(s) => def(s).is_some_and(|d| matches!(d.function, PartFunction::Door)),
        SinkKind::FloodValve(_) | SinkKind::HelmHeading | SinkKind::HelmSpeed | SinkKind::TrackStanceOverride | SinkKind::TrackIdOverride | SinkKind::ShipPreset | SinkKind::ShipEmcon => true,
    }
}

/// The value type the rule builder offers for a sink.
pub(crate) fn sink_value_type(sink: SinkKind) -> &'static str {
    match sink {
        SinkKind::MountFirePermission(_) | SinkKind::MountAimPoint(_) | SinkKind::MissileTarget(_) | SinkKind::MissileLaunch(_) => "nearest_contact",
        SinkKind::EmissionState(_) | SinkKind::Pump(_) | SinkKind::Door(_) | SinkKind::FloodValve(_) => "bool",
        SinkKind::HelmHeading | SinkKind::HelmSpeed | SinkKind::PowerPriority(_) => "scalar",
        SinkKind::TrackStanceOverride => "stance",
        SinkKind::TrackIdOverride => "rung",
        SinkKind::ShipPreset => "preset",
        SinkKind::ShipEmcon => "emcon",
    }
}

// ---------------------------------------------------------------------------------------------------------
// Layers (pure).

/// Control layers a design uses.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LayerReport { pub by_officer: [ControlLayer; 5], pub in_use: ControlLayer, pub overrides: u32 }

fn bump(l: &mut ControlLayer, to: ControlLayer) { if to > *l { *l = to; } }
fn role_index(r: OfficerRole) -> usize { r.to_gd().clamp(0, 4) as usize }

/// What layer each officer is at by the design's stored settings, rules and overrides, and how many values
/// are overridden. `compiled` is the compiled graph's per-officer layers, when built.
pub(crate) fn layer_report(bp: &Blueprint, cat: &Catalogue, compiled: Option<&std::collections::BTreeMap<OfficerRole, ControlLayer>>) -> LayerReport {
    use sim_systems::graph::generate;
    let s = &bp.officers;
    let mut by = [ControlLayer::L0Doctrine; 5];
    let l1 = |by: &mut [ControlLayer; 5], r: OfficerRole| bump(&mut by[role_index(r)], ControlLayer::L1Tune);
    for (k, v) in &s.sliders { if v.is_finite() { if let Some(d) = tuning::slider(k) { l1(&mut by, d.role); } } }
    if s.emcon != tuning::preset(s.preset).emcon || s.burst_trigger_range_m.is_some() || !s.radar_hz.is_empty() { l1(&mut by, OfficerRole::Sensors); }
    if s.self_defence_range_m.is_some() || !s.doctrine.is_empty() || !s.mount_doctrine.is_empty() || !s.mount_limits.is_empty() { l1(&mut by, OfficerRole::Gunnery); }
    if !s.power_priority.is_empty() { l1(&mut by, OfficerRole::Engineer); }
    if !s.repair_order.is_empty() { l1(&mut by, OfficerRole::DamageControl); }
    for r in &bp.rules { bump(&mut by[role_index(sim_systems::graph::library::sink_officer(r.sink))], ControlLayer::L2Rules); }
    for r in &bp.graph.replaced {
        let scripted = r.subgraph.nodes.iter().any(|n| matches!(n.kind, content::graph::NodeKind::Script(_)));
        bump(&mut by[role_index(r.stage.officer())], if scripted { ControlLayer::L4Script } else { ControlLayer::L3Graph });
    }
    for n in &bp.graph.pinned.nodes {
        let layer = if matches!(n.kind, content::graph::NodeKind::Script(_)) { ControlLayer::L4Script } else { ControlLayer::L3Graph };
        let officer = generate::officer_of_code(generate::id_fields(n.id).0).filter(|_| n.id.0 < content::graph::USER_NODE_BASE).or(match &n.kind {
            content::graph::NodeKind::Stage(st) => Some(st.officer()),
            content::graph::NodeKind::Sink(k) => Some(sim_systems::graph::library::sink_officer(*k)),
            _ => None,
        });
        if let Some(o) = officer { bump(&mut by[role_index(o)], layer); }
    }
    if let Some(c) = compiled { for (r, l) in c { bump(&mut by[role_index(*r)], *l); } }
    let mut in_use = by.iter().copied().max().unwrap_or(ControlLayer::L0Doctrine);
    let forked = layers_banned(bp, cat, &content::ruleset::Ruleset { kind: content::ruleset::RulesetKind::Standard, luau: true, ..Default::default() });
    if !bp.graph.pinned.nodes.is_empty() || !bp.graph.pinned.edges.is_empty() || forked { bump(&mut in_use, ControlLayer::L3Graph); }
    if !bp.graph.scripts.is_empty() { bump(&mut in_use, ControlLayer::L4Script); }
    let overrides = (s.preset != DoctrinePreset::default()) as usize + s.mount_presets.len() + s.sliders.len()
        + (s.emcon != tuning::preset(s.preset).emcon) as usize + s.burst_trigger_range_m.is_some() as usize + s.self_defence_range_m.is_some() as usize
        + s.doctrine.len() + s.mount_doctrine.values().map(Vec::len).sum::<usize>() + (!s.repair_order.is_empty()) as usize
        + s.power_priority.len() + s.mount_limits.len() + s.radar_hz.len() + bp.rules.len() + bp.graph.pinned.nodes.len() + bp.graph.replaced.len() + bp.graph.scripts.len();
    LayerReport { by_officer: by, in_use, overrides: overrides.min(u32::MAX as usize) as u32 }
}

/// Whether the design uses a layer the ruleset bans, by the validator's own rule (L313, L2119-2121):
/// graph edits or a player-authored kit graph under a ruleset below L3, scripts where Luau may not run.
pub(crate) fn layers_banned(bp: &Blueprint, cat: &Catalogue, r: &content::ruleset::Ruleset) -> bool {
    let graph_edits = !bp.graph.pinned.nodes.is_empty() || !bp.graph.pinned.edges.is_empty() || !bp.graph.replaced.is_empty();
    let forked = bp.kits.iter().filter_map(|k| bp.embedded_kits.get(&k.kit).or_else(|| cat.kit(&k.kit))).any(|d| {
        let stock = cat.kit(&d.id).is_some_and(|c| c == d);
        !stock && (d.kind == content::kit::KitKind::Officer || !d.graph.nodes.is_empty() || !d.graph.edges.is_empty())
    });
    let scripted = !bp.graph.scripts.is_empty() || bp.graph.pinned.nodes.iter().chain(bp.graph.replaced.iter().flat_map(|x| &x.subgraph.nodes)).any(|n| matches!(n.kind, content::graph::NodeKind::Script(_)));
    (r.kind.max_layer() < ControlLayer::L3Graph && (graph_edits || forked)) || (scripted && !r.luau)
}

// ---------------------------------------------------------------------------------------------------------
// Engine side.

fn issues_of(msgs: Vec<Msg>) -> Array<VarDictionary> {
    let mut a = Array::new();
    for m in msgs { a.push(&d! { "text" => msg_dict(&m), "fatal" => false, "part" => -1i64, "cell" => Variant::nil() }); }
    a
}

impl BpEditor {
    /// Commit a change of the officer settings as one undo step and announce it.
    fn commit_settings(&mut self, next: OfficerSettings, label: &str) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        if m.blueprint.officers == next { return true; }
        let mut bp = m.blueprint.clone();
        bp.officers = next;
        let changed = self.commit(bp, label);
        if changed { self.base_mut().emit_signal("control_changed", &[]); }
        changed
    }
    fn commit_rules(&mut self, rules: Vec<Rule>) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        let mut bp = m.blueprint.clone();
        bp.rules = rules;
        let changed = self.commit(bp, "dock.undo.rules");
        if changed { self.base_mut().emit_signal("control_changed", &[]); self.base_mut().emit_signal("graph_changed", &[]); }
        changed
    }
    fn settings(&self) -> Option<&OfficerSettings> { self.model.as_ref().map(|m| &m.blueprint.officers) }

    /// The mounts of the design under edit, when the newest built design has the same parts.
    pub(crate) fn mounts_now(&self) -> Option<Vec<sim_systems::doctrine::MountInfo>> {
        let m = self.model.as_ref()?;
        let d = self.latest_design()?;
        (d.blueprint().parts == m.blueprint.parts).then(|| d.mounts.clone())
    }
    /// The weapon class of a mount of the design under edit.
    fn weapon_of(&self, slot: PartSlot) -> Option<WeaponClass> { self.mounts_now()?.into_iter().find(|mi| mi.slot == slot).map(|mi| mi.weapon) }

    /// Rule problems under the dock's design.
    fn rule_check(&self, rule: &Rule) -> Vec<Msg> {
        let Some(m) = self.model.as_ref() else { return vec![Msg::new("dock.io.not_open")] };
        let cat = crate::services::services().catalogue();
        let has_sink = self.generated_has_sink(rule.sink);
        rule_problems(rule, &m.blueprint, &cat, has_sink, &crate::text::EngineLookup::new())
    }
}

#[godot_api(secondary)]
impl BpEditor {
    /// Doctrine preset of the ship (`mount == Bp.NONE`) or one mount: `{ "preset": int, "overridden": bool }`.
    #[func]
    fn preset(&self, mount: i64) -> VarDictionary {
        let Some(s) = self.settings() else { return d! { "preset" => DoctrinePreset::default().to_gd(), "overridden" => false } };
        if mount < 0 { return d! { "preset" => s.preset.to_gd(), "overridden" => s.preset != DoctrinePreset::default() }; }
        match crate::convert::slot_from_gd(mount).and_then(|p| s.mount_presets.get(&p)) {
            Some(p) => d! { "preset" => p.to_gd(), "overridden" => true },
            None => d! { "preset" => s.preset.to_gd(), "overridden" => false },
        }
    }
    /// `preset` is `Bp.PRESET_*`; `Bp.NONE` on a mount returns it to the ship preset.
    #[func]
    fn set_preset(&mut self, mount: i64, preset: i64) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        let s = &m.blueprint.officers;
        if mount < 0 {
            let Some(p) = DoctrinePreset::from_gd(preset) else { return false };
            let next = with_ship_preset(s, p);
            return self.commit_settings(next, "dock.undo.preset");
        }
        let Some(slot) = crate::convert::slot_from_gd(mount) else { return false };
        let cat = crate::services::services().catalogue();
        if !def_of(&m.blueprint, slot, &cat).is_some_and(is_mount) { return false; }
        let mut next = s.clone();
        if preset < 0 {
            next.mount_presets.remove(&slot);
        } else {
            let Some(p) = DoctrinePreset::from_gd(preset) else { return false };
            next.mount_presets.insert(slot, p);
        }
        self.commit_settings(next, "dock.undo.preset")
    }
    /// L1 sliders of one officer (`Bp.OFFICER_*`, or `Bp.NONE` for all): `Array[SliderDict]`, basic first.
    #[func]
    fn sliders(&self, officer: i64) -> Array<VarDictionary> {
        let role = if officer < 0 { None } else { match OfficerRole::from_gd(officer) { Some(r) => Some(r), None => return Array::new() } };
        let default_settings = OfficerSettings::default();
        let s = self.settings().unwrap_or(&default_settings);
        let mut defs: Vec<&tuning::SliderDef> = tuning::sliders().iter().filter(|d| role.is_none_or(|r| d.role == r)).collect();
        // Basic first, then by officer and registry order.
        defs.sort_by_key(|d| (!d.primary, d.role.to_gd()));
        let mut out = Array::new();
        for def in defs {
            let stored = s.sliders.get(def.key).copied().filter(|v| v.is_finite());
            let mut dict = crate::convert::slider_dict(def.key, tuning::slider_value(s, def.key), stored.is_some());
            dict.set("default", slider_base(s, def.key));
            let unit = if def.key.ends_with("_m") { "m" } else if def.key.ends_with("_s") { "s" } else { "none" };
            dict.set("unit", &StringName::from(unit));
            out.push(&dict);
        }
        out
    }
    /// Returns the value applied after clamping and stepping.
    #[func]
    fn set_slider(&mut self, key: GString, value: f64) -> f64 {
        let key = key.to_string();
        let Some(s) = self.settings() else { return value.clamp(-1e12, 1e12) };
        let current = tuning::slider_value(s, &key);
        let Some((next, applied)) = with_slider(s, &key, value) else { return current };
        self.commit_settings(next, "dock.undo.slider");
        crate::services::services().tutorial_signal(sim_systems::tutorial::Signal::SliderMoved { key });
        applied
    }
    /// Back to the default ("A" chip returns).
    #[func]
    fn reset_slider(&mut self, key: GString) {
        let Some(s) = self.settings() else { return };
        let key = key.to_string();
        if !s.sliders.contains_key(&key) { return; }
        let mut next = s.clone();
        next.sliders.remove(&key);
        self.commit_settings(next, "dock.undo.slider");
    }
    /// Doctrine rows for the ship (`mount == Bp.NONE`) or one mount: `Array[DoctrineEntryDict]`.
    /// Each row also carries `default: DoctrineEntryDict` (without `default`), the row it would be without its
    /// own override, so one row can be reverted by storing it; absent for a row that exists only as an override.
    #[func]
    fn doctrine(&self, mount: i64) -> Array<VarDictionary> {
        let mut out = Array::new();
        let Some(s) = self.settings() else { return out };
        let (rows, scope) = if mount < 0 {
            let weapons: Option<Vec<WeaponClass>> = self.mounts_now().map(|ms| { let mut w: Vec<WeaponClass> = ms.iter().map(|m| m.weapon).collect(); w.sort(); w.dedup(); w });
            (ship_doctrine(s, weapons.as_deref()), None)
        } else {
            let Some(slot) = crate::convert::slot_from_gd(mount) else { return out };
            let Some(w) = self.weapon_of(slot) else { return out };
            (mount_doctrine(s, slot, w), Some((slot, w)))
        };
        for (e, overridden) in rows {
            let mut dict = crate::convert::doctrine_entry_dict(&e, overridden);
            let base = if overridden { inherited_doctrine_row(s, scope, &e) } else { Some(e) };
            if let Some(b) = base { dict.set("default", &crate::convert::doctrine_entry_dict(&b, false)); }
            out.push(&dict);
        }
        out
    }
    /// Store one row (matched by weapon and target). Returns false for an invalid row.
    #[func]
    fn set_doctrine(&mut self, mount: i64, entry: VarDictionary) -> bool {
        // A salvo of 0 is the hold encoding (silent running, L485); the conversion keeps it.
        let Some(e) = crate::convert::doctrine_entry_from_dict(&entry) else { return false };
        let Some(s) = self.settings().cloned() else { return false };
        let target_ok = matches!((e.weapon, e.target), (_, TargetCategory::Surface | TargetCategory::Air | TargetCategory::InboundWeapon | TargetCategory::Subsurface));
        if !target_ok { return false; }
        let mount = if mount < 0 { None } else {
            let Some(slot) = crate::convert::slot_from_gd(mount) else { return false };
            let Some(w) = self.weapon_of(slot) else { return false };
            Some((slot, w))
        };
        let Some(next) = with_doctrine(&s, mount, e) else { return false };
        self.commit_settings(next, "dock.undo.doctrine")
    }
    /// Drop every override of the ship's or a mount's doctrine.
    #[func]
    fn reset_doctrine(&mut self, mount: i64) {
        let Some(s) = self.settings() else { return };
        let mut next = s.clone();
        if mount < 0 { next.doctrine.clear(); } else if let Some(slot) = crate::convert::slot_from_gd(mount) { next.mount_doctrine.remove(&slot); }
        self.commit_settings(next, "dock.undo.doctrine");
    }
    /// The rules table: `Array[RuleDict]`, top rule first.
    #[func]
    fn rules(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        let Some(m) = self.model.as_ref() else { return out };
        for (i, r) in m.blueprint.rules.iter().enumerate() {
            let valid = self.rule_check(r).is_empty();
            let mut dict = crate::convert::rule_dict(i, r, valid);
            // The sentence names parts of the design under edit, which may not be saved yet.
            let lk = crate::text::EngineLookup::new();
            if let Some(p) = sink_part(r.sink).filter(|p| p.index() < m.blueprint.parts.len()) {
                let cat = crate::services::services().catalogue();
                let mut msg = crate::convert::rule_msg(&lk, r);
                let sink = crate::text::fill(&lk.tr(&format!("units.sink.{}", sink_kind_name(r.sink))), &[part_name_text(&lk, &m.blueprint, p, &cat)]);
                if msg.args.len() >= 2 { msg.args[1] = MsgArg::Name(sink); }
                dict.set("text", &msg_dict(&msg));
            }
            out.push(&dict);
        }
        out
    }
    /// Insert a rule at `index` (`Bp.NONE` = append). Returns `Array[IssueDict]`; empty = stored.
    #[func]
    fn add_rule(&mut self, index: i64, rule: VarDictionary) -> Array<VarDictionary> {
        let Some(m) = self.model.as_ref() else { return issues_of(vec![Msg::new("dock.io.not_open")]) };
        let Some(r) = crate::convert::rule_from_dict(&rule) else { return issues_of(vec![Msg::new("dock.rule.unreadable")]) };
        if m.blueprint.rules.len() >= MAX_RULES {
            let mut a = Array::new();
            a.push(&crate::convert::issue_dict(&content::validate::Issue::TooManyRules(m.blueprint.rules.len() + 1)));
            return a;
        }
        let problems = self.rule_check(&r);
        if !problems.is_empty() { return issues_of(problems); }
        let mut rules = self.model.as_ref().map(|m| m.blueprint.rules.clone()).unwrap_or_default();
        let at = if index < 0 { rules.len() } else { (index as usize).min(rules.len()) };
        rules.insert(at, r);
        self.commit_rules(rules);
        Array::new()
    }
    /// Replace the rule at `index`. Returns `Array[IssueDict]`.
    #[func]
    fn set_rule(&mut self, index: i64, rule: VarDictionary) -> Array<VarDictionary> {
        let Some(m) = self.model.as_ref() else { return issues_of(vec![Msg::new("dock.io.not_open")]) };
        let Some(i) = usize::try_from(index).ok().filter(|i| *i < m.blueprint.rules.len()) else { return issues_of(vec![Msg::new("dock.rule.no_rule")]) };
        let Some(r) = crate::convert::rule_from_dict(&rule) else { return issues_of(vec![Msg::new("dock.rule.unreadable")]) };
        // A rule switched off may be stored as it is; a live one must work.
        if r.enabled {
            let problems = self.rule_check(&r);
            if !problems.is_empty() { return issues_of(problems); }
        }
        let mut rules = self.model.as_ref().map(|m| m.blueprint.rules.clone()).unwrap_or_default();
        rules[i] = r;
        self.commit_rules(rules);
        Array::new()
    }
    #[func]
    fn remove_rule(&mut self, index: i64) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        let Some(i) = usize::try_from(index).ok().filter(|i| *i < m.blueprint.rules.len()) else { return false };
        let mut rules = m.blueprint.rules.clone();
        rules.remove(i);
        self.commit_rules(rules)
    }
    /// Reorder: the rule nearer the top wins when two write the same sink.
    #[func]
    fn move_rule(&mut self, from: i64, to: i64) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        let n = m.blueprint.rules.len();
        let (Some(f), Ok(t)) = (usize::try_from(from).ok().filter(|f| *f < n), usize::try_from(to)) else { return false };
        let t = t.min(n.saturating_sub(1));
        if f == t { return true; }
        let mut rules = m.blueprint.rules.clone();
        let r = rules.remove(f);
        rules.insert(t, r);
        self.commit_rules(rules)
    }
    /// What a rule may be built from on this design, for the rule builder's pickers:
    /// `{ "operands": Array[{ "kind": StringName, "label": StringName, "unit": StringName, "needs": StringName }]`,
    /// `"sinks": Array[{ "sink": SinkDict, "label": MsgDict, "value_type": StringName }]`,
    /// `"comparisons": PackedInt32Array, "max_rules": int }`; `needs` is `&""`, `&"part"` or `&"filter"`.
    #[func]
    fn rule_vocabulary(&self) -> VarDictionary {
        let operands: VarArray = OPERANDS.iter().map(|(k, u, n)| d! {
            "kind" => StringName::from(*k), "label" => StringName::from(format!("dock.rule.operand.{k}").as_str()), "unit" => StringName::from(*u), "needs" => StringName::from(*n),
        }.to_variant()).collect();
        let comparisons: PackedInt32Array = content::graph::Cmp::VARIANTS.iter().map(|c| c.to_gd() as i32).collect();
        let mut sinks = VarArray::new();
        if let Some(m) = self.model.as_ref() {
            let cat = crate::services::services().catalogue();
            let mut list: Vec<SinkKind> = match self.generated_sinks() {
                Some(s) => s,
                None => {
                    let mut v = vec![SinkKind::HelmHeading, SinkKind::HelmSpeed, SinkKind::TrackStanceOverride, SinkKind::TrackIdOverride, SinkKind::ShipPreset, SinkKind::ShipEmcon];
                    for i in 0..m.blueprint.parts.len().min(u16::MAX as usize) {
                        let s = PartSlot(i as u16);
                        for k in [SinkKind::MountFirePermission(s), SinkKind::MountAimPoint(s), SinkKind::MissileLaunch(s), SinkKind::MissileTarget(s), SinkKind::EmissionState(s), SinkKind::PowerPriority(s), SinkKind::Pump(s), SinkKind::Door(s)] {
                            if sink_plausible(k, &m.blueprint, &cat) { v.push(k); }
                        }
                    }
                    v
                }
            };
            list.sort();
            list.dedup();
            for k in list {
                let name = sink_kind_name(k);
                let label = match k {
                    SinkKind::FloodValve(c) => Msg::new(&format!("units.sink.{name}")).arg(MsgArg::Compartment(c)),
                    _ => match sink_part(k) {
                        Some(p) => Msg::new(&format!("units.sink.{name}")).arg(MsgArg::Name(part_name_text(&crate::text::EngineLookup::new(), &m.blueprint, p, &cat))),
                        None => Msg::new(&format!("units.sink.{name}")),
                    },
                };
                sinks.push(&d! { "sink" => crate::convert::sink_dict(&k), "label" => msg_dict(&label), "value_type" => StringName::from(sink_value_type(k)) }.to_variant());
            }
        }
        d! { "operands" => operands, "sinks" => sinks, "comparisons" => comparisons, "max_rules" => MAX_RULES as i64 }
    }
    /// Power priorities per consumer: `Array[Dictionary]` with `slot: int`, `name: MsgDict`, `priority: int`
    /// (1 shed last .. 5), `default: int`, `overridden: bool`, `demand_kw: float`.
    #[func]
    fn power_priorities(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        let Some(m) = self.model.as_ref() else { return out };
        let cat = crate::services::services().catalogue();
        for (i, p) in m.blueprint.parts.iter().enumerate().take(u16::MAX as usize) {
            let Some(def) = cat.part(&p.part) else { continue };
            let default = sim_systems::officers::engineer::default_priority(def);
            if default == 0 { continue; }
            let slot = PartSlot(i as u16);
            let stored = m.blueprint.officers.power_priority.get(&slot).copied().filter(|v| (1..=5).contains(v));
            let demand: f64 = def.ports.iter().filter(|q| q.network == content::enums::NetworkKind::Power && q.role == content::part::PortRole::Consume).map(|q| q.rate).filter(|r| r.is_finite()).sum();
            out.push(&d! {
                "slot" => slot.0 as i64, "name" => msg_dict(&part_name_msg(&m.blueprint, slot, &cat)), "priority" => stored.unwrap_or(default) as i64,
                "default" => default as i64, "overridden" => stored.is_some(), "demand_kw" => demand.max(0.0),
            });
        }
        out
    }
    /// `priority` 1..5, or `Bp.NONE` to return to the default table.
    #[func]
    fn set_power_priority(&mut self, slot: i64, priority: i64) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        let Some(s) = crate::convert::slot_from_gd(slot) else { return false };
        let cat = crate::services::services().catalogue();
        let Some(def) = def_of(&m.blueprint, s, &cat) else { return false };
        let default = sim_systems::officers::engineer::default_priority(def);
        if default == 0 { return false; }
        let mut next = m.blueprint.officers.clone();
        if priority < 0 || priority as u8 == default && (1..=5).contains(&priority) {
            next.power_priority.remove(&s);
        } else if (1..=5).contains(&priority) {
            next.power_priority.insert(s, priority as u8);
        } else {
            return false;
        }
        self.commit_settings(next, "dock.undo.power_priority")
    }
    /// Training and elevation limits of a mount: `{ "train_min_deg": float, "train_max_deg": float,
    /// "elevation_min_deg": float, "elevation_max_deg": float, "auto": bool }`.
    #[func]
    fn mount_limits(&self, slot: i64) -> VarDictionary {
        let Some(m) = self.model.as_ref() else { return VarDictionary::new() };
        let Some(s) = crate::convert::slot_from_gd(slot) else { return VarDictionary::new() };
        let cat = crate::services::services().catalogue();
        let Some(def) = def_of(&m.blueprint, s, &cat) else { return VarDictionary::new() };
        let Some(mech) = mechanical_limits(def) else { return VarDictionary::new() };
        let dict = |l: &MountLimits, auto: bool| d! { "train_min_deg" => l.train_min_deg, "train_max_deg" => l.train_max_deg, "elevation_min_deg" => l.elevation_min_deg, "elevation_max_deg" => l.elevation_max_deg, "auto" => auto };
        if let Some(l) = m.blueprint.officers.mount_limits.get(&s) { return dict(l, false); }
        // In force: the design's own sweep, when the newest built design has these parts.
        let swept = self.mounts_now().and_then(|ms| ms.into_iter().find(|mi| mi.slot == s).map(|mi| mi.limits));
        dict(&swept.unwrap_or(mech), true)
    }
    /// Override the limits (same keys without `auto`); an empty Dictionary returns them to auto-set.
    #[func]
    fn set_mount_limits(&mut self, slot: i64, limits: VarDictionary) -> bool {
        let Some(m) = self.model.as_ref() else { return false };
        let Some(s) = crate::convert::slot_from_gd(slot) else { return false };
        let cat = crate::services::services().catalogue();
        let Some(def) = def_of(&m.blueprint, s, &cat) else { return false };
        let Some(mech) = mechanical_limits(def) else { return false };
        let mut next = m.blueprint.officers.clone();
        if limits.is_empty() {
            next.mount_limits.remove(&s);
        } else {
            let cur = m.blueprint.officers.mount_limits.get(&s).copied().or_else(|| self.mounts_now().and_then(|ms| ms.into_iter().find(|mi| mi.slot == s).map(|mi| mi.limits))).unwrap_or(mech);
            let want = MountLimits {
                train_min_deg: get_f64(&limits, "train_min_deg").unwrap_or(cur.train_min_deg),
                train_max_deg: get_f64(&limits, "train_max_deg").unwrap_or(cur.train_max_deg),
                elevation_min_deg: get_f64(&limits, "elevation_min_deg").unwrap_or(cur.elevation_min_deg),
                elevation_max_deg: get_f64(&limits, "elevation_max_deg").unwrap_or(cur.elevation_max_deg),
            };
            let Some(l) = clamp_limits(want, &mech) else { return false };
            next.mount_limits.insert(s, l);
        }
        self.commit_settings(next, "dock.undo.mount_limits")
    }
    /// Emission settings: `{ "emcon": int, "burst_trigger_range_m": float, "self_defence_range_m": float,
    /// "burst_auto": bool, "self_defence_auto": bool, "emcon_auto": bool, "emcon_default": int }`.
    /// `emcon_auto` is true while the EMCON setting equals the ship preset's, `emcon_default`.
    #[func]
    fn emission_settings(&self) -> VarDictionary {
        let cat = crate::services::services().catalogue();
        let default_settings = OfficerSettings::default();
        let s = self.settings().unwrap_or(&default_settings);
        let (bd, _, _) = burst_spec(&cat);
        let (sd, _, _) = self_defence_spec(&cat);
        d! {
            "emcon" => s.emcon.to_gd(),
            "burst_trigger_range_m" => s.burst_trigger_range_m.filter(|v| v.is_finite()).unwrap_or(bd),
            "self_defence_range_m" => s.self_defence_range_m.filter(|v| v.is_finite()).unwrap_or(sd),
            "burst_auto" => s.burst_trigger_range_m.is_none(),
            "self_defence_auto" => s.self_defence_range_m.is_none(),
            "emcon_auto" => s.emcon == tuning::preset(s.preset).emcon,
            "emcon_default" => tuning::preset(s.preset).emcon.to_gd(),
        }
    }
    /// Same keys; a negative range returns that value to its default.
    #[func]
    fn set_emission_settings(&mut self, settings: VarDictionary) -> bool {
        let Some(s) = self.settings().cloned() else { return false };
        let cat = crate::services::services().catalogue();
        let mut next = s.clone();
        if let Some(e) = get_i64(&settings, "emcon") {
            let Some(e) = Emcon::from_gd(e) else { return false };
            next.emcon = e;
        }
        if let Some(v) = get_f64(&settings, "burst_trigger_range_m") { next.burst_trigger_range_m = range_setting(v, burst_spec(&cat)); }
        if let Some(v) = get_f64(&settings, "self_defence_range_m") { next.self_defence_range_m = range_setting(v, self_defence_spec(&cat)); }
        self.commit_settings(next, "dock.undo.emission")
    }
    /// Repair order as `Bp.REPAIR_*` values, most urgent first.
    #[func]
    fn repair_order(&self) -> PackedInt32Array {
        let default_settings = OfficerSettings::default();
        repair_order_of(self.settings().unwrap_or(&default_settings)).iter().map(|p| p.to_gd() as i32).collect()
    }
    /// True when the design stores its own repair order (hand glyph); false while the default list stands.
    #[func]
    fn repair_order_overridden(&self) -> bool { self.settings().is_some_and(|s| !s.repair_order.is_empty() && repair_order_of(s) != RepairPriority::DEFAULT_ORDER) }
    #[func]
    fn set_repair_order(&mut self, order: PackedInt32Array) -> bool {
        let Some(s) = self.settings().cloned() else { return false };
        let Some(list) = order.as_slice().iter().map(|&v| RepairPriority::from_gd(v as i64)).collect::<Option<Vec<_>>>() else { return false };
        let Some(next) = with_repair_order(&s, &list) else { return false };
        self.commit_settings(next, "dock.undo.repair_order")
    }
    /// Highest layer in use per officer and overall, and the ruleset's ceiling:
    /// `{ "max_layer": int, "in_use": int, "by_officer": PackedInt32Array (5, Bp.OFFICER_* order),
    /// "overrides": int, "legal": bool }` (`legal` false when the design uses a layer the ruleset bans).
    #[func]
    fn layers(&self) -> VarDictionary {
        let Some(m) = self.model.as_ref() else {
            return d! { "max_layer" => ControlLayer::L4Script.to_gd(), "in_use" => ControlLayer::L0Doctrine.to_gd(), "by_officer" => PackedInt32Array::from(&[0i32; 5][..]), "overrides" => 0i64, "legal" => true };
        };
        let cat = crate::services::services().catalogue();
        let compiled = self.graph_layers();
        let r = layer_report(&m.blueprint, &cat, compiled.as_ref());
        let max = super::max_layer(&m.context.ruleset);
        let banned = layers_banned(&m.blueprint, &cat, &m.context.ruleset);
        let by: PackedInt32Array = r.by_officer.iter().map(|l| l.to_gd() as i32).collect();
        d! { "max_layer" => max.to_gd(), "in_use" => r.in_use.to_gd(), "by_officer" => by, "overrides" => r.overrides as i64, "legal" => !banned && r.in_use <= max }
    }
    /// Which auto-set values are automatic in this build (L200-212): `Array[Dictionary]` with
    /// `value: StringName` (`&"conduit_routes"`, `&"power_priorities"`, `&"mount_limits"`, `&"kit_wiring"`,
    /// `&"solver_tables"`), `auto: bool`, `reason: MsgDict` (the failing case when it dropped to Suggest).
    #[func]
    fn auto_set_status(&self) -> Array<VarDictionary> {
        use sim_systems::editor::suggest::AutoSetValue as V;
        let status = sim_systems::editor::suggest::autoset_status();
        let mut out = Array::new();
        for v in V::ALL {
            let name = match v { V::ConduitRoutes => "conduit_routes", V::PowerPriorities => "power_priorities", V::MountLimits => "mount_limits", V::KitWiring => "kit_wiring", V::SolverTables => "solver_tables" };
            let reason = status.dropped.iter().find(|(d, _)| *d == v).map_or_else(empty_msg, |(_, case)| msg_dict(&Msg::new("dock.autoset.dropped").arg(MsgArg::Name(case.chars().take(120).collect()))));
            out.push(&d! { "value" => StringName::from(name), "auto" => status.is_auto(v), "reason" => reason });
        }
        out
    }
    /// Return every officer setting and rule to stock defaults (one undo step).
    #[func]
    fn reset_control(&mut self) {
        let Some(m) = self.model.as_ref() else { return };
        let mut bp = m.blueprint.clone();
        bp.officers = OfficerSettings::default();
        bp.rules.clear();
        if self.commit(bp, "dock.undo.reset_control") {
            self.base_mut().emit_signal("control_changed", &[]);
            self.base_mut().emit_signal("graph_changed", &[]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::enums::{IdRung, TrackGrade};
    use content::graph::{Cmp, TrackFilter};

    fn fac() -> Blueprint { content::campaign::load::stock_blueprint("starters/fac22.bp.ron").unwrap() }

    #[test]
    fn a_preset_carries_an_automatic_emcon_along() {
        let s = OfficerSettings::default();
        let a = with_ship_preset(&s, DoctrinePreset::Aggressive);
        assert_eq!(a.emcon, Emcon::Active);
        let silent = with_ship_preset(&a, DoctrinePreset::SilentRunning);
        assert_eq!(silent.emcon, Emcon::Silent);
        // A hand-set EMCON stays.
        let mut hand = s.clone();
        hand.emcon = Emcon::Active;
        assert_eq!(with_ship_preset(&hand, DoctrinePreset::SilentRunning).emcon, Emcon::Active);
        // The layer report agrees: an automatic EMCON is not an L1 override.
        let mut bp = fac();
        bp.officers = silent;
        let r = layer_report(&bp, content::catalogue::catalogue(), None);
        assert_eq!(r.by_officer[role_index(OfficerRole::Sensors)], ControlLayer::L0Doctrine);
    }

    #[test]
    fn sliders_step_clamp_and_drop_overrides_equal_to_the_preset() {
        let s = OfficerSettings::default();
        let (n, v) = with_slider(&s, "gunnery.aggression", 0.733).unwrap();
        assert!((v - 0.73).abs() < 1e-9);
        assert!(n.sliders.contains_key("gunnery.aggression"));
        let (n2, v2) = with_slider(&n, "gunnery.aggression", 7.0).unwrap();
        assert_eq!(v2, 1.0);
        // Back to the cautious preset's 0.5: the A chip returns.
        let (n3, _) = with_slider(&n2, "gunnery.aggression", 0.5).unwrap();
        assert!(!n3.sliders.contains_key("gunnery.aggression"));
        // Under the aggressive preset the base is 0.85.
        let agg = with_ship_preset(&s, DoctrinePreset::Aggressive);
        assert!((slider_base(&agg, "gunnery.aggression") - 0.85).abs() < 1e-9);
        assert!(with_slider(&s, "no.such.slider", 1.0).is_none());
        assert!(with_slider(&s, "gunnery.aggression", f64::NAN).is_none());
        let (_, r) = with_slider(&s, "helm.station_radius_m", 123.4).unwrap();
        assert!((r - 120.0).abs() < 1e-9, "the 20 to 500 m slider steps by 10 m: {r}");
    }

    #[test]
    fn doctrine_inherits_and_refuses_neutral_targets() {
        let s = OfficerSettings::default();
        let rows = ship_doctrine(&s, None);
        assert!(rows.iter().all(|(_, o)| !o));
        let gun = rows.iter().find(|(e, _)| e.weapon == WeaponClass::Gun && e.target == TargetCategory::Surface).unwrap().0;
        let mut hold = gun;
        hold.salvo = 0;
        let n = with_doctrine(&s, None, hold).unwrap();
        assert_eq!(n.doctrine, vec![hold]);
        // Setting it back to the inherited row removes the override.
        assert!(with_doctrine(&n, None, gun).unwrap().doctrine.is_empty());
        let mut neutral = gun;
        neutral.min_stance = Stance::Neutral;
        assert!(with_doctrine(&s, None, neutral).is_none());
        // A mount row inherits the ship row and its own preset.
        let slot = PartSlot(3);
        let m = mount_doctrine(&n, slot, WeaponClass::Gun);
        assert_eq!(m.iter().find(|(e, _)| e.target == TargetCategory::Surface).unwrap().0.salvo, 0);
        let mut fine = gun;
        fine.min_grade = TrackGrade::Lock;
        let n2 = with_doctrine(&n, Some((slot, WeaponClass::Gun)), fine).unwrap();
        assert_eq!(n2.mount_doctrine[&slot], vec![fine]);
        assert!(with_doctrine(&n, Some((slot, WeaponClass::Sam)), fine).is_none(), "a row of another weapon class");
        let mut silent = s.clone();
        silent.mount_presets.insert(slot, DoctrinePreset::SilentRunning);
        assert!(mount_doctrine(&silent, slot, WeaponClass::Gun).iter().all(|(e, _)| e.salvo == 0));
        // The mount's preset sits over the ship's own entries, as the doctrine stage resolves them.
        let mut both = n.clone();
        both.mount_presets.insert(slot, DoctrinePreset::Aggressive);
        let agg = sim_systems::doctrine::default_entries(DoctrinePreset::Aggressive).into_iter().find(|e| e.weapon == WeaponClass::Gun && e.target == TargetCategory::Surface).unwrap();
        assert_eq!(mount_doctrine(&both, slot, WeaponClass::Gun).iter().find(|(e, _)| e.target == TargetCategory::Surface).unwrap().0, agg);
        let _ = IdRung::Unknown;
    }

    #[test]
    fn a_reverted_row_is_the_inherited_row() {
        let s = OfficerSettings::default();
        let gun = ship_doctrine(&s, None).into_iter().find(|(e, _)| e.weapon == WeaponClass::Gun && e.target == TargetCategory::Surface).unwrap().0;
        let mut hold = gun;
        hold.salvo = 0;
        let n = with_doctrine(&s, None, hold).unwrap();
        let base = inherited_doctrine_row(&n, None, &hold).unwrap();
        assert_eq!(base, gun);
        // Storing the inherited row removes the override.
        assert!(with_doctrine(&n, None, base).unwrap().doctrine.is_empty());
        // A mount row inherits the ship's override, not the preset's row.
        let slot = PartSlot(2);
        let mut fine = gun;
        fine.min_grade = TrackGrade::Lock;
        let m = with_doctrine(&n, Some((slot, WeaponClass::Gun)), fine).unwrap();
        assert_eq!(inherited_doctrine_row(&m, Some((slot, WeaponClass::Gun)), &fine), Some(hold));
        assert!(with_doctrine(&m, Some((slot, WeaponClass::Gun)), hold).unwrap().mount_doctrine.is_empty());
    }

    #[test]
    fn the_default_repair_order_is_the_bp_order() {
        // The control panel reverts the repair order by storing `Bp.REPAIR_*` in ascending order.
        let gd: Vec<i64> = RepairPriority::DEFAULT_ORDER.iter().map(|p| p.to_gd()).collect();
        assert_eq!(gd, (0..6).collect::<Vec<i64>>());
    }

    #[test]
    fn repair_orders_are_permutations() {
        let s = OfficerSettings::default();
        let mut order = RepairPriority::DEFAULT_ORDER.to_vec();
        order.swap(0, 5);
        let n = with_repair_order(&s, &order).unwrap();
        assert_eq!(repair_order_of(&n), order);
        assert!(with_repair_order(&n, &RepairPriority::DEFAULT_ORDER).unwrap().repair_order.is_empty());
        assert!(with_repair_order(&s, &order[..5]).is_none());
        let mut dup = order.clone();
        dup[1] = dup[0];
        assert!(with_repair_order(&s, &dup).is_none());
    }

    #[test]
    fn limits_clamp_to_the_mechanism() {
        let mech = MountLimits { train_min_deg: -180.0, train_max_deg: 180.0, elevation_min_deg: -10.0, elevation_max_deg: 85.0 };
        let l = clamp_limits(MountLimits { train_min_deg: -200.0, train_max_deg: 150.0, elevation_min_deg: -20.0, elevation_max_deg: 60.0 }, &mech).unwrap();
        assert_eq!((l.train_min_deg, l.train_max_deg, l.elevation_min_deg, l.elevation_max_deg), (-180.0, 150.0, -10.0, 60.0));
        assert!(clamp_limits(MountLimits { train_min_deg: 50.0, train_max_deg: 10.0, ..mech }, &mech).is_none());
        assert!(clamp_limits(MountLimits { train_min_deg: f64::NAN, ..mech }, &mech).is_none());
        assert_eq!(range_setting(-1.0, (20_000.0, 2_000.0, 60_000.0)), None);
        assert_eq!(range_setting(20_000.0, (20_000.0, 2_000.0, 60_000.0)), None);
        assert_eq!(range_setting(100_000.0, (20_000.0, 2_000.0, 60_000.0)), Some(60_000.0));
    }

    #[test]
    fn rules_must_fit_their_sink() {
        let near = RuleValue::NearestContact(TrackFilter::default());
        assert!(value_fits_sink(SinkKind::MountFirePermission(PartSlot(1)), &near));
        assert!(!value_fits_sink(SinkKind::HelmSpeed, &near));
        assert!(value_fits_sink(SinkKind::HelmSpeed, &RuleValue::Set(Value::Scalar(20.0))));
        assert!(value_fits_sink(SinkKind::HelmHeading, &RuleValue::Set(Value::Scalar(40.0))));
        assert!(!value_fits_sink(SinkKind::HelmSpeed, &RuleValue::Set(Value::Scalar(f64::NAN))));
        assert!(value_fits_sink(SinkKind::ShipPreset, &RuleValue::Set(Value::Preset(DoctrinePreset::Aggressive))));
        assert!(!value_fits_sink(SinkKind::HelmSpeed, &RuleValue::Set(Value::Preset(DoctrinePreset::Aggressive))));
        assert!(value_fits_sink(SinkKind::ShipEmcon, &RuleValue::Set(Value::Emcon(Emcon::Silent))));
        assert!(!value_fits_sink(SinkKind::ShipEmcon, &RuleValue::Set(Value::Bool(true))));
        assert!(value_fits_sink(SinkKind::TrackIdOverride, &RuleValue::Set(Value::Rung(IdRung::Class))));
        assert!(value_fits_sink(SinkKind::TrackStanceOverride, &RuleValue::Set(Value::Stance(Stance::Hostile))));
        assert!(value_fits_sink(SinkKind::Pump(PartSlot(0)), &RuleValue::Set(Value::Bool(true))));
        assert!(value_fits_sink(SinkKind::PowerPriority(PartSlot(0)), &RuleValue::Set(Value::Scalar(2.0))));
        assert!(!value_fits_sink(SinkKind::PowerPriority(PartSlot(0)), &RuleValue::Set(Value::Scalar(9.0))));
        assert!(!value_fits_sink(SinkKind::MountAimPoint(PartSlot(0)), &RuleValue::Set(Value::Text("x".into()))));
    }

    #[test]
    fn an_accepted_rule_compiles_clean() {
        let lk = crate::text::tests::CsvLookup::load();
        let cat = content::catalogue::catalogue();
        let bp = fac();
        let ruleset = content::ruleset::Ruleset::campaign(content::enums::Act::Act1);
        let prefab = std::sync::Arc::new(sim_core::vessel::build_prefab(&bp, cat));
        let design = sim_systems::ship::ShipDesign::build(&bp, prefab, cat, &ruleset);
        let gun = design.mounts.iter().find(|m| m.weapon == WeaponClass::Gun).expect("a gun").slot;
        let rule = Rule { when: Condition::Compare { lhs: Operand::NearestContactRangeM(TrackFilter::default()), cmp: Cmp::Lt, rhs: Operand::Const(4_000.0) }, sink: SinkKind::MountFirePermission(gun), value: RuleValue::NearestContact(TrackFilter::default()), enabled: true };
        let generated = sim_systems::graph::generate::generate(&sim_systems::ship::DesignFacts { blueprint: &bp, prefab: &design.prefab, catalogue: cat, mounts: &design.mounts, emitters: &design.emitters, sensor_slots: &design.sensor_slots, consumer_slots: &design.consumer_slots, decoys: &design.decoys, pumps: &design.pumps, doors: &design.doors });
        let has = generated.nodes.iter().any(|n| n.id == sim_systems::graph::generate::sink_node(rule.sink));
        assert!(rule_problems(&rule, &bp, cat, Some(has), &lk).is_empty());
        let g = design.recompile(&bp.officers, std::slice::from_ref(&rule), &bp.graph, cat, &ruleset);
        assert!(!g.issues.iter().any(|i| matches!(i, sim_systems::graph::GraphIssue::RuleSinkMissing { .. } | sim_systems::graph::GraphIssue::TypeMismatch { .. })), "{:?}", g.issues);
        // A sink the design lacks is refused with a sentence.
        let bad = Rule { sink: SinkKind::Pump(PartSlot(9_999)), value: RuleValue::Set(Value::Bool(true)), ..rule.clone() };
        let p = rule_problems(&bad, &bp, cat, None, &lk);
        assert_eq!(p.first().map(|m| m.key.as_str()), Some("dock.rule.no_sink"));
        let reads_missing = Rule { when: Condition::Compare { lhs: Operand::PartHealth(PartSlot(9_999)), cmp: Cmp::Lt, rhs: Operand::Const(0.5) }, ..rule };
        assert!(rule_problems(&reads_missing, &bp, cat, Some(true), &lk).iter().any(|m| m.key.as_str() == "dock.rule.no_part"));
    }

    #[test]
    fn layers_count_what_the_design_uses() {
        let cat = content::catalogue::catalogue();
        let mut bp = fac();
        let r = layer_report(&bp, cat, None);
        assert_eq!(r.in_use, ControlLayer::L0Doctrine);
        assert_eq!(r.overrides, 0);
        bp.officers.sliders.insert("helm.evasion".into(), 0.9);
        bp.rules.push(Rule { when: Condition::All(vec![]), sink: SinkKind::HelmSpeed, value: RuleValue::Set(Value::Scalar(10.0)), enabled: true });
        let r = layer_report(&bp, cat, None);
        assert_eq!(r.by_officer[role_index(OfficerRole::Helm)], ControlLayer::L2Rules);
        assert_eq!(r.in_use, ControlLayer::L2Rules);
        assert_eq!(r.overrides, 2);
        bp.graph.scripts.push(content::graph::ScriptSource { name: "s".into(), source: String::new(), inputs: vec![], outputs: vec![], enabled: false });
        assert_eq!(layer_report(&bp, cat, None).in_use, ControlLayer::L4Script);
    }

    #[test]
    fn the_layer_ban_agrees_with_validation() {
        let cat = content::catalogue::catalogue();
        let standard = content::ruleset::Ruleset::default();
        let open = content::ruleset::Ruleset { kind: content::ruleset::RulesetKind::Open, ..Default::default() };
        let banned_by_validation = |bp: &Blueprint, r: &content::ruleset::Ruleset| {
            let ctx = content::validate::ValidationContext { ruleset: r, available: None, enemy_class: false, complexity: None };
            content::validate::validate_blueprint(bp, cat, &ctx).issues.iter().any(|i| matches!(i, content::validate::Issue::LayerNotAllowed | content::validate::Issue::ScriptsNotAllowed))
        };
        let mut bp = fac();
        for r in [&standard, &open] { assert_eq!(layers_banned(&bp, cat, r), banned_by_validation(&bp, r)); }
        bp.graph.pinned.nodes.push(content::graph::GraphNode { id: content::ids::GraphNodeId(content::graph::USER_NODE_BASE), kind: content::graph::NodeKind::Source(content::graph::SourceKind::OwnShip), params: Default::default(), pos: [0.0, 0.0] });
        for r in [&standard, &open] { assert_eq!(layers_banned(&bp, cat, r), banned_by_validation(&bp, r), "{:?}", r.kind); }
        assert!(layers_banned(&bp, cat, &standard) && !layers_banned(&bp, cat, &open));
        bp.graph.scripts.push(content::graph::ScriptSource { name: "s".into(), source: String::new(), inputs: vec![], outputs: vec![], enabled: false });
        assert_eq!(layers_banned(&bp, cat, &open), banned_by_validation(&bp, &open));
    }

    #[test]
    fn part_names_follow_the_design_under_edit() {
        let lk = crate::text::tests::CsvLookup::load();
        let cat = content::catalogue::catalogue();
        let mut bp = fac();
        let gun = bp.parts.iter().position(|p| cat.part(&p.part).is_some_and(|d| d.category == content::part::PartCategory::Gun)).unwrap();
        let text = part_name_text(&lk, &bp, PartSlot(gun as u16), cat);
        assert!(text.starts_with("Gun") && text.ends_with('1'), "{text}");
        bp.parts[gun].label = Some("Forward gun".into());
        assert_eq!(part_name_text(&lk, &bp, PartSlot(gun as u16), cat), "Forward gun");
    }
}
