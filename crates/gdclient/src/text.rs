//! Unit `gd-text`. `BpText` (autoload `Tx`): turns messages, enumerations and numbers into translated text.
//! DESIGN.md L1693-1708 (copy rules: sentence case, capitals only for glossary codes, merge labels into
//! values such as "12 kn", reasons under 60 characters, officer lines as two 40-character lines),
//! L1707 (every string through `tr()` with a context column), L1740 (numbers through
//! `TranslationServer.format_number()`), L1645 (no micro sign in the font; minutes use the ASCII apostrophe),
//! L1661 (track numbers are tabular digits, never reused).
//! English lives in `res://i18n/*.csv`; the Rust side only ever holds text keys. Enumeration words come
//! from `sim_systems::text` keys.
//!
//! # Layout
//! The formatting rules are plain Rust behind the [`Lookup`] trait (translate a key, localise digits), so
//! they are unit-tested without the engine and shared with `crate::convert` (`value_text`, rule sentences).
//! `BpText` only wraps them with the engine's `TranslationServer`. This unit owns the key namespace `units.*`
//! and its English file `game/i18n/units.csv`.
use crate::enums::GdEnum;
use content::ids::PartSlot;
use godot::classes::TranslationServer;
use godot::obj::Singleton;
use godot::prelude::*;
use sim_systems::protocol::{Msg, MsgArg, Unit};

// ---------------------------------------------------------------------------------------------------------
// Pure formatting core
// ---------------------------------------------------------------------------------------------------------

/// Translation and digit localisation, so the rules below run with or without the engine.
pub trait Lookup {
    /// The translated text of `key`; the key itself when there is no row.
    fn tr(&self, key: &str) -> String;
    /// Localise the digits of an ASCII number string (`TranslationServer.format_number`, L1740).
    fn digits(&self, ascii: &str) -> String { ascii.to_string() }
    /// Display name of part `slot` of the viewer's ship, if one is known.
    fn part_name(&self, _slot: PartSlot) -> Option<String> { None }
}

/// Unit names as `MsgDict` spells them, in `protocol::Unit` order.
pub const UNIT_NAMES: [&str; 16] = ["none", "m", "km", "kn", "deg", "s", "min", "h", "pct", "kw", "l", "t", "mm", "rounds", "salvage", "intel"];
const UNITS: [Unit; 16] = [
    Unit::None, Unit::Metres, Unit::Kilometres, Unit::Knots, Unit::Degrees, Unit::Seconds, Unit::Minutes, Unit::Hours,
    Unit::Percent, Unit::Kilowatts, Unit::Litres, Unit::Tonnes, Unit::Millimetres, Unit::Rounds, Unit::Salvage, Unit::Intel,
];

pub fn unit_name(unit: Unit) -> &'static str { UNITS.iter().position(|u| *u == unit).map_or("none", |i| UNIT_NAMES[i]) }
pub fn unit_from_name(name: &str) -> Option<Unit> { UNIT_NAMES.iter().position(|n| *n == name).map(|i| UNITS[i]) }

/// Fill `{0}`, `{1}`... of a translated pattern. Placeholders without an argument stay as written, so a
/// missing argument is visible in review instead of silently vanishing.
pub fn fill(pattern: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(pattern.len() + args.iter().map(|a| a.len()).sum::<usize>());
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let digits = after.bytes().take_while(|b| b.is_ascii_digit()).count();
        let arg = if digits > 0 && after.as_bytes().get(digits) == Some(&b'}') { after[..digits].parse::<usize>().ok().and_then(|n| args.get(n)) } else { None };
        match arg {
            Some(a) => { out.push_str(a); rest = &after[digits + 1..]; }
            None => { out.push('{'); rest = after; }
        }
    }
    out.push_str(rest);
    out
}

/// A number as text with `decimals` places, rounded half away from zero, thousands grouped from 1,000 up
/// (the spec writes "2,000 m"). Separators come from `units.sep.*` so a locale can change them; digits then
/// go through [`Lookup::digits`]. NaN and infinities print as 0: a bad value never reaches the screen as
/// "NaN".
pub fn number_text(lk: &dyn Lookup, value: f64, decimals: usize) -> String {
    let decimals = decimals.min(6);
    let value = if value.is_finite() { value } else { 0.0 };
    let unit = 10u64.pow(decimals as u32);
    let scaled = (value.abs() * unit as f64).round().min(9.0e15);
    let negative = value < 0.0 && scaled > 0.0;
    // Integer arithmetic from here on, so 0.1 + 0.2 never prints as 0.30000000000000004.
    let scaled = scaled as u64;
    let (int_part, frac_part) = (scaled / unit, scaled % unit);
    let int_digits = int_part.to_string();
    let thousands = sep(lk, "units.sep.thousands", ",");
    let mut s = String::with_capacity(int_digits.len() + 8);
    if negative { s.push('-'); }
    for (n, ch) in int_digits.chars().enumerate() {
        if n > 0 && (int_digits.len() - n) % 3 == 0 { s.push_str(&thousands); }
        s.push(ch);
    }
    if decimals > 0 {
        s.push_str(&sep(lk, "units.sep.decimal", "."));
        s.push_str(&format!("{frac_part:0width$}", width = decimals));
    }
    lk.digits(&s)
}

fn sep(lk: &dyn Lookup, key: &str, fallback: &str) -> String { let t = lk.tr(key); if t == key || t.is_empty() { fallback.to_string() } else { t } }

/// Decimal places the HUD uses when a message does not say: whole numbers for most units, one decimal for
/// small fractional speeds, times and tonnages and for kilometres, so "0.4 kn" and "2.5 s" do not read as 0
/// and 3.
pub fn auto_decimals(unit: Unit, value: f64) -> usize {
    let a = value.abs();
    let fractional = (a - a.round()).abs() >= 0.05;
    match unit {
        Unit::Knots | Unit::Seconds | Unit::Hours | Unit::Tonnes if a < 10.0 && fractional => 1,
        Unit::Kilometres if a < 100.0 => 1,
        _ => 0,
    }
}

/// Text key of the pattern that merges a value with its unit (`units.unit.<name>`). Rounds have a singular
/// form so "1 round" reads right.
pub fn unit_key(unit: Unit, shown_value: f64) -> &'static str {
    match unit {
        Unit::None => "units.unit.none",
        Unit::Metres => "units.unit.m",
        Unit::Kilometres => "units.unit.km",
        Unit::Knots => "units.unit.kn",
        Unit::Degrees => "units.unit.deg",
        Unit::Seconds => "units.unit.s",
        Unit::Minutes => "units.unit.min",
        Unit::Hours => "units.unit.h",
        Unit::Percent => "units.unit.pct",
        Unit::Kilowatts => "units.unit.kw",
        Unit::Litres => "units.unit.l",
        Unit::Tonnes => "units.unit.t",
        Unit::Millimetres => "units.unit.mm",
        Unit::Rounds => if shown_value == 1.0 { "units.unit.rounds_one" } else { "units.unit.rounds" },
        Unit::Salvage => "units.unit.salvage",
        Unit::Intel => "units.unit.intel",
    }
}

/// A value with its unit merged in ("12 kn"). `decimals` `None` picks [`auto_decimals`]; metres then follow
/// the range rule ([`range_text`]) so long distances read in kilometres, and seconds from two minutes up read
/// as a duration ([`duration_text`]: "25 min", not "1,500 s").
pub fn value_with_unit(lk: &dyn Lookup, value: f64, unit: Unit, decimals: Option<usize>) -> String {
    let value = if value.is_finite() { value } else { 0.0 };
    if unit == Unit::Metres && decimals.is_none() { return range_text(lk, value); }
    if unit == Unit::Seconds && decimals.is_none() && value.abs() >= 120.0 { return duration_text(lk, value); }
    let d = decimals.unwrap_or_else(|| auto_decimals(unit, value)).min(6);
    let s = 10u64.pow(d as u32) as f64;
    let shown = (value * s).round() / s;
    fill(&lk.tr(unit_key(unit, shown)), &[number_text(lk, value, d)])
}

/// Under 2,000 m in metres, else kilometres with one decimal. Under 10 m the metres keep one decimal, and
/// under 1 m two, so a ship's own dimensions (a 0.15 m metacentric height, a 1.9 m draft) never read as 0 m.
pub fn range_text(lk: &dyn Lookup, metres: f64) -> String {
    let m = if metres.is_finite() { metres } else { 0.0 };
    if m.abs() < 1.0 && m != 0.0 {
        fill(&lk.tr("units.unit.m"), &[number_text(lk, m, 2)])
    } else if m.abs() < 10.0 && m != 0.0 {
        fill(&lk.tr("units.unit.m"), &[number_text(lk, m, 1)])
    } else if m.abs() < 2_000.0 {
        fill(&lk.tr("units.unit.m"), &[number_text(lk, m, 0)])
    } else {
        fill(&lk.tr("units.unit.km"), &[number_text(lk, m / 1_000.0, 1)])
    }
}

/// Three digits, 0..359: 47.2 gives "047", 359.6 gives "000". A negative or non-finite value is the "no
/// value" sentinel and gives "".
pub fn bearing_text(degrees: f64) -> String {
    if !degrees.is_finite() || degrees < 0.0 { return String::new(); }
    let d = (degrees.round() as i64).rem_euclid(360);
    format!("{d:03}")
}

/// "45 s", "12 min", "3 h 20 min". Under a minute in whole seconds, under an hour in whole minutes, else
/// hours and minutes (minutes dropped when zero).
pub fn duration_text(lk: &dyn Lookup, seconds: f64) -> String {
    let s = if seconds.is_finite() { seconds.clamp(0.0, 1.0e12) } else { 0.0 };
    let total = s.round() as u64;
    if total < 60 { return fill(&lk.tr("units.duration.s"), &[number_text(lk, total as f64, 0)]); }
    let minutes = (s / 60.0).round() as u64;
    if minutes < 60 { return fill(&lk.tr("units.duration.min"), &[number_text(lk, minutes as f64, 0)]); }
    let (h, m) = (minutes / 60, minutes % 60);
    if m == 0 {
        fill(&lk.tr("units.duration.h"), &[number_text(lk, h as f64, 0)])
    } else {
        fill(&lk.tr("units.duration.h_min"), &[number_text(lk, h as f64, 0), number_text(lk, m as f64, 0)])
    }
}

/// Sortie clock "04:12" or "1:04:12": whole seconds, rounded down so the clock never runs ahead.
pub fn clock_text(lk: &dyn Lookup, seconds: f64) -> String {
    let s = if seconds.is_finite() { seconds.clamp(0.0, 1.0e12) } else { 0.0 } as u64;
    let (h, m, sec) = (s / 3600, (s / 60) % 60, s % 60);
    let ascii = if h > 0 { format!("{h}:{m:02}:{sec:02}") } else { format!("{m:02}:{sec:02}") };
    lk.digits(&ascii)
}

/// Month (1..=12), day of month and hour of a theatre hour. DECISION (the spec names date-time groups but no
/// calendar): theatre hour 0 is 0000 on 1 March; the operation clock (28 to 60 days) ends in April or May.
pub fn theatre_date(theatre_hour: i64) -> (u32, u32, u32) {
    const DAYS: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let h = theatre_hour.max(0) as u64;
    let (mut day_index, hour) = (h / 24, (h % 24) as u32);
    day_index %= 365;
    let mut month = 2usize; // March, 0-based.
    while day_index >= DAYS[month] {
        day_index -= DAYS[month];
        month = (month + 1) % 12;
    }
    (month as u32 + 1, day_index as u32 + 1, hour)
}

/// Date-time group "141800Z MAR" (day, hour, minutes 00, zone Z, month code), L1704. The month code is
/// translatable (`units.month.<n>`).
pub fn dtg_text(lk: &dyn Lookup, theatre_hour: i64) -> String {
    let (month, day, hour) = theatre_date(theatre_hour);
    let month_code = lk.tr(&format!("units.month.{month}"));
    fill(&lk.tr("units.dtg"), &[lk.digits(&format!("{day:02}{hour:02}00")), month_code])
}

/// Four tabular digits; numbers past 9999 keep every digit (they are never reused, L1661). `""` for none.
pub fn track_label_text(track: i64) -> String { if track <= 0 { String::new() } else { format!("{track:04}") } }

/// Break radio text into at most `max_lines` lines of at most `max_chars` characters at word boundaries
/// (L1706). A word longer than a line is split. Text that does not fit ends its last line with "...".
pub fn break_lines(text: &str, max_chars: usize, max_lines: usize) -> Vec<String> {
    let max_chars = max_chars.max(4);
    let max_lines = max_lines.max(1);
    let len = |s: &str| s.chars().count();
    // Pieces no longer than a line: whole words, long words split.
    let mut pieces: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let chars: Vec<char> = word.chars().collect();
        for chunk in chars.chunks(max_chars) { pieces.push(chunk.iter().collect()); }
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for piece in &pieces {
        let need = if current.is_empty() { len(piece) } else { len(&current) + 1 + len(piece) };
        if need > max_chars {
            lines.push(std::mem::take(&mut current));
            if lines.len() == max_lines { break; }
        }
        if !current.is_empty() { current.push(' '); }
        current.push_str(piece);
        used += 1;
    }
    if lines.len() < max_lines && !current.is_empty() { lines.push(current); }
    if used < pieces.len() {
        if let Some(last) = lines.last_mut() {
            let keep = max_chars.saturating_sub(3);
            let mut cut: String = last.chars().take(keep).collect();
            // Prefer to end on a whole word when the boundary is near.
            if len(last) > keep {
                if let Some(space) = cut.rfind(' ') { if len(&cut[space..]) < 12 { cut.truncate(space); } }
            }
            *last = format!("{}...", cut.trim_end());
        }
    }
    lines
}

// ---- Enumeration words ----

/// Key prefix and value names per family, in `Bp.*` value order (the `GdEnum::VARIANTS` order of
/// `crate::enums`; checked by a test).
const WORD_FAMILIES: &[(&str, &str, &[&str])] = &[
    ("stance", "sim.word.stance", &["friend", "neutral", "suspect", "hostile"]),
    ("rung", "sim.word.rung", &["unknown", "domain", "type", "class", "exact"]),
    ("grade", "sim.word.grade", &["bearing", "coarse", "fine", "lock"]),
    ("officer", "sim.word.officer", &["helm", "engineer", "sensors", "gunnery", "damage_control"]),
    ("domain", "units.word.domain", &["surface", "subsurface", "air", "weapon"]),
    ("emcon", "units.word.emcon", &["silent", "burst", "active"]),
    ("preset", "units.word.preset", &["aggressive", "cautious", "silent_running"]),
    ("layer", "units.word.layer", &["doctrine", "tune", "rules", "graph", "script"]),
    ("damage", "units.word.damage", &["intact", "damaged", "heavy", "destroyed"]),
    ("priority", "units.word.priority", &["caution", "warning", "alarm", "emergency"]),
    ("subsystem", "units.word.subsystem", &["threat", "flooding", "fire", "stability", "power", "fuel", "propulsion", "steering", "weapons", "sensors", "data", "crew", "navigation", "mission", "system"]),
    ("mode", "units.word.mode", &["campaign", "skirmish", "sandbox", "pvp_elimination", "pvp_escort", "splash", "tutorial", "trial"]),
    ("weather", "units.word.weather", &["clear", "haze", "overcast", "rain", "storm", "fog_bank"]),
    ("time", "units.word.time", &["night", "dusk", "day"]),
    ("check", "units.word.check", &["pass", "caution", "fail"]),
    ("category", "units.word.category", &["hull", "propulsion", "power", "fuel", "control", "gun", "missile", "underwater", "defence", "offboard", "sensor", "crew", "damage_control", "special_ops"]),
    ("mission", "units.word.mission", &["convoy_raid", "depot_strike", "radar_decapitation", "agent_extraction", "minelaying", "prize_capture", "defector_escort", "feint", "photo_reconnaissance", "hunter_group_survival", "cutting_out"]),
    ("station", "units.word.station", &["board_and_capture", "insert_or_extract", "carry_passengers", "recover_survivors", "lay_mines", "photograph"]),
    ("repair", "units.word.repair", &["magazine_fires", "breaches_below_waterline", "other_fires", "conduits_to_critical", "other_conduits", "part_repairs"]),
    ("weapon", "units.word.weapon", &["gun", "anti_ship_missile", "sam", "point_defence_gun", "point_defence_missile", "torpedo", "depth_charge", "decoy"]),
    ("target", "units.word.target", &["surface", "air", "inbound_weapon", "subsurface"]),
    ("link", "units.word.link", &["network", "redundant", "buffer", "local_control", "dead"]),
    ("outcome", "units.word.outcome", &["victory", "defeat", "withdrawn", "aborted", "draw"]),
    ("quality", "units.word.quality", &["low", "medium", "high", "fallback"]),
    ("tier", "units.word.tier", &["1", "2", "3", "4", "5"]),
    ("act", "units.word.act", &["1", "2", "3", "4"]),
    ("confidence", "units.word.confidence", &["possible", "probable", "confirmed"]),
    ("kill", "units.word.kill", &["gunfire", "missile", "torpedo", "mine", "depth_charge", "fire", "capture", "collision"]),
    ("network", "units.word.network", &["power", "fuel", "data"]),
];

/// The text key of an enumeration word, or `None` for an unknown family or an out-of-range value.
pub fn word_key_of(kind: &str, value: i64) -> Option<String> {
    let (_, prefix, names) = WORD_FAMILIES.iter().find(|(k, _, _)| *k == kind)?;
    let name = usize::try_from(value).ok().and_then(|i| names.get(i))?;
    Some(format!("{prefix}.{name}"))
}

/// The word of a Rust enumeration value through its `Bp.*` number.
pub fn word_of<E: GdEnum>(lk: &dyn Lookup, kind: &str, e: E) -> String { word_key_of(kind, e.to_gd()).map(|k| lk.tr(&k)).unwrap_or_default() }

// ---- Messages ----

/// One message argument as text.
pub fn arg_text(lk: &dyn Lookup, arg: &MsgArg) -> String {
    match arg {
        MsgArg::Int(v) => number_text(lk, *v as f64, 0),
        MsgArg::Num { value, unit } => value_with_unit(lk, *value, *unit, None),
        MsgArg::Key(k) => if k.0.is_empty() { String::new() } else { lk.tr(k.as_str()) },
        MsgArg::Name(s) => s.clone(),
        MsgArg::Track(t) => lk.digits(&track_label_text(crate::convert::track_to_gd(*t))),
        MsgArg::Part(slot) => lk.part_name(*slot).unwrap_or_else(|| fill(&lk.tr("units.part.numbered"), &[number_text(lk, slot.0 as f64 + 1.0, 0)])),
        MsgArg::Compartment(c) => fill(&lk.tr("units.compartment.numbered"), &[number_text(lk, c.0 as f64 + 1.0, 0)]),
        MsgArg::Stance(s) => word_of(lk, "stance", *s),
        MsgArg::Rung(r) => word_of(lk, "rung", *r),
        MsgArg::Grade(g) => word_of(lk, "grade", *g),
        MsgArg::Emcon(e) => word_of(lk, "emcon", *e),
    }
}

/// A whole message: the translated key with every argument filled in. An empty message gives `""`.
pub fn msg_text(lk: &dyn Lookup, msg: &Msg) -> String {
    if msg.is_empty() { return String::new(); }
    let args: Vec<String> = msg.args.iter().map(|a| arg_text(lk, a)).collect();
    fill(&lk.tr(msg.key.as_str()), &args)
}

// ---- Keys ----

/// Every `units.*` key the text and convert units emit, for the CSV coverage check.
pub fn unit_keys() -> Vec<String> {
    let mut keys: Vec<String> = OWN_KEYS.iter().map(|k| k.to_string()).collect();
    for (_, prefix, names) in WORD_FAMILIES {
        if prefix.starts_with("units.") { keys.extend(names.iter().map(|n| format!("{prefix}.{n}"))); }
    }
    keys.extend((1..=12).map(|m| format!("units.month.{m}")));
    keys.extend(crate::convert::text_keys().into_iter().map(String::from));
    keys
}

const OWN_KEYS: &[&str] = &[
    "units.sep.thousands", "units.sep.decimal",
    "units.unit.none", "units.unit.m", "units.unit.km", "units.unit.kn", "units.unit.deg", "units.unit.s", "units.unit.min",
    "units.unit.h", "units.unit.pct", "units.unit.kw", "units.unit.l", "units.unit.t", "units.unit.mm", "units.unit.rounds",
    "units.unit.rounds_one", "units.unit.salvage", "units.unit.intel",
    "units.duration.s", "units.duration.min", "units.duration.h", "units.duration.h_min",
    "units.dtg", "units.part.numbered", "units.part.generated", "units.compartment.numbered",
];

// ---------------------------------------------------------------------------------------------------------
// Engine side
// ---------------------------------------------------------------------------------------------------------

/// [`Lookup`] through the engine's `TranslationServer`. Main thread only.
pub(crate) struct EngineLookup { server: Gd<TranslationServer>, locale: GString }
impl EngineLookup {
    pub(crate) fn new() -> Self { let server = TranslationServer::singleton(); let locale = server.get_locale(); Self { server, locale } }
}
impl Lookup for EngineLookup {
    fn tr(&self, key: &str) -> String { self.server.translate(&StringName::from(key)).to_string() }
    fn digits(&self, ascii: &str) -> String { self.server.format_number(ascii, &self.locale).to_string() }
    fn part_name(&self, slot: PartSlot) -> Option<String> { design_part_name(self, slot) }
}

/// The part's label, or its category word and order ("Gun 2", as `PartPlacement::label` describes), from
/// the design last saved in the dry dock, which is the ship the player fields.
fn design_part_name(lk: &dyn Lookup, slot: PartSlot) -> Option<String> {
    let services = crate::services::services();
    let design = services.design.borrow().clone()?;
    let parts = &design.blueprint.parts;
    let placement = parts.get(slot.index())?;
    if let Some(label) = placement.label.as_ref().filter(|l| !l.trim().is_empty()) { return Some(label.clone()); }
    let catalogue = services.catalogue();
    let category = catalogue.part(&placement.part)?.category;
    let order = parts[..=slot.index()].iter().filter(|p| catalogue.part(&p.part).is_some_and(|d| d.category == category)).count();
    Some(fill(&lk.tr("units.part.generated"), &[word_of(lk, "category", category), number_text(lk, order as f64, 0)]))
}

/// Text formatting. Autoload `Tx`.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct BpText {
    base: Base<Node>,
}

#[godot_api]
impl INode for BpText {
    fn init(base: Base<Node>) -> Self { Self { base } }
}

#[godot_api]
impl BpText {
    /// Format a `MsgDict` in the current locale: translates the key, then fills `{0}`, `{1}`... with the
    /// arguments (numbers with units, nested keys translated, track numbers as four digits, enumeration
    /// arguments as their words). An empty message gives `""`.
    #[func]
    fn msg(&self, msg: VarDictionary) -> GString { GString::from(msg_text(&EngineLookup::new(), &crate::convert::msg_from_dict(&msg)).as_str()) }
    /// A number with its unit merged in, for example `number(12.4, &"kn", 0)` gives "12 kn". Units are those
    /// of `MsgDict` (`&"none"`, `&"m"`, `&"km"`, `&"kn"`, `&"deg"`, `&"s"`, `&"min"`, `&"h"`, `&"pct"`,
    /// `&"kw"`, `&"l"`, `&"t"`, `&"mm"`, `&"rounds"`, `&"salvage"`, `&"intel"`).
    #[func]
    fn number(&self, value: f64, unit: StringName, decimals: i64) -> GString {
        let unit = unit_from_name(&unit.to_string()).unwrap_or(Unit::None);
        GString::from(value_with_unit(&EngineLookup::new(), value, unit, Some(decimals.clamp(0, 6) as usize)).as_str())
    }
    /// The word for an enumeration value. `kind` names the constant family in lower case: `&"stance"`,
    /// `&"rung"`, `&"grade"`, `&"domain"`, `&"emcon"`, `&"preset"`, `&"officer"`, `&"layer"`, `&"damage"`,
    /// `&"priority"`, `&"subsystem"`, `&"mode"`, `&"weather"`, `&"time"`, `&"check"`, `&"category"`,
    /// `&"mission"`, `&"station"`, `&"repair"`, `&"weapon"`, `&"target"`, `&"link"`, `&"outcome"`,
    /// `&"quality"`, `&"tier"`, `&"act"`, `&"confidence"`, `&"kill"`, `&"network"`. Stance, rung and billet
    /// words keep their initial capital (L1695).
    #[func]
    fn word(&self, kind: StringName, value: i64) -> GString {
        match word_key_of(&kind.to_string(), value) { Some(k) => GString::from(EngineLookup::new().tr(&k).as_str()), None => GString::new() }
    }
    /// The text key behind `word`, for controls that translate themselves.
    #[func]
    fn word_key(&self, kind: StringName, value: i64) -> StringName { word_key_of(&kind.to_string(), value).map_or_else(StringName::default, |k| StringName::from(k.as_str())) }
    /// Bearing or heading as three digits: 47.2 gives "047".
    #[func]
    fn bearing(&self, degrees: f64) -> GString { GString::from(EngineLookup::new().digits(&bearing_text(degrees)).as_str()) }
    /// Range with a sensible unit: under 2,000 m in metres, else kilometres with one decimal.
    #[func]
    fn range(&self, metres: f64) -> GString { GString::from(range_text(&EngineLookup::new(), metres).as_str()) }
    /// Duration: "45 s", "12 min", "3 h 20 min".
    #[func]
    fn duration(&self, seconds: f64) -> GString { GString::from(duration_text(&EngineLookup::new(), seconds).as_str()) }
    /// Sortie clock "04:12" or "1:04:12".
    #[func]
    fn clock(&self, seconds: f64) -> GString { GString::from(clock_text(&EngineLookup::new(), seconds).as_str()) }
    /// Date-time group of a theatre hour for briefings and the chart table (L1704).
    #[func]
    fn date_time_group(&self, theatre_hour: i64) -> GString { GString::from(dtg_text(&EngineLookup::new(), theatre_hour).as_str()) }
    /// Track label: four tabular digits.
    #[func]
    fn track_label(&self, track: i64) -> GString { GString::from(EngineLookup::new().digits(&track_label_text(track)).as_str()) }
    /// Break a line of radio text into at most `max_lines` lines of at most `max_chars` characters at word
    /// boundaries (L1706: two lines of 40).
    #[func]
    fn lines(&self, text: GString, max_chars: i64, max_lines: i64) -> PackedStringArray {
        let lines = break_lines(&text.to_string(), max_chars.clamp(1, 10_000) as usize, max_lines.clamp(1, 1_000) as usize);
        lines.iter().map(|l| GString::from(l.as_str())).collect()
    }
    /// Every text key the Rust side can emit, for the test that checks each has an English row.
    #[func]
    fn all_keys(&self) -> PackedStringArray {
        let mut keys: Vec<String> = unit_keys();
        keys.extend(crate::settings::text_keys());
        keys.extend(crate::campaign::text_keys());
        let lists: [fn() -> Vec<&'static str>; 12] = [
            crate::hud::text_keys,
            crate::editor::text_keys,
            crate::content_api::model::text_keys,
            sim_systems::text::text_keys,
            sim_systems::officers::board::text_keys,
            sim_systems::objectives::text_keys,
            sim_systems::officers::tuning::text_keys,
            sim_systems::graph::library::text_keys,
            sim_systems::editor::notes::text_keys,
            sim_systems::editor::suggest::text_keys,
            sim_systems::editor::splash::text_keys,
            crate::link::net::text_keys,
        ];
        for list in lists { keys.extend(list().into_iter().map(String::from)); }
        keys.sort();
        keys.dedup();
        keys.iter().map(|k| GString::from(k.as_str())).collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Reads `game/i18n/*.csv` English rows, as the engine would.
    pub(crate) struct CsvLookup(pub BTreeMap<String, String>);
    impl CsvLookup {
        pub(crate) fn load() -> Self {
            let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/i18n");
            let mut map = BTreeMap::new();
            for entry in std::fs::read_dir(dir).expect("i18n dir") {
                let path = entry.expect("entry").path();
                if path.extension().and_then(|e| e.to_str()) != Some("csv") { continue; }
                for (key, _ctx, en) in parse_csv(&std::fs::read_to_string(&path).expect("csv")) { map.insert(key, en); }
            }
            Self(map)
        }
    }
    impl Lookup for CsvLookup { fn tr(&self, key: &str) -> String { self.0.get(key).cloned().unwrap_or_else(|| key.to_string()) } }

    /// Minimal RFC 4180 reader: rows of (key, context, English), header skipped.
    pub(crate) fn parse_csv(text: &str) -> Vec<(String, String, String)> {
        let mut rows = Vec::new();
        let mut fields: Vec<String> = Vec::new();
        let mut field = String::new();
        let mut quoted = false;
        let mut chars = text.chars().peekable();
        let push_row = |fields: &mut Vec<String>, rows: &mut Vec<(String, String, String)>| {
            if fields.len() >= 3 && !fields[0].is_empty() { rows.push((fields[0].clone(), fields[1].clone(), fields[2].clone())); }
            fields.clear();
        };
        while let Some(c) = chars.next() {
            match (quoted, c) {
                (true, '"') if chars.peek() == Some(&'"') => { chars.next(); field.push('"'); }
                (true, '"') => quoted = false,
                (true, c) => field.push(c),
                (false, '"') => quoted = true,
                (false, ',') => fields.push(std::mem::take(&mut field)),
                (false, '\n') => { fields.push(std::mem::take(&mut field)); push_row(&mut fields, &mut rows); }
                (false, '\r') => {}
                (false, c) => field.push(c),
            }
        }
        if !field.is_empty() || !fields.is_empty() { fields.push(field); push_row(&mut fields, &mut rows); }
        rows.into_iter().skip(1).collect()
    }

    fn en() -> CsvLookup { CsvLookup::load() }

    #[test]
    fn numbers_group_and_round() {
        let lk = en();
        assert_eq!(number_text(&lk, 1234.5, 0), "1,235");
        assert_eq!(number_text(&lk, -0.04, 1), "0.0");
        assert_eq!(number_text(&lk, -2.25, 1), "-2.3");
        assert_eq!(number_text(&lk, 0.1 + 0.2, 2), "0.30");
        assert_eq!(number_text(&lk, 999.0, 0), "999");
        assert_eq!(number_text(&lk, 1_000_000.0, 0), "1,000,000");
        assert_eq!(number_text(&lk, f64::NAN, 0), "0");
    }

    #[test]
    fn units_merge_into_values() {
        let lk = en();
        assert_eq!(value_with_unit(&lk, 12.4, Unit::Knots, Some(0)), "12 kn");
        assert_eq!(value_with_unit(&lk, 0.4, Unit::Knots, None), "0.4 kn");
        assert_eq!(value_with_unit(&lk, 1.0, Unit::Rounds, None), "1 round");
        assert_eq!(value_with_unit(&lk, 40.0, Unit::Rounds, None), "40 rounds");
        assert_eq!(value_with_unit(&lk, 1_850.0, Unit::Metres, None), "1,850 m");
        assert_eq!(value_with_unit(&lk, 1_500.0, Unit::Seconds, None), "25 min");
        assert_eq!(value_with_unit(&lk, 14.0, Unit::Seconds, None), "14 s");
        // A ship's own small dimensions keep their decimals (a GM of 0.15 m against a 0.24 m limit).
        assert_eq!(value_with_unit(&lk, 0.151, Unit::Metres, None), "0.15 m");
        assert_eq!(value_with_unit(&lk, 1.94, Unit::Metres, None), "1.9 m");
        assert_eq!(value_with_unit(&lk, 0.0, Unit::Metres, None), "0 m");
        assert_eq!(value_with_unit(&lk, 12_340.0, Unit::Metres, None), "12.3 km");
        assert_eq!(value_with_unit(&lk, 25.0, Unit::Percent, None), "25%");
        for name in UNIT_NAMES { let u = unit_from_name(name).unwrap(); assert_eq!(unit_name(u), name); }
    }

    #[test]
    fn bearings_ranges_durations_clocks() {
        let lk = en();
        assert_eq!(bearing_text(47.2), "047");
        assert_eq!(bearing_text(359.6), "000");
        assert_eq!(bearing_text(-1.0), "");
        assert_eq!(range_text(&lk, 1_999.0), "1,999 m");
        assert_eq!(range_text(&lk, 2_000.0), "2.0 km");
        assert_eq!(duration_text(&lk, 45.0), "45 s");
        assert_eq!(duration_text(&lk, 12.0 * 60.0), "12 min");
        assert_eq!(duration_text(&lk, 3.0 * 3600.0 + 20.0 * 60.0), "3 h 20 min");
        assert_eq!(duration_text(&lk, 2.0 * 3600.0), "2 h");
        assert_eq!(clock_text(&lk, 252.9), "04:12");
        assert_eq!(clock_text(&lk, 3852.0), "1:04:12");
        assert_eq!(track_label_text(412), "0412");
        assert_eq!(track_label_text(-1), "");
    }

    #[test]
    fn date_time_groups() {
        let lk = en();
        assert_eq!(theatre_date(0), (3, 1, 0));
        assert_eq!(theatre_date(31 * 24 + 5), (4, 1, 5));
        assert_eq!(dtg_text(&lk, 13 * 24 + 18), "141800Z MAR");
    }

    #[test]
    fn radio_lines_fit_two_by_forty() {
        let text = "Helm, coming to 040: torpedo bearing 210, closing fast on the port quarter now";
        let lines = break_lines(text, 40, 2);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l.chars().count() <= 40), "{lines:?}");
        assert!(lines[1].ends_with("..."));
        assert_eq!(break_lines("short line", 40, 2), vec!["short line".to_string()]);
        let long = break_lines(&"x".repeat(100), 40, 2);
        assert!(long.iter().all(|l| l.chars().count() <= 40) && long[1].ends_with("..."));
        assert!(break_lines("", 40, 2).is_empty());
    }

    #[test]
    fn fill_handles_missing_and_braces() {
        assert_eq!(fill("{0} and {1}", &["a".into()]), "a and {1}");
        assert_eq!(fill("{x} {0}", &["a".into()]), "{x} a");
        assert_eq!(fill("héllo {0}°", &["5".into()]), "héllo 5°");
    }

    #[test]
    fn word_families_match_constants() {
        use crate::enums::GdEnum;
        let check = |kind: &str, n: usize| {
            let names = WORD_FAMILIES.iter().find(|f| f.0 == kind).unwrap().2;
            assert_eq!(names.len(), n, "{kind}");
            assert!(word_key_of(kind, n as i64).is_none());
        };
        check("stance", content::enums::Stance::VARIANTS.len());
        check("rung", content::enums::IdRung::VARIANTS.len());
        check("grade", content::enums::TrackGrade::VARIANTS.len());
        check("officer", content::enums::OfficerRole::VARIANTS.len());
        check("domain", content::enums::Domain::VARIANTS.len());
        check("emcon", content::enums::Emcon::VARIANTS.len());
        check("preset", content::enums::DoctrinePreset::VARIANTS.len());
        check("layer", content::enums::ControlLayer::VARIANTS.len());
        check("damage", content::enums::DamageLevel::VARIANTS.len());
        check("priority", sim_systems::protocol::AlertPriority::VARIANTS.len());
        check("subsystem", sim_systems::protocol::Subsystem::VARIANTS.len());
        check("mode", content::enums::GameMode::VARIANTS.len());
        check("weather", content::weather::WeatherKind::VARIANTS.len());
        check("time", content::enums::TimeOfDay::VARIANTS.len());
        check("check", sim_systems::editor::Check::VARIANTS.len());
        check("category", content::part::PartCategory::VARIANTS.len());
        check("mission", content::campaign::MissionType::VARIANTS.len());
        check("station", content::campaign::StationAction::VARIANTS.len());
        check("repair", content::graph::RepairPriority::VARIANTS.len());
        check("weapon", content::graph::WeaponClass::VARIANTS.len());
        check("target", content::graph::TargetCategory::VARIANTS.len());
        check("link", sim_core::networks::LinkState::VARIANTS.len());
        check("outcome", sim_systems::protocol::Outcome::VARIANTS.len());
        check("quality", crate::enums::QualityTier::VARIANTS.len());
        check("tier", content::enums::Tier::VARIANTS.len());
        check("act", content::enums::Act::VARIANTS.len());
        check("confidence", sim_systems::theatre::Confidence::VARIANTS.len());
        check("kill", content::campaign::KillMethod::VARIANTS.len());
        check("network", content::enums::NetworkKind::VARIANTS.len());
        assert_eq!(word_key_of("stance", 3).as_deref(), Some("sim.word.stance.hostile"));
        assert!(word_key_of("nonsense", 0).is_none());
        assert!(word_key_of("stance", -1).is_none());
    }

    #[test]
    fn messages_format_every_argument_kind() {
        use content::enums::{Emcon, Stance};
        use content::ids::CompartmentId;
        use sim_systems::ids::TrackId;
        let lk = en();
        let msg = Msg::new("sim.hold.stance").arg(MsgArg::Stance(Stance::Suspect)).arg(MsgArg::Stance(Stance::Hostile));
        assert_eq!(msg_text(&lk, &msg), "Holding: track is Suspect, doctrine needs Hostile");
        let m = Msg::new("sim.hold.out_of_range").num(1500.0, Unit::Metres).num(12_000.0, Unit::Metres);
        assert_eq!(msg_text(&lk, &m), "Holding: target at 1,500 m, range limit 12.0 km");
        assert_eq!(arg_text(&lk, &MsgArg::Track(TrackId(47))), "0047");
        assert_eq!(arg_text(&lk, &MsgArg::Compartment(CompartmentId(2))), "compartment 3");
        assert_eq!(arg_text(&lk, &MsgArg::Part(PartSlot(0))), "part 1");
        assert_eq!(arg_text(&lk, &MsgArg::Emcon(Emcon::Silent)), "Silent");
        assert_eq!(msg_text(&lk, &Msg::default()), "");
    }

    /// Every key `Tx.all_keys` lists has an English row, so no screen ever shows a raw key.
    #[test]
    fn every_emitted_key_has_english() {
        let lk = en();
        let mut keys: Vec<String> = unit_keys();
        keys.extend(crate::settings::text_keys());
        keys.extend(crate::campaign::text_keys());
        let lists: [fn() -> Vec<&'static str>; 13] = [
            crate::hud::text_keys, crate::editor::text_keys, crate::content_api::model::text_keys,
            sim_systems::text::text_keys, sim_systems::officers::board::text_keys, sim_systems::objectives::text_keys,
            sim_systems::officers::tuning::text_keys, sim_systems::graph::library::text_keys,
            sim_systems::editor::notes::text_keys, sim_systems::editor::suggest::text_keys,
            sim_systems::editor::splash::text_keys, crate::link::net::text_keys, crate::convert::text_keys,
        ];
        for list in lists { keys.extend(list().into_iter().map(String::from)); }
        let missing: Vec<&String> = keys.iter().filter(|k| !lk.0.contains_key(k.as_str())).collect();
        assert!(missing.is_empty(), "keys with no English row: {missing:?}");
    }

    /// Every `units.*` and `settings.*` key has an English row with a context, and no row is orphaned.
    #[test]
    fn own_keys_have_english_rows() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/i18n");
        for (file, keys) in [("units.csv", unit_keys()), ("settings.csv", crate::settings::text_keys())] {
            let rows = parse_csv(&std::fs::read_to_string(format!("{dir}/{file}")).expect("csv"));
            let in_csv: std::collections::BTreeSet<&str> = rows.iter().map(|r| r.0.as_str()).collect();
            for k in &keys { assert!(in_csv.contains(k.as_str()), "{file} lacks {k}"); }
            let wanted: std::collections::BTreeSet<&str> = keys.iter().map(|k| k.as_str()).collect();
            for (key, ctx, en) in &rows {
                assert!(wanted.contains(key.as_str()), "{file} row {key} is not emitted");
                assert!(!ctx.trim().is_empty() && !en.trim().is_empty(), "{file} row {key} lacks context or English");
                assert!(!en.contains('\u{b5}'), "{key}: micro sign");
                let lower = en.to_lowercase();
                for banned in ["introducing", "unlock", "empower", "elevate", "streamline", "transform", "discover", "seamless", "smart", "next-gen", "revolutionize", "leverage", "delve", "vibrant", "pivotal", "showcase", "tapestry", "testament", "unleash"] {
                    assert!(!lower.split(|c: char| !c.is_alphanumeric() && c != '-').any(|w| w == banned), "{key}: banned word {banned}");
                }
                assert!(!en.contains('!'), "{key}: exclamation mark");
            }
            let mut sorted = keys.clone();
            sorted.sort();
            let n = sorted.len();
            sorted.dedup();
            assert_eq!(n, sorted.len(), "{file}: duplicate key");
        }
    }
}
