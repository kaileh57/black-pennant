//! Test-only builders shared by the gauge, notes and suggest tests: a hand-made catalogue, a freeform box
//! hull blueprint, and a prefab and design built from landed pieces only (grid, compartments), so no test
//! reaches a physics stub.
use crate::prelude::*;
use crate::ship::{ShipDesign, TurnModel};
use content::blueprint::{Blueprint, PartPlacement};
use content::catalogue::MaterialDef;
use content::cell::{ArmourTier, Cell, CellGrid, CellPos, CellShape, Material, CELL_M};
use content::hull::{Hull, HullSource};
use content::kit::PartRot;
use content::part::weapons::CalibreBand;
use content::part::{EngineDef, EngineKind, GunDef, PartCategory, PartCategory as C, PartDef, PartFunction, PartFunction as F, Placement, Placement as P, PortDef, PortRole, ProcModel, SensorDef, SensorKind};
use sim_core::compartments::CompartmentSet;
use sim_core::grid::VesselGrid;
use sim_core::parts::{PartRuntime, PartState};
use sim_core::vessel::VesselPrefab;

pub(crate) fn def(id: &str, category: PartCategory, size: [u8; 3], placement: Placement, function: PartFunction, ports: Vec<PortDef>) -> PartDef {
    PartDef {
        id: PartId::new(id), name: TextKey::new(format!("part.{id}")), description: TextKey::new("d"), category, tier: Default::default(),
        phase: Default::default(), size, placement, mass_kg: 500.0, cost: 10, ports, power_priority: 0, buffer_s: None,
        watertight: false, flammable: content::part::Flammable::No, rcs_factor: 1.0, tags: vec![], function,
        model: ProcModel::default(), icon: Default::default(), replaced_by: None, standard_limit: None,
    }
}
pub(crate) fn port(role: PortRole, rate: f64) -> PortDef { PortDef { network: NetworkKind::Power, role, rate, capacity: 0.0 } }

pub(crate) fn steel() -> MaterialDef { MaterialDef { material: Material::Steel, cell_mass_kg: 40.0, armour_mass_kg: [10.0, 30.0, 80.0, 160.0], cell_cost: 1.0, armour_cost: [1.0, 2.0, 4.0, 8.0], rcs_factor: 1.0, ir_factor: 1.0 } }

pub(crate) fn catalogue(defs: Vec<PartDef>) -> Catalogue {
    let mut c = Catalogue::default();
    for d in defs { c.parts.insert(d.id.clone(), d); }
    c.parts.sort_keys();
    c.materials.push(steel());
    c
}

/// A closed steel box: `len` x `height` x `beam` cells, bottom, sides, ends and a deck at the top row,
/// freeform, design waterline at `wl`.
pub(crate) fn box_blueprint(len: i16, height: i16, beam: i16, wl: i16) -> Blueprint {
    let mut g = CellGrid::new([len as u16, height as u16, beam as u16]);
    let s = Cell { material: Material::Steel, armour: ArmourTier::None, paint: 2, shape: CellShape::CUBE };
    for x in 0..len { for y in 0..height { for z in 0..beam {
        if x == 0 || x == len - 1 || y == 0 || y == height - 1 || z == 0 || z == beam - 1 { g.set(CellPos::new(x, y, z), s); }
    }}}
    let mut bp = content::blueprint::io::new_blueprint("Test", 20.0);
    bp.hull = Hull { source: HullSource::Freeform, planes: Vec::new(), design_waterline_y: wl };
    bp.cells = g.to_rows();
    bp.header.hash = content::blueprint::io::compute_hash(&bp);
    bp
}

pub(crate) fn place(bp: &mut Blueprint, id: &str, pos: CellPos) -> PartSlot {
    bp.parts.push(PartPlacement { part: PartId::new(id), pos, rot: PartRot::default(), kit: None, label: None, load: None });
    PartSlot(bp.parts.len() as u16 - 1)
}

/// A prefab from landed pieces: grid, parts, compartments. No networks, mass or buoyancy blocks.
pub(crate) fn prefab(bp: &Blueprint, cat: &Catalogue) -> VesselPrefab {
    let design = Arc::new(content::blueprint::io::build_grid(bp, cat));
    let mut parts = Vec::new();
    let mut owned = Vec::new();
    let mut berths = 0u16;
    for (i, p) in bp.parts.iter().enumerate() {
        let def = cat.part(&p.part).expect("test part");
        let cells = crate::editor::notes::edit::part_cells(p, cat).expect("footprint");
        let idx: Vec<CellIndex> = cells.iter().map(|c| design.index(*c).expect("in grid")).collect();
        let centre = cells.iter().fold(DVec3::ZERO, |s, c| s + DVec3::from_array(c.centre_m())) / cells.len() as f64;
        let state = match def.function {
            PartFunction::FuelTank { litres } => PartState::FuelTank { litres: litres * p.load.unwrap_or(1.0) },
            PartFunction::Ballast { max_kg } => PartState::Ballast { kg: p.load.unwrap_or(max_kg) },
            _ => PartState::Inert,
        };
        if let PartFunction::Berths { count } = def.function { berths += count as u16; }
        parts.push(PartRuntime { slot: PartSlot(i as u16), def: cat.part_idx(&p.part).expect("idx"), pos: p.pos, rot: p.rot, cells: idx.clone(), centre, compartment: None, health: 1.0, level: Default::default(), links: Default::default(), priority: 0, state });
        owned.push((PartSlot(i as u16), idx));
    }
    let flags = |s: PartSlot| { let d = cat.part(&bp.parts[s.index()].part).expect("def"); (d.watertight, d.flammable) };
    let grid = VesselGrid::build(design.clone(), &owned, &flags);
    let (mut compartments, _) = CompartmentSet::derive(&grid, &parts);
    compartments.set_waterline(bp.hull.design_waterline_y as f64 * CELL_M, cat.tuning.flooding.valve_m2);
    compartments.link_parts(&mut parts);
    let size = design.size;
    VesselPrefab {
        hash: bp.header.hash, name: bp.header.name.clone(), grid, parts, compartments, networks: Default::default(), dry_mass: Default::default(),
        cuboids: Vec::new(), blocks: Default::default(), root_cell: CellIndex(0), berths, waterline_y: bp.hull.design_waterline_y as f64 * CELL_M,
        dims: [size[0] as f64 * CELL_M, size[1] as f64 * CELL_M, size[2] as f64 * CELL_M], work_counts: [0; 4],
    }
}

/// A design around a blueprint's prefab with no mounts and an empty compiled graph.
pub(crate) fn design(bp: &Blueprint, prefab: VesselPrefab) -> ShipDesign {
    let hash = prefab.hash;
    ShipDesign {
        hash, prefab: Arc::new(prefab), settings: Default::default(), rules: Vec::new(), overrides: Default::default(), graph: Arc::new(Default::default()),
        mounts: Vec::new(), emitters: Vec::new(), sensor_slots: Vec::new(), consumer_slots: Vec::new(), decoys: Vec::new(), pumps: Vec::new(), doors: Vec::new(),
        bays: Vec::new(), range_tables: BTreeMap::new(), turn: TurnModel::default(), cost: 0, tonnage_t: 100.0,
        blueprint: Arc::new(bp.clone()), compiled_under: (Default::default(), false), auto_limits: BTreeMap::new(),
    }
}

pub(crate) fn power_catalogue() -> Catalogue {
    catalogue(vec![
        def("gen", C::Power, [2, 2, 2], P::Internal, F::Generator { kw: 100.0, fuel_lps: 0.01 }, vec![]),
        def("diesel", C::Propulsion, [2, 2, 2], P::Internal, F::Engine(EngineDef { kind: EngineKind::Diesel, shaft_kw: 1_000.0, fuel_lps: 0.06, spool_s: 5.0 }), vec![]),
        def("radar", C::Sensor, [1, 1, 1], P::Weather, F::Sensor(SensorDef { kind: SensorKind::SearchRadar, r_ref_m: 20_000.0, rate_hz: 5.0, rpm: Some(24.0), fov_half_deg: 180.0, emitter: String::new() }), vec![port(PortRole::Consume, 20.0)]),
        def("gun", C::Gun, [1, 1, 1], P::Weather, F::Gun(GunDef { calibre_mm: 76, band: CalibreBand::Medium, barrels: 2, rof_rpm: 60.0, muzzle_mps: 900.0, shell_kg: 6.0, train_rate_dps: 30.0, elevation_rate_dps: 30.0, elevation_min_deg: -10.0, elevation_max_deg: 85.0, dispersion_mrad: 1.0, point_defence: false, ready_rounds: 10 }), vec![port(PortRole::Consume, 30.0)]),
        def("tank", C::Fuel, [2, 1, 2], P::Internal, F::FuelTank { litres: 2_000.0 }, vec![]),
        def("hoist", C::Gun, [1, 1, 1], P::Internal, F::Hoist { rounds_per_min: 40.0 }, vec![]),
        def("funnel", C::Hull, [2, 6, 2], P::Weather, F::Structure, vec![]),
        def("berths", C::Crew, [2, 1, 2], P::Internal, F::Berths { count: 8 }, vec![]),
        def("bridge", C::Control, [1, 1, 1], P::Weather, F::Console, vec![]),
        def("ballast", C::Hull, [2, 1, 2], P::Internal, F::Ballast { max_kg: 4_000.0 }, vec![]),
    ])
}

/// A 20 m box with a generator, an engine, a radar, a gun over its hoist and a half-full tank.
pub(crate) fn fitted() -> (content::blueprint::Blueprint, Catalogue) {
    let cat = power_catalogue();
    let mut bp = box_blueprint(40, 8, 12, 3);
    place(&mut bp, "gen", CellPos::new(5, 1, 5));
    place(&mut bp, "diesel", CellPos::new(8, 1, 5));
    place(&mut bp, "radar", CellPos::new(20, 8, 6));
    place(&mut bp, "gun", CellPos::new(30, 8, 6));
    let t = place(&mut bp, "tank", CellPos::new(12, 1, 5));
    bp.parts[t.index()].load = Some(0.5);
    place(&mut bp, "hoist", CellPos::new(30, 6, 6));
    (bp, cat)
}

