//! Unit `data-act2`: commitment gate 1 (DESIGN.md L96) on the shipped Act 2 data. The operation generator builds
//! 200 seeded instances of every Act 2 template for every first-release campaign player count, and every one
//! commits at least 5 hulls and 4x the 400 t cap per player, with no capital over the limit and no more enemy
//! aircraft than the minimum-swarm budget (L1447). The authored operations are generated from a campaign started
//! in Act 2.
use content::campaign::{DifficultySliders, MissionType, OperationKind};
use content::enums::Act;
use content::part::weapons::MunitionKind;
use sim_systems::theatre::opgen::{commitment_ok, generate, generate_standalone, Generated};
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

#[test]
fn every_act2_template_meets_both_multiples_on_200_seeds() {
    let data = data();
    let diff = DifficultySliders::default();
    let mut failures = Vec::new();
    for t in MissionType::ALL {
        for players in 1..=2u8 {
            for seed in 0..200u64 {
                let g = generate_standalone(t, Act::Act2, players, seed, &diff, None, &data).unwrap_or_else(|| panic!("{t:?} has a template"));
                let op = &data.campaign.operations[&g.briefing.operation];
                assert_eq!(op.act, Act::Act2, "{t:?}: Act 2 draws its own templates, not {}", op.id);
                if !commitment_ok(&g, Act::Act2, players, &data) {
                    failures.push(format!("{t:?} x{players} seed {seed}: {} hulls, {:.0} t", g.committed_hulls, g.committed_tonnage_t));
                }
                assert!(aircraft(&g, &data) <= 6, "{t:?} seed {seed}: {} aircraft", aircraft(&g, &data));
            }
        }
    }
    assert!(failures.is_empty(), "{} of {} instances short:\n{}", failures.len(), 11 * 2 * 200, failures.iter().take(20).cloned().collect::<Vec<_>>().join("\n"));
}

/// The authored operations (story, ace duels, capstone) generated from a fresh Act 2 campaign: each commits both
/// multiples within the aircraft budget, and in a duel the ace sails in the group tagged "ace" aboard her own ship.
#[test]
fn authored_act2_operations_generate_and_commit() {
    let data = data();
    let campaign = data.campaign.clone();
    let starter = content::campaign::load::stock_blueprint("starters/corvette40.bp.ron").expect("the 40 m starter");
    let new = NewCampaign { seed: 7, difficulty: DifficultySliders::default(), ironman: false, slot_name: "s".into(), players: vec![(1, "p".into(), starter)], start_act: Act::Act2 };
    let t = Theatre::new_campaign(new, data.clone());
    assert_eq!(t.state.act, Act::Act2);
    // The chart table opens on Act 2's own work: the first story operation and a slate of Act 2 templates.
    let offered: Vec<&str> = t.state.offered.iter().map(|o| o.as_str()).collect();
    assert!(offered.contains(&"op.act2.story.ness"), "{offered:?}");
    assert!(offered.iter().all(|o| o.starts_with("op.act2.")), "{offered:?}");
    let templates = t.state.offered.iter().filter(|o| matches!(campaign.operations[*o].kind, OperationKind::Template(_))).count();
    assert_eq!(templates, sim_systems::theatre::opgen::TEMPLATE_SLATE, "{offered:?}");
    let act = campaign.acts.iter().find(|a| a.act == Act::Act2).expect("an Act 2 row");
    for id in act.story_ops.iter().chain(&act.ace_duels).chain([&act.capstone]) {
        let g = generate(&t.state, &data, id, &[]).unwrap_or_else(|| panic!("{id} generates"));
        assert!(commitment_ok(&g, Act::Act2, 1, &data), "{id}: {} hulls, {:.0} t", g.committed_hulls, g.committed_tonnage_t);
        assert!(aircraft(&g, &data) <= 6, "{id}: {} aircraft", aircraft(&g, &data));
        if let OperationKind::AceDuel(ace) = &campaign.operations[id].kind {
            let groups: Vec<_> = g.config.groups.iter().filter(|gr| gr.ace.as_ref() == Some(ace)).collect();
            assert_eq!(groups.len(), 1, "{id}: one group led by the ace");
            assert_eq!(groups[0].tag, "ace", "{id}");
            let ship = &campaign.aces[ace].ship;
            let members: Vec<_> = g.config.vessels.iter().filter(|v| v.group == Some(groups[0].id)).collect();
            assert!(members.iter().any(|v| v.class.as_ref() == Some(ship)), "{id}: the ace sails her own ship");
        }
    }
    // The capstone's raid flies in with the second wave.
    let g = generate(&t.state, &data, &act.capstone, &[]).unwrap();
    assert_eq!(aircraft(&g, &data), 4, "the Sallow raid");
}
