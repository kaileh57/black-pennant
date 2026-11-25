# Theme tokens: citations and item names

`tokens.json` is the only place a UI colour is written. `compile_theme.gd` turns it into
`project_theme.tres` (dusk, set as `gui/theme/custom`) and `project_theme_high_contrast.tres`.
A `.tres` drops comments, so the sources sit here (DESIGN.md "Godot implementation" item 1).

Rebuild after editing `tokens.json`, an icon or a font:

```
cd game
godot --headless --path . -s res://ui/theme/compile_theme.gd
godot --headless --path . -s res://test/lint/ui_lint.gd
```

The lint fails when a committed theme is stale, so the two files never drift from the tokens.

## Sources

| Tokens | Source |
| --- | --- |
| Dusk and high-contrast palettes, ratios | DESIGN.md "Tokens" table (L1624-1639): WCAG 2 ratios on surface.raised, truncated. `contrast` in `tokens.json` repeats the table and the lint recomputes it. |
| Chart palette | Exact palette entries of NOAA Chart 18649, Entrance to San Francisco Bay, last edition, public domain, credited to NOAA Office of Coast Survey (https://historicalcharts.noaa.gov/pdfs/18649.pdf). Meanings per U.S. Chart No. 1 (https://nauticalcharts.noaa.gov/publications/us-chart-1.html). L1643. |
| Typeface | Atkinson Hyperlegible Next v2.001, Braille Institute, SIL OFL 1.1, no Reserved Font Name; from github.com/google/fonts `ofl/atkinsonhyperlegiblenext` (upstream commit 7925f50f). Licence: `res://ui/fonts/licenses/OFL.txt`. Checked on import: version string 2.001, tabular digits 632 units, 93 of 128 Latin Extended-A, no µ. L1645. |
| Mono | Atkinson Hyperlegible Mono v2.001 (OFL 1.1), set to wght 400 or 700 because its default is 200. Not yet in the repository: until `fonts.mono.file` exists, Expression and Traffic use the sans with tabular figures. |
| Fallbacks | Noto Sans (Cyrillic), Noto Sans SC, Noto Sans JP (OFL 1.1), loaded from `fonts.fallbacks` when the files exist, each marked as supporting its language. Digits stay Atkinson. |
| Type scale | Rendered body height at 1080p over the font's ratio, rounded up: font_size = ceil(height / 0.910). 18 px gives 20, 20 px gives 22, 24 px gives 27, 30 px gives 33 and 40 px gives 44 (L1647, L1734). |
| Line height | 1.5: `line_spacing` = round(1.5 x size - font height) per variation (L1647, L1720). |
| Spacing, strokes, radii, motion, blink | L1647-1649, L1565. |
| Damage hatching | L1651. |
| High-contrast rules | L1639, L1641, L1643: a 2 px text.primary frame on the warning fill, blocked arcs hatched in text.primary, chart text in ink only. |

## How defaults are replaced

Every item of every type in Godot's default theme gets a token value (L1742). The compiler
stops on any item it has no rule for, so an engine upgrade cannot ship Godot's look.

- Text is text.primary, support and placeholders text.secondary, and disabled text.inactive.
- Outlines are surface.inset, and shadows are clear.
- Controls have radius 2 with a 1 px rule.divider border. Hover uses a text.secondary border, pressed uses surface.inset with a text.primary border, and disabled uses surface.base.
- Focus is a 2 px ui.focus ring outside the control, with no fill.
- Docked panels are opaque surface.base with radius 0 and no border or shadow. Popups, tooltips and dialogs are surface.raised with a 1 px rule. Fields are surface.inset wells.
- A selected list row is filled with rule.divider while text.primary on rule.divider meets the text floor: 4.5:1 in dusk, where it is 6.28:1. Otherwise the row gets a 3 px select frame on surface.raised. In high contrast that pair is 3.0:1, so the frame applies. Lists draw selected text in `font_color`, so the fill can't be inverted. Text selections and popup hover invert in high contrast: a text.secondary fill under surface.inset text.
- Graph nodes and frames have radius 4. Their selection is a 3 px select border.
- Every icon slot points at an SVG in `res://ui/icons/`. The texture's colour map recolours white to a token, so no default texture remains. Scroll-bar step buttons, scroll hints and the SpinBox `updown` slot are empty textures, so the control draws nothing there. The colour picker's hue strip is a hue ramp.
- Every font slot uses Atkinson Hyperlegible Next at wght 400 with tnum, or 700 for titles. `CodeEdit/font` uses the mono. RichTextLabel's italic and mono slots use the sans, because no italic is shipped and decorative mono is banned.
- Tree, ItemList and CodeEdit use the Dense size, 20. Everything else uses the Body size, 22.

## Type variations (L1729)

All are variations of Label: set `theme_type_variation`.

| Variation | Font | Size | Colour |
| --- | --- | --- | --- |
| MenuTitle | Next 700 tnum | 44 | text.primary |
| ScreenTitle | Next 700 tnum | 33 | text.primary |
| PanelTitle | Next 700 tnum | 27 | text.primary |
| Body | Next 400 tnum | 22 | text.primary |
| Dense | Next 400 tnum | 20 | text.primary |
| Meta | Next 400 tnum | 22 | text.secondary. Bound to data in `res://ui/lint/meta_fields.txt`. |
| Code | Next 400 tnum, spacing_glyph 1 | 20 | text.primary |
| PlotLabel | Next 400 tnum, MSDF | 20 | text.primary with a 4 px surface.inset outline |
| ExpressionText | Mono 400 | 20 | text.primary |
| Traffic | Mono 400 | 20 | text.primary |
| Severity, Stance, Damage | Next 400 tnum | 22 | text.primary with a 4 px left stripe |

DESIGN.md L1729 names the expression look `Expression`, but Expression is an engine class. A Theme refuses an engine class name as a variation, and a Control then ignores the type: a Label set to `Expression` measured 22 px, the Body size. The look is therefore `ExpressionText`, and the owner needs to rule on the name in DESIGN.md.

Only Severity, Stance and Damage carry a side stripe (L1596). Each state is a sub-variation:

- Stance: `StanceFriend`, `StanceNeutral`, `StanceSuspect` (also the bare `Stance`, the default stance), and `StanceHostile`. The stripe uses the stance tint.
- Severity: `SeverityPass` (text.primary), `SeverityCaution` (alert.caution), `SeverityFail` (alert.warning), and `SeverityAdvisory` (text.secondary, also the bare `Severity`).
- Damage: `DamageIntact` (no stripe), `DamageDamaged` (text.secondary), `DamageHeavy`, `DamageDestroyed` (on a surface.inset fill), and `DamageFlooding` (text.primary). Damage uses value only.

Items on the bare families:

- `Severity`:
  - Styles `badge_caution` and `badge_warning` are the alert and check badge fills. In high contrast, the warning badge also gets a 2 px text.primary frame.
  - Style `badge_blink_off` is the 3 px outline for the off phase of a blink (L1687).
  - Colours `badge_text` and `badge_text_warning` are the glyph and word on a badge. Both are surface.inset, except text.primary on the high-contrast warning fill.
  - Colours `stripe_<state>`.
- `Damage`:
  - Colours `hatch_<level>` and `hatch_destroyed_fill`.
  - Constants `hatch_<level>_spacing`, `hatch_<level>_angle`, `hatch_destroyed_angle_2` and `hatch_width`.
  - The levels are damaged, heavy, destroyed and flooding.
- `Stance`: colours `stripe_<state>`.

## Custom drawing types (L1735)

`_draw()` code reads these with `get_theme_color(&"name", &"Plot")` and the matching getters for
other item kinds. Item names come from token keys with dots replaced by underscores, so
`stance.hostile` becomes `stance_hostile`.

Shared constants on Plot, Chart and Gauge:

- `stroke_rule` 1, `stroke_frame` 2, `stroke_icon` 2, `stroke_focus` 2, `stroke_selection` 3, `stroke_icon_grid` 24, `stroke_label_outline` 4.
- `spacing_4`, `spacing_8`, `spacing_12`, `spacing_16`, `spacing_24`, `spacing_32`, `spacing_48`.
- `target_min` 24.
- `motion_default_ms` 0, `motion_feedback_ms` 100, `motion_panel_ms` 150, `motion_ceiling_ms` 250, `motion_mode_change_box_ms` 10000.
- `blink_slow_ms` 1000, `blink_fast_ms` 400.

Plot, the plot table:

- Colours: every palette token by name, plus these roles:
  - `background`: surface.inset.
  - `rule_reference`: graticules, rings, bearing lines and scale bars.
  - `label`: text.primary.
  - `label_outline`: surface.inset.
  - `uncertainty` and `history`: text.secondary.
  - `blocked_arc_hatch`: alert.warning, or text.primary in high contrast.
- Fonts: `label` and `label_bold`, both MSDF.
- Font size: `label` 20.
- Constants: `label_outline_size` 4 and `symbol_size` 24.

Chart, the briefing chart and campaign chart:

- Colours: `water_deep`, `water_shallow`, `land`, `land_builtup`, `foreshore`, `ink`, `overlay`, `overlay_text` and `overlay_tint`, plus these roles:
  - `text`: ink.
  - `label_overlay`: overlay.text, or ink in high contrast.
  - `label_outline`: water.deep.
- Fonts: `label` and `label_bold`, both MSDF.
- Font size: `label` 20.
- Constant: `label_outline_size` 4.

Gauge:

- Colours: every palette token by name, plus these roles:
  - `background`: surface.inset.
  - `frame` and `band`: rule.divider.
  - `value` and `needle`: text.primary, the brightest.
  - `units` and `trend`: text.secondary.
  - `scale`: rule.reference.
  - `limit`: text.primary.
  - `limit_caution_active` and `limit_warning_active`: alert colours, only while the value is past that limit, per HUD rule 1.
- Fonts: `value` (bold), `units` and `scale`.
- Font sizes: `value` 22, `units` 20 and `scale` 20.
- Constants: `band_thickness` 8, `tick_length` 8 and `tick_length_minor` 4.
- Style: `panel`.

## Icons

`res://ui/icons/*.svg`: one hand-written set on a 24 px grid with 2 px strokes, round caps and joins, and white ink. The 1 px hatch lines inside the damage and fill glyphs match the damage hatch tokens. The toggles are 48 x 24. The files import as DPITexture, so they stay sharp at any UI scale. Tint them with `modulate` or `self_modulate` from a theme colour.

Every icon stem appears exactly once below.

| Group | Stems |
| --- | --- |
| Stance symbols | `stance_<friend, neutral, suspect, hostile>_<surface, air, subsurface, weapon>`: air is a bar above, subsurface a bar below, weapon a solid centre mark |
| Vertical trend | `trend_climbing`, `trend_diving`, `trend_steady` |
| ID rung | `rung_0` to `rung_4` (four bars rising in height, filled up to the rung, outlined above it) |
| Alerts | `alert_emergency` (octagon, knocked-out mark), `alert_alarm` (circle), `alert_warning` (triangle), `alert_caution` (square), `alert_silence`, `alert_cleared` |
| Editor checks | `check_pass`, `check_caution`, `check_fail` |
| Damage | `damage_intact`, `damage_damaged`, `damage_heavy`, `damage_destroyed`, `damage_flooding`, `damage_fire` |
| Overrides | `pin`, `hand`, `chip_auto`, `select_bracket` |
| Track legend | `track_bearing_wedge`, `track_fire_control`, `track_samples`, `track_history`, `track_own` |
| Part categories | `part_hull`, `part_propulsion`, `part_power`, `part_fuel`, `part_control`, `part_gun`, `part_missile`, `part_underwater`, `part_defence`, `part_offboard`, `part_sensor`, `part_crew`, `part_damage_control`, `part_special_ops` |
| Arrows | `arrow_<up, down, left, right>`, `chevron_<up, down, left, right>`, `arrow_revert`, `undo`, `redo`, `updown` |
| Gamepad, by position | `pad_face_<south, east, west, north>`, `pad_dpad_<up, down, left, right>`, `pad_shoulder_<left, right>`, `pad_trigger_<left, right>`, `pad_stick_<left, right>`, `pad_stick_<left, right>_press`, `pad_start`, `pad_select` |
| Legend patterns | `line_solid`, `line_dashed`, `line_dotted`, `line_dash_dot`, `line_double`, `fill_solid`, `fill_hatch_45`, `fill_hatch_135`, `fill_crosshatch`, `fill_horizontal`, `fill_vertical`, `fill_dots` |
| Controls | `box_checked`, `box_unchecked`, `box_mixed`, `radio_checked`, `radio_unchecked`, `toggle_on`, `toggle_off`, `close`, `add`, `remove`, `search`, `menu`, `ellipsis`, `grabber`, `grabber_line`, `tick`, `dragger`, `dragger_horizontal`, `resizer`, `port`, `dot`, `drop_mark`, `rule` |
| Files and views | `folder`, `folder_open`, `folder_add`, `folder_up`, `file`, `save`, `reload`, `visible`, `hidden`, `filter`, `sort`, `view_grid`, `view_list`, `copy`, `delete`, `link`, `lock`, `symmetry`, `camera`, `settings` |
| Graph and picker | `zoom_in`, `zoom_out`, `zoom_reset`, `grid`, `minimap`, `snap`, `layout`, `checker`, `eyedropper`, `shape_circle`, `shape_rect`, `shape_wheel`, `cursor_ring`, `cursor_ring_back`, `text_tab`, `text_space` |
