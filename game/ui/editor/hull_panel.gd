extends VBoxContainer
## Hull handles as numeric fields, stations, decks, bulkheads, bake, sculpt.
## DESIGN.md L122-134. Unit: ui-editor-hull.
## Reads and calls: Editor.hull, Editor.handles, Editor.set_handle, Editor.add_plane, Editor.bake_hull, BpEditorView.handle_points.
##
## Three modes, each a dry-dock tool (the dock's view clicks follow it):
## - Shape (`Bp.TOOL_HULL_HANDLES`): length, mirroring, the hull-level handles (bow rake, transom rake and
##   width) and, for one station at a time, beam, draft, deadrise, flare and sheer (L124: every handle has a
##   numeric field, so no edit needs a drag). The station picker steps through the 4 to 16 stations; a drag
##   on a handle in the view picks its station here. Stations are added at a position along the hull and
##   removed down to four.
## - Decks and bulkheads (`Bp.TOOL_PLANES`, L132): each plane's position as a field (snapped to 0.5 m), add
##   and remove, and how many compartments they cut.
## - Sculpt (`Bp.TOOL_SCULPT`, L128, L134): add or remove cells in one of the five shapes (cube, 45 degree
##   wedge, corner, inverse corner, 1:2 slope) in 24 orientations. Bake turns the lofted shell into freeform
##   cells after a confirmation: the handles are gone afterwards and only Undo brings them back.
## A baked hull shows no shape fields and says why. Fields update in place on every design change and the
## one being typed in is never overwritten.

signal tool_requested(tool: int)
signal frame_requested(what: StringName)

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const STATION_IDS: Array[String] = ["beam", "draft", "deadrise", "flare", "sheer"]
const HULL_IDS: Array[String] = ["bow_rake", "transom", "transom_width"]
const MIN_STATIONS := 4
const MAX_STATIONS := 16
const PLANE_STEP_M := 0.5
const FIELD_H := 40.0
## Sculpt shapes as `Editor.sculpt` numbers them.
const SHAPES: Array[StringName] = [&"editor_hull.shape.cube", &"editor_hull.shape.wedge", &"editor_hull.shape.corner",
	&"editor_hull.shape.inverse_corner", &"editor_hull.shape.slope"]
const ORIENTATIONS := 24

enum Page { SHAPE, PLANES, SCULPT }

@onready var _mode_buttons: Array[Button] = [$Modes/Shape, $Modes/Planes, $Modes/Sculpt]
@onready var _page: VBoxContainer = $Scroll/Page

var _current_page: int = Page.SHAPE
var _station := 0
var _fields: Dictionary[String, SpinBox] = {}
var _layout := ""
var _refresh_pending := false
var _sculpt_add := true
var _sculpt_shape := 0
var _sculpt_orientation := 0
var _length: SpinBox
var _mirrored: CheckButton
var _station_pick: OptionButton
var _station_remove: Button
var _add_station_at: SpinBox
var _add_station: Button
var _compartments: Label
var _plane_fields: Dictionary[String, SpinBox] = {}
var _add_deck_at: SpinBox
var _add_bulkhead_at: SpinBox
var _orientation: SpinBox
var _shape_buttons: Array[Button] = []
var _add_mode: Button
var _remove_mode: Button
var _station_count := 0
var _stale := false


func _ready() -> void:
	Ui.set_separation(self, 2)
	Ui.set_separation($Modes as BoxContainer, 1)
	Ui.set_separation(_page, 2)
	accessibility_name = tr(&"editor_hull.name")
	var group := ButtonGroup.new()
	for i in _mode_buttons.size():
		_mode_buttons[i].button_group = group
		_mode_buttons[i].pressed.connect(_on_mode.bind(i))
	_mode_buttons[Page.SHAPE].set_meta(&"control_id", "editor.hull.shape")
	_mode_buttons[Page.PLANES].set_meta(&"control_id", "editor.hull.planes")
	_mode_buttons[Page.SCULPT].set_meta(&"control_id", "editor.hull.sculpt")
	_mode_buttons[Page.SHAPE].tooltip_text = tr(&"editor_hull.mode.shape_tip")
	_mode_buttons[Page.PLANES].tooltip_text = tr(&"editor_hull.mode.planes_tip")
	_mode_buttons[Page.SCULPT].tooltip_text = tr(&"editor_hull.mode.sculpt_tip")
	var editor := _editor()
	if editor != null:
		editor.connect(&"design_changed", func(_rev: int) -> void: _queue_refresh())
		editor.connect(&"tool_changed", _on_tool_changed)
		if editor.has_signal(&"gauges_changed"):
			editor.connect(&"gauges_changed", _refresh_compartments)
	visibility_changed.connect(func() -> void:
		if _stale and is_visible_in_tree():
			_stale = false
			_refresh())
	_refresh()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_layout = ""
		_refresh()


func _editor() -> Node:
	return Kit.autoload(&"Editor")


# --- what the dock asks -----------------------------------------------------------------------------

## The tool this panel's mode stands for.
func tool() -> int:
	match _current_page:
		Page.PLANES:
			return Bp.TOOL_PLANES
		Page.SCULPT:
			return Bp.TOOL_SCULPT
	return Bp.TOOL_HULL_HANDLES if not _baked() else Bp.TOOL_SCULPT


func current_station() -> int:
	return _station


## A handle drag in the view picked a station: show its fields.
func focus_station(index: int) -> void:
	if index == _station:
		return
	_station = index
	if _station_pick != null and index < _station_pick.item_count:
		_station_pick.select(index)
	_refresh_values()


func sculpt_adds() -> bool:
	return _sculpt_add


func sculpt_shape() -> int:
	return _sculpt_shape


func sculpt_orientation() -> int:
	return _sculpt_orientation


# --- modes ------------------------------------------------------------------------------------------

func _on_mode(page: int) -> void:
	if not _allowed(String(_mode_buttons[page].get_meta(&"control_id"))):
		_mode_buttons[_current_page].set_pressed_no_signal(true)
		return
	_current_page = page
	_layout = ""
	_refresh()
	tool_requested.emit(tool())


## Show a mode without the tutorial gate (the dock does this when a step points into the panel).
func show_page(page: int) -> void:
	_current_page = clampi(page, 0, 2)
	_layout = ""
	_refresh()


func _on_tool_changed(t: int) -> void:
	var want := -1
	match t:
		Bp.TOOL_HULL_HANDLES:
			want = Page.SHAPE
		Bp.TOOL_PLANES:
			want = Page.PLANES
		Bp.TOOL_SCULPT:
			want = Page.SCULPT
	if want >= 0 and want != _current_page:
		_current_page = want
		_layout = ""
		_refresh()


func _baked() -> bool:
	var e := _editor()
	return e != null and Kit.get_bool(Kit.as_dict(e.call(&"hull")), "baked")


# --- refresh ----------------------------------------------------------------------------------------

func _queue_refresh() -> void:
	if _refresh_pending:
		return
	if not is_visible_in_tree():
		_stale = true
		return
	_refresh_pending = true
	_refresh.call_deferred()


## Rebuild the page when its shape changed (mode, station count, plane count, bake), else update values.
func _refresh() -> void:
	_refresh_pending = false
	if not is_node_ready():
		return
	for i in _mode_buttons.size():
		_mode_buttons[i].set_pressed_no_signal(i == _current_page)
	var e := _editor()
	var open := e != null and bool(e.call(&"is_open"))
	var hull := Kit.as_dict(e.call(&"hull")) if open else {}
	var stations: Array = hull.get("stations", []) if hull.get("stations") is Array else []
	var decks: PackedFloat32Array = hull.get("decks", PackedFloat32Array()) if hull.get("decks") is PackedFloat32Array else PackedFloat32Array()
	var bulkheads: PackedFloat32Array = hull.get("bulkheads", PackedFloat32Array()) if hull.get("bulkheads") is PackedFloat32Array else PackedFloat32Array()
	var layout := "%s:%d:%d:%d:%d:%s" % [str(open), _current_page, stations.size(), decks.size(), bulkheads.size(), str(Kit.get_bool(hull, "baked"))]
	if layout != _layout:
		_layout = layout
		_build(open, hull)
	_refresh_values(hull)
	apply_tutorial()


func _build(open: bool, hull: Dictionary) -> void:
	for c: Node in _page.get_children():
		_page.remove_child(c)
		c.queue_free()
	_fields.clear()
	_plane_fields.clear()
	_shape_buttons.clear()
	_length = null
	_mirrored = null
	_station_pick = null
	_compartments = null
	if not open:
		_page.add_child(Kit.label("editor_hull.closed", &"Body"))
		return
	match _current_page:
		Page.SHAPE:
			_build_shape(hull)
		Page.PLANES:
			_build_planes(hull)
		Page.SCULPT:
			_build_sculpt(hull)


# --- shape ------------------------------------------------------------------------------------------

func _build_shape(hull: Dictionary) -> void:
	if Kit.get_bool(hull, "baked"):
		_page.add_child(Kit.label("editor_hull.baked.title", &"PanelTitle"))
		_page.add_child(Kit.label("editor_hull.baked.body", &"Body"))
		var go := Kit.inline_button("editor_hull.baked.go_sculpt")
		go.custom_minimum_size.y = FIELD_H
		go.pressed.connect(_on_mode.bind(Page.SCULPT))
		_page.add_child(go)
		return
	var e := _editor()
	var limits := Kit.as_dict(e.call(&"limits"))
	_page.add_child(Kit.label("editor_hull.section.hull", &"PanelTitle"))
	var grid := _grid()
	_length = _field(&"editor_hull.field.length", &"editor_hull.tip.length", "m", 0.5)
	_length.min_value = 10.0
	_length.max_value = maxf(Kit.get_float(limits, "max_length_m", 90.0), 10.0)
	_length.set_meta(&"control_id", "editor.handle.length")
	_length.value_changed.connect(_on_length)
	_add_row(grid, &"editor_hull.field.length", _length)
	for id: String in HULL_IDS:
		var f := _field(StringName("editor_hull.field." + id), StringName("editor_hull.tip." + id), "", 0.01)
		f.set_meta(&"control_id", "editor.handle." + id)
		f.value_changed.connect(_on_handle_field.bind(id, 0))
		_fields["%s:0" % id] = f
		_add_row(grid, StringName("editor_hull.field." + id), f)
	_page.add_child(grid)
	_mirrored = CheckButton.new()
	_mirrored.text = tr(&"editor_hull.field.mirrored")
	_mirrored.tooltip_text = tr(&"editor_hull.tip.mirrored")
	_mirrored.custom_minimum_size.y = FIELD_H
	_mirrored.toggled.connect(func(on: bool) -> void: _editor().call(&"set_mirrored", on))
	_mirrored.set_meta(&"control_id", "editor.hull.mirrored")
	_page.add_child(_mirrored)
	_page.add_child(HSeparator.new())
	_page.add_child(Kit.label("editor_hull.section.stations", &"PanelTitle"))
	var stations: Array = hull.get("stations", [])
	_station = clampi(_station, 0, maxi(stations.size() - 1, 0))
	var pick_row := Kit.hbox(1)
	var prev := Kit.icon_button(&"chevron_left", tr(&"editor_hull.station.prev"))
	prev.custom_minimum_size = Vector2(FIELD_H, FIELD_H)
	prev.pressed.connect(_step_station.bind(-1))
	_station_pick = OptionButton.new()
	_station_pick.custom_minimum_size.y = FIELD_H
	_station_pick.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_station_pick.accessibility_name = tr(&"editor_hull.station.pick")
	_station_pick.item_selected.connect(func(i: int) -> void:
		_station = i
		_refresh_values())
	var next := Kit.icon_button(&"chevron_right", tr(&"editor_hull.station.next"))
	next.custom_minimum_size = Vector2(FIELD_H, FIELD_H)
	next.pressed.connect(_step_station.bind(1))
	for c: Control in [prev, _station_pick, next]:
		c.set_meta(&"control_id", "editor.hull.station")
		pick_row.add_child(c)
	_page.add_child(pick_row)
	var sgrid := _grid()
	for id: String in STATION_IDS:
		var f := _field(StringName("editor_hull.field." + id), StringName("editor_hull.tip." + id), "", 0.1)
		f.set_meta(&"control_id", "editor.handle." + id)
		f.value_changed.connect(_on_station_field.bind(id))
		_fields["%s:*" % id] = f
		_add_row(sgrid, StringName("editor_hull.field." + id), f)
	_page.add_child(sgrid)
	_station_remove = Kit.inline_button("editor_hull.station.remove")
	_station_remove.custom_minimum_size.y = FIELD_H
	_station_remove.tooltip_text = tr(&"editor_hull.station.remove_tip")
	_station_remove.pressed.connect(_on_remove_station)
	_station_remove.set_meta(&"control_id", "editor.hull.station")
	_page.add_child(_station_remove)
	var add_row := Kit.hbox(1)
	var add_label := Kit.label("editor_hull.station.add_at", &"Body")
	add_label.autowrap_mode = TextServer.AUTOWRAP_OFF
	add_row.add_child(add_label)
	_add_station_at = _field(&"editor_hull.station.add_at", &"editor_hull.station.add_tip", "m", 0.5)
	_add_station_at.min_value = 0.0
	_add_station_at.max_value = 150.0
	_add_station_at.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_add_station_at.set_meta(&"control_id", "editor.hull.station")
	_add_station = Kit.inline_button("editor_hull.station.add")
	_add_station.custom_minimum_size.y = FIELD_H
	_add_station.pressed.connect(_on_add_station)
	_add_station.set_meta(&"control_id", "editor.hull.station")
	add_row.add_child(_add_station_at)
	add_row.add_child(_add_station)
	_page.add_child(add_row)
	var frame := Kit.inline_button("editor_hull.frame")
	frame.custom_minimum_size.y = FIELD_H
	frame.pressed.connect(func() -> void: frame_requested.emit(&"port"))
	_page.add_child(frame)


func _step_station(d: int) -> void:
	if _station_pick == null or _station_pick.item_count == 0:
		return
	_station = clampi(_station + d, 0, _station_pick.item_count - 1)
	_station_pick.select(_station)
	_refresh_values()


func _on_length(v: float) -> void:
	var applied := float(_editor().call(&"set_length", v))
	_length.set_value_no_signal(applied)


func _on_handle_field(v: float, id: String, station: int) -> void:
	var applied: Variant = _editor().call(&"set_handle", id, station, v, false)
	if applied is float:
		_fields["%s:%d" % [id, station]].set_value_no_signal(applied)


func _on_station_field(v: float, id: String) -> void:
	var applied: Variant = _editor().call(&"set_handle", id, _station, v, false)
	if applied is float:
		_fields["%s:*" % id].set_value_no_signal(applied)


func _on_remove_station() -> void:
	if not bool(_editor().call(&"remove_station", _station)):
		_station_remove.tooltip_text = tr(&"editor_hull.station.remove_refused")
	_station = maxi(_station - 1, 0)


func _on_add_station() -> void:
	var index := int(_editor().call(&"add_station", _add_station_at.value))
	if index >= 0:
		_station = index


# --- planes -----------------------------------------------------------------------------------------

func _build_planes(hull: Dictionary) -> void:
	for axis: StringName in [&"deck", &"bulkhead"]:
		var list: PackedFloat32Array = hull.get("decks" if axis == &"deck" else "bulkheads", PackedFloat32Array())
		_page.add_child(Kit.label("editor_hull.section.decks" if axis == &"deck" else "editor_hull.section.bulkheads", &"PanelTitle"))
		if list.is_empty():
			_page.add_child(Kit.label("editor_hull.planes.none_decks" if axis == &"deck" else "editor_hull.planes.none_bulkheads", &"Dense"))
		var grid := GridContainer.new()
		grid.columns = 3
		grid.add_theme_constant_override(&"h_separation", Ui.spacing(1))
		grid.add_theme_constant_override(&"v_separation", Ui.spacing(1))
		for i in list.size():
			var name_key := &"editor_hull.planes.deck" if axis == &"deck" else &"editor_hull.planes.bulkhead"
			var label := Kit.label("", &"Body", true)
			label.text = tr(name_key).format([Kit.number(float(i + 1), &"none", 0)])
			label.autowrap_mode = TextServer.AUTOWRAP_OFF
			label.size_flags_horizontal = Control.SIZE_EXPAND_FILL
			grid.add_child(label)
			var f := _field(name_key, &"editor_hull.tip.deck" if axis == &"deck" else &"editor_hull.tip.bulkhead", "m", PLANE_STEP_M)
			f.min_value = 0.0
			f.max_value = 150.0
			f.accessibility_name = label.text
			f.get_line_edit().accessibility_name = label.text
			f.value_changed.connect(_on_plane_field.bind(axis, i))
			f.set_meta(&"control_id", "editor.hull.planes")
			_plane_fields["%s:%d" % [axis, i]] = f
			grid.add_child(f)
			var remove := Kit.icon_button(&"delete", tr(&"editor_hull.planes.remove").format([label.text]))
			remove.custom_minimum_size = Vector2(FIELD_H, FIELD_H)
			remove.pressed.connect(func() -> void: _editor().call(&"remove_plane", axis, i))
			remove.set_meta(&"control_id", "editor.hull.planes")
			grid.add_child(remove)
		_page.add_child(grid)
		var add_row := Kit.hbox(1)
		var add_label := Kit.label("editor_hull.planes.add_at", &"Body")
		add_label.autowrap_mode = TextServer.AUTOWRAP_OFF
		add_row.add_child(add_label)
		var at := _field(&"editor_hull.planes.add_at", &"editor_hull.tip.add_plane", "m", PLANE_STEP_M)
		at.min_value = 0.0
		at.max_value = 150.0
		at.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		at.set_meta(&"control_id", "editor.hull.planes")
		var add := Kit.inline_button("editor_hull.planes.add_deck" if axis == &"deck" else "editor_hull.planes.add_bulkhead")
		add.custom_minimum_size.y = FIELD_H
		add.pressed.connect(_on_add_plane.bind(axis, at))
		add.set_meta(&"control_id", "editor.hull.planes")
		add_row.add_child(at)
		add_row.add_child(add)
		_page.add_child(add_row)
		if axis == &"deck":
			_add_deck_at = at
		else:
			_add_bulkhead_at = at
		_page.add_child(HSeparator.new())
	_compartments = Kit.label("", &"Body", true)
	_page.add_child(_compartments)


func _on_plane_field(v: float, axis: StringName, index: int) -> void:
	var applied: Variant = _editor().call(&"move_plane", axis, index, v)
	if applied is float:
		_plane_fields["%s:%d" % [axis, index]].set_value_no_signal(applied)


func _on_add_plane(axis: StringName, at: SpinBox) -> void:
	if int(_editor().call(&"add_plane", axis, at.value)) < 0:
		at.tooltip_text = tr(&"editor_hull.planes.add_refused")
		at.get_line_edit().select_all()


func _refresh_compartments() -> void:
	if _compartments == null or not is_instance_valid(_compartments):
		return
	var e := _editor()
	var n := Kit.dicts(e.call(&"compartments")).size() if e != null else 0
	_compartments.text = tr(&"editor_hull.planes.compartments").format([Kit.number(float(n), &"none", 0)]) if n > 0 else tr(&"editor_hull.planes.compartments_pending")


# --- sculpt -----------------------------------------------------------------------------------------

func _build_sculpt(hull: Dictionary) -> void:
	_page.add_child(Kit.label("editor_hull.section.sculpt", &"PanelTitle"))
	_page.add_child(Kit.label("editor_hull.sculpt.body_baked" if Kit.get_bool(hull, "baked") else "editor_hull.sculpt.body_lofted", &"Dense"))
	var modes := Kit.hbox(1)
	var mg := ButtonGroup.new()
	_add_mode = Kit.inline_button("editor_hull.sculpt.add")
	_remove_mode = Kit.inline_button("editor_hull.sculpt.remove")
	for b: Button in [_add_mode, _remove_mode]:
		b.toggle_mode = true
		b.button_group = mg
		b.custom_minimum_size.y = FIELD_H
		b.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		b.set_meta(&"control_id", "editor.hull.sculpt")
		modes.add_child(b)
	_add_mode.set_pressed_no_signal(_sculpt_add)
	_remove_mode.set_pressed_no_signal(not _sculpt_add)
	_add_mode.pressed.connect(func() -> void: _sculpt_add = true)
	_remove_mode.pressed.connect(func() -> void: _sculpt_add = false)
	_page.add_child(modes)
	_page.add_child(Kit.label("editor_hull.sculpt.shape", &"Dense"))
	var shapes := HFlowContainer.new()
	shapes.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	shapes.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	var sg := ButtonGroup.new()
	for i in SHAPES.size():
		var b := Kit.inline_button(String(SHAPES[i]))
		b.toggle_mode = true
		b.button_group = sg
		b.custom_minimum_size.y = FIELD_H
		b.set_pressed_no_signal(i == _sculpt_shape)
		b.pressed.connect(func() -> void: _sculpt_shape = i)
		b.set_meta(&"control_id", "editor.hull.sculpt")
		shapes.add_child(b)
		_shape_buttons.append(b)
	_page.add_child(shapes)
	var orient := Kit.hbox(1)
	_orientation = _field(&"editor_hull.sculpt.orientation", &"editor_hull.sculpt.orientation_tip", "", 1.0)
	_orientation.min_value = 1
	_orientation.max_value = ORIENTATIONS
	_orientation.rounded = true
	_orientation.suffix = tr(&"editor_hull.sculpt.of").format([Kit.number(float(ORIENTATIONS), &"none", 0)])
	_orientation.set_value_no_signal(float(_sculpt_orientation + 1))
	_orientation.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_orientation.value_changed.connect(func(v: float) -> void: _sculpt_orientation = clampi(int(v) - 1, 0, ORIENTATIONS - 1))
	_orientation.set_meta(&"control_id", "editor.hull.sculpt")
	var orient_label := Kit.label("editor_hull.sculpt.orientation", &"Dense")
	orient_label.autowrap_mode = TextServer.AUTOWRAP_OFF
	orient.add_child(orient_label)
	orient.add_child(_orientation)
	var turn := Kit.inline_button("editor_hull.sculpt.next")
	turn.custom_minimum_size.y = FIELD_H
	turn.pressed.connect(func() -> void: _orientation.value = float(posmod(_sculpt_orientation + 1, ORIENTATIONS) + 1))
	turn.set_meta(&"control_id", "editor.hull.sculpt")
	orient.add_child(turn)
	_page.add_child(orient)
	if not Kit.get_bool(hull, "baked"):
		_page.add_child(HSeparator.new())
		_page.add_child(Kit.label("editor_hull.bake.title", &"PanelTitle"))
		_page.add_child(Kit.label("editor_hull.bake.body", &"Dense"))
		var bake := Kit.inline_button("editor_hull.bake.button")
		bake.custom_minimum_size.y = FIELD_H
		bake.pressed.connect(_confirm_bake)
		bake.set_meta(&"control_id", "editor.hull.bake")
		_page.add_child(bake)


## Baking is one-way (L134): ask first, with Keep the shell as the safe default.
func _confirm_bake() -> void:
	var dialog: Node = Screens.open_dialog(&"editor_hull.bake.confirm_title", &"editor_hull.bake.confirm_body", [
		{"id": &"keep", "label": &"editor_hull.bake.keep"},
		{"id": &"bake", "label": &"editor_hull.bake.confirm"},
	], &"keep", &"keep")
	if dialog != null:
		dialog.connect(&"chosen", _on_bake_chosen)


func _on_bake_chosen(choice: StringName) -> void:
	if choice != &"bake":
		return
	if bool(_editor().call(&"bake_hull")):
		_current_page = Page.SCULPT
		_layout = ""
		_refresh()
		tool_requested.emit(Bp.TOOL_SCULPT)
	else:
		Screens.open_message(&"editor_hull.bake.failed_title", &"editor_hull.bake.failed_body")


# --- values -----------------------------------------------------------------------------------------

func _refresh_values(known: Dictionary = {}) -> void:
	var e := _editor()
	if e == null or not bool(e.call(&"is_open")):
		return
	var hull := known if not known.is_empty() else Kit.as_dict(e.call(&"hull"))
	if _length != null and not _length.get_line_edit().has_focus():
		_length.set_block_signals(true)
		_length.value = Kit.get_float(hull, "length_m")
		_length.set_block_signals(false)
	if _mirrored != null:
		_mirrored.set_pressed_no_signal(Kit.get_bool(hull, "mirrored"))
	var stations: Array = hull.get("stations", []) if hull.get("stations") is Array else []
	if _station_pick != null:
		var keep := clampi(_station, 0, maxi(stations.size() - 1, 0))
		_station_pick.clear()
		for s: Variant in stations:
			if s is Dictionary:
				var sd := s as Dictionary
				_station_pick.add_item(tr(&"editor_hull.station.item").format([Kit.number(float(Kit.get_int(sd, "index") + 1), &"none", 0),
					Kit.number(float(stations.size()), &"none", 0), Kit.number(Kit.get_float(sd, "x_m"), &"m", 1)]))
		_station = keep
		if _station_pick.item_count > 0:
			_station_pick.select(keep)
		_station_count = stations.size()
		_station_remove.disabled = _station_count <= MIN_STATIONS or not _allowed("editor.hull.station")
		_add_station.disabled = _station_count >= MAX_STATIONS or not _allowed("editor.hull.station")
		_add_station_at.set_block_signals(true)
		_add_station_at.max_value = maxf(Kit.get_float(hull, "length_m"), 1.0)
		_add_station_at.set_block_signals(false)
		if not _add_station_at.get_line_edit().has_focus():
			_add_station_at.set_value_no_signal(_midpoint(stations))
	for h: Dictionary in Kit.dicts(e.call(&"handles")):
		var id := String(h.get("id", ""))
		var station := Kit.get_int(h, "station")
		var f: SpinBox = null
		if id in HULL_IDS:
			f = _fields.get("%s:0" % id)
		elif id in STATION_IDS and station == _station:
			f = _fields.get("%s:*" % id)
		if f == null:
			continue
		_apply_handle(f, h)
	for key: String in _plane_fields:
		var f := _plane_fields[key]
		var axis := key.get_slice(":", 0)
		var i := key.get_slice(":", 1).to_int()
		var list: PackedFloat32Array = hull.get("decks" if axis == "deck" else "bulkheads", PackedFloat32Array())
		if i < list.size() and not f.get_line_edit().has_focus():
			f.set_value_no_signal(float(list[i]))
	if _add_deck_at != null and is_instance_valid(_add_deck_at) and not _add_deck_at.get_line_edit().has_focus():
		var decks: PackedFloat32Array = hull.get("decks", PackedFloat32Array())
		var spacing := Kit.get_float(hull, "deck_spacing_m", 2.5)
		_add_deck_at.set_value_no_signal(snappedf((decks[decks.size() - 1] + spacing) if not decks.is_empty() else spacing, PLANE_STEP_M))
	if _add_bulkhead_at != null and is_instance_valid(_add_bulkhead_at) and not _add_bulkhead_at.get_line_edit().has_focus():
		_add_bulkhead_at.set_value_no_signal(snappedf(Kit.get_float(hull, "length_m") * 0.5, PLANE_STEP_M))
	_refresh_compartments()


## A station field takes its handle's range and step, so it never offers a value the hull would refuse.
func _apply_handle(f: SpinBox, h: Dictionary) -> void:
	var lo := Kit.get_float(h, "min")
	var hi := Kit.get_float(h, "max")
	var step := Kit.get_float(h, "step", 0.1)
	# Changing the range clamps the value and would report it as an edit: no signals while it changes.
	f.set_block_signals(true)
	f.step = step if step > 0.0 else 0.01
	f.min_value = lo
	f.max_value = maxf(hi, lo)
	f.suffix = _unit_suffix(StringName(h.get("unit", &"none")))
	if not f.get_line_edit().has_focus():
		f.value = Kit.get_float(h, "value")
	f.set_block_signals(false)
	f.tooltip_text = tr(StringName(f.get_meta(&"tip", &""))) + "\n" + tr(&"editor_hull.tip.range").format([
		Kit.number(lo, StringName(h.get("unit", &"none")), Kit.decimals_for(f.step)), Kit.number(hi, StringName(h.get("unit", &"none")), Kit.decimals_for(f.step))])


static func _midpoint(stations: Array) -> float:
	if stations.size() < 2:
		return 0.0
	var best := 0.0
	var at := 0.0
	for i in range(1, stations.size()):
		var a := float((stations[i - 1] as Dictionary).get("x_m", 0.0))
		var b := float((stations[i] as Dictionary).get("x_m", 0.0))
		if absf(b - a) > best:
			best = absf(b - a)
			at = (a + b) * 0.5
	return snappedf(at, 0.5)


func _unit_suffix(unit: StringName) -> String:
	match unit:
		&"m":
			return tr(&"editor_hull.unit.m")
		&"deg":
			return tr(&"editor_hull.unit.deg")
	return ""


# --- helpers ----------------------------------------------------------------------------------------

func _grid() -> GridContainer:
	var g := GridContainer.new()
	g.columns = 2
	g.add_theme_constant_override(&"h_separation", Ui.spacing(2))
	g.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	return g


func _add_row(grid: GridContainer, label_key: StringName, field: SpinBox) -> void:
	var l := Kit.label(String(label_key), &"Body")
	l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	l.autowrap_mode = TextServer.AUTOWRAP_OFF
	l.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
	grid.add_child(l)
	grid.add_child(field)


func _field(name_key: StringName, tip_key: StringName, unit: String, step: float) -> SpinBox:
	var f := SpinBox.new()
	f.step = step
	f.custom_minimum_size = Vector2(160.0, FIELD_H)
	f.select_all_on_focus = true
	f.update_on_text_changed = false
	f.suffix = _unit_suffix(StringName(unit)) if not unit.is_empty() else ""
	f.tooltip_text = tr(tip_key)
	f.set_meta(&"tip", tip_key)
	f.get_line_edit().accessibility_name = tr(name_key)
	return f


func _allowed(id: String) -> bool:
	var t := Kit.autoload(&"Tutorial")
	return t == null or not t.has_method(&"is_allowed") or bool(t.call(&"is_allowed", id))


## Disable the fields and buttons the current tutorial step does not allow (`editor.handle.<id>`,
## `editor.hull.*`). The beam field stays live in the step that teaches the beam handle.
func apply_tutorial() -> void:
	for c: Node in find_children("*", "Control", true, false):
		if not c.has_meta(&"control_id"):
			continue
		var ok := _allowed(String(c.get_meta(&"control_id")))
		if c == _station_remove:
			_station_remove.disabled = not ok or _station_count <= MIN_STATIONS
		elif c == _add_station:
			_add_station.disabled = not ok or _station_count >= MAX_STATIONS
		elif c is BaseButton:
			(c as BaseButton).disabled = not ok
		elif c is SpinBox:
			(c as SpinBox).editable = ok
	if _station_pick != null and _fields.has("beam:*"):
		# The station picker serves the beam field too.
		_station_pick.disabled = not (_allowed("editor.hull.station") or _allowed("editor.handle.beam"))
