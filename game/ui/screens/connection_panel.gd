extends Control
## Connection panel visible to everyone: ping, host tick rate, frames behind, drop warnings, re-host.
## DESIGN.md L1033-1039, L1005-1007. Unit: ui-lobby.
## Reads and calls: Session.connections, Session.host_tick_hz, Session.host_lost, Session.rehost, Lobby.connection_log_text.
##
## Every player sees every player's ping, packet loss, frames behind (snapshots behind the newest
## one sent) and bandwidth quality, and the host's simulation tick rate (L1025, L1033). A warning
## shows on the row after 5 s of poor connection, before the host drops it at 15 s (L1035); a
## dropped co-op player's row says their officers keep their hull and how long the 5 minute rejoin
## hold has left (L838). State is never hue alone: each warning row carries the caution glyph and
## the sentence. When no packet has come from the host for 10 s (`Session.host_lost`), a notice
## takes the top: the first member on the succession list re-hosts automatically; when nobody's
## mapping worked, each member gets Re-host, which shows the new join code to share by hand (L1005).
## Copy log puts the local connection log (drops, warnings, ladder steps, mapping results) on the
## clipboard for a playtest report (L1039).
## Embedded as a lobby tab (`lobby.gd` calls `bind()`), or pushed over the sortie
## (`Screens.push(&"connection_panel")`, which calls `setup` and shows Close).
## `show_rows(rows, tick_hz)` renders plain data (`Array[ConnectionDict]`) for tests and captures.

const Kit := preload("res://ui/screens/menu_kit.gd")

## `Bp.CONN_*`, `Bp.NET_QUALITY_*`.
const CONN_CLOSED := 0
const CONN_CONNECTING := 1
const CONN_CONNECTED := 2
const CONN_WARNING := 3
const CONN_DROPPED := 4
const CONN_REFUSED := 5
const QUALITY_WORDS: Array[StringName] = [&"conn.quality.good", &"conn.quality.fair", &"conn.quality.poor"]
const STATE_WORDS: Array[StringName] = [&"conn.state.closed", &"conn.state.connecting", &"conn.state.connected", &"conn.state.warning", &"conn.state.dropped", &"conn.state.refused"]
const COL_NAME := 0
const COL_PING := 1
const COL_LOSS := 2
const COL_BEHIND := 3
const COL_QUALITY := 4
const COL_STATE := 5
## The nominal simulation rate (60 Hz); the host's tick rate is shown against it.
const NOMINAL_HZ := 60.0

signal closed

var _title: Label

var _backing: Panel
var _close: Button
var _tick_text: Label
var _lost_box: VBoxContainer
var _lost_text: Label
var _lost_icon: TextureRect
var _rehost: Button
var _rehost_code: LineEdit
var _rehost_copy: Button
var _tree: Tree
var _empty_text: Label
var _warning_text: Label
var _copy_log: Button
var _log_note: Label

var _rows: Array[Dictionary] = []
var _bound := false
var _host_lost := false
var _can_rehost := false
var _rehost_result := ""


func _ready() -> void:
	accessibility_name = tr(&"conn.title")
	if custom_minimum_size == Vector2.ZERO:
		custom_minimum_size = Vector2(480.0, 320.0)
	_backing = Panel.new()
	_backing.set_anchors_preset(Control.PRESET_FULL_RECT)
	_backing.visible = false
	add_child(_backing)
	var margin := MarginContainer.new()
	margin.set_anchors_preset(Control.PRESET_FULL_RECT)
	Ui.set_margins(margin, 3)
	add_child(margin)
	var column := Kit.vbox(2)
	margin.add_child(column)

	var header := Kit.hbox(3)
	_title = Kit.label("conn.title", &"PanelTitle")
	_title.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	# Hidden while a lobby tab of the same name stands over the panel; shown when pushed alone.
	_title.visible = false
	header.add_child(_title)
	_close = Kit.inline_button("conn.close")
	_close.visible = false
	_close.pressed.connect(_on_close)
	header.add_child(_close)
	column.add_child(header)
	_tick_text = Kit.label("", &"Body", true)
	column.add_child(_tick_text)

	_lost_box = Kit.vbox(1)
	_lost_box.visible = false
	column.add_child(_lost_box)
	var lost_row := Kit.hbox(2)
	_lost_icon = TextureRect.new()
	var lost_icon := _lost_icon
	lost_icon.texture = Ui.icon(&"alert_warning")
	lost_icon.stretch_mode = TextureRect.STRETCH_KEEP_CENTERED
	lost_icon.size_flags_vertical = Control.SIZE_SHRINK_BEGIN
	lost_row.add_child(lost_icon)
	_lost_text = Kit.label("", &"Body", true)
	_lost_text.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	lost_row.add_child(_lost_text)
	_lost_box.add_child(lost_row)
	_rehost = Kit.inline_button("conn.rehost")
	_rehost.pressed.connect(_on_rehost)
	_lost_box.add_child(_rehost)
	var code_row := Kit.hbox(2)
	_rehost_code = LineEdit.new()
	_rehost_code.editable = false
	_rehost_code.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_rehost_code.custom_minimum_size = Vector2(240.0, Kit.BUTTON_H)
	_rehost_code.accessibility_name = tr(&"conn.rehost_code")
	_rehost_code.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	code_row.add_child(_rehost_code)
	_rehost_copy = Kit.inline_button("conn.copy_code")
	_rehost_copy.icon = Ui.icon(&"copy")
	_rehost_copy.pressed.connect(func() -> void: _copy(_rehost_code.text, &"conn.copied_code"))
	code_row.add_child(_rehost_copy)
	code_row.visible = false
	_lost_box.add_child(code_row)

	_tree = Tree.new()
	_tree.columns = 6
	_tree.column_titles_visible = true
	_tree.hide_root = true
	_tree.select_mode = Tree.SELECT_ROW
	_tree.size_flags_vertical = Control.SIZE_EXPAND_FILL
	_tree.custom_minimum_size = Vector2(0.0, 160.0)
	_tree.focus_mode = Control.FOCUS_ALL
	_tree.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_tree.accessibility_name = tr(&"conn.table")
	_tree.set_column_expand(COL_NAME, true)
	_tree.set_column_clip_content(COL_NAME, true)
	for c: int in [COL_PING, COL_LOSS, COL_BEHIND, COL_QUALITY, COL_STATE]:
		_tree.set_column_expand(c, false)
	_tree.set_column_custom_minimum_width(COL_NAME, 96)
	_tree.set_column_custom_minimum_width(COL_PING, 92)
	_tree.set_column_custom_minimum_width(COL_LOSS, 64)
	_tree.set_column_custom_minimum_width(COL_BEHIND, 76)
	_tree.set_column_custom_minimum_width(COL_QUALITY, 88)
	_tree.set_column_custom_minimum_width(COL_STATE, 112)
	column.add_child(_tree)
	_empty_text = Kit.label("conn.empty", &"Body")
	column.add_child(_empty_text)
	_warning_text = Kit.label("", &"Body", true)
	column.add_child(_warning_text)
	var log_row := Kit.hbox(2)
	_copy_log = Kit.inline_button("conn.copy_log")
	_copy_log.icon = Ui.icon(&"copy")
	_copy_log.pressed.connect(_on_copy_log)
	log_row.add_child(_copy_log)
	_log_note = Kit.label("", &"Dense", true)
	_log_note.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	_log_note.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	log_row.add_child(_log_note)
	column.add_child(log_row)
	_apply_columns()
	show_rows([], 0.0)


func _apply_columns() -> void:
	var titles: Array[StringName] = [&"conn.col.player", &"conn.col.ping", &"conn.col.loss", &"conn.col.behind", &"conn.col.quality", &"conn.col.state"]
	for c in titles.size():
		_tree.set_column_title(c, tr(titles[c]))
		_tree.set_column_title_alignment(c, HORIZONTAL_ALIGNMENT_LEFT)


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_apply_columns()
		show_rows(_rows.duplicate(), _last_hz)


## Pushed on its own: an opaque backing, Close, and start listening.
func setup(_args: Dictionary) -> void:
	_backing.visible = true
	_close.visible = true
	_title.visible = true
	bind()


## Start listening to the session and draw its current state. Safe to call more than once.
func bind() -> void:
	if not _bound:
		var session := Kit.autoload(&"Session")
		if session != null:
			if session.has_signal(&"connection_changed") and not session.is_connected(&"connection_changed", refresh):
				session.connect(&"connection_changed", refresh)
			if session.has_signal(&"host_lost") and not session.is_connected(&"host_lost", show_host_lost):
				session.connect(&"host_lost", show_host_lost)
			if session.has_signal(&"state_changed") and not session.is_connected(&"state_changed", _on_state):
				session.connect(&"state_changed", _on_state)
			_bound = true
	refresh()


func default_focus() -> Control:
	if _rehost.visible:
		return _rehost
	return _close if _close.visible else _copy_log


func back_requested() -> void:
	_on_close()


func refresh() -> void:
	if not is_inside_tree():
		return
	var hz: Variant = Kit.call_on(&"Session", &"host_tick_hz")
	show_rows(Kit.dicts(Kit.call_on(&"Session", &"connections")), float(hz) if (hz is float or hz is int) else 0.0)


func _on_state(_state: int) -> void:
	# A new sortie or lobby clears an old host-loss notice once packets flow again.
	if _host_lost and not _rows.is_empty():
		_host_lost = false
		_lost_box.visible = false
	refresh()


var _last_hz := 0.0


## Render `Array[ConnectionDict]` (own row first) and the host's tick rate in Hz (0 = not running).
func show_rows(rows: Array[Dictionary], tick_hz: float) -> void:
	_rows = rows
	_last_hz = tick_hz
	if tick_hz > 0.0:
		var key: StringName = &"conn.tick" if tick_hz >= NOMINAL_HZ - 0.5 else &"conn.tick_slow"
		_tick_text.text = tr(key).format([Kit.number(tick_hz, &"none", 0), Kit.number(NOMINAL_HZ, &"none", 0)])
	else:
		_tick_text.text = tr(&"conn.tick_none")
	_tree.clear()
	var root := _tree.create_item()
	var warnings := PackedStringArray()
	for r: Dictionary in rows:
		var item := _tree.create_item(root)
		var state := Kit.get_int(r, "state", CONN_CONNECTED)
		var pname := Kit.get_str(r, "name")
		if pname.is_empty():
			pname = tr(&"conn.unnamed")
		if Kit.get_bool(r, "is_host"):
			pname = tr(&"conn.host_name").format([pname])
		item.set_text(COL_NAME, pname)
		var live := state == CONN_CONNECTED or state == CONN_WARNING
		item.set_text(COL_PING, tr(&"conn.ms").format([Kit.number(float(Kit.get_int(r, "ping_ms")), &"none", 0)]) if live else "")
		item.set_text(COL_LOSS, Kit.percent(Kit.get_float(r, "loss")) if live else "")
		item.set_text(COL_BEHIND, Kit.number(float(Kit.get_int(r, "frames_behind")), &"none", 0) if live else "")
		var q := Kit.get_int(r, "quality", 0)
		item.set_text(COL_QUALITY, tr(QUALITY_WORDS[clampi(q, 0, QUALITY_WORDS.size() - 1)]) if live else "")
		item.set_text(COL_STATE, tr(STATE_WORDS[clampi(state, 0, STATE_WORDS.size() - 1)]))
		for c: int in [COL_PING, COL_LOSS, COL_BEHIND]:
			item.set_text_alignment(c, HORIZONTAL_ALIGNMENT_RIGHT)
		var warning := Kit._Text.resolve(r.get("warning_text", {}))
		if state == CONN_WARNING or not warning.is_empty():
			item.set_icon(COL_STATE, Ui.icon(&"alert_caution"))
			warnings.append(tr(&"conn.warning_line").format([pname, warning if not warning.is_empty() else tr(&"conn.state.warning")]))
		if state == CONN_DROPPED:
			var held := Kit.get_int(r, "held_s", -1)
			var line := tr(&"conn.dropped_officers") if Kit.get_bool(r, "officers_in_command") else tr(&"conn.dropped")
			if held >= 0:
				line += " " + tr(&"conn.held").format([_clock(float(held))])
			warnings.append(tr(&"conn.warning_line").format([pname, line]))
	_tree.visible = not rows.is_empty()
	_empty_text.visible = rows.is_empty()
	_warning_text.text = "\n".join(warnings)
	_warning_text.visible = not warnings.is_empty()
	_link_focus()


static func _clock(seconds: float) -> String:
	var s: Variant = Kit.call_on(&"Tx", &"clock", [maxf(seconds, 0.0)])
	if s is String and not (s as String).is_empty():
		return s
	var whole := maxi(int(seconds), 0)
	return "%d:%02d" % [whole / 60, whole % 60]


## No packet from the host for 10 s. `can_rehost`: this member may re-host by hand.
func show_host_lost(can_rehost: bool) -> void:
	_host_lost = true
	_can_rehost = can_rehost
	_rehost_result = ""
	_lost_box.visible = true
	_lost_text.text = tr(&"conn.host_lost_rehost") if can_rehost else tr(&"conn.host_lost_auto")
	_lost_icon.visible = true
	_rehost.visible = can_rehost
	_rehost.disabled = false
	_rehost_code.get_parent().visible = false
	_link_focus()
	if is_inside_tree() and can_rehost:
		Ui.focus(_rehost)


func _on_rehost() -> void:
	_rehost.disabled = true
	var code: Variant = Kit.call_on(&"Session", &"rehost")
	show_rehosted(String(code) if code is String else "")


## The result of a hand re-host: the new join code, or empty when it failed.
func show_rehosted(code: String) -> void:
	_rehost_result = code
	if code.is_empty():
		_lost_text.text = tr(&"conn.rehost_failed")
		_rehost.disabled = false
		_rehost_code.get_parent().visible = false
	else:
		_lost_text.text = tr(&"conn.rehosted")
		_lost_icon.visible = false
		_rehost.visible = false
		_rehost_code.text = code
		_rehost_code.get_parent().visible = true
	_link_focus()


func _on_copy_log() -> void:
	var text: Variant = Kit.call_on(&"Lobby", &"connection_log_text")
	var s := String(text) if text is String else ""
	if s.is_empty():
		_log_note.text = tr(&"conn.log_empty")
		return
	DisplayServer.clipboard_set(s)
	_log_note.text = tr(&"conn.log_copied")


func _copy(text: String, note_key: StringName) -> void:
	if text.is_empty():
		return
	DisplayServer.clipboard_set(text)
	_log_note.text = tr(note_key)


func _on_close() -> void:
	closed.emit()
	if Screens.top() == self:
		Screens.pop()


## The focusable controls top to bottom, for a parent screen's focus chain.
func focus_controls() -> Array[Control]:
	return [_close, _rehost, _rehost_copy, _tree, _copy_log]


func _link_focus() -> void:
	var list := Kit.chain_vertical(focus_controls())
	for c: Control in list:
		if c.focus_neighbor_left.is_empty():
			c.focus_neighbor_left = c.get_path_to(c)
