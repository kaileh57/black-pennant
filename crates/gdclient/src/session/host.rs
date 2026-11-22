//! Unit `gd-host` (hard: concurrency and timing). The host driver: the clock thread that pumps the session's
//! server end, in single-player, on a listen host, in Splash, in the tutorial and in the benchmark.
//! DESIGN.md L1084 (sim pool of exactly 3 rayon workers), L1363 (tick clock), L1500-1517 (ladder inputs),
//! L112 and L843 (60 s operation checkpoint), L1043 (replay recording), L1338 (a worker panic is reported and
//! handled as host loss).
//!
//! # Who steps the simulation
//! Not this unit. The [`HostPort`] owns the `Sim` while a sortie runs and its `update` does the whole driver
//! contract of `sim_systems::sim`: host load, inputs, commands, the owed ticks inside the sim pool, event
//! routing, 30 Hz snapshots per player and the replay (`net::ServerSession::update`; `link::channel`'s
//! stand-in does the same without the wire). This unit owns what `net` leaves to the host app
//! (`net::session` docs): the thread, the tick deadline, the 3-worker pool, building each `Sim` from
//! `SimDeps`, checkpoints, and relaying session events to the main thread, where `BpLobby` and `BpCampaign`
//! decide.
//!
//! # Driver loop (one iteration per 60 Hz deadline)
//! 1. Drain [`HostControl`] messages (start and stop sorties, sends, kicks, UDP changes).
//! 2. Wait for the next absolute deadline: `start + n x 16,666,667 ns`; sleep until 1.5 ms before it, then
//!    spin. With `SortieOptions::fixed_step` there is no deadline: each `HostControl::StepOnce` runs one
//!    pass after the main thread advanced the shared `net::clock::ManualClock` by one tick.
//! 3. `HostPort::update(&mut events)`. The port reads the same clock and runs up to 4 owed ticks per pass
//!    (`ServerOptions::max_catch_up_ticks = 4`, L1363); a larger backlog shows in `Sim::pacing()` and the
//!    connection panel, not here. An `Err` is posted as `HostNote::Failed` and treated as host loss.
//! 4. Post every `HostEvent` as [`HostNote::Session`]. On `HostEvent::SortieOver` (or `StopSortie`): take
//!    `end_sortie()`, post [`HostNote::SortieEnded`] with `Sim::result()` and the replay bytes, and idle in
//!    `SessionPhase::AfterAction` until told what is next.
//! 5. With `SortieOptions::checkpoints`, every `sim_core::time::CHECKPOINT_TICKS` of sortie time (and on
//!    `CheckpointNow`): `HostPort::sim().checkpoint()` posted as [`HostNote::Checkpoint`]. The main thread
//!    bundles it with the save (`net::coop::CoopHost::bundle`) and sends it back as `HostControl::SendBulk`.
//! 6. At 1 Hz: [`HostNote::Stats`] from `HostPort::connection_panel()` and `Sim::pacing()`, and
//!    [`HostNote::Log`] when `HostPort::drain_log()` is not empty.
//!
//! While hosting, the thread holds a `crate::platform::HostingActivity`, which asks the OS not to throttle
//! the process and is reset on stop (L1363). This unit contains no FFI itself.
use crate::seams::{DropReason, HostEvent, HostPort, ServerMsg, SessionPhase, TransferKind, UdpHostOptions};
use content::ids::PlayerSlot;
use content::save::SortieCheckpoint;
use content::share::PackedBlueprint;
use sim_systems::editor::splash::{PreparedSplash, SplashStage};
use sim_systems::protocol::{Outcome, SortieConfig, SortieResult};
use sim_systems::Sim;
use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Tick period: 60 Hz from absolute deadlines.
pub const TICK_NS: u64 = 16_666_667;
/// Owed ticks one pass may run before the backlog is reported instead (L1363); goes into
/// `net::session::ServerOptions::max_catch_up_ticks` when the port is built.
pub const MAX_CATCH_UP_TICKS: u32 = 4;
/// Operation checkpoint period in seconds (L112, L843): `sim_core::time::CHECKPOINT_TICKS` ticks.
pub const CHECKPOINT_S: u32 = 60;

/// How a sortie begins on the host. In every case the driver builds the `Sim` on the clock thread, puts the
/// config's blueprints into the port's store (`HostPort::put_blueprint`) and calls `HostPort::start_sortie`.
pub enum HostStart {
    /// `Sim::new(config, deps)`.
    New { config: Box<SortieConfig> },
    /// Resume a suspend save or a co-op host-loss checkpoint: `Sim::from_checkpoint`.
    Checkpoint { checkpoint: Box<SortieCheckpoint> },
    /// Swap in a prepared splash world (L218): `sim_systems::editor::splash::launch`; `stage` picks
    /// progressive splash.
    Splash { prepared: Arc<PreparedSplash>, stage: SplashStage },
    /// Start from the session's lobby after `HostEvent::StartRequested`: read `HostPort::lobby_setup()`,
    /// unpack each member's design from `HostPort::blueprint(hash)` with `content::share::unpack`, build the
    /// config with `sim_systems::modes::{elimination_config, escort_config, skirmish_config, sandbox_config}`
    /// for the setup's mode, then `Sim::new`. The lobby already validated every design
    /// (`HostEvent::BlueprintAccepted`).
    Lobby,
}

/// Options of one sortie on the host.
#[derive(Clone, Debug, PartialEq)]
pub struct SortieOptions {
    /// Take 60 s checkpoints (campaign and co-op only).
    pub checkpoints: bool,
    /// Benchmark and capture runs: one pass per `StepOnce` on a `ManualClock`, instead of by wall clock.
    pub fixed_step: bool,
}

/// Main thread to host thread. Each variant after the first four is one [`HostPort`] call.
pub enum HostControl {
    StartSortie { start: HostStart, options: SortieOptions },
    /// End the sortie now with this outcome (`Aborted` for quit and suspend): `Sim::finish`, then as
    /// `HostEvent::SortieOver`.
    StopSortie(Outcome),
    /// Take a checkpoint now (suspend on quit); answered by `HostNote::Checkpoint`.
    CheckpointNow,
    /// Run one pass (only with `SortieOptions::fixed_step`).
    StepOnce,
    /// `HostPort::set_phase`: the main thread moves the session between lobby, chart table and after-action.
    SetPhase(SessionPhase),
    /// `HostPort::send`: theatre events, chart, briefing, after-action, succession (`None` = everyone).
    Send { to: Option<PlayerSlot>, msg: ServerMsg },
    /// `HostPort::send_bulk`: checkpoint bundles, save copies, released replays.
    SendBulk { to: Option<PlayerSlot>, kind: TransferKind, bytes: Vec<u8> },
    /// `HostPort::put_blueprint`: a design the host app supplies itself (stock classes of a campaign sortie).
    PutBlueprint(PackedBlueprint),
    /// `HostPort::kick`.
    Kick { slot: PlayerSlot, ban: bool },
    /// `HostPort::set_upload_kbps`.
    SetUploadKbps(u32),
    /// `HostPort::set_udp`; answered by `HostNote::UdpBound`.
    SetUdp { options: Option<UdpHostOptions>, reason: DropReason },
    Shutdown,
}

/// Tick timing for the HUD, the benchmark and the ladder (L1474). `tick_p95_ms` and `tick_hz` are the
/// connection panel's host figures; the rest are the driver's own measurements of `HostPort::update`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HostStats { pub tick_p50_ms: f32, pub tick_p95_ms: f32, pub tick_p99_ms: f32, pub backlog_ticks: u32, pub late_passes: u64, pub tick_hz: f32 }

/// Host thread to main thread, polled once per frame.
pub enum HostNote {
    /// The world is built and `start_sortie` returned (splash load time ends here, L218).
    SortieLoaded { millis: f32 },
    /// The `Sim` could not be built or started; developer text for the log, the UI shows `session.failed`.
    SortieFailed(String),
    SortieEnded { result: Box<SortieResult>, replay: Option<Vec<u8>> },
    Checkpoint(Box<SortieCheckpoint>),
    /// Everything the session reports, unchanged: joins, leaves, refusals, expired holds, lobby changes,
    /// start requests, theatre commands, blueprint verdicts, mapping reports, saturation, replay requests,
    /// finished bulk transfers. `BpLobby` and `BpCampaign` read these from `FrameFeed::host_notes`.
    Session(HostEvent),
    /// Answer to `HostControl::SetUdp`: the addresses listened on, or the error as developer text.
    UdpBound(Result<Vec<std::net::SocketAddr>, String>),
    /// New connection-log lines as RON (L1039), appended to the log file by `BpLobby`.
    Log(String),
    Stats(HostStats),
    /// `HostPort::update` failed; treat as host loss.
    Failed(String),
    /// The host thread or a sim worker panicked; already reported. Treat as host loss.
    Panicked(String),
}

/// Main-thread handle to the host thread and its sim pool.
pub struct HostHandle {
    pub(crate) control: std::sync::mpsc::Sender<HostControl>,
    pub(crate) notes: std::sync::mpsc::Receiver<HostNote>,
    pub(crate) thread: Option<std::thread::JoinHandle<()>>,
    /// The sim pool, kept so Fit Check can borrow it during briefings, when no sortie ticks (L1086).
    pub(crate) pool: Arc<rayon::ThreadPool>,
}

/// Build the sim pool: `num_threads(3)`, threads named `bp-sim-N` (L1084). Built before the port, because
/// both `link::net::loopback` and `link::channel::pair` take it.
///
/// A panic inside a job spawned on the pool is already reported by the crash-report hook (L1338); the pool's
/// panic handler keeps it from aborting the process. Panics inside `install` (every `Sim::step`) travel back
/// to the clock thread, which contains them.
pub fn sim_pool() -> Arc<rayon::ThreadPool> {
    let build = |n: usize| rayon::ThreadPoolBuilder::new().num_threads(n).thread_name(|i| format!("bp-sim-{i}")).panic_handler(|_| {}).build();
    match build(SIM_WORKERS) {
        Ok(pool) => Arc::new(pool),
        // The OS refused three threads: one keeps the game running, more slowly.
        Err(_) => Arc::new(build(1).expect("the operating system refused to start a simulation thread")),
    }
}

impl HostHandle {
    /// Start the clock thread (named `bp-clock`) around `port`. Nothing ticks until `StartSortie`; the
    /// thread still pumps `HostPort::update` at the tick rate so the lobby and theatre phases are served.
    /// `deps` builds fresh `SimDeps` on the host thread.
    pub fn spawn(port: Box<dyn HostPort>, pool: Arc<rayon::ThreadPool>, deps: Box<dyn Fn() -> sim_systems::sim::SimDeps + Send>) -> HostHandle {
        let (control_tx, control_rx) = mpsc::channel();
        let (notes_tx, notes_rx) = mpsc::channel();
        let failure = notes_tx.clone();
        let driver_pool = pool.clone();
        let spawned = std::thread::Builder::new().name("bp-clock".into()).spawn(move || {
            Driver::new(port, driver_pool, deps, control_rx, notes_tx).run();
        });
        let thread = match spawned {
            Ok(handle) => Some(handle),
            Err(e) => {
                let _ = failure.send(HostNote::Failed(format!("the clock thread could not start: {e}")));
                None
            }
        };
        HostHandle { control: control_tx, notes: notes_rx, thread, pool }
    }
    pub fn send(&self, control: HostControl) { let _ = self.control.send(control); }
    /// Take pending notes without blocking.
    pub fn poll(&self, out: &mut Vec<HostNote>) {
        while let Ok(note) = self.notes.try_recv() { out.push(note); }
    }
    pub fn pool(&self) -> &Arc<rayon::ThreadPool> { &self.pool }
    /// Stop the sortie, `HostPort::shutdown`, join the thread and reset OS throttling hints. Blocks up to 2 s.
    ///
    /// A thread still busy after 2 s (a world build that cannot be interrupted) is left to finish on its own:
    /// it stops at its next message check, and the process can exit meanwhile.
    pub fn shutdown(&mut self) {
        let Some(thread) = self.thread.take() else { return };
        let _ = self.control.send(HostControl::StopSortie(Outcome::Aborted));
        let _ = self.control.send(HostControl::Shutdown);
        let deadline = Instant::now() + SHUTDOWN_WAIT;
        while !thread.is_finished() && Instant::now() < deadline { std::thread::sleep(Duration::from_millis(2)); }
        if thread.is_finished() { let _ = thread.join(); }
    }
}

impl Drop for HostHandle {
    /// A handle dropped without `shutdown` still stops its thread.
    fn drop(&mut self) { self.shutdown(); }
}

// ---------------------------------------------------------------------------------------------------------
// The clock thread
// ---------------------------------------------------------------------------------------------------------

/// Exactly three simulation workers (L1084).
const SIM_WORKERS: usize = 3;
/// Sleep until this long before a deadline, then spin (L1363).
const SPIN_BEFORE: Duration = Duration::from_micros(1_500);
/// A pass that starts later than this after its deadline counts as late (the L1363 nightly gate).
const LATE_AFTER: Duration = Duration::from_millis(2);
/// How long `HostHandle::shutdown` waits for the thread.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);
/// Window of the driver's own update-time percentiles.
const STATS_WINDOW: Duration = Duration::from_secs(2);
/// Cadence of `HostNote::Stats` and `HostNote::Log`.
const STATS_PERIOD: Duration = Duration::from_secs(1);
/// Fixed-step mode waits this long for the next control message before checking again.
const FIXED_STEP_POLL: Duration = Duration::from_millis(50);
const TICK: Duration = Duration::from_nanos(TICK_NS);

/// Timed samples over a sliding window: per-step tick times on the host, per-pass update times in the
/// driver. Percentiles are nearest-rank.
#[derive(Clone, Debug)]
pub(crate) struct TickTimes {
    window: Duration,
    /// (time stamp, milliseconds), oldest first.
    samples: VecDeque<(Duration, f32)>,
}

/// Upper bound on kept samples whatever the window: 4x compression at 60 Hz for 2 s is 480.
const MAX_SAMPLES: usize = 4_096;

impl TickTimes {
    pub(crate) fn new(window: Duration) -> Self { Self { window, samples: VecDeque::new() } }
    /// Add one sample taken at `at`; samples older than the window before `at` are dropped.
    pub(crate) fn record(&mut self, at: Duration, ms: f32) {
        if !ms.is_finite() { return; }
        self.samples.push_back((at, ms.max(0.0)));
        while let Some(&(t, _)) = self.samples.front() {
            if at.saturating_sub(t) > self.window || self.samples.len() > MAX_SAMPLES { self.samples.pop_front(); } else { break; }
        }
    }
    /// The `p` quantile (0..=1) of the kept samples; 0 with none.
    pub(crate) fn percentile(&self, p: f64) -> f32 {
        if self.samples.is_empty() { return 0.0; }
        let mut v: Vec<f32> = self.samples.iter().map(|s| s.1).collect();
        v.sort_by(|a, b| a.total_cmp(b));
        let rank = (p.clamp(0.0, 1.0) * v.len() as f64).ceil() as usize;
        v[rank.clamp(1, v.len()) - 1]
    }
    /// Samples stamped at or after `since`.
    pub(crate) fn count_since(&self, since: Duration) -> usize { self.samples.iter().rev().take_while(|s| s.0 >= since).count() }
    pub(crate) fn len(&self) -> usize { self.samples.len() }
}

/// The running sortie as the driver sees it.
struct Running {
    options: SortieOptions,
    /// Simulation tick of the next 60 s checkpoint.
    next_checkpoint: u64,
}

/// What the clock thread owns.
struct Driver {
    port: Option<Box<dyn HostPort>>,
    pool: Arc<rayon::ThreadPool>,
    deps: Box<dyn Fn() -> sim_systems::sim::SimDeps + Send>,
    control: Receiver<HostControl>,
    notes: Sender<HostNote>,
    sortie: Option<Running>,
    events: Vec<HostEvent>,
    /// Wall time of each `HostPort::update`, stamped since `started`.
    update_times: TickTimes,
    started: Instant,
    late_passes: u64,
    /// Whole tick periods the last pass started behind its deadline.
    backlog: u32,
    last_stats: Option<Instant>,
    /// `HostPort::update` failed: the port is not pumped again (host loss is the main thread's call).
    failed: bool,
    /// Asks the OS not to throttle the process while this thread runs (L1363).
    _activity: crate::platform::HostingActivity,
}

/// Text of a caught panic payload.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() { return (*s).to_string(); }
    if let Some(s) = payload.downcast_ref::<String>() { return s.clone(); }
    "panic with a non-text payload".to_string()
}

/// Sleep to `SPIN_BEFORE` ahead of `deadline`, then spin to it (L1363). Without `spin` it only sleeps:
/// outside a sortie nothing needs sub-millisecond wakes.
fn wait_until(deadline: Instant, spin: bool) {
    loop {
        let now = Instant::now();
        if now >= deadline { return; }
        let left = deadline - now;
        if !spin { std::thread::sleep(left); return; }
        if left > SPIN_BEFORE { std::thread::sleep(left - SPIN_BEFORE); } else { std::hint::spin_loop(); }
    }
}

impl Driver {
    fn new(port: Box<dyn HostPort>, pool: Arc<rayon::ThreadPool>, deps: Box<dyn Fn() -> sim_systems::sim::SimDeps + Send>, control: Receiver<HostControl>, notes: Sender<HostNote>) -> Driver {
        Driver {
            port: Some(port), pool, deps, control, notes, sortie: None, events: Vec::new(), update_times: TickTimes::new(STATS_WINDOW),
            started: Instant::now(), late_passes: 0, backlog: 0, last_stats: None, failed: false, _activity: crate::platform::HostingActivity::begin(),
        }
    }

    fn post(&self, note: HostNote) { let _ = self.notes.send(note); }

    fn fixed_step(&self) -> bool { self.sortie.as_ref().is_some_and(|s| s.options.fixed_step) }

    /// The driver loop: one iteration per 60 Hz deadline (module docs). Returns when told to shut down,
    /// when the main thread's handle is gone, or after a panic.
    fn run(mut self) {
        let epoch = Instant::now();
        let mut n: u64 = 0;
        'run: loop {
            // 1. Control messages.
            loop {
                match self.control.try_recv() {
                    Ok(msg) => if !self.contained(|d| d.handle(msg)) { break 'run; },
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break 'run,
                }
            }
            if self.fixed_step() {
                // 2'. No deadline: the main thread drives each pass with `StepOnce`.
                match self.control.recv_timeout(FIXED_STEP_POLL) {
                    Ok(msg) => if !self.contained(|d| d.handle(msg)) { break 'run; },
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break 'run,
                }
                // Keep the schedule current for when the fixed-step sortie ends.
                n = (epoch.elapsed().as_nanos() / TICK_NS as u128) as u64;
                continue;
            }
            // 2. The next absolute deadline: start + n x 16,666,667 ns.
            n += 1;
            let mut deadline = epoch + Duration::from_nanos(n.saturating_mul(TICK_NS));
            let now = Instant::now();
            self.backlog = 0;
            if now > deadline + TICK {
                // Deadlines missed while a pass ran long are skipped, not replayed: the port runs the owed
                // ticks from its own clock (at most 4 per pass) and reports the rest.
                let behind = ((now - deadline).as_nanos() / TICK_NS as u128) as u64;
                self.backlog = behind.min(u32::MAX as u64) as u32;
                n += behind;
                deadline = epoch + Duration::from_nanos(n.saturating_mul(TICK_NS));
            }
            wait_until(deadline, self.sortie.is_some());
            if self.sortie.is_some() && Instant::now().saturating_duration_since(deadline) > LATE_AFTER { self.late_passes += 1; }
            // 3 to 6. One pass.
            if !self.contained(|d| d.pass()) { break 'run; }
        }
        self.finish();
    }

    /// Run `f` with panics contained: a panic is posted as `HostNote::Panicked` (the crash-report hook has
    /// already reported it) and the port is never used again. Returns false when the loop must stop.
    fn contained(&mut self, f: impl FnOnce(&mut Driver) -> bool) -> bool {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self))) {
            Ok(go_on) => go_on,
            Err(payload) => {
                self.post(HostNote::Panicked(panic_text(payload.as_ref())));
                self.sortie = None;
                // Drop the port and whatever Sim it held; a second panic in its drop changes nothing.
                if let Some(port) = self.port.take() { let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(port))); }
                false
            }
        }
    }

    /// One pass: `HostPort::update`, relay its events, checkpoints, 1 Hz statistics. Returns false to stop.
    fn pass(&mut self) -> bool {
        if self.failed { return true; }
        let Some(port) = self.port.as_mut() else { return false };
        let mut events = std::mem::take(&mut self.events);
        events.clear();
        let t0 = Instant::now();
        let result = port.update(&mut events);
        let at = self.started.elapsed();
        self.update_times.record(at, t0.elapsed().as_secs_f32() * 1_000.0);
        if let Err(e) = result {
            // Host loss: say so once and stop pumping the port; the main thread tears the session down.
            self.failed = true;
            self.post(HostNote::Failed(e.to_string()));
            self.events = events;
            return true;
        }
        for event in events.drain(..) {
            match event {
                HostEvent::SortieOver => self.end_sortie(),
                other => self.post(HostNote::Session(other)),
            }
        }
        self.events = events;
        self.checkpoint_if_due();
        self.stats_if_due();
        true
    }

    /// Step 5: a checkpoint every `CHECKPOINT_TICKS` of sortie time when the sortie asks for them.
    fn checkpoint_if_due(&mut self) {
        let Some(running) = self.sortie.as_mut() else { return };
        if !running.options.checkpoints { return; }
        let Some(sim) = self.port.as_ref().and_then(|p| p.sim()) else { return };
        let tick = sim.tick().0;
        if tick < running.next_checkpoint { return; }
        // Compression can step past a boundary; the next one stays on the 60 s grid.
        while running.next_checkpoint <= tick { running.next_checkpoint = running.next_checkpoint.saturating_add(sim_core::time::CHECKPOINT_TICKS); }
        let checkpoint = sim.checkpoint();
        self.post(HostNote::Checkpoint(Box::new(checkpoint)));
    }

    /// Step 6: statistics and new connection-log lines at 1 Hz.
    fn stats_if_due(&mut self) {
        let now = Instant::now();
        if self.last_stats.is_some_and(|t| now.duration_since(t) < STATS_PERIOD) { return; }
        self.last_stats = Some(now);
        let Some(port) = self.port.as_mut() else { return };
        let panel = port.connection_panel();
        let pacing_ms = port.sim().map(|s| s.pacing().host_tick_ms).filter(|m| m.is_finite()).unwrap_or(0.0);
        let stats = HostStats {
            tick_p50_ms: self.update_times.percentile(0.50),
            tick_p95_ms: if panel.host_tick_p95_ms.is_finite() && panel.host_tick_p95_ms > 0.0 { panel.host_tick_p95_ms } else { pacing_ms },
            tick_p99_ms: self.update_times.percentile(0.99),
            backlog_ticks: self.backlog,
            late_passes: self.late_passes,
            tick_hz: if panel.host_tick_hz.is_finite() { panel.host_tick_hz } else { 0.0 },
        };
        let log = port.drain_log();
        self.post(HostNote::Stats(stats));
        if !log.is_empty() { self.post(HostNote::Log(log)); }
    }

    /// One control message. Returns false on `Shutdown`.
    fn handle(&mut self, msg: HostControl) -> bool {
        match msg {
            HostControl::StartSortie { start, options } => self.start_sortie(start, options),
            HostControl::StopSortie(outcome) => {
                if let Some(sim) = self.port.as_mut().and_then(|p| p.sim_mut()) {
                    // The first outcome stands when the Sim already decided.
                    let _ = sim.finish(outcome);
                    self.end_sortie();
                }
            }
            HostControl::CheckpointNow => {
                if let Some(sim) = self.port.as_ref().and_then(|p| p.sim()) {
                    let checkpoint = sim.checkpoint();
                    self.post(HostNote::Checkpoint(Box::new(checkpoint)));
                }
            }
            HostControl::StepOnce => if self.fixed_step() { return self.pass(); },
            HostControl::SetPhase(phase) => {
                // Loading and Sortie are entered by `start_sortie`; asking for them is refused and ignored.
                if let Some(port) = self.port.as_mut() { let _ = port.set_phase(phase); }
            }
            HostControl::Send { to, msg } => if let Some(port) = self.port.as_mut() { port.send(to, msg); },
            HostControl::SendBulk { to, kind, bytes } => if let Some(port) = self.port.as_mut() { port.send_bulk(to, kind, bytes); },
            HostControl::PutBlueprint(packed) => if let Some(port) = self.port.as_mut() { port.put_blueprint(packed); },
            HostControl::Kick { slot, ban } => if let Some(port) = self.port.as_mut() { port.kick(slot, ban); },
            HostControl::SetUploadKbps(kbps) => if let Some(port) = self.port.as_mut() { port.set_upload_kbps(kbps); },
            HostControl::SetUdp { options, reason } => {
                let result = match self.port.as_mut() { Some(port) => port.set_udp(options, reason).map_err(|e| e.to_string()), None => Err("no session".to_string()) };
                self.post(HostNote::UdpBound(result));
            }
            HostControl::Shutdown => return false,
        }
        true
    }

    /// Build the Sim on this thread, store its blueprints, hand it to the port.
    fn start_sortie(&mut self, start: HostStart, options: SortieOptions) {
        if self.failed || self.port.is_none() { self.post(HostNote::SortieFailed("the session has ended".into())); return; }
        // One sortie at a time: a running one ends as aborted first.
        if self.port.as_ref().is_some_and(|p| p.sim().is_some()) {
            if let Some(sim) = self.port.as_mut().and_then(|p| p.sim_mut()) { let _ = sim.finish(Outcome::Aborted); }
            self.end_sortie();
        }
        let t0 = Instant::now();
        let built = self.build_sim(start);
        let Some(port) = self.port.as_mut() else { return };
        match built {
            Ok(sim) => {
                for bp in &sim.config().blueprints {
                    port.put_blueprint(content::share::pack(&content::blueprint::Shareable::Ship(Box::new(bp.clone()))));
                }
                let start_tick = sim.tick().0;
                match port.start_sortie(sim) {
                    Ok(()) => {
                        self.sortie = Some(Running { options, next_checkpoint: start_tick.saturating_add(sim_core::time::CHECKPOINT_TICKS) });
                        self.post(HostNote::SortieLoaded { millis: t0.elapsed().as_secs_f32() * 1_000.0 });
                    }
                    Err(e) => self.post(HostNote::SortieFailed(format!("the session refused the sortie: {e}"))),
                }
            }
            Err(e) => self.post(HostNote::SortieFailed(e)),
        }
    }

    fn build_sim(&mut self, start: HostStart) -> Result<Sim, String> {
        let deps = (self.deps)();
        match start {
            HostStart::New { config } => Sim::new(*config, deps).map_err(|e| e.to_string()),
            HostStart::Checkpoint { checkpoint } => Sim::from_checkpoint(&checkpoint, deps).map_err(|e| e.to_string()),
            HostStart::Splash { prepared, stage } => sim_systems::editor::splash::launch(&prepared, deps, stage).map_err(|e| e.to_string()),
            HostStart::Lobby => {
                let port = self.port.as_mut().ok_or_else(|| "no session".to_string())?;
                let setup = port.lobby_setup();
                let mut blueprints: Vec<content::blueprint::Blueprint> = Vec::new();
                let mut seen = std::collections::BTreeSet::new();
                for hash in setup.players.iter().filter_map(|p| p.blueprint) {
                    if !seen.insert(hash) { continue; }
                    let Some(compressed) = port.blueprint(&hash) else { continue };
                    // The lobby validated every design (`HostEvent::BlueprintAccepted`); one that no longer
                    // unpacks leaves its player a spectator, as the config builders do for a missing design.
                    if let Ok(content::blueprint::Shareable::Ship(bp)) = content::share::unpack(&PackedBlueprint { hash, compressed }) { blueprints.push(*bp); }
                }
                use content::enums::GameMode;
                let config = match setup.mode {
                    GameMode::PvpElimination => sim_systems::modes::elimination_config(&setup, &blueprints, &deps.campaign),
                    GameMode::PvpEscort => sim_systems::modes::escort_config(&setup, &blueprints, &deps.campaign),
                    GameMode::Skirmish => sim_systems::modes::skirmish_config(&setup, &blueprints, &deps.campaign, &deps.catalogue),
                    GameMode::Sandbox => sim_systems::modes::sandbox_config(&setup, &blueprints, &deps.campaign),
                    other => return Err(format!("a {other:?} sortie does not start from a lobby")),
                };
                Sim::new(config, deps).map_err(|e| e.to_string())
            }
        }
    }

    /// Step 4: take the Sim back, post its result and the replay, idle in `AfterAction`.
    fn end_sortie(&mut self) {
        self.sortie = None;
        let Some(port) = self.port.as_mut() else { return };
        let Some((mut sim, replay)) = port.end_sortie() else { return };
        let result = match sim.result() { Some(r) => r.clone(), None => sim.finish(Outcome::Aborted).clone() };
        self.post(HostNote::SortieEnded { result: Box::new(result), replay });
        // The world is dropped here, on the clock thread, not on the main thread.
        drop(sim);
    }

    /// Leaving the loop: stop the sortie, shut the session, release the throttling opt-out.
    fn finish(&mut self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(sim) = self.port.as_mut().and_then(|p| p.sim_mut()) { let _ = sim.finish(Outcome::Aborted); }
            if self.port.as_ref().is_some_and(|p| p.sim().is_some()) { self.end_sortie(); }
            if let Some(port) = self.port.as_mut() { port.shutdown(); }
        }));
        let _ = &self.pool;
    }
}

#[cfg(test)]
mod tests;
