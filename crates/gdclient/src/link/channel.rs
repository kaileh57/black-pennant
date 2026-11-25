//! Unit `gd-link-channel` (hard: lock-free hand-off between the clock thread and the main thread).
//! Three things: the triple buffer, the [`Presenter`] that every non-net link uses to turn whole snapshots
//! into sampled frames, and an in-process [`HostPort`] and [`ClientLink`] pair built from both.
//! DESIGN.md L1088 (snapshots and a command queue between the simulation and the main thread; the simulation
//! never touches a Godot type), L1449 (2 ms adapter budget: `poll` must not block or allocate per event beyond
//! the output vectors), L910-916 and L926 (interpolation and client-flown shells, done by `net::interp`).
//!
//! Stand-in status: the pair skips wire encoding and the lobby, so it does not exercise the multiplayer path
//! that L27 requires of single-player. It exists so a sortie is playable before `net`'s hard units land;
//! `link::LOCAL_BACKEND` switches local sessions to `link::net::loopback` afterwards. It depends on one `net`
//! unit only, `net-interp` (`SnapshotBuffer`, `ShellTracker`), and uses `net`'s public API as it stands.
//!
//! # The pair's timing (decisions where the contract is silent)
//! - The host end keeps a real-tick count since the sortie started on its clock. Each pass runs the owed real
//!   ticks, at most [`MAX_CATCH_UP_TICKS`]; a larger backlog is dropped and counted, as L1363 says, and the
//!   owed count is what `HostLoad::backlog_ticks` reports to the ladder.
//! - One real tick runs `pacing().effective.factor()` simulation steps scaled by `pacing().time_scale` (a
//!   fractional remainder carries to the next real tick, so 0.7x runs 7 steps in 10 real ticks). While paused
//!   it still calls `Sim::step` once per real tick, which applies queued time and alert commands and advances
//!   nothing.
//! - Snapshots go out every other real tick (30 Hz of wall time, as net's server does), so the client keeps
//!   receiving the paused tick during a pause and compression does not multiply snapshot cost.
//! - The tick p95 handed to the ladder (`HostLoad::tick_p95_ms`) reads 0 for a sortie's first 60 steps, so
//!   the world's slow first tick never steps the ladder; after that it is the p95 of the 2 s window.
//! - The host end sends a `ClientEvent::Connection` panel once a second, so the HUD's host tick rate and the
//!   connection list read the same figures offline as on a listen host.
use crate::seams::{ClientEvent, ClientLink, ClientMsg, ConnectionPanel, HostEvent, HostPort, InterpTuning, LinkInbox, LinkState, LinkStatus, NetError, PlayerLink, ServerMsg, SessionEnd, SessionPhase, ShellPose, SortieIntro, Welcome, WireEvent};
use crate::session::host::{TickTimes, MAX_CATCH_UP_TICKS, TICK_NS};
use content::catalogue::Catalogue;
use content::ids::{ContentHash, PlayerSlot, SideId};
use content::part::weapons::{BandDef, CalibreBand};
use content::share::PackedBlueprint;
use glam::DVec3;
use net::clock::Clock;
use net::delta::{DecodedFrame, ItemKey, Section};
use net::interp::{ShellTracker, SnapshotBuffer};
use sim_core::events::PhysEvent;
use sim_core::projectiles::ProjectileId;
use sim_systems::lod::HostLoad;
use sim_systems::modes::{LobbyPlayer, LobbySetup};
use sim_systems::protocol::config::BlueprintRef;
use sim_systems::protocol::event::SimEvent;
use sim_systems::protocol::{Command, CommandEnvelope, PlayerInput, Snapshot, SortieSummary, Viewer};
use sim_systems::theatre::TheatreCommand;
use sim_systems::Sim;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------------------
// Triple buffer
// ---------------------------------------------------------------------------------------------------------

/// Low two bits of the state word: the slot index in the middle (owned by neither end).
const INDEX_BITS: u8 = 0b011;
/// Set by the writer when the middle slot holds a value the reader has not taken.
const FRESH: u8 = 0b100;

/// Writer end of a triple buffer: publishing never blocks and never waits for the reader.
pub struct TripleWriter<T> {
    pub(crate) shared: Arc<TripleShared<T>>,
    /// The slot only the writer touches.
    pub(crate) back: u8,
}
/// Reader end: `take_latest` returns the newest value published since the last call, if any.
pub struct TripleReader<T> {
    pub(crate) shared: Arc<TripleShared<T>>,
    /// The slot only the reader touches.
    pub(crate) front: u8,
}
/// Three slots and an atomic state word; the owner picks the scheme (atomic index swap).
///
/// Scheme: the writer owns `back`, the reader owns `front`, and the state word holds the third index plus
/// the [`FRESH`] bit. Publishing fills `back`, then swaps it into the middle with `FRESH` set and takes the old
/// middle as the new `back`; taking swaps `front` into the middle with `FRESH` clear. The three indices stay
/// a permutation of 0..3, so each slot's mutex is only ever taken by the one end that owns the slot at the
/// time: the locks never contend and never block. They exist so the buffer needs no `unsafe`.
pub struct TripleShared<T> { pub(crate) slots: [std::sync::Mutex<Option<T>>; 3], pub(crate) state: std::sync::atomic::AtomicU8 }

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> { m.lock().unwrap_or_else(|e| e.into_inner()) }

/// A triple buffer for one writer thread and one reader thread.
pub fn triple_buffer<T: Send>() -> (TripleWriter<T>, TripleReader<T>) {
    let shared = Arc::new(TripleShared { slots: [Mutex::new(None), Mutex::new(None), Mutex::new(None)], state: AtomicU8::new(1) });
    (TripleWriter { shared: shared.clone(), back: 0 }, TripleReader { shared, front: 2 })
}
impl<T: Send> TripleWriter<T> {
    pub fn publish(&mut self, value: T) {
        // An unread older value in this slot is dropped here, on the writer's thread.
        *lock(&self.shared.slots[self.back as usize]) = Some(value);
        let old = self.shared.state.swap(self.back | FRESH, Ordering::AcqRel);
        self.back = old & INDEX_BITS;
    }
}
impl<T: Send> TripleReader<T> {
    pub fn take_latest(&mut self) -> Option<T> {
        if self.shared.state.load(Ordering::Acquire) & FRESH == 0 { return None; }
        // Only the reader clears FRESH, so the middle is still fresh when the swap lands.
        let old = self.shared.state.swap(self.front, Ordering::AcqRel);
        self.front = old & INDEX_BITS;
        lock(&self.shared.slots[self.front as usize]).take()
    }
}

// ---------------------------------------------------------------------------------------------------------
// Presenter
// ---------------------------------------------------------------------------------------------------------

/// The client-side presentation state of a link that receives whole `Snapshot`s instead of wire frames:
/// the in-process pair, `link::replay::ReplayLink` and `BpTestFeed` scripts. It is `net`'s own machinery
/// driven by hand, so these links interpolate, extrapolate and fly shells exactly as a `net::ClientSession`
/// does, and `crate::interp` and the renderer see one kind of frame.
pub struct Presenter {
    pub(crate) buffer: SnapshotBuffer,
    pub(crate) shells: ShellTracker,
    pub(crate) catalogue: Arc<Catalogue>,
    /// Sequence number given to the next pushed snapshot.
    pub(crate) next_seq: u32,
    /// Drag constant of each calibre band, in `CalibreBand::ALL` order, resolved from the catalogue once.
    pub(crate) drag_k: [f64; 4],
    /// Reused output of `ShellTracker::render_positions`.
    pub(crate) scratch: Vec<(ProjectileId, DVec3, DVec3)>,
}

/// The drag constant the simulation flies a band with: the catalogue row's committed value, 0 for a row
/// with no usable value, the fit of the spec's starting row when the catalogue has no row (the same rule as
/// `sim_core::projectiles`). Client shells must use the server's constant or they miss the 0.5 m impact gate
/// (L939).
pub(crate) fn band_drag_k(bands: &[BandDef], band: CalibreBand) -> f64 {
    match bands.iter().find(|b| b.band == band) {
        Some(b) if b.drag_k.is_finite() && b.drag_k > 0.0 => b.drag_k,
        Some(_) => 0.0,
        None => sim_core::projectiles::fit_drag_k(&BandDef::spec_default(band)),
    }
}

fn band_index(band: CalibreBand) -> usize { CalibreBand::ALL.iter().position(|b| *b == band).unwrap_or(0) }

impl Presenter {
    /// `tuning` is usually `InterpTuning::default()` (4 intervals at 30 Hz).
    pub fn new(tuning: InterpTuning, catalogue: Arc<Catalogue>) -> Presenter {
        let drag_k = CalibreBand::ALL.map(|b| band_drag_k(&catalogue.bands, b));
        Presenter { buffer: SnapshotBuffer::new(tuning), shells: ShellTracker::new(), catalogue, next_seq: 1, drag_k, scratch: Vec::new() }
    }
    /// Add a whole snapshot that arrived at `arrival` on the link's clock: wrapped as a
    /// `net::delta::DecodedFrame { seq, tick: snapshot.tick, snapshot, fresh }` with every item of the
    /// snapshot listed in `fresh` (every vessel, body, decoy and track key and all five sections), then
    /// `SnapshotBuffer::push`.
    pub fn push_snapshot(&mut self, snapshot: Snapshot, arrival: Duration) {
        let mut fresh = Vec::with_capacity(snapshot.vessels.len() + snapshot.bodies.len() + snapshot.decoys.len() + snapshot.tracks.len() + Section::ALL.len());
        fresh.extend(snapshot.vessels.iter().map(|v| ItemKey::Vessel(v.entity)));
        fresh.extend(snapshot.bodies.iter().map(|b| ItemKey::Body(b.entity)));
        fresh.extend(snapshot.decoys.iter().map(|d| ItemKey::Decoy(d.entity)));
        fresh.extend(snapshot.tracks.iter().map(|t| ItemKey::Track(t.id)));
        fresh.extend(Section::ALL.map(ItemKey::Section));
        let seq = self.next_seq;
        // Sequence 0 means "nothing" on the wire; skip it on wrap.
        self.next_seq = self.next_seq.wrapping_add(1).max(1);
        self.buffer.push(DecodedFrame { seq, tick: snapshot.tick, snapshot, fresh }, arrival);
    }
    /// Apply one event before it is surfaced: `SnapshotBuffer::on_event`; `PhysEvent::ProjectileSpawned` to
    /// `ShellTracker::spawn` with the band's drag constant from the catalogue; projectile impact and splash
    /// events to `ShellTracker::remove`.
    pub fn push_event(&mut self, event: &WireEvent) {
        self.buffer.on_event(event);
        match &event.event {
            SimEvent::Phys(PhysEvent::ProjectileSpawned(spawn)) => {
                let k = self.drag_k[band_index(spawn.band)];
                self.shells.spawn(spawn, k);
            }
            SimEvent::Phys(PhysEvent::ShellImpact { projectile, .. }) | SimEvent::Phys(PhysEvent::Splash { projectile, .. }) => {
                let _ = self.shells.remove(*projectile);
            }
            _ => {}
        }
    }
    /// Sample at `now`: `SnapshotBuffer::sample` into `inbox.frame`, `newest_tick()` into
    /// `inbox.newest_tick`, then `ShellTracker::advance_to(floor(render_tick))` and `render_positions` into
    /// `inbox.shells`. Leaves `inbox.events` and `inbox.session` alone.
    pub fn sample(&mut self, now: Duration, inbox: &mut LinkInbox) {
        let frame = self.buffer.sample(now);
        inbox.newest_tick = self.buffer.newest_tick();
        inbox.shells.clear();
        if let Some(f) = &frame {
            let render_tick = if f.render_tick.is_finite() { f.render_tick.max(0.0) } else { f.snapshot.tick as f64 };
            // Whole fixed ticks, never past the newest tick the server has sent (L926).
            let mut tick = render_tick.floor() as u64;
            if let Some(newest) = inbox.newest_tick { tick = tick.min(newest); }
            self.shells.advance_to(tick);
            self.scratch.clear();
            self.shells.render_positions(render_tick, &mut self.scratch);
            inbox.shells.extend(self.scratch.iter().map(|&(id, pos, vel)| ShellPose { id, pos, vel }));
        }
        inbox.frame = frame;
    }
    pub fn stats(&self) -> crate::seams::InterpStats { self.buffer.stats() }
    /// Forget everything (new sortie, replay seek).
    pub fn clear(&mut self) { self.buffer.clear(); self.shells.clear(); self.next_seq = 1; }
}

// ---------------------------------------------------------------------------------------------------------
// The in-process pair
// ---------------------------------------------------------------------------------------------------------

/// Client end to host end, in send order.
enum ToHost {
    Command { seq: u32, client_render_tick: u64, command: Command },
    Input(PlayerInput),
    Theatre(TheatreCommand),
}

/// What both ends read: the host's phase (for the welcome) and the blueprint store.
struct Shared {
    phase: AtomicU8,
    store: Mutex<BTreeMap<ContentHash, Vec<u8>>>,
}

fn phase_code(p: SessionPhase) -> u8 { match p { SessionPhase::Lobby => 0, SessionPhase::Theatre => 1, SessionPhase::Loading => 2, SessionPhase::Sortie => 3, SessionPhase::AfterAction => 4 } }
fn phase_from(c: u8) -> SessionPhase { match c { 1 => SessionPhase::Theatre, 2 => SessionPhase::Loading, 3 => SessionPhase::Sortie, 4 => SessionPhase::AfterAction, _ => SessionPhase::Lobby } }

/// Window of the host tick p95 (L1504).
const P95_WINDOW: Duration = Duration::from_secs(2);
/// Steps of a sortie before its tick p95 is reported: one second at 1x.
const MIN_P95_STEPS: u64 = sim_core::TICK_HZ as u64;
/// Connection panel cadence (L1033).
const PANEL_PERIOD: Duration = Duration::from_secs(1);
/// Real ticks between snapshots: 30 Hz of wall time.
const SNAPSHOT_EVERY: u64 = 2;
/// The stand-in publishes whole snapshots every [`SNAPSHOT_EVERY`] real ticks (30 Hz), every vessel included,
/// so it interpolates at that rate.
pub(crate) fn channel_interp() -> InterpTuning { crate::link::net::interp_for_vessel_hz((sim_core::TICK_HZ as u64 / SNAPSHOT_EVERY) as u32) }
/// The top of the adaptive cap range (L937); the in-process link has no cap, so it shows the largest.
const IN_PROCESS_CAP_KBPS: u16 = 768;

/// Clock bookkeeping of one running sortie on the host end.
struct SortieRun {
    /// Clock reading at `start_sortie`.
    epoch: Duration,
    /// Real ticks accounted for since `epoch` (run or dropped).
    real_ticks: u64,
    /// Fractional simulation steps carried between real ticks (time scale below 1).
    carry: f64,
    /// Real tick at or after which the next snapshot goes out.
    next_snapshot: u64,
    /// Owed real ticks dropped because one pass could not run them (L1363).
    dropped: u64,
    /// `SortieOver` already appended.
    over_reported: bool,
    /// Simulation steps run since `start_sortie` (the p95 warm-up guard).
    steps: u64,
}

/// The host end of [`pair`]. Lives on the clock thread.
struct ChannelHost {
    slot: PlayerSlot,
    side: SideId,
    mode: content::enums::GameMode,
    clock: Arc<dyn Clock>,
    pool: Arc<rayon::ThreadPool>,
    shared: Arc<Shared>,
    to_client: Sender<ClientEvent>,
    events: Sender<WireEvent>,
    snapshots: TripleWriter<Snapshot>,
    from_client: Receiver<ToHost>,
    phase: SessionPhase,
    sim: Option<Sim>,
    run: Option<SortieRun>,
    /// Commands received since the last pass, applied at the next real tick.
    pending: Vec<CommandEnvelope>,
    /// Newest input not yet handed to the Sim.
    input: Option<PlayerInput>,
    /// Added to the client's sequence numbers: a Sim restored from a checkpoint already holds that player's
    /// numbers from the session that took it (`Sim::command_seq_floor`).
    seq_base: u32,
    /// Wall time of each simulation step, stamped with the clock reading (p95 and achieved rate).
    times: TickTimes,
    last_panel: Option<Duration>,
    /// The player's `SortieEnded` summary, for `SortieClosed`.
    summary: Option<SortieSummary>,
    shut: bool,
}

/// The client end of [`pair`]. Lives on the main thread.
struct ChannelLink {
    slot: PlayerSlot,
    mode: content::enums::GameMode,
    clock: Arc<dyn Clock>,
    shared: Arc<Shared>,
    from_host: Receiver<ClientEvent>,
    events: Receiver<WireEvent>,
    snapshots: TripleReader<Snapshot>,
    to_host: Sender<ToHost>,
    presenter: Presenter,
    state: LinkState,
    welcomed: bool,
    /// Floor of the last sampled render tick, stamped on commands (L949).
    render_tick: u64,
    next_seq: u32,
    /// Between `SortieClosed` and the next `SortieLoading` nothing is sampled: a last stray snapshot would
    /// otherwise be extrapolated forever behind the after-action screen.
    between_sorties: bool,
}

/// A connected in-process pair for one local player in `slot` on `side`.
///
/// Host end (`HostPort`): `start_sortie` takes the `Sim`; `update` reads `clock`, drains the client's
/// messages, runs the driver contract of `sim_systems::sim` (`set_host_load`, `set_input`, `apply`, then
/// `step` the owed ticks, at most 4 per call, inside `pool`), drains events and forwards those whose
/// `Audience::admits(slot, side)` as `net::events::to_wire`, publishes `Sim::snapshot(Viewer::Player(slot))`
/// through a triple buffer on every other tick, and appends `HostEvent::SortieOver` once `Sim::is_over`.
/// `phase`, `set_phase`, `sim`, `sim_mut`, `end_sortie` (replay `None`), `put_blueprint`, `blueprint` and
/// `connection_panel` (one row, tick figures from its own timing) work; `send` delivers theatre messages to
/// the client end as the matching `ClientEvent`s; `lobby_setup` returns a one-player setup for the mode it was
/// built with; `send_bulk`, `kick`, `set_upload_kbps` and `drain_log` do nothing; `set_udp` returns
/// `NetError::WrongPhase`.
///
/// Client end (`ClientLink`): the first `poll` yields `ClientEvent::Welcomed` (slot, `host`-side phase);
/// on `start_sortie` a `ClientEvent::SortieLoading` with the `SortieIntro`, then `SortieRunning`; snapshots
/// and events go through a [`Presenter`] using `clock.now()` as the arrival time; `ClientMsg::Command` and
/// `Input` reach the host end stamped as `CommandEnvelope { player: slot, seq, client_render_tick, rtt_ms: 0,
/// .. }`; `Theatre` surfaces on the host end as `HostEvent::Theatre`; the other messages are ignored.
/// `status()` is `Joined` with zero link numbers and the presenter's interpolation counters; `blueprint`
/// reads the store shared with the host end.
pub fn pair(slot: PlayerSlot, side: SideId, mode: content::enums::GameMode, clock: Arc<dyn Clock>, pool: Arc<rayon::ThreadPool>, catalogue: Arc<Catalogue>) -> (Box<dyn HostPort>, Box<dyn ClientLink>) {
    let (host, link) = pair_parts(slot, side, mode, clock, pool, catalogue);
    (Box::new(host), Box::new(link))
}

/// [`pair`] with the concrete ends.
fn pair_parts(slot: PlayerSlot, side: SideId, mode: content::enums::GameMode, clock: Arc<dyn Clock>, pool: Arc<rayon::ThreadPool>, catalogue: Arc<Catalogue>) -> (ChannelHost, ChannelLink) {
    let shared = Arc::new(Shared { phase: AtomicU8::new(phase_code(SessionPhase::Lobby)), store: Mutex::new(BTreeMap::new()) });
    let (to_client, from_host) = mpsc::channel();
    let (events_tx, events_rx) = mpsc::channel();
    let (to_host, from_client) = mpsc::channel();
    let (writer, reader) = triple_buffer();
    let host = ChannelHost {
        slot, side, mode, clock: clock.clone(), pool, shared: shared.clone(), to_client, events: events_tx, snapshots: writer, from_client,
        phase: SessionPhase::Lobby, sim: None, run: None, pending: Vec::new(), input: None, seq_base: 0, times: TickTimes::new(P95_WINDOW), last_panel: None, summary: None, shut: false,
    };
    let link = ChannelLink {
        slot, mode, clock, shared, from_host, events: events_rx, snapshots: reader, to_host,
        presenter: Presenter::new(channel_interp(), catalogue), state: LinkState::Joined, welcomed: false, render_tick: 0, next_seq: 0, between_sorties: true,
    };
    (host, link)
}

impl ChannelHost {
    fn set_phase_internal(&mut self, phase: SessionPhase) {
        self.phase = phase;
        self.shared.phase.store(phase_code(phase), Ordering::Release);
    }

    fn to_client(&self, event: ClientEvent) { let _ = self.to_client.send(event); }

    /// Everything the client sent since the last pass.
    fn drain_client(&mut self, out: &mut Vec<HostEvent>) {
        loop {
            match self.from_client.try_recv() {
                Ok(ToHost::Command { seq, client_render_tick, command }) => {
                    // Commands outside a sortie have nothing to act on.
                    if self.sim.is_some() { self.pending.push(CommandEnvelope { player: self.slot, seq: self.seq_base.saturating_add(seq), client_render_tick, rtt_ms: 0, command }); }
                }
                Ok(ToHost::Input(input)) => {
                    // Newest by input tick, as the server keeps it.
                    if self.input.is_none_or(|old| input.input_tick >= old.input_tick) { self.input = Some(input); }
                }
                Ok(ToHost::Theatre(command)) => out.push(HostEvent::Theatre { slot: self.slot, command }),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    /// Route the Sim's queued events to the player and remember its end-of-sortie summary.
    fn forward_events(&mut self) {
        let Some(sim) = self.sim.as_mut() else { return };
        for e in sim.drain_events() {
            if !e.audience.admits(self.slot, self.side) { continue; }
            let wire = net::events::to_wire(&e);
            if let SimEvent::SortieEnded(summary) = &wire.event { self.summary = Some((**summary).clone()); }
            let _ = self.events.send(wire);
        }
    }

    /// The driver contract for the real ticks owed by `now`.
    fn run_sortie(&mut self, now: Duration, out: &mut Vec<HostEvent>) {
        let p95 = self.p95_ms();
        let (Some(sim), Some(run)) = (self.sim.as_mut(), self.run.as_mut()) else { return };
        let elapsed = now.saturating_sub(run.epoch);
        let due = (elapsed.as_nanos() / TICK_NS as u128).min(u64::MAX as u128) as u64;
        let owed = due.saturating_sub(run.real_ticks);
        let runnable = owed.min(MAX_CATCH_UP_TICKS as u64);
        // Past the catch-up limit the backlog is dropped, not carried (L1363); it is counted, and the ladder
        // sees how far behind this pass started.
        if owed > runnable {
            run.dropped = run.dropped.saturating_add(owed - runnable);
            run.real_ticks = run.real_ticks.saturating_add(owed - runnable);
        }
        sim.set_host_load(HostLoad { tick_p95_ms: p95, backlog_ticks: owed.saturating_sub(1).min(u32::MAX as u64) as u32 });
        if let Some(input) = self.input.take() { sim.set_input(self.slot, input); }
        if !self.pending.is_empty() { sim.apply(&self.pending); self.pending.clear(); }

        let times = &mut self.times;
        let clock = &self.clock;
        self.pool.install(|| {
            for _ in 0..runnable {
                run.real_ticks += 1;
                if sim.is_over() { continue; }
                let pacing = sim.pacing();
                if pacing.paused {
                    // Applies queued time and alert commands (a resume); the clock stands.
                    sim.step();
                    continue;
                }
                let scale = if pacing.time_scale.is_finite() { pacing.time_scale.clamp(0.5, 1.0) as f64 } else { 1.0 };
                run.carry += pacing.effective.factor() as f64 * scale;
                let steps = run.carry.floor();
                run.carry -= steps;
                for _ in 0..steps as u32 {
                    let t0 = Instant::now();
                    sim.step();
                    run.steps += 1;
                    times.record(clock.now(), t0.elapsed().as_secs_f32() * 1_000.0);
                    if sim.is_over() { break; }
                }
            }
        });
        let publish = runnable > 0 && run.real_ticks >= run.next_snapshot;
        if publish { run.next_snapshot = run.real_ticks + SNAPSHOT_EVERY; }
        let report_over = sim.is_over() && !run.over_reported;
        if report_over { run.over_reported = true; }

        self.forward_events();
        if publish {
            if let Some(sim) = self.sim.as_mut() {
                let snapshot = sim.snapshot(Viewer::Player(self.slot));
                sim.note_snapshot_items(sim_systems::sim::snapshot::snapshot_items(&snapshot));
                self.snapshots.publish(snapshot);
            }
        }
        if report_over { out.push(HostEvent::SortieOver); }
    }

    /// The tick p95 the ladder and the panel see. Not reported for a sortie's first second of steps, so the
    /// world's slow first tick is not read as overload (it would cap compression for the next 12 s). The
    /// guard counts steps since the start, once: later, however few slow steps fit the 2 s window, the p95 is
    /// what the ladder gets.
    fn p95_ms(&self) -> f32 {
        if self.run.as_ref().is_some_and(|r| r.steps < MIN_P95_STEPS) { 0.0 } else { self.times.percentile(0.95) }
    }

    /// Simulation steps completed over the last second of the clock.
    fn tick_hz(&self) -> f32 {
        let now = self.clock.now();
        self.times.count_since(now.saturating_sub(Duration::from_secs(1))) as f32
    }

    fn player_name(&self) -> String {
        self.sim.as_ref().and_then(|s| s.players().iter().find(|p| p.slot == self.slot).map(|p| p.name.clone())).unwrap_or_default()
    }
}

impl HostPort for ChannelHost {
    fn update(&mut self, events: &mut Vec<HostEvent>) -> Result<(), NetError> {
        if self.shut { return Ok(()); }
        let now = self.clock.now();
        self.drain_client(events);
        if self.phase == SessionPhase::Sortie { self.run_sortie(now, events); }
        if self.last_panel.is_none_or(|t| now.saturating_sub(t) >= PANEL_PERIOD) {
            self.last_panel = Some(now);
            let panel = self.connection_panel();
            self.to_client(ClientEvent::Connection(Box::new(panel)));
        }
        Ok(())
    }
    fn phase(&self) -> SessionPhase { self.phase }
    fn set_phase(&mut self, phase: SessionPhase) -> Result<(), NetError> {
        match phase {
            SessionPhase::Lobby | SessionPhase::Theatre | SessionPhase::AfterAction if self.sim.is_none() => { self.set_phase_internal(phase); Ok(()) }
            _ => Err(NetError::WrongPhase),
        }
    }
    fn start_sortie(&mut self, sim: Sim) -> Result<(), NetError> {
        if self.shut || self.sim.is_some() { return Err(NetError::WrongPhase); }
        self.set_phase_internal(SessionPhase::Loading);
        let config = sim.config();
        let mut own: Vec<ContentHash> = config.vessels.iter().filter(|v| v.side == self.side).filter_map(|v| match &v.blueprint {
            BlueprintRef::Inline(h) => Some(*h),
            BlueprintRef::Stock(_) => None,
        }).collect();
        own.sort();
        own.dedup();
        let tick = sim.tick().0;
        let intro = SortieIntro {
            mode: config.mode,
            seed: config.seed,
            viewer: Viewer::Player(self.slot),
            side: self.side,
            tick,
            own_side_blueprints: own,
            operation: config.operation.as_ref().map(|o| o.as_str().to_string()).unwrap_or_default(),
        };
        self.to_client(ClientEvent::SortieLoading(Box::new(intro)));
        // Every blueprint is already in the shared store: nothing streams, so the sortie runs at once.
        self.set_phase_internal(SessionPhase::Sortie);
        self.to_client(ClientEvent::SortieRunning { tick });
        self.seq_base = self.seq_base.max(sim.command_seq_floor(self.slot));
        self.pending.clear();
        self.input = None;
        self.summary = None;
        self.times = TickTimes::new(P95_WINDOW);
        self.run = Some(SortieRun { epoch: self.clock.now(), real_ticks: 0, carry: 0.0, next_snapshot: 0, dropped: 0, over_reported: false, steps: 0 });
        self.sim = Some(sim);
        Ok(())
    }
    fn sim(&self) -> Option<&Sim> { self.sim.as_ref() }
    fn sim_mut(&mut self) -> Option<&mut Sim> { self.sim.as_mut() }
    fn end_sortie(&mut self) -> Option<(Sim, Option<Vec<u8>>)> {
        // A forced end (`Sim::finish` from the driver) leaves its events queued: deliver them first.
        self.forward_events();
        let sim = self.sim.take()?;
        self.run = None;
        let summary = self.summary.take().or_else(|| sim.result().map(summary_of)).unwrap_or_default();
        self.to_client(ClientEvent::SortieClosed(Box::new(summary)));
        self.pending.clear();
        self.input = None;
        self.set_phase_internal(SessionPhase::AfterAction);
        Some((sim, None))
    }
    fn send(&mut self, to: Option<PlayerSlot>, msg: ServerMsg) {
        if to.is_some_and(|s| s != self.slot) { return; }
        let event = match msg {
            ServerMsg::Welcome(w) => ClientEvent::Welcomed(w),
            ServerMsg::Refused(r) => ClientEvent::Refused(r),
            ServerMsg::Dropped(r) => ClientEvent::Ended(SessionEnd::Dropped(r)),
            ServerMsg::Lobby(v) => ClientEvent::Lobby(v),
            ServerMsg::SortieLoading(i) => ClientEvent::SortieLoading(i),
            ServerMsg::SortieRunning { tick } => ClientEvent::SortieRunning { tick },
            ServerMsg::SortieClosed { summary } => ClientEvent::SortieClosed(summary),
            ServerMsg::Connection(p) => ClientEvent::Connection(p),
            ServerMsg::Saturation { on } => ClientEvent::Saturation { on },
            ServerMsg::Theatre(e) => ClientEvent::Theatre(e),
            ServerMsg::TheatreRejected(r) => ClientEvent::TheatreRejected(r),
            ServerMsg::Chart(c) => ClientEvent::Chart(c),
            ServerMsg::Briefing(b) => ClientEvent::Briefing(b),
            ServerMsg::AfterAction(a) => ClientEvent::AfterAction(a),
            ServerMsg::Succession(s) => ClientEvent::Succession(s),
            ServerMsg::BlueprintRefused { hash, reason } => ClientEvent::BlueprintRefused(hash, reason),
            ServerMsg::SessionEnded(end) => ClientEvent::Ended(end),
            // Bulk traffic does not exist in process.
            ServerMsg::TransferAnnounce(_) => return,
        };
        self.to_client(event);
    }
    fn send_bulk(&mut self, to: Option<PlayerSlot>, kind: crate::seams::TransferKind, bytes: Vec<u8>) {}
    fn lobby_setup(&self) -> LobbySetup {
        LobbySetup {
            mode: self.mode,
            ruleset: Default::default(),
            seed: 0,
            players: vec![LobbyPlayer { slot: self.slot, client_id: 0, name: self.player_name(), team: 0, blueprint: None, ready: true, complexity: 0.0 }],
            skirmish: None,
            weather: Default::default(),
            sea_state: Default::default(),
            time_of_day: Default::default(),
            map: None,
            at_1_0: false,
        }
    }
    fn blueprint(&mut self, hash: &ContentHash) -> Option<Vec<u8>> { lock(&self.shared.store).get(hash).cloned() }
    fn put_blueprint(&mut self, packed: PackedBlueprint) { lock(&self.shared.store).insert(packed.hash, packed.compressed); }
    fn kick(&mut self, slot: PlayerSlot, ban: bool) {}
    fn set_upload_kbps(&mut self, kbps: u32) {}
    fn set_udp(&mut self, options: Option<crate::seams::UdpHostOptions>, reason: crate::seams::DropReason) -> Result<Vec<std::net::SocketAddr>, NetError> { Err(NetError::WrongPhase) }
    fn connection_panel(&self) -> ConnectionPanel {
        let row = PlayerLink { slot: self.slot, name: self.player_name(), host: true, ping_ms: 0, loss: 0.0, frames_behind: 0, quality: Default::default(), cap_kbps: IN_PROCESS_CAP_KBPS, warning: None, held_s: None };
        let running = self.phase == SessionPhase::Sortie;
        ConnectionPanel { players: vec![row], host_tick_hz: if running { self.tick_hz() } else { 0.0 }, host_tick_p95_ms: self.p95_ms(), saturated: false }
    }
    fn drain_log(&mut self) -> String { String::new() }
    fn shutdown(&mut self) {
        if self.shut { return; }
        self.shut = true;
        self.sim = None;
        self.run = None;
        self.to_client(ClientEvent::Ended(SessionEnd::HostQuit));
    }
}

/// The summary a client may show, built from a host result when no `SortieEnded` event was seen.
fn summary_of(r: &sim_systems::protocol::SortieResult) -> SortieSummary {
    SortieSummary { outcome: r.outcome, winner: r.winner, duration_ticks: r.duration_ticks, objectives: Vec::new(), stats: r.stats.clone(), tip: None, end_reason: r.end_reason }
}

impl ClientLink for ChannelLink {
    fn poll(&mut self, now_s: f64, inbox: &mut LinkInbox) {
        if self.state == LinkState::Closed { return; }
        if !self.welcomed {
            self.welcomed = true;
            let phase = phase_from(self.shared.phase.load(Ordering::Acquire));
            inbox.session.push(ClientEvent::Welcomed(Welcome { protocol: net::WIRE_VERSION, slot: self.slot, mode: self.mode, lobby_name: String::new(), host_game_version: env!("CARGO_PKG_VERSION").to_string(), rejoined: false, phase }));
        }
        loop {
            match self.from_host.try_recv() {
                Ok(event) => {
                    match &event {
                        ClientEvent::SortieLoading(_) => { self.presenter.clear(); self.between_sorties = false; }
                        ClientEvent::SortieClosed(_) => { self.presenter.clear(); self.between_sorties = true; }
                        ClientEvent::Ended(end) => { self.state = LinkState::Ended(*end); self.presenter.clear(); }
                        _ => {}
                    }
                    inbox.session.push(event);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // The host end went away without saying so: host loss, reported once.
                    if self.state == LinkState::Joined {
                        self.state = LinkState::Ended(SessionEnd::HostLost);
                        self.presenter.clear();
                        inbox.session.push(ClientEvent::Ended(SessionEnd::HostLost));
                    }
                    break;
                }
            }
        }
        if self.state != LinkState::Joined { return; }
        let now = self.clock.now();
        // Events before the snapshot: reveals gate the poses they introduce.
        while let Ok(event) = self.events.try_recv() {
            self.presenter.push_event(&event);
            inbox.events.push(event);
        }
        let snapshot = self.snapshots.take_latest();
        if self.between_sorties { return; }
        if let Some(snapshot) = snapshot { self.presenter.push_snapshot(snapshot, now); }
        self.presenter.sample(now, inbox);
        if let Some(f) = &inbox.frame { if f.render_tick.is_finite() && f.render_tick >= 0.0 { self.render_tick = f.render_tick.floor() as u64; } }
    }
    fn send(&mut self, msg: ClientMsg) {
        if self.state != LinkState::Joined { return; }
        let out = match msg {
            ClientMsg::Command(command) => {
                self.next_seq = self.next_seq.wrapping_add(1).max(1);
                ToHost::Command { seq: self.next_seq, client_render_tick: self.render_tick, command }
            }
            ClientMsg::Input(input) => ToHost::Input(input),
            ClientMsg::Theatre(command) => ToHost::Theatre(command),
            ClientMsg::Lobby(_) | ClientMsg::UploadBlueprint(_) | ClientMsg::RequestBlueprint(_) | ClientMsg::Mapping(_) | ClientMsg::RequestReplay => return,
        };
        let _ = self.to_host.send(out);
    }
    fn status(&self) -> LinkStatus { LinkStatus { state: self.state.clone(), link: Default::default(), interp: self.presenter.stats() } }
    fn blueprint(&mut self, hash: &ContentHash) -> Option<PackedBlueprint> { lock(&self.shared.store).get(hash).map(|c| PackedBlueprint { hash: *hash, compressed: c.clone() }) }
    fn close(&mut self) {
        if self.state == LinkState::Closed { return; }
        self.state = LinkState::Closed;
        self.presenter.clear();
    }
}

#[cfg(test)]
mod tests;
