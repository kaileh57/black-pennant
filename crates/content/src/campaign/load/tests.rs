use super::*;
use crate::campaign::{FleetSlot, FleetKind, GhostCondition, GhostKind, MapDef, Marker, MarkerKind, RegionDef, ForceEntry, LaneDef};
use crate::enums::{Phase, Stance, Tier};
use crate::ids::{ClassId, FleetTemplateId, TextKey};
use crate::stats::testkit::catalogue;

fn op(id: &str, kind: OperationKind) -> OperationDef {
    OperationDef {
        id: OperationId::new(id), kind, act: Act::Act1, name: TextKey::new("n"), briefing: TextKey::new("b"), unlock: Unlock::Always, node: None, map: None,
        objective: Objective::DestroyGroup { tag: "convoy".into(), fraction: 0.5 },
        tags: vec![ConstraintTag::DraftLimit { max_m: 3.0 }, ConstraintTag::Range { min_km: 100.0 }],
        ghosts: vec![GhostCondition { kind: GhostKind::NoLosses, reward: Reward::Intel(1) }, GhostCondition { kind: GhostKind::NoKills, reward: Reward::Intel(1) }],
        force: vec![ForceEntry { fleet: FleetTemplateId::new("fleet.a"), marker: "spawn".into(), tag: "convoy".into(), stance: Stance::Hostile, arrives_s: 0, emcon: None }],
        per_extra_player: None, complications: vec![], approach_m: 10_000.0, sea_states: vec![], actions: vec![], declared_hostile: vec![], recommended_emcon: None, pushback_days: None, forced: false, reference_setup: None,
    }
}

fn data() -> CampaignData {
    let map = MapDef { size_m: [10_000.0, 10_000.0], depth_m: 50.0, markers: vec![Marker { kind: MarkerKind::PlayerSpawn, name: "spawn".into(), pos_m: [0.0, 0.0], heading_deg: 0.0 }], ..Default::default() };
    let node = |id: &str, kind: NodeKind| NodeDef { id: TheatreNodeId::new(id), name: TextKey::new("n"), region: RegionId::new("r"), pos_km: [0.0, 0.0], kind, map: map.clone() };
    let theatre = TheatreDef {
        regions: vec![RegionDef { id: RegionId::new("r"), name: TextKey::new("n"), act: Act::Act1, base_patrols: 4, layer_depth_m: 50.0, weather: vec![], order_of_battle: vec![(ClassId::new("class.a"), 10)], civilian: vec![] }],
        nodes: vec![node("cove", NodeKind::Cove), node("sea", NodeKind::Sea)],
        lanes: vec![LaneDef { a: TheatreNodeId::new("cove"), b: TheatreNodeId::new("sea"), traffic: 1.0 }],
        ..Default::default()
    };
    let class = EnemyClassDef { id: ClassId::new("class.a"), name: TextKey::new("n"), blueprint: "examples/minimal.bp.ron".into(), role: crate::campaign::ClassRole::PatrolBoat, tier: Tier::T1, act: Act::Act1, behaviour: "patrol".into(), engagement_range_m: None, variants: vec![], surrender: Default::default(), air_wing: vec![], submersible: false, capital: false };
    let fleet = FleetTemplate { id: FleetTemplateId::new("fleet.a"), kind: FleetKind::Patrol, act: Act::Act1, slots: vec![FleetSlot { classes: vec![ClassId::new("class.a")], min: 1, max: 2 }], formation: Default::default(), bodies: vec![] };
    let mut story = op("op.story", OperationKind::Story);
    story.node = Some(TheatreNodeId::new("sea"));
    let ops = vec![op("op.t", OperationKind::Template(crate::campaign::MissionType::ConvoyRaid)), story, op("op.cap", OperationKind::Capstone)];
    let act = ActDef { act: Act::Act1, phase: Phase::R010, name: TextKey::new("n"), starters: vec!["examples/minimal.bp.ron".into()], start_tier: Tier::T1, tier_unlocks: vec![(OperationId::new("op.t"), Tier::T2)], regions: vec![RegionId::new("r")], story_ops: vec![OperationId::new("op.story")], ace_duels: vec![], capstone: OperationId::new("op.cap"), aces: vec![], coves: vec![TheatreNodeId::new("cove")], retreat_closed_cove: None, opens_open_theatre: false };
    CampaignData {
        theatre,
        acts: vec![act],
        classes: [(class.id.clone(), class)].into_iter().collect(),
        fleets: [(fleet.id.clone(), fleet)].into_iter().collect(),
        operations: ops.into_iter().map(|o| (o.id.clone(), o)).collect(),
        ..Default::default()
    }
}

fn run(d: &CampaignData) -> ContentResult<()> { check(d, &catalogue(), &|p| stock_blueprint_with(p, crate::catalogue::catalogue())) }

#[test]
fn consistent_data_passes() { run(&data()).unwrap(); }

#[test]
fn broken_references_are_named() {
    let mut d = data();
    d.acts[0].capstone = OperationId::new("op.missing");
    assert!(matches!(run(&d), Err(ContentError::UnknownRef { kind: "operation", .. })));

    let mut d = data();
    d.acts[0].story_ops.push(OperationId::new("op.t"));
    assert!(matches!(run(&d), Err(ContentError::Parse { .. })));

    let mut d = data();
    d.classes[0].blueprint = "nowhere.bp.ron".into();
    assert!(matches!(run(&d), Err(ContentError::UnknownRef { kind: "stock blueprint", .. })));

    let mut d = data();
    d.fleets[0].slots[0].classes.push(ClassId::new("class.ghost"));
    assert!(matches!(run(&d), Err(ContentError::UnknownRef { kind: "enemy class", .. })));

    let mut d = data();
    d.operations[1].force[0].marker = "nowhere".into();
    assert!(matches!(run(&d), Err(ContentError::UnknownRef { kind: "map marker", .. })));

    let mut d = data();
    d.operations[0].objective = Objective::Capture { tag: "prize".into() };
    assert!(matches!(run(&d), Err(ContentError::UnknownRef { kind: "force tag", .. })));

    let mut d = data();
    d.theatre.lanes.push(LaneDef { a: TheatreNodeId::new("sea"), b: TheatreNodeId::new("cove"), traffic: 1.0 });
    assert!(run(&d).is_err());

    let mut d = data();
    d.acts[0].coves = vec![TheatreNodeId::new("sea")];
    assert!(run(&d).is_err());
}

#[test]
fn briefing_shape_rules() {
    let mut d = data();
    d.operations[0].tags.truncate(1);
    assert!(run(&d).is_err());
    let mut d = data();
    d.operations[0].ghosts.clear();
    assert!(run(&d).is_err());
    // The tutorial is exempt.
    let mut d = data();
    let mut t = op("op.tutorial", OperationKind::Tutorial);
    t.tags.clear();
    t.ghosts.clear();
    d.operations.insert(t.id.clone(), t);
    run(&d).unwrap();
}

#[test]
fn starters_must_fit_the_act_cap() {
    let d = data();
    // Every cell weighs 100 t: the minimal raft's 16 cells are far over 150 t.
    let mut heavy = catalogue();
    heavy.materials[0].cell_mass_kg = 100_000.0;
    let err = check(&d, &heavy, &|p| stock_blueprint_with(p, crate::catalogue::catalogue())).unwrap_err();
    assert!(err.to_string().contains("cap"), "{err}");
}

#[test]
fn stock_blueprints_resolve_and_cache() {
    let a = stock_blueprint("examples/minimal.bp.ron").unwrap();
    let b = stock_blueprint("blueprints\\examples\\minimal.bp.ron").unwrap();
    assert_eq!(a, b);
    assert!(matches!(stock_blueprint("examples/none.bp.ron"), Err(ContentError::UnknownRef { .. })));
    assert!(stock_blueprint("examples/minimal.kit.ron").is_err());
}

#[test]
fn embedded_data_loads_or_names_its_problem() {
    // The data units author the campaign files; until they land, the example rows may not cross-check.
    // Loading must still never panic, and a failure must name what is wrong.
    match load_embedded(crate::catalogue::catalogue()) {
        Ok(d) => assert!(!d.acts.is_empty()),
        Err(e) => assert!(!e.to_string().is_empty()),
    }
}
