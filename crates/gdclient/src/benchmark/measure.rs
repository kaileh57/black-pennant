//! Part of unit `gd-benchmark`. The pure-Rust parts of the benchmark: percentiles, the two headless stages
//! (editor timing and sim-calib) that run on their own thread, and the report with its JSON form.
//!
//! # The editor stage (L176-181, L218)
//! The spec's gauge replay is "200 edits on each of the 6 starter hulls and on a 90 m stress hull"; no
//! shared edit replay or stress hull exists in the build, so this stage defines its own deterministic replay
//! on the six stock starters: edit k of a hull removes its part `k / 2 mod n` (even k) or puts it back (odd
//! k), so every edit changes the design and the replay ends where it began. Each edit is timed from the
//! commit to the fast gauges being ready (`worker::fast_gauges`, the headless Rust part, as the hosted
//! check measures it). Splash load is timed 20 times each on the 22 m fast attack craft and the 90 m
//! destroyer starter, cold then warm: `editor::splash::prepare`, `launch` and the first tick, on a pool of
//! the sim's 3 workers.
//!
//! # sim-calib (L1383)
//! 600 ticks of the reference battle (`trials::reference_config`, seed 1, as `server reference-battle`
//! runs it) with `HostLoad::IDLE`, headless, on a pool forced to 1 thread. Only `Sim::step` is timed; the
//! score is the mean wall time per tick in microseconds.
use crate::enums::BenchStage;
use crate::seams::QualityTier;
use sim_systems::editor::trials::ReferenceScene;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// The six stock starter hulls (L151).
pub(crate) const STARTERS: [&str; 6] = ["starters/patrol15.bp.ron", "starters/fac22.bp.ron", "starters/insertion26.bp.ron", "starters/corvette40.bp.ron", "starters/corvette60.bp.ron", "starters/destroyer90.bp.ron"];
/// Hulls the splash load is timed on (L218; the stress hull does not exist yet).
pub(crate) const SPLASH_HULLS: [&str; 2] = ["starters/fac22.bp.ron", "starters/destroyer90.bp.ron"];
pub(crate) const EDITS_PER_HULL: usize = 200;
pub(crate) const SPLASH_RUNS: usize = 20;
pub(crate) const CALIB_TICKS: u64 = 600;
pub(crate) const CALIB_SEED: u64 = 1;

/// Nearest-rank percentile of unsorted samples; 0 for none. Non-finite samples are ignored.
pub(crate) fn percentile(samples: &[f32], p: f64) -> f32 {
    let mut v: Vec<f32> = samples.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() { return 0.0; }
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    let idx = ((p.clamp(0.0, 1.0) * n as f64).ceil() as usize).clamp(1, n) - 1;
    v[idx]
}

/// Progress and cancellation shared with a headless stage's thread.
#[derive(Default)]
pub(crate) struct JobShared {
    pub(crate) cancel: AtomicBool,
    /// Per mille of the stage done.
    pub(crate) progress: AtomicU32,
}
impl JobShared {
    fn set(&self, done: usize, total: usize) { self.progress.store(((done as f64 / total.max(1) as f64) * 1000.0).round().clamp(0.0, 1000.0) as u32, Ordering::Relaxed); }
    fn cancelled(&self) -> bool { self.cancel.load(Ordering::Relaxed) }
}

/// What a headless stage measured.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JobFigures {
    pub(crate) gauge_ms: Vec<f32>,
    pub(crate) splash_ms: Vec<f32>,
    pub(crate) calib_us: Option<f64>,
    /// Developer text of anything that could not be measured.
    pub(crate) problems: Vec<String>,
}

/// Counts of the editor stage; smaller in a shortened (development) run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EditorPlan { pub(crate) edits_per_hull: usize, pub(crate) splash_runs: usize, pub(crate) hulls: usize }
impl Default for EditorPlan { fn default() -> Self { Self { edits_per_hull: EDITS_PER_HULL, splash_runs: SPLASH_RUNS, hulls: STARTERS.len() } } }

/// The edit replay on one hull: the design after edit `k` (see the module notes).
pub(crate) fn replay_edit(base: &content::blueprint::Blueprint, k: usize) -> content::blueprint::Blueprint {
    let mut bp = base.clone();
    if k.is_multiple_of(2) && !bp.parts.is_empty() {
        let i = (k / 2) % bp.parts.len();
        bp.parts.remove(i);
    }
    bp
}

/// The editor stage. Runs on its own thread; checks `shared.cancel` between edits.
pub(crate) fn editor_stage(plan: EditorPlan, shared: &JobShared, deps: &dyn Fn() -> sim_systems::sim::SimDeps) -> JobFigures {
    let mut out = JobFigures::default();
    let catalogue = content::catalogue::catalogue();
    let campaign = content::campaign::load::campaign();
    let ruleset = content::ruleset::Ruleset::default();
    let hulls = &STARTERS[..plan.hulls.min(STARTERS.len())];
    let total = hulls.len() * plan.edits_per_hull + SPLASH_HULLS.len() * plan.splash_runs;
    let mut done = 0;
    for path in hulls {
        let bp = match content::campaign::load::stock_blueprint(path) {
            Ok(bp) => bp,
            Err(e) => { out.problems.push(format!("{path}: {e}")); done += plan.edits_per_hull; continue; }
        };
        for k in 0..plan.edits_per_hull {
            if shared.cancelled() { return out; }
            let edited = replay_edit(&bp, k);
            let t = Instant::now();
            let (_design, strip) = crate::worker::fast_gauges(&edited, 0.0, false, &ruleset, catalogue);
            out.gauge_ms.push(millis(t));
            std::hint::black_box(&strip);
            done += 1;
            shared.set(done, total);
        }
    }
    let pool = match rayon::ThreadPoolBuilder::new().num_threads(3).thread_name(|i| format!("bp-bench-sim-{i}")).build() {
        Ok(p) => p,
        Err(e) => { out.problems.push(format!("sim pool: {e}")); return out; }
    };
    for path in SPLASH_HULLS {
        let bp = match content::campaign::load::stock_blueprint(path) {
            Ok(bp) => bp,
            Err(e) => { out.problems.push(format!("{path}: {e}")); done += plan.splash_runs; continue; }
        };
        for run in 0..plan.splash_runs {
            if shared.cancelled() { return out; }
            let options = content::settings::SplashOptions::default();
            let d = deps();
            let t = Instant::now();
            let ok = pool.install(|| {
                let prepared = sim_systems::editor::splash::prepare(&bp, options, run as u64 + 1, campaign, catalogue);
                match sim_systems::editor::splash::launch(&prepared, d, sim_systems::editor::splash::SplashStage::Full) {
                    Ok(mut sim) => { sim.step(); drop(sim.drain_events()); true }
                    Err(e) => { std::hint::black_box(e); false }
                }
            });
            if ok { out.splash_ms.push(millis(t)); } else { out.problems.push(format!("{path}: the splash did not start")); }
            done += 1;
            shared.set(done, total);
        }
    }
    shared.set(total, total);
    out
}

/// sim-calib: mean `Sim::step` wall time per tick, microseconds.
pub(crate) fn sim_calib(ticks: u64, shared: &JobShared, deps: sim_systems::sim::SimDeps) -> JobFigures {
    let mut out = JobFigures::default();
    let pool = match rayon::ThreadPoolBuilder::new().num_threads(1).thread_name(|_| "bp-bench-calib".into()).build() {
        Ok(p) => p,
        Err(e) => { out.problems.push(format!("calib pool: {e}")); return out; }
    };
    let config = sim_systems::editor::trials::reference_config(ReferenceScene::ReferenceBattle, CALIB_SEED, &deps.campaign);
    let result = pool.install(|| -> Result<Option<f64>, String> {
        let mut sim = sim_systems::Sim::new(config, deps).map_err(|e| e.to_string())?;
        sim.set_host_load(sim_systems::lod::HostLoad::IDLE);
        let mut total_ns: u128 = 0;
        for i in 0..ticks {
            if shared.cancelled() { return Ok(None); }
            let t = Instant::now();
            sim.step();
            total_ns += t.elapsed().as_nanos();
            drop(sim.drain_events());
            shared.set(i as usize + 1, ticks as usize);
        }
        Ok(Some(total_ns as f64 / ticks.max(1) as f64 / 1_000.0))
    });
    match result {
        Ok(v) => out.calib_us = v,
        Err(e) => out.problems.push(format!("sim-calib: {e}")),
    }
    out
}

fn millis(t: Instant) -> f32 { (t.elapsed().as_secs_f64() * 1_000.0) as f32 }

/// A headless stage running on its own thread.
pub(crate) struct Job {
    pub(crate) shared: Arc<JobShared>,
    pub(crate) rx: std::sync::mpsc::Receiver<JobFigures>,
    pub(crate) thread: Option<std::thread::JoinHandle<()>>,
}
impl Job {
    pub(crate) fn spawn(name: &str, work: impl FnOnce(&JobShared) -> JobFigures + Send + 'static) -> Option<Job> {
        let shared = Arc::new(JobShared::default());
        let (tx, rx) = std::sync::mpsc::channel();
        let s = shared.clone();
        let thread = std::thread::Builder::new().name(format!("bp-bench-{name}")).spawn(move || {
            let figures = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&s))) {
                Ok(f) => f,
                Err(_) => JobFigures { problems: vec!["the stage stopped on an internal error".into()], ..Default::default() },
            };
            let _ = tx.send(figures);
        }).ok()?;
        Some(Job { shared, rx, thread: Some(thread) })
    }
    pub(crate) fn progress(&self) -> f64 { self.shared.progress.load(Ordering::Relaxed) as f64 / 1000.0 }
    pub(crate) fn poll(&mut self) -> Option<JobFigures> {
        match self.rx.try_recv() {
            Ok(f) => { if let Some(t) = self.thread.take() { let _ = t.join(); } Some(f) }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => { if let Some(t) = self.thread.take() { let _ = t.join(); } Some(JobFigures { problems: vec!["the stage thread ended".into()], ..Default::default() }) }
        }
    }
    /// Ask the thread to stop; it is not waited for (it ends at its next check).
    pub(crate) fn cancel(&mut self) { self.shared.cancel.store(true, Ordering::Relaxed); }
    /// Wait for the thread (after `cancel` it ends within one edit or tick).
    pub(crate) fn join(&mut self) { if let Some(t) = self.thread.take() { let _ = t.join(); } }
}
impl Drop for Job {
    fn drop(&mut self) { self.cancel(); }
}

// ---------------------------------------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------------------------------------

/// One battle stage's figures.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StageResult {
    pub(crate) stage: BenchStage,
    pub(crate) tier: QualityTier,
    /// p50, p95, p99.
    pub(crate) frame_ms: [f32; 3],
    pub(crate) gpu_ms: [f32; 3],
    /// p95, p99.
    pub(crate) tick_ms: [f32; 2],
    pub(crate) adapter_ms_p99: f32,
    /// Share of frames that missed a vblank, 0..=1.
    pub(crate) missed_vblanks: f32,
    pub(crate) frames: u32,
    pub(crate) seconds: f64,
    pub(crate) failed: bool,
}

/// Everything the JSON results file holds (L1322, L1401, L2045). No user names, paths or identifiers.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Report {
    pub(crate) build: String,
    pub(crate) os: String,
    pub(crate) cpu: String,
    pub(crate) ram_gb: f64,
    pub(crate) adapter: String,
    pub(crate) driver_version: String,
    pub(crate) rendering_method: String,
    pub(crate) rendering_driver: String,
    pub(crate) fallbacks: Vec<String>,
    pub(crate) tier: QualityTier,
    pub(crate) window: (i32, i32),
    pub(crate) scale_3d: f32,
    pub(crate) stages: Vec<StageResult>,
    pub(crate) pipeline_compilations: u64,
    pub(crate) video_mem_mb: f64,
    pub(crate) peak_mem_mb: f64,
    /// macOS: swap growth over the run and the peak memory-pressure level; 0 elsewhere.
    pub(crate) swap_growth_mb: f64,
    pub(crate) pressure_peak: u8,
    /// Windows: DXGI `CurrentUsage` peak; 0 elsewhere.
    pub(crate) dxgi_usage_mb: f64,
    pub(crate) sim_calib_us: f64,
    pub(crate) editor_gauge_ms_p95: f64,
    pub(crate) editor_splash_ms_p95: f64,
    pub(crate) thermal: Vec<i32>,
    pub(crate) dpi_awareness: String,
    /// The soak's frame p95 over its last 5 minutes and whether it met the tier's budget.
    pub(crate) soak: Option<(f64, bool)>,
    pub(crate) problems: Vec<String>,
}

/// The soak passes with frame p95 over its last 5 minutes at 33.3 ms on Low, 16.7 ms on Medium and above
/// (L1399). The Compatibility fallback is held to the Low figure.
pub(crate) fn soak_budget_ms(tier: QualityTier) -> f64 { match tier { QualityTier::Fallback | QualityTier::Low => 33.3, _ => 16.7 } }

pub(crate) fn tier_name(t: QualityTier) -> &'static str { match t { QualityTier::Fallback => "fallback", QualityTier::Low => "low", QualityTier::Medium => "medium", QualityTier::High => "high" } }
pub(crate) fn stage_name(s: BenchStage) -> &'static str {
    match s {
        BenchStage::Idle => "idle", BenchStage::ReferenceBattle => "reference_battle", BenchStage::CoopReference => "coop_reference", BenchStage::MinimumSwarm => "minimum_swarm",
        BenchStage::Editor => "editor", BenchStage::SimCalib => "sim_calib", BenchStage::Soak => "soak", BenchStage::Done => "done",
    }
}

/// A JSON string literal.
fn js(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}
/// A JSON number: finite, rounded to 3 decimals; non-finite becomes 0.
fn jn(x: f64) -> String { if x.is_finite() { let r = (x * 1000.0).round() / 1000.0; if r == r.trunc() && r.abs() < 1e15 { format!("{}", r as i64) } else { format!("{r}") } } else { "0".into() } }
fn jarr<T>(v: &[T], f: impl Fn(&T) -> String) -> String { format!("[{}]", v.iter().map(f).collect::<Vec<_>>().join(",")) }

impl Report {
    /// The results file, one JSON object (stable key order, `"format": 1`).
    pub(crate) fn json(&self) -> String {
        let stage = |s: &StageResult| {
            format!(
                "{{\"stage\":{},\"tier\":{},\"frame_ms\":{{\"p50\":{},\"p95\":{},\"p99\":{}}},\"gpu_ms\":{{\"p50\":{},\"p95\":{},\"p99\":{}}},\"tick_ms\":{{\"p95\":{},\"p99\":{}}},\"adapter_ms_p99\":{},\"missed_vblanks\":{},\"frames\":{},\"seconds\":{},\"failed\":{}}}",
                js(stage_name(s.stage)), js(tier_name(s.tier)), jn(s.frame_ms[0] as f64), jn(s.frame_ms[1] as f64), jn(s.frame_ms[2] as f64), jn(s.gpu_ms[0] as f64), jn(s.gpu_ms[1] as f64), jn(s.gpu_ms[2] as f64),
                jn(s.tick_ms[0] as f64), jn(s.tick_ms[1] as f64), jn(s.adapter_ms_p99 as f64), jn(s.missed_vblanks as f64), s.frames, jn(s.seconds), s.failed,
            )
        };
        let soak = match self.soak { Some((p95, pass)) => format!("{{\"frame_p95_last_5_min_ms\":{},\"pass\":{}}}", jn(p95), pass), None => "null".into() };
        let mut o = String::from("{");
        let fields: Vec<String> = vec![
            "\"format\":1".into(),
            format!("\"build\":{}", js(&self.build)),
            format!("\"os\":{}", js(&self.os)),
            format!("\"cpu\":{}", js(&self.cpu)),
            format!("\"ram_gb\":{}", jn(self.ram_gb)),
            format!("\"adapter\":{}", js(&self.adapter)),
            format!("\"driver_version\":{}", js(&self.driver_version)),
            format!("\"rendering_method\":{}", js(&self.rendering_method)),
            format!("\"rendering_driver\":{}", js(&self.rendering_driver)),
            format!("\"fallbacks\":{}", jarr(&self.fallbacks, |s| js(s))),
            format!("\"tier\":{}", js(tier_name(self.tier))),
            format!("\"window\":[{},{}]", self.window.0, self.window.1),
            format!("\"scale_3d\":{}", jn(self.scale_3d as f64)),
            format!("\"stages\":{}", jarr(&self.stages, stage)),
            format!("\"pipeline_compilations\":{}", self.pipeline_compilations),
            format!("\"video_mem_mb\":{}", jn(self.video_mem_mb)),
            format!("\"peak_mem_mb\":{}", jn(self.peak_mem_mb)),
            format!("\"swap_growth_mb\":{}", jn(self.swap_growth_mb)),
            format!("\"memory_pressure_peak\":{}", self.pressure_peak),
            format!("\"dxgi_usage_mb\":{}", jn(self.dxgi_usage_mb)),
            format!("\"sim_calib_us\":{}", jn(self.sim_calib_us)),
            format!("\"editor_gauge_ms_p95\":{}", jn(self.editor_gauge_ms_p95)),
            format!("\"editor_splash_ms_p95\":{}", jn(self.editor_splash_ms_p95)),
            format!("\"thermal\":{}", jarr(&self.thermal, |t| t.to_string())),
            format!("\"dpi_awareness\":{}", js(&self.dpi_awareness)),
            format!("\"soak\":{soak}"),
            format!("\"problems\":{}", jarr(&self.problems, |s| js(s))),
        ];
        o.push_str(&fields.join(","));
        o.push('}');
        o
    }
}

/// Text from the engine that goes into the report: control characters dropped, anything that looks like a
/// path or carries the user's name reduced (L1401: no account names, user paths or identifiers).
pub(crate) fn scrub(text: &str, user: &str) -> String {
    let mut t: String = text.chars().filter(|c| !c.is_control()).take(200).collect();
    let user = user.trim();
    if user.len() >= 2 { t = t.replace(user, "user"); }
    let words: Vec<String> = t.split_whitespace().map(|w| if w.contains('/') || w.contains('\\') { "(path)".to_string() } else { w.to_string() }).collect();
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_are_nearest_rank_and_ignore_bad_samples() {
        let v: Vec<f32> = (1..=100).map(|i| i as f32).collect();
        assert_eq!((percentile(&v, 0.5), percentile(&v, 0.95), percentile(&v, 0.99)), (50.0, 95.0, 99.0));
        assert_eq!(percentile(&[], 0.95), 0.0);
        assert_eq!(percentile(&[f32::NAN, 3.0, f32::INFINITY], 0.99), 3.0);
    }

    #[test]
    fn the_edit_replay_changes_every_edit_and_returns_to_the_start() {
        let bp = content::campaign::load::stock_blueprint("starters/fac22.bp.ron").unwrap();
        assert!(!bp.parts.is_empty());
        for k in 0..20 {
            let e = replay_edit(&bp, k);
            if k % 2 == 0 { assert_eq!(e.parts.len() + 1, bp.parts.len()); } else { assert_eq!(e, bp); }
        }
        // Every part is removed once in a pass over the hull.
        let n = bp.parts.len();
        let removed: std::collections::BTreeSet<usize> = (0..2 * n).step_by(2).map(|k| (k / 2) % n).collect();
        assert_eq!(removed.len(), n);
    }

    #[test]
    fn the_report_json_is_well_formed_and_holds_no_paths() {
        let r = Report {
            build: "0.1.0 (local)".into(), os: "macOS 26.0".into(), cpu: "Apple \"M1\"".into(), ram_gb: 8.0, adapter: "Apple M1".into(), tier: QualityTier::Low, window: (1920, 1080),
            stages: vec![StageResult { stage: BenchStage::ReferenceBattle, tier: QualityTier::Low, frame_ms: [16.0, 20.5, 33.25], frames: 10_800, seconds: 180.0, ..Default::default() }],
            fallbacks: vec!["vulkan".into()], thermal: vec![0, 1], soak: Some((30.0, true)), sim_calib_us: f64::NAN, ..Default::default()
        };
        let j = r.json();
        assert!(j.starts_with('{') && j.ends_with('}'));
        assert!(j.contains("\"cpu\":\"Apple \\\"M1\\\"\""));
        assert!(j.contains("\"stage\":\"reference_battle\"") && j.contains("\"p99\":33.25"));
        assert!(j.contains("\"sim_calib_us\":0"));
        assert!(j.contains("\"soak\":{\"frame_p95_last_5_min_ms\":30,\"pass\":true}"));
        // Balanced braces and brackets outside strings.
        let (mut depth, mut inside, mut esc) = (0i32, false, false);
        for c in j.chars() {
            if inside { if esc { esc = false } else if c == '\\' { esc = true } else if c == '"' { inside = false } continue; }
            match c { '"' => inside = true, '{' | '[' => depth += 1, '}' | ']' => depth -= 1, _ => {} }
            assert!(depth >= 0);
        }
        assert_eq!(depth, 0);
        assert_eq!(scrub("/Users/alice/Library/x driver 1.2 alice", "alice"), "(path) driver 1.2 user");
        assert_eq!(soak_budget_ms(QualityTier::Low), 33.3);
        assert_eq!(soak_budget_ms(QualityTier::Medium), 16.7);
    }

    #[test]
    fn a_short_editor_stage_times_gauges_and_splashes_and_stops_on_cancel() {
        let deps = || crate::services::services().sim_deps();
        let shared = JobShared::default();
        let f = editor_stage(EditorPlan { edits_per_hull: 4, splash_runs: 1, hulls: 1 }, &shared, &deps);
        assert_eq!(f.gauge_ms.len(), 4, "{:?}", f.problems);
        assert_eq!(f.splash_ms.len(), SPLASH_HULLS.len(), "{:?}", f.problems);
        assert_eq!(shared.progress.load(Ordering::Relaxed), 1000);
        let cancelled = JobShared::default();
        cancelled.cancel.store(true, Ordering::Relaxed);
        let f = editor_stage(EditorPlan::default(), &cancelled, &deps);
        assert!(f.gauge_ms.is_empty() && f.splash_ms.is_empty());
    }

    #[test]
    fn sim_calib_scores_mean_tick_time() {
        let shared = JobShared::default();
        let f = sim_calib(5, &shared, crate::services::services().sim_deps());
        let us = f.calib_us.expect("a score");
        assert!(us.is_finite() && us > 0.0, "{us} {:?}", f.problems);
    }
}
