//! Unit `sys-officer-tuning`. The lower rungs of the control ladder: L0 doctrine presets, the L1 slider
//! registry, and override bookkeeping (A chip, hand glyph, diff, revert). DESIGN.md L322-329, L351, L210
//! (Suggest: L1 Aggression slider).
//!
//! L0: presets per ship or per mount: `aggressive`, `cautious`, `silent running`. A preset is a named set of
//! slider values, doctrine entries and an emission setting. L1: 3 to 6 named sliders per behaviour, with the
//! full parameter set (every stage parameter of `graph::library`) under an expander. Slider keys are stable
//! identifiers stored in `OfficerSettings::sliders`, for example `gunnery.aggression`, `helm.evasion`.
//!
//! # L0 preset table
//! | Preset | Sliders | EMCON | Doctrine |
//! | aggressive | aggression 0.85, open fire at full effective range, evasion 0.3, cruise 0.9, bursts every 15 s | Active | `doctrine::default_entries(Aggressive)` |
//! | cautious (default) | every slider at its default | Burst | `doctrine::default_entries(Cautious)` |
//! | silent running | aggression 0.2, open fire at 0.8 of effective range, evasion 0.7, cruise 0.5 | Silent | `doctrine::default_entries(SilentRunning)` |
//!
//! # Slider to stage parameter mapping
//! Stage parameter keys are the slider keys (see `graph::library`), so every `drives` entry maps the slider
//! range onto the same range: the stage sees the slider value unchanged. Two settings that are not sliders
//! also bake into stage parameters: `OfficerSettings::burst_trigger_range_m` into the emission controller's
//! `sensors.burst_trigger_range_m` and `self_defence_range_m` into threat ranking's
//! `gunnery.self_defence_range_m`.
//!
//! Settings, sliders and doctrine changed by a command or a revert take effect when the caller recompiles the
//! ship's graph (`ShipDesign::recompile` through `graph::compile::compile_with`), as after any L0 to L2 command.
use crate::prelude::*;
use crate::protocol::command::OverrideKey;
use crate::protocol::event::OverrideView;
use crate::ship::{Ship, ShipDesign};
use crate::officers::damage_control::DcState;
use content::graph::{DoctrineEntry, OfficerSettings, OfficerStage, RepairPriority, SinkKind, Value, WeaponClass};

/// One L1 slider.
#[derive(Clone, Debug, PartialEq)]
pub struct SliderDef {
    pub key: &'static str,
    pub role: OfficerRole,
    /// Translation key of the label (`tuning.slider.<key>`).
    pub label: &'static str,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    /// Stage parameters this slider drives: (stage, parameter key, value at min, value at max).
    pub drives: &'static [(OfficerStage, &'static str, f64, f64)],
    /// True for the 3 to 6 shown by default; false for the expander.
    pub primary: bool,
}

const fn sl(key: &'static str, role: OfficerRole, label: &'static str, min: f64, max: f64, default: f64, drives: &'static [(OfficerStage, &'static str, f64, f64)], primary: bool) -> SliderDef {
    SliderDef { key, role, label, min, max, default, drives, primary }
}

use OfficerRole as R;
use OfficerStage as S;
static SLIDERS: [SliderDef; 20] = [
    // Helm.
    sl("helm.evasion", R::Helm, "tuning.slider.helm.evasion", 0.0, 1.0, 0.5, &[(S::Evasion, "helm.evasion", 0.0, 1.0)], true),
    sl("helm.responsiveness", R::Helm, "tuning.slider.helm.responsiveness", 0.0, 1.0, 0.5, &[(S::HelmAutopilot, "helm.responsiveness", 0.0, 1.0), (S::StationKeeping, "helm.responsiveness", 0.0, 1.0)], true),
    sl("helm.cruise_speed", R::Helm, "tuning.slider.helm.cruise_speed", 0.3, 1.0, 0.7, &[(S::HelmAutopilot, "helm.cruise_speed", 0.3, 1.0)], true),
    sl("helm.lookahead", R::Helm, "tuning.slider.helm.lookahead", 1.0, 4.0, 1.5, &[(S::HelmAutopilot, "helm.lookahead", 1.0, 4.0)], false),
    sl("helm.station_radius_m", R::Helm, "tuning.slider.helm.station_radius_m", 20.0, 500.0, 100.0, &[(S::StationKeeping, "helm.station_radius_m", 20.0, 500.0)], false),
    // Gunnery.
    sl("gunnery.aggression", R::Gunnery, "tuning.slider.gunnery.aggression", 0.0, 1.0, 0.5, &[(S::ThreatRanking, "gunnery.aggression", 0.0, 1.0), (S::MountAssignment, "gunnery.aggression", 0.0, 1.0)], true),
    sl("gunnery.open_fire_range", R::Gunnery, "tuning.slider.gunnery.open_fire_range", 0.5, 1.0, 1.0, &[(S::MountAssignment, "gunnery.open_fire_range", 0.5, 1.0)], true),
    sl("gunnery.pn_surface", R::Gunnery, "tuning.slider.gunnery.pn_surface", 2.0, 6.0, 3.0, &[(S::PnGuidance, "gunnery.pn_surface", 2.0, 6.0)], true),
    sl("gunnery.pn_air", R::Gunnery, "tuning.slider.gunnery.pn_air", 2.0, 6.0, 4.0, &[(S::PnGuidance, "gunnery.pn_air", 2.0, 6.0)], true),
    sl("gunnery.iteration_cap", R::Gunnery, "tuning.slider.gunnery.iteration_cap", 1.0, 10.0, 6.0, &[(S::BallisticSolver, "gunnery.iteration_cap", 1.0, 10.0)], false),
    sl("gunnery.converge_m", R::Gunnery, "tuning.slider.gunnery.converge_m", 0.05, 2.0, 0.25, &[(S::BallisticSolver, "gunnery.converge_m", 0.05, 2.0)], false),
    // Sensors (the EMCON setting itself is `OfficerSettings::emcon`, a three-way L1 control).
    sl("sensors.burst_rotations", R::Sensors, "tuning.slider.sensors.burst_rotations", 1.0, 6.0, 3.0, &[(S::EmissionController, "sensors.burst_rotations", 1.0, 6.0)], true),
    sl("sensors.burst_repeat_s", R::Sensors, "tuning.slider.sensors.burst_repeat_s", 10.0, 60.0, 30.0, &[(S::EmissionController, "sensors.burst_repeat_s", 10.0, 60.0)], true),
    sl("sensors.pd_radar_range_m", R::Sensors, "tuning.slider.sensors.pd_radar_range_m", 2_000.0, 12_000.0, 8_000.0, &[(S::EmissionController, "sensors.pd_radar_range_m", 2_000.0, 12_000.0)], true),
    // Engineer.
    sl("engineer.reserve_margin", R::Engineer, "tuning.slider.engineer.reserve_margin", 0.0, 0.3, 0.05, &[(S::LoadShedder, "engineer.reserve_margin", 0.0, 0.3)], true),
    sl("engineer.restore_delay_s", R::Engineer, "tuning.slider.engineer.restore_delay_s", 0.0, 10.0, 2.0, &[(S::LoadShedder, "engineer.restore_delay_s", 0.0, 10.0)], true),
    sl("engineer.fuel_reserve", R::Engineer, "tuning.slider.engineer.fuel_reserve", 0.0, 0.5, 0.1, &[(S::LoadShedder, "engineer.fuel_reserve", 0.0, 0.5)], true),
    // Damage control.
    sl("dc.pump_threshold_m", R::DamageControl, "tuning.slider.dc.pump_threshold_m", 0.0, 0.5, 0.05, &[(S::DamageControlPlanner, "dc.pump_threshold_m", 0.0, 0.5)], true),
    sl("dc.door_discipline", R::DamageControl, "tuning.slider.dc.door_discipline", 0.0, 1.0, 0.5, &[(S::DamageControlPlanner, "dc.door_discipline", 0.0, 1.0)], true),
    // Seconds of cook-off delay left when the officer floods a burning magazine (60 s by default, L719).
    sl("dc.magazine_flood_s", R::DamageControl, "tuning.slider.dc.magazine_flood_s", 30.0, 120.0, 60.0, &[(S::DamageControlPlanner, "dc.magazine_flood_s", 30.0, 120.0)], true),
];

/// The slider registry: 3 to 6 primary sliders per officer. Unit: `sys-officer-tuning`.
pub fn sliders() -> &'static [SliderDef] { &SLIDERS }
pub fn slider(key: &str) -> Option<&'static SliderDef> { sliders().iter().find(|s| s.key == key) }

/// What a preset sets.
#[derive(Clone, Debug, PartialEq)]
pub struct PresetDef { pub preset: DoctrinePreset, pub name: &'static str, pub description: &'static str, pub sliders: Vec<(&'static str, f64)>, pub emcon: Emcon, pub doctrine: Vec<DoctrineEntry> }

/// Slider values a preset sets (the table in the module docs).
pub(crate) fn preset_sliders(preset: DoctrinePreset) -> &'static [(&'static str, f64)] {
    match preset {
        DoctrinePreset::Aggressive => &[("gunnery.aggression", 0.85), ("gunnery.open_fire_range", 1.0), ("helm.evasion", 0.3), ("helm.cruise_speed", 0.9), ("sensors.burst_repeat_s", 15.0)],
        DoctrinePreset::Cautious => &[("gunnery.aggression", 0.5), ("gunnery.open_fire_range", 1.0), ("helm.evasion", 0.5), ("helm.cruise_speed", 0.7)],
        DoctrinePreset::SilentRunning => &[("gunnery.aggression", 0.2), ("gunnery.open_fire_range", 0.8), ("helm.evasion", 0.7), ("helm.cruise_speed", 0.5)],
    }
}
/// Emission setting a preset sets.
pub(crate) fn preset_emcon(preset: DoctrinePreset) -> Emcon {
    match preset { DoctrinePreset::Aggressive => Emcon::Active, DoctrinePreset::Cautious => Emcon::Burst, DoctrinePreset::SilentRunning => Emcon::Silent }
}
fn preset_keys(preset: DoctrinePreset) -> (&'static str, &'static str) {
    match preset {
        DoctrinePreset::Aggressive => ("tuning.preset.aggressive", "tuning.preset.aggressive.description"),
        DoctrinePreset::Cautious => ("tuning.preset.cautious", "tuning.preset.cautious.description"),
        DoctrinePreset::SilentRunning => ("tuning.preset.silent_running", "tuning.preset.silent_running.description"),
    }
}

/// The three presets. Unit: `sys-officer-tuning`.
pub fn preset(preset: DoctrinePreset) -> PresetDef {
    let (name, description) = preset_keys(preset);
    PresetDef { preset, name, description, sliders: preset_sliders(preset).to_vec(), emcon: preset_emcon(preset), doctrine: crate::doctrine::default_entries(preset) }
}

/// Effective value of a slider on a ship: stored override, else the preset's value, else the default.
/// Stored values are clamped to the slider's range; a non-finite stored value counts as absent. A key outside
/// the registry returns its stored value, else 0. Unit: `sys-officer-tuning`.
pub fn slider_value(settings: &OfficerSettings, key: &str) -> f64 {
    let def = slider(key);
    if let Some(v) = settings.sliders.get(key).copied().filter(|v| v.is_finite()) {
        return match def { Some(d) => v.clamp(d.min, d.max), None => v };
    }
    if let Some((_, v)) = preset_sliders(settings.preset).iter().find(|(k, _)| *k == key) { return *v; }
    def.map(|d| d.default).unwrap_or(0.0)
}

/// Whether the preset or a stored value sets this slider (otherwise the stage parameter keeps its kit or
/// library default).
fn slider_is_set(settings: &OfficerSettings, key: &str) -> bool {
    settings.sliders.get(key).is_some_and(|v| v.is_finite()) || preset_sliders(settings.preset).iter().any(|(k, _)| *k == key)
}

/// Stage parameter values implied by the settings (preset then sliders), for the compiler to bake into
/// generated stage nodes. Sliders that neither the preset nor the player set are absent, so kit and library
/// defaults stand. Unit: `sys-officer-tuning`.
pub fn stage_params(settings: &OfficerSettings) -> BTreeMap<(OfficerStage, &'static str), f64> {
    let mut out = BTreeMap::new();
    for s in sliders() {
        if !slider_is_set(settings, s.key) { continue; }
        let v = slider_value(settings, s.key);
        let t = if s.max > s.min { ((v - s.min) / (s.max - s.min)).clamp(0.0, 1.0) } else { 0.0 };
        for (stage, pkey, lo, hi) in s.drives { out.insert((*stage, *pkey), lo + t * (hi - lo)); }
    }
    if let Some(r) = settings.burst_trigger_range_m.filter(|r| r.is_finite() && *r > 0.0) {
        out.insert((OfficerStage::EmissionController, "sensors.burst_trigger_range_m"), r);
    }
    if let Some(r) = settings.self_defence_range_m.filter(|r| r.is_finite() && *r > 0.0) {
        out.insert((OfficerStage::ThreatRanking, "gunnery.self_defence_range_m"), r);
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Overrides and revert.

fn opt_scalar(v: Option<f64>) -> Value { v.filter(|x| x.is_finite()).map(Value::Scalar).unwrap_or(Value::None) }
fn role_ok(filter: Option<OfficerRole>, role: OfficerRole) -> bool { filter.is_none_or(|r| r == role) }
fn effective_repair_order(o: &[RepairPriority]) -> Vec<RepairPriority> { if o.is_empty() { RepairPriority::DEFAULT_ORDER.to_vec() } else { o.to_vec() } }
/// Officer a slider key belongs to: the registry's, else by key prefix.
fn slider_role(key: &str) -> OfficerRole {
    if let Some(s) = slider(key) { return s.role; }
    match key.split('.').next().unwrap_or("") {
        "helm" => OfficerRole::Helm,
        "engineer" => OfficerRole::Engineer,
        "sensors" => OfficerRole::Sensors,
        "dc" => OfficerRole::DamageControl,
        _ => OfficerRole::Gunnery,
    }
}

/// Every stored value in `current` that differs from `design`, plus damage-control sinks the player holds by
/// hand, for one officer or all. Pure over settings so the Bridge Board, the editor and tests share it.
pub(crate) fn settings_diff(current: &OfficerSettings, design: &OfficerSettings, dc: Option<&DcState>, role: Option<OfficerRole>) -> Vec<OverrideView> {
    let mut out = Vec::new();
    let mut push = |key: OverrideKey, label: Msg, default: Value, current: Value, layer: ControlLayer| out.push(OverrideView { key, label, default, current, layer });

    // L0: ship preset (it sets doctrine, so it belongs to Gunnery) and per-mount presets.
    if role_ok(role, OfficerRole::Gunnery) {
        if current.preset != design.preset {
            push(OverrideKey::Preset, Msg::new("tuning.override.preset"), Value::Preset(design.preset), Value::Preset(current.preset), ControlLayer::L0Doctrine);
        }
        let slots: BTreeSet<PartSlot> = current.mount_presets.keys().chain(design.mount_presets.keys()).copied().collect();
        for slot in slots {
            let (c, d) = (current.mount_presets.get(&slot), design.mount_presets.get(&slot));
            if c != d {
                push(OverrideKey::MountPreset(slot), Msg::new("tuning.override.mount_preset").arg(MsgArg::Part(slot)),
                    d.map(|p| Value::Preset(*p)).unwrap_or(Value::None), c.map(|p| Value::Preset(*p)).unwrap_or(Value::None), ControlLayer::L0Doctrine);
            }
        }
        // Doctrine entries, compared by position at ship level and per mount.
        doctrine_diff(&current.doctrine, &design.doctrine, None, &mut push);
        let mounts: BTreeSet<PartSlot> = current.mount_doctrine.keys().chain(design.mount_doctrine.keys()).copied().collect();
        for slot in mounts {
            let empty = Vec::new();
            doctrine_diff(current.mount_doctrine.get(&slot).unwrap_or(&empty), design.mount_doctrine.get(&slot).unwrap_or(&empty), Some(slot), &mut push);
        }
        if current.self_defence_range_m != design.self_defence_range_m {
            push(OverrideKey::SelfDefenceRange, Msg::new("tuning.override.self_defence_range"), opt_scalar(design.self_defence_range_m), opt_scalar(current.self_defence_range_m), ControlLayer::L1Tune);
        }
    }

    // L1 sliders: effective values, registry order first, then stored keys outside the registry.
    for s in sliders() {
        if !role_ok(role, s.role) { continue; }
        let (c, d) = (slider_value(current, s.key), slider_value(design, s.key));
        if c != d {
            push(OverrideKey::Slider(s.key.to_string()), Msg::new("tuning.override.slider").arg(MsgArg::Key(TextKey::new(s.label))), Value::Scalar(d), Value::Scalar(c), ControlLayer::L1Tune);
        }
    }
    let extra: BTreeSet<&String> = current.sliders.keys().chain(design.sliders.keys()).filter(|k| slider(k).is_none()).collect();
    for k in extra {
        if !role_ok(role, slider_role(k)) { continue; }
        let (c, d) = (current.sliders.get(k).copied(), design.sliders.get(k).copied());
        if c != d {
            push(OverrideKey::Slider(k.clone()), Msg::new("tuning.override.slider").arg(MsgArg::Name(k.clone())), opt_scalar(d), opt_scalar(c), ControlLayer::L1Tune);
        }
    }

    if role_ok(role, OfficerRole::Sensors) {
        if current.emcon != design.emcon {
            push(OverrideKey::Emcon, Msg::new("tuning.override.emcon"), Value::Emcon(design.emcon), Value::Emcon(current.emcon), ControlLayer::L1Tune);
        }
        if current.burst_trigger_range_m != design.burst_trigger_range_m {
            push(OverrideKey::BurstTriggerRange, Msg::new("tuning.override.burst_trigger_range"), opt_scalar(design.burst_trigger_range_m), opt_scalar(current.burst_trigger_range_m), ControlLayer::L1Tune);
        }
    }
    if role_ok(role, OfficerRole::Engineer) {
        let slots: BTreeSet<PartSlot> = current.power_priority.keys().chain(design.power_priority.keys()).copied().collect();
        for slot in slots {
            let (c, d) = (current.power_priority.get(&slot), design.power_priority.get(&slot));
            if c != d {
                push(OverrideKey::PowerPriority(slot), Msg::new("tuning.override.power_priority").arg(MsgArg::Part(slot)),
                    d.map(|p| Value::Scalar(*p as f64)).unwrap_or(Value::None), c.map(|p| Value::Scalar(*p as f64)).unwrap_or(Value::None), ControlLayer::L1Tune);
            }
        }
    }
    if role_ok(role, OfficerRole::DamageControl) {
        if effective_repair_order(&current.repair_order) != effective_repair_order(&design.repair_order) {
            push(OverrideKey::RepairOrder, Msg::new("tuning.override.repair_order"), Value::None, Value::None, ControlLayer::L1Tune);
        }
        if let Some(dc) = dc {
            let mut pumps: Vec<&(PartSlot, bool, Option<CompartmentId>)> = dc.manual_pumps.iter().collect();
            pumps.sort_by_key(|p| p.0);
            pumps.dedup_by_key(|p| p.0);
            for (slot, on, _) in pumps {
                push(OverrideKey::Sink(SinkKind::Pump(*slot)), Msg::new("tuning.override.pump").arg(MsgArg::Part(*slot)), Value::None, Value::Bool(*on), ControlLayer::L1Tune);
            }
            let mut doors: Vec<&(PartSlot, bool)> = dc.manual_doors.iter().collect();
            doors.sort_by_key(|d| d.0);
            doors.dedup_by_key(|d| d.0);
            for (slot, closed) in doors {
                push(OverrideKey::Sink(SinkKind::Door(*slot)), Msg::new("tuning.override.door").arg(MsgArg::Part(*slot)), Value::None, Value::Bool(*closed), ControlLayer::L1Tune);
            }
            let mut valves: Vec<&(CompartmentId, bool)> = dc.manual_valves.iter().collect();
            valves.sort_by_key(|v| v.0);
            valves.dedup_by_key(|v| v.0);
            for (c, open) in valves {
                push(OverrideKey::Sink(SinkKind::FloodValve(*c)), Msg::new("tuning.override.flood_valve").arg(MsgArg::Compartment(*c)), Value::None, Value::Bool(*open), ControlLayer::L1Tune);
            }
        }
    }
    out
}

fn doctrine_diff(current: &[DoctrineEntry], design: &[DoctrineEntry], mount: Option<PartSlot>, push: &mut impl FnMut(OverrideKey, Msg, Value, Value, ControlLayer)) {
    let n = current.len().max(design.len()).min(u8::MAX as usize + 1);
    for i in 0..n {
        if current.get(i) == design.get(i) { continue; }
        let label = match mount {
            None => Msg::new("tuning.override.doctrine").arg(MsgArg::Int(i as i64 + 1)),
            Some(slot) => Msg::new("tuning.override.mount_doctrine").arg(MsgArg::Part(slot)).arg(MsgArg::Int(i as i64 + 1)),
        };
        // A doctrine row has no single value; the client opens the doctrine table for the detail.
        push(OverrideKey::Doctrine { mount, index: i as u8 }, label, Value::None, Value::None, ControlLayer::L0Doctrine);
    }
}

/// Every value on a ship that differs from its design default, for one officer or all (Bridge Board override
/// count and diff, L351). Track stance and identity overrides live in the side picture and are listed by the
/// track card, not here. Unit: `sys-officer-tuning`.
pub fn overrides(ship: &Ship, role: Option<OfficerRole>) -> Vec<OverrideView> { settings_diff(&ship.settings, &ship.design.settings, Some(&ship.stages.dc), role) }

fn revert_doctrine(current: &mut Vec<DoctrineEntry>, design: &[DoctrineEntry], index: usize) -> bool {
    match (current.get(index).copied(), design.get(index).copied()) {
        (Some(c), Some(d)) if c != d => { current[index] = d; true }
        (Some(_), None) => { current.remove(index); true }
        (None, Some(d)) => { let at = index.min(current.len()); current.insert(at, d); true }
        _ => false,
    }
}

/// Revert one stored value to the design's. Pure over settings; `dc` holds the hand-set pumps, doors and valves.
pub(crate) fn revert_settings(current: &mut OfficerSettings, design: &OfficerSettings, dc: &mut DcState, key: &OverrideKey) -> bool {
    fn put<K: Ord + Copy, V: PartialEq + Copy>(cur: &mut BTreeMap<K, V>, des: &BTreeMap<K, V>, k: K) -> bool {
        let (c, d) = (cur.get(&k).copied(), des.get(&k).copied());
        if c == d { return false; }
        match d { Some(v) => { cur.insert(k, v); } None => { cur.remove(&k); } }
        true
    }
    match key {
        OverrideKey::Preset => { let changed = current.preset != design.preset; current.preset = design.preset; changed }
        OverrideKey::MountPreset(slot) => put(&mut current.mount_presets, &design.mount_presets, *slot),
        OverrideKey::Slider(k) => {
            let (c, d) = (current.sliders.get(k).copied(), design.sliders.get(k).copied());
            let effective_differs = slider(k).is_some() && slider_value(current, k) != slider_value(design, k);
            if c == d && !effective_differs { return false; }
            match d { Some(v) => { current.sliders.insert(k.clone(), v); } None => { current.sliders.remove(k); } }
            true
        }
        OverrideKey::Emcon => { let changed = current.emcon != design.emcon; current.emcon = design.emcon; changed }
        OverrideKey::BurstTriggerRange => { let changed = current.burst_trigger_range_m != design.burst_trigger_range_m; current.burst_trigger_range_m = design.burst_trigger_range_m; changed }
        OverrideKey::SelfDefenceRange => { let changed = current.self_defence_range_m != design.self_defence_range_m; current.self_defence_range_m = design.self_defence_range_m; changed }
        OverrideKey::Doctrine { mount: None, index } => revert_doctrine(&mut current.doctrine, &design.doctrine, *index as usize),
        OverrideKey::Doctrine { mount: Some(slot), index } => {
            let empty = Vec::new();
            let des = design.mount_doctrine.get(slot).unwrap_or(&empty);
            let cur = current.mount_doctrine.entry(*slot).or_default();
            let changed = revert_doctrine(cur, des, *index as usize);
            if cur.is_empty() && !design.mount_doctrine.contains_key(slot) { current.mount_doctrine.remove(slot); }
            changed
        }
        OverrideKey::PowerPriority(slot) => put(&mut current.power_priority, &design.power_priority, *slot),
        OverrideKey::RepairOrder => {
            let changed = effective_repair_order(&current.repair_order) != effective_repair_order(&design.repair_order);
            current.repair_order = design.repair_order.clone();
            changed
        }
        OverrideKey::Sink(SinkKind::Pump(slot)) => { let n = dc.manual_pumps.len(); dc.manual_pumps.retain(|p| p.0 != *slot); n != dc.manual_pumps.len() }
        OverrideKey::Sink(SinkKind::Door(slot)) => { let n = dc.manual_doors.len(); dc.manual_doors.retain(|d| d.0 != *slot); n != dc.manual_doors.len() }
        OverrideKey::Sink(SinkKind::FloodValve(c)) => { let n = dc.manual_valves.len(); dc.manual_valves.retain(|v| v.0 != *c); n != dc.manual_valves.len() }
        // Other sinks are only ever written by the graph; track overrides live in the side picture and are
        // reverted with `Command::SetStance` / `SetIdentity` carrying `None`.
        OverrideKey::Sink(_) | OverrideKey::TrackStance(_) | OverrideKey::TrackIdentity(_) => false,
    }
}

/// Revert one value to the design default (one-click revert). Returns false when it was not overridden.
/// The caller recompiles the ship's graph when a preset, slider or settings value changed.
/// Unit: `sys-officer-tuning`.
pub fn revert(ship: &mut Ship, key: &OverrideKey) -> bool {
    let design = ship.design.clone();
    revert_settings(&mut ship.settings, &design.settings, &mut ship.stages.dc, key)
}

/// Suggested Aggression from the facts that decide how a hull should fight: offensive reach (range-weighted
/// count of non-point-defence guns, anti-ship missiles and torpedoes against an 8 km reference mount), speed
/// (fast hulls can close and break off) and size (bigger hulls absorb return fire). A hull with no offensive
/// weapon gets the bottom of the useful range. Rounded to 0.05 so the slider lands on a tick.
pub(crate) fn suggest_from(mounts: &[crate::doctrine::MountInfo], top_speed_mps: f64, tonnage_t: f64) -> f64 {
    let offensive: Vec<&crate::doctrine::MountInfo> = mounts.iter().filter(|m| !m.point_defence && matches!(m.weapon, WeaponClass::Gun | WeaponClass::AntiShipMissile | WeaponClass::Torpedo)).collect();
    if offensive.is_empty() { return 0.2; }
    let punch: f64 = offensive.iter().map(|m| if m.effective_range_m.is_finite() { (m.effective_range_m / 8_000.0).clamp(0.0, 2.0) } else { 0.0 }).sum();
    let speed_kn = if top_speed_mps.is_finite() { top_speed_mps * 1.943_844 } else { 0.0 };
    let mut a = 0.45 + 0.08 * libm::log(1.0 + punch);
    a += (0.004 * (speed_kn - 30.0)).clamp(-0.1, 0.1);
    if offensive.iter().any(|m| m.weapon == WeaponClass::AntiShipMissile) { a += 0.05; }
    if tonnage_t.is_finite() && tonnage_t > 150.0 { a += 0.05 * libm::log10(tonnage_t / 150.0).clamp(0.0, 1.2); }
    let a = a.clamp(0.2, 0.9);
    (libm::round(a * 20.0) / 20.0).clamp(0.2, 0.9)
}

/// Suggested L1 Aggression for a design (Suggest button, L210). Unit: `sys-officer-tuning`.
pub fn suggest_aggression(design: &ShipDesign) -> f64 { suggest_from(&design.mounts, design.turn.top_speed_mps, design.tonnage_t) }

const OVERRIDE_KEYS: &[&str] = &[
    "tuning.override.preset", "tuning.override.mount_preset", "tuning.override.slider", "tuning.override.emcon",
    "tuning.override.burst_trigger_range", "tuning.override.self_defence_range", "tuning.override.doctrine",
    "tuning.override.mount_doctrine", "tuning.override.power_priority", "tuning.override.repair_order",
    "tuning.override.pump", "tuning.override.door", "tuning.override.flood_valve",
];

/// Every `tuning.*` key this unit can emit: slider labels, preset names and descriptions
/// (`game/i18n/tuning.csv`). Unit: `sys-officer-tuning`.
pub fn text_keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = sliders().iter().map(|s| s.label).collect();
    for p in [DoctrinePreset::Aggressive, DoctrinePreset::Cautious, DoctrinePreset::SilentRunning] { let (n, d) = preset_keys(p); keys.push(n); keys.push(d); }
    keys.extend_from_slice(OVERRIDE_KEYS);
    keys.sort_unstable();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::library;

    #[test]
    fn registry_shape() {
        for role in OfficerRole::ALL {
            let n = sliders().iter().filter(|s| s.role == role && s.primary).count();
            assert!((3..=6).contains(&n), "{role:?} has {n} primary sliders");
        }
        let mut keys: Vec<_> = sliders().iter().map(|s| s.key).collect();
        keys.sort();
        let len = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), len, "slider keys are unique");
        for s in sliders() {
            assert!(s.min < s.max && (s.min..=s.max).contains(&s.default), "{}", s.key);
            assert_eq!(s.label, format!("tuning.slider.{}", s.key));
            for (stage, pkey, lo, hi) in s.drives {
                let spec = library::stage_params_spec(*stage).into_iter().find(|p| p.key == *pkey).unwrap_or_else(|| panic!("{} drives a missing parameter {pkey}", s.key));
                assert!(*lo >= spec.min && *hi <= spec.max);
                assert_eq!(spec.default, s.default, "slider and stage defaults agree for {pkey}");
            }
        }
        // Every stage parameter is reachable from a slider or a settings field.
        for st in library::ALL_STAGES {
            for p in library::stage_params_spec(st) {
                let driven = sliders().iter().any(|s| s.drives.iter().any(|d| d.0 == st && d.1 == p.key));
                assert!(driven || matches!(p.key, "sensors.burst_trigger_range_m" | "gunnery.self_defence_range_m"), "{} is unreachable", p.key);
            }
        }
        for p in [DoctrinePreset::Aggressive, DoctrinePreset::Cautious, DoctrinePreset::SilentRunning] {
            for (k, v) in preset_sliders(p) { let s = slider(k).expect("preset names a slider"); assert!((s.min..=s.max).contains(v)); }
        }
    }

    #[test]
    fn slider_value_precedence() {
        let mut s = OfficerSettings::default();
        assert_eq!(slider_value(&s, "helm.lookahead"), 1.5);
        s.preset = DoctrinePreset::Aggressive;
        assert_eq!(slider_value(&s, "gunnery.aggression"), 0.85);
        s.sliders.insert("gunnery.aggression".into(), 7.0);
        assert_eq!(slider_value(&s, "gunnery.aggression"), 1.0, "stored values clamp to the range");
        s.sliders.insert("gunnery.aggression".into(), f64::NAN);
        assert_eq!(slider_value(&s, "gunnery.aggression"), 0.85, "NaN counts as absent");
        assert_eq!(slider_value(&s, "no.such"), 0.0);
    }

    #[test]
    fn stage_params_only_what_is_set() {
        let mut s = OfficerSettings::default();
        let p = stage_params(&s);
        assert_eq!(p.get(&(OfficerStage::ThreatRanking, "gunnery.aggression")), Some(&0.5));
        assert_eq!(p.get(&(OfficerStage::MountAssignment, "gunnery.aggression")), Some(&0.5));
        assert!(!p.contains_key(&(OfficerStage::HelmAutopilot, "helm.lookahead")));
        s.sliders.insert("helm.lookahead".into(), 3.0);
        s.burst_trigger_range_m = Some(12_000.0);
        s.self_defence_range_m = Some(f64::NAN);
        let p = stage_params(&s);
        assert_eq!(p.get(&(OfficerStage::HelmAutopilot, "helm.lookahead")), Some(&3.0));
        assert_eq!(p.get(&(OfficerStage::EmissionController, "sensors.burst_trigger_range_m")), Some(&12_000.0));
        assert!(!p.contains_key(&(OfficerStage::ThreatRanking, "gunnery.self_defence_range_m")));
    }

    #[test]
    fn diff_and_revert_round_trip() {
        let design = OfficerSettings::default();
        let mut cur = design.clone();
        let mut dc = DcState::default();
        assert!(settings_diff(&cur, &design, Some(&dc), None).is_empty());
        cur.preset = DoctrinePreset::SilentRunning;
        cur.sliders.insert("helm.evasion".into(), 0.9);
        cur.emcon = Emcon::Silent;
        cur.power_priority.insert(PartSlot(3), 1);
        cur.repair_order = vec![RepairPriority::PartRepairs];
        cur.doctrine.push(DoctrineEntry { weapon: WeaponClass::Gun, target: content::graph::TargetCategory::Surface, min_stance: Stance::Hostile, min_id: IdRung::Type, min_grade: TrackGrade::Fine, salvo: 2, effective_range_m: None });
        dc.manual_pumps.push((PartSlot(9), true, None));
        let all = settings_diff(&cur, &design, Some(&dc), None);
        // The preset also moves the effective aggression, cruise and open-fire sliders.
        assert!(all.iter().any(|o| o.key == OverrideKey::Preset));
        assert!(all.iter().any(|o| o.key == OverrideKey::Slider("gunnery.aggression".into())));
        let helm = settings_diff(&cur, &design, Some(&dc), Some(OfficerRole::Helm));
        assert!(helm.iter().all(|o| matches!(&o.key, OverrideKey::Slider(k) if k.starts_with("helm."))));
        assert_eq!(settings_diff(&cur, &design, Some(&dc), Some(OfficerRole::Engineer)).len(), 1);
        for o in &all { assert!(revert_settings(&mut cur, &design, &mut dc, &o.key) || matches!(o.key, OverrideKey::Slider(_)), "{:?}", o.key); }
        assert!(settings_diff(&cur, &design, Some(&dc), None).is_empty(), "everything reverted");
        assert_eq!(cur, design);
        assert!(!revert_settings(&mut cur, &design, &mut dc, &OverrideKey::Emcon));
    }

    #[test]
    fn doctrine_revert_by_index() {
        let e = |salvo| DoctrineEntry { weapon: WeaponClass::Gun, target: content::graph::TargetCategory::Surface, min_stance: Stance::Hostile, min_id: IdRung::Type, min_grade: TrackGrade::Fine, salvo, effective_range_m: None };
        let mut design = OfficerSettings::default();
        design.mount_doctrine.insert(PartSlot(2), vec![e(1)]);
        let mut cur = design.clone();
        cur.mount_doctrine.get_mut(&PartSlot(2)).unwrap().push(e(3));
        cur.mount_doctrine.get_mut(&PartSlot(2)).unwrap()[0] = e(4);
        let mut dc = DcState::default();
        let diff = settings_diff(&cur, &design, None, None);
        assert_eq!(diff.len(), 2);
        assert!(revert_settings(&mut cur, &design, &mut dc, &OverrideKey::Doctrine { mount: Some(PartSlot(2)), index: 1 }));
        assert!(revert_settings(&mut cur, &design, &mut dc, &OverrideKey::Doctrine { mount: Some(PartSlot(2)), index: 0 }));
        assert_eq!(cur, design);
    }

    #[test]
    fn suggestion_is_sane_and_deterministic() {
        let mount = |weapon, range, pd| crate::doctrine::MountInfo {
            slot: PartSlot(0), part: PartIdx(0), weapon, munition: None, pos_local: DVec3::ZERO, zero_rad: 0.0,
            limits: content::graph::MountLimits { train_min_deg: -150.0, train_max_deg: 150.0, elevation_min_deg: -5.0, elevation_max_deg: 70.0 },
            effective_range_m: range, director: None, kit: None, group: 0, muzzle_mps: 900.0, point_defence: pd,
        };
        assert_eq!(suggest_from(&[], 15.0, 150.0), 0.2);
        assert_eq!(suggest_from(&[mount(WeaponClass::PointDefenceGun, 2_000.0, true)], 15.0, 150.0), 0.2);
        let fac = suggest_from(&[mount(WeaponClass::Gun, 8_000.0, false), mount(WeaponClass::AntiShipMissile, 30_000.0, false)], 20.0, 200.0);
        let slow = suggest_from(&[mount(WeaponClass::Gun, 3_000.0, false)], 6.0, 150.0);
        assert!(fac > slow, "{fac} {slow}");
        for v in [fac, slow] { assert!((0.2..=0.9).contains(&v)); assert!(((v * 20.0).round() - v * 20.0).abs() < 1e-9); }
        assert_eq!(suggest_from(&[mount(WeaponClass::Gun, f64::NAN, false)], f64::NAN, f64::NAN), suggest_from(&[mount(WeaponClass::Gun, f64::NAN, false)], f64::NAN, f64::NAN));
    }

    #[test]
    fn keys_have_rows() {
        let csv = include_str!("../../../../game/i18n/tuning.csv");
        assert!(csv.starts_with("keys,?context,en"));
        for k in text_keys() { assert!(csv.lines().any(|l| l.starts_with(&format!("{k},"))), "tuning.csv lacks {k}"); }
    }
}
