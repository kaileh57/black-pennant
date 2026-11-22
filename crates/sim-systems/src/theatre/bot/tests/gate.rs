//! The stock-officer gate (L317, L81) with one record per sortie, for finding out why an operation fails it.
//!
//! `gate_act1` plays the same sorties as `full_physics_act1` (same starter, config, pilot and tick loop as
//! `FullPhysicsResolver` and `editor::trials::run_headless`) but keeps the events and samples the player's
//! picture, and prints one `SORTIE` line per sortie and one `SUMMARY` line per operation, as `key=value`
//! pairs. Run it by hand:
//!
//! `BP_SEEDS=4 BP_OPS=op.act1.convoy_raid cargo test --release -p sim-systems --lib gate_act1 -- --ignored --nocapture`
//!
//! - `BP_OPS`: substring of the operation ids to run (all of Act 1 when unset). `BP_SEEDS`: seeds per operation
//!   (default 4), `BP_FIRST`: index of the first seed (default 0). `BP_MIN`: the cap in simulated minutes (45).
//! - `BP_DROP=n`: the L317 fallback for this run only: drop the n last-added droppable hostile hulls of every
//!   operation run (`CampaignRules::hostile_drop`; `all` drops every droppable one). Nothing is written to data.
//! - `BP_PILOT`: `full` (default, the gate) or `route` (the hull only sails the briefing route). The pilot no
//!   longer orders EMCON Active or designates (22 November 2026), so the diagnosis's `noradiate`,
//!   `nodesignate` and `nosensor` are accepted and mean `full`.
//! - `BP_EMCON`: the emission setting the player hull spawns with. Unset, the config's own (the generator
//!   applies an operation's authored setting only; the rest sail in at the design's). `design`: the starter's
//!   own setting (Burst, the default of L507); `silent`, `burst`, `active`: that one.
//! - `BP_STARTER=starters/patrol15.bp.ron`: sail this stock design instead of the operation's starter
//!   (`op_starter`: the gate's table, the fast attack craft unless the table names another, refitted by the
//!   Quartermaster for the briefing tags). The named design gets the same refit; `starter_fits` on the
//!   `SORTIE` line says whether what sailed passes the tags.
//! - `BP_ASSERT=1`: fail unless every operation run passes the gate at this seed count: wins at least 80%,
//!   the median inside 15 to 30 minutes and no sortie at the stop. For the nightly gate.
//! - The run always fails if the stand-in pilot orders an emission state after its first orders or designates
//!   a track: those are the player's and the officers' (L108, L507), not the gate's.
//! - `BP_CHECK=1`: also resolve each sortie through `FullPhysicsResolver` and assert the same outcome and
//!   length (doubles the run time; for checking this loop against the gate's).
//! - `BP_LOG=1`: print pilot commands, own-hull status changes and a 60 s sample line as they happen.
//!
//! What a `SORTIE` line holds (times in simulated seconds, `-` = never):
//! - `result` win, draw or loss (a win is what the theatre pays as one); `outcome` and `end`, the reason
//!   (`cap_own_disabled`: the run reached the cap with the player hull disabled, which the sortie rules do
//!   not end); `min`; `hostiles` committed and `vessels`; `limit_s`, the operation's time limit.
//! - Detection, from the player's side picture: `contact_s` first live track on any hostile hull and `obj_s`
//!   on a hull or site the objective names, with range and sources; `hostile_s` first Hostile stance on an
//!   objective hull, `fine_s` first grade Fine or better, the best stance, grade and rung held on one.
//!   `seen_s`: first time any enemy picture held the player hull.
//! - Fire: `fire_s` first own gun round, `rounds`/`hits` and `missiles`/`mhits` from the sortie stats,
//!   `shots_ev`/`hits_ev` counted from events, `launched_ev` own bodies put in the water or the air (missiles,
//!   torpedoes, mines and the boarding or landing boat) and `launch_s` the first; `mounts`, mount-seconds by engagement state while a hostile
//!   hull was tracked (the hold reasons), `ammo_out_s` first mount out of ammunition.
//! - Events reach the driver once per audience (a side that cannot draw the shooter gets its own copy with
//!   the shooter scrubbed), so counts take only the copy the player's side receives, and the shooter of a
//!   shell that hit the player hull is read from whichever copy still names it.
//! - Objective: every objective as state and progress (`g:` marks ghost conditions), `prog` the primary's
//!   progress over time, the named hulls out of action over their count, `named_cells` the largest share of
//!   cells a named hull lost, `named_exits` named hulls that reached an exit, all enemy hulls out, kills (by
//!   the player), station actions with their last state and block.
//! - Own hull: final status, `own_s` when it first left Afloat, cells lost, hits taken, damage cells by
//!   cause, the class that hit it most, groundings and collisions.
//! - Helm: `obj_min_m` closest approach to the objective point and `obj_min_s` when, `obj_end_m`,
//!   `named_min_m` closest approach to a hull the objective names, `exit_min_m` to an exit marker,
//!   `hostile_min_m` closest true hostile hull, `sailed_m`, `stopped_s` under 1 kn.
//! - EMCON: `emcon` the setting over time, `radiating_s` total, `rad_first_s`, `bursts` (starts of radiating).
//! - `orders`: the pilot's commands by kind with count and first time.
use super::super::super::abstract_combat::{prepare_config, success, FullPhysicsResolver, Pilot, PilotOrders, PlayerSetup, SortieResolver};
use super::*;
use crate::editor::trials::HeadlessPolicy;
use crate::protocol::command::Command;
use crate::protocol::event::SimEvent;
use crate::protocol::snapshot::{ObjectiveState, Viewer};
use crate::protocol::text::EngageStatus;
use content::campaign::Objective;
use sim_core::time::{KN, TICK_HZ};
use sim_core::vessel::VesselStatus;
use sim_core::PhysEvent;

const HZ: u64 = TICK_HZ as u64;
/// Picture and helm samples, in ticks (1 s); ship status samples (5 s).
const PICTURE_EVERY: u64 = HZ;
const STATUS_EVERY: u64 = 5 * HZ;

fn env_u(name: &str, default: u64) -> u64 { std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default) }

/// The pilot orders `BP_PILOT` asks for.
pub(super) fn pilot_orders() -> (String, PilotOrders) {
    let name = std::env::var("BP_PILOT").unwrap_or_else(|_| "full".into());
    let o = match name.as_str() {
        "full" | "noradiate" | "nodesignate" | "nosensor" => PilotOrders::ALL,
        "route" => PilotOrders { beyond_route: false },
        other => panic!("BP_PILOT={other}: expected full or route"),
    };
    (name, o)
}

/// Apply `BP_EMCON` to the player hulls of a config; returns the switch's name for the record.
pub(super) fn spawn_emcon(config: &mut SortieConfig) -> String {
    let name = std::env::var("BP_EMCON").unwrap_or_else(|_| "config".into());
    let set = match name.as_str() {
        "config" => return name,
        "design" => None,
        "silent" => Some(Emcon::Silent),
        "burst" => Some(Emcon::Burst),
        "active" => Some(Emcon::Active),
        other => panic!("BP_EMCON={other}: expected design, silent, burst or active"),
    };
    for v in config.vessels.iter_mut().filter(|v| v.role == VesselRole::PlayerHull) { v.emcon = set; }
    name
}

/// The shipped data with `BP_DROP` hostile hulls dropped from every operation in `ops` (this run only).
pub(super) fn dropped_data(d: &TheatreData, ops: &[OperationId]) -> (TheatreData, u8) {
    let drop: u8 = match std::env::var("BP_DROP").ok().as_deref() { None | Some("") | Some("0") => 0, Some("all") => u8::MAX, Some(n) => n.parse().expect("BP_DROP is a count or `all`") };
    if drop == 0 { return (d.clone(), 0); }
    let mut campaign = (*d.campaign).clone();
    for op in ops { campaign.rules.hostile_drop.insert(op.clone(), drop); }
    (TheatreData { campaign: Arc::new(campaign), catalogue: d.catalogue.clone() }, drop)
}

fn deps(d: &TheatreData) -> crate::sim::SimDeps {
    crate::sim::SimDeps { catalogue: d.catalogue.clone(), campaign: d.campaign.clone(), designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() }
}

fn secs(tick: Option<u64>) -> String { tick.map_or("-".into(), |t| format!("{:.0}", t as f64 / HZ as f64)) }
fn metres(m: f64) -> String { if m.is_finite() { format!("{m:.0}") } else { "-".into() } }

fn command_kind(c: &Command) -> String {
    match c {
        Command::SetEmcon(e) => format!("SetEmcon({e:?})"),
        Command::StartAction { action, .. } => format!("StartAction({action:?})"),
        Command::Designate { track } => if track.is_some() { "Designate".into() } else { "Undesignate".into() },
        other => { let s = format!("{other:?}"); s.split(|ch: char| !ch.is_alphanumeric()).next().unwrap_or("").to_string() }
    }
}

fn status_kind(s: &EngageStatus) -> String {
    match s {
        EngageStatus::Idle => "Idle".into(),
        EngageStatus::Engaging { reason, .. } => format!("Engaging:{reason:?}"),
        EngageStatus::Holding { reason, .. } => { let r = format!("{reason:?}"); format!("Holding:{}", r.split(|ch: char| !ch.is_alphanumeric()).next().unwrap_or("")) }
    }
}

fn top<K: core::fmt::Display + Ord + Clone>(m: &BTreeMap<K, u32>, n: usize) -> String {
    let mut v: Vec<(&K, &u32)> = m.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    if v.is_empty() { return "-".into(); }
    v.iter().take(n).map(|(k, c)| format!("{k}:{c}")).collect::<Vec<_>>().join(",")
}

/// What one gate sortie did.
pub(super) struct Record {
    /// The pilot ordered an emission state after the first tick, or designated a track (it must not).
    pub sensor_orders: u32,
    pub win: bool,
    pub end: &'static str,
    pub outcome: Outcome,
    pub minutes: f64,
    pub ticks: u64,
    pub line: String,
}

/// Play one gate sortie with the stand-in pilot and record it. `None`: the `Sim` refused the config.
pub(super) fn play(op: &content::campaign::OperationDef, index: u32, config: &SortieConfig, d: &TheatreData, orders: PilotOrders, max_ticks: u64, log: bool) -> Option<Record> {
    let setup = player_setup(Policy::Mixed);
    let prepared = prepare_config(config, &setup);
    let mut pilot = Pilot::new(&prepared, &setup, d).with_orders(orders);
    let objective_pos = pilot.objective_pos();
    let slot = prepared.players[0].slot;
    let me = prepared.players[0].side;
    let hostiles = prepared.vessels.iter().filter(|v| v.role == VesselRole::Combatant && v.side != me).count();
    let objective = prepared.objective.clone();
    let named = |h: &crate::objectives::HullFacts| -> bool {
        match &objective {
            Objective::DestroySite { .. } => h.role == VesselRole::Site,
            Objective::DestroyGroup { tag, .. } | Objective::Capture { tag } | Objective::CutOut { tag, .. } | Objective::Photograph { tag, .. } | Objective::Escort { tag, .. } => !tag.is_empty() && &h.tag == tag,
            Objective::DestroyAll => h.enemy,
            _ => false,
        }
    };
    let exits: Vec<DVec2> = prepared.map.markers.iter().filter(|m| matches!(m.kind, content::campaign::MarkerKind::Exit | content::campaign::MarkerKind::ConvoyExit)).map(|m| DVec2::from(m.pos_m)).collect();
    let mut sim = crate::sim::Sim::new(prepared.clone(), deps(d)).ok()?;
    let mut commands = Vec::new();

    // Detection.
    let (mut contact, mut obj_contact, mut obj_hostile, mut obj_fine): (Option<u64>, Option<u64>, Option<u64>, Option<u64>) = (None, None, None, None);
    let (mut contact_note, mut obj_note) = (String::new(), String::new());
    let (mut best_stance, mut best_grade, mut best_rung): (Option<Stance>, Option<TrackGrade>, Option<IdRung>) = (None, None, None);
    let mut seen: Option<u64> = None;
    // Fire.
    let (mut fire, mut launch): (Option<u64>, Option<u64>) = (None, None);
    let (mut shots_ev, mut hits_ev, mut hits_in, mut bodies_out, mut bodies_in) = (0u32, 0u32, 0u32, 0u32, 0u32);
    let mut shooters: BTreeMap<EntityId, u32> = BTreeMap::new();
    let mut ammo_out: Option<u64> = None;
    let mut progress: Vec<(u64, f32)> = Vec::new();
    let (mut obj_min_tick, mut named_min, mut exit_min) = (0u64, f64::INFINITY, f64::INFINITY);
    let mut damage_in: BTreeMap<String, u32> = BTreeMap::new();
    let mut mounts: BTreeMap<String, u32> = BTreeMap::new();
    // Own hull and helm.
    let mut own_left_afloat: Option<(u64, VesselStatus)> = None;
    let mut own_status = VesselStatus::default();
    let (mut groundings, mut collisions) = (0u32, 0u32);
    let (mut obj_min, mut hostile_min, mut sailed) = (f64::INFINITY, f64::INFINITY, 0.0f64);
    let mut last_pos: Option<DVec2> = None;
    let mut stopped_s = 0u32;
    // EMCON.
    let mut emcon_log: Vec<(u64, Emcon)> = Vec::new();
    let (mut bursts, mut rad_first): (u32, Option<u64>) = (0, None);
    let mut radiating_now: BTreeSet<PartSlot> = BTreeSet::new();
    // Orders and actions.
    let mut orders_log: BTreeMap<String, (u32, u64)> = BTreeMap::new();
    let mut sensor_orders = 0u32;
    let mut actions: BTreeMap<String, String> = BTreeMap::new();

    for i in 0..max_ticks {
        if sim.is_over() || pilot.done(&sim) { break; }
        commands.clear();
        pilot.commands(&sim, &mut commands);
        if !commands.is_empty() {
            for c in &commands {
                let e = orders_log.entry(command_kind(&c.command)).or_insert((0, i));
                e.0 += 1;
                if matches!(c.command, Command::Designate { .. }) || (matches!(c.command, Command::SetEmcon(_)) && i > HZ) { sensor_orders += 1; }
                if log { eprintln!("  t={:.0}s cmd {:?}", i as f64 / HZ as f64, c.command); }
            }
            sim.apply(&commands);
        }
        sim.set_host_load(crate::lod::HostLoad::IDLE);
        sim.step();
        let own = sim.players().first().and_then(|p| p.vessel).unwrap_or(EntityId::NONE);
        // Shells that hit the player hull this tick, and the shooter any copy of the event names.
        let mut struck: Vec<u64> = Vec::new();
        let mut named_shooter: BTreeMap<u64, EntityId> = BTreeMap::new();
        for e in sim.drain_events() {
            let mine = e.audience.admits(slot, me);
            match &e.event {
                SimEvent::Radiating { vessel, slot: part, on } if *vessel == own => {
                    if *on { if radiating_now.is_empty() { bursts += 1; rad_first.get_or_insert(i); } radiating_now.insert(*part); } else { radiating_now.remove(part); }
                }
                SimEvent::Phys(PhysEvent::ShellImpact { projectile, shooter, .. }) if !mine => { if *shooter != EntityId::NONE { named_shooter.insert(projectile.0 as u64, *shooter); } }
                SimEvent::Phys(p) if mine => match p {
                    PhysEvent::ProjectileSpawned(ps) if ps.shooter == own => { shots_ev += 1; fire.get_or_insert(i); }
                    PhysEvent::BodySpawned { launcher, .. } if *launcher == own => { bodies_out += 1; launch.get_or_insert(i); }
                    PhysEvent::BodySpawned { side, .. } if *side != me => bodies_in += 1,
                    PhysEvent::ShellImpact { shooter, vessel, .. } if *shooter == own && *vessel != own => hits_ev += 1,
                    PhysEvent::ShellImpact { projectile, shooter, vessel, .. } if *vessel == own => {
                        hits_in += 1;
                        struck.push(projectile.0 as u64);
                        if *shooter != EntityId::NONE { named_shooter.insert(projectile.0 as u64, *shooter); }
                    }
                    PhysEvent::CellsDamaged { vessel, changes, cause, .. } if *vessel == own => *damage_in.entry(format!("{cause:?}")).or_default() += changes.len() as u32,
                    PhysEvent::StatusChanged { vessel, status } if *vessel == own => {
                        own_status = *status;
                        if *status != VesselStatus::default() && own_left_afloat.is_none() { own_left_afloat = Some((i, *status)); }
                        if log { eprintln!("  t={:.0}s own status {:?}", i as f64 / HZ as f64, status); }
                    }
                    PhysEvent::Grounded { vessel, .. } if *vessel == own => groundings += 1,
                    PhysEvent::Collision { a, b, impulse, .. } if (*a == own || *b == own) && *impulse > 1.0e4 => collisions += 1,
                    _ => {}
                },
                _ => {}
            }
        }
        for p in struck { *shooters.entry(named_shooter.get(&p).copied().unwrap_or(EntityId::NONE)).or_default() += 1; }
        if i % PICTURE_EVERY != 0 { continue; }
        let Some(v) = sim.world().vessel(own) else { continue };
        let pos3 = v.kin.pose.pos;
        let pos = DVec2::new(pos3.x, pos3.z);
        if let Some(l) = last_pos { sailed += l.distance(pos); }
        last_pos = Some(pos);
        if v.forward_speed().abs() < KN { stopped_s += 1; }
        if pos.distance(objective_pos) < obj_min { obj_min = pos.distance(objective_pos); obj_min_tick = i; }
        for e in &exits { exit_min = exit_min.min(e.distance(pos)); }
        let hulls = &sim.objectives.hulls;
        for h in hulls.iter().filter(|h| named(h) && !h.lost) { named_min = named_min.min(DVec2::from(h.pos).distance(pos)); }
        for h in hulls.iter().filter(|h| h.enemy && h.role != VesselRole::Site && !h.lost) { hostile_min = hostile_min.min(DVec2::from(h.pos).distance(pos)); }
        if seen.is_none() { seen = sim.objectives.detected_tick; }
        let mut tracked = false;
        if let Some(p) = sim.pictures().side(me) {
            for t in p.tracks.iter().filter(|t| t.is_live()) {
                let Some(h) = hulls.iter().find(|h| h.vessel == t.truth) else { continue };
                let truth_range = DVec2::from(h.pos).distance(pos);
                if h.enemy {
                    tracked = true;
                    if contact.is_none() { contact = Some(i); contact_note = format!("{}m/{:?}", metres(truth_range), t.sources).replace(' ', ""); }
                }
                if named(h) {
                    if obj_contact.is_none() { obj_contact = Some(i); obj_note = format!("{}m/{:?}", metres(truth_range), t.sources).replace(' ', ""); }
                    if t.stance == Stance::Hostile { obj_hostile.get_or_insert(i); }
                    if t.grade >= TrackGrade::Fine { obj_fine.get_or_insert(i); }
                    best_stance = Some(best_stance.map_or(t.stance, |b| b.max(t.stance)));
                    best_grade = Some(best_grade.map_or(t.grade, |b| b.max(t.grade)));
                    best_rung = Some(best_rung.map_or(t.rung, |b| b.max(t.rung)));
                }
            }
        }
        if i % STATUS_EVERY != 0 { continue; }
        let snap = sim.snapshot(Viewer::Player(slot));
        let Some(ship) = snap.ship.as_ref() else { continue };
        if emcon_log.last().map(|e| e.1) != Some(ship.board.emcon) { emcon_log.push((i, ship.board.emcon)); }
        if ammo_out.is_none() && ship.mounts.iter().any(|m| matches!(m.status, EngageStatus::Holding { reason: crate::protocol::text::HoldReason::NoAmmunition, .. })) { ammo_out = Some(i); }
        if let Some(o) = snap.objectives.iter().find(|o| !o.ghost) {
            if progress.last().is_none_or(|l| (l.1 - o.progress).abs() >= 0.1) { progress.push((i, o.progress)); }
        }
        if tracked { for m in &ship.mounts { *mounts.entry(status_kind(&m.status)).or_default() += (STATUS_EVERY / HZ) as u32; } }
        for a in &ship.actions { actions.insert(format!("{:?}", a.action), format!("{:?}/{:.2}/{}", a.state, a.progress, a.block.map_or("-".into(), |b| format!("{b:?}")))); }
        if log && i % (60 * HZ) == 0 {
            eprintln!("t={:.0}s pos ({:.0},{:.0}) {:.1}/{:.1} kn emcon {:?} radiating {} tracks {} obj {:.0} m | fired {} hits {} in {} | {:?}", i as f64 / HZ as f64, pos.x, pos.y, ship.speed_kn, ship.ordered_speed_kn, ship.board.emcon,
                ship.board.radiating.len(), snap.tracks.iter().filter(|t| !t.last_known).count(), pos.distance(objective_pos), shots_ev, hits_ev, hits_in, snap.objectives.iter().map(|o| (o.ghost, o.state, o.progress)).collect::<Vec<_>>());
        }
    }
    let capped = !sim.is_over();
    let r = match sim.result() { Some(r) => r.clone(), None => sim.finish(Outcome::Draw).clone() };
    let win = success(&r);
    let minutes = r.duration_ticks as f64 / (HZ as f64 * 60.0);
    let snap = sim.snapshot(Viewer::Player(slot));
    let primary = snap.objectives.iter().find(|o| !o.ghost);
    let own = sim.players().first().and_then(|p| p.vessel).unwrap_or(EntityId::NONE);
    let own_hull = sim.objectives.hulls.iter().find(|h| h.vessel == own);
    let own_lost = r.players.first().is_some_and(|p| p.hull_lost);
    let time_up = prepared.time_limit_s.is_some_and(|s| r.duration_ticks >= s as u64 * HZ);
    let end = match r.outcome {
        Outcome::Victory => "objective_done",
        Outcome::Defeat if primary.is_some_and(|o| o.state == ObjectiveState::Failed) => "objective_failed",
        Outcome::Defeat if own_lost => "own_hull_lost",
        Outcome::Defeat => "defeat_other",
        Outcome::Withdrawn if time_up => "time_limit",
        Outcome::Withdrawn => "withdrew",
        Outcome::Draw if capped && own_status == VesselStatus::Disabled => "cap_own_disabled",
        Outcome::Draw if capped => "cap",
        Outcome::Draw => "draw",
        Outcome::Aborted => "aborted",
    };
    let result = if win { "win" } else if matches!(r.outcome, Outcome::Draw | Outcome::Withdrawn) { "draw" } else { "loss" };
    let hulls = &sim.objectives.hulls;
    let named_n = hulls.iter().filter(|h| named(h)).count();
    let named_out = hulls.iter().filter(|h| named(h) && h.out()).count();
    let named_struck = hulls.iter().filter(|h| named(h) && h.struck).count();
    let enemy_n = hulls.iter().filter(|h| h.enemy && h.role != VesselRole::Site).count();
    let enemy_out = hulls.iter().filter(|h| h.enemy && h.role != VesselRole::Site && h.out()).count();
    // Shells that hit the player hull, by the class (else the role) of the hull that fired them.
    let mut by_class: BTreeMap<String, u32> = BTreeMap::new();
    for (e, n) in &shooters {
        let name = match hulls.iter().find(|h| h.vessel == *e) { Some(h) => h.class.as_ref().map_or(format!("{:?}", h.role), |c| c.as_str().trim_start_matches("class.").to_string()), None => "unseen".into() };
        *by_class.entry(name).or_default() += n;
    }
    let top_shooter = top(&by_class, 3);
    let named_cells = hulls.iter().filter(|h| named(h)).filter_map(|h| sim.world().vessel(h.vessel)).map(|v| v.cells_lost_fraction()).fold(f64::NAN, f64::max);
    let named_exits = hulls.iter().filter(|h| named(h) && !h.exits.is_empty()).count();
    let mut prog_s: Vec<String> = progress.iter().map(|(t, p)| format!("{}:{p:.2}", secs(Some(*t)))).collect();
    if prog_s.len() > 8 { let tail = prog_s.split_off(prog_s.len() - 2); prog_s.truncate(6); prog_s.push("..".into()); prog_s.extend(tail); }
    let objectives: Vec<String> = snap.objectives.iter().map(|o| format!("{}{:?}:{:.2}", if o.ghost { "g:" } else { "" }, o.state, o.progress)).collect();
    let orders_s: Vec<String> = orders_log.iter().map(|(k, (n, t))| format!("{k}x{n}@{}", secs(Some(*t)))).collect();
    let emcon_s: Vec<String> = emcon_log.iter().map(|(t, e)| format!("{}:{e:?}", secs(Some(*t)))).collect();
    let actions_s: Vec<String> = actions.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let end_pos = own_hull.map(|h| DVec2::from(h.pos));
    let line = format!(
        "op={} seed={} result={} outcome={:?} end={} min={:.1} limit_s={} hostiles={} vessels={} sea={} vis_m={:.0} | contact_s={} contact={} obj_s={} obj={} hostile_s={} fine_s={} best={}/{}/{} seen_s={} | fire_s={} rounds={} hits={} missiles={} mhits={} torps={} shots_ev={} hits_ev={} launched_ev={} launch_s={} ammo_out_s={} mounts={} | objectives={} prog={} named_out={}/{} named_struck={} named_cells={} named_exits={} enemy_out={}/{} kills={} by_player={} actions={} | own={:?} own_s={} own_lost={} cells_lost={:.3} hits_in={} enemy_weapons={} damage_in={} top_shooter={} grounded={} collisions={} | obj_min_m={} obj_min_s={} obj_end_m={} named_min_m={} exit_min_m={} hostile_min_m={} sailed_m={:.0} stopped_s={} | emcon={} radiating_s={:.0} rad_first_s={} bursts={} | orders={}",
        op.id.as_str(), index, result, r.outcome, end, minutes, prepared.time_limit_s.map_or("-".into(), |s| s.to_string()), hostiles, prepared.vessels.len(), prepared.weather.sea_state.0, prepared.weather.visibility_m,
        secs(contact), if contact_note.is_empty() { "-" } else { &contact_note }, secs(obj_contact), if obj_note.is_empty() { "-" } else { &obj_note }, secs(obj_hostile), secs(obj_fine),
        best_stance.map_or("-".into(), |s| format!("{s:?}")), best_grade.map_or("-".into(), |s| format!("{s:?}")), best_rung.map_or("-".into(), |s| format!("{s:?}")), secs(seen),
        secs(fire), r.stats.rounds_fired, r.stats.rounds_hit, r.stats.missiles_fired, r.stats.missiles_hit, r.stats.torpedoes_fired, shots_ev, hits_ev, bodies_out, secs(launch), secs(ammo_out), top(&mounts, 4),
        if objectives.is_empty() { "-".into() } else { objectives.join(",") }, if prog_s.is_empty() { "-".into() } else { prog_s.join(",") }, named_out, named_n, named_struck, if named_cells.is_finite() { format!("{named_cells:.3}") } else { "-".into() }, named_exits,
        enemy_out, enemy_n, r.kills.len(), r.kills.iter().filter(|k| k.by_player.is_some()).count(), if actions_s.is_empty() { "-".into() } else { actions_s.join(",") },
        own_status, own_left_afloat.map_or("-".into(), |(t, s)| format!("{}:{s:?}", secs(Some(t)))), own_lost, r.stats.own_cells_lost_fraction, hits_in, bodies_in, top(&damage_in, 4), top_shooter, groundings, collisions,
        metres(obj_min), secs(Some(obj_min_tick)), end_pos.map_or("-".into(), |p| metres(p.distance(objective_pos))), metres(named_min), metres(exit_min), metres(hostile_min), sailed, stopped_s,
        if emcon_s.is_empty() { "-".into() } else { emcon_s.join(",") }, r.stats.radiating_s, secs(rad_first), bursts,
        if orders_s.is_empty() { "-".into() } else { orders_s.join(",") },
    );
    Some(Record { sensor_orders, win, end, outcome: r.outcome, minutes: if minutes.is_finite() { minutes.max(0.0) } else { 0.0 }, ticks: r.duration_ticks, line })
}

/// Median as `winnability` takes it (the mean of the two middle lengths of an even count).
fn median(minutes: &mut [f64]) -> f64 {
    minutes.sort_by(f64::total_cmp);
    match minutes.len() { 0 => 0.0, n if n % 2 == 1 => minutes[n / 2], n => 0.5 * (minutes[n / 2 - 1] + minutes[n / 2]) }
}

/// The Act 1 gate, one line per sortie and one per operation. See the module docs for the switches.
#[test]
#[ignore = "full-physics sorties; minutes of run time"]
fn gate_act1() {
    let shipped = data();
    let seeds = env_u("BP_SEEDS", 4) as u32;
    let first = env_u("BP_FIRST", 0) as u32;
    let cap_min = env_u("BP_MIN", 45);
    let max_ticks = cap_min * 60 * HZ;
    let filter = std::env::var("BP_OPS").unwrap_or_default();
    let check = std::env::var("BP_CHECK").is_ok();
    let log = std::env::var("BP_LOG").is_ok();
    let assert_gate = std::env::var("BP_ASSERT").is_ok_and(|v| v != "0");
    let mut failed: Vec<String> = Vec::new();
    let (pilot_name, orders) = pilot_orders();
    let mut ids: Vec<OperationId> = shipped.campaign.operations.values().filter(|o| o.act == Act::Act1 && o.kind != OperationKind::Tutorial && o.id.as_str().contains(filter.as_str())).map(|o| o.id.clone()).collect();
    ids.sort();
    let (d, drop) = dropped_data(&shipped, &ids);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap();
    pool.install(|| {
        for id in &ids {
            let op = &d.campaign.operations[id];
            let starter = match std::env::var("BP_STARTER") {
                // The named design, fitted for the operation's tags the way the gate's own starter is; bare
                // when it cannot be made to pass them.
                Ok(p) => { let bp = content::campaign::load::stock_blueprint(&p).expect("BP_STARTER names a stock blueprint"); fitted(&bp, op, Act::Act1, &d).unwrap_or(bp) }
                Err(_) => op_starter(op, Act::Act1, &d),
            };
            let starter_fits = fits(&starter, op, Act::Act1, &d);
            let t0 = std::time::Instant::now();
            let (mut wins, mut draws, mut losses, mut aborted, mut no_config, mut over_cap) = (0u32, 0u32, 0u32, 0u32, 0u32, 0u32);
            let mut minutes: Vec<f64> = Vec::new();
            let mut hostiles_seen: Vec<usize> = Vec::new();
            let mut ends: BTreeMap<&'static str, u32> = BTreeMap::new();
            for s in first..first + seeds {
                let Some(mut config) = winnability_config(id, Act::Act1, s, &starter, &d) else { no_config += 1; eprintln!("SORTIE op={} seed={s} result=no_config", id.as_str()); continue };
                let emcon_name = spawn_emcon(&mut config);
                if drop > 0 {
                    // The drop must be seen to drop: the same seed without it, by hostile count and map.
                    let base = winnability_config(id, Act::Act1, s, &starter, &shipped).expect("the undropped config");
                    let n = |c: &SortieConfig| c.vessels.iter().filter(|v| v.role == VesselRole::Combatant).count();
                    eprintln!("DROP op={} seed={s} combatants {} -> {} vessels {} -> {} map_same={} player_spawn_same={}", id.as_str(), n(&base), n(&config), base.vessels.len(), config.vessels.len(), base.map == config.map,
                        base.players.first().and_then(|p| p.vessel).map(|v| base.vessels[v as usize].pos) == config.players.first().and_then(|p| p.vessel).map(|v| config.vessels[v as usize].pos));
                }
                hostiles_seen.push(config.vessels.iter().filter(|v| v.role == VesselRole::Combatant).count());
                let Some(rec) = play(op, s, &config, &d, orders, max_ticks, log) else { aborted += 1; eprintln!("SORTIE op={} seed={s} result=aborted", id.as_str()); continue };
                if check {
                    let (cat, camp) = (d.catalogue.clone(), d.campaign.clone());
                    let mut resolver = FullPhysicsResolver { make_deps: Box::new(move || crate::sim::SimDeps { catalogue: cat.clone(), campaign: camp.clone(), designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() }), max_ticks, last_min: 0.0 };
                    let setup: PlayerSetup = player_setup(Policy::Mixed);
                    let r = resolver.resolve(&config, &setup, &d);
                    assert_eq!((r.outcome, r.duration_ticks), (rec.outcome, rec.ticks), "{} seed {s}: the traced loop and the resolver disagree", id.as_str());
                    assert_eq!(orders, PilotOrders::ALL, "BP_CHECK compares against the gate's pilot");
                    assert_eq!(emcon_name, "config", "BP_CHECK compares against the gate's config");
                    assert!(std::env::var("BP_STARTER").is_err(), "BP_CHECK compares against the gate's starter");
                }
                eprintln!("SORTIE {} pilot={pilot_name} drop={drop} spawn_emcon={emcon_name} starter={} starter_fits={starter_fits}", rec.line, starter.header.name.replace(' ', "_"));
                assert_eq!(rec.sensor_orders, 0, "{} seed {s}: the stand-in pilot ordered an emission state or a designation", id.as_str());
                if rec.win { wins += 1; } else if matches!(rec.outcome, Outcome::Draw | Outcome::Withdrawn) { draws += 1; } else { losses += 1; }
                if rec.minutes >= cap_min as f64 - 1e-9 { over_cap += 1; }
                *ends.entry(rec.end).or_default() += 1;
                minutes.push(rec.minutes);
            }
            let longest = minutes.iter().copied().fold(0.0, f64::max);
            let med = median(&mut minutes);
            let played = wins + draws + losses;
            // The gate at this seed count: 80% wins (16 of 20), the median inside 15 to 30 min, no run at the cap.
            let wins_ok = played > 0 && wins * 5 >= seeds * 4;
            let median_ok = (15.0..=30.0).contains(&med);
            let cap_ok = over_cap == 0;
            eprintln!(
                "SUMMARY op={} seeds={seeds} first={first} pilot={pilot_name} drop={drop} spawn_emcon={} wins={wins} draws={draws} losses={losses} aborted={aborted} no_config={no_config} median_min={med:.1} longest_min={longest:.1} at_cap={over_cap} ends={} hostiles={:?} wins_ok={wins_ok} median_ok={median_ok} cap_ok={cap_ok} gate={} wall_s={:.0}",
                id.as_str(), std::env::var("BP_EMCON").unwrap_or_else(|_| "config".into()), top(&ends, 6), hostiles_seen, if wins_ok && median_ok && cap_ok { "pass" } else { "fail" }, t0.elapsed().as_secs_f64()
            );
            if !(wins_ok && median_ok && cap_ok) { failed.push(format!("{} (wins {wins}/{seeds}, median {med:.1} min, {over_cap} at the stop)", id.as_str())); }
        }
    });
    assert!(!assert_gate || failed.is_empty(), "the Act 1 stock-officer gate fails at {seeds} seeds: {}", failed.join("; "));
}
