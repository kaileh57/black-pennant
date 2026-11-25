//! Unit `data-act4`: the Act 4 campaign data (the Meridian Roads) loads through the real loader and holds the rules
//! the theatre, the operation generator, the act flow and Open Theatre rely on (DESIGN.md L747-782, L801-807,
//! L866-875, L94-102, L126, L1447).
use content::campaign::load::{load_embedded, stock_blueprint};
use content::campaign::{CampaignData, ClassRole, Complication, ConstraintTag, FleetKind, MissionType, NodeKind, Objective, OperationDef, OperationKind, Reward, Unlock};
use content::catalogue::Catalogue;
use content::data::{self, parse_ron};
use content::enums::{Act, Emcon, GameMode, NetworkKind, Phase, Stance, Tier};
use content::ids::{ClassId, FleetTemplateId, OperationId, PartId};
use content::part::weapons::{CalibreBand, MunitionKind};
use content::part::{PartDef, PartFunction, PartTag, PortRole, SensorKind};
use std::collections::BTreeSet;

/// The cap per player hull in Act 4 and the commitment multiples (L94, L747: 7,200 t in Act 4).
const CAP_T: f64 = 1800.0;
const MIN_HULLS: usize = 5;
const MIN_T: f64 = 4.0 * CAP_T;
/// Enemy aircraft one operation may carry (minimum swarm, L1447).
const MAX_AIRCRAFT: usize = 6;
/// The marker vocabulary every theatre node map carries (theatre.ron header): templates name only these, so they
/// play at any node of any region in Open Theatre.
const SHARED_MARKERS: [&str; 13] = ["spawn", "enemy", "enemy_2", "objective", "lane", "exit", "convoy_entry", "convoy_exit", "survivors", "berth", "landing", "depot", "radar"];

fn cat() -> &'static Catalogue {
    static CAT: std::sync::OnceLock<Catalogue> = std::sync::OnceLock::new();
    CAT.get_or_init(|| Catalogue::load_embedded().expect("embedded catalogue loads"))
}
fn data() -> &'static CampaignData {
    static DATA: std::sync::OnceLock<CampaignData> = std::sync::OnceLock::new();
    DATA.get_or_init(|| load_embedded(cat()).unwrap_or_else(|e| panic!("campaign data: {e}")))
}
fn act4() -> &'static content::campaign::ActDef { data().acts.iter().find(|a| a.act == Act::Act4).expect("an Act 4 row") }
fn ops() -> impl Iterator<Item = &'static OperationDef> { data().operations.values().filter(|o| o.act == Act::Act4) }
fn act4_parts() -> Vec<PartDef> { parse_ron(data::PARTS[3], "parts/act4.ron").unwrap() }

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
fn capital(class: &ClassId) -> bool { data().classes[class].capital }
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
/// Hulls over 90 m an operation can field at most: its force's capital slots at their maximum, plus an arriving
/// ace's ship.
fn capitals_in(o: &OperationDef) -> usize {
    let c = data();
    o.force.iter().map(|e| c.fleets[&e.fleet].slots.iter().filter(|s| s.classes.iter().any(capital)).map(|s| s.max as usize).sum::<usize>()).sum::<usize>()
        + o.complications.iter().filter(|x| matches!(x, Complication::AceArrives(a) if capital(&c.aces[a].ship))).count()
}
/// True when every way of meeting `u` completes `op` first.
fn requires(u: &Unlock, op: &str) -> bool {
    match u {
        Unlock::Completed(v) => v.iter().any(|o| o.as_str() == op),
        Unlock::All(v) => v.iter().any(|x| requires(x, op)),
        Unlock::Any(v) => !v.is_empty() && v.iter().all(|x| requires(x, op)),
        _ => false,
    }
}
/// Deepest point of a stock design below its design waterline, metres (keel or the lowest appendage).
fn draft_max_m(bp: &content::blueprint::Blueprint) -> f64 {
    let mut low = bp.cells.0.iter().map(|r| r.y as i32).min().unwrap_or(0);
    for p in &bp.parts { low = low.min(p.pos.y as i32); }
    (bp.hull.design_waterline_y as i32 - low).max(0) as f64 * content::cell::CELL_M
}
/// Markers an objective names.
fn objective_markers(o: &Objective) -> Vec<&str> {
    match o {
        Objective::DestroySite { marker } | Objective::Extract { marker, .. } | Objective::LayMines { marker, .. } => vec![marker.as_str()],
        Objective::Escort { exit, .. } | Objective::Survive { exit, .. } | Objective::CutOut { exit, .. } => vec![exit.as_str()],
        _ => vec![],
    }
}

#[test]
fn the_act4_row_opens_the_meridian_roads_under_the_1800_t_cap_and_then_open_theatre() {
    let c = data();
    let a = act4();
    assert_eq!(c.acts.iter().map(|a| a.act).collect::<Vec<_>>(), Act::ALL, "all four act rows, in order");
    assert_eq!((a.phase, a.start_tier), (Phase::Act4, Tier::T5), "every tier is open from the first day of Act 4 (L807)");
    assert!(a.tier_unlocks.is_empty(), "Act 4 adds parts to existing tiers, not a tier (L807)");
    assert_eq!(a.starters, ["starters/destroyer90.bp.ron"], "the 90 m destroyer (L871)");
    let bp = stock_blueprint(&a.starters[0]).unwrap();
    let st = content::stats::compute_stats(&bp, cat());
    assert!(st.tonnage_t <= Act::Act4.hull_cap_t() && st.tonnage_t > Act::Act3.hull_cap_t(), "{} t", st.tonnage_t);
    assert!(bp.parts.iter().all(|p| cat().part(&p.part).unwrap().tier <= a.start_tier));
    assert_eq!(a.story_ops.len(), 4, "four story operations (L871)");
    assert_eq!(a.ace_duels.len(), 1);
    assert_eq!(a.aces.len(), 1, "one ace arc in Act 4 (L774)");
    assert_eq!(a.regions.iter().map(|r| r.as_str()).collect::<Vec<_>>(), ["region.meridian"]);
    assert_eq!(a.coves.len(), 3, "a normal act opens 3 coves (L750)");
    let node = |id: &content::ids::TheatreNodeId| c.theatre.nodes.iter().find(|n| &n.id == id).unwrap();
    for cove in &a.coves {
        assert_eq!(node(cove).region.as_str(), "region.meridian", "{cove}");
        assert_eq!(node(cove).kind, NodeKind::Cove, "{cove}");
    }
    assert!(a.retreat_closed_cove.as_ref().is_some_and(|x| a.coves.contains(x)));
    // The final capstone opens Open Theatre in the same save (L873); no earlier act does.
    assert!(a.opens_open_theatre);
    assert!(c.acts.iter().filter(|x| x.act != Act::Act4).all(|x| !x.opens_open_theatre));
    for id in a.story_ops.iter().chain([&a.capstone]) {
        let n = c.operations[id].node.as_ref().unwrap_or_else(|| panic!("{id} has a node"));
        assert_eq!(node(n).region.as_str(), "region.meridian", "{id}");
    }
}

/// Act 4's eleven templates are also the Open Theatre set (L873): the generator plays the last act's templates in
/// every region, so each one must be placeable anywhere, from the first day, with groups that sail anywhere.
#[test]
fn act4_has_eleven_templates_that_play_in_every_region() {
    let c = data();
    let templates: Vec<&OperationDef> = ops().filter(|o| matches!(o.kind, OperationKind::Template(_))).collect();
    let kinds: BTreeSet<MissionType> = templates.iter().filter_map(|o| match o.kind { OperationKind::Template(t) => Some(t), _ => None }).collect();
    assert_eq!((templates.len(), kinds.len()), (11, 11), "one row per template (L778)");
    // No later act exists, so these are the templates of the last act that has any: the Open Theatre set.
    assert!(c.operations.values().all(|o| o.act <= Act::Act4));
    let own_act = |f: &FleetTemplateId| c.fleets[f].act == Act::Act4 || c.fleets[f].kind == FleetKind::AirPatrol;
    for o in &templates {
        assert_eq!(o.unlock, Unlock::Always, "{}: offered from the Act 4 start and in Open Theatre", o.id);
        assert!(o.node.is_none() && o.map.is_none(), "{}: placed by the generator, in any region", o.id);
        for e in &o.force {
            assert!(own_act(&e.fleet), "{}: {}", o.id, e.fleet);
            assert!(SHARED_MARKERS.contains(&e.marker.as_str()), "{}: marker {}", o.id, e.marker);
        }
        for m in objective_markers(&o.objective) { assert!(SHARED_MARKERS.contains(&m), "{}: marker {m}", o.id); }
        for x in &o.complications {
            if let Complication::Reinforcements { fleet, .. } = x { assert!(own_act(fleet), "{}", o.id); }
            assert!(!matches!(x, Complication::AceArrives(_)), "{}: an ace arrives only in authored operations", o.id);
        }
        assert_eq!(capitals_in(o), 0, "{}: no capital sails in a template", o.id);
    }
    // Every node map carries the shared vocabulary, so a template's markers exist wherever it is placed.
    for n in &c.theatre.nodes {
        for m in SHARED_MARKERS { assert!(n.map.markers.iter().any(|x| x.name == m), "{}: no {m} marker", n.id); }
    }
    assert_eq!(ops().filter(|o| o.kind == OperationKind::Capstone).count(), 1);
    assert_eq!(ops().filter(|o| o.kind == OperationKind::Story).count(), 4);
    assert_eq!(ops().filter(|o| matches!(o.kind, OperationKind::AceDuel(_))).count(), 1);
    // The repeat cap in Open Theatre counts per 40 theatre days (L873).
    assert_eq!(c.rules.open_theatre_repeat_window_days, 40);
}

/// Every Act 4 operation commits 5 hulls and 7,200 t on its surface force's worst draw, and every task group,
/// hunter-killer and the convoy escort does alone, since the generator draws reinforcement waves and co-op groups
/// from them (L747). Capitals stay within 4 an operation (L126).
#[test]
fn act4_forces_meet_the_commitment_floor_on_their_worst_draw() {
    let c = data();
    for (id, f) in c.fleets.iter().filter(|(_, f)| f.act == Act::Act4) {
        if matches!(f.kind, FleetKind::TaskGroup | FleetKind::HunterKiller) || id.as_str() == "fleet.act4.escort.convoy" {
            let (h, t) = worst(id);
            assert!(h >= MIN_HULLS && t >= MIN_T, "{id}: worst draw {h} hulls, {t:.0} t");
            // Waves and task groups never carry a capital: the generator would drop it at the capital limit.
            assert!(f.slots.iter().flat_map(|s| &s.classes).all(|cl| !capital(cl)), "{id}");
        }
        for s in &f.slots {
            assert!(s.classes.iter().all(|cl| !cl.as_str().starts_with("class.ace_")) || f.slots.len() == 1, "{id}: ace flagships sail alone");
            // A capital is alone in its slot, at most one of it, so a group never brings more than it names.
            if s.classes.iter().any(capital) { assert!(s.classes.len() == 1 && s.max == 1, "{id}"); }
        }
    }
    for o in ops() {
        let (h, t) = committed(o);
        match o.kind {
            OperationKind::Template(MissionType::HunterGroupSurvival) => assert!(h >= 1, "{}", o.id),
            _ => assert!(h >= MIN_HULLS && t >= MIN_T, "{}: {h} hulls, {t:.0} t", o.id),
        }
        assert!(capitals_in(o) <= c.rules.max_capitals as usize, "{}: {} capitals", o.id, capitals_in(o));
    }
    // The capitals do sail: the heavy cruiser and the Chairman's flagship each appear in authored operations.
    let named: BTreeSet<&str> = ops().flat_map(|o| &o.force).flat_map(|e| c.fleets[&e.fleet].slots.iter().flat_map(|s| &s.classes)).filter(|cl| capital(cl)).map(|cl| cl.as_str()).collect();
    assert_eq!(named, BTreeSet::from(["class.ace_charter", "class.principal"]));
}

/// Act 4 builds no boats: its submarine patrols sail Act 3's, beside a surface force, never as an objective (L875).
#[test]
fn act4_submarines_are_act3_boats_and_can_be_cut() {
    let c = data();
    assert!(c.classes.values().filter(|x| x.act == Act::Act4).all(|x| !x.submersible));
    for (id, f) in c.fleets.iter().filter(|(_, f)| f.act == Act::Act4) {
        assert_eq!(has_subs(id), f.kind == FleetKind::SubmarinePatrol, "{id}");
        if has_subs(id) { assert!(f.slots.iter().flat_map(|s| &s.classes).all(|cl| submersible(cl) && c.classes[cl].act == Act::Act3), "{id}"); }
    }
    let mut with_subs = 0;
    for o in ops() {
        let sub_tags: Vec<&str> = o.force.iter().filter(|e| has_subs(&e.fleet)).map(|e| e.tag.as_str()).collect();
        if sub_tags.is_empty() { continue; }
        with_subs += 1;
        let objective_tag = match &o.objective {
            Objective::DestroyGroup { tag, .. } | Objective::Capture { tag } | Objective::Escort { tag, .. } | Objective::Photograph { tag, .. } | Objective::CutOut { tag, .. } => Some(tag.as_str()),
            _ => None,
        };
        assert!(objective_tag.is_none_or(|t| !sub_tags.contains(&t)), "{}: a submarine is never the objective", o.id);
        assert!(sub_tags.iter().all(|t| o.force.iter().filter(|e| e.tag == *t).all(|e| has_subs(&e.fleet))), "{}: boats keep a tag of their own", o.id);
    }
    assert!(with_subs >= 4, "{with_subs} Act 4 operations meet submarines");
}

/// Act 4 brings no aircraft of its own; Act 2's patrols and raids stay inside the swarm budget (L1447).
#[test]
fn act4_aircraft_stay_inside_the_swarm_budget() {
    let c = data();
    assert!(c.fleets.values().filter(|f| f.act == Act::Act4).all(|f| f.bodies.is_empty()), "Act 4 adds no air groups");
    let first = c.fleets.iter().filter(|(_, f)| f.kind == FleetKind::AirPatrol).map(|(id, _)| id).min().unwrap();
    let patrol = aircraft_in(first);
    for o in ops() {
        let own: usize = o.force.iter().map(|e| aircraft_in(&e.fleet)).sum();
        let complication = if o.complications.contains(&Complication::AirPatrol) { patrol } else { 0 };
        assert!(own + complication + patrol <= MAX_AIRCRAFT, "{}: up to {} aircraft", o.id, own + complication + patrol);
    }
    assert!(c.operations[&act4().capstone].force.iter().any(|e| aircraft_in(&e.fleet) > 0), "the capstone brings a raid");
}

#[test]
fn act4_operations_are_well_formed() {
    let c = data();
    // Every tier is open from the first day of Act 4 and the dry dock offers every part of an open tier, so a
    // blueprint would unlock nothing: Act 4's ghosts pay Intel (L780), and the hardest pay the most.
    assert!(act4().start_tier == Tier::T5 && act4().tier_unlocks.is_empty());
    for o in ops() {
        for g in &o.ghosts { assert!(matches!(g.reward, Reward::Intel(1..=3)), "{}: {:?}", o.id, g.reward); }
    }
    let duel = ops().find(|o| matches!(o.kind, OperationKind::AceDuel(_))).unwrap();
    assert!(duel.ghosts.iter().any(|g| g.kind == content::campaign::GhostKind::CaptureTheAce && g.reward == Reward::Intel(3)));
    let starter = stock_blueprint(&act4().starters[0]).unwrap();
    let starter_draft = draft_max_m(&starter);
    assert!(starter_draft > 1.0 && starter_draft < 5.0, "{starter_draft} m");
    for o in ops() {
        let id = o.id.as_str();
        assert_eq!(o.name.as_str(), format!("{id}.name"));
        assert_eq!(o.briefing.as_str(), format!("{id}.briefing"));
        assert!(id.starts_with("op.act4."), "{id}");
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
        // Every tag can be met: the starter's draft clears every draft limit with a metre to spare, and every
        // special-operations part is a stock kit away for a refit from the tiers open in Act 4 (L803).
        for t in &o.tags {
            match t {
                ConstraintTag::DraftLimit { max_m } => assert!(*max_m >= starter_draft + 1.0, "{id}: the starter draws {starter_draft} m"),
                ConstraintTag::Requires(tag) => {
                    let kits = cat().kits_for_tag(*tag);
                    assert!(kits.iter().any(|k| k.parts.iter().all(|kp| cat().part(&kp.part).is_some_and(|d| d.tier <= act4().start_tier))), "{id}: no stock kit for {tag:?}");
                }
                ConstraintTag::Passengers { count, allowed_losses } => assert!(allowed_losses < count && *count <= 8, "{id}"),
                ConstraintTag::RcsLimit { max_m2 } => assert!((350.0..=500.0).contains(max_m2), "{id}: {max_m2} m^2"),
                ConstraintTag::Range { min_km } => assert!((400.0..=600.0).contains(min_km), "{id}: {min_km} km"),
                ConstraintTag::MinSpeed { kn } => assert!((24.0..=30.0).contains(kn), "{id}: {kn} kn"),
            }
        }
        if let OperationKind::AceDuel(ace) = &o.kind {
            let ship = &c.aces[ace].ship;
            let ace_entry = o.force.iter().find(|e| e.tag == "ace").expect("ace entry");
            assert_eq!(c.fleets[&ace_entry.fleet].slots[0].classes, vec![ship.clone()], "{id}");
            assert_eq!(o.objective, Objective::DestroyGroup { tag: "ace".into(), fraction: 1.0 });
            assert!(o.ghosts.iter().any(|g| g.kind == content::campaign::GhostKind::CaptureTheAce), "{id}");
        }
        for e in &o.force { assert!(c.fleets[&e.fleet].act <= Act::Act4, "{id}"); }
    }
    // Three of the story chain are gun and missile work the starter can do as built; the fourth asks for one
    // refit, the act's editor lesson.
    let refits = act4().story_ops.iter().filter(|s| c.operations[*s].tags.iter().any(|t| matches!(t, ConstraintTag::Requires(tag) if !starter.parts.iter().any(|p| cat().part(&p.part).unwrap().tags.contains(tag))))).count();
    assert_eq!(refits, 1);
}

/// The story operations unlock in order of play, each by progress or a theatre condition, and nothing in the act
/// can stall (L778).
#[test]
fn act4_story_unlocks_can_always_be_met() {
    let c = data();
    let act = act4();
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
    assert_eq!(c.operations[&act.story_ops[0]].unlock, Unlock::Always, "the act opens on its first story operation");
    fn has_condition(u: &Unlock) -> bool { match u { Unlock::SupplyBelow { .. } | Unlock::AlertAtLeast { .. } => true, Unlock::All(v) | Unlock::Any(v) => v.iter().any(has_condition), _ => false } }
    for s in &act.story_ops[1..] { assert!(has_condition(&c.operations[s].unlock), "{s} is unlocked by theatre conditions too"); }
    // Nothing in Act 4 waits on an earlier act's operation: a campaign can start in Act 4.
    fn names(u: &Unlock, out: &mut Vec<OperationId>) { match u { Unlock::Completed(v) => out.extend(v.iter().cloned()), Unlock::All(v) | Unlock::Any(v) => v.iter().for_each(|x| names(x, out)), _ => {} } }
    for o in ops() {
        let mut v = Vec::new();
        names(&o.unlock, &mut v);
        assert!(v.iter().all(|x| c.operations[x].act == Act::Act4), "{}", o.id);
    }
}

#[test]
fn the_chairman_sails_the_charter_and_arrives_before_his_duel() {
    let c = data();
    for id in &act4().aces {
        let a = &c.aces[id];
        let ship = &c.classes[&a.ship];
        assert_eq!(ship.act, Act::Act4, "{id}");
        assert!(a.ship.as_str().starts_with("class.ace_"), "{id}: a signature ship");
        assert!(ship.capital && !ship.submersible, "{id}: the Combine's flagship");
        let arc = a.arcs.iter().find(|x| x.act == Act::Act4).expect("an Act 4 arc");
        let duel = c.operations.values().find(|o| o.kind == OperationKind::AceDuel(id.clone())).expect("a duel");
        assert!(arc.operations.contains(&duel.id), "{id}: the duel is in the arc");
        assert!(arc.operations.contains(&act4().capstone), "{id}: the final capstone is in the arc");
        assert!(arc.operations.iter().all(|o| c.operations[o].act == Act::Act4), "{id}");
        // The ace arrives in a story operation the duel waits for.
        let arrives: Vec<&OperationId> = arc.operations.iter().filter(|o| c.operations[*o].kind == OperationKind::Story && c.operations[*o].complications.contains(&Complication::AceArrives(id.clone()))).collect();
        assert!(arrives.iter().any(|o| requires(&duel.unlock, o.as_str())), "{id}");
        // Every operation he arrives in is in his arc.
        for o in ops().filter(|o| o.complications.contains(&Complication::AceArrives(id.clone()))) { assert!(arc.operations.contains(&o.id), "{}", o.id); }
    }
    // Every ace of every act now sails a signature ship of that act.
    for a in c.aces.values() {
        let act = a.arcs[0].act;
        assert_eq!(c.classes[&a.ship].act, act, "{}", a.id);
        assert!(a.arcs.iter().all(|x| !x.operations.is_empty()), "{}", a.id);
    }
}

/// The new classes field in the Roads beside the earlier hulls still in service; the hunter group's top tier is the
/// Consol destroyer, a pooled hull of 90 m or under whose five-hull group meets the act's floor on its own.
#[test]
fn the_meridian_roads_field_the_act4_classes() {
    let c = data();
    let r = c.theatre.regions.iter().find(|r| r.id.as_str() == "region.meridian").unwrap();
    assert_eq!(r.act, Act::Act4);
    let fielded: BTreeSet<&str> = r.order_of_battle.iter().map(|(cl, _)| cl.as_str()).chain(r.civilian.iter().map(|cl| cl.as_str())).collect();
    for cl in c.classes.values().filter(|x| x.act == Act::Act4 && !x.id.as_str().starts_with("class.ace_") && !x.capital) {
        assert!(fielded.contains(cl.id.as_str()), "{} is not in the Roads' order of battle", cl.id);
    }
    assert!(r.civilian.iter().any(|cl| c.classes[cl].act == Act::Act4));
    assert!(r.order_of_battle.iter().all(|(cl, _)| !capital(cl) && !cl.as_str().starts_with("class.ace_")), "aces and capitals are not pooled hulls");
    // About as many records as each earlier region (the Normal total is about 300, L756).
    let total: u32 = r.order_of_battle.iter().map(|(_, n)| *n as u32).sum();
    assert!((60..=90).contains(&total), "{total} hull records");
    // Theatre alert takes the first top-tier surface combatant of the act for the hunter group (L749).
    let surface = |x: &&content::campaign::EnemyClassDef| x.act == Act::Act4 && combat(&x.id) && !x.submersible;
    let top = c.classes.values().filter(surface).map(|x| x.tier).max().unwrap();
    let hunter = c.classes.values().filter(surface).find(|x| x.tier == top).unwrap();
    assert_eq!(hunter.id.as_str(), "class.consol");
    assert!(!hunter.capital);
    assert!(5.0 * tonnage(&hunter.id) >= MIN_T, "a hunter group of five meets 7,200 t");
    // Act 4 hulls are bigger than Act 3's: its fleets meet 4 x 1,800 t in five hulls.
    let heavy = c.classes.values().filter(|x| x.act == Act::Act4 && combat(&x.id) && !x.capital && tonnage(&x.id) >= 1_400.0).count();
    assert!(heavy >= 6, "{heavy} Act 4 combat classes of 1,400 t or more");
    // Variants are heavier hulls of the same role (L834).
    for cl in c.classes.values().filter(|x| x.act == Act::Act4) {
        for v in &cl.variants {
            assert_eq!(c.classes[v].role, cl.role);
            assert!(tonnage(v) > tonnage(&cl.id), "{} -> {v}", cl.id);
        }
    }
}

/// Act 4 adds parts in existing tiers (L807): rows in parts/act4.ron, well formed like the first release's, offered
/// in the campaign from Act 4 and in sandbox at once, each with its stock kit where one belongs.
#[test]
fn act4_parts_are_added_to_existing_tiers() {
    let parts = act4_parts();
    assert!((4..=8).contains(&parts.len()), "{} Act 4 parts", parts.len());
    for p in &parts {
        let id = p.id.as_str();
        assert_eq!(p.phase, Phase::Act4, "{id}");
        assert!(p.tier <= Tier::T5 && p.tier <= act4().start_tier, "{id}: an existing tier, open from the act start");
        assert_eq!(p.name.0, format!("part.{id}.name"));
        assert_eq!(p.description.0, format!("part.{id}.desc"));
        assert!(p.mass_kg.is_finite() && p.mass_kg > 0.0 && p.cost > 0, "{id}");
        let cells: usize = p.size.iter().map(|&s| s as usize).product();
        let density = p.mass_kg / (cells as f64 * 0.125);
        assert!((25.0..=7_850.0).contains(&density), "{id}: {density:.0} kg/m^3");
        assert!(p.rcs_factor > 0.0 && p.rcs_factor <= 1.0, "{id}");
        let consumes = p.ports.iter().any(|q| q.network == NetworkKind::Power && q.role == PortRole::Consume);
        assert_eq!(consumes, p.power_priority != 0, "{id}");
        match &p.function {
            PartFunction::Gun(g) => {
                assert_eq!(g.band, CalibreBand::Heavy, "{id}");
                assert!(cat().parts.values().any(|m| matches!(m.function, PartFunction::Magazine { calibre_mm, .. } if calibre_mm == g.calibre_mm)), "{id}: a magazine feeds it");
                assert_eq!(p.power_priority, 3, "{id}");
            }
            PartFunction::Launcher(l) => {
                assert!(cat().munition(&l.munition).is_some_and(|m| m.phase <= Phase::Act4), "{id}: an existing munition");
                assert_eq!(p.power_priority, 3, "{id}");
            }
            PartFunction::Sensor(s) => { assert_eq!(s.kind, SensorKind::SearchRadar, "{id}"); assert_eq!(p.power_priority, 4, "{id}"); }
            PartFunction::Engine(e) => {
                assert!(p.tags.contains(&PartTag::Propulsion));
                let fuel = p.ports.iter().find(|q| q.network == NetworkKind::Fuel).map_or(0.0, |q| q.rate);
                assert!((fuel - e.fuel_lps).abs() < 1e-9, "{id}");
                let g_per_kwh = e.fuel_lps * 0.84 * 3_600_000.0 / e.shaft_kw;
                assert!((190.0..=250.0).contains(&g_per_kwh), "{id}: {g_per_kwh:.0} g/kWh");
            }
            PartFunction::Generator { kw, fuel_lps } => {
                assert!(p.ports.iter().any(|q| q.network == NetworkKind::Power && q.role == PortRole::Supply && (q.rate - kw).abs() < 1e-9));
                let g_per_kwh = fuel_lps * 0.84 * 3_600_000.0 / kw;
                assert!((190.0..=260.0).contains(&g_per_kwh), "{id}: {g_per_kwh:.0} g/kWh");
            }
            f => panic!("{id}: unexpected function {f:?}"),
        }
        if !matches!(p.function, PartFunction::Generator { .. }) {
            assert!(cat().kits.values().any(|k| k.parts.iter().any(|kp| kp.part == p.id)), "{id}: a stock kit carries it");
        }
        // The campaign offers them from Act 4, not before even with every tier open and the part unlocked;
        // sandbox offers them at once (L807).
        let idx = cat().part_idx(&p.id).expect("in the catalogue");
        let open = [p.id.clone()];
        assert!(!cat().available_parts(GameMode::Campaign, Act::Act3, Tier::T5, &open).contains(&idx), "{id}");
        assert!(cat().available_parts(GameMode::Campaign, Act::Act4, Tier::T5, &open).contains(&idx), "{id}");
        assert!(cat().available_parts(GameMode::Sandbox, Act::Act1, Tier::T1, &[]).contains(&idx), "{id}");
    }
    // The twin turret outweighs the single it doubles and fires twice as fast for less than twice the weight.
    let gun = |id: &str| cat().part(&PartId::new(id)).unwrap();
    let (single, twin) = (gun("gun.130mm.single"), gun("gun.130mm.twin"));
    let rate = |p: &PartDef| match &p.function { PartFunction::Gun(g) => g.rof_rpm * g.barrels as f64, _ => 0.0 };
    assert!(twin.mass_kg > single.mass_kg && twin.mass_kg < 2.0 * single.mass_kg && twin.cost > single.cost);
    assert!(rate(twin) > 1.5 * rate(single));
}

#[test]
fn act4_text_has_english_rows() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../game/i18n");
    let mut keys = BTreeSet::new();
    // Keys are unique across the files this unit writes rows in (other units own the rest of game/i18n).
    let own = ["operations_act4.csv", "enemy.csv", "parts.csv", "kits.csv", "theatre.csv"];
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        if e.path().extension().is_some_and(|x| x == "csv") {
            let mine = e.path().file_name().and_then(|n| n.to_str()).is_some_and(|n| own.contains(&n));
            for line in std::fs::read_to_string(e.path()).unwrap().lines().skip(1) {
                if let Some((k, _)) = line.split_once(',') { assert!(keys.insert(k.to_string()) || !mine, "{k} is defined twice"); }
            }
        }
    }
    let c = data();
    let mut need: Vec<String> = vec![act4().name.0.clone()];
    need.extend(ops().flat_map(|o| [o.name.0.clone(), o.briefing.0.clone()]));
    need.extend(c.classes.values().filter(|x| x.act == Act::Act4).map(|x| x.name.0.clone()));
    let parts = act4_parts();
    need.extend(parts.iter().flat_map(|p| [p.name.0.clone(), p.description.0.clone()]));
    for k in cat().kits.values().filter(|k| k.parts.iter().any(|kp| parts.iter().any(|p| p.id == kp.part))) {
        need.extend([format!("{}.name", k.id), format!("{}.desc", k.id)]);
    }
    let missing: Vec<&String> = need.iter().filter(|k| !keys.contains(*k)).collect();
    assert!(missing.is_empty(), "no English row for {missing:?}");
    // The Act 4 nodes the story names exist and are the right kinds.
    let kind = |id: &str| c.theatre.nodes.iter().find(|n| n.id.as_str() == id).map(|n| n.kind.clone()).unwrap();
    assert!(matches!(kind("depot.orrin"), NodeKind::Depot { .. }));
    assert_eq!(kind("esm.halden"), NodeKind::EsmSite);
    assert_eq!(kind("harbour.castor"), NodeKind::Harbour);
    assert_eq!(kind("sea.sentinel"), NodeKind::Sea);
    assert!(matches!(kind("yard.meridian"), NodeKind::Shipyard { .. }));
}
