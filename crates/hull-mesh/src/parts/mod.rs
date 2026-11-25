//! Contract-owned registry plus four units (`hm-parts-weapons` weapons.rs, `hm-parts-sensors` sensors.rs,
//! `hm-parts-fittings` fittings.rs, `hm-parts-bodies` bodies.rs). Procedural part and small-body models built
//! from parameters; nothing is modelled by hand. DESIGN.md L1233 (weapons, masts and a small detail set built
//! procedurally in hull-mesh from parameters), L1921 and L1945 (one MultiMesh per part type, shadowed near and
//! unshadowed far bands), L2154 (after 1.0, data-only part packs compose these generators inside the documented
//! parameter ranges), L1786-1799 (tokens; value before hue), L1953-1955 (emission ladder).
//!
//! # Output
//! A [`PartModel`] is a small articulated hierarchy: pieces with joints the client drives from replicated state
//! (`TurretView.train_rad` / `elevation_rad`, sensor `rpm`, propeller shaft power). One MultiMesh per (generator,
//! resolved params, piece) on the client.
//!
//! # Colour convention for part meshes
//! Vertex colour RGB is linear albedo; vertex colour A is the paint mask: 1 = multiply by the instance colour
//! (the owning ship's superstructure swatch, so mounts wear ship paint), 0 = fixed albedo (bores, rubber, glass,
//! bare metal). `custom0.x` is emission strength on the emission ladder (lamps <= 1, L1955); 0 elsewhere.
//! Albedos come from [`Tone`] only, which carries token Y values, so no part breaks the value-before-hue rule.
//!
//! # Frames
//! Part-local metres: origin at the footprint's minimum corner at rotation 0, +X bow, +Y up, +Z starboard. A
//! generator fits its model inside `footprint * 0.5 m` (masts and antennas may rise above it; underwater fittings
//! hang below y = 0). Trainable pieces point along +X at zero train and elevation.
//!
//! Sim-core does not define muzzle or antenna offsets beyond the footprint centre (sim-core `parts`), so
//! [`AnchorKind::Muzzle`] anchors are visual only.
pub mod bodies;
pub mod fittings;
pub mod sensors;
pub mod weapons;

use crate::types::{Aabb, MeshData};
use content::enums::Phase;
use content::ids::ModelId;
use content::part::weapons::{MunitionDef, MunitionKind};
use content::part::{PartDef, PartFunction, ProcModel, SensorKind, LauncherKind, DecoyKind};

/// Fixed albedo tones (linear grey Y from the world tokens, L1790-1799), with A = paint mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    /// Takes the ship's paint (multiplied by the instance colour). Base albedo 1.0.
    Paint,
    /// hull.deck, Y 0.05: deck-coloured fittings (heli deck, walkways).
    Deck,
    /// hull.black, Y 0.03: bores, muzzle ends, rubber, radome seams.
    Black,
    /// Bare gunmetal, Y 0.12.
    Metal,
    /// hull.pale, Y 0.55: radome GRP, liferaft canisters, missile bodies.
    Pale,
    /// Dark glossy glass, Y 0.03 (rendered with the glass material flag in custom0.y = 1).
    Glass,
    /// Primer, Y 0.25, warm (C <= 0.06), for exposed internals in cutaway (L1941).
    Primer,
    /// Lamp lens: emission 1.0 (custom0.x), albedo Y 0.55.
    Lamp,
}
impl Tone {
    /// Linear RGBA (A = paint mask). Neutral greys except `Primer` (slightly warm). Values are the token Y.
    pub fn rgba(self) -> [f32; 4] {
        match self {
            Tone::Paint => [1.0, 1.0, 1.0, 1.0],
            Tone::Deck => [0.05, 0.05, 0.05, 0.0],
            Tone::Black | Tone::Glass => [0.03, 0.03, 0.03, 0.0],
            Tone::Metal => [0.12, 0.12, 0.12, 0.0],
            Tone::Pale | Tone::Lamp => [0.55, 0.55, 0.55, 0.0],
            Tone::Primer => [0.27, 0.245, 0.225, 0.0],
        }
    }
}

/// What a piece's joint does. Rotations are about the piece's pivot in its parent's frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Joint {
    Fixed,
    /// Rotates about +Y by the mount's train angle (`TurretView.train_rad`).
    Train,
    /// Rotates about +Z by the mount's elevation (`TurretView.elevation_rad`); child of a `Train` piece.
    Elevate,
    /// Spins about local +Y at the sensor's `rpm` (rotating radars, L412) or, for propellers, about +X at a
    /// rate the client derives from shaft power.
    Spin { axis: [f32; 3] },
    /// Slides back along -X by up to `travel_m` on firing (recoil), client-animated from `MountFired`.
    Recoil { travel_m: f32 },
}

/// Semantic role of a piece, so the client can find the parts it animates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PieceRole { Base, Turret, Cradle, Barrel, Antenna, Rotor, Propeller, Arm, Door, Body, Detail }

#[derive(Clone, Debug, PartialEq)]
pub struct PartPiece {
    pub role: PieceRole,
    /// Index of the parent piece in [`PartModel::pieces`]; parents always precede children.
    pub parent: Option<u8>,
    /// Pivot in the parent's frame (part-local for roots); the mesh is relative to the pivot.
    pub pivot: [f32; 3],
    pub joint: Joint,
    pub mesh: MeshData,
}

/// Points the client hangs effects on, in the frame of `piece` (relative to its pivot).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AnchorKind {
    /// Muzzle star and smoke (L1968); one per barrel.
    Muzzle,
    /// Launch cell or tube mouth: missile trail start, launch smoke.
    LaunchCell,
    /// Funnel or engine exhaust (Layer B mist / smoke).
    Exhaust,
    /// Searchlight or navigation light; the client adds a light within the VFX light budget.
    Light,
    /// Propulsor wash for wake and pump effects.
    Wash,
    /// Water outflow of a pump (flooding pump arcs, L1969).
    PumpOutlet,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor { pub kind: AnchorKind, pub piece: u8, pub pos: [f32; 3], pub dir: [f32; 3] }

/// An articulated procedural model.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PartModel {
    pub pieces: Vec<PartPiece>,
    pub anchors: Vec<Anchor>,
    /// Bounds at rest in part-local metres.
    pub aabb: Aabb,
}

/// One documented generator parameter (L2154: packs stay inside these ranges).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamSpec { pub name: &'static str, pub unit: &'static str, pub min: f32, pub max: f32, pub default: f32, pub integer: bool }
const fn p(name: &'static str, unit: &'static str, min: f32, max: f32, default: f32, integer: bool) -> ParamSpec { ParamSpec { name, unit, min, max, default, integer } }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family { Weapons, Sensors, Fittings }

/// Every part generator. `key` is the [`ModelId`] string used in the catalogue's `ProcModel.generator`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Generator {
    // Weapons (`hm-parts-weapons`).
    GunTurret, GunOpen, Ciws, LauncherBox, LauncherVls, TorpedoTubes, DepthChargeRack, DecoyLauncher, MineRail,
    // Sensors and masts (`hm-parts-sensors`).
    MastPole, MastLattice, MastTripod, RadarSearch, RadarNav, RadarFireControl, Director, Esm, SonarDome, Camera, Jammer,
    // Fittings and details (`hm-parts-fittings`).
    Funnel, Propeller, Waterjet, Rudder, Davit, Crane, HeliDeck, DroneBay, RecoveryNet, Searchlight, Railing, Bollard,
    Liferaft, Vent, Anchor, Door, Internal, Box,
}

/// Static description of a generator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeneratorSpec { pub generator: Generator, pub key: &'static str, pub family: Family, pub phase: Phase, pub params: &'static [ParamSpec] }

macro_rules! spec { ($g:ident, $k:literal, $f:ident, $ph:ident, [$($p:expr),* $(,)?]) => { GeneratorSpec { generator: Generator::$g, key: $k, family: Family::$f, phase: Phase::$ph, params: &[$($p),*] } }; }

/// The registry, in `Generator` order. Ranges are this contract's choice (the spec sets none); data units and
/// later part packs must stay inside them.
pub const GENERATORS: &[GeneratorSpec] = &[
    spec!(GunTurret, "gun.turret", Weapons, Slice, [p("calibre", "mm", 20.0, 155.0, 76.0, true), p("barrels", "", 1.0, 3.0, 1.0, true), p("shield", "bool", 0.0, 1.0, 1.0, true)]),
    spec!(GunOpen, "gun.open", Weapons, Slice, [p("calibre", "mm", 12.0, 57.0, 30.0, true), p("barrels", "", 1.0, 2.0, 1.0, true)]),
    spec!(Ciws, "gun.ciws", Weapons, Slice, [p("calibre", "mm", 20.0, 35.0, 30.0, true), p("barrels", "", 4.0, 7.0, 6.0, true), p("radome", "bool", 0.0, 1.0, 1.0, true)]),
    spec!(LauncherBox, "launcher.box", Weapons, Slice, [p("cells", "", 1.0, 8.0, 4.0, true), p("cell_length", "m", 2.0, 8.0, 6.0, false), p("cell_diameter", "m", 0.3, 1.0, 0.6, false), p("trainable", "bool", 0.0, 1.0, 0.0, true), p("elevation", "deg", 0.0, 45.0, 20.0, false)]),
    spec!(LauncherVls, "launcher.vls", Weapons, R010, [p("columns", "", 1.0, 8.0, 2.0, true), p("rows", "", 1.0, 8.0, 4.0, true), p("cell", "m", 0.5, 1.0, 0.6, false)]),
    spec!(TorpedoTubes, "launcher.torpedo", Weapons, Slice, [p("tubes", "", 1.0, 4.0, 2.0, true), p("length", "m", 3.0, 8.0, 6.5, false), p("diameter", "m", 0.3, 0.6, 0.53, false), p("trainable", "bool", 0.0, 1.0, 1.0, true)]),
    spec!(DepthChargeRack, "launcher.dc_rack", Weapons, Act3, [p("charges", "", 2.0, 12.0, 6.0, true)]),
    spec!(DecoyLauncher, "launcher.decoy", Weapons, Slice, [p("barrels", "", 2.0, 12.0, 6.0, true), p("elevation", "deg", 30.0, 60.0, 45.0, false)]),
    spec!(MineRail, "launcher.mine_rail", Weapons, R010, [p("length", "cells", 4.0, 40.0, 12.0, true)]),
    spec!(MastPole, "mast.pole", Sensors, Slice, [p("height", "m", 2.0, 30.0, 8.0, false), p("yards", "", 0.0, 3.0, 1.0, true)]),
    spec!(MastLattice, "mast.lattice", Sensors, Slice, [p("height", "m", 4.0, 40.0, 15.0, false), p("base", "m", 1.0, 6.0, 3.0, false), p("platforms", "", 0.0, 3.0, 1.0, true)]),
    spec!(MastTripod, "mast.tripod", Sensors, R010, [p("height", "m", 4.0, 30.0, 12.0, false), p("spread", "m", 1.0, 6.0, 3.0, false)]),
    spec!(RadarSearch, "radar.search", Sensors, Slice, [p("width", "m", 1.0, 8.0, 3.0, false), p("height", "m", 0.3, 3.0, 1.0, false), p("planar", "bool", 0.0, 1.0, 0.0, true)]),
    spec!(RadarNav, "radar.nav", Sensors, Slice, [p("length", "m", 0.8, 4.0, 2.0, false)]),
    spec!(RadarFireControl, "radar.fc", Sensors, Slice, [p("dish", "m", 0.5, 3.0, 1.2, false)]),
    spec!(Director, "sensor.director", Sensors, Slice, [p("size", "m", 1.0, 4.0, 2.0, false)]),
    spec!(Esm, "sensor.esm", Sensors, Slice, [p("domes", "", 1.0, 4.0, 2.0, true)]),
    spec!(SonarDome, "sensor.sonar_dome", Sensors, R010, [p("length", "m", 1.0, 6.0, 3.0, false)]),
    spec!(Camera, "sensor.camera", Sensors, Slice, [p("head", "m", 0.3, 1.0, 0.5, false), p("mast", "m", 0.0, 10.0, 3.0, false)]),
    spec!(Jammer, "sensor.jammer", Sensors, R010, [p("panels", "", 1.0, 4.0, 2.0, true)]),
    spec!(Funnel, "fit.funnel", Fittings, Slice, [p("height", "m", 1.0, 8.0, 3.0, false), p("width", "m", 0.5, 4.0, 1.5, false), p("length", "m", 0.5, 6.0, 2.0, false), p("rake", "deg", 0.0, 20.0, 5.0, false), p("cap", "bool", 0.0, 1.0, 1.0, true)]),
    spec!(Propeller, "fit.propeller", Fittings, Slice, [p("diameter", "m", 0.5, 4.0, 1.5, false), p("blades", "", 3.0, 7.0, 4.0, true)]),
    spec!(Waterjet, "fit.waterjet", Fittings, Slice, [p("diameter", "m", 0.3, 2.0, 0.8, false)]),
    spec!(Rudder, "fit.rudder", Fittings, Slice, [p("height", "m", 0.5, 4.0, 1.5, false), p("chord", "m", 0.3, 3.0, 1.0, false)]),
    spec!(Davit, "fit.davit", Fittings, R010, [p("boat", "m", 0.0, 9.0, 6.0, false)]),
    spec!(Crane, "fit.crane", Fittings, R010, [p("reach", "m", 2.0, 12.0, 6.0, false)]),
    spec!(HeliDeck, "fit.heli_deck", Fittings, Act2, [p("length", "m", 10.0, 25.0, 15.0, false), p("width", "m", 8.0, 20.0, 12.0, false)]),
    spec!(DroneBay, "fit.drone_bay", Fittings, Slice, [p("doors", "", 1.0, 2.0, 1.0, true)]),
    spec!(RecoveryNet, "fit.recovery_net", Fittings, R010, [p("width", "m", 2.0, 8.0, 4.0, false)]),
    spec!(Searchlight, "fit.searchlight", Fittings, Slice, [p("diameter", "m", 0.3, 1.2, 0.6, false)]),
    spec!(Railing, "fit.railing", Fittings, Slice, [p("length", "m", 0.5, 16.0, 4.0, false)]),
    spec!(Bollard, "fit.bollard", Fittings, Slice, [p("pair", "", 1.0, 2.0, 2.0, true)]),
    spec!(Liferaft, "fit.liferaft", Fittings, Slice, [p("count", "", 1.0, 6.0, 2.0, true)]),
    spec!(Vent, "fit.vent", Fittings, Slice, [p("height", "m", 0.5, 2.0, 1.0, false)]),
    spec!(Anchor, "fit.anchor", Fittings, Slice, [p("size", "m", 0.3, 1.5, 0.6, false)]),
    spec!(Door, "fit.door", Fittings, Slice, [p("hatch", "bool", 0.0, 1.0, 0.0, true)]),
    spec!(Internal, "fit.internal", Fittings, Slice, [p("tone", "", 0.0, 3.0, 0.0, true)]),
    spec!(Box, "fit.box", Fittings, Slice, []),
];

impl Generator {
    pub fn spec(self) -> &'static GeneratorSpec { &GENERATORS[self as usize] }
    pub fn key(self) -> &'static str { self.spec().key }
    pub fn from_key(key: &str) -> Option<Generator> { GENERATORS.iter().find(|s| s.key == key).map(|s| s.generator) }
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ModelError {
    #[error("unknown model generator `{0}`")]
    UnknownGenerator(String),
    #[error("generator `{key}` takes {expected} parameters, got {got}")]
    ParamCount { key: &'static str, expected: usize, got: usize },
    #[error("generator `{key}` parameter `{name}` = {value} outside {min}..={max}")]
    ParamRange { key: &'static str, name: &'static str, value: f32, min: f32, max: f32 },
}

/// Fill missing trailing parameters with defaults, round integer parameters, and reject values outside the
/// documented ranges (packs must not exceed them, L2154).
pub fn resolve_params(generator: Generator, params: &[f32]) -> Result<Vec<f32>, ModelError> {
    let spec = generator.spec();
    if params.len() > spec.params.len() { return Err(ModelError::ParamCount { key: spec.key, expected: spec.params.len(), got: params.len() }); }
    let mut out = Vec::with_capacity(spec.params.len());
    for (i, ps) in spec.params.iter().enumerate() {
        let mut v = params.get(i).copied().unwrap_or(ps.default);
        if ps.integer { v = libm::roundf(v); }
        if !(v >= ps.min && v <= ps.max) { return Err(ModelError::ParamRange { key: spec.key, name: ps.name, value: v, min: ps.min, max: ps.max }); }
        out.push(v);
    }
    Ok(out)
}

/// Build a part's model. An empty generator key takes [`default_model`] for the part. `footprint` is the
/// part's size in cells at rotation 0.
pub fn build_part_model(def: &PartDef) -> Result<PartModel, ModelError> {
    let model = if def.model.generator.as_str().is_empty() { default_model(def) } else { def.model.clone() };
    build_model(&model, def.size)
}

/// Build a model from a generator reference and footprint.
pub fn build_model(model: &ProcModel, footprint: [u8; 3]) -> Result<PartModel, ModelError> {
    let g = Generator::from_key(model.generator.as_str()).ok_or_else(|| ModelError::UnknownGenerator(model.generator.0.clone()))?;
    let params = resolve_params(g, &model.params)?;
    Ok(match g.spec().family {
        Family::Weapons => weapons::build(g, &params, footprint),
        Family::Sensors => sensors::build(g, &params, footprint),
        Family::Fittings => fittings::build(g, &params, footprint),
    })
}

/// The generator a part uses when its catalogue row names none, from its function. Parameters are left empty
/// (defaults); gun calibre and barrels, launcher cells and tubes, decoy rounds per salvo come from the function,
/// clamped into the generator's ranges, so `build_model(&default_model(def), def.size)` never returns `Err`.
pub fn default_model(def: &PartDef) -> ProcModel {
    let (g, params): (Generator, Vec<f32>) = match &def.function {
        PartFunction::Gun(g) if g.point_defence => (Generator::Ciws, vec![g.calibre_mm as f32]),
        PartFunction::Gun(g) if g.calibre_mm <= 40 => (Generator::GunOpen, vec![g.calibre_mm as f32, g.barrels as f32]),
        PartFunction::Gun(g) => (Generator::GunTurret, vec![g.calibre_mm as f32, g.barrels as f32]),
        PartFunction::Launcher(l) => match l.kind {
            LauncherKind::Missile if l.train_rate_dps.is_none() && l.cells > 4 => (Generator::LauncherVls, vec![]),
            LauncherKind::Missile => (Generator::LauncherBox, vec![l.cells.min(8) as f32]),
            LauncherKind::TorpedoTube => (Generator::TorpedoTubes, vec![l.cells.clamp(1, 4) as f32]),
            LauncherKind::DepthChargeRack => (Generator::DepthChargeRack, vec![]),
        },
        PartFunction::Decoy(d) if d.kind == DecoyKind::TowedDecoy => (Generator::Box, vec![]),
        PartFunction::Decoy(d) => (Generator::DecoyLauncher, vec![(d.salvo as f32).clamp(2.0, 12.0)]),
        PartFunction::MineRail { .. } => (Generator::MineRail, vec![]),
        PartFunction::Jammer { .. } => (Generator::Jammer, vec![]),
        PartFunction::Sensor(s) => match s.kind {
            SensorKind::SearchRadar => (Generator::RadarSearch, vec![]),
            SensorKind::NavRadar => (Generator::RadarNav, vec![]),
            SensorKind::FireControlRadar | SensorKind::Seeker => (Generator::RadarFireControl, vec![]),
            SensorKind::Esm => (Generator::Esm, vec![]),
            SensorKind::PassiveSonar | SensorKind::ActiveSonar => (Generator::SonarDome, vec![]),
            SensorKind::Visual | SensorKind::Camera => (Generator::Camera, vec![]),
        },
        PartFunction::CameraMast { .. } => (Generator::Camera, vec![]),
        PartFunction::Director => (Generator::Director, vec![]),
        PartFunction::Propulsor(pd) => match pd.kind {
            content::part::PropulsorKind::Propeller => (Generator::Propeller, vec![]),
            content::part::PropulsorKind::Waterjet => (Generator::Waterjet, vec![]),
        },
        PartFunction::Rudder { .. } => (Generator::Rudder, vec![]),
        PartFunction::RhibDavit | PartFunction::BoardingKit { .. } => (Generator::Davit, vec![]),
        PartFunction::Crane => (Generator::Crane, vec![]),
        PartFunction::HeliDeck { .. } => (Generator::HeliDeck, vec![]),
        PartFunction::DroneBay { .. } => (Generator::DroneBay, vec![]),
        PartFunction::RecoveryNet => (Generator::RecoveryNet, vec![]),
        PartFunction::Door => (Generator::Door, vec![]),
        PartFunction::Engine(_) | PartFunction::Generator { .. } | PartFunction::Battery { .. } | PartFunction::Pump { .. } => (Generator::Internal, vec![0.0]),
        PartFunction::FuelTank { .. } | PartFunction::Ballast { .. } => (Generator::Internal, vec![1.0]),
        PartFunction::Magazine { .. } | PartFunction::Hoist { .. } => (Generator::Internal, vec![2.0]),
        PartFunction::Console | PartFunction::Berths { .. } | PartFunction::PassengerBerths { .. } => (Generator::Internal, vec![3.0]),
        PartFunction::Structure => (Generator::Box, vec![]),
    };
    // Clamp derived values into the generator's documented ranges so this fallback never fails validation.
    let params = params.iter().zip(g.spec().params).map(|(v, ps)| v.clamp(ps.min, ps.max)).collect();
    ProcModel { generator: ModelId::new(g.key()), params }
}

/// Small-body generator keys (`MunitionDef.model`); bodies take their dimensions from the munition row, not from
/// parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BodyGenerator { Missile, Torpedo, Mine, DepthCharge, Drone, DroneRotor, Aircraft, Helicopter, Rhib, TowedDecoy }
impl BodyGenerator {
    pub const ALL: [BodyGenerator; 10] = [Self::Missile, Self::Torpedo, Self::Mine, Self::DepthCharge, Self::Drone, Self::DroneRotor, Self::Aircraft, Self::Helicopter, Self::Rhib, Self::TowedDecoy];
    pub fn key(self) -> &'static str {
        match self { Self::Missile => "body.missile", Self::Torpedo => "body.torpedo", Self::Mine => "body.mine", Self::DepthCharge => "body.depth_charge", Self::Drone => "body.drone", Self::DroneRotor => "body.drone_rotor", Self::Aircraft => "body.aircraft", Self::Helicopter => "body.helicopter", Self::Rhib => "body.rhib", Self::TowedDecoy => "body.towed_decoy" }
    }
    pub fn from_key(key: &str) -> Option<Self> { Self::ALL.iter().copied().find(|g| g.key() == key) }
    /// Default generator per munition kind when the row names none.
    pub fn for_kind(kind: MunitionKind) -> Self {
        match kind {
            MunitionKind::AntiShipMissile | MunitionKind::Sam | MunitionKind::PdMissile => Self::Missile,
            MunitionKind::Torpedo => Self::Torpedo, MunitionKind::Mine => Self::Mine, MunitionKind::DepthCharge => Self::DepthCharge,
            MunitionKind::Drone => Self::Drone, MunitionKind::Aircraft => Self::Aircraft, MunitionKind::Helicopter => Self::Helicopter, MunitionKind::Rhib => Self::Rhib,
        }
    }
}

/// Build a small body's model (missiles, torpedoes, mines, depth charges, drones, aircraft, helicopters, RHIBs).
/// Body-local frame: centre of mass at the origin, nose along +X, up +Y.
pub fn build_body_model(def: &MunitionDef) -> Result<PartModel, ModelError> {
    let g = if def.model.as_str().is_empty() { BodyGenerator::for_kind(def.kind) } else { BodyGenerator::from_key(def.model.as_str()).ok_or_else(|| ModelError::UnknownGenerator(def.model.0.clone()))? };
    Ok(bodies::build(g, def))
}
