//! Part of unit `sys-sim`: the phases of `Sim::step` after the command phase, in the contract order of
//! `sim/mod.rs`. Every rate is gated on the tick the step starts at, so a phase never sees the counter move
//! under it (`World::step` advances it in phase 11).
//!
//! # Choices where the contract is silent
//! - Graph ticks are 30 Hz per ship, staggered by the parity of the vessel's entity slot, so half the fleet
//!   runs its graph on even ticks and half on odd ones. The 10 Hz stages key on the 100 ms period, not on the
//!   ship's phase, so nothing else changes.
//! - A ship is a script ship while its compiled graph holds a script node that is not greyed out. Script
//!   ships tick serially after the parallel batch. A batch of fewer than 4 ships runs serially too: the
//!   result is the same and the fan-out would cost more than it saves.
//! - Detections reach a side's picture only from carriers on that side's datalink (`LinkPicture`); a hull
//!   cut off from the link reads an empty picture and fights from its own detections (L394).
//! - A track is `emitting` while an ESM bearing on it is at most 1.5 s old (three ESM looks at 2 Hz).
//! - A briefing declaration for a hull reaches a player-side track once the track is identified to Class, or
//!   at once for hulls of civilian and protected sides (they sail a declared lane). AI sides get Hostile for
//!   every contact whose side their `hostile_to` names, as `ident::stance` asks.
//! - One-shot part orders from outside the graph (mine drops, RHIB and drone launches and recoveries, camera
//!   training) are held and re-written after each control pass until sim-core consumes them, for at most
//!   5 s, so an order given on a tick the part cannot act is not lost to the next graph tick.
//! - Reinforcements: when a side's commander calls for them, each of that side's scheduled arrivals comes
//!   forward by half the wait it had left, with at least 60 s of notice and never later than planned.
//! - Scheduled hulls spawn one a tick in config order, and the clearance masks their groups plan with are
//!   built before the first tick, so an arrival costs no long tick. `Reinforcements` is sent once per side
//!   and wave, with the first hull.
//! - A sink-driven preset change recompiles the graph at most once a second per ship.
//! - Alerts and Bridge Boards update at 10 Hz for hulls a player owns (connected or held by officers).
//! - The rewind ring of a basin is filled at the start of a step, before that step's commands: the frame for
//!   tick T is the state tick T - 1 left, and tick 0 has one.
//! - A hull that is lost has its controls cleared on the tick the loss is reported.
//! - Shoal ahead is reported when the charted depth one stopping distance ahead (at least 150 m) is under
//!   the design draft plus 2 m. Grounded holds for 5 s after the last grounding contact.
use super::events::{allied_masks, is_pvp};
use super::{RunState, Sim};
use crate::ai::AiOut;
use crate::alerts::{AlertChange, Condition};
use crate::datalink::{LinkPicture, PartCommand};
use crate::graph::vm::{self, VmBudget};
use crate::graph::{PortValue, StageHost};
use crate::guidance::missile::GuidanceOut;
use crate::ident;
use crate::officers::stages::StageRunner;
use crate::offboard::OffboardOut;
use crate::prelude::*;
use crate::protocol::alert::AlertKind;
use crate::protocol::config::{Faction, GroupOrders, Outcome, SideDef};
use crate::protocol::event::{Audience, Lane, QueryReply, ScriptFault, SimEvent};
use crate::protocol::snapshot::{ActionBlock, ActionState, SeekerPhase, SourceMask};
use crate::script::ScriptHost;
use crate::sensors::{Detection, SenseEnv};
use crate::ship::controls::{build_controls, SideEffects};
use crate::ship::{Ship, ShipView};
use crate::specops::SpecOpsOut;
use crate::tracks::{SidePicture, Track, TrackChange};
use content::campaign::DifficultySliders;
use content::graph::{NodeKind, OfficerStage, SinkKind};
use content::part::weapons::MunitionKind;
use content::part::SensorKind;
use rayon::prelude::*;
use sim_core::events::PhysEvent;
use sim_core::munitions::Steer;
use sim_core::parts::{PartOrder, VesselControls};
use sim_core::time::{GRAPH_HZ, GUIDANCE_HZ, TICK_HZ, TRACK_HZ};
use sim_core::vessel::VesselStatus;

/// Seconds per graph tick.
const GRAPH_DT: f64 = 1.0 / GRAPH_HZ as f64;
/// A ship's own detections are kept this long (the "raw detections" source and local control), ticks.
const OWN_DETECTION_TICKS: u64 = TICK_HZ as u64;
/// A track counts as emitting while its last ESM bearing is at most this old, ticks.
const EMITTING_TICKS: u64 = 90;
/// A one-shot order from outside the graph is retried for this long, ticks.
const ORDER_TTL_TICKS: u64 = 5 * TICK_HZ as u64;
/// A launch ordered by the graph waits this long for its body, ticks.
const LAUNCH_BIND_TICKS: u64 = 4 * TICK_HZ as u64;
/// Called reinforcements never arrive sooner than this after the call, ticks.
const REINFORCE_NOTICE_TICKS: u64 = 60 * TICK_HZ as u64;
/// Shortest gap between two sink-driven preset recompiles of one ship, ticks.
const PRESET_GAP_TICKS: u64 = TICK_HZ as u64;
/// Scheduled hulls spawned per tick.
const ARRIVALS_PER_TICK: usize = 1;
/// Smallest fan-out worth handing to the pool.
const PAR_MIN_SHIPS: usize = 4;
/// Grounded and interrupted-action conditions hold this long after their event, ticks.
const CONDITION_HOLD_TICKS: u64 = 5 * TICK_HZ as u64;
/// Ports one player may scope at once, and the rate their values stream at.
pub(super) const MAX_SCOPES: usize = 16;
/// Output ports of the `BallisticSolver` stage this unit reads (`graph::library::stage_sig`).
const SOLVER_OUT_FIRE: u32 = 1;
const SOLVER_OUT_RESIDUAL: u32 = 6;
const SOLVER_OUT_CONVERGED: u32 = 7;
const SOLVER_OUT_IN_RANGE: u32 = 8;
/// Highest part slot a held order may grow a control list to.
const MAX_ORDER_SLOT: usize = 4_096;
/// A RHIB working a site never closes its own ship nearer than this plus the ship's length (sim-core reads
/// a waypoint within 60 m of the launcher's hull as a recall), metres.
const RHIB_STANDOFF_M: f64 = 80.0;
/// The RHIB is recalled to within this distance before the davit is asked to take it, metres.
const RHIB_RECOVER_M: f64 = 25.0;

/// Owned copies of what a `SimCtx` borrows, so systems can be borrowed mutably beside the context.
pub(super) struct TickEnv { cat: Arc<Catalogue>, weather: WeatherState, difficulty: DifficultySliders, seed: u64, time_of_day: TimeOfDay, layer_depth_m: f64 }
impl TickEnv {
    pub(super) fn of(sim: &Sim) -> Self {
        Self { cat: sim.deps.catalogue.clone(), weather: *sim.world.weather(), difficulty: sim.config.settings.difficulty, seed: sim.config.seed, time_of_day: sim.config.time_of_day, layer_depth_m: sim.config.layer_depth_m }
    }
    pub(super) fn ctx(&self, tick: Tick) -> SimCtx<'_> {
        SimCtx { seed: self.seed, tick, catalogue: &self.cat, tuning: &self.cat.tuning, weather: &self.weather, time_of_day: self.time_of_day, layer_depth_m: self.layer_depth_m, difficulty: &self.difficulty }
    }
}

/// A guided launch the graph ordered, waiting for sim-core to spawn its body.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct LaunchBind { pub launcher: EntityId, pub slot: PartSlot, pub track: TrackId, pub pn_n: Option<f64>, pub tick: u64 }

/// A one-shot part order from outside the graph, re-written until sim-core consumes it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct HeldOrder { pub vessel: EntityId, pub slot: PartSlot, pub order: PartOrder, pub until_tick: u64 }

/// A RHIB in the water for a station action: where it is bound, or that it is coming home.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RhibTask { pub vessel: EntityId, pub slot: PartSlot, pub toward: DVec2, pub recall: bool }

/// What was last reported about one ship, so changes become events exactly once.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ShipReport {
    pub vessel: EntityId,
    pub radiating: Vec<PartSlot>,
    pub esm_warning: bool,
    pub engage: Vec<(PartSlot, EngageStatus)>,
    /// Gun mounts firing on an unconverged solution as of the last graph tick, ascending.
    pub unconverged: Vec<PartSlot>,
    pub preset_tick: Option<u64>,
    pub grounded_tick: Option<u64>,
    pub interrupted_tick: Option<u64>,
}

fn alive(status: VesselStatus) -> bool { matches!(status, VesselStatus::Active | VesselStatus::Disabled) }

/// DECISION (r3): the briefing's rendezvous (L797) is met when a player-side hull comes within this distance
/// of the escorted hull. No meet distance exists in the content data; the gate's stand-in Helm closes to
/// 1,000 m (`theatre::abstract_combat`), inside this.
pub const RENDEZVOUS_M: f64 = 1_500.0;

/// A player-side hull (`players`, world x and z) is within [`RENDEZVOUS_M`] of any escorted hull (`members`).
pub(crate) fn rendezvous_met(members: &[DVec2], players: &[DVec2]) -> bool {
    members.iter().any(|m| players.iter().any(|p| m.is_finite() && p.is_finite() && m.distance_squared(*p) <= RENDEZVOUS_M * RENDEZVOUS_M))
}

/// Indices of the player ships (`(player owned, side, designated track)` per ship) whose designation is a
/// track of `vessel` on their side's picture. The officers stop firing on a hull that strikes (ruling 1); a
/// player who designates it again afterwards is firing on purpose, so the Strike handler clears each one
/// once, at the strike.
pub(crate) fn designations_on(ships: &[(bool, SideId, TrackId)], pictures: &crate::tracks::Pictures, vessel: EntityId) -> Vec<usize> {
    ships.iter().enumerate().filter(|(_, (player, _, d))| *player && !d.is_none())
        .filter(|(_, (_, side, d))| pictures.side(*side).is_some_and(|p| p.tracks.iter().any(|t| t.id == *d && t.truth == vessel)))
        .map(|(i, _)| i).collect()
}
/// The vessel `id` names, if it is still that vessel: `World::vessel` looks up by slot, and a slot freed by a
/// despawned wreck can hold another hull (a split piece) under a newer generation. A ship whose hull is gone
/// must never read, or steer, the hull that took its slot.
pub(super) fn hull(world: &World, id: EntityId) -> Option<&sim_core::vessel::Vessel> { world.vessel(id).filter(|v| v.id == id) }
fn graph_phase(vessel: EntityId) -> u64 { (vessel.index & 1) as u64 }
fn graph_due(vessel: EntityId, t: Tick) -> bool { t.is_due(GRAPH_HZ, graph_phase(vessel)) }
fn is_link_friend(t: &Track) -> bool { t.stance == Stance::Friend && t.stance_reason == StanceReason::Datalink && t.sources.contains(SourceMask::DATALINK) }

/// A ship whose graph holds a script node that may still run.
fn has_live_scripts(ship: &Ship) -> bool {
    ship.graph.scripts.iter().any(|&i| ship.graph.nodes.get(i as usize).is_some_and(|n| !ship.graph_state.greyed.iter().any(|(id, _)| *id == n.id)))
}

/// What every graph tick of one sim tick reads.
struct Shared<'a> {
    ctx: &'a SimCtx<'a>,
    world: &'a World,
    pictures: &'a crate::tracks::Pictures,
    links: &'a [LinkPicture],
    /// Live hulls (position, velocity) of each side and its allies, by side index.
    hulls: &'a [Vec<(DVec3, DVec3)>],
    /// Briefing ESM sites, for player sides only.
    esm_sites: &'a [[f64; 2]],
    player_sides: SideMask,
    empty: &'a [SidePicture],
    /// What a side with no picture slot reads.
    fallback: &'a SidePicture,
    budget: VmBudget,
}
impl<'a> Shared<'a> {
    /// The picture a ship reads: its side's, or an empty one while it is cut off from the datalink.
    fn picture(&self, vessel: EntityId, side: SideId) -> &'a SidePicture {
        let i = side.index();
        let isolated = self.links.get(i).is_some_and(|l| l.isolated.binary_search(&vessel).is_ok());
        match self.pictures.side(side) {
            Some(p) if !isolated => p,
            _ => self.empty.get(i).unwrap_or(self.fallback),
        }
    }
    fn view(&self, ship: &'a Ship, vessel: &'a sim_core::vessel::Vessel) -> ShipView<'a> {
        let side = ship.side;
        ShipView {
            ctx: self.ctx, world: self.world, vessel, design: &ship.design, settings: &ship.settings, orders: &ship.orders, doctrine: &ship.doctrine,
            picture: self.picture(ship.vessel, side), detections: &ship.detections, part_command: &ship.part_command, control: &ship.control,
            own_side_hulls: self.hulls.get(side.index()).map(|v| v.as_slice()).unwrap_or(&[]),
            esm_sites: if self.player_sides.has(side) { self.esm_sites } else { &[] },
            proficiency: ship.proficiency,
        }
    }
}

/// One ship's rebuilt Bridge Board with the radio lines its change calls for: (ship index, board, lines).
type BoardUpdate = (usize, crate::protocol::snapshot::BridgeBoard, Vec<(OfficerRole, Msg)>);

/// Work and faults of one ship's graph tick.
#[derive(Default)]
struct GraphOut { node_evals: u32, luau_interrupts: u32, gun_iterations: u32, engagement_pairs: u32, faults: Vec<(GraphNodeId, ScriptFault)>, not_reproducible: bool }

/// One ship's graph tick: sources and stages through the `StageRunner`, sinks into `Ship::sinks`.
fn graph_tick(ship: &mut Ship, sh: &Shared<'_>, scripts: Option<&mut dyn ScriptHost>) -> GraphOut {
    let Some(vessel) = hull(sh.world, ship.vessel) else { return GraphOut::default() };
    let side = ship.side;
    let picture = sh.picture(ship.vessel, side);
    let Ship { design, settings, orders, doctrine, control, stages, graph, graph_state, detections, part_command, proficiency, sinks, .. } = ship;
    let view = ShipView {
        ctx: sh.ctx, world: sh.world, vessel, design, settings, orders, doctrine, picture, detections, part_command, control,
        own_side_hulls: sh.hulls.get(side.index()).map(|v| v.as_slice()).unwrap_or(&[]),
        esm_sites: if sh.player_sides.has(side) { sh.esm_sites } else { &[] },
        proficiency: *proficiency,
    };
    let mut runner = StageRunner::new(view, stages);
    let res = vm::run_tick(vessel.id, graph, graph_state, &mut runner as &mut dyn StageHost, scripts, sh.budget, GRAPH_DT);
    let out = GraphOut { node_evals: res.node_evals, luau_interrupts: res.luau_interrupts, gun_iterations: runner.gun_iterations, engagement_pairs: runner.engagement_pairs, faults: res.faults, not_reproducible: res.not_reproducible };
    *sinks = res.sinks;
    out
}

/// The events special operations read from the previous tick, without the bulk they never look at.
fn keep_for_specops(events: &[PhysEvent]) -> Vec<PhysEvent> {
    events.iter().filter_map(|e| match e {
        PhysEvent::CellsDamaged { vessel, cause, attacker, .. } => Some(PhysEvent::CellsDamaged { vessel: *vessel, changes: Vec::new(), cause: *cause, attacker: *attacker }),
        PhysEvent::ShellImpact { .. } | PhysEvent::BodyImpact { .. } | PhysEvent::Splash { .. } | PhysEvent::VesselLost { .. } | PhysEvent::MineLaid { .. } | PhysEvent::BodySpawned { .. }
        | PhysEvent::BodyDestroyed { .. } | PhysEvent::BodyRecovered { .. } | PhysEvent::StatusChanged { .. } => Some(e.clone()),
        _ => None,
    }).collect()
}

impl RunState {
    pub(super) fn report_mut(&mut self, vessel: EntityId) -> &mut ShipReport {
        let i = match self.reports.binary_search_by(|r| r.vessel.cmp(&vessel)) {
            Ok(i) => i,
            Err(i) => { self.reports.insert(i, ShipReport { vessel, ..Default::default() }); i }
        };
        &mut self.reports[i]
    }
    /// Hold a one-shot order for a part, replacing an older one for the same part.
    fn hold_order(&mut self, vessel: EntityId, slot: PartSlot, order: PartOrder, tick: u64) {
        let h = HeldOrder { vessel, slot, order, until_tick: tick.saturating_add(ORDER_TTL_TICKS) };
        match self.orders.iter_mut().find(|o| o.vessel == vessel && o.slot == slot) { Some(o) => *o = h, None => self.orders.push(h) }
    }
}

impl Sim {
    /// Phases 2 to 14 of one tick (the caller has run the command phase and checked the pause).
    pub(super) fn run_tick(&mut self) {
        let t = self.world.tick();
        let mut work = crate::ctx::SystemsWork::default();
        // Wall-clock spans for the tick gate's report only (`sim_core::spans`); nothing reads them back.
        use sim_core::spans::Span;
        let mut lap = sim_core::spans::Lap::start();
        self.phase_schedule(t);
        self.phase_load(t);
        let env = TickEnv::of(self);
        lap.mark(Span::Schedule);
        self.phase_sense(t, &env, &mut work);
        lap.mark(Span::Sensors);
        if t.is_due(TRACK_HZ, 0) { self.phase_tracks(t, &env, &mut work); }
        lap.mark(Span::Tracks);
        self.phase_ai(t, &env, &mut work);
        lap.mark(Span::Ai);
        self.phase_control(t, &env, &mut work);
        lap.mark(Span::Control);
        if t.is_due(GUIDANCE_HZ, 0) { self.phase_guidance(t, &env, &mut work); }
        lap.mark(Span::Guidance);
        self.phase_specops(t, &env);
        self.apply_held_orders();
        lap.mark(Span::SpecOps);
        // World::step times its own sections.
        self.world.step();
        lap.skip();
        self.phase_events(t);
        self.phase_report(t);
        lap.mark(Span::Events);
        self.work = work;
    }

    // ---- Phase 2: schedule ----

    fn phase_schedule(&mut self, t: Tick) {
        if !self.run.arrivals_ready {
            self.run.arrivals_ready = true;
            self.prepare_arrivals();
        }
        if !self.schedule.vessels.is_empty() {
            // One hull a tick, in config order: a group arrives over a few ticks instead of in one long one.
            let due: Vec<u16> = self.schedule.vessels.iter().copied().filter(|&i| {
                let planned = self.config.vessels.get(i as usize).map(|v| v.arrives_s as u64 * TICK_HZ as u64).unwrap_or(0);
                let hastened = self.run.hastened.iter().find(|h| h.0 == i).map(|h| h.1).unwrap_or(u64::MAX);
                planned.min(hastened) <= t.0
            }).take(ARRIVALS_PER_TICK).collect();
            for i in due {
                match self.spawn_configured(i) {
                    Ok(_) => {
                        // Announced once per side and wave: when its first hull arrives.
                        let Some(v) = self.config.vessels.get(i as usize) else { continue };
                        let (side, at) = (v.side, DVec3::new(v.pos.x, 0.0, v.pos.z));
                        let wave = v.arrives_s;
                        let first = !self.schedule.spawned.keys().any(|k| *k != i && self.config.vessels.get(*k as usize).is_some_and(|o| o.side == side && o.arrives_s == wave));
                        if first { self.emit(Audience::side(side), Lane::Reliable, SimEvent::Reinforcements { side, at }); }
                    }
                    // A hull that cannot be built never will be: drop it from the schedule instead of retrying.
                    Err(_) => self.schedule.vessels.retain(|x| *x != i),
                }
                self.run.hastened.retain(|h| h.0 != i);
            }
        }
        if !self.schedule.bodies.is_empty() {
            let due: Vec<u16> = self.schedule.bodies.iter().copied().filter(|&i| self.config.bodies.get(i as usize).map(|b| b.arrives_s as u64 * TICK_HZ as u64).unwrap_or(0) <= t.0).collect();
            for i in due { if self.spawn_configured_body(i).is_err() { self.schedule.bodies.retain(|x| *x != i); } }
        }
        if !self.schedule.weather.is_empty() {
            let due: Vec<u16> = self.schedule.weather.iter().copied().filter(|&i| self.config.weather_changes.get(i as usize).map(|w| w.0 as u64 * TICK_HZ as u64).unwrap_or(0) <= t.0).collect();
            // Config order: when two turns are due at once the later entry is the weather in force.
            for i in due {
                if let Some((_, w)) = self.config.weather_changes.get(i as usize).copied() { self.world.set_weather(w); }
                self.schedule.weather.retain(|x| *x != i);
            }
        }
    }

    /// Before the first tick: build the clearance masks the groups of scheduled arrivals will plan with, so
    /// their first route request costs a search and not a mask build in the middle of a fight.
    fn prepare_arrivals(&mut self) {
        if self.schedule.vessels.is_empty() || self.ai.planner.grid.depth.is_none() { return; }
        let mut groups: Vec<(GroupId, SideId)> = self.schedule.vessels.iter().filter_map(|&i| self.config.vessels.get(i as usize)).filter_map(|v| v.group.map(|g| (g, v.side))).collect();
        groups.sort_unstable();
        groups.dedup();
        for (group, side) in groups {
            let refs: Vec<crate::protocol::config::BlueprintRef> = self.config.vessels.iter().filter(|v| v.group == Some(group) && v.side == side).map(|v| v.blueprint.clone()).collect();
            let mut members: Vec<crate::ai::group::Member> = Vec::new();
            for r in refs {
                let Ok(design) = self.design_for(&r) else { continue };
                let turn = design.turn;
                let pos_or = |v: f64, d: f64| if v.is_finite() && v > 0.0 { v } else { d };
                members.push(crate::ai::group::Member {
                    vessel: EntityId::NONE, pos: DVec2::ZERO, heading: 0.0, active: true, top_speed_mps: pos_or(turn.top_speed_mps, 0.0), turn_radius_m: pos_or(turn.turn_radius_m, 200.0),
                    beam_m: pos_or(turn.beam_m, 6.0), draft_m: pos_or(turn.draft_m, 3.0), engage_m: 0.0, ace: false, under_fire: false,
                });
            }
            if members.is_empty() { continue; }
            let (beam, draft) = crate::ai::group::mask_numbers(&members);
            self.ai.planner.ensure_mask(beam, draft);
        }
    }

    // ---- Phase 3: load ----

    fn phase_load(&mut self, t: Tick) {
        let tuning = self.deps.ladder;
        if let Some(up) = crate::lod::step_ladder(&mut self.ladder, self.host_load, t.0, &tuning) {
            let (rung, sub_step) = (self.ladder.rung, self.ladder.sub_step);
            self.emit(Audience::All, Lane::Reliable, SimEvent::LadderStep { rung, sub_step, up });
        }
        let player_sides = self.player_sides();
        for (vessel, fidelity) in crate::lod::assign_fidelity(&self.world, &self.ships, &self.pictures, player_sides, &self.ladder, &tuning) { self.world.set_fidelity(vessel, fidelity); }
        self.world.set_projectile_cap_rung(self.ladder.projectile_cap_rung());
        self.update_pacing(player_sides);
    }

    /// Recompute the effective rate and tell every client when pacing changed.
    pub(super) fn update_pacing(&mut self, player_sides: SideMask) {
        let humans: Vec<PlayerSlot> = self.players.iter().filter(|p| p.human && p.connected).map(|p| p.slot).collect();
        // The safety test only matters while somebody asks for compression.
        let wanted = self.time.requested.iter().any(|(_, r)| r.factor() > 1);
        let safety = if wanted { crate::timeflow::safety_test(&self.world, &self.ships, &self.pictures, player_sides) } else { Ok(()) };
        crate::timeflow::update(&mut self.time, self.config.mode, &humans, safety, &self.ladder);
        self.note_pacing();
    }

    /// Emit `PacingChanged` when what the HUD shows differs from what was last sent. The measured tick time is
    /// left out of the comparison: it moves every tick and rides in snapshots.
    pub(super) fn note_pacing(&mut self) {
        let mut p = self.pacing();
        if !p.host_tick_ms.is_finite() { p.host_tick_ms = 0.0; }
        let sent = crate::protocol::snapshot::Pacing { host_tick_ms: 0.0, ..p.clone() };
        if self.run.pacing.as_ref() == Some(&sent) { return; }
        // A sortie that starts at plain 1x has nothing to announce on its first tick.
        let first = self.run.pacing.is_none();
        let plain = !sent.paused && sent.effective.factor() == 1 && sent.blocked.is_none() && sent.rung == 0 && sent.sub_step == 0 && sent.requested.iter().all(|r| r.1.factor() == 1);
        self.run.pacing = Some(sent);
        if first && plain { return; }
        self.emit(Audience::All, Lane::Reliable, SimEvent::PacingChanged(p));
    }

    pub(super) fn player_sides(&self) -> SideMask {
        self.config.sides.iter().filter(|s| s.faction == Faction::Players).fold(SideMask::NONE, |m, s| m.with(s.id))
    }

    // ---- Phases 4 and 5: signature and detection ----

    fn phase_sense(&mut self, t: Tick, env: &TickEnv, work: &mut crate::ctx::SystemsWork) {
        let ctx = env.ctx(t);
        self.sigs.update(&self.world, &self.ships, &ctx);
        // Command mode of every part from its links; a hull whose consoles are gone commands nothing.
        for ship in &mut self.ships.ships {
            let Some(v) = hull(&self.world, ship.vessel) else { continue };
            let command = crate::datalink::has_command(v, &env.cat);
            ship.part_command.resize(v.parts.len(), PartCommand::Networked);
            for (i, p) in v.parts.iter().enumerate() {
                let c = crate::datalink::part_command(&p.links, p.works(), &ctx.tuning.networks);
                ship.part_command[i] = match c { PartCommand::Networked if !command => PartCommand::Local { error_factor: ctx.tuning.networks.local_control_error_factor }, c => c };
            }
        }
        self.sensors.refresh(&self.world, &self.ships, &ctx);
        let mut found: Vec<Detection> = Vec::new();
        {
            let sense = SenseEnv { ctx: &ctx, world: &self.world, clutter: crate::sensors::clutter_for(&env.weather, &ctx.tuning.sensors), night: env.time_of_day == TimeOfDay::Night };
            self.sensors.step(&self.sigs, &sense, self.deps.budgets.sensor_pairs, self.ladder.rung >= 2, &mut found);
        }
        work.sensor_pairs = self.sensors.pairs_last_tick;
        work.detections = found.len().min(u32::MAX as usize) as u32;
        // Each ship keeps a second of its own detections, ascending (sensor, tick).
        let floor = t.0.saturating_sub(OWN_DETECTION_TICKS);
        for ship in &mut self.ships.ships {
            let before = ship.detections.len();
            ship.detections.retain(|d| d.tick >= floor);
            let vessel = ship.vessel;
            let mine = found.iter().filter(|d| d.sensor.carrier == vessel);
            let mut added = false;
            for d in mine { ship.detections.push(*d); added = true; }
            if added || ship.detections.len() != before { ship.detections.sort_by_key(|d| (d.sensor, d.tick, d.target)); }
        }
        self.detections.extend(found);
    }

    // ---- Phase 6: tracks, identification and stance, per side ----

    fn phase_tracks(&mut self, t: Tick, env: &TickEnv, work: &mut crate::ctx::SystemsWork) {
        let ctx = env.ctx(t);
        let tick = t.0;
        let allied = allied_masks(&self.config);
        self.detections.sort_by_key(|d| (d.sensor, d.target, d.tick));
        let index_of: BTreeMap<EntityId, u16> = self.schedule.spawned.iter().map(|(i, e)| (*e, *i)).collect();
        let hulls = self.side_hulls(&allied);
        if self.links.len() < self.pictures.sides.len() { self.links.resize(self.pictures.sides.len(), LinkPicture::default()); }
        let mut events: Vec<(SideId, SimEvent)> = Vec::new();
        let mut updates = 0u32;
        // Fusion touches only its own side's picture, so the sides fuse in parallel; everything after it runs
        // serially in side order below.
        let fused: Vec<Option<_>> = {
            let (world, config, detections, tuning) = (&self.world, &self.config, &self.detections, ctx.tuning);
            let allied = &allied;
            self.pictures.sides.par_iter_mut().enumerate().map(|(si, picture)| {
                let side = SideId(si as u8);
                config.sides.iter().find(|s| s.id == side)?;
                let mask = allied.get(si).copied().unwrap_or(SideMask::only(side));
                let link = crate::datalink::membership(world, side, mask);
                let friends = crate::datalink::friend_reports(world, &link);
                let dets: Vec<Detection> = detections.iter().filter(|d| mask.has(d.side) && link.is_member(d.sensor.carrier)).copied().collect();
                let mut changes: Vec<TrackChange> = Vec::new();
                let n = crate::tracks::fuse_core(picture, &dets, &friends, tick, tuning, &mut changes);
                Some((link, dets, changes, n))
            }).collect()
        };
        for (si, fused) in fused.into_iter().enumerate() {
            let side = SideId(si as u8);
            let Some((link, dets, changes, n)) = fused else { continue };
            let Some(def) = self.config.sides.iter().find(|s| s.id == side) else { continue };
            let picture = &mut self.pictures.sides[si];
            updates = updates.saturating_add(n);

            // Evidence by observed entity, in detection order.
            let mut by_target: BTreeMap<EntityId, Vec<Detection>> = BTreeMap::new();
            for d in &dets { by_target.entry(d.target).or_default().push(*d); }
            let players = def.faction == Faction::Players;
            let own_hulls: &[(DVec3, DVec3)] = hulls.get(si).map(|v| v.as_slice()).unwrap_or(&[]);
            // Mounts of this side's hulls afloat engaging each track.
            let mut engaged: BTreeMap<TrackId, u8> = BTreeMap::new();
            for s in self.ships.ships.iter().filter(|s| s.side == side && hull(&self.world, s.vessel).is_some_and(|v| alive(v.status))) {
                for m in &s.stages.engage.mounts {
                    if let EngageStatus::Engaging { track, .. } = m.status { if !track.is_none() { let n = engaged.entry(track).or_insert(0); *n = n.saturating_add(1); } }
                }
            }
            let facts_base = ident::stance::StanceFacts { viewer: def, sides: &self.config.sides, intel: &self.config.intel, lib: &self.ident, declared: None };
            let world = &self.world;
            let lib = &self.ident;
            let truth = |e: EntityId| ident::truth_candidate(world, e, lib);
            for track in picture.tracks.iter_mut().filter(|tr| tr.is_live()) {
                let friend = is_link_friend(track);
                let seen = by_target.get(&track.truth);
                // A contact seen this update shows whether it is still in the fight: colours struck, dead in the
                // water with no working propulsion, or going down (DECISION, k2: the officers stop engaging it;
                // the player's designation still can). Cleared when it is seen in the fight again.
                if seen.is_some() && !friend {
                    if let Some(v) = world.vessel(track.truth) {
                        track.out_of_action = v.struck || !matches!(v.status, sim_core::vessel::VesselStatus::Active);
                        track.struck = v.struck;
                    }
                }
                if let (Some(ds), false) = (seen, friend) {
                    if let Some((rung, reason)) = ident::apply_evidence_with(track, ds, lib, &ctx, &truth) {
                        events.push((side, SimEvent::RungChanged { side, track: track.id, rung, reason, class: track.class.clone() }));
                    }
                }
                let index = index_of.get(&track.truth).copied();
                let init = index.and_then(|i| self.config.vessels.get(i as usize));
                if !friend {
                    // A hull the briefing names is known exactly from its first association (L475).
                    if players && track.rung < IdRung::Exact && seen.is_some() && index.is_some_and(|i| self.config.intel.named.contains(&i)) {
                        let class = init.and_then(|v| v.class.clone());
                        ident::set_exact(track, class, RungReason::Briefing, lib, tick);
                        if track.rung == IdRung::Exact { events.push((side, SimEvent::RungChanged { side, track: track.id, rung: track.rung, reason: track.rung_reason, class: track.class.clone() })); }
                    }
                    let truth_side = world.locate(track.truth).map(|l| l.2);
                    let declared = if players {
                        let by_lane = truth_side.and_then(|s| self.config.sides.iter().find(|d| d.id == s)).is_some_and(|d| matches!(d.faction, Faction::Civilian | Faction::Protected));
                        init.and_then(|v| v.declared).filter(|_| track.rung >= IdRung::Class || by_lane)
                    } else {
                        // An AI side's briefing: its enemies are Hostile, and civilian traffic and protected
                        // hulls it is not at war with are Neutral, so it never auto-engages them (L490).
                        let truth_def = truth_side.and_then(|s| self.config.sides.iter().find(|d| d.id == s));
                        // A civilian or protected side (a defector) is at war with nobody: a side hostile to it
                        // is Hostile, every other side Neutral, so it never fires on its own escort as Suspect
                        // (DECISION, r2c).
                        match truth_def {
                            Some(d) if def.hostile_to.has(d.id) => Some(Stance::Hostile),
                            Some(d) if matches!(d.faction, Faction::Civilian | Faction::Protected) => Some(Stance::Neutral),
                            Some(d) if matches!(def.faction, Faction::Civilian | Faction::Protected) => Some(if d.hostile_to.has(side) { Stance::Hostile } else { Stance::Neutral }),
                            _ => None,
                        }
                    };
                    let facts = ident::stance::StanceFacts { declared, ..facts_base };
                    if ident::stance::evaluate(track, &facts) {
                        events.push((side, SimEvent::StanceChanged { side, track: track.id, stance: track.stance, reason: track.stance_reason }));
                    }
                }
                // Engagement bookkeeping the plot and the captains read.
                // Inbound is a statement about weapons and aircraft (L486); a hull on a closing course is not one.
                track.inbound = !friend && matches!(track.domain, Domain::Weapon | Domain::Air) && crate::engage::inbound(track, own_hulls, tick, ctx.tuning).is_some();
                track.engaged_by = engaged.get(&track.id).copied().unwrap_or(0);
                if seen.is_some_and(|ds| ds.iter().any(|d| d.kind == SensorKind::Esm)) {
                    match self.run.esm_heard.iter_mut().find(|h| h.0 == side && h.1 == track.id) { Some(h) => h.2 = tick, None => self.run.esm_heard.push((side, track.id, tick)) }
                }
                track.emitting = !friend && self.run.esm_heard.iter().any(|h| h.0 == side && h.1 == track.id && tick.saturating_sub(h.2) <= EMITTING_TICKS);
            }
            for ch in &changes {
                let ev = match *ch {
                    TrackChange::Created(id) => picture.get(id).map(|tr| SimEvent::TrackCreated { side, track: id, domain: tr.domain }),
                    TrackChange::Dropped(id) => Some(SimEvent::TrackDropped { side, track: id }),
                    TrackChange::Merged { from, into } => Some(SimEvent::TrackMerged { side, from, into }),
                    TrackChange::Split { from, new } => Some(SimEvent::TrackSplit { side, from, new }),
                    TrackChange::Expired(_) | TrackChange::Grade(..) => None,
                };
                if let Some(ev) = ev { events.push((side, ev)); }
            }
            if !changes.is_empty() {
                for s in self.ships.ships.iter_mut().filter(|s| s.side == side) {
                    if let Some(c) = s.captain.as_mut() { crate::ai::unit::perceive(c, &changes, false, tick); }
                }
            }
            if let Some(slot) = self.links.get_mut(si) { *slot = link; }
        }
        self.run.esm_heard.retain(|h| tick.saturating_sub(h.2) <= EMITTING_TICKS);
        self.detections.clear();
        work.track_updates = updates;
        for (side, ev) in events {
            let lane = if matches!(ev, SimEvent::RungChanged { .. } | SimEvent::StanceChanged { .. }) { Lane::Reliable } else { Lane::Log };
            self.emit(Audience::side(side), lane, ev);
        }
    }

    /// Live hulls (position, velocity) each side counts as its own: its hulls and its allies'.
    fn side_hulls(&self, allied: &[SideMask]) -> Vec<Vec<(DVec3, DVec3)>> {
        let mut out: Vec<Vec<(DVec3, DVec3)>> = vec![Vec::new(); allied.len().max(self.pictures.sides.len())];
        for (_, v) in self.world.vessels() {
            if !alive(v.status) { continue; }
            let Some((pos, vel, _)) = self.world.locate(v.id) else { continue };
            for (si, list) in out.iter_mut().enumerate() {
                let mask = allied.get(si).copied().unwrap_or(SideMask::only(SideId(si as u8)));
                if mask.has(v.side) { list.push((pos, vel)); }
            }
        }
        out
    }

    // ---- Phase 7: enemy AI ----

    /// The briefing's rendezvous (L797): the escorted group of an Escort objective holds at its start under
    /// `Anchor` until a player-side hull comes within [`RENDEZVOUS_M`] of a live member, then takes `Flee` to
    /// the objective's exit. One-shot per group: once retasked it never goes back.
    fn release_escorted(&mut self) {
        let content::campaign::Objective::Escort { tag, exit } = &self.config.objective else { return };
        if tag.is_empty() || !self.ai.groups.iter().any(|g| g.tag == *tag && matches!(g.orders, Some(GroupOrders::Anchor))) { return; }
        if !self.world.terrain().map.markers.iter().any(|m| m.name == *exit) { return; }
        let player_sides = self.player_sides();
        let players: Vec<DVec2> = self.world.vessels()
            .filter(|(_, v)| v.parent.is_none() && alive(v.status) && (v.player.is_some() || player_sides.has(v.side)))
            .map(|(_, v)| DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z)).collect();
        if players.is_empty() { return; }
        let world = &self.world;
        for g in self.ai.groups.iter_mut().filter(|g| g.tag == *tag && matches!(g.orders, Some(GroupOrders::Anchor))) {
            let members: Vec<DVec2> = g.members.iter().filter_map(|m| hull(world, *m)).filter(|v| alive(v.status)).map(|v| DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z)).collect();
            if rendezvous_met(&members, &players) { g.orders = Some(GroupOrders::Flee { exit: exit.clone() }); }
        }
    }

    fn phase_ai(&mut self, t: Tick, env: &TickEnv, work: &mut crate::ctx::SystemsWork) {
        self.release_escorted();
        let ctx = env.ctx(t);
        let mut out: Vec<AiOut> = Vec::new();
        let (decisions, expansions) = crate::ai::step(&mut self.ai, &self.world, &mut self.ships, &self.pictures, &ctx, self.deps.budgets.astar_expansions, &mut out);
        work.ai_decisions = decisions;
        work.astar_expansions = expansions;
        for o in out {
            match o {
                AiOut::Strike { vessel } => {
                    self.world.set_struck(vessel, true);
                    let held: Vec<(bool, SideId, TrackId)> = self.ships.ships.iter().map(|s| (s.player.is_some(), s.side, s.orders.designated)).collect();
                    for i in designations_on(&held, &self.pictures, vessel) { self.ships.ships[i].orders.designated = TrackId::NONE; }
                    // One radio line (text, L1706) to each player whose side holds a live track of the hull.
                    let mut lines: Vec<(PlayerSlot, EntityId, TrackId)> = Vec::new();
                    for ship in &self.ships.ships {
                        let Some(player) = ship.player else { continue };
                        let Some(t) = self.pictures.side(ship.side).and_then(|p| p.tracks.iter().find(|t| t.is_live() && t.truth == vessel)) else { continue };
                        lines.push((player, ship.vessel, t.id));
                    }
                    for (player, own, track) in lines {
                        let text = crate::protocol::text::Msg::new("officer.radio.gunnery.struck").arg(crate::protocol::text::MsgArg::Track(track));
                        self.emit(Audience::player(player), Lane::Reliable, SimEvent::OfficerMessage { vessel: own, role: content::enums::OfficerRole::Gunnery, text });
                    }
                }
                AiOut::SteerBody { body, steer } => self.world.steer_body(body, steer),
                AiOut::Alert { side, level } => self.emit(Audience::Host, Lane::Log, SimEvent::EnemyAlert { side, level }),
                AiOut::CallReinforcements { side } => {
                    for &i in &self.schedule.vessels {
                        let Some(v) = self.config.vessels.get(i as usize).filter(|v| v.side == side) else { continue };
                        if self.run.hastened.iter().any(|h| h.0 == i) { continue; }
                        // Half the wait that was left, but never less than a minute's notice.
                        let planned = v.arrives_s as u64 * TICK_HZ as u64;
                        let left = planned.saturating_sub(t.0);
                        let by = t.0.saturating_add((left / 2).max(REINFORCE_NOTICE_TICKS)).min(planned);
                        self.run.hastened.push((i, by));
                    }
                }
                AiOut::SettingsChanged { vessel } => self.recompile_ship(vessel),
                AiOut::AceLine { ace, cue, text, from } => self.ace_line(ace, cue, text, from),
                AiOut::Release(r) => {
                    let b = self.world.bodies();
                    let Some(row) = b.row_of(r.aircraft) else { continue };
                    let (Some(&pos), Some(&vel), Some(&side)) = (b.pos.get(row), b.vel.get(row), b.side.get(row)) else { continue };
                    if !(pos.is_finite() && vel.is_finite()) || env.cat.munitions.get_index(r.munition.index()).is_none() { continue; }
                    let body = self.world.spawn_body(r.munition, side, pos, vel);
                    // Registered here with the track the aircraft attacked; the event fold then leaves it alone.
                    self.guidance.launch(body, side, r.aircraft, r.munition, r.track, None, &self.pictures, &ctx);
                }
            }
        }
    }

    /// An ace speaks: every side that is not her own hears it, with the track it holds on her ship if any.
    fn ace_line(&mut self, ace: AceId, cue: content::campaign::AceCue, text: Msg, from: EntityId) {
        let Some(side) = self.world.locate(from).map(|l| l.2) else { return };
        let allied = allied_masks(&self.config);
        let own = allied.get(side.index()).copied().unwrap_or(SideMask::only(side));
        let mut groups: Vec<(Option<TrackId>, SideMask)> = Vec::new();
        for si in 0..allied.len() {
            let s = SideId(si as u8);
            if own.has(s) { continue; }
            let track = self.pictures.side(s).and_then(|p| p.track_of(from)).map(|t| t.id);
            match groups.iter_mut().find(|g| g.0 == track) { Some(g) => g.1 = g.1.with(s), None => groups.push((track, SideMask::only(s))) }
        }
        for (from, m) in groups { self.emit(Audience::Sides(m), Lane::Reliable, SimEvent::AceMessage { ace: ace.clone(), cue, text: text.clone(), from }); }
    }

    /// Recompile a ship's graph after its live settings or rules changed, keeping node scopes and greyed
    /// scripts, and resolve its doctrine table again.
    pub(super) fn recompile_ship(&mut self, vessel: EntityId) {
        let Some(player) = self.ships.get(vessel).map(|s| s.player) else { return };
        let ruleset = self.compile_ruleset(player);
        let cat = self.deps.catalogue.clone();
        let Some(ship) = self.ships.get_mut(vessel) else { return };
        ship.graph = ship.design.recompile(&ship.settings, &ship.rules, &ship.design.overrides, &cat, &ruleset);
        ship.graph_state = vm::init_state(&ship.graph, Some(&ship.graph_state));
        ship.doctrine = crate::doctrine::resolve(&ship.fleet_doctrine, &ship.settings, &ship.design.mounts, &cat.tuning);
        ship.control.settings_dirty = false;
    }

    // ---- Phase 8: control ----

    fn phase_control(&mut self, t: Tick, env: &TickEnv, work: &mut crate::ctx::SystemsWork) {
        let ctx = env.ctx(t);
        let tick = t.0;
        let due: Vec<usize> = self.ships.ships.iter().enumerate()
            .filter(|(_, s)| graph_due(s.vessel, t) && hull(&self.world, s.vessel).is_some_and(|v| alive(v.status)))
            .map(|(i, _)| i).collect();
        if due.is_empty() { return; }
        let allied = allied_masks(&self.config);
        let hulls = self.side_hulls(&allied);
        let empty: Vec<SidePicture> = (0..self.pictures.sides.len().max(1)).map(|i| SidePicture { side: SideId(i as u8), tracks: Vec::new(), next_id: 1 }).collect();
        let budget = VmBudget { node_evals: self.deps.budgets.graph_node_evals, luau_interrupts: self.deps.budgets.luau_interrupts };
        let fallback = SidePicture { side: SideId(0), tracks: Vec::new(), next_id: 1 };
        let player_sides = self.player_sides();
        let mut outs: Vec<(usize, GraphOut)> = Vec::with_capacity(due.len());
        {
            let shared = Shared { ctx: &ctx, world: &self.world, pictures: &self.pictures, links: &self.links, hulls: &hulls, esm_sites: &self.config.intel.esm_sites, player_sides, empty: &empty, fallback: &fallback, budget };
            let plain = |s: &Ship| graph_due(s.vessel, t) && hull(shared.world, s.vessel).is_some_and(|v| alive(v.status)) && !has_live_scripts(s);
            let n_plain = self.ships.ships.iter().filter(|s| plain(s)).count();
            if n_plain >= PAR_MIN_SHIPS {
                // Fan out per vessel; `collect` keeps ascending vessel order for the serial merge.
                outs = self.ships.ships.par_iter_mut().enumerate().filter(|(_, s)| plain(s)).map(|(i, s)| (i, graph_tick(s, &shared, None))).collect();
            } else {
                for (i, s) in self.ships.ships.iter_mut().enumerate() { if plain(s) { outs.push((i, graph_tick(s, &shared, None))); } }
            }
            // Script ships, serially in ascending vessel order with the one script host.
            for &i in &due {
                let s = &mut self.ships.ships[i];
                if !has_live_scripts(s) { continue; }
                self.deps.scripts.begin_tick(s.vessel);
                let o = graph_tick(s, &shared, Some(&mut *self.deps.scripts));
                outs.push((i, o));
            }
        }
        outs.sort_by_key(|o| o.0);

        // Serial merge, ascending vessel: work counts, faults, controls and side effects.
        let mut faults: Vec<(EntityId, SideId, GraphNodeId, ScriptFault)> = Vec::new();
        let mut unreproducible: Vec<EntityId> = Vec::new();
        let mut applied: Vec<(EntityId, VesselControls, SideEffects)> = Vec::with_capacity(outs.len());
        let mut rows: Vec<crate::protocol::event::ShotLogEntry> = Vec::new();
        for (i, o) in outs {
            work.graph_node_evals = work.graph_node_evals.saturating_add(o.node_evals);
            work.luau_interrupts = work.luau_interrupts.saturating_add(o.luau_interrupts);
            work.gun_iterations = work.gun_iterations.saturating_add(o.gun_iterations);
            work.engagement_pairs = work.engagement_pairs.saturating_add(o.engagement_pairs);
            let ship = &mut self.ships.ships[i];
            for (node, fault) in o.faults { faults.push((ship.vessel, ship.side, node, fault)); }
            if o.not_reproducible { unreproducible.push(ship.vessel); }
            let Some(v) = hull(&self.world, ship.vessel) else { continue };
            let sinks = core::mem::take(&mut ship.sinks);
            let (mut controls, fx) = build_controls(ship, v, &sinks, &self.pictures, &ctx);
            ship.sinks = sinks;
            let held = crate::input::held_mounts(ship);
            if !held.is_empty() {
                let none = SidePicture { side: ship.side, tracks: Vec::new(), next_id: 1 };
                let picture = self.pictures.side(ship.side).unwrap_or(&none);
                for slot in held {
                    let (order, shots) = crate::input::manual_mount_order(ship, slot, picture, &self.history, &self.world, &ctx);
                    if let Some(pc) = controls.parts.get_mut(slot.index()) { pc.order = PartOrder::Mount(order); }
                    rows.extend(shots);
                }
            }
            applied.push((ship.vessel, controls, fx));
        }
        for (vessel, controls, fx) in applied {
            self.world.set_controls(vessel, controls);
            self.apply_side_effects(vessel, fx, tick);
        }
        for row in rows {
            self.shot_log.push(row);
            self.emit(Audience::Host, Lane::Log, SimEvent::ShotLogged(row));
        }
        for (vessel, side, node, fault) in faults { self.emit(Audience::side(side), Lane::Reliable, SimEvent::ScriptGreyedOut { vessel, node, fault }); }
        for vessel in unreproducible {
            let node = self.ships.get(vessel).and_then(|s| s.graph_state.greyed.iter().rev().find(|g| g.1 == ScriptFault::Backstop).map(|g| g.0)).unwrap_or_default();
            self.emit(Audience::Host, Lane::Log, SimEvent::TickNotReproducible { vessel, node });
        }
        self.report_ship_changes(&due);
        self.stream_scopes(&due, t);
    }

    /// Side-level effects of one ship's sinks: track overrides by the graph, the ship's own preset and
    /// emission setting, and the launches guidance must bind.
    fn apply_side_effects(&mut self, vessel: EntityId, fx: SideEffects, tick: u64) {
        let Some(side) = self.ships.get(vessel).map(|s| s.side) else { return };
        for (track, stance) in fx.stance_overrides {
            let Some(tr) = self.pictures.side_mut(side).and_then(|p| p.get_mut(track)) else { continue };
            let before = (tr.stance, tr.stance_reason, tr.stance_override);
            if ident::stance::set_override(tr, stance, true) && before != (tr.stance, tr.stance_reason, tr.stance_override) {
                let ev = SimEvent::StanceChanged { side, track, stance: tr.stance, reason: tr.stance_reason };
                self.emit(Audience::side(side), Lane::Reliable, ev);
            }
        }
        for (track, rung) in fx.id_overrides {
            let Some(tr) = self.pictures.side_mut(side).and_then(|p| p.get_mut(track)) else { continue };
            if is_link_friend(tr) { continue; }
            let want = rung.map(|r| (r, tr.class.clone()));
            if tr.id_override == want { continue; }
            let (rung, reason) = ident::set_override(tr, want, &self.ident, tick);
            let ev = SimEvent::RungChanged { side, track, rung, reason, class: tr.class.clone() };
            self.emit(Audience::side(side), Lane::Reliable, ev);
        }
        if let Some(e) = fx.emcon { if let Some(s) = self.ships.get_mut(vessel) { s.settings.emcon = e; } }
        if let Some(p) = fx.preset {
            let changed = self.ships.get(vessel).is_some_and(|s| s.settings.preset != p);
            let report = self.run.report_mut(vessel);
            if changed && report.preset_tick.is_none_or(|last| tick < last || tick - last >= PRESET_GAP_TICKS) {
                report.preset_tick = Some(tick);
                if let Some(s) = self.ships.get_mut(vessel) { s.settings.preset = p; }
                self.recompile_ship(vessel);
            }
        }
        if !fx.launches.is_empty() {
            let Some(ship) = self.ships.get(vessel) else { return };
            for (slot, track) in fx.launches {
                let pn_n = ship.sinks.get(SinkKind::MissileLaunch(slot)).and_then(|w| w.values.get(1)).and_then(|v| if let PortValue::Scalar(n) = v { Some(*n) } else { None }).filter(|n| n.is_finite() && *n > 0.0);
                self.run.launches.retain(|l| !(l.launcher == vessel && l.slot == slot));
                self.run.launches.push(LaunchBind { launcher: vessel, slot, track, pn_n, tick });
            }
        }
    }

    /// Turn what each ship's stages decided this graph tick into events, once per change: emitters coming on
    /// and going off, the ESM warning, and each mount's engaging or holding state.
    fn report_ship_changes(&mut self, due: &[usize]) {
        let mut out: Vec<(SideId, Lane, SimEvent)> = Vec::new();
        for &i in due {
            let Some(ship) = self.ships.ships.get(i) else { continue };
            let (vessel, side) = (ship.vessel, ship.side);
            let now_radiating = ship.stages.emcon.radiating.clone();
            let warn = ship.stages.emcon.esm_warning;
            let engage: Vec<(PartSlot, EngageStatus)> = ship.stages.engage.mounts.iter().map(|m| (m.slot, m.status)).collect();
            // Solver nodes firing on their last iterate (L518): (mount, residual in metres).
            let loose: Vec<(PartSlot, f64)> = ship.graph.nodes.iter().filter_map(|n| {
                let (NodeKind::Stage(OfficerStage::BallisticSolver), Some(slot)) = (&n.kind, n.part) else { return None };
                let out = |k: u32| ship.graph_state.values.get((n.out_base + k) as usize);
                if n.out_count < 9 { return None; }
                let (Some(PortValue::Bool(true)), Some(PortValue::Bool(false)), Some(PortValue::Bool(true))) = (out(SOLVER_OUT_FIRE), out(SOLVER_OUT_CONVERGED), out(SOLVER_OUT_IN_RANGE)) else { return None };
                let residual = match out(SOLVER_OUT_RESIDUAL) { Some(PortValue::Scalar(r)) if r.is_finite() => *r, _ => 0.0 };
                Some((slot, residual))
            }).collect();
            let report = self.run.report_mut(vessel);
            let now_loose: Vec<PartSlot> = { let mut v: Vec<PartSlot> = loose.iter().map(|l| l.0).collect(); v.sort_unstable(); v.dedup(); v };
            if report.unconverged != now_loose {
                for (slot, residual) in &loose {
                    if report.unconverged.contains(slot) { continue; }
                    let track = engage.iter().find(|e| e.0 == *slot).and_then(|e| match e.1 { EngageStatus::Engaging { track, .. } => Some(track), _ => None }).unwrap_or(TrackId::NONE);
                    out.push((side, Lane::Log, SimEvent::SolutionUnconverged { mount: MountRef { vessel, slot: *slot }, track, error_m: *residual as f32 }));
                }
                report.unconverged = now_loose;
            }
            if report.radiating != now_radiating {
                for s in now_radiating.iter().filter(|s| !report.radiating.contains(s)) { out.push((side, Lane::Log, SimEvent::Radiating { vessel, slot: *s, on: true })); }
                for s in report.radiating.iter().filter(|s| !now_radiating.contains(s)) { out.push((side, Lane::Log, SimEvent::Radiating { vessel, slot: *s, on: false })); }
                report.radiating = now_radiating;
            }
            if report.esm_warning != warn {
                report.esm_warning = warn;
                out.push((side, Lane::Reliable, SimEvent::EsmWarning { vessel, on: warn }));
            }
            if report.engage != engage {
                for (slot, status) in &engage {
                    let before = report.engage.iter().find(|e| e.0 == *slot).map(|e| e.1);
                    // The numbers inside a hold reason move every tick; the event is for the reason changing.
                    let same = before.is_some_and(|b| core::mem::discriminant(&b) == core::mem::discriminant(status) && match (b, *status) {
                        (EngageStatus::Engaging { track: a, reason: ra }, EngageStatus::Engaging { track: b, reason: rb }) => a == b && ra == rb,
                        (EngageStatus::Holding { track: a, reason: ra }, EngageStatus::Holding { track: b, reason: rb }) => a == b && core::mem::discriminant(&ra) == core::mem::discriminant(&rb),
                        _ => true,
                    });
                    if same { continue; }
                    let mount = MountRef { vessel, slot: *slot };
                    match *status {
                        EngageStatus::Engaging { track, reason } => out.push((side, Lane::Log, SimEvent::Engaging { mount, track, reason })),
                        EngageStatus::Holding { track, reason } if !track.is_none() => out.push((side, Lane::Log, SimEvent::Holding { mount, track, reason })),
                        _ => {}
                    }
                }
                report.engage = engage;
            }
        }
        for (side, lane, ev) in out { self.emit(Audience::side(side), lane, ev); }
    }

    /// Scoped ports stream their values to the scoping player at 10 Hz (L216): on the ship's first graph tick
    /// of each 100 ms period.
    fn stream_scopes(&mut self, due: &[usize], t: Tick) {
        let mut out: Vec<(PlayerSlot, QueryReply)> = Vec::new();
        for &i in due {
            let Some(ship) = self.ships.ships.get(i) else { continue };
            let Some(player) = ship.player else { continue };
            if ship.graph_state.scoped.is_empty() || crate::officers::stages::slow_period(t.0) == crate::officers::stages::slow_period(t.0.saturating_sub(2)) { continue; }
            for port in &ship.graph_state.scoped {
                let Some(node) = ship.graph.nodes.iter().find(|n| n.id == port.node) else { continue };
                if port.port >= node.out_count { continue; }
                let Some(v) = ship.graph_state.values.get(node.out_base as usize + port.port as usize) else { continue };
                out.push((player, QueryReply::Scope { port: *port, value: v.to_value() }));
            }
        }
        for (player, reply) in out { self.emit(Audience::player(player), Lane::Log, SimEvent::QueryReply { player, reply }); }
    }

    // ---- Phase 9: guidance, offboard units, RHIBs ----

    fn phase_guidance(&mut self, t: Tick, env: &TickEnv, work: &mut crate::ctx::SystemsWork) {
        let ctx = env.ctx(t);
        let before: Vec<(EntityId, SeekerPhase, SideId, TrackId)> = self.guidance.guided.iter().map(|g| (g.body, g.phase, g.side, g.track)).collect();
        let mut out: Vec<GuidanceOut> = Vec::new();
        work.guidance_steps = self.guidance.step(&self.world, &self.pictures, &self.sigs, &ctx, &mut out);
        for o in out {
            self.world.steer_body(o.body, o.steer);
            self.world.set_body_radiating(o.body, o.radiating);
            let Ok(k) = before.binary_search_by(|b| b.0.cmp(&o.body)) else { continue };
            let (_, was, side, track) = before[k];
            if was == o.phase { continue; }
            self.emit(Audience::side(side), Lane::Log, SimEvent::Seeker { body: o.body, phase: o.phase, target: track });
            if let (SeekerPhase::Seduced, Some(decoy)) = (o.phase, o.seduced_by) {
                let d = self.world.decoys();
                let owner_side = d.row_of(decoy).and_then(|r| d.owner.get(r)).and_then(|owner| self.world.locate(*owner)).map(|l| l.2);
                let mut mask = SideMask::only(side);
                if let Some(s) = owner_side { mask = mask.with(s); }
                self.emit(Audience::Sides(mask), Lane::Reliable, SimEvent::Seduced { body: o.body, decoy });
                if owner_side.is_some_and(|s| self.player_sides().has(s)) { crate::objectives::note_seduced(&mut self.objectives); }
            }
        }

        // Offboard units of every ship with a bay.
        let mut asks: Vec<(EntityId, OffboardOut)> = Vec::new();
        for ship in &mut self.ships.ships {
            if ship.design.bays.is_empty() || !hull(&self.world, ship.vessel).is_some_and(|v| alive(v.status)) { continue; }
            let none = SidePicture { side: ship.side, tracks: Vec::new(), next_id: 1 };
            let picture = self.pictures.side(ship.side).unwrap_or(&none);
            let mut o: Vec<OffboardOut> = Vec::new();
            crate::offboard::step(&mut ship.offboard, ship.vessel, &self.world, picture, &ship.orders.route, &ctx, &mut o);
            let vessel = ship.vessel;
            asks.extend(o.into_iter().map(|x| (vessel, x)));
        }
        for (vessel, ask) in asks {
            match ask {
                OffboardOut::Launch { bay } => self.run.hold_order(vessel, bay, PartOrder::LaunchOffboard, t.0),
                OffboardOut::Recover { bay } => self.run.hold_order(vessel, bay, PartOrder::RecoverOffboard, t.0),
                OffboardOut::Steer { body, steer } => self.world.steer_body(body, steer),
            }
        }
        self.steer_rhibs(t);
    }

    /// RHIBs of station actions: out to the site, and home when recalled.
    fn steer_rhibs(&mut self, t: Tick) {
        if self.run.rhibs.is_empty() { return; }
        let cat = self.deps.catalogue.clone();
        let mut steer: Vec<(EntityId, Steer)> = Vec::new();
        let mut recover: Vec<(EntityId, PartSlot)> = Vec::new();
        let mut done: Vec<(EntityId, PartSlot)> = Vec::new();
        let b = self.world.bodies();
        for task in &self.run.rhibs {
            let ship = hull(&self.world, task.vessel).filter(|v| alive(v.status));
            let body = (0..b.len()).find(|&r| b.launcher.get(r) == Some(&task.vessel) && b.launcher_slot.get(r) == Some(&task.slot)
                && b.kind.get(r).and_then(|k| cat.munitions.get_index(k.index())).is_some_and(|(_, d)| d.kind == MunitionKind::Rhib));
            let (Some(ship), Some(row)) = (ship, body) else {
                // No boat in the water: a recall is finished, and so is anything from a lost ship.
                if task.recall || ship.is_none() { done.push((task.vessel, task.slot)); }
                continue;
            };
            let (Some(&id), Some(&pos)) = (b.entity.get(row), b.pos.get(row)) else { continue };
            if task.recall {
                let home = ship.kin.pose.pos;
                steer.push((id, Steer::Waypoint(DVec3::new(home.x, 0.0, home.z))));
                let (dx, dz) = (pos.x - home.x, pos.z - home.z);
                let reach = RHIB_RECOVER_M + 0.5 * ship.dims[0].max(ship.dims[2]);
                if dx * dx + dz * dz <= reach * reach { recover.push((task.vessel, task.slot)); }
            } else if task.toward.is_finite() {
                // sim-core recalls a boat whose waypoint lies beside its own ship, so a site the ship is
                // sitting on is worked from a standoff on the site's side (to starboard when it is dead under).
                let home = DVec2::new(ship.kin.pose.pos.x, ship.kin.pose.pos.z);
                let standoff = RHIB_STANDOFF_M + ship.dims[0].max(ship.dims[2]);
                let out = task.toward - home;
                let goal = if out.length_squared() >= standoff * standoff { task.toward } else {
                    let beam = ship.kin.pose.rot * DVec3::Z;
                    let dir = out.try_normalize().or_else(|| DVec2::new(beam.x, beam.z).try_normalize()).unwrap_or(DVec2::X);
                    home + dir * standoff
                };
                steer.push((id, Steer::Waypoint(DVec3::new(goal.x, 0.0, goal.y))));
            }
        }
        for (id, s) in steer { self.world.steer_body(id, s); }
        for (vessel, slot) in recover { self.run.hold_order(vessel, slot, PartOrder::RecoverOffboard, t.0); }
        if !done.is_empty() { self.run.rhibs.retain(|r| !done.contains(&(r.vessel, r.slot))); }
    }

    // ---- Phase 10: special operations ----

    fn phase_specops(&mut self, t: Tick, env: &TickEnv) {
        let ctx = env.ctx(t);
        let mut out: Vec<SpecOpsOut> = Vec::new();
        crate::specops::step(&mut self.specops, &self.world, &self.ships, &self.pictures, &self.run.prev_events, &ctx, &mut out);
        self.apply_specops(out, t, &ctx);
    }

    /// Carry out what the station actions asked for this tick, in the order they asked.
    pub(super) fn apply_specops(&mut self, out: Vec<SpecOpsOut>, t: Tick, ctx: &SimCtx<'_>) {
        for o in out {
            match o {
                SpecOpsOut::Changed(view) => {
                    let side = self.ships.get(view.actor).map(|s| s.side);
                    if view.state == ActionState::Aborted && !matches!(view.block, Some(ActionBlock::Cancelled)) { self.run.report_mut(view.actor).interrupted_tick = Some(t.0); }
                    match side { Some(s) => self.emit(Audience::side(s), Lane::Reliable, SimEvent::ActionChanged(view)), None => self.emit(Audience::Host, Lane::Log, SimEvent::ActionChanged(view)) }
                }
                SpecOpsOut::Rhib { vessel, slot, launch, toward } => {
                    self.run.rhibs.retain(|r| !(r.vessel == vessel && r.slot == slot));
                    self.run.rhibs.push(RhibTask { vessel, slot, toward, recall: !launch });
                    if launch { self.run.hold_order(vessel, slot, PartOrder::LaunchOffboard, t.0); }
                }
                SpecOpsOut::DropMine { vessel, slot } => self.run.hold_order(vessel, slot, PartOrder::DropMine, t.0),
                SpecOpsOut::TrainCamera { vessel, slot, toward } => {
                    let Some(v) = hull(&self.world, vessel) else { continue };
                    let local = v.kin.pose.rot.inverse() * (toward - v.kin.pose.pos);
                    if !local.is_finite() { continue; }
                    // Vessel-local frame: +X bow, +Z starboard. Sensor training is clockwise from the part's
                    // facing (sim-core parts), so a camera turned on its mount trains from where it faces.
                    let facing = v.parts.iter().find(|p| p.slot == slot).map_or(0.0, |p| crate::sensors::part_facing_cw(p.rot));
                    let train_rad = libm::atan2(local.z, local.x) - facing;
                    self.run.hold_order(vessel, slot, PartOrder::Emit { radiating: false, train_rad }, t.0);
                }
                SpecOpsOut::Capture { prize, to_side, by } => self.capture(prize, to_side, by, ctx),
                SpecOpsOut::Photo { by, side, track, target } => {
                    let class = hull(&self.world, target).and_then(|v| v.class.clone());
                    if let Some(tr) = self.pictures.side_mut(side).and_then(|p| p.get_mut(track)) {
                        ident::set_exact(tr, class, RungReason::Photograph, &self.ident, t.0);
                        let ev = SimEvent::RungChanged { side, track, rung: tr.rung, reason: tr.rung_reason, class: tr.class.clone() };
                        self.emit(Audience::side(side), Lane::Reliable, ev);
                    }
                    self.emit(Audience::side(side), Lane::Reliable, SimEvent::PhotoTaken { by, track });
                }
                SpecOpsOut::Strip { prize } => {
                    // Stripped for parts and scuttled on the spot: the hull leaves the sortie.
                    self.retire_prize(prize);
                }
                SpecOpsOut::Evidence { side } => {
                    if let Some(c) = self.ai.commanders.iter_mut().find(|c| c.side == side) { c.evidence = c.evidence.saturating_add(1); }
                }
                SpecOpsOut::Recovered { .. } => {}
            }
        }
    }

    /// Boarding succeeded: the prize changes side and sails under a prize crew (a `Proxy` captain).
    fn capture(&mut self, prize: EntityId, to_side: SideId, by: PlayerSlot, ctx: &SimCtx<'_>) {
        let class = hull(&self.world, prize).and_then(|v| v.class.clone());
        self.world.set_side(prize, to_side);
        self.world.set_struck(prize, false);
        if let Some(ship) = self.ships.get_mut(prize) {
            ship.side = to_side;
            ship.player = None;
            let mut captain = crate::ai::unit::new_captain(crate::ai::unit::Behaviour::Proxy, None, None, content::campaign::SurrenderRule::Never);
            captain.route = Vec::new();
            ship.captain = Some(captain);
            ship.orders = crate::ship::ShipOrders { weapons_tight: true, speed_kn: Some(0.0), ..Default::default() };
            ship.fleet_doctrine.clear();
            ship.proficiency = Default::default();
            ship.detections.clear();
        }
        for g in &mut self.ai.groups {
            g.members.retain(|m| *m != prize);
            if g.leader == Some(prize) { g.leader = None; }
        }
        // Its sensors now report to the captor's link.
        self.sensors.remove_carrier(prize);
        self.sensors.add_carrier(&self.world, prize, ctx);
        self.emit(Audience::All, Lane::Reliable, SimEvent::PrizeTaken { prize, by, class });
    }

    /// Remove a stripped prize from the sortie. The despawn event does the rest in phase 12: the ship table,
    /// sensors, groups and interest sets forget the hull, and the sides that drew it are told.
    fn retire_prize(&mut self, prize: EntityId) {
        self.deps.scripts.unload(prize);
        self.run.orders.retain(|o| o.vessel != prize);
        self.run.rhibs.retain(|r| r.vessel != prize);
        self.world.despawn(prize);
    }

    /// Write the held one-shot orders into the controls physics is about to read.
    fn apply_held_orders(&mut self) {
        for h in &self.run.orders {
            if let Some(c) = self.world.controls_mut(h.vessel) {
                if c.parts.len() <= h.slot.index() && h.slot.index() < MAX_ORDER_SLOT { c.parts.resize(h.slot.index() + 1, Default::default()); }
                if let Some(p) = c.parts.get_mut(h.slot.index()) { p.order = h.order; }
            }
        }
    }

    // ---- Phases 11 and 12: physics events ----

    fn phase_events(&mut self, t: Tick) {
        let now = self.world.tick().0;
        // One-shot orders sim-core consumed are done; camera training persists until its action ends.
        let world = &self.world;
        self.run.orders.retain(|h| {
            if now >= h.until_tick { return false; }
            let standing = hull(world, h.vessel).and_then(|v| v.controls.parts.get(h.slot.index())).map(|p| p.order);
            matches!(h.order, PartOrder::Emit { .. }) || standing == Some(h.order)
        });
        let events = self.world.drain_events();
        let spawned: Vec<(EntityId, EntityId, PartSlot)> = events.iter().filter_map(|e| if let PhysEvent::BodySpawned { body, launcher, slot, .. } = e { Some((*body, *launcher, *slot)) } else { None }).collect();
        for e in &events {
            match e {
                PhysEvent::Grounded { vessel, .. } => self.run.report_mut(*vessel).grounded_tick = Some(now),
                // Nobody is left to steer or fire: a lost hull's last orders must not outlive it, and neither
                // must what its officers last decided (the plot would count its mounts as engaging).
                PhysEvent::VesselLost { vessel, .. } => {
                    self.world.set_controls(*vessel, VesselControls::default());
                    self.run.orders.retain(|o| o.vessel != *vessel);
                    if let Some(ship) = self.ships.get_mut(*vessel) {
                        for m in &mut ship.stages.engage.mounts { m.status = EngageStatus::Idle; }
                        ship.stages.engage.ranked.clear();
                        ship.stages.emcon.radiating.clear();
                        ship.stages.emcon.esm_warning = false;
                    }
                }
                _ => {}
            }
        }
        self.run.prev_events = keep_for_specops(&events);
        // The event fold turns a track Hostile the moment its hull fires on the side (L501). It has no way to
        // say so, so stances are compared across it whenever anything was hit or splashed.
        let fire = events.iter().any(|e| matches!(e, PhysEvent::ShellImpact { .. } | PhysEvent::BodyImpact { .. } | PhysEvent::CellsDamaged { .. } | PhysEvent::Splash { .. }));
        let before: Vec<(SideId, TrackId, Stance)> = if fire {
            self.pictures.sides.iter().flat_map(|p| p.tracks.iter().filter(|tr| tr.is_live() && tr.stance != Stance::Hostile).map(move |tr| (p.side, tr.id, tr.stance))).collect()
        } else { Vec::new() };
        self.process_phys_events(events);
        for (side, track, was) in before {
            let Some(tr) = self.pictures.side(side).and_then(|p| p.get(track)) else { continue };
            if tr.stance == was { continue; }
            let ev = SimEvent::StanceChanged { side, track, stance: tr.stance, reason: tr.stance_reason };
            self.emit(Audience::side(side), Lane::Reliable, ev);
        }

        // Bind bodies the graph launched to the track and navigation constant the graph chose: the launch
        // request is gone from the ship's orders by the time the body exists.
        if !spawned.is_empty() && !self.run.launches.is_empty() {
            let env = TickEnv::of(self);
            let ctx = env.ctx(self.world.tick());
            for (body, launcher, slot) in spawned {
                let Some(k) = self.run.launches.iter().position(|l| l.launcher == launcher && l.slot == slot) else { continue };
                let bind = self.run.launches.remove(k);
                let Ok(g) = self.guidance.guided.binary_search_by(|g| g.body.cmp(&body)) else { continue };
                let (side, kind, cur_track) = { let g = &self.guidance.guided[g]; (g.side, g.kind, g.track) };
                if cur_track != bind.track || bind.pn_n.is_some() { self.guidance.launch(body, side, launcher, kind, bind.track, bind.pn_n, &self.pictures, &ctx); }
            }
        }
        self.run.launches.retain(|l| t.0.saturating_sub(l.tick) <= LAUNCH_BIND_TICKS);
        // Reports of ships that are gone.
        if now.is_multiple_of(10 * TICK_HZ as u64) {
            let ships = &self.ships;
            self.run.reports.retain(|r| ships.get(r.vessel).is_some());
        }
    }

    // ---- Phase 13: outcome and reporting ----

    fn phase_report(&mut self, t: Tick) {
        let env = TickEnv::of(self);
        let now = self.world.tick();
        let ctx = env.ctx(now);
        let views = crate::objectives::evaluate(&mut self.objectives, &self.world, &self.ships, &self.pictures, &self.specops, &self.config, &ctx);
        for v in views { self.emit(Audience::All, Lane::Reliable, SimEvent::ObjectiveChanged { id: v.id, state: v.state, progress: v.progress }); }
        if self.result.is_none() { if let Some(outcome) = self.objectives.outcome { self.conclude(outcome); } }

        if t.is_due(TRACK_HZ, 3) { self.update_alerts_and_boards(&ctx); }
        crate::input::record_poses(&mut self.history, &self.world);
        if self.config.splash.is_some() { crate::editor::splash::observe(self); }
    }

    /// Build the result, tell every client, and keep it. Idempotent.
    pub(super) fn conclude(&mut self, outcome: Outcome) {
        if self.result.is_some() { return; }
        let tick = self.world.tick().0;
        if self.objectives.outcome.is_none() { self.objectives.outcome = Some(outcome); }
        if self.objectives.ended_tick.is_none() { self.objectives.ended_tick = Some(tick); }
        let result = crate::objectives::build_result(&self.objectives, &self.world, &self.ships, &self.specops, &self.ai.commanders, &self.ai.aces, &self.config, self.ladder.highest_rung);
        let mut summary = crate::objectives::summary(&result, &self.config);
        self.result = Some(result);
        if matches!(self.config.mode, GameMode::Splash | GameMode::Tutorial) && self.config.splash.is_some() { summary.tip = Some(crate::editor::splash::tip(self)); }
        self.emit(Audience::All, Lane::Reliable, SimEvent::SortieEnded(Box::new(summary)));
    }

    /// Alerts and Bridge Boards of player hulls (10 Hz), and the officers' radio lines.
    fn update_alerts_and_boards(&mut self, ctx: &SimCtx<'_>) {
        let tick = ctx.tick.0;
        let owned: Vec<usize> = self.ships.ships.iter().enumerate().filter(|(_, s)| s.player.is_some()).map(|(i, _)| i).collect();
        if owned.is_empty() && self.alerts.alerts.is_empty() { return; }
        let allied = allied_masks(&self.config);
        let hulls = self.side_hulls(&allied);
        let empty: Vec<SidePicture> = (0..self.pictures.sides.len().max(1)).map(|i| SidePicture { side: SideId(i as u8), tracks: Vec::new(), next_id: 1 }).collect();
        let player_sides = self.player_sides();
        let hunters = self.hunter_hulls(player_sides);
        let mut conditions: Vec<Condition> = Vec::new();
        let mut boards: Vec<BoardUpdate> = Vec::new();
        {
            let budget = VmBudget { node_evals: 0, luau_interrupts: 0 };
            let fallback = SidePicture { side: SideId(0), tracks: Vec::new(), next_id: 1 };
            let shared = Shared { ctx, world: &self.world, pictures: &self.pictures, links: &self.links, hulls: &hulls, esm_sites: &self.config.intel.esm_sites, player_sides, empty: &empty, fallback: &fallback, budget };
            for &i in &owned {
                let ship = &self.ships.ships[i];
                let Some(v) = hull(&self.world, ship.vessel).filter(|v| alive(v.status)) else { continue };
                let picture = shared.picture(ship.vessel, ship.side);
                self.alerts.note_layout(v);
                crate::alerts::conditions(ship, v, picture, ctx, &mut conditions);
                let mut push = |kind: AlertKind| conditions.push(Condition { vessel: ship.vessel, kind });
                let report = self.run.reports.binary_search_by(|r| r.vessel.cmp(&ship.vessel)).ok().map(|k| &self.run.reports[k]);
                let recent = |at: Option<u64>| at.is_some_and(|a| tick >= a && tick - a <= CONDITION_HOLD_TICKS);
                if recent(report.and_then(|r| r.grounded_tick)) { push(AlertKind::Grounded); } else if shoal_ahead(v, ship, &self.world) { push(AlertKind::ShoalAhead); }
                if recent(report.and_then(|r| r.interrupted_tick)) { push(AlertKind::ActionInterrupted); }
                if self.ladder.rung >= 2 { push(AlertKind::HostOverload { rung: self.ladder.rung }); }
                if picture.tracks.iter().any(|tr| tr.is_live() && tr.stance != Stance::Friend && hunters.binary_search(&tr.truth).is_ok()) { push(AlertKind::HunterGroupApproaching); }
                let view = shared.view(ship, v);
                let board = crate::officers::board::update_board(ship, &view);
                let lines = crate::officers::board::messages(&ship.board, &board, ship, tick);
                boards.push((i, board, lines));
            }
        }
        // The alert manager wants conditions ascending by vessel; kinds keep their collection order.
        conditions.sort_by_key(|c| c.vessel);
        let mut changes: Vec<AlertChange> = Vec::new();
        self.alerts.update(&conditions, tick, &mut changes);
        self.emit_alert_changes(changes);
        for (i, board, lines) in boards {
            let ship = &mut self.ships.ships[i];
            ship.board = board;
            let (vessel, player) = (ship.vessel, ship.player);
            let Some(player) = player else { continue };
            for (role, text) in lines { self.emit(Audience::player(player), Lane::Reliable, SimEvent::OfficerMessage { vessel, role, text }); }
        }
    }

    /// Alert changes go to the player who owns the vessel.
    pub(super) fn emit_alert_changes(&mut self, changes: Vec<AlertChange>) {
        for ch in changes {
            let (vessel, event) = match ch {
                AlertChange::Raised(view) => { crate::objectives::note_alert_raised(&mut self.objectives); (view.vessel, SimEvent::AlertRaised(view)) }
                AlertChange::Escalated(view) => (view.vessel, SimEvent::AlertEscalated(view)),
                AlertChange::State(id, state) => {
                    let Some(v) = self.alerts.alerts.iter().find(|a| a.id == id).map(|a| a.vessel) else { continue };
                    (v, SimEvent::AlertChanged { id, state })
                }
                AlertChange::Flood(vessel, on) => (vessel, SimEvent::AlertFlood { vessel, on }),
                AlertChange::Silenced(vessel, until_tick) => (vessel, SimEvent::AlertsSilenced { vessel, until_tick }),
            };
            let audience = match self.ships.get(vessel).and_then(|s| s.player) { Some(p) => Audience::player(p), None => Audience::Host };
            self.emit(audience, Lane::Reliable, event);
        }
    }

    /// Hulls of hostile groups under Hunt orders, ascending (the alert fires only on a track of one).
    fn hunter_hulls(&self, player_sides: SideMask) -> Vec<EntityId> {
        // The alert names the theatre's hunter-killer group (L749), not every group sent to search.
        let mut v: Vec<EntityId> = self.ai.groups.iter()
            .filter(|g| !player_sides.has(g.side) && matches!(g.orders, Some(GroupOrders::Hunt)) && g.kind == Some(content::campaign::FleetKind::HunterKiller))
            .flat_map(|g| g.members.iter().copied()).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

/// Charted water one stopping distance ahead is shallower than the hull needs.
fn shoal_ahead(v: &sim_core::vessel::Vessel, ship: &Ship, world: &World) -> bool {
    let speed = v.forward_speed();
    if !(speed.is_finite() && speed > 1.0) { return false; }
    let fwd = v.kin.pose.rot * DVec3::X;
    let flat = DVec2::new(fwd.x, fwd.z);
    if !(flat.is_finite() && flat.length_squared() > 1e-6) { return false; }
    let reach = if ship.design.turn.stop_distance_m.is_finite() { ship.design.turn.stop_distance_m.clamp(150.0, 2_000.0) } else { 150.0 };
    let centre = v.to_world(DVec3::new(v.dims[0] * 0.5, 0.0, v.dims[2] * 0.5));
    let p = DVec2::new(centre.x, centre.z) + flat.normalize() * reach;
    let draft = if ship.design.turn.draft_m.is_finite() { ship.design.turn.draft_m.max(0.0) } else { 0.0 };
    let depth = world.terrain().depth_at(p.x, p.y);
    depth.is_finite() && depth < draft + 2.0
}

impl Sim {
    /// Run `f` with the view one ship's stages see now (the Bridge Board and "Why?" read it).
    pub(super) fn with_view<R>(&self, vessel: EntityId, env: &TickEnv, f: impl FnOnce(&Ship, &ShipView<'_>) -> R) -> Option<R> {
        let ship = self.ships.get(vessel)?;
        let v = hull(&self.world, vessel)?;
        let ctx = env.ctx(self.world.tick());
        let allied = allied_masks(&self.config);
        let hulls = self.side_hulls(&allied);
        let empty: Vec<SidePicture> = (0..self.pictures.sides.len().max(1)).map(|i| SidePicture { side: SideId(i as u8), tracks: Vec::new(), next_id: 1 }).collect();
        let fallback = SidePicture { side: SideId(0), tracks: Vec::new(), next_id: 1 };
        let shared = Shared { ctx: &ctx, world: &self.world, pictures: &self.pictures, links: &self.links, hulls: &hulls, esm_sites: &self.config.intel.esm_sites, player_sides: self.player_sides(), empty: &empty, fallback: &fallback, budget: VmBudget { node_evals: 0, luau_interrupts: 0 } };
        let view = shared.view(ship, v);
        Some(f(ship, &view))
    }
}
