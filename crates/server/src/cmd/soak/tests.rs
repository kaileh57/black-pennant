//! Tests of unit `server-soak`: gate arithmetic at its boundaries, the scene fitting, the aim bot's geometry,
//! and short end-to-end soaks on a small generated operation (the reference scenes run from the release
//! binary).
use super::*;
use content::campaign::{DifficultySliders, MissionType};
use content::enums::Act;
use sim_core::math::DQuat;

fn client(slot: u8) -> ClientReport {
    ClientReport { slot, frames_rendered: 1000, traffic: Traffic { snapshot_bytes: 1000, vessel_bytes: 200, vessel_updates: 10, event_bytes: 500, projectile_bytes: 100, session_bytes: 0, bulk_bytes: 0, p95_kbps: 200.0 }, ..Default::default() }
}
fn report(clients: Vec<ClientReport>) -> SoakReport { SoakReport { sorties: 1, ticks: 100, minutes: 1.0, clients, ..Default::default() } }
fn verdict(gates: &[(String, bool, f64, f64)], name: &str) -> Option<bool> { gates.iter().find(|g| g.0 == name).map(|g| g.1) }

#[test]
fn bandwidth_and_extrapolation_gates_at_their_limits() {
    let mut a = client(0);
    a.frames_extrapolated = 10; // exactly 1%
    let g = evaluate(&report(vec![a.clone()]), LinkPreset::Clean);
    assert_eq!(verdict(&g, "extrapolated_frames"), Some(true));
    assert_eq!(verdict(&g, "vessel_bytes_per_update"), Some(true), "exactly 20 bytes");
    assert_eq!(verdict(&g, "client_p95_kbps"), Some(true));
    assert_eq!(verdict(&g, "projectile_share"), Some(true));
    assert_eq!(verdict(&g, "sortie_ran"), Some(true));
    a.frames_extrapolated = 11;
    a.traffic.vessel_bytes = 201;
    a.traffic.p95_kbps = 256.5;
    a.traffic.projectile_bytes = 600;
    let g = evaluate(&report(vec![a.clone()]), LinkPreset::Clean);
    for name in ["extrapolated_frames", "vessel_bytes_per_update", "client_p95_kbps", "projectile_share"] { assert_eq!(verdict(&g, name), Some(false), "{name}"); }
    // Nothing measured is a failure, not a pass.
    let empty = ClientReport { slot: 0, ..Default::default() };
    let g = evaluate(&SoakReport { minutes: 1.0, clients: vec![empty], ..Default::default() }, LinkPreset::Clean);
    for name in ["sortie_ran", "extrapolated_frames", "vessel_bytes_per_update", "client_p95_kbps"] { assert_eq!(verdict(&g, name), Some(false), "{name}"); }
    // A bot that never joined does not count.
    let mut never = client(u8::MAX);
    never.frames_extrapolated = 1000;
    assert_eq!(verdict(&evaluate(&report(vec![client(0), never]), LinkPreset::Clean), "extrapolated_frames"), Some(true));
}

#[test]
fn link_gates_by_preset() {
    let mut a = client(0);
    a.max_own_correction_m = 0.5;
    let mut r = report(vec![a.clone(), client(1)]);
    r.baseline_hit_rate = Some(0.4);
    r.baseline_shots = 100;
    r.clients[1].shots = 100;
    r.clients[1].hits = 36; // exactly 90% of the baseline
    let g = evaluate(&r, LinkPreset::Gate);
    assert_eq!(verdict(&g, "own_hull_correction_m"), Some(true));
    assert_eq!(verdict(&g, "manual_aim_hit_ratio"), Some(true));
    r.clients[1].hits = 35;
    r.clients[0].max_own_correction_m = 0.51;
    let g = evaluate(&r, LinkPreset::Gate);
    assert_eq!(verdict(&g, "manual_aim_hit_ratio"), Some(false));
    assert_eq!(verdict(&g, "own_hull_correction_m"), Some(false));
    r.clients[1].shots = MIN_AIM_SHOTS - 1;
    r.clients[1].hits = MIN_AIM_SHOTS - 1;
    assert_eq!(verdict(&evaluate(&r, LinkPreset::Gate), "manual_aim_hit_ratio"), Some(false), "too few shots to judge");
    assert_eq!(verdict(&evaluate(&r, LinkPreset::Clean), "own_hull_correction_m"), None);
    // Drops.
    let mut d = report(vec![client(0), client(1)]);
    d.clients[0].dropped_at_s = Some(20.0);
    d.clients[1].dropped_at_s = Some(12.0);
    assert_eq!(verdict(&evaluate(&d, LinkPreset::DropPing), "dropped_within_s"), Some(true));
    assert_eq!(verdict(&evaluate(&d, LinkPreset::Tolerated), "nobody_dropped"), Some(false));
    d.clients[0].dropped_at_s = Some(20.1);
    assert_eq!(verdict(&evaluate(&d, LinkPreset::DropLoss), "dropped_within_s"), Some(false));
    d.clients[0].dropped_at_s = None;
    assert_eq!(verdict(&evaluate(&d, LinkPreset::DropLoss), "dropped_within_s"), Some(false), "a client never dropped fails");
    assert_eq!(verdict(&evaluate(&report(vec![client(0)]), LinkPreset::Tolerated), "nobody_dropped"), Some(true));
    // Upload saturation.
    let mut u = report(vec![client(0)]);
    assert_eq!(verdict(&evaluate(&u, LinkPreset::Upload1Mbps), "upload_loss_under_5pct_s"), Some(false), "never settled");
    u.loss_settled_s = Some(20.0);
    assert_eq!(verdict(&evaluate(&u, LinkPreset::Upload1Mbps), "upload_loss_under_5pct_s"), Some(true));
}

#[test]
fn ladder_gates() {
    let mut r = report(vec![client(0)]);
    assert!(verdict(&evaluate(&r, LinkPreset::Clean), "backlog_ticks").is_none(), "no ladder gate without load");
    r.synthetic_tick_ms = 12.0;
    r.max_backlog_ticks = 30;
    r.backlog_recovery_s = vec![1.0, 5.0];
    r.ladder_steps = vec![(10.0, 2, 0, true), (40.0, 0, 0, false), (70.0, 2, 0, true)];
    r.load_dropped_at_s = Some(50.0);
    r.rung0_after_drop_s = Some(30.0);
    let g = evaluate(&r, LinkPreset::Clean);
    for name in ["backlog_ticks", "backlog_recovery_s", "down_then_up_gap_s", "rung0_after_load_drop_s"] { assert_eq!(verdict(&g, name), Some(true), "{name}"); }
    r.max_backlog_ticks = 31;
    r.backlog_recovery_s.push(5.5);
    r.ladder_steps.push((80.0, 0, 0, false));
    r.ladder_steps.push((109.0, 2, 0, true));
    r.rung0_after_drop_s = None;
    let g = evaluate(&r, LinkPreset::Clean);
    for name in ["backlog_ticks", "backlog_recovery_s", "down_then_up_gap_s", "rung0_after_load_drop_s"] { assert_eq!(verdict(&g, name), Some(false), "{name}"); }
    assert_eq!(down_then_up_gap(&r.ladder_steps), Some(29.0));
    assert_eq!(down_then_up_gap(&[(1.0, 2, 0, true)]), None);
}

#[test]
fn series_helpers() {
    let loss = [(10.0, 0.2), (11.0, 0.1), (12.0, 0.04), (13.0, 0.06), (14.0, 0.01), (15.0, 0.0), (25.0, 0.02), (26.0, 0.03)];
    assert_eq!(settled_after(&loss, 10.0, 0.05, 10.0), Some(4.0));
    assert_eq!(settled_after(&loss, 10.0, 0.05, 30.0), None, "not observed long enough");
    let backlog = [(0.0, 0u32), (5.0, 12), (7.5, 2), (20.0, 40)];
    assert_eq!(recovery_after(&backlog, 6.0, 100.0), 1.5);
    assert_eq!(recovery_after(&backlog, 8.0, 100.0), 0.0);
    assert_eq!(recovery_after(&backlog, 21.0, 100.0), 79.0);
    let (p95, p99) = tick_percentiles(&mut (1..=100).rev().map(|i| i as f32).collect::<Vec<_>>());
    assert_eq!((p95, p99), (95.0, 99.0));
    assert_eq!(tick_percentiles(&mut []), (0.0, 0.0));
    assert_eq!(upload_for(2), 1_000);
    assert_eq!(upload_for(7), 1_992);
    assert_eq!(sim_systems::modes::player_limit(content::enums::GameMode::Sandbox, false, upload_for(7)), 8);
}

#[test]
fn presets_and_policies() {
    let (h, c) = profile(LinkPreset::Gate);
    assert_eq!(h, c);
    assert_eq!((h.latency_ms, h.jitter_ms), (75.0, 30.0));
    assert!((h.loss.mean() - 0.05).abs() < 1e-6);
    let (h, c) = profile(LinkPreset::Upload1Mbps);
    assert_eq!((h.upstream_kbps, c.upstream_kbps), (1_000, 0));
    assert_eq!(profile(LinkPreset::DropPing).0.latency_ms, 300.0);
    assert!((profile(LinkPreset::DropLoss).1.loss.mean() - 0.25).abs() < 1e-6);
    assert!(is_clean(&profile(LinkPreset::Clean).0));
    assert_eq!(policy_for(0), BotPolicy::Officers);
    assert_eq!(policy_for(1), BotPolicy::AimBot { reaction_ms: 250, min_target_kn: 20.0 });
}

#[test]
fn the_aim_bot_leads_its_target_from_what_it_saw() {
    let gun = GunInfo { slot: PartSlot(3), pos_local: DVec3::new(10.0, 5.0, 0.0), muzzle_mps: 800.0, range_m: 8_000.0 };
    let pose = Pose::new(DVec3::ZERO, DQuat::IDENTITY);
    // A target 4 km off along +Z, crossing at 12 m/s along +X, seen 0.25 s ago; a slow one closer.
    let seen = [(DVec3::new(0.0, 0.0, 4_000.0), DVec3::new(12.0, 0.0, 0.0)), (DVec3::new(0.0, 0.0, 1_000.0), DVec3::new(2.0, 0.0, 0.0)), (DVec3::new(0.0, 0.0, 9_000.0), DVec3::new(15.0, 0.0, 0.0))];
    let d = aim_direction(&pose, &gun, &seen, 0.25, 20.0 * KN).expect("a target");
    assert!((d.length() - 1.0).abs() < 1e-9);
    assert!(d.z > 0.99 && d.x > 0.0, "leads toward +X: {d:?}");
    assert!(d.y < 0.0, "aimed at the sea below the muzzle");
    // Lead: about (0.25 + 4000 / 640) s at 12 m/s, so about 78 m at 4 km.
    let lead = d.x / d.z * 4_000.0;
    assert!((lead - 88.0).abs() < 20.0, "{lead}");
    assert!(aim_direction(&pose, &gun, &seen[1..2], 0.25, 20.0 * KN).is_none(), "too slow");
    assert!(aim_direction(&pose, &GunInfo { range_m: 6_000.0, ..gun }, &seen, 0.25, 20.0 * KN).is_none(), "beyond 60% of the gun's range");
    // In the hull frame: a hull turned 90 degrees sees the same world point on another local axis.
    let turned = Pose::from_heading(DVec3::ZERO, core::f64::consts::FRAC_PI_2);
    let l = aim_direction(&turned, &GunInfo { pos_local: DVec3::ZERO, ..gun }, &seen, 0.25, 20.0 * KN).unwrap();
    let back = (turned.rot * l).normalize();
    assert!(back.z > 0.99, "{back:?}");
}

fn data() -> crate::runtime::Data { crate::runtime::load_data().expect("embedded data") }

fn generated(data: &crate::runtime::Data, seed: u64) -> SortieConfig {
    let td = sim_systems::theatre::TheatreData { campaign: data.campaign.clone(), catalogue: data.catalogue.clone() };
    sim_systems::theatre::opgen::generate_standalone(MissionType::HunterGroupSurvival, Act::Act1, 1, seed, &DifficultySliders::default(), None, &td).expect("an Act 1 operation").config
}

#[test]
fn fit_players_seats_every_bot_on_its_own_hull() {
    let d = data();
    let base = generated(&d, 1);
    let hulls_before = base.vessels.len();
    let seats: Vec<(PlayerSlot, u64, String)> = (0..3).map(|i| (PlayerSlot(i), 100 + u64::from(i), format!("Bot {i}"))).collect();
    let mut c = base.clone();
    fit_players(&mut c, &seats);
    assert_eq!(c.players.len(), 3);
    let vessels: std::collections::BTreeSet<u16> = c.players.iter().filter_map(|p| p.vessel).collect();
    assert_eq!(vessels.len(), 3, "one hull each");
    assert_eq!(c.vessels.len(), hulls_before + 2);
    assert!(c.players.iter().all(|p| p.human && p.side == base.players[0].side));
    assert_eq!(c.players[2].client_id, 102);
    let (a, b) = (c.vessels[c.players[1].vessel.unwrap() as usize].pos, c.vessels[c.players[2].vessel.unwrap() as usize].pos);
    assert!(((a - b).length() - EXTRA_HULL_SPACING_M).abs() < 1e-6);
    let sim = sim_systems::Sim::new(c, crate::runtime::sim_deps(&d)).expect("the fitted scene starts");
    assert_eq!(sim.players().len(), 3);
    assert!(sim.ships().of_player(PlayerSlot(2)).is_some());
}

fn args(link: LinkPreset, minutes: f64, clients: u8) -> SoakArgs {
    SoakArgs { minutes, clients, link, scene: sim_systems::editor::trials::ReferenceScene::ReferenceBattle, seed: 3, udp: false, synthetic_tick_ms: 0.0, out: None }
}

/// Two bots over the in-memory transport on a clean link: both seated, rendering, measured; the run repeats
/// exactly for its seed on the manual clock.
#[test]
fn a_short_clean_soak_measures_both_bots_and_repeats() {
    let d = data();
    let pool = crate::runtime::sim_pool(3).unwrap();
    let make = |seed: u64| generated(&d, seed);
    let a = soak(&args(LinkPreset::Clean, 0.25, 2), &make, &d, &pool).expect("runs");
    assert_eq!(a.sorties, 1);
    assert!(a.ticks >= 850, "15 s of sortie at 60 Hz: {}", a.ticks);
    assert_eq!(a.clients.len(), 2);
    for c in &a.clients {
        assert!(c.slot < 2);
        assert!(c.frames_rendered > 600, "{c:?}");
        assert!(c.traffic.vessel_updates > 0 && c.traffic.snapshot_bytes > 0);
        assert!(c.dropped_at_s.is_none());
        assert!(c.max_own_correction_m < 0.5, "{}", c.max_own_correction_m);
    }
    assert_eq!(verdict(&a.gates, "sortie_ran"), Some(true));
    assert_eq!(verdict(&a.gates, "nobody_dropped"), Some(true));
    assert_eq!(verdict(&a.gates, "extrapolated_frames"), Some(true), "{:?}", a.gates);
    let b = soak(&args(LinkPreset::Clean, 0.25, 2), &make, &d, &pool).expect("runs");
    let strip = |r: &SoakReport| r.clients.iter().map(|c| (c.slot, c.traffic.snapshot_bytes, c.traffic.vessel_updates, c.frames_rendered, c.frames_extrapolated)).collect::<Vec<_>>();
    assert_eq!((a.ticks, strip(&a)), (b.ticks, strip(&b)), "the manual clock repeats the run");
}

/// L1036: a bot at 600 ms RTT is warned and dropped within 20 s.
#[test]
fn a_600_ms_link_is_dropped_within_20_s() {
    let d = data();
    let pool = crate::runtime::sim_pool(3).unwrap();
    let make = |seed: u64| generated(&d, seed);
    let r = soak(&args(LinkPreset::DropPing, 0.5, 1), &make, &d, &pool).expect("runs");
    let c = &r.clients[0];
    assert!(c.dropped_at_s.is_some_and(|t| t <= 20.0), "{c:?}");
    assert!(c.warnings >= 1, "warned first");
    assert_eq!(verdict(&r.gates, "dropped_within_s"), Some(true));
}

/// A synthetic tick cost makes the ladder step, and the load drop is recorded for the rung-0 gate.
#[test]
fn synthetic_cost_drives_the_ladder() {
    let d = data();
    let pool = crate::runtime::sim_pool(3).unwrap();
    let make = |seed: u64| generated(&d, seed);
    let r = soak(&SoakArgs { synthetic_tick_ms: 12.0, ..args(LinkPreset::Clean, 0.5, 1) }, &make, &d, &pool).expect("runs");
    assert!(r.ladder_steps.iter().any(|s| s.3), "the ladder stepped up: {:?}", r.ladder_steps);
    assert!(r.tick_p95_ms >= 12.0);
    assert!(r.load_dropped_at_s.is_some());
    assert!(verdict(&r.gates, "backlog_ticks").is_some());
}

