extends Control
## Skirmish setup against AI.
## DESIGN.md L959. Unit: ui-skirmish.
## Reads and calls: Session.start_skirmish, Content.mission_templates, Editor.browse, Editor.file_issues.
##
## "A single operation from the campaign templates, with a chosen tonnage cap and enemy size"
## (L959), offline against AI. A base screen (`Screens.change`), so a start that fails stays here
## with its problems above Start; a start that works moves to the sortie through `Screens` routing.
## Left column, in the order a player decides: the operation template (its description and the
## fittings a hull needs for it), the opposition (act and its tonnage cap, enemy hulls, difficulty
## preset), then the conditions. Right column: the design to field. Every part is available, with no
## unlock track (L963). Start is refused while the design does not fit the chosen act's cap.
## The last setup is kept for the rest of the run, so a rematch is one press. Esc and Back return to
## the main menu.

const Kit := preload("res://ui/screens/menu_kit.gd")
const ConditionsPanel := preload("res://ui/screens/conditions_panel.gd")
const DesignPicker := preload("res://ui/screens/design_picker.gd")

const ACT_COUNT := 4
const FALLBACK_CAPS: Array[float] = [150.0, 400.0, 900.0, 1800.0]
## Enemy hulls per player: at least 1 (Session); 5 is the campaign's one-against-many multiple.
const ENEMY_MIN := 1
const ENEMY_MAX := 20
const ENEMY_DEFAULT := 5

## The last setup started in this run: `template`, `act`, `enemy_per_player`, `preset`.
static var last_choice: Dictionary = {}

@onready var _back: Button = $Margin/Column/Header/Back
@onready var _left: VBoxContainer = $Margin/Column/Body/Flow/Left
@onready var _right: VBoxContainer = $Margin/Column/Body/Flow/Right
@onready var _problems: Label = $Margin/Column/Footer/Problems
@onready var _start: Button = $Margin/Column/Footer/Start

var _templates: Array[Dictionary] = []
var _presets: Array[Dictionary] = []
var _caps: Array[float] = []
var _template_list: ItemList
var _template_text: Label
var _needs_text: Label
var _act: OptionButton
var _enemies: HSlider
var _enemies_text: Label
var _difficulty: OptionButton
var _conditions: ConditionsPanel
var _picker: DesignPicker
var _busy := false

var _theme_started := false

func _ready() -> void:
	Ui.set_margins($Margin, 5)
	Ui.set_separation($Margin/Column, 3)
	($Margin/Column/Body/Flow as HFlowContainer).add_theme_constant_override(&"h_separation", Ui.spacing(6))
	($Margin/Column/Body/Flow as HFlowContainer).add_theme_constant_override(&"v_separation", Ui.spacing(5))
	Ui.set_separation(_left, 5)
	Ui.set_separation(_right, 3)
	Ui.set_separation($Margin/Column/Footer, 4)
	_problems.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_problems.text = ""
	_back.pressed.connect(_leave)
	_start.pressed.connect(_on_start)
	_build_static()
	accessibility_name = tr(&"skirmish.title")


func _exit_tree() -> void:
	Kit.leave_menu_theme(_theme_started)


func setup(_args: Dictionary) -> void:
	_theme_started = true
	Kit.start_menu_theme()
	var caps: Array[float] = []
	var raw_caps: Variant = Kit.call_on(&"Content", &"act_caps")
	if raw_caps is PackedFloat32Array:
		for c: float in raw_caps as PackedFloat32Array:
			caps.append(c)
	fill(Kit.dicts(Kit.call_on(&"Content", &"mission_templates")), Kit.dicts(Kit.call_on(&"Content", &"difficulty_presets")), caps)
	_picker.load_entries()
	_refresh_start()


## Start when the defaults already make a valid skirmish, so a keyboard or gamepad player can play
## at once and walk Up to change things; otherwise the operation list.
func default_focus() -> Control:
	return _start if _start != null and not _start.disabled else _template_list


func _unhandled_input(event: InputEvent) -> void:
	if event.is_action_pressed(&"ui_cancel", false, true) and Screens.top() == self:
		get_viewport().set_input_as_handled()
		_leave()


func _build_static() -> void:
	var op := Kit.vbox(2)
	op.add_child(Kit.label("skirmish.operation", &"PanelTitle"))
	_template_list = ItemList.new()
	# Six and a half rows: the cut row shows the list scrolls, and at 200% the heading above it still
	# fits on screen when focus scrolls the list into view.
	_template_list.custom_minimum_size = Vector2(0.0, 224.0)
	_template_list.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_template_list.select_mode = ItemList.SELECT_SINGLE
	_template_list.focus_mode = Control.FOCUS_ALL
	_template_list.accessibility_name = tr(&"skirmish.operation")
	_template_list.item_selected.connect(func(_i: int) -> void: _show_template())
	op.add_child(_template_list)
	_template_text = Kit.label("", &"Body", true)
	_needs_text = Kit.label("", &"Body", true)
	op.add_child(_template_text)
	op.add_child(_needs_text)
	_left.add_child(op)

	var opposition := Kit.vbox(2)
	opposition.add_child(Kit.label("skirmish.opposition", &"PanelTitle"))
	_act = OptionButton.new()
	_act.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_act.item_selected.connect(func(_i: int) -> void: _on_act())
	opposition.add_child(_row("skirmish.act", _act))
	_enemies = preload("res://ui/widgets/focus_slider.gd").new()
	_enemies.min_value = ENEMY_MIN
	_enemies.max_value = ENEMY_MAX
	_enemies.step = 1
	_enemies.value = ENEMY_DEFAULT
	_enemies.custom_minimum_size = Vector2(240.0, Kit.BUTTON_H)
	_enemies.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	_enemies_text = Kit.label("", &"Body", true)
	_enemies_text.custom_minimum_size = Vector2(160.0, 0.0)
	_enemies_text.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	_enemies.value_changed.connect(func(v: float) -> void: _enemies_text.text = enemy_text(int(v)))
	var enemy_row := _row("skirmish.enemies", _enemies)
	enemy_row.add_child(_enemies_text)
	opposition.add_child(enemy_row)
	_difficulty = OptionButton.new()
	_difficulty.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	opposition.add_child(_row("skirmish.difficulty", _difficulty))
	_left.add_child(opposition)

	_conditions = ConditionsPanel.new()
	_left.add_child(_conditions)

	_picker = DesignPicker.new()
	_picker.selected.connect(func(_p: String) -> void: _refresh_start())
	_right.add_child(_picker)
	_enemies_text.text = enemy_text(ENEMY_DEFAULT)
	_link_focus()


func _row(label_key: String, control: Control) -> HBoxContainer:
	var row := Kit.hbox(3)
	var l := Kit.label(label_key, &"Body")
	l.custom_minimum_size = Vector2(220.0, 0.0)
	l.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	row.add_child(l)
	if control is OptionButton:
		control.custom_minimum_size = Vector2(320.0, Kit.BUTTON_H)
	control.accessibility_name = tr(StringName(label_key))
	row.add_child(control)
	return row


## Fill the choices from plain data (also the test entry point). Empty lists fall back to what the
## spec fixes (four acts and their caps); an empty template list leaves Start refused.
func fill(templates: Array[Dictionary], presets: Array[Dictionary], caps: Array[float]) -> void:
	_templates = templates
	_presets = Kit.difficulty_order(presets)
	_caps = caps if caps.size() >= ACT_COUNT else FALLBACK_CAPS.duplicate()
	_template_list.clear()
	for t: Dictionary in _templates:
		var i := _template_list.add_item(tr(StringName(Kit.get_str(t, "name"))))
		_template_list.set_item_tooltip_enabled(i, false)
	if _templates.is_empty():
		_template_text.text = tr(&"skirmish.templates.empty")
	_act.clear()
	for a in ACT_COUNT:
		_act.add_item(tr(&"skirmish.act.row").format([tr(StringName("units.word.act.%d" % (a + 1))), Kit.number(_caps[a], &"t", 0)]), a)
	_difficulty.clear()
	if _presets.is_empty():
		_difficulty.add_item(tr(&"difficulty.normal"), 0)
	for i in _presets.size():
		_difficulty.add_item(tr(StringName(Kit.get_str(_presets[i], "name", "difficulty." + Kit.get_str(_presets[i], "preset")))), i)
	_restore_choice()
	_show_template()
	_on_act()


func _restore_choice() -> void:
	var template := Kit.get_int(last_choice, "template", -1)
	var row := 0
	for i in _templates.size():
		if Kit.get_int(_templates[i], "template", -2) == template:
			row = i
	if not _templates.is_empty():
		_template_list.select(row)
	_act.select(clampi(Kit.get_int(last_choice, "act", 0), 0, ACT_COUNT - 1))
	_enemies.value = clampi(Kit.get_int(last_choice, "enemy_per_player", ENEMY_DEFAULT), ENEMY_MIN, ENEMY_MAX)
	var preset := Kit.get_str(last_choice, "preset", "normal")
	_difficulty.select(0)
	for i in _presets.size():
		if Kit.get_str(_presets[i], "preset") == preset:
			_difficulty.select(i)


func _selected_template() -> Dictionary:
	var rows := _template_list.get_selected_items()
	if rows.is_empty() or rows[0] >= _templates.size():
		return {}
	return _templates[rows[0]]


func _show_template() -> void:
	var t := _selected_template()
	if t.is_empty():
		_needs_text.text = ""
		_needs_text.visible = false
		_refresh_start()
		return
	_template_text.text = tr(StringName(Kit.get_str(t, "description")))
	_needs_text.text = needs_text(t.get("needs", PackedStringArray()))
	_needs_text.visible = not _needs_text.text.is_empty()
	_refresh_start()


## "Your ship needs: boarding kit, RHIB davit." for a template's part tags, or "" for none.
static func needs_text(needs: Variant) -> String:
	var words := PackedStringArray()
	var tags: Array = Array(needs) if needs is PackedStringArray else Kit.as_array(needs)
	for tag: Variant in tags:
		var key := StringName("notes.tag_word." + String(tag))
		var word := Kit.tr_key(key)
		words.append(word if word != String(key) else String(tag).replace("_", " "))
	if words.is_empty():
		return ""
	return Kit.tr_key(&"skirmish.needs").format([", ".join(words)])


static func enemy_text(n: int) -> String:
	return Kit.tr_key(&"skirmish.enemies.one" if n == 1 else &"skirmish.enemies.many").format([n])


func _on_act() -> void:
	var a := clampi(_act.get_selected_id(), 0, ACT_COUNT - 1)
	_picker.set_cap(_caps[a] if a < _caps.size() else 0.0)
	_refresh_start()


func _refresh_start() -> void:
	if _picker == null:
		return
	var reason := ""
	if _selected_template().is_empty():
		reason = tr(&"skirmish.need_template")
	elif _picker.selected_path().is_empty():
		reason = tr(&"skirmish.need_design")
	elif _picker.blocked():
		reason = tr(&"skirmish.design_blocked")
	_start.disabled = not reason.is_empty() or _busy
	if not _busy:
		_problems.text = reason


func _link_focus() -> void:
	var left: Array = [_template_list, _act, _enemies, _difficulty]
	left.append_array(_conditions.focus_controls())
	left.append(_start)
	var live := Kit.chain_vertical(left)
	var right := Kit.chain_vertical(_picker.focus_controls())
	var right_first: Control = right[0] if not right.is_empty() else null
	for c: Control in live:
		c.focus_neighbor_left = c.get_path_to(c)
		if right_first != null and not c is HSlider:
			c.focus_neighbor_right = c.get_path_to(right_first)
	for c: Control in right:
		c.focus_neighbor_left = c.get_path_to(_template_list)
		c.focus_neighbor_right = c.get_path_to(c)
	if not right.is_empty():
		right[-1].focus_neighbor_bottom = right[-1].get_path_to(_start)
	_template_list.focus_neighbor_top = _template_list.get_path_to(_back)
	if right_first != null:
		right_first.focus_neighbor_top = right_first.get_path_to(_back)
	_back.focus_neighbor_bottom = _back.get_path_to(_template_list)
	_back.focus_neighbor_top = _back.get_path_to(_back)


## The `Session.start_skirmish` setup the form describes.
func setup_dict() -> Dictionary:
	var t := _selected_template()
	var preset: Dictionary = _presets[_difficulty.get_selected_id()] if _difficulty.get_selected_id() >= 0 and _difficulty.get_selected_id() < _presets.size() else {}
	var difficulty := {}
	for k: String in ["preset", "ai_accuracy", "ai_reaction_s", "adaptation_cadence", "hull_count", "economy", "clock_days"]:
		if preset.has(k):
			difficulty[k] = preset[k]
	var out := {
		"template": Kit.get_int(t, "template", 0),
		"act": clampi(_act.get_selected_id(), 0, ACT_COUNT - 1),
		"enemy_per_player": int(_enemies.value),
		"difficulty": difficulty,
		"blueprint": _picker.selected_path(),
	}
	out.merge(_conditions.values())
	return out


func _on_start() -> void:
	if _busy or _start.disabled:
		return
	var setup := setup_dict()
	_busy = true
	_start.disabled = true
	_problems.text = ""
	var problems := Kit.problem_texts(Kit.call_on(&"Session", &"start_skirmish", [setup]), &"skirmish.failed")
	_busy = false
	if problems.is_empty():
		last_choice = {"template": setup["template"], "act": setup["act"], "enemy_per_player": setup["enemy_per_player"],
			"preset": String(Kit.as_dict(setup["difficulty"]).get("preset", "normal"))}
		DesignPicker.last_path = String(setup["blueprint"])
		return
	if is_inside_tree():
		_refresh_start()
		_problems.text = Kit.problems_body(problems)


func _leave() -> void:
	Screens.change(&"main_menu")
