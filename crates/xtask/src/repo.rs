//! Contract-owned, implemented. Repository paths and a deterministic file walker shared by generators and lints.
use std::path::{Path, PathBuf};

/// Repository root (two levels above this crate's manifest).
pub fn root() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent().and_then(Path::parent).map(Path::to_path_buf).unwrap_or(here)
}

/// The Godot project directory (`res://`).
pub fn game_dir(root: &Path) -> PathBuf { root.join("game") }

/// Directories never walked: build output, VCS, Godot's cache and generated files.
pub const SKIP_DIRS: &[&str] = &["target", ".git", ".godot", "node_modules", "game/shaders/gen", "game/art/gen", "game/audio/gen", "game/export", "captures", "bench-results"];

/// Every file under `root` as repo-relative paths with `/` separators, sorted, skipping [`SKIP_DIRS`].
pub fn walk(root: &Path) -> Vec<String> {
    fn rec(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            if SKIP_DIRS.iter().any(|s| rel == *s) { continue; }
            if p.is_dir() { rec(root, &p, out); } else { out.push(rel); }
        }
    }
    let mut out = Vec::new();
    rec(root, root, &mut out);
    out
}
