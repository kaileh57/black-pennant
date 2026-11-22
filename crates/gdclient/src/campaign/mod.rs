//! Unit `gd-campaign`. `BpCampaign` (autoload `Campaign`): the theatre layer for the chart table, briefing,
//! cove and after-action screens. DESIGN.md "Campaign" (L729-896): L62-73 (core loop), L741 (time advances
//! only on a committed transit, refit or wait, in animated 1-hour steps until the next decision or
//! interrupt), L749-750 (regional Alert, operation clock), L772 (doctrine bulletin), L801-807 (refits),
//! L811-822 (economy), L826-828 (failure), L832-845 (co-op: proposals commit when all accept or the host
//! confirms; shared pool), L847-860 (difficulty), L750 (Open Theatre after the Act 4 capstone).
//! Sibling unit: `saves.rs` = `gd-campaign-saves` (slots, suspend, Ironman, copies).
//!
//! All rules live in `sim_systems::theatre::Theatre`. On the host and in single-player this class owns the
//! `Theatre` on the main thread (it is cheap and not part of the sortie simulation) and broadcasts views to
//! co-op members; on a co-op client it is a mirror fed by the `ClientEvent::{Chart, Briefing, Theatre,
//! TheatreRejected, AfterAction}` entries of `Services::feed`, and every decision method sends
//! `ClientMsg::Theatre(TheatreCommand)` instead. GDScript cannot tell the two apart.
//!
//! Co-op traffic on the host (L832-845): members' commands arrive as
//! `HostNote::Session(HostEvent::Theatre { slot, command })`; the answers go out as
//! `HostControl::Send { to, msg }` with `net::msg::ServerMsg::{Theatre, TheatreRejected, Chart, Briefing,
//! AfterAction}`. Save copies go as `HostControl::SendBulk` with `TransferKind::SaveCopy(sequence)` (bincode
//! of `SaveGame`) and arrive on members as `ClientEvent::BulkReceived`; a 60 s `HostNote::Checkpoint` is
//! bundled with the current save by `net::coop::CoopHost::bundle` (`session::Coop::Host`) and sent as
//! `TransferKind::Checkpoint(tick)`, arriving on members as `ClientEvent::Checkpoint`. A member's copy of a
//! save is compared with `net::coop::compare_saves` before it replaces the local one (L843).
//!
//! Sortie hand-off: `launch_sortie` asks the theatre for the `SortieConfig` and leaves it in
//! `Services::handoff`; when the session ends, `process` takes the result, applies it and emits
//! `after_action_ready`.
//!
//! # Decisions where the contract is silent
//! - A committed transit, wait or refit starts theatre time at once (`advance` then just reports true):
//!   the spec says time moves when the player commits, and a co-op proposal can commit on another player's
//!   vote. A refit committed from the open dry dock starts its time when the dock closes.
//! - `commit_refit` charges the refit and then commits the refit time action (co-op: proposes it, or
//!   accepts an open refit proposal), since refit time passes only through `TheatreAction::Refit`.
//! - In co-op a refit the shared pool cannot pay for is refused, as `Theatre` rules ("a refit commits
//!   directly when the shared pool can pay"); it is not turned into a proposal.
//! - `open_briefing` in single-player accepts the operation if it is not accepted yet; in co-op it proposes
//!   (or seconds) the accept and opens the briefing locally, and `launch_sortie` waits for the accept.
//! - The sortie fields every connected player; a design that fails the act's campaign ruleset blocks the
//!   launch (parts already fitted stay legal, so a starter that carries a later-tier part still sails).
//! - A sortie the session never starts or loses (state Failed, or Idle with nothing handed back) counts as
//!   aborted: the operation stays on offer. So does a finished sortie whose full result does not arrive
//!   within [`RESULT_WAIT_S`].
//! - Quitting mid-sortie (`Session.leave`) writes the suspend slot and closes the campaign to the main menu,
//!   whose Resume button reads `suspend_slot()`.
//! - The theatre log keeps the newest [`model::LOG_CAP`] events and leaves out hour ticks, interrupts and
//!   store, supply, patrol and tally drift, which have their own signals and displays.
//! - A full save is written automatically whenever the theatre settles between operations (after an
//!   interrupt, a service, a refit or a vote; at most every [`AUTOSAVE_MIN_S`]), and at once after a result.
//! - Animation runs at most [`MAX_HOURS_PER_FRAME`] theatre hours in one frame, so a long frame never
//!   stalls the main thread.
pub mod saves;
mod coop;
mod dicts;
mod model;
mod store;
mod words;
pub(crate) use words::end_reason_msg;
#[cfg(test)]
mod tests;

/// Every text key the campaign facade emits (for `Tx.all_keys`).
pub(crate) fn text_keys() -> Vec<String> { words::text_keys() }

use crate::enums::{CampaignPhase, GdEnum, SessionKind, SessionState};
use crate::services::{services, Services};
use content::ids::{OperationId, PartId, PlayerSlot};
use content::save::{TheatreAction, TheatreState};
use godot::prelude::*;
use sim_systems::protocol::{Msg, Outcome};
use sim_systems::theatre::{AfterAction, Briefing, ChartView, Interrupt, Proposal, Theatre, TheatreCommand, TheatreData, TheatreEvent, TheatreReject};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Seconds a finished sortie waits for the host's full result before it counts as aborted.
pub(crate) const RESULT_WAIT_S: f64 = 5.0;
/// Seconds after a launch before an idle session with nothing handed back counts as a failed start.
pub(crate) const START_WAIT_S: f64 = 3.0;
/// Fewest seconds between two automatic saves.
pub(crate) const AUTOSAVE_MIN_S: f64 = 2.0;
/// Most theatre hours animated in one frame.
pub(crate) const MAX_HOURS_PER_FRAME: f64 = 24.0;
/// The starter hull a new campaign uses when none is chosen: the 22 m fast attack craft.
pub(crate) const DEFAULT_STARTER: &str = "starters/fac22.bp.ron";

/// Where the theatre state comes from.
pub enum Backend {
    /// Single-player and co-op host.
    Local(Box<Theatre>),
    /// Co-op client: the last views the host sent.
    Remote { chart: Option<Box<ChartView>>, briefings: Vec<Briefing>, resources: content::save::Resources },
}

/// A signal waiting for the end of the call (emitted once no borrow is held).
pub(crate) enum Sig {
    Chart,
    Resources,
    Phase(i64),
    Hour(u64),
    Event(Box<TheatreEvent>, u64),
    Interrupted(Interrupt),
    Proposal(Box<Proposal>, &'static str),
    Rejected(Msg),
    AfterAction,
    Over,
    Saved(Box<store::SlotInfo>),
}

/// What the facade tracks beside the theatre (none of it is saved).
#[derive(Default)]
pub(crate) struct CampaignRun {
    pub sigs: Vec<Sig>,
    pub log: model::TheatreLog,
    pub played: model::PlayedClock,
    pub now_s: f64,
    /// The time animation is paused (mirrors `Theatre`'s clock flag).
    pub clock_paused: bool,
    /// The operation whose briefing is open.
    pub briefed: Option<OperationId>,
    /// Phase to return to when the dry dock closes; whether the editor was seen open.
    pub dock_return: Option<CampaignPhase>,
    pub dock_seen_open: bool,
    pub dock_opened_s: f64,
    /// A refit committed from the dock: start its time when the dock closes.
    pub refit_waiting: bool,
    /// SEXTANT is on passage (a transit committed and not yet arrived).
    pub under_way: bool,
    /// Open proposals by id, kept so later answers can be shown with their action.
    pub proposals: BTreeMap<u32, Proposal>,
    /// The launched sortie: when, at which theatre hour, whether the session was seen running.
    pub sortie: Option<SortieWatch>,
    /// The suspend file this sortie was resumed from. It is deleted once the resumed sortie shows its first
    /// picture, so a resume that fails keeps the sortie; quitting clears it with the rest of the run.
    pub resumed_from: Option<std::path::PathBuf>,
    /// Theatre hour the after-action report's first events happened at.
    pub report_hour: u64,
    /// Parts made fieldable by the last operation.
    pub recent_unlocks: Vec<PartId>,
    pub autosave_due: bool,
    pub last_autosave_s: f64,
    /// `new_campaign` option kept for the briefing screens; the theatre offers no tutorial operations.
    pub tutorial_done: bool,
    pub coop: coop::CoopState,
    pub slot_cache: RefCell<store::SlotCache>,
    /// Briefing Dictionaries built at one revision, by operation (generation and contour tracing take
    /// about 15 ms each, and a briefing is fixed for the state).
    pub briefing_cache: RefCell<(u64, BTreeMap<OperationId, VarDictionary>)>,
    /// Bumped by every change of the theatre state.
    pub rev: u64,
    /// Fit Check of the fielded hull against the open briefing.
    pub brief_fit: BriefFit,
}

/// Fit Check of the local player's fielded hull against the open briefing (L801), run on its own thread when
/// the briefing is read and again whenever the briefing or the fielded design changes, so the briefing shows
/// tag verdicts without a trip to the dry dock. `key` is the operation and the design's content hash.
#[derive(Default)]
pub(crate) struct BriefFit {
    pub key: Option<(OperationId, content::ids::ContentHash)>,
    pub checked_rev: Option<u64>,
    pub result: Option<sim_systems::editor::fitcheck::FitResult>,
    pub rx: Option<std::sync::mpsc::Receiver<sim_systems::editor::fitcheck::FitResult>>,
    /// Stops the running check: Fit Check's seakeeping sweep takes seconds, and it runs only while the
    /// briefing is open, never while a sortie ticks (L1084).
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}
impl BriefFit {
    /// Stop the running check, if any, and forget it so the briefing runs it again when it next opens.
    fn stop(&mut self) {
        if let Some(c) = self.cancel.take() { c.store(true, std::sync::atomic::Ordering::Relaxed); }
        if self.rx.take().is_some() { self.key = None; }
    }
}
impl Drop for BriefFit { fn drop(&mut self) { self.stop(); } }

/// A Fit Check deadline that expires when the briefing's check is stopped.
struct Stopped(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl sim_systems::editor::Deadline for Stopped { fn expired(&mut self) -> bool { self.0.load(std::sync::atomic::Ordering::Relaxed) } }

pub(crate) struct SortieWatch { pub launched_s: f64, pub hour: u64, pub seen_running: bool, pub finished_s: Option<f64> }

/// The campaign facade. Autoload `Campaign`.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct BpCampaign {
    pub(crate) backend: Option<Backend>,
    pub(crate) phase: CampaignPhase,
    pub(crate) after_action: Option<Box<AfterAction>>,
    pub(crate) advancing: bool,
    pub(crate) advance_accum_s: f64,
    pub(crate) hours_per_s: f64,
    pub(crate) slot_path: String,
    pub(crate) run: CampaignRun,
    base: Base<Node>,
}

#[godot_api]
impl INode for BpCampaign {
    fn init(base: Base<Node>) -> Self {
        Self { backend: None, phase: CampaignPhase::None, after_action: None, advancing: false, advance_accum_s: 0.0, hours_per_s: 6.0, slot_path: String::new(), run: CampaignRun::default(), base }
    }
    fn ready(&mut self) { self.base_mut().set_process_priority(-90); }
    /// Unit `gd-campaign`: while advancing, step theatre hours at `hours_per_s` and emit their events; apply
    /// co-op messages from `Services::feed`; collect a finished sortie from `Services::handoff`. Inert with
    /// no campaign loaded.
    fn process(&mut self, delta: f64) {
        let services = services();
        let delta = if delta.is_finite() { delta.clamp(0.0, 1.0) } else { 0.0 };
        self.run.now_s += delta;
        self.coop_frame(&services);
        if self.backend.is_some() {
            let busy = self.advancing || self.phase == CampaignPhase::Sortie;
            let played = self.run.played.tick(delta, busy);
            if played > 0 { if let Some(t) = self.local_mut() { t.add_played_s(played); } }
            self.watch_sortie(&services);
            self.watch_dock();
            self.poll_brief_fit();
            if self.advancing { self.step_time(delta); }
            self.maybe_autosave(false);
        } else {
            // A finished sortie of a campaign that is gone (closed mid-sortie) is nobody's.
            let stale = services.handoff.borrow().finished.as_ref().is_some_and(|f| f.kind == SessionKind::Campaign);
            if stale { services.handoff.borrow_mut().finished = None; }
        }
        drop(services);
        self.flush();
    }
}

#[godot_api]
impl BpCampaign {
    /// `chart()` changed.
    #[signal]
    fn chart_changed();
    /// `resources()` changed.
    #[signal]
    fn resources_changed();
    /// `phase` is `Bp.PHASE_*`.
    #[signal]
    fn phase_changed(phase: i64);
    /// One theatre hour passed during an advance (drives the chart animation).
    #[signal]
    fn hour_advanced(hour: i64);
    /// Something happened in theatre time (`TheatreEventDict`): animate it and file it in the theatre log.
    #[signal]
    fn theatre_event(event: VarDictionary);
    /// Theatre time stopped (`InterruptDict`): the player decides next.
    #[signal]
    fn interrupted(interrupt: VarDictionary);
    /// A co-op proposal was made, answered, committed or withdrawn (`ProposalDict`; `state` is `&"made"`,
    /// `&"answered"`, `&"committed"` or `&"withdrawn"`).
    #[signal]
    fn proposal_changed(proposal: VarDictionary, state: StringName);
    /// The last decision was refused; `text` is a `MsgDict`.
    #[signal]
    fn rejected(text: VarDictionary);
    /// `after_action()` is ready to show.
    #[signal]
    fn after_action_ready();
    /// Ironman: no player hull remains (L828).
    #[signal]
    fn campaign_over();
    /// A full save was written (`saves.rs`); `slot` is its `SaveSlotDict`. One of the few events that may
    /// toast (L1605).
    #[signal]
    fn saved(slot: VarDictionary);
    /// `briefing_fit()` changed: a Fit Check of the fielded hull started or finished.
    #[signal]
    fn briefing_fit_changed();

    // ---- State ----

    /// True while a campaign is loaded.
    #[func]
    fn is_active(&self) -> bool { self.backend.is_some() }
    /// `Bp.PHASE_*`.
    #[func]
    fn phase(&self) -> i64 { self.phase.to_gd() }
    /// Returns `ChartDict`. Re-read on `chart_changed`. The static graph (nodes, lanes, coastlines) is
    /// `Content.theatre_graph()`.
    #[func]
    fn chart(&self) -> VarDictionary {
        let data = theatre_data();
        let extra = dicts::ChartExtra { phase: self.phase.to_gd(), at_cove: self.at_cove(&data), can_save: self.save_gate() };
        let names = self.names(&data);
        match &self.backend {
            Some(Backend::Local(t)) => dicts::chart_dict(&t.chart(), &extra, &names),
            Some(Backend::Remote { chart: Some(c), .. }) => dicts::chart_dict(c, &extra, &names),
            _ => VarDictionary::new(),
        }
    }
    /// Returns `ResourcesDict`: the one shared pool on SEXTANT.
    #[func]
    fn resources(&self) -> VarDictionary {
        let data = theatre_data();
        let tier = self.state().map_or(content::enums::Tier::T1, |s| sim_systems::theatre::progress::open_tier(s, &data));
        match &self.backend {
            Some(Backend::Local(t)) => dicts::resources_dict(&t.state.resources, tier),
            Some(Backend::Remote { resources, .. }) => dicts::resources_dict(resources, tier),
            None => VarDictionary::new(),
        }
    }
    /// Players of this campaign: `Array[Dictionary]` with `player: int`, `name: String`, `hull_name: String`,
    /// `blueprint_hash: String`, `hull_lost: bool`, `hull_ready_h: int` (theatre hour the hull is ready, or
    /// -1 = ready), `damage: float`, `fuel: float`, `ammo: float`, `connected: bool`, `is_local: bool`.
    #[func]
    fn players(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        let Some(state) = self.state() else { return out };
        let data = theatre_data();
        let local = self.local_slot();
        let connected = self.connected_slots();
        for (i, p) in state.players.iter().enumerate() {
            let slot = PlayerSlot(i as u8);
            let h = model::hull_summary(state, slot, &data);
            let hash = if p.blueprint.header.hash == content::ids::ContentHash::EMPTY { content::blueprint::io::compute_hash(&p.blueprint) } else { p.blueprint.header.hash };
            let ready = p.ready_at_hour.filter(|h| *h > state.hour).map_or(-1, |h| h.min(i64::MAX as u64) as i64);
            out.push(&crate::convert::d! {
                "player" => i as i64, "name" => GString::from(p.name.as_str()), "hull_name" => GString::from(p.blueprint.header.name.as_str()),
                "blueprint_hash" => GString::from(hash.to_hex().as_str()), "hull_lost" => p.hull_lost, "hull_ready_h" => ready,
                "damage" => h.damage, "fuel" => h.fuel, "ammo" => h.ammo, "connected" => connected.contains(&slot), "is_local" => slot == local,
            });
        }
        out
    }
    /// The act in play: `{ "act": int, "cap_t": float, "retreat": bool, "clock_days_left": float,
    /// "story_done": int, "story_total": int, "capstone_open": bool, "starters": Array[BlueprintInfoDict],
    /// "max_tier": int, "open_theatre": bool }`.
    #[func]
    fn act_info(&self) -> VarDictionary {
        let Some(state) = self.state() else { return VarDictionary::new() };
        let data = theatre_data();
        let a = model::act_info(state, &data);
        let starters: VarArray = a.starters.iter().filter_map(|path| match content::campaign::load::stock_blueprint(path) {
            Ok(bp) => {
                let stats = content::stats::compute_stats(&bp, &data.catalogue);
                Some(crate::convert::blueprint_info_dict(&bp, &stats, &crate::content_api::model::normalize_stock(path), true, 0).to_variant())
            }
            Err(e) => { godot_warn!("campaign: starter hull {path} does not load: {e}"); None }
        }).collect();
        crate::convert::d! {
            "act" => a.act.to_gd(), "cap_t" => a.cap_t, "retreat" => a.retreat, "clock_days_left" => a.clock_days_left.unwrap_or(-1.0),
            "story_done" => a.story_done as i64, "story_total" => a.story_total as i64, "capstone_open" => a.capstone_open,
            "starters" => starters, "max_tier" => a.max_tier.to_gd(), "open_theatre" => a.open_theatre,
        }
    }
    /// The doctrine bulletin: active and queued counter cards (`Array[CardDict]`, soonest first).
    #[func]
    fn doctrine_bulletin(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        let data = theatre_data();
        let names = self.names(&data);
        let chart = match &self.backend { Some(Backend::Local(t)) => Some(t.chart()), Some(Backend::Remote { chart: Some(c), .. }) => Some((**c).clone()), _ => None };
        if let Some(c) = chart {
            for b in model::bulletin(&c) { out.push(&dicts::card_dict(&b.card, &b.region, b.arrives_hour, &names)); }
        }
        out
    }
    /// The most recent theatre events, oldest first (`Array[TheatreEventDict]`, at most `limit`).
    #[func]
    fn theatre_log(&self, limit: i64) -> Array<VarDictionary> {
        let mut out = Array::new();
        let data = theatre_data();
        let names = self.names(&data);
        let limit = usize::try_from(limit).unwrap_or(0).min(model::LOG_CAP);
        for (hour, e) in self.run.log.tail(limit) { out.push(&dicts::event_dict(e, *hour, &names)); }
        out
    }
    /// Returns `DifficultyDict` in force.
    #[func]
    fn difficulty(&self) -> VarDictionary {
        let Some(state) = self.state() else { return VarDictionary::new() };
        let data = services().campaign();
        crate::convert::difficulty_dict(&state.difficulty, &model::preset_of(&state.difficulty, &data.difficulty))
    }
    /// Change difficulty between operations. Refused under Ironman, which locks all six sliders at the start,
    /// and for `hull_count`, which locks at campaign start (L860).
    #[func]
    fn set_difficulty(&mut self, difficulty: VarDictionary) -> bool {
        let ok = self.set_difficulty_inner(&difficulty);
        self.flush();
        ok
    }

    // ---- Decisions at the chart table (L71, L839) ----

    /// Propose a transit, refit, wait, accept, decline or Intel spend (`TheatreActionDict`). In single-player
    /// it commits at once; in co-op it is shown to all and commits when every connected player accepts or
    /// the host confirms. Returns false when refused (`rejected` says why).
    #[func]
    fn propose(&mut self, action: VarDictionary) -> bool {
        let ok = match dicts::action_from_dict(&action) {
            Some(a) => self.propose_action(a),
            None => { self.reject(Msg::new("campaign.reject.bad_action")); false }
        };
        self.flush();
        ok
    }
    /// Answer a co-op proposal.
    #[func]
    fn respond(&mut self, proposal: i64, accept: bool) -> bool {
        let ok = match proposal_id(proposal) {
            Some(id) => self.command(TheatreCommand::Respond { proposal: id, accept }),
            None => { self.reject_typed(TheatreReject::NoSuchProposal); false }
        };
        self.flush();
        ok
    }
    /// Host only: commit a proposal without waiting for every answer.
    #[func]
    fn confirm(&mut self, proposal: i64) -> bool {
        let ok = match proposal_id(proposal) {
            Some(id) => self.command(TheatreCommand::Confirm(id)),
            None => { self.reject_typed(TheatreReject::NoSuchProposal); false }
        };
        self.flush();
        ok
    }
    #[func]
    fn withdraw_proposal(&mut self, proposal: i64) -> bool {
        let ok = match proposal_id(proposal) {
            Some(id) => self.command(TheatreCommand::Withdraw(id)),
            None => { self.reject_typed(TheatreReject::NoSuchProposal); false }
        };
        self.flush();
        ok
    }
    /// What a transit to a node would take: `{ "reachable": bool, "hours": int, "path": Array[StringName],
    /// "passes_cut_lane": bool, "text": MsgDict }`.
    #[func]
    fn transit_preview(&self, node: StringName) -> VarDictionary {
        let target = content::ids::TheatreNodeId::new(node.to_string());
        let data = theatre_data();
        let Some(state) = self.state() else {
            return crate::convert::d! { "reachable" => false, "hours" => 0i64, "path" => VarArray::new(), "passes_cut_lane" => false, "text" => crate::convert::msg_dict(&Msg::new("campaign.reject.no_campaign")) };
        };
        let names = self.names(&data);
        let mut p = model::transit_preview(state, &data, &target, self.run.under_way, names.node(&target));
        if p.reachable && self.busy() { p.reachable = false; p.text = Msg::new("campaign.transit.under_way"); }
        let path: VarArray = p.path.iter().map(|n| StringName::from(n.as_str()).to_variant()).collect();
        crate::convert::d! { "reachable" => p.reachable, "hours" => p.hours as i64, "path" => path, "passes_cut_lane" => p.passes_cut_lane, "text" => crate::convert::msg_dict(&p.text) }
    }
    /// Intel spends on offer now with their costs (`Array[IntelDict]`): delay a card 72 h (2), cancel a card
    /// (5), reveal a region's convoys for 4 theatre days (1), reveal an ace's location (3).
    #[func]
    fn intel_options(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        let Some(state) = self.state() else { return out };
        let data = theatre_data();
        for o in model::intel_options(state, &data) { out.push(&dicts::intel_dict(&o.spend, o.cost)); }
        out
    }

    // ---- Theatre time (L741) ----

    /// Run theatre time after a committed decision until the next decision or interrupt. Animated: emits
    /// `hour_advanced` and `theatre_event` at the advance speed, then `interrupted`.
    #[func]
    fn advance(&mut self) -> bool {
        let ok = self.start_advance();
        self.flush();
        ok
    }
    /// Pause or resume the time animation (not a simulation pause).
    #[func]
    fn set_clock_paused(&mut self, paused: bool) {
        self.run.clock_paused = paused;
        self.run.played.activity();
        if self.backend.is_some() { let _ = self.command(TheatreCommand::PauseClock(paused)); }
        self.flush();
    }
    #[func]
    fn is_advancing(&self) -> bool { self.advancing }
    /// Animation speed in theatre hours per second (1 to 48).
    #[func]
    fn set_advance_speed(&mut self, hours_per_s: f64) { self.hours_per_s = if hours_per_s.is_finite() { hours_per_s.clamp(1.0, 48.0) } else { 6.0 }; }

    // ---- Operations (L66, L776-797) ----

    /// The `BriefingDict` of an offered operation, or `null`. Deterministic for the state and operation; the
    /// forecast is the weather the sortie will have.
    #[func]
    fn briefing(&self, operation: StringName) -> Variant {
        let op = OperationId::new(operation.to_string());
        {
            let mut cache = self.run.briefing_cache.borrow_mut();
            if cache.0 != self.run.rev { *cache = (self.run.rev, BTreeMap::new()); }
            if let Some(d) = cache.1.get(&op) { return d.to_variant(); }
        }
        let Some(b) = self.briefing_of(&op) else { return Variant::nil() };
        let data = theatre_data();
        let names = self.names(&data);
        let hour = self.state().map_or(0, |s| s.hour);
        let dtg = crate::text::dtg_text(&crate::text::EngineLookup::new(), hour.min(i64::MAX as u64) as i64);
        let dict = dicts::briefing_dict(&b, &names, &dtg);
        self.run.briefing_cache.borrow_mut().1.insert(op, dict.clone());
        dict.to_variant()
    }
    /// Fit Check of this player's fielded hull against the open briefing: `FitResultDict` (its `state` is
    /// `Bp.JOB_RUNNING` while the 30 s run is under way, `Bp.JOB_IDLE` with no briefing open). Reading it
    /// starts the run when the briefing or the fielded design changed; `briefing_fit_changed` announces the
    /// result.
    #[func]
    fn briefing_fit(&mut self) -> VarDictionary {
        use crate::enums::JobState;
        let state = self.ensure_brief_fit();
        let r = self.run.brief_fit.result.as_ref();
        crate::editor::analysis::fit_result_dict(r, if r.is_none() && state == JobState::Done { JobState::Idle } else { state })
    }
    /// Open the briefing phase for an accepted operation: Fit Check and the Quartermaster read it from here.
    #[func]
    fn open_briefing(&mut self, operation: StringName) -> bool {
        let ok = self.open_briefing_inner(OperationId::new(operation.to_string()));
        self.flush();
        ok
    }
    /// The operation whose briefing is open, or `&""` outside the briefing phase. The briefing screen reads it:
    /// the screen router opens that screen on `phase_changed` with no arguments.
    #[func]
    fn briefed_operation(&self) -> StringName {
        match &self.run.briefed {
            Some(op) if self.phase == CampaignPhase::Briefing => StringName::from(op.as_str()),
            _ => StringName::default(),
        }
    }
    /// Leave the briefing for the chart table without launching. The operation stays accepted (decline it
    /// with `propose`), and `open_briefing` returns to it. Does nothing outside the briefing phase.
    #[func]
    fn close_briefing(&mut self) {
        if self.phase == CampaignPhase::Briefing {
            self.set_phase(CampaignPhase::Chart);
            self.run.played.activity();
        }
        self.flush();
    }
    /// Build the sortie for the briefed operation with the connected players and hand it to the session.
    /// Returns false when a hull is not ready or the design fails validation.
    #[func]
    fn launch_sortie(&mut self) -> bool {
        let ok = self.launch_inner();
        self.flush();
        ok
    }
    /// The `AfterActionDict` of the last sortie, or `null`.
    #[func]
    fn after_action(&self) -> Variant {
        let Some(a) = &self.after_action else { return Variant::nil() };
        let data = theatre_data();
        dicts::after_action_dict(a, self.run.report_hour, &self.names(&data)).to_variant()
    }
    /// Close the after-action report and return to the chart table.
    #[func]
    fn close_after_action(&mut self) {
        if self.phase == CampaignPhase::AfterAction {
            let over = self.local().is_some_and(sim_systems::theatre::saves::campaign_over);
            self.set_phase(if over { CampaignPhase::Over } else { CampaignPhase::Chart });
            self.run.played.activity();
        }
        self.flush();
    }

    // ---- Cove and dry dock (L801-828) ----

    /// Open the dry dock on the local player's hull in campaign mode (cap, unlocks, parts store, fielded
    /// design and current briefing are passed to `Editor`). Returns false away from a cove or SEXTANT.
    #[func]
    fn open_dry_dock(&mut self) -> bool {
        let ok = self.open_dock_inner();
        self.flush();
        ok
    }
    /// Cost of committing the design now in the dock: `RefitDiffDict` (Salvage and theatre hours: 2 h per
    /// module swap, 1 h per 10 t of structure; removed parts go to the parts store).
    #[func]
    fn refit_quote(&self) -> VarDictionary {
        let data = theatre_data();
        let empty = || dicts::refit_diff_dict(&Default::default(), &Default::default(), true, &data.catalogue);
        let Some(state) = self.state() else { return empty() };
        let Some(rec) = state.players.get(self.local_slot().0 as usize) else { return empty() };
        let Some(design) = services().design.borrow().as_ref().map(|d| d.blueprint.clone()) else { return empty() };
        let diff = content::stats::diff_blueprints(&rec.blueprint, &design, &data.catalogue);
        let cost = content::stats::refit_cost(&diff, &data.catalogue, &state.resources.parts_store);
        dicts::refit_diff_dict(&diff, &cost, state.resources.salvage >= cost.salvage, &data.catalogue)
    }
    /// Commit the refit. In co-op, overdrawing the shared pool goes through a proposal.
    #[func]
    fn commit_refit(&mut self) -> bool {
        let ok = self.commit_refit_inner();
        self.flush();
        ok
    }
    /// Quotes for the cove services: `{ "repair": { "salvage": int, "hours": int, "needed": bool },
    /// "rearm": { "salvage": int, "from_stock": bool, "needed": bool },
    /// "rebuild": { "salvage": int, "hours": int, "needed": bool, "allowed": bool } }`
    /// (rebuild: 60% of build cost and 48 theatre hours; never under Ironman).
    #[func]
    fn service_quotes(&self) -> VarDictionary {
        let data = theatre_data();
        let q = match self.state() { Some(s) => model::service_quotes(s, self.local_slot(), self.ironman(), &data), None => Default::default() };
        crate::convert::d! {
            "repair" => crate::convert::d! { "salvage" => q.repair.salvage as i64, "hours" => q.repair.hours as i64, "needed" => q.repair.needed },
            "rearm" => crate::convert::d! { "salvage" => q.rearm.salvage as i64, "from_stock" => q.rearm.from_stock, "needed" => q.rearm.needed },
            "rebuild" => crate::convert::d! { "salvage" => q.rebuild.salvage as i64, "hours" => q.rebuild.hours as i64, "needed" => q.rebuild.needed, "allowed" => q.rebuild.allowed },
        }
    }
    /// Buy a service: `&"repair"`, `&"repair_full"`, `&"rearm"` or `&"rebuild"`.
    #[func]
    fn buy_service(&mut self, service: StringName) -> bool {
        let cmd = match service.to_string().as_str() {
            "repair" => Some(TheatreCommand::Repair { full: false }),
            "repair_full" => Some(TheatreCommand::Repair { full: true }),
            "rearm" => Some(TheatreCommand::Rearm),
            "rebuild" => Some(TheatreCommand::Rebuild),
            _ => None,
        };
        let ok = match cmd {
            Some(_) if self.backend.is_none() => { self.reject(Msg::new("campaign.reject.no_campaign")); false }
            Some(_) if self.busy() => { self.reject(Msg::new("campaign.reject.busy")); false }
            Some(c) => { let ok = self.command(c); if ok { self.run.sigs.push(Sig::Resources); } ok }
            None => { self.reject(Msg::new("campaign.reject.unknown_service")); false }
        };
        self.flush();
        ok
    }
    /// Part unlock state: `{ "max_tier": int, "unlocked": Array[StringName], "recent": Array[StringName],
    /// "store": Dictionary (part id to int) }`.
    #[func]
    fn unlocks(&self) -> VarDictionary {
        let Some(state) = self.state() else { return VarDictionary::new() };
        let data = theatre_data();
        let unlocked: VarArray = state.resources.unlocked_parts.iter().map(|p| StringName::from(p.as_str()).to_variant()).collect();
        let recent: VarArray = self.run.recent_unlocks.iter().map(|p| StringName::from(p.as_str()).to_variant()).collect();
        let mut store = VarDictionary::new();
        for (p, n) in &state.resources.parts_store { store.set(&StringName::from(p.as_str()), *n as i64); }
        crate::convert::d! { "max_tier" => sim_systems::theatre::progress::open_tier(state, &data).to_gd(), "unlocked" => unlocked, "recent" => recent, "store" => store }
    }
}

// -------------------------------------------------------------------------------------------------------
// Internals
// -------------------------------------------------------------------------------------------------------

/// Read-only campaign data for the theatre functions.
pub(crate) fn theatre_data() -> TheatreData {
    let s = services();
    TheatreData { campaign: s.campaign(), catalogue: s.catalogue() }
}

fn proposal_id(v: i64) -> Option<sim_systems::ids::ProposalId> { u32::try_from(v).ok().map(sim_systems::ids::ProposalId) }

/// The install's client id (`user://identity.ron`, written by `App.client_id`); 1 when there is none, which
/// resolves to player 0 of any save.
pub(crate) fn local_client_id() -> u64 {
    use godot::classes::FileAccess;
    let file = crate::app::IDENTITY_FILE;
    if !FileAccess::file_exists(file) { return 1; }
    crate::app::parse_identity(&FileAccess::get_file_as_string(file).to_string()).unwrap_or(1)
}

/// The result of a sortie that never ran: the theatre goes back to the chart table and the offer stands.
pub(crate) fn aborted_result(t: &Theatre) -> sim_systems::protocol::SortieResult {
    let active = t.active();
    sim_systems::protocol::SortieResult {
        operation: active.map(|a| a.operation.clone()), region: active.map(|a| a.region.clone()), seed: active.map_or(0, |a| a.seed),
        outcome: Outcome::Aborted, winner: None, duration_ticks: 0, objective_done: false, ghosts: Vec::new(), kills: Vec::new(),
        survivors: Vec::new(), prizes: Vec::new(), aces: Vec::new(), players: Vec::new(), sightings: 0, evidence: 0, photos: 0, survivors_recovered: 0,
        crew_captured: 0, passengers_delivered: 0, mines_laid: 0, site_destroyed: false, tender_lost: false, highest_rung: 0, belief: Vec::new(),
        stats: Default::default(), end_reason: None,
    }
}

/// Whether an action passes theatre time.
pub(crate) fn is_time_action(a: &TheatreAction) -> bool { matches!(a, TheatreAction::Transit(_) | TheatreAction::Wait { .. } | TheatreAction::Refit) }

impl BpCampaign {
    pub(crate) fn local(&self) -> Option<&Theatre> { match &self.backend { Some(Backend::Local(t)) => Some(t), _ => None } }
    pub(crate) fn local_mut(&mut self) -> Option<&mut Theatre> { match &mut self.backend { Some(Backend::Local(t)) => Some(t), _ => None } }
    /// The ledger: the theatre's own on the host, the last save copy on a member.
    pub(crate) fn state(&self) -> Option<&TheatreState> {
        match &self.backend {
            Some(Backend::Local(t)) => Some(&t.state),
            Some(Backend::Remote { .. }) => self.run.coop.copy.as_ref().map(|s| &s.theatre),
            None => None,
        }
    }
    pub(crate) fn ironman(&self) -> bool {
        match &self.backend {
            Some(Backend::Local(t)) => t.header().ironman,
            Some(Backend::Remote { .. }) => self.run.coop.copy.as_ref().is_some_and(|s| s.header.ironman),
            None => false,
        }
    }
    pub(crate) fn names<'a>(&self, data: &'a TheatreData) -> words::Names<'a> {
        words::Names { campaign: &data.campaign, catalogue: &data.catalogue, players: self.state().map(|s| s.players.iter().map(|p| p.name.clone()).collect()).unwrap_or_default(), act: self.state().map(|s| s.act) }
    }
    /// This machine's player in the theatre.
    pub(crate) fn local_slot(&self) -> PlayerSlot {
        match self.state() { Some(s) => model::local_slot(s, self.run.coop.client_id()), None => PlayerSlot(0) }
    }
    fn at_cove(&self, data: &TheatreData) -> bool {
        self.phase != CampaignPhase::Sortie && self.state().is_some_and(|s| model::at_cove(s, data, self.run.under_way))
    }
    /// Time is running or a sortie is out: nothing at the cove can be bought or changed.
    fn busy(&self) -> bool { self.advancing || matches!(self.phase, CampaignPhase::Sortie | CampaignPhase::Advancing) }
    pub(crate) fn bump(&mut self) { self.run.rev = self.run.rev.wrapping_add(1); }

    pub(crate) fn set_phase(&mut self, p: CampaignPhase) {
        if self.phase != p {
            self.phase = p;
            self.bump();
            self.run.sigs.push(Sig::Phase(p.to_gd()));
            self.run.sigs.push(Sig::Chart);
        }
    }
    pub(crate) fn reject(&mut self, m: Msg) { self.run.sigs.push(Sig::Rejected(m)); }
    pub(crate) fn reject_typed(&mut self, r: TheatreReject) { self.reject(words::reject_msg(r)); }

    // ---- Commands ----

    /// One command from this machine's player: applied to the theatre here, or sent to the host.
    pub(crate) fn command(&mut self, cmd: TheatreCommand) -> bool {
        self.run.played.activity();
        if let TheatreCommand::PauseClock(p) = cmd { self.run.clock_paused = p; }
        if self.backend.is_none() { self.reject(Msg::new("campaign.reject.no_campaign")); return false; }
        if self.local().is_none() { return self.send_to_host(cmd); }
        let slot = self.local_slot();
        let mask = self.connected_mask();
        let r = match self.local_mut() { Some(t) => t.command(slot, true, mask, cmd), None => return false };
        self.pump();
        match r {
            Ok(()) => { self.run.autosave_due = true; true }
            Err(e) => { self.reject_typed(e); false }
        }
    }

    fn propose_action(&mut self, a: TheatreAction) -> bool {
        if self.backend.is_none() { self.reject(Msg::new("campaign.reject.no_campaign")); return false; }
        if self.busy() { self.reject(Msg::new("campaign.reject.busy")); return false; }
        let coop = self.is_coop();
        let ok = self.command(TheatreCommand::Propose(a.clone()));
        // Single-player: the action committed at once.
        if ok && !coop && self.local().is_some() { self.after_commit(&a); }
        ok
    }

    /// A committed action's follow-up on the host: passage flag, briefing cleared, time started.
    pub(crate) fn after_commit(&mut self, a: &TheatreAction) {
        if let TheatreAction::Transit(n) = a { self.run.under_way = self.state().is_some_and(|s| s.sextant_node != *n); }
        if let TheatreAction::DeclineOperation(op) = a { if self.run.briefed.as_ref() == Some(op) { self.run.briefed = None; } }
        if is_time_action(a) {
            if self.phase == CampaignPhase::DryDock { self.run.refit_waiting = true; } else { let _ = self.start_advance(); }
        }
    }

    fn set_difficulty_inner(&mut self, dict: &VarDictionary) -> bool {
        let Some(current) = self.state().map(|s| s.difficulty) else { self.reject(Msg::new("campaign.reject.no_campaign")); return false };
        let Some(d) = crate::convert::difficulty_from_dict(dict) else { self.reject(Msg::new("campaign.new.bad_difficulty")); return false };
        if self.ironman() { self.reject_typed(TheatreReject::IronmanLocked); return false; }
        if d.hull_count != current.hull_count { self.reject(Msg::new("campaign.reject.hull_count_locked")); return false; }
        if self.busy() { self.reject(Msg::new("campaign.reject.busy")); return false; }
        let ok = self.command(TheatreCommand::SetDifficulty(d));
        if ok { self.bump(); self.run.sigs.push(Sig::Chart); }
        ok
    }

    // ---- Events ----

    /// Take the theatre's queued events and turn them into log entries, signals and co-op broadcasts.
    pub(crate) fn pump(&mut self) {
        let Some(t) = self.local_mut() else { return };
        let events = t.drain_events();
        if events.is_empty() { return; }
        let hour = t.state.hour;
        self.bump();
        // Events before the batch's first hour tick happened at the hour before it.
        let mut at = events.iter().find_map(|e| if let TheatreEvent::HourAdvanced { hour } = e { Some(hour.saturating_sub(1)) } else { None }).unwrap_or(hour);
        for e in &events {
            if let TheatreEvent::HourAdvanced { hour } = e { at = *hour; }
            self.observe_event(e, at);
        }
        self.coop_broadcast_events(&events);
    }

    /// One theatre event, on the host or mirrored on a member.
    pub(crate) fn observe_event(&mut self, e: &TheatreEvent, hour: u64) {
        use TheatreEvent as E;
        match e {
            E::HourAdvanced { hour } => self.run.sigs.push(Sig::Hour(*hour)),
            E::Interrupted(i) => {
                if let Interrupt::Arrived(_) = i { self.run.under_way = false; }
                self.run.sigs.push(Sig::Interrupted(i.clone()));
            }
            E::SextantMoved { .. } => self.run.under_way = false,
            E::ResourcesChanged => self.run.sigs.push(Sig::Resources),
            E::ProposalMade(p) => {
                self.run.proposals.insert(p.id.0, p.clone());
                self.run.sigs.push(Sig::Proposal(Box::new(p.clone()), "made"));
            }
            E::ProposalAnswered { proposal, player, accept } => {
                if let Some(p) = self.run.proposals.get_mut(&proposal.0) {
                    if *accept { p.accepted = p.accepted.with(*player); }
                    let p = p.clone();
                    self.run.sigs.push(Sig::Proposal(Box::new(p), "answered"));
                }
            }
            E::ProposalCommitted { proposal } => {
                if let Some(p) = self.run.proposals.remove(&proposal.0) {
                    if self.local().is_some() { self.after_commit(&p.action); }
                    self.run.sigs.push(Sig::Proposal(Box::new(p), "committed"));
                }
            }
            E::ProposalWithdrawn { proposal } => {
                if let Some(p) = self.run.proposals.remove(&proposal.0) { self.run.sigs.push(Sig::Proposal(Box::new(p), "withdrawn")); }
            }
            E::CampaignOver => self.run.sigs.push(Sig::Over),
            _ => {}
        }
        if words::logged(e) {
            self.run.log.push(hour, e.clone());
            self.run.sigs.push(Sig::Event(Box::new(e.clone()), hour));
        }
        self.run.sigs.push(Sig::Chart);
    }

    // ---- Theatre time ----

    pub(crate) fn start_advance(&mut self) -> bool {
        match &self.backend {
            None => { self.reject(Msg::new("campaign.reject.no_campaign")); return false; }
            // Time is the host's: a member's chart animates from the host's hour events.
            Some(Backend::Remote { .. }) => return self.phase == CampaignPhase::Advancing,
            Some(Backend::Local(_)) => {}
        }
        if self.advancing { return true; }
        if !matches!(self.phase, CampaignPhase::Chart | CampaignPhase::Briefing) { self.reject(Msg::new("campaign.reject.busy")); return false; }
        self.advancing = true;
        self.advance_accum_s = 0.0;
        self.run.briefed = None;
        self.set_phase(CampaignPhase::Advancing);
        true
    }

    fn stop_advance(&mut self) {
        self.advancing = false;
        self.advance_accum_s = 0.0;
        if self.phase == CampaignPhase::Advancing { self.set_phase(CampaignPhase::Chart); }
        self.run.autosave_due = true;
    }

    /// Animate theatre time: whole hours at the advance speed, stopping at the first interrupt.
    pub(crate) fn step_time(&mut self, delta: f64) {
        if self.run.clock_paused || self.local().is_none() { return; }
        self.advance_accum_s = (self.advance_accum_s + delta * self.hours_per_s).min(MAX_HOURS_PER_FRAME);
        while self.advance_accum_s >= 1.0 {
            self.advance_accum_s -= 1.0;
            if self.step_hour() { self.stop_advance(); return; }
        }
    }

    /// Run one theatre hour; true when time stopped (an interrupt, or nothing left to run).
    pub(crate) fn step_hour(&mut self) -> bool {
        let Some(t) = self.local_mut() else { return true };
        let before = t.state.hour;
        let stop = t.advance(1);
        let moved = t.state.hour != before;
        let mut events = t.drain_events();
        let interrupted = events.iter().any(|e| matches!(e, TheatreEvent::Interrupted(_)));
        // Nothing ran (no time action left, a sortie out, the clock paused elsewhere): say why.
        if !moved && !interrupted { events.push(TheatreEvent::Interrupted(stop)); }
        self.bump();
        let hour = self.local().map_or(before, |t| t.state.hour);
        for e in &events { self.observe_event(e, hour); }
        self.coop_broadcast_events(&events);
        interrupted || !moved
    }

    // ---- Operations ----

    fn briefing_of(&self, op: &OperationId) -> Option<Briefing> {
        match &self.backend {
            Some(Backend::Local(t)) => t.briefing(op),
            Some(Backend::Remote { briefings, .. }) => briefings.iter().find(|b| &b.operation == op).cloned(),
            None => None,
        }
    }

    /// Accept an operation: committed in single-player; in co-op proposed, or seconded when proposed already.
    fn vote_accept(&mut self, op: &OperationId) -> bool {
        let open = self.run.proposals.values().find(|p| p.action == TheatreAction::AcceptOperation(op.clone())).map(|p| p.id);
        match open {
            Some(id) => self.command(TheatreCommand::Respond { proposal: id, accept: true }),
            None => self.command(TheatreCommand::Propose(TheatreAction::AcceptOperation(op.clone()))),
        }
    }

    fn open_briefing_inner(&mut self, op: OperationId) -> bool {
        if self.backend.is_none() { self.reject(Msg::new("campaign.reject.no_campaign")); return false; }
        if self.busy() || !matches!(self.phase, CampaignPhase::Chart | CampaignPhase::Briefing) { self.reject(Msg::new("campaign.reject.busy")); return false; }
        if self.briefing_of(&op).is_none() { self.reject_typed(TheatreReject::OperationNotOffered); return false; }
        let accepted = self.local().and_then(|t| t.accepted().cloned());
        match self.local() {
            Some(_) if accepted.as_ref() != Some(&op) => {
                let ok = if self.is_coop() { self.vote_accept(&op) } else { self.command(TheatreCommand::Propose(TheatreAction::AcceptOperation(op.clone()))) };
                if !ok { return false; }
            }
            Some(_) => {}
            // A member votes; the host's chart shows whether the accept committed.
            None => { let _ = self.vote_accept(&op); }
        }
        self.run.briefed = Some(op);
        self.run.played.activity();
        self.set_phase(CampaignPhase::Briefing);
        true
    }

    fn launch_inner(&mut self) -> bool {
        if self.local().is_none() {
            let m = if self.backend.is_some() { words::reject_msg(TheatreReject::NotHost) } else { Msg::new("campaign.reject.no_campaign") };
            self.reject(m);
            return false;
        }
        let Some(op) = self.run.briefed.clone().filter(|_| self.phase == CampaignPhase::Briefing) else { self.reject(Msg::new("campaign.reject.no_briefing")); return false };
        let live = { let s = services(); let st = s.feed.borrow().state; matches!(st, SessionState::Loading | SessionState::InSortie) || s.handoff.borrow().sortie.is_some() };
        if live { self.reject(Msg::new("campaign.new.busy")); return false; }
        if self.local().and_then(|t| t.accepted().cloned()).as_ref() != Some(&op) {
            if self.is_coop() { self.reject(Msg::new("campaign.reject.not_accepted")); return false; }
            if !self.command(TheatreCommand::Propose(TheatreAction::AcceptOperation(op.clone()))) { return false; }
        }
        let data = theatre_data();
        let mut slots: Vec<PlayerSlot> = self.connected_slots().into_iter().collect();
        slots.sort();
        // Every fielded design must pass the act's campaign ruleset.
        let bad = self.local().and_then(|t| slots.iter().filter_map(|p| t.state.players.get(p.0 as usize)).filter(|r| !r.hull_lost).find_map(|r| model::validate_fielded(&r.blueprint, &r.blueprint, &t.state, &data).err()));
        if let Some(r) = bad { self.reject_typed(r); return false; }
        let Some(t) = self.local_mut() else { return false };
        let hour = t.state.hour;
        let started = t.start_sortie(&op, &slots);
        self.pump();
        let config = match started { Ok(c) => c, Err(r) => { self.reject_typed(r); return false; } };
        let services = services();
        services.handoff.borrow_mut().sortie = Some(crate::session::PendingSortie {
            kind: SessionKind::Campaign,
            start: crate::session::host::HostStart::New { config: Box::new(config) },
            options: crate::session::host::SortieOptions { checkpoints: true, fixed_step: false },
        });
        self.coop_sortie_started(&services);
        drop(services);
        self.run.sortie = Some(SortieWatch { launched_s: self.run.now_s, hour, seen_running: false, finished_s: None });
        self.run.briefed = None;
        self.run.played.activity();
        self.set_phase(CampaignPhase::Sortie);
        true
    }

    /// Follow the launched sortie through the session and take its result.
    pub(crate) fn watch_sortie(&mut self, services: &Services) {
        if self.local().is_none() {
            // A member's own sortie end carries no result: the host's report arrives as `AfterAction`.
            let mine = services.handoff.borrow().finished.as_ref().is_some_and(|f| f.kind == SessionKind::Campaign);
            if mine { services.handoff.borrow_mut().finished = None; }
            return;
        }
        if self.phase != CampaignPhase::Sortie { return; }
        let state = services.feed.borrow().state;
        let pending_start = services.handoff.borrow().sortie.is_some();
        let now = self.run.now_s;
        let hour = self.state().map_or(0, |s| s.hour);
        // A sortie resumed or re-hosted without a launch from here is watched from now.
        let w = self.run.sortie.get_or_insert(SortieWatch { launched_s: now, hour, seen_running: false, finished_s: None });
        if matches!(state, SessionState::Loading | SessionState::InSortie | SessionState::Ended) { w.seen_running = true; }
        if state == SessionState::InSortie {
            if let Some(path) = self.run.resumed_from.take() {
                if !store::delete(&path) { godot_warn!("campaign: the resumed suspend save could not be deleted: {}", path.display()); }
            }
        }
        let (launched, seen) = (w.launched_s, w.seen_running);

        let finished = services.handoff.borrow().finished.as_ref().filter(|f| f.kind == SessionKind::Campaign).map(|f| (f.summary.outcome, f.result.as_ref().map(|r| r.outcome)));
        match finished {
            Some((summary, result)) if summary == Outcome::Aborted || result == Some(Outcome::Aborted) => {
                let checkpoint = services.handoff.borrow_mut().finished.take().and_then(|f| f.checkpoint);
                self.sortie_quit(checkpoint);
            }
            Some((_, Some(_))) => {
                let result = services.handoff.borrow_mut().finished.take().and_then(|f| f.result);
                if let Some(r) = result { self.take_result(&r); }
            }
            Some((_, None)) => {
                let first = *self.run.sortie.as_mut().map_or(&mut 0.0, |w| w.finished_s.get_or_insert(now));
                if now - first > RESULT_WAIT_S {
                    services.handoff.borrow_mut().finished = None;
                    self.sortie_failed(Msg::new("campaign.sortie.lost_result"));
                }
            }
            None => {
                if pending_start { return; }
                let failed = state == SessionState::Failed || (state == SessionState::Idle && (seen || now - launched > START_WAIT_S));
                if failed { self.sortie_failed(Msg::new("campaign.sortie.failed")); }
            }
        }
    }

    /// The sortie never produced a result: back to the chart table with the offer standing.
    pub(crate) fn sortie_failed(&mut self, why: Msg) {
        if let Some(t) = self.local_mut() {
            let r = aborted_result(t);
            let _ = t.apply_result(&r);
        }
        self.pump();
        self.run.sortie = None;
        // A resumed sortie that never ran: the theatre now goes on from the suspend slot's state with the
        // sortie aborted, so the slot must go, or the menu offers it again and a second resume rebuilds this
        // older theatre over whatever is saved after it (DECISION, r4). The campaign is saved first where the
        // gate allows, so a crash before the next save cannot lose the resumed state.
        let resumed = self.run.resumed_from.take();
        self.reject(why);
        self.set_phase(CampaignPhase::Chart);
        if let Some(path) = resumed {
            self.maybe_autosave(true);
            if !store::delete(&path) { godot_warn!("campaign: the suspend save of a failed resume could not be deleted: {}", path.display()); }
        }
        self.coop_send_chart(None);
    }

    /// Apply a finished sortie's result and show the report.
    pub(crate) fn take_result(&mut self, result: &sim_systems::protocol::SortieResult) {
        let data = theatre_data();
        let now_hour = self.state().map_or(0, |s| s.hour);
        let hour = self.run.sortie.as_ref().map_or(now_hour, |w| w.hour);
        let before: Vec<PartId> = self.state().map(|s| model::fieldable_parts(s, &data)).unwrap_or_default();
        let Some(t) = self.local_mut() else { return };
        let aa = t.apply_result(result);
        self.pump();
        let after: Vec<PartId> = self.state().map(|s| model::fieldable_parts(s, &data)).unwrap_or_default();
        self.run.recent_unlocks = after.into_iter().filter(|p| !before.contains(p)).collect();
        self.run.report_hour = hour;
        self.run.sortie = None;
        self.coop_send_after_action(&aa);
        self.after_action = Some(Box::new(aa));
        self.run.sigs.push(Sig::AfterAction);
        self.run.sigs.push(Sig::Resources);
        self.set_phase(CampaignPhase::AfterAction);
        // Ironman's rolling save must hold the result at once, so a quit cannot undo a loss.
        self.maybe_autosave(true);
    }

    // ---- Dry dock ----

    fn open_dock_inner(&mut self) -> bool {
        let data = theatre_data();
        if self.backend.is_none() { self.reject(Msg::new("campaign.reject.no_campaign")); return false; }
        if self.busy() || !matches!(self.phase, CampaignPhase::Chart | CampaignPhase::Briefing) { self.reject(Msg::new("campaign.reject.busy")); return false; }
        if !self.at_cove(&data) { self.reject_typed(TheatreReject::NotAtCove); return false; }
        let slot = self.local_slot();
        let brief_src = self.run.briefed.as_ref().and_then(|op| self.briefing_of(op));
        let Some(state) = self.state() else { return false };
        if state.sextant_refit_until_hour.is_some_and(|h| h > state.hour) { self.reject_typed(TheatreReject::NotAtCove); return false; }
        let Some(rec) = state.players.get(slot.0 as usize) else { self.reject_typed(TheatreReject::UnknownTarget); return false };
        let dock = crate::editor::CampaignDock {
            act: model::rules_act(state),
            cap_t: model::rules_act(state).hull_cap_t(),
            fielded: Arc::new(rec.blueprint.clone()),
            path: String::new(),
            brief: brief_src.map(|b| Arc::new(fit_brief(&b, state))),
            available: Some(Arc::new(model::refit_parts(state, &data, &rec.blueprint))),
            parts_store: Arc::new(state.resources.parts_store.clone()),
            salvage: state.resources.salvage as i64,
        };
        crate::editor::offer_campaign_dock(dock);
        self.run.dock_return = Some(self.phase);
        self.run.dock_seen_open = false;
        self.run.dock_opened_s = self.run.now_s;
        self.run.played.activity();
        self.set_phase(CampaignPhase::DryDock);
        true
    }

    /// Leave the dry-dock phase once the editor has closed (or never opened within a second).
    fn watch_dock(&mut self) {
        if self.phase != CampaignPhase::DryDock { return; }
        let open = self.base().get_node_or_null("/root/Editor").and_then(|mut n| n.try_call("is_open", &[]).ok()).and_then(|v| v.try_to::<bool>().ok()).unwrap_or(false);
        if open { self.run.dock_seen_open = true; return; }
        if !self.run.dock_seen_open && self.run.now_s - self.run.dock_opened_s < 1.0 { return; }
        let back = self.run.dock_return.take().unwrap_or(CampaignPhase::Chart);
        self.set_phase(if back == CampaignPhase::Briefing && self.run.briefed.is_some() { back } else { CampaignPhase::Chart });
        if std::mem::take(&mut self.run.refit_waiting) { self.set_phase(CampaignPhase::Chart); let _ = self.start_advance(); }
        self.run.sigs.push(Sig::Resources);
    }

    fn commit_refit_inner(&mut self) -> bool {
        if self.backend.is_none() { self.reject(Msg::new("campaign.reject.no_campaign")); return false; }
        if self.busy() { self.reject(Msg::new("campaign.reject.busy")); return false; }
        let Some(design) = services().design.borrow().as_ref().map(|d| d.blueprint.clone()) else { self.reject(Msg::new("campaign.reject.no_design")); return false };
        let data = theatre_data();
        let slot = self.local_slot();
        let invalid = self.state().and_then(|s| s.players.get(slot.0 as usize).and_then(|r| model::validate_fielded(&design, &r.blueprint, s, &data).err()));
        if let Some(r) = invalid { self.reject_typed(r); return false; }
        if !self.command(TheatreCommand::Refit { blueprint: Box::new((*design).clone()) }) { return false; }
        // Refit time passes only through the refit time action. The refit itself is committed and paid
        // for: a refused time action (a forced operation waiting) leaves the hull refitting until a later
        // wait, and is reported by its own refusal.
        let open = self.run.proposals.values().find(|p| p.action == TheatreAction::Refit).map(|p| p.id);
        let coop = self.is_coop();
        let timed = match open {
            Some(id) => self.command(TheatreCommand::Respond { proposal: id, accept: true }),
            None => self.command(TheatreCommand::Propose(TheatreAction::Refit)),
        };
        if timed && open.is_none() && !coop && self.local().is_some() { self.after_commit(&TheatreAction::Refit); }
        self.run.sigs.push(Sig::Resources);
        true
    }

    // ---- Signals ----

    /// Emit every queued signal. Called at the end of each entry point with no `Services` borrow held,
    /// because handlers call straight back into `Campaign`. Change signals are folded to one each.
    pub(crate) fn flush(&mut self) {
        // Handlers may queue more (a refusal, a phase); a few rounds settle any sane chain.
        for _ in 0..8 {
            if self.run.sigs.is_empty() { return; }
            let sigs = std::mem::take(&mut self.run.sigs);
            let data = theatre_data();
            let (mut chart, mut resources) = (false, false);
            for s in sigs {
                match s {
                    Sig::Chart => chart = true,
                    Sig::Resources => resources = true,
                    Sig::Phase(p) => { self.base_mut().emit_signal("phase_changed", &[p.to_variant()]); }
                    Sig::Hour(h) => { self.base_mut().emit_signal("hour_advanced", &[(h.min(i64::MAX as u64) as i64).to_variant()]); }
                    Sig::Event(e, hour) => { let d = dicts::event_dict(&e, hour, &self.names(&data)); self.base_mut().emit_signal("theatre_event", &[d.to_variant()]); }
                    Sig::Interrupted(i) => { let d = dicts::interrupt_dict(&i, &self.names(&data)); self.base_mut().emit_signal("interrupted", &[d.to_variant()]); }
                    Sig::Proposal(p, state) => { let d = dicts::proposal_dict(&p, &self.names(&data)); self.base_mut().emit_signal("proposal_changed", &[d.to_variant(), StringName::from(state).to_variant()]); }
                    Sig::Rejected(m) => { let d = crate::convert::msg_dict(&m); self.base_mut().emit_signal("rejected", &[d.to_variant()]); }
                    Sig::AfterAction => { self.base_mut().emit_signal("after_action_ready", &[]); }
                    Sig::Over => { self.base_mut().emit_signal("campaign_over", &[]); }
                    Sig::Saved(info) => { let d = dicts::save_slot_dict(&info); self.base_mut().emit_signal("saved", &[d.to_variant()]); }
                }
            }
            if resources { self.base_mut().emit_signal("resources_changed", &[]); }
            if chart { self.base_mut().emit_signal("chart_changed", &[]); }
        }
        self.run.sigs.clear();
    }
}

impl BpCampaign {
    /// Start the briefing Fit Check if the briefing or the fielded design changed; the job state now.
    fn ensure_brief_fit(&mut self) -> crate::enums::JobState {
        use crate::enums::JobState;
        let Some(op) = self.run.briefed.clone().filter(|_| self.phase == CampaignPhase::Briefing) else { return JobState::Idle };
        if self.run.brief_fit.checked_rev == Some(self.run.rev) && self.run.brief_fit.key.as_ref().is_some_and(|k| k.0 == op) {
            return if self.run.brief_fit.rx.is_some() { JobState::Running } else { JobState::Done };
        }
        let slot = self.local_slot();
        let Some(state) = self.state() else { return JobState::Idle };
        let Some(rec) = state.players.get(slot.0 as usize).filter(|r| !r.hull_lost) else { return JobState::Idle };
        let Some(b) = self.briefing_of(&op) else { return JobState::Idle };
        let brief = fit_brief(&b, state);
        let bp = rec.blueprint.clone();
        let hash = content::blueprint::io::compute_hash(&bp);
        self.run.brief_fit.checked_rev = Some(self.run.rev);
        if self.run.brief_fit.key.as_ref() == Some(&(op.clone(), hash)) {
            return if self.run.brief_fit.rx.is_some() { JobState::Running } else { JobState::Done };
        }
        self.run.brief_fit.stop();
        self.run.brief_fit.key = Some((op, hash));
        self.run.brief_fit.result = None;
        let deps = services().sim_deps_factory();
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = Stopped(cancel.clone());
        let spawned = std::thread::Builder::new().name("briefing-fit".into()).spawn(move || {
            let mut stop = stop;
            let r = sim_systems::editor::fitcheck::fit_check(&bp, &brief, deps(), sim_systems::editor::fitcheck::DEFAULT_SECONDS, &mut stop);
            // A stopped check is dropped, not shown: the briefing runs it again when it next opens.
            if !stop.0.load(std::sync::atomic::Ordering::Relaxed) { let _ = tx.send(r); }
        });
        if spawned.is_err() { return JobState::Failed; }
        self.run.brief_fit.rx = Some(rx);
        self.run.brief_fit.cancel = Some(cancel);
        JobState::Running
    }

    fn poll_brief_fit(&mut self) {
        // Out of the briefing (launched, or back to the chart) the check stops.
        if self.phase != CampaignPhase::Briefing && self.run.brief_fit.rx.is_some() { self.run.brief_fit.stop(); return; }
        let Some(rx) = self.run.brief_fit.rx.as_ref() else { return };
        match rx.try_recv() {
            Ok(r) => {
                self.run.brief_fit.result = Some(r);
                self.run.brief_fit.rx = None;
                self.run.brief_fit.cancel = None;
                self.base_mut().emit_signal("briefing_fit_changed", &[]);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.run.brief_fit.rx = None;
                self.base_mut().emit_signal("briefing_fit_changed", &[]);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }
}

/// What Fit Check needs from a briefing.
pub(crate) fn fit_brief(b: &Briefing, state: &TheatreState) -> sim_systems::editor::fitcheck::FitBrief {
    let mut threats: Vec<content::ids::ClassId> = b.threats.iter().filter_map(|t| t.class.clone()).collect();
    threats.sort();
    threats.dedup();
    sim_systems::editor::fitcheck::FitBrief { tags: b.tags.clone(), weather: b.weather, approach_km: if b.distance_km.is_finite() { b.distance_km.max(0.0) } else { 0.0 }, threats, cap_t: model::rules_act(state).hull_cap_t() }
}
