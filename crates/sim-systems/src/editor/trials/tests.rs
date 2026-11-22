use super::*;
use crate::editor::NoDeadline;
use content::campaign::AceDef;

fn real_deps() -> SimDeps { crate::editor::splash::tests::real_deps() }
fn starter(name: &str) -> Blueprint { content::campaign::load::stock_blueprint(&format!("starters/{name}.bp.ron")).unwrap() }
fn pool3() -> rayon::ThreadPool { rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap() }

fn class(id: &str, role: ClassRole, act: Act) -> EnemyClassDef {
    EnemyClassDef {
        id: ClassId::new(id), name: TextKey::new("x"), blueprint: format!("{id}.bp.ron"), role, tier: content::enums::Tier::T1, act,
        behaviour: "combatant".into(), engagement_range_m: None, variants: vec![], surrender: Default::default(), air_wing: vec![], submersible: false, capital: false,
    }
}
fn campaign_with(classes: Vec<EnemyClassDef>) -> CampaignData {
    let mut c = CampaignData::default();
    c.theatre.basin = MapDef { size_m: [12_000.0, 12_000.0], depth_m: 100.0, islands: vec![], markers: vec![Marker { kind: MarkerKind::PlayerSpawn, name: "spawn".into(), pos_m: [0.0, 0.0], heading_deg: 90.0 }], seed: 0 };
    for d in classes { c.classes.insert(d.id.clone(), d); }
    c
}

/// Counts down: expires after `n` checks.
struct After(u32);
impl Deadline for After { fn expired(&mut self) -> bool { if self.0 == 0 { true } else { self.0 -= 1; false } } }

#[test]
fn complexity_points_is_the_weighted_sum() {
    let c = ComplexityCounts { cuboids: 100, buoyancy_samples: 40, conduit_segments: 20, compartments: 10, graph_node_evals: 50, luau_interrupts: 7 };
    let k = ComplexityCoefficients { cuboid: 0.5, buoyancy_sample: 1.0, conduit_segment: 0.25, compartment: 2.0, graph_node_eval: 0.1, luau_interrupt: 3.0, caution_points: 400.0 };
    assert!((complexity_points(&c, &k) - (50.0 + 40.0 + 5.0 + 20.0 + 5.0 + 21.0)).abs() < 1e-9);
    assert_eq!(complexity_points(&c, &ComplexityCoefficients::default()), 0.0);
}

#[test]
fn the_design_share_of_work_is_the_scene_less_the_baseline_per_tick() {
    let c = design_counts((6_000, 1_200, 600, 300, 900, 0), (600, 600, 60, 60, 300, 0), 600);
    assert_eq!(c, ComplexityCounts { cuboids: 9, buoyancy_samples: 1, conduit_segments: 1, compartments: 0, graph_node_evals: 1, luau_interrupts: 0 });
    // A baseline larger than the scene never goes negative, and no ticks give nothing.
    assert_eq!(design_counts((10, 10, 10, 10, 10, 10), (20, 20, 20, 20, 20, 20), 10), ComplexityCounts::default());
    assert_eq!(design_counts((10, 10, 10, 10, 10, 10), (0, 0, 0, 0, 0, 0), 0), ComplexityCounts::default());
}

#[test]
fn a_circle_fit_recovers_the_diameter_from_part_of_a_turn() {
    let centre = DVec2::new(1_200.0, -300.0);
    let pts: Vec<DVec2> = (0..120).map(|i| { let a = 0.3 + 4.5 * i as f64 / 120.0; centre + DVec2::new(libm::cos(a), libm::sin(a)) * 250.0 }).collect();
    let (c, r) = fit_circle(&pts).unwrap();
    assert!((r - 250.0).abs() < 1e-6 && (c - centre).length() < 1e-6, "{c:?} {r}");
    assert!(fit_circle(&pts[..2]).is_none());
    let line: Vec<DVec2> = (0..10).map(|i| DVec2::new(i as f64, 2.0 * i as f64)).collect();
    assert!(fit_circle(&line).is_none(), "points on a line have no circle");
}

#[test]
fn acceleration_numbers_come_from_the_speed_record() {
    // v = 10 (1 - e^(-t / 8)) m/s sampled at 60 Hz for 120 s.
    let v: Vec<f64> = (0..7_200).map(|i| 10.0 * (1.0 - libm::exp(-(i as f64 / 60.0) / 8.0))).collect();
    let (top, t90) = top_and_t90(&v);
    assert!((top - 10.0).abs() < 0.01, "{top}");
    // 0.9 = 1 - e^(-t/8) at t = 8 ln 10 = 18.42 s.
    assert!((t90 - 18.42).abs() < 0.1, "{t90}");
    assert!(plateaued(&v), "the curve is flat by 120 s");
    assert!(!plateaued(&v[..1_200]), "still rising at 20 s");
    assert_eq!(top_and_t90(&[]), (0.0, 0.0));
    assert_eq!(top_and_t90(&[0.0; 600]), (0.0, 0.0), "a hull that does not move");
    assert!(top_and_t90(&[f64::NAN; 600]).0 == 0.0);
}

#[test]
fn heel_settles_when_it_stays_in_the_band() {
    let mut h = vec![10.0; 60];
    h.extend(vec![1.5; 30]);
    h.push(3.0); // one wave over the band restarts the hold
    h.extend(vec![1.0; 200]);
    let t = settle_time(&h, 2.0);
    assert!((t - 91.0 / 60.0).abs() < 1e-9, "{t}");
    assert_eq!(settle_time(&vec![5.0; 10_000], 2.0), HEEL_RECOVER_MAX_S);
    assert_eq!(settled_band(0.5), 2.0);
    assert_eq!(settled_band(3.0), 4.0);
    assert_eq!(settled_band(f64::NAN), 2.0);
}

#[test]
fn heading_change_unwraps_through_north() {
    let mut m = HeadingMeter::default();
    let mut last = 0.0;
    for i in 0..=720 { last = m.update((i as f64).to_radians().rem_euclid(core::f64::consts::TAU) - core::f64::consts::PI); }
    assert!((last - 720.0).abs() < 1e-6, "{last}");
}

#[test]
fn opponents_come_from_the_lowest_act_that_fits_and_only_from_combatants() {
    let mut cap = class("c.capital", ClassRole::Capital, Act::Act1);
    cap.capital = true;
    let mut sub = class("c.sub", ClassRole::Submarine, Act::Act1);
    sub.submersible = true;
    let mut c = campaign_with(vec![
        class("c.boat", ClassRole::PatrolBoat, Act::Act1), class("c.fac", ClassRole::FastAttack, Act::Act1), class("c.trawler", ClassRole::Civilian, Act::Act1),
        class("c.tanker", ClassRole::Tanker, Act::Act1), class("c.ace", ClassRole::Frigate, Act::Act1), cap, sub,
        class("c.frigate3", ClassRole::Frigate, Act::Act3), class("c.destroyer4", ClassRole::Destroyer, Act::Act4),
    ]);
    c.aces.insert(AceId::new("ace.x"), AceDef { id: AceId::new("ace.x"), name: TextKey::new("x"), callsign: TextKey::new("x"), ship: ClassId::new("c.ace"), doctrine: vec![], arcs: vec![] });
    let act1: BTreeSet<&str> = ["c.boat", "c.fac"].into();
    for seed in 1..=40 {
        let o = opponents(120.0, seed, &c);
        assert_eq!(o.len(), OPPONENTS);
        assert!(o.iter().all(|id| act1.contains(id.as_str())), "seed {seed}: {o:?}");
        assert_eq!(o, opponents(120.0, seed, &c), "fixed by seed");
    }
    let seen: BTreeSet<ClassId> = (1..=40).flat_map(|s| opponents(120.0, s, &c)).collect();
    assert_eq!(seen.len(), 2, "both Act 1 combatants are drawn");
    // 400 t fits Act 2, which has no classes here: the lowest fitting act with classes is Act 3.
    assert!(opponents(400.0, 1, &c).iter().all(|id| id.as_str() == "c.frigate3"));
    // Over every cap: the heaviest act with classes.
    assert!(opponents(5_000.0, 1, &c).iter().all(|id| id.as_str() == "c.destroyer4"));
    assert!(opponents(f64::NAN, 1, &c).iter().all(|id| id.as_str() == "c.destroyer4"));
    assert!(opponents(100.0, 1, &CampaignData::default()).is_empty());
    assert!(opponents(100.0, 1, &campaign_with(vec![class("c.trawler", ClassRole::Civilian, Act::Act1)])).is_empty());
}

#[test]
fn the_fight_config_puts_three_hostile_opponents_three_km_off() {
    let campaign = content::campaign::load::campaign();
    let bp = starter("fac22");
    let a = fight_config(&bp, 2, &BatterySpec::ON_SAVE, campaign);
    assert_eq!(a, fight_config(&bp, 2, &BatterySpec::ON_SAVE, campaign), "deterministic");
    assert_eq!(a.mode, GameMode::Trial);
    assert_eq!(a.rules, ModeRules::Trial { ticks: 180 * 60 });
    assert_eq!(a.weather.sea_state, SeaState::BEAUFORT_4);
    assert_eq!(a.vessels.len(), 1 + OPPONENTS);
    assert_eq!(a.players.len(), 1);
    assert!(matches!(a.vessels[0].blueprint, BlueprintRef::Inline(h) if h == a.blueprints[0].header.hash));
    let tonnage = content::stats::compute_stats(&bp, content::catalogue::catalogue()).tonnage_t;
    let ids: Vec<ClassId> = a.vessels[1..].iter().filter_map(|v| v.class.clone()).collect();
    assert_eq!(ids, opponents(tonnage, 2, campaign));
    for v in &a.vessels[1..] {
        assert_eq!((v.side, v.declared, v.group), (THEM, Some(Stance::Hostile), Some(GroupId(0))));
        let d = (DVec2::new(v.pos.x, v.pos.z) - DVec2::new(a.vessels[0].pos.x, a.vessels[0].pos.z)).length();
        assert!((2_500.0..=3_600.0).contains(&d), "{d}");
        assert!(a.intel.declared_hostile.contains(v.class.as_ref().unwrap()));
    }
    let short = fight_config(&bp, 2, &BatterySpec::SHORT, campaign);
    assert_eq!(short.rules, ModeRules::Trial { ticks: 90 * 60 });
    assert!(a.map.size_m.iter().all(|s| *s >= TRIAL_MAP_M));
}

#[test]
fn run_headless_ends_at_the_tick_limit_or_aborts_on_the_deadline() {
    pool3().install(|| {
        let campaign = content::campaign::load::campaign();
        let bp = starter("patrol15");
        let mut config = acceleration_config(&bp, SeaState::CALM, campaign);
        config.rules = ModeRules::Trial { ticks: 120 };
        let r = run_headless(config.clone(), real_deps(), 10_000, &mut NoCommands, &mut NoDeadline).unwrap();
        assert_eq!((r.outcome, r.duration_ticks), (Outcome::Draw, 120));
        let r = run_headless(config.clone(), real_deps(), 30, &mut NoCommands, &mut NoDeadline).unwrap();
        assert_eq!(r.outcome, Outcome::Draw, "stopped by max_ticks");
        let r = run_headless(config.clone(), real_deps(), 10_000, &mut NoCommands, &mut After(1)).unwrap();
        assert_eq!(r.outcome, Outcome::Aborted);
        let mut bad = config;
        bad.protocol += 1;
        assert!(run_headless(bad, real_deps(), 10, &mut NoCommands, &mut NoDeadline).is_err());
    });
}

#[test]
fn compare_lists_every_metric_and_cross_platform_applies_the_tolerances() {
    let mut a = Scorecard { turning_circle_m: 200.0, time_to_90pct_speed_s: 20.0, top_speed_kn: 30.0, heel_recovery_s: 10.0, peak_heel_deg: 12.0, ..Default::default() };
    a.fight = FightScore { score: 40.0, per_seed: vec![40.0; 10], wins: 3, mean_hull_fraction_left: 0.8, outcomes: vec![2, 2, 2, 1, 1, 1, 1, 0, 0, 0], hull_left: vec![0.8; 10], ..Default::default() };
    let rows = compare(&a, &a);
    assert_eq!(rows.len(), 11);
    assert!(rows.iter().all(|(_, x, y)| x == y));
    assert!(rows.iter().any(|(n, x, _)| n == "fight.wins" && *x == 3.0));

    assert!(cross_platform_check(&a, &a).is_empty());
    let mut b = a.clone();
    b.turning_circle_m = 201.5; // 0.75%: inside 1%
    b.heel_recovery_s = 10.15; // 1.5%: inside 2%
    b.top_speed_kn = 30.1; // 0.33%: inside 0.5%
    b.fight.outcomes[0] = 1; // 9 of 10 match
    b.fight.hull_left = vec![0.76; 10]; // 4 points
    assert!(cross_platform_check(&a, &b).is_empty(), "{:?}", cross_platform_check(&a, &b));
    b.turning_circle_m = 204.0;
    b.peak_heel_deg = 12.5;
    b.top_speed_kn = 30.2;
    b.fight.outcomes[1] = 1;
    b.fight.hull_left = vec![0.74; 10];
    assert_eq!(cross_platform_check(&a, &b), vec!["turning_circle_m", "peak_heel_deg", "top_speed_kn", "fight.outcomes", "fight.hull_fraction"]);
    let mut c = a.clone();
    c.fight.per_seed.pop();
    c.fight.outcomes.pop();
    assert_eq!(cross_platform_check(&a, &c), vec!["fight.seeds"]);
    // Zero against zero is no drift.
    assert!(cross_platform_check(&Scorecard::default(), &Scorecard::default()).is_empty());
}

#[test]
fn reference_scenes_field_the_spec_counts_and_start() {
    let campaign = content::campaign::load::campaign();
    let hostile = |c: &SortieConfig| c.vessels.iter().filter(|v| v.side == THEM).count();
    let capitals = |c: &SortieConfig| c.vessels.iter().filter(|v| v.class.as_ref().and_then(|k| campaign.classes.get(k)).is_some_and(|d| d.capital)).count();
    let battle = reference_config(ReferenceScene::ReferenceBattle, 7, campaign);
    assert_eq!(battle, reference_config(ReferenceScene::ReferenceBattle, 7, campaign));
    assert_eq!((battle.vessels.len(), hostile(&battle), capitals(&battle), battle.players.len()), (41, 40, 4, 1));
    let coop = reference_config(ReferenceScene::CoopReference, 7, campaign);
    assert_eq!((coop.vessels.len(), hostile(&coop), capitals(&coop), coop.players.len()), (44, 40, 0, 4));
    let swarm = reference_config(ReferenceScene::MinimumSwarm, 7, campaign);
    assert_eq!((hostile(&swarm), capitals(&swarm), swarm.bodies.len()), (20, 4, 6));
    assert!(swarm.vessels.len() <= sim_core::time::MAX_FULL_PHYSICS_HULLS && coop.vessels.len() <= sim_core::time::MAX_FULL_PHYSICS_HULLS);
    pool3().install(|| {
        let mut sim = Sim::new(swarm, real_deps()).expect("the swarm starts");
        assert_eq!(sim.ai.air.aircraft.len(), 6, "the strike aircraft fly under the air AI");
        for _ in 0..30 { sim.set_host_load(HostLoad::IDLE); sim.step(); drop(sim.drain_events()); }
        assert_eq!(sim.ships().ships.len(), 21);
    });
}

#[test]
fn the_battery_measures_a_starter_and_holds_the_speed_gauge_within_ten_percent() {
    pool3().install(|| {
        let campaign = real_deps().campaign;
        let bp = starter("patrol15");
        let spec = BatterySpec { seeds: 1, fight_s: 20.0, sea: SeaState::BEAUFORT_4, short: true };
        let card = run_battery(&bp, &spec, &campaign, &mut real_deps, &mut NoDeadline);
        assert_eq!(card.design, crate::editor::splash::canonical(&bp).header.hash);
        assert!(card.short_trial);
        assert!(card.top_speed_kn > 10.0 && card.time_to_90pct_speed_s > 0.0, "{card:?}");
        assert!(card.turning_circle_m > 0.0 && card.peak_heel_deg > 0.0 && card.heel_recovery_s > 0.0 && card.heel_recovery_s < HEEL_RECOVER_MAX_S, "{card:?}");
        assert!(card.complexity.cuboids > 0 && card.complexity.buoyancy_samples > 0 && card.complexity.graph_node_evals > 0, "{:?}", card.complexity);
        assert_eq!(card.fight.per_seed.len(), 1);
        assert!(!card.opponents.is_empty());
        assert_eq!(card.drift.len(), 2);
        for d in &card.drift { assert!((d.simulated - d.predicted).abs() <= 0.1 * d.predicted, "{d:?}"); }
        // A deadline that is already up returns at once with the design stamped.
        let empty = run_battery(&bp, &spec, &campaign, &mut real_deps, &mut After(0));
        assert_eq!((empty.design, empty.top_speed_kn, empty.fight.per_seed.len()), (card.design, 0.0, 0));
    });
}

#[test]
fn a_fight_seed_is_deterministic() {
    pool3().install(|| {
        let campaign = real_deps().campaign;
        let bp = starter("fac22");
        let spec = BatterySpec { seeds: 1, fight_s: 8.0, sea: SeaState::BEAUFORT_4, short: true };
        let a = run_fight(fight_config(&bp, 3, &spec, &campaign), real_deps(), &mut NoDeadline).unwrap();
        let b = run_fight(fight_config(&bp, 3, &spec, &campaign), real_deps(), &mut NoDeadline).unwrap();
        assert_eq!(a, b);
        assert!(a.outcome <= 2 && (0.0..=1.0).contains(&a.hull_left));
    });
}

/// Measurement of the on-save battery on all six starters (L227: 60 s p95 gate).
#[test]
#[ignore]
fn timing_probe() {
    let deps = real_deps();
    let campaign = deps.campaign.clone();
    pool3().install(|| {
        for name in ["patrol15", "fac22", "insertion26", "corvette40", "corvette60", "destroyer90"] {
            let bp = starter(name);
            let t0 = std::time::Instant::now();
            let card = run_battery(&bp, &BatterySpec::ON_SAVE, &campaign, &mut real_deps, &mut NoDeadline);
            eprintln!("{name}: {:.1}s top {:.2} t90 {:.1} circle {:.0} heel {:.1}s/{:.1}deg fight {:?} cx {:?} drift {:?} opp {:?}", t0.elapsed().as_secs_f64(), card.top_speed_kn, card.time_to_90pct_speed_s, card.turning_circle_m, card.heel_recovery_s, card.peak_heel_deg, card.fight, card.complexity, card.drift.iter().map(|d| (d.predicted, d.simulated)).collect::<Vec<_>>(), card.opponents);
        }
    });
}

/// Counts own hits through the salvo log's audience filter against every routed copy.
#[test]
#[ignore]
fn salvo_filter_probe() {
    pool3().install(|| {
        let campaign = real_deps().campaign;
        for name in ["fac22", "corvette60"] {
            let bp = starter(name);
            let cfg = fight_config(&bp, 1, &BatterySpec::ON_SAVE, &campaign);
            let mut sim = Sim::new(cfg, real_deps()).unwrap();
            let own = own_vessel(&sim).unwrap();
            let mut log = SalvoLog::default();
            let (mut all, mut filtered, mut fired) = (BTreeSet::new(), BTreeSet::new(), 0u32);
            for t in 0..10_800u64 {
                sim.set_host_load(HostLoad::IDLE);
                sim.step();
                let ev = sim.drain_events();
                log.observe(own, &ev);
                for e in &ev {
                    let crate::protocol::event::SimEvent::Phys(p) = &e.event else { continue };
                    match p {
                        PhysEvent::ShellImpact { projectile, shooter, .. } if *shooter == own => { all.insert(*projectile); if e.audience.admits(SLOT, ME) { filtered.insert(*projectile); } }
                        PhysEvent::MountFired { vessel, .. } if *vessel == own && e.audience.admits(SLOT, ME) && t < 600 => fired += 1,
                        _ => {}
                    }
                }
            }
            eprintln!("{name}: own impacts all {} filtered {} | first salvos {:?} | mount fired in first 10 s {fired}", all.len(), filtered.len(), log.first.iter().map(|(s, (t, ids, hit))| (s.0, *t, ids.len(), *hit)).collect::<Vec<_>>());
        }
    });
}

#[test]
#[ignore]
fn complexity_and_scene_probe() {
    pool3().install(|| {
        let campaign = real_deps().campaign;
        for name in ["patrol15", "fac22", "corvette60", "destroyer90"] {
            let cfg = complexity_config(&starter(name), &campaign);
            let mut sim = Sim::new(cfg, real_deps()).unwrap();
            let own = own_vessel(&sim).unwrap();
            let mut fired = BTreeSet::new();
            for t in 0..600u64 {
                sim.set_host_load(HostLoad::IDLE);
                sim.step();
                for e in sim.drain_events() {
                    if let crate::protocol::event::SimEvent::Phys(PhysEvent::MountFired { vessel, slot, .. }) = &e.event { if *vessel == own { fired.insert((t, *slot)); } }
                }
            }
            eprintln!("{name}: complexity trial mount firings {} first at {:?}", fired.len(), fired.iter().next());
        }
        for scene in [ReferenceScene::ReferenceBattle, ReferenceScene::CoopReference] {
            let t0 = std::time::Instant::now();
            let mut sim = Sim::new(reference_config(scene, 1, &campaign), real_deps()).expect("scene starts");
            let built = t0.elapsed().as_secs_f64();
            for _ in 0..300 { sim.set_host_load(HostLoad::IDLE); sim.step(); drop(sim.drain_events()); }
            eprintln!("{scene:?}: built in {built:.1}s, 300 ticks in {:.1}s, ships {}", t0.elapsed().as_secs_f64() - built, sim.ships().ships.len());
        }
    });
}

fn t_tick(sim: &Sim) -> u64 { sim.tick().0 }

/// One trial fight seed in detail: opponents, own rounds and hits per mount, kills and how the design went
/// out. `cargo test --release -p sim-systems --lib fight_probe -- --ignored --nocapture` with `BP_STARTER`
/// (fac22), `BP_SEEDS` (1..=20 as `a-b`, default 1-20).
#[test]
#[ignore]
fn fight_probe() {
    let name = std::env::var("BP_STARTER").unwrap_or_else(|_| "fac22".into());
    let (a, b) = std::env::var("BP_SEEDS").ok().and_then(|s| { let mut p = s.split('-'); Some((p.next()?.parse().ok()?, p.next()?.parse().ok()?)) }).unwrap_or((1u64, 20u64));
    let verbose = std::env::var("BP_VERBOSE").is_ok();
    pool3().install(|| {
        let campaign = real_deps().campaign;
        let bp = starter(&name);
        let (mut wins, mut fired_all, mut hits_all) = (0, 0u32, 0u32);
        for seed in a..=b {
            let cfg = fight_config(&bp, seed, &BatterySpec::CI, &campaign);
            let opp: Vec<String> = cfg.vessels.iter().skip(1).map(|v| v.class.as_ref().map(|c| c.as_str().to_string()).unwrap_or_default()).collect();
            let mut sim = Sim::new(cfg, real_deps()).unwrap();
            let own = own_vessel(&sim).unwrap();
            let enemies: Vec<EntityId> = sim.ships().ships.iter().filter(|s| s.side != ME).map(|s| s.vessel).collect();
            let mut log = SalvoLog::default();
            let mut fired: BTreeMap<u16, u32> = BTreeMap::new();
            let mut hits: BTreeMap<u16, u32> = BTreeMap::new();
            let mut proj_mount: BTreeMap<ProjectileId, u16> = BTreeMap::new();
            let mut first_fire: BTreeMap<u16, (f64, f64)> = BTreeMap::new();
            let mut enemy_hits = 0u32;
            let mut on_enemy: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
            let mut notes: Vec<String> = Vec::new();
            let mut end = 0u64;
            for t in 0..10_800u64 {
                sim.set_host_load(HostLoad::IDLE);
                sim.step();
                let ev = sim.drain_events();
                log.observe(own, &ev);
                let mut seen: BTreeSet<String> = BTreeSet::new();
                for e in &ev {
                    let crate::protocol::event::SimEvent::Phys(p) = &e.event else { continue };
                    let k = format!("{p:?}");
                    if !seen.insert(k) { continue; }
                    match p {
                        PhysEvent::ProjectileSpawned(ps) if ps.shooter == own => {
                            proj_mount.insert(ps.id, ps.mount.0);
                            *fired.entry(ps.mount.0).or_default() += 1;
                            first_fire.entry(ps.mount.0).or_insert_with(|| {
                                if std::env::var("BP_FIRST").is_ok() {
                                    if let Some(ship) = sim.ships().get(own) {
                                        let tr = ship.stages.engage.mounts.iter().find(|m| m.slot == ps.mount).and_then(|m| match m.status { EngageStatus::Engaging { track, .. } => sim.pictures().side(ME).and_then(|p| p.get(track)), _ => None });
                                        if let (Some(t), Some(m)) = (tr, ship.design.mounts.iter().find(|m| m.slot == ps.mount)) {
                                            let cat = sim.world().catalogue();
                                            let gf = crate::engage::GunneryFacts { tables: &ship.design.range_tables, catalogue: cat, accuracy_mult: ship.proficiency.aim_error_mult, sea: sim.config().weather.sea_state };
                                            let muzzle = sim.world().vessel(own).map(|v| v.kin.pose.to_world(m.pos_local)).unwrap_or_default();
                                            let range = (t.predict(t_tick(&sim)) - muzzle).length();
                                            let h = crate::engage::hit_chance(m, t, muzzle, range, t_tick(&sim), &gf, &cat.tuning);
                                            eprintln!("    first round mount {} track {} grade {:?} fit {:?} rcs {:.0}: {:?}", m.slot.0, t.id.0, t.grade, t.fit.map(|f| f.fixes), t.rcs_m2, h.map(|h| (h.pk, h.errors, h.target)));
                                        }
                                    }
                                }
                                let mp = sim.world().locate(own).map(|l| l.0).unwrap_or_default();
                                let r = enemies.iter().filter_map(|e| sim.world().locate(*e)).map(|l| (l.0 - mp).length()).fold(f64::MAX, f64::min);
                                (t as f64 / 60.0, r)
                            });
                        }
                        PhysEvent::ShellImpact { projectile, shooter, vessel, outcome, .. } if *shooter == own && *vessel != own => {
                            if !enemies.contains(vessel) { continue; }
                            if let Some(m) = proj_mount.get(projectile) { *hits.entry(*m).or_default() += 1; }
                            let h = on_enemy.entry(vessel.index).or_default();
                            h.0 += 1;
                            if matches!(outcome, sim_core::armour::HitOutcome::Penetrated | sim_core::armour::HitOutcome::PassedThrough) { h.1 += 1; }
                        }
                        PhysEvent::ShellImpact { vessel, .. } if *vessel == own => enemy_hits += 1,
                        PhysEvent::StatusChanged { vessel, status } => notes.push(format!("t={:.0} {} {:?}", t as f64 / 60.0, if *vessel == own { "OWN".to_string() } else { format!("e{}", vessel.index) }, status)),
                        PhysEvent::BodySpawned { .. } if verbose => notes.push(format!("t={:.0} body {p:?}", t as f64 / 60.0)),
                        _ => {}
                    }
                }
                end = t;
                if out_of_fight(&sim, own) || enemies.iter().all(|e| out_of_fight(&sim, *e)) { break; }
            }
            let own_out = out_of_fight(&sim, own);
            let all_out = enemies.iter().all(|e| out_of_fight(&sim, *e));
            if all_out && !own_out { wins += 1; }
            let (s, h) = log.tally();
            fired_all += fired.values().sum::<u32>();
            hits_all += hits.values().sum::<u32>();
            let own_status = sim.world().vessel(own).map(|v| (v.status, v.cells_lost_fraction()));
            eprintln!("seed {seed:>2} vs {opp:?}: {} at {:.0}s own {own_status:?} enemy hits on us {enemy_hits} | first salvos {h}/{s} | fired {fired:?} hits {hits:?} first fire (t, range) {first_fire:?}", if all_out && !own_out { "WIN " } else if own_out { "LOSS" } else { "draw" }, end as f64 / 60.0);
            for n in &notes { eprintln!("      {n}"); }
            for e in &enemies {
                let v = sim.world().vessel(*e);
                eprintln!("      e{} {:?} dims {:?} status {:?} lost {:.3} our hits (all, pen) {:?}", e.index, v.map(|v| v.class.clone()), v.map(|v| v.dims), v.map(|v| v.status), v.map_or(0.0, |v| v.cells_lost_fraction()), on_enemy.get(&e.index));
            }
        }
        eprintln!("{name}: wins {wins} rounds {fired_all} hits {hits_all}");
    });
}

/// Duplicate tracks (L467: no truth object holds two live tracks for more than 10 s) in a reference scene.
/// `cargo test --release -p sim-systems --lib duplicate_probe -- --ignored --nocapture` with `BP_TICKS`
/// (3600), `BP_SCENE` (`battle`, `coop`, `swarm`), `BP_SEED` (1).
#[test]
#[ignore]
fn duplicate_probe() {
    let ticks: u64 = std::env::var("BP_TICKS").ok().and_then(|s| s.parse().ok()).unwrap_or(3_600);
    let seed: u64 = std::env::var("BP_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let scene = match std::env::var("BP_SCENE").as_deref() { Ok("coop") => ReferenceScene::CoopReference, Ok("swarm") => ReferenceScene::MinimumSwarm, _ => ReferenceScene::ReferenceBattle };
    pool3().install(|| {
        let campaign = real_deps().campaign;
        let mut deps = real_deps();
        // BP_GATE: the association gate in sigma (L468's duplicate fallback is 3.5); BP_SIGMA_A: scale every
        // sigma_a (L468's containment fallback is 1.5).
        if let (Ok(g), a) = (std::env::var("BP_GATE").map(|g| g.parse::<f64>().unwrap_or(3.03)), std::env::var("BP_SIGMA_A").ok().and_then(|a| a.parse::<f64>().ok())) {
            let mut c = (*deps.catalogue).clone();
            c.tuning.tracks.gate_sigma = g;
            if let Some(a) = a { for s in c.tuning.tracks.sigma_a.iter_mut() { *s *= a; } }
            deps.catalogue = std::sync::Arc::new(c);
        }
        let mut sim = Sim::new(reference_config(scene, seed, &campaign), deps).expect("scene starts");
        let mut since: BTreeMap<(u8, EntityId), u64> = BTreeMap::new();
        let mut worst: BTreeMap<(u8, EntityId), u64> = BTreeMap::new();
        // Swaps (L467: under 1% of track updates): a live track whose truth changes between two samples.
        let mut seen_track: BTreeMap<(u8, u32), (EntityId, u64)> = BTreeMap::new();
        let (mut updates, mut swaps) = (0u64, 0u64);
        let mut swap_kinds: BTreeMap<String, u64> = BTreeMap::new();
        for i in 0..ticks {
            sim.set_host_load(HostLoad::IDLE);
            sim.step();
            drop(sim.drain_events());
            if i % 6 != 0 { continue; }
            let now = i + 1;
            for (si, p) in sim.pictures().sides.iter().enumerate() {
                for t in p.tracks.iter().filter(|t| t.is_live()) {
                    let k = (si as u8, t.id.0);
                    if let Some((truth, upd)) = seen_track.get(&k).copied() {
                        if t.last_update_tick != upd {
                            updates += 1;
                            if !truth.is_none() && !t.truth.is_none() && truth != t.truth {
                                swaps += 1;
                                let kind = |e: EntityId| if sim.world().vessel(e).is_some() { "vessel" } else { "body" };
                                *swap_kinds.entry(format!("{:?} {}->{}", t.domain, kind(truth), kind(t.truth))).or_default() += 1;
                            }
                        }
                    }
                    seen_track.insert(k, (t.truth, t.last_update_tick));
                }
            }
            let mut held: BTreeMap<(u8, EntityId), usize> = BTreeMap::new();
            for (si, p) in sim.pictures().sides.iter().enumerate() {
                for t in p.tracks.iter().filter(|t| t.is_live() && !t.truth.is_none()) { *held.entry((si as u8, t.truth)).or_default() += 1; }
            }
            since.retain(|k, _| held.get(k).is_some_and(|n| *n >= 2));
            for (k, _) in held.iter().filter(|(_, n)| **n >= 2) {
                let s = *since.entry(*k).or_insert(now);
                let w = worst.entry(*k).or_default();
                *w = (*w).max(now - s);
            }
            if now % 1800 == 1 || i + 6 >= ticks {
                let over: Vec<_> = since.iter().filter(|(_, s)| now - **s > 600).map(|(k, s)| (k.0, k.1.index, (now - s) / 60)).collect();
                eprintln!("tick {now}: {} truths with duplicates, {} over 10 s: {over:?}", since.len(), over.len());
                for (k, _) in since.iter().filter(|(_, s)| now - **s > 600).take(6) {
                    let p = &sim.pictures().sides[k.0 as usize];
                    let kind = sim.world().vessel(k.1).map(|_| "vessel").unwrap_or("body");
                    let tp = sim.world().locate(k.1).map(|l| l.0);
                    for t in p.tracks.iter().filter(|t| t.is_live() && t.truth == k.1) {
                        eprintln!("    side {} truth {} ({kind}) track {} {:?} {:?} created {} upd {} grade {:?} sources {:?} speed {:.0} err {:.0} sigma {:.0}", k.0, k.1.index, t.id.0, t.domain, t.stance, t.created_tick, t.last_update_tick, t.grade, t.sources, t.vel.length(), tp.map_or(-1.0, |p| (t.pos - p).length()), t.sigma_m(now, &content::tuning::TuningTable::default().tracks));
                    }
                }
            }
        }
        let mut w: Vec<_> = worst.iter().map(|(k, v)| (*v, k.0, k.1.index)).collect();
        w.sort_unstable_by(|a, b| b.cmp(a));
        eprintln!("longest duplicate runs (ticks, side, truth): {:?}", &w[..w.len().min(12)]);
        eprintln!("track swaps {swaps} of {updates} sampled updates ({:.3}%), by kind {swap_kinds:?}", 100.0 * swaps as f64 / updates.max(1) as f64);
    });
}

/// Radar silhouettes against hull sizes: per stock class and starter, its dims and the projected area above
/// water (the radar cross-section before part factors) bow-on, on the beam and the mean.
/// `cargo test --release -p sim-systems --lib silhouette_probe -- --ignored --nocapture`.
#[test]
#[ignore]
fn silhouette_probe() {
    let cat = content::catalogue::catalogue();
    let mut paths: Vec<String> = content::data::STOCK_FILES.iter().map(|(p, _)| p.to_string()).filter(|p| (p.starts_with("combine/") || p.starts_with("starters/")) && p.ends_with(".bp.ron")).collect();
    paths.sort();
    for path in paths {
        let Ok(bp) = content::campaign::load::stock_blueprint(&path) else { continue };
        let prefab = sim_core::vessel::build_prefab(&bp, cat);
        let a = sim_core::vessel::projected_areas_prefab(&prefab, cat);
        let mean = a.rcs_m2.iter().map(|&x| x as f64).sum::<f64>() / 36.0;
        let ab = a.above_m2.iter().map(|&x| x as f64).sum::<f64>() / 36.0;
        eprintln!("{path}: dims {:?} top {:.1} rcs bow {:.0} beam {:.0} quarter {:.0} mean {:.0} | above bow {:.0} beam {:.0} mean {ab:.0}", prefab.dims, a.top_height_m, a.rcs_m2[0], a.rcs_m2[9], a.rcs_m2[4], mean, a.above_m2[0], a.above_m2[9]);
    }
}

/// The integration test's reference battle (tests/reference_battle.rs) with the player's mount statuses and
/// hold reasons over time. `cargo test --release -p sim-systems --lib refbattle_probe -- --ignored --nocapture`.
#[test]
#[ignore]
fn refbattle_probe() {
    use crate::protocol::event::SimEvent;
    let deps = real_deps();
    let data = crate::theatre::TheatreData { campaign: deps.campaign.clone(), catalogue: deps.catalogue.clone() };
    let g = crate::theatre::opgen::generate_standalone(content::campaign::MissionType::HunterGroupSurvival, Act::Act1, 1, 1, &content::campaign::DifficultySliders::default(), None, &data).expect("template");
    let mut config = g.config;
    let own = config.players[0].vessel.expect("hull") as usize;
    config.vessels[own].pos = DVec3::new(10_450.0 - 1_250.0, 0.0, 0.0);
    for v in config.vessels.iter_mut().filter(|v| v.arrives_s > 0) { v.arrives_s = 60; }
    let ticks: u64 = std::env::var("BP_TICKS").ok().and_then(|s| s.parse().ok()).unwrap_or(18_000);
    for v in &config.vessels { eprintln!("vessel side {} {:?} class {:?} at ({:.0}, {:.0}) arrives {} emcon {:?}", v.side.0, v.blueprint, v.class, v.pos.x, v.pos.z, v.arrives_s, v.emcon); }
    eprintln!("weather {:?}", config.weather);
    pool3().install(|| {
        let mut deps = real_deps();
        if std::env::var("BP_NOGATE").is_ok() { let mut c = (*deps.catalogue).clone(); c.tuning.doctrine.effective_range_pk = 0.0; deps.catalogue = std::sync::Arc::new(c); }
        let mut sim = Sim::new(config, deps).unwrap();
        let me = sim.ships().of_player(SLOT).unwrap().vessel;
        sim.apply(&[CommandEnvelope { player: SLOT, seq: 1, client_render_tick: 0, rtt_ms: 0, command: Command::SetCourse { waypoints: vec![DVec2::new(10_450.0, 0.0)], speed_kn: Some(20.0), append: false } }]);
        let mut last: BTreeMap<PartSlot, String> = BTreeMap::new();
        let (mut seen, mut seen_fire): (BTreeSet<ProjectileId>, BTreeSet<(u64, u32, u16)>) = (BTreeSet::new(), BTreeSet::new());
        let mut hits: BTreeMap<(u32, u32), u32> = BTreeMap::new();
        let mut fired: BTreeMap<u32, u32> = BTreeMap::new();
        let mut lc: Option<u64> = None;
        let mut unp: Option<u64> = None;
        for i in 0..ticks {
            sim.set_host_load(HostLoad::IDLE);
            sim.step();
            if let Some(ship) = sim.ships().get(me) {
                for m in &ship.stages.engage.mounts {
                    if lc.is_none() && matches!(m.status, crate::protocol::text::EngageStatus::Engaging { reason: FireReason::LocalControl, .. }) { lc = Some(i / 60); }
                    if unp.is_none() && matches!(m.status, EngageStatus::Holding { reason: HoldReason::Unpowered, .. }) { unp = Some(i / 60); }
                }
            }
            for e in sim.drain_events() {
                match &e.event {
                    SimEvent::Phys(PhysEvent::StatusChanged { vessel, status }) => eprintln!("t={:.0} vessel {} {:?}", i as f64 / 60.0, vessel.index, status),
                    SimEvent::Phys(PhysEvent::VesselLost { vessel, cause, by, .. }) => eprintln!("t={:.0} vessel {} lost {:?} by {}", i as f64 / 60.0, vessel.index, cause, by.index),
                    SimEvent::Phys(PhysEvent::ShellImpact { projectile, shooter, vessel, .. }) if seen.insert(*projectile) => { *hits.entry((shooter.index, vessel.index)).or_default() += 1; }
                    SimEvent::Phys(PhysEvent::MountFired { vessel, rounds, slot }) if seen_fire.insert((i, vessel.index, slot.0)) => { *fired.entry(vessel.index).or_default() += *rounds as u32; }
                    _ => {}
                }
            }
            if i % 600 == 0 {
                let Some(ship) = sim.ships().get(me) else { break };
                for m in &ship.stages.engage.mounts {
                    let s = format!("{:?}", m.status);
                    if last.get(&m.slot) != Some(&s) { eprintln!("t={:.0} mount {} {s}", i as f64 / 60.0, m.slot.0); last.insert(m.slot, s); }
                }
                let p = sim.pictures().side(ME).unwrap();
                let mp = sim.world().locate(me).map(|l| l.0).unwrap_or_default();
                let tr: Vec<String> = p.tracks.iter().filter(|t| t.is_live() && t.stance >= Stance::Suspect).map(|t| format!("{}:{:?}/{:?}/{:.0}m/rcs{:.0}/fc{}", t.id.0, t.stance, t.grade, (t.pos - mp).length(), t.rcs_m2, t.fire_control().is_some())).collect();
                eprintln!("t={:.0} tracks {tr:?} emcon {:?}", i as f64 / 60.0, ship.stages.emcon.radiating);
                if i % 3600 == 0 || i + 600 >= ticks { eprintln!("    hits (shooter, target) {hits:?} fired {fired:?} local control at {lc:?} unpowered at {unp:?} lost {:?}", sim.world().vessels().map(|(_, v)| (v.id.index, (v.cells_lost_fraction() * 1000.0).round())).collect::<Vec<_>>()); }
                let cat = sim.world().catalogue();
                let gf = crate::engage::GunneryFacts { tables: &ship.design.range_tables, catalogue: cat, accuracy_mult: ship.proficiency.aim_error_mult, sea: sim.config().weather.sea_state };
                let pose = sim.world().vessel(me).map(|v| v.kin.pose).unwrap_or_default();
                for m in ship.design.mounts.iter().filter(|m| m.weapon == content::graph::WeaponClass::Gun) {
                    let Some(t) = ship.stages.engage.mounts.iter().find(|x| x.slot == m.slot).and_then(|x| match x.status { EngageStatus::Engaging { track, .. } | EngageStatus::Holding { track, .. } => p.get(track), _ => None }) else { continue };
                    let muzzle = pose.to_world(m.pos_local);
                    let range = (t.predict(sim.tick().0) - muzzle).length();
                    if let Some(h) = crate::engage::hit_chance(m, t, muzzle, range, sim.tick().0, &gf, &cat.tuning) { eprintln!("    mount {} pk {:.3} {:?} {:?} fit {:?}", m.slot.0, h.pk, h.errors, h.target, t.fit.map(|f| (f.fixes, f.tick))); }
                }
            }
        }
    });
}

/// L491's first-salvo gate at a reduced spec (ruling 2 of 20 November 2026): the fac22's officers hit with at
/// least 10% of first salvos. 6 seeds of 120 s at Beaufort 4 instead of the CI battery's 20 of 180 s.
#[test]
fn the_fac22_lands_a_tenth_of_its_first_salvos_at_a_reduced_spec() {
    pool3().install(|| {
        let campaign = real_deps().campaign;
        let bp = starter("fac22");
        let spec = BatterySpec { seeds: 6, fight_s: 120.0, sea: SeaState::BEAUFORT_4, short: false };
        let card = run_battery(&bp, &spec, &campaign, &mut real_deps, &mut NoDeadline);
        assert_eq!(card.fight.per_seed.len(), 6);
        assert!(card.fight.first_salvo_hit_rate >= 0.10, "first-salvo hit rate {} under 0.10 (L491): {:?}", card.fight.first_salvo_hit_rate, card.fight);
    });
}

/// The Combine surrender rule in a real fight (L537, ruling 1 of 20 November 2026): the fac22 against its
/// trial opponents until one strikes. A struck hull is Disabled, stops firing and making way, the player's
/// officers stop engaging it (it is not designated), the fight score counts its tonnage, and the player hears
/// one radio line.
#[test]
fn a_struck_combine_hull_counts_as_disabled_and_stops_fighting() {
    use crate::protocol::event::SimEvent;
    use sim_core::PhysEvent;
    pool3().install(|| {
        let campaign = real_deps().campaign;
        let bp = starter("fac22");
        let spec = BatterySpec { seeds: 1, fight_s: 240.0, sea: SeaState::BEAUFORT_4, short: false };
        let mut found = false;
        for seed in 0..6u64 {
            let config = fight_config(&bp, seed, &spec, &campaign);
            let mut sim = Sim::new(config, real_deps()).expect("the fight starts");
            let own = own_vessel(&sim).expect("own hull");
            let mut top = sim.ships().get(own).map(|s| s.design.turn.top_speed_mps / sim_core::time::KN).unwrap_or(20.0);
            if !top.is_finite() { top = 20.0; }
            let enemies: Vec<EntityId> = sim.ships().ships.iter().filter(|s| s.side != ME).map(|s| s.vessel).collect();
            let c = enemies.iter().filter_map(|e| sim.world().vessel(*e)).map(|v| DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z)).fold(DVec2::ZERO, |a, p| a + p) / enemies.len().max(1) as f64;
            sim.apply(&[CommandEnvelope { player: SLOT, seq: 0, client_render_tick: 0, rtt_ms: 0, command: Command::SetCourse { waypoints: vec![c], speed_kn: Some(top), append: false } }]);
            let mut struck: Option<(EntityId, u64)> = None;
            let (mut lines, mut shots_after, mut engaged_after) = (0u32, 0u32, 0u32);
            for i in 0..ticks_of(spec.fight_s) {
                sim.set_host_load(HostLoad::IDLE);
                sim.step();
                for e in sim.drain_events() {
                    match &e.event {
                        SimEvent::OfficerMessage { text, .. } if text.key.as_str() == "officer.radio.gunnery.struck" => lines += 1,
                        SimEvent::Phys(PhysEvent::ProjectileSpawned(p)) if struck.is_some_and(|(v, t)| p.shooter == v && i > t + 60) => shots_after += 1,
                        _ => {}
                    }
                }
                if struck.is_none() {
                    struck = enemies.iter().copied().find(|e| sim.world().vessel(*e).is_some_and(|v| v.struck)).map(|v| (v, i));
                } else if let Some((v, t)) = struck {
                    if i > t + 5 * TICKS_PER_S {
                        let tracks: Vec<TrackId> = sim.pictures().side(ME).map(|p| p.tracks.iter().filter(|tr| tr.truth == v).map(|tr| tr.id).collect()).unwrap_or_default();
                        let ship = sim.ships().get(own).expect("own ship");
                        if ship.orders.designated.is_none() || !tracks.contains(&ship.orders.designated) {
                            engaged_after += ship.stages.engage.mounts.iter().filter(|m| matches!(m.status, crate::protocol::text::EngageStatus::Engaging { track, .. } if tracks.contains(&track))).count() as u32;
                        }
                    }
                    if i > t + 30 * TICKS_PER_S { break; }
                }
                if sim.is_over() { break; }
            }
            let Some((v, _)) = struck else { continue };
            found = true;
            let hull = sim.world().vessel(v).expect("the struck hull is afloat");
            assert_eq!(hull.status, VesselStatus::Disabled, "a struck hull counts as disabled (L537)");
            // Stopped engines: what way is left 30 s on is the run-down and the drift of a Beaufort 4 sea.
            assert_eq!(sim.ships().get(v).and_then(|s| s.orders.speed_kn), Some(0.0), "a struck hull is ordered stopped");
            assert!(hull.forward_speed().abs() < 2.5, "a struck hull stops making way: {} m/s", hull.forward_speed());
            assert_eq!(shots_after, 0, "a struck hull stops firing");
            assert_eq!(engaged_after, 0, "the officers do not re-engage a struck hull that is not designated");
            let struck_now = enemies.iter().filter(|e| sim.world().vessel(**e).is_some_and(|x| x.struck)).count() as u32;
            assert_eq!(lines, struck_now, "one radio line to the player for each hull that struck");
            let result = sim.finish(Outcome::Draw).clone();
            assert!(result.stats.enemy_tonnage_disabled > 0.0, "the struck hull's tonnage counts as disabled");
            break;
        }
        assert!(found, "no opponent struck in six 240 s fights");
    });
}
