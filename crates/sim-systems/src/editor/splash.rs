//! Unit `sys-splash`. Instant testing: the splash basin. DESIGN.md L68, L214-225.
//!
//! Space saves the design and puts the ship in a test basin: calm, Beaufort 4 or Beaufort 7 water, against
//! drones or a patrol boat that fights back. During a splash: 30 s rewind, a scope on any port, live edits to
//! kits, tuning and graphs (changing the hull shell returns to dry dock). Every splash ends with a report
//! card and one tip. Load-time target: p95 at most 1 s from Space to the first frame with helm input
//! accepted, hard limit 2 s. The basin world is built from the collider compound and mass properties cached
//! by the save (`VesselPrefab`, `ShipDesign`). Fallbacks: (1) pre-built splash: when the editor has been idle
//! 500 ms the background worker keeps a ready `PreparedSplash` in sync and Space only swaps worlds;
//! (2) progressive splash: ship and calm water first, sea state and targets within the next second
//! (`SplashStage`).
//!
//! # The basin (choices where the spec is silent)
//! - Side 0 is the player's ship; side 1 (`Faction::Target`) the targets, declared Hostile so the officers
//!   engage them under stock doctrine. Day, clear sky; Beaufort 4 and 7 use the sea-state codes of
//!   `SeaState::BEAUFORT_4` and `BEAUFORT_7` and the clearest weather that may carry them (Beaufort 7 is rain).
//! - Drones: [`DRONE_COUNT`] remote target hulls (the lowest `ClassRole::Drone` class, else the lowest patrol
//!   boat) orbiting clockwise at [`DRONE_SPEED_KN`] on a [`DRONE_ORBIT_RADIUS_M`] circle whose centre lies
//!   [`DRONE_ORBIT_RANGE_M`] off the bow. They hold fire (`ShipOrders::weapons_tight`) and run `Proxy`
//!   captains that only follow the orbit and evade inbound weapons.
//! - Patrol boat: the lowest-act patrol-boat class with a `Duelist` captain, [`PATROL_RANGE_M`] off the
//!   starboard bow, closing at [`PATROL_SPEED_KN`]; it strikes under fire below [`PATROL_STRIKE_HEALTH`].
//! - The basin ruleset allows everything the dry dock allows (open graphs, the player's own Luau) with no
//!   tonnage or complexity cap: Ship's Notes never block a launch, and there is no hull size cap below the
//!   90 m player limit (L225).
//!
//! # The report card and its one tip
//! The tip is the highest-ranked thing the player could change, in this order: a Ship's Note of tier 1 (the
//! ship cannot float, move or make power); the design-related hold reason seen most (a mount that could not
//! train, elevate, see past own structure, reach, keep power or ammunition; at least
//! [`MIN_HOLD_SECONDS`] mount-seconds against a target); a tier 2 or 3 note; the worst gauge at Fail, then
//! at Caution; what the fight showed (nothing fired, a low hit rate, heavy damage); any other note; else a
//! line suggesting rougher water or the patrol boat.
use super::gauges::GaugeStrip;
use super::notes::{NoteTier, ShipNote};
use super::Check;
use crate::prelude::*;
use crate::protocol::config::{BlueprintRef, Faction, GroupInit, GroupOrders, ModeRules, PlayerInit, SideDef, SortieConfig, SortieIntel, SortieSummary, VesselInit, VesselRole};
use crate::ship::ShipDesign;
use crate::sim::{Sim, SimDeps, SimError};
use content::blueprint::Blueprint;
use content::campaign::{CampaignData, ClassRole, EnemyClassDef, FleetKind, Formation, MapDef, Marker, MarkerKind, Objective, MAX_MAP_M};
use content::settings::{SimSettings, SplashOptions, SplashTarget};
use content::weather::{SeaState, WeatherKind};

/// Tag of splash drone hulls; `Sim::new` gives them `Proxy` captains that hold fire.
pub const DRONE_TAG: &str = "drone";
/// Tag of the splash patrol boat; `Sim::new` gives it a `Duelist` captain.
pub const PATROL_BOAT_TAG: &str = "patrol_boat";
/// Tag of the player's hull in a basin.
pub const PLAYER_TAG: &str = "player";

pub const DRONE_COUNT: usize = 3;
pub const DRONE_ORBIT_RANGE_M: f64 = 4_000.0;
pub const DRONE_ORBIT_RADIUS_M: f64 = 1_500.0;
pub const DRONE_SPEED_KN: f64 = 10.0;
/// Waypoints of the drone orbit (one every 30 degrees).
pub const ORBIT_MARKERS: usize = 12;
/// Start range of the patrol boat: just outside a small gun's 3 km reach, so the duel starts within about half a
/// minute of closing instead of after minutes of transit.
pub const PATROL_RANGE_M: f64 = 3_500.0;
pub const PATROL_SPEED_KN: f64 = 15.0;
/// The splash patrol boat strikes its colours under fire below this share of its cells (10% lost).
pub const PATROL_STRIKE_HEALTH: f64 = 0.9;
/// A hold reason needs this many mount-seconds against a target before it can be the tip.
pub const MIN_HOLD_SECONDS: u32 = 5;
/// The progressive splash brings sea state and targets in this many seconds after the ship (L224).
pub const PROGRESSIVE_DELAY_S: u32 = 1;
/// Low hit rate tip: below this share of hits over at least [`LOW_HIT_MIN_SHOTS`] shots.
pub const LOW_HIT_RATE: f64 = 0.15;
pub const LOW_HIT_MIN_SHOTS: u32 = 20;
/// Heavy damage tip: more than this share of the ship's cells lost.
pub const HEAVY_DAMAGE: f64 = 0.25;

/// Build the basin config for a design: one player hull at the basin's spawn marker, targets per the options
/// (`Faction::Target` side: drones orbiting, or the lowest-act patrol boat with a `Duelist` captain), basin
/// map from `TheatreDef::basin`. Unit: `sys-splash`.
pub fn splash_config(bp: &Blueprint, options: SplashOptions, seed: u64, campaign: &CampaignData) -> SortieConfig {
    let bp = canonical(bp);
    let hash = bp.header.hash;
    let mut map = basin_map(&campaign.theatre.basin);
    let (spawn, heading_deg) = map.markers.iter().find(|m| m.kind == MarkerKind::PlayerSpawn).map(|m| (DVec2::from(m.pos_m), m.heading_deg)).unwrap_or((DVec2::ZERO, 0.0));
    let heading = heading_deg.to_radians();
    let me = SideId(0);
    let them = SideId(1);
    let sides = vec![
        SideDef { id: me, faction: Faction::Players, name: TextKey::new("splash.side.ship"), hostile_to: SideMask::only(them), allied: SideMask::only(me), pvp_team: None },
        SideDef { id: them, faction: Faction::Target, name: TextKey::new("splash.side.targets"), hostile_to: SideMask::only(me), allied: SideMask::only(them), pvp_team: None },
    ];
    let player = PlayerInit { slot: PlayerSlot(0), client_id: 0, name: bp.header.name.clone(), side: me, vessel: Some(0), human: true };
    let mut vessels = vec![VesselInit {
        blueprint: BlueprintRef::Inline(hash), side: me, class: None, group: None, pos: DVec3::new(spawn.x, 0.0, spawn.y), heading_rad: heading, speed_kn: 0.0, arrives_s: 0,
        damage: None, fuel_fraction: 1.0, ammo_fraction: 1.0, ammo: BTreeMap::new(), hull_record: None, veterancy: 0.0, tag: PLAYER_TAG.into(), declared: None, emcon: None, passengers: 0, role: VesselRole::PlayerHull,
    }];
    let mut groups = Vec::new();
    let mut intel = SortieIntel::default();
    let target = |class: &ClassId, def: &EnemyClassDef, pos: DVec2, heading_rad: f64, speed_kn: f64, tag: &str, group: Option<GroupId>| VesselInit {
        blueprint: BlueprintRef::Stock(def.blueprint.clone()), side: them, class: Some(class.clone()), group, pos: DVec3::new(pos.x, 0.0, pos.y), heading_rad, speed_kn, arrives_s: 0,
        damage: None, fuel_fraction: 1.0, ammo_fraction: 1.0, ammo: BTreeMap::new(), hull_record: None, veterancy: 0.0, tag: tag.into(), declared: Some(Stance::Hostile), emcon: None, passengers: 0, role: VesselRole::Target,
    };
    let mut act = Act::Act1;
    match options.target {
        SplashTarget::Drones => {
            if let Some((id, def)) = lowest_class(campaign, ClassRole::Drone).or_else(|| lowest_class(campaign, ClassRole::PatrolBoat)) {
                act = def.act;
                let centre = spawn + compass(heading) * DRONE_ORBIT_RANGE_M;
                let mut names = Vec::with_capacity(ORBIT_MARKERS);
                for k in 0..ORBIT_MARKERS {
                    let a = core::f64::consts::TAU * k as f64 / ORBIT_MARKERS as f64;
                    let p = centre + compass(a) * DRONE_ORBIT_RADIUS_M;
                    let name = format!("splash.orbit.{k}");
                    map.markers.push(Marker { kind: MarkerKind::EnemySpawn, name: name.clone(), pos_m: [p.x, p.y], heading_deg: (a + core::f64::consts::FRAC_PI_2).to_degrees().rem_euclid(360.0) });
                    names.push(name);
                }
                for i in 0..DRONE_COUNT {
                    let a = core::f64::consts::TAU * i as f64 / DRONE_COUNT as f64;
                    let p = centre + compass(a) * DRONE_ORBIT_RADIUS_M;
                    vessels.push(target(id, def, p, (a + core::f64::consts::FRAC_PI_2).rem_euclid(core::f64::consts::TAU), DRONE_SPEED_KN, DRONE_TAG, Some(GroupId(0))));
                }
                groups.push(GroupInit { id: GroupId(0), side: them, kind: FleetKind::Patrol, formation: Formation::Column, orders: GroupOrders::Patrol { markers: names }, ace: None, theatre_group: None, tag: DRONE_TAG.into() });
                intel.order_of_battle.push(id.clone());
                intel.declared_hostile.push(id.clone());
            }
        }
        SplashTarget::PatrolBoat => {
            if let Some((id, def)) = lowest_class(campaign, ClassRole::PatrolBoat) {
                act = def.act;
                let p = spawn + compass(heading + core::f64::consts::FRAC_PI_4) * PATROL_RANGE_M;
                let d = spawn - p;
                let toward = libm::atan2(d.x, -d.y).rem_euclid(core::f64::consts::TAU);
                vessels.push(target(id, def, p, toward, PATROL_SPEED_KN, PATROL_BOAT_TAG, None));
                intel.order_of_battle.push(id.clone());
                intel.declared_hostile.push(id.clone());
            }
        }
    }
    SortieConfig {
        protocol: crate::protocol::PROTOCOL_VERSION, seed, mode: GameMode::Splash, rules: ModeRules::Splash,
        settings: SimSettings { difficulty: Default::default(), ironman: false, ruleset: basin_ruleset(), guest_scripts: false, sim_threads: 3 },
        act, weather: basin_weather(options.water, seed), weather_changes: Vec::new(), time_of_day: TimeOfDay::Day, map, layer_depth_m: 50.0,
        sides, players: vec![player], blueprints: vec![bp], vessels, bodies: Vec::new(), groups, objective: Objective::DestroyAll, ghosts: Vec::new(), tags: Vec::new(),
        actions: Vec::new(), intel, cards: Vec::new(), operation: None, region: None, theatre_hour: 12, splash: Some(options), tutorial: None, time_limit_s: None,
    }
}

/// A splash ready to start: the design is built and the config made.
pub struct PreparedSplash {
    pub design: Arc<ShipDesign>,
    pub config: SortieConfig,
    pub blueprint_hash: ContentHash,
    /// Target class designs, built ahead so Space does not wait for them. Added by `sys-splash`.
    pub(crate) targets: Vec<Arc<ShipDesign>>,
}

/// Build everything that can be built before Space is pressed. Unit: `sys-splash`.
pub fn prepare(bp: &Blueprint, options: SplashOptions, seed: u64, campaign: &CampaignData, catalogue: &Catalogue) -> PreparedSplash {
    let config = splash_config(bp, options, seed, campaign);
    let ruleset = config.settings.ruleset.clone();
    let own = &config.blueprints[0];
    let prefab = Arc::new(sim_core::vessel::build_prefab(own, catalogue));
    let design = Arc::new(ShipDesign::build(own, prefab, catalogue, &ruleset));
    let mut targets: Vec<Arc<ShipDesign>> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for v in &config.vessels {
        let BlueprintRef::Stock(path) = &v.blueprint else { continue };
        if seen.contains(&path.as_str()) { continue; }
        seen.push(path);
        // A missing stock class leaves the target unbuilt here; `Sim::new` reports it.
        let Ok(tbp) = content::campaign::load::stock_blueprint(path) else { continue };
        let tbp = canonical(&tbp);
        let p = Arc::new(sim_core::vessel::build_prefab(&tbp, catalogue));
        targets.push(Arc::new(ShipDesign::build(&tbp, p, catalogue, &ruleset)));
    }
    PreparedSplash { blueprint_hash: design.hash, design, config, targets }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplashStage { /// Ship and calm water only; accepts helm at once.
    ShipOnly, Full }

/// Start the Sim from a prepared splash. With `SplashStage::ShipOnly` targets and sea state are scheduled
/// one second in (progressive splash). Unit: `sys-splash`.
pub fn launch(prepared: &PreparedSplash, mut deps: SimDeps, stage: SplashStage) -> Result<Sim, SimError> {
    let config = match stage { SplashStage::Full => prepared.config.clone(), SplashStage::ShipOnly => progressive(&prepared.config) };
    deps.designs.insert(prepared.design.clone());
    for t in &prepared.targets { if deps.designs.get(t.hash).is_none() { deps.designs.insert(t.clone()); } }
    Sim::new(config, deps)
}

/// The progressive form of a basin config: calm water and no targets at start; the targets arrive and the
/// sea rises [`PROGRESSIVE_DELAY_S`] later.
pub fn progressive(config: &SortieConfig) -> SortieConfig {
    let mut c = config.clone();
    let player_side = c.players.first().map(|p| p.side).unwrap_or(SideId(0));
    for v in c.vessels.iter_mut().filter(|v| v.side != player_side) { v.arrives_s = v.arrives_s.max(PROGRESSIVE_DELAY_S); }
    for b in c.bodies.iter_mut() { b.arrives_s = b.arrives_s.max(PROGRESSIVE_DELAY_S); }
    let full = c.weather;
    let calm = WeatherState { sea_state: SeaState::CALM, wind_speed_mps: WIND_MPS[0], kind: WeatherKind::Clear, rain: 0.0, visibility_m: WeatherKind::Clear.visibility_m(), cloud_cover: 0.2, ..full };
    if calm != full {
        c.weather = calm;
        for w in c.weather_changes.iter_mut() { w.0 = w.0.max(PROGRESSIVE_DELAY_S); }
        c.weather_changes.insert(0, (PROGRESSIVE_DELAY_S, full));
    }
    c
}

/// The report card shown when a splash ends (L216).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReportCard { pub duration_s: f64, pub rounds_fired: u32, pub hit_rate: f64, pub targets_destroyed: u16, pub time_to_first_hit_s: Option<f64>, pub damage_taken: f64, pub peak_heel_deg: f64, pub score: f64, pub tip: Msg }

/// Build the report card and pick its one tip (the highest-ranked thing the player could change: from the
/// hold reasons seen most, the worst gauge, or the top Ship's Note). Unit: `sys-splash`.
///
/// `rounds_fired` counts every weapon fired (shells, missiles and torpedoes) and `hit_rate` is the share of
/// them that hit; `damage_taken` is the share of the ship's cells lost; `score` is the trial battery's score
/// (`objectives::trial_score`). A tip the Sim already put in the summary is kept.
pub fn report_card(sim: &Sim, summary: &SortieSummary) -> ReportCard {
    let s = &summary.stats;
    let (shots, hits) = shots_and_hits(s);
    let log = sim.splash_log.as_ref();
    let tick_s = 1.0 / sim_core::time::TICK_HZ as f64;
    let start = log.map_or(0, |l| l.start_tick);
    let time_to_first_hit_s = log.and_then(|l| l.first_hit_tick).map(|t| t.saturating_sub(start) as f64 * tick_s);
    let live = player_hull(sim).and_then(|e| sim.world.vessel(e));
    let peak_heel_deg = log.map_or(0.0, |l| l.peak_heel_deg).max(live.map_or(0.0, |v| heel_deg(&v.kin.pose)));
    let lost = player_hull(sim).map(|e| match sim.world.vessel(e) { Some(v) if !matches!(v.status, sim_core::vessel::VesselStatus::Lost(_)) => v.cells_lost_fraction(), _ => 1.0 }).unwrap_or(0.0);
    let damage_taken = finite01(s.own_cells_lost_fraction).max(finite01(lost));
    let tip = match &summary.tip { Some(t) if !t.is_empty() => t.clone(), _ => tip(sim) };
    ReportCard {
        duration_s: summary.duration_ticks as f64 * tick_s,
        rounds_fired: shots,
        hit_rate: if shots > 0 { (hits as f64 / shots as f64).clamp(0.0, 1.0) } else { 0.0 },
        targets_destroyed: targets_destroyed(sim),
        time_to_first_hit_s,
        damage_taken,
        peak_heel_deg,
        score: { let v = crate::objectives::trial_score(s); if v.is_finite() { v } else { 0.0 } },
        tip,
    }
}

/// The one tip for a finished splash, also placed in `SortieSummary::tip` by the Sim in Splash and Tutorial
/// modes. Unit: `sys-splash`.
pub fn tip(sim: &Sim) -> Msg {
    let cat = &*sim.deps.catalogue;
    let ship = player_hull(sim).and_then(|e| sim.ships.get(e));
    let (gauges, notes) = match ship {
        Some(ship) => {
            let bp = ship.design.blueprint();
            let cap = sim.config.settings.ruleset.tonnage_cap_t;
            let cap = if cap.is_finite() && cap > 0.0 { cap } else { f64::MAX };
            let gauges = super::gauges::all_gauges(bp, &ship.design, cap, 0.0, cat);
            let notes = super::notes::ship_notes(bp, &ship.design, &gauges, None, &[], cat);
            (Some(gauges), notes)
        }
        None => (None, Vec::new()),
    };
    let (shots, hits) = shots_and_hits(&sim.objectives.stats);
    let lost = player_hull(sim).and_then(|e| sim.world.vessel(e)).map_or(0.0, |v| v.cells_lost_fraction());
    let holds = sim.splash_log.as_ref().map(|l| l.ranked_holds()).unwrap_or_default();
    let targets = sim.config.vessels.iter().any(|v| side_faction(&sim.config, v.side) == Some(Faction::Target));
    pick_tip(&TipFacts { notes: &notes, gauges: gauges.as_ref(), holds: &holds, shots, hits, cells_lost: finite01(sim.objectives.stats.own_cells_lost_fraction).max(finite01(lost)), targets })
}

/// Every `splash.*` key this unit can emit (`game/i18n/splash.csv`). Unit: `sys-splash`.
pub fn text_keys() -> Vec<&'static str> {
    let mut v = vec!["splash.side.ship", "splash.side.targets"];
    v.extend(HoldTip::ALL.iter().map(|h| h.key()));
    v.extend(GAUGE_TIPS.iter().map(|(_, k)| *k));
    v.extend(["splash.tip.no_shots", "splash.tip.low_hit_rate", "splash.tip.heavy_damage", "splash.tip.none"]);
    v
}

// ------------------------------------------------------------------------------------------------------------
// Bookkeeping during a splash
// ------------------------------------------------------------------------------------------------------------

/// Design-related hold reasons a tip can name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum HoldTip { TrainLimit, ElevationLimit, Masked, OutOfRange, Power, Ammunition, Slewing, Damaged, Emcon, NoSolution, Identification }
impl HoldTip {
    pub const ALL: [HoldTip; 11] = [Self::TrainLimit, Self::ElevationLimit, Self::Masked, Self::OutOfRange, Self::Power, Self::Ammunition, Self::Slewing, Self::Damaged, Self::Emcon, Self::NoSolution, Self::Identification];
    pub fn of(reason: &HoldReason) -> Option<HoldTip> {
        Some(match reason {
            HoldReason::TrainLimit { .. } => Self::TrainLimit,
            HoldReason::ElevationLimit { .. } => Self::ElevationLimit,
            HoldReason::Masked => Self::Masked,
            HoldReason::OutOfRange { .. } => Self::OutOfRange,
            HoldReason::Unpowered | HoldReason::Shed => Self::Power,
            HoldReason::NoAmmunition | HoldReason::Reloading { .. } => Self::Ammunition,
            HoldReason::Slewing => Self::Slewing,
            HoldReason::Damaged => Self::Damaged,
            HoldReason::EmconSilent => Self::Emcon,
            HoldReason::NoSolution => Self::NoSolution,
            HoldReason::Stance { .. } | HoldReason::Id { .. } | HoldReason::Grade { .. } | HoldReason::SuspectOutsideSelfDefence { .. } => Self::Identification,
            HoldReason::NoTarget | HoldReason::NeutralInSeekerBasket { .. } | HoldReason::FriendInLine { .. } | HoldReason::SalvoInFlight | HoldReason::PermissionWithheld | HoldReason::PlayerControl | HoldReason::ShoreSite => return None,
        })
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::TrainLimit => "splash.tip.train_limit",
            Self::ElevationLimit => "splash.tip.elevation_limit",
            Self::Masked => "splash.tip.masked",
            Self::OutOfRange => "splash.tip.out_of_range",
            Self::Power => "splash.tip.power",
            Self::Ammunition => "splash.tip.ammunition",
            Self::Slewing => "splash.tip.slewing",
            Self::Damaged => "splash.tip.damaged",
            Self::Emcon => "splash.tip.emcon",
            Self::NoSolution => "splash.tip.no_solution",
            Self::Identification => "splash.tip.identification",
        }
    }
    /// Whether the message names the mount.
    fn names_mount(self) -> bool { !matches!(self, Self::Emcon | Self::Identification) }
}

/// What the report card needs that the sortie stats do not keep.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SplashLog {
    pub start_tick: u64,
    pub first_hit_tick: Option<u64>,
    /// Largest heel of the player's ship seen, degrees.
    pub peak_heel_deg: f64,
    /// Mount-seconds spent holding against a target, by design-related reason and mount.
    pub holds: BTreeMap<(HoldTip, PartSlot), u32>,
}
impl SplashLog {
    pub fn new(start_tick: u64) -> Self { Self { start_tick, ..Default::default() } }
    /// Hold reasons, most seen first (ties: reason order, then mount).
    pub fn ranked_holds(&self) -> Vec<(HoldTip, PartSlot, u32)> {
        let mut v: Vec<(HoldTip, PartSlot, u32)> = self.holds.iter().map(|((h, s), n)| (*h, *s, *n)).collect();
        v.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
        v
    }
}

/// Per-tick splash bookkeeping, called by `Sim::step` in phase 13 when the config is a splash: first hit,
/// peak heel every tick, hold reasons of the player's mounts once a second. Does nothing outside a splash.
pub(crate) fn observe(sim: &mut Sim) {
    let tick = sim.world.tick();
    let hull = player_hull(sim);
    let (_, hits) = shots_and_hits(&sim.objectives.stats);
    let heel = hull.and_then(|e| sim.world.vessel(e)).map(|v| heel_deg(&v.kin.pose));
    let holds: Vec<(HoldTip, PartSlot)> = if tick.is_due(1, 0) {
        hull.and_then(|e| sim.ships.get(e)).map(|s| s.stages.engage.mounts.iter().filter_map(|m| match m.status {
            EngageStatus::Holding { track, reason } if !track.is_none() => HoldTip::of(&reason).map(|h| (h, m.slot)),
            _ => None,
        }).collect()).unwrap_or_default()
    } else { Vec::new() };
    let Some(log) = sim.splash_log.as_mut() else { return };
    if log.first_hit_tick.is_none() && hits > 0 { log.first_hit_tick = Some(tick.0); }
    if let Some(h) = heel { if h > log.peak_heel_deg { log.peak_heel_deg = h; } }
    for k in holds { let n = log.holds.entry(k).or_insert(0); *n = n.saturating_add(1); }
}

// ------------------------------------------------------------------------------------------------------------
// The tip
// ------------------------------------------------------------------------------------------------------------

/// Gauges in tip order with their keys.
const GAUGE_TIPS: [(&str, &str); 8] = [
    ("float", "splash.tip.gauge.float"), ("speed", "splash.tip.gauge.speed"), ("power", "splash.tip.gauge.power"), ("punch", "splash.tip.gauge.punch"),
    ("sight", "splash.tip.gauge.sight"), ("resilience", "splash.tip.gauge.resilience"), ("endurance", "splash.tip.gauge.endurance"), ("complexity", "splash.tip.gauge.complexity"),
];

pub(crate) struct TipFacts<'a> {
    /// Ship's Notes, ranked (first is top).
    pub notes: &'a [ShipNote],
    pub gauges: Option<&'a GaugeStrip>,
    /// Hold reasons, most seen first.
    pub holds: &'a [(HoldTip, PartSlot, u32)],
    pub shots: u32,
    pub hits: u32,
    pub cells_lost: f64,
    /// The basin had targets.
    pub targets: bool,
}

fn gauge_checks(g: &GaugeStrip) -> [Check; 8] { [g.float.check, g.speed.check, g.power.check, g.punch.check, g.sight.check, g.resilience.check, g.endurance.check, g.complexity.check] }

pub(crate) fn pick_tip(f: &TipFacts<'_>) -> Msg {
    let note = |pred: &dyn Fn(NoteTier) -> bool| f.notes.iter().find(|n| pred(n.tier) && !n.text.is_empty()).map(|n| n.text.clone());
    if let Some(m) = note(&|t| t == NoteTier::CannotFloatMoveOrPower) { return m; }
    if let Some((h, slot, _)) = f.holds.iter().find(|h| h.2 >= MIN_HOLD_SECONDS) {
        let m = Msg::new(h.key());
        return if h.names_mount() { m.arg(MsgArg::Part(*slot)) } else { m };
    }
    if let Some(m) = note(&|t| t <= NoteTier::CannotWork) { return m; }
    if let Some(g) = f.gauges {
        let checks = gauge_checks(g);
        for want in [Check::Fail, Check::Caution] {
            if let Some(i) = checks.iter().position(|c| *c == want) { return Msg::new(GAUGE_TIPS[i].1); }
        }
    }
    if f.targets && f.shots == 0 { return Msg::new("splash.tip.no_shots"); }
    if f.shots >= LOW_HIT_MIN_SHOTS && (f.hits as f64) < LOW_HIT_RATE * f.shots as f64 {
        return Msg::new("splash.tip.low_hit_rate").num(100.0 * f.hits as f64 / f.shots as f64, Unit::Percent);
    }
    if f.cells_lost > HEAVY_DAMAGE { return Msg::new("splash.tip.heavy_damage").num(100.0 * f.cells_lost.min(1.0), Unit::Percent); }
    if let Some(m) = note(&|_| true) { return m; }
    Msg::new("splash.tip.none")
}

// ------------------------------------------------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------------------------------------------------

/// Wind speed per sea-state code, the Beaufort scale's mid values (same table as the lobby weather).
const WIND_MPS: [f64; 9] = [0.5, 2.0, 4.5, 7.0, 9.5, 12.5, 16.0, 20.0, 26.0];

/// The basin ruleset: everything the dry dock allows, no tonnage or complexity cap.
pub fn basin_ruleset() -> content::ruleset::Ruleset {
    content::ruleset::Ruleset { kind: content::ruleset::RulesetKind::Open, tonnage_cap_t: 1.0e9, complexity_per_vessel: 0.0, complexity_per_lobby: 0.0, part_limits: BTreeMap::new(), max_length_m: 90.0, luau: true }
}

/// Daylight weather for a sea state: the clearest weather kind whose range carries it.
pub fn basin_weather(sea: SeaState, seed: u64) -> WeatherState {
    let code = sea.0.min(8);
    let kind = [WeatherKind::Clear, WeatherKind::Overcast, WeatherKind::Rain, WeatherKind::Storm].into_iter()
        .find(|k| { let (lo, hi) = k.sea_state_range(); (lo..=hi).contains(&code) }).unwrap_or(WeatherKind::Storm);
    let (cloud, rain) = match kind { WeatherKind::Clear => (0.2, 0.0), WeatherKind::Overcast => (0.9, 0.0), WeatherKind::Rain => (1.0, 0.6), _ => (1.0, 1.0) };
    let wind_dir = sim_core::rng::uniform(sim_core::rng::Key::new(seed, 0, sim_core::rng::Stream::Generator, 0x5350_4C41_5348, 0).draw(0)) * core::f64::consts::TAU;
    WeatherState { kind, sea_state: SeaState(code), wind_dir_rad: wind_dir, wind_speed_mps: WIND_MPS[code as usize], cloud_cover: cloud, visibility_m: kind.visibility_m(), rain, sun_elevation_deg: 40.0, sun_azimuth_deg: 180.0 }
}

/// The basin map with unusable values replaced: 10 km square, 100 m deep, spawn at the centre.
fn basin_map(m: &MapDef) -> MapDef {
    let size_ok = m.size_m.iter().all(|s| s.is_finite() && (2_000.0..=MAX_MAP_M).contains(s));
    let mut out = MapDef {
        size_m: if size_ok { m.size_m } else { [10_000.0, 10_000.0] },
        depth_m: if m.depth_m.is_finite() && m.depth_m > 0.0 { m.depth_m } else { 100.0 },
        islands: m.islands.iter().filter(|i| i.centre_m.iter().all(|c| c.is_finite()) && i.radius_m.is_finite() && i.radius_m > 0.0 && i.height_m.is_finite()).copied().collect(),
        markers: m.markers.iter().filter(|k| k.pos_m.iter().all(|c| c.is_finite()) && k.heading_deg.is_finite()).cloned().collect(),
        seed: m.seed,
    };
    if !out.markers.iter().any(|k| k.kind == MarkerKind::PlayerSpawn) {
        out.markers.push(Marker { kind: MarkerKind::PlayerSpawn, name: "spawn".into(), pos_m: [0.0, 0.0], heading_deg: 0.0 });
    }
    out
}

/// The lowest-act, lowest-tier class of a role (first in data order on ties); no submarines or capitals.
fn lowest_class(campaign: &CampaignData, role: ClassRole) -> Option<(&ClassId, &EnemyClassDef)> {
    campaign.classes.iter().filter(|(_, d)| d.role == role && !d.submersible && !d.capital && !d.blueprint.is_empty()).min_by_key(|(_, d)| (d.act, d.tier))
}

/// Unit vector of a compass bearing in world (x, z): 0 = north = -Z, clockwise.
fn compass(bearing_rad: f64) -> DVec2 { DVec2::new(libm::sin(bearing_rad), -libm::cos(bearing_rad)) }

/// The blueprint as every load leaves it: canonical form with its hash recomputed.
pub(crate) fn canonical(bp: &Blueprint) -> Blueprint {
    let mut b = bp.clone();
    content::blueprint::io::canonicalize_blueprint(&mut b);
    b.header.hash = content::blueprint::io::compute_hash(&b);
    b
}

/// Heel of a pose in degrees: the tilt of the beam axis out of the horizontal.
pub(crate) fn heel_deg(pose: &Pose) -> f64 {
    let z = pose.rot * DVec3::Z;
    let s = z.y.clamp(-1.0, 1.0);
    let h = libm::asin(s).abs().to_degrees();
    if h.is_finite() { h } else { 0.0 }
}

fn finite01(v: f64) -> f64 { if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 } }

fn shots_and_hits(s: &crate::protocol::config::SortieStats) -> (u32, u32) {
    let shots = s.rounds_fired.saturating_add(s.missiles_fired as u32).saturating_add(s.torpedoes_fired as u32);
    let hits = s.rounds_hit.saturating_add(s.missiles_hit as u32).saturating_add(s.torpedoes_hit as u32);
    (shots, hits.min(shots.max(s.rounds_hit)))
}

fn side_faction(config: &SortieConfig, side: SideId) -> Option<Faction> { config.sides.iter().find(|s| s.id == side).map(|s| s.faction) }

/// The splash player's hull: the first human player's vessel.
fn player_hull(sim: &Sim) -> Option<EntityId> {
    sim.players.iter().filter(|p| p.human).find_map(|p| p.vessel).or_else(|| sim.players.iter().find_map(|p| p.vessel))
}

/// Target hulls that are out of the fight: lost, disabled, wrecked or gone after spawning.
fn targets_destroyed(sim: &Sim) -> u16 {
    let mut n = 0u16;
    for (idx, e) in &sim.schedule.spawned {
        let Some(v) = sim.config.vessels.get(*idx as usize) else { continue };
        if side_faction(&sim.config, v.side) != Some(Faction::Target) { continue; }
        let out = match sim.world.vessel(*e) { Some(v) => !matches!(v.status, sim_core::vessel::VesselStatus::Active), None => true };
        if out { n = n.saturating_add(1); }
    }
    n
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::editor::notes::NoteKind;

    fn campaign_with(classes: &[(&str, ClassRole, Act)]) -> CampaignData {
        let mut c = CampaignData::default();
        c.theatre.basin = MapDef { size_m: [10_000.0, 10_000.0], depth_m: 100.0, islands: vec![], markers: vec![Marker { kind: MarkerKind::PlayerSpawn, name: "spawn".into(), pos_m: [0.0, 0.0], heading_deg: 90.0 }], seed: 0 };
        for (id, role, act) in classes {
            c.classes.insert(ClassId::new(*id), EnemyClassDef {
                id: ClassId::new(*id), name: TextKey::new("x"), blueprint: format!("{id}.bp.ron"), role: *role, tier: content::enums::Tier::T1, act: *act,
                behaviour: "patrol_boat".into(), engagement_range_m: None, variants: vec![], surrender: Default::default(), air_wing: vec![], submersible: false, capital: false,
            });
        }
        c
    }

    fn bp() -> Blueprint { content::blueprint::io::new_blueprint("Test", 30.0) }

    #[test]
    fn drone_basin_has_three_orbiting_targets_that_hold_fire_by_tag() {
        let camp = campaign_with(&[("class.pb_b", ClassRole::PatrolBoat, Act::Act2), ("class.drone", ClassRole::Drone, Act::Act1)]);
        let c = splash_config(&bp(), SplashOptions { water: SeaState::BEAUFORT_4, target: SplashTarget::Drones }, 7, &camp);
        assert_eq!(c.mode, GameMode::Splash);
        assert_eq!(c.rules, ModeRules::Splash);
        assert_eq!(c.vessels.len(), 1 + DRONE_COUNT);
        assert_eq!(c.vessels[0].role, VesselRole::PlayerHull);
        assert_eq!(c.players[0].vessel, Some(0));
        let hash = c.blueprints[0].header.hash;
        assert_eq!(c.vessels[0].blueprint, BlueprintRef::Inline(hash));
        assert_eq!(content::blueprint::io::compute_hash(&c.blueprints[0]), hash);
        for v in &c.vessels[1..] {
            assert_eq!(v.class.as_ref().map(|c| c.0.as_str()), Some("class.drone"));
            assert_eq!(v.tag, DRONE_TAG);
            assert_eq!(v.side, SideId(1));
            assert_eq!(v.declared, Some(Stance::Hostile));
            // On the orbit circle, 4 km ahead (spawn heading 090: east).
            let d = DVec2::new(v.pos.x, v.pos.z) - DVec2::new(DRONE_ORBIT_RANGE_M, 0.0);
            assert!((d.length() - DRONE_ORBIT_RADIUS_M).abs() < 1e-6);
        }
        assert_eq!(c.groups.len(), 1);
        let GroupOrders::Patrol { markers } = &c.groups[0].orders else { panic!("patrol orders") };
        assert_eq!(markers.len(), ORBIT_MARKERS);
        assert!(markers.iter().all(|m| c.map.markers.iter().any(|k| &k.name == m)));
        assert_eq!(c.weather.sea_state, SeaState::BEAUFORT_4);
        assert_eq!(c.weather.kind, WeatherKind::Clear);
        assert!(crate::sim::setup::validate_config(&c).is_ok());
    }

    #[test]
    fn patrol_boat_basin_picks_the_lowest_act_patrol_boat_closing_on_the_player() {
        let camp = campaign_with(&[("class.pb_b", ClassRole::PatrolBoat, Act::Act2), ("class.pb_a", ClassRole::PatrolBoat, Act::Act1), ("class.fac", ClassRole::FastAttack, Act::Act1)]);
        let c = splash_config(&bp(), SplashOptions { water: SeaState::BEAUFORT_7, target: SplashTarget::PatrolBoat }, 1, &camp);
        assert_eq!(c.vessels.len(), 2);
        let pb = &c.vessels[1];
        assert_eq!(pb.class.as_ref().map(|c| c.0.as_str()), Some("class.pb_a"));
        assert_eq!(pb.tag, PATROL_BOAT_TAG);
        assert!((DVec2::new(pb.pos.x, pb.pos.z).length() - PATROL_RANGE_M).abs() < 1e-6);
        // Heading points back at the player at the origin.
        let to_player = -DVec2::new(pb.pos.x, pb.pos.z).normalize();
        assert!((compass(pb.heading_rad) - to_player).length() < 1e-9);
        assert_eq!(c.weather.sea_state, SeaState::BEAUFORT_7);
        assert_eq!(c.weather.kind, WeatherKind::Rain);
        assert!(crate::sim::setup::validate_config(&c).is_ok());
    }

    #[test]
    fn drones_fall_back_to_a_patrol_boat_class_and_an_empty_basin_has_no_targets() {
        let camp = campaign_with(&[("class.pb_a", ClassRole::PatrolBoat, Act::Act1)]);
        let c = splash_config(&bp(), SplashOptions { water: SeaState::CALM, target: SplashTarget::Drones }, 1, &camp);
        assert_eq!(c.vessels.len(), 1 + DRONE_COUNT);
        let c = splash_config(&bp(), SplashOptions { water: SeaState::CALM, target: SplashTarget::PatrolBoat }, 1, &campaign_with(&[]));
        assert_eq!(c.vessels.len(), 1);
        assert!(crate::sim::setup::validate_config(&c).is_ok());
    }

    #[test]
    fn progressive_splash_brings_targets_and_sea_in_one_second_later() {
        let camp = campaign_with(&[("class.pb_a", ClassRole::PatrolBoat, Act::Act1)]);
        let c = splash_config(&bp(), SplashOptions { water: SeaState::BEAUFORT_7, target: SplashTarget::PatrolBoat }, 3, &camp);
        let p = progressive(&c);
        assert_eq!(p.vessels[0].arrives_s, 0);
        assert_eq!(p.vessels[1].arrives_s, PROGRESSIVE_DELAY_S);
        assert_eq!(p.weather.sea_state, SeaState::CALM);
        assert_eq!(p.weather_changes, vec![(PROGRESSIVE_DELAY_S, c.weather)]);
        // Already calm: nothing to schedule.
        let calm = splash_config(&bp(), SplashOptions { water: SeaState::CALM, target: SplashTarget::PatrolBoat }, 3, &camp);
        assert!(progressive(&calm).weather_changes.is_empty());
    }

    #[test]
    fn basin_config_is_deterministic_by_seed() {
        let camp = campaign_with(&[("class.drone", ClassRole::Drone, Act::Act1)]);
        let o = SplashOptions { water: SeaState::BEAUFORT_4, target: SplashTarget::Drones };
        assert_eq!(splash_config(&bp(), o, 5, &camp), splash_config(&bp(), o, 5, &camp));
        assert_ne!(splash_config(&bp(), o, 5, &camp).weather.wind_dir_rad, splash_config(&bp(), o, 6, &camp).weather.wind_dir_rad);
    }

    fn note(tier: NoteTier, key: &str) -> ShipNote { ShipNote { tier, kind: NoteKind::Advisory(String::new()), part: None, affected_parts: 1, text: Msg::new(key), fix: None, flips_tag: false, score_effect: None } }

    #[test]
    fn tip_ranking_follows_the_documented_order() {
        let holds = [(HoldTip::TrainLimit, PartSlot(3), 12)];
        let mut gauges = GaugeStrip::default();
        gauges.power.check = Check::Caution;
        gauges.sight.check = Check::Fail;
        let tier1 = [note(NoteTier::CannotFloatMoveOrPower, "notes.float_fail"), note(NoteTier::Advisory, "notes.advice")];
        let f = TipFacts { notes: &tier1, gauges: Some(&gauges), holds: &holds, shots: 0, hits: 0, cells_lost: 0.0, targets: true };
        assert_eq!(pick_tip(&f).key.0, "notes.float_fail");
        let tier3 = [note(NoteTier::CannotWork, "notes.arc_blocked")];
        let f = TipFacts { notes: &tier3, ..f };
        let m = pick_tip(&f);
        assert_eq!(m.key.0, "splash.tip.train_limit");
        assert_eq!(m.args, vec![MsgArg::Part(PartSlot(3))]);
        let few = [(HoldTip::TrainLimit, PartSlot(3), MIN_HOLD_SECONDS - 1)];
        let f = TipFacts { holds: &few, ..f };
        assert_eq!(pick_tip(&f).key.0, "notes.arc_blocked");
        let f = TipFacts { notes: &[], ..f };
        assert_eq!(pick_tip(&f).key.0, "splash.tip.gauge.sight", "Fail beats Caution");
        let ok = GaugeStrip::default();
        let f = TipFacts { gauges: Some(&ok), ..f };
        assert_eq!(pick_tip(&f).key.0, "splash.tip.no_shots");
        let f = TipFacts { shots: 40, hits: 2, ..f };
        let m = pick_tip(&f);
        assert_eq!(m.key.0, "splash.tip.low_hit_rate");
        assert_eq!(m.args, vec![MsgArg::Num { value: 5.0, unit: Unit::Percent }]);
        let f = TipFacts { shots: 40, hits: 20, cells_lost: 0.4, ..f };
        assert_eq!(pick_tip(&f).key.0, "splash.tip.heavy_damage");
        let adv = [note(NoteTier::Advisory, "notes.advice")];
        let f = TipFacts { cells_lost: 0.0, notes: &adv, ..f };
        assert_eq!(pick_tip(&f).key.0, "notes.advice");
        let f = TipFacts { notes: &[], ..f };
        assert_eq!(pick_tip(&f).key.0, "splash.tip.none");
    }

    #[test]
    fn identification_and_emcon_tips_name_no_mount() {
        let holds = [(HoldTip::Emcon, PartSlot(1), 30)];
        let f = TipFacts { notes: &[], gauges: None, holds: &holds, shots: 5, hits: 5, cells_lost: 0.0, targets: true };
        let m = pick_tip(&f);
        assert_eq!(m.key.0, "splash.tip.emcon");
        assert!(m.args.is_empty());
    }

    #[test]
    fn hold_ranking_is_by_count_then_reason() {
        let mut log = SplashLog::new(0);
        log.holds.insert((HoldTip::Masked, PartSlot(2)), 7);
        log.holds.insert((HoldTip::TrainLimit, PartSlot(5)), 7);
        log.holds.insert((HoldTip::Power, PartSlot(1)), 9);
        let r = log.ranked_holds();
        assert_eq!(r[0], (HoldTip::Power, PartSlot(1), 9));
        assert_eq!(r[1], (HoldTip::TrainLimit, PartSlot(5), 7));
        assert_eq!(r[2], (HoldTip::Masked, PartSlot(2), 7));
        assert_eq!(HoldTip::of(&HoldReason::NoTarget), None);
        assert_eq!(HoldTip::of(&HoldReason::Shed), Some(HoldTip::Power));
    }

    #[test]
    fn heel_of_a_rolled_pose() {
        let p = Pose { pos: DVec3::ZERO, rot: DQuat::from_xyzw(libm::sin(0.5 * 0.3), 0.0, 0.0, libm::cos(0.5 * 0.3)) };
        assert!((heel_deg(&p) - 0.3f64.to_degrees()).abs() < 1e-9);
        assert_eq!(heel_deg(&Pose::default()), 0.0);
    }

    pub(crate) fn real_deps() -> SimDeps {
        SimDeps {
            catalogue: Arc::new(content::catalogue::catalogue().clone()), campaign: Arc::new(content::campaign::load::campaign().clone()), designs: Default::default(),
            scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default(),
        }
    }

    #[test]
    fn a_full_drone_splash_starts_from_the_prepared_designs() {
        let deps = real_deps();
        let bp = content::campaign::load::stock_blueprint("starters/fac22.bp.ron").unwrap();
        let prepared = prepare(&bp, SplashOptions { water: SeaState::BEAUFORT_4, target: SplashTarget::Drones }, 11, &deps.campaign, &deps.catalogue);
        assert_eq!(prepared.targets.len(), 1, "one drone class, built once");
        let sim = launch(&prepared, deps, SplashStage::Full).expect("the basin starts");
        assert_eq!(sim.ships.ships.len(), 1 + DRONE_COUNT);
        let me = player_hull(&sim).expect("the player has a hull");
        let mine = sim.ships.get(me).unwrap();
        assert!(Arc::ptr_eq(&mine.design, &prepared.design), "Space only swaps worlds: the prepared design is used");
        assert!(mine.captain.is_none() && !mine.control.officers_in_command);
        let drones: Vec<&crate::ship::Ship> = sim.ships.ships.iter().filter(|s| s.tag == DRONE_TAG).collect();
        assert_eq!(drones.len(), DRONE_COUNT);
        for d in &drones {
            assert!(Arc::ptr_eq(&d.design, &prepared.targets[0]), "target designs are the prepared ones");
            let c = d.captain.as_ref().expect("drones have captains");
            assert_eq!(c.behaviour, crate::ai::unit::Behaviour::Proxy);
            assert!(!c.route.is_empty(), "drones follow the orbit");
            assert!(d.orders.weapons_tight, "drones hold fire");
        }
        let group = sim.ai.groups.iter().find(|g| g.id == GroupId(0) && g.side == SideId(1)).expect("the drone group");
        assert_eq!(group.members.len(), DRONE_COUNT);
        assert!(sim.splash_log.is_some());
        assert!(sim.out.iter().any(|e| matches!(e.event, crate::protocol::event::SimEvent::SortieStarted { .. })));
    }

    #[test]
    fn a_progressive_patrol_boat_splash_starts_with_the_ship_alone() {
        let deps = real_deps();
        let bp = content::campaign::load::stock_blueprint("starters/patrol15.bp.ron").unwrap();
        let prepared = prepare(&bp, SplashOptions { water: SeaState::BEAUFORT_7, target: SplashTarget::PatrolBoat }, 2, &deps.campaign, &deps.catalogue);
        let sim = launch(&prepared, deps, SplashStage::ShipOnly).expect("the basin starts");
        assert_eq!(sim.ships.ships.len(), 1, "targets arrive later");
        assert_eq!(sim.world.weather().sea_state, SeaState::CALM);
        assert_eq!(sim.schedule.vessels, vec![1]);
        assert_eq!(sim.schedule.weather, vec![0]);
        // The patrol boat's design is already cached for its arrival.
        let BlueprintRef::Stock(path) = &sim.config.vessels[1].blueprint else { panic!("stock patrol boat") };
        assert!(prepared.targets.iter().any(|t| sim.deps.designs.get(t.hash).is_some()), "{path}");
    }

    #[test]
    fn every_key_has_a_csv_row_that_passes_the_copy_lint() {
        use crate::text::csv_check::{lint, parse, placeholder_count, GLOSSARY};
        let rows = parse(include_str!("../../../../game/i18n/splash.csv"));
        let keys = text_keys();
        for k in &keys {
            let (_, en) = rows.get(*k).unwrap_or_else(|| panic!("no splash.csv row for {k}"));
            lint(k, en, 120, GLOSSARY);
        }
        assert_eq!(rows.len(), keys.len(), "splash.csv rows without a key");
        // Placeholders match what `pick_tip` passes.
        for h in HoldTip::ALL { assert_eq!(placeholder_count(&rows[h.key()].1), usize::from(h.names_mount()), "{}", h.key()); }
        assert_eq!(placeholder_count(&rows["splash.tip.low_hit_rate"].1), 1);
        assert_eq!(placeholder_count(&rows["splash.tip.heavy_damage"].1), 1);
        assert_eq!(placeholder_count(&rows["splash.tip.none"].1), 0);
    }
}
