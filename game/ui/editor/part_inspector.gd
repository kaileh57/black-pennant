extends VBoxContainer
## Selected part or kit: label, load, kit settings, kit update.
## DESIGN.md L140. Unit: ui-editor-tray.
## Reads and calls: Editor.selection, Editor.parts, Editor.kits, Editor.set_label, Editor.set_load, Editor.set_kit_setting, Editor.migrate_kit.
##
## Under the tray, for what is selected in the view:
## - One part: its name field (empty restores the generated name, "Gun 2"), its fill when it holds fuel,
##   ammunition or ballast, its cell and turn as numbers (the non-drag way to move it, L233), and Why?,
##   Show arcs, Add data link and Remove. A part of a kit also shows the kit: version, its settings sheet
##   (L140) with a revert to each default, Select kit and Remove kit.
## - Several parts: the count, mass and cost, with Copy, Delete and Capture as kit.
## - Kit updates (L140: updating a kit offers to migrate every placed copy): a line per kit with a newer
##   version, offered whatever is selected, until migrated or put off.
## Values update in place when the design changes; the field being typed in is never overwritten.

signal why_requested(slot: int)
signal route_requested(slot: int)
signal focus_arcs(slots: PackedInt32Array)

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const MAX_BODY_H := 460.0
const LOAD_STEP := 0.05

@onready var _updates: VBoxContainer = $Updates
@onready var _rule: HSeparator = $Rule
@onready var _scroll: ScrollContainer = $Scroll
@onready var _body: VBoxContainer = $Scroll/Body

## What the body shows: "", "part:<slot>" or "many:<slots>".
var _shown := ""
var _slot := -1
var _kit_slot := -1
var _put_off: Dictionary[String, int] = {}
var _refresh_pending := false
# Controls of the single-part view, updated in place.
var _title: Label
var _kind_line: Label
var _label_field: LineEdit
var _load_slider: HSlider
var _load_value: Label
var _cell_fields: Array[SpinBox] = []
var _kit_line: Label
var _kit_update: Button
var _settings: Dictionary[String, Array] = {}
var _many_line: Label
var _parts_cache: Array[Dictionary] = []
var _parts_rev := -1
var _stale := false
static var _categories: Dictionary[StringName, int] = {}


func _ready() -> void:
	Ui.set_separation(self, 1)
	Ui.set_separation(_updates, 1)
	Ui.set_separation(_body, 1)
	accessibility_name = tr(&"editor_tray.inspector.name")
	var editor := Kit.autoload(&"Editor")
	if editor != null:
		editor.connect(&"selection_changed", _queue_refresh)
		editor.connect(&"design_changed", func(_rev: int) -> void: _queue_refresh())
		editor.connect(&"kit_update_offered", _on_kit_update_offered)
	visibility_changed.connect(func() -> void:
		if _stale and is_visible_in_tree():
			_stale = false
			_queue_refresh())
	_refresh()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_shown = ""
		_refresh()


func _editor() -> Node:
	return Kit.autoload(&"Editor")


func _queue_refresh() -> void:
	if _refresh_pending:
		return
	if not is_visible_in_tree():
		_stale = true
		return
	_refresh_pending = true
	_refresh.call_deferred()


func _refresh() -> void:
	_refresh_pending = false
	if not is_node_ready():
		return
	_refresh_updates()
	var e := _editor()
	if e == null or not bool(e.call(&"is_open")):
		_show_nothing()
		return
	var sel := Kit.as_dict(e.call(&"selection"))
	var parts: PackedInt32Array = sel.get("parts", PackedInt32Array()) if sel.get("parts") is PackedInt32Array else PackedInt32Array()
	if parts.is_empty():
		_show_nothing()
	elif parts.size() == 1:
		var p := _placed(parts[0])
		if p.is_empty():
			_show_nothing()
			return
		var key := "part:%d" % parts[0]
		if key != _shown:
			_build_part(p)
			_shown = key
		_update_part(p)
	else:
		var key := "many:" + ",".join(Array(parts).map(func(s: int) -> String: return str(s)))
		if key != _shown:
			_build_many(parts)
			_shown = key
		_update_many(sel, parts.size())
	_fit_scroll()
	apply_tutorial()


func _show_nothing() -> void:
	_shown = ""
	_slot = -1
	_kit_slot = -1
	_clear(_body)
	_scroll.visible = false
	_rule.visible = _updates.visible


func _fit_scroll() -> void:
	_scroll.visible = _body.get_child_count() > 0
	_rule.visible = _scroll.visible or _updates.visible
	var cap := MAX_BODY_H
	var parent := get_parent() as Control
	if parent != null and parent.size.y > 0.0:
		cap = minf(cap, parent.size.y * 0.6)
	_scroll.custom_minimum_size.y = minf(_body.get_combined_minimum_size().y, cap)


static func _clear(box: Node) -> void:
	for c: Node in box.get_children():
		box.remove_child(c)
		c.queue_free()


func _placed(slot: int) -> Dictionary:
	for p: Dictionary in _all_parts():
		if int(p.get("slot", -1)) == slot:
			return p
	return {}


## The design's placed parts, read once per revision (a 90 m hull's list costs milliseconds).
func _all_parts() -> Array[Dictionary]:
	var e := _editor()
	if e == null:
		return []
	var rev := int(e.call(&"rev"))
	if rev != _parts_rev:
		_parts_rev = rev
		_parts_cache = Kit.dicts(e.call(&"parts"))
	return _parts_cache


func _placed_kit(kit_slot: int) -> Dictionary:
	if kit_slot < 0:
		return {}
	for k: Dictionary in Kit.dicts(_editor().call(&"kits")):
		if int(k.get("slot", -1)) == kit_slot:
			return k
	return {}


## The part's label, or its generated name ("Gun 2": its category and its order among the design's parts of
## that category, the rule the text unit uses for saved designs), read from the design under edit. `parts`
## is `Editor.parts()` when the caller has it.
static func part_name(slot: int, parts: Array[Dictionary] = []) -> String:
	if parts.is_empty():
		parts = Kit.dicts(Kit.call_on(&"Editor", &"parts"))
	for p: Dictionary in parts:
		if int(p.get("slot", -1)) == slot:
			var label := Kit.get_str(p, "label").strip_edges()
			return label if not label.is_empty() else generated_name(slot, parts)
	return _Text.resolve({"key": &"editor_shell.part.name", "args": [{"part": slot}]})


## The name a part gets with no label of its own.
static func generated_name(slot: int, parts: Array[Dictionary]) -> String:
	var mine := -1
	for p: Dictionary in parts:
		if int(p.get("slot", -1)) == slot:
			mine = _category_of(StringName(p.get("part", &"")))
	if mine < 0:
		return _Text.resolve({"key": &"editor_shell.part.name", "args": [{"part": slot}]})
	var order := 0
	for p: Dictionary in parts:
		if _category_of(StringName(p.get("part", &""))) == mine:
			order += 1
		if int(p.get("slot", -1)) == slot:
			break
	var tx := Kit.autoload(&"Tx")
	var word := String(tx.call(&"word", &"category", mine)) if tx != null else ""
	return TranslationServer.translate(&"units.part.generated").format([word, Kit.number(float(order), &"none", 0)])


## A part's category; the catalogue does not change while the game runs, so it is read once per part id.
static func _category_of(id: StringName) -> int:
	if not _categories.has(id):
		_categories[id] = Kit.get_int(Kit.as_dict(Kit.call_on(&"Content", &"part", [id])), "category", -1)
	return _categories[id]


static func _kit_name(kit: StringName) -> String:
	var def := Kit.as_dict(Kit.call_on(&"Content", &"kit", [kit]))
	var n := StringName(def.get("name", &""))
	return String(TranslationServer.translate(n)) if n != &"" else String(kit)


# --- one part -------------------------------------------------------------------------------------

func _build_part(p: Dictionary) -> void:
	_clear(_body)
	_settings.clear()
	_cell_fields.clear()
	_slot = int(p.get("slot", -1))
	_kit_slot = int(p.get("kit", -1))
	_title = Kit.label("", &"PanelTitle", true)
	_body.add_child(_title)
	_kind_line = Kit.label("", &"Dense", true)
	_body.add_child(_kind_line)
	# Name
	_body.add_child(Kit.label("editor_tray.inspector.label", &"Dense"))
	_label_field = LineEdit.new()
	_label_field.custom_minimum_size = Vector2(0.0, 40.0)
	_label_field.max_length = 32
	_label_field.accessibility_name = tr(&"editor_tray.inspector.label")
	_label_field.text_submitted.connect(func(_t: String) -> void: _commit_label())
	_label_field.focus_exited.connect(_commit_label)
	_label_field.set_meta(&"control_id", "editor.inspector.label")
	_body.add_child(_label_field)
	# Load
	if float(p.get("load", -1.0)) >= 0.0:
		var row := Kit.hbox(1)
		var load_label := Kit.label("editor_tray.inspector.load", &"Dense")
		load_label.autowrap_mode = TextServer.AUTOWRAP_OFF
		row.add_child(load_label)
		_load_slider = preload("res://ui/widgets/focus_slider.gd").new()
		_load_slider.min_value = 0.0
		_load_slider.max_value = 1.0
		_load_slider.step = LOAD_STEP
		_load_slider.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		_load_slider.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		_load_slider.custom_minimum_size = Vector2(0.0, 32.0)
		_load_slider.focus_mode = Control.FOCUS_ALL
		_load_slider.accessibility_name = tr(&"editor_tray.inspector.load")
		_load_slider.tooltip_text = tr(&"editor_tray.inspector.load_tip")
		_load_slider.drag_ended.connect(func(changed: bool) -> void:
			if changed:
				_commit_load())
		_load_slider.value_changed.connect(_on_load_moved)
		_load_slider.set_meta(&"control_id", "editor.inspector.load")
		row.add_child(_load_slider)
		_load_value = Kit.label("", &"Dense", true)
		_load_value.custom_minimum_size = Vector2(64.0, 0.0)
		_load_value.horizontal_alignment = HORIZONTAL_ALIGNMENT_RIGHT
		row.add_child(_load_value)
		_body.add_child(row)
	else:
		_load_slider = null
		_load_value = null
	# Cell and turn: the non-drag way to move a part.
	_body.add_child(Kit.label("editor_tray.inspector.position", &"Dense"))
	var cells := Kit.hbox(1)
	var axes: Array[StringName] = [&"editor_tray.inspector.cell_x", &"editor_tray.inspector.cell_y", &"editor_tray.inspector.cell_z"]
	for i in 3:
		var f := SpinBox.new()
		f.min_value = 0
		f.max_value = 1024
		f.step = 1
		f.rounded = true
		f.prefix = tr(axes[i])
		f.custom_minimum_size = Vector2(0.0, 40.0)
		f.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		f.get_line_edit().accessibility_name = tr(axes[i])
		f.value_changed.connect(_on_cell_field.bind(i))
		f.set_meta(&"control_id", "editor.inspector.position")
		cells.add_child(f)
		_cell_fields.append(f)
	_body.add_child(cells)
	var turns := Kit.hbox(1)
	var left := Kit.inline_button("editor_tray.inspector.turn_left")
	left.pressed.connect(_turn.bind(-1, 0))
	var right := Kit.inline_button("editor_tray.inspector.turn_right")
	right.pressed.connect(_turn.bind(1, 0))
	var tilt := Kit.inline_button("editor_tray.inspector.tilt")
	tilt.pressed.connect(_turn.bind(0, 1))
	for b: Button in [left, right, tilt]:
		b.custom_minimum_size.y = 40.0
		b.set_meta(&"control_id", "editor.inspector.position")
		turns.add_child(b)
	_body.add_child(turns)
	# Actions
	var actions := HFlowContainer.new()
	actions.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	actions.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	var why := Kit.inline_button("editor_tray.inspector.why")
	why.pressed.connect(func() -> void: why_requested.emit(_slot))
	var arcs := Kit.inline_button("editor_tray.inspector.arcs")
	arcs.tooltip_text = tr(&"editor_tray.inspector.arcs_tip")
	arcs.pressed.connect(func() -> void: focus_arcs.emit(PackedInt32Array([_slot])))
	var link := Kit.inline_button("editor_tray.inspector.link")
	link.tooltip_text = tr(&"editor_tray.inspector.link_tip")
	link.pressed.connect(func() -> void: route_requested.emit(_slot))
	var remove := Kit.inline_button("editor_tray.inspector.remove")
	remove.pressed.connect(func() -> void: _editor().call(&"remove_part", _slot))
	for b: Button in [why, arcs, link, remove]:
		b.custom_minimum_size.y = 40.0
		b.set_meta(&"control_id", "editor.inspector.actions")
		actions.add_child(b)
	_body.add_child(actions)
	if _kit_slot >= 0:
		_build_kit(p)


func _build_kit(_p: Dictionary) -> void:
	var k := _placed_kit(_kit_slot)
	if k.is_empty():
		return
	_body.add_child(HSeparator.new())
	_kit_line = Kit.label("", &"Body", true)
	_kit_line.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	_body.add_child(_kit_line)
	_kit_update = Kit.inline_button("")
	_kit_update.custom_minimum_size.y = 40.0
	_kit_update.pressed.connect(_migrate.bind(StringName(k.get("kit", &""))))
	_kit_update.set_meta(&"control_id", "editor.inspector.kit")
	_body.add_child(_kit_update)
	var def := Kit.as_dict(Kit.call_on(&"Content", &"kit", [StringName(k.get("kit", &""))]))
	var sheet: Variant = def.get("settings", [])
	if sheet is Array and not (sheet as Array).is_empty():
		_body.add_child(Kit.label("editor_tray.inspector.settings", &"Dense"))
		for s: Variant in sheet as Array:
			if s is Dictionary:
				_add_setting(s as Dictionary)
	var row := Kit.hbox(1)
	var select := Kit.inline_button("editor_tray.inspector.select_kit")
	select.pressed.connect(func() -> void: _editor().call(&"select_kits", PackedInt32Array([_kit_slot]), false))
	var remove := Kit.inline_button("editor_tray.inspector.remove_kit")
	remove.pressed.connect(func() -> void: _editor().call(&"remove_kit", _kit_slot))
	for b: Button in [select, remove]:
		b.custom_minimum_size.y = 40.0
		b.set_meta(&"control_id", "editor.inspector.kit")
		row.add_child(b)
	_body.add_child(row)


## One line of the kit's settings sheet: its label, a slider and a field, and a revert to the default.
func _add_setting(s: Dictionary) -> void:
	var key := Kit.get_str(s, "key")
	var lo := Kit.get_float(s, "min")
	var hi := Kit.get_float(s, "max", 1.0)
	if key.is_empty() or hi <= lo:
		return
	var step := _setting_step(lo, hi)
	_body.add_child(Kit.label(String(s.get("label", &"")), &"Dense"))
	var row := Kit.hbox(1)
	var slider := preload("res://ui/widgets/focus_slider.gd").new()
	slider.min_value = lo
	slider.max_value = hi
	slider.step = step
	slider.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	slider.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	slider.custom_minimum_size = Vector2(0.0, 32.0)
	slider.accessibility_name = tr(StringName(s.get("label", &"")))
	slider.set_meta(&"control_id", "editor.inspector.kit")
	var field := SpinBox.new()
	field.min_value = lo
	field.max_value = hi
	field.step = step
	field.custom_minimum_size = Vector2(120.0, 40.0)
	field.get_line_edit().accessibility_name = tr(StringName(s.get("label", &"")))
	field.set_meta(&"control_id", "editor.inspector.kit")
	var revert := Kit.icon_button(&"arrow_revert", tr(&"editor_tray.inspector.revert").format([Kit.number(Kit.get_float(s, "default"), &"none", Kit.decimals_for(step))]))
	revert.custom_minimum_size = Vector2(40.0, 40.0)
	revert.set_meta(&"control_id", "editor.inspector.kit")
	slider.drag_ended.connect(func(changed: bool) -> void:
		if changed:
			_commit_setting(key, slider.value))
	slider.value_changed.connect(func(v: float) -> void:
		field.set_value_no_signal(v)
		if not slider.has_focus() or Input.is_mouse_button_pressed(MOUSE_BUTTON_LEFT):
			return
		_commit_setting(key, v))
	field.value_changed.connect(func(v: float) -> void:
		slider.set_value_no_signal(v)
		_commit_setting(key, v))
	revert.pressed.connect(func() -> void: _commit_setting(key, Kit.get_float(s, "default")))
	row.add_child(slider)
	row.add_child(field)
	row.add_child(revert)
	_body.add_child(row)
	_settings[key] = [slider, field, revert, Kit.get_float(s, "default")]


static func _setting_step(lo: float, hi: float) -> float:
	var span := hi - lo
	if span <= 1.0:
		return 0.01
	if span <= 10.0:
		return 0.1
	if span <= 200.0:
		return 1.0
	return 5.0


func _update_part(p: Dictionary) -> void:
	_title.text = part_name(_slot, _all_parts())
	var def := Kit.as_dict(Kit.call_on(&"Content", &"part", [StringName(p.get("part", &""))]))
	var def_name := tr(StringName(def.get("name", p.get("part", &""))))
	_kind_line.text = def_name
	if not _label_field.has_focus():
		_label_field.text = Kit.get_str(p, "label")
	_label_field.placeholder_text = generated_name(_slot, _all_parts())
	if _load_slider != null:
		var load := clampf(float(p.get("load", 0.0)), 0.0, 1.0)
		if not _load_slider.has_focus():
			_load_slider.set_value_no_signal(load)
		_load_value.text = Kit.percent(_load_slider.value)
	var cell: Variant = p.get("cell", Vector3i.ZERO)
	if cell is Vector3i:
		var c := cell as Vector3i
		var v := [c.x, c.y, c.z]
		for i in 3:
			if not _cell_fields[i].get_line_edit().has_focus():
				_cell_fields[i].set_value_no_signal(float(v[i]))
	if _kit_slot >= 0 and _kit_line != null:
		var k := _placed_kit(_kit_slot)
		if not k.is_empty():
			var kit_id := StringName(k.get("kit", &""))
			_kit_line.text = tr(&"editor_tray.inspector.kit_line").format([_kit_name(kit_id), Kit.number(float(Kit.get_int(k, "version", 1)), &"none", 0)])
			var latest := Kit.get_int(k, "latest_version", 1)
			_kit_update.visible = latest > Kit.get_int(k, "version", 1)
			_kit_update.text = tr(&"editor_tray.inspector.kit_update").format([Kit.number(float(latest), &"none", 0)])
			var values: Variant = k.get("settings", {})
			if values is Dictionary:
				for key: String in _settings:
					var ctl: Array = _settings[key]
					var v: float = float((values as Dictionary).get(key, ctl[3]))
					var slider: HSlider = ctl[0]
					var field: SpinBox = ctl[1]
					if not slider.has_focus() or not Input.is_mouse_button_pressed(MOUSE_BUTTON_LEFT):
						slider.set_value_no_signal(v)
					if not field.get_line_edit().has_focus():
						field.set_value_no_signal(v)
					(ctl[2] as Button).visible = not is_equal_approx(v, float(ctl[3]))


func _commit_label() -> void:
	var e := _editor()
	if e == null or _slot < 0 or _label_field == null:
		return
	var p := _placed(_slot)
	var text := _label_field.text.strip_edges()
	if not p.is_empty() and text != Kit.get_str(p, "label"):
		e.call(&"set_label", _slot, text)


func _on_load_moved(v: float) -> void:
	_load_value.text = Kit.percent(v)
	if not Input.is_mouse_button_pressed(MOUSE_BUTTON_LEFT):
		_commit_load()


func _commit_load() -> void:
	var e := _editor()
	if e != null and _slot >= 0 and _load_slider != null:
		e.call(&"set_load", _slot, _load_slider.value)


func _commit_setting(key: String, value: float) -> void:
	var e := _editor()
	if e != null and _kit_slot >= 0:
		e.call(&"set_kit_setting", _kit_slot, key, value)


func _on_cell_field(_value: float, _axis: int) -> void:
	var p := _placed(_slot)
	if p.is_empty():
		return
	var to := Vector3i(int(_cell_fields[0].value), int(_cell_fields[1].value), int(_cell_fields[2].value))
	if not bool(_editor().call(&"move_part", _slot, to, Kit.get_int(p, "yaw"), Kit.get_int(p, "tilt"))):
		_update_part(p)
		_kind_line.text = tr(&"editor_tray.inspector.move_blocked")


func _turn(yaw: int, tilt: int) -> void:
	var p := _placed(_slot)
	if p.is_empty():
		return
	var ok := bool(_editor().call(&"move_part", _slot, p.get("cell", Vector3i.ZERO), posmod(Kit.get_int(p, "yaw") + yaw, 4), posmod(Kit.get_int(p, "tilt") + tilt, 3)))
	if not ok:
		_kind_line.text = tr(&"editor_tray.inspector.move_blocked")


# --- several parts --------------------------------------------------------------------------------

func _build_many(parts: PackedInt32Array) -> void:
	_clear(_body)
	_slot = -1
	_kit_slot = -1
	_title = Kit.label("", &"PanelTitle", true)
	_body.add_child(_title)
	_many_line = Kit.label("", &"Dense", true)
	_body.add_child(_many_line)
	var actions := HFlowContainer.new()
	actions.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	actions.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	var copy := Kit.inline_button("editor_tray.inspector.copy")
	copy.pressed.connect(func() -> void: _editor().call(&"copy_selection"))
	var arcs := Kit.inline_button("editor_tray.inspector.arcs")
	arcs.tooltip_text = tr(&"editor_tray.inspector.arcs_tip")
	arcs.pressed.connect(func() -> void: focus_arcs.emit(parts))
	var delete := Kit.inline_button("editor_tray.inspector.delete")
	delete.pressed.connect(func() -> void: _editor().call(&"delete_selection"))
	for b: Button in [copy, arcs, delete]:
		b.custom_minimum_size.y = 40.0
		b.set_meta(&"control_id", "editor.inspector.actions")
		actions.add_child(b)
	_body.add_child(actions)
	_body.add_child(Kit.label("editor_tray.inspector.capture", &"Dense"))
	var row := Kit.hbox(1)
	var field := LineEdit.new()
	field.placeholder_text = tr(&"editor_shell.capture.placeholder")
	field.accessibility_name = tr(&"editor_tray.inspector.capture")
	field.custom_minimum_size = Vector2(0.0, 40.0)
	field.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	field.max_length = 48
	var capture := Kit.inline_button("editor_tray.inspector.capture_confirm")
	capture.custom_minimum_size.y = 40.0
	capture.pressed.connect(_capture.bind(field))
	field.text_submitted.connect(func(_t: String) -> void: _capture(field))
	row.add_child(field)
	row.add_child(capture)
	_body.add_child(row)


func _update_many(sel: Dictionary, n: int) -> void:
	_title.text = tr(&"editor_tray.inspector.many").format([Kit.number(float(n), &"none", 0)])
	_many_line.text = tr(&"editor_tray.inspector.many_line").format([Kit.number(Kit.get_float(sel, "mass_kg") / 1000.0, &"t", 1), Kit.number(float(Kit.get_int(sel, "cost")), &"none", 0)])


func _capture(field: LineEdit) -> void:
	var made := Kit.as_dict(_editor().call(&"capture_prefab", field.text.strip_edges()))
	if made.is_empty():
		_many_line.text = tr(&"editor_shell.capture.failed_body")
	else:
		field.text = ""
		Screens.toast({"key": &"editor_shell.capture.done", "args": [String(made.get("name", ""))]})


# --- kit updates ----------------------------------------------------------------------------------

func _on_kit_update_offered(kit: StringName, _copies: int) -> void:
	_put_off.erase(String(kit))
	_refresh_updates()


func _refresh_updates() -> void:
	_clear(_updates)
	var e := _editor()
	if e == null or not bool(e.call(&"is_open")):
		_updates.visible = false
		return
	for u: Dictionary in Kit.dicts(e.call(&"kit_updates")):
		var kit := StringName(u.get("kit", &""))
		var latest := Kit.get_int(u, "latest_version")
		if _put_off.get(String(kit), -1) == latest:
			continue
		var line := Kit.label("", &"Body", true)
		line.text = tr(&"editor_tray.update.line").format([_kit_name(kit), Kit.number(float(latest), &"none", 0),
			Kit.number(float(Kit.get_int(u, "copies")), &"none", 0), Kit.number(float(Kit.get_int(u, "placed_version")), &"none", 0)])
		_updates.add_child(line)
		var row := Kit.hbox(1)
		var update := Kit.inline_button("")
		update.text = tr(&"editor_tray.update.confirm").format([Kit.number(float(Kit.get_int(u, "copies")), &"none", 0)])
		update.custom_minimum_size.y = 40.0
		update.pressed.connect(_migrate.bind(kit))
		update.set_meta(&"control_id", "editor.inspector.kit")
		var later := Kit.inline_button("editor_tray.update.later")
		later.custom_minimum_size.y = 40.0
		later.pressed.connect(func() -> void:
			_put_off[String(kit)] = latest
			_refresh_updates())
		row.add_child(update)
		row.add_child(later)
		_updates.add_child(row)
	_updates.visible = _updates.get_child_count() > 0
	_rule.visible = _updates.visible or _scroll.visible


func _migrate(kit: StringName) -> void:
	_editor().call(&"migrate_kit", kit)
	_refresh_updates()


func _allowed(id: String) -> bool:
	var t := Kit.autoload(&"Tutorial")
	return t == null or not t.has_method(&"is_allowed") or bool(t.call(&"is_allowed", id))


func apply_tutorial() -> void:
	for c: Node in find_children("*", "Control", true, false):
		if c.has_meta(&"control_id"):
			var ok := _allowed(String(c.get_meta(&"control_id")))
			if c is BaseButton:
				(c as BaseButton).disabled = not ok
			elif c is LineEdit:
				(c as LineEdit).editable = ok
			elif c is Slider:
				(c as Slider).editable = ok
			elif c is SpinBox:
				(c as SpinBox).editable = ok
