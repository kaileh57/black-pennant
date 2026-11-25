extends VBoxContainer
## Control layers L0 to L1 and settings: presets, sliders, doctrine, priorities, limits, emission.
## DESIGN.md L322-336, L287-295. Unit: ui-editor-control.
## Reads and calls: Editor.preset, Editor.sliders, Editor.doctrine, Editor.power_priorities, Editor.mount_limits, Editor.emission_settings, Editor.layers, Editor.auto_set_status.
##
## The Officers tab of the dry dock's build dock, above the rules table. Top to bottom, most used first:
## - A layer line: the highest control layer the design uses against what this dock's ruleset allows
##   (L969-971), how many values are set by hand, and Return to stock (`Editor.reset_control`, confirmed).
## - Doctrine (L0): a scope picker (the whole ship or one mount), the preset for that scope as three
##   choices, and the engagement rows of that scope (L479-489: lowest stance, ID rung, track grade, salvo or
##   hold, range). A mount scope adds "same as the ship" to the presets and the mount's training and
##   elevation limits (auto-set by sweep, L205).
## - Tuning (L1): each officer's named sliders, the emission setting under Sensors and the two emission
##   ranges, with the rest of the parameters under an expander (L333).
## - Power priorities (L287-295) and the damage-control repair order, folded: they are auto-set and
##   seldom touched.
## Every automatic value carries the A chip; a value set by hand shows the hand glyph, and pressing it
## reverts that one value (L351). Storing a value equal to the automatic one drops the override, so the
## chip comes back by itself. A layer the ruleset caps is shown disabled with the reason, never hidden.
## The panel rebuilds only when the shape of what it shows changes (the design's mounts, consumers or the
## ruleset) and updates values in place otherwise, so a drag or a focused field is never torn down.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const Inspector := preload("res://ui/editor/part_inspector.gd")
const AUTO_CHIP := preload("res://ui/widgets/auto_chip.tscn")

## Officers in `Bp.OFFICER_*` order.
const OFFICERS: Array[int] = [Bp.OFFICER_HELM, Bp.OFFICER_ENGINEER, Bp.OFFICER_SENSORS, Bp.OFFICER_GUNNERY, Bp.OFFICER_DAMAGE_CONTROL]
const PRESETS: Array[int] = [Bp.PRESET_AGGRESSIVE, Bp.PRESET_CAUTIOUS, Bp.PRESET_SILENT_RUNNING]
const PRESET_NAMES: Array[String] = ["aggressive", "cautious", "silent_running"]
const EMCONS: Array[int] = [Bp.EMCON_SILENT, Bp.EMCON_BURST, Bp.EMCON_ACTIVE]
## `RepairPriority::DEFAULT_ORDER` is the `Bp.REPAIR_*` order (checked by a gdclient control test).
const REPAIR_DEFAULT: Array[int] = [Bp.REPAIR_MAGAZINE_FIRES, Bp.REPAIR_BREACHES_BELOW_WATERLINE, Bp.REPAIR_OTHER_FIRES,
	Bp.REPAIR_CONDUITS_TO_CRITICAL, Bp.REPAIR_OTHER_CONDUITS, Bp.REPAIR_PART_REPAIRS]
## Doctrine row stance floor: nothing auto-engages a Neutral or Friend track (L486).
const STANCE_FLOORS: Array[int] = [Bp.STANCE_SUSPECT, Bp.STANCE_HOSTILE]
const RUNGS: Array[int] = [Bp.RUNG_UNKNOWN, Bp.RUNG_DOMAIN, Bp.RUNG_TYPE, Bp.RUNG_CLASS, Bp.RUNG_EXACT]
const GRADES: Array[int] = [Bp.GRADE_BEARING, Bp.GRADE_COARSE, Bp.GRADE_FINE, Bp.GRADE_LOCK]
## Salvo sizes the doctrine row offers; 0 is the hold encoding (silent running).
## The simulation's cap (sim_systems::doctrine::MAX_SALVO).
const MAX_SALVO := 16
## A range switched on starts here (a gun's typical effective range at this scale).
const RANGE_START_M := 5000.0
const RANGE_MAX_M := 60000.0
const FIELD_H := 40.0
const FIELD_W := 180.0

var _layout := ""
var _refresh_pending := false
var _stale := false
## Scope of the doctrine section: `Bp.NONE` for the whole ship, else a mount's part slot.
var _scope := -1
## Mount slots of the design under edit, from the newest built design.
var _mounts: Array[int] = []
var _parts: Array[Dictionary] = []
var _status_text: Variant = &""

# Built controls, updated in place by `_apply`.
var _layer_line: Label
var _legal_line: Label
var _overrides_line: Label
var _reset_all: Button
var _status: Label
var _scope_pick: OptionButton
var _preset_buttons: Dictionary[int, Button] = {}
var _preset_same: Button
var _preset_chip: AutoChip
var _preset_description: Label
var _doctrine_rows: Array[Dictionary] = []
var _doctrine_wait: Label
var _reset_doctrine: Button
var _limit_fields: Dictionary[String, SpinBox] = {}
var _limit_chip: AutoChip
var _limit_reason: Label
var _slider_rows: Dictionary[String, Dictionary] = {}
var _emcon_buttons: Dictionary[int, Button] = {}
var _emcon_chip: AutoChip
var _burst_field: SpinBox
var _burst_chip: AutoChip
var _defence_field: SpinBox
var _defence_chip: AutoChip
var _solver_reason: Label
var _power_rows: Dictionary[int, Dictionary] = {}
var _power_reason: Label
var _repair_rows: Array[Dictionary] = []
var _repair_chip: AutoChip
## The slider being dragged (one undo group per drag).
var _dragging := ""
## Expanders the player opened, by title key (kept across rebuilds; all start folded).
var _folds_open: Dictionary[StringName, bool] = {}
## The mount list follows the design and its newest build, not control edits.
var _mounts_dirty := true


func _ready() -> void:
	Ui.set_separation(self, 3)
	size_flags_horizontal = Control.SIZE_EXPAND_FILL
	accessibility_name = tr(&"editor_control.name")
	var e := _editor()
	if e != null:
		for sig: StringName in [&"control_changed", &"undo_changed"]:
			if e.has_signal(sig):
				e.connect(sig, _queue_refresh)
		if e.has_signal(&"gauges_changed"):
			e.connect(&"gauges_changed", _on_design_moved)
		if e.has_signal(&"design_changed"):
			e.connect(&"design_changed", func(_rev: int) -> void: _queue_refresh())
	visibility_changed.connect(func() -> void:
		if _stale and is_visible_in_tree():
			_stale = false
			_refresh())
	_refresh()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_layout = ""
		_queue_refresh()


func _editor() -> Node:
	return Kit.autoload(&"Editor")


func _on_design_moved() -> void:
	_mounts_dirty = true
	_queue_refresh()


func _word(kind: StringName, value: int) -> String:
	var tx := Kit.autoload(&"Tx")
	return String(tx.call(&"word", kind, value)) if tx != null else str(value)


# --- refresh ----------------------------------------------------------------------------------------

func _queue_refresh() -> void:
	if _refresh_pending:
		return
	# A slider drag commits every step; the panel catches up when the drag ends.
	if not _dragging.is_empty():
		return
	if not is_visible_in_tree():
		_stale = true
		return
	_refresh_pending = true
	_refresh.call_deferred()


## Pull everything once, rebuild when the shape changed, then write the values.
func _refresh() -> void:
	_refresh_pending = false
	if not is_node_ready():
		return
	var e := _editor()
	var open := e != null and bool(e.call(&"is_open"))
	if not open:
		_mounts_dirty = true
	var data := _pull(e) if open else {}
	var key := _layout_key(open, data)
	if key != _layout:
		_layout = key
		_build(open, data)
	if open:
		_apply(data)


func _pull(e: Node) -> Dictionary:
	if _mounts_dirty:
		_parts = Kit.dicts(e.call(&"parts"))
		_mounts = _find_mounts(e)
		_mounts_dirty = false
	if _scope >= 0 and not _mounts.has(_scope):
		_scope = -1
	var limits := Kit.as_dict(e.call(&"limits"))
	var d := {
		"layers": Kit.as_dict(e.call(&"layers")),
		"max_layer": Kit.get_int(limits, "max_layer", Bp.LAYER_SCRIPT),
		"preset": Kit.as_dict(e.call(&"preset", -1)),
		"scope_preset": Kit.as_dict(e.call(&"preset", _scope)),
		"doctrine": Kit.dicts(e.call(&"doctrine", _scope)),
		"limits": Kit.as_dict(e.call(&"mount_limits", _scope)) if _scope >= 0 else {},
		"sliders": Kit.dicts(e.call(&"sliders", -1)),
		"emission": Kit.as_dict(e.call(&"emission_settings")),
		"power": Kit.dicts(e.call(&"power_priorities")),
		"repair": e.call(&"repair_order"),
		"repair_overridden": bool(e.call(&"repair_order_overridden")) if e.has_method(&"repair_order_overridden") else false,
		"autoset": Kit.dicts(e.call(&"auto_set_status")),
	}
	return d


## Weapon mounts: parts with doctrine rows of their own (guns, launchers, point defence).
func _find_mounts(e: Node) -> Array[int]:
	var out: Array[int] = []
	for p: Dictionary in _parts:
		var slot := Kit.get_int(p, "slot", -1)
		if slot >= 0 and Kit.as_array(e.call(&"doctrine", slot)).size() > 0:
			out.append(slot)
	return out


func _layout_key(open: bool, d: Dictionary) -> String:
	if not open:
		return "closed"
	var parts: PackedStringArray = ["open", str(d.max_layer), str(_scope), ",".join(PackedStringArray(_mounts.map(func(m: int) -> String: return str(m))))]
	for r: Dictionary in d.doctrine:
		parts.append("d%d:%d" % [Kit.get_int(r, "weapon"), Kit.get_int(r, "target")])
	parts.append("lim" if not (d.limits as Dictionary).is_empty() else "")
	for s: Dictionary in d.sliders:
		parts.append(Kit.get_str(s, "key"))
	for p: Dictionary in d.power:
		parts.append("p%d" % Kit.get_int(p, "slot"))
	for a: Dictionary in d.autoset:
		parts.append("%s%s" % [Kit.get_str(a, "value"), "a" if Kit.get_bool(a, "auto", true) else "m"])
	for p: Dictionary in _parts:
		parts.append(Kit.get_str(p, "label"))
	return "|".join(parts)


# --- building ---------------------------------------------------------------------------------------

func _build(open: bool, d: Dictionary) -> void:
	var keep := get_viewport().gui_get_focus_owner() if get_viewport() != null else null
	var keep_id := String(keep.get_meta(&"focus_key", "")) if keep != null and is_ancestor_of(keep) else ""
	Kit.clear(self)
	_doctrine_rows.clear()
	_slider_rows.clear()
	_power_rows.clear()
	_repair_rows.clear()
	_preset_buttons.clear()
	_emcon_buttons.clear()
	_limit_fields.clear()
	_preset_same = null
	_limit_chip = null
	_limit_reason = null
	_reset_doctrine = null
	_solver_reason = null
	_power_reason = null
	_burst_field = null
	_defence_field = null
	if not open:
		var closed := Kit.label("editor_control.closed", &"Body")
		add_child(closed)
		return
	_build_header()
	_build_doctrine(d)
	_build_tuning(d)
	_build_power(d)
	_build_repair()
	_chain_focus()
	if not keep_id.is_empty():
		for c: Node in find_children("*", "Control", true, false):
			if String(c.get_meta(&"focus_key", "")) == keep_id:
				Ui.focus(c as Control)
				break


func _build_header() -> void:
	_layer_line = Kit.label("", &"Body", true)
	add_child(_layer_line)
	_legal_line = Kit.label("", &"SeverityFail", true)
	add_child(_legal_line)
	var row := Kit.hbox(2)
	_overrides_line = Kit.label("", &"Body", true)
	_overrides_line.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	row.add_child(_overrides_line)
	_reset_all = Kit.inline_button("editor_control.reset_all")
	_reset_all.tooltip_text = tr(&"editor_control.reset_all_tip")
	_reset_all.pressed.connect(_confirm_reset_all)
	_reset_all.set_meta(&"focus_key", "reset_all")
	row.add_child(_reset_all)
	add_child(row)
	_status = Kit.label("", &"SeverityCaution", true)
	_status.visible = false
	add_child(_status)


func _section_title(key: StringName) -> Label:
	var l := Kit.label(String(key), &"PanelTitle")
	add_child(l)
	return l


# --- doctrine (L0) ----------------------------------------------------------------------------------

func _build_doctrine(d: Dictionary) -> void:
	_section_title(&"editor_control.doctrine.title")
	var scope_row := Kit.hbox(2)
	var scope_label := Kit.label("editor_control.doctrine.scope", &"Body")
	scope_label.autowrap_mode = TextServer.AUTOWRAP_OFF
	scope_row.add_child(scope_label)
	_scope_pick = OptionButton.new()
	_scope_pick.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_scope_pick.custom_minimum_size.y = FIELD_H
	_scope_pick.accessibility_name = tr(&"editor_control.doctrine.scope")
	_scope_pick.tooltip_text = tr(&"editor_control.doctrine.scope_tip")
	_scope_pick.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_scope_pick.add_item(tr(&"editor_control.doctrine.whole_ship"), 0)
	_scope_pick.set_item_metadata(0, -1)
	for m in _mounts:
		var idx := _scope_pick.item_count
		_scope_pick.add_item(Inspector.part_name(m, _parts), idx)
		_scope_pick.set_item_metadata(idx, m)
		if m == _scope:
			_scope_pick.select(idx)
	if _scope < 0:
		_scope_pick.select(0)
	_scope_pick.item_selected.connect(_on_scope_picked)
	_scope_pick.set_meta(&"focus_key", "scope")
	scope_row.add_child(_scope_pick)
	add_child(scope_row)

	# Presets: the most-used control of the tab, one row of equal buttons.
	var presets := HFlowContainer.new()
	presets.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	presets.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	var group := ButtonGroup.new()
	group.allow_unpress = false
	if _scope >= 0:
		_preset_same = _toggle(&"editor_control.preset.same", group)
		_preset_same.pressed.connect(_on_preset.bind(-1))
		_preset_same.set_meta(&"focus_key", "preset:same")
		presets.add_child(_preset_same)
	for i in PRESETS.size():
		var b := _toggle(StringName("tuning.preset." + PRESET_NAMES[i]), group)
		b.tooltip_text = tr(StringName("tuning.preset.%s.description" % PRESET_NAMES[i]))
		b.pressed.connect(_on_preset.bind(PRESETS[i]))
		b.set_meta(&"focus_key", "preset:%d" % PRESETS[i])
		_preset_buttons[PRESETS[i]] = b
		presets.add_child(b)
	add_child(presets)
	var desc_row := Kit.hbox(1)
	_preset_description = Kit.label("", &"Dense", true)
	_preset_description.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	desc_row.add_child(_preset_description)
	_preset_chip = _chip()
	_preset_chip.size_flags_vertical = Control.SIZE_SHRINK_BEGIN
	_preset_chip.revert_requested.connect(_on_preset_revert)
	desc_row.add_child(_preset_chip)
	add_child(desc_row)

	# Engagement rows: the preset sets them, so they sit folded until the player wants to tune one.
	var fold := _fold(&"editor_control.doctrine.rows")
	var rows_box := Kit.vbox(2)
	fold.add_child(rows_box)
	_doctrine_wait = Kit.label("editor_control.doctrine.no_weapons", &"Dense")
	rows_box.add_child(_doctrine_wait)
	var rows: Array[Dictionary] = d.doctrine
	for i in rows.size():
		_build_doctrine_row(rows_box, i, rows[i])
	_reset_doctrine = Kit.inline_button("editor_control.doctrine.reset_ship" if _scope < 0 else "editor_control.doctrine.reset_mount")
	_reset_doctrine.pressed.connect(_on_reset_doctrine)
	_reset_doctrine.set_meta(&"focus_key", "reset_doctrine")
	rows_box.add_child(_reset_doctrine)
	add_child(fold)

	if _scope >= 0 and not (d.limits as Dictionary).is_empty():
		_build_limits()


func _build_doctrine_row(parent: VBoxContainer, i: int, r: Dictionary) -> void:
	var box := Kit.vbox(1)
	var head := Kit.hbox(1)
	var title := Kit.label(tr(&"editor_control.doctrine.row").format([_word(&"weapon", Kit.get_int(r, "weapon")), _word(&"target", Kit.get_int(r, "target"))]), &"Body", true)
	title.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	head.add_child(title)
	var chip := _chip()
	chip.revert_requested.connect(_on_doctrine_revert.bind(i))
	head.add_child(chip)
	box.add_child(head)
	var grid := _grid()
	var stance := _option("editor_control.doctrine.stance", "editor_control.doctrine.stance_tip")
	for s in STANCE_FLOORS:
		stance.add_item(_word(&"stance", s), s)
	stance.item_selected.connect(func(idx: int) -> void: _on_doctrine_field(i, "min_stance", stance.get_item_id(idx)))
	_add_row(grid, &"editor_control.doctrine.stance", stance)
	var rung := _option("editor_control.doctrine.rung", "editor_control.doctrine.rung_tip")
	for v in RUNGS:
		rung.add_item(_word(&"rung", v), v)
	rung.item_selected.connect(func(idx: int) -> void: _on_doctrine_field(i, "min_id", rung.get_item_id(idx)))
	_add_row(grid, &"editor_control.doctrine.rung", rung)
	var grade := _option("editor_control.doctrine.grade", "editor_control.doctrine.grade_tip")
	for v in GRADES:
		grade.add_item(_word(&"grade", v), v)
	grade.item_selected.connect(func(idx: int) -> void: _on_doctrine_field(i, "min_grade", grade.get_item_id(idx)))
	_add_row(grid, &"editor_control.doctrine.grade", grade)
	var salvo := _option("editor_control.doctrine.salvo", "editor_control.doctrine.salvo_tip")
	salvo.add_item(tr(&"editor_control.doctrine.hold"), 0)
	for n in range(1, MAX_SALVO + 1):
		salvo.add_item(Kit.number(float(n), &"none", 0), n)
	salvo.item_selected.connect(func(idx: int) -> void: _on_doctrine_field(i, "salvo", salvo.get_item_id(idx)))
	_add_row(grid, &"editor_control.doctrine.salvo", salvo)
	var own := CheckBox.new()
	own.text = tr(&"editor_control.doctrine.own_range")
	own.tooltip_text = tr(&"editor_control.doctrine.own_range_tip")
	own.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	own.toggled.connect(func(on: bool) -> void: _on_doctrine_field(i, "effective_range_m", RANGE_START_M if on else -1.0))
	_add_row(grid, &"editor_control.doctrine.range", own)
	var range_field := _spin("editor_control.doctrine.range_value", "editor_control.doctrine.range_value_tip", &"m", 100.0, 100.0, RANGE_MAX_M)
	range_field.value_changed.connect(func(v: float) -> void: _on_doctrine_field(i, "effective_range_m", v))
	var range_label := _add_row(grid, &"editor_control.doctrine.range_value", range_field)
	box.add_child(grid)
	parent.add_child(box)
	for c: Control in [stance, rung, grade, salvo, own, range_field]:
		c.set_meta(&"focus_key", "doctrine:%d:%s" % [i, c.accessibility_name])
	_doctrine_rows.append({"chip": chip, "stance": stance, "rung": rung, "grade": grade, "salvo": salvo, "own": own,
		"range": range_field, "range_label": range_label, "data": r})


func _build_limits() -> void:
	var head := Kit.hbox(1)
	var t := Kit.label("editor_control.limits.title", &"Body")
	t.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	head.add_child(t)
	_limit_chip = _chip()
	_limit_chip.revert_requested.connect(_on_limits_revert)
	head.add_child(_limit_chip)
	add_child(head)
	_limit_reason = Kit.label("", &"SeverityAdvisory", true)
	add_child(_limit_reason)
	var grid := _grid()
	var specs: Array[Array] = [
		["train_min_deg", &"editor_control.limits.train_min", -180.0, 180.0],
		["train_max_deg", &"editor_control.limits.train_max", -180.0, 180.0],
		["elevation_min_deg", &"editor_control.limits.elevation_min", -90.0, 90.0],
		["elevation_max_deg", &"editor_control.limits.elevation_max", -90.0, 90.0],
	]
	for s: Array in specs:
		var key: String = s[0]
		var f := _spin(String(s[1]), "editor_control.limits.tip", &"deg", 1.0, float(s[2]), float(s[3]))
		f.value_changed.connect(func(v: float) -> void: _on_limit(key, v))
		f.set_meta(&"focus_key", "limit:" + key)
		_limit_fields[key] = f
		_add_row(grid, s[1], f)
	add_child(grid)


# --- tuning (L1) ------------------------------------------------------------------------------------

func _build_tuning(d: Dictionary) -> void:
	_section_title(&"editor_control.tuning.title")
	var max_layer: int = d.max_layer
	var allowed := max_layer >= Bp.LAYER_TUNE
	if not allowed:
		add_child(Kit.label(tr(&"editor_control.layer.capped").format([_word(&"layer", Bp.LAYER_TUNE)]), &"SeverityAdvisory", true))
	var sliders: Array[Dictionary] = d.sliders
	var more := _fold(&"editor_control.tuning.more")
	more.tooltip_text = tr(&"editor_control.tuning.more_tip")
	var more_box := Kit.vbox(2)
	more.add_child(more_box)
	for officer in OFFICERS:
		var basic: Array[Dictionary] = []
		var extra: Array[Dictionary] = []
		for s: Dictionary in sliders:
			if Kit.get_int(s, "officer", -1) != officer or StringName(s.get("label", &"")) == &"":
				continue
			if Kit.get_bool(s, "basic"):
				basic.append(s)
			else:
				extra.append(s)
		var group := Kit.vbox(1)
		group.add_child(Kit.label(_word(&"officer", officer), &"Body", true))
		for s: Dictionary in basic:
			group.add_child(_slider_row(s, allowed))
		if officer == Bp.OFFICER_SENSORS:
			_build_emcon(group, allowed)
		if officer == Bp.OFFICER_GUNNERY:
			_defence_field = _range_field(group, &"editor_control.emission.self_defence", &"editor_control.emission.self_defence_tip", allowed, "self_defence_range_m")
			_defence_chip = group.get_child(group.get_child_count() - 1).get_meta(&"chip") as AutoChip
			_solver_reason = Kit.label("", &"SeverityAdvisory", true)
			group.add_child(_solver_reason)
		add_child(group)
		if not extra.is_empty():
			var g2 := Kit.vbox(1)
			g2.add_child(Kit.label(_word(&"officer", officer), &"Body", true))
			for s: Dictionary in extra:
				g2.add_child(_slider_row(s, allowed))
			more_box.add_child(g2)
	if more_box.get_child_count() > 0:
		add_child(more)
	else:
		more.free()


func _slider_row(s: Dictionary, allowed: bool) -> Control:
	var key := Kit.get_str(s, "key")
	var box := Kit.vbox(0)
	var head := Kit.hbox(1)
	var name_label := Kit.label(String(s.get("label", &"")), &"Dense")
	name_label.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	head.add_child(name_label)
	var value := Kit.label("", &"Dense", true)
	value.autowrap_mode = TextServer.AUTOWRAP_OFF
	head.add_child(value)
	var chip := _chip()
	chip.revert_requested.connect(func() -> void: _on_slider_reset(key))
	head.add_child(chip)
	box.add_child(head)
	var slider := preload("res://ui/widgets/focus_slider.gd").new()
	slider.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	slider.custom_minimum_size.y = 24.0
	slider.min_value = Kit.get_float(s, "min")
	slider.max_value = maxf(Kit.get_float(s, "max", 1.0), slider.min_value)
	slider.step = maxf(Kit.get_float(s, "step", 0.01), 0.0001)
	slider.editable = allowed
	slider.accessibility_name = tr(StringName(s.get("label", &"")))
	slider.tooltip_text = _slider_tip(s)
	slider.set_meta(&"focus_key", "slider:" + key)
	# The tutorial frames a slider by this id (`editor.officers.slider.gunnery.aggression`, L259).
	box.set_meta(&"control_id", "editor.officers.slider." + key)
	slider.value_changed.connect(func(v: float) -> void: _on_slider(key, v))
	slider.drag_started.connect(func() -> void: _begin_drag(key))
	slider.drag_ended.connect(func(_changed: bool) -> void: _end_drag(key))
	box.add_child(slider)
	_slider_rows[key] = {"slider": slider, "value": value, "chip": chip, "data": s}
	return box


## The tooltip's first line says what the slider does (this panel's text, as the tuning table carries
## none), the second its range and automatic value.
func _slider_tip(s: Dictionary) -> String:
	var key := Kit.get_str(s, "key")
	var own := StringName("editor_control.slider_tip." + key)
	var tip := tr(own)
	if tip == String(own):
		var shared := StringName(s.get("tooltip", &""))
		tip = tr(shared) if shared != &"" and tr(shared) != String(shared) else ""
	var lo := Kit.get_float(s, "min")
	var hi := Kit.get_float(s, "max", 1.0)
	var span := tr(&"editor_control.slider.range").format([_slider_text(s, lo), _slider_text(s, hi)])
	return span if tip.is_empty() else tip + "\n" + span


## A slider value as text: a share of 0 to 1 as a percentage, metres and seconds with their units.
func _slider_text(s: Dictionary, v: float) -> String:
	var unit := StringName(s.get("unit", &"none"))
	var step := Kit.get_float(s, "step", 0.01)
	if unit == &"none" and Kit.get_float(s, "min") >= 0.0 and Kit.get_float(s, "max", 1.0) <= 1.0:
		return Kit.percent(v)
	return Kit.number(v, unit, Kit.decimals_for(step))


func _build_emcon(group: VBoxContainer, allowed: bool) -> void:
	var head := Kit.hbox(1)
	var t := Kit.label("editor_control.emission.emcon", &"Dense")
	t.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	head.add_child(t)
	_emcon_chip = _chip()
	_emcon_chip.revert_requested.connect(_on_emcon_revert)
	head.add_child(_emcon_chip)
	group.add_child(head)
	var row := HFlowContainer.new()
	row.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	row.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	var bg := ButtonGroup.new()
	bg.allow_unpress = false
	for e in EMCONS:
		var b := _toggle(StringName(), bg)
		b.text = _word(&"emcon", e)
		b.tooltip_text = tr(StringName("editor_control.emission.emcon_tip.%d" % e))
		b.disabled = not allowed
		b.pressed.connect(_on_emcon.bind(e))
		b.set_meta(&"focus_key", "emcon:%d" % e)
		_emcon_buttons[e] = b
		row.add_child(b)
	group.add_child(row)
	_burst_field = _range_field(group, &"editor_control.emission.burst", &"editor_control.emission.burst_tip", allowed, "burst_trigger_range_m")
	_burst_chip = group.get_child(group.get_child_count() - 1).get_meta(&"chip") as AutoChip


## A range setting with its chip, added to `group`; the row carries the chip as `chip` metadata.
func _range_field(group: VBoxContainer, label: StringName, tip: StringName, allowed: bool, key: String) -> SpinBox:
	var row := Kit.hbox(1)
	var l := Kit.label(String(label), &"Dense")
	l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	row.add_child(l)
	var f := _spin(String(label), String(tip), &"m", 100.0, 100.0, RANGE_MAX_M)
	f.editable = allowed
	f.value_changed.connect(func(v: float) -> void: _on_emission({key: v}))
	f.set_meta(&"focus_key", "emission:" + key)
	row.add_child(f)
	var chip := _chip()
	chip.revert_requested.connect(func() -> void: _on_emission({key: -1.0}))
	row.add_child(chip)
	row.set_meta(&"chip", chip)
	group.add_child(row)
	return f


# --- power priorities and repair order --------------------------------------------------------------

func _build_power(d: Dictionary) -> void:
	var rows: Array[Dictionary] = d.power
	var fold := _fold(&"editor_control.power.title")
	var box := Kit.vbox(1)
	fold.add_child(box)
	box.add_child(Kit.label("editor_control.power.explain", &"Dense"))
	_power_reason = Kit.label("", &"SeverityAdvisory", true)
	box.add_child(_power_reason)
	if rows.is_empty():
		box.add_child(Kit.label("editor_control.power.empty", &"Dense"))
	for p: Dictionary in rows:
		var slot := Kit.get_int(p, "slot", -1)
		var row := Kit.hbox(1)
		var n := Kit.label(_Text.resolve(p.get("name", {})), &"Dense", true)
		n.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		row.add_child(n)
		var demand := Kit.label(Kit.number(Kit.get_float(p, "demand_kw"), &"kw", 0), &"Dense", true)
		demand.autowrap_mode = TextServer.AUTOWRAP_OFF
		row.add_child(demand)
		var pick := OptionButton.new()
		pick.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		pick.custom_minimum_size.y = FIELD_H
		for level in range(1, 6):
			pick.add_item(tr(StringName("editor_control.power.level.%d" % level)), level)
		pick.accessibility_name = tr(&"editor_control.power.pick").format([n.text])
		pick.tooltip_text = tr(&"editor_control.power.pick_tip")
		pick.item_selected.connect(func(idx: int) -> void: _on_power(slot, pick.get_item_id(idx)))
		pick.set_meta(&"focus_key", "power:%d" % slot)
		row.add_child(pick)
		var chip := _chip()
		chip.revert_requested.connect(func() -> void: _on_power(slot, -1))
		row.add_child(chip)
		box.add_child(row)
		_power_rows[slot] = {"pick": pick, "chip": chip}
	add_child(fold)


func _build_repair() -> void:
	var fold := _fold(&"editor_control.repair.title")
	var box := Kit.vbox(1)
	fold.add_child(box)
	var head := Kit.hbox(1)
	var explain := Kit.label("editor_control.repair.explain", &"Dense")
	explain.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	head.add_child(explain)
	_repair_chip = _chip()
	_repair_chip.revert_requested.connect(func() -> void: _set_repair(REPAIR_DEFAULT))
	head.add_child(_repair_chip)
	box.add_child(head)
	for i in REPAIR_DEFAULT.size():
		var row := Kit.hbox(1)
		var l := Kit.label("", &"Dense", true)
		l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		row.add_child(l)
		var up := Kit.icon_button(&"arrow_up", tr(&"editor_control.repair.up"))
		up.pressed.connect(_move_repair.bind(i, -1))
		up.set_meta(&"focus_key", "repair_up:%d" % i)
		row.add_child(up)
		var down := Kit.icon_button(&"arrow_down", tr(&"editor_control.repair.down"))
		down.pressed.connect(_move_repair.bind(i, 1))
		down.set_meta(&"focus_key", "repair_down:%d" % i)
		row.add_child(down)
		box.add_child(row)
		_repair_rows.append({"label": l, "up": up, "down": down})
	add_child(fold)


# --- values -----------------------------------------------------------------------------------------

func _apply(d: Dictionary) -> void:
	var layers: Dictionary = d.layers
	var in_use := Kit.get_int(layers, "in_use")
	var max_layer: int = d.max_layer
	_layer_line.text = tr(&"editor_control.layer.summary").format([_word(&"layer", in_use), _word(&"layer", max_layer)])
	var legal := Kit.get_bool(layers, "legal", true)
	_legal_line.visible = not legal
	if not legal:
		_legal_line.text = tr(&"editor_control.layer.illegal").format([_word(&"layer", in_use), _word(&"layer", max_layer)])
	var overrides := Kit.get_int(layers, "overrides")
	if overrides == 0:
		_overrides_line.text = tr(&"editor_control.overrides.none")
	elif overrides == 1:
		_overrides_line.text = tr(&"editor_control.overrides.one")
	else:
		_overrides_line.text = tr(&"editor_control.overrides.count").format([Kit.number(float(overrides), &"none", 0)])
	var rules_count := Kit.as_array(_editor().call(&"rules")).size()
	var stock_preset := Kit.get_int(Kit.as_dict(d.preset), "preset", Bp.PRESET_CAUTIOUS) == Bp.PRESET_CAUTIOUS
	_reset_all.disabled = overrides == 0 and rules_count == 0 and stock_preset
	_status.visible = not _Text.is_empty(_status_text)
	_status.text = _Text.resolve(_status_text)
	_apply_doctrine(d)
	_apply_tuning(d)
	_apply_power(d)
	_apply_repair(d)
	_chain_focus()


func _apply_doctrine(d: Dictionary) -> void:
	var ship: Dictionary = d.preset
	var scoped: Dictionary = d.scope_preset
	var ship_preset := Kit.get_int(ship, "preset", Bp.PRESET_CAUTIOUS)
	var preset := Kit.get_int(scoped, "preset", ship_preset)
	var own := Kit.get_bool(scoped, "overridden")
	var same := _scope >= 0 and not own
	if _preset_same != null:
		_preset_same.set_pressed_no_signal(same)
		_preset_same.text = tr(&"editor_control.preset.same").format([_word(&"preset", ship_preset)])
	for p: int in _preset_buttons:
		_preset_buttons[p].set_pressed_no_signal(not same and p == preset)
	_preset_chip.overridden = own
	_preset_chip.default_text = _word(&"preset", Bp.PRESET_CAUTIOUS) if _scope < 0 else tr(&"editor_control.preset.same_short")
	var pi := PRESETS.find(preset)
	var desc := tr(StringName("tuning.preset.%s.description" % PRESET_NAMES[pi])) if pi >= 0 else ""
	_preset_description.text = tr(&"editor_control.preset.inherits").format([desc]) if same else desc
	var rows: Array[Dictionary] = d.doctrine
	_doctrine_wait.visible = rows.is_empty()
	var any_own := false
	for i in mini(rows.size(), _doctrine_rows.size()):
		var r: Dictionary = rows[i]
		var w: Dictionary = _doctrine_rows[i]
		w["data"] = r
		var over := Kit.get_bool(r, "overridden")
		any_own = any_own or over
		var chip := w["chip"] as AutoChip
		chip.overridden = over
		chip.revertible = r.has("default")
		_select_id(w["stance"], Kit.get_int(r, "min_stance", Bp.STANCE_HOSTILE))
		_select_id(w["rung"], Kit.get_int(r, "min_id"))
		_select_id(w["grade"], Kit.get_int(r, "min_grade"))
		_select_id(w["salvo"], Kit.get_int(r, "salvo", 1))
		var rng := Kit.get_float(r, "effective_range_m", -1.0)
		(w["own"] as CheckBox).set_pressed_no_signal(rng > 0.0)
		var field := w["range"] as SpinBox
		field.visible = rng > 0.0
		(w["range_label"] as Control).visible = rng > 0.0
		_set_spin(field, rng if rng > 0.0 else RANGE_START_M)
	if _reset_doctrine != null:
		_reset_doctrine.visible = any_own
	if not _limit_fields.is_empty():
		var lim: Dictionary = d.limits
		for k: String in _limit_fields:
			_set_spin(_limit_fields[k], Kit.get_float(lim, k))
		_limit_chip.overridden = not Kit.get_bool(lim, "auto", true)
		_set_reason(_limit_reason, d.autoset, "mount_limits")


func _apply_tuning(d: Dictionary) -> void:
	var sliders: Array[Dictionary] = d.sliders
	for s: Dictionary in sliders:
		var key := Kit.get_str(s, "key")
		if not _slider_rows.has(key):
			continue
		var w: Dictionary = _slider_rows[key]
		w["data"] = s
		var slider := w["slider"] as HSlider
		var v := Kit.get_float(s, "value")
		if _dragging != key:
			slider.set_value_no_signal(v)
		(w["value"] as Label).text = _slider_text(s, v)
		var chip := w["chip"] as AutoChip
		chip.overridden = Kit.get_bool(s, "overridden")
		chip.default_text = _slider_text(s, Kit.get_float(s, "default"))
	var em: Dictionary = d.emission
	var emcon := Kit.get_int(em, "emcon", Bp.EMCON_BURST)
	for e: int in _emcon_buttons:
		_emcon_buttons[e].set_pressed_no_signal(e == emcon)
	if _emcon_chip != null:
		_emcon_chip.overridden = not Kit.get_bool(em, "emcon_auto", true)
		_emcon_chip.default_text = _word(&"emcon", Kit.get_int(em, "emcon_default", Bp.EMCON_BURST))
	if _burst_field != null:
		_set_spin(_burst_field, Kit.get_float(em, "burst_trigger_range_m"))
		_burst_chip.overridden = not Kit.get_bool(em, "burst_auto", true)
	if _defence_field != null:
		_set_spin(_defence_field, Kit.get_float(em, "self_defence_range_m"))
		_defence_chip.overridden = not Kit.get_bool(em, "self_defence_auto", true)
	if _solver_reason != null:
		_set_reason(_solver_reason, d.autoset, "solver_tables")


func _apply_power(d: Dictionary) -> void:
	for p: Dictionary in (d.power as Array[Dictionary]):
		var slot := Kit.get_int(p, "slot", -1)
		if not _power_rows.has(slot):
			continue
		var w: Dictionary = _power_rows[slot]
		_select_id(w["pick"], Kit.get_int(p, "priority", 5))
		var chip := w["chip"] as AutoChip
		chip.overridden = Kit.get_bool(p, "overridden")
		chip.default_text = Kit.number(float(Kit.get_int(p, "default")), &"none", 0)
	if _power_reason != null:
		_set_reason(_power_reason, d.autoset, "power_priorities")


func _apply_repair(d: Dictionary) -> void:
	var order := _repair_order(d.repair)
	for i in mini(order.size(), _repair_rows.size()):
		var w: Dictionary = _repair_rows[i]
		(w["label"] as Label).text = tr(&"editor_control.repair.row").format([Kit.number(float(i + 1), &"none", 0), _word(&"repair", order[i])])
		(w["up"] as Button).disabled = i == 0
		(w["down"] as Button).disabled = i == order.size() - 1
	_repair_chip.overridden = bool(d.repair_overridden) or order != REPAIR_DEFAULT


func _repair_order(v: Variant) -> Array[int]:
	var out: Array[int] = []
	if v is PackedInt32Array:
		for x: int in (v as PackedInt32Array):
			out.append(x)
	elif v is Array:
		for x: Variant in v:
			out.append(int(x))
	return out if out.size() == REPAIR_DEFAULT.size() else REPAIR_DEFAULT.duplicate()


## Show why an auto-set value is set by hand this release (it dropped to Suggest, L200-212); hidden while
## it is automatic.
func _set_reason(label: Label, autoset: Array[Dictionary], value: String) -> void:
	if label == null:
		return
	label.visible = false
	for a: Dictionary in autoset:
		if Kit.get_str(a, "value") == value and not Kit.get_bool(a, "auto", true):
			label.text = _Text.resolve(a.get("reason", {}))
			label.visible = not label.text.is_empty()


# --- actions ----------------------------------------------------------------------------------------

func _say(msg: Variant) -> void:
	_status_text = msg
	if _status != null:
		_status.text = _Text.resolve(msg)
		_status.visible = not _status.text.is_empty()


## Run one edit; a refusal says so on the status line, success clears it.
func _did(ok: bool, refusal: StringName = &"editor_control.refused") -> void:
	_say(&"" if ok else refusal)
	_queue_refresh()


func _on_scope_picked(idx: int) -> void:
	_scope = int(_scope_pick.get_item_metadata(idx))
	_layout = ""
	_refresh()
	Ui.focus(_scope_pick)


func _on_preset(preset: int) -> void:
	_did(bool(_editor().call(&"set_preset", _scope, preset)))


func _on_preset_revert() -> void:
	_did(bool(_editor().call(&"set_preset", _scope, Bp.PRESET_CAUTIOUS if _scope < 0 else -1)))


func _on_doctrine_field(i: int, key: String, value: Variant) -> void:
	if i >= _doctrine_rows.size():
		return
	var entry: Dictionary = (_doctrine_rows[i]["data"] as Dictionary).duplicate()
	entry.erase("default")
	entry.erase("overridden")
	entry[key] = value
	_did(bool(_editor().call(&"set_doctrine", _scope, entry)), &"editor_control.doctrine.refused")


func _on_doctrine_revert(i: int) -> void:
	if i >= _doctrine_rows.size():
		return
	var r: Dictionary = _doctrine_rows[i]["data"]
	if not r.has("default"):
		return
	_did(bool(_editor().call(&"set_doctrine", _scope, r["default"])), &"editor_control.doctrine.refused")


func _on_reset_doctrine() -> void:
	_editor().call(&"reset_doctrine", _scope)
	_did(true)


func _on_limit(key: String, v: float) -> void:
	_did(bool(_editor().call(&"set_mount_limits", _scope, {key: v})), &"editor_control.limits.refused")


func _on_limits_revert() -> void:
	_did(bool(_editor().call(&"set_mount_limits", _scope, {})))


func _begin_drag(key: String) -> void:
	_dragging = key
	_editor().call(&"begin_group", &"dock.undo.slider")


func _end_drag(key: String) -> void:
	if _dragging != key:
		return
	_dragging = ""
	_editor().call(&"end_group", false)
	_queue_refresh()


func _on_slider(key: String, v: float) -> void:
	var applied := float(_editor().call(&"set_slider", key, v))
	if _slider_rows.has(key):
		var w: Dictionary = _slider_rows[key]
		(w["value"] as Label).text = _slider_text(w["data"], applied)
		if _dragging != key:
			(w["slider"] as HSlider).set_value_no_signal(applied)
	_say(&"")
	if _dragging != key:
		_queue_refresh()


func _on_slider_reset(key: String) -> void:
	_editor().call(&"reset_slider", key)
	_did(true)


func _on_emcon(e: int) -> void:
	_on_emission({"emcon": e})


func _on_emcon_revert() -> void:
	var em := Kit.as_dict(_editor().call(&"emission_settings"))
	_on_emission({"emcon": Kit.get_int(em, "emcon_default", Bp.EMCON_BURST)})


func _on_emission(settings: Dictionary) -> void:
	_did(bool(_editor().call(&"set_emission_settings", settings)))


func _on_power(slot: int, priority: int) -> void:
	_did(bool(_editor().call(&"set_power_priority", slot, priority)))


func _move_repair(i: int, delta: int) -> void:
	var order := _repair_order(_editor().call(&"repair_order"))
	var j := i + delta
	if j < 0 or j >= order.size():
		return
	var t := order[i]
	order[i] = order[j]
	order[j] = t
	_set_repair(order)
	# Keep the focus on the moved row's button so repeated presses keep moving it.
	var w: Dictionary = _repair_rows[clampi(j, 0, _repair_rows.size() - 1)]
	Ui.focus(w["up"] if delta < 0 and j > 0 else w["down"] if j < order.size() - 1 else w["up"])


func _set_repair(order: Array[int]) -> void:
	_did(bool(_editor().call(&"set_repair_order", PackedInt32Array(order))))


func _confirm_reset_all() -> void:
	var dialog: Node = Screens.open_dialog(&"editor_control.reset_all_title", &"editor_control.reset_all_body", [
		{"id": &"keep", "label": &"editor_control.reset_all_keep"},
		{"id": &"reset", "label": &"editor_control.reset_all_confirm"},
	], &"keep", &"keep")
	if dialog != null:
		dialog.connect(&"chosen", func(choice: StringName) -> void:
			if choice == &"reset":
				_editor().call(&"reset_control")
				_did(true)
			Ui.focus(_reset_all))


# --- small builders ---------------------------------------------------------------------------------

## A folded section whose open state survives a rebuild.
func _fold(title_key: StringName) -> FoldableContainer:
	var fold := FoldableContainer.new()
	# The title is a key the engine translates; children inherit translation, so it stays on.
	fold.title = String(title_key)
	fold.folded = not _folds_open.get(title_key, false)
	fold.set_meta(&"focus_key", "fold:" + String(title_key))
	fold.folding_changed.connect(func(folded: bool) -> void:
		_folds_open[title_key] = not folded
		_chain_focus.call_deferred())
	return fold


func _chip() -> AutoChip:
	return AUTO_CHIP.instantiate() as AutoChip


func _toggle(key: StringName, group: ButtonGroup) -> Button:
	var b := Button.new()
	b.toggle_mode = true
	b.button_group = group
	b.text = String(key)
	b.custom_minimum_size = Vector2(0.0, FIELD_H)
	b.focus_mode = Control.FOCUS_ALL
	return b


func _grid() -> GridContainer:
	var g := GridContainer.new()
	g.columns = 2
	g.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	g.add_theme_constant_override(&"h_separation", Ui.spacing(2))
	g.add_theme_constant_override(&"v_separation", Ui.spacing(0))
	return g


## A labelled row in a two-column grid; returns the label.
func _add_row(grid: GridContainer, label_key: StringName, field: Control) -> Label:
	var l := Kit.label(String(label_key), &"Dense")
	l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	grid.add_child(l)
	field.size_flags_horizontal = Control.SIZE_SHRINK_END
	grid.add_child(field)
	return l


func _option(name_key: String, tip_key: String) -> OptionButton:
	var o := OptionButton.new()
	o.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	o.custom_minimum_size = Vector2(FIELD_W, FIELD_H)
	o.accessibility_name = tr(StringName(name_key))
	o.tooltip_text = tr(StringName(tip_key))
	o.fit_to_longest_item = true
	return o


func _spin(name_key: String, tip_key: String, unit: StringName, step: float, lo: float, hi: float) -> SpinBox:
	var f := SpinBox.new()
	f.step = step
	f.min_value = lo
	f.max_value = hi
	f.custom_minimum_size = Vector2(FIELD_W, FIELD_H)
	f.select_all_on_focus = true
	f.update_on_text_changed = false
	f.suffix = tr(StringName("editor_control.unit." + String(unit))) if unit != &"none" else ""
	f.tooltip_text = tr(StringName(tip_key))
	f.get_line_edit().accessibility_name = tr(StringName(name_key))
	return f


func _set_spin(f: SpinBox, v: float) -> void:
	if f.get_line_edit().has_focus():
		return
	f.set_value_no_signal(v)


static func _select_id(o: OptionButton, id: int) -> void:
	var idx := o.get_item_index(id)
	if idx >= 0 and o.selected != idx:
		o.select(idx)


## Up and Down walk every control top to bottom (L1738); hidden and disabled ones are skipped.
func _chain_focus() -> void:
	var list: Array = []
	for c: Node in find_children("*", "Control", true, false):
		var ctl := c as Control
		if ctl.focus_mode == Control.FOCUS_NONE or not ctl.is_visible_in_tree():
			continue
		if ctl is LineEdit and ctl.get_parent() is SpinBox and (ctl.get_parent() as Control).focus_mode != Control.FOCUS_NONE:
			continue
		list.append(ctl)
	var live := Kit.chain_vertical(list)
	# The ends hand Up and Down on to the tab bar above and the rules table below.
	if not live.is_empty():
		live[0].focus_neighbor_top = NodePath()
		live[0].focus_previous = NodePath()
		live[-1].focus_neighbor_bottom = NodePath()
		live[-1].focus_next = NodePath()


func default_focus() -> Control:
	for b: Button in _preset_buttons.values():
		if b.button_pressed:
			return b
	return _scope_pick
