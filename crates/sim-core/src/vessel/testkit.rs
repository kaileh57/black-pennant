//! Test fixtures: blueprints from hand-built cell grids and a live `Vessel` from a prefab.
use super::{Fidelity, Vessel, VesselPrefab, VesselStatus};
use crate::entity::EntityId;
use crate::math::{Aabb, Kinematics, Pose};
use crate::parts::VesselControls;
use crate::physics::BodyHandle;
use content::blueprint::{Blueprint, PartPlacement};
use content::cell::{CellGrid, CellPos};
use content::hull::{Hull, HullSource};
use content::ids::{PartId, SideId};

/// A freeform blueprint holding exactly these cells (no paint table edits, no parts).
pub fn blueprint(grid: &CellGrid, waterline_row: i16) -> Blueprint {
    let mut bp = content::blueprint::io::new_blueprint("test hull", 22.0);
    bp.hull = Hull { source: HullSource::Freeform, planes: Vec::new(), design_waterline_y: waterline_row };
    bp.cells = grid.to_rows();
    bp.header.hash = content::ids::ContentHash::EMPTY;
    bp
}

pub fn place(bp: &mut Blueprint, part: &str, pos: [i16; 3]) {
    bp.parts.push(PartPlacement { part: PartId::new(part), pos: CellPos::new(pos[0], pos[1], pos[2]), rot: Default::default(), kit: None, label: None, load: None });
}

/// A vessel spawned from a prefab at a pose, with dry mass and default crew.
pub fn vessel(prefab: &VesselPrefab, pose: Pose) -> Vessel {
    let crew = crate::crew::build_crew(prefab, &content::tuning::CrewTuning::default());
    Vessel {
        id: EntityId { index: 0, generation: 0 },
        side: SideId(0),
        player: None,
        class: None,
        blueprint: prefab.hash,
        name: prefab.name.clone(),
        status: VesselStatus::Active,
        fidelity: Fidelity::Full,
        grid: prefab.grid.clone(),
        parts: prefab.parts.clone(),
        compartments: prefab.compartments.clone(),
        networks: prefab.networks.clone(),
        crew,
        controls: VesselControls::default(),
        dry_mass: prefab.dry_mass,
        mass: prefab.dry_mass,
        body: BodyHandle::NONE,
        cuboids: prefab.cuboids.clone(),
        blocks: prefab.blocks.clone(),
        kin: Kinematics { pose, ..Default::default() },
        bounds: Aabb::default(),
        root_cell: prefab.root_cell,
        dims: prefab.dims,
        waterline_y: prefab.waterline_y,
        last_fired_tick: 0,
        capsize_ticks: 0,
        struck: false,
        pending_destroyed: Vec::new(),
        parent: None,
        spawn_tick: 0,
        submersible: false,
        tonnage_t: prefab.dry_mass.mass_kg / 1000.0,
    }
}
