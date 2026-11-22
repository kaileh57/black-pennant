//! Theatre flow tests (unit `sys-theatre`): new campaign, offers, sortie round trip, time actions, co-op
//! proposals, forced operations and Ironman.
use super::alert::tests::result;
use super::logistics::testkit::{id, rid};
use super::opgen::tests::gen_data;
use super::*;
use crate::protocol::config::PlayerResult;

fn new_theatre(players: usize, ironman: bool) -> Theatre {
    let bp = content::blueprint::io::new_blueprint("lantern", 15.0);
    let new = NewCampaign {
        seed: 11, difficulty: DifficultySliders::NORMAL, ironman, slot_name: "slot".into(),
        players: (0..players).map(|i| (100 + i as u64, format!("p{i}"), bp.clone())).collect(), start_act: Act::Act1,
    };
    Theatre::new_campaign(new, gen_data())
}

fn solo() -> PlayerMask { PlayerMask(0).with(PlayerSlot(0)) }
fn both() -> PlayerMask { PlayerMask(0).with(PlayerSlot(0)).with(PlayerSlot(1)) }

fn first_template(t: &Theatre) -> OperationId {
    t.state.offered.iter().find(|o| matches!(t.data.campaign.operations[*o].kind, content::campaign::OperationKind::Template(_))).cloned().expect("a template on offer")
}

fn player_result(lost: bool) -> PlayerResult {
    PlayerResult { player: PlayerSlot(0), hull_lost: lost, damage: Default::default(), fuel_fraction: 0.5, ammo: BTreeMap::new(), expended: BTreeMap::new(), ammo_kg_expended: BTreeMap::new(), fuel_l_used: 10.0, passengers_lost: 0, dropped: false }
}

/// Advance past informational interrupts to the one that ends the time action.
fn run(t: &mut Theatre) -> Interrupt {
    for _ in 0..50 {
        let stop = t.advance(500);
        if !matches!(stop, Interrupt::OperationOffered(_) | Interrupt::CardArriving { .. } | Interrupt::HullReady(_) | Interrupt::ClockLow { .. } | Interrupt::HunterGroupWarning { .. }) { return stop; }
    }
    Interrupt::Paused
}

#[test]
fn new_campaign_opens_act_one() {
    let mut t = new_theatre(1, false);
    assert_eq!(t.state.act, Act::Act1);
    assert_eq!(t.state.sextant_node, id("C1"));
    assert_eq!(t.state.clock_hours_left, Some(960.0));
    assert_eq!(t.state.resources.salvage, STARTING_SALVAGE);
    assert!(!t.state.hulls.is_empty());
    assert!(t.state.offered.len() >= 2);
    let ev = t.drain_events();
    assert!(ev.iter().any(|e| matches!(e, TheatreEvent::ActStarted { act: Act::Act1, retreat: false })));
    assert!(ev.iter().any(|e| matches!(e, TheatreEvent::OperationOffered(_))));
    assert!(!ev.iter().any(|e| matches!(e, TheatreEvent::SextantMoved { .. })));
    let chart = t.chart();
    assert_eq!(chart.offers.len(), t.state.offered.len());
    assert_eq!(chart.regions.len(), 2);
    assert!(chart.regions[0].open && !chart.regions[1].open);
    // Time does not move without a committed action.
    assert_eq!(t.advance(10), Interrupt::WaitDone);
    assert_eq!(t.state.hour, 0);
    assert_eq!(saves::can_save(&t), saves::SaveGate::Allowed);
}

#[test]
fn sortie_round_trip() {
    let mut t = new_theatre(1, false);
    let op = first_template(&t);
    let b = t.briefing(&op).expect("briefing");
    t.command(PlayerSlot(0), true, solo(), TheatreCommand::Propose(TheatreAction::AcceptOperation(op.clone()))).unwrap();
    assert_eq!(t.accepted(), Some(&op));
    let config = t.start_sortie(&op, &[PlayerSlot(0)]).unwrap();
    assert_eq!(config.seed, b.seed);
    assert_eq!(config.weather, b.weather);
    assert_eq!(saves::can_save(&t), saves::SaveGate::InSortie);
    assert_eq!(t.start_sortie(&op, &[PlayerSlot(0)]), Err(TheatreReject::NotAtCove));
    // A won sortie with one sunk record.
    let victim = config.vessels.iter().find_map(|v| v.hull_record).expect("a record in the sortie");
    let mut k = super::alert::tests::kill(false);
    k.hull_record = Some(victim);
    let mut r = result(vec![k], 0, 0);
    r.operation = Some(op.clone());
    r.duration_ticks = 60 * 60 * 25;
    r.players = vec![player_result(false)];
    let aa = t.apply_result(&r);
    assert_eq!(aa.operation, Some(op.clone()));
    assert!(t.active().is_none());
    assert!(!t.state.hulls.iter().find(|h| h.id == victim).unwrap().alive);
    assert!(t.state.ops.completed.contains(&op));
    assert!(!t.state.offered.contains(&op));
    assert_eq!(t.state.players[0].fuel_fraction, 0.5);
    assert_eq!(aa.clock_pushback_days, super::progress::pushback_days(&t.data.campaign.operations[&op], &t.data));
    assert_eq!(aa.kills_by_method.len(), 1);
    // The operation's hours (the sortie and LANTERN's passage) have run: the report is between operations.
    assert!(t.state.hour >= 1);
    assert!(aa.events.iter().any(|e| matches!(e, TheatreEvent::HourAdvanced { .. })));
    assert_eq!(saves::can_save(&t), saves::SaveGate::Allowed);
    assert_eq!(t.advance(10), Interrupt::WaitDone);
}

#[test]
fn ironman_reaches_the_sortie() {
    let mut t = new_theatre(1, true);
    let op = first_template(&t);
    assert!(t.start_sortie(&op, &[PlayerSlot(0)]).unwrap().settings.ironman);
}

#[test]
fn transit_between_open_coves() {
    let bp = content::blueprint::io::new_blueprint("lantern", 15.0);
    let new = NewCampaign { seed: 3, difficulty: DifficultySliders::NORMAL, ironman: false, slot_name: "s".into(), players: vec![(1, "a".into(), bp)], start_act: Act::Act2 };
    let mut t = Theatre::new_campaign(new, gen_data());
    assert_eq!(t.state.sextant_node, id("C1"));
    assert!(t.state.open_coves.contains(&id("C2")));
    t.drain_events();
    t.command(PlayerSlot(0), true, solo(), TheatreCommand::Propose(TheatreAction::Transit(id("C2")))).unwrap();
    assert_eq!(t.commit(&TheatreAction::Transit(id("C1"))), Err(TheatreReject::NotAtCove), "one passage at a time");
    assert_eq!(run(&mut t), Interrupt::Arrived(id("C2")));
    assert_eq!(t.state.sextant_node, id("C2"));
    assert!(t.drain_events().iter().any(|e| matches!(e, TheatreEvent::SextantMoved { to, .. } if *to == id("C2"))));
    assert_eq!(saves::can_save(&t), saves::SaveGate::Allowed);
}

/// 45 theatre days of waiting with nothing played: no panic, a template always on offer, the offer counts
/// stay put, and the clock running out loses Act 1 into an Act 2 retreat.
#[test]
fn long_wait_soak() {
    let mut t = new_theatre(1, false);
    let counts = t.state.ops.offered.clone();
    let mut ended = false;
    for _ in 0..45 {
        t.commit(&TheatreAction::Wait { hours: 24 }).unwrap();
        loop {
            match t.advance(500) {
                Interrupt::ActEnded => { ended = true; }
                Interrupt::WaitDone => break,
                Interrupt::Paused => panic!("stuck"),
                _ => {}
            }
            if t.pending.is_none() { break; }
        }
        assert!(t.state.offered.iter().any(|o| matches!(t.data.campaign.operations[o].kind, content::campaign::OperationKind::Template(_))));
        if !ended { assert_eq!(t.state.ops.offered, counts); }
    }
    assert!(ended);
    assert_eq!(t.state.act, Act::Act2);
    assert!(t.state.retreat);
}

#[test]
fn aborted_result_changes_nothing() {
    let mut t = new_theatre(1, false);
    let op = first_template(&t);
    t.start_sortie(&op, &[]).unwrap();
    let before = t.state.clone();
    let mut r = result(vec![super::alert::tests::kill(true)], 3, 3);
    r.outcome = crate::protocol::config::Outcome::Aborted;
    r.operation = Some(op.clone());
    t.apply_result(&r);
    assert_eq!(t.state, before);
    // Back at the chart table with the offer standing.
    assert!(t.active().is_none());
    assert!(t.start_sortie(&op, &[]).is_ok());
}

#[test]
fn waits_and_transits() {
    let mut t = new_theatre(1, false);
    t.commit(&TheatreAction::Wait { hours: 5 }).unwrap();
    assert_eq!(saves::can_save(&t), saves::SaveGate::NotAtCoveOrBetweenOperations);
    assert_eq!(run(&mut t), Interrupt::WaitDone);
    assert_eq!(t.state.hour, 5);
    // C2 lies in a region that opens in Act 2; unknown nodes are refused.
    assert_eq!(t.commit(&TheatreAction::Transit(id("C2"))), Err(TheatreReject::UnknownTarget));
    assert_eq!(t.commit(&TheatreAction::Transit(id("nowhere"))), Err(TheatreReject::UnknownTarget));
    // Pausing stops the animation without moving time.
    t.commit(&TheatreAction::Wait { hours: 2 }).unwrap();
    t.command(PlayerSlot(0), true, solo(), TheatreCommand::PauseClock(true)).unwrap();
    assert_eq!(t.advance(10), Interrupt::Paused);
    assert_eq!(t.state.hour, 5);
}

#[test]
fn coop_proposals_need_everyone_or_the_host() {
    let mut t = new_theatre(2, false);
    t.command(PlayerSlot(1), false, both(), TheatreCommand::Propose(TheatreAction::Wait { hours: 3 })).unwrap();
    assert_eq!(t.proposals.len(), 1);
    assert!(t.pending.is_none());
    let id0 = t.proposals[0].id;
    assert_eq!(t.command(PlayerSlot(1), false, both(), TheatreCommand::Confirm(id0)), Err(TheatreReject::NotHost));
    t.command(PlayerSlot(0), true, both(), TheatreCommand::Respond { proposal: id0, accept: false }).unwrap();
    assert!(t.pending.is_none());
    t.command(PlayerSlot(0), true, both(), TheatreCommand::Respond { proposal: id0, accept: true }).unwrap();
    assert!(t.proposals.is_empty());
    assert!(matches!(t.pending, Some(Pending::Wait { until_hour: 3 })));
    // Host confirmation commits without the other vote.
    t.command(PlayerSlot(1), false, both(), TheatreCommand::Propose(TheatreAction::Wait { hours: 6 })).unwrap();
    let id1 = t.proposals[0].id;
    t.command(PlayerSlot(0), true, both(), TheatreCommand::Confirm(id1)).unwrap();
    assert!(matches!(t.pending, Some(Pending::Wait { until_hour: 6 })));
    // Bad proposals are refused up front; unknown proposals too.
    assert_eq!(t.command(PlayerSlot(1), false, both(), TheatreCommand::Propose(TheatreAction::AcceptOperation(OperationId::new("nope")))), Err(TheatreReject::OperationNotOffered));
    assert_eq!(t.command(PlayerSlot(1), false, both(), TheatreCommand::Respond { proposal: ProposalId(999), accept: true }), Err(TheatreReject::NoSuchProposal));
    // Only the host sets difficulty or saves in co-op.
    assert_eq!(t.command(PlayerSlot(1), false, both(), TheatreCommand::SetDifficulty(DifficultySliders::HARD)), Err(TheatreReject::NotHost));
    assert_eq!(t.command(PlayerSlot(1), false, both(), TheatreCommand::Save), Err(TheatreReject::NotHost));
}

#[test]
fn forced_operation_blocks_everything_else() {
    let mut t = new_theatre(1, false);
    let hulls: Vec<u32> = t.state.hulls.iter().filter(|h| h.alive && h.ace.is_none()).take(5).map(|h| h.id).collect();
    let node = t.state.sextant_node.clone();
    t.state.groups.push(content::save::TaskGroupState { id: 9_999, hulls, node, destination: None, order: content::save::GroupOrder::HuntSextant, arrive_hour: None, belief: vec![] });
    t.state.hunter_group = Some(9_999);
    assert_eq!(t.commit(&TheatreAction::Refit), Err(TheatreReject::ForcedOperationPending));
    let hunt = OperationId::new("op.hunt");
    assert_eq!(t.advance(10), Interrupt::ForcedOperation(hunt.clone()));
    t.refresh();
    assert_eq!(t.state.offered, vec![hunt.clone()]);
    assert_eq!(t.commit(&TheatreAction::DeclineOperation(hunt.clone())), Err(TheatreReject::ForcedOperationPending));
    let config = t.start_sortie(&hunt, &[PlayerSlot(0)]).unwrap();
    assert!(config.vessels.iter().any(|v| v.hull_record.is_some()));
    let mut r = result(vec![], 0, 0);
    r.operation = Some(hunt.clone());
    t.apply_result(&r);
    // The hunt is resolved: the group is released and the operation leaves the chart.
    assert!(t.state.hunter_group.is_none());
    assert!(!t.state.offered.contains(&hunt));
}

#[test]
fn ironman_locks_and_ends() {
    let mut t = new_theatre(1, true);
    assert_eq!(t.command(PlayerSlot(0), true, solo(), TheatreCommand::SetDifficulty(DifficultySliders::EASY)), Err(TheatreReject::IronmanLocked));
    let op = first_template(&t);
    t.start_sortie(&op, &[PlayerSlot(0)]).unwrap();
    let mut r = result(vec![], 0, 0);
    r.operation = Some(op);
    r.outcome = crate::protocol::config::Outcome::Defeat;
    r.objective_done = false;
    r.players = vec![player_result(true)];
    let aa = t.apply_result(&r);
    assert_eq!(aa.hulls_lost, vec![PlayerSlot(0)]);
    assert!(aa.events.iter().any(|e| matches!(e, TheatreEvent::CampaignOver)));
    assert!(saves::campaign_over(&t));
    assert_eq!(t.command(PlayerSlot(0), true, solo(), TheatreCommand::Rebuild), Err(TheatreReject::CampaignOver));
    assert_eq!(t.advance(5), Interrupt::Paused);
}

#[test]
fn difficulty_changes_shift_the_clock_but_not_the_hull_count() {
    let mut t = new_theatre(1, false);
    let mut hard = DifficultySliders::HARD;
    hard.ai_accuracy = f64::NAN;
    t.command(PlayerSlot(0), true, solo(), TheatreCommand::SetDifficulty(hard)).unwrap();
    assert_eq!(t.state.difficulty.hull_count, 300);
    assert_eq!(t.state.difficulty.clock_days, 32);
    assert_eq!(t.state.difficulty.ai_accuracy, 1.0);
    assert_eq!(t.state.clock_hours_left, Some(32.0 * 24.0));
    assert_eq!(t.state.regions[&rid("r1")].alert, 0.0);
}

/// Park a hunter group of five live records at SEXTANT's cove.
fn park_hunter(t: &mut Theatre) {
    let hulls: Vec<u32> = t.state.hulls.iter().filter(|h| h.alive && h.ace.is_none()).take(5).map(|h| h.id).collect();
    let node = t.state.sextant_node.clone();
    t.state.groups.push(content::save::TaskGroupState { id: 9_999, hulls, node, destination: None, order: content::save::GroupOrder::HuntSextant, arrive_hour: None, belief: vec![] });
    t.state.hunter_group = Some(9_999);
    t.refresh();
}

#[test]
fn forced_operation_waits_for_a_hull_in_the_dock() {
    let mut t = new_theatre(1, false);
    park_hunter(&mut t);
    let hunt = OperationId::new("op.hunt");
    let ready = t.state.hour + 4;
    t.state.players[0].ready_at_hour = Some(ready);
    assert_eq!(t.start_sortie(&hunt, &[PlayerSlot(0)]), Err(TheatreReject::HullNotReady));
    assert_eq!(t.start_sortie(&hunt, &[]), Err(TheatreReject::HullNotReady));
    // Transits stay refused; the refit may run to its end.
    assert_eq!(t.commit(&TheatreAction::Transit(t.state.sextant_node.clone())), Err(TheatreReject::ForcedOperationPending));
    t.commit(&TheatreAction::Refit).unwrap();
    assert_eq!(t.advance(10), Interrupt::ForcedOperation(hunt.clone()));
    assert_eq!(t.state.hour, ready);
    // The hull can sail: the hunt holds time still again.
    assert_eq!(t.commit(&TheatreAction::Wait { hours: 1 }), Err(TheatreReject::ForcedOperationPending));
    assert!(t.start_sortie(&hunt, &[]).is_ok());
}

#[test]
fn forced_operation_waits_for_a_rebuild() {
    let mut t = new_theatre(1, false);
    park_hunter(&mut t);
    let hunt = OperationId::new("op.hunt");
    t.state.players[0].hull_lost = true;
    t.state.resources.salvage = 1_000_000;
    t.command(PlayerSlot(0), true, solo(), TheatreCommand::Rebuild).unwrap();
    let ready = t.state.players[0].ready_at_hour.expect("rebuild sets a ready hour");
    t.commit(&TheatreAction::Wait { hours: 100 }).unwrap();
    assert_eq!(t.advance(200), Interrupt::ForcedOperation(hunt.clone()));
    assert_eq!(t.state.hour, ready);
    assert!(t.start_sortie(&hunt, &[]).is_ok());
}

#[test]
fn forced_operation_returns_template_offers() {
    let mut t = new_theatre(1, false);
    let total = |t: &Theatre| t.state.ops.offered.values().map(|n| *n as u32).sum::<u32>();
    let before = total(&t);
    assert!(before > 0);
    park_hunter(&mut t);
    assert_eq!(t.state.offered, vec![OperationId::new("op.hunt")]);
    assert_eq!(total(&t), before - 3.min(before));
    t.state.hunter_group = None;
    t.state.groups.retain(|g| g.id != 9_999);
    t.refresh();
    assert_eq!(total(&t), before);
}

#[test]
fn commands_from_unknown_slots_are_refused() {
    let mut t = new_theatre(1, false);
    let mask = PlayerMask(0).with(PlayerSlot(0)).with(PlayerSlot(7));
    assert_eq!(t.command(PlayerSlot(7), true, mask, TheatreCommand::Propose(TheatreAction::Wait { hours: 3 })), Err(TheatreReject::UnknownTarget));
    assert_eq!(t.command(PlayerSlot(200), true, solo(), TheatreCommand::Rearm), Err(TheatreReject::UnknownTarget));
    assert!(t.pending.is_none());
}
