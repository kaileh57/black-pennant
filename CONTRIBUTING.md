# Contributing

Black Pennant is built mostly by an AI coding agent under one owner's direction, and the owner reviews every change. Small, focused pull requests get reviewed fastest.

## Before you start

`DESIGN.md` is the build spec. It decides how the game works, how it looks and what it may depend on. A change that contradicts it needs an issue first, so the owner can decide and record the answer in the spec. Read the section for the system you are touching, and name that section's heading in your pull request.

`AGENTS.md` and `CLAUDE.md` hold the short form of the rules. The ones that catch people most often:

- The simulation is deterministic. In `sim-core` and `sim-systems`, use `libm` for every transcendental function, never iterate a `HashMap`, never sum floats in parallel, and draw randomness from the hash RNG keyed on seed, tick and ids.
- Only the `gdclient` crate depends on Godot, and GDScript is UI glue only.
- Art and audio are made in code: procedural meshes and shaders, hand-written SVG icons, sound synthesized in the `audio` crate. No generative models and no asset packs, CC0 included.
- UI and copy follow DESIGN.md's "Interface and visual design" section: sentence case, colours only from `game/ui/theme/tokens.json`, no `theme_override_*` in scenes. `cargo xtask lint` checks most of it.
- The project spends no money: no paid services, runners or certificates.

## Set up

Follow "Build from source" in the [README](README.md). In short: install Rust through rustup and Godot 4.7.2, then run `cargo build -p gdclient`, `cargo xtask gen` and `godot --path game`.

## Check your change

Run these before you open a pull request:

```sh
cargo test --workspace
cargo xtask lint
godot --headless --path game -s res://test/tools/check_project.gd
```

A simulation change that alters results also updates the golden state hash, and the pull request says why.

For anything a player sees, attach screenshots at 1280x720 and 1920x1080 of every screen you changed, and check each against the banned-pattern table in DESIGN.md.

Pull requests must pass CI on macOS and Windows.

## Licences of contributions

Code you contribute is licensed under MIT or Apache 2.0, at the user's option, like the rest of the code. Blueprints, kits and other game data are licensed under CC BY 4.0. By opening a pull request you agree to license your contribution that way.

## Reporting bugs

Use the bug report or playtest report form on GitHub issues; [docs/playtest.md](docs/playtest.md) explains both. Give the game version from the main menu (for example "Version 0.1.0, build 3f2a9c1"), your system, and what you did. Logs help: on Windows, run `Black Pennant.console.exe`; on macOS, run `"/Applications/Black Pennant.app/Contents/MacOS/Black Pennant"` from Terminal. After a crash, the game names its logs folder at the next start; attach the newest `godot<date>.log` from there. Playtest logs are copied from Settings, under Playtest.
