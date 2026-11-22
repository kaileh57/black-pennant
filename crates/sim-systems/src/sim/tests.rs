//! Tests of unit `sys-sim` and `sys-sim-keyframe` on a small real Sim: a splash basin with one stock starter
//! hull. The end-to-end battle is `tests/reference_battle.rs`.
use super::*;
use crate::protocol::command::{Command, ControlRole, Query, ShotDesignation, TimeRate};
use crate::protocol::config::Outcome;
use crate::protocol::event::{RejectReason, SimEvent};
use content::settings::{SplashOptions, SplashTarget};
use content::weather::SeaState;
use std::sync::OnceLock;

const P0: PlayerSlot = PlayerSlot(0);

fn data() -> &'static (Arc<Catalogue>, Arc<CampaignData>) {
    static DATA: OnceLock<(Arc<Catalogue>, Arc<CampaignData>)> = OnceLock::new();
    DATA.get_or_init(|| {
        let catalogue = Arc::new(Catalogue::load_embedded().expect("catalogue"));
        let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign"));
        (catalogue, campaign)
    })
}

fn deps() -> SimDeps {
    let (catalogue, campaign) = data().clone();
    SimDeps { catalogue, campaign, designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Budgets::default(), ladder: LadderTuning::default() }
}

fn basin_config(target: SplashTarget) -> SortieConfig {
    let bp = content::campaign::load::stock_blueprint("starters/fac22.bp.ron").expect("the 22 m starter");
    crate::editor::splash::splash_config(&bp, SplashOptions { water: SeaState::CALM, target }, 11, &data().1)
}

fn basin(target: SplashTarget) -> Sim { Sim::new(basin_config(target), deps()).expect("the basin starts") }

fn cmd(seq: u32, command: Command) -> CommandEnvelope { CommandEnvelope { player: P0, seq, client_render_tick: 0, rtt_ms: 0, command } }

fn rejections(events: &[OutEvent]) -> Vec<(u32, RejectReason)> {
    events.iter().filter_map(|e| if let SimEvent::CommandRejected { seq, reason, .. } = &e.event { Some((*seq, *reason)) } else { None }).collect()
}

#[test]
fn orders_wait_out_a_pause_and_apply_when_time_resumes() {
    let mut sim = basin(SplashTarget::Drones);
    for _ in 0..3 { sim.step(); }
    sim.drain_events();
    sim.apply(&[cmd(1, Command::Pause(true))]);
    sim.step();
    assert!(sim.pacing().paused);
    assert_eq!(sim.tick().0, 3, "a paused step does not advance the clock");
    assert!(sim.drain_events().iter().any(|e| matches!(&e.event, SimEvent::PacingChanged(p) if p.paused)));

    // Orders given while paused wait, in order; time and alert commands do not.
    sim.apply(&[cmd(3, Command::SetSpeed { speed_kn: 12.0 }), cmd(2, Command::SetCourse { waypoints: vec![DVec2::new(500.0, 0.0)], speed_kn: None, append: false })]);
    sim.step();
    sim.step();
    assert_eq!(sim.tick().0, 3);
    let ship = sim.ships().of_player(P0).expect("ship");
    assert!(ship.orders.route.is_empty(), "the course is not laid while paused");
    assert_eq!(sim.pending.len(), 2);
    assert!(rejections(&sim.drain_events()).is_empty());

    // The resume carries a higher sequence number than the waiting orders: they still apply, in order.
    sim.apply(&[cmd(4, Command::Pause(false))]);
    sim.step();
    assert_eq!(sim.tick().0, 4, "the step that ends the pause runs a tick");
    let ship = sim.ships().of_player(P0).expect("ship");
    assert_eq!(ship.orders.route, vec![DVec2::new(500.0, 0.0)]);
    assert_eq!(ship.orders.speed_kn, Some(12.0));
    assert!(sim.pending.is_empty());
    assert!(rejections(&sim.drain_events()).is_empty());

    // Resends are duplicates in both command kinds, and the first application stands.
    sim.apply(&[cmd(3, Command::SetSpeed { speed_kn: 30.0 }), cmd(4, Command::Pause(true))]);
    sim.step();
    assert_eq!(rejections(&sim.drain_events()), vec![(3, RejectReason::Duplicate), (4, RejectReason::Duplicate)]);
    assert!(!sim.pacing().paused);
    assert_eq!(sim.ships().of_player(P0).expect("ship").orders.speed_kn, Some(12.0));
}

#[test]
fn bad_commands_are_refused_with_a_reason_and_never_panic() {
    let mut sim = basin(SplashTarget::PatrolBoat);
    for _ in 0..2 { sim.step(); }
    sim.drain_events();
    let strangers = CommandEnvelope { player: PlayerSlot(5), ..cmd(1, Command::Pause(true)) };
    sim.apply(&[
        cmd(1, Command::SetCourse { waypoints: vec![DVec2::new(f64::NAN, 0.0)], speed_kn: None, append: false }),
        cmd(2, Command::Designate { track: Some(TrackId(9_999)) }),
        cmd(3, Command::Fire { role: ControlRole::Helm, shot: ShotDesignation::Direction { dir_local: DVec3::X } }),
        cmd(4, Command::StartAction { action: content::campaign::StationAction::LayMines, target: crate::protocol::command::ActionTarget::None }),
        cmd(5, Command::Acknowledge(Some(AlertId(77)))),
        cmd(6, Command::Query(Query::TrackCard(TrackId(9_999)))),
        cmd(7, Command::Query(Query::Why(PartSlot(60_000)))),
        cmd(8, Command::SetStance { track: TrackId(9_999), stance: Some(Stance::Hostile) }),
        cmd(9, Command::CancelAction(ActionId(3))),
        cmd(10, Command::Prize { prize: EntityId::NONE, decision: crate::protocol::command::PrizeDecision::StripOnSpot }),
        cmd(11, Command::SplashRewind { ticks: u32::MAX }),
        strangers,
    ]);
    sim.step();
    let events = sim.drain_events();
    let got = rejections(&events);
    let reason = |seq: u32| got.iter().find(|r| r.0 == seq).map(|r| r.1);
    assert_eq!(reason(1), Some(RejectReason::InvalidValue));
    assert_eq!(reason(2), Some(RejectReason::NoSuchTrack));
    assert_eq!(reason(3), Some(RejectReason::InvalidValue));
    assert_eq!(reason(4), Some(RejectReason::NotAllowedInMode), "a basin offers no station actions");
    assert_eq!(reason(5), Some(RejectReason::InvalidValue));
    assert_eq!(reason(6), Some(RejectReason::NoSuchTrack));
    assert_eq!(reason(7), Some(RejectReason::NoSuchPart));
    assert_eq!(reason(8), Some(RejectReason::NoSuchTrack));
    assert_eq!(reason(9), Some(RejectReason::ConditionsNotMet));
    assert_eq!(reason(10), Some(RejectReason::ConditionsNotMet));
    assert_eq!(reason(11), None, "a rewind longer than the ring goes back as far as the ring reaches");
    assert_eq!(got.len(), 10, "a command from an empty slot gets no answer: {got:?}");
    assert!(!sim.pacing().paused, "and changes nothing");

    // Queries answer the asking player only.
    sim.apply(&[cmd(12, Command::Query(Query::Overrides(OfficerRole::Gunnery))), cmd(13, Command::Query(Query::Why(PartSlot(0))))]);
    sim.step();
    let replies: Vec<&OutEvent> = sim.out.iter().filter(|e| matches!(e.event, SimEvent::QueryReply { .. })).collect();
    assert_eq!(replies.len(), 2);
    assert!(replies.iter().all(|e| e.audience == crate::protocol::event::Audience::player(P0)));
}

#[test]
fn input_from_the_wire_is_cleaned() {
    let mut sim = basin(SplashTarget::Drones);
    // A Sim can be looked at before its first step.
    let first = sim.snapshot(crate::protocol::snapshot::Viewer::Player(P0));
    assert_eq!(first.tick, 0);
    assert!(!sim.snapshot(crate::protocol::snapshot::Viewer::Omniscient).vessels.is_empty());
    assert!(!snapshot::catch_up(&sim, crate::protocol::snapshot::Viewer::Player(P0)).is_empty());
    sim.note_snapshot_items(snapshot::snapshot_items(&first));
    let hash = sim.state_hash();
    sim.note_snapshot_items(5);
    assert_eq!(sim.state_hash(), hash, "who is watching does not change the state hash");
    sim.set_input(P0, PlayerInput { input_tick: 1, client_render_tick: 0, throttle: f32::NAN, rudder: 7.0, aim_local: [f32::INFINITY, 0.0, 0.0], trigger: true, camera_pos: [f64::NAN, 0.0, 0.0] });
    sim.set_input(PlayerSlot(6), PlayerInput::default());
    let c = &sim.ships().of_player(P0).expect("ship").control.input;
    assert_eq!((c.throttle, c.rudder, c.aim_local, c.camera_pos), (0.0, 1.0, [0.0; 3], [0.0; 3]));
    for _ in 0..4 { sim.step(); }
    assert_eq!(sim.tick().0, 4);
}

#[test]
fn rewind_and_restart_go_back_and_resynchronise_clients() {
    let mut sim = basin(SplashTarget::Drones);
    sim.apply(&[cmd(1, Command::SetCourse { waypoints: vec![DVec2::new(2_000.0, 0.0)], speed_kn: Some(15.0), append: false })]);
    let mut hash_at_60 = None;
    for _ in 0..200 {
        if sim.tick().0 == 60 { hash_at_60 = Some(sim.state_hash()); }
        sim.step();
    }
    sim.drain_events();
    assert_eq!(sim.rewind.frames.iter().map(|f| f.0).collect::<Vec<_>>(), vec![0, 60, 120, 180]);
    let moved = sim.world().vessel(sim.ships().of_player(P0).expect("ship").vessel).expect("hull").kin.pose.pos;

    // 120 ticks back from 200 is 80: the nearest stored frame at or before it is 60.
    sim.apply(&[cmd(2, Command::SplashRewind { ticks: 120 })]);
    sim.step();
    assert_eq!(sim.tick().0, 61, "restored to 60, then this step's tick");
    assert_eq!(sim.rewind.frames.iter().map(|f| f.0).collect::<Vec<_>>(), vec![0, 60], "later frames are dropped");
    let events = sim.drain_events();
    assert!(events.iter().any(|e| matches!(e.event, SimEvent::SortieStarted { .. })), "clients are resynchronised");
    assert!(events.iter().any(|e| matches!(e.event, SimEvent::EntityRevealed(_))));
    let back = sim.world().vessel(sim.ships().of_player(P0).expect("ship").vessel).expect("hull").kin.pose.pos;
    assert!(back.distance(moved) > 1.0, "the hull is back where it was");
    // The frame itself restores exactly.
    let mut again = Sim::restore_keyframe(sim.config().clone(), deps(), &sim.rewind.frames[1].1).expect("the frame restores");
    assert_eq!(Some(again.state_hash()), hash_at_60);
    again.step();

    // A command sent before the rewind is still a duplicate after it.
    sim.apply(&[cmd(1, Command::SetSpeed { speed_kn: 3.0 }), cmd(2, Command::SplashRestart)]);
    sim.step();
    assert_eq!(rejections(&sim.drain_events()), vec![(1, RejectReason::Duplicate), (2, RejectReason::Duplicate)]);

    sim.apply(&[cmd(3, Command::SplashRestart)]);
    sim.step();
    assert_eq!(sim.tick().0, 1, "restart goes back to the first step");
    assert!(sim.ships().of_player(P0).expect("ship").orders.route.is_empty(), "before the first order");
    assert!(rejections(&sim.drain_events()).is_empty());
    // The first step's order was queued in the restart frame; it is history now, not given again.
    assert!(sim.pending.is_empty());
    for _ in 0..3 { sim.step(); }
    assert!(rejections(&sim.drain_events()).is_empty());
    assert!(sim.ships().of_player(P0).expect("ship").orders.route.is_empty());

    // Outside a basin the splash commands are refused.
    let (catalogue, campaign) = data().clone();
    let g = crate::theatre::opgen::generate_standalone(content::campaign::MissionType::Feint, Act::Act1, 1, 2, &Default::default(), None, &crate::theatre::TheatreData { campaign, catalogue }).expect("an Act 1 feint");
    let mut op = Sim::new(g.config, deps()).expect("starts");
    op.apply(&[cmd(1, Command::SplashRewind { ticks: 60 }), cmd(2, Command::SplashRestart)]);
    op.step();
    assert_eq!(rejections(&op.drain_events()), vec![(1, RejectReason::NotAllowedInMode), (2, RejectReason::NotAllowedInMode)]);
    assert!(op.rewind.frames.is_empty() && op.rewind.start.is_none(), "no rewind ring outside a basin");
}

#[test]
fn compression_is_requested_announced_and_dropped() {
    let mut sim = basin(SplashTarget::Drones);
    sim.step();
    sim.drain_events();
    sim.apply(&[cmd(1, Command::RequestTimeRate(TimeRate::X4))]);
    sim.step();
    let p = sim.pacing();
    assert_eq!(p.requested, vec![(P0, TimeRate::X4)]);
    assert!(p.effective == TimeRate::X4 || p.blocked.is_some(), "the rate is in force or says why not: {p:?}");
    let sent: Vec<_> = sim.drain_events().into_iter().filter(|e| matches!(e.event, SimEvent::PacingChanged(_))).collect();
    assert_eq!(sent.len(), 1, "pacing is announced once per change");
    for _ in 0..10 { sim.step(); }
    assert!(!sim.drain_events().iter().any(|e| matches!(e.event, SimEvent::PacingChanged(_))) || sim.pacing() != p);
    sim.apply(&[cmd(2, Command::RequestTimeRate(TimeRate::X1))]);
    sim.step();
    assert_eq!(sim.pacing().effective, TimeRate::X1);
}

#[test]
fn finish_decides_once_and_the_config_round_trips() {
    let mut sim = basin(SplashTarget::PatrolBoat);
    for _ in 0..30 { sim.step(); }
    sim.drain_events();
    assert!(!sim.is_over());
    assert_eq!(sim.finish(Outcome::Withdrawn).outcome, Outcome::Withdrawn);
    assert!(sim.is_over());
    assert_eq!(sim.finish(Outcome::Victory).outcome, Outcome::Withdrawn, "the first outcome stands");
    let ended: Vec<OutEvent> = sim.drain_events().into_iter().filter(|e| matches!(e.event, SimEvent::SortieEnded(_))).collect();
    assert_eq!(ended.len(), 1);
    assert!(matches!(&ended[0].event, SimEvent::SortieEnded(s) if s.tip.is_some()), "a basin's summary carries its tip");
    // A finished sortie still steps (the driver decides when to stop) and stays finished.
    sim.step();
    assert!(sim.is_over());

    let bytes = keyframe::encode_config(sim.config());
    assert_eq!(&keyframe::decode_config(&bytes).expect("decodes"), sim.config());
    assert!(keyframe::decode_config(&bytes[..bytes.len() / 2]).is_err());
    assert!(keyframe::decode_config(&[]).is_err());
    let mut longer = bytes.clone();
    longer.push(0);
    assert!(keyframe::decode_config(&longer).is_err(), "trailing bytes are refused");

    // The checkpoint record carries its tick and restores a finished sortie as finished.
    let cp = sim.checkpoint();
    assert_eq!((cp.tick, cp.keyframe_format), (sim.tick().0, crate::protocol::PROTOCOL_VERSION));
    let back = Sim::from_checkpoint(&cp, deps()).expect("restores");
    assert!(back.is_over());
    assert_eq!(back.state_hash(), sim.state_hash());
    assert_eq!(back.subsystem_hashes().len(), 15);
}

/// Keyframes arrive over the network on a re-host: whatever a damaged one decodes to, restoring it is an
/// error or a Sim, never a panic.
#[test]
fn damaged_keyframes_are_errors_or_sims_never_panics() {
    let mut sim = basin(SplashTarget::PatrolBoat);
    sim.apply(&[cmd(1, Command::SetCourse { waypoints: vec![DVec2::new(900.0, 300.0)], speed_kn: Some(18.0), append: false })]);
    for _ in 0..240 { sim.step(); }
    let good = sim.save_keyframe();
    let config = sim.config().clone();
    // Designs are shared so each attempt costs a decode, not a rebuild.
    let with_designs = |sim: &Sim| { let mut d = deps(); d.designs.designs = sim.deps.designs.designs.clone(); d };
    let back = Sim::restore_keyframe(config.clone(), with_designs(&sim), &good).expect("the undamaged frame restores");
    assert_eq!(back.state_hash(), sim.state_hash());
    assert_eq!(back.subsystem_hashes(), sim.subsystem_hashes());

    let world_len = u64::from_le_bytes(good[8..16].try_into().expect("8 bytes")) as usize;
    let systems_at = 16 + world_len;
    assert!(systems_at < good.len(), "the frame has a systems part");
    let (mut restored, mut refused) = (0, 0);
    // The header, a sweep through the world part, and a dense sweep through the systems part.
    let spots = (0..16).chain((16..systems_at).step_by((world_len / 60).max(1))).chain((systems_at..good.len()).step_by(((good.len() - systems_at) / 400).max(1)));
    for (n, k) in spots.enumerate() {
        let mut bent = good.clone();
        bent[k] ^= [0x01, 0x80, 0xFF, 0x5A][n % 4];
        match Sim::restore_keyframe(config.clone(), with_designs(&sim), &bent) {
            Ok(mut s) => {
                restored += 1;
                // A sample of what restored anyway runs on: a frame that decodes must also step.
                if restored % 16 == 0 { for _ in 0..12 { s.step(); s.snapshot(crate::protocol::snapshot::Viewer::Player(P0)); } }
            }
            Err(_) => refused += 1,
        }
    }
    assert!(refused > 0 && restored + refused > 400, "{restored} restored, {refused} refused");
    for cut in [0, 3, 15, 16, systems_at - 1, systems_at, systems_at + 1, good.len() - 1] {
        assert!(Sim::restore_keyframe(config.clone(), with_designs(&sim), &good[..cut]).is_err(), "cut at {cut}");
    }
    let mut longer = good.clone();
    longer.extend_from_slice(&[0; 9]);
    assert!(Sim::restore_keyframe(config, with_designs(&sim), &longer).is_err(), "trailing bytes are refused");
}

/// Live splash edits (L216): a settings edit swaps the design in place and keeps the hull's prefab; a paint
/// edit replaces the vessel. Both survive a checkpoint taken afterwards, and a rewind across them.
#[test]
fn live_edits_survive_checkpoints_and_a_rewind_across_them() {
    let mut sim = basin(SplashTarget::Drones);
    sim.apply(&[cmd(1, Command::SetCourse { waypoints: vec![DVec2::new(1_500.0, 0.0)], speed_kn: Some(14.0), append: false })]);
    for _ in 0..90 { sim.step(); }
    sim.drain_events();
    let hull0 = sim.ships().of_player(P0).expect("ship").vessel;
    let original = sim.ships().of_player(P0).expect("ship").design.blueprint().clone();

    // In place: only the officers' settings change.
    let mut quiet = original.clone();
    quiet.officers.emcon = if original.officers.emcon == Emcon::Silent { Emcon::Active } else { Emcon::Silent };
    sim.apply(&[cmd(2, Command::SplashApplyDesign(Box::new(quiet.clone())))]);
    sim.step();
    assert!(rejections(&sim.drain_events()).is_empty());
    let ship = sim.ships().of_player(P0).expect("ship");
    assert_eq!((ship.vessel, ship.settings.emcon), (hull0, quiet.officers.emcon), "same hull, new settings");
    assert_ne!(ship.design.hash, sim.world().vessel(hull0).expect("hull").blueprint, "the hull keeps the prefab it was built from");
    for _ in 0..30 { sim.step(); }
    let back = Sim::from_checkpoint(&sim.checkpoint(), deps()).expect("a checkpoint after an in-place edit restores");
    assert_eq!(back.state_hash(), sim.state_hash());
    assert_eq!(back.ships().of_player(P0).expect("ship").settings.emcon, quiet.officers.emcon);

    // Replaced: the paint changes, so the vessel is rebuilt at the same pose.
    let mut painted = quiet.clone();
    assert!(!painted.paint.is_empty(), "the starter carries paint");
    painted.paint[0].rgb = [0.25, 0.5, 0.75];
    let before = sim.world().vessel(hull0).expect("hull").kin.pose.pos;
    sim.apply(&[cmd(3, Command::SplashApplyDesign(Box::new(painted)))]);
    sim.step();
    let events = sim.drain_events();
    assert!(rejections(&events).is_empty());
    let hull1 = sim.ships().of_player(P0).expect("ship").vessel;
    assert_ne!(hull1, hull0, "a new vessel");
    assert!(events.iter().any(|e| matches!(e.event, SimEvent::PlayerJoined { vessel, .. } if vessel == hull1)));
    assert!(sim.world().vessel(hull1).expect("hull").kin.pose.pos.distance(before) < 5.0, "at the same place");
    for _ in 0..60 { sim.step(); }
    assert_eq!(sim.players()[0].vessel, Some(hull1));
    let back = Sim::from_checkpoint(&sim.checkpoint(), deps()).expect("a checkpoint after a replacement restores");
    assert_eq!(back.state_hash(), sim.state_hash());

    // A changed hull shell is refused: the client goes back to dry dock.
    let mut longer = original.clone();
    longer.hull.design_waterline_y += 1;
    sim.apply(&[cmd(4, Command::SplashApplyDesign(Box::new(longer)))]);
    sim.step();
    assert_eq!(rejections(&sim.drain_events()), vec![(4, RejectReason::OnlyInDryDockOrSplash)]);

    // Back across both edits: the original hull and design sail again.
    let now = sim.tick().0;
    sim.apply(&[cmd(5, Command::SplashRewind { ticks: (now - 60) as u32 })]);
    sim.step();
    assert_eq!(sim.tick().0, 61);
    let ship = sim.ships().of_player(P0).expect("ship");
    assert_eq!((ship.vessel, ship.design.hash, ship.settings.emcon), (hull0, original.header.hash, original.officers.emcon));
    for _ in 0..30 { sim.step(); }
}

/// A campaign template with every hull but the player's (and optionally the first hostile one) removed, the
/// player sailing `blueprint`.
fn quiet_operation(template: content::campaign::MissionType, blueprint: &str, keep_hostile: bool) -> SortieConfig {
    let (catalogue, campaign) = data().clone();
    let g = crate::theatre::opgen::generate_standalone(template, Act::Act1, 1, 4, &Default::default(), None, &crate::theatre::TheatreData { campaign, catalogue }).expect("an Act 1 template");
    let mut c = g.config;
    let own = c.players[0].vessel.expect("a hull") as usize;
    let mut player = c.vessels[own].clone();
    player.blueprint = crate::protocol::config::BlueprintRef::Stock(blueprint.into());
    let hostile = c.vessels.iter().find(|v| v.side != player.side && v.arrives_s == 0 && c.sides.iter().any(|s| s.id == v.side && s.faction == crate::protocol::config::Faction::Combine)).cloned();
    c.vessels = vec![player];
    if let (true, Some(h)) = (keep_hostile, hostile) { c.vessels.push(h); }
    c.players[0].vessel = Some(0);
    c.intel.named.clear();
    c.bodies.clear();
    c
}

fn first_action_event(events: &[OutEvent], state: crate::protocol::snapshot::ActionState) -> bool {
    events.iter().any(|e| matches!(&e.event, SimEvent::ActionChanged(v) if v.state == state))
}

/// Station actions reach the world: a mine line drops mines from the rails, and an insertion launches the
/// RHIB, holds, finishes and takes the boat back aboard. (The 26 m insertion starter is not used: as shipped
/// it capsizes at rest, which is a hull-data matter and not this unit's.)
#[test]
fn mine_lines_and_rhib_actions_run_to_completion() {
    use crate::protocol::command::ActionTarget;
    use crate::protocol::snapshot::ActionState;
    use content::campaign::{MissionType, Objective, StationAction};

    // Mines: the 60 m corvette carries two rails.
    let mut config = quiet_operation(MissionType::Minelaying, "starters/corvette60.bp.ron", false);
    assert!(config.actions.contains(&StationAction::LayMines), "the minelaying template offers the action");
    config.vessels[0].speed_kn = 10.0;
    let mut sim = Sim::new(config, deps()).expect("starts");
    let hull = sim.ships().of_player(P0).expect("ship").vessel;
    let (pos, fwd) = { let v = sim.world().vessel(hull).expect("hull"); let c = v.to_world(DVec3::new(v.dims[0] * 0.5, 0.0, v.dims[2] * 0.5)); let f = v.kin.pose.rot * DVec3::X; (DVec2::new(c.x, c.z), DVec2::new(f.x, f.z).normalize()) };
    sim.apply(&[
        cmd(1, Command::SetCourse { waypoints: vec![pos + fwd * 3_000.0], speed_kn: Some(12.0), append: false }),
        cmd(2, Command::StartAction { action: StationAction::LayMines, target: ActionTarget::Line { a: pos + fwd * 150.0, b: pos + fwd * 550.0 } }),
        cmd(3, Command::StartAction { action: StationAction::Photograph, target: ActionTarget::None }),
    ]);
    let (mut laid, mut events) = (0, Vec::new());
    for _ in 0..(110 * 60) {
        sim.step();
        for e in sim.drain_events() {
            if matches!(e.event, SimEvent::Phys(sim_core::events::PhysEvent::MineLaid { layer, .. }) if layer == hull) { laid += 1; }
            events.push(e);
        }
        if first_action_event(&events, ActionState::Done) || first_action_event(&events, ActionState::Aborted) { break; }
    }
    let got = rejections(&events);
    assert_eq!(got.len(), 1, "the corvette has no camera mast, and the template offers no photograph: {got:?}");
    assert_eq!(got[0].0, 3);
    assert!(laid >= 3, "mines dropped along the line: {laid}");
    assert!(sim.specops.mines_laid as u32 >= laid, "and counted");

    // Insertion: the same corvette carries a RHIB davit; it holds at the shore marker.
    let mut config = quiet_operation(MissionType::AgentExtraction, "starters/corvette60.bp.ron", false);
    let Objective::Extract { marker, .. } = config.objective.clone() else { panic!("the extraction template extracts") };
    let at = config.map.markers.iter().find(|m| m.name == marker).expect("the shore marker").pos_m;
    config.vessels[0].pos = DVec3::new(at[0], 0.0, at[1]);
    config.vessels[0].speed_kn = 0.0;
    let mut sim = Sim::new(config, deps()).expect("starts");
    let hull = sim.ships().of_player(P0).expect("ship").vessel;
    sim.apply(&[cmd(1, Command::SetSpeed { speed_kn: 0.0 }), cmd(2, Command::StartAction { action: StationAction::InsertOrExtract, target: ActionTarget::Marker(marker.clone()) })]);
    let is_rhib = |k: MunitionIdx| data().0.munitions.get_index(k.index()).is_some_and(|(_, d)| d.kind == content::part::weapons::MunitionKind::Rhib);
    let (mut launched, mut recovered, mut done, mut boat, mut events) = (false, None, None, None, Vec::new());
    for _ in 0..(150 * 60) {
        sim.step();
        for e in sim.drain_events() {
            match &e.event {
                SimEvent::Phys(sim_core::events::PhysEvent::BodySpawned { body, kind, launcher, .. }) if *launcher == hull && is_rhib(*kind) => { launched = true; boat = Some(*body); }
                SimEvent::Phys(sim_core::events::PhysEvent::BodyRecovered { body, vessel }) if *vessel == hull && Some(*body) == boat => recovered = Some(sim.tick().0),
                SimEvent::ActionChanged(v) if v.state == ActionState::Done => done = Some(sim.tick().0),
                _ => {}
            }
            events.push(e);
        }
        if done.is_some() && (recovered.is_some() || sim.run.rhibs.is_empty()) { break; }
    }
    assert!(rejections(&events).is_empty(), "{:?}", rejections(&events));
    assert!(launched, "the davit put the RHIB in the water");
    let done = done.unwrap_or_else(|| panic!("the hold completed: {:?}", sim.specops.actions.iter().map(|a| (a.state, a.block, a.progress)).collect::<Vec<_>>()));
    let recovered = recovered.expect("the boat came back aboard");
    assert!(recovered > done, "the boat stayed out for the hold (done at {done}, aboard at {recovered})");
    for _ in 0..4 { sim.step(); }
    assert!(sim.run.rhibs.is_empty() && sim.world().bodies().entity.iter().all(|b| Some(*b) != boat), "nothing is left in the water");
}

/// What a finished station action asks of the Sim, applied to a real basin: a photograph makes its track
/// Exact, a camera order reaches the part's controls, a capture changes the prize's side and crew, and a
/// stripped prize leaves the sortie. (No stock hull that stays upright carries a camera mast, so the
/// photograph cannot be run from its command; its handler is driven directly.)
#[test]
fn station_action_results_reach_tracks_controls_and_the_prize() {
    use crate::specops::SpecOpsOut;
    let mut sim = basin(SplashTarget::PatrolBoat);
    let own = sim.ships().of_player(P0).expect("ship").vessel;
    let side = sim.players()[0].side;
    let prize = sim.world().vessels().map(|(_, v)| v.id).find(|e| *e != own).expect("the patrol boat");
    let their_side = sim.world().vessel(prize).expect("hull").side;
    let mut track = None;
    for _ in 0..600 {
        sim.step();
        track = sim.pictures().side(side).and_then(|p| p.track_of(prize)).map(|t| t.id);
        if track.is_some() { break; }
    }
    let track = track.expect("the patrol boat is tracked");
    sim.drain_events();
    let env = step::TickEnv::of(&sim);
    let t = sim.tick();

    let slot = PartSlot(0);
    let toward = sim.world().vessel(prize).expect("hull").kin.pose.pos;
    sim.apply_specops(vec![SpecOpsOut::Photo { by: own, side, track, target: prize }, SpecOpsOut::TrainCamera { vessel: own, slot, toward }], t, &env.ctx(t));
    let tr = sim.pictures().side(side).and_then(|p| p.get(track)).expect("track");
    assert_eq!((tr.rung, tr.rung_reason), (IdRung::Exact, RungReason::Photograph));
    assert_eq!(tr.class, sim.world().vessel(prize).expect("hull").class);
    let events = sim.drain_events();
    assert!(events.iter().any(|e| matches!(e.event, SimEvent::PhotoTaken { by, track: tk } if by == own && tk == track)));
    assert!(events.iter().any(|e| matches!(e.event, SimEvent::RungChanged { rung: IdRung::Exact, .. })));
    sim.step();
    let order = sim.world().vessel(own).expect("hull").controls.parts[slot.index()].order;
    assert!(matches!(order, sim_core::parts::PartOrder::Emit { radiating: false, .. }), "the camera order stands in the controls: {order:?}");

    // Capture: the prize sails for the captor under a prize crew and reports to the captor's link.
    let t = sim.tick();
    sim.apply_specops(vec![SpecOpsOut::Capture { prize, to_side: side, by: P0 }], t, &env.ctx(t));
    assert_eq!(sim.world().vessel(prize).expect("hull").side, side);
    let ship = sim.ships().get(prize).expect("the prize keeps its ship");
    assert_eq!((ship.side, ship.player), (side, None));
    assert!(ship.captain.as_ref().is_some_and(|c| c.behaviour == crate::ai::unit::Behaviour::Proxy) && ship.orders.weapons_tight);
    assert!(sim.sensors.sensors.iter().filter(|s| s.id.carrier == prize).all(|s| s.side == side));
    assert!(sim.drain_events().iter().any(|e| matches!(e.event, SimEvent::PrizeTaken { prize: p, by, .. } if p == prize && by == P0)));
    for _ in 0..60 { sim.step(); }
    let held = sim.pictures().side(side).and_then(|p| p.track_of(prize)).expect("the prize is in the captor's picture");
    assert_eq!(held.stance, Stance::Friend, "and is a Friend over the datalink");
    assert!(sim.pictures().side(their_side).is_none_or(|p| p.track_of(own).is_none_or(|t| t.last_update_tick + 60 < sim.tick().0)), "its old side no longer sees through it");
    assert!(sim.snapshot(crate::protocol::snapshot::Viewer::Player(P0)).ship.is_some());

    // The captor decides the prize's fate; nobody else does. Sent home, it takes the exit if the chart has one.
    sim.specops.prizes.push(crate::specops::Prize { vessel: prize, captor: P0, decision: None, reached_exit: false, crew_captured: 0, stripped: false });
    let home = crate::protocol::command::PrizeDecision::SailHome;
    sim.apply(&[cmd(1, Command::Prize { prize, decision: home }), CommandEnvelope { player: PlayerSlot(3), ..cmd(1, Command::Prize { prize, decision: home }) }, cmd(2, Command::Prize { prize: own, decision: home })]);
    sim.step();
    assert_eq!(rejections(&sim.drain_events()), vec![(2, RejectReason::ConditionsNotMet)]);
    assert_eq!(sim.specops.prizes[0].decision, Some(home));
    let exits = sim.config().map.markers.iter().filter(|m| matches!(m.kind, content::campaign::MarkerKind::Exit | content::campaign::MarkerKind::ConvoyExit)).count();
    let ship = sim.ships().get(prize).expect("prize");
    assert_eq!(ship.orders.route.len(), exits.min(1));
    assert_eq!(ship.captain.as_ref().map(|c| c.route.len()), Some(exits.min(1)));
    sim.specops.prizes.clear();

    // Stripped on the spot: the hull leaves the sortie and every table forgets it.
    let t = sim.tick();
    sim.apply_specops(vec![SpecOpsOut::Strip { prize }], t, &env.ctx(t));
    sim.step();
    sim.step();
    assert!(sim.world().vessel(prize).is_none() && sim.ships().get(prize).is_none());
    assert!(!sim.sensors.sensors.iter().any(|s| s.id.carrier == prize));
    for _ in 0..30 { sim.step(); }
}

/// The lobby modes start and run under the same Sim, a snapshot per seat and per side builds, and a dropped
/// player's hull is held by the officers until the player rejoins.
#[test]
fn lobby_modes_run_and_a_dropped_player_can_rejoin() {
    use crate::modes::{LobbyPlayer, LobbySetup, SkirmishOptions};
    use crate::protocol::snapshot::Viewer;
    let (_, campaign) = data().clone();
    let catalogue = data().0.clone();
    let bp = crate::editor::splash::canonical(&content::campaign::load::stock_blueprint("starters/fac22.bp.ron").expect("starter"));
    let seat = |slot: u8, team: u8| LobbyPlayer { slot: PlayerSlot(slot), client_id: 100 + slot as u64, name: format!("p{slot}"), team, blueprint: Some(bp.header.hash), ready: true, complexity: 0.0 };
    let setup = |mode: GameMode, players: Vec<LobbyPlayer>| LobbySetup { mode, ruleset: Default::default(), seed: 9, players, skirmish: Some(SkirmishOptions { template: content::campaign::MissionType::ConvoyRaid, act: Act::Act1, enemy_per_player: 2, difficulty: Default::default() }), weather: content::weather::WeatherKind::Clear, sea_state: SeaState(3), time_of_day: TimeOfDay::Day, map: None, at_1_0: false };
    let blueprints = vec![bp.clone()];
    let configs = vec![
        crate::modes::elimination_config(&setup(GameMode::PvpElimination, vec![seat(0, 0), seat(1, 1)]), &blueprints, &campaign),
        crate::modes::skirmish_config(&setup(GameMode::Skirmish, vec![seat(0, 0), seat(1, 0)]), &blueprints, &campaign, &catalogue),
        crate::modes::sandbox_config(&setup(GameMode::Sandbox, vec![seat(0, 0), seat(1, 0)]), &blueprints, &campaign),
    ];
    for config in configs {
        let mode = config.mode;
        let sides: Vec<SideId> = config.sides.iter().map(|s| s.id).collect();
        let mut sim = Sim::new(config, deps()).unwrap_or_else(|e| panic!("{mode:?} starts: {e}"));
        assert_eq!(sim.players().len(), 2, "{mode:?}");
        // Pause is for one human alone; PvP has neither pause nor compression.
        sim.apply(&[cmd(1, Command::Pause(true)), cmd(2, Command::RequestTimeRate(TimeRate::X4))]);
        for _ in 0..120 { sim.step(); }
        assert_eq!(sim.tick().0, 120, "{mode:?} never pauses with two humans");
        let refused = rejections(&sim.drain_events());
        assert!(refused.iter().any(|r| r.0 == 1), "{mode:?}: {refused:?}");
        assert_eq!(refused.iter().any(|r| r.0 == 2), mode == GameMode::PvpElimination, "{mode:?}: {refused:?}");
        assert_eq!(sim.pacing().effective, TimeRate::X1, "{mode:?}: one player asking is not all players asking");

        let hull = sim.players()[1].vessel.expect("the second player's hull");
        sim.drop_player(PlayerSlot(1));
        for _ in 0..60 { sim.step(); }
        let ship = sim.ships().get(hull).expect("the hull stays");
        assert!(ship.captain.is_some() && ship.control.officers_in_command, "{mode:?}: officers hold the hull");
        sim.apply(&[CommandEnvelope { player: PlayerSlot(1), ..cmd(1, Command::SetSpeed { speed_kn: 5.0 }) }]);
        sim.step();
        assert!(rejections(&sim.drain_events()).is_empty(), "{mode:?}: a dropped player's packets get no answer");
        assert_eq!(sim.rejoin_player(101), Some(PlayerSlot(1)), "{mode:?}");
        sim.apply(&[CommandEnvelope { player: PlayerSlot(1), ..cmd(2, Command::SetSpeed { speed_kn: 5.0 }) }]);
        for _ in 0..60 { sim.step(); }
        assert_eq!(sim.ships().get(hull).expect("ship").orders.speed_kn, Some(5.0), "{mode:?}: back in command");

        for p in [PlayerSlot(0), PlayerSlot(1)] {
            let snap = sim.snapshot(Viewer::Player(p));
            assert!(snap.ship.is_some(), "{mode:?}: {p:?} sees their ship");
            assert!(!snapshot::catch_up(&sim, Viewer::Player(p)).is_empty());
        }
        for s in sides { let _ = sim.snapshot(Viewer::Side(s)); }
        assert!(!sim.snapshot(Viewer::Omniscient).vessels.is_empty());
        let back = Sim::from_checkpoint(&sim.checkpoint(), deps()).unwrap_or_else(|e| panic!("{mode:?} restores: {e}"));
        assert_eq!(back.state_hash(), sim.state_hash(), "{mode:?}");
    }
}

/// A player's hull that is gone from the world (its wreck despawned) leaves its ship in the table: the Sim
/// runs on, and a checkpoint or rewind frame taken afterwards restores.
#[test]
fn a_player_ship_without_a_hull_still_checkpoints_and_rewinds() {
    let mut sim = basin(SplashTarget::Drones);
    for _ in 0..70 { sim.step(); }
    let own = sim.ships().of_player(P0).expect("ship").vessel;
    sim.world.despawn(own);
    for _ in 0..130 { sim.step(); }
    assert!(step::hull(sim.world(), own).is_none());
    let kept = sim.ships().get(own).is_some();
    let back = Sim::from_checkpoint(&sim.checkpoint(), deps()).expect("a checkpoint without the player's hull restores");
    assert_eq!(back.state_hash(), sim.state_hash());
    assert_eq!(back.ships().get(own).is_some(), kept);
    sim.drain_events();
    sim.apply(&[cmd(1, Command::SplashRewind { ticks: 30 })]);
    sim.step();
    assert!(rejections(&sim.drain_events()).is_empty(), "a frame taken after the hull went restores");
}

/// An AI side's track whose truth entity is gone from the world (in a sortie, the chaff cloud it last took a
/// fix from, expired) keeps the stance the briefing declared for it; before, the declaration fell to None and
/// the track turned Suspect (u1, duel.auditor: the Arrears stood off from 630 to 1,990 s).
#[test]
fn an_ai_track_keeps_its_declared_stance_when_its_truth_is_gone() {
    let mut sim = basin(SplashTarget::PatrolBoat);
    let own = sim.ships().of_player(P0).expect("ship").vessel;
    let boat = sim.world().vessels().map(|(_, v)| v.id).find(|e| *e != own).expect("the patrol boat");
    let their = sim.world().vessel(boat).expect("hull").side;
    let mut track = None;
    for _ in 0..(120 * 60) {
        sim.step();
        track = sim.pictures().side(their).and_then(|p| p.track_of(own)).filter(|t| t.stance == Stance::Hostile && t.stance_reason == StanceReason::BriefingHostile).map(|t| t.id);
        if track.is_some() { break; }
    }
    let track = track.expect("the patrol boat's side holds the player hull Hostile by its briefing");
    // The truth entity leaves the world while the track lives on (surface tracks drop after 120 s).
    sim.world.despawn(own);
    for _ in 0..60 { sim.step(); }
    let t = sim.pictures().side(their).and_then(|p| p.get(track)).filter(|t| t.is_live()).expect("the track is still live");
    assert!(sim.world().locate(t.truth).is_none(), "its truth is gone");
    assert_eq!((t.stance, t.stance_reason), (Stance::Hostile, StanceReason::BriefingHostile), "the declaration stands");
    // The helper: a located truth on no known side, or no declaration before, gives none.
    let def = sim.config().sides.iter().find(|d| d.id == their).expect("side").clone();
    let mut fresh = t.clone();
    fresh.stance = Stance::Suspect;
    fresh.stance_reason = StanceReason::Default;
    assert_eq!(step::ai_declared(&def, their, false, None, &fresh), None);
    assert_eq!(step::ai_declared(&def, their, true, None, t), None);
    assert_eq!(step::ai_declared(&def, their, false, None, t), Some(Stance::Hostile));
}

/// A side's call for reinforcements brings its scheduled waves forward, but an ace keeps the arrival her
/// complication gave her (capstone seed 0: the Covenant, due at 900 s, was launching missiles from 570 s).
#[test]
fn a_call_for_reinforcements_leaves_the_ace_to_her_own_time() {
    use crate::protocol::config::{GroupInit, VesselInit};
    use sim_core::time::TICK_HZ;
    let mut c = quiet_operation(content::campaign::MissionType::ConvoyRaid, "starters/fac22.bp.ron", true);
    let hostile = c.vessels[1].clone();
    let side = hostile.side;
    let g = |id: u16, ace: Option<&str>| GroupInit { id: GroupId(id), side, kind: content::campaign::FleetKind::TaskGroup, formation: Default::default(), orders: crate::protocol::config::GroupOrders::Hunt, ace: ace.map(AceId::new), theatre_group: None, tag: String::new() };
    c.groups = vec![g(50, None), g(51, Some("ace.bailiff"))];
    let wave = VesselInit { group: Some(GroupId(50)), arrives_s: 600, ..hostile.clone() };
    let ace = VesselInit { group: Some(GroupId(51)), arrives_s: 900, ..hostile.clone() };
    let loose = VesselInit { group: None, arrives_s: 900, ..hostile };
    c.vessels.extend([wave, ace, loose]);
    let mut hastened = Vec::new();
    let now = 26 * TICK_HZ as u64;
    step::hasten_reinforcements(&c, &[2, 3, 4], &mut hastened, side, now);
    let hz = TICK_HZ as u64;
    assert_eq!(hastened, vec![(2, now + (600 * hz - now) / 2), (4, now + (900 * hz - now) / 2)], "the wave and the loose hull come at half their wait; the ace does not");
    step::hasten_reinforcements(&c, &[2, 3, 4], &mut hastened, SideId(side.0 + 1), now);
    assert_eq!(hastened.len(), 2, "another side's call moves nothing");
}

/// Ruling 1: when a hull strikes, a player's designation of it is cleared (once; the officers then leave it
/// alone), other designations stay, and the track view carries the struck flag the track card shows.
#[test]
fn a_strike_clears_the_designation_and_the_track_view_shows_it() {
    use crate::tracks::{Pictures, SidePicture};
    let struck = EntityId { index: 7, generation: 0 };
    let other = EntityId { index: 8, generation: 0 };
    let mut a = crate::timeflow::tests::track(3, DVec3::new(1_000.0, 0.0, 0.0), Stance::Hostile, Domain::Surface);
    a.truth = struck;
    let mut b = crate::timeflow::tests::track(4, DVec3::new(2_000.0, 0.0, 0.0), Stance::Hostile, Domain::Surface);
    b.truth = other;
    let mut c = a.clone();
    c.side = SideId(1);
    c.id = TrackId(5);
    let pictures = Pictures { sides: vec![SidePicture { side: SideId(0), tracks: vec![a.clone(), b], next_id: 6 }, SidePicture { side: SideId(1), tracks: vec![c], next_id: 6 }] };
    // Ship 0 designates the striking hull; ship 1 another hull; ship 2 is an AI ship on the same track; ship 3
    // a player on side 1 designating its own track of the striking hull; ship 4 designates nothing.
    let ships = [(true, SideId(0), TrackId(3)), (true, SideId(0), TrackId(4)), (false, SideId(0), TrackId(3)), (true, SideId(1), TrackId(5)), (true, SideId(0), TrackId::NONE)];
    assert_eq!(super::step::designations_on(&ships, &pictures, struck), vec![0, 3]);
    assert!(super::step::designations_on(&ships, &pictures, EntityId { index: 9, generation: 0 }).is_empty());
    // The track view carries the side's struck flag.
    let tuning = content::tuning::TrackTuning::default();
    assert!(!super::snapshot::track_view(&a, None, 0, &tuning).struck);
    a.struck = true;
    assert!(super::snapshot::track_view(&a, None, 0, &tuning).struck);
}

/// L797 "Neutral until the briefing's rendezvous": the defector holds at its start while no player hull comes
/// near, and the Escort objective is not done; a player hull within the meet distance releases it to its exit.
#[test]
fn the_defector_holds_until_the_rendezvous() {
    use crate::protocol::config::GroupOrders;
    use content::campaign::{MissionType, Objective};
    let (catalogue, campaign) = data().clone();
    let g = crate::theatre::opgen::generate_standalone(MissionType::DefectorEscort, Act::Act1, 1, 4, &Default::default(), None, &crate::theatre::TheatreData { campaign, catalogue }).expect("the defector escort template");
    let Objective::Escort { tag, .. } = g.config.objective.clone() else { panic!("the defector escort escorts") };
    assert!(super::step::rendezvous_met(&[DVec2::ZERO], &[DVec2::new(super::step::RENDEZVOUS_M - 1.0, 0.0)]));
    assert!(!super::step::rendezvous_met(&[DVec2::ZERO], &[DVec2::new(super::step::RENDEZVOUS_M + 1.0, 0.0)]));
    // Only the player hull and the defector: nothing else can release, sink or shield it.
    let mut base = g.config.clone();
    let own = base.players[0].vessel.expect("a hull") as usize;
    let player = base.vessels[own].clone();
    let defector: Vec<_> = base.vessels.iter().filter(|v| v.tag == tag).cloned().collect();
    assert!(!defector.is_empty(), "the operation fields its defector");
    base.vessels = std::iter::once(player).chain(defector.iter().cloned()).collect();
    base.players[0].vessel = Some(0);
    base.intel.named.clear();
    base.bodies.clear();
    let start = DVec2::new(defector[0].pos.x, defector[0].pos.z);
    let run = |config: SortieConfig, seconds: u64| -> (Sim, f64) {
        let mut sim = Sim::new(config, deps()).expect("starts");
        for _ in 0..seconds * 60 { sim.step(); }
        let moved = sim.world().vessels().filter(|(_, v)| v.player.is_none() && v.parent.is_none()).map(|(_, v)| DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z).distance(start)).fold(0.0, f64::max);
        (sim, moved)
    };
    let orders = |sim: &Sim| sim.ai.groups.iter().find(|g| g.tag == tag).and_then(|g| g.orders.clone());
    let (sim, moved) = run(base.clone(), 120);
    assert_eq!(orders(&sim), Some(GroupOrders::Anchor), "no player hull came near");
    assert!(moved < 400.0, "the defector left its start without a rendezvous: {moved:.0} m");
    assert!(sim.objectives.primary.as_ref().is_some_and(|p| p.state != crate::protocol::snapshot::ObjectiveState::Done), "the Escort objective is not done");
    // A player hull 1 km off the defector: released to its exit.
    let mut near = base;
    near.vessels[0].pos = DVec3::new(start.x + 1_000.0, 0.0, start.y);
    let (sim, _) = run(near, 2);
    assert!(matches!(orders(&sim), Some(GroupOrders::Flee { .. })), "{:?}", orders(&sim));
}

/// A shore site gets no anti-ship missile from the officers (coordinator ruling of 22 November 2026,
/// `HoldReason::ShoreSite`): the track phase fills each ship's `shore_sites`. Depot strike seed 2 in the fast
/// attack craft, before: the site, declared Hostile and held Exact at about 12 km, took the craft's missiles
/// inside 5 s with nothing designated. Only the player hull and the site are kept, so no ship is a target.
#[test]
fn officers_hold_anti_ship_missiles_off_a_shore_site() {
    use crate::protocol::config::VesselRole;
    use crate::protocol::text::{EngageStatus, HoldReason};
    use crate::theatre::bot::{op_starter, winnability_config};
    use sim_core::PhysEvent;
    let (catalogue, campaign) = data().clone();
    let d = crate::theatre::TheatreData { campaign, catalogue };
    let op = d.campaign.operations.values().find(|o| o.act == Act::Act1 && o.id.as_str() == "op.act1.depot_strike").expect("depot strike");
    let starter = op_starter(op, Act::Act1, &d);
    let mut c = winnability_config(&op.id, Act::Act1, 2, &starter, &d).expect("config");
    let own = c.players[0].vessel.expect("a hull") as usize;
    let keep: Vec<usize> = (0..c.vessels.len()).filter(|i| *i == own || c.vessels[*i].role == VesselRole::Site).collect();
    assert!(keep.len() >= 2, "the operation fields a site");
    let named: Vec<u16> = keep.iter().enumerate().filter(|(_, i)| c.intel.named.contains(&(**i as u16))).map(|(n, _)| n as u16).collect();
    c.vessels = keep.iter().map(|i| c.vessels[*i].clone()).collect();
    c.players[0].vessel = Some(keep.iter().position(|i| *i == own).unwrap() as u16);
    c.intel.named = named;
    c.groups.clear();
    for v in &mut c.vessels { v.group = None; }
    let mut sim = Sim::new(c, deps_with(&d)).expect("sim");
    let me = sim.ships().of_player(P0).expect("ship").vessel;
    let mut missiles = 0;
    let mut held = false;
    let mut site_tracks = false;
    for _ in 0..(30 * 60) {
        sim.step();
        for e in sim.drain_events() {
            if let SimEvent::Phys(PhysEvent::BodySpawned { kind, launcher, .. }) = &e.event {
                let asm = d.catalogue.munitions.get_index(kind.index()).is_some_and(|(_, m)| m.kind == content::part::weapons::MunitionKind::AntiShipMissile);
                if *launcher == me && asm { missiles += 1; }
            }
        }
        let ship = sim.ships().of_player(P0).expect("ship");
        site_tracks |= !ship.stages.engage.shore_sites.is_empty();
        held |= ship.stages.engage.mounts.iter().any(|m| matches!(m.status, EngageStatus::Holding { reason: HoldReason::ShoreSite, .. }));
    }
    assert_eq!(missiles, 0, "the officers launched anti-ship missiles at the site");
    assert!(site_tracks, "the site's track is listed as a shore site");
    assert!(held, "a launcher holds for the shore site");
    // A site that has lost its engines stays in the fight; one that strikes or is gone does not, nor any
    // other hull dead in the water.
    use sim_core::vessel::VesselStatus;
    assert!(!step::out_of_action(false, VesselStatus::Disabled, true));
    assert!(step::out_of_action(false, VesselStatus::Disabled, false));
    assert!(step::out_of_action(true, VesselStatus::Disabled, true));
    assert!(step::out_of_action(false, VesselStatus::Wreck, true));
    assert!(!step::out_of_action(false, VesselStatus::Active, false));
}

/// An anchored prize lies still under fire (owner ruling of 22 November 2026: a hull at anchor makes no way).
/// cutting_out seed 0 under the gate pilot: with orders of 0 kn and no route, the prize's Helm beamed inbound
/// missiles on its own evasion stage and made 21.5 kn from 30 to 90 s.
#[test]
fn an_anchored_prize_is_not_got_under_way_by_its_helm() {
    use crate::protocol::config::VesselRole;
    use crate::theatre::abstract_combat::{prepare_config, Pilot};
    use crate::theatre::bot::{op_starter, player_setup, winnability_config, Policy};
    use crate::editor::trials::HeadlessPolicy;
    let (catalogue, campaign) = data().clone();
    let d = crate::theatre::TheatreData { campaign, catalogue };
    let op = d.campaign.operations.values().find(|o| o.act == Act::Act1 && o.id.as_str() == "op.act1.cutting_out").expect("cutting out");
    let starter = op_starter(op, Act::Act1, &d);
    let config = winnability_config(&op.id, Act::Act1, 0, &starter, &d).expect("config");
    let setup = player_setup(Policy::Mixed);
    let prepared = prepare_config(&config, &setup);
    let prize = prepared.vessels.iter().position(|v| v.role == VesselRole::Prize).expect("a prize") as u16;
    let mut pilot = Pilot::new(&prepared, &setup, &d);
    let mut sim = Sim::new(prepared, deps_with(&d)).expect("sim");
    let mut commands = Vec::new();
    let mut fastest: f64 = 0.0;
    for _ in 0..(120 * 60) {
        if sim.is_over() { break; }
        commands.clear();
        pilot.commands(&sim, &mut commands);
        if !commands.is_empty() { sim.apply(&commands); }
        sim.set_host_load(crate::lod::HostLoad::IDLE);
        sim.step();
        let e = sim.schedule.spawned.iter().find(|(i, _)| **i == prize).map(|(_, e)| *e);
        if let Some(v) = e.and_then(|e| sim.world().vessel(e)) { fastest = fastest.max(v.forward_speed().abs() / sim_core::time::KN); }
    }
    assert!(fastest < 3.0, "the anchored prize made {fastest:.1} kn");
}

/// Probe (s2, items 5 and 6): one gate sortie of `BP_OPS` at `BP_SEED`, printing when each scheduled hull
/// arrives and, for every missile an AI hull launches, the launcher's class and preset, the track its mount
/// engaged (grade, rung, stance, bearing-only, age, sources) and the track's range against the truth's.
/// `BP_OPS=op.act1.duel.bailiff BP_SEED=0 cargo test --release -p sim-systems --lib ai_launch_probe -- --ignored --nocapture`
#[test]
#[ignore = "full physics; prints for a reader"]
fn ai_launch_probe() {
    use crate::theatre::abstract_combat::{prepare_config, Pilot};
    use crate::theatre::bot::{op_starter, player_setup, winnability_config, Policy};
    use crate::protocol::text::EngageStatus;
    use crate::editor::trials::HeadlessPolicy;
    use sim_core::PhysEvent;
    let (catalogue, campaign) = data().clone();
    let d = crate::theatre::TheatreData { campaign, catalogue };
    let filter = std::env::var("BP_OPS").unwrap_or_else(|_| "op.act1.duel.bailiff".into());
    let seed: u32 = std::env::var("BP_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minutes: u64 = std::env::var("BP_MIN").ok().and_then(|s| s.parse().ok()).unwrap_or(45);
    let op = d.campaign.operations.values().filter(|o| o.act == Act::Act1 && o.id.as_str().contains(filter.as_str())).min_by(|a, b| a.id.cmp(&b.id)).expect("an operation");
    let starter = op_starter(op, Act::Act1, &d);
    let config = winnability_config(&op.id, Act::Act1, seed, &starter, &d).expect("config");
    let setup = player_setup(Policy::Mixed);
    let prepared = prepare_config(&config, &setup);
    let mut pilot = Pilot::new(&prepared, &setup, &d);
    for (i, v) in prepared.vessels.iter().enumerate() { eprintln!("  vessel {i}: side {} class {:?} tag {:?} group {:?} arrives {} s", v.side.0, v.class.as_ref().map(|c| c.as_str()), v.tag, v.group, v.arrives_s); }
    for g in &prepared.groups { eprintln!("  group {:?} tag {:?} ace {:?} orders {:?}", g.id, g.tag, g.ace.as_ref().map(|a| a.as_str()), g.orders); }
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    pool.install(|| {
        let mut sim = Sim::new(prepared.clone(), deps_with(&d)).expect("sim");
        let mut commands = Vec::new();
        let mut seen: BTreeMap<u16, u64> = BTreeMap::new();
        // Fastest speed through the water each AI hull made, knots, and its design top speed.
        let mut fastest: BTreeMap<u32, (f64, f64, String)> = BTreeMap::new();
        for i in 0..minutes * 3600 {
            if i % 60 == 0 {
                for s in sim.ships().ships.iter().filter(|s| s.player.is_none()) {
                    let Some(v) = sim.world().vessel(s.vessel) else { continue };
                    let e = fastest.entry(s.vessel.index).or_insert((0.0, s.design.turn.top_speed_mps / sim_core::time::KN, v.class.as_ref().map_or(String::new(), |c| c.as_str().to_string())));
                    e.0 = e.0.max(v.forward_speed() / sim_core::time::KN);
                }
            }
            if i % (300 * 60) == 0 && std::env::var("BP_SPEEDS").is_ok() {
                for s in sim.ships().ships.iter().filter(|s| s.player.is_none()) {
                    let Some(v) = sim.world().vessel(s.vessel) else { continue };
                    eprintln!("    t={:.0}s e{} {:?} speed {:.1} kn top {:.1} kn ordered {:?} captain {:?} group {:?}", i as f64 / 60.0, s.vessel.index, v.class.as_ref().map(|c| c.as_str()), v.forward_speed() / sim_core::time::KN, s.design.turn.top_speed_mps / sim_core::time::KN, s.orders.speed_kn, s.captain.as_ref().map(|c| (c.behaviour, c.mode)), s.captain.as_ref().map(|c| c.directive.mode));
                }
            }
            if sim.is_over() { eprintln!("over at {:.1} min: {:?}", i as f64 / 3600.0, sim.result().map(|r| (r.outcome, r.objective_done))); break; }
            commands.clear();
            pilot.commands(&sim, &mut commands);
            if !commands.is_empty() { sim.apply(&commands); }
            sim.set_host_load(crate::lod::HostLoad::IDLE);
            sim.step();
            for (k, e) in sim.schedule.spawned.iter() { if !seen.contains_key(k) { seen.insert(*k, i); eprintln!("  t={:.1}s config vessel {k} spawned as e{} (planned {} s)", i as f64 / 60.0, e.index, prepared.vessels.get(*k as usize).map_or(0, |v| v.arrives_s)); } }
            for e in sim.drain_events() {
                match &e.event {
                    SimEvent::EnemyAlert { side, level } => eprintln!("  t={:.1}s side {} alert {level}", i as f64 / 60.0, side.0),
                    SimEvent::Phys(PhysEvent::BodySpawned { body, kind, side, launcher, .. }) if sim.players().iter().all(|p| p.side != *side) => {
                        let Some(ship) = sim.ships().get(*launcher) else { continue };
                        let Some(v) = sim.world().vessel(*launcher) else { continue };
                        let me = DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z);
                        let ace = ship.captain.as_ref().and_then(|c| c.ace.clone());
                        let engaged: Vec<TrackId> = ship.stages.engage.mounts.iter().filter_map(|m| match m.status { EngageStatus::Engaging { track, .. } => Some(track), _ => None }).collect();
                        let tid = engaged.first().copied().unwrap_or(ship.orders.designated);
                        let pic = sim.pictures().side(*side);
                        let line = pic.and_then(|p| p.get(tid)).map(|t| {
                            let est = DVec2::new(t.pos.x, t.pos.z).distance(me);
                            let truth = sim.world().vessel(t.truth).map(|tv| DVec2::new(tv.kin.pose.pos.x, tv.kin.pose.pos.z).distance(me));
                            // A truth that is no hull: a body still in the world (its munition kind) or gone.
                            let body = if truth.is_some() { String::new() } else {
                                let b = sim.world().bodies();
                                match b.row_of(t.truth) { Some(r) => format!(" body {:?}", b.kind.get(r)), None if sim.world().locate(t.truth).is_some() => " located".into(), None => " gone".into() }
                            };
                            format!("t{} domain {:?} grade {:?} rung {:?} stance {:?} reason {:?} bearing_only {} age {:.1} s sources {:?} est {:.0} m truth {:?} m{body} created {:.1} s", t.id.0, t.domain, t.grade, t.rung, t.stance, t.stance_reason, t.bearing_only.is_some(), (i.saturating_sub(t.last_update_tick)) as f64 / 60.0, t.sources, est, truth.map(|x| x.round()), t.created_tick as f64 / 60.0)
                        }).unwrap_or_else(|| format!("track {} not in picture", tid.0));
                        // Every engaged track, since the launching mount is not known here.
                        let all: Vec<String> = engaged.iter().filter_map(|id| pic.and_then(|p| p.get(*id))).map(|t| format!("t{}:{:?}/{:?}/bo={}", t.id.0, t.domain, t.grade, t.bearing_only.is_some())).collect();
                        eprintln!("  t={:.1}s e{} kind {:?} from e{} class {:?} ace {:?} preset {:?} designated {} engaging {:?} all {all:?} | {line}", i as f64 / 60.0, body.index, kind, launcher.index, v.class.as_ref().map(|c| c.as_str()), ace.as_ref().map(|a| a.as_str()), ship.settings.preset, ship.orders.designated.0, engaged.iter().map(|t| t.0).collect::<Vec<_>>());
                    }
                    _ => {}
                }
            }
        }
        for (e, (max, top, class)) in &fastest { eprintln!("  e{e} {class}: fastest {max:.1} kn, design top {top:.1} kn"); }
    });
}

fn deps_with(d: &crate::theatre::TheatreData) -> SimDeps {
    SimDeps { catalogue: d.catalogue.clone(), campaign: d.campaign.clone(), designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() }
}
