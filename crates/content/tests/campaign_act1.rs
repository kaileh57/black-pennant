//! Units `data-theatre-acts`, `data-enemy-act1`, `data-operations-act1` and `data-aces-cards`: the Act 1 campaign
//! data loads through the real loader and holds the rules the theatre, the operation generator and the doctrine
//! bulletin rely on (DESIGN.md L735-807, L85-102, L455, L866-875).
use content::campaign::load::{load_embedded, stock_blueprint};
use content::campaign::{
    AceCue, CampaignData, ClassRole, ConstraintTag, FleetKind, KillMethod, MapDef, MarkerKind, MissionType, NodeKind, Objective, OperationDef, OperationKind,
    RangeBand, TallyKey, Unlock,
};
use content::catalogue::Catalogue;
use content::enums::{Act, Emcon, Stance, Tier, TimeOfDay};
use content::ids::{FleetTemplateId, OperationId};
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

fn cat() -> &'static Catalogue {
    static CAT: std::sync::OnceLock<Catalogue> = std::sync::OnceLock::new();
    CAT.get_or_init(|| Catalogue::load_embedded().expect("embedded catalogue loads"))
}
fn data() -> &'static CampaignData {
    static DATA: std::sync::OnceLock<CampaignData> = std::sync::OnceLock::new();
    DATA.get_or_init(|| load_embedded(cat()).unwrap_or_else(|e| panic!("campaign data: {e}")))
}

/// Full-load tonnage of a class, from its stock blueprint.
fn tonnage(class: &content::ids::ClassId) -> f64 {
    let c = &data().classes[class];
    content::stats::compute_stats(&stock_blueprint(&c.blueprint).unwrap(), cat()).tonnage_t
}
fn combat(class: &content::ids::ClassId) -> bool {
    !matches!(data().classes[class].role, ClassRole::Freighter | ClassRole::Tanker | ClassRole::Tender | ClassRole::Civilian)
}
/// Worst draw of a fleet: (combat hulls, combat tonnage) taking each slot's minimum count of its lightest class.
fn worst(fleet: &FleetTemplateId) -> (usize, f64) {
    let f = &data().fleets[fleet];
    let (mut h, mut t) = (0, 0.0);
    for s in &f.slots {
        let light = s.classes.iter().filter(|c| combat(c)).map(|c| tonnage(c)).fold(f64::INFINITY, f64::min);
        if light.is_finite() && s.classes.iter().all(|c| combat(c)) {
            h += s.min as usize;
            t += s.min as f64 * light;
        }
    }
    (h, t)
}

#[test]
fn campaign_data_loads_with_act1_complete() {
    let c = data();
    assert!(!c.acts.is_empty(), "Act 1 ships first; Acts 2 to 4 add their rows with their content");
    let a = &c.acts[0];
    assert_eq!(a.act, Act::Act1);
    assert_eq!(a.start_tier, Tier::T1);
    let opened: BTreeSet<Tier> = a.tier_unlocks.iter().map(|(_, t)| *t).collect();
    assert_eq!(opened, BTreeSet::from([Tier::T2, Tier::T3]), "tiers 2 and 3 open within Act 1 (L805)");
    assert_eq!(a.story_ops.len(), 4, "four story operations (L778)");
    assert_eq!(a.ace_duels.len(), 2);
    assert_eq!(a.aces.len(), 2, "two ace arcs in Act 1 (L774)");
    assert_eq!(a.coves.len(), 3, "a normal act opens 3 coves (L750)");
    assert!(a.retreat_closed_cove.is_some());

    let act1: Vec<&OperationDef> = c.operations.values().filter(|o| o.act == Act::Act1).collect();
    let templates: BTreeSet<MissionType> = act1.iter().filter_map(|o| match o.kind { OperationKind::Template(t) => Some(t), _ => None }).collect();
    assert_eq!(templates.len(), 11, "all eleven templates (L778)");
    assert_eq!(act1.iter().filter(|o| matches!(o.kind, OperationKind::Template(_))).count(), 11, "one row per template");
    assert_eq!(act1.iter().filter(|o| o.kind == OperationKind::Capstone).count(), 1);
    assert_eq!(act1.iter().filter(|o| o.kind == OperationKind::Story).count(), 4);
    assert_eq!(c.aces.len(), 7, "seven aces (L774)");
    assert!((28..=34).contains(&c.cards.len()), "about 30 doctrine cards (L772)");
}

// ---------------------------------------------------------------------------------------------------------
// Theatre

#[test]
fn the_chart_has_four_regions_of_twelve_nodes() {
    let th = &data().theatre;
    assert_eq!(th.size_km, [600.0, 400.0]);
    assert_eq!(th.regions.len(), 4);
    assert_eq!(th.nodes.len(), 48, "48 nodes (L741)");
    let acts: Vec<Act> = th.regions.iter().map(|r| r.act).collect();
    assert_eq!(acts, Act::ALL.to_vec(), "one region opens per act");
    for r in &th.regions {
        let nodes: Vec<_> = th.nodes.iter().filter(|n| n.region == r.id).collect();
        assert_eq!(nodes.len(), 12, "{}", r.id);
        let has = |p: &dyn Fn(&NodeKind) -> bool| nodes.iter().filter(|n| p(&n.kind)).count();
        assert_eq!(has(&|k| matches!(k, NodeKind::Cove)), 3, "{} coves", r.id);
        assert!(has(&|k| matches!(k, NodeKind::Shipyard { .. })) >= 1, "{} shipyard", r.id);
        assert!(has(&|k| matches!(k, NodeKind::Depot { .. })) >= 1, "{} depot", r.id);
        assert!(has(&|k| matches!(k, NodeKind::ForwardBase)) >= 1, "{} forward base", r.id);
        assert!(has(&|k| matches!(k, NodeKind::Harbour)) >= 1, "{} harbour", r.id);
        assert!(has(&|k| matches!(k, NodeKind::RadarSite | NodeKind::EsmSite)) >= 1, "{} radar or ESM site", r.id);
        assert!((30.0..=80.0).contains(&r.layer_depth_m), "{} layer depth (L455)", r.id);
        assert!(r.base_patrols > 0 && !r.weather.is_empty() && !r.order_of_battle.is_empty() && !r.civilian.is_empty());
        let w: f64 = r.weather.iter().map(|(_, w)| *w).sum();
        assert!(w > 0.0 && r.weather.iter().all(|(_, w)| w.is_finite() && *w >= 0.0));
        for n in &nodes {
            assert!(n.pos_km[0] >= 0.0 && n.pos_km[0] <= 600.0 && n.pos_km[1] >= 0.0 && n.pos_km[1] <= 400.0, "{} off the chart", n.id);
        }
    }
    // Roughly 300 Combine hull records at Normal (L756).
    let total: u32 = th.regions.iter().flat_map(|r| &r.order_of_battle).map(|(_, n)| *n as u32).sum();
    assert!((280..=330).contains(&total), "{total} hull records");
    // Convoys need freighters or tankers in every region's order of battle (3 per sailing).
    for r in &th.regions {
        let merchants: u16 = r.order_of_battle.iter().filter(|(c, _)| matches!(data().classes[c].role, ClassRole::Freighter | ClassRole::Tanker)).map(|(_, n)| *n).sum();
        assert!(merchants >= 9, "{}: {merchants} merchant hulls", r.id);
    }
}

fn graph() -> (BTreeMap<&'static str, [f64; 2]>, Vec<(&'static str, &'static str, f64)>) {
    let th = &data().theatre;
    let pos = th.nodes.iter().map(|n| (n.id.as_str(), n.pos_km)).collect();
    let lanes = th.lanes.iter().map(|l| (l.a.as_str(), l.b.as_str(), l.traffic)).collect();
    (pos, lanes)
}

/// Shortest lane distance in km from `from` to every node, skipping `cut` lanes.
fn dist(from: &str, cut: &[(&str, &str)]) -> BTreeMap<&'static str, f64> {
    let (pos, lanes) = graph();
    let km = |a: &str, b: &str| { let (p, q) = (pos[a], pos[b]); libm::hypot(p[0] - q[0], p[1] - q[1]) };
    let mut d: BTreeMap<&str, f64> = BTreeMap::new();
    let mut heap = BinaryHeap::new();
    d.insert(pos.keys().find(|k| **k == from).copied().unwrap(), 0.0);
    heap.push((std::cmp::Reverse(0u64), from.to_string()));
    while let Some((std::cmp::Reverse(dm), x)) = heap.pop() {
        let dx = dm as f64 / 1000.0;
        if dx > d.get(x.as_str()).copied().unwrap_or(f64::INFINITY) + 1e-3 { continue; }
        for &(a, b, _) in &lanes {
            if cut.iter().any(|&(p, q)| (p == a && q == b) || (p == b && q == a)) { continue; }
            let y = if a == x { b } else if b == x { a } else { continue };
            let nd = dx + km(a, b);
            if nd + 1e-9 < d.get(y).copied().unwrap_or(f64::INFINITY) {
                d.insert(y, nd);
                heap.push((std::cmp::Reverse((nd * 1000.0) as u64), y.to_string()));
            }
        }
    }
    d
}

#[test]
fn lanes_connect_the_chart_and_coves_stay_off_the_shipping_lanes() {
    let th = &data().theatre;
    let all = dist(th.nodes[0].id.as_str(), &[]);
    assert_eq!(all.len(), 48, "every node reachable");
    for n in th.nodes.iter().filter(|n| matches!(n.kind, NodeKind::Cove)) {
        let lanes: Vec<_> = th.lanes.iter().filter(|l| l.a == n.id || l.b == n.id).collect();
        assert_eq!(lanes.len(), 1, "{} is a leaf, so no convoy routes through it", n.id);
        assert_eq!(lanes[0].traffic, 0.0, "{} carries no Combine traffic", n.id);
    }
    // Templates that need trafficked sea nodes (convoy raid, prize capture, minelaying) can be placed in Act 1.
    let traffic = |id: &content::ids::TheatreNodeId| th.lanes.iter().any(|l| (l.a == *id || l.b == *id) && l.traffic > 0.0);
    assert!(th.nodes.iter().any(|n| n.region.as_str() == "region.gannet" && matches!(n.kind, NodeKind::Sea) && traffic(&n.id)));
}

/// Each region's first sea node is its chokepoint: cut the shipyard's lane into it and the north depot's convoys
/// face a detour over 1.5x (cancelled), the south depot's under it (rerouted), so one raid cuts Supply to about
/// 40-45 and the Act 1 Supply unlocks are reachable (L747-748; logistics DETOUR_LIMIT).
#[test]
fn every_region_has_a_chokepoint_one_raid_can_close() {
    let th = &data().theatre;
    let all_yards: Vec<&str> = th.nodes.iter().filter(|n| matches!(n.kind, NodeKind::Shipyard { .. })).map(|n| n.id.as_str()).collect();
    let from_yard: BTreeMap<&str, BTreeMap<&str, f64>> = all_yards.iter().map(|y| (*y, dist(y, &[]))).collect();
    for r in &th.regions {
        let of = |p: &dyn Fn(&NodeKind) -> bool| -> Vec<&str> { th.nodes.iter().filter(|n| n.region == r.id && p(&n.kind)).map(|n| n.id.as_str()).collect() };
        let yard = of(&|k| matches!(k, NodeKind::Shipyard { .. }))[0];
        let depots = of(&|k| matches!(k, NodeKind::Depot { .. }));
        let gap = of(&|k| matches!(k, NodeKind::Sea))[0];
        let base = &from_yard[yard];
        let cut = dist(yard, &[(yard, gap)]);
        let ratios: Vec<f64> = depots.iter().map(|d| cut.get(d).copied().unwrap_or(f64::INFINITY) / base[d]).collect();
        assert!(ratios.iter().any(|&x| x > 1.5), "{}: a cut at {gap} must cancel a depot's convoys ({ratios:?})", r.id);
        assert!(ratios.iter().any(|&x| x <= 1.5), "{}: one depot keeps a longer route ({ratios:?})", r.id);
        // Every depot's nearest yard is its own region's.
        for d in &depots {
            let home = all_yards.iter().min_by(|a, b| from_yard[*a][d].total_cmp(&from_yard[*b][d])).unwrap();
            assert_eq!(*home, yard, "{d} must be supplied from its own region's yard");
        }
    }
}

/// Same shoreline bound as sim-core terrain: shores lie within 0.84 to 1.16 of an island's radius, and a shelf of
/// max(0.5 r, 300 m) falls to open depth beyond.
const SHORE_MAX: f64 = 1.16;
fn shelf(r: f64) -> f64 { (0.5 * r).max(300.0) }

fn check_map(map: &MapDef, what: &str) {
    assert!(map.size_m[0] > 0.0 && map.size_m[0] <= content::campaign::MAX_MAP_M && map.size_m[1] > 0.0 && map.size_m[1] <= content::campaign::MAX_MAP_M, "{what} size");
    assert!(map.depth_m > 10.0, "{what} depth");
    let near_shore = |k: MarkerKind| matches!(k, MarkerKind::ShoreLanding | MarkerKind::Berth | MarkerKind::DepotSite | MarkerKind::RadarSite);
    for m in &map.markers {
        assert!(m.pos_m[0].abs() <= map.size_m[0] / 2.0 - 1000.0 && m.pos_m[1].abs() <= map.size_m[1] / 2.0 - 1000.0, "{what}: {} near the edge", m.name);
        let mut nearest = f64::INFINITY;
        for i in &map.islands {
            let d = libm::hypot(m.pos_m[0] - i.centre_m[0], m.pos_m[1] - i.centre_m[1]);
            let shore = if i.height_m > 0.0 { SHORE_MAX * i.radius_m } else { i.radius_m };
            assert!(d > shore, "{what}: {} is on land", m.name);
            nearest = nearest.min(d - shore - shelf(i.radius_m));
        }
        if !near_shore(m.kind) {
            assert!(nearest >= 0.0, "{what}: {} sits on a shelf", m.name);
        }
    }
    let names: BTreeSet<&str> = map.markers.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names.len(), map.markers.len(), "{what}: marker names unique");
}

#[test]
fn every_sortie_map_keeps_its_markers_in_open_water() {
    let th = &data().theatre;
    let vocabulary = ["spawn", "enemy", "enemy_2", "objective", "lane", "exit", "convoy_entry", "convoy_exit", "survivors", "berth", "landing", "depot", "radar"];
    for n in &th.nodes {
        check_map(&n.map, n.id.as_str());
        for v in vocabulary { assert!(n.map.markers.iter().any(|m| m.name == v), "{}: no {v} marker", n.id); }
        assert!(n.map.markers.iter().any(|m| m.kind == MarkerKind::PlayerSpawn));
    }
    check_map(&th.basin, "basin");
    for o in data().operations.values() {
        if let Some(m) = &o.map { check_map(m, o.id.as_str()); }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Enemy and fleets

/// Task groups are 5 hulls of at least 600 t (L747); every task group and hunter-killer meets that on its worst
/// draw, since the generator reuses them as reinforcement waves.
#[test]
fn task_groups_meet_the_commitment_floor_on_their_worst_draw() {
    let c = data();
    for (id, f) in c.fleets.iter().filter(|(_, f)| f.act == Act::Act1) {
        if matches!(f.kind, FleetKind::TaskGroup | FleetKind::HunterKiller) {
            let (h, t) = worst(id);
            assert!(h >= 5 && t >= 600.0, "{id}: worst draw {h} hulls, {t:.0} t");
        }
        assert!(f.kind != FleetKind::AirPatrol, "{id}: Act 1 has no aircraft");
        for s in &f.slots {
            assert!(s.classes.iter().all(|cl| c.classes[cl].act == Act::Act1), "{id}");
            assert!(s.classes.iter().all(|cl| !cl.as_str().starts_with("class.ace_")) || f.slots.len() == 1, "{id}: ace flagships sail alone");
        }
    }
    // The generator's add-by-kind picks the first fleet of a kind by sorted id: the generic escort and decoy.
    let first = |k: FleetKind| c.fleets.iter().filter(|(_, f)| f.kind == k).map(|(id, _)| id.as_str()).min().unwrap();
    assert_eq!(first(FleetKind::Escort), "fleet.act1.escort");
    assert_eq!(first(FleetKind::DecoyConvoy), "fleet.act1.decoy");
    // The hunter group fills with the first top-tier combat class of the act: the frigate.
    let top = c.classes.values().filter(|x| x.act == Act::Act1 && combat(&x.id)).map(|x| x.tier).max().unwrap();
    assert_eq!(c.classes.values().find(|x| x.tier == top && x.act == Act::Act1 && combat(&x.id)).unwrap().id.as_str(), "class.frigate");
}

#[test]
fn variants_are_heavier_hulls_of_the_same_role() {
    let c = data();
    let mut any = false;
    for cl in c.classes.values() {
        for v in &cl.variants {
            any = true;
            assert_eq!(c.classes[v].role, cl.role, "{} -> {v}", cl.id);
            assert!(tonnage(v) > tonnage(&cl.id), "{} -> {v} is not heavier", cl.id);
        }
    }
    assert!(any, "co-op scaling and the use-variants cards need variants (L834)");
}

// ---------------------------------------------------------------------------------------------------------
// Operations

/// Hostile combat hulls and tonnage an operation's force commits on its worst draw (L94): neutral entries,
/// protected (escorted) tags and non-combat roles do not count.
fn committed(o: &OperationDef) -> (usize, f64) {
    let protected = match &o.objective { Objective::Escort { tag, .. } => Some(tag.as_str()), _ => None };
    let (mut h, mut t) = (0, 0.0);
    for e in &o.force {
        if e.stance == Stance::Neutral || Some(e.tag.as_str()) == protected { continue; }
        let (a, b) = worst(&e.fleet);
        h += a;
        t += b;
    }
    (h, t)
}

#[test]
fn every_act1_operation_commits_five_hulls_and_600_t() {
    for o in data().operations.values().filter(|o| o.act == Act::Act1) {
        let (h, t) = committed(o);
        match o.kind {
            // The theatre's hunter group is the threat; standalone, the generator's waves make up the rest.
            OperationKind::Template(MissionType::HunterGroupSurvival) | OperationKind::Tutorial => assert!(h >= 1, "{}", o.id),
            _ => assert!(h >= 5 && t >= 600.0, "{}: {h} hulls, {t:.0} t", o.id),
        }
    }
}

#[test]
fn operations_are_well_formed() {
    let c = data();
    for o in c.operations.values().filter(|o| o.act == Act::Act1) {
        let id = o.id.as_str();
        assert_eq!(o.name.as_str(), format!("{id}.name"));
        assert_eq!(o.briefing.as_str(), format!("{id}.briefing"));
        if let Objective::DestroyGroup { fraction, .. } = &o.objective { assert!((0.5..=1.0).contains(fraction), "{id}"); }
        assert!((4_000.0..=20_000.0).contains(&o.approach_m), "{id}: approach");
        // Authored operations sit at a node (or bring a map); templates and duels are placed by the generator.
        match o.kind {
            OperationKind::Template(_) | OperationKind::AceDuel(_) => assert!(o.node.is_none() && o.map.is_none(), "{id}"),
            OperationKind::Tutorial => assert!(o.map.is_some(), "{id}"),
            _ => assert!(o.node.is_some(), "{id}"),
        }
        // Station actions match the objective (L798).
        use content::campaign::StationAction as A;
        let need = match &o.objective {
            Objective::Extract { .. } => Some(A::InsertOrExtract),
            Objective::LayMines { .. } => Some(A::LayMines),
            Objective::Capture { .. } | Objective::CutOut { .. } => Some(A::BoardAndCapture),
            Objective::Photograph { .. } => Some(A::Photograph),
            _ => None,
        };
        if let Some(a) = need { assert!(o.actions.contains(&a), "{id} needs {a:?}"); }
        // Ghost conditions that forbid emissions come with a recommended setting (L507). The generator infers
        // Silent for the briefing from the condition itself, so the authored value only has to agree with it.
        if o.ghosts.iter().any(|g| g.kind == content::campaign::GhostKind::NoRadarEmissions) {
            assert!(matches!(o.recommended_emcon, None | Some(Emcon::Silent)), "{id}: the authored setting contradicts the ghost condition");
        }
        // The authored setting is the one the player hull sails in at. An objective that needs gunfire is never
        // authored Silent: the stock Gunnery officer fires only on a track graded Fine (L484), which a silent
        // hull does not get before visual range (the L317 gate, decision of 22 November 2026).
        if matches!(o.objective, Objective::DestroyAll | Objective::DestroyGroup { .. } | Objective::DestroySite { .. } | Objective::Capture { .. } | Objective::CutOut { .. }) {
            assert_ne!(o.recommended_emcon, Some(Emcon::Silent), "{id}: a gunfire objective sails in at the design's own setting");
        }
        // An ace duel's objective is the ace's own group.
        if let OperationKind::AceDuel(ace) = &o.kind {
            let ship = &c.aces[ace].ship;
            let ace_entry = o.force.iter().find(|e| e.tag == "ace").expect("ace entry");
            assert_eq!(c.fleets[&ace_entry.fleet].slots[0].classes, vec![ship.clone()], "{id}");
            assert_eq!(o.objective, Objective::DestroyGroup { tag: "ace".into(), fraction: 1.0 });
        }
    }
}

fn unlock_requires(u: &Unlock, op: &OperationId) -> bool {
    match u {
        Unlock::Completed(v) => v.contains(op),
        Unlock::All(v) => v.iter().any(|x| unlock_requires(x, op)),
        _ => false,
    }
}

/// An operation whose tags require a part from a tier that opens inside the act waits for the operation that
/// opens it, so no briefing asks for a part the player cannot fit yet (L805).
#[test]
fn operations_wait_for_the_tier_their_tags_require() {
    let c = data();
    let act = &c.acts[0];
    let tier_of = |tag: &content::part::PartTag| cat().parts.values().filter(|p| p.tags.contains(tag)).map(|p| p.tier).min();
    for o in c.operations.values().filter(|o| o.act == Act::Act1) {
        let mut need: Vec<Tier> = Vec::new();
        for t in &o.tags {
            match t {
                ConstraintTag::Requires(tag) => need.push(tier_of(tag).unwrap_or_else(|| panic!("{}: no part carries {tag:?}", o.id))),
                ConstraintTag::Passengers { .. } => need.push(tier_of(&content::part::PartTag::PassengerBerths).unwrap()),
                _ => {}
            }
        }
        for tier in need.into_iter().filter(|t| *t > act.start_tier) {
            let ok = act.tier_unlocks.iter().any(|(op, t)| *t >= tier && unlock_requires(&o.unlock, op));
            assert!(ok, "{} needs a tier {tier:?} part before that tier opens", o.id);
        }
    }
}

/// The story operations unlock in order of play, each by progress or a theatre condition, and nothing in the act
/// can stall: in completion order every story operation, duel and the capstone becomes reachable.
#[test]
fn story_unlocks_can_always_be_met() {
    let c = data();
    let act = &c.acts[0];
    fn met(u: &Unlock, done: &BTreeSet<OperationId>, story: usize) -> bool {
        match u {
            Unlock::Always => true,
            Unlock::Completed(v) => v.iter().all(|o| done.contains(o)),
            Unlock::StoryCount(n) => story >= *n as usize,
            Unlock::All(v) => v.iter().all(|x| met(x, done, story)),
            Unlock::Any(v) => v.iter().any(|x| met(x, done, story)),
            // Theatre conditions are alternatives, never the only path.
            Unlock::SupplyBelow { .. } | Unlock::AlertAtLeast { .. } | Unlock::AceBeaten(_) => false,
        }
    }
    let mut done = BTreeSet::new();
    let mut story = 0;
    let order: Vec<OperationId> = act.story_ops.iter().chain(&act.ace_duels).cloned().chain([act.capstone.clone()]).collect();
    for _ in 0..order.len() {
        let next = order.iter().find(|o| !done.contains(*o) && met(&c.operations[*o].unlock, &done, story) && (c.operations[*o].kind != OperationKind::Capstone || act.story_ops.iter().all(|s| done.contains(s))));
        let next = next.unwrap_or_else(|| panic!("stalled after {done:?}")).clone();
        if c.operations[&next].kind == OperationKind::Story { story += 1; }
        done.insert(next);
    }
    assert_eq!(done.len(), order.len());
    // Each story operation after the first also has a theatre-condition path (L778).
    fn has_condition(u: &Unlock) -> bool { match u { Unlock::SupplyBelow { .. } | Unlock::AlertAtLeast { .. } => true, Unlock::All(v) | Unlock::Any(v) => v.iter().any(has_condition), _ => false } }
    for s in &act.story_ops[1..] {
        assert!(has_condition(&c.operations[s].unlock), "{s} is unlocked by theatre conditions too");
    }
}

// ---------------------------------------------------------------------------------------------------------
// Aces and doctrine

#[test]
fn aces_have_full_arcs_and_act1_aces_appear_in_act1() {
    let c = data();
    let cues = [AceCue::Arrives, AceCue::SightsPlayer, AceCue::FiresFirst, AceCue::TakesDamage, AceCue::PlayerDamaged, AceCue::Withdraws, AceCue::Defeated, AceCue::Captured];
    let mut arcs_per_act: BTreeMap<Act, usize> = BTreeMap::new();
    for arc in c.aces.values().flat_map(|a| &a.arcs) { *arcs_per_act.entry(arc.act).or_default() += 1; }
    assert_eq!(arcs_per_act, BTreeMap::from([(Act::Act1, 2), (Act::Act2, 2), (Act::Act3, 2), (Act::Act4, 1)]), "L774");
    for a in c.aces.values() {
        for arc in &a.arcs {
            for cue in cues { assert!(arc.lines.iter().any(|l| l.cue == cue), "{}: no {cue:?} line", a.id); }
        }
        assert!(!a.doctrine.is_empty(), "{}", a.id);
    }
    for id in &c.acts[0].aces {
        let a = &c.aces[id];
        assert_eq!(c.classes[&a.ship].act, Act::Act1);
        let arc = a.arcs.iter().find(|x| x.act == Act::Act1).expect("an Act 1 arc");
        let duel = c.operations.values().find(|o| o.kind == OperationKind::AceDuel(id.clone())).expect("a duel");
        assert!(arc.operations.contains(&duel.id), "{id}: the duel is in the arc");
        // The ace arrives in a story operation before the duel.
        assert!(arc.operations.iter().any(|o| c.operations[o].complications.contains(&content::campaign::Complication::AceArrives(id.clone()))), "{id}");
    }
}

#[test]
fn doctrine_cards_cover_every_tally_with_effects_the_systems_read() {
    let c = data();
    let mut tallies: BTreeSet<TallyKey> = BTreeSet::new();
    for m in [KillMethod::Gunfire, KillMethod::Missile, KillMethod::Torpedo, KillMethod::Mine, KillMethod::DepthCharge, KillMethod::Fire, KillMethod::Capture, KillMethod::Collision] { tallies.insert(TallyKey::Method(m)); }
    for r in [RangeBand::Close, RangeBand::Medium, RangeBand::Long, RangeBand::BeyondHorizon] { tallies.insert(TallyKey::Range(r)); }
    for t in [TimeOfDay::Night, TimeOfDay::Dusk, TimeOfDay::Day] { tallies.insert(TallyKey::Time(t)); }
    for e in [Emcon::Silent, Emcon::Burst, Emcon::Active] { tallies.insert(TallyKey::Emission(e)); }
    let covered: BTreeSet<TallyKey> = c.cards.values().map(|x| x.counters).collect();
    assert_eq!(covered, tallies, "every one of the 18 tallies queues a counter");
    let flags = ["close_in", "stand_off", "hold_ground", "silent_approach", "aggressive"];
    for card in c.cards.values() {
        let e = &card.effect;
        assert!(e.add_fleet.is_some() || e.use_variants || e.enemy_emcon.is_some() || e.patrol_density_factor.is_some() || e.engagement_range_factor.is_some() || !e.behaviour_flags.is_empty(), "{} does nothing", card.id);
        assert!(e.behaviour_flags.iter().all(|f| flags.contains(&f.as_str())), "{}: unread flag", card.id);
        for f in [e.patrol_density_factor, e.engagement_range_factor].into_iter().flatten() { assert!((0.25..=4.0).contains(&f), "{}", card.id); }
        assert_eq!(card.name.as_str(), format!("{}.name", card.id));
        assert_eq!(card.description.as_str(), format!("{}.desc", card.id));
    }
    // The spec's two examples (L772).
    assert!(c.cards.values().any(|x| x.counters == TallyKey::Method(KillMethod::Missile) && x.effect.add_fleet.is_some()), "missiles: point defence thickens");
    let pickets = |f: &FleetTemplateId| c.fleets[f].slots.iter().any(|s| s.classes.iter().any(|cl| c.classes[cl].role == ClassRole::Picket));
    assert!(c.cards.values().any(|x| x.counters == TallyKey::Time(TimeOfDay::Night) && x.effect.add_fleet.as_ref().is_some_and(pickets)), "night raids: radar pickets appear");
}

// ---------------------------------------------------------------------------------------------------------
// Text

fn csv_rows(file: &str) -> Vec<(String, String, String)> {
    let text = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../game/i18n").join(file)).unwrap();
    // RFC 4180 with quoted fields; no field of these files spans lines.
    let mut out = Vec::new();
    for line in text.lines().skip(1).filter(|l| !l.is_empty()) {
        let mut cells = Vec::new();
        let (mut cell, mut q, mut chars) = (String::new(), false, line.chars().peekable());
        while let Some(ch) = chars.next() {
            match (ch, q) {
                ('"', true) if chars.peek() == Some(&'"') => { cell.push('"'); chars.next(); }
                ('"', _) => q = !q,
                (',', false) => cells.push(std::mem::take(&mut cell)),
                _ => cell.push(ch),
            }
        }
        cells.push(cell);
        assert_eq!(cells.len(), 3, "{file}: {line}");
        out.push((cells[0].clone(), cells[1].clone(), cells[2].clone()));
    }
    out
}

const OWN_CSV: [&str; 8] = ["theatre.csv", "enemy.csv", "operations_act1.csv", "operations_act2.csv", "operations_act3.csv", "operations_act4.csv", "aces.csv", "doctrine.csv"];

#[test]
fn every_text_key_has_english_and_radio_lines_fit_two_short_lines() {
    let c = data();
    let mut rows: BTreeMap<String, (String, String)> = BTreeMap::new();
    for f in OWN_CSV {
        for (k, ctx, en) in csv_rows(f) {
            assert!(!ctx.trim().is_empty() && !en.trim().is_empty(), "{f}: {k}");
            assert!(rows.insert(k.clone(), (ctx, en)).is_none(), "{k} defined twice");
        }
    }
    let mut keys: Vec<&str> = vec![c.theatre.name.as_str()];
    keys.extend(c.theatre.regions.iter().map(|r| r.name.as_str()));
    keys.extend(c.theatre.nodes.iter().map(|n| n.name.as_str()));
    keys.extend(c.acts.iter().map(|a| a.name.as_str()));
    keys.extend(c.classes.values().map(|x| x.name.as_str()));
    keys.extend(c.operations.values().flat_map(|o| [o.name.as_str(), o.briefing.as_str()]));
    keys.extend(c.cards.values().flat_map(|x| [x.name.as_str(), x.description.as_str()]));
    for a in c.aces.values() {
        keys.extend([a.name.as_str(), a.callsign.as_str()]);
        for l in a.arcs.iter().flat_map(|x| &x.lines) {
            let (ctx, en) = rows.get(l.text.as_str()).unwrap_or_else(|| panic!("{} has no row", l.text));
            assert!(ctx.starts_with("Radio line"), "{}: radio lines are exempt chatter only with a radio context", l.text);
            assert!(fits_two_lines(en, 40), "{}: \"{en}\" does not fit two 40-character lines (copy rule 12)", l.text);
        }
    }
    for k in keys { assert!(rows.contains_key(k), "{k} has no English row in {OWN_CSV:?}"); }
}

/// Greedy word wrap into at most two lines of `width` characters.
fn fits_two_lines(text: &str, width: usize) -> bool {
    let mut lines = 1;
    let mut len = 0;
    for w in text.split_whitespace() {
        let add = if len == 0 { w.chars().count() } else { len + 1 + w.chars().count() };
        if add > width { lines += 1; len = w.chars().count(); } else { len = add; }
    }
    lines <= 2
}

#[test]
fn copy_avoids_the_banned_words() {
    const BANNED: [&str; 19] = ["introducing", "unlock", "empower", "elevate", "streamline", "transform", "discover", "seamless", "smart", "next-gen", "revolutionize", "leverage", "delve", "vibrant", "pivotal", "showcase", "tapestry", "testament", "unleash"];
    for f in OWN_CSV {
        for (k, _, en) in csv_rows(f) {
            let lower = en.to_lowercase();
            for w in lower.split(|ch: char| !ch.is_alphanumeric() && ch != '-') {
                assert!(!BANNED.iter().any(|b| w.starts_with(b)), "{f}: {k} uses \"{w}\"");
            }
            assert!(!en.contains('!') && !en.contains('\u{2014}'), "{f}: {k}");
        }
    }
}
