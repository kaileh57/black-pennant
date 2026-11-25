extends Control
## Host panel: mapping result, join code and key, manual steps, upload speed, kick, ban, regenerate.
## DESIGN.md L994-1007, L1031. Unit: ui-lobby.
## Reads and calls: Lobby.host_panel, Lobby.set_upload_kbps, Lobby.kick, Lobby.ban, Lobby.regenerate_secret.
##
## Shown to the host only, as a tab of the lobby screen (`lobby.gd` adds it and calls `bind()`), or
## pushed on its own (`Screens.push(&"host_panel")`, which calls `setup` and adds a Close button).
## Top to bottom, in the order a host needs it: whether the port is open and what port mapping
## found (UPnP, NAT-PMP or PCP; double NAT says outside players cannot reach the host); the join code
## with Copy and the sentence that it contains the host's public IP address (L1001); the 16-character
## lobby key for joining by address; the LAN address for players on the same network; when mapping
## failed or double NAT was found, the manual forwarding steps and the mesh VPN option, with a join
## code for the VPN address when Tailscale or ZeroTier is running (L997); the upload speed field
## with the client limit it allows and the saturation warning (L1031); the players with Kick and Ban
## (ban holds the client id for this session); Regenerate key, which locks out old codes.
## Redraws on `Lobby.host_panel_changed` and `Lobby.lobby_changed`. `show_panel(panel, players)`
## renders plain data (`HostPanelDict` and `Array[LobbyPlayerDict]`) for tests and captures.

const Kit := preload("res://ui/screens/menu_kit.gd")

## `Bp.PORTMAP_*`.
const PORTMAP_IDLE := 0
const PORTMAP_TRYING := 1
const PORTMAP_MAPPED := 2
const PORTMAP_DOUBLE_NAT := 3
const PORTMAP_FAILED := 4
const PORTMAP_RENEW_FAILED := 5
const UPLOAD_MIN_MBPS := 0.3
const UPLOAD_MAX_MBPS := 100.0

signal closed

var _title: Label

var _content: VBoxContainer
var _backing: Panel
var _close: Button
var _state_text: Label
var _map_text: Label
var _code_field: Label
var _code_copy: Button
var _code_warning: Label
var _key_field: Label
var _key_copy: Button
var _lan_text: Label
var _external_text: Label
var _manual_box: VBoxContainer
var _manual_steps: Label
var _vpn_box: VBoxContainer
var _vpn_text: Label
var _vpn_field: Label
var _vpn_copy: Button
var _upload: SpinBox
var _limit_text: Label
var _saturated_text: Label
var _players_box: VBoxContainer
var _banned_text: Label
var _regenerate: Button
var _sections: Array[Control] = []
var _copy_note: Label

var _panel: Dictionary = {}
var _players: Array[Dictionary] = []
var _bound := false
var _setting_upload := false


func _ready() -> void:
	accessibility_name = tr(&"hosting.title")
	if custom_minimum_size == Vector2.ZERO:
		custom_minimum_size = Vector2(480.0, 360.0)
	_backing = Panel.new()
	_backing.set_anchors_preset(Control.PRESET_FULL_RECT)
	_backing.visible = false
	add_child(_backing)
	var scroll := ScrollContainer.new()
	scroll.set_anchors_preset(Control.PRESET_FULL_RECT)
	scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	scroll.follow_focus = true
	add_child(scroll)
	var margin := MarginContainer.new()
	margin.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	Ui.set_margins(margin, 3)
	scroll.add_child(margin)
	_content = Kit.vbox(2)
	_content.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	margin.add_child(_content)

	var header := Kit.hbox(3)
	_title = Kit.label("hosting.title", &"PanelTitle")
	_title.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	# Hidden while a lobby tab of the same name stands over the panel; shown when pushed alone.
	_title.visible = false
	header.add_child(_title)
	_close = Kit.inline_button("hosting.close")
	_close.visible = false
	_close.pressed.connect(_on_close)
	header.add_child(_close)
	_content.add_child(header)
	_state_text = Kit.label("", &"Body", true)
	_content.add_child(_state_text)
	_map_text = Kit.label("", &"Body", true)
	_content.add_child(_map_text)

	var code_box := _section()
	code_box.add_child(Kit.label("hosting.code", &"Body"))
	var code_row := Kit.vbox(1)
	_code_field = _readonly_field("hosting.code")
	code_row.add_child(_code_field)
	_code_copy = Kit.inline_button("hosting.copy_code")
	_code_copy.icon = Ui.icon(&"copy")
	_code_copy.pressed.connect(func() -> void: _copy(_code_field.text, &"hosting.copied_code"))
	code_row.add_child(_code_copy)
	code_box.add_child(code_row)
	_code_warning = Kit.label("hosting.code_ip_warning", &"Dense")
	code_box.add_child(_code_warning)
	_copy_note = Kit.label("", &"Dense", true)
	_copy_note.visible = false
	code_box.add_child(_copy_note)

	var key_box := _section()
	key_box.add_child(Kit.label("hosting.key", &"Body"))
	var key_row := Kit.vbox(1)
	_key_field = _readonly_field("hosting.key")
	key_row.add_child(_key_field)
	_key_copy = Kit.inline_button("hosting.copy_key")
	_key_copy.icon = Ui.icon(&"copy")
	_key_copy.pressed.connect(func() -> void: _copy(_key_field.text, &"hosting.copied_key"))
	key_row.add_child(_key_copy)
	key_box.add_child(key_row)
	_lan_text = Kit.label("", &"Dense", true)
	key_box.add_child(_lan_text)
	_external_text = Kit.label("", &"Dense", true)
	key_box.add_child(_external_text)

	_manual_box = _section()
	_manual_box.add_child(Kit.label("hosting.manual_title", &"PanelTitle"))
	_manual_steps = Kit.label("", &"Body", true)
	_manual_box.add_child(_manual_steps)

	_vpn_box = _section()
	_vpn_box.add_child(Kit.label("hosting.vpn_title", &"PanelTitle"))
	_vpn_text = Kit.label("", &"Body", true)
	_vpn_box.add_child(_vpn_text)
	var vpn_row := Kit.vbox(1)
	_vpn_field = _readonly_field("hosting.vpn_code")
	vpn_row.add_child(_vpn_field)
	_vpn_copy = Kit.inline_button("hosting.copy_code")
	_vpn_copy.icon = Ui.icon(&"copy")
	_vpn_copy.pressed.connect(func() -> void: _copy(_vpn_field.text, &"hosting.copied_code"))
	vpn_row.add_child(_vpn_copy)
	_vpn_box.add_child(vpn_row)

	var upload_box := _section()
	upload_box.add_child(Kit.label("hosting.upload_title", &"PanelTitle"))
	var upload_row := Kit.hbox(3)
	var upload_label := Kit.label("hosting.upload", &"Body")
	upload_label.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	upload_label.autowrap_mode = TextServer.AUTOWRAP_OFF
	upload_row.add_child(upload_label)
	_upload = SpinBox.new()
	_upload.min_value = UPLOAD_MIN_MBPS
	_upload.max_value = UPLOAD_MAX_MBPS
	_upload.step = 0.1
	_upload.value = 1.0
	_upload.suffix = tr(&"hosting.mbps")
	_upload.custom_minimum_size = Vector2(200.0, Kit.BUTTON_H)
	_upload.select_all_on_focus = true
	_upload.accessibility_name = tr(&"hosting.upload")
	_upload.value_changed.connect(_on_upload)
	upload_row.add_child(_upload)
	upload_box.add_child(upload_row)
	_limit_text = Kit.label("", &"Body", true)
	upload_box.add_child(_limit_text)
	_saturated_text = Kit.label("hosting.saturated", &"Body")
	_saturated_text.visible = false
	upload_box.add_child(_saturated_text)

	var players := _section()
	players.add_child(Kit.label("hosting.players_title", &"PanelTitle"))
	_players_box = Kit.vbox(1)
	players.add_child(_players_box)
	_banned_text = Kit.label("", &"Dense", true)
	players.add_child(_banned_text)

	var key_reset := _section()
	key_reset.add_child(Kit.label("hosting.regenerate_help", &"Body"))
	_regenerate = Kit.inline_button("hosting.regenerate")
	_regenerate.icon = Ui.icon(&"reload")
	_regenerate.pressed.connect(_ask_regenerate)
	key_reset.add_child(_regenerate)
	show_panel({}, [])


## A section: a 1 px rule and its content, kept together so a section moves as one.
func _section() -> VBoxContainer:
	var outer := Kit.vbox(2)
	outer.add_child(HSeparator.new())
	var box := Kit.vbox(1)
	outer.add_child(box)
	_content.add_child(outer)
	_sections.append(outer)
	return box


## A code or key the host reads out or copies: plain full-strength text, wrapped rather than cut off, so
## the whole code is always on screen (a read-only field drew it dimmed, like a disabled control, and
## clipped its end).
func _readonly_field(label_key: String) -> Label:
	var e := Kit.label("", &"Body", true)
	e.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	e.autowrap_mode = TextServer.AUTOWRAP_ARBITRARY
	e.accessibility_name = tr(StringName(label_key))
	e.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	e.focus_mode = Control.FOCUS_NONE
	return e


## Pushed on its own: show Close and start listening.
func setup(_args: Dictionary) -> void:
	_backing.visible = true
	_close.visible = true
	_title.visible = true
	bind()


## Start listening to the lobby and draw its current state. Safe to call more than once.
func bind() -> void:
	if not _bound:
		var lobby := Kit.autoload(&"Lobby")
		if lobby != null:
			for s: StringName in [&"host_panel_changed", &"lobby_changed"]:
				if lobby.has_signal(s) and not lobby.is_connected(s, refresh):
					lobby.connect(s, refresh)
			_bound = true
	refresh()


func refresh() -> void:
	if not is_inside_tree():
		return
	var lobby := Kit.as_dict(Kit.call_on(&"Lobby", &"lobby"))
	show_panel(Kit.as_dict(Kit.call_on(&"Lobby", &"host_panel")), Kit.dicts(lobby.get("players", [])))


func default_focus() -> Control:
	return _code_copy if _code_copy.visible and not _code_copy.disabled else _close


func back_requested() -> void:
	_on_close()


## Render a `HostPanelDict` and the lobby's players.
func show_panel(panel: Dictionary, players: Array[Dictionary]) -> void:
	_panel = panel
	_players = players
	var hosting := Kit.get_bool(panel, "hosting")
	for sec: Control in _sections:
		sec.visible = hosting
	if not hosting:
		_state_text.text = tr(&"hosting.not_hosting")
		_map_text.visible = false
		_link_focus()
		return
	var port := Kit.get_int(panel, "port")
	_state_text.text = tr(&"hosting.bound").format([port]) if Kit.get_bool(panel, "bound") else tr(&"hosting.binding").format([port])
	var portmap := Kit.get_int(panel, "portmap")
	var map_sentence := Kit._Text.resolve(panel.get("portmap_text", {}))
	if map_sentence.is_empty():
		map_sentence = tr(StringName("hosting.portmap.%d" % portmap)) if portmap != PORTMAP_IDLE else ""
	_map_text.text = map_sentence
	_map_text.visible = not map_sentence.is_empty()

	var code := Kit.get_str(panel, "join_code")
	_code_field.text = code if not code.is_empty() else tr(&"hosting.code_pending")
	_code_copy.disabled = code.is_empty()
	var key := Kit.get_str(panel, "lobby_key")
	_key_field.text = key if not key.is_empty() else tr(&"hosting.code_pending")
	_key_copy.disabled = key.is_empty()
	var lan := Kit.get_str(panel, "lan_address")
	_lan_text.text = tr(&"hosting.lan").format([lan, port]) if not lan.is_empty() else tr(&"hosting.lan_unknown")
	var external := Kit.get_str(panel, "external_address")
	_external_text.text = tr(&"hosting.external").format([external]) if not external.is_empty() else ""
	_external_text.visible = not external.is_empty()

	var trouble := portmap == PORTMAP_FAILED or portmap == PORTMAP_DOUBLE_NAT or portmap == PORTMAP_RENEW_FAILED
	var steps := PackedStringArray()
	var n := 1
	for m: Variant in Kit.as_array(panel.get("manual_steps", [])):
		var line := Kit._Text.resolve(m)
		if not line.is_empty():
			steps.append(tr(&"hosting.step").format([n, line]))
			n += 1
	_manual_steps.text = "\n".join(steps)
	_manual_box.get_parent().visible = trouble and not steps.is_empty()
	var vpn := Kit.get_str(panel, "vpn_address")
	var vpn_code := Kit.get_str(panel, "vpn_join_code")
	_vpn_box.get_parent().visible = trouble or not vpn.is_empty()
	_vpn_text.text = tr(&"hosting.vpn_found").format([vpn]) if not vpn.is_empty() else tr(&"hosting.vpn_none")
	_vpn_field.get_parent().visible = not vpn_code.is_empty()
	_vpn_field.text = vpn_code
	# The VPN path comes first while the automatic mapping has not worked (L1009).
	var vpn_section := _vpn_box.get_parent()
	var manual_section := _manual_box.get_parent()
	if trouble and vpn_section.get_index() > manual_section.get_index():
		_content.move_child(vpn_section, manual_section.get_index())
	elif not trouble and vpn_section.get_index() < manual_section.get_index():
		_content.move_child(manual_section, vpn_section.get_index())

	var kbps := Kit.get_int(panel, "upload_kbps", 1000)
	if not _upload.get_line_edit().has_focus():
		_setting_upload = true
		_upload.value = clampf(kbps / 1000.0, UPLOAD_MIN_MBPS, UPLOAD_MAX_MBPS)
		_setting_upload = false
	var limit := Kit.get_int(panel, "client_limit")
	_limit_text.text = tr(&"hosting.limit_one" if limit == 1 else &"hosting.limit_many").format([limit])
	_saturated_text.visible = Kit.get_bool(panel, "saturated")

	Kit.clear(_players_box)
	var others := 0
	for p: Dictionary in players:
		if Kit.get_bool(p, "is_local") or Kit.get_bool(p, "is_host"):
			continue
		others += 1
		_players_box.add_child(_player_row(p))
	if others == 0:
		_players_box.add_child(Kit.label("hosting.players_none", &"Body"))
	var banned := Kit.get_int(panel, "banned")
	_banned_text.text = tr(&"hosting.banned").format([banned])
	_banned_text.visible = banned > 0
	_link_focus()


func _player_row(p: Dictionary) -> HBoxContainer:
	var row := Kit.hbox(2)
	var name_label := Kit.label(Kit.get_str(p, "name", tr(&"hosting.unnamed")), &"Body", true)
	name_label.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	name_label.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	name_label.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
	name_label.autowrap_mode = TextServer.AUTOWRAP_OFF
	row.add_child(name_label)
	var slot := Kit.get_int(p, "slot", -1)
	var pname := Kit.get_str(p, "name")
	var kick := Kit.inline_button("hosting.kick")
	kick.accessibility_name = tr(&"hosting.kick_named").format([pname])
	kick.pressed.connect(func() -> void: _ask_remove(slot, pname, false))
	row.add_child(kick)
	var ban := Kit.inline_button("hosting.ban")
	ban.accessibility_name = tr(&"hosting.ban_named").format([pname])
	ban.pressed.connect(func() -> void: _ask_remove(slot, pname, true))
	row.add_child(ban)
	return row


# --- actions -------------------------------------------------------------------------------------

func _copy(text: String, toast_key: StringName) -> void:
	if text.is_empty():
		return
	DisplayServer.clipboard_set(text)
	# Inline state, not a toast: only saves, joins and leaves toast (L1605).
	_copy_note.text = tr(toast_key)
	_copy_note.visible = true


func _on_upload(mbps: float) -> void:
	if _setting_upload or not is_inside_tree():
		return
	Kit.call_on(&"Lobby", &"set_upload_kbps", [roundi(mbps * 1000.0)])


func _ask_remove(slot: int, pname: String, ban: bool) -> void:
	var title: StringName = &"hosting.ban_title" if ban else &"hosting.kick_title"
	var body := tr(&"hosting.ban_body" if ban else &"hosting.kick_body").format([pname])
	var dialog := Screens.open_dialog(title, body,
		[{"id": &"keep", "label": &"hosting.keep_player"}, {"id": &"go", "label": &"hosting.ban_confirm" if ban else &"hosting.kick_confirm"}], &"keep", &"keep")
	if dialog != null:
		dialog.connect(&"chosen", _on_remove_choice.bind(slot, ban))


func _on_remove_choice(id: StringName, slot: int, ban: bool) -> void:
	if id == &"go" and is_inside_tree():
		Kit.call_on(&"Lobby", &"ban" if ban else &"kick", [slot])
		refresh()


func _ask_regenerate() -> void:
	var dialog := Screens.open_dialog(&"hosting.regenerate_title", &"hosting.regenerate_body",
		[{"id": &"keep", "label": &"hosting.regenerate_keep"}, {"id": &"go", "label": &"hosting.regenerate"}], &"keep", &"keep")
	if dialog != null:
		dialog.connect(&"chosen", _on_regenerate_choice)


func _on_regenerate_choice(id: StringName) -> void:
	if id == &"go" and is_inside_tree():
		Kit.call_on(&"Lobby", &"regenerate_secret")
		refresh()


func _on_close() -> void:
	closed.emit()
	if Screens.top() == self:
		Screens.pop()


## The focusable controls top to bottom, for a parent screen's focus chain.
func focus_controls() -> Array[Control]:
	var out: Array[Control] = [_close, _code_copy, _key_copy, _vpn_copy, _upload.get_line_edit()]
	for row: Node in _players_box.get_children():
		for c: Node in row.get_children():
			if c is Button:
				out.append(c as Button)
	out.append(_regenerate)
	return out


func _link_focus() -> void:
	var list := Kit.chain_vertical(focus_controls())
	for c: Control in list:
		if c.focus_neighbor_left.is_empty():
			c.focus_neighbor_left = c.get_path_to(c)
