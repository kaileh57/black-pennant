//! Unit `server-runtime`. Shared setup for every subcommand: embedded data, the 3-worker sim pool, SimDeps,
//! reading RON inputs and writing RON reports. DESIGN.md L1084 (the sim pool is exactly 3 workers; `Sim::step`
//! and `run_headless` run inside it), L899 (the same Sim everywhere).
use crate::error::CliError;
use content::campaign::CampaignData;
use content::catalogue::Catalogue;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sim_systems::SimDeps;
use std::path::Path;
use std::sync::Arc;

/// Embedded data loaded once per process.
#[derive(Clone)]
pub struct Data { pub catalogue: Arc<Catalogue>, pub campaign: Arc<CampaignData> }

/// Load the catalogue and campaign data embedded in the binary. Unit: `server-runtime`.
///
/// Fresh loads, not the process-wide `content::catalogue::catalogue()`, so bad embedded data comes back as a
/// [`CliError::Content`] naming the problem instead of a panic. The campaign keeps only the act rows this
/// build ships (`content::campaign::load::shipped`, Act 1 in 0.1.0, L2283), as the game client reads it.
pub fn load_data() -> Result<Data, CliError> {
    let catalogue = Catalogue::load_embedded().map_err(|e| CliError::Content(format!("catalogue: {e}")))?;
    let all = content::campaign::load::load_embedded(&catalogue).map_err(|e| CliError::Content(format!("campaign data: {e}")))?;
    let campaign = content::campaign::load::shipped(&all);
    Ok(Data { catalogue: Arc::new(catalogue), campaign: Arc::new(campaign) })
}

/// The sim pool: `rayon::ThreadPoolBuilder::new().num_threads(threads)` (3 in every gate run; tests may use 1),
/// threads named `sim-N`. Unit: `server-runtime`.
///
/// Zero threads is refused: rayon would read it as "one per core", and the gates need a fixed pool.
pub fn sim_pool(threads: u8) -> Result<Arc<rayon::ThreadPool>, CliError> {
    if threads == 0 { return Err(CliError::Config("the sim pool needs at least 1 thread".into())); }
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads as usize)
        .thread_name(|i| format!("sim-{i}"))
        .build()
        .map(Arc::new)
        .map_err(|e| CliError::Io(std::io::Error::other(format!("sim pool: {e}"))))
}

/// Fresh SimDeps for one Sim: shared data, empty design cache, `NullScriptHost` (the headless binary never
/// runs Luau until the `sys-script-luau` unit lands and the L4 guest gate passes), default budgets and ladder.
/// Unit: `server-runtime`.
pub fn sim_deps(data: &Data) -> SimDeps {
    SimDeps {
        catalogue: data.catalogue.clone(),
        campaign: data.campaign.clone(),
        designs: Default::default(),
        scripts: Box::new(sim_systems::script::NullScriptHost),
        guest_gate_passed: false,
        budgets: Default::default(),
        ladder: Default::default(),
    }
}

/// Inputs the binary reads are small (configs, briefs, blueprints); anything larger is refused before it is
/// read into memory.
const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;

fn read_text(path: &Path) -> Result<String, CliError> {
    let shown = path.display();
    let meta = std::fs::metadata(path).map_err(|e| CliError::Config(format!("{shown}: {e}")))?;
    if !meta.is_file() { return Err(CliError::Config(format!("{shown}: not a file"))); }
    if meta.len() > MAX_INPUT_BYTES { return Err(CliError::Config(format!("{shown}: {} bytes is over the {MAX_INPUT_BYTES}-byte input limit", meta.len()))); }
    let bytes = std::fs::read(path).map_err(|e| CliError::Config(format!("{shown}: {e}")))?;
    String::from_utf8(bytes).map_err(|_| CliError::Config(format!("{shown}: not UTF-8 text")))
}

/// Read a RON file with `content::data::ron_options`. Unit: `server-runtime`.
pub fn read_ron<T: DeserializeOwned>(path: &Path) -> Result<T, CliError> {
    let text = read_text(path)?;
    content::data::ron_options().from_str(&text).map_err(|e| CliError::Config(format!("{}: {e}", path.display())))
}

/// Read a blueprint file through `content::blueprint::io::load_blueprint` (migrations, limits). Unit: `server-runtime`.
///
/// Parts a migration removed without replacement are named on stderr; ruleset validation is the caller's.
pub fn read_blueprint(path: &Path) -> Result<content::blueprint::Blueprint, CliError> {
    let text = read_text(path)?;
    let file = path.display().to_string();
    let loaded = content::blueprint::io::load_blueprint(&text, &file).map_err(|e| CliError::Config(format!("{file}: {e}")))?;
    for removed in &loaded.removed_parts { eprintln!("note: {file}: removed part without replacement: {removed:?}"); }
    Ok(loaded.blueprint)
}

/// Write a pretty RON report to `path`, or to stdout when `None`. Unit: `server-runtime`.
///
/// Written through `content::data::ron_options`, so [`read_ron`] reads it back. A file is written whole to a
/// temporary sibling and renamed into place, so a reader never sees half a report.
pub fn write_report<T: Serialize>(report: &T, path: Option<&Path>) -> Result<(), CliError> {
    let mut text = report_text(report)?;
    text.push('\n');
    match path {
        None => {
            use std::io::Write;
            let mut out = std::io::stdout().lock();
            out.write_all(text.as_bytes())?;
            out.flush()?;
        }
        Some(path) => {
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) { std::fs::create_dir_all(dir)?; }
            let mut tmp = path.as_os_str().to_owned();
            tmp.push(".tmp");
            let tmp = std::path::PathBuf::from(tmp);
            std::fs::write(&tmp, text.as_bytes())?;
            if let Err(e) = std::fs::rename(&tmp, path) { let _ = std::fs::remove_file(&tmp); return Err(e.into()); }
        }
    }
    Ok(())
}

fn report_text<T: Serialize>(report: &T) -> Result<String, CliError> {
    content::data::ron_options()
        .to_string_pretty(report, ron::ser::PrettyConfig::default())
        .map_err(|e| CliError::Config(format!("report: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Report { name: String, value: Option<f64>, missing: Option<u32>, rows: Vec<(String, bool)> }

    fn scratch(test: &str, name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bp-server-runtime-{}-{test}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn report_round_trips() {
        let r = Report { name: "gate".into(), value: Some(1.5), missing: None, rows: vec![("a".into(), true)] };
        let path = scratch("round_trip", "sub/report.ron");
        write_report(&r, Some(&path)).unwrap();
        assert_eq!(read_ron::<Report>(&path).unwrap(), r);
        assert!(!path.with_extension("ron.tmp").exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn bad_inputs_are_errors_not_panics() {
        assert!(matches!(read_ron::<Report>(Path::new("/nonexistent/x.ron")), Err(CliError::Config(_))));
        let path = scratch("bad_inputs", "bad.ron");
        std::fs::write(&path, "(name: 3,").unwrap();
        assert!(matches!(read_ron::<Report>(&path), Err(CliError::Config(m)) if m.contains("bad.ron")));
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert!(matches!(read_blueprint(&path), Err(CliError::Config(_))));
        std::fs::write(&path, "not a blueprint").unwrap();
        assert!(matches!(read_blueprint(&path), Err(CliError::Config(_))));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_server_plays_only_the_shipped_acts() {
        let data = load_data().unwrap();
        let acts = |c: &CampaignData| c.acts.iter().map(|a| a.act).collect::<Vec<_>>();
        assert_eq!(acts(&data.campaign), acts(content::campaign::load::shipped_campaign()));
        assert!(!data.campaign.acts.is_empty() && data.campaign.acts.iter().all(content::campaign::load::act_ships));
    }

    #[test]
    fn pool_is_fixed_and_named() {
        assert!(sim_pool(0).is_err());
        let pool = sim_pool(3).unwrap();
        assert_eq!(pool.current_num_threads(), 3);
        assert_eq!(pool.install(|| std::thread::current().name().map(str::to_string)).as_deref().map(|n| n.starts_with("sim-")), Some(true));
    }
}
