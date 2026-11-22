use super::*;
use content::tuning::SpecialOpsTuning;
use glam::DVec3;

fn t() -> SpecialOpsTuning { SpecialOpsTuning::default() }
fn e(i: u32) -> EntityId { EntityId { index: i, generation: 0 } }

#[test]
fn hold_rule_matches_spec_numbers() {
    let t = t();
    assert_eq!(hold_rule(99.0, 2.9, t.hold_range_m, t.hold_max_speed_kn), Step::Advance);
    assert_eq!(hold_rule(100.0, 0.0, t.hold_range_m, t.hold_max_speed_kn), Step::Advance);
    assert_eq!(hold_rule(100.1, 0.0, t.hold_range_m, t.hold_max_speed_kn), Step::Pause(ActionBlock::TooFar));
    // "under 3 kn": exactly 3 kn is too fast.
    assert_eq!(hold_rule(10.0, 3.0, t.hold_range_m, t.hold_max_speed_kn), Step::Pause(ActionBlock::TooFast));
    assert_eq!(hold_rule(f64::NAN, 0.0, t.hold_range_m, t.hold_max_speed_kn), Step::Pause(ActionBlock::TooFar));
}

#[test]
fn boarding_pauses_before_and_aborts_after_progress() {
    let t = t();
    assert_eq!(board_rule(0.0, 150.0, 4.0, false, TargetCond::Disabled, &t), Step::Advance);
    assert_eq!(board_rule(0.0, 201.0, 4.0, false, TargetCond::Disabled, &t), Step::Pause(ActionBlock::TooFar));
    assert_eq!(board_rule(0.0, 150.0, 5.0, false, TargetCond::Disabled, &t), Step::Pause(ActionBlock::TooFast));
    assert_eq!(board_rule(0.0, 150.0, 1.0, true, TargetCond::Disabled, &t), Step::Pause(ActionBlock::UnderFire));
    assert_eq!(board_rule(10.0, 150.0, 1.0, true, TargetCond::Disabled, &t), Step::Abort(ActionBlock::UnderFire));
    assert_eq!(board_rule(0.0, 150.0, 1.0, false, TargetCond::UnderWay, &t), Step::Pause(ActionBlock::TargetUnderWay));
    assert_eq!(board_rule(10.0, 150.0, 1.0, false, TargetCond::UnderWay, &t), Step::Abort(ActionBlock::TargetUnderWay));
    assert_eq!(board_rule(10.0, 150.0, 1.0, false, TargetCond::Lost, &t), Step::Abort(ActionBlock::TargetLost));
    // Paused on distance mid-capture keeps progress (no abort).
    assert_eq!(board_rule(45.0, 260.0, 1.0, false, TargetCond::Disabled, &t), Step::Pause(ActionBlock::TooFar));
}

/// Sail a 500 m line at 8 kn and count drops: one per 50 m, 11 in all (0, 50 ... 500 m).
#[test]
fn mine_line_drops_every_50_m() {
    let t = t();
    let a = DVec2::new(0.0, 0.0);
    let b = DVec2::new(500.0, 0.0);
    assert_eq!(mines_for_line(500.0, &t), 11);
    let v = DVec2::new(8.0 * KN, 0.0);
    let mut pos = DVec2::new(-30.0, 5.0);
    let (mut next, mut laid, mut finished) = (0u16, 0u16, false);
    for _ in 0..20_000 {
        let m = mine_rule(a, b, MineFacts { pos, vel: v, rail_mines: 40 }, next, laid, &t);
        if m.drop { laid += 1; }
        next = m.next_drop;
        if let Step::Finish(_) = m.step { finished = true; break; }
        pos += v * DT;
    }
    assert!(finished);
    assert_eq!(laid, 11);
}

#[test]
fn mine_line_blocks() {
    let t = t();
    let a = DVec2::ZERO;
    let b = DVec2::new(500.0, 0.0);
    let on = MineFacts { pos: DVec2::new(10.0, 0.0), vel: DVec2::new(4.0, 0.0), rail_mines: 5 };
    // Too slow: 5 kn.
    let slow = MineFacts { vel: DVec2::new(5.0 * KN, 0.0), ..on };
    assert_eq!(mine_rule(a, b, slow, 1, 1, &t).step, Step::Pause(ActionBlock::TooSlow));
    // 6 degrees off course.
    let ang = 6.0f64.to_radians();
    let off = MineFacts { vel: DVec2::new(libm::cos(ang), libm::sin(ang)) * 4.0, ..on };
    assert_eq!(mine_rule(a, b, off, 1, 1, &t).step, Step::Pause(ActionBlock::OffLine));
    // 4 degrees is inside the 5 degree tolerance.
    let ang = 4.0f64.to_radians();
    let ok = MineFacts { vel: DVec2::new(libm::cos(ang), libm::sin(ang)) * 4.0, ..on };
    assert_eq!(mine_rule(a, b, ok, 1, 1, &t).step, Step::Advance);
    // Wrong way along the line.
    let back = MineFacts { vel: DVec2::new(-4.0, 0.0), ..on };
    assert_eq!(mine_rule(a, b, back, 1, 1, &t).step, Step::Pause(ActionBlock::OffLine));
    // Too far across the line.
    let wide = MineFacts { pos: DVec2::new(100.0, 80.0), ..on };
    assert_eq!(mine_rule(a, b, wide, 1, 1, &t).step, Step::Pause(ActionBlock::OffLine));
    // Rail empty.
    let empty = MineFacts { rail_mines: 0, ..on };
    assert_eq!(mine_rule(a, b, empty, 1, 1, &t).step, Step::Finish(Some(ActionBlock::NoMinesLeft)));
    assert_eq!(mine_rule(a, b, empty, 0, 0, &t).step, Step::Abort(ActionBlock::NoMinesLeft));
    // Passing a drop point off course skips it: back on the line at 160 m, the next drop is point 3 (150 m)
    // and after it point 4.
    let back_on = MineFacts { pos: DVec2::new(160.0, 0.0), ..on };
    let m = mine_rule(a, b, back_on, 1, 1, &t);
    assert!(m.drop);
    assert_eq!(m.next_drop, 4);
}

#[test]
fn photo_rule_orders_reasons() {
    let base = PhotoFacts { range_m: 5_000.0, horizon_m: 20_000.0, line_of_sight: true, visibility_m: 30_000.0, photo_range_m: PHOTO_RANGE_M, fov_rad: PHOTO_FOV_DEG.to_radians(), off_boresight_rad: 0.0 };
    assert_eq!(photo_rule(base), Step::Advance);
    assert_eq!(photo_rule(PhotoFacts { range_m: 6_001.0, ..base }), Step::Pause(ActionBlock::TooFar));
    // Rain: V = 2 km caps the camera.
    assert_eq!(photo_rule(PhotoFacts { visibility_m: 2_000.0, ..base }), Step::Pause(ActionBlock::BeyondVisibility));
    assert_eq!(photo_rule(PhotoFacts { horizon_m: 4_000.0, ..base }), Step::Pause(ActionBlock::NoLineOfSight));
    assert_eq!(photo_rule(PhotoFacts { line_of_sight: false, ..base }), Step::Pause(ActionBlock::NoLineOfSight));
    assert_eq!(photo_rule(PhotoFacts { off_boresight_rad: 4.9f64.to_radians(), ..base }), Step::Advance);
    assert_eq!(photo_rule(PhotoFacts { off_boresight_rad: -5.1f64.to_radians(), ..base }), Step::Pause(ActionBlock::OutsideFieldOfView));
    assert_eq!(photo_rule(PhotoFacts { range_m: f64::NAN, ..base }), Step::Pause(ActionBlock::TooFar));
}

#[test]
fn camera_slews_the_short_way_and_wraps() {
    let step = CAMERA_SLEW_DPS.to_radians() * DT;
    let c = slew(0.0, 1.0, step);
    assert!((c - step).abs() < 1e-12);
    // From +170 to -170 degrees the short way is through 180.
    let c = slew(170f64.to_radians(), -170f64.to_radians(), 5f64.to_radians());
    assert!((wrap_pi(c - 175f64.to_radians())).abs() < 1e-9);
    assert_eq!(slew(0.2, 0.2001, step), wrap_pi(0.2001));
    for a in [-10.0, -3.2, 0.0, 3.2, 10.0] { let w = wrap_pi(a); assert!((-core::f64::consts::PI..core::f64::consts::PI).contains(&w)); }
}

#[test]
fn under_fire_reads_hits_and_close_hostile_splashes() {
    use sim_core::projectiles::ProjectileId;
    let us = e(1);
    let prize = e(2);
    let enemy = e(3);
    let friend = e(4);
    let hostile = |s: EntityId| s == enemy;
    let pos = [Some(DVec2::ZERO), Some(DVec2::new(150.0, 0.0))];
    let dmg = |v, cause| PhysEvent::CellsDamaged { vessel: v, changes: vec![], cause, attacker: enemy };
    assert!(under_fire(&[dmg(prize, DamageCause::Shell)], [Some(us), Some(prize)], pos, &hostile));
    assert!(!under_fire(&[dmg(prize, DamageCause::Fire)], [Some(us), Some(prize)], pos, &hostile));
    assert!(!under_fire(&[dmg(e(9), DamageCause::Shell)], [Some(us), Some(prize)], pos, &hostile));
    let splash = |x: f64, shooter| PhysEvent::Splash { projectile: ProjectileId(1), pos: DVec3::new(x, 0.0, 0.0), calibre_mm: 76, shooter };
    assert!(under_fire(&[splash(180.0, enemy)], [Some(us), Some(prize)], pos, &hostile));
    assert!(!under_fire(&[splash(300.0, enemy)], [Some(us), Some(prize)], pos, &hostile));
    assert!(!under_fire(&[splash(160.0, friend)], [Some(us), Some(prize)], pos, &hostile));
}

fn action(id: u32, state: ActionState) -> Action {
    Action { id: ActionId(id), action: StationAction::Photograph, actor: e(1), player: Some(PlayerSlot(0)), target: ActionTarget::Track(TrackId(7)), target_entity: Some(e(2)), state, block: None, progress: 10.0, needed: 20.0, rhib: None, mines_laid: 0, started_tick: 0, part: Some(PartSlot(3)), site: Some(DVec2::new(5.0, 5.0)), site_name: String::new(), rhib_requested: false, evidence_sides: 0, camera_rel_rad: None, next_drop: 0, reported: None }
}

#[test]
fn cancel_and_view() {
    let mut s = SpecOpsState::default();
    s.actions.push(action(1, ActionState::Running));
    s.actions.push(action(2, ActionState::Done));
    assert!(!cancel(&mut s, ActionId(2)));
    assert!(!cancel(&mut s, ActionId(9)));
    assert!(cancel(&mut s, ActionId(1)));
    assert_eq!(s.actions[0].state, ActionState::Aborted);
    assert_eq!(s.actions[0].block, Some(ActionBlock::Cancelled));
    let v = view(&s.actions[0]);
    assert_eq!(v.progress, 0.5);
    // Track targets never expose the truth position.
    assert_eq!(v.at, None);
    let mut line = action(3, ActionState::Running);
    line.target = ActionTarget::Line { a: DVec2::ZERO, b: DVec2::X * 100.0 };
    line.needed = 0.0;
    let v = view(&line);
    assert_eq!(v.at, Some(DVec2::new(5.0, 5.0)));
    assert_eq!(v.progress, 0.0);
}

#[test]
fn prize_decisions() {
    let mut s = SpecOpsState::default();
    s.prizes.push(Prize { vessel: e(5), captor: PlayerSlot(1), decision: None, reached_exit: false, crew_captured: 12, stripped: false });
    assert!(!decide_prize(&mut s, e(6), PrizeDecision::SailHome));
    assert!(decide_prize(&mut s, e(5), PrizeDecision::SailHome));
    assert!(decide_prize(&mut s, e(5), PrizeDecision::StripOnSpot));
    s.prizes[0].stripped = true;
    assert!(!decide_prize(&mut s, e(5), PrizeDecision::SailHome));
    assert_eq!(s.prizes[0].decision, Some(PrizeDecision::StripOnSpot));
}

#[test]
fn state_round_trips_through_serde() {
    let mut s = SpecOpsState::default();
    s.actions.push(action(1, ActionState::Paused));
    s.prizes.push(Prize { vessel: e(5), captor: PlayerSlot(1), decision: Some(PrizeDecision::SailHome), reached_exit: true, crew_captured: 3, stripped: false });
    s.loss_sites.push(LossSite { vessel: e(8), side: SideId(1), pos: DVec2::new(1.0, 2.0), crew: 14 });
    let bytes = bincode::serialize(&s).unwrap();
    let back: SpecOpsState = bincode::deserialize(&bytes).unwrap();
    assert_eq!(back, s);
}
