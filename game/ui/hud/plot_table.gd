extends Control
## Plot table panel around BpPlotView: layers, scale, legend, tools, cursor.
## DESIGN.md L106, L1669-1670. Unit: ui-hud-plot.
## Reads and calls: BpPlotView, Orders.set_course, Orders.designate, Hud.select_track.
##
## The plot (drawn in Rust) fills the panel; a tool column sits at its left edge, vertically
## centred, clear of the objectives and pacing bar the HUD shell puts along the top:
## - Tools: Orders (default: click a track to designate it and select it for the track card,
##   right-click a track to select it only, right-click the sea to set course, Shift to add a
##   waypoint, L108), Measure (bearing and range between two points, drawn by the plot), and Mine
##   line while the hull can lay mines (L793): the special-operations bar takes the line and starts
##   the action; without the bar the panel starts it itself. Esc, Backspace or B, and a right-click
##   on the plot, leave a tool.
## - Scale: the eight fixed steps (no continuous zoom, rule 16) with closer and farther buttons and
##   the width the plot covers, "12 km across". The wheel, a pinch, Page Up and Down and the D-pad
##   step it too. "Centre on own ship" brings back the follow the plot drops when panned.
## - Layers: one checkbox per plot layer (rule 14: sparse by default, density by player layers).
## - Legend: what each symbol and shape drawn now means (rule 6), refreshed when the picture's
##   legend changes. One click away and closed by default, so the picture stays sparse (rule 14).
## - Hover list: the labels declutter hid near the pointer or the gamepad cursor, as a list beside
##   it (rule 14: hover shows hidden).
## - Gamepad: the HUD shell feeds the right stick to `move_cursor`; A and X at the cursor are the
##   plot's own (it reports them as clicks). `zoom(steps)` steps about the cursor or the centre.
## - `centre_on_entry(entry)` centres on what a `LogEntryDict` is about (rule 15).
## In a replay the plot draws the replay and sends no orders.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Legend := preload("res://ui/widgets/legend_row.gd")

## Layer checkboxes in display order: (bit, text key).
const LAYERS: Array[Array] = [
	[1, &"hud_plot.layer.tracks"], [2, &"hud_plot.layer.uncertainty"], [4, &"hud_plot.layer.envelopes"],
	[8, &"hud_plot.layer.emitters"], [16, &"hud_plot.layer.route"], [32, &"hud_plot.layer.rings"],
	[64, &"hud_plot.layer.graticule"], [128, &"hud_plot.layer.history"], [256, &"hud_plot.layer.land"],
	[512, &"hud_plot.layer.objectives"], [1024, &"hud_plot.layer.mines"], [2048, &"hud_plot.layer.allies"],
	[4096, &"hud_plot.layer.labels"],
]
## The tool column keeps this far from the band's ends and the plot's left edge.
const BAND_GAP := 8.0
const SIDE_INSET := 16.0
## The plot's scale steps in metres per pixel at 100% UI scale, mirrored from
## `plot::SCALE_STEPS_M_PER_PX` for the readout before the first `scale_changed`.
const STEP_MPP: Array[float] = [1.0, 2.5, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0]
const STEPS := 8
## Hover list: recomputed when the pointer moved this far.
const HOVER_MOVE_PX := 4.0
const HOVER_OFFSET := Vector2(20.0, 20.0)
const HOVER_MAX := 8

@onready var _plot: Control = $Plot
@onready var _side: HBoxContainer = $Side
## The tool column scrolls inside the band the HUD shell leaves it (`set_tool_band`).
@onready var _tools_margin: ScrollContainer = $Side/Tools/Margin
@onready var _tools: VBoxContainer = $Side/Tools/Margin/Column
@onready var _orders: Button = $Side/Tools/Margin/Column/Orders
@onready var _measure: Button = $Side/Tools/Margin/Column/Measure
@onready var _mine_line: Button = $Side/Tools/Margin/Column/MineLine
@onready var _scale_row: HBoxContainer = $Side/Tools/Margin/Column/Scale
@onready var _zoom_out: Button = $Side/Tools/Margin/Column/Scale/ZoomOut
@onready var _across: Label = $Side/Tools/Margin/Column/Scale/Across
@onready var _zoom_in: Button = $Side/Tools/Margin/Column/Scale/ZoomIn
@onready var _follow: Button = $Side/Tools/Margin/Column/Follow
@onready var _layers_button: Button = $Side/Tools/Margin/Column/Layers
@onready var _legend_button: Button = $Side/Tools/Margin/Column/Legend
@onready var _pop: VBoxContainer = $Side/Pop
@onready var _layers_panel: PanelContainer = $Side/Pop/LayersPanel
@onready var _layers_margin: MarginContainer = $Side/Pop/LayersPanel/Margin
@onready var _layers_column: VBoxContainer = $Side/Pop/LayersPanel/Margin/Column
@onready var _layers_grid: GridContainer = $Side/Pop/LayersPanel/Margin/Column/Grid
@onready var _legend_panel: PanelContainer = $Side/Pop/LegendPanel
@onready var _legend_margin: MarginContainer = $Side/Pop/LegendPanel/Margin
@onready var _legend_column: VBoxContainer = $Side/Pop/LegendPanel/Margin/Column
@onready var _legend_rows: VBoxContainer = $Side/Pop/LegendPanel/Margin/Column/Rows
@onready var _legend_empty: Label = $Side/Pop/LegendPanel/Margin/Column/Empty
@onready var _hover: PanelContainer = $Hover
@onready var _hover_margin: MarginContainer = $Hover/Margin
@onready var _hover_column: VBoxContainer = $Hover/Margin/Column

var _actions: Node = null
var _tool: StringName = &"orders"
## True while the mine line is this panel's own (no special-operations bar took it).
var _own_line := false
var _step := 4
var _mpp := 25.0
var _layer_boxes: Dictionary[int, CheckBox] = {}
var _legend: Array = []
var _hover_at := Vector2(-1.0e9, -1.0e9)
var _hover_ids := PackedInt32Array()
var _replay := false
var _tool_group := ButtonGroup.new()


## The band (plot-local y, top and bottom) between the alert strip and the helm cluster that the
## tool column keeps to; (-1, -1) centres it on the plot's height.
var _band := Vector2(-1.0, -1.0)
## The HUD shell's clusters over the plot (global rectangles; `set_blocked`).
var _blocked: Array[Rect2] = []

func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	Ui.set_margins(_layers_margin, 2)
	Ui.set_margins(_legend_margin, 2)
	Ui.set_margins(_hover_margin, 1)
	Ui.set_separation(_side, 2)
	Ui.set_separation(_tools, 1)
	Ui.set_separation(_scale_row, 1)
	Ui.set_separation(_pop, 2)
	Ui.set_separation(_layers_column, 1)
	Ui.set_separation(_layers_grid, 1)
	Ui.set_separation(_legend_column, 1)
	Ui.set_separation(_legend_rows, 1)
	Ui.set_separation(_hover_column, 0)
	_across.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_plot.accessibility_name = tr(&"hud_plot.access.plot")
	_side.accessibility_name = tr(&"hud_plot.access.tools")
	for b: Button in [_orders, _measure, _mine_line]:
		b.button_group = _tool_group
		b.focus_mode = Control.FOCUS_ALL
	_orders.tooltip_text = tr(&"hud_plot.tool.orders_tooltip")
	_measure.tooltip_text = tr(&"hud_plot.tool.measure_tooltip")
	_mine_line.tooltip_text = tr(&"hud_plot.tool.mine_line_tooltip")
	_orders.pressed.connect(func() -> void: set_tool(&"orders"))
	_measure.pressed.connect(func() -> void: set_tool(&"measure"))
	_mine_line.pressed.connect(func() -> void: set_tool(&"line"))
	_zoom_in.icon = Ui.icon(&"zoom_in")
	_zoom_out.icon = Ui.icon(&"zoom_out")
	_zoom_in.accessibility_name = tr(&"hud_plot.zoom_in")
	_zoom_out.accessibility_name = tr(&"hud_plot.zoom_out")
	_zoom_in.tooltip_text = tr(&"hud_plot.zoom_in_tooltip")
	_zoom_out.tooltip_text = tr(&"hud_plot.zoom_out_tooltip")
	_zoom_in.pressed.connect(func() -> void: zoom(1))
	_zoom_out.pressed.connect(func() -> void: zoom(-1))
	_follow.tooltip_text = tr(&"hud_plot.follow_tooltip")
	_follow.pressed.connect(func() -> void: _plot.call(&"set_follow_own", true))
	_layers_button.toggled.connect(func(on: bool) -> void: _layers_panel.visible = on)
	_legend_button.toggled.connect(func(on: bool) -> void:
		_legend_panel.visible = on
		if on:
			_refresh_legend())
	_layers_panel.accessibility_name = tr(&"hud_plot.layers_title")
	_legend_panel.accessibility_name = tr(&"hud_plot.legend_title")
	_build_layers()
	Kit.chain_vertical([_orders, _measure, _mine_line, _zoom_out, _follow, _layers_button, _legend_button])
	Kit.chain_horizontal([_zoom_out, _zoom_in])
	_plot.connect(&"track_clicked", _on_track_clicked)
	_plot.connect(&"sea_clicked", _on_sea_clicked)
	_plot.connect(&"scale_changed", _on_scale_changed)
	_plot.connect(&"line_drawn", _on_line_drawn)
	_plot.connect(&"hover_changed", _on_hover_changed)
	_plot.gui_input.connect(_on_plot_input)
	_plot.mouse_exited.connect(_hide_hover)
	_plot.resized.connect(_render_scale)
	resized.connect(_place_side)
	_side.minimum_size_changed.connect(_place_side, CONNECT_DEFERRED)
	_place_side.call_deferred()
	var hud := Kit.autoload(&"Hud")
	if hud != null:
		for sig: StringName in [&"tracks_changed", &"ship_static_changed"]:
			if hud.has_signal(sig):
				hud.connect(sig, _refresh_legend)
		for sig: StringName in [&"actions_changed", &"ship_static_changed"]:
			if hud.has_signal(sig):
				hud.connect(sig, _refresh_mine_tool)
	var session := Kit.autoload(&"Session")
	if session != null and session.has_signal(&"state_changed"):
		session.connect(&"state_changed", func(_s: int) -> void: _apply_mode())
	_apply_mode()
	_step = int(_plot.call(&"scale_step"))
	_mpp = _step_mpp(_step)
	_render_scale()
	_refresh_mine_tool()
	_refresh_legend()
	visibility_changed.connect(_on_visibility)


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_render_scale()
		_legend = []
		_refresh_legend()


## The tool column's focusable controls, top first (the HUD shell links them to its neighbours).
func focus_targets() -> Array[Control]:
	var out: Array[Control] = []
	for c: Control in [_orders, _measure, _mine_line, _zoom_out, _zoom_in, _follow, _layers_button, _legend_button]:
		if c.is_visible_in_tree() and not (c is BaseButton and (c as BaseButton).disabled):
			out.append(c)
	return out


## The BpPlotView this panel hosts (the HUD shell hands it to the special-operations bar).
func plot() -> Control:
	return _plot


## The special-operations bar that takes mine lines (`begin_line`, `cancel_pick`).
func set_actions(bar: Node) -> void:
	_actions = bar


func _on_visibility() -> void:
	if not visible:
		_hide_hover()
	else:
		_refresh_legend()


func _apply_mode() -> void:
	var info := Kit.as_dict(Kit.call_on(&"Session", &"info"))
	_replay = Kit.get_int(info, "kind", -1) == Bp.KIND_REPLAY or Kit.get_int(info, "state", 0) == Bp.SESSION_REPLAY
	_plot.call(&"set_mode", &"replay" if _replay else &"sortie")
	_refresh_mine_tool()


# --- tools ---------------------------------------------------------------------------------------

## `&"orders"`, `&"measure"` or `&"line"` (the mine line).
func set_tool(tool: StringName) -> void:
	if tool == &"line" and not _mine_line.visible:
		tool = &"orders"
	if tool == _tool and tool != &"line":
		_sync_tool_buttons()
		return
	if _tool == &"line" and tool != &"line":
		_end_line()
	_tool = tool
	match tool:
		&"measure":
			_plot.call(&"set_tool", &"measure")
		&"line":
			if _actions != null and is_instance_valid(_actions) and _actions.has_method(&"begin_line"):
				_own_line = false
				_actions.call(&"begin_line")
			else:
				_own_line = true
				_plot.call(&"set_tool", &"line")
		_:
			_tool = &"orders"
			_plot.call(&"set_tool", &"orders")
	_sync_tool_buttons()


func tool() -> StringName:
	return _tool


## Leave the current tool for orders. True when there was one to leave.
func cancel_tool() -> bool:
	if _tool == &"orders":
		return false
	set_tool(&"orders")
	return true


func _end_line() -> void:
	if not _own_line and _actions != null and is_instance_valid(_actions) and _actions.has_method(&"cancel_pick"):
		_actions.call(&"cancel_pick")
	_own_line = false


func _sync_tool_buttons() -> void:
	_orders.set_pressed_no_signal(_tool == &"orders")
	_measure.set_pressed_no_signal(_tool == &"measure")
	_mine_line.set_pressed_no_signal(_tool == &"line")


func _refresh_mine_tool() -> void:
	var can := false
	if not _replay:
		for a: Dictionary in Kit.dicts(Kit.call_on(&"Hud", &"available_actions")):
			if Kit.get_int(a, "action", -1) == Bp.STATION_LAY_MINES and Kit.get_bool(a, "enabled"):
				can = true
	_mine_line.visible = can
	if not can and _tool == &"line":
		set_tool(&"orders")


func _on_line_drawn(a: Vector2, b: Vector2) -> void:
	if _own_line and a.is_finite() and b.is_finite() and a != b:
		Kit.call_on(&"Orders", &"start_action", [Bp.STATION_LAY_MINES, {"kind": &"line", "a": a, "b": b}])
	_own_line = false
	_tool = &"orders"
	_plot.call(&"set_tool", &"orders")
	_sync_tool_buttons()


# --- orders --------------------------------------------------------------------------------------

func _on_track_clicked(track: int, button: int) -> void:
	if track <= 0 or _tool != &"orders":
		return
	Kit.call_on(&"Hud", &"select_track", [track])
	if button == MOUSE_BUTTON_LEFT and not _replay:
		Kit.call_on(&"Orders", &"designate", [track])


func _on_sea_clicked(map: Vector2, button: int) -> void:
	if not map.is_finite() or _tool != &"orders":
		return
	if button == MOUSE_BUTTON_RIGHT and not _replay:
		Kit.call_on(&"Orders", &"set_course", [PackedVector2Array([map]), _course_speed(), Input.is_key_pressed(KEY_SHIFT)])
	elif button == MOUSE_BUTTON_LEFT:
		Kit.call_on(&"Hud", &"select_track", [Bp.NONE])


## Centre on what a log entry is about: its track while the plot holds it, else its position.
func centre_on_entry(entry: Dictionary) -> bool:
	var track := Kit.get_int(entry, "track", Bp.NONE)
	if track > 0 and bool(_plot.call(&"centre_on_track", track)):
		return true
	var pos: Variant = entry.get("pos")
	if pos is Vector2 and (pos as Vector2).is_finite():
		_plot.call(&"centre_on", pos)
		return true
	return false


# --- scale and cursor ----------------------------------------------------------------------------

## Step the scale (positive = closer) about the gamepad cursor when it shows, else the centre.
func zoom(steps: int) -> void:
	var at := _plot.size * 0.5
	var cursor := Kit.as_dict(_plot.call(&"plot_cursor"))
	var local: Variant = cursor.get("local")
	if local is Vector2 and (local as Vector2).is_finite() and Ui.using_gamepad:
		at = local
	_plot.call(&"zoom_step", steps, at)


## Move the gamepad plot cursor by a delta in canvas pixels.
func move_cursor(delta: Vector2) -> void:
	if not is_visible_in_tree() or not delta.is_finite() or delta == Vector2.ZERO:
		return
	_plot.call(&"move_plot_cursor", delta)
	var c := Kit.as_dict(_plot.call(&"plot_cursor"))
	var local: Variant = c.get("local")
	if local is Vector2:
		_update_hover(local)


func _on_scale_changed(step: int, metres_per_px: float) -> void:
	_step = step
	_mpp = metres_per_px if metres_per_px > 0.0 and is_finite(metres_per_px) else _step_mpp(step)
	_render_scale()


func _render_scale() -> void:
	if not is_node_ready():
		return
	var across := _mpp * _plot.size.x
	_across.text = tr(&"hud_plot.scale.across").format([String(Kit.call_on(&"Tx", &"range", [across])) if Kit.autoload(&"Tx") != null else str(roundi(across))])
	_across.tooltip_text = tr(&"hud_plot.scale.tooltip").format([_num(_step + 1), _num(STEPS)])
	_zoom_in.disabled = _step <= 0
	_zoom_out.disabled = _step >= STEPS - 1


## Metres per pixel of a step before the plot reports one.
static func _step_mpp(step: int) -> float:
	return STEP_MPP[clampi(step, 0, STEP_MPP.size() - 1)]


# --- layers and legend ---------------------------------------------------------------------------

func _build_layers() -> void:
	var mask := int(_plot.call(&"layers"))
	for entry: Array in LAYERS:
		var bit: int = entry[0]
		var box := CheckBox.new()
		box.text = entry[1]
		box.button_pressed = (mask & bit) != 0
		box.custom_minimum_size = Vector2(0.0, 40.0)
		box.focus_mode = Control.FOCUS_ALL
		box.toggled.connect(_on_layer_toggled.bind(bit))
		_layers_grid.add_child(box)
		_layer_boxes[bit] = box


func _on_layer_toggled(on: bool, bit: int) -> void:
	var mask := int(_plot.call(&"layers"))
	mask = (mask | bit) if on else (mask & ~bit)
	_plot.call(&"set_layers", mask)
	_refresh_legend.call_deferred()


func _refresh_legend() -> void:
	if not is_node_ready() or not _legend_panel.visible:
		return
	var rows := Kit.dicts(_plot.call(&"legend"))
	if rows == _legend:
		return
	_legend = rows
	Kit.clear(_legend_rows)
	for r: Dictionary in rows:
		var row := _Legend.new()
		_legend_rows.add_child(row)
		var stem := StringName(Kit.get_str(r, "symbol"))
		row.call(&"show_icon", stem, StringName(Kit.get_str(r, "text")), _legend_tint(stem))
	_legend_empty.visible = rows.is_empty()


## Legend swatches take the tint the plot draws them in: stance frames their stance, the rest the
## reference ink.
static func _legend_tint(stem: StringName) -> StringName:
	var s := String(stem)
	for st: String in ["friend", "neutral", "suspect", "hostile"]:
		if s.begins_with("stance_" + st):
			return StringName("stance." + st)
	if s == "select_bracket":
		return &"select"
	return &"text.primary"


# --- hover list ----------------------------------------------------------------------------------

func _on_plot_input(event: InputEvent) -> void:
	if event is InputEventMouseMotion:
		var at := (event as InputEventMouseMotion).position
		if at.distance_to(_hover_at) >= HOVER_MOVE_PX:
			_update_hover(at)


func _on_hover_changed(_track: int) -> void:
	_update_hover(_hover_at if _hover_at.x > -1.0e8 else _plot.size * 0.5)


func _update_hover(at: Vector2) -> void:
	_hover_at = at
	var ids: PackedInt32Array = _plot.call(&"hidden_labels_at", at)
	if ids == _hover_ids and _hover.visible == not ids.is_empty():
		_place_hover()
		return
	_hover_ids = ids
	Kit.clear(_hover_column)
	if ids.is_empty():
		_hover.visible = false
		return
	var shown := 0
	for id: int in ids:
		if shown >= HOVER_MAX:
			break
		var t: Variant = Kit.call_on(&"Hud", &"track", [id])
		if not t is Dictionary:
			continue
		var d := t as Dictionary
		var text := Kit.get_str(d, "name")
		var range_m := Kit.get_float(d, "range_m", -1.0)
		if range_m >= 0.0:
			text = tr(&"hud_plot.hover.line").format([text, String(Kit.call_on(&"Tx", &"range", [range_m]))])
		_hover_column.add_child(_hover_line(text))
		shown += 1
	if ids.size() > shown:
		_hover_column.add_child(_hover_line(tr(&"hud_plot.hover.more").format([_num(ids.size() - shown)])))
	_hover.visible = shown > 0
	_hover.size = _hover.get_combined_minimum_size()
	_place_hover()


## One line of the hover list: no wrapping, so the list is as wide as its longest line.
static func _hover_line(text: String) -> Label:
	var l := Kit.label(text, &"Dense", true)
	l.autowrap_mode = TextServer.AUTOWRAP_OFF
	l.mouse_filter = Control.MOUSE_FILTER_IGNORE
	return l


func _place_hover() -> void:
	if not _hover.visible:
		return
	var s := _hover.get_combined_minimum_size()
	var p := _hover_at + HOVER_OFFSET
	if p.x + s.x > size.x:
		p.x = _hover_at.x - HOVER_OFFSET.x - s.x
	if p.y + s.y > size.y:
		p.y = _hover_at.y - HOVER_OFFSET.y - s.y
	_hover.position = p.clamp(Vector2.ZERO, (size - s).max(Vector2.ZERO))


func _hide_hover() -> void:
	_hover.visible = false
	_hover_ids = PackedInt32Array()


static func _num(n: int) -> String:
	return TranslationServer.format_number(str(n), TranslationServer.get_locale())


## The speed for a course order: the sortie HUD's rule (a stopped ship gets half its top speed, so
## the course is sailed), or -1 to keep the ordered speed when no HUD is above this table.
func _course_speed() -> float:
	var n: Node = get_parent()
	while n != null:
		if n.has_method(&"course_speed"):
			return float(n.call(&"course_speed"))
		n = n.get_parent()
	return -1.0


## Keep the tool column (and the layers and legend popovers beside it) inside the band from `top`
## to `bottom` (plot-local y): the alert strip sits above it and the helm cluster below, both
## centred over the plot. Past the band's height the tools scroll (floor 2).
func set_tool_band(top: float, bottom: float) -> void:
	var band := Vector2(top, bottom)
	if band.is_equal_approx(_band):
		return
	_band = band
	_place_side()


func _place_side() -> void:
	if not is_node_ready():
		return
	var top := _band.x if _band.x >= 0.0 else 0.0
	var bottom := _band.y if _band.y > top else size.y
	top += BAND_GAP
	bottom -= BAND_GAP
	var room := maxf(bottom - top, 0.0)
	var natural := (_tools_margin.get_child(0) as Control).get_combined_minimum_size().y
	var chrome := float(_tools_margin.get_parent().get_combined_minimum_size().y) - _tools_margin.get_combined_minimum_size().y
	var want := ceilf(clampf(room - chrome, 0.0, natural))
	if not is_equal_approx(_tools_margin.custom_minimum_size.y, want):
		_tools_margin.custom_minimum_size = Vector2(0.0, want)
	var side_min := _side.get_combined_minimum_size()
	var y := top + maxf(0.0, (room - side_min.y) * 0.5)
	_side.set_anchors_preset(Control.PRESET_TOP_LEFT)
	_side.position = Vector2(SIDE_INSET, floorf(y))
	_side.size = side_min
	# The graticule's edge labels keep clear of the alert strip, the tool column (not its popovers,
	# so they hold still when one opens) and the helm.
	if _plot.has_method(&"set_clear_margins"):
		var tools_w := (_tools_margin.get_parent() as Control).get_combined_minimum_size().x
		var left := SIDE_INSET + tools_w + BAND_GAP if _side.visible else 0.0
		var clear_top := _band.x if _band.x >= 0.0 else 0.0
		var clear_bottom := size.y - _band.y if _band.y > clear_top else 0.0
		_plot.call(&"set_clear_margins", left, clear_top, 0.0, maxf(clear_bottom, 0.0))
	# After the column sorts its popovers into place.
	_send_blocked.call_deferred()


## The HUD shell's clusters over the plot (global rectangles): the plot's labels give way to them
## and to this table's own tool column and popovers.
func set_blocked(rects: Array[Rect2]) -> void:
	if rects == _blocked:
		return
	_blocked = rects.duplicate()
	_send_blocked()


func _send_blocked() -> void:
	if not is_node_ready() or not _plot.has_method(&"set_blocked"):
		return
	var to_local := _plot.get_global_transform().affine_inverse()
	var out: Array[Rect2] = []
	for r: Rect2 in _blocked:
		out.append(to_local * r)
	if _side.visible:
		for c: Control in _side.get_children():
			if c.visible and c.size.x >= 1.0:
				out.append(to_local * c.get_global_rect())
	_plot.call(&"set_blocked", out)
