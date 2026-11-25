//! Unit `gd-editor-analysis`. Live feedback on `BpEditor`: the gauge strip, Ship's Notes with Fix previews,
//! Suggest buttons, weapon arcs, the trial scorecard and Compare view, Fit Check, the Quartermaster and
//! validation. DESIGN.md L153-187 (gauges within 100 ms; split-strip fallback with stale values),
//! L188-198 (Ship's Notes: ranked, each Fix previews as a ghost, never blocks Launch), L200-212 (Suggest),
//! L227-229 (trial battery on each save), L801-807 (Fit Check and Quartermaster in missions), L168 (overlays).
//! Everything is computed by `sim_systems::editor` functions; fast gauges on the worker inside 100 ms, the
//! rest in the background. Results older than the current revision are reported as stale, never as current.
//!
//! # Revisions
//! A gauge is stale while the worker's newest value predates the design under edit; the strip names it in
//! `stale`, worked out when asked (the strip the worker returned only knows its own moment). Fixes,
//! suggestions and Quartermaster refits are previewed and accepted only from results of the current
//! revision: an edit computed for another revision could name parts by slots that have moved. Arcs, blind
//! sectors and the balance overlay use the newest built design while the hull, cells and parts are unchanged
//! since it was built (a tuning or graph edit does not move a gun).
//!
//! # Choices the spec leaves open
//! - Part names in notes and suggestions are taken from the design under edit (the HUD's part names come
//!   from the saved design, which may not have the part yet).
//! - Scorecard history is kept per design file under `user://trials/` (20 most recent), written when a
//!   battery result arrives, and follows the file through rename, move, copy and delete (`io.rs`).
//! - The release-gate fallbacks of L198 (notes ordered by measured effect) and L227 (short on-save trial)
//!   are build switches, [`NOTE_EFFECTS_FALLBACK`] and [`SHORT_TRIAL_FALLBACK`], flipped by the release
//!   process when a gate fails at a tag; both are off.
//! - In a campaign an unaffordable refit is a validation problem: Launch commits the refit, so it cannot
//!   launch what the shared Salvage cannot pay for. Open notes never block (L188).
use super::BpEditor;
use crate::convert::{d, empty_msg, msg_dict};
use crate::enums::{EditorMode, GdEnum, JobState};
use content::blueprint::Blueprint;
use content::campaign::ConstraintTag;
use content::catalogue::Catalogue;
use content::ids::{ContentHash, PartSlot};
use content::validate::Issue;
use godot::prelude::*;
use sim_systems::editor::gauges::{ArcSector, GaugeStrip};
use sim_systems::editor::notes::ShipNote;
use sim_systems::editor::suggest::{SuggestKind, Suggestion};
use sim_systems::editor::trials::Scorecard;
use sim_systems::editor::BlueprintEdit;
use sim_systems::protocol::{Msg, MsgArg, Unit};
use sim_systems::ship::ShipDesign;
use std::rc::Rc;
use std::sync::Arc;

/// L198 fallback: order notes within a tier by their measured effect on the trial score. Off unless the
/// playtest gate fails at a tag.
pub(crate) const NOTE_EFFECTS_FALLBACK: bool = false;
/// L227 fallback: the on-save battery runs 1 seed for 90 s, marked "Short trial". Off unless the 60 s gate
/// fails at a tag.
pub(crate) const SHORT_TRIAL_FALLBACK: bool = false;
/// Scorecards kept per design file.
const HISTORY_LEN: usize = 20;
/// Where scorecard history lives.
const TRIALS_DIR: &str = "user://trials";
/// Largest history file read.
const MAX_HISTORY_BYTES: u64 = 4 * 1024 * 1024;

/// Text keys this unit emits (`game/i18n/dock.csv`).
pub(crate) const ANALYSIS_KEYS: &[&str] = &[
    "dock.undo.fix", "dock.undo.suggest", "dock.undo.refit",
    "dock.fix.move_part", "dock.fix.add_part", "dock.fix.remove_part", "dock.fix.add_kit", "dock.fix.remove_kit", "dock.fix.swap_kit",
    "dock.fix.set_load", "dock.fix.hull", "dock.fix.bulkhead", "dock.fix.armour", "dock.fix.slider", "dock.fix.burst", "dock.fix.limits",
    "dock.fix.unpin_node", "dock.fix.unpin_route", "dock.fix.several",
    "dock.suggest.waiting", "dock.suggest.matches",
    "dock.tag.draft", "dock.tag.rcs", "dock.tag.range", "dock.tag.passengers", "dock.tag.requires", "dock.tag.speed",
    "dock.issue.cannot_afford",
    "dock.launch.launch", "dock.launch.lock_in", "dock.launch.blocked", "dock.launch.not_open",
    "dock.metric.turning_circle_m", "dock.metric.time_to_90pct_speed_s", "dock.metric.top_speed_kn", "dock.metric.heel_recovery_s",
    "dock.metric.peak_heel_deg", "dock.metric.fight_score", "dock.metric.fight_wins", "dock.metric.fight_mean_hull_fraction_left",
    "dock.metric.fight_rounds_per_kill", "dock.metric.fight_first_salvo_hit_rate", "dock.metric.complexity_points",
];

// ---------------------------------------------------------------------------------------------------------
// Pure pieces.

/// The `Bp.JOB_*` state to report for a result of revision `result_rev` while the design is at `rev`.
pub(crate) fn result_state(result_rev: Option<u64>, rev: u64, job: Option<JobState>) -> JobState {
    match (result_rev, job) {
        (_, Some(JobState::Running)) => JobState::Running,
        (_, Some(JobState::Failed)) => JobState::Failed,
        (Some(r), _) if r == rev => JobState::Done,
        (Some(_), _) => JobState::Stale,
        (None, Some(j)) => j,
        (None, None) => JobState::Idle,
    }
}

/// Gauges whose newest value predates `rev`.
pub(crate) fn stale_gauges(fast_rev: u64, slow_rev: u64, rev: u64, trials_running: bool) -> Vec<&'static str> {
    let mut v = Vec::new();
    if fast_rev < rev { v.extend(crate::worker::FAST_GAUGES); }
    if slow_rev < rev { v.extend(crate::worker::SLOW_GAUGES); }
    // An unmeasured Complexity gauge says so through `measured`; it is "Updating" only while a battery runs.
    if trials_running { v.push("complexity"); }
    v
}

/// The Compare row of one metric: (text key, unit, whether a larger value is better).
fn metric_meta(name: &str) -> (String, &'static str, Option<bool>) {
    let key = format!("dock.metric.{}", name.replace('.', "_"));
    let (unit, higher) = match name {
        "turning_circle_m" => ("m", Some(false)),
        "time_to_90pct_speed_s" | "heel_recovery_s" => ("s", Some(false)),
        "top_speed_kn" => ("kn", Some(true)),
        "peak_heel_deg" => ("deg", Some(false)),
        "fight.score" | "fight.wins" => ("none", Some(true)),
        "fight.mean_hull_fraction_left" | "fight.first_salvo_hit_rate" => ("pct", Some(true)),
        "fight.rounds_per_kill" => ("rounds", Some(false)),
        "complexity_points" => ("none", Some(false)),
        _ => ("none", None),
    };
    (key, unit, higher)
}
/// -1 worse, 0 same, 1 better.
pub(crate) fn better(before: f64, after: f64, higher_is_better: Option<bool>) -> i64 {
    let Some(up) = higher_is_better else { return 0 };
    if !(before.is_finite() && after.is_finite()) { return 0; }
    let tol = 1e-9 * before.abs().max(after.abs()).max(1.0);
    if (after - before).abs() <= tol { return 0; }
    if (after > before) == up { 1 } else { -1 }
}

/// A constraint tag as one sentence.
pub(crate) fn tag_msg(tag: &ConstraintTag) -> (Msg, Unit) {
    use content::part::PartTag as T;
    let num = |v: f64, u: Unit| MsgArg::Num { value: if v.is_finite() { v } else { 0.0 }, unit: u };
    match tag {
        // Drafts differ by tenths of a metre, which the metres unit would round away ("Draft 3 m or less" for a
        // 2.5 m limit), so the depth goes in with one decimal, as the briefing writes it.
        ConstraintTag::DraftLimit { max_m } => (Msg::new("dock.tag.draft").arg(MsgArg::Name(format!("{:.1}", if max_m.is_finite() { max_m.max(0.0) } else { 0.0 }))), Unit::Metres),
        ConstraintTag::RcsLimit { max_m2 } => (Msg::new("dock.tag.rcs").arg(num(*max_m2, Unit::None)), Unit::None),
        ConstraintTag::Range { min_km } => (Msg::new("dock.tag.range").arg(num(*min_km, Unit::Kilometres)), Unit::Kilometres),
        ConstraintTag::Passengers { count, .. } => (Msg::new("dock.tag.passengers").arg(MsgArg::Int(*count as i64)), Unit::None),
        ConstraintTag::MinSpeed { kn } => (Msg::new("dock.tag.speed").arg(num(*kn, Unit::Knots)), Unit::Knots),
        ConstraintTag::Requires(t) => {
            let word = match t {
                T::Crane => "crane", T::MineRail => "mine_rail", T::RhibDavit => "rhib_davit", T::BoardingKit => "boarding_kit", T::RecoveryNet => "recovery_net",
                T::CameraMast => "camera_mast", T::PassengerBerths => "passenger_berths", T::PointDefence => "point_defence", T::Magazine => "magazine",
                T::FuelStore => "fuel_store", T::Propulsion => "propulsion", T::Steering => "steering", T::Generation => "generation", T::LowSignature => "low_signature",
            };
            (Msg::new("dock.tag.requires").arg(MsgArg::Key(content::ids::TextKey::new(format!("notes.tag_word.{word}")))), Unit::None)
        }
    }
}

/// A sentence with every part argument named from `bp` (the design under edit).
fn named_parts(msg: &Msg, bp: &Blueprint, cat: &Catalogue, lk: &dyn crate::text::Lookup) -> Msg {
    let mut m = msg.clone();
    for a in &mut m.args {
        if let MsgArg::Part(p) = a { if p.index() < bp.parts.len() { *a = MsgArg::Name(super::control::part_name_text(lk, bp, *p, cat)); } }
    }
    m
}

/// The label of a Fix button.
pub(crate) fn fix_label(edit: &BlueprintEdit, bp: &Blueprint, cat: &Catalogue, lk: &dyn crate::text::Lookup) -> Msg {
    let part = |s: PartSlot| MsgArg::Name(super::control::part_name_text(lk, bp, s, cat));
    let kit_name = |k: &content::ids::KitId| match bp.embedded_kits.get(k).or_else(|| cat.kit(k)) {
        Some(def) if !def.name.is_empty() => MsgArg::Name(def.name.clone()),
        Some(def) => def.name_key.clone().map_or(MsgArg::Name(k.as_str().to_string()), MsgArg::Key),
        None => MsgArg::Name(k.as_str().to_string()),
    };
    match edit {
        BlueprintEdit::MovePart { slot, .. } => Msg::new("dock.fix.move_part").arg(part(*slot)),
        BlueprintEdit::AddPart(p) => Msg::new("dock.fix.add_part").arg(cat.part(&p.part).map_or(MsgArg::Name(p.part.as_str().to_string()), |d| MsgArg::Key(d.name.clone()))),
        BlueprintEdit::RemovePart(s) => Msg::new("dock.fix.remove_part").arg(part(*s)),
        BlueprintEdit::AddKit { kit, .. } => Msg::new("dock.fix.add_kit").arg(kit_name(kit)),
        BlueprintEdit::RemoveKit(k) => Msg::new("dock.fix.remove_kit").arg(bp.kits.get(k.index()).map_or(MsgArg::Int(k.0 as i64 + 1), |p| kit_name(&p.kit))),
        BlueprintEdit::SwapKit { kit, .. } => Msg::new("dock.fix.swap_kit").arg(kit_name(kit)),
        BlueprintEdit::SetLoad { slot, load } => {
            let frac = bp.parts.get(slot.index()).and_then(|p| cat.part(&p.part)).map_or(*load, |d| match d.function { content::part::PartFunction::Ballast { max_kg } if max_kg > 0.0 => load / max_kg, _ => *load });
            Msg::new("dock.fix.set_load").arg(part(*slot)).arg(MsgArg::Num { value: (frac * 100.0).clamp(0.0, 100.0), unit: Unit::Percent })
        }
        BlueprintEdit::SetHullHandle { .. } => Msg::new("dock.fix.hull"),
        BlueprintEdit::AddBulkhead { .. } => Msg::new("dock.fix.bulkhead"),
        BlueprintEdit::PaintArmour { cells, .. } => Msg::new("dock.fix.armour").arg(MsgArg::Int(cells.len() as i64)),
        BlueprintEdit::SetSlider { key, value } => {
            let label = sim_systems::officers::tuning::slider(key).map_or(MsgArg::Name(key.clone()), |s| MsgArg::Key(content::ids::TextKey::new(s.label)));
            Msg::new("dock.fix.slider").arg(label).arg(MsgArg::Num { value: if value.is_finite() { *value } else { 0.0 }, unit: Unit::None })
        }
        BlueprintEdit::SetBurstTrigger { range_m } => Msg::new("dock.fix.burst").arg(MsgArg::Num { value: range_m / 1_000.0, unit: Unit::Kilometres }),
        BlueprintEdit::SetMountLimits { slot, .. } => Msg::new("dock.fix.limits").arg(part(*slot)),
        BlueprintEdit::UnpinNode(_) => Msg::new("dock.fix.unpin_node"),
        BlueprintEdit::UnpinRoute { .. } => Msg::new("dock.fix.unpin_route"),
        BlueprintEdit::Several(list) => match list.as_slice() { [one] => fix_label(one, bp, cat, lk), _ => Msg::new("dock.fix.several").arg(MsgArg::Int(list.len() as i64)) },
    }
}

/// Scorecard history of one file: (path, saved unix seconds, label, scorecard), oldest first.
pub(crate) type History = Vec<(String, i64, String, Scorecard)>;

/// The history file of a design path.
fn history_file(path: &str) -> String {
    let h = content::blueprint::io::blake3_hash(path.as_bytes()).to_hex();
    format!("{TRIALS_DIR}/{}.ron", &h[..32.min(h.len())])
}
/// History as file text.
pub(crate) fn history_text(h: &History) -> Option<String> { content::data::ron_options().to_string(h).ok() }
pub(crate) fn history_parse(text: &str) -> History { content::data::parse_ron::<History>(text, "trials.ron").unwrap_or_default() }
/// Append a scorecard unless its design is already the newest entry; keep the newest [`HISTORY_LEN`].
pub(crate) fn history_push(h: &mut History, path: &str, saved_unix: i64, label: &str, card: &Scorecard) -> bool {
    if h.last().is_some_and(|(_, _, _, c)| c.design == card.design && c == card) { return false; }
    h.retain(|(_, _, _, c)| !(c.design == card.design && c.short_trial == card.short_trial));
    h.push((path.to_string(), saved_unix, label.to_string(), card.clone()));
    if h.len() > HISTORY_LEN { let cut = h.len() - HISTORY_LEN; h.drain(..cut); }
    true
}

fn read_history(path: &str) -> History {
    use godot::classes::FileAccess;
    let file = history_file(path);
    let Some(f) = FileAccess::open(file.as_str(), godot::classes::file_access::ModeFlags::READ) else { return Vec::new() };
    if f.get_length() > MAX_HISTORY_BYTES { return Vec::new(); }
    let mut h = history_parse(&f.get_as_text().to_string());
    h.retain(|(p, _, _, _)| p == path);
    h
}
fn write_history(path: &str, h: &History) {
    let file = history_file(path);
    if h.is_empty() { let _ = godot::classes::DirAccess::remove_absolute(file.as_str()); return; }
    // `io::write_atomic` creates parent folders only under `user://blueprints`; the trials folder is made here.
    let _ = godot::classes::DirAccess::make_dir_recursive_absolute(TRIALS_DIR);
    if let Some(text) = history_text(h) { let _ = super::io::write_atomic(&file, &text); }
}

/// A design file was renamed or moved: its scorecards go with it.
pub(crate) fn history_moved(from: &str, to: &str) {
    let mut h = read_history(from);
    if h.is_empty() { return; }
    for e in &mut h { e.0 = to.to_string(); }
    write_history(to, &h);
    write_history(from, &Vec::new());
}
/// A folder was renamed or moved: every design file inside keeps its scorecards.
pub(crate) fn history_folder_moved(from: &str, to: &str) {
    use godot::classes::DirAccess;
    if !DirAccess::dir_exists_absolute(TRIALS_DIR) { return; }
    let prefix = format!("{from}/");
    for f in DirAccess::get_files_at(TRIALS_DIR).as_slice().iter().map(|f| f.to_string()).filter(|f| f.ends_with(".ron")).take(10_000) {
        let file = format!("{TRIALS_DIR}/{f}");
        let Ok(text) = super::io::read_text(&file) else { continue };
        let h = history_parse(&text);
        let Some(old) = h.first().map(|e| e.0.clone()).filter(|p| p.starts_with(&prefix)) else { continue };
        let new = format!("{to}/{}", &old[prefix.len()..]);
        history_moved(&old, &new);
    }
}
/// A design file was copied: the copy starts with the original's scorecards.
pub(crate) fn history_copied(from: &str, to: &str) {
    let mut h = read_history(from);
    if h.is_empty() { return; }
    for e in &mut h { e.0 = to.to_string(); }
    write_history(to, &h);
}
/// A design file went to the trash: its scorecards go too.
pub(crate) fn history_deleted(path: &str) { write_history(path, &Vec::new()); }

// ---------------------------------------------------------------------------------------------------------
// Caches.

/// Suggestions of one revision, by kind: `None` = not computed yet, `Some(None)` = nothing to suggest.
type SuggestSlots = Vec<(SuggestKind, Option<Option<Suggestion>>)>;
/// Arcs and blind sectors of one built design by (slot, sensor).
type SectorMap = std::collections::BTreeMap<(u16, bool), Rc<Vec<ArcSector>>>;
type Refit = Rc<(content::stats::RefitDiff, content::stats::RefitCost)>;

#[derive(Default)]
pub(crate) struct AnalysisCache {
    validation: std::cell::RefCell<Option<(u64, Rc<Vec<Issue>>)>>,
    /// Suggestions of a revision, one kind at a time (`None` = not computed yet), and whether the UI asked.
    suggestions: std::cell::RefCell<Option<(u64, SuggestSlots)>>,
    suggest_wanted: std::cell::Cell<bool>,
    /// Arcs and blind sectors, and the balance overlay, by the revision of the built design.
    arcs: std::cell::RefCell<Option<(u64, SectorMap)>>,
    balance: std::cell::RefCell<Option<(u64, VarDictionary)>>,
    refit: std::cell::RefCell<Option<(u64, Refit)>>,
    /// The scorecard last filed into history (design hash).
    filed: Option<ContentHash>,
    /// History of the design file under edit, as read.
    history: std::cell::RefCell<Option<(String, Rc<History>)>>,
}

// ---------------------------------------------------------------------------------------------------------
// Engine side.

fn gs(s: &str) -> GString { GString::from(s) }

/// `ArcDict`.
fn arc_dict(a: &ArcSector) -> VarDictionary { d! { "from_deg" => a.from_deg, "to_deg" => a.to_deg, "blocked" => a.blocked, "range_m" => if a.range_m.is_finite() { a.range_m } else { 0.0 } } }

fn f32s(v: &[f64]) -> PackedFloat32Array { v.iter().map(|x| if x.is_finite() { *x as f32 } else { 0.0 }).collect() }
fn fin(x: f64) -> f64 { if x.is_finite() { x } else { 0.0 } }

/// `GaugeStripDict`.
fn gauge_strip_dict(g: &GaugeStrip, stale: &[&str], rev: u64, beam_m: f64, measured: bool, cat: &Catalogue) -> VarDictionary {
    let st = &cat.tuning.stability;
    let f = &g.float;
    let gz: PackedVector2Array = f.gz.iter().map(|(h, a)| Vector2::new(fin(*h) as f32, fin(*a) as f32)).collect();
    let float = d! {
        "displacement_t" => fin(f.displacement_t), "cap_t" => fin(f.cap_t), "reserve" => fin(f.reserve), "draft_m" => fin(f.draft_m),
        "heel_deg" => fin(f.heel_deg), "trim_deg" => fin(f.trim_deg), "gm_m" => fin(f.gm_m), "gm_over_beam" => fin(f.gm_over_beam), "gz" => gz,
        "range_of_stability_deg" => fin(f.range_of_stability_deg), "gm_caution_low" => fin(st.gm_caution_low * beam_m), "gm_caution_high" => fin(st.gm_caution_high * beam_m),
        "check" => f.check.to_gd(),
    };
    let speed = d! { "top_speed_kn" => fin(g.speed.top_speed_kn), "turning_circle_m" => fin(g.speed.turning_circle_m), "check" => g.speed.check.to_gd() };
    let p = &g.power;
    let power = d! { "generation_kw" => fin(p.generation_kw), "cruise_demand_kw" => fin(p.cruise_demand_kw), "combat_demand_kw" => fin(p.combat_demand_kw), "battery_kwh" => fin(p.battery_kwh), "check" => p.check.to_gd() };
    let e = &g.endurance;
    let endurance = d! { "cruise_kn" => fin(e.cruise_kn), "range_km" => fin(e.range_km), "fuel_l" => fin(e.fuel_l), "check" => e.check.to_gd() };
    let ranges: VarArray = g.punch.effective_range_m.iter().map(|(s, r)| d! { "slot" => s.0 as i64, "range_m" => fin(*r) }.to_variant()).collect();
    let punch = d! { "weight_of_fire_kg_per_min" => fin(g.punch.weight_of_fire_kg_per_min), "ranges" => ranges, "check" => g.punch.check.to_gd() };
    let s = &g.sight;
    let coverage: PackedFloat32Array = (0..36).map(|i| s.coverage.get(i).copied().filter(|v| v.is_finite()).unwrap_or(0.0)).collect();
    let sight = d! { "antenna_m" => fin(s.antenna_m), "horizon_km" => f32s(&s.horizon_km), "radar_km" => f32s(&s.radar_km), "visual_km" => f32s(&s.visual_km), "coverage" => coverage, "check" => s.check.to_gd() };
    let r = &g.resilience;
    let worst: VarArray = r.worst.iter().map(|c| d! { "a" => c.a.0 as i64, "b" => c.b.map_or(-1, |b| b.0 as i64), "heel_deg" => fin(c.heel_deg), "trim_deg" => fin(c.trim_deg), "survives" => c.survives }.to_variant()).collect();
    let resilience = d! { "one_flooded" => fin(r.one_flooded_survival), "two_flooded" => fin(r.two_flooded_survival), "worst" => worst, "systems_surviving_hit" => fin(r.systems_surviving_hit), "check" => r.check.to_gd() };
    let c = &g.complexity;
    let complexity = d! { "points" => fin(c.points), "caution_at" => fin(c.caution_at), "pvp_cap" => fin(c.pvp_cap), "check" => c.check.to_gd(), "measured" => measured };
    let stale: PackedStringArray = stale.iter().map(|s| GString::from(*s)).collect();
    d! { "float" => float, "speed" => speed, "power" => power, "endurance" => endurance, "punch" => punch, "sight" => sight, "resilience" => resilience, "complexity" => complexity, "stale" => stale, "rev" => rev as i64 }
}

/// `TagResultDict`.
fn tag_result_dict(t: &sim_systems::editor::fitcheck::TagResult) -> VarDictionary {
    let (msg, unit) = tag_msg(&t.tag);
    d! { "text" => msg_dict(&msg), "pass" => t.pass, "value" => fin(t.value), "limit" => fin(t.limit), "unit" => StringName::from(crate::text::unit_name(unit)) }
}
/// `FitResultDict`.
pub(crate) fn fit_result_dict(f: Option<&sim_systems::editor::fitcheck::FitResult>, state: JobState) -> VarDictionary {
    let def = sim_systems::editor::fitcheck::FitResult::default();
    let f = f.unwrap_or(&def);
    let tags: VarArray = f.tags.iter().map(|t| tag_result_dict(t).to_variant()).collect();
    let detect: VarArray = f.detect_km.iter().map(|(c, we, they)| d! { "class_id" => StringName::from(c.as_str()), "we_detect_km" => fin(*we), "they_detect_km" => fin(*they) }.to_variant()).collect();
    d! {
        "state" => state.to_gd(), "passes" => f.passes() && (!f.tags.is_empty() || state == JobState::Done), "tags" => tags,
        "top_speed_kn" => fin(f.top_speed_kn), "time_to_turn_180_s" => fin(f.time_to_turn_180_s), "range_km" => fin(f.range_km), "detect" => detect, "simulated" => f.simulated,
        "seakeeping" => seakeeping_dict(f.seakeeping.as_ref()),
    }
}
/// `SeakeepingDict` (`editor::fitcheck::Seakeeping`), or nil when the sweep did not run.
fn seakeeping_dict(s: Option<&sim_systems::editor::fitcheck::Seakeeping>) -> Variant {
    let Some(s) = s else { return Variant::nil() };
    let worst = s.worst().copied().unwrap_or_default();
    d! {
        "sea_state" => s.sea_state.0 as i64, "complete" => s.complete(), "rolls_over" => s.rolls_over(), "safe" => s.safe(),
        "worst_heel_deg" => fin(worst.peak_heel_deg), "worst_from_deg" => fin(worst.from_deg), "worst_rolled_over" => worst.rolled_over,
        "short_range" => s.short_range(), "range_deg" => fin(s.range_deg), "runs" => s.runs.len() as i64, "directions" => sim_systems::editor::fitcheck::SEA_DIRECTIONS as i64,
    }.to_variant()
}
/// `RefitDiffDict`.
pub(crate) fn refit_diff_dict(diff: &content::stats::RefitDiff, cost: &content::stats::RefitCost, budget: i64, cat: &Catalogue) -> VarDictionary {
    use content::stats::PartChange;
    let cell = |c: &content::cell::CellPos| Vector3i::new(c.x as i32, c.y as i32, c.z as i32);
    let name = |p: &content::ids::PartId| StringName::from(cat.part(p).map_or(p.as_str(), |d| d.name.as_str()));
    let changes: VarArray = diff.changes.iter().map(|c| match c {
        PartChange::Added(p) => d! { "kind" => StringName::from("added"), "part" => StringName::from(p.part.as_str()), "name" => name(&p.part), "cell" => cell(&p.pos), "from_cell" => Variant::nil() },
        PartChange::Removed(p) => d! { "kind" => StringName::from("removed"), "part" => StringName::from(p.part.as_str()), "name" => name(&p.part), "cell" => cell(&p.pos), "from_cell" => Variant::nil() },
        PartChange::Moved { before, after } => d! { "kind" => StringName::from("moved"), "part" => StringName::from(after.part.as_str()), "name" => name(&after.part), "cell" => cell(&after.pos), "from_cell" => cell(&before.pos) },
    }.to_variant()).collect();
    let mut store = VarDictionary::new();
    for (p, n) in &cost.from_store { store.set(&StringName::from(p.as_str()).to_variant(), &(*n as i64).to_variant()); }
    d! {
        "changes" => changes, "structure_delta_t" => fin(diff.structure_delta_t), "salvage" => cost.salvage as i64, "hours" => cost.hours as i64,
        "from_store" => store, "affordable" => budget < 0 || (cost.salvage as i64) <= budget,
    }
}

impl BpEditor {
    /// The newest built design whose hull, cells and parts are those of the design under edit.
    /// Returns the design with the revision it was built for.
    pub(crate) fn structural_design(&self) -> Option<(u64, Arc<ShipDesign>)> {
        let m = self.model.as_ref()?;
        let (rev, d) = m.design.clone()?;
        let b = d.blueprint();
        (b.parts == m.blueprint.parts && b.cells == m.blueprint.cells && b.hull == m.blueprint.hull && b.kits == m.blueprint.kits).then_some((rev, d))
    }

    /// Complexity points a trial measured for a design hash, if any.
    pub(crate) fn measured_complexity(&self, hash: &ContentHash) -> Option<f64> {
        if let Some((_, c)) = self.state.results.scorecard.as_ref().filter(|(_, c)| &c.design == hash) { return Some(c.complexity_points); }
        self.state.analysis.history.borrow().as_ref().and_then(|(_, h)| h.iter().rev().find(|e| &e.3.design == hash).map(|e| e.3.complexity_points))
    }

    /// Validation issues of the design under edit, cached per revision.
    pub(crate) fn validation_issues(&self) -> Rc<Vec<Issue>> {
        let Some(m) = self.model.as_ref() else { return Rc::new(Vec::new()) };
        if let Some((r, v)) = self.state.analysis.validation.borrow().as_ref() {
            // Mid-drag the last answer stands (a full check of a 90 m hull takes several milliseconds); the
            // revision the drag ends on is checked.
            if *r == m.rev || ((self.state.dragging || m.group.is_some()) && *r >= self.state.first_rev) { return v.clone(); }
        }
        let cat = crate::services::services().catalogue();
        // Complexity is measured by the battery on each save (L161): the design's last measured value stands
        // until the next one. Only rulesets with a cap read it.
        let complexity = (m.context.ruleset.complexity_per_vessel > 0.0).then(|| self.state.results.scorecard.as_ref().map(|(_, c)| c.complexity_points)).flatten();
        let ctx = content::validate::ValidationContext { ruleset: &m.context.ruleset, available: m.context.available.as_deref().map(|v| v.as_slice()), enemy_class: m.context.mode == EditorMode::EnemyClass, complexity };
        let mut issues = content::validate::validate_blueprint(&m.blueprint, &cat, &ctx).issues;
        let max = super::max_length_m(m.context.mode, &m.context.ruleset);
        let length = super::design_length_m(&m.blueprint);
        if length > max + 1e-9 && !issues.iter().any(|i| matches!(i, Issue::TooLong { .. })) { issues.insert(0, Issue::TooLong { length_m: length, max_m: max }); }
        let v = Rc::new(issues);
        *self.state.analysis.validation.borrow_mut() = Some((m.rev, v.clone()));
        v
    }

    /// The refit of the design under edit against the fielded hull (campaign), cached per revision.
    fn refit(&self) -> Option<Refit> {
        let m = self.model.as_ref()?;
        if m.context.mode != EditorMode::Campaign { return None; }
        let fielded = m.context.fielded.as_ref()?;
        if let Some((r, v)) = self.state.analysis.refit.borrow().as_ref() { if *r == m.rev { return Some(v.clone()); } }
        let cat = crate::services::services().catalogue();
        let diff = content::stats::diff_blueprints(fielded, &m.blueprint, &cat);
        let cost = content::stats::refit_cost(&diff, &cat, &m.context.parts_store);
        let v = Rc::new((diff, cost));
        *self.state.analysis.refit.borrow_mut() = Some((m.rev, v.clone()));
        Some(v)
    }

    /// Suggestions of the current revision as far as they are computed, from its own built design only.
    /// `None` until that design is built. Asking marks them wanted: `analysis_frame` fills one kind a frame.
    fn suggestion_list(&self) -> Option<SuggestSlots> {
        let m = self.model.as_ref()?;
        self.current_design()?;
        let mut cache = self.state.analysis.suggestions.borrow_mut();
        if cache.as_ref().is_none_or(|(r, _)| *r != m.rev) { *cache = Some((m.rev, SuggestKind::ALL.iter().map(|k| (*k, None)).collect())); }
        let list = cache.as_ref().map(|(_, l)| l.clone())?;
        if list.iter().any(|(_, s)| s.is_none()) { self.state.analysis.suggest_wanted.set(true); }
        Some(list)
    }
    /// One suggestion of the current revision, computed now if it is not yet.
    fn suggestion(&self, kind: SuggestKind) -> Option<Suggestion> {
        let m = self.model.as_ref()?;
        let design = self.current_design()?;
        if let Some(s) = self.suggestion_list()?.into_iter().find(|(k, _)| *k == kind).and_then(|(_, s)| s) { return s; }
        let cat = crate::services::services().catalogue();
        let s = sim_systems::editor::suggest::suggest(kind, &m.blueprint, &design, &cat);
        if let Some((r, l)) = self.state.analysis.suggestions.borrow_mut().as_mut() {
            if *r == m.rev { if let Some(e) = l.iter_mut().find(|(k, _)| *k == kind) { e.1 = Some(s.clone()); } }
        }
        s
    }

    /// Notes of the current revision.
    fn current_notes(&self) -> Option<&[ShipNote]> {
        let m = self.model.as_ref()?;
        (m.notes.0 == m.rev).then_some(m.notes.1.as_slice())
    }

    /// Show a previewed edit as the ghost.
    fn show_preview(&mut self, next: Blueprint) -> bool {
        let Some(m) = self.model.as_mut() else { return false };
        m.ghost = Some(super::Ghost::Edit(Box::new(next)));
        self.base_mut().emit_signal("ghost_changed", &[]);
        true
    }
    /// Accept an edit as one undo step; the preview goes.
    fn accept_edit(&mut self, edit: &BlueprintEdit, label: &str) -> bool {
        let had_preview = self.model.as_ref().is_some_and(|m| matches!(m.ghost, Some(super::Ghost::Edit(_))));
        let ok = self.commit_edit(edit, label);
        if let Some(m) = self.model.as_mut() { if matches!(m.ghost, Some(super::Ghost::Edit(_))) { m.ghost = None; } }
        if ok || had_preview { self.base_mut().emit_signal("ghost_changed", &[]); }
        if ok { self.base_mut().emit_signal("control_changed", &[]); }
        ok
    }

    /// Arcs or blind sectors of one slot, cached per design.
    fn sectors(&self, slot: i64, sensor: bool) -> Rc<Vec<ArcSector>> {
        let Some(s) = crate::convert::slot_from_gd(slot) else { return Rc::new(Vec::new()) };
        let Some((key, design)) = self.structural_design() else { return Rc::new(Vec::new()) };
        {
            let mut cache = self.state.analysis.arcs.borrow_mut();
            if cache.as_ref().is_none_or(|(k, _)| *k != key) { *cache = Some((key, Default::default())); }
            if let Some(v) = cache.as_ref().and_then(|(_, m)| m.get(&(s.0, sensor))) { return v.clone(); }
        }
        let cat = crate::services::services().catalogue();
        let v = Rc::new(if sensor { sim_systems::editor::gauges::blind_sectors(&design, s, &cat) } else { sim_systems::editor::gauges::weapon_arcs(&design, s, &cat) });
        if let Some((_, m)) = self.state.analysis.arcs.borrow_mut().as_mut() { m.insert((s.0, sensor), v.clone()); }
        v
    }

    /// The history of the design file under edit (or of `path`).
    fn history(&self, path: &str) -> Rc<History> {
        if path.is_empty() { return Rc::new(Vec::new()); }
        if let Some((p, h)) = self.state.analysis.history.borrow().as_ref() { if p == path { return h.clone(); } }
        let h = Rc::new(read_history(path));
        *self.state.analysis.history.borrow_mut() = Some((path.to_string(), h.clone()));
        h
    }

    /// Per-frame: compute the next wanted suggestion (each takes a few milliseconds on a 90 m hull, so one a
    /// frame) and announce the set with `notes_changed` when it is complete; file a new battery result into
    /// its design file's history.
    pub(crate) fn analysis_frame(&mut self) {
        if self.state.analysis.suggest_wanted.get() && !self.state.dragging && self.model.as_ref().is_some_and(|m| m.group.is_none()) {
            let next = self.suggestion_list().and_then(|l| l.iter().find(|(_, s)| s.is_none()).map(|(k, _)| *k));
            match next {
                Some(k) => {
                    let _ = self.suggestion(k);
                    if self.suggestion_list().is_some_and(|l| l.iter().all(|(_, s)| s.is_some())) {
                        self.state.analysis.suggest_wanted.set(false);
                        self.base_mut().emit_signal("notes_changed", &[]);
                    }
                }
                None => self.state.analysis.suggest_wanted.set(false),
            }
        }
        let Some((_, card)) = self.state.results.scorecard.as_ref() else { return };
        if self.state.analysis.filed == Some(card.design) { return; }
        let card = card.clone();
        self.state.analysis.filed = Some(card.design);
        let saved = crate::services::services().design.borrow().clone();
        let Some(cur) = saved.filter(|c| c.hash == card.design && c.path.starts_with(super::io::ROOT)) else { return };
        let mut h = read_history(&cur.path);
        if history_push(&mut h, &cur.path, super::io::now_unix(), &cur.blueprint.header.name, &card) { write_history(&cur.path, &h); }
        *self.state.analysis.history.borrow_mut() = Some((cur.path.clone(), Rc::new(h)));
    }

    /// `ScorecardDict`.
    fn scorecard_dict(&self, card: &Scorecard, saved_unix: i64, label: &str) -> VarDictionary {
        let f = &card.fight;
        let opponents: VarArray = card.opponents.iter().map(|c| StringName::from(c.as_str()).to_variant()).collect();
        let drift: VarArray = card.drift.iter().map(|d| d! { "gauge" => gs(&d.gauge), "predicted" => fin(d.predicted), "simulated" => fin(d.simulated) }.to_variant()).collect();
        d! {
            "hash" => crate::convert::hash_hex(&card.design), "turning_circle_m" => fin(card.turning_circle_m), "time_to_90pct_speed_s" => fin(card.time_to_90pct_speed_s),
            "top_speed_kn" => fin(card.top_speed_kn), "heel_recovery_s" => fin(card.heel_recovery_s), "peak_heel_deg" => fin(card.peak_heel_deg), "score" => fin(f.score),
            "per_seed" => f32s(&f.per_seed), "wins" => f.wins as i64, "hull_left" => fin(f.mean_hull_fraction_left), "rounds_per_kill" => fin(f.rounds_per_kill),
            "first_salvo_hit_rate" => fin(f.first_salvo_hit_rate), "complexity_points" => fin(card.complexity_points), "short_trial" => card.short_trial,
            "opponents" => opponents, "drift" => drift, "saved_unix" => saved_unix, "label" => gs(label),
        }
    }

    /// The saved design the trial battery runs on, with its revision.
    fn saved_for_trials(&self) -> Option<(u64, Arc<Blueprint>)> {
        let m = self.model.as_ref()?;
        let cur = crate::services::services().design.borrow().clone()?;
        let rev = if m.is_dirty() { m.saved_rev } else { m.rev };
        Some((rev, cur.blueprint.clone()))
    }

    /// The briefing a campaign dock opened with.
    fn brief(&self) -> Option<Arc<sim_systems::editor::fitcheck::FitBrief>> {
        let m = self.model.as_ref()?;
        if m.context.mode != EditorMode::Campaign { return None; }
        m.context.brief.clone()
    }
    /// The design under edit as the worker takes it.
    fn current_arc(&self) -> Option<(u64, Arc<Blueprint>)> {
        let m = self.model.as_ref()?;
        Some(match self.state.current.as_ref().filter(|(r, _)| *r == m.rev) { Some((r, a)) => (*r, a.clone()), None => (m.rev, Arc::new(m.blueprint.clone())) })
    }
}

#[godot_api(secondary)]
impl BpEditor {
    /// Returns `GaugeStripDict`. Announced by `gauges_changed`. Gauges named in `stale` still show their
    /// previous values and are drawn with the stale glyph and the word for "Updating" (L185).
    #[func]
    fn gauges(&self) -> VarDictionary {
        let Some(m) = self.model.as_ref() else { return VarDictionary::new() };
        let Some((rev, strip)) = m.gauges.as_ref() else { return VarDictionary::new() };
        let cat = crate::services::services().catalogue();
        let beam = self.stats().map_or(0.0, |s| s.beam_m);
        let measured = self.state.results.scorecard.is_some();
        let trials = self.jobs.get("trials") == Some(&JobState::Running);
        let stale = stale_gauges(self.state.results.fast_rev, self.state.results.slow_rev, m.rev, trials);
        gauge_strip_dict(strip, &stale, *rev, beam, measured, &cat)
    }
    /// Gauge latency over the last 50 edits: `{ "p95_ms": float, "split_strip": bool }` (the split strip
    /// switches itself on above 100 ms and off after 50 edits under 70 ms, L185).
    #[func]
    fn gauge_latency(&self) -> VarDictionary {
        let shipped = crate::services::services().settings.borrow().bool("editor.split_gauge_strip");
        d! { "p95_ms" => self.state.latency.p95(), "split_strip" => self.state.latency.split_strip || shipped }
    }
    /// Ship's Notes, highest tier first: `Array[NoteDict]`. Announced by `notes_changed`.
    #[func]
    fn notes(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        let Some(m) = self.model.as_ref() else { return out };
        let cat = crate::services::services().catalogue();
        let lk = crate::text::EngineLookup::new();
        let current = m.notes.0 == m.rev;
        for (i, n) in m.notes.1.iter().enumerate() {
            let fix = n.fix.as_ref().filter(|_| current);
            out.push(&d! {
                "index" => i as i64, "id" => StringName::from(sim_systems::tutorial::note_id(&n.kind)), "tier" => n.tier.to_gd(), "text" => msg_dict(&named_parts(&n.text, &m.blueprint, &cat, &lk)),
                "part" => n.part.map_or(-1, |p| p.0 as i64), "affected_parts" => n.affected_parts as i64, "has_fix" => fix.is_some(),
                "flips_tag" => n.flips_tag && fix.is_some(), "score_effect" => n.score_effect.filter(|v| v.is_finite()).unwrap_or(0.0),
                "fix_label" => fix.map_or_else(empty_msg, |f| msg_dict(&fix_label(f, &m.blueprint, &cat, &lk))),
            });
        }
        out
    }
    /// Preview a note's Fix as a ghost (`BpEditorView` draws it). Returns false when the note has no Fix.
    #[func]
    fn note_preview(&mut self, index: i64) -> bool {
        let Some(edit) = self.current_notes().and_then(|n| usize::try_from(index).ok().and_then(|i| n.get(i))).and_then(|n| n.fix.clone()) else { return false };
        let Some(m) = self.model.as_ref() else { return false };
        let cat = crate::services::services().catalogue();
        let Some(next) = sim_systems::editor::notes::apply_edit(&m.blueprint, &edit, &cat) else { return false };
        self.show_preview(next)
    }
    /// Accept the Fix (one undo step). Returns false when it no longer applies.
    #[func]
    fn note_apply_fix(&mut self, index: i64) -> bool {
        let Some(note) = self.current_notes().and_then(|n| usize::try_from(index).ok().and_then(|i| n.get(i))).filter(|n| n.fix.is_some()).cloned() else { return false };
        let Some(edit) = note.fix.as_ref() else { return false };
        let applied = self.accept_edit(edit, "dock.undo.fix");
        // The tutorial's Fix step waits on this (`tutorial::Trigger::NoteFixed`).
        if applied { crate::services::services().tutorial_signal(sim_systems::tutorial::Signal::NoteFixApplied { note: sim_systems::tutorial::note_id(&note.kind).to_string() }); }
        applied
    }
    /// Drop any Fix, Suggest or Quartermaster preview.
    #[func]
    fn preview_clear(&mut self) {
        let Some(m) = self.model.as_mut() else { return };
        if matches!(m.ghost, Some(super::Ghost::Edit(_))) {
            m.ghost = None;
            self.base_mut().emit_signal("ghost_changed", &[]);
        }
    }
    /// Suggest buttons (L210): `Array[SuggestionDict]`, one per `Bp.SUGGEST_*`.
    #[func]
    fn suggestions(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        let list = self.suggestion_list();
        let cat = crate::services::services().catalogue();
        let lk = crate::text::EngineLookup::new();
        for k in SuggestKind::ALL {
            let s = list.as_ref().and_then(|l| l.iter().find(|(kind, _)| *kind == k).map(|(_, s)| s.clone()));
            let (available, text) = match (s, self.model.as_ref()) {
                (Some(Some(Some(s))), Some(m)) => (true, named_parts(&s.text, &m.blueprint, &cat, &lk)),
                (Some(Some(None)), _) => (false, Msg::new("dock.suggest.matches")),
                _ => (false, Msg::new("dock.suggest.waiting")),
            };
            out.push(&d! { "kind" => k.to_gd(), "available" => available, "text" => msg_dict(&text) });
        }
        out
    }
    /// Preview a suggestion as a ghost. Returns false when there is nothing to suggest.
    #[func]
    fn suggest_preview(&mut self, kind: i64) -> bool {
        let Some(k) = SuggestKind::from_gd(kind) else { return false };
        let Some(s) = self.suggestion(k) else { return false };
        let Some(m) = self.model.as_ref() else { return false };
        let cat = crate::services::services().catalogue();
        let Some(next) = sim_systems::editor::notes::apply_edit(&m.blueprint, &s.edit, &cat) else { return false };
        self.show_preview(next)
    }
    #[func]
    fn suggest_apply(&mut self, kind: i64) -> bool {
        let Some(k) = SuggestKind::from_gd(kind) else { return false };
        let Some(s) = self.suggestion(k) else { return false };
        self.accept_edit(&s.edit, "dock.undo.suggest")
    }
    /// Firing arcs of one mount on the water: `Array[ArcDict]`; blocked sectors are hatched in
    /// `alert.warning` by the overlay.
    #[func]
    fn arcs(&self, slot: i64) -> Array<VarDictionary> {
        let mut out = Array::new();
        for a in self.sectors(slot, false).iter() { out.push(&arc_dict(a)); }
        out
    }
    /// Blind sectors of one sensor: `Array[ArcDict]`.
    #[func]
    fn blind_sectors(&self, slot: i64) -> Array<VarDictionary> {
        let mut out = Array::new();
        for a in self.sectors(slot, true).iter() { out.push(&arc_dict(a)); }
        out
    }
    /// Centre of gravity, centre of buoyancy and waterline for the overlay: `{ "cg": Vector3, "cb": Vector3,
    /// "waterline_m": float, "heel_deg": float, "trim_deg": float }` (hull frame, metres).
    #[func]
    fn balance(&self) -> VarDictionary {
        let empty = || d! { "cg" => Vector3::ZERO, "cb" => Vector3::ZERO, "waterline_m" => 0.0, "heel_deg" => 0.0, "trim_deg" => 0.0 };
        let (Some(m), Some((key, design))) = (self.model.as_ref(), self.structural_design()) else { return empty() };
        if let Some((k, v)) = self.state.analysis.balance.borrow().as_ref() { if *k == key { return v.duplicate_shallow(); } }
        let cat = crate::services::services().catalogue();
        let p = &design.prefab;
        let mass = sim_core::mass::with_fluids(&p.dry_mass, &p.parts, &p.compartments, &cat);
        let eq = sim_core::hydro::equilibrium(&p.blocks, &mass, &[], &p.compartments, &cat.tuning.buoyancy);
        let keel = m.blueprint.cells.0.iter().filter(|r| r.runs.iter().any(|run| !run.cell.is_empty() && run.len > 0)).map(|r| r.y).min().unwrap_or(0) as f64 * content::cell::CELL_M;
        let v3 = |v: glam::DVec3| if v.is_finite() { Vector3::new(v.x as f32, v.y as f32, v.z as f32) } else { Vector3::ZERO };
        let dict = d! {
            "cg" => v3(mass.com), "cb" => v3(eq.cb), "waterline_m" => fin(keel + eq.draft_m),
            "heel_deg" => fin(eq.heel_rad.to_degrees()), "trim_deg" => fin(eq.trim_rad.to_degrees()),
        };
        *self.state.analysis.balance.borrow_mut() = Some((key, dict.duplicate_shallow()));
        dict
    }

    // ---- Trials (L227-229) ----

    /// The latest `ScorecardDict` for the saved design, or `null` before the first trial finishes.
    #[func]
    fn scorecard(&self) -> Variant {
        let Some((_, card)) = self.state.results.scorecard.as_ref() else { return Variant::nil() };
        let path = self.model.as_ref().map(|m| m.path.clone()).unwrap_or_default();
        let h = self.history(&path);
        let entry = h.iter().rev().find(|e| e.3.design == card.design);
        let (when, label) = entry.map_or_else(|| (super::io::now_unix(), self.model.as_ref().map(|m| m.blueprint.header.name.clone()).unwrap_or_default()), |e| (e.1, e.2.clone()));
        self.scorecard_dict(card, when, &label).to_variant()
    }
    /// Earlier scorecards of this design file, newest first: `Array[ScorecardDict]`.
    #[func]
    fn scorecard_history(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        let Some(m) = self.model.as_ref() else { return out };
        let h = self.history(&m.path);
        let newest = self.state.results.scorecard.as_ref().map(|(_, c)| c.design);
        let mut list: Vec<&(String, i64, String, Scorecard)> = h.iter().collect();
        // The scorecard shown as current is not an earlier one.
        if list.last().is_some_and(|e| Some(e.3.design) == newest) { list.pop(); }
        for e in list.into_iter().rev() { out.push(&self.scorecard_dict(&e.3, e.1, &e.2)); }
        out
    }
    /// Compare two scorecards by hash: `Array[Dictionary]` with `metric: StringName`, `before: float`,
    /// `after: float`, `unit: StringName`, `better: int` (-1 worse, 0 same, 1 better).
    #[func]
    fn compare(&self, before_hash: GString, after_hash: GString) -> Array<VarDictionary> {
        let mut out = Array::new();
        let path = self.model.as_ref().map(|m| m.path.clone()).unwrap_or_default();
        let h = self.history(&path);
        let find = |hex: &str| -> Option<Scorecard> {
            let hash = ContentHash::from_hex(hex.trim())?;
            if let Some((_, c)) = self.state.results.scorecard.as_ref().filter(|(_, c)| c.design == hash) { return Some((**c).clone()); }
            h.iter().rev().find(|e| e.3.design == hash).map(|e| e.3.clone())
        };
        let (Some(a), Some(b)) = (find(&before_hash.to_string()), find(&after_hash.to_string())) else { return out };
        for (name, before, after) in sim_systems::editor::trials::compare(&a, &b) {
            let (key, unit, higher) = metric_meta(&name);
            out.push(&d! { "metric" => StringName::from(key.as_str()), "before" => fin(before), "after" => fin(after), "unit" => StringName::from(unit), "better" => better(before, after, higher) });
        }
        out
    }
    /// Run the battery now on the saved design (it also runs on every save).
    #[func]
    fn run_trials(&mut self) {
        self.state.short_trials = SHORT_TRIAL_FALLBACK;
        self.state.note_effects_on = NOTE_EFFECTS_FALLBACK;
        let Some((rev, bp)) = self.saved_for_trials() else { return };
        let spec = if self.state.short_trials { sim_systems::editor::trials::BatterySpec::SHORT } else { sim_systems::editor::trials::BatterySpec::ON_SAVE };
        // The same design again is measured again: let the next result file into history afresh.
        self.state.analysis.filed = None;
        self.submit_job("trials", crate::worker::Job::Trials { rev, blueprint: bp.clone(), spec });
        if self.state.note_effects_on { self.submit_job("note_effects", crate::worker::Job::NoteEffects { rev, blueprint: bp }); }
        self.base_mut().emit_signal("gauges_changed", &[]);
    }

    // ---- Fit Check and Quartermaster (campaign, L801-807) ----

    /// Start a Fit Check against the current briefing (30 s headless run in the forecast sea).
    /// False outside a campaign briefing.
    #[func]
    fn fit_check(&mut self) -> bool {
        let Some(brief) = self.brief() else { return false };
        let Some((rev, bp)) = self.current_arc() else { return false };
        let short = crate::services::services().worker.borrow().as_ref().is_some_and(|w| w.fit_short());
        let seconds = if short { sim_systems::editor::fitcheck::DEFAULT_SECONDS / 2.0 } else { sim_systems::editor::fitcheck::DEFAULT_SECONDS };
        self.submit_job("fit", crate::worker::Job::FitCheck { rev, blueprint: bp, brief, seconds });
        self.base_mut().emit_signal("fit_changed", &[]);
        true
    }
    /// Returns `FitResultDict`; `state` says whether it is current. Announced by `fit_changed`.
    #[func]
    fn fit_result(&self) -> VarDictionary {
        let rev = self.model.as_ref().map_or(0, |m| m.rev);
        let r = self.state.results.fit.as_ref();
        let state = result_state(r.map(|(r, _)| *r), rev, self.jobs.get("fit").copied());
        fit_result_dict(r.map(|(_, f)| f.as_ref()), state)
    }
    /// Ask the Quartermaster for the cheapest legal refit (5 s at most). False outside a campaign briefing.
    #[func]
    fn quartermaster(&mut self) -> bool {
        let Some(brief) = self.brief() else { return false };
        let Some((rev, bp)) = self.current_arc() else { return false };
        let Some(m) = self.model.as_ref() else { return false };
        let job = crate::worker::Job::Quartermaster { rev, blueprint: bp, brief, ruleset: Arc::new(m.context.ruleset.clone()), parts_store: m.context.parts_store.clone(), available: m.context.available.clone() };
        self.submit_job("quartermaster", job);
        self.base_mut().emit_signal("quartermaster_changed", &[]);
        true
    }
    /// Returns `QmResultDict`. Announced by `quartermaster_changed`. "No legal refit found" names the
    /// blocking tag; an illegal refit is never returned.
    #[func]
    fn quartermaster_result(&self) -> VarDictionary {
        use sim_systems::editor::quartermaster::QmResult;
        let rev = self.model.as_ref().map_or(0, |m| m.rev);
        let r = self.state.results.quartermaster.as_ref();
        let state = result_state(r.map(|(r, _)| *r), rev, self.jobs.get("quartermaster").copied());
        let cat = crate::services::services().catalogue();
        let budget = if self.model.as_ref().is_some_and(|m| m.context.mode == EditorMode::Campaign) { self.state.budget_salvage } else { -1 };
        let (kind, diff, fit, blocking) = match r.map(|(_, q)| q.as_ref()) {
            None => (-1i64, Variant::nil(), Variant::nil(), empty_msg()),
            Some(QmResult::AlreadyFits(f)) => (0, Variant::nil(), fit_result_dict(Some(f), state).to_variant(), empty_msg()),
            Some(QmResult::Refit { diff, cost, fit, .. }) => (1, refit_diff_dict(diff, cost, budget, &cat).to_variant(), fit_result_dict(Some(fit), state).to_variant(), empty_msg()),
            Some(QmResult::NoLegalRefit { blocking }) => (2, Variant::nil(), Variant::nil(), msg_dict(&tag_msg(blocking).0)),
        };
        d! { "state" => state.to_gd(), "kind" => kind, "diff" => diff, "fit" => fit, "blocking" => blocking }
    }
    /// Preview the proposed refit as a ghost with its diff.
    #[func]
    fn quartermaster_preview(&mut self) -> bool {
        use sim_systems::editor::quartermaster::QmResult;
        let rev = self.model.as_ref().map_or(0, |m| m.rev);
        let Some(bp) = self.state.results.quartermaster.as_ref().filter(|(r, _)| *r == rev).and_then(|(_, q)| if let QmResult::Refit { blueprint, .. } = q.as_ref() { Some((**blueprint).clone()) } else { None }) else { return false };
        self.show_preview(bp)
    }
    /// Accept the proposal into the design (one undo step).
    #[func]
    fn quartermaster_accept(&mut self) -> bool {
        use sim_systems::editor::quartermaster::QmResult;
        let rev = self.model.as_ref().map_or(0, |m| m.rev);
        let Some(bp) = self.state.results.quartermaster.as_ref().filter(|(r, _)| *r == rev).and_then(|(_, q)| if let QmResult::Refit { blueprint, .. } = q.as_ref() { Some((**blueprint).clone()) } else { None }) else { return false };
        let had_preview = self.model.as_ref().is_some_and(|m| matches!(m.ghost, Some(super::Ghost::Edit(_))));
        let ok = self.commit(bp, "dock.undo.refit");
        if let Some(m) = self.model.as_mut() { if matches!(m.ghost, Some(super::Ghost::Edit(_))) { m.ghost = None; } }
        if ok || had_preview { self.base_mut().emit_signal("ghost_changed", &[]); }
        ok
    }
    /// Cost of the design under edit against the fielded one (campaign): `RefitDiffDict`.
    #[func]
    fn refit_diff(&self) -> VarDictionary {
        let cat = crate::services::services().catalogue();
        let budget = self.state.budget_salvage;
        match self.refit() {
            Some(r) => refit_diff_dict(&r.0, &r.1, budget, &cat),
            None => refit_diff_dict(&Default::default(), &Default::default(), -1, &cat),
        }
    }

    // ---- Validation and launch ----

    /// Problems that make the design illegal here (schema, part legality, ruleset limits, bounds,
    /// connectivity): `Array[IssueDict]`. Empty = legal.
    #[func]
    fn validation(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        if self.model.is_none() { return out; }
        for i in self.validation_issues().iter() { out.push(&crate::convert::issue_dict(i)); }
        if let Some(r) = self.refit() {
            let budget = self.state.budget_salvage;
            if budget >= 0 && r.1.salvage as i64 > budget {
                let msg = Msg::new("dock.issue.cannot_afford").arg(MsgArg::Num { value: r.1.salvage as f64, unit: Unit::Salvage }).arg(MsgArg::Num { value: budget as f64, unit: Unit::Salvage });
                out.push(&d! { "text" => msg_dict(&msg), "fatal" => false, "part" => -1i64, "cell" => Variant::nil() });
            }
        }
        out
    }
    /// State of the Launch button: `{ "can_launch": bool, "open_notes": int, "caution": bool,
    /// "text": MsgDict }`. Open notes add a Caution triangle and a count and never block Launch (L188);
    /// only `validation()` issues do.
    #[func]
    pub(crate) fn launch_state(&self) -> VarDictionary {
        let Some(m) = self.model.as_ref() else {
            return d! { "can_launch" => false, "open_notes" => 0i64, "caution" => false, "text" => msg_dict(&Msg::new("dock.launch.not_open")) };
        };
        let problems = self.validation().len();
        let open = m.notes.1.len() as i64;
        let text = if problems > 0 {
            Msg::new("dock.launch.blocked").arg(MsgArg::Int(problems as i64))
        } else if m.context.mode == EditorMode::Lobby {
            Msg::new("dock.launch.lock_in")
        } else {
            Msg::new("dock.launch.launch")
        };
        d! { "can_launch" => problems == 0, "open_notes" => open, "caution" => open > 0, "text" => msg_dict(&text) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_follow_revisions() {
        assert_eq!(result_state(None, 5, None), JobState::Idle);
        assert_eq!(result_state(Some(5), 5, Some(JobState::Done)), JobState::Done);
        assert_eq!(result_state(Some(4), 5, Some(JobState::Done)), JobState::Stale);
        assert_eq!(result_state(Some(4), 5, Some(JobState::Running)), JobState::Running);
        assert_eq!(result_state(None, 5, Some(JobState::Failed)), JobState::Failed);
        assert_eq!(stale_gauges(5, 5, 5, false), Vec::<&str>::new());
        assert_eq!(stale_gauges(4, 3, 5, false), vec!["float", "speed", "power", "punch", "endurance", "sight", "resilience"]);
        assert_eq!(stale_gauges(5, 4, 5, true), vec!["endurance", "sight", "resilience", "complexity"]);
        assert_eq!(stale_gauges(5, 5, 5, false), Vec::<&str>::new());
    }

    #[test]
    fn compare_knows_which_way_is_better() {
        assert_eq!(better(300.0, 250.0, metric_meta("turning_circle_m").2), 1);
        assert_eq!(better(30.0, 28.0, metric_meta("top_speed_kn").2), -1);
        assert_eq!(better(10.0, 10.0, metric_meta("fight.score").2), 0);
        assert_eq!(better(f64::NAN, 1.0, Some(true)), 0);
        let (key, unit, _) = metric_meta("fight.rounds_per_kill");
        assert_eq!((key.as_str(), unit), ("dock.metric.fight_rounds_per_kill", "rounds"));
        // Every metric the battery compares has a key in the CSV list.
        let a = Scorecard::default();
        for (name, _, _) in sim_systems::editor::trials::compare(&a, &a) {
            let (key, _, higher) = metric_meta(&name);
            assert!(ANALYSIS_KEYS.contains(&key.as_str()), "{key}");
            assert!(higher.is_some(), "{name}");
        }
    }

    #[test]
    fn history_keeps_the_newest_twenty_once_each() {
        let mut h: History = Vec::new();
        let card = |n: u8| Scorecard { design: ContentHash([n; 32]), top_speed_kn: n as f64, ..Default::default() };
        for n in 0..25 { assert!(history_push(&mut h, "user://blueprints/a.bp.ron", n as i64, "A", &card(n))); }
        assert_eq!(h.len(), HISTORY_LEN);
        assert_eq!(h[0].3.design, ContentHash([5; 32]));
        assert!(!history_push(&mut h, "user://blueprints/a.bp.ron", 99, "A", &card(24)), "the same result twice");
        // Re-measuring an older design moves it to the front.
        assert!(history_push(&mut h, "user://blueprints/a.bp.ron", 100, "A", &card(10)));
        assert_eq!(h.len(), HISTORY_LEN - 1 + 1);
        assert_eq!(h.last().unwrap().3.design, ContentHash([10; 32]));
        let text = history_text(&h).unwrap();
        assert_eq!(history_parse(&text), h);
        assert!(history_parse("not ron at all").is_empty());
        assert_ne!(history_file("user://blueprints/a.bp.ron"), history_file("user://blueprints/b.bp.ron"));
    }

    #[test]
    fn every_tag_and_fix_has_a_sentence() {
        let lk = crate::text::tests::CsvLookup::load();
        let cat = content::catalogue::catalogue();
        let bp = content::campaign::load::stock_blueprint("starters/fac22.bp.ron").unwrap();
        let tags = [
            ConstraintTag::DraftLimit { max_m: 2.5 }, ConstraintTag::RcsLimit { max_m2: 400.0 }, ConstraintTag::Range { min_km: 300.0 },
            ConstraintTag::Passengers { count: 6, allowed_losses: 1 }, ConstraintTag::Requires(content::part::PartTag::Crane), ConstraintTag::MinSpeed { kn: 28.0 },
        ];
        for t in &tags {
            let text = crate::text::msg_text(&lk, &tag_msg(t).0);
            assert!(!text.contains("dock.") && !text.contains("notes.") && !text.contains('{'), "{text}");
        }
        let edits = [
            BlueprintEdit::RemovePart(PartSlot(0)), BlueprintEdit::AddBulkhead { x_cell: 3 }, BlueprintEdit::SetSlider { key: "gunnery.aggression".into(), value: 0.7 },
            BlueprintEdit::SetBurstTrigger { range_m: 15_000.0 }, BlueprintEdit::Several(vec![BlueprintEdit::UnpinRoute { index: 0 }, BlueprintEdit::UnpinNode(content::ids::GraphNodeId(1))]),
        ];
        for e in &edits {
            let text = crate::text::msg_text(&lk, &fix_label(e, &bp, cat, &lk));
            assert!(!text.contains("dock.") && !text.contains('{'), "{text}");
        }
    }
}
