//! Test support for the session units: embedded data, simulation dependencies, a test-basin `Sim` and one
//! shared sim pool (a pool per test would start three threads each).
use content::campaign::CampaignData;
use content::catalogue::Catalogue;
use sim_systems::protocol::config::SortieConfig;
use sim_systems::sim::SimDeps;
use sim_systems::Sim;
use std::sync::{Arc, OnceLock};

pub(crate) fn catalogue() -> Arc<Catalogue> {
    static C: OnceLock<Arc<Catalogue>> = OnceLock::new();
    C.get_or_init(|| Arc::new(content::catalogue::catalogue().clone())).clone()
}
pub(crate) fn campaign() -> Arc<CampaignData> {
    static C: OnceLock<Arc<CampaignData>> = OnceLock::new();
    C.get_or_init(|| Arc::new(content::campaign::load::campaign().clone())).clone()
}
pub(crate) fn deps() -> SimDeps {
    SimDeps { catalogue: catalogue(), campaign: campaign(), designs: Default::default(), scripts: Box::new(sim_systems::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() }
}
pub(crate) fn starter() -> content::blueprint::Blueprint { content::campaign::load::stock_blueprint("starters/fac22.bp.ron").expect("the 22 m starter loads") }
/// The splash basin with the 22 m starter and drone targets.
pub(crate) fn basin_config() -> SortieConfig {
    let options = content::settings::SplashOptions { water: content::weather::SeaState::CALM, target: content::settings::SplashTarget::Drones };
    sim_systems::editor::splash::splash_config(&starter(), options, 11, &campaign())
}
pub(crate) fn basin() -> Sim { Sim::new(basin_config(), deps()).expect("the basin starts") }
pub(crate) fn pool() -> Arc<rayon::ThreadPool> {
    static P: OnceLock<Arc<rayon::ThreadPool>> = OnceLock::new();
    P.get_or_init(super::host::sim_pool).clone()
}

/// Held by every test that steps a `Sim` on the shared pool. A step blocks its worker on the physics
/// engine's deferred job until another worker of the same pool runs it, so three steps at once on the
/// three-worker pool would wait on each other forever. The game never steps two Sims on one pool.
pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
