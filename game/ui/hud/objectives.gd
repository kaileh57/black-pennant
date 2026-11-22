extends PanelContainer
## Objective and ghost conditions; allies.
## DESIGN.md L778-782. Unit: ui-hud-ship.
## Reads and calls: Hud.objectives, Hud.allies.
##
## The primary objective first, then the operation's ghost conditions in briefing order (L780:
## optional conditions that pay out Intel or blueprints). The panel sits at the foot of the left dock
## and shows the primary objective with one line counting the ghost conditions ("Ghost conditions:
## 1 failed, 1 open"); the chevron in its header (kept for the session) lists every condition, one
## row each: a state glyph and word
## (done: check, failed: cross; none while in play, rule 1), the objective's sentence, and its
## progress while it is partly met. A row with a map marker has a Show on plot button that switches
## to the plot and centres on it. Under a rule, the side's other hulls (co-op partners, SEXTANT,
## prizes): name, speed, hull damage, fire, flooding, radar silence, and officers in command while a
## partner has dropped. Each ally row centres the plot on that hull. Rows are updated in place by
## id; the list is rebuilt only when the set of ids changes, so a click is never lost to a rebuild.
## The panel hides outside a sortie, and while it has nothing to show. `show_lists(objectives,
## allies)` renders plain data (tests and captures); `refresh()` pulls it.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")

const GLYPH_PX := 24.0
const TEXT_MIN_W := 160.0

@onready var _margin: MarginContainer = $Margin
@onready var _column: VBoxContainer = $Margin/Column
@onready var _rows_box: VBoxContainer = $Margin/Column/Rows
@onready var _more: Button = $Margin/Column/Header/More
@onready var _summary: Label = $Margin/Column/Summary
@onready var _empty: Label = $Margin/Column/Empty
@onready var _allies_rule: HSeparator = $Margin/Column/AlliesRule
@onready var _allies_title: Label = $Margin/Column/AlliesTitle
@onready var _allies_box: VBoxContainer = $Margin/Column/Allies

var _objectives: Array[Dictionary] = []
var _allies: Array[Dictionary] = []
## Objective id to { "row", "glyph", "text", "show", "marker" }.
var _obj_rows: Dictionary[int, Dictionary] = {}
var _obj_order: Array[int] = []
## Ally vessel to { "button", "pos" }.
var _ally_rows: Dictionary[int, Dictionary] = {}
var _ally_order: Array[int] = []

## Every ghost condition listed, kept for the session.
static var expanded := false


func _ready() -> void:
	# Docked: the panel's own 16 px margin is the dock's inset (every docked cluster keeps 16 px,
	# so their text lines up down the dock).
	Ui.set_margins(_margin, 1)
	for side: StringName in [&"margin_left", &"margin_right", &"margin_top", &"margin_bottom"]:
		_margin.add_theme_constant_override(side, 0)
	Ui.set_separation(_column, 1)
	Ui.set_separation(_rows_box, 0)
	Ui.set_separation(_allies_box, 0)
	_empty.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_summary.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_more.focus_mode = Control.FOCUS_ALL
	_more.set_pressed_no_signal(expanded)
	_more.toggled.connect(func(on: bool) -> void:
		expanded = on
		show_lists(_objectives, _allies))
	accessibility_name = tr(&"hud_ship.objectives.title")
	_on(&"Hud", &"objectives_changed", refresh)
	_on(&"Hud", &"allies_changed", refresh)
	_on(&"Hud", &"ship_static_changed", refresh)
	_on(&"Session", &"state_changed", func(_s: int) -> void: refresh())
	if Ui.has_signal(&"changed"):
		Ui.changed.connect(_on_ui_changed)
	_apply_backing()
	refresh()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_obj_order.clear()
		_ally_order.clear()
		show_lists(_objectives, _allies)


func _on(autoload_name: StringName, signal_name: StringName, handler: Callable) -> void:
	var node := Kit.autoload(autoload_name)
	if node != null and node.has_signal(signal_name) and not node.is_connected(signal_name, handler):
		node.connect(signal_name, handler)


func _on_ui_changed(what: StringName) -> void:
	if what == &"backing" or what == &"transparency":
		_apply_backing()
	elif what == &"palette":
		show_lists(_objectives, _allies)


func _apply_backing() -> void:
	self_modulate.a = 1.0 if Ui.reduce_transparency else clampf(Ui.hud_backing_opacity, 0.0, 1.0)


## Controls that take focus, top to bottom (the HUD shell links panels with them).
func focus_targets() -> Array:
	var out: Array = []
	if _more.visible:
		out.append(_more)
	for id: int in _obj_order:
		var b: Button = _obj_rows[id]["show"]
		if b.is_visible_in_tree():
			out.append(b)
	for v: int in _ally_order:
		out.append(_ally_rows[v]["button"])
	return out


# --- data ----------------------------------------------------------------------------------------

func refresh() -> void:
	if not is_node_ready():
		return
	var state: Variant = Kit.call_on(&"Session", &"state")
	var live := state is int and (int(state) == Bp.SESSION_IN_SORTIE or int(state) == Bp.SESSION_REPLAY or int(state) == Bp.SESSION_ENDED)
	if not live:
		show_lists([], [])
		return
	show_lists(Kit.dicts(Kit.call_on(&"Hud", &"objectives")), Kit.dicts(Kit.call_on(&"Hud", &"allies")))


## Render `ObjectiveDict`s (primary first) and `AllyDict`s. Also the test entry point.
func show_lists(objectives: Array[Dictionary], allies: Array[Dictionary]) -> void:
	_objectives = objectives
	_allies = allies
	if not is_node_ready():
		return
	_show_objectives(objectives)
	_show_allies(allies)
	visible = not objectives.is_empty() or not allies.is_empty()
	_chain()


func _show_objectives(list: Array[Dictionary]) -> void:
	var ids: Array[int] = []
	for o: Dictionary in list:
		ids.append(Kit.get_int(o, "id", -1))
	if ids != _obj_order:
		Kit.clear(_rows_box)
		_obj_rows.clear()
		for i in list.size():
			_obj_rows[ids[i]] = _objective_row(ids[i])
		_obj_order = ids
	var described: PackedStringArray = []
	var ghosts := 0
	var counts: Dictionary[int, int] = {}
	for o: Dictionary in list:
		var id := Kit.get_int(o, "id", -1)
		var row: Dictionary = _obj_rows[id]
		var text := objective_text(o)
		(row["text"] as Label).text = text
		described.append(text)
		var ghost := Kit.get_bool(o, "ghost")
		(row["row"] as Control).visible = expanded or not ghost
		if ghost:
			ghosts += 1
			var st := Kit.get_int(o, "state", Bp.OBJECTIVE_ACTIVE)
			counts[st] = counts.get(st, 0) + 1
		var state := Kit.get_int(o, "state", Bp.OBJECTIVE_ACTIVE)
		var glyph: TextureRect = row["glyph"]
		var stem := glyph_for(state)
		glyph.texture = Ui.icon(stem) if stem != &"" else null
		glyph.self_modulate = Ui.colour(&"text.primary" if state == Bp.OBJECTIVE_DONE else &"text.secondary")
		var marker: Variant = o.get("marker")
		var has_marker := marker is Vector2 and (marker as Vector2).is_finite() and state == Bp.OBJECTIVE_ACTIVE
		row["marker"] = marker if has_marker else null
		(row["show"] as Button).visible = has_marker
	_empty.visible = list.is_empty() and not _allies.is_empty()
	_empty.text = tr(&"hud_ship.objectives.none")
	_more.visible = ghosts > 0
	var tip := tr(&"hud_ship.objectives.fewer" if expanded else &"hud_ship.objectives.more")
	_more.icon = Ui.icon(&"chevron_down" if expanded else &"chevron_up")
	_more.tooltip_text = tip
	_more.accessibility_name = tip
	_summary.text = ghost_summary(counts)
	_summary.visible = ghosts > 0 and not expanded
	accessibility_description = " ".join(described)


func _objective_row(id: int) -> Dictionary:
	var row := HBoxContainer.new()
	row.mouse_filter = Control.MOUSE_FILTER_IGNORE
	Ui.set_separation(row, 1)
	var glyph := TextureRect.new()
	glyph.custom_minimum_size = Vector2(GLYPH_PX, GLYPH_PX)
	glyph.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
	glyph.stretch_mode = TextureRect.STRETCH_KEEP_ASPECT_CENTERED
	glyph.size_flags_vertical = Control.SIZE_SHRINK_BEGIN
	glyph.mouse_filter = Control.MOUSE_FILTER_IGNORE
	row.add_child(glyph)
	var text := Kit.label("", &"Body", true)
	text.custom_minimum_size = Vector2(TEXT_MIN_W, 0.0)
	text.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	text.mouse_filter = Control.MOUSE_FILTER_IGNORE
	row.add_child(text)
	var show := Kit.icon_button(&"search", tr(&"hud_ship.objective.show"))
	show.custom_minimum_size = Vector2(32, 32)
	show.size_flags_vertical = Control.SIZE_SHRINK_BEGIN
	show.visible = false
	show.pressed.connect(_on_show_objective.bind(id))
	row.add_child(show)
	_rows_box.add_child(row)
	return {"row": row, "glyph": glyph, "text": text, "show": show, "marker": null}


func _show_allies(list: Array[Dictionary]) -> void:
	var ids: Array[int] = []
	for a: Dictionary in list:
		ids.append(Kit.get_int(a, "vessel", -1))
	if ids != _ally_order:
		Kit.clear(_allies_box)
		_ally_rows.clear()
		for v: int in ids:
			var b := Kit.button("")
			b.flat = true
			b.custom_minimum_size = Vector2(0, 32)
			b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
			b.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
			b.text_overrun_behavior = TextServer.OVERRUN_NO_TRIMMING
			b.tooltip_text = tr(&"hud_ship.allies.tooltip")
			b.pressed.connect(_on_show_ally.bind(v))
			_allies_box.add_child(b)
			_ally_rows[v] = {"button": b, "pos": null}
		_ally_order = ids
	for a: Dictionary in list:
		var v := Kit.get_int(a, "vessel", -1)
		var row: Dictionary = _ally_rows[v]
		var text := ally_text(a)
		var b: Button = row["button"]
		if b.text != text:
			b.text = text
		b.accessibility_name = text
		row["pos"] = a.get("pos")
	var any := not list.is_empty()
	_allies_rule.visible = any and not _objectives.is_empty()
	_allies_title.visible = any
	_allies_box.visible = any


func _chain() -> void:
	Kit.chain_vertical(focus_targets())


func _hud() -> Node:
	var n: Node = get_parent()
	while n != null:
		if n.has_method(&"centre_on_log"):
			return n
		n = n.get_parent()
	return null


func _on_show_objective(id: int) -> void:
	if not _obj_rows.has(id):
		return
	var marker: Variant = _obj_rows[id]["marker"]
	var hud := _hud()
	if hud != null and marker is Vector2:
		hud.call(&"centre_on_log", {"pos": marker})


func _on_show_ally(vessel: int) -> void:
	if not _ally_rows.has(vessel):
		return
	var pos: Variant = _ally_rows[vessel]["pos"]
	var hud := _hud()
	if hud != null and pos is Vector2:
		hud.call(&"centre_on_log", {"pos": pos, "vessel": vessel})


# --- text (static so tests share them) -----------------------------------------------------------

## "Ghost conditions: 1 met, 1 failed, 2 open"; "" with none.
static func ghost_summary(counts: Dictionary) -> String:
	var parts := PackedStringArray()
	for pair: Array in [[Bp.OBJECTIVE_DONE, &"hud_ship.objectives.ghosts_met"], [Bp.OBJECTIVE_FAILED, &"hud_ship.objectives.ghosts_failed"], [Bp.OBJECTIVE_ACTIVE, &"hud_ship.objectives.ghosts_open"]]:
		var n := int(counts.get(int(pair[0]), 0))
		if n > 0:
			parts.append(TranslationServer.translate(pair[1]).format([str(n)]))
	if parts.is_empty():
		return ""
	return TranslationServer.translate(&"hud_ship.objectives.ghosts").format([", ".join(parts)])


static func glyph_for(state: int) -> StringName:
	match state:
		Bp.OBJECTIVE_DONE: return &"check_pass"
		Bp.OBJECTIVE_FAILED: return &"check_fail"
	return &""


## The row's sentence: "Ghost condition: no radar emissions, failed", "Sink the convoy, 40%".
static func objective_text(o: Dictionary) -> String:
	var text := _Text.resolve(o.get("text", {}))
	if text.is_empty():
		text = TranslationServer.translate(&"hud_ship.objective.unnamed")
	if Kit.get_bool(o, "ghost"):
		text = TranslationServer.translate(&"hud_ship.objective.ghost").format([text])
	match Kit.get_int(o, "state", Bp.OBJECTIVE_ACTIVE):
		Bp.OBJECTIVE_DONE:
			return TranslationServer.translate(&"hud_ship.objective.done").format([text])
		Bp.OBJECTIVE_FAILED:
			return TranslationServer.translate(&"hud_ship.objective.failed").format([text])
	var p := Kit.get_float(o, "progress", 0.0)
	if p > 0.0 and p < 1.0:
		return TranslationServer.translate(&"hud_ship.objective.progress").format([text, Kit.number(p * 100.0, &"pct", 0)])
	return text


## "Kestrel: 14 kn, 8% hull lost, fire, flooding".
static func ally_text(a: Dictionary) -> String:
	var name := Kit.get_str(a, "name")
	if name.is_empty():
		name = TranslationServer.translate(&"hud_ship.allies.unnamed")
	var parts: PackedStringArray = [Kit.number(maxf(Kit.get_float(a, "speed_kn", 0.0), 0.0), &"kn", 0)]
	var lost := clampf(Kit.get_float(a, "cells_lost", 0.0), 0.0, 1.0)
	if lost >= 0.005:
		parts.append(TranslationServer.translate(&"hud_ship.allies.lost").format([Kit.number(lost * 100.0, &"pct", 0)]))
	if Kit.get_bool(a, "burning"):
		parts.append(TranslationServer.translate(&"hud_ship.allies.fire"))
	if Kit.get_bool(a, "flooding"):
		parts.append(TranslationServer.translate(&"hud_ship.allies.flooding"))
	if Kit.get_int(a, "emcon", Bp.EMCON_ACTIVE) == Bp.EMCON_SILENT:
		parts.append(TranslationServer.translate(&"hud_ship.allies.silent"))
	if Kit.get_bool(a, "officers_in_command"):
		parts.append(TranslationServer.translate(&"hud_ship.allies.officers"))
	return TranslationServer.translate(&"hud_ship.allies.row").format([name, ", ".join(parts)])
