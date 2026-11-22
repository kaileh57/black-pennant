//! Unit `gd-content`. `BpContent` (autoload `Content`): read-only queries on the embedded game data.
//! DESIGN.md L136-151 (parts, kits, starter hulls), L801-807 (tiers and unlocks), L955-976 (rulesets),
//! L847-860 (difficulty presets), L739-751 (the theatre graph: 48 nodes and lanes on a 600 by 400 km map),
//! L756 (enemy classes), L774 (aces), L776-782 (mission templates).
//! Everything comes from `content::catalogue` and `content::campaign`; nothing here changes state. Results
//! are constant for a build, so each list is converted once and cached.
//!
//! Implementation notes (unit-owned):
//! - Rules live in [`model`] (pure Rust, tested headless), chart geometry in [`chart`], shapes in [`dicts`].
//! - The cache holds only what is constant for a build: per-part and per-kit static fields, and the class,
//!   starter, ruleset, difficulty, template, theatre and ace lists. Availability, locked reasons, limits,
//!   store counts, search hits and the name order (which follows the locale) are worked out per call, and
//!   player kits are read from disk per call. Every return is a deep copy, so GDScript may edit it freely.
//! - Embedded data that fails its load is reported once with `godot_error!` and the queries that need it
//!   return empty results instead of panicking.
//! - In campaign mode the save's part state is read from the `Campaign` autoload's `unlocks()` (the method
//!   the contract names). With no campaign loaded every campaign part reads as locked with a reason.
pub(crate) mod chart;
pub(crate) mod dicts;
pub(crate) mod model;
#[cfg(test)]
mod tests;

use crate::convert::{get_i64, get_text};
use crate::enums::GdEnum;
use crate::services::services;
use crate::text::EngineLookup;
use content::campaign::CampaignData;
use content::catalogue::Catalogue;
use content::enums::{Act, GameMode, Tier};
use content::ids::{KitId, PartId};
use content::kit::KitDef;
use content::part::PartCategory;
use content::ruleset::Ruleset;
use godot::prelude::*;
use model::{Filter, Unlocks};
use std::cell::{OnceCell, RefCell};
use std::sync::Arc;

/// Player kits live here (`gd-editor-io` writes them).
const PLAYER_KIT_DIR: &str = "user://blueprints/kits";
/// Most player kit files read per call; a folder with more lists the first ones by file name.
const MAX_PLAYER_KITS: usize = 512;

/// Embedded data, checked once.
struct Data {
    catalogue: Option<Arc<Catalogue>>,
    campaign: Option<Arc<CampaignData>>,
}

/// Catalogue and campaign data queries. Autoload `Content`.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct BpContent {
    /// Converted static results by key (`part:<id>`, `kit:<id>`, `classes:<act>`, ...). A `RefCell`
    /// because every query takes `&self`.
    pub(crate) cache: RefCell<std::collections::BTreeMap<String, Variant>>,
    data: OnceCell<Data>,
    rulesets: OnceCell<Vec<(String, Ruleset)>>,
    /// Stock kit summaries by kit id (static).
    kit_summaries: RefCell<std::collections::BTreeMap<KitId, model::KitSummary>>,
    /// Parsed player kits by file name, with the modified time they were parsed at (`None` = unreadable).
    player_kits: RefCell<std::collections::BTreeMap<String, (u64, Option<KitDef>)>>,
    base: Base<Node>,
}

#[godot_api]
impl INode for BpContent {
    fn init(base: Base<Node>) -> Self {
        Self { cache: Default::default(), data: OnceCell::new(), rulesets: OnceCell::new(), kit_summaries: Default::default(), player_kits: Default::default(), base }
    }
}

#[godot_api]
impl BpContent {
    /// Parts matching a filter: `Array[PartDefDict]`, by category then tier then name. `filter` keys (all
    /// optional): `mode: int` (`Bp.MODE_*`; default sandbox = every part of the build), `act: int`,
    /// `category: int`, `tier: int`, `tag: String`, `search: String` (matches the translated name),
    /// `ruleset: String` (fills `limit`). In campaign mode availability comes from `Campaign.unlocks()`.
    #[func]
    fn parts(&self, filter: VarDictionary) -> Array<VarDictionary> {
        let Some(cat) = self.data().catalogue.clone() else { return Array::new() };
        let f = read_filter(&filter);
        let lk = EngineLookup::new();
        let unlocks = if f.campaign() { self.read_unlocks() } else { None };
        let act = campaign_act(&f);
        let ruleset = f.ruleset.as_deref().and_then(|id| self.ruleset(id));
        model::sorted_parts(&cat, &f, &lk)
            .into_iter()
            .map(|def| {
                let mut dict = self.part_base(&cat, def);
                dicts::set_access(&mut dict, &model::part_access(def, f.campaign(), act, unlocks.as_ref()));
                dict.set("limit", model::part_limit(ruleset.as_ref(), def) as i64);
                dict.set("in_store", unlocks.as_ref().and_then(|u| u.store.get(&def.id)).map_or(0, |n| *n as i64));
                dict
            })
            .collect()
    }
    /// One `PartDefDict`, or an empty Dictionary for an unknown id.
    #[func]
    fn part(&self, id: StringName) -> VarDictionary {
        let Some(cat) = self.data().catalogue.clone() else { return VarDictionary::new() };
        let Some(def) = cat.part(&PartId::new(id.to_string())) else { return VarDictionary::new() };
        let mut dict = self.part_base(&cat, def);
        dicts::set_access(&mut dict, &model::part_access(def, false, Act::Act4, None));
        dict
    }
    /// Kits matching the same filter keys (`category` is the category of the kit's main part):
    /// `Array[KitDefDict]`, stock kits first, then the player's from `user://blueprints/kits/`.
    #[func]
    fn kits(&self, filter: VarDictionary) -> Array<VarDictionary> {
        let Some(cat) = self.data().catalogue.clone() else { return Array::new() };
        let f = read_filter(&filter);
        let lk = EngineLookup::new();
        let unlocks = if f.campaign() { self.read_unlocks() } else { None };
        let act = campaign_act(&f);
        let mut stock: Vec<&KitDef> = cat.kits.values().collect();
        stock.sort_by(|a, b| (a.tray_order, &a.id).cmp(&(b.tray_order, &b.id)));
        let mut seen: std::collections::BTreeSet<KitId> = stock.iter().map(|k| k.id.clone()).collect();
        let mut player = self.player_kits_now();
        player.retain(|k| seen.insert(k.id.clone()));
        let mut named: Vec<(String, KitDef)> = player.into_iter().map(|k| (model::kit_display_name(&k, &lk).to_lowercase(), k)).collect();
        named.sort_by(|(an, a), (bn, b)| (an, &a.id).cmp(&(bn, &b.id)));
        let mut out = Array::new();
        for kit in stock {
            let summary = self.stock_summary(&cat, kit);
            if !model::kit_matches(kit, &summary, &f, &lk) { continue; }
            let mut dict = self.kit_base(&cat, kit);
            dicts::set_access(&mut dict, &model::kit_access(kit, &cat, f.campaign(), act, unlocks.as_ref()));
            out.push(&dict);
        }
        for (_, kit) in &named {
            let summary = model::kit_summary(kit, &cat);
            if !model::kit_matches(kit, &summary, &f, &lk) { continue; }
            let mut dict = dicts::kit_def_dict(kit, &summary, false);
            dicts::set_access(&mut dict, &model::kit_access(kit, &cat, f.campaign(), act, unlocks.as_ref()));
            out.push(&dict);
        }
        out
    }
    #[func]
    fn kit(&self, id: StringName) -> VarDictionary {
        let Some(cat) = self.data().catalogue.clone() else { return VarDictionary::new() };
        let key = KitId::new(id.to_string());
        let (mut dict, kit) = match cat.kit(&key) {
            Some(kit) => (self.kit_base(&cat, kit), kit.clone()),
            None => match self.player_kits_now().into_iter().find(|k| k.id == key) {
                Some(kit) => (dicts::kit_def_dict(&kit, &model::kit_summary(&kit, &cat), false), kit),
                None => return VarDictionary::new(),
            },
        };
        dicts::set_access(&mut dict, &model::kit_access(&kit, &cat, false, Act::Act4, None));
        dict
    }
    /// Part categories in tray order with counts: `Array[Dictionary]` with `category: int`,
    /// `label: StringName`, `icon: StringName`, `count: int`.
    #[func]
    fn categories(&self) -> Array<VarDictionary> {
        self.cached_list("categories", || {
            let Some(cat) = self.data().catalogue.clone() else { return Vec::new() };
            model::category_counts(&cat)
                .into_iter()
                .map(|(c, n)| crate::convert::d! { "category" => c.to_gd(), "label" => StringName::from(model::category_label(c).as_str()), "icon" => StringName::from(model::category_icon(c).as_str()), "count" => n as i64 })
                .collect()
        })
    }
    /// Enemy classes of the acts up to `act` (`Bp.NONE` = all in the build): `Array[ClassDict]`.
    #[func]
    fn classes(&self, act: i64) -> Array<VarDictionary> {
        let up_to = Act::from_gd(act);
        let key = format!("classes:{}", up_to.map_or(-1, |a| a.to_gd()));
        self.cached_list(&key, || {
            let Some(data) = self.data().campaign.clone() else { return Vec::new() };
            let mut list: Vec<&content::campaign::EnemyClassDef> = data.classes.values().filter(|c| up_to.is_none_or(|a| c.act <= a)).collect();
            list.sort_by(|a, b| (a.act, a.tier, &a.id).cmp(&(b.act, b.tier, &b.id)));
            list.into_iter().map(|c| self.class_base(c)).collect()
        })
    }
    #[func]
    fn class_info(&self, id: StringName) -> VarDictionary {
        let Some(data) = self.data().campaign.clone() else { return VarDictionary::new() };
        match data.classes.get(&content::ids::ClassId::new(id.to_string())) { Some(c) => self.class_base(c), None => VarDictionary::new() }
    }
    /// Lobby rulesets: `Array[RulesetDict]`, the default (`standard`) first.
    #[func]
    fn rulesets(&self) -> Array<VarDictionary> { self.rulesets_list().iter().map(|(id, r)| crate::convert::ruleset_dict(id, r)).collect() }
    /// Difficulty presets: `Array[DifficultyDict]` with an extra `name: StringName`, Normal first in play
    /// order. Slider ranges are in `difficulty_ranges()`.
    #[func]
    fn difficulty_presets(&self) -> Array<VarDictionary> {
        self.cached_list("difficulty_presets", || {
            let presets: Vec<content::campaign::DifficultyPreset> = match content::data::parse_ron(content::data::DIFFICULTY, "crates/content/data/difficulty.ron") {
                Ok(p) => p,
                Err(e) => { godot_error!("difficulty presets do not load: {e}"); return Vec::new() }
            };
            model::presets_in_order(&presets)
                .into_iter()
                .map(|p| {
                    let mut d = crate::convert::difficulty_dict(&p.sliders.clamped(), p.id.as_str());
                    d.set("name", &StringName::from(p.name.as_str()));
                    d
                })
                .collect()
        })
    }
    /// Range per slider: `{ "<slider>": { "min": float, "max": float, "step": float, "locks_at_start": bool } }`.
    #[func]
    fn difficulty_ranges(&self) -> VarDictionary {
        let mut out = VarDictionary::new();
        for r in &model::DIFFICULTY_RANGES {
            out.set(r.key, &crate::convert::d! { "min" => r.min, "max" => r.max, "step" => r.step, "locks_at_start" => r.locks_at_start });
        }
        out
    }
    /// Starter hulls offered at an act (`Bp.NONE` = all six, as in sandbox, skirmish and PvP):
    /// `Array[BlueprintInfoDict]`.
    #[func]
    fn starters(&self, act: i64) -> Array<VarDictionary> {
        let at = Act::from_gd(act);
        let key = format!("starters:{}", at.map_or(-1, |a| a.to_gd()));
        self.cached_list(&key, || {
            let (Some(cat), Some(data)) = (self.data().catalogue.clone(), self.data().campaign.clone()) else { return Vec::new() };
            // Sandbox, skirmish and PvP keep all six starters; an act's list comes from the acts this build ships.
            let acts = if at.is_some() { &data.acts } else { &content::campaign::load::campaign().acts };
            model::starter_paths(acts, at)
                .into_iter()
                .filter_map(|path| match content::campaign::load::stock_blueprint(&path) {
                    Ok(bp) => {
                        let stats = content::stats::compute_stats(&bp, &cat);
                        Some(crate::convert::blueprint_info_dict(&bp, &stats, &model::normalize_stock(&path), true, 0))
                    }
                    Err(e) => { godot_warn!("starter hull {path} does not load: {e}"); None }
                })
                .collect()
        })
    }
    /// Mission templates for skirmish setup: `Array[Dictionary]` with `template: int` (`Bp.MISSION_*`),
    /// `name: StringName`, `description: StringName`, `needs: PackedStringArray` (part tags a hull needs).
    #[func]
    fn mission_templates(&self) -> Array<VarDictionary> {
        self.cached_list("mission_templates", || {
            let data = self.data().campaign.clone();
            content::campaign::MissionType::ALL
                .iter()
                .map(|m| {
                    let word = model::mission_word(*m);
                    let needs: PackedStringArray = model::mission_needs(*m, data.as_deref()).into_iter().map(|t| GString::from(model::tag_name(t))).collect();
                    crate::convert::d! {
                        "template" => m.to_gd(),
                        "name" => StringName::from(format!("units.word.mission.{word}").as_str()),
                        "description" => StringName::from(format!("content.mission.{word}.desc").as_str()),
                        "needs" => needs,
                    }
                })
                .collect()
        })
    }
    /// The static theatre graph for the chart table: `{ "size_km": Vector2, "nodes": Array[NodeDict],
    /// "lanes": Array[{ "a": StringName, "b": StringName, "length_km": float }], "regions":
    /// Array[{ "id": StringName, "name": StringName, "outline": PackedVector2Array }],
    /// "land": Array[PackedVector2Array], "shallows": Array[PackedVector2Array] }` (chart km).
    #[func]
    fn theatre_graph(&self) -> VarDictionary {
        self.cached_dict("theatre_graph".into(), || {
            let Some(data) = self.data().campaign.clone() else { return VarDictionary::new() };
            let t = &data.theatre;
            let outlines = chart::region_outlines(t);
            let (land, shallows) = chart::archipelago(t);
            dicts::theatre_graph_dict(t, &outlines, &land, &shallows)
        })
    }
    /// The seven aces: `Array[Dictionary]` with `id: StringName`, `name: StringName`, `callsign: StringName`,
    /// `ship_class: StringName`, `act: int`.
    #[func]
    fn aces(&self) -> Array<VarDictionary> {
        self.cached_list("aces", || {
            let Some(data) = self.data().campaign.clone() else { return Vec::new() };
            let mut list: Vec<VarDictionary> = data.aces.values().map(|a| dicts::ace_dict(a, &data)).collect();
            list.sort_by_key(|d| (get_i64(d, "act").unwrap_or(0), get_text(d, "id").unwrap_or_default()));
            list
        })
    }
    /// Tonnage caps in tonnes of the acts this build ships, in act order (150 for Act 1; later releases add
    /// 400, 900, 1,800). Skirmish and lobby setup offer one act per entry.
    #[func]
    fn act_caps(&self) -> PackedFloat32Array { model::shipped_acts(&services().campaign().acts).iter().map(|a| a.hull_cap_t() as f32).collect() }
    /// Player limit of a mode at this release: `{ "max_players": int, "team_sizes": PackedInt32Array }`.
    #[func]
    fn mode_limits(&self, mode: i64) -> VarDictionary {
        let Some(mode) = GameMode::from_gd(mode) else { return VarDictionary::new() };
        let (max, sizes) = model::mode_limits(mode, model::at_1_0());
        let sizes: PackedInt32Array = sizes.into_iter().map(i32::from).collect();
        crate::convert::d! { "max_players" => max as i64, "team_sizes" => sizes }
    }
}

impl BpContent {
    /// The embedded data, checked once. A load error is reported once and leaves that half `None`.
    fn data(&self) -> &Data {
        self.data.get_or_init(|| {
            let s = services();
            let catalogue = match Catalogue::load_embedded() {
                Ok(_) => Some(s.catalogue()),
                Err(e) => { godot_error!("the embedded part catalogue does not load: {e}"); None }
            };
            let campaign = catalogue.as_ref().and_then(|c| match content::campaign::load::load_embedded(c) {
                Ok(_) => Some(s.campaign()),
                Err(e) => { godot_error!("the embedded campaign data does not load: {e}"); None }
            });
            Data { catalogue, campaign }
        })
    }

    fn rulesets_list(&self) -> &[(String, Ruleset)] {
        self.rulesets.get_or_init(|| match content::data::parse_ron::<Vec<(String, Ruleset)>>(content::data::RULESETS, "crates/content/data/rulesets.ron") {
            Ok(list) => model::rulesets_in_order(list),
            Err(e) => { godot_error!("lobby rulesets do not load: {e}"); Vec::new() }
        })
    }
    fn ruleset(&self, id: &str) -> Option<Ruleset> { self.rulesets_list().iter().find(|(k, _)| k == id).map(|(_, r)| r.clone()) }

    /// A cached list, built on first use; returns a deep copy. An empty result after a data error is
    /// cached too: embedded data cannot change within a run.
    fn cached_list(&self, key: &str, build: impl FnOnce() -> Vec<VarDictionary>) -> Array<VarDictionary> {
        let hit = self.cache.borrow().get(key).and_then(|v| v.try_to::<Array<VarDictionary>>().ok());
        let list = match hit {
            Some(l) => l,
            None => {
                let list: Array<VarDictionary> = build().into_iter().collect();
                self.cache.borrow_mut().insert(key.to_string(), list.to_variant());
                list
            }
        };
        list.duplicate_deep()
    }

    /// A cached Dictionary by key; returns a deep copy.
    fn cached_dict(&self, key: String, build: impl FnOnce() -> VarDictionary) -> VarDictionary {
        let hit = self.cache.borrow().get(&key).and_then(|v| v.try_to::<VarDictionary>().ok());
        let dict = match hit {
            Some(d) => d,
            None => {
                let d = build();
                self.cache.borrow_mut().insert(key, d.to_variant());
                d
            }
        };
        dict.duplicate_deep()
    }

    fn part_base(&self, cat: &Catalogue, def: &content::part::PartDef) -> VarDictionary {
        self.cached_dict(format!("part:{}", def.id.as_str()), || dicts::part_def_dict(def, cat))
    }
    fn kit_base(&self, cat: &Catalogue, kit: &KitDef) -> VarDictionary {
        self.cached_dict(format!("kit:{}", kit.id.as_str()), || dicts::kit_def_dict(kit, &self.stock_summary(cat, kit), true))
    }
    fn stock_summary(&self, cat: &Catalogue, kit: &KitDef) -> model::KitSummary {
        if let Some(s) = self.kit_summaries.borrow().get(&kit.id) { return s.clone(); }
        let s = model::kit_summary(kit, cat);
        self.kit_summaries.borrow_mut().insert(kit.id.clone(), s.clone());
        s
    }

    /// Player kits from [`PLAYER_KIT_DIR`], parsed as untrusted text. A file is parsed again only when its
    /// modified time changes; unreadable files are skipped (the dry dock's file browser reports them).
    fn player_kits_now(&self) -> Vec<KitDef> {
        use godot::classes::{DirAccess, FileAccess};
        let mut memo = self.player_kits.borrow_mut();
        if !DirAccess::dir_exists_absolute(PLAYER_KIT_DIR) { memo.clear(); return Vec::new(); }
        let mut files: Vec<String> = DirAccess::get_files_at(PLAYER_KIT_DIR).as_slice().iter().map(|f| f.to_string()).filter(|f| f.ends_with(".kit.ron")).collect();
        files.sort();
        files.truncate(MAX_PLAYER_KITS);
        memo.retain(|name, _| files.binary_search(name).is_ok());
        let mut out = Vec::with_capacity(files.len());
        for f in files {
            let path = format!("{PLAYER_KIT_DIR}/{f}");
            let modified = FileAccess::get_modified_time(&path);
            let fresh = memo.get(&f).is_some_and(|(m, _)| *m == modified && modified != 0);
            if !fresh {
                let text = FileAccess::get_file_as_string(&path).to_string();
                let kit = if text.is_empty() { None } else { content::kit::parse_kit(&text, &f).ok() };
                memo.insert(f.clone(), (modified, kit));
            }
            if let Some((_, Some(kit))) = memo.get(&f) { out.push(kit.clone()); }
        }
        out
    }
    fn class_base(&self, c: &content::campaign::EnemyClassDef) -> VarDictionary {
        self.cached_dict(format!("class:{}", c.id.as_str()), || {
            let stats = self.data().catalogue.clone().and_then(|cat| match content::campaign::load::stock_blueprint(&c.blueprint) {
                Ok(bp) => Some(content::stats::compute_stats(&bp, &cat)),
                Err(e) => { godot_warn!("enemy class {} blueprint does not load: {e}", c.id.as_str()); None }
            });
            dicts::class_dict(c, stats.as_ref())
        })
    }

    /// The save's part state from `Campaign.unlocks()`, or `None` with no campaign loaded.
    fn read_unlocks(&self) -> Option<Unlocks> {
        if !self.base().is_inside_tree() { return None; }
        let mut node = self.base().get_node_or_null("/root/Campaign")?;
        if !node.has_method("unlocks") { return None; }
        // `try_call`: a failing `unlocks()` (an error in the campaign facade) must read as "no campaign",
        // not take this query down with it.
        let dict = node.try_call("unlocks", &[]).ok()?.try_to::<VarDictionary>().ok()?;
        unlocks_from_dict(&dict)
    }
}

/// The campaign act for availability: the filter's, else the last act (the save's unlock list already
/// holds only what the player has reached, so a missing `act` must not hide parts they own).
fn campaign_act(f: &Filter) -> Act { f.act.unwrap_or(Act::Act4) }

/// Read the `filter` Dictionary (see [`Filter`]).
fn read_filter(dict: &VarDictionary) -> Filter {
    let tag = get_text(dict, "tag").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).map(|s| model::tag_from_name(&s));
    Filter {
        mode: get_i64(dict, "mode").and_then(GameMode::from_gd),
        act: get_i64(dict, "act").and_then(Act::from_gd),
        category: get_i64(dict, "category").and_then(PartCategory::from_gd),
        tier: get_i64(dict, "tier").and_then(Tier::from_gd),
        tag,
        search: get_text(dict, "search").map(|s| s.trim().to_lowercase()).unwrap_or_default(),
        ruleset: get_text(dict, "ruleset").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
    }
}

/// `Campaign.unlocks()` to [`Unlocks`]. `None` for an empty Dictionary (no campaign loaded) or one without a
/// usable `max_tier`; bad entries in the lists are skipped.
fn unlocks_from_dict(dict: &VarDictionary) -> Option<Unlocks> {
    let max_tier = get_i64(dict, "max_tier").and_then(Tier::from_gd)?;
    let text = |v: &Variant| -> Option<String> {
        match v.get_type() {
            VariantType::STRING_NAME => v.try_to::<StringName>().ok().map(|s| s.to_string()),
            VariantType::STRING => v.try_to::<GString>().ok().map(|s| s.to_string()),
            _ => None,
        }
    };
    let unlocked = dict.get("unlocked").and_then(|v| v.try_to::<VarArray>().ok()).map(|a| a.iter_shared().filter_map(|v| text(&v)).map(PartId::new).collect()).unwrap_or_default();
    let store = dict
        .get("store")
        .and_then(|v| v.try_to::<VarDictionary>().ok())
        .map(|d| {
            d.iter_shared()
                .filter_map(|(k, v)| Some((PartId::new(text(&k)?), crate::convert::variant_f64(&v)?)))
                .filter(|(_, n)| *n >= 1.0)
                .map(|(k, n)| (k, n.min(u16::MAX as f64) as u16))
                .collect()
        })
        .unwrap_or_default();
    Some(Unlocks { max_tier, unlocked, store })
}
