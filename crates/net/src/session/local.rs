//! Unit `net-server-session`. The in-process pairing for single-player and the listen host's own client
//! (L899, L986): a `ServerSession` with one in-memory transport and a `ClientSession` connected to it.
//!
//! The pair is the same program as a networked session: renet2 channels over renet2_netcode's memory
//! sockets, the hello, the lobby admission and every message on the wire. Before returning, the netcode
//! handshake of the two backends is pumped to completion, so the client's first `update` sends its hello and
//! the server's next `update` admits it as slot 0 with `host = true`.
use super::client::ClientSession;
use super::server::ServerSession;
use super::{ClientOptions, ServerOptions};
use crate::blueprints::BlueprintStore;
use crate::clock::Clock;
use crate::transport::netcode::memory_pair;
use crate::transport::{ClientTransport, ConnState, ServerTransport};
use content::catalogue::Catalogue;
use sim_systems::modes::LobbySetup;
use std::sync::Arc;
use std::time::Duration;

/// Handshake round trips pumped at most (netcode needs request, challenge, response and keep-alive).
const HANDSHAKE_PUMPS: usize = 16;

/// Build the pair; the client is admitted as slot 0 with `host = true` on the first update of each.
/// Unit: `net-server-session`.
#[allow(clippy::too_many_arguments)]
pub fn local_pair(server: ServerOptions, setup: LobbySetup, client: ClientOptions, clock: Arc<dyn Clock>, pool: Arc<rayon::ThreadPool>, catalogue: Arc<Catalogue>, host_store: Box<dyn BlueprintStore>, client_store: Box<dyn BlueprintStore>) -> (ServerSession, ClientSession) {
    let (mut host_end, mut ends) = memory_pair(1, clock.unix_now(), None);
    let mut client_end = ends.pop();
    if let Some(c) = client_end.as_mut() {
        // In-memory sockets deliver within the same call, so a few zero-length updates complete the handshake.
        for _ in 0..HANDSHAKE_PUMPS {
            let _ = c.update(Duration::ZERO, clock.unix_now());
            let _ = c.flush();
            let _ = host_end.update(Duration::ZERO, clock.unix_now());
            host_end.flush();
            if c.state() == ConnState::Connected { break; }
        }
    }
    let mut session = ServerSession::new(server, setup, clock.clone(), pool, catalogue.clone(), host_store);
    session.add_transport(Box::new(host_end));
    let transport: Box<dyn ClientTransport> = match client_end {
        Some(c) => Box::new(c),
        // `memory_pair(1, ..)` always builds its client; an empty list would mean renet2 refused an in-memory
        // token, which cannot happen. A transport that never connects keeps the session well-defined.
        None => Box::new(NeverConnected),
    };
    let client = ClientSession::new(client, transport, clock, catalogue, client_store);
    (session, client)
}

/// A client transport that is disconnected from the start.
struct NeverConnected;
impl ClientTransport for NeverConnected {
    fn kind(&self) -> crate::transport::TransportKind { crate::transport::TransportKind::Memory }
    fn update(&mut self, _dt: Duration, _unix_now: Duration) -> Result<(), crate::error::NetError> { Ok(()) }
    fn state(&self) -> ConnState { ConnState::Disconnected(crate::transport::DisconnectCause::Error("no in-memory client".into())) }
    fn receive(&mut self, _channel: crate::channel::Channel) -> Option<Vec<u8>> { None }
    fn can_send(&self, _channel: crate::channel::Channel, _bytes: usize) -> bool { false }
    fn send(&mut self, _channel: crate::channel::Channel, _bytes: Vec<u8>) {}
    fn flush(&mut self) -> Result<(), crate::error::NetError> { Ok(()) }
    fn stats(&self) -> crate::transport::LinkStats { Default::default() }
    fn time_since_last_packet(&self) -> Duration { Duration::ZERO }
    fn disconnect(&mut self) {}
}
