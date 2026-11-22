extends Control
## Lobby: players, teams, ready, design, ruleset, issues, start.
## DESIGN.md L955-978. Unit: ui-lobby.
## Reads and calls: Lobby.lobby, Lobby.set_ready, Lobby.set_team, Lobby.select_blueprint, Lobby.set_setup, Lobby.start.
##
## The base screen while the session is in its lobby (`Screens` routes here on `Bp.SESSION_LOBBY`).
## The same for the host and every member; the host also gets the lobby settings and the host panel.
## Heading: the lobby's name, then one line of what the lobby is (mode, player count, and for PvP the
## ruleset and its tonnage and complexity caps, L963-969).
## Left: the players table (name, team in PvP, design, tonnage, ready, ping) and, under it, every
## reason the sortie cannot start yet as a sentence (`LobbyDict.issues`).
## Middle: your ship. The design picker lists stock and own designs; Field this design checks it
## against the lobby's ruleset (`Lobby.select_blueprint`) and shows each problem as a sentence. PvP
## players pick a team. Ready is the most prominent control for a member; for the host it is Start
## sortie, enabled when `can_start`, with Ready beside it.
## Right: tabs. Connection (everyone: ping, tick rate, warnings, re-host); for the host also Hosting
## (join code, key, mapping, upload, kick and ban) and Settings (PvP ruleset and cap, the skirmish
## operation and opposition, conditions).
## `Lobby.starting(seconds)` shows the countdown; `Lobby.left(text)` shows why we left (kicked, host
## quit) once the router is back at the menu. Leave lobby asks first only for the host, whose
## leaving ends the lobby for everyone. `show_lobby(lobby)` renders a plain `LobbyDict` for tests
## and captures.

const Kit := preload("res://ui/screens/menu_kit.gd")
const DesignPicker := preload("res://ui/screens/design_picker.gd")
const ConditionsPanel := preload("res://ui/screens/conditions_panel.gd")
const HostPanelScene := preload("res://ui/screens/host_panel.tscn")
const ConnectionPanelScene := preload("res://ui/screens/connection_panel.tscn")

## `Bp.MODE_*`.
const MODE_CAMPAIGN := 0
const MODE_SKIRMISH := 1
const MODE_SANDBOX := 2
const MODE_PVP_ELIMINATION := 3
const MODE_PVP_ESCORT := 4
const RULESET_IDS: Array[String] = ["standard", "open"]
const PVP_CAPS: Array[float] = [150.0, 400.0, 900.0, 1800.0]
const ENEMY_MIN := 1
const ENEMY_MAX := 20
const COL_NAME := 0
const COL_TEAM := 1
const COL_DESIGN := 2
const COL_TONNAGE := 3
const COL_READY := 4
const COL_PING := 5
## Your-ship column and the tabs; the tabs are wider so a whole join code fits on one line.
const SIDE_W := 460.0
const TABS_W := 620.0

var _title: Label
var _summary: Label
var _leave_button: Button
var _tree: Tree
var _issues_title: Label
var _issues: Label
var _picker: DesignPicker
var _field: Button
var _field_result: Label
var _team_row: HBoxContainer
var _team: OptionButton
var _ready_toggle: Button
var _start: Button
var _status: Label
var _tabs: TabContainer
var _host_panel: Control
var _connection_panel: Control
var _settings_tab: ScrollContainer
var _settings_box: VBoxContainer
var _ruleset: OptionButton
var _cap: OptionButton
var _pvp_box: VBoxContainer
var _skirmish_box: VBoxContainer
var _template: OptionButton
var _act: OptionButton
var _enemies: SpinBox
var _difficulty: OptionButton
var _conditions: ConditionsPanel

var _lobby: Dictionary = {}
var _templates: Array[Dictionary] = []
var _presets: Array[Dictionary] = []
var _bound := false
var _applying := false
var _countdown_end_ms := -1
var _was_host := false
## The host changed a condition here, so the view no longer overwrites the conditions panel.
var _conditions_touched := false


var _body: BoxContainer
var _body_scroll: ScrollContainer
var _body_step := 0
## `[control, width side by side, height when stacked]`, see `_fit_columns`.
var _fit: Array[Array] = []

func _ready() -> void:
	accessibility_name = tr(&"lobby.title")
	var backing := Panel.new()
	backing.set_anchors_preset(Control.PRESET_FULL_RECT)
	add_child(backing)
	var margin := MarginContainer.new()
	margin.set_anchors_preset(Control.PRESET_FULL_RECT)
	Ui.set_margins(margin, 5)
	add_child(margin)
	var column := Kit.vbox(3)
	margin.add_child(column)

	var header := Kit.hbox(4)
	var heading := Kit.vbox(0)
	heading.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_title = Kit.label("", &"ScreenTitle", true)
	heading.add_child(_title)
	_summary = Kit.label("", &"Body", true)
	heading.add_child(_summary)
	header.add_child(heading)
	_leave_button = Kit.inline_button("lobby.leave")
	_leave_button.custom_minimum_size = Vector2(200.0, Kit.BUTTON_H)
	_leave_button.size_flags_vertical = Control.SIZE_SHRINK_BEGIN
	_leave_button.pressed.connect(_ask_leave)
	header.add_child(_leave_button)
	column.add_child(header)
	column.add_child(HSeparator.new())

	var body := _responsive_body(column, 5)

	var left := Kit.vbox(2)
	left.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	body.add_child(left)
	left.add_child(Kit.label("lobby.players", &"PanelTitle"))
	_tree = Tree.new()
	_tree.columns = 6
	_tree.column_titles_visible = true
	_tree.hide_root = true
	_tree.select_mode = Tree.SELECT_ROW
	_tree.size_flags_vertical = Control.SIZE_EXPAND_FILL
	_tree.focus_mode = Control.FOCUS_ALL
	_tree.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_tree.accessibility_name = tr(&"lobby.players")
	_tree.set_column_expand(COL_NAME, true)
	_tree.set_column_expand(COL_DESIGN, true)
	_tree.set_column_clip_content(COL_NAME, true)
	_tree.set_column_clip_content(COL_DESIGN, true)
	for c: int in [COL_TEAM, COL_TONNAGE, COL_READY, COL_PING]:
		_tree.set_column_expand(c, false)
	_tree.set_column_custom_minimum_width(COL_NAME, 128)
	_tree.set_column_custom_minimum_width(COL_TEAM, 76)
	_tree.set_column_custom_minimum_width(COL_DESIGN, 140)
	_tree.set_column_custom_minimum_width(COL_TONNAGE, 96)
	_tree.set_column_custom_minimum_width(COL_READY, 136)
	_tree.set_column_custom_minimum_width(COL_PING, 76)
	left.add_child(_tree)
	_issues_title = Kit.label("lobby.issues", &"PanelTitle")
	left.add_child(_issues_title)
	_issues = Kit.label("", &"Body", true)
	left.add_child(_issues)

	var middle_scroll := ScrollContainer.new()
	middle_scroll.custom_minimum_size = Vector2(SIDE_W, 0.0)
	middle_scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	middle_scroll.follow_focus = true
	body.add_child(middle_scroll)
	var middle := Kit.vbox(2)
	middle.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	middle.size_flags_vertical = Control.SIZE_EXPAND_FILL
	middle_scroll.add_child(middle)
	_picker = DesignPicker.new()
	# Tall enough for the starters and a few of the player's own designs without scrolling.
	_picker.list.custom_minimum_size = Vector2(0.0, 380.0)
	_picker.edit_button.visible = false
	_picker.selected.connect(func(_p: String) -> void: _refresh_field())
	middle.add_child(_picker)
	_field = Kit.button("lobby.field")
	_field.pressed.connect(_on_field)
	middle.add_child(_field)
	_field_result = Kit.label("", &"Body", true)
	_field_result.visible = false
	middle.add_child(_field_result)
	_team = OptionButton.new()
	_team.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_team.custom_minimum_size = Vector2(240.0, Kit.BUTTON_H)
	_team.add_item(tr(&"lobby.team.0"), 0)
	_team.add_item(tr(&"lobby.team.1"), 1)
	_team.accessibility_name = tr(&"lobby.team")
	_team.item_selected.connect(func(i: int) -> void: Kit.call_on(&"Lobby", &"set_team", [_team.get_item_id(i)]))
	_team_row = Kit.hbox(3)
	var team_label := Kit.label("lobby.team", &"Body")
	team_label.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	team_label.autowrap_mode = TextServer.AUTOWRAP_OFF
	_team_row.add_child(team_label)
	_team_row.add_child(_team)
	middle.add_child(_team_row)

	_tabs = TabContainer.new()
	_tabs.custom_minimum_size = Vector2(TABS_W, 0.0)
	_tabs.size_flags_vertical = Control.SIZE_EXPAND_FILL
	body.add_child(_tabs)
	_connection_panel = ConnectionPanelScene.instantiate()
	_connection_panel.name = &"Connection"
	_tabs.add_child(_connection_panel)
	_host_panel = HostPanelScene.instantiate()
	_host_panel.name = &"Hosting"
	_tabs.add_child(_host_panel)
	_build_settings()
	_tabs.add_child(_settings_tab)

	column.add_child(HSeparator.new())
	var footer := Kit.hbox(4)
	_status = Kit.label("", &"Body", true)
	_status.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_status.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	footer.add_child(_status)
	# Ready is the member's main action (L955): a full-height toggle with its state as icon and word.
	_ready_toggle = Kit.button("lobby.ready", true)
	_ready_toggle.toggle_mode = true
	_ready_toggle.custom_minimum_size = Vector2(320.0, Kit.PRIMARY_H)
	_ready_toggle.size_flags_horizontal = Control.SIZE_SHRINK_END
	_ready_toggle.toggled.connect(_on_ready)
	footer.add_child(_ready_toggle)
	_start = Kit.button("lobby.start", true)
	_start.custom_minimum_size = Vector2(320.0, Kit.PRIMARY_H)
	_start.size_flags_horizontal = Control.SIZE_SHRINK_END
	_start.pressed.connect(_on_start)
	footer.add_child(_start)
	column.add_child(footer)
	_fit = [[left, 560.0, 0.0], [_tree, 0.0, 320.0], [middle_scroll, SIDE_W, 560.0], [_tabs, TABS_W, 720.0]]
	_apply_columns()
	set_process(false)
	show_lobby({})


func _build_settings() -> void:
	_settings_tab = ScrollContainer.new()
	_settings_tab.name = &"Settings"
	_settings_tab.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	_settings_tab.follow_focus = true
	var margin := MarginContainer.new()
	margin.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	Ui.set_margins(margin, 3)
	_settings_tab.add_child(margin)
	_settings_box = Kit.vbox(3)
	_settings_box.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	margin.add_child(_settings_box)

	_pvp_box = Kit.vbox(2)
	_pvp_box.add_child(Kit.label("lobby.settings.pvp", &"PanelTitle"))
	_ruleset = _option()
	for i in RULESET_IDS.size():
		_ruleset.add_item(tr(StringName("mp.ruleset." + RULESET_IDS[i])), i)
	_ruleset.item_selected.connect(func(_i: int) -> void: _push_setup())
	_pvp_box.add_child(_row("lobby.settings.ruleset", _ruleset))
	_pvp_box.add_child(Kit.label("lobby.settings.ruleset_help", &"Dense"))
	_cap = _option()
	for i in PVP_CAPS.size():
		_cap.add_item(Kit.number(PVP_CAPS[i], &"t", 0), i)
	_cap.item_selected.connect(func(_i: int) -> void: _push_setup())
	_pvp_box.add_child(_row("lobby.settings.cap", _cap))
	_settings_box.add_child(_pvp_box)

	_skirmish_box = Kit.vbox(2)
	_skirmish_box.add_child(Kit.label("lobby.settings.skirmish", &"PanelTitle"))
	_template = _option()
	_template.item_selected.connect(func(_i: int) -> void: _push_setup())
	_skirmish_box.add_child(_row("lobby.settings.template", _template))
	_act = _option()
	_act.item_selected.connect(func(_i: int) -> void: _push_setup())
	_skirmish_box.add_child(_row("lobby.settings.act", _act))
	_enemies = SpinBox.new()
	_enemies.min_value = ENEMY_MIN
	_enemies.max_value = ENEMY_MAX
	_enemies.step = 1
	_enemies.value = 1
	_enemies.custom_minimum_size = Vector2(160.0, Kit.BUTTON_H)
	_enemies.value_changed.connect(func(_v: float) -> void: _push_setup())
	_skirmish_box.add_child(_row("lobby.settings.enemies", _enemies))
	_difficulty = _option()
	_difficulty.item_selected.connect(func(_i: int) -> void: _push_setup())
	_skirmish_box.add_child(_row("lobby.settings.difficulty", _difficulty))
	_settings_box.add_child(_skirmish_box)

	_conditions = ConditionsPanel.new()
	_conditions.changed.connect(_push_setup)
	_settings_box.add_child(_conditions)


func _option() -> OptionButton:
	var o := OptionButton.new()
	o.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	o.custom_minimum_size = Vector2(260.0, Kit.BUTTON_H)
	o.focus_mode = Control.FOCUS_ALL
	return o


func _row(label_key: String, control: Control) -> HBoxContainer:
	var row := Kit.hbox(3)
	var l := Kit.label(label_key, &"Body")
	l.custom_minimum_size = Vector2(180.0, 0.0)
	l.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	l.autowrap_mode = TextServer.AUTOWRAP_OFF
	row.add_child(l)
	control.accessibility_name = tr(StringName(label_key))
	row.add_child(control)
	return row


func _apply_columns() -> void:
	var titles: Array[StringName] = [&"lobby.col.name", &"lobby.col.team", &"lobby.col.design", &"lobby.col.tonnage", &"lobby.col.ready", &"lobby.col.ping"]
	for c in titles.size():
		_tree.set_column_title(c, tr(titles[c]))
		_tree.set_column_title_alignment(c, HORIZONTAL_ALIGNMENT_LEFT)
	_tabs.set_tab_title(_connection_panel.get_index(), tr(&"lobby.tab.connection"))
	_tabs.set_tab_title(_host_panel.get_index(), tr(&"lobby.tab.hosting"))
	_tabs.set_tab_title(_settings_tab.get_index(), tr(&"lobby.tab.settings"))


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_apply_columns()
		show_lobby(_lobby)


# --- facade -------------------------------------------------------------------------------------

func setup(_args: Dictionary) -> void:
	_templates = Kit.dicts(Kit.call_on(&"Content", &"mission_templates"))
	_presets = Kit.difficulty_order(Kit.dicts(Kit.call_on(&"Content", &"difficulty_presets")))
	var caps: Array[float] = []
	var raw_caps: Variant = Kit.call_on(&"Content", &"act_caps")
	if raw_caps is PackedFloat32Array:
		for c: float in raw_caps as PackedFloat32Array:
			caps.append(c)
	fill_choices(_templates, _presets, caps)
	_picker.load_entries()
	_bind()
	_host_panel.call(&"bind")
	_connection_panel.call(&"bind")
	refresh()


func _bind() -> void:
	if _bound:
		return
	var lobby := Kit.autoload(&"Lobby")
	if lobby == null:
		return
	_bound = true
	for pair: Array in [[&"lobby_changed", refresh], [&"starting", show_starting], [&"left", _on_left]]:
		if lobby.has_signal(pair[0]) and not lobby.is_connected(pair[0], pair[1]):
			lobby.connect(pair[0], pair[1])


func refresh() -> void:
	if not is_inside_tree():
		return
	show_lobby(Kit.as_dict(Kit.call_on(&"Lobby", &"lobby")))


func default_focus() -> Control:
	if Kit.get_bool(_lobby, "is_host") and not _start.disabled:
		return _start
	return _ready_toggle if not _ready_toggle.disabled else _picker.list


func _unhandled_input(event: InputEvent) -> void:
	if event.is_action_pressed(&"ui_cancel", false, true) and Screens.top() == self:
		get_viewport().set_input_as_handled()
		_ask_leave()


## Template, act and difficulty choices for the host's skirmish settings (also the test entry point).
func fill_choices(templates: Array[Dictionary], presets: Array[Dictionary], caps: Array[float]) -> void:
	_templates = templates
	_presets = presets
	_template.clear()
	for i in templates.size():
		_template.add_item(tr(StringName(Kit.get_str(templates[i], "name"))), i)
	_act.clear()
	# One row per act this build ships (Content.act_caps: Act 1 alone in 0.1.0); Act 1 when none is given.
	var acts := maxi(caps.size(), 1)
	for a in acts:
		var cap := caps[a] if a < caps.size() else PVP_CAPS[a]
		_act.add_item(tr(&"lobby.settings.act_row").format([tr(StringName("units.word.act.%d" % (a + 1))), Kit.number(cap, &"t", 0)]), a)
	_difficulty.clear()
	for i in presets.size():
		_difficulty.add_item(tr(StringName(Kit.get_str(presets[i], "name", "difficulty." + Kit.get_str(presets[i], "preset")))), i)


## Render a `LobbyDict` (an empty one is the moment before the first view arrives).
func show_lobby(lobby: Dictionary) -> void:
	_lobby = lobby
	var is_host := Kit.get_bool(lobby, "is_host")
	var mode := Kit.get_int(lobby, "mode", MODE_SANDBOX)
	var pvp := mode == MODE_PVP_ELIMINATION or mode == MODE_PVP_ESCORT
	var players := Kit.dicts(lobby.get("players", []))
	var ruleset := Kit.as_dict(lobby.get("ruleset", {}))
	var lobby_name := Kit.get_str(lobby, "name")
	_title.text = lobby_name if not lobby_name.is_empty() else tr(&"lobby.title")
	_summary.text = summary_text(lobby) if not lobby.is_empty() else tr(&"lobby.waiting")

	# Team means something only in PvP; elsewhere the column folds away.
	_tree.set_column_custom_minimum_width(COL_TEAM, 76 if pvp else 0)
	_tree.set_column_title(COL_TEAM, tr(&"lobby.col.team") if pvp else "")
	_tree.clear()
	var root := _tree.create_item()
	var me: Dictionary = {}
	for p: Dictionary in players:
		var item := _tree.create_item(root)
		var pname := Kit.get_str(p, "name")
		if pname.is_empty():
			pname = tr(&"lobby.unnamed")
		var marks := PackedStringArray()
		if Kit.get_bool(p, "is_host"):
			marks.append(tr(&"lobby.mark.host"))
		if Kit.get_bool(p, "is_local"):
			marks.append(tr(&"lobby.mark.you"))
			me = p
		item.set_text(COL_NAME, pname if marks.is_empty() else tr(&"lobby.name_marks").format([pname, ", ".join(marks)]))
		item.set_text(COL_TEAM, tr(StringName("lobby.team.%d" % clampi(Kit.get_int(p, "team"), 0, 1))) if pvp else "")
		item.set_text(COL_DESIGN, design_text(p))
		var t := Kit.get_float(p, "tonnage_t", -1.0)
		item.set_text(COL_TONNAGE, Kit.number(t, &"t", 0) if t >= 0.0 else "")
		item.set_text_alignment(COL_TONNAGE, HORIZONTAL_ALIGNMENT_RIGHT)
		var ready := Kit.get_bool(p, "ready")
		item.set_text(COL_READY, tr(&"lobby.is_ready") if ready else tr(&"lobby.not_ready"))
		item.set_icon(COL_READY, Ui.icon(&"check_pass" if ready else &"dot"))
		var conn := Kit.as_dict(p.get("conn", {}))
		var ping := Kit.get_int(conn, "ping_ms", -1)
		item.set_text(COL_PING, "" if Kit.get_bool(p, "is_local") or ping < 0 else tr(&"conn.ms").format([Kit.number(float(ping), &"none", 0)]))
		item.set_text_alignment(COL_PING, HORIZONTAL_ALIGNMENT_RIGHT)
		if Kit.get_int(conn, "state", 2) == 3:
			item.set_icon(COL_PING, Ui.icon(&"alert_caution"))
	var max_players := Kit.get_int(lobby, "max_players")
	for i in range(players.size(), max_players):
		var open := _tree.create_item(root)
		open.set_text(COL_NAME, tr(&"lobby.open_slot"))
		open.set_custom_color(COL_NAME, Ui.colour(&"text.secondary"))
		for c in _tree.columns:
			open.set_selectable(c, false)

	var issues := PackedStringArray()
	for m: Variant in Kit.as_array(lobby.get("issues", [])):
		var line := Kit._Text.resolve(m)
		if not line.is_empty():
			issues.append(line)
	if Kit.get_bool(lobby, "saturated"):
		issues.append(tr(&"lobby.saturated"))
	_issues.text = "\n".join(issues)
	_issues_title.visible = not issues.is_empty()
	_issues.visible = not issues.is_empty()

	_team_row.visible = pvp
	if pvp and not me.is_empty():
		_team.select(clampi(Kit.get_int(me, "team"), 0, 1))
	_picker.set_cap(Kit.get_float(ruleset, "tonnage_cap_t", 0.0) if pvp or mode == MODE_SKIRMISH else 0.0,
		&"lobby.design_over_cap" if pvp else &"")
	_ready_toggle.set_block_signals(true)
	_ready_toggle.button_pressed = Kit.get_bool(me, "ready")
	_ready_toggle.set_block_signals(false)
	_show_ready_toggle()
	_ready_toggle.disabled = lobby.is_empty() or Kit.get_str(me, "blueprint_hash").is_empty() and mode != MODE_CAMPAIGN
	_start.visible = is_host
	_start.disabled = not Kit.get_bool(lobby, "can_start")
	_ready_toggle.size_flags_horizontal = Control.SIZE_SHRINK_END

	_tabs.set_tab_hidden(_host_panel.get_index(), not is_host)
	_tabs.set_tab_hidden(_settings_tab.get_index(), not is_host)
	if is_host and not _was_host:
		_tabs.current_tab = _host_panel.get_index()
	_was_host = is_host
	if is_host:
		_show_setup(mode, ruleset, lobby.get("setup"))
	_show_status()
	_link_focus()


static func design_text(p: Dictionary) -> String:
	var name_text := Kit.get_str(p, "blueprint_name")
	var upload := Kit.get_float(p, "upload", -1.0)
	if upload >= 0.0 and upload < 1.0:
		return Kit.tr_key(&"lobby.design_uploading").format([Kit.percent(upload)])
	if not name_text.is_empty():
		return name_text
	if Kit.get_str(p, "blueprint_hash").is_empty():
		return Kit.tr_key(&"lobby.design_none")
	return Kit.tr_key(&"lobby.design_fetching")


## One line of what the lobby is: mode and players, and for PvP the ruleset and caps.
static func summary_text(lobby: Dictionary) -> String:
	var mode := Kit.get_int(lobby, "mode", MODE_SANDBOX)
	var mode_word := Kit.tr_key(&"mp.mode.coop") if mode == MODE_CAMPAIGN else String(Kit.call_on(&"Tx", &"word", [&"mode", mode]) if Kit.autoload(&"Tx") != null else "")
	var count := Kit.as_array(lobby.get("players", [])).size()
	var line := Kit.tr_key(&"lobby.summary").format([mode_word, count, Kit.get_int(lobby, "max_players")])
	if mode == MODE_PVP_ELIMINATION or mode == MODE_PVP_ESCORT:
		var r := Kit.as_dict(lobby.get("ruleset", {}))
		var rid := Kit.get_str(r, "id")
		var rname := Kit.tr_key(StringName("mp.ruleset." + rid))
		line += " " + Kit.tr_key(&"lobby.summary_pvp").format([rname if rname != "mp.ruleset." + rid else rid,
			Kit.number(Kit.get_float(r, "tonnage_cap_t"), &"t", 0),
			Kit.number(Kit.get_float(r, "complexity_per_vessel"), &"none", 0),
			Kit.number(Kit.get_float(r, "complexity_per_lobby"), &"none", 0)])
	return line


func _show_setup(mode: int, ruleset: Dictionary, setup_v: Variant) -> void:
	_applying = true
	var pvp := mode == MODE_PVP_ELIMINATION or mode == MODE_PVP_ESCORT
	_pvp_box.visible = pvp
	_skirmish_box.visible = mode == MODE_SKIRMISH
	if pvp:
		_ruleset.select(maxi(RULESET_IDS.find(Kit.get_str(ruleset, "id")), 0))
		var cap := Kit.get_float(ruleset, "tonnage_cap_t", 400.0)
		var best := 1
		for i in PVP_CAPS.size():
			if absf(PVP_CAPS[i] - cap) < absf(PVP_CAPS[best] - cap):
				best = i
		_cap.select(best)
	var setup := Kit.as_dict(setup_v)
	var sk := Kit.as_dict(setup.get("skirmish"))
	if mode == MODE_SKIRMISH and not sk.is_empty():
		for i in _templates.size():
			if Kit.get_int(_templates[i], "template", -2) == Kit.get_int(sk, "template", -1):
				_template.select(i)
		_act.select(clampi(Kit.get_int(sk, "act"), 0, _act.item_count - 1))
		_enemies.set_value_no_signal(clampi(Kit.get_int(sk, "enemy_per_player", 1), ENEMY_MIN, ENEMY_MAX))
		var preset := Kit.get_str(Kit.as_dict(sk.get("difficulty")), "preset")
		for i in _presets.size():
			if Kit.get_str(_presets[i], "preset") == preset:
				_difficulty.select(i)
	if not setup.is_empty() and not _conditions_touched:
		_conditions.weather.select(maxi(_conditions.weather.get_item_index(Kit.get_int(setup, "weather")), 0))
		_conditions.sea.set_value_no_signal(clampi(Kit.get_int(setup, "sea_state", 3), 0, 8))
		_conditions.time_of_day.select(maxi(_conditions.time_of_day.get_item_index(Kit.get_int(setup, "time_of_day", 1)), 0))
	_applying = false



## The settings tab as the `Lobby.set_setup` Dictionary.
func setup_patch() -> Dictionary:
	var mode := Kit.get_int(_lobby, "mode", MODE_SANDBOX)
	var out := {}
	if mode == MODE_PVP_ELIMINATION or mode == MODE_PVP_ESCORT:
		out["ruleset"] = RULESET_IDS[clampi(_ruleset.get_selected_id(), 0, RULESET_IDS.size() - 1)]
		out["tonnage_cap_t"] = PVP_CAPS[clampi(_cap.get_selected_id(), 0, PVP_CAPS.size() - 1)]
	var cond := _conditions.values()
	for k: String in ["weather", "sea_state", "time_of_day"]:
		if cond.has(k):
			out[k] = cond[k]
	if mode == MODE_SKIRMISH and not _templates.is_empty():
		var t := _templates[clampi(_template.get_selected_id(), 0, _templates.size() - 1)]
		var sk := {"template": Kit.get_int(t, "template"), "act": clampi(_act.get_selected_id(), 0, maxi(_act.item_count - 1, 0)), "enemy_per_player": int(_enemies.value)}
		var d := _difficulty.get_selected_id()
		if d >= 0 and d < _presets.size():
			sk["difficulty"] = {"preset": Kit.get_str(_presets[d], "preset")}
		out["skirmish"] = sk
	return out


func _push_setup() -> void:
	if _applying or not Kit.get_bool(_lobby, "is_host"):
		return
	_conditions_touched = true
	var ok: Variant = Kit.call_on(&"Lobby", &"set_setup", [setup_patch()])
	if ok != true:
		_status.text = tr(&"lobby.settings_refused")


func _show_status() -> void:
	if _countdown_end_ms >= 0:
		var left := maxi(0, ceili((_countdown_end_ms - Time.get_ticks_msec()) / 1000.0))
		_status.text = tr(&"lobby.starting").format([left])
		return
	if _lobby.is_empty():
		_status.text = tr(&"lobby.waiting")
	elif Kit.get_bool(_lobby, "can_start"):
		_status.text = tr(&"lobby.can_start_host") if Kit.get_bool(_lobby, "is_host") else tr(&"lobby.can_start_member")
	elif _ready_toggle.disabled:
		_status.text = tr(&"lobby.need_design")
	else:
		_status.text = tr(&"lobby.waiting_ready")


## The host started the sortie; it begins in `seconds`.
func show_starting(seconds: float) -> void:
	_countdown_end_ms = Time.get_ticks_msec() + roundi(maxf(seconds, 0.0) * 1000.0)
	_start.disabled = true
	_ready_toggle.disabled = true
	_show_status()
	set_process(true)


func _process(_delta: float) -> void:
	# Runs only during the start countdown; the label changes once a second.
	if _countdown_end_ms < 0:
		set_process(false)
		return
	if Time.get_ticks_msec() > _countdown_end_ms + 1000:
		_countdown_end_ms = -1
		set_process(false)
	_show_status()


func _on_left(text: Dictionary) -> void:
	var why := Kit._Text.resolve(text)
	if why.is_empty():
		return
	# The router returns to the menu when the session goes idle; say why once it has.
	Screens.open_message.call_deferred(&"lobby.left_title", text)


# --- actions -------------------------------------------------------------------------------------

func _refresh_field() -> void:
	_field.disabled = _picker.selected_path().is_empty() or _picker.blocked()
	_field_result.visible = false


func _on_field() -> void:
	var path := _picker.selected_path()
	if path.is_empty():
		return
	var result: Variant = Kit.call_on(&"Lobby", &"select_blueprint", [path])
	var problems := Kit.problem_texts(result, &"lobby.field_failed")
	if problems.is_empty():
		_field_result.text = tr(&"lobby.fielded").format([Kit.get_str(_picker.selected_info(), "name")])
	else:
		_field_result.text = Kit.problems_body(problems)
	_field_result.visible = true


func _on_ready(on: bool) -> void:
	_show_ready_toggle()
	Kit.call_on(&"Lobby", &"set_ready", [on])


## The toggle's word and icon say the state it is in now: "Ready" with a tick, or "Mark me ready".
func _show_ready_toggle() -> void:
	var on := _ready_toggle.button_pressed
	_ready_toggle.text = tr(&"lobby.is_ready_toggle") if on else tr(&"lobby.mark_ready")
	_ready_toggle.icon = Ui.icon(&"check_pass" if on else &"dot")


func _on_start() -> void:
	if _start.disabled:
		return
	var ok: Variant = Kit.call_on(&"Lobby", &"start")
	if ok != true:
		_status.text = tr(&"lobby.start_refused")


func _ask_leave() -> void:
	if not Kit.get_bool(_lobby, "is_host"):
		_leave_now()
		return
	var dialog := Screens.open_dialog(&"lobby.leave_title", &"lobby.leave_host_body",
		[{"id": &"stay", "label": &"lobby.stay"}, {"id": &"leave", "label": &"lobby.close_lobby"}], &"stay", &"stay")
	if dialog != null:
		dialog.connect(&"chosen", _on_leave_choice)


func _on_leave_choice(id: StringName) -> void:
	if id == &"leave":
		_leave_now()


func _leave_now() -> void:
	Kit.call_on(&"Lobby", &"leave")
	# The session going idle routes back to the menu; with no live session nothing would.
	if Screens.base_name() == &"lobby" and int(Kit.call_on(&"Session", &"state") if Kit.autoload(&"Session") != null else 0) == 0:
		Screens.change(&"multiplayer")


func _link_focus() -> void:
	var middle: Array = _picker.focus_controls()
	middle.append_array([_field, _team])
	var mid := Kit.chain_vertical(middle)
	var foot := [_ready_toggle, _start]
	Kit.chain_horizontal(foot)
	if not mid.is_empty():
		mid[-1].focus_neighbor_bottom = mid[-1].get_path_to(_ready_toggle)
		mid[0].focus_neighbor_top = mid[0].get_path_to(_leave_button)
	for c: Control in mid:
		c.focus_neighbor_left = c.get_path_to(_tree)
		c.focus_neighbor_right = c.get_path_to(_tabs.get_tab_bar())
	_tree.focus_neighbor_right = _tree.get_path_to(mid[0] if not mid.is_empty() else _ready_toggle)
	_tree.focus_neighbor_top = _tree.get_path_to(_leave_button)
	_tree.focus_neighbor_bottom = _tree.get_path_to(_ready_toggle)
	_ready_toggle.focus_neighbor_top = _ready_toggle.get_path_to(mid[-1] if not mid.is_empty() else _tree)
	_start.focus_neighbor_top = _start.get_path_to(_tabs.get_tab_bar())
	_leave_button.focus_neighbor_bottom = _leave_button.get_path_to(_tree)


# --- layout that never clips (L1714) ------------------------------------------------------------

func _responsive_body(parent: Control, step: int) -> BoxContainer:
	_body_scroll = ScrollContainer.new()
	_body_scroll.size_flags_vertical = Control.SIZE_EXPAND_FILL
	_body_scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	_body_scroll.follow_focus = true
	parent.add_child(_body_scroll)
	_body = BoxContainer.new()
	_body.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_body.size_flags_vertical = Control.SIZE_EXPAND_FILL
	_body_step = step
	Ui.set_separation(_body, step)
	_body_scroll.add_child(_body)
	_body_scroll.resized.connect(_fit_columns)
	return _body


## Columns side by side while they fit; one scrolling column when they do not, so a UI scale up to
## 200% never clips (L1714). `_fit` rows: [control, width side by side, height when stacked].
func _fit_columns() -> void:
	if _body_scroll == null:
		return
	var need := 0.0
	var count := 0
	for f: Array in _fit:
		if float(f[1]) > 0.0:
			need += float(f[1])
			count += 1
	need += Ui.spacing(_body_step) * maxi(count - 1, 0)
	var stack := _body_scroll.size.x + 1.0 < need
	_body.vertical = stack
	for f: Array in _fit:
		var c: Control = f[0]
		c.custom_minimum_size = Vector2(0.0 if stack else float(f[1]), float(f[2]) if stack else 0.0)
