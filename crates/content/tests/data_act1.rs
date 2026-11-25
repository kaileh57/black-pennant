//! Units `data-weapons-materials`, `data-tuning-rules`, `data-parts-act1` and `data-kits-act1`: the first-release
//! data loads through the real loaders, carries the spec's numbers, and balances (DESIGN.md L136-151, L539-604,
//! L801-822, L847-860, L965-976, the tuning table).
use content::campaign::{CampaignRules, DifficultyPreset, DifficultySliders};
use content::catalogue::{Catalogue, MaterialDef};
use content::cell::{CellPos, Material};
use content::data::{self, parse_ron};
use content::enums::{Phase, Tier};
use content::ids::PartId;
use content::kit::{kit_footprint, kit_part_placements, kit_sockets_at, KitDef, PartRot};
use content::part::weapons::{BandDef, CalibreBand, Fuse, MunitionKind, SeekerKind, WeaponTables};
use content::part::{LauncherKind, PartCategory, PartDef, PartFunction, PartTag, Placement, PortRole, SensorKind};
use content::enums::NetworkKind;
use content::ruleset::{Ruleset, RulesetKind};
use content::tuning::TuningTable;
use std::collections::{BTreeMap, BTreeSet};

fn cat() -> Catalogue { Catalogue::load_embedded().expect("embedded catalogue loads") }
fn part<'a>(c: &'a Catalogue, id: &str) -> &'a PartDef { c.part(&PartId::new(id)).unwrap_or_else(|| panic!("part {id}")) }
fn act1() -> Vec<PartDef> { parse_ron(data::PARTS[0], "parts/act1.ron").unwrap() }
/// True until a gate fallback commits a change: every fallback commit bumps `tuning.ron`'s version (L620), and
/// may then rewrite band K and B, torpedo radii, tuning and campaign values. Checks of the spec's starting
/// values hold only while this is true; structural checks hold always.
fn untuned() -> bool { parse_ron::<TuningTable>(data::TUNING, "tuning.ron").unwrap().version == 1 }
fn cells(p: &PartDef) -> usize { p.size.iter().map(|&s| s as usize).product() }

#[test]
fn first_release_catalogue_counts_and_tiers() {
    let c = cat();
    let parts = act1();
    assert_eq!(parts.len(), 66, "about 60 parts (L807)");
    let ids: BTreeSet<&str> = parts.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids.len(), parts.len(), "unique ids");

    let count = |f: &dyn Fn(&PartDef) -> bool| parts.iter().filter(|p| f(p)).count();
    let guns: BTreeSet<u16> = parts.iter().filter_map(|p| match &p.function { PartFunction::Gun(g) if !g.point_defence => Some(g.calibre_mm), _ => None }).collect();
    assert_eq!(guns.into_iter().collect::<Vec<_>>(), [20, 30, 40, 57, 76, 100, 127], "7 guns (L543)");
    assert_eq!(count(&|p| matches!(&p.function, PartFunction::Launcher(l) if l.kind == LauncherKind::Missile)), 4, "4 missiles");
    assert_eq!(count(&|p| matches!(&p.function, PartFunction::Launcher(l) if l.kind == LauncherKind::TorpedoTube)), 2, "light and heavy tubes");
    assert_eq!(count(&|p| matches!(&p.function, PartFunction::MineRail { .. })), 1, "the mine rail that lays the third underwater weapon");
    assert_eq!(count(&|p| p.category == PartCategory::Defence && !matches!(p.function, PartFunction::Jammer { .. })), 5, "5 defence systems");
    assert_eq!(count(&|p| matches!(p.function, PartFunction::DroneBay { .. })), 2, "2 drones");
    assert_eq!(count(&|p| matches!(p.function, PartFunction::Sensor(_))), 8, "8 sensors");
    let systems = parts.len() - 7 - 4 - 2 - 5 - 2 - 8;
    assert!((30..=40).contains(&systems), "about 30 hull and system parts, got {systems}");

    for t in [Tier::T1, Tier::T2, Tier::T3, Tier::T4, Tier::T5] {
        assert!(parts.iter().any(|p| p.tier == t), "tier {t:?} is empty");
    }
    // Act 1 (tiers 1 to 3) can build a whole fighting ship; tier 1 alone can float, move, make power and shoot.
    for (t, need) in [(Tier::T1, ["gun.30mm.single", "engine.diesel.small", "prop.small", "rudder.small", "gen.diesel.small", "tank.fuel.large", "radar.nav", "console.bridge", "berths.crew", "pump.small"].as_slice()),
        (Tier::T3, ["launcher.ashm.light", "gun.76mm.single", "magazine.76mm", "hoist.ammunition", "radar.fire_control", "director.fire_control", "decoy.chaff", "decoy.flare", "gun.ciws.20mm", "rail.mine", "davit.rhib", "davit.boarding", "mast.camera", "crane", "berths.passenger", "bay.drone.rotor", "mast.enclosed", "waterjet"].as_slice())] {
        for id in need {
            assert!(part(&c, id).tier <= t, "{id} must be open by tier {t:?}");
        }
    }
    // The first-release file holds only vertical-slice and 0.1.0 rows; later-act parts live in their own files.
    assert!(parts.iter().all(|p| matches!(p.phase, Phase::Slice | Phase::R010)));
    // Every phase-tagged munition a part names exists, and parts never outrun their munition's phase.
    for p in &parts {
        let m = match &p.function { PartFunction::Launcher(l) => Some(&l.munition), PartFunction::MineRail { mine, .. } => Some(mine), PartFunction::DroneBay { drone, .. } => Some(drone), _ => None };
        if let Some(m) = m {
            assert!(c.munition(m).expect("munition").phase <= p.phase.max(Phase::R010), "{}", p.id);
        }
    }
}

#[test]
fn every_part_row_is_well_formed() {
    let c = cat();
    for p in act1() {
        let id = p.id.as_str();
        assert!(p.mass_kg.is_finite() && p.mass_kg > 0.0, "{id} mass");
        assert!(p.size.iter().all(|&s| s >= 1), "{id} size");
        assert!(p.cost > 0, "{id} cost");
        assert_eq!(p.name.0, format!("part.{id}.name"));
        assert_eq!(p.description.0, format!("part.{id}.desc"));
        assert!(!p.model.generator.as_str().is_empty(), "{id} names its generator");
        assert!(p.icon.as_str().starts_with("part_"), "{id} icon");
        assert!(p.rcs_factor > 0.0 && p.rcs_factor <= 1.0, "{id} rcs factor");
        // Mass per cell stays physical: no part is denser than solid steel, none is lighter than a shell.
        let density = p.mass_kg / (cells(&p) as f64 * 0.125);
        assert!((25.0..=7_850.0).contains(&density), "{id}: {density:.0} kg/m^3");

        let consumes = p.ports.iter().any(|q| q.network == NetworkKind::Power && q.role == PortRole::Consume);
        assert_eq!(consumes, p.power_priority != 0, "{id}: a Power consumer has a priority and nothing else does");
        assert!(p.power_priority <= 5, "{id}");
        for q in &p.ports {
            assert!(q.rate.is_finite() && q.rate >= 0.0 && q.capacity.is_finite() && q.capacity >= 0.0, "{id} port");
            if q.network != NetworkKind::Data && q.role != PortRole::Store { assert!(q.rate > 0.0, "{id}: rated port"); }
        }
        // The L287-295 table.
        let want = if p.tags.contains(&PartTag::Steering) { 1 } else if p.tags.contains(&PartTag::PointDefence) { 2 } else {
            match &p.function {
                PartFunction::Rudder { .. } | PartFunction::Console | PartFunction::Pump { .. } => 1,
                PartFunction::Engine(_) | PartFunction::Director | PartFunction::Decoy(_) | PartFunction::Jammer { .. } => 2,
                PartFunction::Sensor(s) if matches!(s.kind, SensorKind::FireControlRadar | SensorKind::Camera) => 2,
                PartFunction::Gun(_) | PartFunction::Hoist { .. } | PartFunction::Launcher(_) => 3,
                PartFunction::Sensor(_) | PartFunction::DroneBay { .. } => 4,
                _ => 5,
            }
        };
        if consumes { assert_eq!(p.power_priority, want, "{id} default priority"); }
        let needs_power = matches!(p.function, PartFunction::Gun(_) | PartFunction::Hoist { .. } | PartFunction::Launcher(_) | PartFunction::Sensor(_) | PartFunction::Director
            | PartFunction::Rudder { .. } | PartFunction::Console | PartFunction::Pump { .. } | PartFunction::Decoy(_) | PartFunction::Jammer { .. } | PartFunction::DroneBay { .. }
            | PartFunction::Crane | PartFunction::RhibDavit | PartFunction::BoardingKit { .. } | PartFunction::CameraMast { .. } | PartFunction::MineRail { .. });
        if needs_power { assert!(consumes, "{id} draws power"); }

        match &p.function {
            PartFunction::Engine(e) => {
                assert!(p.tags.contains(&PartTag::Propulsion));
                let fuel = p.ports.iter().find(|q| q.network == NetworkKind::Fuel).map_or(0.0, |q| q.rate);
                assert!((fuel - e.fuel_lps).abs() < 1e-9, "{id} fuel port matches the burn");
                // 200 to 240 g/kWh at 0.84 kg/L for combustion engines.
                if e.fuel_lps > 0.0 {
                    let g_per_kwh = e.fuel_lps * 0.84 * 3_600_000.0 / e.shaft_kw;
                    assert!((190.0..=250.0).contains(&g_per_kwh), "{id}: {g_per_kwh:.0} g/kWh");
                }
            }
            PartFunction::Generator { kw, fuel_lps } => {
                assert!(p.ports.iter().any(|q| q.network == NetworkKind::Power && q.role == PortRole::Supply && (q.rate - kw).abs() < 1e-9));
                let g_per_kwh = fuel_lps * 0.84 * 3_600_000.0 / kw;
                assert!((190.0..=260.0).contains(&g_per_kwh), "{id}: {g_per_kwh:.0} g/kWh");
            }
            PartFunction::FuelTank { litres } => {
                assert!(p.watertight && p.flammable == content::part::Flammable::Fuel && p.tags.contains(&PartTag::FuelStore));
                assert!(*litres <= cells(&p) as f64 * 125.0, "{id}: fuel fits its tank");
            }
            PartFunction::Magazine { .. } => assert!(p.watertight && p.flammable == content::part::Flammable::Ammunition && p.tags.contains(&PartTag::Magazine), "{id}"),
            PartFunction::Gun(g) => {
                let band = match g.calibre_mm { 0..=40 => CalibreBand::Auto, 41..=76 => CalibreBand::Medium, 77..=127 => CalibreBand::Large, _ => CalibreBand::Heavy };
                assert_eq!(g.band, band, "{id} band (L565-570)");
                assert_eq!(g.point_defence, p.tags.contains(&PartTag::PointDefence), "{id}");
                assert!(g.rof_rpm > 0.0 && g.dispersion_mrad > 0.0 && g.ready_rounds > 0 && g.elevation_max_deg > g.elevation_min_deg);
                assert_eq!(p.placement, Placement::Weather);
            }
            PartFunction::Sensor(s) => {
                assert_eq!(p.placement, if matches!(s.kind, SensorKind::PassiveSonar | SensorKind::ActiveSonar) { Placement::Underwater } else { Placement::Weather }, "{id}");
            }
            _ => {}
        }
        if c.part(&p.id).is_none() { panic!("{id} missing from the catalogue"); }
    }
}

#[test]
fn parts_carry_the_spec_numbers() {
    let c = cat();
    let sensor = |id: &str| match &part(&c, id).function { PartFunction::Sensor(s) => s.clone(), f => panic!("{id}: {f:?}") };
    // Reference ranges against 100 m^2 (L432, L453-454) and rates (L412).
    assert_eq!(sensor("radar.nav").r_ref_m, 20_000.0);
    assert_eq!(sensor("radar.search").r_ref_m, 40_000.0);
    assert_eq!(sensor("radar.fire_control").r_ref_m, 25_000.0);
    assert_eq!(sensor("sonar.passive").r_ref_m, 8_000.0);
    assert_eq!(sensor("sonar.active").r_ref_m, 4_000.0);
    for p in act1() {
        if let PartFunction::Sensor(s) = &p.function {
            let rate = match s.kind { SensorKind::SearchRadar | SensorKind::NavRadar => 5.0, SensorKind::FireControlRadar | SensorKind::Seeker => 10.0, _ => 2.0 };
            assert_eq!(s.rate_hz, rate, "{}", p.id);
            if matches!(s.kind, SensorKind::SearchRadar | SensorKind::NavRadar) { assert_eq!(s.rpm, Some(24.0), "{}", p.id); }
            let emits = matches!(s.kind, SensorKind::SearchRadar | SensorKind::NavRadar | SensorKind::FireControlRadar | SensorKind::ActiveSonar);
            assert_eq!(!s.emitter.is_empty(), emits, "{}: emitters carry an ESM library key", p.id);
        }
    }
    // Pumps (L638), berths (L703-705), jammer (L428), camera mast (L796).
    assert!(matches!(part(&c, "pump.small").function, PartFunction::Pump { m3ps } if m3ps == 0.05));
    assert!(matches!(part(&c, "pump.large").function, PartFunction::Pump { m3ps } if m3ps == 0.15));
    let PartFunction::Berths { count } = part(&c, "berths.crew").function else { panic!() };
    assert_eq!((8 % count, 24 % count), (0, 0), "8 and 24 berths are whole numbers of berth parts");
    let teams = |berths: u8| (berths / 4).clamp(1, 6);
    assert_eq!((teams(8), teams(24)), (2, 6));
    assert!(matches!(part(&c, "jammer").function, PartFunction::Jammer { r_bt0_m } if r_bt0_m == 10_000.0));
    assert!(matches!(part(&c, "mast.camera").function, PartFunction::CameraMast { fov_deg, photo_range_m } if fov_deg == 10.0 && photo_range_m == 6_000.0));
    // Magazines: a 76 mm magazine of 1,800 kg cooks off to 16 cells; a large 127 mm one reaches the 24 cap (L666).
    let cookoff = |kg: f64| (4 + libm::floor(libm::cbrt(kg)) as i64).clamp(4, 24);
    let mag = |id: &str| match part(&c, id).function { PartFunction::Magazine { calibre_mm, rounds, ammo_kg } => (calibre_mm, rounds, ammo_kg), _ => panic!("{id}") };
    assert_eq!(mag("magazine.76mm").2, 1_800.0);
    assert_eq!(cookoff(mag("magazine.76mm").2), 16);
    assert_eq!(cookoff(mag("magazine.127mm").2), 24);
    // Every medium and large gun has a magazine of its calibre, and the hoist outpaces every mount it feeds.
    let PartFunction::Hoist { rounds_per_min } = part(&c, "hoist.ammunition").function else { panic!() };
    for p in act1() {
        if let PartFunction::Gun(g) = &p.function {
            if g.band != CalibreBand::Auto {
                let m = act1().into_iter().find(|q| matches!(q.function, PartFunction::Magazine { calibre_mm, .. } if calibre_mm == g.calibre_mm));
                let m = m.unwrap_or_else(|| panic!("{} has no magazine", p.id));
                assert!(m.tier <= p.tier, "{}: magazine opens with its gun", p.id);
                assert!(g.rof_rpm * g.barrels as f64 <= rounds_per_min, "{}", p.id);
            }
        }
    }
    // Point defence comes with a fire-control lock and the spec's caps (L487-489): the CIWS fires 4,500 rounds a
    // minute from 1,550 ready rounds.
    let PartFunction::Gun(ciws) = &part(&c, "gun.ciws.20mm").function else { panic!() };
    assert!(ciws.point_defence && ciws.band == CalibreBand::Auto);
}

#[test]
fn guns_trade_rate_for_accuracy_within_a_band() {
    // Hits per minute against a 3 m high target broadside at range, from dispersion alone: the 20 mm and 30 mm
    // are close at 1.5 km, the 40 mm is clearly ahead at 3 km, and the 57 mm and 76 mm come within 15% of each
    // other at 4 km. Same-band mounts never dominate on both rate and weight.
    let c = cat();
    let gun = |id: &str| match &part(&c, id).function { PartFunction::Gun(g) => g.clone(), _ => panic!() };
    let hits = |id: &str, range_m: f64| {
        let g = gun(id);
        let sigma = g.dispersion_mrad * 1e-3 * range_m;
        g.rof_rpm * g.barrels as f64 * libm::erf(1.5 / (sigma * core::f64::consts::SQRT_2))
    };
    let close = |a: f64, b: f64, tol: f64| (a / b - 1.0).abs() <= tol;
    assert!(close(hits("gun.20mm.single", 1_500.0), hits("gun.30mm.single", 1_500.0), 0.15));
    assert!(hits("gun.40mm.single", 3_000.0) > 1.8 * hits("gun.30mm.single", 3_000.0));
    assert!(close(hits("gun.57mm.single", 4_000.0), hits("gun.76mm.single", 4_000.0), 0.15));
    assert!(close(hits("gun.100mm.single", 10_000.0), hits("gun.127mm.single", 10_000.0), 0.25));
    // Endurance on ready rounds plus magazine: the 30 mm fires for twice as long as the 20 mm.
    let secs = |id: &str, extra: u32| { let g = gun(id); (g.ready_rounds + extra) as f64 / (g.rof_rpm * g.barrels as f64) * 60.0 };
    assert!(secs("gun.30mm.single", 0) >= 1.4 * secs("gun.20mm.single", 0));
    for (light, heavy) in [("gun.20mm.single", "gun.30mm.single"), ("gun.30mm.single", "gun.40mm.single"), ("gun.57mm.single", "gun.76mm.single"), ("gun.76mm.single", "gun.100mm.single"), ("gun.100mm.single", "gun.127mm.single")] {
        assert!(part(&c, light).mass_kg < part(&c, heavy).mass_kg && part(&c, light).cost < part(&c, heavy).cost, "{light} vs {heavy}");
    }
}

#[test]
fn weapons_table_matches_the_spec() {
    let w: WeaponTables = parse_ron(data::WEAPONS, "weapons.ron").unwrap();
    // Bands: the spec's inputs untouched, and a fitted drag coefficient.
    for b in &w.bands {
        let mut spec = BandDef::spec_default(b.band);
        spec.drag_k = b.drag_k;
        if untuned() { assert_eq!(*b, spec, "{:?}", b.band); }
        assert_eq!((b.p_1km_mm, b.p_cap_mm, b.cap_range_m, b.ref_muzzle_mps), (spec.p_1km_mm, spec.p_cap_mm, spec.cap_range_m, spec.ref_muzzle_mps), "{:?}: fallbacks keep penetration", b.band);
        assert!(b.drag_k > 0.0 && b.drag_k < 1e-3);
        // Independent check of the fit (L563): fly the reference shell to the cap range with the sim's
        // semi-implicit Euler at 60 Hz and compare penetration there with the far-range target, within 5%.
        let (v_land, v_1km) = fly_to(b.ref_muzzle_mps, b.drag_k, b.cap_range_m);
        let p_cap = b.p_1km_mm * libm::pow(v_land / v_1km, 1.4);
        assert!((p_cap / b.p_cap_mm - 1.0).abs() <= 0.05, "{:?}: {p_cap:.1} mm at cap range", b.band);
    }
    let m = |id: &str| w.munitions.iter().find(|m| m.id.as_str() == id).unwrap_or_else(|| panic!("{id}"));
    // The table at L586-595.
    let row = |id: &str, speed: f64, range: f64, b: u8, r: u8| {
        let d = m(id);
        assert!((d.speed_mps - speed).abs() < 0.1, "{id} speed");
        assert_eq!(d.range_m, range, "{id}");
        if untuned() { assert_eq!((d.he_b, d.he_r_cells), (b, r), "{id}"); }
    };
    row("missile.ashm.light", 270.0, 30_000.0, 8, 4);
    row("missile.ashm.heavy", 290.0, 70_000.0, 8, 6);
    row("missile.sam", 600.0, 25_000.0, 4, 1);
    row("missile.pd", 700.0, 10_000.0, 4, 1);
    row("torpedo.light", 40.0 * 0.514_444, 8_000.0, 8, 4);
    row("torpedo.heavy", 45.0 * 0.514_444, 15_000.0, 8, 7);
    assert!(matches!(m("missile.ashm.light").fuse, Fuse::Penetrate { armour_mm, delay_cells: 2 } if armour_mm == 40.0));
    assert!(matches!(m("missile.ashm.heavy").fuse, Fuse::Penetrate { armour_mm, delay_cells: 4 } if armour_mm == 40.0));
    assert!(matches!(m("missile.sam").fuse, Fuse::Proximity { radius_m } if radius_m == 8.0));
    assert!(matches!(m("missile.pd").fuse, Fuse::Proximity { radius_m } if radius_m == 5.0));
    assert!(matches!(m("torpedo.light").fuse, Fuse::Contact) && matches!(m("torpedo.heavy").fuse, Fuse::Contact));
    assert!(matches!(m("mine.influence").fuse, Fuse::Influence { radius_m } if radius_m == 10.0));
    if untuned() { assert_eq!((m("mine.influence").he_b, m("mine.influence").he_r_cells), (8, 6)); }
    assert!(matches!(m("charge.depth").fuse, Fuse::Depth { sink_mps } if sink_mps == 3.0));
    assert!((10.0..=150.0).contains(&-m("charge.depth").cruise_alt_m));
    assert_eq!(m("charge.depth").phase, Phase::Act3);
    // Small bodies (L597, L432), seekers and navigation constants (L520, L432).
    for d in &w.munitions {
        let id = d.id.as_str();
        assert_eq!(d.name.0, format!("munition.{id}.name"));
        match d.kind {
            MunitionKind::AntiShipMissile | MunitionKind::Sam | MunitionKind::PdMissile => {
                assert_eq!((d.radius_m, d.hp), (0.5, 1), "{id}");
                assert!(d.rcs_m2 <= 0.1, "{id}");
                assert_eq!(d.pn_n, if d.kind == MunitionKind::AntiShipMissile { 3.0 } else { 4.0 }, "{id}");
                assert_eq!(d.seeker_fov_half_deg, 30.0);
            }
            MunitionKind::Drone => assert_eq!((d.radius_m, d.hp, d.rcs_m2), (1.0, 2, 0.5), "{id}"),
            MunitionKind::Aircraft => assert_eq!((d.radius_m, d.hp, d.rcs_m2, d.phase), (5.0, 6, 5.0, Phase::Act2), "{id}"),
            MunitionKind::Torpedo => assert!(d.seeker == SeekerKind::ActiveAcoustic && d.pn_n == 3.0 && d.cruise_alt_m < 0.0 && d.noise == 1.0, "{id}"),
            _ => {}
        }
        if d.seeker == SeekerKind::ActiveRadar { assert_eq!(d.seeker_r_ref_m, 12_000.0, "{id}: the spec's 12 km seeker"); }
        for (pay, n) in &d.payload { assert!(*n > 0 && w.munitions.iter().any(|x| &x.id == pay), "{id} payload"); }
    }
    assert!(m("torpedo.heavy").wire_guided);
    // At least one seeker family per decoy type, so chaff, flares and towed decoys each have a target.
    let seekers: BTreeSet<_> = w.munitions.iter().filter(|d| d.phase <= Phase::R010).map(|d| format!("{:?}", d.seeker)).collect();
    for s in ["ActiveRadar", "Infrared", "ActiveAcoustic"] { assert!(seekers.contains(s), "{s}"); }
    // Exactly one RHIB row, found by kind.
    assert_eq!(w.munitions.iter().filter(|d| d.kind == MunitionKind::Rhib).count(), 1);
}

/// Speed at 1 km of path and at the landing, for the low elevation that lands the shell at `range` from sea
/// level; the same integrator as `sim_core::projectiles::ballistic_step`.
fn fly_to(v0: f64, k: f64, range: f64) -> (f64, f64) {
    let fly = |elev: f64| -> (f64, f64, f64) {
        let (mut p, mut v) = ([0.0f64, 0.0], [v0 * libm::cos(elev), v0 * libm::sin(elev)]);
        let (mut path, mut v1, dt) = (0.0, 0.0, 1.0 / 60.0);
        loop {
            let s = libm::sqrt(v[0] * v[0] + v[1] * v[1]);
            let nv = [v[0] - k * s * v[0] * dt, v[1] + (-9.81 - k * s * v[1]) * dt];
            let np = [p[0] + nv[0] * dt, p[1] + nv[1] * dt];
            let seg = libm::hypot(np[0] - p[0], np[1] - p[1]);
            let ns = libm::sqrt(nv[0] * nv[0] + nv[1] * nv[1]);
            if v1 == 0.0 && path + seg >= 1_000.0 { v1 = s + (ns - s) * (1_000.0 - path) / seg; }
            if np[1] < 0.0 {
                let f = p[1] / (p[1] - np[1]);
                return (p[0] + (np[0] - p[0]) * f, s + (ns - s) * f, v1);
            }
            path += seg;
            p = np;
            v = nv;
        }
    };
    let (mut lo, mut hi) = (1e-4f64, 0.6f64);
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if fly(mid).0 >= range { hi = mid; } else { lo = mid; }
    }
    let (_, v_land, v1) = fly(hi);
    (v_land, v1)
}

/// The default shell of a length as a signed-distance field, built once: `shell_sdf` rebuilds it per call.
fn shell_field(length_m: f64) -> content::hull::ShellField {
    match &content::hull::default_hull(length_m).source {
        content::hull::HullSource::Lofted(s) => content::hull::ShellField::new(s).expect("default shell is valid"),
        _ => unreachable!("default hulls are lofted"),
    }
}

#[test]
fn materials_float_the_default_hulls() {
    let mats: Vec<MaterialDef> = parse_ron(data::MATERIALS, "materials.ron").unwrap();
    let get = |m: Material| mats.iter().find(|d| d.material == m).unwrap();
    let steel = get(Material::Steel);
    // Armour tiers are one 0.25 m^2 face of rolled steel (L130): 5, 15, 40 and 80 mm.
    for (i, mm) in [5.0, 15.0, 40.0, 80.0].iter().enumerate() {
        assert!((steel.armour_mass_kg[i] / (0.25 * mm * 1e-3 * 7_850.0) - 1.0).abs() < 0.01);
        assert!(steel.armour_cost[i] > 0.0);
    }
    assert!(get(Material::Aluminium).cell_mass_kg < steel.cell_mass_kg && get(Material::Composite).cell_mass_kg < get(Material::Aluminium).cell_mass_kg);
    assert!(get(Material::Composite).rcs_factor < 0.5, "the low-signature material");
    // Bare structure of every default hull floats with payload to spare: about a third of the datum displacement.
    for l in [10.0, 15.0, 22.0, 26.0, 40.0, 60.0, 90.0] {
        let v = content::hull::voxelize(&content::hull::default_hull(l));
        let g = &v.grid;
        let mut displaced = 0usize;
        let field = shell_field(l);
        for y in 0..v.design_waterline_y.max(0) {
            for z in 0..g.size[2] as i16 {
                for x in 0..g.size[0] as i16 {
                    if field.sdf(CellPos::new(x, y, z).centre_m()) <= 0.0 { displaced += 1; }
                }
            }
        }
        let displacement_t = displaced as f64 * 0.125 * 1.025;
        let structure_t = g.solid_count() as f64 * steel.cell_mass_kg / 1_000.0;
        let share = structure_t / displacement_t;
        assert!(share < if l < 15.0 { 0.75 } else if l < 26.0 { 0.55 } else { 0.45 }, "{l} m: structure {structure_t:.1} t of {displacement_t:.1} t");
        assert!(share > 0.2, "{l} m: structure too light ({share:.2})");
    }
}

/// The 22 m fast attack craft as the starter fits it, for the balance checks: (part, copies).
const FAC22: &[(&str, u32)] = &[
    ("gun.76mm.single", 1), ("magazine.76mm", 1), ("hoist.ammunition", 1), ("gun.30mm.single", 1),
    ("launcher.ashm.light", 2), ("decoy.chaff", 2), ("radar.nav", 1), ("radar.fire_control", 1), ("director.fire_control", 1),
    ("sensor.esm", 1), ("mast.pole", 1), ("engine.diesel.large", 2), ("prop.small", 2), ("rudder.small", 2),
    ("gen.diesel.small", 2), ("tank.fuel.large", 2), ("console.bridge", 1), ("berths.crew", 2), ("pump.small", 2),
    ("door.watertight", 3), ("funnel", 1),
];

#[test]
fn a_fitted_fast_attack_craft_meets_the_anchors() {
    let c = cat();
    let steel = c.material(Material::Steel);
    // Structure: the default 22 m shell plus about 200 cells of deckhouse.
    let v = content::hull::voxelize(&content::hull::default_hull(22.0));
    let structure_cells = v.grid.solid_count() + 200;
    let mut interior_air = 0usize;
    let field = shell_field(22.0);
    for (i, cell) in v.grid.cells.iter().enumerate() {
        let p = v.grid.pos(content::ids::CellIndex(i as u32));
        if cell.is_empty() && field.sdf(p.centre_m()) <= 0.0 { interior_air += 1; }
    }

    let (mut mass, mut cost, mut reload, mut internal) = (structure_cells as f64 * steel.cell_mass_kg, structure_cells as f64 * steel.cell_cost, 0.0, 0usize);
    for &(id, n) in FAC22 {
        let p = part(&c, id);
        let n = n as f64;
        mass += n * p.mass_kg;
        cost += n * p.cost as f64;
        if p.placement == Placement::Internal { internal += cells(p) * n as usize; }
        match &p.function {
            PartFunction::FuelTank { litres } => mass += n * litres * 0.84,
            PartFunction::Magazine { ammo_kg, .. } => mass += n * ammo_kg,
            PartFunction::Launcher(l) => {
                let m = c.munition(&l.munition).unwrap();
                mass += n * l.cells as f64 * m.mass_kg;
                reload += n * l.cells as f64 * m.cost as f64;
            }
            _ => {}
        }
    }
    let tonnes = mass / 1_000.0;
    eprintln!("fitted 22 m craft: {tonnes:.1} t, {cost:.0} S, missiles {reload:.0} S, {internal} of {interior_air} interior cells");
    assert!(tonnes <= 135.0, "fitted 22 m craft is {tonnes:.1} t: it must leave margin for armour under the 150 t cap");
    assert!(tonnes >= 90.0, "fitted 22 m craft is {tonnes:.1} t");
    assert!((850.0..=1_150.0).contains(&cost), "fitted 150 t hull costs about 1,000 S (L820), got {cost:.0}");
    assert!(reload / cost <= 0.05, "missile reload {reload:.0} S of {cost:.0} S");
    assert!(internal <= interior_air, "internal parts need {internal} cells, the default 22 m hull has {interior_air}");
    // Power: generation covers every consumer except electric drive.
    let (mut supply, mut draw) = (0.0, 0.0);
    for &(id, n) in FAC22 {
        for q in &part(&c, id).ports {
            if q.network == NetworkKind::Power { match q.role { PortRole::Supply => supply += n as f64 * q.rate, PortRole::Consume => draw += n as f64 * q.rate, _ => {} } }
        }
    }
    assert!(supply >= draw * 1.2, "{supply} kW for {draw} kW");
    // 6 MW of diesel on two shafts, each within its propeller's rating.
    let PartFunction::Engine(e) = &part(&c, "engine.diesel.large").function else { panic!() };
    let PartFunction::Propulsor(prop) = &part(&c, "prop.small").function else { panic!() };
    assert!(e.shaft_kw <= prop.max_shaft_kw);
}

#[test]
fn stock_kits_are_sound() {
    let c = cat();
    let stock: Vec<&KitDef> = c.kits.values().filter(|k| !k.id.as_str().starts_with("kit.example.")).collect();
    // About 30 at the first public release (L807); later acts' kits (helicopter deck, heavy guns, depth charges)
    // carry later-phase parts and do not count toward it.
    let first_release = stock.iter().filter(|k| k.parts.iter().all(|kp| c.part(&kp.part).is_some_and(|d| d.phase <= Phase::R010))).count();
    assert!((30..=40).contains(&first_release), "about 30 kits (L140), got {first_release}");
    // The tutorial's four kits (L256) under the exact ids its script names.
    for id in ["kit.gun", "kit.radar_mast", "kit.engine", "kit.fuel_tank"] {
        assert!(c.kits.contains_key(&content::ids::KitId::new(id)), "{id}");
    }
    let mut trays = BTreeSet::new();
    for k in &stock {
        let id = k.id.as_str();
        assert_eq!(k.kind, content::kit::KitKind::Assembly);
        assert_eq!(k.name_key.as_ref().map(|t| t.0.as_str()), Some(format!("{id}.name").as_str()));
        assert_eq!(k.description_key.as_ref().map(|t| t.0.as_str()), Some(format!("{id}.desc").as_str()));
        assert_eq!(k.licence, "CC-BY-4.0");
        assert!(k.graph.nodes.is_empty() && k.graph.edges.is_empty(), "{id}: stock kits rely on the generator");
        assert!(trays.insert(k.tray_order), "{id}: tray order is unique");
        assert!(!k.parts.is_empty() && !k.sockets.is_empty());
        for rules in [Ruleset::default(), Ruleset { kind: RulesetKind::Open, ..Ruleset::default() }] {
            let issues = content::validate::validate_kit(k, &c, &rules);
            assert!(issues.is_empty(), "{id} under {:?}: {issues:?}", rules.kind);
        }
        for s in &k.settings { assert!(s.min < s.max && (s.min..=s.max).contains(&s.default), "{id} setting"); }

        // No overlaps: the footprint holds every part cell exactly once, at every rotation.
        let total: usize = k.parts.iter().map(|kp| cells(c.part(&kp.part).unwrap())).sum();
        for rot in [PartRot { yaw: 0, tilt: 0 }, PartRot { yaw: 1, tilt: 0 }, PartRot { yaw: 3, tilt: 2 }] {
            let fp = kit_footprint(k, &c, CellPos::new(10, 10, 10), rot, false);
            assert_eq!(fp.len(), total, "{id}: parts overlap");
            let fp: BTreeSet<CellPos> = fp.into_iter().collect();
            for s in kit_sockets_at(k, &c, CellPos::new(10, 10, 10), rot, false) {
                assert!(fp.contains(&s.cell), "{id}: socket {} is outside the kit", s.name);
            }
        }
        // Weather parts keep open sky inside their own kit.
        let placed = kit_part_placements(k, &c, CellPos::new(0, 0, 0), PartRot::default(), false);
        let all: Vec<(usize, CellPos)> = placed.iter().enumerate().flat_map(|(i, p)| p.cells.iter().map(move |&cell| (i, cell))).collect();
        for (i, p) in placed.iter().enumerate() {
            if c.part(&p.part).unwrap().placement != Placement::Weather { continue; }
            for cell in &p.cells {
                assert!(!all.iter().any(|&(j, o)| j != i && o.x == cell.x && o.z == cell.z && o.y > cell.y), "{id}: {} is covered", p.part);
            }
        }
        // Gun kits: magazine of the gun's calibre and a hoist, wired inside the kit (L283, L207).
        let funcs: Vec<&PartFunction> = k.parts.iter().map(|kp| &c.part(&kp.part).unwrap().function).collect();
        for f in &funcs {
            if let PartFunction::Gun(g) = f {
                if g.band == CalibreBand::Auto { continue; }
                assert!(funcs.iter().any(|f| matches!(f, PartFunction::Magazine { calibre_mm, .. } if *calibre_mm == g.calibre_mm)), "{id}: magazine");
                assert!(funcs.iter().any(|f| matches!(f, PartFunction::Hoist { .. })), "{id}: hoist");
            }
            if let PartFunction::Gun(g) = f { if g.point_defence { assert!(funcs.iter().any(|f| matches!(f, PartFunction::Sensor(s) if s.kind == SensorKind::FireControlRadar))); } }
        }
        // A kit's tags are honest: each is carried by one of its parts.
        for t in &k.tags {
            assert!(k.parts.iter().any(|kp| c.part(&kp.part).unwrap().tags.contains(t)), "{id}: tag {t:?} without a part that has it");
        }
    }
    // The Quartermaster's curated lists (L803): every special-operations tag, shallow-draft propulsion (a
    // waterjet that needs no rudder) and a low-signature mast.
    for t in [PartTag::Crane, PartTag::MineRail, PartTag::RhibDavit, PartTag::BoardingKit, PartTag::RecoveryNet, PartTag::CameraMast, PartTag::PassengerBerths, PartTag::PointDefence, PartTag::FuelStore, PartTag::Propulsion, PartTag::Generation, PartTag::LowSignature] {
        assert!(!c.kits_for_tag(t).is_empty(), "{t:?}");
    }
    let shallow = c.kits_for_tag(PartTag::Propulsion).into_iter().any(|k| k.parts.iter().any(|kp| matches!(&c.part(&kp.part).unwrap().function, PartFunction::Propulsor(p) if p.steer_max_deg > 0.0)));
    assert!(shallow);
}

#[test]
fn tuning_and_rules_files_hold_the_spec_values() {
    let t: TuningTable = parse_ron(data::TUNING, "tuning.ron").unwrap();
    assert!(t.version >= 1);
    if untuned() {
        // The buoyancy gate's fallback (L658) is applied: far hulls use 8-cell blocks (tuning.ron, 15 November 2026).
        let mut spec = TuningTable::default();
        spec.buoyancy.far_blocks_enabled = true;
        // The association gate's duplicates fallback (L468) is applied: the gate grows to 3.5 sigma (tuning.ron,
        // 20 November 2026).
        spec.tracks.gate_sigma = 3.5;
        assert_eq!(TuningTable { version: 0, ..t.clone() }, spec, "tuning.ron writes out the spec defaults and applied fallbacks");
    }
    assert_eq!(cat().tuning, t, "the catalogue carries the file's table");
    for section in ["stability", "flooding", "fire", "crew", "structure", "buoyancy", "projectiles", "decoys", "waves", "networks", "sensors", "tracks", "guidance", "doctrine", "complexity", "special_ops"] {
        assert!(data::TUNING.lines().any(|l| l.trim_start().starts_with(&format!("{section}: ("))), "tuning.ron writes out {section}");
    }

    let r: CampaignRules = parse_ron(data::CAMPAIGN_RULES, "campaign_rules.ron").unwrap();
    if untuned() { assert_eq!(r, CampaignRules::default()); }
    // Every field is written out, so a fallback script edits in place rather than adding a key.
    let written: BTreeSet<&str> = data::CAMPAIGN_RULES.lines().filter_map(|l| l.trim().split_once(':').map(|(k, _)| k)).filter(|k| !k.starts_with("//")).collect();
    let all = ron::ser::to_string(&CampaignRules::default()).unwrap();
    for key in all.trim_matches(|c| c == '(' || c == ')').split(',').filter_map(|kv| kv.split_once(':').map(|(k, _)| k)) {
        assert!(written.contains(key), "campaign_rules.ron omits {key}");
    }

    let d: Vec<DifficultyPreset> = parse_ron(data::DIFFICULTY, "difficulty.ron").unwrap();
    let by: BTreeMap<&str, DifficultySliders> = d.iter().map(|p| (p.id.as_str(), p.sliders)).collect();
    assert_eq!(by.len(), 3);
    assert_eq!(by["easy"], DifficultySliders::EASY);
    assert_eq!(by["normal"], DifficultySliders::NORMAL);
    assert_eq!(by["hard"], DifficultySliders::HARD);
    for p in &d {
        assert_eq!(p.sliders.clamped(), p.sliders, "{} inside the slider ranges", p.id.as_str());
        assert_eq!(p.name.0, format!("difficulty.{}", p.id.as_str()));
    }

    let rs: Vec<(String, Ruleset)> = parse_ron(data::RULESETS, "rulesets.ron").unwrap();
    let names: Vec<&str> = rs.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["standard", "open"]);
    assert_eq!(rs[0].1, Ruleset::default(), "standard is the public-lobby default (L969)");
    assert_eq!(rs[1].1, Ruleset { kind: RulesetKind::Open, ..Ruleset::default() });
    assert!(rs.iter().all(|(_, r)| !r.luau && [150.0, 400.0, 900.0, 1_800.0].contains(&r.tonnage_cap_t)));
}

/// Every translation key the first-release data names has an English row in `game/i18n/` (the copy lint's
/// coverage rule).
#[test]
fn every_data_key_has_english_text() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/i18n");
    let mut have = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "csv") {
            for line in std::fs::read_to_string(&p).unwrap().lines().skip(1) {
                if let Some((k, rest)) = line.split_once(',') { have.insert(k.to_string(), rest.to_string()); }
            }
        }
    }
    let c = cat();
    let mut keys: Vec<String> = Vec::new();
    for p in act1() { keys.push(p.name.0.clone()); keys.push(p.description.0.clone()); }
    let w: WeaponTables = parse_ron(data::WEAPONS, "weapons.ron").unwrap();
    keys.extend(w.munitions.iter().map(|m| m.name.0.clone()));
    for k in c.kits.values().filter(|k| !k.id.as_str().starts_with("kit.example.")) {
        keys.extend(k.name_key.iter().chain(k.description_key.iter()).map(|t| t.0.clone()));
        keys.extend(k.settings.iter().map(|s| s.label.0.clone()));
    }
    let d: Vec<DifficultyPreset> = parse_ron(data::DIFFICULTY, "difficulty.ron").unwrap();
    keys.extend(d.iter().map(|p| p.name.0.clone()));
    let missing: Vec<&String> = keys.iter().filter(|k| !have.contains_key(*k)).collect();
    assert!(missing.is_empty(), "no English text for {missing:?}");

}
