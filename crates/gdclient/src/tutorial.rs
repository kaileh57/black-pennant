//! Unit `gd-tutorial`. `BpTutorial` (autoload `Tutorial`): the tutorial driver.
//! DESIGN.md L252-275 (the first ten minutes: six steps, two timing gates, four fallback levels, a local log
//! of step timestamps that is never sent). The scripts are data (`crates/sim-systems/data/tutorial.ron`) and
//! the state machine is `sim_systems::tutorial::TutorialRun`; this class feeds it signals and hands its
//! effects to the UI.
//!
//! Signals reach the run from three places: Rust facades push `tutorial::Signal`s into
//! `Services::tutorial_signals` (gauge passed, kit placed, note Fix applied, slider moved, splash started and
//! ended); simulation events become signals through `tutorial::signal_from_event`; the UI reports only
//! `continue_step()` and finished demos. Effects that change the model (load a blueprint, place kits, apply a
//! Fix, set splash options) are carried out here through `Services`; effects for the UI are emitted.
//!
//! # Decisions where the contract is silent
//! - Model effects go to the `Editor` autoload as deferred Godot calls (`open`, `pick_kit` / `ghost_*`,
//!   `note_apply_fix`) and, for the splash options, as a deferred write of `EditorState::splash_options`
//!   (whose doc names the tutorial as a writer). Deferred, because the editor emits signals whose handlers
//!   call straight back into `Tutorial` (`is_allowed`); fallible dynamic calls (`try_call`), so a refused
//!   edit or a method that fails is logged and the remaining operations and the tutorial carry on.
//! - Each splash basin counts as exactly one `SplashStarted` and one `SplashEnded`: the splash facade's own
//!   signals are taken when they come, and a basin the session runs (`SessionKind::Splash` or `Tutorial`)
//!   without one is reported here, so the tutorial can neither stall on a missing signal nor skip a step on a
//!   doubled one.
//! - Steering in a splash (`Orders` throttle or rudder off centre) reports `HelmInput` once per basin, as the
//!   script notes ask of the client.
//! - Wall time feeds the run each frame. A pause (the sortie paused, or the scene tree paused) of at most 2
//!   minutes counts; a longer one, and any single frame gap over 2 minutes (the app suspended), is dropped
//!   (L79). `Signal::Elapsed` goes in once a second so the timed escapes fire without other signals.
//! - The local log (L266) is the plain-text form of `TutorialLog`: `user://logs/tutorial_current.txt` is
//!   rewritten on every log row while a run is going, and each run is appended to
//!   `user://logs/tutorial_log.txt` when it finishes or is left. With Settings > Playtest > timing log off
//!   neither file is written; the in-memory log for `log_text` is kept either way. Nothing is sent anywhere.
//! - `start(Bp.NONE)` uses `editor.tutorial_level`, clamped to the shipped levels 0..=4.
//! - `needs_continue` is true for a step whose trigger waits on Continue and that plays no demo (a demo step
//!   continues when its animation ends).
//! - A new step plays `Bp.UI_SOUND_TUTORIAL_STEP`.
//! - A hull the tutorial loads is renamed "Tutorial fast attack craft" (`tutorial_overlay.hull_name`, one undo
//!   step), so the hull Splash saves for the player and the stock reward copied at the finish never share a
//!   name in the blueprint list.
use crate::enums::{GdEnum, SessionKind, SessionState, TutorialState};
use crate::services::{services, Services};
use godot::classes::file_access::ModeFlags;
use godot::classes::{DirAccess, FileAccess, ProjectSettings};
use godot::prelude::*;
use sim_core::EntityId;
use sim_systems::tutorial::{Effect, Signal, Step, Trigger, TutorialLog, TutorialRun};

/// The tutorial driver. Autoload `Tutorial`.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct BpTutorial {
    pub(crate) run: Option<Box<sim_systems::tutorial::TutorialRun>>,
    pub(crate) state: TutorialState,
    pub(crate) second_accum: f64,
    /// Everything else the driver keeps between frames.
    pub(crate) drv: Driver,
    base: Base<Node>,
}

/// Plain-data state of the driver besides the contract fields. `Default` is inert.
#[derive(Default)]
pub(crate) struct Driver {
    /// The log of the last run, kept after `stop` and after the run finished (`log_text`).
    pub(crate) last_log: Option<TutorialLog>,
    /// Seconds of the pause under way (counted only if it ends within [`PAUSE_LIMIT_S`]).
    pub(crate) pause_s: f64,
    /// Whole seconds sent as `Signal::Elapsed` in the current step, and that step's index.
    pub(crate) elapsed_sent: u32,
    pub(crate) elapsed_step: usize,
    /// The splash basin bookkeeping (see the module notes).
    pub(crate) splash: SplashWatch,
    /// The player's hull in the running basin.
    pub(crate) player_vessel: Option<EntityId>,
    /// Index of the step `step_changed` last announced, so a repeated `Show` is not announced twice.
    pub(crate) announced: Option<usize>,
    /// The step and its progress leaves as last reported, for `progress_changed`.
    pub(crate) progress_seen: Option<(usize, Vec<bool>)>,
}

/// One `SplashStarted` and one `SplashEnded` per basin.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct SplashWatch {
    /// A basin session is loading or running.
    pub(crate) live: bool,
    /// `SplashStarted` has been counted for the current (or the announced, not yet loaded) basin.
    pub(crate) started_counted: bool,
    /// Seconds since a facade `SplashStarted` arrived with no basin yet.
    pub(crate) waiting_s: f64,
    /// `HelmInput` was reported in this basin.
    pub(crate) helm_reported: bool,
    /// `PlayerGunsFired` was reported in this basin from an own mount engaging.
    pub(crate) fire_reported: bool,
}

/// Pauses longer than this are dropped from the timing (L79).
const PAUSE_LIMIT_S: f64 = 120.0;
/// A facade `SplashStarted` with no basin after this long no longer covers the next basin.
const SPLASH_WAIT_S: f64 = 5.0;
/// Steering input beyond this counts as taking the helm.
const HELM_DEADZONE: f32 = 0.1;
const LOG_DIR: &str = "user://logs";
const LOG_CURRENT: &str = "user://logs/tutorial_current.txt";
const LOG_FILE: &str = "user://logs/tutorial_log.txt";
/// Highest shipped fallback level.
const MAX_LEVEL: i64 = 4;

/// What one batch of effects asks for, carried out after the run is updated.
#[derive(Debug, Default, PartialEq)]
struct Outcome {
    /// Announce the current step.
    step: bool,
    demo: Option<String>,
    finished: Option<String>,
    /// Model effects for the editor, in order.
    editor: Vec<EditorOp>,
    /// Log rows were added.
    logged: bool,
}

/// A change to the dry dock the tutorial makes for the player.
#[derive(Clone, Debug, PartialEq)]
enum EditorOp {
    /// `Editor.open(DOCK_TUTORIAL, { blueprint })` with a `stock:` path.
    Load(String),
    /// Place one kit at a cell (pick, move, place, cancel).
    PlaceKit { kit: String, cell: [i32; 3] },
    /// Apply the Fix of the first open note with this id.
    Fix(String),
    SplashOptions(content::settings::SplashOptions),
}

#[godot_api]
impl INode for BpTutorial {
    fn init(base: Base<Node>) -> Self { Self { run: None, state: TutorialState::Off, second_accum: 0.0, drv: Driver::default(), base } }
    fn ready(&mut self) { self.base_mut().set_process_priority(-80); }
    /// Unit `gd-tutorial`: tick the run, drain `Services::tutorial_signals` and this frame's simulation
    /// events into it, carry out and emit the effects. Inert while off.
    fn process(&mut self, delta: f64) {
        let services = services();
        // The queue is drained every frame, also while off: facades push whatever happens.
        let queued: Vec<Signal> = std::mem::take(&mut *services.tutorial_signals.borrow_mut());
        if self.state != TutorialState::Running || self.run.is_none() { return; }
        let paused = self.is_paused(&services);
        self.advance_clock(delta, paused);
        let mut signals = self.splash_signals(queued, &services, delta);
        signals.extend(self.sim_signals(&services));
        drop(services);
        // Once a second, the step's age (timed escapes are checked on signals only).
        if delta.is_finite() && delta > 0.0 && delta <= PAUSE_LIMIT_S && !paused { self.second_accum += delta; }
        if self.second_accum >= 1.0 {
            self.second_accum %= 1.0;
            if let Some(s) = self.elapsed_signal() { signals.push(s); }
        }
        if signals.is_empty() { return; }
        let out = self.feed(&signals);
        self.carry_out(out);
    }
}

#[godot_api]
impl BpTutorial {
    /// A step began (`TutorialStepDict`): show its text, point at its control, enable only what it allows.
    #[signal]
    fn step_changed(step: VarDictionary);
    /// Play a named client animation instead of asking the player (fallback levels): the UI calls
    /// `demo_finished(name)` when it ends.
    #[signal]
    fn demo_requested(name: GString);
    /// The tutorial finished; `reward` is the stock path of the fully fitted hull it grants.
    #[signal]
    fn finished(reward: GString);
    /// Part of the current step was done (a lesson kit placed): pull `step()` again for its progress.
    #[signal]
    fn progress_changed();

    /// Start the tutorial at a fallback level: 0 = full, 1..4 = the fallbacks in order; `Bp.NONE` = the level
    /// stored in settings. Opens the dry dock on the tutorial hull. Returns false when the script is missing.
    #[func]
    fn start(&mut self, level: i64) -> bool {
        let level = if level == -1 { services().settings.borrow().int("editor.tutorial_level").clamp(0, MAX_LEVEL) } else { level };
        let Ok(level) = u8::try_from(level) else { return false };
        let Some(script) = sim_systems::tutorial::script_for(level) else {
            godot_warn!("tutorial: no script for fallback level {level}");
            return false;
        };
        // A run already going is left first (its log is kept and written).
        if self.run.is_some() { self.end_run(); }
        let blueprint = script.blueprint.clone();
        let (run, effects) = TutorialRun::start(script);
        self.run = Some(Box::new(run));
        self.state = TutorialState::Running;
        self.second_accum = 0.0;
        self.drv = Driver::default();
        // Edge-triggered reports from before the start belong to whatever was on screen then.
        services().tutorial_signals.borrow_mut().clear();
        let mut out = Outcome::default();
        self.collect(effects, &mut out);
        // The dock always opens on the script's hull, even when its first step does not load it.
        if !out.editor.iter().any(|op| matches!(op, EditorOp::Load(_))) { out.editor.insert(0, EditorOp::Load(blueprint)); }
        out.step = true;
        self.carry_out(out);
        true
    }
    /// Leave the tutorial; the timing log so far is kept.
    #[func]
    fn stop(&mut self) {
        self.end_run();
        self.state = TutorialState::Off;
    }
    /// `Bp.TUTORIAL_*`.
    #[func]
    fn state(&self) -> i64 { use crate::enums::GdEnum; self.state.to_gd() }
    /// The current `TutorialStepDict`, or `null` when off.
    #[func]
    fn step(&self) -> Variant {
        match self.current_step() {
            Some(_) if self.state == TutorialState::Running => self.run.as_deref().map_or(Variant::nil(), |r| step_dict(r).to_variant()),
            _ => Variant::nil(),
        }
    }
    /// The player dismissed a text-only step.
    #[func]
    fn continue_step(&mut self) {
        if self.state != TutorialState::Running { return; }
        let out = self.feed(&[Signal::Continue]);
        self.carry_out(out);
    }
    /// A demo animation requested by `demo_requested` ended.
    #[func]
    fn demo_finished(&mut self, name: GString) {
        if self.state != TutorialState::Running { return; }
        // A late end of an earlier step's demo must not dismiss the step now showing.
        let current = self.current_step().and_then(|(_, _, s)| demo_of(s));
        if current.as_deref() != Some(name.to_string().as_str()) { return; }
        let out = self.feed(&[Signal::Continue]);
        self.carry_out(out);
    }
    /// Whether a control (stable id such as `editor.handle.beam`, `editor.tray.kit.gun`, `editor.notes.fix`,
    /// `editor.launch`, `hud.slider.aggression`) is enabled in the current step. Always true while off.
    #[func]
    fn is_allowed(&self, control_id: GString) -> bool {
        match (&self.run, self.state) {
            (Some(run), TutorialState::Running) => run.allows(&control_id.to_string()),
            _ => true,
        }
    }
    /// The timing log as plain text for the clipboard and the GitHub issue form (L266).
    #[func]
    fn log_text(&self) -> GString {
        let empty = TutorialLog::default();
        let log = self.run.as_ref().map(|r| &r.log).or(self.drv.last_log.as_ref()).unwrap_or(&empty);
        GString::from(sim_systems::tutorial::log_text(log).as_str())
    }
    /// True once the tutorial has been finished on this install (the main menu then offers it as optional).
    #[func]
    fn was_finished(&self) -> bool { services().settings.borrow().bool("editor.tutorial_finished") }
}

// -------------------------------------------------------------------------------------------------------
// Driving the run
// -------------------------------------------------------------------------------------------------------

impl BpTutorial {
    fn current_step(&self) -> Option<(usize, usize, &Step)> {
        let run = self.run.as_ref()?;
        if run.finished { return None; }
        run.current().map(|s| (run.step, run.script.steps.len(), s))
    }

    /// Feed signals one by one, collecting what they ask for.
    fn feed(&mut self, signals: &[Signal]) -> Outcome {
        let mut out = Outcome::default();
        for s in signals {
            let Some(run) = self.run.as_mut() else { break };
            let effects = run.signal(s);
            self.collect(effects, &mut out);
        }
        out
    }

    /// Sort effects into UI announcements and editor operations.
    fn collect(&mut self, effects: Vec<Effect>, out: &mut Outcome) {
        for e in effects {
            match e {
                Effect::LoadBlueprint(path) => out.editor.push(EditorOp::Load(path)),
                Effect::PlaceKits(kits) => out.editor.extend(kits.into_iter().map(|(kit, cell)| EditorOp::PlaceKit { kit: kit.as_str().to_string(), cell: [cell.x as i32, cell.y as i32, cell.z as i32] })),
                Effect::ApplyNoteFix(note) => out.editor.push(EditorOp::Fix(note)),
                Effect::SplashOptions(o) => out.editor.push(EditorOp::SplashOptions(o)),
                Effect::Show { .. } | Effect::Allow(_) => out.step = true,
                Effect::Demo(name) => out.demo = Some(name),
                Effect::Reward(path) => out.finished = Some(path),
                Effect::Finished => { if out.finished.is_none() { out.finished = Some(String::new()); } }
                Effect::Log { .. } => out.logged = true,
            }
        }
    }

    /// Carry out a batch: editor operations (deferred), the log file, then the signals. State is updated
    /// before anything is emitted, because the handlers call straight back in.
    fn carry_out(&mut self, out: Outcome) {
        if !out.editor.is_empty() { self.queue_editor(out.editor.clone()); }
        if out.logged { self.write_current_log(); }
        let current = self.current_step().map(|(i, _, s)| (i, demo_of(s))).and_then(|(i, d)| self.run.as_deref().map(|r| (i, step_dict(r), d)));
        if let Some((index, _, _)) = &current {
            if *index != self.drv.elapsed_step { self.drv.elapsed_step = *index; self.drv.elapsed_sent = 0; }
        }
        // The demo belongs to the step now showing; a demo of a step that chained past is not played.
        let demo = match (&out.demo, &current) { (Some(d), Some((_, _, Some(cd)))) if d == cd => Some(d.clone()), _ => None };
        let step_payload = current.as_ref().filter(|(i, _, _)| out.step && (self.drv.announced != Some(*i) || demo.is_some())).map(|(i, d, _)| (*i, d.clone()));
        let finished = out.finished.clone().filter(|_| self.run.as_ref().is_some_and(|r| r.finished));
        if finished.is_some() { self.finish(); }
        let announcing = step_payload.is_some();
        if let Some((index, dict)) = step_payload {
            self.drv.announced = Some(index);
            services().audio.borrow_mut().ui(crate::seams::sound::UiSfx::TutorialStep);
            self.base_mut().emit_signal("step_changed", &[dict.to_variant()]);
        }
        if let Some(name) = demo { self.base_mut().emit_signal("demo_requested", &[GString::from(name.as_str()).to_variant()]); }
        // Progress inside the step on screen (a lesson kit placed): the card re-reads the step.
        let progress = self.run.as_ref().filter(|r| !r.finished).map(|r| (r.step, r.progress.clone()));
        if progress != self.drv.progress_seen {
            let same_step = matches!((&progress, &self.drv.progress_seen), (Some((a, _)), Some((b, _))) if a == b);
            self.drv.progress_seen = progress;
            if same_step && !announcing { self.base_mut().emit_signal("progress_changed", &[]); }
        }
        self.sync_spots();
        if let Some(reward) = finished { self.base_mut().emit_signal("finished", &[GString::from(reward.as_str()).to_variant()]); }
    }

    /// Publish the lesson kits' marked spots to the dock while the step on screen waits on kits.
    fn sync_spots(&self) {
        let spots = match self.run.as_deref() { Some(r) if !r.finished && self.state == TutorialState::Running && r.wants_kits() => r.script.spots.clone(), _ => Vec::new() };
        let services = services();
        let mut cur = services.tutorial_spots.borrow_mut();
        if *cur != spots { *cur = spots; }
    }

    /// The run finished: the install remembers it, the log is written, the state moves on.
    fn finish(&mut self) {
        crate::settings::write("editor.tutorial_finished", crate::settings::SettingValue::Bool(true));
        self.end_run();
        self.state = TutorialState::Finished;
    }

    /// Keep and write the log, forget the run.
    fn end_run(&mut self) {
        let Some(run) = self.run.take() else { return };
        let log = run.log.clone();
        if timing_log_on() {
            let text = sim_systems::tutorial::log_text(&log);
            append_file(LOG_FILE, &format!("{text}\n"));
            write_file(LOG_CURRENT, &text);
        }
        self.drv.last_log = Some(log);
        self.drv.announced = None;
        self.drv.player_vessel = None;
        self.drv.splash = SplashWatch::default();
        self.drv.progress_seen = None;
        services().tutorial_spots.borrow_mut().clear();
    }

    fn write_current_log(&self) {
        if !timing_log_on() { return; }
        if let Some(run) = &self.run { write_file(LOG_CURRENT, &sim_systems::tutorial::log_text(&run.log)); }
    }

    /// `Signal::Elapsed` for the current step when a new whole second has passed.
    fn elapsed_signal(&mut self) -> Option<Signal> {
        let run = self.run.as_ref()?;
        if run.step != self.drv.elapsed_step { self.drv.elapsed_step = run.step; self.drv.elapsed_sent = 0; }
        let age = run.elapsed_s - run.step_started_s;
        let whole = if age.is_finite() && age > 0.0 { age.floor().min(u32::MAX as f64) as u32 } else { 0 };
        if whole == 0 || whole <= self.drv.elapsed_sent { return None; }
        self.drv.elapsed_sent = whole;
        Some(Signal::Elapsed { seconds: whole })
    }

    /// The sortie or the scene tree is paused.
    fn is_paused(&self, services: &Services) -> bool {
        let sortie = services.feed.borrow().snapshot.as_ref().is_some_and(|s| s.pacing.paused);
        let tree = self.base().is_inside_tree() && self.base().get_tree().is_paused();
        sortie || tree
    }

    /// Advance the run's clock by this frame, dropping pauses over 2 minutes.
    fn advance_clock(&mut self, delta: f64, paused: bool) {
        let Some(run) = self.run.as_mut() else { return };
        let (ticked, pause) = clock_step(delta, paused, self.drv.pause_s);
        let counted_pause = paused_part(delta, paused, self.drv.pause_s, ticked);
        self.drv.pause_s = pause;
        tick_run(run, ticked, counted_pause);
    }

    /// Splash start and end from the facades' signals and from the session, once per basin; helm input.
    fn splash_signals(&mut self, queued: Vec<Signal>, services: &Services, delta: f64) -> Vec<Signal> {
        let live = {
            let feed = services.feed.borrow();
            matches!(feed.kind, SessionKind::Splash | SessionKind::Tutorial) && matches!(feed.state, SessionState::Loading | SessionState::InSortie)
        };
        let helm = live && {
            let orders = self.base().get_node_or_null("/root/Orders").and_then(|n| n.try_cast::<crate::orders::BpOrders>().ok());
            orders.is_some_and(|o| { let i = o.bind().input; i.throttle.abs() > HELM_DEADZONE || i.rudder.abs() > HELM_DEADZONE })
        };
        let (signals, watch) = splash_filter(queued, self.drv.splash, live, helm, if delta.is_finite() { delta.max(0.0) } else { 0.0 });
        self.drv.splash = watch;
        if !live { self.drv.player_vessel = None; }
        signals
    }

    /// This frame's simulation events as signals, in a tutorial or splash basin only.
    fn sim_signals(&mut self, services: &Services) -> Vec<Signal> {
        let feed = services.feed.borrow();
        if !matches!(feed.kind, SessionKind::Splash | SessionKind::Tutorial) || !matches!(feed.state, SessionState::InSortie | SessionState::Loading) { return Vec::new(); }
        if let Some(v) = feed.snapshot.as_ref().and_then(|s| s.ship.as_ref()).map(|s| s.vessel).filter(|v| !v.is_none()) { self.drv.player_vessel = Some(v); }
        // A reveal of the player's hull may come before the first sampled frame.
        let slot = feed.welcome.as_ref().map(|w| w.slot).unwrap_or(content::ids::PlayerSlot(0));
        let mut out = Vec::new();
        // The own mounts as the snapshot reports them: a weapon engaging a track is firing on it. This does not
        // depend on the fire event reaching this frame's feed (the drone step waits on it, L263).
        let engaging = feed.snapshot.as_ref().and_then(|s| s.ship.as_ref()).is_some_and(|s| s.mounts.iter().any(|m| matches!(m.status, sim_systems::protocol::text::EngageStatus::Engaging { .. })));
        if engaging && !self.drv.splash.fire_reported {
            self.drv.splash.fire_reported = true;
            out.push(Signal::PlayerGunsFired);
        }
        for e in &feed.events {
            if let sim_systems::protocol::event::SimEvent::EntityRevealed(intro) = &e.event {
                if intro.player == Some(slot) { self.drv.player_vessel = Some(intro.entity); }
            }
            let vessel = self.drv.player_vessel.unwrap_or(EntityId::NONE);
            if let Some(s) = sim_systems::tutorial::signal_from_event(&e.event, vessel) { out.push(s); }
        }
        out
    }

    /// Hand the editor operations to the `Editor` autoload at idle time.
    fn queue_editor(&mut self, ops: Vec<EditorOp>) {
        let Some(mut editor) = self.base().get_node_or_null("/root/Editor") else {
            godot_warn!("tutorial: no Editor autoload; {} dry-dock changes not made", ops.len());
            return;
        };
        editor.run_deferred_gd(move |mut ed: Gd<Node>| {
            for op in ops { apply_editor_op(&mut ed, op); }
        });
    }
}

/// Text key of the name the hull built in the tutorial is saved under (`game/i18n/ui_tutorial.csv`).
const TUTORIAL_HULL_NAME: &str = "tutorial_overlay.hull_name";

/// One editor operation, through the editor's own methods.
fn apply_editor_op(ed: &mut Gd<Node>, op: EditorOp) {
    match op {
        EditorOp::Load(path) => {
            let path = stock_path(&path);
            let opts = crate::convert::d! { "blueprint" => GString::from(path.as_str()) };
            let issues = ed.try_call("open", &[crate::enums::EditorMode::Tutorial.to_gd().to_variant(), opts.to_variant()]);
            // `open` returns a typed `Array[Dictionary]`.
            if let Ok(list) = issues.as_ref().map_err(|_| ()).and_then(|v| v.try_to::<Array<VarDictionary>>().map_err(|_| ())) {
                if list.iter_shared().any(|d| crate::convert::get_bool(&d, "fatal").unwrap_or(false)) {
                    godot_warn!("tutorial: the dry dock did not open {path}");
                }
            }
            let opened = ed.try_call("is_open", &[]).ok().and_then(|v| v.try_to::<bool>().ok()).unwrap_or(false);
            // The tutorial hulls carry the stock name of the reward ("Fast attack craft"); the hull the player
            // builds here is saved by Splash, so it takes its own name and the blueprint list never shows two
            // designs with the same name.
            if opened {
                let name = godot::classes::TranslationServer::singleton().translate(TUTORIAL_HULL_NAME);
                let _ = ed.try_call("set_design_name", &[GString::from(name.to_string().as_str()).to_variant()]);
            }
        }
        EditorOp::PlaceKit { kit, cell } => {
            let picked = ed.try_call("pick_kit", &[StringName::from(kit.as_str()).to_variant()]).ok().and_then(|v| v.try_to::<bool>().ok()).unwrap_or(false);
            if !picked { godot_warn!("tutorial: kit {kit} could not be picked"); return; }
            let _ = ed.try_call("ghost_move", &[Vector3i::new(cell[0], cell[1], cell[2]).to_variant()]);
            let placed = ed.try_call("ghost_place", &[]).ok().and_then(|v| v.try_to::<PackedInt32Array>().ok()).map_or(0, |a| a.len());
            let _ = ed.try_call("ghost_cancel", &[]);
            if placed == 0 { godot_warn!("tutorial: kit {kit} does not fit at {cell:?}"); }
        }
        EditorOp::Fix(note) => {
            let index = match ed.clone().try_cast::<crate::editor::BpEditor>() {
                Ok(e) => e.bind().model.as_ref().and_then(|m| m.notes.1.iter().position(|n| sim_systems::tutorial::note_id(&n.kind) == note)),
                Err(_) => None,
            };
            // A Fix for a note that is not open is a no-op (the script notes).
            if let Some(i) = index {
                if ed.try_call("note_apply_fix", &[(i as i64).to_variant()]).is_err() { godot_warn!("tutorial: the Fix for {note} could not be applied"); }
            }
        }
        EditorOp::SplashOptions(o) => {
            if let Ok(mut e) = ed.clone().try_cast::<crate::editor::BpEditor>() {
                let mut b = e.bind_mut();
                if b.state.splash_options != o {
                    b.state.splash_options = o;
                    // The pre-built basin was for the old options.
                    b.state.prebuild_rev = 0;
                }
            }
        }
    }
}

// -------------------------------------------------------------------------------------------------------
// Pure helpers
// -------------------------------------------------------------------------------------------------------

/// A script's blueprint path (relative to `blueprints/`) as the dock's `stock:` form.
fn stock_path(path: &str) -> String {
    let p = path.trim();
    if p.starts_with("stock:") || p.starts_with("user://") || p.starts_with("res://") { p.to_string() } else { format!("stock:{p}") }
}

/// The demo a step plays on entry, if any.
fn demo_of(step: &Step) -> Option<String> {
    step.on_enter.iter().find_map(|e| if let Effect::Demo(name) = e { Some(name.clone()) } else { None })
}

/// Whether a trigger waits on the Continue button anywhere in its tree.
fn waits_on_continue(t: &Trigger) -> bool {
    match t {
        Trigger::Continue => true,
        Trigger::All(ts) | Trigger::Any(ts) => ts.iter().any(waits_on_continue),
        _ => false,
    }
}

/// Control ids of the gauges a step's completion watches (`GaugePass`), for example `editor.gauge.float` on
/// the beam step: the overlay keeps its card off them.
pub(crate) fn watched_gauges(t: &Trigger, out: &mut Vec<String>) {
    match t {
        Trigger::GaugePass(g) => { let id = format!("editor.gauge.{g}"); if !out.contains(&id) { out.push(id); } }
        Trigger::All(ts) | Trigger::Any(ts) => for t in ts { watched_gauges(t, out); },
        _ => {}
    }
}

/// `TutorialStepDict` for the run's current step (an empty dictionary when it has none).
fn step_dict(run: &TutorialRun) -> VarDictionary {
    let Some(step) = run.current() else { return VarDictionary::new() };
    let demo = demo_of(step);
    let allow: PackedStringArray = step.allow.iter().map(|s| GString::from(s.as_str())).collect();
    let mut watched = Vec::new();
    watched_gauges(&step.complete, &mut watched);
    let watch: PackedStringArray = watched.iter().map(|s| GString::from(s.as_str())).collect();
    let (want, have) = run.kits_progress();
    let ids = |v: &[content::ids::KitId]| -> PackedStringArray { v.iter().map(|k| GString::from(k.as_str())).collect() };
    crate::convert::d! {
        "id" => GString::from(step.id.as_str()),
        "index" => run.step as i64,
        "count" => run.script.steps.len() as i64,
        "text" => StringName::from(step.text.0.as_str()),
        "point_at" => GString::from(step.point_at.as_deref().unwrap_or("")),
        "allow" => allow,
        "watch" => watch,
        "demo" => GString::from(demo.as_deref().unwrap_or("")),
        "needs_continue" => demo.is_none() && waits_on_continue(&step.complete),
        "lesson" => step.lesson as i64,
        "lessons" => run.script.lessons() as i64,
        "after" => StringName::from(run.previous_done().map_or("", |k| k.0.as_str())),
        "kits_wanted" => ids(&want),
        "kits_fitted" => ids(&have),
    }
}

/// One frame of the run's clock: `(seconds to tick, pause carried)`. Paused time is held until the pause
/// ends, then counted when it lasted at most [`PAUSE_LIMIT_S`]; a frame gap over the limit is dropped.
fn clock_step(delta: f64, paused: bool, pause_s: f64) -> (f64, f64) {
    let d = if delta.is_finite() && delta > 0.0 { delta } else { 0.0 };
    if d > PAUSE_LIMIT_S { return (0.0, if paused { pause_s } else { 0.0 }); }
    if paused { return (0.0, pause_s + d); }
    let carried = if pause_s > 0.0 && pause_s <= PAUSE_LIMIT_S { pause_s } else { 0.0 };
    (d + carried, 0.0)
}

/// The part of a frame's ticked seconds that is a short pause counted on resume (0 on any other frame).
fn paused_part(delta: f64, paused: bool, pause_s: f64, ticked: f64) -> f64 {
    let d = if delta.is_finite() && delta > 0.0 { delta } else { 0.0 };
    if paused || d > PAUSE_LIMIT_S || ticked <= 0.0 || !(pause_s > 0.0 && pause_s <= PAUSE_LIMIT_S) { 0.0 } else { pause_s }
}

/// Tick the run by `ticked` seconds of which `paused` were a short pause. The run's clock keeps the pause
/// (the L79 total-time measurement counts pauses of up to 2 minutes, and the log stamps read it), but the
/// current step's start moves on by the same amount, so the step's age, and with it its `After` and `Dwell`
/// timers, counts only time played: a paused player neither burns a step's escape nor sits out a dwell.
fn tick_run(run: &mut TutorialRun, ticked: f64, paused: f64) {
    if ticked.is_nan() || ticked <= 0.0 { return; }
    run.tick(ticked);
    if paused > 0.0 && paused.is_finite() { run.step_started_s += paused.min(ticked); }
}

/// The splash rules of the module notes: the facades' queued signals and the basin's observed state in,
/// the signals to feed out. Other queued signals pass through unchanged and in order.
fn splash_filter(queued: Vec<Signal>, mut w: SplashWatch, live: bool, helm: bool, delta: f64) -> (Vec<Signal>, SplashWatch) {
    let mut out = Vec::with_capacity(queued.len() + 2);
    for s in queued {
        match s {
            Signal::SplashStarted => {
                if w.started_counted { continue; }
                w.started_counted = true;
                w.waiting_s = 0.0;
                out.push(Signal::SplashStarted);
            }
            Signal::SplashEnded => {
                // Only a counted basin can end.
                if !w.started_counted { continue; }
                w.live = false;
                w.started_counted = false;
                w.helm_reported = false;
                w.fire_reported = false;
                out.push(Signal::SplashEnded);
            }
            other => out.push(other),
        }
    }
    if live && !w.live {
        w.live = true;
        w.waiting_s = 0.0;
        w.helm_reported = false;
        w.fire_reported = false;
        if !w.started_counted { w.started_counted = true; out.push(Signal::SplashStarted); }
    } else if !live && w.live {
        w.live = false;
        w.helm_reported = false;
        w.fire_reported = false;
        if w.started_counted { w.started_counted = false; out.push(Signal::SplashEnded); }
    } else if !live && w.started_counted {
        // Announced by the facade and not loaded yet: give the session a few seconds.
        w.waiting_s += delta;
        if w.waiting_s > SPLASH_WAIT_S { w.started_counted = false; w.waiting_s = 0.0; }
    }
    if live && helm && !w.helm_reported { w.helm_reported = true; out.push(Signal::HelmInput); }
    (out, w)
}

fn timing_log_on() -> bool { services().settings.borrow().bool("playtest.timing_log") }

fn ensure_log_dir() {
    let dir = ProjectSettings::singleton().globalize_path(LOG_DIR);
    if !DirAccess::dir_exists_absolute(&dir) { let _ = DirAccess::make_dir_recursive_absolute(&dir); }
}
fn write_file(path: &str, text: &str) {
    ensure_log_dir();
    match FileAccess::open(path, ModeFlags::WRITE) {
        Some(mut f) => { f.store_string(text); f.close(); }
        None => { godot_warn!("tutorial: could not write {path}"); }
    }
}
fn append_file(path: &str, text: &str) {
    ensure_log_dir();
    let file = if FileAccess::file_exists(path) { FileAccess::open(path, ModeFlags::READ_WRITE) } else { FileAccess::open(path, ModeFlags::WRITE) };
    match file {
        Some(mut f) => { f.seek_end(); f.store_string(text); f.close(); }
        None => { godot_warn!("tutorial: could not write {path}"); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_beam_step_watches_the_float_gauge() {
        let mut w = Vec::new();
        watched_gauges(&Trigger::Any(vec![Trigger::GaugePass("float".into()), Trigger::After { seconds: 240 }]), &mut w);
        assert_eq!(w, ["editor.gauge.float"]);
        let mut none = Vec::new();
        watched_gauges(&Trigger::Continue, &mut none);
        assert!(none.is_empty());
    }

    #[test]
    fn short_pauses_count_and_long_ones_are_dropped() {
        assert_eq!(clock_step(0.016, false, 0.0), (0.016, 0.0));
        // A 60 s pause is held, then counted when play resumes.
        let mut held = 0.0;
        for _ in 0..60 { held = clock_step(1.0, true, held).1; }
        let (t, p) = clock_step(0.5, false, held);
        assert!((t - 60.5).abs() < 1e-9 && p == 0.0);
        // A 3 minute pause is dropped.
        let mut held = 0.0;
        for _ in 0..180 { held = clock_step(1.0, true, held).1; }
        let (t, _) = clock_step(0.5, false, held);
        assert!((t - 0.5).abs() < 1e-9);
        // A single gap over 2 minutes (the app suspended) and bad deltas count nothing.
        assert_eq!(clock_step(300.0, false, 0.0), (0.0, 0.0));
        assert_eq!(clock_step(f64::NAN, false, 0.0), (0.0, 0.0));
        assert_eq!(clock_step(-1.0, false, 0.0), (0.0, 0.0));
    }

    #[test]
    fn step_timers_exclude_pauses_but_the_total_keeps_them() {
        let script = sim_systems::tutorial::Script {
            id: "t".into(), fallback_level: 0, blueprint: "b".into(), reward: "r".into(), spots: vec![],
            steps: vec![
                Step { id: "wait".into(), text: content::ids::TextKey::new("tutorial.wait"), point_at: None, allow: vec![], on_enter: vec![], on_escape: vec![], complete: Trigger::After { seconds: 30 }, marks_first_fight: false, marks_first_kill: false, lesson: 1, done: None },
                Step { id: "end".into(), text: content::ids::TextKey::new("tutorial.end"), point_at: None, allow: vec![], on_enter: vec![], on_escape: vec![], complete: Trigger::Continue, marks_first_fight: false, marks_first_kill: false, lesson: 2, done: None },
            ],
        };
        let (mut run, _) = TutorialRun::start(script);
        let mut pause = 0.0;
        let frame = |run: &mut TutorialRun, delta: f64, paused: bool, pause: &mut f64| {
            let (ticked, held) = clock_step(delta, paused, *pause);
            let counted = paused_part(delta, paused, *pause, ticked);
            *pause = held;
            tick_run(run, ticked, counted);
        };
        for _ in 0..10 { frame(&mut run, 1.0, false, &mut pause); }
        // A 90 s pause: counted in the run's total, not in the step's age.
        for _ in 0..90 { frame(&mut run, 1.0, true, &mut pause); }
        frame(&mut run, 1.0, false, &mut pause);
        assert!((run.elapsed_s - 101.0).abs() < 1e-9, "the total keeps the short pause: {}", run.elapsed_s);
        assert!((run.elapsed_s - run.step_started_s - 11.0).abs() < 1e-9, "the step aged only by play: {}", run.elapsed_s - run.step_started_s);
        assert_eq!(run.step, 0, "the 30 s escape has not run out after 11 s of play");
        let fx = run.signal(&Signal::Elapsed { seconds: 11 });
        assert!(fx.is_empty() && run.step == 0);
        for _ in 0..20 { frame(&mut run, 1.0, false, &mut pause); }
        let age = (run.elapsed_s - run.step_started_s) as u32;
        run.signal(&Signal::Elapsed { seconds: age });
        assert_eq!(run.step, 1, "after 31 s of play the escape ran out");
    }

    fn starts(v: &[Signal]) -> usize { v.iter().filter(|s| **s == Signal::SplashStarted).count() }
    fn ends(v: &[Signal]) -> usize { v.iter().filter(|s| **s == Signal::SplashEnded).count() }

    #[test]
    fn each_basin_starts_and_ends_once_with_or_without_the_facade_signals() {
        // Facade signal first, basin loads next frame: one start.
        let (a, w) = splash_filter(vec![Signal::SplashStarted], SplashWatch::default(), false, false, 0.016);
        let (b, w) = splash_filter(vec![], w, true, false, 0.016);
        assert_eq!(starts(&a) + starts(&b), 1);
        // The facade's end and the session's end in the same frame: one end.
        let (c, w) = splash_filter(vec![Signal::SplashEnded], w, false, false, 0.016);
        assert_eq!(ends(&c), 1);
        // A second basin with no facade start, and a late facade start while it runs: one start, one end.
        let (d, w) = splash_filter(vec![], w, true, false, 0.016);
        let (e, w) = splash_filter(vec![Signal::SplashStarted], w, true, false, 0.016);
        let (f, w) = splash_filter(vec![], w, false, false, 0.016);
        assert_eq!((starts(&d) + starts(&e), ends(&f)), (1, 1));
        // A facade start whose basin never loads does not swallow the next basin's start.
        let (g, mut w) = splash_filter(vec![Signal::SplashStarted], w, false, false, 0.016);
        assert_eq!(starts(&g), 1);
        for _ in 0..400 { w = splash_filter(vec![], w, false, false, 0.016).1; }
        let (h, _) = splash_filter(vec![], w, true, false, 0.016);
        assert_eq!(starts(&h), 1);
    }

    #[test]
    fn helm_input_is_reported_once_per_basin_and_other_signals_pass_in_order() {
        let queued = vec![Signal::Gauge { gauge: "float".into(), pass: true }, Signal::NotesOpen { count: 0 }];
        let (a, w) = splash_filter(queued.clone(), SplashWatch::default(), true, true, 0.016);
        assert_eq!(&a[..2], &queued[..]);
        assert_eq!(a.iter().filter(|s| **s == Signal::HelmInput).count(), 1);
        let (b, w) = splash_filter(vec![], w, true, true, 0.016);
        assert!(b.is_empty());
        let (_, w) = splash_filter(vec![], w, false, false, 0.016);
        let (c, _) = splash_filter(vec![], w, true, true, 0.016);
        assert_eq!(c.iter().filter(|s| **s == Signal::HelmInput).count(), 1);
    }

    #[test]
    fn the_engaging_report_is_taken_again_in_each_new_basin() {
        // Live basin: the driver marks the fire report taken; the basin ends; the next one starts fresh.
        let (_, mut w) = splash_filter(vec![], SplashWatch::default(), true, false, 0.016);
        w.fire_reported = true;
        let (_, w) = splash_filter(vec![], w, true, false, 0.016);
        assert!(w.fire_reported, "still the same basin");
        let (_, w) = splash_filter(vec![], w, false, false, 0.016);
        assert!(!w.fire_reported, "the basin ended");
        let (_, mut w) = splash_filter(vec![], w, true, false, 0.016);
        w.fire_reported = true;
        let (_, w) = splash_filter(vec![Signal::SplashEnded], w, true, false, 0.016);
        assert!(!w.fire_reported, "the facade's end counts too");
    }

    #[test]
    fn shipped_scripts_open_on_their_hull_and_paths_take_the_stock_form() {
        let scripts = sim_systems::tutorial::scripts();
        for level in 0..=MAX_LEVEL as u8 { assert!(sim_systems::tutorial::script_for(level).is_some(), "level {level}"); }
        for s in &scripts {
            let first = &s.steps[0];
            assert!(waits_on_continue(&first.complete) && demo_of(first).is_none());
            assert!(first.on_enter.iter().any(|e| matches!(e, Effect::LoadBlueprint(p) if *p == s.blueprint)));
            // Every demo step can still end without its animation.
            for step in s.steps.iter().filter(|st| demo_of(st).is_some()) { assert!(matches!(step.complete, Trigger::Any(_))); }
        }
        assert_eq!(stock_path("tutorial/fac22_tutorial.bp.ron"), "stock:tutorial/fac22_tutorial.bp.ron");
        assert_eq!(stock_path("stock:starters/fac22.bp.ron"), "stock:starters/fac22.bp.ron");
        assert_eq!(stock_path("user://blueprints/a.bp.ron"), "user://blueprints/a.bp.ron");
    }

    #[test]
    fn the_full_tutorial_runs_to_its_reward_on_the_signals_this_driver_feeds() {
        let (mut run, _) = TutorialRun::start(sim_systems::tutorial::script_for(0).unwrap());
        let mut w = SplashWatch::default();
        let feed = |run: &mut TutorialRun, queued: Vec<Signal>, live: bool, w: &mut SplashWatch| {
            run.tick(1.0);
            let (sigs, nw) = splash_filter(queued, *w, live, live, 1.0);
            *w = nw;
            let mut effects = Vec::new();
            for s in &sigs { effects.extend(run.signal(s)); }
            effects
        };
        feed(&mut run, vec![Signal::Continue], false, &mut w);
        feed(&mut run, vec![Signal::Gauge { gauge: "float".into(), pass: true }], false, &mut w);
        let kits = ["kit.gun", "kit.radar_mast", "kit.engine", "kit.fuel_tank"].map(|k| Signal::KitPlaced { kit: content::ids::KitId::new(k) });
        feed(&mut run, kits.to_vec(), false, &mut w);
        feed(&mut run, vec![Signal::NoteFixApplied { note: "arc_blocked".into() }], false, &mut w);
        assert_eq!(run.current().map(|s| s.id.as_str()), Some("launch"));
        // The splash facade says nothing; the session's basin is enough.
        feed(&mut run, vec![], true, &mut w);
        feed(&mut run, vec![Signal::PlayerGunsFired], true, &mut w);
        assert_eq!(run.current().map(|s| s.id.as_str()), Some("drones"), "the basin stays open until a drone is down");
        feed(&mut run, vec![Signal::EnemyDestroyed { class: None }], true, &mut w);
        assert_eq!(run.current().map(|s| s.id.as_str()), Some("patrol_launch"));
        assert!(run.log.first_fight_s.is_some());
        // Still in the drone basin: a repeated start does not count until it ends and the next one loads.
        feed(&mut run, vec![Signal::SplashStarted], true, &mut w);
        assert_eq!(run.current().map(|s| s.id.as_str()), Some("patrol_launch"));
        feed(&mut run, vec![Signal::SplashEnded], false, &mut w);
        feed(&mut run, vec![Signal::SplashStarted], false, &mut w);
        feed(&mut run, vec![], true, &mut w);
        assert_eq!(run.current().map(|s| s.id.as_str()), Some("patrol_boat"));
        feed(&mut run, vec![Signal::EnemyDestroyed { class: None }], true, &mut w);
        feed(&mut run, vec![Signal::SliderMoved { key: "gunnery.aggression".into() }], true, &mut w);
        feed(&mut run, vec![], false, &mut w);
        feed(&mut run, vec![], true, &mut w);
        assert_eq!(run.current().map(|s| s.id.as_str()), Some("done"));
        let effects = feed(&mut run, vec![Signal::Continue], true, &mut w);
        assert!(run.finished);
        assert!(effects.iter().any(|e| matches!(e, Effect::Reward(r) if r == "starters/fac22.bp.ron")));
        assert!(run.log.first_kill_s.is_some() && run.log.timed_out.is_empty());
    }
}
