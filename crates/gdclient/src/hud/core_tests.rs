//! Tests of the HUD core's pure parts: section diffing, the event walk, the log book, override ids and the
//! text rows of `game/i18n/log.csv`.
use super::*;
use crate::seams::WireEvent;
use content::enums::{Domain, IdRung};
use sim_systems::ids::{MountRef, ObjectiveId};
use sim_systems::protocol::{
    AlertKind, CompressionBlock, FireReason, HoldReason, ObjectiveState, ObjectiveView, PartView, RungReason, StanceReason, Subsystem, TimeRate, TrackView, UncertaintyShape,
};

fn entity(n: u64) -> EntityId { EntityId::from_u64(n) }
const OWN: u64 = 1 << 20 | 3;

fn ship() -> ShipStatus { ShipStatus { vessel: entity(OWN), speed_kn: 12.0, ..Default::default() } }
fn own_view() -> VesselView {
    VesselView { entity: entity(OWN), kin: Default::default(), status: Default::default(), flags: Default::default(), turrets: Vec::new(), rudder: 0.0, power: 0.5, track: TrackId::NONE, relevance: Default::default() }
}
fn snap(tick: u64) -> Snapshot { Snapshot { tick, ship: Some(ship()), vessels: vec![own_view()], ..Default::default() } }
fn track(id: u32) -> TrackView {
    TrackView {
        id: TrackId(id), domain: Domain::Surface, stance: Stance::Suspect, stance_reason: StanceReason::Default, rung: IdRung::Domain, rung_reason: RungReason::None,
        grade: content::enums::TrackGrade::Coarse, pos: glam::DVec3::new(100.0, 0.0, -2_000.0), vel: glam::DVec3::ZERO, shape: UncertaintyShape::Circle { radius_m: 30.0 },
        last_update_tick: 0, last_known: false, class: None, candidates: 2, sources: Default::default(), trend: Default::default(), inbound: false, engaged_by: 0,
        envelope_m: 0.0, emitting: false, relevance: Default::default(), struck: false,
    }
}
fn has(bits: u32, s: Section) -> bool { bits & s.bit() != 0 }
fn ev(tick: u64, event: SimEvent) -> WireEvent { WireEvent { tick, event } }

fn core() -> HudCore {
    HudCore { own_vessel: Some(entity(OWN)), own_side: Some(SideId(0)), own_slot: Some(PlayerSlot(0)), started_logged: true, ..HudCore::default() }
}
fn logs(outs: &[Out]) -> Vec<&LogDraft> { outs.iter().filter_map(|o| if let Out::Log(d) = o { Some(d) } else { None }).collect() }

fn alert(id: u32, priority: AlertPriority, tick: u64) -> AlertView {
    AlertView { id: AlertId(id), vessel: entity(OWN), priority, subsystem: Subsystem::Fire, kind: AlertKind::Fire { compartment: CompartmentId(2) }, state: AlertState::Unacknowledged, raised_tick: tick, officer: Some(OfficerRole::DamageControl), blink_fast: false }
}

#[test]
fn sections_diff_only_what_changed() {
    let a = snap(30);
    assert_eq!(diff_sections(Some(&a), Some(&a), Some((DVec2::ZERO, 30))), 0);
    assert_eq!(diff_sections(None, None, None), 0);
    let all = diff_sections(None, Some(&a), None);
    assert!(Section::ALL.iter().all(|s| has(all, *s)));

    let mut b = a.clone();
    b.ship.as_mut().unwrap().speed_kn = 13.0;
    let bits = diff_sections(Some(&a), Some(&b), Some((DVec2::ZERO, 30)));
    assert!(has(bits, Section::Motion) && !has(bits, Section::Parts) && !has(bits, Section::Board) && !has(bits, Section::Tracks));

    let mut c = a.clone();
    c.ship.as_mut().unwrap().parts.push(PartView { slot: PartSlot(2), level: Default::default(), health: 255, power: Default::default(), fuel: Default::default(), data: Default::default(), shed: true, priority: 3, readout: Default::default() });
    let bits = diff_sections(Some(&a), Some(&c), Some((DVec2::ZERO, 30)));
    assert!(has(bits, Section::Parts) && has(bits, Section::Power) && has(bits, Section::Networks) && has(bits, Section::Actions) && !has(bits, Section::Motion));

    let mut d = a.clone();
    d.ship.as_mut().unwrap().held.push(ControlRole::Helm);
    let bits = diff_sections(Some(&a), Some(&d), Some((DVec2::ZERO, 30)));
    assert!(has(bits, Section::Board) && has(bits, Section::Mounts));

    // Tracks: content, own movement past 5 m, and a sortie second of age.
    let mut e = a.clone();
    e.tracks.push(track(4));
    assert!(has(diff_sections(Some(&a), Some(&e), Some((DVec2::ZERO, 30))), Section::Tracks));
    let mut f = e.clone();
    f.tick = 40;
    assert!(!has(diff_sections(Some(&e), Some(&f), Some((DVec2::ZERO, 30))), Section::Tracks));
    f.tick = 90;
    assert!(has(diff_sections(Some(&e), Some(&f), Some((DVec2::ZERO, 30))), Section::Tracks));
    let mut g = e.clone();
    g.vessels[0].kin.pose.pos.x = 6.0;
    assert!(has(diff_sections(Some(&e), Some(&g), Some((DVec2::ZERO, 30))), Section::Tracks));

    // Losing the hull changes every own-ship section.
    let mut lost = a.clone();
    lost.ship = None;
    let bits = diff_sections(Some(&a), Some(&lost), None);
    assert!(Section::OWN_SHIP.iter().all(|s| has(bits, *s)));
}

#[test]
fn walk_logs_announces_and_skips_repeats() {
    let mut c = core();
    let s = snap(90);
    let mut out = Vec::new();
    let events = vec![
        ev(90, SimEvent::TrackCreated { side: SideId(0), track: TrackId(7), domain: Domain::Surface }),
        ev(90, SimEvent::TrackCreated { side: SideId(1), track: TrackId(8), domain: Domain::Air }),
        ev(91, SimEvent::OfficerMessage { vessel: entity(OWN), role: OfficerRole::Gunnery, text: Msg::new("officer.x") }),
        ev(91, SimEvent::OfficerMessage { vessel: entity(99), role: OfficerRole::Helm, text: Msg::new("officer.y") }),
        ev(92, SimEvent::ControlChanged { vessel: entity(OWN), role: ControlRole::Helm, holder: ControlHolder::Player }),
    ];
    c.walk(&events, Some(&s), SessionState::InSortie, SessionKind::Skirmish, &mut out);
    assert!(out.iter().any(|o| matches!(o, Out::TrackEvent { kind: "created", track, .. } if *track == TrackId(7))));
    assert!(!out.iter().any(|o| matches!(o, Out::TrackEvent { track, .. } if *track == TrackId(8))));
    let lines: Vec<_> = out.iter().filter_map(|o| if let Out::Line(l) = o { Some(l) } else { None }).collect();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].new_speaker && lines[0].tick == 91);
    assert!(out.iter().any(|o| matches!(o, Out::Control { role: ControlRole::Helm, holder: ControlHolder::Player })));
    let l = logs(&out);
    assert_eq!(l.len(), 3);
    assert_eq!(l[0].text.key.as_str(), "log.track.created");
    assert_eq!(l[0].tick, 90);
    assert!(l[1].silent && l[1].kind == LogKind::Officer);
    assert!(!l[2].silent && l[2].text.key.as_str() == "log.control.take.helm");

    // A kill-cam clip replays older events: they build state but announce and log nothing.
    let mut out = Vec::new();
    let replayed = vec![
        ev(60, SimEvent::SortieStarted { seed: 1, tick_hz: 60 }),
        ev(61, SimEvent::OfficerMessage { vessel: entity(OWN), role: OfficerRole::Gunnery, text: Msg::new("officer.x") }),
        ev(62, SimEvent::AlertRaised(alert(5, AlertPriority::Alarm, 62))),
    ];
    c.walk(&replayed, Some(&s), SessionState::InSortie, SessionKind::Skirmish, &mut out);
    assert!(logs(&out).is_empty());
    assert!(!out.iter().any(|o| matches!(o, Out::Line(_) | Out::AlertRaised(_) | Out::Reset)));
    assert!(c.alerts.get(AlertId(5)).is_some());
    // The same speaker again soon is not named again; after 30 s it is.
    let mut out = Vec::new();
    c.walk(&[ev(100, SimEvent::OfficerMessage { vessel: entity(OWN), role: OfficerRole::Gunnery, text: Msg::new("officer.z") })], Some(&s), SessionState::InSortie, SessionKind::Skirmish, &mut out);
    assert!(matches!(&out[0], Out::Line(l) if !l.new_speaker));
    let mut out = Vec::new();
    c.walk(&[ev(100 + 31 * 60, SimEvent::OfficerMessage { vessel: entity(OWN), role: OfficerRole::Gunnery, text: Msg::new("officer.z") })], Some(&s), SessionState::InSortie, SessionKind::Skirmish, &mut out);
    assert!(matches!(&out[0], Out::Line(l) if l.new_speaker));

    // Events of another channel may arrive behind the newest tick: they still log.
    let mut out = Vec::new();
    c.walk(&[ev(95, SimEvent::Phys(PhysEvent::FireStarted { vessel: entity(OWN), compartment: CompartmentId(1), spread: false }))], Some(&s), SessionState::InSortie, SessionKind::Skirmish, &mut out);
    assert_eq!(logs(&out).len(), 1);
    // The end of a clip re-opens at the live tick: nothing is muted and the start is not logged twice.
    let mut out = Vec::new();
    c.walk(&[ev(5_000, SimEvent::SortieStarted { seed: 1, tick_hz: 60 }), ev(5_000, SimEvent::Seduced { body: entity(5), decoy: entity(6) })], Some(&s), SessionState::InSortie, SessionKind::Skirmish, &mut out);
    assert_eq!(logs(&out).iter().map(|d| d.text.key.as_str()).collect::<Vec<_>>(), vec!["log.engage.seduced"]);
    // A splash rewind goes back in time: everything starts again rather than going mute.
    let mut out = Vec::new();
    c.walk(&[ev(1_000, SimEvent::SortieStarted { seed: 1, tick_hz: 60 }), ev(1_001, SimEvent::Seduced { body: entity(5), decoy: entity(6) })], None, SessionState::InSortie, SessionKind::Splash, &mut out);
    assert!(matches!(out.first(), Some(Out::Reset)));
    assert_eq!(logs(&out).len(), 2);

    // A replay's opening resets.
    let mut out = Vec::new();
    c.walk(&[ev(10, SimEvent::SortieStarted { seed: 1, tick_hz: 60 })], None, SessionState::Replay, SessionKind::Replay, &mut out);
    assert!(matches!(out.first(), Some(Out::Reset)));
    assert!(c.alerts.history.is_empty() && c.own_vessel.is_none());
}

#[test]
fn engagement_and_alert_entries_follow_changes() {
    let mut c = core();
    let mount = MountRef { vessel: entity(OWN), slot: PartSlot(4) };
    let mut out = Vec::new();
    let events = vec![
        ev(1, SimEvent::Holding { mount, track: TrackId(3), reason: HoldReason::Slewing }),
        ev(2, SimEvent::Holding { mount, track: TrackId(3), reason: HoldReason::Slewing }),
        ev(3, SimEvent::Engaging { mount, track: TrackId(3), reason: FireReason::Rule(2) }),
        ev(4, SimEvent::Engaging { mount, track: TrackId(3), reason: FireReason::Rule(2) }),
        ev(5, SimEvent::SolutionUnconverged { mount, track: TrackId(3), error_m: f32::NAN }),
        ev(6, SimEvent::SolutionUnconverged { mount, track: TrackId(3), error_m: 3.0 }),
        ev(7, SimEvent::AlertRaised(alert(1, AlertPriority::Caution, 7))),
        ev(8, SimEvent::AlertChanged { id: AlertId(1), state: AlertState::Cleared }),
        ev(9, SimEvent::AlertChanged { id: AlertId(77), state: AlertState::Cleared }),
        ev(10, SimEvent::AlertsSilenced { vessel: entity(OWN), until_tick: 10 + 30 * 60 }),
        ev(11, SimEvent::AlertFlood { vessel: entity(OWN), on: true }),
        ev(12, SimEvent::AlertFlood { vessel: entity(OWN), on: true }),
    ];
    c.walk(&events, None, SessionState::InSortie, SessionKind::Skirmish, &mut out);
    let l = logs(&out);
    let keys: Vec<&str> = l.iter().map(|d| d.text.key.as_str()).collect();
    assert_eq!(keys, vec!["log.engage.holding", "log.engage.engaging", "log.engage.unconverged", "sim.alert.fire", "log.alert.cleared", "log.alert.silenced", "log.alert.flood_on"]);
    // The engaging reason carries an argument, so it travels as a detail sentence.
    assert_eq!(l[1].detail.as_ref().map(|m| m.args.len()), Some(1));
    assert!(matches!(l[2].text.args.last(), Some(MsgArg::Num { value, .. }) if *value == 0.0));
    // A rectified Caution goes to the log unread and loud.
    assert!(!l[4].silent);
    assert_eq!(c.alerts.get(AlertId(1)).map(|a| a.state), Some(AlertState::Cleared));
    assert!(c.alerts.flood);
    assert!((c.alerts.silenced_s(10) - 30.0).abs() < 1e-9 && c.alerts.silenced_s(10_000) == 0.0);
    assert!(out.iter().any(|o| matches!(o, Out::AlertRaised(a) if a.id == AlertId(1))));
}

#[test]
fn pacing_entries_only_on_visible_change() {
    let base = Pacing { time_scale: 1.0, ..Default::default() };
    assert!(pacing_entry(None, &base).is_none());
    assert_eq!(pacing_entry(None, &Pacing { paused: true, ..base.clone() }).map(|d| d.text.key.0), Some("log.pacing.paused".into()));
    assert!(pacing_entry(Some(&base), &Pacing { host_tick_ms: 9.0, ..base.clone() }).is_none());
    let fast = Pacing { effective: TimeRate::X4, ..base.clone() };
    assert_eq!(pacing_entry(Some(&base), &fast).map(|d| d.text.args), Some(vec![MsgArg::Int(4)]));
    let held = Pacing { blocked: Some(CompressionBlock::WeaponInbound), ..base.clone() };
    assert_eq!(pacing_entry(Some(&base), &held).and_then(|d| d.detail).map(|m| m.key.0), Some("sim.compress.weapon_inbound".into()));
    let slow = Pacing { time_scale: 0.7, ..base.clone() };
    assert!(matches!(pacing_entry(Some(&base), &slow).map(|d| d.text.args), Some(v) if matches!(v[0], MsgArg::Num { value, .. } if (value - 70.0).abs() < 1e-6)));
    // Repeats of the same pacing through the walk log once.
    let mut c = core();
    let mut out = Vec::new();
    c.walk(&[ev(1, SimEvent::PacingChanged(fast.clone())), ev(2, SimEvent::PacingChanged(fast.clone()))], None, SessionState::InSortie, SessionKind::Skirmish, &mut out);
    assert_eq!(logs(&out).len(), 0, "the first pacing of a sortie is the starting state");
    c.walk(&[ev(3, SimEvent::PacingChanged(base.clone())), ev(4, SimEvent::PacingChanged(base.clone()))], None, SessionState::InSortie, SessionKind::Skirmish, &mut out);
    assert_eq!(logs(&out).len(), 1);
}

#[test]
fn objective_entries() {
    let mut c = core();
    let mut s = snap(5);
    s.objectives.push(ObjectiveView { id: ObjectiveId(1), ghost: true, state: ObjectiveState::Done, text: Msg::new("objective.no_radar"), progress: 1.0, marker: Some(DVec2::new(5.0, 6.0)) });
    let mut out = Vec::new();
    c.walk(&[
        ev(5, SimEvent::ObjectiveChanged { id: ObjectiveId(1), state: ObjectiveState::Done, progress: 1.0 }),
        ev(5, SimEvent::ObjectiveChanged { id: ObjectiveId(0), state: ObjectiveState::Active, progress: 0.5 }),
    ], Some(&s), SessionState::InSortie, SessionKind::Skirmish, &mut out);
    let l = logs(&out);
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].text.key.as_str(), "log.objective.ghost_done");
    assert_eq!(l[0].detail.as_ref().map(|m| m.key.as_str()), Some("objective.no_radar"));
    assert_eq!(l[0].pos, Some([5.0, 6.0]));
    assert!(out.iter().any(|o| matches!(o, Out::Bump(Section::Objectives))));
}

#[test]
fn log_book_filters_pages_and_caps() {
    let mut log = Vec::new();
    let mut seq = 1;
    let entry = |kind: LogKind, silent: bool, track: u32| LogEntry { seq: 0, tick: 0, kind, text: Msg::new("x"), track: TrackId(track), part: None, vessel: None, pos: None, unread: true, silent };
    for i in 0..10 { logbook::push(&mut log, &mut seq, entry(if i % 2 == 0 { LogKind::Track } else { LogKind::Alert }, i % 3 == 0, i)); }
    assert_eq!(log.iter().map(|e| e.seq).collect::<Vec<_>>(), (1..=10).collect::<Vec<_>>());
    let all = logbook::Filter::default();
    assert_eq!(logbook::select(&log, &all, 0, 3).iter().map(|e| e.seq).collect::<Vec<_>>(), vec![8, 9, 10]);
    assert_eq!(logbook::select(&log, &all, 8, 200).iter().map(|e| e.seq).collect::<Vec<_>>(), vec![9, 10]);
    let alerts = logbook::Filter { kinds: Some(vec![LogKind::Alert]), ..Default::default() };
    assert!(logbook::select(&log, &alerts, 0, 200).iter().all(|e| e.kind == LogKind::Alert));
    let t4 = logbook::Filter { track: Some(TrackId(4)), ..Default::default() };
    assert_eq!(logbook::select(&log, &t4, 0, 200).len(), 1);
    // Silent entries are seq 1, 4, 7 and 10: never counted unread.
    assert_eq!(logbook::unread(&log), 6);
    assert!(logbook::mark_read(&mut log, 5));
    assert!(!logbook::mark_read(&mut log, 5));
    assert_eq!(logbook::unread(&log), 3);
    let unread = logbook::Filter { unread_only: true, ..Default::default() };
    assert_eq!(logbook::select(&log, &unread, 0, 200).first().map(|e| e.seq), Some(6));
    // The cap drops the oldest silent entry first.
    let mut big: Vec<LogEntry> = Vec::new();
    let mut seq = 1;
    logbook::push(&mut big, &mut seq, entry(LogKind::Alert, false, 0));
    for _ in 0..LOG_CAP { logbook::push(&mut big, &mut seq, entry(LogKind::Track, true, 0)); }
    assert_eq!(big.len(), LOG_CAP);
    assert_eq!(big[0].seq, 1);
}

#[test]
fn override_ids_round_trip() {
    let sinks = [
        SinkKind::HelmHeading, SinkKind::HelmSpeed, SinkKind::MountAimPoint(PartSlot(3)), SinkKind::MountFirePermission(PartSlot(4)), SinkKind::MissileLaunch(PartSlot(5)),
        SinkKind::MissileTarget(PartSlot(6)), SinkKind::EmissionState(PartSlot(7)), SinkKind::PowerPriority(PartSlot(8)), SinkKind::Pump(PartSlot(9)), SinkKind::Door(PartSlot(10)),
        SinkKind::FloodValve(CompartmentId(11)), SinkKind::TrackStanceOverride, SinkKind::TrackIdOverride, SinkKind::ShipPreset, SinkKind::ShipEmcon,
    ];
    let mut keys = vec![
        OverrideKey::Preset, OverrideKey::MountPreset(PartSlot(65_535)), OverrideKey::Slider("gunnery.aggression/x y".into()), OverrideKey::Emcon,
        OverrideKey::BurstTriggerRange, OverrideKey::SelfDefenceRange, OverrideKey::Doctrine { mount: None, index: 3 }, OverrideKey::Doctrine { mount: Some(PartSlot(2)), index: 255 },
        OverrideKey::PowerPriority(PartSlot(1)), OverrideKey::RepairOrder, OverrideKey::TrackStance(TrackId(412)), OverrideKey::TrackIdentity(TrackId(u32::MAX)),
    ];
    keys.extend(sinks.into_iter().map(OverrideKey::Sink));
    for k in keys {
        let id = override_id::encode(&k);
        assert_eq!(override_id::decode(&id), Some(k.clone()), "{id}");
    }
    let long = "slider/".repeat(200);
    for bad in ["", "preset/1", "mount_preset/-1", "mount_preset/70000", "slider/", "doctrine/ship", "doctrine/x/1", "sink/helm_heading/2", "sink/pump", "track_stance/0", "nope", long.as_str()] {
        assert_eq!(override_id::decode(bad), None, "{bad}");
    }
}

#[test]
fn section_names_round_trip() {
    for s in Section::ALL {
        assert_eq!(Section::from_name(s.name()), Some(s));
        assert_eq!(Section::ALL[s.index()], s);
    }
    assert_eq!(Section::from_name("bogus"), None);
}

#[test]
fn every_key_has_an_english_row() {
    let lk = crate::text::tests::CsvLookup::load();
    let keys = text_keys();
    for k in &keys { assert!(lk.0.contains_key(*k), "no row for {k}"); }
    // Every row of log.csv is used, and every row is a plain sentence-case English line.
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/i18n/log.csv");
    let rows = crate::text::tests::parse_csv(&std::fs::read_to_string(path).expect("log.csv"));
    for (key, ctx, en) in &rows {
        assert!(keys.contains(&key.as_str()), "unused row {key}");
        assert!(!ctx.is_empty() && !en.is_empty(), "{key}");
        assert!(key.starts_with("log.") || key.starts_with("hud.") || key.starts_with("orders."), "{key}");
        let first = en.chars().next().unwrap();
        assert!(first == '{' || first.is_uppercase(), "{key}: sentence case");
        assert!(!en.contains('\u{2014}') && !en.contains('!'), "{key}");
        // Placeholders are numbered from 0 without gaps.
        let n = (0..10).take_while(|i| en.contains(&format!("{{{i}}}"))).count();
        assert!((n..10).all(|i| !en.contains(&format!("{{{i}}}"))), "{key}");
    }
    // Keys the walk builds from other namespaces exist too.
    for k in ["units.part.generated", "units.part.numbered", "units.word.station.photograph", "units.word.outcome.victory", "units.word.domain.surface", "units.word.damage.heavy", "units.word.priority.alarm"] {
        assert!(lk.0.contains_key(k), "{k}");
    }
}
