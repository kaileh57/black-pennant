//! Unit `gd-lobby`. `BpLobby` (autoload `Lobby`): hosting, joining and the pre-sortie lobby.
//! DESIGN.md L955-978 (modes, rulesets, complexity caps), L992-1007 (hosting over UDP: port 47610 bound only
//! when the player hosts, firewall explained first, port mapping, double NAT, manual steps and mesh VPN, join
//! codes, join by address and key, client id, display name, kick, ban, regenerate secret, protocol mismatch,
//! host loss), L1031 (upload figure and client limit), L1033-1039 (connection quality and drops),
//! L943-945 (blueprints stream to the host, which re-validates them).
//! The lobby rules are `sim_systems::modes`; the lobby itself is `net::lobby::Lobby` inside the host's
//! `net::ServerSession`, which admits, holds slots, bans and applies requests. `BpLobby` never holds the
//! authoritative state:
//! - Every `BpLobby`, the host's included, draws the `net::msg::LobbyView` of the last
//!   `ClientEvent::Lobby` in `Services::feed` and sends `ClientMsg::Lobby(LobbyRequest)` for ready, team,
//!   blueprint choice (after `ClientMsg::UploadBlueprint`), and, from the host's own client only,
//!   `SetSetup` and `Start`.
//! - The host's `BpLobby` also owns the `link::net::HostingHandle` (secret, port mapping, join codes) and
//!   acts on `HostNote::Session` events: `StartRequested` becomes a `PendingSortie` with `HostStart::Lobby`;
//!   kick and ban are `HostControl::Kick`; the upload figure is `HostControl::SetUploadKbps`; hosting,
//!   regenerating the secret and stopping are `HostControl::SetUdp` with the handle's `UdpHostOptions`.
//! - `setup` below is the host's working copy of what it last sent with `SetSetup` (seed, weather, sea state,
//!   time of day, skirmish options, map), so its own setup controls show values the `LobbyView` does not
//!   carry. Members see those fields only through the briefing and the sortie itself.
//! - Reasons are `net`'s typed enums turned into `MsgDict`s by `link::net::{refusal_msg, drop_msg, end_msg,
//!   join_code_msg, portmap_msg}`.
//!
//! Campaign co-op uses the same lobby with the campaign's save as its setup.
//!
//! # Decisions where the contract is silent
//! - Who is hosting is read from `Services::connection` every frame, not only from `host()`: a co-op
//!   member that re-hosts through `Session.rehost` becomes the host here too, and a session left from
//!   elsewhere (`Session.leave`) ends the lobby with an empty `left` reason.
//! - On a member `setup` holds a mirror of the last `LobbyView` (mode, ruleset, players), which is what makes
//!   `is_active` true; `LobbyDict.setup` stays `null` for members.
//! - `host` and `join_*` need a session that is idle, failed or ended; a finished session is closed first
//!   (`Session.leave(false)`). Leaving always goes through `Session.leave(false)`, which closes the link, stops
//!   the clock thread and deletes the port mappings. Calls into `Session` and every signal are made with no
//!   `Services` borrow held and under `base_mut`, because handlers call back into `Lobby`.
//! - The sortie begins loading the moment the host starts it: `starting` carries 0 seconds.
//! - A join that ends before the host ever answered is `join_failed` with `net.join.no_answer`, not a
//!   lost-host message, because there was never a host to lose.
//! - The hosted lobby is named after the host's display name unless `options.name` names it.
//! - Port and upload figure default to the `net.port` and `net.upload_kbps` settings; a figure changed on
//!   the host panel is written back to `net.upload_kbps`.
//! - PvP hosts pick the per-hull tonnage cap with an extra `tonnage_cap_t` key in `host` and `set_setup`
//!   options (150, 400, 900 or 1,800 t, L971; 400 t by default).
//! - Co-op members map their own port in the background once admitted and report the result to the host
//!   once (`ClientMsg::Mapping`), for the succession list (L1005).
//! - Other members' designs are fetched for the lobby list (`ClientMsg::RequestBlueprint`) and named from
//!   the local store on a background thread (a 90 m hull takes about 16 ms to unpack and weigh, well over
//!   the 2 ms adapter budget); an opponent's exterior design has a name and no tonnage.
//! - The connection log is the file `app::CONNECTION_LOG`, RON lines in the host session's own format. The
//!   host's session writes joins, refusals, drops, warnings, saturation and ladder steps; this class adds
//!   port-mapping results on the host and, on a member, its own warnings and drops, refusals, ladder steps
//!   and host silence. A log file over 1 MiB is cut to its newest half when a lobby starts.
use crate::convert::{d, get_bool, get_dict, get_f64, get_i64, get_text, msg_dict};
use crate::enums::{GdEnum, NetRole, PortMapState, SessionKind, SessionState};
use crate::link::net::{self as link_net, HostingHandle, LinkOptions};
use crate::seams::{ClientEvent, ClientLink, ClientMsg, InterpTuning, LobbyRequest, LobbyView, PlayerLink};
use crate::services::{services, Services};
use crate::session::host::{HostControl, HostHandle, HostNote, HostStart, SortieOptions};
use crate::session::{Connection, Coop, PendingSortie};
use content::enums::{GameMode, TimeOfDay};
use content::ids::{ContentHash, PlayerSlot};
use content::weather::{SeaState, WeatherKind};
use godot::classes::{DirAccess, FileAccess, ProjectSettings, Time};
use godot::classes::file_access::ModeFlags;
use godot::prelude::*;
use net::portmap::{MappingOutcome, PortMapOptions, PortMapper};
use net::quality::{LinkSample, LogKind};
use sim_systems::modes::{LobbyIssue, LobbyPlayer, LobbySetup, SkirmishOptions};
use sim_systems::protocol::{Msg, MsgArg};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

mod logic;

/// Folder of the client blueprint cache (L943), shared with `BpSession`.
const CACHE_DIR: &str = "user://blueprint_cache";
/// A connection log larger than this is cut to its newest half when a lobby starts.
const LOG_FILE_MAX: u64 = 1 << 20;

/// The multiplayer lobby. Autoload `Lobby`.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct BpLobby {
    pub(crate) setup: Option<Box<LobbySetup>>,
    pub(crate) issues: Vec<LobbyIssue>,
    pub(crate) hosting: bool,
    pub(crate) display_name: String,
    pub(crate) connection_log: Vec<String>,
    /// Everything else the lobby keeps between frames.
    pub(crate) run: LobbyRun,
    base: Base<Node>,
}

/// Lobby state besides the contract fields of [`BpLobby`]. Plain data; `Default` is inert.
#[derive(Default)]
pub(crate) struct LobbyRun {
    /// A join is under way: waiting for `Welcomed`, `Refused` or `Ended`.
    pub(crate) joining: bool,
    pub(crate) own_slot: Option<PlayerSlot>,
    pub(crate) view: Option<LobbyView>,
    /// The lobby's name as the host chose it (members read it from the view).
    pub(crate) name: String,
    /// The design we field, and the host's refusals of it.
    pub(crate) chosen: Option<ContentHash>,
    pub(crate) refusals: Vec<Msg>,
    /// A sortie began since the last view that showed a member not ready. The session clears every ready
    /// flag when it returns to the lobby, but views sent before that (and during the sortie) still say
    /// everyone is ready; until a view shows the reset, the lobby cannot start.
    pub(crate) stale_ready: bool,
    /// Lobby messages given before the host admitted us (the first frames of hosting), sent in order on
    /// `Welcomed`.
    pub(crate) pending: Vec<ClientMsg>,
    /// Member designs asked for, and the name and tonnage of each design in the local store.
    pub(crate) requested: BTreeSet<ContentHash>,
    pub(crate) designs: BTreeMap<ContentHash, (String, f64)>,
    /// Designs being read off the main thread (unpacking and stats of a large hull take over 10 ms).
    pub(crate) learning: BTreeSet<ContentHash>,
    pub(crate) learned: Option<(std::sync::mpsc::Sender<(ContentHash, (String, f64))>, std::sync::mpsc::Receiver<(ContentHash, (String, f64))>)>,
    /// A co-op member's own port mapping (L1005), and whether its result went to the host.
    pub(crate) member_mapper: Option<PortMapper>,
    pub(crate) mapping_reported: bool,
    /// The newest mapping outcome written to the log on the host.
    pub(crate) logged_outcome: Option<MappingOutcome>,
    /// Our own row's warning on the last connection panel (logged on change).
    pub(crate) own_warning: Option<net::msg::DropWarning>,
    pub(crate) own_sample: LinkSample,
    pub(crate) client_id: u64,
    /// A sim pool for hosting when no `Session` node is there to lend its own.
    pub(crate) pool: Option<Arc<rayon::ThreadPool>>,
}

/// Signals raised while a call runs, emitted once no borrow is held.
enum Sig {
    Lobby,
    Panel,
    Joined,
    JoinFailed(Msg),
    Left(Option<Msg>),
    Starting(f64),
}

/// What this frame's host notes ask of the lobby (`HostNote` is not `Clone`, so the feed is read once).
enum Note {
    Start,
    Saturation(bool),
    Bound(Result<Vec<std::net::SocketAddr>, String>),
    Log(String),
}

#[godot_api]
impl INode for BpLobby {
    fn init(base: Base<Node>) -> Self { Self { setup: None, issues: Vec::new(), hosting: false, display_name: String::new(), connection_log: Vec::new(), run: LobbyRun::default(), base } }
    fn ready(&mut self) { self.base_mut().set_process_priority(-90); }
    /// Unit `gd-lobby`: apply `ClientEvent::Lobby`, `Welcomed`, `Refused`, `Ended`, `BlueprintRefused` and
    /// `Saturation` from `Services::feed`; on the host also `HostNote::Session`, `UdpBound` and `Log`, and
    /// `HostingHandle::poll`; emit change signals. Inert with no lobby.
    fn process(&mut self, _delta: f64) {
        let services = services();
        self.sync_name(&services);
        let mut sigs = Vec::new();
        self.frame(&services, &mut sigs);
        drop(services);
        self.emit(sigs);
    }
}

// -------------------------------------------------------------------------------------------------------
// The frame
// -------------------------------------------------------------------------------------------------------

impl BpLobby {
    fn sync_name(&mut self, services: &Services) {
        let s = services.settings.borrow();
        if s.display_name != self.display_name { self.display_name = s.display_name.clone(); }
    }

    fn frame(&mut self, services: &Services, sigs: &mut Vec<Sig>) {
        let role = services.connection.borrow().as_ref().map(|c| c.net);
        let in_lobby = self.setup.is_some() || self.run.joining;
        match role {
            None | Some(NetRole::Offline) => {
                // The connection went away under us (left from elsewhere, or replaced by an offline sortie).
                if in_lobby { self.reset(); sigs.push(Sig::Left(None)); }
                return;
            }
            Some(r) => self.hosting = r == NetRole::Host,
        }
        // A connection this class did not build (`Session.rehost`) still logs under our own id.
        if self.run.client_id == 0 { self.run.client_id = local_client_id(); }

        // Session events and host notes of this frame, copied out of the feed.
        let (events, notes, panel, ladder) = {
            let feed = services.feed.borrow();
            let events: Vec<ClientEvent> = feed.session.iter().filter(|e| matches!(e,
                ClientEvent::Welcomed(_) | ClientEvent::Refused(_) | ClientEvent::Lobby(_) | ClientEvent::BlueprintRefused(..)
                | ClientEvent::BlueprintReady(_) | ClientEvent::Saturation { .. } | ClientEvent::SortieLoading(_) | ClientEvent::Ended(_)
                | ClientEvent::HostSilent)).cloned().collect();
            let notes: Vec<Note> = feed.host_notes.iter().filter_map(|n| match n {
                HostNote::Session(crate::seams::HostEvent::StartRequested { .. }) => Some(Note::Start),
                HostNote::Session(crate::seams::HostEvent::Saturation { on }) => Some(Note::Saturation(*on)),
                HostNote::UdpBound(r) => Some(Note::Bound(r.clone())),
                HostNote::Log(text) => Some(Note::Log(text.clone())),
                _ => None,
            }).collect();
            let panel = feed.session.iter().any(|e| matches!(e, ClientEvent::Connection(_))).then(|| feed.panel.clone()).flatten();
            let ladder: Vec<(u8, u8, bool)> = feed.events.iter().filter_map(|e| match &e.event {
                sim_systems::protocol::event::SimEvent::LadderStep { rung, sub_step, up } => Some((*rung, *sub_step, *up)),
                _ => None,
            }).collect();
            (events, notes, panel, ladder)
        };

        for e in events { self.on_event(e, services, sigs); }
        if !self.hosting {
            if let Some(panel) = panel { self.own_warnings(&panel); }
            if !ladder.is_empty() {
                let p95 = services.feed.borrow().panel.as_ref().map_or(0.0, |p| p.host_tick_p95_ms);
                let entries = ladder.into_iter().map(|(rung, sub_step, up)| (self.log_time(), LogKind::LadderStep { rung, sub_step, up, tick_p95_ms: p95 })).collect();
                self.log(entries);
            }
        }
        for n in notes { self.on_note(n, services, sigs); }
        if self.designs_learned() { sigs.push(Sig::Lobby); }

        // Hosting state beyond the session: port mapping, addresses, codes.
        let (changed, outcome) = {
            let mut c = services.connection.borrow_mut();
            match c.as_mut().and_then(|c| c.hosting.as_mut()) {
                Some(h) => (h.poll(), h.info().outcome.clone()),
                None => (false, None),
            }
        };
        if changed { sigs.push(Sig::Panel); }
        if outcome.is_some() && outcome != self.run.logged_outcome {
            self.run.logged_outcome = outcome.clone();
            if let Some(o) = outcome { self.log(vec![(self.log_time(), LogKind::PortMapping(o))]); }
        }
        self.member_mapping(services);
    }

    fn on_event(&mut self, e: ClientEvent, services: &Services, sigs: &mut Vec<Sig>) {
        match e {
            ClientEvent::Welcomed(w) => {
                self.run.joining = false;
                self.run.own_slot = Some(w.slot);
                {
                    let mut c = services.connection.borrow_mut();
                    if let Some(conn) = c.as_mut() {
                        conn.kind = logic::kind_for(w.mode);
                        // A co-op member keeps the state host succession and re-hosting need (L1005).
                        if w.mode == GameMode::Campaign && conn.net == NetRole::Client && conn.coop.is_none() {
                            conn.coop = Some(Coop::Member(net::coop::CoopMember::new(self.run.client_id)));
                        }
                    }
                }
                if self.run.name.is_empty() { self.run.name = w.lobby_name.clone(); }
                let pending = std::mem::take(&mut self.run.pending);
                if let Some(c) = services.connection.borrow_mut().as_mut() {
                    for msg in pending { c.link.send(msg); }
                }
                if self.setup.is_none() { self.setup = Some(Box::new(logic::initial_setup(w.mode, 0))); }
                if w.mode == GameMode::Campaign && !self.hosting && self.run.member_mapper.is_none() && !self.run.mapping_reported {
                    let port = port_setting(services);
                    self.run.member_mapper = Some(PortMapper::start(PortMapOptions::new(port)));
                }
                sigs.push(Sig::Joined);
                sigs.push(Sig::Lobby);
            }
            ClientEvent::Refused(r) => {
                self.log(vec![(self.log_time(), LogKind::Refused { client_id: self.run.client_id, reason: r.clone() })]);
                if self.run.joining || self.setup.is_some() {
                    self.reset();
                    sigs.push(Sig::JoinFailed(link_net::refusal_msg(&r)));
                }
            }
            ClientEvent::Lobby(view) => {
                self.on_view(*view, services);
                sigs.push(Sig::Lobby);
            }
            ClientEvent::BlueprintRefused(hash, reason) => {
                if Some(hash) == self.run.chosen {
                    let msg = logic::refusal_msg(reason);
                    if !self.run.refusals.contains(&msg) { self.run.refusals.push(msg); }
                    sigs.push(Sig::Lobby);
                }
                self.run.requested.remove(&hash);
            }
            ClientEvent::BlueprintReady(hash) => {
                if self.run.requested.contains(&hash) { self.learn_design(&hash, services); }
            }
            ClientEvent::Saturation { on } => {
                if let Some(h) = services.connection.borrow_mut().as_mut().and_then(|c| c.hosting.as_mut()) { h.on_saturation(on); }
                sigs.push(Sig::Panel);
                sigs.push(Sig::Lobby);
            }
            ClientEvent::SortieLoading(_) => {
                self.run.stale_ready = true;
                if self.setup.is_some() { sigs.push(Sig::Starting(0.0)); }
            }
            ClientEvent::HostSilent => {
                if !self.hosting { self.log(vec![(self.log_time(), LogKind::HostLost { last_packet_s: net::session::client::HOST_SILENCE.as_secs_f64() })]); }
            }
            ClientEvent::Ended(end) => {
                use crate::seams::{DropReason, SessionEnd};
                if let SessionEnd::Dropped(reason) = end {
                    if reason != DropReason::Left { self.log(vec![(self.log_time(), LogKind::Dropped { client_id: self.run.client_id, reason, sample: self.run.own_sample })]); }
                }
                let joining = self.run.joining;
                if joining || self.setup.is_some() {
                    self.reset();
                    if joining {
                        let msg = if end == SessionEnd::HostLost { Msg::new("net.join.no_answer") } else { link_net::end_msg(end) };
                        sigs.push(Sig::JoinFailed(msg));
                    } else {
                        sigs.push(Sig::Left(Some(link_net::end_msg(end))));
                    }
                }
            }
            _ => {}
        }
    }

    /// A new lobby view: the member mirror, issues, and the designs of the other members to fetch.
    fn on_view(&mut self, view: LobbyView, services: &Services) {
        let players: Vec<LobbyPlayer> = view.members.iter().map(|m| LobbyPlayer { slot: m.slot, client_id: 0, name: m.name.clone(), team: m.team, blueprint: m.blueprint, ready: m.ready, complexity: m.complexity }).collect();
        match self.setup.as_mut() {
            Some(s) if self.hosting => { s.players = players; }
            Some(s) => { s.mode = view.mode; s.ruleset = view.ruleset.clone(); s.players = players; }
            None => {
                let mut s = logic::initial_setup(view.mode, 0);
                s.ruleset = view.ruleset.clone();
                s.players = players;
                self.setup = Some(Box::new(s));
            }
        }
        self.issues = view.issues.clone();
        if self.run.stale_ready && view.members.iter().any(|m| !m.ready) { self.run.stale_ready = false; }
        let hashes: BTreeSet<ContentHash> = view.members.iter().filter_map(|m| m.blueprint).collect();
        for hash in hashes {
            if self.run.designs.contains_key(&hash) || self.run.requested.contains(&hash) || self.run.learning.contains(&hash) { continue; }
            if !self.learn_design(&hash, services) {
                self.run.requested.insert(hash);
                if let Some(c) = services.connection.borrow_mut().as_mut() { c.link.send(ClientMsg::RequestBlueprint(hash)); }
            }
        }
        self.run.view = Some(view);
    }

    /// Start reading the name and tonnage of a design the local store holds, on a background thread; false
    /// when the store does not have it yet.
    fn learn_design(&mut self, hash: &ContentHash, services: &Services) -> bool {
        if self.run.designs.contains_key(hash) || self.run.learning.contains(hash) { return true; }
        let packed = services.connection.borrow_mut().as_mut().and_then(|c| c.link.blueprint(hash));
        let Some(packed) = packed else { return false };
        self.run.requested.remove(hash);
        self.run.learning.insert(*hash);
        let tx = self.run.learned.get_or_insert_with(std::sync::mpsc::channel).0.clone();
        let catalogue = services.catalogue();
        let spawned = std::thread::Builder::new().name("bp-lobby-design".into()).spawn(move || {
            let entry = match content::share::unpack_with(&packed, &catalogue) {
                Ok(content::blueprint::Shareable::Ship(bp)) => (bp.header.name.clone(), content::stats::compute_stats(&bp, &catalogue).tonnage_t),
                _ => match net::blueprints::unpack_exterior(&packed) {
                    Ok(ext) => (ext.name.clone(), -1.0),
                    Err(_) => (String::new(), -1.0),
                },
            };
            let _ = tx.send((packed.hash, entry));
        });
        if spawned.is_err() {
            self.run.learning.remove(hash);
            self.run.designs.insert(*hash, (String::new(), -1.0));
        }
        true
    }

    /// Designs read since the last frame. True when any arrived.
    fn designs_learned(&mut self) -> bool {
        let Some((_, rx)) = self.run.learned.as_ref() else { return false };
        let mut any = false;
        while let Ok((hash, entry)) = rx.try_recv() {
            self.run.learning.remove(&hash);
            self.run.designs.insert(hash, entry);
            any = true;
        }
        any
    }

    fn on_note(&mut self, note: Note, services: &Services, sigs: &mut Vec<Sig>) {
        match note {
            Note::Start => {
                // The session accepts `Start` from the host's own client only; a co-op sortie is launched by
                // the campaign instead.
                let mode = self.run.view.as_ref().map(|v| v.mode).or(self.setup.as_ref().map(|s| s.mode));
                let Some(mode) = mode.filter(|m| *m != GameMode::Campaign && self.hosting) else { return };
                // The session does not check the lobby itself: start only from a view that can start.
                if !self.can_start(services) {
                    godot_warn!("lobby: a start request arrived while the lobby could not start; ignored");
                    return;
                }
                let mut handoff = services.handoff.borrow_mut();
                if handoff.sortie.is_none() {
                    handoff.sortie = Some(PendingSortie { kind: logic::kind_for(mode), start: HostStart::Lobby, options: SortieOptions { checkpoints: false, fixed_step: false } });
                }
            }
            Note::Saturation(on) => {
                if let Some(h) = services.connection.borrow_mut().as_mut().and_then(|c| c.hosting.as_mut()) { h.on_saturation(on); }
                sigs.push(Sig::Panel);
            }
            Note::Bound(result) => {
                let port = {
                    let mut c = services.connection.borrow_mut();
                    let h = c.as_mut().and_then(|c| c.hosting.as_mut());
                    h.map(|h| { h.on_bound(&result); h.info().port })
                };
                if let (Err(e), Some(port)) = (&result, port) {
                    godot_warn!("lobby: UDP port {port} could not be opened: {e}");
                    sigs.push(Sig::JoinFailed(Msg::new("net.host.bind_failed").arg(port_arg(port))));
                }
                sigs.push(Sig::Panel);
            }
            Note::Log(text) => self.append_log(&text),
        }
    }

    /// Our own row of the connection panel: warnings into the log as they start.
    fn own_warnings(&mut self, panel: &crate::seams::ConnectionPanel) {
        let Some(own) = self.run.own_slot else { return };
        let Some(row) = panel.players.iter().find(|p| p.slot == own) else { return };
        self.run.own_sample = LinkSample { rtt_ms: row.ping_ms as f32, loss: row.loss, frames_behind: row.frames_behind as u32, sent_kbps: 0.0, recv_kbps: 0.0 };
        if row.warning != self.run.own_warning {
            self.run.own_warning = row.warning;
            if let Some(warning) = row.warning {
                self.log(vec![(self.log_time(), LogKind::Warning { client_id: self.run.client_id, warning, sample: self.run.own_sample })]);
            }
        }
    }

    /// A co-op member's own port mapping: the first result goes to the host for the succession list.
    fn member_mapping(&mut self, services: &Services) {
        let Some(mapper) = self.run.member_mapper.as_mut() else { return };
        let Some(outcome) = mapper.poll() else { return };
        if self.run.mapping_reported { return; }
        let report = match &outcome {
            MappingOutcome::Mapped { external, .. } => net::msg::MappingReport { external: Some(*external), double_nat: false },
            MappingOutcome::DoubleNat { external, .. } => net::msg::MappingReport { external: Some(*external), double_nat: true },
            MappingOutcome::Failed { .. } | MappingOutcome::RenewFailed { .. } => net::msg::MappingReport { external: None, double_nat: false },
        };
        self.run.mapping_reported = true;
        if let Some(c) = services.connection.borrow_mut().as_mut() { c.link.send(ClientMsg::Mapping(report)); }
        self.log(vec![(self.log_time(), LogKind::PortMapping(outcome))]);
    }

    /// Forget the lobby (the connection itself belongs to `BpSession`).
    fn reset(&mut self) {
        self.setup = None;
        self.issues.clear();
        self.hosting = false;
        let client_id = self.run.client_id;
        let pool = self.run.pool.take();
        // Dropping the member's mapper deletes its mappings in the background.
        self.run = LobbyRun { client_id, pool, ..LobbyRun::default() };
    }

    fn emit(&mut self, sigs: Vec<Sig>) {
        let mut lobby_sent = false;
        let mut panel_sent = false;
        for sig in sigs {
            match sig {
                Sig::Lobby => { if !lobby_sent { lobby_sent = true; self.base_mut().emit_signal("lobby_changed", &[]); } }
                Sig::Panel => { if !panel_sent { panel_sent = true; self.base_mut().emit_signal("host_panel_changed", &[]); } }
                Sig::Joined => { self.base_mut().emit_signal("joined", &[]); }
                Sig::JoinFailed(m) => { let d = msg_dict(&m); self.base_mut().emit_signal("join_failed", &[d.to_variant()]); }
                Sig::Left(m) => { let d = m.as_ref().map_or_else(crate::convert::empty_msg, msg_dict); self.base_mut().emit_signal("left", &[d.to_variant()]); }
                Sig::Starting(s) => { self.base_mut().emit_signal("starting", &[s.to_variant()]); }
            }
        }
    }

    // ---- The session, the pool, identity ----

    /// `Session.leave(false)`, made under `base_mut` so its signal handlers may call back into `Lobby`.
    fn session_leave(&mut self) {
        let node = self.base().get_node_or_null("/root/Session");
        match node {
            Some(mut session) => { let _guard = self.base_mut(); session.call("leave", &[false.to_variant()]); }
            None => {
                // No session driver (a bare test scene): close what is there directly.
                let services = services();
                let conn = services.connection.borrow_mut().take();
                if let Some(mut conn) = conn {
                    conn.link.close();
                    if let Some(mut h) = conn.host.take() { h.shutdown(); }
                    if let Some(mut h) = conn.hosting.take() { h.stop(); }
                }
            }
        }
    }

    /// Why a new lobby cannot start now; a finished session is closed first.
    fn make_room(&mut self, busy_key: &str) -> Option<Msg> {
        let (state, has_conn) = { let s = services(); let state = s.feed.borrow().state; let c = s.connection.borrow().is_some(); (state, c) };
        if self.setup.is_some() || self.run.joining { return Some(Msg::new(busy_key)); }
        match state {
            SessionState::Idle | SessionState::Failed | SessionState::Ended => {
                if has_conn { self.session_leave(); }
                if services().connection.borrow().is_some() { return Some(Msg::new(busy_key)); }
                None
            }
            _ => Some(Msg::new(busy_key)),
        }
    }

    /// The process's sim pool, borrowed from `Session` (one pool for the life of the process, L1084).
    fn pool(&mut self) -> Arc<rayon::ThreadPool> {
        if let Some(node) = self.base().get_node_or_null("/root/Session") {
            if let Ok(mut session) = node.try_cast::<crate::session::BpSession>() {
                let mut s = session.bind_mut();
                return s.run.pool.get_or_insert_with(crate::session::host::sim_pool).clone();
            }
        }
        self.run.pool.get_or_insert_with(crate::session::host::sim_pool).clone()
    }

    fn link_options(&mut self, services: &Services) -> LinkOptions {
        let client_id = local_client_id();
        self.run.client_id = client_id;
        LinkOptions {
            identity: net::join::ClientIdentity { client_id, name: logic::clean_name(&services.settings.borrow().display_name) },
            game_version: env!("CARGO_PKG_VERSION").to_string(),
            build: if cfg!(feature = "steam") { net::msg::BuildKind::Steam } else if cfg!(debug_assertions) { net::msg::BuildKind::Dev } else { net::msg::BuildKind::GitHub },
            interp: InterpTuning::default(),
            clock: Arc::new(net::clock::SystemClock::new()),
            catalogue: services.catalogue(),
        }
    }

    fn join(&mut self, target: net::join::JoinTarget) {
        let mut sigs = Vec::new();
        if let Some(busy) = self.make_room("net.join.busy") {
            sigs.push(Sig::JoinFailed(busy));
            self.emit(sigs);
            return;
        }
        let services = services();
        let options = self.link_options(&services);
        trim_log_file();
        match link_net::join_udp(&options, &target, globalize(CACHE_DIR)) {
            Ok(link) => {
                *services.connection.borrow_mut() = Some(Connection { link: Box::new(link), host: None, hosting: None, welcome: None, intro: None, coop: None, kind: SessionKind::None, net: NetRole::Client });
                self.reset();
                self.run.client_id = options.identity.client_id;
                self.run.joining = true;
            }
            Err(e) => {
                godot_warn!("lobby: could not start the join: {e}");
                sigs.push(Sig::JoinFailed(link_net::error_msg(&e)));
            }
        }
        drop(services);
        self.emit(sigs);
    }

    fn host_only(&self) -> bool { self.hosting && self.setup.is_some() }

    /// The newest view can start, it is newer than the last sortie's ready flags, the host has not refused
    /// our design, and the session is in its lobby.
    fn can_start(&self, services: &Services) -> bool {
        self.run.view.as_ref().is_some_and(|v| v.can_start && v.mode != GameMode::Campaign)
            && !self.run.stale_ready
            && self.run.refusals.is_empty()
            && services.feed.borrow().state == SessionState::Lobby
    }

    /// Send a lobby request through our own link.
    fn request(&mut self, request: LobbyRequest) -> bool { self.send_lobby(vec![ClientMsg::Lobby(request)]) }

    /// Send lobby messages, or keep them for the welcome while the host has not admitted us yet.
    fn send_lobby(&mut self, msgs: Vec<ClientMsg>) -> bool {
        let services = services();
        let mut c = services.connection.borrow_mut();
        let Some(conn) = c.as_mut().filter(|c| c.net != NetRole::Offline) else { return false };
        match conn.link.status().state {
            crate::seams::LinkState::Joined => { for m in msgs { conn.link.send(m); } true }
            crate::seams::LinkState::Connecting => { self.run.pending.extend(msgs); true }
            _ => false,
        }
    }

    fn send_control(&self, control: HostControl) -> bool {
        let services = services();
        let c = services.connection.borrow();
        match c.as_ref().and_then(|c| c.host.as_ref()) { Some(h) => { h.send(control); true } None => false }
    }

    fn member_known(&self, slot: PlayerSlot) -> bool {
        self.run.view.as_ref().is_some_and(|v| v.members.iter().any(|m| m.slot == slot))
            || services().feed.borrow().panel.as_ref().is_some_and(|p| p.players.iter().any(|q| q.slot == slot))
    }

    fn kick_slot(&mut self, player: i64, ban: bool) -> bool {
        if !self.host_only() { return false; }
        let Ok(slot) = u8::try_from(player).map(PlayerSlot) else { return false };
        if Some(slot) == self.run.own_slot || !self.member_known(slot) { return false; }
        if !self.send_control(HostControl::Kick { slot, ban }) { return false; }
        if ban {
            if let Some(h) = services().connection.borrow_mut().as_mut().and_then(|c| c.hosting.as_mut()) { h.info.banned = h.info.banned.saturating_add(1); }
            self.emit(vec![Sig::Panel]);
        }
        true
    }

    // ---- The connection log ----

    fn log_time(&self) -> Duration { Duration::from_micros(Time::singleton().get_ticks_usec()) }

    fn log(&mut self, entries: Vec<(Duration, LogKind)>) {
        let text = logic::log_lines(entries);
        self.append_log(&text);
    }

    /// New RON lines into the file and the in-memory tail.
    fn append_log(&mut self, text: &str) {
        if text.trim().is_empty() { return; }
        for line in text.lines().filter(|l| !l.trim().is_empty()) { self.connection_log.push(line.to_string()); }
        if self.connection_log.len() > logic::LOG_LINES_KEPT {
            let cut = self.connection_log.len() - logic::LOG_LINES_KEPT;
            self.connection_log.drain(..cut);
        }
        let path = crate::app::CONNECTION_LOG;
        if !DirAccess::dir_exists_absolute("user://logs") { let _ = DirAccess::make_dir_recursive_absolute("user://logs"); }
        let file = if FileAccess::file_exists(path) { FileAccess::open(path, ModeFlags::READ_WRITE) } else { FileAccess::open(path, ModeFlags::WRITE) };
        match file {
            Some(mut f) => {
                f.seek_end();
                let mut t = text.to_string();
                if !t.ends_with('\n') { t.push('\n'); }
                f.store_string(&t);
                f.close();
            }
            None => { godot_warn!("lobby: could not write {path}"); }
        }
    }

    fn host_inner(&mut self, options: &VarDictionary) -> Result<(), Vec<Msg>> {
        if let Some(busy) = self.make_room("net.host.busy") { return Err(vec![busy]); }
        let services = services();
        let campaign = get_bool(options, "campaign").unwrap_or(false);
        let mode = if campaign { Some(GameMode::Campaign) } else { get_i64(options, "mode").and_then(GameMode::from_gd) };
        let Some(mode) = mode.filter(|m| logic::hostable(*m)) else { return Err(vec![Msg::new("net.host.bad_mode")]) };
        let campaign_id = if mode == GameMode::Campaign {
            match self.loaded_campaign_id() { Some(id) => Some(id), None => return Err(vec![Msg::new("net.host.no_campaign")]) }
        } else { None };
        let patch = read_patch(options, mode, &services).map_err(|m| vec![m])?;
        let setup = logic::apply_patch(&logic::initial_setup(mode, random_seed()), &patch, random_seed()).map_err(|m| vec![m])?;
        let port = match get_i64(options, "port").unwrap_or(0) {
            0 => port_setting(&services),
            p => match u16::try_from(p) { Ok(p) if p >= 1024 => p, _ => return Err(vec![Msg::new("net.host.port_bad").arg(MsgArg::Name(p.to_string()))]) },
        };
        let upload = match get_i64(options, "upload_kbps").unwrap_or(0) {
            0 => (services.settings.borrow().int("net.upload_kbps").max(0) as u32).max(logic::UPLOAD_MIN_KBPS),
            k => (k.clamp(logic::UPLOAD_MIN_KBPS as i64, logic::UPLOAD_MAX_KBPS as i64)) as u32,
        };
        // The port is tried here first so a port in use is reported at once, not a frame later.
        let probe = if loopback_host() { std::net::Ipv4Addr::LOCALHOST } else { std::net::Ipv4Addr::UNSPECIFIED };
        if let Err(e) = std::net::UdpSocket::bind((probe, port)) {
            godot_warn!("lobby: UDP port {port} is not free: {e}");
            return Err(vec![Msg::new("net.host.port_in_use").arg(port_arg(port))]);
        }
        let options_link = self.link_options(&services);
        let name = get_text(options, "name").map(|n| logic::clean_name(&n)).filter(|n| !n.is_empty()).unwrap_or_else(|| options_link.identity.name.clone());
        drop(services);
        let pool = self.pool();
        let services = crate::services::services();
        trim_log_file();
        let (port_end, link) = link_net::loopback_named(&options_link, &name, setup.clone(), upload, true, pool.clone(), globalize(CACHE_DIR), &[]);
        let max_clients = logic::client_limit(mode, setup.at_1_0, upload) as usize;
        let (hosting, udp) = if loopback_host() { HostingHandle::start_loopback(port, upload, max_clients) } else { HostingHandle::start(port, upload, max_clients) };
        let handle = HostHandle::spawn(Box::new(port_end), pool, services.sim_deps_factory());
        handle.send(HostControl::SetUdp { options: Some(udp), reason: crate::seams::DropReason::Left });
        let coop = campaign_id.map(|id| Coop::Host(net::coop::CoopHost::new(id)));
        *services.connection.borrow_mut() = Some(Connection { link: Box::new(link), host: Some(handle), hosting: Some(hosting), welcome: None, intro: None, coop, kind: logic::kind_for(mode), net: NetRole::Host });
        drop(services);
        let client_id = self.run.client_id;
        self.reset();
        self.run.client_id = client_id;
        self.run.name = name;
        self.hosting = true;
        self.setup = Some(Box::new(setup));
        Ok(())
    }

    /// The loaded campaign's id, from `Campaign` when it holds the theatre (single-player or co-op host).
    fn loaded_campaign_id(&self) -> Option<u64> {
        let node = self.base().get_node_or_null("/root/Campaign")?;
        let campaign = node.try_cast::<crate::campaign::BpCampaign>().ok()?;
        let b = campaign.bind();
        match b.backend.as_ref()? {
            crate::campaign::Backend::Local(theatre) => Some(theatre.header().campaign_id),
            crate::campaign::Backend::Remote { .. } => None,
        }
    }

    fn player_dict(&self, m: &net::msg::LobbyMember, panel: Option<&crate::seams::ConnectionPanel>) -> VarDictionary {
        let (name, tonnage) = m.blueprint.and_then(|h| self.run.designs.get(&h)).cloned().unwrap_or((String::new(), -1.0));
        let link = panel.and_then(|p| p.players.iter().find(|q| q.slot == m.slot)).cloned()
            .unwrap_or(PlayerLink { slot: m.slot, name: m.name.clone(), host: m.host, ..Default::default() });
        d! {
            "slot" => crate::convert::player_to_gd(m.slot),
            "name" => GString::from(m.name.as_str()),
            "team" => m.team as i64,
            "ready" => m.ready,
            "blueprint_hash" => m.blueprint.map_or_else(GString::new, |h| crate::convert::hash_hex(&h)),
            "blueprint_name" => GString::from(name.as_str()),
            "tonnage_t" => if tonnage.is_finite() { tonnage } else { -1.0 },
            "complexity" => if m.complexity.is_finite() { m.complexity } else { -1.0 },
            "upload" => m.upload.filter(|u| u.is_finite()).map_or(-1.0, |u| u.clamp(0.0, 1.0) as f64),
            "is_host" => m.host,
            "is_local" => Some(m.slot) == self.run.own_slot,
            "conn" => crate::convert::connection_dict(&link),
        }
    }

    fn issue_msgs(&self) -> Vec<Msg> {
        let names: BTreeMap<PlayerSlot, String> = self.run.view.as_ref().map(|v| v.members.iter().map(|m| (m.slot, m.name.clone())).collect()).unwrap_or_default();
        let name_of = |p: PlayerSlot| names.get(&p).cloned().unwrap_or_default();
        let mut out = logic::issue_msgs(&self.issues, self.run.own_slot, &name_of, &crate::convert::issue_msg);
        for r in &self.run.refusals { if !out.contains(r) { out.push(r.clone()); } }
        out
    }

    fn setup_dict(&self) -> Variant {
        let Some(s) = self.setup.as_ref().filter(|_| self.hosting) else { return Variant::nil() };
        let skirmish = s.skirmish.as_ref().map_or(Variant::nil(), |k| {
            let presets = services().campaign();
            let preset = presets.difficulty.iter().find(|p| p.sliders == k.difficulty).map_or(String::new(), |p| p.id.as_str().to_string());
            d! {
                "template" => k.template.to_gd(),
                "act" => k.act.to_gd(),
                "enemy_per_player" => k.enemy_per_player as i64,
                "difficulty" => crate::convert::difficulty_dict(&k.difficulty, &preset),
            }.to_variant()
        });
        d! {
            "seed" => (s.seed & i64::MAX as u64) as i64,
            "weather" => s.weather.to_gd(),
            "sea_state" => s.sea_state.0 as i64,
            "time_of_day" => s.time_of_day.to_gd(),
            "skirmish" => skirmish,
        }.to_variant()
    }
}

/// The settable keys of a host or setup Dictionary. Keys absent from `dict` stay as they are.
fn read_patch(dict: &VarDictionary, mode: GameMode, services: &Services) -> Result<logic::SetupPatch, Msg> {
    let mut p = logic::SetupPatch { mode: Some(mode), ..Default::default() };
    p.ruleset = get_text(dict, "ruleset");
    p.tonnage_cap_t = get_f64(dict, "tonnage_cap_t");
    let mut conditions = dict.clone();
    let skirmish = get_dict(dict, "skirmish");
    if let Some(sk) = &skirmish {
        // The skirmish Dictionary of `Session.start_skirmish` also carries the conditions.
        for key in ["weather", "sea_state", "time_of_day", "seed"] {
            if !dict.contains_key(key) { if let Some(v) = sk.get(key) { conditions.set(key, &v); } }
        }
    }
    p.seed = get_i64(&conditions, "seed").map(|s| s.unsigned_abs());
    p.weather = match get_i64(&conditions, "weather") { None => None, Some(v) => Some(WeatherKind::from_gd(v).ok_or_else(|| Msg::new("session.problem.weather"))?) };
    p.sea_state = match get_i64(&conditions, "sea_state") { None => None, Some(v) if (0..=8).contains(&v) => Some(SeaState(v as u8)), Some(_) => return Err(Msg::new("session.problem.sea_state")) };
    p.time_of_day = match get_i64(&conditions, "time_of_day") { None => None, Some(v) => Some(TimeOfDay::from_gd(v).ok_or_else(|| Msg::new("session.problem.time_of_day"))?) };
    if let Some(sk) = skirmish {
        let template = match get_i64(&sk, "template") { None => content::campaign::MissionType::ConvoyRaid, Some(v) => content::campaign::MissionType::from_gd(v).ok_or_else(|| Msg::new("session.problem.template"))? };
        // Only the acts this build ships (Act 1 in 0.1.0), as in offline skirmish.
        let shipped = crate::content_api::model::shipped_acts(&content::campaign::load::shipped_campaign().acts);
        let act = match get_i64(&sk, "act") { None => content::enums::Act::Act1, Some(v) => content::enums::Act::from_gd(v).filter(|a| shipped.contains(a)).ok_or_else(|| Msg::new("session.problem.act"))? };
        let enemy_per_player = get_i64(&sk, "enemy_per_player").unwrap_or(1).clamp(1, logic::MAX_ENEMY_PER_PLAYER as i64) as u8;
        let campaign = services.campaign();
        let difficulty = get_dict(&sk, "difficulty").map(|d| {
            crate::convert::difficulty_from_dict(&d).unwrap_or_else(|| {
                let name = get_text(&d, "preset").unwrap_or_default();
                campaign.difficulty.iter().find(|p| p.id.as_str() == name).map(|p| p.sliders).unwrap_or_default()
            })
        }).unwrap_or_default();
        p.skirmish = Some(SkirmishOptions { template, act, enemy_per_player, difficulty });
    }
    Ok(p)
}

/// `--loopback-host` after `--` on the command line: host for this machine only (bound to 127.0.0.1, no
/// port mapping), so two copies of the game can test a lobby without touching the router or the firewall.
fn loopback_host() -> bool { godot::classes::Os::singleton().get_cmdline_user_args().as_slice().iter().any(|a| *a == "--loopback-host") }

/// `user://` path as an OS path.
fn globalize(path: &str) -> std::path::PathBuf { std::path::PathBuf::from(ProjectSettings::singleton().globalize_path(path).to_string()) }

/// The install's client id (`app::install_client_id`, created on first use).
fn local_client_id() -> u64 { crate::app::install_client_id() }

/// A port number as message text: written as typed, never grouped like a count ("47610", not "47,610").
pub(crate) fn port_arg(port: u16) -> MsgArg { MsgArg::Name(port.to_string()) }

fn port_setting(services: &Services) -> u16 {
    u16::try_from(services.settings.borrow().int("net.port")).ok().filter(|p| *p >= 1024).unwrap_or(net::DEFAULT_PORT)
}

/// A seed for a new lobby: client-side, never read back by a running Sim.
fn random_seed() -> u64 {
    let b = net::join::LobbySecret::generate().0;
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]).max(1)
}

/// Keep the connection log bounded across sessions: over [`LOG_FILE_MAX`], keep its newest half.
fn trim_log_file() {
    let path = crate::app::CONNECTION_LOG;
    if !FileAccess::file_exists(path) { return; }
    let Some(f) = FileAccess::open(path, ModeFlags::READ) else { return };
    let len = f.get_length();
    drop(f);
    if len <= LOG_FILE_MAX { return; }
    let text = FileAccess::get_file_as_string(path).to_string();
    let lines: Vec<&str> = text.lines().collect();
    let keep = lines[lines.len() / 2..].join("\n");
    if let Some(mut f) = FileAccess::open(path, ModeFlags::WRITE) { f.store_string(&format!("{keep}\n")); f.close(); }
}

/// An `IssueDict` for a problem that is not a validation issue.
fn problem_dict(msg: &Msg) -> VarDictionary { d! { "text" => msg_dict(msg), "fatal" => true, "part" => -1i64, "cell" => Variant::nil() } }

#[godot_api]
impl BpLobby {
    /// `lobby()` changed.
    #[signal]
    fn lobby_changed();
    /// `host_panel()` changed (mapping result, join code, saturation).
    #[signal]
    fn host_panel_changed();
    /// We are in a lobby (as host or member).
    #[signal]
    fn joined();
    /// Joining failed or was refused; `text` is a `MsgDict` (a protocol mismatch names both versions).
    #[signal]
    fn join_failed(text: VarDictionary);
    /// We left or were removed; `text` is a `MsgDict` (empty when we chose to leave).
    #[signal]
    fn left(text: VarDictionary);
    /// The host started the sortie; it begins in `seconds`.
    #[signal]
    fn starting(seconds: f64);

    // ---- Hosting (L992-999, L1031) ----

    /// Text to show before binding the port: the OS firewall prompt that is about to appear (`MsgDict`).
    #[func]
    fn firewall_notice(&self) -> VarDictionary {
        msg_dict(&Msg::new("net.host.firewall").arg(port_arg(port_setting(&services()))))
    }
    /// Host a lobby. `options` keys: `mode: int` (`Bp.MODE_*`), `ruleset: String` (ruleset id), `name: String`,
    /// `port: int` (0 = 47610), `upload_kbps: int` (0 = 1000), `campaign: bool` (co-op on the loaded
    /// campaign), `skirmish: Dictionary` (as in `Session.start_skirmish`). Test-binds the port on the main
    /// thread so a port in use is reported at once, then builds the session (`link::net::loopback`), starts
    /// port mapping (`HostingHandle::start`) and asks the clock thread to bind (`HostControl::SetUdp`).
    /// Returns `Array[MsgDict]` of problems; empty = hosting is starting, and `host_panel_changed` follows
    /// when the port is bound (`HostPanelDict.bound`).
    #[func]
    fn host(&mut self, options: VarDictionary) -> Array<VarDictionary> {
        match self.host_inner(&options) {
            Ok(()) => { self.emit(vec![Sig::Panel, Sig::Lobby]); Array::new() }
            Err(problems) => problems.iter().map(msg_dict).collect(),
        }
    }
    /// Returns `HostPanelDict`. Meaningful only while hosting.
    #[func]
    fn host_panel(&self) -> VarDictionary {
        let services = services();
        let c = services.connection.borrow();
        let hosting = c.as_ref().and_then(|c| c.hosting.as_ref()).filter(|_| self.hosting);
        let info = hosting.map(|h| h.info().clone()).unwrap_or_default();
        let mode = self.run.view.as_ref().map(|v| v.mode).or(self.setup.as_ref().map(|s| s.mode)).unwrap_or(GameMode::Sandbox);
        let at_1_0 = self.setup.as_ref().is_some_and(|s| s.at_1_0);
        let manual: VarArray = logic::manual_steps(&info).iter().map(|m| msg_dict(m).to_variant()).collect();
        let portmap = if hosting.is_some() { info.portmap } else { PortMapState::Idle };
        d! {
            "hosting" => hosting.is_some(),
            "bound" => info.bound,
            "port" => info.port as i64,
            "portmap" => portmap.to_gd(),
            "portmap_text" => if hosting.is_some() { msg_dict(&link_net::portmap_msg(info.outcome.as_ref())) } else { crate::convert::empty_msg() },
            "external_address" => GString::from(info.external_address.as_str()),
            "lan_address" => GString::from(info.lan_address.as_str()),
            "join_code" => GString::from(info.join_code.as_str()),
            "lobby_key" => GString::from(info.lobby_key.as_str()),
            "vpn_address" => GString::from(info.vpn_address.as_str()),
            "vpn_join_code" => GString::from(info.vpn_join_code.as_str()),
            "upload_kbps" => info.upload_kbps as i64,
            "client_limit" => if hosting.is_some() { logic::client_limit(mode, at_1_0, info.upload_kbps) as i64 } else { 0 },
            "saturated" => info.saturated,
            "manual_steps" => manual,
            "banned" => info.banned as i64,
        }
    }
    #[func]
    fn set_upload_kbps(&mut self, kbps: i64) {
        if !self.host_only() { return; }
        let kbps = kbps.clamp(logic::UPLOAD_MIN_KBPS as i64, logic::UPLOAD_MAX_KBPS as i64) as u32;
        if let Some(h) = services().connection.borrow_mut().as_mut().and_then(|c| c.hosting.as_mut()) { h.set_upload_kbps(kbps); }
        self.send_control(HostControl::SetUploadKbps(kbps));
        crate::settings::write("net.upload_kbps", crate::settings::SettingValue::Int(kbps as i64));
        self.emit(vec![Sig::Panel]);
    }
    /// New lobby secret: old join codes and keys stop working. Members connected over UDP drop with
    /// `net.drop.secret_regenerated` and may rejoin into their held slot with the new code (L1001).
    #[func]
    fn regenerate_secret(&mut self) {
        if !self.host_only() { return; }
        let options = {
            let services = services();
            let mut c = services.connection.borrow_mut();
            c.as_mut().and_then(|c| c.hosting.as_mut()).map(|h| h.regenerate_secret(link_net::MAX_UDP_CLIENTS))
        };
        if let Some(options) = options { self.send_control(HostControl::SetUdp { options: Some(options), reason: crate::seams::DropReason::SecretRegenerated }); }
        self.emit(vec![Sig::Panel]);
    }
    /// Host only: change the lobby's setup (`LobbyRequest::SetSetup` from the host's own client). `setup`
    /// takes the settable keys of `LobbyDict` (`mode`, `ruleset: String`, `seed`, `weather`, `sea_state`,
    /// `time_of_day`, `skirmish`). Returns false for a member or an invalid value.
    #[func]
    fn set_setup(&mut self, setup: VarDictionary) -> bool {
        if !self.host_only() { return false; }
        let Some(base) = self.setup.as_deref().cloned() else { return false };
        let mode = match get_i64(&setup, "mode") { None => base.mode, Some(v) => match GameMode::from_gd(v) { Some(m) => m, None => return false } };
        let services = services();
        let Ok(patch) = read_patch(&setup, mode, &services) else { return false };
        let Ok(next) = logic::apply_patch(&base, &patch, random_seed()) else { return false };
        if let Some(c) = services.connection.borrow_mut().as_mut() { c.kind = logic::kind_for(next.mode); }
        drop(services);
        if !self.request(LobbyRequest::SetSetup(Box::new(next.clone()))) { return false; }
        self.setup = Some(Box::new(next));
        self.emit(vec![Sig::Lobby, Sig::Panel]);
        true
    }
    #[func]
    fn kick(&mut self, player: i64) -> bool { self.kick_slot(player, false) }
    /// Ban the player's client id for this session.
    #[func]
    fn ban(&mut self, player: i64) -> bool { self.kick_slot(player, true) }
    /// Host only: start the sortie when `lobby().can_start` (`LobbyRequest::Start`; the session answers
    /// with `HostEvent::StartRequested`, and the sortie is handed to `BpSession` as `HostStart::Lobby`).
    #[func]
    fn start(&mut self) -> bool {
        if !self.host_only() { return false; }
        if !self.can_start(&services()) { return false; }
        self.request(LobbyRequest::Start)
    }

    // ---- Joining (L1001-1003) ----

    /// Check a join code without connecting (`link::net::parse_join_code`): `{ "valid": bool,
    /// "address": String, "port": int, "protocol": int, "protocol_ok": bool, "text": MsgDict }`. `text` is
    /// the `net.join_code.*` sentence for an invalid code and empty otherwise.
    #[func]
    fn check_join_code(&self, code: GString) -> VarDictionary {
        match link_net::parse_join_code(&code.to_string()) {
            Ok(c) => d! {
                "valid" => true,
                "address" => GString::from(c.addr.ip().to_string().as_str()),
                "port" => c.addr.port() as i64,
                "protocol" => c.protocol as i64,
                "protocol_ok" => c.protocol == net::PROTOCOL_VERSION,
                "text" => crate::convert::empty_msg(),
            },
            Err(e) => {
                let protocol = if let crate::seams::JoinCodeError::ProtocolMismatch { code, .. } = e { code as i64 } else { 0 };
                d! {
                    "valid" => false,
                    "address" => GString::new(),
                    "port" => 0i64,
                    "protocol" => protocol,
                    "protocol_ok" => !matches!(e, crate::seams::JoinCodeError::ProtocolMismatch { .. }),
                    "text" => msg_dict(&link_net::join_code_msg(e)),
                }
            }
        }
    }
    /// Join by code. Returns false when the code does not parse; otherwise `joined` or `join_failed` follows.
    #[func]
    fn join_code(&mut self, code: GString) -> bool {
        let Ok(code) = link_net::parse_join_code(&code.to_string()) else { return false };
        self.join(net::join::JoinTarget::Code(code));
        true
    }
    /// Join by `address:port` and the 16-character lobby key.
    #[func]
    fn join_address(&mut self, address: GString, key: GString) -> bool {
        let Ok(target) = link_net::parse_address(&address.to_string(), &key.to_string()) else { return false };
        self.join(target);
        true
    }
    /// Leave the lobby or stop hosting (deletes port mappings).
    #[func]
    fn leave(&mut self) {
        let was = self.setup.is_some() || self.run.joining;
        let ours = services().connection.borrow().as_ref().is_some_and(|c| c.net != NetRole::Offline);
        self.reset();
        if ours { self.session_leave(); }
        if was { self.emit(vec![Sig::Left(None)]); }
    }

    // ---- In the lobby ----

    /// True while in a lobby as host or member.
    #[func]
    fn is_active(&self) -> bool { self.setup.is_some() }
    #[func]
    fn is_host(&self) -> bool { self.hosting }
    /// Returns `LobbyDict`, or an empty Dictionary outside a lobby. Re-read on `lobby_changed`.
    #[func]
    fn lobby(&self) -> VarDictionary {
        let Some(setup) = self.setup.as_ref() else { return VarDictionary::new() };
        let services = services();
        let panel = services.feed.borrow().panel.clone();
        let view = self.run.view.as_ref();
        let mode = view.map_or(setup.mode, |v| v.mode);
        let ruleset = view.map_or(&setup.ruleset, |v| &v.ruleset);
        let players: VarArray = view.map(|v| v.members.iter().map(|m| self.player_dict(m, panel.as_deref()).to_variant()).collect()).unwrap_or_default();
        let issues: VarArray = self.issue_msgs().iter().map(|m| msg_dict(m).to_variant()).collect();
        let saturated = view.is_some_and(|v| v.saturated) || panel.as_ref().is_some_and(|p| p.saturated);
        let max_players = view.map_or_else(|| sim_systems::modes::player_limit(mode, setup.at_1_0, 1_000), |v| v.max_players);
        d! {
            "mode" => mode.to_gd(),
            "name" => GString::from(view.map_or(self.run.name.as_str(), |v| v.name.as_str())),
            "ruleset" => crate::convert::ruleset_dict(&logic::ruleset_id(mode, ruleset), ruleset),
            "players" => players,
            "max_players" => max_players as i64,
            "issues" => issues,
            "can_start" => self.can_start(&services),
            "saturated" => saturated,
            "is_host" => self.hosting,
            "setup" => self.setup_dict(),
        }
    }
    #[func]
    fn set_ready(&mut self, ready: bool) { if self.setup.is_some() { self.request(LobbyRequest::SetReady(ready)); } }
    /// PvP team 0 or 1.
    #[func]
    fn set_team(&mut self, team: i64) {
        if self.setup.is_none() { return; }
        if let Ok(team @ 0..=1) = u8::try_from(team) { self.request(LobbyRequest::SetTeam(team)); }
    }
    /// Field a design from a blueprint file. Checks it against the lobby's ruleset first and returns
    /// `Array[IssueDict]`; empty = selected: `ClientMsg::UploadBlueprint` then
    /// `LobbyRequest::SetBlueprint(hash)`. The host re-validates it; a refusal arrives as
    /// `ClientEvent::BlueprintRefused` and shows in `lobby().issues`.
    #[func]
    fn select_blueprint(&mut self, path: GString) -> Array<VarDictionary> {
        let Some(setup) = self.setup.as_ref() else { return [problem_dict(&Msg::new("net.lobby.not_in_lobby"))].into_iter().collect() };
        let ruleset = self.run.view.as_ref().map_or_else(|| setup.ruleset.clone(), |v| v.ruleset.clone());
        let pvp = logic::is_pvp(self.run.view.as_ref().map_or(setup.mode, |v| v.mode));
        let bp = match load_design(&path.to_string()) { Ok(bp) => bp, Err(d) => return [d].into_iter().collect() };
        let services = services();
        let catalogue = services.catalogue();
        let mut ruleset = ruleset;
        if pvp { ruleset.luau = false; }
        let ctx = content::validate::ValidationContext { ruleset: &ruleset, available: None, enemy_class: false, complexity: None };
        let report = content::validate::validate_blueprint(&bp, &catalogue, &ctx);
        if !report.issues.is_empty() { return report.issues.iter().map(crate::convert::issue_dict).collect(); }
        let packed = content::share::pack(&content::blueprint::Shareable::Ship(Box::new(bp.clone())));
        if packed.compressed.len() > content::blueprint::MAX_COMPRESSED_BYTES { return [problem_dict(&Msg::new("net.lobby.refused.too_large"))].into_iter().collect(); }
        let hash = packed.hash;
        drop(services);
        let sent = self.send_lobby(vec![ClientMsg::UploadBlueprint(packed), ClientMsg::Lobby(LobbyRequest::SetBlueprint(hash))]);
        if !sent { return [problem_dict(&Msg::new("net.error.not_connected"))].into_iter().collect(); }
        self.run.designs.insert(hash, (bp.header.name.clone(), report.stats.tonnage_t));
        self.run.chosen = Some(hash);
        self.run.refusals.clear();
        self.emit(vec![Sig::Lobby]);
        Array::new()
    }
    /// Display name, chosen locally; there are no accounts (L1003). Stored in settings. It is sent once,
    /// in the hello (`net::join::ClientIdentity::name`), so a change applies to the next lobby joined. The
    /// host trims it to 24 characters.
    #[func]
    fn set_display_name(&mut self, name: GString) {
        let name = logic::clean_name(&name.to_string());
        if name == self.display_name { return; }
        self.display_name = name.clone();
        crate::settings::write_display_name(&name);
    }
    #[func]
    fn display_name(&self) -> GString { GString::from(self.display_name.as_str()) }
    /// The local connection log as text (drops, warnings, ladder steps, mapping results; L1039).
    #[func]
    fn connection_log_text(&self) -> GString {
        let mut text = self.connection_log.join("\n");
        if !text.is_empty() { text.push('\n'); }
        GString::from(text.as_str())
    }
}

/// Read a design from a path the screens give: `user://` and `res://` files, else a stock path. The design
/// is canonicalised so its hash is the one the host computes.
fn load_design(path: &str) -> Result<content::blueprint::Blueprint, VarDictionary> {
    let path = path.trim();
    if path.is_empty() { return Err(problem_dict(&Msg::new("net.lobby.no_blueprint"))); }
    let mut bp = if path.starts_with("user://") || path.starts_with("res://") {
        if !FileAccess::file_exists(path) { return Err(problem_dict(&Msg::new("net.lobby.design_missing"))); }
        let text = FileAccess::get_file_as_string(path).to_string();
        content::blueprint::io::load_blueprint(&text, path).map(|l| l.blueprint).map_err(|e| crate::convert::error_issue_dict(&e))?
    } else {
        let key = path.replace('\\', "/");
        let key = key.trim_start_matches("./");
        let key = key.strip_prefix("blueprints/").unwrap_or(key);
        content::campaign::load::stock_blueprint(key).map_err(|e| crate::convert::error_issue_dict(&e))?
    };
    content::blueprint::io::canonicalize_blueprint(&mut bp);
    bp.header.hash = content::blueprint::io::compute_hash(&bp);
    Ok(bp)
}
