extends MarginContainer
## Splash basin options: water and target.
## DESIGN.md L216. Unit: ui-editor-splash.
## Reads and calls: Editor.splash, Editor.splash_options, Editor.set_splash_options, Editor.splash_stats.
##
## The popup under the Splash button's chevron. It picks what the next splash meets (L216): the water,
## calm, Beaufort 4 or Beaufort 7 (WMO sea-state codes 0, 3 and 6), and the target, drones or a patrol
## boat that fights back. A pick takes effect at once through `Editor.set_splash_options`, so Space and the
## Splash button start that basin; the pre-built basin (L221) is rebuilt for it while the dock is idle.
## Under the picks: whether a pre-built basin is ready for this design, and the last load time against
## the 1 s target (L218), so a slow machine can see why the pre-built or progressive splash came on.
## The popup takes focus on the selected water option; Up and Down move between the two rows.

const Kit := preload("res://ui/screens/menu_kit.gd")

## WMO sea-state codes of the three basins (L216).
const WATER_CODES: Array[int] = [0, 3, 6]
const WATER_KEYS: Array[StringName] = [&"editor_splash.water.calm", &"editor_splash.water.b4", &"editor_splash.water.b7"]
const WATER_TIPS: Array[StringName] = [&"editor_splash.water.calm.tip", &"editor_splash.water.b4.tip", &"editor_splash.water.b7.tip"]
const TARGET_KEYS: Array[StringName] = [&"editor_splash.target.drones", &"editor_splash.target.patrol"]
const TARGET_TIPS: Array[StringName] = [&"editor_splash.target.drones.tip", &"editor_splash.target.patrol.tip"]
## Load target and hard limit, milliseconds (L218).
const TARGET_MS := 1000.0

var _water: Array[Button] = []
var _target: Array[Button] = []
var _stats: Label
var _hint: Label


func _ready() -> void:
	Ui.set_margins(self, 3)
	accessibility_name = tr(&"editor_splash.options.name")
	var column := Kit.vbox(2)
	add_child(column)
	column.add_child(_line(&"editor_splash.water", &"Body"))
	var water_row := Kit.hbox(1)
	column.add_child(water_row)
	var wg := ButtonGroup.new()
	for i in WATER_KEYS.size():
		var b := _option(WATER_KEYS[i], WATER_TIPS[i], wg)
		b.pressed.connect(_on_pick)
		water_row.add_child(b)
		_water.append(b)
	column.add_child(_line(&"editor_splash.target", &"Body"))
	var target_row := Kit.hbox(1)
	column.add_child(target_row)
	var tg := ButtonGroup.new()
	for i in TARGET_KEYS.size():
		var b := _option(TARGET_KEYS[i], TARGET_TIPS[i], tg)
		b.pressed.connect(_on_pick)
		target_row.add_child(b)
		_target.append(b)
	column.add_child(HSeparator.new())
	_stats = _line(&"", &"Dense")
	_stats.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	column.add_child(_stats)
	_hint = _line(&"editor_splash.options.hint", &"Dense")
	column.add_child(_hint)
	# Focus: rows left to right, Up and Down between the rows.
	Kit.chain_horizontal(_water)
	Kit.chain_horizontal(_target)
	for i in _water.size():
		_water[i].focus_neighbor_bottom = _water[i].get_path_to(_target[mini(i, _target.size() - 1)])
		_water[i].focus_neighbor_top = _water[i].get_path_to(_water[i])
	for i in _target.size():
		_target[i].focus_neighbor_top = _target[i].get_path_to(_water[mini(i, _water.size() - 1)])
		_target[i].focus_neighbor_bottom = _target[i].get_path_to(_target[i])
	visibility_changed.connect(_on_shown)
	var e := Kit.autoload(&"Editor")
	if e != null and e.has_signal(&"job_changed"):
		e.connect(&"job_changed", func(job: StringName, _s: int) -> void:
			if job == &"prebuild" and is_visible_in_tree():
				_show_stats())
	_load()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		accessibility_name = tr(&"editor_splash.options.name")
		_show_stats()


## A label that never wraps. The dock sizes this popup while it is hidden, when a wrapping label would
## measure itself at one character wide; every line here is short enough not to need wrapping.
func _line(key: StringName, variation: StringName) -> Label:
	var l := Label.new()
	l.theme_type_variation = variation
	l.text = String(key)
	return l


func _option(key: StringName, tip: StringName, group: ButtonGroup) -> Button:
	var b := Kit.inline_button(String(key))
	b.toggle_mode = true
	b.button_group = group
	b.tooltip_text = tr(tip)
	b.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	b.set_meta(&"control_id", "editor.splash")
	return b


func _on_shown() -> void:
	if not is_visible_in_tree():
		return
	_load()
	for b: Button in _water:
		if b.button_pressed:
			Ui.focus(b)
			return


## Read the options the next splash will use and mark them.
func _load() -> void:
	var o := Kit.as_dict(Kit.call_on(&"Editor", &"splash_options"))
	var water := int(o.get("water", 3))
	var best := 0
	for i in WATER_CODES.size():
		if absi(WATER_CODES[i] - water) < absi(WATER_CODES[best] - water):
			best = i
	_water[best].set_pressed_no_signal(true)
	var target := clampi(int(o.get("target", Bp.SPLASH_TARGET_DRONES)), 0, _target.size() - 1)
	_target[target].set_pressed_no_signal(true)
	_show_stats()


func _picked(buttons: Array[Button]) -> int:
	for i in buttons.size():
		if buttons[i].button_pressed:
			return i
	return 0


func _on_pick() -> void:
	var options := {"water": WATER_CODES[_picked(_water)], "target": _picked(_target)}
	Kit.call_on(&"Editor", &"set_splash_options", [options])
	_show_stats()


func _show_stats() -> void:
	var s := Kit.as_dict(Kit.call_on(&"Editor", &"splash_stats"))
	var lines: PackedStringArray = []
	if bool(s.get("ready", false)):
		lines.append(tr(&"editor_splash.stats.ready"))
	elif bool(s.get("prebuilt", false)):
		lines.append(tr(&"editor_splash.stats.preparing"))
	var last := float(s.get("last_ms", 0.0))
	if last > 0.0:
		lines.append(tr(&"editor_splash.stats.last").format([Kit.number(last / 1000.0, &"s", 1), Kit.number(float(s.get("p95_ms", 0.0)) / 1000.0, &"s", 1)]))
		if last > TARGET_MS:
			lines.append(tr(&"editor_splash.stats.slow"))
	if bool(s.get("progressive", false)):
		lines.append(tr(&"editor_splash.stats.progressive"))
	_stats.text = "\n".join(lines)
	_stats.visible = not lines.is_empty()
