//! Unit `sys-objectives`. Objective and ghost-condition tracking, the per-vessel last-damage ledger, kill
//! records and assembly of the sortie result. DESIGN.md L778-782 (objectives, ghost conditions), L749 (kill
//! observed or unobserved, sightings, evidence), L772 (kill method, range band, time band, emission state),
//! L686 (salvage recorded when a vessel is lost), L537 (end states), L227 (trial score inputs).
//!
//! `SortieResult` is the only channel from a sortie into the theatre, so everything the ledger needs is
//! gathered here from the physical event stream and the systems' states.
//!
//! Rules this unit settles where the spec is silent (design decisions):
//! - Hull facts (config index, role, tag, hull record, cost, starting loads) are bound when a vessel first
//!   appears, so they survive the ship table dropping a lost hull. The Sim should call [`note_spawn`] with
//!   the config index; without it the index is inferred from spawn order (config order sorted by arrival).
//! - Kill records are kept for hulls of sides hostile to the players (every side but the players' own and
//!   their allies that is Combine-faction or hostile either way); in PvP for every crewed hull. One record per
//!   hull: Disabled is upgraded to Lost or Captured in place. Wreck pieces never get one.
//! - `observed`: a surviving unit of the victim's side or an ally (never the victim itself) held the killer
//!   or the victim in its own detections of the last second, or the side's picture held a non-Friend track on
//!   the killer updated within the last second.
//! - Damage with no attacker (fire, flooding) keeps the last attributed attacker, range and emission state.
//!   `player_buckets` counts damaged cells (each change raises a cell at least one bucket), not bucket sums.
//! - Ghost conditions pay only when the primary objective is done. "No losses" counts lost hulls of the
//!   player side; "never detected" fails on the first non-Friend track an enemy side holds on a player hull.
//! - Withdrawal: once every player with a hull afloat has asked to withdraw and those hulls have been clear
//!   of contact for [`CLEAR_HOLD_S`] (no enemy picture holds a track on them updated in the last
//!   [`CLEAR_TRACK_AGE_S`], and no enemy hull is within [`CLEAR_RANGE_M`]), the sortie ends Withdrawn. It
//!   also ends Withdrawn as soon as every one of those hulls is within [`EXIT_RADIUS_M`] of an exit marker:
//!   a hull that sails out through an exit has left the area even with a pursuer astern.
//! - Elimination: a team is out when none of its hulls is Active (Disabled hulls are out of the fight). The
//!   rule waits until both teams have fielded a hull. Time limit: campaign and skirmish end Withdrawn, PvP
//!   elimination a Draw, PvP escort a win for the side stopping the convoy.
//! - A Victory brings home every afloat prize under sail (the area is won). Survivor records list enemy
//!   hulls with a hull record that were neither lost nor captured, Disabled ones included.
//! - The minefield of a mine-laying objective counts player mines within [`MINEFIELD_RADIUS_M`] of its marker;
//!   a site objective counts Site-role hulls within [`SITE_RADIUS_M`] of its marker.
use crate::ai::commander::CommanderState;
use crate::prelude::*;
use crate::protocol::config::{Faction, KillFate, KillRecord, ModeRules, Outcome, PlayerResult, PrizeRecord, SideDef, SortieConfig, SortieResult, SortieStats, SortieSummary, SurvivorRecord, VesselRole};
use crate::protocol::command::PrizeDecision;
use crate::protocol::snapshot::{ObjectiveState, ObjectiveView};
use crate::ship::ShipTable;
use crate::specops::{SpecOpsState, EXIT_RADIUS_M};
use crate::tracks::Pictures;
use content::campaign::{ConstraintTag, GhostCondition, GhostKind, KillMethod, Marker, MarkerKind, Objective, RangeBand, StationAction};
use content::cell::CellChange;
use content::part::weapons::MunitionKind;
use content::part::{PartFunction, SensorKind};
use content::save::HullDamage;
use sim_core::damage::DamageCause;
use sim_core::events::PhysEvent;
use sim_core::parts::PartState;
use sim_core::time::DT;
use sim_core::vessel::{LossCause, Vessel, VesselStatus};

/// DECISION: withdrawal clear-of-contact test (see module docs).
pub const CLEAR_TRACK_AGE_S: f64 = 30.0;
pub const CLEAR_RANGE_M: f64 = 10_000.0;
pub const CLEAR_HOLD_S: f64 = 10.0;
/// DECISION: radius around a mine-laying objective's marker in which laid mines count.
pub const MINEFIELD_RADIUS_M: f64 = 2_000.0;
/// DECISION: radius around a site objective's marker in which Site-role hulls belong to the site.
pub const SITE_RADIUS_M: f64 = 1_500.0;
/// DECISION: a wreck that sank in water this shallow or less is grounded and salvageable (the 20 m despawn
/// depth, L686).
pub const SALVAGE_DEPTH_M: f64 = 20.0;
/// DECISION (owner ruling, 22 November 2026; L537 is silent on what a disabled player hull does to a sortie): in
/// objective mode the sortie is a defeat once every player hull is lost or has been disabled this long without
/// a break. Five minutes leaves Damage Control time to restore propulsion; a hull that regains it restarts the
/// count.
pub const PLAYER_DISABLED_DEFEAT_S: f64 = 300.0;
/// Objectives and end rules are evaluated at 10 Hz.
const EVAL_HZ: u32 = 10;
/// A kill is observed through a picture track updated this recently, in ticks (one second).
const OBSERVE_FRESH_TICKS: u64 = 60;

/// Last damage each vessel took: who, with what, from how far. `VesselLost` carries only a `LossCause`, so
/// the kill method comes from here.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LastDamage { pub vessel: EntityId, pub attacker: EntityId, pub cause: DamageCause, pub tick: u64, pub range_m: f64, pub attacker_emcon: Emcon, /// Damage buckets dealt by player-side attackers in total, for kill credit.
    pub player_buckets: u32 }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DamageLedger { pub rows: Vec<LastDamage> }
impl DamageLedger {
    pub fn get(&self, vessel: EntityId) -> Option<&LastDamage> { self.rows.binary_search_by(|r| r.vessel.cmp(&vessel)).ok().map(|i| &self.rows[i]) }
    /// Fold one damage application in. `attacker` NONE (fire, flooding) keeps the attributed attacker, range
    /// and emission state of the previous row.
    pub fn record(&mut self, vessel: EntityId, attacker: EntityId, cause: DamageCause, tick: u64, range_m: f64, emcon: Emcon, player_cells: u32) {
        match self.rows.binary_search_by(|r| r.vessel.cmp(&vessel)) {
            Ok(i) => {
                let r = &mut self.rows[i];
                r.cause = cause;
                r.tick = tick;
                r.player_buckets = r.player_buckets.saturating_add(player_cells);
                if !attacker.is_none() { r.attacker = attacker; r.range_m = range_m; r.attacker_emcon = emcon; }
            }
            Err(i) => self.rows.insert(i, LastDamage { vessel, attacker, cause, tick, range_m, attacker_emcon: emcon, player_buckets: player_cells }),
        }
    }
}

/// Kill method from a damage cause and the weapon that dealt it. Real code: the mapping is contract.
pub fn kill_method(cause: DamageCause, captured: bool) -> KillMethod {
    if captured { return KillMethod::Capture; }
    match cause { DamageCause::Shell => KillMethod::Gunfire, DamageCause::Missile => KillMethod::Missile, DamageCause::Torpedo => KillMethod::Torpedo, DamageCause::Mine => KillMethod::Mine, DamageCause::DepthCharge => KillMethod::DepthCharge, DamageCause::CookOff | DamageCause::Fire => KillMethod::Fire, DamageCause::Collision | DamageCause::Grounding => KillMethod::Collision }
}
/// Engagement-range band of a kill: Close under 2 km, Medium under 8 km, Long under the killer's radar horizon
/// to the victim, else BeyondHorizon. DECISION (the spec names the bands' purpose at L772 but not their
/// edges): these edges follow the gun class caps at L489. Real code.
pub fn range_band(range_m: f64, horizon_m: f64) -> RangeBand { if range_m < 2_000.0 { RangeBand::Close } else if range_m < 8_000.0 { RangeBand::Medium } else if range_m <= horizon_m { RangeBand::Long } else { RangeBand::BeyondHorizon } }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObjectiveTracker { pub objective: Objective, pub state: ObjectiveState, pub progress: f64, pub started_tick: u64 }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GhostTracker { pub ghost: GhostCondition, pub state: ObjectiveState }

/// Facts about one crewed hull, bound when it first appears and refreshed at 10 Hz.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HullFacts {
    pub vessel: EntityId,
    /// Index into `SortieConfig::vessels`.
    pub index: Option<u16>,
    pub side: SideId,
    pub class: Option<ClassId>,
    pub player: Option<PlayerSlot>,
    pub tonnage_t: f64,
    pub cost: u32,
    pub role: VesselRole,
    pub tag: String,
    pub hull_record: Option<u32>,
    /// Counts for kills, tonnage shares and DestroyAll.
    pub enemy: bool,
    /// On a players' side (player hulls, the tender, escorted ships that joined).
    pub player_side: bool,
    pub lost: bool,
    pub disabled: bool,
    pub pos: [f64; 2],
    pub fuel_l0: f64,
    pub ammo_kg0: BTreeMap<u16, f64>,
    pub rounds0: u64,
    pub passengers_lost: u16,
    pub expended: BTreeMap<MunitionId, u16>,
    /// Exit markers reached (latched), by name: convoys, escorted ships and prizes.
    pub exits: Vec<String>,
    /// Exit markers the hull is at now, by name: player hulls must be there at the moment that counts.
    #[serde(default)]
    pub at_exits: Vec<String>,
    /// Solid cells at spawn, for the share of own cells lost.
    #[serde(default)]
    pub cells0: u32,
    /// The hull has struck its colours at some evaluation (latched): a prize that is later captured, which
    /// clears the world's flag, still counts in the Struck colours row.
    #[serde(default)]
    pub struck: bool,
    /// Tick the hull's status last became Disabled; `None` while it is Active, lost or never disabled.
    #[serde(default)]
    pub disabled_since: Option<u64>,
}
impl HullFacts {
    pub fn out(&self) -> bool { self.lost || self.disabled }
    pub fn afloat(&self) -> bool { !self.lost }
}

/// Why an objective-mode sortie ended (set with the outcome).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EndReason {
    ObjectiveDone,
    ObjectiveFailed,
    /// Every player hull was lost.
    PlayerHullsLost,
    /// Every player hull was lost or disabled for [`PLAYER_DISABLED_DEFEAT_S`] without a break, at least one
    /// still afloat.
    PlayerHullsDisabled,
    TimeLimit,
    Withdrew,
}
impl EndReason {
    /// Player-facing text key for the reasons the objective text does not already say, `None` for the rest.
    pub fn text_key(self) -> Option<&'static str> { match self { EndReason::PlayerHullsDisabled => Some("objective.end.hulls_disabled"), _ => None } }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ObjectivesState {
    pub primary: Option<ObjectiveTracker>,
    pub ghosts: Vec<GhostTracker>,
    pub ledger: DamageLedger,
    pub kills: Vec<KillRecord>,
    pub stats: SortieStats,
    pub passengers_lost: u16,
    pub tender_lost: bool,
    pub site_destroyed: bool,
    /// Set once the sortie is decided.
    pub outcome: Option<Outcome>,
    pub winner: Option<SideId>,
    pub ended_tick: Option<u64>,
    /// Why an objective-mode sortie ended; `None` until decided and in the other modes.
    #[serde(default)]
    pub end_reason: Option<EndReason>,
    /// Players who asked to withdraw.
    pub withdrawing: Vec<PlayerSlot>,
    /// Hull facts ascending by vessel.
    #[serde(default)]
    pub(crate) hulls: Vec<HullFacts>,
    /// Vessel of each kill record, same order as `kills`.
    #[serde(default)]
    pub(crate) kill_vessels: Vec<EntityId>,
    /// Small bodies and mines: (body, launcher, kind code) ascending by body. Kind 1 missile, 2 torpedo, 3 mine.
    #[serde(default)]
    pub(crate) bodies: Vec<(EntityId, EntityId, u8)>,
    /// Last player to damage each vessel, for kill credit.
    #[serde(default)]
    pub(crate) credit: BTreeMap<EntityId, PlayerSlot>,
    /// Mines laid by the player side, (x, z).
    #[serde(default)]
    pub(crate) mines: Vec<[f64; 2]>,
    #[serde(default)]
    pub(crate) player_hit: bool,
    #[serde(default)]
    pub(crate) radiated: bool,
    #[serde(default)]
    pub(crate) detected_tick: Option<u64>,
    #[serde(default)]
    pub(crate) contact: bool,
    #[serde(default)]
    pub(crate) feint_held_s: f64,
    #[serde(default)]
    pub(crate) last_eval_tick: Option<u64>,
    #[serde(default)]
    pub(crate) primary_done_tick: Option<u64>,
    #[serde(default)]
    pub(crate) clear_since: Option<u64>,
    /// Last reported (state, percent) per objective id.
    #[serde(default)]
    pub(crate) reported: Vec<(ObjectiveState, u8)>,
    /// Hold range and time in force, for the extraction objective's text.
    #[serde(default)]
    pub(crate) hold_text: Option<(f64, f64)>,
}
impl ObjectivesState {
    pub(crate) fn hull(&self, v: EntityId) -> Option<&HullFacts> { self.hulls.binary_search_by(|h| h.vessel.cmp(&v)).ok().map(|i| &self.hulls[i]) }
    fn hull_mut(&mut self, v: EntityId) -> Option<&mut HullFacts> { match self.hulls.binary_search_by(|h| h.vessel.cmp(&v)) { Ok(i) => Some(&mut self.hulls[i]), Err(_) => None } }
    fn hull_of_index(&self, idx: u16) -> Option<&HullFacts> { self.hulls.iter().find(|h| h.index == Some(idx)) }
    fn body(&self, b: EntityId) -> Option<(EntityId, u8)> { self.bodies.binary_search_by(|r| r.0.cmp(&b)).ok().map(|i| (self.bodies[i].1, self.bodies[i].2)) }
    fn add_body(&mut self, b: EntityId, launcher: EntityId, kind: u8) { if let Err(i) = self.bodies.binary_search_by(|r| r.0.cmp(&b)) { self.bodies.insert(i, (b, launcher, kind)); } }
    fn player_side_entity(&self, e: EntityId) -> bool { self.hull(e).map_or(false, |h| h.player_side) }
}

// ---------------------------------------------------------------------------------------------------------
// Side relations
// ---------------------------------------------------------------------------------------------------------

fn side_def(config: &SortieConfig, s: SideId) -> Option<&SideDef> { config.sides.iter().find(|d| d.id == s) }

fn is_pvp(config: &SortieConfig) -> bool { matches!(config.rules, ModeRules::Elimination | ModeRules::Escort { .. }) }

/// Sides that count as "the players": every side a player is on, plus Players-faction sides.
fn player_sides(config: &SortieConfig) -> SideMask {
    let mut m = SideMask::NONE;
    for p in &config.players { m = m.with(p.side); }
    for s in &config.sides { if s.faction == Faction::Players { m = m.with(s.id); } }
    m
}

/// Sides allied with a side (always including itself).
fn allies(config: &SortieConfig, s: SideId) -> SideMask { side_def(config, s).map_or(SideMask::only(s), |d| d.allied.with(s)) }

/// A side the players fight (outside PvP).
fn enemy_of_players(config: &SortieConfig, s: SideId) -> bool {
    let players = player_sides(config);
    if players.has(s) { return false; }
    if players.iter().any(|p| allies(config, p).has(s)) { return false; }
    let Some(d) = side_def(config, s) else { return false };
    d.faction == Faction::Combine || !(d.hostile_to.0 & players.0 == 0) || players.iter().any(|p| side_def(config, p).map_or(false, |pd| pd.hostile_to.has(s)))
}

fn first_enemy_side(config: &SortieConfig) -> Option<SideId> { config.sides.iter().find(|d| enemy_of_players(config, d.id)).map(|d| d.id) }
fn first_player_side(config: &SortieConfig) -> Option<SideId> { config.players.first().map(|p| p.side).or_else(|| config.sides.iter().find(|d| d.faction == Faction::Players).map(|d| d.id)) }

// ---------------------------------------------------------------------------------------------------------
// Init and spawn binding
// ---------------------------------------------------------------------------------------------------------

/// Build trackers from the config. Unit: `sys-objectives`.
pub fn init(config: &SortieConfig) -> ObjectivesState {
    let mut s = ObjectivesState {
        primary: Some(ObjectiveTracker { objective: config.objective.clone(), state: ObjectiveState::Active, progress: 0.0, started_tick: 0 }),
        ghosts: config.ghosts.iter().map(|g| GhostTracker { ghost: g.clone(), state: ObjectiveState::Active }).collect(),
        ..Default::default()
    };
    if !matches!(config.rules, ModeRules::Objective) { if let Some(p) = s.primary.as_mut() { p.state = ObjectiveState::Hidden; } }
    s
}

/// Bind a spawned vessel to its `SortieConfig::vessels` index (the Sim knows it from its schedule). Optional:
/// without it the index is inferred from spawn order.
pub fn note_spawn(state: &mut ObjectivesState, vessel: EntityId, config_index: u16, world: &World, ships: &ShipTable, config: &SortieConfig) {
    if let Some(h) = state.hull_mut(vessel) { if h.index != Some(config_index) { h.index = Some(config_index); refresh_from_config(h, config); } return; }
    bind_hull(state, vessel, Some(config_index), world, ships, config);
}

/// A missile was seduced by a player decoy (guidance event; the Sim forwards it).
pub fn note_seduced(state: &mut ObjectivesState) { state.stats.missiles_seduced = state.stats.missiles_seduced.saturating_add(1); }
/// An alert was raised on a player hull (the alert manager forwards it).
pub fn note_alert_raised(state: &mut ObjectivesState) { state.stats.alerts_raised = state.stats.alerts_raised.saturating_add(1); }

/// Config indices in spawn order: sorted by arrival time, then index.
fn spawn_order(config: &SortieConfig) -> Vec<u16> {
    let mut v: Vec<u16> = (0..config.vessels.len().min(u16::MAX as usize) as u16).collect();
    v.sort_by_key(|&i| (config.vessels[i as usize].arrives_s, i));
    v
}

fn refresh_from_config(h: &mut HullFacts, config: &SortieConfig) {
    let Some(init) = h.index.and_then(|i| config.vessels.get(i as usize)) else { return };
    h.role = init.role;
    if h.tag.is_empty() { h.tag = init.tag.clone(); }
    h.hull_record = init.hull_record;
    if h.class.is_none() { h.class = init.class.clone(); }
}

fn ensure_hull(state: &mut ObjectivesState, vessel: EntityId, world: &World, ships: &ShipTable, config: &SortieConfig) {
    if state.hull(vessel).is_some() || vessel.is_none() || world.vessel(vessel).map_or(true, |v| v.parent.is_some()) { return; }
    // Inference follows spawn order, so bind every earlier unbound hull first.
    let earlier: Vec<EntityId> = world.vessels().map(|(_, v)| v).filter(|v| v.parent.is_none() && v.id.index < vessel.index && state.hull(v.id).is_none()).map(|v| v.id).collect();
    for e in earlier { bind_hull(state, e, None, world, ships, config); }
    bind_hull(state, vessel, None, world, ships, config);
}

fn bind_hull(state: &mut ObjectivesState, vessel: EntityId, index: Option<u16>, world: &World, ships: &ShipTable, config: &SortieConfig) {
    let Some(v) = world.vessel(vessel) else { return };
    if v.parent.is_some() { return; }
    let index = index.or_else(|| {
        let used: BTreeSet<u16> = state.hulls.iter().filter_map(|h| h.index).collect();
        let fits = |i: &u16| { let c = &config.vessels[*i as usize]; !used.contains(i) && c.side == v.side && (c.class.is_none() || v.class.is_none() || c.class == v.class) };
        spawn_order(config).into_iter().find(|i| fits(i))
    });
    let ship = ships.get(vessel);
    let catalogue = world.catalogue();
    let mut fuel = 0.0;
    let mut kg: BTreeMap<u16, f64> = BTreeMap::new();
    for p in &v.parts {
        match p.state {
            PartState::FuelTank { litres } => fuel += litres.max(0.0),
            PartState::Magazine { ammo_kg, .. } => {
                if let Some(PartFunction::Magazine { calibre_mm, .. }) = catalogue.parts.get_index(p.def.index()).map(|(_, d)| &d.function) { *kg.entry(*calibre_mm).or_insert(0.0) += ammo_kg.max(0.0); }
            }
            _ => {}
        }
    }
    let players = player_sides(config);
    let mut h = HullFacts {
        vessel, index, side: v.side, class: v.class.clone(), player: v.player.or_else(|| ship.and_then(|s| s.player)),
        tonnage_t: if v.tonnage_t > 0.0 { v.tonnage_t } else { ship.map_or(0.0, |s| s.design.tonnage_t) },
        cost: ship.map_or(0, |s| s.design.cost), tag: ship.map_or_else(String::new, |s| s.tag.clone()),
        enemy: if is_pvp(config) { true } else { enemy_of_players(config, v.side) },
        player_side: players.has(v.side) || v.player.is_some(),
        pos: [v.kin.pose.pos.x, v.kin.pose.pos.z], fuel_l0: fuel, ammo_kg0: kg, rounds0: rounds_of(v), cells0: v.grid.original_count,
        ..Default::default()
    };
    refresh_from_config(&mut h, config);
    if h.role == VesselRole::PlayerHull { h.player_side = true; }
    let i = state.hulls.partition_point(|x| x.vessel < vessel);
    state.hulls.insert(i, h);
}

/// Rounds, ready missiles, mines and decoy rounds aboard.
fn rounds_of(v: &Vessel) -> u64 {
    v.parts.iter().map(|p| match p.state {
        PartState::Magazine { rounds, .. } => rounds as u64,
        PartState::Launcher { ready, .. } => ready as u64,
        PartState::MineRail { mines, .. } => mines as u64,
        PartState::Decoy { rounds, .. } => rounds as u64,
        _ => 0,
    }).sum()
}

// ---------------------------------------------------------------------------------------------------------
// Ingest
// ---------------------------------------------------------------------------------------------------------

fn munition_kind_code(world: &World, kind: content::ids::MunitionIdx) -> (u8, Option<MunitionId>) {
    match world.catalogue().munitions.get_index(kind.index()) {
        Some((id, def)) => (match def.kind { MunitionKind::AntiShipMissile | MunitionKind::Sam | MunitionKind::PdMissile => 1, MunitionKind::Torpedo => 2, MunitionKind::Mine => 3, _ => 0 }, Some(id.clone())),
        None => (0, None),
    }
}

/// Fold this tick's physical events into the ledger, kill list and stats: `CellsDamaged` updates last damage
/// (range from attacker to victim now, attacker's emission setting); `VesselLost` and Disabled status make a
/// `KillRecord` whose `observed` flag is true when any surviving unit of the victim's side holds the killer or
/// the victim inside its sensors at that tick (`observers`). Unit: `sys-objectives`.
pub fn ingest(state: &mut ObjectivesState, events: &[PhysEvent], world: &World, ships: &ShipTable, pictures: &Pictures, config: &SortieConfig, ctx: &SimCtx<'_>) {
    let tick = ctx.tick.0;
    for ev in events {
        match ev {
            PhysEvent::VesselSpawned { vessel, .. } => ensure_hull(state, *vessel, world, ships, config),
            PhysEvent::CellsDamaged { vessel, changes, cause, attacker } => {
                ensure_hull(state, *vessel, world, ships, config);
                let attacker = state.body(*attacker).map_or(*attacker, |(l, _)| l);
                let victim = world.vessel(*vessel).map(|v| v.kin.pose.pos);
                let (range, emcon) = if attacker.is_none() { (0.0, Emcon::default()) } else {
                    let apos = world.vessel(attacker).map(|v| v.kin.pose.pos).or_else(|| world.locate(attacker).map(|l| l.0));
                    let range = match (apos, victim) { (Some(a), Some(b)) => DVec2::new(a.x - b.x, a.z - b.z).length(), _ => 0.0 };
                    (range, ships.get(attacker).map_or(Emcon::default(), |s| s.settings.emcon))
                };
                let attacker_player = state.hull(attacker).and_then(|h| h.player).or_else(|| world.vessel(attacker).and_then(|v| v.player));
                let player_attack = !attacker.is_none() && (attacker_player.is_some() || state.player_side_entity(attacker));
                let cells = if player_attack { changes.len().min(u32::MAX as usize) as u32 } else { 0 };
                state.ledger.record(*vessel, attacker, *cause, tick, range, emcon, cells);
                if let Some(p) = attacker_player { if !changes.is_empty() { state.credit.insert(*vessel, p); } }
                if !changes.is_empty() && state.player_side_entity(*vessel) { state.player_hit = true; }
            }
            PhysEvent::MountFired { vessel, rounds, .. } => { if state.player_side_entity(*vessel) { state.stats.rounds_fired = state.stats.rounds_fired.saturating_add(*rounds as u32); } }
            PhysEvent::ShellImpact { vessel, shooter, .. } => {
                if state.player_side_entity(*shooter) && opposed(state, config, *shooter, *vessel) { state.stats.rounds_hit = state.stats.rounds_hit.saturating_add(1); }
            }
            PhysEvent::BodySpawned { body, kind, launcher, .. } => {
                let (code, id) = munition_kind_code(world, *kind);
                state.add_body(*body, *launcher, code);
                if state.player_side_entity(*launcher) {
                    match code { 1 => state.stats.missiles_fired = state.stats.missiles_fired.saturating_add(1), 2 => state.stats.torpedoes_fired = state.stats.torpedoes_fired.saturating_add(1), _ => {} }
                }
                if let (Some(id), Some(h)) = (id, state.hull_mut(*launcher)) { let n = h.expended.entry(id).or_insert(0); *n = n.saturating_add(1); }
            }
            PhysEvent::MineLaid { mine, layer, pos } => {
                state.add_body(*mine, *layer, 3);
                if state.player_side_entity(*layer) { state.mines.push([pos.x, pos.z]); }
                let rail_mine = world.vessel(*layer).and_then(|v| v.parts.iter().find_map(|p| match world.catalogue().parts.get_index(p.def.index()).map(|(_, d)| &d.function) { Some(PartFunction::MineRail { mine, .. }) => Some(mine.clone()), _ => None }));
                if let (Some(id), Some(h)) = (rail_mine, state.hull_mut(*layer)) { let n = h.expended.entry(id).or_insert(0); *n = n.saturating_add(1); }
            }
            PhysEvent::BodyImpact { body, vessel, cause, .. } => {
                if let Some((launcher, _)) = state.body(*body) {
                    if state.player_side_entity(launcher) && opposed(state, config, launcher, *vessel) {
                        match cause { DamageCause::Missile => state.stats.missiles_hit = state.stats.missiles_hit.saturating_add(1), DamageCause::Torpedo => state.stats.torpedoes_hit = state.stats.torpedoes_hit.saturating_add(1), _ => {} }
                    }
                }
            }
            PhysEvent::DecoyDeployed { owner, .. } => { if state.player_side_entity(*owner) { state.stats.decoys_fired = state.stats.decoys_fired.saturating_add(1); } }
            PhysEvent::FireStarted { vessel, .. } => { if state.player_side_entity(*vessel) { state.stats.fires = state.stats.fires.saturating_add(1); } }
            PhysEvent::PassengersLost { vessel, count } => {
                ensure_hull(state, *vessel, world, ships, config);
                if let Some(h) = state.hull_mut(*vessel) {
                    h.passengers_lost = h.passengers_lost.saturating_add(*count);
                    if h.player_side { state.passengers_lost = state.passengers_lost.saturating_add(*count); }
                }
            }
            PhysEvent::StatusChanged { vessel, status } => {
                ensure_hull(state, *vessel, world, ships, config);
                if *status == VesselStatus::Disabled {
                    if let Some(h) = state.hull_mut(*vessel) { h.disabled = true; }
                    record_kill(state, *vessel, KillFate::Disabled, None, world, ships, pictures, config, ctx);
                } else if *status == VesselStatus::Active {
                    if let Some(h) = state.hull_mut(*vessel) { h.disabled = false; }
                    unrecord_disabled(state, *vessel);
                }
            }
            PhysEvent::VesselLost { vessel, cause, tonnage_t, .. } => {
                ensure_hull(state, *vessel, world, ships, config);
                if let Some(h) = state.hull_mut(*vessel) {
                    h.lost = true;
                    if h.tonnage_t <= 0.0 && *tonnage_t > 0.0 { h.tonnage_t = *tonnage_t; }
                    match h.role { VesselRole::Tender => state.tender_lost = true, VesselRole::Site => state.site_destroyed = true, _ => {} }
                }
                record_kill(state, *vessel, KillFate::Lost, Some(*cause), world, ships, pictures, config, ctx);
            }
            _ => {}
        }
    }
}

/// Two hulls on sides that are neither the same nor allied (a hit on the other team counts in PvP too).
fn opposed(state: &ObjectivesState, config: &SortieConfig, a: EntityId, b: EntityId) -> bool {
    match (state.hull(a), state.hull(b)) { (Some(x), Some(y)) => x.side != y.side && !allies(config, x.side).has(y.side), _ => false }
}

/// A hull that regains propulsion was not taken out of the fight: drop its Disabled record.
fn unrecord_disabled(state: &mut ObjectivesState, vessel: EntityId) {
    let Some(i) = state.kill_vessels.iter().position(|v| *v == vessel) else { return };
    if state.kills[i].fate != KillFate::Disabled { return; }
    let k = state.kills.remove(i);
    state.kill_vessels.remove(i);
    if let Some(n) = state.stats.kills_by_method.get_mut(&k.method) { *n = n.saturating_sub(1); if *n == 0 { state.stats.kills_by_method.remove(&k.method); } }
}

/// The observers rule (see module docs).
fn observed(victim: EntityId, killer: EntityId, victim_side: SideId, ships: &ShipTable, world: &World, pictures: &Pictures, config: &SortieConfig, tick: u64) -> bool {
    let friends = allies(config, victim_side);
    let saw = ships.ships.iter().any(|s| {
        s.vessel != victim && friends.has(s.side) && world.vessel(s.vessel).map_or(false, |v| !matches!(v.status, VesselStatus::Lost(_) | VesselStatus::Wreck))
            && s.detections.iter().any(|d| d.target == victim || (!killer.is_none() && d.target == killer))
    });
    if saw { return true; }
    !killer.is_none() && friends.iter().any(|side| pictures.side(side).and_then(|p| p.track_of(killer)).map_or(false, |t| t.stance != Stance::Friend && tick.saturating_sub(t.last_update_tick) <= OBSERVE_FRESH_TICKS))
}

#[allow(clippy::too_many_arguments)]
fn record_kill(state: &mut ObjectivesState, vessel: EntityId, fate: KillFate, cause: Option<LossCause>, world: &World, ships: &ShipTable, pictures: &Pictures, config: &SortieConfig, ctx: &SimCtx<'_>) {
    let Some(h) = state.hull(vessel).cloned() else { return };
    if !h.enemy { return; }
    let salvageable = match (fate, cause) {
        (KillFate::Lost, Some(LossCause::Capsized)) => true,
        (KillFate::Lost, Some(LossCause::Sunk)) => world.vessel(vessel).map_or(false, |v| { let p = v.kin.pose.pos; world.terrain().depth_at(p.x, p.z) <= SALVAGE_DEPTH_M }),
        (KillFate::Lost, _) => false,
        _ => true,
    };
    if let Some(i) = state.kill_vessels.iter().position(|v| *v == vessel) {
        let k = &mut state.kills[i];
        let rank = |f: KillFate| match f { KillFate::Disabled => 0, KillFate::Lost => 1, KillFate::Captured => 2 };
        if rank(fate) > rank(k.fate) { k.fate = fate; k.salvageable = salvageable; if fate == KillFate::Captured { k.method = KillMethod::Capture; } }
        return;
    }
    let row = state.ledger.get(vessel).copied();
    let method = if fate == KillFate::Captured { KillMethod::Capture } else {
        match (row, cause) { (Some(r), _) => kill_method(r.cause, false), (None, Some(LossCause::CookOff)) => KillMethod::Fire, _ => KillMethod::Collision }
    };
    let killer = row.map_or(EntityId::NONE, |r| r.attacker);
    let range_m = row.map_or(0.0, |r| r.range_m);
    let h_att = world.vessel(killer).map_or(0.0, |v| v.top_height_m());
    let h_vic = world.vessel(vessel).map_or(0.0, |v| v.top_height_m());
    let horizon = crate::sensors::radar::horizon_m(h_att, h_vic);
    let rec = KillRecord {
        tick: ctx.tick.0, hull_record: h.hull_record, class: h.class.clone(), side: h.side, tonnage_t: h.tonnage_t, build_cost: h.cost, fate, method,
        range: range_band(range_m, horizon), time: ctx.time_of_day, emcon: row.map_or(Emcon::default(), |r| r.attacker_emcon),
        observed: observed(vessel, killer, h.side, ships, world, pictures, config, ctx.tick.0), by_player: state.credit.get(&vessel).copied(), salvageable,
    };
    *state.stats.kills_by_method.entry(method).or_insert(0) += 1;
    state.kills.push(rec);
    state.kill_vessels.push(vessel);
}

// ---------------------------------------------------------------------------------------------------------
// Evaluate: plain-data facts, pure rules
// ---------------------------------------------------------------------------------------------------------

/// A prize as the objective rules see it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PrizeFact { pub vessel: EntityId, pub tag: String, pub decided: bool, pub stripped: bool, pub afloat: bool }

/// Everything the objective rules read, gathered from the world, specops and the state.
#[derive(Clone, Debug, Default)]
pub(crate) struct EvalFacts<'a> {
    pub now_s: f64,
    pub hulls: &'a [HullFacts],
    pub markers: &'a [Marker],
    pub prizes: Vec<PrizeFact>,
    /// Completed insert or extract holds by the player side: (marker name, site).
    pub holds: Vec<(String, Option<DVec2>)>,
    pub mines: &'a [[f64; 2]],
    /// Photographed targets (distinct).
    pub photos: Vec<EntityId>,
    /// Config vessels not spawned yet, as (enemy, tag).
    pub pending: Vec<(bool, String)>,
    pub detected: bool,
    pub feint_held_s: f64,
    pub clear_of_contact: bool,
    pub passengers_lost: u16,
    pub allowed_losses: Option<u8>,
}

fn marker_pos<'a>(markers: &'a [Marker], name: &str) -> Option<DVec2> { markers.iter().find(|m| m.name == name).map(|m| DVec2::from(m.pos_m)).filter(|p| p.x.is_finite() && p.y.is_finite()) }
fn tag_match(tag: &str, want: &str) -> bool { want.is_empty() || tag == want }
fn exit_names(markers: &[Marker]) -> impl Iterator<Item = &Marker> { markers.iter().filter(|m| matches!(m.kind, MarkerKind::Exit | MarkerKind::ConvoyExit)) }
fn exit_in(list: &[String], exit: &str, markers: &[Marker]) -> bool {
    if exit.is_empty() || marker_pos(markers, exit).is_none() { list.iter().any(|n| exit_names(markers).any(|m| m.name == *n)) } else { list.iter().any(|n| n == exit) }
}
/// Ever reached the exit (latched): convoys, escorted ships.
fn reached(h: &HullFacts, exit: &str, markers: &[Marker]) -> bool { exit_in(&h.exits, exit, markers) }
/// At the exit now: player hulls and prizes leaving.
fn at_exit(h: &HullFacts, exit: &str, markers: &[Marker]) -> bool { exit_in(&h.at_exits, exit, markers) }

/// Primary objective state and progress (0..=1) from the facts. Pure.
pub(crate) fn eval_objective(obj: &Objective, f: &EvalFacts<'_>) -> (ObjectiveState, f64) {
    use ObjectiveState::{Active, Done, Failed};
    let ratio = |a: f64, b: f64| if b > 0.0 { (a / b).clamp(0.0, 1.0) } else { 0.0 };
    let captured = |v: EntityId| f.prizes.iter().any(|p| p.vessel == v);
    let res = match obj {
        Objective::DestroyAll => {
            let enemies: Vec<&HullFacts> = f.hulls.iter().filter(|h| h.enemy).collect();
            let total: f64 = enemies.iter().map(|h| h.tonnage_t.max(0.0)).sum();
            let down: f64 = enemies.iter().filter(|h| h.out() || captured(h.vessel)).map(|h| h.tonnage_t.max(0.0)).sum();
            let all_out = !enemies.is_empty() && enemies.iter().all(|h| h.out() || captured(h.vessel));
            let pending = f.pending.iter().any(|(e, _)| *e);
            (if all_out && !pending { Done } else { Active }, ratio(down, total))
        }
        Objective::DestroyGroup { tag, fraction } => {
            let need = if fraction.is_finite() { fraction.clamp(0.0, 1.0) } else { 1.0 };
            let group: Vec<&HullFacts> = f.hulls.iter().filter(|h| h.enemy && tag_match(&h.tag, tag)).collect();
            let total: f64 = group.iter().map(|h| h.tonnage_t.max(0.0)).sum();
            let down: f64 = group.iter().filter(|h| h.out() || captured(h.vessel)).map(|h| h.tonnage_t.max(0.0)).sum();
            let escaped: f64 = group.iter().filter(|h| !h.out() && !captured(h.vessel) && reached(h, "", f.markers)).map(|h| h.tonnage_t.max(0.0)).sum();
            let pending = f.pending.iter().any(|(e, t)| *e && tag_match(t, tag));
            let progress = if need > 0.0 { ratio(down, need * total) } else { 1.0 };
            if total > 0.0 && down >= need * total - 1e-9 { (Done, 1.0) } else if total > 0.0 && !pending && escaped > (1.0 - need) * total + 1e-9 { (Failed, progress) } else { (Active, progress) }
        }
        Objective::DestroySite { marker } => {
            let at = marker_pos(f.markers, marker);
            let mut site: Vec<&HullFacts> = f.hulls.iter().filter(|h| h.role == VesselRole::Site && at.map_or(true, |p| p.distance(DVec2::from(h.pos)) <= SITE_RADIUS_M)).collect();
            if site.is_empty() { site = f.hulls.iter().filter(|h| h.enemy && !marker.is_empty() && h.tag == *marker).collect(); }
            let total = site.len() as f64;
            let down = site.iter().filter(|h| h.out()).count() as f64;
            (if !site.is_empty() && down >= total { Done } else { Active }, ratio(down, total))
        }
        Objective::Extract { marker, .. } => {
            let at = marker_pos(f.markers, marker);
            let held = f.holds.iter().any(|(name, site)| name == marker || matches!((site, at), (Some(s), Some(m)) if s.distance(m) <= 1.0));
            let out = held && f.hulls.iter().any(|h| h.player_side && h.player.is_some() && h.afloat() && at_exit(h, "", f.markers));
            if out { (Done, 1.0) } else if held { (Active, 0.5) } else { (Active, 0.0) }
        }
        Objective::LayMines { marker, count } => {
            let at = marker_pos(f.markers, marker);
            let n = f.mines.iter().filter(|m| at.map_or(true, |p| p.distance(DVec2::from(**m)) <= MINEFIELD_RADIUS_M)).count();
            let need = (*count).max(1) as usize;
            (if n >= need { Done } else { Active }, ratio(n as f64, need as f64))
        }
        Objective::Capture { tag } => {
            let taken: Vec<&PrizeFact> = f.prizes.iter().filter(|p| tag_match(&p.tag, tag)).collect();
            if taken.iter().any(|p| p.decided) { (Done, 1.0) }
            else if !taken.is_empty() { (Active, 0.5) }
            else {
                let left = f.hulls.iter().any(|h| h.enemy && tag_match(&h.tag, tag) && !h.lost);
                let pending = f.pending.iter().any(|(e, t)| *e && tag_match(t, tag));
                (if !left && !pending && f.hulls.iter().any(|h| h.enemy && tag_match(&h.tag, tag)) { Failed } else { Active }, 0.0)
            }
        }
        Objective::Escort { tag, exit } => {
            let group: Vec<&HullFacts> = f.hulls.iter().filter(|h| !h.enemy && !tag.is_empty() && h.tag == *tag).collect();
            let through = group.iter().filter(|h| reached(h, exit, f.markers)).count();
            let waiting = group.iter().filter(|h| !reached(h, exit, f.markers) && !h.out()).count();
            let pending = f.pending.iter().any(|(_, t)| !tag.is_empty() && t == tag);
            let progress = ratio(through as f64, group.len() as f64);
            if group.is_empty() || pending { (Active, progress) } else if through > 0 && waiting == 0 { (Done, 1.0) } else if through == 0 && waiting == 0 { (Failed, progress) } else { (Active, progress) }
        }
        Objective::Feint { hold_s } => {
            let hold = (*hold_s).max(1) as f64;
            if !f.detected { (Active, 0.0) }
            else if f.feint_held_s < hold { (Active, 0.1 + 0.8 * ratio(f.feint_held_s, hold)) }
            else if f.clear_of_contact || f.hulls.iter().any(|h| h.player_side && h.player.is_some() && h.afloat() && at_exit(h, "", f.markers)) { (Done, 1.0) }
            else { (Active, 0.9) }
        }
        Objective::Photograph { tag, count } => {
            let need = (*count).max(1) as usize;
            let shots = f.photos.iter().filter(|v| f.hulls.iter().any(|h| h.vessel == **v && h.enemy && tag_match(&h.tag, tag))).count();
            let left = f.hulls.iter().filter(|h| h.enemy && tag_match(&h.tag, tag) && !h.lost && !f.photos.contains(&h.vessel)).count();
            let pending = f.pending.iter().filter(|(e, t)| *e && tag_match(t, tag)).count();
            if shots >= need { (Done, 1.0) } else if shots + left + pending < need && !f.hulls.is_empty() { (Failed, ratio(shots as f64, need as f64)) } else { (Active, ratio(shots as f64, need as f64)) }
        }
        Objective::Survive { seconds, exit } => {
            let out = f.hulls.iter().any(|h| h.player_side && h.player.is_some() && h.afloat() && !exit.is_empty() && at_exit(h, exit, f.markers));
            let s = (*seconds).max(1) as f64;
            if out || f.now_s >= s { (Done, 1.0) } else { (Active, ratio(f.now_s, s)) }
        }
        Objective::CutOut { tag, exit } => {
            let taken: Vec<&PrizeFact> = f.prizes.iter().filter(|p| tag_match(&p.tag, tag)).collect();
            let home = taken.iter().any(|p| p.afloat && !p.stripped && f.hulls.iter().any(|h| h.vessel == p.vessel && at_exit(h, exit, f.markers)));
            if home { (Done, 1.0) }
            else if !taken.is_empty() {
                if taken.iter().all(|p| p.stripped || !p.afloat) { (Failed, 0.5) } else { (Active, 0.5) }
            } else {
                let left = f.hulls.iter().any(|h| h.enemy && tag_match(&h.tag, tag) && !h.lost);
                let pending = f.pending.iter().any(|(e, t)| *e && tag_match(t, tag));
                (if !left && !pending && f.hulls.iter().any(|h| h.enemy && tag_match(&h.tag, tag)) { Failed } else { Active }, 0.0)
            }
        }
    };
    // Losing more passengers than the briefing allows fails the operation (L794).
    match f.allowed_losses { Some(a) if f.passengers_lost > a as u16 => (Failed, res.1), _ => res }
}

/// Ghost-condition state during play. Pure. `ace` is (captured, lost) of the ace's hull if known.
pub(crate) fn eval_ghost(kind: &GhostKind, current: ObjectiveState, s: &ObjectivesState, now_s: f64, primary_done_s: Option<f64>, ace: Option<(bool, bool)>) -> ObjectiveState {
    use ObjectiveState::{Active, Done, Failed};
    if matches!(current, Done | Failed) { return current; }
    match kind {
        GhostKind::NoRadarEmissions => if s.radiated { Failed } else { Active },
        GhostKind::NoLosses => if s.hulls.iter().any(|h| h.player_side && h.lost) { Failed } else { Active },
        GhostKind::NeverDetected => if s.detected_tick.is_some() { Failed } else { Active },
        GhostKind::CaptureTheAce => match ace { Some((true, _)) => Done, Some((false, true)) => Failed, _ => Active },
        GhostKind::NoKills => if s.kills.iter().any(|k| k.by_player.is_some()) { Failed } else { Active },
        GhostKind::UnderTime { seconds } => match primary_done_s {
            Some(t) if t <= *seconds as f64 => Done,
            Some(_) => Failed,
            None => if now_s > *seconds as f64 { Failed } else { Active },
        },
        GhostKind::NoHullDamage => if s.player_hit { Failed } else { Active },
    }
}

/// Final ghost result: Done, or still clean at the end with the objective done.
fn ghost_result(g: &GhostTracker, objective_done: bool) -> bool {
    match g.state { ObjectiveState::Done => objective_done, ObjectiveState::Failed | ObjectiveState::Hidden => false, ObjectiveState::Active => objective_done && !matches!(g.ghost.kind, GhostKind::CaptureTheAce | GhostKind::UnderTime { .. }) }
}

/// Elimination: which teams still have an Active hull, among teams that fielded one. Pure.
pub(crate) fn elimination(hulls: &[HullFacts], team_of: &dyn Fn(SideId) -> Option<u8>) -> Option<Option<u8>> {
    let mut fielded = [false; 2];
    let mut alive = [false; 2];
    for h in hulls {
        let Some(t) = team_of(h.side) else { continue };
        let t = (t as usize).min(1);
        fielded[t] = true;
        if !h.out() { alive[t] = true; }
    }
    if !(fielded[0] && fielded[1]) { return None; }
    match (alive[0], alive[1]) { (true, true) => None, (true, false) => Some(Some(0)), (false, true) => Some(Some(1)), (false, false) => Some(None) }
}

fn primary_state(state: &ObjectivesState) -> ObjectiveState { state.primary.as_ref().map_or(ObjectiveState::Hidden, |p| p.state) }

/// Evaluate the primary objective, the ghost conditions and the mode's end rule (objective, elimination: last
/// team afloat, escort: convoy fraction through the exit, time limit, all player hulls lost, withdrawal once
/// clear of contact). Returns views that changed. Unit: `sys-objectives`.
pub fn evaluate(state: &mut ObjectivesState, world: &World, ships: &ShipTable, pictures: &Pictures, specops: &SpecOpsState, config: &SortieConfig, ctx: &SimCtx<'_>) -> Vec<ObjectiveView> {
    if state.outcome.is_some() { return Vec::new(); }
    if !ctx.tick.is_due(EVAL_HZ, 0) { return Vec::new(); }
    let tick = ctx.tick.0;
    let dt = state.last_eval_tick.map_or(0.0, |t| tick.saturating_sub(t) as f64 * DT);
    state.last_eval_tick = Some(tick);
    let now_s = ctx.time_s();
    let markers = &world.terrain().map.markers;

    refresh_hulls(state, world, ships, config, markers, dt, tick);
    contact_and_detection(state, pictures, config, tick, now_s);
    state.hold_text = Some((ctx.tuning.special_ops.hold_range_m, ctx.tuning.special_ops.hold_s));
    // Captured prizes are kills with fate Captured.
    for p in &specops.prizes { record_kill(state, p.vessel, KillFate::Captured, None, world, ships, pictures, config, ctx); }
    if state.contact && state.detected_tick.is_some() { state.feint_held_s += dt; }
    let clear = clear_of_contact(state, world, pictures, config, tick, false);

    let mut photos: Vec<EntityId> = specops.photos.iter().map(|(_, t)| *t).collect();
    photos.sort();
    photos.dedup();
    let players = player_sides(config);
    let prizes: Vec<PrizeFact> = specops.prizes.iter().map(|p| PrizeFact {
        vessel: p.vessel, tag: state.hull(p.vessel).map_or_else(String::new, |h| h.tag.clone()), decided: p.decision.is_some(), stripped: p.stripped,
        afloat: world.vessel(p.vessel).map_or(false, |v| !matches!(v.status, VesselStatus::Lost(_) | VesselStatus::Wreck)),
    }).collect();
    let holds = specops.completed.iter().filter(|c| c.action == StationAction::InsertOrExtract && world.vessel(c.actor).map_or(true, |v| players.has(v.side)) ).map(|c| (c.site_name.clone(), c.site)).collect();
    let spawned: BTreeSet<u16> = state.hulls.iter().filter_map(|h| h.index).collect();
    let pending = config.vessels.iter().enumerate().filter(|(i, _)| !spawned.contains(&(*i as u16))).map(|(_, v)| (if is_pvp(config) { true } else { enemy_of_players(config, v.side) }, v.tag.clone())).collect();
    let allowed_losses = config.tags.iter().find_map(|t| match t { ConstraintTag::Passengers { allowed_losses, .. } => Some(*allowed_losses), _ => None });
    let facts = EvalFacts { now_s, hulls: &state.hulls, markers, prizes, holds, mines: &state.mines, photos, pending, detected: state.detected_tick.is_some(), feint_held_s: state.feint_held_s, clear_of_contact: clear, passengers_lost: state.passengers_lost, allowed_losses };

    let mut primary = state.primary.clone();
    if let Some(p) = primary.as_mut() {
        if matches!(config.rules, ModeRules::Objective) && p.state == ObjectiveState::Active {
            let (s, prog) = eval_objective(&p.objective, &facts);
            p.state = s;
            p.progress = prog;
        }
    }
    drop(facts);
    state.primary = primary;
    if primary_state(state) == ObjectiveState::Done && state.primary_done_tick.is_none() { state.primary_done_tick = Some(tick); }
    // Ghosts.
    let ace = ace_hull_state(state, config, specops);
    let done_s = state.primary_done_tick.map(|t| Tick(t).seconds());
    let mut ghosts = state.ghosts.clone();
    for g in ghosts.iter_mut() { g.state = eval_ghost(&g.ghost.kind, g.state, state, now_s, done_s, ace); }
    state.ghosts = ghosts;

    end_rules(state, world, pictures, config, tick, now_s);
    if state.outcome.is_some() { finalize_ghosts(state); }
    changed_views(state, config)
}

/// Refresh positions, status and exits of every hull; bind hulls seen for the first time.
fn refresh_hulls(state: &mut ObjectivesState, world: &World, ships: &ShipTable, config: &SortieConfig, markers: &[Marker], dt: f64, tick: u64) {
    for (_, v) in world.vessels() { if v.parent.is_none() { ensure_hull(state, v.id, world, ships, config); } }
    let exits: Vec<(String, DVec2)> = exit_names(markers).map(|m| (m.name.clone(), DVec2::from(m.pos_m))).filter(|(_, p)| p.x.is_finite() && p.y.is_finite()).collect();
    let catalogue = world.catalogue();
    let mut radar_on = false;
    let mut emitting = false;
    for h in state.hulls.iter_mut() {
        let Some(v) = world.vessel(h.vessel) else { h.lost = true; h.disabled_since = None; continue };
        if v.struck { h.struck = true; }
        note_status(h, v.status, v.struck, tick);
        h.pos = [v.kin.pose.pos.x, v.kin.pose.pos.z];
        h.at_exits.clear();
        if !h.lost {
            let here = DVec2::from(h.pos);
            for (name, p) in &exits {
                if here.distance(*p) <= EXIT_RADIUS_M {
                    h.at_exits.push(name.clone());
                    if !h.exits.contains(name) { h.exits.push(name.clone()); }
                }
            }
            if h.player_side {
                for p in &v.parts {
                    match p.state {
                        PartState::Sensor { radiating: true, .. } => {
                            emitting = true;
                            if let Some(PartFunction::Sensor(s)) = catalogue.parts.get_index(p.def.index()).map(|(_, d)| &d.function) {
                                if matches!(s.kind, SensorKind::SearchRadar | SensorKind::NavRadar | SensorKind::FireControlRadar) { radar_on = true; }
                            }
                        }
                        PartState::Jammer { radiating: true } => emitting = true,
                        _ => {}
                    }
                }
            }
        }
    }
    if radar_on { state.radiated = true; }
    if emitting { state.stats.radiating_s += dt as f32; }
}

/// Fold a vessel status into the hull facts at `tick`: lost latches, disabled follows the status (a struck hull
/// counts as disabled), and the disabled-since tick runs only while the status stays Disabled. Pure.
pub(crate) fn note_status(h: &mut HullFacts, status: VesselStatus, struck: bool, tick: u64) {
    match status {
        VesselStatus::Lost(_) | VesselStatus::Wreck => h.lost = true,
        VesselStatus::Disabled => h.disabled = true,
        VesselStatus::Active => h.disabled = struck,
    }
    h.disabled_since = if status == VesselStatus::Disabled && !h.lost { Some(h.disabled_since.unwrap_or(tick)) } else { None };
}

/// The players' side is out of the sortie at `tick`: every player hull lost (`PlayerHullsLost`), or every one
/// lost or disabled for [`PLAYER_DISABLED_DEFEAT_S`] without a break (`PlayerHullsDisabled`). Co-op counts
/// every player hull. Pure.
pub(crate) fn player_hulls_out(hulls: &[HullFacts], tick: u64) -> Option<EndReason> {
    let ph: Vec<&HullFacts> = hulls.iter().filter(|h| h.player.is_some() || h.role == VesselRole::PlayerHull).collect();
    if ph.is_empty() { return None; }
    if ph.iter().all(|h| h.lost) { return Some(EndReason::PlayerHullsLost); }
    let dead = |h: &HullFacts| h.disabled_since.is_some_and(|t| Tick(tick.saturating_sub(t)).seconds() >= PLAYER_DISABLED_DEFEAT_S);
    if ph.iter().all(|h| h.lost || dead(h)) { Some(EndReason::PlayerHullsDisabled) } else { None }
}

/// Does any enemy picture hold a non-Friend live track on a player hull now (contact), and when first.
fn contact_and_detection(state: &mut ObjectivesState, pictures: &Pictures, config: &SortieConfig, tick: u64, now_s: f64) {
    let players = player_sides(config);
    let mut contact = false;
    for p in &pictures.sides {
        if players.has(p.side) || players.iter().any(|ps| allies(config, ps).has(p.side)) { continue; }
        if p.tracks.iter().any(|t| t.is_live() && t.stance != Stance::Friend && state.hull(t.truth).map_or(false, |h| h.player_side && h.player.is_some() && h.afloat())) { contact = true; break; }
    }
    state.contact = contact;
    if contact && state.detected_tick.is_none() {
        state.detected_tick = Some(tick);
        state.stats.first_detection_s = Some(now_s as f32);
    }
}

/// Clear-of-contact test for withdrawal and feint escape. With `withdrawing_only`, only the hulls of players
/// who asked to withdraw are tested.
fn clear_of_contact(state: &ObjectivesState, world: &World, pictures: &Pictures, config: &SortieConfig, tick: u64, withdrawing_only: bool) -> bool {
    let players = player_sides(config);
    let hulls: Vec<&HullFacts> = state.hulls.iter().filter(|h| h.player_side && h.player.is_some() && h.afloat() && (!withdrawing_only || h.player.map_or(false, |p| state.withdrawing.contains(&p)))).collect();
    if hulls.is_empty() { return false; }
    let age = (CLEAR_TRACK_AGE_S / DT) as u64;
    for p in &pictures.sides {
        if players.has(p.side) || players.iter().any(|ps| allies(config, ps).has(p.side)) { continue; }
        if p.tracks.iter().any(|t| t.is_live() && t.stance != Stance::Friend && hulls.iter().any(|h| h.vessel == t.truth) && tick.saturating_sub(t.last_update_tick) <= age) { return false; }
    }
    let _ = world;
    !state.hulls.iter().any(|e| e.enemy && !e.out() && hulls.iter().any(|h| DVec2::from(h.pos).distance(DVec2::from(e.pos)) <= CLEAR_RANGE_M))
}

/// (captured, lost) of the ace's hull: the first config vessel of a group an ace leads.
fn ace_hull_state(state: &ObjectivesState, config: &SortieConfig, specops: &SpecOpsState) -> Option<(bool, bool)> {
    let groups: Vec<_> = config.groups.iter().filter(|g| g.ace.is_some()).map(|g| g.id).collect();
    let idx = config.vessels.iter().position(|v| v.group.map_or(false, |g| groups.contains(&g)))? as u16;
    let h = state.hull_of_index(idx)?;
    Some((specops.prizes.iter().any(|p| p.vessel == h.vessel), h.lost))
}

fn decide(state: &mut ObjectivesState, outcome: Outcome, winner: Option<SideId>, tick: u64) {
    if state.outcome.is_some() { return; }
    state.outcome = Some(outcome);
    state.winner = winner;
    state.ended_tick = Some(tick);
}

/// [`decide`] with the objective-mode end reason.
fn decide_why(state: &mut ObjectivesState, why: EndReason, outcome: Outcome, winner: Option<SideId>, tick: u64) {
    if state.outcome.is_some() { return; }
    state.end_reason = Some(why);
    decide(state, outcome, winner, tick);
}

fn end_rules(state: &mut ObjectivesState, world: &World, pictures: &Pictures, config: &SortieConfig, tick: u64, now_s: f64) {
    let time_up = config.time_limit_s.map_or(false, |s| now_s >= s as f64);
    match &config.rules {
        ModeRules::Objective => {
            match primary_state(state) {
                ObjectiveState::Done => { decide_why(state, EndReason::ObjectiveDone, Outcome::Victory, first_player_side(config), tick); return; }
                ObjectiveState::Failed => { decide_why(state, EndReason::ObjectiveFailed, Outcome::Defeat, first_enemy_side(config), tick); return; }
                _ => {}
            }
            if let Some(why) = player_hulls_out(&state.hulls, tick) { decide_why(state, why, Outcome::Defeat, first_enemy_side(config), tick); return; }
            if time_up { decide_why(state, EndReason::TimeLimit, Outcome::Withdrawn, None, tick); return; }
            // Withdrawal: every player with a hull afloat asked, and those hulls are clear of contact.
            let afloat: Vec<PlayerSlot> = state.hulls.iter().filter(|h| h.afloat()).filter_map(|h| h.player).collect();
            if !afloat.is_empty() && afloat.iter().all(|p| state.withdrawing.contains(p)) {
                // A withdrawing hull that reaches an exit marker has left the area, contact or not.
                let out = state.hulls.iter().filter(|h| h.afloat() && h.player.is_some()).all(|h| !h.at_exits.is_empty());
                if out { decide_why(state, EndReason::Withdrew, Outcome::Withdrawn, None, tick); return; }
                if clear_of_contact(state, world, pictures, config, tick, true) {
                    let since = *state.clear_since.get_or_insert(tick);
                    if Tick(tick.saturating_sub(since)).seconds() >= CLEAR_HOLD_S { decide_why(state, EndReason::Withdrew, Outcome::Withdrawn, None, tick); }
                } else { state.clear_since = None; }
            } else { state.clear_since = None; }
        }
        ModeRules::Sandbox | ModeRules::Splash => {}
        ModeRules::Trial { ticks } => { if tick >= *ticks { decide(state, Outcome::Draw, None, tick); } }
        ModeRules::Elimination => {
            let team_of = |s: SideId| side_def(config, s).and_then(|d| d.pvp_team);
            match elimination(&state.hulls, &team_of) {
                Some(Some(t)) => { let w = config.sides.iter().find(|d| d.pvp_team.map(|x| x.min(1)) == Some(t)).map(|d| d.id); decide(state, Outcome::Victory, w, tick); }
                Some(None) => decide(state, Outcome::Draw, None, tick),
                None => if time_up { decide(state, Outcome::Draw, None, tick); },
            }
        }
        ModeRules::Escort { convoy_side, exit, required_fraction, .. } => {
            let need = if required_fraction.is_finite() { required_fraction.clamp(0.0, 1.0) } else { 1.0 };
            let other = config.sides.iter().find(|d| d.id != *convoy_side && d.pvp_team.is_some() && d.pvp_team != side_def(config, *convoy_side).and_then(|c| c.pvp_team)).map(|d| d.id);
            let mut convoy: Vec<&HullFacts> = state.hulls.iter().filter(|h| h.side == *convoy_side && h.role == VesselRole::Convoy).collect();
            if convoy.is_empty() { convoy = state.hulls.iter().filter(|h| h.side == *convoy_side && h.player.is_none()).collect(); }
            let markers = &world.terrain().map.markers;
            let total: f64 = convoy.iter().map(|h| h.tonnage_t.max(0.0)).sum();
            let through: f64 = convoy.iter().filter(|h| h.afloat() && reached(h, exit, markers)).map(|h| h.tonnage_t.max(0.0)).sum();
            let gone: f64 = convoy.iter().filter(|h| h.out() && !reached(h, exit, markers)).map(|h| h.tonnage_t.max(0.0)).sum();
            let team_of = |s: SideId| side_def(config, s).and_then(|d| d.pvp_team);
            let convoy_team = team_of(*convoy_side).map(|t| t.min(1));
            if total > 0.0 && through >= need * total - 1e-9 { decide(state, Outcome::Victory, Some(*convoy_side), tick); return; }
            if total > 0.0 && total - gone < need * total - 1e-9 { decide(state, Outcome::Victory, other, tick); return; }
            // Attackers wiped out: the convoy side wins; escorts wiped out alone do not end it.
            let human: Vec<HullFacts> = state.hulls.iter().filter(|h| h.player.is_some()).cloned().collect();
            if let Some(Some(t)) = elimination(&human, &team_of) { if Some(t) == convoy_team { decide(state, Outcome::Victory, Some(*convoy_side), tick); return; } }
            if total <= 0.0 { if let Some(Some(t)) = elimination(&human, &team_of) { let w = config.sides.iter().find(|d| d.pvp_team.map(|x| x.min(1)) == Some(t)).map(|d| d.id); decide(state, Outcome::Victory, w, tick); return; } }
            if time_up { decide(state, Outcome::Victory, other, tick); }
        }
    }
}

fn finalize_ghosts(state: &mut ObjectivesState) {
    let done = primary_state(state) == ObjectiveState::Done;
    for g in state.ghosts.iter_mut() { g.state = if ghost_result(g, done) { ObjectiveState::Done } else { ObjectiveState::Failed }; }
}

fn changed_views(state: &mut ObjectivesState, config: &SortieConfig) -> Vec<ObjectiveView> {
    let all = views(state, config);
    state.reported.resize(all.len(), (ObjectiveState::Hidden, u8::MAX));
    let mut out = Vec::new();
    for (i, v) in all.into_iter().enumerate() {
        let now = (v.state, (v.progress * 100.0).floor().clamp(0.0, 100.0) as u8);
        if state.reported[i] != now { state.reported[i] = now; out.push(v); }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Views and text
// ---------------------------------------------------------------------------------------------------------

fn objective_text(o: &Objective, progress: f64, hold: (f64, f64)) -> Msg {
    match o {
        Objective::DestroyAll => Msg::new("objective.destroy_all"),
        Objective::DestroyGroup { fraction, .. } => Msg::new("objective.destroy_group").num((fraction.clamp(0.0, 1.0) * 100.0).round(), Unit::Percent),
        Objective::DestroySite { .. } => Msg::new("objective.destroy_site"),
        Objective::Extract { passengers, .. } => {
            if progress >= 0.5 { Msg::new("objective.extract.leave") }
            else if *passengers > 0 { Msg::new("objective.extract.passengers").arg(MsgArg::Int(*passengers as i64)).num(hold.0, Unit::Metres).num(hold.1, Unit::Seconds) }
            else { Msg::new("objective.extract.hold").num(hold.0, Unit::Metres).num(hold.1, Unit::Seconds) }
        }
        Objective::LayMines { count, .. } => Msg::new("objective.lay_mines").arg(MsgArg::Int(*count as i64)),
        Objective::Capture { .. } => if progress >= 0.5 { Msg::new("objective.capture.decide") } else { Msg::new("objective.capture") },
        Objective::Escort { .. } => Msg::new("objective.escort"),
        Objective::Feint { hold_s } => {
            if progress <= 0.0 { Msg::new("objective.feint.detect") } else if progress < 0.9 { Msg::new("objective.feint.hold").num(*hold_s as f64, Unit::Seconds) } else { Msg::new("objective.feint.escape") }
        }
        Objective::Photograph { count, .. } => Msg::new("objective.photograph").arg(MsgArg::Int(*count as i64)),
        Objective::Survive { seconds, exit } => if exit.is_empty() { Msg::new("objective.survive").num(*seconds as f64, Unit::Seconds) } else { Msg::new("objective.survive.exit").num(*seconds as f64, Unit::Seconds) },
        Objective::CutOut { .. } => Msg::new("objective.cut_out"),
    }
}

fn ghost_text(k: &GhostKind) -> Msg {
    match k {
        GhostKind::NoRadarEmissions => Msg::new("objective.ghost.no_radar"),
        GhostKind::NoLosses => Msg::new("objective.ghost.no_losses"),
        GhostKind::NeverDetected => Msg::new("objective.ghost.never_detected"),
        GhostKind::CaptureTheAce => Msg::new("objective.ghost.capture_ace"),
        GhostKind::NoKills => Msg::new("objective.ghost.no_kills"),
        GhostKind::UnderTime { seconds } => Msg::new("objective.ghost.under_time").num(*seconds as f64, Unit::Seconds),
        GhostKind::NoHullDamage => Msg::new("objective.ghost.no_hull_damage"),
    }
}

fn objective_marker(o: &Objective, config: &SortieConfig) -> Option<DVec2> {
    let name = match o {
        Objective::DestroySite { marker } | Objective::Extract { marker, .. } | Objective::LayMines { marker, .. } => marker.as_str(),
        Objective::Escort { exit, .. } | Objective::Survive { exit, .. } | Objective::CutOut { exit, .. } => exit.as_str(),
        _ => return None,
    };
    marker_pos(&config.map.markers, name)
}

/// Current views of every objective for a snapshot. Unit: `sys-objectives`.
pub fn views(state: &ObjectivesState, config: &SortieConfig) -> Vec<ObjectiveView> {
    let mut v = Vec::with_capacity(1 + state.ghosts.len());
    let hold = state.hold_text.unwrap_or_else(|| { let t = content::tuning::SpecialOpsTuning::default(); (t.hold_range_m, t.hold_s) });
    let (text, st, progress, marker) = match (&config.rules, state.primary.as_ref()) {
        // A sortie lost to disabled hulls says so in the primary objective's place: the sortie-end screen lists
        // the objectives' last texts.
        (ModeRules::Objective, Some(p)) => match state.end_reason.and_then(|r| r.text_key().map(|k| (r, k))) {
            Some((EndReason::PlayerHullsDisabled, k)) => (Msg::new(k).num(PLAYER_DISABLED_DEFEAT_S, Unit::Seconds), ObjectiveState::Failed, p.progress, objective_marker(&p.objective, config)),
            _ => (objective_text(&p.objective, p.progress, hold), p.state, p.progress, objective_marker(&p.objective, config)),
        },
        (ModeRules::Elimination, _) => (Msg::new("objective.elimination"), mode_state(state), 0.0, None),
        (ModeRules::Escort { required_fraction, exit, .. }, _) => (Msg::new("objective.pvp_escort").num((required_fraction.clamp(0.0, 1.0) * 100.0).round(), Unit::Percent), mode_state(state), 0.0, marker_pos(&config.map.markers, exit)),
        (ModeRules::Sandbox, _) => (Msg::new("objective.sandbox"), ObjectiveState::Hidden, 0.0, None),
        _ => (Msg::default(), ObjectiveState::Hidden, 0.0, None),
    };
    v.push(ObjectiveView { id: ObjectiveId(0), ghost: false, state: st, text, progress: progress.clamp(0.0, 1.0) as f32, marker });
    for (i, g) in state.ghosts.iter().enumerate() {
        v.push(ObjectiveView { id: ObjectiveId((i + 1).min(u8::MAX as usize) as u8), ghost: true, state: g.state, text: ghost_text(&g.ghost.kind), progress: 0.0, marker: None });
    }
    v
}

fn mode_state(state: &ObjectivesState) -> ObjectiveState { if state.outcome.is_some() { ObjectiveState::Done } else { ObjectiveState::Active } }

// ---------------------------------------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------------------------------------

fn hull_damage(v: &Vessel) -> HullDamage {
    let cells = v.grid.damage.iter().enumerate().filter(|(_, b)| **b > 0).map(|(i, b)| CellChange { cell: content::ids::CellIndex(i as u32), bucket: *b }).collect();
    let parts = v.parts.iter().filter(|p| p.health < 1.0).map(|p| (p.slot, p.health.clamp(0.0, 1.0))).collect();
    let jury_rigged = v.networks.conduits.iter().filter(|c| c.state == sim_core::networks::ConduitState::JuryRigged).count().min(u16::MAX as usize) as u16;
    HullDamage { cells, parts, jury_rigged }
}

fn fuel_now(v: &Vessel, catalogue: &Catalogue) -> (f64, f64) {
    let mut now = 0.0;
    let mut cap = 0.0;
    for p in &v.parts {
        if let PartState::FuelTank { litres } = p.state { now += litres.max(0.0); }
        if let Some(PartFunction::FuelTank { litres }) = catalogue.parts.get_index(p.def.index()).map(|(_, d)| &d.function) { cap += litres.max(0.0); }
    }
    (now, cap)
}

fn ammo_slots(v: &Vessel) -> BTreeMap<PartSlot, u32> {
    v.parts.iter().filter_map(|p| match p.state {
        PartState::Magazine { rounds, .. } => Some((p.slot, rounds)),
        PartState::Launcher { ready, .. } => Some((p.slot, ready as u32)),
        PartState::MineRail { mines, .. } => Some((p.slot, mines as u32)),
        PartState::Decoy { rounds, .. } => Some((p.slot, rounds as u32)),
        _ => None,
    }).collect()
}

fn ammo_kg_now(v: &Vessel, catalogue: &Catalogue) -> BTreeMap<u16, f64> {
    let mut m = BTreeMap::new();
    for p in &v.parts {
        if let PartState::Magazine { ammo_kg, .. } = p.state {
            if let Some(PartFunction::Magazine { calibre_mm, .. }) = catalogue.parts.get_index(p.def.index()).map(|(_, d)| &d.function) { *m.entry(*calibre_mm).or_insert(0.0) += ammo_kg.max(0.0); }
        }
    }
    m
}

/// Assemble the full result at sortie end: kills, survivors with damage and stores, prizes, ace fates,
/// per-player hull damage (`HullDamage` from the vessel's cell buckets and part health), fuel and ammunition
/// left and expended, sightings and evidence from the commanders, counters, belief samples.
/// Unit: `sys-objectives`.
#[allow(clippy::too_many_arguments)]
pub fn build_result(state: &ObjectivesState, world: &World, ships: &ShipTable, specops: &SpecOpsState, commanders: &[CommanderState], aces: &[crate::ai::ace::AceState], config: &SortieConfig, highest_rung: u8) -> SortieResult {
    let _ = ships;
    let catalogue = world.catalogue();
    let outcome = state.outcome.unwrap_or_default();
    let objective_done = primary_state(state) == ObjectiveState::Done;
    let victory = outcome == Outcome::Victory;
    let gone = |v: EntityId| world.vessel(v).map_or(true, |v| matches!(v.status, VesselStatus::Lost(_) | VesselStatus::Wreck));
    let captured = |v: EntityId| specops.prizes.iter().any(|p| p.vessel == v);

    // Kills, with captures folded in (record_kill may not have seen a capture made on the last tick).
    let mut kills = state.kills.clone();
    for p in &specops.prizes {
        if let Some(i) = state.kill_vessels.iter().position(|v| *v == p.vessel) { kills[i].fate = KillFate::Captured; kills[i].method = KillMethod::Capture; kills[i].salvageable = true; }
    }
    let survivors = state.hulls.iter().filter(|h| h.enemy && h.hull_record.is_some() && !gone(h.vessel) && !captured(h.vessel)).filter_map(|h| {
        let v = world.vessel(h.vessel)?;
        let (f, cap) = fuel_now(v, catalogue);
        let rounds = rounds_of(v) as f64;
        Some(SurvivorRecord { hull_record: h.hull_record?, damage: v.cells_lost_fraction().clamp(0.0, 1.0), fuel: if cap > 0.0 { (f / cap).clamp(0.0, 1.0) } else { 0.0 }, ammunition: if h.rounds0 > 0 { (rounds / h.rounds0 as f64).clamp(0.0, 1.0) } else { 1.0 } })
    }).collect();
    let prizes = specops.prizes.iter().map(|p| {
        let h = state.hull(p.vessel);
        let v = world.vessel(p.vessel);
        let mut parts: Vec<PartId> = v.map_or_else(Vec::new, |v| v.parts.iter().filter(|x| x.works()).filter_map(|x| catalogue.parts.get_index(x.def.index()).map(|(id, _)| id.clone())).collect());
        parts.sort();
        parts.dedup();
        let decision = p.decision.unwrap_or(PrizeDecision::SailHome);
        let afloat = !gone(p.vessel);
        let reached_exit = decision == PrizeDecision::SailHome && afloat && (p.reached_exit || victory);
        PrizeRecord { hull_record: h.and_then(|h| h.hull_record), class: h.and_then(|h| h.class.clone()), decision, tonnage_t: h.map_or(0.0, |h| h.tonnage_t), build_cost: h.map_or(0, |h| h.cost), parts, reached_exit, crew_captured: p.crew_captured }
    }).collect();
    let aces_out = aces.iter().map(|a| {
        let fate = if a.captured || captured(a.vessel) { crate::protocol::config::AceFate::Captured }
            else if a.beaten || world.vessel(a.vessel).map_or(true, |v| v.status != VesselStatus::Active) { crate::protocol::config::AceFate::Beaten }
            else { crate::protocol::config::AceFate::Escaped };
        (a.ace.clone(), fate)
    }).collect::<Vec<_>>();
    let ace_captured = aces.iter().any(|a| a.captured || captured(a.vessel));
    let ghosts = state.ghosts.iter().map(|g| {
        if matches!(g.ghost.kind, GhostKind::CaptureTheAce) && ace_captured { objective_done } else { ghost_result(g, objective_done) }
    }).collect();

    // Players.
    let mut players = Vec::new();
    for p in &config.players {
        let Some(h) = p.vessel.and_then(|i| state.hull_of_index(i)).or_else(|| state.hulls.iter().find(|h| h.player == Some(p.slot))) else { continue };
        let v = world.vessel(h.vessel);
        let (fuel, cap) = v.map_or((0.0, 0.0), |v| fuel_now(v, catalogue));
        let kg_now = v.map_or_else(BTreeMap::new, |v| ammo_kg_now(v, catalogue));
        let ammo_kg_expended = h.ammo_kg0.iter().filter_map(|(c, k0)| { let used = k0 - kg_now.get(c).copied().unwrap_or(0.0); (used > 0.0).then_some((*c, used)) }).collect();
        players.push(PlayerResult {
            player: p.slot, hull_lost: h.lost || gone(h.vessel), damage: v.map_or_else(HullDamage::default, hull_damage), fuel_fraction: if cap > 0.0 { (fuel / cap).clamp(0.0, 1.0) } else { 0.0 },
            ammo: v.map_or_else(BTreeMap::new, ammo_slots), expended: h.expended.clone(), ammo_kg_expended, fuel_l_used: (h.fuel_l0 - fuel).max(0.0), passengers_lost: h.passengers_lost, dropped: false,
        });
    }

    let (sightings, evidence) = commanders.iter().fold((0u16, 0u16), |(s, e), c| (s.saturating_add(c.sightings), e.saturating_add(c.evidence)));
    let belief = commanders.iter().find(|c| enemy_of_players(config, c.side) && !c.belief.p.is_empty()).map_or_else(Vec::new, |c| crate::ai::commander::belief_samples(&c.belief, 32));
    let mut distinct_photos: Vec<EntityId> = specops.photos.iter().map(|(_, t)| *t).collect();
    distinct_photos.sort();
    distinct_photos.dedup();
    let crew_captured = specops.prizes.iter().fold(specops.crew_captured, |a, p| a.saturating_add(p.crew_captured));
    // Passengers brought through: the briefing's count less losses, when the operation succeeded.
    let carried = config.tags.iter().filter_map(|t| match t { ConstraintTag::Passengers { count, .. } => Some(*count as u16), _ => None })
        .chain(match &config.objective { Objective::Extract { passengers, .. } => Some(*passengers as u16), _ => None }).max().unwrap_or(0);
    let passengers_delivered = specops.passengers_delivered.saturating_add(if objective_done { carried.saturating_sub(state.passengers_lost) } else { 0 });

    // Stats: tonnage shares and own cells lost for the report card and trial score.
    let mut stats = state.stats.clone();
    let enemies: Vec<&HullFacts> = state.hulls.iter().filter(|h| h.enemy).collect();
    stats.enemy_tonnage_total = enemies.iter().map(|h| h.tonnage_t.max(0.0)).sum();
    stats.enemy_tonnage_disabled = enemies.iter().filter(|h| h.out() || gone(h.vessel) || captured(h.vessel)).map(|h| h.tonnage_t.max(0.0)).sum();
    stats.enemy_struck = enemies.iter().filter(|h| h.struck || world.vessel(h.vessel).is_some_and(|v| v.id == h.vessel && v.struck)).count().min(u16::MAX as usize) as u16;
    let (mut lost_cells, mut orig_cells) = (0.0, 0.0);
    for h in state.hulls.iter().filter(|h| h.player.is_some() || h.role == VesselRole::PlayerHull) {
        match world.vessel(h.vessel) {
            Some(v) if v.grid.original_count > 0 => { orig_cells += v.grid.original_count as f64; lost_cells += v.grid.original_count.saturating_sub(v.grid.solid_count) as f64; }
            Some(_) => {}
            // Gone from the world (sunk and despawned): every cell is lost.
            None => { let c = h.cells0.max(1) as f64; orig_cells += c; lost_cells += c; }
        }
    }
    stats.own_cells_lost_fraction = if orig_cells > 0.0 { (lost_cells / orig_cells).clamp(0.0, 1.0) } else { 0.0 };

    SortieResult {
        operation: config.operation.clone(), region: config.region.clone(), seed: config.seed, outcome, winner: state.winner,
        duration_ticks: state.ended_tick.unwrap_or(world.tick().0), objective_done, ghosts, kills, survivors, prizes, aces: aces_out, players,
        sightings, evidence, photos: distinct_photos.len().min(u8::MAX as usize) as u8, survivors_recovered: specops.survivors_recovered, crew_captured,
        passengers_delivered, mines_laid: specops.mines_laid.max(state.mines.len().min(u16::MAX as usize) as u16), site_destroyed: state.site_destroyed || (matches!(config.objective, Objective::DestroySite { .. }) && objective_done),
        tender_lost: state.tender_lost, highest_rung, belief, stats,
    }
}

/// The client-facing summary. `tip` is left `None`; in Splash and Tutorial modes the Sim fills it from
/// `editor::splash::tip`. Unit: `sys-objectives`.
pub fn summary(result: &SortieResult, config: &SortieConfig) -> SortieSummary {
    let mut objectives = Vec::with_capacity(1 + result.ghosts.len());
    if matches!(config.rules, ModeRules::Objective) { objectives.push((ObjectiveId(0), result.objective_done)); }
    for (i, g) in result.ghosts.iter().enumerate() { objectives.push((ObjectiveId((i + 1).min(u8::MAX as usize) as u8), *g)); }
    SortieSummary { outcome: result.outcome, winner: result.winner, duration_ticks: result.duration_ticks, objectives, stats: result.stats.clone(), tip: None }
}

/// Trial-battery score of a finished fight: 100 x share of enemy tonnage disabled - 50 x share of own cells
/// lost (L227). Real code: spec formula.
pub fn trial_score(stats: &SortieStats) -> f64 { let e = if stats.enemy_tonnage_total > 0.0 { stats.enemy_tonnage_disabled / stats.enemy_tonnage_total } else { 0.0 }; 100.0 * e - 50.0 * stats.own_cells_lost_fraction }

/// Every `objective.*` key this unit can emit (`game/i18n/objectives.csv`). Unit: `sys-objectives`.
pub fn text_keys() -> Vec<&'static str> {
    vec![
        "objective.destroy_all", "objective.destroy_group", "objective.destroy_site",
        "objective.extract.hold", "objective.extract.passengers", "objective.extract.leave",
        "objective.lay_mines", "objective.capture", "objective.capture.decide", "objective.escort",
        "objective.feint.detect", "objective.feint.hold", "objective.feint.escape",
        "objective.photograph", "objective.survive", "objective.survive.exit", "objective.cut_out",
        "objective.ghost.no_radar", "objective.ghost.no_losses", "objective.ghost.never_detected", "objective.ghost.capture_ace",
        "objective.ghost.no_kills", "objective.ghost.under_time", "objective.ghost.no_hull_damage",
        "objective.elimination", "objective.pvp_escort", "objective.sandbox", "objective.end.hulls_disabled",
        "objective.side.team_a", "objective.side.team_b", "objective.side.players", "objective.side.convoy", "objective.side.combine",
        "objective.side.civilian", "objective.side.protected",
    ]
}

#[cfg(test)]
mod tests;
