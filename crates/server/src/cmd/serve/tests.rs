//! Tests of unit `server-dedicated`.
use super::*;
use net::portmap::{MapFailure, MapProtocol, VpnAddr, VpnKind};
use net::session::client::{ClientSession, ClientState};
use net::session::ClientOptions;

fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bp-serve-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn config_checks_and_lobby_setup() {
    let mut c = ServerConfig { mode: GameMode::Campaign, ..ServerConfig::default() };
    assert!(matches!(lobby_setup(&c, 1), Err(CliError::Config(m)) if m.contains("campaign")));
    for bad in [GameMode::Trial, GameMode::Splash, GameMode::Tutorial, GameMode::PvpEscort] { c.mode = bad; assert!(lobby_setup(&c, 1).is_err(), "{bad:?}"); }
    c.mode = GameMode::Sandbox;
    c.port = 0;
    assert!(lobby_setup(&c, 1).is_err());
    c.port = 47610;
    c.lobby_size = 0;
    assert!(lobby_setup(&c, 1).is_err());
    c.lobby_size = 4;
    let a = lobby_setup(&c, 7).unwrap();
    assert_eq!(a, lobby_setup(&c, 7).unwrap(), "the same seed gives the same lobby");
    assert_eq!((a.mode, a.seed, a.skirmish.is_none(), a.map.is_none()), (GameMode::Sandbox, 7, true, true));
    assert!((2..=5).contains(&a.sea_state.0));
    c.mode = GameMode::Skirmish;
    let s = lobby_setup(&c, 7).unwrap();
    let opts = s.skirmish.expect("a skirmish lobby has options");
    assert!(SKIRMISH_TEMPLATES.contains(&opts.template));
    let last = content::campaign::load::shipped_campaign().acts.iter().map(|a| a.act).max().unwrap_or(Act::Act1);
    assert_eq!(opts.act, Act::ALL.iter().copied().find(|a| c.ruleset.tonnage_cap_t <= a.hull_cap_t()).unwrap_or(Act::Act4).min(last));
    // A cap past every act's still fields no act this build does not ship.
    let mut big = c.clone();
    big.ruleset.tonnage_cap_t = 5_000.0;
    assert_eq!(lobby_setup(&big, 7).unwrap().skirmish.expect("options").act, last);
    c.mode = GameMode::PvpElimination;
    c.ruleset.luau = true;
    assert!(!lobby_setup(&c, 7).unwrap().ruleset.luau, "Luau never runs in PvP");
    // Seeds vary the lobby.
    let seas: std::collections::BTreeSet<u8> = (0..64).map(|s| lobby_setup(&ServerConfig::default(), s).unwrap().sea_state.0).collect();
    assert!(seas.len() > 1);
}

#[test]
fn join_lines_name_every_way_in() {
    let secret = LobbySecret([7; 10]);
    let local = LocalAddrs { lan: vec!["192.168.1.20".parse().unwrap()], vpn: vec![VpnAddr { kind: VpnKind::Tailscale, ip: "100.101.102.103".parse().unwrap() }] };
    let external: SocketAddr = "203.0.113.9:47610".parse().unwrap();
    let mapped = MappingOutcome::Mapped { protocol: MapProtocol::UpnpIgd, external, lease_s: 3600, permanent: false };
    let lines = join_lines(&secret, 47610, Some(&mapped), true, &local);
    assert!(lines[0].contains(&secret.lobby_key()));
    let code_of = |l: &str| l.split_whitespace().nth(2).unwrap().to_string();
    let internet = lines.iter().find(|l| l.contains("internet")).expect("an internet code");
    let decoded = JoinCode::decode(&code_of(internet)).unwrap();
    assert_eq!((decoded.addr, decoded.secret), (external, secret));
    let lan = lines.iter().find(|l| l.contains("(LAN 192.168.1.20)")).expect("a LAN code");
    assert_eq!(JoinCode::decode(&code_of(lan)).unwrap().addr, "192.168.1.20:47610".parse().unwrap());
    assert!(lines.iter().any(|l| l.contains("Tailscale 100.101.102.103")));
    let double = join_lines(&secret, 47611, Some(&MappingOutcome::DoubleNat { protocol: MapProtocol::Pcp, external: "10.0.0.5:47611".parse().unwrap() }), true, &local);
    assert!(double.iter().any(|l| l.starts_with("double NAT")) && double.iter().any(|l| l.contains("forward UDP port 47611 to this machine's LAN address 192.168.1.20")));
    assert!(!double.iter().any(|l| l.contains("(internet")), "no internet code behind double NAT");
    let failed = join_lines(&secret, 47610, Some(&MappingOutcome::Failed { upnp: MapFailure::Timeout, nat_pmp_pcp: MapFailure::NoGateway }), true, &LocalAddrs::default());
    assert!(failed.iter().any(|l| l == "port mapping failed: UPnP did not answer within 3 s, NAT-PMP and PCP found no gateway; players on this network can still join"));
    assert!(failed.iter().any(|l| l.starts_with("to host for the internet, forward UDP port 47610")));
    assert!(join_lines(&secret, 47610, None, true, &LocalAddrs::default()).iter().any(|l| l.starts_with("port mapping: asking the router")));
    assert!(join_lines(&secret, 47610, None, false, &LocalAddrs::default()).iter().any(|l| l.starts_with("port mapping off (--no-map)")));
}

/// The dedicated host over loopback UDP: a player joins, uploads a design and readies; the sandbox starts on
/// its own, runs, ends when the player leaves, saves its replay, writes the connection log, and returns to the
/// lobby (on the next seed) after the debrief.
#[test]
fn a_player_joins_sails_and_leaves_over_loopback_udp() {
    let dir = scratch("loopback");
    let data = crate::runtime::load_data().unwrap();
    let pool = crate::runtime::sim_pool(3).unwrap();
    let config = ServerConfig { mode: GameMode::Sandbox, lobby_size: 4, lobby_name: "test".into(), ..ServerConfig::default() };
    let bind: SocketAddr = (std::net::Ipv4Addr::LOCALHOST, 0).into();
    let mut host = Host::new(config, data.clone(), pool, bind, false, dir.join("connection.log"), dir.join("replays")).unwrap();
    host.echo = false;
    host.print_join();
    assert!(host.lines.iter().any(|l| l.starts_with("lobby key ")));
    assert_ne!(host.addr.port(), 0);
    let clock = Arc::new(SystemClock::new());
    let transport = net::transport::netcode::NetcodeClientBackend::connect_udp(host.addr, &host.secret(), 4242, clock.unix_now(), None).unwrap();
    let options = ClientOptions { identity: net::join::ClientIdentity { client_id: 4242, name: "Tester".into() }, game_version: env!("CARGO_PKG_VERSION").into(), build: Default::default(), interp: Default::default() };
    let mut client = ClientSession::new(options, Box::new(transport), clock, data.catalogue.clone(), Box::new(net::blueprints::MemoryStore::default()));
    let pump_until = |host: &mut Host, client: &mut ClientSession, limit_s: f64, done: &dyn Fn(&Host, &ClientSession) -> bool| -> bool {
        let end = std::time::Instant::now() + Duration::from_secs_f64(limit_s);
        while std::time::Instant::now() < end {
            host.pump().unwrap();
            let _ = client.update().unwrap();
            if done(host, client) { return true; }
            std::thread::sleep(Duration::from_millis(1));
        }
        false
    };
    assert!(pump_until(&mut host, &mut client, 10.0, &|_, c| matches!(c.state(), ClientState::Joined(_))), "the player joins");
    let mut bp = content::campaign::load::stock_blueprint("starters/fac22.bp.ron").unwrap();
    content::blueprint::io::canonicalize_blueprint(&mut bp);
    bp.header.hash = content::blueprint::io::compute_hash(&bp);
    let packed = content::share::pack(&content::blueprint::Shareable::Ship(Box::new(bp.clone())));
    client.upload_blueprint(packed);
    client.lobby(net::msg::LobbyRequest::SetBlueprint(bp.header.hash));
    client.lobby(net::msg::LobbyRequest::SetReady(true));
    assert!(pump_until(&mut host, &mut client, 20.0, &|h, _| h.server.phase() == SessionPhase::Sortie), "the dedicated host starts the sandbox by itself");
    assert!(host.designs.contains_key(&bp.header.hash));
    assert!(pump_until(&mut host, &mut client, 5.0, &|h, _| h.server.sim().is_some_and(|s| s.tick().0 > 60)), "the sortie runs");
    client.leave();
    assert!(pump_until(&mut host, &mut client, 10.0, &|h, _| h.server.phase() == SessionPhase::AfterAction), "an empty sandbox ends");
    let saved = std::fs::read_dir(dir.join("replays")).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().ends_with(".bpreplay")).count();
    assert_eq!(saved, 1, "the replay is saved beside the config");
    let seed = host.server.lobby().setup().seed;
    host.after_action_until = Some(Duration::ZERO);
    host.pump().unwrap();
    assert_eq!(host.server.phase(), SessionPhase::Lobby);
    assert_eq!(host.server.lobby().setup().seed, seed.wrapping_add(1), "the next lobby is on the next seed");
    host.next_second = Duration::ZERO;
    host.pump().unwrap();
    let log = std::fs::read_to_string(dir.join("connection.log")).unwrap();
    assert!(log.contains("Joined"), "{log}");
    assert!(!host.command("status") && !host.command("kick 9") && !host.command("") && !host.command("dance"));
    assert!(host.command("quit"));
    host.stop();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn build_sortie_refuses_what_cannot_start() {
    let data = crate::runtime::load_data().unwrap();
    let mut setup = lobby_setup(&ServerConfig::default(), 3).unwrap();
    setup.players.push(sim_systems::modes::LobbyPlayer { slot: PlayerSlot(0), client_id: 1, name: "A".into(), team: 0, blueprint: None, ready: false, complexity: 0.0 });
    assert!(matches!(build_sortie(&setup, &[], &data), Err(CliError::Config(_))), "a player not ready");
    setup.mode = GameMode::Campaign;
    setup.players[0].ready = true;
    assert!(build_sortie(&setup, &[], &data).is_err());
}
