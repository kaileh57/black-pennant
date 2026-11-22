//! Unit `server-soak`. Soak tests and bot matches: a `ServerSession` and N bot `ClientSession`s in one process,
//! over the in-memory transport or loopback UDP, each client link through the link conditioner. DESIGN.md L901
//! (bot matches put server and every bot client on one hosted runner over loopback UDP through the link
//! conditioner), L915 (extrapolation in 1% or fewer of rendered frames at 150 ms, 5% loss, 30 ms jitter),
//! L928-933 (bandwidth gate: 20 bytes or less per vessel update, projectile events 25% or less, per-client p95
//! 256 kbps or less; no own-hull correction over 0.5 m; manual-aim hit rate at least 90% of the 0 ms baseline),
//! L951 (aim bot: direction-only shots, fixed 250 ms reaction, targets at 20 kn or more; shot log), L1027
//! (ladder controller gate with injected synthetic tick cost), L1031 (1 Mbps host upload with 7 clients: loss
//! under 5% within 20 s, nobody dropped), L1036 (300 ms, 10% loss, 50 ms jitter never dropped; 600 ms or 25%
//! loss dropped within 20 s).
//!
//! By default time runs on a `ManualClock` advanced in fixed 1 ms steps, so a soak of N minutes is repeatable
//! for its seed; the session then measures zero tick cost and the ladder sees only `--synthetic-tick-ms`
//! (the controller gate, L1027). Real-load runs (`--synthetic-tick-ms` unset and `--udp`) use `SystemClock`
//! in real time, so measured tick cost drives the ladder (the real-load gate, L1028). Bot clients render at
//! 60 Hz of the manual clock and sample their buffers as gdclient would.
//!
//! # How a soak runs (choices the spec leaves open)
//! - The host is a dedicated session (no host member), so every bot is subject to the drop rule and counts in
//!   the saturation medians. Its lobby is a sandbox lobby, the one mode that seats up to 8 players; the host's
//!   upload figure is set to what the bot count needs (`ceil(n x 256 / 0.9)` kbps, at least 1 Mbps), as a
//!   host running a large sandbox would enter it (L1031). The `upload-1mbps` preset then limits the real link
//!   to 1 Mbps underneath that figure, which is exactly the saturation case.
//! - The scene is the chosen reference scene for the seed. Bot `i` takes the scene's player hull `i`; a bot
//!   beyond the scene's player hulls gets a copy of the first one, 400 m further along the line abreast. A
//!   scene player hull with no bot sails under its officers. When a sortie ends before the soak does, the
//!   next one starts at once with the seed plus the sortie count.
//! - The soak's minutes run from the start of the first sortie; joining and loading come before. A bot whose
//!   connection ends before it is welcomed connects again, up to [`JOIN_ATTEMPTS`] times: renetcode2 1.0.0
//!   leaves a client whose first keep-alive was lost waiting until its 15 s timeout, because the host sends
//!   payload packets from then on and never another keep-alive, so a lossy link (the `gate` preset, seed 1)
//!   can fail a first handshake. A bot not welcomed within [`JOIN_TIMEOUT_S`] is reported without a slot
//!   (slot 255). `dropped_at_s` counts from the bot's welcome.
//! - Odd slots fly the aim bot (250 ms reaction, targets at 20 kn or more), even slots leave everything to
//!   their officers; every bot sends an input each render frame (camera at its hull), as gdclient does.
//! - The aim bot takes the hull's main gun by hand (the non-point-defence gun with the longest effective
//!   range), aims on what it saw 250 ms ago, leads the target by its seen velocity over that reaction time,
//!   the lag until the host lays the shot (render delay plus half the RTT) and the shell's flight (range over
//!   80% of muzzle velocity), aims at the sea at that point in the hull attitude carried forward over the
//!   lag, and fires a direction-only shot every 2 s while a target is inside 60% of the gun's effective range. Hits and shots come
//!   from the server's shot log (resolved rows only).
//! - The `gate` preset with aim bots first runs the same soak on a clean link for the 0 ms baseline hit rate.
//! - Frames count toward the extrapolation share from 2 s after each sortie's first frame reached a bot.
//! - Own-hull correction is measured as gdclient measures it (`gd-interp`): the jump of the sampled own pose
//!   against the previous sample carried forward by its velocity over the render-tick step.
//! - Tick times: wall time of each `ServerSession::update` that stepped, divided by its ticks, plus the
//!   synthetic cost. With a synthetic cost the load drops to 0 for the last third of the soak, so the ladder
//!   gate can see the host return to rung 0 (L1027).
//! - The `upload-1mbps` loss is the median of the panel's per-player loss at 1 Hz; it has "fallen under 5%"
//!   from the first second after which it stays under 5% for 10 s. Over the in-memory transport the
//!   conditioner wraps every socket with one profile, so there the 1 Mbps limit also applies (harmlessly) to
//!   each bot's own upload; loopback UDP conditions the host and each bot separately.
//! - The run exits 1 when a gate fails. A gate with nothing to measure (no frames, no vessel updates) fails;
//!   the aim gate needs at least [`MIN_AIM_SHOTS`] resolved shots in both runs and is reported failing
//!   otherwise.
use crate::args::{LinkPreset, SoakArgs};
use crate::error::{CliError, Code};
use net::session::client::ClientSession;
use net::session::server::Traffic;
use net::transport::conditioner::ConditionerProfile;
use serde::{Deserialize, Serialize};

use content::ids::{PartSlot, PlayerSlot};
use net::clock::{Clock, ManualClock, SystemClock};
use net::session::client::ClientState;
use net::session::server::ServerSession;
use net::session::{ClientEvent, ClientOptions, HostEvent, ServerOptions, SessionPhase};
use net::transport::conditioner::Conditioning;
use sim_core::math::{DVec3, Pose};
use sim_systems::protocol::command::{Command, ControlRole, PlayerInput, ShotDesignation};
use sim_systems::protocol::config::{PlayerInit, SortieConfig};
use sim_systems::protocol::snapshot::{Relevance, VesselFlags};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

/// What a bot does with its hull.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum BotPolicy {
    /// Officers do everything; the bot only keeps acks and inputs flowing.
    Officers,
    /// Direction-only manual shots at the nearest drawn target moving at least `min_target_kn`, aimed at
    /// where the target was `reaction_ms` ago in the bot's render frame (L951).
    AimBot { reaction_ms: u32, min_target_kn: f32 },
}

pub struct BotClient { pub session: ClientSession, pub policy: BotPolicy, pub shots: u32, pub(crate) aim: AimState }

impl BotClient {
    pub fn new(session: ClientSession, policy: BotPolicy) -> Self { Self { session, policy, shots: 0, aim: AimState::default() } }
    /// The hull's hand-aimable gun for the aim bot (set by the soak loop from the host's ship table).
    pub(crate) fn set_gun(&mut self, gun: Option<GunInfo>) { self.aim = AimState { gun, input_tick: self.aim.input_tick, ..AimState::default() }; }
}

/// The aim bot's gun: mount slot, muzzle position in the hull frame, muzzle velocity and effective range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GunInfo { pub(crate) slot: PartSlot, pub(crate) pos_local: DVec3, pub(crate) muzzle_mps: f64, pub(crate) range_m: f64 }

/// What the aim bot remembers between frames.
#[derive(Clone, Debug, Default)]
pub(crate) struct AimState {
    pub(crate) gun: Option<GunInfo>,
    held: bool,
    /// Render time (s) and the targets drawn then: (position, velocity).
    seen: VecDeque<(f64, Vec<(DVec3, DVec3)>)>,
    next_fire_s: f64,
    pub(crate) input_tick: u64,
}

/// One client's results.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClientReport {
    pub slot: u8,
    pub traffic: Traffic,
    pub frames_rendered: u64,
    pub frames_extrapolated: u64,
    pub max_own_correction_m: f64,
    pub dropped_at_s: Option<f64>,
    pub warnings: u32,
    pub shots: u32,
    pub hits: u32,
}

/// The soak report written as RON.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SoakReport {
    pub game_version: String,
    pub seed: u64,
    pub minutes: f64,
    pub link: String,
    pub ticks: u64,
    pub tick_p95_ms: f32,
    pub tick_p99_ms: f32,
    pub max_backlog_ticks: u32,
    pub ladder_steps: Vec<(f64, u8, u8, bool)>,
    pub clients: Vec<ClientReport>,
    /// Gate verdicts: (gate name, pass, measured value, limit).
    pub gates: Vec<(String, bool, f64, f64)>,
    /// Injected synthetic tick cost of the run (0 = none).
    #[serde(default)]
    pub(crate) synthetic_tick_ms: f32,
    /// Real-time run whose measured tick cost drives the ladder (L1028).
    #[serde(default)]
    pub(crate) real_load: bool,
    /// Sorties started during the soak.
    #[serde(default)]
    pub(crate) sorties: u32,
    /// Hit rate of the same soak on a clean link (the aim gate's 0 ms baseline) and its resolved shots.
    #[serde(default)]
    pub(crate) baseline_hit_rate: Option<f64>,
    #[serde(default)]
    pub(crate) baseline_shots: u32,
    /// Seconds after the first sortie start at which the median loss fell under 5% for good (upload preset).
    #[serde(default)]
    pub(crate) loss_settled_s: Option<f64>,
    /// Per time-scale step: seconds until the backlog was 3 ticks or fewer (the run's end if never).
    #[serde(default)]
    pub(crate) backlog_recovery_s: Vec<f64>,
    /// When the synthetic load was dropped, and how long the ladder took to reach rung 0 after it.
    #[serde(default)]
    pub(crate) load_dropped_at_s: Option<f64>,
    #[serde(default)]
    pub(crate) rung0_after_drop_s: Option<f64>,
}

/// Loop step of the soak: the session updates at 1 ms granularity.
const STEP: Duration = Duration::from_millis(1);
/// Bot render rate.
const FRAME: Duration = Duration::from_micros(16_667);
/// Bots that have not joined by then are left out.
pub const JOIN_TIMEOUT_S: f64 = 60.0;
/// Connection attempts a bot gets while joining (the first plus retries).
pub const JOIN_ATTEMPTS: usize = 3;
/// Frames after a sortie's first frame that do not count toward the extrapolation share.
const WARMUP_S: f64 = 2.0;
/// The aim bot's fire interval, render seconds.
const FIRE_INTERVAL_S: f64 = 2.0;
/// Mean shell speed over the flight as a share of muzzle velocity (the bot's lead estimate).
const MEAN_SHELL_SPEED: f64 = 0.8;
/// The aim bot engages inside this share of the gun's effective range (a gunlayer's practical range).
const ENGAGE_SHARE: f64 = 0.6;
/// The aim gate needs this many resolved shots in the run and in its baseline.
pub const MIN_AIM_SHOTS: u32 = 30;
/// Line-abreast spacing of extra bot hulls.
const EXTRA_HULL_SPACING_M: f64 = 400.0;
const KN: f64 = 0.514_444;

/// Unit: `server-soak`.
pub fn run(args: &SoakArgs) -> Result<Code, CliError> {
    let data = crate::runtime::load_data()?;
    let pool = crate::runtime::sim_pool(3)?;
    let campaign = data.campaign.clone();
    let scene = args.scene;
    let make = move |seed: u64| sim_systems::editor::trials::reference_config(scene, seed, &campaign);
    let started = std::time::Instant::now();
    let report = soak(args, &make, &data, &pool)?;
    crate::runtime::write_report(&report, args.out.as_deref())?;
    let failed: Vec<&(String, bool, f64, f64)> = report.gates.iter().filter(|g| !g.1).collect();
    for g in &report.gates { eprintln!("gate {} {} measured {:.4} limit {:.4}", g.0, if g.1 { "pass" } else { "FAIL" }, g.2, g.3); }
    eprintln!("soak {} minutes link {} clients {}: {} ticks, tick p95 {:.3} ms p99 {:.3} ms, {:.1} s wall", args.minutes, report.link, report.clients.len(), report.ticks, report.tick_p95_ms, report.tick_p99_ms, started.elapsed().as_secs_f64());
    Ok(if failed.is_empty() { crate::error::PASS } else { crate::error::FAIL })
}

/// The whole soak for a scene builder: the baseline when the gate needs one, the run, and its gates.
pub(crate) fn soak(args: &SoakArgs, make: &dyn Fn(u64) -> SortieConfig, data: &crate::runtime::Data, pool: &Arc<rayon::ThreadPool>) -> Result<SoakReport, CliError> {
    let aim_bots = (0..args.clients).any(|i| matches!(policy_for(i), BotPolicy::AimBot { .. }));
    let baseline = if args.link == LinkPreset::Gate && aim_bots {
        let clean = SoakArgs { link: LinkPreset::Clean, out: None, ..args.clone() };
        let b = run_once(&clean, make, data, pool)?;
        let (shots, hits) = b.clients.iter().fold((0u32, 0u32), |a, c| (a.0 + c.shots, a.1 + c.hits));
        Some((shots, if shots > 0 { hits as f64 / shots as f64 } else { 0.0 }))
    } else { None };
    let mut report = run_once(args, make, data, pool)?;
    if let Some((shots, rate)) = baseline { report.baseline_shots = shots; report.baseline_hit_rate = Some(rate); }
    report.gates = evaluate(&report, args.link);
    Ok(report)
}

/// Policy of the bot with index `i`: odd indices fly the aim bot.
pub(crate) fn policy_for(i: u8) -> BotPolicy {
    if i % 2 == 1 { BotPolicy::AimBot { reaction_ms: 250, min_target_kn: 20.0 } } else { BotPolicy::Officers }
}

/// Conditioner profile for a preset; `Upload1Mbps` limits the host side. Unit: `server-soak`.
pub fn profile(preset: LinkPreset) -> (ConditionerProfile, ConditionerProfile) {
    let both = |p: ConditionerProfile| (p, p);
    match preset {
        LinkPreset::Clean => both(ConditionerProfile::CLEAN),
        LinkPreset::Gate => both(ConditionerProfile::round_trip(150.0, 0.05, 30.0)),
        LinkPreset::Tolerated => both(ConditionerProfile::round_trip(300.0, 0.10, 50.0)),
        LinkPreset::DropPing => both(ConditionerProfile::round_trip(600.0, 0.0, 0.0)),
        LinkPreset::DropLoss => both(ConditionerProfile::round_trip(0.0, 0.25, 0.0)),
        LinkPreset::Upload1Mbps => (ConditionerProfile::CLEAN.with_upstream(1_000), ConditionerProfile::CLEAN),
    }
}

fn is_clean(p: &ConditionerProfile) -> bool { *p == ConditionerProfile::CLEAN }

/// One bot decision per render frame. Unit: `server-soak`.
pub fn bot_frame(bot: &mut BotClient, frame: &net::interp::RenderFrame) {
    let snap = &frame.snapshot;
    let render_tick = if frame.render_tick.is_finite() { frame.render_tick.max(0.0) } else { snap.tick as f64 };
    let client_render_tick = render_tick.floor() as u64;
    bot.aim.input_tick += 1;
    let own = snap.vessels.iter().find(|v| v.relevance == Relevance::Own);
    let camera = own.map(|v| v.kin.pose.pos).filter(|p| p.is_finite()).unwrap_or(DVec3::ZERO);
    let mut input = PlayerInput { input_tick: bot.aim.input_tick, client_render_tick, camera_pos: [camera.x, camera.y, camera.z], ..PlayerInput::default() };
    if let (BotPolicy::AimBot { reaction_ms, min_target_kn }, Some(own), Some(gun)) = (bot.policy, own, bot.aim.gun) {
        let now_s = render_tick / sim_core::time::TICK_HZ as f64;
        let reaction_s = f64::from(reaction_ms) / 1000.0;
        let a = &mut bot.aim;
        // A render clock that went back (a new sortie, a rejoin) forgets what it saw.
        if a.seen.back().is_some_and(|(t, _)| *t > now_s) { a.seen.clear(); a.next_fire_s = 0.0; }
        let targets: Vec<(DVec3, DVec3)> = snap.vessels.iter()
            .filter(|v| !matches!(v.relevance, Relevance::Own | Relevance::OwnSide) && !v.flags.intersects(VesselFlags::WRECK | VesselFlags::STRUCK_COLOURS | VesselFlags::SUBMERGED))
            .map(|v| (v.kin.pose.pos, v.kin.lin_vel))
            .filter(|(p, v)| p.is_finite() && v.is_finite())
            .collect();
        a.seen.push_back((now_s, targets));
        // Keep the newest memory at or before the reaction time at the front.
        while a.seen.len() > 1 && a.seen[1].0 <= now_s - reaction_s { a.seen.pop_front(); }
        if !a.held {
            bot.session.send_command(Command::TakeControl { role: ControlRole::Mount(gun.slot), on: true }, client_render_tick);
            a.held = true;
        }
        // The host lays the shot with its own pose when the command arrives: the render delay plus half the
        // round trip after this frame. Carry the hull's attitude there by its angular velocity, as a
        // stabilised sight would, so roll and pitch do not throw the shot's elevation off.
        let stats = bot.session.buffer().stats();
        let rtt_s = f64::from(bot.session.link().rtt_ms) / 1000.0;
        let lag_s = (if stats.delay_s.is_finite() { stats.delay_s } else { 0.0 } + if rtt_s.is_finite() { 0.5 * rtt_s } else { 0.0 }).clamp(0.0, 1.0);
        let k = own.kin;
        let pose = if k.ang_vel.is_finite() && k.lin_vel.is_finite() {
            Pose::new(k.pose.pos + k.lin_vel * lag_s, (sim_core::math::DQuat::from_scaled_axis(k.ang_vel * lag_s) * k.pose.rot).normalize())
        } else { k.pose };
        let stale = a.seen.front().filter(|(t, _)| *t <= now_s - reaction_s + 1e-9);
        if let Some(dir_local) = stale.and_then(|(t_seen, list)| aim_direction(&pose, &gun, list, now_s - t_seen + lag_s, f64::from(min_target_kn) * KN)) {
            input.aim_local = [dir_local.x as f32, dir_local.y as f32, dir_local.z as f32];
            if now_s >= a.next_fire_s {
                bot.session.send_command(Command::Fire { role: ControlRole::Mount(gun.slot), shot: ShotDesignation::Direction { dir_local } }, client_render_tick);
                bot.shots = bot.shots.saturating_add(1);
                a.next_fire_s = now_s + FIRE_INTERVAL_S;
            }
        }
    }
    bot.session.set_input(input);
}

/// The aim bot's shot: the nearest target fast enough and inside [`ENGAGE_SHARE`] of the gun's range, led from
/// where it was seen `age_s` before the shot is laid by its seen velocity over that age plus the shell's
/// flight, as a direction in the frame of `pose` (the hull's attitude when the host lays it) from the muzzle
/// to the sea at that point.
fn aim_direction(pose: &Pose, gun: &GunInfo, seen: &[(DVec3, DVec3)], age_s: f64, min_speed_mps: f64) -> Option<DVec3> {
    let muzzle = pose.to_world(gun.pos_local);
    if !muzzle.is_finite() { return None; }
    let flat = |v: DVec3| DVec3::new(v.x, 0.0, v.z);
    let (p, v) = seen.iter()
        .filter(|(_, v)| flat(*v).length() >= min_speed_mps)
        .map(|(p, v)| (*p, *v, flat(*p - muzzle).length()))
        .filter(|(_, _, r)| *r <= gun.range_m * ENGAGE_SHARE && *r > 1.0)
        .min_by(|a, b| a.2.total_cmp(&b.2))
        .map(|(p, v, _)| (p, v))?;
    let speed = (gun.muzzle_mps * MEAN_SHELL_SPEED).max(1.0);
    let mut aim = p + v * age_s;
    for _ in 0..3 {
        let tof = flat(aim - muzzle).length() / speed;
        aim = p + v * (age_s + tof);
    }
    let point = DVec3::new(aim.x, 0.0, aim.z);
    let dir = (point - muzzle).try_normalize()?;
    let local = (pose.rot.inverse() * dir).try_normalize()?;
    local.is_finite().then_some(local)
}

// ---------------------------------------------------------------------------------------------------------
// Gates.

fn gate(name: &str, pass: bool, measured: f64, limit: f64) -> (String, bool, f64, f64) { (name.to_string(), pass, measured, limit) }

/// Gate verdicts for a finished run under its preset. Unit: `server-soak`.
pub fn evaluate(report: &SoakReport, preset: LinkPreset) -> Vec<(String, bool, f64, f64)> {
    let mut out = Vec::new();
    let joined: Vec<&ClientReport> = report.clients.iter().filter(|c| c.slot != u8::MAX).collect();
    out.push(gate("sortie_ran", report.sorties > 0 && report.ticks > 0 && !joined.is_empty(), report.ticks as f64, 1.0));
    let dropped = joined.iter().filter(|c| c.dropped_at_s.is_some()).count() as f64;
    let bandwidth = matches!(preset, LinkPreset::Clean | LinkPreset::Gate);
    if bandwidth {
        let (frames, ex) = joined.iter().fold((0u64, 0u64), |a, c| (a.0 + c.frames_rendered, a.1 + c.frames_extrapolated));
        let share = if frames > 0 { ex as f64 / frames as f64 } else { 1.0 };
        out.push(gate("extrapolated_frames", frames > 0 && share <= 0.01, share, 0.01));
        let (bytes, updates) = joined.iter().fold((0u64, 0u64), |a, c| (a.0 + c.traffic.vessel_bytes, a.1 + c.traffic.vessel_updates));
        let per = if updates > 0 { bytes as f64 / updates as f64 } else { f64::MAX };
        out.push(gate("vessel_bytes_per_update", updates > 0 && per <= 20.0, if updates > 0 { per } else { 0.0 }, 20.0));
        let live: u64 = joined.iter().map(|c| c.traffic.snapshot_bytes + c.traffic.event_bytes + c.traffic.projectile_bytes + c.traffic.session_bytes).sum();
        let projectile: u64 = joined.iter().map(|c| c.traffic.projectile_bytes).sum();
        let pshare = if live > 0 { projectile as f64 / live as f64 } else { 0.0 };
        out.push(gate("projectile_share", live > 0 && pshare <= 0.25, pshare, 0.25));
        let p95 = joined.iter().map(|c| f64::from(c.traffic.p95_kbps)).fold(0.0, f64::max);
        out.push(gate("client_p95_kbps", live > 0 && p95 <= 256.0, p95, 256.0));
    }
    if preset == LinkPreset::Gate {
        let corr = joined.iter().map(|c| c.max_own_correction_m).fold(0.0, f64::max);
        out.push(gate("own_hull_correction_m", corr <= 0.5, corr, 0.5));
        if let Some(base) = report.baseline_hit_rate {
            let (shots, hits) = joined.iter().fold((0u32, 0u32), |a, c| (a.0 + c.shots, a.1 + c.hits));
            let rate = if shots > 0 { hits as f64 / shots as f64 } else { 0.0 };
            let enough = shots >= MIN_AIM_SHOTS && report.baseline_shots >= MIN_AIM_SHOTS && base > 0.0;
            let ratio = if base > 0.0 { rate / base } else { 0.0 };
            out.push(gate("manual_aim_hit_ratio", enough && ratio >= 0.9 - 1e-9, ratio, 0.9));
        }
    }
    match preset {
        LinkPreset::Clean | LinkPreset::Gate | LinkPreset::Tolerated | LinkPreset::Upload1Mbps => out.push(gate("nobody_dropped", dropped == 0.0, dropped, 0.0)),
        LinkPreset::DropPing | LinkPreset::DropLoss => {
            let limit_end = report.minutes * 60.0 + JOIN_TIMEOUT_S;
            let worst = joined.iter().map(|c| c.dropped_at_s.unwrap_or(f64::INFINITY)).fold(0.0, f64::max);
            let measured = if worst.is_finite() { worst } else { limit_end };
            out.push(gate("dropped_within_s", !joined.is_empty() && worst <= 20.0, measured, 20.0));
        }
    }
    if preset == LinkPreset::Upload1Mbps {
        let s = report.loss_settled_s;
        out.push(gate("upload_loss_under_5pct_s", s.is_some_and(|s| s <= 20.0), s.unwrap_or(report.minutes * 60.0), 20.0));
    }
    if report.synthetic_tick_ms > 0.0 || report.real_load {
        out.push(gate("backlog_ticks", report.max_backlog_ticks <= 30, f64::from(report.max_backlog_ticks), 30.0));
        let worst = report.backlog_recovery_s.iter().copied().fold(0.0, f64::max);
        out.push(gate("backlog_recovery_s", worst <= 5.0, worst, 5.0));
        let gap = down_then_up_gap(&report.ladder_steps);
        out.push(gate("down_then_up_gap_s", gap.is_none_or(|g| g >= 30.0), gap.unwrap_or(f64::from(u32::MAX)), 30.0));
        if report.load_dropped_at_s.is_some() {
            let r = report.rung0_after_drop_s;
            out.push(gate("rung0_after_load_drop_s", r.is_some_and(|r| r <= 30.0), r.unwrap_or(report.minutes * 60.0), 30.0));
        }
    }
    out
}

/// Shortest time from a step down to the next step up, if any step down is followed by one.
fn down_then_up_gap(steps: &[(f64, u8, u8, bool)]) -> Option<f64> {
    let mut best: Option<f64> = None;
    for (i, s) in steps.iter().enumerate() {
        if s.3 { continue; }
        if let Some(up) = steps[i + 1..].iter().find(|n| n.3) {
            let g = up.0 - s.0;
            best = Some(best.map_or(g, |b: f64| b.min(g)));
        }
    }
    best
}

/// Seconds (from `from_s`) after which every later sample stays under `limit` for `hold_s`; `None` if never.
fn settled_after(samples: &[(f64, f64)], from_s: f64, limit: f64, hold_s: f64) -> Option<f64> {
    let s: Vec<&(f64, f64)> = samples.iter().filter(|(t, _)| *t >= from_s).collect();
    for (i, (t, v)) in s.iter().map(|p| (p.0, p.1)).enumerate() {
        if v >= limit { continue; }
        let window: Vec<&&(f64, f64)> = s[i..].iter().take_while(|(u, _)| *u <= t + hold_s).collect();
        let covers = s.last().is_some_and(|l| l.0 >= t + hold_s);
        if covers && window.iter().all(|(_, w)| *w < limit) { return Some(t - from_s); }
    }
    None
}

/// p95 and p99 (nearest rank) of tick times in ms.
fn tick_percentiles(ms: &mut [f32]) -> (f32, f32) {
    if ms.is_empty() { return (0.0, 0.0); }
    ms.sort_by(f32::total_cmp);
    let n = ms.len();
    let rank = |p: f64| ms[((p * n as f64).ceil() as usize).clamp(1, n) - 1];
    (rank(0.95), rank(0.99))
}

// ---------------------------------------------------------------------------------------------------------
// The run.

/// The soak's clock: hand-stepped, or real time.
enum RunClock { Manual(ManualClock), System(Arc<SystemClock>) }
impl RunClock {
    fn shared(&self) -> Arc<dyn Clock> { match self { RunClock::Manual(c) => Arc::new(c.clone()), RunClock::System(c) => c.clone() } }
    fn now(&self) -> Duration { match self { RunClock::Manual(c) => c.now(), RunClock::System(c) => c.now() } }
    /// Move time on by one step: at once on the manual clock, by waiting on the system clock.
    fn step(&self, to: Duration) {
        match self {
            RunClock::Manual(c) => c.set(to),
            RunClock::System(c) => {
                let now = c.now();
                if to > now { std::thread::sleep(to - now); }
            }
        }
    }
}

/// One bot's measurement state.
#[derive(Default)]
struct Meter {
    slot: Option<PlayerSlot>,
    client_id: u64,
    frames: u64,
    extrapolated: u64,
    max_correction: f64,
    /// When this sortie's first frame reached the bot (sim tick of the sortie start marks a new sortie).
    first_frame_at: Option<Duration>,
    last_own: Option<(f64, DVec3, DVec3)>,
    dropped_at: Option<f64>,
    /// When the bot was welcomed (session time), and its connection attempts so far.
    joined_at: Option<Duration>,
    attempts: usize,
    ended: bool,
    warnings: u32,
    traffic: Traffic,
    shots: u32,
    hits: u32,
}

/// Fit the scene's players to the seated bots: bot `k` takes player hull `k`, extra bots get copies of the
/// first player hull further along the line, unseated scene players stay as officer-run hulls.
pub(crate) fn fit_players(config: &mut SortieConfig, seats: &[(PlayerSlot, u64, String)]) {
    let scene: Vec<PlayerInit> = std::mem::take(&mut config.players);
    let hulls: Vec<u16> = scene.iter().filter_map(|p| p.vessel).filter(|&v| (v as usize) < config.vessels.len()).collect();
    let Some(&first) = hulls.first() else {
        // A scene without a player hull seats everyone as a spectator on side 0.
        let side = config.sides.first().map(|s| s.id).unwrap_or_default();
        config.players = seats.iter().map(|(slot, id, name)| PlayerInit { slot: *slot, client_id: *id, name: name.clone(), side, vessel: None, human: true }).collect();
        return;
    };
    let template = config.vessels[first as usize].clone();
    let side = scene.iter().find(|p| p.vessel == Some(first)).map(|p| p.side).unwrap_or(template.side);
    let last = config.vessels[*hulls.last().unwrap_or(&first) as usize].pos;
    let across = Pose::from_heading(DVec3::ZERO, template.heading_rad).rot * DVec3::Z;
    let across = if across.is_finite() && across.length_squared() > 0.0 { across.normalize() } else { DVec3::X };
    let mut players = Vec::with_capacity(seats.len().max(scene.len()));
    let mut extra = 0usize;
    for (k, (slot, id, name)) in seats.iter().enumerate() {
        let vessel = match hulls.get(k) {
            Some(&v) => v,
            None => {
                extra += 1;
                let mut v = template.clone();
                v.pos = last + across * (EXTRA_HULL_SPACING_M * extra as f64);
                v.hull_record = None;
                config.vessels.push(v);
                (config.vessels.len() - 1).min(u16::MAX as usize) as u16
            }
        };
        let side = scene.iter().find(|p| p.vessel == Some(vessel)).map_or(side, |p| p.side);
        players.push(PlayerInit { slot: *slot, client_id: *id, name: name.clone(), side, vessel: Some(vessel), human: true });
    }
    // Scene hulls nobody took sail under their officers, in slots after the bots'.
    let mut next = seats.iter().map(|s| s.0 .0).max().map_or(0, |m| m.saturating_add(1));
    for p in scene.iter().skip(seats.len()) {
        players.push(PlayerInit { slot: PlayerSlot(next), client_id: 0, human: false, ..p.clone() });
        next = next.saturating_add(1);
    }
    config.players = players;
}

/// The aim bot's gun on a hull: the non-point-defence gun with the longest effective range.
fn main_gun(server: &ServerSession, slot: PlayerSlot) -> Option<GunInfo> {
    let ship = server.sim()?.ships().of_player(slot)?;
    ship.design.mounts.iter()
        .filter(|m| m.weapon == content::graph::WeaponClass::Gun && !m.point_defence && m.muzzle_mps.is_finite() && m.muzzle_mps > 0.0 && m.effective_range_m.is_finite() && m.effective_range_m > 0.0)
        .max_by(|a, b| a.effective_range_m.total_cmp(&b.effective_range_m).then(b.slot.cmp(&a.slot)))
        .map(|m| GunInfo { slot: m.slot, pos_local: if m.pos_local.is_finite() { m.pos_local } else { DVec3::ZERO }, muzzle_mps: m.muzzle_mps, range_m: m.effective_range_m })
}

/// Bot client ids (the identity the host holds slots by), distinct per bot.
fn bot_client_id(i: u8) -> u64 { 0xB07_0000 + u64::from(i) }

struct Soak<'a> {
    args: &'a SoakArgs,
    make: &'a dyn Fn(u64) -> SortieConfig,
    data: &'a crate::runtime::Data,
    pool: &'a Arc<rayon::ThreadPool>,
    clock: RunClock,
    server: ServerSession,
    bots: Vec<BotClient>,
    meters: Vec<Meter>,
    report: SoakReport,
    tick_ms: Vec<f32>,
    /// Session time of the first sortie start.
    t0: Option<Duration>,
    origin: Duration,
    loss: Vec<(f64, f64)>,
    backlog: Vec<(f64, u32)>,
    ladder: (u8, u8),
    rung0_at: Option<f64>,
    /// Times of time-scale steps (rung 4 entered, stepped within, or left).
    backlog_marks: Vec<f64>,
    /// Synthetic tick cost in force now (dropped to 0 for the last third).
    synthetic_now: f32,
}

impl Soak<'_> {
    fn secs(&self, t: Duration) -> f64 { t.saturating_sub(self.origin).as_secs_f64() }

    /// One loop step: the host, then every bot, then rendering when a frame is due.
    fn step(&mut self, next_frame: &mut Duration) -> Result<(), CliError> {
        let before = self.server.sim().map(|s| s.tick().0);
        let wall = std::time::Instant::now();
        let events = self.server.update()?;
        let elapsed_ms = wall.elapsed().as_secs_f64() * 1000.0;
        if let (Some(b), Some(a)) = (before, self.server.sim().map(|s| s.tick().0)) {
            let n = a.saturating_sub(b);
            if n > 0 {
                let per = (elapsed_ms / n as f64) as f32 + self.synthetic_now;
                for _ in 0..n.min(1_000) { self.tick_ms.push(per); }
                self.report.ticks += n;
            }
        }
        let now = self.clock.now();
        if self.server.phase() == SessionPhase::Sortie {
            let b = self.server.backlog_ticks();
            self.report.max_backlog_ticks = self.report.max_backlog_ticks.max(b);
            if self.backlog.last().is_none_or(|l| l.1 != b) { self.backlog.push((self.secs(now), b)); }
        }
        for e in events { self.host_event(e, now)?; }
        for (bot, meter) in self.bots.iter_mut().zip(self.meters.iter_mut()) {
            if meter.ended { continue; }
            let events = match bot.session.update() { Ok(e) => e, Err(_) => { meter.ended = true; continue; } };
            for e in events {
                match e {
                    ClientEvent::SortieLoading(_) => { meter.first_frame_at = None; meter.last_own = None; }
                    ClientEvent::Ended(_) => meter.ended = true,
                    _ => {}
                }
            }
        }
        if now >= *next_frame {
            *next_frame += FRAME;
            let in_sortie = self.server.phase() == SessionPhase::Sortie;
            for (bot, meter) in self.bots.iter_mut().zip(self.meters.iter_mut()) {
                if meter.ended || meter.slot.is_none() { continue; }
                let Some(frame) = bot.session.sample() else { continue };
                if in_sortie {
                    let first = *meter.first_frame_at.get_or_insert(now);
                    if now.saturating_sub(first).as_secs_f64() >= WARMUP_S {
                        meter.frames += 1;
                        if frame.extrapolating { meter.extrapolated += 1; }
                    }
                    measure_correction(meter, &frame);
                }
                bot_frame(bot, &frame);
            }
        }
        Ok(())
    }

    fn host_event(&mut self, e: HostEvent, now: Duration) -> Result<(), CliError> {
        match e {
            HostEvent::PlayerLeft { client_id, .. } => {
                if let Some(m) = self.meters.iter_mut().find(|m| m.client_id == client_id && m.dropped_at.is_none() && m.joined_at.is_some()) {
                    m.dropped_at = Some(now.saturating_sub(m.joined_at.unwrap_or(now)).as_secs_f64());
                }
            }
            HostEvent::SortieOver => {
                self.close_sortie();
                if self.t0.is_some() && self.remaining(now) > Duration::ZERO { self.open_sortie()?; }
            }
            _ => {}
        }
        Ok(())
    }

    fn remaining(&self, now: Duration) -> Duration {
        let total = Duration::from_secs_f64(self.args.minutes * 60.0);
        match self.t0 { Some(t0) => (t0 + total).saturating_sub(now), None => total }
    }

    /// Build the next sortie for the seated bots and hand it to the host.
    fn open_sortie(&mut self) -> Result<(), CliError> {
        let seats: Vec<(PlayerSlot, u64, String)> = self.bots.iter().zip(&self.meters)
            .filter(|(_, m)| !m.ended && m.dropped_at.is_none())
            .filter_map(|(b, m)| m.slot.map(|s| (s, m.client_id, match b.session.state() { ClientState::Joined(_) => format!("Bot {}", s.0 + 1), _ => String::new() })))
            .collect();
        let seed = self.args.seed.wrapping_add(u64::from(self.report.sorties));
        let mut config = (self.make)(seed);
        fit_players(&mut config, &seats);
        let deps = crate::runtime::sim_deps(self.data);
        let sim = self.pool.install(|| sim_systems::Sim::new(config, deps))?;
        self.server.start_sortie(sim)?;
        self.report.sorties += 1;
        let now = self.clock.now();
        if self.t0.is_none() { self.t0 = Some(now); }
        // Guns for the aim bots, from the host's ship table.
        for (bot, meter) in self.bots.iter_mut().zip(self.meters.iter_mut()) {
            let gun = meter.slot.and_then(|s| main_gun(&self.server, s));
            bot.set_gun(gun);
            meter.first_frame_at = None;
            meter.last_own = None;
        }
        Ok(())
    }

    /// Take the sortie back: count each bot's resolved manual shots.
    fn close_sortie(&mut self) {
        self.refresh_traffic();
        if let Some((sim, _replay)) = self.server.end_sortie() {
            for m in &mut self.meters {
                let Some(slot) = m.slot else { continue };
                for row in sim.shot_log().iter().filter(|r| r.player == slot) {
                    let Some(hit) = row.hit else { continue };
                    m.shots = m.shots.saturating_add(1);
                    if hit { m.hits = m.hits.saturating_add(1); }
                }
            }
        }
    }

    fn refresh_traffic(&mut self) {
        for (slot, t) in self.server.traffic() {
            if let Some(m) = self.meters.iter_mut().find(|m| m.slot == Some(slot)) { m.traffic = t; }
        }
    }

    /// Once a second: traffic, panel loss, and the connection log's warnings and ladder steps.
    fn each_second(&mut self, now: Duration) {
        self.refresh_traffic();
        if self.t0.is_some() {
            let panel = self.server.connection_panel();
            let mut losses: Vec<f64> = panel.players.iter().filter(|p| p.held_s.is_none() && !p.host).map(|p| f64::from(p.loss)).filter(|l| l.is_finite()).collect();
            if !losses.is_empty() {
                losses.sort_by(f64::total_cmp);
                let n = losses.len();
                let median = if n % 2 == 1 { losses[n / 2] } else { 0.5 * (losses[n / 2 - 1] + losses[n / 2]) };
                self.loss.push((self.secs(now), median));
            }
        }
        let log = self.server.log_mut();
        let start = log.flushed.min(log.entries.len());
        let fresh: Vec<net::quality::LogEntry> = log.entries[start..].to_vec();
        let _ = log.take_unflushed_ron();
        for e in fresh {
            let at = (e.at_s - self.origin.as_secs_f64()).max(0.0);
            match e.kind {
                net::quality::LogKind::Warning { client_id, .. } => {
                    if let Some(m) = self.meters.iter_mut().find(|m| m.client_id == client_id) { m.warnings_inc(); }
                }
                net::quality::LogKind::LadderStep { rung, sub_step, up, .. } => {
                    let prev = self.ladder;
                    self.ladder = (rung, sub_step);
                    self.report.ladder_steps.push((at, rung, sub_step, up));
                    if rung == 4 || (!up && prev.0 == 4) { self.backlog_marks.push(at); }
                    if rung == 0 && self.report.load_dropped_at_s.is_some_and(|d| at >= d) && self.rung0_at.is_none() { self.rung0_at = Some(at); }
                }
                _ => {}
            }
        }
    }
}

impl Meter { fn warnings_inc(&mut self) { self.warnings = self.warnings.saturating_add(1); } }

/// gdclient's correction measure for the own hull: the sampled pose against the previous sample carried by its
/// velocity over the render-tick step.
fn measure_correction(meter: &mut Meter, frame: &net::interp::RenderFrame) {
    let Some(own) = frame.snapshot.vessels.iter().find(|v| v.relevance == Relevance::Own) else { meter.last_own = None; return };
    let (pos, vel) = (own.kin.pose.pos, own.kin.lin_vel);
    if !(pos.is_finite() && vel.is_finite() && frame.render_tick.is_finite()) { meter.last_own = None; return; }
    if let Some((tick, p, v)) = meter.last_own {
        let dt = (frame.render_tick - tick) / sim_core::time::TICK_HZ as f64;
        // A step back or a long gap (a stall, a new epoch) is not a correction the filter would blend.
        if (0.0..=0.5).contains(&dt) {
            let jump = (p + v * dt - pos).length();
            if jump.is_finite() { meter.max_correction = meter.max_correction.max(jump); }
        }
    }
    meter.last_own = Some((frame.render_tick, pos, vel));
}

/// Host upload figure that admits `n` bot clients (L1031: floor(0.9 x upload / 256 kbps) clients).
pub(crate) fn upload_for(n: u8) -> u32 { (u32::from(n) * 2_560).div_ceil(9).max(1_000) }

/// The soak lobby: a sandbox lobby for the seed (the scene builder decides the sortie itself).
fn soak_lobby(seed: u64) -> sim_systems::modes::LobbySetup {
    sim_systems::modes::LobbySetup {
        mode: content::enums::GameMode::Sandbox, ruleset: Default::default(), seed, players: Vec::new(), skirmish: None,
        weather: content::weather::WeatherKind::Clear, sea_state: content::weather::SeaState::BEAUFORT_4, time_of_day: content::enums::TimeOfDay::Day, map: None, at_1_0: false,
    }
}

/// Per-link conditioner seeds from the soak seed (splitmix64 finaliser).
fn link_seed(seed: u64, k: u64) -> u64 {
    let mut z = seed ^ k.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Seconds after `mark` until the change-only backlog series is at 3 ticks or fewer (`end - mark` if never).
fn recovery_after(series: &[(f64, u32)], mark: f64, end: f64) -> f64 {
    let at_mark = series.iter().take_while(|(t, _)| *t <= mark).last().map_or(0, |s| s.1);
    if at_mark <= 3 { return 0.0; }
    series.iter().find(|(t, b)| *t > mark && *b <= 3).map_or(end - mark, |(t, _)| t - mark)
}

/// One soak run under `args` (no baseline, no gates).
fn run_once(args: &SoakArgs, make: &dyn Fn(u64) -> SortieConfig, data: &crate::runtime::Data, pool: &Arc<rayon::ThreadPool>) -> Result<SoakReport, CliError> {
    use net::transport::netcode::{memory_pair, NetcodeClientBackend, NetcodeServerBackend, UdpHostOptions};
    use net::transport::{ClientTransport, ServerTransport};
    let n = args.clients;
    let max = content::ruleset::max_players(content::enums::GameMode::Sandbox, false);
    if n == 0 || n > max { return Err(CliError::Config(format!("a soak seats 1 to {max} bot clients, not {n}"))); }
    if !(args.minutes.is_finite() && args.minutes > 0.0) { return Err(CliError::Config("soak minutes must be above 0".into())); }
    let synthetic = if args.synthetic_tick_ms.is_finite() { args.synthetic_tick_ms.max(0.0) } else { 0.0 };
    let real_load = args.udp && synthetic <= 0.0;
    let clock = if real_load { RunClock::System(Arc::new(SystemClock::new())) } else {
        let c = ManualClock::new();
        c.set(Duration::from_secs(5));
        RunClock::Manual(c)
    };
    let shared = clock.shared();
    let origin = clock.now();
    let (host_p, bot_p) = profile(args.link);
    let version = env!("CARGO_PKG_VERSION");
    let mut options = ServerOptions::new(version, content::enums::GameMode::Sandbox);
    options.lobby_name = "soak".into();
    options.dedicated = true;
    options.upload_kbps = upload_for(n);
    options.synthetic_tick_ms = synthetic;
    let mut server = ServerSession::new(options, soak_lobby(args.seed), shared.clone(), pool.clone(), data.catalogue.clone(), Box::new(net::blueprints::MemoryStore::default()));
    let conditioning = |p: ConditionerProfile, k: u64| (!is_clean(&p)).then(|| Conditioning { profile: p, seed: link_seed(args.seed, k), clock: shared.clone() });
    let mut ends: Vec<Box<dyn ClientTransport>> = Vec::with_capacity(n as usize);
    let mut spares: Vec<Box<dyn ClientTransport>> = Vec::new();
    let mut udp_target: Option<(std::net::SocketAddr, net::join::LobbySecret)> = None;
    if args.udp {
        let secret = net::join::LobbySecret::generate();
        let bind: std::net::SocketAddr = (std::net::Ipv4Addr::LOCALHOST, 0).into();
        let host = NetcodeServerBackend::bind_udp(&UdpHostOptions { bind, secret, max_clients: n as usize }, shared.unix_now(), conditioning(host_p, 0))?;
        let addrs = host.local_addrs();
        let addr = addrs.iter().copied().find(|a| a.ip().is_loopback()).or_else(|| addrs.first().map(|a| (std::net::Ipv4Addr::LOCALHOST, a.port()).into()))
            .ok_or_else(|| CliError::Config("the loopback UDP host reports no address".into()))?;
        server.add_transport(Box::new(host));
        udp_target = Some((addr, secret));
        for i in 0..n {
            ends.push(Box::new(NetcodeClientBackend::connect_udp(addr, &secret, bot_client_id(i), shared.unix_now(), conditioning(bot_p, u64::from(i) + 1))?));
        }
    } else {
        // One profile wraps every in-memory socket: the bots' link, with the host's upstream limit.
        let p = ConditionerProfile { upstream_kbps: host_p.upstream_kbps, ..bot_p };
        let (host, mut clients) = memory_pair(u16::from(n) * JOIN_ATTEMPTS as u16, shared.unix_now(), conditioning(p, 0));
        server.add_transport(Box::new(host));
        let rest = clients.split_off((n as usize).min(clients.len()));
        for c in clients { ends.push(Box::new(c)); }
        for c in rest { spares.push(Box::new(c)); }
    }
    let mut bots = Vec::with_capacity(ends.len());
    let mut meters = Vec::with_capacity(ends.len());
    for (i, t) in ends.into_iter().enumerate() {
        let i = i as u8;
        let identity = net::join::ClientIdentity { client_id: bot_client_id(i), name: format!("Bot {}", u32::from(i) + 1) };
        let options = ClientOptions { identity, game_version: version.into(), build: net::msg::BuildKind::default(), interp: net::interp::InterpTuning::default() };
        bots.push(BotClient::new(ClientSession::new(options, t, shared.clone(), data.catalogue.clone(), Box::new(net::blueprints::MemoryStore::default())), policy_for(i)));
        meters.push(Meter { client_id: bot_client_id(i), attempts: 1, ..Meter::default() });
    }
    let report = SoakReport { game_version: version.into(), seed: args.seed, minutes: args.minutes, link: crate::args::link_name(args.link).into(), synthetic_tick_ms: synthetic, real_load, ..SoakReport::default() };
    let mut s = Soak {
        args, make, data, pool, clock, server, bots, meters, report, tick_ms: Vec::new(), t0: None, origin, loss: Vec::new(), backlog: Vec::new(),
        ladder: (0, 0), rung0_at: None, backlog_marks: Vec::new(), synthetic_now: synthetic,
    };

    // Join: every bot welcomed, refused or gone, or the timeout.
    let mut t = origin;
    let mut next_frame = origin;
    let join_end = origin + Duration::from_secs_f64(JOIN_TIMEOUT_S);
    while t < join_end {
        t += STEP;
        s.clock.step(t);
        s.step(&mut next_frame)?;
        let now = s.clock.now();
        for (i, (b, m)) in s.bots.iter_mut().zip(s.meters.iter_mut()).enumerate() {
            if m.slot.is_some() { continue; }
            match b.session.state() {
                ClientState::Joined(w) => { m.slot = Some(w.slot); m.joined_at = Some(now); }
                ClientState::Refused(why) => { eprintln!("soak: bot {:x} refused: {why:?}", m.client_id); m.ended = true; m.attempts = JOIN_ATTEMPTS; }
                ClientState::Ended(why) if m.attempts < JOIN_ATTEMPTS => {
                    // A connection that never completed its handshake (renetcode2 keeps a client whose first
                    // keep-alive was lost waiting until it times out): connect again, as a player would.
                    let fresh: Option<Box<dyn ClientTransport>> = match &udp_target {
                        Some((addr, secret)) => NetcodeClientBackend::connect_udp(*addr, secret, m.client_id, shared.unix_now(), conditioning(bot_p, (i as u64 + 1) * 100 + m.attempts as u64)).ok().map(|t| Box::new(t) as Box<dyn ClientTransport>),
                        None => spares.pop(),
                    };
                    eprintln!("soak: bot {:x} could not join ({why:?}); attempt {} of {JOIN_ATTEMPTS}", m.client_id, m.attempts + 1);
                    m.attempts += 1;
                    if let Some(t) = fresh {
                        let identity = net::join::ClientIdentity { client_id: m.client_id, name: format!("Bot {}", i + 1) };
                        let options = ClientOptions { identity, game_version: version.into(), build: net::msg::BuildKind::default(), interp: net::interp::InterpTuning::default() };
                        b.session = ClientSession::new(options, t, shared.clone(), data.catalogue.clone(), Box::new(net::blueprints::MemoryStore::default()));
                        m.ended = false;
                    } else { m.ended = true; m.attempts = JOIN_ATTEMPTS; }
                }
                ClientState::Ended(why) => { if m.attempts <= JOIN_ATTEMPTS { eprintln!("soak: bot {:x} gave up joining: {why:?}", m.client_id); } m.ended = true; m.attempts = JOIN_ATTEMPTS + 1; }
                ClientState::Connecting => {}
            }
        }
        if s.meters.iter().all(|m| m.slot.is_some() || (m.ended && m.attempts >= JOIN_ATTEMPTS)) { break; }
    }
    if std::env::var_os("BP_SOAK_DEBUG").is_some() {
        for (b, m) in s.bots.iter().zip(&s.meters) { eprintln!("soak: join at {:.3} s: bot {:x} {:?} slot {:?}", s.secs(t), m.client_id, b.session.state(), m.slot); }
    }
    if s.meters.iter().all(|m| m.slot.is_none()) { return Err(CliError::Config(format!("no bot joined the soak host within {JOIN_TIMEOUT_S} s"))); }
    s.open_sortie()?;

    // The soak proper.
    let t0 = s.t0.unwrap_or(t);
    let total = Duration::from_secs_f64(args.minutes * 60.0);
    let drop_load_at = t0 + total.mul_f64(2.0 / 3.0);
    let mut next_second = t + Duration::from_secs(1);
    loop {
        let now = s.clock.now();
        if s.remaining(now) == Duration::ZERO { break; }
        if s.meters.iter().all(|m| m.ended || m.dropped_at.is_some() || m.slot.is_none()) { break; }
        if s.synthetic_now > 0.0 && now >= drop_load_at {
            s.server.set_synthetic_tick_ms(0.0);
            s.synthetic_now = 0.0;
            s.report.load_dropped_at_s = Some(s.secs(now));
        }
        t = if real_load { now.max(t) + STEP } else { t + STEP };
        s.clock.step(t);
        s.step(&mut next_frame)?;
        let now = s.clock.now();
        if now >= next_second {
            next_second += Duration::from_secs(1);
            s.each_second(now);
        }
    }
    let end = s.clock.now();
    s.each_second(end);
    s.close_sortie();

    // Results.
    let end_s = s.secs(end);
    let t0_s = s.secs(t0);
    let mut r = s.report;
    (r.tick_p95_ms, r.tick_p99_ms) = tick_percentiles(&mut s.tick_ms);
    r.backlog_recovery_s = s.backlog_marks.iter().map(|&m| recovery_after(&s.backlog, m, end_s)).collect();
    if let Some(d) = r.load_dropped_at_s {
        let at_drop = r.ladder_steps.iter().take_while(|st| st.0 <= d).last().map_or(0, |st| st.1);
        r.rung0_after_drop_s = if at_drop == 0 { Some(0.0) } else { s.rung0_at.map(|a| a - d) };
    }
    if args.link == LinkPreset::Upload1Mbps { r.loss_settled_s = settled_after(&s.loss, t0_s, 0.05, 10.0); }
    r.clients = s.meters.iter().map(|m| ClientReport {
        slot: m.slot.map_or(u8::MAX, |p| p.0), traffic: m.traffic, frames_rendered: m.frames, frames_extrapolated: m.extrapolated,
        max_own_correction_m: m.max_correction, dropped_at_s: m.dropped_at, warnings: m.warnings, shots: m.shots, hits: m.hits,
    }).collect();
    s.server.shutdown();
    Ok(r)
}

#[cfg(test)]
mod tests;
