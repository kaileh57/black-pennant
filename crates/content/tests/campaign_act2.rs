//! Unit `data-act2`: the Act 2 campaign data (Corran Sound) loads through the real loader and holds the rules the
//! theatre, the operation generator and the act flow rely on (DESIGN.md L747-782, L801-807, L866-875, L94-102,
//! L551, L1447).
use content::campaign::load::{load_embedded, stock_blueprint};
use content::campaign::{CampaignData, ClassRole, Complication, ConstraintTag, FleetKind, MissionType, NodeKind, Objective, OperationDef, OperationKind, Unlock};
use content::catalogue::Catalogue;
use content::data::{self, parse_ron};
use content::enums::{Act, Emcon, Phase, Stance, Tier};
use content::ids::{FleetTemplateId, OperationId};
use content::part::weapons::MunitionKind;
use content::part::{PartDef, PartFunction};
use std::collections::BTreeSet;

/// The cap per player hull in Act 2 and the commitment multiples (L94).
const CAP_T: f64 = 400.0;
const MIN_HULLS: usize = 5;
const MIN_T: f64 = 4.0 * CAP_T;
/// Enemy aircraft one operation may carry (minimum swarm, L1447).
const MAX_AIRCRAFT: usize = 6;

fn cat() -> &'static Catalogue {
    static CAT: std::sync::OnceLock<Catalogue> = std::sync::OnceLock::new();
    CAT.get_or_init(|| Catalogue::load_embedded().expect("embedded catalogue loads"))
}
fn data() -> &'static CampaignData {
    static DATA: std::sync::OnceLock<CampaignData> = std::sync::OnceLock::new();
    DATA.get_or_init(|| load_embedded(cat()).unwrap_or_else(|e| panic!("campaign data: {e}")))
}
fn act2() -> &'static content::campaign::ActDef { data().acts.iter().find(|a| a.act == Act::Act2).expect("an Act 2 row") }
fn ops() -> impl Iterator<Item = &'static OperationDef> { data().operations.values().filter(|o| o.act == Act::Act2) }

fn tonnage(class: &content::ids::ClassId) -> f64 {
    let c = &data().classes[class];
    content::stats::compute_stats(&stock_blueprint(&c.blueprint).unwrap(), cat()).tonnage_t
}
fn combat(class: &content::ids::ClassId) -> bool { !matches!(data().classes[class].role, ClassRole::Freighter | ClassRole::Tanker | ClassRole::Tender | ClassRole::Civilian) }
/// Worst draw of a fleet: (combat hulls, combat tonnage) taking each slot's minimum count of its lightest class.
fn worst(fleet: &FleetTemplateId) -> (usize, f64) {
    let f = &data().fleets[fleet];
    let (mut h, mut t) = (0, 0.0);
    for s in f.slots.iter().filter(|s| s.classes.iter().all(combat)) {
        let light = s.classes.iter().map(tonnage).fold(f64::INFINITY, f64::min);
        h += s.min as usize;
        t += s.min as f64 * light;
    }
    (h, t)
}
fn aircraft_in(fleet: &FleetTemplateId) -> usize {
    data().fleets[fleet].bodies.iter().filter(|(m, _)| cat().munition(m).is_some_and(|d| d.kind == MunitionKind::Aircraft)).map(|(_, n)| *n as usize).sum()
}
/// Hostile combat hulls and tonnage an operation's force commits on its worst draw (L94).
fn committed(o: &OperationDef) -> (usize, f64) {
    let protected = match &o.objective { Objective::Escort { tag, .. } => Some(tag.as_str()), _ => None };
    o.force.iter().filter(|e| e.stance != Stance::Neutral && Some(e.tag.as_str()) != protected).map(|e| worst(&e.fleet)).fold((0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1))
}

#[test]
fn the_act2_row_opens_corran_sound_under_the_400_t_cap() {
    let c = data();
    let a = act2();
    assert_eq!(c.acts.iter().map(|a| a.act).collect::<Vec<_>>()[..2], [Act::Act1, Act::Act2]);
    assert_eq!((a.phase, a.start_tier), (Phase::Act2, Tier::T4), "tier 4 opens in Act 2 (L805)");
    assert!(a.tier_unlocks.is_empty(), "Act 2 opens no tier inside the act");
    assert_eq!(a.starters, ["starters/corvette40.bp.ron"], "the 40 m missile corvette (L870)");
    let st = content::stats::compute_stats(&stock_blueprint(&a.starters[0]).unwrap(), cat());
    assert!(st.tonnage_t <= Act::Act2.hull_cap_t() && st.tonnage_t > Act::Act1.hull_cap_t(), "{} t", st.tonnage_t);
    // The starter is buildable from the tiers open at the act start.
    let bp = stock_blueprint(&a.starters[0]).unwrap();
    assert!(bp.parts.iter().all(|p| cat().part(&p.part).unwrap().tier <= a.start_tier));
    assert_eq!(a.story_ops.len(), 4, "four story operations (L778)");
    assert_eq!(a.ace_duels.len(), 2);
    assert_eq!(a.aces.len(), 2, "two ace arcs in Act 2 (L774)");
    assert_eq!(a.regions.iter().map(|r| r.as_str()).collect::<Vec<_>>(), ["region.corran"]);
    assert_eq!(a.coves.len(), 3, "a normal act opens 3 coves (L750)");
    let node = |id: &content::ids::TheatreNodeId| c.theatre.nodes.iter().find(|n| &n.id == id).unwrap();
    for cove in &a.coves { assert_eq!(node(cove).region.as_str(), "region.corran", "{cove}"); }
    assert!(a.retreat_closed_cove.as_ref().is_some_and(|x| a.coves.contains(x)));
    assert!(!a.opens_open_theatre);
    // Story operations and the capstone take place in Corran Sound.
    for id in a.story_ops.iter().chain([&a.capstone]) {
        let n = c.operations[id].node.as_ref().unwrap_or_else(|| panic!("{id} has a node"));
        assert_eq!(node(n).region.as_str(), "region.corran", "{id}");
    }
}

#[test]
fn act2_has_eleven_templates_of_its_own() {
    let templates: Vec<&OperationDef> = ops().filter(|o| matches!(o.kind, OperationKind::Template(_))).collect();
    let kinds: BTreeSet<MissionType> = templates.iter().filter_map(|o| match o.kind { OperationKind::Template(t) => Some(t), _ => None }).collect();
    assert_eq!((templates.len(), kinds.len()), (11, 11), "one row per template (L778)");
    for o in &templates {
        // The generator scopes templates to the latest act that has any: these replace Act 1's from the Act 2
        // start, so they cannot wait on Act 1 progress (a campaign may start in Act 2).
        assert_eq!(o.unlock, Unlock::Always, "{}", o.id);
        for e in &o.force { assert_eq!(data().fleets[&e.fleet].act, Act::Act2, "{}: {}", o.id, e.fleet); }
        for c in &o.complications {
            if let Complication::Reinforcements { fleet, .. } = c { assert_eq!(data().fleets[fleet].act, Act::Act2, "{}", o.id); }
        }
    }
    assert_eq!(ops().filter(|o| o.kind == OperationKind::Capstone).count(), 1);
    assert_eq!(ops().filter(|o| o.kind == OperationKind::Story).count(), 4);
    assert_eq!(ops().filter(|o| matches!(o.kind, OperationKind::AceDuel(_))).count(), 2);
}

/// Every Act 2 operation commits 5 hulls and 1,600 t on its force's worst draw, and every task group,
/// hunter-killer and convoy escort does alone, since the generator draws reinforcement waves from them (L747).
#[test]
fn act2_forces_meet_the_commitment_floor_on_their_worst_draw() {
    let c = data();
    for (id, f) in c.fleets.iter().filter(|(_, f)| f.act == Act::Act2) {
        if matches!(f.kind, FleetKind::TaskGroup | FleetKind::HunterKiller) || id.as_str() == "fleet.act2.escort.convoy" {
            let (h, t) = worst(id);
            assert!(h >= MIN_HULLS && t >= MIN_T, "{id}: worst draw {h} hulls, {t:.0} t");
        }
        for s in &f.slots {
            assert!(s.classes.iter().all(|cl| !cl.as_str().starts_with("class.ace_")) || f.slots.len() == 1, "{id}: ace flagships sail alone");
        }
    }
    for o in ops() {
        let (h, t) = committed(o);
        match o.kind {
            OperationKind::Template(MissionType::HunterGroupSurvival) => assert!(h >= 1, "{}", o.id),
            _ => assert!(h >= MIN_HULLS && t >= MIN_T, "{}: {h} hulls, {t:.0} t", o.id),
        }
    }
}

/// Strike aircraft sail in hull-less air patrols; the generic two-aircraft patrol is the one the generator adds
/// at Alert 3 and for the air-patrol complication, and no operation can reach more than six aircraft even with
/// both added (L1447).
#[test]
fn strike_aircraft_stay_inside_the_swarm_budget() {
    let c = data();
    let air: Vec<(&FleetTemplateId, &content::campaign::FleetTemplate)> = c.fleets.iter().filter(|(_, f)| f.kind == FleetKind::AirPatrol).collect();
    assert!(!air.is_empty(), "Act 2 brings strike aircraft (L870)");
    for (id, f) in &air {
        assert_eq!(f.act, Act::Act2, "{id}");
        assert!(f.slots.is_empty() && !f.bodies.is_empty(), "{id}: aircraft only");
        assert!(f.bodies.iter().all(|(m, n)| *n > 0 && cat().munition(m).is_some_and(|d| d.kind == MunitionKind::Aircraft && d.phase == Phase::Act2)), "{id}");
    }
    let first = air.iter().map(|(id, _)| *id).min().unwrap();
    assert_eq!(first.as_str(), "fleet.act2.air");
    let patrol = aircraft_in(first);
    for o in ops() {
        let own: usize = o.force.iter().map(|e| aircraft_in(&e.fleet)).sum();
        let complication = if o.complications.contains(&Complication::AirPatrol) { patrol } else { 0 };
        assert!(own + complication + patrol <= MAX_AIRCRAFT, "{}: up to {} aircraft", o.id, own + complication + patrol);
    }
    // The air threat is part of the act: most operations can draw it, and the capstone brings a raid.
    assert!(ops().filter(|o| o.complications.contains(&Complication::AirPatrol) || o.force.iter().any(|e| aircraft_in(&e.fleet) > 0)).count() >= 12);
    assert!(c.operations[&act2().capstone].force.iter().any(|e| aircraft_in(&e.fleet) > 0));
}

#[test]
fn act2_operations_are_well_formed() {
    let c = data();
    // Tier 4 opens on the first day of the act and the dry dock offers every part of an open tier, so a blueprint
    // reward (L815: unlocks within tiers already open) would unlock nothing in Act 2. Act 2's ghosts pay Intel,
    // and capturing an ace pays the most.
    assert!(act2().start_tier == Tier::T4 && act2().tier_unlocks.is_empty());
    for o in ops() {
        let id = o.id.as_str();
        assert_eq!(o.name.as_str(), format!("{id}.name"));
        assert_eq!(o.briefing.as_str(), format!("{id}.briefing"));
        assert!(id.starts_with("op.act2."), "{id}");
        if let Objective::DestroyGroup { fraction, .. } = &o.objective { assert!((0.5..=1.0).contains(fraction), "{id}"); }
        assert!((4_000.0..=20_000.0).contains(&o.approach_m), "{id}: approach");
        match o.kind {
            OperationKind::Template(_) | OperationKind::AceDuel(_) => assert!(o.node.is_none() && o.map.is_none(), "{id}"),
            _ => assert!(o.node.is_some(), "{id}"),
        }
        use content::campaign::StationAction as A;
        let need = match &o.objective {
            Objective::Extract { .. } => Some(A::InsertOrExtract),
            Objective::LayMines { .. } => Some(A::LayMines),
            Objective::Capture { .. } | Objective::CutOut { .. } => Some(A::BoardAndCapture),
            Objective::Photograph { .. } => Some(A::Photograph),
            Objective::Escort { .. } => Some(A::CarryPassengers),
            _ => None,
        };
        if let Some(a) = need { assert!(o.actions.contains(&a), "{id} needs {a:?}"); }
        if o.ghosts.iter().any(|g| g.kind == content::campaign::GhostKind::NoRadarEmissions) {
            assert_eq!(o.recommended_emcon, Some(Emcon::Silent), "{id}: recommend an emission setting");
        }
        // Every tag a briefing asks for can be met by the starter's draft and the parts open in the act.
        for t in &o.tags {
            match t {
                ConstraintTag::DraftLimit { max_m } => assert!(*max_m >= 3.0, "{id}: the 40 m starter draws about 2 m"),
                ConstraintTag::Requires(tag) => assert!(cat().parts.values().any(|p| p.tags.contains(tag) && p.tier <= act2().start_tier), "{id}: {tag:?}"),
                _ => {}
            }
        }
        for g in &o.ghosts {
            assert!(matches!(g.reward, content::campaign::Reward::Intel(1..=3)), "{id}: {:?}", g.reward);
            if g.kind == content::campaign::GhostKind::CaptureTheAce { assert_eq!(g.reward, content::campaign::Reward::Intel(3), "{id}"); }
        }
        if let OperationKind::AceDuel(ace) = &o.kind {
            let ship = &c.aces[ace].ship;
            let ace_entry = o.force.iter().find(|e| e.tag == "ace").expect("ace entry");
            assert_eq!(c.fleets[&ace_entry.fleet].slots[0].classes, vec![ship.clone()], "{id}");
            assert_eq!(o.objective, Objective::DestroyGroup { tag: "ace".into(), fraction: 1.0 });
        }
    }
}

/// The story operations unlock in order of play, each by progress or a theatre condition, and nothing in the act
/// can stall (L778).
#[test]
fn act2_story_unlocks_can_always_be_met() {
    let c = data();
    let act = act2();
    fn met(u: &Unlock, done: &BTreeSet<OperationId>) -> bool {
        match u {
            Unlock::Always => true,
            Unlock::Completed(v) => v.iter().all(|o| done.contains(o)),
            Unlock::All(v) => v.iter().all(|x| met(x, done)),
            Unlock::Any(v) => v.iter().any(|x| met(x, done)),
            _ => false,
        }
    }
    let order: Vec<OperationId> = act.story_ops.iter().chain(&act.ace_duels).cloned().chain([act.capstone.clone()]).collect();
    let mut done = BTreeSet::new();
    for _ in 0..order.len() {
        let next = order.iter().find(|o| !done.contains(*o) && met(&c.operations[*o].unlock, &done) && (c.operations[*o].kind != OperationKind::Capstone || act.story_ops.iter().all(|s| done.contains(s))));
        done.insert(next.unwrap_or_else(|| panic!("stalled after {done:?}")).clone());
    }
    fn has_condition(u: &Unlock) -> bool { match u { Unlock::SupplyBelow { .. } | Unlock::AlertAtLeast { .. } => true, Unlock::All(v) | Unlock::Any(v) => v.iter().any(has_condition), _ => false } }
    for s in &act.story_ops[1..] { assert!(has_condition(&c.operations[s].unlock), "{s} is unlocked by theatre conditions too"); }
    // Nothing in Act 2 waits on an Act 1 operation: a campaign can start in Act 2.
    fn names(u: &Unlock, out: &mut Vec<OperationId>) { match u { Unlock::Completed(v) => out.extend(v.iter().cloned()), Unlock::All(v) | Unlock::Any(v) => v.iter().for_each(|x| names(x, out)), _ => {} } }
    for o in ops() {
        let mut v = Vec::new();
        names(&o.unlock, &mut v);
        assert!(v.iter().all(|x| c.operations[x].act == Act::Act2), "{}", o.id);
    }
}

#[test]
fn act2_aces_sail_their_own_ships_and_arrive_before_their_duels() {
    let c = data();
    for id in &act2().aces {
        let a = &c.aces[id];
        assert_eq!(c.classes[&a.ship].act, Act::Act2, "{id}");
        assert!(a.ship.as_str().starts_with("class.ace_"), "{id}: a signature ship");
        let arc = a.arcs.iter().find(|x| x.act == Act::Act2).expect("an Act 2 arc");
        let duel = c.operations.values().find(|o| o.kind == OperationKind::AceDuel(id.clone())).expect("a duel");
        assert!(arc.operations.contains(&duel.id), "{id}: the duel is in the arc");
        assert!(arc.operations.iter().all(|o| c.operations[o].act == Act::Act2), "{id}");
        // The ace arrives in a story operation the duel waits for.
        let arrives: Vec<&OperationId> = arc.operations.iter().filter(|o| c.operations[*o].kind == OperationKind::Story && c.operations[*o].complications.contains(&Complication::AceArrives(id.clone()))).collect();
        assert!(arrives.iter().any(|o| duel.unlock == Unlock::Completed(vec![(*o).clone()])), "{id}");
    }
}

/// The new classes field in Corran Sound beside the Act 1 hulls still in service; the hunter group's top tier
/// is the helicopter frigate.
#[test]
fn corran_fields_the_act2_classes() {
    let c = data();
    let r = c.theatre.regions.iter().find(|r| r.id.as_str() == "region.corran").unwrap();
    assert_eq!(r.act, Act::Act2);
    let fielded: BTreeSet<&str> = r.order_of_battle.iter().map(|(cl, _)| cl.as_str()).chain(r.civilian.iter().map(|cl| cl.as_str())).collect();
    for cl in c.classes.values().filter(|x| x.act == Act::Act2 && !x.id.as_str().starts_with("class.ace_")) {
        assert!(fielded.contains(cl.id.as_str()), "{} is not in Corran's order of battle", cl.id);
    }
    assert!(r.civilian.iter().any(|cl| c.classes[cl].act == Act::Act2));
    let top = c.classes.values().filter(|x| x.act == Act::Act2 && combat(&x.id)).map(|x| x.tier).max().unwrap();
    assert_eq!(c.classes.values().find(|x| x.tier == top && x.act == Act::Act2 && combat(&x.id)).unwrap().id.as_str(), "class.mortgage");
    // Act 2 hulls are bigger than Act 1's: its fleets meet 4 x 400 t in five hulls.
    let heavy = c.classes.values().filter(|x| x.act == Act::Act2 && combat(&x.id) && tonnage(&x.id) >= 300.0).count();
    assert!(heavy >= 6, "{heavy} Act 2 combat classes of 300 t or more");
    // Variants are heavier hulls of the same role (L834).
    for cl in c.classes.values().filter(|x| x.act == Act::Act2) {
        for v in &cl.variants {
            assert_eq!(c.classes[v].role, cl.role);
            assert!(tonnage(v) > tonnage(&cl.id), "{} -> {v}", cl.id);
        }
    }
}

/// The helicopter deck arrives with Act 2 (L551, L870): one part in parts/act2.ron and its stock kit.
#[test]
fn the_helicopter_deck_arrives_with_act2() {
    let parts: Vec<PartDef> = parse_ron(data::PARTS[1], "parts/act2.ron").unwrap();
    assert_eq!(parts.len(), 1);
    let p = &parts[0];
    assert_eq!((p.id.as_str(), p.tier, p.phase), ("deck.heli", Tier::T4, Phase::Act2));
    let PartFunction::HeliDeck { helicopter } = &p.function else { panic!("a helicopter deck") };
    let h = cat().munition(helicopter).expect("its helicopter");
    assert_eq!((h.kind, h.phase), (MunitionKind::Helicopter, Phase::Act2));
    // The pad fits a 400 t hull's beam (the 40 m starter is 8.2 m wide) and the hull-mesh generator's range.
    assert!(p.size[2] as f64 * content::cell::CELL_M <= 8.2);
    assert_eq!(p.model.params, [p.size[0] as f32 * 0.5, p.size[2] as f32 * 0.5]);
    assert!(cat().kits.values().any(|k| k.parts.len() == 1 && k.parts[0].part == p.id), "a stock kit carries it");
    // The campaign offers it from the Act 2 start at tier 4, not in Act 1 even with every tier open and the part
    // unlocked; sandbox offers it at once (L551).
    let idx = cat().part_idx(&p.id).expect("in the catalogue");
    let open = [p.id.clone()];
    use content::enums::GameMode;
    assert!(!cat().available_parts(GameMode::Campaign, Act::Act1, Tier::T5, &open).contains(&idx));
    assert!(cat().available_parts(GameMode::Campaign, Act::Act2, act2().start_tier, &open).contains(&idx));
    assert!(cat().available_parts(GameMode::Sandbox, Act::Act1, Tier::T1, &[]).contains(&idx));
}

#[test]
fn act2_text_has_english_rows() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../game/i18n");
    let mut keys = BTreeSet::new();
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        if e.path().extension().is_some_and(|x| x == "csv") {
            for line in std::fs::read_to_string(e.path()).unwrap().lines().skip(1) {
                if let Some((k, _)) = line.split_once(',') { keys.insert(k.to_string()); }
            }
        }
    }
    let c = data();
    let mut need: Vec<String> = vec![act2().name.0.clone()];
    need.extend(ops().flat_map(|o| [o.name.0.clone(), o.briefing.0.clone()]));
    need.extend(c.classes.values().filter(|x| x.act == Act::Act2).map(|x| x.name.0.clone()));
    need.extend(["part.deck.heli.name", "part.deck.heli.desc", "kit.heli_deck.name", "kit.heli_deck.desc"].map(String::from));
    let missing: Vec<&String> = need.iter().filter(|k| !keys.contains(*k)).collect();
    assert!(missing.is_empty(), "no English row for {missing:?}");
    // The Act 2 nodes the story names exist and are the right kinds.
    let kind = |id: &str| c.theatre.nodes.iter().find(|n| n.id.as_str() == id).map(|n| n.kind.clone()).unwrap();
    assert_eq!(kind("esm.ness"), NodeKind::EsmSite);
    assert_eq!(kind("base.sallow"), NodeKind::ForwardBase);
    assert_eq!(kind("harbour.kittering"), NodeKind::Harbour);
}
