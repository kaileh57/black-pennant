//! Unit `sys-sim-keyframe` (hard: must capture every system's state; restore must rebuild derived data).
//! Sim keyframes for the 60 s operation checkpoint, suspend saves, co-op host migration and the 30 s splash
//! rewind. DESIGN.md L112 and L828 (suspend from the latest 60 s checkpoint), L843 (host writes a checkpoint
//! every 60 s and sends it to members; any member can re-host), L216 (30 s rewind).
//!
//! A keyframe = `World::save_keyframe` bytes plus every serialisable system state (pictures, guidance, AI,
//! special operations, objectives, alerts, ladder, time, schedule, players, per-ship orders, settings, rules,
//! stage states, control state). Derived data is rebuilt on restore: designs and compiled graphs from
//! blueprints, signatures, sensor instances, interest sets, graph value slots. As with the world, a restored
//! Sim is not bit-identical to the uninterrupted run from that tick on; replays and netcode do not rely on it.
//! The encoding is versioned with `protocol::PROTOCOL_VERSION`; a build restores only its own version.
//!
//! # Encoding
//! `"BPSK"`, the protocol version (u32 LE), the length of the world keyframe (u64 LE), the world keyframe,
//! then the systems as bincode (varint). The systems part is the [`Systems`] view below. `Sim::state_hash`
//! hashes the same view, so the hash covers exactly what a keyframe carries and a restored Sim hashes the
//! same as the saved one.
//!
//! # What is rebuilt instead of saved
//! - Designs, prefabs and compiled graphs: from the config's blueprints, each ship's graph recompiled from
//!   its saved live settings and rules. Node memory is restored when the recompiled graph has the same
//!   memory slots; graph value slots and the last sink writes fill in at the ship's next graph tick.
//! - The doctrine table (from fleet doctrine, settings and the design's mounts), signatures, the PvP exterior
//!   indices (lazily, as `sys-interest` documents), scripts (reloaded into the host; greyed nodes stay grey).
//! - The AI's navigation grid and clearance masks: static functions of the terrain. The keyframe keeps which
//!   masks existed and the planner's queue.
//!
//! Sensor instances and interest sets are small and are saved as they are (stagger phases and swept sectors
//! then continue exactly), which is stricter than the list above requires.
//!
//! # Trust
//! Keyframe bytes arrive over the network on a re-host. Decoding is size-limited and never panics: a
//! truncated, oversized or inconsistent keyframe is `SimError::Keyframe`. A decoded frame is checked against
//! the config (seed, side count) and the restored world (every ship has its hull, except a player's, which
//! outlives its despawned wreck).
//!
//! # Rewind (splash and tutorial basins)
//! `Sim::step` stores one keyframe a second, 30 kept, and the state at the first step as the restart point.
//! A rewind restores in place, keeps the frames up to the one restored, keeps the pause and rate requests in
//! force, drops the commands the frame had queued (they were given in the timeline being undone), and
//! re-sends every connected player the catch-up a joining client gets.
use super::{PlayerState, RunState, Schedule, Sim, SimDeps, SimError};
use crate::ai::AiState;
use crate::datalink::{LinkPicture, PartCommand};
use crate::graph::NodeMem;
use crate::prelude::*;
use crate::protocol::command::CommandEnvelope;
use crate::protocol::config::{BlueprintRef, SortieConfig, SortieResult};
use crate::protocol::event::{ScriptFault, ShotLogEntry};
use crate::sensors::{Detection, SensorInstance};
use crate::ship::{Ship, ShipTable};
use bincode::Options;
use content::save::SortieCheckpoint;
use serde::de::DeserializeOwned;
use sim_core::hash::{StateHash, StateHasher};
use std::borrow::Cow;

/// First bytes of a Sim keyframe.
pub const SIM_KEYFRAME_MAGIC: [u8; 4] = *b"BPSK";
/// Decode limit of the systems part of a keyframe, bytes.
pub const MAX_SYSTEMS_BYTES: u64 = 256 * 1024 * 1024;
/// Decode limit of an encoded `SortieConfig` (inline blueprints included), bytes.
pub const MAX_CONFIG_BYTES: u64 = 64 * 1024 * 1024;
/// Bytes before the world keyframe: magic, version, world length.
const HEADER_BYTES: usize = 16;
/// Most side slots a sortie has.
const MAX_SIDES: usize = 8;

fn bad<T>(what: impl Into<String>) -> Result<T, SimError> { Err(SimError::Keyframe(what.into())) }

/// One ship's saved state: everything in `Ship` that is not derived from its design.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ShipFrame<'a> {
    vessel: EntityId,
    side: SideId,
    player: Option<PlayerSlot>,
    design: ContentHash,
    settings: Cow<'a, content::graph::OfficerSettings>,
    rules: Cow<'a, [content::graph::Rule]>,
    mem: Cow<'a, [NodeMem]>,
    greyed: Cow<'a, [(GraphNodeId, ScriptFault)]>,
    scoped: Cow<'a, [content::graph::PortRef]>,
    fleet_doctrine: Cow<'a, [content::graph::DoctrineEntry]>,
    orders: Cow<'a, crate::ship::ShipOrders>,
    control: Cow<'a, crate::input::ControlState>,
    stages: Cow<'a, crate::ship::StageStates>,
    offboard: Cow<'a, crate::offboard::OffboardState>,
    proficiency: crate::ship::Proficiency,
    captain: Cow<'a, Option<crate::ai::unit::CaptainState>>,
    part_command: Cow<'a, [PartCommand]>,
    board: Cow<'a, crate::protocol::snapshot::BridgeBoard>,
    detections: Cow<'a, [Detection]>,
    tag: Cow<'a, str>,
}
impl<'a> ShipFrame<'a> {
    fn of(s: &'a Ship) -> Self {
        ShipFrame {
            vessel: s.vessel, side: s.side, player: s.player, design: s.design.hash, settings: Cow::Borrowed(&s.settings), rules: Cow::Borrowed(&s.rules),
            mem: Cow::Borrowed(&s.graph_state.mem), greyed: Cow::Borrowed(&s.graph_state.greyed), scoped: Cow::Borrowed(&s.graph_state.scoped),
            fleet_doctrine: Cow::Borrowed(&s.fleet_doctrine), orders: Cow::Borrowed(&s.orders), control: Cow::Borrowed(&s.control), stages: Cow::Borrowed(&s.stages),
            offboard: Cow::Borrowed(&s.offboard), proficiency: s.proficiency, captain: Cow::Borrowed(&s.captain), part_command: Cow::Borrowed(&s.part_command),
            board: Cow::Borrowed(&s.board), detections: Cow::Borrowed(&s.detections), tag: Cow::Borrowed(&s.tag),
        }
    }
}

/// The path planner without its grid: the grid and masks are rebuilt from the terrain.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PlannerFrame<'a> {
    has_grid: bool,
    masks: Vec<(u32, u32)>,
    queue: Cow<'a, [crate::ai::path::PathRequest]>,
    done: Cow<'a, [(u32, EntityId, crate::ai::path::Route)]>,
    debt: u32,
    next_id: u32,
    pending: Cow<'a, Option<(u32, EntityId, crate::ai::path::Route)>>,
    latest: Cow<'a, [(EntityId, u32, [u64; 6])]>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SensorsPart<'a> { sensors: Cow<'a, [SensorInstance]>, pairs_last_tick: u32, detections: Cow<'a, [Detection]>, links: Cow<'a, [LinkPicture]> }

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TracksPart<'a> { pictures: Cow<'a, crate::tracks::Pictures>, ident: Cow<'a, crate::ident::IdentLibrary> }

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ControlPart<'a> {
    ships: Vec<ShipFrame<'a>>,
    players: Cow<'a, [PlayerState]>,
    pending: Cow<'a, [CommandEnvelope]>,
    time: Cow<'a, crate::timeflow::TimeState>,
    ladder: crate::lod::LadderState,
    alerts: Cow<'a, crate::alerts::AlertManager>,
    history: Cow<'a, crate::input::PoseHistory>,
    shot_log: Cow<'a, [ShotLogEntry]>,
    work: crate::ctx::SystemsWork,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GuidancePart<'a> { guidance: Cow<'a, crate::guidance::missile::GuidanceTable> }

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AiPart<'a> {
    commanders: Cow<'a, [crate::ai::commander::CommanderState]>,
    groups: Cow<'a, [crate::ai::group::GroupState]>,
    planner: PlannerFrame<'a>,
    aces: Cow<'a, [crate::ai::ace::AceState]>,
    air: Cow<'a, crate::ai::air::AirState>,
    mods: Cow<'a, crate::ai::DoctrineMods>,
    mem: Cow<'a, crate::ai::AiMemory>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ObjectivesPart<'a> {
    objectives: Cow<'a, crate::objectives::ObjectivesState>,
    specops: Cow<'a, crate::specops::SpecOpsState>,
    schedule: Cow<'a, Schedule>,
    interest: Cow<'a, super::interest::InterestState>,
    result: Cow<'a, Option<SortieResult>>,
    splash_log: Cow<'a, Option<crate::editor::splash::SplashLog>>,
    run: Cow<'a, RunState>,
}

/// Every system's saved state, grouped as the subsystem hashes are (L1162-1168).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Systems<'a> {
    tick: u64,
    seed: u64,
    sensors: SensorsPart<'a>,
    tracks: TracksPart<'a>,
    control: ControlPart<'a>,
    guidance: GuidancePart<'a>,
    ai: AiPart<'a>,
    objectives: ObjectivesPart<'a>,
}

fn view(sim: &Sim) -> Systems<'_> {
    let p = &sim.ai.planner;
    Systems {
        tick: sim.world.tick().0,
        seed: sim.config.seed,
        sensors: SensorsPart { sensors: Cow::Borrowed(&sim.sensors.sensors), pairs_last_tick: sim.sensors.pairs_last_tick, detections: Cow::Borrowed(&sim.detections), links: Cow::Borrowed(&sim.links) },
        tracks: TracksPart { pictures: Cow::Borrowed(&sim.pictures), ident: Cow::Borrowed(&sim.ident) },
        control: ControlPart {
            ships: sim.ships.ships.iter().map(ShipFrame::of).collect(), players: Cow::Borrowed(&sim.players), pending: Cow::Borrowed(&sim.pending), time: Cow::Borrowed(&sim.time),
            ladder: sim.ladder, alerts: Cow::Borrowed(&sim.alerts), history: Cow::Borrowed(&sim.history), shot_log: Cow::Borrowed(&sim.shot_log),
            // Snapshot items are counted by the driver per viewer: not part of the simulation's state.
            work: crate::ctx::SystemsWork { snapshot_items: 0, ..sim.work },
        },
        guidance: GuidancePart { guidance: Cow::Borrowed(&sim.guidance) },
        ai: AiPart {
            commanders: Cow::Borrowed(&sim.ai.commanders), groups: Cow::Borrowed(&sim.ai.groups),
            planner: PlannerFrame {
                has_grid: p.grid.depth.is_some(), masks: p.grid.masks.keys().copied().collect(), queue: Cow::Borrowed(&p.queue), done: Cow::Borrowed(&p.done), debt: p.debt, next_id: p.next_id,
                pending: Cow::Borrowed(&p.pending), latest: Cow::Borrowed(&p.latest),
            },
            aces: Cow::Borrowed(&sim.ai.aces), air: Cow::Borrowed(&sim.ai.air), mods: Cow::Borrowed(&sim.ai.mods), mem: Cow::Borrowed(&sim.ai.mem),
        },
        objectives: ObjectivesPart {
            objectives: Cow::Borrowed(&sim.objectives), specops: Cow::Borrowed(&sim.specops), schedule: Cow::Borrowed(&sim.schedule), interest: Cow::Borrowed(&sim.interest),
            result: Cow::Borrowed(&sim.result), splash_log: Cow::Borrowed(&sim.splash_log), run: Cow::Borrowed(&sim.run),
        },
    }
}

/// Hash of each sim-systems subsystem, over exactly what a keyframe saves, in the fixed order sensors,
/// tracks, control, guidance, ai, objectives.
pub(crate) fn system_hashes(sim: &Sim) -> Vec<(&'static str, StateHash)> {
    let v = view(sim);
    fn one<T: Serialize>(part: &T) -> StateHash { let mut h = StateHasher::new(); h.write_serde(part); h.finish() }
    vec![("sensors", one(&v.sensors)), ("tracks", one(&v.tracks)), ("control", one(&(v.tick, v.seed, &v.control))), ("guidance", one(&v.guidance)), ("ai", one(&v.ai)), ("objectives", one(&v.objectives))]
}

/// A value from bincode, size-limited, with nothing left over.
fn decode<T: DeserializeOwned>(bytes: &[u8], limit: u64, what: &str) -> Result<T, SimError> {
    bincode::DefaultOptions::new().with_limit(limit).deserialize(bytes).map_err(|e| SimError::Keyframe(format!("{what}: {e}")))
}

/// Split a keyframe into its world part and its systems part.
fn split(bytes: &[u8]) -> Result<(&[u8], &[u8]), SimError> {
    if bytes.len() < HEADER_BYTES || bytes[..4] != SIM_KEYFRAME_MAGIC { return bad("not a simulation keyframe"); }
    let found = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if found != crate::protocol::PROTOCOL_VERSION { return Err(SimError::Protocol { found, expected: crate::protocol::PROTOCOL_VERSION }); }
    let mut len = [0u8; 8];
    len.copy_from_slice(&bytes[8..16]);
    let world_len = u64::from_le_bytes(len);
    let rest = &bytes[HEADER_BYTES..];
    if world_len > rest.len() as u64 { return bad("truncated"); }
    Ok(rest.split_at(world_len as usize))
}

impl Sim {
    /// Serialise the whole simulation. Unit: `sys-sim-keyframe`.
    ///
    /// Returns an empty vector only if encoding fails, which no Sim built through this crate causes; an empty
    /// keyframe restores as `SimError::Keyframe`.
    pub fn save_keyframe(&self) -> Vec<u8> {
        let world = self.world.save_keyframe();
        if world.is_empty() { return Vec::new(); }
        let mut out = Vec::with_capacity(HEADER_BYTES + world.len() + (1 << 16));
        out.extend_from_slice(&SIM_KEYFRAME_MAGIC);
        out.extend_from_slice(&crate::protocol::PROTOCOL_VERSION.to_le_bytes());
        out.extend_from_slice(&(world.len() as u64).to_le_bytes());
        out.extend_from_slice(&world);
        match bincode::DefaultOptions::new().serialize_into(&mut out, &view(self)) {
            Ok(()) => out,
            Err(_) => Vec::new(),
        }
    }

    /// Rebuild a simulation from its config and a keyframe. Unit: `sys-sim-keyframe`.
    ///
    /// `config` must be the config of the Sim that saved the keyframe as it stood then (`Sim::config`, which
    /// `checkpoint` encodes): sandbox joins and live splash edits add vessels and blueprints to it.
    pub fn restore_keyframe(config: SortieConfig, deps: SimDeps, bytes: &[u8]) -> Result<Sim, SimError> {
        restore(config, deps, bytes).map_err(|failed| failed.0)
    }

    /// The checkpoint record the save layer stores: tick, format, config bytes, keyframe bytes.
    /// Unit: `sys-sim-keyframe`.
    pub fn checkpoint(&self) -> SortieCheckpoint {
        SortieCheckpoint { tick: self.world.tick().0, keyframe_format: crate::protocol::PROTOCOL_VERSION, config: encode_config(&self.config), keyframe: self.save_keyframe() }
    }

    /// Restore from a checkpoint record (suspend load, re-host). Unit: `sys-sim-keyframe`.
    pub fn from_checkpoint(checkpoint: &SortieCheckpoint, deps: SimDeps) -> Result<Sim, SimError> {
        if checkpoint.keyframe_format != crate::protocol::PROTOCOL_VERSION { return Err(SimError::Protocol { found: checkpoint.keyframe_format, expected: crate::protocol::PROTOCOL_VERSION }); }
        let config = decode_config(&checkpoint.config)?;
        let mut sim = Sim::restore_keyframe(config, deps, &checkpoint.keyframe)?;
        if sim.world.tick().0 != checkpoint.tick { return bad("the checkpoint's tick does not match its keyframe"); }
        // A resumed sortie announces its weather as a new one does (setup), so clients draw the restored sea,
        // sky and time of day rather than their defaults.
        let (weather, wave_seed) = (*sim.world.weather(), sim.world.waves().seed);
        sim.emit(crate::protocol::event::Audience::All, crate::protocol::event::Lane::Reliable, crate::protocol::event::SimEvent::Phys(sim_core::PhysEvent::WeatherChanged { weather, wave_seed }));
        Ok(sim)
    }

    /// Splash rewind: go back `ticks` (at most `sim_core::time::REWIND_TICKS`) to the nearest stored
    /// keyframe at or before the target. Returns the tick restored to. Unit: `sys-sim-keyframe`.
    ///
    /// `None` when no stored keyframe is that old (the basin has only just started) or it does not restore.
    pub(crate) fn rewind(&mut self, ticks: u32) -> Option<u64> {
        let now = self.world.tick().0;
        let target = now.saturating_sub((ticks as u64).min(sim_core::time::REWIND_TICKS));
        let k = self.rewind.frames.iter().rposition(|f| f.0 <= target)?;
        let frame = core::mem::take(&mut self.rewind.frames[k].1);
        let done = self.restore_in_place(&frame);
        // The frame goes back into the ring either way; later frames describe a future that no longer exists.
        if let Some(f) = self.rewind.frames.get_mut(k) { f.1 = frame; }
        done.ok()?;
        self.rewind.frames.truncate(k + 1);
        Some(self.world.tick().0)
    }

    /// Splash restart: back to the state at the basin's first step. Returns the tick restored to.
    pub(crate) fn restart(&mut self) -> Option<u64> {
        let (at, frame) = self.rewind.start.take()?;
        let done = self.restore_in_place(&frame);
        self.rewind.start = Some((at, frame));
        done.ok()?;
        self.rewind.frames.clear();
        Some(self.world.tick().0)
    }

    /// Store the restart point and the once-a-second rewind frames. Called by `Sim::step` in basins.
    pub(crate) fn note_rewind_frame(&mut self) {
        let now = self.world.tick().0;
        if self.rewind.start.is_none() { self.rewind.start = Some((now, self.save_keyframe())); }
        let every = self.rewind.every_ticks.max(1) as u64;
        if !now.is_multiple_of(every) || self.rewind.frames.last().is_some_and(|f| f.0 == now) { return; }
        let frame = self.save_keyframe();
        if frame.is_empty() { return; }
        self.rewind.frames.push((now, frame));
        let keep = self.rewind.keep.max(1);
        if self.rewind.frames.len() > keep { let over = self.rewind.frames.len() - keep; self.rewind.frames.drain(0..over); }
    }

    /// Replace this Sim's state with a keyframe's, keeping what belongs to the session and not to the
    /// moment: the dependencies, the rewind ring, the host load, queued out events, the pause and the
    /// players' rate requests. Clients are then resynchronised.
    fn restore_in_place(&mut self, bytes: &[u8]) -> Result<(), SimError> {
        let stand_in = SimDeps {
            catalogue: self.deps.catalogue.clone(), campaign: self.deps.campaign.clone(), designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost),
            guest_gate_passed: self.deps.guest_gate_passed, budgets: self.deps.budgets, ladder: self.deps.ladder,
        };
        let deps = core::mem::replace(&mut self.deps, stand_in);
        match restore(self.config.clone(), deps, bytes) {
            Ok(mut sim) => {
                sim.rewind = core::mem::take(&mut self.rewind);
                sim.host_load = self.host_load;
                sim.out = core::mem::take(&mut self.out);
                sim.time.paused = self.time.paused;
                sim.time.requested = core::mem::take(&mut self.time.requested);
                // Sequence numbers only go forward: a command sent before the rewind is still a duplicate.
                for (player, seq) in core::mem::take(&mut self.run.direct_seq) {
                    match sim.run.direct_seq.binary_search_by(|e| e.0.cmp(&player)) { Ok(i) => sim.run.direct_seq[i].1 = sim.run.direct_seq[i].1.max(seq), Err(i) => sim.run.direct_seq.insert(i, (player, seq)) }
                }
                for old in &self.ships.ships {
                    if let Some(s) = sim.ships.ships.iter_mut().find(|s| s.player.is_some() && s.player == old.player) { s.control.last_seq = s.control.last_seq.max(old.control.last_seq); }
                }
                // The frame was taken before its step's commands ran. Those commands are the past the player
                // went back to change: they are not given again (and would only come back as duplicates).
                sim.pending.clear();
                sim.run.pacing = None;
                *self = sim;
                self.resync_clients();
                Ok(())
            }
            Err(failed) => { let (e, deps) = *failed; self.deps = deps; Err(e) }
        }
    }
}

/// Decode and rebuild. On failure the dependencies come back, so an in-place restore loses nothing.
fn restore(config: SortieConfig, deps: SimDeps, bytes: &[u8]) -> Result<Sim, Box<(SimError, SimDeps)>> {
    let parsed = split(bytes).and_then(|(world, systems)| {
        let sys: Systems<'static> = decode(systems, MAX_SYSTEMS_BYTES, "systems")?;
        Ok((world, sys))
    });
    let (world_bytes, sys) = match parsed { Ok(p) => p, Err(e) => return Err(Box::new((e, deps))) };
    let fail = |e: SimError, sim: Sim| -> Result<Sim, Box<(SimError, SimDeps)>> { Err(Box::new((e, sim.deps))) };
    if sys.seed != config.seed { return Err(Box::new((SimError::Keyframe("the keyframe belongs to another sortie".into()), deps))); }
    if config.protocol != crate::protocol::PROTOCOL_VERSION { return Err(Box::new((SimError::Protocol { found: config.protocol, expected: crate::protocol::PROTOCOL_VERSION }, deps))); }
    if let Err(e) = super::setup::validate_config(&config) { return Err(Box::new((e, deps))); }
    if sys.tracks.pictures.sides.len() > MAX_SIDES || sys.sensors.links.len() > MAX_SIDES { return Err(Box::new((SimError::Keyframe("too many sides".into()), deps))); }

    // A shell Sim first: building designs needs the config and the design cache together.
    let world_config = || sim_core::WorldConfig { seed: config.seed, weather: config.weather, map: config.map.clone(), catalogue: deps.catalogue.clone() };
    let (shell_world, restore_config) = (World::new(world_config()), world_config());
    let mut sim = Sim {
        world: shell_world, config, deps, ships: ShipTable::default(), players: Vec::new(), schedule: Schedule::default(), sigs: Default::default(), sensors: Default::default(),
        detections: Vec::new(), pictures: Default::default(), links: Vec::new(), ident: Default::default(), guidance: Default::default(), ai: AiState::default(), specops: Default::default(),
        objectives: Default::default(), alerts: Default::default(), ladder: Default::default(), host_load: crate::lod::HostLoad::IDLE, time: Default::default(), history: Default::default(),
        shot_log: Vec::new(), interest: Default::default(), rewind: RewindBuffer::new(), pending: Vec::new(), out: Vec::new(), work: Default::default(), result: None, splash_log: None,
        run: RunState::default(),
    };
    // Every design the sortie can field or has fielded: vessel references, then every inline blueprint (a
    // live splash edit leaves the design it replaced in the list, and a hull may still be built from it).
    let mut refs: BTreeSet<BlueprintRef> = sim.config.vessels.iter().map(|v| v.blueprint.clone()).collect();
    refs.extend(sim.config.blueprints.iter().map(|b| BlueprintRef::Inline(b.header.hash)));
    for r in &refs { if let Err(e) = sim.design_for(r) { return fail(e, sim); } }

    let world = {
        let designs = &sim.deps.designs;
        // By design hash, else by the hash of the prefab a design reuses (an in-place edit keeps the old one).
        let lookup = |h: ContentHash| designs.prefab(h).or_else(|| designs.designs.values().find(|d| d.prefab.hash == h).map(|d| d.prefab.clone()));
        World::restore_keyframe(restore_config, world_bytes, &lookup)
    };
    sim.world = match world { Ok(w) => w, Err(e) => return fail(SimError::Keyframe(e.to_string()), sim) };
    if sim.world.tick().0 != sys.tick { return fail(SimError::Keyframe("world and systems disagree on the tick".into()), sim); }

    // Ships: designs from the cache, graphs recompiled from the saved live settings and rules.
    let cat = sim.deps.catalogue.clone();
    let mut last: Option<EntityId> = None;
    for f in sys.control.ships {
        if last.is_some_and(|l| f.vessel <= l) { return fail(SimError::Keyframe("ships are not in ascending order".into()), sim); }
        last = Some(f.vessel);
        // A player's ship outlives its hull (the wreck despawns, the seat and its result stay); any other ship
        // leaves the table with its hull, so one without a hull is damage.
        if f.vessel.is_none() || (f.player.is_none() && super::step::hull(&sim.world, f.vessel).is_none()) { return fail(SimError::Keyframe("a ship has no hull".into()), sim); }
        let Some(design) = sim.deps.designs.get(f.design) else { return fail(SimError::Keyframe("a ship's design was not supplied".into()), sim) };
        let ruleset = sim.compile_ruleset(f.player);
        let settings = f.settings.into_owned();
        let rules = f.rules.into_owned();
        let graph = if design.compiled_for(&ruleset) && settings == design.settings && rules == design.rules { design.graph.clone() } else { design.recompile(&settings, &rules, &design.overrides, &cat, &ruleset) };
        let mut graph_state = crate::graph::vm::init_state(&graph, None);
        let mem = f.mem.into_owned();
        if mem.len() == graph_state.mem.len() { graph_state.mem = mem; }
        graph_state.greyed = f.greyed.into_owned();
        graph_state.scoped = f.scoped.into_owned();
        let fleet_doctrine = f.fleet_doctrine.into_owned();
        let doctrine = crate::doctrine::resolve(&fleet_doctrine, &settings, &design.mounts, &cat.tuning);
        sim.ships.ships.push(Ship {
            vessel: f.vessel, side: f.side, player: f.player, design, settings, rules, graph, graph_state, fleet_doctrine, doctrine, orders: f.orders.into_owned(), control: f.control.into_owned(),
            stages: f.stages.into_owned(), offboard: f.offboard.into_owned(), proficiency: f.proficiency, captain: f.captain.into_owned(), sinks: Default::default(),
            part_command: f.part_command.into_owned(), board: f.board.into_owned(), detections: f.detections.into_owned(), tag: f.tag.into_owned(),
        });
    }

    // Plain state.
    sim.players = sys.control.players.into_owned();
    sim.pending = sys.control.pending.into_owned();
    sim.time = sys.control.time.into_owned();
    sim.ladder = sys.control.ladder;
    sim.alerts = sys.control.alerts.into_owned();
    sim.history = sys.control.history.into_owned();
    sim.shot_log = sys.control.shot_log.into_owned();
    sim.work = sys.control.work;
    sim.sensors.sensors = sys.sensors.sensors.into_owned();
    sim.sensors.pairs_last_tick = sys.sensors.pairs_last_tick;
    {
        // A sensor whose carrier is not in the restored world would never be refreshed.
        let world = &sim.world;
        sim.sensors.sensors.retain(|s| world.locate(s.id.carrier).is_some());
        sim.sensors.sensors.sort_by_key(|s| s.id);
        sim.sensors.sensors.dedup_by_key(|s| s.id);
    }
    sim.detections = sys.sensors.detections.into_owned();
    sim.links = sys.sensors.links.into_owned();
    sim.pictures = sys.tracks.pictures.into_owned();
    sim.ident = sys.tracks.ident.into_owned();
    sim.guidance = sys.guidance.guidance.into_owned();
    sim.guidance.guided.sort_by_key(|g| g.body);
    sim.guidance.guided.dedup_by_key(|g| g.body);
    let p = sys.ai.planner;
    let mut planner = if p.has_grid { crate::ai::path::PathPlanner::new(sim.world.terrain()) } else { crate::ai::path::PathPlanner::default() };
    for (beam_dm, draft_dm) in p.masks.into_iter().take(64) {
        // Half a decimetre under the key, so the key the planner derives is the saved one.
        let m = |dm: u32| if dm == 0 { 0.0 } else { (dm as f64 - 0.5) / 10.0 };
        planner.ensure_mask(m(beam_dm), m(draft_dm));
    }
    planner.queue = p.queue.into_owned();
    planner.done = p.done.into_owned();
    planner.debt = p.debt;
    planner.next_id = p.next_id;
    planner.pending = p.pending.into_owned();
    planner.latest = p.latest.into_owned();
    sim.ai = AiState { commanders: sys.ai.commanders.into_owned(), groups: sys.ai.groups.into_owned(), planner, aces: sys.ai.aces.into_owned(), air: sys.ai.air.into_owned(), mods: sys.ai.mods.into_owned(), mem: sys.ai.mem.into_owned() };
    sim.objectives = sys.objectives.objectives.into_owned();
    sim.specops = sys.objectives.specops.into_owned();
    sim.schedule = sys.objectives.schedule.into_owned();
    sim.interest = sys.objectives.interest.into_owned();
    sim.result = sys.objectives.result.into_owned();
    sim.splash_log = sys.objectives.splash_log.into_owned();
    sim.run = sys.objectives.run.into_owned();

    // Derived state: scripts back into the host, signatures for whoever reads the Sim before its next step.
    let vessels: Vec<EntityId> = sim.ships.ships.iter().map(|s| s.vessel).collect();
    for v in vessels {
        let greyed = sim.ships.get(v).map(|s| s.graph_state.greyed.clone()).unwrap_or_default();
        sim.load_scripts(v);
        // A restore announces nothing, and a script that loads again does not un-grey itself. The greyed
        // list is put back exactly as saved, so the restored Sim hashes as the saved one did.
        if let Some(s) = sim.ships.get_mut(v) { s.graph_state.greyed = greyed; }
    }
    sim.out.clear();
    {
        let env = super::step::TickEnv::of(&sim);
        let ctx = env.ctx(sim.world.tick());
        sim.sigs.update(&sim.world, &sim.ships, &ctx);
    }
    Ok(sim)
}

/// Encode and decode a config for `SortieCheckpoint::config` (bincode). Unit: `sys-sim-keyframe`.
///
/// Varint bincode, as every other stored form in this crate. Encoding a config cannot fail; were it to, the
/// empty result decodes as an error.
pub fn encode_config(config: &SortieConfig) -> Vec<u8> { bincode::DefaultOptions::new().serialize(config).unwrap_or_default() }
/// Decoding is size-limited and rejects trailing bytes; the config is validated by `Sim::restore_keyframe`.
pub fn decode_config(bytes: &[u8]) -> Result<SortieConfig, SimError> { decode(bytes, MAX_CONFIG_BYTES, "config") }

/// Ring of recent keyframes for the splash rewind: one per second, 30 kept. Only filled in Splash and
/// Tutorial modes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RewindBuffer { pub every_ticks: u32, pub keep: usize, pub frames: Vec<(u64, Vec<u8>)>,
    /// The basin as it stood at its first step: the restart point (`Command::SplashRestart`). Added by
    /// `sys-sim-keyframe` (additive).
    pub(crate) start: Option<(u64, Vec<u8>)> }
impl RewindBuffer { pub fn new() -> Self { Self { every_ticks: 60, keep: 30, frames: Vec::new(), start: None } } }
