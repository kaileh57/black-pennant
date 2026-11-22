//! Seakeeping probe: stock hulls at full power in waves.
use super::*;
use crate::parts::VesselControls;
use crate::vessel::{build_prefab, Fidelity, VesselPrefab, VesselSpawn, VesselStatus};
use crate::world::{World, WorldConfig};
use content::campaign::MapDef;
use content::catalogue::Catalogue;
use content::ids::SideId;
use content::weather::{SeaState, WeatherState};
use std::sync::Arc;

fn env<T: std::str::FromStr>(k: &str, d: T) -> T { std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d) }

fn hulls(cat: &Catalogue, filter: &str) -> Vec<(&'static str, Arc<VesselPrefab>)> {
    let mut out: Vec<_> = content::data::stock_blueprint_files()
        .filter(|(p, _)| (p.contains("starters/") || p.contains("combine/act1/")) && p.ends_with(".bp.ron") && p.contains(filter))
        .map(|(p, text)| {
            let mut prefab = build_prefab(&content::blueprint::io::load_blueprint(text, p).expect("loads").blueprint, cat);
            // BP_DKG lowers the dry centre of mass by that many metres (a what-if for ballast).
            prefab.dry_mass.com.y -= env("BP_DKG", 0.0f64);
            (p, Arc::new(prefab))
        })
        .collect();
    out.sort_by_key(|(p, _)| *p);
    out
}

struct Run { peak_deg: f64, lost: bool, at_s: f64, speed: f64 }

fn run(cat: &Arc<Catalogue>, prefab: &Arc<VesselPrefab>, sea: u8, rel_deg: f64, secs: f64, turn_at: Option<f64>, throttle: f64, trace: bool) -> Run {
    // Hull heads north (heading 0); the wind (and the sea) blows toward `rel_deg` off the bow, so 180 is a head sea.
    let wind = rel_deg.to_radians();
    let mut w = World::new(WorldConfig { seed: env("BP_SEED", 7u64), weather: WeatherState { sea_state: SeaState(sea), wind_dir_rad: wind, ..WeatherState::default() }, map: MapDef { size_m: [40_000.0, 40_000.0], depth_m: 500.0, ..MapDef::default() }, catalogue: cat.clone() });
    let (top, _) = predict_speed_turn(prefab, cat, &cat.tuning.buoyancy);
    let id = w.spawn_vessel(&VesselSpawn { prefab: prefab.clone(), side: SideId(0), player: None, class: None, pose: crate::math::Pose::from_heading(DVec3::ZERO, 0.0), speed_mps: top * throttle.abs(), damage: None, fuel_fraction: 1.0, ammo_fraction: 1.0, fidelity: Fidelity::Full, submersible: false });
    w.set_controls(id, VesselControls { throttle, ..Default::default() });
    let ticks = (secs * 60.0) as u64;
    let mut peak = 0.0f64;
    let mut speed = 0.0;
    let mut kicks: Vec<f64> = Vec::new();
    for k in 0..ticks {
        if let Some(kick) = std::env::var("BP_KICK").ok().and_then(|v| v.parse::<f64>().ok()) {
            if k == 1200 {
                let v = w.vessel(id).expect("hull");
                let mut kin = v.kin;
                let bow = kin.pose.rot * DVec3::X;
                kin.ang_vel += DVec3::new(bow.x, 0.0, bow.z).normalize() * kick;
                w.teleport(id, &kin);
            }
            if k > 1200 && k % 6 == 0 { let v = w.vessel(id).expect("hull"); kicks.push(v.heel_trim().0.to_degrees()); }
        }
        if turn_at.is_some_and(|t| k == (t * 60.0) as u64) { w.set_controls(id, VesselControls { throttle, rudder: 1.0, ..Default::default() }); }
        w.step();
        let Some(v) = w.vessel(id) else { return Run { peak_deg: peak, lost: true, at_s: k as f64 / 60.0, speed } };
        let (heel, trim) = v.heel_trim();
        peak = peak.max(heel.to_degrees().abs());
        speed = v.forward_speed();
        if trace && k % 15 == 0 {
            let ang = v.kin.pose.rot.inverse() * v.kin.ang_vel;
            let m = moments(v, w.waves(), w.time_s(), cat);
            let mut b = HydroForces::default();
            buoyancy(v, w.waves(), w.time_s(), &cat.tuning.buoyancy, &mut b);
            let mut d = HydroForces { submerged: b.submerged, ..Default::default() };
            drag(v, w.waves(), w.time_s(), &cat.tuning.buoyancy, &mut d);
            let hv = d.forces.last().map_or(0.0, |f| f.force.y / 1000.0);
            let bz = b.forces.first().map_or(0.0, |f| f.force.y / 1000.0);
            let wt = v.mass.mass_kg * crate::physics::GRAVITY / 1000.0;
            let at = d.forces.last().map_or(DVec3::ZERO, |f| f.at);
            let com = v.kin.pose.to_world(v.mass.com);
            let pv = v.kin.lin_vel + v.kin.ang_vel.cross(at - com);
            let wv = w.waves().sample(at.x, at.z, w.time_s()).velocity;
            eprintln!("  t {:6.2} buoy {:6.1}/{:5.1} kN heave {:6.1} kN, vy com {:6.3} pt {:6.3} water {:6.3} sub {:.3} off {:5.2}", k as f64 / 60.0, bz, wt, hv, v.kin.lin_vel.y, pv.y, wv.y, b.submerged, (at - com).x * 0.0 + DVec3::new(at.x - com.x, 0.0, at.z - com.z).length());
            eprintln!("  t {:6.2} heel {:7.2} trim {:6.2} p {:6.3} q {:6.3} r {:6.3} u {:5.2} | kNm buoy {:7.1} surge {:6.1} lat {:6.1} heave {:6.1} damp {:6.1} thrust {:6.1} foil {:6.1}", k as f64 / 60.0, heel.to_degrees(), trim.to_degrees(), ang.x, ang.y, ang.z, speed, m[0], m[1], m[2], m[3], m[4], m[5], m[6]);
        }
        if matches!(v.status, VesselStatus::Lost(_)) { return Run { peak_deg: peak, lost: true, at_s: k as f64 / 60.0, speed } }
    }
    if !kicks.is_empty() {
        // Successive extremes of heel after the kick.
        let mut ext = Vec::new();
        for i in 1..kicks.len() - 1 { if (kicks[i] - kicks[i - 1]) * (kicks[i + 1] - kicks[i]) < 0.0 { ext.push(format!("{:.1}@{:.1}", kicks[i], 20.0 + i as f64 * 0.1)); } }
        eprintln!("    kick extremes: {}", ext.iter().take(14).cloned().collect::<Vec<_>>().join(" "));
    }
    Run { peak_deg: peak, lost: false, at_s: secs, speed }
}

/// Roll moments in kN m about the centre of mass, positive heeling to starboard, along the level bow axis:
/// buoyancy, surge drag, lateral strips, heave damping, roll damping torque, thrust, rudders.
fn moments(v: &crate::vessel::Vessel, waves: &crate::waves::WaveSet, t: f64, cat: &Catalogue) -> [f64; 7] {
    let pose = v.kin.pose;
    let com = pose.to_world(v.mass.com);
    let bow = pose.rot * DVec3::X;
    let ahead = DVec3::new(bow.x, 0.0, bow.z).normalize_or(DVec3::X);
    let arm = |f: &PointForce| (f.at - com).cross(f.force).dot(ahead) / 1000.0;
    let mut out = [0.0; 7];
    let mut b = HydroForces::default();
    buoyancy(v, waves, t, &cat.tuning.buoyancy, &mut b);
    out[0] = b.forces.iter().map(arm).sum();
    let mut d = HydroForces { submerged: b.submerged, ..Default::default() };
    drag(v, waves, t, &cat.tuning.buoyancy, &mut d);
    let n = d.forces.len();
    if n >= 10 {
        out[1] = arm(&d.forces[0]);
        out[2] = d.forces[1..n - 1].iter().map(arm).sum();
        out[3] = arm(&d.forces[n - 1]);
    }
    out[4] = d.torque.dot(ahead) / 1000.0;
    let mut p = HydroForces::default();
    propulsion(v, waves, t, &mut p);
    let dev = forces::Devices::from_state(&v.parts, &v.grid);
    for f in &p.forces {
        if dev.thrusters.iter().any(|th| (pose.to_world(th.pos) - f.at).length() < 1e-9) { out[5] += arm(f); } else { out[6] += arm(f); }
    }
    // A moment along +ahead turns the up axis toward starboard: it heels the hull to starboard (positive heel).
    out
}

/// `cargo test --release -p sim-core --lib seakeeping_probe -- --ignored --nocapture`
/// BP_HULL (path substring), BP_SEA (list), BP_REL (list of wind directions off the bow, degrees),
/// BP_SECS, BP_TURN (seconds at which the helm goes hard over), BP_THROTTLE, BP_TRACE.
#[test]
#[ignore = "probe"]
fn seakeeping_probe() {
    let cat = Arc::new(content::catalogue::catalogue().clone());
    let filter = std::env::var("BP_HULL").unwrap_or_default();
    let seas: Vec<u8> = std::env::var("BP_SEA").unwrap_or_else(|_| "3".into()).split(',').filter_map(|s| s.parse().ok()).collect();
    let rels: Vec<f64> = std::env::var("BP_REL").unwrap_or_else(|_| "0,60,120,180,240,300".into()).split(',').filter_map(|s| s.parse().ok()).collect();
    let secs = env("BP_SECS", 60.0f64);
    let turn = std::env::var("BP_TURN").ok().and_then(|v| v.parse().ok());
    let throttle = env("BP_THROTTLE", 1.0f64);
    let trace = std::env::var("BP_TRACE").is_ok();
    for (path, prefab) in hulls(&cat, &filter) {
        let mass = crate::mass::with_fluids(&prefab.dry_mass, &prefab.parts, &prefab.compartments, &cat);
        let eq = equilibrium(&prefab.blocks, &mass, &[], &prefab.compartments, &cat.tuning.buoyancy);
        let (top, _) = predict_speed_turn(&prefab, &cat, &cat.tuning.buoyancy);
        eprintln!("{path}: {:.0} t, L {:.1} B {:.1} H {:.1}, GM {:.2}, heel {:.1} trim {:.1} draft {:.2}, top {:.1} m/s", mass.mass_kg / 1000.0, prefab.dims[0], prefab.dims[2], prefab.dims[1], eq.gm_m, eq.heel_rad.to_degrees(), eq.trim_rad.to_degrees(), eq.draft_m, top);
        if std::env::var("BP_DEV").is_ok() {
            let dev = forces::Devices::from_state(&prefab.parts, &prefab.grid);
            eprintln!("  com {:?} wl {:.2} form lo {:?} hi {:?}", mass.com, prefab.waterline_y, prefab.blocks.form.lo, prefab.blocks.form.hi);
            let full = forces::Devices::from_defs(&prefab.parts, &prefab.grid, &cat, 0.0);
            for t in &full.thrusters { eprintln!("  thruster {:?} dir {:?} {:.0} N {:.0} kW disc {:.2}", t.pos, t.dir, t.static_n, t.shaft_kw, t.disc_m2); }
            for f in &full.foils { eprintln!("  foil {:?} n {:?} area {:.2} half {:.2}", f.pos, f.normal, f.area_m2, f.half_m); }
            let _ = dev;
        }
        if std::env::var("BP_LAYERS").is_ok() {
            // Displacing volume per 0.5 m layer of the hull, in cells.
            let mut layers: std::collections::BTreeMap<i16, usize> = Default::default();
            let ex = build_blocks(&prefab.grid, &prefab.compartments, 1);
            for b in &ex.blocks { let y = (b.centre.y / 0.5).floor() as i16; *layers.entry(y).or_default() += (b.volume_m3 / 0.125).round() as usize; }
            eprintln!("  layer y(m): displacing cells; waterline {:.2}", prefab.waterline_y);
            for (y, d) in &layers { eprintln!("    {:5.2}: {d:5}", *y as f64 * 0.5); }
        }
        if std::env::var("BP_GZ").is_ok() {
            let gz = gz_curve(&prefab.blocks, &mass, 10.0, &cat.tuning.buoyancy);
            eprintln!("  GZ {}", gz.iter().map(|(a, g)| format!("{a:.0}:{g:.2}")).collect::<Vec<_>>().join(" "));
            continue;
        }
        for &sea in &seas {
            let mut line = format!("  sea {sea}:");
            for &rel in &rels {
                let r = run(&cat, &prefab, sea, rel, secs, turn, throttle, trace);
                line += &format!(" {rel:>3.0}:{:5.1}{}", r.peak_deg, if r.lost { format!(" LOST@{:.0}s", r.at_s) } else { format!(" ({:.1})", r.speed) });
            }
            eprintln!("{line}");
        }
    }
}
