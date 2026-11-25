//! Acts 2 to 4 load and play (DESIGN.md L862-875, L739-751): a campaign started in each later act offers
//! operations on its chart, every offer has a briefing, and every offer's sortie builds a simulation that steps.
//! The campaign playtest found nothing reaches a later act from the client yet except winning or losing Act 1,
//! so this is where a broken later-act operation shows first.
use content::campaign::DifficultySliders;
use content::enums::Act;
use content::ids::PlayerSlot;
use sim_systems::sim::{Sim, SimDeps};
use sim_systems::theatre::opgen::generate;
use sim_systems::theatre::{NewCampaign, Theatre, TheatreData};
use std::sync::Arc;

fn data() -> TheatreData {
    let catalogue = Arc::new(content::catalogue::Catalogue::load_embedded().expect("catalogue"));
    let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign data"));
    TheatreData { campaign, catalogue }
}

/// Simulated seconds each generated sortie is stepped.
const STEP_S: u64 = 1;

#[test]
fn later_acts_offer_operations_that_build_sorties() {
    let data = data();
    for act in [Act::Act2, Act::Act3, Act::Act4] {
        let row = data.campaign.acts.iter().find(|a| a.act == act).unwrap_or_else(|| panic!("{act:?} has a row"));
        let path = row.starters.first().unwrap_or_else(|| panic!("{act:?} has a starter"));
        let starter = content::campaign::load::stock_blueprint(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let new = NewCampaign { seed: 21, difficulty: DifficultySliders::default(), ironman: false, slot_name: "s".into(), players: vec![(1, "p".into(), starter)], start_act: act };
        let t = Theatre::new_campaign(new, data.clone());
        assert_eq!(t.state.act, act);
        assert!(!t.state.offered.is_empty(), "{act:?}: no operation on the chart");
        assert!(t.chart().regions.iter().any(|r| r.open), "{act:?}: no region open");
        for op in &t.state.offered {
            assert!(t.briefing(op).is_some(), "{act:?} {}: no briefing", op.as_str());
            let g = generate(&t.state, &data, op, &[PlayerSlot(0)]).unwrap_or_else(|| panic!("{act:?} {}: does not generate", op.as_str()));
            let deps = SimDeps {
                catalogue: data.catalogue.clone(), campaign: data.campaign.clone(), designs: Default::default(),
                scripts: Box::new(sim_systems::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default(),
            };
            let mut sim = Sim::new(g.config, deps).unwrap_or_else(|e| panic!("{act:?} {}: the sortie does not build: {e:?}", op.as_str()));
            for _ in 0..STEP_S * sim_core::time::TICK_HZ as u64 { sim.step(); }
        }
    }
}
