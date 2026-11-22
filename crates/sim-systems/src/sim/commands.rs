//! Part of unit `sys-sim`: phase 1 of the tick, the commands queued by `Sim::apply`.
//! DESIGN.md L108 (orders), L110-116 (pause and compression), L216 (splash commands), L953 (one player per
//! vehicle), L1687 (only the player acknowledges alerts).
//!
//! # Rules
//! - A tick's commands are applied in (player slot, seq) order; arrival order breaks ties, so unsequenced
//!   commands (seq 0, from trials, bots and tutorials) keep the order they were pushed in.
//! - Two kinds of command exist. Ship commands go through `input::apply_command`, which owns their sequence
//!   numbers and their validation; what it returns as `NotShip` (track overrides, station actions, prizes,
//!   withdrawal, script re-arming) is finished here. Direct commands never touch the ship: time (pause,
//!   compression), alerts (acknowledge, silence), queries and the splash basin's rewind, restart and live
//!   edit. They are deduplicated by their own per-player counter.
//! - While paused only direct commands apply. Ship commands stay queued in order and apply in the step that
//!   ends the pause (L112). Keeping two counters is what makes that safe: a resume cannot make the
//!   orders queued before it look like duplicates.
//! - Commands of a player who is not seated, or whose slot the officers hold after a drop, are dropped
//!   without an answer: there is nobody to answer. Ship commands of a player whose hull is gone are rejected
//!   with `HullLost`.
//! - Every refusal is a `CommandRejected` to the sender. Nothing here panics on any command.
//! - The splash basin's commands are also accepted in a tutorial basin, which keeps the same rewind ring.
use super::step::{TickEnv, MAX_SCOPES};
use super::Sim;
use crate::ident;
use crate::prelude::*;
use crate::protocol::command::{Command, CommandEnvelope, OverrideKey, Query};
use crate::protocol::event::{Audience, CandidateView, Lane, QueryReply, RejectReason, SimEvent};
use crate::protocol::snapshot::Viewer;
use crate::input::Applied;
use sim_core::vessel::VesselStatus;

/// Orders that may wait out a pause; more are refused, so a paused Sim cannot be made to grow without
/// bound.
const MAX_WAITING: usize = 512;
/// Most candidates a track card lists.
const MAX_CARD_CANDIDATES: usize = 128;

fn basin(mode: GameMode) -> bool { matches!(mode, GameMode::Splash | GameMode::Tutorial) }

/// Commands the Sim handles without the ship: they apply while paused.
fn is_direct(c: &Command) -> bool {
    matches!(c, Command::Pause(_) | Command::RequestTimeRate(_) | Command::Acknowledge(_) | Command::SilenceAlerts | Command::Query(_)
        | Command::SplashRewind { .. } | Command::SplashRestart | Command::SplashApplyDesign(_))
}

/// The override a settings command sets, for the A chip to hand glyph (L351). `None` where the command does
/// not name one value.
fn override_key(c: &Command) -> Option<(OverrideKey, bool)> {
    Some(match c {
        Command::SetPreset { mount: None, .. } => (OverrideKey::Preset, true),
        Command::SetPreset { mount: Some(m), .. } => (OverrideKey::MountPreset(*m), true),
        Command::SetSlider { key, .. } => (OverrideKey::Slider(key.clone()), true),
        Command::SetEmcon(_) => (OverrideKey::Emcon, true),
        Command::SetPowerPriority { slot, .. } => (OverrideKey::PowerPriority(*slot), true),
        Command::ReorderRepairs(_) => (OverrideKey::RepairOrder, true),
        Command::Revert(k) => (k.clone(), false),
        _ => return None,
    })
}

impl Sim {
    /// Phase 1: apply what `apply` queued. Ship commands wait while the sortie is paused.
    pub(super) fn run_commands(&mut self) {
        if self.pending.is_empty() { return; }
        let mut queue = core::mem::take(&mut self.pending);
        queue.sort_by_key(|c| (c.player, c.seq));
        let mut waiting: Vec<CommandEnvelope> = Vec::new();
        for env in queue {
            if is_direct(&env.command) { self.run_direct(&env); }
            else if !self.time.paused { self.run_ship_command(&env); }
            else if waiting.len() < MAX_WAITING { waiting.push(env); }
            else { self.reject(&env, RejectReason::ConditionsNotMet); }
        }
        // Orders given while paused take effect the moment time resumes (L112).
        if !self.time.paused { for env in waiting.drain(..) { self.run_ship_command(&env); } }
        // A restart or rewind inside the loop replaces `pending` with the frame's; orders given since then
        // still stand.
        waiting.append(&mut self.pending);
        self.pending = waiting;
    }

    fn reject(&mut self, env: &CommandEnvelope, reason: RejectReason) {
        self.emit(Audience::player(env.player), Lane::Reliable, SimEvent::CommandRejected { player: env.player, seq: env.seq, reason });
    }

    /// The sender's seat: (side, hull). A human who dropped has none until they rejoin; a bot slot (not
    /// human) is driven in process and always has one.
    fn seat(&self, player: PlayerSlot) -> Option<(SideId, Option<EntityId>)> {
        self.players.iter().find(|p| p.slot == player && (p.connected || !p.human)).map(|p| (p.side, p.vessel))
    }

    /// The sender's hull while it is afloat.
    fn hull_afloat(&self, vessel: Option<EntityId>) -> Option<EntityId> {
        vessel.filter(|e| self.ships.get(*e).is_some() && super::step::hull(&self.world, *e).is_some_and(|v| matches!(v.status, VesselStatus::Active | VesselStatus::Disabled)))
    }

    fn run_ship_command(&mut self, env: &CommandEnvelope) {
        let Some((side, vessel)) = self.seat(env.player) else { return };
        let Some(vessel) = self.hull_afloat(vessel) else { self.reject(env, RejectReason::HullLost); return };
        let tick = self.world.tick().0;
        let none = crate::tracks::SidePicture { side, tracks: Vec::new(), next_id: 1 };
        let applied = {
            let picture = self.pictures.side(side).unwrap_or(&none);
            let Some(ship) = self.ships.get_mut(vessel) else { return };
            crate::input::apply_command(ship, env, picture, self.config.mode, &self.config.settings.ruleset, tick)
        };
        match applied {
            Applied::Rejected(reason) => self.reject(env, reason),
            Applied::Ok => {
                if self.ships.get(vessel).is_some_and(|s| s.control.settings_dirty) { self.recompile_ship(vessel); }
                if let Some((key, overridden)) = override_key(&env.command) {
                    self.emit(Audience::player(env.player), Lane::Reliable, SimEvent::OverrideChanged { vessel, key, overridden });
                }
            }
            Applied::NotShip => self.run_sim_command(env, side, vessel, tick),
        }
    }

    /// The commands `apply_command` validated and handed back.
    fn run_sim_command(&mut self, env: &CommandEnvelope, side: SideId, vessel: EntityId, tick: u64) {
        let player = env.player;
        match &env.command {
            Command::SetStance { track, stance } => self.override_stance(env, side, vessel, *track, *stance),
            Command::Revert(OverrideKey::TrackStance(track)) => self.override_stance(env, side, vessel, *track, None),
            Command::SetIdentity { track, rung, class } => self.override_identity(env, side, vessel, *track, rung.map(|r| (r, class.clone())), tick),
            Command::Revert(OverrideKey::TrackIdentity(track)) => self.override_identity(env, side, vessel, *track, None, tick),
            Command::StartAction { action, target } => {
                if !self.config.actions.contains(action) { self.reject(env, RejectReason::NotAllowedInMode); return; }
                if let Err(reason) = crate::specops::start(&mut self.specops, vessel, Some(player), *action, target.clone(), &self.world, &self.ships, &self.pictures, tick) { self.reject(env, reason); }
            }
            Command::CancelAction(id) => {
                let own = self.specops.actions.iter().any(|a| a.id == *id && a.actor == vessel);
                if !(own && crate::specops::cancel(&mut self.specops, *id)) { self.reject(env, RejectReason::ConditionsNotMet); }
            }
            Command::Prize { prize, decision } => {
                // Only the player who took the prize decides its fate.
                let own = self.specops.prizes.iter().any(|p| p.vessel == *prize && p.captor == player);
                if !(own && crate::specops::decide_prize(&mut self.specops, *prize, *decision)) { self.reject(env, RejectReason::ConditionsNotMet); return; }
                if *decision == crate::protocol::command::PrizeDecision::SailHome { self.send_prize_home(*prize); }
            }
            Command::Withdraw => {
                if let Err(i) = self.objectives.withdrawing.binary_search(&player) { self.objectives.withdrawing.insert(i, player); }
                self.withdraw_route(vessel);
            }
            Command::RearmScripts => {
                if let Some(ship) = self.ships.get_mut(vessel) { crate::graph::vm::rearm(&mut ship.graph_state); }
                self.load_scripts(vessel);
            }
            // Everything else `apply_command` either applied or refused.
            _ => self.reject(env, RejectReason::InvalidValue),
        }
    }

    /// A hull ordered to withdraw sails for the nearest exit marker at its top speed; reaching it ends the
    /// sortie for that player (sys-objectives). With no exit on the chart the helm keeps its orders and the
    /// clear-of-contact rule alone ends the sortie.
    fn withdraw_route(&mut self, vessel: EntityId) {
        let Some(at) = super::step::hull(&self.world, vessel).map(|v| DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z)) else { return };
        let exit = self.config.map.markers.iter()
            .filter(|m| matches!(m.kind, content::campaign::MarkerKind::Exit))
            .map(|m| DVec2::from(m.pos_m)).filter(|p| p.is_finite())
            .min_by(|a, b| a.distance_squared(at).total_cmp(&b.distance_squared(at)));
        let (Some(exit), Some(ship)) = (exit, self.ships.get_mut(vessel)) else { return };
        let top_kn = ship.design.turn.top_speed_mps / sim_core::time::KN;
        ship.orders.route = vec![exit];
        ship.orders.station = None;
        ship.orders.formation = None;
        ship.orders.speed_kn = (top_kn.is_finite() && top_kn > 0.0).then_some(top_kn);
    }

    /// A prize ordered home sails for the nearest exit marker under its prize crew, at cruise, weapons tight.
    /// With no exit on the chart it holds where it is.
    fn send_prize_home(&mut self, prize: EntityId) {
        let Some(at) = super::step::hull(&self.world, prize).map(|v| DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z)) else { return };
        let exit = self.config.map.markers.iter()
            .filter(|m| matches!(m.kind, content::campaign::MarkerKind::Exit | content::campaign::MarkerKind::ConvoyExit))
            .map(|m| DVec2::from(m.pos_m)).filter(|p| p.is_finite())
            .min_by(|a, b| a.distance_squared(at).total_cmp(&b.distance_squared(at)));
        let (Some(exit), Some(ship)) = (exit, self.ships.get_mut(prize)) else { return };
        if let Some(c) = ship.captain.as_mut() { c.route = vec![exit]; }
        ship.orders.route = vec![exit];
        ship.orders.station = None;
        ship.orders.speed_kn = None;
        ship.orders.weapons_tight = true;
    }

    fn override_stance(&mut self, env: &CommandEnvelope, side: SideId, vessel: EntityId, track: TrackId, stance: Option<Stance>) {
        let Some(tr) = self.pictures.side_mut(side).and_then(|p| p.get_mut(track)).filter(|t| t.is_live()) else { self.reject(env, RejectReason::NoSuchTrack); return };
        if !ident::stance::set_override(tr, stance, false) { self.reject(env, RejectReason::FriendCannotBeEngaged); return; }
        let changed = SimEvent::StanceChanged { side, track, stance: tr.stance, reason: tr.stance_reason };
        self.emit(Audience::side(side), Lane::Reliable, changed);
        self.emit(Audience::player(env.player), Lane::Reliable, SimEvent::OverrideChanged { vessel, key: OverrideKey::TrackStance(track), overridden: stance.is_some() });
    }

    fn override_identity(&mut self, env: &CommandEnvelope, side: SideId, vessel: EntityId, track: TrackId, value: Option<(IdRung, Option<ClassId>)>, tick: u64) {
        let Some(tr) = self.pictures.side_mut(side).and_then(|p| p.get_mut(track)).filter(|t| t.is_live()) else { self.reject(env, RejectReason::NoSuchTrack); return };
        // A class the sortie does not know cannot be claimed.
        if let Some((_, Some(class))) = &value { if self.ident.class_entry(class).is_none() { self.reject(env, RejectReason::InvalidValue); return; } }
        let overridden = value.is_some();
        let (rung, reason) = ident::set_override(tr, value, &self.ident, tick);
        let changed = SimEvent::RungChanged { side, track, rung, reason, class: tr.class.clone() };
        self.emit(Audience::side(side), Lane::Reliable, changed);
        self.emit(Audience::player(env.player), Lane::Reliable, SimEvent::OverrideChanged { vessel, key: OverrideKey::TrackIdentity(track), overridden });
    }

    /// True when a sequenced direct command is new for its player.
    fn direct_is_new(&mut self, player: PlayerSlot, seq: u32) -> bool {
        if seq == 0 { return true; }
        match self.run.direct_seq.binary_search_by(|e| e.0.cmp(&player)) {
            Ok(i) => { if seq <= self.run.direct_seq[i].1 { return false; } self.run.direct_seq[i].1 = seq; true }
            Err(i) => { self.run.direct_seq.insert(i, (player, seq)); true }
        }
    }

    fn run_direct(&mut self, env: &CommandEnvelope) {
        let player = env.player;
        let Some((side, vessel)) = self.seat(player) else { return };
        if !self.direct_is_new(player, env.seq) { self.reject(env, RejectReason::Duplicate); return; }
        let mode = self.config.mode;
        let tick = self.world.tick().0;
        match &env.command {
            Command::Pause(on) => {
                if is_pvp(mode) { self.reject(env, RejectReason::NotAllowedInMode); return; }
                let humans = self.players.iter().filter(|p| p.human && p.connected).count();
                if !crate::timeflow::set_pause(&mut self.time, *on, mode, humans) { self.reject(env, RejectReason::SinglePlayerOnly); return; }
                self.note_pacing();
            }
            Command::RequestTimeRate(rate) => {
                if is_pvp(mode) { self.reject(env, RejectReason::NotAllowedInMode); return; }
                crate::timeflow::request_rate(&mut self.time, player, *rate);
                let sides = self.player_sides();
                self.update_pacing(sides);
            }
            Command::Acknowledge(id) => {
                let Some(vessel) = vessel else { self.reject(env, RejectReason::HullLost); return };
                // A named alert must be this player's own.
                if id.is_some_and(|id| !self.alerts.alerts.iter().any(|a| a.id == id && a.vessel == vessel)) { self.reject(env, RejectReason::InvalidValue); return; }
                let mut changes = Vec::new();
                if !self.alerts.acknowledge(vessel, *id, &mut changes) { self.reject(env, RejectReason::ConditionsNotMet); }
                self.emit_alert_changes(changes);
            }
            Command::SilenceAlerts => {
                let Some(vessel) = vessel else { self.reject(env, RejectReason::HullLost); return };
                let mut changes = Vec::new();
                self.alerts.silence(vessel, tick, &mut changes);
                self.emit_alert_changes(changes);
            }
            Command::Query(q) => self.answer(env, side, vessel, *q),
            Command::SplashRewind { ticks } => {
                if !basin(mode) { self.reject(env, RejectReason::NotAllowedInMode); return; }
                if self.rewind(*ticks).is_none() { self.reject(env, RejectReason::ConditionsNotMet); }
            }
            Command::SplashRestart => {
                if !basin(mode) { self.reject(env, RejectReason::NotAllowedInMode); return; }
                if self.restart().is_none() { self.reject(env, RejectReason::ConditionsNotMet); }
            }
            Command::SplashApplyDesign(bp) => {
                if !basin(mode) { self.reject(env, RejectReason::OnlyInDryDockOrSplash); return; }
                if self.hull_afloat(vessel).is_none() { self.reject(env, RejectReason::HullLost); return; }
                // A changed hull shell, an invalid design: the client returns to dry dock.
                if self.apply_design(player, bp).is_err() { self.reject(env, RejectReason::OnlyInDryDockOrSplash); }
            }
            _ => {}
        }
    }

    /// Answer a read request to the asking player only.
    fn answer(&mut self, env: &CommandEnvelope, side: SideId, vessel: Option<EntityId>, query: Query) {
        let player = env.player;
        let reply = match query {
            Query::TrackCard(track) => {
                let Some(tr) = self.pictures.side(side).and_then(|p| p.get(track)) else { self.reject(env, RejectReason::NoSuchTrack); return };
                let pool = &self.ident.pool;
                let view = |c: &ident::Candidate| CandidateView { class: c.class.clone(), domain: c.domain, civilian: c.civilian, own_side: c.own_side };
                // An empty set is a track not narrowed yet: the whole pool.
                let mut candidates: Vec<CandidateView> = if tr.candidates.remaining.is_empty() { pool.iter().map(view).collect() } else { tr.candidates.remaining.iter().filter_map(|&i| pool.get(i as usize)).map(view).collect() };
                // Munition types share a row once their class is stripped; the card lists each line once.
                candidates.dedup();
                candidates.truncate(MAX_CARD_CANDIDATES);
                let evidence = tr.history.iter().map(|h| (h.rung, h.reason, h.tick)).collect();
                QueryReply::TrackCard { track, candidates, evidence, stance_reason: tr.stance_reason }
            }
            Query::Why(slot) => {
                let Some(vessel) = vessel else { self.reject(env, RejectReason::HullLost); return };
                let env_t = TickEnv::of(self);
                let chain = self.with_view(vessel, &env_t, |ship, view| {
                    if slot.index() < view.vessel.parts.len() { Some(crate::officers::board::why(ship, view, slot)) } else { None }
                }).flatten();
                match chain { Some(chain) => QueryReply::Why { slot, chain }, None => { self.reject(env, RejectReason::NoSuchPart); return; } }
            }
            Query::Overrides(role) => {
                let Some(ship) = vessel.and_then(|v| self.ships.get(v)) else { self.reject(env, RejectReason::HullLost); return };
                QueryReply::Overrides { role, values: crate::officers::tuning::overrides(ship, Some(role)) }
            }
            Query::Scope { port, on } => {
                // Scopes are a splash tool (L216).
                if !basin(self.config.mode) { self.reject(env, RejectReason::OnlyInDryDockOrSplash); return; }
                let Some(ship) = vessel.and_then(|v| self.ships.get_mut(v)) else { self.reject(env, RejectReason::HullLost); return };
                let exists = ship.graph.nodes.iter().any(|n| n.id == port.node && port.port < n.out_count);
                let scoped = &mut ship.graph_state.scoped;
                scoped.retain(|p| *p != port);
                if !on { return; }
                if !exists || scoped.len() >= MAX_SCOPES { self.reject(env, RejectReason::InvalidValue); return; }
                scoped.push(port);
                let value = ship.graph.nodes.iter().find(|n| n.id == port.node)
                    .and_then(|n| ship.graph_state.values.get(n.out_base as usize + port.port as usize))
                    .map(|v| v.to_value()).unwrap_or(content::graph::Value::None);
                QueryReply::Scope { port, value }
            }
        };
        self.emit(Audience::player(player), Lane::Reliable, SimEvent::QueryReply { player, reply });
    }

    /// After the clock went back (rewind, restart): every connected player gets the catch-up a joining client
    /// gets, so clients rebuild what they draw from the restored state.
    pub(super) fn resync_clients(&mut self) {
        let viewers: Vec<PlayerSlot> = self.players.iter().filter(|p| p.connected && p.human).map(|p| p.slot).collect();
        for p in viewers {
            let events = super::snapshot::catch_up(self, Viewer::Player(p));
            self.out.extend(events);
        }
        let host = super::snapshot::catch_up(self, Viewer::Omniscient);
        self.out.extend(host);
    }
}

fn is_pvp(mode: GameMode) -> bool { super::events::is_pvp(mode) }
