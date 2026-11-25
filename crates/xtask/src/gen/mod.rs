//! Contract-owned, implemented driver for `cargo xtask gen`. DESIGN.md L1076-1078.
//!
//! A generator is a pure function from the repository (read-only) to a list of files. The driver runs each
//! generator twice and fails if the two outputs differ by a byte (determinism), then writes the files under
//! `game/` (only when the bytes changed) or, with `--check`, compares them with what is on disk. Generators
//! never write files themselves. Output paths must lie under `shaders/gen/`, `art/gen/` or `audio/gen/`.
pub mod art;
pub mod basic;
pub mod fft;

use std::path::Path;

/// One generated file: path relative to `game/` and its exact bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenFile { pub path: String, pub bytes: Vec<u8> }

pub type GenFn = fn(&Path) -> Result<Vec<GenFile>, String>;

/// A registered generator.
pub struct Generator { pub id: &'static str, pub spec: &'static str, pub run: GenFn }

/// Every generator, in run order.
pub const GENERATORS: &[Generator] = &[
    Generator { id: "waves", spec: "L1891 wave shader include", run: basic::waves },
    Generator { id: "audio", spec: "L1239-1247 rendered audio", run: basic::audio },
    Generator { id: "ocean-detail", spec: "L1903 baked FFT detail", run: fft::ocean_detail },
    Generator { id: "luts", spec: "L1788, L1846 colour-grading LUTs", run: art::luts },
    Generator { id: "ramps", spec: "L1788, L1953 VFX colour ramps", run: art::ramps },
    Generator { id: "noise", spec: "L1788, L1905, L1940 noise textures", run: art::noise },
];

const GEN_PREFIXES: &[&str] = &["shaders/gen/", "art/gen/", "audio/gen/"];

/// Run every generator (or only `only`), check determinism, then write or check.
pub fn run(root: &Path, check: bool, only: Option<&str>) -> Result<(), String> {
    let game = crate::repo::game_dir(root);
    let mut failures = Vec::new();
    let mut ran = 0;
    for g in GENERATORS.iter().filter(|g| only.map_or(true, |o| o == g.id)) {
        ran += 1;
        let a = (g.run)(root).map_err(|e| format!("{}: {e}", g.id))?;
        let b = (g.run)(root).map_err(|e| format!("{}: {e}", g.id))?;
        if a != b { failures.push(format!("{}: two runs differ (generator is not deterministic)", g.id)); continue; }
        for f in &a {
            if !GEN_PREFIXES.iter().any(|p| f.path.starts_with(p)) || f.path.contains("..") {
                failures.push(format!("{}: output path {} is outside the generated directories", g.id, f.path));
                continue;
            }
            let dest = game.join(&f.path);
            let on_disk = std::fs::read(&dest).ok();
            if check {
                if on_disk.as_deref() != Some(&f.bytes[..]) { failures.push(format!("{}: {} differs from a fresh run", g.id, f.path)); }
            } else if on_disk.as_deref() != Some(&f.bytes[..]) {
                if let Some(parent) = dest.parent() { std::fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
                std::fs::write(&dest, &f.bytes).map_err(|e| format!("{}: {e}", dest.display()))?;
            }
        }
    }
    if ran == 0 { return Err(format!("no generator named {:?}", only.unwrap_or(""))); }
    if failures.is_empty() { Ok(()) } else { Err(failures.join("\n")) }
}
