//! Unit `gd-plot` (hard: declutter, accessibility sub-elements, per-frame drawing inside the budget).
//! `BpPlotView` (Control): the plot table, a 2D chart drawn in Rust. DESIGN.md L106 (tracks with their
//! uncertainty shapes, weapon envelopes, enemy emitters, planned route), L1660-1663 (stance by frame, tint
//! and label; domain modifiers; uncertainty keeps sensor shapes with graded edges and sample points, never
//! hue), L1669 (every line is a labelled reference), L1670 (sparse by default; label overlap hide order;
//! hover shows hidden), L1650 (symbols: Friend circle, Neutral 2:1 capsule, Suspect downward shield, Hostile
//! upward triangle; air bar above, subsurface bar below), L1735 (a `_draw()` node reading theme items from
//! the custom type; `queue_redraw()` on data change only), L1743 (one accessibility sub-element per track,
//! rebuilt on `NOTIFICATION_ACCESSIBILITY_UPDATE`, positions at 1 Hz), L1712 (labels at least 18 px).
//!
//! The same control draws the briefing chart (route, map, threat areas), the after-action overlay (belief
//! map) and replays, by `set_mode`. It reads `Services::view` and the newest snapshot directly; no
//! Dictionary is built per frame. Colours, fonts and sizes come from the theme type `Plot`
//! (`res://ui/theme/tokens.json`); nothing here holds a colour literal.
//! Gamepad: a plot cursor moved by `move_plot_cursor`, snapping to tracks.
//!
//! # Layout
//! - [`core`]: transform, scale steps, symbol and shape geometry, declutter, hit tests (no Godot).
//! - [`picture`]: what one redraw shows, built from the view frame, the snapshot, the HUD's selection and
//!   history and the static chart (no Godot).
//! - [`chart`]: the static chart from a `MapDict` or traced from the sortie map.
//! - [`paint`]: theme items and drawing. [`access`]: the screen-reader tree. [`check`]: `BpPlotCheck`, a
//!   test-only feed for captures.
//!
//! # Redraws
//! `process` redraws only when something drawn changed: a new snapshot (`FrameFeed::snapshot_rev`, about
//! 30 Hz while the sortie runs, nothing while paused), the selection or hover, a change box ending, the
//! chart arriving, a layer, scale, tool or cursor change, a theme or size change. Nothing is done while the
//! control is hidden (the bridge view), except keeping change boxes and the chart current.
//!
//! # Decisions where the spec is silent
//! - Briefing and after-action draw with the theme type `Chart` (the NOAA palette of L1643, "Chart, the
//!   briefing chart and campaign chart" in theme_tokens.md); sortie and replay with `Plot`.
//! - `show_static` with a map replaces the sortie map in every mode; without one the sortie's land layer is
//!   traced from the map the world view works out from the sortie intro, on a helper thread, once per map.
//! - `show_static` in briefing and after-action picks the finest scale step that fits the map and centres
//!   on the map's centre.
//! - While the plot follows the own hull, the wheel and `zoom_step` keep the own hull centred (the player
//!   stays oriented); once panned away, zoom keeps the point under the pointer fixed.
//! - Left drag and middle drag pan; a press that moves under 5 px is a click and fires on release
//!   (accessibility floor 5). The trackpad's pan gesture pans; a pinch past 25% steps the scale.
//! - Right-click on a symbol reports `track_clicked` with the right button; the UI decides what it means.
//! - With the line tool, a drag or two clicks draw the line; right-click cancels. The measure tool keeps its
//!   last line on the plot until the next click or tool change.
//! - Gamepad: the cursor shows once `move_plot_cursor` moves it and hides when the mouse moves; the
//!   `designate` and `set_course` actions on a joypad act at the cursor while the plot is visible.
//! - "Threat areas" on the briefing chart: `show_static` carries no threat geometry, so the briefing draws
//!   the map's markers (radar and depot sites as site marks); nothing is invented.
//! - The search-radar sweep (L412, and "a sweep at the simulated rate" in the visual direction) is a single
//!   1 px `rule.reference` line from the own hull, shown only while an own rotating radar radiates, turning
//!   at that radar's rotation period on simulation time, so it stops with pause and speeds with compression.
//!   It is a child canvas item turned by its transform each frame; the plot itself is not redrawn for it.
//!   The client does not know each antenna's phase, so the line starts at north at sortie time zero.
// Client presentation, not simulation (clippy.toml): std floats are allowed here.
#![allow(clippy::disallowed_methods)]
pub mod access;
pub mod chart;
pub mod check;
pub mod core;
pub mod paint;
pub mod picture;

use self::chart::Chart;
use self::core::Frame;
use self::picture::{layer, Env, Inputs, Overlay, Picture, SymKind};
use crate::hud::BpHud;
use crate::interp::ViewFrame;
use crate::services::services;
use glam::{DVec2, Vec2};
use godot::classes::control::FocusMode;
use godot::classes::notify::ControlNotification;
use godot::classes::{Control, IControl, InputEvent, RenderingServer, InputEventJoypadButton, InputEventMagnifyGesture, InputEventMouseButton, InputEventMouseMotion, InputEventPanGesture};
use godot::global::MouseButton;
use godot::prelude::*;
use sim_systems::ids::TrackId;
use sim_systems::protocol::SimEvent;
use std::collections::BTreeMap;
use std::sync::mpsc::Receiver;

/// Plot scale steps in metres per pixel at 100% UI scale; there is no continuous zoom.
pub const SCALE_STEPS_M_PER_PX: [f32; 8] = [1.0, 2.5, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0];
pub const DEFAULT_SCALE_STEP: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlotMode { #[default] Sortie, Briefing, AfterAction, Replay }

/// Seconds a change box stays (L1649: mode-change box 10 s).
const BOX_S: f64 = 10.0;
/// Accessibility positions move at 1 Hz (L1743).
const ACCESS_PERIOD_S: f64 = 1.0;
/// Hidden labels count as "near" a point within this distance of their symbol.
const HIDDEN_NEAR_PX: f32 = 32.0;
/// Trackpad pinch factor that steps the scale.
const PINCH_STEP: f32 = 1.25;

/// The sortie map the land layer was traced from (as the world view keys it).
#[derive(Clone, Debug, PartialEq)]
struct MapKey { mode: content::enums::GameMode, seed: u64, operation: String, players: usize }

/// A press being watched for click or drag.
#[derive(Clone, Copy, Debug)]
struct Press { button: MouseButton, at: Vec2, centre: DVec2, moved: bool }

/// Position history per track (the HUD's history layer).
type History = BTreeMap<TrackId, Vec<[f32; 2]>>;

/// A test feed's picture source in place of the session (`BpPlotCheck`).
pub(crate) struct Injected { pub view: ViewFrame, pub selected: TrackId, pub history: History, pub rev: u64, pub chart: Option<Chart> }

/// Everything the plot keeps besides the contract fields.
#[derive(Default)]
pub(crate) struct PlotState {
    picture: Picture,
    palette: Option<paint::Palette>,
    static_chart: Option<Chart>,
    route: Vec<DVec2>,
    belief: Vec<[f64; 3]>,
    sortie_chart: Option<(MapKey, Chart)>,
    pending_chart: Option<(MapKey, Receiver<Chart>)>,
    /// Change boxes: wall-clock second each ends.
    boxes: BTreeMap<TrackId, f64>,
    now_s: f64,
    /// What the last redraw showed, to decide whether the next frame needs one.
    sig: Option<Sig>,
    dirty: bool,
    press: Option<Press>,
    /// First point of a line or measurement, the pointer's map position, a kept measurement.
    tool_start: Option<DVec2>,
    tool_end: Option<DVec2>,
    measured: Option<(DVec2, DVec2)>,
    pub(crate) cursor_shown: bool,
    pinch: f32,
    hud: Option<Gd<BpHud>>,
    access: access::Access,
    access_at_s: f64,
    /// Track set and selection the last accessibility update was queued for.
    access_queued: (Vec<TrackId>, TrackId),
    widths: std::cell::RefCell<BTreeMap<String, f32>>,
    pub(crate) injected: Option<Injected>,
    /// Main-thread time of the last redraw (build and draw calls), microseconds.
    pub(crate) draw_us: u64,
    /// The sweep's child canvas item, its rotation period in seconds (0 = no rotating radar radiates) and
    /// the line length it was built with.
    sweep: Option<Rid>,
    sweep_period_s: f64,
    sweep_len: f32,
}

#[derive(Clone, Debug, PartialEq)]
struct Sig { rev: u64, live: bool, selected: TrackId, boxes: usize, size: [i32; 2], centre: [i64; 2], chart: bool }

/// The plot table. Scene node; theme type `Plot`.
#[derive(GodotClass)]
#[class(base = Control)]
pub struct BpPlotView {
    pub(crate) mode: PlotMode,
    pub(crate) layers: i64,
    pub(crate) scale_step: usize,
    pub(crate) centre: [f64; 2],
    pub(crate) follow_own: bool,
    pub(crate) cursor: Vector2,
    pub(crate) hover: sim_systems::ids::TrackId,
    pub(crate) tool: StringName,
    pub(crate) seen_view_rev: u64,
    pub(crate) st: PlotState,
    /// Margins HUD clusters cover (left, top, right, bottom); see `set_clear_margins`.
    pub(crate) clear: [f32; 4],
    /// HUD clusters over the plot (local rectangles); see `set_clear_margins`.
    pub(crate) blocked: Vec<self::core::Rect>,
    base: Base<Control>,
}

/// Signals collected while borrowed, emitted after.
enum Out { Track(TrackId, MouseButton), Sea(DVec2, MouseButton), Hover(TrackId), Scale(usize), Line(DVec2, DVec2) }

/// Text measured with the theme font, translated through the engine, names from the HUD.
struct GodotEnv<'a> { lk: crate::text::EngineLookup, font: Option<Gd<godot::classes::Font>>, size: i32, line_h: f32, widths: &'a std::cell::RefCell<BTreeMap<String, f32>>, hud: Option<&'a BpHud> }
impl Env for GodotEnv<'_> {
    fn lk(&self) -> &dyn crate::text::Lookup { &self.lk }
    fn measure(&self, text: &str) -> Vec2 {
        if let Some(w) = self.widths.borrow().get(text) { return Vec2::new(*w, self.line_h); }
        let w = self.font.as_ref().map_or(text.chars().count() as f32 * self.size as f32 * 0.55, |f| f.get_string_size_ex(text).font_size(self.size).done().x) + 2.0;
        let mut cache = self.widths.borrow_mut();
        if cache.len() > 4_096 { cache.clear(); }
        cache.insert(text.to_string(), w);
        Vec2::new(w, self.line_h)
    }
    fn class_name(&self, class: &content::ids::ClassId) -> Option<String> {
        use crate::text::Lookup;
        services().campaign().classes.get(class).map(|d| self.lk.tr(d.name.as_str()))
    }
    fn vessel_name(&self, entity: sim_core::EntityId) -> Option<String> { self.hud?.core.intros.get(&entity).map(|f| f.name.clone()) }
    fn is_mine(&self, kind: content::ids::MunitionIdx) -> bool {
        services().catalogue().munitions.get_index(kind.index()).is_some_and(|(_, d)| d.kind == content::part::weapons::MunitionKind::Mine)
    }
}

fn gv(p: Vector2) -> Vec2 { Vec2::new(p.x, p.y) }
fn vg(p: Vec2) -> Vector2 { Vector2::new(p.x, p.y) }
fn map_v(p: DVec2) -> Vector2 { Vector2::new(p.x as f32, p.y as f32) }

#[godot_api]
impl IControl for BpPlotView {
    fn init(base: Base<Control>) -> Self {
        Self { mode: PlotMode::Sortie, layers: layer::DEFAULT, scale_step: DEFAULT_SCALE_STEP, centre: [0.0, 0.0], follow_own: true, cursor: Vector2::ZERO, hover: sim_systems::ids::TrackId::NONE, tool: StringName::from("orders"), seen_view_rev: 0, st: PlotState::default(), clear: [0.0; 4], blocked: Vec::new(), base }
    }
    fn ready(&mut self) {
        let mut b = self.base_mut();
        b.set_clip_contents(true);
        b.set_focus_mode(FocusMode::ALL);
        drop(b);
        self.st.hud = self.base().get_node_or_null("/root/Hud").and_then(|n| n.try_cast::<BpHud>().ok());
        self.st.dirty = true;
        if self.st.sweep.is_none() {
            let mut rs = RenderingServer::singleton();
            let item = rs.canvas_item_create();
            if item.is_valid() {
                rs.canvas_item_set_parent(item, self.base().get_canvas_item());
                rs.canvas_item_set_visible(item, false);
                self.st.sweep = Some(item);
            }
        }
        self.warm_fonts();
    }
    /// Unit `gd-plot`: `queue_redraw()` when the picture, the own position or the cursor changed.
    fn process(&mut self, _delta: f64) {
        let mut out = Vec::new();
        for req in self.st.access.take_requests() {
            match req {
                access::Request::Click(t) => out.push(Out::Track(t, MouseButton::LEFT)),
                access::Request::Focus(t) => { self.st.access.focus = t; self.base_mut().queue_accessibility_update(); }
            }
        }
        self.tick_state();
        if self.base().is_visible_in_tree() {
            let sig = self.signature();
            if self.st.dirty || self.st.sig.as_ref() != Some(&sig) {
                self.st.dirty = false;
                self.st.sig = Some(sig);
                self.base_mut().queue_redraw();
            }
            // Positions for screen readers at 1 Hz, and at once when the track set changes.
            let ids: Vec<TrackId> = self.st.picture.symbols.iter().filter(|s| s.kind == SymKind::Track).map(|s| s.track).collect();
            let selected = self.selected();
            if self.st.now_s - self.st.access_at_s >= ACCESS_PERIOD_S || ids != self.st.access_queued.0 || selected != self.st.access_queued.1 {
                self.st.access_at_s = self.st.now_s;
                self.st.access_queued = (ids, selected);
                self.base_mut().queue_accessibility_update();
            }
        }
        self.update_sweep();
        self.emit(out);
    }
    /// Unit `gd-plot`: draw layers bottom to top: water and land, graticule and range rings with labels,
    /// route and objective markers, envelopes, uncertainty shapes, history polylines, symbols, labels after
    /// declutter, selection bracket, cursor. Draws nothing until implemented.
    fn draw(&mut self) {
        let chart_mode = matches!(self.mode, PlotMode::Briefing | PlotMode::AfterAction);
        if self.st.palette.as_ref().is_none_or(|p| p.chart != chart_mode) {
            let ctrl: Gd<Control> = self.to_gd().upcast();
            self.st.palette = Some(paint::Palette::load(&ctrl, chart_mode));
        }
        let Some(pal) = self.st.palette.clone() else { return };
        let started = std::time::Instant::now();
        let picture = self.build_picture(&pal);
        let focused = self.base().has_focus_ex().ignore_hidden_focus(true).done();
        let mut ctrl: Gd<Control> = self.to_gd().upcast();
        {
            let chart = self.chart();
            paint::paint(&mut ctrl, &picture, chart, &pal, focused);
        }
        self.st.draw_us = started.elapsed().as_micros() as u64;
        // The sweep line reaches the farthest corner from the own hull; rebuilt only when that changes.
        if let (Some(item), Some(own)) = (self.st.sweep, picture.symbols.iter().find(|s| s.kind == SymKind::Own)) {
            let size = gv(self.base().get_size());
            let len = [Vec2::ZERO, Vec2::new(size.x, 0.0), size, Vec2::new(0.0, size.y)].iter().map(|k| k.distance(own.pos)).fold(0.0f32, f32::max).ceil();
            if (len - self.st.sweep_len).abs() >= 1.0 {
                let mut rs = RenderingServer::singleton();
                rs.canvas_item_clear(item);
                let col = Color::from_rgba(pal.reference.r, pal.reference.g, pal.reference.b, pal.reference.a * 0.6);
                rs.canvas_item_add_line_ex(item, Vector2::ZERO, Vector2::new(0.0, -len), col).width(pal.stroke_rule).antialiased(true).done();
                self.st.sweep_len = len;
            }
        }
        self.seen_view_rev = match &self.st.injected { Some(i) => i.view.rev, None => services().view.try_borrow().map_or(0, |v| v.rev) };
        self.st.picture = picture;
    }
    /// Unit `gd-plot`: mouse hover, click to select or designate, right-click for course, wheel for scale
    /// steps, drag to pan. Emits the signals below; sends no command itself.
    fn gui_input(&mut self, event: Gd<InputEvent>) {
        let mut out = Vec::new();
        let handled = self.input(&event, &mut out);
        if handled { self.base_mut().accept_event(); }
        self.emit(out);
    }
    fn unhandled_input(&mut self, event: Gd<InputEvent>) {
        if !self.st.cursor_shown || !self.base().is_visible_in_tree() { return; }
        let Ok(pad) = event.try_cast::<InputEventJoypadButton>() else { return };
        if !pad.is_pressed() || pad.is_echo() { return; }
        let designate = pad.is_action_pressed("designate");
        let course = pad.is_action_pressed("set_course");
        if !designate && !course { return; }
        let at = gv(self.cursor);
        let button = if designate { MouseButton::LEFT } else { MouseButton::RIGHT };
        // The cursor shows snapped to a track within 24 px: act on that track, as `plot_cursor` reports it.
        let snapped = self.st.picture.hit_within(at, core::SNAP_RADIUS_PX).map(|i| self.st.picture.symbols[i].track).filter(|t| !t.is_none());
        let out = vec![match (snapped, button) {
            (Some(t), b) => Out::Track(t, b),
            (None, b) => self.click_at(at, b),
        }];
        if let Some(mut vp) = self.base().get_viewport() { vp.set_input_as_handled(); }
        self.emit(out);
    }
    fn on_notification(&mut self, what: ControlNotification) {
        match what {
            ControlNotification::THEME_CHANGED => { self.st.palette = None; self.st.widths.borrow_mut().clear(); self.st.sweep_len = 0.0; self.st.dirty = true; }
            ControlNotification::PREDELETE => {
                if let Some(item) = self.st.sweep.take() { RenderingServer::singleton().free_rid(item); }
            }
            ControlNotification::TRANSLATION_CHANGED => { self.st.widths.borrow_mut().clear(); self.st.dirty = true; }
            ControlNotification::VISIBILITY_CHANGED | ControlNotification::RESIZED | ControlNotification::FOCUS_ENTER | ControlNotification::FOCUS_EXIT => { self.st.dirty = true; }
            ControlNotification::MOUSE_EXIT => {
                if !self.hover.is_none() && !self.st.cursor_shown { self.hover = TrackId::NONE; self.st.dirty = true; self.emit(vec![Out::Hover(TrackId::NONE)]); }
            }
            ControlNotification::ACCESSIBILITY_INVALIDATE => self.st.access.invalidate(),
            ControlNotification::ACCESSIBILITY_UPDATE => {
                use crate::text::Lookup;
                let parent = self.base().get_accessibility_element();
                let lk = crate::text::EngineLookup::new();
                let (name, description) = (lk.tr("plot.access.name"), lk.tr("plot.access.description"));
                let selected = self.selected();
                let pic = std::mem::take(&mut self.st.picture);
                self.st.access.update(parent, &pic, selected, &name, &description);
                self.st.picture = pic;
            }
            _ => {}
        }
    }
    fn get_focused_accessibility_element(&self) -> Rid {
        self.st.access.focused(self.selected()).unwrap_or_else(|| self.base().get_accessibility_element())
    }
}

impl PlotState {
    /// Symbol scale (theme `symbol_size` / 24) of the last redraw.
    pub(crate) fn symbol_scale(&self) -> f32 { self.palette.as_ref().map_or(1.0, |p| (p.symbol_size / core::ICON_GRID).clamp(0.5, 4.0)) }
}

impl BpPlotView {
    fn emit(&mut self, out: Vec<Out>) {
        for o in out {
            match o {
                Out::Track(t, b) => { self.base_mut().emit_signal("track_clicked", &[crate::convert::track_to_gd(t).to_variant(), (b.ord() as i64).to_variant()]); }
                Out::Sea(m, b) => { self.base_mut().emit_signal("sea_clicked", &[map_v(m).to_variant(), (b.ord() as i64).to_variant()]); }
                Out::Hover(t) => { self.base_mut().emit_signal("hover_changed", &[crate::convert::track_to_gd(t).to_variant()]); }
                Out::Scale(s) => { self.base_mut().emit_signal("scale_changed", &[(s as i64).to_variant(), (core::step_mpp(s)).to_variant()]); }
                Out::Line(a, b) => { self.base_mut().emit_signal("line_drawn", &[map_v(a).to_variant(), map_v(b).to_variant()]); }
            }
        }
    }

    /// The search-radar sweep: period from the own hull's radiating rotating emitters, angle from sortie time.
    fn update_sweep(&mut self) {
        let Some(item) = self.st.sweep else { return };
        let live = matches!(self.mode, PlotMode::Sortie | PlotMode::Replay) && self.st.injected.is_none() && self.base().is_visible_in_tree();
        let period = if live {
            self.st.hud.as_ref().filter(|h| h.is_instance_valid()).map_or(0.0, |h| {
                let hud = h.bind();
                let (Some(ship), Some(design)) = (hud.ship(), hud.own_design.as_ref()) else { return 0.0 };
                design.emitters.iter()
                    .filter(|e| e.rotation_s.is_finite() && e.rotation_s > 0.0 && ship.board.radiating.contains(&e.slot))
                    .map(|e| e.rotation_s).fold(f64::INFINITY, f64::min)
            })
        } else { 0.0 };
        let period = if period.is_finite() { period } else { 0.0 };
        let own = self.st.picture.symbols.iter().find(|s| s.kind == SymKind::Own).map(|s| s.pos);
        let mut rs = RenderingServer::singleton();
        match (period > 0.0, own) {
            (true, Some(at)) if self.st.sweep_len > 0.0 => {
                let t = services().view.try_borrow().map_or(0.0, |v| v.sim_time_s);
                let turn = if t.is_finite() { (t / period).fract() } else { 0.0 };
                rs.canvas_item_set_transform(item, Transform2D::from_angle_origin((turn * std::f64::consts::TAU) as f32, vg(at)));
                rs.canvas_item_set_visible(item, true);
            }
            _ => rs.canvas_item_set_visible(item, false),
        }
        self.st.sweep_period_s = period;
    }

    /// Shape the label font's common text once, so the first redraw of a sortie does not pay for it.
    fn warm_fonts(&self) {
        let font = self.base().get_theme_font_ex("label").theme_type("Plot").done();
        let size = self.base().get_theme_font_size_ex("label").theme_type("Plot").done();
        let Some(font) = font else { return };
        use crate::text::Lookup;
        let lk = crate::text::EngineLookup::new();
        let mut words: Vec<String> = ["plot.label.own", "plot.label.track", "plot.label.last_known", "plot.label.waypoint", "plot.grid.east", "plot.grid.west", "plot.grid.north", "plot.grid.south"].iter().map(|k| lk.tr(k)).collect();
        words.extend(["sim.word.stance.hostile", "sim.word.stance.suspect", "sim.word.stance.neutral", "sim.word.stance.friend", "units.unit.km", "units.unit.m"].iter().map(|k| lk.tr(k)));
        words.push("0123456789,. ".to_string());
        for w in words { let _ = font.get_string_size_ex(w.as_str()).font_size(size.max(1)).done(); }
    }

    /// The HUD's selection (or the test feed's).
    fn selected(&self) -> TrackId {
        if let Some(i) = &self.st.injected { return i.selected; }
        self.st.hud.as_ref().filter(|h| h.is_instance_valid()).map_or(TrackId::NONE, |h| h.bind().selected)
    }

    fn chart(&self) -> Option<&Chart> {
        self.st.static_chart.as_ref()
            .or_else(|| self.st.injected.as_ref().and_then(|i| i.chart.as_ref()))
            .or_else(|| self.st.sortie_chart.as_ref().map(|(_, c)| c).filter(|_| matches!(self.mode, PlotMode::Sortie | PlotMode::Replay)))
    }

    /// Per-frame state that is kept even while hidden: wall clock, change boxes, the sortie chart.
    fn tick_state(&mut self) {
        let services = services();
        let Ok(feed) = services.feed.try_borrow() else { return };
        self.st.now_s = if feed.now_s.is_finite() { feed.now_s } else { self.st.now_s };
        let now = self.st.now_s;
        for ev in &feed.events {
            let tracks: [TrackId; 2] = match &ev.event {
                SimEvent::RungChanged { track, .. } | SimEvent::StanceChanged { track, .. } => [*track, TrackId::NONE],
                SimEvent::TrackMerged { into, .. } => [*into, TrackId::NONE],
                SimEvent::TrackSplit { from, new, .. } => [*from, *new],
                SimEvent::SortieStarted { .. } => { self.st.boxes.clear(); [TrackId::NONE; 2] }
                _ => [TrackId::NONE; 2],
            };
            for t in tracks.into_iter().filter(|t| !t.is_none()) { self.st.boxes.insert(t, now + BOX_S); }
        }
        let before = self.st.boxes.len();
        self.st.boxes.retain(|_, until| *until > now);
        if self.st.boxes.len() != before { self.st.dirty = true; }

        // The sortie's land layer, traced once per map on a helper thread.
        let key = feed.intro.as_ref().map(|i| MapKey { mode: i.mode, seed: i.seed, operation: i.operation.clone(), players: feed.panel.as_ref().map_or(1, |p| p.players.len().max(1)) });
        drop(feed);
        if let Some((k, rx)) = &self.st.pending_chart {
            match rx.try_recv() {
                Ok(chart) => { self.st.sortie_chart = Some((k.clone(), chart)); self.st.pending_chart = None; self.st.dirty = true; }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => { self.st.pending_chart = None; }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(key) = key {
            let have = self.st.sortie_chart.as_ref().map(|(k, _)| k) == Some(&key) || self.st.pending_chart.as_ref().map(|(k, _)| k) == Some(&key);
            if !have {
                let map = crate::world::picture::sortie_map(&services.campaign(), key.mode, key.seed, &key.operation, key.players);
                let (tx, rx) = std::sync::mpsc::channel();
                let spawned = std::thread::Builder::new().name("plot-chart".into()).spawn(move || { let _ = tx.send(Chart::from_def(&map)); });
                if spawned.is_ok() { self.st.pending_chart = Some((key, rx)); }
            }
        }
    }

    fn signature(&self) -> Sig {
        let size = self.base().get_size();
        let (rev, live, centre) = match &self.st.injected {
            Some(i) => (i.rev, true, picture::own_map_pos(&i.view)),
            None => {
                let services = services();
                let rev = services.feed.try_borrow().map_or(0, |f| f.snapshot_rev);
                let (live, own) = services.view.try_borrow().map_or((false, None), |v| (v.snapshot.is_some(), picture::own_map_pos(&v)));
                (rev, live, own)
            }
        };
        let mpp = core::step_mpp(self.scale_step);
        let centre = if self.follow_own { centre.map_or([0, 0], |c| [(c.x / mpp).round() as i64, (c.y / mpp).round() as i64]) } else { [0, 0] };
        Sig { rev, live, selected: self.selected(), boxes: self.st.boxes.len(), size: [size.x as i32, size.y as i32], centre, chart: self.chart().is_some() }
    }

    /// The frame shown now: the stored centre, or the own hull while following.
    fn frame_now(&self) -> Frame {
        let size = gv(self.base().get_size());
        let mpp = core::step_mpp(self.scale_step);
        let mut centre = DVec2::new(self.centre[0], self.centre[1]);
        if self.follow_own {
            let own = match &self.st.injected { Some(i) => picture::own_map_pos(&i.view), None => services().view.try_borrow().ok().and_then(|v| picture::own_map_pos(&v)) };
            if let Some(o) = own { centre = o; }
        }
        Frame::new(centre, mpp, size)
    }

    fn build_picture(&mut self, pal: &paint::Palette) -> Picture {
        let frame = self.frame_now();
        self.centre = [frame.centre.x, frame.centre.y];
        let services = services();
        let view_guard = services.view.try_borrow().ok();
        let hud_guard = self.st.hud.as_ref().filter(|h| h.is_instance_valid()).map(|h| h.bind());
        let (view, selected, history): (Option<&ViewFrame>, TrackId, Option<&History>) = match &self.st.injected {
            Some(i) => (Some(&i.view), i.selected, Some(&i.history)),
            None => (view_guard.as_deref(), hud_guard.as_ref().map_or(TrackId::NONE, |h| h.selected), hud_guard.as_ref().map(|h| &h.history)),
        };
        let boxed: Vec<TrackId> = self.st.boxes.keys().copied().collect();
        let line = match (self.st.tool_start, self.st.tool_end, self.st.measured) {
            (Some(a), Some(b), _) => Some((a, b, self.tool == "measure")),
            (_, _, Some((a, b))) => Some((a, b, true)),
            _ => None,
        };
        let overlay = Overlay { line, cursor: self.st.cursor_shown.then_some(gv(self.cursor)) };
        let chart = self.chart();
        let inputs = Inputs {
            mode: self.mode, layers: self.layers, frame, view, selected, hovered: self.hover, history, boxed: &boxed, chart,
            route: &self.st.route, belief: &self.st.belief, prev_sides: &self.st.picture.sides, overlay, symbol_size: pal.symbol_size, clear: self.clear, blocked: &self.blocked,
        };
        let (font, size) = (pal.font.clone(), pal.font_size);
        let line_h = font.as_ref().map_or(size as f32 * 1.2, |f| f.get_height_ex().font_size(size).done());
        let env = GodotEnv { lk: crate::text::EngineLookup::new(), font, size, line_h, widths: &self.st.widths, hud: hud_guard.as_deref() };
        picture::build(&inputs, &env)
    }

    /// One input event; returns whether it was used.
    fn input(&mut self, event: &Gd<InputEvent>, out: &mut Vec<Out>) -> bool {
        if let Ok(m) = event.clone().try_cast::<InputEventMouseMotion>() {
            let at = gv(m.get_position());
            if self.st.cursor_shown { self.st.cursor_shown = false; self.st.dirty = true; }
            if let Some(p) = self.st.press.as_mut() {
                if !p.moved && p.at.distance(at) >= core::CLICK_SLOP_PX { p.moved = true; }
                let p = *p;
                let tool_drag = p.button == MouseButton::LEFT && self.tool != "orders";
                if p.moved && !tool_drag && matches!(p.button, MouseButton::LEFT | MouseButton::MIDDLE) {
                    let d = at - p.at;
                    let mpp = core::step_mpp(self.scale_step);
                    let c = p.centre - DVec2::new(d.x as f64, d.y as f64) * mpp;
                    if c.is_finite() { self.centre = [c.x, c.y]; self.follow_own = false; self.st.dirty = true; }
                }
            }
            if self.tool != "orders" {
                let m = self.frame_now().to_map(at);
                if self.st.tool_start.is_some() { self.st.tool_end = Some(m); self.st.dirty = true; }
            }
            let hovered = self.st.picture.hit(at).map(|i| &self.st.picture.symbols[i]).filter(|s| s.kind == SymKind::Track).map_or(TrackId::NONE, |s| s.track);
            if hovered != self.hover { self.hover = hovered; self.st.dirty = true; out.push(Out::Hover(hovered)); }
            return self.st.press.is_some();
        }
        if let Ok(b) = event.clone().try_cast::<InputEventMouseButton>() {
            let at = gv(b.get_position());
            let button = b.get_button_index();
            match button {
                MouseButton::WHEEL_UP | MouseButton::WHEEL_DOWN => {
                    if b.is_pressed() { self.zoom(if button == MouseButton::WHEEL_UP { 1 } else { -1 }, at, out); }
                    return true;
                }
                MouseButton::LEFT | MouseButton::RIGHT | MouseButton::MIDDLE => {}
                _ => return false,
            }
            if b.is_pressed() {
                if self.st.press.is_none() { self.st.press = Some(Press { button, at, centre: self.frame_now().centre, moved: false }); }
                if button == MouseButton::LEFT && self.tool != "orders" && self.st.tool_start.is_none() {
                    // A line or measurement starts where the button went down (drag) or at the first click.
                    let m = self.frame_now().to_map(at);
                    self.st.measured = None;
                    self.st.tool_start = Some(m);
                    self.st.tool_end = Some(m);
                    self.st.dirty = true;
                }
                return true;
            }
            let Some(p) = self.st.press.filter(|p| p.button == button) else { return false };
            self.st.press = None;
            self.release(p, at, out);
            return true;
        }
        if let Ok(g) = event.clone().try_cast::<InputEventPanGesture>() {
            let d = gv(g.get_delta());
            if d.is_finite() && d != Vec2::ZERO {
                let mpp = core::step_mpp(self.scale_step);
                let c = self.frame_now().centre + DVec2::new(d.x as f64, d.y as f64) * mpp * 8.0;
                if c.is_finite() { self.centre = [c.x, c.y]; self.follow_own = false; self.st.dirty = true; }
            }
            return true;
        }
        if let Ok(g) = event.clone().try_cast::<InputEventMagnifyGesture>() {
            let f = g.get_factor();
            if f.is_finite() && f > 0.0 {
                self.st.pinch = if self.st.pinch > 0.0 { self.st.pinch * f } else { f };
                if self.st.pinch >= PINCH_STEP { self.st.pinch = 1.0; self.zoom(1, gv(g.get_position()), out); }
                else if self.st.pinch <= 1.0 / PINCH_STEP { self.st.pinch = 1.0; self.zoom(-1, gv(g.get_position()), out); }
            }
            return true;
        }
        false
    }

    fn release(&mut self, p: Press, at: Vec2, out: &mut Vec<Out>) {
        let tool = self.tool.to_string();
        let frame = self.frame_now();
        match (tool.as_str(), p.button) {
            ("line" | "measure", MouseButton::LEFT) => {
                let Some(a) = self.st.tool_start else { return };
                let b = frame.to_map(at);
                // A click on the start point waits for the second click; a drag or second click finishes.
                if !p.moved && a.distance(b) < core::CLICK_SLOP_PX as f64 * frame.mpp { return; }
                self.st.tool_start = None;
                self.st.tool_end = None;
                if tool == "line" { out.push(Out::Line(a, b)); } else { self.st.measured = Some((a, b)); }
                self.st.dirty = true;
            }
            ("line" | "measure", MouseButton::RIGHT) => {
                self.st.tool_start = None;
                self.st.tool_end = None;
                self.st.measured = None;
                self.st.dirty = true;
            }
            (_, MouseButton::LEFT | MouseButton::RIGHT) if !p.moved => out.push(self.click_at(at, p.button)),
            _ => {}
        }
    }

    /// A click at a local position: on a track symbol or its label, else on the sea.
    fn click_at(&self, at: Vec2, button: MouseButton) -> Out {
        let pic = &self.st.picture;
        let hit = pic.hit(at).map(|i| &pic.symbols[i]).filter(|s| s.kind == SymKind::Track && !s.track.is_none());
        match hit {
            Some(s) => Out::Track(s.track, button),
            None => Out::Sea(self.frame_now().to_map(at), button),
        }
    }

    fn zoom(&mut self, steps: i64, at: Vec2, out: &mut Vec<Out>) {
        let next = core::clamp_step(self.scale_step as i64 - steps.clamp(-8, 8));
        if next == self.scale_step { return; }
        let frame = self.frame_now();
        if !self.follow_own && at.is_finite() {
            let c = core::zoom_about(&frame, core::step_mpp(next), at);
            if c.is_finite() { self.centre = [c.x, c.y]; }
        }
        self.scale_step = next;
        self.st.dirty = true;
        out.push(Out::Scale(next));
    }

    fn track_map_pos(&self, track: TrackId) -> Option<DVec2> {
        let find = |v: &ViewFrame| -> Option<DVec2> {
            let tp = v.tracks.iter().find(|t| t.id == track)?;
            let p = match v.vessels.iter().find(|x| x.track == track && !x.own) {
                Some(x) => { let w = v.origin.to_world(x.pos); DVec2::new(w.x, w.z) }
                None => DVec2::new(tp.world_xz[0], tp.world_xz[1]),
            };
            p.is_finite().then_some(p)
        };
        match &self.st.injected { Some(i) => find(&i.view), None => services().view.try_borrow().ok().and_then(|v| find(&v)) }
    }
}

#[godot_api]
impl BpPlotView {
    /// A track symbol or label was clicked. `button` is a `MouseButton` value; gamepad designate reports
    /// `MOUSE_BUTTON_LEFT`.
    #[signal]
    fn track_clicked(track: i64, button: i64);
    /// Open water was clicked at a map position.
    #[signal]
    fn sea_clicked(map: Vector2, button: i64);
    /// The hovered track changed (`Bp.NONE` = none).
    #[signal]
    fn hover_changed(track: i64);
    /// The scale step changed; `metres_per_px` is at 100% UI scale.
    #[signal]
    fn scale_changed(step: i64, metres_per_px: f64);
    /// The line tool finished a line (mine laying, L793).
    #[signal]
    fn line_drawn(a: Vector2, b: Vector2);

    /// `&"sortie"`, `&"briefing"`, `&"after_action"` or `&"replay"`.
    #[func]
    fn set_mode(&mut self, mode: StringName) {
        let next = match mode.to_string().as_str() {
            "sortie" => PlotMode::Sortie,
            "briefing" => PlotMode::Briefing,
            "after_action" => PlotMode::AfterAction,
            "replay" => PlotMode::Replay,
            other => { godot_warn!("BpPlotView.set_mode: unknown mode {other:?}"); return; }
        };
        if next == self.mode { return; }
        self.mode = next;
        self.follow_own = matches!(next, PlotMode::Sortie | PlotMode::Replay);
        self.st.cursor_shown = false;
        self.st.palette = None;
        self.st.picture = Picture::default();
        self.st.dirty = true;
        self.base_mut().queue_redraw();
        self.base_mut().queue_accessibility_update();
    }
    /// Layers to draw, as `Bp.PLOT_*` bits. Default: everything except history.
    #[func]
    fn set_layers(&mut self, layers: i64) {
        let next = if layers < 0 { layer::DEFAULT } else { layers & layer::ALL };
        if next != self.layers { self.layers = next; self.st.dirty = true; }
    }
    #[func]
    fn layers(&self) -> i64 { self.layers }
    /// Scale step index 0..7 (1 m to 250 m per pixel).
    #[func]
    fn set_scale_step(&mut self, step: i64) {
        let next = core::clamp_step(step);
        if next == self.scale_step { return; }
        self.scale_step = next;
        self.st.dirty = true;
        self.emit(vec![Out::Scale(next)]);
    }
    #[func]
    fn scale_step(&self) -> i64 { self.scale_step as i64 }
    /// Step the scale (positive = closer) keeping the map point under `at` (local position) fixed.
    #[func]
    fn zoom_step(&mut self, steps: i64, at: Vector2) {
        let mut out = Vec::new();
        self.zoom(steps, gv(at), &mut out);
        self.emit(out);
    }
    /// Centre on a map position and stop following the own hull.
    #[func]
    fn centre_on(&mut self, map: Vector2) {
        if !(map.x.is_finite() && map.y.is_finite()) { return; }
        self.centre = [map.x as f64, map.y as f64];
        self.follow_own = false;
        self.st.dirty = true;
    }
    /// Centre on a track (log entries centre on click, rule 15). Returns false for an unknown track.
    #[func]
    fn centre_on_track(&mut self, track: i64) -> bool {
        let Some(t) = u32::try_from(track).ok().filter(|t| *t > 0).map(TrackId) else { return false };
        let Some(p) = self.track_map_pos(t) else { return false };
        self.centre_on(map_v(p));
        true
    }
    /// Margins (local pixels) that HUD clusters cover: the graticule's edge labels sit inside them.
    #[func]
    fn set_clear_margins(&mut self, left: f32, top: f32, right: f32, bottom: f32) {
        let next = [left, top, right, bottom];
        if next != self.clear { self.clear = next; self.st.dirty = true; }
    }
    /// HUD clusters over the plot (local rectangles): track and reference labels take a free side
    /// or hide rather than sit under one (hover still reads them).
    #[func]
    fn set_blocked(&mut self, rects: Array<Rect2>) {
        let next: Vec<self::core::Rect> = rects.iter_shared().map(|r| self::core::Rect::at(Vec2::new(r.position.x, r.position.y), Vec2::new(r.size.x, r.size.y))).collect();
        if next != self.blocked { self.blocked = next; self.st.dirty = true; }
    }
    /// Keep the own hull centred (default on).
    #[func]
    fn set_follow_own(&mut self, on: bool) { if on != self.follow_own { self.follow_own = on; self.st.dirty = true; } }
    /// Map position under a local position, and back.
    #[func]
    fn to_map(&self, local: Vector2) -> Vector2 {
        let m = self.frame_now().to_map(gv(local));
        if m.is_finite() { map_v(m) } else { Vector2::ZERO }
    }
    #[func]
    #[allow(clippy::wrong_self_convention)]
    fn from_map(&self, map: Vector2) -> Vector2 {
        let l = self.frame_now().to_local(DVec2::new(map.x as f64, map.y as f64));
        if l.is_finite() { vg(l) } else { Vector2::ZERO }
    }
    /// Gamepad plot cursor: move by a local delta in pixels; it snaps to the nearest symbol within 24 px.
    #[func]
    fn move_plot_cursor(&mut self, delta: Vector2) {
        let size = gv(self.base().get_size());
        let start = if self.st.cursor_shown { gv(self.cursor) } else { size * 0.5 };
        let tracks: Vec<(Vec2, TrackId)> = self.st.picture.symbols.iter().filter(|s| s.kind == SymKind::Track).map(|s| (s.pos, s.track)).collect();
        let pts: Vec<Vec2> = tracks.iter().map(|t| t.0).collect();
        let (free, _shown, hit) = core::move_cursor(start, gv(delta), size, &pts);
        self.cursor = vg(free);
        self.st.cursor_shown = true;
        self.st.dirty = true;
        let hovered = hit.map_or(TrackId::NONE, |i| tracks[i].1);
        if hovered != self.hover { self.hover = hovered; self.emit(vec![Out::Hover(hovered)]); }
    }
    /// Returns `{ "local": Vector2, "map": Vector2, "track": int, "bearing_deg": float, "range_m": float }`
    /// (bearing and range from the own hull).
    #[func]
    fn plot_cursor(&self) -> VarDictionary {
        let frame = self.frame_now();
        let tracks: Vec<(Vec2, TrackId)> = self.st.picture.symbols.iter().filter(|s| s.kind == SymKind::Track).map(|s| (s.pos, s.track)).collect();
        let pts: Vec<Vec2> = tracks.iter().map(|t| t.0).collect();
        // Before the cursor first moves it rests at the centre.
        let free = if self.st.cursor_shown { gv(self.cursor) } else { frame.half() };
        let snapped = core::nearest_within(&pts, free, core::SNAP_RADIUS_PX);
        let local = snapped.map_or(free, |i| pts[i]);
        let track = snapped.map_or(TrackId::NONE, |i| tracks[i].1);
        let map = frame.to_map(local);
        let own = match &self.st.injected { Some(i) => picture::own_map_pos(&i.view), None => services().view.try_borrow().ok().and_then(|v| picture::own_map_pos(&v)) };
        let (bearing, range) = own.and_then(|o| core::bearing_range(o, map)).unwrap_or((-1.0, -1.0));
        crate::convert::d! {
            "local" => vg(local),
            "map" => if map.is_finite() { map_v(map) } else { Vector2::ZERO },
            "track" => crate::convert::track_to_gd(track),
            "bearing_deg" => bearing,
            "range_m" => range,
        }
    }
    /// `&"orders"` (default: click selects, right-click sets course), `&"line"` (drag or two clicks draw a
    /// line, then `line_drawn`), `&"measure"` (bearing and range between two points).
    #[func]
    fn set_tool(&mut self, tool: StringName) {
        let t = match tool.to_string().as_str() { "line" => "line", "measure" => "measure", "orders" => "orders", other => { godot_warn!("BpPlotView.set_tool: unknown tool {other:?}"); return; } };
        self.tool = StringName::from(t);
        self.st.tool_start = None;
        self.st.tool_end = None;
        self.st.measured = None;
        self.st.press = None;
        self.st.dirty = true;
    }
    /// Tracks whose labels are hidden by declutter near a local position (hover shows hidden, rule 14).
    #[func]
    fn hidden_labels_at(&self, local: Vector2) -> PackedInt32Array {
        let at = gv(local);
        let mut v: Vec<(f32, i32)> = self.st.picture.hidden.iter().map(|(t, p)| (p.distance(at), t.0 as i32)).filter(|(d, _)| *d <= HIDDEN_NEAR_PX).collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        PackedInt32Array::from(v.into_iter().map(|(_, t)| t).collect::<Vec<i32>>().as_slice())
    }
    /// Legend rows for what is drawn now: `Array[Dictionary]` with `symbol: StringName`, `text: StringName`.
    #[func]
    fn legend(&self) -> Array<VarDictionary> {
        self.st.picture.legend.iter().map(|(symbol, text)| crate::convert::d! { "symbol" => StringName::from(*symbol), "text" => StringName::from(*text) }).collect()
    }
    /// Briefing and after-action content: a `MapDict`, the briefing route and, for the after-action report,
    /// the belief samples (`AfterActionDict.belief`). Pass an empty array for what does not apply.
    #[func]
    fn show_static(&mut self, map: VarDictionary, route: PackedVector2Array, belief: PackedVector3Array) {
        let chart = Chart::from_dict(&map);
        self.st.static_chart = (!chart.is_empty()).then_some(chart);
        self.st.route = route.as_slice().iter().map(|p| DVec2::new(p.x as f64, p.y as f64)).filter(|p| p.is_finite()).take(4_096).collect();
        self.st.belief = belief.as_slice().iter().map(|b| [b.x as f64, b.y as f64, b.z as f64]).filter(|b| b.iter().all(|c| c.is_finite())).take(65_536).collect();
        let mut out = Vec::new();
        if matches!(self.mode, PlotMode::Briefing | PlotMode::AfterAction) {
            let size = gv(self.base().get_size());
            let size = if size.x >= 2.0 && size.y >= 2.0 { size } else { Vec2::new(1920.0, 1080.0) };
            // Fit the map, or failing that the route and belief.
            let extent = self.st.static_chart.as_ref().map(|c| c.size_m).filter(|s| s.x > 0.0 && s.y > 0.0);
            let pts: Vec<DVec2> = self.st.route.iter().copied().chain(self.st.belief.iter().map(|b| DVec2::new(b[0], b[1]))).collect();
            let (centre, extent) = match extent {
                Some(e) => (DVec2::ZERO, e),
                None if !pts.is_empty() => {
                    let lo = pts.iter().fold(DVec2::splat(f64::MAX), |a, p| a.min(*p));
                    let hi = pts.iter().fold(DVec2::splat(f64::MIN), |a, p| a.max(*p));
                    ((lo + hi) * 0.5, (hi - lo) * 1.2 + DVec2::splat(500.0))
                }
                None => (DVec2::ZERO, DVec2::ZERO),
            };
            if extent != DVec2::ZERO {
                let step = core::fit_step(extent, size);
                self.centre = [centre.x, centre.y];
                self.follow_own = false;
                if step != self.scale_step { self.scale_step = step; out.push(Out::Scale(step)); }
            }
        }
        self.st.dirty = true;
        self.emit(out);
    }
}
