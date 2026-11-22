//! Tests of unit `net-interp`: the extrapolation gate profile (L915), interpolation accuracy, pacing changes,
//! the adaptive delay (L916), lifecycle gating, the correction filter (L910) and client shells (L926).
use super::*;
use crate::delta::{DecodedFrame, ItemKey};
use crate::transport::conditioner::BurstLoss;
use content::ids::{ContentHash, SideId};
use sim_core::vessel::VesselStatus;
use sim_systems::ids::TrackId;
use sim_systems::protocol::command::TimeRate;
use sim_systems::protocol::config::EntityIntro;
use sim_systems::protocol::snapshot::{BodyView, SeekerPhase, VesselFlags};

fn eid(i: u32) -> EntityId { EntityId { index: i, generation: 1 } }

fn pacing(rate: TimeRate, paused: bool) -> Pacing { Pacing { paused, effective: rate, time_scale: 1.0, ..Default::default() } }

fn vessel(e: EntityId, pos: DVec3, vel: DVec3, rel: Relevance) -> VesselView {
    VesselView { entity: e, kin: Kinematics { pose: Pose::new(pos, DQuat::IDENTITY), lin_vel: vel, ang_vel: DVec3::ZERO }, status: VesselStatus::Active, flags: VesselFlags::empty(), turrets: vec![], rudder: 0.0, power: 0.5, track: TrackId::NONE, relevance: rel }
}

fn frame(seq: u32, tick: u64, pace: Pacing, vessels: Vec<VesselView>, bodies: Vec<BodyView>) -> DecodedFrame {
    let mut fresh: Vec<ItemKey> = vessels.iter().map(|v| ItemKey::Vessel(v.entity)).collect();
    fresh.extend(bodies.iter().map(|b| ItemKey::Body(b.entity)));
    DecodedFrame { seq, tick, snapshot: Snapshot { tick, pacing: pace, vessels, bodies, ..Default::default() }, fresh }
}

fn reveal(e: EntityId, tick: u64) -> WireEvent {
    WireEvent { tick, event: SimEvent::EntityRevealed(Box::new(EntityIntro { entity: e, side: SideId(0), blueprint: ContentHash::EMPTY, exterior_only: false, class: None, name: String::new(), player: None, status: VesselStatus::Active, damage: vec![], removed: vec![], parent: None, track: TrackId::NONE })) }
}

/// A hull on a 400 m circle at 12 m/s: position and velocity at sim time `s`.
fn circle(s: f64) -> (DVec3, DVec3) {
    let (r, v) = (400.0, 12.0);
    let w = v / r;
    (DVec3::new(r * libm::cos(w * s), 0.0, r * libm::sin(w * s)), DVec3::new(-v * libm::sin(w * s), 0.0, v * libm::cos(w * s)))
}

struct Lcg(u64);
impl Lcg {
    fn unit(&mut self) -> f64 { self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); ((self.0 >> 11) as f64) / ((1u64 << 53) as f64) }
}

/// Gilbert-Elliott loss like the link conditioner's.
struct Loss { model: BurstLoss, bad: bool }
impl Loss {
    fn lose(&mut self, rng: &mut Lcg) -> bool {
        let l = self.model;
        if self.bad { if rng.unit() < l.p_bad_to_good as f64 { self.bad = false; } } else if rng.unit() < l.p_good_to_bad as f64 { self.bad = true; }
        let p = if self.bad { l.loss_bad } else { l.loss_good };
        p > 0.0 && rng.unit() < p as f64
    }
}

/// Result of one simulated link run.
struct Run { frames: u64, extrapolated: u64, max_err_m: f64, min_rate: f64, max_rate: f64, delays: Vec<f64> }

/// How loss falls on the snapshot frames.
#[derive(Clone, Copy, Debug)]
enum LossModel {
    /// Each frame lost independently.
    Independent,
    /// The link conditioner's Gilbert-Elliott chain (mean burst 3 packets), stepped once per packet with
    /// `packets` packets sent per frame interval and the frame riding in one of them.
    Burst { packets: u32 },
}

/// 30 Hz frames of one own hull over a link with `one_way_ms` latency, uniform +/- `jitter_ms` jitter and
/// mean `loss`, rendered at 60 Hz for `seconds`; counts from `warm_s` on.
#[allow(clippy::too_many_arguments)]
fn run_link(tuning: InterpTuning, one_way_ms: f64, jitter_ms: f64, loss: f32, model: LossModel, seconds: f64, warm_s: f64, seed: u64) -> Run {
    let mut rng = Lcg(seed);
    let (chain, packets) = match model {
        LossModel::Independent => (BurstLoss { p_good_to_bad: 0.0, p_bad_to_good: 1.0, loss_good: loss, loss_bad: loss }, 1),
        LossModel::Burst { packets } => (BurstLoss::with_mean(loss), packets.max(1)),
    };
    let mut loss = Loss { model: chain, bad: false };
    let mut buf = SnapshotBuffer::new(tuning);
    let own = eid(1);
    buf.on_event(&reveal(own, 0));
    let interval = 1.0 / 30.0;
    // Deliveries: (arrival s, tick).
    let mut inflight: Vec<(f64, u64)> = Vec::new();
    let mut next_send = 0.0;
    let mut tick = 0u64;
    let mut seq = 0u32;
    let mut next_render = 0.0;
    let mut run = Run { frames: 0, extrapolated: 0, max_err_m: 0.0, min_rate: f64::MAX, max_rate: 0.0, delays: vec![] };
    let mut last_r = -1.0;
    let mut t = 0.0;
    while t < seconds {
        t += 0.0005;
        if t >= next_send {
            next_send += interval;
            tick += 2;
            let mut lost = false;
            for p in 0..packets { let l = loss.lose(&mut rng); if p == 0 { lost = l; } }
            if !lost {
                let d = (one_way_ms + jitter_ms * (2.0 * rng.unit() - 1.0)).max(0.0) / 1000.0;
                inflight.push((t + d, tick));
            }
        }
        inflight.sort_by(|a, b| a.0.total_cmp(&b.0));
        while inflight.first().is_some_and(|f| f.0 <= t) {
            let (_, k) = inflight.remove(0);
            seq += 1;
            let (p, v) = circle(k as f64 / 60.0);
            // A late frame older than the newest is dropped by the decoder.
            if buf.newest_tick().is_some_and(|n| k <= n) { continue; }
            buf.push(frame(seq, k, pacing(TimeRate::X1, false), vec![vessel(own, p, v, Relevance::Own)], vec![]), Duration::from_secs_f64(t));
        }
        if t >= next_render {
            next_render += 1.0 / 60.0;
            let Some(f) = buf.sample(Duration::from_secs_f64(t)) else { continue };
            assert!(f.render_tick >= last_r, "playback never runs backwards: {} after {last_r}", f.render_tick);
            last_r = f.render_tick;
            if t < warm_s { continue; }
            run.frames += 1;
            if f.extrapolating { run.extrapolated += 1; continue; }
            let truth = circle(f.render_tick / 60.0).0;
            let shown = f.snapshot.vessels.first().expect("own hull drawn").kin.pose.pos;
            run.max_err_m = run.max_err_m.max((shown - truth).length());
            let s = buf.stats();
            run.min_rate = run.min_rate.min(s.playback_rate);
            run.max_rate = run.max_rate.max(s.playback_rate);
            run.delays.push(s.delay_s);
        }
    }
    run
}

#[test]
fn independent_loss_at_the_gate_profile_extrapolates_under_one_percent() {
    // L915's profile (150 ms latency, 75 ms each way, 5% loss, 30 ms jitter) with independent loss: what the
    // clock itself adds. The gate under the conditioner's burst loss is measured end to end in
    // `session::server::tests::gate_profile_over_the_conditioned_transport`.
    for seed in [1u64, 2, 3] {
        let run = run_link(InterpTuning::default(), 75.0, 30.0, 0.05, LossModel::Independent, 125.0, 5.0, seed);
        let share = run.extrapolated as f64 / run.frames as f64;
        assert!(share <= 0.01, "seed {seed}: extrapolated {} of {} frames ({:.2}%)", run.extrapolated, run.frames, share * 100.0);
        assert!(run.max_err_m < 0.02, "seed {seed}: interpolated pose off by {} m", run.max_err_m);
        assert!(run.min_rate >= 0.95 - 1e-9 && run.max_rate <= 1.05 + 1e-9, "playback rate {}..{}", run.min_rate, run.max_rate);
        // The fixed default delay is 4 intervals.
        assert!(run.delays.iter().all(|d| (d - 4.0 / 30.0).abs() < 1e-9));
    }
}

/// Extrapolated share of frames over five seeds of the gate profile under burst loss.
fn burst_share(adaptive: bool, packets: u32) -> f64 {
    let tuning = InterpTuning { adaptive, ..InterpTuning::default() };
    let (mut ex, mut n) = (0, 0);
    for seed in 1..6u64 {
        let run = run_link(tuning, 75.0, 30.0, 0.05, LossModel::Burst { packets }, 125.0, 5.0, seed);
        ex += run.extrapolated;
        n += run.frames;
    }
    ex as f64 / n as f64
}

#[test]
fn burst_loss_stays_near_what_the_delay_can_cover() {
    // The conditioner loses packets in bursts of 3 on average. When every snapshot rides in its own packet and
    // nothing else is sent, a burst removes 3 frames (100 ms) and 4 or more outrun a 133 ms buffer whatever
    // the clock does. With other traffic between frames (events, session messages, renet2 acks) a burst
    // covers fewer frames and the share falls under the gate's 1%.
    let one = burst_share(false, 1);
    eprintln!("burst loss, one packet per frame: fixed {:.2}%, adaptive {:.2}%", one * 100.0, burst_share(true, 1) * 100.0);
    assert!(one < 0.025, "{one}");
    assert!(burst_share(true, 1) < 0.03);
    assert!(burst_share(false, 2) <= 0.01);
    assert!(burst_share(false, 3) <= 0.01);
}

#[test]
fn clean_link_never_extrapolates_and_tracks_truth() {
    let run = run_link(InterpTuning::default(), 20.0, 0.0, 0.0, LossModel::Independent, 30.0, 2.0, 7);
    assert_eq!(run.extrapolated, 0);
    assert!(run.max_err_m < 1e-3, "{}", run.max_err_m);
}

#[test]
fn adaptive_delay_follows_jitter_inside_its_clamp() {
    let tuning = InterpTuning { adaptive: true, ..InterpTuning::default() };
    let iv = 1.0 / 30.0;
    // No jitter: the floor of 3 intervals.
    let quiet = run_link(tuning, 40.0, 0.0, 0.0, LossModel::Independent, 20.0, 8.0, 3);
    assert!(quiet.delays.iter().all(|d| (d - 3.0 * iv).abs() < 0.002), "{:?}", quiet.delays.first());
    // Heavy jitter: up to the ceiling of 4.5 intervals, and never extrapolating more than the gate allows.
    let rough = run_link(tuning, 75.0, 60.0, 0.05, LossModel::Independent, 60.0, 8.0, 4);
    assert!(rough.delays.iter().all(|d| *d >= 3.0 * iv - 1e-9 && *d <= 4.5 * iv + 1e-9));
    assert!(rough.delays.iter().any(|d| *d > 4.0 * iv), "60 ms jitter needs a longer delay");
    assert!(rough.min_rate >= 0.95 - 1e-9 && rough.max_rate <= 1.05 + 1e-9);
    // 30 ms jitter, the gate profile, under the adaptive fallback.
    let gate = run_link(tuning, 75.0, 30.0, 0.05, LossModel::Independent, 65.0, 5.0, 5);
    assert!(gate.extrapolated as f64 / gate.frames as f64 <= 0.01, "{} of {}", gate.extrapolated, gate.frames);
}

/// Drive a buffer with frames every 1/30 s at `ticks_per_frame`, delivered after 50 ms, rendering at 60 Hz.
fn drive(buf: &mut SnapshotBuffer, t: &mut f64, tick: &mut u64, seq: &mut u32, seconds: f64, ticks_per_frame: u64, pace: Pacing) -> Vec<f64> {
    let own = eid(1);
    let mut renders = Vec::new();
    let end = *t + seconds;
    let mut next_frame = *t;
    while *t < end {
        if *t >= next_frame {
            next_frame += 1.0 / 30.0;
            *tick += ticks_per_frame;
            *seq += 1;
            let (p, v) = circle(*tick as f64 / 60.0);
            buf.push(frame(*seq, *tick, pace.clone(), vec![vessel(own, p, v, Relevance::Own)], vec![]), Duration::from_secs_f64(*t + 0.05));
        }
        if let Some(f) = buf.sample(Duration::from_secs_f64(*t + 0.05)) { renders.push(f.render_tick); }
        *t += 1.0 / 60.0;
    }
    renders
}

#[test]
fn pause_and_compression_change_the_tick_rate_without_running_backwards() {
    let mut buf = SnapshotBuffer::new(InterpTuning::default());
    buf.on_event(&reveal(eid(1), 0));
    let (mut t, mut tick, mut seq) = (0.0, 0u64, 0u32);
    let a = drive(&mut buf, &mut t, &mut tick, &mut seq, 5.0, 2, pacing(TimeRate::X1, false));
    // Paused: the tick stands; playback stops without passing the newest frame.
    let paused = drive(&mut buf, &mut t, &mut tick, &mut seq, 3.0, 0, pacing(TimeRate::X1, true));
    let held = *paused.last().unwrap();
    assert!(held <= tick as f64 + 1e-9, "{held} > {tick}");
    assert!(paused[paused.len() / 2..].windows(2).all(|w| w[1] == w[0]), "playback stands while paused");
    // 4x: 8 ticks a frame. After settling the playback runs at about 240 ticks a second.
    let fast = drive(&mut buf, &mut t, &mut tick, &mut seq, 6.0, 8, pacing(TimeRate::X4, false));
    let all: Vec<f64> = a.iter().chain(&paused).chain(&fast).copied().collect();
    assert!(all.windows(2).all(|w| w[1] >= w[0]), "never backwards");
    let n = fast.len();
    let rate = (fast[n - 1] - fast[n - 61]) / 1.0;
    assert!((rate - 240.0).abs() < 240.0 * 0.06, "rate {rate}");
    // Delay in ticks scales with the rate: about 4 intervals of 8 ticks behind the newest frame.
    let lag = tick as f64 - fast[n - 1];
    assert!(lag > 20.0 && lag < 48.0, "lag {lag} ticks");
    assert_eq!(buf.stats().extrapolated, 0);
}

#[test]
fn reveals_gate_poses_and_ends_stop_drawing_at_their_tick() {
    let mut buf = SnapshotBuffer::new(InterpTuning::default());
    let (a, b) = (eid(1), eid(2));
    let pace = pacing(TimeRate::X1, false);
    let mut t = 0.0;
    for k in 0..90u64 {
        let tick = k * 2;
        let mut vs = vec![vessel(a, DVec3::new(tick as f64, 0.0, 0.0), DVec3::new(30.0, 0.0, 0.0), Relevance::Own)];
        // b is present until tick 96, when the server hides it.
        if tick < 96 { vs.push(vessel(b, DVec3::new(0.0, 0.0, tick as f64), DVec3::ZERO, Relevance::Near)); }
        buf.push(frame(k as u32 + 1, tick, pace.clone(), vs, vec![]), Duration::from_secs_f64(t));
        // a is revealed at once; b's reveal arrives late (another channel).
        if k == 0 { buf.on_event(&reveal(a, 0)); }
        if k == 30 { buf.on_event(&reveal(b, 0)); }
        // The hide event arrives for tick 96, ahead of the frames that drop b.
        if k == 40 { buf.on_event(&WireEvent { tick: 96, event: SimEvent::EntityHidden { entity: b } }); }
        let f = buf.sample(Duration::from_secs_f64(t)).unwrap();
        let ids: Vec<EntityId> = f.snapshot.vessels.iter().map(|v| v.entity).collect();
        assert!(ids.contains(&a));
        if k < 30 { assert!(!ids.contains(&b), "b held back before its reveal at frame {k}"); }
        if f.render_tick >= 96.0 { assert!(!ids.contains(&b), "b gone from tick 96, render {}", f.render_tick); }
        if k > 30 && f.render_tick < 95.0 { assert!(ids.contains(&b), "b drawn after its reveal at frame {k}, render {}", f.render_tick); }
        t += 1.0 / 30.0;
    }
    // Ended records are forgotten once playback is well past the end.
    for k in 90..200u64 {
        buf.push(frame(k as u32 + 1, k * 2, pace.clone(), vec![vessel(a, DVec3::ZERO, DVec3::ZERO, Relevance::Own)], vec![]), Duration::from_secs_f64(t));
        buf.sample(Duration::from_secs_f64(t));
        t += 1.0 / 30.0;
    }
    assert!(!buf.vessels.contains_key(&b) && !buf.life.contains_key(&b));
    // A body is not drawn before its first sample; it ends at its impact tick.
    let m = eid(9);
    let body = |tick: u64| BodyView { entity: m, kind: content::ids::MunitionIdx(0), side: SideId(1), pos: DVec3::new(0.0, 10.0, tick as f64 * 5.0), vel: DVec3::new(0.0, 0.0, 300.0), phase: Default::default(), seeker: SeekerPhase::Off, track: TrackId::NONE, relevance: Relevance::Engaged };
    let mut buf = SnapshotBuffer::new(InterpTuning::default());
    buf.on_event(&reveal(a, 0));
    buf.on_event(&reveal(m, 0));
    let mut t = 0.0;
    for k in 0..60u64 {
        let tick = k * 2;
        let bodies = if (40..100).contains(&tick) && tick % 12 == 4 { vec![body(tick)] } else { vec![] };
        let mut f = frame(k as u32 + 1, tick, pace.clone(), vec![vessel(a, DVec3::ZERO, DVec3::ZERO, Relevance::Own)], bodies.clone());
        // Bodies between their 5 Hz samples are carried forward unchanged (not fresh).
        if bodies.is_empty() && (40..100).contains(&tick) {
            if let Some(last) = buf.bodies.get(&m).and_then(|r| r.samples.back()) { f.snapshot.bodies.push(last.1); }
        }
        buf.push(f, Duration::from_secs_f64(t));
        if k == 45 { buf.on_event(&WireEvent { tick: 100, event: SimEvent::Phys(PhysEvent::BodyImpact { body: m, vessel: a, cell: content::ids::CellIndex(0), world_point: DVec3::ZERO, cause: sim_core::damage::DamageCause::Missile }) }); }
        let s = buf.sample(Duration::from_secs_f64(t)).unwrap();
        let drawn = s.snapshot.bodies.iter().any(|b| b.entity == m);
        if s.render_tick < 40.0 { assert!(!drawn, "no body before its first sample (render {})", s.render_tick); }
        if s.render_tick >= 100.0 { assert!(!drawn, "no body after its impact (render {})", s.render_tick); }
        if (41.0..99.0).contains(&s.render_tick) {
            assert!(drawn, "render {}", s.render_tick);
            // Between 5 Hz samples the missile flies, it does not stall.
            let b = s.snapshot.bodies.iter().find(|b| b.entity == m).unwrap();
            assert!((b.pos.z - s.render_tick * 5.0).abs() < 0.5, "{} at {}", b.pos.z, s.render_tick);
        }
        t += 1.0 / 30.0;
    }
}

#[test]
fn a_new_timeline_clears_the_buffer() {
    let mut buf = SnapshotBuffer::new(InterpTuning::default());
    buf.on_event(&reveal(eid(1), 0));
    let pace = pacing(TimeRate::X1, false);
    for k in 1..40u64 { buf.push(frame(k as u32, 1000 + k * 2, pace.clone(), vec![vessel(eid(1), DVec3::ZERO, DVec3::ZERO, Relevance::Own)], vec![]), Duration::from_secs_f64(k as f64 / 30.0)); }
    assert!(buf.sample(Duration::from_secs_f64(1.4)).is_some());
    buf.push(frame(41, 4, pace, vec![vessel(eid(1), DVec3::ONE, DVec3::ZERO, Relevance::Own)], vec![]), Duration::from_secs_f64(1.5));
    assert_eq!(buf.newest_tick(), Some(4));
    let f = buf.sample(Duration::from_secs_f64(1.5)).unwrap();
    assert!(f.render_tick <= 4.0);
    // Samples went with the old timeline; the reveal (sent once, reliably) stays.
    assert_eq!(f.snapshot.vessels.len(), 1);
    assert_eq!(f.snapshot.vessels[0].kin.pose.pos, DVec3::ONE);
    // A full clear forgets the reveal too.
    buf.clear();
    buf.push(frame(42, 6, pacing(TimeRate::X1, false), vec![vessel(eid(1), DVec3::ONE, DVec3::ZERO, Relevance::Own)], vec![]), Duration::from_secs_f64(1.6));
    assert!(buf.sample(Duration::from_secs_f64(1.6)).unwrap().snapshot.vessels.is_empty());
}

#[test]
fn corrections_decay_with_the_half_life_and_large_errors_snap() {
    let tuning = InterpTuning::default();
    let mut c = CorrectionFilter::new();
    let e = eid(1);
    let auth = Pose::new(DVec3::new(100.0, 0.0, 0.0), DQuat::IDENTITY);
    let shown = Pose::new(DVec3::new(102.0, 0.0, 0.0), DQuat::from_rotation_y(0.02));
    assert!(!c.correct(e, &shown, &auth, &tuning));
    assert!((c.apply(e, &auth).pos - shown.pos).length() < 1e-9, "no visible jump when the correction arrives");
    assert!((c.max_offset_m() - 2.0).abs() < 1e-12);
    c.decay(0.15, &tuning);
    assert!((c.max_offset_m() - 1.0).abs() < 1e-9, "one half-life halves it: {}", c.max_offset_m());
    for _ in 0..30 { c.decay(0.15 / 3.0, &tuning); }
    assert!((c.max_offset_m() - 2.0 * libm::exp2(-11.0)).abs() < 1e-9);
    let p = c.apply(e, &auth);
    assert!(p.rot.angle_between(auth.rot) < 0.02 * libm::exp2(-10.0));
    // Over 10 m snaps and clears.
    assert!(c.correct(e, &Pose::new(DVec3::new(115.0, 0.0, 0.0), DQuat::IDENTITY), &auth, &tuning));
    assert_eq!(c.max_offset_m(), 0.0);
    assert_eq!(c.apply(e, &auth), auth);
    // Garbage snaps too.
    assert!(c.correct(e, &Pose::new(DVec3::NAN, DQuat::IDENTITY), &auth, &tuning));
    c.decay(f64::NAN, &tuning);
    c.decay(-1.0, &tuning);
}

fn spawn(id: u32, tick: u64) -> ProjectileSpawn {
    ProjectileSpawn { id: ProjectileId(id), tick, origin: DVec3::new(0.0, 12.0, 0.0), velocity: DVec3::new(600.0, 150.0, 30.0), calibre_mm: 76, band: content::part::weapons::CalibreBand::Medium, shooter: eid(1), mount: content::ids::PartSlot(2), rounds: 1, point_defence: false }
}

#[test]
fn client_shells_match_the_server_integration() {
    let k = 2.2e-4;
    let mut tr = ShellTracker::new();
    let s = spawn(5, 100);
    tr.spawn(&s, k);
    tr.spawn(&s, k);
    assert_eq!(tr.len(), 1);
    // The server: one step in the spawn tick and every tick after.
    let (mut p, mut v) = (s.origin, s.velocity);
    let mut server = vec![(p, v)];
    for _ in 0..600 {
        (p, v) = sim_core::projectiles::ballistic_step(p, v, k, sim_core::time::DT);
        server.push((p, v));
    }
    tr.advance_to(100 + 240);
    let sh = tr.shells[&ProjectileId(5)];
    assert_eq!((sh.pos, sh.vel), server[240], "bit-identical to the server's steps");
    // Drawing between ticks is linear within the tick.
    let mut out = Vec::new();
    tr.render_positions(340.25, &mut out);
    assert_eq!(out.len(), 1);
    assert!((out[0].1 - server[240].0.lerp(server[241].0, 0.25)).length() < 1e-9);
    // Not fired yet at an earlier render tick.
    tr.render_positions(99.5, &mut out);
    assert!(out.is_empty());

    // Close at an impact inside the step of tick 400 (event tick 401): drawn up to the point, then gone.
    let (a, b) = (server[300].0, server[301].0);
    let hit = a.lerp(b, 0.6);
    tr.close(ProjectileId(5), 401, hit);
    assert!((tr.impact_point(ProjectileId(5), 401, hit).unwrap() - hit).length() < 1e-6, "the client's flight passes through the server's impact point");
    let mut tr2 = ShellTracker::new();
    tr2.spawn(&s, k);
    tr2.close(ProjectileId(5), 401, hit);
    tr2.advance_to(398);
    tr2.render_positions(400.5, &mut out);
    assert_eq!(out.len(), 1);
    assert!((out[0].1 - a.lerp(b, 0.5)).length() < 1e-6);
    tr2.render_positions(400.7, &mut out);
    assert!(out.is_empty(), "past the impact point");
    tr2.advance_to(500);
    assert_eq!(tr2.len(), 0);
    assert!(tr2.ends.is_empty());

    // Removal returns the client's position; unknown ids give None.
    let mut tr3 = ShellTracker::new();
    tr3.spawn(&s, k);
    tr3.advance_to(110);
    assert_eq!(tr3.remove(ProjectileId(5)), Some(server[10].0));
    assert_eq!(tr3.remove(ProjectileId(5)), None);
    // Bad spawns are ignored; a shell that never hears its splash goes once it is far under the sea.
    tr3.spawn(&ProjectileSpawn { origin: DVec3::NAN, ..s }, k);
    assert_eq!(tr3.len(), 0);
    tr3.spawn(&ProjectileSpawn { velocity: DVec3::new(300.0, -50.0, 0.0), ..s }, k);
    tr3.advance_to(100 + 600);
    assert_eq!(tr3.len(), 0);
}

#[test]
fn drag_constants_come_from_the_catalogue_band_rows() {
    let cat = content::catalogue::Catalogue::load_embedded().expect("catalogue");
    for b in content::part::weapons::CalibreBand::ALL {
        let k = band_drag_k(&cat, b);
        assert!(k > 0.0 && k < 1e-2, "{b:?} {k}");
        assert_eq!(k, cat.band(b).drag_k);
    }
}

/// A sampled snapshot's live tracks describe its own tick: the base frame's tracks are carried from the base
/// frame's tick by their velocity (last-known marks stay). Before, they kept the base frame's positions under
/// the sampled tick, so the client's carry from that tick fell short by up to a frame interval.
#[test]
fn sampled_tracks_describe_the_sampled_tick() {
    use sim_systems::protocol::snapshot::TrackView;
    let mut buf = SnapshotBuffer::new(InterpTuning::default());
    buf.on_event(&reveal(eid(1), 0));
    let pace = pacing(TimeRate::X1, false);
    let mut t = sim_systems::tracks::blank_track(TrackId(3), SideId(0), EntityId::NONE, content::enums::Domain::Weapon, 900);
    t.vel = DVec3::new(0.0, 0.0, 600.0);
    let base = sim_systems::sim::snapshot::track_view(&t, None, 900, &content::tuning::TrackTuning::default());
    let track = |pos: DVec3, last_known: bool| TrackView { pos, last_known, ..base.clone() };
    for k in 1..60u64 {
        let tick = 1000 + k * 2;
        let at = DVec3::new(0.0, 0.0, 600.0 * (tick - 900) as f64 / 60.0);
        let mut f = frame(k as u32, tick, pace.clone(), vec![vessel(eid(1), DVec3::ZERO, DVec3::ZERO, Relevance::Own)], vec![]);
        f.snapshot.tracks = vec![track(at, false), TrackView { id: TrackId(4), ..track(DVec3::new(5.0, 0.0, 5.0), true) }];
        buf.push(f, Duration::from_secs_f64(k as f64 / 30.0));
    }
    let mut odd = 0;
    for i in 0..30 {
        let Some(f) = buf.sample(Duration::from_secs_f64(1.5 + i as f64 / 97.0)) else { continue };
        let tick = f.snapshot.tick;
        if tick % 2 == 1 { odd += 1; }
        let live = f.snapshot.tracks.iter().find(|t| t.id == TrackId(3)).expect("the live track");
        let want = 600.0 * (tick - 900) as f64 / 60.0;
        assert!((live.pos.z - want).abs() < 1e-6, "tick {tick}: track at {} m, its prediction {want} m", live.pos.z);
        let mark = f.snapshot.tracks.iter().find(|t| t.id == TrackId(4)).expect("the mark");
        assert_eq!(mark.pos, DVec3::new(5.0, 0.0, 5.0));
    }
    assert!(odd > 0, "some samples fall between frames");
}
