//! Unit `net-steam` (phase 2; schedule last). Steam Networking Sockets with Steam Datagram Relay through
//! renet2_steam, compiled only with the `steam` cargo feature, which is off by default. DESIGN.md L988, L1011,
//! L1203: Rust owns the Steam API through the steamworks crate renet2_steam uses; the Steamworks SDK is never
//! committed (the steamworks crate's build fetches it); lobbies, invites, Workshop, Cloud and auth reach the UI
//! through gdclient; no Godot Steam plugin. The relay hides IP addresses. Join codes stay available.
//!
//! The caller owns the `steamworks::Client` and pumps its callbacks (`Client::run_callbacks`) once per frame
//! on the main thread; these backends only use the networking-sockets interface. Client ids are Steam ids
//! (`SteamId::raw`). Sessions still identify players by the id in their hello, as on every backend.
//! The relay hides addresses, so `client_addr` and `local_addrs` are always empty.
use super::{ClientTransport, ConnState, DisconnectCause, LinkStats, ServerTransport, ServerTransportEvent, TransportKind};
use crate::channel::Channel;
use crate::error::NetError;
use renet2_steam::steamworks;
use std::net::SocketAddr;
use std::time::Duration;

/// Who may open a connection to a Steam host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SteamAccess {
    /// Anyone who knows the host's Steam id.
    Public,
    /// The host's Steam friends.
    FriendsOnly,
    /// Members of this Steam lobby (raw lobby id): the usual setting, since invites go through lobbies.
    InLobby(u64),
    /// Nobody new.
    Private,
}

impl SteamAccess {
    fn permission(&self) -> renet2_steam::AccessPermission {
        match self {
            SteamAccess::Public => renet2_steam::AccessPermission::Public,
            SteamAccess::FriendsOnly => renet2_steam::AccessPermission::FriendsOnly,
            SteamAccess::InLobby(id) => renet2_steam::AccessPermission::InLobby(steamworks::LobbyId::from_raw(*id)),
            SteamAccess::Private => renet2_steam::AccessPermission::Private,
        }
    }
}

/// renet2_steam fixes its client limit at construction; it is built with this ceiling and net enforces the
/// lobby's own limit on top, so `set_max_clients` works like the other backends.
const STEAM_MAX_CLIENTS: usize = 64;

/// Host side over Steam Networking Sockets (P2P listen socket, relayed).
pub struct SteamServerBackend {
    renet: renet2::RenetServer,
    transport: renet2_steam::SteamServerTransport,
    events: std::collections::VecDeque<ServerTransportEvent>,
    max_clients: usize,
    /// Connections accepted by the transport and admitted under `max_clients` (reported as connected).
    admitted: Vec<u64>,
    local_drops: Vec<u64>,
}

impl SteamServerBackend {
    /// Open the P2P listen socket. `max_clients` is the lobby's client limit.
    pub fn new(client: &steamworks::Client, max_clients: usize, access: SteamAccess) -> Result<SteamServerBackend, NetError> {
        let config = renet2_steam::SteamServerConfig { max_clients: STEAM_MAX_CLIENTS, access_permission: access.permission() };
        let transport = renet2_steam::SteamServerTransport::new(client, config).map_err(|_| NetError::Transport("steam listen socket: invalid handle".into()))?;
        Ok(SteamServerBackend {
            renet: renet2::RenetServer::new(super::netcode::connection_config()),
            transport,
            events: std::collections::VecDeque::new(),
            max_clients: max_clients.min(STEAM_MAX_CLIENTS),
            admitted: Vec::new(),
            local_drops: Vec::new(),
        })
    }

    /// Change who may connect; applies to new connections only.
    pub fn set_access(&mut self, access: SteamAccess) { self.transport.set_access_permissions(access.permission()); }

    fn pump_events(&mut self) {
        while let Some(e) = self.renet.get_event() {
            match e {
                renet2::ServerEvent::ClientConnected { client_id } => {
                    if self.admitted.len() >= self.max_clients {
                        // Over the lobby limit: close it before the session ever sees it.
                        self.transport.disconnect_client(client_id, &mut self.renet, false);
                        continue;
                    }
                    self.admitted.push(client_id);
                    self.local_drops.retain(|&c| c != client_id);
                    self.events.push_back(ServerTransportEvent::Connected { client: client_id });
                }
                renet2::ServerEvent::ClientDisconnected { client_id, reason } => {
                    let Some(i) = self.admitted.iter().position(|&c| c == client_id) else { continue };
                    self.admitted.swap_remove(i);
                    let local = self.local_drops.iter().position(|&c| c == client_id).map(|i| self.local_drops.swap_remove(i)).is_some();
                    let cause = match reason {
                        _ if local => DisconnectCause::Local,
                        renet2::DisconnectReason::DisconnectedByServer => DisconnectCause::Local,
                        renet2::DisconnectReason::Transport | renet2::DisconnectReason::DisconnectedByClient => DisconnectCause::Remote,
                        other => DisconnectCause::Error(other.to_string()),
                    };
                    self.events.push_back(ServerTransportEvent::Disconnected { client: client_id, cause });
                }
            }
        }
    }

    fn admitted(&self, client: u64) -> bool { self.admitted.contains(&client) && self.renet.is_connected(client) }
}

fn link_stats(info: renet2::NetworkInfo) -> LinkStats {
    let finite = |v: f64| if v.is_finite() && v > 0.0 { v as f32 } else { 0.0 };
    LinkStats {
        rtt_ms: finite(info.rtt * 1000.0),
        loss: finite(info.packet_loss).min(1.0),
        sent_kbps: finite(info.bytes_sent_per_second * 8.0 / 1000.0),
        recv_kbps: finite(info.bytes_received_per_second * 8.0 / 1000.0),
    }
}

impl ServerTransport for SteamServerBackend {
    fn kind(&self) -> TransportKind { TransportKind::Steam }
    fn update(&mut self, dt: Duration, _unix_now: Duration) -> Result<(), NetError> {
        self.renet.update(dt);
        self.transport.update(&mut self.renet);
        self.pump_events();
        Ok(())
    }
    fn poll_event(&mut self) -> Option<ServerTransportEvent> { self.events.pop_front() }
    fn clients(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self.admitted.iter().copied().filter(|&c| self.renet.is_connected(c)).collect();
        ids.sort_unstable();
        ids
    }
    fn receive(&mut self, client: u64, channel: Channel) -> Option<Vec<u8>> {
        if !Channel::CLIENT.contains(&channel) || !self.admitted(client) {
            return None;
        }
        self.renet.receive_message(client, channel.id()).map(|b| b.to_vec())
    }
    fn can_send(&self, client: u64, channel: Channel, bytes: usize) -> bool {
        Channel::SERVER.contains(&channel) && self.admitted(client) && self.renet.can_send_message(client, channel.id(), bytes)
    }
    fn send(&mut self, client: u64, channel: Channel, bytes: Vec<u8>) {
        if Channel::SERVER.contains(&channel) && self.admitted(client) {
            self.renet.send_message(client, channel.id(), bytes);
        }
    }
    fn flush(&mut self) { self.transport.send_packets(&mut self.renet); }
    fn stats(&self, client: u64) -> Option<LinkStats> {
        if !self.admitted.contains(&client) {
            return None;
        }
        self.renet.network_info(client).ok().map(link_stats)
    }
    fn client_addr(&self, _client: u64) -> Option<SocketAddr> { None }
    fn disconnect(&mut self, client: u64) {
        if self.admitted.contains(&client) {
            if !self.local_drops.contains(&client) {
                self.local_drops.push(client);
            }
            // Flush what is queued (a refusal or kick message) before closing.
            self.transport.disconnect_client(client, &mut self.renet, true);
            self.pump_events();
        }
    }
    fn disconnect_all(&mut self) {
        for &c in &self.admitted {
            if !self.local_drops.contains(&c) {
                self.local_drops.push(c);
            }
        }
        self.transport.disconnect_all(&mut self.renet, true);
        self.pump_events();
    }
    fn set_max_clients(&mut self, n: usize) { self.max_clients = n.min(STEAM_MAX_CLIENTS); }
    fn local_addrs(&self) -> Vec<SocketAddr> { Vec::new() }
}

/// renet2's received-rate window less its current, incomplete bucket (6 s window, 300 ms buckets).
const RATE_WINDOW: Duration = Duration::from_millis(5_700);

/// Client side over Steam Networking Sockets.
pub struct SteamClientBackend {
    renet: renet2::RenetClient,
    transport: renet2_steam::SteamClientTransport,
    ended: Option<DisconnectCause>,
    /// Host-loss clock (L1005). The Steam transport hides individual packets, so this is read off renet2's
    /// received-bytes rate, a sum over its last 5.7 s of completed 300 ms buckets: while the rate is non-zero
    /// the host was heard inside that window and the clock reads zero; when it falls to zero the newest bytes
    /// just left the window, so the clock jumps to [`RATE_WINDOW`] and counts on from there. Below the window
    /// it reads low, never high, and past it (where the 10 s threshold sits) it is accurate to one bucket.
    silent_for: Duration,
    last_recv_rate: f64,
}

impl SteamClientBackend {
    /// Connect to a host by its raw Steam id.
    pub fn connect(client: &steamworks::Client, host_steam_id: u64) -> Result<SteamClientBackend, NetError> {
        let id = steamworks::SteamId::from_raw(host_steam_id);
        let transport = renet2_steam::SteamClientTransport::new(client, &id).map_err(|_| NetError::Transport("steam connect: invalid handle".into()))?;
        Ok(SteamClientBackend {
            renet: renet2::RenetClient::new(super::netcode::connection_config(), false),
            transport,
            ended: None,
            silent_for: Duration::ZERO,
            last_recv_rate: 0.0,
        })
    }

    fn is_ended(&self) -> bool { self.ended.is_some() || self.renet.is_disconnected() }

    fn note_end(&mut self) {
        if self.ended.is_some() {
            return;
        }
        if let Some(reason) = self.renet.disconnect_reason() {
            self.ended = Some(match reason {
                renet2::DisconnectReason::DisconnectedByClient => DisconnectCause::Local,
                renet2::DisconnectReason::DisconnectedByServer => DisconnectCause::Remote,
                renet2::DisconnectReason::Transport => steam_end_cause(self.transport.disconnect_reason()),
                other => DisconnectCause::Error(other.to_string()),
            });
        }
    }
}

/// Steam's end reason: a timeout, a peer that closed, or a local problem.
fn steam_end_cause(end: Option<steamworks::networking_types::NetConnectionEnd>) -> DisconnectCause {
    use steamworks::networking_types::NetConnectionEnd as E;
    match end {
        Some(E::RemoteTimeout | E::MiscTimeout) => DisconnectCause::TimedOut,
        Some(E::App(_) | E::MiscPeerSentNoConnection) | None => DisconnectCause::Remote,
        Some(other) => DisconnectCause::Error(format!("{other:?}")),
    }
}

impl ClientTransport for SteamClientBackend {
    fn kind(&self) -> TransportKind { TransportKind::Steam }
    fn update(&mut self, dt: Duration, _unix_now: Duration) -> Result<(), NetError> {
        if self.ended.is_some() {
            return Ok(());
        }
        self.renet.update(dt);
        self.transport.update(&mut self.renet);
        let rate = self.renet.bytes_received_per_sec();
        let rate = if rate.is_finite() { rate } else { 0.0 };
        if rate > 0.0 {
            self.silent_for = Duration::ZERO;
        } else if self.last_recv_rate > 0.0 {
            self.silent_for = RATE_WINDOW;
        } else {
            self.silent_for += dt;
        }
        self.last_recv_rate = rate;
        self.note_end();
        Ok(())
    }
    fn state(&self) -> ConnState {
        if let Some(c) = &self.ended {
            return ConnState::Disconnected(c.clone());
        }
        if self.renet.is_connected() {
            ConnState::Connected
        } else if self.renet.is_disconnected() {
            ConnState::Disconnected(DisconnectCause::Remote)
        } else {
            ConnState::Connecting
        }
    }
    fn receive(&mut self, channel: Channel) -> Option<Vec<u8>> {
        if !Channel::SERVER.contains(&channel) || self.is_ended() {
            return None;
        }
        self.renet.receive_message(channel.id()).map(|b| b.to_vec())
    }
    fn can_send(&self, channel: Channel, bytes: usize) -> bool {
        Channel::CLIENT.contains(&channel) && !self.is_ended() && self.renet.can_send_message(channel.id(), bytes)
    }
    fn send(&mut self, channel: Channel, bytes: Vec<u8>) {
        if Channel::CLIENT.contains(&channel) && !self.is_ended() {
            self.renet.send_message(channel.id(), bytes);
        }
    }
    fn flush(&mut self) -> Result<(), NetError> {
        if self.is_ended() {
            return Err(NetError::NotConnected);
        }
        match self.transport.send_packets(&mut self.renet) {
            Ok(()) => Ok(()),
            Err(steamworks::SteamError::NoConnection) => {
                self.note_end();
                Err(NetError::NotConnected)
            }
            Err(e) => Err(NetError::Transport(format!("steam send: {e}"))),
        }
    }
    fn stats(&self) -> LinkStats { link_stats(self.renet.network_info()) }
    fn time_since_last_packet(&self) -> Duration { self.silent_for }
    fn disconnect(&mut self) {
        if self.is_ended() {
            return;
        }
        self.renet.disconnect();
        self.transport.disconnect();
        self.ended = Some(DisconnectCause::Local);
    }
}
