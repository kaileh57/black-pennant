//! Units `data-enemy-act1` and `data-operations-act1`: commitment gate 1 (DESIGN.md L96) on the shipped Act 1
//! data. The operation generator builds 200 seeded instances of every template for every first-release
//! campaign player count, and every one commits at least 5 hulls and 4x the act's tonnage cap per player,
//! with no capital over the limit.
use content::campaign::{DifficultySliders, MissionType};
use content::enums::Act;
use sim_systems::theatre::opgen::{commitment_ok, generate_standalone};
use sim_systems::theatre::TheatreData;
use std::sync::Arc;

#[test]
fn every_act1_template_meets_both_multiples_on_200_seeds() {
    let catalogue = Arc::new(content::catalogue::Catalogue::load_embedded().expect("catalogue"));
    let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign data"));
    let data = TheatreData { campaign, catalogue };
    let diff = DifficultySliders::default();
    let mut failures = Vec::new();
    for t in MissionType::ALL {
        for players in 1..=2u8 {
            for seed in 0..200u64 {
                let g = generate_standalone(t, Act::Act1, players, seed, &diff, None, &data).unwrap_or_else(|| panic!("{t:?} has an Act 1 template"));
                if !commitment_ok(&g, Act::Act1, players, &data) {
                    failures.push(format!("{t:?} x{players} seed {seed}: {} hulls, {:.0} t", g.committed_hulls, g.committed_tonnage_t));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{} of {} instances short:\n{}", failures.len(), 11 * 2 * 200, failures.iter().take(20).cloned().collect::<Vec<_>>().join("\n"));
}

/// The authored operations (story, ace duels, capstone) generated from a fresh Act 1 campaign on the shipped
/// data: each commits both multiples, and in a duel the ace sails in the group tagged "ace" aboard its own ship.
#[test]
fn authored_act1_operations_generate_and_commit() {
    use content::campaign::OperationKind;
    use sim_systems::theatre::opgen::generate;
    use sim_systems::theatre::{NewCampaign, Theatre};
    let catalogue = Arc::new(content::catalogue::Catalogue::load_embedded().expect("catalogue"));
    let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign data"));
    let data = TheatreData { campaign: campaign.clone(), catalogue };
    let starter = content::campaign::load::stock_blueprint("starters/fac22.bp.ron").expect("the 22 m starter");
    let new = NewCampaign { seed: 5, difficulty: DifficultySliders::default(), ironman: false, slot_name: "s".into(), players: vec![(1, "p".into(), starter)], start_act: Act::Act1 };
    let t = Theatre::new_campaign(new, data.clone());
    let act = &campaign.acts[0];
    for id in act.story_ops.iter().chain(&act.ace_duels).chain([&act.capstone]) {
        let g = generate(&t.state, &data, id, &[]).unwrap_or_else(|| panic!("{id} generates"));
        assert!(commitment_ok(&g, Act::Act1, 1, &data), "{id}: {} hulls, {:.0} t", g.committed_hulls, g.committed_tonnage_t);
        if let OperationKind::AceDuel(ace) = &campaign.operations[id].kind {
            let groups: Vec<_> = g.config.groups.iter().filter(|gr| gr.ace.as_ref() == Some(ace)).collect();
            assert_eq!(groups.len(), 1, "{id}: one group led by the ace");
            assert_eq!(groups[0].tag, "ace", "{id}");
            let ship = &campaign.aces[ace].ship;
            let members: Vec<_> = g.config.vessels.iter().filter(|v| v.group == Some(groups[0].id)).collect();
            assert!(members.iter().any(|v| v.class.as_ref() == Some(ship)), "{id}: the ace sails her own ship");
        }
    }
}
