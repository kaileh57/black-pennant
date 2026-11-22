extends Control
## Bindings page: every action per device, rebind, unbind, reset, conflict prompt.
## DESIGN.md L1719. Unit: ui-settings.
## Reads and calls: Settings.bindings, Settings.rebind, Settings.reset_bindings.
##
## Pushed over Settings. One row per rebindable action, grouped as `Settings.bindings()` groups
## them (sortie, dry dock, menus), with two slots per device. Every action is rebindable on both
## devices, Esc included (L1719 floor 8); dry-dock actions take no gamepad binding (L58) and say so.
##
## Rebinding: press a slot, then the new key, mouse button, gamepad button or stick direction.
## The capture runs in `_input`, before any control sees the event, so Esc, Enter, a click or the
## gamepad's accept button can all be bound. Listening stops after LISTEN_S seconds or on input from
## the other device. A lone modifier binds on release, so Shift then W is read as a chord (refused,
## L1719) instead of binding Shift. A clash inside a group opens a dialog: move the binding (the
## other action loses it) or keep the old one.
## Clearing: Delete or Backspace on a focused slot, a right-click, or the gamepad's `ui_select`
## button. Both reset buttons ask first.

const Kit := preload("res://ui/screens/menu_kit.gd")

const LISTEN_S := 5
## Slot width: wide enough for "Shift+Middle click" and "Right stick press" where the window has
## room, narrower when a large UI scale leaves little (L1717 floor 2: abbreviation allowed at 200%).
const SLOT_W := 200.0
const SLOT_W_NARROW := 160.0
## Canvas width below which slots narrow so the action names keep a readable column.
const NARROW_BELOW_PX := 1500.0
const DEVICES: Array[StringName] = [&"keyboard", &"gamepad"]
const SLOTS := 2
## Stick movement that counts as a press while listening for a gamepad binding.
const AXIS_PRESS := 0.6

@onready var _title: Label = $Margin/Column/Header/Title
@onready var _reset_keyboard: Button = $Margin/Column/Header/ResetKeyboard
@onready var _reset_gamepad: Button = $Margin/Column/Header/ResetGamepad
@onready var _close: Button = $Margin/Column/Header/Close
@onready var _status: Label = $Margin/Column/Status
@onready var _list: VBoxContainer = $Margin/Column/Scroll/List
@onready var _timer: Timer = $Listen

var _rows: Array[Dictionary] = []
## Slot buttons by "<action>/<device>/<index>".
var _slots: Dictionary[String, Button] = {}
## The slot being listened for: `{ "action", "device", "index", "label" }`, or empty.
var _listening: Dictionary = {}
var _listen_left := 0
var _pending_modifier: InputEventKey
var _loaded := false
var _slot_w := SLOT_W


func _ready() -> void:
	_slot_w = _slot_width_for(get_viewport_rect().size.x)
	resized.connect(_fit_slots)
	Ui.set_margins($Margin, 5)
	Ui.set_separation($Margin/Column, 3)
	Ui.set_separation($Margin/Column/Header, 3)
	Ui.set_separation($Margin/Column/Columns, 5)
	Ui.set_separation(_list, 1)
	var device_w := _slot_w * SLOTS + Ui.spacing(1) * (SLOTS - 1)
	$Margin/Column/Columns/Keyboard.custom_minimum_size.x = device_w
	$Margin/Column/Columns/Gamepad.custom_minimum_size.x = device_w
	_status.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_status.text = ""
	_close.pressed.connect(_leave)
	_reset_keyboard.pressed.connect(_confirm_reset.bind(&"keyboard"))
	_reset_gamepad.pressed.connect(_confirm_reset.bind(&"gamepad"))
	Kit.chain_horizontal([_reset_keyboard, _reset_gamepad, _close])
	_timer.wait_time = 1.0
	_timer.one_shot = false
	_timer.timeout.connect(_on_listen_tick)
	accessibility_name = tr(&"bindings.title")
	var settings := Kit.autoload(&"Settings")
	if settings != null and settings.has_signal(&"bindings_changed"):
		settings.connect(&"bindings_changed", _on_bindings_changed)


static func _slot_width_for(width_px: float) -> float:
	return SLOT_W if width_px >= NARROW_BELOW_PX else SLOT_W_NARROW


## Narrow or widen every slot when the page's own width crosses NARROW_BELOW_PX.
func _fit_slots() -> void:
	var w := _slot_width_for(size.x)
	if is_equal_approx(w, _slot_w):
		return
	_slot_w = w
	var device_w := _slot_w * SLOTS + Ui.spacing(1) * (SLOTS - 1)
	$Margin/Column/Columns/Keyboard.custom_minimum_size.x = device_w
	$Margin/Column/Columns/Gamepad.custom_minimum_size.x = device_w
	for b: Button in _slots.values():
		if is_instance_valid(b):
			b.custom_minimum_size.x = _slot_w
	for row: Node in _list.get_children():
		for pair: Node in row.get_children():
			for child: Node in pair.get_children():
				if child is Label and not (child is Button) and (child as Label).text == "bindings.keyboard_only":
					(child as Label).custom_minimum_size.x = device_w


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and _loaded:
		accessibility_name = tr(&"bindings.title")
		_rebuild(true)


func setup(_args: Dictionary) -> void:
	_loaded = true
	_rebuild()


func default_focus() -> Control:
	var first := _first_slot()
	return first if first != null else _close


func back_requested() -> void:
	if not _listening.is_empty():
		_stop_listening(&"bindings.listen.cancelled")
		return
	_leave()


func _leave() -> void:
	_stop_listening(&"")
	if Screens.top() == self and Screens.depth() > 1:
		Screens.pop()
	else:
		Screens.change(&"main_menu")


# --- the list ------------------------------------------------------------------------------------

## Read the bindings again. When the actions are the same (every rebind, clear and reset), the
## slots are relabelled in place, so focus stays on the slot the player was using; a different
## action list (first open, a locale change) builds the list anew.
func _rebuild(force: bool = false) -> void:
	var rows := Kit.dicts(Kit.call_on(&"Settings", &"bindings"))
	if not force and not _slots.is_empty() and _same_layout(rows):
		_rows = rows
		_refresh_slots()
		return
	var owner_now := get_viewport().gui_get_focus_owner() if is_inside_tree() else null
	var keep := ""
	for k: String in _slots:
		if _slots[k] == owner_now:
			keep = k
	_rows = rows
	_slots.clear()
	Kit.clear(_list)
	if _rows.is_empty():
		_list.add_child(Kit.label("bindings.empty", &"Body"))
		_link_focus()
		return
	var group := &""
	for row: Dictionary in _rows:
		var g := StringName(Kit.get_str(row, "group"))
		if g != group:
			group = g
			var heading := Kit.label("settings.group." + String(g), &"PanelTitle")
			_list.add_child(heading)
		_list.add_child(_build_row(row))
	_link_focus()
	if not keep.is_empty() and _slots.has(keep):
		Ui.focus(_slots[keep])


func _same_layout(rows: Array[Dictionary]) -> bool:
	if rows.size() != _rows.size():
		return false
	for i in rows.size():
		if Kit.get_str(rows[i], "action") != Kit.get_str(_rows[i], "action"):
			return false
		if Kit.get_bool(rows[i], "gamepad_allowed", true) != Kit.get_bool(_rows[i], "gamepad_allowed", true):
			return false
	return true


func _refresh_slots() -> void:
	for row: Dictionary in _rows:
		var action := StringName(Kit.get_str(row, "action"))
		var action_text := tr(StringName(Kit.get_str(row, "label", "settings.action." + String(action))))
		for device: StringName in DEVICES:
			var events := Kit.as_array(row.get(String(device)))
			for i in SLOTS:
				var key := "%s/%s/%d" % [action, device, i]
				if _slots.has(key):
					_label_slot(_slots[key], device, i, events[i] if i < events.size() else null, action_text)


func _build_row(row: Dictionary) -> HBoxContainer:
	var action := StringName(Kit.get_str(row, "action"))
	var box := Kit.hbox(5)
	var name_label := Kit.label(Kit.get_str(row, "label", "settings.action." + String(action)), &"Body")
	name_label.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	name_label.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	box.add_child(name_label)
	for device: StringName in DEVICES:
		var pair := Kit.hbox(1)
		var allowed := device == &"keyboard" or Kit.get_bool(row, "gamepad_allowed", true)
		if not allowed:
			var only := Kit.label("bindings.keyboard_only", &"Body")
			only.custom_minimum_size = Vector2(_slot_w * SLOTS + Ui.spacing(1) * (SLOTS - 1), 0.0)
			only.size_flags_vertical = Control.SIZE_SHRINK_CENTER
			pair.add_child(only)
		else:
			var events := Kit.as_array(row.get(String(device)))
			for i in SLOTS:
				var b := _slot_button(action, device, i, events[i] if i < events.size() else null, tr(StringName(name_label.text)))
				pair.add_child(b)
		box.add_child(pair)
	return box


func _slot_button(action: StringName, device: StringName, index: int, event: Variant, action_text: String) -> Button:
	var b := Button.new()
	b.custom_minimum_size = Vector2(_slot_w, Kit.BUTTON_H)
	b.focus_mode = Control.FOCUS_ALL
	b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	b.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
	b.clip_text = true
	_label_slot(b, device, index, event, action_text)
	b.pressed.connect(_listen.bind(action, device, index, action_text))
	b.gui_input.connect(_on_slot_input.bind(action, device, index))
	_slots["%s/%s/%d" % [action, device, index]] = b
	return b


func _label_slot(b: Button, device: StringName, index: int, event: Variant, action_text: String) -> void:
	var text := ""
	var stem := ""
	if event is InputEvent:
		var t: Variant = Kit.call_on(&"Settings", &"event_text", [event])
		text = String(t) if t is String else ""
		var g: Variant = Kit.call_on(&"Settings", &"event_glyph", [event])
		stem = String(g) if g is String else ""
	b.text = text if not text.is_empty() else tr(&"settings.binding.none")
	b.set_meta(&"bound", not text.is_empty())
	var device_text := tr(StringName("settings.device." + String(device)))
	b.accessibility_name = tr(&"bindings.slot.access").format([action_text, device_text, index + 1, b.text])
	b.tooltip_text = b.accessibility_name
	# Gamepad slots lead with the glyph the shortcut hints use; a stick or D-pad slot then needs only
	# the direction, so "Left stick down" is not cut to "Left stick do...".
	b.icon = Ui.icon(StringName(stem)) if not stem.is_empty() else null
	if not stem.is_empty() and event is InputEvent:
		var short := _pad_direction(event as InputEvent, stem)
		if not short.is_empty():
			b.text = short


## The direction word for a stick or D-pad binding, or "" for other pad inputs.
func _pad_direction(event: InputEvent, stem: String) -> String:
	if event is InputEventJoypadMotion and stem.begins_with("pad_stick_"):
		var m := event as InputEventJoypadMotion
		var vertical := m.axis == JOY_AXIS_LEFT_Y or m.axis == JOY_AXIS_RIGHT_Y
		var positive := m.axis_value > 0.0
		if vertical:
			return tr(&"bindings.dir.down") if positive else tr(&"bindings.dir.up")
		return tr(&"bindings.dir.right") if positive else tr(&"bindings.dir.left")
	if stem.begins_with("pad_dpad_"):
		return tr(StringName("bindings.dir." + stem.trim_prefix("pad_dpad_")))
	if stem.ends_with("_press"):
		return tr(&"bindings.dir.press")
	return ""


func _link_focus() -> void:
	var header: Array[Control] = [_reset_keyboard, _reset_gamepad, _close]
	# One column of slots per device and index, Up and Down along it; Left and Right across a row.
	var columns: Array[Array] = []
	for d in DEVICES.size() * SLOTS:
		columns.append([])
	for row: Dictionary in _rows:
		var action := Kit.get_str(row, "action")
		var across: Array[Control] = []
		for di in DEVICES.size():
			for i in SLOTS:
				var key := "%s/%s/%d" % [action, DEVICES[di], i]
				if _slots.has(key):
					columns[di * SLOTS + i].append(_slots[key])
					across.append(_slots[key])
		Kit.chain_horizontal(across)
	for column: Array in columns:
		var live := Kit.chain_vertical(column)
		if not live.is_empty():
			live[0].focus_neighbor_top = live[0].get_path_to(_close)
	var first := _first_slot()
	for b: Control in header:
		b.focus_neighbor_bottom = b.get_path_to(first if first != null else b)
		b.focus_neighbor_top = b.get_path_to(b)


func _first_slot() -> Button:
	for row: Dictionary in _rows:
		var key := "%s/keyboard/0" % Kit.get_str(row, "action")
		if _slots.has(key):
			return _slots[key]
	return null


func _on_bindings_changed() -> void:
	if _loaded and is_inside_tree():
		_rebuild()


# --- clearing ------------------------------------------------------------------------------------

func _on_slot_input(event: InputEvent, action: StringName, device: StringName, index: int) -> void:
	if not _listening.is_empty() or not event.is_pressed() or event.is_echo():
		return
	var clear := false
	if event is InputEventKey:
		var k := (event as InputEventKey).physical_keycode
		clear = k == KEY_DELETE or k == KEY_BACKSPACE
	elif event is InputEventMouseButton:
		clear = (event as InputEventMouseButton).button_index == MOUSE_BUTTON_RIGHT
	elif event is InputEventJoypadButton:
		clear = event.is_action_pressed(&"ui_select")
	if not clear:
		return
	accept_event()
	var key := "%s/%s/%d" % [action, device, index]
	if not _slots.has(key) or not bool(_slots[key].get_meta(&"bound", false)):
		_set_status(tr(&"bindings.clear.nothing"))
		return
	var ok: Variant = Kit.call_on(&"Settings", &"unbind", [action, device, index])
	_set_status(tr(&"bindings.clear.done") if ok == true else tr(&"bindings.clear.failed"))


# --- listening -----------------------------------------------------------------------------------

func _listen(action: StringName, device: StringName, index: int, action_text: String) -> void:
	_listening = {"action": action, "device": device, "index": index, "label": action_text}
	_listen_left = LISTEN_S
	_pending_modifier = null
	var key := "%s/%s/%d" % [action, device, index]
	if _slots.has(key):
		_slots[key].text = tr(&"bindings.listen.slot")
	_show_listen_prompt()
	_timer.start()


func _show_listen_prompt() -> void:
	var prompt := &"bindings.listen.keyboard" if _listening.get("device") == &"keyboard" else &"bindings.listen.gamepad"
	_set_status(tr(prompt).format([String(_listening.get("label", "")), _listen_left]))


func _on_listen_tick() -> void:
	if _listening.is_empty():
		_timer.stop()
		return
	_listen_left -= 1
	if _listen_left <= 0:
		_stop_listening(&"bindings.listen.timed_out")
	else:
		_show_listen_prompt()


func _stop_listening(status_key: StringName) -> void:
	var was := _listening
	_listening = {}
	_pending_modifier = null
	_timer.stop()
	if status_key != &"":
		_set_status(tr(status_key))
	if not was.is_empty() and is_inside_tree():
		_rebuild()


func _input(event: InputEvent) -> void:
	if _listening.is_empty():
		return
	if event is InputEventMouseMotion:
		return
	var device: StringName = _listening["device"]
	var on_gamepad := event is InputEventJoypadButton or event is InputEventJoypadMotion
	if event is InputEventJoypadMotion and absf((event as InputEventJoypadMotion).axis_value) < AXIS_PRESS:
		get_viewport().set_input_as_handled()
		return
	if (device == &"gamepad") != on_gamepad:
		# A press on the other device stops listening instead of binding.
		if event.is_pressed() and not event.is_echo():
			get_viewport().set_input_as_handled()
			_stop_listening(&"bindings.listen.cancelled")
		return
	get_viewport().set_input_as_handled()
	if event is InputEventKey:
		var key := event as InputEventKey
		if key.is_echo():
			return
		if _is_modifier(key.physical_keycode if key.physical_keycode != KEY_NONE else key.keycode):
			if key.pressed:
				_pending_modifier = key.duplicate() as InputEventKey
			elif _pending_modifier != null:
				var lone := _pending_modifier
				_pending_modifier = null
				_try_bind(lone, false)
			return
		if not key.pressed:
			return
		_pending_modifier = null
		_try_bind(key, false)
		return
	if not event.is_pressed() and not event is InputEventJoypadMotion:
		return
	_try_bind(event, false)


static func _is_modifier(code: Key) -> bool:
	return code == KEY_SHIFT or code == KEY_CTRL or code == KEY_ALT or code == KEY_META


func _try_bind(event: InputEvent, replace: bool) -> void:
	var target := _listening.duplicate()
	if target.is_empty():
		return
	_listening = {}
	_timer.stop()
	var result := Kit.as_dict(Kit.call_on(&"Settings", &"rebind", [target["action"], target["device"], target["index"], event, replace]))
	var text := Kit._Text.resolve(result.get("text", {}))
	if result.is_empty():
		_set_status(tr(&"bindings.failed"))
		_rebuild()
		return
	if Kit.get_bool(result, "ok"):
		_set_status(text)
		_rebuild()
		return
	var conflict := StringName(Kit.get_str(result, "conflict"))
	if conflict != &"" and not replace:
		_ask_replace(target, event, result.get("text", {}))
		return
	_set_status(text if not text.is_empty() else tr(&"bindings.failed"))
	_rebuild()


func _ask_replace(target: Dictionary, event: InputEvent, text: Variant) -> void:
	var dialog := Screens.open_dialog(text, &"bindings.conflict.body", [
		{"id": &"move", "label": &"bindings.conflict.move"},
		{"id": &"keep", "label": &"bindings.conflict.keep"},
	], &"keep", &"keep")
	if dialog == null:
		_rebuild()
		return
	dialog.connect(&"chosen", _on_replace_chosen.bind(target, event), CONNECT_ONE_SHOT)


func _on_replace_chosen(id: StringName, target: Dictionary, event: InputEvent) -> void:
	if id == &"move":
		_listening = target
		_try_bind(event, true)
	else:
		_set_status(tr(&"bindings.conflict.kept"))
		_rebuild()


# --- reset ---------------------------------------------------------------------------------------

func _confirm_reset(device: StringName) -> void:
	_stop_listening(&"")
	var title := &"bindings.reset.keyboard_title" if device == &"keyboard" else &"bindings.reset.gamepad_title"
	var dialog := Screens.open_dialog(title, &"bindings.reset.body", [
		{"id": &"reset", "label": &"bindings.reset.confirm"},
		{"id": &"keep", "label": &"bindings.reset.keep"},
	], &"keep", &"keep")
	if dialog != null:
		dialog.connect(&"chosen", _on_reset_chosen.bind(device), CONNECT_ONE_SHOT)


func _on_reset_chosen(id: StringName, device: StringName) -> void:
	if id != &"reset":
		return
	Kit.call_on(&"Settings", &"reset_bindings", [device])
	_set_status(tr(&"bindings.reset.done"))
	_rebuild()


func _set_status(text: String) -> void:
	_status.text = text
	_status.accessibility_description = text
