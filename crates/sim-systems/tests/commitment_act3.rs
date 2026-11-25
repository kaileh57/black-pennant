//! Unit `data-act3`: commitment gate 1 (DESIGN.md L96) on the shipped Act 3 data. The operation generator builds
//! 200 seeded instances of every Act 3 template for every first-release campaign player count, and every one
//! commits at least 5 hulls and 4x the 900 t cap per player, with no more capitals than the limit and no more enemy
//! aircraft than the minimum-swarm budget (L1447). The authored operations are generated from a campaign started
//! in Act 3. Submarines (L451) only ever add to a sortie: the surface hulls meet the floor without them.
use content::campaign::{DifficultySliders, MissionType, OperationKind};
use content::enums::Act;
use content::part::weapons::MunitionKind;
use sim_systems::theatre::opgen::{commitment_minimums, commitment_ok, generate, generate_standalone, Generated};
use sim_systems::theatre::{NewCampaign, Theatre, TheatreData};
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

#[test]
fn every_act3_template_meets_both_multiples_on_200_seeds() {
    let data = data();
    let diff = DifficultySliders::default();
    let mut failures = Vec::new();
    let mut boats = 0usize;
    for t in MissionType::ALL {
        for players in 1..=2u8 {
            for seed in 0..200u64 {
                let g = generate_standalone(t, Act::Act3, players, seed, &diff, None, &data).unwrap_or_else(|| panic!("{t:?} has a template"));
                let op = &data.campaign.operations[&g.briefing.operation];
                assert_eq!(op.act, Act::Act3, "{t:?}: Act 3 draws its own templates, not {}", op.id);
                if !commitment_ok(&g, Act::Act3, players, &data) {
                    failures.push(format!("{t:?} x{players} seed {seed}: {} hulls, {:.0} t", g.committed_hulls, g.committed_tonnage_t));
                }
                // The floor holds on surface hulls alone, so cutting submarines (L875) changes no composition.
                // The hunter-group template is filled by the generator's waves when no theatre state is given.
                let (h, tn) = surface_committed(&g, &data);
                let (min_h, min_t) = commitment_minimums(Act::Act3, players, &data.campaign.rules);
                if t != MissionType::HunterGroupSurvival && (h < min_h as usize || tn + 1e-6 < min_t) {
                    failures.push(format!("{t:?} x{players} seed {seed}: {h} surface hulls, {tn:.0} t"));
                }
                boats += g.config.vessels.iter().filter(|v| v.class.as_ref().and_then(|c| data.campaign.classes.get(c)).is_some_and(|d| d.submersible)).count();
                assert!(aircraft(&g, &data) <= 6, "{t:?} seed {seed}: {} aircraft", aircraft(&g, &data));
                assert!(capitals(&g, &data) <= data.campaign.rules.max_capitals as usize, "{t:?} seed {seed}");
            }
        }
    }
    assert!(failures.is_empty(), "{} instances short:\n{}", failures.len(), failures.iter().take(20).cloned().collect::<Vec<_>>().join("\n"));
    assert!(boats > 0, "Act 3 templates bring submarines");
}

/// The authored operations (story, ace duels, capstone) generated from a fresh Act 3 campaign: each commits both
/// multiples within the aircraft and capital budgets, and in a duel the ace sails in the group tagged "ace" aboard
/// their own ship.
#[test]
fn authored_act3_operations_generate_and_commit() {
    let data = data();
    let campaign = data.campaign.clone();
    let starter = content::campaign::load::stock_blueprint("starters/corvette60.bp.ron").expect("the 60 m starter");
    let new = NewCampaign { seed: 11, difficulty: DifficultySliders::default(), ironman: false, slot_name: "s".into(), players: vec![(1, "p".into(), starter)], start_act: Act::Act3 };
    let t = Theatre::new_campaign(new, data.clone());
    assert_eq!(t.state.act, Act::Act3);
    assert!(t.state.open_coves.iter().all(|c| c.as_str() != "cove.wren"), "LANTERN starts in Hollin's coves");
    // The chart table opens on Act 3's own work: the first story operation and a slate of Act 3 templates.
    let offered: Vec<&str> = t.state.offered.iter().map(|o| o.as_str()).collect();
    assert!(offered.contains(&"op.act3.story.saltby"), "{offered:?}");
    assert!(offered.iter().all(|o| o.starts_with("op.act3.")), "{offered:?}");
    let templates = t.state.offered.iter().filter(|o| matches!(campaign.operations[*o].kind, OperationKind::Template(_))).count();
    assert_eq!(templates, sim_systems::theatre::opgen::TEMPLATE_SLATE, "{offered:?}");
    let act = campaign.acts.iter().find(|a| a.act == Act::Act3).expect("an Act 3 row");
    for id in act.story_ops.iter().chain(&act.ace_duels).chain([&act.capstone]) {
        let g = generate(&t.state, &data, id, &[]).unwrap_or_else(|| panic!("{id} generates"));
        assert!(commitment_ok(&g, Act::Act3, 1, &data), "{id}: {} hulls, {:.0} t", g.committed_hulls, g.committed_tonnage_t);
        assert!(aircraft(&g, &data) <= 6, "{id}: {} aircraft", aircraft(&g, &data));
        assert!(capitals(&g, &data) <= data.campaign.rules.max_capitals as usize, "{id}");
        if let OperationKind::AceDuel(ace) = &campaign.operations[id].kind {
            let groups: Vec<_> = g.config.groups.iter().filter(|gr| gr.ace.as_ref() == Some(ace)).collect();
            assert_eq!(groups.len(), 1, "{id}: one group led by the ace");
            assert_eq!(groups[0].tag, "ace", "{id}");
            let ship = &campaign.aces[ace].ship;
            let members: Vec<_> = g.config.vessels.iter().filter(|v| v.group == Some(groups[0].id)).collect();
            assert!(members.iter().any(|v| v.class.as_ref() == Some(ship)), "{id}: the ace sails their own ship");
        }
    }
    // The capstone's raid flies in with the second wave.
    let g = generate(&t.state, &data, &act.capstone, &[]).unwrap();
    assert_eq!(aircraft(&g, &data), 4, "the Hollin raid");
}
