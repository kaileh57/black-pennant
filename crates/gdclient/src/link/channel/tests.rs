use super::*;
use crate::session::testkit;
use net::clock::ManualClock;
use sim_systems::protocol::{Outcome, TimeRate};

fn ends(clock: &ManualClock) -> (ChannelHost, ChannelLink) {
    // Callers that step a Sim hold `testkit::serial()`.
    pair_parts(PlayerSlot(0), SideId(0), content::enums::GameMode::Splash, Arc::new(clock.clone()), testkit::pool(), testkit::catalogue())
}

fn tick(clock: &ManualClock, n: u64) { clock.advance(Duration::from_nanos(TICK_NS * n)); }

fn session_events(link: &ChannelLink) -> Vec<ClientEvent> { std::iter::from_fn(|| link.from_host.try_recv().ok()).collect() }
fn sim_events(link: &ChannelLink) -> Vec<WireEvent> { std::iter::from_fn(|| link.events.try_recv().ok()).collect() }

#[test]
fn triple_buffer_hands_over_only_the_newest_value() {
    let (mut w, mut r) = triple_buffer::<u32>();
    assert_eq!(r.take_latest(), None);
    w.publish(1);
    w.publish(2);
    w.publish(3);
    assert_eq!(r.take_latest(), Some(3));
    assert_eq!(r.take_latest(), None, "a value is taken once");
    w.publish(4);
    assert_eq!(r.take_latest(), Some(4));
    for i in 5..100 { w.publish(i); if i % 7 == 0 { assert_eq!(r.take_latest(), Some(i)); } }
    assert_eq!(r.take_latest(), Some(99));
}

#[test]
fn triple_buffer_never_goes_back_across_threads() {
    let (mut w, mut r) = triple_buffer::<u64>();
    const N: u64 = 200_000;
    let writer = std::thread::spawn(move || { for i in 1..=N { w.publish(i); } });
    let mut last = 0;
    loop {
        let finished = writer.is_finished();
        if let Some(v) = r.take_latest() {
            assert!(v > last, "values only move forward: {v} after {last}");
            last = v;
        }
        if last == N { break; }
        // Once the writer is done its newest value must be waiting.
        if finished { assert_eq!(r.take_latest().unwrap_or(last), N); break; }
    }
    writer.join().expect("the writer ends");
}

#[test]
fn shells_fly_with_the_simulations_drag_constants() {
    let cat = testkit::catalogue();
    for band in CalibreBand::ALL {
        let k = band_drag_k(&cat.bands, band);
        assert!(k.is_finite() && k >= 0.0, "{band:?}: {k}");
        if let Some(row) = cat.bands.iter().find(|b| b.band == band) { if row.drag_k.is_finite() && row.drag_k > 0.0 { assert_eq!(k, row.drag_k); } }
    }
    // A catalogue without rows falls back to the fit of the spec's starting row, as sim-core does.
    let fit = band_drag_k(&[], CalibreBand::ALL[0]);
    assert_eq!(fit, sim_core::projectiles::fit_drag_k(&BandDef::spec_default(CalibreBand::ALL[0])));
}

#[test]
fn the_host_end_runs_the_driver_contract_at_the_real_tick_rate() {
    let _serial = testkit::serial();
    let clock = ManualClock::new();
    let (mut host, link) = ends(&clock);
    let mut out = Vec::new();
    // Before a sortie the pass only serves the panel.
    host.update(&mut out).expect("a pass");
    assert!(matches!(session_events(&link).as_slice(), [ClientEvent::Connection(_)]));
    host.start_sortie(testkit::basin()).expect("the sortie starts");
    assert_eq!(host.phase(), SessionPhase::Sortie);
    let started = session_events(&link);
    match started.as_slice() {
        [ClientEvent::SortieLoading(intro), ClientEvent::SortieRunning { tick: 0 }] => {
            assert_eq!(intro.viewer, Viewer::Player(PlayerSlot(0)));
            assert_eq!(intro.side, SideId(0));
            assert_eq!(intro.mode, content::enums::GameMode::Splash);
            assert_eq!(intro.own_side_blueprints.len(), 1, "the player's design is the side's only inline design");
        }
        other => panic!("unexpected start: {other:?}"),
    }
    let mut published = 0;
    let mut reader = link.snapshots;
    let mut last_tick = 0;
    for _ in 0..60 {
        tick(&clock, 1);
        host.update(&mut out).expect("a pass");
        if let Some(s) = reader.take_latest() {
            published += 1;
            assert!(s.tick >= last_tick);
            last_tick = s.tick;
            assert!(s.ship.is_some(), "the player's own view");
        }
    }
    assert_eq!(host.sim().expect("sim").tick().0, 60, "one simulation step per real tick at 1x");
    assert_eq!(published, 30, "snapshots at 30 Hz of real time");
    let events: Vec<WireEvent> = std::iter::from_fn(|| link.events.try_recv().ok()).collect();
    assert!(events.iter().any(|e| matches!(e.event, SimEvent::SortieStarted { .. })), "events reach the player");
    assert!(out.iter().all(|e| !matches!(e, HostEvent::SortieOver)));
    let panel = host.connection_panel();
    assert!(panel.host_tick_hz > 0.0 && panel.host_tick_p95_ms > 0.0, "the panel carries the measured tick: {panel:?}");
}

#[test]
fn a_late_pass_runs_four_ticks_and_drops_the_rest() {
    let _serial = testkit::serial();
    let clock = ManualClock::new();
    let (mut host, _link) = ends(&clock);
    let mut out = Vec::new();
    host.start_sortie(testkit::basin()).expect("starts");
    tick(&clock, 10);
    host.update(&mut out).expect("a pass");
    assert_eq!(host.sim().expect("sim").tick().0, MAX_CATCH_UP_TICKS as u64);
    assert_eq!(host.run.as_ref().expect("run").dropped, 10 - MAX_CATCH_UP_TICKS as u64);
    // The schedule continues from now, not from the dropped ticks.
    tick(&clock, 1);
    host.update(&mut out).expect("a pass");
    assert_eq!(host.sim().expect("sim").tick().0, MAX_CATCH_UP_TICKS as u64 + 1);
}

#[test]
fn commands_arrive_stamped_and_pause_holds_the_clock_but_not_the_snapshots() {
    let _serial = testkit::serial();
    let clock = ManualClock::new();
    let (mut host, mut link) = ends(&clock);
    let mut out = Vec::new();
    host.start_sortie(testkit::basin()).expect("starts");
    tick(&clock, 4);
    host.update(&mut out).expect("a pass");
    link.send(ClientMsg::Command(Command::Pause(true)));
    link.send(ClientMsg::Input(PlayerInput { input_tick: 3, throttle: 0.5, ..Default::default() }));
    link.send(ClientMsg::Input(PlayerInput { input_tick: 2, throttle: -1.0, ..Default::default() }));
    host.drain_client(&mut out);
    assert_eq!(host.pending.len(), 1);
    assert_eq!((host.pending[0].player, host.pending[0].seq, host.pending[0].rtt_ms), (PlayerSlot(0), 1, 0));
    assert_eq!(host.input.map(|i| i.input_tick), Some(3), "the newest input by input tick");
    tick(&clock, 2);
    host.update(&mut out).expect("a pass");
    assert!(host.sim().expect("sim").pacing().paused);
    let paused_at = host.sim().expect("sim").tick().0;
    let _ = link.snapshots.take_latest();
    for _ in 0..10 { tick(&clock, 1); host.update(&mut out).expect("a pass"); }
    assert_eq!(host.sim().expect("sim").tick().0, paused_at, "paused: no ticks");
    let held = link.snapshots.take_latest().expect("snapshots keep coming while paused");
    assert_eq!(held.tick, paused_at);
    link.send(ClientMsg::Command(Command::Pause(false)));
    for _ in 0..5 { tick(&clock, 1); host.update(&mut out).expect("a pass"); }
    assert!(host.sim().expect("sim").tick().0 > paused_at, "resumed");
    // Compression is the Sim's to grant or refuse; either way one real tick runs the effective rate.
    link.send(ClientMsg::Command(Command::RequestTimeRate(TimeRate::X4)));
    for _ in 0..5 { tick(&clock, 1); host.update(&mut out).expect("a pass"); }
    let rate = host.sim().expect("sim").pacing().effective.factor() as u64;
    let before = host.sim().expect("sim").tick().0;
    tick(&clock, 1);
    host.update(&mut out).expect("a pass");
    assert_eq!(host.sim().expect("sim").tick().0 - before, rate, "one real tick runs the effective rate's steps");
}

#[test]
fn ending_the_sortie_delivers_its_last_events_and_closes_it() {
    let _serial = testkit::serial();
    let clock = ManualClock::new();
    let (mut host, link) = ends(&clock);
    let mut out = Vec::new();
    host.start_sortie(testkit::basin()).expect("starts");
    tick(&clock, 3);
    host.update(&mut out).expect("a pass");
    let _ = session_events(&link);
    let _ = sim_events(&link);
    host.sim_mut().expect("sim").finish(Outcome::Aborted);
    let (sim, replay) = host.end_sortie().expect("the sim comes back");
    assert!(replay.is_none(), "the stand-in records no replay");
    assert_eq!(sim.result().map(|r| r.outcome), Some(Outcome::Aborted));
    assert_eq!(host.phase(), SessionPhase::AfterAction);
    assert!(host.sim().is_none());
    assert!(sim_events(&link).iter().any(|e| matches!(e.event, SimEvent::SortieEnded(_))));
    assert!(session_events(&link).iter().any(|e| matches!(e, ClientEvent::SortieClosed(s) if s.outcome == Outcome::Aborted)));
    assert!(host.end_sortie().is_none());
    // Between sorties the phase moves freely; sortie phases only through `start_sortie`.
    assert!(host.set_phase(SessionPhase::Lobby).is_ok());
    assert!(host.set_phase(SessionPhase::Sortie).is_err());
}

#[test]
fn the_rest_of_the_host_port_does_what_the_stand_in_promises() {
    let clock = ManualClock::new();
    let (mut host, mut link) = ends(&clock);
    let setup = host.lobby_setup();
    assert_eq!(setup.players.len(), 1);
    assert_eq!(setup.mode, content::enums::GameMode::Splash);
    assert!(matches!(host.set_udp(None, crate::seams::DropReason::Left), Err(NetError::WrongPhase)));
    let panel = host.connection_panel();
    assert_eq!(panel.players.len(), 1);
    assert!(panel.players[0].host);
    let packed = content::share::pack(&content::blueprint::Shareable::Ship(Box::new(testkit::starter())));
    host.put_blueprint(packed.clone());
    assert_eq!(host.blueprint(&packed.hash), Some(packed.compressed.clone()));
    assert_eq!(link.blueprint(&packed.hash), Some(packed.clone()), "both ends share one store");
    host.send(Some(PlayerSlot(3)), ServerMsg::Saturation { on: true });
    host.send(None, ServerMsg::Saturation { on: true });
    assert_eq!(session_events(&link), vec![ClientEvent::Saturation { on: true }], "only messages for this player arrive");
    assert_eq!(host.drain_log(), "");
}

#[test]
fn the_client_end_is_welcomed_and_sees_the_host_go() {
    let clock = ManualClock::new();
    let (mut host, mut link) = ends(&clock);
    let mut inbox = LinkInbox::default();
    host.shutdown();
    link.poll(0.0, &mut inbox);
    assert!(matches!(inbox.session.first(), Some(ClientEvent::Welcomed(w)) if w.slot == PlayerSlot(0) && w.phase == SessionPhase::Lobby));
    assert!(inbox.session.iter().any(|e| matches!(e, ClientEvent::Ended(SessionEnd::HostQuit))));
    assert_eq!(link.status().state, LinkState::Ended(SessionEnd::HostQuit));
    // A host end dropped without a word is host loss, reported once.
    let clock = ManualClock::new();
    let (host, mut link) = ends(&clock);
    drop(host);
    let mut inbox = LinkInbox::default();
    link.poll(0.0, &mut inbox);
    link.poll(0.0, &mut inbox);
    assert_eq!(inbox.session.iter().filter(|e| matches!(e, ClientEvent::Ended(SessionEnd::HostLost))).count(), 1);
    link.close();
    link.close();
    assert_eq!(link.status().state, LinkState::Closed);
    link.send(ClientMsg::Command(Command::Pause(true)));
}

#[test]
fn the_tick_p95_skips_only_the_first_second_of_a_sortie() {
    let clock = ManualClock::new();
    let (mut host, _link) = ends(&clock);
    let run = |steps| SortieRun { epoch: Duration::ZERO, real_ticks: 0, carry: 0.0, next_snapshot: 0, dropped: 0, over_reported: false, steps };
    // Heavy overload: 40 ms steps, far fewer than 60 of them in the 2 s window.
    for i in 0..30u64 { host.times.record(Duration::from_millis(i * 40), 40.0); }
    host.run = Some(run(10));
    assert_eq!(host.p95_ms(), 0.0, "warming up");
    host.run = Some(run(MIN_P95_STEPS));
    assert_eq!(host.p95_ms(), 40.0, "a slow sortie is reported however few steps fit the window");
    host.run = None;
    assert_eq!(host.p95_ms(), 40.0);
}

#[test]
fn a_sim_restored_from_a_checkpoint_takes_the_new_sessions_commands() {
    let _serial = testkit::serial();
    let clock = ManualClock::new();
    let (mut host, mut link) = ends(&clock);
    let mut out = Vec::new();
    host.start_sortie(testkit::basin()).expect("starts");
    tick(&clock, 2);
    host.update(&mut out).expect("a pass");
    for on in [true, false, true] {
        link.send(ClientMsg::Command(Command::Pause(on)));
        tick(&clock, 1);
        host.update(&mut out).expect("a pass");
    }
    assert!(host.sim().expect("sim").pacing().paused);
    let checkpoint = host.sim().expect("sim").checkpoint();
    let _ = host.end_sortie();
    // A new pair numbers its commands from 1 again; the restored Sim has seen 1 to 3.
    let (mut host, mut link) = ends(&clock);
    host.start_sortie(Sim::from_checkpoint(&checkpoint, testkit::deps()).expect("restores")).expect("starts");
    let _ = sim_events(&link);
    link.send(ClientMsg::Command(Command::Pause(false)));
    for _ in 0..3 { tick(&clock, 1); host.update(&mut out).expect("a pass"); }
    assert!(!host.sim().expect("sim").pacing().paused, "the unpause is applied, not refused as a duplicate");
    let refused = sim_events(&link).into_iter().filter(|e| matches!(e.event, SimEvent::CommandRejected { .. })).count();
    assert_eq!(refused, 0);
}

#[test]
fn the_stand_in_interpolates_at_its_own_snapshot_rate() {
    // Whole snapshots every 2 real ticks: 30 Hz, so L914's default 4 x 33 ms.
    assert_eq!(channel_interp(), InterpTuning::default());
    let clock = ManualClock::new();
    let (_host, link) = ends(&clock);
    assert_eq!(*link.presenter.buffer.tuning(), InterpTuning::default());
}
