//! Unit `sys-sim` (hard: system order, parallel fan-out with serial merge, determinism). The top-level
//! simulation of one sortie: owns the `World` and every system, applies commands, steps, and gives each viewer
//! a snapshot and events. The same `Sim` runs in the headless server, in single-player (in-process server
//! over the in-memory transport), on a listen host, in Splash, in trials and under bots (L899).
//! Sibling units share the [`Sim`] struct below as contract: `setup.rs` = `sys-sim-setup`,
//! `events.rs` = `sys-sim-events`, `interest.rs` = `sys-interest` (hard), `snapshot.rs` = `sys-snapshot`,
//! `keyframe.rs` = `sys-sim-keyframe` (hard).
//! DESIGN.md L400-408 (pipeline stages and rates), L416 (tick split), L899-926 (server authority, wire
//! content), L1150-1158 (determinism), L1413-1423 (rates).
//!
//! # Driver contract
//! The driver (server loop, gdclient sim thread, trial runner) owns real time. Each real tick it calls, in
//! order: [`Sim::set_host_load`] with its measured tick time and backlog, [`Sim::set_input`] and
//! [`Sim::apply`] with what arrived, then [`Sim::step`] `pacing().effective.factor()` times (scaled by
//! `pacing().time_scale`, zero times while paused), then [`Sim::drain_events`], and at 30 Hz
//! [`Sim::snapshot`] per viewer. `Sim` never reads a clock and never spawns threads; `step` must be called
//! from inside the sim rayon pool (3 workers, L1084).
//!
//! # Tick order (contract; golden hashes depend on it)
//! `Sim::step` runs these phases in this order. Rates use `Tick::is_due` with the constants in
//! `sim_core::time`. Parallel phases fan out per vessel and merge serially in ascending vessel slot.
//!  1. Commands queued by `apply`, sorted by (player slot, seq): ship commands through `input::apply_command`,
//!     the rest (time, alerts, actions, queries, splash) here. Rejections become `CommandRejected`.
//!  2. Schedule: vessels and bodies whose `arrives_s` is due spawn in config order; scheduled weather turns.
//!  3. Load: `lod::step_ladder` from the host load, `lod::assign_fidelity` to `World::set_fidelity`, projectile
//!     cap rung, `timeflow::update` with `timeflow::safety_test`.
//!  4. Signature: `SignatureTable::update` (60 Hz).
//!  5. Detection: `SensorScheduler::refresh` and `step` (each sensor at its own staggered 2 to 10 Hz).
//!  6. Tracks at 10 Hz, per side ascending: `datalink::membership`, `tracks::fuse`, `ident::apply_evidence`,
//!     `ident::stance::evaluate`; captains get percepts.
//!  7. Enemy AI: `ai::step` (writes orders of AI ships).
//!  8. Control at 30 Hz: ships whose compiled graph has no enabled script node tick in parallel
//!     (`StageRunner` + `graph::vm::run_tick` with `scripts = None`); ships with script nodes then tick
//!     serially in ascending vessel order with the one `ScriptHost`; then serially per ship
//!     `ship::controls::build_controls`, `World::set_controls`, and the side effects (stance and ID overrides,
//!     launches bound for guidance).
//!  9. Guidance at 30 Hz: `GuidanceTable::step` to `World::steer_body`; `offboard::step`; aircraft steering.
//! 10. Special operations: `specops::step`.
//! 11. Physics: `World::step`.
//! 12. Events: `Sim::process_phys_events` (ledger and kills, fired-on stance, guidance and sensor
//!     registration of new bodies, ship table upkeep on loss and split, shot log, AI percepts), then
//!     `InterestState::update` and audience routing into the out queue.
//! 13. Outcome and reporting: `objectives::evaluate`, `alerts` update, Bridge Boards at 10 Hz, pose history,
//!     the splash rewind buffer, and in a splash `editor::splash::observe` (report card bookkeeping).
//! 14. Work counters stored; tick counter increments (inside `World::step`).
mod commands;
pub mod events;
pub mod interest;
pub mod keyframe;
pub mod setup;
pub mod snapshot;
mod step;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod combat_trace;
#[cfg(test)]
mod lethality;

use crate::ai::AiState;
use crate::alerts::AlertManager;
use crate::ctx::{Budgets, SystemsWork};
use crate::datalink::LinkPicture;
use crate::guidance::missile::GuidanceTable;
use crate::ident::IdentLibrary;
use crate::input::PoseHistory;
use crate::lod::{HostLoad, LadderState, LadderTuning};
use crate::objectives::ObjectivesState;
use crate::prelude::*;
use crate::protocol::command::{CommandEnvelope, PlayerInput};
use crate::protocol::config::{SortieConfig, SortieResult};
use crate::protocol::event::{OutEvent, ShotLogEntry};
use crate::protocol::snapshot::{Pacing, Snapshot, Viewer};
use crate::script::ScriptHost;
use crate::sensors::{Detection, SensorScheduler};
use crate::ship::design::DesignCache;
use crate::ship::ShipTable;
use crate::signature::SignatureTable;
use crate::specops::SpecOpsState;
use crate::timeflow::TimeState;
use crate::tracks::Pictures;
use content::campaign::CampaignData;
use sim_core::hash::StateHash;

/// Shared, immutable or host-supplied things a `Sim` needs besides its config.
pub struct SimDeps {
    pub catalogue: Arc<Catalogue>,
    pub campaign: Arc<CampaignData>,
    /// Designs already built (Splash passes the pre-built design; sorties reuse class designs).
    pub designs: DesignCache,
    pub scripts: Box<dyn ScriptHost>,
    /// The L4 guest-script gate has passed for this build (L2144).
    pub guest_gate_passed: bool,
    pub budgets: Budgets,
    pub ladder: LadderTuning,
}

#[derive(Debug, thiserror::Error)]
pub enum SimError {
    #[error("sortie config protocol {found} does not match this build's {expected}")]
    Protocol { found: u32, expected: u32 },
    #[error("blueprint missing or invalid: {0}")]
    Blueprint(String),
    #[error("config invalid: {0}")]
    Config(String),
    #[error("keyframe: {0}")]
    Keyframe(String),
}

/// Connection state of one player slot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerState { pub slot: PlayerSlot, pub client_id: u64, pub name: String, pub side: SideId, pub vessel: Option<EntityId>, pub human: bool, pub connected: bool, /// Tick the player dropped; the slot is held for 5 minutes (L838).
    pub dropped_tick: Option<u64> }

/// Scheduled spawns not yet due: indices into `SortieConfig::vessels` and `bodies`, ascending.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Schedule { pub vessels: Vec<u16>, pub bodies: Vec<u16>, pub weather: Vec<u16>, /// Entity spawned for each config vessel index so far.
    pub spawned: BTreeMap<u16, EntityId> }

pub struct Sim {
    pub(crate) config: SortieConfig,
    pub(crate) deps: SimDeps,
    pub(crate) world: World,
    pub(crate) ships: ShipTable,
    pub(crate) players: Vec<PlayerState>,
    pub(crate) schedule: Schedule,
    pub(crate) sigs: SignatureTable,
    pub(crate) sensors: SensorScheduler,
    /// Detections of the current track period, ascending (sensor, target).
    pub(crate) detections: Vec<Detection>,
    pub(crate) pictures: Pictures,
    pub(crate) links: Vec<LinkPicture>,
    pub(crate) ident: IdentLibrary,
    pub(crate) guidance: GuidanceTable,
    pub(crate) ai: AiState,
    pub(crate) specops: SpecOpsState,
    pub(crate) objectives: ObjectivesState,
    pub(crate) alerts: AlertManager,
    pub(crate) ladder: LadderState,
    pub(crate) host_load: HostLoad,
    pub(crate) time: TimeState,
    pub(crate) history: PoseHistory,
    pub(crate) shot_log: Vec<ShotLogEntry>,
    pub(crate) interest: interest::InterestState,
    pub(crate) rewind: keyframe::RewindBuffer,
    pub(crate) pending: Vec<CommandEnvelope>,
    pub(crate) out: Vec<OutEvent>,
    pub(crate) work: SystemsWork,
    pub(crate) result: Option<SortieResult>,
    /// Splash basins only: what the report card needs that the sortie stats do not keep (first hit, peak
    /// heel, hold reasons seen). `Sim::new` creates it when `config.splash` is set; phase 13 of `step` calls
    /// `crate::editor::splash::observe` once per tick. Added by `sys-splash` (additive).
    pub(crate) splash_log: Option<crate::editor::splash::SplashLog>,
    /// Bookkeeping of the tick loop itself (unit `sys-sim`): what one phase must remember for a later phase or
    /// a later tick. Serialised with keyframes and covered by the state hash. Added by `sys-sim` (additive).
    pub(crate) run: RunState,
}

impl Sim {
    /// Queue commands for the next `step` (or for resume while paused, L112). Real code.
    pub fn apply(&mut self, commands: &[CommandEnvelope]) { self.pending.extend_from_slice(commands); }
    /// Latest continuous input of a player. Unit: `sys-sim`.
    ///
    /// Input from the wire is cleaned first: a non-finite axis, aim or camera position reads as zero. Input
    /// for a player without a hull, or for a human who dropped and has not rejoined, is ignored.
    pub fn set_input(&mut self, player: PlayerSlot, input: PlayerInput) {
        let Some(ps) = self.players.iter().find(|p| p.slot == player) else { return };
        if ps.human && !ps.connected { return; }
        let fin = |v: f32| if v.is_finite() { v } else { 0.0 };
        let mut input = input;
        input.throttle = fin(input.throttle).clamp(-1.0, 1.0);
        input.rudder = fin(input.rudder).clamp(-1.0, 1.0);
        if input.aim_local.iter().any(|c| !c.is_finite()) { input.aim_local = [0.0; 3]; }
        if input.camera_pos.iter().any(|c| !c.is_finite()) { input.camera_pos = [0.0; 3]; }
        if let Some(ship) = self.ships.of_player_mut(player) { crate::input::set_input(&mut ship.control, input); }
    }
    /// Host timing measured by the driver; the only wall-clock input. Real code.
    pub fn set_host_load(&mut self, load: HostLoad) { self.host_load = load; }
    /// Advance one 60 Hz tick in the documented order. Does nothing while paused except applying queued
    /// time and alert commands. Unit: `sys-sim`.
    pub fn step(&mut self) {
        // Basins keep a restart point and a ring of rewind frames, taken before this step changes anything.
        if matches!(self.config.mode, GameMode::Splash | GameMode::Tutorial) { self.note_rewind_frame(); }
        self.run_commands();
        if self.time.paused {
            // Pacing still reaches the HUD while the clock stands (pause, a rate request).
            self.note_pacing();
            return;
        }
        self.run_tick();
    }
    /// Take the events queued since the last drain, in emission order. Real code.
    pub fn drain_events(&mut self) -> Vec<OutEvent> { core::mem::take(&mut self.out) }
    /// What a viewer may know now. Never includes truth the viewer's side has not sensed.
    pub fn snapshot(&self, viewer: Viewer) -> Snapshot { snapshot::build(self, viewer) }
    pub fn pacing(&self) -> Pacing { crate::timeflow::pacing(&self.time, &self.ladder, self.host_load.tick_p95_ms) }

    pub fn tick(&self) -> Tick { self.world.tick() }
    pub fn config(&self) -> &SortieConfig { &self.config }
    pub fn world(&self) -> &World { &self.world }
    pub fn ships(&self) -> &ShipTable { &self.ships }
    pub fn pictures(&self) -> &Pictures { &self.pictures }
    pub fn players(&self) -> &[PlayerState] { &self.players }
    pub fn work(&self) -> &SystemsWork { &self.work }
    /// Count the items of a snapshot the driver built this tick toward `SystemsWork::snapshot_items`
    /// (`snapshot::snapshot_items`): the replication share of the tick's work units. The count starts again
    /// at each step and is not part of the state hash, since it depends on who is watching. Added by
    /// `sys-sim` (additive).
    pub fn note_snapshot_items(&mut self, items: u32) { self.work.snapshot_items = self.work.snapshot_items.saturating_add(items); }
    pub fn shot_log(&self) -> &[ShotLogEntry] { &self.shot_log }
    /// True once the sortie is decided; `result` is then available.
    pub fn is_over(&self) -> bool { self.result.is_some() }
    pub fn result(&self) -> Option<&SortieResult> { self.result.as_ref() }
    /// End the sortie now as withdrawn or aborted (host quit, forced end) and build the result.
    /// Unit: `sys-sim`.
    ///
    /// A sortie already decided keeps its result: the first outcome stands.
    pub fn finish(&mut self, outcome: crate::protocol::config::Outcome) -> &SortieResult {
        self.conclude(outcome);
        // `conclude` always leaves a result behind.
        self.result.get_or_insert_with(|| crate::objectives::build_result(&self.objectives, &self.world, &self.ships, &self.specops, &self.ai.commanders, &self.ai.aces, &self.config, self.ladder.highest_rung))
    }

    /// Combined state hash: sim-core's world hash plus every system's state through
    /// `sim_core::hash::StateHasher` in struct field order. Golden tests commit it. Unit: `sys-sim`.
    ///
    /// Covers exactly what a keyframe carries (`keyframe.rs` feeds the hasher from the same view it saves),
    /// so a restored Sim hashes the same as the one that was saved. Left out, as in a keyframe: graph value
    /// slots and the last sink writes (recomputed at each ship's next graph tick), signatures (rebuilt every
    /// tick), queued out events, the host load and the rewind ring.
    pub fn state_hash(&self) -> StateHash {
        let mut h = sim_core::hash::StateHasher::new();
        h.write_u64(sim_core::hash::state_hash(&self.world).0);
        for (_, part) in keyframe::system_hashes(self) { h.write_u64(part.0); }
        h.finish()
    }
    /// Per-subsystem hashes (world subsystems, then sensors, tracks, control, guidance, ai, objectives) for
    /// the nightly cross-platform comparison (L1162-1168). Unit: `sys-sim`.
    pub fn subsystem_hashes(&self) -> Vec<(&'static str, StateHash)> {
        let w = sim_core::hash::subsystem_hashes(&self.world);
        let world = [w.entities, w.bodies, w.cells, w.compartments, w.networks_parts, w.projectiles, w.munitions, w.crew, w.environment];
        let mut out: Vec<(&'static str, StateHash)> = sim_core::hash::SubsystemHashes::NAMES.iter().copied().zip(world).collect();
        out.extend(keyframe::system_hashes(self));
        out
    }
}

/// What the tick loop remembers between phases and ticks. Every field is plain data, saved in keyframes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct RunState {
    /// Last tick's physical events that special operations read (phase 10 runs before physics).
    pub(crate) prev_events: Vec<sim_core::events::PhysEvent>,
    /// Guided launches the graph ordered whose bodies have not spawned yet.
    pub(crate) launches: Vec<step::LaunchBind>,
    /// One-shot part orders from outside the graph, not yet consumed by sim-core.
    pub(crate) orders: Vec<step::HeldOrder>,
    pub(crate) rhibs: Vec<step::RhibTask>,
    /// Ascending vessel.
    pub(crate) reports: Vec<step::ShipReport>,
    /// Tick of the last ESM bearing on a track: (side, track, tick).
    pub(crate) esm_heard: Vec<(SideId, TrackId, u64)>,
    /// Scheduled vessels called forward: (config index, arrival tick).
    pub(crate) hastened: Vec<(u16, u64)>,
    /// Pacing as last sent to clients (without the measured tick time).
    pub(crate) pacing: Option<Pacing>,
    /// Last sequence number of the commands the Sim handles itself, per player, ascending slot.
    pub(crate) direct_seq: Vec<(PlayerSlot, u32)>,
    /// The clearance masks of scheduled groups have been built (done before the first tick).
    pub(crate) arrivals_ready: bool,
}

// Compile-time contract: a Sim moves to the sim thread.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Sim>();
};
