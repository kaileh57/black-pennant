use super::*;
use crate::seams::{ClientEvent, ClientMsg, HostEvent, LinkInbox, LinkState, LobbyRequest, LobbyView};
use crate::session::testkit;
use content::enums::{GameMode, TimeOfDay};
use content::ruleset::{Ruleset, RulesetKind};
use content::weather::{SeaState, WeatherKind};
use net::clock::ManualClock;
use net::portmap::{MapFailure, MapProtocol, VpnAddr, VpnKind};
use std::collections::BTreeSet;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("bp-gd-link-net-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn options(clock: &ManualClock, client_id: u64, name: &str) -> LinkOptions {
    LinkOptions {
        identity: ClientIdentity { client_id, name: name.into() },
        game_version: "0.0.0-test".into(),
        build: net::msg::BuildKind::Dev,
        interp: InterpTuning::default(),
        clock: Arc::new(clock.clone()),
        catalogue: testkit::catalogue(),
    }
}

fn sandbox_setup() -> LobbySetup {
    LobbySetup {
        mode: GameMode::Sandbox,
        ruleset: Ruleset { kind: RulesetKind::Open, tonnage_cap_t: 1_800.0, complexity_per_vessel: 0.0, complexity_per_lobby: 0.0, part_limits: Default::default(), max_length_m: 90.0, luau: false },
        seed: 7,
        players: Vec::new(),
        skirmish: None,
        weather: WeatherKind::Clear,
        sea_state: SeaState(2),
        time_of_day: TimeOfDay::Day,
        map: None,
        at_1_0: false,
    }
}

fn packed_starter() -> content::share::PackedBlueprint {
    content::share::pack(&content::blueprint::Shareable::Ship(Box::new(testkit::starter())))
}

/// One pass of everything, 16 ms of manual time, and a moment for loopback UDP to deliver.
fn pump(clock: &ManualClock, host: &mut NetHost, host_events: &mut Vec<HostEvent>, links: &mut [(&mut NetLink, &mut LinkInbox)]) {
    clock.advance(Duration::from_millis(16));
    host.update(host_events).expect("the host updates");
    for (link, inbox) in links.iter_mut() { link.poll(0.0, inbox); }
    std::thread::sleep(Duration::from_millis(1));
}

fn welcome_slot(inbox: &LinkInbox) -> Option<PlayerSlot> {
    inbox.session.iter().find_map(|e| if let ClientEvent::Welcomed(w) = e { Some(w.slot) } else { None })
}
fn last_view(inbox: &LinkInbox) -> Option<LobbyView> {
    inbox.session.iter().rev().find_map(|e| if let ClientEvent::Lobby(v) = e { Some((**v).clone()) } else { None })
}
fn ended(inbox: &LinkInbox) -> Option<SessionEnd> {
    inbox.session.iter().find_map(|e| if let ClientEvent::Ended(end) = e { Some(*end) } else { None })
}

#[test]
fn every_text_key_has_an_english_row() {
    let csv = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/i18n/net.csv")).expect("net.csv");
    let rows: BTreeSet<&str> = csv.lines().skip(1).filter_map(|l| l.split(',').next()).collect();
    let keys = text_keys();
    let unique: BTreeSet<&str> = keys.iter().copied().collect();
    assert_eq!(unique.len(), keys.len(), "keys are listed once");
    for k in &keys { assert!(rows.contains(k), "net.csv lacks {k}"); }
    // Every function returns a listed key.
    let mut produced = vec![
        refusal_msg(&JoinRefusal::ProtocolMismatch { host: 2, client: 1 }), refusal_msg(&JoinRefusal::LobbyFull { max_players: 4 }),
        refusal_msg(&JoinRefusal::HostSaturated), refusal_msg(&JoinRefusal::Banned), refusal_msg(&JoinRefusal::OperationInProgress),
        refusal_msg(&JoinRefusal::MatchInProgress), refusal_msg(&JoinRefusal::DuplicateClient), refusal_msg(&JoinRefusal::BadHello),
        refusal_msg(&JoinRefusal::ShuttingDown),
        end_msg(SessionEnd::HostQuit), end_msg(SessionEnd::HostLost), end_msg(SessionEnd::Refused), end_msg(SessionEnd::MatchOver),
        join_code_msg(JoinCodeError::BadCharacter), join_code_msg(JoinCodeError::WrongLength), join_code_msg(JoinCodeError::Checksum),
        join_code_msg(JoinCodeError::UnknownFormat(3)), join_code_msg(JoinCodeError::ProtocolMismatch { code: 1, ours: 2 }),
        join_code_msg(JoinCodeError::BadAddress),
        portmap_msg(None), portmap_msg(Some(&MappingOutcome::Failed { upnp: MapFailure::Timeout, nat_pmp_pcp: MapFailure::NoGateway })),
        portmap_msg(Some(&MappingOutcome::RenewFailed { protocol: MapProtocol::Pcp, failure: MapFailure::Timeout })),
        portmap_msg(Some(&MappingOutcome::Mapped { protocol: MapProtocol::UpnpIgd, external: "203.0.113.9:47610".parse().unwrap(), lease_s: 3600, permanent: false })),
        portmap_msg(Some(&MappingOutcome::DoubleNat { protocol: MapProtocol::NatPmp, external: "100.64.1.2:47610".parse().unwrap() })),
        warning_msg(net::msg::DropWarning::HighPing), warning_msg(net::msg::DropWarning::PacketLoss), warning_msg(net::msg::DropWarning::FallingBehind),
        error_msg(&NetError::Io(std::io::Error::other("x"))), error_msg(&NetError::Transport("x".into())), error_msg(&NetError::Decode("x".into())),
        error_msg(&NetError::NotConnected), error_msg(&NetError::WrongPhase), error_msg(&NetError::Encode("x".into())),
    ];
    for r in [DropReason::Kicked, DropReason::Banned, DropReason::HighPing, DropReason::PacketLoss, DropReason::FallingBehind, DropReason::TimedOut, DropReason::Left, DropReason::SecretRegenerated] {
        produced.push(drop_msg(r));
        assert_eq!(end_msg(SessionEnd::Dropped(r)), drop_msg(r));
    }
    let made: BTreeSet<String> = produced.iter().map(|m| m.key.0.clone()).collect();
    assert_eq!(made, unique.iter().map(|s| s.to_string()).collect(), "the list is exactly what the functions return");
    // Argument counts match the placeholders of the English rows.
    for msg in &produced {
        let line = csv.lines().find(|l| l.starts_with(&format!("{},", msg.key.0))).expect("row");
        for i in 0..msg.args.len() { assert!(line.contains(&format!("{{{i}}}")), "{} uses argument {i}", msg.key.0); }
        assert!(!line.contains(&format!("{{{}}}", msg.args.len())), "{} has no argument {}", msg.key.0, msg.args.len());
    }
    assert_eq!(error_msg(&NetError::JoinCode(JoinCodeError::Checksum)), join_code_msg(JoinCodeError::Checksum));
    assert_eq!(error_msg(&NetError::Refused(JoinRefusal::Banned)), refusal_msg(&JoinRefusal::Banned));
}

#[test]
fn join_codes_round_trip_and_bad_ones_say_why() {
    let secret = LobbySecret([7; 10]);
    let addr: SocketAddr = "203.0.113.9:47610".parse().unwrap();
    let code = JoinCode::new(addr, secret).encode();
    let parsed = parse_join_code(&format!("  {}  ", code.to_lowercase())).expect("a code decodes in any case, with spaces round it");
    assert_eq!((parsed.addr, parsed.secret), (addr, secret));
    assert_eq!(parse_join_code(""), Err(JoinCodeError::WrongLength));
    assert_eq!(parse_join_code("!!!!"), Err(JoinCodeError::BadCharacter));
    // One character changed breaks the checksum (or the length, never a wrong address).
    let mut broken: Vec<char> = code.chars().collect();
    broken[5] = if broken[5] == 'A' { 'B' } else { 'A' };
    assert!(parse_join_code(&broken.into_iter().collect::<String>()).is_err());
    let other = JoinCode { protocol: net::PROTOCOL_VERSION + 1, addr, secret }.encode();
    assert_eq!(parse_join_code(&other), Err(JoinCodeError::ProtocolMismatch { code: net::PROTOCOL_VERSION + 1, ours: net::PROTOCOL_VERSION }));
}

#[test]
fn addresses_take_a_port_or_the_default_and_a_key() {
    let secret = LobbySecret([9; 10]);
    let key = secret.lobby_key();
    let spaced = format!("{} {}", &key[..8], &key[8..]);
    match parse_address(" 192.168.1.20:5000 ", &spaced).expect("address and key") {
        JoinTarget::Address { addr, secret: s } => { assert_eq!(addr, "192.168.1.20:5000".parse::<SocketAddr>().unwrap()); assert_eq!(s, secret); }
        other => panic!("{other:?}"),
    }
    assert_eq!(parse_address("192.168.1.20", &key).unwrap().addr().port(), net::DEFAULT_PORT);
    assert_eq!(parse_address("[fd00::2]", &key).unwrap().addr(), SocketAddr::new("fd00::2".parse().unwrap(), net::DEFAULT_PORT));
    assert_eq!(parse_address("host.example:47610", &key), Err(JoinCodeError::BadAddress));
    assert_eq!(parse_address("", &key), Err(JoinCodeError::BadAddress));
    assert_eq!(parse_address("0.0.0.0:47610", &key), Err(JoinCodeError::BadAddress));
    assert_eq!(parse_address("10.0.0.1:0", &key), Err(JoinCodeError::BadAddress));
    assert_eq!(parse_address("10.0.0.1", ""), Err(JoinCodeError::WrongLength));
    assert!(parse_address("10.0.0.1", "ABC").is_err());
}

#[test]
fn the_panel_codes_follow_mapping_and_the_secret() {
    let lan: IpAddr = "192.168.1.20".parse().unwrap();
    let local = LocalAddrs { lan: vec!["fd00::5".parse().unwrap(), lan], vpn: vec![VpnAddr { kind: VpnKind::Tailscale, ip: "100.101.102.103".parse().unwrap() }] };
    let mut h = HostingHandle::new(47610, 1000, LobbySecret([1; 10]), local);
    let info = h.info().clone();
    assert_eq!(info.portmap, PortMapState::Trying);
    assert_eq!(info.lan_address, "192.168.1.20:47610", "IPv4 first");
    assert_eq!(parse_join_code(&info.join_code).unwrap().addr, SocketAddr::new(lan, 47610), "the LAN address until mapping works");
    assert_eq!(info.vpn_address, "100.101.102.103:47610");
    assert_eq!(parse_join_code(&info.vpn_join_code).unwrap().addr.ip(), "100.101.102.103".parse::<IpAddr>().unwrap());
    assert_eq!(info.lobby_key, LobbySecret([1; 10]).lobby_key());
    assert!(info.external_address.is_empty());
    let external: SocketAddr = "203.0.113.9:51000".parse().unwrap();
    h.on_outcome(MappingOutcome::Mapped { protocol: MapProtocol::UpnpIgd, external, lease_s: 3600, permanent: false });
    h.refresh();
    assert_eq!(h.info().portmap, PortMapState::Mapped);
    assert_eq!(h.info().external_address, "203.0.113.9:51000");
    assert_eq!(parse_join_code(&h.info().join_code).unwrap().addr, external, "the code names the mapped address");
    h.on_outcome(MappingOutcome::DoubleNat { protocol: MapProtocol::NatPmp, external: "100.64.0.9:47610".parse().unwrap() });
    h.refresh();
    assert_eq!(h.info().portmap, PortMapState::DoubleNat);
    assert!(h.info().external_address.is_empty(), "a private external address is not shown as reachable");
    let old = h.info().join_code.clone();
    let options = h.regenerate_secret(3);
    assert_ne!(h.info().join_code, old);
    assert_eq!(options.secret, h.secret);
    assert_eq!(options.max_clients, MAX_UDP_CLIENTS);
    assert_eq!(options.bind, "0.0.0.0:47610".parse::<SocketAddr>().unwrap());
    assert!(!h.info().bound);
    h.on_bound(&Ok(vec!["0.0.0.0:51515".parse().unwrap()]));
    assert!(h.info().bound);
    assert_eq!(h.info().port, 51515, "a port the OS picked goes into the codes");
    assert_eq!(parse_join_code(&h.info().join_code).unwrap().addr.port(), 51515);
    h.on_bound(&Err("in use".into()));
    assert!(!h.info().bound);
    // No address at all: the loopback address, so a second copy on this machine can join.
    let lonely = HostingHandle::new(47610, 1000, LobbySecret([2; 10]), LocalAddrs::default());
    assert_eq!(parse_join_code(&lonely.info().join_code).unwrap().addr, "127.0.0.1:47610".parse::<SocketAddr>().unwrap());
    assert!(lonely.info().vpn_join_code.is_empty());
}

/// The whole multiplayer path: a listen host (loopback) plus a UDP member, designs uploaded and accepted,
/// both ready, the start request, a running sortie with sampled frames on both ends, then a kick with ban
/// and a refused rejoin.
#[test]
fn a_hosted_lobby_runs_to_a_sortie_over_udp_then_kicks_and_bans() {
    let _serial = testkit::serial();
    let clock = ManualClock::new();
    let host_opts = options(&clock, 0x1111, "Host");
    let (mut host, mut own) = loopback_named(&host_opts, "Evening sail", sandbox_setup(), 2_000, false, testkit::pool(), temp_dir("host"), &[]);
    let secret = LobbySecret([5; 10]);
    let addrs = host.set_udp(Some(UdpHostOptions { bind: "127.0.0.1:0".parse().unwrap(), secret, max_clients: 2 }), DropReason::Left).expect("binds a free port");
    let port = addrs.iter().map(|a| a.port()).find(|p| *p != 0).expect("a port");
    let target = JoinTarget::Address { addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port), secret };
    let mut guest = join_udp(&options(&clock, 0x2222, "Guest"), &target, temp_dir("guest")).expect("connects");
    assert_eq!(guest.status().state, LinkState::Connecting);
    let (mut own_in, mut guest_in) = (LinkInbox::default(), LinkInbox::default());
    let mut events = Vec::new();
    let (mut own_slot, mut guest_slot) = (None, None);
    for _ in 0..400 {
        own_in.session.clear();
        guest_in.session.clear();
        pump(&clock, &mut host, &mut events, &mut [(&mut own, &mut own_in), (&mut guest, &mut guest_in)]);
        own_slot = own_slot.or(welcome_slot(&own_in));
        guest_slot = guest_slot.or(welcome_slot(&guest_in));
        if own_slot.is_some() && guest_slot.is_some() { break; }
    }
    assert_eq!(own_slot, Some(PlayerSlot(0)), "the host's own client is slot 0");
    assert_eq!(guest_slot, Some(PlayerSlot(1)));
    assert!(events.iter().any(|e| matches!(e, HostEvent::PlayerJoined { slot: PlayerSlot(1), client_id: 0x2222, .. })));

    // Both field the starter; the host validates and the lobby can start.
    let packed = packed_starter();
    for link in [&mut own, &mut guest] {
        link.send(ClientMsg::UploadBlueprint(packed.clone()));
        link.send(ClientMsg::Lobby(LobbyRequest::SetBlueprint(packed.hash)));
    }
    let mut view = None;
    let mut readied = false;
    for _ in 0..800 {
        own_in.session.clear();
        guest_in.session.clear();
        pump(&clock, &mut host, &mut events, &mut [(&mut own, &mut own_in), (&mut guest, &mut guest_in)]);
        if let Some(v) = last_view(&own_in) { view = Some(v); }
        let accepted = events.iter().filter(|e| matches!(e, HostEvent::BlueprintAccepted { .. })).count();
        if accepted >= 1 && !readied {
            readied = true;
            own.send(ClientMsg::Lobby(LobbyRequest::SetReady(true)));
            guest.send(ClientMsg::Lobby(LobbyRequest::SetReady(true)));
        }
        if view.as_ref().is_some_and(|v| v.can_start) { break; }
    }
    let view = view.expect("lobby views arrive");
    assert!(!events.iter().any(|e| matches!(e, HostEvent::BlueprintRejected { .. })), "the starter is accepted");
    assert_eq!(view.members.len(), 2);
    assert!(view.members.iter().all(|m| m.blueprint == Some(packed.hash) && m.ready), "{view:?}");
    assert!(view.issues.is_empty() && view.can_start, "{:?}", view.issues);
    assert_eq!(view.name, "Evening sail");
    // The guest's design is in the host's store under the hash it chose.
    assert!(host.blueprint(&packed.hash).is_some());

    // Start: the session asks the host app, which builds the sortie from the lobby.
    events.clear();
    own.send(ClientMsg::Lobby(LobbyRequest::Start));
    for _ in 0..50 {
        pump(&clock, &mut host, &mut events, &mut [(&mut own, &mut own_in), (&mut guest, &mut guest_in)]);
        if events.iter().any(|e| matches!(e, HostEvent::StartRequested { .. })) { break; }
    }
    assert!(events.iter().any(|e| matches!(e, HostEvent::StartRequested { by: PlayerSlot(0) })));
    let setup = host.lobby_setup();
    let blueprints: Vec<content::blueprint::Blueprint> = setup.players.iter().filter_map(|p| p.blueprint).collect::<BTreeSet<_>>().into_iter().filter_map(|h| {
        let compressed = host.blueprint(&h)?;
        match content::share::unpack(&content::share::PackedBlueprint { hash: h, compressed }) { Ok(content::blueprint::Shareable::Ship(bp)) => Some(*bp), _ => None }
    }).collect();
    assert_eq!(blueprints.len(), 1);
    let config = sim_systems::modes::sandbox_config(&setup, &blueprints, &testkit::campaign());
    assert_eq!(config.players.len(), 2);
    host.start_sortie(sim_systems::Sim::new(config, testkit::deps()).expect("the sortie builds")).expect("starts");
    let (mut own_frame, mut guest_frame) = (false, false);
    for _ in 0..600 {
        own_in.session.clear();
        guest_in.session.clear();
        pump(&clock, &mut host, &mut events, &mut [(&mut own, &mut own_in), (&mut guest, &mut guest_in)]);
        own_frame |= own_in.frame.is_some();
        guest_frame |= guest_in.frame.is_some();
        if own_frame && guest_frame { break; }
    }
    assert_eq!(host.phase(), crate::seams::SessionPhase::Sortie);
    assert!(own_frame && guest_frame, "both ends draw the running sortie");
    assert!(guest.status().interp.frames > 0);
    assert_eq!(host.connection_panel().players.len(), 2);
    assert!(!host.drain_log().is_empty(), "joins are in the connection log");

    // A polite leave mid-sortie reaches the host before the disconnect: a sandbox leave is not held, a drop
    // would be. The host keeps running on its own thread meanwhile, as the clock thread does.
    events.clear();
    std::thread::scope(|scope| {
        let pumping = scope.spawn(|| {
            for _ in 0..60 { pump(&clock, &mut host, &mut events, &mut [(&mut own, &mut own_in)]); std::thread::sleep(Duration::from_millis(4)); }
        });
        std::thread::sleep(Duration::from_millis(30));
        guest.close();
        // Dropped at once, as `BpSession::teardown` does: the disconnect follows the close directly.
        drop(guest);
        pumping.join().expect("the host thread");
    });
    assert!(events.iter().any(|e| matches!(e, HostEvent::PlayerLeft { slot: PlayerSlot(1), reason: DropReason::Left, held: false, .. })), "{events:?}");

    // Kick with ban: the member is told why, and the same install cannot come back.
    let mut guest = join_udp(&options(&clock, 0x2222, "Guest"), &target, temp_dir("guest1")).expect("connects");
    for _ in 0..400 {
        guest_in.session.clear();
        pump(&clock, &mut host, &mut events, &mut [(&mut own, &mut own_in), (&mut guest, &mut guest_in)]);
        if guest.status().state == LinkState::Joined { break; }
    }
    assert_eq!(guest.status().state, LinkState::Joined, "a sandbox takes joins mid-sortie");
    let slot = guest_in.session.iter().find_map(|e| if let ClientEvent::Welcomed(w) = e { Some(w.slot) } else { None }).unwrap_or(PlayerSlot(1));
    host.kick(slot, true);
    let mut end = None;
    for _ in 0..200 {
        guest_in.session.clear();
        pump(&clock, &mut host, &mut events, &mut [(&mut own, &mut own_in), (&mut guest, &mut guest_in)]);
        end = end.or(ended(&guest_in));
        if end.is_some() { break; }
    }
    assert_eq!(end, Some(SessionEnd::Dropped(DropReason::Banned)));
    assert_eq!(guest.status().state, LinkState::Ended(SessionEnd::Dropped(DropReason::Banned)));
    let mut again = join_udp(&options(&clock, 0x2222, "Guest"), &target, temp_dir("guest2")).expect("connects");
    let mut again_in = LinkInbox::default();
    let mut refused = None;
    for _ in 0..400 {
        again_in.session.clear();
        pump(&clock, &mut host, &mut events, &mut [(&mut own, &mut own_in), (&mut again, &mut again_in)]);
        refused = refused.or(again_in.session.iter().find_map(|e| if let ClientEvent::Refused(r) = e { Some(r.clone()) } else { None }));
        if refused.is_some() { break; }
    }
    assert_eq!(refused, Some(JoinRefusal::Banned));
    let _ = host.end_sortie().expect("the sortie hands back its Sim");
    own.close();
    assert_eq!(own.status().state, LinkState::Closed);
    own.close();
    host.shutdown();
}

/// A new lobby secret replaces the UDP backend: connected UDP members are told and dropped, the host's own
/// client stays, and the old key no longer connects.
#[test]
fn a_new_secret_drops_udp_members_and_keeps_the_host() {
    let clock = ManualClock::new();
    let (mut host, mut own) = loopback_named(&options(&clock, 0x10, "Host"), "", sandbox_setup(), 1_000, false, testkit::pool(), temp_dir("rh"), &[]);
    let old = LobbySecret([3; 10]);
    let addrs = host.set_udp(Some(UdpHostOptions { bind: "127.0.0.1:0".parse().unwrap(), secret: old, max_clients: 2 }), DropReason::Left).expect("binds");
    let port = addrs[0].port();
    let target = JoinTarget::Address { addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port), secret: old };
    let mut guest = join_udp(&options(&clock, 0x20, "Guest"), &target, temp_dir("rg")).expect("connects");
    let (mut own_in, mut guest_in) = (LinkInbox::default(), LinkInbox::default());
    let mut events = Vec::new();
    for _ in 0..400 {
        pump(&clock, &mut host, &mut events, &mut [(&mut own, &mut own_in), (&mut guest, &mut guest_in)]);
        if guest.status().state == LinkState::Joined && own.status().state == LinkState::Joined { break; }
    }
    assert_eq!(guest.status().state, LinkState::Joined);
    // The member keeps polling on its own thread, as a remote game would, while the host replaces the backend.
    let watcher = std::thread::spawn(move || {
        let mut inbox = LinkInbox::default();
        let end = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < end {
            inbox.session.clear();
            guest.poll(0.0, &mut inbox);
            if let Some(e) = ended(&inbox) { return Some(e); }
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    });
    std::thread::sleep(Duration::from_millis(20));
    let fresh = LobbySecret([4; 10]);
    let addrs = host.set_udp(Some(UdpHostOptions { bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port), secret: fresh, max_clients: 2 }), DropReason::SecretRegenerated).expect("rebinds the same port");
    assert_eq!(addrs[0].port(), port);
    for _ in 0..50 { pump(&clock, &mut host, &mut events, &mut [(&mut own, &mut own_in)]); }
    assert_eq!(watcher.join().expect("the watcher"), Some(SessionEnd::Dropped(DropReason::SecretRegenerated)), "the member is told why");
    assert!(events.iter().any(|e| matches!(e, HostEvent::PlayerLeft { slot: PlayerSlot(1), reason: DropReason::SecretRegenerated, .. })));
    assert_eq!(own.status().state, LinkState::Joined, "the host's own client is not on UDP");
    // The old key is refused by netcode (no reply at all); the new one gets in.
    let mut stale = join_udp(&options(&clock, 0x30, "Late"), &target, temp_dir("rs")).expect("socket");
    let mut stale_in = LinkInbox::default();
    for _ in 0..120 { pump(&clock, &mut host, &mut events, &mut [(&mut own, &mut own_in), (&mut stale, &mut stale_in)]); }
    assert_eq!(stale.status().state, LinkState::Connecting, "an old code never connects");
    let mut rejoin = join_udp(&options(&clock, 0x20, "Guest"), &JoinTarget::Address { addr: target.addr(), secret: fresh }, temp_dir("rj")).expect("socket");
    let mut rejoin_in = LinkInbox::default();
    for _ in 0..400 {
        pump(&clock, &mut host, &mut events, &mut [(&mut own, &mut own_in), (&mut rejoin, &mut rejoin_in)]);
        if rejoin.status().state == LinkState::Joined { break; }
    }
    assert_eq!(rejoin.status().state, LinkState::Joined, "the new key connects");
    // Hosting stops: the UDP member is told it is over.
    let removed = host.set_udp(None, DropReason::Left).expect("stops");
    assert!(removed.is_empty());
    let mut end = None;
    for _ in 0..200 {
        rejoin_in.session.clear();
        pump(&clock, &mut host, &mut events, &mut [(&mut own, &mut own_in), (&mut rejoin, &mut rejoin_in)]);
        end = end.or(ended(&rejoin_in));
        if end.is_some() { break; }
    }
    // The disconnect closes the link as the reason arrives; either way the member reads that the session closed.
    assert!(matches!(end, Some(SessionEnd::Dropped(DropReason::Left) | SessionEnd::HostQuit)), "{end:?}");
    host.shutdown();
}

#[test]
fn a_link_is_inert_once_closed_and_sends_nothing_before_it_joins() {
    let clock = ManualClock::new();
    let (mut host, mut own) = loopback(&options(&clock, 0x40, "Solo"), sandbox_setup(), 1_000, false, testkit::pool(), temp_dir("solo"), &[packed_starter()]);
    // Seeded designs are in both stores before anything travels.
    let hash = packed_starter().hash;
    assert!(own.blueprint(&hash).is_some());
    assert!(host.blueprint(&hash).is_some());
    assert!(own.blueprint(&content::ids::ContentHash::EMPTY).is_none());
    assert_eq!(own.status().state, LinkState::Connecting);
    own.send(ClientMsg::Lobby(LobbyRequest::SetReady(true)));
    let mut inbox = LinkInbox::default();
    let mut events = Vec::new();
    for _ in 0..20 { pump(&clock, &mut host, &mut events, &mut [(&mut own, &mut inbox)]); if own.status().state == LinkState::Joined { break; } }
    assert_eq!(own.status().state, LinkState::Joined);
    assert!(host.set_phase(crate::seams::SessionPhase::Sortie).is_err(), "sorties are entered by start_sortie");
    own.close();
    inbox = LinkInbox::default();
    own.poll(0.0, &mut inbox);
    assert!(inbox.session.is_empty() && inbox.frame.is_none());
    host.shutdown();
    host.shutdown();
}

#[test]
fn test_link_flag_parses_round_trip_and_loss() {
    let p = parse_test_link("100,2").expect("parses");
    assert!((p.latency_ms - 50.0).abs() < 1e-3, "each end delays half the round trip");
    assert!((p.loss.mean() - 0.02).abs() < 1e-3);
    let p = parse_test_link("150, 5, 30").expect("parses with jitter");
    assert!((p.jitter_ms - 30.0).abs() < 1e-3);
    for bad in ["", "100", "100,x", "-1,2", "100,150", "1,2,3,4"] { assert!(parse_test_link(bad).is_none(), "{bad}"); }
}

// ---- Resume from a checkpoint (suspend save) ----

/// A campaign sortie config from a fresh theatre: the first template on offer, accepted solo.
fn campaign_config(client_id: u64) -> sim_systems::protocol::SortieConfig {
    use sim_systems::ids::PlayerMask;
    use content::save::TheatreAction;
    use sim_systems::theatre::{NewCampaign, Theatre, TheatreCommand, TheatreData};
    let data = TheatreData { campaign: testkit::campaign(), catalogue: testkit::catalogue() };
    let new = NewCampaign { seed: 7, difficulty: content::campaign::DifficultySliders::NORMAL, ironman: false, slot_name: "t".into(), players: vec![(client_id, "Lantern".into(), testkit::starter())], start_act: content::enums::Act::Act1 };
    let mut t = Theatre::new_campaign(new, data);
    let op = t.state.offered.iter().find(|o| matches!(t.data().campaign.operations[*o].kind, content::campaign::OperationKind::Template(_))).cloned().expect("a template on offer");
    t.command(PlayerSlot(0), true, PlayerMask(0).with(PlayerSlot(0)), TheatreCommand::Propose(TheatreAction::AcceptOperation(op.clone()))).expect("accepted");
    t.start_sortie(&op, &[PlayerSlot(0)]).expect("the sortie starts")
}

fn campaign_setup(client_id: u64) -> LobbySetup {
    LobbySetup {
        mode: GameMode::Campaign, ruleset: Ruleset::default(), seed: 7,
        players: vec![sim_systems::modes::LobbyPlayer { slot: PlayerSlot(0), client_id, name: "Lantern".into(), team: 0, blueprint: None, ready: true, complexity: 0.0 }],
        skirmish: None, weather: WeatherKind::Clear, sea_state: SeaState(2), time_of_day: TimeOfDay::Day, map: None, at_1_0: false,
    }
}

/// Start a local loopback sortie from `sim` (the session's offline flow: welcome first, then the sortie, the
/// client reporting loaded) and pump `passes` times. Returns both ends and everything the client received.
fn run_local(clock: &ManualClock, sim: sim_systems::Sim, passes: usize) -> (NetHost, NetLink, LinkInbox) {
    let opts = options(clock, 100, "Lantern");
    let packed: Vec<_> = sim.config().blueprints.iter().map(|b| content::share::pack(&content::blueprint::Shareable::Ship(Box::new(b.clone())))).collect();
    let (mut host, mut link) = loopback(&opts, campaign_setup(100), 1_000, false, testkit::pool(), temp_dir("resume"), &packed);
    let mut inbox = LinkInbox::default();
    let mut events = Vec::new();
    for _ in 0..50 {
        pump(clock, &mut host, &mut events, &mut [(&mut link, &mut inbox)]);
        if welcome_slot(&inbox).is_some() { break; }
    }
    host.start_sortie(sim).expect("the sortie starts");
    for _ in 0..passes {
        pump(clock, &mut host, &mut events, &mut [(&mut link, &mut inbox)]);
        if inbox.session.iter().any(|e| matches!(e, ClientEvent::SortieLoading(_))) { link.send(ClientMsg::Lobby(LobbyRequest::SetReady(true))); }
        inbox.session.clear();
    }
    (host, link, inbox)
}

fn pump_n(clock: &ManualClock, host: &mut NetHost, link: &mut NetLink, inbox: &mut LinkInbox, n: usize) {
    let mut events = Vec::new();
    for _ in 0..n { pump(clock, host, &mut events, &mut [(&mut *link, &mut *inbox)]); }
}

fn rejections(inbox: &LinkInbox) -> Vec<String> {
    inbox.events.iter().filter(|e| matches!(e.event, sim_systems::protocol::SimEvent::CommandRejected { .. })).map(|e| format!("{:?}", e.event)).collect()
}

/// A campaign sortie paused, checkpointed (Save and leave) and resumed on a fresh local session, twice: the
/// resumed client learns its own hull, its commands are not taken for duplicates of the first session's,
/// the sortie unpauses and runs on, and a sortie resumed once suspends and resumes again.
#[test]
fn a_resumed_sortie_shows_its_own_hull_takes_commands_and_suspends_again() {
    use sim_systems::protocol::{Command, SimEvent};
    let _serial = testkit::serial();
    let clock = ManualClock::new();
    let mut sim = sim_systems::Sim::new(campaign_config(100), testkit::deps()).expect("the sortie builds");
    for round in 0..3 {
        let (mut host, mut link, mut inbox) = run_local(&clock, sim, 120);
        let frame = inbox.frame.clone().expect("frames arrive");
        let own = inbox.events.iter().filter_map(|e| if let SimEvent::EntityRevealed(i) = &e.event { Some(i) } else { None }).find(|i| i.player == Some(PlayerSlot(0)));
        assert!(own.is_some_and(|i| !i.blueprint.is_empty()), "round {round}: the client is told its own hull and design");
        assert!(inbox.events.iter().any(|e| matches!(e.event, SimEvent::SortieStarted { .. })), "round {round}: sortie start");
        assert!(frame.snapshot.ship.is_some(), "round {round}: the frame carries the own ship");
        if round > 0 {
            assert!(frame.snapshot.pacing.paused, "round {round}: a sortie suspended while paused comes back paused");
            assert!(inbox.events.iter().any(|e| matches!(&e.event, SimEvent::PacingChanged(p) if p.paused)), "round {round}: the client is told it is paused");
            link.send(ClientMsg::Command(Command::Pause(false)));
        }
        let before = host.sim().expect("running").tick().0;
        pump_n(&clock, &mut host, &mut link, &mut inbox, 90);
        let after = host.sim().expect("running").tick().0;
        assert!(!host.sim().unwrap().pacing().paused, "round {round}: unpaused");
        assert!(after > before + 30, "round {round}: the tick advances ({before} to {after})");
        // The pause menu, then Save and leave: a checkpoint of the paused moment.
        link.send(ClientMsg::Command(Command::Pause(true)));
        pump_n(&clock, &mut host, &mut link, &mut inbox, 20);
        assert!(host.sim().unwrap().pacing().paused, "round {round}: paused again");
        assert_eq!(rejections(&inbox), Vec::<String>::new(), "round {round}: no command was refused");
        let checkpoint = host.sim().unwrap().checkpoint();
        let _ = host.end_sortie();
        link.close();
        sim = sim_systems::Sim::from_checkpoint(&checkpoint, testkit::deps()).expect("the checkpoint restores");
        assert!(sim.command_seq_floor(PlayerSlot(0)) >= 1 + 2 * round as u32, "the restored Sim remembers the old numbers");
    }
}
