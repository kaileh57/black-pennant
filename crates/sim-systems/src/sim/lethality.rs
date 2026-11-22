//! Lethality measurement for the weapon gates of DESIGN.md L606-L612: rounds of one band fired from a set
//! range at a stock hull crewed as a Combine hull (a class-less Combine captain over the stock officers: Damage
//! Control fights fires, patches and repairs, and the captain strikes under the Combine surrender rule, L537),
//! counting the hits until the hull is Disabled (propulsion lost or colours struck) or lost. The rounds come from no mount (`World::fire_shell`), laid as the
//! stock solver lays (the target's centre at the waterline, `guidance::gun::predicted_pk`) with the gun's
//! dispersion and the officer laying error in quadrature per axis, and lead the target's motion.
//! `cargo test --release -p sim-systems --lib lethality_gate -- --ignored --nocapture` with `LG_GATE`
//! (`76`, `30` or `127`: L610, L611, L612), `LG_RUNS` (200), `LG_THREADS` (4), `LG_SEA` (0), `LG_GAP` (ticks
//! between rounds; the gun's rate of fire), `LG_KB` (the L610 fallback's what-if: K and B scaled by this),
//! `LG_UNDERWAY` (the hull steams at full speed), `LG_MAX` (hits before a run counts as afloat, 200),
//! `LG_STRIKE` (the surrender rule's cells-lost fraction instead of the campaign's; 0 turns the rule off),
//! `LG_VERBOSE`, `LG_CURVE` (print each run's cells-lost fraction after every hit, for calibration).
use super::*;
use crate::protocol::event::SimEvent;
use content::part::weapons::CalibreBand;
use content::weather::SeaState;
use sim_core::PhysEvent;
use sim_core::vessel::VesselStatus;

fn env<T: core::str::FromStr>(k: &str, d: T) -> T { std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d) }

/// One L606 gate row: the target, the band, the gun's calibre, muzzle velocity and dispersion, the range.
#[derive(Clone, Debug)]
pub(crate) struct GateRow { pub target: &'static str, pub band: CalibreBand, pub calibre_mm: u16, pub muzzle_mps: f64, pub dispersion_mrad: f64, pub range_m: f64, pub rof_rpm: f64 }

pub(crate) fn gate_row(name: &str) -> GateRow {
    match name {
        "30" => GateRow { target: "starters/fac22.bp.ron", band: CalibreBand::Auto, calibre_mm: 30, muzzle_mps: 1_080.0, dispersion_mrad: 1.5, range_m: 1_500.0, rof_rpm: 200.0 },
        "127" => GateRow { target: "starters/destroyer90.bp.ron", band: CalibreBand::Large, calibre_mm: 127, muzzle_mps: 808.0, dispersion_mrad: 0.55, range_m: 10_000.0, rof_rpm: 40.0 },
        _ => GateRow { target: "starters/fac22.bp.ron", band: CalibreBand::Medium, calibre_mm: 76, muzzle_mps: 925.0, dispersion_mrad: 0.9, range_m: 4_000.0, rof_rpm: 100.0 },
    }
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 { self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); ((self.0 >> 11) as f64) / ((1u64 << 53) as f64) }
    fn gauss(&mut self) -> f64 { let (a, b) = (self.next().max(1e-12), self.next()); libm::sqrt(-2.0 * libm::log(a)) * libm::cos(core::f64::consts::TAU * b) }
}

/// Height and time of flight of a round fired at `elev` when it has travelled `range` over the ground.
fn flight(origin: DVec3, dir_h: DVec3, elev: f64, v0: f64, k: f64, range: f64) -> Option<(f64, f64)> {
    let (mut p, mut v) = (origin, (dir_h * libm::cos(elev) + DVec3::Y * libm::sin(elev)) * v0);
    for i in 0..60 * 120 {
        let (np, nv) = sim_core::projectiles::ballistic_step(p, v, k, sim_core::time::DT);
        if ((np - origin) * DVec3::new(1.0, 0.0, 1.0)).length() >= range { return Some((np.y, (i + 1) as f64 * sim_core::time::DT)); }
        p = np;
        v = nv;
    }
    None
}

/// One seeded run: rounds every `gap` ticks (the gun's rate of fire) until the hull is out or `max_hits` land.
/// Returns (hits, rounds, how it ended, seconds).
pub(crate) fn run_gate(row: &GateRow, seed: u64, sea: u8, max_hits: u32, verbose: bool, deps: SimDeps) -> (u32, u32, String, f64) {
    let bp = content::campaign::load::stock_blueprint(row.target).expect("target hull");
    let mut rng = Lcg(0x5DEE_CE66 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let mut config = crate::editor::trials::trial_base(&bp, SeaState(sea), seed, &deps.campaign, 60 * 60 * 60 * 4, 0.0);
    config.vessels[0].heading_rad = rng.next() * core::f64::consts::TAU;
    // The gate's target is crewed as a Combine hull (the ruling's reading of L606: the rule a player meets),
    // so its captain strikes under the surrender rule. The player slot watches with no hull.
    let combine = config.sides.iter().find(|d| d.faction == crate::protocol::config::Faction::Combine).map(|d| d.id).expect("a Combine side");
    config.vessels[0].side = combine;
    config.vessels[0].role = crate::protocol::VesselRole::Combatant;
    config.vessels[0].declared = None;
    config.players[0].vessel = None;
    let underway = std::env::var("LG_UNDERWAY").is_ok();
    if underway { config.vessels[0].speed_kn = 40.0; }
    let band = deps.catalogue.bands.iter().find(|b| b.band == row.band).copied().unwrap_or_else(|| content::part::weapons::BandDef::spec_default(row.band));
    let Ok(mut sim) = Sim::new(config, deps) else { return (0, 0, "no start".into(), 0.0) };
    let Some(own) = sim.world().vessels().map(|v| v.1.id).next() else { return (0, 0, "no hull".into(), 0.0) };
    let mut curve: Vec<String> = Vec::new();
    for _ in 0..600 { sim.step(); sim.drain_events(); }
    let kw0 = sim.world().vessel(own).map_or(0.0, |v| v.propulsion_kw());
    let mut dead_since: Option<(u64, u32)> = None;
    let mut dead_at: Option<u32> = None;
    let bearing = rng.next() * core::f64::consts::TAU;
    let sigma_rad = libm::sqrt((row.dispersion_mrad * 1e-3) * (row.dispersion_mrad * 1e-3) + crate::guidance::gun::PLATFORM_SIGMA_B4_RAD * crate::guidance::gun::PLATFORM_SIGMA_B4_RAD);
    // LG_GAP: ticks between rounds instead of the gun's rate of fire (hits against time under fire).
    let gap: u64 = env("LG_GAP", libm::round(3_600.0 / row.rof_rpm).max(1.0) as u64).max(1);
    let (mut hits, mut rounds) = (0u32, 0u32);
    let mut end = String::from("afloat");
    let t0 = sim.tick().0;
    let curve_on = std::env::var("LG_CURVE").is_ok();
    'run: while hits < max_hits && rounds < max_hits * 50 {
        let Some(v) = sim.world().vessel(own) else { end = "gone".into(); break };
        if v.status != VesselStatus::Active { end = format!("{:?}", v.status); break; }
        let to_shooter = DVec3::new(libm::cos(bearing), 0.0, libm::sin(bearing));
        let lateral = DVec3::new(-to_shooter.z, 0.0, to_shooter.x);
        let centre = (v.bounds.min + v.bounds.max) * 0.5;
        let water = sim.world().water_height(centre.x, centre.z);
        let sigma = sigma_rad * row.range_m;
        let lay = DVec3::new(centre.x, water, centre.z);
        let origin = lay + to_shooter * row.range_m + DVec3::Y * (5.0 - lay.y);
        let dir_h = -to_shooter;
        let tof = flight(origin, dir_h, 0.01, row.muzzle_mps, band.drag_k, row.range_m).map_or(0.0, |f| f.1);
        let lead = v.kin.lin_vel * DVec3::new(1.0, 0.0, 1.0) * tof;
        let aim = lay + lead + lateral * (rng.gauss() * sigma) + DVec3::Y * (rng.gauss() * sigma);
        let range = ((aim - origin) * DVec3::new(1.0, 0.0, 1.0)).length();
        let dir_h = ((aim - origin) * DVec3::new(1.0, 0.0, 1.0)).normalize();
        let (mut lo, mut hi) = (-0.05f64, 0.3f64);
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            match flight(origin, dir_h, mid, row.muzzle_mps, band.drag_k, range) { Some((y, _)) if y > aim.y => hi = mid, _ => lo = mid }
        }
        let elev = 0.5 * (lo + hi);
        let vel = (dir_h * libm::cos(elev) + DVec3::Y * libm::sin(elev)) * row.muzzle_mps;
        if sim.world.fire_shell(origin, vel, row.calibre_mm, row.band, EntityId::NONE).is_none() { break; }
        rounds += 1;
        for _ in 0..gap {
            sim.set_host_load(crate::lod::HostLoad::IDLE);
            sim.step();
            if kw0 > 1.0 && dead_at.is_none() {
                let kw = sim.world().vessel(own).map_or(0.0, |v| v.propulsion_kw());
                if kw < 0.05 * kw0 { let d = *dead_since.get_or_insert((sim.tick().0, hits)); if sim.tick().0 - d.0 >= 600 { dead_at = Some(d.1); } } else { dead_since = None; }
            }
            if verbose && (sim.tick().0 - t0) % 300 == 0 {
                if let Some(v) = sim.world().vessel(own) {
                    let eng: Vec<String> = v.parts.iter().filter(|p| matches!(p.state, sim_core::parts::PartState::Engine { .. } | sim_core::parts::PartState::Propulsor { .. } | sim_core::parts::PartState::Magazine { .. })).map(|p| format!("{}:{:.2}{:?}/c{:?}", p.slot.0, p.health, p.level, p.compartment.map(|c| c.0))).collect();
                    let fires: Vec<String> = v.compartments.compartments.iter().filter(|c| c.fire > 0.0).map(|c| format!("c{}:{:.0}", c.id.0, c.fire)).collect();
                    let water: f64 = v.compartments.compartments.iter().map(|c| c.water_m3).sum();
                    let outk: Vec<String> = v.parts.iter().filter_map(|p| match p.state { sim_core::parts::PartState::Engine { output_kw, .. } => Some(format!("e{:.0}", output_kw)), sim_core::parts::PartState::Propulsor { shaft_kw, .. } => Some(format!("p{:.0}", shaft_kw)), _ => None }).collect();
                    eprintln!("  seed {seed} t={:.0}s hits {hits} parts {eng:?} kw {outk:?} speed {:.1} fires {fires:?} water {water:.1} teams {:?} lost {:.3}", (sim.tick().0 - t0) as f64 / 60.0, v.forward_speed() / sim_core::time::KN, v.controls.teams, v.cells_lost_fraction());
                }
            }
            let mut seen = std::collections::BTreeSet::new();
            for e in sim.drain_events() {
                let SimEvent::Phys(p) = &e.event else { continue };
                match p {
                    PhysEvent::ShellImpact { projectile, vessel, outcome, .. } if *vessel == own && seen.insert(*projectile) => {
                        hits += 1;
                        if curve_on { curve.push(format!("{}:{:.4}", sim.tick().0 - t0, sim.world().vessel(own).map_or(1.0, |v| v.cells_lost_fraction()))); }
                        if verbose { eprintln!("  seed {seed} t={:.0}s hit {hits} {outcome:?}", (sim.tick().0 - t0) as f64 / 60.0); }
                    }
                    PhysEvent::FireStarted { vessel, compartment, spread } if *vessel == own && verbose => eprintln!("  seed {seed} fire in {compartment:?} spread {spread}"),
                    PhysEvent::StatusChanged { vessel, status } if *vessel == own && *status != VesselStatus::Active => {
                        let struck = sim.world().vessel(own).is_some_and(|v| v.struck);
                        end = if struck { "Struck".into() } else { format!("{status:?}") };
                        break 'run;
                    }
                    _ => {}
                }
            }
        }
    }
    if let Some(d) = dead_at { end = format!("{end} (propulsion dead from hit {d})"); }
    if curve_on { eprintln!("LGCURVE {seed} {} {}", end.split(' ').next().unwrap_or(""), curve.join(" ")); }
    (hits, rounds, end, (sim.tick().0 - t0) as f64 / 60.0)
}

/// The L610-L612 bands asserted in CI on a few runs: each gate's median hits to disable or lose sits in its band
/// widened by a quarter at each end (200-run medians come from `lethality_gate`). 9 seeds per gate.
#[test]
fn lethality_bands_hold_on_a_short_run() {
    let catalogue = Arc::new(Catalogue::load_embedded().expect("catalogue"));
    let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign"));
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    for (gate, lo, hi) in [("76", 8.0, 20.0), ("30", 40.0, 120.0), ("127", 25.0, 60.0)] {
        let row = gate_row(gate);
        let mut hits: Vec<u32> = (0..9u64).map(|seed| {
            let deps = SimDeps { catalogue: catalogue.clone(), campaign: campaign.clone(), designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() };
            let (h, _, end, _) = pool.install(|| run_gate(&row, 1_000 + seed, 0, 200, false, deps));
            if end == "afloat" { u32::MAX } else { h }
        }).collect();
        hits.sort_unstable();
        let med = hits[hits.len() / 2] as f64;
        assert!(med >= 0.75 * lo && med <= 1.25 * hi, "{gate} mm gate: median {med} hits outside {lo}-{hi} widened by 25% ({hits:?})");
    }
}

#[test]
#[ignore = "weapon gate measurement"]
fn lethality_gate() {
    let row = gate_row(&std::env::var("LG_GATE").unwrap_or_else(|_| "76".into()));
    let runs: u64 = env("LG_RUNS", 200);
    let threads: usize = env("LG_THREADS", 4);
    let sea: u8 = env("LG_SEA", 0);
    let max_hits: u32 = env("LG_MAX", 200);
    let verbose = std::env::var("LG_VERBOSE").is_ok();
    let mut cat = Catalogue::load_embedded().expect("catalogue");
    // LG_KB: the L610 fallback's what-if, the band's K and B scaled linearly (rounded, at least 1).
    if let Ok(k) = std::env::var("LG_KB") { let f: f64 = k.parse().unwrap_or(1.0); for b in cat.bands.iter_mut().filter(|b| b.band == row.band) { b.path_k = libm::round(b.path_k as f64 * f).clamp(1.0, 255.0) as u8; b.he_b = libm::round(b.he_b as f64 * f).clamp(1.0, 255.0) as u8; eprintln!("K {} B {}", b.path_k, b.he_b); } }
    let catalogue = Arc::new(cat);
    let mut campaign = content::campaign::load::load_embedded(&catalogue).expect("campaign");
    if let Ok(f) = std::env::var("LG_STRIKE") { campaign.rules.strike_cells_lost = f.parse().unwrap_or(0.0); }
    eprintln!("strike_cells_lost {}", campaign.rules.strike_cells_lost);
    let campaign = Arc::new(campaign);
    let next = std::sync::atomic::AtomicU64::new(0);
    let out: std::sync::Mutex<Vec<(u64, u32, u32, String, f64)>> = std::sync::Mutex::new(Vec::new());
    let t0 = std::time::Instant::now();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
                loop {
                    let seed = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if seed >= runs { break; }
                    let deps = SimDeps { catalogue: catalogue.clone(), campaign: campaign.clone(), designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() };
                    let (h, r, end, secs) = pool.install(|| run_gate(&row, seed, sea, max_hits, verbose, deps));
                    eprintln!("LG {} seed {seed:>3}: {h:>3} hits of {r:>4} rounds, {end} at {secs:.0} s", row.calibre_mm);
                    out.lock().unwrap().push((seed, h, r, end, secs));
                }
            });
        }
    });
    let out = out.into_inner().unwrap();
    let mut h: Vec<u32> = out.iter().map(|o| if o.3 == "afloat" { u32::MAX } else { o.1 }).collect();
    h.sort_unstable();
    let med = h[h.len() / 2];
    let lost = out.iter().filter(|o| o.3.starts_with("Lost")).count();
    let disabled = out.iter().filter(|o| o.3.starts_with("Disabled") || o.3.starts_with("Struck")).count();
    let struck = out.iter().filter(|o| o.3.starts_with("Struck")).count();
    eprintln!("LGSTRUCK {struck}");
    eprintln!("LGSUM {} mm against {} at {} m, sea {sea}: median hits to disable or lose {} over {} runs ({disabled} disabled, {lost} lost, {} afloat after {max_hits} hits), {:.0} s",
        row.calibre_mm, row.target, row.range_m, if med == u32::MAX { "never".into() } else { med.to_string() }, out.len(), out.iter().filter(|o| o.3 == "afloat").count(), t0.elapsed().as_secs_f64());
}
