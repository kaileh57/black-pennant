//! Unit `data-act3`: the Act 3 submarine classes work in the simulation, not only in the validator (DESIGN.md
//! L451, L618). Each stock boat is spawned `submersible` in deep water with an ordered keel depth, steamed for a
//! while, and must go fully under, hold that depth, stay level and active, and fire a torpedo from its tubes.
use content::ids::ClassId;
use content::part::{LauncherKind, PartFunction};
use content::weather::{SeaState, WeatherState};
use glam::DVec3;
use sim_core::parts::{LaunchAim, PartControl, PartOrder, VesselControls};
use sim_core::vessel::{build_prefab, Fidelity, VesselSpawn, VesselStatus};
use sim_core::world::{World, WorldConfig};
use std::sync::Arc;

const ORDERED_DEPTH_M: f64 = 30.0;

fn dive(class: &str) {
    let catalogue = Arc::new(content::catalogue::catalogue().clone());
    let campaign = content::campaign::load::campaign();
    let def = &campaign.classes[&ClassId::new(class)];
    assert!(def.submersible, "{class}");
    let bp = content::campaign::load::stock_blueprint(&def.blueprint).expect("a stock boat");
    let prefab = Arc::new(build_prefab(&bp, &catalogue));
    let weather = WeatherState { sea_state: SeaState::BEAUFORT_4, ..WeatherState::default() };
    let map = content::campaign::MapDef { size_m: [20_000.0, 20_000.0], depth_m: 200.0, ..Default::default() };
    let mut w = World::new(WorldConfig { seed: 0x5AB0_0A73, weather, map, catalogue: catalogue.clone() });
    let spawn = VesselSpawn {
        prefab: prefab.clone(), side: content::ids::SideId(1), player: None, class: Some(def.id.clone()), pose: sim_core::Pose::from_heading(DVec3::ZERO, 0.0), speed_mps: 0.0,
        damage: None, fuel_fraction: 1.0, ammo_fraction: 1.0, fidelity: Fidelity::Full, submersible: true,
    };
    let id = w.spawn_vessel(&spawn);
    let top0 = w.vessel(id).unwrap().top_height_m();
    assert!(top0 > 0.0, "{class} spawns surfaced");
    let controls = VesselControls { throttle: 0.4, depth_m: Some(ORDERED_DEPTH_M), ..Default::default() };
    w.set_controls(id, controls.clone());
    // Two minutes of simulated time.
    for _ in 0..(120 * sim_core::TICK_HZ as u64) { w.step(); }
    let v = w.vessel(id).unwrap();
    assert_eq!(v.status, VesselStatus::Active, "{class}");
    let top = v.top_height_m();
    assert!(top < 0.0, "{class}: masthead {top:.1} m above the surface after diving");
    let (heel, trim) = v.heel_trim();
    assert!(heel.abs() < 0.15 && trim.abs() < 0.15, "{class}: heel {heel:.2} rad, trim {trim:.2} rad");
    // The keel sits near the ordered depth: the hull's lowest point is within a few metres of 30 m down.
    let keel = v.kin.pose.pos.y;
    assert!((-ORDERED_DEPTH_M - 6.0..=-ORDERED_DEPTH_M + 6.0).contains(&keel), "{class}: keel at {keel:.1} m");
    assert!(v.forward_speed() > 0.5, "{class}: under way at {:.2} m/s", v.forward_speed());
    // Fire one torpedo from the first tube, dead ahead.
    let tube = v.parts.iter().position(|p| matches!(catalogue.parts.get_index(p.def.index()).map(|(_, d)| &d.function), Some(PartFunction::Launcher(l)) if l.kind == LauncherKind::TorpedoTube)).expect("a torpedo tube");
    let mut fire = controls;
    fire.parts = vec![PartControl::default(); tube + 1];
    fire.parts[tube].order = PartOrder::Launch(LaunchAim::Bearing { heading_rad: 0.0 });
    let before = w.bodies().len();
    w.set_controls(id, fire);
    for _ in 0..(5 * sim_core::TICK_HZ as u64) { w.step(); }
    assert!(w.bodies().len() > before, "{class}: the tube launched nothing");
}

#[test]
fn the_coastal_boat_dives_holds_depth_and_fires() { dive("class.tontine"); }

#[test]
fn the_patrol_boat_dives_holds_depth_and_fires() { dive("class.accrual"); }
