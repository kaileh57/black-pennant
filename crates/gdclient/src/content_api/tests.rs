use super::chart;
use super::model::*;
use crate::text::tests::CsvLookup;
use crate::text::Lookup;
use content::catalogue::{catalogue, Catalogue};
use content::enums::{Act, GameMode, Tier};
use content::ids::PartId;
use content::part::{PartCategory, PartTag};
use std::collections::BTreeSet;

fn cat() -> &'static Catalogue { catalogue() }

#[test]
fn tag_words_are_unique_and_round_trip() {
    let names: BTreeSet<&str> = TAGS.iter().map(|(_, n)| *n).collect();
    assert_eq!(names.len(), TAGS.len());
    for (t, n) in TAGS { assert_eq!(tag_from_name(n), Some(t)); assert_eq!(tag_name(t), n); }
    assert_eq!(tag_from_name("Crane"), None);
}

#[test]
fn availability_agrees_with_the_catalogue() {
    let c = cat();
    let ids: Vec<PartId> = c.parts.keys().cloned().collect();
    let some: Vec<PartId> = ids.iter().step_by(2).cloned().collect();
    for (max_tier, unlocked) in [(Tier::T1, &ids), (Tier::T3, &some), (Tier::T5, &ids), (Tier::T2, &Vec::new())] {
        for act in Act::ALL {
            let u = Unlocks { max_tier, unlocked: unlocked.iter().cloned().collect(), ..Default::default() };
            let want: BTreeSet<usize> = c.available_parts(GameMode::Campaign, act, max_tier, unlocked).into_iter().map(|i| i.index()).collect();
            let got: BTreeSet<usize> = c.parts.values().enumerate().filter(|(_, d)| part_access(d, true, act, Some(&u)).is_ok()).map(|(i, _)| i).collect();
            assert_eq!(got, want, "tier {max_tier:?} act {act:?}");
        }
    }
    let open: BTreeSet<usize> = c.available_parts(GameMode::Sandbox, Act::Act1, Tier::T1, &[]).into_iter().map(|i| i.index()).collect();
    let got: BTreeSet<usize> = c.parts.values().enumerate().filter(|(_, d)| part_access(d, false, Act::Act1, None).is_ok()).map(|(i, _)| i).collect();
    assert_eq!(got, open);
}

#[test]
fn locked_parts_say_why() {
    let c = cat();
    let def = c.parts.values().find(|d| d.tier > Tier::T1 && listed(d)).expect("a part above tier 1");
    let u = Unlocks { max_tier: Tier::T1, unlocked: [def.id.clone()].into(), ..Default::default() };
    let e = part_access(def, true, Act::Act4, Some(&u)).unwrap_err();
    assert_eq!(e.key.as_str(), "content.locked.tier");
    let u = Unlocks { max_tier: Tier::T5, ..Default::default() };
    assert_eq!(part_access(def, true, Act::Act4, Some(&u)).unwrap_err().key.as_str(), "content.locked.blueprint");
    assert_eq!(part_access(def, true, Act::Act4, None).unwrap_err().key.as_str(), "content.locked.no_campaign");
    assert!(part_access(def, false, Act::Act1, None).is_ok());
}

#[test]
fn parts_sort_by_category_tier_then_name_and_filter() {
    let c = cat();
    let lk = CsvLookup::load();
    let all = sorted_parts(c, &Filter::default(), &lk);
    assert_eq!(all.len(), c.parts.values().filter(|d| listed(d)).count());
    for w in all.windows(2) {
        let key = |d: &content::part::PartDef| (d.category, d.tier, lk.tr(d.name.as_str()).to_lowercase());
        assert!(key(w[0]) <= key(w[1]), "{} before {}", w[0].id.as_str(), w[1].id.as_str());
    }
    let guns = sorted_parts(c, &Filter { category: Some(PartCategory::Gun), ..Default::default() }, &lk);
    assert!(!guns.is_empty() && guns.iter().all(|d| d.category == PartCategory::Gun));
    let t1 = sorted_parts(c, &Filter { tier: Some(Tier::T1), ..Default::default() }, &lk);
    assert!(t1.iter().all(|d| d.tier == Tier::T1));
    let pd = sorted_parts(c, &Filter { tag: Some(Some(PartTag::PointDefence)), ..Default::default() }, &lk);
    assert!(!pd.is_empty() && pd.iter().all(|d| d.tags.contains(&PartTag::PointDefence)));
    assert!(sorted_parts(c, &Filter { tag: Some(None), ..Default::default() }, &lk).is_empty());
    let first = all[0];
    let word = lk.tr(first.name.as_str());
    let hits = sorted_parts(c, &Filter { search: word.to_lowercase(), ..Default::default() }, &lk);
    assert!(hits.iter().any(|d| d.id == first.id));
    assert!(sorted_parts(c, &Filter { search: "no part has this name".into(), ..Default::default() }, &lk).is_empty());
}

#[test]
fn spec_rows_use_known_units_labels_and_finite_values() {
    let c = cat();
    let keys: BTreeSet<&str> = STAT_KEYS.iter().copied().chain(["content.stat.power_priority"]).collect();
    for def in c.parts.values() {
        for r in stat_rows(def, c) {
            assert!(crate::text::UNIT_NAMES.contains(&r.unit), "{} {}", def.id.as_str(), r.unit);
            assert!(keys.contains(r.label), "{}", r.label);
            assert!(r.value.is_finite());
        }
    }
    let gun = c.parts.values().find(|d| matches!(d.function, content::part::PartFunction::Gun(_))).expect("a gun");
    let rows = stat_rows(gun, c);
    assert_eq!(rows[0].label, "content.stat.calibre");
    assert!(rows.iter().any(|r| r.label == "content.stat.effective_range" && r.value > 0.0));
}

#[test]
fn power_sign_follows_supply_and_demand() {
    let c = cat();
    for def in c.parts.values() {
        match def.function {
            content::part::PartFunction::Generator { .. } | content::part::PartFunction::Battery { .. } => assert!(power_kw(def) < 0.0, "{}", def.id.as_str()),
            content::part::PartFunction::Gun(_) if def.ports.iter().any(|p| p.network == content::enums::NetworkKind::Power && p.rate > 0.0) => assert!(power_kw(def) > 0.0),
            _ => {}
        }
    }
}

#[test]
fn limits_mirror_validation() {
    let c = cat();
    let def = c.parts.values().find(|d| d.standard_limit.is_some()).expect("a limited part");
    let standard = content::ruleset::Ruleset::default();
    let open = content::ruleset::Ruleset { kind: content::ruleset::RulesetKind::Open, ..Default::default() };
    assert_eq!(part_limit(Some(&standard), def), def.standard_limit.unwrap());
    assert_eq!(part_limit(Some(&open), def), 0);
    assert_eq!(part_limit(None, def), 0);
    let mut custom = open.clone();
    custom.part_limits.insert(def.id.clone(), 3);
    assert_eq!(part_limit(Some(&custom), def), 3);
}

#[test]
fn kits_add_up_and_file_under_their_main_part() {
    let c = cat();
    assert!(!c.kits.is_empty());
    for kit in c.kits.values() {
        let s = kit_summary(kit, c);
        let cost: u64 = kit.parts.iter().filter_map(|p| c.part(&p.part)).map(|d| d.cost as u64).sum();
        assert_eq!(s.cost, cost);
        assert_eq!(s.unknown_parts, 0);
        if !kit.parts.is_empty() {
            assert!(s.size.iter().all(|v| *v >= 1), "{} {:?}", kit.id.as_str(), s.size);
            let max = kit.parts.iter().filter_map(|p| c.part(&p.part)).map(|d| d.cost).max().unwrap();
            let main = kit.parts.iter().filter_map(|p| c.part(&p.part)).find(|d| d.cost == max).unwrap();
            assert_eq!(s.category, main.category);
            assert!(!s.icon.is_empty());
        }
        assert!(kit_access(kit, c, false, Act::Act1, None).is_ok(), "{}", kit.id.as_str());
    }
    let gun_kit = c.kits.values().find(|k| k.parts.iter().any(|p| c.part(&p.part).is_some_and(|d| d.category == PartCategory::Gun)));
    if let Some(k) = gun_kit { assert_eq!(kit_summary(k, c).category, PartCategory::Gun); }
}

#[test]
fn categories_cover_every_listed_part() {
    let c = cat();
    let counts = category_counts(c);
    assert_eq!(counts.len(), 14);
    assert_eq!(counts.iter().map(|(_, n)| n).sum::<usize>(), c.parts.values().filter(|d| listed(d)).count());
    assert_eq!(category_label(PartCategory::DamageControl), "units.word.category.damage_control");
    assert_eq!(category_icon(PartCategory::SpecialOps), "part_special_ops");
    for (cat, _) in counts {
        let icon = format!("{}/../../game/ui/icons/{}.svg", env!("CARGO_MANIFEST_DIR"), category_icon(cat));
        assert!(std::path::Path::new(&icon).exists(), "{icon}");
    }
}

#[test]
fn mode_limits_follow_the_release() {
    assert_eq!(mode_limits(GameMode::PvpElimination, false), (4, vec![1, 2]));
    assert_eq!(mode_limits(GameMode::PvpElimination, true), (8, vec![1, 2, 3, 4]));
    assert_eq!(mode_limits(GameMode::PvpEscort, false), (0, vec![]));
    assert_eq!(mode_limits(GameMode::PvpEscort, true), (8, vec![1, 2, 3, 4]));
    assert_eq!(mode_limits(GameMode::Campaign, false), (2, vec![1, 2]));
    assert_eq!(mode_limits(GameMode::Skirmish, false), (4, vec![1, 2, 3, 4]));
    assert_eq!(mode_limits(GameMode::Splash, true), (1, vec![1]));
}

#[test]
fn difficulty_ranges_match_the_spec_and_presets_fit() {
    let lo = content::campaign::DifficultySliders { ai_accuracy: -9.0, ai_reaction_s: -9.0, adaptation_cadence: -9.0, hull_count: 0, economy: -9.0, clock_days: 0 }.clamped();
    let hi = content::campaign::DifficultySliders { ai_accuracy: 99.0, ai_reaction_s: 99.0, adaptation_cadence: 99.0, hull_count: 9999, economy: 99.0, clock_days: 999 }.clamped();
    let get = |s: &content::campaign::DifficultySliders, k: &str| match k {
        "ai_accuracy" => s.ai_accuracy,
        "ai_reaction_s" => s.ai_reaction_s,
        "adaptation_cadence" => s.adaptation_cadence,
        "hull_count" => s.hull_count as f64,
        "economy" => s.economy,
        "clock_days" => s.clock_days as f64,
        _ => unreachable!(),
    };
    let presets: Vec<content::campaign::DifficultyPreset> = content::data::parse_ron(content::data::DIFFICULTY, "difficulty.ron").unwrap();
    for r in &DIFFICULTY_RANGES {
        assert_eq!((r.min, r.max), (get(&lo, r.key), get(&hi, r.key)), "{}", r.key);
        let stops = (r.max - r.min) / r.step;
        assert!((stops - stops.round()).abs() < 1e-6 && (10.0..=100.0).contains(&stops), "{} {stops}", r.key);
        for p in &presets {
            let v = get(&p.sliders, r.key);
            let k = (v - r.min) / r.step;
            assert!((k - k.round()).abs() < 1e-6, "{} preset {} is off the step grid", r.key, p.id.as_str());
        }
    }
    assert_eq!(DIFFICULTY_RANGES.iter().filter(|r| r.locks_at_start).map(|r| r.key).collect::<Vec<_>>(), ["hull_count"]);
    let order = presets_in_order(&presets);
    assert_eq!(order[0].id.as_str(), "normal");
    assert_eq!(order.len(), presets.len());
}

#[test]
fn rulesets_put_standard_first() {
    let list: Vec<(String, content::ruleset::Ruleset)> = content::data::parse_ron(content::data::RULESETS, "rulesets.ron").unwrap();
    let mut reversed = list.clone();
    reversed.reverse();
    reversed.push(reversed[0].clone());
    let out = rulesets_in_order(reversed);
    assert_eq!(out[0].0, "standard");
    assert_eq!(out.len(), list.len());
}

fn act(a: Act, starters: &[&str]) -> content::campaign::ActDef {
    content::campaign::ActDef {
        act: a, phase: content::enums::Phase::R010, name: content::ids::TextKey::new("x"), starters: starters.iter().map(|s| s.to_string()).collect(),
        start_tier: Tier::T1, tier_unlocks: vec![], regions: vec![], story_ops: vec![], ace_duels: vec![], capstone: content::ids::OperationId::new("c"),
        aces: vec![], coves: vec![], retreat_closed_cove: None, opens_open_theatre: false,
    }
}

#[test]
fn starters_follow_the_act_table() {
    let acts = vec![act(Act::Act2, &["s/corvette40.bp.ron"]), act(Act::Act1, &["s/fac22.bp.ron", "s/patrol15.bp.ron", "./blueprints/s/insertion26.bp.ron"]), act(Act::Act3, &["s/corvette60.bp.ron"])];
    assert_eq!(starter_paths(&acts, Some(Act::Act1)), ["s/fac22.bp.ron", "s/patrol15.bp.ron", "./blueprints/s/insertion26.bp.ron"]);
    assert_eq!(starter_paths(&acts, Some(Act::Act2))[0], "s/corvette40.bp.ron");
    let all = starter_paths(&acts, None);
    assert_eq!(all, ["s/fac22.bp.ron", "s/patrol15.bp.ron", "./blueprints/s/insertion26.bp.ron", "s/corvette40.bp.ron", "s/corvette60.bp.ron"]);
    assert_eq!(normalize_stock("./blueprints/s/insertion26.bp.ron"), "s/insertion26.bp.ron");
    assert_eq!(normalize_stock("s\\a.bp.ron"), "s/a.bp.ron");
}

#[test]
fn mission_needs_come_from_the_actions() {
    use content::campaign::MissionType as M;
    assert_eq!(mission_needs(M::Minelaying, None), [PartTag::MineRail]);
    assert_eq!(mission_needs(M::PhotoReconnaissance, None), [PartTag::CameraMast]);
    assert_eq!(mission_needs(M::AgentExtraction, None), [PartTag::RhibDavit, PartTag::PassengerBerths]);
    assert!(mission_needs(M::ConvoyRaid, None).is_empty());
    // A tag every operation of a template carries is a need; a tag one variant carries is not.
    let mut data = content::campaign::CampaignData::default();
    let op = |id: &str, tags: Vec<content::campaign::ConstraintTag>| content::campaign::OperationDef {
        id: content::ids::OperationId::new(id), kind: content::campaign::OperationKind::Template(M::ConvoyRaid), act: Act::Act1,
        name: content::ids::TextKey::new("n"), briefing: content::ids::TextKey::new("b"), unlock: content::campaign::Unlock::Always, node: None, map: None,
        objective: content::campaign::Objective::DestroyAll, tags, ghosts: vec![], force: vec![], per_extra_player: None, complications: vec![], approach_m: 1000.0,
        sea_states: vec![], actions: vec![], declared_hostile: vec![], recommended_emcon: None, pushback_days: None, forced: false, reference_setup: None,
    };
    use content::campaign::ConstraintTag::Requires;
    data.operations.insert(content::ids::OperationId::new("a"), op("a", vec![Requires(PartTag::Crane), Requires(PartTag::RecoveryNet)]));
    data.operations.insert(content::ids::OperationId::new("b"), op("b", vec![Requires(PartTag::Crane)]));
    assert_eq!(mission_needs(M::ConvoyRaid, Some(&data)), [PartTag::Crane]);
    for m in M::ALL { assert!(crate::text::word_key_of("mission", crate::enums::GdEnum::to_gd(m)).is_some_and(|k| k.ends_with(mission_word(m)))); }
}

#[test]
fn every_key_has_english() {
    let lk = CsvLookup::load();
    for k in text_keys() { assert!(lk.0.contains_key(k), "no English for {k}"); }
    for c in CATEGORIES { assert!(lk.0.contains_key(&category_label(c))); }
}

fn area(p: &[[f64; 2]]) -> f64 { 0.5 * (0..p.len()).map(|i| { let (a, b) = (p[i], p[(i + 1) % p.len()]); a[0] * b[1] - b[0] * a[1] }).sum::<f64>() }

#[test]
fn voronoi_cells_tile_the_chart() {
    let sites = [[100.0, 100.0], [500.0, 120.0], [300.0, 300.0], [120.0, 350.0], [450.0, 380.0], [300.0, 300.0]];
    let cells = chart::voronoi_cells(&sites, 600.0, 400.0);
    let total: f64 = cells.iter().map(|c| area(c).abs()).sum();
    assert!((total - 240_000.0).abs() < 1e-6, "{total}");
    assert!(cells[5].is_empty(), "a duplicate site owns nothing");
    // Every cell holds its own site.
    for (i, c) in cells.iter().enumerate().take(5) {
        let s = sites[i];
        let inside = (0..c.len()).all(|k| { let (a, b) = (c[k], c[(k + 1) % c.len()]); (b[0] - a[0]) * (s[1] - a[1]) - (b[1] - a[1]) * (s[0] - a[0]) >= -1e-9 });
        assert!(inside, "site {i}");
    }
}

#[test]
fn union_of_two_squares_is_one_rectangle() {
    let a = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let b = vec![[1.0, 0.0], [2.0, 0.0], [2.0, 1.0], [1.0, 1.0]];
    let out = chart::union_outline(&[&a, &b]);
    assert_eq!(out.len(), 4, "{out:?}");
    assert!((area(&out).abs() - 2.0).abs() < 1e-9);
    // A cell edge that spans two neighbours' edges still merges (T-junction).
    let c = vec![[0.0, 1.0], [2.0, 1.0], [2.0, 2.0], [0.0, 2.0]];
    let out = chart::union_outline(&[&a, &b, &c]);
    assert_eq!(out.len(), 4, "{out:?}");
    assert!((area(&out).abs() - 4.0).abs() < 1e-9);
}

fn node(id: &str, region: &str, x: f64, y: f64) -> content::campaign::NodeDef {
    content::campaign::NodeDef { id: content::ids::TheatreNodeId::new(id), name: content::ids::TextKey::new(id), region: content::ids::RegionId::new(region), pos_km: [x, y], kind: content::campaign::NodeKind::Sea, map: Default::default() }
}

fn region(id: &str) -> content::campaign::RegionDef {
    content::campaign::RegionDef { id: content::ids::RegionId::new(id), name: content::ids::TextKey::new(id), act: Act::Act1, base_patrols: 1, layer_depth_m: 50.0, weather: vec![], order_of_battle: vec![], civilian: vec![] }
}

#[test]
fn region_outlines_split_the_chart_and_lanes_dedupe() {
    let mut t = content::campaign::TheatreDef { size_km: [600.0, 400.0], ..Default::default() };
    t.regions = vec![region("west"), region("east"), region("empty")];
    t.nodes = vec![node("a", "west", 100.0, 100.0), node("b", "west", 120.0, 300.0), node("c", "east", 450.0, 150.0), node("d", "east", 480.0, 320.0), node("e", "west", 250.0, 200.0)];
    let outlines = chart::region_outlines(&t);
    assert_eq!(outlines.len(), 3);
    assert!(outlines[2].is_empty());
    let sum = area(&outlines[0]).abs() + area(&outlines[1]).abs();
    assert!((sum - 240_000.0).abs() < 1e-3, "{sum}");
    let lane = |a: &str, b: &str| content::campaign::LaneDef { a: content::ids::TheatreNodeId::new(a), b: content::ids::TheatreNodeId::new(b), traffic: 1.0 };
    t.lanes = vec![lane("a", "b"), lane("b", "a"), lane("a", "a"), lane("a", "zz"), lane("c", "d")];
    let l = chart::lanes(&t);
    assert_eq!(l.len(), 2);
    assert!((l[0].2 - (20.0f64 * 20.0 + 200.0 * 200.0).sqrt()).abs() < 1e-9);
}

#[test]
fn contour_loops_close_and_drop_holes() {
    // A ring of land around a lake on a 9 x 9 grid: one outer loop, the lake dropped.
    let n = 9;
    let mut f = vec![-1.0; n * n];
    for j in 2..7 { for i in 2..7 { f[j * n + i] = 1.0; } }
    f[4 * n + 4] = -1.0;
    let loops = chart::contour_loops(&f, n, n, 1.0, 0.0);
    assert_eq!(loops.len(), 1, "{loops:?}");
    let a = area(&loops[0]).abs();
    assert!(a > 16.0 && a < 25.0, "{a}");
}

#[test]
fn archipelago_keeps_lanes_and_nodes_in_open_water() {
    let t: content::campaign::TheatreDef = content::data::parse_ron(content::data::THEATRE, "theatre.ron").unwrap();
    let (land, shallows) = chart::archipelago(&t);
    assert!(land.len() >= 8, "an archipelago, not one island: {}", land.len());
    assert!(!shallows.is_empty());
    let inside = |poly: &[[f64; 2]], p: [f64; 2]| {
        let mut c = false;
        let mut j = poly.len() - 1;
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[j]);
            if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] { c = !c; }
            j = i;
        }
        c
    };
    for n in &t.nodes { assert!(!land.iter().any(|l| inside(l, n.pos_km)), "{} is on land", n.id.as_str()); }
    for (a, b, _) in chart::lanes(&t) {
        let (pa, pb) = (t.nodes[a].pos_km, t.nodes[b].pos_km);
        for k in 0..=20 {
            let s = k as f64 / 20.0;
            let p = [pa[0] + (pb[0] - pa[0]) * s, pa[1] + (pb[1] - pa[1]) * s];
            assert!(!land.iter().any(|l| inside(l, p)), "lane {} - {} crosses land", t.nodes[a].id.as_str(), t.nodes[b].id.as_str());
        }
    }
    // The same theatre always draws the same coast.
    assert_eq!(chart::archipelago(&t), (land, shallows));
}

#[test]
fn embedded_theatre_and_starters_convert_without_panicking() {
    // The cross-checked campaign load may still fail on an unfinished data tip, so read the raw files.
    let t: content::campaign::TheatreDef = content::data::parse_ron(content::data::THEATRE, "theatre.ron").unwrap();
    let outlines = chart::region_outlines(&t);
    assert_eq!(outlines.len(), t.regions.len());
    let (land, shallows) = chart::archipelago(&t);
    assert!(land.iter().chain(&shallows).flatten().all(|p| p[0].is_finite() && p[1].is_finite()));
    let acts: Vec<content::campaign::ActDef> = content::data::parse_ron(content::data::ACTS, "acts.ron").unwrap();
    for path in starter_paths(&acts, None) {
        let bp = content::campaign::load::stock_blueprint(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let stats = content::stats::compute_stats(&bp, cat());
        assert!(stats.length_m > 0.0, "{path}");
    }
}

#[test]
#[ignore = "prints the chart's land share for tuning"]
fn archipelago_land_share() {
    let t: content::campaign::TheatreDef = content::data::parse_ron(content::data::THEATRE, "theatre.ron").unwrap();
    let (land, shallows) = chart::archipelago(&t);
    let a: f64 = land.iter().map(|l| area(l).abs()).sum();
    let s: f64 = shallows.iter().map(|l| area(l).abs()).sum();
    eprintln!("islands {} land {:.1}% shallows {:.1}%", land.len(), 100.0 * a / 240_000.0, 100.0 * s / 240_000.0);
}

/// Skirmish and lobby offer one act per shipped act row: Act 1 alone in this build (L2283).
#[test]
fn skirmish_acts_follow_the_shipped_rows() {
    let acts = vec![act(Act::Act2, &[]), act(Act::Act1, &[])];
    assert_eq!(shipped_acts(&acts), [Act::Act1, Act::Act2]);
    assert_eq!(shipped_acts(&[]), [Act::Act1]);
    let shipped = &content::campaign::load::shipped_campaign().acts;
    assert_eq!(shipped_acts(shipped), [Act::Act1], "this build ships Act 1 only");
}
