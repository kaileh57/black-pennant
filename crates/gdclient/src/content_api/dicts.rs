//! The Dictionary shapes `BpContent` returns (`PartDefDict`, `KitDefDict`, `ClassDict`, `NodeDict` and the
//! one-method shapes), keys exactly as `shapes.rs` gives them. Static fields only where the facade caches;
//! the per-call fields (`available`, `locked_reason`, `limit`, `in_store`) are set by [`set_access`].
use super::model::{self, KitSummary};
use crate::convert::{d, empty_msg, msg_dict};
use crate::enums::GdEnum;
use content::campaign::{AceDef, CampaignData, EnemyClassDef, TheatreDef};
use content::catalogue::Catalogue;
use content::kit::{KitDef, KitKind};
use content::part::PartDef;
use godot::prelude::*;
use sim_systems::protocol::Msg;

fn sn(s: &str) -> StringName { StringName::from(s) }
fn gs(s: &str) -> GString { GString::from(s) }
fn v2(p: [f64; 2]) -> Vector2 { Vector2::new(p[0] as f32, p[1] as f32) }
fn poly(points: &[[f64; 2]]) -> PackedVector2Array { points.iter().map(|p| v2(*p)).collect() }
fn polys(list: &[Vec<[f64; 2]>]) -> VarArray { list.iter().map(|p| poly(p).to_variant()).collect() }
fn finite(v: f64) -> f64 { if v.is_finite() { v } else { 0.0 } }

/// `PartDefDict` with its static fields; `available` true, no reason, no limit, none in store until
/// [`set_access`] says otherwise.
pub(crate) fn part_def_dict(def: &PartDef, cat: &Catalogue) -> VarDictionary {
    let tags: PackedStringArray = def.tags.iter().map(|t| gs(model::tag_name(*t))).collect();
    let stats: VarArray = model::stat_rows(def, cat).iter().map(|r| d! { "label" => sn(r.label), "value" => r.value, "unit" => sn(r.unit) }.to_variant()).collect();
    let icon = if def.icon.as_str().is_empty() { model::category_icon(def.category) } else { def.icon.as_str().to_string() };
    d! {
        "id" => sn(def.id.as_str()),
        "name" => sn(def.name.as_str()),
        "description" => sn(def.description.as_str()),
        "category" => def.category.to_gd(),
        "tier" => def.tier.to_gd(),
        "size" => Vector3i::new(def.size[0] as i32, def.size[1] as i32, def.size[2] as i32),
        "mass_kg" => finite(def.mass_kg),
        "cost" => def.cost as i64,
        "power_kw" => finite(model::power_kw(def)),
        "placement" => sn(model::placement_name(def.placement)),
        "tags" => tags,
        "icon" => sn(&icon),
        "available" => true,
        "locked_reason" => empty_msg(),
        "limit" => 0i64,
        "in_store" => 0i64,
        "stats" => stats,
    }
}

/// Fill the per-call fields of a `PartDefDict` or `KitDefDict`.
pub(crate) fn set_access(dict: &mut VarDictionary, access: &Result<(), Msg>) {
    dict.set("available", access.is_ok());
    dict.set("locked_reason", &match access { Ok(()) => empty_msg(), Err(m) => msg_dict(m) });
}

/// `KitDefDict` with its static fields.
pub(crate) fn kit_def_dict(kit: &KitDef, s: &KitSummary, stock: bool) -> VarDictionary {
    let parts: VarArray = kit
        .parts
        .iter()
        .map(|p| d! { "part" => sn(p.part.as_str()), "offset" => Vector3i::new(p.offset.x as i32, p.offset.y as i32, p.offset.z as i32), "yaw" => p.rot.yaw as i64, "tilt" => p.rot.tilt as i64 }.to_variant())
        .collect();
    let settings: VarArray = kit
        .settings
        .iter()
        .map(|k| {
            let (lo, hi) = if k.min <= k.max { (k.min, k.max) } else { (k.max, k.min) };
            d! { "key" => gs(&k.key), "label" => sn(k.label.as_str()), "min" => finite(lo), "max" => finite(hi), "default" => finite(k.default).clamp(finite(lo), finite(hi)) }.to_variant()
        })
        .collect();
    let name = match &kit.name_key { Some(k) if !k.as_str().is_empty() => k.as_str().to_string(), _ => kit.name.clone() };
    let description = kit.description_key.as_ref().map_or("", |k| k.as_str());
    d! {
        "id" => sn(kit.id.as_str()),
        "name" => sn(&name),
        "description" => sn(description),
        "version" => kit.version as i64,
        "author" => gs(kit.author.as_deref().unwrap_or("")),
        "kind" => sn(match kit.kind { KitKind::Assembly => "assembly", KitKind::Officer => "officer" }),
        "parts" => parts,
        "size" => Vector3i::new(s.size[0], s.size[1], s.size[2]),
        "cost" => s.cost.min(i64::MAX as u64) as i64,
        "mass_kg" => finite(s.mass_kg),
        "tier" => s.tier.to_gd(),
        "available" => true,
        "locked_reason" => empty_msg(),
        "settings" => settings,
        "stock" => stock,
        "icon" => sn(&s.icon),
    }
}

/// `ClassDict`. `length_m` and `tonnage_t` come from the class blueprint; 0 when it does not load.
pub(crate) fn class_dict(c: &EnemyClassDef, stats: Option<&content::stats::BlueprintStats>) -> VarDictionary {
    d! {
        "id" => sn(c.id.as_str()),
        "name" => sn(c.name.as_str()),
        "role" => sn(model::role_key(c.role)),
        "tier" => c.tier.to_gd(),
        "length_m" => stats.map_or(0.0, |s| finite(s.length_m)),
        "tonnage_t" => stats.map_or(0.0, |s| finite(s.tonnage_t)),
        "blueprint" => gs(&model::normalize_stock(&c.blueprint)),
    }
}

/// The seven aces: `id`, `name`, `callsign`, `ship_class`, `act` (the act of the first appearance, else
/// the act of the ace's ship class).
pub(crate) fn ace_dict(a: &AceDef, data: &CampaignData) -> VarDictionary {
    let act = a.arcs.iter().map(|arc| arc.act).min().or_else(|| data.classes.get(&a.ship).map(|c| c.act)).unwrap_or_default();
    d! {
        "id" => sn(a.id.as_str()),
        "name" => sn(a.name.as_str()),
        "callsign" => sn(a.callsign.as_str()),
        "ship_class" => sn(a.ship.as_str()),
        "act" => act.to_gd(),
    }
}

/// The static theatre graph (see `BpContent::theatre_graph`).
pub(crate) fn theatre_graph_dict(t: &TheatreDef, outlines: &[Vec<[f64; 2]>], land: &[Vec<[f64; 2]>], shallows: &[Vec<[f64; 2]>]) -> VarDictionary {
    let lanes = super::chart::lanes(t);
    let nodes: VarArray = t
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let mut next: Vec<&str> = lanes.iter().filter_map(|&(a, b, _)| if a == i { Some(t.nodes[b].id.as_str()) } else if b == i { Some(t.nodes[a].id.as_str()) } else { None }).collect();
            next.sort_unstable();
            let next: Array<StringName> = next.into_iter().map(sn).collect();
            d! {
                "id" => sn(n.id.as_str()),
                "name" => sn(n.name.as_str()),
                "pos" => v2([finite(n.pos_km[0]), finite(n.pos_km[1])]),
                "region" => sn(n.region.as_str()),
                "kind" => sn(model::node_kind(&n.kind)),
                "lanes" => next,
            }
            .to_variant()
        })
        .collect();
    let lane_list: VarArray = lanes.iter().map(|&(a, b, km)| d! { "a" => sn(t.nodes[a].id.as_str()), "b" => sn(t.nodes[b].id.as_str()), "length_km" => km }.to_variant()).collect();
    let regions: VarArray = t
        .regions
        .iter()
        .enumerate()
        .map(|(i, r)| d! { "id" => sn(r.id.as_str()), "name" => sn(r.name.as_str()), "outline" => poly(outlines.get(i).map_or(&[][..], |o| o.as_slice())) }.to_variant())
        .collect();
    d! {
        "size_km" => v2(super::chart::chart_size(t)),
        "nodes" => nodes,
        "lanes" => lane_list,
        "regions" => regions,
        "land" => polys(land),
        "shallows" => polys(shallows),
    }
}
