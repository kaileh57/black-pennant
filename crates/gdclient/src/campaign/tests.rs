//! Headless tests of the campaign units (`gd-campaign`, `gd-campaign-saves`): wording against the CSV, the
//! save store on disk, quotes and previews against the theatre's own rules, and played time.
use super::{coop, model, store, words};
use crate::text::tests::CsvLookup;
use content::ids::{CardId, PlayerSlot, RegionId, TheatreNodeId};
use content::save::{IntelSpend, QueuedCard, SaveGame, SuspendSave, TheatreAction};
use sim_systems::protocol::{Msg, MsgArg};
use sim_systems::theatre::{economy, logistics, AlertCause, Interrupt, NewCampaign, Theatre, TheatreData, TheatreEvent, TheatreReject};
use std::path::PathBuf;
use std::sync::Arc;

fn data() -> TheatreData {
    TheatreData { campaign: Arc::new(content::campaign::load::campaign().clone()), catalogue: Arc::new(content::catalogue::catalogue().clone()) }
}

fn starter() -> content::blueprint::Blueprint { content::campaign::load::stock_blueprint(super::DEFAULT_STARTER).expect("default starter loads") }

fn theatre(players: usize, ironman: bool) -> Theatre {
    let new = NewCampaign {
        seed: 7, difficulty: content::campaign::DifficultySliders::NORMAL, ironman, slot_name: "test".into(),
        players: (0..players).map(|i| (100 + i as u64, format!("p{i}"), starter())).collect(), start_act: content::enums::Act::Act1,
    };
    Theatre::new_campaign(new, data())
}

/// A fresh empty folder under the system temp folder.
fn temp_root(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let p = std::env::temp_dir().join(format!("bp-campaign-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn names(d: &TheatreData, players: Vec<String>) -> words::Names<'_> { words::Names { campaign: &d.campaign, catalogue: &d.catalogue, players, act: None } }
fn text(lk: &CsvLookup, m: &Msg) -> String { crate::text::msg_text(lk, m) }
fn filled(s: &str) -> bool { !s.contains('{') && !s.contains("campaign.") && !s.contains("units.") && !s.trim().is_empty() }

// ---- Wording ----

#[test]
fn every_key_has_an_english_row_and_no_banned_word() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/i18n/campaign.csv");
    let rows = crate::text::tests::parse_csv(&std::fs::read_to_string(path).expect("campaign.csv"));
    let in_csv: std::collections::BTreeSet<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    for k in words::text_keys() { assert!(in_csv.contains(k.as_str()), "campaign.csv lacks {k}"); }
    for (key, _, en) in &rows {
        assert!(key.starts_with("campaign."), "{key} is outside the campaign namespace");
        assert!(!en.trim().is_empty(), "{key} has no English");
        let lower = en.to_lowercase();
        for banned in ["introducing", "unlock", "empower", "elevate", "streamline", "transform", "discover", "seamless", "smart", "next-gen", "revolutionize", "leverage", "delve", "vibrant", "pivotal", "showcase", "tapestry", "testament", "unleash"] {
            assert!(!lower.split(|c: char| !c.is_alphanumeric() && c != '-').any(|w| w == banned), "{key}: banned word {banned}");
        }
    }
}

#[test]
fn every_sentence_fills_all_its_placeholders() {
    let lk = CsvLookup::load();
    let d = data();
    let n = names(&d, vec!["Ash".into(), String::new()]);
    let node = d.campaign.theatre.nodes[0].id.clone();
    let region = d.campaign.theatre.regions[0].id.clone();
    let op = d.campaign.operations.keys().next().unwrap().clone();
    let card = d.campaign.cards.keys().next().unwrap().clone();
    let ace = d.campaign.aces.keys().next().unwrap().clone();
    let part = d.catalogue.parts.keys().next().unwrap().clone();
    let p = sim_systems::theatre::Proposal { id: sim_systems::ids::ProposalId(1), by: PlayerSlot(0), action: TheatreAction::Wait { hours: 6 }, accepted: sim_systems::ids::PlayerMask(1), made_hour: 3 };
    use content::enums::{Act, Tier};
    let events = vec![
        TheatreEvent::HourAdvanced { hour: 5 }, TheatreEvent::SextantMoved { from: node.clone(), to: node.clone() }, TheatreEvent::SextantMoved { from: TheatreNodeId::default(), to: node.clone() },
        TheatreEvent::LaneCut { a: node.clone(), b: node.clone(), until_hour: 250 }, TheatreEvent::LaneRestored { a: node.clone(), b: node.clone() },
        TheatreEvent::DepotEmptied { node: node.clone() }, TheatreEvent::SupplyChanged { region: region.clone(), supply: 41.6 },
        TheatreEvent::PatrolDensityChanged { region: region.clone(), factor: 0.62 }, TheatreEvent::AlertChanged { region: region.clone(), alert: 2.5, cause: AlertCause::Sighting },
        TheatreEvent::AlertEscalated { region: region.clone() }, TheatreEvent::HunterGroupSortied { group: 1, arrives_hour: 40, ace: None },
        TheatreEvent::HunterGroupSortied { group: 1, arrives_hour: 40, ace: Some(ace.clone()) }, TheatreEvent::HunterGroupArrived { group: 1 },
        TheatreEvent::TallyChanged { key: content::campaign::TallyKey::Method(content::campaign::KillMethod::Missile), value: 4.25 },
        TheatreEvent::CardQueued { region: region.clone(), card: card.clone(), arrives_hour: 80 }, TheatreEvent::CardDelayed { region: region.clone(), card: card.clone(), arrives_hour: 150 },
        TheatreEvent::CardCancelled { region: region.clone(), card: card.clone() }, TheatreEvent::CardArrived { region: region.clone(), card: card.clone() },
        TheatreEvent::ClockPushedBack { days: 1.5, hours_left: 500.0 }, TheatreEvent::ClockExpired, TheatreEvent::ActLost { act: Act::Act1 },
        TheatreEvent::ActWon { act: Act::Act2 }, TheatreEvent::ActStarted { act: Act::Act2, retreat: true }, TheatreEvent::ActStarted { act: Act::Act1, retreat: false },
        TheatreEvent::OpenTheatreOpened, TheatreEvent::OperationOffered(op.clone()), TheatreEvent::OperationWithdrawn(op.clone()), TheatreEvent::OperationUnlocked(op.clone()),
        TheatreEvent::TierOpened(Tier::T3), TheatreEvent::PartUnlocked(part.clone()), TheatreEvent::ResourcesChanged, TheatreEvent::HullLost { player: PlayerSlot(0) },
        TheatreEvent::HullRebuilt { player: PlayerSlot(1) }, TheatreEvent::RefitStarted { player: PlayerSlot(0), hours: 6 }, TheatreEvent::SextantSunk { relocated_to: node.clone(), ready_hour: 90 },
        TheatreEvent::CampaignOver, TheatreEvent::GroupSighted { group: 2, node: node.clone() }, TheatreEvent::ConvoyRevealed { convoy: 3 }, TheatreEvent::AceRevealed { ace: ace.clone(), node: node.clone() },
        TheatreEvent::ProposalMade(p.clone()), TheatreEvent::ProposalAnswered { proposal: p.id, player: PlayerSlot(1), accept: true }, TheatreEvent::ProposalAnswered { proposal: p.id, player: PlayerSlot(1), accept: false },
        TheatreEvent::ProposalCommitted { proposal: p.id }, TheatreEvent::ProposalWithdrawn { proposal: p.id }, TheatreEvent::Saved { sequence: 4 },
    ];
    let mut kinds = std::collections::BTreeSet::new();
    for e in &events {
        kinds.insert(words::event_kind(e));
        let t = text(&lk, &words::event_msg(e, 10, &n));
        assert!(filled(&t), "{e:?} reads {t:?}");
    }
    for k in words::EVENT_KINDS { assert!(kinds.contains(k), "no sample of {k}"); }
    assert_eq!(text(&lk, &words::event_msg(&TheatreEvent::LaneCut { a: node.clone(), b: node.clone(), until_hour: 250 }, 10, &n)).matches("10 days").count(), 1);
    let interrupts = vec![
        Interrupt::Arrived(node.clone()), Interrupt::RefitDone, Interrupt::WaitDone, Interrupt::OperationOffered(op.clone()), Interrupt::ForcedOperation(op.clone()),
        Interrupt::HunterGroupWarning { hours: 30 }, Interrupt::CardArriving { region: region.clone(), card: card.clone(), hours: 24 }, Interrupt::ClockLow { days_left: 6.9 },
        Interrupt::ActEnded, Interrupt::HullReady(PlayerSlot(1)), Interrupt::Paused,
    ];
    for (i, int) in interrupts.iter().enumerate() {
        assert_eq!(words::interrupt_kind(int), i as i64, "Bp.INTERRUPT_* order");
        let t = text(&lk, &words::interrupt_msg(int, &n));
        assert!(filled(&t), "{int:?} reads {t:?}");
    }
    // An unnamed player reads as its number.
    assert_eq!(text(&lk, &words::interrupt_msg(&Interrupt::HullReady(PlayerSlot(1)), &n)), "2's hull is ready.");
    let actions = vec![
        TheatreAction::Transit(node.clone()), TheatreAction::Refit, TheatreAction::Wait { hours: 12 }, TheatreAction::AcceptOperation(op.clone()), TheatreAction::DeclineOperation(op.clone()),
        TheatreAction::SpendIntel(IntelSpend::DelayCard { region: region.clone(), card: card.clone() }), TheatreAction::SpendIntel(IntelSpend::CancelCard { region: region.clone(), card: card.clone() }),
        TheatreAction::SpendIntel(IntelSpend::RevealConvoys(region.clone())), TheatreAction::SpendIntel(IntelSpend::RevealAce(ace.clone())), TheatreAction::RebuildHull { player: 0 },
    ];
    for a in &actions { let t = text(&lk, &words::action_msg(a, &n)); assert!(filled(&t), "{a:?} reads {t:?}"); }
    let cancel = text(&lk, &words::action_msg(&TheatreAction::SpendIntel(IntelSpend::CancelCard { region: region.clone(), card: card.clone() }), &n));
    assert!(cancel.starts_with("Spend 5 intel"), "{cancel}");
    for r in [TheatreReject::NotHost, TheatreReject::NotEnoughSalvage, TheatreReject::NotEnoughIntel, TheatreReject::NotAtCove, TheatreReject::HullNotReady, TheatreReject::OperationNotOffered, TheatreReject::ForcedOperationPending, TheatreReject::IronmanLocked, TheatreReject::OverTonnageCap, TheatreReject::InvalidBlueprint, TheatreReject::NoSuchProposal, TheatreReject::UnknownTarget, TheatreReject::CampaignOver] {
        let t = text(&lk, &words::reject_msg(r));
        assert!(filled(&t), "{r:?} reads {t:?}");
    }
    use content::campaign::{ConstraintTag, GhostKind, RangeBand, Reward, TallyKey};
    for t in [ConstraintTag::DraftLimit { max_m: 1.8 }, ConstraintTag::RcsLimit { max_m2: 40.0 }, ConstraintTag::Range { min_km: 300.0 }, ConstraintTag::Passengers { count: 6, allowed_losses: 1 }, ConstraintTag::Requires(content::part::PartTag::MineRail), ConstraintTag::MinSpeed { kn: 30.0 }] {
        let s = text(&lk, &words::tag_msg(&t));
        assert!(filled(&s), "{t:?} reads {s:?}");
    }
    assert_eq!(text(&lk, &words::tag_msg(&ConstraintTag::DraftLimit { max_m: 1.8 })), "Draft at most 1.8 m.");
    for g in [GhostKind::NoRadarEmissions, GhostKind::NoLosses, GhostKind::NeverDetected, GhostKind::CaptureTheAce, GhostKind::NoKills, GhostKind::UnderTime { seconds: 1200 }, GhostKind::NoHullDamage] {
        let s = text(&lk, &words::ghost_msg(&g));
        assert!(filled(&s), "{g:?} reads {s:?}");
    }
    for r in [Reward::Intel(2), Reward::Blueprint(part.clone()), Reward::Salvage(120)] { let s = text(&lk, &words::reward_msg(&r, &n)); assert!(filled(&s), "{s}"); }
    for k in [TallyKey::Method(content::campaign::KillMethod::Torpedo), TallyKey::Range(RangeBand::BeyondHorizon), TallyKey::Time(content::enums::TimeOfDay::Night), TallyKey::Emission(content::enums::Emcon::Silent)] {
        let s = text(&lk, &words::tally_msg(&k));
        assert!(filled(&s), "{k:?} reads {s:?}");
        assert!(words::tally_key_string(&k).contains(':'));
    }
    assert_eq!(words::tally_key_string(&TallyKey::Method(content::campaign::KillMethod::Missile)), "method:missile");
    assert_eq!(text(&lk, &words::tally_msg(&TallyKey::Time(content::enums::TimeOfDay::Night))), "Night kills");
}

// ---- Theatre-backed rules ----

#[test]
fn transit_preview_matches_the_theatre() {
    let d = data();
    let t = theatre(1, false);
    let s = &t.state;
    let any = || MsgArg::Int(0);
    let mut checked = 0;
    for cove in &s.open_coves {
        if *cove == s.sextant_node { continue; }
        let p = model::transit_preview(s, &d, cove, false, any());
        let want = logistics::transit_hours(s, &d, &s.sextant_node, cove, false);
        assert_eq!(p.reachable, want.is_some(), "{cove:?}");
        if let Some(h) = want {
            assert_eq!(p.hours, h.max(1));
            assert_eq!(p.path.first(), Some(&s.sextant_node));
            assert_eq!(p.path.last(), Some(cove));
            // The path's length gives the theatre's hours.
            let (km, _) = model::transit_path(s, &d, &s.sextant_node, cove).unwrap();
            assert_eq!(h, ((km / logistics::SEXTANT_KMH).ceil() as u32).max(1));
            checked += 1;
        }
    }
    assert!(checked > 0, "no other open cove to check");
    // Refusals.
    assert_eq!(model::transit_preview(s, &d, &s.sextant_node, false, any()).text.key.as_str(), "campaign.transit.here");
    let other = s.open_coves.iter().find(|c| **c != s.sextant_node).unwrap();
    assert_eq!(model::transit_preview(s, &d, other, true, any()).text.key.as_str(), "campaign.transit.under_way");
    let sea = d.campaign.theatre.nodes.iter().find(|n| !s.open_coves.contains(&n.id)).unwrap();
    assert_eq!(model::transit_preview(s, &d, &sea.id, false, any()).text.key.as_str(), "campaign.transit.not_cove");
    assert!(!model::transit_preview(s, &d, &TheatreNodeId::new("nowhere"), false, any()).reachable);
}

#[test]
fn quotes_are_what_the_services_charge() {
    let d = data();
    let mut t = theatre(1, false);
    let p = PlayerSlot(0);
    let q = model::service_quotes(&t.state, p, false, &d);
    assert!(!q.repair.needed && q.repair.salvage == 0);
    assert!(!q.rearm.needed, "a new hull sails full");
    assert!(!q.rebuild.needed);
    let h = model::hull_summary(&t.state, p, &d);
    assert_eq!((h.damage, h.fuel, h.ammo), (0.0, 1.0, 1.0));

    // Damage a part, empty the magazines and tanks, and the stock.
    t.state.players[0].damage.parts.insert(content::ids::PartSlot(0), 0.5);
    let caps: Vec<content::ids::PartSlot> = {
        let mut probe = t.state.clone();
        probe.resources.salvage = 1 << 30;
        economy::rearm(&mut probe, p, &d, &mut Vec::new()).unwrap();
        probe.players[0].ammo.keys().copied().collect()
    };
    for s in &caps { t.state.players[0].ammo.insert(*s, 0); }
    t.state.players[0].fuel_fraction = 0.25;
    t.state.resources.fuel_l = 0.0;
    t.state.resources.ammo_kg.clear();
    t.state.resources.munitions.clear();
    t.state.resources.salvage = 1 << 20;
    let q = model::service_quotes(&t.state, p, false, &d);
    assert!(q.repair.needed && q.repair.salvage > 0);
    assert!(q.rearm.needed && !q.rearm.from_stock && q.rearm.salvage > 0);
    let h = model::hull_summary(&t.state, p, &d);
    assert!(h.damage > 0.0 && h.damage < 1.0 && h.fuel == 0.25, "{h:?}");
    if !caps.is_empty() { assert_eq!(h.ammo, 0.0); }
    let before = t.state.resources.salvage;
    assert_eq!(economy::rearm(&mut t.state, p, &d, &mut Vec::new()).unwrap(), q.rearm.salvage);
    assert_eq!(before - t.state.resources.salvage, q.rearm.salvage);
    let before = t.state.resources.salvage;
    assert_eq!(economy::repair(&mut t.state, p, &d, &mut Vec::new()).unwrap(), q.repair.salvage);
    assert_eq!(before - t.state.resources.salvage, q.repair.salvage);

    // A lost hull: rebuild at 60% of build cost and 48 hours, never under Ironman.
    t.state.players[0].hull_lost = true;
    let q = model::service_quotes(&t.state, p, false, &d);
    let build = content::stats::compute_stats(&t.state.players[0].blueprint, &d.catalogue).cost as f64;
    assert!(q.rebuild.needed && q.rebuild.allowed && q.rebuild.hours == 48);
    assert!((q.rebuild.salvage as f64 - 0.6 * build).abs() <= 1.0, "{} vs {build}", q.rebuild.salvage);
    assert!(!model::service_quotes(&t.state, p, true, &d).rebuild.allowed);
    assert_eq!(model::hull_summary(&t.state, p, &d).damage, 1.0);
    // An unknown player has no quotes.
    assert_eq!(model::service_quotes(&t.state, PlayerSlot(5), false, &d), Default::default());
}

#[test]
fn intel_options_list_every_target_at_its_price() {
    let d = data();
    let mut t = theatre(1, false);
    let region = d.campaign.theatre.regions[0].id.clone();
    let card = d.campaign.cards.keys().next().unwrap().clone();
    t.state.regions.entry(region.clone()).or_default().queued_cards.push(QueuedCard { card: card.clone(), arrives_hour: 60 });
    let opts = model::intel_options(&t.state, &d);
    let find = |s: &IntelSpend| opts.iter().find(|o| &o.spend == s).map(|o| o.cost);
    assert_eq!(find(&IntelSpend::DelayCard { region: region.clone(), card: card.clone() }), Some(2));
    assert_eq!(find(&IntelSpend::CancelCard { region: region.clone(), card: card.clone() }), Some(5));
    assert_eq!(find(&IntelSpend::RevealConvoys(region.clone())), Some(1));
    // Closed regions offer no convoy reveal.
    for r in &d.campaign.theatre.regions {
        assert_eq!(find(&IntelSpend::RevealConvoys(r.id.clone())).is_some(), model::region_open(&t.state, &d, &r.id), "{:?}", r.id);
    }
    for o in &opts { if let IntelSpend::RevealAce(a) = &o.spend { assert_eq!(o.cost, 3); assert!(!t.state.ops.aces_revealed.contains(a)); } }
    // Every listed spend is one the theatre accepts when the pool can pay.
    t.state.resources.intel = 100;
    for o in &opts {
        let mut probe = t.state.clone();
        assert!(sim_systems::theatre::adaptation::spend_intel(&mut probe, &o.spend, &d, &mut Vec::new()).is_ok(), "{:?}", o.spend);
    }
}

#[test]
fn bulletin_lists_active_cards_then_queued_soonest_first() {
    let mut chart = theatre(1, false).chart();
    let r = RegionId::new("r");
    chart.regions.truncate(1);
    chart.regions[0].region = r.clone();
    chart.regions[0].active_cards = vec![CardId::new("on")];
    chart.regions[0].queued_cards = vec![(CardId::new("late"), 90), (CardId::new("soon"), 30)];
    let b = model::bulletin(&chart);
    let order: Vec<(&str, u64)> = b.iter().map(|c| (c.card.as_str(), c.arrives_hour)).collect();
    assert_eq!(order, vec![("on", 0), ("soon", 30), ("late", 90)]);
}

#[test]
fn act_info_and_starters() {
    let d = data();
    let t = theatre(1, false);
    let a = model::act_info(&t.state, &d);
    assert_eq!(a.act, content::enums::Act::Act1);
    assert_eq!(a.cap_t, 150.0);
    assert_eq!(a.clock_days_left, Some(40.0));
    assert_eq!(a.story_total, 4);
    assert_eq!(a.story_done, 0);
    assert!(!a.starters.is_empty());
    assert!(!a.open_theatre && !a.retreat);
    assert_eq!(model::preset_of(&t.state.difficulty, &d.campaign.difficulty), "normal");
    // Starter choice: any spelling of an Act 1 starter path; nothing else.
    assert_eq!(super::saves::act1_starter("starters/fac22.bp.ron", &d.campaign).as_deref(), Some("starters/fac22.bp.ron"));
    assert_eq!(super::saves::act1_starter("stock:starters/fac22.bp.ron", &d.campaign).as_deref(), Some("starters/fac22.bp.ron"));
    assert_eq!(super::saves::act1_starter("blueprints\\starters\\patrol15.bp.ron", &d.campaign).as_deref(), Some("starters/patrol15.bp.ron"));
    assert!(super::saves::act1_starter("starters/corvette40.bp.ron", &d.campaign).is_none());
    assert!(super::saves::act1_starter("../../etc/passwd", &d.campaign).is_none());
    // Every Act 1 starter is a legal campaign hull.
    for s in crate::content_api::model::starter_paths(&d.campaign.acts, Some(content::enums::Act::Act1)) {
        let bp = content::campaign::load::stock_blueprint(&s).unwrap();
        assert_eq!(model::validate_fielded(&bp, &bp, &t.state, &d), Ok(()), "{s}");
        // The dock offers what the theatre accepts: every part the starter carries stays fieldable,
        // and parts of unopened tiers that the detachment does not own stay out.
        let allowed = model::refit_parts(&t.state, &d, &bp);
        assert!(bp.parts.iter().all(|p| allowed.contains(&p.part)), "{s}");
        let tier = sim_systems::theatre::progress::open_tier(&t.state, &d);
        let foreign = d.catalogue.parts.values().find(|p| p.tier > tier && !bp.parts.iter().any(|q| q.part == p.id) && !t.state.resources.parts_store.contains_key(&p.id));
        if let Some(p) = foreign { assert!(!allowed.contains(&p.id), "{s}: {}", p.id.as_str()); }
    }
    assert_eq!(super::saves::clean_slot_name("  My\u{7} campaign \n"), "My campaign");
    assert_eq!(super::saves::clean_slot_name(&"x".repeat(100)).chars().count(), 64);
    assert_ne!(super::saves::fresh_seed(1), 0);
}

#[test]
fn local_slot_falls_back_to_player_zero() {
    let t = theatre(2, false);
    assert_eq!(model::local_slot(&t.state, 101), PlayerSlot(1));
    assert_eq!(model::local_slot(&t.state, 100), PlayerSlot(0));
    assert_eq!(model::local_slot(&t.state, 1), PlayerSlot(0));
}

#[test]
fn theatre_time_and_an_aborted_sortie_are_cheap() {
    let mut t = theatre(1, false);
    let _ = t.drain_events();
    // A wait runs in hours; each animated hour must stay well inside a frame.
    t.commit(&TheatreAction::Wait { hours: 24 }).unwrap();
    let start = std::time::Instant::now();
    let mut hours = 0;
    for _ in 0..24 {
        let before = t.state.hour;
        let _ = t.advance(1);
        let _ = t.drain_events();
        if t.state.hour == before { break; }
        hours += 1;
    }
    let per_hour = start.elapsed().as_secs_f64() * 1000.0 / hours.max(1) as f64;
    assert!(hours > 0);
    assert!(per_hour < 50.0, "{per_hour:.2} ms per theatre hour");
    // The chart and a briefing are rebuilt on change signals, never per frame; keep them inside a frame.
    let start = std::time::Instant::now();
    let _ = t.chart();
    let chart_ms = start.elapsed().as_secs_f64() * 1000.0;
    let op = t.state.offered.first().cloned().expect("an offer");
    let start = std::time::Instant::now();
    let _ = t.briefing(&op);
    let brief_ms = start.elapsed().as_secs_f64() * 1000.0;
    // About 0.1 ms and 13 ms on the development Mac; a briefing is cached per operation and revision.
    assert!(chart_ms < 16.0 && brief_ms < 100.0, "chart {chart_ms:.2} ms, briefing {brief_ms:.2} ms");
    // Launch the first offer and abort it: the offer stands and the theatre is between operations.
    let _ = t.commit(&TheatreAction::AcceptOperation(op.clone()));
    if t.start_sortie(&op, &[PlayerSlot(0)]).is_ok() {
        let r = super::aborted_result(&t);
        assert_eq!(r.operation.as_ref(), Some(&op));
        let _ = t.apply_result(&r);
        assert!(t.active().is_none());
        assert!(t.state.offered.contains(&op));
        assert_eq!(sim_systems::theatre::saves::can_save(&t), sim_systems::theatre::saves::SaveGate::Allowed);
    }
}

#[test]
fn fit_brief_reads_the_briefing() {
    let t = theatre(1, false);
    let op = t.state.offered.first().cloned().unwrap();
    let b = t.briefing(&op).expect("a briefing for an offer");
    let f = super::fit_brief(&b, &t.state);
    assert_eq!(f.tags, b.tags);
    assert_eq!(f.cap_t, 150.0);
    assert!(f.approach_km >= 0.0);
    let mut sorted = f.threats.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted, f.threats);
}

// ---- Played time and the log ----

#[test]
fn played_time_drops_long_pauses() {
    let mut c = model::PlayedClock::default();
    let mut total = 0;
    for _ in 0..600 { total += c.tick(0.1, true); }
    assert!((59..=60).contains(&total), "{total}");
    let base = total;
    // 100 s idle, then activity: counted.
    for _ in 0..1000 { total += c.tick(0.1, false); }
    c.activity();
    total += c.tick(0.0, false);
    assert!((99..=101).contains(&(total - base)), "{}", total - base);
    let base = total;
    // 200 s idle: none of it counts.
    for _ in 0..2000 { total += c.tick(0.1, false); }
    c.activity();
    total += c.tick(0.0, false);
    assert!(total - base <= 1, "{}", total - base);
    // Bad deltas count nothing; a long frame counts at most one second.
    let base = total + c.tick(0.0, true);
    assert_eq!(c.tick(f64::NAN, true) + c.tick(-3.0, true), 0);
    assert!(c.tick(30.0, true) <= 2);
    let _ = base;
}

#[test]
fn theatre_log_keeps_the_newest() {
    let mut log = model::TheatreLog::default();
    for h in 0..(model::LOG_CAP as u64 + 50) { log.push(h, TheatreEvent::ClockExpired); }
    assert_eq!(log.entries.len(), model::LOG_CAP);
    let tail: Vec<u64> = log.tail(3).map(|e| e.0).collect();
    let last = model::LOG_CAP as u64 + 49;
    assert_eq!(tail, vec![last - 2, last - 1, last]);
    assert_eq!(log.tail(0).count(), 0);
    assert!(words::logged(&TheatreEvent::ClockExpired) && !words::logged(&TheatreEvent::HourAdvanced { hour: 1 }));
}

// ---- Saves on disk ----

fn save_of(t: &mut Theatre) -> SaveGame { t.to_save("0.1.0") }

#[test]
fn saves_round_trip_under_non_ascii_and_long_paths() {
    let base = temp_root("paths");
    let mut root = base.join("Ünïcødé 名前 Données");
    // Push the full file path past 260 characters (Windows' old limit).
    while root.as_os_str().len() < 240 { root = root.join("a long folder name for the saves test"); }
    let mut t = theatre(1, true);
    let save = save_of(&mut t);
    let path = store::write_save(&root, &save, store::IRONMAN_STEM).expect("write");
    assert!(path.as_os_str().len() > 260, "{}", path.as_os_str().len());
    assert_eq!(store::read_save(&path).expect("read"), save);
    // Overwriting keeps one file and leaves no temporary behind.
    let save2 = save_of(&mut t);
    store::write_save(&root, &save2, store::IRONMAN_STEM).unwrap();
    let dir = store::campaign_dir(&root, save.header.campaign_id);
    let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    assert_eq!(names, vec!["ironman.bpsave".to_string()]);
    assert_eq!(store::read_save(&path).unwrap().header.sequence, save2.header.sequence);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn resolve_refuses_anything_outside_the_saves_folder() {
    let root = PathBuf::from("/saves");
    let ok = store::resolve(&root, "user://saves/00000000000000ff/autosave.bpsave").unwrap();
    assert_eq!(ok, root.join("00000000000000ff").join("autosave.bpsave"));
    assert!(store::resolve(&root, "00000000000000ff/save-00000003.bpsave").is_some());
    for bad in ["user://saves/../secret.bpsave", "user://saves/00000000000000ff/../../x.bpsave", "user://saves/notacampaign/autosave.bpsave",
        "user://saves/00000000000000ff/autosave.ron", "user://saves/00000000000000ff/a/b.bpsave", "user://blueprints/x.bpsave", "/etc/passwd", "", "user://saves/00000000000000ff/.bpsave"] {
        assert!(store::resolve(&root, bad).is_none(), "{bad}");
    }
}

#[test]
fn listing_suspend_and_pruning() {
    let root = temp_root("list");
    let mut a = theatre(1, false);
    let mut b = theatre(2, false);
    let mut sa = save_of(&mut a);
    sa.header.campaign_id = 0xA;
    let mut sb = save_of(&mut b);
    sb.header.campaign_id = 0xB;
    store::write_save(&root, &sa, store::AUTOSAVE_STEM).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    store::write_save(&root, &sb, &store::manual_stem(sb.header.sequence)).unwrap();
    // A damaged file is listed, not loadable.
    std::fs::write(store::campaign_dir(&root, 0xA).join("save-00000099.bpsave"), b"not a save").unwrap();
    let mut cache = store::SlotCache::default();
    let list = store::list(&root, &mut cache);
    assert_eq!(list.len(), 3);
    assert!(list.iter().any(|s| s.problem.is_some() && s.header.is_none()));
    let b_pos = list.iter().position(|s| s.header.as_ref().is_some_and(|h| h.campaign_id == 0xB)).unwrap();
    let a_pos = list.iter().position(|s| s.header.as_ref().is_some_and(|h| h.campaign_id == 0xA)).unwrap();
    assert!(b_pos < a_pos, "newest first");
    let bslot = list[b_pos].clone();
    assert_eq!(bslot.players, vec!["p0".to_string(), "p1".to_string()]);
    assert_eq!(bslot.act, Some(content::enums::Act::Act1));
    // Cached: the same entry comes back while the file is unchanged.
    let again = store::slot_info(&root, &bslot.path, &mut cache);
    assert!(Arc::ptr_eq(&again, &bslot));

    // One suspend slot in all: writing another campaign's removes the first.
    let cp = content::save::SortieCheckpoint { tick: 3600, keyframe_format: 1, config: vec![1, 2, 3], keyframe: vec![9; 1000] };
    let s1 = SuspendSave { header: sa.header.clone(), theatre: sa.theatre.clone(), sortie: cp.clone() };
    let s2 = SuspendSave { header: sb.header.clone(), theatre: sb.theatre.clone(), sortie: cp.clone() };
    let p1 = store::write_suspend(&root, &s1).unwrap();
    assert!(p1.is_file());
    let p2 = store::write_suspend(&root, &s2).unwrap();
    assert!(!p1.exists() && p2.is_file());
    let found = store::find_suspend(&root, &mut cache).expect("suspend");
    assert!(found.is_suspend && found.problem.is_none());
    assert_eq!(store::read_suspend(&p2).unwrap(), s2);
    assert!(store::delete(&p2));
    assert!(store::find_suspend(&root, &mut cache).is_none());
    assert!(store::delete(&p2), "a missing file counts as removed");

    // The newest full save of a campaign by sequence.
    let mut later = sb.clone();
    later.header.sequence += 5;
    store::write_save(&root, &later, store::AUTOSAVE_STEM).unwrap();
    assert_eq!(store::latest_save(&root, 0xB).unwrap().header.sequence, later.header.sequence);

    // Only the newest manual saves stay.
    for seq in 0..(store::MANUAL_KEEP as u64 + 5) {
        let mut s = sb.clone();
        s.header.sequence = 1000 + seq;
        store::write_save(&root, &s, &store::manual_stem(s.header.sequence)).unwrap();
    }
    store::prune_manual(&root, 0xB);
    let manual: Vec<String> = std::fs::read_dir(store::campaign_dir(&root, 0xB)).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.starts_with("save-")).collect();
    assert_eq!(manual.len(), store::MANUAL_KEEP);
    assert!(manual.contains(&store::file_name(&store::manual_stem(1000 + store::MANUAL_KEEP as u64 + 4))));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn suspend_resume_advances_the_sequence() {
    let mut t = theatre(1, false);
    let first = save_of(&mut t);
    let cp = content::save::SortieCheckpoint { tick: 3600, keyframe_format: 1, config: Vec::new(), keyframe: Vec::new() };
    let s = sim_systems::theatre::saves::make_suspend(&t, cp, "0.1.0");
    assert_eq!(s.header.sequence, first.header.sequence + 1);
    let (resumed, _) = sim_systems::theatre::saves::resume_suspend(s.clone(), data());
    assert_eq!(resumed.header().sequence, s.header.sequence + 1);
}

// ---- Co-op copies ----

#[test]
fn save_copies_round_trip_and_reconcile() {
    let mut t = theatre(2, false);
    let a = save_of(&mut t);
    let bytes = coop::encode_copy(&a);
    assert_eq!(coop::decode_copy(&bytes), Some(a.clone()));
    assert_eq!(coop::decode_copy(b"garbage"), None);
    assert_eq!(coop::decode_copy(&bytes[..bytes.len() / 2]), None);
    // Newer: kept. Older: not written. Forked (same sequence, another chain): refused.
    let b = save_of(&mut t);
    assert_eq!(coop::copy_verdict(None, &a), coop::CopyVerdict::Keep);
    assert_eq!(coop::copy_verdict(Some(&a), &b), coop::CopyVerdict::Keep);
    assert_eq!(coop::copy_verdict(Some(&b), &b), coop::CopyVerdict::Keep);
    assert_eq!(coop::copy_verdict(Some(&b), &a), coop::CopyVerdict::Refuse);
    let mut fork = b.clone();
    fork.header.played_s += 99;
    fork.header.prev_hash = content::ids::ContentHash([7; 32]);
    assert_eq!(coop::copy_verdict(Some(&b), &fork), coop::CopyVerdict::Refuse);
    // Another campaign's copy is a new campaign for this member.
    let mut other = a.clone();
    other.header.campaign_id ^= 1;
    assert_eq!(coop::copy_verdict(Some(&b), &other), coop::CopyVerdict::Keep);
}

#[test]
fn ironman_and_campaign_over() {
    let mut t = theatre(1, true);
    assert!(!sim_systems::theatre::saves::campaign_over(&t));
    t.state.players[0].hull_lost = true;
    assert!(sim_systems::theatre::saves::campaign_over(&t));
    let q = model::service_quotes(&t.state, PlayerSlot(0), true, &data());
    assert!(q.rebuild.needed && !q.rebuild.allowed);
}

/// This build ends at Act 1 (L2283): the clock running out restarts Act 1 in retreat (no Act 2 row ships), and
/// the log line and the clock bar's interrupt say that Act 1 is the end of this build. With every act row
/// loaded the same events keep the ordinary wording.
#[test]
fn act_one_end_says_it_is_the_end_of_this_build() {
    use content::enums::Act;
    let lk = CsvLookup::load();
    let shipped = TheatreData { campaign: Arc::new(content::campaign::load::shipped_campaign().clone()), catalogue: Arc::new(content::catalogue::catalogue().clone()) };
    let new = NewCampaign {
        seed: 7, difficulty: content::campaign::DifficultySliders::NORMAL, ironman: false, slot_name: "end".into(),
        players: vec![(100, "p0".into(), starter())], start_act: Act::Act1,
    };
    let mut t = Theatre::new_campaign(new, shipped.clone());
    t.state.clock_hours_left = Some(1.0);
    let _ = t.drain_events();
    t.advance_hour();
    assert_eq!((t.state.act, t.state.retreat), (Act::Act1, true), "Act 1 starts again in retreat");
    let n = words::Names { campaign: &shipped.campaign, catalogue: &shipped.catalogue, players: vec![], act: Some(t.state.act) };
    let lost = t.drain_events().into_iter().find(|e| matches!(e, TheatreEvent::ActLost { .. })).expect("the act is lost");
    let line = text(&lk, &words::event_msg(&lost, 10, &n));
    assert!(filled(&line) && line.contains("Act 1") && line.to_lowercase().contains("this build") && !line.contains("next act"), "{line}");
    let banner = text(&lk, &words::interrupt_msg(&Interrupt::ActEnded, &n));
    assert!(filled(&banner) && banner.contains("Act 1") && banner.to_lowercase().contains("this build"), "{banner}");
    let won = text(&lk, &words::event_msg(&TheatreEvent::ActWon { act: Act::Act1 }, 10, &n));
    assert!(filled(&won) && won.to_lowercase().contains("this build"), "{won}");
    // With all four rows (a later release), Act 1's end reads as before.
    let full = data();
    let n = words::Names { campaign: &full.campaign, catalogue: &full.catalogue, players: vec![], act: Some(Act::Act1) };
    assert_eq!(text(&lk, &words::event_msg(&TheatreEvent::ActWon { act: Act::Act1 }, 10, &n)), "Act 1 won.");
    assert!(!text(&lk, &words::interrupt_msg(&Interrupt::ActEnded, &n)).to_lowercase().contains("this build"));
}
