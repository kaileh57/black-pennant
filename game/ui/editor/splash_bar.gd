extends Control
## In-splash bar: rewind, restart, apply edits, scope, end.
## DESIGN.md L216. Unit: ui-editor-splash.
## Reads and calls: Editor.splash_rewind, Editor.splash_restart, Editor.splash_apply_edits, Editor.splash_end,
## Editor.splash_options, Editor.graph_nodes, Editor.graph_live_values, Orders.query_scope, Hud.scope_value,
## Session.state.
##
## A bar centred along the top of the basin while a splash runs (the dock shows it on its splash layer).
## It owns a band across the top of the screen: the dock hands `band_height()` to the sortie HUD
## (`set_top_inset`), whose docks and clusters start below it, so the two never overlap. When the
## bar does not fit on one line (UI scale 200%) the basin sentence moves into End splash's tooltip; if
## the buttons still do not fit they wrap onto a second line rather than clip, and the band grows with
## them. Left to right:
## - Tune and edit (the dock's toggle, handed over with `add_lead`): the build panels over the basin.
## - Rewind 30 s (also the `splash_rewind` key, R): the basin goes back up to 30 s (L216).
## - Restart: the same basin from the beginning.
## - Apply edits: pushes the edits made in the Tune dock into the running basin (kits, tuning, rules,
##   graphs, scripts), and names the number of edits made since the last push; a changed hull shell
##   closes the basin and the dock says why instead (L216).
## - Scope: a menu of every output port of the control graph, by officer; a checked port streams its value
##   (`Orders.query_scope`), shown in a readout under the bar and printed on the graph's edges.
## - The basin: water and target, so the player knows what they are fighting.
## - End splash: closes the basin; the report card follows (`Editor.splash_ended`).
## Until the basin is running (`Bp.SESSION_IN_SORTIE`) the bar says it is filling and only End works.
## The buttons take no keyboard or gamepad focus, so the pad keeps driving the helm and mounts as in a
## sortie (L216); the pause menu (Esc, Start) also ends a splash. End carries a `menu.*` tutorial id, which
## every tutorial step allows, so a step never traps the player in the basin.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")

## Seconds one rewind goes back (L216).
const REWIND_S := 30.0
## The scope menu lists at most this many ports per officer.
const MAX_PORTS := 120
## The basin in a sentence: water and target in running text (sentence case, L1695).
const WATER_KEYS: Dictionary[int, StringName] = {0: &"editor_splash.basin.calm", 3: &"editor_splash.basin.b4", 6: &"editor_splash.basin.b7"}
const TARGET_KEYS: Array[StringName] = [&"editor_splash.basin.drones", &"editor_splash.basin.patrol"]

## The band under the bar changed height (`band_height`).
signal band_changed(height: float)

var _panel: PanelContainer
var _row: HFlowContainer
var _settle_queued := false
var _rewind: Button
var _restart: Button
var _apply: Button
var _scope: MenuButton
var _basin: Label
## The rules either side of the basin sentence, hidden with it on a narrow canvas.
var _basin_rules: Array[Control] = []
var _end: Button
var _readout: PanelContainer
var _readout_box: VBoxContainer
var _state := -1
var _edits := 0
var _in_splash := false
## Scoped ports: "node:port" to `{ "node", "port", "label" }`.
var _scoped: Dictionary[String, Dictionary] = {}
var _values: Dictionary[String, Label] = {}
## Popup item id to port.
var _menu_ports: Dictionary[int, Dictionary] = {}


func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	accessibility_name = tr(&"editor_splash.bar.name")
	_panel = PanelContainer.new()
	_panel.name = &"Panel"
	_panel.set_anchors_and_offsets_preset(Control.PRESET_CENTER_TOP, Control.PRESET_MODE_MINSIZE)
	_panel.grow_horizontal = Control.GROW_DIRECTION_BOTH
	add_child(_panel)
	var margin := MarginContainer.new()
	Ui.set_margins(margin, 1)
	_panel.add_child(margin)
	var row := HFlowContainer.new()
	row.name = &"Row"
	row.alignment = FlowContainer.ALIGNMENT_CENTER
	row.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	row.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	_row = row
	margin.add_child(row)
	_rewind = _button("editor_splash.rewind", "editor_splash.rewind.tip", "splash.rewind")
	_rewind.icon = Ui.icon(&"arrow_revert")
	_rewind.pressed.connect(_on_rewind)
	row.add_child(_rewind)
	_restart = _button("editor_splash.restart", "editor_splash.restart.tip", "splash.restart")
	_restart.icon = Ui.icon(&"reload")
	_restart.pressed.connect(_on_restart)
	row.add_child(_restart)
	_apply = _button("editor_splash.apply", "editor_splash.apply.tip", "splash.apply")
	_apply.pressed.connect(_on_apply)
	row.add_child(_apply)
	_scope = MenuButton.new()
	_scope.text = "editor_splash.scope"
	_scope.flat = false
	_scope.focus_mode = Control.FOCUS_NONE
	_scope.custom_minimum_size = Vector2(0.0, Kit.BUTTON_H)
	_scope.tooltip_text = tr(&"editor_splash.scope.tip")
	_scope.set_meta(&"control_id", "splash.scope")
	_scope.about_to_popup.connect(_fill_scope_menu)
	_scope.get_popup().id_pressed.connect(_on_scope_picked)
	row.add_child(_scope)
	var rule_a := VSeparator.new()
	row.add_child(rule_a)
	_basin = Kit.label("", &"Body", true)
	_basin.autowrap_mode = TextServer.AUTOWRAP_OFF
	_basin.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	row.add_child(_basin)
	var rule_b := VSeparator.new()
	row.add_child(rule_b)
	_basin_rules = [rule_a, rule_b]
	_end = _button("editor_splash.end", "editor_splash.end.tip", "menu.end_splash")
	_end.pressed.connect(_on_end)
	row.add_child(_end)
	_readout = PanelContainer.new()
	_readout.name = &"Readout"
	_readout.visible = false
	_readout.mouse_filter = Control.MOUSE_FILTER_IGNORE
	add_child(_readout)
	var rm := MarginContainer.new()
	Ui.set_margins(rm, 1)
	_readout.add_child(rm)
	_readout_box = Kit.vbox(0)
	rm.add_child(_readout_box)
	_panel.resized.connect(_place_readout)
	_panel.resized.connect(_emit_band)
	_readout.visibility_changed.connect(_emit_band)
	_readout.resized.connect(_emit_band)
	visibility_changed.connect(_emit_band)
	get_viewport().size_changed.connect(_fit_width)
	row.minimum_size_changed.connect(_queue_settle)
	Ui.changed.connect(func(_what: StringName) -> void: _fit_width.call_deferred())
	for c: Node in row.get_children():
		_watch(c as Control)
	var e := Kit.autoload(&"Editor")
	if e != null and e.has_signal(&"design_changed"):
		e.connect(&"design_changed", _on_design_changed)
		e.connect(&"splash_ended", func(_card: Dictionary) -> void: _on_splash_over())
		e.connect(&"returned_to_dock", func(_t: Dictionary) -> void: _on_splash_over())
	var s := Kit.autoload(&"Session")
	if s != null and s.has_signal(&"state_changed"):
		s.connect(&"state_changed", _on_session_state)
	var tutorial := Kit.autoload(&"Tutorial")
	if tutorial != null and tutorial.has_signal(&"step_changed"):
		tutorial.connect(&"step_changed", func(_s: Dictionary) -> void: _refresh())
	var h := Kit.autoload(&"Hud")
	if h != null and h.has_signal(&"scope_value"):
		h.connect(&"scope_value", _on_scope_value)
	_on_session_state(int(s.call(&"state")) if s != null and s.has_method(&"state") else -1)
	_fit_width.call_deferred()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		accessibility_name = tr(&"editor_splash.bar.name")
		_refresh()


func _button(key: String, tip: String, id: String) -> Button:
	var b := Kit.inline_button(key)
	b.focus_mode = Control.FOCUS_NONE
	b.tooltip_text = tr(tip)
	b.set_meta(&"control_id", id)
	return b


func _place_readout() -> void:
	_readout.position = Vector2(_panel.position.x, _panel.position.y + _panel.size.y + Ui.spacing(1))


## Put a host control (the dock's Tune and edit toggle) at the start of the bar.
func add_lead(control: Control) -> void:
	if control == null:
		return
	if control.get_parent() != null:
		control.get_parent().remove_child(control)
	control.owner = null
	control.custom_minimum_size = Vector2(0.0, Kit.BUTTON_H)
	control.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	_row.add_child(control)
	_row.move_child(control, 0)
	_watch(control)
	_fit_width.call_deferred()


## How far down from the top of the screen the bar and its scope readout reach, with the gap under them;
## 0 while the bar is hidden.
func band_height() -> float:
	if not is_visible_in_tree() or _panel == null:
		return 0.0
	var bottom := _panel.position.y + _panel.size.y
	if _readout.visible:
		bottom = maxf(bottom, _readout.position.y + _readout.size.y)
	return position.y + bottom + float(Ui.spacing(1))


func _watch(c: Control) -> void:
	if c == null:
		return
	c.minimum_size_changed.connect(_fit_width, CONNECT_DEFERRED)
	c.visibility_changed.connect(_fit_width, CONNECT_DEFERRED)


## One line when the buttons fit the screen, else as many lines as they need, centred, never wider than the
## screen less its edge margins.
func _fit_width() -> void:
	if _row == null or not is_inside_tree():
		return
	var chrome := _panel.get_combined_minimum_size().x - _row.get_combined_minimum_size().x
	var room := get_viewport_rect().size.x - float(Ui.spacing(3) * 2) - chrome
	# On a narrow canvas (UI scale 200%) the basin sentence goes first, into End splash's tooltip, so the
	# buttons keep one line and the basin's HUD keeps its height; only if they still do not fit do they wrap.
	var full := _natural_width(true)
	var compact := full > room + 0.5
	_basin.visible = not compact
	for c: Node in _basin_rules:
		(c as Control).visible = not compact
	_end.tooltip_text = tr(&"editor_splash.end.tip") if not compact else "%s\n%s" % [_basin.text, tr(&"editor_splash.end.tip")]
	var natural := full if not compact else _natural_width(false)
	var w := floorf(minf(natural, maxf(room, 0.0)))
	if not is_equal_approx(_row.custom_minimum_size.x, w):
		_row.custom_minimum_size.x = w
	_queue_settle()


## The row's width on one line, with or without the basin sentence and its rules.
func _natural_width(with_basin: bool) -> float:
	var sep := float(Ui.spacing(1))
	var natural := 0.0
	var count := 0
	for c: Node in _row.get_children():
		var ctl := c as Control
		if ctl == null:
			continue
		var basin_part := ctl == _basin or _basin_rules.has(ctl)
		if basin_part and not with_basin:
			continue
		if not basin_part and not ctl.visible:
			continue
		natural += ctl.get_combined_minimum_size().x
		count += 1
	return natural + sep * float(maxi(count - 1, 0))


## The flow's height follows its width only after it sorts, and a panel outside a container never shrinks
## by itself: once the row has settled, the panel takes its minimum again and stays centred.
func _queue_settle() -> void:
	if _settle_queued:
		return
	_settle_queued = true
	_settle.call_deferred()


func _settle() -> void:
	_settle_queued = false
	if _panel == null:
		return
	_panel.reset_size()
	_panel.set_anchors_and_offsets_preset(Control.PRESET_CENTER_TOP, Control.PRESET_MODE_MINSIZE)
	_emit_band()


func _emit_band() -> void:
	band_changed.emit(band_height())


func _on_session_state(state: int) -> void:
	_state = state
	var now := bool(Kit.call_on(&"Editor", &"in_splash"))
	if now and not _in_splash:
		_edits = 0
		_scoped.clear()
		_values.clear()
	_in_splash = now
	_refresh()


func _on_design_changed(_rev: int) -> void:
	if _in_splash:
		_edits += 1
		_refresh()


func _on_splash_over() -> void:
	_in_splash = false
	_edits = 0
	_scoped.clear()
	_values.clear()
	_refresh()


func _running() -> bool:
	return _in_splash and _state == Bp.SESSION_IN_SORTIE


func _refresh() -> void:
	var running := _running()
	_rewind.disabled = not running
	_restart.disabled = not running
	# Rust pushes nothing when the basin already has this design, so the button never needs to be held.
	_apply.disabled = not running
	_apply.text = tr(&"editor_splash.apply_count").format([Kit.number(float(_edits), &"none", 0)]) if _edits > 0 else tr(&"editor_splash.apply")
	_apply.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_scope.disabled = not running
	_end.disabled = not _in_splash
	var o := Kit.as_dict(Kit.call_on(&"Editor", &"splash_options"))
	var water: StringName = WATER_KEYS.get(int(o.get("water", 3)), &"editor_splash.basin.b4")
	var target: StringName = TARGET_KEYS[clampi(int(o.get("target", 0)), 0, TARGET_KEYS.size() - 1)]
	if _in_splash and not running:
		_basin.text = tr(&"editor_splash.filling")
	else:
		_basin.text = tr(&"editor_splash.basin").format([tr(water), tr(target)])
	_apply_tutorial()
	_render_readout()


func _apply_tutorial() -> void:
	var t := Kit.autoload(&"Tutorial")
	if t == null or not t.has_method(&"is_allowed"):
		return
	for b: BaseButton in [_rewind, _restart, _apply, _scope, _end]:
		if not bool(t.call(&"is_allowed", String(b.get_meta(&"control_id")))):
			b.disabled = true


func _unhandled_input(event: InputEvent) -> void:
	if not _running() or not is_visible_in_tree():
		return
	if event.is_action_pressed(&"splash_rewind", false, true):
		_on_rewind()
		get_viewport().set_input_as_handled()


# --- actions ----------------------------------------------------------------------------------------

func _on_rewind() -> void:
	if not _rewind.disabled and bool(Kit.call_on(&"Editor", &"splash_rewind", [REWIND_S])):
		# A rewind pushes the current design into the basin again: nothing is left waiting.
		_edits = 0
		_refresh()


func _on_restart() -> void:
	Kit.call_on(&"Editor", &"splash_restart")


func _on_apply() -> void:
	if bool(Kit.call_on(&"Editor", &"splash_apply_edits")):
		_edits = 0
	_refresh()


func _on_end() -> void:
	Kit.call_on(&"Editor", &"splash_end")


# --- scope ------------------------------------------------------------------------------------------

## The menu: one submenu per officer, each output port checkable.
func _fill_scope_menu() -> void:
	var popup := _scope.get_popup()
	for c: Node in popup.get_children():
		if c is PopupMenu:
			popup.remove_child(c)
			c.queue_free()
	popup.clear()
	_menu_ports.clear()
	var nodes := Kit.dicts(Kit.call_on(&"Editor", &"graph_nodes", [Bp.NONE]))
	var by_officer: Dictionary[int, Array] = {}
	for n: Dictionary in nodes:
		if Kit.dicts(n.get("outputs", [])).is_empty():
			continue
		var officer := int(n.get("officer", -1))
		if not by_officer.has(officer):
			by_officer[officer] = []
		by_officer[officer].append(n)
	if by_officer.is_empty():
		popup.add_item(tr(&"editor_splash.scope.none"))
		popup.set_item_disabled(0, true)
		return
	var next_id := 0
	var officers: Array = by_officer.keys()
	officers.sort()
	for officer: int in officers:
		var sub := PopupMenu.new()
		sub.name = "Officer%d" % (officer + 1)
		sub.id_pressed.connect(_on_scope_picked)
		var count := 0
		for n: Dictionary in by_officer[officer]:
			var title := _Text.resolve(n.get("title", {}))
			var outs := Kit.dicts(n.get("outputs", []))
			for p in outs.size():
				if count >= MAX_PORTS:
					break
				var port_name := tr(StringName(str(outs[p].get("name", ""))))
				var label := tr(&"editor_splash.scope.port").format([title, port_name])
				var key := "%d:%d" % [int(n.get("id", -1)), p]
				sub.add_check_item(label, next_id)
				sub.set_item_checked(sub.get_item_index(next_id), _scoped.has(key))
				_menu_ports[next_id] = {"node": int(n.get("id", -1)), "port": p, "label": label, "key": key}
				next_id += 1
				count += 1
		var officer_name := String(Kit.call_on(&"Tx", &"word", [&"officer", officer])) if officer >= 0 else ""
		if officer_name.is_empty():
			officer_name = tr(&"editor_splash.scope.ship")
		popup.add_submenu_node_item(officer_name, sub)


func _on_scope_picked(id: int) -> void:
	var p: Dictionary = _menu_ports.get(id, {})
	if p.is_empty():
		return
	var key := String(p["key"])
	var on := not _scoped.has(key)
	if not bool(Kit.call_on(&"Orders", &"query_scope", [int(p["node"]), int(p["port"]), on])):
		return
	if on:
		_scoped[key] = p
	else:
		_scoped.erase(key)
		_values.erase(key)
	_render_readout()


func _on_scope_value(node: int, port: int, value: Dictionary) -> void:
	var key := "%d:%d" % [node, port]
	var label: Label = _values.get(key)
	if label == null or not is_instance_valid(label):
		return
	var live := Kit.as_dict(Kit.call_on(&"Editor", &"graph_live_values"))
	var text := String(live.get(key, ""))
	label.text = text if not text.is_empty() else value_text(value)


## A `ValueDict` as text when the editor has not formatted it yet.
static func value_text(v: Dictionary) -> String:
	var t := StringName(str(v.get("type", "none")))
	var x: Variant = v.get("value", null)
	match t:
		&"bool":
			return TranslationServer.translate(&"editor_splash.value.on" if bool(x) else &"editor_splash.value.off")
		&"scalar":
			return Kit.number(float(x), &"none", 2)
		&"vec3":
			if x is Vector3:
				var v3 := x as Vector3
				return "%s, %s, %s" % [Kit.number(v3.x, &"none", 1), Kit.number(v3.y, &"none", 1), Kit.number(v3.z, &"none", 1)]
		&"text":
			return str(x)
		&"stance", &"emcon", &"preset", &"rung":
			return String(Kit.call_on(&"Tx", &"word", [t, int(x)]))
	return TranslationServer.translate(&"editor_splash.value.none")


func _render_readout() -> void:
	Kit.clear(_readout_box)
	_values.clear()
	_readout.visible = _running() and not _scoped.is_empty()
	if not _readout.visible:
		return
	for key: String in _scoped:
		var row := Kit.hbox(2)
		var name_label := Kit.label(String(_scoped[key]["label"]), &"Dense", true)
		name_label.autowrap_mode = TextServer.AUTOWRAP_OFF
		row.add_child(name_label)
		var value := Kit.label(tr(&"editor_splash.value.waiting"), &"Body", true)
		value.autowrap_mode = TextServer.AUTOWRAP_OFF
		value.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		value.horizontal_alignment = HORIZONTAL_ALIGNMENT_RIGHT
		row.add_child(value)
		_readout_box.add_child(row)
		_values[key] = value
	_readout.reset_size()
	_place_readout.call_deferred()
