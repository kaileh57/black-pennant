//! Unit `server-dedicated`. The dedicated server. DESIGN.md L1015 (headless binary from the first milestone;
//! ships for Linux x64 and Windows x64 after the first public release; reads a RON config with port, mode,
//! ruleset, lobby size and upload figure; maps its port the same way the game does; prints its join code at
//! start; provider-agnostic), L994-1003 (hosting and join codes), L1031 (client limit from upload), L1039
//! (connection log).
//!
//! Flow: read `content::settings::ServerConfig` (defaults when no file), apply `--port`; load data and the
//! sim pool; draw the session seed and a fresh `LobbySecret`; `ServerSession::new` with `dedicated = true`;
//! bind UDP (`NetcodeServerBackend::bind_udp`) and add it; start `PortMapper` unless `--no-map`; print the join
//! information (stdout, plain lines: the join code for the mapped external address when mapping succeeded, the
//! LAN and VPN join codes, the lobby key, and a double-NAT or mapping-failed notice); then loop at 1 ms
//! granularity calling `ServerSession::update`, handling `HostEvent`s: `StartRequested` builds the sortie
//! ([`build_sortie`]) when the lobby can start; `SortieOver` ends it, saves the replay to `replays/` beside
//! the config and returns to the lobby. Connection log lines append to the log file each second. Stops on
//! stdin EOF or a line `quit`, deleting port mappings.
//!
//! Campaign co-op is hosted from the game client, not from this binary: a `ServerConfig` with
//! `mode: Campaign` is refused with a config error (the host machine writes the campaign save, L834).
//!
//! # Choices this unit makes where the spec is silent
//! - The config's `lobby_size` caps the UDP backend's connections (at most the mode's player maximum); the
//!   lobby's own limit from the upload figure (L1031) applies on top. Port 0 and a lobby size of 0 are config
//!   errors, and so are modes no lobby offers (splash, tutorial, trial) and PvP escort before 1.0.
//! - Seeds: the session seed comes from fresh random bytes; each sortie after the first moves to the next
//!   seed, so maps, weather and skirmish templates vary. Sea state (Beaufort 2 to 5), weather (clear, haze or
//!   overcast) and time of day (day or dusk) are drawn from the seed; a skirmish picks one of the combat
//!   templates by seed at the act the tonnage cap allows, one enemy hull per player, at the config's
//!   difficulty. The config's `guest_scripts` only ever narrows what the lobby ruleset allows.
//! - After a sortie the server waits [`AFTER_ACTION_S`] (debrief) and returns to the lobby. A sortie that
//!   every player has left, with no slot held for rejoin, ends at once as aborted; a sandbox never ends on
//!   its own otherwise. A start the Sim refuses is reported on stderr and retried every
//!   [`START_RETRY_S`] while the lobby can still start.
//! - Replays go through `net::replay::ReplayStore` (20 most recent kept). A PvP player asking for the
//!   released replay gets the last match's replay cut for their side.
//! - Stdin also takes `status` (connection panel), `code` (join information again), `kick N` and `ban N`
//!   (slot numbers).
use crate::args::ServeArgs;
use crate::error::{CliError, Code};
use crate::runtime::Data;
use content::blueprint::Blueprint;
use content::settings::ServerConfig;
use net::join::LobbySecret;
use net::portmap::{LocalAddrs, MappingOutcome};
use sim_systems::modes::LobbySetup;
use sim_systems::protocol::config::SortieConfig;

use content::enums::{Act, GameMode, TimeOfDay};
use content::ids::{ContentHash, PlayerSlot};
use content::weather::{SeaState, WeatherKind};
use net::clock::{Clock, SystemClock};
use net::join::JoinCode;
use net::session::server::ServerSession;
use net::session::{HostEvent, ServerOptions, SessionPhase};
use sim_core::rng::{Key, Stream};
use std::collections::BTreeMap;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Debrief time between a sortie's end and the return to the lobby, seconds.
pub const AFTER_ACTION_S: u64 = 15;
/// Retry period of a start the Sim refused, seconds.
pub const START_RETRY_S: u64 = 5;
/// Skirmish templates a dedicated server rotates through (the ones that play as a fight).
const SKIRMISH_TEMPLATES: [content::campaign::MissionType; 6] = {
    use content::campaign::MissionType as M;
    [M::ConvoyRaid, M::DepotStrike, M::RadarDecapitation, M::HunterGroupSurvival, M::CuttingOut, M::PrizeCapture]
};
/// Salt of the lobby draws (counter-hash RNG).
const SALT_LOBBY: u64 = 0x5345_5256_455F_4C42;

/// Unit: `server-dedicated`.
pub fn run(args: &ServeArgs) -> Result<Code, CliError> {
    let mut config = match &args.config { Some(p) => crate::runtime::read_ron::<ServerConfig>(p)?, None => ServerConfig::default() };
    if let Some(port) = args.port { config.port = port; }
    let base = match &args.config {
        Some(p) => p.parent().filter(|d| !d.as_os_str().is_empty()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(".")),
        None => PathBuf::from("."),
    };
    let log = args.log.clone().unwrap_or_else(|| base.join("connection.log"));
    let data = crate::runtime::load_data()?;
    let pool = crate::runtime::sim_pool(3)?;
    let bind: SocketAddr = (std::net::Ipv4Addr::UNSPECIFIED, config.port).into();
    let mut host = Host::new(config, data, pool, bind, args.map_port, log, base.join("replays"))?;
    host.print_join();
    let rx = stdin_lines();
    loop {
        match rx.try_recv() {
            Ok(line) => if host.command(&line) { break },
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        host.pump()?;
        std::thread::sleep(Duration::from_millis(1));
    }
    host.stop();
    Ok(crate::error::PASS)
}

/// Lines from stdin on a background thread; the channel closes on EOF or a read error.
fn stdin_lines() -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("stdin".into()).spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() { break; }
        }
    }).ok();
    rx
}

/// A uniform draw in [0, 1) for a lobby choice.
fn draw(seed: u64, purpose: u64) -> f64 { sim_core::rng::uniform(Key::new(seed, 0, Stream::Generator, SALT_LOBBY, purpose)) }
fn pick<T: Copy>(seed: u64, purpose: u64, options: &[T]) -> T { options[((draw(seed, purpose) * options.len() as f64) as usize).min(options.len() - 1)] }

/// Check the config's mode and numbers before anything binds.
fn check_config(config: &ServerConfig) -> Result<(), CliError> {
    match config.mode {
        GameMode::Campaign => return Err(CliError::Config("campaign co-op is hosted from the game client, whose machine writes the campaign save (DESIGN.md L834); choose skirmish, sandbox or a PvP mode".into())),
        GameMode::Splash | GameMode::Tutorial | GameMode::Trial => return Err(CliError::Config(format!("mode {:?} is not a lobby mode", config.mode))),
        GameMode::PvpEscort => return Err(CliError::Config("PvP escort arrives with 1.0 and is not in this build".into())),
        _ => {}
    }
    if config.port == 0 { return Err(CliError::Config("port must be 1 to 65535".into())); }
    if config.lobby_size == 0 { return Err(CliError::Config("lobby_size must be at least 1".into())); }
    Ok(())
}

/// Lobby setup from the config: mode, ruleset, default weather and map for the seed. Unit: `server-dedicated`.
pub fn lobby_setup(config: &ServerConfig, seed: u64) -> Result<LobbySetup, CliError> {
    check_config(config)?;
    let mut ruleset = config.ruleset.clone();
    let pvp = matches!(config.mode, GameMode::PvpElimination | GameMode::PvpEscort);
    if pvp { ruleset.luau = false; }
    let skirmish = (config.mode == GameMode::Skirmish).then(|| {
        let cap = if ruleset.tonnage_cap_t.is_finite() { ruleset.tonnage_cap_t } else { f64::INFINITY };
        // The act whose hull cap fits the ruleset, but never past the last act this build ships, whose enemy
        // classes and starters are the only ones the build plays (DECISION, r4, from c4).
        let last = content::campaign::load::shipped_campaign().acts.iter().map(|a| a.act).max().unwrap_or(Act::Act1);
        let act = Act::ALL.iter().copied().find(|a| cap <= a.hull_cap_t()).unwrap_or(Act::Act4).min(last);
        sim_systems::modes::SkirmishOptions { template: pick(seed, 3, &SKIRMISH_TEMPLATES), act, enemy_per_player: 1, difficulty: config.settings.difficulty }
    });
    Ok(LobbySetup {
        mode: config.mode, ruleset, seed, players: Vec::new(), skirmish,
        weather: pick(seed, 0, &[WeatherKind::Clear, WeatherKind::Haze, WeatherKind::Overcast]),
        sea_state: SeaState(pick(seed, 1, &[2u8, 3, 4, 5])),
        time_of_day: pick(seed, 2, &[TimeOfDay::Day, TimeOfDay::Day, TimeOfDay::Dusk]),
        map: None, at_1_0: false,
    })
}

/// Dispatch to `sim_systems::modes::{skirmish,sandbox,elimination,escort}_config` by mode. Unit: `server-dedicated`.
pub fn build_sortie(setup: &LobbySetup, blueprints: &[Blueprint], data: &Data) -> Result<SortieConfig, CliError> {
    use sim_systems::modes as m;
    let issues = m::validate_lobby(setup, blueprints, &data.catalogue);
    if !issues.is_empty() { return Err(CliError::Config(format!("the lobby cannot start: {issues:?}"))); }
    match setup.mode {
        GameMode::Skirmish => Ok(m::skirmish_config(setup, blueprints, &data.campaign, &data.catalogue)),
        GameMode::Sandbox => Ok(m::sandbox_config(setup, blueprints, &data.campaign)),
        GameMode::PvpElimination => Ok(m::elimination_config(setup, blueprints, &data.campaign)),
        GameMode::PvpEscort => Ok(m::escort_config(setup, blueprints, &data.campaign)),
        GameMode::Campaign => Err(CliError::Config("campaign co-op is hosted from the game client".into())),
        other => Err(CliError::Config(format!("mode {other:?} is not a lobby mode"))),
    }
}

/// The join information lines printed at start and whenever the mapping result changes. `mapping_on` is false
/// under `--no-map`. Unit: `server-dedicated`.
pub fn join_lines(secret: &LobbySecret, port: u16, mapping: Option<&MappingOutcome>, mapping_on: bool, local: &LocalAddrs) -> Vec<String> {
    let code = |ip: IpAddr| JoinCode::new(SocketAddr::new(ip, port), *secret).encode();
    let lan = match local.lan.first() { Some(ip) => format!("this machine's LAN address {ip}"), None => "this machine's LAN address".to_string() };
    let forward = format!("forward UDP port {port} to {lan}, or use a mesh VPN (Tailscale or ZeroTier)");
    let mut out = vec![format!("lobby key {}", secret.lobby_key())];
    match mapping {
        Some(MappingOutcome::Mapped { protocol, external, lease_s, permanent }) => {
            let lease = if *permanent { "permanent lease".to_string() } else { format!("lease {lease_s} s") };
            out.push(format!("join code {} (internet, {} via {protocol:?}, {lease})", JoinCode::new(*external, *secret).encode(), external));
            out.push("the internet join code contains this server's public IP address".to_string());
        }
        Some(MappingOutcome::DoubleNat { protocol, external }) => {
            out.push(format!("double NAT: {protocol:?} mapped {external}, a private or shared address, so players outside cannot reach this server"));
            out.push(format!("to host for the internet, {forward}"));
        }
        Some(MappingOutcome::Failed { upnp, nat_pmp_pcp }) => {
            out.push(format!("port mapping failed: UPnP {}, NAT-PMP and PCP {}; players on this network can still join", failure_text(upnp), failure_text(nat_pmp_pcp)));
            out.push(format!("to host for the internet, {forward}"));
        }
        Some(MappingOutcome::RenewFailed { protocol, failure }) => {
            out.push(format!("port mapping renewal failed ({protocol:?}: {}); the mapping may lapse", failure_text(failure)));
            out.push(format!("if players cannot join, {forward}"));
        }
        None if mapping_on => out.push("port mapping: asking the router (UPnP, NAT-PMP and PCP, up to 3 s); the result follows".to_string()),
        None => out.push(format!("port mapping off (--no-map); to host for the internet, {forward}")),
    }
    for ip in &local.lan { out.push(format!("join code {} (LAN {ip})", code(*ip))); }
    for v in &local.vpn { out.push(format!("join code {} ({:?} {})", code(v.ip), v.kind, v.ip)); }
    out.push(format!("join by address: this machine's address, port {port}, and the lobby key"));
    out
}

/// Why one mapping protocol did not work, in words for the console.
fn failure_text(f: &net::portmap::MapFailure) -> String {
    use net::portmap::MapFailure as F;
    match f {
        F::Timeout => "did not answer within 3 s".to_string(),
        F::NoGateway => "found no gateway".to_string(),
        F::Refused(why) => format!("was refused ({why})"),
        F::Io(why) => format!("failed ({why})"),
    }
}

/// The running dedicated host: session, mapping, logs, replays and the sortie cycle.
pub(crate) struct Host {
    pub(crate) server: ServerSession,
    data: Data,
    config: ServerConfig,
    clock: Arc<dyn Clock>,
    secret: LobbySecret,
    port: u16,
    pub(crate) addr: SocketAddr,
    mapper: Option<net::portmap::PortMapper>,
    mapping: Option<MappingOutcome>,
    local: LocalAddrs,
    log: PathBuf,
    replays: net::replay::ReplayStore,
    /// Designs members uploaded, decoded from the session's store.
    designs: BTreeMap<ContentHash, Blueprint>,
    seed: u64,
    sorties: u64,
    after_action_until: Option<Duration>,
    retry_at: Option<Duration>,
    next_second: Duration,
    /// The last PvP match's replay and each slot's side, for release requests.
    last_replay: Option<(Vec<u8>, BTreeMap<PlayerSlot, content::ids::SideId>)>,
    pub(crate) lines: Vec<String>,
    pub(crate) echo: bool,
}

impl Host {
    pub(crate) fn new(config: ServerConfig, data: Data, pool: Arc<rayon::ThreadPool>, bind: SocketAddr, map_port: bool, log: PathBuf, replays: PathBuf) -> Result<Host, CliError> {
        check_config(&config)?;
        let secret = LobbySecret::generate();
        let seed_bytes = LobbySecret::generate().0;
        let mut eight = [0u8; 8];
        for (d, s) in eight.iter_mut().zip(seed_bytes.iter()) { *d = *s; }
        let seed = u64::from_le_bytes(eight);
        let setup = lobby_setup(&config, seed)?;
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
        let mut options = ServerOptions::new(env!("CARGO_PKG_VERSION"), config.mode);
        options.lobby_name = config.lobby_name.clone();
        options.upload_kbps = config.upload_kbps.max(1);
        options.dedicated = true;
        options.record_replays = true;
        let mut server = ServerSession::new(options, setup, clock.clone(), pool, data.catalogue.clone(), Box::new(net::blueprints::MemoryStore::default()));
        let max_clients = usize::from(config.lobby_size.min(content::ruleset::max_players(config.mode, false)));
        let backend = net::transport::netcode::NetcodeServerBackend::bind_udp(&net::transport::netcode::UdpHostOptions { bind, secret, max_clients }, clock.unix_now(), None)?;
        let addr = {
            use net::transport::ServerTransport;
            backend.local_addrs().into_iter().find(|a| a.port() != 0).unwrap_or(bind)
        };
        let port = addr.port();
        server.add_transport(Box::new(backend));
        let mapper = map_port.then(|| net::portmap::PortMapper::start(net::portmap::PortMapOptions::new(port)));
        let local = net::portmap::local_addresses();
        let now = clock.now();
        Ok(Host {
            server, data, config, clock, secret, port, addr, mapper, mapping: None, local, log, replays: net::replay::ReplayStore::new(replays),
            designs: BTreeMap::new(), seed, sorties: 0, after_action_until: None, retry_at: None, next_second: now + Duration::from_secs(1),
            last_replay: None, lines: Vec::new(), echo: true,
        })
    }

    pub(crate) fn secret(&self) -> LobbySecret { self.secret }

    fn say(&mut self, line: String) {
        if self.echo {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
        self.lines.push(line);
    }
    fn note(&self, line: &str) { if self.echo { eprintln!("{line}"); } }

    pub(crate) fn print_join(&mut self) {
        self.say(format!("black-pennant-server {} (protocol {}) mode {:?} on UDP port {}", env!("CARGO_PKG_VERSION"), net::PROTOCOL_VERSION, self.config.mode, self.port));
        for l in join_lines(&self.secret, self.port, self.mapping.as_ref(), self.mapper.is_some(), &self.local) { self.say(l); }
    }

    /// One pass of the host loop.
    pub(crate) fn pump(&mut self) -> Result<(), CliError> {
        let events = self.server.update()?;
        let now = self.clock.now();
        for e in events { self.event(e, now); }
        if let Some(o) = self.mapper.as_mut().and_then(|m| m.poll()) {
            self.server.log_mut().push(now, net::quality::LogKind::PortMapping(o.clone()));
            self.mapping = Some(o);
            for l in join_lines(&self.secret, self.port, self.mapping.as_ref(), self.mapper.is_some(), &self.local) { self.say(l); }
        }
        // Back to the lobby after the debrief.
        if self.after_action_until.is_some_and(|t| now >= t) && self.server.phase() == SessionPhase::AfterAction {
            self.after_action_until = None;
            self.next_lobby();
        }
        if self.server.phase() == SessionPhase::Sortie && self.server.lobby().members().is_empty() && self.server.lobby().held_slots(now).is_empty() {
            self.note("every player left: the sortie ends");
            self.finish_sortie(now);
        }
        if self.retry_at.is_some_and(|t| now >= t) {
            self.retry_at = None;
            if self.server.phase() == SessionPhase::Lobby { self.try_start(now); }
        }
        if now >= self.next_second {
            self.next_second = now + Duration::from_secs(1);
            self.flush_log();
        }
        Ok(())
    }

    fn event(&mut self, e: HostEvent, now: Duration) {
        match e {
            HostEvent::BlueprintAccepted { slot, hash } => {
                if let Some(bytes) = self.server.store_mut().get(&hash) {
                    let packed = content::share::PackedBlueprint { hash, compressed: bytes };
                    match net::blueprints::accept_upload(&packed, &self.data.catalogue, &self.server.lobby().setup().ruleset) {
                        Ok(bp) => { self.designs.insert(hash, bp); }
                        Err(r) => self.note(&format!("design from slot {} could not be decoded: {r:?}", slot.0)),
                    }
                }
            }
            HostEvent::StartRequested { .. } => if self.server.phase() == SessionPhase::Lobby { self.try_start(now) },
            HostEvent::SortieOver => self.finish_sortie(now),
            HostEvent::PlayerJoined { slot, name, rejoined, .. } => self.note(&format!("{} slot {} {name}", if rejoined { "rejoined" } else { "joined" }, slot.0)),
            HostEvent::PlayerLeft { slot, reason, held, .. } => self.note(&format!("slot {} left ({reason:?}){}", slot.0, if held { ", slot held" } else { "" })),
            HostEvent::JoinRefused { client_id, reason } => self.note(&format!("refused client {client_id:016x}: {reason:?}")),
            HostEvent::Saturation { on } => self.note(if on { "upload saturated: caps cut and joins refused" } else { "upload saturation over" }),
            HostEvent::ReplayRequested { slot } => self.release_replay(slot),
            HostEvent::Theatre { .. } => {}
            _ => {}
        }
    }

    /// Designs the lobby's players chose, in slot order.
    fn lobby_designs(&self) -> Vec<Blueprint> {
        let mut out: Vec<Blueprint> = Vec::new();
        for p in &self.server.lobby().setup().players {
            if let Some(bp) = p.blueprint.and_then(|h| self.designs.get(&h)) {
                if !out.iter().any(|b| b.header.hash == bp.header.hash) { out.push(bp.clone()); }
            }
        }
        out
    }

    fn try_start(&mut self, now: Duration) {
        let designs = self.lobby_designs();
        if !self.server.lobby().can_start(&designs, &self.data.catalogue) { return; }
        let result = build_sortie(self.server.lobby().setup(), &designs, &self.data).and_then(|mut config| {
            config.settings.guest_scripts = config.settings.guest_scripts && self.config.settings.guest_scripts;
            let sim = sim_systems::Sim::new(config, crate::runtime::sim_deps(&self.data))?;
            self.server.start_sortie(sim)?;
            Ok(())
        });
        match result {
            Ok(()) => {
                self.sorties += 1;
                self.note(&format!("sortie {} started, seed {}", self.sorties, self.server.lobby().setup().seed));
            }
            Err(e) => {
                self.note(&format!("the sortie could not start: {e}"));
                self.retry_at = Some(now + Duration::from_secs(START_RETRY_S));
            }
        }
    }

    fn finish_sortie(&mut self, now: Duration) {
        let Some((sim, replay)) = self.server.end_sortie() else { return };
        let outcome = sim.result().map(|r| r.outcome);
        self.note(&format!("sortie {} over: {outcome:?} after {} ticks", self.sorties, sim.tick().0));
        if let Some(bytes) = replay {
            match self.replays.save(&bytes) {
                Ok(name) => self.note(&format!("replay saved: {name}")),
                Err(e) => self.note(&format!("replay not saved: {e}")),
            }
            let pvp = matches!(sim.config().mode, GameMode::PvpElimination | GameMode::PvpEscort);
            self.last_replay = pvp.then(|| (bytes, sim.players().iter().map(|p| (p.slot, p.side)).collect()));
        }
        self.after_action_until = Some(now + Duration::from_secs(AFTER_ACTION_S));
    }

    /// The next seed's lobby: new weather and map, everyone readies again.
    fn next_lobby(&mut self) {
        self.seed = self.seed.wrapping_add(1);
        if let Ok(fresh) = lobby_setup(&self.config, self.seed) {
            let setup = self.server.lobby_mut().setup_mut();
            setup.seed = fresh.seed;
            setup.weather = fresh.weather;
            setup.sea_state = fresh.sea_state;
            setup.time_of_day = fresh.time_of_day;
            if let (Some(s), Some(f)) = (setup.skirmish.as_mut(), fresh.skirmish) { s.template = f.template; }
        }
        if let Err(e) = self.server.set_phase(SessionPhase::Lobby) { self.note(&format!("could not return to the lobby: {e}")); }
    }

    fn release_replay(&mut self, slot: PlayerSlot) {
        if matches!(self.server.phase(), SessionPhase::Sortie | SessionPhase::Loading) { return; }
        let Some((bytes, sides)) = &self.last_replay else { return };
        let Some(side) = sides.get(&slot).copied() else { return };
        match net::replay::release_pvp(bytes, &[side]) {
            Ok(cuts) => for (_, cut) in cuts { self.server.send_bulk(Some(slot), net::msg::TransferKind::Replay, cut); },
            Err(e) => self.note(&format!("replay release failed: {e}")),
        }
    }

    /// Append the connection log's new lines to the log file.
    fn flush_log(&mut self) {
        let text = self.server.log_mut().take_unflushed_ron();
        if text.is_empty() { return; }
        if let Some(dir) = self.log.parent().filter(|d| !d.as_os_str().is_empty()) { let _ = std::fs::create_dir_all(dir); }
        let written = std::fs::OpenOptions::new().create(true).append(true).open(&self.log).and_then(|mut f| f.write_all(text.as_bytes()));
        if let Err(e) = written { self.note(&format!("connection log {}: {e}", self.log.display())); }
    }

    /// An operator line from stdin; true means quit.
    pub(crate) fn command(&mut self, line: &str) -> bool {
        let mut words = line.split_whitespace();
        match (words.next(), words.next().and_then(|w| w.parse::<u8>().ok())) {
            (Some("quit" | "exit" | "stop"), _) => return true,
            (Some("status"), _) => {
                let p = self.server.connection_panel();
                self.say(format!("phase {:?}, host tick {:.1} Hz p95 {:.2} ms{}", self.server.phase(), p.host_tick_hz, p.host_tick_p95_ms, if p.saturated { ", upload saturated" } else { "" }));
                for l in p.players { self.say(format!("slot {} {} ping {} ms loss {:.1}% behind {} cap {} kbps {:?}", l.slot.0, l.name, l.ping_ms, l.loss * 100.0, l.frames_behind, l.cap_kbps, l.quality)); }
            }
            (Some("code"), _) => { for l in join_lines(&self.secret, self.port, self.mapping.as_ref(), self.mapper.is_some(), &self.local) { self.say(l); } }
            (Some("kick"), Some(slot)) => self.server.kick(PlayerSlot(slot), false),
            (Some("ban"), Some(slot)) => self.server.kick(PlayerSlot(slot), true),
            (None, _) => {}
            _ => self.say("commands: status, code, kick N, ban N, quit".to_string()),
        }
        false
    }

    /// Quit: end the session, delete port mappings, write the last log lines.
    pub(crate) fn stop(mut self) {
        if self.server.sim().is_some() { self.finish_sortie(self.clock.now()); }
        self.server.shutdown();
        if let Some(m) = self.mapper.take() { m.stop(); }
        self.flush_log();
    }
}

#[cfg(test)]
mod tests;
