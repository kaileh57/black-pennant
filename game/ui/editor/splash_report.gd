extends Control
## Report card with one tip.
## DESIGN.md L216. Unit: ui-editor-splash.
## Reads and calls: Editor.splash_ended, Editor.returned_to_dock, Editor.in_splash, Session.state_changed.
##
## Every splash ends with a report card and one tip (L216). It opens over the dry dock on
## `Editor.splash_ended(card)` (`ReportCardDict`), behind a scrim that keeps clicks off the dock:
## - the tip first, because it is what to do next (the simulation's pick, else the dock's own notes);
## - why the basin closed, when a live edit changed the hull shell or was refused (`returned_to_dock`);
## - the fight as a spec sheet: time in the basin, score, rounds fired and hit rate, targets destroyed,
##   time to the first hit, damage taken and peak heel;
## - "Back to the design" (focused; Esc too) and "Splash again" (Space, as in the dock).
## It hides itself when a new splash starts, and gives focus back to what had it before it opened.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const HINT_SCENE := preload("res://ui/widgets/shortcut_hint.tscn")

var _scrim: ColorRect
var _panel: PanelContainer
var _tip: Label
var _closed_why: Label
var _grid: GridContainer
var _back: Button
var _again: Button
var _card: Dictionary = {}
var _returned: Dictionary = {}
var _focus_before: Control


func _ready() -> void:
	visible = false
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	_scrim = ColorRect.new()
	_scrim.name = &"Scrim"
	_scrim.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	_scrim.mouse_filter = Control.MOUSE_FILTER_STOP
	_scrim.color = Ui.scrim_colour()
	add_child(_scrim)
	var centre := CenterContainer.new()
	centre.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	centre.mouse_filter = Control.MOUSE_FILTER_IGNORE
	add_child(centre)
	_panel = PanelContainer.new()
	_panel.name = &"Panel"
	_panel.custom_minimum_size = Vector2(640.0, 0.0)
	centre.add_child(_panel)
	var margin := MarginContainer.new()
	Ui.set_margins(margin, 4)
	_panel.add_child(margin)
	var column := Kit.vbox(3)
	margin.add_child(column)
	column.add_child(Kit.label(&"editor_splash.report.title", &"PanelTitle"))
	_tip = Kit.label("", &"Body", true)
	_tip.custom_minimum_size.x = 560.0
	column.add_child(_tip)
	_closed_why = Kit.label("", &"Dense", true)
	_closed_why.visible = false
	column.add_child(_closed_why)
	column.add_child(HSeparator.new())
	_grid = GridContainer.new()
	_grid.columns = 4
	_grid.add_theme_constant_override(&"h_separation", Ui.spacing(4))
	_grid.add_theme_constant_override(&"v_separation", Ui.spacing(0))
	column.add_child(_grid)
	var actions := Kit.hbox(2)
	column.add_child(actions)
	_back = Kit.inline_button("editor_splash.report.back")
	_back.custom_minimum_size.y = Kit.PRIMARY_H
	_back.pressed.connect(close)
	actions.add_child(_back)
	_again = Kit.inline_button("editor_splash.report.again")
	_again.custom_minimum_size.y = Kit.PRIMARY_H
	_again.tooltip_text = tr(&"editor_splash.report.again.tip")
	_again.set_meta(&"control_id", "editor.splash")
	_again.pressed.connect(_on_again)
	actions.add_child(_again)
	var hint := HINT_SCENE.instantiate() as Control
	if hint != null:
		hint.set(&"action", &"splash")
		hint.set(&"compact", true)
		hint.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		actions.add_child(hint)
	Kit.chain_horizontal([_back, _again])
	var e := Kit.autoload(&"Editor")
	if e != null and e.has_signal(&"splash_ended"):
		e.connect(&"splash_ended", show_card)
		e.connect(&"returned_to_dock", func(text: Dictionary) -> void: _returned = text)
	var s := Kit.autoload(&"Session")
	if s != null and s.has_signal(&"state_changed"):
		s.connect(&"state_changed", _on_session_state)
	var tutorial := Kit.autoload(&"Tutorial")
	if tutorial != null and tutorial.has_signal(&"step_changed"):
		tutorial.connect(&"step_changed", func(_s: Dictionary) -> void:
			if visible:
				_render())
	var u := get_node_or_null(^"/root/Ui")
	if u != null and u.has_signal(&"changed"):
		u.connect(&"changed", func(_what: StringName) -> void: _scrim.color = Ui.scrim_colour())


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready() and visible:
		_render()


## Open the card for a `ReportCardDict`.
func show_card(card: Dictionary) -> void:
	_card = card
	_render()
	_returned = {}
	var owner_now := get_viewport().gui_get_focus_owner() if is_inside_tree() else null
	_focus_before = owner_now if owner_now != null and not is_ancestor_of(owner_now) else null
	visible = true
	accessibility_name = tr(&"editor_splash.report.title")
	Ui.focus(_back)


func close() -> void:
	if not visible:
		return
	visible = false
	if _focus_before != null and is_instance_valid(_focus_before) and _focus_before.is_visible_in_tree():
		Ui.focus(_focus_before)
	_focus_before = null


func _on_session_state(state: int) -> void:
	# A new splash started (Space in the dock, or Splash again): the old card goes.
	if visible and (state == Bp.SESSION_LOADING or state == Bp.SESSION_IN_SORTIE) and bool(Kit.call_on(&"Editor", &"in_splash")):
		close()


func _f(key: String) -> float:
	var v: Variant = _card.get(key, 0.0)
	return float(v) if (v is float or v is int) and is_finite(float(v)) else 0.0


func _render() -> void:
	var tip := _Text.resolve(_card.get("tip", {}))
	_tip.text = tr(&"editor_splash.report.tip").format([tip]) if not tip.is_empty() else tr(&"editor_splash.report.no_tip")
	var why := _Text.resolve(_returned)
	_closed_why.text = why
	_closed_why.visible = not why.is_empty()
	Kit.clear(_grid)
	var first_hit := _f("time_to_first_hit_s")
	var rows: Array = [
		[&"editor_splash.report.duration", Kit.number(_f("duration_s"), &"s", 0)],
		[&"editor_splash.report.score", Kit.number(_f("score"), &"none", 0)],
		[&"editor_splash.report.rounds", Kit.number(_f("rounds_fired"), &"none", 0)],
		[&"editor_splash.report.hit_rate", Kit.percent(_f("hit_rate")) if _f("rounds_fired") > 0.0 else tr(&"editor_splash.report.no_shots")],
		[&"editor_splash.report.destroyed", Kit.number(_f("targets_destroyed"), &"none", 0)],
		[&"editor_splash.report.first_hit", Kit.number(first_hit, &"s", 1) if first_hit >= 0.0 else tr(&"editor_splash.report.no_hit")],
		[&"editor_splash.report.damage", Kit.percent(_f("damage_taken"))],
		[&"editor_splash.report.heel", Kit.number(_f("peak_heel_deg"), &"deg", 0)],
	]
	for r: Array in rows:
		var l := Kit.label(String(r[0]), &"Dense")
		l.autowrap_mode = TextServer.AUTOWRAP_OFF
		_grid.add_child(l)
		var v := Kit.label(String(r[1]), &"Body", true)
		v.autowrap_mode = TextServer.AUTOWRAP_OFF
		v.horizontal_alignment = HORIZONTAL_ALIGNMENT_RIGHT
		v.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		_grid.add_child(v)
	var t := Kit.autoload(&"Tutorial")
	_again.disabled = t != null and t.has_method(&"is_allowed") and not bool(t.call(&"is_allowed", "editor.splash")) and not bool(t.call(&"is_allowed", "editor.launch"))


func _unhandled_input(event: InputEvent) -> void:
	if not visible:
		return
	if event.is_action_pressed(&"ui_cancel", false, true):
		close()
		get_viewport().set_input_as_handled()


## Splash again: the same as the splash key in the dock (Space unless rebound), which saves and starts the
## basin with the same options. The dock reads that key, so the key the player bound is sent.
func _on_again() -> void:
	close()
	for ev: InputEvent in InputMap.action_get_events(&"splash"):
		if ev is InputEventKey:
			for pressed: bool in [true, false]:
				var key := (ev as InputEventKey).duplicate() as InputEventKey
				key.pressed = pressed
				Input.parse_input_event(key)
			return
