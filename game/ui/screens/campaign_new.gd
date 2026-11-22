extends Control
## New campaign: slot name, difficulty preset and sliders, Ironman, starter hull.
## DESIGN.md L847-860, L142-151. Unit: ui-campaign-menu.
## Reads and calls: Campaign.new_campaign, Content.difficulty_presets, Content.difficulty_ranges, Content.starters.
##
## A base screen (opened with `Screens.change`), so a start that fails stays here with its problems
## shown above the Start button. Top to bottom, in the order a player decides, the choices every
## player makes first and the fine-tuning after them:
## - Name: prefilled "Campaign N", so Start works without typing.
## - Difficulty: the presets as one row, easiest first with Normal chosen (L851).
## - Ironman: one toggle with what it changes (L826-828, L860).
## - First hull: the starters Act 1 offers (L142-151), the 22 m fast attack craft chosen, since it is
##   the first starter a player meets. If the list cannot be read the campaign still starts with
##   that default (`starter` left out).
## - Difficulty in detail: the six sliders with their ranges from `Content.difficulty_ranges()`. Moving
##   a slider off every preset makes the difficulty Custom (L858); moving it back onto one selects that
##   preset again. Hull count says it locks at the start because it sizes the theatre (L860). There is
##   no damage slider (L849).
## - Seed: blank for random; a number is used as it is, any other text is hashed so a word can be
##   shared as a seed.
## Esc and Back return to where the player came from: the campaign list when there are saves, the
## main menu otherwise.

const Kit := preload("res://ui/screens/menu_kit.gd")

const ACT_1 := 0
const NAME_MAX := 40
## The slider rows in spec order (L851-856): DifficultyDict key, value format, label and help keys.
const SLIDERS: Array[Dictionary] = [
	{"key": "ai_accuracy", "format": &"times"},
	{"key": "ai_reaction_s", "format": &"seconds"},
	{"key": "adaptation_cadence", "format": &"times"},
	{"key": "hull_count", "format": &"hulls"},
	{"key": "economy", "format": &"times"},
	{"key": "clock_days", "format": &"days"},
]
## Spec values (L851-856), used only when the content tables cannot be read.
const FALLBACK_RANGES: Dictionary = {
	"ai_accuracy": {"min": 0.6, "max": 2.0, "step": 0.05, "locks_at_start": false},
	"ai_reaction_s": {"min": 0.8, "max": 4.0, "step": 0.1, "locks_at_start": false},
	"adaptation_cadence": {"min": 0.25, "max": 2.0, "step": 0.05, "locks_at_start": false},
	"hull_count": {"min": 200.0, "max": 450.0, "step": 10.0, "locks_at_start": true},
	"economy": {"min": 0.7, "max": 1.5, "step": 0.05, "locks_at_start": false},
	"clock_days": {"min": 28.0, "max": 60.0, "step": 1.0, "locks_at_start": false},
}
const FALLBACK_PRESETS: Array[Dictionary] = [
	{"preset": &"normal", "name": &"difficulty.normal", "ai_accuracy": 1.0, "ai_reaction_s": 2.0, "adaptation_cadence": 1.0, "hull_count": 300, "economy": 1.0, "clock_days": 40},
	{"preset": &"easy", "name": &"difficulty.easy", "ai_accuracy": 1.5, "ai_reaction_s": 3.0, "adaptation_cadence": 0.5, "hull_count": 220, "economy": 1.25, "clock_days": 50},
	{"preset": &"hard", "name": &"difficulty.hard", "ai_accuracy": 0.75, "ai_reaction_s": 1.2, "adaptation_cadence": 1.5, "hull_count": 380, "economy": 0.85, "clock_days": 32},
]
## The first starter a player meets (L151): the starter nearest this length is chosen first.
const FIRST_STARTER_M := 22.0

@onready var _back: Button = $Margin/Column/Header/Back
@onready var _form: VBoxContainer = $Margin/Column/Scroll/Form
@onready var _problems: Label = $Margin/Column/Footer/Problems
@onready var _start: Button = $Margin/Column/Footer/Start

var _presets: Array[Dictionary] = []
var _ranges: Dictionary = {}
var _starters: Array[Dictionary] = []
var _values: Dictionary = {}
var _preset_buttons: Dictionary[StringName, Button] = {}
var _preset_group := ButtonGroup.new()
var _custom_label: Label
var _sliders: Dictionary[String, HSlider] = {}
var _slider_values: Dictionary[String, Label] = {}
var _name: LineEdit
var _ironman: CheckButton
var _starter_group := ButtonGroup.new()
var _starter_buttons: Array[Button] = []
var _starter_path := ""
var _seed: LineEdit
var _has_slots := false
var _tutorial_done := false
var _syncing := false
var _busy := false

var _theme_started := false

func _ready() -> void:
	Ui.set_margins($Margin, 5)
	Ui.set_separation($Margin/Column, 3)
	Ui.set_separation(_form, 5)
	Ui.set_separation($Margin/Column/Footer, 4)
	_problems.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_problems.text = ""
	_back.pressed.connect(_leave)
	_start.pressed.connect(_on_start)
	accessibility_name = tr(&"campaign_menu.new.title")


func _exit_tree() -> void:
	Kit.leave_menu_theme(_theme_started)


func setup(_args: Dictionary) -> void:
	_theme_started = true
	Kit.start_menu_theme()
	var presets := Kit.dicts(Kit.call_on(&"Content", &"difficulty_presets"))
	var ranges := Kit.as_dict(Kit.call_on(&"Content", &"difficulty_ranges"))
	var starters := Kit.dicts(Kit.call_on(&"Content", &"starters", [ACT_1]))
	var slots: Variant = Kit.call_on(&"Campaign", &"slots")
	_has_slots = slots is Array and not (slots as Array).is_empty()
	_tutorial_done = Kit.call_on(&"Tutorial", &"was_finished") == true
	build(presets, ranges, starters, (slots as Array).size() if slots is Array else 0)


func default_focus() -> Control:
	return _start


func _unhandled_input(event: InputEvent) -> void:
	if event.is_action_pressed(&"ui_cancel", false, true) and Screens.top() == self:
		get_viewport().set_input_as_handled()
		_leave()


## Build the form from plain data (also the test entry point). `slot_count` numbers the default name.
func build(presets: Array[Dictionary], ranges: Dictionary, starters: Array[Dictionary], slot_count: int) -> void:
	_presets = presets if not presets.is_empty() else FALLBACK_PRESETS.duplicate(true)
	_ranges = ranges if not ranges.is_empty() else FALLBACK_RANGES.duplicate(true)
	_starters = starters
	Kit.clear(_form)
	_preset_buttons.clear()
	_sliders.clear()
	_slider_values.clear()
	_starter_buttons.clear()
	_build_name(slot_count)
	_build_difficulty()
	_build_ironman()
	_build_starters()
	_build_sliders()
	_build_seed()
	apply_preset(StringName(Kit.get_str(_presets[0], "preset", "normal")))
	_link_focus()


func _section(title_key: String) -> VBoxContainer:
	var box := Kit.vbox(2)
	box.add_child(Kit.label(title_key, &"PanelTitle"))
	_form.add_child(box)
	return box


func _build_name(slot_count: int) -> void:
	var box := _section("campaign_menu.new.name")
	_name = LineEdit.new()
	_name.max_length = NAME_MAX
	_name.custom_minimum_size = Vector2(560.0, Kit.BUTTON_H)
	_name.size_flags_horizontal = Control.SIZE_SHRINK_BEGIN
	_name.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_name.text = tr(&"campaign_menu.new.default_name").format([slot_count + 1])
	_name.accessibility_name = tr(&"campaign_menu.new.name")
	_name.select_all_on_focus = true
	_name.text_changed.connect(func(_t: String) -> void: _update_start())
	box.add_child(_name)


func _build_difficulty() -> void:
	var box := _section("campaign_menu.new.difficulty")
	var row := Kit.hbox(2)
	for p: Dictionary in Kit.difficulty_order(_presets):
		var id := StringName(Kit.get_str(p, "preset"))
		if id == &"":
			continue
		var b := Button.new()
		b.toggle_mode = true
		b.button_group = _preset_group
		b.text = Kit.get_str(p, "name", "difficulty." + String(id))
		b.custom_minimum_size = Vector2(180.0, Kit.BUTTON_H)
		b.focus_mode = Control.FOCUS_ALL
		b.pressed.connect(apply_preset.bind(id))
		row.add_child(b)
		_preset_buttons[id] = b
	_custom_label = Kit.label("campaign_menu.new.custom", &"Body")
	_custom_label.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	row.add_child(_custom_label)
	box.add_child(row)
	box.add_child(Kit.label("campaign_menu.new.difficulty.help", &"Body"))


func _build_sliders() -> void:
	var box := _section("campaign_menu.new.sliders")
	for s: Dictionary in SLIDERS:
		box.add_child(_slider_row(s))


func _slider_row(s: Dictionary) -> VBoxContainer:
	var key := String(s["key"])
	var r: Dictionary = Kit.as_dict(_ranges.get(key, FALLBACK_RANGES[key]))
	var holder := Kit.vbox(0)
	var row := Kit.hbox(3)
	var name_label := Kit.label("campaign_menu.slider." + key, &"Body")
	name_label.custom_minimum_size = Vector2(300.0, 0.0)
	name_label.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	var slider := preload("res://ui/widgets/focus_slider.gd").new()
	slider.min_value = Kit.get_float(r, "min", 0.0)
	slider.max_value = Kit.get_float(r, "max", 1.0)
	slider.step = maxf(Kit.get_float(r, "step", 0.05), 0.0001)
	slider.custom_minimum_size = Vector2(340.0, Kit.BUTTON_H)
	slider.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	slider.accessibility_name = tr(StringName("campaign_menu.slider." + key))
	slider.value_changed.connect(_on_slider.bind(key))
	var value := Kit.label("", &"Body", true)
	value.custom_minimum_size = Vector2(150.0, 0.0)
	value.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	value.autowrap_mode = TextServer.AUTOWRAP_OFF
	row.add_child(name_label)
	row.add_child(slider)
	row.add_child(value)
	holder.add_child(row)
	var help := "campaign_menu.slider." + key + ".help"
	holder.add_child(Kit.label(help, &"Body"))
	if Kit.get_bool(r, "locks_at_start"):
		holder.add_child(Kit.label("campaign_menu.slider.locks", &"Body"))
	_sliders[key] = slider
	_slider_values[key] = value
	return holder


func _build_ironman() -> void:
	var box := _section("campaign_menu.new.rules")
	_ironman = CheckButton.new()
	_ironman.text = "campaign_menu.new.ironman"
	_ironman.custom_minimum_size = Vector2(560.0, Kit.BUTTON_H)
	_ironman.size_flags_horizontal = Control.SIZE_SHRINK_BEGIN
	box.add_child(_ironman)
	box.add_child(Kit.label("campaign_menu.new.ironman.help", &"Body"))


func _build_starters() -> void:
	var box := _section("campaign_menu.new.starter")
	if _starters.is_empty():
		_starter_path = ""
		box.add_child(Kit.label("campaign_menu.new.starter.unread", &"Body"))
		return
	var row := HFlowContainer.new()
	row.add_theme_constant_override(&"h_separation", Ui.spacing(2))
	row.add_theme_constant_override(&"v_separation", Ui.spacing(2))
	var best := 0
	var best_gap := INF
	for i in _starters.size():
		var gap := absf(Kit.get_float(_starters[i], "length_m", 0.0) - FIRST_STARTER_M)
		if gap < best_gap:
			best_gap = gap
			best = i
	for i in _starters.size():
		var st := _starters[i]
		var b := Button.new()
		b.toggle_mode = true
		b.button_group = _starter_group
		b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		b.custom_minimum_size = Vector2(270.0, 96.0)
		b.focus_mode = Control.FOCUS_ALL
		b.alignment = HORIZONTAL_ALIGNMENT_LEFT
		b.text = starter_text(st)
		b.accessibility_name = b.text.replace("\n", ", ")
		b.pressed.connect(_on_starter.bind(Kit.get_str(st, "path")))
		row.add_child(b)
		_starter_buttons.append(b)
		if i == best:
			b.set_pressed_no_signal(true)
			_starter_path = Kit.get_str(st, "path")
	box.add_child(row)


## Two lines for a starter tile: its name, then length, displacement and build cost.
static func starter_text(st: Dictionary) -> String:
	var facts := Kit.tr_key(&"campaign_menu.new.starter.facts").format([
		Kit.number(Kit.get_float(st, "length_m"), &"m", 0),
		Kit.number(Kit.get_float(st, "displacement_t"), &"t", 0),
		Kit.number(float(Kit.get_int(st, "cost")), &"salvage", 0),
	])
	return "%s\n%s" % [Kit.get_str(st, "name"), facts]


func _build_seed() -> void:
	var box := _section("campaign_menu.new.seed")
	_seed = LineEdit.new()
	_seed.custom_minimum_size = Vector2(360.0, Kit.BUTTON_H)
	_seed.size_flags_horizontal = Control.SIZE_SHRINK_BEGIN
	_seed.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_seed.placeholder_text = tr(&"campaign_menu.new.seed.placeholder")
	_seed.accessibility_name = tr(&"campaign_menu.new.seed")
	_seed.max_length = 32
	box.add_child(_seed)
	box.add_child(Kit.label("campaign_menu.new.seed.help", &"Body"))


func _link_focus() -> void:
	var order: Array = [_name]
	var presets: Array = _preset_buttons.values()
	Kit.chain_horizontal(presets)
	if not presets.is_empty():
		order.append(presets[0])
	order.append(_ironman)
	Kit.chain_horizontal(_starter_buttons)
	if not _starter_buttons.is_empty():
		order.append(_starter_buttons[0])
	for s: Dictionary in SLIDERS:
		order.append(_sliders[String(s["key"])])
	order.append(_seed)
	order.append(_start)
	var live := Kit.chain_vertical(order)
	# Rows of several controls share the Up and Down of their first one.
	for group: Array in [presets, _starter_buttons]:
		if group.is_empty():
			continue
		var first := group[0] as Control
		for c: Variant in group:
			var control := c as Control
			control.focus_neighbor_top = control.get_path_to(first.get_node(first.focus_neighbor_top))
			control.focus_neighbor_bottom = control.get_path_to(first.get_node(first.focus_neighbor_bottom))
	if not live.is_empty():
		live[0].focus_neighbor_top = live[0].get_path_to(_back)
	_back.focus_neighbor_bottom = _back.get_path_to(_name)
	_back.focus_neighbor_top = _back.get_path_to(_back)
	for c: Control in [_name, _ironman, _seed, _back]:
		c.focus_neighbor_left = c.get_path_to(c)
		c.focus_neighbor_right = c.get_path_to(c)
	_start.focus_neighbor_bottom = _start.get_path_to(_start)
	_enter_rows()


## Up and Down into the preset row and the starter row land on the chosen tile, not the first one.
func _enter_rows() -> void:
	if _name == null or _sliders.is_empty():
		return
	_point_into(_name, _ironman, _pressed_in(_preset_buttons.values()))
	_point_into(_ironman, _sliders[String(SLIDERS[0]["key"])], _pressed_in(_starter_buttons))


static func _pressed_in(buttons: Array) -> Button:
	for b: Variant in buttons:
		if b is Button and is_instance_valid(b) and (b as Button).button_pressed:
			return b
	return null


static func _point_into(above: Control, below: Control, target: Control) -> void:
	if target == null or not is_instance_valid(above) or not is_instance_valid(below):
		return
	above.focus_neighbor_bottom = above.get_path_to(target)
	below.focus_neighbor_top = below.get_path_to(target)


# --- difficulty ----------------------------------------------------------------------------------

## Set every slider to a preset's values.
func apply_preset(id: StringName) -> void:
	for p: Dictionary in _presets:
		if StringName(Kit.get_str(p, "preset")) == id:
			_syncing = true
			for s: Dictionary in SLIDERS:
				var key := String(s["key"])
				var v := Kit.get_float(p, key, _sliders[key].value)
				_sliders[key].value = v
				_values[key] = _sliders[key].value
				_slider_values[key].text = value_text(key, _sliders[key].value)
			_syncing = false
	_sync_preset()


func _on_slider(value: float, key: String) -> void:
	_values[key] = value
	_slider_values[key].text = value_text(key, value)
	if not _syncing:
		_sync_preset()


## Mark the preset the sliders match, or Custom when none does.
func _sync_preset() -> void:
	var matched := matching_preset()
	for id: StringName in _preset_buttons:
		_preset_buttons[id].set_pressed_no_signal(id == matched)
	_custom_label.visible = matched == &""
	_enter_rows()


## The preset whose six values the sliders hold (within half a step), or `&""` for Custom.
func matching_preset() -> StringName:
	for p: Dictionary in _presets:
		var all := true
		for s: Dictionary in SLIDERS:
			var key := String(s["key"])
			var slider := _sliders[key]
			if absf(Kit.get_float(p, key, -INF) - slider.value) > slider.step * 0.5:
				all = false
				break
		if all:
			return StringName(Kit.get_str(p, "preset"))
	return &""


func value_text(key: String, v: float) -> String:
	var format: StringName = &"times"
	for s: Dictionary in SLIDERS:
		if String(s["key"]) == key:
			format = s["format"]
	var step := _sliders[key].step if _sliders.has(key) else 0.05
	match format:
		&"seconds":
			return Kit.number(v, &"s", Kit.decimals_for(step))
		&"hulls":
			return tr(&"campaign_menu.value.hulls").format([Kit.number(v, &"none", 0)])
		&"days":
			return tr(&"campaign_menu.value.days").format([Kit.number(v, &"none", 0)])
		_:
			return tr(&"campaign_menu.value.times").format([Kit.number(v, &"none", Kit.decimals_for(step))])


## The DifficultyDict the sliders describe.
func difficulty() -> Dictionary:
	return {
		"preset": matching_preset(),
		"ai_accuracy": float(_sliders["ai_accuracy"].value),
		"ai_reaction_s": float(_sliders["ai_reaction_s"].value),
		"adaptation_cadence": float(_sliders["adaptation_cadence"].value),
		"hull_count": int(roundf(_sliders["hull_count"].value)),
		"economy": float(_sliders["economy"].value),
		"clock_days": int(roundf(_sliders["clock_days"].value)),
	}


# --- starting ------------------------------------------------------------------------------------

func _on_starter(path: String) -> void:
	_starter_path = path
	_enter_rows()


func _update_start() -> void:
	_start.disabled = _name.text.strip_edges().is_empty()
	if _start.disabled:
		_problems.text = tr(&"campaign_menu.new.name_needed")
	elif _problems.text == tr(&"campaign_menu.new.name_needed"):
		_problems.text = ""


## The options `Campaign.new_campaign` takes, from the form.
func options() -> Dictionary:
	var opts := {
		"slot_name": _name.text.strip_edges().left(NAME_MAX),
		"difficulty": difficulty(),
		"ironman": _ironman.button_pressed,
		"seed": seed_value(_seed.text),
		"tutorial_done": _tutorial_done,
	}
	if not _starter_path.is_empty():
		opts["starter"] = _starter_path
	return opts


## Blank is 0 (random); a whole number is used as it is; other text is hashed into a seed.
static func seed_value(text: String) -> int:
	var t := text.strip_edges()
	if t.is_empty():
		return 0
	if t.is_valid_int():
		return absi(t.to_int())
	return absi(hash(t)) % 2147483647 + 1


func _on_start() -> void:
	if _busy or _name.text.strip_edges().is_empty():
		return
	_busy = true
	_problems.text = ""
	Screens.show_loading(&"campaign_menu.new.loading", -1.0)
	await get_tree().process_frame
	var problems := Kit.problem_texts(Kit.call_on(&"Campaign", &"new_campaign", [options()]), &"campaign_menu.new.failed")
	_busy = false
	Screens.hide_loading()
	if not problems.is_empty() and is_inside_tree():
		_problems.text = Kit.problems_body(problems)
		Ui.focus(_start)


func _leave() -> void:
	Screens.change(&"campaign_slots" if _has_slots else &"main_menu")
