//! The six starter hulls and the tutorial variants, as code.
use super::*;
use content::catalogue::Catalogue;
use content::cell::ArmourTier;

/// Every stock file this generator writes, with the act whose cap it must meet.
pub struct Stock {
    pub path: &'static str,
    pub act: content::enums::Act,
    pub bp: Blueprint,
}

/// 22 m fast attack craft hull: a hard-chine semi-planing form with a deep forefoot, a run that lifts aft over
/// the shafts, flare and sheer forward.
const FAC: [StationRow; 7] = [
    (0.00, 0.90, 0.20, 10.0, 0.0, 0.95),
    (0.15, 0.96, 0.44, 10.0, 0.0, 0.93),
    (0.35, 1.00, 0.94, 10.0, 1.0, 0.95),
    (0.55, 0.99, 1.00, 12.0, 5.0, 1.00),
    (0.75, 0.94, 1.00, 16.0, 9.0, 1.10),
    (0.90, 0.74, 1.00, 24.0, 16.0, 1.26),
    (1.00, 0.00, 0.90, 30.0, 22.0, 1.40),
];

pub fn fac_shell(beam_scale: f64) -> HullShell { shell(22.0, 6.6 * beam_scale, 1.9, 2.2, &FAC, 22.0, 8.0, 1.0) }

/// 15 m patrol boat hull: a semi-planing vee with a deep forefoot and a run that lifts aft over the shafts, low
/// freeboard aft and a flared bow.
const PATROL: [StationRow; 6] = [
    (0.00, 0.92, 0.10, 20.0, 0.0, 0.95),
    (0.15, 0.97, 0.30, 22.0, 0.0, 0.95),
    (0.40, 1.00, 1.00, 25.0, 0.0, 1.00),
    (0.65, 0.95, 1.00, 28.0, 5.0, 1.12),
    (0.88, 0.66, 0.86, 32.0, 16.0, 1.28),
    (1.00, 0.00, 0.58, 36.0, 22.0, 1.42),
];

pub fn patrol_shell() -> HullShell { shell(15.0, 6.0, 1.9, 1.85, &PATROL, 24.0, 6.0, 1.0) }

/// The 15 m patrol boat: two 1 MW diesels on twin screws with a rudder behind each, a 30 mm gun forward and a
/// 20 mm aft, navigation radar and smoke on the wheelhouse. Tier 1 and vertical-slice parts throughout.
pub fn p15(cat: &Catalogue) -> Builder<'_> {
    let mut b = Builder::new(cat, patrol_shell(), &[], &[6.25, 9.0, 11.5]);
    let h = b.half();
    let d = b.deck_m(6.0);
    b.houses(&[House::new(4.5, 9.0, 1.5, d, 2.0).lean(0.75, 0.0, 0.25).mat(Material::Aluminium).glass(1.0, 1.5)]);
    // Two diesels on twin shafts under the cut-up run, a rudder behind each screw. The blades also keep her
    // running straight: a hull with no foil aft yaws away from its course.
    for (zi, zo) in [(b.zp(0, 3), b.zp(1, 3)), (b.zs(0), b.zs(1))] {
        b.below("engine.diesel.small", YAW_0, 4.2, zi);
        b.prop("prop.small", 1.2, zo);
    }
    for z in [b.zp(2, 1), b.zs(2)] {
        b.part("rudder.small", YAW_0, At::Lift { x: b.x(0.1), z });
    }
    b.below("pump.small", YAW_0, 1.5, h + 1);
    // The generator and the berths amidships, fuel forward of them.
    b.below("gen.diesel.small", YAW_0, 6.75, b.zp(0, 2));
    b.below("berths.crew", YAW_0, 6.75, b.zs(0));
    for z in [b.zp(0, 3), b.zs(0)] {
        b.below("tank.fuel.small", YAW_0, 9.5, z);
    }
    b.below("pump.small", YAW_0, 12.0, h);
    for xm in [6.25, 9.0] {
        b.door(b.x(xm), h - 1);
    }
    // Wheelhouse.
    let x = b.x(5.0);
    b.under("console.bridge", YAW_0, 5.0, b.zc(2), b.roof(x, h));
    // Guns, radar, smoke.
    // The mounts are three cells wide, so each sits half a cell off the centreline; the heavier one to port.
    b.on("kit.gun.30mm", YAW_0, 10.5, b.zc(3) - 1);
    b.on("kit.gun.20mm", YAW_AFT, 0.0, b.zc(3));
    b.on("radar.nav", YAW_0, 5.5, b.zc(2));
    for z in [b.zp(1, 2), b.zs(1)] {
        b.on("decoy.smoke", YAW_0, 4.1, z);
    }
    b.hull_number("P 15", b.x(11.5), 2);
    b
}

/// 26 m insertion craft hull: a deep-vee planing form with near-vertical topsides, a reverse bow and an
/// immersed transom raked well aft, so the jets exhaust under an overhang that also shelters the steering
/// blades. Its low radar return comes from the composite topsides, the inward-leaning deckhouse and the
/// enclosed mast, not from tumblehome, which costs a light, tall craft like this its righting arm at large
/// angles. Even so her righting arm is thin between 30 and 50 degrees: she is a fair-weather boat.
const INSERTION: [StationRow; 6] = [
    (0.00, 0.94, 0.92, 10.0, 2.0, 0.96),
    (0.20, 0.98, 0.98, 12.0, 2.0, 0.96),
    (0.45, 1.00, 1.00, 13.0, 2.0, 1.00),
    (0.70, 0.90, 1.00, 16.0, 3.0, 1.05),
    (0.88, 0.62, 0.92, 24.0, 4.0, 1.12),
    (1.00, 0.00, 0.72, 34.0, 0.0, 1.18),
];

pub fn insertion_shell() -> HullShell { shell(26.0, 5.6, 1.6, 2.1, &INSERTION, -12.0, 25.0, 1.0) }

/// The 26 m low-signature insertion craft: composite hull and leaning deckhouse, two diesel waterjets with a
/// rudder blade in each jet's race under the transom overhang, the enclosed low-signature mast, a
/// stern-launched RHIB, a camera mast and berths for an insertion team. Its cross-section answers Act 1's
/// stealth-insertion tags without a redesign (L151).
pub fn i26(cat: &Catalogue) -> Builder<'_> {
    let mut b = Builder::new(cat, insertion_shell(), &[], &[9.5, 14.0, 18.0, 21.5]);
    // Composite above the waterline, where radar sees it; an aluminium bottom below, which keeps her weight low
    // (a composite cell weighs half a steel one, so an all-composite hull stands tall on its topsides).
    b.dress(Material::Composite, PAINT_SEA);
    b.bottom(Material::Aluminium);
    let h = b.half();
    let d = b.deck_m(13.0);
    b.houses(&[House::new(9.0, 17.0, 2.25, d, 2.5).lean(1.0, 0.5, 0.5).mat(Material::Composite).paint(PAINT_SEA).glass(1.25, 1.75)]);
    // Two 3 MW diesels on jets through the transom, the jets' intakes through the bottom. A jet hull with no
    // foil aft yaws away from its course, so a blade hangs in each jet's race under the overhang.
    for z in [b.zp(0, 3), b.zs(0)] {
        b.jet("waterjet", z, 5, true);
        b.below("engine.diesel.large", YAW_0, 5.0, z);
    }
    for z in [b.zp(1, 1), b.zs(1)] {
        b.part("rudder.small", YAW_0, At::Lift { x: 0, z });
    }
    b.below("pump.small", YAW_0, 4.0, b.zs(3));
    // Generators and the insertion team's berths amidships, fuel low in the two compartments forward of them
    // (her range, and ballast that keeps a light, tall craft stiff in a hard turn); her crew of four berths in
    // the deckhouse.
    b.below("kit.generator", YAW_0, 10.0, b.zc(4));
    b.below("kit.passengers", YAW_0, 12.0, b.zc(4));
    b.below("kit.fuel_tank", YAW_0, 14.5, b.zc(4));
    b.below("pump.small", YAW_0, 17.5, h);
    b.below("kit.fuel_tank", YAW_0, 18.5, b.zc(4));
    for xm in [9.5, 14.0, 18.0] {
        b.door(b.x(xm), h - 1);
    }
    // Bridge, and the crew's berths aft of it in the deckhouse.
    let x = b.x(10.0);
    b.under("console.bridge", YAW_0, 10.0, b.zc(2), b.roof(x, h));
    let x = b.x(12.0);
    b.under("berths.crew", YAW_0, 12.0, b.zc(3), b.roof(x, h));
    // Topside: the low-signature mast stepped on the deck aft of the deckhouse rather than on its roof (the
    // eight-metre enclosed mast is the heaviest thing aloft, and every metre lower keeps her stiff in a turn),
    // the camera mast on the deckhouse, RHIB at the stern, a 30 mm forward: light enough to keep her weight
    // low, for a craft that wins by not being seen.
    b.on("kit.radar_mast.stealth", YAW_0, 6.5, b.zc(4));
    b.on("kit.camera_mast", YAW_0, 10.0, b.zc(2));
    b.on("kit.decoys", YAW_0, 13.5, b.zc(4));
    b.on("kit.rhib", YAW_0, 0.0, b.zc(5) - 1);
    b.on("kit.gun.30mm", YAW_0, 18.5, b.zc(4));
    b.hull_number("P 26", b.x(21.0), 2);
    b
}

/// 40 m missile corvette hull: a round-bilge displacement form with a cut-up run over the shafts, a raked
/// flared bow and a transom stern.
const CORVETTE40: [StationRow; 7] = [
    (0.00, 0.88, 0.30, 12.0, 4.0, 0.92),
    (0.12, 0.95, 0.55, 12.0, 5.0, 0.92),
    (0.30, 1.00, 0.92, 12.0, 4.0, 0.94),
    (0.50, 1.00, 1.00, 13.0, 5.0, 1.00),
    (0.70, 0.90, 1.00, 18.0, 9.0, 1.10),
    (0.87, 0.62, 0.95, 26.0, 18.0, 1.28),
    (1.00, 0.00, 0.75, 32.0, 24.0, 1.45),
];

pub fn corvette40_shell() -> HullShell { shell(40.0, 8.2, 2.4, 2.8, &CORVETTE40, 25.0, 10.0, 1.0) }

/// The 40 m missile corvette: eight heavy anti-ship missiles amidships, a 76 mm gun forward, point-defence
/// missiles and a CIWS, a search radar behind the bridge under the fire-control mast, and a rotor drone aft.
/// Four 3 MW diesels on four shafts.
pub fn k40(cat: &Catalogue) -> Builder<'_> {
    let mut b = Builder::new(cat, corvette40_shell(), &[], &[4.0, 9.5, 15.5, 21.5, 28.0, 33.5]);
    let h = b.half();
    // Superstructure, all aluminium: a low after deckhouse for the CIWS, the main deckhouse, the bridge on top.
    let (da, dm) = (b.deck_m(6.0), b.deck_m(21.0));
    b.houses(&[
        House::new(4.0, 9.0, 2.5, da, 2.0).lean(0.0, 0.5, 0.25).mat(Material::Aluminium),
        House::new(15.5, 27.0, 3.0, dm, 2.5).lean(0.5, 0.5, 0.25).mat(Material::Aluminium),
        House::new(21.0, 27.0, 2.5, dm + 2.5, 2.5).lean(1.0, 0.0, 0.5).mat(Material::Aluminium).glass(1.25, 1.75),
    ]);
    // Four diesels in two engine rooms, fuel forward of them with the berths, generators aft.
    for xm in [10.0, 16.0] {
        for z in [b.zp(1, 3), b.zs(1)] {
            b.below("kit.engine", YAW_0, xm, z);
        }
    }
    for z in [b.zp(0, 4), b.zs(0)] {
        b.below("kit.fuel_tank", YAW_0, 22.0, z);
    }
    b.below("kit.fuel_tank", YAW_0, 25.0, b.zc(4));
    b.below("gen.diesel.large", YAW_0, 4.5, b.zc(3) - 1);
    b.below("kit.generator", YAW_0, 7.5, b.zc(4));
    for z in [b.zp(0, 3), b.zs(0)] {
        b.below("berths.crew", YAW_0, 28.5, z);
    }
    b.below("berths.crew", YAW_0, 30.5, b.zc(3));
    for xm in [2.0, 7.0, 13.0, 19.0, 24.5, 32.5] {
        b.below("pump.small", YAW_0, xm, h);
    }
    // Shafts and rudders.
    for z in [b.zp(0, 3), b.zs(0), b.zp(4, 3), b.zs(4)] {
        b.prop("prop.small", 1.5, z);
    }
    for z in [b.zp(1, 1), b.zs(1)] {
        b.part("rudder.small", YAW_0, At::Lift { x: b.x(0.3), z });
    }
    // Bridge.
    let x = b.x(22.0);
    b.under("console.bridge", YAW_0, 22.0, b.zc(2), b.roof(x, h));
    // Weapons.
    let x = b.x(33.0);
    let deck = b.deck(x);
    b.kit("kit.gun", YAW_0, false, At::Socket { name: "deck", target: CellPos::new(x, deck, h) });
    b.on("kit.pd_missile", YAW_0, 27.5, b.zc(5));
    b.on("kit.ashm.heavy", YAW_0, 10.0, b.zc(8));
    b.on("kit.ciws", YAW_AFT, 5.0, b.zc(4));
    b.on("kit.drone.rotor", YAW_0, 0.5, b.zc(4));
    // Sensors: the fire-control mast on the bridge, the search radar and funnel behind it, decoys either side.
    b.on("sensor.esm", YAW_0, 23.5, b.zc(2));
    b.on("kit.radar_mast", YAW_0, 21.5, b.zc(4));
    b.on("radar.search", PartRot { yaw: 1, tilt: 0 }, 19.0, b.zc(8));
    b.on("funnel", YAW_0, 16.2, b.zc(3));
    b.on("kit.decoys", YAW_0, 16.2, b.zp(2, 4));
    b.on_mirrored("kit.decoys", YAW_0, 16.2, b.zs(2));
    for xm in [9.5, 15.5, 21.5, 28.0] {
        b.door(b.x(xm), h - 1);
    }
    b.hull_number("K 40", b.x(34.0), 3);
    b
}

/// 60 m corvette hull: a longer, finer round-bilge form with a raised forecastle sheer, a cut-up run over four
/// shafts and a broad transom for the mine rails.
const CORVETTE60: [StationRow; 7] = [
    (0.00, 0.90, 0.30, 12.0, 4.0, 0.90),
    (0.12, 0.96, 0.55, 12.0, 5.0, 0.90),
    (0.30, 1.00, 0.92, 12.0, 6.0, 0.92),
    (0.50, 1.00, 1.00, 13.0, 7.0, 1.00),
    (0.70, 0.90, 1.00, 18.0, 10.0, 1.12),
    (0.87, 0.60, 0.95, 26.0, 18.0, 1.32),
    (1.00, 0.00, 0.75, 32.0, 24.0, 1.50),
];

pub fn corvette60_shell() -> HullShell { shell(60.0, 9.6, 3.0, 3.4, &CORVETTE60, 25.0, 10.0, 1.0) }

/// The 60 m corvette: a 100 mm gun, eight heavy anti-ship missiles, a CIWS, light torpedoes and a hull sonar
/// for the Act 3 submarines, two mine rails over the stern, a boarding davit to starboard and a RHIB to port,
/// with a crane to work them. Four 3 MW diesels on four shafts; machinery under an internal deck.
pub fn k60(cat: &Catalogue) -> Builder<'_> {
    let mut b = Builder::new(cat, corvette60_shell(), &[0.5], &[5.0, 11.0, 18.0, 25.0, 32.0, 39.0, 46.0, 52.0]);
    let h = b.half();
    let lower = b.lay.waterline_y + 1;
    // Superstructure: a low after deckhouse carrying the CIWS and crane, the main deckhouse amidships with the
    // bridge on top, kept narrow so the davits clear it.
    let (da, dm) = (b.deck_m(12.0), b.deck_m(36.0));
    b.houses(&[
        House::new(9.0, 15.0, 3.0, da, 2.5).lean(0.0, 0.5, 0.25).mat(Material::Aluminium),
        House::new(24.0, 42.0, 3.25, dm, 2.5).lean(0.5, 0.5, 0.25).mat(Material::Aluminium),
        House::new(34.0, 42.0, 3.0, dm + 2.5, 2.5).lean(1.0, 0.5, 0.5).mat(Material::Aluminium).glass(1.25, 1.75),
    ]);
    // Machinery under the internal deck: four diesels in two engine rooms, fuel forward of them, generators
    // forward again over the sonar.
    let low = Some(lower);
    for z in [b.zp(1, 3), b.zs(1)] {
        b.fit_in("gen.diesel.large", YAW_0, 39.0, 46.0, z, low);
        b.fit_in("kit.engine", YAW_0, 12.0, 18.0, z, low);
        b.fit_in("kit.engine", YAW_0, 18.0, 25.0, z, low);
    }
    for z in [b.zp(0, 4), b.zs(0)] {
        b.fit_in("kit.fuel_tank", YAW_0, 25.0, 32.0, z, low);
        b.fit_in("kit.fuel_tank", YAW_0, 32.0, 39.0, z, low);
    }
    b.fit_in("kit.fuel_tank", YAW_0, 25.0, 32.0, b.zc(4), low);
    b.keel("kit.sonar", 41.0);
    // Trim ballast low in the after machinery space, to port: the boarding davit to starboard outweighs the
    // RHIB to port.
    let ballast = b.part("ballast.tank", YAW_0, At::Drop { x: b.x(7.0), z: b.zp(0, 4), under: lower });
    // Between decks: berths, pumps in every compartment, doors through the bulkheads.
    for (xm, z) in [(25.5, b.zp(1, 3)), (25.5, b.zs(1)), (32.5, b.zp(1, 3)), (32.5, b.zs(1))] {
        b.below("berths.crew", YAW_0, xm, z);
    }
    for xm in [2.5, 8.0, 14.5, 21.5, 29.0, 36.0, 43.0, 49.0] {
        b.below("pump.small", YAW_0, xm, h);
    }
    for xm in [11.0, 18.0, 25.0, 32.0, 39.0] {
        b.door(b.x(xm), h - 1);
    }
    // Shafts and rudders.
    for z in [b.zp(0, 3), b.zs(0), b.zp(4, 3), b.zs(4)] {
        b.prop("prop.small", 2.5, z);
    }
    for z in [b.zp(1, 1), b.zs(1)] {
        b.part("rudder.small", YAW_0, At::Lift { x: b.x(1.2), z });
    }
    // Bridge.
    let x = b.x(36.0);
    b.under("console.bridge", YAW_0, 36.0, b.zc(2), b.roof(x, h));
    // Weapons.
    b.gun("kit.gun.100mm", 44.5);
    b.on("kit.ashm.heavy", YAW_0, 17.0, b.zc(8));
    b.on("kit.ciws", YAW_AFT, 10.0, b.zc(4));
    for z in [b.zp(6, 3), b.zs(6)] {
        b.on("kit.torpedo.light", YAW_0, 44.0, z);
    }
    // Special operations: mine rails over the stern, the boarding davit and the RHIB either side of the
    // missile launchers, a crane on the after deckhouse.
    let edge = b.half() - 1;
    for z in [b.zp(edge - 1, 2), b.zs(edge - 1)] {
        b.on("kit.mine_rail", YAW_0, 0.0, z);
    }
    b.on("kit.boarding", YAW_0, 17.0, b.zs(h - 6));
    b.on("kit.rhib", YAW_0, 17.5, b.zp(h - 5, 5));
    b.on("kit.crane", YAW_0, 13.0, b.zc(4));
    b.on("kit.drone.rotor", YAW_0, 5.0, b.zc(4));
    // Sensors and decoys.
    b.on("sensor.esm", YAW_0, 39.0, b.zc(2));
    b.on("kit.radar_mast.lattice", YAW_0, 35.0, b.zc(4) - 1);
    b.on("kit.radar_mast", YAW_0, 26.0, b.zc(4));
    b.on("sensor.eo", YAW_0, 40.0, b.zs(3));
    b.on("funnel", YAW_0, 29.0, b.zc(3));
    b.on("kit.decoys", YAW_0, 31.5, b.zp(2, 4));
    b.on_mirrored("kit.decoys", YAW_0, 31.5, b.zs(2));
    b.hull_number("K 60", b.x(52.0), 3);
    b.balance_heel(ballast);
    b
}

/// 90 m destroyer hull: a long fine displacement form with a flared clipper bow, a high forecastle sheer and a
/// flat counter stern that lifts clear over the two large propellers.
const DESTROYER: [StationRow; 10] = [
    (0.00, 0.86, 0.00, 0.0, 4.0, 0.88),
    (0.035, 0.90, 0.04, 0.0, 4.0, 0.88),
    (0.07, 0.93, 0.48, 3.0, 5.0, 0.88),
    (0.14, 0.97, 0.78, 6.0, 5.0, 0.89),
    (0.25, 1.00, 0.95, 12.0, 6.0, 0.90),
    (0.45, 1.00, 1.00, 13.0, 7.0, 0.96),
    (0.65, 0.94, 1.00, 16.0, 10.0, 1.06),
    (0.80, 0.82, 0.98, 22.0, 15.0, 1.18),
    (0.92, 0.60, 0.97, 28.0, 22.0, 1.26),
    (1.00, 0.00, 0.80, 34.0, 28.0, 1.36),
];

/// The destroyer's internal deck, metres above the design waterline: high enough for the turbine under it.
pub const DESTROYER_DECK_M: f64 = 1.5;

pub fn destroyer_shell() -> HullShell { shell(90.0, 11.4, 3.8, 4.2, &DESTROYER, 30.0, 8.0, 1.0) }

/// The 90 m destroyer: a 127 mm gun, sixteen surface-to-air missiles in two vertical launchers, eight heavy
/// anti-ship missiles, two CIWS and point-defence missiles, search radar, hull sonar, light torpedoes and a
/// fixed-wing drone aft. A 20 MW gas turbine and two 3 MW cruise diesels on six shafts (21 MW of propellers, so
/// the turbine alone drives her flat out and the diesels cruise); 24 berths (six damage-control teams, L705).
pub fn d90(cat: &Catalogue) -> Builder<'_> {
    let mut b = Builder::new(cat, destroyer_shell(), &[DESTROYER_DECK_M], &[6.0, 14.0, 22.0, 30.0, 38.0, 46.0, 54.0, 62.0, 70.0, 78.0]);
    let h = b.half();
    let lower = b.planes[0].offset;
    let low = Some(lower);
    // Superstructure: steel deckhouses (the after one with the funnel and hangar roof, the forward one under the
    // bridge, splinter-proof where the crew works), an aluminium bridge on top to keep the weight down high up.
    let (da, df) = (b.deck_m(22.0), b.deck_m(50.0));
    b.houses(&[
        House::new(12.0, 30.0, 4.0, da, 3.0).lean(0.5, 0.5, 0.25),
        House::new(38.0, 57.0, 4.25, df, 3.0).lean(0.5, 0.5, 0.25),
        House::new(44.0, 53.5, 3.75, df + 3.0, 3.0).lean(1.0, 0.5, 0.5).mat(Material::Aluminium).glass(1.5, 2.25),
    ]);
    // Machinery under the internal deck: generators, the turbine, the cruise diesels, then fuel.
    for z in [b.zp(0, 3), b.zs(0)] {
        b.fit_in("gen.diesel.large", YAW_0, 14.0, 22.0, z, low);
    }
    b.fit_in("kit.engine.turbine", YAW_0, 22.0, 30.0, b.zc(6), low);
    for z in [b.zp(1, 3), b.zs(1)] {
        b.fit_in("kit.engine", YAW_0, 30.0, 38.0, z, low);
    }
    for (x0, x1, off) in [(22.0, 30.0, 3), (46.0, 54.0, 0)] {
        for z in [b.zp(off, 4), b.zs(off)] {
            b.fit_in("kit.fuel_tank", YAW_0, x0, x1, z, low);
        }
    }
    for z in [b.zp(5, 4), b.zs(5)] {
        b.fit_in("kit.fuel_tank", YAW_0, 30.0, 38.0, z, low);
        b.fit_in("kit.fuel_tank", YAW_0, 38.0, 46.0, z, low);
    }
    b.fit_in("kit.generator", YAW_0, 54.0, 62.0, b.zc(4), low);
    for z in [b.zp(2, 4), b.zs(2)] {
        b.fit_in("kit.fuel_tank", YAW_0, 6.0, 14.0, z, low);
        b.fit_in("kit.fuel_tank", YAW_0, 14.0, 22.0, z, low);
    }
    b.keel("kit.sonar", 64.0);
    // Between decks: berths amidships, pumps in every compartment, doors through the bulkheads.
    for x0 in [38.0, 46.0, 54.0] {
        for z in [b.zp(1, 3), b.zs(1)] {
            b.fit_in("berths.crew", YAW_0, x0, x0 + 8.0, z, None);
        }
    }
    for xm in [3.0, 10.0, 18.0, 26.0, 34.0, 42.0, 50.0, 58.0, 66.0, 76.5, 81.0] {
        b.below("pump.small", YAW_0, xm, h);
    }
    for xm in [14.0, 22.0, 30.0, 38.0, 46.0, 54.0, 62.0] {
        b.door(b.x(xm), h - 1);
    }
    // Shafts and rudders.
    for off in [0, 4, 8] {
        for z in [b.zp(off, 3), b.zs(off)] {
            b.prop("prop.small", 4.5, z);
        }
    }
    for z in [b.zp(4, 1), b.zs(4)] {
        b.part("rudder.large", YAW_0, At::Lift { x: b.x(0.2), z });
    }
    // Bridge.
    let x = b.x(47.0);
    b.under("console.bridge", YAW_0, 47.0, b.zc(2), b.roof(x, h));
    // Weapons: the 127 mm forward of two vertical launchers, anti-ship missiles amidships, CIWS fore and aft,
    // point-defence missiles on the hangar, torpedoes either side.
    b.gun("kit.gun.127mm", 70.5);
    for z in [b.zp(1, 3), b.zs(1)] {
        b.vls("kit.sam", 59.5, z);
    }
    b.on("kit.ashm.heavy", YAW_0, 33.0, b.zc(8));
    b.on("kit.ciws", YAW_AFT, 12.5, b.zc(4));
    b.on("kit.ciws", YAW_0, 54.5, b.zc(4));
    b.on("kit.pd_missile", YAW_AFT, 22.0, b.zc(5));
    for z in [b.zp(7, 3), b.zs(7)] {
        b.on("kit.torpedo.light", YAW_0, 39.0, z);
    }
    // Flight deck and stern: the fixed-wing drone, the towed decoy.
    b.on("kit.drone.fixed", YAW_0, 5.0, b.zc(4));
    b.on("kit.towed_decoy", YAW_AFT, 0.0, b.zc(3) - 3);
    // Sensors, decoys, the jammer.
    b.on("sensor.esm", YAW_0, 45.0, b.zc(2));
    b.on("sensor.eo", YAW_0, 49.0, b.zs(3));
    b.on("jammer", YAW_0, 49.0, b.zp(3, 2));
    b.on("kit.radar_mast", YAW_0, 46.5, b.zc(4));
    b.on("kit.radar_mast.lattice", YAW_0, 40.0, b.zc(4) - 1);
    b.on("funnel", YAW_0, 26.0, b.zc(3));
    b.on("kit.decoys", YAW_0, 44.0, b.zp(3, 4));
    b.on_mirrored("kit.decoys", YAW_0, 44.0, b.zs(3));
    // A 40 mm belt over the machinery and magazines, from two rows under the waterline to four over it: proof
    // against the medium guns of the Act 4 escorts, and the starter that shows a player what armour is for.
    b.belt(12.0, 74.0, 2, 4, ArmourTier::Mm40);
    b.hull_number("D 90", b.x(78.0), 4);
    b
}

/// Beam of the level-0 tutorial hull as a fraction of the fitted craft's: narrow enough that Float fails
/// (GM at or below zero) until the player widens it (L256), and close enough to the level 1 hull that the
/// dock's settle onto that hull when Float first shows Pass (the kit step's `LoadBlueprint`) barely moves it.
pub const TUTORIAL_BEAM: f64 = 0.76;
/// Beam of the level 1 to 3 tutorial hulls ("the beam already at Pass"): the 6 m hull the level-0 player reaches
/// by widening, where the unfitted craft's GM sits inside the Pass band. The fitted craft is a metre wider.
pub const TUTORIAL_PASS_BEAM: f64 = 0.88;
/// Beam of the level 2 and 3 hull, which carries its engines and fuel from the start: wide enough that the gun
/// and radar mast the player drops, and the gun's Fix, leave Float at Pass (at 0.88 they end at GM 0.04 m).
pub const TUTORIAL_DRIVE_BEAM: f64 = 0.96;

/// What to leave off a fitted 22 m craft (the tutorial variants).
#[derive(Clone, Copy, Default)]
pub struct FacOmit {
    pub gun: bool,
    pub mast: bool,
    pub drive: bool,
}

pub fn fac22(cat: &Catalogue, beam_scale: f64, omit: FacOmit) -> Builder<'_> {
    let mut b = Builder::new(cat, fac_shell(beam_scale), &[], &[2.5, 6.0, 11.0, 14.5, 19.0]);
    let h = b.half();
    let sky = b.size()[1];
    // Deckhouse: steel (its weight keeps GM inside the Pass band with and without the gun), raked front, bridge
    // windows; a low after deckhouse carries the funnel.
    let d = b.deck_m(11.0);
    b.houses(&[
        House::new(8.5, 14.0, 1.75, d, 2.75).lean(1.0, 0.0, 0.25).glass(1.25, 2.0),
        House::new(6.5, 9.0, 1.25, d, 1.0),
    ]);
    // Machinery amidships where the hull is deepest: two 3 MW diesels, generators aft of them, fuel forward.
    if !omit.drive {
        let x = b.x(6.5);
        for z in [b.zs(1), b.zp(1, 3)] {
            b.kit("kit.engine", YAW_0, false, At::Drop { x, z, under: b.deck(x) });
        }
        let x = b.x(11.5);
        for z in [b.zs(0), b.zp(0, 4)] {
            b.kit("kit.fuel_tank", YAW_0, false, At::Drop { x, z, under: b.deck(x) });
        }
    }
    let x = b.x(3.2);
    b.kit("kit.generator", YAW_0, false, At::Drop { x, z: b.zc(4), under: b.deck(x) });
    // Shafts and rudders.
    let x = b.x(1.2);
    for z in [b.zs(1), b.zp(1, 3)] {
        b.part("prop.small", YAW_0, At::Lift { x, z });
    }
    let x = b.x(0.1);
    for z in [b.zs(2), b.zp(2, 1)] {
        b.part("rudder.small", YAW_0, At::Lift { x, z });
    }
    // Berths beside the generators (in the narrow tutorial hull, in the empty tank space instead), pumps in the
    // engine room and the forward compartment.
    let (xa, xt) = (b.x(3.5), b.x(11.5));
    for (za, zt) in [(b.zs(2), b.zs(0)), (b.zp(2, 3), b.zp(0, 3))] {
        let (ua, ut) = (b.deck(xa), b.deck(xt));
        b.part_first("berths.crew", YAW_0, &[At::Drop { x: xa, z: za, under: ua }, At::Drop { x: xt, z: zt, under: ut }]);
    }
    for (xm, z) in [(10.5, h), (15.5, h)] {
        let x = b.x(xm);
        b.part("pump.small", YAW_0, At::Drop { x, z, under: b.deck(x) });
    }
    // Bridge.
    let x = b.x(11.2);
    b.part("console.bridge", YAW_0, At::Drop { x, z: b.zc(2), under: b.roof(x, h) });
    // Weapons: 76 mm forward, 30 mm aft, two light anti-ship launchers abaft the deckhouse.
    if !omit.gun {
        let x = b.x(18.0);
        let deck = b.deck(x);
        b.kit("kit.gun", YAW_0, false, At::Socket { name: "deck", target: CellPos::new(x, deck, h) });
    }
    b.kit("kit.gun.30mm", YAW_AFT, false, At::Drop { x: b.x(0.0), z: b.zc(3), under: sky });
    b.kit("kit.ashm.light", YAW_0, false, At::Drop { x: b.x(2.0), z: b.zc(6), under: sky });
    // Sensors: radar mast on the bridge roof, ESM and chaff beside it.
    if !omit.mast {
        b.kit("kit.radar_mast", YAW_0, false, At::Drop { x: b.x(9.6), z: b.zc(4), under: sky });
    }
    b.part("sensor.esm", YAW_0, At::Drop { x: b.x(8.6), z: b.zp(2, 2), under: sky });
    // Watertight doors through the bulkheads between the machinery spaces.
    for xm in [6.0, 11.0, 14.5] {
        b.door(b.x(xm), h - 1);
    }
    for z in [b.zs(2), b.zp(2, 2)] {
        b.part("decoy.chaff", YAW_0, At::Drop { x: b.x(12.5), z, under: sky });
    }
    b.part("funnel", YAW_0, At::Drop { x: b.x(6.8), z: b.zc(3), under: sky });
    b.hull_number("P 22", b.x(17.0), 2);
    b
}

pub fn all(cat: &Catalogue) -> Vec<Stock> {
    use content::enums::Act;
    let fac = |beam: f64, omit: FacOmit| fac22(cat, beam, omit).finish("Fast attack craft", "starter.fac22.name");
    let bare = FacOmit { gun: true, mast: true, drive: true };
    vec![
        Stock { path: "starters/patrol15.bp.ron", act: Act::Act1, bp: p15(cat).finish("Patrol boat", "starter.patrol15.name") },
        Stock { path: "starters/fac22.bp.ron", act: Act::Act1, bp: fac(1.0, FacOmit::default()) },
        Stock { path: "starters/insertion26.bp.ron", act: Act::Act1, bp: i26(cat).finish("Insertion craft", "starter.insertion26.name") },
        Stock { path: "starters/corvette40.bp.ron", act: Act::Act2, bp: k40(cat).finish("Missile corvette", "starter.corvette40.name") },
        Stock { path: "starters/corvette60.bp.ron", act: Act::Act3, bp: k60(cat).finish("Raiding corvette", "starter.corvette60.name") },
        Stock { path: "starters/destroyer90.bp.ron", act: Act::Act4, bp: d90(cat).finish("Destroyer", "starter.destroyer90.name") },
        Stock { path: "tutorial/fac22_tutorial.bp.ron", act: Act::Act1, bp: fac(TUTORIAL_BEAM, bare) },
        Stock { path: "tutorial/fac22_tutorial_float.bp.ron", act: Act::Act1, bp: fac(TUTORIAL_PASS_BEAM, bare) },
        Stock { path: "tutorial/fac22_tutorial_drive.bp.ron", act: Act::Act1, bp: fac(TUTORIAL_DRIVE_BEAM, FacOmit { gun: true, mast: true, drive: false }) },
        Stock { path: "tutorial/fac22_tutorial_nogun.bp.ron", act: Act::Act1, bp: fac(1.0, FacOmit { gun: true, mast: false, drive: false }) },
    ]
}
