//! Pure-Rust half of `gd-content`: filters, availability and its reasons, spec-sheet rows, kit summaries,
//! mission needs, mode limits and slider ranges. No Godot type appears here, so every rule is unit-tested
//! headless; `dicts.rs` turns the results into the shapes of `shapes.rs`.
use crate::text::Lookup;
use content::campaign::{CampaignData, ConstraintTag, MissionType, OperationKind};
use content::catalogue::{Catalogue, BUILD_PHASE};
use content::enums::{Act, GameMode, NetworkKind, Phase, Tier};
use content::ids::PartId;
use content::kit::{KitDef, KitKind, PartRot};
use content::part::{PartCategory, PartDef, PartFunction, PartTag, Placement, PortRole, SensorKind};
use content::ruleset::{Ruleset, RulesetKind};
use sim_systems::protocol::{Msg, MsgArg};
use std::collections::{BTreeMap, BTreeSet};

// ---------------------------------------------------------------------------------------------------------
// Names of enumerations that cross as text (tags, placement, kinds)
// ---------------------------------------------------------------------------------------------------------

/// Every part tag with the snake_case word GDScript sees in `tags` and passes as `filter.tag`.
pub(crate) const TAGS: [(PartTag, &str); 14] = [
    (PartTag::Crane, "crane"),
    (PartTag::MineRail, "mine_rail"),
    (PartTag::RhibDavit, "rhib_davit"),
    (PartTag::BoardingKit, "boarding_kit"),
    (PartTag::RecoveryNet, "recovery_net"),
    (PartTag::CameraMast, "camera_mast"),
    (PartTag::PassengerBerths, "passenger_berths"),
    (PartTag::PointDefence, "point_defence"),
    (PartTag::Magazine, "magazine"),
    (PartTag::FuelStore, "fuel_store"),
    (PartTag::Propulsion, "propulsion"),
    (PartTag::Steering, "steering"),
    (PartTag::Generation, "generation"),
    (PartTag::LowSignature, "low_signature"),
];

pub(crate) fn tag_name(tag: PartTag) -> &'static str { TAGS.iter().find(|(t, _)| *t == tag).map_or("", |(_, n)| n) }
pub(crate) fn tag_from_name(name: &str) -> Option<PartTag> { TAGS.iter().find(|(_, n)| *n == name).map(|(t, _)| *t) }

/// `PartDefDict.placement`. DECISION: the shape lists `anywhere`, `weather`, `keel` and `internal`;
/// `Underwater` (touching the outer bottom below the waterline) is the keel, and `DeckEdge`, which the shape
/// does not name, crosses as `deck_edge`.
pub(crate) fn placement_name(p: Placement) -> &'static str {
    match p {
        Placement::Anywhere => "anywhere",
        Placement::Weather => "weather",
        Placement::Internal => "internal",
        Placement::Underwater => "keel",
        Placement::DeckEdge => "deck_edge",
    }
}

pub(crate) const CATEGORIES: [PartCategory; 14] = [
    PartCategory::Hull, PartCategory::Propulsion, PartCategory::Power, PartCategory::Fuel, PartCategory::Control, PartCategory::Gun, PartCategory::Missile,
    PartCategory::Underwater, PartCategory::Defence, PartCategory::Offboard, PartCategory::Sensor, PartCategory::Crew, PartCategory::DamageControl, PartCategory::SpecialOps,
];
const CATEGORY_WORDS: [&str; 14] = ["hull", "propulsion", "power", "fuel", "control", "gun", "missile", "underwater", "defence", "offboard", "sensor", "crew", "damage_control", "special_ops"];

fn category_word(c: PartCategory) -> &'static str { CATEGORIES.iter().position(|x| *x == c).map_or("hull", |i| CATEGORY_WORDS[i]) }
/// Tray label of a category (the shared `units.word.category.*` words).
pub(crate) fn category_label(c: PartCategory) -> String { format!("units.word.category.{}", category_word(c)) }
/// Icon of a category: the hand-drawn `part_*` set in `game/ui/icons/`.
pub(crate) fn category_icon(c: PartCategory) -> String { format!("part_{}", category_word(c)) }

const TIERS: [Tier; 5] = [Tier::T1, Tier::T2, Tier::T3, Tier::T4, Tier::T5];
fn tier_number(t: Tier) -> usize { TIERS.iter().position(|x| *x == t).unwrap_or(0) + 1 }
fn act_number(a: Act) -> u8 { a.number() }

/// The act that adds a part of this phase in the campaign (`None` = present from the start), as
/// `Catalogue::available_parts` reads it.
pub(crate) fn phase_act(phase: Phase) -> Option<Act> {
    match phase {
        Phase::Act2 => Some(Act::Act2),
        Phase::Act3 => Some(Act::Act3),
        Phase::Act4 => Some(Act::Act4),
        Phase::Slice | Phase::R010 | Phase::R100 | Phase::L4 => None,
    }
}

/// The act a release phase belongs to for the `act` filter outside the campaign: first-release and slice
/// parts are Act 1, later acts their own, and 1.0 and scripting parts Act 4.
fn phase_list_act(phase: Phase) -> Act {
    match phase {
        Phase::Slice | Phase::R010 => Act::Act1,
        Phase::Act2 => Act::Act2,
        Phase::Act3 => Act::Act3,
        Phase::Act4 | Phase::R100 | Phase::L4 => Act::Act4,
    }
}

/// First release or 1.0 player limits: this build carries every phase up to [`BUILD_PHASE`].
pub(crate) fn at_1_0() -> bool { BUILD_PHASE >= Phase::R100 }

// ---------------------------------------------------------------------------------------------------------
// Filters and availability
// ---------------------------------------------------------------------------------------------------------

/// The `filter` Dictionary of `parts()` and `kits()`, read leniently: a missing key, a wrong type or an
/// out-of-range enumeration (`Bp.NONE` included) means "no constraint" on that key. An unknown tag word
/// matches nothing, since the caller asked for something that does not exist.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Filter {
    pub mode: Option<GameMode>,
    pub act: Option<Act>,
    pub category: Option<PartCategory>,
    pub tier: Option<Tier>,
    /// `Some(None)` = a tag word was given but names no tag.
    pub tag: Option<Option<PartTag>>,
    /// Lower-cased, trimmed; empty = no search.
    pub search: String,
    pub ruleset: Option<String>,
}

impl Filter {
    pub fn mode(&self) -> GameMode { self.mode.unwrap_or(GameMode::Sandbox) }
    pub fn campaign(&self) -> bool { self.mode() == GameMode::Campaign }
    fn tag_ok(&self, tags: &BTreeSet<PartTag>) -> bool {
        match self.tag { None => true, Some(None) => false, Some(Some(t)) => tags.contains(&t) }
    }
    fn search_ok(&self, translated: &str) -> bool { self.search.is_empty() || translated.to_lowercase().contains(&self.search) }
}

/// The save's part state as `Campaign.unlocks()` reports it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Unlocks {
    pub max_tier: Tier,
    pub unlocked: BTreeSet<PartId>,
    pub store: BTreeMap<PartId, u16>,
}

/// Why a part cannot be placed in the requested mode, or `Ok` when it can.
pub(crate) fn part_access(def: &PartDef, campaign: bool, act: Act, unlocks: Option<&Unlocks>) -> Result<(), Msg> {
    if def.phase > BUILD_PHASE { return Err(Msg::new("content.locked.not_in_build")); }
    if def.replaced_by.is_some() { return Err(Msg::new("content.locked.retired")); }
    if !campaign { return Ok(()); }
    let Some(u) = unlocks else { return Err(Msg::new("content.locked.no_campaign")) };
    if let Some(a) = phase_act(def.phase).filter(|a| *a > act) {
        return Err(Msg::new("content.locked.act").arg(MsgArg::Key(content::ids::TextKey::new(format!("units.word.act.{}", act_number(a))))));
    }
    if def.tier > u.max_tier {
        return Err(Msg::new("content.locked.tier").arg(MsgArg::Key(content::ids::TextKey::new(format!("units.word.tier.{}", tier_number(def.tier))))));
    }
    if !u.unlocked.contains(&def.id) { return Err(Msg::new("content.locked.blueprint")); }
    Ok(())
}

/// Whether a part is ever listed: in this build and not retired. Locked campaign parts are listed (greyed
/// with their reason), so the player sees what is coming.
pub(crate) fn listed(def: &PartDef) -> bool { def.phase <= BUILD_PHASE && def.replaced_by.is_none() }

/// Copies of a part a ruleset allows, 0 = no limit. Mirrors `content::validate`: the ruleset's own row,
/// else the part's standard limit under the Standard kind.
pub(crate) fn part_limit(ruleset: Option<&Ruleset>, def: &PartDef) -> u16 {
    let Some(r) = ruleset else { return 0 };
    r.part_limits.get(&def.id).copied().or(if r.kind == RulesetKind::Standard { def.standard_limit } else { None }).unwrap_or(0)
}

fn part_tags(def: &PartDef) -> BTreeSet<PartTag> { def.tags.iter().copied().collect() }

/// Whether a listed part passes the filter's static keys (`act`, `category`, `tier`, `tag`, `search`).
/// In the campaign `act` gates through availability instead, so here it only narrows other modes.
pub(crate) fn part_matches(def: &PartDef, f: &Filter, lk: &dyn Lookup) -> bool {
    if let Some(c) = f.category { if def.category != c { return false; } }
    if let Some(t) = f.tier { if def.tier != t { return false; } }
    if let Some(a) = f.act { if !f.campaign() && phase_list_act(def.phase) > a { return false; } }
    f.tag_ok(&part_tags(def)) && f.search_ok(&lk.tr(def.name.as_str()))
}

/// Listed parts matching `f`, by category, tier, translated name, then id.
pub(crate) fn sorted_parts<'a>(cat: &'a Catalogue, f: &Filter, lk: &dyn Lookup) -> Vec<&'a PartDef> {
    let mut out: Vec<(&PartDef, String)> = cat.parts.values().filter(|d| listed(d) && part_matches(d, f, lk)).map(|d| (d, lk.tr(d.name.as_str()).to_lowercase())).collect();
    out.sort_by(|(a, an), (b, bn)| (a.category, a.tier, an, &a.id).cmp(&(b.category, b.tier, bn, &b.id)));
    out.into_iter().map(|(d, _)| d).collect()
}

/// Listed parts per category, for the tray headings (every mode offers every listed part outside the campaign).
pub(crate) fn category_counts(cat: &Catalogue) -> Vec<(PartCategory, usize)> {
    CATEGORIES.iter().map(|c| (*c, cat.parts.values().filter(|d| listed(d) && d.category == *c).count())).collect()
}

// ---------------------------------------------------------------------------------------------------------
// Spec-sheet rows
// ---------------------------------------------------------------------------------------------------------

/// One spec-sheet row: a `content.stat.*` label, a value, and a `text::UNIT_NAMES` unit. Rates whose unit
/// has no `Unit` name carry it in the label and use `none`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StatRow { pub label: &'static str, pub value: f64, pub unit: &'static str }

fn row(label: &'static str, value: f64, unit: &'static str) -> StatRow { StatRow { label, value: if value.is_finite() { value } else { 0.0 }, unit } }

/// Power demand in kW at full output: consumers positive, generators and batteries negative.
pub(crate) fn power_kw(def: &PartDef) -> f64 {
    let mut kw = 0.0;
    for p in def.ports.iter().filter(|p| p.network == NetworkKind::Power) {
        let rate = if p.rate.is_finite() { p.rate.max(0.0) } else { 0.0 };
        match p.role { PortRole::Consume => kw += rate, PortRole::Supply | PortRole::Store => kw -= rate, PortRole::Link => {} }
    }
    if kw == 0.0 {
        // A part whose ports omit the rate still says what it does through its function.
        match &def.function {
            PartFunction::Generator { kw: g, .. } if g.is_finite() => kw = -g.max(0.0),
            PartFunction::Battery { max_kw, .. } if max_kw.is_finite() => kw = -max_kw.max(0.0),
            _ => {}
        }
    }
    kw
}

/// The rows the part's spec sheet shows, in reading order. Every row is a number the simulation uses.
pub(crate) fn stat_rows(def: &PartDef, cat: &Catalogue) -> Vec<StatRow> {
    let mut r = Vec::new();
    let munition = |id: &content::ids::MunitionId, r: &mut Vec<StatRow>| {
        if let Some(m) = cat.munition(id) {
            r.push(row("content.stat.weapon_range", m.range_m, "m"));
            if m.speed_mps > 0.0 { r.push(row("content.stat.weapon_speed", crate::convert::mps_to_kn(m.speed_mps), "kn")); }
        }
    };
    match &def.function {
        PartFunction::Structure | PartFunction::Console | PartFunction::Director | PartFunction::RhibDavit | PartFunction::RecoveryNet | PartFunction::Crane | PartFunction::Door => {}
        PartFunction::Ballast { max_kg } => r.push(row("content.stat.ballast", max_kg / 1000.0, "t")),
        PartFunction::Engine(e) => {
            r.push(row("content.stat.shaft_power", e.shaft_kw, "kw"));
            if e.fuel_lps > 0.0 { r.push(row("content.stat.fuel_burn", e.fuel_lps, "none")); }
            r.push(row("content.stat.spool_time", e.spool_s, "s"));
        }
        PartFunction::Generator { kw, fuel_lps } => {
            r.push(row("content.stat.output", *kw, "kw"));
            r.push(row("content.stat.fuel_burn", *fuel_lps, "none"));
        }
        PartFunction::Battery { kwh, max_kw } => {
            r.push(row("content.stat.stored_energy", *kwh, "none"));
            r.push(row("content.stat.max_output", *max_kw, "kw"));
        }
        PartFunction::FuelTank { litres } => r.push(row("content.stat.fuel_capacity", *litres, "l")),
        PartFunction::Propulsor(p) => {
            r.push(row("content.stat.max_shaft_power", p.max_shaft_kw, "kw"));
            r.push(row("content.stat.static_thrust", p.thrust_n_per_kw, "none"));
            r.push(row("content.stat.cavitation_speed", p.cavitation_kn, "kn"));
            if p.steer_max_deg > 0.0 { r.push(row("content.stat.steer_angle", p.steer_max_deg, "deg")); }
        }
        PartFunction::Rudder { area_m2, max_deg, rate_dps } => {
            r.push(row("content.stat.rudder_area", *area_m2, "none"));
            r.push(row("content.stat.rudder_angle", *max_deg, "deg"));
            r.push(row("content.stat.rudder_rate", *rate_dps, "none"));
        }
        PartFunction::Gun(g) => {
            r.push(row("content.stat.calibre", g.calibre_mm as f64, "mm"));
            if g.barrels > 1 { r.push(row("content.stat.barrels", g.barrels as f64, "none")); }
            r.push(row("content.stat.rate_of_fire", g.rof_rpm, "none"));
            if let Some(b) = cat.bands.iter().find(|b| b.band == g.band) { r.push(row("content.stat.effective_range", b.cap_range_m, "m")); }
            r.push(row("content.stat.muzzle_velocity", g.muzzle_mps, "none"));
            r.push(row("content.stat.shell_mass", g.shell_kg, "none"));
            r.push(row("content.stat.ready_rounds", g.ready_rounds as f64, "none"));
            r.push(row("content.stat.train_rate", g.train_rate_dps, "none"));
            r.push(row("content.stat.elevation_rate", g.elevation_rate_dps, "none"));
            r.push(row("content.stat.elevation_min", g.elevation_min_deg, "deg"));
            r.push(row("content.stat.elevation_max", g.elevation_max_deg, "deg"));
            r.push(row("content.stat.dispersion", g.dispersion_mrad, "none"));
        }
        PartFunction::Magazine { calibre_mm, rounds, ammo_kg } => {
            r.push(row("content.stat.calibre", *calibre_mm as f64, "mm"));
            r.push(row("content.stat.rounds_stowed", *rounds as f64, "none"));
            r.push(row("content.stat.ammo_mass", *ammo_kg, "none"));
        }
        PartFunction::Hoist { rounds_per_min } => r.push(row("content.stat.hoist_rate", *rounds_per_min, "none")),
        PartFunction::Launcher(l) => {
            r.push(row("content.stat.cells", l.cells as f64, "none"));
            munition(&l.munition, &mut r);
            r.push(row("content.stat.launch_interval", l.interval_s, "s"));
            if l.reload_s > 0.0 { r.push(row("content.stat.reload_time", l.reload_s, "s")); }
            if let Some(t) = l.train_rate_dps { r.push(row("content.stat.train_rate", t, "none")); }
        }
        PartFunction::MineRail { capacity, .. } => r.push(row("content.stat.mines", *capacity as f64, "none")),
        PartFunction::Decoy(d) => {
            if d.kind != content::part::DecoyKind::TowedDecoy {
                r.push(row("content.stat.decoy_rounds", d.rounds as f64, "none"));
                r.push(row("content.stat.salvo", d.salvo as f64, "none"));
                r.push(row("content.stat.salvo_interval", d.reload_s, "s"));
            }
        }
        PartFunction::Jammer { r_bt0_m } => r.push(row("content.stat.burn_through", *r_bt0_m, "m")),
        PartFunction::Sensor(s) => {
            if !matches!(s.kind, SensorKind::Esm | SensorKind::Visual) && s.r_ref_m > 0.0 { r.push(row("content.stat.reference_range", s.r_ref_m, "m")); }
            r.push(row("content.stat.update_rate", s.rate_hz, "none"));
            if let Some(rpm) = s.rpm { r.push(row("content.stat.rotation", rpm, "none")); }
            r.push(row("content.stat.field_of_view", (2.0 * s.fov_half_deg).min(360.0), "deg"));
        }
        PartFunction::Pump { m3ps } => r.push(row("content.stat.pump_rate", *m3ps, "none")),
        PartFunction::Berths { count } => r.push(row("content.stat.berths", *count as f64, "none")),
        PartFunction::PassengerBerths { count } => r.push(row("content.stat.passenger_berths", *count as f64, "none")),
        PartFunction::DroneBay { drone, count, recharge_s } => {
            r.push(row("content.stat.drones", *count as f64, "none"));
            munition(drone, &mut r);
            r.push(row("content.stat.recharge_time", *recharge_s, "s"));
        }
        PartFunction::HeliDeck { helicopter } => munition(helicopter, &mut r),
        PartFunction::BoardingKit { team_berths } => r.push(row("content.stat.team_berths", *team_berths as f64, "none")),
        PartFunction::CameraMast { fov_deg, photo_range_m } => {
            r.push(row("content.stat.field_of_view", *fov_deg, "deg"));
            r.push(row("content.stat.photo_range", *photo_range_m, "m"));
        }
    }
    if def.power_priority > 0 { r.push(row("content.stat.power_priority", def.power_priority as f64, "none")); }
    r
}

/// Every `content.stat.*` label [`stat_rows`] can emit.
pub(crate) const STAT_KEYS: [&str; 51] = [
    "content.stat.weapon_range", "content.stat.weapon_speed", "content.stat.ballast", "content.stat.shaft_power", "content.stat.fuel_burn",
    "content.stat.spool_time", "content.stat.output", "content.stat.stored_energy", "content.stat.max_output", "content.stat.fuel_capacity",
    "content.stat.max_shaft_power", "content.stat.static_thrust", "content.stat.cavitation_speed", "content.stat.steer_angle", "content.stat.rudder_area",
    "content.stat.rudder_angle", "content.stat.rudder_rate", "content.stat.calibre", "content.stat.barrels", "content.stat.rate_of_fire",
    "content.stat.effective_range", "content.stat.muzzle_velocity", "content.stat.shell_mass", "content.stat.ready_rounds", "content.stat.train_rate",
    "content.stat.elevation_rate", "content.stat.elevation_min", "content.stat.elevation_max", "content.stat.dispersion", "content.stat.rounds_stowed",
    "content.stat.ammo_mass", "content.stat.hoist_rate", "content.stat.cells", "content.stat.launch_interval", "content.stat.reload_time",
    "content.stat.mines", "content.stat.decoy_rounds", "content.stat.salvo", "content.stat.salvo_interval", "content.stat.burn_through",
    "content.stat.reference_range", "content.stat.update_rate", "content.stat.rotation", "content.stat.field_of_view", "content.stat.pump_rate",
    "content.stat.berths", "content.stat.passenger_berths", "content.stat.drones", "content.stat.recharge_time", "content.stat.team_berths",
    "content.stat.photo_range",
];

// ---------------------------------------------------------------------------------------------------------
// Kits
// ---------------------------------------------------------------------------------------------------------

/// What a kit adds up to: footprint, price, mass, highest tier, the category and icon it shows under and
/// the tags it satisfies (its own plus its parts').
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct KitSummary {
    pub size: [i32; 3],
    pub cost: u64,
    pub mass_kg: f64,
    pub tier: Tier,
    pub category: PartCategory,
    pub icon: String,
    pub tags: BTreeSet<PartTag>,
    /// Parts the catalogue does not know (a player kit from a newer build or a broken file).
    pub unknown_parts: usize,
    /// Latest act any of its parts belongs to, for the `act` filter outside the campaign.
    pub list_act: Act,
}

/// DECISION: a kit's main part is its most expensive part (ties: the first listed), so a gun kit files under
/// guns and a radar mast under sensors, not under the mast structure. Officer kits carry no parts and file
/// under Control with its icon.
pub(crate) fn kit_summary(kit: &KitDef, cat: &Catalogue) -> KitSummary {
    let cells = content::kit::kit_footprint(kit, cat, content::cell::CellPos::new(0, 0, 0), PartRot::default(), false);
    let size = match (cells.iter().map(|c| [c.x, c.y, c.z]).reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])]), cells.iter().map(|c| [c.x, c.y, c.z]).reduce(|a, b| [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])])) {
        (Some(lo), Some(hi)) => [0, 1, 2].map(|i| hi[i] as i32 - lo[i] as i32 + 1),
        _ => [0, 0, 0],
    };
    let mut cost = 0u64;
    let mut mass = 0.0;
    let mut tier = Tier::T1;
    let mut tags: BTreeSet<PartTag> = kit.tags.iter().copied().collect();
    let mut main: Option<&PartDef> = None;
    let mut unknown = 0;
    let mut list_act = Act::Act1;
    for kp in &kit.parts {
        let Some(def) = cat.part(&kp.part) else { unknown += 1; continue };
        cost += def.cost as u64;
        if def.mass_kg.is_finite() { mass += def.mass_kg.max(0.0); }
        tier = tier.max(def.tier);
        list_act = list_act.max(phase_list_act(def.phase));
        tags.extend(def.tags.iter().copied());
        if main.is_none_or(|m| def.cost > m.cost) { main = Some(def); }
    }
    let category = main.map_or(PartCategory::Control, |m| m.category);
    let icon = main.map(|m| m.icon.as_str()).filter(|s| !s.is_empty()).map_or_else(|| category_icon(category), str::to_string);
    KitSummary { size, cost, mass_kg: mass, tier, category, icon, tags, unknown_parts: unknown, list_act }
}

/// A kit's availability: every part must be placeable; the first refusal is the kit's reason.
pub(crate) fn kit_access(kit: &KitDef, cat: &Catalogue, campaign: bool, act: Act, unlocks: Option<&Unlocks>) -> Result<(), Msg> {
    if kit.kind == KitKind::Officer { return Ok(()); }
    for kp in &kit.parts {
        let Some(def) = cat.part(&kp.part) else { return Err(Msg::new("content.locked.unknown_part")) };
        part_access(def, campaign, act, unlocks)?;
    }
    Ok(())
}

/// Translated display name of a kit: stock kits through their key, player kits as typed.
pub(crate) fn kit_display_name(kit: &KitDef, lk: &dyn Lookup) -> String {
    match &kit.name_key { Some(k) if !k.as_str().is_empty() => lk.tr(k.as_str()), _ => kit.name.clone() }
}

/// Whether a kit passes the filter's static keys; `category` is that of the kit's main part.
pub(crate) fn kit_matches(kit: &KitDef, s: &KitSummary, f: &Filter, lk: &dyn Lookup) -> bool {
    if let Some(c) = f.category { if s.category != c { return false; } }
    if let Some(t) = f.tier { if s.tier != t { return false; } }
    if let Some(a) = f.act { if !f.campaign() && s.list_act > a { return false; } }
    f.tag_ok(&s.tags) && f.search_ok(&kit_display_name(kit, lk))
}

// ---------------------------------------------------------------------------------------------------------
// Missions, modes, difficulty
// ---------------------------------------------------------------------------------------------------------

/// Snake_case word of a template (the `units.word.mission.*` family and `content.mission.*` keys).
pub(crate) fn mission_word(m: MissionType) -> &'static str {
    match m {
        MissionType::ConvoyRaid => "convoy_raid",
        MissionType::DepotStrike => "depot_strike",
        MissionType::RadarDecapitation => "radar_decapitation",
        MissionType::AgentExtraction => "agent_extraction",
        MissionType::Minelaying => "minelaying",
        MissionType::PrizeCapture => "prize_capture",
        MissionType::DefectorEscort => "defector_escort",
        MissionType::Feint => "feint",
        MissionType::PhotoReconnaissance => "photo_reconnaissance",
        MissionType::HunterGroupSurvival => "hunter_group_survival",
        MissionType::CuttingOut => "cutting_out",
    }
}

/// Part tags a hull needs for a template (L797): extraction needs a RHIB davit and passenger berths,
/// minelaying mine rails, prize capture and cutting out a boarding kit, defector escort passenger berths,
/// photo reconnaissance a camera mast; the rest use weapons, emission control and drones, which every hull
/// can carry. To that the campaign data adds any `Requires` tag that every operation of the template
/// carries (a tag only some variants carry is a briefing detail, not a need).
pub(crate) fn mission_needs(m: MissionType, data: Option<&CampaignData>) -> Vec<PartTag> {
    let mut need: BTreeSet<PartTag> = match m {
        MissionType::AgentExtraction => [PartTag::RhibDavit, PartTag::PassengerBerths].into(),
        MissionType::Minelaying => [PartTag::MineRail].into(),
        MissionType::PrizeCapture | MissionType::CuttingOut => [PartTag::BoardingKit].into(),
        MissionType::DefectorEscort => [PartTag::PassengerBerths].into(),
        MissionType::PhotoReconnaissance => [PartTag::CameraMast].into(),
        MissionType::ConvoyRaid | MissionType::DepotStrike | MissionType::RadarDecapitation | MissionType::Feint | MissionType::HunterGroupSurvival => BTreeSet::new(),
    };
    if let Some(d) = data {
        let mut common: Option<BTreeSet<PartTag>> = None;
        for op in d.operations.values().filter(|o| o.kind == OperationKind::Template(m)) {
            let tags: BTreeSet<PartTag> = op.tags.iter().filter_map(|t| match t { ConstraintTag::Requires(p) => Some(*p), _ => None }).collect();
            common = Some(match common { None => tags, Some(c) => c.intersection(&tags).copied().collect() });
        }
        need.extend(common.unwrap_or_default());
    }
    need.into_iter().collect()
}

/// `(max_players, team_sizes)` of a mode in this build. PvP team sizes are players per side (1v1 and 2v2 at
/// the first release, up to 4v4 by 1.0; escort only by 1.0); co-op modes put everyone on one side, so their
/// sizes run from 1 to the mode's maximum. A mode this build does not offer has `(0, [])`.
pub(crate) fn mode_limits(mode: GameMode, at_1_0: bool) -> (u8, Vec<u8>) {
    let max = content::ruleset::max_players(mode, at_1_0);
    match mode {
        GameMode::PvpEscort if !at_1_0 => (0, Vec::new()),
        GameMode::PvpElimination => (max, (1..=if at_1_0 { 4 } else { 2 }).collect()),
        GameMode::PvpEscort => (max, (1..=4).collect()),
        _ => (max, (1..=max.max(1)).collect()),
    }
}

/// One difficulty slider: key, min, max, step, locks at campaign start (L851-860). Hull count locks
/// because it sizes the theatre; Ironman locking the rest is the campaign's rule, not the range's.
pub(crate) struct SliderRange { pub key: &'static str, pub min: f64, pub max: f64, pub step: f64, pub locks_at_start: bool }
/// DECISION: steps give each slider 16 to 35 stops: 0.05 for the multipliers, 0.1 s for reaction time, 10
/// hulls, 1 day.
pub(crate) const DIFFICULTY_RANGES: [SliderRange; 6] = [
    SliderRange { key: "ai_accuracy", min: 0.6, max: 2.0, step: 0.05, locks_at_start: false },
    SliderRange { key: "ai_reaction_s", min: 0.8, max: 4.0, step: 0.1, locks_at_start: false },
    SliderRange { key: "adaptation_cadence", min: 0.25, max: 2.0, step: 0.05, locks_at_start: false },
    SliderRange { key: "hull_count", min: 200.0, max: 450.0, step: 10.0, locks_at_start: true },
    SliderRange { key: "economy", min: 0.7, max: 1.5, step: 0.05, locks_at_start: false },
    SliderRange { key: "clock_days", min: 28.0, max: 60.0, step: 1.0, locks_at_start: false },
];

/// Presets in play order: Normal first, then the rest as the data lists them.
pub(crate) fn presets_in_order(presets: &[content::campaign::DifficultyPreset]) -> Vec<&content::campaign::DifficultyPreset> {
    let mut out: Vec<&content::campaign::DifficultyPreset> = presets.iter().collect();
    out.sort_by_key(|p| p.id.as_str() != "normal");
    out
}

/// Lobby rulesets from the embedded table, `standard` first, each id once.
pub(crate) fn rulesets_in_order(mut list: Vec<(String, Ruleset)>) -> Vec<(String, Ruleset)> {
    let mut seen = BTreeSet::new();
    list.retain(|(id, _)| seen.insert(id.clone()));
    list.sort_by_key(|(id, _)| id != "standard");
    list
}

/// Starter paths offered at an act (from this act on, L142-151): that act's own first, then earlier acts'
/// newest first, each once. `None` = every act's starters in act order (sandbox, skirmish, PvP).
pub(crate) fn starter_paths(acts: &[content::campaign::ActDef], act: Option<Act>) -> Vec<String> {
    let mut list: Vec<&content::campaign::ActDef> = acts.iter().filter(|a| act.is_none_or(|x| a.act <= x)).collect();
    if act.is_some() { list.sort_by(|a, b| b.act.cmp(&a.act)); } else { list.sort_by_key(|a| a.act); }
    let mut seen = BTreeSet::new();
    list.iter().flat_map(|a| a.starters.iter()).filter(|p| seen.insert(normalize_stock(p))).cloned().collect()
}

/// The acts that have a row in `acts`, in act order: the acts this build ships when `acts` is the shipped
/// campaign's (Act 1 alone in 0.1.0). Never empty: Act 1 when no row is present.
pub(crate) fn shipped_acts(acts: &[content::campaign::ActDef]) -> Vec<Act> {
    let mut out: Vec<Act> = acts.iter().map(|a| a.act).collect();
    out.sort();
    out.dedup();
    if out.is_empty() { out.push(Act::Act1); }
    out
}

/// Stock path form: forward slashes, no leading `./` or `blueprints/` (as `content` keys them).
pub(crate) fn normalize_stock(path: &str) -> String {
    let p = path.trim().replace('\\', "/");
    let p = p.trim_start_matches("./");
    p.strip_prefix("blueprints/").unwrap_or(p).to_string()
}

/// Role word key of an enemy class.
pub(crate) fn role_key(role: content::campaign::ClassRole) -> &'static str {
    use content::campaign::ClassRole as R;
    match role {
        R::Submarine => "content.role.submarine",
        R::PatrolBoat => "content.role.patrol_boat",
        R::FastAttack => "content.role.fast_attack",
        R::Corvette => "content.role.corvette",
        R::Frigate => "content.role.frigate",
        R::Destroyer => "content.role.destroyer",
        R::Capital => "content.role.capital",
        R::Picket => "content.role.picket",
        R::Minesweeper => "content.role.minesweeper",
        R::Freighter => "content.role.freighter",
        R::Tanker => "content.role.tanker",
        R::Tender => "content.role.tender",
        R::Civilian => "content.role.civilian",
        R::Drone => "content.role.drone",
    }
}

/// Theatre node kind word of `NodeDict.kind`.
pub(crate) fn node_kind(kind: &content::campaign::NodeKind) -> &'static str {
    use content::campaign::NodeKind as K;
    match kind {
        K::Sea => "sea",
        K::Cove => "cove",
        K::Shipyard { .. } => "shipyard",
        K::Depot { .. } => "depot",
        K::ForwardBase => "base",
        K::Harbour => "harbour",
        K::RadarSite => "radar_site",
        K::EsmSite => "esm_site",
    }
}

/// Every key this unit emits; their English is in `game/i18n/content.csv`.
pub fn text_keys() -> Vec<&'static str> {
    let mut keys = vec![
        "content.locked.not_in_build", "content.locked.retired", "content.locked.no_campaign", "content.locked.act", "content.locked.tier",
        "content.locked.blueprint", "content.locked.unknown_part", "content.stat.power_priority",
        "content.role.submarine", "content.role.patrol_boat", "content.role.fast_attack", "content.role.corvette", "content.role.frigate",
        "content.role.destroyer", "content.role.capital", "content.role.picket", "content.role.minesweeper", "content.role.freighter",
        "content.role.tanker", "content.role.tender", "content.role.civilian", "content.role.drone",
        "content.mission.convoy_raid.desc", "content.mission.depot_strike.desc", "content.mission.radar_decapitation.desc",
        "content.mission.agent_extraction.desc", "content.mission.minelaying.desc", "content.mission.prize_capture.desc",
        "content.mission.defector_escort.desc", "content.mission.feint.desc", "content.mission.photo_reconnaissance.desc",
        "content.mission.hunter_group_survival.desc", "content.mission.cutting_out.desc",
    ];
    keys.extend(STAT_KEYS);
    keys
}
