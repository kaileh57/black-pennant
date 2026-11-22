use super::*;
use crate::session::testkit;
use sim_systems::replay::record::{RecordOptions, ReplayRecorder};
use std::sync::OnceLock;

/// Seconds of the recorded basin: past the first 60 s keyframe.
const RECORDED_S: u64 = 70;

/// A real recording of the test basin (recorded once per test run).
fn recording() -> &'static Vec<u8> {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| {
        let _serial = testkit::serial();
        let mut sim = testkit::basin();
        let mut rec = ReplayRecorder::start(&sim, "test", RecordOptions::default());
        testkit::pool().install(|| {
            for _ in 0..RECORDED_S * sim_core::TICK_HZ as u64 {
                sim.step();
                let events = sim.drain_events();
                rec.record_tick(&sim, &events);
            }
        });
        rec.finish()
    })
}

/// A private cache folder: `DiskCache::open` tidies the folder it is given.
fn cache() -> std::path::PathBuf { std::env::temp_dir().join("bp-replay-link-test-cache") }
fn open() -> ReplayLink { ReplayLink::open(recording(), testkit::catalogue(), cache()).expect("the recording opens") }

#[test]
fn a_recording_opens_with_its_viewers_and_another_format_is_refused() {
    let link = open();
    assert!(link.duration_ticks() >= (RECORDED_S - 1) * sim_core::TICK_HZ as u64, "{}", link.duration_ticks());
    let viewers = link.viewers();
    assert_eq!(viewers.first(), Some(&Viewer::Omniscient), "a single-player replay shows ground truth");
    assert!(viewers.contains(&Viewer::Side(SideId(0))), "and the player's side: {viewers:?}");
    assert_eq!(link.control.viewer, Viewer::Omniscient);
    assert!(link.opening.as_ref().is_some_and(|o| o.iter().any(|e| matches!(e.event, SimEvent::EntityRevealed(_)))));
    // Format word after the magic: a build opens only its own (L1045).
    let mut other = recording().clone();
    let found = content::replay::REPLAY_FORMAT + 1;
    other[4..8].copy_from_slice(&found.to_le_bytes());
    match ReplayLink::open(&other, testkit::catalogue(), cache()) {
        Err(ReplayError::Format { found: f, expected }) => assert_eq!((f, expected), (found, content::replay::REPLAY_FORMAT)),
        Err(e) => panic!("wrong refusal: {e}"),
        Ok(_) => panic!("another format opened"),
    }
    assert!(ReplayLink::open(b"not a replay", testkit::catalogue(), cache()).is_err());
}

#[test]
fn audiences_follow_the_viewer() {
    let mut link = open();
    let ev = |audience| OutEvent { tick: 0, audience, lane: Default::default(), event: SimEvent::EntityHidden { entity: sim_core::EntityId { index: 1, generation: 1 } } };
    let msg = |audience| OutEvent { tick: 0, audience, lane: Default::default(), event: SimEvent::SortieStarted { seed: 1, tick_hz: 60 } };
    link.built_for = Viewer::Omniscient;
    assert!(link.admits(&ev(Audience::Sides(SideMask::NONE))));
    assert!(!link.admits(&ev(Audience::side(SideId(0)))), "a side's own removals would hide ships from ground truth");
    assert!(link.admits(&msg(Audience::side(SideId(1)))));
    assert!(!link.admits(&msg(Audience::Host)));
    link.built_for = Viewer::Side(SideId(0));
    assert!(link.admits(&ev(Audience::side(SideId(0)))));
    assert!(!link.admits(&ev(Audience::side(SideId(1)))));
    assert!(!link.admits(&ev(Audience::Sides(SideMask::NONE))));
    assert!(link.admits(&msg(Audience::player(PlayerSlot(0)))), "the side's player is on the roster");
    assert!(!link.admits(&msg(Audience::player(PlayerSlot(5)))));
    assert!(!link.admits(&msg(Audience::Host)));
}

#[test]
fn playback_runs_seeks_changes_rate_and_pauses_at_the_end() {
    let mut link = open();
    let mut inbox = LinkInbox::default();
    let mut now = 0.0;
    let mut frames = 0;
    let mut revealed = false;
    for i in 0..60 {
        inbox = LinkInbox::default();
        link.poll(now, &mut inbox);
        if i == 0 { assert!(matches!(inbox.session.first(), Some(ClientEvent::SortieRunning { .. }))); }
        revealed |= inbox.events.iter().any(|e| matches!(e.event, SimEvent::EntityRevealed(_)));
        if inbox.frame.is_some() { frames += 1; }
        now += 1.0 / 60.0;
    }
    assert!(revealed, "the opening introductions reach the client");
    assert!(frames >= 55, "{frames} frames of 60");
    let after_1s = link.tick();
    assert!((50..=70).contains(&after_1s), "1 s of wall time is about 60 ticks at 1x: {after_1s}");
    // Seek past the keyframe at 60 s.
    link.control.seek_to = Some(62 * 60);
    link.poll(now, &mut inbox);
    assert!(link.tick() >= 62 * 60 && link.tick() < 62 * 60 + 8, "{}", link.tick());
    let mut seen_frame = false;
    for _ in 0..30 { now += 1.0 / 60.0; inbox = LinkInbox::default(); link.poll(now, &mut inbox); seen_frame |= inbox.frame.is_some(); }
    assert!(seen_frame);
    // 8x for a quarter second of wall time moves two seconds of replay.
    let t0 = link.tick();
    link.control.rate = 8.0;
    for _ in 0..15 { now += 1.0 / 60.0; link.poll(now, &mut LinkInbox::default()); }
    let moved = link.tick() - t0;
    assert!((100..=140).contains(&moved), "{moved} ticks at 8x");
    // Paused, nothing moves; at the end it pauses itself.
    link.control.paused = true;
    let t1 = link.tick();
    for _ in 0..10 { now += 1.0 / 60.0; link.poll(now, &mut LinkInbox::default()); }
    assert_eq!(link.tick(), t1);
    link.control.paused = false;
    for _ in 0..200 { now += 0.25; link.poll(now, &mut LinkInbox::default()); }
    assert!(link.control.paused, "the end of the file pauses");
    assert_eq!(link.tick(), link.duration_ticks());
    // A viewer change replays the current moment for that side.
    link.control.viewer = Viewer::Side(SideId(0));
    let mut inbox = LinkInbox::default();
    link.poll(now, &mut inbox);
    assert_eq!(link.built_for, Viewer::Side(SideId(0)));
    assert!(inbox.events.iter().any(|e| matches!(e.event, SimEvent::SortieStarted { .. })), "a seek opens with the sortie start");
    let _ = inbox;
}

#[test]
fn a_clip_plays_its_window_and_stops() {
    let mut link = ReplayLink::clip(recording(), 600, 900, Viewer::Side(SideId(0)), testkit::catalogue(), cache()).expect("clip");
    assert_eq!(link.control.viewer, Viewer::Side(SideId(0)));
    let mut now = 0.0;
    for _ in 0..1_000 { link.poll(now, &mut LinkInbox::default()); now += 1.0 / 60.0; if link.control.paused { break; } }
    assert!(link.control.paused);
    assert_eq!(link.tick(), 900);
}
