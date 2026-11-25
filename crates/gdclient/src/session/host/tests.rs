use super::*;
use crate::seams::{ConnectionPanel, NetError};
use crate::session::testkit;
use sim_systems::modes::LobbySetup;
use std::sync::Mutex;

/// What a fake port saw, shared with the test.
#[derive(Default)]
struct Seen { updates: usize, started: usize, ended: usize, shut: usize, blueprints: usize, panic_on_update: bool, fail_on_update: bool }

struct FakePort { seen: Arc<Mutex<Seen>>, sim: Option<Sim>, over_after: Option<usize> }

impl HostPort for FakePort {
    fn update(&mut self, events: &mut Vec<HostEvent>) -> Result<(), NetError> {
        let (panic, fail, n) = { let mut s = self.seen.lock().unwrap(); s.updates += 1; (s.panic_on_update, s.fail_on_update, s.updates) };
        if panic && self.sim.is_some() { panic!("a simulation worker failed"); }
        if fail { return Err(NetError::NotConnected); }
        if let (Some(sim), Some(after)) = (self.sim.as_mut(), self.over_after) {
            if n >= after && !sim.is_over() { sim.finish(Outcome::Victory); events.push(HostEvent::SortieOver); }
        }
        Ok(())
    }
    fn phase(&self) -> SessionPhase { if self.sim.is_some() { SessionPhase::Sortie } else { SessionPhase::Lobby } }
    fn set_phase(&mut self, _phase: SessionPhase) -> Result<(), NetError> { Ok(()) }
    fn start_sortie(&mut self, sim: Sim) -> Result<(), NetError> { self.seen.lock().unwrap().started += 1; self.sim = Some(sim); Ok(()) }
    fn sim(&self) -> Option<&Sim> { self.sim.as_ref() }
    fn sim_mut(&mut self) -> Option<&mut Sim> { self.sim.as_mut() }
    fn end_sortie(&mut self) -> Option<(Sim, Option<Vec<u8>>)> { let sim = self.sim.take()?; self.seen.lock().unwrap().ended += 1; Some((sim, Some(vec![1, 2, 3]))) }
    fn send(&mut self, _to: Option<PlayerSlot>, _msg: ServerMsg) {}
    fn send_bulk(&mut self, _to: Option<PlayerSlot>, _kind: TransferKind, _bytes: Vec<u8>) {}
    fn lobby_setup(&self) -> LobbySetup {
        LobbySetup { mode: content::enums::GameMode::Sandbox, ruleset: Default::default(), seed: 1, players: Vec::new(), skirmish: None, weather: Default::default(), sea_state: Default::default(), time_of_day: Default::default(), map: None, at_1_0: false }
    }
    fn blueprint(&mut self, _hash: &content::ids::ContentHash) -> Option<Vec<u8>> { None }
    fn put_blueprint(&mut self, _packed: PackedBlueprint) { self.seen.lock().unwrap().blueprints += 1; }
    fn kick(&mut self, _slot: PlayerSlot, _ban: bool) {}
    fn set_upload_kbps(&mut self, _kbps: u32) {}
    fn set_udp(&mut self, _options: Option<UdpHostOptions>, _reason: DropReason) -> Result<Vec<std::net::SocketAddr>, NetError> { Err(NetError::WrongPhase) }
    fn connection_panel(&self) -> ConnectionPanel { ConnectionPanel { host_tick_hz: 60.0, host_tick_p95_ms: 1.0, ..Default::default() } }
    fn drain_log(&mut self) -> String { String::new() }
    fn shutdown(&mut self) { self.seen.lock().unwrap().shut += 1; }
}

fn spawn(over_after: Option<usize>) -> (HostHandle, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let port = FakePort { seen: seen.clone(), sim: None, over_after };
    let handle = HostHandle::spawn(Box::new(port), testkit::pool(), Box::new(testkit::deps));
    (handle, seen)
}

/// Poll until `f` accepts a note or `limit` passes; returns every note seen.
fn wait_for(h: &HostHandle, limit: Duration, f: impl Fn(&HostNote) -> bool) -> Vec<HostNote> {
    let end = Instant::now() + limit;
    let mut all = Vec::new();
    loop {
        let mut notes = Vec::new();
        h.poll(&mut notes);
        let hit = notes.iter().any(&f);
        all.extend(notes);
        if hit || Instant::now() >= end { return all; }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn start(h: &HostHandle, options: SortieOptions) {
    h.send(HostControl::StartSortie { start: HostStart::New { config: Box::new(testkit::basin_config()) }, options });
}

#[test]
fn percentiles_are_nearest_rank_over_the_window() {
    let mut t = TickTimes::new(Duration::from_secs(2));
    assert_eq!(t.percentile(0.95), 0.0);
    for i in 1..=100 { t.record(Duration::from_millis(i * 10), i as f32); }
    assert_eq!(t.percentile(0.5), 50.0);
    assert_eq!(t.percentile(0.95), 95.0);
    assert_eq!(t.percentile(1.0), 100.0);
    assert_eq!(t.count_since(Duration::from_millis(500)), 51);
    // Samples older than the window before the newest are dropped.
    t.record(Duration::from_millis(4_000), 1.0);
    assert_eq!(t.len(), 1);
    t.record(Duration::from_millis(4_001), f32::NAN);
    assert!(t.percentile(0.95).is_finite());
}

#[test]
fn the_driver_builds_runs_ends_and_shuts_down() {
    let (mut h, seen) = spawn(Some(40));
    start(&h, SortieOptions { checkpoints: false, fixed_step: false });
    let notes = wait_for(&h, Duration::from_secs(20), |n| matches!(n, HostNote::SortieEnded { .. }));
    assert!(notes.iter().any(|n| matches!(n, HostNote::SortieLoaded { millis } if *millis > 0.0)), "loaded");
    let ended = notes.iter().find_map(|n| if let HostNote::SortieEnded { result, replay } = n { Some((result.outcome, replay.clone())) } else { None });
    assert_eq!(ended, Some((Outcome::Victory, Some(vec![1, 2, 3]))), "the result and the replay come back");
    assert!(!notes.iter().any(|n| matches!(n, HostNote::Session(HostEvent::SortieOver))), "the clock thread handles SortieOver itself");
    {
        let s = seen.lock().unwrap();
        assert_eq!((s.started, s.ended), (1, 1));
        assert_eq!(s.blueprints, 1, "the config's inline design goes into the store");
        assert!(s.updates >= 40);
    }
    let t0 = Instant::now();
    h.shutdown();
    assert!(t0.elapsed() < SHUTDOWN_WAIT + Duration::from_millis(200));
    assert!(h.thread.is_none());
    assert_eq!(seen.lock().unwrap().shut, 1);
    h.shutdown();
}

#[test]
fn stop_and_checkpoint_on_request() {
    let (mut h, seen) = spawn(None);
    start(&h, SortieOptions { checkpoints: true, fixed_step: false });
    let notes = wait_for(&h, Duration::from_secs(20), |n| matches!(n, HostNote::SortieLoaded { .. }));
    assert!(notes.iter().any(|n| matches!(n, HostNote::SortieLoaded { .. })));
    h.send(HostControl::CheckpointNow);
    h.send(HostControl::StopSortie(Outcome::Aborted));
    let notes = wait_for(&h, Duration::from_secs(5), |n| matches!(n, HostNote::SortieEnded { .. }));
    let checkpoint = notes.iter().find_map(|n| if let HostNote::Checkpoint(c) = n { Some(c) } else { None }).expect("a checkpoint of the moment");
    assert!(Sim::from_checkpoint(checkpoint, testkit::deps()).is_ok(), "the checkpoint restores");
    assert!(notes.iter().any(|n| matches!(n, HostNote::SortieEnded { result, .. } if result.outcome == Outcome::Aborted)));
    // Stats arrive at 1 Hz.
    let notes = wait_for(&h, Duration::from_millis(1_500), |n| matches!(n, HostNote::Stats(_)));
    assert!(notes.iter().any(|n| matches!(n, HostNote::Stats(s) if s.tick_hz == 60.0)));
    h.shutdown();
    assert_eq!(seen.lock().unwrap().ended, 1);
}

#[test]
fn fixed_step_runs_one_pass_per_step() {
    let (mut h, seen) = spawn(None);
    start(&h, SortieOptions { checkpoints: false, fixed_step: true });
    let _ = wait_for(&h, Duration::from_secs(20), |n| matches!(n, HostNote::SortieLoaded { .. }));
    std::thread::sleep(Duration::from_millis(100));
    let before = seen.lock().unwrap().updates;
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(seen.lock().unwrap().updates, before, "no deadline drives a fixed-step sortie");
    for _ in 0..5 { h.send(HostControl::StepOnce); }
    let end = Instant::now() + Duration::from_secs(2);
    while seen.lock().unwrap().updates < before + 5 && Instant::now() < end { std::thread::sleep(Duration::from_millis(2)); }
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(seen.lock().unwrap().updates, before + 5);
    h.shutdown();
}

#[test]
fn a_panic_is_contained_and_reported_as_host_loss() {
    let (mut h, seen) = spawn(None);
    start(&h, SortieOptions { checkpoints: false, fixed_step: false });
    let _ = wait_for(&h, Duration::from_secs(20), |n| matches!(n, HostNote::SortieLoaded { .. }));
    seen.lock().unwrap().panic_on_update = true;
    let notes = wait_for(&h, Duration::from_secs(2), |n| matches!(n, HostNote::Panicked(_)));
    assert!(notes.iter().any(|n| matches!(n, HostNote::Panicked(text) if text.contains("worker failed"))));
    let end = Instant::now() + Duration::from_secs(2);
    while h.thread.as_ref().is_some_and(|t| !t.is_finished()) && Instant::now() < end { std::thread::sleep(Duration::from_millis(2)); }
    assert!(h.thread.as_ref().is_some_and(|t| t.is_finished()), "the clock thread stops after a panic");
    h.shutdown();
}

#[test]
fn a_failed_update_is_reported_once_and_the_port_left_alone() {
    let (mut h, seen) = spawn(None);
    seen.lock().unwrap().fail_on_update = true;
    let notes = wait_for(&h, Duration::from_millis(300), |_| false);
    assert_eq!(notes.iter().filter(|n| matches!(n, HostNote::Failed(_))).count(), 1);
    let updates = seen.lock().unwrap().updates;
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(seen.lock().unwrap().updates, updates, "not pumped again after the failure");
    start(&h, SortieOptions { checkpoints: false, fixed_step: false });
    let notes = wait_for(&h, Duration::from_secs(2), |n| matches!(n, HostNote::SortieFailed(_)));
    assert!(notes.iter().any(|n| matches!(n, HostNote::SortieFailed(_))));
    h.shutdown();
}

#[test]
fn a_bad_config_fails_the_start_without_harm() {
    let (mut h, _seen) = spawn(None);
    let mut config = testkit::basin_config();
    config.protocol = config.protocol.wrapping_add(7);
    h.send(HostControl::StartSortie { start: HostStart::New { config: Box::new(config) }, options: SortieOptions { checkpoints: false, fixed_step: false } });
    let notes = wait_for(&h, Duration::from_secs(10), |n| matches!(n, HostNote::SortieFailed(_)));
    assert!(notes.iter().any(|n| matches!(n, HostNote::SortieFailed(_))));
    h.send(HostControl::SetUdp { options: None, reason: DropReason::Left });
    let notes = wait_for(&h, Duration::from_secs(1), |n| matches!(n, HostNote::UdpBound(_)));
    assert!(notes.iter().any(|n| matches!(n, HostNote::UdpBound(Err(_)))));
    h.shutdown();
}

#[test]
fn the_pool_has_three_named_workers() {
    let pool = sim_pool();
    assert_eq!(pool.current_num_threads(), 3);
    let name = pool.install(|| std::thread::current().name().map(str::to_string));
    assert!(name.is_some_and(|n| n.starts_with("bp-sim-")));
}
