extends VBoxContainer
## Compartment schematic, teams, repair order, pumps, doors, valves.
## DESIGN.md L622-727, L1651. Unit: ui-hud-damage.
## Reads and calls: Hud.compartments_static, Hud.compartments, Hud.crew, Hud.damage_controls, Hud.hull_outline, Orders.assign_team, Orders.reorder_repairs, Orders.set_pump, Orders.set_door, Orders.set_flood_valve, Orders.flood_magazine.
##
## The damage-control station, opened from the HUD shell's station buttons. Top to bottom:
## - Summary: fires, flooding and breached compartments, damaged parts, teams standing by, pumps
##   running, heel. "No damage" when there is none.
## - Schematic: the hull in profile (every deck) over the hull in plan (one deck; the deck buttons
##   above choose it, and picking a compartment in profile moves to its deck). Damage reads by
##   value and pattern, never hue (L1651, HUD rule 2): flooding is horizontal lines every 4 px up to
##   the water level in profile and over the compartment in plan; damaged parts carry the 45 degree
##   hatch of their level; a burning compartment shows the fire glyph; a breached one a dashed
##   frame; a magazine a double frame; the selection the white bracket. Compartment numbers match
##   the team and log texts. A legend names every mark. Click or tap a compartment to select it;
##   with focus on the schematic, Left and Right step through compartments and Up and Down change
##   deck (keyboard and gamepad, L1738).
## - The selected compartment: its deck, volume and state, its parts with their damage, and the
##   jobs that fit it: send a team to fight the fire or patch the breach (only under 50% flooded,
##   L652), repair a part (not destroyed ones: tender only, L723), open or close its flood valve
##   (below the waterline, L654), flood its magazine. Flooding a magazine asks first: it cannot be
##   undone in the sortie (L668).
## - Teams: each team's job and progress; a job you set shows "your order" and a Hand back button
##   that returns the team to the officer. Teams come from berths (L703); with no surviving berths
##   no job starts, and the send buttons say so.
## - Repair order: the six priorities of L713-719 with Move up and Move down (no drag, floor 8).
## - Pumps (run or stop, and which compartment: the officer's choice, its own or an adjacent one,
##   L640), doors (open or close) and magazines.
## The team chosen for a job is the lowest-numbered team standing by, else the lowest-numbered
## team the officer is directing, else team 1. Data arrives at 2 Hz; rows update in place and are
## rebuilt only when what they list changes, so a click is never lost. States: outside a sortie,
## waiting for the first picture, no hull, no damage, damaged. `show_state(state)` renders plain
## data (tests and captures); `refresh()` pulls it.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const HatchScene := preload("res://ui/widgets/damage_hatch.tscn")
const LegendScene := preload("res://ui/widgets/legend_row.tscn")

## A breach face can be patched only below this flooded fraction (L652).
const PATCH_LIMIT := 0.5
const REPAIR_CAP := 0.75
const ROW_H := 36.0

@onready var _summary: Label = $Summary
@onready var _scroll: ScrollContainer = $Scroll
@onready var _column: VBoxContainer = $Scroll/Column
@onready var _decks_row: HBoxContainer = $Scroll/Column/Decks
@onready var _legend: HFlowContainer = $Scroll/Column/Legend
@onready var _detail: VBoxContainer = $Scroll/Column/Detail
@onready var _teams_box: VBoxContainer = $Scroll/Column/Teams
@onready var _order_box: VBoxContainer = $Scroll/Column/Order
@onready var _equipment: VBoxContainer = $Scroll/Column/Equipment
@onready var _empty: Label = $Empty

var _schematic: Schematic
var _state: Dictionary = {}
var _statics: Array[Dictionary] = []
var _static_by_id: Dictionary[int, Dictionary] = {}
var _live_by_id: Dictionary[int, Dictionary] = {}
var _crew: Dictionary = {}
var _controls: Array[Dictionary] = []
var _parts_static: Array[Dictionary] = []
var _parts_by_slot: Dictionary[int, Dictionary] = {}
var _selected := Bp.NONE
var _deck := 0
var _decks: Array[int] = []
var _deck_buttons: Array[Button] = []
## Detail controls, built once.
var _d_title: Label
var _d_facts: Label
var _d_state: Label
var _d_parts: VBoxContainer
var _d_parts_for := -2
var _d_fire: Button
var _d_patch: Button
var _d_valve: Button
var _d_mags: VBoxContainer
var _d_hint: Label
var _d_actions: HFlowContainer
## Rows by key.
var _team_rows: Array[Dictionary] = []
var _order_rows: Array[Dictionary] = []
var _order: Array[int] = []
var _equip_key: Array = []
var _equip_rows: Dictionary[String, Dictionary] = {}
var _pump_targets: Dictionary[int, int] = {}
var _part_rows: Dictionary[int, Dictionary] = {}


func _ready() -> void:
	Ui.set_separation(self, 1)
	Ui.set_separation(_column, 1)
	Ui.set_separation(_decks_row, 1)
	Ui.set_separation(_detail, 1)
	Ui.set_separation(_teams_box, 0)
	Ui.set_separation(_order_box, 0)
	Ui.set_separation(_equipment, 0)
	_legend.add_theme_constant_override(&"h_separation", Ui.spacing(2))
	_summary.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_empty.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_schematic = Schematic.new()
	_schematic.name = "Schematic"
	_schematic.picked.connect(_on_picked)
	_schematic.deck_step.connect(_on_deck_step)
	_column.add_child(_schematic)
	_column.move_child(_schematic, _decks_row.get_index() + 1)
	_build_legend()
	_build_detail()
	_build_order()
	accessibility_name = tr(&"hud_damage.title")
	_on(&"Hud", &"compartments_changed", refresh)
	_on(&"Hud", &"crew_changed", refresh)
	_on(&"Hud", &"parts_changed", refresh)
	_on(&"Hud", &"ship_static_changed", _on_static)
	_on(&"Session", &"state_changed", func(_s: int) -> void: _on_static())
	refresh()


func _notification(what: int) -> void:
	match what:
		NOTIFICATION_TRANSLATION_CHANGED:
			if is_node_ready():
				_build_legend()
				_decks = []
				_equip_key = []
				_d_parts_for = -2
				show_state(_state)
		NOTIFICATION_VISIBILITY_CHANGED:
			if is_node_ready() and is_visible_in_tree():
				refresh()


func _on(autoload_name: StringName, signal_name: StringName, handler: Callable) -> void:
	var node := Kit.autoload(autoload_name)
	if node != null and node.has_signal(signal_name) and not node.is_connected(signal_name, handler):
		node.connect(signal_name, handler)


func _on_static() -> void:
	_selected = Bp.NONE
	_pump_targets.clear()
	refresh()


# --- data ----------------------------------------------------------------------------------------

func refresh() -> void:
	if not is_node_ready():
		return
	# Hidden in the station stack: nothing to draw until the player opens it.
	if not is_visible_in_tree() and not _state.is_empty():
		return
	var session_state: Variant = Kit.call_on(&"Session", &"state")
	var s := int(session_state) if session_state is int else Bp.SESSION_IDLE
	show_state({
		"session": s,
		"statics": Kit.dicts(Kit.call_on(&"Hud", &"compartments_static")),
		"live": Kit.dicts(Kit.call_on(&"Hud", &"compartments")),
		"crew": Kit.as_dict(Kit.call_on(&"Hud", &"crew")),
		"controls": Kit.dicts(Kit.call_on(&"Hud", &"damage_controls")),
		"outline": Kit.as_dict(Kit.call_on(&"Hud", &"hull_outline")),
		"summary": Kit.as_dict(Kit.call_on(&"Hud", &"damage_summary")),
		"parts_static": Kit.dicts(Kit.call_on(&"Hud", &"parts_static")),
		"parts": Kit.dicts(Kit.call_on(&"Hud", &"parts")),
	})


## Render the station from plain data: `{ session: int, statics: Array[CompartmentStaticDict],
## live: Array[CompartmentDict], crew: CrewDict, controls: damage_controls(), outline:
## hull_outline(), summary: damage_summary(), parts_static: Array[PartStaticDict], parts:
## Array[PartDict] }`. Also the test and capture entry point.
func show_state(state: Dictionary) -> void:
	_state = state
	if not is_node_ready():
		return
	_statics = Kit.dicts(state.get("statics", []))
	_static_by_id.clear()
	for c: Dictionary in _statics:
		_static_by_id[Kit.get_int(c, "id", -1)] = c
	_live_by_id.clear()
	for c: Dictionary in Kit.dicts(state.get("live", [])):
		_live_by_id[Kit.get_int(c, "id", -1)] = c
	_crew = Kit.as_dict(state.get("crew", {}))
	_controls = Kit.dicts(state.get("controls", []))
	_parts_static = Kit.dicts(state.get("parts_static", []))
	_parts_by_slot.clear()
	for p: Dictionary in Kit.dicts(state.get("parts", [])):
		_parts_by_slot[Kit.get_int(p, "slot", -1)] = p
	var has := not _statics.is_empty()
	_scroll.visible = has
	_summary.visible = has
	_empty.visible = not has
	if not has:
		var session := Kit.get_int(state, "session", Bp.SESSION_IDLE)
		_empty.text = tr(&"hud_damage.waiting" if session == Bp.SESSION_IN_SORTIE or session == Bp.SESSION_LOADING else &"hud_damage.no_hull")
		return
	if not _static_by_id.has(_selected):
		_selected = _default_selection()
	_deck = Kit.get_int(_static_by_id.get(_selected, {}), "deck", _deck)
	_summary.text = summary_text(Kit.as_dict(state.get("summary", {})))
	_show_decks()
	_schematic.show_data(_statics, _live_by_id, Kit.as_dict(state.get("outline", {})), _parts_static, _parts_by_slot, _selected, _deck)
	_show_detail()
	_show_teams()
	_show_order()
	_show_equipment()
	_chain()


## The most urgent compartment: burning beside a magazine, burning, flooding deepest, breached,
## else the first.
func _default_selection() -> int:
	var best := Bp.NONE
	var best_score := -1.0
	for c: Dictionary in _statics:
		var id := Kit.get_int(c, "id", -1)
		var live: Dictionary = _live_by_id.get(id, {})
		var score := 0.0
		if Kit.get_float(live, "fire", 0.0) > 0.0:
			score = 4.0 + (1.0 if Kit.get_bool(c, "has_magazine") else 0.0)
		elif Kit.get_float(live, "fill", 0.0) > 0.0:
			score = 2.0 + Kit.get_float(live, "fill", 0.0)
		elif Kit.get_bool(live, "breached"):
			score = 1.0
		if score > best_score:
			best_score = score
			best = id
	return best


func _on_picked(id: int) -> void:
	if not _static_by_id.has(id):
		return
	_selected = id
	_deck = Kit.get_int(_static_by_id[id], "deck", _deck)
	show_state(_state)


func _on_deck_step(step: int) -> void:
	var i := _decks.find(_deck)
	if i < 0 or _decks.is_empty():
		return
	var next := _decks[clampi(i + step, 0, _decks.size() - 1)]
	if next != _deck:
		_select_deck(next)


func _select_deck(deck: int) -> void:
	# Keep the selection's position along the hull: the nearest compartment on the new deck.
	var x := 0.0
	if _static_by_id.has(_selected):
		var r: Variant = _static_by_id[_selected].get("rect")
		if r is Rect2:
			x = (r as Rect2).get_center().x
	var best := Bp.NONE
	var best_d := INF
	for c: Dictionary in _statics:
		if Kit.get_int(c, "deck", 0) != deck:
			continue
		var r: Variant = c.get("rect")
		var d := absf((r as Rect2).get_center().x - x) if r is Rect2 else 0.0
		if d < best_d:
			best_d = d
			best = Kit.get_int(c, "id", -1)
	_deck = deck
	if best != Bp.NONE:
		_selected = best
	show_state(_state)


# --- decks ---------------------------------------------------------------------------------------

func _show_decks() -> void:
	var decks: Array[int] = []
	for c: Dictionary in _statics:
		var d := Kit.get_int(c, "deck", 0)
		if not decks.has(d):
			decks.append(d)
	decks.sort()
	if decks != _decks:
		_decks = decks
		Kit.clear(_decks_row)
		_deck_buttons.clear()
		var label := Kit.label(tr(&"hud_damage.plan_deck"), &"Dense", true)
		label.autowrap_mode = TextServer.AUTOWRAP_OFF
		label.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		_decks_row.add_child(label)
		for d: int in decks:
			var b := Kit.inline_button("")
			b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
			b.text = tr(&"hud_damage.deck").format([str(d + 1)])
			b.toggle_mode = true
			b.custom_minimum_size = Vector2(0, ROW_H)
			b.tooltip_text = tr(&"hud_damage.deck_tooltip")
			b.pressed.connect(_select_deck.bind(d))
			_decks_row.add_child(b)
			_deck_buttons.append(b)
	_decks_row.visible = decks.size() > 1
	for i in _deck_buttons.size():
		_deck_buttons[i].set_pressed_no_signal(_decks[i] == _deck)


# --- legend --------------------------------------------------------------------------------------

func _build_legend() -> void:
	Kit.clear(_legend)
	var entries: Array = [
		["damage", Bp.DAMAGE_DAMAGED, &"hud_damage.legend.damaged"],
		["damage", Bp.DAMAGE_HEAVY, &"hud_damage.legend.heavy"],
		["damage", Bp.DAMAGE_DESTROYED, &"hud_damage.legend.destroyed"],
		["flood", 0, &"hud_damage.legend.flooding"],
		["icon", &"damage_fire", &"hud_damage.legend.fire"],
		["icon", &"line_dashed", &"hud_damage.legend.breach"],
		["icon", &"line_double", &"hud_damage.legend.magazine"],
		["icon", &"select_bracket", &"hud_damage.legend.selected"],
	]
	for e: Array in entries:
		var row: Node = LegendScene.instantiate()
		_legend.add_child(row)
		match String(e[0]):
			"damage":
				row.call(&"show_damage", int(e[1]), 0.0, e[2])
			"flood":
				row.call(&"show_damage", Bp.DAMAGE_INTACT, 1.0, e[2])
			_:
				row.call(&"show_icon", e[1], e[2], &"text.primary")


# --- the selected compartment --------------------------------------------------------------------

func _build_detail() -> void:
	_d_title = Kit.label("", &"Body", true)
	_detail.add_child(_d_title)
	_d_facts = Kit.label("", &"Dense", true)
	_detail.add_child(_d_facts)
	_d_state = Kit.label("", &"Body", true)
	_detail.add_child(_d_state)
	_d_actions = HFlowContainer.new()
	_d_actions.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	_d_actions.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	_detail.add_child(_d_actions)
	_d_fire = _action_button(_on_fire)
	_d_patch = _action_button(_on_patch)
	_d_valve = _action_button(_on_valve)
	_d_mags = Kit.vbox(0)
	_detail.add_child(_d_mags)
	_d_hint = Kit.label("", &"Dense", true)
	_detail.add_child(_d_hint)
	_d_parts = Kit.vbox(0)
	_detail.add_child(_d_parts)


func _action_button(handler: Callable) -> Button:
	var b := Kit.inline_button("")
	b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	b.custom_minimum_size = Vector2(0, ROW_H)
	b.pressed.connect(handler)
	_d_actions.add_child(b)
	return b


func _show_detail() -> void:
	var c: Dictionary = _static_by_id.get(_selected, {})
	var live: Dictionary = _live_by_id.get(_selected, {})
	var name := _Text.resolve(c.get("name", {}))
	_d_title.text = name
	_d_facts.text = facts_text(c)
	_d_state.text = state_text(live, Kit.get_bool(c, "below_waterline"))
	var fill := Kit.get_float(live, "fill", 0.0)
	var fire := Kit.get_float(live, "fire", 0.0)
	var breached := Kit.get_bool(live, "breached")
	var can_work := Kit.get_int(_crew, "surviving_berths", 1) > 0 and not Kit.dicts(_crew.get("teams", [])).is_empty()
	var team := pick_team(Kit.dicts(_crew.get("teams", [])))
	var team_text := str(team + 1)
	_d_fire.visible = fire > 0.0
	_d_fire.text = tr(&"hud_damage.send.fire").format([team_text])
	_d_fire.disabled = not can_work or _team_on(Bp.TEAM_FIGHT_FIRE, _selected)
	_d_fire.tooltip_text = tr(&"hud_damage.send.fire_tooltip") if can_work else tr(&"hud_damage.send.no_berths")
	_d_patch.visible = breached
	_d_patch.text = tr(&"hud_damage.send.patch").format([team_text])
	_d_patch.disabled = not can_work or fill >= PATCH_LIMIT or _team_on(Bp.TEAM_PATCH, _selected)
	_d_patch.tooltip_text = tr(&"hud_damage.send.no_berths") if not can_work else (tr(&"hud_damage.send.patch_flooded") if fill >= PATCH_LIMIT else tr(&"hud_damage.send.patch_tooltip"))
	var valve := _control_row(&"valve", Bp.NONE, _selected)
	_d_valve.visible = not valve.is_empty()
	if not valve.is_empty():
		var open := Kit.get_bool(valve, "on")
		_d_valve.text = tr(&"hud_damage.valve.close" if open else &"hud_damage.valve.open")
		_d_valve.tooltip_text = tr(&"hud_damage.valve.close_tooltip" if open else &"hud_damage.valve.open_tooltip")
		_d_valve.disabled = not Kit.get_bool(valve, "usable")
	_d_actions.visible = _d_fire.visible or _d_patch.visible or _d_valve.visible
	# Magazines in this compartment.
	var mags: Array[Dictionary] = []
	for row: Dictionary in _controls:
		if StringName(row.get("kind", &"")) == &"magazine_flood" and Kit.get_int(row, "compartment", -1) == _selected:
			mags.append(row)
	_show_magazines(_d_mags, mags, "detail")
	_d_hint.visible = not can_work
	_d_hint.text = tr(&"hud_damage.send.no_berths")
	_show_parts()
	_schematic.accessibility_description = "%s. %s %s" % [name, _d_facts.text, _d_state.text]


func _team_on(kind: int, target: int) -> bool:
	for t: Dictionary in Kit.dicts(_crew.get("teams", [])):
		if Kit.get_int(t, "kind", 0) == kind and Kit.get_int(t, "target", -1) == target:
			return true
	return false


func _control_row(kind: StringName, slot: int, compartment: int) -> Dictionary:
	for row: Dictionary in _controls:
		if StringName(row.get("kind", &"")) != kind:
			continue
		if slot != Bp.NONE and Kit.get_int(row, "slot", -2) != slot:
			continue
		if compartment != Bp.NONE and Kit.get_int(row, "compartment", -2) != compartment:
			continue
		return row
	return {}


func _show_parts() -> void:
	var parts: Array[Dictionary] = []
	for p: Dictionary in _parts_static:
		if Kit.get_int(p, "compartment", -1) == _selected:
			parts.append(p)
	if _d_parts_for != _selected:
		_d_parts_for = _selected
		Kit.clear(_d_parts)
		_part_rows.clear()
		for p: Dictionary in parts:
			var slot := Kit.get_int(p, "slot", -1)
			var row := Kit.hbox(1)
			var swatch: Control = HatchScene.instantiate()
			swatch.set(&"framed", true)
			swatch.size_flags_vertical = Control.SIZE_SHRINK_CENTER
			row.add_child(swatch)
			var label := Kit.label("", &"Dense", true)
			label.size_flags_horizontal = Control.SIZE_EXPAND_FILL
			label.size_flags_vertical = Control.SIZE_SHRINK_CENTER
			row.add_child(label)
			var repair := Kit.inline_button("")
			repair.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
			repair.custom_minimum_size = Vector2(0, 32)
			repair.pressed.connect(_on_repair.bind(slot))
			row.add_child(repair)
			_d_parts.add_child(row)
			_part_rows[slot] = {"swatch": swatch, "label": label, "repair": repair}
	var team := pick_team(Kit.dicts(_crew.get("teams", [])))
	var can_work := Kit.get_int(_crew, "surviving_berths", 1) > 0 and not Kit.dicts(_crew.get("teams", [])).is_empty()
	for p: Dictionary in parts:
		var slot := Kit.get_int(p, "slot", -1)
		if not _part_rows.has(slot):
			continue
		var row: Dictionary = _part_rows[slot]
		var live: Dictionary = _parts_by_slot.get(slot, {})
		var level := Kit.get_int(live, "level", Bp.DAMAGE_INTACT)
		var health := clampf(Kit.get_float(live, "health", 1.0), 0.0, 1.0)
		(row["swatch"] as Control).set(&"level", level)
		(row["label"] as Label).text = part_text(_Text.resolve(p.get("name", {})), level, health)
		var repair: Button = row["repair"]
		repair.visible = level != Bp.DAMAGE_INTACT
		if level == Bp.DAMAGE_DESTROYED:
			repair.text = tr(&"hud_damage.repair.tender")
			repair.disabled = true
			repair.tooltip_text = tr(&"hud_damage.repair.tender_tooltip")
		else:
			repair.text = tr(&"hud_damage.repair.send").format([str(team + 1)])
			repair.disabled = not can_work or health >= REPAIR_CAP or _team_on(Bp.TEAM_REPAIR, slot)
			repair.tooltip_text = tr(&"hud_damage.repair.capped") if health >= REPAIR_CAP else tr(&"hud_damage.repair.tooltip")


func _show_magazines(box: VBoxContainer, mags: Array[Dictionary], scope: String) -> void:
	var key: Array = []
	for m: Dictionary in mags:
		key.append(Kit.get_int(m, "slot", -1))
	var meta := "mag_key_" + scope
	if box.get_meta(meta, []) != key:
		box.set_meta(meta, key)
		Kit.clear(box)
		for slot: int in key:
			var b := Kit.inline_button("")
			b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
			b.custom_minimum_size = Vector2(0, ROW_H)
			b.pressed.connect(_on_flood_magazine.bind(slot))
			b.set_meta(&"slot", slot)
			box.add_child(b)
	box.visible = not mags.is_empty()
	for i in mags.size():
		var b := box.get_child(i) as Button
		if b == null:
			continue
		var m := mags[i]
		var flooded := Kit.get_bool(m, "on")
		var part_name := _part_name(Kit.get_int(m, "slot", -1))
		b.text = tr(&"hud_damage.magazine.flooded").format([part_name]) if flooded else _Text.resolve(m.get("name", {}))
		b.disabled = flooded or not Kit.get_bool(m, "usable")
		b.tooltip_text = tr(&"hud_damage.magazine.tooltip")


# --- teams ---------------------------------------------------------------------------------------

func _show_teams() -> void:
	var teams := Kit.dicts(_crew.get("teams", []))
	if teams.size() != _team_rows.size():
		Kit.clear(_teams_box)
		_team_rows.clear()
		var title := Kit.label(tr(&"hud_damage.teams.title"), &"Body", true)
		_teams_box.add_child(title)
		for i in teams.size():
			var row := Kit.hbox(1)
			var label := Kit.label("", &"Dense", true)
			label.size_flags_horizontal = Control.SIZE_EXPAND_FILL
			label.size_flags_vertical = Control.SIZE_SHRINK_CENTER
			row.add_child(label)
			var back := Kit.inline_button("")
			back.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
			back.custom_minimum_size = Vector2(0, 32)
			back.pressed.connect(_on_hand_back.bind(i))
			row.add_child(back)
			_teams_box.add_child(row)
			_team_rows.append({"label": label, "back": back})
	var title_label := _teams_box.get_child(0) as Label
	if title_label != null:
		title_label.text = teams_title(_crew)
	for i in teams.size():
		var t := teams[i]
		var row := _team_rows[i]
		(row["label"] as Label).text = team_text(t)
		var back: Button = row["back"]
		back.visible = Kit.get_bool(t, "pinned")
		back.text = tr(&"hud_damage.teams.hand_back")
		back.tooltip_text = tr(&"hud_damage.teams.hand_back_tooltip")
		back.accessibility_name = tr(&"hud_damage.teams.hand_back_access").format([str(i + 1)])


# --- repair order --------------------------------------------------------------------------------

func _build_order() -> void:
	var title := Kit.label(tr(&"hud_damage.order.title"), &"Body", true)
	_order_box.add_child(title)
	for i in 6:
		var row := Kit.hbox(1)
		var label := Kit.label("", &"Dense", true)
		label.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		label.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		row.add_child(label)
		var up := Kit.icon_button(&"arrow_up", tr(&"hud_damage.order.up"))
		up.custom_minimum_size = Vector2(32, 32)
		up.pressed.connect(_on_move.bind(i, -1))
		row.add_child(up)
		var down := Kit.icon_button(&"arrow_down", tr(&"hud_damage.order.down"))
		down.custom_minimum_size = Vector2(32, 32)
		down.pressed.connect(_on_move.bind(i, 1))
		row.add_child(down)
		_order_box.add_child(row)
		_order_rows.append({"label": label, "up": up, "down": down})


func _show_order() -> void:
	var order: Array[int] = []
	var raw: Variant = _crew.get("repair_order", PackedInt32Array())
	if raw is PackedInt32Array:
		for v: int in raw as PackedInt32Array:
			order.append(v)
	if not valid_order(order):
		order = [Bp.REPAIR_MAGAZINE_FIRES, Bp.REPAIR_BREACHES_BELOW_WATERLINE, Bp.REPAIR_OTHER_FIRES, Bp.REPAIR_CONDUITS_TO_CRITICAL, Bp.REPAIR_OTHER_CONDUITS, Bp.REPAIR_PART_REPAIRS]
	_order = order
	(_order_box.get_child(0) as Label).text = tr(&"hud_damage.order.title")
	for i in 6:
		var row := _order_rows[i]
		var word := _repair_word(order[i])
		(row["label"] as Label).text = tr(&"hud_damage.order.row").format([str(i + 1), word])
		var up: Button = row["up"]
		var down: Button = row["down"]
		up.disabled = i == 0
		down.disabled = i == 5
		up.accessibility_name = tr(&"hud_damage.order.up_access").format([word])
		down.accessibility_name = tr(&"hud_damage.order.down_access").format([word])


static func _repair_word(v: int) -> String:
	var w: Variant = Kit.call_on(&"Tx", &"word", [&"repair", v])
	if w is String and not (w as String).is_empty():
		return w
	return TranslationServer.translate(StringName("hud_damage.repair_word.%d" % v))


## True when `order` holds each of the six repair priorities once.
static func valid_order(order: Array[int]) -> bool:
	if order.size() != 6:
		return false
	var seen: Array[int] = []
	for v: int in order:
		if v < 0 or v > 5 or seen.has(v):
			return false
		seen.append(v)
	return true


## `order` with the entry at `index` moved by `step` (-1 up, +1 down); unchanged at the ends.
static func moved(order: Array[int], index: int, step: int) -> Array[int]:
	var out: Array[int] = order.duplicate()
	var j := index + step
	if index < 0 or index >= out.size() or j < 0 or j >= out.size():
		return out
	var t: int = out[index]
	out[index] = out[j]
	out[j] = t
	return out


# --- pumps, doors, magazines ---------------------------------------------------------------------

func _show_equipment() -> void:
	var key: Array = []
	for row: Dictionary in _controls:
		var kind := String(row.get("kind", ""))
		if kind == "valve":
			continue
		key.append("%s:%d:%d" % [kind, Kit.get_int(row, "slot", -1), Kit.get_int(row, "compartment", -1)])
	if key != _equip_key:
		_equip_key = key
		_build_equipment()
	for row: Dictionary in _controls:
		var kind := String(row.get("kind", ""))
		var k := "%s:%d:%d" % [kind, Kit.get_int(row, "slot", -1), Kit.get_int(row, "compartment", -1)]
		if not _equip_rows.has(k):
			continue
		var r: Dictionary = _equip_rows[k]
		var on := Kit.get_bool(row, "on")
		var usable := Kit.get_bool(row, "usable")
		var name := _Text.resolve(row.get("name", {}))
		match kind:
			"pump":
				(r["label"] as Label).text = tr(&"hud_damage.pump.state_on" if on else &"hud_damage.pump.state_off").format([name])
				var b: Button = r["button"]
				b.text = tr(&"hud_damage.pump.stop" if on else &"hud_damage.pump.run")
				b.disabled = not usable
				b.tooltip_text = tr(&"hud_damage.pump.tooltip") if usable else tr(&"hud_damage.pump.unusable")
				(r["target"] as OptionButton).disabled = not usable
			"door":
				(r["label"] as Label).text = tr(&"hud_damage.door.closed" if on else &"hud_damage.door.open_state").format([name])
				var b: Button = r["button"]
				b.text = tr(&"hud_damage.door.open" if on else &"hud_damage.door.close")
				b.disabled = not usable
				b.tooltip_text = tr(&"hud_damage.door.tooltip") if usable else tr(&"hud_damage.door.unusable")
	var mags: Array[Dictionary] = []
	for row: Dictionary in _controls:
		if StringName(row.get("kind", &"")) == &"magazine_flood":
			mags.append(row)
	var mag_box: VBoxContainer = _equipment.get_meta(&"mags") if _equipment.has_meta(&"mags") else null
	if mag_box != null:
		_show_magazines(mag_box, mags, "list")


func _build_equipment() -> void:
	Kit.clear(_equipment)
	_equip_rows.clear()
	var any_pump := false
	var any_door := false
	var any_mag := false
	for row: Dictionary in _controls:
		match String(row.get("kind", "")):
			"pump": any_pump = true
			"door": any_door = true
			"magazine_flood": any_mag = true
	if any_pump:
		_equipment.add_child(Kit.label(tr(&"hud_damage.pumps.title"), &"Body", true))
	for row: Dictionary in _controls:
		if String(row.get("kind", "")) != "pump":
			continue
		var slot := Kit.get_int(row, "slot", -1)
		var line := Kit.hbox(1)
		var label := Kit.label("", &"Dense", true)
		label.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		label.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		line.add_child(label)
		var target := OptionButton.new()
		target.focus_mode = Control.FOCUS_ALL
		target.custom_minimum_size = Vector2(0, 32)
		target.tooltip_text = tr(&"hud_damage.pump.target_tooltip")
		target.accessibility_name = tr(&"hud_damage.pump.target_access").format([_Text.resolve(row.get("name", {}))])
		var own := Kit.get_int(row, "compartment", -1)
		target.add_item(tr(&"hud_damage.pump.officer"), 0)
		target.set_item_metadata(0, Bp.NONE)
		var choices: Array[int] = []
		if own >= 0:
			choices.append(own)
			var n: Variant = _static_by_id.get(own, {}).get("neighbours", PackedInt32Array())
			if n is PackedInt32Array:
				for c: int in n as PackedInt32Array:
					if not choices.has(c):
						choices.append(c)
		for c: int in choices:
			var idx := target.item_count
			target.add_item(_compartment_name(c) + (tr(&"hud_damage.pump.own") if c == own else ""), idx)
			target.set_item_metadata(idx, c)
		var chosen: int = _pump_targets.get(slot, Bp.NONE)
		for i in target.item_count:
			if int(target.get_item_metadata(i)) == chosen:
				target.select(i)
		target.item_selected.connect(_on_pump_target.bind(slot, target))
		line.add_child(target)
		var b := Kit.inline_button("")
		b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		b.custom_minimum_size = Vector2(0, 32)
		b.pressed.connect(_on_pump.bind(slot))
		line.add_child(b)
		_equipment.add_child(line)
		_equip_rows["pump:%d:%d" % [slot, own]] = {"label": label, "button": b, "target": target}
	if any_door:
		_equipment.add_child(Kit.label(tr(&"hud_damage.doors.title"), &"Body", true))
	for row: Dictionary in _controls:
		if String(row.get("kind", "")) != "door":
			continue
		var slot := Kit.get_int(row, "slot", -1)
		var line := Kit.hbox(1)
		var label := Kit.label("", &"Dense", true)
		label.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		label.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		line.add_child(label)
		var b := Kit.inline_button("")
		b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		b.custom_minimum_size = Vector2(0, 32)
		b.pressed.connect(_on_door.bind(slot))
		line.add_child(b)
		_equipment.add_child(line)
		_equip_rows["door:%d:%d" % [slot, Kit.get_int(row, "compartment", -1)]] = {"label": label, "button": b}
	if any_mag:
		_equipment.add_child(Kit.label(tr(&"hud_damage.magazines.title"), &"Body", true))
		var box := Kit.vbox(0)
		_equipment.add_child(box)
		_equipment.set_meta(&"mags", box)
	elif _equipment.has_meta(&"mags"):
		_equipment.remove_meta(&"mags")
	_equipment.visible = any_pump or any_door or any_mag


func _part_name(slot: int) -> String:
	for p: Dictionary in _parts_static:
		if Kit.get_int(p, "slot", -2) == slot:
			return _Text.resolve(p.get("name", {}))
	return ""


func _compartment_name(id: int) -> String:
	var c: Dictionary = _static_by_id.get(id, {})
	var t := _Text.resolve(c.get("name", {}))
	return t if not t.is_empty() else tr(&"hud_damage.compartment").format([str(id + 1)])


# --- focus ---------------------------------------------------------------------------------------

## Controls that take focus, top to bottom.
func focus_targets() -> Array:
	var out: Array = []
	for b: Button in _deck_buttons:
		out.append(b)
	out.append(_schematic)
	for b: Button in [_d_fire, _d_patch, _d_valve]:
		out.append(b)
	for c: Node in _d_mags.get_children():
		out.append(c)
	for slot: int in _part_rows:
		out.append(_part_rows[slot]["repair"])
	for r: Dictionary in _team_rows:
		out.append(r["back"])
	for r: Dictionary in _order_rows:
		out.append(r["up"])
		out.append(r["down"])
	for c: Node in _equipment.get_children():
		_collect_focus(c, out)
	return out


func _collect_focus(n: Node, out: Array) -> void:
	if n is Control and (n as Control).focus_mode == Control.FOCUS_ALL:
		out.append(n)
	for c: Node in n.get_children():
		_collect_focus(c, out)


func _chain() -> void:
	Kit.chain_vertical(focus_targets())
	# Order rows read left to right.
	for r: Dictionary in _order_rows:
		Kit.chain_horizontal([r["up"], r["down"]])


# --- orders --------------------------------------------------------------------------------------

func _on_fire() -> void:
	var team := pick_team(Kit.dicts(_crew.get("teams", [])))
	Kit.call_on(&"Orders", &"assign_team", [team, Bp.TEAM_FIGHT_FIRE, _selected])


func _on_patch() -> void:
	var team := pick_team(Kit.dicts(_crew.get("teams", [])))
	Kit.call_on(&"Orders", &"assign_team", [team, Bp.TEAM_PATCH, _selected])


func _on_repair(slot: int) -> void:
	var team := pick_team(Kit.dicts(_crew.get("teams", [])))
	Kit.call_on(&"Orders", &"assign_team", [team, Bp.TEAM_REPAIR, slot])


func _on_hand_back(team: int) -> void:
	Kit.call_on(&"Orders", &"assign_team", [team, Bp.NONE, Bp.NONE])


func _on_valve() -> void:
	var valve := _control_row(&"valve", Bp.NONE, _selected)
	if valve.is_empty():
		return
	Kit.call_on(&"Orders", &"set_flood_valve", [_selected, not Kit.get_bool(valve, "on")])


func _on_move(index: int, step: int) -> void:
	if not valid_order(_order):
		return
	var next := moved(_order, index, step)
	if next == _order:
		return
	var packed := PackedInt32Array(next)
	if Kit.call_on(&"Orders", &"reorder_repairs", [packed]) == true:
		# Show the new order now; the next crew update confirms or corrects it. The crew Dictionary is
		# the facade's cached one, so it is copied before it is changed.
		_crew = _crew.duplicate()
		_crew["repair_order"] = packed
		_show_order()
		# Focus follows the moved entry, on the same arrow while it can still move that way.
		var j := clampi(index + step, 0, 5)
		var row := _order_rows[j]
		var same: Button = row["up"] if step < 0 else row["down"]
		var other: Button = row["down"] if step < 0 else row["up"]
		Ui.focus(other if same.disabled else same)


func _on_pump(slot: int) -> void:
	var row := _control_row(&"pump", slot, Bp.NONE)
	if row.is_empty():
		return
	Kit.call_on(&"Orders", &"set_pump", [slot, not Kit.get_bool(row, "on"), _pump_targets.get(slot, Bp.NONE)])


func _on_pump_target(index: int, slot: int, target: OptionButton) -> void:
	var c := int(target.get_item_metadata(index)) if index >= 0 else Bp.NONE
	_pump_targets[slot] = c
	var row := _control_row(&"pump", slot, Bp.NONE)
	# A new target for a running pump takes effect at once.
	if not row.is_empty() and Kit.get_bool(row, "on"):
		Kit.call_on(&"Orders", &"set_pump", [slot, true, c])


func _on_door(slot: int) -> void:
	var row := _control_row(&"door", slot, Bp.NONE)
	if row.is_empty():
		return
	Kit.call_on(&"Orders", &"set_door", [slot, not Kit.get_bool(row, "on")])


## Flooding a magazine cannot be undone in the sortie (L668): ask first, the safe answer focused.
func _on_flood_magazine(slot: int) -> void:
	var row := _control_row(&"magazine_flood", slot, Bp.NONE)
	var name := _Text.resolve(row.get("name", {}))
	var screens := Kit.autoload(&"Screens")
	if screens == null or not screens.has_method(&"open_dialog"):
		return
	var dialog: Variant = screens.call(&"open_dialog", tr(&"hud_damage.magazine.confirm_title").format([name]), tr(&"hud_damage.magazine.confirm_body"),
		[{"id": &"keep", "label": &"hud_damage.magazine.keep"}, {"id": &"flood", "label": &"hud_damage.magazine.flood"}], &"keep", &"keep")
	if dialog is Node and (dialog as Node).has_signal(&"chosen"):
		(dialog as Node).connect(&"chosen", func(id: StringName) -> void:
			if id == &"flood":
				Kit.call_on(&"Orders", &"flood_magazine", [slot]), CONNECT_ONE_SHOT)


# --- text and rules (static so tests share them) -------------------------------------------------

## The team a new job goes to: the lowest-numbered team standing by, else the lowest-numbered team
## the officer directs (not pinned by the player), else team 0.
static func pick_team(teams: Array[Dictionary]) -> int:
	for t: Dictionary in teams:
		if Kit.get_int(t, "kind", 0) == Bp.TEAM_IDLE and not Kit.get_bool(t, "pinned"):
			return Kit.get_int(t, "index", 0)
	for t: Dictionary in teams:
		if Kit.get_int(t, "kind", 0) == Bp.TEAM_IDLE:
			return Kit.get_int(t, "index", 0)
	for t: Dictionary in teams:
		if not Kit.get_bool(t, "pinned"):
			return Kit.get_int(t, "index", 0)
	return 0


## "2 fires, 3 compartments flooding, 1 breached, 4 parts damaged, 1 team standing by, heel 4 to
## port", or "No damage".
static func summary_text(s: Dictionary) -> String:
	if s.is_empty():
		return ""
	var parts: PackedStringArray = []
	var fires := Kit.get_int(s, "fires", 0)
	if fires > 0:
		parts.append((_t(&"hud_damage.sum.fire_one") if fires == 1 else _t(&"hud_damage.sum.fires")).format([str(fires)]))
	var flooding := Kit.get_int(s, "flooding", 0)
	if flooding > 0:
		parts.append((_t(&"hud_damage.sum.flood_one") if flooding == 1 else _t(&"hud_damage.sum.floods")).format([str(flooding)]))
	var breached := Kit.get_int(s, "breached", 0)
	if breached > 0:
		parts.append(_t(&"hud_damage.sum.breached_one") if breached == 1 else _t(&"hud_damage.sum.breached").format([str(breached)]))
	var damaged := Kit.get_int(s, "parts_damaged", 0) + Kit.get_int(s, "parts_heavy", 0)
	if damaged > 0:
		parts.append((_t(&"hud_damage.sum.part_one") if damaged == 1 else _t(&"hud_damage.sum.parts")).format([str(damaged)]))
	var destroyed := Kit.get_int(s, "parts_destroyed", 0)
	if destroyed > 0:
		parts.append(_t(&"hud_damage.sum.destroyed").format([str(destroyed)]))
	var head := ", ".join(parts) if not parts.is_empty() else _t(&"hud_damage.sum.none")
	var tail: PackedStringArray = []
	var idle := Kit.get_int(s, "teams_idle", 0)
	tail.append(_t(&"hud_damage.sum.idle_one") if idle == 1 else _t(&"hud_damage.sum.idle").format([str(idle)]))
	var pumps := Kit.get_int(s, "pumps_running", 0)
	if pumps > 0:
		tail.append(_t(&"hud_damage.sum.pump_one") if pumps == 1 else _t(&"hud_damage.sum.pumps").format([str(pumps)]))
	var heel := Kit.get_float(s, "heel_deg", 0.0)
	if absf(heel) >= 1.0 and is_finite(heel):
		tail.append(_t(&"hud_damage.sum.heel_starboard" if heel > 0.0 else &"hud_damage.sum.heel_port").format([Kit.number(absf(heel), &"deg", 0)]))
	return _t(&"hud_damage.sum.line").format([head, ", ".join(tail)])


static func _t(key: StringName) -> String:
	return String(TranslationServer.translate(key))


## "Teams: 2 of 2, 8 of 8 berths".
static func teams_title(crew: Dictionary) -> String:
	var teams := Kit.dicts(crew.get("teams", []))
	return TranslationServer.translate(&"hud_damage.teams.title_counts").format([str(teams.size()), str(Kit.get_int(crew, "surviving_berths", 0)), str(Kit.get_int(crew, "original_berths", 0))])


## "Team 1: fighting the fire in compartment 3, 40%, your order".
static func team_text(t: Dictionary) -> String:
	var n := str(Kit.get_int(t, "index", 0) + 1)
	var job := _Text.resolve(t.get("text", {}))
	if job.is_empty():
		job = TranslationServer.translate(&"hud_damage.teams.idle")
	var text := TranslationServer.translate(&"hud_damage.teams.row").format([n, job])
	var p := Kit.get_float(t, "progress", 0.0)
	if Kit.get_int(t, "kind", 0) != Bp.TEAM_IDLE and p > 0.0 and p < 1.0:
		text = TranslationServer.translate(&"hud_damage.teams.progress").format([text, Kit.number(p * 100.0, &"pct", 0)])
	if Kit.get_bool(t, "pinned"):
		text = TranslationServer.translate(&"hud_damage.teams.pinned").format([text])
	return text


## "Deck 2, below the waterline, 42 m3, magazine".
static func facts_text(c: Dictionary) -> String:
	if c.is_empty():
		return ""
	var parts: PackedStringArray = [TranslationServer.translate(&"hud_damage.deck").format([str(Kit.get_int(c, "deck", 0) + 1)])]
	if Kit.get_bool(c, "below_waterline"):
		parts.append(TranslationServer.translate(&"hud_damage.facts.below"))
	parts.append(TranslationServer.translate(&"hud_damage.facts.volume").format([Kit.number(maxf(Kit.get_float(c, "volume_m3", 0.0), 0.0), &"none", 0)]))
	if Kit.get_bool(c, "has_magazine"):
		parts.append(TranslationServer.translate(&"hud_damage.facts.magazine"))
	return ", ".join(parts)


## "Flooded 35%, fire at 80%, breached, flood valve open" or "Dry, no fire".
static func state_text(live: Dictionary, below: bool) -> String:
	var parts: PackedStringArray = []
	var fill := clampf(Kit.get_float(live, "fill", 0.0), 0.0, 1.0)
	var fire := clampf(Kit.get_float(live, "fire", 0.0), 0.0, 1.0)
	if fill > 0.0:
		parts.append(TranslationServer.translate(&"hud_damage.state.flooded").format([Kit.number(fill * 100.0, &"pct", 0)]))
	if fire > 0.0:
		parts.append(TranslationServer.translate(&"hud_damage.state.fire").format([Kit.number(fire * 100.0, &"pct", 0)]))
	if Kit.get_bool(live, "breached"):
		parts.append(TranslationServer.translate(&"hud_damage.state.breached"))
	if below and Kit.get_bool(live, "valve_open"):
		parts.append(TranslationServer.translate(&"hud_damage.state.valve"))
	if not live.is_empty() and not Kit.get_bool(live, "sealed", true):
		parts.append(TranslationServer.translate(&"hud_damage.state.open"))
	if parts.is_empty():
		return TranslationServer.translate(&"hud_damage.state.sound")
	return ", ".join(parts)


## "Gun 1: damaged, 55%".
static func part_text(name: String, level: int, health: float) -> String:
	var w: Variant = Kit.call_on(&"Tx", &"word", [&"damage", level])
	var word: String = w if w is String and not (w as String).is_empty() else TranslationServer.translate(StringName("hud_damage.level.%d" % clampi(level, 0, 3)))
	if level == Bp.DAMAGE_INTACT or level == Bp.DAMAGE_DESTROYED:
		return TranslationServer.translate(&"hud_damage.part.row").format([name, word])
	return TranslationServer.translate(&"hud_damage.part.row_health").format([name, word, Kit.number(health * 100.0, &"pct", 0)])


# --- the schematic -------------------------------------------------------------------------------

## Profile over plan, drawn from the Plot and Damage theme types. Emits `picked(id)` on a click or a
## Left/Right step, `deck_step(step)` on Up/Down.
class Schematic extends Control:
	signal picked(id: int)
	signal deck_step(step: int)

	const PAD := 8.0
	const GAP := 28.0
	const MAX_HEIGHT := 300.0
	const PART_MIN_PX := 3.0
	const CELL := 0.5
	const BRACKET := 8.0

	var _statics: Array[Dictionary] = []
	var _live: Dictionary[int, Dictionary] = {}
	var _outline: Dictionary = {}
	var _parts_static: Array[Dictionary] = []
	var _parts: Dictionary[int, Dictionary] = {}
	var _selected := -1
	var _deck := 0
	var _scale := 1.0
	var _origin_profile := Vector2.ZERO
	var _origin_plan := Vector2.ZERO
	var _length := 1.0
	var _beam := 1.0
	var _height := 1.0
	var _order: Array[int] = []

	func _init() -> void:
		focus_mode = Control.FOCUS_ALL
		mouse_filter = Control.MOUSE_FILTER_STOP
		size_flags_horizontal = Control.SIZE_EXPAND_FILL
		# Masts and superstructure above the compartments are cropped off the profile.
		clip_contents = true
		accessibility_name = TranslationServer.translate(&"hud_damage.schematic.access")
		resized.connect(_relayout)

	func _notification(what: int) -> void:
		if what == NOTIFICATION_THEME_CHANGED or what == NOTIFICATION_FOCUS_ENTER or what == NOTIFICATION_FOCUS_EXIT:
			queue_redraw()

	func show_data(statics: Array[Dictionary], live: Dictionary[int, Dictionary], outline: Dictionary, parts_static: Array[Dictionary], parts: Dictionary[int, Dictionary], selected: int, deck: int) -> void:
		_statics = statics
		_live = live
		_outline = outline
		_parts_static = parts_static
		_parts = parts
		_selected = selected
		_deck = deck
		_order.clear()
		var sorted := statics.duplicate()
		sorted.sort_custom(func(a: Dictionary, b: Dictionary) -> bool:
			var da := int(a.get("deck", 0))
			var db := int(b.get("deck", 0))
			if da != db:
				return da < db
			var ra: Variant = a.get("rect")
			var rb: Variant = b.get("rect")
			var xa := (ra as Rect2).position.x if ra is Rect2 else 0.0
			var xb := (rb as Rect2).position.x if rb is Rect2 else 0.0
			if not is_equal_approx(xa, xb):
				return xa < xb
			var ya := (ra as Rect2).position.y if ra is Rect2 else 0.0
			var yb := (rb as Rect2).position.y if rb is Rect2 else 0.0
			return ya < yb)
		for c: Dictionary in sorted:
			_order.append(int(c.get("id", -1)))
		_measure()
		_relayout()
		queue_redraw()

	func _measure() -> void:
		var l := float(_outline.get("length_m", 0.0))
		var b := float(_outline.get("beam_m", 0.0))
		var h := 0.0
		var outline_h := 0.0
		for c: Dictionary in _statics:
			var r: Variant = c.get("rect")
			var s: Variant = c.get("side_rect")
			if r is Rect2:
				l = maxf(l, (r as Rect2).end.x)
				b = maxf(b, (r as Rect2).end.y)
			if s is Rect2:
				h = maxf(h, (s as Rect2).end.y)
		var prof: Variant = _outline.get("profile", PackedVector2Array())
		if prof is PackedVector2Array:
			for p: Vector2 in prof as PackedVector2Array:
				outline_h = maxf(outline_h, p.y)
				l = maxf(l, p.x)
		var plan: Variant = _outline.get("plan", PackedVector2Array())
		if plan is PackedVector2Array:
			for p: Vector2 in plan as PackedVector2Array:
				b = maxf(b, p.y)
		# The profile shows the compartments and a deck's height above them, not the masts.
		if h <= 0.0:
			h = outline_h
		else:
			h = minf(outline_h, h + 1.5) if outline_h > 0.0 else h
		_length = maxf(l, 1.0)
		_beam = maxf(b, 0.5)
		_height = maxf(h, 0.5)

	func _relayout() -> void:
		var w := maxf(size.x - PAD * 2.0, 10.0)
		_scale = w / _length
		# Cap the height; a long, deep hull then narrows and centres.
		var needed := (_height + _beam) * _scale + GAP + PAD * 2.0
		if needed > MAX_HEIGHT:
			_scale = (MAX_HEIGHT - GAP - PAD * 2.0) / (_height + _beam)
		var x0 := (size.x - _length * _scale) * 0.5
		_origin_profile = Vector2(x0, PAD)
		_origin_plan = Vector2(x0, PAD + _height * _scale + GAP)
		var want := ceilf((_height + _beam) * _scale + GAP + PAD * 2.0)
		if not is_equal_approx(custom_minimum_size.y, want):
			custom_minimum_size = Vector2(0.0, want)
		queue_redraw()

	func _profile_rect(r: Rect2) -> Rect2:
		# Profile y is height above the grid bottom: flip it so up is up.
		return Rect2(_origin_profile.x + r.position.x * _scale, _origin_profile.y + (_height - r.end.y) * _scale, r.size.x * _scale, r.size.y * _scale)

	func _plan_rect(r: Rect2) -> Rect2:
		return Rect2(_origin_plan + r.position * _scale, r.size * _scale)

	func _profile_point(p: Vector2) -> Vector2:
		return Vector2(_origin_profile.x + p.x * _scale, _origin_profile.y + (_height - p.y) * _scale)

	func _plan_point(p: Vector2) -> Vector2:
		return _origin_plan + p * _scale

	func _get_minimum_size() -> Vector2:
		return Vector2(200.0, custom_minimum_size.y)

	func _draw() -> void:
		var inset := get_theme_color(&"surface_inset", &"Plot")
		var raised := get_theme_color(&"surface_raised", &"Plot")
		var divider := get_theme_color(&"rule_divider", &"Plot")
		var reference := get_theme_color(&"rule_reference", &"Plot")
		var primary := get_theme_color(&"text_primary", &"Plot")
		var select := get_theme_color(&"select", &"Plot")
		var font := get_theme_font(&"label", &"Plot")
		var fs := get_theme_font_size(&"label", &"Plot")
		var stroke := float(get_theme_constant(&"stroke_frame", &"Plot"))
		draw_rect(Rect2(Vector2.ZERO, size), inset, true)
		if _statics.is_empty():
			return
		# Hull outlines.
		var prof: Variant = _outline.get("profile", PackedVector2Array())
		if prof is PackedVector2Array and (prof as PackedVector2Array).size() >= 3:
			var pts := PackedVector2Array()
			for p: Vector2 in prof as PackedVector2Array:
				pts.append(_profile_point(p))
			pts.append(pts[0])
			draw_polyline(pts, reference, stroke)
		var plan: Variant = _outline.get("plan", PackedVector2Array())
		if plan is PackedVector2Array and (plan as PackedVector2Array).size() >= 3:
			var pts := PackedVector2Array()
			for p: Vector2 in plan as PackedVector2Array:
				pts.append(_plan_point(p))
			pts.append(pts[0])
			draw_polyline(pts, reference, stroke)
		# Compartments: profile shows every deck, plan the chosen one. Larger ones first, so a small
		# compartment inside a large one's outline stays visible; labels never overlap.
		var selected_rects: Array[Rect2] = []
		var labels: Array[Rect2] = []
		var to_label: Array = []
		var fires: Array[Rect2] = []
		for c: Dictionary in _by_area():
			var id := int(c.get("id", -1))
			var live: Dictionary = _live.get(id, {})
			var s: Variant = c.get("side_rect")
			if s is Rect2:
				var r := _profile_rect(s as Rect2)
				_compartment(r, c, live, true, raised, divider, primary)
				if float(live.get("fire", 0.0)) > 0.0:
					fires.append(r)
				if id == _selected:
					selected_rects.append(r)
			if int(c.get("deck", 0)) == _deck:
				var pr: Variant = c.get("rect")
				if pr is Rect2:
					var r := _plan_rect(pr as Rect2)
					_compartment(r, c, live, false, raised, divider, primary)
					if float(live.get("fire", 0.0)) > 0.0:
						fires.append(r)
					to_label.append([r, id])
					if id == _selected:
						selected_rects.append(r)
		# Damaged parts on the chosen deck, in plan.
		_draw_parts(divider)
		# Fire glyphs and compartment numbers go on top of every fill, so a smaller compartment drawn
		# later never covers them. Numbers sit in each compartment's top-left corner, the selected
		# one first, and a number that would overlap one already placed is left out.
		var icon := Ui.icon(&"damage_fire")
		for r: Rect2 in fires:
			var px := minf(24.0, minf(r.size.x, r.size.y) - 2.0)
			if icon != null and px >= 10.0:
				var at := r.get_center() - Vector2(px, px) * 0.5
				draw_rect(Rect2(at, Vector2(px, px)).grow(1.0), inset, true)
				draw_texture_rect(icon, Rect2(at, Vector2(px, px)), false, primary)
		to_label.sort_custom(func(a: Array, b: Array) -> bool: return int(a[1]) == _selected and int(b[1]) != _selected)
		for entry: Array in to_label:
			var r: Rect2 = entry[0]
			if font == null or r.size.x < 18.0 or r.size.y < 18.0:
				continue
			var t := str(int(entry[1]) + 1)
			var tw := font.get_string_size(t, HORIZONTAL_ALIGNMENT_LEFT, -1, fs).x
			var at := Vector2(r.position.x + 4.0, r.position.y + font.get_ascent(fs) + 2.0)
			var box := Rect2(at.x - 2.0, at.y - font.get_ascent(fs), tw + 4.0, font.get_height(fs))
			var free := true
			for other: Rect2 in labels:
				if other.intersects(box):
					free = false
			for f: Rect2 in fires:
				if f.get_center().distance_to(box.get_center()) < 14.0:
					free = false
			if free:
				labels.append(box)
				draw_string_outline(font, at, t, HORIZONTAL_ALIGNMENT_LEFT, -1, fs, 4, inset)
				draw_string(font, at, t, HORIZONTAL_ALIGNMENT_LEFT, -1, fs, primary)
		# Waterline in profile.
		var wl := float(_outline.get("waterline_m", NAN))
		if is_finite(wl):
			var y := _origin_profile.y + (_height - wl) * _scale
			var x0 := _origin_profile.x - 4.0
			var x1 := _origin_profile.x + _length * _scale + 4.0
			var x := x0
			while x < x1:
				draw_line(Vector2(x, y), Vector2(minf(x + 8.0, x1), y), reference, 1.0)
				x += 14.0
			if font != null:
				var t := TranslationServer.translate(&"hud_damage.schematic.waterline")
				var tw := font.get_string_size(t, HORIZONTAL_ALIGNMENT_LEFT, -1, fs).x
				var at := Vector2(minf(x1 - tw, size.x - tw - 2.0), y - 4.0)
				draw_string_outline(font, at, t, HORIZONTAL_ALIGNMENT_LEFT, -1, fs, 4, inset)
				draw_string(font, at, t, HORIZONTAL_ALIGNMENT_LEFT, -1, fs, reference)
		# View names, bow to the right.
		if font != null:
			var plan_name := TranslationServer.translate(&"hud_damage.schematic.plan").format([str(_deck + 1)])
			draw_string(font, Vector2(PAD, _origin_plan.y - 6.0), plan_name, HORIZONTAL_ALIGNMENT_LEFT, -1, fs, reference)
			var bow := TranslationServer.translate(&"hud_damage.schematic.bow")
			var bw := font.get_string_size(bow, HORIZONTAL_ALIGNMENT_LEFT, -1, fs).x
			draw_string(font, Vector2(size.x - bw - PAD, _origin_plan.y - 6.0), bow, HORIZONTAL_ALIGNMENT_LEFT, -1, fs, reference)
		# Selection bracket (rule 2), then the focus frame.
		for r: Rect2 in selected_rects:
			_bracket(r.grow(3.0), select, float(get_theme_constant(&"stroke_selection", &"Plot")))
		if has_focus():
			draw_rect(Rect2(Vector2.ONE, size - Vector2.ONE * 2.0), get_theme_color(&"ui_focus", &"Plot"), false, float(get_theme_constant(&"stroke_focus", &"Plot")))

	func _by_area() -> Array[Dictionary]:
		var out := _statics.duplicate()
		out.sort_custom(func(a: Dictionary, b: Dictionary) -> bool:
			var ra: Variant = a.get("rect")
			var rb: Variant = b.get("rect")
			var aa := (ra as Rect2).get_area() if ra is Rect2 else 0.0
			var ab := (rb as Rect2).get_area() if rb is Rect2 else 0.0
			return aa > ab)
		return out

	func _compartment(r: Rect2, c: Dictionary, live: Dictionary, profile: bool, raised: Color, divider: Color, primary: Color) -> void:
		if r.size.x < 1.0 or r.size.y < 1.0:
			return
		draw_rect(r, raised, true)
		var fill := clampf(float(live.get("fill", 0.0)), 0.0, 1.0)
		var hatch_w := float(get_theme_constant(&"hatch_width", &"Damage"))
		if fill > 0.0:
			var spacing := float(get_theme_constant(&"hatch_flooding_spacing", &"Damage"))
			var wet := r
			if profile:
				var h := r.size.y * fill
				wet = Rect2(r.position.x, r.end.y - h, r.size.x, h)
			var lines := DamageHatch.hatch_lines(wet, float(get_theme_constant(&"hatch_flooding_angle", &"Damage")), spacing)
			if lines.size() >= 2:
				draw_multiline(lines, get_theme_color(&"hatch_flooding", &"Damage"), hatch_w)
		if bool(live.get("breached", false)):
			_dashed_rect(r.grow(-1.0), primary, 2.0)
		else:
			draw_rect(r, divider, false, 1.0)
		if bool(c.get("has_magazine", false)):
			draw_rect(r.grow(-4.0), primary, false, 1.0)
			draw_rect(r.grow(-1.5), primary, false, 1.0)

	func _draw_parts(divider: Color) -> void:
		var hatch_w := float(get_theme_constant(&"hatch_width", &"Damage"))
		var deck_of: Dictionary[int, int] = {}
		for c: Dictionary in _statics:
			deck_of[int(c.get("id", -1))] = int(c.get("deck", 0))
		for p: Dictionary in _parts_static:
			var comp := int(p.get("compartment", -1))
			if deck_of.get(comp, -99) != _deck:
				continue
			var cell: Variant = p.get("cell")
			var sz: Variant = p.get("size")
			if not (cell is Vector3i and sz is Vector3i):
				continue
			var cv := cell as Vector3i
			var sv := sz as Vector3i
			var r := _plan_rect(Rect2(Vector2(cv.x, cv.z) * CELL, Vector2(maxi(sv.x, 1), maxi(sv.z, 1)) * CELL))
			if r.size.x < PART_MIN_PX or r.size.y < PART_MIN_PX:
				continue
			var level := int(_parts.get(int(p.get("slot", -1)), {}).get("level", 0))
			if level == 0:
				continue
			var name: String = ["intact", "damaged", "heavy", "destroyed"][clampi(level, 0, 3)]
			if level == 3:
				draw_rect(r, get_theme_color(&"hatch_destroyed_fill", &"Damage"), true)
			var colour := get_theme_color(StringName("hatch_%s" % name), &"Damage")
			var lines := DamageHatch.hatch_lines(r, float(get_theme_constant(StringName("hatch_%s_angle" % name), &"Damage")), float(get_theme_constant(StringName("hatch_%s_spacing" % name), &"Damage")))
			if lines.size() >= 2:
				draw_multiline(lines, colour, hatch_w)
			if level == 3:
				var lines_2 := DamageHatch.hatch_lines(r, float(get_theme_constant(&"hatch_destroyed_angle_2", &"Damage")), float(get_theme_constant(&"hatch_destroyed_spacing", &"Damage")))
				if lines_2.size() >= 2:
					draw_multiline(lines_2, colour, hatch_w)
			draw_rect(r, colour, false, 1.0)

	func _dashed_rect(r: Rect2, colour: Color, width: float) -> void:
		var corners := [r.position, Vector2(r.end.x, r.position.y), r.end, Vector2(r.position.x, r.end.y)]
		var segs := PackedVector2Array()
		for i in 4:
			var a: Vector2 = corners[i]
			var b: Vector2 = corners[(i + 1) % 4]
			var length := a.distance_to(b)
			var d := (b - a) / maxf(length, 0.001)
			var t := 0.0
			while t < length:
				segs.append(a + d * t)
				segs.append(a + d * minf(t + 5.0, length))
				t += 9.0
		if segs.size() >= 2:
			draw_multiline(segs, colour, width)

	func _bracket(r: Rect2, colour: Color, width: float) -> void:
		var k := minf(BRACKET, minf(r.size.x, r.size.y) * 0.5)
		var segs := PackedVector2Array([
			r.position, r.position + Vector2(k, 0), r.position, r.position + Vector2(0, k),
			Vector2(r.end.x, r.position.y), Vector2(r.end.x - k, r.position.y), Vector2(r.end.x, r.position.y), Vector2(r.end.x, r.position.y + k),
			r.end, r.end - Vector2(k, 0), r.end, r.end - Vector2(0, k),
			Vector2(r.position.x, r.end.y), Vector2(r.position.x + k, r.end.y), Vector2(r.position.x, r.end.y), Vector2(r.position.x, r.end.y - k),
		])
		draw_multiline(segs, colour, width)

	## The compartment under a point: in plan on the chosen deck, else in profile (the smallest).
	func pick_at(at: Vector2) -> int:
		var best := -1
		var best_area := INF
		for c: Dictionary in _statics:
			var id := int(c.get("id", -1))
			var hit := false
			var area := INF
			if int(c.get("deck", 0)) == _deck:
				var pr: Variant = c.get("rect")
				if pr is Rect2 and _plan_rect(pr as Rect2).has_point(at):
					hit = true
					area = (pr as Rect2).get_area()
			var s: Variant = c.get("side_rect")
			if not hit and s is Rect2 and _profile_rect(s as Rect2).has_point(at):
				hit = true
				area = (s as Rect2).get_area()
			if hit and area < best_area:
				best_area = area
				best = id
		return best

	func _gui_input(event: InputEvent) -> void:
		if event is InputEventMouseButton:
			var mb := event as InputEventMouseButton
			# Fire on release (floor 5).
			if mb.button_index == MOUSE_BUTTON_LEFT and not mb.pressed:
				var id := pick_at(mb.position)
				if id >= 0:
					picked.emit(id)
					accept_event()
			return
		# At the ends the keys fall through to focus navigation, so focus can leave the schematic.
		if event.is_action_pressed(&"ui_right") and _step(1):
			accept_event()
		elif event.is_action_pressed(&"ui_left") and _step(-1):
			accept_event()
		elif event.is_action_pressed(&"ui_up") and _deck_exists(1):
			deck_step.emit(1)
			accept_event()
		elif event.is_action_pressed(&"ui_down") and _deck_exists(-1):
			deck_step.emit(-1)
			accept_event()

	func _step(step: int) -> bool:
		if _order.is_empty():
			return false
		var i := _order.find(_selected)
		var j := clampi(i + step, 0, _order.size() - 1) if i >= 0 else 0
		if j == i:
			return false
		picked.emit(_order[j])
		return true

	func _deck_exists(step: int) -> bool:
		for c: Dictionary in _statics:
			var d := int(c.get("deck", 0))
			if (step > 0 and d > _deck) or (step < 0 and d < _deck):
				return true
		return false

	func _get_tooltip(at: Vector2) -> String:
		var id := pick_at(at)
		if id < 0:
			return ""
		for c: Dictionary in _statics:
			if int(c.get("id", -1)) == id:
				var name := ""
				var n: Variant = c.get("name", {})
				if n is Dictionary:
					var tx: Node = (Engine.get_main_loop() as SceneTree).root.get_node_or_null(^"Tx")
					name = String(tx.call(&"msg", n)) if tx != null and not (n as Dictionary).is_empty() else ""
				return "%s: %s" % [name, DcText.state(_live.get(id, {}), bool(c.get("below_waterline", false)))]
		return ""


## The state sentence for the schematic's inner class, which cannot reach the outer statics.
class DcText extends RefCounted:
	static func state(live: Dictionary, below: bool) -> String:
		var script: GDScript = load("res://ui/hud/damage_control.gd")
		return String(script.call(&"state_text", live, below))
