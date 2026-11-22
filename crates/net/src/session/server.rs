//! Unit `net-server-session` (hard: the host loop ties the Sim, transports, lobby, replication and quality
//! together, and the tick timing must stay deterministic for the Sim). DESIGN.md L899-953, L1019-1039.
//!
//! `update` does, in order: read the clock; update every transport and collect connects and disconnects;
//! read hellos (`schema::codec::decode_hello`; a version mismatch is refused with both versions, L1003) and
//! admit through the lobby; read client messages (commands become `CommandEnvelope`s stamped with the
//! sender's slot and the measured RTT; inputs keep the newest by `input_tick` and feed acks to the snapshot
//! encoder; lobby, theatre, blueprint and bulk traffic); in `Sortie`: run the driver contract of
//! `sim_systems::sim` (set_host_load from [`crate::quality::HostLoadMeter`], set_input, apply, then step
//! `pacing().effective.factor()` times scaled by `time_scale`, zero while paused, at the 60 Hz fixed rate with
//! catch-up bounded by `max_catch_up_ticks`, each step inside the app's rayon pool), drain events into the
//! replay sink and [`crate::events::route`]; at 30 Hz per client build `Sim::snapshot(Viewer::Player(slot))`,
//! order it with that client's `PriorityAccumulator`, encode it with its `SnapshotEncoder` inside the
//! client's byte budget for the interval (cap minus reliable bytes already queued, bulk limited by
//! `blueprints::bulk_budget`); run the quality monitors (adaptive cap, drop rule, saturation; warnings after
//! 5 s; drops after 15 s; mid-sortie drops call `Sim::drop_player`, rejoins `Sim::rejoin_player`); send the
//! connection panel at 1 Hz and on change; log ladder steps (`SimEvent::LadderStep`) and quality events to
//! the connection log; flush transports.
//!
//! Joins, rejoins and leaves during a sortie:
//! - A client admitted while a sortie runs (co-op rejoin into its held slot within 5 minutes, L834; sandbox
//!   join any time, L961) gets `SortieLoading`, then `sim_systems::sim::snapshot::catch_up(sim, viewer)`
//!   events (sortie start, current weather and wave seed, pacing, an `EntityRevealed` with damage so far for
//!   everything its side may draw) on the `Events` and `Entity` channels before its first snapshot frame,
//!   with its encoder and accumulator reset. A co-op rejoin calls `Sim::rejoin_player(client_id)`.
//! - A sandbox join calls `Sim::add_player(PlayerInit { slot, client_id, name, side, vessel: None }, None,
//!   Some(blueprint))` once the joiner's blueprint has been accepted; `sys-sim-setup` then spawns it at a free
//!   spawn marker. A sandbox `Leave` calls `Sim::remove_player`.
//! - Any other drop mid-sortie calls `Sim::drop_player`; officers keep the hull (L834).
//!
//! The Sim never reads a clock; only this loop does, through the session's [`Clock`].
//!
//! # Choices this unit makes where the spec is silent (design decisions)
//! - Loading: a client is loaded when it reports `LobbyRequest::SetReady(true)` during loading (the client
//!   session sends it once it holds every own-side design of its `SortieIntro`), or 30 s after its
//!   `SortieLoading`. The sortie starts when every seated client is loaded. Events the Sim emitted while it
//!   was built are held and routed at the start; a client loading mid-sortie gets the catch-up instead. No
//!   event or frame of a sortie reaches a client before it has reported (or timed out), so a new sortie's
//!   reveals can never overtake its `SortieLoading` on another channel.
//! - Pause: `Sim::step` is where queued commands run, so while paused the session still calls it at 60 Hz
//!   (it applies time and alert commands and leaves the clock standing); unpausing is one such command.
//!   No backlog builds while paused.
//! - Catch-up: one `update` steps at most `max_catch_up_ticks` times the compression factor; what is left is
//!   the backlog, reported to the ladder in 60 Hz ticks behind real time and kept for at most 2 s of sortie
//!   time (past that the host has stalled, and catching up would only freeze it again). The synthetic tick
//!   cost of the ladder gate (L1027) is recorded in the load meter and also spent as real time: the session
//!   runs no more ticks than a host whose ticks cost that much could.
//! - Command sequence numbers are mapped per slot (`slot base + wire seq`, the base being the highest number
//!   the slot used before this connection), so a rejoining client that restarts at 1 is not dropped as a
//!   duplicate by the Sim.
//! - Reliable messages to a client wait in per-channel queues while renet2 cannot take them and go out in
//!   order; a client with more than 32 MiB queued has stalled and is dropped as falling behind. Event
//!   batches are split to at most 512 events and 256 KiB each.
//! - Refusals, kicks and quality drops send their reason and disconnect half a second later, so the reason
//!   arrives first. A connection that sends no hello within 10 s is refused `BadHello`. Malformed messages
//!   are skipped; they never end the update.
//! - The host's own member (slot 0 of a listen session) is exempt from the drop rule and from the
//!   saturation medians: its link is in-process.
//! - Bulk: 25% of the client's cap while it receives a live sortie (L943), its whole cap while it loads into
//!   one (it receives nothing else yet), uncapped otherwise.
//! - Snapshot budget a frame leaves unspent while items wait carries over (up to four intervals), so an item
//!   larger than one interval's share (a first full ship status at the 256 kbps start) still goes out and
//!   the long-run rate stays the cap.
//! - Over the cap (L937) the accumulator sheds in stages: a client whose frames defer items for half a
//!   second moves to `DistantTracksSlow`, then `UntrackedPoses`; two seconds without deferrals and with a
//!   third of the budget spare steps back.
//! - Blueprints: an accepted upload is stored with its exterior-only form (L945); `start_sortie` stores
//!   every design of the config. A client may fetch what its entitlements hold (granted from the sortie
//!   intro and from every `EntityRevealed` it receives, so in PvP only exterior hashes of the other team)
//!   and, between sorties, the lobby's designs: all of them outside PvP; in PvP its own in full and the
//!   others' exterior forms only, since teams may still change.
//! - A mid-sortie drop holds the slot (rejoin within 5 minutes) unless it was a kick or a sandbox leave.
//! - `set_phase(Lobby)` clears every ready flag, so a new start needs everyone to ready again.
//! - A dedicated server raises `StartRequested` (by the lowest seated slot) when the lobby becomes
//!   startable, since nobody there sends `Start`.
use super::{HostEvent, ServerOptions, SessionPhase};
use crate::blueprints::{BlueprintStore, Completed, Entitlements, Incoming, Outgoing};
use crate::channel::Channel;
use crate::clock::Clock;
use crate::delta::SnapshotEncoder;
use crate::error::{DropReason, JoinRefusal, NetError, SessionEnd};
use crate::events::{route, Recipient};
use crate::lobby::{Admission, Lobby};
use crate::msg::{BlueprintRefusal, BulkMsg, ClientMsg, ConnectionPanel, EventBatch, Hello, InputPacket, LinkQuality, LobbyRequest, LobbyView, PlayerLink, ServerMsg, SortieIntro, TransferKind, Welcome, WireEvent};
use crate::priority::{PriorityAccumulator, Shedding};
use crate::quality::{quality_of, BandwidthCap, ConnectionLog, DropMonitor, HostLoadMeter, LinkSample, LogKind, SaturationChange, SaturationDetector, Verdict};
use crate::replay::{ReplaySink, StreamCapture};
use crate::schema::codec::{self, HelloRead};
use crate::transport::{DisconnectCause, ServerTransport, ServerTransportEvent, TransportKind};
use content::blueprint::{Blueprint, Shareable};
use content::catalogue::Catalogue;
use content::enums::GameMode;
use content::ids::{ContentHash, PlayerSlot};
use content::share::PackedBlueprint;
use sim_systems::modes::LobbySetup;
use sim_systems::protocol::command::{CommandEnvelope, PlayerInput};
use sim_systems::protocol::config::{BlueprintRef, Outcome, PlayerInit};
use sim_systems::protocol::event::{OutEvent, SimEvent};
use sim_systems::protocol::snapshot::Viewer;
use sim_systems::replay::record::RecordOptions;
use sim_systems::Sim;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

/// A connection must say hello within this long.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// A refused, kicked or dropped connection stays open this long so its reason arrives first.
const LINGER: Duration = Duration::from_millis(500);
/// Most a client may take to load a sortie before it starts without it (and for a mid-sortie join).
pub const LOADING_TIMEOUT: Duration = Duration::from_secs(30);
/// Quality monitors run at this rate.
const QUALITY_PERIOD: Duration = Duration::from_millis(100);
/// Connection panel period, and the shortest gap between panels sent for a change.
const PANEL_PERIOD: Duration = Duration::from_secs(1);
const PANEL_MIN_GAP: Duration = Duration::from_millis(200);
/// Shortest gap between lobby views sent for a change, and the period of the check for changes the host
/// app made through `lobby_mut`.
const LOBBY_MIN_GAP: Duration = Duration::from_millis(250);
const LOBBY_RECHECK: Duration = Duration::from_secs(1);
/// Reliable bytes a client may hold queued before it counts as stalled.
const MAX_QUEUED_BYTES: usize = 32 << 20;
/// Event batch limits per message.
const BATCH_EVENTS: usize = 512;
const BATCH_BYTES: usize = 256 << 10;
/// Backlog kept at most, in seconds of sortie time.
const MAX_BACKLOG_S: f64 = 2.0;
/// Bulk bytes per snapshot interval outside a live sortie (uncapped, L943): far above renet2's allowance per
/// update, so the transport's ceiling and backpressure set the pace.
const LOBBY_BULK_BYTES: usize = 4 * (crate::blueprints::CHUNK_BYTES + 64);
/// Shedding hysteresis in snapshot intervals: escalate after this many deferring frames in a row (0.5 s),
/// relax after this many quiet ones (2 s).
const SHED_UP: i32 = 15;
const SHED_DOWN: i32 = -60;
/// Steps of a sortie before its tick p95 is reported: one second at 1x (mirrors gdclient's channel backend).
const MIN_P95_STEPS: u64 = sim_core::TICK_HZ as u64;
/// Per-second kbps samples kept for the p95 (4 hours).
const KBPS_KEEP: usize = 4 * 3600;

/// A transport-level connection: which backend and that backend's client id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnRef { pub transport: u8, pub id: u64 }

/// Per-connection host state.
pub struct Peer {
    pub conn: ConnRef,
    /// `None` until the hello is accepted.
    pub client_id: Option<u64>,
    pub slot: Option<PlayerSlot>,
    pub encoder: SnapshotEncoder,
    pub priority: PriorityAccumulator,
    pub cap: BandwidthCap,
    pub monitor: DropMonitor,
    pub outgoing: Outgoing,
    pub incoming: Incoming,
    pub last_input_tick: u64,
    pub latest_input: Option<PlayerInput>,
    pub warned: Option<crate::msg::DropWarning>,
    pub connected_at: Duration,
    /// Disconnect at this time (reason already queued).
    pub(crate) closing: Option<Duration>,
    /// Reliable messages waiting for renet2, per channel id.
    pub(crate) outbox: [VecDeque<Vec<u8>>; 6],
    pub(crate) queued_bytes: usize,
    /// Reliable bytes queued since the last snapshot interval (taken off the frame budget).
    pub(crate) interval_reliable: usize,
    pub(crate) traffic: Traffic,
    pub(crate) second_start: Duration,
    pub(crate) second_bytes: u64,
    pub(crate) kbps: Vec<f32>,
    /// The slot's command sequence before this connection.
    pub(crate) seq_base: u32,
    pub(crate) input_dirty: bool,
    pub(crate) sample: LinkSample,
    pub(crate) quality: LinkQuality,
    /// Receives the running sortie's events and frames.
    pub(crate) loaded: bool,
    /// Loading a sortie since then; waiting for its report.
    pub(crate) loading_since: Option<Duration>,
    pub(crate) loaded_report: bool,
    /// Blueprint being uploaded to the host.
    pub(crate) upload: Option<ContentHash>,
    pub(crate) shed_pressure: i32,
    /// Snapshot budget left unspent while items waited, carried into the next frame.
    pub(crate) frame_credit: usize,
    pub(crate) capture: Option<StreamCapture>,
    /// Captured frames whose fate the client's acks have not settled yet: (seq, tick, bytes).
    pub(crate) capture_pending: VecDeque<(u32, u64, Vec<u8>)>,
    pub(crate) last_lobby: Option<LobbyView>,
    /// The connection's memory or network backend.
    pub(crate) kind: TransportKind,
}

impl Peer {
    fn new(conn: ConnRef, kind: TransportKind, now: Duration, options: &ServerOptions) -> Peer {
        Peer {
            conn, client_id: None, slot: None, encoder: SnapshotEncoder::new(), priority: PriorityAccumulator::new(options.rates), cap: BandwidthCap::new(&options.quality, now),
            monitor: DropMonitor::new(), outgoing: Outgoing::new(), incoming: Incoming::new(), last_input_tick: 0, latest_input: None, warned: None, connected_at: now,
            closing: None, outbox: Default::default(), queued_bytes: 0, interval_reliable: 0, traffic: Traffic::default(), second_start: now, second_bytes: 0, kbps: Vec::new(),
            seq_base: 0, input_dirty: false, sample: LinkSample::default(), quality: LinkQuality::Good, loaded: false, loading_since: None, loaded_report: false, upload: None,
            shed_pressure: 0, frame_credit: 0, capture: None, capture_pending: VecDeque::new(), last_lobby: None, kind,
        }
    }
    fn admitted(&self) -> bool { self.client_id.is_some() && self.slot.is_some() && self.closing.is_none() }
    /// Count bytes sent toward the per-second totals of the p95.
    fn account(&mut self, now: Duration, bytes: usize) {
        let el = now.saturating_sub(self.second_start);
        if el >= Duration::from_secs(1) {
            let kbps = (self.second_bytes as f64 * 8.0 / 1000.0 / el.as_secs_f64()) as f32;
            self.kbps.push(kbps);
            if self.kbps.len() > KBPS_KEEP { self.kbps.remove(0); }
            self.second_start = now;
            self.second_bytes = 0;
        }
        self.second_bytes += bytes as u64;
    }
}

pub struct ServerSession {
    pub(crate) options: ServerOptions,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) pool: Arc<rayon::ThreadPool>,
    pub(crate) catalogue: Arc<Catalogue>,
    pub(crate) transports: Vec<Option<Box<dyn ServerTransport>>>,
    pub(crate) peers: std::collections::BTreeMap<ConnRef, Peer>,
    pub(crate) lobby: Lobby,
    pub(crate) phase: SessionPhase,
    pub(crate) sim: Option<Sim>,
    pub(crate) store: Box<dyn BlueprintStore>,
    pub(crate) entitlements: Entitlements,
    pub(crate) load: HostLoadMeter,
    pub(crate) saturation: SaturationDetector,
    pub(crate) log: ConnectionLog,
    pub(crate) replay: Option<ReplaySink>,
    pub(crate) last_update: Duration,
    pub(crate) tick_accumulator: f64,
    pub(crate) last_snapshot: Duration,
    pub(crate) last_panel: Duration,
    /// Host events raised outside `update` (kicks, transport removal), returned by the next one.
    pub(crate) pending_host: Vec<HostEvent>,
    /// Events the Sim emitted while it was built, routed when the sortie starts.
    pub(crate) held_events: Vec<OutEvent>,
    /// Decoded, validated designs by hash, and each one's exterior-only hash (L945).
    pub(crate) accepted: BTreeMap<ContentHash, Blueprint>,
    pub(crate) exterior_of: BTreeMap<ContentHash, ContentHash>,
    /// Highest command sequence each slot has used (see the module notes).
    pub(crate) seq_base: BTreeMap<PlayerSlot, u32>,
    pub(crate) shut_down: bool,
    pub(crate) last_quality: Duration,
    pub(crate) lobby_dirty: bool,
    pub(crate) last_lobby_sent: Duration,
    pub(crate) last_lobby_check: Duration,
    pub(crate) panel_dirty: bool,
    pub(crate) can_start_prev: bool,
    /// Fingerprint of the lobby state the last views were built from (skips the periodic rebuild when
    /// nothing changed; validation is not free).
    pub(crate) lobby_print: Option<blake3::Hash>,
    /// Real ticks owed while paused (each runs one paused `Sim::step`).
    pub(crate) pause_acc: f64,
    /// Synthetic tick cost not yet paid in real time, seconds.
    pub(crate) synthetic_debt_s: f64,
    pub(crate) backlog: u32,
    pub(crate) over_reported: bool,
    /// Simulation steps run since `start_sortie` (the tick-p95 warm-up guard, see [`MIN_P95_STEPS`]).
    pub(crate) sortie_steps: u64,
}

/// What one client message asks for.
enum Inbound { Session(Vec<u8>), Unreliable(Vec<u8>), Bulk(Vec<u8>) }

fn slot_of(ch: Channel) -> usize { ch.id() as usize }

impl ServerSession {
    /// A session in `Lobby` with no transports yet. Unit: `net-server-session`.
    pub fn new(options: ServerOptions, setup: LobbySetup, clock: Arc<dyn Clock>, pool: Arc<rayon::ThreadPool>, catalogue: Arc<Catalogue>, store: Box<dyn BlueprintStore>) -> ServerSession {
        let now = clock.now();
        let lobby = Lobby::new(options.lobby_name.clone(), setup, options.upload_kbps, options.dedicated);
        let mut load = HostLoadMeter::new();
        load.set_synthetic_ms(options.synthetic_tick_ms);
        ServerSession {
            options, clock, pool, catalogue, transports: Vec::new(), peers: BTreeMap::new(), lobby, phase: SessionPhase::Lobby, sim: None, store,
            entitlements: Entitlements::default(), load, saturation: SaturationDetector::new(), log: ConnectionLog::default(), replay: None, last_update: now,
            tick_accumulator: 0.0, last_snapshot: now, last_panel: now, pending_host: Vec::new(), held_events: Vec::new(), accepted: BTreeMap::new(),
            exterior_of: BTreeMap::new(), seq_base: BTreeMap::new(), shut_down: false, last_quality: now, lobby_dirty: true, last_lobby_sent: now,
            last_lobby_check: now, panel_dirty: true, can_start_prev: false, lobby_print: None, pause_acc: 0.0, synthetic_debt_s: 0.0, backlog: 0, over_reported: false, sortie_steps: 0,
        }
    }
    /// Add a backend (in-memory for the host's own client; UDP when the player hosts). Returns its index.
    /// Unit: `net-server-session`.
    pub fn add_transport(&mut self, transport: Box<dyn ServerTransport>) -> u8 {
        if let Some(i) = self.transports.iter().position(|t| t.is_none()) {
            self.transports[i] = Some(transport);
            return i as u8;
        }
        self.transports.push(Some(transport));
        (self.transports.len() - 1).min(u8::MAX as usize) as u8
    }
    /// Stop a backend: its peers drop (`DropReason::SecretRegenerated` when replacing for a new secret).
    /// Unit: `net-server-session`.
    pub fn remove_transport(&mut self, index: u8, reason: DropReason) {
        let now = self.clock.now();
        let conns: Vec<ConnRef> = self.peers.keys().filter(|c| c.transport == index).copied().collect();
        for conn in &conns {
            if self.peers.get(conn).is_some_and(|p| p.admitted()) {
                self.send_msg(*conn, &ServerMsg::Dropped(reason));
                self.depart(*conn, reason, false, now);
            }
            self.flush_peer(*conn);
        }
        if let Some(Some(t)) = self.transports.get_mut(index as usize) {
            t.flush();
            t.disconnect_all();
        }
        for conn in conns { self.peers.remove(&conn); }
        if let Some(slot) = self.transports.get_mut(index as usize) { *slot = None; }
        self.panel_dirty = true;
    }

    /// One pass of the host loop, as described in the module docs. Unit: `net-server-session`.
    pub fn update(&mut self) -> Result<Vec<HostEvent>, NetError> {
        if self.shut_down { return Ok(Vec::new()); }
        let now = self.clock.now();
        let dt = now.saturating_sub(self.last_update);
        self.last_update = now;
        let mut out = std::mem::take(&mut self.pending_host);

        self.update_transports(dt, now);
        self.read_peers(now, &mut out);
        self.expire(now, &mut out);
        match self.phase {
            SessionPhase::Loading => self.run_loading(now),
            SessionPhase::Sortie => {
                self.run_joiners(now);
                self.run_sim(now, dt, &mut out);
            }
            _ => {}
        }
        self.run_interval(now);
        if now.saturating_sub(self.last_quality) >= QUALITY_PERIOD {
            self.last_quality = now;
            self.run_quality(now, &mut out);
        }
        self.run_lobby(now, &mut out);
        self.run_panel(now);
        self.flush_all(now);
        out.append(&mut self.pending_host);
        Ok(out)
    }

    // ---- Transports and inbound traffic ---------------------------------------------------------------

    fn update_transports(&mut self, dt: Duration, now: Duration) {
        let unix = self.clock.unix_now();
        let mut connects = Vec::new();
        let mut disconnects = Vec::new();
        for (i, t) in self.transports.iter_mut().enumerate() {
            let Some(t) = t else { continue };
            // Receive errors are per datagram and renetcode keeps reading past them; the session goes on.
            let _ = t.update(dt, unix);
            while let Some(ev) = t.poll_event() {
                match ev {
                    ServerTransportEvent::Connected { client } => connects.push((ConnRef { transport: i as u8, id: client }, t.kind())),
                    ServerTransportEvent::Disconnected { client, cause } => disconnects.push((ConnRef { transport: i as u8, id: client }, cause)),
                }
            }
        }
        for (conn, cause) in disconnects {
            let reason = match cause { DisconnectCause::Remote => DropReason::Left, DisconnectCause::Local => DropReason::Left, DisconnectCause::TimedOut | DisconnectCause::Error(_) => DropReason::TimedOut };
            if self.peers.get(&conn).is_some_and(|p| p.client_id.is_some() && p.slot.is_some()) { self.depart(conn, reason, false, now); }
            self.peers.remove(&conn);
            self.panel_dirty = true;
        }
        for (conn, kind) in connects {
            let peer = Peer::new(conn, kind, now, &self.options);
            self.peers.insert(conn, peer);
        }
    }

    fn read_peers(&mut self, now: Duration, out: &mut Vec<HostEvent>) {
        let conns: Vec<ConnRef> = self.peers.keys().copied().collect();
        for conn in conns {
            let mut inbound = Vec::new();
            {
                let Some(Some(t)) = self.transports.get_mut(conn.transport as usize) else { continue };
                while let Some(b) = t.receive(conn.id, Channel::Session) { inbound.push(Inbound::Session(b)); }
                while let Some(b) = t.receive(conn.id, Channel::Unreliable) { inbound.push(Inbound::Unreliable(b)); }
                while let Some(b) = t.receive(conn.id, Channel::Bulk) { inbound.push(Inbound::Bulk(b)); }
            }
            for m in inbound {
                let Some(peer) = self.peers.get(&conn) else { break };
                if peer.closing.is_some() { break; }
                let admitted = peer.admitted();
                match m {
                    Inbound::Session(b) if !admitted => self.on_hello(conn, &b, now, out),
                    Inbound::Session(b) => if let Ok(msg) = codec::decode::<ClientMsg>(&b) { self.on_client_msg(conn, msg, now, out) },
                    Inbound::Unreliable(b) if admitted => if let Ok(p) = codec::decode::<InputPacket>(&b) { self.on_input(conn, p) },
                    Inbound::Bulk(b) if admitted => if let Ok(msg) = codec::decode::<BulkMsg>(&b) { self.on_bulk(conn, msg, now, out) },
                    _ => {}
                }
            }
        }
    }

    fn on_hello(&mut self, conn: ConnRef, bytes: &[u8], now: Duration, out: &mut Vec<HostEvent>) {
        let hello = match codec::decode_hello(bytes, crate::WIRE_VERSION) {
            HelloRead::Ok(h) => h,
            HelloRead::VersionMismatch { client } => return self.refuse(conn, 0, JoinRefusal::ProtocolMismatch { host: crate::WIRE_VERSION, client }, now, out),
            HelloRead::Malformed => return self.refuse(conn, 0, JoinRefusal::BadHello, now, out),
        };
        match self.lobby.admit(&hello, now, self.phase) {
            Ok(adm) => self.admitted(conn, &hello, adm, now, out),
            Err(r) => self.refuse(conn, hello.client_id, r, now, out),
        }
    }

    fn refuse(&mut self, conn: ConnRef, client_id: u64, reason: JoinRefusal, now: Duration, out: &mut Vec<HostEvent>) {
        self.send_msg(conn, &ServerMsg::Refused(reason.clone()));
        self.log.push(now, LogKind::Refused { client_id, reason: reason.clone() });
        out.push(HostEvent::JoinRefused { client_id, reason });
        self.close(conn, now);
    }

    fn admitted(&mut self, conn: ConnRef, hello: &Hello, adm: Admission, now: Duration, out: &mut Vec<HostEvent>) {
        let slot = adm.slot;
        let name = self.lobby.member(slot).map(|m| m.name.clone()).unwrap_or_default();
        let base = self.seq_base.get(&slot).copied().unwrap_or(0);
        if let Some(p) = self.peers.get_mut(&conn) {
            p.client_id = Some(hello.client_id);
            p.slot = Some(slot);
            p.seq_base = base;
        }
        let welcome = Welcome { protocol: crate::WIRE_VERSION, slot, mode: self.lobby.setup().mode, lobby_name: self.lobby.name.clone(), host_game_version: self.options.game_version.clone(), rejoined: adm.rejoined, phase: self.phase };
        self.send_msg(conn, &ServerMsg::Welcome(welcome));
        self.log.push(now, LogKind::Joined { client_id: hello.client_id, slot: slot.0 });
        out.push(HostEvent::PlayerJoined { slot, client_id: hello.client_id, name, rejoined: adm.rejoined });
        self.lobby_dirty = true;
        self.panel_dirty = true;
        // A join while a sortie is loaded or running.
        if matches!(self.phase, SessionPhase::Loading | SessionPhase::Sortie) {
            let in_sim = self.sim.as_ref().is_some_and(|s| s.players().iter().any(|p| p.client_id == hello.client_id && p.slot == slot));
            if in_sim {
                if let Some(sim) = self.sim.as_mut() { sim.rejoin_player(hello.client_id); }
                self.begin_loading(conn, now);
            } else {
                // A sandbox joiner enters the Sim once its design is accepted.
                self.try_sandbox_join(conn, now, out);
            }
        }
    }

    fn on_client_msg(&mut self, conn: ConnRef, msg: ClientMsg, now: Duration, out: &mut Vec<HostEvent>) {
        let Some(peer) = self.peers.get(&conn) else { return };
        let (Some(slot), Some(client_id)) = (peer.slot, peer.client_id) else { return };
        match msg {
            ClientMsg::Command(wc) => {
                let peer = &self.peers[&conn];
                if self.phase != SessionPhase::Sortie || !peer.loaded { return; }
                let rtt = self.transport_stats(conn).map_or(0.0, |s| s.rtt_ms);
                let seq = peer.seq_base.saturating_add(wc.seq);
                let rtt_ms = if rtt.is_finite() { rtt.clamp(0.0, u16::MAX as f32) as u16 } else { u16::MAX };
                let env = CommandEnvelope { player: slot, seq, client_render_tick: wc.client_render_tick, rtt_ms, command: wc.command };
                let e = self.seq_base.entry(slot).or_insert(0);
                *e = (*e).max(seq);
                if let Some(sim) = self.sim.as_mut() { sim.apply(&[env]); }
            }
            ClientMsg::Lobby(req) => {
                let loading = self.phase == SessionPhase::Loading || peer.loading_since.is_some();
                if loading && matches!(req, LobbyRequest::SetReady(true)) {
                    if let Some(p) = self.peers.get_mut(&conn) { p.loaded_report = true; }
                    return;
                }
                if self.lobby.apply(slot, &req, self.phase).is_ok() {
                    self.lobby_dirty = true;
                    match req {
                        LobbyRequest::Start => out.push(HostEvent::StartRequested { by: slot }),
                        LobbyRequest::SetBlueprint(h) => {
                            self.ensure_decoded(&h);
                            self.try_sandbox_join(conn, now, out);
                        }
                        _ => {}
                    }
                }
            }
            ClientMsg::Theatre(command) => out.push(HostEvent::Theatre { slot, command }),
            ClientMsg::RequestBlueprint(h) => self.serve_blueprint(conn, client_id, slot, h),
            ClientMsg::Mapping(report) => out.push(HostEvent::MappingReported { slot, report }),
            ClientMsg::RequestReplay => out.push(HostEvent::ReplayRequested { slot }),
            ClientMsg::Leave => {
                self.depart(conn, DropReason::Left, true, now);
                self.close(conn, now);
            }
        }
    }

    fn on_input(&mut self, conn: ConnRef, packet: InputPacket) {
        let Some(p) = self.peers.get_mut(&conn) else { return };
        p.encoder.on_ack(packet.ack);
        if p.capture.is_some() { settle_capture(p, packet.ack); }
        for input in packet.inputs {
            if input.input_tick > p.last_input_tick {
                p.last_input_tick = input.input_tick;
                p.latest_input = Some(input);
                p.input_dirty = true;
            }
        }
    }

    fn on_bulk(&mut self, conn: ConnRef, msg: BulkMsg, now: Duration, out: &mut Vec<HostEvent>) {
        let Some(p) = self.peers.get_mut(&conn) else { return };
        let Some(slot) = p.slot else { return };
        let start_kind = match &msg { BulkMsg::Start(s) => Some(s.kind), _ => None };
        if let Some(TransferKind::Blueprint(h)) = start_kind { p.upload = Some(h); }
        match p.incoming.on_msg(msg) {
            Ok(Some(done)) => {
                if matches!(done.kind, TransferKind::Blueprint(_)) { p.upload = None; self.lobby.set_upload_progress(slot, None); self.lobby_dirty = true; }
                self.on_upload(conn, slot, done, now, out);
            }
            Ok(None) => {
                if let Some(h) = p.upload {
                    let progress = p.incoming.progress(TransferKind::Blueprint(h));
                    self.lobby.set_upload_progress(slot, progress);
                    self.lobby_dirty = true;
                }
            }
            Err(e) => {
                let failed = match start_kind { Some(TransferKind::Blueprint(h)) => Some(h), _ => p.upload.filter(|h| p.incoming.progress(TransferKind::Blueprint(*h)).is_none()) };
                if let Some(h) = failed {
                    p.upload = None;
                    self.lobby.set_upload_progress(slot, None);
                    self.lobby_dirty = true;
                    let reason = if e == crate::blueprints::IncomingError::TooLarge { BlueprintRefusal::TooLarge } else { BlueprintRefusal::Invalid };
                    self.send_msg(conn, &ServerMsg::BlueprintRefused { hash: h, reason });
                    out.push(HostEvent::BlueprintRejected { slot, hash: h });
                }
            }
        }
    }

    fn on_upload(&mut self, conn: ConnRef, slot: PlayerSlot, done: Completed, now: Duration, out: &mut Vec<HostEvent>) {
        match done.kind {
            TransferKind::Blueprint(hash) => {
                let packed = PackedBlueprint { hash, compressed: done.bytes };
                match crate::blueprints::accept_upload(&packed, &self.catalogue, &self.lobby.setup().ruleset) {
                    Ok(bp) => {
                        self.store.put(hash, packed.compressed);
                        self.remember(hash, bp);
                        out.push(HostEvent::BlueprintAccepted { slot, hash });
                        self.lobby_dirty = true;
                        self.try_sandbox_join(conn, now, out);
                    }
                    Err(reason) => {
                        self.send_msg(conn, &ServerMsg::BlueprintRefused { hash, reason });
                        out.push(HostEvent::BlueprintRejected { slot, hash });
                    }
                }
            }
            _ => out.push(HostEvent::BulkReceived { slot, completed: done }),
        }
    }

    /// Keep a validated design and store its exterior-only form for the other PvP team (L945).
    fn remember(&mut self, hash: ContentHash, bp: Blueprint) {
        if !self.exterior_of.contains_key(&hash) {
            let (_, ext) = crate::blueprints::pack_exterior(&bp, &self.catalogue);
            if !self.store.contains(&ext.hash) { self.store.put(ext.hash, ext.compressed); }
            self.exterior_of.insert(hash, ext.hash);
        }
        self.accepted.insert(hash, bp);
    }

    /// Decode a design the host's store holds but has not validated yet (one the host app put there).
    fn ensure_decoded(&mut self, hash: &ContentHash) {
        if hash.is_empty() || self.accepted.contains_key(hash) { return; }
        let Some(bytes) = self.store.get(hash) else { return };
        let packed = PackedBlueprint { hash: *hash, compressed: bytes };
        if let Ok(bp) = crate::blueprints::accept_upload(&packed, &self.catalogue, &self.lobby.setup().ruleset) { self.remember(*hash, bp); }
    }

    /// Designs a member may fetch between sorties. Outside PvP, every design in the lobby. In PvP only the
    /// member's own design in full and everyone else's exterior form: teams can change until the start, so a
    /// teammate's full design now could be an opponent's interior later (L945). Teammates' full designs come
    /// with the sortie intro, once the teams are fixed.
    fn lobby_visible(&self, viewer: PlayerSlot) -> Vec<ContentHash> {
        let setup = self.lobby.setup();
        let pvp = matches!(setup.mode, GameMode::PvpElimination | GameMode::PvpEscort);
        setup.players.iter().filter_map(|p| {
            let h = p.blueprint?;
            if pvp && p.slot != viewer { self.exterior_of.get(&h).copied() } else { Some(h) }
        }).collect()
    }

    fn serve_blueprint(&mut self, conn: ConnRef, client_id: u64, slot: PlayerSlot, hash: ContentHash) {
        let lobby_ok = !matches!(self.phase, SessionPhase::Loading | SessionPhase::Sortie) && self.lobby_visible(slot).contains(&hash);
        if !(self.entitlements.allowed(client_id, &hash) || lobby_ok) {
            return self.send_msg(conn, &ServerMsg::BlueprintRefused { hash, reason: BlueprintRefusal::NotEntitled });
        }
        match self.store.get(&hash) {
            Some(bytes) if bytes.len() > content::blueprint::MAX_COMPRESSED_BYTES => self.send_msg(conn, &ServerMsg::BlueprintRefused { hash, reason: BlueprintRefusal::TooLarge }),
            Some(bytes) => {
                let Some(p) = self.peers.get_mut(&conn) else { return };
                let start = p.outgoing.enqueue(TransferKind::Blueprint(hash), bytes);
                self.send_msg(conn, &ServerMsg::TransferAnnounce(start));
            }
            None => self.send_msg(conn, &ServerMsg::BlueprintRefused { hash, reason: BlueprintRefusal::Unknown }),
        }
    }

    // ---- Leaving -------------------------------------------------------------------------------------

    /// Disconnect after the linger time (anything queued still goes out).
    fn close(&mut self, conn: ConnRef, now: Duration) {
        if let Some(p) = self.peers.get_mut(&conn) {
            if p.closing.is_none() { p.closing = Some(now + LINGER); }
        }
    }

    /// An admitted member leaves the session: lobby, Sim, entitlements, log and the host event.
    fn depart(&mut self, conn: ConnRef, reason: DropReason, polite: bool, now: Duration) {
        let Some(peer) = self.peers.get_mut(&conn) else { return };
        let (Some(client_id), Some(slot)) = (peer.client_id, peer.slot) else { return };
        let sample = peer.sample;
        peer.client_id = None;
        peer.slot = None;
        peer.loaded = false;
        peer.loading_since = None;
        let mid = matches!(self.phase, SessionPhase::Loading | SessionPhase::Sortie) && self.sim.is_some();
        let mode = self.sim.as_ref().map_or(self.lobby.setup().mode, |s| s.config().mode);
        let sandbox_leave = mode == GameMode::Sandbox && polite;
        let kicked = matches!(reason, DropReason::Kicked | DropReason::Banned);
        let hold = mid && !kicked && !sandbox_leave;
        if !kicked { self.lobby.remove(client_id, now, hold); }
        if let Some(sim) = self.sim.as_mut() {
            if sim.players().iter().any(|p| p.slot == slot && p.client_id == client_id) {
                if sandbox_leave && mid { sim.remove_player(slot); } else if mid { sim.drop_player(slot); }
            }
        }
        self.entitlements.revoke_client(client_id);
        self.log.push(now, if reason == DropReason::Left { LogKind::Left { client_id } } else { LogKind::Dropped { client_id, reason, sample } });
        self.pending_host.push(HostEvent::PlayerLeft { slot, client_id, reason, held: hold });
        self.lobby_dirty = true;
        self.panel_dirty = true;
    }

    fn expire(&mut self, now: Duration, out: &mut Vec<HostEvent>) {
        // Lingering connections.
        let done: Vec<ConnRef> = self.peers.iter().filter(|(_, p)| p.closing.is_some_and(|t| now >= t)).map(|(c, _)| *c).collect();
        for conn in done {
            self.flush_peer(conn);
            if let Some(Some(t)) = self.transports.get_mut(conn.transport as usize) { t.disconnect(conn.id); }
            self.peers.remove(&conn);
        }
        // Silent connections.
        let silent: Vec<ConnRef> = self.peers.iter().filter(|(_, p)| p.client_id.is_none() && p.closing.is_none() && now.saturating_sub(p.connected_at) >= HELLO_TIMEOUT).map(|(c, _)| *c).collect();
        for conn in silent { self.refuse(conn, 0, JoinRefusal::BadHello, now, out); }
        // Stalled links.
        let stalled: Vec<ConnRef> = self.peers.iter().filter(|(_, p)| p.admitted() && p.queued_bytes > MAX_QUEUED_BYTES).map(|(c, _)| *c).collect();
        for conn in stalled {
            if let Some(p) = self.peers.get_mut(&conn) { p.outbox = Default::default(); p.queued_bytes = 0; }
            self.send_msg(conn, &ServerMsg::Dropped(DropReason::FallingBehind));
            self.depart(conn, DropReason::FallingBehind, false, now);
            self.close(conn, now);
        }
        // Holds.
        for (client_id, slot) in self.lobby.expire_holds(now) {
            out.push(HostEvent::HoldExpired { slot, client_id });
            self.lobby_dirty = true;
            self.panel_dirty = true;
        }
    }

    // ---- Sortie --------------------------------------------------------------------------------------

    /// What a seated player prepares before its first frame.
    fn intro_for(sim: &Sim, slot: PlayerSlot) -> Option<SortieIntro> {
        let player = sim.players().iter().find(|p| p.slot == slot)?;
        let config = sim.config();
        let allied = config.sides.iter().find(|s| s.id == player.side).map_or(sim_systems::ids::SideMask::only(player.side), |s| s.allied.with(player.side));
        let mut own: Vec<ContentHash> = Vec::new();
        for v in &config.vessels {
            if let BlueprintRef::Inline(h) = &v.blueprint {
                if allied.has(v.side) && !h.is_empty() && !own.contains(h) { own.push(*h); }
            }
        }
        own.sort();
        Some(SortieIntro { mode: config.mode, seed: config.seed, viewer: Viewer::Player(slot), side: player.side, tick: sim.tick().0, own_side_blueprints: own, operation: config.operation.as_ref().map(|o| o.to_string()).unwrap_or_default() })
    }

    /// Start a seated client's loading: its intro, entitlements, a fresh encoder and accumulator.
    fn begin_loading(&mut self, conn: ConnRef, now: Duration) {
        let Some(sim) = self.sim.as_ref() else { return };
        let Some(p) = self.peers.get(&conn) else { return };
        let (Some(slot), Some(client_id)) = (p.slot, p.client_id) else { return };
        let Some(intro) = Self::intro_for(sim, slot) else { return };
        for h in &intro.own_side_blueprints { self.entitlements.grant(client_id, *h); }
        if let Some(p) = self.peers.get_mut(&conn) {
            p.loaded = false;
            p.loading_since = Some(now);
            p.loaded_report = false;
            p.encoder.reset();
            p.priority.clear();
            p.frame_credit = 0;
        }
        self.send_msg(conn, &ServerMsg::SortieLoading(Box::new(intro)));
    }

    fn try_sandbox_join(&mut self, conn: ConnRef, now: Duration, out: &mut Vec<HostEvent>) {
        if !matches!(self.phase, SessionPhase::Loading | SessionPhase::Sortie) { return; }
        let Some(sim) = self.sim.as_ref() else { return };
        if sim.config().mode != GameMode::Sandbox { return; }
        let Some(p) = self.peers.get(&conn) else { return };
        let (Some(slot), Some(client_id)) = (p.slot, p.client_id) else { return };
        if p.loading_since.is_some() || p.loaded || sim.players().iter().any(|q| q.slot == slot) { return; }
        let Some(player) = self.lobby.setup().players.iter().find(|q| q.slot == slot) else { return };
        let Some(hash) = player.blueprint else { return };
        let Some(bp) = self.accepted.get(&hash).cloned() else { return };
        let side = {
            let want = content::ids::SideId(player.team.min(1));
            if sim.config().sides.iter().any(|s| s.id == want) { want } else { sim.config().sides.first().map_or(content::ids::SideId(0), |s| s.id) }
        };
        let init = PlayerInit { slot, client_id, name: player.name.clone(), side, vessel: None, human: true };
        let Some(sim) = self.sim.as_mut() else { return };
        match sim.add_player(init, None, Some(bp)) {
            Ok(()) => self.begin_loading(conn, now),
            Err(_) => out.push(HostEvent::BlueprintRejected { slot, hash }),
        }
    }

    /// `Loading`: start once every seated client has loaded or timed out.
    fn run_loading(&mut self, now: Duration) {
        let Some(sim) = self.sim.as_ref() else { return };
        let seated: Vec<ConnRef> = self.peers.iter().filter(|(_, p)| p.admitted() && p.slot.is_some_and(|s| sim.players().iter().any(|q| q.slot == s))).map(|(c, _)| *c).collect();
        let ready = seated.iter().all(|c| self.peers.get(c).is_some_and(|p| p.loaded_report || p.loading_since.is_none_or(|t| now.saturating_sub(t) >= LOADING_TIMEOUT)));
        if !ready { return; }
        self.phase = SessionPhase::Sortie;
        self.tick_accumulator = 0.0;
        self.pause_acc = 0.0;
        self.backlog = 0;
        let tick = sim.tick().0;
        for c in &seated {
            if let Some(p) = self.peers.get_mut(c) {
                p.loaded = true;
                p.loading_since = None;
                p.encoder.reset();
                p.priority.clear();
            }
            self.send_msg(*c, &ServerMsg::SortieRunning { tick });
        }
        let held = std::mem::take(&mut self.held_events);
        self.route_events(&held, None);
        self.lobby_dirty = true;
    }

    /// Mid-sortie joiners: catch-up once loaded, then frames.
    fn run_joiners(&mut self, now: Duration) {
        let ready: Vec<ConnRef> = self.peers.iter().filter(|(_, p)| p.admitted() && p.loading_since.is_some_and(|t| p.loaded_report || now.saturating_sub(t) >= LOADING_TIMEOUT)).map(|(c, _)| *c).collect();
        for conn in ready {
            let Some(sim) = self.sim.as_ref() else { return };
            let Some(slot) = self.peers.get(&conn).and_then(|p| p.slot) else { continue };
            let events = sim_systems::sim::snapshot::catch_up(sim, Viewer::Player(slot));
            let tick = sim.tick().0;
            if let Some(p) = self.peers.get_mut(&conn) {
                p.loaded = true;
                p.loading_since = None;
                p.encoder.reset();
                p.priority.clear();
            }
            self.send_msg(conn, &ServerMsg::SortieRunning { tick });
            self.route_events(&events, Some(conn));
        }
    }

    /// The host load the ladder and the panel see. The tick p95 is not reported for a sortie's first second of
    /// steps, so the world's slow first tick (the build, 100 ms or more) is not read as overload; it would
    /// otherwise step the ladder to rung 2 at 00:00 and hold it until the tick leaves the 2 s window. The
    /// guard counts steps since `start_sortie`, once, as gdclient's channel backend does.
    fn host_load(&self) -> sim_systems::lod::HostLoad {
        let mut load = self.load.load(self.backlog);
        if self.sortie_steps < MIN_P95_STEPS { load.tick_p95_ms = 0.0; }
        load
    }

    /// The driver contract of `sim_systems::sim`.
    fn run_sim(&mut self, now: Duration, dt: Duration, out: &mut Vec<HostEvent>) {
        let load = self.host_load();
        let Some(sim) = self.sim.as_mut() else { return };
        sim.set_host_load(load);
        for p in self.peers.values_mut() {
            if !(p.loaded && p.input_dirty) { continue; }
            p.input_dirty = false;
            if let (Some(slot), Some(input)) = (p.slot, p.latest_input) { sim.set_input(slot, input); }
        }
        let dt_s = dt.as_secs_f64().min(MAX_BACKLOG_S);
        self.synthetic_debt_s = (self.synthetic_debt_s - dt_s).max(0.0);
        let synthetic_s = if self.options.synthetic_tick_ms.is_finite() { self.options.synthetic_tick_ms.max(0.0) as f64 / 1000.0 } else { 0.0 };
        let tick_s = 1.0 / sim_core::TICK_HZ as f64;
        let pacing = sim.pacing();
        let factor = pacing.effective.factor().max(1) as f64;
        let scale = if pacing.time_scale.is_finite() && pacing.time_scale > 0.0 { (pacing.time_scale as f64).min(1.0) } else { 1.0 };
        let paused = pacing.paused;
        let rate = if paused { 1.0 } else { factor * scale };
        let max_steps = (self.options.max_catch_up_ticks.max(1) as f64 * factor) as u32;
        if paused {
            self.tick_accumulator = 0.0;
            self.pause_acc = (self.pause_acc + dt_s * sim_core::TICK_HZ as f64).min(self.options.max_catch_up_ticks.max(1) as f64);
        } else {
            self.pause_acc = 0.0;
            self.tick_accumulator = (self.tick_accumulator + dt_s * sim_core::TICK_HZ as f64 * rate).min(MAX_BACKLOG_S * sim_core::TICK_HZ as f64 * rate);
        }
        let mut events: Vec<OutEvent> = Vec::new();
        let mut steps = 0u32;
        loop {
            let owed = if paused { self.pause_acc >= 1.0 } else { self.tick_accumulator >= 1.0 };
            if !owed || steps >= max_steps || self.synthetic_debt_s >= tick_s { break; }
            let t0 = self.clock.now();
            let sim = self.sim.as_mut().expect("sim checked above");
            self.pool.install(|| sim.step());
            let t1 = self.clock.now();
            self.load.record(t1, t1.saturating_sub(t0).as_secs_f64() as f32 * 1000.0);
            self.synthetic_debt_s += synthetic_s;
            steps += 1;
            self.sortie_steps += 1;
            if paused { self.pause_acc -= 1.0; } else { self.tick_accumulator -= 1.0; }
            let drained = sim.drain_events();
            if let Some(r) = self.replay.as_mut() { r.record(sim, &drained); }
            let now_paused = sim.pacing().paused;
            events.extend(drained);
            // A pause or unpause changes how time is owed: settle it on the next update.
            if now_paused != paused { break; }
        }
        self.backlog = (self.tick_accumulator.max(0.0) / rate.max(1e-6)).floor().min(u32::MAX as f64) as u32;
        for e in &events {
            if let SimEvent::LadderStep { rung, sub_step, up } = e.event {
                self.log.push(now, LogKind::LadderStep { rung, sub_step, up, tick_p95_ms: self.host_load().tick_p95_ms });
                self.panel_dirty = true;
            }
        }
        self.route_events(&events, None);
        if self.sim.as_ref().is_some_and(|s| s.is_over()) && !self.over_reported {
            self.over_reported = true;
            out.push(HostEvent::SortieOver);
        }
    }

    /// Route events to every loaded seated client (or to one), grant the designs reveals name, and queue the
    /// batches on their channels.
    fn route_events(&mut self, events: &[OutEvent], only: Option<ConnRef>) {
        if events.is_empty() { return; }
        let Some(sim) = self.sim.as_ref() else { return };
        let mut conns = Vec::new();
        let mut recipients = Vec::new();
        for (c, p) in &self.peers {
            if !p.admitted() || !(p.loaded || only == Some(*c)) { continue; }
            if only.is_some_and(|o| o != *c) { continue; }
            let Some(slot) = p.slot else { continue };
            let Some(ps) = sim.players().iter().find(|q| q.slot == slot) else { continue };
            conns.push(*c);
            recipients.push(Recipient { player: slot, side: ps.side });
        }
        if recipients.is_empty() { return; }
        let tick = sim.tick().0;
        let routed = route(events, &recipients, self.options.router);
        for (conn, r) in conns.into_iter().zip(routed) {
            if let Some(id) = self.peers.get(&conn).and_then(|p| p.client_id) {
                for e in &r.entity.events {
                    if let SimEvent::EntityRevealed(intro) = &e.event {
                        if !intro.blueprint.is_empty() { self.entitlements.grant(id, intro.blueprint); }
                    }
                }
            }
            for ch in [Channel::Events, Channel::Entity, Channel::Projectile] {
                let Some(batch) = r.batch(ch) else { continue };
                if batch.events.is_empty() { continue; }
                for bytes in split_batches(&batch.events) {
                    if let Some(p) = self.peers.get_mut(&conn) { if let Some(c) = p.capture.as_mut() { c.push(tick, ch, &bytes); } }
                    self.queue(conn, ch, bytes);
                }
            }
        }
    }

    /// Every snapshot interval: bulk within its share and the snapshot frame of each seated client.
    fn run_interval(&mut self, now: Duration) {
        let hz = self.options.rates.snapshot_hz.max(1);
        let interval = Duration::from_secs_f64(1.0 / hz as f64);
        if now.saturating_sub(self.last_snapshot) < interval { return; }
        self.last_snapshot = if now.saturating_sub(self.last_snapshot) > interval * 2 { now } else { self.last_snapshot + interval };
        let running = self.phase == SessionPhase::Sortie;
        let conns: Vec<ConnRef> = self.peers.iter().filter(|(_, p)| p.admitted()).map(|(c, _)| *c).collect();
        for conn in conns {
            let Some(p) = self.peers.get_mut(&conn) else { continue };
            let interval_bytes = p.cap.bytes_per_interval(hz);
            let live = running && p.loaded;
            // Live: 25% of the cap (L943). A client still loading into a live sortie receives nothing else,
            // so it may use its whole cap; outside a sortie the lane is uncapped.
            let bulk_budget = if live {
                crate::blueprints::bulk_budget(interval_bytes, true).min(interval_bytes.saturating_sub(p.interval_reliable))
            } else if running { interval_bytes.saturating_sub(p.interval_reliable) } else { LOBBY_BULK_BYTES };
            let mut bulk_used = 0usize;
            let msgs = p.outgoing.take(bulk_budget);
            for m in msgs {
                let b = codec::encode(&m);
                bulk_used += b.len();
                self.queue(conn, Channel::Bulk, b);
            }
            if live { self.send_frame(conn, interval_bytes, bulk_used, now); }
            if let Some(p) = self.peers.get_mut(&conn) { p.interval_reliable = 0; }
        }
    }

    fn send_frame(&mut self, conn: ConnRef, interval_bytes: usize, bulk_used: usize, now: Duration) {
        let Some(sim) = self.sim.as_mut() else { return };
        let Some(p) = self.peers.get_mut(&conn) else { return };
        let Some(slot) = p.slot else { return };
        if !sim.players().iter().any(|q| q.slot == slot) { return; }
        let snap = sim.snapshot(Viewer::Player(slot));
        sim.note_snapshot_items(sim_systems::sim::snapshot::snapshot_items(&snap));
        let order = p.priority.order(&snap);
        let budget = interval_bytes.saturating_sub(p.interval_reliable).saturating_sub(bulk_used);
        let available = budget.saturating_add(p.frame_credit);
        let frame = p.encoder.encode(&snap, &order, available);
        // An item larger than one interval's budget (a first full ship status, a big track list) would never
        // fit: while items wait, what a frame leaves unspent carries over, up to four intervals, so the
        // long-run rate stays the cap.
        p.frame_credit = if frame.deferred > 0 { available.saturating_sub(frame.bytes.len()).min(interval_bytes.saturating_mul(4)) } else { 0 };
        p.priority.sent(&frame.sent, snap.tick);
        // Shedding stages (L937).
        if frame.deferred > 0 { p.shed_pressure = p.shed_pressure.max(0) + 1; } else if frame.bytes.len() * 3 < available * 2 { p.shed_pressure = p.shed_pressure.min(0) - 1; } else { p.shed_pressure = 0; }
        let stage = p.priority.shedding();
        if p.shed_pressure >= SHED_UP && stage != Shedding::UntrackedPoses {
            p.priority.set_shedding(if stage == Shedding::None { Shedding::DistantTracksSlow } else { Shedding::UntrackedPoses });
            p.shed_pressure = 0;
        } else if p.shed_pressure <= SHED_DOWN && stage != Shedding::None {
            p.priority.set_shedding(if stage == Shedding::UntrackedPoses { Shedding::DistantTracksSlow } else { Shedding::None });
            p.shed_pressure = 0;
        }
        let n = frame.bytes.len();
        p.traffic.snapshot_bytes += n as u64;
        p.traffic.vessel_bytes += frame.stats.vessels as u64;
        p.traffic.vessel_updates += frame.stats.vessel_items as u64;
        let d = &mut p.traffic.detail;
        d.channel_bytes[Channel::Unreliable.id() as usize] += n as u64;
        d.channel_msgs[Channel::Unreliable.id() as usize] += 1;
        d.frames += 1;
        d.frame_header_bytes += frame.stats.header as u64;
        d.removal_bytes += frame.stats.removals as u64;
        d.body_bytes += frame.stats.bodies as u64;
        d.body_items += frame.stats.body_items as u64;
        d.track_bytes += frame.stats.tracks as u64;
        d.track_items += frame.stats.track_items as u64;
        for (a, b) in d.section_bytes.iter_mut().zip(frame.stats.section_bytes) { *a += b as u64; }
        d.deferred_items += frame.deferred as u64;
        d.deferred_frames += u64::from(frame.deferred > 0);
        d.shed_frames[match p.priority.shedding() { Shedding::None => 0, Shedding::DistantTracksSlow => 1, Shedding::UntrackedPoses => 2 }] += 1;
        p.account(now, n);
        if p.capture.is_some() { p.capture_pending.push_back((frame.seq, snap.tick, frame.bytes.clone())); }
        if let Some(Some(t)) = self.transports.get_mut(conn.transport as usize) {
            if t.can_send(conn.id, Channel::Unreliable, n) { t.send(conn.id, Channel::Unreliable, frame.bytes); }
        }
    }

    // ---- Quality, lobby and panel --------------------------------------------------------------------

    fn transport_stats(&self, conn: ConnRef) -> Option<crate::transport::LinkStats> {
        self.transports.get(conn.transport as usize).and_then(|t| t.as_ref()).and_then(|t| t.stats(conn.id))
    }

    fn is_host_member(&self, p: &Peer) -> bool { p.slot.and_then(|s| self.lobby.member(s)).is_some_and(|m| m.host) }

    fn run_quality(&mut self, now: Duration, out: &mut Vec<HostEvent>) {
        let tuning = self.options.quality;
        let running = self.phase == SessionPhase::Sortie;
        let conns: Vec<ConnRef> = self.peers.iter().filter(|(_, p)| p.admitted()).map(|(c, _)| *c).collect();
        let mut samples = Vec::new();
        let mut measured = 0.0f32;
        let mut drops = Vec::new();
        for conn in conns {
            let stats = self.transport_stats(conn).unwrap_or_default();
            let host = self.peers.get(&conn).is_some_and(|p| self.is_host_member(p));
            let Some(p) = self.peers.get_mut(&conn) else { continue };
            let behind = if running && p.loaded { p.encoder.frames_behind() } else { 0 };
            let sample = LinkSample { rtt_ms: stats.rtt_ms, loss: stats.loss, frames_behind: behind, sent_kbps: stats.sent_kbps, recv_kbps: stats.recv_kbps };
            p.sample = sample;
            p.cap.update(now, &sample, &tuning);
            let q = quality_of(&sample, p.cap.kbps(), &tuning);
            if q != p.quality { p.quality = q; self.panel_dirty = true; }
            if host { continue; }
            samples.push(sample);
            if sample.sent_kbps.is_finite() { measured += sample.sent_kbps.max(0.0); }
            match p.monitor.update(now, &sample, &tuning) {
                Verdict::Drop(reason) => drops.push((conn, reason)),
                Verdict::Warn(w) => {
                    if p.warned != Some(w) {
                        p.warned = Some(w);
                        if let Some(client_id) = p.client_id { self.log.push(now, LogKind::Warning { client_id, warning: w, sample }); }
                        self.panel_dirty = true;
                    }
                }
                Verdict::Fine => if p.warned.take().is_some() { self.panel_dirty = true; },
            }
        }
        for (conn, reason) in drops {
            self.send_msg(conn, &ServerMsg::Dropped(reason));
            self.depart(conn, reason, false, now);
            self.close(conn, now);
        }
        match self.saturation.update(now, &samples, measured, &tuning) {
            Some(SaturationChange::Saturated { upload_kbps }) => {
                for p in self.peers.values_mut() { p.cap.scale(1.0 - tuning.cap_cut, &tuning); }
                if upload_kbps > 0 { self.options.upload_kbps = upload_kbps; self.lobby.set_upload_kbps(upload_kbps); }
                self.lobby.set_saturated(true);
                self.saturation_changed(true, SaturationChange::Saturated { upload_kbps }, now, out);
            }
            Some(SaturationChange::Recover) => {
                for p in self.peers.values_mut() { p.cap.scale(1.0 + tuning.sat_recover, &tuning); }
                self.log.push(now, LogKind::Saturation(SaturationChange::Recover));
                self.panel_dirty = true;
            }
            Some(SaturationChange::Cleared) => {
                self.lobby.set_saturated(false);
                self.saturation_changed(false, SaturationChange::Cleared, now, out);
            }
            None => {}
        }
    }

    fn saturation_changed(&mut self, on: bool, change: SaturationChange, now: Duration, out: &mut Vec<HostEvent>) {
        self.log.push(now, LogKind::Saturation(change));
        self.broadcast(ServerMsg::Saturation { on });
        out.push(HostEvent::Saturation { on });
        self.lobby_dirty = true;
        self.panel_dirty = true;
    }

    /// Validated designs the lobby's players chose, for lobby validation.
    fn lobby_blueprints(&mut self) -> Vec<Blueprint> {
        let hashes: Vec<ContentHash> = self.lobby.setup().players.iter().filter_map(|p| p.blueprint).collect();
        for h in &hashes { self.ensure_decoded(h); }
        let mut seen = std::collections::BTreeSet::new();
        hashes.iter().filter(|h| seen.insert(**h)).filter_map(|h| self.accepted.get(h).cloned()).collect()
    }

    fn run_lobby(&mut self, now: Duration, out: &mut Vec<HostEvent>) {
        let recheck = self.phase == SessionPhase::Lobby && now.saturating_sub(self.last_lobby_check) >= LOBBY_RECHECK;
        let due = self.lobby_dirty && now.saturating_sub(self.last_lobby_sent) >= LOBBY_MIN_GAP;
        if !(due || recheck) { return; }
        if recheck { self.last_lobby_check = now; }
        let print = self.lobby_fingerprint();
        if !due && self.lobby_print == Some(print) { return; }
        self.lobby_print = Some(print);
        if due { self.last_lobby_sent = now; self.lobby_dirty = false; }
        let blueprints = self.lobby_blueprints();
        let conns: Vec<(ConnRef, PlayerSlot)> = self.peers.iter().filter_map(|(c, p)| if p.admitted() { p.slot.map(|s| (*c, s)) } else { None }).collect();
        let ext = &self.exterior_of;
        let views: Vec<(ConnRef, LobbyView)> = conns.iter().map(|(c, s)| (*c, self.lobby.view_for(*s, &blueprints, &self.catalogue, &|h| ext.get(h).copied()))).collect();
        for (conn, view) in views {
            let changed = self.peers.get(&conn).is_none_or(|p| p.last_lobby.as_ref() != Some(&view));
            if !changed { continue; }
            if let Some(p) = self.peers.get_mut(&conn) { p.last_lobby = Some(view.clone()); }
            self.send_msg(conn, &ServerMsg::Lobby(Box::new(view)));
            out.push(HostEvent::LobbyChanged);
        }
        out.dedup_by(|a, b| matches!((a, b), (HostEvent::LobbyChanged, HostEvent::LobbyChanged)));
        // A dedicated server starts on its own once the lobby can start.
        if self.options.dedicated && self.phase == SessionPhase::Lobby {
            let can = self.lobby.can_start(&blueprints, &self.catalogue);
            if can && !self.can_start_prev {
                if let Some(by) = self.lobby.members().iter().map(|m| m.slot).min() { out.push(HostEvent::StartRequested { by }); }
            }
            self.can_start_prev = can;
        }
    }

    /// What lobby views depend on, hashed: the setup (players, choices, rules), members, holds, limits,
    /// saturation and the designs accepted so far.
    fn lobby_fingerprint(&self) -> blake3::Hash {
        let mut h = blake3::Hasher::new();
        h.update(&codec::encode(self.lobby.setup()));
        for m in self.lobby.members() { h.update(&[m.slot.0, m.host as u8]); h.update(&m.client_id.to_le_bytes()); h.update(m.name.as_bytes()); }
        h.update(&[self.lobby.max_players(), self.lobby.saturated as u8, self.phase as u8]);
        h.update(&(self.lobby.held.len() as u64).to_le_bytes());
        h.update(&(self.accepted.len() as u64).to_le_bytes());
        h.update(&(self.peers.len() as u64).to_le_bytes());
        h.finalize()
    }

    fn run_panel(&mut self, now: Duration) {
        let since = now.saturating_sub(self.last_panel);
        if !(since >= PANEL_PERIOD || (self.panel_dirty && since >= PANEL_MIN_GAP)) { return; }
        self.last_panel = now;
        self.panel_dirty = false;
        let panel = self.panel_at(now);
        self.broadcast(ServerMsg::Connection(Box::new(panel)));
    }

    fn panel_at(&self, now: Duration) -> ConnectionPanel {
        let mut players = Vec::new();
        for m in self.lobby.members() {
            let peer = self.peers.values().find(|p| p.client_id == Some(m.client_id));
            let (sample, cap, quality, warning) = match peer {
                Some(p) => (p.sample, p.cap.kbps(), p.quality, p.warned),
                None => (LinkSample::default(), self.options.quality.cap_start_kbps as f32, LinkQuality::Good, None),
            };
            let ping = if sample.rtt_ms.is_finite() { sample.rtt_ms.clamp(0.0, u16::MAX as f32) as u16 } else { u16::MAX };
            let loss = if sample.loss.is_finite() { sample.loss.clamp(0.0, 1.0) } else { 1.0 };
            players.push(PlayerLink { slot: m.slot, name: m.name.clone(), host: m.host, ping_ms: if m.host { 0 } else { ping }, loss: if m.host { 0.0 } else { loss }, frames_behind: sample.frames_behind.min(u16::MAX as u32) as u16, quality, cap_kbps: cap.round().clamp(0.0, u16::MAX as f32) as u16, warning, held_s: None });
        }
        for (client_id, slot, left) in self.lobby.held_slots(now) {
            let name = self.sim.as_ref().and_then(|s| s.players().iter().find(|p| p.client_id == client_id).map(|p| p.name.clone())).unwrap_or_default();
            players.push(PlayerLink { slot, name, host: false, ping_ms: 0, loss: 0.0, frames_behind: 0, quality: LinkQuality::Poor, cap_kbps: 0, warning: None, held_s: Some(left.as_secs().min(u16::MAX as u64) as u16) });
        }
        players.sort_by_key(|p| p.slot);
        let load = self.host_load();
        ConnectionPanel { players, host_tick_hz: self.load.tick_hz(now), host_tick_p95_ms: load.tick_p95_ms, saturated: self.saturation.saturated() }
    }

    // ---- Sending -------------------------------------------------------------------------------------

    /// Queue a reliable message for a peer, counting it toward its budget and traffic.
    fn queue(&mut self, conn: ConnRef, ch: Channel, bytes: Vec<u8>) {
        let now = self.last_update;
        let Some(p) = self.peers.get_mut(&conn) else { return };
        let n = bytes.len();
        p.queued_bytes += n;
        p.interval_reliable += n;
        match ch {
            Channel::Session => p.traffic.session_bytes += n as u64,
            Channel::Events | Channel::Entity => p.traffic.event_bytes += n as u64,
            Channel::Projectile => p.traffic.projectile_bytes += n as u64,
            Channel::Bulk => p.traffic.bulk_bytes += n as u64,
            Channel::Unreliable => p.traffic.snapshot_bytes += n as u64,
        }
        p.traffic.detail.channel_bytes[ch.id() as usize] += n as u64;
        p.traffic.detail.channel_msgs[ch.id() as usize] += 1;
        p.account(now, n);
        p.outbox[slot_of(ch)].push_back(bytes);
    }

    fn send_msg(&mut self, conn: ConnRef, msg: &ServerMsg) {
        let bytes = codec::encode(msg);
        self.queue(conn, Channel::Session, bytes);
    }

    /// Hand queued messages to renet2 in order while it can take them.
    fn flush_peer(&mut self, conn: ConnRef) {
        let Some(Some(t)) = self.transports.get_mut(conn.transport as usize) else { return };
        let Some(p) = self.peers.get_mut(&conn) else { return };
        for ch in Channel::SERVER {
            let q = &mut p.outbox[slot_of(ch)];
            while let Some(front) = q.front() {
                if !t.can_send(conn.id, ch, front.len()) { break; }
                let Some(b) = q.pop_front() else { break };
                p.queued_bytes = p.queued_bytes.saturating_sub(b.len());
                t.send(conn.id, ch, b);
            }
        }
    }

    fn flush_all(&mut self, _now: Duration) {
        let conns: Vec<ConnRef> = self.peers.keys().copied().collect();
        for c in conns { self.flush_peer(c); }
        for t in self.transports.iter_mut().flatten() { t.flush(); }
    }

    // ---- Public surface ------------------------------------------------------------------------------

    pub fn phase(&self) -> SessionPhase { self.phase }
    /// Move between `Lobby`, `Theatre` and `AfterAction`, broadcasting the change. Unit: `net-server-session`.
    ///
    /// Refused while a sortie is loaded or running (`end_sortie` first). Every member gets a fresh lobby view.
    pub fn set_phase(&mut self, phase: SessionPhase) -> Result<(), NetError> {
        if matches!(phase, SessionPhase::Loading | SessionPhase::Sortie) || self.sim.is_some() { return Err(NetError::WrongPhase); }
        if phase == SessionPhase::Lobby && self.phase != SessionPhase::Lobby {
            for p in &mut self.lobby.setup_mut().players { p.ready = false; }
        }
        self.phase = phase;
        for p in self.peers.values_mut() { p.last_lobby = None; }
        self.lobby_dirty = true;
        self.last_lobby_sent = Duration::ZERO;
        self.can_start_prev = false;
        Ok(())
    }
    pub fn lobby(&self) -> &Lobby { &self.lobby }
    pub fn lobby_mut(&mut self) -> &mut Lobby { &mut self.lobby }
    pub fn store_mut(&mut self) -> &mut dyn BlueprintStore { self.store.as_mut() }

    /// Hand over a freshly built Sim: phase `Loading` then `Sortie` once every connected player has the
    /// own-side blueprints (or after a 30 s loading timeout); sends each player its `SortieIntro`, grants
    /// blueprint entitlements (own side full, other PvP team exterior), resets encoders and starts the replay
    /// sink. Unit: `net-server-session`.
    pub fn start_sortie(&mut self, sim: Sim) -> Result<(), NetError> {
        if self.shut_down || self.sim.is_some() || matches!(self.phase, SessionPhase::Loading | SessionPhase::Sortie) { return Err(NetError::WrongPhase); }
        let now = self.clock.now();
        let mut sim = sim;
        // Every design of the sortie can be served, with its exterior form (L945).
        let designs: Vec<Blueprint> = sim.config().blueprints.clone();
        for bp in designs {
            let hash = bp.header.hash;
            if hash.is_empty() { continue; }
            if !self.store.contains(&hash) {
                let packed = content::share::pack(&Shareable::Ship(Box::new(bp.clone())));
                if packed.hash == hash { self.store.put(hash, packed.compressed); }
            }
            self.remember(hash, bp);
        }
        self.entitlements.clear();
        let initial = sim.drain_events();
        self.replay = if self.options.record_replays {
            let mut r = ReplaySink::start(&sim, &self.options.game_version, RecordOptions::default());
            r.record(&sim, &initial);
            Some(r)
        } else { None };
        self.held_events = initial;
        self.sim = Some(sim);
        self.phase = SessionPhase::Loading;
        self.tick_accumulator = 0.0;
        self.pause_acc = 0.0;
        self.synthetic_debt_s = 0.0;
        self.backlog = 0;
        self.over_reported = false;
        self.sortie_steps = 0;
        let seated: Vec<ConnRef> = {
            let sim = self.sim.as_ref().expect("just set");
            self.peers.iter().filter(|(_, p)| p.admitted() && p.slot.is_some_and(|s| sim.players().iter().any(|q| q.slot == s))).map(|(c, _)| *c).collect()
        };
        for c in seated { self.begin_loading(c, now); }
        self.lobby_dirty = true;
        self.panel_dirty = true;
        Ok(())
    }
    pub fn sim(&self) -> Option<&Sim> { self.sim.as_ref() }
    pub fn sim_mut(&mut self) -> Option<&mut Sim> { self.sim.as_mut() }
    /// Take the Sim back at the end of a sortie (result readable by the app), finish the replay and return
    /// its bytes, move to `AfterAction`. Unit: `net-server-session`.
    ///
    /// A sortie still undecided is finished as `Aborted`. The last events go out and everyone seated gets
    /// `SortieClosed` with the summary.
    pub fn end_sortie(&mut self) -> Option<(Sim, Option<Vec<u8>>)> {
        self.sim.as_ref()?;
        if let Some(sim) = self.sim.as_mut() { if !sim.is_over() { sim.finish(Outcome::Aborted); } }
        let last = self.sim.as_mut().map(|s| s.drain_events()).unwrap_or_default();
        if let (Some(r), Some(sim)) = (self.replay.as_mut(), self.sim.as_ref()) { r.record(sim, &last); }
        if self.phase == SessionPhase::Sortie { self.route_events(&last, None); }
        let sim = self.sim.take()?;
        let summary = match sim.result() {
            Some(result) => sim_systems::objectives::summary(result, sim.config()),
            None => sim_systems::protocol::config::SortieSummary { outcome: Outcome::Aborted, winner: None, duration_ticks: sim.tick().0, objectives: Vec::new(), stats: Default::default(), tip: None },
        };
        self.broadcast(ServerMsg::SortieClosed { summary: Box::new(summary) });
        for p in self.peers.values_mut() { p.loaded = false; p.loading_since = None; p.loaded_report = false; }
        self.held_events.clear();
        let bytes = self.replay.take().map(|r| r.finish());
        self.phase = SessionPhase::AfterAction;
        self.tick_accumulator = 0.0;
        self.backlog = 0;
        self.lobby_dirty = true;
        self.panel_dirty = true;
        let now = self.clock.now();
        self.flush_all(now);
        Some((sim, bytes))
    }

    /// Send a session message to one player. Unit: `net-server-session`.
    pub fn send_to(&mut self, slot: PlayerSlot, msg: ServerMsg) {
        if let Some(conn) = self.conn_of(slot) { self.send_msg(conn, &msg); }
    }
    /// Send to every admitted player. Unit: `net-server-session`.
    pub fn broadcast(&mut self, msg: ServerMsg) {
        let bytes = codec::encode(&msg);
        let conns: Vec<ConnRef> = self.peers.iter().filter(|(_, p)| p.admitted()).map(|(c, _)| *c).collect();
        for c in conns { self.queue(c, Channel::Session, bytes.clone()); }
    }
    fn conn_of(&self, slot: PlayerSlot) -> Option<ConnRef> { self.peers.iter().find(|(_, p)| p.admitted() && p.slot == Some(slot)).map(|(c, _)| *c) }
    /// Queue a bulk transfer to one player or everyone (checkpoints, save copies, released replays).
    /// Unit: `net-server-session`.
    pub fn send_bulk(&mut self, to: Option<PlayerSlot>, kind: TransferKind, bytes: Vec<u8>) {
        let conns: Vec<ConnRef> = match to {
            Some(slot) => self.conn_of(slot).into_iter().collect(),
            None => self.peers.iter().filter(|(_, p)| p.admitted()).map(|(c, _)| *c).collect(),
        };
        for c in conns {
            let Some(p) = self.peers.get_mut(&c) else { continue };
            let start = p.outgoing.enqueue(kind, bytes.clone());
            self.send_msg(c, &ServerMsg::TransferAnnounce(start));
        }
    }
    /// Host command: kick, optionally ban for the session (L1003). Unit: `net-server-session`.
    pub fn kick(&mut self, slot: PlayerSlot, ban: bool) {
        let now = self.clock.now();
        let Some(client_id) = self.lobby.kick(slot, ban) else { return };
        let reason = if ban { DropReason::Banned } else { DropReason::Kicked };
        if let Some(conn) = self.peers.iter().find(|(_, p)| p.client_id == Some(client_id)).map(|(c, _)| *c) {
            self.send_msg(conn, &ServerMsg::Dropped(reason));
            self.depart(conn, reason, false, now);
            self.close(conn, now);
        }
        self.lobby_dirty = true;
        self.panel_dirty = true;
    }
    /// Host panel upload figure (L1031). Unit: `net-server-session`.
    pub fn set_upload_kbps(&mut self, kbps: u32) {
        self.options.upload_kbps = kbps;
        self.lobby.set_upload_kbps(kbps);
        self.lobby_dirty = true;
    }
    /// Ticks the host was behind real time after the last `update` (the backlog the ladder sees). Additive,
    /// for the soak's ladder controller gate (L1027: backlog at most 30 ticks, back to 3 within 5 s).
    pub fn backlog_ticks(&self) -> u32 { self.backlog }
    /// Change the injected synthetic tick cost mid-session, so the ladder gate can drop the load and watch the
    /// host return to rung 0 (L1027). Non-finite or negative values mean 0. Additive.
    pub fn set_synthetic_tick_ms(&mut self, ms: f32) {
        let ms = if ms.is_finite() { ms.max(0.0) } else { 0.0 };
        self.options.synthetic_tick_ms = ms;
        self.load.set_synthetic_ms(ms);
    }
    pub fn connection_panel(&self) -> ConnectionPanel { self.panel_at(self.clock.now()) }
    pub fn log_mut(&mut self) -> &mut ConnectionLog { &mut self.log }
    /// Gate counters: bytes sent per client by channel and frame stats since start. Unit: `net-server-session`.
    pub fn traffic(&self) -> Vec<(PlayerSlot, crate::session::server::Traffic)> {
        let mut out: Vec<(PlayerSlot, Traffic)> = self.peers.values().filter_map(|p| {
            let slot = p.slot?;
            let mut t = p.traffic;
            let mut v = p.kbps.clone();
            v.sort_by(f32::total_cmp);
            t.p95_kbps = if v.is_empty() { 0.0 } else { v[((v.len() as f64 * 0.95).ceil() as usize).clamp(1, v.len()) - 1] };
            Some((slot, t))
        }).collect();
        out.sort_by_key(|t| t.0);
        out
    }
    /// Record the exact bytes sent to one player (snapshot frames and event batches) for the cross-platform
    /// decode check (L939), from now on; `false` stops and discards. Additive: no contract name covered the
    /// tap that `replay::StreamCapture` needs.
    ///
    /// Event batches are recorded as sent (the lanes are reliable). A snapshot frame is recorded once the
    /// client's acks show it decoded it, and dropped when they show it did not (or never say): delta frames
    /// decode against the client's own history, so the capture holds exactly the frames the client decoded,
    /// in order, and decodes to the same state on any machine. Starting a capture resets the client's encoder
    /// (one full frame), so the capture needs nothing sent before it.
    pub fn capture_stream(&mut self, slot: PlayerSlot, on: bool) {
        let Some(conn) = self.conn_of(slot) else { return };
        if let Some(p) = self.peers.get_mut(&conn) {
            p.capture = if on { Some(StreamCapture { protocol: crate::WIRE_VERSION, frames: Vec::new() }) } else { None };
            p.capture_pending.clear();
            // A capture starts on a fresh epoch, so its first frames decode from nothing on any machine.
            if on { p.encoder.reset(); p.priority.clear(); }
        }
    }
    /// Take what has been captured for a player so far (capture continues).
    pub fn take_capture(&mut self, slot: PlayerSlot) -> Option<StreamCapture> {
        let conn = self.conn_of(slot)?;
        let p = self.peers.get_mut(&conn)?;
        let c = p.capture.as_mut()?;
        Some(StreamCapture { protocol: c.protocol, frames: std::mem::take(&mut c.frames) })
    }
    /// End the session: `SessionEnded(HostQuit)` to everyone, disconnect, flush. Unit: `net-server-session`.
    pub fn shutdown(&mut self) {
        if self.shut_down { return; }
        let bytes = codec::encode(&ServerMsg::SessionEnded(SessionEnd::HostQuit));
        let conns: Vec<ConnRef> = self.peers.keys().copied().collect();
        for c in &conns {
            if self.peers.get(c).is_some_and(|p| p.admitted()) { self.queue(*c, Channel::Session, bytes.clone()); }
            self.flush_peer(*c);
        }
        for t in self.transports.iter_mut().flatten() {
            t.flush();
            t.disconnect_all();
        }
        self.peers.clear();
        self.shut_down = true;
    }
}

/// Move captured frames whose fate an ack settles into the capture: decoded ones (the ack's newest, or set in
/// its mask) in order; lost ones, and ones too old for the mask, are dropped.
fn settle_capture(p: &mut Peer, ack: crate::msg::SnapshotAck) {
    let Some(c) = p.capture.as_mut() else { return };
    while let Some((seq, _, _)) = p.capture_pending.front() {
        let seq = *seq;
        if seq > ack.newest { break; }
        let back = ack.newest - seq;
        let decoded = back == 0 || (back <= 32 && ack.mask & (1u32 << (back - 1)) != 0);
        let Some((_, tick, bytes)) = p.capture_pending.pop_front() else { break };
        if decoded { c.frames.push((tick, Channel::Unreliable.id(), bytes)); }
    }
}

/// Encode events as batches of at most [`BATCH_EVENTS`] events and about [`BATCH_BYTES`] each (and always
/// under the codec's message limit); an event that alone exceeds the limit cannot be sent and is skipped.
fn split_batches(events: &[WireEvent]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for chunk in events.chunks(BATCH_EVENTS) { split_into(chunk, &mut out); }
    out
}
fn split_into(events: &[WireEvent], out: &mut Vec<Vec<u8>>) {
    if events.is_empty() { return; }
    let bytes = codec::encode_events(&EventBatch { events: events.to_vec() });
    if bytes.len() <= BATCH_BYTES || (events.len() == 1 && (bytes.len() as u64) <= codec::MAX_MESSAGE_BYTES) {
        out.push(bytes);
        return;
    }
    if events.len() == 1 { return; }
    let mid = events.len() / 2;
    split_into(&events[..mid], out);
    split_into(&events[mid..], out);
}

/// Bytes sent to one client, for the bandwidth gate (L928-933) and soak reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Traffic {
    pub snapshot_bytes: u64,
    pub vessel_bytes: u64,
    pub vessel_updates: u64,
    pub event_bytes: u64,
    pub projectile_bytes: u64,
    pub bulk_bytes: u64,
    pub session_bytes: u64,
    /// p95 of per-second totals in kbps.
    pub p95_kbps: f32,
    /// Where the bytes went (soak breakdown). Additive diagnostics; the gates read the fields above.
    #[serde(default)]
    pub detail: TrafficDetail,
}

/// Per-client byte breakdown by channel and, inside snapshot frames, by item kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrafficDetail {
    /// Bytes and messages queued per channel, by [`Channel::id`] (Session, Events, Entity, Projectile,
    /// Unreliable, Bulk).
    pub channel_bytes: [u64; 6],
    pub channel_msgs: [u64; 6],
    /// Snapshot frames sent and their bytes by part: frame header (including removals), removals alone,
    /// bodies and decoys, tracks, and each section in `delta::Section::ALL` order.
    pub frames: u64,
    pub frame_header_bytes: u64,
    pub removal_bytes: u64,
    pub body_bytes: u64,
    pub body_items: u64,
    pub track_bytes: u64,
    pub track_items: u64,
    pub section_bytes: [u64; 6],
    /// Items due that did not fit their frame's budget, and frames that deferred any.
    pub deferred_items: u64,
    pub deferred_frames: u64,
    /// Frames sent at each over-cap shedding stage (L937): none, distant tracks slow, untracked poses.
    pub shed_frames: [u64; 3],
}

#[cfg(test)]
mod tests;
