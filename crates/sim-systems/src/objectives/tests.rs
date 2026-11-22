use super::*;
use crate::protocol::config::{SortieIntel, SortieResult};
use content::campaign::{MapDef, Reward};

fn e(i: u32) -> EntityId { EntityId { index: i, generation: 0 } }

pub(crate) fn config(objective: Objective, ghosts: Vec<GhostKind>, rules: ModeRules) -> SortieConfig {
    SortieConfig {
        protocol: crate::protocol::PROTOCOL_VERSION, seed: 7, mode: GameMode::Skirmish, rules, settings: Default::default(), act: Act::Act1,
        weather: WeatherState::default(), weather_changes: vec![], time_of_day: TimeOfDay::Dusk, map: MapDef::default(), layer_depth_m: 50.0,
        sides: vec![], players: vec![], blueprints: vec![], vessels: vec![], bodies: vec![], groups: vec![], objective,
        ghosts: ghosts.into_iter().map(|kind| GhostCondition { kind, reward: Reward::Intel(1) }).collect(), tags: vec![], actions: vec![], intel: SortieIntel::default(),
        cards: vec![], operation: None, region: None, theatre_hour: 0, splash: None, tutorial: None, time_limit_s: None,
    }
}

fn hull(i: u32, enemy: bool, tonnage: f64, tag: &str) -> HullFacts {
    HullFacts { vessel: e(i), side: SideId(if enemy { 1 } else { 0 }), enemy, player_side: !enemy, tonnage_t: tonnage, tag: tag.into(), ..Default::default() }
}

fn marker(kind: MarkerKind, name: &str, x: f64, z: f64) -> Marker { Marker { kind, name: name.into(), pos_m: [x, z], heading_deg: 0.0 } }

#[test]
fn ledger_keeps_attacker_through_fire() {
    let mut l = DamageLedger::default();
    l.record(e(5), e(1), DamageCause::Shell, 10, 4_000.0, Emcon::Active, 12);
    l.record(e(5), EntityId::NONE, DamageCause::Fire, 20, 0.0, Emcon::Silent, 0);
    let r = l.get(e(5)).unwrap();
    assert_eq!(r.attacker, e(1));
    assert_eq!(r.cause, DamageCause::Fire);
    assert_eq!(r.range_m, 4_000.0);
    assert_eq!(r.attacker_emcon, Emcon::Active);
    assert_eq!(r.tick, 20);
    assert_eq!(r.player_buckets, 12);
    l.record(e(5), e(2), DamageCause::Torpedo, 30, 900.0, Emcon::Silent, 3);
    let r = *l.get(e(5)).unwrap();
    assert_eq!((r.attacker, r.range_m, r.player_buckets), (e(2), 900.0, 15));
    // Rows stay sorted for lookup.
    l.record(e(1), e(9), DamageCause::Mine, 1, 0.0, Emcon::Burst, 0);
    assert!(l.rows.windows(2).all(|w| w[0].vessel < w[1].vessel));
    assert_eq!(kill_method(r.cause, false), KillMethod::Torpedo);
    assert_eq!(kill_method(DamageCause::Fire, true), KillMethod::Capture);
    assert_eq!(range_band(1_999.0, 20_000.0), RangeBand::Close);
    assert_eq!(range_band(25_000.0, 20_000.0), RangeBand::BeyondHorizon);
}

#[test]
fn destroy_all_waits_for_reinforcements() {
    let mut hs = vec![hull(1, false, 150.0, ""), hull(2, true, 100.0, ""), hull(3, true, 300.0, "")];
    hs[1].lost = true;
    let f = EvalFacts { hulls: &hs, ..Default::default() };
    let (s, p) = eval_objective(&Objective::DestroyAll, &f);
    assert_eq!(s, ObjectiveState::Active);
    assert!((p - 0.25).abs() < 1e-9);
    hs[2].disabled = true;
    let f = EvalFacts { hulls: &hs, pending: vec![(true, String::new())], ..Default::default() };
    assert_eq!(eval_objective(&Objective::DestroyAll, &f).0, ObjectiveState::Active);
    let f = EvalFacts { hulls: &hs, ..Default::default() };
    assert_eq!(eval_objective(&Objective::DestroyAll, &f), (ObjectiveState::Done, 1.0));
    // No enemies at all never counts as won.
    let only_us = vec![hull(1, false, 150.0, "")];
    assert_eq!(eval_objective(&Objective::DestroyAll, &EvalFacts { hulls: &only_us, ..Default::default() }).0, ObjectiveState::Active);
}

#[test]
fn convoy_raid_fraction_and_escape() {
    let markers = vec![marker(MarkerKind::ConvoyExit, "out", 10_000.0, 0.0)];
    let obj = Objective::DestroyGroup { tag: "convoy".into(), fraction: 0.6 };
    let mut hs = vec![hull(2, true, 100.0, "convoy"), hull(3, true, 100.0, "convoy"), hull(4, true, 200.0, "convoy"), hull(5, true, 500.0, "escort")];
    hs[2].lost = true; // 200 of 400 t
    let f = EvalFacts { hulls: &hs, markers: &markers, ..Default::default() };
    let (s, p) = eval_objective(&obj, &f);
    assert_eq!(s, ObjectiveState::Active);
    assert!((p - 200.0 / 240.0).abs() < 1e-9);
    // One more freighter goes down: 300 of 400 t >= 60%.
    hs[1].disabled = true;
    assert_eq!(eval_objective(&obj, &EvalFacts { hulls: &hs, markers: &markers, ..Default::default() }).0, ObjectiveState::Done);
    // Instead two freighters escape: 200 of 400 t left through the exit, more than the 40% allowed.
    hs[1].disabled = false;
    hs[2].lost = false;
    hs[1].exits = vec!["out".into()];
    hs[2].exits = vec!["out".into()];
    assert_eq!(eval_objective(&obj, &EvalFacts { hulls: &hs, markers: &markers, ..Default::default() }).0, ObjectiveState::Failed);
}

#[test]
fn extraction_needs_hold_then_exit_and_passenger_allowance() {
    let markers = vec![marker(MarkerKind::ShoreLanding, "beach", 0.0, 0.0), marker(MarkerKind::Exit, "exit", 5_000.0, 0.0)];
    let obj = Objective::Extract { marker: "beach".into(), passengers: 4 };
    let mut us = hull(1, false, 150.0, "");
    us.player = Some(PlayerSlot(0));
    let mut hs = vec![us];
    let f = EvalFacts { hulls: &hs, markers: &markers, ..Default::default() };
    assert_eq!(eval_objective(&obj, &f), (ObjectiveState::Active, 0.0));
    let held = vec![("beach".to_string(), Some(DVec2::ZERO))];
    let f = EvalFacts { hulls: &hs, markers: &markers, holds: held.clone(), ..Default::default() };
    assert_eq!(eval_objective(&obj, &f), (ObjectiveState::Active, 0.5));
    // Passing the exit on the way in does not count; being there after the pickup does.
    hs[0].exits = vec!["exit".into()];
    let f = EvalFacts { hulls: &hs, markers: &markers, holds: held.clone(), ..Default::default() };
    assert_eq!(eval_objective(&obj, &f).0, ObjectiveState::Active);
    hs[0].at_exits = vec!["exit".into()];
    let f = EvalFacts { hulls: &hs, markers: &markers, holds: held.clone(), ..Default::default() };
    assert_eq!(eval_objective(&obj, &f).0, ObjectiveState::Done);
    let f = EvalFacts { hulls: &hs, markers: &markers, holds: held, passengers_lost: 2, allowed_losses: Some(1), ..Default::default() };
    assert_eq!(eval_objective(&obj, &f).0, ObjectiveState::Failed);
}

#[test]
fn mines_photos_capture_cutout_feint_survive() {
    let markers = vec![marker(MarkerKind::Objective, "lane", 0.0, 0.0), marker(MarkerKind::Exit, "exit", 9_000.0, 0.0)];
    let mines = vec![[100.0, 0.0], [150.0, 0.0], [5_000.0, 0.0]];
    let lay = Objective::LayMines { marker: "lane".into(), count: 2 };
    assert_eq!(eval_objective(&lay, &EvalFacts { markers: &markers, mines: &mines, ..Default::default() }).0, ObjectiveState::Done);
    let lay3 = Objective::LayMines { marker: "lane".into(), count: 3 };
    let (s, p) = eval_objective(&lay3, &EvalFacts { markers: &markers, mines: &mines, ..Default::default() });
    assert_eq!(s, ObjectiveState::Active);
    assert!((p - 2.0 / 3.0).abs() < 1e-9);

    let mut hs = vec![hull(2, true, 100.0, "pickets"), hull(3, true, 100.0, "pickets"), hull(4, true, 100.0, "other")];
    let photo = Objective::Photograph { tag: "pickets".into(), count: 2 };
    let f = EvalFacts { hulls: &hs, photos: vec![e(2), e(4)], ..Default::default() };
    assert_eq!(eval_objective(&photo, &f).0, ObjectiveState::Active);
    hs[1].lost = true;
    let f = EvalFacts { hulls: &hs, photos: vec![e(2)], ..Default::default() };
    assert_eq!(eval_objective(&photo, &f).0, ObjectiveState::Failed);
    let f = EvalFacts { hulls: &hs, photos: vec![e(2), e(3)], ..Default::default() };
    assert_eq!(eval_objective(&photo, &f).0, ObjectiveState::Done);

    let cap = Objective::Capture { tag: "pickets".into() };
    let prize = |decided, stripped| PrizeFact { vessel: e(2), tag: "pickets".into(), decided, stripped, afloat: true };
    assert_eq!(eval_objective(&cap, &EvalFacts { hulls: &hs, prizes: vec![prize(false, false)], ..Default::default() }), (ObjectiveState::Active, 0.5));
    assert_eq!(eval_objective(&cap, &EvalFacts { hulls: &hs, prizes: vec![prize(true, false)], ..Default::default() }).0, ObjectiveState::Done);

    let cut = Objective::CutOut { tag: "pickets".into(), exit: "exit".into() };
    assert_eq!(eval_objective(&cut, &EvalFacts { hulls: &hs, markers: &markers, prizes: vec![prize(true, true)], ..Default::default() }).0, ObjectiveState::Failed);
    hs[0].at_exits = vec!["exit".into()];
    assert_eq!(eval_objective(&cut, &EvalFacts { hulls: &hs, markers: &markers, prizes: vec![prize(true, false)], ..Default::default() }).0, ObjectiveState::Done);

    let feint = Objective::Feint { hold_s: 120 };
    assert_eq!(eval_objective(&feint, &EvalFacts::default()).1, 0.0);
    let (s, p) = eval_objective(&feint, &EvalFacts { detected: true, feint_held_s: 60.0, ..Default::default() });
    assert_eq!(s, ObjectiveState::Active);
    assert!((p - 0.5).abs() < 1e-9);
    assert_eq!(eval_objective(&feint, &EvalFacts { detected: true, feint_held_s: 130.0, ..Default::default() }).0, ObjectiveState::Active);
    assert_eq!(eval_objective(&feint, &EvalFacts { detected: true, feint_held_s: 130.0, clear_of_contact: true, ..Default::default() }).0, ObjectiveState::Done);

    let surv = Objective::Survive { seconds: 600, exit: "exit".into() };
    assert_eq!(eval_objective(&surv, &EvalFacts { now_s: 300.0, ..Default::default() }), (ObjectiveState::Active, 0.5));
    assert_eq!(eval_objective(&surv, &EvalFacts { now_s: 600.0, ..Default::default() }).0, ObjectiveState::Done);
}

#[test]
fn escort_done_and_failed() {
    let markers = vec![marker(MarkerKind::Exit, "exit", 0.0, 0.0)];
    let obj = Objective::Escort { tag: "defector".into(), exit: "exit".into() };
    let mut hs = vec![hull(6, false, 300.0, "defector"), hull(7, false, 300.0, "defector")];
    hs[0].exits = vec!["exit".into()];
    assert_eq!(eval_objective(&obj, &EvalFacts { hulls: &hs, markers: &markers, ..Default::default() }), (ObjectiveState::Active, 0.5));
    hs[1].lost = true;
    assert_eq!(eval_objective(&obj, &EvalFacts { hulls: &hs, markers: &markers, ..Default::default() }).0, ObjectiveState::Done);
    hs[0].exits.clear();
    hs[0].disabled = true;
    assert_eq!(eval_objective(&obj, &EvalFacts { hulls: &hs, markers: &markers, ..Default::default() }).0, ObjectiveState::Failed);
}

#[test]
fn ghosts() {
    let mut s = ObjectivesState::default();
    let a = ObjectiveState::Active;
    assert_eq!(eval_ghost(&GhostKind::NoRadarEmissions, a, &s, 0.0, None, None), a);
    s.radiated = true;
    assert_eq!(eval_ghost(&GhostKind::NoRadarEmissions, a, &s, 0.0, None, None), ObjectiveState::Failed);
    assert_eq!(eval_ghost(&GhostKind::UnderTime { seconds: 600 }, a, &s, 500.0, Some(590.0), None), ObjectiveState::Done);
    assert_eq!(eval_ghost(&GhostKind::UnderTime { seconds: 600 }, a, &s, 601.0, None, None), ObjectiveState::Failed);
    assert_eq!(eval_ghost(&GhostKind::CaptureTheAce, a, &s, 0.0, None, Some((true, false))), ObjectiveState::Done);
    assert_eq!(eval_ghost(&GhostKind::CaptureTheAce, a, &s, 0.0, None, Some((false, true))), ObjectiveState::Failed);
    assert_eq!(eval_ghost(&GhostKind::CaptureTheAce, a, &s, 0.0, None, None), a);
    let mut lost = hull(1, false, 150.0, "");
    lost.lost = true;
    s.hulls.push(lost);
    assert_eq!(eval_ghost(&GhostKind::NoLosses, a, &s, 0.0, None, None), ObjectiveState::Failed);
    // Settled ghosts never change back.
    assert_eq!(eval_ghost(&GhostKind::NoLosses, ObjectiveState::Done, &s, 0.0, None, None), ObjectiveState::Done);
    // Ghosts pay only when the objective is done.
    let g = GhostTracker { ghost: GhostCondition { kind: GhostKind::NoHullDamage, reward: Reward::Intel(1) }, state: a };
    assert!(ghost_result(&g, true));
    assert!(!ghost_result(&g, false));
    let g = GhostTracker { ghost: GhostCondition { kind: GhostKind::CaptureTheAce, reward: Reward::Intel(1) }, state: a };
    assert!(!ghost_result(&g, true));
}

#[test]
fn elimination_rule() {
    let team = |s: SideId| Some(s.0);
    let mut hs = vec![hull(1, false, 400.0, ""), hull(2, true, 400.0, "")];
    assert_eq!(elimination(&hs, &team), None);
    hs[1].disabled = true;
    assert_eq!(elimination(&hs, &team), Some(Some(0)));
    hs[0].lost = true;
    assert_eq!(elimination(&hs, &team), Some(None));
    // Only one team fielded: nothing decided.
    assert_eq!(elimination(&hs[..1], &team), None);
}

#[test]
fn init_views_and_summary() {
    let c = config(Objective::LayMines { marker: "lane".into(), count: 10 }, vec![GhostKind::NoLosses, GhostKind::UnderTime { seconds: 900 }], ModeRules::Objective);
    let s = init(&c);
    assert_eq!(s.ghosts.len(), 2);
    let v = views(&s, &c);
    assert_eq!(v.len(), 3);
    assert_eq!(v[0].id, ObjectiveId(0));
    assert!(!v[0].ghost);
    assert_eq!(v[0].text.key.0, "objective.lay_mines");
    assert_eq!(v[2].id, ObjectiveId(2));
    assert!(v[2].ghost);
    assert_eq!(v[2].text.key.0, "objective.ghost.under_time");
    let sandbox = config(Objective::DestroyAll, vec![], ModeRules::Sandbox);
    assert_eq!(views(&init(&sandbox), &sandbox)[0].state, ObjectiveState::Hidden);
    let r = SortieResult {
        operation: None, region: None, seed: 7, outcome: Outcome::Victory, winner: Some(SideId(0)), duration_ticks: 100, objective_done: true, ghosts: vec![true, false],
        kills: vec![], survivors: vec![], prizes: vec![], aces: vec![], players: vec![], sightings: 0, evidence: 0, photos: 0, survivors_recovered: 0, crew_captured: 0,
        passengers_delivered: 0, mines_laid: 10, site_destroyed: false, tender_lost: false, highest_rung: 0, belief: vec![], stats: SortieStats::default(),
    };
    let sum = summary(&r, &c);
    assert_eq!(sum.objectives, vec![(ObjectiveId(0), true), (ObjectiveId(1), true), (ObjectiveId(2), false)]);
    assert_eq!(sum.tip, None);
}

#[test]
fn every_key_has_a_row_and_every_row_a_key() {
    let csv = include_str!("../../../../game/i18n/objectives.csv");
    let mut lines = csv.lines();
    assert_eq!(lines.next(), Some("keys,?context,en"));
    let rows: BTreeSet<&str> = lines.filter(|l| !l.trim().is_empty()).map(|l| l.split(',').next().unwrap()).collect();
    let keys: BTreeSet<&str> = text_keys().into_iter().collect();
    assert_eq!(rows, keys);
    for k in &keys { assert!(k.starts_with("objective.")); }
}

#[test]
fn state_round_trips() {
    let c = config(Objective::DestroyAll, vec![GhostKind::NoKills], ModeRules::Objective);
    let mut s = init(&c);
    s.hulls.push(hull(3, true, 90.0, "x"));
    s.mines.push([1.0, 2.0]);
    s.reported.push((ObjectiveState::Active, 12));
    let back: ObjectivesState = bincode::deserialize(&bincode::serialize(&s).unwrap()).unwrap();
    assert_eq!(back, s);
    let stats = SortieStats { enemy_tonnage_disabled: 300.0, enemy_tonnage_total: 600.0, own_cells_lost_fraction: 0.2, ..Default::default() };
    assert!((trial_score(&stats) - 40.0).abs() < 1e-9);
}

#[test]
fn reactivated_hull_drops_its_disabled_kill() {
    let mut s = ObjectivesState::default();
    let rec = |fate| KillRecord { tick: 1, hull_record: Some(4), class: None, side: SideId(1), tonnage_t: 100.0, build_cost: 10, fate, method: KillMethod::Gunfire, range: RangeBand::Close, time: TimeOfDay::Dusk, emcon: Emcon::Burst, observed: false, by_player: Some(PlayerSlot(0)), salvageable: true };
    s.kills = vec![rec(KillFate::Disabled), rec(KillFate::Lost)];
    s.kill_vessels = vec![e(5), e(6)];
    s.stats.kills_by_method.insert(KillMethod::Gunfire, 2);
    unrecord_disabled(&mut s, e(6));
    assert_eq!(s.kills.len(), 2);
    unrecord_disabled(&mut s, e(5));
    assert_eq!(s.kill_vessels, vec![e(6)]);
    assert_eq!(s.stats.kills_by_method.get(&KillMethod::Gunfire), Some(&1));
    // PvP: hits on the other team count, on your own side they do not.
    let mut c = config(Objective::DestroyAll, vec![], ModeRules::Elimination);
    c.sides = vec![
        SideDef { id: SideId(0), faction: Faction::Players, name: TextKey::new("a"), hostile_to: SideMask::only(SideId(1)), allied: SideMask::only(SideId(0)), pvp_team: Some(0) },
        SideDef { id: SideId(1), faction: Faction::Players, name: TextKey::new("b"), hostile_to: SideMask::only(SideId(0)), allied: SideMask::only(SideId(1)), pvp_team: Some(1) },
    ];
    s.hulls = vec![hull(1, false, 1.0, ""), hull(2, true, 1.0, ""), hull(3, false, 1.0, "")];
    assert!(opposed(&s, &c, e(1), e(2)));
    assert!(!opposed(&s, &c, e(1), e(3)));
}

/// A hull that strikes and is then captured (`Sim::capture` clears the world's flag) still counts once in
/// the Struck colours row; a hull that never struck does not count.
#[test]
fn a_struck_then_captured_hull_counts_once() {
    let cat = std::sync::Arc::new(content::catalogue::catalogue().clone());
    let bp = content::campaign::load::stock_blueprint("starters/fac22.bp.ron").unwrap();
    let prefab = std::sync::Arc::new(sim_core::vessel::build_prefab(&bp, &cat));
    let map = MapDef { size_m: [20_000.0, 20_000.0], depth_m: 200.0, ..Default::default() };
    let mut w = World::new(sim_core::WorldConfig { seed: 1, weather: WeatherState::default(), map, catalogue: cat.clone() });
    let spawn = |x: f64| sim_core::vessel::VesselSpawn {
        prefab: prefab.clone(), side: SideId(1), player: None, class: None, pose: sim_core::Pose::from_heading(DVec3::new(x, 0.0, 0.0), 0.0), speed_mps: 0.0,
        damage: None, fuel_fraction: 1.0, ammo_fraction: 1.0, fidelity: sim_core::vessel::Fidelity::Full, submersible: false,
    };
    let prize = w.spawn_vessel(&spawn(0.0));
    let other = w.spawn_vessel(&spawn(2_000.0));
    let mut c = config(Objective::DestroyAll, vec![], ModeRules::Objective);
    c.sides = vec![
        SideDef { id: SideId(0), faction: Faction::Players, name: TextKey::new("a"), hostile_to: SideMask::only(SideId(1)), allied: SideMask::only(SideId(0)), pvp_team: None },
        SideDef { id: SideId(1), faction: Faction::Combine, name: TextKey::new("b"), hostile_to: SideMask::only(SideId(0)), allied: SideMask::only(SideId(1)), pvp_team: None },
    ];
    let ships = crate::ship::ShipTable::default();
    let mut s = ObjectivesState::default();
    refresh_hulls(&mut s, &w, &ships, &c, &[], 0.1, 0);
    assert_eq!(s.hulls.iter().filter(|h| h.enemy).count(), 2);
    w.set_struck(prize, true);
    refresh_hulls(&mut s, &w, &ships, &c, &[], 0.1, 0);
    // Captured: the flag is cleared and the hull changes side.
    w.set_struck(prize, false);
    w.set_side(prize, SideId(0));
    refresh_hulls(&mut s, &w, &ships, &c, &[], 0.1, 0);
    let r = build_result(&s, &w, &ships, &crate::specops::SpecOpsState::default(), &[], &[], &c, 0);
    assert_eq!(r.stats.enemy_struck, 1);
    assert!(s.hull(prize).is_some_and(|h| h.struck) && s.hull(other).is_some_and(|h| !h.struck));
}

#[test]
fn disabled_player_hulls_end_the_sortie_after_five_minutes() {
    let s300 = (PLAYER_DISABLED_DEFEAT_S * sim_core::time::TICK_HZ as f64) as u64;
    let player = |i: u32| HullFacts { player: Some(PlayerSlot(i as u8)), role: VesselRole::PlayerHull, ..hull(i, false, 100.0, "") };
    // One hull: disabled at tick 1000, still disabled one tick short of 300 s, then at 300 s.
    let mut a = player(1);
    note_status(&mut a, VesselStatus::Disabled, false, 1_000);
    note_status(&mut a, VesselStatus::Disabled, false, 5_000);
    assert_eq!(a.disabled_since, Some(1_000));
    assert_eq!(player_hulls_out(std::slice::from_ref(&a), 1_000 + s300 - 1), None);
    assert_eq!(player_hulls_out(std::slice::from_ref(&a), 1_000 + s300), Some(EndReason::PlayerHullsDisabled));
    // A break (propulsion restored for one evaluation) restarts the count.
    note_status(&mut a, VesselStatus::Active, false, 6_000);
    assert_eq!(a.disabled_since, None);
    note_status(&mut a, VesselStatus::Disabled, false, 7_000);
    assert_eq!(player_hulls_out(std::slice::from_ref(&a), 1_000 + s300), None);
    assert_eq!(player_hulls_out(std::slice::from_ref(&a), 7_000 + s300), Some(EndReason::PlayerHullsDisabled));
    // Co-op: one hull still under way holds the sortie open; once it is lost, the disabled one decides it.
    let mut b = player(2);
    note_status(&mut b, VesselStatus::Active, false, 7_000);
    let both = vec![a.clone(), b.clone()];
    assert_eq!(player_hulls_out(&both, 7_000 + 10 * s300), None);
    note_status(&mut b, VesselStatus::Lost(LossCause::Sunk), false, 8_000);
    assert_eq!(b.disabled_since, None);
    assert_eq!(player_hulls_out(&[a.clone(), b.clone()], 7_000 + s300 - 1), None);
    assert_eq!(player_hulls_out(&[a.clone(), b.clone()], 7_000 + s300), Some(EndReason::PlayerHullsDisabled));
    // Every hull lost is its own reason; enemy hulls and an empty list never decide.
    let mut c = a.clone();
    note_status(&mut c, VesselStatus::Lost(LossCause::Capsized), false, 9_000);
    assert_eq!(player_hulls_out(&[c, b], 9_000), Some(EndReason::PlayerHullsLost));
    let mut enemy = hull(9, true, 100.0, "");
    note_status(&mut enemy, VesselStatus::Disabled, false, 0);
    assert_eq!(player_hulls_out(&[enemy], 10 * s300), None);
    assert_eq!(player_hulls_out(&[], 10 * s300), None);
}

#[test]
fn disabled_defeat_shows_its_own_text() {
    let c = config(Objective::DestroyAll, vec![GhostKind::NoKills], ModeRules::Objective);
    let mut s = init(&c);
    assert_eq!(views(&s, &c)[0].text.key.0, "objective.destroy_all");
    s.end_reason = Some(EndReason::PlayerHullsDisabled);
    let v = views(&s, &c);
    assert_eq!(v[0].text.key.0, "objective.end.hulls_disabled");
    assert_eq!(v[0].state, ObjectiveState::Failed);
    assert!(text_keys().contains(&"objective.end.hulls_disabled"));
    s.end_reason = Some(EndReason::TimeLimit);
    assert_eq!(views(&s, &c)[0].text.key.0, "objective.destroy_all");
}

#[test]
fn a_site_is_destroyed_by_its_share_of_cells_lost() {
    let markers = vec![marker(MarkerKind::DepotSite, "depot", 0.0, 0.0)];
    let obj = Objective::DestroySite { marker: "depot".into() };
    let mut site = HullFacts { role: VesselRole::Site, tag: "depot".into(), ..hull(4, true, 900.0, "depot") };
    let eval = |h: &HullFacts| eval_objective(&obj, &EvalFacts { hulls: std::slice::from_ref(h), markers: &markers, ..Default::default() });
    assert_eq!(eval(&site), (ObjectiveState::Active, 0.0));
    // Progress rises with the share of cells destroyed.
    site.cells_lost = 0.4 * SITE_DESTROYED_CELLS;
    let (s, p) = eval(&site);
    assert_eq!(s, ObjectiveState::Active);
    assert!((p - 0.4).abs() < 1e-9, "{p}");
    // Propulsion gone (disabled) or colours struck does not destroy a site.
    site.disabled = true;
    site.struck = true;
    assert_eq!(eval(&site).0, ObjectiveState::Active);
    site.disabled = false;
    site.struck = false;
    // The stated share does, and so does sinking.
    site.cells_lost = SITE_DESTROYED_CELLS;
    assert_eq!(eval(&site), (ObjectiveState::Done, 1.0));
    let mut sunk = HullFacts { cells_lost: 0.0, lost: true, ..site.clone() };
    assert_eq!(eval(&sunk), (ObjectiveState::Done, 1.0));
    // Two site hulls: both must go; progress is their mean.
    sunk.lost = false;
    sunk.vessel = e(5);
    let two = vec![site.clone(), sunk];
    assert_eq!(eval_objective(&obj, &EvalFacts { hulls: &two, markers: &markers, ..Default::default() }), (ObjectiveState::Active, 0.5));
}
