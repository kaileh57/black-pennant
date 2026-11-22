//! Unit `gd-session`. `BpSession` (autoload `Session`): the session and sortie driver on the main thread.
//! DESIGN.md L104-116 (views, time rules, suspend), L899 (one program for single-player, host and client),
//! L1043-1050 (replays, kill cam), L1500-1517 (ladder shown in HUD), L1033-1039 (connection quality).
//! Sibling unit: `host.rs` = `gd-host`.
//!
//! One session at a time: offline sortie (campaign, skirmish, sandbox), Splash, tutorial basin, hosted or
//! joined multiplayer sortie, replay playback, benchmark run. `BpSession` owns none of the game rules; it
//! starts and stops the host thread, polls the link once per frame, fills `Services::feed` and
//! `Services::view`, and turns session-level events into signals.
//!
//! # How a session is wired
//! `BpSession` builds the [`Connection`]. For anything this process simulates: `host::sim_pool()`, then a
//! server and client end from `link::net::loopback` (or `link::channel::pair` while
//! `link::LOCAL_BACKEND` says so), then `host::HostHandle::spawn` moves the server end to the clock thread.
//! Joining (`BpLobby`) builds only `link::net::join_udp`; replays only `link::replay::ReplayLink`. Each frame
//! `process` clears the inbox, calls `ClientLink::poll` once, moves the result into `Services::feed`, and
//! runs `interp::ViewBuilder::build` on the sampled frame into `Services::view`.
//!
//! # How a sortie starts
//! Another facade leaves a [`PendingSortie`] in `Services::handoff` (`Campaign.launch_sortie`,
//! `Editor.splash`, `Lobby.start`, `Tutorial.start`, `Bench.start`), or GDScript calls `start_skirmish`,
//! `start_sandbox` or `replay_open` here. On the next frame the session sends `HostControl::StartSortie` and
//! moves to `Bp.SESSION_LOADING`; on a client the same state is entered by `ClientEvent::SortieLoading`,
//! whose [`SortieIntro`] is kept in the feed. It moves to `Bp.SESSION_IN_SORTIE` when the first sampled
//! frame arrives (`LinkInbox::frame` is `Some`), then `Bp.SESSION_ENDED` on `SimEvent::SortieEnded` or
//! `ClientEvent::SortieClosed`. `leave()` returns to `Bp.SESSION_IDLE`. The finished result is left in
//! `Services::handoff` for the facade that started it.
//!
//! # What `net` reports and where it goes
//! | `ClientEvent` | Handled by |
//! | `Welcomed`, `Refused`, `Ended` | here: `Connection::welcome`, state, `failed`; `BpLobby` raises `joined`, `join_failed`, `left` |
//! | `Lobby` | `BpLobby` (the host's own lobby screen reads the same `LobbyView` as every member's) |
//! | `SortieLoading`, `SortieRunning`, `SortieClosed` | here |
//! | `Connection`, `Saturation` | here: `FrameFeed::panel`, `connection_changed`; `BpLobby` for the host panel |
//! | `Theatre`, `TheatreRejected`, `Chart`, `Briefing`, `AfterAction` | `BpCampaign` on a co-op member |
//! | `Succession`, `Checkpoint`, `HostSilent` | here: `Connection::coop` (`net::coop::CoopMember`), `host_lost` |
//! | `BlueprintReady` | here: `ClientLink::blueprint` then `worker::Job::BuildReceived` |
//! | `BlueprintRefused` | `BpLobby` |
//! | `BulkReceived` | `BpCampaign` (save copies), here (a released replay goes to `user://replays/`) |
//! | `HostEvent` (host only, from `HostNote::Session`) | Handled by |
//! | `PlayerJoined`, `PlayerLeft`, `HoldExpired`, `MappingReported` | here: `Coop::Host` (`member_joined`, `member_left`, `on_mapping`; a changed `take_succession()` goes out as `HostControl::Send` of `ServerMsg::Succession`) |
//! | `JoinRefused`, `LobbyChanged`, `BlueprintAccepted`, `BlueprintRejected`, `Saturation` | `BpLobby` |
//! | `StartRequested` | `BpLobby`: a `PendingSortie` with `HostStart::Lobby` |
//! | `Theatre` | `BpCampaign` |
//! | `SortieOver` | the clock thread itself; the main thread sees `HostNote::SortieEnded` |
//! | `ReplayRequested` | here: `net::replay::release_pvp` on the finished replay, then `HostControl::SendBulk` with `TransferKind::Replay` |
//! | `BulkReceived` | `BpCampaign` (a member's save copy) |
//! Simulation events (`SimEvent::PlayerJoined`, `PlayerDropped`, `PacingChanged`, `LadderStep`,
//! `CommandRejected`, `SortieStarted`, `SortieEnded`) arrive in `FrameFeed::events` and raise the signals
//! below.
//!
//! The sortie map is not sent: a client takes it from `Services::campaign()` by `SortieIntro::operation`,
//! or, when that is empty, from `sim_systems::modes::default_map(intro.mode, intro.seed, members)` with the
//! member count of the last `LobbyView`.
//!
//! # Decisions where the contract is silent
//! - Local sessions run on one sim pool that `BpSession` creates on first use and keeps for the life of the
//!   process (exactly 3 workers, L1084); every host thread and every design build borrows it.
//! - A sortie this session builds itself (`start_skirmish`, `start_sandbox`) goes through the same
//!   `PendingSortie` hand-off as the other facades' sorties, so the next frame starts every kind the same way.
//! - Loading progress is 5% on the request, 60% more when the world is built (`HostNote::SortieLoaded`, or
//!   `ClientEvent::SortieRunning` on a client), the remaining 35% spread over the design builds the renderer
//!   needs, and 100% with the first sampled frame.
//! - Quitting a campaign sortie asks the clock thread for a checkpoint of the moment (`CheckpointNow`) and
//!   waits up to 1.5 s for it, so the suspend slot loses nothing; if it does not come, the latest 60 s
//!   checkpoint is used (L112).
//! - The 30 Hz input is sent through the whole sortie, not only while a role is held: the server reads the
//!   axes only for held roles, and the camera position always feeds relevance (`PlayerInput::camera_pos`).
//! - Signals are queued while the frame runs and emitted once no `Services` borrow is held, because the
//!   handlers call straight back into `Session`.
pub mod host;
mod check;
mod designs;
mod killcam;
mod setup;
#[cfg(test)]
pub(crate) mod testkit;

use crate::enums::{GdEnum, NetRole, SessionKind, SessionState};
use crate::interp::ViewBuilder;
use crate::link::replay::{ReplayControl, ReplayLink};
use crate::seams::{ClientEvent, ClientLink, ClientMsg, ConnectionPanel, HostEvent, InterpTuning, LinkInbox, LinkState, LinkStatus, ServerMsg, SortieIntro, TransferKind, Welcome, WireEvent};
use crate::services::{services, Services};
use content::enums::{GameMode, TimeOfDay};
use content::ids::{ContentHash, PlayerSlot, SideId};
use content::ruleset::Ruleset;
use content::weather::WeatherState;
use godot::classes::{FileAccess, ProjectSettings, Time};
use godot::prelude::*;
use sim_systems::protocol::config::{BlueprintRef, SortieConfig};
use sim_systems::protocol::event::SimEvent;
use sim_systems::protocol::{Command, Msg, MsgArg, Pacing, PlayerInput, Snapshot, SortieResult, SortieSummary, TimeRate, Viewer};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// Co-op campaign continuity state (L842-843, L1005), decided by `net::coop`; neither owns a session.
pub enum Coop {
    /// On the host: join order, members' mapping reports, the succession list, checkpoint bundling.
    Host(net::coop::CoopHost),
    /// On a member: the newest checkpoint and succession list, and the re-host decision
    /// (`CoopMember::rehost_plan`) after `ClientEvent::HostSilent`.
    Member(net::coop::CoopMember),
}

/// The live link and, when this process runs the simulation, the host thread.
pub struct Connection {
    pub link: Box<dyn ClientLink>,
    pub host: Option<host::HostHandle>,
    /// Hosting state beyond the session (lobby secret, port mapping, join code); `None` offline and on
    /// clients.
    pub hosting: Option<crate::link::net::HostingHandle>,
    /// What the server told us on admission (`net::msg::Welcome`: slot, mode, lobby name, phase).
    pub welcome: Option<Welcome>,
    /// The running or loading sortie's intro (viewer, side, seed, start tick, operation key).
    pub intro: Option<SortieIntro>,
    pub coop: Option<Coop>,
    pub kind: SessionKind,
    pub net: NetRole,
}

/// A sortie another facade wants started on this machine's host thread.
pub struct PendingSortie {
    pub kind: SessionKind,
    pub start: host::HostStart,
    pub options: host::SortieOptions,
}

/// What a finished sortie leaves for the facade that started it.
pub struct FinishedSortie {
    pub kind: SessionKind,
    pub summary: SortieSummary,
    /// Host only: the full result the theatre consumes.
    pub result: Option<Box<SortieResult>>,
    /// Path of the replay written to `user://replays/`, if one was recorded.
    pub replay_path: Option<String>,
    /// Latest 60 s checkpoint, for the suspend slot when the player quits mid-sortie (L112).
    pub checkpoint: Option<Box<content::save::SortieCheckpoint>>,
}

/// Slots other facades fill and empty.
#[derive(Default)]
pub struct Handoff {
    pub sortie: Option<PendingSortie>,
    pub finished: Option<FinishedSortie>,
}

/// Everything that arrived this frame; valid from `BpSession::process` until the next frame.
#[derive(Default)]
pub struct FrameFeed {
    pub frame: u64,
    /// Seconds since the application started, and this frame's length.
    pub now_s: f64,
    pub delta_s: f64,
    pub state: SessionState,
    pub kind: SessionKind,
    pub net: NetRole,
    pub welcome: Option<Welcome>,
    pub intro: Option<SortieIntro>,
    /// This frame's sampled snapshot (`LinkInbox::frame`): poses at the playback tick, slow sections as
    /// the link sampled them. Replaced every frame of a sortie; `None` outside one. This is "the newest
    /// snapshot" the HUD and plot docs speak of.
    pub snapshot: Option<Arc<Snapshot>>,
    /// Increments when the newest received tick changes (`LinkInbox::newest_tick`), about 30 Hz. HUD lists
    /// rebuild on this, not on every frame.
    pub snapshot_rev: u64,
    /// Simulation events that arrived this frame, in order. Every consumer reads them here; nobody removes
    /// them.
    pub events: Vec<WireEvent>,
    /// Session events that arrived this frame, in order (`LinkInbox::session`).
    pub session: Vec<ClientEvent>,
    /// The newest connection panel (1 Hz and on change, L1033); kept between arrivals.
    pub panel: Option<Arc<ConnectionPanel>>,
    /// Host-only notes of this frame (results, checkpoints, session events from the clock thread).
    pub host_notes: Vec<host::HostNote>,
    pub status: LinkStatus,
}

/// The session and sortie driver. Autoload `Session`.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct BpSession {
    pub(crate) state: SessionState,
    pub(crate) view_builder: ViewBuilder,
    pub(crate) inbox: crate::seams::LinkInbox,
    pub(crate) last_summary: Option<SortieSummary>,
    pub(crate) replay: Option<crate::link::replay::ReplayControl>,
    pub(crate) input_accum_s: f64,
    /// Everything else the driver keeps between frames.
    pub(crate) run: SessionRun,
    base: Base<Node>,
}

/// Inputs per second (L1422).
const INPUT_HZ: f64 = sim_core::time::INPUT_HZ as f64;
/// How long `leave` waits for the clock thread's checkpoint and result when quitting a campaign sortie.
const SUSPEND_WAIT: Duration = Duration::from_millis(1_500);
/// A replay position change smaller than this is not announced (one second of sortie time).
const REPLAY_ANNOUNCE_TICKS: u64 = sim_core::TICK_HZ as u64;
/// Folder of the replay store and of the blueprint cache (L943, L1047).
const REPLAY_DIR: &str = "user://replays";
const CACHE_DIR: &str = "user://blueprint_cache";

/// Signals raised while a frame runs, emitted after every borrow is dropped.
enum Sig {
    State(SessionState),
    Loading(f64),
    Started,
    Ended(SortieSummary),
    Pacing,
    Ladder { rung: u8, sub_step: u8, up: bool },
    Joined { player: PlayerSlot, name: String },
    Dropped { player: PlayerSlot, officers: bool },
    Rejected(Msg),
    Connection,
    HostLost(bool),
    KillCam { victim: i64, killer: i64, from_s: f64, to_s: f64 },
    ReplayPosition(u64),
    Failed(Msg),
}

/// A `ClientLink` over a replay link the session also reads (position, viewers, controls).
struct SharedReplay(Rc<RefCell<ReplayLink>>);
impl ClientLink for SharedReplay {
    fn poll(&mut self, now_s: f64, inbox: &mut LinkInbox) { self.0.borrow_mut().poll(now_s, inbox) }
    fn send(&mut self, msg: ClientMsg) { self.0.borrow_mut().send(msg) }
    fn status(&self) -> LinkStatus { self.0.borrow().status() }
    fn blueprint(&mut self, hash: &ContentHash) -> Option<content::share::PackedBlueprint> { self.0.borrow_mut().blueprint(hash) }
    fn close(&mut self) { self.0.borrow_mut().close() }
}

/// Session state besides the contract fields of [`BpSession`]. Plain data; `Default` is inert.
#[derive(Default)]
pub(crate) struct SessionRun {
    pub(crate) frame: u64,
    pub(crate) now_s: f64,
    pub(crate) delta_s: f64,
    pub(crate) kind: SessionKind,
    pub(crate) net: NetRole,
    pub(crate) mode: Option<GameMode>,
    pub(crate) seed: u64,
    pub(crate) time_of_day: Option<TimeOfDay>,
    pub(crate) weather: Option<WeatherState>,
    pub(crate) pacing: Option<Pacing>,
    pub(crate) loading: f64,
    pub(crate) loading_sent: f64,
    pub(crate) world_built: bool,
    pub(crate) error: Option<Msg>,
    /// The sim pool, created on first use and kept (L1084).
    pub(crate) pool: Option<Arc<rayon::ThreadPool>>,
    /// Fixed-step sorties: the clock the main thread advances one tick per frame.
    pub(crate) manual_clock: Option<net::clock::ManualClock>,
    pub(crate) fixed_step: bool,
    /// Ruleset the designs of this sortie are compiled under.
    pub(crate) ruleset: Ruleset,
    pub(crate) designs: designs::DesignBuilds,
    pub(crate) recorder: killcam::ClipRecorder,
    pub(crate) clip: Option<killcam::ClipPlayer>,
    pub(crate) clip_inbox: LinkInbox,
    /// Opening events to feed before the live stream again after a kill-cam clip.
    pub(crate) reentry: Option<Vec<WireEvent>>,
    pub(crate) replay_link: Option<Rc<RefCell<ReplayLink>>>,
    pub(crate) replay_path: Option<String>,
    pub(crate) replay_announced: Option<u64>,
    pub(crate) input_tick: u64,
    /// The newest sampled frame, kept to draw a frozen world after the sortie ends.
    pub(crate) last_frame: Option<(Arc<Snapshot>, f64)>,
    pub(crate) newest_tick: Option<u64>,
    pub(crate) snapshot_rev: u64,
    /// Host side: the latest checkpoint, the result and replay of the ended sortie.
    pub(crate) checkpoint: Option<Box<content::save::SortieCheckpoint>>,
    pub(crate) result: Option<Box<SortieResult>>,
    pub(crate) finished_replay: Option<Arc<Vec<u8>>>,
    pub(crate) saved_replay: Option<String>,
    /// Slot of each player and the side it played, from joins and introductions (PvP replay release).
    pub(crate) sides: BTreeMap<PlayerSlot, SideId>,
    pub(crate) client_ids: BTreeMap<PlayerSlot, u64>,
    pub(crate) stats: Option<host::HostStats>,
    pub(crate) panel_sent_s: f64,
    /// The welcome and intro changed this frame (the feed copies them only then).
    pub(crate) welcome_dirty: bool,
    /// Session events this unit adds to the next frame's feed (a re-host's checkpoint bundle).
    pub(crate) injected: Vec<ClientEvent>,
    /// The open replay is pinned in the store.
    pub(crate) replay_pinned: bool,
    /// `pacing` came from a `PacingChanged` event (newer than any sampled frame's copy).
    pub(crate) pacing_from_event: bool,
    /// An offline sortie over net's loopback waits here until the local client is welcomed: the server seats
    /// only admitted clients when a sortie starts, and a config whose player carries no client id (Splash,
    /// trials, a standalone operation) cannot seat it later.
    pub(crate) deferred_start: Option<(host::HostStart, host::SortieOptions)>,
    /// The rate this player asked for last and when (`now_s`), until the host's pacing shows it: over a
    /// 100 ms link a second press of time_faster must step from the request in flight, not the old rate.
    pub(crate) rate_in_flight: Option<(TimeRate, f64)>,
}

#[godot_api]
impl INode for BpSession {
    fn init(base: Base<Node>) -> Self {
        Self { state: SessionState::Idle, view_builder: ViewBuilder::new(), inbox: Default::default(), last_summary: None, replay: None, input_accum_s: 0.0, run: SessionRun::default(), base }
    }
    fn ready(&mut self) {
        // First in the per-frame order (see the crate docs).
        self.base_mut().set_process_priority(-100);
    }
    /// Unit `gd-session`: take a pending sortie from the handoff; poll host notes and the link; refresh
    /// `Services::feed`; run `ViewBuilder::build` on the link's sampled frame into `Services::view`; send
    /// the 30 Hz `PlayerInput` built by `BpOrders` as `ClientMsg::Input`; emit session signals. Does nothing
    /// while idle with no handoff.
    fn process(&mut self, delta: f64) {
        let services = services();
        let mut sigs = Vec::new();
        self.run.frame += 1;
        self.run.delta_s = if delta.is_finite() { delta.max(0.0) } else { 0.0 };
        self.run.now_s = Time::singleton().get_ticks_usec() as f64 / 1_000_000.0;
        let pending = services.handoff.borrow_mut().sortie.take();
        if let Some(p) = pending { self.begin(p, &services, &mut sigs); }
        let live = services.connection.borrow().is_some();
        if live { self.frame(&services, &mut sigs); } else { self.idle_feed(&services); }
        self.emit(sigs);
    }
    /// Stops the host thread and closes the link so the process can exit cleanly.
    fn exit_tree(&mut self) {
        self.teardown(&services());
    }
}

// -------------------------------------------------------------------------------------------------------
// The frame
// -------------------------------------------------------------------------------------------------------

impl BpSession {
    /// The feed outside any session: time moves on, nothing else is there.
    fn idle_feed(&mut self, services: &Services) {
        let mut feed = services.feed.borrow_mut();
        feed.frame = self.run.frame;
        feed.now_s = self.run.now_s;
        feed.delta_s = self.run.delta_s;
        feed.state = self.state;
        feed.kind = self.run.kind;
        feed.net = self.run.net;
        if !feed.events.is_empty() { feed.events.clear(); }
        if !feed.session.is_empty() { feed.session.clear(); }
        if !feed.host_notes.is_empty() { feed.host_notes.clear(); }
        feed.snapshot = None;
    }

    fn frame(&mut self, services: &Services, sigs: &mut Vec<Sig>) {
        // `BpLobby` builds hosted and joined connections: their role and kind come from the connection.
        if let Some(conn) = services.connection.borrow().as_ref() {
            self.run.net = conn.net;
            if conn.kind != SessionKind::None { self.run.kind = conn.kind; }
        }
        // Fixed-step runs: the main thread is the clock (one tick per rendered frame), once the world is built.
        if self.run.fixed_step && self.run.world_built && matches!(self.state, SessionState::Loading | SessionState::InSortie) {
            if let Some(clock) = &self.run.manual_clock { clock.advance(Duration::from_nanos(host::TICK_NS)); }
            if let Some(h) = services.connection.borrow().as_ref().and_then(|c| c.host.as_ref()) { h.send(host::HostControl::StepOnce); }
        }

        // 1. Host notes.
        let mut notes = Vec::new();
        if let Some(h) = services.connection.borrow().as_ref().and_then(|c| c.host.as_ref()) { h.poll(&mut notes); }
        for note in &notes { self.on_note(note, services, sigs); }
        if self.state == SessionState::Failed && services.connection.borrow().as_ref().is_some_and(|c| c.net == NetRole::Offline) {
            self.teardown(services);
            let mut feed = services.feed.borrow_mut();
            feed.host_notes = notes;
            return;
        }

        // 2. The link, once.
        let inbox = &mut self.inbox;
        inbox.frame = None;
        inbox.newest_tick = None;
        inbox.shells.clear();
        inbox.events.clear();
        inbox.session.clear();
        if let (Some(rc), Some(control)) = (&self.run.replay_link, &self.replay) { rc.borrow_mut().control = *control; }
        let status = {
            let mut c = services.connection.borrow_mut();
            match c.as_mut() {
                Some(conn) => { conn.link.poll(self.run.now_s, inbox); conn.link.status() }
                None => LinkStatus::default(),
            }
        };
        if let (Some(rc), Some(control)) = (&self.run.replay_link, self.replay.as_mut()) {
            let link = rc.borrow();
            // The link pauses itself at the end and takes seeks; the session mirrors what it did.
            control.paused = link.control.paused;
            control.seek_to = None;
            control.viewer = link.control.viewer;
        }

        // 3. Session events, then simulation events.
        let session_events = std::mem::take(&mut self.inbox.session);
        for e in &session_events { self.on_client_event(e, services, sigs); }
        self.inbox.session = session_events;
        let mut wanted: Vec<(ContentHash, bool)> = Vec::new();
        let events = std::mem::take(&mut self.inbox.events);
        for e in &events { self.on_sim_event(e, &mut wanted, sigs); }
        self.inbox.events = events;
        if self.replay.is_none() {
            self.run.recorder.record_events(&self.inbox.events);
        }

        // 4. A kill-cam clip replaces what the feed shows (the live link is still polled above).
        let clip_active = self.run.clip.is_some();
        if let Some(clip) = self.run.clip.as_mut() {
            let ci = &mut self.run.clip_inbox;
            ci.frame = None;
            ci.newest_tick = None;
            ci.shells.clear();
            ci.events.clear();
            ci.session.clear();
            clip.poll(self.run.now_s, ci);
            for e in &self.run.clip_inbox.events {
                if let SimEvent::EntityRevealed(i) = &e.event { wanted.push((i.blueprint, i.exterior_only)); }
            }
        }

        // 5. Designs for the renderer.
        self.request_designs(&wanted, services);
        let arrived = self.run.designs.poll(&mut services.designs.borrow_mut());
        if arrived > 0 && self.state == SessionState::Loading { self.update_loading(sigs); }

        // 6. The frame and the view.
        let (frame, newest, shells_from_clip) = if clip_active {
            (self.run.clip_inbox.frame.take(), self.run.clip_inbox.newest_tick, true)
        } else {
            (self.inbox.frame.take(), self.inbox.newest_tick, false)
        };
        if newest.is_some() && newest != self.run.newest_tick {
            self.run.newest_tick = newest;
            self.run.snapshot_rev = self.run.snapshot_rev.wrapping_add(1);
        }
        let camera = services.camera.get();
        let tuning = InterpTuning::default();
        let mut shown: Option<Arc<Snapshot>> = None;
        match frame {
            Some(f) => {
                let snapshot = Arc::new(f.snapshot);
                let shells = if shells_from_clip { &self.run.clip_inbox.shells } else { &self.inbox.shells };
                self.view_builder.build(snapshot.clone(), f.render_tick, f.extrapolating, shells, self.run.delta_s, &tuning, camera, &mut services.view.borrow_mut());
                if !clip_active && self.replay.is_none() && matches!(self.state, SessionState::Loading | SessionState::InSortie) { self.run.recorder.record_frame(f.render_tick, &snapshot); }
                if !clip_active { self.run.last_frame = Some((snapshot.clone(), f.render_tick)); }
                self.snapshot_pacing(&snapshot.pacing);
                shown = Some(snapshot);
                if self.state == SessionState::Loading { self.first_frame(sigs); }
            }
            None => {
                // After the sortie the world stands still behind the after-action screen; elsewhere it clears.
                match (&self.run.last_frame, self.state) {
                    (Some((snap, tick)), SessionState::Ended) => {
                        self.view_builder.build(snap.clone(), *tick, false, &[], self.run.delta_s, &tuning, camera, &mut services.view.borrow_mut());
                        shown = Some(snap.clone());
                    }
                    _ => self.view_builder.build_empty(&mut services.view.borrow_mut()),
                }
            }
        }

        // 7. Replay position.
        if let Some(rc) = &self.run.replay_link {
            let tick = rc.borrow().tick();
            if self.run.replay_announced.is_none_or(|t| t.abs_diff(tick) >= REPLAY_ANNOUNCE_TICKS) {
                self.run.replay_announced = Some(tick);
                sigs.push(Sig::ReplayPosition(tick));
            }
        }

        // 8. The feed.
        {
            let mut feed = services.feed.borrow_mut();
            feed.frame = self.run.frame;
            feed.now_s = self.run.now_s;
            feed.delta_s = self.run.delta_s;
            feed.state = self.state;
            feed.kind = self.run.kind;
            feed.net = self.run.net;
            if self.run.welcome_dirty {
                self.run.welcome_dirty = false;
                let c = services.connection.borrow();
                feed.welcome = c.as_ref().and_then(|c| c.welcome.clone());
                feed.intro = c.as_ref().and_then(|c| c.intro.clone());
            }
            feed.snapshot = shown;
            feed.snapshot_rev = self.run.snapshot_rev;
            // Swap, so the vectors' allocations are reused frame after frame.
            feed.events.clear();
            if let Some(mut reentry) = self.run.reentry.take() {
                feed.events.append(&mut reentry);
                self.view_builder.clear();
            }
            if clip_active { feed.events.append(&mut self.run.clip_inbox.events); } else { feed.events.append(&mut self.inbox.events); }
            std::mem::swap(&mut feed.session, &mut self.inbox.session);
            self.inbox.session.clear();
            if !self.run.injected.is_empty() { let injected = std::mem::take(&mut self.run.injected); feed.session.splice(0..0, injected); }
            feed.host_notes = notes;
            feed.status = status;
        }

        // 9. The 30 Hz input.
        self.send_input(services);
    }

    /// A pending sortie: build the local connection unless one is already hosting, pre-build the designs it
    /// names, and send `StartSortie`.
    fn begin(&mut self, pending: PendingSortie, services: &Services, sigs: &mut Vec<Sig>) {
        let hosting = services.connection.borrow().as_ref().is_some_and(|c| c.host.is_some() && c.net == NetRole::Host);
        let facts = start_facts(&pending.start, services);
        if !hosting {
            self.teardown(services);
            let Some(facts) = facts.as_ref() else {
                // A lobby start with no lobby to start from.
                godot_warn!("session: a lobby sortie was handed over with no hosted session");
                self.fail(Msg::new("session.failed"), sigs);
                return;
            };
            match self.local_connection(pending.kind, &pending.options, facts, services) {
                Some(conn) => *services.connection.borrow_mut() = Some(conn),
                None => { self.fail(Msg::new("session.failed"), sigs); return; }
            }
            self.run.net = NetRole::Offline;
        } else {
            self.run.net = NetRole::Host;
            self.reset_sortie_state(services);
        }
        self.run.kind = pending.kind;
        self.run.fixed_step = pending.options.fixed_step;
        if let Some(f) = &facts {
            self.run.mode = Some(f.mode);
            self.run.seed = f.seed;
            self.run.time_of_day = Some(f.time_of_day);
            self.run.weather = Some(f.weather);
            self.run.ruleset = f.ruleset.clone();
        }
        self.prebuild(&pending.start, facts.as_ref(), services);
        if let Some(conn) = services.connection.borrow_mut().as_mut() {
            conn.kind = pending.kind;
            if conn.net == NetRole::Offline && conn.welcome.is_none() && matches!(crate::link::LOCAL_BACKEND, crate::link::LocalBackend::Net) {
                self.run.deferred_start = Some((pending.start, pending.options));
            } else if let Some(h) = &conn.host {
                h.send(host::HostControl::StartSortie { start: pending.start, options: pending.options });
            }
        }
        self.run.loading = 0.05;
        self.run.loading_sent = -1.0;
        self.set_state(SessionState::Loading, sigs);
        self.update_loading(sigs);
    }

    /// Forget the last sortie's per-sortie state (a new one starts on the same connection).
    fn reset_sortie_state(&mut self, services: &Services) {
        self.view_builder.clear();
        self.run.recorder.clear();
        self.run.clip = None;
        self.run.reentry = None;
        self.run.world_built = false;
        self.run.last_frame = None;
        self.run.newest_tick = None;
        self.run.pacing = None;
        self.run.pacing_from_event = false;
        self.run.rate_in_flight = None;
        self.run.checkpoint = None;
        self.run.result = None;
        self.run.finished_replay = None;
        self.run.saved_replay = None;
        self.run.error = None;
        self.input_accum_s = 0.0;
        let _ = services;
    }

    /// The pool, created on first use.
    fn pool(&mut self) -> Arc<rayon::ThreadPool> { self.run.pool.get_or_insert_with(host::sim_pool).clone() }

    /// An offline connection: the local pair (or net's loopback) and the host thread around its server end.
    fn local_connection(&mut self, kind: SessionKind, options: &host::SortieOptions, facts: &StartFacts, services: &Services) -> Option<Connection> {
        let pool = self.pool();
        let clock: Arc<dyn net::clock::Clock> = if options.fixed_step {
            let manual = net::clock::ManualClock::new();
            self.run.manual_clock = Some(manual.clone());
            Arc::new(manual)
        } else {
            self.run.manual_clock = None;
            Arc::new(net::clock::SystemClock::new())
        };
        let catalogue = services.catalogue();
        let (port, link): (Box<dyn crate::seams::HostPort>, Box<dyn ClientLink>) = match crate::link::LOCAL_BACKEND {
            crate::link::LocalBackend::Channel => crate::link::channel::pair(PlayerSlot(0), facts.side, facts.mode, clock, pool.clone(), catalogue),
            crate::link::LocalBackend::Net => {
                let link_options = crate::link::net::LinkOptions {
                    identity: net::join::ClientIdentity { client_id: local_client_id(), name: display_name(services) },
                    game_version: env!("CARGO_PKG_VERSION").to_string(),
                    build: if cfg!(debug_assertions) { net::msg::BuildKind::Dev } else { net::msg::BuildKind::GitHub },
                    interp: InterpTuning::default(),
                    clock,
                    catalogue,
                };
                let setup = sim_systems::modes::LobbySetup {
                    mode: facts.mode, ruleset: facts.ruleset.clone(), seed: facts.seed,
                    players: vec![sim_systems::modes::LobbyPlayer { slot: PlayerSlot(0), client_id: link_options.identity.client_id, name: link_options.identity.name.clone(), team: 0, blueprint: None, ready: true, complexity: 0.0 }],
                    skirmish: None, weather: facts.weather.kind, sea_state: facts.weather.sea_state, time_of_day: facts.time_of_day, map: None, at_1_0: false,
                };
                let record = matches!(kind, SessionKind::Campaign | SessionKind::Skirmish | SessionKind::Sandbox);
                let blueprints: Vec<content::share::PackedBlueprint> = facts.blueprints.iter().map(|b| content::share::pack(&content::blueprint::Shareable::Ship(Box::new(b.clone())))).collect();
                let (host, link) = crate::link::net::loopback(&link_options, setup, 1_000, record, pool.clone(), globalize(CACHE_DIR), &blueprints);
                (Box::new(host), Box::new(link))
            }
        };
        let host = host::HostHandle::spawn(port, pool, services.sim_deps_factory());
        Some(Connection { link, host: Some(host), hosting: None, welcome: None, intro: None, coop: None, kind, net: NetRole::Offline })
    }

    /// Start building every design the sortie names, while the clock thread builds the world.
    fn prebuild(&mut self, start: &host::HostStart, facts: Option<&StartFacts>, services: &Services) {
        let Some(facts) = facts else { return };
        let pool = self.pool();
        // The dry dock's design is already built: Splash and refits draw it at once.
        if let Some(current) = services.design.borrow().as_ref() {
            if let Some(design) = &current.design {
                if facts.blueprints.iter().any(|b| designs::blueprint_hash(b) == current.hash) {
                    services.designs.borrow_mut().entry(current.hash).or_insert_with(|| crate::worker::ReceivedDesign::Full { design: design.clone(), blueprint: current.blueprint.clone() });
                }
            }
        }
        if let host::HostStart::Splash { prepared, .. } = start {
            let bp = facts.blueprints.iter().find(|b| designs::blueprint_hash(b) == prepared.blueprint_hash).cloned();
            if let Some(bp) = bp { services.designs.borrow_mut().entry(prepared.blueprint_hash).or_insert_with(|| crate::worker::ReceivedDesign::Full { design: prepared.design.clone(), blueprint: Arc::new(bp) }); }
        }
        for source in &facts.designs {
            let (hash, src) = match source {
                FactDesign::Inline(bp) => (designs::blueprint_hash(bp), designs::DesignSource::Blueprint(Arc::new((**bp).clone()))),
                FactDesign::Stock(bp) => (designs::blueprint_hash(bp), designs::DesignSource::Blueprint(Arc::new((**bp).clone()))),
            };
            if self.run.designs.wants(&hash, &services.designs.borrow()) { self.run.designs.request(&pool, Some(hash), src, facts.ruleset.clone()); }
        }
    }

    /// Ask for every revealed design the store lacks: from the link's store, else the stock designs.
    fn request_designs(&mut self, wanted: &[(ContentHash, bool)], services: &Services) {
        if wanted.is_empty() { return; }
        let pool = self.pool();
        for &(hash, exterior_only) in wanted {
            if !self.run.designs.wants(&hash, &services.designs.borrow()) { continue; }
            let packed = services.connection.borrow_mut().as_mut().and_then(|c| c.link.blueprint(&hash));
            let source = match packed {
                Some(packed) => designs::DesignSource::Packed { packed, exterior_only },
                None => designs::DesignSource::StockHash(hash),
            };
            self.run.designs.request(&pool, Some(hash), source, self.run.ruleset.clone());
        }
    }

    fn update_loading(&mut self, sigs: &mut Vec<Sig>) {
        let d = &self.run.designs;
        let designs = if d.started == 0 { 1.0 } else { d.finished as f64 / d.started as f64 };
        let p = (0.05 + if self.run.world_built { 0.6 } else { 0.0 } + 0.35 * designs).clamp(0.0, 0.99);
        self.run.loading = self.run.loading.max(p);
        if (self.run.loading - self.run.loading_sent).abs() >= 0.01 {
            self.run.loading_sent = self.run.loading;
            sigs.push(Sig::Loading(self.run.loading));
        }
    }

    fn first_frame(&mut self, sigs: &mut Vec<Sig>) {
        self.run.loading = 1.0;
        self.run.loading_sent = 1.0;
        sigs.push(Sig::Loading(1.0));
        let next = if self.run.replay_link.is_some() { SessionState::Replay } else { SessionState::InSortie };
        self.set_state(next, sigs);
        sigs.push(Sig::Started);
    }

    fn set_state(&mut self, state: SessionState, sigs: &mut Vec<Sig>) {
        if self.state == state { return; }
        self.state = state;
        sigs.push(Sig::State(state));
    }

    fn fail(&mut self, msg: Msg, sigs: &mut Vec<Sig>) {
        self.run.error = Some(msg.clone());
        self.set_state(SessionState::Failed, sigs);
        sigs.push(Sig::Failed(msg));
    }

    /// Stop the host thread, close the link, forget the session's designs.
    fn teardown(&mut self, services: &Services) {
        let conn = services.connection.borrow_mut().take();
        if let Some(mut conn) = conn {
            conn.link.close();
            if let Some(mut h) = conn.host.take() { h.shutdown(); }
            if let Some(mut hosting) = conn.hosting.take() { hosting.stop(); }
        }
        self.run.replay_link = None;
        self.run.deferred_start = None;
        self.replay = None;
        self.run.designs.clear();
        services.designs.borrow_mut().clear();
        self.reset_sortie_state(services);
        self.run.manual_clock = None;
        self.run.fixed_step = false;
        self.run.kind = SessionKind::None;
        self.run.net = NetRole::Offline;
        self.run.mode = None;
        self.run.weather = None;
        self.run.sides.clear();
        self.run.client_ids.clear();
        self.run.stats = None;
        self.run.replay_announced = None;
        let mut feed = services.feed.borrow_mut();
        feed.snapshot = None;
        feed.events.clear();
        feed.session.clear();
        feed.panel = None;
        feed.welcome = None;
        feed.intro = None;
        feed.status = LinkStatus::default();
        drop(feed);
        self.view_builder.build_empty(&mut services.view.borrow_mut());
    }

    // ---- What arrives ----

    fn on_note(&mut self, note: &host::HostNote, services: &Services, sigs: &mut Vec<Sig>) {
        use host::HostNote as N;
        match note {
            N::SortieLoaded { .. } => { self.run.world_built = true; if self.state == SessionState::Loading { self.update_loading(sigs); } }
            N::SortieFailed(text) => {
                godot_warn!("session: the sortie did not start: {text}");
                if matches!(self.state, SessionState::Loading | SessionState::InSortie) { self.fail(Msg::new("session.failed"), sigs); }
            }
            N::SortieEnded { result, replay } => {
                self.run.result = Some(result.clone());
                if let Some(bytes) = replay {
                    let bytes = Arc::new(bytes.clone());
                    self.run.saved_replay = save_replay(&bytes);
                    self.run.finished_replay = Some(bytes);
                }
                // The client end may already have closed the sortie: complete what it left.
                if let Some(f) = services.handoff.borrow_mut().finished.as_mut() {
                    if f.result.is_none() { f.result = Some(result.clone()); }
                    if f.replay_path.is_none() { f.replay_path = self.run.saved_replay.clone(); }
                }
            }
            N::Checkpoint(c) => self.run.checkpoint = Some(c.clone()),
            N::Session(e) => self.on_host_event(e, services),
            N::Failed(text) => {
                godot_warn!("session: the host session failed: {text}");
                self.fail(Msg::new("session.failed.host_lost"), sigs);
            }
            N::Panicked(text) => {
                godot_warn!("session: the simulation stopped on an internal error: {text}");
                self.fail(Msg::new("session.failed.panicked"), sigs);
            }
            N::Stats(s) => self.run.stats = Some(*s),
            N::UdpBound(_) | N::Log(_) => {}
        }
    }

    /// Host-only session events this unit acts on (the table in the module docs).
    fn on_host_event(&mut self, e: &HostEvent, services: &Services) {
        let mut c = services.connection.borrow_mut();
        let Some(conn) = c.as_mut() else { return };
        match e {
            HostEvent::PlayerJoined { slot, client_id, .. } => {
                self.run.client_ids.insert(*slot, *client_id);
                if let Some(Coop::Host(h)) = conn.coop.as_mut() { h.member_joined(*client_id, *slot); }
            }
            HostEvent::PlayerLeft { client_id, .. } | HostEvent::HoldExpired { client_id, .. } => {
                if let Some(Coop::Host(h)) = conn.coop.as_mut() { h.member_left(*client_id); }
            }
            HostEvent::MappingReported { slot, report } => {
                if let (Some(Coop::Host(h)), Some(id)) = (conn.coop.as_mut(), self.run.client_ids.get(slot)) { h.on_mapping(*id, report.clone()); }
            }
            HostEvent::ReplayRequested { slot } => {
                // Only after the match, and only that side's cut (L1049).
                if let (Some(bytes), Some(side)) = (self.run.finished_replay.clone(), self.run.sides.get(slot).copied()) {
                    match net::replay::release_pvp(&bytes, &[side]) {
                        Ok(copies) => if let (Some(h), Some((_, cut))) = (conn.host.as_ref(), copies.into_iter().next()) {
                            h.send(host::HostControl::SendBulk { to: Some(*slot), kind: TransferKind::Replay, bytes: cut });
                        },
                        Err(err) => { godot_warn!("session: the replay for slot {} could not be cut: {err}", slot.0); }
                    }
                }
            }
            _ => {}
        }
        // A changed succession list goes to every member (L1005).
        if let Some(Coop::Host(h)) = conn.coop.as_mut() {
            if let Some(list) = h.take_succession() {
                if let Some(host) = conn.host.as_ref() { host.send(host::HostControl::Send { to: None, msg: ServerMsg::Succession(Box::new(list)) }); }
            }
        }
    }

    fn on_client_event(&mut self, e: &ClientEvent, services: &Services, sigs: &mut Vec<Sig>) {
        match e {
            ClientEvent::Welcomed(w) => {
                if let Some(conn) = services.connection.borrow_mut().as_mut() {
                    conn.welcome = Some(w.clone());
                    if let (Some((start, options)), Some(h)) = (self.run.deferred_start.take(), conn.host.as_ref()) { h.send(host::HostControl::StartSortie { start, options }); }
                }
                self.run.welcome_dirty = true;
                if self.run.mode.is_none() { self.run.mode = Some(w.mode); }
                // Admitted to a hosted or joined session: its lobby (or chart table) until a sortie loads.
                if self.run.net != NetRole::Offline && matches!(self.state, SessionState::Idle | SessionState::Failed) { self.set_state(SessionState::Lobby, sigs); }
            }
            ClientEvent::Refused(r) => self.fail(crate::link::net::refusal_msg(r), sigs),
            ClientEvent::SortieLoading(intro) => {
                if let Some(conn) = services.connection.borrow_mut().as_mut() { conn.intro = Some((**intro).clone()); }
                self.run.welcome_dirty = true;
                self.run.mode = Some(intro.mode);
                self.run.seed = intro.seed;
                if self.run.net == NetRole::Client || self.state != SessionState::Loading {
                    // A joined client enters loading here; a host already did when it sent the start.
                    self.reset_sortie_state(services);
                    self.run.loading = 0.05;
                    self.run.loading_sent = -1.0;
                    self.set_state(SessionState::Loading, sigs);
                }
            }
            ClientEvent::SortieRunning { .. } => {
                if self.run.net == NetRole::Client { self.run.world_built = true; }
                if self.state == SessionState::Loading { self.update_loading(sigs); }
            }
            ClientEvent::SortieClosed(summary) => self.sortie_over((**summary).clone(), services, sigs),
            ClientEvent::Connection(panel) => {
                services.feed.borrow_mut().panel = Some(Arc::new((**panel).clone()));
                // At most 1 Hz (the panel itself arrives at 1 Hz and on change).
                if self.run.now_s - self.run.panel_sent_s >= 0.999 { self.run.panel_sent_s = self.run.now_s; sigs.push(Sig::Connection); }
            }
            ClientEvent::Saturation { .. } => sigs.push(Sig::Connection),
            ClientEvent::Succession(list) => {
                if let Some(Coop::Member(m)) = services.connection.borrow_mut().as_mut().and_then(|c| c.coop.as_mut()) { m.on_succession((**list).clone()); }
            }
            ClientEvent::Checkpoint(bundle) => {
                if let Some(Coop::Member(m)) = services.connection.borrow_mut().as_mut().and_then(|c| c.coop.as_mut()) { m.on_checkpoint((**bundle).clone()); }
            }
            ClientEvent::HostSilent => {
                let can = match services.connection.borrow().as_ref().and_then(|c| c.coop.as_ref()) {
                    Some(Coop::Member(m)) => !matches!(m.rehost_plan(), net::coop::RehostPlan::Manual { checkpoint: None }),
                    _ => false,
                };
                sigs.push(Sig::HostLost(can));
            }
            ClientEvent::BlueprintReady(hash) => {
                // Built for drawing at once; the reveal that names it may already have come.
                let packed = services.connection.borrow_mut().as_mut().and_then(|c| c.link.blueprint(hash));
                if let Some(packed) = packed {
                    if self.run.designs.wants(hash, &services.designs.borrow()) {
                        let pool = self.pool();
                        self.run.designs.request(&pool, Some(*hash), designs::DesignSource::Packed { packed, exterior_only: false }, self.run.ruleset.clone());
                    }
                }
            }
            ClientEvent::BulkReceived(done) => {
                if done.kind == TransferKind::Replay {
                    if let Some(name) = save_replay(&done.bytes) { godot_print!("session: a released replay was saved as {name}"); }
                }
            }
            ClientEvent::Ended(end) => {
                use crate::seams::SessionEnd;
                match end {
                    // A sortie that already ended cleanly needs no failure on top.
                    SessionEnd::MatchOver | SessionEnd::HostQuit if self.state == SessionState::Ended => {}
                    _ => {
                        if matches!(self.state, SessionState::Loading | SessionState::InSortie | SessionState::Lobby) {
                            let lost = matches!(end, SessionEnd::HostLost);
                            let can = lost && matches!(services.connection.borrow().as_ref().and_then(|c| c.coop.as_ref()), Some(Coop::Member(m)) if !matches!(m.rehost_plan(), net::coop::RehostPlan::Manual { checkpoint: None }));
                            if lost && self.run.net == NetRole::Client { sigs.push(Sig::HostLost(can)); }
                            if self.run.net == NetRole::Offline { self.fail(Msg::new("session.failed.host_lost"), sigs); } else { self.fail(crate::link::net::end_msg(*end), sigs); }
                        }
                    }
                }
            }
            ClientEvent::Lobby(_) | ClientEvent::Theatre(_) | ClientEvent::TheatreRejected(_) | ClientEvent::Chart(_) | ClientEvent::Briefing(_) | ClientEvent::AfterAction(_) | ClientEvent::BlueprintRefused(..) | ClientEvent::Sim(_) => {}
        }
    }

    fn on_sim_event(&mut self, e: &WireEvent, wanted: &mut Vec<(ContentHash, bool)>, sigs: &mut Vec<Sig>) {
        match &e.event {
            SimEvent::PacingChanged(p) => self.note_pacing(p.clone(), true, sigs),
            SimEvent::LadderStep { rung, sub_step, up } => sigs.push(Sig::Ladder { rung: *rung, sub_step: *sub_step, up: *up }),
            SimEvent::PlayerJoined { player, name, side, .. } => {
                self.run.sides.insert(*player, *side);
                sigs.push(Sig::Joined { player: *player, name: name.clone() });
            }
            SimEvent::PlayerDropped { player, officers_in_command } => sigs.push(Sig::Dropped { player: *player, officers: *officers_in_command }),
            SimEvent::CommandRejected { reason, .. } => sigs.push(Sig::Rejected(sim_systems::text::reject(*reason))),
            SimEvent::SortieStarted { seed, .. } => { if self.replay.is_none() { self.run.seed = *seed; } }
            SimEvent::SortieEnded(summary) => {
                // Replays end by pausing at the last frame, not by leaving the replay.
                if self.replay.is_none() { self.sortie_over((**summary).clone(), &services(), sigs); }
            }
            SimEvent::KillCam { victim, killer, from_tick, to_tick } => {
                if self.replay.is_none() {
                    sigs.push(Sig::KillCam { victim: crate::convert::entity_to_gd(*victim), killer: killer.map_or(-1, crate::convert::entity_to_gd), from_s: *from_tick as f64 / sim_core::TICK_HZ as f64, to_s: *to_tick as f64 / sim_core::TICK_HZ as f64 });
                }
            }
            SimEvent::EntityRevealed(intro) => {
                if let Some(p) = intro.player { self.run.sides.insert(p, intro.side); }
                wanted.push((intro.blueprint, intro.exterior_only));
            }
            SimEvent::Phys(sim_core::events::PhysEvent::WeatherChanged { weather, .. }) => self.run.weather = Some(*weather),
            _ => {}
        }
    }

    /// A `SimEvent::PacingChanged`: the authoritative pacing, announced. The Sim sends one for every pause,
    /// rate, time-scale and rung change.
    fn note_pacing(&mut self, p: Pacing, announce: bool, sigs: &mut Vec<Sig>) {
        if announce { sigs.push(Sig::Pacing); }
        self.run.pacing_from_event = true;
        self.run.pacing = Some(p);
    }

    /// The sampled frame's pacing section. It is as old as the playback tick (about 133 ms behind the
    /// events), so once an event has set the pacing only its measured host tick time is taken from here.
    fn snapshot_pacing(&mut self, p: &Pacing) {
        match self.run.pacing.as_mut() {
            Some(cur) if self.run.pacing_from_event => cur.host_tick_ms = p.host_tick_ms,
            _ => { if self.run.pacing.as_ref() != Some(p) { self.run.pacing = Some(p.clone()); } }
        }
    }

    /// The sortie decided (or the server closed it): `Bp.SESSION_ENDED`, the summary, the hand-off.
    fn sortie_over(&mut self, summary: SortieSummary, services: &Services, sigs: &mut Vec<Sig>) {
        if !matches!(self.state, SessionState::Loading | SessionState::InSortie) { return; }
        self.last_summary = Some(summary.clone());
        self.run.clip = None;
        services.handoff.borrow_mut().finished = Some(FinishedSortie {
            kind: self.run.kind,
            summary: summary.clone(),
            result: self.run.result.clone(),
            replay_path: self.run.saved_replay.clone(),
            checkpoint: self.run.checkpoint.clone(),
        });
        self.set_state(SessionState::Ended, sigs);
        sigs.push(Sig::Ended(summary));
    }

    /// The 30 Hz `PlayerInput` (L1422): helm axes and trigger from `Orders`, the aim from the world view,
    /// the camera for relevance.
    fn send_input(&mut self, services: &Services) {
        if self.state != SessionState::InSortie || self.run.clip.is_some() || self.replay.is_some() { self.input_accum_s = 0.0; return; }
        let period = 1.0 / INPUT_HZ;
        self.input_accum_s += self.run.delta_s;
        if self.input_accum_s < period { return; }
        // Never a burst after a hitch: at most one input per frame, and the remainder is capped.
        self.input_accum_s = (self.input_accum_s - period).min(period);
        let orders = self.base().get_node_or_null("/root/Orders").and_then(|n| n.try_cast::<crate::orders::BpOrders>().ok()).map(|o| o.bind().input).unwrap_or_default();
        let fin = |v: f32| if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 };
        let aim = services.aim_local.get();
        let camera = services.camera.get();
        self.run.input_tick += 1;
        let render_tick = services.view.borrow().render_tick;
        let input = PlayerInput {
            input_tick: self.run.input_tick,
            client_render_tick: if render_tick.is_finite() && render_tick > 0.0 { render_tick.floor() as u64 } else { 0 },
            throttle: fin(orders.throttle),
            rudder: fin(orders.rudder),
            aim_local: if aim.iter().all(|c| c.is_finite()) { aim } else { [0.0; 3] },
            trigger: orders.trigger,
            camera_pos: if camera.valid && camera.pos.iter().all(|c| c.is_finite()) { camera.pos } else { [0.0; 3] },
        };
        if let Some(conn) = services.connection.borrow_mut().as_mut() { conn.link.send(ClientMsg::Input(input)); }
    }

    fn emit(&mut self, sigs: Vec<Sig>) {
        for sig in sigs {
            match sig {
                Sig::State(s) => { self.base_mut().emit_signal("state_changed", &[s.to_gd().to_variant()]); }
                Sig::Loading(f) => { self.base_mut().emit_signal("loading_progress", &[f.to_variant()]); }
                Sig::Started => { self.base_mut().emit_signal("sortie_started", &[]); }
                Sig::Ended(summary) => { let d = crate::convert::summary_dict(&summary); self.base_mut().emit_signal("sortie_ended", &[d.to_variant()]); }
                Sig::Pacing => { self.base_mut().emit_signal("pacing_changed", &[]); }
                Sig::Ladder { rung, sub_step, up } => { self.base_mut().emit_signal("ladder_step", &[(rung as i64).to_variant(), (sub_step as i64).to_variant(), up.to_variant()]); }
                Sig::Joined { player, name } => { self.base_mut().emit_signal("player_joined", &[crate::convert::player_to_gd(player).to_variant(), GString::from(name.as_str()).to_variant()]); }
                Sig::Dropped { player, officers } => { self.base_mut().emit_signal("player_dropped", &[crate::convert::player_to_gd(player).to_variant(), officers.to_variant()]); }
                Sig::Rejected(msg) => { let d = crate::convert::msg_dict(&msg); self.base_mut().emit_signal("command_rejected", &[d.to_variant()]); }
                Sig::Connection => { self.base_mut().emit_signal("connection_changed", &[]); }
                Sig::HostLost(can) => { self.base_mut().emit_signal("host_lost", &[can.to_variant()]); }
                Sig::KillCam { victim, killer, from_s, to_s } => {
                    let d = crate::convert::d! { "victim" => victim, "killer" => killer, "from_s" => from_s, "to_s" => to_s };
                    self.base_mut().emit_signal("kill_cam_ready", &[d.to_variant()]);
                }
                Sig::ReplayPosition(t) => { self.base_mut().emit_signal("replay_position_changed", &[(t.min(i64::MAX as u64) as i64).to_variant()]); }
                Sig::Failed(msg) => { let d = crate::convert::msg_dict(&msg); self.base_mut().emit_signal("failed", &[d.to_variant()]); }
            }
        }
    }

    // ---- Small queries ----

    fn own_slot(&self) -> PlayerSlot {
        services().connection.borrow().as_ref().and_then(|c| c.welcome.as_ref().map(|w| w.slot)).unwrap_or(PlayerSlot(0))
    }
    fn is_pvp(&self) -> bool { matches!(self.run.mode, Some(GameMode::PvpElimination | GameMode::PvpEscort)) }
    /// One human with the server in process: pause is allowed (L112).
    fn single_player(&self) -> bool {
        self.run.net == NetRole::Offline && !self.is_pvp() && !matches!(self.run.kind, SessionKind::None | SessionKind::Replay | SessionKind::Pvp)
    }
    fn send_command(&self, command: Command) -> bool {
        let services = services();
        let mut c = services.connection.borrow_mut();
        match c.as_mut() {
            Some(conn) if conn.link.status().state == LinkState::Joined => { conn.link.send(ClientMsg::Command(command)); true }
            _ => false,
        }
    }
    fn current_pacing(&self) -> Pacing {
        self.run.pacing.clone().unwrap_or(Pacing { time_scale: 1.0, ..Pacing::default() })
    }
    fn in_sortie(&self) -> bool { self.state == SessionState::InSortie }

    fn replay_cmd(&mut self, f: impl FnOnce(&mut ReplayControl)) -> bool {
        match self.replay.as_mut() { Some(c) => { f(c); true } None => false }
    }
}

/// The part of the sortie's start the main thread can read before the clock thread builds it.
struct StartFacts {
    mode: GameMode,
    side: SideId,
    seed: u64,
    time_of_day: TimeOfDay,
    weather: WeatherState,
    ruleset: Ruleset,
    /// The config's inline designs (the player's).
    blueprints: Vec<content::blueprint::Blueprint>,
    /// Every distinct design the config's vessels name.
    designs: Vec<FactDesign>,
}
enum FactDesign { Inline(Box<content::blueprint::Blueprint>), Stock(Box<content::blueprint::Blueprint>) }

fn start_facts(start: &host::HostStart, services: &Services) -> Option<StartFacts> {
    let owned;
    let config: &SortieConfig = match start {
        host::HostStart::New { config } => config,
        host::HostStart::Checkpoint { checkpoint } => { owned = sim_systems::sim::keyframe::decode_config(&checkpoint.config).ok()?; &owned }
        host::HostStart::Splash { prepared, .. } => &prepared.config,
        host::HostStart::Lobby => return None,
    };
    let side = config.players.iter().find(|p| p.slot == PlayerSlot(0)).or(config.players.first()).map(|p| p.side).unwrap_or(SideId(0));
    let mut seen = std::collections::BTreeSet::new();
    let mut designs = Vec::new();
    for v in &config.vessels {
        match &v.blueprint {
            BlueprintRef::Inline(h) => {
                if !seen.insert(format!("#{}", h.to_hex())) { continue; }
                if let Some(bp) = config.blueprints.iter().find(|b| designs::blueprint_hash(b) == *h || content::blueprint::io::compute_hash(b) == *h) { designs.push(FactDesign::Inline(Box::new(bp.clone()))); }
            }
            BlueprintRef::Stock(path) => {
                if !seen.insert(path.clone()) { continue; }
                match content::campaign::load::stock_blueprint(path) {
                    Ok(bp) => designs.push(FactDesign::Stock(Box::new(bp))),
                    Err(e) => { godot_warn!("session: stock design {path} does not load: {e}"); }
                }
            }
        }
    }
    let _ = services;
    Some(StartFacts { mode: config.mode, side, seed: config.seed, time_of_day: config.time_of_day, weather: config.weather, ruleset: config.settings.ruleset.clone(), blueprints: config.blueprints.clone(), designs })
}

/// `user://` path as an OS path.
fn globalize(path: &str) -> std::path::PathBuf { std::path::PathBuf::from(ProjectSettings::singleton().globalize_path(path).to_string()) }

fn replay_store() -> net::replay::ReplayStore { net::replay::ReplayStore::new(globalize(REPLAY_DIR)) }

/// Save replay bytes to the store (retention applied there); the `user://` path, or `None` on failure.
fn save_replay(bytes: &[u8]) -> Option<String> {
    match replay_store().save(bytes) {
        Ok(name) => Some(format!("{REPLAY_DIR}/{name}")),
        Err(e) => { godot_warn!("session: the replay could not be saved: {e}"); None }
    }
}

/// The store's file name of a replay path, if it is in the store.
fn store_file(path: &str) -> Option<&str> { path.strip_prefix(REPLAY_DIR).map(|p| p.trim_start_matches('/')).filter(|p| !p.is_empty() && !p.contains('/') && !p.contains('\\') && !p.contains("..")) }

/// How long a rate request counts as in flight before the host's pacing is trusted again (a lost or refused
/// request must not pin the stepping forever).
const RATE_IN_FLIGHT_S: f64 = 1.5;

/// The rate a time_faster or time_slower press steps from: the request still in flight when there is one,
/// else the rate the host confirmed.
fn step_base(confirmed: TimeRate, in_flight: Option<(TimeRate, f64)>, now_s: f64) -> TimeRate {
    match in_flight {
        Some((rate, at)) if rate != confirmed && now_s - at < RATE_IN_FLIGHT_S => rate,
        _ => confirmed,
    }
}

/// The install's client id (`app::install_client_id`, created on first use).
fn local_client_id() -> u64 { crate::app::install_client_id() }

fn display_name(services: &Services) -> String { services.settings.borrow().display_name.clone() }

/// Read a design from a path the setup screens give: `user://` and `res://` files, else a stock path.
fn load_design(path: &str) -> Result<content::blueprint::Blueprint, Msg> {
    let path = path.trim();
    if path.is_empty() { return Err(Msg::new("session.problem.no_blueprint")); }
    let bp = if path.starts_with("user://") || path.starts_with("res://") {
        if !FileAccess::file_exists(path) { return Err(Msg::new("session.problem.design_missing")); }
        let text = FileAccess::get_file_as_string(path).to_string();
        content::blueprint::io::load_blueprint(&text, path).map(|l| l.blueprint).map_err(|e| crate::convert::error_msg(&e))?
    } else {
        content::campaign::load::stock_blueprint(&setup::stock_key(path)).map_err(|e| crate::convert::error_msg(&e))?
    };
    Ok(setup::canonical(bp))
}

fn msgs(list: Vec<Msg>) -> Array<VarDictionary> { list.iter().map(crate::convert::msg_dict).collect() }

#[godot_api]
impl BpSession {
    /// The state moved; `state` is `Bp.SESSION_*`.
    #[signal]
    fn state_changed(state: i64);
    /// Loading progress 0..1 while `Bp.SESSION_LOADING` (blueprint streaming, world build, shader warm-up).
    #[signal]
    fn loading_progress(fraction: f64);
    /// First snapshot arrived: the HUD and world may draw.
    #[signal]
    fn sortie_started();
    /// `summary` is a `SummaryDict`. The state is now `Bp.SESSION_ENDED`.
    #[signal]
    fn sortie_ended(summary: VarDictionary);
    /// Pause, rate, time scale or ladder rung changed; read `pacing()`. Every change also gets a log entry.
    #[signal]
    fn pacing_changed();
    /// The host's overload ladder moved (L1019-1029).
    #[signal]
    fn ladder_step(rung: i64, sub_step: i64, up: bool);
    #[signal]
    fn player_joined(player: i64, name: GString);
    /// `officers_in_command` is true while the 5 minute rejoin window keeps the slot (L838).
    #[signal]
    fn player_dropped(player: i64, officers_in_command: bool);
    /// A command was refused; `text` is a `MsgDict`.
    #[signal]
    fn command_rejected(text: VarDictionary);
    /// Ping, loss, frames behind or a drop warning changed; read `connections()`. At most 1 Hz.
    #[signal]
    fn connection_changed();
    /// No packet from the host for 10 s (L1005). `can_rehost` says whether this member may re-host.
    #[signal]
    fn host_lost(can_rehost: bool);
    /// A kill-cam clip is ready: `{ "victim": int, "killer": int, "from_s": float, "to_s": float }` (L1049).
    #[signal]
    fn kill_cam_ready(clip: VarDictionary);
    /// Replay playback position moved by more than a second or by a seek.
    #[signal]
    fn replay_position_changed(tick: i64);
    /// The session failed to start or the link closed; `text` is a `MsgDict`. State is `Bp.SESSION_FAILED`.
    #[signal]
    fn failed(text: VarDictionary);

    // ---- State ----

    /// `Bp.SESSION_*`.
    #[func]
    fn state(&self) -> i64 { use crate::enums::GdEnum; self.state.to_gd() }
    /// Returns `SessionDict`. Cheap; changes are announced by `state_changed`.
    #[func]
    fn info(&self) -> VarDictionary {
        let services = services();
        let c = services.connection.borrow();
        let conn = c.as_ref();
        let welcome = conn.and_then(|c| c.welcome.as_ref());
        let intro = conn.and_then(|c| c.intro.as_ref());
        let feed = services.feed.borrow();
        let snapshot = feed.snapshot.as_ref();
        let vessel = snapshot.and_then(|s| s.ship.as_ref().map(|sh| sh.vessel).or_else(|| s.vessels.iter().find(|v| v.relevance == sim_systems::protocol::Relevance::Own).map(|v| v.entity)));
        let weather = self.run.weather.unwrap_or_default();
        let time_of_day = self.run.time_of_day.unwrap_or_else(|| TimeOfDay::from_sun_elevation_deg(weather.sun_elevation_deg));
        crate::convert::d! {
            "state" => self.state.to_gd(),
            "kind" => self.run.kind.to_gd(),
            "mode" => self.run.mode.or(welcome.map(|w| w.mode)).map_or(-1, |m| m.to_gd()),
            "net" => self.run.net.to_gd(),
            "player" => welcome.map_or(if self.run.kind == SessionKind::None { -1 } else { 0 }, |w| crate::convert::player_to_gd(w.slot)),
            "side" => intro.map_or(-1, |i| i.side.0 as i64),
            "vessel" => vessel.map_or(-1, crate::convert::entity_to_gd),
            "tick" => snapshot.map_or(0, |s| s.tick.min(i64::MAX as u64) as i64),
            "time_s" => self.time_s(),
            "seed" => (intro.map_or(self.run.seed, |i| i.seed) & (i64::MAX as u64)) as i64,
            "weather" => crate::convert::weather_dict(&weather),
            "time_of_day" => time_of_day.to_gd(),
            "is_host" => conn.is_some_and(|c| c.host.is_some()),
            "loading" => if self.state == SessionState::Loading { self.run.loading } else if matches!(self.state, SessionState::InSortie | SessionState::Ended | SessionState::Replay) { 1.0 } else { 0.0 },
            "error" => self.run.error.as_ref().map_or_else(crate::convert::empty_msg, crate::convert::msg_dict),
        }
    }
    /// Returns `PacingDict` (L110-116). Changes are announced by `pacing_changed`.
    #[func]
    fn pacing(&self) -> VarDictionary {
        let p = self.current_pacing();
        let requested: VarArray = p.requested.iter().map(|(slot, rate)| crate::convert::d! { "player" => crate::convert::player_to_gd(*slot), "rate" => rate.factor() as i64 }.to_variant()).collect();
        let in_sortie = self.in_sortie();
        crate::convert::d! {
            "paused" => p.paused,
            "rate" => p.effective.factor() as i64,
            "requested" => requested,
            "blocked" => p.blocked.map_or(-1, |b| b.to_gd()),
            "blocked_text" => p.blocked.map_or_else(crate::convert::empty_msg, |b| crate::convert::msg_dict(&sim_systems::text::compression_block(b))),
            "time_scale" => if p.time_scale.is_finite() { p.time_scale as f64 } else { 1.0 },
            "rung" => p.rung as i64,
            "sub_step" => p.sub_step as i64,
            "host_tick_ms" => if p.host_tick_ms.is_finite() { p.host_tick_ms as f64 } else { 0.0 },
            "can_pause" => in_sortie && self.single_player(),
            "can_compress" => in_sortie && !self.is_pvp() && p.blocked.is_none(),
        }
    }
    /// Sortie time in seconds at the rendered moment (interpolated), 0.0 outside a sortie.
    #[func]
    fn time_s(&self) -> f64 {
        if !matches!(self.state, SessionState::InSortie | SessionState::Ended | SessionState::Replay) { return 0.0; }
        let t = services().view.borrow().sim_time_s;
        if t.is_finite() { t.max(0.0) } else { 0.0 }
    }
    /// Returns `Array[ConnectionDict]`, own entry first, from the newest `ConnectionPanel`
    /// (`FrameFeed::panel`). 1 Hz; announced by `connection_changed`.
    #[func]
    fn connections(&self) -> Array<VarDictionary> {
        let services = services();
        let panel = services.feed.borrow().panel.clone();
        let Some(panel) = panel else { return Array::new() };
        let own = self.own_slot();
        let mut rows: Vec<&crate::seams::PlayerLink> = panel.players.iter().collect();
        rows.sort_by_key(|p| (p.slot != own, p.slot));
        rows.into_iter().map(crate::convert::connection_dict).collect()
    }
    /// Host tick rate in Hz as every player sees it (`ConnectionPanel::host_tick_hz`, L1025); 60.0 when
    /// healthy.
    #[func]
    fn host_tick_hz(&self) -> f64 {
        let panel = services().feed.borrow().panel.clone();
        match panel {
            Some(p) if p.host_tick_hz.is_finite() && p.host_tick_hz > 0.0 => p.host_tick_hz as f64,
            // Before the first panel of a running sortie there is no measurement: report the nominal rate.
            _ if self.in_sortie() => sim_core::TICK_HZ as f64,
            _ => 0.0,
        }
    }
    /// The last sortie's `SummaryDict`, or `null` before any sortie ended.
    #[func]
    fn last_summary(&self) -> Variant { self.last_summary.as_ref().map_or(Variant::nil(), |s| crate::convert::summary_dict(s).to_variant()) }

    // ---- Starting and leaving ----

    /// Start an offline skirmish against AI (L959). `setup` keys: `template: int` (`Bp.MISSION_*`),
    /// `act: int` (`Bp.ACT_*`, picks the tonnage cap), `enemy_per_player: int` (at least 1),
    /// `difficulty: DifficultyDict`, `weather: int`, `sea_state: int`, `time_of_day: int`, `seed: int`
    /// (0 = random), `blueprint: String` (path of the design to field). Returns `Array[MsgDict]` of problems;
    /// empty means the session is loading.
    #[func]
    fn start_skirmish(&mut self, setup: VarDictionary) -> Array<VarDictionary> {
        if let Some(busy) = self.busy() { return msgs(vec![busy]); }
        let services = services();
        let campaign = services.campaign();
        let catalogue = services.catalogue();
        let parsed = match setup::read_skirmish(&setup, &campaign.difficulty) { Ok(s) => s, Err(p) => return msgs(p) };
        let bp = match load_design(&parsed.blueprint) { Ok(b) => b, Err(m) => return msgs(vec![m]) };
        let problems = setup::design_problems(&bp, &catalogue, &setup::skirmish_ruleset(parsed.act), false);
        if !problems.is_empty() { return msgs(problems); }
        let player = setup::LocalPlayer { client_id: local_client_id(), name: display_name(&services) };
        let config = match setup::skirmish_config(&parsed, &bp, &player, &campaign, &catalogue) { Ok(c) => c, Err(m) => return msgs(vec![m]) };
        self.queue_local(SessionKind::Skirmish, config);
        Array::new()
    }
    /// Start an offline sandbox: free play with no end condition (L961). `setup` keys: `blueprint: String`,
    /// `weather: int`, `sea_state: int`, `time_of_day: int`, `seed: int`,
    /// `targets: Array[{ "blueprint": String, "hostile": bool, "count": int }]`. Returns problems as above.
    #[func]
    fn start_sandbox(&mut self, setup: VarDictionary) -> Array<VarDictionary> {
        if let Some(busy) = self.busy() { return msgs(vec![busy]); }
        let services = services();
        let campaign = services.campaign();
        let catalogue = services.catalogue();
        let parsed = match setup::read_sandbox(&setup) { Ok(s) => s, Err(p) => return msgs(p) };
        let bp = match load_design(&parsed.blueprint) { Ok(b) => b, Err(m) => return msgs(vec![m]) };
        let ruleset = setup::sandbox_ruleset();
        let problems = setup::design_problems(&bp, &catalogue, &ruleset, false);
        if !problems.is_empty() { return msgs(problems); }
        let mut targets = Vec::new();
        for row in &parsed.targets {
            let key = setup::stock_key(&row.blueprint);
            let design = if row.blueprint.starts_with("user://") || row.blueprint.starts_with("res://") {
                match load_design(&row.blueprint) {
                    Ok(t) => {
                        let issues = setup::design_problems(&t, &catalogue, &ruleset, true);
                        if !issues.is_empty() { return msgs(issues); }
                        setup::TargetDesign::Inline(Box::new(t))
                    }
                    Err(m) => return msgs(vec![m]),
                }
            } else {
                if let Err(e) = content::campaign::load::stock_blueprint(&key) { return msgs(vec![crate::convert::error_msg(&e)]); }
                let class = campaign.classes.iter().find(|(_, d)| setup::stock_key(&d.blueprint) == key).map(|(id, _)| id.clone());
                setup::TargetDesign::Stock { path: key, class }
            };
            targets.push((design, row.hostile, row.count));
        }
        let player = setup::LocalPlayer { client_id: local_client_id(), name: display_name(&services) };
        let config = match setup::sandbox_config(&parsed, &bp, &targets, &player, &campaign) { Ok(c) => c, Err(m) => return msgs(vec![m]) };
        self.queue_local(SessionKind::Sandbox, config);
        Array::new()
    }
    /// End the session and return to `Bp.SESSION_IDLE`. In a campaign sortie this first writes the suspend
    /// save from the latest 60 s checkpoint (L112); in multiplayer it leaves the lobby too unless
    /// `keep_lobby` is true. Safe to call in any state.
    #[func]
    fn leave(&mut self, keep_lobby: bool) {
        let services = services();
        let mut sigs = Vec::new();
        let running = matches!(self.state, SessionState::Loading | SessionState::InSortie);
        let hosted_here = services.connection.borrow().as_ref().is_some_and(|c| c.host.is_some());
        if running && hosted_here {
            let notes = self.stop_hosted_sortie(&services);
            // A suspend save needs the checkpoint; the campaign reads the aborted result from the hand-off.
            let checkpoint = notes.iter().rev().find_map(|n| if let host::HostNote::Checkpoint(c) = n { Some(c.clone()) } else { None }).or(self.run.checkpoint.clone());
            let result = notes.iter().find_map(|n| if let host::HostNote::SortieEnded { result, .. } = n { Some(result.clone()) } else { None });
            let summary = SortieSummary { outcome: sim_systems::protocol::Outcome::Aborted, duration_ticks: result.as_ref().map_or(0, |r| r.duration_ticks), ..Default::default() };
            services.handoff.borrow_mut().finished = Some(FinishedSortie { kind: self.run.kind, summary, result, replay_path: None, checkpoint });
        }
        let multiplayer = services.connection.borrow().as_ref().is_some_and(|c| c.net != NetRole::Offline);
        if multiplayer && keep_lobby {
            // Back to the lobby on the same connection: the host returns its session to the lobby phase.
            if let Some(h) = services.connection.borrow().as_ref().and_then(|c| c.host.as_ref()) { h.send(host::HostControl::SetPhase(crate::seams::SessionPhase::Lobby)); }
            self.reset_sortie_state(&services);
            self.run.designs.clear();
            services.designs.borrow_mut().clear();
            self.view_builder.build_empty(&mut services.view.borrow_mut());
            services.feed.borrow_mut().snapshot = None;
            self.set_state(SessionState::Lobby, &mut sigs);
        } else {
            self.teardown(&services);
            self.set_state(SessionState::Idle, &mut sigs);
        }
        drop(services);
        self.emit(sigs);
    }
    /// True when quitting now would write a suspend save (campaign sortie, this process hosts).
    #[func]
    fn can_suspend(&self) -> bool {
        self.run.kind == SessionKind::Campaign
            && matches!(self.state, SessionState::Loading | SessionState::InSortie)
            && services().connection.borrow().as_ref().is_some_and(|c| c.host.is_some())
    }
    /// Re-host a co-op sortie from the last checkpoint after host loss, following
    /// `net::coop::CoopMember::rehost_plan`. Returns the new join code, or `""`.
    #[func]
    fn rehost(&mut self) -> GString {
        let services = services();
        let plan = match services.connection.borrow().as_ref().and_then(|c| c.coop.as_ref()) {
            Some(Coop::Member(m)) => m.rehost_plan(),
            _ => return GString::new(),
        };
        let identity = net::join::ClientIdentity { client_id: local_client_id(), name: display_name(&services) };
        let link_options = crate::link::net::LinkOptions {
            identity: identity.clone(),
            game_version: env!("CARGO_PKG_VERSION").to_string(),
            build: if cfg!(debug_assertions) { net::msg::BuildKind::Dev } else { net::msg::BuildKind::GitHub },
            interp: InterpTuning::default(),
            clock: Arc::new(net::clock::SystemClock::new()),
            catalogue: services.catalogue(),
        };
        let mut sigs = Vec::new();
        let code = match plan {
            net::coop::RehostPlan::ConnectTo { addr, secret } => {
                // Another member re-hosts: join it, keeping our member state for the next host loss.
                let target = net::join::JoinTarget::Address { addr, secret };
                match crate::link::net::join_udp(&link_options, &target, globalize(CACHE_DIR)) {
                    Ok(link) => {
                        let coop = services.connection.borrow_mut().as_mut().and_then(|c| c.coop.take());
                        self.teardown(&services);
                        *services.connection.borrow_mut() = Some(Connection { link: Box::new(link), host: None, hosting: None, welcome: None, intro: None, coop, kind: SessionKind::Campaign, net: NetRole::Client });
                        self.run.net = NetRole::Client;
                        self.run.kind = SessionKind::Campaign;
                        self.set_state(SessionState::Lobby, &mut sigs);
                    }
                    Err(e) => { godot_warn!("session: could not reach the new host: {e}"); self.fail(crate::link::net::error_msg(&e), &mut sigs); }
                }
                GString::new()
            }
            net::coop::RehostPlan::HostHere { checkpoint, .. } | net::coop::RehostPlan::Manual { checkpoint: Some(checkpoint) } => {
                // Host the operation from the checkpoint; the campaign restores its theatre from the bundle's
                // save, which it finds as a `ClientEvent::Checkpoint` in the first frame's session events.
                let settings_port = net::DEFAULT_PORT;
                let max_clients = sim_systems::modes::player_limit(GameMode::Campaign, false, 1_000).saturating_sub(1) as usize;
                let (hosting, udp) = crate::link::net::HostingHandle::start(settings_port, 1_000, max_clients);
                let code = hosting.info().join_code.clone();
                let pool = self.pool();
                let setup = sim_systems::modes::LobbySetup {
                    mode: GameMode::Campaign, ruleset: Ruleset::default(), seed: 0,
                    players: vec![sim_systems::modes::LobbyPlayer { slot: PlayerSlot(0), client_id: identity.client_id, name: identity.name.clone(), team: 0, blueprint: None, ready: true, complexity: 0.0 }],
                    skirmish: None, weather: Default::default(), sea_state: Default::default(), time_of_day: Default::default(), map: None, at_1_0: false,
                };
                let (port, link) = crate::link::net::loopback(&link_options, setup, 1_000, true, pool.clone(), globalize(CACHE_DIR), &[]);
                let handle = host::HostHandle::spawn(Box::new(port), pool, services.sim_deps_factory());
                handle.send(host::HostControl::SetUdp { options: Some(udp), reason: crate::seams::DropReason::Left });
                let campaign_id = checkpoint.save.header.campaign_id;
                self.teardown(&services);
                let conn = Connection { link: Box::new(link), host: Some(handle), hosting: Some(hosting), welcome: None, intro: None, coop: Some(Coop::Host(net::coop::CoopHost::new(campaign_id))), kind: SessionKind::Campaign, net: NetRole::Host };
                *services.connection.borrow_mut() = Some(conn);
                self.run.injected.push(ClientEvent::Checkpoint(checkpoint.clone()));
                services.handoff.borrow_mut().sortie = Some(PendingSortie { kind: SessionKind::Campaign, start: host::HostStart::Checkpoint { checkpoint: Box::new(checkpoint.checkpoint.clone()) }, options: host::SortieOptions { checkpoints: true, fixed_step: false } });
                self.run.net = NetRole::Host;
                GString::from(code.as_str())
            }
            net::coop::RehostPlan::Manual { checkpoint: None } => GString::new(),
        };
        drop(services);
        self.emit(sigs);
        code
    }

    // ---- Time (L110-116) ----

    /// Pause or resume. Single-player only; returns false elsewhere. Orders given while paused apply on resume.
    #[func]
    fn set_paused(&mut self, paused: bool) -> bool {
        if !self.in_sortie() || !self.single_player() { return false; }
        self.send_command(Command::Pause(paused))
    }
    /// Request a compression rate: 1, 2 or 4. The effective rate is in `pacing()`; in co-op it is the minimum
    /// over players. Returns false in PvP or for another number.
    #[func]
    fn request_time_rate(&mut self, factor: i64) -> bool {
        let rate = match factor { 1 => TimeRate::X1, 2 => TimeRate::X2, 4 => TimeRate::X4, _ => return false };
        if !self.in_sortie() || self.is_pvp() { return false; }
        let sent = self.send_command(Command::RequestTimeRate(rate));
        if sent { self.run.rate_in_flight = Some((rate, self.run.now_s)); }
        sent
    }
    /// Step the requested rate up (+1) or down (-1) through 1, 2, 4. For the `time_faster` and `time_slower`
    /// actions and their gamepad equivalents.
    #[func]
    fn step_time_rate(&mut self, direction: i64) -> bool {
        if direction == 0 || !self.in_sortie() || self.is_pvp() { return false; }
        let p = self.current_pacing();
        let own = self.own_slot();
        let confirmed = p.requested.iter().find(|(s, _)| *s == own).map(|(_, r)| *r).unwrap_or(p.effective);
        let current = step_base(confirmed, self.run.rate_in_flight, self.run.now_s);
        const RATES: [TimeRate; 3] = [TimeRate::X1, TimeRate::X2, TimeRate::X4];
        let i = RATES.iter().position(|r| *r == current).unwrap_or(0) as i64;
        let j = i + direction.signum();
        if !(0..RATES.len() as i64).contains(&j) { return false; }
        let sent = self.send_command(Command::RequestTimeRate(RATES[j as usize]));
        if sent { self.run.rate_in_flight = Some((RATES[j as usize], self.run.now_s)); }
        sent
    }

    // ---- Replays (L1043-1050) ----
    // The files are managed by `net::replay::ReplayStore` over `user://replays`: `list` for `replays()`,
    // `save` (which applies the 20-plus-pinned retention) when a sortie ends or a released PvP replay
    // arrives, `set_pinned`, `load` for `replay_open`. Playback is `link::replay::ReplayLink`.

    /// Returns `Array[Dictionary]`, newest first: `path: String`, `mode: int`, `operation: String`,
    /// `players: PackedStringArray`, `duration_s: float`, `recorded_unix: int`, `pinned: bool`,
    /// `size_mb: float`, `playable: bool` (false for another format version), `problem: MsgDict`.
    #[func]
    fn replays(&self) -> Array<VarDictionary> {
        let dir = globalize(REPLAY_DIR);
        let entries = match replay_store().list() { Ok(e) => e, Err(e) => { if dir.exists() { godot_warn!("session: the replay list could not be read: {e}"); } return Array::new(); } };
        let mut rows: Vec<(i64, VarDictionary)> = entries.iter().map(|entry| {
            let recorded = std::fs::metadata(dir.join(&entry.file)).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
            let playable = entry.header.format == content::replay::REPLAY_FORMAT;
            let problem = if playable { crate::convert::empty_msg() } else { crate::convert::msg_dict(&Msg::new("session.replay.other_format").arg(MsgArg::Int(entry.header.format as i64)).arg(MsgArg::Int(content::replay::REPLAY_FORMAT as i64))) };
            let players: PackedStringArray = entry.header.player_names.iter().map(|n| GString::from(n.as_str())).collect();
            (recorded, crate::convert::d! {
                "path" => GString::from(format!("{REPLAY_DIR}/{}", entry.file).as_str()),
                "mode" => entry.header.mode.to_gd(),
                "operation" => GString::from(entry.header.operation.as_str()),
                "players" => players,
                "duration_s" => entry.header.duration_ticks as f64 / sim_core::TICK_HZ as f64,
                "recorded_unix" => recorded,
                "pinned" => entry.pinned,
                "size_mb" => entry.bytes as f64 / (1024.0 * 1024.0),
                "playable" => playable,
                "problem" => problem,
            })
        }).collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.0));
        rows.into_iter().map(|(_, d)| d).collect()
    }
    /// Open a replay; the session goes to `Bp.SESSION_REPLAY` through loading. Returns false with a
    /// `failed` signal if the file cannot be opened by this build.
    #[func]
    fn replay_open(&mut self, path: GString) -> bool {
        let mut sigs = Vec::new();
        let ok = self.open_replay(&path.to_string(), &mut sigs);
        self.emit(sigs);
        ok
    }
    /// Returns `{ "tick": int, "duration_ticks": int, "time_s": float, "duration_s": float, "rate": float,
    /// "paused": bool, "viewer_side": int (Bp.NONE = ground truth), "sides": PackedInt32Array, "pinned": bool }`.
    #[func]
    fn replay_info(&self) -> VarDictionary {
        let Some(rc) = &self.run.replay_link else { return VarDictionary::new() };
        let link = rc.borrow();
        let control = self.replay.unwrap_or(link.control);
        let sides: PackedInt32Array = link.viewers().iter().filter_map(|v| if let Viewer::Side(s) = v { Some(s.0 as i32) } else { None }).collect();
        let viewer_side = match control.viewer { Viewer::Side(s) => s.0 as i64, _ => -1 };
        let pinned = self.run.replay_pinned;
        let tick = link.tick();
        let duration = link.duration_ticks();
        crate::convert::d! {
            "tick" => tick.min(i64::MAX as u64) as i64,
            "duration_ticks" => duration.min(i64::MAX as u64) as i64,
            "time_s" => self.time_s(),
            "duration_s" => duration as f64 / sim_core::TICK_HZ as f64,
            "rate" => control.rate,
            "paused" => control.paused,
            "viewer_side" => viewer_side,
            "sides" => sides,
            "pinned" => pinned,
        }
    }
    /// Seek to a time in seconds (uses the 60 s keyframes).
    #[func]
    fn replay_seek(&mut self, time_s: f64) {
        if !time_s.is_finite() { return; }
        let tick = (time_s.max(0.0) * sim_core::TICK_HZ as f64).round() as u64;
        if self.replay_cmd(|c| c.seek_to = Some(tick)) {
            self.view_builder.clear();
            self.run.replay_announced = None;
        }
    }
    /// Playback rate 0.25 to 8.0.
    #[func]
    fn replay_set_rate(&mut self, rate: f64) {
        if !rate.is_finite() { return; }
        self.replay_cmd(|c| c.rate = rate.clamp(crate::link::replay::MIN_RATE, crate::link::replay::MAX_RATE));
    }
    #[func]
    fn replay_set_paused(&mut self, paused: bool) {
        let at_end = self.run.replay_link.as_ref().is_some_and(|rc| { let l = rc.borrow(); l.tick() >= l.duration_ticks() });
        self.replay_cmd(|c| {
            c.paused = paused;
            // Playing again from the end starts over.
            if !paused && at_end { c.seek_to = Some(0); c.end_tick = None; }
        });
    }
    /// Watch as one side saw it, or `Bp.NONE` for ground truth where the replay allows it.
    #[func]
    fn replay_set_viewer(&mut self, side: i64) -> bool {
        let Some(rc) = &self.run.replay_link else { return false };
        let viewer = if side < 0 { Viewer::Omniscient } else { match u8::try_from(side) { Ok(s) => Viewer::Side(SideId(s)), Err(_) => return false } };
        if !rc.borrow().viewers().contains(&viewer) { return false; }
        let changed = self.replay.is_some_and(|c| c.viewer != viewer);
        self.replay_cmd(|c| c.viewer = viewer);
        if changed { self.view_builder.clear(); }
        true
    }
    /// Pin or unpin (20 most recent plus pinned are kept, L1047).
    #[func]
    fn replay_pin(&mut self, path: GString, pinned: bool) -> bool {
        let path = path.to_string();
        let Some(file) = store_file(&path) else { return false };
        match replay_store().set_pinned(file, pinned) {
            Ok(()) => {
                if self.run.replay_path.as_deref().and_then(store_file) == Some(file) { self.run.replay_pinned = pinned; }
                true
            }
            Err(e) => { godot_warn!("session: could not pin {file}: {e}"); false }
        }
    }
    #[func]
    fn replay_delete(&mut self, path: GString) -> bool {
        let path = path.to_string();
        let Some(file) = store_file(&path) else { return false };
        // The replay being watched stays open; its file goes.
        let store = replay_store();
        let _ = store.set_pinned(file, false);
        match std::fs::remove_file(globalize(REPLAY_DIR).join(file)) { Ok(()) => true, Err(e) => { godot_warn!("session: could not delete {file}: {e}"); false } }
    }
    /// Play a kill-cam clip (the Dictionary from `kill_cam_ready`) over the live sortie; `kill_cam_stop`
    /// returns to the live view.
    #[func]
    fn kill_cam_play(&mut self, clip: VarDictionary) -> bool {
        let (Some(from_s), Some(to_s)) = (crate::convert::get_f64(&clip, "from_s"), crate::convert::get_f64(&clip, "to_s")) else { return false };
        if to_s < from_s || from_s < 0.0 { return false; }
        let from = (from_s * sim_core::TICK_HZ as f64).floor() as u64;
        let to = (to_s * sim_core::TICK_HZ as f64).ceil() as u64;
        if self.replay.is_some() {
            // In a replay the clip is the replay itself: play the window and pause at its end.
            self.view_builder.clear();
            return self.replay_cmd(|c| { c.seek_to = Some(from); c.end_tick = Some(to); c.paused = false; });
        }
        if !matches!(self.state, SessionState::InSortie | SessionState::Ended) { return false; }
        match self.run.recorder.clip(from, to, services().catalogue()) {
            Some(player) => { self.run.clip = Some(player); self.view_builder.clear(); true }
            None => false,
        }
    }
    #[func]
    fn kill_cam_stop(&mut self) {
        if self.replay.is_some() {
            self.replay_cmd(|c| c.end_tick = None);
            return;
        }
        if self.run.clip.take().is_some() {
            self.run.reentry = Some(self.run.recorder.live_opening());
            self.view_builder.clear();
        }
    }
}

impl BpSession {
    /// Why no new session can start now, if one is running.
    fn busy(&self) -> Option<Msg> {
        match self.state {
            SessionState::Idle | SessionState::Ended | SessionState::Failed => None,
            SessionState::Replay if self.run.clip.is_none() => None,
            _ => Some(Msg::new("session.problem.busy")),
        }
    }

    /// Hand a locally built sortie to the next frame, as other facades do.
    fn queue_local(&mut self, kind: SessionKind, config: SortieConfig) {
        let services = services();
        let mut sigs = Vec::new();
        if matches!(self.state, SessionState::Ended | SessionState::Failed | SessionState::Replay) { self.teardown(&services); }
        services.handoff.borrow_mut().sortie = Some(PendingSortie { kind, start: host::HostStart::New { config: Box::new(config) }, options: host::SortieOptions { checkpoints: false, fixed_step: false } });
        self.run.loading = 0.0;
        self.run.error = None;
        self.set_state(SessionState::Loading, &mut sigs);
        drop(services);
        self.emit(sigs);
    }

    /// Quit a sortie this process simulates: a checkpoint of the moment for a campaign sortie, then stop it,
    /// and wait (bounded) for the clock thread's answers. Returns the notes that arrived.
    fn stop_hosted_sortie(&mut self, services: &Services) -> Vec<host::HostNote> {
        let campaign = self.run.kind == SessionKind::Campaign;
        let mut notes = Vec::new();
        let c = services.connection.borrow();
        let Some(h) = c.as_ref().and_then(|c| c.host.as_ref()) else { return notes };
        if campaign { h.send(host::HostControl::CheckpointNow); }
        h.send(host::HostControl::StopSortie(sim_systems::protocol::Outcome::Aborted));
        if campaign {
            let deadline = std::time::Instant::now() + SUSPEND_WAIT;
            loop {
                h.poll(&mut notes);
                let done = notes.iter().any(|n| matches!(n, host::HostNote::SortieEnded { .. } | host::HostNote::SortieFailed(_) | host::HostNote::Panicked(_) | host::HostNote::Failed(_)));
                if done || std::time::Instant::now() >= deadline { break; }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        notes
    }

    fn open_replay(&mut self, path: &str, sigs: &mut Vec<Sig>) -> bool {
        if let Some(busy) = self.busy() { self.fail_soft(busy, sigs); return false; }
        let services = services();
        let bytes = match store_file(path) {
            Some(file) => replay_store().load(file).ok(),
            None if path.starts_with("res://") || path.starts_with("user://") => {
                if FileAccess::file_exists(path) { Some(FileAccess::get_file_as_bytes(path).to_vec()) } else { None }
            }
            None => None,
        };
        let Some(bytes) = bytes else { self.fail_soft(Msg::new("session.replay.missing"), sigs); return false };
        let link = match ReplayLink::open(&bytes, services.catalogue(), globalize(CACHE_DIR)) {
            Ok(l) => l,
            Err(sim_systems::replay::ReplayError::Format { found, expected }) => {
                self.fail_soft(Msg::new("session.replay.other_format").arg(MsgArg::Int(found as i64)).arg(MsgArg::Int(expected as i64)), sigs);
                return false;
            }
            Err(e) => { godot_warn!("session: replay {path} does not open: {e}"); self.fail_soft(Msg::new("session.replay.damaged"), sigs); return false; }
        };
        self.teardown(&services);
        let header = link.player.header().clone();
        self.replay = Some(link.control);
        let rc = Rc::new(RefCell::new(link));
        self.run.replay_link = Some(rc.clone());
        self.run.replay_path = Some(path.to_string());
        self.run.replay_pinned = store_file(path).is_some_and(|f| replay_store().list().map(|l| l.iter().any(|e| e.file == f && e.pinned)).unwrap_or(false));
        self.run.kind = SessionKind::Replay;
        self.run.net = NetRole::Offline;
        self.run.mode = Some(header.mode);
        self.run.seed = header.seed;
        self.run.weather = Some(header.weather);
        self.run.time_of_day = Some(TimeOfDay::from_sun_elevation_deg(header.weather.sun_elevation_deg));
        self.run.ruleset = Ruleset::default();
        self.run.loading = 0.05;
        self.run.loading_sent = -1.0;
        // Replays draw their designs as the opening introductions name them (the replay link's store or the
        // stock designs); the world is already built.
        self.run.world_built = true;
        *services.connection.borrow_mut() = Some(Connection { link: Box::new(SharedReplay(rc)), host: None, hosting: None, welcome: None, intro: None, coop: None, kind: SessionKind::Replay, net: NetRole::Offline });
        self.set_state(SessionState::Loading, sigs);
        true
    }

    /// A failure to start: with nothing running the session is `Bp.SESSION_FAILED`; a running or ended
    /// session is left alone and only the signal says why.
    fn fail_soft(&mut self, msg: Msg, sigs: &mut Vec<Sig>) {
        if matches!(self.state, SessionState::Idle | SessionState::Failed) { self.fail(msg, sigs); } else { sigs.push(Sig::Failed(msg)); }
    }
}

#[cfg(test)]
mod tests;
