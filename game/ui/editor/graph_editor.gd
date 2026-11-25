extends Control
## L3 graph editor on GraphEdit; the model owns connections.
## DESIGN.md L331-349, L1736. Unit: ui-editor-graph.
## Reads and calls: Editor.graph_nodes, Editor.graph_edges, Editor.node_library, Editor.graph_connect, Editor.graph_pin, Editor.graph_replace_stage, Editor.graph_live_values.
##
## The dry dock's Control graph view (the Ship / Control graph / Scripts switch above the stage). It shows
## the ship's compiled control graph one officer at a time (or all of them): every stage of every officer
## is a node with typed ports, and the rules table's rules appear as the nodes they compile to (L336).
##
## - Stock chrome is off (L1736): no menu, minimap, zoom or arrange buttons, a themed grid. The wheel pans
##   and Ctrl or Cmd with the wheel zooms, as GraphEdit does by default; "Show all" frames the graph.
## - The model owns connections. A drag between ports, a drag off an input, Delete and the inspector all
##   call the model, and the canvas is redrawn from `Editor.graph_edges()` afterwards, so it never shows a
##   connection the model refused. A refusal says why on the bar.
## - Generated nodes have text.secondary frames (L320). A node the player edits is pinned and shows the
##   pin glyph; a pinned node that lost its input shows the caution glyph and says so in the inspector.
## - The inspector on the right is the non-drag way to do everything (L1739 item 8): pick the source of
##   each input, set parameters, pin or unpin, replace an officer stage with an editable subgraph or return
##   to the stock stage, delete a node of the player's own. With nothing selected it lists the graph's
##   problems and the legend.
## - The node library (Add node, or a right click on empty canvas) lists sources, maths and logic, officer
##   stages, sinks and, with L4 in the build, scripts, with a search field and each node's description.
## - During a splash the view subscribes to the ports it shows (`Orders.query_scope`) and prints their
##   latest values on the edges (`Editor.graph_live_values`, already formatted), redrawn when
##   `Hud.scope_value` reports a change, at most ten times a second.
## Layout: generated nodes keep the generator's columns and order, laid out here from their real sizes so
## nothing overlaps; a node the player drags or adds keeps the place it was given (`Editor.graph_move_node`).

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const Inspector := preload("res://ui/editor/part_inspector.gd")

## Officer stages in `Bp.STAGE_*` order, as their type ids name them.
const STAGES: Array[String] = ["helm_autopilot", "station_keeping", "evasion", "track_fusion", "threat_ranking",
	"mount_assignment", "ballistic_solver", "pn_guidance", "load_shedder", "damage_control_planner", "emission_controller"]
const OFFICERS: Array[int] = [Bp.OFFICER_HELM, Bp.OFFICER_ENGINEER, Bp.OFFICER_SENSORS, Bp.OFFICER_GUNNERY, Bp.OFFICER_DAMAGE_CONTROL]
const PORT_NAMES: Array[String] = ["bool", "scalar", "angle", "vec3", "text", "stance", "emcon", "track", "track_list",
	"detection_list", "part_state", "orders"]
const GROUP_KEYS: Array[StringName] = [&"graph.group.sources", &"graph.group.math", &"graph.group.stages", &"graph.group.sinks", &"graph.group.scripts"]
## Ids at or above this are the player's own nodes (`content::graph::USER_NODE_BASE`).
const USER_NODE_BASE := 1 << 24
## Slot type of a port that takes any value (the Select node's choices and its output).
const PORT_ANY := 100
const COLUMN_GAP := 80.0
const ROW_GAP := 24.0
const NODE_MIN_W := 220.0
const FIELD_H := 40.0
## Most ports scoped at once during a splash (the model keeps 512 values).
const MAX_SCOPED := 96
## "Show all" zooms out no further than this: Dense text stays at 18 px or more.
const MIN_FRAME_ZOOM := 0.9
## Inputs fed by another officer's node get a stub this long, and its name when there is room.
const STUB_PX := 28.0
## Live values redraw at most this often.
const LIVE_REDRAW_S := 0.1

@onready var _officer_pick: OptionButton = $Column/Bar/Margin/Row/Officer
@onready var _add: Button = $Column/Bar/Margin/Row/Add
@onready var _frame_all: Button = $Column/Bar/Margin/Row/Frame
@onready var _status: Label = $Column/Bar/Margin/Row/Status
@onready var _canvas: GraphEdit = $Column/Body/CanvasBox/Canvas
@onready var _live_layer: Control = $Column/Body/CanvasBox/Live
@onready var _empty: CenterContainer = $Column/Body/CanvasBox/Empty
@onready var _empty_text: Label = $Column/Body/CanvasBox/Empty/Text
@onready var _inspector: PanelContainer = $Column/Body/Inspector
@onready var _inspect: VBoxContainer = $Column/Body/Inspector/Scroll/Margin/Column
@onready var _library: PanelContainer = $Column/Body/CanvasBox/Library
@onready var _library_close: Button = $Column/Body/CanvasBox/Library/Margin/Column/Head/Close
@onready var _search: LineEdit = $Column/Body/CanvasBox/Library/Margin/Column/Search
@onready var _tree: Tree = $Column/Body/CanvasBox/Library/Margin/Column/Tree
@onready var _description: Label = $Column/Body/CanvasBox/Library/Margin/Column/Description
@onready var _place: Button = $Column/Body/CanvasBox/Library/Margin/Column/Place

## `Bp.OFFICER_*` shown, or `Bp.NONE` for every officer.
var _officer := Bp.OFFICER_HELM
var _nodes: Dictionary[int, GraphNode] = {}
var _signatures: Dictionary[int, String] = {}
var _data: Dictionary[int, Dictionary] = {}
var _all: Dictionary[int, Dictionary] = {}
var _edges: Array[Dictionary] = []
var _library_list: Array[Dictionary] = []
var _library_by_type: Dictionary[String, Dictionary] = {}
## Nodes the player moved this session and where to: they keep that place while the model still reports it
## (a newly opened design reports its own layout, which drops the entry).
var _moved: Dictionary[int, Vector2] = {}
var _selected := -1
var _refresh_pending := false
var _stale := true
var _editable := false
var _first_frame := true
var _add_at := Vector2.ZERO
var _add_at_set := false
var _status_text: Variant = &""
var _scoped: Dictionary[String, bool] = {}
var _live: Dictionary = {}
var _live_timer: Timer
var _frame_box: StyleBoxFlat
var _finish_pending := false
var _frame_pending := false
var _focus_pending := -1
var _place_under: Dictionary = {}


func _ready() -> void:
	accessibility_name = tr(&"editor_graph.name")
	Ui.set_margins($Column/Bar/Margin as MarginContainer, 1)
	Ui.set_margins($Column/Body/Inspector/Scroll/Margin as MarginContainer, 2)
	Ui.set_margins($Column/Body/CanvasBox/Library/Margin as MarginContainer, 2)
	Ui.set_separation(_inspect, 2)
	Ui.set_separation($Column/Body/CanvasBox/Library/Margin/Column as BoxContainer, 2)
	_library.accessibility_name = tr(&"editor_graph.library.title")
	_library_close.icon = Ui.icon(&"close")
	_library_close.accessibility_name = tr(&"editor_graph.library.close")
	_library_close.tooltip_text = tr(&"editor_graph.library.close")
	_library_close.pressed.connect(_close_library)
	_library.gui_input.connect(_on_library_input)
	_search.gui_input.connect(_on_library_input)
	_tree.gui_input.connect(_on_library_input)
	var row := $Column/Bar/Margin/Row as HFlowContainer
	row.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	row.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	($Column/Body as BoxContainer).add_theme_constant_override(&"separation", 0)
	($Column as BoxContainer).add_theme_constant_override(&"separation", 0)
	_inspector.accessibility_name = tr(&"editor_graph.inspector.name")
	_setup_canvas()
	_fill_officers()
	_officer_pick.item_selected.connect(func(i: int) -> void: set_officer(_officer_pick.get_item_id(i)))
	_officer_pick.tooltip_text = tr(&"editor_graph.officer_tip")
	_officer_pick.accessibility_name = tr(&"editor_graph.officer")
	_add.tooltip_text = tr(&"editor_graph.add_tip")
	_add.icon = Ui.icon(&"add")
	_add.pressed.connect(func() -> void:
		_add_at_set = false
		if _library.visible:
			_close_library()
		else:
			open_library())
	_frame_all.tooltip_text = tr(&"editor_graph.frame_tip")
	_frame_all.pressed.connect(_frame_view)
	_status.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_status.visible = false
	_search.text_changed.connect(func(_t: String) -> void: _fill_tree())
	_search.text_submitted.connect(func(_t: String) -> void: _place_selected())
	_search.accessibility_name = tr(&"editor_graph.library.search")
	_tree.item_selected.connect(_on_tree_selected)
	_tree.item_activated.connect(_place_selected)
	_tree.accessibility_name = tr(&"editor_graph.library.title")
	_place.pressed.connect(_place_selected)
	_description.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_live_layer.draw.connect(_draw_live)
	_live_timer = Timer.new()
	_live_timer.one_shot = true
	_live_timer.wait_time = LIVE_REDRAW_S
	_live_timer.timeout.connect(_pull_live)
	add_child(_live_timer)
	_frame_box = StyleBoxFlat.new()
	_frame_box.draw_center = false
	_frame_box.set_corner_radius_all(int(Ui.token("radius.tile", 4)))
	_frame_box.set_border_width_all(int(Ui.token("stroke.rule", 1)))
	_frame_box.corner_detail = 4
	var e := _editor()
	if e != null:
		for sig: StringName in [&"graph_changed", &"control_changed", &"gauges_changed", &"undo_changed"]:
			if e.has_signal(sig):
				e.connect(sig, _queue_refresh)
		if e.has_signal(&"design_changed"):
			e.connect(&"design_changed", func(_rev: int) -> void: _queue_refresh())
		if e.has_signal(&"splash_ended"):
			e.connect(&"splash_ended", func(_card: Dictionary) -> void: _unscope_all())
	var hud := Kit.autoload(&"Hud")
	if hud != null and hud.has_signal(&"scope_value"):
		hud.connect(&"scope_value", _on_scope_value)
	Ui.changed.connect(func(what: StringName) -> void:
		if what == &"palette":
			_restyle_nodes())
	visibility_changed.connect(_on_visibility)
	_refresh_inspector()


func _exit_tree() -> void:
	_unscope_all()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_fill_officers()
		_signatures.clear()
		_queue_refresh()


func _editor() -> Node:
	return Kit.autoload(&"Editor")


func _word(kind: StringName, value: int) -> String:
	var tx := Kit.autoload(&"Tx")
	return String(tx.call(&"word", kind, value)) if tx != null else str(value)


func _setup_canvas() -> void:
	_canvas.show_menu = false
	_canvas.show_zoom_label = false
	_canvas.show_zoom_buttons = false
	_canvas.show_grid_buttons = false
	_canvas.show_minimap_button = false
	_canvas.show_arrange_button = false
	_canvas.minimap_enabled = false
	_canvas.show_grid = true
	_canvas.right_disconnects = false
	_canvas.zoom_min = 0.3
	_canvas.zoom_max = 1.5
	_canvas.connection_lines_antialiased = true
	_canvas.accessibility_name = tr(&"editor_graph.canvas")
	# Port types: the model's compatibility (scalars, angles and booleans mix; Select takes anything).
	var names: Dictionary = {}
	for t in PORT_NAMES.size():
		names[t] = tr(StringName("editor_graph.port." + PORT_NAMES[t]))
		_canvas.add_valid_left_disconnect_type(t)
		_canvas.add_valid_connection_type(t, PORT_ANY)
		_canvas.add_valid_connection_type(PORT_ANY, t)
	names[PORT_ANY] = tr(&"editor_graph.port.any")
	_canvas.add_valid_left_disconnect_type(PORT_ANY)
	for pair: Array in [[Bp.PORT_SCALAR, Bp.PORT_ANGLE], [Bp.PORT_ANGLE, Bp.PORT_SCALAR], [Bp.PORT_BOOL, Bp.PORT_SCALAR], [Bp.PORT_SCALAR, Bp.PORT_BOOL]]:
		_canvas.add_valid_connection_type(int(pair[0]), int(pair[1]))
	_canvas.type_names = names
	_canvas.connection_request.connect(_on_connection_request)
	_canvas.disconnection_request.connect(_on_disconnection_request)
	_canvas.node_selected.connect(_on_node_selected)
	_canvas.node_deselected.connect(_on_node_deselected)
	_canvas.delete_nodes_request.connect(_on_delete_request)
	_canvas.end_node_move.connect(_on_end_move)
	_canvas.popup_request.connect(_on_popup_request)
	_canvas.scroll_offset_changed.connect(func(_o: Vector2) -> void: _live_layer.queue_redraw())
	_canvas.connection_drag_ended.connect(_queue_refresh)


func _fill_officers() -> void:
	var keep := _officer
	_officer_pick.clear()
	_officer_pick.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	for o in OFFICERS:
		_officer_pick.add_item(_word(&"officer", o), o)
	_officer_pick.add_item(tr(&"editor_graph.all_officers"), 99)
	_officer_pick.select(_officer_pick.get_item_index(keep if keep >= 0 else 99))


# --- public -----------------------------------------------------------------------------------------

## Show one officer's part of the graph (`Bp.OFFICER_*`), or every officer (`Bp.NONE`).
func set_officer(officer: int) -> void:
	_officer = officer if officer >= 0 and officer < OFFICERS.size() else -1
	var idx := _officer_pick.get_item_index(_officer if _officer >= 0 else 99)
	if idx >= 0:
		_officer_pick.select(idx)
	_selected = -1
	_frame_pending = true
	_refresh()


## Select a node and bring it into view (Why? links open the node responsible, L353). Runs once the
## canvas is laid out.
func focus_node(node: int) -> void:
	var e := _editor()
	if e == null or not bool(e.call(&"is_open")):
		return
	if _all.is_empty() or _stale or _refresh_pending or _finish_pending:
		_focus_pending = node
		if not _refresh_pending and not _finish_pending:
			_refresh()
		return
	if not _all.has(node):
		return
	var officer := Kit.get_int(_all[node], "officer", -1)
	if officer >= 0 and _officer >= 0 and officer != _officer:
		_focus_pending = node
		set_officer(officer)
		return
	if not _nodes.has(node):
		return
	var g := _nodes[node]
	_canvas.set_selected(g)
	_selected = node
	_refresh_inspector()
	_canvas.scroll_offset = (g.position_offset + g.size * 0.5) * _canvas.zoom - _canvas.size * 0.5
	Ui.focus(g)


## Open the node library, docked over the left of the canvas.
func open_library() -> void:
	_load_library()
	_search.text = ""
	_fill_tree()
	_library.visible = true
	_add.set_pressed_no_signal(true)
	Ui.focus(_search)


func _close_library() -> void:
	_library.visible = false
	_add.set_pressed_no_signal(false)
	_add_at_set = false
	Ui.focus(_add)


## Esc closes the library wherever its focus is.
func _on_library_input(event: InputEvent) -> void:
	if event.is_action_pressed(&"ui_cancel") and _library.visible:
		_close_library()
		get_viewport().set_input_as_handled()


# --- refresh ----------------------------------------------------------------------------------------

func _on_visibility() -> void:
	if not is_visible_in_tree():
		_unscope_all()
		return
	if _stale:
		if _first_frame:
			_first_frame = false
			_frame_pending = true
		_refresh()


func _queue_refresh() -> void:
	if not is_visible_in_tree():
		_stale = true
		return
	if _refresh_pending:
		return
	_refresh_pending = true
	_refresh.call_deferred()


func _refresh() -> void:
	_refresh_pending = false
	_stale = false
	if not is_node_ready():
		return
	var e := _editor()
	var open := e != null and bool(e.call(&"is_open"))
	if not open:
		_clear_canvas()
		_empty.visible = true
		_empty_text.text = tr(&"editor_graph.closed")
		_inspector.visible = false
		return
	_inspector.visible = true
	var limits := Kit.as_dict(e.call(&"limits"))
	_editable = Kit.get_int(limits, "max_layer", Bp.LAYER_SCRIPT) >= Bp.LAYER_GRAPH
	_add.disabled = not _editable
	var list := Kit.dicts(e.call(&"graph_nodes", -1))
	_all.clear()
	for n: Dictionary in list:
		_all[Kit.get_int(n, "id", -1)] = n
	_edges = Kit.dicts(e.call(&"graph_edges"))
	var shown: Array[Dictionary] = []
	for n: Dictionary in list:
		var off := Kit.get_int(n, "officer", -1)
		if _officer < 0 or off == _officer or off < 0:
			shown.append(n)
	_empty.visible = shown.is_empty()
	_empty_text.text = tr(&"editor_graph.empty") if list.is_empty() else tr(&"editor_graph.empty_officer").format([_word(&"officer", maxi(_officer, 0))])
	_sync_nodes(shown)
	# GraphNode sorts its rows and counts its ports on a deferred sort: lay out and wire after it.
	_finish_pending = true
	_finish.call_deferred(shown)


func _finish(shown: Array[Dictionary]) -> void:
	_finish_pending = false
	if not is_node_ready():
		return
	_layout_nodes(shown)
	_sync_edges()
	if _selected >= 0 and not _nodes.has(_selected):
		_selected = -1
	elif _selected >= 0 and not _nodes[_selected].selected:
		# A rebuilt node keeps the selection the player made.
		_canvas.set_selected(_nodes[_selected])
	_refresh_inspector()
	_rescope()
	_live_layer.queue_redraw()
	if not _place_under.is_empty():
		_place_fork()
	if _frame_pending:
		_frame_pending = false
		_frame_view()
	if _focus_pending >= 0:
		var node := _focus_pending
		_focus_pending = -1
		focus_node(node)


func _clear_canvas() -> void:
	_canvas.clear_connections()
	for id: int in _nodes:
		_nodes[id].queue_free()
	_nodes.clear()
	_signatures.clear()
	_data.clear()
	_all.clear()
	_edges.clear()
	_moved.clear()
	_selected = -1
	_unscope_all()


# --- nodes ------------------------------------------------------------------------------------------

func _sync_nodes(shown: Array[Dictionary]) -> void:
	var keep: Dictionary[int, bool] = {}
	for n: Dictionary in shown:
		var id := Kit.get_int(n, "id", -1)
		keep[id] = true
		var sig := _signature(n)
		if _nodes.has(id) and _signatures.get(id, "") == sig:
			_data[id] = n
			continue
		if _nodes.has(id):
			var old := _nodes[id]
			_nodes.erase(id)
			_canvas.remove_child(old)
			old.queue_free()
		var g := _make_node(n)
		_canvas.add_child(g)
		_nodes[id] = g
		_signatures[id] = sig
		_data[id] = n
	for id: int in _nodes.keys():
		if not keep.has(id):
			var g := _nodes[id]
			_nodes.erase(id)
			_signatures.erase(id)
			_data.erase(id)
			_canvas.remove_child(g)
			g.queue_free()


func _signature(n: Dictionary) -> String:
	return "%s|%s|%s|%s|%s|%s|%s|%s|%s" % [n.get("type", ""), _Text.resolve(n.get("title", {})), n.get("generated", false),
		n.get("pinned", false), n.get("broken", false), n.get("replaced_stage", -1), n.get("inputs", []), n.get("outputs", []), n.get("params", {})]


## A replaced stage stands for the stock stage and also bounds its subgraph: edges from it read the stage's
## inputs and edges into it drive the stage's outputs. Its rows carry both.
func _is_boundary(n: Dictionary) -> bool:
	return Kit.get_int(n, "replaced_stage", -1) >= 0 and Kit.get_str(n, "type").begins_with("stage.") and Kit.get_bool(n, "generated")


func _make_node(n: Dictionary) -> GraphNode:
	var id := Kit.get_int(n, "id", -1)
	var g := GraphNode.new()
	g.name = str(id)
	g.title = _Text.resolve(n.get("title", {}))
	g.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	g.custom_minimum_size.x = NODE_MIN_W
	g.ignore_invalid_connection_type = false
	g.draggable = true
	g.focus_mode = Control.FOCUS_ALL
	g.accessibility_name = g.title
	g.tooltip_text = _node_tooltip(n)
	var bar := g.get_titlebar_hbox()
	if Kit.get_bool(n, "broken"):
		var warn := TextureRect.new()
		warn.texture = Ui.icon(&"check_caution")
		warn.self_modulate = Ui.colour(&"alert.caution")
		warn.stretch_mode = TextureRect.STRETCH_KEEP_CENTERED
		warn.tooltip_text = tr(&"editor_graph.node.broken")
		bar.add_child(warn)
	if Kit.get_bool(n, "pinned") or id >= USER_NODE_BASE:
		var pin := TextureRect.new()
		pin.texture = Ui.icon(&"pin")
		pin.stretch_mode = TextureRect.STRETCH_KEEP_CENTERED
		pin.tooltip_text = tr(&"editor_graph.node.pinned")
		bar.add_child(pin)
	var ins := _numbered(Kit.dicts(n.get("inputs", [])))
	var outs := _numbered(Kit.dicts(n.get("outputs", [])))
	var any_ports := Kit.get_str(n, "type") == "math.select"
	var colour := Ui.colour(&"text.secondary")
	if _is_boundary(n):
		# Rows: each input with its read-through, then each output with its drive.
		for i in ins.size():
			_add_row(g, ins[i], ins[i], i, &"", &"editor_graph.node.passed")
			var t := Kit.get_int(ins[i], "type")
			g.set_slot(i, true, t, colour, true, t, colour)
		for j in outs.size():
			var r := ins.size() + j
			_add_row(g, outs[j], outs[j], r, &"editor_graph.node.set_by_you", &"")
			var t := Kit.get_int(outs[j], "type")
			g.set_slot(r, true, t, colour, true, t, colour)
	else:
		for i in maxi(ins.size(), outs.size()):
			_add_row(g, ins[i] if i < ins.size() else {}, outs[i] if i < outs.size() else {}, i)
			var lt := PORT_ANY if any_ports and i > 0 else (Kit.get_int(ins[i], "type") if i < ins.size() else 0)
			var rt := PORT_ANY if any_ports else (Kit.get_int(outs[i], "type") if i < outs.size() else 0)
			g.set_slot(i, i < ins.size(), lt, colour, i < outs.size(), rt, colour)
	if ins.is_empty() and outs.is_empty():
		g.add_child(Kit.label("editor_graph.node.no_ports", &"Dense"))
	g.draw.connect(_draw_frame.bind(g, id))
	return g


## One row of ports. `in_fmt` and `out_fmt` reword a replaced stage's read-through and driven ports.
func _add_row(g: GraphNode, inp: Dictionary, out: Dictionary, i: int, in_fmt: StringName = &"", out_fmt: StringName = &"") -> void:
	var row := HBoxContainer.new()
	Ui.set_separation(row, 2)
	var in_text := _port_name(inp) if in_fmt == &"" else tr(in_fmt).format([_port_name(inp)])
	var out_text := _port_name(out) if out_fmt == &"" else tr(out_fmt).format([_port_name(out)])
	var l := Kit.label(in_text, &"Dense", true)
	l.autowrap_mode = TextServer.AUTOWRAP_OFF
	l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	l.mouse_filter = Control.MOUSE_FILTER_PASS
	if not inp.is_empty():
		l.tooltip_text = _port_tip(inp)
	row.add_child(l)
	var r := Kit.label(out_text, &"Dense", true)
	r.autowrap_mode = TextServer.AUTOWRAP_OFF
	r.horizontal_alignment = HORIZONTAL_ALIGNMENT_RIGHT
	r.mouse_filter = Control.MOUSE_FILTER_PASS
	if not out.is_empty():
		r.tooltip_text = _port_tip(out)
	row.add_child(r)
	row.name = "Row%d" % i
	g.add_child(row)


func _port_name(p: Dictionary) -> String:
	if p.is_empty():
		return ""
	var key := StringName(p.get("name", &""))
	if p.has("ordinal"):
		return tr(&"editor_graph.port.numbered").format([tr(key), Kit.number(float(p.ordinal), &"none", 0)])
	return tr(key)


## Ports that share a name (a load shedder's one priority per consumer) are numbered in order.
static func _numbered(ports: Array[Dictionary]) -> Array[Dictionary]:
	var count: Dictionary[StringName, int] = {}
	for p: Dictionary in ports:
		var k := StringName(p.get("name", &""))
		count[k] = count.get(k, 0) + 1
	var seen: Dictionary[StringName, int] = {}
	var out: Array[Dictionary] = []
	for p: Dictionary in ports:
		var k := StringName(p.get("name", &""))
		if count[k] > 1:
			seen[k] = seen.get(k, 0) + 1
			var q := p.duplicate()
			q["ordinal"] = seen[k]
			out.append(q)
		else:
			out.append(p)
	return out


func _port_tip(p: Dictionary) -> String:
	var t := Kit.get_int(p, "type")
	var type_word := tr(StringName("editor_graph.port." + PORT_NAMES[t])) if t >= 0 and t < PORT_NAMES.size() else ""
	return tr(&"editor_graph.port.tip").format([_port_name(p), type_word])


func _node_tooltip(n: Dictionary) -> String:
	var lib := _library_entry(Kit.get_str(n, "type"))
	var desc := tr(StringName(lib.get("description", &""))) if not lib.is_empty() else ""
	return desc


## Generated nodes get text.secondary frames (L320); pinned and player nodes text.primary. The theme's
## selection frame wins while selected.
func _draw_frame(g: GraphNode, id: int) -> void:
	if g.selected or not _data.has(id):
		return
	var n: Dictionary = _data[id]
	var own := Kit.get_bool(n, "pinned") or id >= USER_NODE_BASE
	_frame_box.border_color = Ui.colour(&"text.primary" if own else &"text.secondary")
	g.draw_style_box(_frame_box, Rect2(Vector2.ZERO, g.size))


func _restyle_nodes() -> void:
	_signatures.clear()
	_queue_refresh()


## Columns and order from the generator; sizes from the nodes themselves.
func _layout_nodes(shown: Array[Dictionary]) -> void:
	var columns: Dictionary[float, Array] = {}
	for n: Dictionary in shown:
		var id := Kit.get_int(n, "id", -1)
		if not _nodes.has(id):
			continue
		var g := _nodes[id]
		var pos: Vector2 = n.get("pos", Vector2.ZERO)
		if id >= USER_NODE_BASE:
			g.position_offset = pos
			continue
		if _moved.has(id):
			if _moved[id].is_equal_approx(pos):
				g.position_offset = pos
				continue
			_moved.erase(id)
		if not columns.has(pos.x):
			columns[pos.x] = []
		columns[pos.x].append(n)
	var xs: Array = columns.keys()
	xs.sort()
	var x := 0.0
	for cx: float in xs:
		var col: Array = columns[cx]
		col.sort_custom(func(a: Dictionary, b: Dictionary) -> bool:
			var pa: Vector2 = a.get("pos", Vector2.ZERO)
			var pb: Vector2 = b.get("pos", Vector2.ZERO)
			return pa.y < pb.y or (pa.y == pb.y and Kit.get_int(a, "id") < Kit.get_int(b, "id")))
		var width := NODE_MIN_W
		var y := 0.0
		for n: Dictionary in col:
			var g := _nodes[Kit.get_int(n, "id")]
			var s := g.get_combined_minimum_size()
			g.size = s
			width = maxf(width, s.x)
			g.position_offset = Vector2(x, y)
			y += s.y + ROW_GAP
		x += width + COLUMN_GAP


func _frame_view() -> void:
	if _nodes.is_empty() or _canvas.size.x < 2.0:
		return
	var box := Rect2()
	var first := true
	for id: int in _nodes:
		var g := _nodes[id]
		var r := Rect2(g.position_offset, g.size)
		box = r if first else box.merge(r)
		first = false
	var pad := 48.0
	var zx := (_canvas.size.x - pad * 2.0) / maxf(box.size.x, 1.0)
	var zy := (_canvas.size.y - pad * 2.0) / maxf(box.size.y, 1.0)
	# Never so small that text falls under the 18 px floor (Dense is 20 px at zoom 1); a graph larger than
	# the view starts at its top left and scrolls. The player may still zoom out with Ctrl and the wheel.
	var z := clampf(minf(zx, zy), MIN_FRAME_ZOOM, 1.0)
	_canvas.zoom = z
	var view := _canvas.size / z
	var centre := box.get_center()
	var top_left := Vector2(centre.x - view.x * 0.5, box.position.y - pad / z) if box.size.y * z > _canvas.size.y else centre - view * 0.5
	if box.size.x * z > _canvas.size.x:
		top_left.x = box.position.x - pad / z
	_canvas.scroll_offset = top_left * z
	_live_layer.queue_redraw()


# --- edges ------------------------------------------------------------------------------------------

## Canvas port of a model edge end. `out_side` is true for the edge's source end.
func _canvas_port(id: int, model_port: int, out_side: bool, generated: bool) -> int:
	var n: Dictionary = _data.get(id, {})
	if n.is_empty() or not _is_boundary(n):
		return model_port
	var n_in := Kit.as_array(n.get("inputs", [])).size()
	if out_side:
		# A generated edge leaves a real output; a subgraph edge reads an input through.
		return n_in + model_port if generated else model_port
	return model_port if generated else n_in + model_port


## Model port of a canvas port and whether the player may wire it; `out_side` as above.
func _model_port(id: int, canvas_port: int, out_side: bool) -> Dictionary:
	var n: Dictionary = _data.get(id, {})
	if n.is_empty() or not _is_boundary(n):
		return {"port": canvas_port, "ok": true}
	var n_in := Kit.as_array(n.get("inputs", [])).size()
	if out_side:
		return {"port": canvas_port, "ok": canvas_port < n_in}
	return {"port": canvas_port - n_in, "ok": canvas_port >= n_in}


func _sync_edges() -> void:
	_canvas.clear_connections()
	for e: Dictionary in _edges:
		var f := Kit.get_int(e, "from_node", -1)
		var t := Kit.get_int(e, "to_node", -1)
		if not _nodes.has(f) or not _nodes.has(t):
			continue
		var gen := Kit.get_bool(e, "generated")
		var fp := _canvas_port(f, Kit.get_int(e, "from_port"), true, gen)
		var tp := _canvas_port(t, Kit.get_int(e, "to_port"), false, gen)
		if fp < _nodes[f].get_output_port_count() and tp < _nodes[t].get_input_port_count():
			_canvas.connect_node(StringName(str(f)), fp, StringName(str(t)), tp)


func _on_connection_request(from_name: StringName, from_port: int, to_name: StringName, to_port: int) -> void:
	var f := int(String(from_name))
	var t := int(String(to_name))
	var mf := _model_port(f, from_port, true)
	var mt := _model_port(t, to_port, false)
	if not bool(mf.ok) or not bool(mt.ok):
		_say(&"editor_graph.refused.boundary")
		_queue_refresh()
		return
	var e := _editor()
	if bool(e.call(&"graph_connect", f, int(mf.port), t, int(mt.port))):
		_say(&"")
	else:
		_say(&"editor_graph.refused.connect" if _editable else &"editor_graph.refused.layer")
	_queue_refresh()


func _on_disconnection_request(from_name: StringName, from_port: int, to_name: StringName, to_port: int) -> void:
	var f := int(String(from_name))
	var t := int(String(to_name))
	var edge := _edge_at(f, from_port, t, to_port)
	if edge.is_empty():
		_queue_refresh()
		return
	_disconnect(edge)


func _disconnect(edge: Dictionary) -> void:
	if Kit.get_bool(edge, "generated"):
		_say(&"editor_graph.refused.generated_edge")
		_queue_refresh()
		return
	var ok := bool(_editor().call(&"graph_disconnect", Kit.get_int(edge, "from_node"), Kit.get_int(edge, "from_port"),
		Kit.get_int(edge, "to_node"), Kit.get_int(edge, "to_port")))
	_say(&"" if ok else &"editor_graph.refused.disconnect")
	_queue_refresh()


## The model edge drawn between two canvas ports.
func _edge_at(f: int, fp: int, t: int, tp: int) -> Dictionary:
	for e: Dictionary in _edges:
		if Kit.get_int(e, "from_node") != f or Kit.get_int(e, "to_node") != t:
			continue
		var gen := Kit.get_bool(e, "generated")
		if _canvas_port(f, Kit.get_int(e, "from_port"), true, gen) == fp and _canvas_port(t, Kit.get_int(e, "to_port"), false, gen) == tp:
			return e
	return {}


# --- selection, moves, deletes ----------------------------------------------------------------------

func _on_node_selected(node: Node) -> void:
	_selected = int(String(node.name))
	(node as GraphNode).queue_redraw()
	_refresh_inspector()


func _on_node_deselected(node: Node) -> void:
	(node as GraphNode).queue_redraw()
	if int(String(node.name)) == _selected:
		_selected = -1
		_refresh_inspector.call_deferred()


func _on_end_move() -> void:
	var e := _editor()
	for id: int in _nodes:
		var g := _nodes[id]
		if not g.selected:
			continue
		_moved[id] = g.position_offset
		e.call(&"graph_move_node", id, g.position_offset)
	_live_layer.queue_redraw()


func _on_delete_request(names: Array) -> void:
	var removed := false
	var refused := false
	for n: Variant in names:
		var id := int(String(n))
		if id < USER_NODE_BASE:
			refused = true
			continue
		if bool(_editor().call(&"graph_remove_node", id)):
			removed = true
			_moved.erase(id)
	if refused:
		_say(&"editor_graph.refused.delete_generated")
	elif removed:
		_say(&"")
	_queue_refresh()


func _on_popup_request(at: Vector2) -> void:
	if not _editable:
		return
	_add_at = (at + _canvas.scroll_offset) / _canvas.zoom
	_add_at_set = true
	open_library()


func _say(msg: Variant) -> void:
	_status_text = msg
	_status.text = _Text.resolve(msg)
	_status.visible = not _status.text.is_empty()


# --- the inspector ----------------------------------------------------------------------------------

func _refresh_inspector() -> void:
	if not is_node_ready():
		return
	var keep := ""
	var f := get_viewport().gui_get_focus_owner() if get_viewport() != null else null
	if f != null and _inspect.is_ancestor_of(f):
		keep = String(f.get_meta(&"focus_key", ""))
	Kit.clear(_inspect)
	if _selected >= 0 and _data.has(_selected):
		_inspect_node(_data[_selected])
	else:
		_inspect_overview()
	_chain_inspector()
	if not keep.is_empty():
		for c: Node in _inspect.find_children("*", "Control", true, false):
			if String(c.get_meta(&"focus_key", "")) == keep:
				Ui.focus(c as Control)
				break


func _inspect_overview() -> void:
	var count := _nodes.size()
	_inspect.add_child(Kit.label("editor_graph.overview.title", &"PanelTitle"))
	var who := tr(&"editor_graph.all_officers") if _officer < 0 else _word(&"officer", _officer)
	_inspect.add_child(Kit.label(tr(&"editor_graph.overview.count").format([who, Kit.number(float(count), &"none", 0)]), &"Body", true))
	_inspect.add_child(Kit.label("editor_graph.overview.hint", &"Dense"))
	if not _editable:
		_inspect.add_child(Kit.label(tr(&"editor_control.layer.capped").format([_word(&"layer", Bp.LAYER_GRAPH)]), &"SeverityAdvisory", true))
	# Legend: frames and glyphs.
	_inspect.add_child(Kit.label("editor_graph.legend.title", &"Body"))
	_inspect.add_child(Kit.label("editor_graph.legend.generated", &"Dense"))
	_inspect.add_child(_glyph_row(&"pin", &"editor_graph.legend.pinned", &""))
	_inspect.add_child(_glyph_row(&"check_caution", &"editor_graph.legend.broken", &"alert.caution"))
	var e := _editor()
	var issues: Array[Dictionary] = []
	if e != null:
		issues = Kit.dicts(e.call(&"graph_issues"))
	_inspect.add_child(Kit.label("editor_graph.issues.title", &"Body"))
	if issues.is_empty():
		_inspect.add_child(Kit.label("editor_graph.issues.none", &"Dense"))
	for i: Dictionary in issues:
		_inspect.add_child(Kit.label(_Text.resolve(i.get("text", {})), &"SeverityFail" if Kit.get_bool(i, "fatal") else &"SeverityCaution", true))


func _glyph_row(icon: StringName, key: StringName, tint: StringName) -> Control:
	var row := Kit.hbox(1)
	var t := TextureRect.new()
	t.texture = Ui.icon(icon)
	t.stretch_mode = TextureRect.STRETCH_KEEP_CENTERED
	if tint != &"":
		t.self_modulate = Ui.colour(tint)
	row.add_child(t)
	var l := Kit.label(String(key), &"Dense")
	l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	row.add_child(l)
	return row


func _inspect_node(n: Dictionary) -> void:
	var id := Kit.get_int(n, "id", -1)
	var type := Kit.get_str(n, "type")
	var lib := _library_entry(type)
	var title := Kit.label(_Text.resolve(n.get("title", {})), &"PanelTitle", true)
	_inspect.add_child(title)
	if not lib.is_empty():
		_inspect.add_child(Kit.label(tr(StringName(lib.get("description", &""))), &"Dense", true))
	var rule := StringName(Kit.as_dict(n.get("title", {})).get("key", &"")) == &"dock.graph.title_rule"
	var generated := Kit.get_bool(n, "generated")
	var pinned := Kit.get_bool(n, "pinned")
	var own := id >= USER_NODE_BASE
	var replaced := Kit.get_int(n, "replaced_stage", -1)
	# What the node is and who drives it.
	var state: StringName = &"editor_graph.state.generated"
	if rule:
		state = &"editor_graph.state.rule"
	elif own:
		state = &"editor_graph.state.own"
	elif pinned:
		state = &"editor_graph.state.pinned"
	elif replaced >= 0 and _is_boundary(n):
		state = &"editor_graph.state.replaced"
	elif replaced >= 0:
		state = &"editor_graph.state.subgraph"
	_inspect.add_child(Kit.label(String(state), &"Body"))
	if Kit.get_bool(n, "broken"):
		_inspect.add_child(Kit.label("editor_graph.node.broken", &"SeverityCaution"))
	var officer := Kit.get_int(n, "officer", -1)
	if officer >= 0:
		_inspect.add_child(Kit.label(tr(&"editor_graph.node.officer").format([_word(&"officer", officer)]), &"Dense", true))
	# Actions.
	var actions := HFlowContainer.new()
	actions.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	actions.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	if _editable and not rule:
		if own:
			var del := Kit.inline_button("editor_graph.action.delete")
			del.tooltip_text = tr(&"editor_graph.action.delete_tip")
			del.pressed.connect(func() -> void: _on_delete_request([StringName(str(id))]))
			del.set_meta(&"focus_key", "delete")
			actions.add_child(del)
		elif pinned:
			var unpin := Kit.inline_button("editor_graph.action.unpin")
			unpin.tooltip_text = tr(&"editor_graph.action.unpin_tip")
			unpin.pressed.connect(func() -> void: _act(bool(_editor().call(&"graph_pin", id, false))))
			unpin.set_meta(&"focus_key", "unpin")
			actions.add_child(unpin)
		elif generated and replaced < 0:
			var pin := Kit.inline_button("editor_graph.action.pin")
			pin.tooltip_text = tr(&"editor_graph.action.pin_tip")
			pin.pressed.connect(func() -> void: _act(bool(_editor().call(&"graph_pin", id, true))))
			pin.set_meta(&"focus_key", "pin")
			actions.add_child(pin)
		var stage := STAGES.find(type.trim_prefix("stage.")) if type.begins_with("stage.") else -1
		if stage >= 0 and not own:
			if replaced >= 0:
				var restore := Kit.inline_button("editor_graph.action.restore")
				restore.tooltip_text = tr(&"editor_graph.action.restore_tip")
				restore.pressed.connect(func() -> void: _act(bool(_editor().call(&"graph_restore_stage", stage))))
				restore.set_meta(&"focus_key", "restore")
				actions.add_child(restore)
			elif not pinned:
				var fork := Kit.inline_button("editor_graph.action.replace")
				fork.tooltip_text = tr(&"editor_graph.action.replace_tip")
				fork.pressed.connect(func() -> void: _replace_stage(stage))
				fork.set_meta(&"focus_key", "replace")
				actions.add_child(fork)
	if actions.get_child_count() > 0:
		_inspect.add_child(actions)
	else:
		actions.free()
	if rule:
		_inspect.add_child(Kit.label("editor_graph.state.rule_edit", &"Dense"))
	_inspect_params(n, lib, rule)
	_inspect_inputs(n, rule)
	_inspect_outputs(n)


func _inspect_params(n: Dictionary, lib: Dictionary, rule: bool) -> void:
	var specs := Kit.dicts(lib.get("params", []))
	if specs.is_empty():
		return
	var id := Kit.get_int(n, "id", -1)
	var values := Kit.as_dict(n.get("params", {}))
	var can_edit := _editable and not rule and not (_is_boundary(n))
	_inspect.add_child(Kit.label("editor_graph.params.title", &"Body"))
	var grid := Kit.vbox(1)
	for p: Dictionary in specs:
		var key := Kit.get_str(p, "key")
		var kind := StringName(p.get("type", &"scalar"))
		var l := Kit.label(String(p.get("label", &"")), &"Dense")
		l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		grid.add_child(l)
		var current: Variant = values.get(key, p.get("default", 0.0))
		var field: Control
		match kind:
			&"part":
				var pick := OptionButton.new()
				pick.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
				pick.custom_minimum_size.y = FIELD_H
				var parts := Kit.dicts(_editor().call(&"parts"))
				for part: Dictionary in parts:
					var slot := Kit.get_int(part, "slot", -1)
					pick.add_item(Inspector.part_name(slot, parts), slot)
				var at := pick.get_item_index(int(current) if current is int or current is float else -1)
				if at >= 0:
					pick.select(at)
				# Only the player's own nodes change what they are bound to.
				pick.disabled = not can_edit or id < USER_NODE_BASE
				pick.item_selected.connect(func(i: int) -> void: _set_param(id, key, pick.get_item_id(i)))
				field = pick
			&"compartment":
				var spin := _number_field(float(current) if current is int or current is float else 0.0, 1.0)
				spin.min_value = 0.0
				spin.editable = can_edit and id >= USER_NODE_BASE
				spin.value_changed.connect(func(v: float) -> void: _set_param(id, key, int(v)))
				field = spin
			&"value":
				field = _constant_field(id, current, can_edit)
			_:
				var spin := _number_field(float(current) if current is int or current is float else 0.0, 0.01)
				spin.editable = can_edit
				spin.value_changed.connect(func(v: float) -> void: _set_param(id, key, v))
				field = spin
		field.set_meta(&"focus_key", "param:" + key)
		field.accessibility_name = tr(StringName(p.get("label", &"")))
		field.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		grid.add_child(field)
	_inspect.add_child(grid)
	if can_edit and Kit.get_bool(n, "generated") and not Kit.get_bool(n, "pinned"):
		_inspect.add_child(Kit.label("editor_graph.params.pins", &"Dense"))


func _number_field(v: float, step: float) -> SpinBox:
	var f := SpinBox.new()
	f.step = step
	f.allow_greater = true
	f.allow_lesser = true
	f.min_value = -1000000.0
	f.max_value = 1000000.0
	f.custom_minimum_size = Vector2(150.0, FIELD_H)
	f.select_all_on_focus = true
	f.update_on_text_changed = false
	f.set_value_no_signal(v)
	return f


## A constant's value: a number, or on and off.
func _constant_field(id: int, current: Variant, can_edit: bool) -> Control:
	var value := current as Dictionary if current is Dictionary else {"type": &"scalar", "value": float(current) if current is float or current is int else 0.0}
	var box := Kit.vbox(1)
	var kind := OptionButton.new()
	kind.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	kind.custom_minimum_size.y = FIELD_H
	kind.add_item(tr(&"editor_graph.params.number"), 0)
	kind.add_item(tr(&"editor_graph.params.switch"), 1)
	var is_bool := StringName(value.get("type", &"scalar")) == &"bool"
	kind.select(1 if is_bool else 0)
	kind.disabled = not can_edit
	kind.accessibility_name = tr(&"editor_graph.params.kind")
	box.add_child(kind)
	if is_bool:
		var on := CheckButton.new()
		on.button_pressed = bool(value.get("value", false))
		on.text = tr(&"editor_graph.params.on")
		on.disabled = not can_edit
		on.toggled.connect(func(b: bool) -> void: _set_param(id, "value", {"type": &"bool", "value": b}))
		box.add_child(on)
	else:
		var spin := _number_field(float(value.get("value", 0.0)) if value.get("value", 0.0) is float or value.get("value", 0.0) is int else 0.0, 0.01)
		spin.editable = can_edit
		spin.value_changed.connect(func(v: float) -> void: _set_param(id, "value", {"type": &"scalar", "value": v}))
		box.add_child(spin)
	kind.item_selected.connect(func(i: int) -> void:
		_set_param(id, "value", {"type": &"bool", "value": false} if i == 1 else {"type": &"scalar", "value": 0.0}))
	return box


func _set_param(id: int, key: String, value: Variant) -> void:
	var ok := bool(_editor().call(&"graph_set_param", id, key, value))
	_say(&"" if ok else &"editor_graph.refused.param")
	_queue_refresh()


## Each input: what feeds it, and a picker of every compatible output shown (non-drag wiring).
func _inspect_inputs(n: Dictionary, rule: bool) -> void:
	var id := Kit.get_int(n, "id", -1)
	var ins := Kit.dicts(n.get("inputs", []))
	var boundary := _is_boundary(n)
	var drives: Array[Dictionary] = []
	if boundary:
		drives = Kit.dicts(n.get("outputs", []))
	if ins.is_empty() and drives.is_empty():
		return
	_inspect.add_child(Kit.label("editor_graph.inputs.title", &"Body"))
	var targets: Array[Dictionary] = []
	for i in ins.size():
		targets.append({"port": ins[i], "model": i, "generated_side": true})
	for j in drives.size():
		targets.append({"port": drives[j], "model": j, "generated_side": false, "drive": true})
	for t: Dictionary in targets:
		var port: Dictionary = t.port
		var model_port: int = t.model
		var driving := bool(t.get("drive", false))
		var box := Kit.vbox(0)
		var label := tr(&"editor_graph.inputs.drive").format([_port_name(port)]) if driving else _port_name(port)
		box.add_child(Kit.label(label, &"Dense", true))
		var current := _feeding(id, model_port, driving if boundary else false)
		var pick := OptionButton.new()
		pick.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		pick.custom_minimum_size.y = FIELD_H
		pick.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		pick.fit_to_longest_item = false
		pick.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
		pick.accessibility_name = tr(&"editor_graph.inputs.pick").format([_port_name(port)])
		pick.add_item(tr(&"editor_graph.inputs.none"), 0)
		pick.set_item_metadata(0, {})
		var want := Kit.get_int(port, "type")
		var any := Kit.get_str(n, "type") == "math.select" and model_port > 0
		var selected_index := 0
		for src: Dictionary in _sources(id, want, any):
			var idx := pick.item_count
			pick.add_item(String(src.text), idx)
			pick.set_item_metadata(idx, src)
			if not current.is_empty() and Kit.get_int(current, "from_node") == int(src.node) and Kit.get_int(current, "from_port") == int(src.port):
				selected_index = idx
		if not current.is_empty() and selected_index == 0:
			# Fed from outside what is shown (another officer): list it so the picker tells the truth.
			var idx := pick.item_count
			pick.add_item(_source_text(Kit.get_int(current, "from_node"), Kit.get_int(current, "from_port")), idx)
			pick.set_item_metadata(idx, {"node": Kit.get_int(current, "from_node"), "port": Kit.get_int(current, "from_port")})
			selected_index = idx
		pick.select(selected_index)
		# A replaced stage's own inputs stay with the generated graph; only its drives are the player's.
		pick.disabled = not _editable or rule or (boundary and not driving)
		pick.item_selected.connect(func(i: int) -> void: _pick_source(id, model_port, driving and boundary, current, Kit.as_dict(pick.get_item_metadata(i))))
		pick.set_meta(&"focus_key", "input:%d:%s" % [model_port, str(driving)])
		box.add_child(pick)
		var live := _live_text(Kit.get_int(current, "from_node", -1), Kit.get_int(current, "from_port", -1)) if not current.is_empty() else ""
		if not live.is_empty():
			box.add_child(Kit.label(tr(&"editor_graph.live.now").format([live]), &"Dense", true))
		_inspect.add_child(box)


## The model edge into a node's port; `drive` picks a subgraph edge into a replaced stage's output.
func _feeding(id: int, port: int, drive: bool) -> Dictionary:
	for e: Dictionary in _edges:
		if Kit.get_int(e, "to_node") != id or Kit.get_int(e, "to_port") != port:
			continue
		if _is_boundary(_data.get(id, {})) and Kit.get_bool(e, "generated") == drive:
			continue
		return e
	return {}


## Outputs of shown nodes that may feed a port of type `want`: same type, scalars, angles and booleans
## mixed, anything into a Select choice. The model has the last word when one is picked.
func _sources(target: int, want: int, any: bool) -> Array[Dictionary]:
	var out: Array[Dictionary] = []
	var ids: Array = _nodes.keys()
	ids.sort()
	for id: int in ids:
		if id == target:
			continue
		var n: Dictionary = _data[id]
		var ports := Kit.dicts(n.get("inputs", [])) if _is_boundary(n) else Kit.dicts(n.get("outputs", []))
		var select := Kit.get_str(n, "type") == "math.select"
		for p in ports.size():
			var t := Kit.get_int(ports[p], "type")
			if any or select or _compatible(t, want):
				out.append({"node": id, "port": p, "text": _source_text(id, p)})
	return out


static func _compatible(from: int, to: int) -> bool:
	if from == to:
		return true
	var mix: Array[int] = [Bp.PORT_SCALAR, Bp.PORT_ANGLE, Bp.PORT_BOOL]
	return mix.has(from) and mix.has(to) and (from == Bp.PORT_SCALAR or to == Bp.PORT_SCALAR)


func _source_text(id: int, port: int) -> String:
	var n: Dictionary = _all.get(id, {})
	if n.is_empty():
		return tr(&"editor_graph.inputs.unknown")
	var ports := Kit.dicts(n.get("inputs", [])) if _is_boundary(n) else Kit.dicts(n.get("outputs", []))
	var port_name := _port_name(ports[port]) if port >= 0 and port < ports.size() else ""
	return tr(&"editor_graph.inputs.source").format([_Text.resolve(n.get("title", {})), port_name])


func _pick_source(id: int, port: int, _drive: bool, current: Dictionary, src: Dictionary) -> void:
	if src.is_empty():
		if not current.is_empty():
			_disconnect(current)
		return
	var ok := bool(_editor().call(&"graph_connect", int(src.node), int(src.port), id, port))
	_say(&"" if ok else &"editor_graph.refused.connect")
	_queue_refresh()


func _inspect_outputs(n: Dictionary) -> void:
	var id := Kit.get_int(n, "id", -1)
	var outs := Kit.dicts(n.get("outputs", []))
	if outs.is_empty():
		return
	_inspect.add_child(Kit.label("editor_graph.outputs.title", &"Body"))
	var boundary := _is_boundary(n)
	for p in outs.size():
		var to: PackedStringArray = []
		for e: Dictionary in _edges:
			if Kit.get_int(e, "from_node") != id or Kit.get_int(e, "from_port") != p:
				continue
			if boundary and not Kit.get_bool(e, "generated"):
				continue
			var t: Dictionary = _all.get(Kit.get_int(e, "to_node"), {})
			if not t.is_empty():
				to.append(_Text.resolve(t.get("title", {})))
		var text := tr(&"editor_graph.outputs.row").format([_port_name(outs[p]), ", ".join(to) if not to.is_empty() else tr(&"editor_graph.outputs.unused")])
		var live := _live_text(id, p)
		if not live.is_empty():
			text += "\n" + tr(&"editor_graph.live.now").format([live])
		_inspect.add_child(Kit.label(text, &"Dense", true))


func _replace_stage(stage: int) -> void:
	var first := int(_editor().call(&"graph_replace_stage", stage))
	if first < 0:
		_say(&"editor_graph.refused.replace")
		return
	_say(&"")
	# The fork's first node goes under its stage in this layout once the canvas is laid out.
	_place_under = {"node": first, "type": "stage." + STAGES[stage]}
	_focus_pending = first
	_refresh()


## Put a new subgraph node under the stage it replaces and keep it there.
func _place_fork() -> void:
	var first := int(_place_under.get("node", -1))
	var type := String(_place_under.get("type", ""))
	if not _all.has(first):
		_place_under = {}
		return
	if not _nodes.has(first):
		# Placed when the officer it belongs to is shown.
		return
	for id: int in _data:
		if _nodes.has(id) and _is_boundary(_data[id]) and Kit.get_str(_data[id], "type") == type:
			var s := _nodes[id]
			var at := s.position_offset + Vector2(0.0, s.size.y + ROW_GAP * 2.0)
			_nodes[first].position_offset = at
			_moved[first] = at
			_editor().call(&"graph_move_node", first, at)
			_place_under = {}
			return


func _act(ok: bool) -> void:
	_say(&"" if ok else &"editor_graph.refused.edit")
	_queue_refresh()


func _chain_inspector() -> void:
	var list: Array = [_officer_pick, _add, _frame_all]
	for c: Node in _inspect.find_children("*", "Control", true, false):
		var ctl := c as Control
		if ctl.focus_mode == Control.FOCUS_NONE or not ctl.visible:
			continue
		if ctl is LineEdit and ctl.get_parent() is SpinBox and (ctl.get_parent() as Control).focus_mode != Control.FOCUS_NONE:
			continue
		list.append(ctl)
	var live := Kit.chain_vertical(list)
	if not live.is_empty():
		live[0].focus_neighbor_top = NodePath()
		live[0].focus_previous = NodePath()
		live[-1].focus_neighbor_bottom = NodePath()
		live[-1].focus_next = NodePath()


# --- the library ------------------------------------------------------------------------------------

func _load_library() -> void:
	var e := _editor()
	if e == null:
		return
	_library_list = Kit.dicts(e.call(&"node_library"))
	_library_by_type.clear()
	for l: Dictionary in _library_list:
		_library_by_type[Kit.get_str(l, "type")] = l


func _library_entry(type: String) -> Dictionary:
	if _library_by_type.is_empty():
		_load_library()
	if _library_by_type.has(type):
		return _library_by_type[type]
	# Bound and numbered kinds share their library entry's stem (`sink.pump`, `script:0`).
	var stem := type.get_slice(":", 0)
	return _library_by_type.get(stem, {})


func _fill_tree() -> void:
	_tree.clear()
	var root := _tree.create_item()
	var groups: Dictionary[int, TreeItem] = {}
	var filter := _search.text.strip_edges().to_lower()
	var first: TreeItem = null
	for l: Dictionary in _library_list:
		var name := tr(StringName(l.get("name", &""))) if Kit.get_int(l, "group") != Bp.NODE_SCRIPT else String(l.get("name", ""))
		var desc := tr(StringName(l.get("description", &"")))
		if not filter.is_empty() and not name.to_lower().contains(filter) and not desc.to_lower().contains(filter):
			continue
		var group := Kit.get_int(l, "group")
		if not groups.has(group):
			var g := _tree.create_item(root)
			g.set_text(0, tr(GROUP_KEYS[clampi(group, 0, GROUP_KEYS.size() - 1)]))
			g.set_selectable(0, false)
			g.set_custom_font_size(0, Ui.font_size(&"Body"))
			groups[group] = g
		var item := _tree.create_item(groups[group])
		item.set_text(0, name)
		item.set_metadata(0, l)
		item.set_tooltip_text(0, desc)
		var ok := Kit.get_bool(l, "phase_ok", true) and _editable
		item.set_selectable(0, true)
		if not ok:
			item.set_text(0, tr(&"editor_graph.library.unavailable").format([name]))
		if first == null and ok:
			first = item
	if first != null:
		first.select(0)
		_tree.scroll_to_item(first)
	_on_tree_selected()


func _on_tree_selected() -> void:
	var item := _tree.get_selected()
	if item == null or item.get_metadata(0) == null:
		_description.text = tr(&"editor_graph.library.none") if _tree.get_root() == null or _tree.get_root().get_child_count() == 0 else ""
		_place.disabled = true
		return
	var l := Kit.as_dict(item.get_metadata(0))
	var ins := Kit.as_array(l.get("inputs", [])).size()
	var outs := Kit.as_array(l.get("outputs", [])).size()
	_description.text = tr(StringName(l.get("description", &""))) + "\n" + tr(&"editor_graph.library.ports").format([Kit.number(float(ins), &"none", 0), Kit.number(float(outs), &"none", 0)])
	_place.disabled = not (Kit.get_bool(l, "phase_ok", true) and _editable)


func _place_selected() -> void:
	var item := _tree.get_selected()
	if item == null or item.get_metadata(0) == null or _place.disabled:
		return
	var l := Kit.as_dict(item.get_metadata(0))
	var at := _add_at if _add_at_set else (_canvas.scroll_offset + _canvas.size * 0.5) / _canvas.zoom - Vector2(NODE_MIN_W * 0.5, 40.0)
	var id := int(_editor().call(&"graph_add_node", Kit.get_str(l, "type"), at))
	_library.visible = false
	_add.set_pressed_no_signal(false)
	_add_at_set = false
	if id < 0:
		_say(&"editor_graph.refused.add")
		return
	_say(&"")
	_focus_pending = id
	_refresh()


# --- live values (splash) ---------------------------------------------------------------------------

func _in_splash() -> bool:
	var e := _editor()
	return e != null and e.has_method(&"in_splash") and bool(e.call(&"in_splash"))


## Scope the source ports of every edge shown; drop the ones no longer shown.
func _rescope() -> void:
	if not _in_splash() or not is_visible_in_tree():
		_unscope_all()
		return
	var want: Dictionary[String, bool] = {}
	for e: Dictionary in _edges:
		var f := Kit.get_int(e, "from_node", -1)
		if not _nodes.has(f) or not _nodes.has(Kit.get_int(e, "to_node", -1)):
			continue
		if want.size() >= MAX_SCOPED:
			break
		want["%d:%d" % [f, Kit.get_int(e, "from_port")]] = true
	var orders := Kit.autoload(&"Orders")
	if orders == null:
		return
	for k: String in _scoped.keys():
		if not want.has(k):
			orders.call(&"query_scope", int(k.get_slice(":", 0)), int(k.get_slice(":", 1)), false)
			_scoped.erase(k)
	for k: String in want:
		if not _scoped.has(k):
			if bool(orders.call(&"query_scope", int(k.get_slice(":", 0)), int(k.get_slice(":", 1)), true)):
				_scoped[k] = true


func _unscope_all() -> void:
	var orders := Kit.autoload(&"Orders")
	if orders != null:
		for k: String in _scoped:
			orders.call(&"query_scope", int(k.get_slice(":", 0)), int(k.get_slice(":", 1)), false)
	_scoped.clear()
	if not _live.is_empty():
		_live.clear()
		if is_node_ready():
			_live_layer.queue_redraw()


func _on_scope_value(_node: int, _port: int, _value: Dictionary) -> void:
	if not is_visible_in_tree() or _live_timer.time_left > 0.0:
		return
	_live_timer.start()


func _pull_live() -> void:
	var e := _editor()
	_live = Kit.as_dict(e.call(&"graph_live_values")) if e != null else {}
	_live_layer.queue_redraw()


func _live_text(node: int, port: int) -> String:
	return String(_live.get("%d:%d" % [node, port], ""))


## True when a rectangle on the live layer overlaps a shown node.
func _covers_node(r: Rect2) -> bool:
	var z := _canvas.zoom
	for id: int in _nodes:
		var g := _nodes[id]
		if Rect2(_canvas.position + g.position, g.size * z).grow(4.0).intersects(r):
			return true
	return false


## Values on edges during a splash, and on inputs fed from another officer, the source's name.
func _draw_live() -> void:
	if _nodes.is_empty():
		return
	var font := get_theme_font(&"font", &"Label")
	var size := Ui.font_size(&"Dense")
	var ink := Ui.colour(&"text.primary")
	var quiet := Ui.colour(&"text.secondary")
	var back := Ui.colour(&"surface.inset")
	var z := _canvas.zoom
	var origin := _canvas.position
	for e: Dictionary in _edges:
		var f := Kit.get_int(e, "from_node", -1)
		var t := Kit.get_int(e, "to_node", -1)
		if not _nodes.has(t):
			continue
		var gen := Kit.get_bool(e, "generated")
		var gt := _nodes[t]
		var tp := _canvas_port(t, Kit.get_int(e, "to_port"), false, gen)
		if tp >= gt.get_input_port_count():
			continue
		var b := origin + gt.position + gt.get_input_port_position(tp) * z
		if not _nodes.has(f):
			# Fed from an officer not shown: a stub, and the source's name where it does not cover a node.
			var src: Dictionary = _all.get(f, {})
			if src.is_empty():
				continue
			_live_layer.draw_line(b, b - Vector2(STUB_PX * z, 0.0), quiet, 1.0, true)
			_live_layer.draw_circle(b - Vector2(STUB_PX * z, 0.0), 3.0, quiet, false, 1.0, true)
			var text := tr(&"editor_graph.from").format([_Text.resolve(src.get("title", {}))])
			var ts := font.get_string_size(text, HORIZONTAL_ALIGNMENT_LEFT, -1, size)
			var rect := Rect2(b - Vector2(STUB_PX * z + 6.0 + ts.x, ts.y * 0.5), ts)
			if not _covers_node(rect):
				_live_layer.draw_string(font, Vector2(rect.position.x, rect.position.y + font.get_ascent(size)), text, HORIZONTAL_ALIGNMENT_LEFT, -1, size, quiet)
			continue
		var live := _live_text(f, Kit.get_int(e, "from_port"))
		if live.is_empty():
			continue
		var gf := _nodes[f]
		var fp := _canvas_port(f, Kit.get_int(e, "from_port"), true, gen)
		if fp >= gf.get_output_port_count():
			continue
		var a := origin + gf.position + gf.get_output_port_position(fp) * z
		var mid := (a + b) * 0.5
		var ts := font.get_string_size(live, HORIZONTAL_ALIGNMENT_LEFT, -1, size)
		var box := Rect2(mid - Vector2(ts.x * 0.5 + 6.0, ts.y * 0.5 + 2.0), Vector2(ts.x + 12.0, ts.y + 4.0))
		_live_layer.draw_rect(box, back)
		_live_layer.draw_rect(box, quiet, false, 1.0)
		_live_layer.draw_string(font, Vector2(box.position.x + 6.0, box.position.y + 2.0 + font.get_ascent(size)), live, HORIZONTAL_ALIGNMENT_LEFT, -1, size, ink)
