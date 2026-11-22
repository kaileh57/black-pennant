use super::*;
use crate::session::testkit;
use content::campaign::Island;

fn conditions() -> Conditions { Conditions { weather: WeatherKind::Clear, sea_state: SeaState(3), time_of_day: TimeOfDay::Dusk, seed: 42 } }
fn player() -> LocalPlayer { LocalPlayer { client_id: 7, name: "Captain".into() } }

#[test]
fn a_starter_fields_in_an_act_one_skirmish_against_a_group() {
    let bp = canonical(testkit::starter());
    assert!(design_problems(&bp, &testkit::catalogue(), &skirmish_ruleset(Act::Act1), false).is_empty(), "the 22 m starter is legal in Act 1");
    let setup = SkirmishSetup { template: MissionType::ConvoyRaid, act: Act::Act1, enemy_per_player: 2, difficulty: Default::default(), conditions: conditions(), blueprint: "starters/fac22.bp.ron".into() };
    let config = skirmish_config(&setup, &bp, &player(), &testkit::campaign(), &testkit::catalogue()).expect("the skirmish builds");
    assert_eq!(config.mode, GameMode::Skirmish);
    assert_eq!(config.seed, 42);
    let me = config.players.iter().find(|p| p.slot == PlayerSlot(0)).expect("seated");
    assert!(me.human && me.vessel.is_some());
    assert_eq!(me.name, "Captain");
    let own_side = me.side;
    assert!(config.vessels.iter().any(|v| v.side != own_side), "there is an enemy to fight");
    assert!(config.blueprints.iter().any(|b| b.header.hash == bp.header.hash));
    assert!(Sim::new(config, testkit::deps()).is_ok(), "the Sim accepts it");
}

#[test]
fn the_acts_cap_refuses_a_hull_that_is_too_heavy() {
    let big = canonical(content::campaign::load::stock_blueprint("starters/destroyer90.bp.ron").expect("destroyer"));
    let problems = design_problems(&big, &testkit::catalogue(), &skirmish_ruleset(Act::Act1), false);
    assert!(problems.iter().any(|m| m.key.as_str() == "validate.over_tonnage"), "{problems:?}");
    assert!(design_problems(&big, &testkit::catalogue(), &sandbox_ruleset(), false).is_empty(), "the sandbox takes it");
}

use sim_systems::Sim;

#[test]
fn sandbox_targets_sail_in_groups_ahead_of_the_player() {
    let bp = canonical(testkit::starter());
    let setup = SandboxSetup { conditions: conditions(), blueprint: "starters/fac22.bp.ron".into(), targets: Vec::new() };
    let class = testkit::campaign().classes.iter().next().map(|(id, d)| (id.clone(), d.blueprint.clone())).expect("a class");
    let rows = vec![
        (TargetDesign::Stock { path: stock_key(&class.1), class: Some(class.0.clone()) }, true, 3u8),
        (TargetDesign::Inline(Box::new(bp.clone())), false, 2u8),
    ];
    let config = sandbox_config(&setup, &bp, &rows, &player(), &testkit::campaign()).expect("the sandbox builds");
    let me = config.players[0].vessel.map(|i| config.vessels[i as usize].clone()).expect("seated");
    let hostile: Vec<_> = config.vessels.iter().filter(|v| v.side == HOSTILE_SIDE).collect();
    let passive: Vec<_> = config.vessels.iter().filter(|v| v.side == PASSIVE_SIDE).collect();
    assert_eq!((hostile.len(), passive.len()), (3, 2));
    let range = |v: &VesselInit| DVec2::new(v.pos.x - me.pos.x, v.pos.z - me.pos.z).length();
    for v in &hostile { assert!((range(v) - HOSTILE_RANGE_M).abs() < 1_500.0, "hostile at {}", range(v)); assert_eq!(v.declared, Some(Stance::Hostile)); }
    for v in &passive { assert!((range(v) - PASSIVE_RANGE_M).abs() < 1_500.0, "passive at {}", range(v)); assert_eq!(v.declared, Some(Stance::Suspect)); }
    let hostile_side = config.sides.iter().find(|s| s.id == HOSTILE_SIDE).expect("hostile side");
    assert!(hostile_side.hostile_to.has(me.side), "hostile targets fight back");
    assert!(config.sides.iter().find(|s| s.id == PASSIVE_SIDE).expect("passive side").hostile_to.is_empty(), "passive targets never fire");
    assert!(config.sides.iter().find(|s| s.id == me.side).expect("own side").hostile_to.has(HOSTILE_SIDE));
    assert_eq!(config.groups.iter().filter(|g| g.side == HOSTILE_SIDE && g.orders == GroupOrders::Hunt).count(), 1);
    assert_eq!(config.groups.iter().filter(|g| g.side == PASSIVE_SIDE && g.orders == GroupOrders::Anchor).count(), 1);
    assert!(config.intel.declared_hostile.contains(&class.0));
    // Ships of a row stand line abreast 600 m apart.
    let d = DVec2::new(hostile[0].pos.x - hostile[1].pos.x, hostile[0].pos.z - hostile[1].pos.z).length();
    assert!((d - LINE_SPACING_M).abs() < 1.0, "{d}");
    assert!(Sim::new(config, testkit::deps()).is_ok(), "the Sim accepts it");
}

#[test]
fn targets_are_capped_and_kept_off_land_and_inside_the_map() {
    let bp = canonical(testkit::starter());
    let setup = SandboxSetup { conditions: conditions(), blueprint: String::new(), targets: Vec::new() };
    let rows: Vec<(TargetDesign, bool, u8)> = (0..8).map(|_| (TargetDesign::Inline(Box::new(bp.clone())), true, 8u8)).collect();
    let config = sandbox_config(&setup, &bp, &rows, &player(), &testkit::campaign()).expect("builds");
    assert_eq!(config.vessels.iter().filter(|v| v.side == HOSTILE_SIDE).count(), MAX_TARGETS);
    let map = MapDef { size_m: [10_000.0, 10_000.0], islands: vec![Island { centre_m: [1_000.0, 0.0], radius_m: 500.0, height_m: 20.0, seed: 0 }], ..Default::default() };
    let p = clear_of_land(DVec2::new(1_100.0, 0.0), &map);
    assert!((p - DVec2::new(1_000.0, 0.0)).length() >= 900.0 - 1e-9, "pushed clear of the island: {p}");
    let q = clear_of_land(DVec2::new(9_000.0, -9_000.0), &map);
    assert!(q.x <= 4_000.0 && q.y >= -4_000.0, "inside the edge: {q}");
    assert_eq!(stock_key("./blueprints/starters/fac22.bp.ron"), "starters/fac22.bp.ron");
}


/// Every skirmish template builds at Act 1, the only act this build ships (L2283), with Act 1 classes alone.
#[test]
fn every_template_builds_an_act_one_skirmish_with_act_one_classes() {
    let bp = canonical(testkit::starter());
    let campaign = std::sync::Arc::new(content::campaign::load::shipped_campaign().clone());
    for template in MissionType::ALL {
        let setup = SkirmishSetup { template, act: Act::Act1, enemy_per_player: 3, difficulty: Default::default(), conditions: conditions(), blueprint: "starters/fac22.bp.ron".into() };
        let config = skirmish_config(&setup, &bp, &player(), &campaign, &testkit::catalogue()).unwrap_or_else(|m| panic!("{template:?}: {m:?}"));
        let own = config.players.iter().find(|p| p.slot == PlayerSlot(0)).expect("seated").side;
        assert!(config.vessels.iter().any(|v| v.side != own), "{template:?}: there is an enemy");
        for v in &config.vessels {
            let Some(class) = v.class.as_ref() else { continue };
            let def = campaign.classes.get(class).unwrap_or_else(|| panic!("{template:?}: {class}"));
            assert_eq!(def.act, Act::Act1, "{template:?}: {class} is an {:?} class", def.act);
        }
    }
}
