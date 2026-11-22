extends Node
## Autoload `Ui`. Token lookups from res://ui/theme/tokens.json, UI scale (drives
## get_tree().root.content_scale_factor, never times OS DPI), palette switch, reduce motion,
## blink-off and motion durations. DESIGN.md L1620-1651, L1725-1744. Unit: ui-shell.
## Reads and calls: Settings.get_value, Settings.changed.
##
## Start-up: `_ready` reads the token file only and calls no Rust facade, so the headless smoke test
## and the compile check stay quiet. `boot.gd` calls `refresh()` once after `Settings.reload()`;
## from then on every `access.*` change arrives through `Settings.changed`.
##
## Surface for other scripts:
## - `changed(what)` fires after a look or behaviour changed. `what` is one of `&"scale"`,
##   `&"palette"`, `&"motion"`, `&"blink"`, `&"transparency"`, `&"flashing"`, `&"backing"` or
##   `&"officer_lines"`. Theme colours re-skin by themselves (the palette switch rewrites the
##   project theme, which notifies every Control); `_draw()` widgets redraw on
##   NOTIFICATION_THEME_CHANGED like any Control.
## - `blink_changed(slow_on, fast_on)` fires only when a blink phase flips: slow at 1 Hz, fast at
##   2.5 Hz (L1649). Every blinking mark shares this clock, so marks blink in step and a widget
##   redraws twice a cycle instead of every frame. While `blink_off` is true both phases hold on
##   and blinking widgets draw the 3 px outline instead (L1687).
## - `input_device_changed(gamepad)` fires when the player moves between keyboard or mouse and a
##   gamepad. Shortcut hints follow `using_gamepad`.
## - Colours: `colour(&"text.primary")`, `colour(&"chart.ink")`, `scrim_colour()`. Values come from
##   the token file for the palette in use, never from a literal.
## - Tokens: `token("motion_ms.panel")`, `spacing(2)` (the third step, 12), `font_size(&"Body")`.
## - Motion: `motion_s(&"feedback")` for movement, `fade_s(&"panel")` for fades. Reduce motion turns
##   movement instant and every fade into a 150 ms fade, never into no feedback (L1721).
## - `tween_for(node, &"Session.state_changed")` makes an EASE_OUT tween that names the sim signal it
##   reports in its `sim_signal` metadata (L1737).
## - `set_separation(box, 3)`, `set_margins(margin, 4)`: spacing constants from the token steps.
## - `icon(&"alert_alarm")` loads one icon of the set in res://ui/icons.
## - `focus(control)` grabs focus deferred, showing the ring only when the keyboard or a gamepad is
##   in use (L1738).

signal changed(what: StringName)
signal blink_changed(slow_on: bool, fast_on: bool)
signal input_device_changed(gamepad: bool)

const TOKENS_PATH := "res://ui/theme/tokens.json"
const ICON_DIR := "res://ui/icons"
const THEME_PATHS: Dictionary[StringName, String] = {
	&"dusk": "res://ui/theme/project_theme.tres",
	&"high_contrast": "res://ui/theme/project_theme_high_contrast.tres",
}
## `access.palette` values in the settings schema, in order.
const PALETTE_BY_SETTING: Array[StringName] = [&"dusk", &"high_contrast"]
## Reduce motion swaps movement for fades of this length (L1721).
const REDUCED_FADE_MS := 150
## A stick has to move this far before the player counts as using the gamepad.
const STICK_WAKE := 0.5
## Mouse movement under this many pixels in one event does not switch away from the gamepad.
const MOUSE_WAKE_PX := 4.0
## The shortest blink cycle allowed: 2.5 Hz (L1608, cycle 400 ms or more).
const MIN_BLINK_CYCLE_MS := 400

const Compiler := preload("res://ui/theme/theme_compiler.gd")

var ui_scale := 1.0
var palette: StringName = &"dusk"
var reduce_motion := false
var blink_off := false
var reduce_transparency := false
var reduce_flashing := false
var hud_backing_opacity := 1.0
var officer_line_scale := 1.0
var officer_line_opacity := 1.0
## True while a gamepad is the last device used.
var using_gamepad := false
## True while the mouse, not keys or a gamepad, made the last press.
var using_mouse := true
var blink_slow_on := true
var blink_fast_on := true

var _tokens: Dictionary = {}
var _colours: Dictionary[StringName, Color] = {}
var _icons: Dictionary[StringName, Texture2D] = {}
## Unmodified copies of both compiled themes, so a switch can always restore either look.
var _pristine: Dictionary[StringName, Theme] = {}
var _slow_cycle_ms := 1000
var _fast_cycle_ms := 400
var _settings_ready := false


func _ready() -> void:
	process_mode = Node.PROCESS_MODE_ALWAYS
	_load_tokens()
	_rebuild_colours()
	var blink: Dictionary = _tokens.get("blink_hz", {})
	_slow_cycle_ms = _cycle_ms(float(blink.get("slow", 1.0)))
	_fast_cycle_ms = _cycle_ms(float(blink.get("fast", 2.5)))
	var settings := get_node_or_null(^"/root/Settings")
	if settings != null and settings.has_signal(&"changed"):
		settings.connect(&"changed", _on_setting_changed)


static func _cycle_ms(hz: float) -> int:
	if not is_finite(hz) or hz <= 0.0:
		return 1000
	return maxi(MIN_BLINK_CYCLE_MS, roundi(1000.0 / hz))


func _process(_delta: float) -> void:
	if blink_off:
		return
	var now := Time.get_ticks_msec()
	var slow := (now % _slow_cycle_ms) < _slow_cycle_ms / 2
	var fast := (now % _fast_cycle_ms) < _fast_cycle_ms / 2
	if slow != blink_slow_on or fast != blink_fast_on:
		blink_slow_on = slow
		blink_fast_on = fast
		blink_changed.emit(slow, fast)


func _input(event: InputEvent) -> void:
	var pad := using_gamepad
	if event is InputEventJoypadButton:
		pad = true
		using_mouse = false
	elif event is InputEventJoypadMotion:
		if absf((event as InputEventJoypadMotion).axis_value) >= STICK_WAKE:
			pad = true
			using_mouse = false
	elif event is InputEventKey:
		if event.is_pressed():
			pad = false
			using_mouse = false
	elif event is InputEventMouseButton:
		if event.is_pressed():
			pad = false
			using_mouse = true
	elif event is InputEventMouseMotion:
		if (event as InputEventMouseMotion).relative.length() >= MOUSE_WAKE_PX:
			pad = false
	if pad != using_gamepad:
		using_gamepad = pad
		input_device_changed.emit(pad)
	if _reveal_focus(event):
		return
	_leave_list_at_end(event)


## A screen opened by the pointer takes focus with the ring hidden. The first arrow or D-pad press
## then shows the ring where focus already is instead of moving it, so the player sees where they
## start (the menu's first button is not skipped).
func _reveal_focus(event: InputEvent) -> bool:
	if event is InputEventMouse or not event.is_pressed() or event.is_echo():
		return false
	var nav := false
	for action: StringName in [&"ui_up", &"ui_down", &"ui_left", &"ui_right"]:
		if event.is_action_pressed(action, false, true):
			nav = true
	if not nav:
		return false
	var vp := get_viewport()
	var f := vp.gui_get_focus_owner()
	if f == null or f.has_focus(true):
		return false
	f.grab_focus()
	vp.set_input_as_handled()
	return true


## An ItemList keeps Up and Down for itself even on its first and last rows, so arrows and the
## D-pad could never move focus past a list (a skirmish's operation list hid everything under it,
## Start included). On the first or last selectable row a fresh press (not a key repeat) moves focus
## to the list's neighbour on that side instead (L1738: every control reachable, no focus traps).
func _leave_list_at_end(event: InputEvent) -> void:
	var down := event.is_action_pressed(&"ui_down", false, true)
	if not down and not event.is_action_pressed(&"ui_up", false, true):
		return
	var vp := get_viewport()
	var list := vp.gui_get_focus_owner() as ItemList
	if list == null or list.max_columns != 1:
		return
	var first := -1
	var last := -1
	for i: int in list.item_count:
		if list.is_item_selectable(i) and not list.is_item_disabled(i):
			if first < 0:
				first = i
			last = i
	var sel := list.get_selected_items()
	var cur := sel[0] if not sel.is_empty() else -1
	var at_end := last < 0 or (down and cur >= last) or (not down and cur >= 0 and cur <= first)
	if not at_end:
		return
	var next := list.find_valid_focus_neighbor(SIDE_BOTTOM if down else SIDE_TOP)
	if next == null or next == list:
		return
	next.grab_focus()
	vp.set_input_as_handled()


# --- settings ------------------------------------------------------------------------------------

## Read every `access.*` setting and apply it. Called by boot after `Settings.reload()`.
func refresh() -> void:
	_settings_ready = true
	_apply_scale(_setting_float(&"access.ui_scale", 1.0))
	_apply_palette(_palette_from_setting())
	_apply_motion(_setting_bool(&"access.reduce_motion", false), _setting_bool(&"access.blink_off", false))
	_apply_transparency(_setting_bool(&"access.reduce_transparency", false))
	_apply_flashing(_setting_bool(&"access.reduce_flashing", false))
	_apply_backing(_setting_float(&"access.hud_backing_opacity", 1.0))
	_apply_officer_lines(_setting_float(&"access.officer_line_scale", 1.0), _setting_float(&"access.officer_line_opacity", 1.0))


func _on_setting_changed(key: StringName) -> void:
	if not _settings_ready:
		return
	match String(key):
		"access.ui_scale":
			_apply_scale(_setting_float(key, 1.0))
		"access.palette":
			_apply_palette(_palette_from_setting())
		"access.reduce_motion", "access.blink_off":
			_apply_motion(_setting_bool(&"access.reduce_motion", false), _setting_bool(&"access.blink_off", false))
		"access.reduce_transparency":
			_apply_transparency(_setting_bool(key, false))
		"access.reduce_flashing":
			_apply_flashing(_setting_bool(key, false))
		"access.hud_backing_opacity":
			_apply_backing(_setting_float(key, 1.0))
		"access.officer_line_scale", "access.officer_line_opacity":
			_apply_officer_lines(_setting_float(&"access.officer_line_scale", 1.0), _setting_float(&"access.officer_line_opacity", 1.0))
		_:
			# A section reset or a reload may report a key outside access, or none: re-read all.
			if not String(key).contains("."):
				refresh()


func _palette_from_setting() -> StringName:
	var index := _setting_int(&"access.palette", 0)
	return PALETTE_BY_SETTING[clampi(index, 0, PALETTE_BY_SETTING.size() - 1)]


func _setting(key: StringName) -> Variant:
	var settings := get_node_or_null(^"/root/Settings")
	if settings == null or not settings.has_method(&"get_value"):
		return null
	return settings.call(&"get_value", key)


func _setting_float(key: StringName, fallback: float) -> float:
	var v: Variant = _setting(key)
	if v is float or v is int:
		return float(v)
	return fallback


func _setting_int(key: StringName, fallback: int) -> int:
	var v: Variant = _setting(key)
	if v is int or v is float:
		return int(v)
	return fallback


func _setting_bool(key: StringName, fallback: bool) -> bool:
	var v: Variant = _setting(key)
	if v is bool:
		return v
	if v is int:
		return int(v) != 0
	return fallback


func _apply_scale(value: float) -> void:
	var s := clamp_scale(value)
	get_tree().root.content_scale_factor = s
	if not is_equal_approx(s, ui_scale):
		ui_scale = s
		changed.emit(&"scale")


## UI scale clamped to the 75% to 200% the accessibility floors allow (L1714).
func clamp_scale(value: float) -> float:
	var limits: Dictionary = _tokens.get("ui_scale", {})
	var lo := float(limits.get("min", 0.75))
	var hi := float(limits.get("max", 2.0))
	return clampf(value if is_finite(value) else 1.0, lo, hi)


func _apply_palette(name: StringName) -> void:
	if not THEME_PATHS.has(name):
		name = &"dusk"
	if name == palette:
		return
	var project := ThemeDB.get_project_theme()
	var source := _pristine_theme(name)
	if project == null or source == null:
		push_error("Ui: cannot switch to the %s palette" % name)
		return
	# Make sure the look we leave can be restored before the shared theme is rewritten.
	_pristine_theme(palette)
	# The project theme object is shared by every Control, those under a CanvasLayer included, so
	# merging the other palette into it re-skins the whole game at once. A Window theme would not
	# reach Controls under a CanvasLayer.
	project.merge_with(source)
	palette = name
	_rebuild_colours()
	changed.emit(&"palette")


func _pristine_theme(name: StringName) -> Theme:
	if _pristine.has(name):
		return _pristine[name]
	var theme := ResourceLoader.load(THEME_PATHS[name], "Theme", ResourceLoader.CACHE_MODE_IGNORE) as Theme
	if theme != null:
		_pristine[name] = theme
	return theme


func _apply_motion(reduce: bool, no_blink: bool) -> void:
	var motion_changed := reduce != reduce_motion
	var blink_changed_now := no_blink != blink_off
	reduce_motion = reduce
	blink_off = no_blink
	if blink_changed_now:
		blink_slow_on = true
		blink_fast_on = true
		blink_changed.emit(true, true)
		changed.emit(&"blink")
	if motion_changed:
		changed.emit(&"motion")


func _apply_transparency(on: bool) -> void:
	if on != reduce_transparency:
		reduce_transparency = on
		changed.emit(&"transparency")


func _apply_flashing(on: bool) -> void:
	if on != reduce_flashing:
		reduce_flashing = on
		changed.emit(&"flashing")


func _apply_backing(value: float) -> void:
	var v := clampf(value if is_finite(value) else 1.0, 0.0, 1.0)
	if not is_equal_approx(v, hud_backing_opacity):
		hud_backing_opacity = v
		changed.emit(&"backing")


func _apply_officer_lines(scale_value: float, opacity: float) -> void:
	var s := clampf(scale_value if is_finite(scale_value) else 1.0, 1.0, 2.0)
	var o := clampf(opacity if is_finite(opacity) else 1.0, 0.0, 1.0)
	if not is_equal_approx(s, officer_line_scale) or not is_equal_approx(o, officer_line_opacity):
		officer_line_scale = s
		officer_line_opacity = o
		changed.emit(&"officer_lines")


# --- tokens --------------------------------------------------------------------------------------

func _load_tokens() -> void:
	var text := FileAccess.get_file_as_string(TOKENS_PATH)
	var parsed: Variant = JSON.parse_string(text) if not text.is_empty() else null
	if parsed is Dictionary:
		_tokens = parsed
	else:
		push_error("Ui: cannot read %s" % TOKENS_PATH)
		_tokens = {}


func _rebuild_colours() -> void:
	_colours.clear()
	var palettes: Dictionary = _tokens.get("palettes", {})
	var current: Dictionary = palettes.get(String(palette), {})
	for key: String in current:
		_colours[StringName(key)] = Compiler.hex(String(current[key]))
	var chart: Dictionary = palettes.get("chart", {})
	for key: String in chart:
		_colours[StringName("chart." + key)] = Compiler.hex(String(chart[key]))


## The parsed token file. Treat it as read-only.
func tokens() -> Dictionary:
	return _tokens


## A token by dotted path into tokens.json, for example `"motion_ms.panel"` or `"radius.control"`.
## Returns `fallback` when the path does not exist. Palette keys contain dots, so read colours
## through `colour()`.
func token(path: String, fallback: Variant = null) -> Variant:
	var node: Variant = _tokens
	for part: String in path.split("."):
		if node is Dictionary and (node as Dictionary).has(part):
			node = (node as Dictionary)[part]
		else:
			return fallback
	return node


## A colour of the palette in use by token name (`&"text.primary"`, `&"alert.caution"`), or a chart
## colour (`&"chart.ink"`). An unknown name reports an error and gives text.primary, never a
## made-up hue.
func colour(name: StringName) -> Color:
	if _colours.has(name):
		return _colours[name]
	push_error("Ui: no colour token %s" % name)
	var unknown: Color
	return _colours.get(&"text.primary", unknown)


## True when the palette in use has this colour token.
func has_colour(name: StringName) -> bool:
	return _colours.has(name)


## The pause or dialog scrim: surface.inset at 72%, opaque under reduce transparency (L1604).
func scrim_colour() -> Color:
	var c := colour(&"surface.inset")
	c.a = 1.0 if reduce_transparency else 0.72
	return c


## Spacing step `index` (0-based) of 4, 8, 12, 16, 24, 32, 48.
func spacing(index: int) -> int:
	var steps: Array = _tokens.get("spacing", [])
	if steps.is_empty():
		return 8
	return int(steps[clampi(index, 0, steps.size() - 1)])


## Set a box container's separation to spacing step `index` (L1647: spacing from tokens only).
func set_separation(container: Control, index: int) -> void:
	container.add_theme_constant_override(&"separation", spacing(index))


## Set all four margins of a MarginContainer to spacing step `index`.
func set_margins(container: MarginContainer, index: int) -> void:
	var px := spacing(index)
	for side: StringName in [&"margin_left", &"margin_top", &"margin_right", &"margin_bottom"]:
		container.add_theme_constant_override(side, px)


## Theme font size of a type variation (`&"Body"` is 22 at 1080p).
func font_size(variation: StringName) -> int:
	var types: Dictionary = _tokens.get("type", {})
	var entry: Dictionary = types.get(String(variation), types.get("Body", {}))
	return int(entry.get("font_size", 22))


## Seconds for a movement of this kind (`&"default"`, `&"feedback"`, `&"panel"`, `&"ceiling"`):
## 0 under reduce motion, so the change is instant.
func motion_s(kind: StringName) -> float:
	if reduce_motion:
		return 0.0
	return _motion_ms(kind) / 1000.0


## Seconds for a fade of this kind. Under reduce motion every fade lasts 150 ms (L1721).
func fade_s(kind: StringName) -> float:
	if reduce_motion:
		return REDUCED_FADE_MS / 1000.0
	return _motion_ms(kind) / 1000.0


## How long a mode-change box stays up, in seconds (L1649).
func mode_box_s() -> float:
	var motion: Dictionary = _tokens.get("motion_ms", {})
	return float(motion.get("mode_change_box", 10000)) / 1000.0


func _motion_ms(kind: StringName) -> float:
	var motion: Dictionary = _tokens.get("motion_ms", {})
	var ceiling := float(motion.get("ceiling", 250))
	return clampf(float(motion.get(String(kind), 0)), 0.0, ceiling)


## Whether a blinking mark is in its on phase. With blink-off it always is.
func blink_on(fast: bool) -> bool:
	if blink_off:
		return true
	return blink_fast_on if fast else blink_slow_on


## A tween on `node` that reports `sim_signal` (for example `&"Hud.alerts_changed"`): EASE_OUT,
## cubic. Animate only `offset_transform_*` or modulate with it, for token durations.
func tween_for(node: Node, sim_signal: StringName) -> Tween:
	var tween := node.create_tween()
	tween.set_meta(&"sim_signal", sim_signal)
	tween.set_ease(Tween.EASE_OUT)
	tween.set_trans(Tween.TRANS_CUBIC)
	return tween


## One icon of the hand-written set, by stem (`&"close"`), or null when the set has no such icon.
func icon(stem: StringName) -> Texture2D:
	if _icons.has(stem):
		return _icons[stem]
	var path := "%s/%s.svg" % [ICON_DIR, stem]
	var tex: Texture2D = null
	if ResourceLoader.exists(path):
		tex = load(path) as Texture2D
	if tex == null:
		push_error("Ui: no icon %s" % stem)
	_icons[stem] = tex
	return tex


## Give `control` focus on the next idle frame. The ring shows unless the mouse made the last press.
func focus(control: Control) -> void:
	if control == null or not is_instance_valid(control):
		return
	if control.focus_mode == Control.FOCUS_NONE:
		return
	control.grab_focus.call_deferred(using_mouse and not using_gamepad)
