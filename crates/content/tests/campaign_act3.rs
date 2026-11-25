//! Unit `data-act3`: the Act 3 campaign data (Hollin) loads through the real loader and holds the rules the
//! theatre, the operation generator and the act flow rely on (DESIGN.md L747-782, L801-807, L866-875, L94-102,
//! L451, L541-553, L595, L618, L1447).
use content::campaign::load::{load_embedded, stock_blueprint};
use content::campaign::{CampaignData, ClassRole, Complication, ConstraintTag, FleetKind, MissionType, NodeKind, Objective, OperationDef, OperationKind, Reward, Unlock};
use content::catalogue::Catalogue;
use content::data::{self, parse_ron};
use content::enums::{Act, Emcon, GameMode, Phase, Stance, Tier};
use content::ids::{ClassId, FleetTemplateId, OperationId};
use content::part::weapons::{CalibreBand, MunitionKind};
use content::part::{LauncherKind, PartDef, PartFunction};
use std::collections::BTreeSet;

/// The cap per player hull in Act 3 and the commitment multiples (L94).
const CAP_T: f64 = 900.0;
const MIN_HULLS: usize = 5;
const MIN_T: f64 = 4.0 * CAP_T;
/// Enemy aircraft one operation may carry (minimum swarm, L1447).
const MAX_AIRCRAFT: usize = 6;
/// The story operation whose completion opens tier 5 (L805).
const TIER5_OP: &str = "op.act3.story.saltby";

fn cat() -> &'static Catalogue {
    static CAT: std::sync::OnceLock<Catalogue> = std::sync::OnceLock::new();
    CAT.get_or_init(|| Catalogue::load_embedded().expect("embedded catalogue loads"))
}
fn data() -> &'static CampaignData {
    static DATA: std::sync::OnceLock<CampaignData> = std::sync::OnceLock::new();
    DATA.get_or_init(|| load_embedded(cat()).unwrap_or_else(|e| panic!("campaign data: {e}")))
}
fn act3() -> &'static content::campaign::ActDef { data().acts.iter().find(|a| a.act == Act::Act3).expect("an Act 3 row") }
fn ops() -> impl Iterator<Item = &'static OperationDef> { data().operations.values().filter(|o| o.act == Act::Act3) }

fn tonnage(class: &ClassId) -> f64 {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<ClassId, f64>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(t) = cache.lock().unwrap().get(class) { return *t; }
    let c = &data().classes[class];
    let t = content::stats::compute_stats(&stock_blueprint(&c.blueprint).unwrap(), cat()).tonnage_t;
    cache.lock().unwrap().insert(class.clone(), t);
    t
}
fn combat(class: &ClassId) -> bool { !matches!(data().classes[class].role, ClassRole::Freighter | ClassRole::Tanker | ClassRole::Tender | ClassRole::Civilian) }
fn submersible(class: &ClassId) -> bool { data().classes[class].submersible }
/// Worst draw of a fleet on surface hulls alone: (combat hulls, combat tonnage) taking each slot's minimum count of
/// its lightest class. Submarines are left out so the floors hold if they are cut (L875).
fn worst(fleet: &FleetTemplateId) -> (usize, f64) {
    let f = &data().fleets[fleet];
    let (mut h, mut t) = (0, 0.0);
    for s in f.slots.iter().filter(|s| s.classes.iter().all(|c| combat(c) && !submersible(c))) {
        let light = s.classes.iter().map(tonnage).fold(f64::INFINITY, f64::min);
        h += s.min as usize;
        t += s.min as f64 * light;
    }
    (h, t)
}
fn aircraft_in(fleet: &FleetTemplateId) -> usize {
    data().fleets[fleet].bodies.iter().filter(|(m, _)| cat().munition(m).is_some_and(|d| d.kind == MunitionKind::Aircraft)).map(|(_, n)| *n as usize).sum()
}
/// Hostile surface combat hulls and tonnage an operation's force commits on its worst draw (L94).
fn committed(o: &OperationDef) -> (usize, f64) {
    let protected = match &o.objective { Objective::Escort { tag, .. } => Some(tag.as_str()), _ => None };
    o.force.iter().filter(|e| e.stance != Stance::Neutral && Some(e.tag.as_str()) != protected).map(|e| worst(&e.fleet)).fold((0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1))
}
fn has_subs(fleet: &FleetTemplateId) -> bool { data().fleets[fleet].slots.iter().any(|s| s.classes.iter().any(submersible)) }
/// True when every way of meeting `u` completes `op` first.
fn requires(u: &Unlock, op: &str) -> bool {
    match u {
        Unlock::Completed(v) => v.iter().any(|o| o.as_str() == op),
        Unlock::All(v) => v.iter().any(|x| requires(x, op)),
        Unlock::Any(v) => !v.is_empty() && v.iter().all(|x| requires(x, op)),
        _ => false,
    }
}

#[test]
fn the_act3_row_opens_hollin_under_the_900_t_cap() {
    let c = data();
    let a = act3();
    assert_eq!(c.acts.iter().map(|a| a.act).collect::<Vec<_>>()[..3], [Act::Act1, Act::Act2, Act::Act3]);
    assert_eq!((a.phase, a.start_tier), (Phase::Act3, Tier::T4), "Act 3 starts on tier 4");
    assert_eq!(a.tier_unlocks, vec![(OperationId::new(TIER5_OP), Tier::T5)], "tier 5 opens inside Act 3 (L805)");
    assert_eq!(a.story_ops[0].as_str(), TIER5_OP, "the first story operation opens tier 5");
    assert_eq!(a.starters, ["starters/corvette60.bp.ron"], "the 60 m corvette with mine rails and boarding kit (L873)");
    let bp = stock_blueprint(&a.starters[0]).unwrap();
    let st = content::stats::compute_stats(&bp, cat());
    assert!(st.tonnage_t <= Act::Act3.hull_cap_t() && st.tonnage_t > Act::Act2.hull_cap_t(), "{} t", st.tonnage_t);
    // The starter is buildable from the tiers open at the act start, and carries the act's special-operations parts.
    assert!(bp.parts.iter().all(|p| cat().part(&p.part).unwrap().tier <= a.start_tier));
    for tag in [content::part::PartTag::MineRail, content::part::PartTag::BoardingKit] {
        assert!(bp.parts.iter().any(|p| cat().part(&p.part).unwrap().tags.contains(&tag)), "starter carries {tag:?}");
    }
    assert_eq!(a.story_ops.len(), 4, "four story operations (L778)");
    assert_eq!(a.ace_duels.len(), 2);
    assert_eq!(a.aces.len(), 2, "two ace arcs in Act 3 (L774)");
    assert_eq!(a.regions.iter().map(|r| r.as_str()).collect::<Vec<_>>(), ["region.hollin"]);
    assert_eq!(a.coves.len(), 3, "a normal act opens 3 coves (L750)");
    let node = |id: &content::ids::TheatreNodeId| c.theatre.nodes.iter().find(|n| &n.id == id).unwrap();
    for cove in &a.coves {
        assert_eq!(node(cove).region.as_str(), "region.hollin", "{cove}");
        assert_eq!(node(cove).kind, NodeKind::Cove, "{cove}");
    }
    assert!(a.retreat_closed_cove.as_ref().is_some_and(|x| a.coves.contains(x)));
    assert!(!a.opens_open_theatre);
    for id in a.story_ops.iter().chain([&a.capstone]) {
        let n = c.operations[id].node.as_ref().unwrap_or_else(|| panic!("{id} has a node"));
        assert_eq!(node(n).region.as_str(), "region.hollin", "{id}");
    }
}

#[test]
fn act3_has_eleven_templates_of_its_own() {
    let templates: Vec<&OperationDef> = ops().filter(|o| matches!(o.kind, OperationKind::Template(_))).collect();
    let kinds: BTreeSet<MissionType> = templates.iter().filter_map(|o| match o.kind { OperationKind::Template(t) => Some(t), _ => None }).collect();
    assert_eq!((templates.len(), kinds.len()), (11, 11), "one row per template (L778)");
    // Only Act 2's air patrols sail in Act 3 from earlier acts: Act 3 adds no aircraft of its own.
    let own_act = |f: &FleetTemplateId| data().fleets[f].act == Act::Act3 || data().fleets[f].kind == FleetKind::AirPatrol;
    for o in &templates {
        // A campaign may start in Act 3, and the templates replace Act 2's from its first day.
        assert_eq!(o.unlock, Unlock::Always, "{}", o.id);
        for e in &o.force { assert!(own_act(&e.fleet), "{}: {}", o.id, e.fleet); }
        for c in &o.complications {
            if let Complication::Reinforcements { fleet, .. } = c { assert!(own_act(fleet), "{}", o.id); }
        }
    }
    assert_eq!(ops().filter(|o| o.kind == OperationKind::Capstone).count(), 1);
    assert_eq!(ops().filter(|o| o.kind == OperationKind::Story).count(), 4);
    assert_eq!(ops().filter(|o| matches!(o.kind, OperationKind::AceDuel(_))).count(), 2);
}

/// Every Act 3 operation commits 5 hulls and 3,600 t on its surface force's worst draw, and every task group,
/// hunter-killer and the convoy escort does alone, since the generator draws reinforcement waves from them (L747).
#[test]
fn act3_forces_meet_the_commitment_floor_on_their_worst_draw() {
    let c = data();
    for (id, f) in c.fleets.iter().filter(|(_, f)| f.act == Act::Act3) {
        if matches!(f.kind, FleetKind::TaskGroup | FleetKind::HunterKiller) || id.as_str() == "fleet.act3.escort.convoy" {
            let (h, t) = worst(id);
            assert!(h >= MIN_HULLS && t >= MIN_T, "{id}: worst draw {h} hulls, {t:.0} t");
            // Waves and task groups never carry the capital: the generator would drop it at the capital limit.
            assert!(f.slots.iter().flat_map(|s| &s.classes).all(|cl| !c.classes[cl].capital), "{id}");
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
        // At most 4 hulls over 90 m (L126): the force's capitals plus an arriving ace's ship.
        let capitals: usize = o.force.iter().map(|e| data().fleets[&e.fleet].slots.iter().filter(|s| s.classes.iter().any(|cl| c.classes[cl].capital)).map(|s| s.max as usize).sum::<usize>()).sum::<usize>()
            + o.complications.iter().filter(|x| matches!(x, Complication::AceArrives(a) if c.classes[&c.aces[a].ship].capital)).count();
        assert!(capitals <= c.rules.max_capitals as usize, "{}: {capitals} capitals", o.id);
    }
}

/// Submarines are additive (L875): they sail only in submarine patrols beside a surface force, never as an
/// objective, and every map an authored submarine operation uses has water enough for them to dive.
#[test]
fn submarines_sail_apart_and_can_always_be_cut() {
    let c = data();
    let subs: Vec<&ClassId> = c.classes.keys().filter(|k| submersible(k)).collect();
    assert_eq!(subs.iter().map(|s| s.as_str()).collect::<Vec<_>>(), ["class.tontine", "class.accrual"]);
    for s in &subs {
        let d = &c.classes[*s];
        assert_eq!((d.act, d.role, d.behaviour.as_str()), (Act::Act3, ClassRole::Submarine, "submarine"), "{s}");
        assert!(d.air_wing.is_empty() && !d.capital, "{s}");
    }
    // The depth-charge gate's reference submarine (L618) is the coastal boat.
    assert!(c.classes[&ClassId::new("class.tontine")].submersible);
    for (id, f) in &c.fleets {
        let any = has_subs(id);
        assert_eq!(any, f.kind == FleetKind::SubmarinePatrol, "{id}: submarines sail only in submarine patrols");
        if any { assert!(f.slots.iter().flat_map(|s| &s.classes).all(submersible), "{id}: a patrol of boats only"); }
    }
    let mut with_subs = 0;
    for o in ops() {
        let sub_tags: Vec<&str> = o.force.iter().filter(|e| has_subs(&e.fleet)).map(|e| e.tag.as_str()).collect();
        if sub_tags.is_empty() && !o.complications.iter().any(|x| matches!(x, Complication::Reinforcements { fleet, .. } if has_subs(fleet))) { continue; }
        with_subs += 1;
        let objective_tag = match &o.objective {
            Objective::DestroyGroup { tag, .. } | Objective::Capture { tag } | Objective::Escort { tag, .. } | Objective::Photograph { tag, .. } | Objective::CutOut { tag, .. } => Some(tag.as_str()),
            _ => None,
        };
        assert!(objective_tag.is_none_or(|t| !sub_tags.contains(&t)), "{}: a submarine is never the objective", o.id);
        assert!(sub_tags.iter().all(|t| o.force.iter().filter(|e| e.tag == *t).all(|e| has_subs(&e.fleet))), "{}: boats keep a tag of their own", o.id);
        if let Some(n) = &o.node {
            let map = &c.theatre.nodes.iter().find(|x| &x.id == n).unwrap().map;
            assert!(map.depth_m >= 35.0, "{}: {} m of water", o.id, map.depth_m);
        }
    }
    assert!(with_subs >= 8, "{with_subs} Act 3 operations meet submarines");
    // The boats can dive anywhere in Hollin: every node map is at least 35 m deep (the combine_act3 sheets keep a
    // boat's full height plus the AI's 9 m of margins within that).
    for n in c.theatre.nodes.iter().filter(|n| n.region.as_str() == "region.hollin") { assert!(n.map.depth_m >= 35.0, "{}", n.id); }
    // Some story water has the 60 m layer for the boats to hide under (L455).
    let layer = c.theatre.regions.iter().find(|r| r.id.as_str() == "region.hollin").unwrap().layer_depth_m;
    assert!(act3().story_ops.iter().any(|s| {
        let o = &c.operations[s];
        o.force.iter().any(|e| has_subs(&e.fleet)) && o.node.as_ref().is_some_and(|n| c.theatre.nodes.iter().any(|x| &x.id == n && x.map.depth_m > layer))
    }));
}

/// Act 3 brings no aircraft of its own; Act 2's patrols and raids stay inside the swarm budget (L1447).
#[test]
fn aircraft_stay_inside_the_swarm_budget() {
    let c = data();
    assert!(c.fleets.values().filter(|f| f.act == Act::Act3).all(|f| f.bodies.is_empty()), "Act 3 adds no air groups");
    let first = c.fleets.iter().filter(|(_, f)| f.kind == FleetKind::AirPatrol).map(|(id, _)| id).min().unwrap();
    let patrol = aircraft_in(first);
    for o in ops() {
        let own: usize = o.force.iter().map(|e| aircraft_in(&e.fleet)).sum();
        let complication = if o.complications.contains(&Complication::AirPatrol) { patrol } else { 0 };
        assert!(own + complication + patrol <= MAX_AIRCRAFT, "{}: up to {} aircraft", o.id, own + complication + patrol);
    }
    assert!(c.operations[&act3().capstone].force.iter().any(|e| aircraft_in(&e.fleet) > 0), "the capstone brings a raid");
}

#[test]
fn act3_operations_are_well_formed() {
    let c = data();
    // Tier 4 is open on the first day of the act and tier 5 opens whole with the Saltby lighter, and the dry dock
    // offers every part of an open tier, so a blueprint reward (L815: unlocks within tiers already open) would
    // unlock nothing in Act 3. Act 3's ghosts pay Intel, and capturing an ace pays the most.
    assert!(act3().start_tier == Tier::T4 && act3().tier_unlocks.iter().all(|(_, t)| *t == Tier::T5));
    let starter = stock_blueprint(&act3().starters[0]).unwrap();
    for o in ops() {
        let id = o.id.as_str();
        assert_eq!(o.name.as_str(), format!("{id}.name"));
        assert_eq!(o.briefing.as_str(), format!("{id}.briefing"));
        assert!(id.starts_with("op.act3."), "{id}");
        assert!((2..=4).contains(&o.tags.len()) && (2..=3).contains(&o.ghosts.len()), "{id}: 2 to 4 tags, 2 or 3 ghosts (L780, L801)");
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
        // Every tag a briefing asks for can be met by the starter's draft or a refit from the parts open in the act.
        for t in &o.tags {
            match t {
                ConstraintTag::DraftLimit { max_m } => assert!(*max_m >= 4.0, "{id}: the 60 m starter floats at about 2.7 m"),
                ConstraintTag::Requires(tag) => assert!(cat().parts.values().any(|p| p.tags.contains(tag) && p.tier <= act3().start_tier), "{id}: {tag:?}"),
                ConstraintTag::Passengers { count, allowed_losses } => assert!(allowed_losses < count && *count <= 8, "{id}"),
                _ => {}
            }
        }
        for g in &o.ghosts {
            assert!(matches!(g.reward, Reward::Intel(1..=3)), "{id}: {:?}", g.reward);
            if g.kind == content::campaign::GhostKind::CaptureTheAce { assert_eq!(g.reward, Reward::Intel(3), "{id}"); }
        }
        if let OperationKind::AceDuel(ace) = &o.kind {
            let ship = &c.aces[ace].ship;
            let ace_entry = o.force.iter().find(|e| e.tag == "ace").expect("ace entry");
            assert_eq!(c.fleets[&ace_entry.fleet].slots[0].classes, vec![ship.clone()], "{id}");
            assert_eq!(o.objective, Objective::DestroyGroup { tag: "ace".into(), fraction: 1.0 });
        }
    }
    // The starter answers every special-operations tag of the story chain without a refit.
    for s in &act3().story_ops {
        for t in &c.operations[s].tags {
            if let ConstraintTag::Requires(tag) = t { assert!(starter.parts.iter().any(|p| cat().part(&p.part).unwrap().tags.contains(tag)), "{s}: {tag:?}"); }
        }
    }
}

/// The story operations unlock in order of play, each by progress or a theatre condition, and nothing in the act
/// can stall (L778).
#[test]
fn act3_story_unlocks_can_always_be_met() {
    let c = data();
    let act = act3();
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
    // Nothing in Act 3 waits on an earlier act's operation: a campaign can start in Act 3.
    fn names(u: &Unlock, out: &mut Vec<OperationId>) { match u { Unlock::Completed(v) => out.extend(v.iter().cloned()), Unlock::All(v) | Unlock::Any(v) => v.iter().for_each(|x| names(x, out)), _ => {} } }
    for o in ops() {
        let mut v = Vec::new();
        names(&o.unlock, &mut v);
        assert!(v.iter().all(|x| c.operations[x].act == Act::Act3), "{}", o.id);
    }
}

#[test]
fn act3_aces_sail_their_own_ships_and_arrive_before_their_duels() {
    let c = data();
    for id in &act3().aces {
        let a = &c.aces[id];
        assert_eq!(c.classes[&a.ship].act, Act::Act3, "{id}");
        assert!(a.ship.as_str().starts_with("class.ace_"), "{id}: a signature ship");
        assert!(!c.classes[&a.ship].submersible, "{id}: aces command surface ships (boarding, hunter groups)");
        let arc = a.arcs.iter().find(|x| x.act == Act::Act3).expect("an Act 3 arc");
        let duel = c.operations.values().find(|o| o.kind == OperationKind::AceDuel(id.clone())).expect("a duel");
        assert!(arc.operations.contains(&duel.id), "{id}: the duel is in the arc");
        assert!(arc.operations.iter().all(|o| c.operations[o].act == Act::Act3), "{id}");
        // The ace arrives in a story operation the duel waits for.
        let arrives: Vec<&OperationId> = arc.operations.iter().filter(|o| c.operations[*o].kind == OperationKind::Story && c.operations[*o].complications.contains(&Complication::AceArrives(id.clone()))).collect();
        assert!(arrives.iter().any(|o| requires(&duel.unlock, o.as_str())), "{id}");
    }
    // The aces are listed in the order they arrive, so the hunter group's ace is the one already met.
    let first_story = |ace: &content::ids::AceId| act3().story_ops.iter().position(|s| c.operations[s].complications.contains(&Complication::AceArrives(ace.clone()))).unwrap();
    assert!(act3().aces.windows(2).all(|w| first_story(&w[0]) < first_story(&w[1])));
}

/// The new classes field in Hollin beside the earlier hulls still in service; the hunter group's top tier is the
/// heavy destroyer.
#[test]
fn hollin_fields_the_act3_classes() {
    let c = data();
    let r = c.theatre.regions.iter().find(|r| r.id.as_str() == "region.hollin").unwrap();
    assert_eq!(r.act, Act::Act3);
    let fielded: BTreeSet<&str> = r.order_of_battle.iter().map(|(cl, _)| cl.as_str()).chain(r.civilian.iter().map(|cl| cl.as_str())).collect();
    for cl in c.classes.values().filter(|x| x.act == Act::Act3 && !x.id.as_str().starts_with("class.ace_")) {
        assert!(fielded.contains(cl.id.as_str()), "{} is not in Hollin's order of battle", cl.id);
    }
    assert!(r.civilian.iter().any(|cl| c.classes[cl].act == Act::Act3));
    assert!(r.order_of_battle.iter().all(|(cl, _)| !c.classes[cl].capital && !cl.as_str().starts_with("class.ace_")), "aces and capitals are not pooled hulls");
    // Theatre alert takes the first top-tier surface combatant of the act for the hunter group (L749).
    let surface = |x: &&content::campaign::EnemyClassDef| x.act == Act::Act3 && combat(&x.id) && !x.submersible;
    let top = c.classes.values().filter(surface).map(|x| x.tier).max().unwrap();
    assert_eq!(c.classes.values().filter(surface).find(|x| x.tier == top).unwrap().id.as_str(), "class.equity");
    // Act 3 hulls are bigger than Act 2's: its fleets meet 4 x 900 t in five hulls.
    let heavy = c.classes.values().filter(|x| x.act == Act::Act3 && combat(&x.id) && tonnage(&x.id) >= 700.0).count();
    assert!(heavy >= 7, "{heavy} Act 3 combat classes of 700 t or more");
    // Variants are heavier hulls of the same role (L834).
    for cl in c.classes.values().filter(|x| x.act == Act::Act3) {
        for v in &cl.variants {
            assert_eq!(c.classes[v].role, cl.role);
            assert!(tonnage(v) > tonnage(&cl.id), "{} -> {v}", cl.id);
        }
    }
}

/// Heavy guns and depth charges arrive with Act 3 (L551, L595, L873): rows in parts/act3.ron and their stock kits.
#[test]
fn heavy_guns_and_depth_charges_arrive_with_act3() {
    let parts: Vec<PartDef> = parse_ron(data::PARTS[2], "parts/act3.ron").unwrap();
    assert!(parts.iter().all(|p| p.phase == Phase::Act3), "every Act 3 part is phase Act3");
    let guns: Vec<&PartDef> = parts.iter().filter(|p| matches!(p.function, PartFunction::Gun(_))).collect();
    assert!(guns.len() >= 2);
    for g in &guns {
        let PartFunction::Gun(d) = &g.function else { unreachable!() };
        assert_eq!(d.band, CalibreBand::Heavy, "{}", g.id);
        assert!((130..=155).contains(&d.calibre_mm), "{}: 130 to 155 mm (L547)", g.id);
        assert_eq!(g.tier, Tier::T5, "{}: heavy guns sit in tier 5", g.id);
        // Mount weight gates them under the 900 t cap (L551): heavy, but a 900 t hull can carry one.
        assert!(g.mass_kg >= 25_000.0 && g.mass_kg <= 0.1 * CAP_T * 1_000.0, "{}: {} kg", g.id, g.mass_kg);
        assert!(parts.iter().any(|m| matches!(m.function, PartFunction::Magazine { calibre_mm, .. } if calibre_mm == d.calibre_mm)), "{}: magazine", g.id);
        assert!(cat().kits.values().any(|k| k.parts.iter().any(|kp| kp.part == g.id)), "{}: a stock kit carries it", g.id);
    }
    let racks: Vec<&PartDef> = parts.iter().filter(|p| matches!(&p.function, PartFunction::Launcher(l) if l.kind == LauncherKind::DepthChargeRack)).collect();
    assert_eq!(racks.len(), 1, "one depth-charge rack");
    let rack = racks[0];
    let PartFunction::Launcher(l) = &rack.function else { unreachable!() };
    let charge = cat().munition(&l.munition).expect("its charge");
    assert_eq!((charge.kind, charge.phase), (MunitionKind::DepthCharge, Phase::Act3));
    assert!(l.cells >= 5, "a rack drops at least the gate's 5-charge pattern (L618)");
    assert!(rack.tier <= act3().start_tier, "a player can fit depth charges from the first day of the act");
    assert!(cat().kits.values().any(|k| k.parts.iter().any(|kp| kp.part == rack.id)), "a stock kit carries the rack");
    // The campaign offers them from Act 3, not before even with every tier open and the part unlocked; sandbox offers
    // them at once (L551).
    for p in guns.iter().copied().chain([rack]) {
        let idx = cat().part_idx(&p.id).expect("in the catalogue");
        let open = [p.id.clone()];
        assert!(!cat().available_parts(GameMode::Campaign, Act::Act2, Tier::T5, &open).contains(&idx), "{}", p.id);
        assert!(cat().available_parts(GameMode::Campaign, Act::Act3, Tier::T5, &open).contains(&idx), "{}", p.id);
        assert!(cat().available_parts(GameMode::Sandbox, Act::Act1, Tier::T1, &[]).contains(&idx), "{}", p.id);
    }
}

#[test]
fn act3_text_has_english_rows() {
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
    let mut need: Vec<String> = vec![act3().name.0.clone()];
    need.extend(ops().flat_map(|o| [o.name.0.clone(), o.briefing.0.clone()]));
    need.extend(c.classes.values().filter(|x| x.act == Act::Act3).map(|x| x.name.0.clone()));
    let parts: Vec<PartDef> = parse_ron(data::PARTS[2], "parts/act3.ron").unwrap();
    need.extend(parts.iter().flat_map(|p| [p.name.0.clone(), p.description.0.clone()]));
    for k in cat().kits.values().filter(|k| k.parts.iter().any(|kp| parts.iter().any(|p| p.id == kp.part))) {
        need.extend([format!("{}.name", k.id), format!("{}.desc", k.id)]);
    }
    let missing: Vec<&String> = need.iter().filter(|k| !keys.contains(*k)).collect();
    assert!(missing.is_empty(), "no English row for {missing:?}");
    // The Act 3 nodes the story names exist and are the right kinds.
    let kind = |id: &str| c.theatre.nodes.iter().find(|n| n.id.as_str() == id).map(|n| n.kind.clone()).unwrap();
    assert_eq!(kind("harbour.saltby"), NodeKind::Harbour);
    assert!(matches!(kind("yard.hollin"), NodeKind::Shipyard { .. }));
    assert!(matches!(kind("depot.vell"), NodeKind::Depot { .. }));
    assert_eq!(kind("sea.greywater"), NodeKind::Sea);
}
