//! Part of unit `gd-render-ocean`. The replica of the shaders' `TIME` (DESIGN.md L1891) and the start-up
//! hook that registers the global shader parameters.
//!
//! Godot adds each drawn frame's time-scaled step to `TIME` and wraps it at
//! `rendering/limits/time/time_rollover_secs` (2048 s here). Nothing in the engine reports the value, so the
//! client keeps its own: [`BpShaderClock`], a node that processes always and before every other node, adds
//! the already time-scaled `delta` it is given and commits it only when `Engine.get_frames_drawn()` has
//! advanced, because a frame that is not drawn (minimised window, headless) never reaches `TIME`. The wave
//! uniforms are packed for the rendered sim time and `bp_waves_t_ref` is set to this replica's value for the
//! frame, so the shader's `TIME - t_ref` is zero at the draw and the drawn sea is the simulation's.
//!
//! # The step the engine draws with is not always the step it processes with
//! `Main::iteration` hands the renderer the frame's whole step, but takes
//! `(physics steps owed - max_physics_steps_per_frame) x physics step` off the `delta` that `_process`
//! receives. With the engine default of 8 steps, every frame longer than 133 ms (a load, a pipeline
//! compile) leaves `TIME` ahead of a replica built from `delta`, for good: measured in the engine, 0.25 s
//! after start-up alone. So [`install`] raises `Engine.max_physics_steps_per_frame` to
//! [`MAX_PHYSICS_STEPS`] (two minutes; this project's physics servers are the dummy ones, so a step costs
//! microseconds), after which `delta` is the drawn step and the replica reads zero against `TIME` across
//! hitches and time-scale changes. A stall longer than that (the machine slept) is still clamped; the
//! clamped amount is a whole number of physics steps, which [`ShaderClock::tick`] recovers from the wall
//! clock.
//!
//! The replica is only right if it has counted every drawn frame since the engine started, so the node is
//! not part of any scene: [`install`] adds it to the tree root when the main loop starts
//! (`ExtensionLibrary::on_stage_init(InitStage::MainLoop)`), and [`ensure_installed`] retries from the
//! per-frame hook if the root did not exist yet. A clock that started late says so through
//! [`calibrated`].
//!
//! # A paused tree
//! Pausing the scene tree stops the node that owns the ocean unit, but the renderer keeps drawing and `TIME`
//! keeps running, so a sea left with its last `bp_waves_t_ref` would roll on under hulls that stand still
//! and jump back on resume. While the tree is paused and a sea is being drawn ([`sea_drawn`]) this node
//! keeps `bp_waves_t_ref` at the replica's value itself, which holds the surface at the paused instant.
use godot::classes::{Engine, INode, Node, SceneTree};
use godot::prelude::*;
use std::cell::{Cell, RefCell};

/// The engine's default when the project does not set the rollover; the wave include wraps at
/// `sim_core::waves::TIME_ROLLOVER_S`, and `project.godot` sets the same number.
const DEFAULT_ROLLOVER_S: f64 = sim_core::waves::TIME_ROLLOVER_S;
/// Earlier than every other node (the session runs at -100).
const CLOCK_PRIORITY: i32 = -10_000;
const NODE_NAME: &str = "BpShaderClock";
/// `Engine.max_physics_steps_per_frame` while the game runs: no frame shorter than two minutes has its
/// `delta` clamped (see the module docs).
pub(crate) const MAX_PHYSICS_STEPS: i32 = 7_200;

/// What the engine's physics-step clamp looks like to one frame, for recovering a clamped step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Stall {
    /// Wall-clock seconds since the previous tick.
    pub wall_s: f64,
    /// `1 / physics_ticks_per_second`.
    pub physics_step_s: f64,
    pub max_steps: i32,
    pub time_scale: f64,
}

/// The arithmetic of the replica, with no Godot in it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ShaderClock {
    /// `TIME` after the last frame known to have been drawn.
    committed: f64,
    /// The step of the frame in progress: part of `TIME` when this frame is drawn.
    pending: f64,
    last_drawn: Option<i64>,
    rollover: f64,
    /// Frames whose clamped step was made whole from the wall clock.
    pub recovered: u32,
}

impl ShaderClock {
    pub fn new(rollover_s: f64) -> Self {
        let rollover = if rollover_s.is_finite() && rollover_s >= 1.0 { rollover_s } else { DEFAULT_ROLLOVER_S };
        Self { committed: 0.0, pending: 0.0, last_drawn: None, rollover, recovered: 0 }
    }
    /// Start of a frame: `delta` is the frame's time-scaled step and `frames_drawn` the engine's counter,
    /// which moves when the previous frame was drawn. `stall` lets a `delta` the engine clamped be made
    /// whole: a clamped frame is one whose unscaled `delta` is at least `max_steps` physics steps, and what
    /// was taken off it is the whole number of physics steps nearest the wall clock's surplus.
    pub fn tick(&mut self, delta: f64, frames_drawn: i64, stall: Option<Stall>) {
        if self.last_drawn.is_some_and(|last| last != frames_drawn) {
            // The engine wraps after every addition; so does the replica, operation for operation.
            self.committed = (self.committed + self.pending) % self.rollover;
        }
        self.last_drawn = Some(frames_drawn);
        let mut step = if delta.is_finite() && delta > 0.0 { delta } else { 0.0 };
        if let Some(st) = stall.filter(|st| st.time_scale.is_finite() && st.time_scale > 0.0 && st.physics_step_s.is_finite() && st.physics_step_s > 0.0 && st.wall_s.is_finite() && st.max_steps > 0) {
            let unscaled = step / st.time_scale;
            if unscaled >= st.max_steps as f64 * st.physics_step_s {
                let lost = ((st.wall_s - unscaled) / st.physics_step_s).round();
                if lost >= 1.0 {
                    // Bounded so a nonsense wall clock cannot spin the modulo below on a huge number.
                    step += lost.min(1.0e9) * st.physics_step_s * st.time_scale;
                    self.recovered += 1;
                }
            }
        }
        self.pending = step % self.rollover;
    }
    /// `TIME` as the shaders will read it when this frame is drawn.
    pub fn now(&self) -> f64 { (self.committed + self.pending) % self.rollover }
}

thread_local! {
    static CLOCK: RefCell<Option<ShaderClock>> = const { RefCell::new(None) };
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
    /// False when the node first ticked after the engine had already drawn frames with a non-zero step.
    static CALIBRATED: Cell<bool> = const { Cell::new(true) };
    /// Ocean units with their clipmap in a scenario.
    static SEAS: Cell<u32> = const { Cell::new(0) };
}

/// An ocean unit put its sea in a scenario (`true`) or took it out (`false`). While any is drawn, the clock
/// node holds the waves still across a paused tree (module docs).
pub(crate) fn sea_drawn(on: bool) { SEAS.with(|s| s.set(if on { s.get().saturating_add(1) } else { s.get().saturating_sub(1) })); }

/// The shaders' `TIME` for the frame being built, or `None` when no clock node has ticked (tests, the
/// editor). The ocean then keeps `t_ref` on its own count.
pub(crate) fn shader_time() -> Option<f64> { CLOCK.with(|c| c.borrow().as_ref().map(|c| c.now())) }

/// False when the clock was installed after frames had been drawn, so its value may be offset from `TIME`.
pub(crate) fn calibrated() -> bool { CALIBRATED.with(|c| c.get()) }

fn rollover_setting() -> f64 {
    let v = godot::classes::ProjectSettings::singleton().get_setting("rendering/limits/time/time_rollover_secs");
    crate::convert::variant_f64(&v).unwrap_or(DEFAULT_ROLLOVER_S)
}

/// The always-running node that keeps the `TIME` replica. Added to the tree root by [`install`]; never
/// placed in a scene.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct BpShaderClock {
    last_tick: Option<std::time::Instant>,
    base: Base<Node>,
}

#[godot_api]
impl INode for BpShaderClock {
    fn init(base: Base<Node>) -> Self { Self { last_tick: None, base } }
    fn ready(&mut self) {
        let mut base = self.base_mut();
        base.set_process_mode(godot::classes::node::ProcessMode::ALWAYS);
        base.set_process_priority(CLOCK_PRIORITY);
    }
    fn process(&mut self, delta: f64) {
        let engine = Engine::singleton();
        let drawn = engine.get_frames_drawn() as i64;
        let now = std::time::Instant::now();
        let stall = self.last_tick.map(|last| Stall {
            wall_s: now.duration_since(last).as_secs_f64(),
            physics_step_s: 1.0 / engine.get_physics_ticks_per_second().max(1) as f64,
            max_steps: engine.get_max_physics_steps_per_frame(),
            time_scale: engine.get_time_scale(),
        });
        self.last_tick = Some(now);
        CLOCK.with(|c| {
            let mut slot = c.borrow_mut();
            if slot.is_none() {
                // Frames drawn before the first process pass carried a zero step (the boot splash), so a
                // clock that starts on the first pass starts level with `TIME`. One that starts later has
                // missed steps it cannot recover; that is reported, not guessed at.
                if engine.get_process_frames() > 0 {
                    CALIBRATED.with(|k| k.set(false));
                    godot_warn!("shader clock: started after the first frame; the sea may be offset in time from the simulation");
                }
                *slot = Some(ShaderClock::new(rollover_setting()));
            }
            if let Some(clock) = slot.as_mut() { clock.tick(delta, drawn, stall); }
        });
        // The ocean unit sets the reference time on every frame it runs; under a paused tree it does not run.
        if SEAS.with(|s| s.get()) > 0 && engine.get_main_loop().and_then(|l| l.try_cast::<SceneTree>().ok()).is_some_and(|tree| tree.is_paused()) {
            if let Some(now) = shader_time() { godot::classes::RenderingServer::singleton().global_shader_parameter_set(sim_core::waves::WAVE_T_REF_UNIFORM, &now.to_variant()); }
        }
    }
    fn exit_tree(&mut self) {
        CLOCK.with(|c| *c.borrow_mut() = None);
        INSTALLED.with(|i| i.set(false));
    }
}

#[godot_api]
impl BpShaderClock {
    /// The replica's value for the frame being built, seconds in `[0, rollover)`. For tests and the
    /// performance overlay.
    #[func]
    fn shader_time(&self) -> f64 { shader_time().unwrap_or(0.0) }
}

/// Add the clock node to the tree root and register the global shader parameters. Called when the main
/// loop starts; does nothing in the editor (which also runs `--import`) and nothing twice.
pub(crate) fn install() {
    if INSTALLED.with(|i| i.get()) { return; }
    let mut engine = Engine::singleton();
    if engine.is_editor_hint() { INSTALLED.with(|i| i.set(true)); return; }
    let Some(tree) = engine.get_main_loop().and_then(|l| l.try_cast::<SceneTree>().ok()) else { return };
    let mut root = tree.get_root();
    INSTALLED.with(|i| i.set(true));
    super::gpu::ensure_globals();
    // `delta` must be the step the renderer draws with (module docs).
    if engine.get_max_physics_steps_per_frame() < MAX_PHYSICS_STEPS { engine.set_max_physics_steps_per_frame(MAX_PHYSICS_STEPS); }
    let mut node = BpShaderClock::new_alloc();
    node.set_name(NODE_NAME);
    // Not deferred: a deferred add would land after the first frame's process pass and miss its step.
    root.add_child(&node);
}

/// The per-frame retry of [`install`], for an engine that had no tree root when the main loop started.
pub(crate) fn ensure_installed() { if !INSTALLED.with(|i| i.get()) { install(); } }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_replica_counts_only_drawn_frames() {
        let mut c = ShaderClock::new(2048.0);
        // Frame 1: nothing drawn yet, TIME at its draw is the first step.
        c.tick(0.016, 7, None);
        assert!((c.now() - 0.016).abs() < 1e-12);
        // Frame 2: frame 1 was drawn.
        c.tick(0.020, 8, None);
        assert!((c.now() - 0.036).abs() < 1e-12);
        // Frames 3 and 4: the window was minimised, frames 2 and 3 were never drawn.
        c.tick(0.5, 9, None);
        c.tick(0.5, 9, None);
        c.tick(0.5, 9, None);
        assert!((c.now() - 0.536).abs() < 1e-12, "{}", c.now());
        // Drawing resumes: the frame in progress when it did is the one that counts.
        c.tick(0.016, 10, None);
        assert!((c.now() - 0.552).abs() < 1e-12, "{}", c.now());
    }

    #[test]
    fn the_replica_wraps_like_the_engine_and_survives_bad_steps() {
        let mut c = ShaderClock::new(2048.0);
        let mut engine = 0.0f64;
        let mut drawn = 0i64;
        for i in 0..200_000 {
            let step = 0.011 + 0.000_1 * (i % 7) as f64;
            c.tick(step, drawn, None);
            // The engine: add, then fmod.
            engine = (engine + step) % 2048.0;
            assert_eq!(c.now(), engine);
            drawn += 1;
        }
        assert!(engine < 2048.0 && c.now() < 2048.0);
        c.tick(f64::NAN, drawn, None);
        c.tick(-1.0, drawn + 1, None);
        assert!(c.now().is_finite());
        assert_eq!(ShaderClock::new(f64::NAN).rollover, DEFAULT_ROLLOVER_S);
    }

    #[test]
    fn a_step_the_engine_clamped_is_made_whole_from_the_wall_clock() {
        let ps = 1.0 / 60.0;
        let stall = |wall_s: f64, time_scale: f64| Some(Stall { wall_s, physics_step_s: ps, max_steps: 8, time_scale });
        // An ordinary frame is never touched, however the wall clock jitters.
        let mut c = ShaderClock::new(2048.0);
        c.tick(0.016, 1, stall(0.030, 1.0));
        assert!((c.now() - 0.016).abs() < 1e-12 && c.recovered == 0);
        // A 0.7 s frame arrives as 8 steps and a remainder: 34 steps were taken off.
        let truth = 0.7031;
        let clamped = truth - 34.0 * ps;
        assert!(clamped >= 8.0 * ps);
        c.tick(clamped, 2, stall(0.7049, 1.0));
        assert!((c.now() - (0.016 + truth)).abs() < 1e-9, "{}", c.now());
        assert_eq!(c.recovered, 1);
        // The same at triple speed: delta and the lost steps are both scaled.
        let mut c = ShaderClock::new(2048.0);
        c.tick(clamped * 3.0, 1, stall(0.7020, 3.0));
        assert!((c.now() - truth * 3.0).abs() < 1e-9);
        // A stall of hours wraps; a broken wall clock or a paused scale changes nothing.
        let mut c = ShaderClock::new(2048.0);
        c.tick(8.0 * ps, 1, stall(7_200.0, 1.0));
        assert!(c.now() >= 0.0 && c.now() < 2048.0);
        let mut c = ShaderClock::new(2048.0);
        c.tick(8.0 * ps, 1, stall(f64::NAN, 1.0));
        c.tick(0.0, 2, stall(5.0, 0.0));
        assert!((c.now() - 8.0 * ps).abs() < 1e-12 && c.recovered == 0);
    }
}
