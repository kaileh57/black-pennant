//! xtask: repository automation run as `cargo xtask <command>` (alias in `.cargo/config.toml`).
//! DESIGN.md L1071 (`cargo xtask gen` writes the generated files: wave shader include, baked FFT detail,
//! rendered audio), L1076-1078 (generated files are not committed; every generator is seeded and deterministic;
//! CI runs it twice per platform and fails if the outputs differ by a byte), L1788 (LUTs, ramps and noise come
//! from the tokens via Rust build steps), L563 (`fit-drag`), CI lints across "Interface and visual design"
//! (L1574-1622, L1724-1745) and "Graphics and rendering" (L2061), L1092 and L1111 (boundary and determinism).
//!
//! Commands (`.github/workflows/ci.yml` calls `lint`, `gen` and `gen --check`):
//! - `gen [--check] [--only <id>]`: run generators (each twice, compared byte for byte), write into `game/`;
//!   `--check` writes nothing and fails if any file on disk differs from a fresh run.
//! - `lint [--release] [--only <id>] [--list]`: run the CI lints; `--release` adds release-only lints.
//! - `fit-drag [--check]`: fit each calibre band's drag k into `crates/content/data/weapons.ron` (L563).
//! - `tokens [--check]`: write the committed `game/art/tokens/world_tokens.json` from the Rust token table.
//! - `release [--version X] [--skip-build]`: the full release build for the host platform ([`release`]):
//!   release library, generated files, Godot export, ad-hoc signing on macOS and a zip under `target/bp-release/`.
//!
//! Module map: contract-owned and implemented: this file, [`repo`], `gen/mod.rs`, `lint/mod.rs`, `art/mod.rs`.
//! Units: `xt-gen-basic` `gen/basic.rs`, `xt-gen-fft` `gen/fft.rs`, `xt-gen-art` `gen/art.rs`,
//! `xt-art-tokens` `art/tokens.rs`, `xt-lint-rust` `lint/rust.rs`, `xt-lint-godot` `lint/godot.rs`,
//! `xt-lint-ui` `lint/ui.rs`, `xt-lint-copy` `lint/copy.rs`, `xt-fit-drag` `fit_drag.rs`.
//!
//! Path note: CLAUDE.md names `game/gen/`; DESIGN.md L1076 and `.gitignore` use `game/shaders/gen/`,
//! `game/art/gen/` and `game/audio/gen/`. This crate follows the spec.
#![allow(dead_code, unused_variables, unused_imports)]

mod art;
mod fit_drag;
mod gen;
mod lint;
mod release;
mod repo;

use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!("usage: cargo xtask <gen [--check] [--only ID] | lint [--release] [--only ID] [--list] | fit-drag [--check] | tokens [--check] | release [--version X] [--skip-build]>");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else { return usage() };
    let flag = |f: &str| args.iter().any(|a| a == f);
    let only = args.iter().position(|a| a == "--only").and_then(|i| args.get(i + 1)).cloned();
    let root = repo::root();
    let result: Result<(), String> = match cmd.as_str() {
        "gen" => gen::run(&root, flag("--check"), only.as_deref()),
        "lint" if flag("--list") => { lint::list(); Ok(()) }
        "lint" => lint::run(&root, if flag("--release") { lint::Profile::Release } else { lint::Profile::Pr }, only.as_deref()),
        "fit-drag" => fit_drag::run(&root, flag("--check")),
        "tokens" => art::tokens::run(&root, flag("--check")),
        "release" => release::run(&root, &args[1..]),
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("xtask {cmd}: {e}"); ExitCode::FAILURE }
    }
}
