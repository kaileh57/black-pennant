extends VBoxContainer
## In-sortie control layers L0 to L2 and emission control.
## DESIGN.md L322-336, L505-514. Unit: ui-hud-control.
## Reads and calls: Hud.preset, Hud.sliders, Hud.doctrine, Hud.rules, Hud.emcon, Orders.set_preset, Orders.set_slider, Orders.set_doctrine, Orders.set_rules, Orders.set_emcon.
##
## The control station of the sortie HUD: the rungs of the control ladder a sortie allows
## (`Hud.max_layer`; graphs are never edited in a sortie, L112), one page each, most used first:
## - Doctrine (L0): the ship's preset (Aggressive, Cautious, Silent running) and each mount's,
##   which inherits the ship's until set; then the engagement doctrine rows (L479-489) of the ship
##   or one mount: minimum stance, identification and grade, salvo and effective range.
## - Tuning (L1): per officer the three to six named sliders, and every other parameter under
##   All parameters.
## - Rules (L2): the ordered when/then table. A rule can be switched off and on, moved or removed,
##   and the design's table restored; new rules are written in the dry dock, where the rule
##   builder knows the design's sinks.
## - Emissions (L505-514): Silent, Burst or Active, what each does, the briefing's
##   recommendation, the burst trigger and self-defence ranges, the emitters radiating now and the
##   ESM warning.
## - Power (L287-295): each consumer's shedding priority, 1 (shed last) to 5.
## Every automatic value carries the A chip; an overridden one carries the hand glyph, and
## pressing it reverts that value (L351).
##
## Decisions where the contract is silent:
## - Values the player sets show at once and hold until the board's next report agrees or 1.5 s
##   pass, so a control never jumps back while the round trip is in flight. Sliders send when a
##   drag ends, and 0.25 s after the last keyboard or gamepad step.
## - The protocol does not report doctrine rows or the rules table after an in-sortie change
##   (`Hud.doctrine` and `Hud.rules` keep the design's values). The panel therefore keeps what it
##   sent for the rest of the sortie and shows that, marked overridden; reverting a doctrine row
##   sends the design's row again (and, once no row of that scope differs, reverts the scope's
##   stored changes by the Gunnery officer's ids), and Restore the design's rules sends the
##   design's table.
## - Revert ids follow `OverrideDict.id` for the values whose dictionaries carry no id (one
##   helper, `_override_id`, mirrors `hud::override_id::encode`).
## - The burst trigger and self-defence ranges have no sortie command: they show their value and
##   chip, revert with one click, and are set in the dry dock.
## - Consumers on the power page are the parts whose catalogue entry draws power.
## `show_page(name)` opens a page (`&"doctrine"`, `&"tuning"`, `&"rules"`, `&"emissions"`, `&"power"`).

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const ChipScene := preload("res://ui/widgets/auto_chip.tscn")

const PAGES: Array[StringName] = [&"doctrine", &"tuning", &"rules", &"emissions", &"power"]
const PAGE_KEYS: Array[StringName] = [&"hud_control.page.doctrine", &"hud_control.page.tuning", &"hud_control.page.rules", &"hud_control.page.emissions", &"hud_control.page.power"]
## Lowest `Bp.LAYER_*` each page needs.
const PAGE_LAYER: Array[int] = [0, 1, 2, 1, 1]
## `Bp.OFFICER_*` in display order: Helm, Gunnery, Sensors, Engineer, Damage Control.
const OFFICERS: Array[int] = [0, 3, 2, 1, 4]
const OFFICER_ENGINEER := 1
const OFFICER_SENSORS := 2
const OFFICER_GUNNERY := 3
const NONE := -1
const PRESETS := 3
const EMCONS := 3
const STANCE_SUSPECT := 2
const STANCE_HOSTILE := 3
const RUNGS := 5
const GRADES := 4
## The simulation's cap (sim_systems::doctrine::MAX_SALVO).
const MAX_SALVO := 16
const MAX_RULES := 32
const PENDING_MS := 1500
const SLIDER_DEBOUNCE_S := 0.25
const ROW_H := 40.0
const RANGE_MIN_KM := 0.5
const RANGE_MAX_KM := 200.0
## One search-radar antenna rotation (L509: three rotations are 7.5 s).
const ANTENNA_ROTATION_S := 2.5

@onready var _tabs: TabBar = $Tabs
@onready var _scroll: ScrollContainer = $Scroll
@onready var _margin: MarginContainer = $Scroll/Margin
@onready var _pages_box: VBoxContainer = $Scroll/Margin/Pages
@onready var _status: Label = $Status
@onready var _empty: Label = $Empty

var _live := false
var _page := 0
var _max_layer := 2
var _built: Dictionary[StringName, bool] = {}
var _page_nodes: Dictionary[StringName, VBoxContainer] = {}
## Values sent but not yet reported back: key to `{ "value": Variant, "until": int }`.
var _pending: Dictionary[String, Dictionary] = {}
## Override ids the officers' lists hold, per officer.
var _override_ids: Dictionary[int, PackedStringArray] = {}

# Doctrine page.
var _ship_preset: OptionButton
var _ship_chip: Button
var _ship_description: Label
var _mount_rows: Array[Dictionary] = []
var _scope: OptionButton
var _scope_slots: Array[int] = []
var _doctrine_box: VBoxContainer
var _doctrine_rows: Array[Dictionary] = []
## Rows sent this sortie, per scope slot (`NONE` = ship): "weapon:target" to `DoctrineEntryDict`.
var _doctrine_sent: Dictionary[int, Dictionary] = {}

# Tuning page.
var _officer_tabs: TabBar
var _officer := 0
var _slider_box: VBoxContainer
var _advanced_toggle: Button
var _advanced_box: VBoxContainer
var _slider_rows: Dictionary[String, Dictionary] = {}
var _dragging := ""
var _debounce: Timer
var _debounce_key := ""

# Rules page.
var _rules_box: VBoxContainer
var _rules_note: Label
var _rules_restore: Button
## The table as last sent this sortie, or empty while the design's table stands. Each entry is a
## `RuleDict` with `orig` (index in the design's table, for its active mark).
var _rules_sent: Array[Dictionary] = []
var _rules_changed := false

# Emissions page.
var _emcon_buttons: Array[Button] = []
var _emcon_chip: Button
var _emcon_description: Label
var _emcon_recommended: Label
var _emcon_reason: Label
var _trigger_label: Label
var _trigger_chip: Button
var _defence_label: Label
var _defence_chip: Button
var _radiating: Label
var _esm: Label

# Power page.
var _power_summary: Label
var _power_box: VBoxContainer
var _power_rows: Array[Dictionary] = []


func _ready() -> void:
	Ui.set_separation(self, 2)
	Ui.set_margins(_margin, 1)
	_margin.add_theme_constant_override(&"margin_left", 0)
	Ui.set_separation(_pages_box, 2)
	_status.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_empty.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_tabs.focus_mode = Control.FOCUS_ALL
	_tabs.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	for i in PAGES.size():
		_tabs.add_tab(tr(PAGE_KEYS[i]))
		var page := Kit.vbox(2)
		page.name = String(PAGES[i]).capitalize().replace(" ", "")
		page.visible = false
		_pages_box.add_child(page)
		_page_nodes[PAGES[i]] = page
	_tabs.tab_changed.connect(_on_tab_changed)
	_debounce = Timer.new()
	_debounce.one_shot = true
	_debounce.wait_time = SLIDER_DEBOUNCE_S
	_debounce.timeout.connect(_flush_slider)
	add_child(_debounce)
	visibility_changed.connect(_on_visibility_changed)
	var hud := Kit.autoload(&"Hud")
	if hud != null:
		_live = true
		if hud.has_signal(&"board_changed"):
			hud.connect(&"board_changed", _on_board_changed)
		if hud.has_signal(&"ship_static_changed"):
			hud.connect(&"ship_static_changed", _on_ship_static)
		if hud.has_signal(&"power_changed"):
			hud.connect(&"power_changed", _on_power_changed)
		if hud.has_signal(&"parts_changed"):
			hud.connect(&"parts_changed", _on_power_changed)
		if hud.has_signal(&"overrides_reply"):
			hud.connect(&"overrides_reply", _on_overrides_reply)
		if hud.has_signal(&"override_changed"):
			hud.connect(&"override_changed", _on_override_changed)
	var session := Kit.autoload(&"Session")
	if session != null and session.has_signal(&"state_changed"):
		session.connect(&"state_changed", func(_s: int) -> void: _on_ship_static())
	_on_ship_static()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		for i in PAGES.size():
			_tabs.set_tab_title(i, tr(PAGE_KEYS[i]))
		_rebuild_all()


## Open one page by name.
func show_page(page_name: StringName) -> void:
	var i := PAGES.find(page_name)
	if i < 0 or _tabs.is_tab_disabled(i):
		return
	_tabs.current_tab = i
	_on_tab_changed(i)


func _on_tab_changed(i: int) -> void:
	_page = clampi(i, 0, PAGES.size() - 1)
	for p in PAGES.size():
		_page_nodes[PAGES[p]].visible = p == _page
	_status.visible = false
	_show_current()


func _on_visibility_changed() -> void:
	if is_visible_in_tree():
		_show_current()


func _has_ship() -> bool:
	return _live and Kit.call_on(&"Hud", &"has_ship") == true


func _on_ship_static() -> void:
	_doctrine_sent.clear()
	_rules_sent.clear()
	_rules_changed = false
	_pending.clear()
	_override_ids.clear()
	_rebuild_all()


func _rebuild_all() -> void:
	if not is_node_ready():
		return
	_built.clear()
	var layer: Variant = Kit.call_on(&"Hud", &"max_layer")
	_max_layer = int(layer) if layer is int else 2
	for i in PAGES.size():
		var allowed := PAGE_LAYER[i] <= _max_layer
		_tabs.set_tab_disabled(i, not allowed)
		_tabs.set_tab_tooltip(i, "" if allowed else tr(&"hud_control.page.not_allowed"))
		Kit.clear(_page_nodes[PAGES[i]])
	if _tabs.is_tab_disabled(_page):
		_page = 0
		_tabs.current_tab = 0
	_show_current()


func _show_current() -> void:
	if not is_node_ready():
		return
	var ship := _has_ship()
	_tabs.visible = ship
	_scroll.visible = ship
	_empty.visible = not ship
	if not ship:
		_empty.text = tr(&"hud_control.waiting")
		return
	var page := PAGES[_page]
	for p in PAGES.size():
		_page_nodes[PAGES[p]].visible = p == _page
	if not _built.get(page, false):
		_built[page] = true
		match page:
			&"doctrine": _build_doctrine()
			&"tuning": _build_tuning()
			&"rules": _build_rules()
			&"emissions": _build_emissions()
			&"power": _build_power()
		_link_focus()
	_update_current()


func _update_current() -> void:
	if not is_visible_in_tree() or not _has_ship():
		return
	match PAGES[_page]:
		&"doctrine": _update_doctrine()
		&"tuning": _update_tuning()
		&"rules": _update_rules()
		&"emissions": _update_emissions()
		&"power": _update_power()


func _on_board_changed() -> void:
	if not _has_ship():
		if _tabs.visible:
			_show_current()
		return
	if not _tabs.visible:
		_show_current()
		return
	_update_current()


func _on_power_changed() -> void:
	if PAGES[_page] == &"power" and is_visible_in_tree() and _built.get(&"power", false):
		_update_power()


# --- shared helpers ------------------------------------------------------------------------------

## `OverrideDict.id` of a value whose dictionary carries none; mirrors `hud::override_id::encode`.
static func _override_id(kind: String, arg: Variant = null) -> String:
	match kind:
		"preset", "emcon", "burst_trigger_range", "self_defence_range":
			return kind
		"mount_preset", "power_priority":
			return "%s/%d" % [kind, int(arg)]
		"slider":
			return "slider/%s" % String(arg)
	return ""


func _revert(id: String) -> void:
	if id.is_empty():
		return
	_report(Kit.call_on(&"Orders", &"revert", [id]) == true)


## Status after a command: sent, or why not.
func _report(sent: bool, message: String = "") -> void:
	_status.visible = true
	if not message.is_empty():
		_status.text = message
	else:
		_status.text = tr(&"hud_control.status.sent") if sent else tr(&"hud_control.status.not_sent")


func _hold(key: String, value: Variant) -> void:
	_pending[key] = {"value": value, "until": Time.get_ticks_msec() + PENDING_MS}


## The value to show for `key`: the pending one while the round trip is in flight, else `reported`.
func _shown(key: String, reported: Variant) -> Variant:
	var p: Dictionary = _pending.get(key, {})
	if p.is_empty():
		return reported
	if Time.get_ticks_msec() > int(p["until"]) or p["value"] == reported:
		_pending.erase(key)
		return reported
	return p["value"]


func _chip(revert_id: String) -> Button:
	var chip: Button = ChipScene.instantiate()
	chip.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	if not revert_id.is_empty():
		chip.connect(&"revert_requested", _revert.bind(revert_id))
	return chip


static func _set_chip(chip: Button, overridden: bool, default_text: String = "") -> void:
	chip.set(&"overridden", overridden)
	chip.set(&"default_text", default_text)


func _word(kind: StringName, value: int) -> String:
	if value < 0:
		return ""
	var tx := Kit.autoload(&"Tx")
	if tx != null and tx.has_method(&"word"):
		return String(tx.call(&"word", kind, value))
	return str(value)


func _number(value: float, unit: StringName, decimals: int) -> String:
	return Kit.number(value, unit, decimals)


func _range(metres: float) -> String:
	var tx := Kit.autoload(&"Tx")
	if tx != null and tx.has_method(&"range"):
		return String(tx.call(&"range", metres))
	return Kit.number(metres, &"m", 0)


func _option(items: Array[String], min_w: float = 0.0) -> OptionButton:
	var o := OptionButton.new()
	o.focus_mode = Control.FOCUS_ALL
	o.custom_minimum_size = Vector2(min_w, ROW_H)
	o.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	o.fit_to_longest_item = true
	for i in items.size():
		o.add_item(items[i], i)
	return o


func _row() -> HBoxContainer:
	var r := HBoxContainer.new()
	Ui.set_separation(r, 2)
	return r


func _name_label(text: String, variation: StringName = &"Body") -> Label:
	var l := Kit.label(text, variation, true)
	l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	l.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	return l


func _part_names() -> Dictionary[int, String]:
	var out: Dictionary[int, String] = {}
	for p: Dictionary in Kit.dicts(Kit.call_on(&"Hud", &"parts_static")):
		out[Kit.get_int(p, "slot", NONE)] = _Text.resolve(p.get("name", {}))
	return out


func _mounts() -> Array[Dictionary]:
	var out: Array[Dictionary] = []
	for p: Dictionary in Kit.dicts(Kit.call_on(&"Hud", &"parts_static")):
		if Kit.get_bool(p, "is_mount"):
			out.append(p)
	return out


# --- doctrine (L0) -------------------------------------------------------------------------------

func _preset_names() -> Array[String]:
	var names: Array[String] = []
	for p in PRESETS:
		names.append(_word(&"preset", p))
	return names


func _build_doctrine() -> void:
	var page := _page_nodes[&"doctrine"]
	var ship_row := _row()
	ship_row.add_child(_name_label(tr(&"hud_control.doctrine.ship")))
	_ship_preset = _option(_preset_names(), 200.0)
	_ship_preset.accessibility_name = tr(&"hud_control.doctrine.ship")
	_ship_preset.item_selected.connect(_on_ship_preset)
	ship_row.add_child(_ship_preset)
	_ship_chip = _chip(_override_id("preset"))
	ship_row.add_child(_ship_chip)
	page.add_child(ship_row)
	_ship_description = Kit.label("", &"Dense", true)
	page.add_child(_ship_description)
	_mount_rows.clear()
	var mounts := _mounts()
	if not mounts.is_empty():
		page.add_child(HSeparator.new())
		page.add_child(Kit.label("hud_control.doctrine.mounts", &"Body"))
	for m: Dictionary in mounts:
		var slot := Kit.get_int(m, "slot", NONE)
		var row := _row()
		var name := _Text.resolve(m.get("name", {}))
		row.add_child(_name_label(name, &"Dense"))
		var items: Array[String] = [""]
		items.append_array(_preset_names())
		var o := _option(items, 200.0)
		o.accessibility_name = name
		o.item_selected.connect(_on_mount_preset.bind(slot))
		row.add_child(o)
		var chip := _chip(_override_id("mount_preset", slot))
		row.add_child(chip)
		page.add_child(row)
		_mount_rows.append({"slot": slot, "option": o, "chip": chip})
	page.add_child(HSeparator.new())
	var head := _row()
	head.add_child(_name_label(tr(&"hud_control.doctrine.rows")))
	var scopes: Array[String] = [tr(&"hud_control.doctrine.scope_ship")]
	_scope_slots = [NONE]
	for m: Dictionary in mounts:
		scopes.append(_Text.resolve(m.get("name", {})))
		_scope_slots.append(Kit.get_int(m, "slot", NONE))
	_scope = _option(scopes, 200.0)
	_scope.accessibility_name = tr(&"hud_control.doctrine.scope")
	_scope.tooltip_text = tr(&"hud_control.doctrine.scope_tip")
	_scope.item_selected.connect(_on_scope_selected)
	head.add_child(_scope)
	page.add_child(head)
	page.add_child(Kit.label("hud_control.doctrine.rows_note", &"Dense"))
	_doctrine_box = Kit.vbox(2)
	page.add_child(_doctrine_box)
	_build_doctrine_rows()


func _on_scope_selected(_index: int) -> void:
	_build_doctrine_rows()
	_update_doctrine()
	_link_focus()


func _scope_slot() -> int:
	if _scope == null:
		return NONE
	var i := _scope.selected
	return _scope_slots[i] if i >= 0 and i < _scope_slots.size() else NONE


func _build_doctrine_rows() -> void:
	Kit.clear(_doctrine_box)
	_doctrine_rows.clear()
	var slot := _scope_slot()
	var rows := Kit.dicts(Kit.call_on(&"Hud", &"doctrine", [slot]))
	if rows.is_empty():
		_doctrine_box.add_child(Kit.label("hud_control.doctrine.no_rows", &"Dense"))
		return
	var editable := _max_layer >= 1
	for e: Dictionary in rows:
		var weapon := Kit.get_int(e, "weapon", 0)
		var target := Kit.get_int(e, "target", 0)
		var box := Kit.vbox(1)
		var head := _row()
		head.add_child(_name_label(tr(&"hud_control.doctrine.pair").format([_word(&"weapon", weapon), _word(&"target", target)])))
		var chip := _chip("")
		chip.connect(&"revert_requested", _revert_doctrine.bind(weapon, target))
		head.add_child(chip)
		box.add_child(head)
		var flow := HFlowContainer.new()
		flow.add_theme_constant_override(&"h_separation", Ui.spacing(1))
		flow.add_theme_constant_override(&"v_separation", Ui.spacing(1))
		var stance_items: Array[String] = []
		stance_items.append(tr(&"hud_control.doctrine.stance_suspect"))
		stance_items.append(tr(&"hud_control.doctrine.stance_hostile"))
		var stance := _option(stance_items)
		stance.tooltip_text = tr(&"hud_control.doctrine.stance_tip")
		var rung_items: Array[String] = []
		for r in RUNGS:
			rung_items.append(tr(&"hud_control.doctrine.rung").format([_word(&"rung", r)]))
		var rung := _option(rung_items)
		rung.tooltip_text = tr(&"hud_control.doctrine.rung_tip")
		var grade_items: Array[String] = []
		for g in GRADES:
			grade_items.append(tr(&"hud_control.doctrine.grade").format([_word(&"grade", g)]))
		var grade := _option(grade_items)
		grade.tooltip_text = tr(&"hud_control.doctrine.grade_tip")
		var salvo := SpinBox.new()
		# 0 is the hold encoding (fire only at a designated track), as silent running sets it.
		salvo.min_value = 0
		salvo.max_value = MAX_SALVO
		salvo.step = 1
		salvo.rounded = true
		salvo.prefix = tr(&"hud_control.doctrine.salvo_prefix")
		salvo.custom_minimum_size = Vector2(150.0, ROW_H)
		salvo.tooltip_text = tr(&"hud_control.doctrine.salvo_tip")
		salvo.focus_mode = Control.FOCUS_ALL
		salvo.get_line_edit().focus_mode = Control.FOCUS_ALL
		var auto_range := CheckBox.new()
		auto_range.text = tr(&"hud_control.doctrine.range_default")
		auto_range.tooltip_text = tr(&"hud_control.doctrine.range_default_tip")
		auto_range.focus_mode = Control.FOCUS_ALL
		auto_range.custom_minimum_size = Vector2(0.0, ROW_H)
		auto_range.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		var range_km := SpinBox.new()
		range_km.min_value = RANGE_MIN_KM
		range_km.max_value = RANGE_MAX_KM
		range_km.step = 0.5
		range_km.suffix = tr(&"hud_control.doctrine.range_suffix")
		range_km.custom_minimum_size = Vector2(140.0, ROW_H)
		range_km.tooltip_text = tr(&"hud_control.doctrine.range_tip")
		# Two lines: who the weapon fires at, then how much and how far.
		for c: Control in [stance, rung, grade]:
			flow.add_child(c)
		var flow2 := HFlowContainer.new()
		flow2.add_theme_constant_override(&"h_separation", Ui.spacing(1))
		flow2.add_theme_constant_override(&"v_separation", Ui.spacing(1))
		for c: Control in [salvo, auto_range, range_km]:
			flow2.add_child(c)
		salvo.editable = editable
		range_km.editable = editable
		stance.disabled = not editable
		rung.disabled = not editable
		grade.disabled = not editable
		auto_range.disabled = not editable
		box.add_child(flow)
		box.add_child(flow2)
		_doctrine_box.add_child(box)
		var row := {"weapon": weapon, "target": target, "chip": chip, "stance": stance, "rung": rung, "grade": grade, "salvo": salvo, "auto": auto_range, "range": range_km, "entry": e.duplicate(), "design": e.duplicate()}
		_doctrine_rows.append(row)
		var i := _doctrine_rows.size() - 1
		stance.item_selected.connect(func(_x: int) -> void: _send_doctrine(i))
		rung.item_selected.connect(func(_x: int) -> void: _send_doctrine(i))
		grade.item_selected.connect(func(_x: int) -> void: _send_doctrine(i))
		salvo.value_changed.connect(func(_x: float) -> void: _send_doctrine(i))
		auto_range.toggled.connect(func(_x: bool) -> void: _send_doctrine(i))
		range_km.value_changed.connect(func(_x: float) -> void: _send_doctrine(i))


static func _pair_key(weapon: int, target: int) -> String:
	return "%d:%d" % [weapon, target]


func _update_doctrine() -> void:
	var ship := Kit.as_dict(Kit.call_on(&"Hud", &"preset", [NONE]))
	var preset := int(_shown("preset", Kit.get_int(ship, "preset", 1)))
	_ship_preset.select(clampi(preset, 0, PRESETS - 1))
	_set_chip(_ship_chip, Kit.get_bool(ship, "overridden"))
	var keys: Array[StringName] = [&"tuning.preset.aggressive.description", &"tuning.preset.cautious.description", &"tuning.preset.silent_running.description"]
	_ship_description.text = tr(keys[clampi(preset, 0, PRESETS - 1)])
	for m: Dictionary in _mount_rows:
		var slot: int = m["slot"]
		var d := Kit.as_dict(Kit.call_on(&"Hud", &"preset", [slot]))
		var o: OptionButton = m["option"]
		o.set_item_text(0, tr(&"hud_control.doctrine.as_ship").format([_word(&"preset", preset)]))
		var overridden := Kit.get_bool(d, "overridden")
		var own := Kit.get_int(d, "preset", preset)
		var index := 0 if (not overridden and own == preset) else own + 1
		index = int(_shown("mount_preset/%d" % slot, index))
		o.select(clampi(index, 0, PRESETS))
		_set_chip(m["chip"], overridden, o.get_item_text(0))
	# Doctrine rows: the reported rows unless this sortie sent its own.
	var slot := _scope_slot()
	var reported := Kit.dicts(Kit.call_on(&"Hud", &"doctrine", [slot]))
	var by_pair: Dictionary[String, Dictionary] = {}
	for e: Dictionary in reported:
		by_pair[_pair_key(Kit.get_int(e, "weapon", 0), Kit.get_int(e, "target", 0))] = e
	if reported.size() != _doctrine_rows.size():
		_build_doctrine_rows()
		_link_focus()
	var sent: Dictionary = _doctrine_sent.get(slot, {})
	for row: Dictionary in _doctrine_rows:
		var key := _pair_key(row["weapon"], row["target"])
		var design: Dictionary = by_pair.get(key, row["design"])
		row["design"] = design
		var e: Dictionary = sent.get(key, design)
		row["entry"] = e
		_show_doctrine_row(row, e, sent.has(key) or Kit.get_bool(design, "overridden"))


func _show_doctrine_row(row: Dictionary, e: Dictionary, overridden: bool) -> void:
	var stance: OptionButton = row["stance"]
	var s := Kit.get_int(e, "min_stance", STANCE_HOSTILE)
	# A stance below Suspect (inbound-weapon rows) is shown as it stands, never rewritten.
	if s < STANCE_SUSPECT:
		if stance.item_count < 3:
			stance.add_item("", 2)
		stance.set_item_text(2, tr(&"hud_control.doctrine.stance_other").format([_word(&"stance", s)]))
		stance.select(2)
	else:
		stance.select(clampi(s - STANCE_SUSPECT, 0, 1))
	(row["rung"] as OptionButton).select(clampi(Kit.get_int(e, "min_id", 0), 0, RUNGS - 1))
	(row["grade"] as OptionButton).select(clampi(Kit.get_int(e, "min_grade", 0), 0, GRADES - 1))
	var salvo: SpinBox = row["salvo"]
	# A field the player is typing in keeps what they typed.
	if not salvo.get_line_edit().has_focus():
		salvo.set_value_no_signal(clampi(Kit.get_int(e, "salvo", 1), 0, MAX_SALVO))
	var r := Kit.get_float(e, "effective_range_m", -1.0)
	var auto_range: CheckBox = row["auto"]
	auto_range.set_pressed_no_signal(r <= 0.0)
	var range_km: SpinBox = row["range"]
	range_km.editable = r > 0.0 and _max_layer >= 1
	if not range_km.get_line_edit().has_focus():
		range_km.set_value_no_signal(clampf(r / 1000.0, RANGE_MIN_KM, RANGE_MAX_KM) if r > 0.0 else RANGE_MIN_KM)
	range_km.visible = r > 0.0
	_set_chip(row["chip"], overridden)


func _send_doctrine(i: int) -> void:
	if i < 0 or i >= _doctrine_rows.size():
		return
	var row := _doctrine_rows[i]
	var e: Dictionary = (row["entry"] as Dictionary).duplicate()
	var stance: OptionButton = row["stance"]
	var sel := stance.get_selected_id()
	if sel == 2:
		e["min_stance"] = Kit.get_int(row["entry"], "min_stance", STANCE_SUSPECT)
	else:
		e["min_stance"] = STANCE_SUSPECT + clampi(sel, 0, 1)
	e["min_id"] = (row["rung"] as OptionButton).selected
	e["min_grade"] = (row["grade"] as OptionButton).selected
	e["salvo"] = int((row["salvo"] as SpinBox).value)
	var auto_range: CheckBox = row["auto"]
	if auto_range.button_pressed:
		e["effective_range_m"] = -1.0
	else:
		var km := (row["range"] as SpinBox).value
		if Kit.get_float(row["entry"], "effective_range_m", -1.0) <= 0.0:
			# Turning the default off starts from the range in force: the mount's effective range,
			# or for the ship's rows the longest of its mounts'.
			km = clampf(_range_in_force(_scope_slot()) / 1000.0, RANGE_MIN_KM, RANGE_MAX_KM)
		e["effective_range_m"] = km * 1000.0
	e.erase("overridden")
	var slot := _scope_slot()
	var sent: bool = Kit.call_on(&"Orders", &"set_doctrine", [slot, e]) == true
	_report(sent)
	if not sent:
		_update_doctrine()
		return
	var key := _pair_key(row["weapon"], row["target"])
	var table: Dictionary = _doctrine_sent.get(slot, {})
	table[key] = e
	_doctrine_sent[slot] = table
	row["entry"] = e
	_show_doctrine_row(row, e, true)
	# A doctrine row announces no override of its own: ask for the Gunnery list, which the revert uses.
	Kit.call_on(&"Orders", &"query_overrides", [OFFICER_GUNNERY])


## Effective range in force for a scope's rows, metres: the mount's, or the longest of the ship's.
func _range_in_force(slot: int) -> float:
	var best := 0.0
	for m: Dictionary in Kit.dicts(Kit.call_on(&"Hud", &"mounts")):
		if slot == NONE or Kit.get_int(m, "slot", NONE) == slot:
			best = maxf(best, Kit.get_float(m, "effective_range_m", 0.0))
	return best if best > 0.0 else 8000.0


## Put a doctrine row back to the design's values. The server files doctrine changes by their place
## in the stored list, which the client cannot see, so a row goes back by sending the design's row;
## once no row of the scope differs any more, the scope's stored changes are reverted by the ids the
## Gunnery officer's list gives, which leaves the design's list exactly.
func _revert_doctrine(weapon: int, target: int) -> void:
	var slot := _scope_slot()
	var key := _pair_key(weapon, target)
	for row: Dictionary in _doctrine_rows:
		if row["weapon"] != weapon or row["target"] != target:
			continue
		var design: Dictionary = (row["design"] as Dictionary).duplicate()
		design.erase("overridden")
		var table: Dictionary = _doctrine_sent.get(slot, {})
		table.erase(key)
		_doctrine_sent[slot] = table
		var sent := true
		if table.is_empty():
			sent = _revert_doctrine_scope(slot)
		if not sent or not table.is_empty():
			sent = Kit.call_on(&"Orders", &"set_doctrine", [slot, design]) == true
		_report(sent)
		row["entry"] = row["design"]
		_show_doctrine_row(row, row["design"], false)
		Kit.call_on(&"Orders", &"query_overrides", [OFFICER_GUNNERY])
		return


## Revert every stored doctrine change of one scope, highest place first so the places hold.
func _revert_doctrine_scope(slot: int) -> bool:
	var prefix := "doctrine/%s/" % ("ship" if slot == NONE else str(slot))
	var places: Array[int] = []
	for id: String in _override_ids.get(OFFICER_GUNNERY, PackedStringArray()):
		if id.begins_with(prefix) and id.trim_prefix(prefix).is_valid_int():
			places.append(int(id.trim_prefix(prefix)))
	if places.is_empty():
		return false
	places.sort()
	places.reverse()
	var ok := true
	for p: int in places:
		ok = Kit.call_on(&"Orders", &"revert", [prefix + str(p)]) == true and ok
	return ok


func _on_ship_preset(index: int) -> void:
	_hold("preset", index)
	_report(Kit.call_on(&"Orders", &"set_preset", [NONE, index]) == true)
	_update_doctrine()


func _on_mount_preset(index: int, slot: int) -> void:
	var sent := false
	if index == 0:
		var d := Kit.as_dict(Kit.call_on(&"Hud", &"preset", [slot]))
		if Kit.get_bool(d, "overridden"):
			sent = Kit.call_on(&"Orders", &"revert", [_override_id("mount_preset", slot)]) == true
		else:
			var ship := Kit.as_dict(Kit.call_on(&"Hud", &"preset", [NONE]))
			sent = Kit.call_on(&"Orders", &"set_preset", [slot, Kit.get_int(ship, "preset", 1)]) == true
	else:
		sent = Kit.call_on(&"Orders", &"set_preset", [slot, index - 1]) == true
	_hold("mount_preset/%d" % slot, index)
	_report(sent)
	_update_doctrine()


# --- tuning (L1) ---------------------------------------------------------------------------------

func _build_tuning() -> void:
	var page := _page_nodes[&"tuning"]
	_officer_tabs = TabBar.new()
	_officer_tabs.focus_mode = Control.FOCUS_ALL
	_officer_tabs.clip_tabs = true
	_officer_tabs.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	for o: int in OFFICERS:
		_officer_tabs.add_tab(_word(&"officer", o))
	_officer_tabs.current_tab = maxi(0, OFFICERS.find(_officer))
	_officer_tabs.tab_changed.connect(_on_officer_tab)
	page.add_child(_officer_tabs)
	_slider_box = Kit.vbox(2)
	page.add_child(_slider_box)
	_advanced_toggle = Button.new()
	_advanced_toggle.toggle_mode = true
	_advanced_toggle.theme_type_variation = &"FlatButton"
	_advanced_toggle.alignment = HORIZONTAL_ALIGNMENT_LEFT
	_advanced_toggle.focus_mode = Control.FOCUS_ALL
	_advanced_toggle.custom_minimum_size = Vector2(0.0, ROW_H)
	_advanced_toggle.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_advanced_toggle.toggled.connect(_on_advanced_toggled)
	page.add_child(_advanced_toggle)
	_advanced_box = Kit.vbox(2)
	_advanced_box.visible = false
	page.add_child(_advanced_box)
	_build_sliders()


func _on_officer_tab(i: int) -> void:
	_officer = OFFICERS[clampi(i, 0, OFFICERS.size() - 1)]
	_build_sliders()
	_update_tuning()
	_link_focus()


func _on_advanced_toggled(on: bool) -> void:
	_advanced_box.visible = on
	_advanced_toggle.icon = Ui.icon(&"chevron_up" if on else &"chevron_down")
	_link_focus()


func _build_sliders() -> void:
	Kit.clear(_slider_box)
	Kit.clear(_advanced_box)
	_slider_rows.clear()
	var sliders := Kit.dicts(Kit.call_on(&"Hud", &"sliders", [_officer]))
	var advanced := 0
	for s: Dictionary in sliders:
		var key := Kit.get_str(s, "key")
		if key.is_empty():
			continue
		var basic := Kit.get_bool(s, "basic", true)
		var box := Kit.vbox(0)
		var head := _row()
		var label_key := StringName(s.get("label", &""))
		var name := tr(label_key) if label_key != &"" else key
		head.add_child(_name_label(name))
		var value := Kit.label("", &"Body", true)
		value.autowrap_mode = TextServer.AUTOWRAP_OFF
		value.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		head.add_child(value)
		var chip := _chip(_override_id("slider", key))
		head.add_child(chip)
		box.add_child(head)
		var slider := preload("res://ui/widgets/focus_slider.gd").new()
		slider.min_value = Kit.get_float(s, "min", 0.0)
		slider.max_value = Kit.get_float(s, "max", 1.0)
		slider.step = maxf(1e-6, Kit.get_float(s, "step", 0.01))
		slider.custom_minimum_size = Vector2(0.0, 32.0)
		slider.focus_mode = Control.FOCUS_ALL
		slider.accessibility_name = name
		var tip := tr(StringName("hud_control.tip." + key))
		slider.tooltip_text = tip
		box.tooltip_text = tip
		slider.drag_started.connect(_on_slider_drag_started.bind(key))
		slider.drag_ended.connect(_on_slider_drag_ended.bind(key))
		slider.value_changed.connect(_on_slider_moved.bind(key))
		box.add_child(slider)
		# The tutorial frames a slider by this id (`hud.slider.gunnery.aggression`).
		box.set_meta(&"control_id", "hud.slider." + key)
		(_slider_box if basic else _advanced_box).add_child(box)
		if not basic:
			advanced += 1
		_slider_rows[key] = {"slider": slider, "value": value, "chip": chip, "def": s}
	_advanced_toggle.visible = advanced > 0
	_advanced_toggle.text = tr(&"hud_control.tuning.all").format([advanced])
	_advanced_toggle.icon = Ui.icon(&"chevron_up" if _advanced_toggle.button_pressed else &"chevron_down")
	if sliders.is_empty():
		_slider_box.add_child(Kit.label("hud_control.tuning.none", &"Dense"))


func _update_tuning() -> void:
	var sliders := Kit.dicts(Kit.call_on(&"Hud", &"sliders", [_officer]))
	if sliders.size() != _slider_rows.size():
		_build_sliders()
		_link_focus()
	for s: Dictionary in sliders:
		var key := Kit.get_str(s, "key")
		var row: Dictionary = _slider_rows.get(key, {})
		if row.is_empty():
			continue
		row["def"] = s
		var slider: HSlider = row["slider"]
		var v := float(_shown("slider/" + key, Kit.get_float(s, "value", 0.0)))
		if _dragging != key and not (_debounce_key == key and not _debounce.is_stopped()):
			slider.set_value_no_signal(v)
		(row["value"] as Label).text = _slider_text(s, slider.value)
		_set_chip(row["chip"], Kit.get_bool(s, "overridden"), _slider_text(s, Kit.get_float(s, "default", 0.0)))
		slider.accessibility_description = (row["value"] as Label).text


func _on_slider_drag_started(key: String) -> void:
	_dragging = key


func _on_slider_drag_ended(changed: bool, key: String) -> void:
	_dragging = ""
	if changed:
		_send_slider(key)


func _on_slider_moved(value: float, key: String) -> void:
	var row: Dictionary = _slider_rows.get(key, {})
	if row.is_empty():
		return
	(row["value"] as Label).text = _slider_text(row["def"], value)
	if _dragging == key:
		return
	# Keyboard and gamepad steps send once the player stops stepping.
	if _debounce_key != key and _debounce_key != "" and not _debounce.is_stopped():
		_flush_slider()
	_debounce_key = key
	_debounce.start()


func _flush_slider() -> void:
	var key := _debounce_key
	_debounce.stop()
	_debounce_key = ""
	if not key.is_empty():
		_send_slider(key)


func _send_slider(key: String) -> void:
	var row: Dictionary = _slider_rows.get(key, {})
	if row.is_empty():
		return
	var v := (row["slider"] as HSlider).value
	_hold("slider/" + key, v)
	_report(Kit.call_on(&"Orders", &"set_slider", [key, v]) == true)


## A slider value with its unit, read from the key and range: metres, seconds, a share as a
## percentage, else a plain number to the slider's step.
func _slider_text(s: Dictionary, value: float) -> String:
	var key := Kit.get_str(s, "key")
	var lo := Kit.get_float(s, "min", 0.0)
	var hi := Kit.get_float(s, "max", 1.0)
	var step := Kit.get_float(s, "step", 0.01)
	if key.ends_with("_m"):
		return _range(value) if hi >= 100.0 else _number(value, &"m", Kit.decimals_for(step))
	if key.ends_with("_s"):
		return _number(value, &"s", Kit.decimals_for(maxf(step, 0.1)) if hi < 20.0 else 0)
	if lo >= 0.0 and hi <= 1.0:
		return _number(value * 100.0, &"pct", 0)
	return _number(value, &"none", Kit.decimals_for(step) if step < 1.0 else 0)


# --- rules (L2) ----------------------------------------------------------------------------------

func _build_rules() -> void:
	var page := _page_nodes[&"rules"]
	page.add_child(Kit.label("hud_control.rules.intro", &"Dense"))
	_rules_box = Kit.vbox(2)
	page.add_child(_rules_box)
	var foot := _row()
	_rules_restore = Kit.inline_button("hud_control.rules.restore")
	_rules_restore.custom_minimum_size = Vector2(0.0, ROW_H)
	_rules_restore.tooltip_text = tr(&"hud_control.rules.restore_tip")
	_rules_restore.pressed.connect(_restore_rules)
	foot.add_child(_rules_restore)
	page.add_child(foot)
	_rules_note = Kit.label("hud_control.rules.note", &"Dense")
	page.add_child(_rules_note)


## The design's table with this tick's active marks.
func _design_rules() -> Array[Dictionary]:
	return Kit.dicts(Kit.call_on(&"Hud", &"rules"))


func _table() -> Array[Dictionary]:
	if _rules_changed:
		return _rules_sent
	var out: Array[Dictionary] = []
	var design := _design_rules()
	for i in design.size():
		var r := design[i].duplicate(true)
		r["orig"] = i
		out.append(r)
	return out


func _update_rules() -> void:
	var table := _table()
	var design := _design_rules()
	if _rules_box.get_child_count() != maxi(1, table.size()) or _rules_box.has_meta(&"stale"):
		_rules_box.remove_meta(&"stale")
		_build_rule_rows(table)
		_link_focus()
	if table.is_empty():
		_rules_restore.visible = _rules_changed and not design.is_empty()
		return
	for i in table.size():
		var r := table[i]
		var row := _rules_box.get_child(i)
		var check: CheckBox = row.get_node("Enabled")
		check.set_pressed_no_signal(Kit.get_bool(r, "enabled", true))
		var orig := Kit.get_int(r, "orig", NONE)
		var active := orig >= 0 and orig < design.size() and Kit.get_bool(design[orig], "active") and Kit.get_bool(r, "enabled", true)
		var status: Label = row.get_node("Text/Status")
		if not Kit.get_bool(r, "valid", true):
			status.text = tr(&"hud_control.rules.invalid")
		elif not Kit.get_bool(r, "enabled", true):
			status.text = tr(&"hud_control.rules.off")
		elif active:
			status.text = tr(&"hud_control.rules.active")
		else:
			status.text = tr(&"hud_control.rules.waiting")
	_rules_restore.visible = _rules_changed


func _build_rule_rows(table: Array[Dictionary]) -> void:
	Kit.clear(_rules_box)
	if table.is_empty():
		_rules_box.add_child(Kit.label("hud_control.rules.none", &"Body"))
		return
	for i in table.size():
		var r := table[i]
		var row := _row()
		row.name = "Rule%d" % i
		var check := CheckBox.new()
		check.name = "Enabled"
		check.focus_mode = Control.FOCUS_ALL
		check.custom_minimum_size = Vector2(ROW_H, ROW_H)
		check.size_flags_vertical = Control.SIZE_SHRINK_BEGIN
		var sentence := _Text.resolve(r.get("text", {}))
		check.accessibility_name = tr(&"hud_control.rules.enabled").format([i + 1, sentence])
		check.tooltip_text = tr(&"hud_control.rules.enabled_tip")
		check.toggled.connect(_on_rule_toggled.bind(i))
		row.add_child(check)
		var text := Kit.vbox(0)
		text.name = "Text"
		text.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		var number := tr(&"hud_control.rules.sentence").format([i + 1, sentence])
		var sentence_label := Kit.label(number, &"Body", true)
		sentence_label.name = "Sentence"
		text.add_child(sentence_label)
		var status := Kit.label("", &"Dense", true)
		status.name = "Status"
		text.add_child(status)
		row.add_child(text)
		var up := Kit.icon_button(&"arrow_up", tr(&"hud_control.rules.up"))
		up.name = "Up"
		up.custom_minimum_size = Vector2(ROW_H, ROW_H)
		up.size_flags_vertical = Control.SIZE_SHRINK_BEGIN
		up.disabled = i == 0
		up.pressed.connect(_move_rule.bind(i, i - 1))
		row.add_child(up)
		var down := Kit.icon_button(&"arrow_down", tr(&"hud_control.rules.down"))
		down.name = "Down"
		down.custom_minimum_size = Vector2(ROW_H, ROW_H)
		down.size_flags_vertical = Control.SIZE_SHRINK_BEGIN
		down.disabled = i == table.size() - 1
		down.pressed.connect(_move_rule.bind(i, i + 1))
		row.add_child(down)
		var remove := Kit.icon_button(&"delete", tr(&"hud_control.rules.remove"))
		remove.name = "Remove"
		remove.custom_minimum_size = Vector2(ROW_H, ROW_H)
		remove.size_flags_vertical = Control.SIZE_SHRINK_BEGIN
		remove.pressed.connect(_remove_rule.bind(i))
		row.add_child(remove)
		_rules_box.add_child(row)


func _on_rule_toggled(on: bool, i: int) -> void:
	var table := _table().duplicate(true)
	if i < 0 or i >= table.size():
		return
	table[i]["enabled"] = on
	_send_rules(table)


func _move_rule(from: int, to: int) -> void:
	var table := _table().duplicate(true)
	if from < 0 or from >= table.size() or to < 0 or to >= table.size():
		return
	var r: Dictionary = table[from]
	table.remove_at(from)
	table.insert(to, r)
	_send_rules(table)
	# Keep focus on the moved rule's same button.
	var row := _rules_box.get_node_or_null(NodePath("Rule%d" % to))
	if row != null:
		var b: Control = row.get_node(^"Up" if to < from else ^"Down")
		if b != null and not (b as BaseButton).disabled:
			b.grab_focus.call_deferred()


func _remove_rule(i: int) -> void:
	var table := _table().duplicate(true)
	if i < 0 or i >= table.size():
		return
	table.remove_at(i)
	_send_rules(table)


func _restore_rules() -> void:
	_rules_changed = false
	var design := _table()
	_rules_changed = true
	if _send_rules(design):
		_rules_changed = false
		_rules_sent.clear()
		_rules_box.set_meta(&"stale", true)
		_update_rules()


func _send_rules(table: Array) -> bool:
	if table.size() > MAX_RULES:
		table.resize(MAX_RULES)
	var issues := Kit.dicts(Kit.call_on(&"Orders", &"set_rules", [table]))
	if not issues.is_empty() or Kit.autoload(&"Orders") == null:
		var texts: PackedStringArray = []
		for issue: Dictionary in issues:
			texts.append(_Text.resolve(issue.get("text", {})))
		_report(false, tr(&"hud_control.status.rules_refused").format(["\n".join(texts)]) if not texts.is_empty() else "")
		_update_rules()
		return false
	_rules_sent.clear()
	for r: Variant in table:
		if r is Dictionary:
			_rules_sent.append(r)
	_rules_changed = true
	_rules_box.set_meta(&"stale", true)
	_report(true, tr(&"hud_control.status.rules_sent").format([table.size()]))
	_update_rules()
	return true


# --- emissions -----------------------------------------------------------------------------------

func _build_emissions() -> void:
	var page := _page_nodes[&"emissions"]
	var head := _row()
	head.add_child(_name_label(tr(&"hud_control.emcon.title")))
	_emcon_chip = _chip(_override_id("emcon"))
	head.add_child(_emcon_chip)
	page.add_child(head)
	var row := _row()
	var group := ButtonGroup.new()
	_emcon_buttons.clear()
	for e in EMCONS:
		var b := Button.new()
		b.toggle_mode = true
		b.button_group = group
		b.text = _word(&"emcon", e)
		b.focus_mode = Control.FOCUS_ALL
		b.custom_minimum_size = Vector2(120.0, ROW_H)
		b.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		b.tooltip_text = tr(StringName("hud_control.emcon.tip_%d" % e))
		b.pressed.connect(_on_emcon.bind(e))
		row.add_child(b)
		_emcon_buttons.append(b)
	page.add_child(row)
	_emcon_description = Kit.label("", &"Dense", true)
	page.add_child(_emcon_description)
	_emcon_recommended = Kit.label("", &"SeverityAdvisory", true)
	page.add_child(_emcon_recommended)
	_emcon_reason = Kit.label("", &"Dense", true)
	page.add_child(_emcon_reason)
	page.add_child(HSeparator.new())
	var trigger := _row()
	_trigger_label = _name_label("", &"Body")
	trigger.add_child(_trigger_label)
	_trigger_chip = _chip(_override_id("burst_trigger_range"))
	trigger.add_child(_trigger_chip)
	page.add_child(trigger)
	var defence := _row()
	_defence_label = _name_label("", &"Body")
	defence.add_child(_defence_label)
	_defence_chip = _chip(_override_id("self_defence_range"))
	defence.add_child(_defence_chip)
	page.add_child(defence)
	page.add_child(Kit.label("hud_control.emcon.ranges_note", &"Dense"))
	page.add_child(HSeparator.new())
	_radiating = Kit.label("", &"Body", true)
	page.add_child(_radiating)
	_esm = Kit.label("", &"SeverityCaution", true)
	page.add_child(_esm)


func _update_emissions() -> void:
	var d := Kit.as_dict(Kit.call_on(&"Hud", &"emcon"))
	var emcon := int(_shown("emcon", Kit.get_int(d, "emcon", 1)))
	for e in EMCONS:
		_emcon_buttons[e].set_pressed_no_signal(e == emcon)
	var recommended := Kit.get_int(d, "recommended", NONE)
	_set_chip(_emcon_chip, Kit.get_bool(d, "overridden"))
	_emcon_description.text = _emcon_text(emcon)
	_emcon_recommended.text = tr(&"hud_control.emcon.recommended").format([_word(&"emcon", recommended)]) if recommended != NONE else ""
	_emcon_recommended.visible = recommended != NONE
	var reason := _Text.resolve(d.get("reason", {}))
	_emcon_reason.text = reason
	_emcon_reason.visible = not reason.is_empty()
	var trigger := Kit.get_float(d, "burst_trigger_range_m", 20000.0)
	_trigger_label.text = tr(&"hud_control.emcon.trigger").format([_range(trigger)])
	var defence := Kit.get_float(d, "self_defence_range_m", 4000.0)
	_defence_label.text = tr(&"hud_control.emcon.self_defence").format([_range(defence)])
	var ids: PackedStringArray = _override_ids.get(OFFICER_SENSORS, PackedStringArray())
	_set_chip(_trigger_chip, ids.has(_override_id("burst_trigger_range")))
	var gun_ids: PackedStringArray = _override_ids.get(OFFICER_GUNNERY, PackedStringArray())
	_set_chip(_defence_chip, gun_ids.has(_override_id("self_defence_range")))
	var radiating: PackedInt32Array = d.get("radiating") if d.get("radiating") is PackedInt32Array else PackedInt32Array()
	if radiating.is_empty():
		_radiating.text = tr(&"hud_board.emitters.none")
	else:
		var names := _part_names()
		var list: PackedStringArray = []
		for slot: int in radiating:
			list.append(names.get(slot, tr(&"hud_board.emitters.part").format([slot + 1])))
		_radiating.text = tr(&"hud_board.emitters.radiating").format([", ".join(list)])
	var warn := Kit.get_bool(d, "esm_warning")
	_esm.text = tr(&"hud_board.esm_warning") if warn else ""
	_esm.visible = warn
	if not _override_ids.has(OFFICER_SENSORS):
		_override_ids[OFFICER_SENSORS] = PackedStringArray()
		Kit.call_on(&"Orders", &"query_overrides", [OFFICER_SENSORS])
		Kit.call_on(&"Orders", &"query_overrides", [OFFICER_GUNNERY])


## What a state does; Burst names its sweep length and repeat from the Sensors sliders in force.
func _emcon_text(emcon: int) -> String:
	var key := StringName("hud_control.emcon.describe_%d" % clampi(emcon, 0, EMCONS - 1))
	if emcon != 1:
		return tr(key)
	var rotations := 3.0
	var repeat_s := 30.0
	for sd: Dictionary in Kit.dicts(Kit.call_on(&"Hud", &"sliders", [OFFICER_SENSORS])):
		match Kit.get_str(sd, "key"):
			"sensors.burst_rotations": rotations = Kit.get_float(sd, "value", rotations)
			"sensors.burst_repeat_s": repeat_s = Kit.get_float(sd, "value", repeat_s)
	return tr(key).format([_number(rotations * ANTENNA_ROTATION_S, &"s", 1), _number(repeat_s, &"s", 0)])


func _on_emcon(e: int) -> void:
	_hold("emcon", e)
	_report(Kit.call_on(&"Orders", &"set_emcon", [e]) == true)
	_update_emissions()


# --- power ---------------------------------------------------------------------------------------

func _build_power() -> void:
	var page := _page_nodes[&"power"]
	_power_summary = Kit.label("", &"Body", true)
	page.add_child(_power_summary)
	page.add_child(Kit.label("hud_control.power.intro", &"Dense"))
	_power_box = Kit.vbox(1)
	page.add_child(_power_box)
	_power_rows.clear()
	var items: Array[String] = []
	for p in range(1, 6):
		items.append(tr(StringName("hud_control.power.priority_%d" % p)))
	var consumers: Array[Dictionary] = []
	for p: Dictionary in Kit.dicts(Kit.call_on(&"Hud", &"parts_static")):
		var part := StringName(p.get("part", &""))
		var def := Kit.as_dict(Kit.call_on(&"Content", &"part", [part]))
		if Kit.get_float(def, "power_kw", 0.0) > 0.0:
			p = p.duplicate()
			p["demand_kw"] = Kit.get_float(def, "power_kw", 0.0)
			consumers.append(p)
	if consumers.is_empty():
		_power_box.add_child(Kit.label("hud_control.power.none", &"Dense"))
		return
	for p: Dictionary in consumers:
		var slot := Kit.get_int(p, "slot", NONE)
		var row := _row()
		var name := _Text.resolve(p.get("name", {}))
		var label := _name_label(name, &"Dense")
		label.tooltip_text = tr(&"hud_control.power.demand").format([_number(Kit.get_float(p, "demand_kw", 0.0), &"kw", 0)])
		label.mouse_filter = Control.MOUSE_FILTER_PASS
		row.add_child(label)
		var state := Kit.label("", &"Dense", true)
		state.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		row.add_child(state)
		var o := _option(items, 180.0)
		o.accessibility_name = name
		o.item_selected.connect(_on_priority.bind(slot))
		row.add_child(o)
		var chip := _chip(_override_id("power_priority", slot))
		row.add_child(chip)
		_power_box.add_child(row)
		_power_rows.append({"slot": slot, "option": o, "chip": chip, "state": state})


func _update_power() -> void:
	var power := Kit.as_dict(Kit.call_on(&"Hud", &"power"))
	var shed := Kit.get_int(power, "shed_count", 0)
	var text := tr(&"hud_control.power.summary").format([_number(Kit.get_float(power, "generation_kw", 0.0), &"kw", 0), _number(Kit.get_float(power, "demand_kw", 0.0), &"kw", 0)])
	if shed > 0:
		text += " " + tr(&"hud_control.power.shed_count").format([shed])
	_power_summary.text = text
	var parts := Kit.dicts(Kit.call_on(&"Hud", &"parts"))
	var by_slot: Dictionary[int, Dictionary] = {}
	for p: Dictionary in parts:
		by_slot[Kit.get_int(p, "slot", NONE)] = p
	var ids: PackedStringArray = _override_ids.get(OFFICER_ENGINEER, PackedStringArray())
	for row: Dictionary in _power_rows:
		var slot: int = row["slot"]
		var p: Dictionary = by_slot.get(slot, {})
		var prio := int(_shown("power_priority/%d" % slot, Kit.get_int(p, "priority", 3)))
		(row["option"] as OptionButton).select(clampi(prio - 1, 0, 4))
		(row["state"] as Label).text = tr(&"hud_control.power.shed") if Kit.get_bool(p, "shed") else ""
		_set_chip(row["chip"], ids.has(_override_id("power_priority", slot)))
	if not _override_ids.has(OFFICER_ENGINEER):
		_override_ids[OFFICER_ENGINEER] = PackedStringArray()
		Kit.call_on(&"Orders", &"query_overrides", [OFFICER_ENGINEER])


func _on_priority(index: int, slot: int) -> void:
	var prio := clampi(index + 1, 1, 5)
	_hold("power_priority/%d" % slot, prio)
	var sent: bool = Kit.call_on(&"Orders", &"set_power_priority", [slot, prio]) == true
	_report(sent)
	if sent:
		var ids: PackedStringArray = _override_ids.get(OFFICER_ENGINEER, PackedStringArray())
		var id := _override_id("power_priority", slot)
		if not ids.has(id):
			ids.append(id)
		_override_ids[OFFICER_ENGINEER] = ids
	_update_power()


# --- override lists ------------------------------------------------------------------------------

func _on_overrides_reply(officer: int, values: Array) -> void:
	var ids: PackedStringArray = []
	for o: Dictionary in Kit.dicts(values):
		ids.append(Kit.get_str(o, "id"))
	_override_ids[officer] = ids
	if is_visible_in_tree():
		_update_current()


func _on_override_changed(id: String, overridden: bool) -> void:
	for officer: int in _override_ids:
		var ids: PackedStringArray = _override_ids[officer]
		var at := ids.find(id)
		if not overridden and at >= 0:
			ids.remove_at(at)
			_override_ids[officer] = ids
	if overridden:
		var officer := OFFICER_ENGINEER if id.begins_with("power_priority/") else (OFFICER_GUNNERY if id == "self_defence_range" or id.begins_with("doctrine/") else (OFFICER_SENSORS if id == "burst_trigger_range" or id == "emcon" else NONE))
		if officer != NONE:
			var ids: PackedStringArray = _override_ids.get(officer, PackedStringArray())
			if not ids.has(id):
				ids.append(id)
			_override_ids[officer] = ids
	if id.begins_with("doctrine/") and not overridden:
		# A revert of a doctrine row from the Bridge Board: the design's rows stand again.
		_doctrine_sent.clear()
	if is_visible_in_tree():
		_update_current()


# --- focus ---------------------------------------------------------------------------------------

## Explicit neighbours (L1738): the tab bar, then the open page's controls top to bottom.
func _link_focus() -> void:
	var list: Array = [_tabs]
	var page := _page_nodes[PAGES[_page]]
	for c: Node in page.find_children("*", "Control", true, false):
		var ctl := c as Control
		if ctl.focus_mode == Control.FOCUS_ALL and ctl.is_visible_in_tree():
			list.append(ctl)
	Kit.chain_vertical(list)
