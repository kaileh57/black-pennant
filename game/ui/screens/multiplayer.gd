extends Control
## Host or join: join code, address and key, display name, firewall notice before hosting.
## DESIGN.md L992-1003. Unit: ui-lobby.
## Reads and calls: Lobby.host, Lobby.firewall_notice, Lobby.check_join_code, Lobby.join_code, Lobby.join_address.
##
## A base screen from the main menu (`Screens.change(&"multiplayer")`). Boot opens it with
## `{ "join_code": String }` for `--join <code>` after it has already called `Lobby.join_code`, so
## that argument only fills the field and shows the joining state; this screen never joins twice.
##
## Layout, most used first: the player's display name (sent in the hello, no accounts, L1003), then
## Join (paste a join code: it is checked as it is typed and names the host's address and port, or
## the sentence that says what is wrong with it; or join by address:port and the 16-character lobby
## key), then Host (mode, PvP ruleset and tonnage cap, lobby name, port, upload speed). Hosting
## binds UDP 47610 only when the player chooses it, and the operating-system firewall prompt is
## explained before it can appear (L992): the notice sits above the Host button and, the first
## time in a run, Host asks once more with the notice as the dialog body.
## States: idle, joining (Cancel leaves), join failed (the refusal sentence, protocol mismatch names
## both versions), hosting (the lobby opens through `Screens` routing when the session reaches its
## lobby), host problems (each as a sentence under Host). Esc and Back return to the main menu.

const Kit := preload("res://ui/screens/menu_kit.gd")

## `Bp.MODE_*`, kept here so the script parses before the extension loads.
const MODE_CAMPAIGN := 0
const MODE_SKIRMISH := 1
const MODE_SANDBOX := 2
const MODE_PVP_ELIMINATION := 3
const MODE_PVP_ESCORT := 4
const HOST_MODES: Array[int] = [MODE_SANDBOX, MODE_SKIRMISH, MODE_PVP_ELIMINATION, MODE_PVP_ESCORT]
const DEFAULT_PORT := 47610
const PORT_MIN := 1024
const PORT_MAX := 65535
## Upload speed in Mbps: 1 Mbps by default (L1031); the facade clamps to 256 kbps .. 100 Mbps.
const UPLOAD_DEFAULT_MBPS := 1.0
const UPLOAD_MIN_MBPS := 0.3
const UPLOAD_MAX_MBPS := 100.0
## PvP tonnage caps are the act caps; 400 t is the default (L967).
const PVP_CAPS: Array[float] = [150.0, 400.0, 900.0, 1800.0]
const PVP_CAP_DEFAULT := 1
const NAME_MAX := 24
const LOBBY_KEY_LEN := 16
const COLUMN_W := 640.0

## The firewall notice was confirmed once this run.
static var firewall_seen := false

var _name_field: LineEdit
var _name_hint: Label
var _code_field: LineEdit
## The field `default_focus` returned, until its opening edit mode is stopped.
var _opening_field: LineEdit = null
var _code_status: Label
var _join_code: Button
var _address_field: LineEdit
var _key_field: LineEdit
var _join_address: Button
var _join_status: Label
var _cancel_join: Button
var _mode: OptionButton
var _ruleset: OptionButton
var _ruleset_row: Control
var _cap: OptionButton
var _cap_row: Control
var _lobby_name: LineEdit
var _port: SpinBox
var _upload: SpinBox
var _firewall: Label
var _host: Button
var _host_status: Label
var _back: Button
var _join_col: VBoxContainer
var _host_col: VBoxContainer

## `{ "id": String, ... }` RulesetDicts in the order offered.
var _rulesets: Array[Dictionary] = []
var _modes: Array[int] = []
var _state: StringName = &"idle"
var _code_ok := false
var _theme_started := false
var _connected := false


func _ready() -> void:
	accessibility_name = tr(&"mp.title")
	var backing := Panel.new()
	backing.set_anchors_preset(Control.PRESET_FULL_RECT)
	add_child(backing)
	var scroll := ScrollContainer.new()
	scroll.set_anchors_preset(Control.PRESET_FULL_RECT)
	scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	scroll.follow_focus = true
	add_child(scroll)
	var margin := MarginContainer.new()
	margin.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	margin.size_flags_vertical = Control.SIZE_EXPAND_FILL
	Ui.set_margins(margin, 5)
	scroll.add_child(margin)
	var column := Kit.vbox(4)
	margin.add_child(column)

	var header := Kit.hbox(4)
	var title := Kit.label("mp.title", &"ScreenTitle")
	title.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	header.add_child(title)
	_back = Kit.inline_button("mp.back")
	_back.custom_minimum_size = Vector2(200.0, Kit.BUTTON_H)
	_back.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	_back.pressed.connect(_leave)
	header.add_child(_back)
	column.add_child(header)
	column.add_child(HSeparator.new())

	_name_field = _line_edit("mp.name", NAME_MAX)
	_name_field.text_submitted.connect(func(_t: String) -> void: _store_name())
	_name_field.text_changed.connect(func(_t: String) -> void: _refresh_buttons())
	_name_field.focus_exited.connect(_store_name)
	var name_row := _row("mp.name", _name_field)
	var name_help := Kit.label("mp.name_help", &"Dense")
	name_help.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	name_help.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	name_row.add_child(name_help)
	column.add_child(name_row)
	_name_hint = Kit.label("mp.name_needed", &"Body")
	_name_hint.visible = false
	column.add_child(_name_hint)

	var flow := HFlowContainer.new()
	flow.add_theme_constant_override(&"h_separation", Ui.spacing(6))
	flow.add_theme_constant_override(&"v_separation", Ui.spacing(5))
	column.add_child(flow)
	_join_col = Kit.vbox(2)
	_join_col.custom_minimum_size = Vector2(COLUMN_W, 0.0)
	_join_col.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	flow.add_child(_join_col)
	_host_col = Kit.vbox(2)
	_host_col.custom_minimum_size = Vector2(COLUMN_W, 0.0)
	_host_col.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	flow.add_child(_host_col)
	_build_join()
	_build_host()
	_link_focus()
	_show_state(&"idle")
	_code_status.text = tr(&"mp.join.code_help")


func _build_join() -> void:
	_join_col.add_child(Kit.label("mp.join.title", &"PanelTitle"))
	_code_field = _line_edit("mp.join.code", 80)
	_code_field.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_code_field.text_changed.connect(func(_t: String) -> void: _check_code())
	_code_field.text_submitted.connect(func(_t: String) -> void: _on_join_code())
	_join_col.add_child(Kit.label("mp.join.code", &"Body"))
	_join_col.add_child(_code_field)
	_code_status = Kit.label("", &"Body", true)
	_join_col.add_child(_code_status)
	_join_code = Kit.button("mp.join.by_code", true)
	_join_code.pressed.connect(_on_join_code)
	_join_col.add_child(_join_code)

	var by_address := Kit.label("mp.join.address_title", &"PanelTitle")
	_join_col.add_child(by_address)
	_join_col.add_child(Kit.label("mp.join.address_help", &"Body"))
	_address_field = _line_edit("mp.join.address", 64)
	_address_field.text_changed.connect(func(_t: String) -> void: _refresh_buttons())
	_join_col.add_child(_row("mp.join.address", _address_field))
	_key_field = _line_edit("mp.join.key", 32)
	_key_field.text_changed.connect(func(_t: String) -> void: _refresh_buttons())
	_key_field.text_submitted.connect(func(_t: String) -> void: _on_join_address())
	_join_col.add_child(_row("mp.join.key", _key_field))
	_join_address = Kit.inline_button("mp.join.by_address")
	_join_address.pressed.connect(_on_join_address)
	_join_col.add_child(_join_address)
	_join_status = Kit.label("", &"Body", true)
	_join_status.visible = false
	_join_col.add_child(_join_status)
	_cancel_join = Kit.inline_button("mp.join.cancel")
	_cancel_join.visible = false
	_cancel_join.pressed.connect(_on_cancel_join)
	_join_col.add_child(_cancel_join)


func _build_host() -> void:
	_host_col.add_child(Kit.label("mp.host.title", &"PanelTitle"))
	_mode = _option()
	_mode.item_selected.connect(func(_i: int) -> void: _on_mode())
	_host_col.add_child(_row("mp.host.mode", _mode))
	_ruleset = _option()
	_ruleset_row = _row("mp.host.ruleset", _ruleset)
	_host_col.add_child(_ruleset_row)
	_cap = _option()
	for i in PVP_CAPS.size():
		_cap.add_item(Kit.number(PVP_CAPS[i], &"t", 0), i)
	_cap.select(PVP_CAP_DEFAULT)
	_cap_row = _row("mp.host.cap", _cap)
	_host_col.add_child(_cap_row)
	_lobby_name = _line_edit("mp.host.name", NAME_MAX)
	_host_col.add_child(_row("mp.host.name", _lobby_name))
	_port = _spin(PORT_MIN, PORT_MAX, 1.0, DEFAULT_PORT)
	_host_col.add_child(_row("mp.host.port", _port))
	_upload = _spin(UPLOAD_MIN_MBPS, UPLOAD_MAX_MBPS, 0.1, UPLOAD_DEFAULT_MBPS)
	_upload.suffix = tr(&"mp.host.mbps")
	var upload_row := _row("mp.host.upload", _upload)
	_host_col.add_child(upload_row)
	_host_col.add_child(Kit.label("mp.host.upload_help", &"Dense"))
	_firewall = Kit.label("", &"Body", true)
	_host_col.add_child(_firewall)
	_host = Kit.button("mp.host.start", true)
	_host.pressed.connect(_on_host)
	_host_col.add_child(_host)
	_host_status = Kit.label("", &"Body", true)
	_host_status.visible = false
	_host_col.add_child(_host_status)


func _line_edit(label_key: String, max_length: int) -> LineEdit:
	var e := LineEdit.new()
	e.max_length = max_length
	e.custom_minimum_size = Vector2(320.0, Kit.BUTTON_H)
	e.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	e.accessibility_name = tr(StringName(label_key))
	e.focus_mode = Control.FOCUS_ALL
	e.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	return e


func _option() -> OptionButton:
	var o := OptionButton.new()
	o.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	o.custom_minimum_size = Vector2(320.0, Kit.BUTTON_H)
	o.focus_mode = Control.FOCUS_ALL
	return o


func _spin(lo: float, hi: float, step: float, value: float) -> SpinBox:
	var s := SpinBox.new()
	s.min_value = lo
	s.max_value = hi
	s.step = step
	s.value = value
	s.custom_minimum_size = Vector2(200.0, Kit.BUTTON_H)
	s.select_all_on_focus = true
	return s


func _row(label_key: String, control: Control) -> HBoxContainer:
	var row := Kit.hbox(3)
	var l := Kit.label(label_key, &"Body")
	l.custom_minimum_size = Vector2(200.0, 0.0)
	l.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	l.autowrap_mode = TextServer.AUTOWRAP_OFF
	row.add_child(l)
	control.accessibility_name = tr(StringName(label_key))
	row.add_child(control)
	return row


# --- facade -------------------------------------------------------------------------------------

func setup(args: Dictionary) -> void:
	_theme_started = true
	Kit.start_menu_theme()
	_connect_lobby()
	var shown_name: Variant = Kit.call_on(&"Lobby", &"display_name")
	_name_field.text = String(shown_name) if shown_name is String else ""
	var raw_rules: Variant = Kit.call_on(&"Content", &"rulesets")
	var notice: Variant = Kit.call_on(&"Lobby", &"firewall_notice")
	var campaign: Variant = Kit.call_on(&"Campaign", &"is_active")
	var limits: Dictionary[int, int] = {}
	for m: int in [MODE_CAMPAIGN, MODE_SKIRMISH, MODE_SANDBOX, MODE_PVP_ELIMINATION, MODE_PVP_ESCORT]:
		limits[m] = Kit.get_int(Kit.as_dict(Kit.call_on(&"Content", &"mode_limits", [m])), "max_players", 0)
	var port: Variant = Kit.call_on(&"Settings", &"get_value", [&"net.port"])
	var upload: Variant = Kit.call_on(&"Settings", &"get_value", [&"net.upload_kbps"])
	fill(Kit.dicts(raw_rules), Kit.as_dict(notice), campaign == true, limits,
		int(port) if (port is int and int(port) >= PORT_MIN) else DEFAULT_PORT,
		float(upload) / 1000.0 if (upload is int or upload is float) and float(upload) > 0.0 else UPLOAD_DEFAULT_MBPS)
	var code := String(args.get("join_code", ""))
	if not code.is_empty():
		_code_field.text = code
		_check_code()
		if _code_ok:
			_show_joining(_code_target())


func _connect_lobby() -> void:
	if _connected:
		return
	var lobby := Kit.autoload(&"Lobby")
	if lobby == null:
		return
	_connected = true
	for pair: Array in [[&"joined", _on_joined], [&"join_failed", _on_join_failed], [&"left", _on_left]]:
		if lobby.has_signal(pair[0]) and not lobby.is_connected(pair[0], pair[1]):
			lobby.connect(pair[0], pair[1])


func _exit_tree() -> void:
	Kit.leave_menu_theme(_theme_started)


func default_focus() -> Control:
	var field := _code_field if not _name_field.text.strip_edges().is_empty() else _name_field
	# The screen opens on the field without edit mode, so one Esc (or pad B) leaves; Enter, a click or pad A
	# starts editing. Godot starts editing when a LineEdit takes focus, so stop it once that has happened.
	_opening_field = field
	_stop_opening_edit.call_deferred(2)
	return field


## Leave edit mode on the field `default_focus` handed out, once its deferred focus has landed.
func _stop_opening_edit(frames: int) -> void:
	var field := _opening_field
	if field == null or not is_instance_valid(field):
		return
	if not field.has_focus() and frames > 0:
		await get_tree().process_frame
		_stop_opening_edit(frames - 1)
		return
	_opening_field = null
	if field.has_focus() and field.is_editing():
		field.unedit()


func _unhandled_input(event: InputEvent) -> void:
	if event.is_action_pressed(&"ui_cancel", false, true) and Screens.top() == self:
		get_viewport().set_input_as_handled()
		_leave()


## Fill the host choices from plain data (also the test and capture entry point). `limits` maps
## `Bp.MODE_*` to the release's player maximum; modes under 2 players are not offered.
func fill(rulesets: Array[Dictionary], firewall_notice: Dictionary, campaign_loaded: bool, limits: Dictionary, port: int, upload_mbps: float) -> void:
	_rulesets = rulesets
	_modes = []
	if campaign_loaded and int(limits.get(MODE_CAMPAIGN, 2)) >= 2:
		_modes.append(MODE_CAMPAIGN)
	for m: int in HOST_MODES:
		if int(limits.get(m, 2)) >= 2:
			_modes.append(m)
	_mode.clear()
	for i in _modes.size():
		_mode.add_item(mode_name(_modes[i]), i)
	if not _modes.is_empty():
		_mode.select(0)
	_ruleset.clear()
	for i in _rulesets.size():
		_ruleset.add_item(ruleset_name(_rulesets[i]), i)
	if not _rulesets.is_empty():
		_ruleset.select(0)
	_firewall.text = Kit._Text.resolve(firewall_notice)
	_firewall.visible = not _firewall.text.is_empty()
	_port.value = clampi(port, PORT_MIN, PORT_MAX)
	_upload.value = clampf(upload_mbps, UPLOAD_MIN_MBPS, UPLOAD_MAX_MBPS)
	_on_mode()
	_refresh_buttons()


static func mode_name(mode: int) -> String:
	if mode == MODE_CAMPAIGN:
		return Kit.tr_key(&"mp.mode.coop")
	var w: Variant = Kit.call_on(&"Tx", &"word", [&"mode", mode])
	if w is String and not (w as String).is_empty():
		return w
	return Kit.tr_key(StringName("mp.mode.%d" % mode))


## A ruleset's name: the two known PvP rulesets by word, any other by its id.
static func ruleset_name(r: Dictionary) -> String:
	var id := Kit.get_str(r, "id")
	var key := StringName("mp.ruleset." + id)
	var text := Kit.tr_key(key)
	return text if text != String(key) else id


func _selected_mode() -> int:
	var i := _mode.get_selected_id()
	return _modes[i] if i >= 0 and i < _modes.size() else MODE_SANDBOX


func _on_mode() -> void:
	var pvp := _selected_mode() == MODE_PVP_ELIMINATION or _selected_mode() == MODE_PVP_ESCORT
	_ruleset_row.visible = pvp and not _rulesets.is_empty()
	_cap_row.visible = pvp
	_link_focus()


# --- joining ------------------------------------------------------------------------------------

func _check_code() -> void:
	var code := _code_field.text.strip_edges()
	if code.is_empty():
		_code_ok = false
		_code_status.text = tr(&"mp.join.code_help")
		_refresh_buttons()
		return
	var res := Kit.as_dict(Kit.call_on(&"Lobby", &"check_join_code", [code]))
	show_code_check(res)


## Show the result of `Lobby.check_join_code` (also the capture entry point).
func show_code_check(res: Dictionary) -> void:
	_code_ok = Kit.get_bool(res, "valid")
	if _code_ok:
		_code_status.text = tr(&"mp.join.code_ok").format([Kit.get_str(res, "address"), Kit.get_int(res, "port")])
	else:
		var why := Kit._Text.resolve(res.get("text", {}))
		_code_status.text = why if not why.is_empty() else tr(&"mp.join.code_bad")
	_refresh_buttons()


func _code_target() -> String:
	var res := Kit.as_dict(Kit.call_on(&"Lobby", &"check_join_code", [_code_field.text.strip_edges()]))
	return "%s:%d" % [Kit.get_str(res, "address"), Kit.get_int(res, "port")]


func _on_join_code() -> void:
	if _state == &"joining" or _state == &"hosting" or not _code_ok:
		return
	_store_name()
	var target := _code_target()
	var ok: Variant = Kit.call_on(&"Lobby", &"join_code", [_code_field.text.strip_edges()])
	if ok == true:
		_show_joining(target)
	else:
		show_join_failed(tr(&"mp.join.code_bad"))


func _on_join_address() -> void:
	if _state == &"joining" or _state == &"hosting" or _join_address.disabled:
		return
	_store_name()
	var address := _address_field.text.strip_edges()
	var key := _key_field.text.strip_edges().replace(" ", "").replace("-", "")
	var ok: Variant = Kit.call_on(&"Lobby", &"join_address", [address, key])
	if ok == true:
		_show_joining(address)
	else:
		show_join_failed(tr(&"mp.join.address_bad"))


func _show_joining(target: String) -> void:
	_join_status.text = tr(&"mp.join.connecting").format([target])
	_show_state(&"joining")


## The join did not work: `text` is the sentence already resolved (also the capture entry point).
func show_join_failed(text: String) -> void:
	_join_status.text = text
	_show_state(&"join_failed")


func _on_cancel_join() -> void:
	Kit.call_on(&"Lobby", &"leave")
	_show_state(&"idle")
	Ui.focus(_code_field)


func _on_joined() -> void:
	if not is_inside_tree():
		return
	# The session's lobby state routes to the lobby; this covers a router that has not started.
	if Screens.base_name() == &"multiplayer":
		Screens.change(&"lobby")


func _on_join_failed(text: Dictionary) -> void:
	if not is_inside_tree():
		return
	var why := Kit._Text.resolve(text)
	if _state == &"hosting":
		_show_host_problems(PackedStringArray([why if not why.is_empty() else tr(&"mp.host.failed")]))
	else:
		show_join_failed(why if not why.is_empty() else tr(&"mp.join.failed"))


func _on_left(text: Dictionary) -> void:
	if not is_inside_tree() or (_state != &"joining" and _state != &"hosting"):
		return
	var why := Kit._Text.resolve(text)
	if _state == &"hosting":
		_show_host_problems(PackedStringArray([why]) if not why.is_empty() else PackedStringArray())
		if why.is_empty():
			_show_state(&"idle")
	else:
		if why.is_empty():
			_show_state(&"idle")
		else:
			show_join_failed(why)


# --- hosting ------------------------------------------------------------------------------------

func host_options() -> Dictionary:
	var mode := _selected_mode()
	var out := {
		"mode": mode,
		"name": _lobby_name.text.strip_edges(),
		"port": int(_port.value),
		"upload_kbps": roundi(_upload.value * 1000.0),
	}
	if mode == MODE_CAMPAIGN:
		out["campaign"] = true
	if mode == MODE_PVP_ELIMINATION or mode == MODE_PVP_ESCORT:
		var r := _ruleset.get_selected_id()
		if r >= 0 and r < _rulesets.size():
			out["ruleset"] = Kit.get_str(_rulesets[r], "id")
		out["tonnage_cap_t"] = PVP_CAPS[clampi(_cap.get_selected_id(), 0, PVP_CAPS.size() - 1)]
	return out


func _on_host() -> void:
	if _state == &"joining" or _state == &"hosting" or _modes.is_empty():
		return
	_store_name()
	if firewall_seen or _firewall.text.is_empty():
		_host_now()
		return
	var dialog := Screens.open_dialog(&"mp.host.firewall_title", _firewall.text,
		[{"id": &"cancel", "label": &"mp.host.firewall_cancel"}, {"id": &"host", "label": &"mp.host.firewall_host"}], &"cancel", &"host")
	if dialog != null:
		dialog.connect(&"chosen", _on_firewall_choice)


func _on_firewall_choice(id: StringName) -> void:
	if id == &"host" and is_inside_tree():
		firewall_seen = true
		_host_now()


func _host_now() -> void:
	_host_status.visible = false
	var result: Variant = Kit.call_on(&"Lobby", &"host", [host_options()])
	var problems := Kit.problem_texts(result, &"mp.host.failed")
	if problems.is_empty():
		_host_status.text = tr(&"mp.host.opening")
		_show_state(&"hosting")
		# Hosting admits the host's own client a frame later; the session's lobby state routes on.
		if bool(Kit.call_on(&"Lobby", &"is_active")) and Screens.base_name() == &"multiplayer":
			_on_joined.call_deferred()
		return
	_show_host_problems(problems)


func _show_host_problems(problems: PackedStringArray) -> void:
	_show_state(&"idle")
	_host_status.text = Kit.problems_body(problems)
	_host_status.visible = not problems.is_empty()


# --- state --------------------------------------------------------------------------------------

func _show_state(state: StringName) -> void:
	_state = state
	var busy := state == &"joining" or state == &"hosting"
	_join_status.visible = state == &"joining" or state == &"join_failed"
	_cancel_join.visible = state == &"joining"
	for c: Control in [_code_field, _address_field, _key_field, _name_field, _lobby_name]:
		(c as LineEdit).editable = not busy
	for c: Control in [_mode, _ruleset, _cap]:
		(c as OptionButton).disabled = busy
	_port.editable = not busy
	_upload.editable = not busy
	if state == &"hosting":
		_host_status.visible = true
	_refresh_buttons()
	if state == &"joining":
		Ui.focus(_cancel_join)


func _refresh_buttons() -> void:
	var busy := _state == &"joining" or _state == &"hosting"
	# Other players see only this name, so a lobby needs one (L1003).
	var named := not _name_field.text.strip_edges().is_empty()
	_name_hint.visible = not named
	_join_code.disabled = busy or not _code_ok or not named
	var key := _key_field.text.strip_edges().replace(" ", "").replace("-", "")
	_join_address.disabled = busy or _address_field.text.strip_edges().is_empty() or key.length() != LOBBY_KEY_LEN or not named
	_host.disabled = busy or _modes.is_empty() or not named
	_link_focus()


func _store_name() -> void:
	var n := _name_field.text.strip_edges()
	if not n.is_empty():
		Kit.call_on(&"Lobby", &"set_display_name", [n])


func _leave() -> void:
	if _state == &"joining":
		Kit.call_on(&"Lobby", &"leave")
	Screens.change(&"main_menu")


func _link_focus() -> void:
	if _host == null:
		return
	var left := Kit.chain_vertical([_name_field, _code_field, _join_code, _address_field, _key_field, _join_address, _cancel_join])
	var right := Kit.chain_vertical([_mode, _ruleset, _cap, _lobby_name, _port.get_line_edit(), _upload.get_line_edit(), _host])
	for c: Control in left:
		c.focus_neighbor_left = c.get_path_to(c)
		if not right.is_empty():
			c.focus_neighbor_right = c.get_path_to(right[0])
	for c: Control in right:
		c.focus_neighbor_right = c.get_path_to(c)
		c.focus_neighbor_left = c.get_path_to(_code_field)
	if not left.is_empty():
		left[0].focus_neighbor_top = left[0].get_path_to(_back)
	if not right.is_empty():
		right[0].focus_neighbor_top = right[0].get_path_to(_back)
	_back.focus_neighbor_bottom = _back.get_path_to(_name_field)
	_back.focus_neighbor_top = _back.get_path_to(_back)
