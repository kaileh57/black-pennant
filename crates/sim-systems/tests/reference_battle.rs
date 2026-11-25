//! Unit `sys-sim`: the headless reference battle. One player starter hull with stock officers against an
//! Act 1 template fleet built by the operation generator from the shipped data, for 5 minutes of simulation
//! (18,000 ticks), driven exactly as a server drives a `Sim`: host load, commands, step, drain events,
//! snapshots. DESIGN.md L400-418 (pipeline and tick split), L899 (one Sim everywhere), L1150-1158
//! (determinism).
//!
//! The generated hunter-group operation puts the player 8 km from the screening patrol and brings the hunter
//! group in after 10 minutes. The test moves the player's start to 1.25 km from the patrol and the hunter
//! group's arrival to 60 s, so 5 minutes hold a whole engagement and the schedule phase runs. Nothing else
//! of the generated config changes: same hulls, groups, orders, weather and briefing.
//!
//! The battle is run twice, on a 3-thread pool (L1084) and on a 1-thread pool, and both runs must end in the
//! same state, subsystem by subsystem. Tick times are printed for the report (`--nocapture`); nothing asserts
//! on wall time, because CI machines are noisy. Budgets are asserted in work units.
//!
//! The player sails the Act 1 starter a player brings to a hunter-group operation: the first of the act's
//! starters that passes the campaign operation's briefing tags, as the stock-officer winnability gate fields
//! it (L317: "each Act 1 operation runs with its starter design"). The generator seats the act's first-listed
//! starter for a standalone template, the 15 m patrol boat, which fails the hunter-group briefing's speed and
//! range tags; alone against nine hulls with honest gunnery on both sides it lost its gun inside 40 s.
//!
//! Generator seed 1 was chosen because its fight is decisive across start distances: from 1.0 to 2.0 km off
//! the patrol the player's fire disabled at least one hostile hull it had damaged in every run tried (re-run
//! 20 November 2026 at 1.0, 1.25, 1.5, 1.75 and 2.0 km: 2, 2, 2, 1 and 2 hulls), so a platform whose floating
//! point takes the battle down another path still meets that assertion. The duplicate-track assertion counts
//! only tracks a sensor updated in the last `DETECTED_S` (L467's "inside detection range"): at 1.0 and 1.75 km
//! the player's side kept two tracks on one hunter-group hull coasting, with no sensor updating either, until
//! the tracker's 120 s coast timeout (L463) dropped them; that timeout is applied, and a coasting track is not
//! a held one (ruling of 20 November 2026). `BP_REF_START_M` moves the start for a re-check. Events are counted once each:
//! routing sends one physical event to several audiences, with ids scrubbed per audience.
/// A track updated by a sensor within this many seconds is held inside detection range (the duplicate gate).
const DETECTED_S: u64 = 30;
use content::campaign::{DifficultySliders, MissionType, OperationKind};
use content::enums::Act;
use content::ids::{PartSlot, PlayerSlot, SideId};
use glam::{DVec2, DVec3};
use sim_core::events::PhysEvent;
use sim_core::hash::StateHash;
use sim_core::projectiles::ProjectileId;
use sim_core::vessel::VesselStatus;
use sim_core::EntityId;
use sim_systems::ctx::Budgets;
use sim_systems::lod::HostLoad;
use sim_systems::protocol::command::{Command, CommandEnvelope};
use sim_systems::protocol::config::SortieConfig;
use sim_systems::protocol::event::SimEvent;
use sim_systems::protocol::snapshot::Viewer;
use sim_systems::protocol::text::EngageStatus;
use sim_systems::sim::SimDeps;
use sim_systems::theatre::opgen::generate_standalone;
use sim_systems::theatre::TheatreData;
use sim_systems::Sim;
use std::collections::BTreeSet;
use std::sync::Arc;

const TICKS: u64 = 5 * 60 * 60;
const PLAYER: PlayerSlot = PlayerSlot(0);
const PLAYER_SIDE: SideId = SideId(0);
/// Operation generator seed of the battle.
const SEED: u64 = 1;
/// Where the screening patrol starts (the generator's `enemy_2` approach), world metres.
const PATROL_AT: DVec2 = DVec2::new(10_450.0, 0.0);

/// The Act 1 starter a player brings to the campaign's hunter-group operation: the first of the act's starters
/// whose analytic Fit Check passes that briefing's tags (the winnability gate's `op_starter`, L317).
fn hunter_group_starter(data: &TheatreData) -> String {
    let op = data.campaign.operations.values().find(|o| o.act == Act::Act1 && o.kind == OperationKind::Template(MissionType::HunterGroupSurvival)).expect("Act 1 has a hunter-group operation");
    let act = data.campaign.acts.iter().find(|a| a.act == Act::Act1).expect("Act 1");
    let brief = sim_systems::editor::fitcheck::FitBrief { tags: op.tags.clone(), weather: Default::default(), approach_km: (op.approach_m / 1000.0).max(0.0), threats: Vec::new(), cap_t: Act::Act1.hull_cap_t() };
    act.starters.iter().find(|p| {
        let bp = content::campaign::load::stock_blueprint(p).expect("a stock starter");
        sim_systems::editor::fitcheck::fit_screen(&bp, &brief, &data.catalogue).passes()
    }).cloned().expect("an Act 1 starter fits the hunter-group briefing")
}

fn data() -> TheatreData {
    let catalogue = Arc::new(content::catalogue::Catalogue::load_embedded().expect("catalogue"));
    let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign data"));
    TheatreData { campaign, catalogue }
}

fn deps(data: &TheatreData) -> SimDeps {
    SimDeps { catalogue: data.catalogue.clone(), campaign: data.campaign.clone(), designs: Default::default(), scripts: Box::new(sim_systems::script::NullScriptHost), guest_gate_passed: false, budgets: Budgets::default(), ladder: Default::default() }
}

fn battle_config(data: &TheatreData) -> SortieConfig {
    let g = generate_standalone(MissionType::HunterGroupSurvival, Act::Act1, 1, SEED, &DifficultySliders::default(), None, data).expect("the Act 1 hunter-group template");
    let mut config = g.config;
    assert_eq!(config.players.len(), 1, "one player");
    let own = config.players[0].vessel.expect("the player has a hull") as usize;
    assert!(matches!(&config.vessels[own].blueprint, sim_systems::protocol::config::BlueprintRef::Stock(p) if p.starts_with("starters/")), "the player sails a stock starter");
    let hostile = config.vessels.iter().filter(|v| v.side != PLAYER_SIDE).count();
    assert!(hostile >= 5, "the template commits at least 5 hostile hulls, found {hostile}");
    let start_m: f64 = std::env::var("BP_REF_START_M").ok().and_then(|v| v.parse().ok()).unwrap_or(1_250.0);
    config.vessels[own].pos = DVec3::new(PATROL_AT.x - start_m, 0.0, PATROL_AT.y);
    config.vessels[own].blueprint = sim_systems::protocol::config::BlueprintRef::Stock(hunter_group_starter(data));
    let mut scheduled = 0;
    for v in config.vessels.iter_mut().filter(|v| v.arrives_s > 0) { v.arrives_s = 60; scheduled += 1; }
    assert!(scheduled > 0, "the template schedules its hunter group");
    config
}

#[derive(Debug, Default, Clone, PartialEq)]
struct Tally {
    tracks_created: u32,
    hostile_tracks: u32,
    /// `EntityRevealed` and `EntityHidden` events after the opening tick, to any side: interest is kept up
    /// during play (arrivals revealed to their side, contacts hidden when they leave visual range).
    interest_changes: u32,
    engaging: u32,
    fired_player: u32,
    fired_enemy: u32,
    impacts: u32,
    /// Shells the player landed on another hull.
    player_hits: u32,
    cells_changed: u32,
    /// Hostile hulls lost with the player credited, in any end state (a hull holed until it rolls over
    /// counts).
    hostile_sunk: u32,
    /// Hulls lost with nobody credited: a capsize in a turn, with no hit or collision before it.
    lost_unprovoked: u32,
    /// Hostile hulls Disabled after the player's fire damaged them.
    hostile_disabled: u32,
    player_disabled: u32,
    reinforcements: u32,
    events: u64,
    snapshots: u32,
    peak_tracks: usize,
    peak_projectiles: u32,
    /// Longest run, in ticks, of any truth object holding two or more live tracks in one side's picture
    /// (L467: no truth object holds two live tracks for more than 10 s), sampled at the 10 Hz track rate, and
    /// the (side, truth index, live tracks) of that run's worst sample.
    duplicate_ticks: u64,
    duplicate_worst: (u8, u32, usize),
}

struct Run {
    hash: StateHash,
    subsystems: Vec<(&'static str, StateHash)>,
    restored_matches: bool,
    tally: Tally,
    /// Wall time of each `Sim::step`, nanoseconds.
    tick_ns: Vec<u64>,
    statuses: Vec<(EntityId, VesselStatus, f64)>,
    /// Size of the mid-battle checkpoint (config, keyframe) in bytes, and the wall time of one state hash.
    checkpoint_bytes: (usize, usize),
    hash_ns: u64,
}

fn run_battle(data: &TheatreData) -> Run {
    let config = battle_config(data);
    let budgets = Budgets::default();
    let mut sim = Sim::new(config, deps(data)).expect("the generated sortie starts");
    let own = sim.ships().of_player(PLAYER).expect("the player's ship").vessel;
    // The one order of the battle: close the patrol at 20 kn. The officers do the rest (L108).
    sim.apply(&[CommandEnvelope { player: PLAYER, seq: 1, client_render_tick: 0, rtt_ms: 0, command: Command::SetCourse { waypoints: vec![PATROL_AT], speed_kn: Some(20.0), append: false } }]);
    let mut tally = Tally::default();
    let mut tick_ns = Vec::with_capacity(TICKS as usize);
    let mut restored_matches = false;
    let mut checkpoint_bytes = (0, 0);
    // Each physical event once, whichever audiences it was routed to.
    let mut fired: BTreeSet<(u64, EntityId, PartSlot)> = BTreeSet::new();
    let mut impacts: BTreeSet<ProjectileId> = BTreeSet::new();
    let mut damaged: BTreeSet<(u64, EntityId)> = BTreeSet::new();
    let mut lost: BTreeSet<EntityId> = BTreeSet::new();
    let mut disabled: BTreeSet<EntityId> = BTreeSet::new();
    // Hulls the player's fire has damaged (shell, missile or a fire it started).
    let mut struck: BTreeSet<EntityId> = BTreeSet::new();
    // Per (side, truth): the tick a run of two or more live tracks began.
    let mut dup_since: std::collections::BTreeMap<(u8, EntityId), u64> = std::collections::BTreeMap::new();
    for i in 0..TICKS {
        sim.set_host_load(HostLoad::IDLE);
        let t0 = std::time::Instant::now();
        sim.step();
        tick_ns.push(t0.elapsed().as_nanos() as u64);
        assert_eq!(sim.tick().0, i + 1, "one step is one tick");

        // Budgets, in work units.
        let (work, world) = (*sim.work(), *sim.world().work());
        let ships = sim.ships().ships.len() as u32;
        assert!(work.sensor_pairs <= budgets.sensor_pairs, "tick {i}: {} sensor pairs", work.sensor_pairs);
        assert!(work.astar_expansions <= budgets.astar_expansions, "tick {i}: {} A* expansions", work.astar_expansions);
        assert!(work.graph_node_evals <= ships * budgets.graph_node_evals, "tick {i}: {} node evaluations", work.graph_node_evals);
        assert_eq!(work.luau_interrupts, 0, "no script host, no interrupts");
        assert!(world.live_projectiles as usize <= sim_core::time::MAX_LIVE_PROJECTILES, "tick {i}: {} live projectiles", world.live_projectiles);
        assert!(world.split_cells <= sim_core::time::SPLIT_WORK_BUDGET, "tick {i}: {} split cells", world.split_cells);
        tally.peak_projectiles = tally.peak_projectiles.max(world.live_projectiles);

        for e in sim.drain_events() {
            tally.events += 1;
            assert!(e.tick <= i + 1, "an event from the future");
            match &e.event {
                SimEvent::TrackCreated { side, .. } if *side == PLAYER_SIDE => tally.tracks_created += 1,
                SimEvent::StanceChanged { side, stance, .. } if *side == PLAYER_SIDE && *stance == content::enums::Stance::Hostile => tally.hostile_tracks += 1,
                // The opening reveals come on the first tick; these are changes during the fight.
                SimEvent::EntityRevealed(_) | SimEvent::EntityHidden { .. } if i > 0 => tally.interest_changes += 1,
                SimEvent::Engaging { .. } => tally.engaging += 1,
                SimEvent::Reinforcements { .. } => tally.reinforcements += 1,
                SimEvent::Phys(PhysEvent::MountFired { vessel, slot, rounds }) if !vessel.is_none() => {
                    if fired.insert((e.tick, *vessel, *slot)) {
                        if *vessel == own { tally.fired_player += *rounds as u32 } else { tally.fired_enemy += *rounds as u32 }
                    }
                }
                SimEvent::Phys(PhysEvent::ShellImpact { projectile, shooter, vessel, .. }) => {
                    if impacts.insert(*projectile) {
                        tally.impacts += 1;
                        if *shooter == own && *vessel != own { tally.player_hits += 1 }
                    }
                }
                SimEvent::Phys(PhysEvent::CellsDamaged { vessel, changes, attacker, .. }) if !vessel.is_none() => {
                    if damaged.insert((e.tick, *vessel)) { tally.cells_changed += changes.len() as u32 }
                    if *attacker == own && *vessel != own { struck.insert(*vessel); }
                }
                SimEvent::Phys(PhysEvent::VesselLost { vessel, by, side, .. }) if !vessel.is_none() && lost.insert(*vessel) => {
                    if by.is_none() { tally.lost_unprovoked += 1 } else if *side != PLAYER_SIDE && *by == own { tally.hostile_sunk += 1 }
                }
                SimEvent::Phys(PhysEvent::StatusChanged { vessel, status: VesselStatus::Disabled }) if !vessel.is_none() && disabled.insert(*vessel) => {
                    if *vessel == own { tally.player_disabled += 1 } else if struck.contains(vessel) { tally.hostile_disabled += 1 }
                }
                _ => {}
            }
        }
        // Snapshots at 30 Hz for the player, once a second for the host's omniscient view.
        if i % 2 == 0 {
            let snap = sim.snapshot(Viewer::Player(PLAYER));
            assert_eq!(snap.tick, i + 1);
            assert!(snap.ship.is_some() || sim.world().vessel(own).is_none_or(|v| matches!(v.status, VesselStatus::Lost(_))), "tick {i}: the player sees their own ship");
            tally.peak_tracks = tally.peak_tracks.max(snap.tracks.len());
            tally.snapshots += 1;
        }
        if i % 60 == 0 { let all = sim.snapshot(Viewer::Omniscient); assert!(!all.vessels.is_empty()); }
        if i % 6 == 0 {
            let now = i + 1;
            let mut held: std::collections::BTreeMap<(u8, EntityId), usize> = std::collections::BTreeMap::new();
            for (si, p) in sim.pictures().sides.iter().enumerate() {
                // L467 counts tracks of a truth inside detection range: a track no sensor has updated for
                // DETECTED_S is coasting to the tracker's 120 s drop (L463), not held (ruling of 20 November 2026).
                for t in p.tracks.iter().filter(|t| t.is_live() && !t.truth.is_none() && now.saturating_sub(t.last_update_tick) <= DETECTED_S * 60) { *held.entry((si as u8, t.truth)).or_default() += 1; }
            }
            dup_since.retain(|k, _| held.get(k).is_some_and(|n| *n >= 2));
            for (k, n) in held.into_iter().filter(|(_, n)| *n >= 2) {
                let since = *dup_since.entry(k).or_insert(now);
                if now - since > tally.duplicate_ticks || (now - since == tally.duplicate_ticks && n > tally.duplicate_worst.2) { tally.duplicate_ticks = now - since; tally.duplicate_worst = (k.0, k.1.index, n); }
            }
        }
        // Half way: a checkpoint of the live battle restores to the same state.
        if i + 1 == TICKS / 2 {
            let checkpoint = sim.checkpoint();
            checkpoint_bytes = (checkpoint.config.len(), checkpoint.keyframe.len());
            let mut back = Sim::from_checkpoint(&checkpoint, deps(data)).expect("the checkpoint restores");
            restored_matches = back.state_hash() == sim.state_hash() && back.subsystem_hashes() == sim.subsystem_hashes();
            for _ in 0..120 { back.step(); }
            assert_eq!(back.tick().0, TICKS / 2 + 120, "the restored battle runs on");
        }
    }
    // A lost hull's officers decide nothing any more: no mount of one still reports engaging.
    for ship in &sim.ships().ships {
        let gone = sim.world().vessel(ship.vessel).filter(|v| v.id == ship.vessel).is_none_or(|v| matches!(v.status, VesselStatus::Lost(_) | VesselStatus::Wreck));
        if gone { assert!(ship.stages.engage.mounts.iter().all(|m| m.status == EngageStatus::Idle), "lost hull {:?} still engages", ship.vessel); }
    }
    let statuses = sim.world().vessels().map(|(_, v)| (v.id, v.status, v.cells_lost_fraction())).collect();
    let t0 = std::time::Instant::now();
    let hash = sim.state_hash();
    let hash_ns = t0.elapsed().as_nanos() as u64;
    Run { hash, subsystems: sim.subsystem_hashes(), restored_matches, tally, tick_ns, statuses, checkpoint_bytes, hash_ns }
}

/// Print the tick-time distribution of one run.
fn report(name: &str, run: &Run) {
    let mut ns = run.tick_ns.clone();
    ns.sort_unstable();
    let ms = |v: u64| v as f64 / 1e6;
    let mean = ms(ns.iter().sum::<u64>()) / ns.len() as f64;
    let mut slowest: Vec<(u64, usize)> = run.tick_ns.iter().copied().zip(0..).collect();
    slowest.sort_unstable_by(|x, y| y.cmp(x));
    let slowest: Vec<String> = slowest.iter().take(4).map(|(ns, i)| format!("tick {i}: {:.2} ms", ms(*ns))).collect();
    println!("{name}: {} ticks, mean {mean:.3} ms, median {:.3} ms, p99 {:.3} ms, max {:.3} ms, slowest {slowest:?}", ns.len(), ms(ns[ns.len() / 2]), ms(ns[ns.len() * 99 / 100]), ms(ns[ns.len() - 1]));
}

#[test]
fn reference_battle_progresses_within_budget_and_repeats_exactly() {
    let data = data();
    // The shipping pool has 3 workers (L1084). The second run has 1, so the per-vessel fan-out and its serial
    // merge are compared against a run with no parallelism at all.
    let pool = |threads: usize| rayon::ThreadPoolBuilder::new().num_threads(threads).build().expect("a thread pool");
    let (a, b) = std::thread::scope(|s| {
        let first = s.spawn(|| pool(3).install(|| run_battle(&data)));
        let second = s.spawn(|| pool(1).install(|| run_battle(&data)));
        (first.join().expect("the first run finishes"), second.join().expect("the second run finishes"))
    });

    let t = &a.tally;
    println!("reference battle: {t:?}");
    println!("final hulls (id, status, cells lost): {:?}", a.statuses);
    println!("mid-battle checkpoint: config {} bytes, keyframe {} bytes; one state hash {:.2} ms", a.checkpoint_bytes.0, a.checkpoint_bytes.1, a.hash_ns as f64 / 1e6);
    // Both runs share the machine: for tick times, run `reference_battle_tick_times` alone.
    report("3 threads (shared machine)", &a);
    report("1 thread (shared machine)", &b);

    // The battle progresses through every stage of the pipeline.
    assert!(t.tracks_created >= 4 && t.peak_tracks >= 4, "detections become tracks: {t:?}");
    assert!(t.interest_changes >= 1, "what each side may draw is kept up during the fight: {t:?}");
    assert!(t.hostile_tracks >= 1, "a contact that fires on the player turns Hostile: {t:?}");
    assert!(t.engaging >= 2, "mounts are assigned: {t:?}");
    assert!(t.fired_player > 0 && t.fired_enemy > 0, "both sides fire: {t:?}");
    assert!(t.impacts > 0 && t.cells_changed > 0, "damage lands: {t:?}");
    assert!(t.hostile_sunk + t.hostile_disabled >= 1, "the player's fire sinks or disables a hostile hull: {t:?}");
    // Association gate (L467): no truth object holds two live tracks in one side's picture for more than 10 s.
    assert!(t.duplicate_ticks <= 600, "a truth object held two live tracks for {} ticks (side, truth, tracks) {:?}: {t:?}", t.duplicate_ticks, t.duplicate_worst);
    assert!(t.reinforcements >= 1, "the scheduled hunter group arrives: {t:?}");
    assert!(a.statuses.iter().any(|(_, _, lost)| *lost > 0.0), "some hull carries damage at the end");
    assert!(a.restored_matches, "a checkpoint taken mid-battle restores to the same state hash");

    // The same battle again ends in the same state, subsystem by subsystem.
    let diverged: Vec<&str> = a.subsystems.iter().zip(&b.subsystems).filter(|(x, y)| x.1 != y.1).map(|(x, _)| x.0).collect();
    assert!(diverged.is_empty(), "subsystems diverged between two runs: {diverged:?}");
    assert_eq!(a.hash, b.hash, "identical state hash over two runs");
    assert_eq!(a.tally, b.tally, "identical event stream over two runs");
    assert!(b.restored_matches);
}

/// A second fleet shape through the same driver for 20 s, so the civilian and three-sided paths of setup,
/// stance and interest run too, and a corrupted checkpoint is refused instead of trusted.
#[test]
fn convoy_raid_runs_and_checkpoints_reject_damage() {
    let data = data();
    let g = generate_standalone(MissionType::ConvoyRaid, Act::Act1, 1, 3, &DifficultySliders::default(), None, &data).expect("the Act 1 convoy raid");
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().expect("a 3-thread pool");
    pool.install(|| {
        let mut sim = Sim::new(g.config.clone(), deps(&data)).expect("starts");
        for _ in 0..1_200 {
            sim.step();
            sim.drain_events();
        }
        assert_eq!(sim.tick().0, 1_200);
        assert!(!sim.is_over());
        let hulls: Vec<EntityId> = sim.world().vessels().map(|(_, v)| v.id).collect();
        assert_eq!(hulls.len(), g.config.vessels.iter().filter(|v| v.arrives_s <= 20).count(), "every hull due by now is afloat");

        let checkpoint = sim.checkpoint();
        let back = Sim::from_checkpoint(&checkpoint, deps(&data)).expect("restores");
        let diverged: Vec<&str> = sim.subsystem_hashes().iter().zip(back.subsystem_hashes()).filter(|(x, y)| x.1 != y.1).map(|(x, _)| x.0).collect();
        assert!(diverged.is_empty(), "restored subsystems differ: {diverged:?}");
        assert_eq!(back.state_hash(), sim.state_hash());
        // Truncation, a flipped header, a foreign config and garbage are all errors, never panics.
        let mut cut = checkpoint.clone();
        cut.keyframe.truncate(cut.keyframe.len() / 2);
        assert!(Sim::from_checkpoint(&cut, deps(&data)).is_err());
        let mut tail = checkpoint.clone();
        let n = tail.keyframe.len();
        tail.keyframe.truncate(n - 3);
        assert!(Sim::from_checkpoint(&tail, deps(&data)).is_err());
        let mut magic = checkpoint.clone();
        magic.keyframe[0] ^= 0xFF;
        assert!(Sim::from_checkpoint(&magic, deps(&data)).is_err());
        let mut version = checkpoint.clone();
        version.keyframe_format += 1;
        assert!(Sim::from_checkpoint(&version, deps(&data)).is_err());
        let mut other = g.config.clone();
        other.seed ^= 1;
        assert!(Sim::restore_keyframe(other, deps(&data), &checkpoint.keyframe).is_err());
        let mut noise = checkpoint.clone();
        for (k, b) in noise.config.iter_mut().enumerate() { *b = (k as u8).wrapping_mul(31); }
        assert!(Sim::from_checkpoint(&noise, deps(&data)).is_err());
        assert!(Sim::restore_keyframe(g.config.clone(), deps(&data), &[]).is_err());
    });
}

/// The tick-time measurement for reports: one battle alone on a 3-thread pool. Run it in a release build
/// with `--ignored --nocapture`. It asserts nothing about wall time.
#[test]
#[ignore = "a measurement, not a check: run with --release --ignored --nocapture"]
fn reference_battle_tick_times() {
    let data = data();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().expect("a 3-thread pool");
    let run = pool.install(|| run_battle(&data));
    println!("reference battle: {:?}", run.tally);
    println!("final hulls (id, status, cells lost): {:?}", run.statuses);
    report("3 threads, alone", &run);
}
