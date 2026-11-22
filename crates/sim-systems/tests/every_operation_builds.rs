//! Every operation in the campaign data, Acts 1 to 4, generates a sortie that builds a simulation and takes its first tick
//! (DESIGN.md L729-896). The later-act test samples only the operations a fresh campaign offers; this one walks
//! the whole operations table, so a broken fleet, class, ace or text reference in any operation fails here.
use content::campaign::DifficultySliders;
use content::enums::Act;
use content::ids::PlayerSlot;
use sim_systems::sim::{Sim, SimDeps};
use sim_systems::theatre::opgen::generate;
use sim_systems::theatre::{NewCampaign, Theatre, TheatreData};
use std::sync::Arc;

#[test]
fn act1_operations_build() { every_operation_builds(Act::Act1, 19) }
#[test]
fn act2_operations_build() { every_operation_builds(Act::Act2, 10) }
#[test]
fn act3_operations_build() { every_operation_builds(Act::Act3, 10) }
#[test]
fn act4_operations_build() { every_operation_builds(Act::Act4, 10) }

/// One test per act so the acts build in parallel.
fn every_operation_builds(act: Act, at_least: usize) {
    let catalogue = Arc::new(content::catalogue::Catalogue::load_embedded().expect("catalogue"));
    let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign data"));
    let data = TheatreData { campaign: campaign.clone(), catalogue: catalogue.clone() };
    let mut failures = Vec::new();
    let mut built = 0;
    for row in campaign.acts.iter().filter(|a| a.act == act) {
        let path = row.starters.first().unwrap_or_else(|| panic!("{:?} has a starter", row.act));
        let starter = content::campaign::load::stock_blueprint(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let new = NewCampaign { seed: 5, difficulty: DifficultySliders::default(), ironman: false, slot_name: "s".into(), players: vec![(1, "p".into(), starter)], start_act: row.act };
        let mut t = Theatre::new_campaign(new, data.clone());
        // Hunter-group survival is placed where the hunter group is; send the act's first task group hunting.
        t.state.hunter_group = t.state.groups.first().map(|g| g.id);
        for (id, _) in campaign.operations.iter().filter(|(_, o)| o.act == row.act) {
            let Some(g) = generate(&t.state, &data, id, &[PlayerSlot(0)]) else { failures.push(format!("{}: does not generate", id.as_str())); continue };
            let deps = SimDeps {
                catalogue: catalogue.clone(), campaign: campaign.clone(), designs: Default::default(),
                scripts: Box::new(sim_systems::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default(),
            };
            match Sim::new(g.config, deps) {
                Ok(mut sim) => { sim.step(); built += 1; }
                Err(e) => failures.push(format!("{}: sortie does not build: {e:?}", id.as_str())),
            }
        }
    }
    assert!(failures.is_empty(), "{built} built; failures:\n{}", failures.join("\n"));
    assert!(built >= at_least, "{act:?}: {built} operations built");
}
