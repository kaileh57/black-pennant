//! Unit `gd-settings`. `BpSettings` (autoload `Settings`): client settings and input bindings.
//! DESIGN.md L1710-1723 (accessibility floors) and the settings list after them: UI scale 75 to 200%,
//! palette, reduce motion, blink-off, reduce transparency, reduce flashing, shake, blur and roll sliders,
//! HUD backing opacity, officer-line scale and opacity, bindings; L1719 (everything rebindable per device
//! including Esc; holds can be toggles; no chords); L1739 (first launch reads the OS accessibility flags);
//! L106 (swap the docked edges); L114 (co-op compression as toggle or hold); L241 (trackpad switches);
//! L221-223 (pre-built and progressive splash persist); L1334 (crash-report consent); L2020 (quality
//! overrides in `user://settings.cfg`); L1875 (lightning toggle); L80 (playtest timing log).
//! Simulation-affecting settings are not here: they travel in `content::settings::SimSettings`.
//!
//! Stored as a Godot `ConfigFile` at `user://settings.cfg`, written on change with a 1 s debounce.
//! Keys are `section.name`. The schema below is the single list: the settings screen is built from
//! `schema()`, so adding a setting needs no UI change.
//!
//! # Writers
//! `BpSettings` is the one writer of `Services::settings`. Other Rust units that must persist a value (the
//! quality auto-detect result, L2018) call [`write`], which validates like `set_value`, and whose change
//! signal `BpSettings` emits on its next frame. File layout: one `[section]` per key prefix, `[bindings]`
//! with one `{ "keyboard": PackedStringArray, "gamepad": PackedStringArray }` per overridden action, and
//! `[player] display_name`. Binding events are stored as short codes (`key:82`, `mouse:1`, `joy:0`,
//! `axis:5:1`), never as engine objects, so the file stays readable and safe to load.
use godot::classes::{
    ConfigFile, DisplayServer, FileAccess, InputEvent, InputEventJoypadButton, InputEventJoypadMotion, InputEventKey, InputEventMouseButton, InputMap, Os,
    ProjectSettings,
};
use godot::global::{Error as GdError, JoyAxis, JoyButton, Key, MouseButton};
use godot::obj::{EngineEnum, Singleton};
use godot::prelude::*;
use std::cell::{Cell, RefCell};

/// Value of one setting.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingValue { Bool(bool), Int(i64), Float(f64), Text(String) }

/// One row of the schema.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingDef {
    /// `section.name`, for example `access.ui_scale`.
    pub key: &'static str,
    pub default: SettingValue,
    /// Inclusive range and step for numbers; ignored otherwise.
    pub min: f64,
    pub max: f64,
    pub step: f64,
    /// Choices for an enumerated `Int`: (value, label text key).
    pub options: &'static [(i64, &'static str)],
    /// `""`, `"macos"` or `"windows"`.
    pub platform: &'static str,
    pub needs_restart: bool,
}

const fn b(key: &'static str, default: bool) -> SettingDef { SettingDef { key, default: SettingValue::Bool(default), min: 0.0, max: 1.0, step: 1.0, options: &[], platform: "", needs_restart: false } }
const fn f(key: &'static str, default: f64, min: f64, max: f64, step: f64) -> SettingDef { SettingDef { key, default: SettingValue::Float(default), min, max, step, options: &[], platform: "", needs_restart: false } }
const fn e(key: &'static str, default: i64, options: &'static [(i64, &'static str)]) -> SettingDef { SettingDef { key, default: SettingValue::Int(default), min: 0.0, max: 0.0, step: 1.0, options, platform: "", needs_restart: false } }
/// A whole number in an inclusive range.
const fn i(key: &'static str, default: i64, min: i64, max: i64) -> SettingDef { SettingDef { key, default: SettingValue::Int(default), min: min as f64, max: max as f64, step: 1.0, options: &[], platform: "", needs_restart: false } }
/// Free text, empty by default.
const fn t(key: &'static str) -> SettingDef { SettingDef { key, default: SettingValue::Text(String::new()), min: 0.0, max: 0.0, step: 0.0, options: &[], platform: "", needs_restart: false } }

/// Every client setting. Label and tooltip text keys are `settings.<key>` and `settings.<key>.tip`.
/// Sections: `access`, `hud`, `input`, `editor`, `graphics`, `audio`, `net`, `privacy`, `playtest`.
pub const SCHEMA: &[SettingDef] = &[
    // Accessibility (L1710-1723).
    f("access.ui_scale", 1.0, 0.75, 2.0, 0.05),
    e("access.palette", 0, &[(0, "settings.palette.dusk"), (1, "settings.palette.high_contrast")]),
    b("access.reduce_motion", false),
    b("access.blink_off", false),
    b("access.reduce_transparency", false),
    b("access.reduce_flashing", false),
    f("access.camera_shake", 1.0, 0.0, 1.0, 0.05),
    f("access.blur", 0.0, 0.0, 1.0, 0.05),
    f("access.camera_roll", 1.0, 0.0, 1.0, 0.05),
    f("access.hud_backing_opacity", 1.0, 0.0, 1.0, 0.05),
    f("access.officer_line_scale", 1.0, 1.0, 2.0, 0.1),
    f("access.officer_line_opacity", 1.0, 0.0, 1.0, 0.05),
    b("access.first_launch_done", false),
    // HUD.
    b("hud.swap_docks", false),
    b("hud.show_host_tick", true),
    b("hud.show_performance", false),
    // Input.
    b("input.holds_toggle", false),
    b("input.compression_hold", false),
    f("input.mouse_orbit_speed", 1.0, 0.1, 3.0, 0.05),
    f("input.stick_orbit_speed", 1.0, 0.1, 3.0, 0.05),
    f("input.stick_deadzone", 0.2, 0.05, 0.6, 0.01),
    b("input.invert_orbit_y", false),
    // Editor and trackpad (L235-246).
    e("editor.two_finger_drag", 0, &[(0, "settings.two_finger.orbit"), (1, "settings.two_finger.pan")]),
    b("editor.invert_orbit", false),
    b("editor.invert_pan", false),
    b("editor.invert_zoom", false),
    f("editor.orbit_speed", 1.0, 0.1, 3.0, 0.05),
    f("editor.pan_speed", 1.0, 0.1, 3.0, 0.05),
    f("editor.zoom_speed", 1.0, 0.1, 3.0, 0.05),
    b("editor.prebuilt_splash", false),
    b("editor.progressive_splash", false),
    b("editor.split_gauge_strip", false),
    // Fallback 1 (L270) by default, pending the owner's review: the level 0 hull cannot finish its lesson (no
    // room for the fuel tank, and the kits take Float back to Fail after the beam step), see
    // `crates/sim-systems/data/tutorial.ron`.
    i("editor.tutorial_level", 0, 0, 1_000),
    b("editor.tutorial_finished", false),
    // Graphics (L1975-2035). -1 = automatic.
    e("graphics.tier", -1, &[(-1, "settings.auto"), (0, "settings.tier.low"), (1, "settings.tier.medium"), (2, "settings.tier.high")]),
    e("graphics.window_mode", 0, &[(0, "settings.window.windowed"), (1, "settings.window.borderless"), (2, "settings.window.fullscreen")]),
    b("graphics.vsync", true),
    b("graphics.dynamic_resolution", true),
    e("graphics.msaa", -1, &[(-1, "settings.auto"), (0, "settings.off"), (2, "settings.msaa.2x"), (4, "settings.msaa.4x")]),
    e("graphics.shadow_atlas", -1, &[(-1, "settings.auto"), (2048, "settings.shadow.2048"), (4096, "settings.shadow.4096")]),
    e("graphics.ssao", -1, &[(-1, "settings.auto"), (0, "settings.off"), (1, "settings.low"), (2, "settings.medium")]),
    b("graphics.pcss", false),
    b("graphics.lightning", true),
    b("graphics.render_behind_plot", false),
    // Auto-detect results (L2018: saved to this file). Internal; written by `BpQuality` through [`write`].
    // `detected_tier` is a `Bp.QUALITY_*` value or -1 before the first detect; `detected_machine` is the
    // adapter and driver signature it ran on, so a change of either re-runs it.
    i("graphics.detected_tier", -1, -1, 3),
    t("graphics.detected_machine"),
    b("graphics.detected_smaa", false),
    b("graphics.detected_two_splits", false),
    // Audio.
    f("audio.master", 1.0, 0.0, 1.0, 0.05),
    f("audio.effects", 1.0, 0.0, 1.0, 0.05),
    f("audio.ambience", 1.0, 0.0, 1.0, 0.05),
    f("audio.alerts", 1.0, 0.0, 1.0, 0.05),
    f("audio.radio", 1.0, 0.0, 1.0, 0.05),
    f("audio.interface", 1.0, 0.0, 1.0, 0.05),
    f("audio.menu_theme", 0.7, 0.0, 1.0, 0.05),
    // Network (L994, L1031).
    i("net.port", 47610, 1024, 65535),
    i("net.upload_kbps", 1000, 256, 100_000),
    // Privacy (L1334): -1 not asked, 0 don't send, 1 send.
    e("privacy.crash_reports", -1, &[(0, "settings.privacy.dont_send"), (1, "settings.privacy.send")]),
    // Playtest (L80).
    b("playtest.timing_log", true),
];

/// Flags the settings screen never shows.
const INTERNAL: &[&str] = &[
    "access.first_launch_done", "editor.tutorial_level", "editor.tutorial_finished",
    "graphics.detected_tier", "graphics.detected_machine", "graphics.detected_smaa", "graphics.detected_two_splits",
];

/// Bindings of one action on one device.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Binding { pub action: String, pub keyboard: Vec<String>, pub gamepad: Vec<String> }

/// All client settings in memory. Lives in `Services::settings`. Unknown keys read as their default.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClientSettings {
    pub values: std::collections::BTreeMap<String, SettingValue>,
    /// Overrides of the default input map only; an action missing here uses `project.godot`.
    pub bindings: Vec<Binding>,
    pub display_name: String,
    pub loaded: bool,
}
impl ClientSettings {
    fn def(key: &str) -> Option<&'static SettingDef> { SCHEMA.iter().find(|d| d.key == key) }
    pub fn get(&self, key: &str) -> Option<SettingValue> { self.values.get(key).cloned().or_else(|| Self::def(key).map(|d| d.default.clone())) }
    pub fn bool(&self, key: &str) -> bool { matches!(self.get(key), Some(SettingValue::Bool(true))) }
    pub fn int(&self, key: &str) -> i64 { match self.get(key) { Some(SettingValue::Int(v)) => v, _ => 0 } }
    pub fn float(&self, key: &str) -> f64 { match self.get(key) { Some(SettingValue::Float(v)) => v, Some(SettingValue::Int(v)) => v as f64, _ => 0.0 } }
    pub fn text(&self, key: &str) -> String { match self.get(key) { Some(SettingValue::Text(v)) => v, _ => String::new() } }
}

// ---------------------------------------------------------------------------------------------------------
// Values: validation (pure)
// ---------------------------------------------------------------------------------------------------------

/// The value a definition accepts for `value`: numbers clamped to the range and snapped to the step, enums
/// limited to their options, text trimmed to 256 characters. `None` for a value of the wrong type.
pub fn normalise(def: &SettingDef, value: SettingValue) -> Option<SettingValue> {
    match (&def.default, value) {
        (SettingValue::Bool(_), SettingValue::Bool(v)) => Some(SettingValue::Bool(v)),
        (SettingValue::Int(_), SettingValue::Float(v)) if v.is_finite() && v.fract() == 0.0 && v.abs() < 9.0e15 => normalise(def, SettingValue::Int(v as i64)),
        (SettingValue::Int(_), SettingValue::Int(v)) => {
            // The default is always valid, also when it is a "not chosen yet" value outside the options.
            if !def.options.is_empty() { return (def.options.iter().any(|(o, _)| *o == v) || def.default == SettingValue::Int(v)).then_some(SettingValue::Int(v)); }
            if def.max > def.min { Some(SettingValue::Int(v.clamp(def.min as i64, def.max as i64))) } else { Some(SettingValue::Int(v)) }
        }
        (SettingValue::Float(_), SettingValue::Int(v)) => normalise(def, SettingValue::Float(v as f64)),
        (SettingValue::Float(_), SettingValue::Float(v)) => {
            if !v.is_finite() { return None; }
            let mut v = v.clamp(def.min, def.max);
            if def.step > 0.0 {
                v = def.min + ((v - def.min) / def.step).round() * def.step;
                // Snap away the binary noise of the multiply (0.75 + 5 * 0.05 = 1.0000000000000002).
                v = (v * 1.0e6).round() / 1.0e6;
                v = v.clamp(def.min, def.max);
            }
            Some(SettingValue::Float(v))
        }
        (SettingValue::Text(_), SettingValue::Text(s)) => Some(SettingValue::Text(s.chars().take(256).collect())),
        _ => None,
    }
}

fn section_of(key: &str) -> &str { key.split_once('.').map_or(key, |(s, _)| s) }
fn visible(def: &SettingDef) -> bool {
    let platform_ok = match def.platform { "macos" => cfg!(target_os = "macos"), "windows" => cfg!(windows), _ => true };
    platform_ok && !INTERNAL.contains(&def.key)
}

// ---------------------------------------------------------------------------------------------------------
// Bindings: event codes and the table (pure)
// ---------------------------------------------------------------------------------------------------------

/// Modifier bits of a key or mouse event code.
pub(crate) mod mods { pub const SHIFT: u8 = 1; pub const CTRL: u8 = 2; pub const ALT: u8 = 4; pub const META: u8 = 8; pub const CMD_OR_CTRL: u8 = 16; }

/// One input event reduced to what identifies it. Keys bind by physical position so a layout change
/// does not move WASD (`physical` false only for project defaults written with a keycode).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventCode {
    Key { code: i64, physical: bool, mods: u8 },
    Mouse { button: i64, mods: u8 },
    JoyButton(i64),
    JoyAxis { axis: i64, positive: bool },
}
impl EventCode {
    pub fn is_gamepad(self) -> bool { matches!(self, EventCode::JoyButton(_) | EventCode::JoyAxis { .. }) }
    /// True when the event needs a held modifier: a chord, which a rebind refuses (L1719).
    pub fn is_chord(self) -> bool { matches!(self, EventCode::Key { mods: 1.., .. } | EventCode::Mouse { mods: 1.., .. }) }
    pub fn encode(self) -> String {
        let m = |mods: u8| {
            let mut s = String::new();
            for (bit, name) in [(mods::SHIFT, "shift"), (mods::CTRL, "ctrl"), (mods::ALT, "alt"), (mods::META, "meta"), (mods::CMD_OR_CTRL, "cmd")] { if mods & bit != 0 { s.push('+'); s.push_str(name); } }
            s
        };
        match self {
            EventCode::Key { code, physical: true, mods } => format!("key:{code}{}", m(mods)),
            EventCode::Key { code, physical: false, mods } => format!("keycode:{code}{}", m(mods)),
            EventCode::Mouse { button, mods } => format!("mouse:{button}{}", m(mods)),
            EventCode::JoyButton(b) => format!("joy:{b}"),
            EventCode::JoyAxis { axis, positive } => format!("axis:{axis}:{}", if positive { "1" } else { "-1" }),
        }
    }
    pub fn decode(s: &str) -> Option<EventCode> {
        let mut parts = s.split('+');
        let head = parts.next()?;
        let mut mods = 0u8;
        for p in parts {
            mods |= match p { "shift" => mods::SHIFT, "ctrl" => mods::CTRL, "alt" => mods::ALT, "meta" => mods::META, "cmd" => mods::CMD_OR_CTRL, _ => return None };
        }
        let (kind, rest) = head.split_once(':')?;
        let num = |v: &str| v.parse::<i64>().ok().filter(|n| (0..=0x7fff_ffff).contains(n));
        match kind {
            "key" => Some(EventCode::Key { code: num(rest)?, physical: true, mods }),
            "keycode" => Some(EventCode::Key { code: num(rest)?, physical: false, mods }),
            "mouse" => Some(EventCode::Mouse { button: num(rest)?, mods }),
            "joy" if mods == 0 => Some(EventCode::JoyButton(num(rest)?)),
            "axis" if mods == 0 => {
                let (axis, sign) = rest.split_once(':')?;
                Some(EventCode::JoyAxis { axis: num(axis)?, positive: match sign { "1" => true, "-1" => false, _ => return None } })
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group { Sortie, Editor, Menus }
impl Group {
    fn name(self) -> &'static str { match self { Group::Sortie => "sortie", Group::Editor => "editor", Group::Menus => "menus" } }
    /// Two actions conflict when they share a group, or when one of them is a menu action (live everywhere).
    fn conflicts_with(self, other: Group) -> bool { self == other || self == Group::Menus || other == Group::Menus }
}

/// Every rebindable action in display order: (action, group, is a hold). Mirrors the `[input]` section of
/// `project.godot` without the `ui_*` actions (a test keeps the two in step). Holds are the actions the
/// hold-or-toggle rule and the continuous inputs read while the button is down.
pub const ACTIONS: &[(&str, Group, bool)] = &[
    ("view_switch", Group::Sortie, false), ("pause", Group::Sortie, false), ("time_faster", Group::Sortie, false), ("time_slower", Group::Sortie, false),
    ("helm_ahead", Group::Sortie, true), ("helm_astern", Group::Sortie, true), ("helm_port", Group::Sortie, true), ("helm_starboard", Group::Sortie, true),
    ("take_helm", Group::Sortie, true), ("take_sensors", Group::Sortie, true), ("take_group_1", Group::Sortie, true), ("take_group_2", Group::Sortie, true),
    ("take_group_3", Group::Sortie, true), ("take_group_4", Group::Sortie, true), ("take_group_current", Group::Sortie, true),
    ("group_next", Group::Sortie, false), ("group_prev", Group::Sortie, false), ("fire", Group::Sortie, true), ("designate", Group::Sortie, false),
    ("set_course", Group::Sortie, false), ("cancel", Group::Sortie, false), ("track_next", Group::Sortie, false), ("track_prev", Group::Sortie, false),
    ("look_left", Group::Sortie, true), ("look_right", Group::Sortie, true), ("look_up", Group::Sortie, true), ("look_down", Group::Sortie, true),
    ("camera_orbit", Group::Sortie, true), ("zoom_in", Group::Sortie, false), ("zoom_out", Group::Sortie, false),
    ("alert_acknowledge", Group::Sortie, false), ("alert_silence", Group::Sortie, false), ("emcon_cycle", Group::Sortie, false), ("decoys", Group::Sortie, false),
    ("log_toggle", Group::Sortie, false), ("splash_rewind", Group::Sortie, false), ("splash", Group::Sortie, false),
    ("editor_rotate", Group::Editor, false), ("editor_rotate_back", Group::Editor, false), ("editor_tilt", Group::Editor, false), ("editor_cancel", Group::Editor, false),
    ("editor_delete", Group::Editor, false), ("editor_undo", Group::Editor, false), ("editor_redo", Group::Editor, false), ("editor_copy", Group::Editor, false),
    ("editor_paste", Group::Editor, false), ("editor_duplicate", Group::Editor, false), ("editor_select_all", Group::Editor, false), ("editor_save", Group::Editor, false),
    ("editor_mirror", Group::Editor, false), ("editor_xray", Group::Editor, false), ("editor_frame", Group::Editor, false), ("editor_context", Group::Editor, false),
    ("editor_orbit", Group::Editor, true), ("editor_pan", Group::Editor, true),
    ("menu", Group::Menus, false),
];

fn action_row(action: &str) -> Option<&'static (&'static str, Group, bool)> { ACTIONS.iter().find(|a| a.0 == action) }
/// Editor actions take no gamepad binding: the dry dock is mouse and keyboard only (L58).
fn gamepad_allowed(group: Group) -> bool { group != Group::Editor }

/// Each binding slot past this is refused: the screen shows two per device.
const MAX_PER_DEVICE: usize = 2;

/// The live bindings: defaults from `project.godot` and the player's overrides.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BindingTable {
    /// Per `ACTIONS` row: default keyboard and gamepad events.
    pub defaults: Vec<(Vec<EventCode>, Vec<EventCode>)>,
    /// Per `ACTIONS` row: current keyboard and gamepad events.
    pub current: Vec<(Vec<EventCode>, Vec<EventCode>)>,
}

/// Outcome of a rebind, before it becomes a Dictionary.
#[derive(Clone, Debug, PartialEq)]
pub enum RebindOutcome {
    Bound,
    /// Bound after taking the event from this action.
    Replaced(&'static str),
    Conflict(&'static str),
    Chord,
    WrongDevice,
    NoGamepad,
    Unknown,
}

impl BindingTable {
    pub fn new(defaults: Vec<(Vec<EventCode>, Vec<EventCode>)>) -> Self { Self { current: defaults.clone(), defaults } }
    fn index(action: &str) -> Option<usize> { ACTIONS.iter().position(|a| a.0 == action) }
    fn list(&mut self, i: usize, gamepad: bool) -> &mut Vec<EventCode> { let row = &mut self.current[i]; if gamepad { &mut row.1 } else { &mut row.0 } }

    /// Apply stored overrides; codes that do not decode or sit on the wrong device are dropped.
    pub fn apply_overrides(&mut self, overrides: &[Binding]) {
        for o in overrides {
            let Some(i) = Self::index(&o.action) else { continue };
            let decode = |list: &[String], gamepad: bool| -> Vec<EventCode> {
                let mut out: Vec<EventCode> = list.iter().filter_map(|s| EventCode::decode(s)).filter(|c| c.is_gamepad() == gamepad).collect();
                out.dedup();
                out.truncate(MAX_PER_DEVICE.max(self.defaults[i].0.len()).max(self.defaults[i].1.len()));
                out
            };
            let kb = decode(&o.keyboard, false);
            let pad = if gamepad_allowed(ACTIONS[i].1) { decode(&o.gamepad, true) } else { self.defaults[i].1.clone() };
            self.current[i] = (kb, pad);
        }
    }

    /// The overrides to store: every action whose events differ from the defaults.
    pub fn overrides(&self) -> Vec<Binding> {
        ACTIONS.iter().enumerate().filter(|(i, _)| self.current[*i] != self.defaults[*i]).map(|(i, a)| Binding {
            action: a.0.to_string(),
            keyboard: self.current[i].0.iter().map(|c| c.encode()).collect(),
            gamepad: self.current[i].1.iter().map(|c| c.encode()).collect(),
        }).collect()
    }

    /// Bind `code` as binding `index` of `action` on one device (L1719).
    pub fn rebind(&mut self, action: &str, gamepad: bool, index: usize, code: EventCode, replace: bool) -> RebindOutcome {
        let Some(i) = Self::index(action) else { return RebindOutcome::Unknown };
        let group = ACTIONS[i].1;
        if code.is_gamepad() != gamepad { return RebindOutcome::WrongDevice; }
        if gamepad && !gamepad_allowed(group) { return RebindOutcome::NoGamepad; }
        if code.is_chord() { return RebindOutcome::Chord; }
        if index >= MAX_PER_DEVICE { return RebindOutcome::WrongDevice; }
        let other = ACTIONS.iter().enumerate().find(|(j, a)| *j != i && a.1.conflicts_with(group) && { let row = &self.current[*j]; if gamepad { row.1.contains(&code) } else { row.0.contains(&code) } });
        let mut outcome = RebindOutcome::Bound;
        if let Some((j, a)) = other {
            if !replace { return RebindOutcome::Conflict(a.0); }
            self.list(j, gamepad).retain(|c| *c != code);
            outcome = RebindOutcome::Replaced(a.0);
        }
        let keep = MAX_PER_DEVICE.max(if gamepad { self.defaults[i].1.len() } else { self.defaults[i].0.len() });
        let list = self.list(i, gamepad);
        // Already bound here: it moves into the asked slot rather than appearing twice.
        list.retain(|c| *c != code);
        if index < list.len() { list[index] = code; } else { list.push(code); }
        list.truncate(keep);
        outcome
    }

    pub fn unbind(&mut self, action: &str, gamepad: bool, index: usize) -> bool {
        let Some(i) = Self::index(action) else { return false };
        let list = self.list(i, gamepad);
        if index < list.len() { list.remove(index); true } else { false }
    }

    pub fn reset_device(&mut self, gamepad: bool) {
        for i in 0..ACTIONS.len() {
            if gamepad { self.current[i].1 = self.defaults[i].1.clone(); } else { self.current[i].0 = self.defaults[i].0.clone(); }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Main-thread state shared with other units' Rust code
// ---------------------------------------------------------------------------------------------------------

pub const PATH: &str = "user://settings.cfg";

thread_local! {
    /// Keys changed through [`write`] whose signal `BpSettings` has not yet emitted.
    static PENDING_SIGNALS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// Unsaved changes exist.
    static DIRTY: Cell<bool> = const { Cell::new(false) };
    /// The binding table, built on `reload`.
    static TABLE: RefCell<Option<BindingTable>> = const { RefCell::new(None) };
    /// The player last used a gamepad (for `binding_text`).
    static LAST_GAMEPAD: Cell<bool> = const { Cell::new(false) };
}

/// Set one setting from Rust (main thread). Validates like `BpSettings.set_value`, marks the file for the
/// debounced write and queues the `changed` signal. Returns false for an unknown key or a wrong type.
pub fn write(key: &str, value: SettingValue) -> bool {
    let Some(def) = ClientSettings::def(key) else { return false };
    let Some(value) = normalise(def, value) else { return false };
    let services = crate::services::services();
    let changed = {
        let mut s = services.settings.borrow_mut();
        let old = s.get(key);
        s.values.insert(key.to_string(), value.clone());
        old.as_ref() != Some(&value)
    };
    if changed {
        DIRTY.with(|d| d.set(true));
        PENDING_SIGNALS.with(|p| p.borrow_mut().push(key.to_string()));
    }
    true
}

/// Set the display name (`[player] display_name`) from Rust (main thread) and write the file now. Used by
/// `BpLobby::set_display_name`; the name has already been cleaned. Returns false only when the write failed.
pub fn write_display_name(name: &str) -> bool {
    let services = crate::services::services();
    let changed = {
        let mut s = services.settings.borrow_mut();
        let changed = s.display_name != name;
        if changed { s.display_name = name.to_string(); }
        changed
    };
    if changed { DIRTY.with(|d| d.set(true)); }
    flush()
}

/// Write the file now if anything changed since the last write (called on exit by `BpApp`). Returns false
/// only when a write was needed and failed.
pub fn flush() -> bool { if DIRTY.with(|d| d.get()) { save_file() } else { true } }

fn save_file() -> bool {
    let services = crate::services::services();
    let s = services.settings.borrow().clone();
    let mut cfg = ConfigFile::new_gd();
    for def in SCHEMA {
        let Some((section, name)) = def.key.split_once('.') else { continue };
        if let Some(v) = s.values.get(def.key) { cfg.set_value(section, name, &setting_variant(v)); }
    }
    for binding in &s.bindings {
        let kb: PackedStringArray = binding.keyboard.iter().map(|c| GString::from(c.as_str())).collect();
        let pad: PackedStringArray = binding.gamepad.iter().map(|c| GString::from(c.as_str())).collect();
        let mut d = VarDictionary::new();
        d.set("keyboard", &kb.to_variant());
        d.set("gamepad", &pad.to_variant());
        cfg.set_value("bindings", binding.action.as_str(), &d.to_variant());
    }
    if !s.display_name.is_empty() { cfg.set_value("player", "display_name", &GString::from(s.display_name.as_str()).to_variant()); }
    let ok = cfg.save(PATH) == GdError::OK;
    if ok { DIRTY.with(|d| d.set(false)); } else { godot_warn!("settings: could not write {PATH}"); }
    ok
}

fn setting_variant(v: &SettingValue) -> Variant {
    match v { SettingValue::Bool(b) => b.to_variant(), SettingValue::Int(i) => i.to_variant(), SettingValue::Float(f) => f.to_variant(), SettingValue::Text(s) => GString::from(s.as_str()).to_variant() }
}

fn variant_setting(v: &Variant) -> Option<SettingValue> {
    match v.get_type() {
        VariantType::BOOL => v.try_to::<bool>().ok().map(SettingValue::Bool),
        VariantType::INT => v.try_to::<i64>().ok().map(SettingValue::Int),
        VariantType::FLOAT => v.try_to::<f64>().ok().map(SettingValue::Float),
        VariantType::STRING => v.try_to::<GString>().ok().map(|s| SettingValue::Text(s.to_string())),
        VariantType::STRING_NAME => v.try_to::<StringName>().ok().map(|s| SettingValue::Text(s.to_string())),
        _ => None,
    }
}

// ---- Engine events <-> codes ----

fn code_of(event: &Gd<InputEvent>) -> Option<EventCode> {
    if let Ok(k) = event.clone().try_cast::<InputEventKey>() {
        let physical = k.get_physical_keycode().ord() as i64;
        let (code, is_physical) = if physical != 0 { (physical, true) } else { (k.get_keycode().ord() as i64, false) };
        if code == 0 { return None; }
        // A lone modifier key (Shift as a binding) is not a chord with itself.
        let is_modifier = [Key::SHIFT, Key::CTRL, Key::ALT, Key::META].iter().any(|m| m.ord() as i64 == code);
        let mut m = 0u8;
        if !is_modifier {
            if k.is_command_or_control_autoremap() { m |= mods::CMD_OR_CTRL; } else {
                if k.is_ctrl_pressed() { m |= mods::CTRL; }
                if k.is_meta_pressed() { m |= mods::META; }
            }
            if k.is_shift_pressed() { m |= mods::SHIFT; }
            if k.is_alt_pressed() { m |= mods::ALT; }
        }
        return Some(EventCode::Key { code, physical: is_physical, mods: m });
    }
    if let Ok(mb) = event.clone().try_cast::<InputEventMouseButton>() {
        let mut m = 0u8;
        if mb.is_command_or_control_autoremap() { m |= mods::CMD_OR_CTRL; } else {
            if mb.is_ctrl_pressed() { m |= mods::CTRL; }
            if mb.is_meta_pressed() { m |= mods::META; }
        }
        if mb.is_shift_pressed() { m |= mods::SHIFT; }
        if mb.is_alt_pressed() { m |= mods::ALT; }
        let button = mb.get_button_index().ord() as i64;
        return (button > 0).then_some(EventCode::Mouse { button, mods: m });
    }
    if let Ok(jb) = event.clone().try_cast::<InputEventJoypadButton>() {
        let b = jb.get_button_index().ord() as i64;
        return (b >= 0).then_some(EventCode::JoyButton(b));
    }
    if let Ok(jm) = event.clone().try_cast::<InputEventJoypadMotion>() {
        let a = jm.get_axis().ord() as i64;
        let v = jm.get_axis_value();
        if a < 0 || v == 0.0 || !v.is_finite() { return None; }
        return Some(EventCode::JoyAxis { axis: a, positive: v > 0.0 });
    }
    None
}

fn event_of(code: EventCode) -> Gd<InputEvent> {
    let with_mods = |ev: &mut Gd<godot::classes::InputEventWithModifiers>, m: u8| {
        ev.set_command_or_control_autoremap(m & mods::CMD_OR_CTRL != 0);
        if m & mods::CMD_OR_CTRL == 0 {
            ev.set_ctrl_pressed(m & mods::CTRL != 0);
            ev.set_meta_pressed(m & mods::META != 0);
        }
        ev.set_shift_pressed(m & mods::SHIFT != 0);
        ev.set_alt_pressed(m & mods::ALT != 0);
    };
    match code {
        EventCode::Key { code, physical, mods: m } => {
            let mut k = InputEventKey::new_gd();
            if physical { k.set_physical_keycode(Key::from_ord(code as i32)); } else { k.set_keycode(Key::from_ord(code as i32)); }
            let mut w = k.clone().upcast::<godot::classes::InputEventWithModifiers>();
            with_mods(&mut w, m);
            k.set_device(-1);
            k.upcast()
        }
        EventCode::Mouse { button, mods: m } => {
            let mut mb = InputEventMouseButton::new_gd();
            mb.set_button_index(MouseButton::from_ord(button as i32));
            let mut w = mb.clone().upcast::<godot::classes::InputEventWithModifiers>();
            with_mods(&mut w, m);
            mb.set_device(-1);
            mb.upcast()
        }
        EventCode::JoyButton(b) => { let mut jb = InputEventJoypadButton::new_gd(); jb.set_button_index(JoyButton::from_ord(b as i32)); jb.set_device(-1); jb.upcast() }
        EventCode::JoyAxis { axis, positive } => {
            let mut jm = InputEventJoypadMotion::new_gd();
            jm.set_axis(JoyAxis::from_ord(axis as i32));
            jm.set_axis_value(if positive { 1.0 } else { -1.0 });
            jm.set_device(-1);
            jm.upcast()
        }
    }
}

/// Default events of every action from `ProjectSettings` (`input/<action>`), which `reset` restores.
fn project_defaults() -> Vec<(Vec<EventCode>, Vec<EventCode>)> {
    let ps = ProjectSettings::singleton();
    ACTIONS.iter().map(|(action, group, _)| {
        let setting = ps.get_setting(&format!("input/{action}"));
        let events: VarArray = setting.try_to::<VarDictionary>().ok().and_then(|d| d.get("events")).and_then(|v| v.try_to::<VarArray>().ok()).unwrap_or_default();
        let codes: Vec<EventCode> = events.iter_shared().filter_map(|v| v.try_to::<Gd<InputEvent>>().ok()).filter_map(|e| code_of(&e)).collect();
        let kb: Vec<EventCode> = codes.iter().copied().filter(|c| !c.is_gamepad()).collect();
        let pad: Vec<EventCode> = if gamepad_allowed(*group) { codes.iter().copied().filter(|c| c.is_gamepad()).collect() } else { Vec::new() };
        (kb, pad)
    }).collect()
}

/// Put the table into the engine's `InputMap`. Actions missing from the project are skipped.
fn apply_to_input_map(table: &BindingTable) {
    let mut map = InputMap::singleton();
    for (i, (action, _, _)) in ACTIONS.iter().enumerate() {
        let name = StringName::from(*action);
        if !map.has_action(&name) { continue; }
        map.action_erase_events(&name);
        let (kb, pad) = &table.current[i];
        for code in kb.iter().chain(pad.iter()) { map.action_add_event(&name, &event_of(*code)); }
    }
}

// ---- Display text of a binding ----

fn mods_text(m: u8) -> String {
    let mut parts = Vec::new();
    if m & mods::CMD_OR_CTRL != 0 { parts.push(if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" }); }
    if m & mods::CTRL != 0 { parts.push("Ctrl"); }
    if m & mods::META != 0 { parts.push(if cfg!(target_os = "macos") { "Cmd" } else { "Win" }); }
    if m & mods::ALT != 0 { parts.push(if cfg!(target_os = "macos") { "Option" } else { "Alt" }); }
    if m & mods::SHIFT != 0 { parts.push("Shift"); }
    parts.iter().map(|p| format!("{p}+")).collect()
}

/// What a player reads for one of Godot's key names: the printed symbol for punctuation ("=" for
/// "Equal"), "Num" for the number pad, and sentence case for two-word names ("Page up"). Names
/// with nothing to change, letters and digits among them, pass through.
fn key_display(name: &str) -> String {
    let symbol = match name {
        "Equal" => Some("="), "Minus" => Some("-"), "Plus" => Some("+"), "Comma" => Some(","), "Period" => Some("."),
        "Slash" => Some("/"), "BackSlash" => Some("\\"), "NumberSign" => Some("#"), "Section" => Some("§"), "Semicolon" => Some(";"), "Apostrophe" => Some("'"),
        "QuoteLeft" => Some("`"), "BracketLeft" => Some("["), "BracketRight" => Some("]"), "Asterisk" => Some("*"),
        "Escape" => Some("Esc"), "PageUp" => Some("Page up"), "PageDown" => Some("Page down"), "CapsLock" => Some("Caps lock"),
        "NumLock" => Some("Num lock"), "ScrollLock" => Some("Scroll lock"), "Print" => Some("Print screen"),
        _ => None,
    };
    if let Some(s) = symbol { return s.to_string(); }
    if let Some(rest) = name.strip_prefix("Kp ") {
        let key = match rest {
            "Add" => "+", "Subtract" => "-", "Multiply" => "*", "Divide" => "/", "Period" => ".", "Enter" => "Enter",
            other => other,
        };
        return format!("Num {key}");
    }
    name.to_string()
}

fn code_text(lk: &dyn crate::text::Lookup, code: EventCode) -> String {
    match code {
        EventCode::Key { code, physical, mods: m } => {
            let key = Key::from_ord(code as i32);
            // The headless display server has no keyboard layout and reports an error for every lookup.
            let ds = DisplayServer::singleton();
            let label = if physical && ds.get_name().to_string() != "headless" { ds.keyboard_get_label_from_physical(key) } else { key };
            let mut name = Os::singleton().get_keycode_string(label).to_string();
            if name.is_empty() { name = Os::singleton().get_keycode_string(key).to_string(); }
            format!("{}{}", mods_text(m), key_display(&name))
        }
        EventCode::Mouse { button, mods: m } => format!("{}{}", mods_text(m), lk.tr(mouse_key(button))),
        EventCode::JoyButton(b) => lk.tr(pad_button_key(b)),
        EventCode::JoyAxis { axis, positive } => lk.tr(pad_axis_key(axis, positive)),
    }
}

/// Stem of the gamepad glyph in res://ui/icons (`pad_face_south`) for a gamepad binding, by
/// position so it reads on any pad layout. None for keys, mouse buttons and pad inputs the icon set
/// has no glyph for (guide, share, paddles, touchpad).
pub fn pad_glyph(code: EventCode) -> Option<&'static str> {
    match code {
        EventCode::JoyButton(b) => Some(match b {
            0 => "pad_face_south", 1 => "pad_face_east", 2 => "pad_face_west", 3 => "pad_face_north",
            4 => "pad_select", 6 => "pad_start", 7 => "pad_stick_left_press", 8 => "pad_stick_right_press",
            9 => "pad_shoulder_left", 10 => "pad_shoulder_right",
            11 => "pad_dpad_up", 12 => "pad_dpad_down", 13 => "pad_dpad_left", 14 => "pad_dpad_right",
            _ => return None,
        }),
        EventCode::JoyAxis { axis, .. } => Some(match axis {
            0 | 1 => "pad_stick_left", 2 | 3 => "pad_stick_right", 4 => "pad_trigger_left", 5 => "pad_trigger_right",
            _ => return None,
        }),
        _ => None,
    }
}

/// Actions a stick drives: their dead zone follows `input.stick_deadzone`.
const STICK_ACTIONS: [&str; 8] = ["helm_ahead", "helm_astern", "helm_port", "helm_starboard", "look_left", "look_right", "look_up", "look_down"];

/// Set the stick actions' dead zone from the `input.stick_deadzone` setting.
fn apply_stick_deadzone() {
    let dz = match crate::services::services().settings.borrow().get("input.stick_deadzone") { Some(SettingValue::Float(f)) => f, _ => 0.2 };
    let mut map = InputMap::singleton();
    for action in STICK_ACTIONS {
        let name = StringName::from(action);
        if map.has_action(&name) { map.action_set_deadzone(&name, dz as f32); }
    }
}

const MOUSE_KEYS: [&str; 10] = ["settings.mouse.other", "settings.mouse.1", "settings.mouse.2", "settings.mouse.3", "settings.mouse.4", "settings.mouse.5", "settings.mouse.6", "settings.mouse.7", "settings.mouse.8", "settings.mouse.9"];
fn mouse_key(button: i64) -> &'static str { usize::try_from(button).ok().and_then(|b| MOUSE_KEYS.get(b)).copied().unwrap_or(MOUSE_KEYS[0]) }
const PAD_BUTTON_KEYS: [&str; 21] = [
    "settings.pad.button.0", "settings.pad.button.1", "settings.pad.button.2", "settings.pad.button.3", "settings.pad.button.4", "settings.pad.button.5",
    "settings.pad.button.6", "settings.pad.button.7", "settings.pad.button.8", "settings.pad.button.9", "settings.pad.button.10", "settings.pad.button.11",
    "settings.pad.button.12", "settings.pad.button.13", "settings.pad.button.14", "settings.pad.button.15", "settings.pad.button.16", "settings.pad.button.17",
    "settings.pad.button.18", "settings.pad.button.19", "settings.pad.button.20",
];
fn pad_button_key(b: i64) -> &'static str { usize::try_from(b).ok().and_then(|b| PAD_BUTTON_KEYS.get(b)).copied().unwrap_or("settings.pad.button.other") }
const PAD_AXIS_KEYS: [[&str; 2]; 6] = [
    ["settings.pad.axis.0.neg", "settings.pad.axis.0.pos"], ["settings.pad.axis.1.neg", "settings.pad.axis.1.pos"],
    ["settings.pad.axis.2.neg", "settings.pad.axis.2.pos"], ["settings.pad.axis.3.neg", "settings.pad.axis.3.pos"],
    ["settings.pad.axis.4.neg", "settings.pad.axis.4.pos"], ["settings.pad.axis.5.neg", "settings.pad.axis.5.pos"],
];
fn pad_axis_key(axis: i64, positive: bool) -> &'static str { usize::try_from(axis).ok().and_then(|a| PAD_AXIS_KEYS.get(a)).map_or("settings.pad.axis.other", |k| k[positive as usize]) }

/// Every `settings.*` key this unit emits: labels and tooltips of the visible schema rows, option labels,
/// section and group names, action labels, input names and rebind replies.
pub fn text_keys() -> Vec<String> {
    let mut keys = Vec::new();
    for def in SCHEMA.iter().filter(|d| visible(d)) {
        keys.push(format!("settings.{}", def.key));
        keys.push(format!("settings.{}.tip", def.key));
        keys.extend(def.options.iter().map(|(_, k)| k.to_string()));
    }
    for section in ["access", "hud", "input", "editor", "graphics", "audio", "net", "privacy", "playtest"] { keys.push(format!("settings.section.{section}")); }
    for g in [Group::Sortie, Group::Editor, Group::Menus] { keys.push(format!("settings.group.{}", g.name())); }
    keys.extend(ACTIONS.iter().map(|a| format!("settings.action.{}", a.0)));
    keys.extend(["settings.device.keyboard", "settings.device.gamepad", "settings.binding.none"].map(String::from));
    keys.extend(REBIND_KEYS.iter().map(|k| k.to_string()));
    keys.extend(MOUSE_KEYS.iter().map(|k| k.to_string()));
    keys.extend(PAD_BUTTON_KEYS.iter().map(|k| k.to_string()));
    keys.push("settings.pad.button.other".into());
    keys.extend(PAD_AXIS_KEYS.iter().flatten().map(|k| k.to_string()));
    keys.push("settings.pad.axis.other".into());
    keys.extend(crate::quality::REASON_KEYS.iter().map(|k| k.to_string()));
    keys.sort();
    keys.dedup();
    keys
}

const REBIND_KEYS: [&str; 7] = ["settings.rebind.bound", "settings.rebind.replaced", "settings.rebind.conflict", "settings.rebind.no_chords", "settings.rebind.wrong_device", "settings.rebind.no_gamepad", "settings.rebind.unknown_action"];

fn rebind_msg(outcome: &RebindOutcome, action: &str, event_text: &str) -> sim_systems::protocol::Msg {
    use sim_systems::protocol::{Msg, MsgArg};
    let label = MsgArg::Key(content::ids::TextKey::new(format!("settings.action.{action}")));
    let ev = MsgArg::Name(event_text.to_string());
    match outcome {
        RebindOutcome::Bound => Msg::new("settings.rebind.bound").arg(ev).arg(label),
        RebindOutcome::Replaced(other) => Msg::new("settings.rebind.replaced").arg(ev).arg(label).arg(MsgArg::Key(content::ids::TextKey::new(format!("settings.action.{other}")))),
        RebindOutcome::Conflict(other) => Msg::new("settings.rebind.conflict").arg(ev).arg(MsgArg::Key(content::ids::TextKey::new(format!("settings.action.{other}")))),
        RebindOutcome::Chord => Msg::new("settings.rebind.no_chords"),
        RebindOutcome::WrongDevice => Msg::new("settings.rebind.wrong_device"),
        RebindOutcome::NoGamepad => Msg::new("settings.rebind.no_gamepad"),
        RebindOutcome::Unknown => Msg::new("settings.rebind.unknown_action"),
    }
}

// ---------------------------------------------------------------------------------------------------------
// The autoload
// ---------------------------------------------------------------------------------------------------------

/// Seconds between the last change and the write.
const DEBOUNCE_S: f64 = 1.0;

/// Client settings. Autoload `Settings`.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct BpSettings {
    pub(crate) dirty_s: f64,
    base: Base<Node>,
}

#[godot_api]
impl INode for BpSettings {
    fn init(base: Base<Node>) -> Self { Self { dirty_s: -1.0, base } }
    /// Unit `gd-settings`: write the file 1 s after the last change.
    fn process(&mut self, delta: f64) {
        let pending: Vec<String> = PENDING_SIGNALS.with(|p| std::mem::take(&mut *p.borrow_mut()));
        if !pending.is_empty() {
            self.dirty_s = DEBOUNCE_S;
            for key in pending { self.emit_changed(&key); }
        }
        if self.dirty_s >= 0.0 {
            self.dirty_s -= delta.max(0.0);
            if self.dirty_s < 0.0 { save_file(); }
        }
    }
    /// Track the device the player last used, for shortcut hints.
    fn input(&mut self, event: Gd<InputEvent>) {
        let gamepad = if event.clone().try_cast::<InputEventJoypadButton>().is_ok() {
            Some(true)
        } else if let Ok(m) = event.clone().try_cast::<InputEventJoypadMotion>() {
            (m.get_axis_value().abs() > 0.5).then_some(true)
        } else if event.clone().try_cast::<InputEventKey>().is_ok() || event.clone().try_cast::<InputEventMouseButton>().is_ok() {
            Some(false)
        } else {
            None
        };
        if let Some(g) = gamepad { LAST_GAMEPAD.with(|l| l.set(g)); }
    }
    fn exit_tree(&mut self) { flush(); }
}

impl BpSettings {
    fn emit_changed(&mut self, key: &str) {
        if key == "input.stick_deadzone" { apply_stick_deadzone(); }
        let key = StringName::from(key).to_variant();
        // Through `base_mut`, so a handler that calls back into this object does not hit a held borrow.
        self.base_mut().emit_signal("changed", &[key]);
    }
    fn emit_bindings_changed(&mut self) { self.base_mut().emit_signal("bindings_changed", &[]); }
    fn mark_dirty(&mut self) { DIRTY.with(|d| d.set(true)); self.dirty_s = DEBOUNCE_S; }

    /// Store the table's overrides in the settings and push it into the `InputMap`.
    fn commit_table(&mut self, table: BindingTable) {
        apply_to_input_map(&table);
        crate::services::services().settings.borrow_mut().bindings = table.overrides();
        TABLE.with(|t| *t.borrow_mut() = Some(table));
        self.mark_dirty();
        self.emit_bindings_changed();
    }
    fn table() -> BindingTable {
        TABLE.with(|t| t.borrow_mut().get_or_insert_with(|| {
            let mut table = BindingTable::new(project_defaults());
            table.apply_overrides(&crate::services::services().settings.borrow().bindings);
            table
        }).clone())
    }
    fn device(device: &StringName) -> Option<bool> { match device.to_string().as_str() { "keyboard" => Some(false), "gamepad" => Some(true), _ => None } }
}

#[godot_api]
impl BpSettings {
    /// A setting changed; `key` is `section.name`. Fired after `set_value`, `reset_section` and `reload`.
    #[signal]
    fn changed(key: StringName);
    /// The input map was changed by a rebind or reset.
    #[signal]
    fn bindings_changed();

    /// Read `user://settings.cfg` and apply bindings to the `InputMap`. `boot.gd` calls it once. Returns
    /// true on the first launch (no file yet).
    #[func]
    fn reload(&mut self) -> bool {
        let first_launch = !FileAccess::file_exists(PATH);
        let mut loaded = ClientSettings { loaded: true, ..Default::default() };
        let mut cfg = ConfigFile::new_gd();
        if !first_launch {
            let err = cfg.load(PATH);
            if err != GdError::OK { godot_warn!("settings: {PATH} did not load ({err:?}); using defaults"); }
        }
        for def in SCHEMA {
            let Some((section, name)) = def.key.split_once('.') else { continue };
            if !cfg.has_section_key(section, name) { continue; }
            let v = cfg.get_value(section, name);
            // A value of the wrong type or outside the options falls back to the default.
            if let Some(value) = variant_setting(&v).and_then(|s| normalise(def, s)) { loaded.values.insert(def.key.to_string(), value); }
        }
        if cfg.has_section("bindings") {
            for action in cfg.get_section_keys("bindings").as_slice() {
                let v = cfg.get_value("bindings", action);
                let Ok(d) = v.try_to::<VarDictionary>() else { continue };
                let list = |k: &str| -> Vec<String> { d.get(k).and_then(|v| v.try_to::<PackedStringArray>().ok()).map(|a| a.as_slice().iter().take(8).map(|s| s.to_string()).collect()).unwrap_or_default() };
                loaded.bindings.push(Binding { action: action.to_string(), keyboard: list("keyboard"), gamepad: list("gamepad") });
            }
        }
        if cfg.has_section_key("player", "display_name") {
            loaded.display_name = cfg.get_value("player", "display_name").try_to::<GString>().map(|s| s.to_string().chars().take(32).collect()).unwrap_or_default();
        }
        let services = crate::services::services();
        *services.settings.borrow_mut() = loaded.clone();
        // Rebuild the binding table from the project defaults and the stored overrides.
        let mut table = BindingTable::new(project_defaults());
        table.apply_overrides(&loaded.bindings);
        services.settings.borrow_mut().bindings = table.overrides();
        apply_to_input_map(&table);
        apply_stick_deadzone();
        TABLE.with(|t| *t.borrow_mut() = Some(table));
        DIRTY.with(|d| d.set(false));
        PENDING_SIGNALS.with(|p| p.borrow_mut().clear());
        self.dirty_s = -1.0;
        for def in SCHEMA { self.emit_changed(def.key); }
        self.emit_bindings_changed();
        first_launch
    }
    /// Write the file now (also happens 1 s after any change and on exit).
    #[func]
    fn save_now(&mut self) -> bool { self.dirty_s = -1.0; save_file() }
    /// The whole schema in display order: `Array[Dictionary]` with `key: StringName`, `section: StringName`,
    /// `type: StringName` (`&"bool"`, `&"int"`, `&"float"`, `&"enum"`, `&"text"`), `label: StringName`,
    /// `tooltip: StringName`, `min: float`, `max: float`, `step: float`,
    /// `options: Array[{ "value": int, "label": StringName }]`, `default: Variant`, `value: Variant`,
    /// `visible: bool` (false on the other platform and for internal flags), `needs_restart: bool`.
    #[func]
    fn schema(&self) -> Array<VarDictionary> {
        let s = crate::services::services().settings.borrow().clone();
        SCHEMA.iter().map(|def| {
            let ty = match &def.default {
                SettingValue::Bool(_) => "bool",
                SettingValue::Int(_) if !def.options.is_empty() => "enum",
                SettingValue::Int(_) => "int",
                SettingValue::Float(_) => "float",
                SettingValue::Text(_) => "text",
            };
            let options: VarArray = def.options.iter().map(|(v, k)| crate::convert::d! { "value" => *v, "label" => StringName::from(*k) }.to_variant()).collect();
            crate::convert::d! {
                "key" => StringName::from(def.key),
                "section" => StringName::from(section_of(def.key)),
                "type" => StringName::from(ty),
                "label" => StringName::from(format!("settings.{}", def.key).as_str()),
                "tooltip" => StringName::from(format!("settings.{}.tip", def.key).as_str()),
                "min" => def.min,
                "max" => def.max,
                "step" => def.step,
                "options" => options,
                "default" => setting_variant(&def.default),
                "value" => s.get(def.key).map_or_else(Variant::nil, |v| setting_variant(&v)),
                "visible" => visible(def),
                "needs_restart" => def.needs_restart,
            }
        }).collect()
    }
    /// Value of one setting as `bool`, `int`, `float` or `String`; `null` for an unknown key.
    #[func]
    fn get_value(&self, key: StringName) -> Variant {
        let key = key.to_string();
        if ClientSettings::def(&key).is_none() { return Variant::nil(); }
        crate::services::services().settings.borrow().get(&key).map_or_else(Variant::nil, |v| setting_variant(&v))
    }
    /// Set one setting; numbers are clamped and stepped. Returns false for an unknown key or a wrong type.
    #[func]
    fn set_value(&mut self, key: StringName, value: Variant) -> bool {
        let key = key.to_string();
        let Some(value) = variant_setting(&value) else { return false };
        if !write(&key, value) { return false; }
        // Emit now rather than next frame; `write` queued it, so drain that entry.
        let emit = PENDING_SIGNALS.with(|p| { let mut p = p.borrow_mut(); let had = p.iter().any(|k| *k == key); p.retain(|k| *k != key); had });
        self.dirty_s = DEBOUNCE_S;
        if emit { self.emit_changed(&key); }
        true
    }
    /// Return one section (`&"access"`, `&"graphics"`...) to its defaults.
    #[func]
    fn reset_section(&mut self, section: StringName) {
        let section = section.to_string();
        let changed: Vec<&'static str> = {
            let services = crate::services::services();
            let mut s = services.settings.borrow_mut();
            // Internal flags are not the player's to reset (the first-launch flag, the tutorial, auto-detect).
            let keys: Vec<&'static str> = SCHEMA.iter().filter(|d| section_of(d.key) == section && !INTERNAL.contains(&d.key)).map(|d| d.key).collect();
            keys.into_iter().filter(|k| s.values.remove(*k).is_some()).collect()
        };
        if changed.is_empty() { return; }
        self.mark_dirty();
        for k in changed { self.emit_changed(k); }
    }
    /// First launch only: apply the OS accessibility flags (L1739). `reduce_motion` sets reduce motion and
    /// blink-off; `reduce_transparency` sets reduce transparency; `increase_contrast` selects the
    /// high-contrast palette. Marks the first launch done.
    #[func]
    fn apply_os_accessibility(&mut self, reduce_motion: bool, reduce_transparency: bool, increase_contrast: bool) {
        if crate::services::services().settings.borrow().bool("access.first_launch_done") { return; }
        if reduce_motion { write("access.reduce_motion", SettingValue::Bool(true)); write("access.blink_off", SettingValue::Bool(true)); }
        if reduce_transparency { write("access.reduce_transparency", SettingValue::Bool(true)); }
        if increase_contrast { write("access.palette", SettingValue::Int(1)); }
        write("access.first_launch_done", SettingValue::Bool(true));
        let pending: Vec<String> = PENDING_SIGNALS.with(|p| std::mem::take(&mut *p.borrow_mut()));
        self.mark_dirty();
        for key in pending { self.emit_changed(&key); }
    }

    // ---- Bindings (L1719) ----

    /// Every rebindable action in display order: `Array[Dictionary]` with `action: StringName`,
    /// `label: StringName`, `group: StringName` (`&"sortie"`, `&"editor"`, `&"menus"`),
    /// `keyboard: Array[InputEvent]`, `gamepad: Array[InputEvent]`, `is_hold: bool`,
    /// `keyboard_default: bool`, `gamepad_default: bool`, `gamepad_allowed: bool` (false for editor actions,
    /// L58).
    #[func]
    fn bindings(&self) -> Array<VarDictionary> {
        let table = Self::table();
        ACTIONS.iter().enumerate().map(|(i, (action, group, hold))| {
            let events = |list: &[EventCode]| -> Array<Gd<InputEvent>> { list.iter().map(|c| event_of(*c)).collect() };
            crate::convert::d! {
                "action" => StringName::from(*action),
                "label" => StringName::from(format!("settings.action.{action}").as_str()),
                "group" => StringName::from(group.name()),
                "keyboard" => events(&table.current[i].0),
                "gamepad" => events(&table.current[i].1),
                "is_hold" => *hold,
                "keyboard_default" => table.current[i].0 == table.defaults[i].0,
                "gamepad_default" => table.current[i].1 == table.defaults[i].1,
                "gamepad_allowed" => gamepad_allowed(*group),
            }
        }).collect()
    }
    /// Bind `event` as binding number `index` (0 or 1) of an action on a device (`&"keyboard"` covers keys
    /// and mouse buttons; `&"gamepad"` buttons and axes). Chords are refused. Returns
    /// `{ "ok": bool, "conflict": StringName (action that already uses the event, or &""), "text": MsgDict }`.
    /// A conflict in the same group is refused unless `replace` is true, which unbinds the other action.
    #[func]
    fn rebind(&mut self, action: StringName, device: StringName, index: i64, event: Gd<InputEvent>, replace: bool) -> VarDictionary {
        let action_s = action.to_string();
        let lk = crate::text::EngineLookup::new();
        let reply = |ok: bool, conflict: &str, outcome: &RebindOutcome, text: &str| crate::convert::d! {
            "ok" => ok,
            "conflict" => StringName::from(conflict),
            "text" => crate::convert::msg_dict(&rebind_msg(outcome, &action_s, text)),
        };
        let (Some(gamepad), Some(code)) = (Self::device(&device), code_of(&event)) else { return reply(false, "", &RebindOutcome::WrongDevice, "") };
        let text = code_text(&lk, code);
        if action_row(&action_s).is_none() || index < 0 { return reply(false, "", &RebindOutcome::Unknown, &text); }
        let mut table = Self::table();
        let outcome = table.rebind(&action_s, gamepad, index as usize, code, replace);
        match outcome {
            RebindOutcome::Bound | RebindOutcome::Replaced(_) => {
                let other = if let RebindOutcome::Replaced(o) = outcome { o } else { "" };
                self.commit_table(table);
                reply(true, other, &outcome, &text)
            }
            RebindOutcome::Conflict(o) => reply(false, o, &outcome, &text),
            _ => reply(false, "", &outcome, &text),
        }
    }
    #[func]
    fn unbind(&mut self, action: StringName, device: StringName, index: i64) -> bool {
        let Some(gamepad) = Self::device(&device) else { return false };
        let Ok(index) = usize::try_from(index) else { return false };
        let mut table = Self::table();
        if !table.unbind(&action.to_string(), gamepad, index) { return false; }
        self.commit_table(table);
        true
    }
    /// Return one device's bindings (`&"keyboard"` or `&"gamepad"`) to the defaults in `project.godot`.
    #[func]
    fn reset_bindings(&mut self, device: StringName) {
        let Some(gamepad) = Self::device(&device) else { return };
        let mut table = Self::table();
        table.reset_device(gamepad);
        self.commit_table(table);
    }
    /// Display text of one bound event as `bindings()` returns it ("W", "Shift+Tab", "Left click",
    /// a gamepad button name), for the bindings page. `""` for an event no binding can hold.
    #[func]
    fn event_text(&self, event: Gd<InputEvent>) -> GString {
        let lk = crate::text::EngineLookup::new();
        code_of(&event).map_or_else(GString::new, |code| GString::from(code_text(&lk, code).as_str()))
    }
    /// Display text for an action's first binding on the device last used ("Tab", "Space", a gamepad glyph
    /// name), for shortcut hints and tooltips.
    #[func]
    fn binding_text(&self, action: StringName) -> GString {
        let lk = crate::text::EngineLookup::new();
        match Self::first_binding(&action, LAST_GAMEPAD.with(|l| l.get()), true) {
            Some(code) => GString::from(code_text(&lk, code).as_str()),
            None => GString::from(crate::text::Lookup::tr(&lk, "settings.binding.none").as_str()),
        }
    }
    /// Display text for an action's first binding on the named device (`gamepad` true: the pad), or `""`
    /// when that device has none. No fallback to the other device: on a pad, a key cap "X" would read
    /// as the X button. Shortcut hints pass `Ui.using_gamepad`, so the hint and the device switch that
    /// redraws it always agree.
    #[func]
    fn binding_text_for(&self, action: StringName, gamepad: bool) -> GString {
        let lk = crate::text::EngineLookup::new();
        Self::first_binding(&action, gamepad, false).map_or_else(GString::new, |code| GString::from(code_text(&lk, code).as_str()))
    }
    /// Glyph stem in res://ui/icons (`pad_face_south`) of the binding `binding_text_for` names, or `""`
    /// when that binding is a key, a mouse button, a pad input with no glyph, or missing.
    #[func]
    fn binding_glyph(&self, action: StringName, gamepad: bool) -> GString {
        Self::first_binding(&action, gamepad, false).and_then(pad_glyph).map_or_else(GString::new, GString::from)
    }
    /// Glyph stem of one bound gamepad event (`pad_stick_left`), or `""`; for the bindings page.
    #[func]
    fn event_glyph(&self, event: Gd<InputEvent>) -> GString {
        code_of(&event).and_then(pad_glyph).map_or_else(GString::new, GString::from)
    }
}

impl BpSettings {
    /// The first binding of an action on one device, or with `fallback` on the other when that has none.
    fn first_binding(action: &StringName, gamepad: bool, fallback: bool) -> Option<EventCode> {
        let table = Self::table();
        let i = ACTIONS.iter().position(|a| a.0 == action.to_string())?;
        let (kb, pad) = &table.current[i];
        let (first, second) = if gamepad { (pad, kb) } else { (kb, pad) };
        first.first().or_else(|| if fallback { second.first() } else { None }).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(key: &str) -> &'static SettingDef { ClientSettings::def(key).unwrap() }

    #[test]
    fn key_names_read_as_printed() {
        assert_eq!(key_display("Equal"), "=");
        assert_eq!(key_display("BracketRight"), "]");
        // Godot 4.7 spells it "BackSlash" (OS.get_keycode_string).
        assert_eq!(key_display("BackSlash"), "\\");
        assert_eq!(key_display("Kp Add"), "Num +");
        assert_eq!(key_display("Kp 7"), "Num 7");
        assert_eq!(key_display("PageDown"), "Page down");
        assert_eq!(key_display("Escape"), "Esc");
        assert_eq!(key_display("W"), "W");
        assert_eq!(key_display("Space"), "Space");
    }

    #[test]
    fn pad_bindings_have_glyphs() {
        assert_eq!(pad_glyph(EventCode::JoyButton(0)), Some("pad_face_south"));
        assert_eq!(pad_glyph(EventCode::JoyButton(6)), Some("pad_start"));
        assert_eq!(pad_glyph(EventCode::JoyButton(5)), None);
        assert_eq!(pad_glyph(EventCode::JoyAxis { axis: 1, positive: false }), Some("pad_stick_left"));
        assert_eq!(pad_glyph(EventCode::JoyAxis { axis: 5, positive: true }), Some("pad_trigger_right"));
        assert_eq!(pad_glyph(EventCode::Key { code: 87, physical: true, mods: 0 }), None);
        // Every stem names an icon of the set.
        for b in 0..21 {
            if let Some(stem) = pad_glyph(EventCode::JoyButton(b)) {
                assert!(std::path::Path::new(&format!("{}/../../game/ui/icons/{stem}.svg", env!("CARGO_MANIFEST_DIR"))).exists(), "{stem}");
            }
        }
    }

    #[test]
    fn numbers_clamp_and_step() {
        assert_eq!(normalise(def("access.ui_scale"), SettingValue::Float(3.0)), Some(SettingValue::Float(2.0)));
        assert_eq!(normalise(def("access.ui_scale"), SettingValue::Float(0.5)), Some(SettingValue::Float(0.75)));
        assert_eq!(normalise(def("access.ui_scale"), SettingValue::Float(1.01)), Some(SettingValue::Float(1.0)));
        assert_eq!(normalise(def("access.ui_scale"), SettingValue::Int(1)), Some(SettingValue::Float(1.0)));
        assert_eq!(normalise(def("access.ui_scale"), SettingValue::Float(f64::NAN)), None);
        assert_eq!(normalise(def("access.ui_scale"), SettingValue::Bool(true)), None);
        assert_eq!(normalise(def("net.port"), SettingValue::Int(80)), Some(SettingValue::Int(1024)));
        assert_eq!(normalise(def("graphics.msaa"), SettingValue::Int(4)), Some(SettingValue::Int(4)));
        assert_eq!(normalise(def("graphics.msaa"), SettingValue::Int(8)), None);
        assert_eq!(normalise(def("privacy.crash_reports"), SettingValue::Float(1.0)), Some(SettingValue::Int(1)));
        assert_eq!(normalise(def("graphics.detected_machine"), SettingValue::Text("x".repeat(300))), Some(SettingValue::Text("x".repeat(256))));
    }

    #[test]
    fn schema_defaults_are_valid() {
        let mut keys = std::collections::BTreeSet::new();
        for d in SCHEMA {
            assert!(keys.insert(d.key), "duplicate {}", d.key);
            assert!(d.key.contains('.'), "{}", d.key);
            assert_eq!(normalise(d, d.default.clone()).as_ref(), Some(&d.default), "{}", d.key);
        }
        for k in INTERNAL { assert!(keys.contains(k), "{k}"); }
    }

    #[test]
    fn event_codes_round_trip() {
        for c in [
            EventCode::Key { code: 82, physical: true, mods: 0 },
            EventCode::Key { code: 90, physical: true, mods: mods::SHIFT | mods::CMD_OR_CTRL },
            EventCode::Key { code: 4194306, physical: false, mods: 0 },
            EventCode::Mouse { button: 2, mods: 0 },
            EventCode::JoyButton(0),
            EventCode::JoyAxis { axis: 5, positive: true },
            EventCode::JoyAxis { axis: 1, positive: false },
        ] { assert_eq!(EventCode::decode(&c.encode()), Some(c), "{}", c.encode()); }
        for bad in ["", "key", "key:", "key:-4", "joy:1+shift", "axis:1:x", "nope:1", "key:1+hyper", "key:99999999999"] { assert_eq!(EventCode::decode(bad), None, "{bad}"); }
        assert!(EventCode::Key { code: 82, physical: true, mods: mods::SHIFT }.is_chord());
        assert!(!EventCode::JoyButton(1).is_chord());
    }

    fn table() -> BindingTable {
        let mut d: Vec<(Vec<EventCode>, Vec<EventCode>)> = vec![(vec![], vec![]); ACTIONS.len()];
        let at = |a: &str| ACTIONS.iter().position(|r| r.0 == a).unwrap();
        d[at("fire")] = (vec![EventCode::Mouse { button: 1, mods: 0 }], vec![EventCode::JoyAxis { axis: 5, positive: true }]);
        d[at("decoys")] = (vec![EventCode::Key { code: 67, physical: true, mods: 0 }], vec![]);
        d[at("editor_copy")] = (vec![EventCode::Key { code: 67, physical: true, mods: mods::CMD_OR_CTRL }], vec![]);
        d[at("editor_mirror")] = (vec![EventCode::Key { code: 77, physical: true, mods: 0 }], vec![]);
        d[at("menu")] = (vec![EventCode::Key { code: 4194305, physical: true, mods: 0 }], vec![EventCode::JoyButton(6)]);
        BindingTable::new(d)
    }

    #[test]
    fn rebind_refuses_chords_conflicts_and_wrong_devices() {
        let mut t = table();
        let c = EventCode::Key { code: 67, physical: true, mods: 0 };
        assert_eq!(t.rebind("emcon_cycle", false, 0, c, false), RebindOutcome::Conflict("decoys"));
        assert_eq!(t.rebind("emcon_cycle", false, 0, EventCode::Key { code: 67, physical: true, mods: mods::SHIFT }, false), RebindOutcome::Chord);
        assert_eq!(t.rebind("emcon_cycle", true, 0, c, false), RebindOutcome::WrongDevice);
        assert_eq!(t.rebind("editor_copy", true, 0, EventCode::JoyButton(2), false), RebindOutcome::NoGamepad);
        assert_eq!(t.rebind("nope", false, 0, c, false), RebindOutcome::Unknown);
        // A sortie key may match an editor key: the groups never run together.
        assert_eq!(t.rebind("decoys", false, 0, EventCode::Key { code: 77, physical: true, mods: 0 }, false), RebindOutcome::Bound);
        // Menu keys conflict with every group, so Esc can never be lost to a sortie action by accident.
        assert_eq!(t.rebind("decoys", false, 1, EventCode::Key { code: 4194305, physical: true, mods: 0 }, false), RebindOutcome::Conflict("menu"));
        assert!(t.overrides().iter().any(|b| b.action == "decoys"));
    }

    #[test]
    fn rebind_replace_moves_the_event_and_reset_restores() {
        let mut t = table();
        let c = EventCode::Key { code: 67, physical: true, mods: 0 };
        assert_eq!(t.rebind("emcon_cycle", false, 0, c, true), RebindOutcome::Replaced("decoys"));
        let at = |a: &str| ACTIONS.iter().position(|r| r.0 == a).unwrap();
        assert!(t.current[at("decoys")].0.is_empty());
        assert_eq!(t.current[at("emcon_cycle")].0, vec![c]);
        // Second slot, then the same event again moves rather than duplicates.
        let x = EventCode::Key { code: 88, physical: true, mods: 0 };
        assert_eq!(t.rebind("emcon_cycle", false, 1, x, false), RebindOutcome::Bound);
        assert_eq!(t.current[at("emcon_cycle")].0, vec![c, x]);
        assert_eq!(t.rebind("emcon_cycle", false, 0, x, false), RebindOutcome::Bound);
        assert_eq!(t.current[at("emcon_cycle")].0, vec![x]);
        assert!(t.unbind("emcon_cycle", false, 0));
        assert!(!t.unbind("emcon_cycle", false, 5));
        let stored = t.overrides();
        let mut again = table();
        again.apply_overrides(&stored);
        assert_eq!(again.current, t.current);
        t.reset_device(false);
        assert!(t.overrides().is_empty());
    }

    #[test]
    fn stored_overrides_are_sanitised() {
        let mut t = table();
        t.apply_overrides(&[Binding { action: "fire".into(), keyboard: vec!["joy:1".into(), "garbage".into(), "mouse:2".into()], gamepad: vec!["key:1".into()] }]);
        let at = ACTIONS.iter().position(|r| r.0 == "fire").unwrap();
        assert_eq!(t.current[at].0, vec![EventCode::Mouse { button: 2, mods: 0 }]);
        assert!(t.current[at].1.is_empty());
        t.apply_overrides(&[Binding { action: "unknown".into(), ..Default::default() }]);
    }

    /// `ACTIONS` lists exactly the project's non-`ui_*` input actions.
    #[test]
    fn actions_match_project_input_map() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/project.godot")).unwrap();
        let section = text.split("\n[input]\n").nth(1).unwrap().split("\n[").next().unwrap();
        let project: Vec<&str> = section.lines().filter_map(|l| l.strip_suffix("={")).filter(|a| !a.starts_with("ui_")).collect();
        let mut ours: Vec<&str> = ACTIONS.iter().map(|a| a.0).collect();
        let mut theirs = project.clone();
        ours.sort();
        theirs.sort();
        assert_eq!(ours, theirs);
    }
}
