//! Unit `gd-worker`. The one background worker thread: editor gauges, Ship's Notes, trials, Fit Check,
//! Quartermaster, splash pre-builds, building received blueprints. DESIGN.md L1086 (one background worker;
//! Fit Check may also use the sim pool during briefings, when no sortie is ticking), L155 and L177-187 (gauges
//! within 100 ms of an edit, split-strip fallback), L218-223 (splash load, pre-built and progressive splash),
//! L227 (trial battery within 60 s of a save), L805 (Quartermaster: 300 ms per candidate, 5 s in total).
//!
//! Plain data in, plain data out. The worker owns no Godot object. A newer job of the same kind for the same
//! design supersedes an older queued one; a running job checks its [`sim_systems::editor::Deadline`] between
//! candidates and simulated seconds. Results are polled from the main thread once per frame by `BpEditor`.
//!
//! # Lanes
//! The `bp-worker` thread runs the quick jobs itself, one at a time and in priority order: `FastGauges`
//! (always first, so an edit is never stuck behind other work), `BuildReceived`, `SlowGaugesAndNotes`,
//! `PrebuildSplash`. The long jobs (`FitCheck`, `Quartermaster`, `Trials`, `NoteEffects`) run on a rayon
//! pool of 3 threads that the worker creates only while no sortie ticks (L1086): at most one job of each
//! kind at a time, so a 60 s trial battery never delays a gauge. When a sortie starts ticking (a Splash
//! included) the running long jobs are stopped at their next deadline check, put back at the head of the
//! queue, the pool is dropped, and they start again from the beginning once the sortie stops. The editor
//! reports the sortie state with [`WorkerHandle::set_sortie_ticking`].
//!
//! # Results
//! A job stopped by a newer job of its kind, by [`WorkerHandle::cancel_before`] or by a sortie starting never
//! reports a partial result. [`WorkerHandle::poll`] takes every result except `BuiltReceived`; those go to
//! [`WorkerHandle::poll_received`], which `BpSession` calls (it is the writer of `Services::designs`).
//!
//! # Panics
//! Every job runs inside `catch_unwind`. The process panic hook (`BpApp::crash_reports_init`, L1338) has
//! already reported the panic when it unwinds here, so the worker only turns it into
//! [`JobResult::Failed`] (or a `BuiltReceived` error) and carries on. Nothing here calls a `godot_*!` macro:
//! this thread may not touch Godot.
use content::blueprint::Blueprint;
use content::ids::ContentHash;
use content::settings::SplashOptions;
use sim_systems::editor::fitcheck::{FitBrief, FitResult};
use sim_systems::editor::gauges::GaugeStrip;
use sim_systems::editor::notes::ShipNote;
use sim_systems::editor::quartermaster::QmResult;
use sim_systems::editor::splash::PreparedSplash;
use sim_systems::editor::trials::{BatterySpec, Scorecard};
use sim_systems::ship::ShipDesign;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What to compute. Every job carries the design revision it was made from; results for an older revision are
/// marked stale by the receiver.
pub enum Job {
    /// Build the design (grid, prefab, compiled graph) and the fast gauges: Float, Speed, Power, Punch.
    /// `dragging` lowers the GZ resolution to 10 degree steps (L187).
    FastGauges { rev: u64, blueprint: Arc<Blueprint>, cap_t: f64, dragging: bool },
    /// The slower gauges of the split strip (Resilience, Sight coverage, Endurance) and the full note list.
    SlowGaugesAndNotes { rev: u64, design: Arc<ShipDesign>, blueprint: Arc<Blueprint>, cap_t: f64, fit: Option<Arc<FitResult>> },
    Trials { rev: u64, blueprint: Arc<Blueprint>, spec: BatterySpec },
    FitCheck { rev: u64, blueprint: Arc<Blueprint>, brief: Arc<FitBrief>, seconds: f64 },
    Quartermaster { rev: u64, blueprint: Arc<Blueprint>, brief: Arc<FitBrief>, ruleset: Arc<content::ruleset::Ruleset>, parts_store: Arc<std::collections::BTreeMap<content::ids::PartId, u16>>, available: Option<Arc<Vec<content::ids::PartId>>> },
    /// Keep a ready splash world in step with the design after 500 ms of idle (L221).
    PrebuildSplash { rev: u64, blueprint: Arc<Blueprint>, options: SplashOptions, seed: u64 },
    /// Unpack and build a blueprint that arrived over the network (`ClientLink::blueprint` after
    /// `ClientEvent::BlueprintReady`, L945). `exterior_only` is `EntityIntro::exterior_only`: the other PvP
    /// team's hulls arrive as the exterior-only form and are decoded with `net::blueprints::unpack_exterior`
    /// (which checks the exterior hash); everything else with `content::share::unpack`, then built into a
    /// `ShipDesign`. The host already validated the design; a client only needs it to draw.
    BuildReceived { packed: content::share::PackedBlueprint, exterior_only: bool },
    /// Measure the fallback note ordering: score with and without each Fix (L198).
    NoteEffects { rev: u64, blueprint: Arc<Blueprint> },
}

pub enum JobResult {
    FastGauges { rev: u64, design: Arc<ShipDesign>, gauges: Box<GaugeStrip>, millis: f32 },
    SlowGaugesAndNotes { rev: u64, gauges: Box<GaugeStrip>, notes: Vec<ShipNote>, millis: f32 },
    Trials { rev: u64, scorecard: Box<Scorecard> },
    FitCheck { rev: u64, result: Box<FitResult> },
    Quartermaster { rev: u64, result: Box<QmResult> },
    PrebuiltSplash { rev: u64, prepared: Arc<PreparedSplash>, millis: f32 },
    BuiltReceived { hash: ContentHash, result: Result<ReceivedDesign, String> },
    NoteEffects { rev: u64, effects: Vec<(usize, f64)> },
    /// A job panicked; the panic was reported (L1338) and the worker carries on.
    Failed { what: &'static str, message: String },
}

/// A design received for drawing. The hull renderer builds its `render::hull::HullCells` from either: the
/// full form has every cell and part, the exterior form has the hull shell, paint, markings and weather-deck
/// parts only, with no sealed-air bits.
#[derive(Clone)]
pub enum ReceivedDesign {
    Full { design: Arc<ShipDesign>, blueprint: Arc<Blueprint> },
    Exterior(Arc<content::blueprint::ExteriorBlueprint>),
}

/// Main-thread handle to the worker thread.
pub struct WorkerHandle {
    pub(crate) jobs: std::sync::mpsc::Sender<Job>,
    pub(crate) results: std::sync::mpsc::Receiver<JobResult>,
    pub(crate) thread: Option<std::thread::JoinHandle<()>>,
    pub(crate) cancel: Arc<std::sync::atomic::AtomicU64>,
    /// `BuiltReceived` results, kept apart for `BpSession` ([`WorkerHandle::poll_received`]).
    pub(crate) received: std::sync::mpsc::Receiver<JobResult>,
    /// State the main thread sets and the worker reads (sortie ticking, editing ruleset, stop flag).
    pub(crate) shared: Arc<WorkerShared>,
}

/// State shared between the main thread and the worker.
pub(crate) struct WorkerShared {
    /// A sortie (or a Splash) is ticking: long jobs wait and the long pool is dropped (L1086).
    pub(crate) sortie_ticking: AtomicBool,
    /// Set by `shutdown`.
    pub(crate) stop: AtomicBool,
    /// A Fit Check took longer than 300 ms of wall time: later checks simulate 15 s (L893 fallback).
    pub(crate) fit_short: AtomicBool,
    pub(crate) config: Mutex<WorkerConfig>,
    /// The note list the last `NoteEffects` job measured, by revision: its effects index into it.
    pub(crate) measured: Mutex<Option<(u64, Vec<ShipNote>)>>,
}

/// What the editor tells the worker about the design under edit that the job variants do not carry.
#[derive(Clone)]
pub(crate) struct WorkerConfig {
    /// The ruleset designs are compiled under (`ShipDesign::build`): the dock's ruleset.
    pub(crate) ruleset: Arc<content::ruleset::Ruleset>,
    /// Parts a migration removed from the design under edit; each becomes a Ship's Note (L2083).
    pub(crate) removed: Arc<Vec<content::migrate::RemovedPart>>,
}
impl Default for WorkerConfig {
    fn default() -> Self { Self { ruleset: Arc::new(content::ruleset::Ruleset::default()), removed: Arc::new(Vec::new()) } }
}

/// Wall-clock limits. The trial battery has no spec limit beyond its 60 s p95 gate, so its ceiling only stops a
/// pathological design from holding a pool thread forever; a run that hits it reports a failure, never a
/// partial scorecard.
const TRIALS_LIMIT: Duration = Duration::from_secs(300);
const NOTE_EFFECTS_LIMIT: Duration = Duration::from_secs(180);
/// A single Fit Check: the 30 s simulation runs in well under a second; this only bounds a runaway design.
const FIT_LIMIT: Duration = Duration::from_secs(10);
/// L805: 300 ms per candidate, 5 s in total.
const QM_TOTAL: Duration = Duration::from_secs(5);
const QM_CANDIDATE: Duration = Duration::from_millis(300);
/// L893: a check slower than this drops later checks to [`FIT_SHORT_S`] simulated seconds.
const FIT_SLOW: Duration = Duration::from_millis(300);
const FIT_SHORT_S: f64 = 15.0;
/// Wall-time ceiling of Fit Check's seakeeping sweep, after the tag run (six 120 s runs; about 4 s for the
/// 90 m starter on three workers). A sweep it cuts short is incomplete and never reads as safe.
const SEAKEEPING_LIMIT: Duration = Duration::from_secs(20);
/// Long jobs run on a pool of this many threads (L805: "across the 3 workers").
const LONG_THREADS: usize = 3;
/// Most Fixes whose effect the fallback ordering measures per save (the highest-ranked first).
const MAX_EFFECT_FIXES: usize = 8;
/// GZ step while a handle is dragged and on release (L187).
const GZ_DRAG_DEG: f64 = 10.0;
const GZ_FULL_DEG: f64 = 2.0;
/// Names of the gauges each job fills, as `GaugeStripDict` keys.
pub(crate) const FAST_GAUGES: [&str; 4] = ["float", "speed", "power", "punch"];
pub(crate) const SLOW_GAUGES: [&str; 3] = ["endurance", "sight", "resilience"];

impl WorkerHandle {
    /// Start the worker thread (named `bp-worker`). `deps` builds fresh `SimDeps` for each headless run
    /// (`Services::sim_deps_factory`). Fit Check and Quartermaster candidates may fan out on a rayon pool of 3
    /// that the worker creates only while no sortie is ticking (L1086, L805).
    pub fn spawn(deps: Box<dyn Fn() -> sim_systems::sim::SimDeps + Send>) -> WorkerHandle {
        let (jobs, job_rx) = std::sync::mpsc::channel::<Job>();
        let (result_tx, results) = std::sync::mpsc::channel::<JobResult>();
        let (received_tx, received) = std::sync::mpsc::channel::<JobResult>();
        let cancel = Arc::new(AtomicU64::new(0));
        let shared = Arc::new(WorkerShared { sortie_ticking: AtomicBool::new(false), stop: AtomicBool::new(false), fit_short: AtomicBool::new(false), config: Mutex::new(WorkerConfig::default()), measured: Mutex::new(None) });
        let (c, s) = (cancel.clone(), shared.clone());
        let thread = std::thread::Builder::new()
            .name("bp-worker".into())
            .spawn(move || {
                let mut w = Worker::new(deps, c, s, result_tx, received_tx);
                w.run(job_rx);
            })
            .ok();
        WorkerHandle { jobs, results, thread, cancel, received, shared }
    }
    /// Queue a job, superseding an older queued job of the same kind.
    pub fn submit(&self, job: Job) {
        // A send fails only after shutdown; the job goes with the worker.
        let _ = self.jobs.send(job);
    }
    /// Tell running and queued jobs made from revisions older than `rev` to stop at their next deadline check.
    pub fn cancel_before(&self, rev: u64) { self.cancel.fetch_max(rev, Ordering::Relaxed); }
    /// Take finished results without blocking: every kind except `BuiltReceived`, which
    /// [`WorkerHandle::poll_received`] hands to `BpSession`. Polled by `BpEditor`.
    pub fn poll(&self, out: &mut Vec<JobResult>) { out.extend(self.results.try_iter()); }
    /// Take finished `BuiltReceived` results without blocking. `BpSession` calls this each frame and inserts
    /// them into `Services::designs`.
    pub fn poll_received(&self, out: &mut Vec<JobResult>) { out.extend(self.received.try_iter()); }
    /// Whether a sortie (a Splash included) is ticking. Long jobs wait while it is, so they never compete with
    /// the sim pool (L1086). `BpEditor::process` reports it every frame.
    pub fn set_sortie_ticking(&self, ticking: bool) { self.shared.sortie_ticking.store(ticking, Ordering::Relaxed); }
    /// The ruleset designs under edit are compiled under, and the parts their last load removed (each a Ship's
    /// Note). `BpEditor` sets it whenever the dock opens or a design is loaded.
    pub fn set_design_context(&self, ruleset: content::ruleset::Ruleset, removed: Vec<content::migrate::RemovedPart>) {
        let mut c = self.shared.config.lock().unwrap_or_else(|e| e.into_inner());
        *c = WorkerConfig { ruleset: Arc::new(ruleset), removed: Arc::new(removed) };
    }
    /// The notes a `NoteEffects` result of revision `rev` indexes into.
    pub fn measured_notes(&self, rev: u64) -> Option<Vec<ShipNote>> {
        self.shared.measured.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|(r, _)| *r == rev).map(|(_, n)| n.clone())
    }
    /// True once a Fit Check has run slower than 300 ms; later checks simulate 15 s (L893).
    pub fn fit_short(&self) -> bool { self.shared.fit_short.load(Ordering::Relaxed) }
    /// Stop and join (application exit). Idempotent.
    pub fn shutdown(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.cancel.store(u64::MAX, Ordering::Relaxed);
        // Dropping the only sender ends the worker's wait at once.
        let (dead, _) = std::sync::mpsc::channel();
        self.jobs = dead;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) { self.shutdown(); }
}

/// The wall-clock deadline the worker hands to `sim_systems::editor` functions.
pub struct WallDeadline { pub(crate) total: std::time::Instant, pub(crate) candidate: std::time::Instant, pub(crate) total_limit: std::time::Duration, pub(crate) candidate_limit: std::time::Duration, pub(crate) cancel: Arc<std::sync::atomic::AtomicU64>, pub(crate) rev: u64 }
impl sim_systems::editor::Deadline for WallDeadline {
    fn expired(&mut self) -> bool { self.total.elapsed() >= self.total_limit || self.cancel.load(std::sync::atomic::Ordering::Relaxed) > self.rev }
    fn start_candidate(&mut self) { self.candidate = std::time::Instant::now(); }
    fn candidate_expired(&mut self) -> bool { self.candidate.elapsed() >= self.candidate_limit }
}
impl WallDeadline {
    pub(crate) fn new(total_limit: Duration, candidate_limit: Duration, cancel: Arc<AtomicU64>, rev: u64) -> Self {
        let now = Instant::now();
        Self { total: now, candidate: now, total_limit, candidate_limit, cancel, rev }
    }
    fn cancelled(&self) -> bool { self.cancel.load(Ordering::Relaxed) > self.rev }
    fn timed_out(&self) -> bool { self.total.elapsed() >= self.total_limit }
}

// ---------------------------------------------------------------------------------------------------------
// Job kinds and the queue (pure).

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind { Fast, Received, Slow, Prebuild, Fit, Qm, Trials, Effects }
impl Kind {
    pub(crate) fn of(job: &Job) -> Kind {
        match job {
            Job::FastGauges { .. } => Kind::Fast,
            Job::BuildReceived { .. } => Kind::Received,
            Job::SlowGaugesAndNotes { .. } => Kind::Slow,
            Job::PrebuildSplash { .. } => Kind::Prebuild,
            Job::FitCheck { .. } => Kind::Fit,
            Job::Quartermaster { .. } => Kind::Qm,
            Job::Trials { .. } => Kind::Trials,
            Job::NoteEffects { .. } => Kind::Effects,
        }
    }
    pub(crate) fn long(self) -> bool { matches!(self, Kind::Fit | Kind::Qm | Kind::Trials | Kind::Effects) }
    /// The name `Failed::what` and `BpEditor::job_changed` use.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::Fast => "gauges",
            Kind::Received => "received",
            Kind::Slow => "notes",
            Kind::Prebuild => "prebuild",
            Kind::Fit => "fit",
            Kind::Qm => "quartermaster",
            Kind::Trials => "trials",
            Kind::Effects => "note_effects",
        }
    }
}

fn job_rev(job: &Job) -> Option<u64> {
    match job {
        Job::FastGauges { rev, .. } | Job::SlowGaugesAndNotes { rev, .. } | Job::Trials { rev, .. } | Job::FitCheck { rev, .. } | Job::Quartermaster { rev, .. } | Job::PrebuildSplash { rev, .. } | Job::NoteEffects { rev, .. } => Some(*rev),
        Job::BuildReceived { .. } => None,
    }
}

/// A copy of a job (every payload is shared or small), kept so a preempted long job can be queued again.
fn copy_job(job: &Job) -> Job {
    match job {
        Job::FastGauges { rev, blueprint, cap_t, dragging } => Job::FastGauges { rev: *rev, blueprint: blueprint.clone(), cap_t: *cap_t, dragging: *dragging },
        Job::SlowGaugesAndNotes { rev, design, blueprint, cap_t, fit } => Job::SlowGaugesAndNotes { rev: *rev, design: design.clone(), blueprint: blueprint.clone(), cap_t: *cap_t, fit: fit.clone() },
        Job::Trials { rev, blueprint, spec } => Job::Trials { rev: *rev, blueprint: blueprint.clone(), spec: *spec },
        Job::FitCheck { rev, blueprint, brief, seconds } => Job::FitCheck { rev: *rev, blueprint: blueprint.clone(), brief: brief.clone(), seconds: *seconds },
        Job::Quartermaster { rev, blueprint, brief, ruleset, parts_store, available } => Job::Quartermaster { rev: *rev, blueprint: blueprint.clone(), brief: brief.clone(), ruleset: ruleset.clone(), parts_store: parts_store.clone(), available: available.clone() },
        Job::PrebuildSplash { rev, blueprint, options, seed } => Job::PrebuildSplash { rev: *rev, blueprint: blueprint.clone(), options: *options, seed: *seed },
        Job::BuildReceived { packed, exterior_only } => Job::BuildReceived { packed: packed.clone(), exterior_only: *exterior_only },
        Job::NoteEffects { rev, blueprint } => Job::NoteEffects { rev: *rev, blueprint: blueprint.clone() },
    }
}

/// Queued jobs. Quick and long jobs share the rules: a new job replaces a queued job of its kind, except
/// `BuildReceived`, which is first in, first out with duplicates of one hash dropped (each ship's design
/// matters). Jobs leave by kind priority, then arrival.
#[derive(Default)]
pub(crate) struct Queue {
    jobs: VecDeque<(u64, Job)>,
    seq: u64,
}
impl Queue {
    pub(crate) fn len(&self) -> usize { self.jobs.len() }
    /// Queue a job; returns the kind it superseded, if a queued job was replaced.
    pub(crate) fn push(&mut self, job: Job) -> Option<Kind> {
        let kind = Kind::of(&job);
        self.seq += 1;
        if let Job::BuildReceived { packed, .. } = &job {
            let hash = packed.hash;
            if self.jobs.iter().any(|(_, j)| matches!(j, Job::BuildReceived { packed, .. } if packed.hash == hash)) { return None; }
            self.jobs.push_back((self.seq, job));
            return None;
        }
        let before = self.jobs.len();
        self.jobs.retain(|(_, j)| Kind::of(j) != kind);
        let replaced = (self.jobs.len() != before).then_some(kind);
        self.jobs.push_back((self.seq, job));
        replaced
    }
    /// Put a preempted job back at the head of its kind's line.
    pub(crate) fn requeue(&mut self, job: Job) {
        let kind = Kind::of(&job);
        // A newer job of the kind arrived meanwhile: it wins.
        if kind != Kind::Received && self.jobs.iter().any(|(_, j)| Kind::of(j) == kind) { return; }
        self.jobs.push_front((0, job));
    }
    /// Next quick job: lowest kind first (gauges before everything), then oldest.
    pub(crate) fn pop_quick(&mut self) -> Option<Job> {
        let i = self.jobs.iter().enumerate().filter(|(_, (_, j))| !Kind::of(j).long()).min_by_key(|(_, (s, j))| (Kind::of(j), *s)).map(|(i, _)| i)?;
        self.jobs.remove(i).map(|(_, j)| j)
    }
    /// Next long job whose kind is not already running: lowest kind first, then oldest.
    pub(crate) fn pop_long(&mut self, running: &[Kind]) -> Option<Job> {
        let i = self.jobs.iter().enumerate().filter(|(_, (_, j))| Kind::of(j).long() && !running.contains(&Kind::of(j))).min_by_key(|(_, (s, j))| (Kind::of(j), *s)).map(|(i, _)| i)?;
        self.jobs.remove(i).map(|(_, j)| j)
    }
    /// Drop queued jobs made from revisions older than `rev` (`cancel_before`).
    pub(crate) fn drop_older(&mut self, rev: u64) { self.jobs.retain(|(_, j)| job_rev(j).is_none_or(|r| r >= rev)); }
}

// ---------------------------------------------------------------------------------------------------------
// The worker thread.

type DepsFactory = Arc<Mutex<Box<dyn Fn() -> sim_systems::sim::SimDeps + Send>>>;

/// One long job on the pool.
struct Running {
    id: u64,
    kind: Kind,
    rev: u64,
    /// The job's own cancel word: its deadline expires once this exceeds `rev`.
    cancel: Arc<AtomicU64>,
    /// Stopped because a sortie started: queue `keep` again.
    preempted: bool,
    keep: Job,
}

/// What a pool thread hands back.
struct Done { id: u64, result: Option<JobResult> }

struct Worker {
    deps: DepsFactory,
    catalogue: Arc<content::catalogue::Catalogue>,
    campaign: Arc<content::campaign::CampaignData>,
    cancel: Arc<AtomicU64>,
    shared: Arc<WorkerShared>,
    results: Sender<JobResult>,
    received: Sender<JobResult>,
    done_tx: Sender<Done>,
    done_rx: Receiver<Done>,
    queue: Queue,
    pool: Option<Arc<rayon::ThreadPool>>,
    running: Vec<Running>,
    next_id: u64,
    /// The last fast strip, so the slow job of the same revision does not recompute it.
    last_fast: Option<(u64, GaugeStrip)>,
    /// Complexity points of the last trial battery this worker ran (notes compare them with the caps).
    complexity_points: Arc<Mutex<f64>>,
}

impl Worker {
    fn new(deps: Box<dyn Fn() -> sim_systems::sim::SimDeps + Send>, cancel: Arc<AtomicU64>, shared: Arc<WorkerShared>, results: Sender<JobResult>, received: Sender<JobResult>) -> Worker {
        let probe = deps();
        let (catalogue, campaign) = (probe.catalogue.clone(), probe.campaign.clone());
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        Worker {
            deps: Arc::new(Mutex::new(deps)),
            catalogue,
            campaign,
            cancel,
            shared,
            results,
            received,
            done_tx,
            done_rx,
            queue: Queue::default(),
            pool: None,
            running: Vec::new(),
            next_id: 0,
            last_fast: None,
            complexity_points: Arc::new(Mutex::new(0.0)),
        }
    }

    fn run(&mut self, jobs: Receiver<Job>) {
        let mut open = true;
        while open && !self.shared.stop.load(Ordering::Relaxed) {
            // Everything that arrived, then finished long jobs.
            loop {
                match jobs.try_recv() {
                    Ok(job) => self.accept(job),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => { open = false; break; }
                }
            }
            if !open { break; }
            self.collect_done();
            self.apply_cancel();
            self.manage_long();
            if let Some(job) = self.queue.pop_quick() {
                self.run_quick(job);
                continue;
            }
            // Nothing quick to do: wait for a job, waking now and then for long-job completions.
            let wait = if self.running.is_empty() { Duration::from_millis(100) } else { Duration::from_millis(10) };
            match jobs.recv_timeout(wait) {
                Ok(job) => self.accept(job),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => open = false,
            }
        }
        // Stop every long job at its next check; the pool threads finish on their own.
        for r in &self.running { r.cancel.store(u64::MAX, Ordering::Relaxed); }
    }

    fn accept(&mut self, job: Job) {
        let kind = Kind::of(&job);
        if kind.long() {
            // A newer job of a kind already running stops the running one: its result would be stale.
            for r in self.running.iter_mut().filter(|r| r.kind == kind) {
                r.preempted = false;
                r.cancel.store(u64::MAX, Ordering::Relaxed);
            }
        }
        self.queue.push(job);
    }

    fn collect_done(&mut self) {
        while let Ok(done) = self.done_rx.try_recv() {
            let Some(i) = self.running.iter().position(|r| r.id == done.id) else { continue };
            let r = self.running.remove(i);
            if r.preempted {
                self.queue.requeue(r.keep);
            } else if r.cancel.load(Ordering::Relaxed) > r.rev {
                // Superseded or cancelled: a partial result is never reported.
            } else if let Some(result) = done.result {
                let _ = self.results.send(result);
            }
        }
    }

    fn apply_cancel(&mut self) {
        let floor = self.cancel.load(Ordering::Relaxed);
        if floor == 0 { return; }
        self.queue.drop_older(floor);
        for r in self.running.iter_mut().filter(|r| r.rev < floor) {
            r.preempted = false;
            r.cancel.store(u64::MAX, Ordering::Relaxed);
        }
    }

    /// Start long jobs while no sortie ticks; stop and park them while one does.
    fn manage_long(&mut self) {
        if self.shared.sortie_ticking.load(Ordering::Relaxed) {
            for r in self.running.iter_mut() {
                if r.cancel.load(Ordering::Relaxed) <= r.rev {
                    r.preempted = true;
                    r.cancel.store(u64::MAX, Ordering::Relaxed);
                }
            }
            // The threads exit once their stopped jobs return.
            self.pool = None;
            return;
        }
        while self.running.len() < LONG_THREADS {
            let kinds: Vec<Kind> = self.running.iter().map(|r| r.kind).collect();
            let Some(job) = self.queue.pop_long(&kinds) else { break };
            let Some(pool) = self.long_pool() else {
                self.queue.requeue(job);
                break;
            };
            self.spawn_long(&pool, job);
        }
    }

    fn long_pool(&mut self) -> Option<Arc<rayon::ThreadPool>> {
        if self.pool.is_none() {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(LONG_THREADS).thread_name(|i| format!("bp-worker-pool-{i}")).build().ok()?;
            self.pool = Some(Arc::new(pool));
        }
        self.pool.clone()
    }

    fn spawn_long(&mut self, pool: &rayon::ThreadPool, job: Job) {
        let kind = Kind::of(&job);
        let rev = job_rev(&job).unwrap_or(0);
        self.next_id += 1;
        let id = self.next_id;
        let cancel = Arc::new(AtomicU64::new(self.cancel.load(Ordering::Relaxed).min(rev)));
        self.running.push(Running { id, kind, rev, cancel: cancel.clone(), preempted: false, keep: copy_job(&job) });
        let ctx = LongCtx { deps: self.deps.clone(), catalogue: self.catalogue.clone(), campaign: self.campaign.clone(), shared: self.shared.clone(), complexity: self.complexity_points.clone() };
        let done = self.done_tx.clone();
        // Inside the pool, the design builder's own parallel loops stay on these 3 threads.
        pool.spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_long(job, &ctx, cancel)));
            let result = match outcome {
                Ok(r) => r,
                Err(p) => Some(JobResult::Failed { what: kind.name(), message: panic_text(&p) }),
            };
            let _ = done.send(Done { id, result });
        });
    }

    fn run_quick(&mut self, job: Job) {
        let kind = Kind::of(&job);
        let received_hash = match &job { Job::BuildReceived { packed, .. } => Some(packed.hash), _ => None };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.quick(job)));
        match (outcome, received_hash) {
            (Ok(Some(r @ JobResult::BuiltReceived { .. })), _) => { let _ = self.received.send(r); }
            (Ok(Some(r)), _) => { let _ = self.results.send(r); }
            (Ok(None), _) => {}
            (Err(p), Some(hash)) => { let _ = self.received.send(JobResult::BuiltReceived { hash, result: Err(panic_text(&p)) }); }
            (Err(p), None) => {
                if kind == Kind::Fast { self.last_fast = None; }
                let _ = self.results.send(JobResult::Failed { what: kind.name(), message: panic_text(&p) });
            }
        }
    }

    fn ruleset(&self) -> Arc<content::ruleset::Ruleset> { self.shared.config.lock().unwrap_or_else(|e| e.into_inner()).ruleset.clone() }
    fn removed(&self) -> Arc<Vec<content::migrate::RemovedPart>> { self.shared.config.lock().unwrap_or_else(|e| e.into_inner()).removed.clone() }

    fn quick(&mut self, job: Job) -> Option<JobResult> {
        let cat = self.catalogue.clone();
        match job {
            Job::FastGauges { rev, blueprint, cap_t, dragging } => {
                let t = Instant::now();
                let (design, strip) = fast_gauges(&blueprint, cap_t, dragging, &self.ruleset(), &cat);
                self.last_fast = Some((rev, strip.clone()));
                Some(JobResult::FastGauges { rev, design, gauges: Box::new(strip), millis: millis(t) })
            }
            Job::SlowGaugesAndNotes { rev, design, blueprint, cap_t, fit } => {
                let t = Instant::now();
                // The fast gauges of this revision, at full GZ resolution: reuse them when the last fast job
                // computed exactly those.
                let fast = match &self.last_fast {
                    Some((r, strip)) if *r == rev && strip.float.gz.windows(2).all(|w| (w[1].0 - w[0].0 - GZ_FULL_DEG).abs() < 1e-6) => strip.clone(),
                    _ => fast_strip(&design, cap_t, GZ_FULL_DEG, &cat),
                };
                let points = *self.complexity_points.lock().unwrap_or_else(|e| e.into_inner());
                let strip = slow_gauges(fast, &design, points, &cat);
                let notes = sim_systems::editor::notes::ship_notes(&blueprint, &design, &strip, fit.as_deref(), &self.removed(), &cat);
                Some(JobResult::SlowGaugesAndNotes { rev, gauges: Box::new(strip), notes, millis: millis(t) })
            }
            Job::PrebuildSplash { rev, blueprint, options, seed } => {
                let t = Instant::now();
                let prepared = sim_systems::editor::splash::prepare(&blueprint, options, seed, &self.campaign, &cat);
                Some(JobResult::PrebuiltSplash { rev, prepared: Arc::new(prepared), millis: millis(t) })
            }
            Job::BuildReceived { packed, exterior_only } => Some(JobResult::BuiltReceived { hash: packed.hash, result: build_received(&packed, exterior_only, &cat) }),
            // Long jobs never reach the quick lane.
            other => {
                self.queue.requeue(other);
                None
            }
        }
    }
}

fn millis(t: Instant) -> f32 { (t.elapsed().as_secs_f64() * 1_000.0) as f32 }

fn panic_text(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() { return (*s).to_string(); }
    if let Some(s) = p.downcast_ref::<String>() { return s.clone(); }
    "panic".to_string()
}

/// Build the design and its fast gauges. The strip's other gauges are defaults and are listed in `stale`.
pub(crate) fn fast_gauges(bp: &Blueprint, cap_t: f64, dragging: bool, ruleset: &content::ruleset::Ruleset, cat: &content::catalogue::Catalogue) -> (Arc<ShipDesign>, GaugeStrip) {
    let prefab = Arc::new(sim_core::vessel::build_prefab(bp, cat));
    let design = Arc::new(ShipDesign::build(bp, prefab, cat, ruleset));
    let strip = fast_strip(&design, cap_t, if dragging { GZ_DRAG_DEG } else { GZ_FULL_DEG }, cat);
    (design, strip)
}

fn fast_strip(design: &ShipDesign, cap_t: f64, gz_step: f64, cat: &content::catalogue::Catalogue) -> GaugeStrip {
    use sim_systems::editor::gauges as g;
    let cap = if cap_t.is_finite() && cap_t > 0.0 { cap_t } else { 0.0 };
    GaugeStrip {
        float: g::float_gauge(&design.prefab, cap, gz_step, cat),
        speed: g::speed_gauge(&design.prefab, cat),
        power: g::power_gauge(&design.prefab, cat),
        punch: g::punch_gauge(design, cat),
        stale: SLOW_GAUGES.iter().chain(["complexity"].iter()).map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

/// Fill the slow gauges and Complexity into a strip whose fast gauges are current. Nothing is stale after.
pub(crate) fn slow_gauges(mut strip: GaugeStrip, design: &ShipDesign, complexity_points: f64, cat: &content::catalogue::Catalogue) -> GaugeStrip {
    use sim_systems::editor::gauges as g;
    strip.endurance = g::endurance_gauge(&design.prefab, cat);
    strip.sight = g::sight_gauge(design, cat);
    strip.resilience = g::resilience_gauge(&design.prefab, cat);
    strip.complexity = g::complexity_gauge(if complexity_points.is_finite() { complexity_points.max(0.0) } else { 0.0 }, cat);
    strip.stale.clear();
    strip
}

fn build_received(packed: &content::share::PackedBlueprint, exterior_only: bool, cat: &content::catalogue::Catalogue) -> Result<ReceivedDesign, String> {
    if exterior_only {
        return net::blueprints::unpack_exterior(packed).map(|e| ReceivedDesign::Exterior(Arc::new(e))).map_err(|e| format!("exterior blueprint refused: {e:?}"));
    }
    match content::share::unpack_with(packed, cat) {
        Ok(content::blueprint::Shareable::Ship(bp)) => {
            let prefab = Arc::new(sim_core::vessel::build_prefab(&bp, cat));
            // Drawing needs no particular ruleset; the host compiled the real graph under its own.
            let design = Arc::new(ShipDesign::build(&bp, prefab, cat, &content::ruleset::Ruleset::default()));
            Ok(ReceivedDesign::Full { design, blueprint: Arc::new(*bp) })
        }
        Ok(content::blueprint::Shareable::Kit(_)) => Err("a kit arrived where a ship was expected".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// What a long job needs from the worker.
struct LongCtx {
    deps: DepsFactory,
    catalogue: Arc<content::catalogue::Catalogue>,
    campaign: Arc<content::campaign::CampaignData>,
    shared: Arc<WorkerShared>,
    complexity: Arc<Mutex<f64>>,
}
impl LongCtx {
    fn make_deps(&self) -> sim_systems::sim::SimDeps { (self.deps.lock().unwrap_or_else(|e| e.into_inner()))() }
}

/// Run one long job. `None` when it was stopped (superseded, cancelled, preempted); the worker then reports
/// nothing or queues it again.
fn run_long(job: Job, ctx: &LongCtx, cancel: Arc<AtomicU64>) -> Option<JobResult> {
    let mut make = || ctx.make_deps();
    match job {
        Job::Trials { rev, blueprint, spec } => {
            let mut dl = WallDeadline::new(TRIALS_LIMIT, TRIALS_LIMIT, cancel, rev);
            let card = sim_systems::editor::trials::run_battery(&blueprint, &spec, &ctx.campaign, &mut make, &mut dl);
            if dl.cancelled() { return None; }
            if dl.timed_out() { return Some(JobResult::Failed { what: Kind::Trials.name(), message: "the trial battery ran past its time limit".to_string() }); }
            *ctx.complexity.lock().unwrap_or_else(|e| e.into_inner()) = card.complexity_points;
            Some(JobResult::Trials { rev, scorecard: Box::new(card) })
        }
        Job::FitCheck { rev, blueprint, brief, seconds } => {
            let short = ctx.shared.fit_short.load(Ordering::Relaxed);
            let requested = if seconds.is_finite() && seconds > 0.0 { seconds } else { sim_systems::editor::fitcheck::DEFAULT_SECONDS };
            let secs = if short { requested.min(FIT_SHORT_S) } else { requested };
            let mut dl = WallDeadline::new(FIT_LIMIT, FIT_LIMIT, cancel, rev);
            let t = Instant::now();
            let mut result = sim_systems::editor::fitcheck::fit_check_tags(&blueprint, &brief, make(), secs, &mut dl);
            if dl.cancelled() { return None; }
            // The 300 ms rule (L893) is the tag run's; the seakeeping sweep after it has its own ceiling.
            if t.elapsed() > FIT_SLOW { ctx.shared.fit_short.store(true, Ordering::Relaxed); }
            if result.simulated {
                let mut sea = WallDeadline::new(SEAKEEPING_LIMIT, SEAKEEPING_LIMIT, dl.cancel.clone(), rev);
                result.seakeeping = Some(sim_systems::editor::fitcheck::seakeeping(&blueprint, &brief.weather, &mut make, &mut sea));
                if sea.cancelled() { return None; }
            }
            // A run that hit the 10 s ceiling still carries the analytic scores; `seconds_simulated` says how
            // far the simulation got, and a sweep cut short is not complete, which never reads as safe.
            Some(JobResult::FitCheck { rev, result: Box::new(result) })
        }
        Job::Quartermaster { rev, blueprint, brief, ruleset, parts_store, available } => {
            let mut dl = WallDeadline::new(QM_TOTAL, QM_CANDIDATE, cancel, rev);
            let result = sim_systems::editor::quartermaster::quartermaster(&blueprint, &brief, &parts_store, available.as_deref().map(|v| v.as_slice()), &ruleset, &mut make, &mut dl);
            if dl.cancelled() { return None; }
            // The search's own 5 s limit ends it with the cheapest legal refit found so far: a real answer.
            Some(JobResult::Quartermaster { rev, result: Box::new(result) })
        }
        Job::NoteEffects { rev, blueprint } => {
            let mut dl = WallDeadline::new(NOTE_EFFECTS_LIMIT, NOTE_EFFECTS_LIMIT, cancel, rev);
            let effects = note_effects(rev, &blueprint, ctx, &mut make, &mut dl);
            if dl.cancelled() { return None; }
            match effects {
                Some(effects) => Some(JobResult::NoteEffects { rev, effects }),
                None => Some(JobResult::Failed { what: Kind::Effects.name(), message: "the note measurement ran past its time limit".to_string() }),
            }
        }
        _ => None,
    }
}

/// The fallback note order (L198): each Fix's effect on the one-against-three score. The design and every
/// fixed variant run the same short battery (1 seed, 90 s), so effects compare like with like and stay inside a
/// background budget. Returns `(note index, score with Fix - score without)` for the notes `ship_notes` gives
/// this blueprint with no briefing, in note order.
fn note_effects(rev: u64, bp: &Blueprint, ctx: &LongCtx, make: &mut dyn FnMut() -> sim_systems::sim::SimDeps, dl: &mut WallDeadline) -> Option<Vec<(usize, f64)>> {
    use sim_systems::editor::Deadline;
    let cat = &ctx.catalogue;
    let (ruleset, removed) = {
        let c = ctx.shared.config.lock().unwrap_or_else(|e| e.into_inner());
        (c.ruleset.clone(), c.removed.clone())
    };
    let (design, fast) = fast_gauges(bp, 0.0, false, &ruleset, cat);
    let points = *ctx.complexity.lock().unwrap_or_else(|e| e.into_inner());
    let strip = slow_gauges(fast, &design, points, cat);
    let notes = sim_systems::editor::notes::ship_notes(bp, &design, &strip, None, &removed, cat);
    *ctx.shared.measured.lock().unwrap_or_else(|e| e.into_inner()) = Some((rev, notes.clone()));
    let spec = BatterySpec::SHORT;
    let mut score = |b: &Blueprint, dl: &mut WallDeadline| -> Option<f64> {
        let card = sim_systems::editor::trials::run_battery(b, &spec, &ctx.campaign, make, dl);
        if dl.expired() || card.fight.per_seed.is_empty() { None } else { Some(card.fight.score) }
    };
    let base = score(bp, dl)?;
    let mut out = Vec::new();
    for (i, note) in notes.iter().enumerate().filter(|(_, n)| n.fix.is_some()).take(MAX_EFFECT_FIXES) {
        let Some(fix) = &note.fix else { continue };
        let Some(fixed) = sim_systems::editor::notes::apply_edit(bp, fix, cat) else { continue };
        let s = score(&fixed, dl)?;
        out.push((i, s - base));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bp(path: &str) -> Arc<Blueprint> { Arc::new(content::campaign::load::stock_blueprint(path).expect("stock blueprint")) }
    fn brief() -> FitBrief { FitBrief { tags: Vec::new(), weather: Default::default(), approach_km: 10.0, threats: Vec::new(), cap_t: 150.0 } }
    fn factory() -> Box<dyn Fn() -> sim_systems::sim::SimDeps + Send> { crate::services::services().sim_deps_factory() }

    #[test]
    fn queue_supersedes_by_kind_and_puts_gauges_first() {
        let b = bp("starters/patrol15.bp.ron");
        let mut q = Queue::default();
        q.push(Job::PrebuildSplash { rev: 1, blueprint: b.clone(), options: Default::default(), seed: 1 });
        q.push(Job::FastGauges { rev: 1, blueprint: b.clone(), cap_t: 150.0, dragging: true });
        assert_eq!(q.push(Job::FastGauges { rev: 2, blueprint: b.clone(), cap_t: 150.0, dragging: false }), Some(Kind::Fast));
        q.push(Job::Trials { rev: 2, blueprint: b.clone(), spec: BatterySpec::SHORT });
        assert_eq!(q.len(), 3);
        match q.pop_quick() {
            Some(Job::FastGauges { rev, dragging, .. }) => { assert_eq!(rev, 2); assert!(!dragging); }
            _ => panic!("gauges first"),
        }
        assert!(matches!(q.pop_quick(), Some(Job::PrebuildSplash { .. })));
        assert!(q.pop_quick().is_none(), "trials are a long job");
        assert!(q.pop_long(&[Kind::Trials]).is_none(), "one trial battery at a time");
        assert!(matches!(q.pop_long(&[]), Some(Job::Trials { .. })));
    }

    #[test]
    fn received_blueprints_queue_in_order_without_duplicates() {
        let packed = |n: u8| content::share::PackedBlueprint { hash: ContentHash([n; 32]), compressed: vec![n] };
        let mut q = Queue::default();
        q.push(Job::BuildReceived { packed: packed(1), exterior_only: false });
        q.push(Job::BuildReceived { packed: packed(2), exterior_only: true });
        q.push(Job::BuildReceived { packed: packed(1), exterior_only: false });
        assert_eq!(q.len(), 2);
        assert!(matches!(q.pop_quick(), Some(Job::BuildReceived { packed, .. }) if packed.hash.0[0] == 1));
        assert!(matches!(q.pop_quick(), Some(Job::BuildReceived { packed, .. }) if packed.hash.0[0] == 2));
    }

    #[test]
    fn cancel_drops_older_queued_jobs_and_a_newer_job_wins_over_a_requeue() {
        let b = bp("starters/patrol15.bp.ron");
        let mut q = Queue::default();
        q.push(Job::FitCheck { rev: 3, blueprint: b.clone(), brief: Arc::new(brief()), seconds: 30.0 });
        q.push(Job::Trials { rev: 5, blueprint: b.clone(), spec: BatterySpec::SHORT });
        q.drop_older(4);
        assert_eq!(q.len(), 1);
        q.requeue(Job::Trials { rev: 4, blueprint: b.clone(), spec: BatterySpec::SHORT });
        assert_eq!(q.len(), 1, "the newer queued battery wins");
        assert!(matches!(q.pop_long(&[]), Some(Job::Trials { rev: 5, .. })));
    }

    fn wait_for(w: &WorkerHandle, mut want: impl FnMut(&JobResult) -> bool, secs: u64) -> Vec<JobResult> {
        let start = Instant::now();
        let mut seen = Vec::new();
        while start.elapsed() < Duration::from_secs(secs) {
            let mut out = Vec::new();
            w.poll(&mut out);
            let hit = out.iter().any(&mut want);
            seen.extend(out);
            if hit { return seen; }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("no matching result within {secs} s");
    }

    #[test]
    fn gauges_then_notes_round_trip_and_a_long_job_does_not_block_gauges() {
        let mut w = WorkerHandle::spawn(factory());
        let b = bp("starters/patrol15.bp.ron");
        // A Fit Check on the long lane, then an edit's gauges: the gauges must not wait for it.
        w.submit(Job::FitCheck { rev: 1, blueprint: b.clone(), brief: Arc::new(brief()), seconds: 30.0 });
        w.submit(Job::FastGauges { rev: 1, blueprint: b.clone(), cap_t: 150.0, dragging: false });
        let mut all = wait_for(&w, |r| matches!(r, JobResult::FastGauges { .. }), 60);
        let design = all
            .iter()
            .find_map(|r| match r {
                JobResult::FastGauges { design, gauges, .. } => {
                    assert!(gauges.float.displacement_t > 0.0);
                    assert!(gauges.stale.iter().any(|s| s == "sight"));
                    Some(design.clone())
                }
                _ => None,
            })
            .unwrap();
        w.submit(Job::SlowGaugesAndNotes { rev: 1, design, blueprint: b.clone(), cap_t: 150.0, fit: None });
        if !all.iter().any(|r| matches!(r, JobResult::SlowGaugesAndNotes { .. })) { all.extend(wait_for(&w, |r| matches!(r, JobResult::SlowGaugesAndNotes { .. }), 60)); }
        let slow = all.iter().find_map(|r| match r { JobResult::SlowGaugesAndNotes { gauges, .. } => Some(gauges.clone()), _ => None }).unwrap();
        assert!(slow.stale.is_empty());
        assert!(slow.float.displacement_t > 0.0, "the slow strip carries the fast gauges too");
        if !all.iter().any(|r| matches!(r, JobResult::FitCheck { .. })) { wait_for(&w, |r| matches!(r, JobResult::FitCheck { .. }), 60); }
        w.shutdown();
        w.shutdown();
    }

    #[test]
    fn received_designs_go_to_their_own_queue() {
        let mut w = WorkerHandle::spawn(factory());
        let b = bp("starters/patrol15.bp.ron");
        let packed = content::share::pack(&content::blueprint::Shareable::Ship(Box::new((*b).clone())));
        let hash = packed.hash;
        w.submit(Job::BuildReceived { packed, exterior_only: false });
        w.submit(Job::BuildReceived { packed: content::share::PackedBlueprint { hash: ContentHash([7; 32]), compressed: vec![1, 2, 3] }, exterior_only: false });
        let start = Instant::now();
        let mut got = Vec::new();
        while got.len() < 2 && start.elapsed() < Duration::from_secs(60) {
            w.poll_received(&mut got);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(got.len(), 2);
        assert!(got.iter().any(|r| matches!(r, JobResult::BuiltReceived { hash: h, result: Ok(ReceivedDesign::Full { .. }) } if *h == hash)));
        assert!(got.iter().any(|r| matches!(r, JobResult::BuiltReceived { result: Err(_), .. })));
        let mut editor = Vec::new();
        w.poll(&mut editor);
        assert!(editor.is_empty(), "the editor never sees received designs");
        w.shutdown();
    }

    #[test]
    fn a_sortie_parks_long_jobs_and_a_cancelled_job_reports_nothing() {
        let mut w = WorkerHandle::spawn(factory());
        let b = bp("starters/patrol15.bp.ron");
        w.set_sortie_ticking(true);
        w.submit(Job::FitCheck { rev: 2, blueprint: b.clone(), brief: Arc::new(brief()), seconds: 30.0 });
        std::thread::sleep(Duration::from_millis(300));
        let mut out = Vec::new();
        w.poll(&mut out);
        assert!(out.is_empty(), "nothing long runs while a sortie ticks");
        // Cancel it and let the sortie end: the parked check is dropped, not run.
        w.cancel_before(3);
        std::thread::sleep(Duration::from_millis(200));
        w.set_sortie_ticking(false);
        w.submit(Job::FitCheck { rev: 3, blueprint: b.clone(), brief: Arc::new(brief()), seconds: 30.0 });
        let seen = wait_for(&w, |r| matches!(r, JobResult::FitCheck { .. }), 60);
        assert!(seen.iter().all(|r| !matches!(r, JobResult::FitCheck { rev: 2, .. })));
        w.shutdown();
    }
}
