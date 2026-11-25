//! Unit `net-lobby`. The host's lobby model for co-op, skirmish, sandbox and PvP: membership, admission,
//! slots held for dropped players, bans, ready state and blueprint choice, and the [`LobbyView`] every client
//! sees. DESIGN.md L957-971 (modes and rulesets), L1031 (client limit from upload; saturation refuses joins),
//! L1003 (kick, ban for the session, regenerate secret), L834 and L959 (co-op joins between operations,
//! rejoin within 5 minutes), L961 (sandbox joins any time), L945 (the other PvP team sees exterior hashes).
//!
//! The sortie rules themselves (team sizes, caps, every blueprint validated) are
//! `sim_systems::modes::validate_lobby`; this module calls it for `LobbyView::issues` and never re-implements
//! it. The host's own client is a member like any other (slot 0, `host = true`, joined over the in-memory
//! transport). A dedicated server has no host member.
//!
//! Choices this unit makes where the spec is silent (design decisions):
//! - The host is the first client admitted to a listen lobby (the session adds the in-memory transport of
//!   the host's own client before any other backend). It always gets slot 0, bypasses saturation and the
//!   player limit, and can never be kicked. Remote clients take the lowest free slot from 1.
//! - Admission order: ban, duplicate, held slot (a rejoin inside the hold skips saturation, limit and phase
//!   rules), saturation, player limit, phase rules. Held slots count as taken for the limit and for slot
//!   choice.
//! - Phase rules: campaign co-op admits in `Lobby` and `Theatre` only (between operations, L834; anything
//!   else is `OperationInProgress`); sandbox admits in every phase (L961); PvP and skirmish admit in `Lobby`
//!   only (`MatchInProgress`).
//! - A dedicated server carries min(floor(0.9 x upload / 256 kbps), mode maximum) clients, since there is no
//!   host player taking a seat.
//! - A held player's lobby entry (team, design, complexity) is kept beside the hold and restored un-ready on
//!   rejoin, so the player reclaims the same hull and team.
//! - Changing team or design clears ready; a new design's complexity is unknown (0) until the host app
//!   reports it with [`Lobby::set_complexity`]. A host `SetSetup` keeps the lobby's own player list and clears
//!   every ready flag, so nobody is committed to rules they did not see.
//! - `SetTeam` accepts teams 0 and 1 only, in PvP and sandbox (sparring teams); a PvP team already at the
//!   mode's largest offered team size is `TeamFull`. Other modes keep team 0.
//! - `Start` is host-only; on a dedicated server nobody sends it and the server starts on its own once
//!   [`Lobby::can_start`] holds.
//! - In PvP, a viewer sees the other team's designs as exterior hashes; when no exterior hash is known yet,
//!   or the viewer is not a member, the design is hidden (`None`), never the full hash. Blueprint issues of
//!   the other team are left out of that viewer's issue list (they name part slots); `can_start` still uses
//!   every issue.
//! - Names: control characters removed, whitespace trimmed, at most 24 characters. An empty name stays empty
//!   and the client shows its own placeholder (net produces no copy).
use crate::error::JoinRefusal;
use crate::msg::{Hello, LobbyMember, LobbyRequest, LobbyView};
use crate::session::SessionPhase;
use content::blueprint::Blueprint;
use content::catalogue::Catalogue;
use content::ids::{ContentHash, PlayerSlot};
use content::enums::GameMode;
use sim_systems::modes::{LobbyIssue, LobbyPlayer, LobbySetup};
use std::time::Duration;

/// Slot hold after a drop (L834).
pub const HOLD: Duration = Duration::from_secs(300);
/// Display names are trimmed to this many characters.
pub const MAX_NAME_CHARS: usize = 24;

#[derive(Clone, Debug, PartialEq)]
pub struct Member { pub client_id: u64, pub slot: PlayerSlot, pub name: String, pub host: bool, pub joined_at: Duration }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Admission { pub slot: PlayerSlot, pub rejoined: bool }

/// Why a lobby request was ignored (the client's control simply does not change; no message).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestDenied { NotHost, WrongPhase, NoSuchBlueprint, TeamFull }

pub struct Lobby {
    pub(crate) name: String,
    pub(crate) setup: LobbySetup,
    pub(crate) members: Vec<Member>,
    pub(crate) banned: std::collections::BTreeSet<u64>,
    /// Client id to (held slot, drop time).
    pub(crate) held: std::collections::BTreeMap<u64, (PlayerSlot, Duration)>,
    pub(crate) upload_kbps: u32,
    pub(crate) saturated: bool,
    pub(crate) dedicated: bool,
    /// Lobby entries of held players, restored on rejoin.
    held_players: std::collections::BTreeMap<u64, LobbyPlayer>,
    /// Blueprint upload progress per slot, shown in the lobby.
    uploads: std::collections::BTreeMap<PlayerSlot, f32>,
}

impl Lobby {
    pub fn new(name: String, setup: LobbySetup, upload_kbps: u32, dedicated: bool) -> Self {
        let mut setup = setup;
        // Players are the lobby's own record; a setup handed in carries none.
        setup.players.clear();
        Self { name, setup, members: Vec::new(), banned: Default::default(), held: Default::default(), upload_kbps, saturated: false, dedicated, held_players: Default::default(), uploads: Default::default() }
    }
    pub fn setup(&self) -> &LobbySetup { &self.setup }
    pub fn members(&self) -> &[Member] { &self.members }
    pub fn member(&self, slot: PlayerSlot) -> Option<&Member> { self.members.iter().find(|m| m.slot == slot) }
    pub fn member_by_client(&self, client_id: u64) -> Option<&Member> { self.members.iter().find(|m| m.client_id == client_id) }
    pub fn set_upload_kbps(&mut self, kbps: u32) { self.upload_kbps = kbps; }
    pub fn set_saturated(&mut self, on: bool) { self.saturated = on; }
    /// Upload-derived limit including the host: `sim_systems::modes::player_limit`. Real code.
    pub fn max_players(&self) -> u8 { sim_systems::modes::player_limit(self.setup.mode, self.setup.at_1_0, self.upload_kbps) }

    /// The limit admission enforces: [`Lobby::max_players`] for a listen host; a dedicated server seats only
    /// clients, min(floor(0.9 x upload / 256 kbps), mode maximum).
    fn seat_limit(&self) -> u8 {
        if !self.dedicated { return self.max_players(); }
        let max = content::ruleset::max_players(self.setup.mode, self.setup.at_1_0);
        let clients = ((0.9 * self.upload_kbps as f64) / 256.0).floor();
        let clients = if clients.is_finite() && clients > 0.0 { clients.min(u8::MAX as f64) as u8 } else { 0 };
        clients.min(max)
    }

    fn has_host(&self) -> bool { self.members.iter().any(|m| m.host) }

    fn slot_taken(&self, slot: PlayerSlot) -> bool {
        self.members.iter().any(|m| m.slot == slot) || self.held.values().any(|(s, _)| *s == slot)
    }

    fn lowest_free_slot(&self, from: u8) -> Option<PlayerSlot> {
        (from..=u8::MAX).map(PlayerSlot).find(|s| !self.slot_taken(*s))
    }

    fn player_mut(&mut self, slot: PlayerSlot) -> Option<&mut LobbyPlayer> { self.setup.players.iter_mut().find(|p| p.slot == slot) }
    fn player(&self, slot: PlayerSlot) -> Option<&LobbyPlayer> { self.setup.players.iter().find(|p| p.slot == slot) }

    /// Keep `setup.players` in slot order, so builders and views see a stable order.
    fn sort_players(&mut self) {
        self.setup.players.sort_by_key(|p| p.slot);
        self.members.sort_by_key(|m| m.slot);
    }

    /// Decide a join from its hello: version (checked earlier by the session), ban, duplicate, held slot
    /// (rejoin always allowed inside the hold), saturation, player limit, phase rules per mode. Assigns the
    /// lowest free slot and records the member. Unit: `net-lobby`.
    pub fn admit(&mut self, hello: &Hello, now: Duration, phase: SessionPhase) -> Result<Admission, JoinRefusal> {
        let id = hello.client_id;
        if self.banned.contains(&id) { return Err(JoinRefusal::Banned); }
        if self.member_by_client(id).is_some() { return Err(JoinRefusal::DuplicateClient); }
        let name = clean_name(&hello.name);

        // A hold past its time is released here too, whether or not the session expired it yet.
        if let Some(&(slot, since)) = self.held.get(&id) {
            self.held.remove(&id);
            let stash = self.held_players.remove(&id);
            if now.saturating_sub(since) < HOLD {
                let host = !self.dedicated && slot == PlayerSlot(0) && !self.has_host();
                self.members.push(Member { client_id: id, slot, name: name.clone(), host, joined_at: now });
                let mut player = stash.unwrap_or_else(|| new_player(slot, id, &name));
                player.slot = slot;
                player.client_id = id;
                player.name = name;
                player.ready = false;
                self.setup.players.retain(|p| p.slot != slot);
                self.setup.players.push(player);
                self.sort_players();
                return Ok(Admission { slot, rejoined: true });
            }
        }

        let host = !self.dedicated && !self.has_host() && !self.slot_taken(PlayerSlot(0));
        if !host {
            if self.saturated { return Err(JoinRefusal::HostSaturated); }
            let limit = self.seat_limit();
            let seated = self.members.len() + self.held.len();
            if seated >= limit as usize { return Err(JoinRefusal::LobbyFull { max_players: limit }); }
            match (self.setup.mode, phase) {
                (GameMode::Sandbox, _) => {}
                (GameMode::Campaign, SessionPhase::Lobby | SessionPhase::Theatre) => {}
                (GameMode::Campaign, _) => return Err(JoinRefusal::OperationInProgress),
                (_, SessionPhase::Lobby) => {}
                _ => return Err(JoinRefusal::MatchInProgress),
            }
        }
        let first = if host || self.dedicated { 0 } else { 1 };
        let Some(slot) = self.lowest_free_slot(first) else { return Err(JoinRefusal::LobbyFull { max_players: self.seat_limit() }) };
        self.members.push(Member { client_id: id, slot, name: name.clone(), host, joined_at: now });
        self.setup.players.retain(|p| p.slot != slot);
        self.setup.players.push(new_player(slot, id, &name));
        self.sort_players();
        Ok(Admission { slot, rejoined: false })
    }

    /// A member left or dropped. With `hold`, the slot is held for [`HOLD`] (mid-operation drops).
    /// Unit: `net-lobby`.
    pub fn remove(&mut self, client_id: u64, now: Duration, hold: bool) -> Option<PlayerSlot> {
        let i = self.members.iter().position(|m| m.client_id == client_id)?;
        let member = self.members.remove(i);
        let slot = member.slot;
        let player = self.setup.players.iter().position(|p| p.slot == slot).map(|j| self.setup.players.remove(j));
        self.uploads.remove(&slot);
        if hold {
            self.held.insert(client_id, (slot, now));
            if let Some(p) = player { self.held_players.insert(client_id, p); }
        }
        Some(slot)
    }

    /// Release holds older than [`HOLD`]; returns (client id, slot) of each. Unit: `net-lobby`.
    pub fn expire_holds(&mut self, now: Duration) -> Vec<(u64, PlayerSlot)> {
        let expired: Vec<(u64, PlayerSlot)> = self.held.iter().filter(|(_, (_, since))| now.saturating_sub(*since) >= HOLD).map(|(id, (slot, _))| (*id, *slot)).collect();
        for (id, _) in &expired {
            self.held.remove(id);
            self.held_players.remove(id);
        }
        expired
    }

    /// Held slots with the time left on each hold: (client id, slot, remaining), for the connection panel.
    pub fn held_slots(&self, now: Duration) -> Vec<(u64, PlayerSlot, Duration)> {
        self.held.iter().map(|(id, (slot, since))| (*id, *slot, HOLD.saturating_sub(now.saturating_sub(*since)))).collect()
    }

    /// Kick a member; with `ban` also ban the client id for the session. Returns the client id. Unit: `net-lobby`.
    ///
    /// A held slot can be kicked too (its hold is released). The host cannot be kicked.
    pub fn kick(&mut self, slot: PlayerSlot, ban: bool) -> Option<u64> {
        let id = if let Some(m) = self.member(slot) {
            if m.host { return None; }
            let id = m.client_id;
            self.members.retain(|m| m.client_id != id);
            self.setup.players.retain(|p| p.slot != slot);
            self.uploads.remove(&slot);
            id
        } else {
            let id = self.held.iter().find(|(_, (s, _))| *s == slot).map(|(id, _)| *id)?;
            self.held.remove(&id);
            self.held_players.remove(&id);
            id
        };
        if ban { self.banned.insert(id); }
        Some(id)
    }

    /// True when this client id is banned for the session.
    pub fn is_banned(&self, client_id: u64) -> bool { self.banned.contains(&client_id) }

    /// Complexity points of a member's chosen design, from its last trial (the host app reports it once the
    /// design is accepted). Non-finite or negative values count as over every cap in validation.
    pub fn set_complexity(&mut self, slot: PlayerSlot, points: f64) {
        if let Some(p) = self.player_mut(slot) { p.complexity = points; }
    }

    /// Blueprint upload progress for a member (`None` when no upload is running), shown in the lobby.
    pub fn set_upload_progress(&mut self, slot: PlayerSlot, progress: Option<f32>) {
        match progress {
            Some(f) if self.member(slot).is_some() => { self.uploads.insert(slot, if f.is_finite() { f.clamp(0.0, 1.0) } else { 0.0 }); }
            _ => { self.uploads.remove(&slot); }
        }
    }

    /// Largest team a PvP mode offers.
    fn max_team_size(&self) -> usize {
        match self.setup.mode {
            GameMode::PvpElimination => if self.setup.at_1_0 { 4 } else { 2 },
            GameMode::PvpEscort => 4,
            _ => content::ruleset::max_players(self.setup.mode, self.setup.at_1_0) as usize,
        }
    }

    /// Apply a member's request to the setup (host-only requests checked). Unit: `net-lobby`.
    pub fn apply(&mut self, slot: PlayerSlot, request: &LobbyRequest, phase: SessionPhase) -> Result<(), RequestDenied> {
        let Some(member) = self.member(slot) else { return Err(RequestDenied::NotHost) };
        let host = member.host;
        let mode = self.setup.mode;
        let pvp = matches!(mode, GameMode::PvpElimination | GameMode::PvpEscort);
        match request {
            LobbyRequest::SetReady(on) => {
                if phase != SessionPhase::Lobby { return Err(RequestDenied::WrongPhase); }
                let p = self.player_mut(slot).ok_or(RequestDenied::NotHost)?;
                p.ready = *on;
            }
            LobbyRequest::SetTeam(team) => {
                if phase != SessionPhase::Lobby { return Err(RequestDenied::WrongPhase); }
                let team = *team;
                if !(pvp || mode == GameMode::Sandbox) {
                    if team != 0 { return Err(RequestDenied::TeamFull); }
                    return Ok(());
                }
                if team > 1 { return Err(RequestDenied::TeamFull); }
                let current = self.player(slot).map(|p| p.team);
                if current == Some(team) { return Ok(()); }
                if pvp {
                    let on_team = self.setup.players.iter().filter(|p| p.team == team && p.slot != slot).count();
                    if on_team >= self.max_team_size() { return Err(RequestDenied::TeamFull); }
                }
                let p = self.player_mut(slot).ok_or(RequestDenied::NotHost)?;
                p.team = team;
                p.ready = false;
            }
            LobbyRequest::SetBlueprint(hash) => {
                // A sandbox joiner picks a design mid-sortie (L961); elsewhere designs are chosen in the lobby.
                if !(phase == SessionPhase::Lobby || mode == GameMode::Sandbox) { return Err(RequestDenied::WrongPhase); }
                if hash.is_empty() { return Err(RequestDenied::NoSuchBlueprint); }
                let p = self.player_mut(slot).ok_or(RequestDenied::NotHost)?;
                if p.blueprint != Some(*hash) {
                    p.blueprint = Some(*hash);
                    p.complexity = 0.0;
                    p.ready = false;
                }
                self.uploads.remove(&slot);
            }
            LobbyRequest::SetSetup(setup) => {
                if !host { return Err(RequestDenied::NotHost); }
                if phase != SessionPhase::Lobby { return Err(RequestDenied::WrongPhase); }
                let mut players = std::mem::take(&mut self.setup.players);
                // Teams outside the new mode's range fall back to team 0.
                let keeps_team = matches!(setup.mode, GameMode::PvpElimination | GameMode::PvpEscort | GameMode::Sandbox);
                for p in &mut players {
                    p.ready = false;
                    if !keeps_team || p.team > 1 { p.team = 0; }
                }
                self.setup = (**setup).clone();
                self.setup.players = players;
            }
            LobbyRequest::Start => {
                if !host { return Err(RequestDenied::NotHost); }
                if phase != SessionPhase::Lobby { return Err(RequestDenied::WrongPhase); }
            }
        }
        Ok(())
    }
    /// The host app changes the setup directly (mode options, map, weather, ruleset). Real code.
    pub fn setup_mut(&mut self) -> &mut LobbySetup { &mut self.setup }
    /// The view for one recipient: blueprint hashes of the other PvP team are replaced by their exterior
    /// hashes (`exterior_of`). Issues come from `validate_lobby` over `blueprints`. Unit: `net-lobby`.
    pub fn view_for(&self, viewer: PlayerSlot, blueprints: &[Blueprint], catalogue: &Catalogue, exterior_of: &dyn Fn(&ContentHash) -> Option<ContentHash>) -> LobbyView {
        let pvp = matches!(self.setup.mode, GameMode::PvpElimination | GameMode::PvpEscort);
        let viewer_team = self.player(viewer).map(|p| p.team);
        let opponent = |team: u8| pvp && viewer_team != Some(team);
        let mut members: Vec<LobbyMember> = self.members.iter().map(|m| {
            let p = self.player(m.slot);
            let team = p.map_or(0, |p| p.team);
            let blueprint = p.and_then(|p| p.blueprint).and_then(|h| if opponent(team) { exterior_of(&h) } else { Some(h) });
            LobbyMember {
                slot: m.slot, name: m.name.clone(), host: m.host, team, ready: p.is_some_and(|p| p.ready), blueprint,
                complexity: p.map_or(0.0, |p| p.complexity), upload: self.uploads.get(&m.slot).copied(),
            }
        }).collect();
        members.sort_by_key(|m| m.slot);
        let all = sim_systems::modes::validate_lobby(&self.setup, blueprints, catalogue);
        let can_start = self.start_ok(&all);
        let issues = all.into_iter().filter(|i| match i {
            LobbyIssue::Blueprint { player, .. } => !self.player(*player).is_some_and(|p| opponent(p.team)),
            _ => true,
        }).collect();
        LobbyView { mode: self.setup.mode, name: self.name.clone(), ruleset: self.setup.ruleset.clone(), members, max_players: self.seat_limit(), issues, can_start, saturated: self.saturated }
    }

    fn start_ok(&self, issues: &[LobbyIssue]) -> bool {
        !self.members.is_empty() && issues.is_empty() && self.setup.players.iter().all(|p| p.ready)
    }

    /// All members ready, no blocking issue. Unit: `net-lobby`.
    pub fn can_start(&self, blueprints: &[Blueprint], catalogue: &Catalogue) -> bool {
        if self.members.is_empty() || !self.setup.players.iter().all(|p| p.ready) { return false; }
        let issues = sim_systems::modes::validate_lobby(&self.setup, blueprints, catalogue);
        self.start_ok(&issues)
    }
}

fn new_player(slot: PlayerSlot, client_id: u64, name: &str) -> LobbyPlayer {
    LobbyPlayer { slot, client_id, name: name.to_string(), team: 0, blueprint: None, ready: false, complexity: 0.0 }
}

/// Display name as the lobby keeps it: no control characters, trimmed, at most [`MAX_NAME_CHARS`] characters.
pub(crate) fn clean_name(raw: &str) -> String {
    let no_ctrl: String = raw.chars().filter(|c| !c.is_control()).collect();
    let trimmed: String = no_ctrl.trim().chars().take(MAX_NAME_CHARS).collect();
    trimmed.trim_end().to_string()
}

#[cfg(test)]
mod tests;
