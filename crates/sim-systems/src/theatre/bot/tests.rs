//! Bot logic on the shipped campaign data with a scripted resolver (no physics): the theatre bookkeeping,
//! stop rules, determinism and the winnability tally. Full-physics runs are measured separately.
use super::*;
use crate::protocol::config::{KillFate, KillRecord, PlayerResult, SortieConfig, SortieResult, SortieStats, VesselRole};
use content::campaign::RangeBand;

mod gate;

fn data() -> TheatreData {
    let catalogue = Arc::new(content::catalogue::Catalogue::load_embedded().expect("catalogue"));
    let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign data"));
    TheatreData { campaign, catalogue }
}

/// Plays every sortie as a fixed outcome: a win sinks every committed Combine hull record by gunfire,
/// observed unless the setup runs silent.
struct Scripted { outcome: Outcome, lose_hull: bool, minutes: f64, last: f64, calls: u32 }
impl Scripted { fn new(outcome: Outcome) -> Scripted { Scripted { outcome, lose_hull: false, minutes: 20.0, last: 0.0, calls: 0 } } }
impl SortieResolver for Scripted {
    fn resolve(&mut self, config: &SortieConfig, setup: &PlayerSetup, _data: &TheatreData) -> SortieResult {
        self.calls += 1;
        let win = self.outcome == Outcome::Victory;
        let silent = setup.emcon == Some(Emcon::Silent);
        let kills = if !win { Vec::new() } else {
            config.vessels.iter().filter(|v| v.hull_record.is_some() && v.role == VesselRole::Combatant).map(|v| KillRecord {
                tick: 60, hull_record: v.hull_record, class: v.class.clone(), side: v.side, tonnage_t: 100.0, build_cost: 500, fate: KillFate::Lost,
                method: setup.method.unwrap_or(KillMethod::Gunfire), range: RangeBand::Medium, time: config.time_of_day, emcon: setup.emcon.unwrap_or(Emcon::Burst),
                observed: !silent, by_player: Some(PlayerSlot(0)), salvageable: true,
            }).collect()
        };
        let players = config.players.iter().filter(|p| p.vessel.is_some()).map(|p| PlayerResult {
            player: p.slot, hull_lost: self.lose_hull, damage: Default::default(), fuel_fraction: 0.6, ammo: BTreeMap::new(), expended: BTreeMap::new(), ammo_kg_expended: BTreeMap::new(), fuel_l_used: 100.0, passengers_lost: 0, dropped: false,
        }).collect();
        let mut r = super::super::abstract_combat::aborted_result(config);
        r.outcome = self.outcome;
        r.objective_done = win;
        r.ghosts = vec![win && setup.ghosts; config.ghosts.len()];
        r.kills = kills;
        r.players = players;
        r.duration_ticks = (self.minutes * 3600.0) as u64;
        r.stats = SortieStats::default();
        self.last = if self.outcome == Outcome::Aborted { 0.0 } else { self.minutes };
        r
    }
    fn last_duration_min(&self) -> f64 { self.last }
}

fn bot(policy: Policy, seed: u64) -> BotConfig {
    BotConfig { policy, seed, difficulty: DifficultySliders::NORMAL, ironman: false, players: 1, until_act: Act::Act1, max_operations: 60, max_theatre_hours: 24 * 120 }
}

#[test]
fn setups_by_policy() {
    assert_eq!(player_setup(Policy::Aggressive).emcon, Some(Emcon::Active));
    let careful = player_setup(Policy::Careful);
    assert!(careful.ghosts && careful.emcon == Some(Emcon::Silent));
    assert_eq!(player_setup(Policy::SingleMethod(KillMethod::Missile)).method, Some(KillMethod::Missile));
    assert!(player_setup(Policy::AllGhost).ghosts && !player_setup(Policy::NoGhost).ghosts);
    for p in [Policy::Aggressive, Policy::Careful, Policy::Mixed, Policy::AllGhost, Policy::NoGhost, Policy::SingleMethod(KillMethod::Torpedo)] {
        let s = player_setup(p);
        assert!(s.follow_briefing_route && s.reference_setup.is_none());
    }
}

#[test]
fn a_fresh_campaign_accepts_an_offer() {
    let d = data();
    let starter = campaign_starter(Policy::Mixed, Act::Act1, &d);
    let new = NewCampaign { seed: 4, difficulty: DifficultySliders::NORMAL, ironman: false, slot_name: "s".into(), players: vec![(1, "p".into(), starter)], start_act: Act::Act1 };
    let t = Theatre::new_campaign(new, d);
    let a = choose_action(&t, Policy::Mixed, 4);
    match a {
        TheatreAction::AcceptOperation(op) => assert!(t.state.offered.contains(&op)),
        TheatreAction::Transit(c) => assert!(t.state.open_coves.contains(&c)),
        other => panic!("expected an operation or a transit, got {other:?}"),
    }
    assert_eq!(choose_action(&t, Policy::Mixed, 4), choose_action(&t, Policy::Mixed, 4), "deterministic");
}

#[test]
fn winning_bot_finishes_act_one_and_is_deterministic() {
    let d = data();
    let mut r = Scripted::new(Outcome::Victory);
    let rep = run_campaign(&bot(Policy::Mixed, 7), d.clone(), &mut r);
    assert!(!rep.operations.is_empty());
    assert_eq!(rep.operations.len() as u32, r.calls);
    assert!(rep.capstone_done.contains(&Act::Act1), "every sortie won: the capstone falls, report {:?}", (rep.operations.len(), rep.theatre_hours, &rep.acts_lost));
    assert!(rep.acts_lost.is_empty());
    assert!(rep.total_income > 0);
    assert!(rep.income_spend_ratio.is_finite() && rep.income_spend_ratio > 0.0);
    assert!(rep.operations.iter().all(|o| o.outcome == Outcome::Victory && (o.sortie_min - 20.0).abs() < 1e-9));
    assert!(rep.operations.windows(2).all(|w| w[0].hour <= w[1].hour));
    assert_eq!(rep.no_offer_states, 0, "the repeat cap never leaves the chart without a template");
    let again = run_campaign(&bot(Policy::Mixed, 7), d.clone(), &mut Scripted::new(Outcome::Victory));
    assert_eq!(again, rep, "same seed, same run");
}

#[test]
fn aggressive_raises_more_alert_than_careful() {
    let d = data();
    let aggressive = run_campaign(&BotConfig { max_operations: 1, ..bot(Policy::Aggressive, 3) }, d.clone(), &mut Scripted::new(Outcome::Victory));
    let careful = run_campaign(&BotConfig { max_operations: 1, ..bot(Policy::Careful, 3) }, d, &mut Scripted::new(Outcome::Victory));
    assert_eq!((aggressive.operations.len(), careful.operations.len()), (1, 1));
    let (a, c) = (aggressive.operations[0].alert_after, careful.operations[0].alert_after);
    assert!(a > c, "observed kills raise Alert at once, unobserved ones by half and late: {a} vs {c}");
    assert!(careful.operations[0].ghosts_met > 0 || careful.operations[0].outcome != Outcome::Victory);
}

#[test]
fn a_bot_that_loses_its_hull_rebuilds_and_sails_again() {
    let d = data();
    let mut r = Scripted::new(Outcome::Defeat);
    r.lose_hull = true;
    let rep = run_campaign(&BotConfig { max_theatre_hours: 24 * 60, ..bot(Policy::Mixed, 5) }, d, &mut r);
    // The starting pool cannot pay a full rebuild; it pays what it holds and the hull sails again
    // (economy::rebuild), so a run of losses only ends when the act's clock runs out.
    assert!(rep.operations.len() > 1, "{}", rep.operations.len());
    assert_eq!(rep.hulls_lost as usize, rep.operations.len());
    assert_eq!(rep.acts_lost, vec![Act::Act1]);
    assert!(rep.capstone_done.is_empty());
    assert!(rep.theatre_hours <= 24 * 60);
}

#[test]
fn an_aborting_resolver_cannot_hang_the_bot() {
    let d = data();
    let mut r = Scripted::new(Outcome::Aborted);
    let rep = run_campaign(&BotConfig { max_theatre_hours: 24 * 20, ..bot(Policy::Aggressive, 9) }, d, &mut r);
    assert!(rep.operations.is_empty());
    assert!(r.calls > 0);
}

#[test]
fn ironman_ends_when_the_last_hull_is_lost() {
    let d = data();
    let mut r = Scripted::new(Outcome::Defeat);
    r.lose_hull = true;
    let rep = run_campaign(&BotConfig { ironman: true, ..bot(Policy::Mixed, 2) }, d, &mut r);
    assert_eq!(rep.operations.len(), 1, "the campaign is over after the only hull is lost");
}

#[test]
fn winnability_tallies_wins_and_lengths() {
    let d = data();
    let act = d.campaign.acts.iter().find(|a| a.act == Act::Act1).expect("Act 1");
    let op = act.story_ops.first().cloned().unwrap_or_else(|| act.capstone.clone());
    let setup = player_setup(Policy::Mixed);
    let mut r = Scripted::new(Outcome::Victory);
    r.minutes = 17.0;
    let (wins, median, longest) = winnability(&op, Act::Act1, 3, &setup, &d, &mut r);
    assert_eq!((wins, median, longest), (3, 17.0, 17.0));
    let (wins, median, longest) = winnability(&op, Act::Act1, 2, &setup, &d, &mut Scripted::new(Outcome::Defeat));
    assert_eq!(wins, 0);
    assert!((median - 20.0).abs() < 1e-9 && (longest - 20.0).abs() < 1e-9);
    assert_eq!(winnability(&op, Act::Act1, 0, &setup, &d, &mut Scripted::new(Outcome::Victory)), (0, 0.0, 0.0));
    assert_eq!(winnability(&OperationId::new("op.none"), Act::Act1, 3, &setup, &d, &mut Scripted::new(Outcome::Victory)), (0, 0.0, 0.0));
    assert_eq!(winnability(&op, Act::Act1, 3, &setup, &d, &mut Scripted::new(Outcome::Aborted)), (0, 0.0, 0.0));
}

#[test]
fn single_method_starter_carries_the_method() {
    let d = data();
    let bp = campaign_starter(Policy::SingleMethod(KillMethod::Gunfire), Act::Act1, &d);
    assert!(carries(&bp, KillMethod::Gunfire, &d.catalogue));
}


/// Full physics: the stock-officer winnability check on the shipped Act 1 operations. Slow (minutes per
/// operation); run by hand: `cargo test -p sim-systems --lib theatre::bot::tests::full_physics_act1 -- --ignored --nocapture`.
/// `BP_SEEDS` sets seeds per operation (default 1), `BP_OPS` filters operation ids by substring.
#[test]
#[ignore = "full-physics sorties; minutes of run time"]
fn full_physics_act1() {
    use super::super::abstract_combat::FullPhysicsResolver;
    let d = data();
    let seeds: u32 = std::env::var("BP_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let filter = std::env::var("BP_OPS").unwrap_or_default();
    let mut ops: Vec<&content::campaign::OperationDef> = d.campaign.operations.values().filter(|o| o.act == Act::Act1 && o.kind != OperationKind::Tutorial && o.id.as_str().contains(filter.as_str())).collect();
    ops.sort_by(|a, b| a.id.cmp(&b.id));
    let setup = player_setup(Policy::Mixed);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap();
    pool.install(|| {
        // The resolver holds a non-Send factory, so it is built on the pool's thread.
        let (cat, camp) = (d.catalogue.clone(), d.campaign.clone());
        let mut resolver = FullPhysicsResolver {
            make_deps: Box::new(move || crate::sim::SimDeps { catalogue: cat.clone(), campaign: camp.clone(), designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() }),
            max_ticks: 45 * 60 * 60, last_min: 0.0,
        };
        for op in ops {
            let t0 = std::time::Instant::now();
            let (w, med, max) = winnability(&op.id, Act::Act1, seeds, &setup, &d, &mut resolver);
            eprintln!("{:<40} wins {w}/{seeds} median {med:.1} min longest {max:.1} min ({:.0} s)", op.id.as_str(), t0.elapsed().as_secs_f64());
        }
    });
}

#[test]
fn a_hull_that_fails_the_tags_is_refitted_once() {
    let d = data();
    let starter = content::campaign::load::stock_blueprint("starters/patrol15.bp.ron").expect("the 15 m starter");
    let new = NewCampaign { seed: 4, difficulty: DifficultySliders::NORMAL, ironman: false, slot_name: "s".into(), players: vec![(1, "p".into(), starter.clone())], start_act: Act::Act1 };
    let mut t = Theatre::new_campaign(new, d.clone());
    let op = OperationId::new("op.act1.minelaying");
    assert!(!fits(&starter, &d.campaign.operations[&op], Act::Act1, &d), "the patrol boat carries no mine rail");
    if !t.state.offered.contains(&op) { t.state.offered.push(op.clone()); }
    t.state.resources.salvage = 50_000;
    let mut run = Run { rep: BotReport::default(), mem: Memory::default(), retreat_acts: BTreeSet::new(), dock_spend: 0 };
    let started = run.refit_for(&mut t, &op);
    if started {
        assert!(t.state.players[0].ready_at_hour.is_some(), "the refit takes dock time");
        assert!(run.dock_spend > 0 && t.state.resources.salvage < 50_000);
        assert!(fits(&t.state.players[0].blueprint, &d.campaign.operations[&op], Act::Act1, &d));
        assert_eq!(run.mem.intended, Some(op.clone()));
        assert_eq!(decide(&t, Policy::Mixed, 4, &run.mem), TheatreAction::Refit, "nothing sails while the dock works");
    }
    // Asked once per hull and operation.
    assert!(!run.refit_for(&mut t, &op));
    eprintln!("refit started: {started}");
}

/// Prints an Act 1 run per policy with every sortie won by script, for reading the economy, Alert and doctrine
/// numbers by eye: `cargo test -p sim-systems --lib scripted_act1_summary -- --ignored --nocapture`.
#[test]
#[ignore = "prints numbers for a reader; asserts nothing new"]
fn scripted_act1_summary() {
    let d = data();
    for policy in [Policy::Aggressive, Policy::Careful, Policy::Mixed, Policy::SingleMethod(KillMethod::Missile)] {
        for seed in [1u64, 2, 3] {
            let rep = run_campaign(&bot(policy, seed), d.clone(), &mut Scripted::new(Outcome::Victory));
            let alerts: Vec<String> = rep.operations.iter().map(|o| format!("{:.1}", o.alert_after)).collect();
            eprintln!(
                "{policy:?} seed {seed}: {} ops in {} h, income {} spend {} ratio {:.2}, peak Alert {:.1} (5 reached: {}), cards {} arrived {} delayed {} cancelled, capstone {:?}, Alert after each op [{}]",
                rep.operations.len(), rep.theatre_hours, rep.total_income, rep.total_spend, rep.income_spend_ratio, rep.peak_alert, rep.reached_alert_5,
                rep.cards_arrived, rep.cards_delayed, rep.cards_cancelled, rep.capstone_done, alerts.join(" ")
            );
        }
    }
}

/// One full-physics sortie per Act 1 operation (filtered by BP_OPS), printing why it ended:
/// `cargo test --release -p sim-systems --lib why_act1 -- --ignored --nocapture`.
#[test]
#[ignore = "full physics; prints for a reader"]
fn why_act1() {
    use super::super::abstract_combat::FullPhysicsResolver;
    let d = data();
    let filter = std::env::var("BP_OPS").unwrap_or_default();
    let minutes: u64 = std::env::var("BP_MIN").ok().and_then(|s| s.parse().ok()).unwrap_or(45);
    let mut ops: Vec<&content::campaign::OperationDef> = d.campaign.operations.values().filter(|o| o.act == Act::Act1 && o.kind != OperationKind::Tutorial && o.id.as_str().contains(filter.as_str())).collect();
    ops.sort_by(|a, b| a.id.cmp(&b.id));
    let setup = player_setup(Policy::Mixed);
    let (cat, camp) = (d.catalogue.clone(), d.campaign.clone());
    let mut resolver = FullPhysicsResolver {
        make_deps: Box::new(move || crate::sim::SimDeps { catalogue: cat.clone(), campaign: camp.clone(), designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() }),
        max_ticks: minutes * 60 * 60, last_min: 0.0,
    };
    for op in ops {
        let starter = super::op_starter(op, Act::Act1, &d);
        let Some(config) = super::winnability_config(&op.id, Act::Act1, 0, &starter, &d) else { eprintln!("{}: no config", op.id.as_str()); continue };
        let hostiles = config.vessels.iter().filter(|v| v.role == VesselRole::Combatant).count();
        let r = resolver.resolve(&config, &setup, &d);
        let p = r.players.first();
        eprintln!(
            "{:<32} sea {} {:?} winner {:?} obj_done {} {:.1} min | vessels {} combatants {} kills {} (by player {}) | hull_lost {:?} dropped {:?} fuel {:.2} | sightings {} evidence {} photos {} mines {} site {} tender_lost {} passengers {} captured {}",
            op.id.as_str(), config.weather.sea_state.0, r.outcome, r.winner, r.objective_done, resolver.last_min, config.vessels.len(), hostiles, r.kills.len(), r.kills.iter().filter(|k| k.by_player.is_some()).count(),
            p.map(|p| p.hull_lost), p.map(|p| p.dropped), p.map_or(0.0, |p| p.fuel_fraction), r.sightings, r.evidence, r.photos, r.mines_laid, r.site_destroyed, r.tender_lost, r.passengers_delivered, r.crew_captured
        );
        if std::env::var("BP_DETAIL").is_ok() { eprintln!("  damage {:?}\n  stats {:?}\n  starter {}", p.map(|p| &p.damage), r.stats, starter.header.name); }
    }
}

/// L317's "its starter design" for an operation that needs a kit no bare starter carries: the starter the
/// Quartermaster fits for it, and the kit is open when the operation can first be offered.
#[test]
fn kit_operations_get_a_fitted_starter_and_open_kit() {
    use content::campaign::ConstraintTag;
    let d = data();
    for id in ["op.act1.minelaying", "op.act1.prize_capture", "op.act1.cutting_out", "op.act1.story.bursars_berth"] {
        let op = &d.campaign.operations[&OperationId::new(id)];
        let bp = op_starter(op, Act::Act1, &d);
        assert!(fits(&bp, op, Act::Act1, &d), "{id}: the starter is fitted for the operation's tags");
    }
    // No Act 1 operation needs a part before its unlock opens that part's tier.
    for op in d.campaign.operations.values().filter(|o| o.act == Act::Act1 && o.kind != OperationKind::Tutorial) {
        let open = op_open_parts(op, Act::Act1, &d);
        for t in &op.tags {
            let ConstraintTag::Requires(tag) = t else { continue };
            let has = open.iter().filter_map(|p| d.catalogue.parts.get(p)).any(|def| def.tags.contains(tag));
            assert!(has, "{}: requires {tag:?}, which no part open at tier {:?} carries", op.id.as_str(), op_open_tier(op, Act::Act1, &d));
        }
    }
}

/// One full-physics sortie traced every `BP_EVERY` seconds (30): own hull, the nearest true hostiles and
/// whether the player picture holds them, the player's tracks, every mount's engagement state and the
/// emitters. `BP_OPS` (one operation id substring), `BP_SEED` (0), `BP_MIN` (45); `BP_DROP`, `BP_PILOT` and
/// `BP_EMCON` as in `gate::gate_act1`. `BP_ALL` lists every other hull's position and status, `BP_GROUPS` every AI group's orders,
/// mode and memory.
/// `cargo test --release -p sim-systems --lib trace_act1 -- --ignored --nocapture`.
#[test]
#[ignore = "full physics; prints for a reader"]
fn trace_act1() {
    use super::super::abstract_combat::{prepare_config, Pilot};
    use crate::editor::trials::HeadlessPolicy;
    use crate::protocol::snapshot::Viewer;
    use content::campaign::MarkerKind;
    use sim_core::PhysEvent;
    let shipped = data();
    let act1: Vec<OperationId> = shipped.campaign.operations.values().filter(|o| o.act == Act::Act1).map(|o| o.id.clone()).collect();
    let (d, _) = gate::dropped_data(&shipped, &act1);
    let filter = std::env::var("BP_OPS").unwrap_or_default();
    let seed: u32 = std::env::var("BP_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minutes: u64 = std::env::var("BP_MIN").ok().and_then(|s| s.parse().ok()).unwrap_or(45);
    let every: u64 = std::env::var("BP_EVERY").ok().and_then(|s| s.parse().ok()).unwrap_or(30);
    let op = d.campaign.operations.values().filter(|o| o.act == Act::Act1 && o.kind != OperationKind::Tutorial && o.id.as_str().contains(filter.as_str())).min_by(|a, b| a.id.cmp(&b.id)).expect("an operation");
    let starter = super::op_starter(op, Act::Act1, &d);
    let config = super::winnability_config(&op.id, Act::Act1, seed, &starter, &d).expect("config");
    let setup = player_setup(Policy::Mixed);
    let mut prepared = prepare_config(&config, &setup);
    if std::env::var("BP_NOREC").is_ok() { for v in prepared.vessels.iter_mut().filter(|v| v.role == VesselRole::PlayerHull) { v.emcon = None; } }
    gate::spawn_emcon(&mut prepared);
    let mut pilot = Pilot::new(&prepared, &setup, &d).with_orders(gate::pilot_orders().1);
    let deps = crate::sim::SimDeps { catalogue: d.catalogue.clone(), campaign: d.campaign.clone(), designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() };
    eprintln!("{} seed {seed} starter {} sea {} vis {:.0} m time {:?} weather {:?} objective {:?}", op.id.as_str(), starter.header.name, prepared.weather.sea_state.0, prepared.weather.visibility_m, prepared.time_of_day, prepared.weather.kind, prepared.objective);
    for (i, v) in prepared.vessels.iter().enumerate() {
        eprintln!("  vessel {i}: side {} role {:?} class {:?} tag {:?} pos ({:.0}, {:.0}) arrives {} s", v.side.0, v.role, v.class.as_ref().map(|c| c.as_str()), v.tag, v.pos.x, v.pos.z, v.arrives_s);
    }
    for g in &prepared.groups { eprintln!("  group {:?}: side {} kind {:?} tag {:?} ace {:?} orders {:?}", g.id, g.side.0, g.kind, g.tag, g.ace.as_ref().map(|a| a.as_str()), g.orders); }
    for m in &prepared.map.markers { eprintln!("  marker {} {:?} ({:.0}, {:.0})", m.name, m.kind, m.pos_m[0], m.pos_m[1]); }
    {
        let terrain = sim_core::terrain::Terrain::new(prepared.map.clone());
        let a = DVec2::new(prepared.vessels[0].pos.x, prepared.vessels[0].pos.z);
        let mut dry = Vec::new();
        for m in prepared.map.markers.iter().filter(|m| matches!(m.kind, MarkerKind::Objective | MarkerKind::DepotSite | MarkerKind::RadarSite | MarkerKind::Berth | MarkerKind::ShoreLanding)) {
            let b = DVec2::from(m.pos_m);
            let shallow = (0..=200).map(|k| a + (b - a) * (k as f64 / 200.0)).filter(|p| terrain.height_at(p.x, p.y) > -3.0).count();
            dry.push(format!("{}: {} of 201 samples shallower than 3 m, at marker {:.1} m", m.name, shallow, terrain.height_at(b.x, b.y)));
        }
        eprintln!("  straight line from the spawn: {dry:?}");
    }
    let slot = prepared.players[0].slot;
    let me = prepared.players[0].side;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    pool.install(|| {
        let mut sim = crate::sim::Sim::new(prepared.clone(), deps).expect("sim");
        let mut commands = Vec::new();
        let (mut fired, mut hits_out, mut hits_in) = (0u32, 0u32, 0u32);
        let mut shooters: BTreeMap<u32, u32> = BTreeMap::new();
        for i in 0..minutes * 3600 {
            if sim.is_over() { eprintln!("over at {:.1} min: {:?}", i as f64 / 3600.0, sim.result().map(|r| (r.outcome, r.objective_done))); break; }
            commands.clear();
            pilot.commands(&sim, &mut commands);
            if !commands.is_empty() {
                for c in &commands { eprintln!("  t={:.0}s cmd {:?}", i as f64 / 60.0, c.command); }
                sim.apply(&commands);
            }
            sim.set_host_load(crate::lod::HostLoad::IDLE);
            sim.step();
            let own = sim.players()[0].vessel.unwrap_or(EntityId::NONE);
            for e in sim.drain_events() {
                let crate::protocol::event::SimEvent::Phys(p) = &e.event else { continue };
                match p {
                    PhysEvent::ProjectileSpawned(ps) if ps.shooter == own => fired += 1,
                    PhysEvent::ShellImpact { shooter, vessel, .. } if *shooter == own && *vessel != own => hits_out += 1,
                    PhysEvent::ShellImpact { shooter, vessel, .. } if *vessel == own => { hits_in += 1; *shooters.entry(shooter.index).or_default() += 1; }
                    PhysEvent::StatusChanged { vessel, status } => eprintln!("  t={:.0}s status e{} {:?}", i as f64 / 60.0, vessel.index, status),
                    PhysEvent::CellsDamaged { vessel, changes, cause, attacker } if *vessel == own && !matches!(cause, sim_core::damage::DamageCause::Shell) => eprintln!("  t={:.1}s own cells damaged {} by {:?} attacker e{}", i as f64 / 60.0, changes.len(), cause, attacker.index),
                    PhysEvent::BodyImpact { body, vessel, cause, .. } if *vessel == own => eprintln!("  t={:.1}s body e{} hit own {:?}", i as f64 / 60.0, body.index, cause),
                    PhysEvent::Collision { a, b, impulse, .. } if (*a == own || *b == own) && *impulse > 1.0e4 => eprintln!("  t={:.1}s collision e{} e{} impulse {:.0}", i as f64 / 60.0, a.index, b.index, impulse),
                    PhysEvent::Grounded { vessel, .. } if *vessel == own => eprintln!("  t={:.1}s own grounded", i as f64 / 60.0),
                    PhysEvent::BodySpawned { body, kind, side, launcher, .. } if *side != me => eprintln!("  t={:.1}s enemy body e{} kind {:?} from e{}", i as f64 / 60.0, body.index, kind, launcher.index),
                    _ => {}
                }
            }
            if i % (every * 60) != 0 { continue; }
            let snap = sim.snapshot(Viewer::Player(slot));
            let Some(ship) = snap.ship.as_ref() else { eprintln!("t={:.0}s no ship", i as f64 / 60.0); continue };
            let Some(mv) = snap.vessels.iter().find(|v| v.entity == ship.vessel) else { continue };
            let pos = mv.kin.pose.pos;
            eprintln!("t={:.0}s pos ({:.0}, {:.0}) ground {:.1} m speed {:.1}/{:.1} kn hdg {:.0} route {} desig {} emcon {:?} radiating {:?} cells lost {:.3} | fired {fired} hits out {hits_out} in {hits_in} from {shooters:?}",
                i as f64 / 60.0, pos.x, pos.z, sim.world().terrain().height_at(pos.x, pos.z), ship.speed_kn, ship.ordered_speed_kn, ship.heading_rad.to_degrees(), ship.route.len(), ship.designated.0, ship.board.emcon, ship.board.radiating, ship.cells_lost);
            let picture = sim.pictures().side(me);
            let mut truths: Vec<(f64, EntityId)> = sim.world().vessels().filter(|v| v.1.side != me).map(|v| ((v.1.kin.pose.pos - pos).length(), v.1.id)).collect();
            truths.sort_by(|a, b| a.0.total_cmp(&b.0));
            if std::env::var("BP_GROUPS").is_ok() {
                for g in &sim.ai.groups {
                    let lead = g.leader.and_then(|l| sim.world().vessel(l)).map(|v| (v.kin.pose.pos.x.round(), v.kin.pose.pos.z.round(), (v.forward_speed() / sim_core::time::KN).round()));
                    eprintln!("    group {:?} tag {:?} orders {:?} mode {:?} contact {} leader {lead:?} mem {:?}", g.id, g.tag, g.orders, g.mode, g.contact.0, g.mem);
                }
            }
            if std::env::var("BP_ALL").is_ok() {
                let all: Vec<String> = sim.world().vessels().filter(|v| v.1.side != me).map(|v| format!("e{} ({:.0},{:.0}) {:?}", v.1.id.index, v.1.kin.pose.pos.x, v.1.kin.pose.pos.z, v.1.status)).collect();
                eprintln!("    all {all:?}");
            }
            for (r, id) in truths.iter().take(4) {
                let v = sim.world().vessel(*id).unwrap();
                let held: Vec<String> = picture.map(|p| p.tracks.iter().filter(|t| t.is_live() && t.truth == *id).map(|t| format!("t{} {:?}/{:?}/{:?}", t.id.0, t.stance, t.rung, t.grade)).collect()).unwrap_or_default();
                eprintln!("    truth e{} {:?} side {} {:.0} m {:?} speed {:.1} kn lost {:.3} | held {held:?}", id.index, v.class.as_ref().map(|c| c.as_str()), v.side.0, r, v.status, v.forward_speed() / sim_core::time::KN, v.cells_lost_fraction());
            }
            for t in snap.tracks.iter().filter(|t| !t.last_known).take(8) {
                eprintln!("    track t{} {:?} {:?} {:?} {:?} {:?} {:.0} m sources {:?} engaged {}", t.id.0, t.domain, t.stance, t.stance_reason, t.rung, t.grade, (t.pos - pos).length(), t.sources, t.engaged_by);
            }
            for m in &ship.mounts { eprintln!("    mount {} {:?} eff {:.0}", m.slot.0, m.status, m.effective_range_m); }
            for a in &ship.actions { eprintln!("    action {:?} {:?} progress {:.2} block {:?} at {:?}", a.action, a.state, a.progress, a.block, a.at); }
            for o in &snap.objectives { eprintln!("    objective ghost {} {:?} progress {:.2}", o.ghost, o.state, o.progress); }
        }
        eprintln!("end: {:?}", sim.result().map(|r| (r.outcome, r.objective_done, r.duration_ticks)));
    });
}

/// Which Act 1 starters pass each operation's briefing tags on the analytic screen, and the one the
/// winnability check sails: `cargo test --release -p sim-systems --lib starter_fit_table -- --ignored --nocapture`.
#[test]
#[ignore = "prints for a reader"]
fn starter_fit_table() {
    let d = data();
    let list = super::act_starters(Act::Act1, &d);
    let mut ops: Vec<&content::campaign::OperationDef> = d.campaign.operations.values().filter(|o| o.act == Act::Act1 && o.kind != OperationKind::Tutorial).collect();
    ops.sort_by(|a, b| a.id.cmp(&b.id));
    for op in ops {
        let fit: Vec<String> = list.iter().map(|(n, b)| format!("{}={}", n, super::fits(b, op, Act::Act1, &d))).collect();
        let sails = super::op_starter(op, Act::Act1, &d);
        eprintln!("{:<34} tags {:?} | sails {} (fits {}) | {fit:?}", op.id.as_str(), op.tags, sails.header.name, super::fits(&sails, op, Act::Act1, &d));
    }
}
