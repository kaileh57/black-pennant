# Black Pennant: Godot project

Godot 4.7.2 project. All game rules, the simulation and 3D rendering of simulation state live in Rust
(`crates/gdclient` and the crates under it). GDScript here is UI glue only: it reads data from the Rust
classes, draws it, and sends player input back. It holds no gameplay logic and no physics.

Run `cargo build -p gdclient` first; `black_pennant.gdextension` loads the library from `../target`.

## Layout

| Path | Holds |
| --- | --- |
| `project.godot` | Engine settings, autoload list, input map. Changed only through the orchestrator. |
| `autoload/` | One-line scripts that extend the Rust autoload classes, plus `i18n.gd`, `ui.gd`, `screens.gd`. |
| `boot/` | Main scene: startup log, consent, loading cover. |
| `ui/theme/` | `tokens.json` (the only place a UI colour is written) and the compiled theme. |
| `ui/fonts/`, `ui/icons/`, `ui/lint/` | Fonts with licences, the one SVG icon set, lint word lists. |
| `ui/widgets/`, `ui/screens/` | Shared controls; menus, settings, lobby, briefing, after-action, replays. |
| `ui/hud/`, `ui/tutorial/` | Sortie HUD clusters; the tutorial overlay. |
| `ui/editor/`, `ui/chart/` | Dry dock panels; the chart table. |
| `world/` | 3D scenes: the sortie world and the dry-dock stage, environment presets. |
| `shaders/` | Hand-written shaders. `shaders/gen/` is written by `cargo xtask gen`. |
| `art/tokens/` | `world_tokens.json`, the only place a world colour is written. `art/gen/` is generated: raw texture `.bin` files that Rust reads, not imported resources. Export presets must include `art/gen/*.bin` as non-resource files. |
| `audio/` | Sound player and bus layout (buses `Sfx`, `Ambience`, `Alert`, `Radio`, `Ui`, `Music`). `audio/gen/` is generated: one WAV per sound of the Rust `audio` crate. |
| `i18n/` | English text as CSV, one file per area. Loaded at start-up by `I18n`; no registration. |
| `test/` | `unit/`, `ui/`, `capture/`, `lint/`, and `tools/check_project.gd`. |
| `_placeholder/`, `addons/` | The only place temporary art may live; vendored add-ons at pinned versions. |

The three `gen/` folders are never edited by hand and never committed.

## Rust classes

Autoloads: `App`, `Settings`, `Quality`, `Tx`, `Content`, `Session`, `Orders`, `Hud`, `Editor`, `Campaign`,
`Lobby`, `Tutorial`, `AudioFeed`, `Bench`. Scene nodes: `BpWorldView`, `BpPlotView`, `BpEditorView`.
Constants: `Bp.STANCE_HOSTILE` and the rest of `Bp.*`. Each method's doc comment in `crates/gdclient/src`
gives its argument and return shapes; named Dictionary shapes are in `crates/gdclient/src/shapes.rs`.

## Naming rules

- Files and folders are `snake_case`. A scene and its script share a stem: `track_list.tscn`, `track_list.gd`.
- Node names are `PascalCase`; the root node is the file stem in `PascalCase`.
- One screen or panel per scene. A scene's script is the only script that touches its nodes.
- Scripts are typed: every variable, argument and return has a type.
- Text keys are `area.thing.detail` in lower case; the CSV header is `keys,?context,en`.
- Input actions are `snake_case` verbs; dry-dock actions start with `editor_`.
- No node or type is named Eyebrow, Kicker, Overline, Tagline, SectionLabel or Pretitle.

## Rules that CI checks

- No `theme_override_*` in scenes and no colour literal outside the two token files.
- `Label.uppercase` stays false. Sentence case everywhere; capitals only for glossary codes.
- No `TRANS_ELASTIC`, `TRANS_BOUNCE`, `TRANS_BACK` or `TRANS_SPRING`. Motion only reports state.
- No `RigidBody3D` or `CharacterBody3D`. Picking goes through `pick` methods on the Rust view nodes.
- Gameplay input is read in `_unhandled_input` and never binds a `ui_*` action.
- UI redraws on a change signal and pulls the data then. It does not rebuild Dictionaries in `_process`.
- A script that extends a Rust class must not define `_ready`, `_process` or `_exit_tree`: it would
  replace the Rust one.

Checks: `godot --headless --path . -- --smoke` and
`godot --headless --path . -s res://test/tools/check_project.gd`.
