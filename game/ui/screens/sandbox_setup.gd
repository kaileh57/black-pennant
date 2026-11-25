extends Control
## Sandbox setup: free play with chosen targets.
## DESIGN.md L961. Unit: ui-skirmish.
## Reads and calls: Session.start_sandbox, Editor.browse.
##
## "Free play with your own designs" (L959-961): no end condition, every part available, no
## tonnage cap beyond the 90 m player-hull limit. A base screen (`Screens.change`), so a start that
## fails stays here with its problems above Start.
## Left column: the target ships, then the conditions. Each target row picks a ship (the enemy
## classes first, then stock starters and the player's designs), whether it is Hostile, and how many,
## and can be removed; up to MAX_ROWS rows of up to MAX_COUNT ships. It starts with two hostile ships
## of the first enemy class, so Start works at once. With no ship to pick the sandbox still opens,
## with the player's ship alone, and says so. Right column: the design to field.
## The last targets are kept for the rest of the run. Esc and Back return to the main menu.

const Kit := preload("res://ui/screens/menu_kit.gd")
const ConditionsPanel := preload("res://ui/screens/conditions_panel.gd")
const DesignPicker := preload("res://ui/screens/design_picker.gd")

const NONE := -1
const MAX_ROWS := 8
const MAX_COUNT := 10
const DEFAULT_COUNT := 2

## The targets of the last sandbox started in this run: `[{ "blueprint", "hostile", "count" }]`.
static var last_targets: Array[Dictionary] = []

@onready var _back: Button = $Margin/Column/Header/Back
@onready var _left: VBoxContainer = $Margin/Column/Body/Flow/Left
@onready var _right: VBoxContainer = $Margin/Column/Body/Flow/Right
@onready var _problems: Label = $Margin/Column/Footer/Problems
@onready var _start: Button = $Margin/Column/Footer/Start

## Ships a target row can pick: `{ "path": String, "text": String }`.
var _choices: Array[Dictionary] = []
var _rows_box: VBoxContainer
var _empty_label: Label
var _add: Button
## One per target row: `{ "box", "ship": OptionButton, "hostile": CheckButton, "count": HSlider, "count_text": Label, "remove": Button }`.
var _rows: Array[Dictionary] = []
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
	var targets := Kit.vbox(2)
	targets.add_child(Kit.label("sandbox.targets", &"PanelTitle"))
	targets.add_child(Kit.label("sandbox.targets.help", &"Body"))
	_rows_box = Kit.vbox(2)
	targets.add_child(_rows_box)
	_empty_label = Kit.label("sandbox.targets.empty", &"Body")
	_empty_label.visible = false
	targets.add_child(_empty_label)
	_add = Kit.inline_button("sandbox.targets.add")
	_add.pressed.connect(_on_add)
	targets.add_child(_add)
	_left.add_child(targets)
	_conditions = ConditionsPanel.new()
	_left.add_child(_conditions)
	_picker = DesignPicker.new()
	_picker.selected.connect(func(_p: String) -> void: _refresh_start())
	_right.add_child(_picker)
	accessibility_name = tr(&"sandbox.title")
	_link_focus()


func _exit_tree() -> void:
	Kit.leave_menu_theme(_theme_started)


func setup(_args: Dictionary) -> void:
	_theme_started = true
	Kit.start_menu_theme()
	_picker.load_entries()
	set_choices(Kit.dicts(Kit.call_on(&"Content", &"classes", [NONE])), _picker.entries())


func default_focus() -> Control:
	if not _rows.is_empty():
		return _rows[0]["ship"]
	return _start if not _start.disabled else _picker.list


func _unhandled_input(event: InputEvent) -> void:
	if event.is_action_pressed(&"ui_cancel", false, true) and Screens.top() == self:
		get_viewport().set_input_as_handled()
		_leave()


## The ships targets can be, from enemy ClassDicts and the picker's design rows (also the test
## entry point). Restores the last targets, or one row of two hostile ships of the first class.
func set_choices(classes: Array[Dictionary], designs: Array[Dictionary]) -> void:
	_choices = []
	var seen: Dictionary[String, bool] = {}
	for c: Dictionary in classes:
		var path := Kit.get_str(c, "blueprint")
		if path.is_empty() or seen.has(path):
			continue
		seen[path] = true
		_choices.append({"path": path, "text": tr(&"sandbox.target.class").format([tr(StringName(Kit.get_str(c, "name"))),
			Kit.number(Kit.get_float(c, "length_m"), &"m", 0), Kit.number(Kit.get_float(c, "tonnage_t"), &"t", 0)])})
	for d: Dictionary in designs:
		var path := Kit.get_str(d, "path")
		if path.is_empty() or seen.has(path):
			continue
		seen[path] = true
		_choices.append({"path": path, "text": DesignPicker.row_text(Kit.as_dict(d.get("info")))})
	for row: Dictionary in _rows.duplicate():
		_remove_row(row, false)
	var restored := 0
	for t: Dictionary in last_targets:
		if seen.has(Kit.get_str(t, "blueprint")) and restored < MAX_ROWS:
			_add_row(Kit.get_str(t, "blueprint"), Kit.get_bool(t, "hostile", true), Kit.get_int(t, "count", DEFAULT_COUNT))
			restored += 1
	if restored == 0 and not _choices.is_empty():
		_add_row(Kit.get_str(_choices[0], "path"), true, DEFAULT_COUNT)
	_after_rows_changed()


func _add_row(path: String, hostile: bool, count: int) -> Dictionary:
	var box := Kit.hbox(2)
	var ship := OptionButton.new()
	ship.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	ship.custom_minimum_size = Vector2(200.0, Kit.BUTTON_H)
	ship.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	ship.clip_text = true
	# The longest class name would otherwise set the row's minimum width and push the screen past
	# the window at 200%; the open list still shows every name in full.
	ship.fit_to_longest_item = false
	ship.accessibility_name = tr(&"sandbox.target.ship")
	for i in _choices.size():
		ship.add_item(String(_choices[i]["text"]), i)
		if String(_choices[i]["path"]) == path:
			ship.select(i)
	if ship.selected < 0 and ship.item_count > 0:
		ship.select(0)
	var hostile_check := CheckButton.new()
	hostile_check.text = "sandbox.target.hostile"
	hostile_check.button_pressed = hostile
	hostile_check.custom_minimum_size = Vector2(0.0, Kit.BUTTON_H)
	var slider := preload("res://ui/widgets/focus_slider.gd").new()
	slider.min_value = 1
	slider.max_value = MAX_COUNT
	slider.step = 1
	slider.value = clampi(count, 1, MAX_COUNT)
	slider.custom_minimum_size = Vector2(112.0, Kit.BUTTON_H)
	slider.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	slider.accessibility_name = tr(&"sandbox.target.count")
	var count_text := Kit.label(count_label(int(slider.value)), &"Body", true)
	count_text.custom_minimum_size = Vector2(90.0, 0.0)
	count_text.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	count_text.autowrap_mode = TextServer.AUTOWRAP_OFF
	slider.value_changed.connect(func(v: float) -> void: count_text.text = count_label(int(v)))
	var remove := Kit.icon_button(&"remove", tr(&"sandbox.target.remove"))
	box.add_child(ship)
	box.add_child(hostile_check)
	box.add_child(slider)
	box.add_child(count_text)
	box.add_child(remove)
	_rows_box.add_child(box)
	var row := {"box": box, "ship": ship, "hostile": hostile_check, "count": slider, "count_text": count_text, "remove": remove}
	remove.pressed.connect(func() -> void: _remove_row(row, true))
	_rows.append(row)
	return row


static func count_label(n: int) -> String:
	return Kit.tr_key(&"sandbox.target.count_one" if n == 1 else &"sandbox.target.count_many").format([n])


func _on_add() -> void:
	if _rows.size() >= MAX_ROWS or _choices.is_empty():
		return
	var path := Kit.get_str(_choices[0], "path")
	var row := _add_row(path, true, DEFAULT_COUNT)
	_after_rows_changed()
	Ui.focus(row["ship"])


func _remove_row(row: Dictionary, refocus: bool) -> void:
	var at := _rows.find(row)
	if at < 0:
		return
	_rows.remove_at(at)
	var box: Node = row["box"]
	_rows_box.remove_child(box)
	box.queue_free()
	if refocus:
		_after_rows_changed()
		if not _rows.is_empty():
			Ui.focus(_rows[mini(at, _rows.size() - 1)]["ship"])
		else:
			Ui.focus(_add)


func _after_rows_changed() -> void:
	_empty_label.visible = _choices.is_empty()
	_add.disabled = _choices.is_empty() or _rows.size() >= MAX_ROWS
	_link_focus()
	_refresh_start()


func _refresh_start() -> void:
	var reason := ""
	if _picker.selected_path().is_empty():
		reason = tr(&"skirmish.need_design")
	elif _picker.blocked():
		reason = tr(&"skirmish.design_blocked")
	_start.disabled = not reason.is_empty() or _busy
	if not _busy:
		_problems.text = reason


func _link_focus() -> void:
	var left: Array = []
	for row: Dictionary in _rows:
		var across: Array = [row["ship"], row["hostile"], row["count"], row["remove"]]
		Kit.chain_horizontal(across)
		left.append(row["ship"])
	left.append(_add)
	left.append_array(_conditions.focus_controls())
	left.append(_start)
	var live := Kit.chain_vertical(left)
	# The other controls of a target row go Up and Down with the row's ship picker.
	for row: Dictionary in _rows:
		var ship := row["ship"] as Control
		for key: String in ["hostile", "count", "remove"]:
			var c := row[key] as Control
			c.focus_neighbor_top = c.get_path_to(ship.get_node(ship.focus_neighbor_top))
			c.focus_neighbor_bottom = c.get_path_to(ship.get_node(ship.focus_neighbor_bottom))
	var right := Kit.chain_vertical(_picker.focus_controls())
	var right_first: Control = right[0] if not right.is_empty() else null
	for c: Control in live:
		if c is HSlider:
			continue
		c.focus_neighbor_left = c.get_path_to(c)
		# A ship picker's Right goes along its row; every other control's Right reaches the design list.
		if right_first != null and not _is_row_control(c):
			c.focus_neighbor_right = c.get_path_to(right_first)
	for row: Dictionary in _rows:
		var last := row["remove"] as Control
		if right_first != null:
			last.focus_neighbor_right = last.get_path_to(right_first)
	var first_left: Control = live[0] if not live.is_empty() else _start
	for c: Control in right:
		c.focus_neighbor_left = c.get_path_to(first_left)
		c.focus_neighbor_right = c.get_path_to(c)
	if not right.is_empty():
		right[-1].focus_neighbor_bottom = right[-1].get_path_to(_start)
		right[0].focus_neighbor_top = right[0].get_path_to(_back)
	if not live.is_empty():
		live[0].focus_neighbor_top = live[0].get_path_to(_back)
	_back.focus_neighbor_bottom = _back.get_path_to(first_left)
	_back.focus_neighbor_top = _back.get_path_to(_back)


func _is_row_control(c: Control) -> bool:
	for row: Dictionary in _rows:
		if c == row["ship"]:
			return true
	return false


## The targets the rows describe.
func targets() -> Array[Dictionary]:
	var out: Array[Dictionary] = []
	for row: Dictionary in _rows:
		var ship := row["ship"] as OptionButton
		var i := ship.get_selected_id()
		if i < 0 or i >= _choices.size():
			continue
		out.append({"blueprint": String(_choices[i]["path"]), "hostile": (row["hostile"] as CheckButton).button_pressed,
			"count": int((row["count"] as HSlider).value)})
	return out


## The `Session.start_sandbox` setup the form describes.
func setup_dict() -> Dictionary:
	var out := {"blueprint": _picker.selected_path(), "targets": targets()}
	out.merge(_conditions.values())
	return out


func _on_start() -> void:
	if _busy or _start.disabled:
		return
	var setup := setup_dict()
	_busy = true
	_start.disabled = true
	_problems.text = ""
	var problems := Kit.problem_texts(Kit.call_on(&"Session", &"start_sandbox", [setup]), &"sandbox.failed")
	_busy = false
	if problems.is_empty():
		last_targets = targets()
		DesignPicker.last_path = String(setup["blueprint"])
		return
	if is_inside_tree():
		_refresh_start()
		_problems.text = Kit.problems_body(problems)


func _leave() -> void:
	Screens.change(&"main_menu")
