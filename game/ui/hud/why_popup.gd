extends Control
## Why? chain for a part, with links to the responsible part or node.
## DESIGN.md L353. Unit: ui-hud-board.
## Reads and calls: Orders.query_why, Hud.why_reply.
##
## "Why?" works on any part: right-click a part of the own hull in the bridge view, or ask from a
## panel that lists parts, and the popup prints the chain of causes the simulation reports from
## graph state ("Gun 2 idle: solution needs +74 degrees, mount maximum is +70"). No spinner: the
## popup opens when the reply arrives, one round trip after the question.
## - The title is the part's name; each link of the chain is one line, in the order the reply
##   gives. A link that names a part is a button that asks Why? of that part in turn (Back returns
##   to the previous part). A link that names a graph node is a button that opens that node:
##   in the dry dock (a splash) it goes to every node in the `graph_focus` group that has
##   `focus_graph_node(node)`; in a sortie, where graphs cannot be edited (L112), it opens the
##   control station, the in-sortie layers of the same behaviour.
## - The popup sits beside where it was asked, kept inside the view between the docks (the screen
##   when there is no HUD round it). A reply nobody here asked
##   for (another panel called `Orders.query_why`) opens beside the mouse.
## - It closes with its close button, a click outside it, Esc while it has focus, or a new sortie.
## Other scripts call `ask(slot, at)` with a viewport position, or `show_why(why)` with a
## `WhyDict` (tests and captures). Signal `node_requested(node)` fires for every node link.

signal node_requested(node: int)

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")

const NONE := -1
## Gap between the place asked and the popup.
const GAP := 16.0
const WIDTH := 480.0
## A question older than this no longer anchors the reply.
const ASK_FRESH_MS := 3000
## Deepest Back history kept.
const HISTORY_MAX := 16

@onready var _panel: PanelContainer = $Panel
@onready var _margin: MarginContainer = $Panel/Margin
@onready var _column: VBoxContainer = $Panel/Margin/Column
@onready var _header: HBoxContainer = $Panel/Margin/Column/Header
@onready var _back: Button = $Panel/Margin/Column/Header/Back
@onready var _title: Label = $Panel/Margin/Column/Header/Title
@onready var _close: Button = $Panel/Margin/Column/Header/Close
@onready var _chain: VBoxContainer = $Panel/Margin/Column/Chain

var _anchor := Vector2.ZERO
var _asked_slot := NONE
var _asked_ms := -ASK_FRESH_MS
var _slot := NONE
var _history: Array[int] = []
var _why: Dictionary = {}
var _following := false


func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	_panel.mouse_filter = Control.MOUSE_FILTER_STOP
	_panel.visible = false
	Ui.set_margins(_margin, 2)
	Ui.set_separation(_column, 2)
	Ui.set_separation(_header, 2)
	Ui.set_separation(_chain, 1)
	_title.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_back.icon = Ui.icon(&"arrow_left")
	_close.icon = Ui.icon(&"close")
	for b: Button in [_back, _close]:
		b.focus_mode = Control.FOCUS_ALL
		b.icon_alignment = HORIZONTAL_ALIGNMENT_CENTER
		b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_back.pressed.connect(_go_back)
	_panel.minimum_size_changed.connect(_place, CONNECT_DEFERRED)
	_close.pressed.connect(close)
	_render_static_text()
	var hud := Kit.autoload(&"Hud")
	if hud != null and hud.has_signal(&"why_reply"):
		hud.connect(&"why_reply", _on_reply)
	var session := Kit.autoload(&"Session")
	if session != null and session.has_signal(&"state_changed"):
		session.connect(&"state_changed", func(_s: int) -> void: close())
	set_process_input(false)


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_render_static_text()
		if _panel.visible:
			show_why(_why)


func _render_static_text() -> void:
	_back.tooltip_text = tr(&"hud_board.why.back")
	_back.accessibility_name = tr(&"hud_board.why.back")
	_close.tooltip_text = tr(&"hud_board.why.close")
	_close.accessibility_name = tr(&"hud_board.why.close")


## Ask Why? of part `slot`; the popup opens beside `at` (a viewport position) when the reply comes.
func ask(slot: int, at: Vector2) -> void:
	if slot < 0:
		return
	_anchor = at
	_asked_slot = slot
	_asked_ms = Time.get_ticks_msec()
	_following = false
	_history.clear()
	Kit.call_on(&"Orders", &"query_why", [slot])


## True while the popup shows a chain.
func is_open() -> bool:
	return is_node_ready() and _panel.visible


func close() -> void:
	if not is_node_ready():
		return
	_panel.visible = false
	_history.clear()
	_following = false
	set_process_input(false)


func _on_reply(why: Dictionary) -> void:
	var slot := Kit.get_int(why, "slot", NONE)
	var fresh := Time.get_ticks_msec() - _asked_ms <= ASK_FRESH_MS and slot == _asked_slot
	if not fresh and not _following:
		_anchor = get_viewport().get_mouse_position()
		_history.clear()
	_asked_slot = NONE
	_following = false
	show_why(why)


## Show one `WhyDict` beside the last anchor.
func show_why(why: Dictionary) -> void:
	if not is_node_ready():
		return
	_why = why
	var slot := Kit.get_int(why, "slot", NONE)
	_slot = slot
	var name := _part_name(slot)
	_title.text = name
	_panel.accessibility_name = tr(&"hud_board.why.access").format([name])
	for c: Node in _chain.get_children():
		_chain.remove_child(c)
		c.queue_free()
	var links := Kit.dicts(why.get("chain", []))
	var focusable: Array[Control] = [_back, _close]
	var spoken: PackedStringArray = []
	if links.is_empty():
		var none := Kit.label("hud_board.why.none", &"Body")
		none.custom_minimum_size = Vector2(WIDTH - 48.0, 0.0)
		_chain.add_child(none)
		spoken.append(tr(&"hud_board.why.none"))
	for link: Dictionary in links:
		var text := _Text.resolve(link.get("text", {}))
		if text.is_empty():
			continue
		spoken.append(text)
		var part := Kit.get_int(link, "part", NONE)
		var node := Kit.get_int(link, "node", NONE)
		if part != NONE and part != slot:
			var b := _link_button(text, tr(&"hud_board.why.part_tip").format([_part_name(part)]))
			b.pressed.connect(_follow_part.bind(part))
			_chain.add_child(b)
			focusable.append(b)
		elif node != NONE:
			var b := _link_button(text, tr(&"hud_board.why.node_tip"))
			b.pressed.connect(_open_node.bind(node))
			_chain.add_child(b)
			focusable.append(b)
		else:
			var l := Kit.label(text, &"Body", true)
			l.custom_minimum_size = Vector2(WIDTH - 48.0, 0.0)
			_chain.add_child(l)
	_panel.accessibility_description = "\n".join(spoken)
	_back.visible = not _history.is_empty()
	_link_focus(focusable)
	_panel.visible = true
	set_process_input(true)
	_place.call_deferred()



func _link_button(text: String, tip: String) -> Button:
	var b := Button.new()
	b.theme_type_variation = &"FlatButton"
	b.text = text
	b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	b.alignment = HORIZONTAL_ALIGNMENT_LEFT
	b.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	b.custom_minimum_size = Vector2(WIDTH - 48.0, 32.0)
	b.icon = Ui.icon(&"link")
	b.focus_mode = Control.FOCUS_ALL
	b.tooltip_text = tip
	b.accessibility_description = tip
	return b


func _link_focus(controls: Array[Control]) -> void:
	var live: Array[Control] = []
	for c: Control in controls:
		if c.visible:
			live.append(c)
	var header: Array[Control] = []
	var rows: Array[Control] = []
	for c: Control in live:
		if c == _back or c == _close:
			header.append(c)
		else:
			rows.append(c)
	Kit.chain_horizontal(header)
	var column: Array = [_close]
	column.append_array(rows)
	Kit.chain_vertical(column)
	if _back.visible:
		_back.focus_neighbor_bottom = _back.get_path_to(rows[0] if not rows.is_empty() else _back)


func _follow_part(part: int) -> void:
	if _slot != NONE and _history.size() < HISTORY_MAX:
		_history.append(_slot)
	_asked_slot = part
	_asked_ms = Time.get_ticks_msec()
	_following = true
	Kit.call_on(&"Orders", &"query_why", [part])


func _go_back() -> void:
	if _history.is_empty():
		return
	var part: int = _history.pop_back()
	_asked_slot = part
	_asked_ms = Time.get_ticks_msec()
	_following = true
	Kit.call_on(&"Orders", &"query_why", [part])


func _open_node(node: int) -> void:
	node_requested.emit(node)
	if get_tree().get_node_count_in_group(&"graph_focus") > 0:
		get_tree().call_group(&"graph_focus", &"focus_graph_node", node)
		return
	var hud := _hud()
	if hud != null and hud.has_method(&"open_station"):
		var open: bool = hud.has_method(&"station") and StringName(hud.call(&"station")) == &"control"
		if not open:
			hud.call(&"open_station", &"control")
	close()


func _hud() -> Node:
	var n := get_parent()
	while n != null:
		if n.has_method(&"open_station"):
			return n
		n = n.get_parent()
	return null


func _part_name(slot: int) -> String:
	if slot < 0:
		return tr(&"hud_board.why.unknown_part")
	for p: Dictionary in Kit.dicts(Kit.call_on(&"Hud", &"parts_static")):
		if Kit.get_int(p, "slot", NONE) == slot:
			var n := _Text.resolve(p.get("name", {}))
			if not n.is_empty():
				return n
	return tr(&"hud_board.why.part_number").format([slot + 1])


## Beside the anchor, inside the visible area.
func _place() -> void:
	if not _panel.visible:
		return
	var size := _panel.get_combined_minimum_size()
	size.x = maxf(size.x, WIDTH)
	_panel.size = size
	var inv := get_global_transform_with_canvas().affine_inverse()
	var local := inv * _anchor
	# Inside the sortie view between the docks when there is one, so the docked panels stay clear.
	var area := get_viewport().get_visible_rect()
	var hud := _hud()
	var centre := hud.get_node_or_null(^"Frame/Docks/Centre") as Control if hud != null else null
	if centre != null and centre.size.x >= WIDTH + GAP * 2.0:
		area = Rect2(centre.get_global_transform_with_canvas().origin, centre.size)
	var view := inv * area
	var pos := local + Vector2(GAP, GAP)
	if pos.x + size.x > view.end.x - GAP:
		pos.x = local.x - GAP - size.x
	if pos.y + size.y > view.end.y - GAP:
		pos.y = view.end.y - GAP - size.y
	pos.x = clampf(pos.x, view.position.x + GAP, maxf(view.position.x + GAP, view.end.x - GAP - size.x))
	pos.y = clampf(pos.y, view.position.y + GAP, maxf(view.position.y + GAP, view.end.y - GAP - size.y))
	_panel.position = pos


## A click outside the popup closes it (the click still reaches what it was meant for); Esc closes
## it while focus is inside.
func _input(event: InputEvent) -> void:
	if not _panel.visible:
		return
	if event is InputEventMouseButton and (event as InputEventMouseButton).pressed:
		if not _contains((event as InputEventMouseButton).position):
			close()
	elif event.is_action_pressed(&"ui_cancel", false, true):
		var f := get_viewport().gui_get_focus_owner()
		if f != null and _panel.is_ancestor_of(f):
			close()
			get_viewport().set_input_as_handled()


func _contains(viewport_pos: Vector2) -> bool:
	var local := _panel.get_global_transform_with_canvas().affine_inverse() * viewport_pos
	return Rect2(Vector2.ZERO, _panel.size).has_point(local)
