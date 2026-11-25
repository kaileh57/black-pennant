//! Unit `net-client-session` (hard: render timing, acks and loss handling). DESIGN.md L910-916 (presentation
//! prediction and interpolation), L949 (commands stamped with the client render tick), L1424 (inputs at 30 Hz,
//! each packet repeating the last 4), L1005 (no packet from the host for 10 s means host loss), L943 (blueprints
//! fetched by hash into the local cache).
//!
//! `update` does: read the clock; update the transport; on first connect send the hello; read `Session`
//! messages into [`ClientEvent`]s; read event channels in channel order and apply each event to the snapshot
//! buffer (`SnapshotBuffer::on_event`) and the shell tracker (spawn, impact, splash) before surfacing it;
//! decode snapshot frames into the buffer; at 30 Hz send an `InputPacket` with the decoder's ack and the last
//! up to 4 inputs; request missing blueprints named by reveals and the sortie intro; feed bulk chunks; flush.
//! Rendering calls [`ClientSession::sample`] once per frame; its `render_tick` stamps commands.
//!
//! # Choices this unit makes where the spec is silent (design decisions)
//! - Loading handshake: the wire has no "loaded" message, so the client reports that it holds every
//!   own-side blueprint of the sortie intro with `LobbyRequest::SetReady(true)` while the sortie is loading.
//!   The host's session reads it as "loaded" in `Loading` (and for a mid-sortie join) and only then sends
//!   that client the sortie's events and frames, so nothing of a new sortie can overtake its
//!   `SortieLoading` on another channel.
//! - `SortieLoading` is the one reset point: decoder, snapshot buffer and shells start empty there, and
//!   snapshot frames are taken only between it and `SortieClosed`.
//! - Reliable sends wait in per-channel queues while the transport cannot take them (a full reliable renet2
//!   channel would disconnect) and go out in order on later updates. Commands and requests made before the
//!   hello has gone out wait behind it.
//! - Inputs: each 30 Hz packet adds the newest input given since the last packet, stamped with an
//!   ascending input tick the session owns (the larger of the caller's and the next one), and repeats the
//!   last 4 (L1424). Packets flow from admission on, with no inputs for a spectator, so acks keep flowing.
//! - Bulk uploads use 25% of the client's own cap from the latest connection panel while a sortie runs
//!   (L943) and the transport's per-update ceiling otherwise.
//! - Blueprints: a reveal or sortie intro naming a design absent from the store is requested once; a stock
//!   class (`EntityIntro::class`) resolves locally and is never requested. Only requested designs are put
//!   into the store when they arrive (the bytes are checked against the transfer's hash on the way; the
//!   content address is checked when the ship is unpacked).
//! - End reasons: what the host said last (`SessionEnded`, `Dropped`, `Refused`) wins; otherwise a remote
//!   close is `HostQuit` and a timeout or transport failure is `HostLost`.
//! - Malformed messages from the host are skipped one by one; they never end the session.
use super::{ClientEvent, ClientOptions};
use crate::blueprints::{BlueprintStore, Completed, Incoming, Outgoing};
use crate::channel::Channel;
use crate::clock::Clock;
use crate::delta::SnapshotDecoder;
use crate::error::{DropReason, NetError, SessionEnd};
use crate::interp::{RenderFrame, ShellTracker, SnapshotBuffer};
use crate::msg::{BulkMsg, ClientMsg, Hello, InputPacket, LobbyRequest, MappingReport, ServerMsg, SortieIntro, TransferKind, Welcome, WireCommand};
use crate::schema::codec;
use crate::transport::{ClientTransport, ConnState, DisconnectCause, LinkStats};
use content::catalogue::Catalogue;
use content::ids::ContentHash;
use content::share::PackedBlueprint;
use sim_systems::protocol::command::{Command, PlayerInput};
use sim_systems::protocol::event::SimEvent;
use sim_systems::theatre::TheatreCommand;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

/// Inputs repeated in each packet (L1424).
pub const INPUTS_PER_PACKET: usize = 4;
/// Input packet rate (L1424).
pub const INPUT_HZ: u32 = sim_core::time::INPUT_HZ;
/// No packet from the host for this long is host loss (L1005).
pub const HOST_SILENCE: Duration = Duration::from_secs(10);
/// Cap assumed until the first connection panel names the client's own (the adaptive cap's start, L937).
const DEFAULT_CAP_KBPS: f32 = 256.0;
/// Bulk bytes per 30 Hz tick outside a live sortie: four chunks, well above any renet2 update allowance, so
/// the transport's own ceiling and backpressure set the pace (uncapped, L943).
const LOBBY_BULK_BYTES: usize = 4 * (crate::blueprints::CHUNK_BYTES + 64);
/// After `leave`, the link stays up at most this long so the `Leave` arrives first.
pub const LEAVE_LINGER: Duration = Duration::from_millis(500);
/// Reliable bytes a client may hold queued before it gives up on a stalled link.
const MAX_QUEUED_BYTES: usize = 32 << 20;

#[derive(Clone, Debug, PartialEq)]
pub enum ClientState { Connecting, Joined(Welcome), Refused(crate::error::JoinRefusal), Ended(crate::error::SessionEnd) }

pub struct ClientSession {
    pub(crate) options: ClientOptions,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) catalogue: Arc<Catalogue>,
    pub(crate) transport: Box<dyn ClientTransport>,
    pub(crate) state: ClientState,
    pub(crate) decoder: SnapshotDecoder,
    pub(crate) buffer: SnapshotBuffer,
    pub(crate) shells: ShellTracker,
    pub(crate) store: Box<dyn BlueprintStore>,
    pub(crate) incoming: Incoming,
    pub(crate) outgoing: Outgoing,
    pub(crate) next_seq: u32,
    pub(crate) inputs: std::collections::VecDeque<PlayerInput>,
    pub(crate) last_input_sent: Duration,
    pub(crate) last_update: Duration,
    pub(crate) requested: std::collections::BTreeSet<ContentHash>,
    /// The hello went out.
    pub(crate) hello_sent: bool,
    /// Reliable messages waiting for the transport, per channel id, oldest first.
    pub(crate) outbox: [VecDeque<Vec<u8>>; 6],
    pub(crate) queued_bytes: usize,
    /// What the host said about the end of the session, if it said anything.
    pub(crate) end_reason: Option<SessionEnd>,
    /// Newest input given since the last packet.
    pub(crate) fresh_input: Option<PlayerInput>,
    pub(crate) input_tick: u64,
    /// The running sortie's intro, from `SortieLoading` to `SortieClosed`.
    pub(crate) intro: Option<SortieIntro>,
    /// The loading report for the current intro went out.
    pub(crate) loaded_sent: bool,
    pub(crate) host_silent: bool,
    /// This client's cap from the latest connection panel.
    pub(crate) cap_kbps: f32,
    pub(crate) last_bulk: Duration,
    /// Frames, event batches and messages that failed to decode (diagnostics).
    pub(crate) malformed: u64,
    /// After `leave`: keep the link up until then so the `Leave` arrives before the disconnect.
    pub(crate) leaving_until: Option<Duration>,
    /// Designs the host refused during this sortie: never asked for again, and settled for loading.
    pub(crate) refused: std::collections::BTreeSet<ContentHash>,
}

fn channel_slot(ch: Channel) -> usize { ch.id() as usize }

impl ClientSession {
    /// Unit: `net-client-session`.
    pub fn new(options: ClientOptions, transport: Box<dyn ClientTransport>, clock: Arc<dyn Clock>, catalogue: Arc<Catalogue>, store: Box<dyn BlueprintStore>) -> ClientSession {
        let now = clock.now();
        let buffer = SnapshotBuffer::new(options.interp);
        ClientSession {
            options, clock, catalogue, transport, state: ClientState::Connecting, decoder: SnapshotDecoder::new(), buffer, shells: ShellTracker::new(), store,
            incoming: Incoming::new(), outgoing: Outgoing::new(), next_seq: 1, inputs: VecDeque::with_capacity(INPUTS_PER_PACKET), last_input_sent: now, last_update: now,
            requested: Default::default(), hello_sent: false, outbox: Default::default(), queued_bytes: 0, end_reason: None, fresh_input: None, input_tick: 0, intro: None,
            loaded_sent: false, host_silent: false, cap_kbps: DEFAULT_CAP_KBPS, last_bulk: now, malformed: 0, leaving_until: None, refused: Default::default(),
        }
    }

    /// One pass of the client loop. Unit: `net-client-session`.
    ///
    /// `Err` only when the transport's socket fails; the session then stays as it was and the caller may
    /// retry or give up. Everything the host sends that does not decode is skipped.
    pub fn update(&mut self) -> Result<Vec<ClientEvent>, NetError> {
        let now = self.clock.now();
        let dt = now.saturating_sub(self.last_update);
        self.last_update = now;
        let mut out = Vec::new();
        if let Some(until) = self.leaving_until {
            // Leaving: keep the link pumping (the `Leave` and its acks) until the host closes it or the linger
            // ends, then say goodbye. Nothing more is surfaced.
            let _ = self.transport.update(dt, self.clock.unix_now());
            let _ = self.transport.flush();
            if now >= until || matches!(self.transport.state(), ConnState::Disconnected(_)) {
                self.transport.disconnect();
                self.leaving_until = None;
            }
            return Ok(out);
        }
        if matches!(self.state, ClientState::Ended(_)) { return Ok(out); }
        self.transport.update(dt, self.clock.unix_now())?;
        match self.transport.state() {
            ConnState::Connected => {
                if !self.hello_sent { self.send_hello(); }
            }
            ConnState::Connecting => {}
            ConnState::Disconnected(cause) => {
                self.finish(cause, &mut out);
                return Ok(out);
            }
        }
        self.read_session(&mut out);
        self.read_events(&mut out);
        self.read_frames(now);
        self.read_bulk(&mut out);
        self.report_loaded();
        if matches!(self.state, ClientState::Joined(_)) {
            self.send_inputs(now);
            self.send_bulk(now);
            let silence = self.transport.time_since_last_packet();
            if silence >= HOST_SILENCE {
                if !self.host_silent { self.host_silent = true; out.push(ClientEvent::HostSilent); }
            } else {
                self.host_silent = false;
            }
        }
        if self.queued_bytes > MAX_QUEUED_BYTES {
            // The link has stalled for good: nothing queued can ever be delivered in order.
            self.transport.disconnect();
            self.finish(DisconnectCause::Error("send queue overflow".into()), &mut out);
            return Ok(out);
        }
        self.flush_outbox();
        if let Err(NetError::Io(e)) = self.transport.flush() { return Err(NetError::Io(e)); }
        Ok(out)
    }

    fn send_hello(&mut self) {
        let hello = Hello { protocol: crate::WIRE_VERSION, game_version: self.options.game_version.clone(), build: self.options.build, client_id: self.options.identity.client_id, name: self.options.identity.name.clone() };
        let bytes = codec::encode_hello(&hello);
        if !self.transport.can_send(Channel::Session, bytes.len()) { return; }
        // The hello must be the first message on `Session`: everything queued waits behind it.
        self.transport.send(Channel::Session, bytes);
        self.hello_sent = true;
    }

    /// The session ended: record the reason once and surface it.
    fn finish(&mut self, cause: DisconnectCause, out: &mut Vec<ClientEvent>) {
        if matches!(self.state, ClientState::Ended(_)) { return; }
        let reason = self.end_reason.unwrap_or(match cause {
            DisconnectCause::Remote => SessionEnd::HostQuit,
            DisconnectCause::Local => SessionEnd::Dropped(DropReason::Left),
            DisconnectCause::TimedOut | DisconnectCause::Error(_) => SessionEnd::HostLost,
        });
        self.state = ClientState::Ended(reason);
        self.outbox = Default::default();
        self.queued_bytes = 0;
        out.push(ClientEvent::Ended(reason));
    }

    fn read_session(&mut self, out: &mut Vec<ClientEvent>) {
        while let Some(bytes) = self.transport.receive(Channel::Session) {
            match codec::decode::<ServerMsg>(&bytes) {
                Ok(msg) => self.on_server_msg(msg, out),
                Err(_) => self.malformed += 1,
            }
        }
    }

    fn on_server_msg(&mut self, msg: ServerMsg, out: &mut Vec<ClientEvent>) {
        match msg {
            ServerMsg::Welcome(w) => {
                self.state = ClientState::Joined(w.clone());
                out.push(ClientEvent::Welcomed(w));
            }
            ServerMsg::Refused(r) => {
                self.state = ClientState::Refused(r.clone());
                self.end_reason = Some(SessionEnd::Refused);
                out.push(ClientEvent::Refused(r));
            }
            ServerMsg::Dropped(reason) => self.end_reason = Some(SessionEnd::Dropped(reason)),
            ServerMsg::SessionEnded(e) => self.end_reason = Some(e),
            ServerMsg::Lobby(v) => out.push(ClientEvent::Lobby(v)),
            ServerMsg::SortieLoading(intro) => {
                self.decoder.reset();
                self.buffer.clear();
                self.shells.clear();
                self.loaded_sent = false;
                self.refused.clear();
                for h in &intro.own_side_blueprints { self.request_blueprint(*h); }
                self.intro = Some((*intro).clone());
                out.push(ClientEvent::SortieLoading(intro));
            }
            ServerMsg::SortieRunning { tick } => out.push(ClientEvent::SortieRunning { tick }),
            ServerMsg::SortieClosed { summary } => {
                self.intro = None;
                out.push(ClientEvent::SortieClosed(summary));
            }
            ServerMsg::Connection(panel) => {
                if let ClientState::Joined(w) = &self.state {
                    if let Some(row) = panel.players.iter().find(|p| p.slot == w.slot) {
                        if row.cap_kbps > 0 { self.cap_kbps = row.cap_kbps as f32; }
                    }
                }
                out.push(ClientEvent::Connection(panel));
            }
            ServerMsg::Saturation { on } => out.push(ClientEvent::Saturation { on }),
            ServerMsg::Theatre(e) => out.push(ClientEvent::Theatre(e)),
            ServerMsg::TheatreRejected(r) => out.push(ClientEvent::TheatreRejected(r)),
            ServerMsg::Chart(c) => out.push(ClientEvent::Chart(c)),
            ServerMsg::Briefing(b) => out.push(ClientEvent::Briefing(b)),
            ServerMsg::AfterAction(a) => out.push(ClientEvent::AfterAction(a)),
            ServerMsg::Succession(s) => out.push(ClientEvent::Succession(s)),
            // Progress shows from the bulk lane itself.
            ServerMsg::TransferAnnounce(_) => {}
            ServerMsg::BlueprintRefused { hash, reason } => {
                self.requested.remove(&hash);
                self.refused.insert(hash);
                out.push(ClientEvent::BlueprintRefused(hash, reason));
            }
        }
    }

    fn read_events(&mut self, out: &mut Vec<ClientEvent>) {
        for ch in [Channel::Events, Channel::Entity, Channel::Projectile] {
            while let Some(bytes) = self.transport.receive(ch) {
                let Ok(batch) = codec::decode_events(&bytes) else { self.malformed += 1; continue };
                for e in batch.events {
                    crate::interp::apply_event(&mut self.buffer, &mut self.shells, &self.catalogue, &e);
                    if let SimEvent::EntityRevealed(intro) = &e.event {
                        if intro.class.is_none() { self.request_blueprint(intro.blueprint); }
                    }
                    out.push(ClientEvent::Sim(e));
                }
            }
        }
    }

    fn read_frames(&mut self, now: Duration) {
        while let Some(bytes) = self.transport.receive(Channel::Unreliable) {
            if self.intro.is_none() { continue; }
            match self.decoder.decode(&bytes) {
                Ok(Some(frame)) => self.buffer.push(frame, now),
                Ok(None) => {}
                Err(_) => self.malformed += 1,
            }
        }
    }

    fn read_bulk(&mut self, out: &mut Vec<ClientEvent>) {
        while let Some(bytes) = self.transport.receive(Channel::Bulk) {
            let Ok(msg) = codec::decode::<BulkMsg>(&bytes) else { self.malformed += 1; continue };
            let kind = match &msg { BulkMsg::Start(s) => Some(s.kind), _ => None };
            match self.incoming.on_msg(msg) {
                Ok(Some(done)) => self.on_completed(done, out),
                Ok(None) => {}
                Err(_) => {
                    // A failed blueprint may be asked for again later.
                    if let Some(TransferKind::Blueprint(h)) = kind { self.requested.remove(&h); }
                }
            }
        }
    }

    fn on_completed(&mut self, done: Completed, out: &mut Vec<ClientEvent>) {
        match done.kind {
            TransferKind::Blueprint(hash) => {
                if self.requested.remove(&hash) && !hash.is_empty() {
                    self.store.put(hash, done.bytes);
                    out.push(ClientEvent::BlueprintReady(hash));
                }
            }
            TransferKind::Checkpoint(_) => match crate::coop::decode_bundle(&done.bytes) {
                Some(b) => out.push(ClientEvent::Checkpoint(Box::new(b))),
                None => self.malformed += 1,
            },
            TransferKind::SaveCopy(_) | TransferKind::Replay => out.push(ClientEvent::BulkReceived(done)),
        }
    }

    /// While a sortie loads, tell the host once every own-side design is in the store.
    fn report_loaded(&mut self) {
        if self.loaded_sent || !matches!(self.state, ClientState::Joined(_)) { return; }
        let Some(intro) = &self.intro else { return };
        // A design the host refused will not come: loading does not wait for it.
        if intro.own_side_blueprints.iter().all(|h| h.is_empty() || self.store.contains(h) || self.refused.contains(h)) {
            self.loaded_sent = true;
            self.queue_msg(&ClientMsg::Lobby(LobbyRequest::SetReady(true)));
        }
    }

    fn send_inputs(&mut self, now: Duration) {
        let interval = Duration::from_secs_f64(1.0 / INPUT_HZ as f64);
        if now.saturating_sub(self.last_input_sent) < interval { return; }
        self.last_input_sent = if now.saturating_sub(self.last_input_sent) > interval * 3 { now } else { self.last_input_sent + interval };
        if let Some(mut input) = self.fresh_input.take() {
            self.input_tick = input.input_tick.max(self.input_tick + 1);
            input.input_tick = self.input_tick;
            self.inputs.push_back(input);
            while self.inputs.len() > INPUTS_PER_PACKET { self.inputs.pop_front(); }
        }
        let packet = InputPacket { ack: self.decoder.ack(), inputs: self.inputs.iter().copied().collect() };
        let bytes = codec::encode(&packet);
        if self.transport.can_send(Channel::Unreliable, bytes.len()) { self.transport.send(Channel::Unreliable, bytes); }
    }

    fn send_bulk(&mut self, now: Duration) {
        let interval = Duration::from_secs_f64(1.0 / INPUT_HZ as f64);
        if now.saturating_sub(self.last_bulk) < interval { return; }
        self.last_bulk = now;
        if self.outgoing.is_idle() { return; }
        let budget = if self.intro.is_some() {
            let interval_bytes = (self.cap_kbps.max(0.0) * 1000.0 / 8.0 / INPUT_HZ as f32) as usize;
            crate::blueprints::bulk_budget(interval_bytes, true)
        } else { LOBBY_BULK_BYTES };
        for msg in self.outgoing.take(budget) {
            let bytes = codec::encode(&msg);
            self.queue(Channel::Bulk, bytes);
        }
    }

    fn queue(&mut self, ch: Channel, bytes: Vec<u8>) {
        self.queued_bytes += bytes.len();
        self.outbox[channel_slot(ch)].push_back(bytes);
    }
    fn queue_msg(&mut self, msg: &ClientMsg) { let b = codec::encode(msg); self.queue(Channel::Session, b); }

    /// Send queued reliable messages, oldest first per channel, while the transport takes them.
    fn flush_outbox(&mut self) {
        if !self.hello_sent { return; }
        for ch in Channel::CLIENT {
            let q = &mut self.outbox[channel_slot(ch)];
            while let Some(front) = q.front() {
                if !self.transport.can_send(ch, front.len()) { break; }
                let Some(bytes) = q.pop_front() else { break };
                self.queued_bytes = self.queued_bytes.saturating_sub(bytes.len());
                self.transport.send(ch, bytes);
            }
        }
    }

    pub fn state(&self) -> &ClientState { &self.state }
    /// Send a command; returns its sequence number. Unit: `net-client-session`.
    ///
    /// Sequence numbers ascend for the life of the session; the host maps them onto the slot's own sequence
    /// so a rejoining client never collides with what its slot sent before.
    pub fn send_command(&mut self, command: Command, client_render_tick: u64) -> u32 {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1).max(1);
        self.queue_msg(&ClientMsg::Command(WireCommand { seq, client_render_tick, command }));
        seq
    }
    /// Latest continuous input (sent at 30 Hz with the 3 before it). Unit: `net-client-session`.
    pub fn set_input(&mut self, input: PlayerInput) { self.fresh_input = Some(input); }
    /// Unit: `net-client-session`.
    pub fn lobby(&mut self, request: LobbyRequest) { self.queue_msg(&ClientMsg::Lobby(request)); }
    /// Unit: `net-client-session`.
    pub fn theatre(&mut self, command: TheatreCommand) { self.queue_msg(&ClientMsg::Theatre(command)); }
    /// Upload a design for the lobby (uncapped in the lobby, 25% of the cap in a live sandbox). Unit: `net-client-session`.
    ///
    /// The design also goes into the local store, so the client never asks the host for its own design.
    pub fn upload_blueprint(&mut self, packed: PackedBlueprint) {
        if packed.hash.is_empty() || packed.compressed.len() > content::blueprint::MAX_COMPRESSED_BYTES { return; }
        if !self.store.contains(&packed.hash) { self.store.put(packed.hash, packed.compressed.clone()); }
        self.outgoing.enqueue(TransferKind::Blueprint(packed.hash), packed.compressed);
    }
    /// Unit: `net-client-session`.
    ///
    /// Nothing is sent for the empty hash, a design already in the store, one already asked for, or one the
    /// host refused during this sortie.
    pub fn request_blueprint(&mut self, hash: ContentHash) {
        if hash.is_empty() || self.store.contains(&hash) || self.refused.contains(&hash) || !self.requested.insert(hash) { return; }
        self.queue_msg(&ClientMsg::RequestBlueprint(hash));
    }
    /// Unit: `net-client-session`.
    pub fn report_mapping(&mut self, report: MappingReport) { self.queue_msg(&ClientMsg::Mapping(report)); }
    /// Unit: `net-client-session`.
    pub fn request_replay(&mut self) { self.queue_msg(&ClientMsg::RequestReplay); }
    /// Interpolated state for this render frame (also advances client shells). Unit: `net-client-session`.
    ///
    /// Shells integrate to the floor of the render tick, never past the newest frame received.
    pub fn sample(&mut self) -> Option<RenderFrame> {
        let now = self.clock.now();
        let frame = self.buffer.sample(now)?;
        let newest = self.buffer.newest_tick().unwrap_or(0);
        let r = if frame.render_tick.is_finite() { frame.render_tick.max(0.0).floor() as u64 } else { 0 };
        self.shells.advance_to(r.min(newest));
        Some(frame)
    }
    pub fn buffer(&self) -> &SnapshotBuffer { &self.buffer }
    pub fn shells(&self) -> &ShellTracker { &self.shells }
    pub fn store_mut(&mut self) -> &mut dyn BlueprintStore { self.store.as_mut() }
    pub fn link(&self) -> LinkStats { self.transport.stats() }
    /// Leave politely: `ClientMsg::Leave`, flush, disconnect. Unit: `net-client-session`.
    ///
    /// The state is `Ended(Dropped(Left))` at once. renet2 discards what a connection still had buffered when
    /// it is closed, so the disconnect waits: later `update` calls keep the link up for up to
    /// [`LEAVE_LINGER`] (or until the host closes it) and then disconnect; dropping the session disconnects
    /// at once.
    pub fn leave(&mut self) {
        if matches!(self.state, ClientState::Ended(_)) { return; }
        self.state = ClientState::Ended(SessionEnd::Dropped(DropReason::Left));
        if self.hello_sent && matches!(self.transport.state(), ConnState::Connected) {
            self.queue_msg(&ClientMsg::Leave);
            self.flush_outbox();
            let _ = self.transport.flush();
            self.leaving_until = Some(self.clock.now() + LEAVE_LINGER);
        } else {
            self.transport.disconnect();
        }
        self.outbox = Default::default();
        self.queued_bytes = 0;
    }
}

impl Drop for ClientSession {
    /// A session dropped while leaving still says goodbye to the host.
    fn drop(&mut self) {
        if self.leaving_until.is_some() { self.transport.disconnect(); }
    }
}
