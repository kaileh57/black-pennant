use super::*;

const S: u64 = TICK_HZ as u64;

fn v(n: u32) -> EntityId { EntityId::from_u64(n as u64) }
fn c(vessel: u32, kind: AlertKind) -> Condition { Condition { vessel: v(vessel), kind } }
fn step(m: &mut AlertManager, conds: &[Condition], tick: u64) -> Vec<AlertChange> { let mut out = Vec::new(); m.update(conds, tick, &mut out); out }
fn state(m: &AlertManager, id: u32) -> AlertState { m.alerts[id as usize].state }

#[test]
fn classification_thresholds() {
    assert_eq!(classify(&AlertKind::MagazineFire { magazine: PartSlot(1), cookoff_in_s: 31.0 }).0, AlertPriority::Alarm);
    assert_eq!(classify(&AlertKind::MagazineFire { magazine: PartSlot(1), cookoff_in_s: 29.0 }).0, AlertPriority::Emergency);
    assert_eq!(classify(&AlertKind::FuelLow { fraction: 0.2 }).0, AlertPriority::Caution);
    assert_eq!(classify(&AlertKind::FuelLow { fraction: 0.05 }).0, AlertPriority::Warning);
    assert_eq!(classify(&AlertKind::WeaponInbound { track: TrackId(1) }), (AlertPriority::Alarm, Subsystem::Threat, Some(OfficerRole::Gunnery)));
    assert_eq!(classify(&AlertKind::CapsizeImminent).0, AlertPriority::Emergency);
}

#[test]
fn raise_update_escalate_and_clear() {
    let mut m = AlertManager::default();
    let mag = |s: f64| c(1, AlertKind::MagazineFire { magazine: PartSlot(3), cookoff_in_s: s });
    let out = step(&mut m, &[mag(90.0)], 10);
    assert!(matches!(out[0], AlertChange::Raised(a) if a.priority == AlertPriority::Alarm && a.blink_fast == false));
    assert!(m.alerts[0].blink_fast, "the only Alarm blinks fast");
    // Value update in place, no event.
    assert!(step(&mut m, &[mag(60.0)], 11).is_empty());
    assert_eq!(m.alerts.len(), 1);
    assert!(m.acknowledge(v(1), None, &mut Vec::new()));
    assert_eq!(state(&m, 0), AlertState::Acknowledged);
    assert!(!m.alerts[0].blink_fast);
    // Threshold crossing escalates and needs a fresh acknowledgement.
    let out = step(&mut m, &[mag(20.0)], 12);
    assert!(matches!(out[0], AlertChange::Escalated(a) if a.priority == AlertPriority::Emergency && a.state == AlertState::Unacknowledged));
    // Rectified while unacknowledged: Emergency shows Cleared; acknowledging dismisses it to the log.
    let out = step(&mut m, &[], 13);
    assert_eq!(out, vec![AlertChange::State(AlertId(0), AlertState::Cleared)]);
    assert!(m.acknowledge(v(1), None, &mut Vec::new()));
    assert_eq!(state(&m, 0), AlertState::LogOnly);
    assert_eq!(m.raised_in_window(v(1), 13, 60.0), 1);
}

#[test]
fn lower_alerts_go_to_the_log_and_chatter_counts_once() {
    let mut m = AlertManager::default();
    let fuel = c(2, AlertKind::FuelLow { fraction: 0.2 });
    step(&mut m, &[fuel], 0);
    step(&mut m, &[], 1);
    assert_eq!(state(&m, 0), AlertState::LogOnly);
    // Back within 30 s: same alert reopens, no new annunciation.
    let out = step(&mut m, &[fuel], 10 * S);
    assert_eq!(out, vec![AlertChange::State(AlertId(0), AlertState::Unacknowledged)]);
    assert_eq!(m.alerts.len(), 1);
    assert_eq!(m.raised_in_window(v(2), 10 * S, 1200.0), 1);
    // Back after 30 s: a new alert.
    step(&mut m, &[], 10 * S + 1);
    step(&mut m, &[fuel], 50 * S);
    assert_eq!(m.alerts.len(), 2);
}

#[test]
fn acknowledge_takes_highest_then_oldest_and_checks_vessel() {
    let mut m = AlertManager::default();
    step(&mut m, &[c(1, AlertKind::PropulsionLost)], 0);
    step(&mut m, &[c(1, AlertKind::PropulsionLost), c(1, AlertKind::Fire { compartment: CompartmentId(2) }), c(1, AlertKind::Fire { compartment: CompartmentId(1) }), c(1, AlertKind::EsmWarning)], 5);
    // Ids: 0 propulsion (Warning), then by key order 1 fire c1, 2 fire c2 (Alarms, same tick), 3 ESM (Caution).
    assert!(!m.acknowledge(v(9), Some(AlertId(1)), &mut Vec::new()), "another vessel's alert");
    assert!(!m.acknowledge(v(1), Some(AlertId(77)), &mut Vec::new()));
    let order: Vec<u32> = (0..4).filter_map(|_| { let mut o = Vec::new(); if m.acknowledge(v(1), None, &mut o) { match o[0] { AlertChange::State(id, _) => Some(id.0), _ => None } } else { None } }).collect();
    assert_eq!(order, vec![1, 2, 0], "Alarms oldest first, then the Warning; the Caution is skipped");
    // A Caution can still be acknowledged by id.
    assert!(m.acknowledge(v(1), Some(AlertId(3)), &mut Vec::new()));
}

#[test]
fn newest_top_alarm_blinks() {
    let mut m = AlertManager::default();
    step(&mut m, &[c(1, AlertKind::WeaponInbound { track: TrackId(1) })], 0);
    step(&mut m, &[c(1, AlertKind::WeaponInbound { track: TrackId(1) }), c(1, AlertKind::WeaponInbound { track: TrackId(2) })], 1);
    let blinking: Vec<u32> = m.alerts.iter().filter(|a| a.blink_fast).map(|a| a.id.0).collect();
    assert_eq!(blinking, vec![1]);
}

#[test]
fn fire_absorbs_same_compartment_and_emergency_suppresses() {
    let mut m = AlertManager::default();
    m.layouts.push(Layout { vessel: v(1), parts: vec![(PartSlot(4), CompartmentId(2)), (PartSlot(5), CompartmentId(3))], conduits: vec![] });
    let fire = c(1, AlertKind::Fire { compartment: CompartmentId(2) });
    let gun = c(1, AlertKind::PartHeavyDamage { slot: PartSlot(4) });
    let other = c(1, AlertKind::PartHeavyDamage { slot: PartSlot(5) });
    step(&mut m, &[fire, gun, other], 0);
    assert_eq!(state(&m, 0), AlertState::Unacknowledged);
    assert_eq!(state(&m, 1), AlertState::Shelved, "same compartment as the fire");
    assert_eq!(state(&m, 2), AlertState::Unacknowledged);
    assert_eq!(m.raised_in_window(v(1), 0, 60.0), 2, "shelved-at-birth items are not annunciated");
    // Fire out: the shelved item returns at shelve expiry, not before.
    step(&mut m, &[gun, other], 10);
    assert_eq!(state(&m, 1), AlertState::Shelved);
    step(&mut m, &[gun, other], 300 * S);
    assert_eq!(state(&m, 1), AlertState::Unacknowledged);
    // An Emergency suppresses every lower alert of the vessel.
    step(&mut m, &[gun, other, c(1, AlertKind::CapsizeImminent)], 300 * S + 1);
    assert_eq!(state(&m, 1), AlertState::Shelved);
    assert_eq!(state(&m, 2), AlertState::Shelved);
}

#[test]
fn flood_mode_combines_and_exits() {
    let mut m = AlertManager::default();
    let mut conds = Vec::new();
    for i in 0..11u16 { conds.push(c(1, AlertKind::ConduitCut { conduit: i })); }
    let out = step(&mut m, &conds, 0);
    assert!(out.contains(&AlertChange::Flood(v(1), true)));
    assert!(m.flood.contains(&v(1)));
    assert!(m.alerts.iter().all(|a| a.state == AlertState::LogOnly), "Cautions go to the log in flood mode");
    // Warnings combine per subsystem; Alarms never combine.
    conds.push(c(1, AlertKind::PropulsionLost));
    conds.push(c(1, AlertKind::Fire { compartment: CompartmentId(0) }));
    step(&mut m, &conds, 1);
    conds.push(c(1, AlertKind::Fire { compartment: CompartmentId(1) }));
    conds.push(c(1, AlertKind::FuelLow { fraction: 0.01 })); // other subsystem: shown
    // Two Power Warnings in one tick: the first in key order (power shortage) shows, the buffer one combines.
    conds.push(c(1, AlertKind::PartOnBuffer { slot: PartSlot(1), seconds_left: 2.0 }));
    conds.push(c(1, AlertKind::PowerShortage { shed: 5 }));
    step(&mut m, &conds, 2);
    let st = |k: AlertKind| m.alerts.iter().find(|a| a.kind == k).map(|a| a.state).unwrap();
    assert_eq!(st(AlertKind::Fire { compartment: CompartmentId(1) }), AlertState::Unacknowledged);
    assert_eq!(st(AlertKind::PowerShortage { shed: 5 }), AlertState::Unacknowledged);
    assert_eq!(st(AlertKind::PartOnBuffer { slot: PartSlot(1), seconds_left: 2.0 }), AlertState::LogOnly);
    assert_eq!(st(AlertKind::FuelLow { fraction: 0.01 }), AlertState::Unacknowledged);
    let out = step(&mut m, &conds, 10 * 60 * S + 5);
    assert!(out.contains(&AlertChange::Flood(v(1), false)));
}

#[test]
fn silence_and_window_math() {
    let mut m = AlertManager::default();
    let mut out = Vec::new();
    m.silence(v(1), 100, &mut out);
    assert_eq!(out, vec![AlertChange::Silenced(v(1), 100 + 30 * S)]);
    step(&mut m, &[], 100 + 30 * S);
    assert!(m.silenced_until.is_empty());
    assert_eq!(m.raised_in_window(v(1), 0, f64::NAN), 0);
}

#[test]
fn restored_without_bookkeeping_rebuilds_it() {
    let mut m = AlertManager::default();
    step(&mut m, &[c(1, AlertKind::EsmWarning)], 0);
    let mut restored = m.clone();
    restored.meta.clear();
    restored.live.clear();
    let out = step(&mut restored, &[c(1, AlertKind::EsmWarning)], 1);
    assert!(out.is_empty());
    assert_eq!(restored.alerts.len(), 1);
}

#[test]
fn hostile_contact_announces_then_moves_to_the_log() {
    let mut m = AlertManager::default();
    let h = c(1, AlertKind::NewHostileTrack { track: TrackId(4) });
    step(&mut m, &[h], 0);
    assert_eq!(state(&m, 0), AlertState::Unacknowledged);
    step(&mut m, &[h], NEW_TRACK_S * S);
    assert_eq!(state(&m, 0), AlertState::LogOnly);
    step(&mut m, &[h], NEW_TRACK_S * S + 100);
    assert_eq!(m.alerts.len(), 1, "still live: not raised again");
    assert!(m.for_vessel(v(1)).is_empty(), "log-only items reach the log through events, not the strip list");
    step(&mut m, &[], NEW_TRACK_S * S + 101);
    assert!(m.for_vessel(v(1)).is_empty(), "rectified history is not repeated");
    assert_eq!(m.raised_in_window(v(1), NEW_TRACK_S * S + 101, 1200.0), 1);
}

#[test]
fn cleared_alarms_go_to_the_log_after_their_hold() {
    let mut m = AlertManager::default();
    let w = c(1, AlertKind::WeaponInbound { track: TrackId(9) });
    step(&mut m, &[w], 0);
    let out = step(&mut m, &[], 10);
    assert_eq!(out, vec![AlertChange::State(AlertId(0), AlertState::Cleared)]);
    assert_eq!(m.for_vessel(v(1)).len(), 1, "Cleared shows on the strip");
    assert!(step(&mut m, &[], 10 + CLEARED_HOLD_S * S - 1).is_empty());
    let out = step(&mut m, &[], 10 + CLEARED_HOLD_S * S);
    assert_eq!(out, vec![AlertChange::State(AlertId(0), AlertState::LogOnly)]);
    assert!(m.for_vessel(v(1)).is_empty());
    assert_eq!(m.alerts.len(), 1, "the log keeps it");
}

#[test]
fn a_long_battles_replicated_alerts_stay_small() {
    // Twenty minutes of a heavy battle, as the reference soak saw it: five fresh inbound weapons a second
    // (each one an Alarm, rectified unacknowledged 4 s later: nobody acknowledges), and 100 Hostile contacts
    // that live the whole time. Before Cleared alarms went to the log the list held every one of them
    // (852 after three minutes of the soak, about 14.8 kB on the wire).
    let mut m = AlertManager::default();
    let hostiles: Vec<Condition> = (0..100).map(|t| c(1, AlertKind::NewHostileTrack { track: TrackId(10_000 + t) })).collect();
    let mut worst = 0usize;
    for sec in 0..1200u64 {
        for sub in 0..S {
            let tick = sec * S + sub;
            let mut conds = hostiles.clone();
            for k in sec.saturating_sub(3) * 5..=sec * 5 + 4 { conds.push(c(1, AlertKind::WeaponInbound { track: TrackId(k as u32) })); }
            step(&mut m, &conds, tick);
        }
        let list = m.for_vessel(v(1));
        // Sized as net's frames carry it: compact varint bincode per element.
        use bincode::Options;
        let opts = bincode::DefaultOptions::new().with_varint_encoding();
        let bytes: usize = list.iter().map(|a| opts.serialized_size(a).unwrap() as usize + 1).sum();
        worst = worst.max(bytes);
    }
    // One snapshot interval at the 256 kbps start cap is 1,066 bytes.
    assert!(worst < 1_066, "alerts section peaked at {worst} bytes");
    assert!(m.alerts.len() > 6_000, "the log keeps every alert");
}
