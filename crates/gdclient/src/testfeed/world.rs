//! Part of unit `gd-testfeed`. The scripted worlds behind `BpTestFeed`: deterministic generators of
//! `Snapshot`s and `WireEvent`s, one per built-in script.
//!
//! # How a script is built
//! - The player's own hull is real: at `start` a one-hull basin (`editor::splash::splash_config` with the
//!   targets removed) is built as a real `Sim` and stepped a few ticks on a one-thread pool. Its opening events
//!   (`SortieStarted`, `WeatherChanged`, the hull's `EntityRevealed`) and its `ShipStatus` and `VesselView` are
//!   the template every snapshot starts from, so parts, mounts, compartments and the Bridge Board are the
//!   real design's, and the design the renderer draws is cell for cell the one the HUD describes.
//! - Everything else is scripted in closed form from the script time: other hulls on straight runs, tracks
//!   that follow them, missiles on fixed flights, and a cue list (alerts, fires, flooding, officer lines,
//!   track merges and splits, kills, host loss, the end). Nothing steps a `Sim` per tick, so a 20 minute
//!   battle (72,000 ticks) and the 41-hull reference battle cost only the snapshots that are sampled.
//! - The world answers the orders the UI sends: course, speed and station orders steer the own hull (2.5
//!   degrees a second, 0.5 kn a second), the helm takeover reads the 30 Hz input, designation, emission
//!   control, alert acknowledgement and silencing, pause and time compression change what the next snapshot
//!   shows. Every other command is only captured.
//!
//! # Time
//! Two clocks: the link tick (one per `advance`d tick, stamped on snapshots and events) and the script time
//! (what cues and motion follow). A pause stops the script time and keeps the link ticking, so sampled frames
//! keep arriving; time compression runs the script time 2 or 4 times faster.
use crate::seams::{ClientEvent, ConnectionPanel, LinkQuality, PlayerLink, SortieIntro, SuccessionList, Welcome, WireEvent};
use content::enums::{Domain, Emcon, GameMode, IdRung, OfficerRole, Stance, TimeOfDay, TrackGrade};
use content::ids::{ClassId, CompartmentId, ContentHash, PartSlot, PlayerSlot, SideId};
use glam::{DVec2, DVec3};
use sim_core::events::PhysEvent;
use sim_core::munitions::BodyPhase;
use sim_core::vessel::{LossCause, VesselStatus};
use sim_core::{EntityId, Kinematics, Pose};
use sim_systems::ids::{AlertId, TrackId};
use sim_systems::protocol::config::EntityIntro;
use sim_systems::protocol::event::SimEvent;
use sim_systems::protocol::{
    AlertKind, AlertPriority, AlertState, AlertView, BodyView, Command, ControlHolder, ControlRole, Msg, MsgArg, Outcome, Pacing, PlayerInput, Relevance, RungReason, SeekerPhase, ShipStatus,
    Snapshot, SortieSummary, SourceMask, StanceReason, Subsystem, TimeRate, TrackView, UncertaintyShape, VerticalTrend, VesselFlags, VesselView, Viewer,
};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Simulation ticks per second.
pub(crate) const TICK_HZ: u64 = sim_core::TICK_HZ as u64;
/// Snapshots go out every other tick (30 Hz).
const SNAPSHOT_EVERY: u64 = 2;
/// Of a long `advance`, only the snapshots of its last ticks are built: the sampled frame trails the newest
/// snapshot by the interpolation delay (8 ticks), and the buffer wants a few behind that.
const SNAPSHOT_TAIL_TICKS: u64 = 32;
const KN: f64 = 0.514_444;
/// Own-hull handling of the scripted helm.
const TURN_RATE_RAD_S: f64 = 2.5 * core::f64::consts::PI / 180.0;
const ACCEL_KN_S: f64 = 0.5;
const MAX_SPEED_KN: f64 = 32.0;
/// A waypoint is reached inside this distance.
const WAYPOINT_M: f64 = 150.0;
/// Alerts silenced for this long (L1687).
const SILENCE_TICKS: u64 = 30 * TICK_HZ;
const ME: SideId = SideId(0);
const THEM: SideId = SideId(1);
/// Entity numbers of scripted hulls and bodies, clear of the template `Sim`'s own few.
const FIRST_OTHER_ENTITY: u32 = 1_000;
const FIRST_BODY_ENTITY: u32 = 100_000;
/// Missile tracks are numbered from here, so they never meet hull tracks.
const FIRST_MISSILE_TRACK: u32 = 5_000;

/// Every built-in script, in the order `scripts()` lists them.
pub(crate) const SCRIPTS: [&str; 7] = ["calm", "plot_40_tracks", "battle", "alert_flood", "track_events", "coop_host_loss", "reference_battle"];

/// What a script is made of, before the template is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind { Calm, Plot40, Battle, AlertFlood, TrackEvents, CoopHostLoss, ReferenceBattle }
impl Kind {
    pub(crate) fn from_name(name: &str) -> Option<Kind> {
        Some(match name {
            "calm" => Kind::Calm,
            "plot_40_tracks" => Kind::Plot40,
            "battle" => Kind::Battle,
            "alert_flood" => Kind::AlertFlood,
            "track_events" => Kind::TrackEvents,
            "coop_host_loss" => Kind::CoopHostLoss,
            "reference_battle" => Kind::ReferenceBattle,
            _ => return None,
        })
    }
    /// The player's stock hull.
    fn own_hull(self) -> &'static str {
        match self {
            Kind::Battle | Kind::AlertFlood => "starters/corvette40.bp.ron",
            Kind::ReferenceBattle => "starters/destroyer90.bp.ron",
            _ => "starters/fac22.bp.ron",
        }
    }
    /// Sea state code of the basin (0 calm, 3 Beaufort 4).
    fn sea(self) -> u8 { match self { Kind::Calm | Kind::Plot40 | Kind::TrackEvents => 0, _ => 3 } }
    /// The local end is a joined co-op member, not a single player.
    pub(crate) fn is_client(self) -> bool { self == Kind::CoopHostLoss }
    /// Slot of the local player.
    fn slot(self) -> PlayerSlot { if self.is_client() { PlayerSlot(1) } else { PlayerSlot(0) } }
}

/// The parts of a real one-hull `Sim` every snapshot starts from.
pub(crate) struct Template {
    pub(crate) vessel: VesselView,
    pub(crate) ship: ShipStatus,
    /// Opening events of the sortie, as the player's link would carry them.
    pub(crate) opening: Vec<SimEvent>,
    pub(crate) seed: u64,
    /// The own design for the renderer, under the hash its reveal names.
    pub(crate) design: Option<(ContentHash, crate::worker::ReceivedDesign)>,
    pub(crate) packed: Option<content::share::PackedBlueprint>,
}

/// Build the own-hull template from a real `Sim`. `Err` is developer text.
pub(crate) fn build_template(kind: Kind, services: &crate::services::Services) -> Result<Template, String> {
    let seed = 0x5eed_0000_u64 + kind as u64;
    let campaign = services.campaign();
    let catalogue = services.catalogue();
    let bp = content::campaign::load::stock_blueprint(kind.own_hull()).map_err(|e| format!("{}: {e}", kind.own_hull()))?;
    let options = content::settings::SplashOptions { water: content::weather::SeaState(kind.sea()), target: content::settings::SplashTarget::PatrolBoat };
    let mut config = sim_systems::editor::splash::splash_config(&bp, options, seed, &campaign);
    // One hull: the player's. Targets, their groups and the basin rules go.
    config.vessels.truncate(1);
    config.groups.clear();
    config.bodies.clear();
    config.intel = Default::default();
    config.mode = GameMode::Sandbox;
    config.rules = sim_systems::protocol::config::ModeRules::Sandbox;
    config.splash = None;
    config.time_of_day = TimeOfDay::Dusk;
    let slot = kind.slot();
    for p in &mut config.players { p.slot = slot; }
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).thread_name(|_| "bp-testfeed".into()).build().map_err(|e| e.to_string())?;
    let deps = services.sim_deps();
    let (snapshot, events) = pool.install(move || -> Result<(Snapshot, Vec<SimEvent>), String> {
        let mut sim = sim_systems::Sim::new(config, deps).map_err(|e| e.to_string())?;
        sim.set_host_load(sim_systems::lod::HostLoad::IDLE);
        let mut events = Vec::new();
        for _ in 0..3 {
            sim.step();
            for e in sim.drain_events() { if e.audience.admits(slot, ME) { events.push(e.event); } }
        }
        Ok((sim.snapshot(Viewer::Player(slot)), events))
    })?;
    let ship = snapshot.ship.clone().ok_or("the template hull has no ship status")?;
    let vessel = snapshot.vessels.iter().find(|v| v.entity == ship.vessel).cloned().ok_or("the template hull is not in its own snapshot")?;
    let opening: Vec<SimEvent> = events.into_iter().filter(|e| matches!(e, SimEvent::SortieStarted { .. } | SimEvent::EntityRevealed(_) | SimEvent::Phys(PhysEvent::WeatherChanged { .. }))).collect();
    // The design for drawing, keyed by the hash the reveal names.
    let reveal_hash = opening.iter().find_map(|e| match e { SimEvent::EntityRevealed(i) if i.entity == ship.vessel => Some(i.blueprint), _ => None });
    let prefab = Arc::new(sim_core::vessel::build_prefab(&bp, &catalogue));
    let built = Arc::new(sim_systems::ship::ShipDesign::build(&bp, prefab, &catalogue, &sim_systems::editor::splash::basin_ruleset()));
    let hash = reveal_hash.unwrap_or(built.hash);
    let packed = content::share::pack(&content::blueprint::Shareable::Ship(Box::new(bp.clone())));
    let design = Some((hash, crate::worker::ReceivedDesign::Full { design: built, blueprint: Arc::new(bp) }));
    Ok(Template { vessel, ship, opening, seed, design, packed: Some(packed) })
}

/// A deterministic 64-bit mix (splitmix64), for script layouts.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}
/// Uniform in [0, 1) from a seed and an index.
fn unit(seed: u64, i: u64) -> f64 { (mix(seed ^ mix(i)) >> 11) as f64 / (1u64 << 53) as f64 }
/// Unit vector of a compass bearing on the map (glam's libm build, so every platform lays a script out alike).
fn compass(h: f64) -> DVec2 { let v = DVec2::from_angle(h); DVec2::new(v.y, -v.x) }
fn bearing_to(a: DVec2, b: DVec2) -> f64 { let d = b - a; DVec2::new(-d.y, d.x).to_angle().rem_euclid(core::f64::consts::TAU) }
fn wrap_pi(a: f64) -> f64 { (a + core::f64::consts::PI).rem_euclid(core::f64::consts::TAU) - core::f64::consts::PI }
fn s(seconds: f64) -> u64 { (seconds * TICK_HZ as f64).round() as u64 }

/// A hull other than the player's, on a straight run.
#[derive(Clone, Debug)]
pub(crate) struct Other {
    pub(crate) entity: EntityId,
    pub(crate) side: SideId,
    pub(crate) class: Option<ClassId>,
    pub(crate) blueprint: ContentHash,
    pub(crate) start: DVec2,
    pub(crate) heading: f64,
    pub(crate) speed_mps: f64,
    /// Furthest it runs (it stops there).
    pub(crate) run_m: f64,
    pub(crate) track: TrackId,
    pub(crate) revealed: bool,
    /// Script tick it was lost at.
    pub(crate) lost: Option<u64>,
    pub(crate) hidden: bool,
    pub(crate) player: Option<PlayerSlot>,
}
impl Other {
    fn pos(&self, t: u64) -> DVec2 {
        let d = (self.speed_mps * t as f64 / TICK_HZ as f64).min(self.run_m.max(0.0));
        self.start + compass(self.heading) * d
    }
    fn moving(&self, t: u64) -> bool { self.lost.is_none() && self.speed_mps * t as f64 / (TICK_HZ as f64) < self.run_m }
}

/// One track of the side picture.
#[derive(Clone, Debug)]
pub(crate) struct Track {
    pub(crate) view: TrackView,
    /// The hull it follows (index into `others`), else it moves on its own velocity from `pos`.
    pub(crate) follows: Option<usize>,
    /// Offset of the estimate from the truth, metres.
    pub(crate) error: DVec2,
    pub(crate) created: bool,
    pub(crate) dropped: bool,
    /// Script tick the free track's `pos` holds.
    pub(crate) epoch: u64,
    pub(crate) altitude: f64,
}

/// A missile on a fixed flight from a launcher to the own hull's position at launch.
#[derive(Clone, Copy, Debug)]
struct Flight { entity: EntityId, track: TrackId, from: DVec2, to: DVec2, launch: u64, arrive: u64, kind: content::ids::MunitionIdx }

/// What a cue does when its tick comes.
#[derive(Clone, Debug)]
pub(crate) enum Cue {
    Reveal(usize),
    CreateTrack(usize),
    DropTrack(TrackId),
    Merge { from: TrackId, into: TrackId },
    Split { from: TrackId, new: usize },
    Rung { track: TrackId, rung: IdRung, reason: RungReason, class: Option<ClassId> },
    Stance { track: TrackId, stance: Stance, reason: StanceReason },
    Raise { priority: AlertPriority, subsystem: Subsystem, kind: AlertKind, officer: Option<OfficerRole> },
    /// The alert of this kind is rectified.
    Rectify(AlertKind),
    Escalate { from: AlertKind, to: AlertKind, priority: AlertPriority },
    Flood(bool),
    Officer { role: OfficerRole, text: Msg },
    Fire { compartment: CompartmentId, level: u8 },
    Fill { compartment: CompartmentId, fill: u8, breached: bool },
    /// Own guns fire a round each this often, from now until `until`.
    Gunfire { every: u64, until: u64, track: TrackId },
    Lose(usize),
    Hide(usize),
    Session(ClientEvent),
    /// The host goes quiet (co-op): pings climb and the link warns.
    HostDegrades,
    End(Outcome),
}

/// Output of one generated tick range.
#[derive(Default)]
pub(crate) struct Produced {
    pub(crate) events: Vec<WireEvent>,
    pub(crate) session: Vec<ClientEvent>,
    pub(crate) snapshots: Vec<Snapshot>,
    /// The script's session ended (host loss): the link reports it ended.
    pub(crate) ended: Option<crate::seams::SessionEnd>,
}

/// A scripted world.
pub(crate) struct World {
    pub(crate) kind: Kind,
    pub(crate) slot: PlayerSlot,
    template: Template,
    /// Link tick of the next tick to generate, and the script time.
    pub(crate) link_tick: u64,
    pub(crate) script_tick: u64,
    /// Fractional script ticks owed under time compression.
    carry: u32,
    // The own hull.
    pos: DVec2,
    ride_y: f64,
    heading: f64,
    speed_kn: f64,
    ordered_heading: f64,
    ordered_speed_kn: f64,
    route: Vec<DVec2>,
    station: Option<DVec2>,
    input: PlayerInput,
    held: Vec<ControlRole>,
    designated: TrackId,
    emcon: Emcon,
    // The rest.
    pub(crate) others: Vec<Other>,
    pub(crate) tracks: Vec<Track>,
    flights: Vec<Flight>,
    flight_cycle: u64,
    cues: Vec<(u64, Cue)>,
    next_cue: usize,
    alerts: Vec<AlertView>,
    next_alert: u32,
    flood_mode: bool,
    fires: BTreeMap<CompartmentId, u8>,
    fills: BTreeMap<CompartmentId, (u8, bool)>,
    gunfire: Option<(u64, u64, TrackId)>,
    pacing: Pacing,
    silenced_until: u64,
    ended: bool,
    allies: Vec<usize>,
    panel_due: u64,
    host_degraded: bool,
    /// Events emitted by orders, delivered with the next tick.
    order_events: Vec<SimEvent>,
}

impl World {
    /// The world for a script, from its template. `campaign` gives the hostile classes.
    pub(crate) fn new(kind: Kind, template: Template, campaign: &content::campaign::CampaignData, catalogue: &content::catalogue::Catalogue) -> World {
        let v = &template.vessel;
        let pos = DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z);
        let heading = v.kin.pose.heading_rad();
        let cruise = match kind { Kind::Calm => 12.0, Kind::ReferenceBattle => 15.0, Kind::CoopHostLoss => 14.0, _ => 18.0 };
        let mut w = World {
            kind, slot: kind.slot(), ride_y: v.kin.pose.pos.y, template, link_tick: 0, script_tick: 0, carry: 0,
            pos, heading, speed_kn: cruise, ordered_heading: heading, ordered_speed_kn: cruise, route: Vec::new(), station: None,
            input: PlayerInput::default(), held: Vec::new(), designated: TrackId::NONE, emcon: Emcon::Active,
            others: Vec::new(), tracks: Vec::new(), flights: Vec::new(), flight_cycle: 0, cues: Vec::new(), next_cue: 0,
            alerts: Vec::new(), next_alert: 1, flood_mode: false, fires: BTreeMap::new(), fills: BTreeMap::new(), gunfire: None,
            pacing: Pacing { time_scale: 1.0, requested: vec![(kind.slot(), TimeRate::X1)], ..Pacing::default() },
            silenced_until: 0, ended: false, allies: Vec::new(), panel_due: 0, host_degraded: false, order_events: Vec::new(),
        };
        w.emcon = w.template.ship.board.emcon;
        match kind {
            Kind::Calm => {}
            Kind::Plot40 => w.script_plot40(),
            Kind::Battle => w.script_battle(campaign),
            Kind::AlertFlood => w.script_alert_flood(campaign),
            Kind::TrackEvents => w.script_track_events(campaign),
            Kind::CoopHostLoss => w.script_coop(),
            Kind::ReferenceBattle => w.script_reference(campaign, catalogue),
        }
        w.cues.sort_by_key(|(t, _)| *t);
        w
    }

    pub(crate) fn own_entity(&self) -> EntityId { self.template.ship.vessel }
    pub(crate) fn seed(&self) -> u64 { self.template.seed }
    pub(crate) fn take_design(&mut self) -> Option<(ContentHash, crate::worker::ReceivedDesign)> { self.template.design.take() }
    pub(crate) fn packed(&self) -> Option<&content::share::PackedBlueprint> { self.template.packed.as_ref() }

    /// What the link says before the first tick: welcome, sortie loading and running.
    pub(crate) fn opening_session(&self) -> Vec<ClientEvent> {
        let mode = GameMode::Sandbox;
        let phase = net::session::SessionPhase::Sortie;
        let welcome = Welcome { protocol: net::WIRE_VERSION, slot: self.slot, mode, lobby_name: if self.kind.is_client() { "Scripted co-op".into() } else { String::new() }, host_game_version: env!("CARGO_PKG_VERSION").to_string(), rejoined: false, phase };
        let own_hash = self.template.design.as_ref().map(|(h, _)| *h);
        let intro = SortieIntro { mode, seed: self.template.seed, viewer: Viewer::Player(self.slot), side: ME, tick: 0, own_side_blueprints: own_hash.into_iter().collect(), operation: String::new() };
        vec![ClientEvent::Welcomed(welcome), ClientEvent::SortieLoading(Box::new(intro)), ClientEvent::SortieRunning { tick: 0 }]
    }

    /// Generate `ticks` ticks. Snapshots only for the last [`SNAPSHOT_TAIL_TICKS`] of them.
    pub(crate) fn advance(&mut self, ticks: u64, out: &mut Produced) {
        let tail_from = self.link_tick + ticks.saturating_sub(SNAPSHOT_TAIL_TICKS);
        for _ in 0..ticks {
            let tick = self.link_tick;
            if tick == 0 {
                for e in self.template.opening.clone() { out.events.push(WireEvent { tick, event: e }); }
                out.events.push(WireEvent { tick, event: SimEvent::PacingChanged(self.pacing.clone()) });
            }
            for e in std::mem::take(&mut self.order_events) { out.events.push(WireEvent { tick, event: e }); }
            if !self.ended {
                let steps = self.script_steps();
                for _ in 0..steps { self.step(out); }
                self.connection_panel(out);
            }
            if tick >= tail_from && tick.is_multiple_of(SNAPSHOT_EVERY) { out.snapshots.push(self.snapshot()); }
            self.link_tick += 1;
        }
    }

    /// Script ticks this link tick runs: none while paused, 2 or 4 under compression.
    fn script_steps(&mut self) -> u32 {
        if self.pacing.paused { return 0; }
        let f = self.pacing.effective.factor() * 10;
        let scaled = (self.pacing.time_scale.clamp(0.1, 1.0) * f as f32).round() as u32;
        self.carry += scaled;
        let steps = self.carry / 10;
        self.carry %= 10;
        steps
    }

    /// One script tick: cues due, the own hull, the scripted motion.
    fn step(&mut self, out: &mut Produced) {
        let t = self.script_tick;
        while self.next_cue < self.cues.len() && self.cues[self.next_cue].0 <= t {
            let cue = self.cues[self.next_cue].1.clone();
            self.next_cue += 1;
            self.run_cue(cue, out);
            if self.ended { return; }
        }
        self.helm();
        self.missiles(t, out);
        if let Some((every, until, track)) = self.gunfire {
            if t >= until { self.gunfire = None; } else if every > 0 && t.is_multiple_of(every) {
                let mounts: Vec<PartSlot> = self.template.ship.mounts.iter().map(|m| m.slot).collect();
                if !mounts.is_empty() {
                    let slot = mounts[((t / every) as usize) % mounts.len()];
                    self.emit(out, SimEvent::Phys(PhysEvent::MountFired { vessel: self.own_entity(), slot, rounds: 1 }));
                    let _ = track;
                }
            }
        }
        self.script_tick += 1;
    }

    fn emit(&self, out: &mut Produced, event: SimEvent) { out.events.push(WireEvent { tick: self.link_tick, event }); }

    /// Steer the own hull one script tick.
    fn helm(&mut self) {
        let dt = 1.0 / TICK_HZ as f64;
        let helm_held = self.held.contains(&ControlRole::Helm);
        if helm_held {
            let throttle = if self.input.throttle.is_finite() { self.input.throttle.clamp(-1.0, 1.0) as f64 } else { 0.0 };
            let rudder = if self.input.rudder.is_finite() { self.input.rudder.clamp(-1.0, 1.0) as f64 } else { 0.0 };
            self.ordered_speed_kn = (throttle * MAX_SPEED_KN).max(-MAX_SPEED_KN * 0.25);
            self.ordered_heading = (self.heading + rudder * TURN_RATE_RAD_S * dt * 2.0).rem_euclid(core::f64::consts::TAU);
        } else if let Some(&wp) = self.route.first() {
            if (wp - self.pos).length() < WAYPOINT_M { self.route.remove(0); }
            if let Some(&wp) = self.route.first() { self.ordered_heading = bearing_to(self.pos, wp); }
        } else if let Some(st) = self.station {
            let d = (st - self.pos).length();
            self.ordered_speed_kn = if d < WAYPOINT_M { 0.0 } else { (d / 100.0).clamp(2.0, 12.0) };
            if d >= WAYPOINT_M { self.ordered_heading = bearing_to(self.pos, st); }
        }
        let turn = wrap_pi(self.ordered_heading - self.heading).clamp(-TURN_RATE_RAD_S * dt, TURN_RATE_RAD_S * dt);
        self.heading = (self.heading + turn).rem_euclid(core::f64::consts::TAU);
        let dv = (self.ordered_speed_kn - self.speed_kn).clamp(-ACCEL_KN_S * dt, ACCEL_KN_S * dt);
        self.speed_kn += dv;
        self.pos += compass(self.heading) * self.speed_kn * KN * dt;
    }

    // ---- Orders ----

    /// Apply an order the UI sent. Returns true when the world changed.
    pub(crate) fn apply(&mut self, command: &Command) {
        match command {
            Command::SetCourse { waypoints, speed_kn, append } => {
                let wps: Vec<DVec2> = waypoints.iter().copied().filter(|p| p.is_finite()).take(64).collect();
                if !*append { self.route.clear(); }
                self.route.extend(wps);
                self.station = None;
                if let Some(s) = speed_kn.filter(|s| s.is_finite()) { self.ordered_speed_kn = s.clamp(0.0, MAX_SPEED_KN); }
                if self.ordered_speed_kn < 1.0 { self.ordered_speed_kn = 12.0; }
            }
            Command::SetSpeed { speed_kn } if speed_kn.is_finite() => self.ordered_speed_kn = speed_kn.clamp(-MAX_SPEED_KN * 0.25, MAX_SPEED_KN),
            Command::HoldStation { at, heading_rad } if at.is_finite() => {
                self.route.clear();
                self.station = Some(*at);
                if let Some(h) = heading_rad.filter(|h| h.is_finite()) { self.ordered_heading = h.rem_euclid(core::f64::consts::TAU); }
            }
            Command::Designate { track } => self.designated = track.unwrap_or(TrackId::NONE),
            Command::TakeControl { role, on } => {
                let had = self.held.contains(role);
                if *on && !had { self.held.push(*role); } else if !*on { self.held.retain(|r| r != role); }
                if had != *on {
                    let holder = if *on { ControlHolder::Player } else { ControlHolder::Officer };
                    self.order_events.push(SimEvent::ControlChanged { vessel: self.own_entity(), role: *role, holder });
                }
                if *role == ControlRole::Helm && !*on { self.ordered_speed_kn = self.speed_kn; self.ordered_heading = self.heading; }
            }
            Command::SetEmcon(e) => self.emcon = *e,
            Command::Acknowledge(id) => {
                let pick = match id {
                    Some(id) => self.alerts.iter().position(|a| a.id == *id && a.state == AlertState::Unacknowledged),
                    None => {
                        let mut best: Option<usize> = None;
                        for (i, a) in self.alerts.iter().enumerate() {
                            if a.state != AlertState::Unacknowledged || a.priority == AlertPriority::Caution { continue; }
                            best = match best { Some(b) if (self.alerts[b].priority, std::cmp::Reverse(self.alerts[b].raised_tick)) >= (a.priority, std::cmp::Reverse(a.raised_tick)) => Some(b), _ => Some(i) };
                        }
                        best
                    }
                };
                if let Some(i) = pick {
                    self.alerts[i].state = AlertState::Acknowledged;
                    self.alerts[i].blink_fast = false;
                    self.order_events.push(SimEvent::AlertChanged { id: self.alerts[i].id, state: AlertState::Acknowledged });
                    self.mark_blink();
                }
            }
            Command::SilenceAlerts => {
                self.silenced_until = self.link_tick + SILENCE_TICKS;
                self.order_events.push(SimEvent::AlertsSilenced { vessel: self.own_entity(), until_tick: self.silenced_until });
            }
            Command::Pause(p) => {
                if self.kind.is_client() { self.order_events.push(SimEvent::CommandRejected { player: self.slot, seq: 0, reason: sim_systems::protocol::RejectReason::SinglePlayerOnly }); return; }
                if self.pacing.paused != *p { self.pacing.paused = *p; self.order_events.push(SimEvent::PacingChanged(self.pacing.clone())); }
            }
            Command::RequestTimeRate(rate) => {
                for r in &mut self.pacing.requested { if r.0 == self.slot { r.1 = *rate; } }
                // Compression is refused while a weapon is inbound (L113).
                let inbound = self.tracks.iter().any(|t| t.created && !t.dropped && t.view.inbound);
                if inbound && *rate != TimeRate::X1 {
                    self.pacing.blocked = Some(sim_systems::protocol::CompressionBlock::WeaponInbound);
                    self.pacing.effective = TimeRate::X1;
                } else {
                    self.pacing.blocked = None;
                    self.pacing.effective = *rate;
                }
                self.order_events.push(SimEvent::PacingChanged(self.pacing.clone()));
            }
            _ => {}
        }
    }

    /// The newest 30 Hz input (helm takeover).
    pub(crate) fn input(&mut self, input: PlayerInput) { if input.input_tick >= self.input.input_tick { self.input = input; } }

    // ---- Cues ----

    fn run_cue(&mut self, cue: Cue, out: &mut Produced) {
        let own = self.own_entity();
        match cue {
            Cue::Reveal(i) => {
                let Some(o) = self.others.get_mut(i) else { return };
                if o.revealed { return; }
                o.revealed = true;
                let intro = EntityIntro { entity: o.entity, side: o.side, blueprint: o.blueprint, exterior_only: false, class: o.class.clone(), name: String::new(), player: o.player, status: VesselStatus::Active, damage: Vec::new(), removed: Vec::new(), parent: None, track: o.track };
                self.emit(out, SimEvent::EntityRevealed(Box::new(intro)));
            }
            Cue::CreateTrack(i) => {
                let t = self.script_tick;
                let Some(tr) = self.tracks.get_mut(i) else { return };
                if tr.created { return; }
                tr.created = true;
                tr.epoch = t;
                let (id, domain) = (tr.view.id, tr.view.domain);
                self.emit(out, SimEvent::TrackCreated { side: ME, track: id, domain });
                if matches!(self.tracks[i].view.stance, Stance::Hostile) && domain == Domain::Surface && self.kind != Kind::ReferenceBattle && self.kind != Kind::Plot40 {
                    self.raise(AlertPriority::Caution, Subsystem::Threat, AlertKind::NewHostileTrack { track: id }, Some(OfficerRole::Sensors), out);
                }
            }
            Cue::DropTrack(id) => {
                if let Some(tr) = self.tracks.iter_mut().find(|t| t.view.id == id && t.created && !t.dropped) {
                    tr.dropped = true;
                    tr.view.last_known = true;
                    self.emit(out, SimEvent::TrackDropped { side: ME, track: id });
                }
            }
            Cue::Merge { from, into } => {
                if let Some(tr) = self.tracks.iter_mut().find(|t| t.view.id == from) { tr.dropped = true; tr.view.last_known = false; tr.created = false; }
                if let Some(tr) = self.tracks.iter_mut().find(|t| t.view.id == into) { tr.view.grade = TrackGrade::Fine; tr.view.sources |= SourceMask::ESM; }
                self.emit(out, SimEvent::TrackMerged { side: ME, from, into });
            }
            Cue::Split { from, new } => {
                let t = self.script_tick;
                let Some(new_id) = self.tracks.get(new).map(|t| t.view.id) else { return };
                if let Some(tr) = self.tracks.get_mut(new) { tr.created = true; tr.epoch = t; }
                self.emit(out, SimEvent::TrackSplit { side: ME, from, new: new_id });
            }
            Cue::Rung { track, rung, reason, class } => {
                if let Some(tr) = self.tracks.iter_mut().find(|t| t.view.id == track) {
                    tr.view.rung = rung;
                    tr.view.rung_reason = reason;
                    tr.view.class = class.clone();
                    tr.view.candidates = match rung { IdRung::Class | IdRung::Exact => 1, IdRung::Type => 3, IdRung::Domain => 12, IdRung::Unknown => 40 };
                }
                self.emit(out, SimEvent::RungChanged { side: ME, track, rung, reason, class });
            }
            Cue::Stance { track, stance, reason } => {
                if let Some(tr) = self.tracks.iter_mut().find(|t| t.view.id == track) { tr.view.stance = stance; tr.view.stance_reason = reason; }
                self.emit(out, SimEvent::StanceChanged { side: ME, track, stance, reason });
            }
            Cue::Raise { priority, subsystem, kind, officer } => self.raise(priority, subsystem, kind, officer, out),
            Cue::Rectify(kind) => {
                let mut changed = Vec::new();
                for a in &mut self.alerts {
                    if a.kind != kind || matches!(a.state, AlertState::Cleared | AlertState::Shelved) { continue; }
                    // Emergencies and Alarms rectified unacknowledged show Cleared; the rest leave the strip.
                    a.state = if a.state == AlertState::Unacknowledged && a.priority >= AlertPriority::Alarm { AlertState::Cleared } else { AlertState::Shelved };
                    a.blink_fast = false;
                    changed.push((a.id, a.state));
                }
                for (id, state) in changed { self.emit(out, SimEvent::AlertChanged { id, state }); }
                self.alerts.retain(|a| a.state != AlertState::Shelved);
                self.mark_blink();
            }
            Cue::Escalate { from, to, priority } => {
                let Some(i) = self.alerts.iter().position(|a| a.kind == from && !matches!(a.state, AlertState::Cleared)) else { return };
                self.alerts[i].kind = to;
                self.alerts[i].priority = priority;
                self.alerts[i].state = AlertState::Unacknowledged;
                self.mark_blink();
                let view = self.alerts[i];
                self.emit(out, SimEvent::AlertEscalated(view));
            }
            Cue::Flood(on) => {
                self.flood_mode = on;
                if on {
                    // Flood mode: Cautions go to the log (L1687).
                    for a in &mut self.alerts { if a.priority == AlertPriority::Caution && a.state == AlertState::Unacknowledged { a.state = AlertState::LogOnly; } }
                }
                self.emit(out, SimEvent::AlertFlood { vessel: own, on });
            }
            Cue::Officer { role, text } => self.emit(out, SimEvent::OfficerMessage { vessel: own, role, text }),
            Cue::Fire { compartment, level } => {
                let was = self.fires.get(&compartment).copied().unwrap_or(0);
                if level == 0 { self.fires.remove(&compartment); } else { self.fires.insert(compartment, level); }
                if was == 0 && level > 0 { self.emit(out, SimEvent::Phys(PhysEvent::FireStarted { vessel: own, compartment, spread: false })); }
            }
            Cue::Fill { compartment, fill, breached } => {
                let was = self.fills.get(&compartment).is_some_and(|f| f.1);
                if fill == 0 && !breached { self.fills.remove(&compartment); } else { self.fills.insert(compartment, (fill, breached)); }
                if breached && !was { self.emit(out, SimEvent::Phys(PhysEvent::Breach { vessel: own, compartment, below_waterline: true })); }
            }
            Cue::Gunfire { every, until, track } => {
                self.gunfire = Some((every, until, track));
                for m in self.template.ship.mounts.clone() {
                    self.emit(out, SimEvent::Engaging { mount: sim_systems::ids::MountRef { vessel: own, slot: m.slot }, track, reason: sim_systems::protocol::FireReason::Doctrine });
                }
            }
            Cue::Lose(i) => {
                let t = self.script_tick;
                let Some(o) = self.others.get_mut(i) else { return };
                if o.lost.is_some() { return; }
                o.lost = Some(t);
                let (vessel, side, track) = (o.entity, o.side, o.track);
                self.emit(out, SimEvent::Phys(PhysEvent::VesselLost { vessel, side, cause: LossCause::Sunk, by: own, tonnage_t: 250.0 }));
                if let Some(tr) = self.tracks.iter_mut().find(|t| t.view.id == track) { tr.dropped = true; tr.view.last_known = true; }
                self.emit(out, SimEvent::TrackDropped { side: ME, track });
                if self.gunfire.is_some_and(|g| g.2 == track) { self.gunfire = None; }
            }
            Cue::Hide(i) => {
                let Some(o) = self.others.get_mut(i) else { return };
                if o.hidden { return; }
                o.hidden = true;
                let entity = o.entity;
                self.emit(out, SimEvent::EntityHidden { entity });
            }
            Cue::Session(e) => {
                if let ClientEvent::Ended(end) = &e { out.ended = Some(*end); self.ended = true; }
                out.session.push(e);
            }
            Cue::HostDegrades => self.host_degraded = true,
            Cue::End(outcome) => {
                let summary = SortieSummary { outcome, winner: if outcome == Outcome::Victory { Some(ME) } else { None }, duration_ticks: self.script_tick, ..Default::default() };
                self.emit(out, SimEvent::SortieEnded(Box::new(summary)));
                self.ended = true;
            }
        }
    }

    /// Raise an alert (honouring flood mode and the budget's silence) and say so.
    fn raise(&mut self, priority: AlertPriority, subsystem: Subsystem, kind: AlertKind, officer: Option<OfficerRole>, out: &mut Produced) {
        let state = if self.flood_mode && priority == AlertPriority::Caution { AlertState::LogOnly } else { AlertState::Unacknowledged };
        let view = AlertView { id: AlertId(self.next_alert), vessel: self.own_entity(), priority, subsystem, kind, state, raised_tick: self.link_tick, officer, blink_fast: false };
        self.next_alert += 1;
        self.alerts.push(view);
        self.mark_blink();
        let view = *self.alerts.last().unwrap_or(&view);
        self.emit(out, SimEvent::AlertRaised(view));
    }

    /// The one newest top unacknowledged Emergency or Alarm blinks fast (L1687).
    fn mark_blink(&mut self) {
        for a in &mut self.alerts { a.blink_fast = false; }
        let top = self.alerts.iter().enumerate()
            .filter(|(_, a)| a.state == AlertState::Unacknowledged && a.priority >= AlertPriority::Alarm)
            .max_by_key(|(_, a)| (a.priority, a.raised_tick)).map(|(i, _)| i);
        if let Some(i) = top { self.alerts[i].blink_fast = true; }
    }

    /// Missiles of the reference battle: launches, bursts, their tracks.
    fn missiles(&mut self, t: u64, out: &mut Produced) {
        if self.flights.is_empty() { return; }
        let cycle_len = self.flights.iter().map(|f| f.arrive).max().unwrap_or(1).max(1) + 1;
        let within = t % cycle_len;
        let cycle = t / cycle_len;
        if cycle != self.flight_cycle { self.flight_cycle = cycle; }
        for f in self.flights.clone() {
            let id = self.flight_entity(&f, cycle);
            if within == f.launch {
                let from = DVec3::new(f.from.x, 15.0, f.from.y);
                let v = self.flight_vel(&f);
                self.emit(out, SimEvent::Phys(PhysEvent::BodySpawned { body: id, kind: f.kind, side: THEM, launcher: EntityId::NONE, slot: PartSlot(0), pos: from, vel: v }));
                self.emit(out, SimEvent::TrackCreated { side: ME, track: self.flight_track(&f, cycle), domain: Domain::Weapon });
            } else if within == f.arrive {
                let to = DVec3::new(f.to.x, 10.0, f.to.y);
                self.emit(out, SimEvent::Phys(PhysEvent::BodyBurst { body: id, pos: to, underwater: false }));
                self.emit(out, SimEvent::TrackDropped { side: ME, track: self.flight_track(&f, cycle) });
            }
        }
    }
    fn flight_entity(&self, f: &Flight, cycle: u64) -> EntityId { EntityId { index: f.entity.index, generation: 1 + (cycle as u32 & 0x7fff) } }
    fn flight_track(&self, f: &Flight, cycle: u64) -> TrackId { TrackId(f.track.0 + (cycle as u32 % 64) * self.flights.len() as u32) }
    fn flight_vel(&self, f: &Flight) -> DVec3 {
        let secs = (f.arrive - f.launch).max(1) as f64 / TICK_HZ as f64;
        let d = (f.to - f.from) / secs;
        DVec3::new(d.x, 0.0, d.y)
    }

    /// The connection panel once a second (every link carries one, the host's own too).
    fn connection_panel(&mut self, out: &mut Produced) {
        if self.link_tick < self.panel_due { return; }
        self.panel_due = self.link_tick + TICK_HZ;
        let me = PlayerLink { slot: self.slot, name: "You".into(), host: !self.kind.is_client(), ping_ms: 0, loss: 0.0, frames_behind: 0, quality: LinkQuality::Good, cap_kbps: 768, warning: None, held_s: None };
        let mut players = vec![me];
        if self.kind.is_client() {
            let (ping, loss, quality) = if self.host_degraded { (900, 0.4, LinkQuality::Poor) } else { (38, 0.0, LinkQuality::Good) };
            players.insert(0, PlayerLink { slot: PlayerSlot(0), name: "Host".into(), host: true, ping_ms: ping, loss, frames_behind: if self.host_degraded { 12 } else { 0 }, quality, cap_kbps: 512, warning: None, held_s: None });
            players[1].ping_ms = 38;
        }
        let panel = ConnectionPanel { players, host_tick_hz: if self.pacing.paused { 0.0 } else { TICK_HZ as f32 }, host_tick_p95_ms: 2.4, saturated: false };
        out.session.push(ClientEvent::Connection(Box::new(panel)));
    }

    // ---- The snapshot ----

    fn snapshot(&self) -> Snapshot {
        let t = self.script_tick;
        let own = self.own_entity();
        let mut vessels = Vec::with_capacity(1 + self.others.len());
        let mut own_view = self.template.vessel.clone();
        own_view.kin = kin(self.pos, self.ride_y, self.heading, self.speed_kn * KN);
        own_view.rudder = (wrap_pi(self.ordered_heading - self.heading) * 4.0).clamp(-1.0, 1.0) as f32;
        own_view.power = (self.speed_kn / MAX_SPEED_KN).clamp(0.0, 1.0) as f32;
        own_view.flags.set(VesselFlags::BURNING, !self.fires.is_empty());
        own_view.flags.set(VesselFlags::FIRED_RECENTLY, self.gunfire.is_some());
        own_view.relevance = Relevance::Own;
        vessels.push(own_view);
        for o in self.others.iter().filter(|o| o.revealed && !o.hidden) {
            let p = o.pos(t);
            let speed = if o.moving(t) { o.speed_mps } else { 0.0 };
            let status = if o.lost.is_some() { VesselStatus::Lost(LossCause::Sunk) } else { VesselStatus::Active };
            let mut flags = VesselFlags::empty();
            if o.lost.is_some() { flags |= VesselFlags::WRECK | VesselFlags::BURNING; }
            let relevance = if o.side == ME { Relevance::OwnSide } else if (p - self.pos).length() < 10_000.0 { Relevance::Near } else { Relevance::Distant };
            vessels.push(VesselView { entity: o.entity, kin: kin(p, 0.0, o.heading, speed), status, flags, turrets: Vec::new(), rudder: 0.0, power: if speed > 0.0 { 0.6 } else { 0.0 }, track: o.track, relevance });
        }
        vessels.sort_by_key(|v| v.entity.index);
        let mut tracks: Vec<TrackView> = self.tracks.iter().filter(|tr| tr.created).map(|tr| self.track_view(tr, t)).collect();
        let mut bodies = Vec::new();
        if !self.flights.is_empty() {
            let cycle_len = self.flights.iter().map(|f| f.arrive).max().unwrap_or(1).max(1) + 1;
            let (within, cycle) = (t % cycle_len, t / cycle_len);
            for f in &self.flights {
                if within < f.launch || within >= f.arrive { continue; }
                let k = (within - f.launch) as f64 / (f.arrive - f.launch).max(1) as f64;
                let p2 = f.from.lerp(f.to, k);
                let pos = DVec3::new(p2.x, 15.0, p2.y);
                let vel = self.flight_vel(f);
                let track = self.flight_track(f, cycle);
                let phase = if k > 0.8 { BodyPhase::Terminal } else { BodyPhase::Cruise };
                bodies.push(BodyView { entity: self.flight_entity(f, cycle), kind: f.kind, side: THEM, pos, vel, phase, seeker: if k > 0.8 { SeekerPhase::Locked } else { SeekerPhase::Midcourse }, track, relevance: Relevance::Engaged });
                tracks.push(TrackView {
                    id: track, domain: Domain::Weapon, stance: Stance::Hostile, stance_reason: StanceReason::Default, rung: IdRung::Type, rung_reason: RungReason::RcsAndSpeed, grade: TrackGrade::Fine,
                    pos, vel, shape: UncertaintyShape::Circle { radius_m: 60.0 }, last_update_tick: self.link_tick, last_known: false, class: None, candidates: 2,
                    sources: SourceMask::SEARCH_RADAR | SourceMask::FIRE_CONTROL, trend: VerticalTrend::Steady, inbound: k > 0.4, engaged_by: 0, envelope_m: 0.0, emitting: k > 0.8, relevance: Relevance::Engaged, struck: false,
                });
            }
            bodies.sort_by_key(|b| b.entity.index);
        }
        tracks.sort_by_key(|t| t.id.0);
        let mut ship = self.template.ship.clone();
        ship.speed_kn = self.speed_kn as f32;
        ship.heading_rad = self.heading as f32;
        ship.ordered_speed_kn = self.ordered_speed_kn as f32;
        ship.ordered_heading_rad = self.ordered_heading as f32;
        ship.route = self.route.clone();
        ship.designated = self.designated;
        ship.held = self.held.clone();
        ship.board.emcon = self.emcon;
        for c in &mut ship.compartments {
            if let Some(level) = self.fires.get(&c.id) { c.fire = *level; }
            if let Some((fill, breached)) = self.fills.get(&c.id) { c.fill = *fill; c.breached = *breached; }
        }
        self.board(&mut ship);
        let damage = self.fires.len() as f32 * 0.01 + self.fills.values().map(|f| f.0 as f32 / 255.0 * 0.02).sum::<f32>();
        ship.cells_lost = (ship.cells_lost + damage).clamp(0.0, 1.0);
        let allies = self.allies.iter().filter_map(|&i| self.others.get(i)).filter(|o| o.revealed).map(|o| sim_systems::protocol::AllyStatus {
            vessel: o.entity, player: o.player, speed_kn: (o.speed_mps / KN) as f32, cells_lost: 0.0, burning: false, flooding: false, emcon: Emcon::Active, officers_in_command: self.host_degraded,
        }).collect();
        let alerts = self.alerts.iter().filter(|a| a.vessel == own).copied().collect();
        Snapshot { tick: self.link_tick, pacing: self.pacing.clone(), vessels, bodies, decoys: Vec::new(), tracks, ship: Some(ship), allies, alerts, objectives: self.template_objectives() }
    }

    fn template_objectives(&self) -> Vec<sim_systems::protocol::ObjectiveView> { Vec::new() }

    /// The Bridge Board lines the scripted damage and gunnery change.
    fn board(&self, ship: &mut ShipStatus) {
        let first_fire = self.fires.iter().next().map(|(c, _)| *c);
        let first_breach = self.fills.iter().find(|(_, f)| f.1).map(|(c, _)| *c);
        for r in &mut ship.board.officers {
            match r.role {
                Some(OfficerRole::DamageControl) => {
                    if let Some(c) = first_fire {
                        r.reason = Msg::new("officer.dc.reason.fire").arg(MsgArg::Compartment(c));
                        r.next = match first_breach { Some(b) => Msg::new("officer.dc.next.pumping").arg(MsgArg::Compartment(b)), None => Msg::new("officer.dc.next.list") };
                    } else if let Some(b) = first_breach {
                        r.reason = Msg::new("officer.dc.reason.breach").arg(MsgArg::Compartment(b));
                        r.next = Msg::new("officer.dc.next.pumping").arg(MsgArg::Compartment(b));
                    }
                }
                Some(OfficerRole::Gunnery) => {
                    if let Some((_, _, track)) = self.gunfire {
                        r.task = Msg::new("officer.gunnery.task.engaging").arg(MsgArg::Track(track));
                        r.next = Msg::new("officer.gunnery.next.continue");
                    }
                }
                _ => {}
            }
            if let Some(role) = r.role {
                let role_ctl = match role { OfficerRole::Helm => Some(ControlRole::Helm), OfficerRole::Sensors => Some(ControlRole::Sensors), _ => None };
                if let Some(rc) = role_ctl { r.holder = if self.held.contains(&rc) { ControlHolder::Player } else { ControlHolder::Officer }; }
            }
        }
    }

    fn track_view(&self, tr: &Track, t: u64) -> TrackView {
        let mut v = tr.view.clone();
        let (p, vel) = match tr.follows.and_then(|i| self.others.get(i)) {
            Some(o) => {
                let lost_at = if tr.dropped { o.lost.unwrap_or(t) } else { t };
                let p = o.pos(lost_at.min(t));
                let speed = if o.moving(t) && !tr.dropped { o.speed_mps } else { 0.0 };
                (p + tr.error, compass(o.heading) * speed)
            }
            None => {
                let dt = if tr.dropped { 0.0 } else { t.saturating_sub(tr.epoch) as f64 / TICK_HZ as f64 };
                let p0 = DVec2::new(tr.view.pos.x, tr.view.pos.z);
                let v2 = DVec2::new(tr.view.vel.x, tr.view.vel.z);
                (p0 + v2 * dt, v2)
            }
        };
        v.pos = DVec3::new(p.x, tr.altitude, p.y);
        v.vel = DVec3::new(vel.x, 0.0, vel.y);
        if !tr.dropped { v.last_update_tick = self.link_tick; }
        v.engaged_by = if self.gunfire.is_some_and(|g| g.2 == v.id) { self.template.ship.mounts.len().min(255) as u8 } else { 0 };
        if let UncertaintyShape::Wedge { apex, bearing_rad, half_angle_rad, near_m, far_m } = v.shape {
            let _ = apex;
            let b = bearing_to(self.pos, p) as f32;
            v.shape = UncertaintyShape::Wedge { apex: self.pos, bearing_rad: if b.is_finite() { b } else { bearing_rad }, half_angle_rad, near_m, far_m };
        }
        let dist = (p - self.pos).length();
        v.relevance = if v.inbound || v.engaged_by > 0 { Relevance::Engaged } else if dist < 10_000.0 { Relevance::Near } else { Relevance::Distant };
        v
    }

    // ---- Scripts ----

    /// Add a hull on a run and its track (created by a cue), returning (other index, track index).
    #[allow(clippy::too_many_arguments)]
    fn add_other(&mut self, side: SideId, class: Option<ClassId>, blueprint: ContentHash, start: DVec2, heading: f64, speed_kn: f64, run_m: f64, track_spec: Option<TrackSpec>) -> (usize, Option<usize>) {
        let i = self.others.len();
        let entity = EntityId { index: FIRST_OTHER_ENTITY + i as u32, generation: 1 };
        let mut track = TrackId::NONE;
        let mut ti = None;
        if let Some(spec) = track_spec {
            let id = TrackId(self.tracks.len() as u32 + 1);
            track = id;
            ti = Some(self.tracks.len());
            let mut tr = spec.build(id, self.seed(), self.pos, start);
            tr.follows = Some(i);
            self.tracks.push(tr);
        }
        self.others.push(Other { entity, side, class, blueprint, start, heading, speed_mps: speed_kn * KN, run_m, track, revealed: false, lost: None, hidden: false, player: None });
        (i, ti)
    }

    /// A free track (no hull behind it).
    fn add_free_track(&mut self, spec: TrackSpec, pos: DVec2, vel: DVec2) -> usize {
        let id = TrackId(self.tracks.len() as u32 + 1);
        let mut tr = spec.build(id, self.seed(), self.pos, pos);
        tr.view.pos = DVec3::new(pos.x, tr.altitude, pos.y);
        tr.view.vel = DVec3::new(vel.x, 0.0, vel.y);
        self.tracks.push(tr);
        self.tracks.len() - 1
    }

    fn compartments(&self) -> Vec<CompartmentId> { self.template.ship.compartments.iter().map(|c| c.id).collect() }

    /// Golden scene 2: 40 tracks of every domain, stance, rung and grade around the own hull.
    fn script_plot40(&mut self) {
        let seed = self.seed();
        let centre = self.pos;
        for i in 0..40u64 {
            let (domain, stance, rung, grade, speed_kn, alt) = match i {
                0..=9 => (Domain::Surface, Stance::Hostile, [IdRung::Class, IdRung::Type, IdRung::Domain][(i % 3) as usize], [TrackGrade::Fine, TrackGrade::Coarse][(i % 2) as usize], 14.0, 0.0),
                10..=17 => (Domain::Surface, Stance::Neutral, IdRung::Type, TrackGrade::Coarse, 11.0, 0.0),
                18..=23 => (Domain::Surface, Stance::Suspect, IdRung::Unknown, TrackGrade::Bearing, 9.0, 0.0),
                24..=29 => (Domain::Air, Stance::Hostile, IdRung::Type, TrackGrade::Fine, 280.0, 1_500.0 + 500.0 * (i % 3) as f64),
                30..=33 => (Domain::Weapon, Stance::Hostile, IdRung::Domain, TrackGrade::Fine, 560.0, 15.0),
                34..=36 => (Domain::Subsurface, Stance::Suspect, IdRung::Domain, TrackGrade::Bearing, 6.0, -40.0),
                _ => (Domain::Surface, Stance::Friend, IdRung::Exact, TrackGrade::Lock, 16.0, 0.0),
            };
            let bearing = core::f64::consts::TAU * unit(seed, i * 3);
            let range = match domain { Domain::Weapon => 6_000.0 + 4_000.0 * unit(seed, i * 3 + 1), _ => 3_000.0 + 22_000.0 * unit(seed, i * 3 + 1) };
            let pos = centre + compass(bearing) * range;
            let course = if domain == Domain::Weapon { bearing_to(pos, centre) } else { core::f64::consts::TAU * unit(seed, i * 3 + 2) };
            let spec = TrackSpec { domain, stance, rung, grade, altitude: alt, inbound: domain == Domain::Weapon, emitting: i % 7 == 0 };
            let ti = self.add_free_track(spec, pos, compass(course) * speed_kn * KN);
            self.cues.push((0, Cue::CreateTrack(ti)));
        }
    }

    /// The scripted 20 minute battle (toast check, alerts of every priority, fire, flooding, officer lines).
    fn script_battle(&mut self, campaign: &content::campaign::CampaignData) {
        let foes = hostile_classes(campaign, 4);
        let centre = self.pos;
        let mut hostile_tracks = Vec::new();
        for (k, (class, hash)) in foes.iter().enumerate() {
            let bearing = self.heading + (k as f64 - 1.5) * 0.35;
            let start = centre + compass(bearing) * (8_000.0 + 900.0 * k as f64);
            let heading = bearing_to(start, centre) + 0.6;
            let spec = TrackSpec { domain: Domain::Surface, stance: Stance::Suspect, rung: IdRung::Domain, grade: TrackGrade::Coarse, altitude: 0.0, inbound: false, emitting: k == 0 };
            let (oi, ti) = self.add_other(THEM, Some(class.clone()), *hash, start, heading, 14.0, 12_000.0, Some(spec));
            let id = TrackId(ti.map_or(0, |t| t as u32 + 1));
            hostile_tracks.push((oi, id, class.clone()));
            self.cues.push((s(2.0 + k as f64 * 3.0), Cue::CreateTrack(ti.unwrap_or(0))));
            self.cues.push((s(15.0 + k as f64 * 4.0), Cue::Stance { track: id, stance: Stance::Hostile, reason: StanceReason::BriefingHostile }));
            self.cues.push((s(25.0 + k as f64 * 5.0), Cue::Rung { track: id, rung: IdRung::Class, reason: RungReason::RcsAndSpeed, class: Some(class.clone()) }));
            self.cues.push((s(40.0 + k as f64 * 6.0), Cue::Reveal(oi)));
        }
        let comps = self.compartments();
        let fire_c = comps.get(comps.len() / 2).copied().unwrap_or(CompartmentId(0));
        let flood_c = comps.get(1).copied().or(comps.first().copied()).unwrap_or(CompartmentId(0));
        let first = hostile_tracks.first().map(|h| h.1).unwrap_or(TrackId::NONE);
        let c = &mut self.cues;
        c.push((s(30.0), Cue::Raise { priority: AlertPriority::Caution, subsystem: Subsystem::Sensors, kind: AlertKind::EsmWarning, officer: Some(OfficerRole::Sensors) }));
        c.push((s(30.0), Cue::Officer { role: OfficerRole::Sensors, text: Msg::new("officer.radio.sensors.esm") }));
        c.push((s(60.0), Cue::Gunfire { every: s(2.5), until: s(1_080.0), track: first }));
        c.push((s(60.0), Cue::Officer { role: OfficerRole::Gunnery, text: Msg::new("officer.radio.gunnery.engaging").arg(MsgArg::Track(first)) }));
        c.push((s(120.0), Cue::Raise { priority: AlertPriority::Warning, subsystem: Subsystem::Power, kind: AlertKind::PowerShortage { shed: 2 }, officer: Some(OfficerRole::Engineer) }));
        c.push((s(120.0), Cue::Officer { role: OfficerRole::Engineer, text: Msg::new("officer.radio.engineer.shedding").arg(MsgArg::Int(2)) }));
        c.push((s(180.0), Cue::Fire { compartment: fire_c, level: 60 }));
        c.push((s(180.0), Cue::Raise { priority: AlertPriority::Alarm, subsystem: Subsystem::Fire, kind: AlertKind::Fire { compartment: fire_c }, officer: Some(OfficerRole::DamageControl) }));
        c.push((s(182.0), Cue::Officer { role: OfficerRole::DamageControl, text: Msg::new("officer.radio.dc.fire").arg(MsgArg::Compartment(fire_c)) }));
        c.push((s(240.0), Cue::Fire { compartment: fire_c, level: 140 }));
        c.push((s(300.0), Cue::Fill { compartment: flood_c, fill: 50, breached: true }));
        c.push((s(300.0), Cue::Raise { priority: AlertPriority::Alarm, subsystem: Subsystem::Flooding, kind: AlertKind::Flooding { compartment: flood_c }, officer: Some(OfficerRole::DamageControl) }));
        c.push((s(302.0), Cue::Officer { role: OfficerRole::DamageControl, text: Msg::new("officer.radio.dc.breach").arg(MsgArg::Compartment(flood_c)) }));
        c.push((s(360.0), Cue::Fill { compartment: flood_c, fill: 120, breached: true }));
        c.push((s(390.0), Cue::Escalate { from: AlertKind::Flooding { compartment: flood_c }, to: AlertKind::FloodingSevere { compartment: flood_c }, priority: AlertPriority::Alarm }));
        c.push((s(420.0), Cue::Raise { priority: AlertPriority::Emergency, subsystem: Subsystem::Stability, kind: AlertKind::SinkingImminent, officer: Some(OfficerRole::DamageControl) }));
        c.push((s(480.0), Cue::Fill { compartment: flood_c, fill: 90, breached: false }));
        c.push((s(500.0), Cue::Rectify(AlertKind::SinkingImminent)));
        c.push((s(540.0), Cue::Fire { compartment: fire_c, level: 40 }));
        c.push((s(600.0), Cue::Fire { compartment: fire_c, level: 0 }));
        c.push((s(600.0), Cue::Rectify(AlertKind::Fire { compartment: fire_c })));
        c.push((s(660.0), Cue::Fill { compartment: flood_c, fill: 20, breached: false }));
        c.push((s(720.0), Cue::Fill { compartment: flood_c, fill: 0, breached: false }));
        c.push((s(720.0), Cue::Rectify(AlertKind::FloodingSevere { compartment: flood_c })));
        c.push((s(780.0), Cue::Rectify(AlertKind::PowerShortage { shed: 2 })));
        c.push((s(790.0), Cue::Officer { role: OfficerRole::Engineer, text: Msg::new("officer.radio.engineer.restored") }));
        for (n, (oi, _, _)) in hostile_tracks.iter().enumerate() {
            let at = 700.0 + 130.0 * n as f64;
            self.cues.push((s(at), Cue::Lose(*oi)));
            self.cues.push((s(at + 90.0), Cue::Hide(*oi)));
            if let Some((_, next, _)) = hostile_tracks.get(n + 1) {
                self.cues.push((s(at + 5.0), Cue::Gunfire { every: s(2.5), until: s(1_150.0), track: *next }));
                self.cues.push((s(at + 6.0), Cue::Officer { role: OfficerRole::Gunnery, text: Msg::new("officer.radio.gunnery.engaging").arg(MsgArg::Track(*next)) }));
            }
        }
        self.cues.push((s(1_200.0), Cue::End(Outcome::Victory)));
    }

    /// More than 10 alerts inside 10 minutes: flood mode on, then off.
    fn script_alert_flood(&mut self, campaign: &content::campaign::CampaignData) {
        let foes = hostile_classes(campaign, 1);
        if let Some((class, hash)) = foes.first() {
            let start = self.pos + compass(self.heading + 0.4) * 7_000.0;
            let spec = TrackSpec { domain: Domain::Surface, stance: Stance::Hostile, rung: IdRung::Class, grade: TrackGrade::Fine, altitude: 0.0, inbound: false, emitting: true };
            let (oi, ti) = self.add_other(THEM, Some(class.clone()), *hash, start, bearing_to(start, self.pos), 12.0, 5_000.0, Some(spec));
            self.cues.push((0, Cue::CreateTrack(ti.unwrap_or(0))));
            self.cues.push((s(1.0), Cue::Reveal(oi)));
        }
        let comps = self.compartments();
        let comp = |k: usize| comps.get(k % comps.len().max(1)).copied().unwrap_or(CompartmentId(0));
        let mounts: Vec<PartSlot> = self.template.ship.mounts.iter().map(|m| m.slot).collect();
        let mount = |k: usize| mounts.get(k % mounts.len().max(1)).copied().unwrap_or(PartSlot(0));
        let list: Vec<(AlertPriority, Subsystem, AlertKind, Option<OfficerRole>)> = vec![
            (AlertPriority::Caution, Subsystem::Fuel, AlertKind::FuelLow { fraction: 0.3 }, Some(OfficerRole::Engineer)),
            (AlertPriority::Warning, Subsystem::Power, AlertKind::PowerShortage { shed: 1 }, Some(OfficerRole::Engineer)),
            (AlertPriority::Caution, Subsystem::Weapons, AlertKind::AmmunitionLow { slot: mount(0) }, Some(OfficerRole::Gunnery)),
            (AlertPriority::Warning, Subsystem::Weapons, AlertKind::PartHeavyDamage { slot: mount(1) }, Some(OfficerRole::Gunnery)),
            (AlertPriority::Caution, Subsystem::Crew, AlertKind::CrewCasualties, Some(OfficerRole::DamageControl)),
            (AlertPriority::Warning, Subsystem::Data, AlertKind::ConduitCut { conduit: 3 }, Some(OfficerRole::Engineer)),
            (AlertPriority::Alarm, Subsystem::Fire, AlertKind::Fire { compartment: comp(2) }, Some(OfficerRole::DamageControl)),
            (AlertPriority::Caution, Subsystem::Sensors, AlertKind::EsmWarning, Some(OfficerRole::Sensors)),
            (AlertPriority::Warning, Subsystem::Steering, AlertKind::PartLocalControl { slot: mount(0) }, Some(OfficerRole::Helm)),
            (AlertPriority::Alarm, Subsystem::Flooding, AlertKind::Flooding { compartment: comp(1) }, Some(OfficerRole::DamageControl)),
            (AlertPriority::Warning, Subsystem::Propulsion, AlertKind::PartDestroyed { slot: mount(1) }, Some(OfficerRole::Engineer)),
            (AlertPriority::Caution, Subsystem::Navigation, AlertKind::ShoalAhead, Some(OfficerRole::Helm)),
            (AlertPriority::Warning, Subsystem::Power, AlertKind::ConduitCut { conduit: 7 }, Some(OfficerRole::Engineer)),
            (AlertPriority::Emergency, Subsystem::Stability, AlertKind::CapsizeImminent, Some(OfficerRole::DamageControl)),
        ];
        for (k, (priority, subsystem, kind, officer)) in list.into_iter().enumerate() {
            self.cues.push((s(5.0 + 8.0 * k as f64), Cue::Raise { priority, subsystem, kind, officer }));
            // The eleventh alert inside 10 minutes starts flood mode.
            if k == 10 { self.cues.push((s(5.0 + 8.0 * k as f64), Cue::Flood(true))); }
        }
        self.cues.push((s(5.0 + 8.0 * 6.0), Cue::Fire { compartment: comp(2), level: 90 }));
        self.cues.push((s(5.0 + 8.0 * 9.0), Cue::Fill { compartment: comp(1), fill: 70, breached: true }));
        self.cues.push((s(200.0), Cue::Rectify(AlertKind::CapsizeImminent)));
        self.cues.push((s(600.0), Cue::Flood(false)));
    }

    /// Create, merge, split, rung and stance changes, and a drop, on cue.
    fn script_track_events(&mut self, campaign: &content::campaign::CampaignData) {
        let foes = hostile_classes(campaign, 3);
        let centre = self.pos;
        let mut hull_tracks = Vec::new();
        for (k, (class, hash)) in foes.iter().enumerate() {
            let start = centre + compass(self.heading + 0.9 * k as f64 - 0.9) * (6_000.0 + 1_500.0 * k as f64);
            let spec = TrackSpec { domain: Domain::Surface, stance: Stance::Neutral, rung: IdRung::Unknown, grade: TrackGrade::Coarse, altitude: 0.0, inbound: false, emitting: false };
            let (_, ti) = self.add_other(THEM, Some(class.clone()), *hash, start, bearing_to(start, centre) + 1.2, 10.0, 20_000.0, Some(spec));
            hull_tracks.push((ti.unwrap_or(0), class.clone()));
        }
        // A bearing-only ghost of the first hull, merged into it later, and a second contact split from it.
        let ghost_pos = centre + compass(self.heading - 0.85) * 6_400.0;
        let ghost = self.add_free_track(TrackSpec { domain: Domain::Surface, stance: Stance::Suspect, rung: IdRung::Unknown, grade: TrackGrade::Bearing, altitude: 0.0, inbound: false, emitting: true }, ghost_pos, DVec2::ZERO);
        let split_pos = centre + compass(self.heading + 0.1) * 7_800.0;
        let split = self.add_free_track(TrackSpec { domain: Domain::Surface, stance: Stance::Neutral, rung: IdRung::Unknown, grade: TrackGrade::Coarse, altitude: 0.0, inbound: false, emitting: false }, split_pos, compass(self.heading + 2.0) * 8.0 * KN);
        let id = |i: usize| TrackId(i as u32 + 1);
        for (k, (ti, _)) in hull_tracks.iter().enumerate() { self.cues.push((s(1.0 + k as f64), Cue::CreateTrack(*ti))); }
        self.cues.push((s(4.0), Cue::CreateTrack(ghost)));
        if let Some((t0, c0)) = hull_tracks.first().cloned() {
            self.cues.push((s(10.0), Cue::Merge { from: id(ghost), into: id(t0) }));
            self.cues.push((s(14.0), Cue::Rung { track: id(t0), rung: IdRung::Domain, reason: RungReason::RcsAndSpeed, class: None }));
            self.cues.push((s(18.0), Cue::Rung { track: id(t0), rung: IdRung::Type, reason: RungReason::EmitterMatch, class: None }));
            self.cues.push((s(22.0), Cue::Rung { track: id(t0), rung: IdRung::Class, reason: RungReason::VisualSighting, class: Some(c0) }));
            self.cues.push((s(26.0), Cue::Stance { track: id(t0), stance: Stance::Hostile, reason: StanceReason::FiredOnOwnSide }));
        }
        if let Some((t1, _)) = hull_tracks.get(1).cloned() {
            self.cues.push((s(16.0), Cue::Split { from: id(t1), new: split }));
            self.cues.push((s(24.0), Cue::Stance { track: id(t1), stance: Stance::Suspect, reason: StanceReason::CombineEmitter }));
        }
        if let Some((t2, _)) = hull_tracks.get(2).cloned() {
            self.cues.push((s(30.0), Cue::DropTrack(id(t2))));
        }
        self.cues.push((s(34.0), Cue::Stance { track: id(split), stance: Stance::Friend, reason: StanceReason::Datalink }));
    }

    /// A joined co-op member whose host goes silent and is then lost.
    fn script_coop(&mut self) {
        // The host's hull, in company.
        let start = self.pos + compass(self.heading + core::f64::consts::FRAC_PI_2) * 600.0;
        let hash = self.template.design.as_ref().map(|d| d.0).unwrap_or_default();
        let (oi, _) = self.add_other(ME, None, hash, start, self.heading, 14.0, 1.0e9, None);
        self.others[oi].player = Some(PlayerSlot(0));
        self.allies.push(oi);
        self.cues.push((0, Cue::Reveal(oi)));
        let me_id = local_client_id_hint();
        let list = SuccessionList {
            entries: vec![net::msg::SuccessionEntry { client_id: me_id.wrapping_add(1), slot: PlayerSlot(2), external: std::net::SocketAddr::from(([192, 0, 2, 10], 27_015)) }],
            next_secret: [7; 10],
            campaign_id: 0x5c21,
        };
        self.cues.push((s(2.0), Cue::Session(ClientEvent::Succession(Box::new(list)))));
        self.cues.push((s(20.0), Cue::HostDegrades));
        self.cues.push((s(30.0), Cue::Session(ClientEvent::HostSilent)));
        self.cues.push((s(40.0), Cue::Session(ClientEvent::Ended(crate::seams::SessionEnd::HostLost))));
    }

    /// The reference battle of the declutter gate: LANTERN and 40 hostile hulls from
    /// `trials::reference_config`, all revealed, with missiles in flight.
    fn script_reference(&mut self, campaign: &content::campaign::CampaignData, catalogue: &content::catalogue::Catalogue) {
        let config = sim_systems::editor::trials::reference_config(sim_systems::editor::trials::ReferenceScene::ReferenceBattle, 1, campaign);
        let Some(me) = config.vessels.first() else { return };
        let origin = DVec2::new(me.pos.x, me.pos.z);
        let shift = self.pos - origin;
        let mut hashes: BTreeMap<String, ContentHash> = BTreeMap::new();
        let mut hulls = Vec::new();
        for v in config.vessels.iter().skip(1) {
            let sim_systems::protocol::config::BlueprintRef::Stock(path) = &v.blueprint else { continue };
            let hash = match hashes.get(path) {
                Some(h) => *h,
                None => {
                    let h = content::campaign::load::stock_blueprint(path).map(|bp| stock_hash(&bp)).unwrap_or_default();
                    hashes.insert(path.clone(), h);
                    h
                }
            };
            let start = DVec2::new(v.pos.x, v.pos.z) + shift;
            let heading = bearing_to(start, self.pos);
            let run = ((start - self.pos).length() - 2_500.0).max(0.0);
            let k = self.others.len() as u64;
            let stance = Stance::Hostile;
            let spec = TrackSpec { domain: Domain::Surface, stance, rung: if k.is_multiple_of(4) { IdRung::Class } else { IdRung::Type }, grade: if k.is_multiple_of(3) { TrackGrade::Fine } else { TrackGrade::Coarse }, altitude: 0.0, inbound: false, emitting: k.is_multiple_of(5) };
            let (oi, ti) = self.add_other(THEM, v.class.clone(), hash, start, heading, v.speed_kn.max(10.0), run, Some(spec));
            if let (Some(ti), Some(class)) = (ti, v.class.clone()) { if self.tracks[ti].view.rung == IdRung::Class { self.tracks[ti].view.class = Some(class); } }
            hulls.push(oi);
            self.cues.push((0, Cue::Reveal(oi)));
            if let Some(ti) = ti { self.cues.push((0, Cue::CreateTrack(ti))); }
        }
        // Missiles: one launch every 2 s from the hulls in turn, at 290 m/s toward the own hull.
        let kind = missile_index(catalogue);
        let launches = 60u64;
        for n in 0..launches {
            let Some(&oi) = hulls.get((n as usize * 7) % hulls.len().max(1)) else { break };
            let from = self.others[oi].pos(0);
            let to = self.pos + compass(self.heading) * (self.speed_kn * KN * (n as f64 * 2.0 + 30.0));
            let secs = ((to - from).length() / 290.0).clamp(5.0, 80.0);
            let launch = s(5.0 + 2.0 * n as f64);
            self.flights.push(Flight { entity: EntityId { index: FIRST_BODY_ENTITY + n as u32, generation: 1 }, track: TrackId(FIRST_MISSILE_TRACK + n as u32), from, to, launch, arrive: launch + s(secs), kind });
        }
    }
}

/// What a track starts as.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TrackSpec { domain: Domain, stance: Stance, rung: IdRung, grade: TrackGrade, altitude: f64, inbound: bool, emitting: bool }
impl TrackSpec {
    fn build(self, id: TrackId, seed: u64, own: DVec2, at: DVec2) -> Track {
        let i = id.0 as u64;
        let range = (at - own).length().max(1.0);
        let err_m = match self.grade { TrackGrade::Lock => 5.0, TrackGrade::Fine => 40.0, TrackGrade::Coarse => 250.0, TrackGrade::Bearing => 0.0 };
        let error = compass(core::f64::consts::TAU * unit(seed, 900 + i)) * err_m * unit(seed, 901 + i);
        let shape = match self.grade {
            TrackGrade::Lock => UncertaintyShape::Circle { radius_m: 25.0 },
            TrackGrade::Fine => UncertaintyShape::Circle { radius_m: 120.0 },
            TrackGrade::Coarse => UncertaintyShape::Ellipse { semi_major_m: (range * 0.06).clamp(300.0, 2_500.0) as f32, semi_minor_m: (range * 0.02).clamp(120.0, 900.0) as f32, bearing_rad: bearing_to(own, at) as f32 },
            TrackGrade::Bearing => UncertaintyShape::Wedge { apex: own, bearing_rad: bearing_to(own, at) as f32, half_angle_rad: 0.04, near_m: (range * 0.5) as f32, far_m: (range * 1.6) as f32 },
        };
        let sources = match (self.domain, self.grade) {
            (_, TrackGrade::Bearing) => SourceMask::ESM,
            (Domain::Subsurface, _) => SourceMask::PASSIVE_SONAR,
            (_, TrackGrade::Lock) => SourceMask::FIRE_CONTROL | SourceMask::SEARCH_RADAR,
            (Domain::Air | Domain::Weapon, _) => SourceMask::SEARCH_RADAR,
            _ => SourceMask::SEARCH_RADAR | if range < 9_000.0 { SourceMask::VISUAL } else { SourceMask::empty() },
        };
        let candidates = match self.rung { IdRung::Class | IdRung::Exact => 1, IdRung::Type => 3, IdRung::Domain => 12, IdRung::Unknown => 40 };
        let trend = if self.domain == Domain::Air { [VerticalTrend::Steady, VerticalTrend::Climbing, VerticalTrend::Diving][(i % 3) as usize] } else { VerticalTrend::Steady };
        let view = TrackView {
            id, domain: self.domain, stance: self.stance, stance_reason: StanceReason::Default, rung: self.rung, rung_reason: RungReason::None, grade: self.grade,
            pos: DVec3::new(at.x, self.altitude, at.y), vel: DVec3::ZERO, shape, last_update_tick: 0, last_known: false, class: None, candidates, sources, trend,
            inbound: self.inbound, engaged_by: 0, envelope_m: if self.stance == Stance::Hostile && self.domain == Domain::Surface { 8_000.0 } else { 0.0 }, emitting: self.emitting, relevance: Relevance::Distant, struck: false,
        };
        Track { view, follows: None, error, created: false, dropped: false, epoch: 0, altitude: self.altitude }
    }
}

fn kin(p: DVec2, y: f64, heading: f64, speed_mps: f64) -> Kinematics {
    let f = compass(heading) * speed_mps;
    Kinematics { pose: Pose::from_heading(DVec3::new(p.x, y, p.y), heading), lin_vel: DVec3::new(f.x, 0.0, f.y), ang_vel: DVec3::ZERO }
}

/// `n` surface combatant classes with stock designs, lowest act first, and their design hashes.
fn hostile_classes(campaign: &content::campaign::CampaignData, n: usize) -> Vec<(ClassId, ContentHash)> {
    use content::campaign::ClassRole as R;
    let mut list: Vec<(&ClassId, &content::campaign::EnemyClassDef)> = campaign.classes.iter()
        .filter(|(_, d)| !d.submersible && !d.blueprint.trim().is_empty() && matches!(d.role, R::PatrolBoat | R::FastAttack | R::Corvette | R::Frigate))
        .collect();
    list.sort_by_key(|(id, d)| (d.act, id.as_str().to_string()));
    let mut out = Vec::new();
    for (id, d) in list {
        if out.len() >= n { break; }
        if let Ok(bp) = content::campaign::load::stock_blueprint(&d.blueprint) { out.push((id.clone(), stock_hash(&bp))); }
    }
    // Fewer classes than asked: repeat the ones found.
    let found = out.len();
    for k in found..n { if found == 0 { break; } out.push(out[k % found].clone()); }
    out
}

/// Index of an anti-ship missile in the catalogue's munitions (`BodyView::kind`).
fn missile_index(catalogue: &content::catalogue::Catalogue) -> content::ids::MunitionIdx {
    let i = catalogue.munitions.values().position(|m| m.kind == content::part::weapons::MunitionKind::AntiShipMissile).unwrap_or(0);
    content::ids::MunitionIdx(i.min(u16::MAX as usize) as u16)
}

/// A stock design's hash as its reveal names it and the session's stock index keys it.
fn stock_hash(bp: &content::blueprint::Blueprint) -> ContentHash { if bp.header.hash.is_empty() { content::blueprint::io::compute_hash(bp) } else { bp.header.hash } }

/// The client id the scripted succession list is built around (any value other than ours puts us second).
fn local_client_id_hint() -> u64 { 0x0b1a_c4e0 }

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A template with no real hull, for logic tests that need no `Sim`.
    pub(crate) fn bare_template() -> Template {
        use sim_systems::protocol::{CompartmentView, MountView, EngageStatus};
        let vessel = VesselView { entity: EntityId { index: 0, generation: 1 }, kin: kin(DVec2::ZERO, 0.0, 0.0, 0.0), status: VesselStatus::Active, flags: VesselFlags::empty(), turrets: Vec::new(), rudder: 0.0, power: 0.0, track: TrackId::NONE, relevance: Relevance::Own };
        let mut ship = ShipStatus { vessel: vessel.entity, ..Default::default() };
        ship.compartments = (0..4).map(|i| CompartmentView { id: CompartmentId(i), fill: 0, fire: 0, breached: false, valve_open: false, sealed: false }).collect();
        ship.mounts = (0..2).map(|i| MountView { slot: PartSlot(i), status: EngageStatus::Idle, holder: ControlHolder::Officer, solution: None, effective_range_m: 8_000.0, local_control: false }).collect();
        Template { vessel, ship, opening: vec![SimEvent::SortieStarted { seed: 1, tick_hz: 60 }], seed: 9, design: None, packed: None }
    }

    fn world(kind: Kind) -> World {
        let campaign = content::campaign::load::campaign();
        World::new(kind, bare_template(), campaign, content::catalogue::catalogue())
    }

    #[test]
    fn scripts_are_deterministic_for_their_ticks() {
        for name in SCRIPTS {
            let kind = Kind::from_name(name).unwrap();
            let run = || { let mut w = world(kind); let mut p = Produced::default(); w.advance(1_800, &mut p); (format!("{:?}", p.events), format!("{:?}", p.snapshots.last())) };
            assert_eq!(run(), run(), "{name}");
        }
        assert!(Kind::from_name("nope").is_none());
    }

    #[test]
    fn plot_has_forty_tracks_and_reference_battle_forty_one_hulls_and_missiles() {
        let mut w = world(Kind::Plot40);
        let mut p = Produced::default();
        w.advance(10, &mut p);
        assert_eq!(p.snapshots.last().unwrap().tracks.len(), 40);
        let mut w = world(Kind::ReferenceBattle);
        let mut p = Produced::default();
        w.advance(s(60.0), &mut p);
        let snap = p.snapshots.last().unwrap();
        assert_eq!(snap.vessels.len(), 41);
        assert!(snap.bodies.len() >= 10, "missiles in flight: {}", snap.bodies.len());
        assert!(snap.tracks.len() >= 50);
    }

    #[test]
    fn the_battle_raises_every_priority_burns_floods_talks_and_ends_at_twenty_minutes() {
        let mut w = world(Kind::Battle);
        let mut p = Produced::default();
        w.advance(s(1_200.0) + 2, &mut p);
        let mut priorities = std::collections::BTreeSet::new();
        let (mut fire, mut breach, mut officer, mut ended) = (false, false, 0, None);
        for e in &p.events {
            match &e.event {
                SimEvent::AlertRaised(a) | SimEvent::AlertEscalated(a) => { priorities.insert(a.priority as u8); }
                SimEvent::Phys(PhysEvent::FireStarted { .. }) => fire = true,
                SimEvent::Phys(PhysEvent::Breach { .. }) => breach = true,
                SimEvent::OfficerMessage { .. } => officer += 1,
                SimEvent::SortieEnded(_) => ended = Some(e.tick),
                _ => {}
            }
        }
        assert_eq!(priorities.len(), 4);
        assert!(fire && breach && officer >= 6);
        assert_eq!(ended, Some(s(1_200.0)));
    }

    #[test]
    fn orders_steer_the_own_hull_and_alerts_acknowledge() {
        let mut w = world(Kind::AlertFlood);
        let mut p = Produced::default();
        w.advance(s(120.0), &mut p);
        let alarm = w.alerts.iter().filter(|a| a.state == AlertState::Unacknowledged && a.priority >= AlertPriority::Warning).count();
        assert!(alarm > 0);
        assert!(p.events.iter().any(|e| matches!(e.event, SimEvent::AlertFlood { on: true, .. })));
        w.apply(&Command::Acknowledge(None));
        let acked = w.alerts.iter().filter(|a| a.state == AlertState::Acknowledged).count();
        assert_eq!(acked, 1);
        // The acknowledged one was the highest priority (Emergency comes last in the list, at 109 s).
        assert!(w.alerts.iter().any(|a| a.state == AlertState::Acknowledged && a.priority == AlertPriority::Emergency));
        // A course order turns the hull toward the waypoint.
        let target = w.pos + DVec2::new(5_000.0, 0.0);
        w.apply(&Command::SetCourse { waypoints: vec![target], speed_kn: Some(20.0), append: false });
        let before = (target - w.pos).length();
        let mut p = Produced::default();
        w.advance(s(90.0), &mut p);
        assert!((target - w.pos).length() < before - 500.0);
        assert!(wrap_pi(bearing_to(w.pos, target) - w.heading).abs() < 0.2);
        // Pause stops the script time but not the link.
        let (lt, st) = (w.link_tick, w.script_tick);
        w.apply(&Command::Pause(true));
        let mut p = Produced::default();
        w.advance(30, &mut p);
        assert_eq!((w.link_tick, w.script_tick), (lt + 30, st));
        assert!(p.events.iter().any(|e| matches!(&e.event, SimEvent::PacingChanged(pc) if pc.paused)));
    }

    #[test]
    fn track_events_merge_split_climb_the_ladder_and_drop() {
        let mut w = world(Kind::TrackEvents);
        let mut p = Produced::default();
        w.advance(s(40.0), &mut p);
        let has = |f: &dyn Fn(&SimEvent) -> bool| p.events.iter().any(|e| f(&e.event));
        assert!(has(&|e| matches!(e, SimEvent::TrackMerged { .. })));
        assert!(has(&|e| matches!(e, SimEvent::TrackSplit { .. })));
        assert!(has(&|e| matches!(e, SimEvent::RungChanged { rung: IdRung::Class, .. })));
        assert!(has(&|e| matches!(e, SimEvent::StanceChanged { stance: Stance::Hostile, .. })));
        assert!(has(&|e| matches!(e, SimEvent::TrackDropped { .. })));
    }

    #[test]
    fn coop_host_loss_goes_silent_then_ends() {
        let mut w = world(Kind::CoopHostLoss);
        let mut p = Produced::default();
        w.advance(s(45.0), &mut p);
        assert!(p.session.iter().any(|e| matches!(e, ClientEvent::Succession(_))));
        assert!(p.session.iter().any(|e| matches!(e, ClientEvent::HostSilent)));
        assert_eq!(p.ended, Some(crate::seams::SessionEnd::HostLost));
        assert!(p.session.iter().filter(|e| matches!(e, ClientEvent::Connection(_))).count() >= 30);
        // A member cannot pause.
        w.apply(&Command::Pause(true));
        assert!(!w.pacing.paused);
    }

    #[test]
    fn the_template_is_a_real_hull_with_its_reveal_and_design() {
        let services = crate::services::services();
        let t0 = std::time::Instant::now();
        let t = build_template(Kind::Calm, &services).expect("template");
        let ms = t0.elapsed().as_millis();
        assert!(!t.ship.compartments.is_empty() && !t.ship.parts.is_empty(), "the real ship status");
        assert_eq!(t.vessel.entity, t.ship.vessel);
        assert!(t.opening.iter().any(|e| matches!(e, SimEvent::SortieStarted { .. })));
        assert!(t.opening.iter().any(|e| matches!(e, SimEvent::Phys(PhysEvent::WeatherChanged { .. }))));
        let reveal = t.opening.iter().find_map(|e| match e { SimEvent::EntityRevealed(i) => Some(i.blueprint), _ => None }).expect("own reveal");
        assert_eq!(t.design.as_ref().map(|d| d.0), Some(reveal));
        assert!(t.packed.is_some());
        let mut w = World::new(Kind::Calm, t, content::campaign::load::campaign(), content::catalogue::catalogue());
        let mut p = Produced::default();
        w.advance(s(30.0), &mut p);
        let snap = p.snapshots.last().unwrap();
        // The 30 s calm capture: no contacts, no alerts.
        assert!(snap.tracks.is_empty() && snap.alerts.is_empty() && snap.vessels.len() == 1);
        assert!(p.events.iter().all(|e| !matches!(e.event, SimEvent::AlertRaised(_))));
        eprintln!("template built in {ms} ms");
    }

    #[test]
    fn long_advances_build_only_the_tail_snapshots() {
        let mut w = world(Kind::Battle);
        let mut p = Produced::default();
        w.advance(s(600.0), &mut p);
        assert_eq!(p.snapshots.len() as u64, SNAPSHOT_TAIL_TICKS / SNAPSHOT_EVERY);
        assert_eq!(p.snapshots.last().unwrap().tick, s(600.0) - 2);
    }
}
