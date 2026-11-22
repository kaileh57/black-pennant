//! Tests of units `net-server-session` and `net-client-session`, end to end: a `ServerSession` and real
//! `ClientSession`s over the in-memory transport (renet2 channels over renet2_netcode memory sockets, through
//! the link conditioner when asked), a real two-player skirmish `Sim`, a `ManualClock`.
use super::*;
use crate::blueprints::MemoryStore;
use crate::clock::ManualClock;
use crate::interp::{InterpTuning, ShellTracker};
use crate::join::ClientIdentity;
use crate::msg::BuildKind;
use crate::session::client::{ClientSession, ClientState};
use crate::session::{ClientEvent, ClientOptions};
use crate::transport::conditioner::{BurstLoss, ConditionerProfile, Conditioning};
use crate::transport::netcode::{memory_pair, NetcodeClientBackend};
use crate::transport::ClientTransport;
use content::campaign::CampaignData;
use content::enums::{Act, TimeOfDay};
use glam::DVec3;
use sim_core::events::PhysEvent;
use sim_core::math::Pose;
use sim_core::projectiles::ProjectileSpawn;
use sim_core::EntityId;
use sim_systems::modes::{LobbyPlayer, SkirmishOptions};
use sim_systems::protocol::command::{Command, ControlRole, ShotDesignation};
use std::sync::OnceLock;

struct Data { catalogue: Arc<Catalogue>, campaign: Arc<CampaignData>, bp: Blueprint, packed: PackedBlueprint }

fn data() -> &'static Data {
    static DATA: OnceLock<Data> = OnceLock::new();
    DATA.get_or_init(|| {
        let catalogue = Arc::new(Catalogue::load_embedded().expect("catalogue"));
        let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign"));
        let mut bp = content::campaign::load::stock_blueprint("starters/fac22.bp.ron").expect("the 22 m starter");
        content::blueprint::io::canonicalize_blueprint(&mut bp);
        bp.header.hash = content::blueprint::io::compute_hash(&bp);
        let packed = content::share::pack(&Shareable::Ship(Box::new(bp.clone())));
        assert_eq!(packed.hash, bp.header.hash);
        Data { catalogue, campaign, bp, packed }
    })
}

fn pool() -> Arc<rayon::ThreadPool> { Arc::new(rayon::ThreadPoolBuilder::new().num_threads(3).build().expect("pool")) }

fn deps() -> sim_systems::SimDeps {
    let d = data();
    sim_systems::SimDeps { catalogue: d.catalogue.clone(), campaign: d.campaign.clone(), designs: Default::default(), scripts: Box::new(sim_systems::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() }
}

fn lobby_setup(mode: GameMode) -> LobbySetup {
    LobbySetup {
        mode, ruleset: Default::default(), seed: 9, players: Vec::<LobbyPlayer>::new(),
        skirmish: Some(SkirmishOptions { template: content::campaign::MissionType::ConvoyRaid, act: Act::Act1, enemy_per_player: 1, difficulty: Default::default() }),
        weather: content::weather::WeatherKind::Clear, sea_state: content::weather::SeaState(3), time_of_day: TimeOfDay::Day, map: None, at_1_0: false,
    }
}

fn client_options(i: u64) -> ClientOptions {
    ClientOptions { identity: ClientIdentity { client_id: 1000 + i, name: format!("Skipper {i}") }, game_version: "0.1.0-test".into(), build: BuildKind::Dev, interp: InterpTuning::default() }
}

/// Server truth: every vessel's pose at a tick, from the omniscient snapshot.
type Truth = BTreeMap<u64, BTreeMap<EntityId, Pose>>;

struct World {
    clock: ManualClock,
    server: ServerSession,
    clients: Vec<ClientSession>,
    /// Spare client ends of the memory pair (for rejoins).
    spare: Vec<NetcodeClientBackend>,
    host_events: Vec<HostEvent>,
    client_events: Vec<Vec<ClientEvent>>,
    truth: Truth,
    /// Per client: (frames rendered, frames extrapolated, decoded vessels not drawn).
    renders: Vec<(u64, u64, u64)>,
    next_render: Duration,
    /// Decoded samples checked against the server so far, and when the next check runs.
    checked: usize,
    next_check: Duration,
}

fn world(clients: usize, spare: usize, link: Option<ConditionerProfile>) -> World { world_in(GameMode::Skirmish, clients, spare, link) }

fn world_in(mode: GameMode, clients: usize, spare: usize, link: Option<ConditionerProfile>) -> World { world_seeded(mode, clients, spare, link, 11) }

fn world_seeded(mode: GameMode, clients: usize, spare: usize, link: Option<ConditionerProfile>, seed: u64) -> World {
    let clock = ManualClock::new();
    clock.set(Duration::from_secs(5));
    let c: Arc<dyn Clock> = Arc::new(clock.clone());
    let conditioning = link.map(|profile| Conditioning { profile, seed, clock: c.clone() });
    let (host, mut ends) = memory_pair((clients + spare) as u16, c.unix_now(), conditioning);
    let mut options = ServerOptions::new("0.1.0-test", mode);
    options.lobby_name = "test".into();
    let mut server = ServerSession::new(options, lobby_setup(mode), c.clone(), pool(), data().catalogue.clone(), Box::new(MemoryStore::default()));
    server.add_transport(Box::new(host));
    let spare_ends = ends.split_off(clients);
    let clients: Vec<ClientSession> = ends.into_iter().enumerate().map(|(i, t)| ClientSession::new(client_options(i as u64), Box::new(t), c.clone(), data().catalogue.clone(), Box::new(MemoryStore::default()))).collect();
    let n = clients.len();
    World { clock, server, clients, spare: spare_ends, host_events: Vec::new(), client_events: vec![Vec::new(); n], truth: BTreeMap::new(), renders: vec![(0, 0, 0); n], next_render: Duration::ZERO, checked: 0, next_check: Duration::ZERO }
}

impl World {
    fn now(&self) -> Duration { self.clock.now() }
    /// One loop pass: server, then each client; render at 60 Hz.
    fn step(&mut self, dt: Duration) {
        self.clock.advance(dt);
        let ev = self.server.update().expect("server update");
        self.host_events.extend(ev);
        if let Some(sim) = self.server.sim() {
            let tick = sim.tick().0;
            if let std::collections::btree_map::Entry::Vacant(slot) = self.truth.entry(tick) {
                let snap = sim.snapshot(Viewer::Omniscient);
                slot.insert(snap.vessels.iter().map(|v| (v.entity, v.kin.pose)).collect());
                while self.truth.len() > 1200 { self.truth.pop_first(); }
            }
        }
        for (i, c) in self.clients.iter_mut().enumerate() {
            let ev = c.update().expect("client update");
            self.client_events[i].extend(ev);
        }
        if self.now() >= self.next_check {
            self.next_check = self.now() + Duration::from_millis(100);
            self.checked += self.check_poses();
        }
        if self.now() >= self.next_render {
            self.next_render = self.now() + Duration::from_micros(16_667);
            for (i, c) in self.clients.iter_mut().enumerate() {
                let Some(frame) = c.sample() else { continue };
                let r = &mut self.renders[i];
                r.0 += 1;
                if frame.extrapolating { r.1 += 1; if std::env::var("BP_DEBUG").is_ok() { eprintln!("client {i} extrapolating at {:?}: render {} newest {:?} stats {:?}", self.clock.now(), frame.render_tick, c.buffer().newest_tick(), c.buffer().stats()); } }
                // Every vessel the client holds samples of, was introduced to and that has not ended is drawn.
                let drawn: std::collections::BTreeSet<EntityId> = frame.snapshot.vessels.iter().map(|v| v.entity).collect();
                for (e, ring) in &c.buffer().vessels {
                    let life = c.buffer().life.get(e).copied().unwrap_or_default();
                    if life.revealed && life.gone_at.is_none() && !ring.samples.is_empty() && !drawn.contains(e) { r.2 += 1; }
                }
            }
        }
    }
    fn run(&mut self, seconds: f64, dt: Duration) {
        let end = self.now() + Duration::from_secs_f64(seconds);
        while self.now() < end { self.step(dt); }
    }
    fn run_until(&mut self, limit_s: f64, dt: Duration, mut done: impl FnMut(&World) -> bool) -> bool {
        let end = self.now() + Duration::from_secs_f64(limit_s);
        while self.now() < end {
            self.step(dt);
            if done(self) { return true; }
        }
        false
    }
    fn welcomed(&self, i: usize) -> bool { matches!(self.clients[i].state(), ClientState::Joined(_)) }
    fn client_saw(&self, i: usize, f: impl Fn(&ClientEvent) -> bool) -> bool { self.client_events[i].iter().any(f) }

    /// Admit everyone, choose the starter, ready up, start the skirmish and run until it is under way.
    fn start_skirmish(&mut self, dt: Duration) {
        let n = self.clients.len();
        assert!(self.run_until(10.0, dt, |w| (0..n).all(|i| w.welcomed(i))), "every client is admitted");
        // Slots go in admission order, which the link decides; keep `clients[i]` in slot `i`.
        let slot_of = |c: &ClientSession| match c.state() { ClientState::Joined(w) => w.slot.0, _ => u8::MAX };
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| slot_of(&self.clients[i]));
        let mut clients: Vec<Option<ClientSession>> = std::mem::take(&mut self.clients).into_iter().map(Some).collect();
        let mut events: Vec<Option<Vec<ClientEvent>>> = std::mem::take(&mut self.client_events).into_iter().map(Some).collect();
        for &i in &order { self.clients.push(clients[i].take().unwrap()); self.client_events.push(events[i].take().unwrap()); }
        for (i, c) in self.clients.iter().enumerate() {
            let ClientState::Joined(w) = c.state() else { unreachable!() };
            assert_eq!(w.slot, PlayerSlot(i as u8));
            assert_eq!(w.phase, SessionPhase::Lobby);
        }
        assert!(self.server.lobby().member(PlayerSlot(0)).is_some_and(|m| m.host), "the first client is the host");
        for c in &mut self.clients {
            c.upload_blueprint(data().packed.clone());
            c.lobby(LobbyRequest::SetBlueprint(data().bp.header.hash));
            c.lobby(LobbyRequest::SetReady(true));
        }
        let hash = data().bp.header.hash;
        assert!(self.run_until(20.0, dt, |w| w.host_events.iter().filter(|e| matches!(e, HostEvent::BlueprintAccepted { hash: h, .. } if *h == hash)).count() >= n
            && w.server.lobby().setup().players.iter().all(|p| p.ready)), "uploads accepted and everyone ready");
        assert!(self.run_until(5.0, dt, |w| (0..n).all(|i| w.client_saw(i, |e| matches!(e, ClientEvent::Lobby(v) if v.can_start)))), "every lobby view says the sortie can start");
        self.clients[0].lobby(LobbyRequest::Start);
        assert!(self.run_until(5.0, dt, |w| w.host_events.iter().any(|e| matches!(e, HostEvent::StartRequested { by } if *by == PlayerSlot(0)))));
        let d = data();
        let config = sim_systems::modes::skirmish_config(self.server.lobby().setup(), std::slice::from_ref(&d.bp), &d.campaign, &d.catalogue);
        let sim = Sim::new(config, deps()).expect("the skirmish starts");
        assert_eq!(sim.players().len(), n);
        self.server.start_sortie(sim).expect("start");
        assert_eq!(self.server.phase(), SessionPhase::Loading);
        let started = self.now();
        assert!(self.run_until(LOADING_TIMEOUT.as_secs_f64() + 5.0, dt, |w| w.server.phase() == SessionPhase::Sortie), "the sortie starts");
        let waited = self.now() - started;
        assert!(waited < Duration::from_secs(5), "every client reported loaded; the start did not wait for the timeout ({waited:?})");
        assert!(self.run_until(5.0, dt, |w| (0..n).all(|i| w.client_saw(i, |e| matches!(e, ClientEvent::SortieRunning { .. })))));
    }

    /// Check every decoded vessel sample of every client against the server's truth at its tick, and the
    /// clients against each other. Returns the number of samples checked.
    fn check_poses(&self) -> usize {
        let mut checked = 0;
        let mut by_tick: BTreeMap<(u64, EntityId), sim_core::math::Kinematics> = BTreeMap::new();
        for c in &self.clients {
            for (e, ring) in &c.buffer().vessels {
                for (tick, v) in &ring.samples {
                    let Some(truth) = self.truth.get(tick).and_then(|m| m.get(e)) else { continue };
                    let dp = (v.kin.pose.pos - truth.pos).length();
                    assert!(dp < 2.0e-3, "{e:?} at {tick}: decoded {:?} vs server {:?}", v.kin.pose.pos, truth.pos);
                    assert!(v.kin.pose.rot.angle_between(truth.rot) < 1.0e-3, "{e:?} at {tick}: rotation off");
                    if let Some(k) = by_tick.get(&(*tick, *e)) { assert_eq!(k, &v.kin, "both clients decode the same pose of {e:?} at {tick}"); }
                    by_tick.insert((*tick, *e), v.kin);
                    checked += 1;
                }
            }
        }
        checked
    }

    /// L939 on the live stream: every shell a client heard the end of, re-flown from its decoded spawn,
    /// passes within 0.5 m of the server's impact or splash point. Returns (shells checked, worst miss).
    fn check_shells(&self) -> (usize, f64) {
        let mut worst = 0.0f64;
        let mut n = 0;
        for ev in &self.client_events {
            let mut spawns: BTreeMap<u32, ProjectileSpawn> = BTreeMap::new();
            let mut tracker = ShellTracker::new();
            for e in ev {
                let ClientEvent::Sim(we) = e else { continue };
                match &we.event {
                    SimEvent::Phys(PhysEvent::ProjectileSpawned(s)) => { spawns.insert(s.id.0, *s); }
                    SimEvent::Phys(PhysEvent::ShellImpact { projectile, world_point: p, .. }) | SimEvent::Phys(PhysEvent::Splash { projectile, pos: p, .. }) => {
                        let Some(s) = spawns.get(&projectile.0) else { continue };
                        tracker.spawn(s, crate::interp::band_drag_k(&data().catalogue, s.band));
                        if let Some(c) = tracker.impact_point(*projectile, we.tick, *p) {
                            worst = worst.max((c - *p).length());
                            n += 1;
                        }
                        tracker.remove(*projectile);
                    }
                    _ => {}
                }
            }
        }
        (n, worst)
    }
}

fn own_ship(server: &ServerSession, slot: u8) -> &sim_systems::ship::Ship { server.sim().expect("sim").ships().of_player(PlayerSlot(slot)).expect("a hull") }

/// Two players over the in-memory transport: 60 s of a skirmish, commands from both, inputs, consistent
/// decoded state, manual fire with client-flown shells.
fn two_player_battle(link: Option<ConditionerProfile>, dt: Duration) -> World {
    let mut w = world(2, 0, link);
    w.start_skirmish(dt);
    let start_tick = w.server.sim().unwrap().tick().0;

    // Orders from each client, and manual fire from the host's client.
    let gun = own_ship(&w.server, 0).design.mounts.iter().find(|m| m.weapon == content::graph::WeaponClass::Gun).map(|m| m.slot);
    w.run(2.0, dt);
    let r0 = w.clients[0].buffer().newest_tick().unwrap_or(0);
    w.clients[0].send_command(Command::SetSpeed { speed_kn: 12.0 }, r0);
    let r1 = w.clients[1].buffer().newest_tick().unwrap_or(0);
    w.clients[1].send_command(Command::SetSpeed { speed_kn: 7.0 }, r1);
    if let Some(slot) = gun { w.clients[0].send_command(Command::TakeControl { role: ControlRole::Mount(slot), on: true }, r0); }
    let mut fire_at = w.now() + Duration::from_secs(1);
    let end = w.now() + Duration::from_secs(58);
    let mut shots = 0;
    while w.now() < end {
        w.step(dt);
        for c in &mut w.clients { c.set_input(PlayerInput { throttle: 0.0, rudder: 0.0, ..Default::default() }); }
        if let Some(slot) = gun {
            if w.now() >= fire_at && shots < 12 {
                fire_at = w.now() + Duration::from_secs(3);
                shots += 1;
                let tick = w.clients[0].buffer().newest_tick().unwrap_or(0);
                w.clients[0].send_command(Command::Fire { role: ControlRole::Mount(slot), shot: ShotDesignation::Direction { dir_local: DVec3::new(1.0, 0.02, 0.15 * (shots as f64 - 6.0) / 6.0) } }, tick);
            }
        }
    }
    let end_tick = w.server.sim().unwrap().tick().0;
    assert!(end_tick - start_tick >= 59 * 60, "the sim ran in real time: {} ticks", end_tick - start_tick);

    // Commands from each client were applied to its own hull.
    assert_eq!(own_ship(&w.server, 0).orders.speed_kn, Some(12.0));
    assert_eq!(own_ship(&w.server, 1).orders.speed_kn, Some(7.0));
    // Inputs flowed from both.
    for p in w.server.peers.values() { assert!(p.last_input_tick > 100, "inputs arrive at 30 Hz: {}", p.last_input_tick); }
    w
}

fn assert_battle(w: &mut World, max_extrapolated: f64) {
    // Both clients are current and agree with the server and with each other.
    let server_tick = w.server.sim().unwrap().tick().0;
    for c in &w.clients {
        let newest = c.buffer().newest_tick().expect("frames arrive");
        assert!(server_tick - newest < 60, "a client is within a second of the server: {newest} vs {server_tick}");
        assert!(c.buffer().vessels.len() >= 2, "both own-side hulls are drawn");
        assert_eq!(c.malformed, 0, "every frame, batch and message decoded");
    }
    assert!(w.checked > 3000, "{} decoded samples matched the server", w.checked);
    for (i, r) in w.renders.iter().enumerate() {
        assert!(r.0 > 3000, "client {i} rendered {} frames", r.0);
        assert!((r.1 as f64) / (r.0 as f64) <= max_extrapolated, "client {i} extrapolated {} of {} frames", r.1, r.0);
        assert_eq!(r.2, 0, "client {i}: every decoded, revealed vessel is drawn");
    }
    let (shells, worst) = w.check_shells();
    assert!(worst <= 0.5, "client-flown shells pass within 0.5 m of the server's impact ({worst} m over {shells})");
    if own_ship(&w.server, 0).design.mounts.iter().any(|m| m.weapon == content::graph::WeaponClass::Gun) { assert!(shells > 0, "the host's gun fired and its shells were checked"); }
    // The last frame draws every vessel each client holds samples of that has not ended, reveal or not
    // (a reveal that never came would leave a hull undrawn all sortie).
    for (i, c) in w.clients.iter_mut().enumerate() {
        let f = c.sample().expect("a frame");
        let drawn: std::collections::BTreeSet<EntityId> = f.snapshot.vessels.iter().map(|v| v.entity).collect();
        for (e, ring) in &c.buffer().vessels {
            let ended = c.buffer().life.get(e).is_some_and(|l| l.gone_at.is_some());
            if !ended && !ring.samples.is_empty() { assert!(drawn.contains(e), "client {i}: {e:?} has samples but is not drawn"); }
        }
    }
    // Nobody dropped or ended.
    let left: Vec<&HostEvent> = w.host_events.iter().filter(|e| matches!(e, HostEvent::PlayerLeft { .. })).collect();
    assert!(left.is_empty(), "{left:?}");
    for (i, ev) in w.client_events.iter().enumerate() { assert!(!ev.iter().any(|e| matches!(e, ClientEvent::Ended(_))), "client {i} ended"); }
    // Traffic: under the per-client budget, vessels cheap (L930-933).
    for (slot, t) in w.server.traffic() {
        assert!(t.p95_kbps <= 256.0, "{slot:?} p95 {} kbps", t.p95_kbps);
        assert!(t.vessel_updates > 0);
    }
}

#[test]
fn two_clients_play_a_skirmish_over_the_memory_transport() {
    let mut w = two_player_battle(None, Duration::from_millis(2));
    assert_battle(&mut w, 0.002);
    for i in 0..2 {
        assert!(w.client_saw(i, |e| matches!(e, ClientEvent::Sim(we) if matches!(we.event, SimEvent::SortieStarted { .. }))));
        assert!(w.client_saw(i, |e| matches!(e, ClientEvent::Sim(we) if matches!(we.event, SimEvent::EntityRevealed(_)))));
        assert!(w.client_events[i].iter().filter(|e| matches!(e, ClientEvent::Connection(_))).count() >= 55, "the panel goes out at 1 Hz");
    }
    let panel = w.server.connection_panel();
    assert_eq!(panel.players.len(), 2);
    assert!(panel.host_tick_hz > 50.0, "{}", panel.host_tick_hz);
    let (shells, worst) = w.check_shells();
    eprintln!("clean link: {shells} shells checked, worst {worst:.4} m; traffic {:?}", w.server.traffic());
}

#[test]
fn a_conditioned_link_still_converges() {
    // 100 ms each way, 2% burst loss, 10 ms jitter on every send of both ends.
    let link = ConditionerProfile { latency_ms: 100.0, jitter_ms: 10.0, loss: BurstLoss::with_mean(0.02), upstream_kbps: 0 };
    let mut w = two_player_battle(Some(link), Duration::from_millis(2));
    assert_battle(&mut w, 0.02);
    for p in w.server.peers.values() {
        assert!(p.sample.rtt_ms > 150.0 && p.sample.rtt_ms < 320.0, "renet2 measures the conditioned RTT: {}", p.sample.rtt_ms);
        assert!(p.encoder.frames_behind() < 15, "acks keep up: {} behind", p.encoder.frames_behind());
    }
    eprintln!("conditioned: renders {:?}, stats {:?}", w.renders, w.clients.iter().map(|c| c.buffer().stats()).collect::<Vec<_>>());
}

/// The L915 gate profile through the real conditioned transport: 150 ms RTT, 5% burst loss, 30 ms jitter on
/// each end, two clients, 60 s, one conditioner seed. Returns the share of rendered frames extrapolated.
fn gate_run(seed: u64, adaptive: bool) -> f64 {
    let mut w = world_seeded(GameMode::Skirmish, 2, 0, Some(ConditionerProfile::round_trip(150.0, 0.05, 30.0)), seed);
    for c in &mut w.clients { c.buffer.set_tuning(InterpTuning { adaptive, ..InterpTuning::default() }); }
    let dt = Duration::from_millis(2);
    w.start_skirmish(dt);
    w.renders = vec![(0, 0, 0); 2];
    w.run(60.0, dt);
    let (n, ex) = w.renders.iter().fold((0, 0), |a, r| (a.0 + r.0, a.1 + r.1));
    // Nobody is dropped at 150 ms and 5% loss; acks keep up.
    assert!(!w.host_events.iter().any(|e| matches!(e, HostEvent::PlayerLeft { .. })), "seed {seed}");
    for p in w.server.peers.values() { assert!(p.encoder.frames_behind() < 20, "seed {seed}"); }
    let share = ex as f64 / n as f64;
    eprintln!("gate profile seed {seed} adaptive {adaptive}: {:.2}% of {n} frames extrapolated", share * 100.0);
    share
}

#[test]
fn gate_profile_over_the_conditioned_transport() {
    // L915: 1% or fewer of rendered frames. Deterministic per seed with the manual clock. Over seeds 1 to 6
    // when written: fixed delay 0.15% to 1.10% (mean 0.68%), adaptive 0.21% to 1.16% (mean 0.83%). The
    // nightly bot matches measure the gate on every runner OS; this keeps the clock from regressing.
    let fixed: Vec<f64> = [1u64, 3, 5].iter().map(|&s| gate_run(s, false)).collect();
    assert!(fixed.iter().sum::<f64>() / fixed.len() as f64 <= 0.01, "{fixed:?}");
    assert!(fixed.iter().all(|s| *s <= 0.0125), "{fixed:?}");
    assert!(gate_run(1, true) <= 0.0125);
}

#[test]
fn the_local_pair_admits_the_host_and_pause_runs_through_the_session() {
    let d = data();
    let clock = ManualClock::new();
    clock.set(Duration::from_secs(3));
    let c: Arc<dyn Clock> = Arc::new(clock.clone());
    let mut host_store = MemoryStore::default();
    host_store.put(d.packed.hash, d.packed.compressed.clone());
    let mut client_store = MemoryStore::default();
    client_store.put(d.packed.hash, d.packed.compressed.clone());
    let (mut server, mut client) = crate::session::local::local_pair(ServerOptions::new("0.1.0-test", GameMode::Skirmish), lobby_setup(GameMode::Skirmish), client_options(0), c.clone(), pool(), d.catalogue.clone(), Box::new(host_store), Box::new(client_store));
    let dt = Duration::from_millis(4);
    let mut host_events = Vec::new();
    let mut client_events = Vec::new();
    let pump = |server: &mut ServerSession, client: &mut ClientSession, n: usize, he: &mut Vec<HostEvent>, ce: &mut Vec<ClientEvent>| {
        for _ in 0..n {
            clock.advance(dt);
            ce.extend(client.update().unwrap());
            he.extend(server.update().unwrap());
            client.sample();
        }
    };
    // The client's first update sends the hello; the server's first update admits it as the host.
    client_events.extend(client.update().unwrap());
    host_events.extend(server.update().unwrap());
    assert!(host_events.iter().any(|e| matches!(e, HostEvent::PlayerJoined { slot: PlayerSlot(0), client_id: 1000, rejoined: false, .. })), "{host_events:?}");
    assert!(server.lobby().member(PlayerSlot(0)).is_some_and(|m| m.host));
    pump(&mut server, &mut client, 10, &mut host_events, &mut client_events);
    assert!(client_events.iter().any(|e| matches!(e, ClientEvent::Welcomed(w) if w.slot == PlayerSlot(0))));
    // The design is already in the host's store: choosing it needs no upload.
    client.lobby(LobbyRequest::SetBlueprint(d.bp.header.hash));
    client.lobby(LobbyRequest::SetReady(true));
    client.lobby(LobbyRequest::Start);
    pump(&mut server, &mut client, 50, &mut host_events, &mut client_events);
    assert!(host_events.iter().any(|e| matches!(e, HostEvent::StartRequested { .. })));
    let config = sim_systems::modes::skirmish_config(server.lobby().setup(), std::slice::from_ref(&d.bp), &d.campaign, &d.catalogue);
    server.start_sortie(Sim::new(config, deps()).unwrap()).unwrap();
    pump(&mut server, &mut client, 100, &mut host_events, &mut client_events);
    assert_eq!(server.phase(), SessionPhase::Sortie);
    pump(&mut server, &mut client, 250, &mut host_events, &mut client_events);
    let t0 = server.sim().unwrap().tick().0;
    assert!(t0 > 50);
    client.send_command(Command::Pause(true), t0);
    pump(&mut server, &mut client, 100, &mut host_events, &mut client_events);
    assert!(server.sim().unwrap().pacing().paused, "one human may pause a skirmish");
    let p = server.sim().unwrap().tick().0;
    pump(&mut server, &mut client, 250, &mut host_events, &mut client_events);
    assert_eq!(server.sim().unwrap().tick().0, p, "the clock stands while paused");
    assert_eq!(server.backlog, 0, "no backlog builds up while paused");
    // The client's playback stops too, short of the newest frame.
    let f = client.sample().unwrap();
    assert!(f.render_tick <= p as f64 && !f.extrapolating);
    client.send_command(Command::Pause(false), p);
    pump(&mut server, &mut client, 250, &mut host_events, &mut client_events);
    let q = server.sim().unwrap().tick().0;
    assert!(q > p + 40, "unpausing goes through the paused Sim's step: {p} -> {q}");
    // Ending the sortie closes it on the client and returns the replay.
    let (sim, replay) = server.end_sortie().expect("a sortie");
    assert!(sim.is_over());
    let replay = replay.expect("replays are recorded by default");
    assert!(sim_systems::replay::play::ReplayPlayer::open(&replay).is_ok());
    pump(&mut server, &mut client, 20, &mut host_events, &mut client_events);
    assert!(client_events.iter().any(|e| matches!(e, ClientEvent::SortieClosed(_))));
    assert_eq!(server.phase(), SessionPhase::AfterAction);
    server.set_phase(SessionPhase::Lobby).unwrap();
    assert!(server.lobby().setup().players.iter().all(|p| !p.ready), "back in the lobby everyone readies again");
    server.shutdown();
    pump(&mut server, &mut client, 50, &mut host_events, &mut client_events);
    assert!(client_events.iter().any(|e| matches!(e, ClientEvent::Ended(SessionEnd::HostQuit))), "{:?}", client_events.iter().filter(|e| matches!(e, ClientEvent::Ended(_))).collect::<Vec<_>>());
}

#[test]
fn a_dropped_player_rejoins_its_held_slot_and_its_commands_apply() {
    let mut w = world(2, 1, None);
    let dt = Duration::from_millis(2);
    w.start_skirmish(dt);
    w.run(2.0, dt);
    let tick = w.clients[1].buffer().newest_tick().unwrap();
    for k in 0..5 { w.clients[1].send_command(Command::SetSpeed { speed_kn: 3.0 + k as f64 }, tick); }
    w.run(1.0, dt);
    assert_eq!(own_ship(&w.server, 1).orders.speed_kn, Some(7.0));
    // Client 1 leaves mid-sortie: officers keep the hull, the slot is held.
    w.clients[1].leave();
    w.run(1.0, dt);
    assert!(w.host_events.iter().any(|e| matches!(e, HostEvent::PlayerLeft { slot: PlayerSlot(1), client_id: 1001, reason: DropReason::Left, held: true })));
    assert!(own_ship(&w.server, 1).control.officers_in_command);
    // The same install comes back on a fresh connection, with an empty blueprint cache.
    let end = w.spare.pop().unwrap();
    let c: Arc<dyn Clock> = Arc::new(w.clock.clone());
    w.clients[1] = ClientSession::new(client_options(1), Box::new(end), c, data().catalogue.clone(), Box::new(MemoryStore::default()));
    w.client_events[1].clear();
    assert!(w.run_until(10.0, dt, |w| w.client_saw(1, |e| matches!(e, ClientEvent::SortieRunning { .. }))), "the rejoiner loads the running sortie");
    assert!(w.client_saw(1, |e| matches!(e, ClientEvent::Welcomed(wl) if wl.rejoined && wl.slot == PlayerSlot(1) && wl.phase == SessionPhase::Sortie)));
    assert!(w.client_saw(1, |e| matches!(e, ClientEvent::BlueprintReady(h) if *h == data().bp.header.hash)), "its own-side designs came over the bulk lane");
    assert!(w.client_saw(1, |e| matches!(e, ClientEvent::Sim(we) if matches!(we.event, SimEvent::EntityRevealed(_)))), "catch-up reveals");
    assert!(!own_ship(&w.server, 1).control.officers_in_command, "back in command");
    w.run(2.0, dt);
    let f = w.clients[1].sample().unwrap();
    let own = own_ship(&w.server, 1).vessel;
    assert!(f.snapshot.vessels.iter().any(|v| v.entity == own), "the rejoiner draws its hull");
    // Its sequence restarts at 1 and still applies.
    let tick = w.clients[1].buffer().newest_tick().unwrap();
    assert_eq!(w.clients[1].send_command(Command::SetSpeed { speed_kn: 14.0 }, tick), 1);
    w.run(1.0, dt);
    assert_eq!(own_ship(&w.server, 1).orders.speed_kn, Some(14.0));
    assert!(w.check_poses() > 0);
}

#[test]
fn version_mismatch_and_garbage_hellos_are_refused_with_reasons() {
    let clock = ManualClock::new();
    clock.set(Duration::from_secs(2));
    let c: Arc<dyn Clock> = Arc::new(clock.clone());
    let (host, mut ends) = memory_pair(2, c.unix_now(), None);
    let mut server = ServerSession::new(ServerOptions::new("0.1.0-test", GameMode::Skirmish), lobby_setup(GameMode::Skirmish), c.clone(), pool(), data().catalogue.clone(), Box::new(MemoryStore::default()));
    server.add_transport(Box::new(host));
    let mut hello = codec::encode_hello(&Hello { protocol: crate::WIRE_VERSION, game_version: "x".into(), build: BuildKind::GitHub, client_id: 5, name: "n".into() });
    hello[4..8].copy_from_slice(&9_999u32.to_le_bytes());
    let mut events = Vec::new();
    let mut got: Vec<Vec<ServerMsg>> = vec![Vec::new(), Vec::new()];
    let mut sent = [false, false];
    for _ in 0..400 {
        clock.advance(Duration::from_millis(5));
        for (i, e) in ends.iter_mut().enumerate() {
            let _ = e.update(Duration::from_millis(5), c.unix_now());
            if e.state() == crate::transport::ConnState::Connected && !sent[i] {
                sent[i] = true;
                e.send(Channel::Session, if i == 0 { hello.clone() } else { vec![1, 2, 3] });
            }
            while let Some(b) = e.receive(Channel::Session) { got[i].push(codec::decode::<ServerMsg>(&b).unwrap()); }
            let _ = e.flush();
        }
        events.extend(server.update().unwrap());
    }
    assert_eq!(got[0], vec![ServerMsg::Refused(JoinRefusal::ProtocolMismatch { host: crate::WIRE_VERSION, client: 9_999 })]);
    assert_eq!(got[1], vec![ServerMsg::Refused(JoinRefusal::BadHello)]);
    assert!(events.iter().any(|e| matches!(e, HostEvent::JoinRefused { reason: JoinRefusal::ProtocolMismatch { .. }, .. })));
    assert!(ends.iter().all(|e| matches!(e.state(), crate::transport::ConnState::Disconnected(_))), "refused connections are closed after the reason");
    assert!(server.peers.is_empty());
    assert!(server.lobby().members().is_empty());
}

#[test]
fn kicks_and_bans_reach_the_player_and_free_the_slot() {
    let mut w = world(3, 1, None);
    let dt = Duration::from_millis(4);
    assert!(w.run_until(10.0, dt, |w| (0..3).all(|i| w.welcomed(i))));
    w.server.kick(PlayerSlot(0), false);
    w.server.kick(PlayerSlot(2), true);
    w.run(2.0, dt);
    assert!(w.server.lobby().member(PlayerSlot(0)).is_some(), "the host cannot be kicked");
    assert!(w.client_saw(2, |e| matches!(e, ClientEvent::Ended(SessionEnd::Dropped(DropReason::Banned)))));
    assert!(w.host_events.iter().any(|e| matches!(e, HostEvent::PlayerLeft { slot: PlayerSlot(2), reason: DropReason::Banned, held: false, .. })));
    assert!(w.server.lobby().member(PlayerSlot(2)).is_none());
    // The banned install cannot come back.
    let end = w.spare.pop().unwrap();
    let c: Arc<dyn Clock> = Arc::new(w.clock.clone());
    w.clients[2] = ClientSession::new(client_options(2), Box::new(end), c, data().catalogue.clone(), Box::new(MemoryStore::default()));
    w.client_events[2].clear();
    assert!(w.run_until(5.0, dt, |w| w.client_saw(2, |e| matches!(e, ClientEvent::Refused(JoinRefusal::Banned)))));
    assert!(w.run_until(5.0, dt, |w| w.client_saw(2, |e| matches!(e, ClientEvent::Ended(SessionEnd::Refused)))));
    let panel = w.server.connection_panel();
    assert_eq!(panel.players.iter().map(|p| p.slot).collect::<Vec<_>>(), vec![PlayerSlot(0), PlayerSlot(1)]);
}

/// L939 on a live stream: the bytes sent to a client decode, through the client path, to exactly the poses
/// the client itself decoded, twice over the same way, with every shell landing within 0.5 m of the server.
#[test]
fn the_wire_capture_decodes_like_the_client() {
    let mut w = world(2, 0, Some(ConditionerProfile { latency_ms: 40.0, jitter_ms: 5.0, loss: BurstLoss::with_mean(0.02), upstream_kbps: 0 }));
    let dt = Duration::from_millis(2);
    w.start_skirmish(dt);
    w.server.capture_stream(PlayerSlot(0), true);
    let gun = own_ship(&w.server, 0).design.mounts.iter().find(|m| m.weapon == content::graph::WeaponClass::Gun).map(|m| m.slot);
    let tick = w.clients[0].buffer().newest_tick().unwrap_or(0);
    if let Some(slot) = gun { w.clients[0].send_command(Command::TakeControl { role: ControlRole::Mount(slot), on: true }, tick); }
    // What client 0 decoded, gathered as it goes (its rings keep only the last few samples).
    let mut seen: BTreeMap<(u64, u64), Pose> = BTreeMap::new();
    let gather = |w: &World, seen: &mut BTreeMap<(u64, u64), Pose>| {
        for (e, ring) in &w.clients[0].buffer().vessels { for (t, v) in &ring.samples { seen.insert((*t, e.to_u64()), v.kin.pose); } }
    };
    for k in 0..6 {
        for _ in 0..15 { w.run(0.1, dt); gather(&w, &mut seen); }
        if let Some(slot) = gun {
            let tick = w.clients[0].buffer().newest_tick().unwrap_or(0);
            w.clients[0].send_command(Command::Fire { role: ControlRole::Mount(slot), shot: ShotDesignation::Direction { dir_local: DVec3::new(1.0, 0.03, 0.05 * k as f64) } }, tick);
        }
    }
    for _ in 0..120 { w.run(0.1, dt); gather(&w, &mut seen); }
    let capture = w.server.take_capture(PlayerSlot(0)).expect("capturing");
    assert!(capture.frames.len() > 400, "{} captured", capture.frames.len());
    let a = crate::replay::decode_capture(&capture, &data().catalogue).expect("decodes");
    let b = crate::replay::decode_capture(&capture, &data().catalogue).expect("decodes again");
    assert!(crate::replay::compare_decodes(&a, &b, 0.5).is_empty());
    // The capture holds exactly the frames the client decoded: every pose the client still holds a sample of
    // is in it, bit for bit.
    let (first, last) = (a.poses.first().map_or(u64::MAX, |p| p.0), a.poses.last().map_or(0, |p| p.0));
    let mut matched = 0;
    for ((tick, e), pose) in &seen {
        // Frames still waiting for their ack are not in the capture yet.
        if *tick < first || *tick > last { continue; }
        let q = a.poses.iter().find(|p| p.0 == *tick && p.1 == *e).unwrap_or_else(|| panic!("{e:#x} at {tick} missing from the capture"));
        assert_eq!(crate::schema::quant::dequantize_pose(&q.2), *pose, "{e:#x} at {tick}");
        matched += 1;
    }
    assert_eq!(matched, a.poses.iter().filter(|p| seen.contains_key(&(p.0, p.1))).count());
    assert!(matched > 500, "{matched} poses matched");
    if gun.is_some() {
        assert!(!a.shell_impacts.is_empty(), "the gun fired and its shells landed");
        for (id, c, s) in &a.shell_impacts { assert!((DVec3::from_array(*c) - DVec3::from_array(*s)).length() <= 0.5, "shell {id}"); }
    }
    // Capturing stops on request.
    w.server.capture_stream(PlayerSlot(0), false);
    assert!(w.server.take_capture(PlayerSlot(0)).is_none());
}

#[test]
fn a_sandbox_joiner_enters_the_running_sim_once_its_design_is_accepted_and_leaves_cleanly() {
    let mut w = world_in(GameMode::Sandbox, 1, 1, None);
    let dt = Duration::from_millis(4);
    assert!(w.run_until(5.0, dt, |w| w.welcomed(0)));
    let d = data();
    w.clients[0].upload_blueprint(d.packed.clone());
    w.clients[0].lobby(LobbyRequest::SetBlueprint(d.bp.header.hash));
    w.clients[0].lobby(LobbyRequest::SetReady(true));
    assert!(w.run_until(10.0, dt, |w| w.server.lobby().setup().players.iter().all(|p| p.ready) && w.host_events.iter().any(|e| matches!(e, HostEvent::BlueprintAccepted { .. }))));
    let config = sim_systems::modes::sandbox_config(w.server.lobby().setup(), std::slice::from_ref(&d.bp), &d.campaign);
    w.server.start_sortie(Sim::new(config, deps()).unwrap()).unwrap();
    assert!(w.run_until(5.0, dt, |w| w.server.phase() == SessionPhase::Sortie));
    w.run(2.0, dt);
    // A second install joins while the sortie runs: admitted, but not in the Sim until its design is in.
    let end = w.spare.pop().unwrap();
    let c: Arc<dyn Clock> = Arc::new(w.clock.clone());
    w.clients.push(ClientSession::new(client_options(7), Box::new(end), c, d.catalogue.clone(), Box::new(MemoryStore::default())));
    w.client_events.push(Vec::new());
    w.renders.push((0, 0, 0));
    assert!(w.run_until(5.0, dt, |w| w.welcomed(1)));
    assert!(w.client_saw(1, |e| matches!(e, ClientEvent::Welcomed(wl) if wl.phase == SessionPhase::Sortie && wl.slot == PlayerSlot(1))));
    w.run(1.0, dt);
    assert_eq!(w.server.sim().unwrap().players().len(), 1);
    assert!(!w.client_saw(1, |e| matches!(e, ClientEvent::SortieLoading(_))));
    w.clients[1].upload_blueprint(d.packed.clone());
    w.clients[1].lobby(LobbyRequest::SetBlueprint(d.bp.header.hash));
    assert!(w.run_until(10.0, dt, |w| w.client_saw(1, |e| matches!(e, ClientEvent::SortieRunning { .. }))), "the joiner loads into the running sortie");
    let sim = w.server.sim().unwrap();
    assert_eq!(sim.players().len(), 2);
    let hull = sim.ships().of_player(PlayerSlot(1)).expect("the joiner got a hull at a spawn marker").vessel;
    w.run(2.0, dt);
    let f = w.clients[1].sample().unwrap();
    assert!(f.snapshot.vessels.iter().any(|v| v.entity == hull), "the joiner draws its own hull");
    assert!(f.snapshot.ship.is_some(), "and gets its ship status");
    let tick = w.clients[1].buffer().newest_tick().unwrap();
    w.clients[1].send_command(Command::SetSpeed { speed_kn: 9.0 }, tick);
    w.run(1.0, dt);
    assert_eq!(w.server.sim().unwrap().ships().of_player(PlayerSlot(1)).unwrap().orders.speed_kn, Some(9.0));
    // A sandbox leave removes the hull and frees the slot.
    w.clients[1].leave();
    w.run(1.0, dt);
    assert!(w.host_events.iter().any(|e| matches!(e, HostEvent::PlayerLeft { slot: PlayerSlot(1), reason: DropReason::Left, held: false, .. })));
    assert_eq!(w.server.sim().unwrap().players().len(), 1);
    assert!(w.server.lobby().member(PlayerSlot(1)).is_none());
    assert!(w.server.lobby().held_slots(w.now()).is_empty());
}

#[test]
fn a_link_too_slow_to_play_is_warned_then_dropped_and_its_slot_held() {
    // 300 ms each way: 600 ms RTT, over the 500 ms rule (L1035). The host's own client is exempt.
    let mut w = world(2, 0, Some(ConditionerProfile { latency_ms: 300.0, jitter_ms: 0.0, loss: BurstLoss::NONE, upstream_kbps: 0 }));
    let dt = Duration::from_millis(4);
    w.start_skirmish(dt);
    let hull = own_ship(&w.server, 1).vessel;
    let t0 = w.now();
    assert!(w.run_until(25.0, dt, |w| w.host_events.iter().any(|e| matches!(e, HostEvent::PlayerLeft { slot: PlayerSlot(1), .. }))), "dropped within 20 s of the start");
    // The 15 s hold runs from when the condition first held (the lobby), so the warning came 10 s earlier.
    let warned_at = w.server.log.entries.iter().find(|e| matches!(e.kind, LogKind::Warning { client_id: 1001, .. })).map(|e| e.at_s).unwrap();
    let dropped_at = w.server.log.entries.iter().find(|e| matches!(e.kind, LogKind::Dropped { client_id: 1001, .. })).map(|e| e.at_s).unwrap();
    assert!((dropped_at - warned_at - 10.0).abs() < 0.5, "warning after 5 s, drop after 15 s: {warned_at} {dropped_at}");
    let _ = t0;
    assert!(w.host_events.iter().any(|e| matches!(e, HostEvent::PlayerLeft { slot: PlayerSlot(1), reason: DropReason::HighPing, held: true, .. })));
    assert!(w.server.log.entries.iter().any(|e| matches!(e.kind, LogKind::Warning { client_id: 1001, warning: crate::msg::DropWarning::HighPing, .. })), "warned first");
    assert!(w.server.log.entries.iter().any(|e| matches!(e.kind, LogKind::Dropped { client_id: 1001, reason: DropReason::HighPing, .. })));
    assert!(w.server.sim().unwrap().ships().get(hull).unwrap().control.officers_in_command, "officers keep the hull");
    assert!(w.run_until(3.0, dt, |w| w.client_saw(1, |e| matches!(e, ClientEvent::Ended(SessionEnd::Dropped(DropReason::HighPing))))), "the player is told why");
    assert!(!w.host_events.iter().any(|e| matches!(e, HostEvent::PlayerLeft { slot: PlayerSlot(0), .. })), "the host is never dropped");
    let panel = w.server.connection_panel();
    assert!(panel.players.iter().any(|p| p.slot == PlayerSlot(1) && p.held_s.is_some_and(|s| s > 280)));
}

fn packed_stock(path: &str) -> (Blueprint, PackedBlueprint) {
    let mut bp = content::campaign::load::stock_blueprint(path).expect("stock design");
    content::blueprint::io::canonicalize_blueprint(&mut bp);
    bp.header.hash = content::blueprint::io::compute_hash(&bp);
    let packed = content::share::pack(&Shareable::Ship(Box::new(bp.clone())));
    (bp, packed)
}

#[test]
fn a_pvp_opponent_never_gets_a_full_design_in_the_lobby_or_the_match() {
    let mut w = world_in(GameMode::PvpElimination, 2, 0, None);
    let dt = Duration::from_millis(4);
    assert!(w.run_until(5.0, dt, |w| w.welcomed(0) && w.welcomed(1)));
    let d = data();
    let (patrol, patrol_packed) = packed_stock("starters/patrol15.bp.ron");
    let slot0 = (0..2).find(|&i| matches!(w.clients[i].state(), ClientState::Joined(wl) if wl.slot == PlayerSlot(0))).unwrap();
    let (a, b) = (slot0, 1 - slot0);
    w.clients[a].upload_blueprint(patrol_packed.clone());
    w.clients[a].lobby(LobbyRequest::SetBlueprint(patrol.header.hash));
    w.clients[b].upload_blueprint(d.packed.clone());
    w.clients[b].lobby(LobbyRequest::SetBlueprint(d.bp.header.hash));
    w.clients[b].lobby(LobbyRequest::SetTeam(1));
    assert!(w.run_until(10.0, dt, |w| w.host_events.iter().filter(|e| matches!(e, HostEvent::BlueprintAccepted { .. })).count() >= 2));
    w.run(1.0, dt);
    // The opponent's lobby view names the exterior form only.
    let ext = w.server.exterior_of[&patrol.header.hash];
    assert_ne!(ext, patrol.header.hash);
    assert!(w.client_saw(b, |e| matches!(e, ClientEvent::Lobby(v) if v.members.iter().any(|m| m.slot == PlayerSlot(0) && m.blueprint == Some(ext)))));
    // Asking for the full design is refused; the exterior form is served.
    w.clients[b].request_blueprint(patrol.header.hash);
    w.clients[b].request_blueprint(ext);
    w.run(2.0, dt);
    assert!(w.client_saw(b, |e| matches!(e, ClientEvent::BlueprintRefused(h, BlueprintRefusal::NotEntitled) if *h == patrol.header.hash)));
    assert!(w.client_saw(b, |e| matches!(e, ClientEvent::BlueprintReady(h) if *h == ext)));
    assert!(!w.clients[b].store.contains(&patrol.header.hash));
    // In the match too.
    for c in &mut w.clients { c.lobby(LobbyRequest::SetReady(true)); }
    w.run(1.0, dt);
    let config = sim_systems::modes::elimination_config(w.server.lobby().setup(), &[d.bp.clone(), patrol.clone()], &d.campaign);
    w.server.start_sortie(Sim::new(config, deps()).unwrap()).unwrap();
    assert!(w.run_until(10.0, dt, |w| w.server.phase() == SessionPhase::Sortie));
    w.client_events[b].clear();
    w.clients[b].request_blueprint(patrol.header.hash);
    w.run(2.0, dt);
    assert!(w.client_saw(b, |e| matches!(e, ClientEvent::BlueprintRefused(h, BlueprintRefusal::NotEntitled) if *h == patrol.header.hash)));
    assert!(!w.clients[b].store.contains(&patrol.header.hash));
    // Every design a reveal named to the opponent is the exterior form.
    for e in &w.client_events[b] {
        if let ClientEvent::Sim(we) = e { if let SimEvent::EntityRevealed(i) = &we.event { assert_ne!(i.blueprint, patrol.header.hash); } }
    }
}

/// A single-player skirmish over `local_pair`, running.
fn local_skirmish(options: ServerOptions) -> (ManualClock, ServerSession, ClientSession) {
    let d = data();
    let clock = ManualClock::new();
    clock.set(Duration::from_secs(3));
    let c: Arc<dyn Clock> = Arc::new(clock.clone());
    let mut host_store = MemoryStore::default();
    host_store.put(d.packed.hash, d.packed.compressed.clone());
    let mut client_store = MemoryStore::default();
    client_store.put(d.packed.hash, d.packed.compressed.clone());
    let (mut server, mut client) = crate::session::local::local_pair(options, lobby_setup(GameMode::Skirmish), client_options(0), c, pool(), d.catalogue.clone(), Box::new(host_store), Box::new(client_store));
    let step = |server: &mut ServerSession, client: &mut ClientSession| { clock.advance(Duration::from_millis(4)); client.update().unwrap(); server.update().unwrap() };
    for _ in 0..10 { step(&mut server, &mut client); }
    client.lobby(LobbyRequest::SetBlueprint(d.bp.header.hash));
    client.lobby(LobbyRequest::SetReady(true));
    for _ in 0..50 { step(&mut server, &mut client); }
    let config = sim_systems::modes::skirmish_config(server.lobby().setup(), std::slice::from_ref(&d.bp), &d.campaign, &d.catalogue);
    server.start_sortie(Sim::new(config, deps()).unwrap()).unwrap();
    for _ in 0..100 { step(&mut server, &mut client); }
    assert_eq!(server.phase(), SessionPhase::Sortie);
    (clock, server, client)
}

#[test]
fn synthetic_tick_cost_slows_the_host_and_builds_backlog() {
    // A host whose ticks cost 25 ms runs at most 40 ticks a second (L1027's injected cost).
    let mut options = ServerOptions::new("0.1.0-test", GameMode::Skirmish);
    options.synthetic_tick_ms = 25.0;
    let (clock, mut server, mut client) = local_skirmish(options);
    let t0 = server.sim().unwrap().tick().0;
    let mut max_backlog = 0;
    for _ in 0..1250 {
        clock.advance(Duration::from_millis(4));
        client.update().unwrap();
        server.update().unwrap();
        max_backlog = max_backlog.max(server.backlog);
    }
    let ticks = server.sim().unwrap().tick().0 - t0;
    assert!((180..=210).contains(&ticks), "5 s at 25 ms a tick: {ticks} ticks");
    assert!(max_backlog > 3, "the backlog shows the host falling behind: {max_backlog}");
    let panel = server.connection_panel();
    assert!((panel.host_tick_p95_ms - 25.0).abs() < 1.0, "{}", panel.host_tick_p95_ms);
    assert!(panel.host_tick_hz <= 41.0, "{}", panel.host_tick_hz);
}

#[test]
fn time_compression_steps_the_sim_faster_through_the_session() {
    let (clock, mut server, mut client) = local_skirmish(ServerOptions::new("0.1.0-test", GameMode::Skirmish));
    let tick = server.sim().unwrap().tick().0;
    client.send_command(Command::RequestTimeRate(sim_systems::protocol::command::TimeRate::X4), tick);
    let run = |n: usize, server: &mut ServerSession, client: &mut ClientSession| {
        for _ in 0..n {
            clock.advance(Duration::from_millis(4));
            client.update().unwrap();
            server.update().unwrap();
            client.sample();
        }
    };
    run(100, &mut server, &mut client);
    let pacing = server.sim().unwrap().pacing();
    assert_eq!(pacing.effective, sim_systems::protocol::command::TimeRate::X4, "compression allowed: {:?}", pacing.blocked);
    let t0 = server.sim().unwrap().tick().0;
    run(500, &mut server, &mut client);
    let ticks = server.sim().unwrap().tick().0 - t0;
    assert!((470..=490).contains(&ticks), "2 s at 4x: {ticks} ticks");
    // The client's playback follows the faster clock without running out of frames.
    let before = client.buffer().stats().extrapolated;
    let r0 = client.sample().unwrap().render_tick;
    run(250, &mut server, &mut client);
    let r1 = client.sample().unwrap().render_tick;
    assert!((r1 - r0 - 240.0).abs() < 24.0, "render advanced {} ticks in 1 s", r1 - r0);
    assert_eq!(client.buffer().stats().extrapolated, before);
}
