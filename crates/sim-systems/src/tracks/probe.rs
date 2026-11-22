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
    let prepared = prepare_config(&config, &setup);
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
