//! Unit `gd-benchmark`. `BpBenchmark` (autoload `Bench`): the benchmark scene driver.
//! DESIGN.md L1392-1401 (stages: 3 minute reference battle at the detected tier, 2 minute co-op reference
//! battle hosted locally, 2 minute minimum swarm, editor stage, sim-calib, optional 20 minute soak; JSON
//! report), L1322 (report fields; never sent automatically; pasted into a GitHub issue form; no user names
//! or paths), L2045 (after stage 1: replays at Low and Medium, driver or upscaler A/B, G1 quiz), L1320 and
//! L1475 (2 ms adapter and 8 ms tick figures), L1744 (the report logs DPI awareness on Windows).
//! Main menu > Benchmark and the `--benchmark` command-line flag both start it. Each battle stage is an
//! ordinary session started from `sim_systems::editor::trials::reference_config`.
//!
//! # How a run goes
//! Each battle stage leaves a `PendingSortie` (`Bp.KIND_BENCHMARK`, a new sortie from the stage's reference
//! scene, seed 1) in `Services::handoff`; `BpSession` runs it like any sortie. Timing starts with the first
//! sampled frame and runs for the stage's length of wall time; the stage then leaves the session and the
//! next one starts once the session is idle. The editor stage and sim-calib run on their own thread
//! ([`measure`]) with no session live. A stage that fails to load in 60 s, or whose session fails, is
//! reported as failed and the run carries on.
//!
//! # Decisions where the contract is silent
//! - Battle stages run on the wall clock (`SortieOptions::fixed_step` off): the spec gives their lengths in
//!   minutes of play, and a fixed step would tie the simulation rate to the frame rate being measured.
//! - Per L2045 the stage-1 battle is replayed at Low and at Medium right after stage 1 (each tier the run
//!   did not already use), as extra `BENCH_REFERENCE_BATTLE` entries whose `tier` key names the tier. The
//!   tier is forced through the `graphics.tier` setting and the player's value is restored when the run
//!   finishes, is cancelled, or the game quits. The relaunch under Vulkan or the MetalFX A/B and the G1 quiz
//!   need a process relaunch and a quiz screen; they belong to the benchmark scene and are not run here.
//! - Frame times come from `frame_sample`; a frame the scene script did not sample is timed from the frame
//!   delta, so the figures exist even without the scene. Adapter time is the world view's own measure
//!   (`BpWorldView` render stats), its p99 per stage. Tick p95 and p99 are percentiles of the host's 2 s
//!   window statistics over the stage. Missed vblanks are a share of frames.
//! - Memory and thermal state are sampled once a second; the thermal history keeps one sample per 10 s.
//!   Pipeline compilations count every compilation counter after each battle stage's first frame.
//! - The co-op reference battle runs on this machine with the local player in slot 0; the scene's three
//!   other human hulls are in the config with no connection, so officers sail them (as a dropped player's
//!   hull is sailed, L838).
//! - Options beyond the contract (development only, ignored in release builds): `duration_scale: float`
//!   shortens every stage and the headless replays, for end-to-end checks.
//! - Text: stage lines are `bench.stage.*` keys with the tier as a `bench.tier.*` argument
//!   (`game/i18n/bench.csv`).
use crate::enums::{BenchStage, GdEnum, SessionKind, SessionState};
use crate::seams::QualityTier;
use crate::services::services;
use godot::classes::{DisplayServer, FileAccess, Os, ProjectSettings, RenderingServer, Time};
use godot::classes::file_access::ModeFlags;
use godot::classes::rendering_server::RenderingInfo;
use godot::prelude::*;
use sim_systems::editor::trials::ReferenceScene;
use content::ids::TextKey;
use sim_systems::protocol::{Msg, MsgArg};

mod measure;
use measure::{percentile, Job, JobFigures, Report, StageResult};

/// Benchmark driver. Autoload `Bench`.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct BpBenchmark {
    pub(crate) stage: BenchStage,
    pub(crate) stage_elapsed_s: f64,
    pub(crate) frame_ms: Vec<f32>,
    pub(crate) gpu_ms: Vec<f32>,
    /// The run under way.
    pub(crate) run: Option<Box<Run>>,
    /// The last finished run's report.
    pub(crate) last: Option<Box<Report>>,
    base: Base<Node>,
}

/// Lengths in seconds of wall time (L1393-1399).
const REFERENCE_S: f64 = 180.0;
const COOP_S: f64 = 120.0;
const SWARM_S: f64 = 120.0;
const SOAK_S: f64 = 1_200.0;
/// The soak's pass window: its last 5 minutes.
const SOAK_WINDOW_S: f64 = 300.0;
/// A battle stage whose first frame has not come after this long has failed.
const LOAD_LIMIT_S: f64 = 60.0;
/// How long to wait for the session to go idle after leaving.
const LEAVE_LIMIT_S: f64 = 10.0;
const SCENE_SEED: u64 = 1;
const EMIT_PERIOD_S: f64 = 0.5;
const THERMAL_PERIOD_S: f64 = 10.0;
/// Frame samples outside this range (milliseconds) are not frames.
const MAX_FRAME_MS: f64 = 10_000.0;

/// One stage of the plan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PlanItem { pub(crate) stage: BenchStage, pub(crate) tier: QualityTier, pub(crate) seconds: f64 }

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Phase { Begin, Loading, Running, Leaving, Job }

/// A run in progress. Main thread only.
pub(crate) struct Run {
    pub(crate) plan: Vec<PlanItem>,
    pub(crate) index: usize,
    pub(crate) phase: Phase,
    pub(crate) phase_s: f64,
    pub(crate) results: Vec<StageResult>,
    /// The player's `graphics.tier` before the run.
    pub(crate) saved_tier: i64,
    pub(crate) detected: QualityTier,
    pub(crate) scale: f64,
    pub(crate) adapter_ms: Vec<f32>,
    pub(crate) tick_p95: Vec<f32>,
    pub(crate) tick_p99: Vec<f32>,
    pub(crate) missed: u32,
    /// Stage time of each frame sample (the soak's last 5 minutes).
    pub(crate) frame_at: Vec<f32>,
    pub(crate) sampled: bool,
    pub(crate) pipelines_at: Option<u64>,
    pub(crate) pipelines: u64,
    pub(crate) video_peak: u64,
    pub(crate) mem_peak: u64,
    pub(crate) swap_start: Option<u64>,
    pub(crate) swap_peak: u64,
    pub(crate) pressure_peak: u8,
    pub(crate) dxgi_peak: u64,
    pub(crate) thermal: Vec<i32>,
    pub(crate) poll_s: f64,
    pub(crate) thermal_s: f64,
    pub(crate) job: Option<Job>,
    pub(crate) figures: JobFigures,
    pub(crate) emit_s: f64,
    pub(crate) emitted_index: Option<usize>,
    pub(crate) view: Option<Gd<crate::world::BpWorldView>>,
    pub(crate) problems: Vec<String>,
    pub(crate) soak: Option<(f64, bool)>,
}

#[godot_api]
impl INode for BpBenchmark {
    fn init(base: Base<Node>) -> Self { Self { stage: BenchStage::Idle, stage_elapsed_s: 0.0, frame_ms: Vec::new(), gpu_ms: Vec::new(), run: None, last: None, base } }
    fn ready(&mut self) {
        self.base_mut().set_process_priority(-60);
        // A run the game did not live to finish (a crash, a killed process) left its forced tier in the saved
        // settings: give the player's own tier back.
        if let Some(tier) = take_restore_marker() { crate::settings::write("graphics.tier", crate::settings::SettingValue::Int(tier)); }
    }
    /// Unit `gd-benchmark`: advance the running stage, start the next one, gather tick and adapter figures.
    /// Inert while idle.
    fn process(&mut self, delta: f64) {
        if self.run.is_none() { return; }
        let dt = if delta.is_finite() && delta > 0.0 { delta.min(10.0) } else { 0.0 };
        self.sample_machine(dt);
        let finished = self.step(dt);
        if finished { self.finish(); return; }
        self.maybe_emit(dt);
    }
    /// A run cut short by quitting gives the player's tier setting back.
    fn exit_tree(&mut self) {
        if let Some(run) = self.run.as_mut() {
            if let Some(mut job) = run.job.take() { job.cancel(); job.join(); }
            crate::settings::write("graphics.tier", crate::settings::SettingValue::Int(run.saved_tier));
            let _ = take_restore_marker();
        }
    }
}

#[godot_api]
impl BpBenchmark {
    /// A stage began or progressed (`BenchStageDict`); at most 2 Hz.
    #[signal]
    fn stage_changed(stage: VarDictionary);
    /// All stages are done; `report` is a `BenchReportDict`.
    #[signal]
    fn finished(report: VarDictionary);

    /// Start a run. `options` keys (optional): `soak: bool` (add the 20 minute soak), `stages:
    /// PackedInt32Array` (`Bp.BENCH_*`; default all but soak), `tier: int` (`Bp.QUALITY_*`; default the
    /// detected tier). Returns false while a session is live.
    #[func]
    fn start(&mut self, options: VarDictionary) -> bool {
        if self.run.is_some() { return false; }
        let services = services();
        let live = services.connection.borrow().is_some() || services.handoff.borrow().sortie.is_some() || !matches!(services.feed.borrow().state, SessionState::Idle);
        if live { return false; }
        let (saved_tier, detected) = {
            let s = services.settings.borrow();
            let saved = s.int("graphics.tier");
            let detected = match s.int("graphics.detected_tier") { t @ 0..=3 => QualityTier::from_gd(t), _ => None }.unwrap_or(services.quality.borrow().tier);
            (saved, detected)
        };
        drop(services);
        write_restore_marker(saved_tier);
        let tier = crate::convert::get_i64(&options, "tier").and_then(QualityTier::from_gd).unwrap_or(detected);
        let soak = crate::convert::get_bool(&options, "soak").unwrap_or(false);
        let stages: Option<Vec<BenchStage>> = options.get("stages").and_then(|v| v.try_to::<PackedInt32Array>().ok().or_else(|| v.try_to::<VarArray>().ok().map(|a| a.iter_shared().filter_map(|x| x.try_to::<i64>().ok()).map(|x| x as i32).collect())))
            .map(|a| a.as_slice().iter().filter_map(|s| BenchStage::from_gd(*s as i64)).collect());
        let scale = if cfg!(debug_assertions) { crate::convert::get_f64(&options, "duration_scale").filter(|s| *s > 0.0).map_or(1.0, |s| s.clamp(0.001, 1.0)) } else { 1.0 };
        let plan = plan(stages.as_deref(), soak, tier, scale);
        if plan.is_empty() { return false; }
        self.frame_ms.clear();
        self.gpu_ms.clear();
        self.stage_elapsed_s = 0.0;
        self.stage = plan[0].stage;
        self.run = Some(Box::new(Run {
            plan, index: 0, phase: Phase::Begin, phase_s: 0.0, results: Vec::new(), saved_tier, detected: tier, scale,
            adapter_ms: Vec::new(), tick_p95: Vec::new(), tick_p99: Vec::new(), missed: 0, frame_at: Vec::new(), sampled: false,
            pipelines_at: None, pipelines: 0, video_peak: 0, mem_peak: 0, swap_start: None, swap_peak: 0, pressure_peak: 0, dxgi_peak: 0,
            thermal: Vec::new(), poll_s: 0.0, thermal_s: 0.0, job: None, figures: JobFigures::default(), emit_s: 0.0, emitted_index: None, view: None,
            problems: Vec::new(), soak: None,
        }));
        let d = self.stage_dict();
        self.base_mut().emit_signal("stage_changed", &[d.to_variant()]);
        true
    }
    #[func]
    fn cancel(&mut self) {
        let Some(mut run) = self.run.take() else { return };
        if let Some(job) = run.job.as_mut() { job.cancel(); }
        crate::settings::write("graphics.tier", crate::settings::SettingValue::Int(run.saved_tier));
        let _ = take_restore_marker();
        // A battle handed over and not yet taken must not start after the run is gone.
        let queued = { let sv = services(); let mut h = sv.handoff.borrow_mut(); if h.sortie.as_ref().is_some_and(|p| p.kind == SessionKind::Benchmark) { h.sortie.take().is_some() } else { false } };
        if matches!(run.phase, Phase::Loading | Phase::Running) && !queued { self.leave_session(); }
        self.stage = BenchStage::Idle;
        self.stage_elapsed_s = 0.0;
        self.frame_ms.clear();
        self.gpu_ms.clear();
        let d = self.stage_dict();
        self.base_mut().emit_signal("stage_changed", &[d.to_variant()]);
    }
    /// Returns `BenchStageDict`.
    #[func]
    fn stage(&self) -> VarDictionary { self.stage_dict() }
    /// Feed one frame's timings from the scene script: CPU frame time and, on D3D12 and Vulkan, the
    /// viewport's measured GPU time (pass -1.0 on Metal, where frame time and missed vblanks stand in, L2022).
    #[func]
    fn frame_sample(&mut self, frame_ms: f64, gpu_ms: f64, missed_vblank: bool) {
        let elapsed = self.stage_elapsed_s;
        let Some(run) = self.run.as_mut() else { return };
        if run.phase != Phase::Running { return; }
        if !(frame_ms.is_finite() && frame_ms > 0.0 && frame_ms < MAX_FRAME_MS) { return; }
        if run.sampled { return; }
        run.sampled = true;
        self.frame_ms.push(frame_ms as f32);
        run.frame_at.push(elapsed as f32);
        if (0.0..MAX_FRAME_MS).contains(&gpu_ms) { self.gpu_ms.push(gpu_ms as f32); }
        if missed_vblank { run.missed += 1; }
    }
    /// The last finished run's `BenchReportDict`, or an empty Dictionary.
    #[func]
    fn report(&self) -> VarDictionary { self.last.as_ref().map_or_else(VarDictionary::new, |r| report_dict(r)) }
    /// Write the report JSON under `user://bench/` and return its path.
    #[func]
    fn save_report(&self) -> GString {
        let Some(r) = &self.last else { return GString::new() };
        let dir = ProjectSettings::singleton().globalize_path("user://bench");
        if !godot::classes::DirAccess::dir_exists_absolute(&dir) { let _ = godot::classes::DirAccess::make_dir_recursive_absolute(&dir); }
        let stamp: String = Time::singleton().get_datetime_string_from_system().to_string().chars().map(|c| if c.is_ascii_digit() { c } else { '-' }).collect();
        let path = format!("user://bench/report-{stamp}.json");
        match FileAccess::open(path.as_str(), ModeFlags::WRITE) {
            Some(mut f) => { f.store_string(r.json().as_str()); f.close(); GString::from(path.as_str()) }
            None => { godot_warn!("benchmark: could not write {path}"); GString::new() }
        }
    }
    /// Address of the "Benchmark report" issue form to open in the browser (nothing is sent by the game).
    #[func]
    fn issue_form_url(&self) -> GString { GString::from(issue_url().as_str()) }
}

/// While a run forces graphics tiers, the player's own tier waits in this file, so a run cut short by a crash
/// is undone at the next start ([`BpBenchmark::ready`]).
const RESTORE_MARKER: &str = "user://bench/restore_tier.txt";

fn write_restore_marker(tier: i64) {
    let dir = ProjectSettings::singleton().globalize_path("user://bench");
    if !godot::classes::DirAccess::dir_exists_absolute(&dir) { let _ = godot::classes::DirAccess::make_dir_recursive_absolute(&dir); }
    if let Some(mut f) = FileAccess::open(RESTORE_MARKER, ModeFlags::WRITE) { f.store_string(tier.to_string().as_str()); f.close(); }
}

/// The tier a cut-short run left behind, removing the marker. `None` when there is none.
fn take_restore_marker() -> Option<i64> {
    if !FileAccess::file_exists(RESTORE_MARKER) { return None; }
    let text = FileAccess::get_file_as_string(RESTORE_MARKER).to_string();
    let _ = godot::classes::DirAccess::remove_absolute(&ProjectSettings::singleton().globalize_path(RESTORE_MARKER));
    text.trim().parse::<i64>().ok()
}

/// The issue form, on the repository the build names.
fn issue_url() -> String {
    let repo = env!("CARGO_PKG_REPOSITORY").trim_end_matches('/');
    format!("{repo}/issues/new?template=benchmark_report.yml")
}

/// The stages of a run, in order (L1393-1399, L2045).
pub(crate) fn plan(stages: Option<&[BenchStage]>, soak: bool, tier: QualityTier, scale: f64) -> Vec<PlanItem> {
    let default = [BenchStage::ReferenceBattle, BenchStage::CoopReference, BenchStage::MinimumSwarm, BenchStage::Editor, BenchStage::SimCalib];
    let mut wanted: Vec<BenchStage> = match stages { Some(s) => s.to_vec(), None => default.to_vec() };
    if soak && !wanted.contains(&BenchStage::Soak) { wanted.push(BenchStage::Soak); }
    let order = [BenchStage::ReferenceBattle, BenchStage::CoopReference, BenchStage::MinimumSwarm, BenchStage::Editor, BenchStage::SimCalib, BenchStage::Soak];
    let mut out = Vec::new();
    for stage in order {
        if !wanted.contains(&stage) { continue; }
        let seconds = match stage { BenchStage::ReferenceBattle => REFERENCE_S, BenchStage::CoopReference => COOP_S, BenchStage::MinimumSwarm => SWARM_S, BenchStage::Soak => SOAK_S, _ => 0.0 } * scale;
        out.push(PlanItem { stage, tier, seconds });
        if stage == BenchStage::ReferenceBattle {
            // The rendering stages after stage 1: the same battle at Low and at Medium.
            for t in [QualityTier::Low, QualityTier::Medium] { if t != tier && tier != QualityTier::Fallback { out.push(PlanItem { stage, tier: t, seconds }); } }
        }
    }
    out
}

fn is_battle(s: BenchStage) -> bool { matches!(s, BenchStage::ReferenceBattle | BenchStage::CoopReference | BenchStage::MinimumSwarm | BenchStage::Soak) }
fn scene_of(s: BenchStage) -> ReferenceScene { match s { BenchStage::CoopReference => ReferenceScene::CoopReference, BenchStage::MinimumSwarm => ReferenceScene::MinimumSwarm, _ => ReferenceScene::ReferenceBattle } }

impl BpBenchmark {
    /// One frame of the state machine. Returns true when the plan is done.
    fn step(&mut self, dt: f64) -> bool {
        let (state, kind) = { let sv = services(); let f = sv.feed.borrow(); (f.state, f.kind) };
        let me: Gd<Node> = self.base().clone();
        let Some(run) = self.run.as_mut() else { return true };
        run.phase_s += dt;
        let Some(item) = run.plan.get(run.index).copied() else { return true };
        match run.phase {
            Phase::Begin => {
                if !matches!(state, SessionState::Idle) {
                    // Whatever is live (an earlier stage, a menu) goes first.
                    run.phase = Phase::Leaving;
                    run.phase_s = 0.0;
                    self.leave_session();
                    return false;
                }
                self.stage = item.stage;
                self.stage_elapsed_s = 0.0;
                self.frame_ms.clear();
                self.gpu_ms.clear();
                let run = self.run.as_mut().expect("run");
                run.adapter_ms.clear();
                run.tick_p95.clear();
                run.tick_p99.clear();
                run.frame_at.clear();
                run.missed = 0;
                run.pipelines_at = None;
                run.phase_s = 0.0;
                if is_battle(item.stage) {
                    crate::settings::write("graphics.tier", crate::settings::SettingValue::Int(item.tier.to_gd()));
                    let campaign = services().campaign();
                    let config = sim_systems::editor::trials::reference_config(scene_of(item.stage), SCENE_SEED, &campaign);
                    services().handoff.borrow_mut().sortie = Some(crate::session::PendingSortie {
                        kind: SessionKind::Benchmark,
                        start: crate::session::host::HostStart::New { config: Box::new(config) },
                        options: crate::session::host::SortieOptions { checkpoints: false, fixed_step: false },
                    });
                    run.phase = Phase::Loading;
                } else {
                    let scale = run.scale;
                    let factory = services().sim_deps_factory();
                    let job = match item.stage {
                        BenchStage::Editor => {
                            let plan = measure::EditorPlan { edits_per_hull: ((measure::EDITS_PER_HULL as f64 * scale).ceil() as usize).max(2), splash_runs: ((measure::SPLASH_RUNS as f64 * scale).ceil() as usize).max(1), hulls: measure::STARTERS.len() };
                            Job::spawn("editor", move |shared| measure::editor_stage(plan, shared, &*factory))
                        }
                        _ => {
                            let ticks = ((measure::CALIB_TICKS as f64 * scale).ceil() as u64).max(10);
                            Job::spawn("calib", move |shared| measure::sim_calib(ticks, shared, factory()))
                        }
                    };
                    match job {
                        Some(j) => { run.job = Some(j); run.phase = Phase::Job; }
                        None => { run.problems.push(format!("{}: the stage thread did not start", measure::stage_name(item.stage))); run.index += 1; }
                    }
                }
            }
            Phase::Loading => {
                if state == SessionState::InSortie && kind == SessionKind::Benchmark {
                    run.phase = Phase::Running;
                    run.phase_s = 0.0;
                    run.pipelines_at = Some(pipeline_compilations());
                    run.view = find_world_view(&me);
                } else if state == SessionState::Failed || run.phase_s > LOAD_LIMIT_S {
                    run.problems.push(format!("{} ({}): the battle did not load", measure::stage_name(item.stage), measure::tier_name(item.tier)));
                    self.close_battle(item, true);
                }
            }
            Phase::Running => {
                if state != SessionState::InSortie {
                    // The session ended or failed under the stage: what was measured stands, marked failed.
                    let failed = state == SessionState::Failed || self.stage_elapsed_s < item.seconds * 0.5;
                    if failed { run.problems.push(format!("{}: the battle stopped after {:.0} s", measure::stage_name(item.stage), self.stage_elapsed_s)); }
                    self.close_battle(item, failed);
                    return false;
                }
                // Last frame's samples: the scene script samples after this node runs.
                if !run.sampled && self.stage_elapsed_s > 0.0 && dt > 0.0 {
                    self.frame_ms.push((dt * 1_000.0) as f32);
                    run.frame_at.push(self.stage_elapsed_s as f32);
                }
                run.sampled = false;
                self.stage_elapsed_s += dt;
                if let Some(v) = run.view.as_ref().filter(|v| v.is_instance_valid()) {
                    let us = v.bind().stats.adapter_us;
                    if us > 0 { run.adapter_ms.push(us as f32 / 1_000.0); }
                } else if run.phase_s.fract() < dt { run.view = find_world_view(&me); }
                for note in services().feed.borrow().host_notes.iter() {
                    if let crate::session::host::HostNote::Stats(s) = note {
                        if s.tick_p95_ms.is_finite() && s.tick_p95_ms > 0.0 { run.tick_p95.push(s.tick_p95_ms); }
                        if s.tick_p99_ms.is_finite() && s.tick_p99_ms > 0.0 { run.tick_p99.push(s.tick_p99_ms); }
                    }
                }
                if self.stage_elapsed_s >= item.seconds { self.close_battle(item, false); }
            }
            Phase::Leaving => {
                if state == SessionState::Idle || run.phase_s > LEAVE_LIMIT_S {
                    if state != SessionState::Idle { run.problems.push("the session did not leave in time".into()); }
                    run.phase = Phase::Begin;
                    run.phase_s = 0.0;
                }
            }
            Phase::Job => {
                let Some(job) = run.job.as_mut() else { run.phase = Phase::Begin; run.index += 1; return false };
                self.stage_elapsed_s += dt;
                if let Some(f) = job.poll() {
                    run.job = None;
                    run.figures.gauge_ms.extend(f.gauge_ms);
                    run.figures.splash_ms.extend(f.splash_ms);
                    if f.calib_us.is_some() { run.figures.calib_us = f.calib_us; }
                    run.problems.extend(f.problems);
                    run.index += 1;
                    run.phase = Phase::Begin;
                    run.phase_s = 0.0;
                }
            }
        }
        self.run.as_ref().is_none_or(|r| r.index >= r.plan.len() && r.phase == Phase::Begin)
    }

    /// Record the stage's figures, leave its session, move on.
    fn close_battle(&mut self, item: PlanItem, failed: bool) {
        let elapsed = self.stage_elapsed_s;
        let frames = std::mem::take(&mut self.frame_ms);
        let gpu = std::mem::take(&mut self.gpu_ms);
        let Some(run) = self.run.as_mut() else { return };
        if let Some(at) = run.pipelines_at.take() { run.pipelines += pipeline_compilations().saturating_sub(at); }
        let n = frames.len().max(1);
        let result = StageResult {
            stage: item.stage, tier: item.tier,
            frame_ms: [percentile(&frames, 0.5), percentile(&frames, 0.95), percentile(&frames, 0.99)],
            gpu_ms: [percentile(&gpu, 0.5), percentile(&gpu, 0.95), percentile(&gpu, 0.99)],
            tick_ms: [percentile(&run.tick_p95, 0.95), percentile(&run.tick_p99, 0.99)],
            adapter_ms_p99: percentile(&run.adapter_ms, 0.99),
            missed_vblanks: run.missed as f32 / n as f32,
            frames: frames.len().min(u32::MAX as usize) as u32,
            seconds: elapsed,
            failed,
        };
        if item.stage == BenchStage::Soak && !failed {
            let from = (item.seconds - SOAK_WINDOW_S * run.scale).max(0.0) as f32;
            let last: Vec<f32> = frames.iter().zip(&run.frame_at).filter(|(_, t)| **t >= from).map(|(f, _)| *f).collect();
            let p95 = percentile(&last, 0.95) as f64;
            run.soak = Some((p95, p95 > 0.0 && p95 <= measure::soak_budget_ms(item.tier)));
        }
        run.results.push(result);
        run.index += 1;
        run.phase = Phase::Leaving;
        run.phase_s = 0.0;
        run.view = None;
        self.leave_session();
    }

    /// Leave the live session at idle time (its signal handlers may call back into `Bench`).
    fn leave_session(&mut self) {
        if let Some(mut session) = self.base().get_node_or_null("/root/Session") {
            session.run_deferred_gd(|mut s: Gd<Node>| { if s.try_call("leave", &[false.to_variant()]).is_err() { godot_warn!("benchmark: the session did not leave"); } });
        } else {
            let conn = services().connection.borrow_mut().take();
            drop(conn);
        }
    }

    /// Memory, video memory and thermal state, once a second.
    fn sample_machine(&mut self, dt: f64) {
        let Some(run) = self.run.as_mut() else { return };
        run.poll_s -= dt;
        run.thermal_s -= dt;
        if run.poll_s > 0.0 { return; }
        run.poll_s = 1.0;
        let m = crate::platform::memory_stats();
        run.mem_peak = run.mem_peak.max(m.footprint_bytes);
        if run.swap_start.is_none() { run.swap_start = Some(m.swap_bytes); }
        run.swap_peak = run.swap_peak.max(m.swap_bytes);
        run.pressure_peak = run.pressure_peak.max(m.pressure_level);
        if let Some(b) = crate::platform::video_memory_in_use_bytes() { run.dxgi_peak = run.dxgi_peak.max(b); }
        run.video_peak = run.video_peak.max(RenderingServer::singleton().get_rendering_info(RenderingInfo::VIDEO_MEM_USED));
        if run.thermal_s <= 0.0 {
            run.thermal_s = THERMAL_PERIOD_S;
            run.thermal.push(crate::platform::thermal_state() as i32);
        }
    }

    /// `stage_changed` at most twice a second, and at once when the stage changes.
    fn maybe_emit(&mut self, dt: f64) {
        let Some(run) = self.run.as_mut() else { return };
        run.emit_s -= dt;
        let changed = run.emitted_index != Some(run.index);
        if !changed && run.emit_s > 0.0 { return; }
        run.emit_s = EMIT_PERIOD_S;
        run.emitted_index = Some(run.index);
        let d = self.stage_dict();
        self.base_mut().emit_signal("stage_changed", &[d.to_variant()]);
    }

    /// The run is done: the report, the tier back, the signals.
    fn finish(&mut self) {
        let Some(mut run) = self.run.take() else { return };
        crate::settings::write("graphics.tier", crate::settings::SettingValue::Int(run.saved_tier));
        let _ = take_restore_marker();
        let report = self.build_report(&mut run);
        self.stage = BenchStage::Done;
        self.stage_elapsed_s = 0.0;
        self.frame_ms.clear();
        self.gpu_ms.clear();
        let dict = report_dict(&report);
        self.last = Some(Box::new(report));
        let d = self.stage_dict();
        self.base_mut().emit_signal("stage_changed", &[d.to_variant()]);
        self.base_mut().emit_signal("finished", &[dict.to_variant()]);
    }

    fn build_report(&self, run: &mut Run) -> Report {
        let os = Os::singleton();
        let rs = RenderingServer::singleton();
        let user = os.get_environment("USER").to_string();
        let user = if user.is_empty() { os.get_environment("USERNAME").to_string() } else { user };
        let clean = |s: GString| measure::scrub(&s.to_string(), &user);
        let mem = os.get_memory_info();
        let ram = mem.get("physical").and_then(|v| v.try_to::<i64>().ok()).unwrap_or(0).max(0) as f64 / 1.0e9;
        let driver = os.get_video_adapter_driver_info();
        let driver_version = driver.as_slice().get(1).map(|s| clean(s.clone())).unwrap_or_default();
        let fallbacks = self.base().get_node_or_null("/root/App").and_then(|n| n.try_cast::<crate::app::BpApp>().ok())
            .and_then(|a| a.bind().startup.as_ref().map(|s| s.fallbacks.clone())).unwrap_or_default();
        let window = DisplayServer::singleton().window_get_size();
        let q = services().quality.borrow().clone();
        let gauge_p95 = percentile(&run.figures.gauge_ms, 0.95) as f64;
        let splash_p95 = percentile(&run.figures.splash_ms, 0.95) as f64;
        let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
        Report {
            build: format!("{} ({})", env!("CARGO_PKG_VERSION"), option_env!("BP_BUILD_ID").unwrap_or("local")),
            os: clean(GString::from(format!("{} {}", os.get_name(), os.get_version()).as_str())),
            cpu: clean(os.get_processor_name()),
            ram_gb: (ram * 10.0).round() / 10.0,
            adapter: clean(rs.get_video_adapter_name()),
            driver_version,
            rendering_method: clean(rs.get_current_rendering_method()),
            rendering_driver: clean(rs.get_current_rendering_driver_name()),
            fallbacks: fallbacks.iter().map(|f| measure::scrub(f, &user)).collect(),
            tier: run.detected,
            window: (window.x, window.y),
            scale_3d: (q.base_scale * q.dynamic_scale).clamp(0.0, 2.0),
            stages: std::mem::take(&mut run.results),
            pipeline_compilations: run.pipelines,
            video_mem_mb: mb(run.video_peak),
            peak_mem_mb: mb(run.mem_peak),
            swap_growth_mb: mb(run.swap_peak.saturating_sub(run.swap_start.unwrap_or(run.swap_peak))),
            pressure_peak: run.pressure_peak,
            dxgi_usage_mb: mb(run.dxgi_peak),
            sim_calib_us: run.figures.calib_us.unwrap_or(0.0),
            editor_gauge_ms_p95: gauge_p95,
            editor_splash_ms_p95: splash_p95,
            thermal: std::mem::take(&mut run.thermal),
            dpi_awareness: crate::platform::dpi_awareness().to_string(),
            soak: run.soak,
            problems: std::mem::take(&mut run.problems),
        }
    }

    /// `BenchStageDict`.
    fn stage_dict(&self) -> VarDictionary {
        let (progress, left, tier) = match self.run.as_ref() {
            None => (if self.stage == BenchStage::Done { 1.0 } else { 0.0 }, 0.0, None),
            Some(run) => match run.plan.get(run.index) {
                None => (1.0, 0.0, None),
                Some(item) if is_battle(item.stage) => {
                    let done = if run.phase == Phase::Running { self.stage_elapsed_s } else { 0.0 };
                    let p = if item.seconds > 0.0 { (done / item.seconds).clamp(0.0, 1.0) } else { 0.0 };
                    (p, (item.seconds - done).max(0.0), Some(item.tier))
                }
                Some(_) => {
                    let p = run.job.as_ref().map_or(0.0, |j| j.progress());
                    let left = if p > 0.01 { self.stage_elapsed_s * (1.0 - p) / p } else { 0.0 };
                    (p, left, None)
                }
            },
        };
        let mut msg = Msg::new(&format!("bench.stage.{}", measure::stage_name(self.stage)));
        if let Some(t) = tier { msg = msg.arg(MsgArg::Key(TextKey::new(format!("bench.tier.{}", measure::tier_name(t))))); }
        crate::convert::d! {
            "stage" => self.stage.to_gd(),
            "progress" => progress,
            "seconds_left" => left,
            "text" => crate::convert::msg_dict(&msg),
        }
    }
}

/// Every compilation counter the renderer keeps.
fn pipeline_compilations() -> u64 {
    let rs = RenderingServer::singleton();
    [RenderingInfo::PIPELINE_COMPILATIONS_CANVAS, RenderingInfo::PIPELINE_COMPILATIONS_MESH, RenderingInfo::PIPELINE_COMPILATIONS_SURFACE, RenderingInfo::PIPELINE_COMPILATIONS_DRAW, RenderingInfo::PIPELINE_COMPILATIONS_SPECIALIZATION]
        .into_iter().map(|i| rs.get_rendering_info(i)).sum()
}

/// The sortie's world view, wherever the benchmark scene put it.
fn find_world_view(base: &Node) -> Option<Gd<crate::world::BpWorldView>> {
    if !base.is_inside_tree() { return None; }
    let root: Gd<Node> = base.get_tree().get_root().upcast();
    root.find_children_ex("*").type_("BpWorldView").recursive(true).owned(false).done().iter_shared().find_map(|n| n.try_cast::<crate::world::BpWorldView>().ok())
}

/// `BenchReportDict`.
fn report_dict(r: &Report) -> VarDictionary {
    let stages: VarArray = r.stages.iter().map(|s| crate::convert::d! {
        "stage" => s.stage.to_gd(),
        "tier" => s.tier.to_gd(),
        "frame_ms" => Vector3::new(s.frame_ms[0], s.frame_ms[1], s.frame_ms[2]),
        "gpu_ms" => Vector3::new(s.gpu_ms[0], s.gpu_ms[1], s.gpu_ms[2]),
        "tick_ms" => Vector2::new(s.tick_ms[0], s.tick_ms[1]),
        "adapter_ms_p99" => s.adapter_ms_p99 as f64,
        "missed_vblanks" => s.missed_vblanks as f64,
        "frames" => s.frames as i64,
        "seconds" => s.seconds,
        "failed" => s.failed,
    }.to_variant()).collect();
    let fallbacks: PackedStringArray = r.fallbacks.iter().map(|s| GString::from(s.as_str())).collect();
    let thermal: PackedInt32Array = r.thermal.iter().copied().collect();
    let problems: PackedStringArray = r.problems.iter().map(|s| GString::from(s.as_str())).collect();
    let mut d = crate::convert::d! {
        "build" => GString::from(r.build.as_str()),
        "os" => GString::from(r.os.as_str()),
        "cpu" => GString::from(r.cpu.as_str()),
        "ram_gb" => r.ram_gb,
        "adapter" => GString::from(r.adapter.as_str()),
        "driver_version" => GString::from(r.driver_version.as_str()),
        "rendering_method" => GString::from(r.rendering_method.as_str()),
        "rendering_driver" => GString::from(r.rendering_driver.as_str()),
        "fallbacks" => fallbacks,
        "tier" => r.tier.to_gd(),
        "window" => Vector2i::new(r.window.0, r.window.1),
        "scale_3d" => r.scale_3d as f64,
        "stages" => stages,
        "pipeline_compilations" => r.pipeline_compilations.min(i64::MAX as u64) as i64,
        "video_mem_mb" => r.video_mem_mb,
        "peak_mem_mb" => r.peak_mem_mb,
        "sim_calib_us" => r.sim_calib_us,
        "editor_gauge_ms_p95" => r.editor_gauge_ms_p95,
        "editor_splash_ms_p95" => r.editor_splash_ms_p95,
        "thermal" => thermal,
        "dpi_awareness" => GString::from(r.dpi_awareness.as_str()),
        "problems" => problems,
        "json" => GString::from(r.json().as_str()),
    };
    match r.soak {
        Some((p95, pass)) => { d.set("soak", &crate::convert::d! { "frame_p95_last_5_min_ms" => p95, "pass" => pass }.to_variant()); }
        None => { d.set("soak", &Variant::nil()); }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_plan_follows_the_spec_and_adds_the_tier_reruns() {
        let p = plan(None, false, QualityTier::High, 1.0);
        let stages: Vec<(BenchStage, QualityTier)> = p.iter().map(|i| (i.stage, i.tier)).collect();
        assert_eq!(stages, vec![
            (BenchStage::ReferenceBattle, QualityTier::High), (BenchStage::ReferenceBattle, QualityTier::Low), (BenchStage::ReferenceBattle, QualityTier::Medium),
            (BenchStage::CoopReference, QualityTier::High), (BenchStage::MinimumSwarm, QualityTier::High), (BenchStage::Editor, QualityTier::High), (BenchStage::SimCalib, QualityTier::High),
        ]);
        assert_eq!((p[0].seconds, p[3].seconds, p[4].seconds), (180.0, 120.0, 120.0));
        // Low: only the Medium re-run; soak last at 20 minutes.
        let p = plan(None, true, QualityTier::Low, 1.0);
        assert_eq!(p.iter().filter(|i| i.stage == BenchStage::ReferenceBattle).count(), 2);
        assert_eq!(p.last().map(|i| (i.stage, i.seconds)), Some((BenchStage::Soak, 1_200.0)));
        // Chosen stages keep the spec's order; Idle and Done are not stages.
        let p = plan(Some(&[BenchStage::SimCalib, BenchStage::Done, BenchStage::Editor]), false, QualityTier::Medium, 0.5);
        assert_eq!(p.iter().map(|i| i.stage).collect::<Vec<_>>(), vec![BenchStage::Editor, BenchStage::SimCalib]);
        assert!(plan(Some(&[]), false, QualityTier::Medium, 1.0).is_empty());
    }

    #[test]
    fn the_issue_form_is_on_the_project_repository() {
        let u = issue_url();
        assert!(u.starts_with("https://github.com/") && u.ends_with("/issues/new?template=benchmark_report.yml"), "{u}");
    }

    #[test]
    fn every_stage_and_tier_line_has_english_text() {
        let csv = crate::text::tests::CsvLookup::load();
        for s in BenchStage::VARIANTS { let k = format!("bench.stage.{}", measure::stage_name(*s)); assert!(csv.0.contains_key(&k), "{k}"); }
        for t in QualityTier::VARIANTS { let k = format!("bench.tier.{}", measure::tier_name(*t)); assert!(csv.0.contains_key(&k), "{k}"); }
    }
}
