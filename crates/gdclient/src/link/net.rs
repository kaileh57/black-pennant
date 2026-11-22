//! Unit `gd-link-net`. The session seams over the `net` crate: in-memory transport for single-player and the
//! host's own client, UDP for listen hosts and joins.
//! DESIGN.md L899 (same program, server in-process over loopback), L982-990 (transports), L992-1007 (hosting,
//! port mapping, join codes, identity, host loss), L1031 (host upstream), L1033-1039 (connection quality).
//! The Steam backend is reached only through net's `steam` feature (L1009-1011); nothing here names Steam.
//!
//! This unit adapts; it adds nothing to `net` and re-implements nothing from it. Everything below is built
//! from `net`'s public API as it stands:
//! | Here | `net` |
//! | [`loopback`] | `session::local::local_pair` |
//! | [`join_udp`] | `transport::netcode::NetcodeClientBackend::connect_udp`, `ClientSession::new` |
//! | [`NetLink`] (`ClientLink`) | `ClientSession`: `update`, `sample`, `shells`, `buffer`, `link`, `state`, `store_mut`, the send methods, `leave` |
//! | [`NetHost`] (`HostPort`) | `ServerSession`: every method of the trait names its call; UDP through `NetcodeServerBackend::bind_udp`, `add_transport`, `remove_transport` |
//! | [`HostingHandle`] | `join::{LobbySecret, JoinCode}`, `portmap::{PortMapper, local_addresses}` |
//! | [`parse_join_code`], [`parse_address`] | `join::JoinCode::decode`, `join::parse_join_input` |
//! | the `*_msg` functions | `error::{JoinRefusal, DropReason, SessionEnd, JoinCodeError}`, `portmap::MappingOutcome` as text keys |
//!
//! Blueprint stores: every client end uses `net::blueprints::DiskCache` under `user://blueprint_cache`
//! (`CLIENT_CACHE_BYTES`, 512 MiB, L943), which is also where replays later find the designs they name by
//! hash. A host end uses `MemoryStore`. [`loopback`] seeds both stores with the designs the caller already
//! holds, so a local session does not stream them to itself.
//!
//! # Decisions where the contract is silent
//! - The UDP backend is always bound for [`MAX_UDP_CLIENTS`] connections, whatever the lobby's current player
//!   limit. The limit moves with the mode and the upload figure after the bind, and a typed refusal
//!   (`LobbyFull`, `Banned`, `HostSaturated`) can only reach a client whose connection the transport has
//!   accepted, so admission is left to the lobby (`net::lobby::Lobby::admit`).
//! - A host with no LAN address at all puts the loopback address in its join code, so a second copy of the
//!   game on the same machine can still join; with mapping it is the mapped external address, otherwise the
//!   first LAN address (default route first, IPv4 before IPv6).
//! - Joining by address takes a numeric address only (`192.168.1.20:47610`, `[fd00::2]:47610` or a bare
//!   address for port 47610). A host name would need a DNS lookup, which can block for seconds and does not
//!   belong on the main thread.
//! - Local addresses are re-read at most every [`ADDRESS_REFRESH`], so a VPN that starts after hosting shows
//!   up without the interface scan running every frame.
//! - Addresses on the panel are written as `address:port` (IPv6 in brackets), ready to type into the join
//!   field.
//! - Closing a remote link sends the polite `Leave` and keeps pumping the client for two round trips plus
//!   50 ms (at least 50 ms, at most 300 ms) before it is dropped: a disconnect that overtakes the `Leave`
//!   would make a polite leave look like a drop (and hold the slot). A local link closes at once.
use crate::enums::PortMapState;
use crate::seams::{
    ClientLink, DropReason, HostPort, InterpTuning, JoinCodeError, JoinRefusal, NetError, SessionEnd, UdpHostOptions,
};
use content::catalogue::Catalogue;
use content::ids::PlayerSlot;
use glam::DVec3;
use net::blueprints::{BlueprintStore, DiskCache, MemoryStore, CLIENT_CACHE_BYTES};
use net::clock::Clock;
use net::join::{ClientIdentity, JoinCode, JoinTarget, LobbySecret};
use net::portmap::{LocalAddrs, MappingOutcome, PortMapOptions, PortMapper};
use net::session::client::{ClientSession, ClientState};
use net::session::server::ServerSession;
use net::session::{ClientOptions, ServerOptions};
use net::transport::netcode::{NetcodeClientBackend, NetcodeServerBackend};
use net::transport::ServerTransport;
use sim_core::projectiles::ProjectileId;
use sim_systems::modes::LobbySetup;
use sim_systems::protocol::{Msg, MsgArg};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Connections the UDP backend accepts: the largest mode's player count (8, sandbox and 4v4) plus room for
/// joins the lobby refuses with a reason, so a full lobby says "full" instead of timing out.
pub const MAX_UDP_CLIENTS: usize = 16;
/// How often [`HostingHandle::poll`] re-reads the machine's interface addresses.
pub const ADDRESS_REFRESH: Duration = Duration::from_secs(5);

/// Interpolation tuning for remote vessels replicated at `vessel_hz` (DESIGN.md L914-916, L935): 30 Hz or
/// more keeps the default 4 intervals at 30 Hz (133 ms); a slower rate uses 3 of its own intervals, which at
/// 20 Hz is fallback 1's 3 x 50 ms = 150 ms. Delaying by 30 Hz intervals while vessels arrive at 20 Hz
/// leaves 133 ms for 50 ms gaps, and a lost update runs the buffer dry, so remote ships stutter.
pub fn interp_for_vessel_hz(vessel_hz: u32) -> InterpTuning {
    if vessel_hz >= 30 { return InterpTuning::default(); }
    InterpTuning { interval_s: 1.0 / vessel_hz.max(1) as f64, delay_intervals: 3.0, ..InterpTuning::default() }
}

/// The tuning for the vessel rate every host this build talks to sends: `ServerOptions::new`'s rates, which
/// the in-process host, a hosted lobby and the dedicated server all use (fallback 1 of L935 is on: 20 Hz).
pub fn session_interp() -> InterpTuning {
    interp_for_vessel_hz(ServerOptions::new(env!("CARGO_PKG_VERSION"), content::enums::GameMode::Sandbox).rates.other_vessel_hz)
}

/// What every link built here needs.
#[derive(Clone)]
pub struct LinkOptions {
    /// The install's id (`BpApp::client_id`, kept in `user://identity.ron`) and the display name from the
    /// settings (`ClientSettings::display_name`).
    pub identity: ClientIdentity,
    /// `env!("CARGO_PKG_VERSION")` of gdclient, shown on a version mismatch.
    pub game_version: String,
    /// `BuildKind::Steam` only in a build with the `steam` feature; `Dev` in debug builds.
    pub build: net::msg::BuildKind,
    /// Interpolation delay and fallbacks: [`session_interp`], from the vessel rate the host sends.
    pub interp: InterpTuning,
    /// `net::clock::SystemClock` in play. Benchmark and capture runs pass a `ManualClock` and advance it one
    /// tick per rendered frame (`session::host::SortieOptions::fixed_step`).
    pub clock: Arc<dyn Clock>,
    pub catalogue: Arc<Catalogue>,
}

/// The client seam over a `net::ClientSession`. Main thread only.
pub struct NetLink {
    pub(crate) session: ClientSession,
    /// Floor of the last sampled `render_tick`, stamped on commands (L949).
    pub(crate) render_tick: u64,
    pub(crate) closed: bool,
    /// The transport's socket failed: the link reads as `Ended(HostLost)` from then on.
    pub(crate) failed: bool,
    /// Reused output of `ShellTracker::render_positions`.
    pub(crate) scratch: Vec<(ProjectileId, DVec3, DVec3)>,
    /// Joined over the network (not the in-memory pair of a local session).
    pub(crate) remote: bool,
}

/// How long [`NetLink::close`] keeps a remote link pumping after the `Leave`, so the host reads it before
/// the disconnect (it is lost otherwise, and a polite leave would look like a drop): two round trips plus
/// three host passes, within these bounds.
const CLOSE_LINGER_MIN: Duration = Duration::from_millis(50);
const CLOSE_LINGER_MAX: Duration = Duration::from_millis(300);
const CLOSE_STEP: Duration = Duration::from_millis(5);

/// The host seam over a `net::ServerSession`. Lives on the clock thread once handed to `HostHandle::spawn`.
pub struct NetHost {
    pub(crate) session: ServerSession,
    pub(crate) clock: Arc<dyn Clock>,
    /// Index `add_transport` returned for the UDP backend, while hosting over UDP.
    pub(crate) udp: Option<u8>,
    /// Host events of passes run inside `set_udp`, returned by the next `update`.
    pub(crate) pending: Vec<crate::seams::HostEvent>,
}

/// How long the old UDP backend keeps running after its members are told a new secret drops them, so the
/// reason arrives before the disconnect (a client cannot read a message once its link has closed).
const REPLACE_LINGER: Duration = Duration::from_millis(250);
const LINGER_STEP: Duration = Duration::from_millis(10);

/// Single-player and the host's own client: `net::session::local::local_pair` with
/// `ServerOptions::new(game_version, setup.mode)` adjusted to `max_catch_up_ticks = 4` (L1363) and
/// `record_replays = record_replays`, the given lobby `setup` (one player for an offline session), a
/// `MemoryStore` for the host end and the disk cache at `cache_dir` for the client end, `options.clock` on
/// both ends and `pool` as the sim pool. `blueprints` (the local player's design and any other the caller
/// holds) are put into both stores before the first update; anything else travels over the in-memory `Bulk`
/// channel as on a remote link. If the cache directory cannot be opened the client end falls back to a
/// `MemoryStore`. The client is admitted as slot 0 with `host = true` on the first update of each end.
pub fn loopback(options: &LinkOptions, setup: LobbySetup, upload_kbps: u32, record_replays: bool, pool: Arc<rayon::ThreadPool>, cache_dir: std::path::PathBuf, blueprints: &[content::share::PackedBlueprint]) -> (NetHost, NetLink) {
    let name = options.identity.name.clone();
    loopback_named(options, &name, setup, upload_kbps, record_replays, pool, cache_dir, blueprints)
}

/// [`loopback`] with the lobby's name (what `Welcome::lobby_name` and `LobbyView::name` carry); `loopback`
/// names the lobby after the local player.
#[allow(clippy::too_many_arguments)]
pub(crate) fn loopback_named(options: &LinkOptions, lobby_name: &str, setup: LobbySetup, upload_kbps: u32, record_replays: bool, pool: Arc<rayon::ThreadPool>, cache_dir: std::path::PathBuf, blueprints: &[content::share::PackedBlueprint]) -> (NetHost, NetLink) {
    let mut server = ServerOptions::new(&options.game_version, setup.mode);
    server.max_catch_up_ticks = crate::session::host::MAX_CATCH_UP_TICKS;
    server.record_replays = record_replays;
    server.upload_kbps = upload_kbps.max(1);
    server.lobby_name = clean_text(lobby_name);
    let mut client = client_options(options);
    // The own client interpolates at the rate this server sends other vessels, whatever the caller passed.
    client.interp = interp_for_vessel_hz(server.rates.other_vessel_hz);
    let mut host_store: Box<dyn BlueprintStore> = Box::new(MemoryStore::default());
    let mut client_store = client_store(cache_dir);
    for bp in blueprints {
        if bp.hash.is_empty() || bp.compressed.len() > content::blueprint::MAX_COMPRESSED_BYTES { continue; }
        host_store.put(bp.hash, bp.compressed.clone());
        if !client_store.contains(&bp.hash) { client_store.put(bp.hash, bp.compressed.clone()); }
    }
    let (session, link) = net::session::local::local_pair(server, setup, client, options.clock.clone(), pool, options.catalogue.clone(), host_store, client_store);
    (NetHost { session, clock: options.clock.clone(), udp: None, pending: Vec::new() }, NetLink::new(link))
}

/// Connect outbound to a host: `NetcodeClientBackend::connect_udp(target.addr(), &target.secret(),
/// identity.client_id, clock.unix_now(), None)`, then `ClientSession::new` with the disk blueprint cache at
/// `cache_dir`. Progress arrives through `status()` (`Connecting`, then `Joined` or `Refused`) and
/// `ClientEvent::Welcomed` or `ClientEvent::Refused`. An unreachable host ends as
/// `ClientEvent::Ended(SessionEnd::HostLost)` after the netcode timeout.
pub fn join_udp(options: &LinkOptions, target: &JoinTarget, cache_dir: std::path::PathBuf) -> Result<NetLink, NetError> {
    let addr = target.addr();
    let conditioning = test_link(&options.clock, options.identity.client_id);
    let transport = NetcodeClientBackend::connect_udp(addr, &target.secret(), options.identity.client_id, options.clock.unix_now(), conditioning)?;
    let session = ClientSession::new(client_options(options), Box::new(transport), options.clock.clone(), options.catalogue.clone(), client_store(cache_dir));
    let mut link = NetLink::new(session);
    link.remote = true;
    Ok(link)
}

/// The link conditioner for a playtest over real UDP: `--link=<rtt ms>,<loss %>[,<jitter ms>]` after `--` on
/// the game's command line (for example `--link=100,2`). Each end conditions only what it sends (L990), so
/// each end delays its sends by half the round trip and loses the whole loss figure; run both copies with the same flag. Never
/// on unless the flag is given; the flag has no settings entry and is not shown to players.
pub fn test_link(clock: &Arc<dyn Clock>, seed: u64) -> Option<net::transport::conditioner::Conditioning> {
    let arg = std::env::args().find_map(|a| a.strip_prefix("--link=").map(str::to_string))?;
    let profile = parse_test_link(&arg)?;
    Some(net::transport::conditioner::Conditioning { profile, seed: seed ^ 0x6c69_6e6b, clock: clock.clone() })
}

/// `<rtt ms>,<loss %>[,<jitter ms>]` as a one-way conditioner profile.
pub(crate) fn parse_test_link(arg: &str) -> Option<net::transport::conditioner::ConditionerProfile> {
    let v: Vec<f32> = arg.split(',').map(|p| p.trim().parse::<f32>()).collect::<Result<_, _>>().ok()?;
    let (rtt, loss, jitter) = match v.as_slice() { [r, l] => (*r, *l, 0.0), [r, l, j] => (*r, *l, *j), _ => return None };
    if !(rtt.is_finite() && loss.is_finite() && jitter.is_finite()) || rtt < 0.0 || !(0.0..=100.0).contains(&loss) || jitter < 0.0 { return None; }
    Some(net::transport::conditioner::ConditionerProfile::round_trip(rtt, loss / 100.0, jitter))
}

fn client_options(options: &LinkOptions) -> ClientOptions {
    ClientOptions {
        identity: ClientIdentity { client_id: options.identity.client_id, name: clean_text(&options.identity.name) },
        game_version: options.game_version.clone(),
        build: options.build,
        interp: options.interp,
    }
}

/// The disk cache, or memory when the folder cannot be opened.
fn client_store(cache_dir: std::path::PathBuf) -> Box<dyn BlueprintStore> {
    match DiskCache::open(cache_dir, CLIENT_CACHE_BYTES) {
        Ok(cache) => Box::new(cache),
        Err(_) => Box::new(MemoryStore::default()),
    }
}

/// Player text as it may go on the wire: no control characters, trimmed, at most 24 characters (the host
/// trims to the same length).
fn clean_text(raw: &str) -> String {
    let s: String = raw.chars().filter(|c| !c.is_control()).collect();
    let s: String = s.trim().chars().take(net::lobby::MAX_NAME_CHARS).collect();
    s.trim_end().to_string()
}

/// Decode a join code (`JoinCode::decode`): format version, protocol version, address, port, lobby secret,
/// checksum. A code for another protocol version is `JoinCodeError::ProtocolMismatch`, refused before
/// connecting with both versions named (L1003).
pub fn parse_join_code(code: &str) -> Result<JoinCode, JoinCodeError> {
    let code = code.trim();
    if code.is_empty() { return Err(JoinCodeError::WrongLength); }
    JoinCode::decode(code)
}
/// Build a target from `address:port` and the 16-character lobby key (`net::join::parse_join_input` on the
/// two joined by a space). A missing port means `net::DEFAULT_PORT`.
pub fn parse_address(address: &str, lobby_key: &str) -> Result<JoinTarget, JoinCodeError> {
    let address = address.trim();
    if address.is_empty() || address.chars().any(char::is_whitespace) || !is_numeric_address(address) { return Err(JoinCodeError::BadAddress); }
    let key: String = lobby_key.chars().filter(|c| !c.is_whitespace()).collect();
    if key.is_empty() { return Err(JoinCodeError::WrongLength); }
    match net::join::parse_join_input(&format!("{address} {key}"))? {
        target @ JoinTarget::Address { .. } => Ok(target),
        JoinTarget::Code(_) => Err(JoinCodeError::BadAddress),
    }
}

/// `address:port`, `[v6]:port`, a bare address or a bracketed one, as `net::join` reads them.
fn is_numeric_address(text: &str) -> bool {
    if text.parse::<SocketAddr>().is_ok() { return true; }
    let bare = text.strip_prefix('[').and_then(|t| t.strip_suffix(']')).unwrap_or(text);
    bare.parse::<IpAddr>().is_ok()
}

impl NetLink {
    fn new(session: ClientSession) -> NetLink { NetLink { session, render_tick: 0, closed: false, failed: false, scratch: Vec::new(), remote: false } }
    fn state(&self) -> crate::seams::LinkState {
        use crate::seams::LinkState;
        if self.closed { return LinkState::Closed; }
        if self.failed { return LinkState::Ended(SessionEnd::HostLost); }
        match self.session.state() {
            ClientState::Connecting => LinkState::Connecting,
            ClientState::Joined(_) => LinkState::Joined,
            ClientState::Refused(r) => LinkState::Refused(r.clone()),
            ClientState::Ended(e) => LinkState::Ended(*e),
        }
    }
}

impl ClientLink for NetLink {
    fn poll(&mut self, _now_s: f64, inbox: &mut crate::seams::LinkInbox) {
        if self.closed || self.failed { return; }
        match self.session.update() {
            Ok(events) => {
                for e in events {
                    match e {
                        crate::seams::ClientEvent::Sim(w) => inbox.events.push(w),
                        other => inbox.session.push(other),
                    }
                }
            }
            Err(_) => {
                // The socket failed: the session is over for this end, said once.
                let ended = matches!(self.session.state(), ClientState::Ended(_));
                self.failed = true;
                if !ended { inbox.session.push(crate::seams::ClientEvent::Ended(SessionEnd::HostLost)); }
                return;
            }
        }
        let frame = self.session.sample();
        inbox.newest_tick = self.session.buffer().newest_tick();
        inbox.shells.clear();
        if let Some(f) = &frame {
            if f.render_tick.is_finite() && f.render_tick >= 0.0 {
                self.render_tick = f.render_tick.floor() as u64;
                self.session.shells().render_positions(f.render_tick, &mut self.scratch);
                inbox.shells.extend(self.scratch.iter().map(|&(id, pos, vel)| crate::seams::ShellPose { id, pos, vel }));
            }
        }
        inbox.frame = frame;
    }
    fn send(&mut self, msg: crate::seams::ClientMsg) {
        use crate::seams::ClientMsg as M;
        if self.state() != crate::seams::LinkState::Joined { return; }
        let s = &mut self.session;
        match msg {
            M::Command(c) => { s.send_command(c, self.render_tick); }
            M::Input(i) => s.set_input(i),
            M::Lobby(r) => s.lobby(r),
            M::Theatre(t) => s.theatre(t),
            M::UploadBlueprint(p) => s.upload_blueprint(p),
            M::RequestBlueprint(h) => s.request_blueprint(h),
            M::Mapping(m) => s.report_mapping(m),
            M::RequestReplay => s.request_replay(),
        }
    }
    fn status(&self) -> crate::seams::LinkStatus {
        crate::seams::LinkStatus { state: self.state(), link: self.session.link(), interp: self.session.buffer().stats() }
    }
    fn blueprint(&mut self, hash: &content::ids::ContentHash) -> Option<content::share::PackedBlueprint> {
        if hash.is_empty() { return None; }
        self.session.store_mut().get(hash).map(|compressed| content::share::PackedBlueprint { hash: *hash, compressed })
    }
    fn close(&mut self) {
        if self.closed { return; }
        self.closed = true;
        if self.failed { return; }
        let joined = matches!(self.session.state(), ClientState::Joined(_));
        let rtt = self.session.link().rtt_ms;
        self.session.leave();
        if !self.remote || !joined {
            let _ = self.session.update();
            return;
        }
        // Keep the link up until the host has read the `Leave`; dropping the session disconnects at once.
        let rtt = if rtt.is_finite() { Duration::from_secs_f32(rtt.clamp(0.0, 1_000.0) / 1_000.0) } else { Duration::ZERO };
        let linger = (rtt * 2 + Duration::from_millis(50)).clamp(CLOSE_LINGER_MIN, CLOSE_LINGER_MAX);
        let until = Instant::now() + linger;
        while Instant::now() < until {
            let _ = self.session.update();
            std::thread::sleep(CLOSE_STEP);
        }
    }
}

impl HostPort for NetHost {
    fn update(&mut self, events: &mut Vec<crate::seams::HostEvent>) -> Result<(), NetError> {
        events.append(&mut self.pending);
        events.extend(self.session.update()?);
        Ok(())
    }
    fn phase(&self) -> crate::seams::SessionPhase { self.session.phase() }
    fn set_phase(&mut self, phase: crate::seams::SessionPhase) -> Result<(), NetError> { self.session.set_phase(phase) }
    fn start_sortie(&mut self, sim: sim_systems::Sim) -> Result<(), NetError> { self.session.start_sortie(sim) }
    fn sim(&self) -> Option<&sim_systems::Sim> { self.session.sim() }
    fn sim_mut(&mut self) -> Option<&mut sim_systems::Sim> { self.session.sim_mut() }
    fn end_sortie(&mut self) -> Option<(sim_systems::Sim, Option<Vec<u8>>)> { self.session.end_sortie() }
    fn send(&mut self, to: Option<PlayerSlot>, msg: crate::seams::ServerMsg) {
        match to {
            Some(slot) => self.session.send_to(slot, msg),
            None => self.session.broadcast(msg),
        }
    }
    fn send_bulk(&mut self, to: Option<PlayerSlot>, kind: crate::seams::TransferKind, bytes: Vec<u8>) {
        if bytes.len() as u64 > net::blueprints::max_transfer_bytes(kind) as u64 { return; }
        self.session.send_bulk(to, kind, bytes);
    }
    fn lobby_setup(&self) -> LobbySetup { self.session.lobby().setup().clone() }
    fn blueprint(&mut self, hash: &content::ids::ContentHash) -> Option<Vec<u8>> {
        if hash.is_empty() { return None; }
        self.session.store_mut().get(hash)
    }
    fn put_blueprint(&mut self, packed: content::share::PackedBlueprint) {
        if packed.hash.is_empty() || packed.compressed.len() > content::blueprint::MAX_COMPRESSED_BYTES { return; }
        if !self.session.store_mut().contains(&packed.hash) { self.session.store_mut().put(packed.hash, packed.compressed); }
    }
    fn kick(&mut self, slot: PlayerSlot, ban: bool) { self.session.kick(slot, ban); }
    fn set_upload_kbps(&mut self, kbps: u32) { self.session.set_upload_kbps(kbps.max(1)); }
    fn set_udp(&mut self, options: Option<UdpHostOptions>, reason: DropReason) -> Result<Vec<std::net::SocketAddr>, NetError> {
        if let Some(index) = self.udp.take() {
            if reason == DropReason::SecretRegenerated { self.tell_udp_members(reason); }
            self.session.remove_transport(index, reason);
        }
        let Some(mut options) = options else { return Ok(Vec::new()) };
        options.max_clients = options.max_clients.max(MAX_UDP_CLIENTS);
        let backend = NetcodeServerBackend::bind_udp(&options, self.clock.unix_now(), test_link(&self.clock, 0))?;
        let mut addrs = backend.local_addrs();
        if addrs.is_empty() { addrs.push(options.bind); }
        self.udp = Some(self.session.add_transport(Box::new(backend)));
        Ok(addrs)
    }
    fn connection_panel(&self) -> crate::seams::ConnectionPanel { self.session.connection_panel() }
    fn drain_log(&mut self) -> String { self.session.log_mut().take_unflushed_ron() }
    fn shutdown(&mut self) { self.session.shutdown(); }
}

impl NetHost {
    /// Every connected member except the host's own client is on UDP in a listen session: tell each why it
    /// is about to drop, and keep the session running for [`REPLACE_LINGER`] so the message gets there. The
    /// passes are the host loop itself (the sortie keeps stepping); their events wait in `pending`.
    fn tell_udp_members(&mut self, reason: DropReason) {
        let members: Vec<PlayerSlot> = self.session.connection_panel().players.iter().filter(|p| !p.host && p.held_s.is_none()).map(|p| p.slot).collect();
        if members.is_empty() { return; }
        for slot in members { self.session.send_to(slot, crate::seams::ServerMsg::Dropped(reason)); }
        let deadline = Instant::now() + REPLACE_LINGER;
        loop {
            match self.session.update() {
                Ok(events) => self.pending.extend(events),
                Err(_) => return,
            }
            if Instant::now() >= deadline { return; }
            std::thread::sleep(LINGER_STEP);
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Hosting state on the main thread (L992-1007)
// ---------------------------------------------------------------------------------------------------------

/// What a host shows on its panel (L994-1003): the data behind `HostPanelDict`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostingInfo {
    pub port: u16,
    /// True once the clock thread reported the UDP port bound (`HostNote::UdpBound`).
    pub bound: bool,
    pub portmap: PortMapState,
    /// The mapper's latest outcome, for `portmap_text` and the manual steps.
    pub outcome: Option<MappingOutcome>,
    /// Mapped external address, empty until mapping succeeds without double NAT.
    pub external_address: String,
    /// First LAN address of `LocalAddrs::lan`, empty if none.
    pub lan_address: String,
    /// `JoinCode::encode` for the external address (or the LAN address until mapping succeeds): Crockford
    /// base32 in groups of 4, about 34 characters for IPv4.
    pub join_code: String,
    /// `LobbySecret::lobby_key`: 16 characters, for join-by-address.
    pub lobby_key: String,
    /// Address on a running mesh VPN (Tailscale, ZeroTier), if one is found (L999), and its join code.
    pub vpn_address: String,
    pub vpn_join_code: String,
    pub upload_kbps: u32,
    pub saturated: bool,
    pub banned: u32,
}

/// Main-thread handle to hosting state that is not part of the session: the lobby secret, port mapping,
/// local addresses, codes. It owns no socket; the UDP backend is bound and held by the clock thread
/// (`HostControl::SetUdp` to `HostPort::set_udp`). `BpLobby` owns it through `session::Connection::hosting`.
pub struct HostingHandle {
    pub(crate) port: u16,
    pub(crate) secret: LobbySecret,
    pub(crate) mapper: Option<PortMapper>,
    pub(crate) local: LocalAddrs,
    pub(crate) info: HostingInfo,
    /// When `local` was last read.
    pub(crate) local_at: Instant,
    /// A test host on this machine only: bound to the loopback address, no port mapping.
    pub(crate) loopback_only: bool,
}

impl HostingHandle {
    /// The player chose to host (L994): generate a `LobbySecret`, read `portmap::local_addresses()`, start
    /// `PortMapper::start(PortMapOptions::new(port))` in the background. Returns the handle and the options
    /// to send to the clock thread as `HostControl::SetUdp { options: Some(..), reason: DropReason::Left }`
    /// (bind `0.0.0.0:port`, this secret, `max_clients` from the lobby's player limit).
    pub fn start(port: u16, upload_kbps: u32, max_clients: usize) -> (HostingHandle, UdpHostOptions) {
        let port = if port == 0 { net::DEFAULT_PORT } else { port };
        let mut handle = HostingHandle::new(port, upload_kbps, LobbySecret::generate(), net::portmap::local_addresses());
        handle.mapper = Some(PortMapper::start(PortMapOptions::new(port)));
        let options = handle.udp_options(max_clients);
        (handle, options)
    }
    /// A host reachable from this machine only (`--loopback-host` on the command line, for two-process
    /// tests): binds `127.0.0.1:port`, maps nothing, and its codes name the loopback address.
    pub(crate) fn start_loopback(port: u16, upload_kbps: u32, max_clients: usize) -> (HostingHandle, UdpHostOptions) {
        let port = if port == 0 { net::DEFAULT_PORT } else { port };
        let mut handle = HostingHandle::new(port, upload_kbps, LobbySecret::generate(), LocalAddrs::default());
        handle.loopback_only = true;
        handle.refresh();
        let options = handle.udp_options(max_clients);
        (handle, options)
    }
    /// A handle with no mapper (tests drive the outcome by hand).
    fn new(port: u16, upload_kbps: u32, secret: LobbySecret, local: LocalAddrs) -> HostingHandle {
        let info = HostingInfo { port, portmap: PortMapState::Trying, upload_kbps, ..HostingInfo::default() };
        let mut handle = HostingHandle { port, secret, mapper: None, local, info, local_at: Instant::now(), loopback_only: false };
        handle.refresh();
        handle
    }
    fn udp_options(&self, max_clients: usize) -> UdpHostOptions {
        let ip = if self.loopback_only { Ipv4Addr::LOCALHOST } else { Ipv4Addr::UNSPECIFIED };
        UdpHostOptions { bind: SocketAddr::new(IpAddr::V4(ip), self.port), secret: self.secret, max_clients: max_clients.max(MAX_UDP_CLIENTS) }
    }
    /// Recompute every address and code from the secret, the port, the local addresses and the outcome.
    fn refresh(&mut self) {
        let info = &mut self.info;
        info.port = self.port;
        info.portmap = if self.loopback_only && info.outcome.is_none() { PortMapState::Idle } else { portmap_state(info.outcome.as_ref()) };
        info.lobby_key = self.secret.lobby_key();
        let external = match &info.outcome {
            Some(MappingOutcome::Mapped { external, .. }) => Some(*external),
            // A failed renewal leaves the last good mapping in place until its lease runs out.
            Some(MappingOutcome::RenewFailed { .. }) => self.mapper.as_ref().and_then(|m| m.external()),
            _ => None,
        };
        info.external_address = external.map(|a| a.to_string()).unwrap_or_default();
        let lan = first_lan(&self.local).map(|ip| SocketAddr::new(ip, self.port));
        info.lan_address = lan.map(|a| a.to_string()).unwrap_or_default();
        let code_addr = external.or(lan).unwrap_or(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self.port));
        info.join_code = JoinCode::new(code_addr, self.secret).encode();
        let vpn = first_vpn(&self.local).map(|ip| SocketAddr::new(ip, self.port));
        info.vpn_address = vpn.map(|a| a.to_string()).unwrap_or_default();
        info.vpn_join_code = vpn.map(|a| JoinCode::new(a, self.secret).encode()).unwrap_or_default();
    }
    /// A mapping outcome arrived (from the mapper, or a test).
    fn on_outcome(&mut self, outcome: MappingOutcome) {
        self.info.outcome = Some(outcome);
    }
    /// Once per frame: `PortMapper::poll`, refresh addresses and codes. True when `info()` changed.
    pub fn poll(&mut self) -> bool {
        let before = self.info.clone();
        let mut outcomes = Vec::new();
        if let Some(mapper) = self.mapper.as_mut() {
            while let Some(outcome) = mapper.poll() { outcomes.push(outcome); }
        }
        for outcome in outcomes { self.on_outcome(outcome); }
        if !self.loopback_only && self.local_at.elapsed() >= ADDRESS_REFRESH {
            self.local_at = Instant::now();
            self.local = net::portmap::local_addresses();
        }
        self.refresh();
        self.info != before
    }
    /// The clock thread's answer to `SetUdp` (`HostNote::UdpBound`): `Ok` marks the port bound.
    pub fn on_bound(&mut self, result: &Result<Vec<std::net::SocketAddr>, String>) {
        match result {
            Ok(addrs) => {
                self.info.bound = true;
                // A port of 0 asks the OS for one: the codes must name the port it gave.
                if let Some(port) = addrs.iter().map(|a| a.port()).find(|p| *p != 0) { self.port = port; }
            }
            Err(_) => self.info.bound = false,
        }
        self.refresh();
    }
    /// Saturation and ban count as the session reports them (`HostEvent::Saturation`, kicks with ban).
    pub fn on_saturation(&mut self, on: bool) { self.info.saturated = on; }
    pub fn info(&self) -> &HostingInfo { &self.info }
    pub fn set_upload_kbps(&mut self, kbps: u32) { self.info.upload_kbps = kbps; }
    /// New secret; old codes and keys stop working (L1003). Returns the options for
    /// `HostControl::SetUdp { options: Some(..), reason: DropReason::SecretRegenerated }`: net rebuilds the
    /// UDP backend, connected UDP players drop and may rejoin into their held slot with the new code.
    pub fn regenerate_secret(&mut self, max_clients: usize) -> UdpHostOptions {
        self.secret = LobbySecret::generate();
        self.info.bound = false;
        self.refresh();
        self.udp_options(max_clients)
    }
    /// Delete port mappings (`PortMapper::stop`, at most 2 s). The caller also sends
    /// `HostControl::SetUdp { options: None, reason: DropReason::Left }`.
    pub fn stop(&mut self) {
        if let Some(mapper) = self.mapper.take() { mapper.stop(); }
        self.info.bound = false;
    }
}

/// The LAN address to show: the default route's interface first, IPv4 before IPv6 (as `local_addresses`
/// sorts them), so the first IPv4 address when there is one.
fn first_lan(local: &LocalAddrs) -> Option<IpAddr> {
    local.lan.iter().copied().find(IpAddr::is_ipv4).or_else(|| local.lan.first().copied())
}
fn first_vpn(local: &LocalAddrs) -> Option<IpAddr> {
    local.vpn.iter().map(|v| v.ip).find(IpAddr::is_ipv4).or_else(|| local.vpn.first().map(|v| v.ip))
}

/// `MappingOutcome` as the `Bp.PORTMAP_*` state: `None` while the mapper has not answered is `Trying`.
pub fn portmap_state(outcome: Option<&MappingOutcome>) -> PortMapState {
    match outcome {
        None => PortMapState::Trying,
        Some(MappingOutcome::Mapped { .. }) => PortMapState::Mapped,
        Some(MappingOutcome::DoubleNat { .. }) => PortMapState::DoubleNat,
        Some(MappingOutcome::Failed { .. }) => PortMapState::Failed,
        Some(MappingOutcome::RenewFailed { .. }) => PortMapState::RenewFailed,
    }
}

// ---------------------------------------------------------------------------------------------------------
// net's typed reasons as player text (net produces no English; these are the only places the keys appear)
// ---------------------------------------------------------------------------------------------------------

fn m(key: &str) -> Msg { Msg::new(key) }
fn int(v: impl Into<i64>) -> MsgArg { MsgArg::Int(v.into()) }

/// `net.refusal.protocol_mismatch` (args: host version, client version), `net.refusal.lobby_full` (arg:
/// max players), `net.refusal.host_saturated`, `net.refusal.banned`, `net.refusal.operation_in_progress`,
/// `net.refusal.match_in_progress`, `net.refusal.duplicate_client`, `net.refusal.bad_hello`,
/// `net.refusal.shutting_down`.
pub fn refusal_msg(reason: &JoinRefusal) -> Msg {
    match *reason {
        JoinRefusal::ProtocolMismatch { host, client } => m("net.refusal.protocol_mismatch").arg(int(host)).arg(int(client)),
        JoinRefusal::LobbyFull { max_players } => m("net.refusal.lobby_full").arg(int(max_players)),
        JoinRefusal::HostSaturated => m("net.refusal.host_saturated"),
        JoinRefusal::Banned => m("net.refusal.banned"),
        JoinRefusal::OperationInProgress => m("net.refusal.operation_in_progress"),
        JoinRefusal::MatchInProgress => m("net.refusal.match_in_progress"),
        JoinRefusal::DuplicateClient => m("net.refusal.duplicate_client"),
        JoinRefusal::BadHello => m("net.refusal.bad_hello"),
        JoinRefusal::ShuttingDown => m("net.refusal.shutting_down"),
    }
}
/// `net.drop.kicked`, `net.drop.banned`, `net.drop.high_ping`, `net.drop.packet_loss`,
/// `net.drop.falling_behind`, `net.drop.timed_out`, `net.drop.left`, `net.drop.secret_regenerated`.
pub fn drop_msg(reason: DropReason) -> Msg {
    m(match reason {
        DropReason::Kicked => "net.drop.kicked",
        DropReason::Banned => "net.drop.banned",
        DropReason::HighPing => "net.drop.high_ping",
        DropReason::PacketLoss => "net.drop.packet_loss",
        DropReason::FallingBehind => "net.drop.falling_behind",
        DropReason::TimedOut => "net.drop.timed_out",
        DropReason::Left => "net.drop.left",
        DropReason::SecretRegenerated => "net.drop.secret_regenerated",
    })
}
/// `net.end.host_quit`, `net.end.host_lost`, `net.end.refused`, `net.end.match_over`; `Dropped(reason)` is
/// [`drop_msg`].
pub fn end_msg(end: SessionEnd) -> Msg {
    match end {
        SessionEnd::HostQuit => m("net.end.host_quit"),
        SessionEnd::HostLost => m("net.end.host_lost"),
        SessionEnd::Refused => m("net.end.refused"),
        SessionEnd::MatchOver => m("net.end.match_over"),
        SessionEnd::Dropped(reason) => drop_msg(reason),
    }
}
/// `net.join_code.bad_character`, `net.join_code.wrong_length`, `net.join_code.checksum`,
/// `net.join_code.unknown_format` (arg: format), `net.join_code.protocol_mismatch` (args: code version, our
/// version), `net.join_code.bad_address`.
pub fn join_code_msg(error: JoinCodeError) -> Msg {
    match error {
        JoinCodeError::BadCharacter => m("net.join_code.bad_character"),
        JoinCodeError::WrongLength => m("net.join_code.wrong_length"),
        JoinCodeError::Checksum => m("net.join_code.checksum"),
        JoinCodeError::UnknownFormat(f) => m("net.join_code.unknown_format").arg(int(f)),
        JoinCodeError::ProtocolMismatch { code, ours } => m("net.join_code.protocol_mismatch").arg(int(code)).arg(int(ours)),
        JoinCodeError::BadAddress => m("net.join_code.bad_address"),
    }
}
/// `net.portmap.trying`, `net.portmap.mapped` (arg: external address), `net.portmap.double_nat` (arg:
/// external address), `net.portmap.failed`, `net.portmap.renew_failed`.
pub fn portmap_msg(outcome: Option<&MappingOutcome>) -> Msg {
    match outcome {
        None => m("net.portmap.trying"),
        Some(MappingOutcome::Mapped { external, .. }) => m("net.portmap.mapped").arg(MsgArg::Name(external.to_string())),
        Some(MappingOutcome::DoubleNat { external, .. }) => m("net.portmap.double_nat").arg(MsgArg::Name(external.to_string())),
        Some(MappingOutcome::Failed { .. }) => m("net.portmap.failed"),
        Some(MappingOutcome::RenewFailed { .. }) => m("net.portmap.renew_failed"),
    }
}
/// `net.warning.high_ping`, `net.warning.packet_loss`, `net.warning.falling_behind` (L1035).
pub fn warning_msg(warning: net::msg::DropWarning) -> Msg {
    use net::msg::DropWarning as W;
    m(match warning {
        W::HighPing => "net.warning.high_ping",
        W::PacketLoss => "net.warning.packet_loss",
        W::FallingBehind => "net.warning.falling_behind",
    })
}
/// A transport or decode failure for the local log and the `failed` signal: `net.error.io`,
/// `net.error.transport`, `net.error.decode`, `net.error.not_connected`, `net.error.wrong_phase`,
/// `net.error.other`; refusals and join-code errors inside a `NetError` use their own functions above.
pub fn error_msg(error: &NetError) -> Msg {
    match error {
        NetError::Io(_) => m("net.error.io"),
        NetError::Transport(_) => m("net.error.transport"),
        NetError::Decode(_) => m("net.error.decode"),
        NetError::NotConnected => m("net.error.not_connected"),
        NetError::WrongPhase => m("net.error.wrong_phase"),
        NetError::JoinCode(e) => join_code_msg(*e),
        NetError::Refused(r) => refusal_msg(r),
        NetError::Encode(_) | NetError::Blueprint(_) | NetError::Sim(_) | NetError::Replay(_) => m("net.error.other"),
    }
}
/// Every key the functions above can return, for the translation-coverage lint and the `net.csv` owner.
pub fn text_keys() -> Vec<&'static str> {
    vec![
        "net.refusal.protocol_mismatch", "net.refusal.lobby_full", "net.refusal.host_saturated", "net.refusal.banned",
        "net.refusal.operation_in_progress", "net.refusal.match_in_progress", "net.refusal.duplicate_client",
        "net.refusal.bad_hello", "net.refusal.shutting_down",
        "net.drop.kicked", "net.drop.banned", "net.drop.high_ping", "net.drop.packet_loss", "net.drop.falling_behind",
        "net.drop.timed_out", "net.drop.left", "net.drop.secret_regenerated",
        "net.end.host_quit", "net.end.host_lost", "net.end.refused", "net.end.match_over",
        "net.join_code.bad_character", "net.join_code.wrong_length", "net.join_code.checksum",
        "net.join_code.unknown_format", "net.join_code.protocol_mismatch", "net.join_code.bad_address",
        "net.portmap.trying", "net.portmap.mapped", "net.portmap.double_nat", "net.portmap.failed",
        "net.portmap.renew_failed",
        "net.warning.high_ping", "net.warning.packet_loss", "net.warning.falling_behind",
        "net.error.io", "net.error.transport", "net.error.decode", "net.error.not_connected", "net.error.wrong_phase",
        "net.error.other",
    ]
}

#[cfg(test)]
mod tests;
