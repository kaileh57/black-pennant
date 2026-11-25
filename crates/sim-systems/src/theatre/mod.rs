//! Unit `sys-theatre`: the `Theatre` struct, its public API, theatre time and co-op proposals. Sibling units
//! work on the plain `content::save::TheatreState` through the functions in their files:
//! `logistics.rs` = `sys-theatre-logistics`, `alert.rs` = `sys-theatre-alert`,
//! `admiral.rs` = `sys-theatre-admiral`, `adaptation.rs` = `sys-theatre-adaptation`,
//! `progress.rs` = `sys-theatre-progress`, `opgen.rs` = `sys-theatre-opgen` (hard),
//! `economy.rs` = `sys-theatre-economy`, `saves.rs` = `sys-theatre-saves`,
//! `abstract_combat.rs` = `sys-theatre-abstract`, `bot.rs` = `sys-campaign-bot`. `types.rs` is contract-owned.
//! DESIGN.md L729-896 (campaign), L62-73 (core loop), L832-845 (co-op campaign).
//!
//! The campaign layer does not live inside `Sim`. A `Theatre` wraps the persistent ledger, creates a
//! `SortieConfig` for the chosen operation and consumes the `SortieResult`. Time advances only when the
//! players commit a transit, refit or wait, in 1-hour steps until the next decision or interrupt; it is not
//! continuous real time. Only one operation runs at a time, so only one physics world ever exists (L1133).
//! All persistent state is in `Theatre::state`; nothing else here survives a save.
//!
//! # Hour step order (contract)
//! `advance_hour` runs: (1) convoys and shipyards (`logistics`), (2) supply, patrol density and lane
//! reopenings (`logistics`), (3) alert decay and delayed kill notices, hunter group (`alert`), (4) tally decay
//! and card arrivals (`adaptation`), (5) Admiral replan every 6 hours, group movement and belief updates
//! (`admiral`), (6) operation clock, act end, offers and unlocks (`progress`, `opgen::refresh_offers`),
//! (7) player timers: refits, rebuilds, SEXTANT relocation. Randomness uses `rng::Stream::Theatre` keyed
//! (theatre seed, hour, ids).
//!
//! # Rules where the spec is silent
//! - Time actions: a committed transit, wait or refit is held here (not in the save) until done;
//!   `saves::can_save` refuses a full save while one runs. A new wait, refit or transit replaces a running
//!   wait or refit; nothing replaces a running transit. `advance` with no time action running returns
//!   `WaitDone` at once: time never moves on its own.
//! - An operation is fought at the hour it is committed (`start_sortie`): its weather and sun are that hour's.
//!   `apply_result` then runs the operation's theatre hours at once, so the after-action state is between
//!   operations and can be saved: LANTERN's one-way transit from SEXTANT to the operation node (SEXTANT's
//!   lanes and 12 kn, at most [`MAX_PASSAGE_H`]) out and back, plus the sortie's length rounded up to whole
//!   hours (at most [`MAX_SORTIE_H`]), at least 1 hour. Those hours' events are in the report.
//! - Transits go only to open coves (SEXTANT moves between hidden coves) and are refused while SEXTANT
//!   refits after being sunk. While a forced operation is pending, no transit commits and no other operation
//!   starts; while some player hull can also sail, no wait or refit commits either and `advance` returns
//!   `ForcedOperation` without moving time. With every hull lost, refitting or rebuilding, waits and refits
//!   still run, so the dock can finish its work and the players are never left without a legal move.
//! - An `Aborted` result clears the active operation and changes nothing else; the offer still stands.
//! - Commands from a player slot outside `TheatreState::players` are refused (`UnknownTarget`).
//! - `start_sortie` with no player list fields every player whose hull can sail now; a listed player whose
//!   hull is refitting or rebuilding refuses the sortie (`HullNotReady`).
//! - Interrupts, most urgent first: forced operation, act ended, hunter group sortied (with hours to
//!   arrival), operation clock below 7, 3 and 1 days, a queued doctrine card 24 hours from arrival, the time
//!   action finished (`Arrived`, `WaitDone`, `RefitDone`), a hull ready, a new operation on offer.
//!   Interrupts raised during an operation's own hours are not returned; a forced operation that arose is
//!   returned by the next `advance`.
//!   `advance` stops at `max_hours` with `Paused`.
//! - Co-op: any player proposes; a proposal commits when every connected player has accepted (the proposer
//!   accepts by proposing) or the host confirms. A refused vote is recorded and leaves the proposal open. A
//!   proposal that no longer commits when its turn comes is withdrawn. Intel spends always go through the
//!   proposal rule; refits, repairs, rearming and rebuilding a player's own hull commit at once. Only the host
//!   changes difficulty or saves in co-op.
//! - Difficulty between operations (never under Ironman, hull count never): a new clock length moves the
//!   time left by the difference, never below 1 hour.
//! - Starting stores: [`STARTING_SALVAGE`] Salvage per player and two full loads of fuel and ammunition for
//!   every starter hull in SEXTANT's stock; no Intel.
//! - Story operations, ace duels and the capstone cannot be declined away: declining one leaves it on the
//!   chart. Accepting only marks the choice; `start_sortie` is what commits the operation.
//! - Player slots are indices into `TheatreState::players`, at most 4 players.
pub mod abstract_combat;
pub mod adaptation;
pub mod admiral;
pub mod alert;
pub mod bot;
pub mod economy;
pub mod logistics;
pub mod opgen;
pub mod progress;
pub mod saves;
pub mod types;

use crate::prelude::*;
use crate::protocol::config::{Outcome, SortieConfig, SortieResult};
use content::blueprint::Blueprint;
use content::campaign::{CampaignData, DifficultySliders, MissionType, OperationKind};
use content::save::{PlayerRecord, Resources, SaveGame, SaveHeader, TheatreAction, TheatreState};
pub use types::*;

/// Longest one-way passage and sortie charged to an operation, in theatre hours.
pub const MAX_PASSAGE_H: u32 = 72;
pub const MAX_SORTIE_H: u64 = 3;
/// Salvage per player in SEXTANT's pool at campaign start.
pub const STARTING_SALVAGE: u32 = 400;
/// Most players in one campaign (four by 1.0, L832).
pub const MAX_CAMPAIGN_PLAYERS: usize = 4;

/// Read-only data every theatre function needs.
#[derive(Clone)]
pub struct TheatreData { pub campaign: Arc<CampaignData>, pub catalogue: Arc<Catalogue> }

/// The operation in progress between `start_sortie` and `apply_result`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveOperation { pub operation: OperationId, pub region: RegionId, pub node: TheatreNodeId, pub seed: u64, pub started_hour: u64 }

/// A new campaign's starting choices.
#[derive(Clone, Debug, PartialEq)]
pub struct NewCampaign { pub seed: u64, pub difficulty: DifficultySliders, pub ironman: bool, pub slot_name: String, /// (client id, name, starter blueprint) per player; one combat hull each, ever (L822).
    pub players: Vec<(u64, String, Blueprint)>, /// Skip to an act for testing and balance bots; `Act1` in play.
    pub start_act: Act }

/// A committed time action still running (not persistent).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Pending { Transit { to: TheatreNodeId, arrive_hour: u64 }, Wait { until_hour: u64 }, Refit }

pub struct Theatre {
    pub state: TheatreState,
    pub(crate) header: SaveHeader,
    pub(crate) data: TheatreData,
    pub(crate) proposals: Vec<Proposal>,
    pub(crate) next_proposal: u32,
    pub(crate) active: Option<ActiveOperation>,
    pub(crate) events: Vec<TheatreEvent>,
    pub(crate) clock_paused: bool,
    /// Running transit, wait, refit or passage home.
    pub(crate) pending: Option<Pending>,
    /// Hash of the last save written or loaded, chained into the next save's header.
    pub(crate) last_hash: Option<ContentHash>,
    /// The operation the players accepted at the chart table, if any.
    pub(crate) accepted: Option<OperationId>,
    /// Interrupt candidates gathered during the current hour.
    interrupts: Vec<Interrupt>,
}

impl Theatre {
    /// Start a campaign: seed the ledger from `TheatreDef` (hull records scaled by the hull-count slider,
    /// groups, convoys, depots), open Act 1 normally (all regions Alert 0, 3 coves), place SEXTANT.
    /// Unit: `sys-theatre`.
    pub fn new_campaign(new: NewCampaign, data: TheatreData) -> Theatre {
        let difficulty = sane_difficulty(new.difficulty);
        let mut players: Vec<PlayerRecord> = new.players.iter().take(MAX_CAMPAIGN_PLAYERS).map(|(client_id, name, bp)| PlayerRecord {
            client_id: *client_id, name: name.clone(), blueprint: bp.clone(), damage: Default::default(), fuel_fraction: 1.0, ammo: BTreeMap::new(), hull_lost: false, ready_at_hour: None, veterancy: 0.0,
        }).collect();
        for p in players.iter_mut() { p.ammo = economy::ammo_capacities(&p.blueprint, &data); }
        let resources = starting_stores(&players, &data);
        let state = TheatreState {
            seed: new.seed, act: new.start_act, hour: 0, clock_hours_left: None, retreat: false, open_theatre: false, difficulty, resources, players,
            sextant_node: TheatreNodeId::default(), sextant_refit_until_hour: None, open_coves: Vec::new(), regions: BTreeMap::new(), lanes: BTreeMap::new(), depots: BTreeMap::new(),
            hulls: Vec::new(), groups: Vec::new(), convoys: Vec::new(), tallies: BTreeMap::new(), ops: Default::default(), offered: Vec::new(), hunter_group: None,
            last_admiral_plan_hour: 0, last_host_rung: 0, next_id: 1,
        };
        let first_client = new.players.first().map_or(0, |p| p.0);
        let campaign_id = sim_core::rng::bits(sim_core::rng::Key::new(new.seed, 0, sim_core::rng::Stream::Theatre, logistics::fnv("campaign"), first_client));
        let header = SaveHeader {
            format: content::save::SAVE_FORMAT, game_version: String::new(), tuning_version: data.catalogue.tuning.version, sequence: 0, prev_hash: ContentHash::EMPTY,
            campaign_id, slot_name: new.slot_name.clone(), ironman: new.ironman, played_s: 0,
        };
        let mut t = Theatre::wrap(state, header, data, None);
        let mut ev = Vec::new();
        progress::start_act(&mut t.state, new.start_act, false, &t.data, &mut ev);
        if t.state.sextant_node.as_str().is_empty() {
            // No cove on the chart: SEXTANT waits at the first node.
            if let Some(n) = t.data.campaign.theatre.nodes.first() { t.state.sextant_node = n.id.clone(); }
        }
        ev.retain(|e| !matches!(e, TheatreEvent::SextantMoved { from, .. } if from.as_str().is_empty()));
        t.events.extend(ev);
        t.refresh();
        t
    }

    /// Wrap a loaded save. Unit: `sys-theatre`.
    pub fn from_save(save: SaveGame, data: TheatreData) -> Theatre {
        let hash = content::save::save_hash(&save);
        let SaveGame { header, theatre } = save;
        Theatre::wrap(theatre, header, data, Some(hash))
    }

    /// The save record for the current state (sequence advanced, previous hash chained) via `saves`.
    /// Unit: `sys-theatre`.
    pub fn to_save(&mut self, game_version: &str) -> SaveGame {
        let prev = self.last_hash.unwrap_or(ContentHash::EMPTY);
        let mut header = saves::next_header(&self.header, prev, game_version, self.header.played_s);
        header.tuning_version = self.data.catalogue.tuning.version;
        let save = SaveGame { header, theatre: self.state.clone() };
        self.last_hash = Some(content::save::save_hash(&save));
        self.header = save.header.clone();
        self.events.push(TheatreEvent::Saved { sequence: self.header.sequence });
        save
    }
    pub fn header(&self) -> &SaveHeader { &self.header }
    pub fn data(&self) -> &TheatreData { &self.data }
    pub fn active(&self) -> Option<&ActiveOperation> { self.active.as_ref() }
    /// Add wall-clock seconds played (the caller excludes pauses over 2 minutes, L79).
    pub fn add_played_s(&mut self, seconds: u64) { self.header.played_s = self.header.played_s.saturating_add(seconds); }
    /// The operation accepted at the chart table and not yet started, if any.
    pub fn accepted(&self) -> Option<&OperationId> { self.accepted.as_ref() }

    /// Handle one chart-table or dry-dock command from a player. Single-player proposals commit at once; in
    /// co-op a proposal commits when every connected player has accepted or the host confirms.
    /// Unit: `sys-theatre`.
    pub fn command(&mut self, player: PlayerSlot, is_host: bool, connected: PlayerMask, cmd: TheatreCommand) -> Result<(), TheatreReject> {
        if player.0 as usize >= self.state.players.len() { return Err(TheatreReject::UnknownTarget); }
        let connected = connected.with(player);
        let coop = connected.0.count_ones() > 1;
        if saves::campaign_over(self) && !matches!(cmd, TheatreCommand::PauseClock(_) | TheatreCommand::Save) { return Err(TheatreReject::CampaignOver); }
        let mut ev = Vec::new();
        let r = match cmd {
            TheatreCommand::Propose(action) => self.propose(player, connected, action),
            TheatreCommand::SpendIntel(spend) => self.propose(player, connected, TheatreAction::SpendIntel(spend)),
            TheatreCommand::Respond { proposal, accept } => {
                let Some(p) = self.proposals.iter_mut().find(|p| p.id == proposal) else { return Err(TheatreReject::NoSuchProposal) };
                if accept { p.accepted = p.accepted.with(player); }
                self.events.push(TheatreEvent::ProposalAnswered { proposal, player, accept });
                let all = self.proposals.iter().find(|p| p.id == proposal).is_some_and(|p| connected.0 & !p.accepted.0 == 0);
                if all { self.commit_proposal(proposal) } else { Ok(()) }
            }
            TheatreCommand::Confirm(id) => {
                if !is_host { return Err(TheatreReject::NotHost); }
                if !self.proposals.iter().any(|p| p.id == id) { return Err(TheatreReject::NoSuchProposal); }
                self.commit_proposal(id)
            }
            TheatreCommand::Withdraw(id) => {
                let Some(i) = self.proposals.iter().position(|p| p.id == id) else { return Err(TheatreReject::NoSuchProposal) };
                if self.proposals[i].by != player && !is_host { return Err(TheatreReject::NotHost); }
                self.proposals.remove(i);
                self.events.push(TheatreEvent::ProposalWithdrawn { proposal: id });
                Ok(())
            }
            TheatreCommand::Refit { blueprint } => self.dock_open().and_then(|_| economy::refit(&mut self.state, player, &blueprint, &self.data, &mut ev).map(|_| ())),
            TheatreCommand::Repair { full } => self.dock_open().and_then(|_| if full { economy::repair(&mut self.state, player, &self.data, &mut ev) } else { economy::repair_partial(&mut self.state, player, &self.data, &mut ev) }.map(|_| ())),
            TheatreCommand::Rearm => self.dock_open().and_then(|_| economy::rearm(&mut self.state, player, &self.data, &mut ev).map(|_| ())),
            TheatreCommand::Rebuild => self.dock_open().and_then(|_| economy::rebuild(&mut self.state, player, self.header.ironman, &self.data, &mut ev).map(|_| ())),
            TheatreCommand::PauseClock(p) => { self.clock_paused = p; Ok(()) }
            TheatreCommand::SetDifficulty(d) => {
                if coop && !is_host { return Err(TheatreReject::NotHost); }
                if self.header.ironman { return Err(TheatreReject::IronmanLocked); }
                if self.active.is_some() { return Err(TheatreReject::NotAtCove); }
                self.set_difficulty(d);
                Ok(())
            }
            TheatreCommand::Save => {
                if coop && !is_host { return Err(TheatreReject::NotHost); }
                match saves::can_save(self) { saves::SaveGate::Allowed => Ok(()), _ => Err(TheatreReject::NotAtCove) }
            }
        };
        self.events.extend(ev);
        r
    }

    /// Commit an action now (the end of a proposal): transit, refit, wait, accept or decline an operation,
    /// spend Intel, rebuild. Sets up what `advance` will run. Unit: `sys-theatre`.
    pub fn commit(&mut self, action: &TheatreAction) -> Result<(), TheatreReject> {
        self.validate(action)?;
        let hour = self.state.hour;
        let mut ev = Vec::new();
        let r = match action {
            TheatreAction::Transit(node) => {
                if *node == self.state.sextant_node { return Ok(()); }
                let h = logistics::transit_hours(&self.state, &self.data, &self.state.sextant_node, node, false).ok_or(TheatreReject::UnknownTarget)?;
                self.pending = Some(Pending::Transit { to: node.clone(), arrive_hour: hour + h.max(1) as u64 });
                Ok(())
            }
            TheatreAction::Wait { hours } => { self.pending = Some(Pending::Wait { until_hour: hour + (*hours).clamp(1, 24 * 30) as u64 }); Ok(()) }
            TheatreAction::Refit => {
                // Nothing in the dock: no time passes.
                let refitting = self.state.players.iter().any(|p| !p.hull_lost && p.ready_at_hour.is_some_and(|h| h > hour));
                self.pending = if refitting { Some(Pending::Refit) } else { None };
                Ok(())
            }
            TheatreAction::AcceptOperation(op) => { self.accepted = Some(op.clone()); Ok(()) }
            TheatreAction::DeclineOperation(op) => {
                let template = self.data.campaign.operations.get(op).is_some_and(|d| matches!(d.kind, OperationKind::Template(_)));
                if template {
                    self.state.offered.retain(|o| o != op);
                    self.state.ops.last_offered = Some(op.clone());
                    if self.accepted.as_ref() == Some(op) { self.accepted = None; }
                }
                Ok(())
            }
            TheatreAction::SpendIntel(spend) => adaptation::spend_intel(&mut self.state, spend, &self.data, &mut ev),
            TheatreAction::RebuildHull { player } => economy::rebuild(&mut self.state, PlayerSlot(*player), self.header.ironman, &self.data, &mut ev).map(|_| ()),
        };
        self.events.extend(ev);
        if r.is_ok() { self.refresh(); }
        r
    }

    /// Advance one theatre hour in the documented order. Unit: `sys-theatre`.
    pub fn advance_hour(&mut self) {
        let mut ev = Vec::new();
        let before_clock = self.state.clock_hours_left;
        self.state.hour += 1;
        let hour = self.state.hour;
        ev.push(TheatreEvent::HourAdvanced { hour });
        {
            let (s, d) = (&mut self.state, &self.data);
            logistics::step_hour(s, d, &mut ev);
            logistics::update_supply(s, d, &mut ev);
            alert::step_hour(s, d, &mut ev);
            adaptation::step_hour(s, d, &mut ev);
            admiral::step_hour(s, d, &mut ev);
            progress::step_hour(s, d, &mut ev);
        }
        let offered_before = self.state.offered.clone();
        opgen::refresh_offers(&mut self.state, &self.data);
        offer_events(&offered_before, &self.state.offered, &mut ev);

        // Player timers, SEXTANT's own refit.
        let mut ready: Vec<PlayerSlot> = Vec::new();
        for (i, p) in self.state.players.iter_mut().enumerate() {
            if p.ready_at_hour.is_some_and(|h| h <= hour) { p.ready_at_hour = None; ready.push(PlayerSlot(i as u8)); }
        }
        if self.state.sextant_refit_until_hour.is_some_and(|h| h <= hour) { self.state.sextant_refit_until_hour = None; }

        // Time action progress.
        let mut done: Option<Interrupt> = None;
        match self.pending.clone() {
            Some(Pending::Transit { to, arrive_hour }) if arrive_hour <= hour => {
                let from = core::mem::replace(&mut self.state.sextant_node, to.clone());
                ev.push(TheatreEvent::SextantMoved { from, to: to.clone() });
                self.pending = None;
                done = Some(Interrupt::Arrived(to));
            }
            Some(Pending::Wait { until_hour }) if until_hour <= hour => { self.pending = None; done = Some(Interrupt::WaitDone); }
            Some(Pending::Refit) if self.state.players.iter().all(|p| p.hull_lost || p.ready_at_hour.is_none()) => { self.pending = None; done = Some(Interrupt::RefitDone); }
            _ => {}
        }

        // Interrupt candidates, most urgent first.
        let mut cands: Vec<Interrupt> = Vec::new();
        if let Some(f) = self.blocking_forced() { cands.push(Interrupt::ForcedOperation(f)); }
        if ev.iter().any(|e| matches!(e, TheatreEvent::ActLost { .. } | TheatreEvent::ActWon { .. } | TheatreEvent::OpenTheatreOpened)) {
            let won = !ev.iter().any(|e| matches!(e, TheatreEvent::ActLost { .. }));
            cands.push(Interrupt::ActEnded { won });
        }
        for e in &ev {
            if let TheatreEvent::HunterGroupSortied { arrives_hour, .. } = e { cands.push(Interrupt::HunterGroupWarning { hours: arrives_hour.saturating_sub(hour).min(u32::MAX as u64) as u32 }); }
        }
        if let (Some(b), Some(a)) = (before_clock, self.state.clock_hours_left) {
            for days in [7.0, 3.0, 1.0] {
                if b > days * 24.0 && a <= days * 24.0 && a > 0.0 { cands.push(Interrupt::ClockLow { days_left: a / 24.0 }); break; }
            }
        }
        for (region, r) in &self.state.regions {
            for q in &r.queued_cards {
                if q.arrives_hour == hour + 24 { cands.push(Interrupt::CardArriving { region: region.clone(), card: q.card.clone(), hours: 24 }); }
            }
        }
        if let Some(d) = done { cands.push(d); }
        if !matches!(self.pending, Some(Pending::Refit)) { for p in ready { cands.push(Interrupt::HullReady(p)); } }
        for e in &ev { if let TheatreEvent::OperationOffered(op) = e { cands.push(Interrupt::OperationOffered(op.clone())); } }
        self.interrupts.extend(cands);
        self.events.extend(ev);
    }

    /// Advance hours until the next decision or interrupt, at most `max_hours`; returns why it stopped. The
    /// client animates the returned events; bots call it in a loop. Unit: `sys-theatre`.
    pub fn advance(&mut self, max_hours: u32) -> Interrupt {
        if self.clock_paused || self.active.is_some() || saves::campaign_over(self) { return Interrupt::Paused; }
        if let Some(f) = self.blocking_forced() { return self.interrupt(Interrupt::ForcedOperation(f)); }
        if self.pending.is_none() { return Interrupt::WaitDone; }
        for _ in 0..max_hours {
            self.interrupts.clear();
            self.advance_hour();
            if let Some(i) = self.interrupts.first().cloned() {
                self.interrupts.clear();
                return self.interrupt(i);
            }
            if self.pending.is_none() { return self.interrupt(Interrupt::WaitDone); }
        }
        Interrupt::Paused
    }
    /// Take the events queued since the last drain. Real code.
    pub fn drain_events(&mut self) -> Vec<TheatreEvent> { core::mem::take(&mut self.events) }

    /// What the chart table shows now. Unit: `sys-theatre`.
    pub fn chart(&self) -> ChartView {
        let s = &self.state;
        let d = &self.data;
        let hour = s.hour;
        let floor = d.campaign.rules.patrol_floor;
        let regions = d.campaign.theatre.regions.iter().map(|def| {
            let r = s.regions.get(&def.id);
            let cards: Vec<CardId> = r.map(|r| r.active_cards.clone()).unwrap_or_default();
            let card_factor: f64 = cards.iter().filter_map(|c| d.campaign.cards.get(c)).filter_map(|c| c.effect.patrol_density_factor).filter(|f| f.is_finite() && *f > 0.0).product();
            RegionView {
                region: def.id.clone(), open: logistics::region_open(s, d, &def.id), alert_floor: r.map_or(0, |r| alert::alert_floor(r.alert)),
                supply: r.map_or(100.0, |r| r.supply), patrol_factor: logistics::patrol_factor(r.map_or(100.0, |r| r.supply), floor) * card_factor,
                active_cards: cards, queued_cards: r.map(|r| r.queued_cards.iter().map(|q| (q.card.clone(), q.arrives_hour)).collect()).unwrap_or_default(),
                combine_control: r.is_some_and(|r| r.combine_control),
            }
        }).collect();
        let cut_lanes = s.lanes.iter().filter_map(|(k, l)| l.cut_until_hour.filter(|u| *u > hour).map(|u| (k.0.clone(), k.1.clone(), u))).collect();
        // Contacts: only what Intel, reveals and warnings allow.
        let region_of = |n: &TheatreNodeId| d.campaign.theatre.nodes.iter().find(|x| &x.id == n).map(|x| &x.region);
        let mut contacts: Vec<ChartContact> = Vec::new();
        for c in &s.convoys {
            let revealed = c.revealed || [&c.from, &c.to].iter().any(|n| region_of(n).and_then(|r| s.regions.get(r)).and_then(|r| r.convoys_revealed_until_hour).is_some_and(|u| u > hour));
            if !revealed { continue; }
            let node = if hour.saturating_sub(c.depart_hour) * 2 < c.arrive_hour.saturating_sub(c.depart_hour) { c.from.clone() } else { c.to.clone() };
            contacts.push(ChartContact { id: c.id, node, destination: Some(c.to.clone()), convoy: true, hulls: c.hulls.len().min(255) as u8, ace: None, last_seen_hour: hour });
        }
        for g in &s.groups {
            let ace = g.hulls.iter().find_map(|h| s.hulls.iter().find(|r| r.id == *h && r.alive).and_then(|r| r.ace.clone()));
            let hunter = s.hunter_group == Some(g.id);
            let revealed_ace = ace.as_ref().is_some_and(|a| s.ops.aces_revealed.contains(a));
            if !(hunter || revealed_ace) { continue; }
            let alive = g.hulls.iter().filter(|h| s.hulls.iter().any(|r| r.id == **h && r.alive)).count();
            contacts.push(ChartContact { id: g.id, node: g.node.clone(), destination: g.destination.clone(), convoy: false, hulls: alive.min(255) as u8, ace: if revealed_ace || hunter { ace } else { None }, last_seen_hour: hour });
        }
        let weather = d.campaign.theatre.regions.iter().map(|r| (r.id.clone(), opgen::region_weather(s.seed, &r.id, hour, d))).collect();
        ChartView {
            hour, act: s.act, retreat: s.retreat, open_theatre: s.open_theatre, clock_hours_left: s.clock_hours_left, sextant: s.sextant_node.clone(), coves: s.open_coves.clone(),
            regions, cut_lanes, contacts, offers: opgen::offers(s, d), proposals: self.proposals.clone(), hunter_warning_hours: alert::hunter_eta_hours(s),
            tallies: s.tallies.iter().map(|(k, v)| (*k, *v)).collect(), weather, time_of_day: progress::time_of_day(hour, d),
        }
    }
    /// The briefing of an offered operation (deterministic for the state and operation).
    pub fn briefing(&self, operation: &OperationId) -> Option<Briefing> { opgen::briefing(&self.state, &self.data, operation) }

    /// Build the sortie for an accepted operation with the given connected players and record it as active.
    /// Unit: `sys-theatre`.
    pub fn start_sortie(&mut self, operation: &OperationId, players: &[PlayerSlot]) -> Result<SortieConfig, TheatreReject> {
        if saves::campaign_over(self) { return Err(TheatreReject::CampaignOver); }
        if self.active.is_some() || matches!(self.pending, Some(Pending::Transit { .. })) { return Err(TheatreReject::NotAtCove); }
        if !self.state.offered.contains(operation) { return Err(TheatreReject::OperationNotOffered); }
        if let Some(f) = opgen::forced_op(&self.state, &self.data) { if &f != operation { return Err(TheatreReject::ForcedOperationPending); } }
        let hour = self.state.hour;
        let mut slots: Vec<PlayerSlot> = players.iter().copied().filter(|p| (p.0 as usize) < self.state.players.len()).collect();
        slots.sort();
        slots.dedup();
        // No list: every player whose hull can sail now (a hull in refit or rebuild stays in the dock).
        if slots.is_empty() {
            slots = (0..self.state.players.len()).map(|i| PlayerSlot(i as u8))
                .filter(|p| self.state.players[p.0 as usize].ready_at_hour.is_none_or(|h| h <= hour)).collect();
        }
        let mut fielded = 0;
        for p in &slots {
            let rec = &self.state.players[p.0 as usize];
            if rec.hull_lost { continue; }
            if rec.ready_at_hour.is_some_and(|h| h > hour) { return Err(TheatreReject::HullNotReady); }
            fielded += 1;
        }
        if fielded == 0 { return Err(TheatreReject::HullNotReady); }
        let mut g = opgen::generate(&self.state, &self.data, operation, &slots).ok_or(TheatreReject::OperationNotOffered)?;
        g.config.settings.ironman = self.header.ironman;
        self.active = Some(ActiveOperation { operation: operation.clone(), region: g.briefing.region.clone(), node: g.briefing.node.clone(), seed: g.config.seed, started_hour: hour });
        self.pending = None;
        self.accepted = None;
        Ok(g.config)
    }
    /// Consume a sortie result: payouts and losses (`economy`), alert (`alert`), tallies and cards
    /// (`adaptation`), clock pushback and unlocks (`progress`), lane cuts and depot strikes (`logistics`),
    /// surviving enemy hulls written back, forced-operation bookkeeping. `Outcome::Aborted` changes nothing.
    /// Unit: `sys-theatre`.
    pub fn apply_result(&mut self, result: &SortieResult) -> AfterAction {
        let mut aa = AfterAction {
            operation: result.operation.clone(), outcome: result.outcome, stats: result.stats.clone(), payout: Payout::default(), tallies: Vec::new(), kills_by_method: Vec::new(), queued_cards: Vec::new(),
            alert_changes: Vec::new(), clock_pushback_days: 0.0, belief: result.belief.clone(), ghosts: Vec::new(), repair_cost: Vec::new(), hulls_lost: Vec::new(), events: Vec::new(),
            end_reason: result.end_reason,
        };
        // An aborted sortie never happened: the theatre is back at the chart table, the offer still stands.
        if result.outcome == Outcome::Aborted { self.active = None; return aa; }
        let active = self.active.take();
        let op_id = active.as_ref().map(|a| a.operation.clone()).or_else(|| result.operation.clone());
        let Some(op) = op_id.as_ref().and_then(|o| self.data.campaign.operations.get(o)).cloned() else { return aa };
        aa.operation = Some(op.id.clone());
        let placed = opgen::placement(&self.state, &self.data, &op.id);
        let region = active.as_ref().map(|a| a.region.clone()).or_else(|| result.region.clone()).or_else(|| placed.as_ref().map(|p| p.0.clone())).unwrap_or_default();
        let node = active.as_ref().map(|a| a.node.clone()).or_else(|| placed.map(|p| p.1)).unwrap_or_else(|| self.state.sextant_node.clone());
        let success = result.outcome == Outcome::Victory || (result.outcome == Outcome::Withdrawn && result.objective_done);
        let data = self.data.clone();
        let mut ev: Vec<TheatreEvent> = Vec::new();

        // Raided depot stock before the strike empties it.
        let depot_stock = if success && result.site_destroyed && op.kind == OperationKind::Template(MissionType::DepotStrike) { self.state.depots.get(&node).copied().filter(|v| v.is_finite()).unwrap_or(0.0).max(0.0) } else { 0.0 };

        let ironman = self.header.ironman;
        let s = &mut self.state;
        let mut payout = economy::apply_result(s, result, ironman, &data, &mut ev);
        if depot_stock > 0.0 {
            let (f, a) = economy::add_raided(s, &data, depot_stock * economy::DEPOT_FUEL_L_PER_T, depot_stock * economy::DEPOT_AMMO_KG_PER_T);
            payout.fuel_l += f;
            payout.ammo_kg += a;
        }
        alert::apply_result(s, &region, result, &data, &mut ev);
        adaptation::record_kills(s, &region, &result.kills, &data, &mut ev);
        admiral::observe_result(s, &node, result, &data);
        write_back_hulls(s, result);
        progress::apply_result(s, &op, result, &data, &mut ev);

        // Logistics effects at the operation's node.
        let lane = busiest_lane(&data, &node);
        match op.kind {
            OperationKind::Template(MissionType::ConvoyRaid) if success => { if let Some((a, b)) = &lane { logistics::cut_lane(s, a, b, &data, &mut ev); } }
            OperationKind::Template(MissionType::DepotStrike) if success && result.site_destroyed => logistics::strike_depot(s, &node, &data, &mut ev),
            _ => {}
        }
        if result.mines_laid > 0 { if let Some((a, b)) = &lane { logistics::lay_minefield(s, a, b, result.mines_laid, &data, &mut ev); } }

        // The operation leaves the chart: a template once played, anything once completed.
        if matches!(op.kind, OperationKind::Template(_)) {
            s.offered.retain(|o| o != &op.id);
            s.ops.last_offered = Some(op.id.clone());
        }
        s.last_host_rung = result.highest_rung;

        let out = opgen::lantern_hours(s, &data, &node).min(MAX_PASSAGE_H) as u64;
        let sortie_h = result.duration_ticks.div_ceil(sim_core::time::TICK_HZ as u64 * 3600).min(MAX_SORTIE_H);
        self.accepted = None;
        let offered_before = self.state.offered.clone();
        opgen::refresh_offers(&mut self.state, &self.data);
        offer_events(&offered_before, &self.state.offered, &mut ev);
        self.events.append(&mut ev.clone());

        // The operation's theatre time: the sortie and LANTERN's passage out and home.
        let mark = self.events.len();
        for _ in 0..(2 * out + sortie_h).max(1) { self.advance_hour(); }
        self.interrupts.clear();
        ev.extend(self.events[mark..].iter().cloned());

        // The report.
        let s = &self.state;
        aa.payout = payout;
        aa.tallies = s.tallies.iter().map(|(k, v)| (*k, *v)).collect();
        let mut kbm: BTreeMap<content::campaign::KillMethod, u16> = BTreeMap::new();
        for k in &result.kills { *kbm.entry(k.method).or_insert(0) = kbm.get(&k.method).copied().unwrap_or(0).saturating_add(1); }
        aa.kills_by_method = kbm.into_iter().collect();
        for e in &ev {
            match e {
                TheatreEvent::CardQueued { region, card, arrives_hour } => aa.queued_cards.push((region.clone(), card.clone(), *arrives_hour)),
                TheatreEvent::AlertChanged { region, alert, cause } => aa.alert_changes.push((region.clone(), *alert, *cause)),
                TheatreEvent::ClockPushedBack { days, .. } => aa.clock_pushback_days += days,
                _ => {}
            }
        }
        aa.ghosts = op.ghosts.iter().enumerate().map(|(i, g)| (g.clone(), result.ghosts.get(i).copied().unwrap_or(false))).collect();
        aa.repair_cost = (0..s.players.len()).map(|i| (PlayerSlot(i as u8), economy::repair_cost(s, PlayerSlot(i as u8), &data))).collect();
        aa.hulls_lost = result.players.iter().filter(|p| p.hull_lost && (p.player.0 as usize) < s.players.len()).map(|p| p.player).collect();
        aa.events = ev;
        aa
    }
}

// ---------------------------------------------------------------------------------------------------------

impl Theatre {
    pub(crate) fn wrap(state: TheatreState, header: SaveHeader, data: TheatreData, last_hash: Option<ContentHash>) -> Theatre {
        Theatre { state, header, data, proposals: Vec::new(), next_proposal: 1, active: None, events: Vec::new(), clock_paused: false, pending: None, last_hash, accepted: None, interrupts: Vec::new() }
    }

    fn interrupt(&mut self, i: Interrupt) -> Interrupt {
        self.events.push(TheatreEvent::Interrupted(i.clone()));
        i
    }

    /// Refresh offers and queue the offer changes as events.
    fn refresh(&mut self) {
        let before = self.state.offered.clone();
        opgen::refresh_offers(&mut self.state, &self.data);
        let mut ev = Vec::new();
        offer_events(&before, &self.state.offered, &mut ev);
        self.events.extend(ev);
    }

    /// Some player hull could sail now: not lost and not refitting or rebuilding.
    fn fieldable_now(&self) -> bool {
        let hour = self.state.hour;
        self.state.players.iter().any(|p| !p.hull_lost && !p.ready_at_hour.is_some_and(|h| h > hour))
    }

    /// The forced operation, when it can be started now. While no hull can sail it does not hold time
    /// still: waits and refits commit, so a hull caught in a refit or rebuild can finish it.
    fn blocking_forced(&self) -> Option<OperationId> {
        opgen::forced_op(&self.state, &self.data).filter(|_| self.fieldable_now())
    }

    /// The dry dock and tender services are open: no sortie and no passage under way.
    fn dock_open(&self) -> Result<(), TheatreReject> {
        if self.active.is_some() || matches!(self.pending, Some(Pending::Transit { .. })) { Err(TheatreReject::NotAtCove) } else { Ok(()) }
    }

    fn propose(&mut self, player: PlayerSlot, connected: PlayerMask, action: TheatreAction) -> Result<(), TheatreReject> {
        self.validate(&action)?;
        if connected.0.count_ones() <= 1 || self.state.players.len() <= 1 { return self.commit(&action); }
        let id = ProposalId(self.next_proposal);
        self.next_proposal = self.next_proposal.wrapping_add(1).max(1);
        let p = Proposal { id, by: player, action, accepted: PlayerMask(0).with(player), made_hour: self.state.hour };
        self.events.push(TheatreEvent::ProposalMade(p.clone()));
        self.proposals.push(p);
        Ok(())
    }

    fn commit_proposal(&mut self, id: ProposalId) -> Result<(), TheatreReject> {
        let Some(i) = self.proposals.iter().position(|p| p.id == id) else { return Err(TheatreReject::NoSuchProposal) };
        let p = self.proposals.remove(i);
        match self.commit(&p.action) {
            Ok(()) => { self.events.push(TheatreEvent::ProposalCommitted { proposal: id }); Ok(()) }
            Err(e) => { self.events.push(TheatreEvent::ProposalWithdrawn { proposal: id }); Err(e) }
        }
    }

    /// Would `action` commit now? No state changes.
    fn validate(&self, action: &TheatreAction) -> Result<(), TheatreReject> {
        if saves::campaign_over(self) { return Err(TheatreReject::CampaignOver); }
        if self.active.is_some() { return Err(TheatreReject::NotAtCove); }
        let forced = opgen::forced_op(&self.state, &self.data);
        let blocking = forced.is_some() && self.fieldable_now();
        let under_way = matches!(self.pending, Some(Pending::Transit { .. }));
        let s = &self.state;
        match action {
            TheatreAction::Transit(node) => {
                if forced.is_some() { return Err(TheatreReject::ForcedOperationPending); }
                if under_way { return Err(TheatreReject::NotAtCove); }
                if s.sextant_refit_until_hour.is_some_and(|h| h > s.hour) { return Err(TheatreReject::HullNotReady); }
                if *node == s.sextant_node { return Ok(()); }
                if !s.open_coves.contains(node) { return Err(TheatreReject::UnknownTarget); }
                logistics::transit_hours(s, &self.data, &s.sextant_node, node, false).map(|_| ()).ok_or(TheatreReject::UnknownTarget)
            }
            TheatreAction::Wait { .. } | TheatreAction::Refit => {
                if blocking { return Err(TheatreReject::ForcedOperationPending); }
                if under_way { return Err(TheatreReject::NotAtCove); }
                Ok(())
            }
            TheatreAction::AcceptOperation(op) => {
                if !s.offered.contains(op) { return Err(TheatreReject::OperationNotOffered); }
                if forced.as_ref().is_some_and(|f| f != op) { return Err(TheatreReject::ForcedOperationPending); }
                Ok(())
            }
            TheatreAction::DeclineOperation(op) => {
                if !s.offered.contains(op) { return Err(TheatreReject::OperationNotOffered); }
                if forced.as_ref() == Some(op) || self.data.campaign.operations.get(op).is_some_and(|d| d.forced) { return Err(TheatreReject::ForcedOperationPending); }
                Ok(())
            }
            TheatreAction::SpendIntel(spend) => {
                let mut probe = s.clone();
                adaptation::spend_intel(&mut probe, spend, &self.data, &mut Vec::new())
            }
            TheatreAction::RebuildHull { player } => {
                if under_way { return Err(TheatreReject::NotAtCove); }
                let mut probe = s.clone();
                economy::rebuild(&mut probe, PlayerSlot(*player), self.header.ironman, &self.data, &mut Vec::new()).map(|_| ())
            }
        }
    }

    fn set_difficulty(&mut self, d: DifficultySliders) {
        let old = self.state.difficulty;
        let mut new = sane_difficulty(d);
        new.hull_count = old.hull_count;
        if let Some(left) = self.state.clock_hours_left {
            let delta = (new.clock_days as f64 - old.clock_days as f64) * 24.0;
            let full = new.clock_days as f64 * 24.0;
            self.state.clock_hours_left = Some((left + delta).min(full).max(1.0));
        }
        self.state.difficulty = new;
    }
}

fn sane_difficulty(d: DifficultySliders) -> DifficultySliders {
    let n = DifficultySliders::NORMAL;
    let f = |x: f64, y: f64| if x.is_finite() { x } else { y };
    DifficultySliders { ai_accuracy: f(d.ai_accuracy, n.ai_accuracy), ai_reaction_s: f(d.ai_reaction_s, n.ai_reaction_s), adaptation_cadence: f(d.adaptation_cadence, n.adaptation_cadence), economy: f(d.economy, n.economy), ..d }.clamped()
}

/// Two full loads of fuel and ammunition for every starter hull, and the starting Salvage.
fn starting_stores(players: &[PlayerRecord], data: &TheatreData) -> Resources {
    let mut r = Resources { salvage: STARTING_SALVAGE.saturating_mul(players.len().max(1) as u32), ..Default::default() };
    for p in players {
        r.fuel_l += 2.0 * economy::fuel_capacity_l(&p.blueprint, data);
        for pl in &p.blueprint.parts {
            let Some(def) = data.catalogue.part(&pl.part) else { continue };
            match &def.function {
                content::part::PartFunction::Magazine { calibre_mm, ammo_kg, .. } if ammo_kg.is_finite() && *ammo_kg > 0.0 => *r.ammo_kg.entry(*calibre_mm).or_insert(0.0) += 2.0 * ammo_kg,
                content::part::PartFunction::Launcher(l) => { let e = r.munitions.entry(l.munition.clone()).or_insert(0); *e = e.saturating_add(2 * l.cells as u16); }
                content::part::PartFunction::MineRail { mine, capacity } => { let e = r.munitions.entry(mine.clone()).or_insert(0); *e = e.saturating_add(capacity.saturating_mul(2)); }
                _ => {}
            }
        }
    }
    r
}

fn offer_events(before: &[OperationId], after: &[OperationId], ev: &mut Vec<TheatreEvent>) {
    for o in before { if !after.contains(o) { ev.push(TheatreEvent::OperationWithdrawn(o.clone())); } }
    for o in after { if !before.contains(o) { ev.push(TheatreEvent::OperationOffered(o.clone())); } }
}

/// The lane at a node with the most traffic (ties by key), for convoy-raid cuts and minefields.
fn busiest_lane(data: &TheatreData, node: &TheatreNodeId) -> Option<(TheatreNodeId, TheatreNodeId)> {
    data.campaign.theatre.lanes.iter().filter(|l| (&l.a == node || &l.b == node) && l.a != l.b)
        .map(|l| (if l.traffic.is_finite() { l.traffic } else { 0.0 }, logistics::lane_key(&l.a, &l.b)))
        .max_by(|a, b| a.0.total_cmp(&b.0).then_with(|| b.1.cmp(&a.1)))
        .map(|x| x.1)
}

/// Kills and prizes leave the Combine; survivors carry their damage, fuel and ammunition home.
fn write_back_hulls(s: &mut TheatreState, result: &SortieResult) {
    let gone: BTreeSet<u32> = result.kills.iter().filter_map(|k| k.hull_record).chain(result.prizes.iter().filter_map(|p| p.hull_record)).collect();
    let unit = |v: f64| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    for h in s.hulls.iter_mut() {
        if gone.contains(&h.id) { h.alive = false; continue; }
        if let Some(sv) = result.survivors.iter().find(|x| x.hull_record == h.id) {
            // The sortie starts from an intact model of the hull, so its losses add to the record's.
            h.damage = 1.0 - (1.0 - unit(h.damage)) * (1.0 - unit(sv.damage));
            h.fuel = unit(sv.fuel);
            h.ammunition = unit(sv.ammunition);
        }
    }
    for g in s.groups.iter_mut() { g.hulls.retain(|id| !gone.contains(id)); }
}

#[cfg(test)]
mod tests;
