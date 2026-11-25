//! Contract-owned, implemented driver for `cargo xtask lint`. Each lint is a pure function over the repository
//! returning findings; the driver runs them in registry order and fails on any error-severity finding.
//!
//! Scope rule: every lint names the files it reads (by path prefix and extension) so it never scans xtask's own
//! word lists or this crate's sources; lints over Rust code read only the crates they govern. Word lists and
//! allow-lists live in data files under `game/ui/lint/` (`banned_phrases.txt`, `glossary.txt`,
//! `meta_fields.txt`), created and owned by the copy and UI lint units.
pub mod copy;
pub mod godot;
pub mod rust;
pub mod ui;

use std::path::{Path, PathBuf};

/// When a lint runs. `Release` lints run only with `--release` (release CI), e.g. placeholder references (L1613).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Profile { Pr, Release }

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Fails CI.
    Error,
    /// Printed only (lints still being calibrated, e.g. sentence length before 0.1.0, L1618).
    Warn,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub lint: &'static str,
    pub severity: Severity,
    /// Repo-relative path with `/` separators.
    pub path: String,
    /// 1-based line, 0 when the finding is about the whole file.
    pub line: u32,
    pub message: String,
}

/// Read-only view of the repository for lints.
pub struct LintCtx {
    pub root: PathBuf,
    pub profile: Profile,
    /// Every non-skipped file, repo-relative, sorted.
    pub files: Vec<String>,
}
impl LintCtx {
    pub fn new(root: &Path, profile: Profile) -> Self { Self { root: root.to_path_buf(), profile, files: crate::repo::walk(root) } }
    /// Files under `prefix` (repo-relative directory, e.g. `game/` or `crates/sim-core/`) ending in one of `exts`
    /// (e.g. `["tscn", "gd"]`; empty = any).
    pub fn files<'a>(&'a self, prefix: &'a str, exts: &'a [&'a str]) -> impl Iterator<Item = &'a str> + 'a {
        self.files.iter().map(String::as_str).filter(move |f| f.starts_with(prefix) && (exts.is_empty() || exts.iter().any(|e| f.rsplit('.').next() == Some(*e))))
    }
    pub fn read(&self, rel: &str) -> String { std::fs::read_to_string(self.root.join(rel)).unwrap_or_default() }
}

pub type LintFn = fn(&LintCtx) -> Vec<Finding>;

/// A registered lint with the spec line that requires it.
pub struct Lint { pub id: &'static str, pub profile: Profile, pub spec: &'static str, pub run: LintFn }

macro_rules! lint { ($id:literal, $p:ident, $spec:literal, $f:path) => { Lint { id: $id, profile: Profile::$p, spec: $spec, run: $f } }; }

/// Every lint, in run order.
pub const LINTS: &[Lint] = &[
    // Rust and workspace (`xt-lint-rust`).
    lint!("rust.transcendentals", Pr, "CLAUDE.md determinism; L441-443 libm only", rust::std_transcendentals),
    lint!("rust.hash-iteration", Pr, "L1111 no std HashMap/HashSet in sim-core and sim-systems", rust::hash_collections),
    lint!("rust.parallel-float-sum", Pr, "L1152 never sum floats in parallel", rust::parallel_float_sum),
    lint!("rust.experimental-threads", Pr, "L1088 never enable gdext experimental-threads", rust::experimental_threads),
    lint!("rust.godot-scope", Pr, "L1060-1072 only gdclient depends on godot", rust::godot_scope),
    lint!("rust.steam-default-off", Pr, "L1290 steam feature off by default; no SDK committed", rust::steam_default_off),
    lint!("rust.version-pins", Pr, "L1180-1224 pinned versions and features", rust::version_pins),
    lint!("rust.mesh-clear", Pr, "L1929, L2061 no mesh_clear in gdclient", rust::mesh_clear),
    // Godot project, scenes, environments and shaders (`xt-lint-godot`).
    lint!("godot.renderer-keys", Pr, "L1268 renderer keys pinned; L1092 physics dummy server", godot::renderer_keys),
    lint!("godot.physics-bodies", Pr, "L1092 no RigidBody3D or CharacterBody3D in gameplay scenes", godot::physics_bodies),
    lint!("godot.environment", Pr, "L1846-1851, L2061 glow, exposure, fog, aerial perspective, sky, background", godot::environment),
    lint!("godot.camera-far", Pr, "L1873, L2061 world camera far >= 20,000 m", godot::camera_far),
    lint!("godot.particles-local-coords", Pr, "L1951, L2061 local_coords true", godot::particles_local_coords),
    lint!("godot.shader-hints", Pr, "L1905, L2061 no depth or screen reads in water or VFX shaders", godot::shader_hints),
    lint!("godot.post-strings", Pr, "L1822, L2061 chromatic, grain, lens_dirt, lens_flare, vignette", godot::post_strings),
    lint!("godot.world-colour-literals", Pr, "L1788, L2061 world colours only from world tokens", godot::world_colour_literals),
    lint!("godot.fonts", Pr, "L1605 only listed font files", godot::fonts),
    lint!("godot.icons", Pr, "L1606 icons only from res://ui/icons", godot::icons),
    lint!("godot.i18n-keep", Pr, "L1707 text CSVs ship raw: the I18n autoload reads them at start-up", godot::i18n_keep),
    lint!("godot.placeholders", Release, "L1613, L1741 no res://_placeholder/ reference", godot::placeholders),
    // UI scenes and scripts (`xt-lint-ui`).
    lint!("ui.theme-overrides", Pr, "L1728 no theme_override_* in .tscn, no add_theme_*_override()", ui::theme_overrides),
    lint!("ui.colour-literals", Pr, "L1593 Color literals only in res://ui/theme/tokens.json", ui::colour_literals),
    lint!("ui.tween", Pr, "L1602, L1737 banned transitions; tweens name a sim signal", ui::tweens),
    lint!("ui.uppercase", Pr, "L1607, L1730 Label.uppercase false", ui::uppercase),
    lint!("ui.eyebrow", Pr, "L1578, L1586-1587 eyebrow and kicker patterns", ui::eyebrow),
    lint!("ui.glass", Pr, "L1594 no BackBufferCopy or blur shaders", ui::glass),
    lint!("ui.radius-shadow", Pr, "L1565, L1593, L1595 radius <= 4, no nested shadowed panels, grey shadows", ui::radius_shadow),
    lint!("ui.graphedit", Pr, "L1601, L1736 GraphEdit chrome off", ui::graphedit),
    lint!("ui.blink", Pr, "L1603 no looped visibility or modulate on Label; 1 and 2.5 Hz only", ui::blink),
    lint!("ui.tooltip", Pr, "L1609 tooltip restating its label", ui::tooltips),
    // Copy (`xt-lint-copy`).
    lint!("copy.banned-words", Pr, "CLAUDE.md word list; L1608 banned_phrases.txt; L1703 copy rule 9", copy::banned_words),
    lint!("copy.emoji", Pr, "L1606 emoji regex", copy::emoji),
    lint!("copy.case", Pr, "L1607, L1696 sentence case; all-caps pairs only from glossary.txt", copy::case),
    lint!("copy.two-digit", Pr, "L1590 two-digit numbers on non-sequences", copy::two_digit),
    lint!("copy.sentence-length", Pr, "L1618 sentence-length lint (warn until calibrated)", copy::sentence_length),
    lint!("copy.i18n-coverage", Pr, "L1707 every key has a CSV row with a context column", copy::i18n_coverage),
    // Tokens export (`xt-art-tokens`).
    lint!("art.tokens-sync", Pr, "L1788 committed world tokens match the Rust table", lint_tokens_sync),
];

fn lint_tokens_sync(ctx: &LintCtx) -> Vec<Finding> {
    match crate::art::tokens::run(&ctx.root, true) {
        Ok(()) => Vec::new(),
        Err(e) => vec![Finding { lint: "art.tokens-sync", severity: Severity::Error, path: format!("game/{}", crate::art::WORLD_TOKENS_JSON), line: 0, message: e }],
    }
}

pub fn list() { for l in LINTS { println!("{:32} {:?}  {}", l.id, l.profile, l.spec); } }

/// Run the lints for `profile` (Release includes Pr lints), or only `only`. Prints every finding as
/// `path:line: [lint] message`; fails if any error-severity finding exists.
pub fn run(root: &Path, profile: Profile, only: Option<&str>) -> Result<(), String> {
    let ctx = LintCtx::new(root, profile);
    let mut errors = 0usize;
    let mut ran = 0usize;
    for l in LINTS.iter().filter(|l| l.profile <= profile && only.map_or(true, |o| o == l.id)) {
        ran += 1;
        for f in (l.run)(&ctx) {
            println!("{}:{}: [{}] {}", f.path, f.line, f.lint, f.message);
            if f.severity == Severity::Error { errors += 1; }
        }
    }
    if ran == 0 { return Err(format!("no lint named {:?} in this profile", only.unwrap_or(""))); }
    if errors == 0 { Ok(()) } else { Err(format!("{errors} lint error(s)")) }
}
