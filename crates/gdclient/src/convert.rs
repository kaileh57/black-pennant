//! Unit `gd-convert`. Conversions shared by more than one facade: ids, vectors, angles and the Dictionary
//! shapes several classes return. Shapes are specified in [`crate::shapes`]; a shape used by one class only is
//! built in that class's own file by a function named `<shape>_dict`.
//! Implement this unit first: most facades call it.
//!
//! Rules: one function per shape, keys exactly as in `shapes.rs`; never produce English; angles leave here in
//! degrees true (0..360), map positions as `Vector2(x, z)`.
//!
//! Every `*_from_dict` reads data GDScript built, so it never panics: a missing or mistyped key gives `None`
//! (or the documented fallback), numbers are accepted as `int` or `float`, and enumerations outside their
//! `Bp.*` range are refused. Text this unit composes itself (rule sentences, load errors) uses keys in the
//! `units.*` namespace, whose English lives in `game/i18n/units.csv` (owned with `gd-text`).
use crate::enums::GdEnum;
use crate::text::{self, Lookup};
use content::blueprint::Blueprint;
use content::campaign::DifficultySliders;
use content::graph::{Cmp, Condition, DoctrineEntry, Operand, Rule, RuleValue, SinkKind, TrackFilter, Value};
use content::ids::{CompartmentId, ContentHash, PartSlot, PlayerSlot, TextKey};
use content::ruleset::Ruleset;
use content::stats::BlueprintStats;
use content::validate::Issue;
use content::weather::WeatherState;
use glam::{DVec2, DVec3};
use godot::obj::{EngineBitfield, EngineEnum};
use godot::prelude::*;
use sim_core::EntityId;
use sim_systems::ids::TrackId;
use sim_systems::protocol::{ControlRole, Msg, MsgArg, SortieStats, SortieSummary, Unit};

// ---- Ids and scalars (real code: one-liners) ----

/// Entity id as the opaque `int` GDScript holds.
pub fn entity_to_gd(e: EntityId) -> i64 { e.to_u64() as i64 }
/// `None` for `Bp.NONE`.
pub fn entity_from_gd(v: i64) -> Option<EntityId> { if v < 0 { None } else { Some(EntityId::from_u64(v as u64)) } }
/// Track number; `TrackId::NONE` becomes `Bp.NONE`.
pub fn track_to_gd(t: TrackId) -> i64 { if t.is_none() { -1 } else { t.0 as i64 } }
pub fn track_from_gd(v: i64) -> Option<TrackId> { if v <= 0 { None } else { Some(TrackId(v as u32)) } }
pub fn slot_to_gd(s: PartSlot) -> i64 { s.0 as i64 }
pub fn slot_from_gd(v: i64) -> Option<PartSlot> { u16::try_from(v).ok().map(PartSlot) }
pub fn player_to_gd(p: PlayerSlot) -> i64 { p.0 as i64 }
pub fn text_key(k: &TextKey) -> StringName { StringName::from(k.as_str()) }
pub fn hash_hex(h: &ContentHash) -> GString { GString::from(h.to_hex().as_str()) }

/// World metres to a map position.
pub fn map_from_world(p: DVec3) -> Vector2 { Vector2::new(p.x as f32, p.z as f32) }
pub fn map_from_dvec2(p: DVec2) -> Vector2 { Vector2::new(p.x as f32, p.y as f32) }
pub fn map_to_dvec2(v: Vector2) -> DVec2 { DVec2::new(v.x as f64, v.y as f64) }
/// Compass heading in radians (0 = north, clockwise) to degrees true in 0..360.
pub fn heading_deg(rad: f64) -> f64 { let d = rad.to_degrees() % 360.0; if d < 0.0 { d + 360.0 } else { d } }
/// Degrees true to radians; `None` for a negative "no value" argument.
pub fn heading_rad(deg: f64) -> Option<f64> { if deg < 0.0 { None } else { Some(deg.to_radians()) } }
/// Course in degrees true of a world velocity (x east, z south).
pub fn course_deg(vel: DVec3) -> f64 { heading_deg(vel.x.atan2(-vel.z)) }
pub fn mps_to_kn(mps: f64) -> f64 { mps * 1.943_844_5 }
pub fn kn_to_mps(kn: f64) -> f64 { kn / 1.943_844_5 }

// ---- Variant helpers (private) ----

/// Build a `VarDictionary` from `"key" => value` pairs of any `ToGodot` type.
macro_rules! d {
    ($($k:literal => $v:expr),* $(,)?) => {{
        let mut dict = VarDictionary::new();
        $( dict.set($k, &($v).to_variant()); )*
        dict
    }};
}
pub(crate) use d;

fn get(dict: &VarDictionary, key: &str) -> Option<Variant> { dict.get(key).filter(|v| !v.is_nil()) }
/// A number given as `int` or `float`; `None` for anything else or a non-finite float.
pub(crate) fn get_f64(dict: &VarDictionary, key: &str) -> Option<f64> { get(dict, key).and_then(|v| variant_f64(&v)) }
pub(crate) fn variant_f64(v: &Variant) -> Option<f64> {
    match v.get_type() {
        VariantType::FLOAT => v.try_to::<f64>().ok().filter(|f| f.is_finite()),
        VariantType::INT => v.try_to::<i64>().ok().map(|i| i as f64),
        _ => None,
    }
}
/// An integer given as `int`, or as a `float` holding a whole number.
pub(crate) fn get_i64(dict: &VarDictionary, key: &str) -> Option<i64> {
    let v = get(dict, key)?;
    match v.get_type() {
        VariantType::INT => v.try_to::<i64>().ok(),
        VariantType::FLOAT => v.try_to::<f64>().ok().filter(|f| f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e15).map(|f| f as i64),
        VariantType::BOOL => v.try_to::<bool>().ok().map(i64::from),
        _ => None,
    }
}
pub(crate) fn get_bool(dict: &VarDictionary, key: &str) -> Option<bool> {
    let v = get(dict, key)?;
    match v.get_type() { VariantType::BOOL => v.try_to::<bool>().ok(), VariantType::INT => v.try_to::<i64>().ok().map(|i| i != 0), _ => None }
}
/// A `String` or `StringName` value as Rust text.
pub(crate) fn get_text(dict: &VarDictionary, key: &str) -> Option<String> { get(dict, key).and_then(|v| variant_text(&v)) }
fn variant_text(v: &Variant) -> Option<String> {
    match v.get_type() {
        VariantType::STRING => v.try_to::<GString>().ok().map(|s| s.to_string()),
        VariantType::STRING_NAME => v.try_to::<StringName>().ok().map(|s| s.to_string()),
        _ => None,
    }
}
pub(crate) fn get_dict(dict: &VarDictionary, key: &str) -> Option<VarDictionary> { get(dict, key).and_then(|v| v.try_to::<VarDictionary>().ok()) }
fn get_array(dict: &VarDictionary, key: &str) -> Option<VarArray> { get(dict, key).and_then(|v| v.try_to::<VarArray>().ok()) }
fn enum_from<E: GdEnum>(dict: &VarDictionary, key: &str) -> Option<E> { get_i64(dict, key).and_then(E::from_gd) }
fn sn(s: &str) -> StringName { StringName::from(s) }
fn gs(s: &str) -> GString { GString::from(s) }
fn opt_slot(s: Option<PartSlot>) -> i64 { s.map_or(-1, slot_to_gd) }
fn array_of(dicts: impl IntoIterator<Item = VarDictionary>) -> VarArray { dicts.into_iter().map(|d| d.to_variant()).collect() }

/// Limits on nested data from GDScript (conditions inside conditions), so a hostile or broken file cannot
/// overflow the stack.
const MAX_CONDITION_DEPTH: usize = 8;
const MAX_CONDITION_ITEMS: usize = 32;

// ---- Shared shapes ----

/// `MsgDict`.
pub fn msg_dict(msg: &Msg) -> VarDictionary {
    let args: VarArray = msg.args.iter().map(msg_arg_variant).collect();
    d! { "key" => sn(msg.key.as_str()), "args" => args }
}

fn msg_arg_variant(arg: &MsgArg) -> Variant {
    match arg {
        MsgArg::Int(i) => i.to_variant(),
        MsgArg::Num { value, unit } => d! { "num" => if value.is_finite() { *value } else { 0.0 }, "unit" => sn(text::unit_name(*unit)) }.to_variant(),
        MsgArg::Key(k) => d! { "key" => sn(k.as_str()) }.to_variant(),
        MsgArg::Name(s) => gs(s).to_variant(),
        MsgArg::Track(t) => d! { "track" => track_to_gd(*t) }.to_variant(),
        MsgArg::Part(p) => d! { "part" => slot_to_gd(*p) }.to_variant(),
        MsgArg::Compartment(c) => d! { "compartment" => c.0 as i64 }.to_variant(),
        MsgArg::Stance(s) => d! { "stance" => s.to_gd() }.to_variant(),
        MsgArg::Rung(r) => d! { "rung" => r.to_gd() }.to_variant(),
        MsgArg::Grade(g) => d! { "grade" => g.to_gd() }.to_variant(),
        MsgArg::Emcon(e) => d! { "emcon" => e.to_gd() }.to_variant(),
    }
}

/// An empty `MsgDict` (`key == &""`).
pub fn empty_msg() -> VarDictionary { d! { "key" => StringName::default(), "args" => VarArray::new() } }

/// A `MsgDict` back into a `Msg` (the text facade formats from either).
pub fn msg_from_dict(dict: &VarDictionary) -> Msg {
    let key = get_text(dict, "key").unwrap_or_default();
    let args = get_array(dict, "args").map(|a| a.iter_shared().filter_map(|v| msg_arg_from_variant(&v)).collect()).unwrap_or_default();
    Msg { key: TextKey::new(key), args }
}

fn msg_arg_from_variant(v: &Variant) -> Option<MsgArg> {
    match v.get_type() {
        VariantType::INT => v.try_to::<i64>().ok().map(MsgArg::Int),
        VariantType::FLOAT => variant_f64(v).map(|value| MsgArg::Num { value, unit: Unit::None }),
        VariantType::BOOL => v.try_to::<bool>().ok().map(|b| MsgArg::Int(b as i64)),
        VariantType::STRING | VariantType::STRING_NAME => variant_text(v).map(MsgArg::Name),
        VariantType::DICTIONARY => {
            let d: VarDictionary = v.try_to().ok()?;
            if let Some(value) = get_f64(&d, "num") {
                let unit = get_text(&d, "unit").and_then(|u| text::unit_from_name(&u)).unwrap_or(Unit::None);
                return Some(MsgArg::Num { value, unit });
            }
            if let Some(k) = get_text(&d, "key") { return Some(MsgArg::Key(TextKey::new(k))); }
            if let Some(t) = get_i64(&d, "track") { return Some(MsgArg::Track(track_from_gd(t).unwrap_or(TrackId::NONE))); }
            if let Some(p) = get_i64(&d, "part") { return slot_from_gd(p).map(MsgArg::Part); }
            if let Some(c) = get_i64(&d, "compartment") { return u16::try_from(c).ok().map(|c| MsgArg::Compartment(CompartmentId(c))); }
            if d.contains_key("stance") { return enum_from(&d, "stance").map(MsgArg::Stance); }
            if d.contains_key("rung") { return enum_from(&d, "rung").map(MsgArg::Rung); }
            if d.contains_key("grade") { return enum_from(&d, "grade").map(MsgArg::Grade); }
            if d.contains_key("emcon") { return enum_from(&d, "emcon").map(MsgArg::Emcon); }
            None
        }
        _ => None,
    }
}

/// `WeatherDict`.
pub fn weather_dict(weather: &WeatherState) -> VarDictionary {
    d! {
        "kind" => weather.kind.to_gd(),
        "sea_state" => weather.sea_state.0.min(8) as i64,
        "wave_height_m" => weather.sea_state.significant_height_m(),
        // The state stores where the wind blows toward; the HUD shows where it comes from.
        "wind_from_deg" => heading_deg(weather.wind_dir_rad + std::f64::consts::PI),
        "wind_kn" => mps_to_kn(weather.wind_speed_mps.max(0.0)),
        "cloud" => weather.cloud_cover.clamp(0.0, 1.0),
        "visibility_m" => weather.visibility_m.max(0.0),
        "rain" => weather.rain.clamp(0.0, 1.0),
        "sun_elevation_deg" => weather.sun_elevation_deg,
        "sun_azimuth_deg" => heading_deg(weather.sun_azimuth_deg.to_radians()),
    }
}

/// `StatsDict`.
pub fn stats_dict(stats: &SortieStats) -> VarDictionary {
    d! {
        "rounds_fired" => stats.rounds_fired as i64,
        "rounds_hit" => stats.rounds_hit as i64,
        "missiles_fired" => stats.missiles_fired as i64,
        "missiles_hit" => stats.missiles_hit as i64,
        "torpedoes_fired" => stats.torpedoes_fired as i64,
        "torpedoes_hit" => stats.torpedoes_hit as i64,
        "decoys_fired" => stats.decoys_fired as i64,
        "missiles_seduced" => stats.missiles_seduced as i64,
        "fires" => stats.fires as i64,
        "first_detection_s" => stats.first_detection_s.map_or(-1.0, |s| s as f64),
        "radiating_s" => stats.radiating_s as f64,
        "enemy_tonnage_disabled_t" => stats.enemy_tonnage_disabled,
        "enemy_tonnage_total_t" => stats.enemy_tonnage_total,
        "own_cells_lost" => stats.own_cells_lost_fraction,
        "alerts_raised" => stats.alerts_raised as i64,
        "kills_by_method" => kills_array(&stats.kills_by_method),
        "enemy_struck" => stats.enemy_struck as i64,
    }
}

/// `Array[{ "method": int, "count": int }]` in `Bp.KILL_*` order, zero counts left out.
pub(crate) fn kills_array(kills: &std::collections::BTreeMap<content::campaign::KillMethod, u16>) -> VarArray {
    let mut rows: Vec<(i64, i64)> = kills.iter().filter(|(_, n)| **n > 0).map(|(m, n)| (m.to_gd(), *n as i64)).collect();
    rows.sort();
    array_of(rows.into_iter().map(|(method, count)| d! { "method" => method, "count" => count }))
}

/// `SummaryDict`.
pub fn summary_dict(summary: &SortieSummary) -> VarDictionary {
    let objectives = array_of(summary.objectives.iter().map(|(id, done)| d! { "id" => id.0 as i64, "done" => *done }));
    d! {
        "outcome" => summary.outcome.to_gd(),
        "winner_side" => summary.winner.map_or(-1, |s| s.0 as i64),
        "duration_s" => summary.duration_ticks as f64 / sim_core::TICK_HZ as f64,
        "objectives" => objectives,
        "stats" => stats_dict(&summary.stats),
        "tip" => summary.tip.as_ref().map_or_else(empty_msg, msg_dict),
        "end_reason" => crate::campaign::end_reason_msg(summary.end_reason).as_ref().map_or_else(empty_msg, msg_dict),
    }
}

/// `RoleDict`.
pub fn role_dict(role: ControlRole) -> VarDictionary {
    let (kind, index) = match role {
        ControlRole::Helm => (0, 0),
        ControlRole::Sensors => (1, 0),
        ControlRole::Mount(slot) => (2, slot_to_gd(slot)),
        ControlRole::WeaponGroup(g) => (3, g as i64),
    };
    d! { "kind" => kind, "index" => index }
}

/// A role from its `Bp.ROLE_*` kind and index.
pub fn role_from_gd(kind: i64, index: i64) -> Option<ControlRole> {
    match kind {
        0 => Some(ControlRole::Helm),
        1 => Some(ControlRole::Sensors),
        2 => slot_from_gd(index).map(ControlRole::Mount),
        3 => u8::try_from(index).ok().map(ControlRole::WeaponGroup),
        _ => None,
    }
}

/// The message of a validation issue: `Issue::text_key` (content's `validate.*` rows) with the issue's
/// values as arguments in field order.
pub fn issue_msg(issue: &Issue) -> Msg {
    let catalogue = crate::services::services().catalogue();
    let part_arg = |part: &content::ids::PartId| match catalogue.part(part) { Some(def) => MsgArg::Key(def.name.clone()), None => MsgArg::Name(part.as_str().to_string()) };
    let mut msg = Msg { key: issue.text_key(), args: Vec::new() };
    let m = |v: f64| MsgArg::Num { value: v, unit: Unit::Metres };
    let t = |v: f64| MsgArg::Num { value: v, unit: Unit::Tonnes };
    let n = |v: f64| MsgArg::Num { value: v, unit: Unit::None };
    msg.args = match issue {
        Issue::TooLong { length_m, max_m } => vec![m(*length_m), m(*max_m)],
        Issue::TooShort { length_m, min_m } => vec![m(*length_m), m(*min_m)],
        Issue::GridOutOfBounds | Issue::LayerNotAllowed | Issue::ScriptsNotAllowed => vec![],
        Issue::OverTonnage { tonnage_t, cap_t } => vec![t(*tonnage_t), t(*cap_t)],
        Issue::OverComplexity { points, cap } => vec![n(*points), n(*cap)],
        Issue::UnknownPart { part } => vec![MsgArg::Name(part.as_str().to_string())],
        Issue::PartNotAvailable { part } => vec![part_arg(part)],
        Issue::PartOverLimit { part, count, limit } => vec![part_arg(part), MsgArg::Int(*count as i64), MsgArg::Int(*limit as i64)],
        Issue::PartOutOfBounds { slot } | Issue::PartBadPlacement { slot } => vec![MsgArg::Part(*slot)],
        Issue::PartsOverlap { a, b } => vec![MsgArg::Part(*a), MsgArg::Part(*b)],
        Issue::Disconnected { .. } | Issue::BadPaintIndex { .. } => vec![],
        Issue::TooManyPaintEntries(count) => vec![MsgArg::Int(*count as i64)],
        Issue::TooManyRules(count) => vec![MsgArg::Int(*count as i64), MsgArg::Int(content::graph::MAX_RULES as i64)],
        Issue::BadGraph { detail } | Issue::BadKitBinding { detail } | Issue::BadShell { detail } => vec![MsgArg::Name(clip(detail, 80))],
    };
    msg
}

/// `IssueDict` from a validation issue. Validation issues are not fatal: the design loads and can be
/// edited; they block launching or fielding it.
pub fn issue_dict(issue: &Issue) -> VarDictionary {
    let part = match issue {
        Issue::PartOutOfBounds { slot } | Issue::PartBadPlacement { slot } => Some(*slot),
        Issue::PartsOverlap { a, .. } => Some(*a),
        _ => None,
    };
    let cell = match issue {
        Issue::Disconnected { stray_cell: c } | Issue::BadPaintIndex { cell: c } => Vector3i::new(c.x as i32, c.y as i32, c.z as i32).to_variant(),
        _ => Variant::nil(),
    };
    d! { "text" => msg_dict(&issue_msg(issue)), "fatal" => false, "part" => opt_slot(part), "cell" => cell }
}

/// The message of a load or decode error, in the `units.error.*` namespace. File names lose their folders
/// so no user path reaches the screen or a bug report.
pub fn error_msg(error: &content::ContentError) -> Msg {
    use content::ContentError as E;
    let name = |s: &str| MsgArg::Name(clip(s, 80));
    match error {
        E::Parse { file, message } => Msg::new("units.error.parse").arg(name(file_name(file))).arg(name(message)),
        E::Limit(what) => Msg::new("units.error.limit").arg(name(what)),
        E::FormatTooNew { found, supported, needs_release } => Msg::new("units.error.format_too_new").arg(MsgArg::Int(*found as i64)).arg(MsgArg::Int(*supported as i64)).arg(name(needs_release)),
        E::DamagedShareString => Msg::new("units.error.damaged_share_string"),
        E::NotAShareString => Msg::new("units.error.not_a_share_string"),
        E::UnknownRef { kind, key } => Msg::new("units.error.unknown_ref").arg(name(kind)).arg(name(key)),
        E::Migration { from, to, .. } => Msg::new("units.error.migration").arg(MsgArg::Int(*from as i64)).arg(MsgArg::Int(*to as i64)),
        E::SaveChain { .. } => Msg::new("units.error.save_chain"),
        E::Invalid(count) => Msg::new("units.error.invalid").arg(MsgArg::Int(*count as i64)),
        E::Io(_) => Msg::new("units.error.io"),
    }
}

fn file_name(path: &str) -> &str { path.rsplit(['/', '\\']).next().unwrap_or(path) }
fn clip(s: &str, max: usize) -> String { if s.chars().count() <= max { s.to_string() } else { format!("{}...", s.chars().take(max.saturating_sub(3)).collect::<String>()) } }

/// `IssueDict` (fatal) from a load or decode error.
pub fn error_issue_dict(error: &content::ContentError) -> VarDictionary {
    d! { "text" => msg_dict(&error_msg(error)), "fatal" => true, "part" => -1i64, "cell" => Variant::nil() }
}

/// `DifficultyDict`; `preset` is the matching preset id or empty.
pub fn difficulty_dict(sliders: &DifficultySliders, preset: &str) -> VarDictionary {
    d! {
        "preset" => sn(preset),
        "ai_accuracy" => sliders.ai_accuracy,
        "ai_reaction_s" => sliders.ai_reaction_s,
        "adaptation_cadence" => sliders.adaptation_cadence,
        "hull_count" => sliders.hull_count as i64,
        "economy" => sliders.economy,
        "clock_days" => sliders.clock_days as i64,
    }
}

/// Sliders from a `DifficultyDict`, clamped to the spec ranges (L847-860). `None` when a key is missing.
pub fn difficulty_from_dict(dict: &VarDictionary) -> Option<DifficultySliders> {
    let whole = |key: &str| get_f64(dict, key).map(|v| v.round().clamp(0.0, u16::MAX as f64) as u16);
    Some(DifficultySliders {
        ai_accuracy: get_f64(dict, "ai_accuracy")?,
        ai_reaction_s: get_f64(dict, "ai_reaction_s")?,
        adaptation_cadence: get_f64(dict, "adaptation_cadence")?,
        hull_count: whole("hull_count")?,
        economy: get_f64(dict, "economy")?,
        clock_days: whole("clock_days")?,
    }
    .clamped())
}

/// `RulesetDict`.
pub fn ruleset_dict(id: &str, ruleset: &Ruleset) -> VarDictionary {
    let mut limits = VarDictionary::new();
    for (part, n) in &ruleset.part_limits { limits.set(&sn(part.as_str()).to_variant(), &(*n as i64).to_variant()); }
    d! {
        "id" => gs(id),
        "kind" => ruleset.kind.to_gd(),
        "tonnage_cap_t" => ruleset.tonnage_cap_t,
        "complexity_per_vessel" => ruleset.complexity_per_vessel,
        "complexity_per_lobby" => ruleset.complexity_per_lobby,
        "max_length_m" => ruleset.max_length_m,
        "luau" => ruleset.luau,
        "part_limits" => limits,
    }
}

/// `BlueprintInfoDict`. `complexity` is -1.0: complexity points are measured by a trial, not stored in the
/// file (the gauge strip has them once measured).
pub fn blueprint_info_dict(bp: &Blueprint, stats: &BlueprintStats, path: &str, stock: bool, modified_unix: i64) -> VarDictionary {
    let header = &bp.header;
    let lineage = array_of(header.lineage.iter().map(|l| d! { "hash" => hash_hex(&l.hash), "author" => gs(&l.author) }));
    let kind = match header.kind { content::blueprint::BlueprintKind::Ship => "ship", content::blueprint::BlueprintKind::EnemyClass => "enemy_class" };
    let format = content::blueprint::BLUEPRINT_FORMAT;
    d! {
        "path" => gs(path),
        "stock" => stock,
        "name" => gs(&header.name),
        "author" => gs(header.author.as_deref().unwrap_or("")),
        "hash" => hash_hex(&header.hash),
        "format" => header.format as i64,
        "game_version" => gs(&header.game_version),
        "kind" => sn(kind),
        "length_m" => stats.length_m,
        "beam_m" => stats.beam_m,
        "displacement_t" => stats.tonnage_t,
        "cost" => stats.cost as i64,
        "complexity" => -1.0,
        "parts" => bp.parts.len() as i64,
        "modified_unix" => modified_unix,
        "lineage" => lineage,
        "needs_migration" => header.format < format,
        "newer_format" => header.format > format,
    }
}

// ---- Graph values, sinks, doctrine, rules ----

const VALUE_TYPES: [&str; 10] = ["none", "bool", "scalar", "vec3", "text", "stance", "emcon", "preset", "rung", "nearest_contact"];

/// `ValueDict`.
pub fn value_dict(value: &Value) -> VarDictionary {
    let (ty, v): (&str, Variant) = match value {
        Value::None => ("none", Variant::nil()),
        Value::Bool(b) => ("bool", b.to_variant()),
        Value::Scalar(s) => ("scalar", (if s.is_finite() { *s } else { 0.0 }).to_variant()),
        Value::Vec3(a) => ("vec3", Vector3::new(a[0] as f32, a[1] as f32, a[2] as f32).to_variant()),
        Value::Text(t) => ("text", gs(t).to_variant()),
        Value::Stance(s) => ("stance", s.to_gd().to_variant()),
        Value::Emcon(e) => ("emcon", e.to_gd().to_variant()),
        Value::Preset(p) => ("preset", p.to_gd().to_variant()),
        Value::Rung(r) => ("rung", r.to_gd().to_variant()),
    };
    d! { "type" => sn(ty), "value" => v }
}

pub fn value_from_dict(dict: &VarDictionary) -> Option<Value> {
    let ty = get_text(dict, "type")?;
    Some(match ty.as_str() {
        "none" => Value::None,
        "bool" => Value::Bool(get_bool(dict, "value")?),
        "scalar" => Value::Scalar(get_f64(dict, "value")?),
        "vec3" => {
            let v = get(dict, "value")?.try_to::<Vector3>().ok()?;
            if !(v.x.is_finite() && v.y.is_finite() && v.z.is_finite()) { return None; }
            Value::Vec3([v.x as f64, v.y as f64, v.z as f64])
        }
        "text" => Value::Text(get_text(dict, "value")?.chars().take(256).collect()),
        "stance" => Value::Stance(enum_from(dict, "value")?),
        "emcon" => Value::Emcon(enum_from(dict, "value")?),
        "preset" => Value::Preset(enum_from(dict, "value")?),
        "rung" => Value::Rung(enum_from(dict, "value")?),
        _ => return None,
    })
}

/// A graph value as a short display string with its unit ("12 kn", "Hostile"), already translated.
pub fn value_text(value: &Value) -> GString { gs(&value_text_with(&text::EngineLookup::new(), value, Unit::None)) }

/// [`value_text`] with an explicit lookup and the unit a scalar is measured in.
pub fn value_text_with(lk: &dyn Lookup, value: &Value, unit: Unit) -> String {
    match value {
        Value::None => lk.tr("units.value.none"),
        Value::Bool(b) => lk.tr(if *b { "units.value.true" } else { "units.value.false" }),
        Value::Scalar(s) => scalar_text(lk, *s, unit),
        Value::Vec3(a) => text::fill(&lk.tr("units.value.vec3"), &[scalar_text(lk, a[0], unit), scalar_text(lk, a[1], unit), scalar_text(lk, a[2], unit)]),
        Value::Text(t) => t.clone(),
        Value::Stance(s) => text::word_of(lk, "stance", *s),
        Value::Emcon(e) => text::word_of(lk, "emcon", *e),
        Value::Preset(p) => text::word_of(lk, "preset", *p),
        Value::Rung(r) => text::word_of(lk, "rung", *r),
    }
}

/// A scalar in `unit`: whole numbers without decimals, others with up to two; percentages are fractions.
fn scalar_text(lk: &dyn Lookup, v: f64, unit: Unit) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    let (v, unit) = if unit == Unit::Percent { (v * 100.0, unit) } else { (v, unit) };
    let decimals = if (v - v.round()).abs() < 1e-6 { 0 } else if (v * 10.0 - (v * 10.0).round()).abs() < 1e-6 { 1 } else { 2 };
    match unit {
        Unit::Metres => text::range_text(lk, v),
        Unit::Seconds => text::duration_text(lk, v),
        _ => text::value_with_unit(lk, v, unit, Some(decimals)),
    }
}

const SINK_KEYS: [&str; 15] = [
    "units.sink.helm_heading", "units.sink.helm_speed", "units.sink.mount_aim_point", "units.sink.mount_fire_permission",
    "units.sink.missile_launch", "units.sink.missile_target", "units.sink.emission_state", "units.sink.power_priority",
    "units.sink.pump", "units.sink.door", "units.sink.flood_valve", "units.sink.track_stance_override",
    "units.sink.track_id_override", "units.sink.ship_preset", "units.sink.ship_emcon",
];
const SINK_NAMES: [&str; 15] = [
    "helm_heading", "helm_speed", "mount_aim_point", "mount_fire_permission", "missile_launch", "missile_target",
    "emission_state", "power_priority", "pump", "door", "flood_valve", "track_stance_override", "track_id_override",
    "ship_preset", "ship_emcon",
];

/// Snake-case name and index argument of a sink (part slot, or compartment id for `flood_valve`).
fn sink_parts(sink: &SinkKind) -> (&'static str, i64) {
    match *sink {
        SinkKind::HelmHeading => (SINK_NAMES[0], -1),
        SinkKind::HelmSpeed => (SINK_NAMES[1], -1),
        SinkKind::MountAimPoint(p) => (SINK_NAMES[2], slot_to_gd(p)),
        SinkKind::MountFirePermission(p) => (SINK_NAMES[3], slot_to_gd(p)),
        SinkKind::MissileLaunch(p) => (SINK_NAMES[4], slot_to_gd(p)),
        SinkKind::MissileTarget(p) => (SINK_NAMES[5], slot_to_gd(p)),
        SinkKind::EmissionState(p) => (SINK_NAMES[6], slot_to_gd(p)),
        SinkKind::PowerPriority(p) => (SINK_NAMES[7], slot_to_gd(p)),
        SinkKind::Pump(p) => (SINK_NAMES[8], slot_to_gd(p)),
        SinkKind::Door(p) => (SINK_NAMES[9], slot_to_gd(p)),
        SinkKind::FloodValve(c) => (SINK_NAMES[10], c.0 as i64),
        SinkKind::TrackStanceOverride => (SINK_NAMES[11], -1),
        SinkKind::TrackIdOverride => (SINK_NAMES[12], -1),
        SinkKind::ShipPreset => (SINK_NAMES[13], -1),
        SinkKind::ShipEmcon => (SINK_NAMES[14], -1),
    }
}

/// `SinkDict`. `part` holds the sink's index argument: the part slot, or the compartment id for
/// `flood_valve`; `Bp.NONE` for ship-wide sinks. No sink takes a track (`track` is always `Bp.NONE`): the
/// track comes from the rule's value.
pub fn sink_dict(sink: &SinkKind) -> VarDictionary {
    let (kind, part) = sink_parts(sink);
    d! { "kind" => sn(kind), "part" => part, "track" => -1i64 }
}

pub fn sink_from_dict(dict: &VarDictionary) -> Option<SinkKind> {
    let kind = get_text(dict, "kind")?;
    let index = get_i64(dict, "part").unwrap_or(-1);
    let slot = || slot_from_gd(index);
    Some(match kind.as_str() {
        "helm_heading" => SinkKind::HelmHeading,
        "helm_speed" => SinkKind::HelmSpeed,
        "mount_aim_point" => SinkKind::MountAimPoint(slot()?),
        "mount_fire_permission" => SinkKind::MountFirePermission(slot()?),
        "missile_launch" => SinkKind::MissileLaunch(slot()?),
        "missile_target" => SinkKind::MissileTarget(slot()?),
        "emission_state" => SinkKind::EmissionState(slot()?),
        "power_priority" => SinkKind::PowerPriority(slot()?),
        "pump" => SinkKind::Pump(slot()?),
        "door" => SinkKind::Door(slot()?),
        "flood_valve" => SinkKind::FloodValve(CompartmentId(u16::try_from(index).ok()?)),
        "track_stance_override" => SinkKind::TrackStanceOverride,
        "track_id_override" => SinkKind::TrackIdOverride,
        "ship_preset" => SinkKind::ShipPreset,
        "ship_emcon" => SinkKind::ShipEmcon,
        _ => return None,
    })
}

/// `DoctrineEntryDict`. `overridden` compares against the design's stored entry.
pub fn doctrine_entry_dict(entry: &DoctrineEntry, overridden: bool) -> VarDictionary {
    d! {
        "weapon" => entry.weapon.to_gd(),
        "target" => entry.target.to_gd(),
        "min_stance" => entry.min_stance.to_gd(),
        "min_id" => entry.min_id.to_gd(),
        "min_grade" => entry.min_grade.to_gd(),
        "salvo" => entry.salvo as i64,
        "effective_range_m" => entry.effective_range_m.filter(|r| r.is_finite() && *r > 0.0).unwrap_or(-1.0),
        "overridden" => overridden,
    }
}

/// Largest salvo a doctrine entry may ask for: the simulation's own cap, so the dry dock, the HUD and the
/// sim agree. A salvo of 0 is the hold encoding (fire only at a designated track, L485).
const MAX_SALVO: i64 = sim_systems::doctrine::MAX_SALVO as i64;

pub fn doctrine_entry_from_dict(dict: &VarDictionary) -> Option<DoctrineEntry> {
    let range = get_f64(dict, "effective_range_m").unwrap_or(-1.0);
    Some(DoctrineEntry {
        weapon: enum_from(dict, "weapon")?,
        target: enum_from(dict, "target")?,
        min_stance: enum_from(dict, "min_stance")?,
        min_id: enum_from(dict, "min_id")?,
        min_grade: enum_from(dict, "min_grade")?,
        salvo: get_i64(dict, "salvo")?.clamp(0, MAX_SALVO) as u8,
        effective_range_m: if range > 0.0 { Some(range.min(content::campaign::theatre::MAX_MAP_M)) } else { None },
    })
}

const OPERAND_NAMES: [&str; 12] = [
    "const", "own_speed_kn", "own_heel_deg", "fuel_fraction", "power_margin", "hull_health", "nearest_contact_range_m",
    "contact_count", "inbound_weapons", "part_health", "ammo_fraction", "sortie_time_s",
];

fn filter_dict(f: &TrackFilter) -> VarDictionary {
    d! { "min_stance" => f.min_stance.to_gd(), "domain" => f.domain.map_or(-1, |d| d.to_gd()), "min_grade" => f.min_grade.to_gd() }
}
fn filter_from_dict(dict: Option<VarDictionary>) -> Option<TrackFilter> {
    let Some(dict) = dict else { return Some(TrackFilter::default()) };
    let domain = match get_i64(&dict, "domain") { None | Some(-1) => None, Some(v) => Some(content::enums::Domain::from_gd(v)?) };
    Some(TrackFilter {
        min_stance: enum_from(&dict, "min_stance").unwrap_or_default(),
        domain,
        min_grade: enum_from(&dict, "min_grade").unwrap_or_default(),
    })
}

/// `OperandDict`.
fn operand_dict(op: &Operand) -> VarDictionary {
    let (i, value, part, filter) = match *op {
        Operand::Const(v) => (0, v, None, None),
        Operand::OwnSpeedKn => (1, 0.0, None, None),
        Operand::OwnHeelDeg => (2, 0.0, None, None),
        Operand::FuelFraction => (3, 0.0, None, None),
        Operand::PowerMargin => (4, 0.0, None, None),
        Operand::HullHealth => (5, 0.0, None, None),
        Operand::NearestContactRangeM(f) => (6, 0.0, None, Some(f)),
        Operand::ContactCount(f) => (7, 0.0, None, Some(f)),
        Operand::InboundWeapons => (8, 0.0, None, None),
        Operand::PartHealth(p) => (9, 0.0, Some(p), None),
        Operand::AmmoFraction(p) => (10, 0.0, Some(p), None),
        Operand::SortieTimeS => (11, 0.0, None, None),
    };
    d! {
        "kind" => sn(OPERAND_NAMES[i]),
        "value" => if value.is_finite() { value } else { 0.0 },
        "part" => opt_slot(part),
        "filter" => filter_dict(&filter.unwrap_or_default()),
    }
}

fn operand_from_dict(dict: &VarDictionary) -> Option<Operand> {
    let kind = get_text(dict, "kind")?;
    let part = || get_i64(dict, "part").and_then(slot_from_gd);
    Some(match kind.as_str() {
        "const" => Operand::Const(get_f64(dict, "value")?),
        "own_speed_kn" => Operand::OwnSpeedKn,
        "own_heel_deg" => Operand::OwnHeelDeg,
        "fuel_fraction" => Operand::FuelFraction,
        "power_margin" => Operand::PowerMargin,
        "hull_health" => Operand::HullHealth,
        "nearest_contact_range_m" => Operand::NearestContactRangeM(filter_from_dict(get_dict(dict, "filter"))?),
        "contact_count" => Operand::ContactCount(filter_from_dict(get_dict(dict, "filter"))?),
        "inbound_weapons" => Operand::InboundWeapons,
        "part_health" => Operand::PartHealth(part()?),
        "ammo_fraction" => Operand::AmmoFraction(part()?),
        "sortie_time_s" => Operand::SortieTimeS,
        _ => return None,
    })
}

/// `ConditionDict`.
fn condition_dict(c: &Condition) -> VarDictionary {
    match c {
        Condition::Compare { lhs, cmp, rhs } => d! { "op" => sn("compare"), "lhs" => operand_dict(lhs), "cmp" => cmp.to_gd(), "rhs" => operand_dict(rhs) },
        Condition::All(items) => d! { "op" => sn("all"), "items" => array_of(items.iter().map(condition_dict)) },
        Condition::Any(items) => d! { "op" => sn("any"), "items" => array_of(items.iter().map(condition_dict)) },
        Condition::Not(item) => d! { "op" => sn("not"), "item" => condition_dict(item) },
    }
}

fn condition_from_dict(dict: &VarDictionary, depth: usize) -> Option<Condition> {
    if depth > MAX_CONDITION_DEPTH { return None; }
    let op = get_text(dict, "op")?;
    let items = || -> Option<Vec<Condition>> {
        let arr = get_array(dict, "items")?;
        if arr.len() > MAX_CONDITION_ITEMS { return None; }
        arr.iter_shared().map(|v| v.try_to::<VarDictionary>().ok().and_then(|d| condition_from_dict(&d, depth + 1))).collect()
    };
    Some(match op.as_str() {
        "compare" => Condition::Compare {
            lhs: operand_from_dict(&get_dict(dict, "lhs")?)?,
            cmp: enum_from(dict, "cmp")?,
            rhs: operand_from_dict(&get_dict(dict, "rhs")?)?,
        },
        "all" => Condition::All(items()?),
        "any" => Condition::Any(items()?),
        "not" => Condition::Not(Box::new(condition_from_dict(&get_dict(dict, "item")?, depth + 1)?)),
        _ => return None,
    })
}

/// `RuleDict` (with nested `ConditionDict`, `OperandDict`, `SinkDict`, `ValueDict`).
pub fn rule_dict(index: usize, rule: &Rule, valid: bool) -> VarDictionary {
    let value = match &rule.value {
        RuleValue::Set(v) => value_dict(v),
        RuleValue::NearestContact(f) => d! { "type" => sn("nearest_contact"), "value" => filter_dict(f) },
    };
    d! {
        "index" => index as i64,
        "enabled" => rule.enabled,
        "when" => condition_dict(&rule.when),
        "sink" => sink_dict(&rule.sink),
        "value" => value,
        "text" => msg_dict(&rule_msg(&text::EngineLookup::new(), rule)),
        "valid" => valid,
    }
}

pub fn rule_from_dict(dict: &VarDictionary) -> Option<Rule> {
    let value_dict = get_dict(dict, "value")?;
    let value = if get_text(&value_dict, "type").as_deref() == Some("nearest_contact") {
        RuleValue::NearestContact(filter_from_dict(get_dict(&value_dict, "value"))?)
    } else {
        RuleValue::Set(value_from_dict(&value_dict)?)
    };
    Some(Rule {
        when: condition_from_dict(&get_dict(dict, "when")?, 0)?,
        sink: sink_from_dict(&get_dict(dict, "sink")?)?,
        value,
        enabled: get_bool(dict, "enabled").unwrap_or(true),
    })
}

// ---- The rule as a sentence ----

/// Unit an operand is measured in, for showing a constant compared against it.
fn operand_unit(op: &Operand) -> Unit {
    match op {
        Operand::OwnSpeedKn => Unit::Knots,
        Operand::OwnHeelDeg => Unit::Degrees,
        Operand::FuelFraction | Operand::HullHealth | Operand::PartHealth(_) | Operand::AmmoFraction(_) => Unit::Percent,
        Operand::PowerMargin => Unit::Kilowatts,
        Operand::NearestContactRangeM(_) => Unit::Metres,
        Operand::SortieTimeS => Unit::Seconds,
        Operand::Const(_) | Operand::ContactCount(_) | Operand::InboundWeapons => Unit::None,
    }
}

/// The contact filter as a noun phrase: "contact", "Hostile air contact", "Suspect contact at Fine or
/// better".
fn filter_text(lk: &dyn Lookup, f: &TrackFilter) -> String {
    let noun = match f.domain {
        None => lk.tr("units.filter.any"),
        Some(d) => text::word_key_of("domain", d.to_gd()).map(|k| lk.tr(&k.replace("units.word.domain.", "units.filter.domain."))).unwrap_or_default(),
    };
    let mut phrase = if f.min_stance > content::enums::Stance::Friend {
        text::fill(&lk.tr("units.filter.with_stance"), &[text::word_of(lk, "stance", f.min_stance), noun])
    } else {
        noun
    };
    if f.min_grade > content::enums::TrackGrade::Bearing {
        phrase = text::fill(&lk.tr("units.filter.with_grade"), &[phrase, text::word_of(lk, "grade", f.min_grade)]);
    }
    phrase
}

fn part_text(lk: &dyn Lookup, p: PartSlot) -> String { text::arg_text(lk, &MsgArg::Part(p)) }

fn operand_text(lk: &dyn Lookup, op: &Operand, unit: Unit) -> String {
    let key = |name: &str| lk.tr(&format!("units.operand.{name}"));
    match op {
        Operand::Const(v) => scalar_text(lk, *v, unit),
        Operand::NearestContactRangeM(f) => text::fill(&key("nearest_contact_range_m"), &[filter_text(lk, f)]),
        Operand::ContactCount(f) => text::fill(&key("contact_count"), &[filter_text(lk, f)]),
        Operand::PartHealth(p) => text::fill(&key("part_health"), &[part_text(lk, *p)]),
        Operand::AmmoFraction(p) => text::fill(&key("ammo_fraction"), &[part_text(lk, *p)]),
        other => key(OPERAND_NAMES[operand_index(other)]),
    }
}
fn operand_index(op: &Operand) -> usize {
    match op {
        Operand::Const(_) => 0, Operand::OwnSpeedKn => 1, Operand::OwnHeelDeg => 2, Operand::FuelFraction => 3, Operand::PowerMargin => 4,
        Operand::HullHealth => 5, Operand::NearestContactRangeM(_) => 6, Operand::ContactCount(_) => 7, Operand::InboundWeapons => 8,
        Operand::PartHealth(_) => 9, Operand::AmmoFraction(_) => 10, Operand::SortieTimeS => 11,
    }
}

const CMP_NAMES: [&str; 6] = ["lt", "le", "gt", "ge", "eq", "ne"];

/// The condition as a clause: "own speed is below 12 kn and Hostile contact count is at least 2".
pub fn condition_text(lk: &dyn Lookup, c: &Condition) -> String { condition_text_at(lk, c, 0) }
fn condition_text_at(lk: &dyn Lookup, c: &Condition, depth: usize) -> String {
    if depth > MAX_CONDITION_DEPTH { return String::new(); }
    let join = |items: &[Condition], key: &str, empty: &str| {
        let parts: Vec<String> = items.iter().map(|i| condition_text_at(lk, i, depth + 1)).collect();
        match parts.len() {
            0 => lk.tr(empty),
            _ => parts.into_iter().reduce(|a, b| text::fill(&lk.tr(key), &[a, b])).unwrap_or_default(),
        }
    };
    match c {
        Condition::Compare { lhs, cmp, rhs } => {
            // A constant takes the unit of what it is compared with.
            let (lu, ru) = (operand_unit(rhs), operand_unit(lhs));
            let cmp_text = lk.tr(&format!("units.cmp.{}", CMP_NAMES[cmp_index(*cmp)]));
            text::fill(&lk.tr("units.rule.compare"), &[operand_text(lk, lhs, lu), cmp_text, operand_text(lk, rhs, ru)])
        }
        Condition::All(items) => join(items, "units.rule.and", "units.rule.always"),
        Condition::Any(items) => join(items, "units.rule.or", "units.rule.never"),
        Condition::Not(item) => text::fill(&lk.tr("units.rule.not"), &[condition_text_at(lk, item, depth + 1)]),
    }
}
fn cmp_index(c: Cmp) -> usize { match c { Cmp::Lt => 0, Cmp::Le => 1, Cmp::Gt => 2, Cmp::Ge => 3, Cmp::Eq => 4, Cmp::Ne => 5 } }

/// The sink as a noun phrase: "helm speed", "fire permission of Gun 2".
pub fn sink_text(lk: &dyn Lookup, sink: &SinkKind) -> String {
    let (name, index) = sink_parts(sink);
    let pattern = lk.tr(&format!("units.sink.{name}"));
    let arg = match sink {
        SinkKind::FloodValve(c) => text::arg_text(lk, &MsgArg::Compartment(*c)),
        _ if index >= 0 => slot_from_gd(index).map(|p| part_text(lk, p)).unwrap_or_default(),
        _ => String::new(),
    };
    text::fill(&pattern, &[arg])
}

fn sink_unit(sink: &SinkKind) -> Unit {
    match sink { SinkKind::HelmHeading => Unit::Degrees, SinkKind::HelmSpeed => Unit::Knots, _ => Unit::None }
}

/// The rule as one sentence: "When own speed is below 12 kn, set helm speed to 20 kn" (L336). The three
/// parts are composed and translated here and travel as `Name` arguments of `units.rule.sentence`, because a
/// `MsgDict` cannot nest arguments.
pub fn rule_msg(lk: &dyn Lookup, rule: &Rule) -> Msg {
    let value = match &rule.value {
        RuleValue::Set(v) => match (v, &rule.sink) {
            (Value::Scalar(h), SinkKind::HelmHeading) => lk.digits(&text::bearing_text(h.rem_euclid(360.0))),
            (v, sink) => value_text_with(lk, v, sink_unit(sink)),
        },
        RuleValue::NearestContact(f) => text::fill(&lk.tr("units.value.nearest_contact"), &[filter_text(lk, f)]),
    };
    Msg::new("units.rule.sentence")
        .arg(MsgArg::Name(condition_text(lk, &rule.when)))
        .arg(MsgArg::Name(sink_text(lk, &rule.sink)))
        .arg(MsgArg::Name(value))
}

/// `SliderDict` for one L1 slider: definition from `sim_systems::officers::tuning`, value from the settings.
/// The step is the power of ten nearest to a hundredth of the range, so every slider has about 100 stops.
/// An unknown key gives a dictionary with `label` `&""` and a 0..1 range, which the UI skips.
pub fn slider_dict(key: &str, value: f64, overridden: bool) -> VarDictionary {
    let def = sim_systems::officers::tuning::slider(key);
    let (label, officer, min, max, default, basic) = match def {
        Some(s) => (s.label, s.role.to_gd(), s.min, s.max, s.default, s.primary),
        None => ("", -1, 0.0, 1.0, 0.0, false),
    };
    let (min, max) = if min <= max { (min, max) } else { (max, min) };
    let value = if value.is_finite() { value.clamp(min, max) } else { default };
    d! {
        "key" => gs(key),
        "label" => sn(label),
        "tooltip" => sn(&if label.is_empty() { String::new() } else { format!("{label}.tip") }),
        "officer" => officer,
        "min" => min,
        "max" => max,
        "step" => slider_step(min, max),
        "default" => default,
        "value" => value,
        "overridden" => overridden,
        "unit" => sn("none"),
        "basic" => basic,
    }
}

/// Power of ten nearest a hundredth of the range (0.01 for 0..1, 0.1 for 0..10).
pub fn slider_step(min: f64, max: f64) -> f64 {
    let range = (max - min).abs();
    if !range.is_finite() || range <= 0.0 { return 0.01; }
    let mut step = 1.0;
    let target = range / 100.0;
    while step > target * 3.16 { step /= 10.0; }
    while step * 3.16 < target { step *= 10.0; }
    step
}

/// `MapDict` for the briefing chart and the plot's land layer. Coasts and the 10 m shallows line are traced
/// around each island from the terrain height field (48 bearings, bisection to 5 m). Enemy spawn markers
/// are left out: the chart must not show where the enemy starts.
pub fn map_dict(map: &content::campaign::MapDef) -> VarDictionary {
    let terrain = sim_core::terrain::Terrain::new(map.clone());
    let (land, shallows) = map_contours(map, &|x, z| terrain.height_at(x, z));
    let to_array = |polys: Vec<Vec<[f64; 2]>>| -> VarArray { polys.into_iter().map(|p| p.into_iter().map(|q| Vector2::new(q[0] as f32, q[1] as f32)).collect::<PackedVector2Array>().to_variant()).collect() };
    let markers = array_of(map.markers.iter().filter(|m| m.kind != content::campaign::MarkerKind::EnemySpawn).map(|m| d! {
        "name" => gs(&m.name),
        "pos" => Vector2::new(m.pos_m[0] as f32, m.pos_m[1] as f32),
        "label" => sn(marker_key(m.kind)),
    }));
    d! {
        "size_m" => Vector2::new(map.size_m[0] as f32, map.size_m[1] as f32),
        "land" => to_array(land),
        "shallows" => to_array(shallows),
        "markers" => markers,
    }
}

/// Depth of the shallows line on the chart. DECISION: 10 m, deeper than any player hull draws, so water
/// inside it is where grounding becomes possible.
pub const SHALLOWS_DEPTH_M: f64 = 10.0;
const CONTOUR_BEARINGS: usize = 48;

/// Coast (height 0) and shallows (height -10 m) polygons, one each per island whose contour exists, traced
/// by bisection along rays from the island centre. `height` is the terrain height field.
pub fn map_contours(map: &content::campaign::MapDef, height: &dyn Fn(f64, f64) -> f64) -> (Vec<Vec<[f64; 2]>>, Vec<Vec<[f64; 2]>>) {
    let mut land = Vec::new();
    let mut shallows = Vec::new();
    for island in &map.islands {
        let [cx, cz] = island.centre_m;
        let reach = (island.radius_m.abs() * 3.0).clamp(50.0, content::campaign::theatre::MAX_MAP_M);
        for (level, out) in [(0.0, &mut land), (-SHALLOWS_DEPTH_M, &mut shallows)] {
            if !(height(cx, cz) > level) { continue; }
            let mut poly = Vec::with_capacity(CONTOUR_BEARINGS);
            for i in 0..CONTOUR_BEARINGS {
                let a = i as f64 * std::f64::consts::TAU / CONTOUR_BEARINGS as f64;
                let (dx, dz) = (a.sin(), -a.cos());
                let at = |r: f64| height(cx + dx * r, cz + dz * r);
                // Outermost crossing along the ray: step out, then bisect the last step that crossed.
                let steps = 64;
                let mut crossing = None;
                for s in 0..steps {
                    let (r0, r1) = (reach * s as f64 / steps as f64, reach * (s + 1) as f64 / steps as f64);
                    if at(r0) > level && at(r1) <= level { crossing = Some((r0, r1)); }
                }
                let Some((mut lo, mut hi)) = crossing else { continue };
                while hi - lo > 5.0 { let mid = 0.5 * (lo + hi); if at(mid) > level { lo = mid } else { hi = mid } }
                let r = 0.5 * (lo + hi);
                poly.push([cx + dx * r, cz + dz * r]);
            }
            if poly.len() >= 3 { out.push(poly); }
        }
    }
    (land, shallows)
}

fn marker_key(kind: content::campaign::MarkerKind) -> &'static str {
    use content::campaign::MarkerKind as K;
    match kind {
        K::PlayerSpawn => "units.marker.player_spawn",
        K::EnemySpawn => "units.marker.enemy_spawn",
        K::Objective => "units.marker.objective",
        K::ShoreLanding => "units.marker.shore_landing",
        K::Survivors => "units.marker.survivors",
        K::Berth => "units.marker.berth",
        K::ConvoyEntry => "units.marker.convoy_entry",
        K::ConvoyExit => "units.marker.convoy_exit",
        K::Exit => "units.marker.exit",
        K::RadarSite => "units.marker.radar_site",
        K::DepotSite => "units.marker.depot_site",
    }
}

/// `ConnectionDict` for one row of the connection panel (`Session.connections`, `LobbyPlayerDict.conn`).
/// `state` is `Bp.CONN_WARNING` while `link.warning` is set, `Bp.CONN_DROPPED` while `link.held_s` is set,
/// else `Bp.CONN_CONNECTED`; `warning_text` is `link::net::warning_msg` or empty.
pub fn connection_dict(link: &crate::seams::PlayerLink) -> VarDictionary {
    use crate::enums::ConnState;
    let state = if link.held_s.is_some() { ConnState::Dropped } else if link.warning.is_some() { ConnState::Warning } else { ConnState::Connected };
    d! {
        "player" => player_to_gd(link.slot),
        "name" => gs(&link.name),
        "state" => state.to_gd(),
        "ping_ms" => link.ping_ms as i64,
        "loss" => if link.loss.is_finite() { link.loss.clamp(0.0, 1.0) as f64 } else { 0.0 },
        "frames_behind" => link.frames_behind as i64,
        "quality" => link.quality.to_gd(),
        "cap_kbps" => link.cap_kbps as i64,
        "is_host" => link.host,
        "warning" => link.warning.map_or(-1, |w| w.to_gd()),
        "warning_text" => link.warning.map_or_else(empty_msg, |w| msg_dict(&crate::link::net::warning_msg(w))),
        "held_s" => link.held_s.map_or(-1, |s| s as i64),
        "officers_in_command" => link.held_s.is_some(),
    }
}

// ---- hull-mesh and generated-file data into Godot containers ----
// Main thread only, like everything that touches a Godot type. These are the only places gdclient turns
// `hull-mesh` output into `RenderingServer` input, so every render unit uploads the same way.

fn aabb(a: &crate::seams::Aabb) -> godot::builtin::Aabb {
    if a.is_empty() { return godot::builtin::Aabb::new(Vector3::ZERO, Vector3::ZERO); }
    let min = Vector3::new(a.min[0], a.min[1], a.min[2]);
    let max = Vector3::new(a.max[0], a.max[1], a.max[2]);
    godot::builtin::Aabb::new(min, max - min)
}

/// The surface Dictionary `RenderingServer::mesh_create_from_surfaces` and `mesh_add_surface` take, built from
/// `hull_mesh::SurfaceBytes` with no per-vertex work: `format: int` (the bytes' own `format`, one of
/// `hull_mesh::format::{CHUNK_SURFACE, COLORED_SURFACE, SHADOW_SURFACE}`), `primitive: int` (3, triangles),
/// `vertex_data: PackedByteArray`, `attribute_data: PackedByteArray` (present only when not empty: the RGBA8
/// colours of a coloured surface), `vertex_count: int`, `index_data: PackedByteArray`, `index_count: int`,
/// `aabb: AABB`. Indices are u16 when `vertex_count <= 65,536`, else u32, as the bytes already are.
pub fn surface_dict(surface: &crate::seams::SurfaceBytes) -> VarDictionary {
    let mut dict = d! {
        "format" => surface.format as i64,
        "primitive" => surface.primitive as i64,
        "vertex_data" => PackedByteArray::from(surface.vertex_data.as_slice()),
        "vertex_count" => surface.vertex_count as i64,
        "index_data" => PackedByteArray::from(surface.index_data.as_slice()),
        "index_count" => surface.index_count as i64,
        "aabb" => aabb(&surface.aabb),
    };
    if !surface.attribute_data.is_empty() { dict.set("attribute_data", &PackedByteArray::from(surface.attribute_data.as_slice()).to_variant()); }
    dict
}

/// The arrays `RenderingServer::mesh_add_surface_from_arrays` takes (`PRIMITIVE_TRIANGLES`), built from
/// `hull_mesh::MeshData`: `ARRAY_VERTEX` (`PackedVector3Array`), `ARRAY_NORMAL`, `ARRAY_COLOR`
/// (`PackedColorArray`, linear) when `colors` is not empty, `ARRAY_CUSTOM0` (`PackedFloat32Array`, 4 per
/// vertex) when `custom0` is not empty, `ARRAY_INDEX` (`PackedInt32Array`). Returns the arrays and the
/// compress-format flags to pass with them: `ARRAY_CUSTOM_RGBA_FLOAT << ARRAY_FORMAT_CUSTOM0_SHIFT` when
/// `custom0` is present, else 0. Winding is already clockwise; nothing is re-indexed.
///
/// A stream whose length does not match the vertex count is left out rather than handed to the engine, and
/// a mesh whose indices are not whole triangles or point past the vertices gives empty arrays (the engine
/// would reject it with an error per frame).
pub fn mesh_arrays(mesh: &crate::seams::MeshData) -> (VarArray, u64) {
    use godot::classes::rendering_server::{ArrayCustomFormat, ArrayFormat, ArrayType};
    let n = mesh.positions.len();
    let mut arrays = VarArray::new();
    arrays.resize(ArrayType::MAX.ord() as usize, &Variant::nil());
    let indices_ok = mesh.indices.len() % 3 == 0 && mesh.indices.iter().all(|&i| (i as usize) < n) && n <= i32::MAX as usize;
    if n == 0 || !indices_ok { return (arrays, 0); }
    let v3 = |a: &[f32; 3]| Vector3::new(a[0], a[1], a[2]);
    arrays.set(ArrayType::VERTEX.ord() as usize, &mesh.positions.iter().map(v3).collect::<PackedVector3Array>().to_variant());
    if mesh.normals.len() == n { arrays.set(ArrayType::NORMAL.ord() as usize, &mesh.normals.iter().map(v3).collect::<PackedVector3Array>().to_variant()); }
    if mesh.colors.len() == n {
        arrays.set(ArrayType::COLOR.ord() as usize, &mesh.colors.iter().map(|c| Color::from_rgba(c[0], c[1], c[2], c[3])).collect::<PackedColorArray>().to_variant());
    }
    let mut flags = 0u64;
    if mesh.custom0.len() == n {
        arrays.set(ArrayType::CUSTOM0.ord() as usize, &mesh.custom0.iter().flat_map(|c| c.iter().copied()).collect::<PackedFloat32Array>().to_variant());
        flags = (ArrayCustomFormat::RGBA_FLOAT.ord() as u64) << ArrayFormat::CUSTOM0_SHIFT.ord();
    }
    arrays.set(ArrayType::INDEX.ord() as usize, &mesh.indices.iter().map(|&i| i as i32).collect::<PackedInt32Array>().to_variant());
    (arrays, flags)
}

/// Read one generated texture: `FileAccess::get_file_as_bytes(seams::gen::res(relative))` then
/// `TexBlob::decode`. `None` when the file is missing (the generators have not run) or does not decode; the
/// caller logs once and draws without it. Works in exports because the `.bin` files ship as non-resource
/// files.
pub fn load_tex_blob(relative: &str) -> Option<crate::seams::gen::TexBlob> {
    let path = crate::seams::gen::res(relative);
    if !godot::classes::FileAccess::file_exists(path.as_str()) { return None; }
    let bytes = godot::classes::FileAccess::get_file_as_bytes(path.as_str());
    crate::seams::gen::TexBlob::decode(bytes.as_slice())
}

/// One `Image` per layer of a blob, in layer order: `Image::create_from_data(width, height, mip_levels > 1,
/// format, layer bytes)`, where `format` is the blob's `TexFormat` discriminant (already a Godot
/// `Image.Format` value) and each layer's bytes hold its whole mip chain. The caller wraps them by
/// `TexBlob::kind`: one image in an `ImageTexture` (`Texture2D`), all in a `Texture2DArray` (`Array`), or all
/// as the depth slices of an `ImageTexture3D` (`Volume`).
pub fn tex_blob_images(blob: &crate::seams::gen::TexBlob) -> Vec<Gd<godot::classes::Image>> {
    use godot::classes::image::Format;
    let layer_bytes = blob.layer_bytes();
    if blob.width == 0 || blob.height == 0 || blob.width > 16_384 || blob.height > 16_384 || layer_bytes == 0 || blob.data.len() != blob.expected_len() { return Vec::new(); }
    let format = Format::from_ord(blob.format as i32);
    blob.data
        .chunks_exact(layer_bytes)
        .take(blob.layers as usize)
        .filter_map(|layer| godot::classes::Image::create_from_data(blob.width as i32, blob.height as i32, blob.mip_levels > 1, format, &PackedByteArray::from(layer)))
        .collect()
}

/// A chunk volume as the 34 depth slices `RenderingServer::texture_3d_create` and `texture_3d_update` take:
/// `FORMAT_RGBA8`, no mipmaps, 34 x 34 x 34, slice z holding texels `z * 34 * 34 * 4 ..` of
/// `hull_mesh::VolumeTexture::bytes` (x fastest, then y). A volume of the wrong size gives an empty array.
pub fn volume_images(volume: &crate::seams::VolumeTexture) -> Array<Gd<godot::classes::Image>> {
    use godot::classes::image::Format;
    let edge = crate::seams::VolumeTexture::EDGE;
    let slice = edge * edge * 4;
    let mut out = Array::new();
    if volume.bytes.len() != crate::seams::VolumeTexture::BYTES { return out; }
    for bytes in volume.bytes.chunks_exact(slice) {
        match godot::classes::Image::create_from_data(edge as i32, edge as i32, false, Format::RGBA8, &PackedByteArray::from(bytes)) {
            Some(img) => out.push(&img),
            None => return Array::new(),
        }
    }
    out
}

/// Every `units.*` key this unit emits (rule sentences, values, filters, load errors, chart markers).
pub fn text_keys() -> Vec<&'static str> {
    let mut keys = vec![
        "units.error.parse", "units.error.limit", "units.error.format_too_new", "units.error.damaged_share_string",
        "units.error.not_a_share_string", "units.error.unknown_ref", "units.error.migration", "units.error.save_chain",
        "units.error.invalid", "units.error.io",
        "units.value.none", "units.value.true", "units.value.false", "units.value.vec3", "units.value.nearest_contact",
        "units.rule.sentence", "units.rule.compare", "units.rule.and", "units.rule.or", "units.rule.not", "units.rule.always", "units.rule.never",
        "units.filter.any", "units.filter.with_stance", "units.filter.with_grade",
        "units.filter.domain.surface", "units.filter.domain.subsurface", "units.filter.domain.air", "units.filter.domain.weapon",
        "units.cmp.lt", "units.cmp.le", "units.cmp.gt", "units.cmp.ge", "units.cmp.eq", "units.cmp.ne",
        "units.operand.own_speed_kn", "units.operand.own_heel_deg", "units.operand.fuel_fraction", "units.operand.power_margin",
        "units.operand.hull_health", "units.operand.nearest_contact_range_m", "units.operand.contact_count",
        "units.operand.inbound_weapons", "units.operand.part_health", "units.operand.ammo_fraction", "units.operand.sortie_time_s",
        "units.marker.player_spawn", "units.marker.enemy_spawn", "units.marker.objective", "units.marker.shore_landing",
        "units.marker.survivors", "units.marker.berth", "units.marker.convoy_entry", "units.marker.convoy_exit",
        "units.marker.exit", "units.marker.radar_site", "units.marker.depot_site",
    ];
    keys.extend(SINK_KEYS);
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::tests::CsvLookup;
    use content::enums::{Domain, Stance, TrackGrade};

    #[test]
    fn rule_reads_as_a_sentence() {
        let lk = CsvLookup::load();
        let rule = Rule {
            when: Condition::All(vec![
                Condition::Compare { lhs: Operand::OwnSpeedKn, cmp: Cmp::Lt, rhs: Operand::Const(12.0) },
                Condition::Compare { lhs: Operand::ContactCount(TrackFilter { min_stance: Stance::Hostile, domain: Some(Domain::Air), min_grade: TrackGrade::Bearing }), cmp: Cmp::Ge, rhs: Operand::Const(2.0) },
            ]),
            sink: SinkKind::HelmSpeed,
            value: RuleValue::Set(Value::Scalar(20.0)),
            enabled: true,
        };
        let text = crate::text::msg_text(&lk, &rule_msg(&lk, &rule));
        assert_eq!(text, "When own speed is below 12 kn and Hostile air contact count is at least 2, set helm speed to 20 kn");
        let heading = Rule { when: Condition::Compare { lhs: Operand::FuelFraction, cmp: Cmp::Le, rhs: Operand::Const(0.25) }, sink: SinkKind::HelmHeading, value: RuleValue::Set(Value::Scalar(40.0)), enabled: true };
        assert_eq!(crate::text::msg_text(&lk, &rule_msg(&lk, &heading)), "When fuel is at most 25%, set helm heading to 040");
        let not = Condition::Not(Box::new(Condition::Any(vec![])));
        assert_eq!(condition_text(&lk, &not), "not (never)");
    }

    #[test]
    fn deep_conditions_do_not_recurse_forever() {
        let lk = CsvLookup::load();
        let mut c = Condition::Compare { lhs: Operand::OwnHeelDeg, cmp: Cmp::Gt, rhs: Operand::Const(20.0) };
        for _ in 0..1000 { c = Condition::Not(Box::new(c)); }
        let _ = condition_text(&lk, &c);
        // Drop iteratively-built deep boxes without overflowing the test thread's stack.
        let mut cur = c;
        while let Condition::Not(inner) = cur { cur = *inner; }
    }

    #[test]
    fn slider_steps_have_about_a_hundred_stops() {
        assert_eq!(slider_step(0.0, 1.0), 0.01);
        assert_eq!(slider_step(0.0, 10.0), 0.1);
        assert!((slider_step(0.5, 3.0) - 0.01).abs() < 1e-12 || (slider_step(0.5, 3.0) - 0.1).abs() < 1e-12);
        assert_eq!(slider_step(1.0, 1.0), 0.01);
    }

    #[test]
    fn contours_trace_a_round_island() {
        let mut map = content::campaign::MapDef::default();
        map.islands.push(content::campaign::Island { centre_m: [1_000.0, -500.0], radius_m: 400.0, height_m: 30.0, seed: 0 });
        // A cone: 30 m at the centre, sea level at 400 m, -10 m at about 533 m.
        let h = |x: f64, z: f64| { let r = ((x - 1_000.0).powi(2) + (z + 500.0).powi(2)).sqrt(); 30.0 - r * 30.0 / 400.0 };
        let (land, shallows) = map_contours(&map, &h);
        assert_eq!((land.len(), shallows.len()), (1, 1));
        assert_eq!(land[0].len(), CONTOUR_BEARINGS);
        for p in &land[0] { let r = ((p[0] - 1_000.0).powi(2) + (p[1] + 500.0).powi(2)).sqrt(); assert!((r - 400.0).abs() < 5.0, "{r}"); }
        for p in &shallows[0] { let r = ((p[0] - 1_000.0).powi(2) + (p[1] + 500.0).powi(2)).sqrt(); assert!((r - 533.3).abs() < 5.0, "{r}"); }
        // A seamount below the shallows line draws nothing.
        let (l2, s2) = map_contours(&map, &|_, _| -20.0);
        assert!(l2.is_empty() && s2.is_empty());
    }

    #[test]
    fn errors_hide_folders() {
        let e = content::ContentError::Parse { file: "/Users/someone/blueprints/my ship.ron".into(), message: "expected `,`".into() };
        let m = error_msg(&e);
        assert_eq!(m.args[0], MsgArg::Name("my ship.ron".into()));
        assert_eq!(file_name("C:\\Users\\x\\a.ron"), "a.ron");
    }

    #[test]
    fn roles_round_trip() {
        for role in [ControlRole::Helm, ControlRole::Sensors, ControlRole::Mount(PartSlot(7)), ControlRole::WeaponGroup(2)] {
            let (k, i) = match role { ControlRole::Helm => (0, 0), ControlRole::Sensors => (1, 0), ControlRole::Mount(s) => (2, s.0 as i64), ControlRole::WeaponGroup(g) => (3, g as i64) };
            assert_eq!(role_from_gd(k, i), Some(role));
        }
        assert_eq!(role_from_gd(2, -1), None);
        assert_eq!(role_from_gd(3, 300), None);
        assert_eq!(role_from_gd(9, 0), None);
    }
}
