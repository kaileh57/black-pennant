//! Track-picture measurement over one Act 1 sortie under the stock-officer gate's pilot (DESIGN.md L317), for
//! the L467 association gate: track events the player's log shows, swaps per track update, and duplicate runs.
//!
//! `cargo test --release -p sim-systems --lib track_event_probe -- --ignored --nocapture` with `BP_OPS`
//! (an operation id substring, default `feint`), `BP_SEED` (0) and `BP_MIN` (25 simulated minutes).
use crate::prelude::*;
use super::Track;
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

/// What a track's truth entity is now: `vessel`, a munition kind, a decoy kind, or `gone`.
fn truth_kind(world: &sim_core::World, e: EntityId) -> String {
    if world.vessel(e).is_some() { return "vessel".into(); }
    if let Some(r) = world.bodies().row_of(e) {
        return content::catalogue::catalogue().munitions.get_index(world.bodies().kind[r].index()).map(|(_, d)| format!("{:?}", d.kind)).unwrap_or_else(|| "body".into());
    }
    if let Some(i) = world.decoys().entity.iter().position(|d| *d == e) { return format!("{:?}", world.decoys().kind[i]); }
    "gone".into()
}

/// The object a ranged track is on, by position: the nearest object afloat or in flight (not the side's own) to
/// the track's predicted position, when it lies within the track's 95% radius or 500 m, whichever is more.
/// `None` when nothing is that near (the track's object is gone, or the track has lost it).
fn nearest_object(t: &Track, tick: u64, objects: &[(EntityId, DVec3)], own: &std::collections::BTreeSet<EntityId>, tt: &content::tuning::TrackTuning) -> Option<EntityId> {
    let p = t.predict(tick);
    let reach = t.radius95_m(tick, tt).max(500.0);
    let mut best: Option<(f64, EntityId)> = None;
    for (e, q) in objects {
        if own.contains(e) { continue; }
        let d2 = (p.x - q.x) * (p.x - q.x) + (p.z - q.z) * (p.z - q.z);
        if d2 <= reach * reach && best.is_none_or(|(b, _)| d2 < b) { best = Some((d2, *e)); }
    }
    best.map(|b| b.1)
}

/// Duplicate tracks in a reference scene (the dedicated-server soak's scene), with their causes: for every
/// track born of a detection whose object already held a live track, the sensor that started it and why the
/// detection did not join the existing track; for every pair of live tracks of one object, why the merge pass
/// left it. Also a census of tracks by what they are tracks of, the swap rate (L467), and the live bodies.
/// `cargo test --release -p sim-systems --lib duplicate_trace -- --ignored --nocapture` with `BP_TICKS` (7200),
/// `BP_SCENE` (`battle`, `coop`, `swarm`), `BP_SEED` (1), `BP_BODIES=1` (list long-lived missiles).
#[test]
#[ignore = "a measurement: run with --release --ignored --nocapture"]
fn duplicate_trace() {
    use crate::editor::trials::{reference_config, ReferenceScene};
    use super::trace;
    let ticks: u64 = std::env::var("BP_TICKS").ok().and_then(|s| s.parse().ok()).unwrap_or(7_200);
    let seed: u64 = std::env::var("BP_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    let scene = match std::env::var("BP_SCENE").as_deref() { Ok("coop") => ReferenceScene::CoopReference, Ok("swarm") => ReferenceScene::MinimumSwarm, _ => ReferenceScene::ReferenceBattle };
    let list_bodies = std::env::var("BP_BODIES").is_ok();
    let dump_s: Option<u64> = std::env::var("BP_DUMP_S").ok().and_then(|s| s.parse().ok());
    let catalogue = Arc::new(content::catalogue::Catalogue::load_embedded().expect("catalogue"));
    let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign data"));
    let deps = crate::sim::SimDeps { catalogue: catalogue.clone(), campaign: campaign.clone(), designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() };
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().expect("pool");
    pool.install(|| {
        let mut sim = crate::sim::Sim::new(reference_config(scene, seed, &campaign), deps).expect("scene starts");
        trace::set(true);
        drop(trace::take());
        let mut seen_track: BTreeMap<(u8, u32), (EntityId, u64)> = BTreeMap::new();
        let (mut updates, mut swaps) = (0u64, 0u64);
        let mut swap_kinds: BTreeMap<String, u64> = BTreeMap::new();
        // The same count by position: a ranged track belongs to the object its prediction is nearest (see
        // `nearest_object`); a change of that object between two of its updates is a swap. `label_wrong` counts
        // the updates at which `Track::truth` is not that object.
        let mut owner_by_pos: BTreeMap<(u8, u32), EntityId> = BTreeMap::new();
        let (mut pos_updates, mut pos_swaps, mut pos_truth_swaps, mut label_wrong, mut hull_updates, mut hull_pos_swaps, mut hull_label_wrong) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
        let mut pos_swap_kinds: BTreeMap<String, u64> = BTreeMap::new();
        // L467 duplicates by position: (side, object) -> start of the current run of two or more ranged tracks
        // nearest that object and updated in the last 30 s; the longest run.
        let mut since_pos: BTreeMap<(u8, EntityId), u64> = BTreeMap::new();
        let mut worst_pos: BTreeMap<(u8, EntityId), u64> = BTreeMap::new();
        // (side, truth) -> tick the current run of two or more held tracks began; longest run; and the same
        // counting every live track (not only those a sensor updated in the last 30 s).
        let mut since: BTreeMap<(u8, EntityId), u64> = BTreeMap::new();
        let mut worst: BTreeMap<(u8, EntityId), u64> = BTreeMap::new();
        let mut dup_samples: BTreeMap<String, u64> = BTreeMap::new();
        let (mut surf_sum, mut hull_sum, mut samples) = (0u64, 0u64, 0u64);
        let mut birth_kind: BTreeMap<String, u64> = BTreeMap::new();
        let mut pair_kind: BTreeMap<String, u64> = BTreeMap::new();
        let mut first_seen: BTreeMap<EntityId, u64> = BTreeMap::new();
        for i in 0..ticks {
            sim.set_host_load(crate::lod::HostLoad::IDLE);
            sim.step();
            drop(sim.drain_events());
            if i % 6 != 0 { continue; }
            let now = i + 1;
            let world = sim.world();
            // Causes recorded during this update, labelled while the objects still exist.
            let log = trace::take();
            for b in &log.births {
                let what = truth_kind(world, b.target);
                for (_, dom, bo, why) in &b.existing {
                    let why = match why { trace::NoJoin::Beyond(s) => format!("Beyond({})", if *s < 5 { "under 5" } else if *s < 10 { "5 to 10" } else { "10 or more" }), w => format!("{w:?}") };
                    *birth_kind.entry(format!("{what} by {:?}{} hint {:?} | existing {dom:?}{} | {why}", b.kind, if b.ranged { "" } else { " (bearing)" }, b.hint, if *bo { " wedge" } else { "" })).or_default() += 1;
                }
            }
            for ((_, truth, why), n) in &log.pairs {
                let why = match why { trace::NoMerge::Beyond(s) => format!("Beyond({})", if *s < 5 { "under 5" } else if *s < 10 { "5 to 10" } else { "10 or more" }), w => format!("{w:?}") };
                *pair_kind.entry(format!("{} | {why}", truth_kind(world, *truth))).or_default() += n;
            }
            // Every object afloat or in flight, for the nearest-object score.
            let objects: Vec<(EntityId, DVec3)> = world.vessels().filter(|(_, v)| matches!(v.status, sim_core::vessel::VesselStatus::Active | sim_core::vessel::VesselStatus::Disabled)).map(|(_, v)| (v.id, v.kin.pose.pos))
                .chain((0..world.bodies().len()).filter(|r| world.bodies().phase[*r] != sim_core::munitions::BodyPhase::Dead).map(|r| (world.bodies().entity[r], world.bodies().pos[r])))
                .chain(world.decoys().entity.iter().filter_map(|e| world.locate(*e).map(|l| (*e, l.0)))).collect();
            let tt = &content::catalogue::catalogue().tuning.tracks;
            let mut held_pos: BTreeMap<(u8, EntityId), usize> = BTreeMap::new();
            for (si, p) in sim.pictures().sides.iter().enumerate() {
                let own: std::collections::BTreeSet<EntityId> = p.tracks.iter().filter(|t| t.is_live() && t.stance == Stance::Friend).map(|t| t.truth).collect();
                for t in p.tracks.iter().filter(|t| t.is_live() && t.stance != Stance::Friend && t.bearing_only.is_none() && now.saturating_sub(t.last_update_tick) <= 30 * 60) {
                    if let Some(near) = nearest_object(t, now, &objects, &own, tt) { *held_pos.entry((si as u8, near)).or_default() += 1; }
                }
                for t in p.tracks.iter().filter(|t| t.is_live()) {
                    let k = (si as u8, t.id.0);
                    if let Some((truth, upd)) = seen_track.get(&k).copied() {
                        if t.last_update_tick != upd {
                            updates += 1;
                            let truth_swap = !truth.is_none() && !t.truth.is_none() && truth != t.truth;
                            if truth_swap {
                                swaps += 1;
                                *swap_kinds.entry(format!("{:?} {}->{}", t.domain, truth_kind(world, truth), truth_kind(world, t.truth))).or_default() += 1;
                            }
                            if t.bearing_only.is_none() && t.stance != Stance::Friend {
                                if let Some(near) = nearest_object(t, now, &objects, &own, tt) {
                                    let hull = world.vessel(near).is_some();
                                    pos_updates += 1;
                                    if hull { hull_updates += 1; }
                                    if truth_swap { pos_truth_swaps += 1; }
                                    if t.truth != near { label_wrong += 1; if hull { hull_label_wrong += 1; } }
                                    if let Some(was) = owner_by_pos.insert(k, near) {
                                        if was != near {
                                            pos_swaps += 1;
                                            if hull && world.vessel(was).is_some() { hull_pos_swaps += 1; }
                                            *pos_swap_kinds.entry(format!("{:?} {}->{}", t.domain, truth_kind(world, was), truth_kind(world, near))).or_default() += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    seen_track.insert(k, (t.truth, t.last_update_tick));
                }
            }
            since_pos.retain(|k, _| held_pos.get(k).is_some_and(|n| *n >= 2));
            for (k, _) in held_pos.iter().filter(|(_, n)| **n >= 2) {
                let s = *since_pos.entry(*k).or_insert(now);
                let w = worst_pos.entry(*k).or_default();
                *w = (*w).max(now - s);
            }
            let mut held: BTreeMap<(u8, EntityId), usize> = BTreeMap::new();
            for (si, p) in sim.pictures().sides.iter().enumerate() {
                for t in p.tracks.iter().filter(|t| t.is_live() && !t.truth.is_none() && t.stance != Stance::Friend && now.saturating_sub(t.last_update_tick) <= 30 * 60) { *held.entry((si as u8, t.truth)).or_default() += 1; }
            }
            since.retain(|k, _| held.get(k).is_some_and(|n| *n >= 2));
            for (k, n) in held.iter().filter(|(_, n)| **n >= 2) {
                let s = *since.entry(*k).or_insert(now);
                let w = worst.entry(*k).or_default();
                *w = (*w).max(now - s);
                *dup_samples.entry(truth_kind(world, k.1)).or_default() += (*n - 1) as u64;
            }
            // Side 0 (the players'): surface tracks that are not Friend against the hostile hulls afloat.
            if let Some(p) = sim.pictures().side(SideId(0)) {
                surf_sum += p.tracks.iter().filter(|t| t.is_live() && t.domain == Domain::Surface && t.stance != Stance::Friend).count() as u64;
                hull_sum += world.vessels().filter(|(_, v)| v.side != SideId(0) && matches!(v.status, sim_core::vessel::VesselStatus::Active | sim_core::vessel::VesselStatus::Disabled)).count() as u64;
                samples += 1;
                if now % 600 == 1 {
                    let surf = p.tracks.iter().filter(|t| t.is_live() && t.domain == Domain::Surface && t.stance != Stance::Friend).count();
                    let hulls = world.vessels().filter(|(_, v)| v.side != SideId(0) && matches!(v.status, sim_core::vessel::VesselStatus::Active | sim_core::vessel::VesselStatus::Disabled)).count();
                    // Air and Weapon tracks of side 0 whose label is a hull afloat (b1's open item).
                    let weapon_on_hull = p.tracks.iter().filter(|t| t.is_live() && t.stance != Stance::Friend && matches!(t.domain, Domain::Air | Domain::Weapon) && world.vessel(t.truth).is_some()).count();
                    // One-return tracks (`Track::unproven`) coasting past the 20 s an air track gets: what a
                    // shorter coast for them would remove.
                    let stale_single = p.tracks.iter().filter(|t| t.is_live() && t.stance != Stance::Friend && t.unproven() && now.saturating_sub(t.last_update_tick) > 20 * 60).count();
                    eprintln!("SPH t={}s side 0 surface tracks {surf} hostile hulls {hulls} per hull {:.2} air-like tracks labelled a hull {weapon_on_hull} one-return tracks older than 20 s {stale_single}", now / 60, surf as f64 / hulls.max(1) as f64);
                }
            }
            let b = world.bodies();
            for r in 0..b.len() { first_seen.entry(b.entity[r]).or_insert(now); }
            // `BP_DUMP_S`: at that second, every live track of side 0 that is not a Friend: what it is a track of,
            // its age, the time since any update and since its last ranged fix, its error from the object.
            if dump_s == Some(i / 60) && i % 60 == 0 {
                let tt = &content::catalogue::catalogue().tuning.tracks;
                if let Some(p) = sim.pictures().side(SideId(0)) {
                    for t in p.tracks.iter().filter(|t| t.is_live() && t.stance != Stance::Friend) {
                        let at = world.locate(t.truth).map(|l| l.0);
                        let err = at.map(|a| { let q = t.predict(now); ((q.x - a.x) * (q.x - a.x) + (q.z - a.z) * (q.z - a.z)).sqrt() });
                        eprintln!("DUMP t{} {:?}{} of {} {} born {}s upd {}s ago fix {:?}s ago {:?} {:?} r95 {:.0} err {:?} speed {:.0} class {:?} rung {:?} {:?}", t.id.0, t.domain, if t.bearing_only.is_some() { " wedge" } else { "" }, truth_kind(world, t.truth), t.truth.index, t.created_tick / 60, (now - t.last_update_tick) / 60, t.fit.as_ref().map(|f| (now.saturating_sub(f.tick)) / 60), t.grade, t.sources, t.radius95_m(now, tt), err.map(|e| e as i64), t.vel.length(), t.class.as_ref().map(|c| c.as_str().to_string()), t.rung, t.stance);
                    }
                }
            }
            if now % 1800 == 1 || i + 6 >= ticks {
                for (si, p) in sim.pictures().sides.iter().enumerate() {
                    let mut by: BTreeMap<String, u32> = BTreeMap::new();
                    for t in &p.tracks { *by.entry(format!("{:?} {} of {}", t.domain, if t.is_live() { if t.stance == Stance::Friend { "friend" } else if t.bearing_only.is_some() { "wedge" } else { "live" } } else { "last-known" }, truth_kind(world, t.truth))).or_default() += 1; }
                    eprintln!("CENSUS t={}s side {si} tracks {}: {by:?}", now / 60, p.tracks.len());
                }
                let mut kinds: BTreeMap<String, u32> = BTreeMap::new();
                for r in 0..b.len() {
                    if b.phase[r] == sim_core::munitions::BodyPhase::Dead { continue; }
                    *kinds.entry(truth_kind(world, b.entity[r])).or_default() += 1;
                }
                eprintln!("BODIES t={}s {kinds:?}", now / 60);
                if list_bodies {
                    for r in 0..b.len() {
                        let e = b.entity[r];
                        let age = (now - first_seen.get(&e).copied().unwrap_or(now)) / 60;
                        if age < 60 || truth_kind(world, e) != "AntiShipMissile" { continue; }
                        let g = sim.guidance.guided.iter().find(|g| g.body == e);
                        let to = g.map(|g| (g.predicted - b.pos[r]).length());
                        eprintln!("  OLD body {} side {} age {age}s phase {:?} steer {:?} speed {:.0} pos ({:.0}, {:.1}, {:.0}) remaining {:.0} seeker {:?} lock {:?} to_predicted {:?} track {:?}", e.index, b.side[r].0, b.phase[r], core::mem::discriminant(&b.steer[r]), b.vel[r].length(), b.pos[r].x, b.pos[r].y, b.pos[r].z, b.remaining[r], g.map(|g| g.phase), g.and_then(|g| g.lock).map(|l| truth_kind(world, l)), to.map(|d| d as i64), g.map(|g| g.track.0));
                    }
                }
            }
        }
        trace::set(false);
        let mut w: Vec<_> = worst.iter().map(|(k, v)| (*v, k.0, k.1.index)).collect();
        w.sort_unstable_by(|a, b| b.cmp(a));
        let over = w.iter().filter(|x| x.0 > 600).count();
        let top = |m: &BTreeMap<String, u64>, n: usize| { let mut v: Vec<(u64, &String)> = m.iter().map(|(k, v)| (*v, k)).collect(); v.sort_unstable_by(|a, b| b.cmp(a)); v.into_iter().take(n).map(|(n, k)| format!("\n    {n:>7}  {k}")).collect::<String>() };
        eprintln!("TRACE scene {scene:?} seed {seed} ticks {ticks}");
        eprintln!("  surface tracks per hostile hull afloat, side 0, mean over {samples} samples: {:.2} ({:.1} tracks, {:.1} hulls)", surf_sum as f64 / hull_sum.max(1) as f64, surf_sum as f64 / samples.max(1) as f64, hull_sum as f64 / samples.max(1) as f64);
        eprintln!("  truths with a duplicate at any sample {}, over 10 s {over}, longest (ticks, side, truth) {:?}", w.len(), &w[..w.len().min(8)]);
        eprintln!("  extra held tracks summed over samples, by what the object is: {dup_samples:?}");
        eprintln!("  swaps {swaps} of {updates} sampled updates ({:.3}%), by kind {swap_kinds:?}", 100.0 * swaps as f64 / updates.max(1) as f64);
        eprintln!("  by position (ranged tracks that are not Friend, nearest object inside the track's reach): swaps {pos_swaps} of {pos_updates} sampled updates ({:.3}%), the same updates by truth {pos_truth_swaps} ({:.3}%), label not the nearest object {label_wrong} ({:.3}%)", 100.0 * pos_swaps as f64 / pos_updates.max(1) as f64, 100.0 * pos_truth_swaps as f64 / pos_updates.max(1) as f64, 100.0 * label_wrong as f64 / pos_updates.max(1) as f64);
        let hull_ids: std::collections::BTreeSet<EntityId> = sim.world().vessels().map(|(_, v)| v.id).collect();
        let over_pos = worst_pos.values().filter(|v| **v > 600).count();
        let over_pos_hull = worst_pos.iter().filter(|(k, v)| **v > 600 && hull_ids.contains(&k.1)).count();
        eprintln!("  by position, objects with two or more ranged tracks nearest them: at any sample {}, over 10 s {over_pos}, of them hulls afloat at the end {over_pos_hull}", worst_pos.len());
        eprintln!("  by position, tracks nearest a hull: hull to hull swaps {hull_pos_swaps} of {hull_updates} updates ({:.3}%), label not that hull {hull_label_wrong} ({:.3}%); swaps by kind {pos_swap_kinds:?}", 100.0 * hull_pos_swaps as f64 / hull_updates.max(1) as f64, 100.0 * hull_label_wrong as f64 / hull_updates.max(1) as f64);
        eprintln!("  births beside an existing track of the same object (object by sensor | existing track | why it did not join):{}", top(&birth_kind, 40));
        eprintln!("  unmerged pairs of one object, per update (object | why):{}", top(&pair_kind, 30));
    });
}
