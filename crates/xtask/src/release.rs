//! `cargo xtask release`: the full release build of the game for the host platform, as one command, so a
//! local build and `.github/workflows/release.yml` run the same steps. DESIGN.md "Distribution and signing"
//! (L1274-1280): a macOS arm64 zip of the ad-hoc signed .app and a Windows x86_64 zip, unsigned.
//!
//! Steps: `cargo build --release -p gdclient --locked` (and the debug library the editor loads), `cargo xtask gen`, `godot --headless --import`,
//! `godot --export-release <preset>` (windowed on macOS so the shader baker has a Metal device), then on macOS ad-hoc signing inside out (each nested Mach-O,
//! then the bundle, never `--deep` when signing) with `codesign --verify --strict` on every one and
//! `codesign --verify --deep --strict` on the bundle, then a zip under `target/bp-release/`.
//!
//! Options: `--version X` names the archive (default: the workspace version), `--skip-build` reuses the
//! release library and generated files already on disk, `--no-bake` exports headless without baked shaders. The Godot binary is `$GODOT`, else `godot` on PATH,
//! and must be 4.7.2 with its export templates installed.
use std::path::{Path, PathBuf};
use std::process::Command;

/// The pinned engine (DESIGN.md "Dependencies"); exports from any other version are refused.
pub const GODOT_VERSION: &str = "4.7.2.stable";
/// Folder under `target/` that holds every release output.
pub const OUT_DIR: &str = "target/bp-release";
/// Player-facing name of the executable and bundle.
pub const APP_NAME: &str = "Black Pennant";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform { MacOs, Windows }

impl Platform {
    pub fn host() -> Option<Platform> {
        if cfg!(target_os = "macos") { Some(Platform::MacOs) } else if cfg!(target_os = "windows") { Some(Platform::Windows) } else { None }
    }
    /// The preset name in `game/export_presets.cfg`.
    pub fn preset(self) -> &'static str { match self { Platform::MacOs => "macOS", Platform::Windows => "Windows" } }
    /// The archive suffix: `black-pennant-<version>-<suffix>.zip`.
    pub fn suffix(self) -> &'static str { match self { Platform::MacOs => "macos-arm64", Platform::Windows => "windows-x86_64" } }
    /// The release library the `.gdextension` file points at, relative to the repository root.
    pub fn library(self) -> &'static str { match self { Platform::MacOs => "target/release/libgdclient.dylib", Platform::Windows => "target/release/gdclient.dll" } }
    /// The debug library the editor loads while it imports and exports.
    pub fn editor_library(self) -> &'static str { match self { Platform::MacOs => "target/debug/libgdclient.dylib", Platform::Windows => "target/debug/gdclient.dll" } }
}

/// The archive file name for a version and platform.
pub fn archive_name(version: &str, platform: Platform) -> String {
    format!("black-pennant-{}-{}.zip", version.trim_start_matches('v'), platform.suffix())
}

/// The build id baked into the library. The menu already shows the crate version (`Version 0.1.0, build
/// ...`), so the id is the commit alone when the release version is that version, and the release version
/// and the commit when it differs (a pre-release tag): `3f2a9c1`, `0.1.0-beta.1 3f2a9c1`.
pub fn build_id(version: &str, crate_version: &str, sha: &str) -> String {
    let sha: String = sha.chars().filter(|c| c.is_ascii_hexdigit()).take(7).collect();
    match (version == crate_version, sha.is_empty()) {
        (true, true) => "local".to_string(),
        (true, false) => sha,
        (false, true) => version.to_string(),
        (false, false) => format!("{version} {sha}"),
    }
}

/// `version = "..."` from the `[workspace.package]` table of the root manifest.
pub fn workspace_version(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') { in_package = t == "[workspace.package]"; continue; }
        if !in_package { continue; }
        if let Some(rest) = t.strip_prefix("version") {
            let rest = rest.trim_start().strip_prefix('=')?.trim();
            return Some(rest.trim_matches('"').to_string());
        }
    }
    None
}

/// A version is `X.Y.Z` with an optional `-pre.N` part, as the release tags use (`vX.Y.Z-beta.N`).
pub fn valid_version(v: &str) -> bool {
    let v = v.trim_start_matches('v');
    let (core, pre) = match v.split_once('-') { Some((c, p)) => (c, Some(p)), None => (v, None) };
    let parts: Vec<&str> = core.split('.').collect();
    let core_ok = parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    let pre_ok = pre.is_none_or(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '.'));
    core_ok && pre_ok
}

pub fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let platform = Platform::host().ok_or("release builds run on macOS (arm64) or Windows (x86_64) only")?;
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let skip_build = args.iter().any(|a| a == "--skip-build");
    let version = match value("--version") {
        Some(v) => v.trim_start_matches('v').to_string(),
        None => workspace_version(&std::fs::read_to_string(root.join("Cargo.toml")).map_err(|e| format!("Cargo.toml: {e}"))?)
            .ok_or("no version in [workspace.package]")?,
    };
    if !valid_version(&version) { return Err(format!("version {version:?} is not X.Y.Z or X.Y.Z-pre.N")); }
    let godot = std::env::var("GODOT").unwrap_or_else(|_| "godot".to_string());
    let game = crate::repo::game_dir(root);

    let reported = output(Command::new(&godot).arg("--version"))?;
    if !reported.trim().starts_with(GODOT_VERSION) {
        return Err(format!("{godot} reports {:?}; release builds need Godot {GODOT_VERSION} (set $GODOT)", reported.trim()));
    }

    if !skip_build {
        // The main menu and every crash and benchmark report show BP_BUILD_ID (gdclient app.rs).
        let build_id = std::env::var("BP_BUILD_ID").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| {
            let sha = output(Command::new("git").current_dir(root).args(["rev-parse", "--short=7", "HEAD"])).map(|s| s.trim().to_string()).unwrap_or_default();
            let crate_version = workspace_version(&std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default()).unwrap_or_default();
            build_id(&version, &crate_version, &sha)
        });
        step(&format!("cargo build --release -p gdclient --locked (BP_BUILD_ID={build_id})"));
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        status(Command::new(&cargo).current_dir(root).env("BP_BUILD_ID", &build_id).args(["build", "--release", "-p", "gdclient", "--locked"]))?;
        // The Godot editor that imports and exports loads the debug library (the `.gdextension` debug
        // entries); without it every script naming a Rust class fails to parse and the export breaks.
        step("cargo build -p gdclient --locked (for the editor)");
        status(Command::new(&cargo).current_dir(root).args(["build", "-p", "gdclient", "--locked"]))?;
        step("cargo xtask gen");
        crate::gen::run(root, false, None)?;
    }
    // Release-only gates: no placeholder art reference (L1613) and text CSVs that the export ships raw.
    for id in ["godot.placeholders", "godot.i18n-keep"] {
        step(&format!("cargo xtask lint --release --only {id}"));
        crate::lint::run(root, crate::lint::Profile::Release, Some(id))?;
    }
    for lib in [platform.library(), platform.editor_library()] {
        let lib = root.join(lib);
        if !lib.is_file() { return Err(format!("{} is missing; run without --skip-build", lib.display())); }
    }
    if !game.join("art/gen").is_dir() { return Err("game/art/gen is missing; run without --skip-build".into()); }

    // A fresh checkout has no .godot/ cache. The first import can print errors for scripts that name Rust
    // classes before the extension is registered; the second import runs clean, so only its status counts.
    // The editor resaves project text files as it imports and exports: a windowed editor saves project.godot
    // (dropping every key equal to its default, and the contract comments) and resources such as the bus
    // layout. Every text file under game/ is put back once the export is done.
    let before = snapshot(&game);
    step("godot --headless --import");
    let _ = Command::new(&godot).current_dir(&game).args(["--headless", "--path", "."]).arg("--import").status();
    if let Err(e) = status(Command::new(&godot).current_dir(&game).args(["--headless", "--path", ".", "--import"])) {
        restore(&before);
        return Err(e);
    }

    let out = root.join(OUT_DIR).join(platform.suffix());
    if out.exists() { std::fs::remove_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?; }
    let target = match platform {
        Platform::MacOs => out.join(format!("{APP_NAME}.app")),
        Platform::Windows => out.join(APP_NAME).join(format!("{APP_NAME}.exe")),
    };
    std::fs::create_dir_all(target.parent().unwrap_or(&out)).map_err(|e| format!("{}: {e}", out.display()))?;
    // The shader baker needs a rendering device, which `--headless` does not create. On macOS the export runs
    // with a window (Metal) so the pack carries baked shaders; if no GPU session exists it falls back to a
    // headless export without them. Windows runners have no GPU, so Windows exports headless and unbaked.
    let bake = platform == Platform::MacOs && !args.iter().any(|a| a == "--no-bake");
    let export = |headless: bool| {
        step(&format!("godot{} --export-release {} {}", if headless { " --headless" } else { "" }, platform.preset(), target.display()));
        let mut cmd = Command::new(&godot);
        cmd.current_dir(&game);
        if headless { cmd.arg("--headless"); }
        output_within(cmd.args(["--path", ".", "--export-release", platform.preset()]).arg(&target), EXPORT_TIMEOUT_S, &root.join(OUT_DIR))
    };
    let result = match export(!bake) {
        Ok(log) => Ok(log),
        Err(e) if bake => {
            println!("{e}\nrelease: the windowed export failed; exporting headless, without baked shaders");
            export(true)
        }
        Err(e) => Err(e),
    };
    let restored = restore(&before);
    if !restored.is_empty() { println!("release: put back {} file(s) the editor rewrote: {}", restored.len(), restored.join(", ")); }
    let log = result?;
    print!("{log}");
    if log.contains("ERROR:") { return Err("the export printed errors (above)".into()); }
    if !target.exists() { return Err(format!("the export wrote no {}", target.display())); }

    let archive = root.join(OUT_DIR).join(archive_name(&version, platform));
    if archive.exists() { std::fs::remove_file(&archive).map_err(|e| format!("{}: {e}", archive.display()))?; }
    match platform {
        Platform::MacOs => {
            check_bundle(&target)?;
            thin_macos(&target)?;
            sign_macos(&target)?;
            step("ditto (zip)");
            status(Command::new("ditto").args(["-c", "-k", "--sequesterRsrc", "--keepParent"]).arg(&target).arg(&archive))?;
        }
        Platform::Windows => {
            check_windows(&target)?;
            step("tar -a (zip)");
            // System32's bsdtar writes zip archives with forward-slash entries; Git's GNU tar earlier on PATH cannot.
            let tar = std::env::var("SystemRoot").map(|r| PathBuf::from(r).join("System32").join("tar.exe")).unwrap_or_else(|_| PathBuf::from("tar"));
            status(Command::new(tar).current_dir(&out).arg("-a").arg("-c").arg("-f").arg(&archive).arg(APP_NAME))?;
        }
    }
    println!("release: {}", archive.display());
    Ok(())
}

fn step(s: &str) { println!("release: {s}"); }

/// An export that has not finished in this time is stopped (a windowed editor can wait on a dialog forever);
/// on macOS the release then retries headless.
const EXPORT_TIMEOUT_S: u64 = 30 * 60;

/// Like [`output`], but kills the command after `timeout_s` seconds. Output goes through a log file in `dir`.
fn output_within(cmd: &mut Command, timeout_s: u64, dir: &Path) -> Result<String, String> {
    let shown = format!("{cmd:?}");
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let log = dir.join("export.log");
    let file = std::fs::File::create(&log).map_err(|e| format!("{}: {e}", log.display()))?;
    let err = file.try_clone().map_err(|e| format!("{}: {e}", log.display()))?;
    let mut child = cmd.stdout(file).stderr(err).spawn().map_err(|e| format!("{shown}: {e}"))?;
    let start = std::time::Instant::now();
    let st = loop {
        match child.try_wait().map_err(|e| format!("{shown}: {e}"))? {
            Some(st) => break Some(st),
            None if start.elapsed().as_secs() >= timeout_s => { let _ = child.kill(); let _ = child.wait(); break None; }
            None => std::thread::sleep(std::time::Duration::from_millis(250)),
        }
    };
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    match st {
        Some(st) if st.success() => Ok(text),
        Some(st) => Err(format!("{shown} exited with {st}:\n{text}")),
        None => Err(format!("{shown} did not finish within {timeout_s} s:\n{text}")),
    }
}

/// Project text files the editor may rewrite when it opens with a window: settings, scenes and resources.
const EDITOR_REWRITES: &[&str] = &["godot", "cfg", "tres", "tscn"];

/// The bytes of every project text file under `game/` that the editor may rewrite.
fn snapshot(game: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn rec(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if p.is_dir() {
                if name != ".godot" && name != "gen" { rec(&p, out); }
            } else if p.extension().and_then(|e| e.to_str()).is_some_and(|e| EDITOR_REWRITES.contains(&e)) {
                if let Ok(bytes) = std::fs::read(&p) { out.push((p, bytes)); }
            }
        }
    }
    let mut out = Vec::new();
    rec(game, &mut out);
    out
}

/// Write back every snapshotted file whose bytes changed; returns their names.
fn restore(before: &[(PathBuf, Vec<u8>)]) -> Vec<String> {
    let mut changed = Vec::new();
    for (p, bytes) in before {
        if std::fs::read(p).ok().as_deref() != Some(bytes.as_slice()) && std::fs::write(p, bytes).is_ok() {
            changed.push(p.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string());
        }
    }
    changed
}

fn status(cmd: &mut Command) -> Result<(), String> {
    let shown = format!("{cmd:?}");
    let st = cmd.status().map_err(|e| format!("{shown}: {e}"))?;
    if st.success() { Ok(()) } else { Err(format!("{shown} exited with {st}")) }
}

fn output(cmd: &mut Command) -> Result<String, String> {
    let shown = format!("{cmd:?}");
    let o = cmd.output().map_err(|e| format!("{shown}: {e}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    if o.status.success() { Ok(text) } else { Err(format!("{shown} exited with {}:\n{text}", o.status)) }
}

/// The exported bundle holds the executable, the pack and the Rust library.
fn check_bundle(app: &Path) -> Result<(), String> {
    let contents = app.join("Contents");
    let need = [contents.join("MacOS").join(APP_NAME), contents.join("Resources").join(format!("{APP_NAME}.pck")), contents.join("Frameworks").join("libgdclient.dylib")];
    for p in need {
        if !p.exists() { return Err(format!("exported bundle has no {}", p.display())); }
    }
    Ok(())
}

fn check_windows(exe: &Path) -> Result<(), String> {
    let dir = exe.parent().unwrap_or(exe);
    for name in [format!("{APP_NAME}.exe"), format!("{APP_NAME}.pck"), "gdclient.dll".to_string()] {
        if !dir.join(&name).exists() { return Err(format!("exported folder has no {name}")); }
    }
    Ok(())
}

/// Godot 4.7.2 ships only a universal macOS template, so the preset exports `universal`. Releases are
/// arm64 only (L1262) and the Rust library is arm64 only, so the x86_64 slice of the engine is cut away
/// here: an Intel Mac then refuses the app at once instead of starting an engine that cannot load the game.
fn thin_macos(app: &Path) -> Result<(), String> {
    let exe = app.join("Contents").join("MacOS").join(APP_NAME);
    let archs = output(Command::new("lipo").arg("-archs").arg(&exe))?;
    let archs: Vec<&str> = archs.split_whitespace().collect();
    if !archs.contains(&"arm64") { return Err(format!("{} has no arm64 slice ({archs:?})", exe.display())); }
    if archs.len() > 1 {
        step("lipo -thin arm64");
        let thin = exe.with_extension("arm64");
        status(Command::new("lipo").arg(&exe).args(["-thin", "arm64", "-output"]).arg(&thin))?;
        std::fs::rename(&thin, &exe).map_err(|e| format!("{}: {e}", exe.display()))?;
        status(Command::new("chmod").arg("755").arg(&exe))?;
        // The universal template's Info.plist still names x86_64; leave only the slice that ships.
        let plist = app.join("Contents").join("Info.plist");
        let _ = Command::new("plutil").args(["-remove", "LSMinimumSystemVersionByArchitecture.x86_64"]).arg(&plist).status();
        status(Command::new("plutil").args(["-replace", "LSArchitecturePriority", "-json", "[\"arm64\"]"]).arg(&plist))?;
    }
    let lib = app.join("Contents").join("Frameworks").join("libgdclient.dylib");
    let lib_archs = output(Command::new("lipo").arg("-archs").arg(&lib))?;
    if lib_archs.trim() != "arm64" { return Err(format!("{} is {:?}, not arm64", lib.display(), lib_archs.trim())); }
    Ok(())
}

/// Every Mach-O inside the bundle other than the main executable, deepest first.
fn nested_code(app: &Path) -> Vec<PathBuf> {
    fn rec(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if p.is_dir() && ext != "framework" { rec(&p, out); }
            if ext == "dylib" || ext == "framework" || ext == "so" || ext == "bundle" { out.push(p); }
        }
    }
    let mut out = Vec::new();
    rec(&app.join("Contents").join("Frameworks"), &mut out);
    out.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    out
}

/// Ad-hoc sign inside out (L1278): each nested library or framework, then the bundle. No hardened runtime,
/// so the GDExtension needs no library-validation entitlement.
fn sign_macos(app: &Path) -> Result<(), String> {
    let nested = nested_code(app);
    for p in &nested {
        step(&format!("codesign -s - {}", p.display()));
        status(Command::new("codesign").args(["--force", "--sign", "-", "--timestamp=none"]).arg(p))?;
    }
    step(&format!("codesign -s - {}", app.display()));
    status(Command::new("codesign").args(["--force", "--sign", "-", "--timestamp=none"]).arg(app))?;
    for p in &nested {
        status(Command::new("codesign").args(["--verify", "--strict"]).arg(p))?;
    }
    step("codesign --verify --deep --strict");
    status(Command::new("codesign").args(["--verify", "--deep", "--strict", "--verbose=2"]).arg(app))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_names() {
        assert_eq!(archive_name("0.1.0", Platform::MacOs), "black-pennant-0.1.0-macos-arm64.zip");
        assert_eq!(archive_name("v0.2.0-beta.3", Platform::Windows), "black-pennant-0.2.0-beta.3-windows-x86_64.zip");
    }

    #[test]
    fn build_ids() {
        assert_eq!(build_id("0.2.0-beta.1", "0.2.0", "3f2a9c1\n"), "0.2.0-beta.1 3f2a9c1");
        assert_eq!(build_id("0.2.0", "0.2.0", "3f2a9c1"), "3f2a9c1");
        assert_eq!(build_id("0.2.0-dev.4", "0.2.0", ""), "0.2.0-dev.4");
    }

    #[test]
    fn versions() {
        for ok in ["0.1.0", "v0.1.0", "0.2.0-beta.1", "1.10.200-rc.2"] { assert!(valid_version(ok), "{ok}"); }
        for bad in ["", "0.1", "0.1.0-", "0.1.0-beta 1", "a.b.c", "0.1.0/../x"] { assert!(!valid_version(bad), "{bad}"); }
    }

    #[test]
    fn workspace_version_reads_the_package_table() {
        let m = "[workspace]\nmembers = []\n\n[workspace.package]\nedition = \"2021\"\nversion = \"0.3.1\"\n\n[dependencies]\nversion = \"9\"\n";
        assert_eq!(workspace_version(m).as_deref(), Some("0.3.1"));
        assert_eq!(workspace_version("[package]\nversion = \"1.0.0\"\n"), None);
    }

    #[test]
    fn presets_exist_for_both_platforms() {
        let presets = std::fs::read_to_string(crate::repo::root().join("game/export_presets.cfg")).expect("export_presets.cfg");
        for p in [Platform::MacOs, Platform::Windows] {
            assert!(presets.contains(&format!("name=\"{}\"", p.preset())), "{:?}", p);
        }
        // DESIGN.md L1264: baked-shader exports abort below macOS 14; L1278: Godot's export signing is off.
        assert!(presets.contains("application/min_macos_version_arm64=\"14.00\""));
        assert!(presets.contains("codesign/codesign=0") && presets.contains("codesign/enable=false"));
        for line in presets.lines().filter(|l| l.starts_with("exclude_filter=")) {
            assert!(line.contains("_placeholder/*") && line.contains("test/*"), "{line}");
        }
        for line in presets.lines().filter(|l| l.starts_with("include_filter=")) {
            assert!(line.contains("art/gen/*.bin") && line.contains("i18n/*.csv"), "{line}");
        }
    }
}
