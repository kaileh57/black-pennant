//! Unit `net-transport`. The in-memory and UDP backends: renet2 `RenetServer` / `RenetClient` over
//! renet2_netcode transports. DESIGN.md L982-990 (backends), L994 (UDP port 47610 bound only when the player
//! hosts), L1001 (netcode private key derived from the lobby secret; the joining client mints its own connect
//! token, so traffic is encrypted and the secret is the lobby password), L1003 (client id = install id).
//!
//! Channel lists come from [`crate::channel::Channel::SERVER`] / `CLIENT` and `Channel::spec` in that order
//! (renet2 serves channels in list order). `ConnectionConfig::available_bytes_per_tick` is fixed when the
//! connection is built, so it is set to the 768 kbps ceiling of the adaptive cap plus headroom; the real
//! per-client limit is net's own budget (`quality::BandwidthCap`, enforced by the session).
//!
//! In-memory: `renet2_netcode::new_memory_sockets` with `encrypted = true` (no netcode crypto in process),
//! unsecure authentication, and `reliable = false`: renet2's own reliable channels run over the in-process link
//! exactly as over UDP. (With `reliable = true` renet2 downgrades every channel to "unreliable over an ordered
//! substrate", which emits a message's slices before the batch of small messages queued ahead of it in the same
//! update, so a large damage event could overtake the reveal it follows, and it never sends a message larger
//! than one update's byte allowance. Both break the channel contract.) Unconditioned, the in-memory allowance
//! per update is unbounded, since there is no link to protect; with `conditioning` the pair keeps the UDP
//! ceiling, so soak runs see the same pacing as the network.
//! UDP: `NativeSocket` on a bound `UdpSocket`, `ServerAuthentication::Secure` with the derived key. Connect
//! tokens list the join-code address first and [`TOKEN_SENTINEL_ADDR`] second; the server always lists the
//! sentinel among its public addresses, so a token stays valid whatever external address the host later
//! learns from port mapping (renetcode accepts a token whose address list intersects the server's). The
//! client tries addresses in order, so the sentinel is only ever "tried" after the real address timed out.
//! Regenerating the secret rebuilds the UDP backend on the same port: connected UDP players drop with
//! `DropReason::SecretRegenerated` and can rejoin with the new code into their held slot.
use super::conditioner::Conditioning;
use super::conditioner::{ConditionedClientSocket, ConditionedServerSocket};
use super::{ClientTransport, ConnState, DisconnectCause, LinkStats, ServerTransport, ServerTransportEvent, TransportKind};
use crate::channel::Channel;
use crate::error::NetError;
use crate::join::LobbySecret;
use std::net::SocketAddr;
use std::time::Duration;

/// Never-routed documentation address (RFC 5737 TEST-NET-1) used as the shared token address. Frozen: changing
/// it breaks joins between builds.
pub const TOKEN_SENTINEL_ADDR: SocketAddr = SocketAddr::V4(std::net::SocketAddrV4::new(std::net::Ipv4Addr::new(192, 0, 2, 1), 47610));
/// Connect token lifetime and netcode timeout, seconds.
pub const TOKEN_EXPIRE_S: u64 = 300;
pub const NETCODE_TIMEOUT_S: i32 = 15;

/// Options to host over UDP.
#[derive(Clone, Debug)]
pub struct UdpHostOptions {
    /// Usually `0.0.0.0:47610` (or `[::]:47610` dual-stack where the OS allows).
    pub bind: SocketAddr,
    pub secret: LobbySecret,
    pub max_clients: usize,
}

/// Server backend over renet2_netcode (memory or UDP).
pub struct NetcodeServerBackend {
    pub(crate) kind: TransportKind,
    pub(crate) renet: renet2::RenetServer,
    pub(crate) transport: renet2_netcode::NetcodeServerTransport,
    pub(crate) events: std::collections::VecDeque<ServerTransportEvent>,
    /// Addresses reported by `local_addrs` (empty for memory).
    pub(crate) listen: Vec<SocketAddr>,
    /// Clients this end disconnected whose `Disconnected` event has not been reported yet.
    pub(crate) local_drops: Vec<u64>,
    /// Time since each connected client's last packet, as of the previous update; tells a timeout from a
    /// remote disconnect when renet2 removes the connection.
    pub(crate) last_heard: Vec<(u64, Duration)>,
}

/// Client backend over renet2_netcode (memory or UDP).
pub struct NetcodeClientBackend {
    pub(crate) kind: TransportKind,
    pub(crate) renet: renet2::RenetClient,
    pub(crate) transport: renet2_netcode::NetcodeClientTransport,
    /// Set once the connection ended; keeps the first cause seen.
    pub(crate) ended: Option<DisconnectCause>,
    pub(crate) local_disconnect: bool,
}

/// Bytes renet2 may put on the wire per update. Fixed when the connection is built, so it is a safety ceiling
/// above net's own adaptive cap: four times the 768 kbps ceiling (lobby blueprint transfers are uncapped,
/// L943) at a 30 Hz update floor. At 60 Hz updates that allows about 6 Mbps.
pub const AVAILABLE_BYTES_PER_TICK: u64 = 768_000 / 8 * 4 / 30;

/// renet2 connection config from the channel table. Unit: `net-transport`.
pub fn connection_config() -> renet2::ConnectionConfig {
    fn channels(list: &[Channel]) -> Vec<renet2::ChannelConfig> {
        list.iter()
            .map(|c| {
                let spec = c.spec();
                let send_type = match spec.delivery {
                    crate::channel::Delivery::Unreliable => renet2::SendType::Unreliable { ordered_reliable_substrate: false },
                    crate::channel::Delivery::ReliableOrdered => {
                        renet2::SendType::ReliableOrdered { resend_time: Duration::from_millis(u64::from(spec.resend_ms)) }
                    }
                };
                renet2::ChannelConfig { channel_id: c.id(), max_memory_usage_bytes: spec.max_memory_bytes, send_type }
            })
            .collect()
    }
    let mut config = renet2::ConnectionConfig::from_channels(channels(&Channel::SERVER), channels(&Channel::CLIENT));
    config.available_bytes_per_tick = AVAILABLE_BYTES_PER_TICK;
    config
}

/// Config for an in-memory pair: no byte ceiling unless the link is conditioned (see the module header).
fn memory_config(conditioned: bool) -> renet2::ConnectionConfig {
    let mut config = connection_config();
    if !conditioned {
        config.available_bytes_per_tick = u64::MAX / 2;
    }
    config
}

/// renetcode's hard limit on clients per server.
const NETCODE_MAX_CLIENTS: usize = 1024;

/// Tokens are minted by the joining machine, so host and client clocks differ. `0` accepts any token that has
/// not expired by the host's clock, so up to `TOKEN_EXPIRE_S` of skew in either direction still joins. Old
/// tokens cannot reach a new session: each session has its own secret, hence its own key.
const MAX_TOKEN_LIFETIME_S: u16 = 0;

/// An in-process server with `clients` in-memory clients (ids 0..clients). Single-player and the host's own
/// client use one; in-process bot tests may use more. Unit: `net-transport`.
pub fn memory_pair(clients: u16, unix_now: Duration, conditioning: Option<Conditioning>) -> (NetcodeServerBackend, Vec<NetcodeClientBackend>) {
    let count = usize::from(clients).min(NETCODE_MAX_CLIENTS);
    let ids: Vec<u16> = (0..count as u16).collect();
    // encrypted = true: no netcode crypto in process. reliable = false: renet2's channels keep their contract.
    let reliable = false;
    let (server_socket, client_sockets) = renet2_netcode::new_memory_sockets(ids, true, reliable);
    let server_addr = renet2_netcode::in_memory_server_addr();
    let setup = renet2_netcode::ServerSetupConfig {
        current_time: unix_now,
        max_token_lifetime: MAX_TOKEN_LIFETIME_S,
        max_clients: count,
        protocol_id: crate::NETCODE_PROTOCOL_ID,
        socket_addresses: vec![vec![server_addr]],
        authentication: renet2_netcode::ServerAuthentication::Unsecure,
    };
    let transport = match &conditioning {
        Some(c) => renet2_netcode::NetcodeServerTransport::new(setup, ConditionedServerSocket::new(server_socket, c)),
        None => renet2_netcode::NetcodeServerTransport::new(setup, server_socket),
    };
    // `NetcodeServerTransport::new` only fails on socket setup errors, which in-memory sockets cannot have.
    let transport = match transport {
        Ok(t) => t,
        Err(e) => unreachable!("in-memory netcode transport: {e}"),
    };
    let config = memory_config(conditioning.is_some());
    let server = NetcodeServerBackend::new(TransportKind::Memory, transport, Vec::new(), config.clone());

    let mut out = Vec::with_capacity(client_sockets.len());
    for socket in client_sockets {
        let id = socket.id();
        let auth = renet2_netcode::ClientAuthentication::Unsecure {
            protocol_id: crate::NETCODE_PROTOCOL_ID,
            client_id: id,
            socket_id: 0,
            server_addr,
            user_data: None,
        };
        let renet = renet2::RenetClient::new(config.clone(), reliable);
        let transport = match &conditioning {
            Some(c) => {
                let c = Conditioning { profile: c.profile, seed: mix_seed(c.seed, id + 1), clock: c.clock.clone() };
                renet2_netcode::NetcodeClientTransport::new(unix_now, auth, ConditionedClientSocket::new(socket, &c))
            }
            None => renet2_netcode::NetcodeClientTransport::new(unix_now, auth, socket),
        };
        // Unsecure tokens with one address cannot fail to build; skip defensively rather than panic.
        if let Ok(transport) = transport {
            out.push(NetcodeClientBackend::new(TransportKind::Memory, renet, transport));
        }
    }
    (server, out)
}

/// Per-client conditioner seeds derived from the pair's seed (splitmix64 finaliser).
fn mix_seed(seed: u64, k: u64) -> u64 {
    let mut z = seed ^ k.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl NetcodeServerBackend {
    fn new(kind: TransportKind, transport: renet2_netcode::NetcodeServerTransport, listen: Vec<SocketAddr>, config: renet2::ConnectionConfig) -> Self {
        NetcodeServerBackend {
            kind,
            renet: renet2::RenetServer::new(config),
            transport,
            events: std::collections::VecDeque::new(),
            listen,
            local_drops: Vec::new(),
            last_heard: Vec::new(),
        }
    }

    /// Bind the UDP port and start serving. Public addresses: the bound address, the sentinel, and every local
    /// interface address on the same port. Unit: `net-transport`.
    pub fn bind_udp(options: &UdpHostOptions, unix_now: Duration, conditioning: Option<Conditioning>) -> Result<NetcodeServerBackend, NetError> {
        let socket = std::net::UdpSocket::bind(options.bind)?;
        let bound = socket.local_addr()?;
        let native = renet2_netcode::NativeSocket::new(socket).map_err(|e| NetError::Transport(format!("udp socket: {e}")))?;
        let socket = QuietUdp(native);

        let listen = listen_addrs(bound);
        let mut public = vec![bound, TOKEN_SENTINEL_ADDR];
        for a in &listen {
            if !public.contains(a) {
                public.push(*a);
            }
        }
        let setup = renet2_netcode::ServerSetupConfig {
            current_time: unix_now,
            max_token_lifetime: MAX_TOKEN_LIFETIME_S,
            max_clients: options.max_clients.min(NETCODE_MAX_CLIENTS),
            protocol_id: crate::NETCODE_PROTOCOL_ID,
            socket_addresses: vec![public],
            authentication: renet2_netcode::ServerAuthentication::Secure { private_key: options.secret.netcode_key() },
        };
        let transport = match &conditioning {
            Some(c) => renet2_netcode::NetcodeServerTransport::new(setup, ConditionedServerSocket::new(socket, c)),
            None => renet2_netcode::NetcodeServerTransport::new(setup, socket),
        }?;
        Ok(NetcodeServerBackend::new(TransportKind::Udp, transport, listen, connection_config()))
    }

    /// Move renet2's connection events into the backend's queue with their causes.
    fn pump_events(&mut self) {
        while let Some(e) = self.renet.get_event() {
            match e {
                renet2::ServerEvent::ClientConnected { client_id } => {
                    self.local_drops.retain(|&c| c != client_id);
                    self.events.push_back(ServerTransportEvent::Connected { client: client_id });
                }
                renet2::ServerEvent::ClientDisconnected { client_id, reason } => {
                    let local = self.local_drops.iter().position(|&c| c == client_id).map(|i| self.local_drops.swap_remove(i)).is_some();
                    let heard = self.last_heard.iter().find(|(c, _)| *c == client_id).map(|(_, t)| *t);
                    let cause = server_cause(reason, local, heard);
                    self.last_heard.retain(|(c, _)| *c != client_id);
                    self.events.push_back(ServerTransportEvent::Disconnected { client: client_id, cause });
                }
            }
        }
    }

    fn sorted_clients(&self) -> Vec<u64> {
        let mut ids = self.renet.clients_id();
        ids.sort_unstable();
        ids
    }
}

/// Why renet2 removed a server-side connection.
fn server_cause(reason: renet2::DisconnectReason, local: bool, since_heard: Option<Duration>) -> DisconnectCause {
    use renet2::DisconnectReason as R;
    match reason {
        R::DisconnectedByServer => DisconnectCause::Local,
        R::DisconnectedByClient => DisconnectCause::Remote,
        // renetcode removed it: either the client said goodbye or it went silent for the netcode timeout.
        R::Transport if local => DisconnectCause::Local,
        R::Transport => match since_heard {
            Some(t) if t + TIMEOUT_MARGIN >= Duration::from_secs(NETCODE_TIMEOUT_S as u64) => DisconnectCause::TimedOut,
            _ => DisconnectCause::Remote,
        },
        other => DisconnectCause::Error(other.to_string()),
    }
}

/// A client silent for the netcode timeout minus this margin counts as timed out (updates are discrete).
const TIMEOUT_MARGIN: Duration = Duration::from_millis(500);

/// Why the client-side connection ended. renet2's own reason wins when it names one (a channel overflow also
/// makes netcode send a goodbye, which would otherwise read as a local disconnect).
fn client_cause(renet: Option<renet2::DisconnectReason>, netcode: Option<renet2_netcode::NetcodeDisconnectReason>, local: bool) -> DisconnectCause {
    use renet2::DisconnectReason as R;
    use renet2_netcode::NetcodeDisconnectReason as N;
    if local {
        return DisconnectCause::Local;
    }
    match renet {
        Some(R::DisconnectedByServer) => DisconnectCause::Remote,
        Some(R::DisconnectedByClient) => DisconnectCause::Local,
        Some(R::Transport) | None => match netcode {
            Some(N::DisconnectedByServer) | None => DisconnectCause::Remote,
            Some(N::DisconnectedByClient) => DisconnectCause::Local,
            Some(N::ConnectionTimedOut | N::ConnectionRequestTimedOut | N::ConnectionResponseTimedOut) => DisconnectCause::TimedOut,
            Some(N::ConnectTokenExpired) => DisconnectCause::Error("connect token expired".into()),
            Some(N::ConnectionDenied) => DisconnectCause::Error("connection denied".into()),
        },
        Some(other) => DisconnectCause::Error(other.to_string()),
    }
}

/// Addresses a socket bound to `bound` listens on: the address itself, or every interface address of its
/// family on the same port when it is a wildcard.
fn listen_addrs(bound: SocketAddr) -> Vec<SocketAddr> {
    if !bound.ip().is_unspecified() {
        return vec![bound];
    }
    let mut out = Vec::new();
    for iface in netdev::get_interfaces() {
        if !iface.is_up() {
            continue;
        }
        let v4 = iface.ipv4.iter().map(|n| std::net::IpAddr::V4(n.addr()));
        let v6 = iface.ipv6.iter().map(|n| std::net::IpAddr::V6(n.addr())).filter(|ip| !is_v6_link_local(ip));
        for ip in v4.chain(v6) {
            // A v6 wildcard is often dual-stack; a v4 wildcard never serves v6.
            if bound.is_ipv4() && ip.is_ipv6() {
                continue;
            }
            let a = SocketAddr::new(ip, bound.port());
            if !out.contains(&a) {
                out.push(a);
            }
        }
    }
    out.sort();
    out
}

fn is_v6_link_local(ip: &std::net::IpAddr) -> bool {
    matches!(ip, std::net::IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80)
}

impl NetcodeClientBackend {
    fn new(kind: TransportKind, renet: renet2::RenetClient, transport: renet2_netcode::NetcodeClientTransport) -> Self {
        NetcodeClientBackend { kind, renet, transport, ended: None, local_disconnect: false }
    }

    /// Connect over UDP to `server` with a token minted from `secret` (`join::mint_connect_token`).
    /// Unit: `net-transport`.
    pub fn connect_udp(server: SocketAddr, secret: &LobbySecret, client_id: u64, unix_now: Duration, conditioning: Option<Conditioning>) -> Result<NetcodeClientBackend, NetError> {
        let token = crate::join::mint_connect_token(server, secret, client_id, unix_now)?;
        let any: SocketAddr = if server.is_ipv4() { (std::net::Ipv4Addr::UNSPECIFIED, 0).into() } else { (std::net::Ipv6Addr::UNSPECIFIED, 0).into() };
        let socket = std::net::UdpSocket::bind(any)?;
        let native = renet2_netcode::NativeSocket::new(socket).map_err(|e| NetError::Transport(format!("udp socket: {e}")))?;
        let socket = QuietUdp(native);
        let auth = renet2_netcode::ClientAuthentication::Secure { connect_token: token };
        let transport = match &conditioning {
            Some(c) => renet2_netcode::NetcodeClientTransport::new(unix_now, auth, ConditionedClientSocket::new(socket, c)),
            None => renet2_netcode::NetcodeClientTransport::new(unix_now, auth, socket),
        }
        .map_err(|e| NetError::Transport(format!("netcode client: {e}")))?;
        Ok(NetcodeClientBackend::new(TransportKind::Udp, renet2::RenetClient::new(connection_config(), false), transport))
    }

    fn is_ended(&self) -> bool { self.ended.is_some() || self.renet.is_disconnected() || self.transport.is_disconnected() }

    fn note_end(&mut self) {
        if self.ended.is_none() && (self.renet.is_disconnected() || self.transport.is_disconnected()) {
            self.ended = Some(client_cause(self.renet.disconnect_reason(), self.transport.disconnect_reason(), self.local_disconnect));
        }
    }
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

fn transport_error(e: renet2_netcode::NetcodeTransportError) -> NetError {
    match e {
        renet2_netcode::NetcodeTransportError::IO(e) => NetError::Io(e),
        other => NetError::Transport(other.to_string()),
    }
}

impl ServerTransport for NetcodeServerBackend {
    fn kind(&self) -> TransportKind { self.kind }
    /// renetcode keeps its own clock from the construction time plus every `dt`, so `unix_now` is not needed
    /// after construction.
    fn update(&mut self, dt: Duration, unix_now: Duration) -> Result<(), NetError> {
        self.last_heard = self
            .transport
            .clients_id_iter()
            .map(|c| (c, self.transport.time_since_last_received_packet(c).unwrap_or_default() + dt))
            .collect();
        self.renet.update(dt);
        let result = self.transport.update(dt, &mut self.renet);
        self.pump_events();
        match result {
            Ok(()) => Ok(()),
            // Receive errors are per datagram (renetcode keeps reading past them); report the first.
            Err(errors) => match errors.into_iter().next() {
                Some(e) => Err(transport_error(e)),
                None => Ok(()),
            },
        }
    }
    fn poll_event(&mut self) -> Option<ServerTransportEvent> { self.events.pop_front() }
    fn clients(&self) -> Vec<u64> { self.sorted_clients() }
    fn receive(&mut self, client: u64, channel: Channel) -> Option<Vec<u8>> {
        if !Channel::CLIENT.contains(&channel) || !self.renet.is_connected(client) {
            return None;
        }
        self.renet.receive_message(client, channel.id()).map(|b| b.to_vec())
    }
    fn can_send(&self, client: u64, channel: Channel, bytes: usize) -> bool {
        Channel::SERVER.contains(&channel) && self.renet.is_connected(client) && self.renet.can_send_message(client, channel.id(), bytes)
    }
    fn send(&mut self, client: u64, channel: Channel, bytes: Vec<u8>) {
        if Channel::SERVER.contains(&channel) && self.renet.is_connected(client) {
            self.renet.send_message(client, channel.id(), bytes);
        }
    }
    fn flush(&mut self) { self.transport.send_packets(&mut self.renet); }
    fn stats(&self, client: u64) -> Option<LinkStats> { self.renet.network_info(client).ok().map(link_stats) }
    fn client_addr(&self, client: u64) -> Option<SocketAddr> {
        match self.kind {
            TransportKind::Memory => None,
            _ => self.transport.client_addr(client).map(|(_, a)| a),
        }
    }
    fn disconnect(&mut self, client: u64) {
        if self.renet.is_connected(client) || self.transport.has_client(client) {
            if !self.local_drops.contains(&client) {
                self.local_drops.push(client);
            }
            // Takes effect on the next update, which sends netcode's disconnect packets.
            self.renet.disconnect(client);
        }
    }
    fn disconnect_all(&mut self) {
        for c in self.sorted_clients() {
            if !self.local_drops.contains(&c) {
                self.local_drops.push(c);
            }
        }
        self.renet.disconnect_all();
        // Sends the disconnect packets now, so a session that ends right after still says goodbye.
        self.transport.disconnect_all(&mut self.renet);
        self.pump_events();
    }
    fn set_max_clients(&mut self, n: usize) { self.transport.set_max_clients(n.min(NETCODE_MAX_CLIENTS)); }
    fn local_addrs(&self) -> Vec<SocketAddr> { self.listen.clone() }
}

impl ClientTransport for NetcodeClientBackend {
    fn kind(&self) -> TransportKind { self.kind }
    /// Ok while the link works or after it ended (`state` then says why); Err only for socket failures.
    fn update(&mut self, dt: Duration, unix_now: Duration) -> Result<(), NetError> {
        if self.ended.is_some() {
            return Ok(());
        }
        self.renet.update(dt);
        let result = self.transport.update(dt, &mut self.renet);
        self.note_end();
        match result {
            Ok(()) => Ok(()),
            Err(_) if self.ended.is_some() => Ok(()),
            Err(e) => Err(transport_error(e)),
        }
    }
    fn state(&self) -> ConnState {
        if let Some(cause) = &self.ended {
            return ConnState::Disconnected(cause.clone());
        }
        if self.renet.is_disconnected() || self.transport.is_disconnected() {
            return ConnState::Disconnected(client_cause(self.renet.disconnect_reason(), self.transport.disconnect_reason(), self.local_disconnect));
        }
        if self.transport.is_connected() && self.renet.is_connected() { ConnState::Connected } else { ConnState::Connecting }
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
            Err(renet2_netcode::NetcodeTransportError::IO(e)) => Err(NetError::Io(e)),
            Err(_) => {
                self.note_end();
                Err(NetError::NotConnected)
            }
        }
    }
    fn stats(&self) -> LinkStats { link_stats(self.renet.network_info()) }
    fn time_since_last_packet(&self) -> Duration { self.transport.time_since_last_received_packet() }
    fn disconnect(&mut self) {
        if self.is_ended() {
            return;
        }
        self.local_disconnect = true;
        self.renet.disconnect();
        // Sends netcode's disconnect packet now.
        self.transport.disconnect();
        self.ended = Some(DisconnectCause::Local);
    }
}

/// The native UDP socket with datagram semantics on both ends: a send that the OS refuses for a transient
/// reason (no route yet, buffer full, ICMP unreachable reported back) is a lost datagram, not a fatal error,
/// and an ICMP "port unreachable" surfacing on receive (Windows reports it as WSAECONNRESET on the next
/// `recv_from`) is skipped instead of aborting the update. Without this a client joining a host whose port is
/// closed would stall instead of timing out.
#[derive(Debug)]
struct QuietUdp(renet2_netcode::NativeSocket);

fn transient_send(kind: std::io::ErrorKind) -> bool {
    use std::io::ErrorKind as K;
    matches!(
        kind,
        K::WouldBlock | K::Interrupted | K::ConnectionRefused | K::ConnectionReset | K::HostUnreachable | K::NetworkUnreachable | K::NetworkDown | K::AddrNotAvailable | K::PermissionDenied
    )
}

/// Skip ICMP error reports surfacing as receive errors; give up for this update after a burst of them.
fn quiet_recv(mut recv: impl FnMut(&mut [u8]) -> std::io::Result<(usize, SocketAddr)>, buffer: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
    for _ in 0..64 {
        match recv(buffer) {
            Err(e) if matches!(e.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionRefused) => continue,
            other => return other,
        }
    }
    Err(std::io::ErrorKind::WouldBlock.into())
}

fn quiet_send(r: Result<(), renet2_netcode::NetcodeTransportError>) -> Result<(), renet2_netcode::NetcodeTransportError> {
    match r {
        Err(renet2_netcode::NetcodeTransportError::IO(e)) if transient_send(e.kind()) => Ok(()),
        other => other,
    }
}

impl renet2_netcode::ServerSocket for QuietUdp {
    fn is_encrypted(&self) -> bool { renet2_netcode::ServerSocket::is_encrypted(&self.0) }
    fn is_reliable(&self) -> bool { renet2_netcode::ServerSocket::is_reliable(&self.0) }
    fn addr(&self) -> std::io::Result<SocketAddr> { renet2_netcode::ServerSocket::addr(&self.0) }
    fn is_closed(&mut self) -> bool { renet2_netcode::ServerSocket::is_closed(&mut self.0) }
    fn close(&mut self) { renet2_netcode::ServerSocket::close(&mut self.0) }
    fn connection_denied(&mut self, addr: SocketAddr) { self.0.connection_denied(addr) }
    fn connection_accepted(&mut self, client_id: u64, addr: SocketAddr) { self.0.connection_accepted(client_id, addr) }
    fn disconnect(&mut self, addr: SocketAddr) { self.0.disconnect(addr) }
    fn preupdate(&mut self) { renet2_netcode::ServerSocket::preupdate(&mut self.0) }
    fn try_recv(&mut self, buffer: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
        let inner = &mut self.0;
        quiet_recv(|b| renet2_netcode::ServerSocket::try_recv(inner, b), buffer)
    }
    fn postupdate(&mut self) { renet2_netcode::ServerSocket::postupdate(&mut self.0) }
    fn send(&mut self, addr: SocketAddr, packet: &[u8]) -> Result<(), renet2_netcode::NetcodeTransportError> {
        quiet_send(renet2_netcode::ServerSocket::send(&mut self.0, addr, packet))
    }
}

impl renet2_netcode::ClientSocket for QuietUdp {
    fn is_encrypted(&self) -> bool { renet2_netcode::ClientSocket::is_encrypted(&self.0) }
    fn is_reliable(&self) -> bool { renet2_netcode::ClientSocket::is_reliable(&self.0) }
    fn addr(&self) -> std::io::Result<SocketAddr> { renet2_netcode::ClientSocket::addr(&self.0) }
    fn is_closed(&mut self) -> bool { renet2_netcode::ClientSocket::is_closed(&mut self.0) }
    fn close(&mut self) { renet2_netcode::ClientSocket::close(&mut self.0) }
    fn preupdate(&mut self) { renet2_netcode::ClientSocket::preupdate(&mut self.0) }
    fn try_recv(&mut self, buffer: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
        let inner = &mut self.0;
        quiet_recv(|b| renet2_netcode::ClientSocket::try_recv(inner, b), buffer)
    }
    fn postupdate(&mut self) { renet2_netcode::ClientSocket::postupdate(&mut self.0) }
    fn send(&mut self, addr: SocketAddr, packet: &[u8]) -> Result<(), renet2_netcode::NetcodeTransportError> {
        quiet_send(renet2_netcode::ClientSocket::send(&mut self.0, addr, packet))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Clock, ManualClock};
    use crate::transport::conditioner::ConditionerProfile;
    use std::sync::Arc;

    const STEP: Duration = Duration::from_millis(16);

    fn step(server: &mut NetcodeServerBackend, clients: &mut [&mut NetcodeClientBackend], clock: &ManualClock, real: bool) {
        clock.advance(STEP);
        let now = clock.unix_now();
        for c in clients.iter_mut() {
            c.update(STEP, now).unwrap();
            let _ = c.flush();
        }
        if real {
            std::thread::sleep(Duration::from_millis(1));
        }
        server.update(STEP, now).unwrap();
        server.flush();
        if real {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn connect(server: &mut NetcodeServerBackend, clients: &mut [&mut NetcodeClientBackend], clock: &ManualClock, real: bool) {
        for _ in 0..200 {
            step(server, clients, clock, real);
            if clients.iter().all(|c| c.state() == ConnState::Connected) && server.clients().len() == clients.len() {
                return;
            }
        }
        panic!("did not connect: {:?}", clients.iter().map(|c| c.state()).collect::<Vec<_>>());
    }

    #[test]
    fn config_follows_the_channel_table() {
        let c = connection_config();
        let ids: Vec<u8> = c.server_channels_config.iter().map(|c| c.channel_id).collect();
        assert_eq!(ids, Channel::SERVER.iter().map(|c| c.id()).collect::<Vec<_>>());
        let ids: Vec<u8> = c.client_channels_config.iter().map(|c| c.channel_id).collect();
        assert_eq!(ids, Channel::CLIENT.iter().map(|c| c.id()).collect::<Vec<_>>());
        assert!(matches!(c.server_channels_config[4].send_type, renet2::SendType::Unreliable { .. }));
        assert!(matches!(c.server_channels_config[0].send_type, renet2::SendType::ReliableOrdered { .. }));
        // At least the 768 kbps ceiling at 30 updates per second.
        assert!(c.available_bytes_per_tick * 30 * 8 >= 768_000);
    }

    #[test]
    fn memory_pair_connects_and_carries_every_channel() {
        let clock = ManualClock::new();
        let (mut server, mut clients) = memory_pair(2, clock.unix_now(), None);
        assert_eq!(server.kind(), TransportKind::Memory);
        assert!(server.local_addrs().is_empty());
        let (a, b) = clients.split_at_mut(1);
        let (a, b) = (&mut a[0], &mut b[0]);
        connect(&mut server, &mut [a, b], &clock, false);
        assert_eq!(server.clients(), vec![0, 1]);
        let mut connected = Vec::new();
        while let Some(e) = server.poll_event() {
            connected.push(e);
        }
        assert_eq!(connected.len(), 2);

        for ch in Channel::SERVER {
            assert!(server.can_send(1, ch, 100));
            server.send(1, ch, vec![ch.id(); 100]);
        }
        // A large reliable message is sliced and reassembled.
        server.send(1, Channel::Bulk, vec![9; 40_000]);
        for ch in Channel::CLIENT {
            a.send(ch, vec![0x40 | ch.id(); 10]);
        }
        // Wrong direction channels are refused without panicking.
        assert!(!a.can_send(Channel::Events, 1));
        a.send(Channel::Events, vec![1]);
        assert_eq!(server.receive(0, Channel::Events), None);
        for _ in 0..10 {
            step(&mut server, &mut [a, b], &clock, false);
        }
        for ch in Channel::SERVER {
            assert_eq!(b.receive(ch), Some(vec![ch.id(); 100]), "{ch:?}");
        }
        assert_eq!(b.receive(Channel::Bulk).map(|v| v.len()), Some(40_000));
        for ch in Channel::CLIENT {
            assert_eq!(server.receive(0, ch), Some(vec![0x40 | ch.id(); 10]));
        }
        assert!(server.stats(0).is_some());
        assert_eq!(server.stats(77), None);
    }

    #[test]
    fn reliable_channels_survive_a_lossy_conditioned_memory_link() {
        let clock = ManualClock::new();
        let c = Conditioning { profile: ConditionerProfile::round_trip(100.0, 0.10, 10.0), seed: 7, clock: Arc::new(clock.clone()) };
        let (mut server, mut clients) = memory_pair(1, clock.unix_now(), Some(c));
        let client = &mut clients[0];
        connect(&mut server, &mut [client], &clock, false);
        for i in 0..200u32 {
            server.send(0, Channel::Events, i.to_le_bytes().to_vec());
        }
        let mut got = Vec::new();
        for _ in 0..400 {
            step(&mut server, &mut [client], &clock, false);
            while let Some(m) = client.receive(Channel::Events) {
                got.push(u32::from_le_bytes(m[..4].try_into().unwrap()));
            }
            if got.len() == 200 {
                break;
            }
        }
        assert_eq!(got, (0..200).collect::<Vec<_>>());
        for _ in 0..60 {
            step(&mut server, &mut [client], &clock, false);
        }
        let rtt = server.stats(0).unwrap().rtt_ms;
        assert!(rtt > 80.0 && rtt < 200.0, "rtt {rtt}");
    }

    #[test]
    fn disconnects_report_their_causes() {
        let clock = ManualClock::new();
        let (mut server, mut clients) = memory_pair(2, clock.unix_now(), None);
        let (a, b) = clients.split_at_mut(1);
        let (a, b) = (&mut a[0], &mut b[0]);
        connect(&mut server, &mut [a, b], &clock, false);
        while server.poll_event().is_some() {}

        server.disconnect(0);
        a.disconnect();
        b.disconnect();
        assert_eq!(b.state(), ConnState::Disconnected(DisconnectCause::Local));
        for _ in 0..5 {
            step(&mut server, &mut [a], &clock, false);
        }
        let mut events = Vec::new();
        while let Some(e) = server.poll_event() {
            events.push(e);
        }
        assert!(events.contains(&ServerTransportEvent::Disconnected { client: 0, cause: DisconnectCause::Local }), "{events:?}");
        assert!(events.contains(&ServerTransportEvent::Disconnected { client: 1, cause: DisconnectCause::Remote }), "{events:?}");
        assert!(server.clients().is_empty());
        assert!(a.flush().is_err());
    }

    #[test]
    fn a_silent_client_times_out_and_the_client_hears_a_kick() {
        let clock = ManualClock::new();
        let (mut server, mut clients) = memory_pair(2, clock.unix_now(), None);
        let (a, b) = clients.split_at_mut(1);
        let (a, b) = (&mut a[0], &mut b[0]);
        connect(&mut server, &mut [a, b], &clock, false);
        while server.poll_event().is_some() {}
        server.disconnect(1);
        // Client 0 goes silent; client 1 keeps updating and hears the kick.
        let mut timed_out = false;
        for _ in 0..((NETCODE_TIMEOUT_S as u64 + 2) * 1000 / 16) {
            step(&mut server, &mut [b], &clock, false);
            while let Some(e) = server.poll_event() {
                if e == (ServerTransportEvent::Disconnected { client: 0, cause: DisconnectCause::TimedOut }) {
                    timed_out = true;
                }
            }
        }
        assert!(timed_out);
        assert_eq!(b.state(), ConnState::Disconnected(DisconnectCause::Remote));
    }

    #[test]
    fn udp_loopback_joins_with_the_secret_and_not_without() {
        let clock = ManualClock::new();
        let secret = LobbySecret::generate();
        let options = UdpHostOptions { bind: "127.0.0.1:0".parse().unwrap(), secret, max_clients: 4 };
        let mut server = NetcodeServerBackend::bind_udp(&options, clock.unix_now(), None).unwrap();
        let addr = server.local_addrs()[0];
        assert_eq!(addr.ip(), std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        assert_ne!(addr.port(), 0);

        let mut good = NetcodeClientBackend::connect_udp(addr, &secret, 0xDEAD_BEEF, clock.unix_now(), None).unwrap();
        let mut bad = NetcodeClientBackend::connect_udp(addr, &LobbySecret::generate(), 0xBAD, clock.unix_now(), None).unwrap();
        for _ in 0..100 {
            step(&mut server, &mut [&mut good, &mut bad], &clock, true);
            if good.state() == ConnState::Connected {
                break;
            }
        }
        assert_eq!(good.state(), ConnState::Connected);
        assert_ne!(bad.state(), ConnState::Connected);
        assert_eq!(server.clients(), vec![0xDEAD_BEEF]);
        assert_eq!(server.client_addr(0xDEAD_BEEF).map(|a| a.ip()), Some(addr.ip()));

        good.send(Channel::Session, b"hello".to_vec());
        let mut got = None;
        for _ in 0..50 {
            step(&mut server, &mut [&mut good], &clock, true);
            if let Some(m) = server.receive(0xDEAD_BEEF, Channel::Session) {
                got = Some(m);
                break;
            }
        }
        assert_eq!(got.as_deref(), Some(&b"hello"[..]));

        server.disconnect_all();
        for _ in 0..10 {
            step(&mut server, &mut [&mut good], &clock, true);
        }
        assert_eq!(good.state(), ConnState::Disconnected(DisconnectCause::Remote));
    }

    #[test]
    fn a_client_minted_token_survives_host_clock_skew() {
        // The joining machine's clock runs two minutes behind the host's.
        let clock = ManualClock::new();
        let secret = LobbySecret::generate();
        let options = UdpHostOptions { bind: "127.0.0.1:0".parse().unwrap(), secret, max_clients: 2 };
        let mut server = NetcodeServerBackend::bind_udp(&options, clock.unix_now(), None).unwrap();
        let addr = server.local_addrs()[0];
        let skewed = clock.unix_now() - Duration::from_secs(120);
        let mut client = NetcodeClientBackend::connect_udp(addr, &secret, 5, skewed, None).unwrap();
        connect(&mut server, &mut [&mut client], &clock, true);
    }
}
