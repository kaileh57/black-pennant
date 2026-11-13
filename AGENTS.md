# Black Pennant: agent rules

Black Pennant is a naval combat vehicle builder: a Godot 4.7.2 client with a Rust simulation through godot-rust (gdext 0.5.5, `api-4-7`). Claude builds it full-time; the owner directs, reviews and playtests.

## The spec

`DESIGN.md` is the build spec, frozen on 12 November 2026. Build from it, and do not re-decide anything it decides. If the spec is wrong, ambiguous or blocks the build, stop and ask the owner. Record the answer in `DESIGN.md` with its date. Never change a frozen decision silently.

Before working on a system, read its section in `DESIGN.md`. Quote the section heading in your PR description.

## Repository layout

- `crates/`: the Rust workspace members named in DESIGN.md's Crate layout table: sim-core, sim-systems, content, net, hull-mesh, audio, server, xtask, gdclient. Only gdclient depends on Godot.
- `game/`: the Godot project (`game/project.godot`). GDScript is UI glue only.
- `game/gen/`: written by `cargo xtask gen` (wave shader include, baked FFT detail, rendered audio). It is never edited by hand and never committed.
- `game/_placeholder/`: the only place temporary art may live. Export presets exclude it, and release CI fails on any reference to it.
- `blueprints/`: stock ships and kits as RON, licensed CC BY 4.0.

## Hard rules

- Zero cash spend. Use no paid service, paid runner or bought hardware. CI runs only on GitHub-hosted standard runners.
- Steam is phase 2. All Steam code sits behind the `steam` cargo feature, which is off by default. Never commit the Steamworks SDK or its binaries.
- Art and audio are made in code only: procedural meshes and shaders, hand-written SVG icons, and sound synthesized with fundsp. Never use generative image, audio, voice or music models, and never use third-party asset packs, CC0 included. Officers and aces speak as text with radio effects.
- Determinism in the simulation:
  - Never call std `sin`, `cos`, `tan`, `exp`, `ln`, `powf`, `powi`, `atan2` or other transcendental functions; use libm.
  - Never sum floats in parallel. Merge parallel work serially in a fixed order.
  - Count per-tick budgets in work units, never in elapsed time.
  - Draw randomness from the counter-based hash RNG keyed on (seed, tick, ids).
- Godot boundary: gdext bindings are touched only from the main thread. Never enable `experimental-threads`. The simulation exchanges data through triple-buffered snapshots and a command queue, and the adapter's main-thread budget is 2 ms per frame.
- Pin versions as DESIGN.md says (`godot = "0.5.5"` with `api-4-7`, the Rust toolchain in `rust-toolchain.toml`, and a committed `Cargo.lock`). Upgrade only behind the full CI matrix.
- Both platforms are first-class: macOS arm64 (Metal) and Windows x86_64 (D3D12, with fallback to Vulkan, then OpenGL 3). Windows can't be tested by hand here, so every PR must pass Windows CI.

## UI and copy rules

DESIGN.md's "Interface and visual design" section governs every screen, the README and the docs site. Its banned-pattern table is enforced in CI. These are the ones agents default to most often:

- No eyebrow, kicker or overline labels above headings. The one exception is a meta line bound to game data, and it must pass the cover and swap tests.
- No glow, glass, blur or gradient text in UI. No purple or cyan accents. Normal state is grey, and hue appears only for coding.
- No card grids with a uniform radius and shadow. No coloured side stripes outside the Severity, Stance and Damage variations.
- No emoji, no stock icon sets, no decorative monospace, no pseudo-code chrome (`SYS://`, `//`, hex) and no fake telemetry.
- Sentence case everywhere. Capitals only for glossary codes. `Label.uppercase` stays false.
- No `theme_override_*` in scenes and no Color literals outside `res://ui/theme/tokens.json`.
- Never use these Tween transitions: `TRANS_ELASTIC`, `TRANS_BOUNCE`, `TRANS_BACK` or `TRANS_SPRING`. Motion only reports state.
- Never use these words in copy: introducing, unlock, empower, elevate, streamline, transform, discover, seamless, smart, next-gen, revolutionize, leverage, delve, vibrant, pivotal, showcase, tapestry, testament, unleash.
- Before handoff, screenshot every screen you changed, score it against the banned-pattern table, and attach the captures and the hit list to the PR. Two or more hits on one screen fail review.
- Never prompt yourself or a subagent to "be distinctive". Follow the written spec.

## Graphics rules

DESIGN.md's "Graphics and rendering" section governs the 3D look:
- The look is geometric and cell-built.
- The hero light is a low warm sun at dusk.
- The sea is a lit sea that matches the simulation's waves.
- Post-processing is restrained and photographic, with no outlines.
- Visual detail is the first thing to degrade.

Budgets are gates. Measure them on this Mac with the min-spec throttle profile, and never raise the minimum spec.

## Working

- `cargo test --workspace` must pass headless on macOS, Linux and Windows.
- Simulation changes that alter results need an updated golden hash, with the reason in the PR.
- Commit or push only when the owner asks.
