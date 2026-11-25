//! Hand-run trace of one skirmish to its end, built exactly as the client builds it (`modes::skirmish_config`).
//! `cargo test -p sim-systems --release --lib combat_trace -- --ignored --nocapture` with env:
//! `BP_STARTER` (starters/fac22.bp.ron), `BP_ENEMIES` (1), `BP_TEMPLATE` (index into `MissionType::ALL`, 0),
//! `BP_ACT` (1), `BP_SEED` (3), `BP_MIN` (45), `BP_PLAYER` (`engage`: steer at the nearest hostile track and
//! designate it; `none`: no orders), `BP_EVERY` (status line period in seconds, 60), `BP_QUIET` (skip events).
use super::*;
use crate::modes::{LobbyPlayer, LobbySetup, SkirmishOptions};
use crate::protocol::command::Command;
use crate::protocol::event::SimEvent;
use crate::protocol::snapshot::Viewer;
use content::campaign::MissionType;
use content::weather::{SeaState, WeatherKind};
use sim_core::PhysEvent;

fn env<T: core::str::FromStr>(k: &str, d: T) -> T { std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d) }

#[test]
#[ignore = "full skirmish trace"]
fn combat_trace() {
    let catalogue = Arc::new(Catalogue::load_embedded().expect("catalogue"));
    let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign"));
    let starter = std::env::var("BP_STARTER").unwrap_or_else(|_| "starters/fac22.bp.ron".into());
    let bp = crate::editor::splash::canonical(&content::campaign::load::stock_blueprint(&starter).expect("starter"));
    let act = match env("BP_ACT", 1u8) { 2 => Act::Act2, 3 => Act::Act3, 4 => Act::Act4, _ => Act::Act1 };
    let template = MissionType::ALL[env("BP_TEMPLATE", 0usize).min(10)];
    let seed: u64 = env("BP_SEED", 3);
    let per: u8 = env("BP_ENEMIES", 1);
    let minutes: u64 = env("BP_MIN", 45);
    let every: u64 = env("BP_EVERY", 60);
    let player_mode = std::env::var("BP_PLAYER").unwrap_or_else(|_| "engage".into());
    let quiet = std::env::var("BP_QUIET").is_ok();
    let seat = LobbyPlayer { slot: PlayerSlot(0), client_id: 1, name: "p0".into(), team: 0, blueprint: Some(bp.header.hash), ready: true, complexity: 0.0 };
    let setup = LobbySetup { mode: GameMode::Skirmish, ruleset: content::ruleset::Ruleset::campaign(act), seed, players: vec![seat], skirmish: Some(SkirmishOptions { template, act, enemy_per_player: per, difficulty: Default::default() }), weather: WeatherKind::Clear, sea_state: SeaState(env("BP_SEA", 3u8)), time_of_day: TimeOfDay::Day, map: None, at_1_0: false };
    let config = crate::modes::skirmish_config(&setup, std::slice::from_ref(&bp), &campaign, &catalogue);
    eprintln!("starter {} template {:?} act {:?} seed {} objective {:?} limit {:?}", bp.header.name, template, act, seed, config.objective, config.time_limit_s);
    for v in &config.vessels { eprintln!("  vessel side {} role {:?} class {:?} at ({:.0},{:.0}) hdg {:.0} kn {:.0} arrives {} declared {:?}", v.side.0, v.role, v.class, v.pos.x, v.pos.z, v.heading_rad.to_degrees(), v.speed_kn, v.arrives_s, v.declared); }
    for g in &config.groups { eprintln!("  group {:?} {:?} {:?} {:?}", g.id, g.kind, g.formation, g.orders); }
    let deps = SimDeps { catalogue, campaign, designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() };
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap();
    pool.install(|| {
        let mut sim = Sim::new(config, deps).expect("starts");
        let me = sim.players()[0].vessel.expect("player hull");
        let mut seq = 0u32;
        let mut counts: BTreeMap<&'static str, u32> = BTreeMap::new();
        let mut pairs: BTreeMap<(u32, u32, &'static str), u32> = BTreeMap::new();
        let mut hits_on: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
        let mut fired_by: BTreeMap<(u32, u16), u32> = BTreeMap::new();
        // Player shell splashes against the designated track's true hull: (along-range, cross-range) errors.
        let mut miss: Vec<(f64, f64, f64, f64)> = Vec::new();
        let mut designated = TrackId::NONE;
        let total = minutes * 3600;
        while sim.tick().0 < total && !sim.is_over() {
            let t = sim.tick().0;
            if player_mode == "engage" && t % 300 == 0 {
                let s = sim.snapshot(Viewer::Player(PlayerSlot(0)));
                let mypos = s.vessels.iter().find(|v| v.entity == me).map(|v| DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z));
                if let Some(p) = mypos {
                    let foe = s.tracks.iter().filter(|t| !t.last_known && matches!(t.stance, Stance::Hostile | Stance::Suspect) && t.domain == Domain::Surface)
                        .min_by(|a, b| DVec2::new(a.pos.x, a.pos.z).distance(p).total_cmp(&DVec2::new(b.pos.x, b.pos.z).distance(p)));
                    let mut cmds = Vec::new();
                    match foe {
                        Some(f) => {
                            let fp = DVec2::new(f.pos.x, f.pos.z);
                            let d = fp.distance(p);
                            // Close to 3 km, then hold a parallel course.
                            let aim = if d > 3_500.0 { fp } else { let side = (fp - p).perp().normalize_or_zero(); p + side * 2_000.0 };
                            if t % 1800 == 0 { cmds.push(Command::SetCourse { waypoints: vec![aim], speed_kn: Some(30.0), append: false }); }
                            if f.id != designated { cmds.push(Command::Designate { track: Some(f.id) }); designated = f.id; }
                        }
                        None if t % 1800 == 0 => {
                            let enemy = sim.config().map.markers.iter().find(|m| m.kind == content::campaign::MarkerKind::EnemySpawn).map(|m| DVec2::from(m.pos_m)).unwrap_or(DVec2::ZERO);
                            cmds.push(Command::SetCourse { waypoints: vec![enemy], speed_kn: Some(25.0), append: false });
                        }
                        None => {}
                    }
                    let env: Vec<CommandEnvelope> = cmds.into_iter().map(|command| { seq += 1; CommandEnvelope { player: PlayerSlot(0), seq, client_render_tick: t, rtt_ms: 0, command } }).collect();
                    sim.apply(&env);
                }
            }
            sim.step();
            let t = sim.tick().0;
            let ts = t as f64 / 60.0;
            let events = sim.drain_events();
            for e in &events {
                if let SimEvent::Phys(PhysEvent::Splash { pos, shooter, calibre_mm, .. }) = &e.event {
                    if *shooter != me || *calibre_mm < 50 { continue; }
                    let truth = sim.ships().get(me).map(|s| s.orders.designated).and_then(|d| sim.pictures().side(SideId(0)).and_then(|p| p.get(d)).map(|t| (t.truth, t.pos)));
                    let (Some((tgt, est)), Some((mp, _, _))) = (truth, sim.world().locate(me)) else { continue };
                    let Some((tp, _, _)) = sim.world().locate(tgt) else { continue };
                    let los = DVec2::new(tp.x - mp.x, tp.z - mp.z);
                    let r = los.length();
                    let u = los / r.max(1.0);
                    let d = DVec2::new(pos.x - tp.x, pos.z - tp.z);
                    let est_err = DVec2::new(est.x - tp.x, est.z - tp.z).length();
                    miss.push((r, d.dot(u), d.perp_dot(u), est_err));
                }
            }
            if t % (every * 60) == 0 && !miss.is_empty() {
                let n = miss.len() as f64;
                let mean = |f: &dyn Fn(&(f64, f64, f64, f64)) -> f64| miss.iter().map(f).sum::<f64>() / n;
                let (r, a, c, e) = (mean(&|m| m.0), mean(&|m| m.1), mean(&|m| m.2), mean(&|m| m.3));
                let sa = (miss.iter().map(|m| (m.1 - a) * (m.1 - a)).sum::<f64>() / n).sqrt();
                let sc = (miss.iter().map(|m| (m.2 - c) * (m.2 - c)).sum::<f64>() / n).sqrt();
                eprintln!("   splashes {} at range {:.0}: along {:.0} +- {:.0} m, across {:.0} +- {:.0} m, track error {:.0} m", miss.len(), r, a, sa, c, sc, e);
                miss.clear();
            }
            // One copy per physical event: the audience split sends a scrubbed copy to sides that cannot
            // see the shooter, so count each (tick, event) once, ignoring the scrubbed ids.
            let mut seen_keys: BTreeSet<String> = BTreeSet::new();
            for e in events {
                let key = match &e.event {
                    SimEvent::Phys(PhysEvent::ShellImpact { projectile, .. }) => Some(format!("si{projectile:?}")),
                    SimEvent::Phys(PhysEvent::BodyImpact { cell, world_point, .. }) => Some(format!("bi{cell:?}{world_point:?}")),
                    SimEvent::Phys(PhysEvent::Collision { point, .. }) => Some(format!("co{point:?}")),
                    SimEvent::Phys(PhysEvent::Splash { projectile, .. }) => Some(format!("sp{projectile:?}")),
                    SimEvent::Phys(p) => Some(format!("{p:?}")),
                    _ => None,
                };
                if let Some(k) = key { if !seen_keys.insert(k) { continue; } }
                let k: &'static str = match &e.event {
                    SimEvent::Phys(p) => match p {
                        PhysEvent::MountFired { vessel, slot, rounds } => { *fired_by.entry((vessel.index, slot.0)).or_default() += u32::from(*rounds); "fired" }
                        PhysEvent::ProjectileSpawned(_) => "proj",
                        PhysEvent::Splash { .. } => "splash",
                        PhysEvent::ShellImpact { vessel, outcome, .. } => { let h = hits_on.entry(vessel.index).or_default(); h.0 += 1; if matches!(outcome, sim_core::armour::HitOutcome::Penetrated | sim_core::armour::HitOutcome::PassedThrough) { h.1 += 1; } "impact" }
                        PhysEvent::BodySpawned { .. } => "body",
                        PhysEvent::BodyImpact { .. } => "body_impact",
                        PhysEvent::CellsDamaged { .. } => "cells",
                        PhysEvent::PartDamaged { .. } => "part",
                        PhysEvent::Breach { .. } => "breach",
                        PhysEvent::FireStarted { .. } => "fire",
                        PhysEvent::FireOut { .. } => "fire_out",
                        PhysEvent::CompartmentHalfFlooded { .. } => "half_flood",
                        PhysEvent::DecoyDeployed { .. } => "decoy",
                        PhysEvent::VesselLost { .. } => "lost",
                        PhysEvent::StatusChanged { .. } => "status",
                        PhysEvent::Collision { .. } => "collision",
                        _ => "phys_other",
                    },
                    SimEvent::TrackCreated { .. } => "track",
                    SimEvent::RungChanged { .. } => "rung",
                    SimEvent::StanceChanged { .. } => "stance",
                    SimEvent::Engaging { .. } => "engaging",
                    SimEvent::Holding { .. } => "holding",
                    SimEvent::OfficerMessage { .. } => "officer",
                    SimEvent::AlertRaised(_) => "alert",
                    SimEvent::CommandRejected { .. } => "rejected",
                    SimEvent::SortieEnded(_) => "ended",
                    _ => "other",
                };
                *counts.entry(k).or_default() += 1;
                let show = !quiet && match &e.event {
                    SimEvent::Phys(PhysEvent::ShellImpact { .. } | PhysEvent::MountFired { .. }) => std::env::var("BP_IMPACTS").is_ok(),
                    SimEvent::Phys(p) => matches!(p, PhysEvent::PartDamaged { .. } | PhysEvent::Breach { .. } | PhysEvent::FireStarted { .. } | PhysEvent::FireOut { .. } | PhysEvent::CompartmentHalfFlooded { .. } | PhysEvent::VesselLost { .. } | PhysEvent::StatusChanged { .. } | PhysEvent::BodySpawned { .. } | PhysEvent::BodyImpact { .. } | PhysEvent::DecoyDeployed { .. } | PhysEvent::CookOff { .. } | PhysEvent::Collision { .. } | PhysEvent::Split { .. }),
                    SimEvent::TrackCreated { side, .. } | SimEvent::RungChanged { side, .. } | SimEvent::StanceChanged { side, .. } => side.0 == 0,
                    SimEvent::Engaging { mount, .. } | SimEvent::Holding { mount, .. } => mount.vessel == me,
                    SimEvent::OfficerMessage { vessel, .. } => *vessel == me,
                    SimEvent::CommandRejected { .. } | SimEvent::SortieEnded(_) | SimEvent::ObjectiveChanged { .. } | SimEvent::AlertRaised(_) => true,

                    _ => false,
                };
                if show { eprintln!("t={ts:>7.1} {:?}", e.event); }
                if std::env::var("BP_PAIRS").is_ok() {
                    match &e.event {
                        SimEvent::Phys(PhysEvent::Collision { a, b, impulse, point }) => { let c = pairs.entry((a.index, b.index, "collide")).or_default(); *c += 1; if *c <= 3 || *c % 50 == 0 { eprintln!("t={ts:.1} collide {} {} impulse {impulse:.0} at ({:.0},{:.0})", a.index, b.index, point.x, point.z); } }
                        SimEvent::Phys(PhysEvent::ShellImpact { vessel, shooter, .. }) => *pairs.entry((shooter.index, vessel.index, "shell")).or_default() += 1,
                        _ => {}
                    }
                }
            }
            if t % (every * 60) == 0 {
                let s = sim.snapshot(Viewer::Omniscient);
                let ps = sim.snapshot(Viewer::Player(PlayerSlot(0)));
                let my = ps.vessels.iter().find(|v| v.entity == me);
                let ship = ps.ship.as_ref();
                eprintln!("== t={ts:.0}s me {:?} pos {:?} kn {:.1}/{:.1} heel {:.1} lost {:.3} mounts {:?}", my.map(|v| v.status), my.map(|v| (v.kin.pose.pos.x.round(), v.kin.pose.pos.z.round())),
                    ship.map_or(0.0, |x| x.speed_kn), ship.map_or(0.0, |x| x.ordered_speed_kn), ship.map_or(0.0, |x| x.heel_deg), ship.map_or(0.0, |x| x.cells_lost), ship.map(|x| x.mounts.iter().map(|m| (m.slot.0, m.status.clone())).collect::<Vec<_>>()));
                for g in &sim.ai.groups { eprintln!("   group {:?} mode {:?} members {:?} route {:?} orders {:?}", g.id, g.mode, g.members.iter().map(|m| m.index).collect::<Vec<_>>(), g.route, g.orders); }
                for gid in [1u32, 4] { if let Some(sh) = sim.ships().ships.iter().find(|s| s.vessel.index == gid) { eprintln!("   orders {gid}: {:?}", sh.orders); } }
                eprintln!("   comps {:?}", ship.map(|x| x.compartments.iter().filter(|c| c.fill > 0 || c.fire > 0 || c.breached).map(|c| (c.id.0, c.fill, c.fire, c.breached)).collect::<Vec<_>>()));
                for v in s.vessels.iter().filter(|v| v.entity != me) {
                    let p = my.map_or(DVec2::ZERO, |m| DVec2::new(m.kin.pose.pos.x, m.kin.pose.pos.z));
                    let d = DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z).distance(p);
                    let spd = DVec2::new(v.kin.lin_vel.x, v.kin.lin_vel.z).length() / sim_core::time::KN;
                    eprintln!("   vessel {} {:?} at {:.0} m kn {:.1} heel(roll) {:.1} lost {:.3} flood {:.0}t", v.entity.index, v.status, d, spd, v.kin.pose.rot.to_euler(glam::EulerRot::YXZ).2.to_degrees(), sim.world().vessel(v.entity).map_or(0.0, |x| x.cells_lost_fraction()), sim.world().vessel(v.entity).map_or(0.0, |x| x.compartments.compartments.iter().map(|c| c.water_m3).sum::<f64>()));
                }
                for tr in ps.tracks.iter().filter(|t| !t.last_known) { eprintln!("   track {} {:?} {:?} {:?} {:?} class {:?} cands {}", tr.id.0, tr.domain, tr.stance, tr.rung, tr.grade, tr.class, tr.candidates); }
                eprintln!("   objectives {:?}", ps.objectives.iter().map(|o| (o.ghost, o.state, o.progress)).collect::<Vec<_>>());
            }
        }
        let r = sim.result().cloned();
        eprintln!("counts {counts:?}");
        eprintln!("pairs {pairs:?}");
        eprintln!("hits per vessel (impacts, penetrations) {hits_on:?}");
        eprintln!("rounds fired by (vessel, slot) {fired_by:?}");
        match r {
            Some(r) => eprintln!("outcome {:?} winner {:?} done {} min {:.1} kills {:?} players {:?} stats {:?}", r.outcome, r.winner, r.objective_done, r.duration_ticks as f64 / 3600.0, r.kills, r.players, r.stats),
            None => eprintln!("no result after {minutes} min"),
        }
    });
}

/// Gunnery on a range: the player's starter at rest in calm water, one stock patrol boat at rest broadside at
/// `BP_RANGE` metres (5000), designated. Prints splash errors per 10 s and the hit count.
/// `cargo test -p sim-systems --lib gun_range_trace -- --ignored --nocapture`; `BP_SPEED` (own kn, 0),
/// `BP_SEA` (0), `BP_TSPEED` (target kn, 0).
#[test]
#[ignore = "gunnery trace"]
fn gun_range_trace() {
    use content::settings::{SplashOptions, SplashTarget};
    let catalogue = Arc::new(Catalogue::load_embedded().expect("catalogue"));
    let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign"));
    let starter = std::env::var("BP_STARTER").unwrap_or_else(|_| "starters/fac22.bp.ron".into());
    let bp = content::campaign::load::stock_blueprint(&starter).expect("starter");
    let range: f64 = env("BP_RANGE", 5_000.0);
    let own_kn: f64 = env("BP_SPEED", 0.0);
    let t_kn: f64 = env("BP_TSPEED", 0.0);
    let mut config = crate::editor::splash::splash_config(&bp, SplashOptions { water: SeaState(env("BP_SEA", 0u8)), target: SplashTarget::PatrolBoat }, 5, &campaign);
    config.splash = None;
    let me_pos = config.vessels[0].pos;
    let hdg = config.vessels[0].heading_rad;
    let fwd = DVec3::new(libm::sin(hdg), 0.0, -libm::cos(hdg));
    let right = DVec3::new(libm::cos(hdg), 0.0, libm::sin(hdg));
    config.vessels.truncate(2);
    config.groups.clear();
    if let Ok(path) = std::env::var("BP_TARGET") {
        config.vessels[1].blueprint = crate::protocol::config::BlueprintRef::Stock(path);
        config.vessels[1].class = None;
    }
    config.vessels[1].group = None;
    config.vessels[1].pos = me_pos + right * range;
    config.vessels[1].heading_rad = hdg;
    config.vessels[1].speed_kn = t_kn;
    config.vessels[0].speed_kn = own_kn;
    // BP_NOGATE: officers fire at any hit chance (the open-fire test passes everything), for calibration.
    let catalogue = if std::env::var("BP_NOGATE").is_ok() { let mut c = (*catalogue).clone(); c.tuning.doctrine.effective_range_pk = 0.0; Arc::new(c) } else { catalogue };
    let deps = SimDeps { catalogue, campaign, designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() };
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap();
    pool.install(|| {
        let mut sim = Sim::new(config, deps).expect("starts");
        let me = sim.players()[0].vessel.expect("hull");
        let target = sim.schedule.spawned.get(&1).copied().expect("target");
        // Hit model check: the officers' predicted chance summed over every round fired, against the hits.
        let mut expected = 0.0f64;
        let mut model_hits: BTreeMap<sim_core::projectiles::ProjectileId, f64> = BTreeMap::new();
        let mut bins: [(f64, u32, u32); 6] = [(0.0, 0, 0); 6];
        let mut seq = 0;
        let close = std::env::var("BP_CLOSE").is_ok();
        if own_kn > 0.0 {
            seq += 1;
            let far = if close { DVec2::new(me_pos.x, me_pos.z) + DVec2::new(right.x, right.z) * range } else { DVec2::new(me_pos.x, me_pos.z) + DVec2::new(fwd.x, fwd.z) * 50_000.0 };
            sim.apply(&[CommandEnvelope { player: PlayerSlot(0), seq, client_render_tick: 0, rtt_ms: 0, command: Command::SetCourse { waypoints: vec![far], speed_kn: Some(own_kn), append: false } }]);
        }
        if std::env::var("BP_GUNS_ONLY").is_ok() {
            // Hold every guided launcher by hand, so the target sees no inbound missile and does not evade.
            let held: Vec<PartSlot> = sim.ships().get(me).map(|s| s.design.mounts.iter().filter(|m| !matches!(m.weapon, content::graph::WeaponClass::Gun | content::graph::WeaponClass::PointDefenceGun)).map(|m| m.slot).collect()).unwrap_or_default();
            for slot in held { seq += 1; sim.apply(&[CommandEnvelope { player: PlayerSlot(0), seq, client_render_tick: 0, rtt_ms: 0, command: Command::TakeControl { role: crate::protocol::command::ControlRole::Mount(slot), on: true } }]); }
        }
        let mut miss: Vec<(f64, f64)> = Vec::new();
        let mut hits = 0u32;
        let mut fired = 0u32;
        let mut designated = false;
        let secs: u64 = env("BP_SECS", 90);
        let mut ended = false;
        let mut first_hits: Option<(u32, u32, f64)> = None;
        // Fire-control prediction checks: (due tick, predicted target position at impact).
        let mut pending: Vec<(u64, DVec3)> = Vec::new();
        let mut pred_err: Vec<(f64, f64)> = Vec::new();
        while sim.tick().0 < secs * 60 && !ended {
            if first_hits.is_none() && fired >= 30 { first_hits = Some((fired, hits, (expected * 10.0).round() / 10.0)); }
            // BP_CLOSE pursues: steer for the target's position every 10 s.
            if close && own_kn > 0.0 && sim.tick().0 % 600 == 300 {
                if let Some((tp, _, _)) = sim.world().locate(target) {
                    seq += 1;
                    sim.apply(&[CommandEnvelope { player: PlayerSlot(0), seq, client_render_tick: 0, rtt_ms: 0, command: Command::SetCourse { waypoints: vec![DVec2::new(tp.x, tp.z)], speed_kn: Some(own_kn), append: false } }]);
                }
            }
            if !designated {
                if let Some(tr) = sim.pictures().side(SideId(0)).and_then(|p| p.track_of(target)) {
                    let id = tr.id;
                    seq += 1;
                    sim.apply(&[CommandEnvelope { player: PlayerSlot(0), seq, client_render_tick: 0, rtt_ms: 0, command: Command::Designate { track: Some(id) } }]);
                    designated = true;
                }
            }
            sim.step();
            let now = sim.tick().0;
            if let (Some((mp, _, _)), Some((tp, _, _))) = (sim.world().locate(me), sim.world().locate(target)) {
                let u = DVec2::new(tp.x - mp.x, tp.z - mp.z).normalize_or_zero();
                for (due, p) in pending.iter().filter(|(d, _)| *d == now) { let _ = due; let d = DVec2::new(p.x - tp.x, p.z - tp.z); pred_err.push((d.dot(u), d.perp_dot(u))); }
                pending.retain(|(d, _)| *d > now);
            }
            for e in sim.drain_events() {
                match &e.event {
                    SimEvent::Phys(PhysEvent::ProjectileSpawned(ps)) if ps.shooter == me && !ps.point_defence => {
                        // The track the mount is engaging, which is the one its solution and open-fire test read.
                        let engaged = sim.ships().get(me).and_then(|s| s.stages.engage.mounts.iter().find(|m| m.slot == ps.mount)).and_then(|m| match m.status { EngageStatus::Engaging { track, .. } => Some(track), _ => None });
                        if let (Some(ship), Some(t)) = (sim.ships().get(me), engaged.and_then(|id| sim.pictures().side(SideId(0)).and_then(|p| p.get(id)))) {
                            let cat = sim.world().catalogue();
                            let g = crate::engage::GunneryFacts { tables: &ship.design.range_tables, catalogue: cat, accuracy_mult: ship.proficiency.aim_error_mult, sea: sim.config().weather.sea_state };
                            let pose = sim.world().vessel(me).map(|v| v.kin.pose).unwrap_or_default();
                            if let Some(m) = ship.design.mounts.iter().find(|m| m.slot == ps.mount && m.weapon == content::graph::WeaponClass::Gun) {
                                let muzzle = pose.to_world(m.pos_local);
                                let range = (t.predict(sim.tick().0) - muzzle).length();
                                if let Some(h) = crate::engage::hit_chance(m, t, muzzle, range, sim.tick().0, &g, &cat.tuning) { expected += h.pk; model_hits.insert(ps.id, h.pk); }
                            }
                        }
                    }
                    SimEvent::Phys(PhysEvent::MountFired { vessel, rounds, slot }) if *vessel == me => {
                        fired += u32::from(*rounds);
                        if let Some(sol) = sim.snapshot(Viewer::Player(PlayerSlot(0))).ship.and_then(|sh| sh.mounts.iter().find(|m| m.slot == *slot).and_then(|m| m.solution)) {
                            if let Some(t) = sim.pictures().side(SideId(0)).and_then(|p| p.track_of(target)) {
                                let tof = sol.time_of_flight_s as f64;
                                let (p, v) = t.gun_target(sim.tick().0);
                                pending.push((sim.tick().0 + (tof * 60.0) as u64, p + v * tof));
                            }
                        }
                        if fired == u32::from(*rounds) {
                            let d = sim.world().locate(me).zip(sim.world().locate(target)).map(|(a, b)| (a.0 - b.0).length()).unwrap_or(0.0);
                            eprintln!("t={:.1} first round from {slot:?} at {d:.0} m", sim.tick().0 as f64 / 60.0);
                        }
                        if std::env::var("BP_SHOTS").is_ok() {
                            let v = sim.world().vessel(me).expect("me");
                            let st = v.parts.iter().find(|p| p.slot == *slot).map(|p| p.state.clone());
                            let ord = v.controls.parts.get(slot.0 as usize).map(|c| c.order);
                            let r = v.kin.pose.rot.to_euler(glam::EulerRot::YXZ);
                            let tr = sim.pictures().side(SideId(0)).and_then(|p| p.track_of(target)).map(|t| (t.pos, t.vel, t.grade, t.sources));
                            let tp = sim.world().locate(target).map(|l| l.0).unwrap_or_default();
                            eprintln!("t={:.2} shot {:?} elev {:.4} train {:.4} track err {:?} vel {:?} grade {:?}", sim.tick().0 as f64 / 60.0, slot,
                                match st { Some(sim_core::parts::PartState::Mount(m)) => m.elevation_rad, _ => 0.0 }, match st { Some(sim_core::parts::PartState::Mount(m)) => m.train_rad, _ => 0.0 },
                                tr.map(|t| (t.0 - tp).round()), tr.map(|t| t.1.round()), tr.map(|t| (t.2, t.3)));
                            let _ = (ord, r);
                            let tv = sim.world().vessel(target).map(|v| v.kin.lin_vel).unwrap_or_default();
                            eprintln!("    truth vel ({:.1}, {:.1}) fc {:?}", tv.x, tv.z, sim.pictures().side(SideId(0)).and_then(|p| p.track_of(target)).and_then(|t| t.fire_control()).map(|e| (e.vel, e.var_vel.sqrt(), e.var_pos.sqrt(), e.manoeuvring)));
                        }
                    }
                    SimEvent::Phys(PhysEvent::ShellImpact { vessel, shooter, projectile, .. }) if *vessel == target && *shooter == me => {
                        hits += 1;
                        if let Some(p) = model_hits.remove(projectile) { let b = &mut bins[((p * 20.0) as usize).min(5)]; b.0 += p; b.1 += 1; b.2 += 1; }
                    }
                    SimEvent::Phys(PhysEvent::StatusChanged { vessel, status }) if *vessel == target => { eprintln!("t={:.1} target {status:?} after {hits} hits, {fired} rounds", sim.tick().0 as f64 / 60.0); ended = true; }
                    SimEvent::Phys(PhysEvent::FireStarted { vessel, .. }) if *vessel == target => eprintln!("t={:.1} target fire", sim.tick().0 as f64 / 60.0),
                    SimEvent::Phys(PhysEvent::CompartmentHalfFlooded { vessel, .. }) if *vessel == target => eprintln!("t={:.1} target half flooded", sim.tick().0 as f64 / 60.0),
                    SimEvent::Phys(PhysEvent::PartDamaged { vessel, slot, level }) if *vessel == target => eprintln!("t={:.1} target part {slot:?} {level:?}", sim.tick().0 as f64 / 60.0),
                    SimEvent::Phys(PhysEvent::Splash { pos, shooter, calibre_mm, projectile, .. }) if *shooter == me && *calibre_mm >= 50 => {
                        if let Some(p) = model_hits.remove(projectile) { let b = &mut bins[((p * 20.0) as usize).min(5)]; b.0 += p; b.1 += 1; }
                        let (Some((mp, _, _)), Some((tp, _, _))) = (sim.world().locate(me), sim.world().locate(target)) else { continue };
                        let u = DVec2::new(tp.x - mp.x, tp.z - mp.z).normalize_or_zero();
                        let d = DVec2::new(pos.x - tp.x, pos.z - tp.z);
                        miss.push((d.dot(u), d.perp_dot(u)));
                        if std::env::var("BP_SHOTS").is_ok() { eprintln!("t={:.2} splash along {:.0} across {:.0}", sim.tick().0 as f64 / 60.0, d.dot(u), d.perp_dot(u)); }
                    }
                    SimEvent::Holding { mount, reason, .. } if mount.vessel == me => eprintln!("t={:.1} hold {:?} {:?}", sim.tick().0 as f64 / 60.0, mount.slot, reason),
                    SimEvent::Engaging { mount, reason, .. } if mount.vessel == me => eprintln!("t={:.1} engage {:?} {:?}", sim.tick().0 as f64 / 60.0, mount.slot, reason),
                    _ => {}
                }
            }
            if sim.tick().0 % 600 == 0 {
                let s = sim.snapshot(Viewer::Player(PlayerSlot(0)));
                let sh = s.ship.as_ref();
                let sol: Vec<_> = sh.map(|x| x.mounts.iter().filter_map(|m| m.solution.map(|so| (m.slot.0, so.time_of_flight_s, so.iterations, so.converged, so.error_m))).collect()).unwrap_or_default();
                let n = miss.len().max(1) as f64;
                let a = miss.iter().map(|m| m.0).sum::<f64>() / n;
                let c = miss.iter().map(|m| m.1).sum::<f64>() / n;
                let sa = (miss.iter().map(|m| (m.0 - a) * (m.0 - a)).sum::<f64>() / n).sqrt();
                let sc = (miss.iter().map(|m| (m.1 - c) * (m.1 - c)).sum::<f64>() / n).sqrt();
                eprintln!("t={:>3}s fired {fired} hits {hits} splashes {} along {a:.0} +- {sa:.0} across {c:.0} +- {sc:.0} heel {:.2} trim {:.2} kn {:.1} sol {sol:?}", sim.tick().0 / 60, miss.len(), sh.map_or(0.0, |x| x.heel_deg), sh.map_or(0.0, |x| x.trim_deg), sh.map_or(0.0, |x| x.speed_kn));
                miss.clear();
                let tid = sim.pictures().side(SideId(0)).and_then(|p| p.track_of(target)).map(|t| t.id);
                if let Some(v) = sim.world().vessel(target) { eprintln!("    target heading {:.0} deg vel ({:.1}, {:.1}) status {:?} rcs {:?}", v.kin.pose.heading_rad().to_degrees(), v.kin.lin_vel.x, v.kin.lin_vel.z, v.status, sim.pictures().side(SideId(0)).and_then(|p| p.track_of(target)).map(|t| t.rcs_m2)); }
                eprintln!("    first 30 rounds: {first_hits:?}; model expects {expected:.1} hits so far, by predicted chance (0-5%, 5-10%, .., 25%+: sum p, rounds, hits) {:?}", bins.iter().map(|b| (b.0.round(), b.1, b.2)).collect::<Vec<_>>());
                if let (Some(ship), Some(t)) = (sim.ships().get(me), sim.pictures().side(SideId(0)).and_then(|p| p.track_of(target))) {
                    let cat = sim.world().catalogue();
                    let g = crate::engage::GunneryFacts { tables: &ship.design.range_tables, catalogue: cat, accuracy_mult: ship.proficiency.aim_error_mult, sea: sim.config().weather.sea_state };
                    let pose = sim.world().vessel(me).map(|v| v.kin.pose).unwrap_or_default();
                    for m in ship.design.mounts.iter().filter(|m| m.weapon == content::graph::WeaponClass::Gun) {
                        let muzzle = pose.to_world(m.pos_local);
                        let range = (t.predict(sim.tick().0) - muzzle).length();
                        if let Some(h) = crate::engage::hit_chance(m, t, muzzle, range, sim.tick().0, &g, &cat.tuning) {
                            eprintln!("    model {:?} at {range:.0} m: pk {:.3} errors {:?} box {:?} est {:?}", m.slot, h.pk, h.errors, h.target, t.fire_control().map(|e| (e.manoeuvring, e.accel_mps2, e.manoeuvre_mps2)));
                        }
                    }
                }
                if !pred_err.is_empty() {
                    let n = pred_err.len() as f64;
                    let (a, c) = (pred_err.iter().map(|e| e.0).sum::<f64>() / n, pred_err.iter().map(|e| e.1).sum::<f64>() / n);
                    let (sa, sc) = ((pred_err.iter().map(|e| (e.0 - a) * (e.0 - a)).sum::<f64>() / n).sqrt(), (pred_err.iter().map(|e| (e.1 - c) * (e.1 - c)).sum::<f64>() / n).sqrt());
                    eprintln!("    prediction error at impact ({} rounds): along {a:.1} +- {sa:.1} across {c:.1} +- {sc:.1}", pred_err.len());
                    pred_err.clear();
                }
                eprintln!("    target track {tid:?} mounts {:?}", sh.map(|x| x.mounts.iter().map(|m| (m.slot.0, m.status.clone())).collect::<Vec<_>>()));
                if let Some(p) = sim.pictures().side(SideId(0)) { for t in p.tracks.iter().filter(|t| t.is_live()) { eprintln!("    track {} {:?} {:?} at ({:.0},{:.0}) truth {:?}", t.id.0, t.domain, t.stance, t.pos.x, t.pos.z, t.truth); } }
            }
        }
    });
}

/// Network probe: every stock Combine and starter hull afloat for 40 s in a calm basin; prints each part that
/// is not on its networks. `cargo test -p sim-systems --lib network_probe -- --ignored --nocapture`.
#[test]
#[ignore = "network probe"]
fn network_probe() {
    use content::settings::{SplashOptions, SplashTarget};
    use sim_core::networks::LinkState;
    let catalogue = Arc::new(Catalogue::load_embedded().expect("catalogue"));
    let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign"));
    let only = std::env::var("BP_ONLY").ok();
    let mut paths: Vec<String> = content::data::STOCK_FILES.iter().map(|(p, _)| p.to_string()).filter(|p| (p.starts_with("combine/") || p.starts_with("starters/")) && p.ends_with(".bp.ron")).collect();
    paths.sort();
    for path in paths {
        if only.as_ref().is_some_and(|o| !path.contains(o.as_str())) { continue; }
        let Ok(bp) = content::campaign::load::stock_blueprint(&path) else { eprintln!("{path}: does not load"); continue };
        let mut config = crate::editor::splash::splash_config(&bp, SplashOptions { water: SeaState(env("BP_SEA", 0u8)), target: SplashTarget::PatrolBoat }, 5, &campaign);
        config.splash = None;
        config.vessels.truncate(1);
        config.groups.clear();
        let deps = SimDeps { catalogue: catalogue.clone(), campaign: campaign.clone(), designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() };
        let Ok(mut sim) = Sim::new(config, deps) else { eprintln!("{path}: sim refuses"); continue };
        let turn = std::env::var("BP_TURN").is_ok();
        if std::env::var("BP_HELM").is_ok() {
            let v = sim.world().vessel(sim.players()[0].vessel.expect("hull")).expect("v");
            let p0 = DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z);
            let h = v.kin.pose.heading_rad();
            let f = DVec2::new(libm::sin(h), -libm::cos(h));
            let r = DVec2::new(-f.y, f.x);
            let mut wps = Vec::new();
            for k in 0..6 { let side = r * (150.0 * (k + 1) as f64); wps.push(p0 + side + f * if k % 2 == 0 { 1_500.0 } else { 0.0 }); wps.push(p0 + side + r * 150.0 + f * if k % 2 == 0 { 1_500.0 } else { 0.0 }); }
            if std::env::var("BP_STRAIGHT").is_ok() { wps = vec![p0 + f * 20_000.0]; }
            sim.apply(&[CommandEnvelope { player: PlayerSlot(0), seq: 1, client_render_tick: 0, rtt_ms: 0, command: Command::SetCourse { waypoints: wps, speed_kn: Some(60.0), append: false } }]);
        }
        let turn = turn || std::env::var("BP_HELM").is_ok();
        let manual = std::env::var("BP_TURN").is_ok();
        if manual { sim.apply(&[CommandEnvelope { player: PlayerSlot(0), seq: 1, client_render_tick: 0, rtt_ms: 0, command: Command::TakeControl { role: crate::protocol::command::ControlRole::Helm, on: true } }]); }
        let mut peak = 0.0f64;
        for k in 0..(env("BP_SECS", 40u64) * 60) {
            if manual { sim.set_input(PlayerSlot(0), crate::protocol::command::PlayerInput { input_tick: k + 1, throttle: 1.0, rudder: if k < 1800 { 0.0 } else if (k / 1200) % 2 == 0 { 1.0 } else { -1.0 }, ..Default::default() }); }
            sim.step();
            if let Some(s) = sim.snapshot(Viewer::Player(PlayerSlot(0))).ship { peak = peak.max(f64::from(s.heel_deg.abs())); }
        }
        if turn { eprintln!("{path}: peak heel {peak:.1}"); }
        let me = sim.players()[0].vessel.expect("hull");
        let v = sim.world().vessel(me).expect("vessel");
        let mut bad = Vec::new();
        for p in &v.parts {
            let l = p.links;
            let d = catalogue.part_at(p.def);
            let def = d.id.as_str().to_string();
            let needs_power = d.power_priority > 0;
            if (needs_power && !matches!(l.power, LinkState::Network | LinkState::Redundant)) || matches!(l.data, LinkState::LocalControl | LinkState::Dead) {
                bad.push(format!("slot {} {} power {:?} {:.2} data {:?}", p.slot.0, def, l.power, l.power_supplied, l.data));
            }
        }
        eprintln!("{path}: {:?} heel {:.1}, {} off-network parts", v.status, sim.snapshot(Viewer::Player(PlayerSlot(0))).ship.map_or(0.0, |s| s.heel_deg), bad.len());
        if std::env::var("BP_DETAIL").is_ok() {
            for c in &v.compartments.compartments { eprintln!("    comp {} cells {} min {:?} max {:?} zones {:?}", c.id.0, c.cells.len(), c.min, c.max, v.networks.zones.get(c.id.0 as usize)); }
            for p in &v.parts { eprintln!("    part slot {} {} at {:?} comp {:?}", p.slot.0, catalogue.part_at(p.def).id.as_str(), p.pos, p.compartment.map(|c| c.0)); }
            for cd in &v.networks.conduits { eprintln!("    conduit {:?} {} -> {} red {} cells {}", cd.network, cd.from.0, cd.to.0, cd.redundant, cd.cells.len()); }
        }
        for b in bad { eprintln!("    {b}"); }
    }
}
