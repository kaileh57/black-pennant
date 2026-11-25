//! Every stock ship (starters, Combine classes, reference setups, tutorial hulls) routes all three networks to
//! every consumer it carries, as sim-core lays them for a sortie (DESIGN.md L281-299). A consumer the router
//! cannot reach runs on its 30 s battery and then goes dark: a Combine corvette whose main gun, or a patrol
//! boat whose rudders, stop half a minute into the fight.
//!
//! This is a cross-unit check: the parts data (which parts are watertight), the Combine and starter build
//! sheets (where kits pierce decks and bulkheads) and the sim-core router all decide it, and no unit test in
//! one of them can see the others.
use content::catalogue::Catalogue;
use sim_core::networks::unconnected_consumers;
use sim_core::vessel::build_prefab;

/// Tutorial hulls that are unfinished on purpose: the player fits their engine and fuel tank (levels 0 and 1),
/// so their generators have no fuel source yet.
const UNFINISHED: [&str; 2] = ["tutorial/fac22_tutorial.bp.ron", "tutorial/fac22_tutorial_float.bp.ron"];

#[test]
fn every_stock_ship_reaches_every_consumer() {
    let cat = Catalogue::load_embedded().expect("catalogue");
    let mut failures = Vec::new();
    let mut checked = 0;
    for (path, _) in content::data::stock_blueprint_files() {
        if path.starts_with("examples/") || UNFINISHED.contains(&path) { continue; }
        let bp = content::campaign::load::stock_blueprint(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let prefab = build_prefab(&bp, &cat);
        checked += 1;
        let cut = unconnected_consumers(&prefab.networks, &prefab.parts, &prefab.compartments, &cat);
        if cut.is_empty() { continue; }
        let names: Vec<String> = cut
            .iter()
            .filter_map(|s| prefab.parts.iter().find(|p| p.slot == *s))
            .map(|p| format!("{} {}", p.slot.0, cat.parts.get_index(p.def.index()).map_or("?", |(k, _)| k.as_str())))
            .collect();
        failures.push(format!("{path}: {}", names.join(", ")));
    }
    assert!(checked >= 60, "{checked} stock ships");
    assert!(failures.is_empty(), "consumers off the networks:\n{}", failures.join("\n"));
}
