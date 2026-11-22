//! Unit `content-campaign`. Loading and cross-checking campaign data.
//!
//! Every check here guards a reference some other system follows at run time (the theatre generator, the
//! sortie builder, the doctrine bulletin), so a broken reference fails the build's tests instead of a
//! campaign hours in. The first problem found is returned; checks run in file order, rows in file order.
use super::{CampaignData, ConstraintTag, Objective, OperationKind, Reward, Unlock};
use crate::campaign::{AceDef, ActDef, CampaignRules, Complication, DifficultyPreset, DoctrineCardDef, EnemyClassDef, FleetTemplate, NodeDef, NodeKind, OperationDef, TheatreDef};
use crate::catalogue::Catalogue;
use crate::enums::Act;
use crate::error::{ContentError, ContentResult};
use crate::ids::{OperationId, RegionId, TheatreNodeId};
use indexmap::IndexMap;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

fn bad(file: &str, message: impl Into<String>) -> ContentError { ContentError::Parse { file: file.into(), message: message.into() } }
fn missing(kind: &'static str, key: impl ToString) -> ContentError { ContentError::UnknownRef { kind, key: key.to_string() } }

const ACTS_FILE: &str = "crates/content/data/acts.ron";
const OPS_FILE: &str = "crates/content/data/operations";
const THEATRE_FILE: &str = "crates/content/data/theatre.ron";

/// Parse every embedded campaign file ([`crate::data`]), merge the per-act lists in act order, and check every
/// reference: classes name existing stock blueprints, fleets name classes, operations name fleets, markers and
/// aces, acts name operations and nodes, cards and tallies are unique, every operation has 2 to 4 constraint
/// tags and 2 or 3 ghost conditions (tutorial exempt), every act's starters fit its tonnage cap (L151).
/// Unit: `content-campaign`.
///
/// Rows of `actN.ron` must belong to act N. "Tallies unique" means no two cards counter the same tally with
/// the same effect (about 30 cards share 18 tallies, so a tally may queue several different counters).
pub fn load_embedded(catalogue: &Catalogue) -> ContentResult<CampaignData> {
    use crate::data::{self, parse_ron};
    let theatre: TheatreDef = parse_ron(data::THEATRE, THEATRE_FILE)?;
    let acts: Vec<ActDef> = parse_ron(data::ACTS, ACTS_FILE)?;
    let rules: CampaignRules = parse_ron(data::CAMPAIGN_RULES, "crates/content/data/campaign_rules.ron")?;
    let difficulty: Vec<DifficultyPreset> = parse_ron(data::DIFFICULTY, "crates/content/data/difficulty.ron")?;
    let aces: Vec<AceDef> = parse_ron(data::ACES, "crates/content/data/aces.ron")?;
    let cards: Vec<DoctrineCardDef> = parse_ron(data::DOCTRINE_CARDS, "crates/content/data/doctrine_cards.ron")?;
    let mut data = CampaignData { theatre, acts, rules, difficulty, ..Default::default() };
    for (i, act) in Act::ALL.into_iter().enumerate() {
        let n = i + 1;
        let file = format!("crates/content/data/enemy_classes/act{n}.ron");
        for c in parse_ron::<Vec<EnemyClassDef>>(data::ENEMY_CLASSES[i], &file)? {
            if c.act != act {
                return Err(bad(&file, format!("class `{}` is {:?} in the {act:?} file", c.id, c.act)));
            }
            let id = c.id.clone();
            if data.classes.insert(id.clone(), c).is_some() {
                return Err(bad(&file, format!("duplicate class `{id}`")));
            }
        }
        let file = format!("crates/content/data/fleets/act{n}.ron");
        for f in parse_ron::<Vec<FleetTemplate>>(data::FLEETS[i], &file)? {
            if f.act != act {
                return Err(bad(&file, format!("fleet `{}` is {:?} in the {act:?} file", f.id, f.act)));
            }
            let id = f.id.clone();
            if data.fleets.insert(id.clone(), f).is_some() {
                return Err(bad(&file, format!("duplicate fleet `{id}`")));
            }
        }
        let file = format!("{OPS_FILE}/act{n}.ron");
        for o in parse_ron::<Vec<OperationDef>>(data::OPERATIONS[i], &file)? {
            if o.act != act {
                return Err(bad(&file, format!("operation `{}` is {:?} in the {act:?} file", o.id, o.act)));
            }
            let id = o.id.clone();
            if data.operations.insert(id.clone(), o).is_some() {
                return Err(bad(&file, format!("duplicate operation `{id}`")));
            }
        }
    }
    data.aces = keyed(aces, |a| a.id.clone(), "crates/content/data/aces.ron", "ace")?;
    data.cards = keyed(cards, |c| c.id.clone(), "crates/content/data/doctrine_cards.ron", "doctrine card")?;
    check(&data, catalogue, &|path| stock_blueprint_with(path, catalogue))?;
    Ok(data)
}

fn keyed<K: std::hash::Hash + Eq + std::fmt::Display + Clone, V>(rows: Vec<V>, key: impl Fn(&V) -> K, file: &str, what: &str) -> ContentResult<IndexMap<K, V>> {
    let mut out = IndexMap::with_capacity(rows.len());
    for r in rows {
        let k = key(&r);
        if out.insert(k.clone(), r).is_some() {
            return Err(bad(file, format!("duplicate {what} `{k}`")));
        }
    }
    Ok(out)
}

type BlueprintLookup<'a> = &'a dyn Fn(&str) -> ContentResult<crate::blueprint::Blueprint>;

/// Every cross-reference of loaded campaign data. `blueprint` resolves a stock blueprint path.
pub(crate) fn check(data: &CampaignData, catalogue: &Catalogue, blueprint: BlueprintLookup<'_>) -> ContentResult<()> {
    let th = &data.theatre;

    // Theatre: regions and nodes unique, nodes in known regions, lanes between distinct known nodes.
    let mut regions = BTreeSet::new();
    for r in &th.regions {
        if !regions.insert(&r.id) {
            return Err(bad(THEATRE_FILE, format!("duplicate region `{}`", r.id)));
        }
        if let Some((class, _)) = r.order_of_battle.iter().find(|(c, _)| !data.classes.contains_key(c)) {
            return Err(missing("enemy class", class));
        }
        if let Some(c) = r.civilian.iter().find(|c| !data.classes.contains_key(*c)) {
            return Err(missing("enemy class", c));
        }
    }
    let mut nodes: BTreeMap<&TheatreNodeId, &NodeDef> = BTreeMap::new();
    for n in &th.nodes {
        if nodes.insert(&n.id, n).is_some() {
            return Err(bad(THEATRE_FILE, format!("duplicate node `{}`", n.id)));
        }
        if !regions.contains(&n.region) {
            return Err(missing("region", &n.region));
        }
    }
    let mut lanes = BTreeSet::new();
    for l in &th.lanes {
        for end in [&l.a, &l.b] {
            if !nodes.contains_key(end) {
                return Err(missing("theatre node", end));
            }
        }
        let key = if l.a < l.b { (&l.a, &l.b) } else { (&l.b, &l.a) };
        if l.a == l.b || !lanes.insert(key) {
            return Err(bad(THEATRE_FILE, format!("lane `{}` - `{}` is a loop or a duplicate", l.a, l.b)));
        }
    }

    // Classes name stock blueprints; variants name classes.
    for c in data.classes.values() {
        blueprint(&c.blueprint)?;
        if let Some(v) = c.variants.iter().find(|v| !data.classes.contains_key(*v)) {
            return Err(missing("enemy class", v));
        }
    }
    // Fleets name classes.
    for f in data.fleets.values() {
        for s in &f.slots {
            if s.classes.is_empty() || s.min > s.max {
                return Err(bad("crates/content/data/fleets", format!("fleet `{}` has an empty slot or min above max", f.id)));
            }
            if let Some(c) = s.classes.iter().find(|c| !data.classes.contains_key(*c)) {
                return Err(missing("enemy class", c));
            }
        }
    }

    for o in data.operations.values() {
        check_operation(o, data, catalogue, &nodes)?;
    }

    for a in data.aces.values() {
        if !data.classes.contains_key(&a.ship) {
            return Err(missing("enemy class", &a.ship));
        }
        if let Some(c) = a.doctrine.iter().find(|c| !data.cards.contains_key(*c)) {
            return Err(missing("doctrine card", c));
        }
        for arc in &a.arcs {
            if let Some(op) = arc.operations.iter().find(|o| !data.operations.contains_key(*o)) {
                return Err(missing("operation", op));
            }
        }
    }

    // Cards: ids are unique by construction; no two cards may be the same counter to the same tally.
    let mut counters = BTreeSet::new();
    for c in data.cards.values() {
        if let Some(f) = &c.effect.add_fleet {
            if !data.fleets.contains_key(f) {
                return Err(missing("fleet template", f));
            }
        }
        if !counters.insert((c.counters, format!("{:?}", c.effect))) {
            return Err(bad("crates/content/data/doctrine_cards.ron", format!("card `{}` repeats another card's tally and effect", c.id)));
        }
    }

    let mut presets = BTreeSet::new();
    if let Some(p) = data.difficulty.iter().find(|p| !presets.insert(&p.id)) {
        return Err(bad("crates/content/data/difficulty.ron", format!("duplicate difficulty preset `{}`", p.id)));
    }

    // Acts: in order, once each, every reference resolves, starters fit the act's cap.
    if data.acts.len() > Act::ALL.len() {
        return Err(bad(ACTS_FILE, format!("{} act rows; the campaign has 4 acts", data.acts.len())));
    }
    for (a, expected) in data.acts.iter().zip(Act::ALL) {
        if a.act != expected {
            return Err(bad(ACTS_FILE, format!("act rows must run Act1 to Act4 in order; found {:?} where {expected:?} belongs", a.act)));
        }
        check_act(a, data, catalogue, &nodes, &regions, blueprint)?;
    }
    Ok(())
}

fn op_kind<'a>(data: &'a CampaignData, id: &OperationId) -> ContentResult<&'a OperationKind> { data.operations.get(id).map(|o| &o.kind).ok_or_else(|| missing("operation", id)) }

fn check_act(a: &ActDef, data: &CampaignData, catalogue: &Catalogue, nodes: &BTreeMap<&TheatreNodeId, &NodeDef>, regions: &BTreeSet<&RegionId>, blueprint: BlueprintLookup<'_>) -> ContentResult<()> {
    let cap = a.act.hull_cap_t();
    if a.starters.is_empty() {
        return Err(bad(ACTS_FILE, format!("{:?} offers no starter hull", a.act)));
    }
    for s in &a.starters {
        let bp = blueprint(s)?;
        let t = crate::stats::compute_stats(&bp, catalogue).tonnage_t;
        if !(t <= cap) {
            return Err(bad(ACTS_FILE, format!("{:?} starter `{s}` is {t:.1} t, over the {cap} t cap", a.act)));
        }
    }
    for (op, _) in &a.tier_unlocks {
        op_kind(data, op)?;
    }
    if let Some(r) = a.regions.iter().find(|r| !regions.contains(r)) {
        return Err(missing("region", r));
    }
    for op in &a.story_ops {
        if !matches!(op_kind(data, op)?, OperationKind::Story) {
            return Err(bad(ACTS_FILE, format!("`{op}` is listed as a story operation but is not one")));
        }
    }
    for op in &a.ace_duels {
        if !matches!(op_kind(data, op)?, OperationKind::AceDuel(_)) {
            return Err(bad(ACTS_FILE, format!("`{op}` is listed as an ace duel but is not one")));
        }
    }
    if !matches!(op_kind(data, &a.capstone)?, OperationKind::Capstone) {
        return Err(bad(ACTS_FILE, format!("`{}` is the {:?} capstone but is not a capstone", a.capstone, a.act)));
    }
    if let Some(ace) = a.aces.iter().find(|x| !data.aces.contains_key(*x)) {
        return Err(missing("ace", ace));
    }
    for c in a.coves.iter().chain(&a.retreat_closed_cove) {
        match nodes.get(c) {
            None => return Err(missing("theatre node", c)),
            Some(n) if !matches!(n.kind, NodeKind::Cove) => return Err(bad(ACTS_FILE, format!("`{c}` is listed as a cove but is not one"))),
            _ => {}
        }
    }
    if let Some(c) = &a.retreat_closed_cove {
        if !a.coves.contains(c) {
            return Err(bad(ACTS_FILE, format!("retreat closes `{c}`, which is not one of {:?}'s coves", a.act)));
        }
    }
    Ok(())
}

fn check_unlock(u: &Unlock, data: &CampaignData, depth: usize) -> ContentResult<()> {
    if depth > 32 {
        return Err(bad(OPS_FILE, "unlock condition nested too deep"));
    }
    let region = |r: &RegionId| if data.theatre.regions.iter().any(|d| d.id == *r) { Ok(()) } else { Err(missing("region", r)) };
    match u {
        Unlock::Always | Unlock::StoryCount(_) => Ok(()),
        Unlock::SupplyBelow { region: r, .. } | Unlock::AlertAtLeast { region: r, .. } => region(r),
        Unlock::Completed(ops) => ops.iter().try_for_each(|o| op_kind(data, o).map(|_| ())),
        Unlock::AceBeaten(a) => if data.aces.contains_key(a) { Ok(()) } else { Err(missing("ace", a)) },
        Unlock::All(v) | Unlock::Any(v) => v.iter().try_for_each(|x| check_unlock(x, data, depth + 1)),
    }
}

fn check_operation(o: &OperationDef, data: &CampaignData, catalogue: &Catalogue, nodes: &BTreeMap<&TheatreNodeId, &NodeDef>) -> ContentResult<()> {
    let tutorial = matches!(o.kind, OperationKind::Tutorial);
    if !tutorial && !(2..=4).contains(&o.tags.len()) {
        return Err(bad(OPS_FILE, format!("operation `{}` has {} constraint tags; briefings carry 2 to 4", o.id, o.tags.len())));
    }
    if !tutorial && !(2..=3).contains(&o.ghosts.len()) {
        return Err(bad(OPS_FILE, format!("operation `{}` has {} ghost conditions; operations carry 2 or 3", o.id, o.ghosts.len())));
    }
    for t in &o.tags {
        let ok = match t {
            ConstraintTag::DraftLimit { max_m } => max_m.is_finite() && *max_m > 0.0,
            ConstraintTag::RcsLimit { max_m2 } => max_m2.is_finite() && *max_m2 > 0.0,
            ConstraintTag::Range { min_km } => min_km.is_finite() && *min_km >= 0.0,
            ConstraintTag::MinSpeed { kn } => kn.is_finite() && *kn >= 0.0,
            ConstraintTag::Passengers { .. } | ConstraintTag::Requires(_) => true,
        };
        if !ok {
            return Err(bad(OPS_FILE, format!("operation `{}` has a constraint tag with a bad limit", o.id)));
        }
    }
    if let OperationKind::AceDuel(a) = &o.kind {
        if !data.aces.contains_key(a) {
            return Err(missing("ace", a));
        }
    }
    check_unlock(&o.unlock, data, 0)?;
    if let Some(n) = &o.node {
        if !nodes.contains_key(n) {
            return Err(missing("theatre node", n));
        }
    }
    let fleet = |f: &crate::ids::FleetTemplateId| if data.fleets.contains_key(f) { Ok(()) } else { Err(missing("fleet template", f)) };
    for e in &o.force {
        fleet(&e.fleet)?;
    }
    if let Some(f) = &o.per_extra_player {
        fleet(f)?;
    }
    for c in &o.complications {
        match c {
            Complication::Reinforcements { fleet: f, .. } => fleet(f)?,
            Complication::AceArrives(a) if !data.aces.contains_key(a) => return Err(missing("ace", a)),
            _ => {}
        }
    }
    for g in &o.ghosts {
        if let Reward::Blueprint(p) = &g.reward {
            if catalogue.part(p).is_none() {
                return Err(missing("part", p));
            }
        }
    }
    if let Some(c) = o.declared_hostile.iter().find(|c| !data.classes.contains_key(*c)) {
        return Err(missing("enemy class", c));
    }
    if !(o.approach_m.is_finite() && o.approach_m > 0.0) {
        return Err(bad(OPS_FILE, format!("operation `{}` has no approach distance", o.id)));
    }

    // Objective tags name force entries.
    let tags: BTreeSet<&str> = o.force.iter().map(|e| e.tag.as_str()).filter(|t| !t.is_empty()).collect();
    let tag = match &o.objective {
        Objective::DestroyGroup { tag, .. } | Objective::Capture { tag } | Objective::Escort { tag, .. } | Objective::Photograph { tag, .. } | Objective::CutOut { tag, .. } => Some(tag.as_str()),
        _ => None,
    };
    if let Some(t) = tag {
        if !tags.contains(t) {
            return Err(missing("force tag", format!("{t} in `{}`", o.id)));
        }
    }
    // Markers: an authored operation's map (its own, else its node's) must hold every marker it names.
    // Templates are placed by the generator, which picks a node whose map fits.
    let map = o.map.as_ref().or_else(|| o.node.as_ref().and_then(|n| nodes.get(n)).map(|n| &n.map));
    if let Some(map) = map {
        let markers: BTreeSet<&str> = map.markers.iter().map(|m| m.name.as_str()).collect();
        let mut named: Vec<&str> = o.force.iter().map(|e| e.marker.as_str()).collect();
        match &o.objective {
            Objective::DestroySite { marker } | Objective::Extract { marker, .. } | Objective::LayMines { marker, .. } => named.push(marker),
            Objective::Escort { exit, .. } | Objective::Survive { exit, .. } | Objective::CutOut { exit, .. } => named.push(exit),
            _ => {}
        }
        if let Some(m) = named.into_iter().find(|m| !markers.contains(m)) {
            return Err(missing("map marker", format!("{m} in `{}`", o.id)));
        }
    }
    Ok(())
}

/// Process-wide campaign data built once from embedded files. Unit: `content-campaign`.
///
/// Panics with the load error if the embedded data is invalid, as [`crate::catalogue::catalogue`] does; the
/// content tests load it on every platform, so a broken reference never reaches a build.
pub fn campaign() -> &'static CampaignData {
    static CAMPAIGN: OnceLock<CampaignData> = OnceLock::new();
    CAMPAIGN.get_or_init(|| load_embedded(crate::catalogue::catalogue()).unwrap_or_else(|e| panic!("embedded campaign data is invalid: {e}")))
}

/// Normalise a path under `blueprints/`: forward slashes, no leading `./` or `blueprints/`.
fn normalize_path(path: &str) -> String {
    let p = path.trim().replace('\\', "/");
    let p = p.trim_start_matches("./");
    p.strip_prefix("blueprints/").unwrap_or(p).to_string()
}

fn stock_blueprint_with(path: &str, catalogue: &Catalogue) -> ContentResult<crate::blueprint::Blueprint> {
    let key = normalize_path(path);
    let (file, text) = crate::data::stock_blueprint_files().find(|(p, _)| *p == key).ok_or_else(|| missing("stock blueprint", path))?;
    Ok(crate::blueprint::io::load_blueprint_with(text, &format!("blueprints/{file}"), catalogue)?.blueprint)
}

/// Look up a stock blueprint (starter hull, tutorial variant, enemy class, trial ship) by its path under
/// `blueprints/`, parsed and migrated. Unit: `content-campaign`.
///
/// Parsed once per path and cached: sorties spawn the same classes many times.
pub fn stock_blueprint(path: &str) -> ContentResult<crate::blueprint::Blueprint> {
    static CACHE: OnceLock<Mutex<BTreeMap<String, crate::blueprint::Blueprint>>> = OnceLock::new();
    let key = normalize_path(path);
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some(bp) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Ok(bp.clone());
    }
    // Parse outside the lock; every thread computes the same blueprint.
    let bp = stock_blueprint_with(&key, crate::catalogue::catalogue())?;
    cache.lock().unwrap_or_else(|e| e.into_inner()).insert(key, bp.clone());
    Ok(bp)
}

#[cfg(test)]
mod tests;
