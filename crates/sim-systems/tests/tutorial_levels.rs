//! Unit `sys-tutorial`: every tutorial level played headless the way its cards direct (DESIGN.md "The first
//! ten minutes", L252-275), from the stock hull to the finish.
//!
//! The dock steps run on the shipped data: level 0 drags the widest station's beam handle in its 0.1 m steps
//! until Float shows Pass, and stops there, as the card says; every lesson kit goes on the valid pose nearest
//! its marked spot (`KitSpot`), in the order the card lists them and in the reverse order, with the dock's
//! default mirror symmetry; the blocked-arc note's own Fix is applied. The dock's pose search lives in the
//! Godot client (`crates/gdclient/src/editor/parts.rs`, `tutorial_spot`, `anchored`, `verdict`), which this
//! crate cannot call, so [`Dock`] repeats it on the same content functions: the same candidate order, the same
//! standing-socket anchor, the same mirror copy and the same refusal rules.
//!
//! The gauges and notes are the dock's: `all_gauges` and `ship_notes` on the design built under the tutorial's
//! ruleset (campaign, Act 1). The network check is sim-core's `unconnected_consumers`, as in
//! `stock_networks.rs`.
//!
//! The basin steps then run a real `TutorialRun` against real basins (`editor::splash::prepare` and `launch`),
//! fed by `tutorial::signal_from_event` and its own clock, over seeds 1 to 4. The test player does what the
//! drone and patrol cards ask: takes the helm, rings full ahead and steers for the nearest target. A step that
//! ends through its timed escape fails the test, so "the step completes" means the player's guns fired and a
//! drone fell (or the 30 s dwell passed) and the patrol boat struck or was disabled inside its 300 s.
use content::blueprint::Blueprint;
use content::catalogue::Catalogue;
use content::cell::{CellGrid, CellPos, Face, MAX_GRID_DIM};
use content::enums::Act;
use content::ids::{KitId, PlayerSlot};
use content::kit::{KitDef, PartRot};
use content::ruleset::Ruleset;
use content::settings::{SplashOptions, SplashTarget};
use content::validate::{validate_blueprint, Issue, ValidationContext};
use glam::DVec3;
use sim_core::vessel::{build_prefab, VesselStatus};
use sim_systems::editor::gauges::{all_gauges, GaugeStrip};
use sim_systems::editor::notes::{apply_edit, ship_notes, ShipNote};
use sim_systems::editor::{BlueprintEdit, Check};
use sim_systems::protocol::command::{Command, CommandEnvelope, ControlRole, PlayerInput};
use sim_systems::ship::ShipDesign;
use sim_systems::sim::SimDeps;
use sim_systems::tutorial::{note_id, script_for, signal_from_event, Effect, Script, Signal, TutorialRun};
use std::sync::Arc;

const SEEDS: [u64; 4] = [1, 2, 3, 4];
/// The gun part of `kit.gun`, the gun the player drops.
const LESSON_GUN: &str = "gun.76mm.single";
/// The most heel the finished design may lie at in the dock (Float's equilibrium heel).
const MAX_HEEL_DEG: f64 = 3.0;
const PLAYER: PlayerSlot = PlayerSlot(0);
/// Simulation seconds a basin step may take before the test gives up on it (past every escape).
const BASIN_LIMIT_S: u32 = 330;

/// The dock's search box around a spot and its check budget (`SPOT_REACH`, `SPOT_DEPTH`, `SPOT_SIDE`,
/// `MAX_SPOT_CHECKS` in the client).
const SPOT_REACH: i32 = 10;
const SPOT_DEPTH: i32 = 6;
const SPOT_SIDE: i32 = 5;
const MAX_SPOT_CHECKS: usize = 400;

fn cat() -> Arc<Catalogue> { Arc::new(content::catalogue::catalogue().clone()) }
fn ruleset() -> Ruleset { Ruleset::campaign(Act::Act1) }
fn stock(path: &str) -> Blueprint { content::campaign::load::stock_blueprint(path).unwrap_or_else(|e| panic!("{path}: {e}")) }

/// A design as the dock sees it: built, gauged and noted.
struct Seen { gauges: GaugeStrip, notes: Vec<ShipNote>, unconnected: Vec<String> }

fn see(bp: &Blueprint, cat: &Catalogue) -> Seen {
    let prefab = Arc::new(build_prefab(bp, cat));
    let design = Arc::new(ShipDesign::build(bp, prefab.clone(), cat, &ruleset()));
    let gauges = all_gauges(bp, &design, Act::Act1.hull_cap_t(), 0.0, cat);
    let notes = ship_notes(bp, &design, &gauges, None, &[], cat);
    let cut = sim_core::networks::unconnected_consumers(&prefab.networks, &prefab.parts, &prefab.compartments, cat);
    let unconnected = cut.iter().filter_map(|s| prefab.parts.iter().find(|p| p.slot == *s)).map(|p| format!("{} {}", p.slot.0, cat.parts.get_index(p.def.index()).map_or("?", |(k, _)| k.as_str()))).collect();
    Seen { gauges, notes, unconnected }
}

fn open_ids(s: &Seen) -> Vec<String> { s.notes.iter().map(|n| note_id(&n.kind).to_string()).collect() }

fn float_line(s: &Seen) -> String {
    let f = &s.gauges.float;
    format!("Float {:?}: GM {:.2} m ({:.1}% of beam), heel {:.2}, trim {:.2}, range {:.0} deg, {:.1} t", f.check, f.gm_m, 100.0 * f.gm_over_beam, f.heel_deg, f.trim_deg, f.range_of_stability_deg, f.displacement_t)
}

// ---- The dock's placement rules (crates/gdclient/src/editor/parts.rs) --------------------------------------

fn cell_of(c: [i32; 3]) -> Option<CellPos> { c.iter().all(|v| (0..MAX_GRID_DIM as i32).contains(v)).then(|| CellPos::new(c[0] as i16, c[1] as i16, c[2] as i16)) }

fn bounds(cells: &[CellPos]) -> Option<([i32; 3], [i32; 3])> {
    let mut lo = [i32::MAX; 3];
    let mut hi = [i32::MIN; 3];
    for c in cells {
        for (k, v) in [c.x, c.y, c.z].into_iter().enumerate() { lo[k] = lo[k].min(v as i32); hi[k] = hi[k].max(v as i32); }
    }
    (lo[0] != i32::MAX).then_some((lo, hi))
}

/// `mirror_sum`: twice the centreline cell plus one, for a lofted hull.
fn mirror_sum(bp: &Blueprint) -> Option<i32> {
    let content::hull::HullSource::Lofted(s) = &bp.hull.source else { return None };
    let l = content::hull::shell_layout(s);
    (l.half_width_cells > 0).then(|| 2 * l.half_width_cells as i32 - 1)
}

/// `anchored`: the kit's standing socket (the `deck` socket first) on `cell`, else its footprint centred there.
fn anchored(k: &KitDef, cat: &Catalogue, cell: CellPos, rot: PartRot) -> Option<(CellPos, Vec<CellPos>)> {
    let probe = content::kit::kit_footprint(k, cat, CellPos::new(0, 0, 0), rot, false);
    let (lo, hi) = bounds(&probe)?;
    let sockets = content::kit::kit_sockets_at(k, cat, CellPos::new(0, 0, 0), rot, false);
    let down: Vec<_> = sockets.iter().filter(|s| s.face == Face::YNeg).collect();
    let socket = down.iter().find(|s| s.name == "deck").or_else(|| down.first()).map(|s| s.cell);
    let pos = match socket {
        Some(s) => cell_of([cell.x as i32 - s.x as i32, cell.y as i32 - s.y as i32, cell.z as i32 - s.z as i32])?,
        None => {
            let ext = [hi[0] - lo[0] + 1, hi[2] - lo[2] + 1];
            cell_of([cell.x as i32 - ext[0] / 2, cell.y as i32, cell.z as i32 - ext[1] / 2])?
        }
    };
    let cells = content::kit::kit_footprint(k, cat, pos, rot, false);
    (cells.len() == probe.len()).then_some((pos, cells))
}

/// `new_problem`: whether the candidate has a problem the base design does not, touching the placed slots.
fn new_problem(base: &[Issue], cand: &[Issue], new: &std::ops::Range<usize>, next: &Blueprint) -> bool {
    let mine = |s: &content::ids::PartSlot| new.contains(&s.index());
    let added = |part: &content::ids::PartId| new.clone().any(|s| next.parts.get(s).is_some_and(|p| &p.part == part));
    cand.iter().filter(|i| !base.contains(i)).any(|i| match i {
        Issue::PartOutOfBounds { slot } | Issue::PartBadPlacement { slot } => mine(slot),
        Issue::PartsOverlap { a, b } => mine(a) || mine(b),
        Issue::UnknownPart { .. } => true,
        Issue::PartNotAvailable { part } | Issue::PartOverLimit { part, .. } => added(part),
        Issue::Disconnected { .. } => !base.iter().any(|b| matches!(b, Issue::Disconnected { .. })),
        _ => false,
    })
}

fn issues(bp: &Blueprint, cat: &Catalogue) -> Vec<Issue> {
    let r = ruleset();
    validate_blueprint(bp, cat, &ValidationContext { ruleset: &r, available: None, enemy_class: false, complexity: None }).issues
}

/// `verdict` with mirror symmetry and no turn: the design after placing the kit at `cell`, if valid.
fn verdict(bp: &Blueprint, base: &[Issue], k: &KitDef, cell: CellPos, cat: &Catalogue) -> Option<Blueprint> {
    let rot = PartRot::default();
    let (pos, cells) = anchored(k, cat, cell, rot)?;
    let mut edits = vec![BlueprintEdit::AddKit { kit: k.id.clone(), pos, rot, mirrored: false }];
    if let Some(s) = mirror_sum(bp) {
        let mut target: Vec<CellPos> = cells.iter().filter_map(|c| cell_of([c.x as i32, c.y as i32, s - c.z as i32])).collect();
        target.sort();
        if target.len() == cells.len() {
            if let Some(mpos) = bounds(&target).and_then(|(lo, _)| cell_of(lo)) {
                // The default pose is its own mirror image; the copy is the mirrored kit.
                let mcells = content::kit::kit_footprint(k, cat, mpos, rot, true);
                if mcells == target && !mcells.iter().any(|x| cells.binary_search(x).is_ok()) {
                    edits.push(BlueprintEdit::AddKit { kit: k.id.clone(), pos: mpos, rot, mirrored: true });
                }
            }
        }
    }
    let edit = if edits.len() == 1 { edits.pop().unwrap() } else { BlueprintEdit::Several(edits) };
    let next = apply_edit(bp, &edit, cat)?;
    let new_slots = bp.parts.len()..next.parts.len();
    (!new_problem(base, &issues(&next, cat), &new_slots, &next)).then_some(next)
}

/// `tutorial_spot` then the click: the design with the kit on the valid pose nearest its spot, and the cell.
fn place_on_spot(bp: &Blueprint, script: &Script, kit: &KitId, cat: &Catalogue) -> Option<(Blueprint, CellPos)> {
    let spot = script.spot_for(kit)?;
    let k = cat.kit(kit)?.clone();
    let size = CellGrid::from_rows(&bp.cells).size;
    let want = [spot.x as i32, spot.y as i32, size[2] as i32 / 2 + spot.dz as i32];
    let mut cands: Vec<[i32; 3]> = Vec::new();
    for dx in -SPOT_REACH..=SPOT_REACH {
        for dy in -SPOT_DEPTH..=SPOT_DEPTH {
            for dz in -SPOT_SIDE..=SPOT_SIDE { cands.push([want[0] + dx, want[1] + dy, want[2] + dz]); }
        }
    }
    let d2 = |c: &[i32; 3]| (0..3).map(|i| (c[i] - want[i]) * (c[i] - want[i])).sum::<i32>();
    cands.sort_by_key(|c| (d2(c), *c));
    let base = issues(bp, cat);
    cands.into_iter().filter_map(cell_of).take(MAX_SPOT_CHECKS).find_map(|c| verdict(bp, &base, &k, c, cat).map(|next| (next, c)))
}

// ---- The beam step ----------------------------------------------------------------------------------------

/// The beam handle the card points at (`pick_beam`: the widest station, the first of equals) and its value.
fn widest_station(bp: &Blueprint) -> (usize, f64) {
    let content::hull::HullSource::Lofted(s) = &bp.hull.source else { panic!("the tutorial hull is lofted") };
    s.stations.iter().enumerate().fold((0, f64::MIN), |best, (i, st)| if st.beam_m > best.1 { (i, st.beam_m) } else { best })
}

fn set_beam(bp: &Blueprint, station: usize, beam_m: f64, cat: &Catalogue) -> Option<Blueprint> {
    apply_edit(bp, &BlueprintEdit::SetHullHandle { station: station as u8, handle: "beam".into(), value: beam_m }, cat)
}

/// Drag the beam handle out in its 0.1 m steps and let go at the first Pass. Returns the design and its beam.
fn drag_beam_to_pass(bp: &Blueprint, cat: &Catalogue) -> (Blueprint, f64) {
    let (station, from) = widest_station(bp);
    for step in 1..=60 {
        let beam = from + 0.1 * step as f64;
        let Some(next) = set_beam(bp, station, beam, cat) else { break };
        if see(&next, cat).gauges.float.check == Check::Pass { return (next, beam); }
    }
    panic!("dragging the beam handle out from {from:.1} m never brings Float to Pass");
}

// ---- Playing a level --------------------------------------------------------------------------------------

/// The dock half of a level, played as the cards direct, with what it found.
struct Docked { run: TutorialRun, bp: Blueprint, report: Vec<String> }

/// What the test player does on a step: report what the step waits for.
fn lesson_kits(run: &TutorialRun) -> Vec<KitId> { run.kits_progress().0 }

/// Carry out effects the dock owns; returns the splash options set for the next splash, if any.
fn effects(out: &[Effect], bp: &mut Blueprint, cat: &Catalogue, options: &mut Option<SplashOptions>, report: &mut Vec<String>) {
    for e in out {
        match e {
            Effect::LoadBlueprint(path) => { *bp = stock(path); report.push(format!("loaded {path}")); }
            Effect::ApplyNoteFix(n) => {
                let s = see(bp, cat);
                if let Some(fix) = s.notes.iter().find(|x| note_id(&x.kind) == n).and_then(|x| x.fix.clone()) {
                    *bp = apply_edit(bp, &fix, cat).expect("the tutorial's Fix applies");
                    report.push(format!("the tutorial applied the {n} Fix"));
                }
            }
            Effect::SplashOptions(o) => *options = Some(*o),
            _ => {}
        }
    }
}

/// Play the dock steps of `level` (open to the first launch) with the kits in `order` (`None` = card order).
/// Every gauge and note the steps require is asserted on the way.
fn dock(level: u8, reverse: bool, cat: &Catalogue) -> Docked {
    let script = script_for(level).unwrap_or_else(|| panic!("level {level} has a script"));
    let (mut run, first) = TutorialRun::start(script.clone());
    let mut bp = stock(&script.blueprint);
    let mut options = None;
    let mut report = Vec::new();
    effects(&first, &mut bp, cat, &mut options, &mut report);
    let mut guard = 0;
    while let Some(step) = run.current().cloned() {
        guard += 1;
        assert!(guard < 64, "level {level}: the dock steps loop");
        if matches!(step.id.as_str(), "launch") { break; }
        let out = match step.id.as_str() {
            "open" => run.signal(&Signal::Continue),
            "beam" if level == 0 => {
                let before = see(&bp, cat);
                assert_eq!(before.gauges.float.check, Check::Fail, "level 0 opens with Float at Fail: {}", float_line(&before));
                let (next, beam) = drag_beam_to_pass(&bp, cat);
                bp = next;
                let s = see(&bp, cat);
                report.push(format!("beam handle let go at {beam:.1} m: {}", float_line(&s)));
                eprintln!("level {level}: {}", report.last().unwrap());
                run.signal(&Signal::Gauge { gauge: "float".into(), pass: s.gauges.float.check == Check::Pass })
            }
            "beam" => run.signal(&Signal::Continue),
            "kits" => {
                let mut kits = lesson_kits(&run);
                assert!(!kits.is_empty(), "level {level}: the kit step waits on kits");
                if reverse { kits.reverse(); }
                let mut out = Vec::new();
                for kit in &kits {
                    let Some((next, cell)) = place_on_spot(&bp, &script, kit, cat) else {
                        let s = see(&bp, cat);
                        panic!("level {level}: {kit} has no valid pose near its spot (kits so far: {:?}); {}", bp.kits.iter().map(|k| k.kit.as_str()).collect::<Vec<_>>(), float_line(&s));
                    };
                    bp = next;
                    let s = see(&bp, cat);
                    report.push(format!("{kit} at {},{},{}: {}", cell.x, cell.y, cell.z, float_line(&s)));
                    eprintln!("level {level}{}: {}", if reverse { " reverse" } else { "" }, report.last().unwrap());
                    assert_ne!(s.gauges.float.check, Check::Fail, "level {level}: placing {kit} takes Float to Fail: {}", float_line(&s));
                    out = run.signal(&Signal::KitPlaced { kit: kit.clone() });
                    effects(&out, &mut bp, cat, &mut options, &mut report);
                    if run.current().is_some_and(|c| c.id != "kits") { break; }
                    run.signal(&Signal::OpenNotes { ids: open_ids(&s) });
                }
                let s = see(&bp, cat);
                assert!(s.unconnected.is_empty(), "level {level}: after the kits, consumers off the networks: {:?}", s.unconnected);
                // The out of the last KitPlaced already ran; the Fix step is now showing.
                let _ = out;
                Vec::new()
            }
            "fix_arc" => {
                let s = see(&bp, cat);
                let note = s.notes.iter().find(|n| note_id(&n.kind) == "arc_blocked");
                let Some(note) = note else { panic!("level {level}: the gun arc is not blocked after the kits; notes {:?}", open_ids(&s)) };
                let fix = note.fix.clone().unwrap_or_else(|| panic!("level {level}: the blocked-arc note offers no Fix"));
                let out = if level == 3 {
                    // The demonstration ends and the tutorial applies the Fix (`ApplyNoteFix`).
                    run.signal(&Signal::Continue)
                } else {
                    bp = apply_edit(&bp, &fix, cat).expect("the Fix applies");
                    run.signal(&Signal::NoteFixApplied { note: "arc_blocked".into() })
                };
                effects(&out, &mut bp, cat, &mut options, &mut report);
                let after = see(&bp, cat);
                assert!(!open_ids(&after).contains(&"arc_blocked".to_string()), "level {level}: the Fix leaves the arc blocked");
                assert!(after.unconnected.is_empty(), "level {level}: after the Fix, consumers off the networks: {:?}", after.unconnected);
                assert_ne!(after.gauges.float.check, Check::Fail, "level {level}: the Fix takes Float to Fail: {}", float_line(&after));
                report.push(format!("after the Fix: {}; notes {:?}", float_line(&after), open_ids(&after)));
                Vec::new()
            }
            other => panic!("level {level}: no dock action for step {other}"),
        };
        effects(&out, &mut bp, cat, &mut options, &mut report);
    }
    assert!(run.log.timed_out.is_empty(), "level {level}: dock steps timed out: {:?}", run.log.timed_out);
    let fin = see(&bp, cat);
    assert!(fin.gauges.float.heel_deg.abs() <= MAX_HEEL_DEG, "level {level}: the finished design lists: {}", float_line(&fin));
    assert!(fin.unconnected.is_empty(), "level {level}: the finished design has consumers off the networks: {:?}", fin.unconnected);
    assert_eq!(run.current().map(|s| s.id.as_str()), Some("launch"), "level {level}: the dock steps end at the launch");
    // The launch step set the drone basin on entry.
    Docked { run, bp, report }
}

fn deps(cat: &Arc<Catalogue>) -> SimDeps {
    SimDeps { catalogue: cat.clone(), campaign: Arc::new(content::campaign::load::campaign().clone()), designs: Default::default(), scripts: Box::new(sim_systems::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() }
}

/// One basin step: launch, take the helm, full ahead, steer for the nearest live target, feed the run.
/// Returns a line on how it went.
/// One basin step; returns a line on how it went and whether the lesson's 76 mm gun fired.
fn basin(run: &mut TutorialRun, bp: &Blueprint, target: SplashTarget, seed: u64, cat: &Arc<Catalogue>) -> (String, bool) {
    let d = deps(cat);
    let options = SplashOptions { water: content::weather::SeaState::CALM, target };
    let prepared = sim_systems::editor::splash::prepare(bp, options, seed, &d.campaign, cat);
    let mut sim = sim_systems::editor::splash::launch(&prepared, d, sim_systems::editor::splash::SplashStage::Full).expect("the basin starts");
    let me = sim.ships().of_player(PLAYER).expect("the player has a hull").vessel;
    let step = run.current().map(|s| s.id.clone()).unwrap_or_default();
    let env = |seq: u32, command: Command| CommandEnvelope { player: PLAYER, seq, client_render_tick: 0, rtt_ms: 0, command };
    sim.apply(&[env(1, Command::TakeControl { role: ControlRole::Helm, on: true })]);
    let (mut fired, mut kills, mut steered) = (0u32, 0u32, false);
    // The lesson gun: the 76 mm mount the player dropped (its kit's gun part).
    let lesson_gun: Vec<content::ids::PartSlot> = prepared.design.prefab.parts.iter().filter(|p| cat.parts.get_index(p.def.index()).is_some_and(|(k, _)| k.as_str() == LESSON_GUN)).map(|p| p.slot).collect();
    assert!(!lesson_gun.is_empty(), "the design carries the lesson's {LESSON_GUN}");
    let (mut gun_rounds, mut kill_notes) = (0u32, Vec::new());
    let mut ended = None;
    for t in 0..BASIN_LIMIT_S as u64 * 60 {
        let v = sim.world().vessel(me).expect("the player's vessel");
        let pose = v.kin.pose;
        let nearest = sim.ships().ships.iter().filter(|s| s.vessel != me).filter_map(|s| sim.world().vessel(s.vessel)).filter(|o| o.status == VesselStatus::Active).map(|o| o.kin.pose.pos).min_by(|a, b| (*a - pose.pos).length().total_cmp(&(*b - pose.pos).length()));
        let rudder = nearest.map_or(0.0, |p| {
            let d: DVec3 = p - pose.pos;
            let bearing = libm::atan2(d.x, -d.z);
            let err = (bearing - pose.heading_rad() + core::f64::consts::PI).rem_euclid(core::f64::consts::TAU) - core::f64::consts::PI;
            (err / 0.5).clamp(-1.0, 1.0) as f32
        });
        sim.set_input(PLAYER, PlayerInput { input_tick: t + 1, client_render_tick: t, throttle: 1.0, rudder, ..Default::default() });
        sim.step();
        run.tick(1.0 / 60.0);
        if !steered && rudder.abs() > 0.05 {
            steered = true;
            run.signal(&Signal::HelmInput);
        }
        for e in sim.drain_events() {
            if let sim_systems::protocol::event::SimEvent::Phys(sim_core::PhysEvent::MountFired { vessel, slot, rounds }) = &e.event {
                if *vessel == me && lesson_gun.contains(slot) { gun_rounds += *rounds as u32; }
            }
            if let Some(s) = signal_from_event(&e.event, me) {
                match s {
                    Signal::PlayerGunsFired => fired += 1,
                    Signal::EnemyDestroyed { .. } => {
                        kills += 1;
                        let what = match &e.event {
                            sim_systems::protocol::event::SimEvent::Phys(sim_core::PhysEvent::StatusChanged { .. }) => "a hull disabled or struck",
                            _ => "a hull lost",
                        };
                        kill_notes.push(format!("{what} at {:.0} s", (t + 1) as f64 / 60.0));
                    }
                    _ => {}
                }
                run.signal(&s);
            }
        }
        if (t + 1) % 60 == 0 { run.signal(&Signal::Elapsed { seconds: ((t + 1) / 60) as u32 }); }
        if run.current().is_none_or(|c| c.id != step) { ended = Some((t + 1) as f64 / 60.0); break; }
    }
    let status = sim.world().vessel(me).map(|v| format!("{:?}", v.status)).unwrap_or_default();
    match ended {
        Some(s) => (format!("{step} seed {seed}: ended at {s:.0} s, {fired} firing events ({gun_rounds} rounds of 76 mm), {kills} kills ({}), player {status}", kill_notes.join(", ")), gun_rounds > 0),
        None => (format!("{step} seed {seed}: still running at {BASIN_LIMIT_S} s, {fired} firing events ({gun_rounds} rounds of 76 mm), {kills} kills, player {status}"), gun_rounds > 0),
    }
}

/// The basin half: drones, the patrol boat, the slider and the second splash, to the finish.
fn basins(mut docked: Docked, seed: u64, cat: &Arc<Catalogue>) -> (TutorialRun, Vec<String>, bool) {
    let level = docked.run.script.fallback_level;
    let mut lines = Vec::new();
    let mut gun = false;
    let mut guard = 0;
    while let Some(step) = docked.run.current().cloned() {
        guard += 1;
        assert!(guard < 64, "level {level}: the basin steps loop");
        match step.id.as_str() {
            "launch" | "patrol_launch" | "splash_again" => { docked.run.signal(&Signal::SplashStarted); }
            "drones" | "patrol_boat" => {
                let target = if step.id == "drones" { SplashTarget::Drones } else { SplashTarget::PatrolBoat };
                let (line, fired) = basin(&mut docked.run, &docked.bp, target, seed, cat);
                lines.push(line);
                gun |= fired;
            }
            "aggression" => { docked.run.signal(&Signal::SliderMoved { key: "gunnery.aggression".into() }); }
            "done" => { docked.run.signal(&Signal::Continue); }
            other => panic!("level {level}: no basin action for step {other}"),
        }
    }
    (docked.run, lines, gun)
}

fn play(level: u8) {
    let cat = cat();
    // Both kit orders build a design that passes every dock check (asserted inside `dock`).
    let docked = dock(level, false, &cat);
    let reverse = dock(level, true, &cat);
    eprintln!("== level {level}\n   {}\n   reverse order: {}", docked.report.join("\n   "), reverse.report.join("; "));
    let mut failures = Vec::new();
    for seed in SEEDS {
        let one = Docked { run: docked.run.clone(), bp: docked.bp.clone(), report: Vec::new() };
        let (run, lines, gun) = basins(one, seed, &cat);
        if !gun { failures.push(format!("seed {seed}: the lesson's 76 mm gun never fired; {}", lines.join("; "))); }
        eprintln!("   {}", lines.join("\n   "));
        if !run.finished || !run.log.timed_out.is_empty() || run.log.first_fight_s.is_none() || (level < 4 && run.log.first_kill_s.is_none()) {
            failures.push(format!("seed {seed}: finished {}, timed out {:?}, first fight {:?}, first kill {:?}; {}", run.finished, run.log.timed_out, run.log.first_fight_s, run.log.first_kill_s, lines.join("; ")));
        }
    }
    assert!(failures.is_empty(), "level {level}:\n{}", failures.join("\n"));
}

#[test]
fn level_0_full_tutorial_plays_to_the_finish() { play(0); }
#[test]
fn level_1_float_at_pass_plays_to_the_finish() { play(1); }
#[test]
fn level_2_engine_and_fuel_fitted_plays_to_the_finish() { play(2); }
#[test]
fn level_3_fix_demonstrated_plays_to_the_finish() { play(3); }
#[test]
fn level_4_one_drag_plays_to_the_finish() { play(4); }

/// Diagnostics: the stock hulls as the dock reads them.
#[test]
#[ignore]
fn report() {
    let cat = cat();
    for path in ["starters/fac22.bp.ron", "tutorial/fac22_tutorial.bp.ron", "tutorial/fac22_tutorial_float.bp.ron", "tutorial/fac22_tutorial_drive.bp.ron", "tutorial/fac22_tutorial_nogun.bp.ron"] {
        let bp = stock(path);
        let s = see(&bp, &cat);
        eprintln!("== {path}: {}\n   size {:?} mirror_sum {:?}", float_line(&s), CellGrid::from_rows(&bp.cells).size, mirror_sum(&bp));
        for k in &bp.kits { eprintln!("   kit {} at {},{},{} mirrored {}", k.kit, k.pos.x, k.pos.y, k.pos.z, k.mirrored); }
        for n in &s.notes { eprintln!("   note {} {} part {:?} fix {}", note_id(&n.kind), n.text.key, n.part, n.fix.is_some()); }
        eprintln!("   unconnected {:?}", s.unconnected);
    }
}

/// Diagnostics: the level 0 hull's beam drag swept from its first Pass, with the four kits on their spots.
#[test]
#[ignore]
fn sweep_level_0() {
    let cat = cat();
    let script = script_for(0).unwrap();
    let bp0 = stock(&script.blueprint);
    let (station, from) = widest_station(&bp0);
    let kits: Vec<KitId> = ["kit.gun", "kit.radar_mast", "kit.engine", "kit.fuel_tank"].iter().map(|k| KitId::new(*k)).collect();
    let mut beam = from;
    while beam < from + 4.0 {
        beam += 0.2;
        let Some(bp) = set_beam(&bp0, station, beam, &cat) else { break };
        let s = see(&bp, &cat);
        let mut line = format!("beam {beam:.1}: bare {}", float_line(&s));
        let mut cur = bp.clone();
        for k in &kits {
            match place_on_spot(&cur, &script, k, &cat) {
                Some((n, c)) => { cur = n; let s = see(&cur, &cat); line += &format!("\n      {k} {},{},{}: {}", c.x, c.y, c.z, float_line(&s)); }
                None => { line += &format!("\n      {k}: no pose"); }
            }
        }
        eprintln!("{line}");
    }
}

/// Diagnostics: the level 1 hull scaled uniformly, bare and with the four kits.
#[test]
#[ignore]
fn sweep_uniform() {
    let cat = cat();
    let script = script_for(1).unwrap();
    let bp1 = stock(&script.blueprint);
    let content::hull::HullSource::Lofted(sh) = &bp1.hull.source else { panic!() };
    let beams: Vec<f64> = sh.stations.iter().map(|s| s.beam_m).collect();
    let kits: Vec<KitId> = ["kit.gun", "kit.radar_mast", "kit.engine", "kit.fuel_tank"].iter().map(|k| KitId::new(*k)).collect();
    for s in [0.70, 0.74, 0.78, 0.82, 0.86, 0.88, 0.92, 0.96, 1.0] {
        let mut bp = bp1.clone();
        for (i, b) in beams.iter().enumerate() {
            if *b <= 0.61 { continue; }
            if let Some(n) = set_beam(&bp, i, b * s / 0.88, &cat) { bp = n; }
        }
        let sb = see(&bp, &cat);
        let mut line = format!("scale {s:.2}: bare {}", float_line(&sb));
        let mut cur = bp.clone();
        for k in &kits {
            match place_on_spot(&cur, &script, k, &cat) {
                Some((n, _)) => cur = n,
                None => { line += &format!(" | {k}: no pose"); }
            }
        }
        line += &format!("\n      kitted {}", float_line(&see(&cur, &cat)));
        eprintln!("{line}");
    }
}
