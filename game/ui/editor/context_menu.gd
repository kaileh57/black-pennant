extends Control
## Context menu: Why?, rotate, remove, select kit.
## DESIGN.md L238, L353. Unit: ui-editor-shell.
## Reads and calls: BpEditorView.pick, Editor.remove_part, Editor.notes.
##
## Secondary click in the dry dock (L245) opens this menu on what is under the cursor, as the dock's
## `BpEditorView.pick` reported it: a part (Why?, rotate, select its kit, copy, remove), a deck or
## bulkhead (remove), or the hull and empty space (paste, select all, frame). With a selection it also
## offers delete and capture as kit; while placing, Stop placing comes first.
##
## Why? (L353). In the dock the chain comes from the design: every Ship's Note and validation issue that
## names the part, each a sentence with its Fix preview when it has one. During a splash the chain is the
## simulation's (`Orders.query_why`, answered by `Hud.why_reply`): each link opens the node or part
## responsible through `why_link`, which the dock turns into the graph view or a framed selection. During a
## splash the same menu holds the course orders (Steer here, Add waypoint) for a point on the sea.

signal why_link(node: int, part: int)
signal frame_requested(what: StringName)
signal closed

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const Inspector := preload("res://ui/editor/part_inspector.gd")

enum Item { WHY, ROTATE, SELECT_KIT, COPY, REMOVE_PART, REMOVE_KIT, REMOVE_PLANE, PASTE, SELECT_ALL, FRAME_ALL,
	FRAME_PART, DELETE_SELECTION, CAPTURE, STOP_PLACING, STEER, WAYPOINT, SPLASH_WHY }

@onready var _menu: PopupMenu = $Menu
@onready var _why: PopupPanel = $Why
@onready var _why_title: Label = $Why/Margin/Column/Title
@onready var _why_lines: VBoxContainer = $Why/Margin/Column/Lines
@onready var _why_close: Button = $Why/Margin/Column/Close
@onready var _capture: PopupPanel = $Capture
@onready var _capture_name: LineEdit = $Capture/Margin/Column/Name
@onready var _capture_confirm: Button = $Capture/Margin/Column/Row/Confirm
@onready var _capture_cancel: Button = $Capture/Margin/Column/Row/Cancel

var _hit: Dictionary = {}
var _at := Vector2.ZERO
var _why_slot := -1


func _ready() -> void:
	visible = false
	_menu.id_pressed.connect(_on_item)
	_menu.popup_hide.connect(_on_menu_hidden)
	_why_close.pressed.connect(_why.hide)
	_why.popup_hide.connect(_on_why_hidden)
	_capture_confirm.pressed.connect(_on_capture)
	_capture_name.text_submitted.connect(func(_t: String) -> void: _on_capture())
	_capture_cancel.pressed.connect(_capture.hide)
	for m: MarginContainer in [$Why/Margin, $Capture/Margin]:
		Ui.set_margins(m, 3)
	for b: BoxContainer in [$Why/Margin/Column, $Capture/Margin/Column, $Capture/Margin/Column/Row, _why_lines]:
		Ui.set_separation(b, 2)
	var editor := Kit.autoload(&"Editor")
	if editor != null and editor.has_signal(&"notes_changed"):
		editor.connect(&"notes_changed", _on_notes_changed)


## Close every popup this menu owns (a splash starting, the dock leaving).
func close() -> void:
	_menu.hide()
	_why.hide()
	_capture.hide()


# --- the dock menu --------------------------------------------------------------------------------

## Open on a dock pick result at canvas position `at`. `placing` adds Stop placing first.
func open_dock(at: Vector2, hit: Dictionary, placing: bool) -> void:
	_hit = hit
	_at = at
	_menu.clear()
	var kind := StringName(hit.get("kind", &"none"))
	if placing:
		_add(Item.STOP_PLACING, &"editor_shell.menu.stop_placing")
		_menu.add_separator()
	if kind == &"part":
		var part := _placed(int(hit.get("slot", -1)))
		if not part.is_empty():
			_menu.add_separator(_part_name(int(part.get("slot", -1))))
			_add(Item.WHY, &"editor_shell.menu.why")
			_add(Item.ROTATE, &"editor_shell.menu.rotate")
			_add(Item.FRAME_PART, &"editor_shell.menu.frame_part")
			_add(Item.COPY, &"editor_shell.menu.copy")
			if int(part.get("kit", -1)) >= 0:
				_add(Item.SELECT_KIT, &"editor_shell.menu.select_kit")
				_add(Item.REMOVE_KIT, &"editor_shell.menu.remove_kit")
			_add(Item.REMOVE_PART, &"editor_shell.menu.remove_part")
			_menu.add_separator()
	elif kind == &"deck" or kind == &"bulkhead":
		_add(Item.REMOVE_PLANE, &"editor_shell.menu.remove_deck" if kind == &"deck" else &"editor_shell.menu.remove_bulkhead")
		_menu.add_separator()
	if _selected_count() > 0:
		_add(Item.DELETE_SELECTION, &"editor_shell.menu.delete_selection")
		_add(Item.CAPTURE, &"editor_shell.menu.capture")
	_add(Item.PASTE, &"editor_shell.menu.paste")
	_add(Item.SELECT_ALL, &"editor_shell.menu.select_all")
	_add(Item.FRAME_ALL, &"editor_shell.menu.frame_all")
	_popup_menu()


## Open on a sortie world pick during a splash (`BpWorldView.pick`): course orders on the sea, Why? on a
## part of the own hull.
func open_splash(at: Vector2, hit: Dictionary) -> void:
	_hit = hit
	_at = at
	_menu.clear()
	match StringName(hit.get("kind", &"none")):
		&"sea":
			_add(Item.STEER, &"editor_shell.menu.steer")
			_add(Item.WAYPOINT, &"editor_shell.menu.waypoint")
		&"own":
			if int(hit.get("slot", -1)) >= 0:
				_add(Item.SPLASH_WHY, &"editor_shell.menu.why")
		_:
			return
	_popup_menu()


func _add(item: int, key: StringName) -> void:
	_menu.add_item(tr(key), item)


func _popup_menu() -> void:
	if _menu.item_count == 0:
		return
	_popup_at(_menu, _at)


## Show a popup with its top-left at a canvas position, kept inside the screen. Embedded popups (the
## project default) are placed in canvas units; a native popup window in screen pixels.
func _popup_at(win: Window, canvas: Vector2) -> void:
	win.reset_size()
	var size := Vector2(win.size)
	var bounds := get_viewport().get_visible_rect().size
	var p := Vector2(clampf(canvas.x, 0.0, maxf(bounds.x - size.x, 0.0)), clampf(canvas.y, 0.0, maxf(bounds.y - size.y, 0.0)))
	if not get_viewport().gui_embed_subwindows:
		p = get_viewport().get_screen_transform() * p + Vector2(get_window().position)
	win.popup(Rect2i(Vector2i(p), Vector2i(size)))
	_refit(win)


## Wrapped labels know their height only after a layout pass; fit the window to its contents then.
func _refit(win: Window) -> void:
	await get_tree().process_frame
	if is_instance_valid(win) and win.visible:
		win.size = Vector2i(win.get_contents_minimum_size())


func _on_menu_hidden() -> void:
	if not _why.visible and not _capture.visible:
		closed.emit()


func _on_item(id: int) -> void:
	var slot := int(_hit.get("slot", -1))
	var part := _placed(slot)
	match id:
		Item.STOP_PLACING:
			_editor_call(&"ghost_cancel")
		Item.WHY:
			show_why_dock(_at, slot)
		Item.ROTATE:
			if not part.is_empty() and not bool(_editor_call(&"move_part", [slot, part.get("cell", Vector3i.ZERO), posmod(int(part.get("yaw", 0)) + 1, 4), int(part.get("tilt", 0))])):
				_tell(&"editor_shell.rotate.blocked")
		Item.FRAME_PART:
			_editor_call(&"select_parts", [PackedInt32Array([slot]), false])
			frame_requested.emit(&"selection")
		Item.COPY:
			_editor_call(&"select_parts", [PackedInt32Array([slot]), false])
			_editor_call(&"copy_selection")
		Item.SELECT_KIT:
			_editor_call(&"select_kits", [PackedInt32Array([int(part.get("kit", -1))]), false])
		Item.REMOVE_KIT:
			_editor_call(&"remove_kit", [int(part.get("kit", -1))])
		Item.REMOVE_PART:
			_editor_call(&"remove_part", [slot])
		Item.REMOVE_PLANE:
			var axis := StringName(_hit.get("kind", &"deck"))
			_editor_call(&"remove_plane", [axis, int(_hit.get("plane", -1))])
		Item.DELETE_SELECTION:
			_editor_call(&"delete_selection")
		Item.CAPTURE:
			_open_capture()
		Item.PASTE:
			if not bool(_editor_call(&"paste")):
				_tell(&"editor_shell.paste.nothing")
		Item.SELECT_ALL:
			_editor_call(&"select_all")
		Item.FRAME_ALL:
			frame_requested.emit(&"all")
		Item.STEER, Item.WAYPOINT:
			var map := Vector2(_hit.get("map", Vector2.ZERO))
			Kit.call_on(&"Orders", &"set_course", [PackedVector2Array([map]), -1.0, id == Item.WAYPOINT])
		Item.SPLASH_WHY:
			_why_slot = slot
			Kit.call_on(&"Orders", &"query_why", [slot])


func _editor_call(method: StringName, args: Array = []) -> Variant:
	return Kit.call_on(&"Editor", method, args)


func _placed(slot: int) -> Dictionary:
	if slot < 0:
		return {}
	for p: Dictionary in Kit.dicts(_editor_call(&"parts")):
		if int(p.get("slot", -1)) == slot:
			return p
	return {}


func _selected_count() -> int:
	var parts: Variant = Kit.as_dict(_editor_call(&"selection")).get("parts", PackedInt32Array())
	return (parts as PackedInt32Array).size() if parts is PackedInt32Array else 0


## The part's label or generated name ("Gun 2"), from the design under edit.
func _part_name(slot: int) -> String:
	return Inspector.part_name(slot)


## Messages from the menu go to the Why? panel's title line when it is open, else to a short popup line.
func _tell(key: StringName) -> void:
	_why_title.text = tr(key)
	_clear_lines()
	_popup_why()


# --- Why? -----------------------------------------------------------------------------------------

## Why? on a placed part in the dock: the notes and validation issues that name it (L353, L188).
func show_why_dock(at: Vector2, slot: int) -> void:
	_at = at
	_why_slot = slot
	_fill_why_dock()
	_popup_why()


func _fill_why_dock() -> void:
	_why_title.text = tr(&"editor_shell.why.title").format([_part_name(_why_slot)])
	_clear_lines()
	var rows := 0
	for n: Dictionary in Kit.dicts(_editor_call(&"notes")):
		if int(n.get("part", -1)) != _why_slot:
			continue
		rows += 1
		_add_line(n.get("text", {}), -1, -1, int(n.get("index", -1)) if bool(n.get("has_fix", false)) else -1, n.get("fix_label", {}))
	for i: Dictionary in Kit.dicts(_editor_call(&"validation")):
		if int(i.get("part", -1)) != _why_slot:
			continue
		rows += 1
		_add_line(i.get("text", {}), -1, -1, -1, {})
	if rows == 0:
		_add_line(&"editor_shell.why.clear", -1, -1, -1, {})


## The simulation's Why? chain during a splash (`WhyDict`). Ignored unless it answers this menu's ask.
func show_why_reply(why: Dictionary) -> void:
	if int(why.get("slot", -2)) != _why_slot:
		return
	_why_title.text = tr(&"editor_shell.why.title").format([_part_name(_why_slot)])
	_clear_lines()
	var chain: Variant = why.get("chain", [])
	if chain is Array and not (chain as Array).is_empty():
		for link: Variant in chain as Array:
			if link is Dictionary:
				var l := link as Dictionary
				_add_line(l.get("text", {}), int(l.get("node", -1)), int(l.get("part", -1)), -1, {})
	else:
		_add_line(&"editor_shell.why.clear", -1, -1, -1, {})
	_popup_why()


func _clear_lines() -> void:
	for c: Node in _why_lines.get_children():
		_why_lines.remove_child(c)
		c.queue_free()


## One link of the chain: a sentence; a link button when it names a node or another part; a Fix preview
## button when the note has one.
func _add_line(text: Variant, node: int, part: int, fix_note: int, fix_label: Variant) -> void:
	var row := HBoxContainer.new()
	Ui.set_separation(row, 2)
	var sentence := _Text.resolve(text)
	if node >= 0 or (part >= 0 and part != _why_slot):
		var link := Button.new()
		link.text = sentence
		link.alignment = HORIZONTAL_ALIGNMENT_LEFT
		link.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		link.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
		link.custom_minimum_size = Vector2(0.0, 40.0)
		link.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		link.tooltip_text = tr(&"editor_shell.why.open_node") if node >= 0 else tr(&"editor_shell.why.open_part")
		link.pressed.connect(_on_link.bind(node, part))
		row.add_child(link)
	else:
		var l := Label.new()
		l.theme_type_variation = &"Body"
		l.text = sentence
		l.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
		l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		l.custom_minimum_size = Vector2(380.0, 0.0)
		l.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		row.add_child(l)
	if fix_note >= 0:
		var fix := Button.new()
		var label := _Text.resolve(fix_label)
		fix.text = label if not label.is_empty() else tr(&"editor_shell.why.preview_fix")
		fix.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		fix.custom_minimum_size = Vector2(0.0, 40.0)
		fix.size_flags_vertical = Control.SIZE_SHRINK_BEGIN
		fix.tooltip_text = tr(&"editor_shell.why.preview_fix_tip")
		fix.pressed.connect(_on_preview_fix.bind(fix_note))
		row.add_child(fix)
	_why_lines.add_child(row)


func _popup_why() -> void:
	_popup_at(_why, _at)
	_why_close.grab_focus.call_deferred()


func _on_link(node: int, part: int) -> void:
	_why.hide()
	why_link.emit(node, part)


func _on_preview_fix(note: int) -> void:
	_editor_call(&"note_preview", [note])
	_why.hide()


func _on_notes_changed() -> void:
	if _why.visible and _why_slot >= 0 and not bool(_editor_call(&"in_splash")):
		_fill_why_dock()
		_why.reset_size()


func _on_why_hidden() -> void:
	closed.emit()


# --- capture as kit -------------------------------------------------------------------------------

func _open_capture() -> void:
	_capture_name.text = ""
	_popup_at(_capture, _at)
	_capture_name.grab_focus.call_deferred()


func _on_capture() -> void:
	var made := Kit.as_dict(_editor_call(&"capture_prefab", [_capture_name.text.strip_edges()]))
	_capture.hide()
	if made.is_empty():
		Screens.open_message(&"editor_shell.capture.failed_title", &"editor_shell.capture.failed_body")
	else:
		Screens.toast({"key": &"editor_shell.capture.done", "args": [String(made.get("name", ""))]})
