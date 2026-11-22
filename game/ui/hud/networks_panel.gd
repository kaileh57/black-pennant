extends VBoxContainer
## Power, fuel and data networks: conduits, cuts, buffers, shed consumers.
## DESIGN.md L281-299, L392-396. Unit: ui-hud-damage.
## Reads and calls: Hud.networks, Hud.parts, Hud.parts_static, Orders.set_power_priority.
##
## The networks station, opened from the HUD shell's station buttons. One network at a time, chosen
## with the Power, Fuel and Data buttons (each carries its line pattern, so the buttons are the
## legend's first half). Top to bottom:
## - Summary: conduits, how many are cut, jury-rigged at 50% and doubled; consumers on their local
##   buffer or local control, and without supply.
## - Diagram: the hull deck by deck in plan, top deck first, with the compartments and the chosen
##   network's conduits between compartment centres. Networks differ by line pattern and label,
##   never by hue (the overlay hues belong to the dry dock; HUD rule 2): power solid, fuel dashed,
##   data dotted; a doubled route is a double line; a cut conduit is broken with a cross at its
##   middle; a jury-rigged one is thin with a link glyph (50% until the tender, L396). A legend
##   names every mark.
## - Cut conduits, each with a button that sends a team to jury-rig it (20 s, L396), and the
##   jury-rigged ones.
## - Consumers whose link on this network is not normal, in fallback order (L394): redundant path,
##   local buffer (battery 30 s, day tank 60 s), local control (own sensor, three times the error),
##   none; shed consumers on power.
## - Power only: every consumer's priority 1 (shed last) to 5 with Lower and Raise (L287-295).
## Data arrives at 2 Hz; rows update in place and are rebuilt only when what they list changes.
## States: outside a sortie, waiting for the first picture, no hull, populated.
## `show_state(state)` renders plain data (tests and captures); `refresh()` pulls it.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const _Damage := preload("res://ui/hud/damage_control.gd")
const LegendScene := preload("res://ui/widgets/legend_row.tscn")

const KINDS: Array[int] = [Bp.NETWORK_POWER, Bp.NETWORK_FUEL, Bp.NETWORK_DATA]
const PATTERN_ICONS: Array[StringName] = [&"line_solid", &"line_dashed", &"line_dotted"]
const LINK_KEYS: Array[String] = ["power", "fuel", "data"]
const ROW_H := 32.0

## The network shown, kept for the session.
static var shown_kind := Bp.NETWORK_POWER

@onready var _tabs: HBoxContainer = $Tabs
@onready var _tab_buttons: Array[Button] = [$Tabs/Power, $Tabs/Fuel, $Tabs/Data]
@onready var _summary: Label = $Summary
@onready var _scroll: ScrollContainer = $Scroll
@onready var _column: VBoxContainer = $Scroll/Column
@onready var _legend: HFlowContainer = $Scroll/Column/Legend
@onready var _conduits_box: VBoxContainer = $Scroll/Column/Conduits
@onready var _conduits_rule: HSeparator = $Scroll/Column/ConduitsRule
@onready var _consumers_box: VBoxContainer = $Scroll/Column/Consumers
@onready var _consumers_rule: HSeparator = $Scroll/Column/ConsumersRule
@onready var _priorities_box: VBoxContainer = $Scroll/Column/Priorities
@onready var _priority_rule: HSeparator = $Scroll/Column/PriorityRule
@onready var _empty: Label = $Empty

var _diagram: NetDiagram
var _state: Dictionary = {}
var _networks: Dictionary[int, Dictionary] = {}
var _statics: Array[Dictionary] = []
var _static_by_id: Dictionary[int, Dictionary] = {}
var _parts_static: Array[Dictionary] = []
var _part_static_by_slot: Dictionary[int, Dictionary] = {}
var _parts: Dictionary[int, Dictionary] = {}
var _crew: Dictionary = {}
## Part id to its demand in kW (from the catalogue), for the priority list.
var _demand: Dictionary[StringName, float] = {}
var _conduit_key: Array = []
var _conduit_rows: Dictionary[int, Dictionary] = {}
var _consumer_key: Array = []
var _consumer_labels: Array[Label] = []
var _priority_key: Array = []
var _priority_rows: Dictionary[int, Dictionary] = {}


func _ready() -> void:
	Ui.set_separation(self, 1)
	Ui.set_separation(_tabs, 0)
	Ui.set_separation(_column, 1)
	Ui.set_separation(_conduits_box, 0)
	Ui.set_separation(_consumers_box, 0)
	Ui.set_separation(_priorities_box, 0)
	_legend.add_theme_constant_override(&"h_separation", Ui.spacing(2))
	_summary.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_empty.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	var group := ButtonGroup.new()
	for i in _tab_buttons.size():
		var b := _tab_buttons[i]
		b.button_group = group
		b.focus_mode = Control.FOCUS_ALL
		b.icon = Ui.icon(PATTERN_ICONS[i])
		b.tooltip_text = tr(StringName("hud_damage.net.%s_tooltip" % LINK_KEYS[i]))
		b.pressed.connect(_on_tab.bind(KINDS[i]))
	Kit.chain_horizontal(_tab_buttons)
	_diagram = NetDiagram.new()
	_diagram.name = "Diagram"
	_column.add_child(_diagram)
	_column.move_child(_diagram, 0)
	_build_legend()
	accessibility_name = tr(&"hud_damage.net.title")
	_on(&"Hud", &"networks_changed", refresh)
	_on(&"Hud", &"parts_changed", refresh)
	_on(&"Hud", &"crew_changed", refresh)
	_on(&"Hud", &"ship_static_changed", _on_static)
	_on(&"Session", &"state_changed", func(_s: int) -> void: _on_static())
	refresh()


func _notification(what: int) -> void:
	match what:
		NOTIFICATION_TRANSLATION_CHANGED:
			if is_node_ready():
				_build_legend()
				_conduit_key = []
				_consumer_key = []
				_priority_key = []
				show_state(_state)
		NOTIFICATION_VISIBILITY_CHANGED:
			if is_node_ready() and is_visible_in_tree():
				refresh()


func _on(autoload_name: StringName, signal_name: StringName, handler: Callable) -> void:
	var node := Kit.autoload(autoload_name)
	if node != null and node.has_signal(signal_name) and not node.is_connected(signal_name, handler):
		node.connect(signal_name, handler)


func _on_static() -> void:
	_demand.clear()
	refresh()


func _on_tab(kind: int) -> void:
	shown_kind = kind
	_conduit_key = []
	_consumer_key = []
	show_state(_state)


# --- data ----------------------------------------------------------------------------------------

func refresh() -> void:
	if not is_node_ready():
		return
	if not is_visible_in_tree() and not _state.is_empty():
		return
	var session_state: Variant = Kit.call_on(&"Session", &"state")
	var parts_static := Kit.dicts(Kit.call_on(&"Hud", &"parts_static"))
	if _demand.is_empty():
		for p: Dictionary in parts_static:
			var id := StringName(p.get("part", &""))
			if id == &"" or _demand.has(id):
				continue
			var def := Kit.as_dict(Kit.call_on(&"Content", &"part", [id]))
			_demand[id] = Kit.get_float(def, "power_kw", 0.0)
	show_state({
		"session": int(session_state) if session_state is int else Bp.SESSION_IDLE,
		"networks": Kit.dicts(Kit.call_on(&"Hud", &"networks")),
		"statics": Kit.dicts(Kit.call_on(&"Hud", &"compartments_static")),
		"outline": Kit.as_dict(Kit.call_on(&"Hud", &"hull_outline")),
		"parts_static": parts_static,
		"parts": Kit.dicts(Kit.call_on(&"Hud", &"parts")),
		"crew": Kit.as_dict(Kit.call_on(&"Hud", &"crew")),
		"demand": _demand,
	})


## Render from plain data: `{ session: int, networks: Array[NetworkDict], statics:
## Array[CompartmentStaticDict], outline: hull_outline(), parts_static: Array[PartStaticDict],
## parts: Array[PartDict], crew: CrewDict, demand: Dictionary (part id to kW) }`. Also the test and
## capture entry point.
func show_state(state: Dictionary) -> void:
	_state = state
	if not is_node_ready():
		return
	_networks.clear()
	for n: Dictionary in Kit.dicts(state.get("networks", [])):
		_networks[Kit.get_int(n, "kind", -1)] = n
	_statics = Kit.dicts(state.get("statics", []))
	_static_by_id.clear()
	for c: Dictionary in _statics:
		_static_by_id[Kit.get_int(c, "id", -1)] = c
	_parts_static = Kit.dicts(state.get("parts_static", []))
	_part_static_by_slot.clear()
	for p: Dictionary in _parts_static:
		_part_static_by_slot[Kit.get_int(p, "slot", -1)] = p
	_parts.clear()
	for p: Dictionary in Kit.dicts(state.get("parts", [])):
		_parts[Kit.get_int(p, "slot", -1)] = p
	_crew = Kit.as_dict(state.get("crew", {}))
	var demand: Variant = state.get("demand", {})
	if demand is Dictionary and demand != _demand:
		_demand.clear()
		for k: Variant in (demand as Dictionary):
			_demand[StringName(k)] = float((demand as Dictionary)[k])
	var has := not _networks.is_empty() and not _statics.is_empty()
	_tabs.visible = has
	_summary.visible = has
	_scroll.visible = has
	_empty.visible = not has
	if not has:
		var session := Kit.get_int(state, "session", Bp.SESSION_IDLE)
		_empty.text = tr(&"hud_damage.waiting" if session == Bp.SESSION_IN_SORTIE or session == Bp.SESSION_LOADING else &"hud_damage.no_hull")
		return
	for i in KINDS.size():
		_tab_buttons[i].set_pressed_no_signal(KINDS[i] == shown_kind)
		_tab_buttons[i].text = tab_text(KINDS[i], _networks.get(KINDS[i], {}))
	var net: Dictionary = _networks.get(shown_kind, {})
	var affected := affected_consumers(shown_kind)
	_summary.text = summary_text(net, affected.size(), _shed_count())
	_diagram.show_data(shown_kind, net, _statics, Kit.as_dict(state.get("outline", {})))
	_show_conduits(net)
	_show_consumers(affected)
	_show_priorities()
	_chain()


func _shed_count() -> int:
	var n := 0
	for p: Dictionary in _parts.values():
		if Kit.get_bool(p, "shed"):
			n += 1
	return n


# --- legend --------------------------------------------------------------------------------------

func _build_legend() -> void:
	Kit.clear(_legend)
	var entries: Array = [
		[&"line_solid", &"hud_damage.net.legend.power"],
		[&"line_dashed", &"hud_damage.net.legend.fuel"],
		[&"line_dotted", &"hud_damage.net.legend.data"],
		[&"line_double", &"hud_damage.net.legend.doubled"],
		[&"close", &"hud_damage.net.legend.cut"],
		[&"link", &"hud_damage.net.legend.jury_rigged"],
	]
	for e: Array in entries:
		var row: Node = LegendScene.instantiate()
		_legend.add_child(row)
		row.call(&"show_icon", e[0], e[1], &"text.primary")


# --- conduits ------------------------------------------------------------------------------------

func _show_conduits(net: Dictionary) -> void:
	var rows: Array[Dictionary] = []
	for c: Dictionary in Kit.dicts(net.get("conduits", [])):
		if Kit.get_bool(c, "cut") or Kit.get_bool(c, "jury_rigged"):
			rows.append(c)
	var key: Array = []
	for c: Dictionary in rows:
		key.append([Kit.get_int(c, "id", -1), Kit.get_bool(c, "cut")])
	if key != _conduit_key:
		_conduit_key = key
		Kit.clear(_conduits_box)
		_conduit_rows.clear()
		if not rows.is_empty():
			_conduits_box.add_child(Kit.label(tr(&"hud_damage.net.conduits_title"), &"Body", true))
		for c: Dictionary in rows:
			var id := Kit.get_int(c, "id", -1)
			var line := Kit.hbox(1)
			var label := Kit.label("", &"Dense", true)
			label.size_flags_horizontal = Control.SIZE_EXPAND_FILL
			label.size_flags_vertical = Control.SIZE_SHRINK_CENTER
			line.add_child(label)
			var send: Button = null
			if Kit.get_bool(c, "cut"):
				send = Kit.inline_button("")
				send.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
				send.custom_minimum_size = Vector2(0, ROW_H)
				send.pressed.connect(_on_jury_rig.bind(id))
				line.add_child(send)
			_conduits_box.add_child(line)
			_conduit_rows[id] = {"label": label, "send": send}
	_conduits_box.visible = not rows.is_empty()
	_conduits_rule.visible = not rows.is_empty()
	var teams := Kit.dicts(_crew.get("teams", []))
	var team := _Damage.pick_team(teams)
	var can_work := Kit.get_int(_crew, "surviving_berths", 1) > 0 and not teams.is_empty()
	for c: Dictionary in rows:
		var id := Kit.get_int(c, "id", -1)
		if not _conduit_rows.has(id):
			continue
		var r: Dictionary = _conduit_rows[id]
		(r["label"] as Label).text = conduit_text(c, _compartment_name(Kit.get_int(c, "from", -1)), _compartment_name(Kit.get_int(c, "to", -1)))
		var send: Button = r["send"]
		if send != null:
			var busy := _jury_rigging(id)
			send.text = tr(&"hud_damage.net.jury_rig").format([str(team + 1)])
			send.disabled = not can_work or busy
			send.tooltip_text = tr(&"hud_damage.send.no_berths") if not can_work else (tr(&"hud_damage.net.jury_rig_busy") if busy else tr(&"hud_damage.net.jury_rig_tooltip"))


func _jury_rigging(conduit: int) -> bool:
	for t: Dictionary in Kit.dicts(_crew.get("teams", [])):
		if Kit.get_int(t, "kind", 0) == Bp.TEAM_JURY_RIG and Kit.get_int(t, "target", -1) == conduit:
			return true
	return false


## Mid-sentence compartment name ("compartment 3"), the number the schematic and log use.
func _compartment_name(id: int) -> String:
	return tr(&"hud_damage.compartment_in_text").format([str(id + 1)])


# --- consumers -----------------------------------------------------------------------------------

## Parts whose link on `kind` is not the normal network path, worst first, then shed parts (power):
## `Array[[slot, link, shed]]`.
func affected_consumers(kind: int) -> Array:
	var out: Array = []
	var field := LINK_KEYS[clampi(KINDS.find(kind), 0, 2)]
	for slot: int in _parts:
		var p: Dictionary = _parts[slot]
		var link := Kit.get_int(p, field, Bp.LINK_NETWORK)
		var shed := kind == Bp.NETWORK_POWER and Kit.get_bool(p, "shed")
		if link != Bp.LINK_NETWORK or shed:
			out.append([slot, link, shed])
	out.sort_custom(func(a: Array, b: Array) -> bool:
		if int(a[1]) != int(b[1]):
			return int(a[1]) > int(b[1])
		return int(a[0]) < int(b[0]))
	return out


func _show_consumers(affected: Array) -> void:
	var key: Array = []
	for a: Array in affected:
		key.append(a[0])
	if key != _consumer_key:
		_consumer_key = key
		Kit.clear(_consumers_box)
		_consumer_labels.clear()
		if not affected.is_empty():
			_consumers_box.add_child(Kit.label(tr(&"hud_damage.net.consumers_title"), &"Body", true))
		for _a: Array in affected:
			var l := Kit.label("", &"Dense", true)
			_consumers_box.add_child(l)
			_consumer_labels.append(l)
	for i in mini(affected.size(), _consumer_labels.size()):
		var a: Array = affected[i]
		var name := _Text.resolve(_part_static_by_slot.get(int(a[0]), {}).get("name", {}))
		_consumer_labels[i].text = consumer_text(name, shown_kind, int(a[1]), bool(a[2]))
	_consumers_box.visible = not affected.is_empty()
	_consumers_rule.visible = not affected.is_empty()


# --- power priorities ----------------------------------------------------------------------------

func _show_priorities() -> void:
	var rows: Array[Dictionary] = []
	if shown_kind == Bp.NETWORK_POWER:
		for p: Dictionary in _parts_static:
			if _demand.get(StringName(p.get("part", &"")), 0.0) > 0.0:
				rows.append(p)
	var key: Array = []
	for p: Dictionary in rows:
		key.append(Kit.get_int(p, "slot", -1))
	if key != _priority_key:
		_priority_key = key
		Kit.clear(_priorities_box)
		_priority_rows.clear()
		if not rows.is_empty():
			_priorities_box.add_child(Kit.label(tr(&"hud_damage.net.priority_title"), &"Body", true))
			var note := Kit.label(tr(&"hud_damage.net.priority_note"), &"Dense", true)
			_priorities_box.add_child(note)
		for p: Dictionary in rows:
			var slot := Kit.get_int(p, "slot", -1)
			var line := Kit.hbox(1)
			var label := Kit.label("", &"Dense", true)
			label.size_flags_horizontal = Control.SIZE_EXPAND_FILL
			label.size_flags_vertical = Control.SIZE_SHRINK_CENTER
			line.add_child(label)
			var lower := Kit.icon_button(&"remove", tr(&"hud_damage.net.priority_lower"))
			lower.custom_minimum_size = Vector2(ROW_H, ROW_H)
			lower.pressed.connect(_on_priority.bind(slot, -1))
			line.add_child(lower)
			var raise := Kit.icon_button(&"add", tr(&"hud_damage.net.priority_raise"))
			raise.custom_minimum_size = Vector2(ROW_H, ROW_H)
			raise.pressed.connect(_on_priority.bind(slot, 1))
			line.add_child(raise)
			_priorities_box.add_child(line)
			_priority_rows[slot] = {"label": label, "lower": lower, "raise": raise}
	for p: Dictionary in rows:
		var slot := Kit.get_int(p, "slot", -1)
		if not _priority_rows.has(slot):
			continue
		var r: Dictionary = _priority_rows[slot]
		var live: Dictionary = _parts.get(slot, {})
		var priority := clampi(Kit.get_int(live, "priority", 3), 1, 5)
		var name := _Text.resolve(p.get("name", {}))
		(r["label"] as Label).text = priority_text(name, priority, Kit.get_bool(live, "shed"))
		# Raising a priority moves it toward 1, shed last.
		var raise: Button = r["raise"]
		var lower: Button = r["lower"]
		raise.disabled = priority <= 1
		lower.disabled = priority >= 5
		raise.accessibility_name = tr(&"hud_damage.net.priority_raise_access").format([name])
		lower.accessibility_name = tr(&"hud_damage.net.priority_lower_access").format([name])
	_priorities_box.visible = not rows.is_empty()
	_priority_rule.visible = not rows.is_empty()


func _on_priority(slot: int, step: int) -> void:
	var live: Dictionary = _parts.get(slot, {})
	var priority := clampi(Kit.get_int(live, "priority", 3), 1, 5)
	var next := next_priority(priority, step)
	if next != priority:
		Kit.call_on(&"Orders", &"set_power_priority", [slot, next])


func _on_jury_rig(conduit: int) -> void:
	var team := _Damage.pick_team(Kit.dicts(_crew.get("teams", [])))
	Kit.call_on(&"Orders", &"assign_team", [team, Bp.TEAM_JURY_RIG, conduit])


# --- focus ---------------------------------------------------------------------------------------

func focus_targets() -> Array:
	var out: Array = []
	for b: Button in _tab_buttons:
		out.append(b)
	for id: int in _conduit_rows:
		if _conduit_rows[id]["send"] != null:
			out.append(_conduit_rows[id]["send"])
	for slot: int in _priority_rows:
		out.append(_priority_rows[slot]["lower"])
		out.append(_priority_rows[slot]["raise"])
	return out


func _chain() -> void:
	var list: Array = [_tab_buttons[0]]
	for c: Control in focus_targets().slice(3):
		list.append(c)
	Kit.chain_vertical(list)
	Kit.chain_horizontal(_tab_buttons)
	for slot: int in _priority_rows:
		Kit.chain_horizontal([_priority_rows[slot]["lower"], _priority_rows[slot]["raise"]])


# --- text (static so tests share them) -----------------------------------------------------------

## The priority after Raise (`step` +1: a smaller number, shed later) or Lower (-1), within 1..5.
static func next_priority(priority: int, step: int) -> int:
	return clampi(clampi(priority, 1, 5) - signi(step), 1, 5)


static func _t(key: StringName) -> String:
	return String(TranslationServer.translate(key))


static func _kind_name(kind: int) -> String:
	return _t(StringName("hud_damage.net.%s" % LINK_KEYS[clampi(KINDS.find(kind), 0, 2)]))


## "Power" or "Power, 2 cut".
static func tab_text(kind: int, net: Dictionary) -> String:
	var cut := 0
	for c: Dictionary in Kit.dicts(net.get("conduits", [])):
		if Kit.get_bool(c, "cut"):
			cut += 1
	if cut == 0:
		return _kind_name(kind)
	return _t(&"hud_damage.net.tab_cut").format([_kind_name(kind), str(cut)])


## "14 conduits: 2 cut, 1 jury-rigged, 3 doubled. 3 consumers on fallback, 1 dead. 2 shed."
static func summary_text(net: Dictionary, affected: int, shed: int) -> String:
	var conduits := Kit.dicts(net.get("conduits", []))
	var cut := 0
	var rigged := 0
	var doubled := 0
	for c: Dictionary in conduits:
		if Kit.get_bool(c, "cut"):
			cut += 1
		elif Kit.get_bool(c, "jury_rigged"):
			rigged += 1
		if Kit.get_bool(c, "redundant"):
			doubled += 1
	var state: PackedStringArray = []
	if cut > 0:
		state.append(_t(&"hud_damage.net.sum.cut").format([str(cut)]))
	if rigged > 0:
		state.append(_t(&"hud_damage.net.sum.rigged").format([str(rigged)]))
	if doubled > 0:
		state.append(_t(&"hud_damage.net.sum.doubled").format([str(doubled)]))
	var text := ""
	if conduits.is_empty():
		text = _t(&"hud_damage.net.sum.no_conduits")
	elif conduits.size() == 1 and state.is_empty():
		text = _t(&"hud_damage.net.sum.intact_one")
	elif state.is_empty():
		text = _t(&"hud_damage.net.sum.intact").format([str(conduits.size())])
	else:
		text = _t(&"hud_damage.net.sum.conduits").format([str(conduits.size()), ", ".join(state)])
	var dead := Kit.get_int(net, "consumers_dead", 0)
	var fallback := Kit.get_int(net, "consumers_on_buffer", 0)
	if fallback > 0 or dead > 0:
		text += " " + _t(&"hud_damage.net.sum.consumer_one" if fallback == 1 else &"hud_damage.net.sum.consumers").format([str(fallback), str(dead)])
	elif affected == 0:
		text += " " + _t(&"hud_damage.net.sum.all_supplied")
	if shed > 0 and Kit.get_int(net, "kind", -1) == Bp.NETWORK_POWER:
		text += " " + (_t(&"hud_damage.net.sum.shed_one") if shed == 1 else _t(&"hud_damage.net.sum.shed").format([str(shed)]))
	return text


## "Conduit 7, compartment 3 to compartment 5: cut" or "...: jury-rigged at 50% until the tender".
static func conduit_text(c: Dictionary, from_name: String, to_name: String) -> String:
	var key := &"hud_damage.net.conduit_cut" if Kit.get_bool(c, "cut") else &"hud_damage.net.conduit_rigged"
	var text := _t(key).format([str(Kit.get_int(c, "id", 0) + 1), from_name, to_name])
	if Kit.get_bool(c, "redundant"):
		text = _t(&"hud_damage.net.conduit_doubled").format([text])
	return text


## "Gun 2: local buffer, battery 30 s" and the other fallback steps of L394.
static func consumer_text(name: String, kind: int, link: int, shed: bool) -> String:
	var state := ""
	match link:
		Bp.LINK_REDUNDANT: state = _t(&"hud_damage.net.link.redundant")
		Bp.LINK_BUFFER: state = _t(&"hud_damage.net.link.buffer_fuel" if kind == Bp.NETWORK_FUEL else &"hud_damage.net.link.buffer_power")
		Bp.LINK_LOCAL_CONTROL: state = _t(&"hud_damage.net.link.local")
		Bp.LINK_DEAD: state = _t(&"hud_damage.net.link.dead")
	if shed:
		state = _t(&"hud_damage.net.link.shed") if state.is_empty() else _t(&"hud_damage.net.link.shed_and").format([state])
	return _t(&"hud_damage.net.consumer").format([name, state])


## "Gun 1: priority 3" or "Gun 1: priority 5, shed".
static func priority_text(name: String, priority: int, shed: bool) -> String:
	var text := _t(&"hud_damage.net.priority_row").format([name, str(priority)])
	if shed:
		text = _t(&"hud_damage.net.priority_shed").format([text])
	return text


# --- the diagram ---------------------------------------------------------------------------------

## The hull deck by deck in plan, top deck first, with one network's conduits.
class NetDiagram extends Control:
	const PAD := 8.0
	const GAP := 26.0
	const MAX_HEIGHT := 440.0
	const ICON_PX := 16.0

	var _kind := 0
	var _net: Dictionary = {}
	var _statics: Array[Dictionary] = []
	var _outline: Dictionary = {}
	var _decks: Array[int] = []
	var _length := 1.0
	var _beam := 1.0
	var _scale := 1.0
	var _x0 := 0.0

	func _init() -> void:
		mouse_filter = Control.MOUSE_FILTER_PASS
		size_flags_horizontal = Control.SIZE_EXPAND_FILL
		resized.connect(_relayout)

	func _notification(what: int) -> void:
		if what == NOTIFICATION_THEME_CHANGED:
			queue_redraw()

	func show_data(kind: int, net: Dictionary, statics: Array[Dictionary], outline: Dictionary) -> void:
		_kind = kind
		_net = net
		_statics = statics
		_outline = outline
		_decks.clear()
		var l := float(outline.get("length_m", 0.0))
		var b := float(outline.get("beam_m", 0.0))
		for c: Dictionary in statics:
			var d := int(c.get("deck", 0))
			if not _decks.has(d):
				_decks.append(d)
			var r: Variant = c.get("rect")
			if r is Rect2:
				l = maxf(l, (r as Rect2).end.x)
				b = maxf(b, (r as Rect2).end.y)
		_decks.sort()
		_decks.reverse()
		_length = maxf(l, 1.0)
		_beam = maxf(b, 0.5)
		var cut := 0
		for c: Dictionary in Kit.dicts(net.get("conduits", [])):
			if bool(c.get("cut", false)):
				cut += 1
		accessibility_name = TranslationServer.translate(&"hud_damage.net.diagram_access").format([NetworksText.kind(kind), str(Kit.dicts(net.get("conduits", [])).size()), str(cut)])
		_relayout()

	func _relayout() -> void:
		var n := maxi(_decks.size(), 1)
		var w := maxf(size.x - PAD * 2.0, 10.0)
		_scale = w / _length
		var needed := _beam * _scale * n + GAP * n + PAD
		if needed > MAX_HEIGHT:
			_scale = (MAX_HEIGHT - GAP * n - PAD) / (_beam * n)
		_x0 = (size.x - _length * _scale) * 0.5
		var want := ceilf(_beam * _scale * n + GAP * n + PAD)
		if not is_equal_approx(custom_minimum_size.y, want):
			custom_minimum_size = Vector2(0.0, want)
		queue_redraw()

	func _band_top(deck: int) -> float:
		var i := _decks.find(deck)
		return GAP * float(i + 1) + _beam * _scale * float(i)

	func _rect(c: Dictionary) -> Rect2:
		var r: Variant = c.get("rect")
		if not r is Rect2:
			return Rect2()
		var rr := r as Rect2
		return Rect2(Vector2(_x0 + rr.position.x * _scale, _band_top(int(c.get("deck", 0))) + rr.position.y * _scale), rr.size * _scale)

	func _draw() -> void:
		var inset := get_theme_color(&"surface_inset", &"Plot")
		var raised := get_theme_color(&"surface_raised", &"Plot")
		var divider := get_theme_color(&"rule_divider", &"Plot")
		var reference := get_theme_color(&"rule_reference", &"Plot")
		var primary := get_theme_color(&"text_primary", &"Plot")
		var secondary := get_theme_color(&"text_secondary", &"Plot")
		var font := get_theme_font(&"label", &"Plot")
		var fs := get_theme_font_size(&"label", &"Plot")
		draw_rect(Rect2(Vector2.ZERO, size), inset, true)
		if _statics.is_empty():
			return
		var plan: Variant = _outline.get("plan", PackedVector2Array())
		var centres: Dictionary[int, Vector2] = {}
		for deck: int in _decks:
			var top := _band_top(deck)
			if font != null:
				var label := TranslationServer.translate(&"hud_damage.deck").format([str(deck + 1)])
				draw_string(font, Vector2(PAD, top - 6.0), label, HORIZONTAL_ALIGNMENT_LEFT, -1, fs, reference)
			if plan is PackedVector2Array and (plan as PackedVector2Array).size() >= 3:
				var pts := PackedVector2Array()
				for p: Vector2 in plan as PackedVector2Array:
					pts.append(Vector2(_x0 + p.x * _scale, top + p.y * _scale))
				pts.append(pts[0])
				draw_polyline(pts, reference, 1.0)
		for c: Dictionary in _statics:
			var r := _rect(c)
			if r.size.x < 1.0 or r.size.y < 1.0:
				continue
			draw_rect(r, raised, true)
			draw_rect(r, divider, false, 1.0)
			centres[int(c.get("id", -1))] = r.get_center()
			if font != null and r.size.x >= 18.0 and r.size.y >= 18.0:
				var t := str(int(c.get("id", -1)) + 1)
				var tw := font.get_string_size(t, HORIZONTAL_ALIGNMENT_LEFT, -1, fs).x
				draw_string(font, Vector2(r.position.x + 3.0, r.position.y + font.get_ascent(fs) + 1.0), t, HORIZONTAL_ALIGNMENT_LEFT, tw + 2.0, fs, secondary)
		# Conduits: intact first, then jury-rigged, then cut on top.
		var conduits := Kit.dicts(_net.get("conduits", []))
		for pass_i in 3:
			for c: Dictionary in conduits:
				var cut := bool(c.get("cut", false))
				var rigged := bool(c.get("jury_rigged", false))
				var layer := 2 if cut else (1 if rigged else 0)
				if layer != pass_i:
					continue
				var a: Variant = centres.get(int(c.get("from", -1)))
				var b: Variant = centres.get(int(c.get("to", -1)))
				if not (a is Vector2 and b is Vector2):
					continue
				_conduit(a, b, cut, rigged, bool(c.get("redundant", false)), primary, secondary, inset)

	func _conduit(a: Vector2, b: Vector2, cut: bool, rigged: bool, doubled: bool, primary: Color, secondary: Color, inset: Color) -> void:
		var colour := primary if cut or rigged else secondary
		var width := 1.0 if rigged else 2.0
		var d := b - a
		var length := d.length()
		if length < 1.0:
			return
		var dir := d / length
		var normal := Vector2(-dir.y, dir.x)
		var offsets: Array[float] = [0.0]
		if doubled:
			offsets = [-2.0, 2.0]
		for off: float in offsets:
			var p0 := a + normal * off
			var p1 := b + normal * off
			if cut:
				# Broken in the middle.
				var mid := (p0 + p1) * 0.5
				var gap := minf(ICON_PX * 0.75, length * 0.3)
				_pattern(p0, mid - dir * gap, colour, width)
				_pattern(mid + dir * gap, p1, colour, width)
			else:
				_pattern(p0, p1, colour, width)
		var icon_stem := &"close" if cut else (&"link" if rigged else &"")
		if icon_stem != &"":
			var icon := Ui.icon(icon_stem)
			if icon != null:
				var at := (a + b) * 0.5 - Vector2(ICON_PX, ICON_PX) * 0.5
				draw_rect(Rect2(at, Vector2(ICON_PX, ICON_PX)).grow(1.0), inset, true)
				draw_texture_rect(icon, Rect2(at, Vector2(ICON_PX, ICON_PX)), false, primary)

	## One segment in the network's pattern: power solid, fuel dashed, data dotted.
	func _pattern(p0: Vector2, p1: Vector2, colour: Color, width: float) -> void:
		var length := p0.distance_to(p1)
		if length < 0.5:
			return
		if _kind == Bp.NETWORK_POWER:
			draw_line(p0, p1, colour, width, true)
			return
		var dash := 8.0 if _kind == Bp.NETWORK_FUEL else 2.0
		var step := 13.0 if _kind == Bp.NETWORK_FUEL else 6.0
		var dir := (p1 - p0) / length
		var segs := PackedVector2Array()
		var t := 0.0
		while t < length:
			segs.append(p0 + dir * t)
			segs.append(p0 + dir * minf(t + dash, length))
			t += step
		if segs.size() >= 2:
			draw_multiline(segs, colour, width)


## Network names for the diagram's inner class.
class NetworksText extends RefCounted:
	static func kind(k: int) -> String:
		var keys: Array[StringName] = [&"hud_damage.net.power", &"hud_damage.net.fuel", &"hud_damage.net.data"]
		return String(TranslationServer.translate(keys[clampi(k, 0, 2)]))
