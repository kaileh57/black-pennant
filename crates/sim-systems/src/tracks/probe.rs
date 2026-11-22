//! Track-picture measurement over one Act 1 sortie under the stock-officer gate's pilot (DESIGN.md L317), for
//! the L467 association gate: track events the player's log shows, swaps per track update, and duplicate runs.
//!
//! `cargo test --release -p sim-systems --lib track_event_probe -- --ignored --nocapture` with `BP_OPS`
//! (an operation id substring, default `feint`), `BP_SEED` (0) and `BP_MIN` (25 simulated minutes).
use crate::prelude::*;
use crate::editor::trials::HeadlessPolicy;
use crate::protocol::event::SimEvent;
use crate::theatre::abstract_combat::{prepare_config, Pilot};
use crate::theatre::bot::{op_starter, player_setup, winnability_config, Policy};
use crate::theatre::TheatreData;
use content::campaign::OperationKind;
use content::enums::Act;
use std::sync::Arc;

#[test]
#[ignore = "a measurement: run with --release --ignored --nocapture"]
fn track_event_probe() {
    let catalogue = Arc::new(content::catalogue::Catalogue::load_embedded().expect("catalogue"));
    let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign data"));
    let d = TheatreData { campaign, catalogue };
    let filter = std::env::var("BP_OPS").unwrap_or_else(|_| "feint".into());
    let seed: u32 = std::env::var("BP_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minutes: u64 = std::env::var("BP_MIN").ok().and_then(|s| s.parse().ok()).unwrap_or(25);
    let op = d.campaign.operations.values().filter(|o| o.act == Act::Act1 && o.kind != OperationKind::Tutorial && o.id.as_str().contains(filter.as_str())).min_by(|a, b| a.id.cmp(&b.id)).expect("an operation");
    let starter = op_starter(op, Act::Act1, &d);
    let config = winnability_config(&op.id, Act::Act1, seed, &starter, &d).expect("config");
    let setup = player_setup(Policy::Mixed);
    let mut prepared = prepare_config(&config, &setup);
    // `BP_EMCON=active|burst|silent`: the player hull's spawn setting (as the gate's switch of that name).
    let emcon = match std::env::var("BP_EMCON").as_deref() { Ok("active") => Some(Emcon::Active), Ok("burst") => Some(Emcon::Burst), Ok("silent") => Some(Emcon::Silent), _ => None };
    if emcon.is_some() { for v in prepared.vessels.iter_mut().filter(|v| v.role == crate::protocol::config::VesselRole::PlayerHull) { v.emcon = emcon; } }
    // `BP_HELD=1`: every 30 s, the other sides' tracks of the player hull (truth) or within 3 km of it.
    let held_dump = std::env::var("BP_HELD").is_ok();
    let mut pilot = Pilot::new(&prepared, &setup, &d);
    let deps = crate::sim::SimDeps { catalogue: d.catalogue.clone(), campaign: d.campaign.clone(), designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() };
    let me = prepared.players[0].side;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().expect("pool");
    pool.install(|| {
        let mut sim = crate::sim::Sim::new(prepared.clone(), deps).expect("sim");
        let mut commands = Vec::new();
        let (mut created, mut merged, mut dropped, mut rung, mut stance) = (0u64, 0u64, 0u64, 0u64, 0u64);
        let mut seen: std::collections::BTreeSet<(u64, String)> = std::collections::BTreeSet::new();
        let mut by_domain: BTreeMap<String, u64> = BTreeMap::new();
        let mut first_src: BTreeMap<String, u64> = BTreeMap::new();
        let mut new_ids: Vec<TrackId> = Vec::new();
        let mut seen_track: BTreeMap<(u8, u32), (EntityId, u64)> = BTreeMap::new();
        let (mut updates, mut swaps) = (0u64, 0u64);
        let mut since: BTreeMap<(u8, EntityId), u64> = BTreeMap::new();
        let mut worst: BTreeMap<(u8, EntityId), u64> = BTreeMap::new();
        let mut ticks = 0u64;
        for i in 0..minutes * 3600 {
            if sim.is_over() { eprintln!("over at {:.1} min", i as f64 / 3600.0); break; }
            ticks = i + 1;
            commands.clear();
            pilot.commands(&sim, &mut commands);
            if !commands.is_empty() { sim.apply(&commands); }
            sim.set_host_load(crate::lod::HostLoad::IDLE);
            sim.step();
            for e in sim.drain_events() {
                let key = (e.tick, format!("{:?}", e.event));
                match &e.event {
                    SimEvent::TrackCreated { side, track, domain } if *side == me && seen.insert(key.clone()) => { created += 1; *by_domain.entry(format!("{domain:?}")).or_default() += 1; new_ids.push(*track); }
                    SimEvent::TrackMerged { side, .. } if *side == me && seen.insert(key.clone()) => merged += 1,
                    SimEvent::TrackDropped { side, .. } if *side == me && seen.insert(key.clone()) => dropped += 1,
                    SimEvent::RungChanged { side, .. } if *side == me && seen.insert(key.clone()) => rung += 1,
                    SimEvent::StanceChanged { side, .. } if *side == me && seen.insert(key.clone()) => stance += 1,
                    _ => {}
                }
            }
            if let Some(p) = sim.pictures().side(me) {
                for id in new_ids.drain(..) { if let Some(t) = p.get(id) { *first_src.entry(format!("{:?}", t.sources)).or_default() += 1; } }
            }
            if held_dump && i % 1800 == 0 {
                let own = sim.players()[0].vessel.unwrap_or(EntityId::NONE);
                let at = sim.world().vessel(own).map(|v| v.kin.pose.pos).unwrap_or(DVec3::ZERO);
                let world = sim.world();
                let what = |e: EntityId| if e == own { "player".to_string() } else if let Some(v) = world.vessel(e) { format!("vessel{}", v.side.0) } else if world.bodies().row_of(e).is_some() { "body".into() } else if world.decoys().entity.contains(&e) { "decoy".into() } else { "gone".into() };
                let nearest_hostile = world.vessels().filter(|(_, v)| v.side != me && matches!(v.status, sim_core::vessel::VesselStatus::Active)).map(|(_, v)| (v.kin.pose.pos - at).length()).fold(f64::INFINITY, f64::min);
                let mut lines = Vec::new();
                for (si, p) in sim.pictures().sides.iter().enumerate().filter(|(si, _)| *si as u8 != me.0) {
                    for t in p.tracks.iter().filter(|t| t.is_live() && (t.truth == own || (t.pos - at).length() < 3_000.0) && t.stance != Stance::Friend) {
                        lines.push(format!("s{si} t{} {} {:?}/{:?} {:?} {:?} bo={} age={}s err={:.0}m", t.id.0, what(t.truth), t.stance, t.stance_reason, t.grade, t.sources, t.bearing_only.is_some(), (i + 1).saturating_sub(t.last_update_tick) / 60, (t.pos - at).length()));
                    }
                }
                let emcon = sim.ships().ships.iter().find(|s| s.vessel == own).map(|s| s.settings.emcon);
                eprintln!("HELD t={}s emcon={emcon:?} nearest_hostile={:.0}m tracks={lines:?}", i / 60, nearest_hostile);
                let mut hulls: Vec<String> = Vec::new();
                for s in sim.ships().ships.iter().filter(|s| s.side != me) {
                    let Some(v) = world.vessel(s.vessel) else { continue };
                    let r = (v.kin.pose.pos - at).length();
                    if r > 12_000.0 { continue; }
                    let c = s.captain.as_ref();
                    hulls.push(format!("e{} {:.0}m {:?} {:?} cap={:?} dir={:?} tgt={:?}", s.vessel.index, r, v.class.as_ref().map(|c| c.as_str()), s.settings.emcon, c.map(|c| (c.behaviour, c.mode)), c.map(|c| c.directive.mode), c.map(|c| c.target.0)));
                }
                eprintln!("HULLS t={}s {hulls:?}", i / 60);
            }
            if i % 6 != 0 { continue; }
            let now = i + 1;
            for (si, p) in sim.pictures().sides.iter().enumerate() {
                for t in p.tracks.iter().filter(|t| t.is_live()) {
                    let k = (si as u8, t.id.0);
                    if let Some((truth, upd)) = seen_track.get(&k).copied() {
                        if t.last_update_tick != upd {
                            updates += 1;
                            if !truth.is_none() && !t.truth.is_none() && truth != t.truth { swaps += 1; }
                        }
                    }
                    seen_track.insert(k, (t.truth, t.last_update_tick));
                }
            }
            // L467 duplicates, counting only tracks a sensor updated in the last 30 s (as the reference battle).
            let mut held: BTreeMap<(u8, EntityId), usize> = BTreeMap::new();
            for (si, p) in sim.pictures().sides.iter().enumerate() {
                for t in p.tracks.iter().filter(|t| t.is_live() && !t.truth.is_none() && now.saturating_sub(t.last_update_tick) <= 30 * 60) { *held.entry((si as u8, t.truth)).or_default() += 1; }
            }
            since.retain(|k, _| held.get(k).is_some_and(|n| *n >= 2));
            for (k, _) in held.iter().filter(|(_, n)| **n >= 2) {
                let s = *since.entry(*k).or_insert(now);
                let w = worst.entry(*k).or_default();
                *w = (*w).max(now - s);
            }
        }
        let mut w: Vec<_> = worst.iter().map(|(k, v)| (*v, k.0, k.1.index)).collect();
        w.sort_unstable_by(|a, b| b.cmp(a));
        let over = w.iter().filter(|x| x.0 > 600).count();
        eprintln!("PROBE op={} seed={seed} min={:.1} player_side: created={created} merged={merged} dropped={dropped} rung={rung} stance={stance} log_lines={} | created_by_domain={by_domain:?} first_sources={first_src:?} | swaps={swaps}/{updates} ({:.3}%) | dup_truths_over_10s={over} longest={:?}",
            op.id.as_str(), ticks as f64 / 3600.0, created + merged + rung, 100.0 * swaps as f64 / updates.max(1) as f64, &w[..w.len().min(6)]);
    });
}

/// Seaway check for the Act 1 starters (not a track measurement; it lives here with the other u1 probes): each
/// starter under its own Helm at the Helm's cruise setting (no ordered speed) in sea state 6, the sea from eight
/// directions, a straight leg of 120 s then a 90 degree turn to starboard every 120 s, for `BP_SEA_S` (600) s.
/// Prints peak heel and whether the hull went over or was lost undamaged. `BP_SEED` (1) draws the wave set,
/// `BP_SEA` (6) the sea state.
/// `cargo test --release -p sim-systems --lib cruise_capsize_probe -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement: run with --release --ignored --nocapture"]
fn cruise_capsize_probe() {
    use crate::editor::trials as tr;
    use crate::protocol::command::{Command, CommandEnvelope};
    let catalogue = Arc::new(content::catalogue::Catalogue::load_embedded().expect("catalogue"));
    let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign data"));
    let seconds: f64 = std::env::var("BP_SEA_S").ok().and_then(|s| s.parse().ok()).unwrap_or(600.0);
    let sea = content::weather::SeaState(std::env::var("BP_SEA").ok().and_then(|s| s.parse().ok()).unwrap_or(6));
    let act = campaign.acts.iter().find(|a| a.act == Act::Act1).expect("Act 1");
    let wave_seed: u64 = std::env::var("BP_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().expect("pool");
    pool.install(|| {
        for path in &act.starters {
            let bp = content::campaign::load::stock_blueprint(path).expect("a stock starter");
            for k in 0..8u32 {
                let ticks = tr::ticks_of(seconds);
                let mut config = tr::trial_base(&bp, sea, wave_seed, &campaign, ticks + 1, 0.0);
                let heading = config.vessels[0].heading_rad;
                config.weather.wind_dir_rad = (heading + (45.0 * k as f64).to_radians() + core::f64::consts::PI).rem_euclid(core::f64::consts::TAU);
                let deps = crate::sim::SimDeps { catalogue: catalogue.clone(), campaign: campaign.clone(), designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() };
                let Ok(mut sim) = crate::sim::Sim::new(config, deps) else { eprintln!("SEA {path} from {} no sortie", 45 * k); continue };
                let Some(own) = tr::own_vessel(&sim) else { continue };
                let (mut peak, mut over_at, mut lost_at, mut cells, mut top_kn) = (0.0f64, None, None, 0.0f64, 0.0f64);
                // The Helm's cruise: its L1 slider (default 0.7) of the design's top speed. The trial starts the
                // hull with an ordered speed of 0, so the cruise is ordered explicitly.
                let frac = bp.officers.sliders.get(crate::officers::helm::SLIDER_CRUISE).copied().filter(|f| f.is_finite() && *f > 0.0 && *f <= 1.0).unwrap_or(0.7);
                let cruise_kn = sim.ships().ships.iter().find(|s| s.vessel == own).map_or(0.0, |s| frac * s.design.turn.top_speed_mps / sim_core::time::KN);
                let mut seq = 1;
                for i in 0..ticks {
                    if i % (120 * tr::TICKS_PER_S) == 0 {
                        // Turn 90 degrees to starboard from the current heading: a waypoint 5 km off.
                        if let Some(v) = sim.world().vessel(own) {
                            let h = v.kin.pose.heading_rad() + if i == 0 { 0.0 } else { core::f64::consts::FRAC_PI_2 };
                            let p = v.kin.pose.pos;
                            let w = DVec2::new(p.x + 5_000.0 * libm::sin(h), p.z - 5_000.0 * libm::cos(h));
                            sim.apply(&[CommandEnvelope { player: content::ids::PlayerSlot(0), seq, client_render_tick: 0, rtt_ms: 0, command: Command::SetCourse { waypoints: vec![w], speed_kn: Some(cruise_kn), append: false } }]);
                            seq += 1;
                        }
                    }
                    sim.set_host_load(crate::lod::HostLoad::IDLE);
                    sim.step();
                    drop(sim.drain_events());
                    let Some(v) = sim.world().vessel(own) else { lost_at.get_or_insert(i); break };
                    let heel = crate::editor::splash::heel_deg(&v.kin.pose).abs();
                    peak = peak.max(if heel.is_finite() { heel } else { 0.0 });
                    top_kn = top_kn.max(v.forward_speed() / sim_core::time::KN);
                    cells = v.cells_lost_fraction();
                    if heel > 90.0 { over_at.get_or_insert(i); }
                    if !matches!(v.status, sim_core::vessel::VesselStatus::Active | sim_core::vessel::VesselStatus::Disabled) { lost_at.get_or_insert(i); break; }
                }
                eprintln!("SEA {path} sea={} cruise_kn={cruise_kn:.1} from={} deg peak_heel={peak:.1} max_kn={top_kn:.1} over_s={:?} lost_s={:?} cells_lost={cells:.3}", sea.0, 45 * k, over_at.map(|t| t / tr::TICKS_PER_S), lost_at.map(|t| t / tr::TICKS_PER_S));
            }
        }
    });
}
