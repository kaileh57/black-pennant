//! Collects stock blueprints and kits under `<repo>/blueprints/` into `OUT_DIR/stock_files.rs` as a sorted
//! slice of (relative path, include_str!) pairs, so data units add files without touching Rust.
use std::{env, fs, path::{Path, PathBuf}};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() { walk(&p, out); } else if p.to_string_lossy().ends_with(".bp.ron") || p.to_string_lossy().ends_with(".kit.ron") { out.push(p); }
    }
}

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../blueprints");
    let root = root.canonicalize().unwrap_or(root);
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    walk(&root, &mut files);
    let mut rows: Vec<(String, String)> = files.iter().map(|p| {
        println!("cargo:rerun-if-changed={}", p.display());
        let rel = p.strip_prefix(&root).unwrap().components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
        (rel.clone(), rel)
    }).collect();
    rows.sort();
    let mut s = String::from("&[\n");
    // Paths are emitted relative to CARGO_MANIFEST_DIR so no canonicalised absolute path (a `\\?\` path on
    // Windows) reaches `include_str!`.
    for (rel, _) in rows { s.push_str(&format!("    ({rel:?}, include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../blueprints/\", {rel:?}))),\n")); }
    s.push_str("]\n");
    fs::write(PathBuf::from(env::var("OUT_DIR").unwrap()).join("stock_files.rs"), s).unwrap();
}
