//! Contract-owned types (no work unit; data is authored by the `data-campaign-*` units, cross-checking by
//! `content-campaign`). Campaign data formats: theatre, regions, enemy classes, fleet templates, operations,
//! special-operations actions, aces, doctrine cards, economy and rules tables, difficulty presets and the
//! scope of each act. DESIGN.md L729-896.
//!
//! Acts 2 to 4 are data on shared systems: each act adds rows to `data/*/actN.ron`, never new formats.
pub mod act;
pub mod ace;
pub mod doctrine;
pub mod economy;
pub mod enemy;
pub mod load;
pub mod operation;
pub mod theatre;

pub use act::*;
pub use ace::*;
pub use doctrine::*;
pub use economy::*;
pub use enemy::*;
pub use operation::*;
pub use theatre::*;

/// All campaign data of one build, loaded and cross-checked.
#[derive(Clone, Debug, Default)]
pub struct CampaignData {
    pub theatre: TheatreDef,
    pub acts: Vec<ActDef>,
    pub classes: indexmap::IndexMap<crate::ids::ClassId, EnemyClassDef>,
    pub fleets: indexmap::IndexMap<crate::ids::FleetTemplateId, FleetTemplate>,
    pub operations: indexmap::IndexMap<crate::ids::OperationId, OperationDef>,
    pub aces: indexmap::IndexMap<crate::ids::AceId, AceDef>,
    pub cards: indexmap::IndexMap<crate::ids::CardId, DoctrineCardDef>,
    pub rules: CampaignRules,
    pub difficulty: Vec<DifficultyPreset>,
}
