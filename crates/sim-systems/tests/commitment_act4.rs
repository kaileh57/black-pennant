//! Unit `data-act4`: commitment gate 1 (DESIGN.md L96) on the shipped Act 4 data, and the Open Theatre post-game
//! that follows its final capstone (L873). The operation generator builds 200 seeded instances of every Act 4
//! template for every first-release campaign player count, and every one commits at least 5 hulls and 4x the
//! 1,800 t cap per player, with no more capitals than the limit and no more enemy aircraft than the minimum-swarm
//! budget (L1447). The authored operations are generated from a campaign started in Act 4. Winning the capstone
//! opens Open Theatre, where the same templates play in every region and still meet the Act 4 floor.
use content::campaign::{DifficultySliders, MissionType, OperationKind};
use content::enums::Act;
use content::ids::{OperationId, RegionId};
use content::part::weapons::MunitionKind;
use sim_systems::protocol::config::{Outcome, SortieResult, SortieStats};
use sim_systems::theatre::opgen::{commitment_minimums, commitment_ok, generate, generate_standalone, refresh_offers, Generated};
use sim_systems::theatre::{progress, NewCampaign, Theatre, TheatreData, TheatreEvent};
use std::collections::BTreeSet;
use std::sync::Arc;

fn data() -> TheatreData {
    let catalogue = Arc::new(content::catalogue::Catalogue::load_embedded().expect("catalogue"));
    let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign data"));
    TheatreData { campaign, catalogue }
}

/// Enemy aircraft in a generated sortie.
fn aircraft(g: &Generated, data: &TheatreData) -> usize {
    g.config.bodies.iter().filter(|b| data.catalogue.munition(&b.munition).is_some_and(|m| m.kind == MunitionKind::Aircraft)).count()
}

/// Hulls over 90 m in a generated sortie (L126).
fn capitals(g: &Generated, data: &TheatreData) -> usize {
    g.config.vessels.iter().filter(|v| v.class.as_ref().and_then(|c| data.campaign.classes.get(c)).is_some_and(|d| d.capital)).count()
}

/// Full-load tonnage of a stock boat, computed once per blueprint.
fn boat_tonnage(path: &str, data: &TheatreData) -> f64 {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String, f64>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(t) = cache.lock().unwrap().get(path) { return *t; }
    let bp = content::campaign::load::stock_blueprint(path).expect("a stock boat");
    let t = content::stats::compute_stats(&bp, &data.catalogue).tonnage_t;
    cache.lock().unwrap().insert(path.to_string(), t);
    t
}

/// (hulls, tonnes) of the Combine's surface combat hulls alone: the generator's committed figures less its boats.
fn surface_committed(g: &Generated, data: &TheatreData) -> (usize, f64) {
    let mut subs = 0usize;
    let mut sub_t = 0.0;
    for v in &g.config.vessels {
        let Some(d) = v.class.as_ref().and_then(|c| data.campaign.classes.get(c)) else { continue };
        if !d.submersible { continue; }
        subs += 1;
        sub_t += boat_tonnage(&d.blueprint, data);
    }
    ((g.committed_hulls as usize).saturating_sub(subs), g.committed_tonnage_t - sub_t)
}

fn act4_campaign(data: &TheatreData, seed: u64) -> Theatre {
    let starter = content::campaign::load::stock_blueprint("starters/destroyer90.bp.ron").expect("the 90 m starter");
    let new = NewCampaign { seed, difficulty: DifficultySliders::default(), ironman: false, slot_name: "s".into(), players: vec![(1, "p".into(), starter)], start_act: Act::Act4 };
    Theatre::new_campaign(new, data.clone())
}

fn victory(op: &OperationId, region: &RegionId) -> SortieResult {
    SortieResult {
        operation: Some(op.clone()), region: Some(region.clone()), seed: 1, outcome: Outcome::Victory, winner: None, duration_ticks: 0, objective_done: true,
        ghosts: vec![], kills: vec![], survivors: vec![], prizes: vec![], aces: vec![], players: vec![], sightings: 0, evidence: 0, photos: 0,
        survivors_recovered: 0, crew_captured: 0, passengers_delivered: 0, mines_laid: 0, site_destroyed: true, tender_lost: false, highest_rung: 0,
        belief: vec![], stats: SortieStats::default(),
    }
}

#[test]
fn every_act4_template_meets_both_multiples_on_200_seeds() {
    let data = data();
    let diff = DifficultySliders::default();
    let mut failures = Vec::new();
    let mut boats = 0usize;
    for t in MissionType::ALL {
        for players in 1..=2u8 {
            for seed in 0..200u64 {
                let g = generate_standalone(t, Act::Act4, players, seed, &diff, None, &data).unwrap_or_else(|| panic!("{t:?} has a template"));
                let op = &data.campaign.operations[&g.briefing.operation];
                assert_eq!(op.act, Act::Act4, "{t:?}: Act 4 draws its own templates, not {}", op.id);
                if !commitment_ok(&g, Act::Act4, players, &data) {
                    failures.push(format!("{t:?} x{players} seed {seed}: {} hulls, {:.0} t", g.committed_hulls, g.committed_tonnage_t));
                }
                // The floor holds on surface hulls alone, so cutting submarines (L875) changes no composition.
                // The hunter-group template is filled by the generator's waves when no theatre state is given.
                let (h, tn) = surface_committed(&g, &data);
                let (min_h, min_t) = commitment_minimums(Act::Act4, players, &data.campaign.rules);
                if t != MissionType::HunterGroupSurvival && (h < min_h as usize || tn + 1e-6 < min_t) {
                    failures.push(format!("{t:?} x{players} seed {seed}: {h} surface hulls, {tn:.0} t"));
                }
                boats += g.config.vessels.iter().filter(|v| v.class.as_ref().and_then(|c| data.campaign.classes.get(c)).is_some_and(|d| d.submersible)).count();
                assert!(aircraft(&g, &data) <= 6, "{t:?} seed {seed}: {} aircraft", aircraft(&g, &data));
                assert_eq!(capitals(&g, &data), 0, "{t:?} seed {seed}: capitals sail only in authored operations");
            }
        }
    }
    assert!(failures.is_empty(), "{} instances short:\n{}", failures.len(), failures.iter().take(20).cloned().collect::<Vec<_>>().join("\n"));
    assert!(boats > 0, "Act 4 templates bring submarines");
}

/// The authored operations (story, ace duel, capstone) generated from a fresh Act 4 campaign: each commits both
/// multiples within the aircraft and capital budgets, and in the duel the Chairman sails in the group tagged "ace"
/// aboard the Charter.
#[test]
fn authored_act4_operations_generate_and_commit() {
    let data = data();
    let campaign = data.campaign.clone();
    let t = act4_campaign(&data, 11);
    assert_eq!(t.state.act, Act::Act4);
    assert!(t.state.open_coves.iter().all(|c| ["cove.ember", "cove.last", "cove.fathom"].contains(&c.as_str())), "LANTERN starts in the Meridian coves");
    assert!(!t.state.open_theatre && t.state.clock_hours_left.is_some(), "Act 4 runs on its operation clock");
    // The chart table opens on Act 4's own work: the first story operation and a slate of Act 4 templates.
    let offered: Vec<&str> = t.state.offered.iter().map(|o| o.as_str()).collect();
    assert!(offered.contains(&"op.act4.story.orrin"), "{offered:?}");
    assert!(offered.iter().all(|o| o.starts_with("op.act4.")), "{offered:?}");
    let templates = t.state.offered.iter().filter(|o| matches!(campaign.operations[*o].kind, OperationKind::Template(_))).count();
    assert_eq!(templates, sim_systems::theatre::opgen::TEMPLATE_SLATE, "{offered:?}");
    let act = campaign.acts.iter().find(|a| a.act == Act::Act4).expect("an Act 4 row");
    for id in act.story_ops.iter().chain(&act.ace_duels).chain([&act.capstone]) {
        let g = generate(&t.state, &data, id, &[]).unwrap_or_else(|| panic!("{id} generates"));
        assert!(commitment_ok(&g, Act::Act4, 1, &data), "{id}: {} hulls, {:.0} t", g.committed_hulls, g.committed_tonnage_t);
        assert!(aircraft(&g, &data) <= 6, "{id}: {} aircraft", aircraft(&g, &data));
        assert!(capitals(&g, &data) <= data.campaign.rules.max_capitals as usize, "{id}: {} capitals", capitals(&g, &data));
        if let OperationKind::AceDuel(ace) = &campaign.operations[id].kind {
            let groups: Vec<_> = g.config.groups.iter().filter(|gr| gr.ace.as_ref() == Some(ace)).collect();
            assert_eq!(groups.len(), 1, "{id}: one group led by the ace");
            assert_eq!(groups[0].tag, "ace", "{id}");
            let ship = &campaign.aces[ace].ship;
            let members: Vec<_> = g.config.vessels.iter().filter(|v| v.group == Some(groups[0].id)).collect();
            assert!(members.iter().any(|v| v.class.as_ref() == Some(ship)), "{id}: the ace sails their own ship");
        }
    }
    // The capstone's raid flies in with the second wave, and the cruiser squadron lies off the yards.
    let g = generate(&t.state, &data, &act.capstone, &[]).unwrap();
    assert_eq!(aircraft(&g, &data), 4, "the Meridian raid");
    assert!(capitals(&g, &data) >= 1, "the heavy cruiser defends the yards");
}

/// Winning the final capstone opens Open Theatre in the same save (L873): no clock, every cove, every region's
/// Combine records, and Act 4's templates offered and generated across all four regions at the 1,800 t floor.
#[test]
fn open_theatre_follows_the_final_capstone_and_plays_act4_templates_everywhere() {
    let data = data();
    let campaign = data.campaign.clone();
    let mut t = act4_campaign(&data, 23);
    let act = campaign.acts.iter().find(|a| a.act == Act::Act4).unwrap();
    for id in act.story_ops.iter().chain(&act.ace_duels) { t.state.ops.completed.push(id.clone()); }
    let capstone = campaign.operations[&act.capstone].clone();
    let mut events = Vec::new();
    progress::apply_result(&mut t.state, &capstone, &victory(&capstone.id, &RegionId::new("region.meridian")), &data, &mut events);
    assert!(events.iter().any(|e| matches!(e, TheatreEvent::OpenTheatreOpened)), "{events:?}");
    let s = &mut t.state;
    assert!(s.open_theatre && s.clock_hours_left.is_none(), "Open Theatre has no operation clock");
    let coves: BTreeSet<&str> = campaign.theatre.nodes.iter().filter(|n| n.kind == content::campaign::NodeKind::Cove).map(|n| n.id.as_str()).collect();
    assert_eq!(s.open_coves.iter().map(|c| c.as_str()).collect::<BTreeSet<_>>(), coves, "every cove on the chart");
    // The Combine fields records in every region again.
    for r in &campaign.theatre.regions {
        assert!(s.hulls.iter().any(|h| h.alive && h.region == r.id), "{}: no Combine records", r.id);
    }
    // The chart table offers templates only, and only the last act's.
    refresh_offers(s, &data);
    assert!(!s.offered.is_empty());
    for o in &s.offered {
        let def = &campaign.operations[o];
        assert!(matches!(def.kind, OperationKind::Template(_)) && def.act == Act::Act4, "{o} in Open Theatre");
    }
    // Every template but the hunter-group survival (offered only while a hunter group is at sea) places in every
    // region and meets the Act 4 floor wherever it lands.
    let mut regions_by_template: std::collections::BTreeMap<&str, BTreeSet<RegionId>> = Default::default();
    let templates: Vec<OperationId> = campaign.operations.values().filter(|o| o.act == Act::Act4 && matches!(o.kind, OperationKind::Template(t) if t != MissionType::HunterGroupSurvival)).map(|o| o.id.clone()).collect();
    for id in &templates {
        for count in 0..32u8 {
            s.ops.offered.insert(id.clone(), count);
            let g = generate(s, &data, id, &[]).unwrap_or_else(|| panic!("{id} generates in Open Theatre"));
            assert!(commitment_ok(&g, Act::Act4, 1, &data), "{id} at {}: {} hulls, {:.0} t", g.briefing.node, g.committed_hulls, g.committed_tonnage_t);
            assert!(aircraft(&g, &data) <= 6 && capitals(&g, &data) == 0, "{id} at {}", g.briefing.node);
            regions_by_template.entry(id.as_str()).or_default().insert(g.briefing.region.clone());
        }
    }
    let all: BTreeSet<RegionId> = campaign.theatre.regions.iter().map(|r| r.id.clone()).collect();
    let everywhere: BTreeSet<RegionId> = regions_by_template.values().flatten().cloned().collect();
    assert_eq!(everywhere, all, "Open Theatre sorties reach every region");
    for (id, rs) in &regions_by_template { assert!(rs.len() >= 3, "{id} placed only in {rs:?}"); }
}

/// The Act 4 row changes how Act 3 ends: winning the Act 3 capstone on the shipped data now starts Act 4 in the
/// Meridian coves with its own clock, where before it stopped the clock.
#[test]
fn winning_the_act3_capstone_starts_act4() {
    let data = data();
    let campaign = data.campaign.clone();
    let starter = content::campaign::load::stock_blueprint("starters/corvette60.bp.ron").expect("the 60 m starter");
    let new = NewCampaign { seed: 5, difficulty: DifficultySliders::default(), ironman: false, slot_name: "s".into(), players: vec![(1, "p".into(), starter)], start_act: Act::Act3 };
    let mut t = Theatre::new_campaign(new, data.clone());
    let act3 = campaign.acts.iter().find(|a| a.act == Act::Act3).unwrap();
    for id in act3.story_ops.iter().chain(&act3.ace_duels) { t.state.ops.completed.push(id.clone()); }
    let capstone = campaign.operations[&act3.capstone].clone();
    let mut events = Vec::new();
    progress::apply_result(&mut t.state, &capstone, &victory(&capstone.id, &RegionId::new("region.hollin")), &data, &mut events);
    assert_eq!(t.state.act, Act::Act4, "{events:?}");
    assert!(!t.state.open_theatre && t.state.clock_hours_left.is_some_and(|h| h > 0.0));
    let act4 = campaign.acts.iter().find(|a| a.act == Act::Act4).unwrap();
    assert_eq!(t.state.open_coves, act4.coves);
    refresh_offers(&mut t.state, &data);
    assert!(t.state.offered.iter().any(|o| o == &act4.story_ops[0]), "{:?}", t.state.offered);
}

/// The Chairman duel's reference setup compiles on the starter it is stored with: every rule finds its sink, and
/// each heavy-missile launcher carries its launch rule (L318: presets, sliders and the rules table only).
#[test]
fn the_act4_reference_setup_compiles_cleanly() {
    let data = data();
    let bp = content::campaign::load::stock_blueprint("reference/act4/duel_chairman.bp.ron").expect("the reference setup");
    let prefab = Arc::new(sim_core::vessel::build_prefab(&bp, &data.catalogue));
    let rs = content::ruleset::Ruleset::campaign(Act::Act4);
    let design = sim_systems::ship::ShipDesign::build(&bp, prefab, &data.catalogue, &rs);
    assert!(design.graph.issues.is_empty(), "{:?}", design.graph.issues);
    let launch_rules: Vec<_> = bp.rules.iter().filter(|r| matches!(r.sink, content::graph::SinkKind::MissileLaunch(_))).collect();
    assert!(launch_rules.len() >= 2, "{} launch rules", launch_rules.len());
    for r in launch_rules {
        let content::graph::SinkKind::MissileLaunch(slot) = r.sink else { unreachable!() };
        assert!(design.mounts.iter().any(|m| m.slot == slot), "rule names launcher {slot:?}, which the design has");
    }
}
