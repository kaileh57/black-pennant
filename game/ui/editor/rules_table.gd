extends VBoxContainer
## L2 rules table and rule builder.
## DESIGN.md L336. Unit: ui-editor-control.
## Reads and calls: Editor.rules, Editor.add_rule, Editor.set_rule, Editor.move_rule, Editor.rule_vocabulary.
##
## Under the control panel in the Officers tab. The table lists the design's rules top to bottom, each as
## the sentence the model writes ("When range to nearest Hostile contact is below 4 km, set aim point of
## Gun 1 to the nearest Hostile contact"), with a switch, up and down, edit and delete. Rules run every
## graph tick (30 Hz) from the top; when two matching rules set the same thing, the higher one wins, so
## order is the one thing the table must make easy. A ship holds at most 32 rules (`max_rules`).
##
## The builder opens inline under the table for a new rule or an edit: one or more comparisons ("when"),
## joined by all or any, then one thing to set ("then") and its value. Its pickers come from
## `Editor.rule_vocabulary()`, so it offers only operands and sinks this design has: the sink is chosen by
## kind first and then by part. A stored condition the builder cannot show as a flat list (a nested or
## negated one, from a file) is kept as it is unless the player replaces it. Percent operands are entered
## in percent and stored as fractions, as the model reads them. The model checks every rule; its problems
## show in the builder and nothing is stored until they are fixed.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const Inspector := preload("res://ui/editor/part_inspector.gd")

const CMPS: Array[int] = [Bp.CMP_LT, Bp.CMP_LE, Bp.CMP_GT, Bp.CMP_GE, Bp.CMP_EQ, Bp.CMP_NE]
const CMP_NAMES: Array[String] = ["lt", "le", "gt", "ge", "eq", "ne"]
## Contact filters: lowest stance, domain (`Bp.NONE` any) and lowest track grade.
const STANCES: Array[int] = [Bp.STANCE_FRIEND, Bp.STANCE_NEUTRAL, Bp.STANCE_SUSPECT, Bp.STANCE_HOSTILE]
const DOMAINS: Array[int] = [-1, Bp.DOMAIN_SURFACE, Bp.DOMAIN_SUBSURFACE, Bp.DOMAIN_AIR, Bp.DOMAIN_WEAPON]
const GRADES: Array[int] = [Bp.GRADE_BEARING, Bp.GRADE_COARSE, Bp.GRADE_FINE, Bp.GRADE_LOCK]
const RUNGS: Array[int] = [Bp.RUNG_UNKNOWN, Bp.RUNG_DOMAIN, Bp.RUNG_TYPE, Bp.RUNG_CLASS, Bp.RUNG_EXACT]
const PRESETS: Array[int] = [Bp.PRESET_AGGRESSIVE, Bp.PRESET_CAUTIOUS, Bp.PRESET_SILENT_RUNNING]
const EMCONS: Array[int] = [Bp.EMCON_SILENT, Bp.EMCON_BURST, Bp.EMCON_ACTIVE]
## Sink kinds in the order the "then" picker lists them.
const SINK_ORDER: Array[String] = ["mount_aim_point", "mount_fire_permission", "missile_target", "missile_launch",
	"helm_heading", "helm_speed", "ship_preset", "ship_emcon", "emission_state", "track_stance_override",
	"track_id_override", "power_priority", "pump", "door", "flood_valve"]
## Most comparisons in one rule's condition (the model's own cap is higher).
const MAX_ITEMS := 8
## Fields in percent, kilometres-free: ranges stay in metres as the rest of the dock shows them.
const FIELD_H := 40.0
const FIELD_W := 170.0
## The new-rule draft follows the spec's example: a contact closing inside 4 km.
const DRAFT_RANGE_M := 4000.0

var _vocab: Dictionary = {}
var _rules: Array[Dictionary] = []
var _max_layer := Bp.LAYER_SCRIPT
var _refresh_pending := false
var _stale := false
var _parts: Array[Dictionary] = []

## The rule being built: `index` (`Bp.NONE` = new), `enabled`, `join` (&"all" or &"any"), `items`
## (Array of compare ConditionDicts), `kept` (a ConditionDict the builder cannot show, or null), `sink`
## (SinkDict), `value` (ValueDict). Empty when the builder is closed.
var _draft: Dictionary = {}
var _issues: Array = []

var _title: Label
var _count: Label
var _capped: Label
var _list: VBoxContainer
var _empty: Label
var _add: Button
var _full: Label
var _builder: VBoxContainer
var _focus_key := ""
## What the rows and the builder were last built from, so a refresh that changes nothing keeps them.
var _rows_key := ""
var _vocab_key := ""


func _ready() -> void:
	Ui.set_separation(self, 2)
	size_flags_horizontal = Control.SIZE_EXPAND_FILL
	accessibility_name = tr(&"editor_rules.name")
	_title = Kit.label("editor_rules.title", &"PanelTitle")
	add_child(_title)
	_count = Kit.label("", &"Body", true)
	add_child(_count)
	_capped = Kit.label("", &"SeverityAdvisory", true)
	add_child(_capped)
	add_child(Kit.label("editor_rules.explain", &"Dense"))
	_empty = Kit.label("editor_rules.empty", &"Body")
	add_child(_empty)
	_list = Kit.vbox(2)
	add_child(_list)
	var row := Kit.hbox(2)
	_add = Kit.inline_button("editor_rules.add")
	_add.tooltip_text = tr(&"editor_rules.add_tip")
	_add.pressed.connect(_on_add)
	_add.set_meta(&"focus_key", "add")
	row.add_child(_add)
	_full = Kit.label("", &"Dense", true)
	_full.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	row.add_child(_full)
	add_child(row)
	_builder = Kit.vbox(2)
	add_child(_builder)
	var e := _editor()
	if e != null:
		for sig: StringName in [&"control_changed", &"undo_changed", &"gauges_changed"]:
			if e.has_signal(sig):
				e.connect(sig, _queue_refresh)
		if e.has_signal(&"design_changed"):
			e.connect(&"design_changed", func(_rev: int) -> void: _queue_refresh())
	visibility_changed.connect(func() -> void:
		if _stale and is_visible_in_tree():
			_stale = false
			_refresh())
	_refresh()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_queue_refresh()


func _editor() -> Node:
	return Kit.autoload(&"Editor")


func _word(kind: StringName, value: int) -> String:
	var tx := Kit.autoload(&"Tx")
	return String(tx.call(&"word", kind, value)) if tx != null else str(value)


func _queue_refresh() -> void:
	if _refresh_pending:
		return
	if not is_visible_in_tree():
		_stale = true
		return
	_refresh_pending = true
	_refresh.call_deferred()


# --- the table --------------------------------------------------------------------------------------

func _refresh() -> void:
	_refresh_pending = false
	if not is_node_ready():
		return
	var e := _editor()
	var open := e != null and bool(e.call(&"is_open"))
	# The table hides its contents, never itself: a hidden node would miss the refresh that opens it.
	for c: Node in get_children():
		(c as Control).visible = open
	if not open:
		_draft = {}
		_issues = []
		_rows_key = ""
		Kit.clear(_list)
		Kit.clear(_builder)
		return
	_vocab = Kit.as_dict(e.call(&"rule_vocabulary"))
	_rules = Kit.dicts(e.call(&"rules"))
	_parts = Kit.dicts(e.call(&"parts"))
	_max_layer = Kit.get_int(Kit.as_dict(e.call(&"limits")), "max_layer", Bp.LAYER_SCRIPT)
	var allowed := _max_layer >= Bp.LAYER_RULES
	var cap := _max_rules()
	_count.text = tr(&"editor_rules.count").format([Kit.number(float(_rules.size()), &"none", 0), Kit.number(float(cap), &"none", 0)])
	_capped.visible = not allowed
	_capped.text = tr(&"editor_control.layer.capped").format([_word(&"layer", Bp.LAYER_RULES)])
	_empty.visible = _rules.is_empty()
	var full := _rules.size() >= cap
	_add.disabled = not allowed or full or (not _draft.is_empty() and int(_draft.index) < 0)
	_full.text = tr(&"editor_rules.full").format([Kit.number(float(cap), &"none", 0)]) if full else ""
	_full.visible = full
	_remember_focus()
	var rows_key := "%s|%s|%d" % [str(_rules), str(allowed), _parts.size()]
	if rows_key != _rows_key:
		_rows_key = rows_key
		_build_rows(allowed)
	var vocab_key := str(_vocab.hash())
	if not _draft.is_empty():
		# An edited rule that vanished (undo, another edit) closes the builder.
		if int(_draft.index) >= _rules.size():
			_close_builder()
		elif vocab_key != _vocab_key:
			_build_builder()
	_vocab_key = vocab_key
	_chain_focus()
	_restore_focus()


func _max_rules() -> int:
	return maxi(Kit.get_int(_vocab, "max_rules", 32), 1)


func _build_rows(allowed: bool) -> void:
	Kit.clear(_list)
	for i in _rules.size():
		var r: Dictionary = _rules[i]
		var box := Kit.vbox(1)
		var top := Kit.hbox(1)
		var on := CheckBox.new()
		on.button_pressed = Kit.get_bool(r, "enabled", true)
		on.disabled = not allowed
		on.accessibility_name = tr(&"editor_rules.row.enabled").format([Kit.number(float(i + 1), &"none", 0)])
		on.tooltip_text = tr(&"editor_rules.row.enabled_tip")
		on.toggled.connect(_on_toggle.bind(i))
		on.set_meta(&"focus_key", "on:%d" % i)
		top.add_child(on)
		var sentence := Kit.label(tr(&"editor_rules.row.text").format([Kit.number(float(i + 1), &"none", 0), _Text.resolve(r.get("text", {}))]), &"Body", true)
		sentence.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		top.add_child(sentence)
		box.add_child(top)
		if not Kit.get_bool(r, "valid", true):
			box.add_child(Kit.label(tr(&"editor_rules.row.invalid"), &"SeverityCaution", true))
		elif not Kit.get_bool(r, "enabled", true):
			box.add_child(Kit.label(tr(&"editor_rules.row.off"), &"SeverityAdvisory", true))
		var actions := Kit.hbox(1)
		actions.alignment = BoxContainer.ALIGNMENT_END
		var up := Kit.icon_button(&"arrow_up", tr(&"editor_rules.row.up"))
		up.disabled = not allowed or i == 0
		up.pressed.connect(_on_move.bind(i, i - 1))
		up.set_meta(&"focus_key", "up:%d" % i)
		actions.add_child(up)
		var down := Kit.icon_button(&"arrow_down", tr(&"editor_rules.row.down"))
		down.disabled = not allowed or i == _rules.size() - 1
		down.pressed.connect(_on_move.bind(i, i + 1))
		down.set_meta(&"focus_key", "down:%d" % i)
		actions.add_child(down)
		var edit := Kit.inline_button("editor_rules.row.edit")
		edit.disabled = not allowed
		edit.tooltip_text = tr(&"editor_rules.row.edit_tip")
		edit.pressed.connect(_on_edit.bind(i))
		edit.set_meta(&"focus_key", "edit:%d" % i)
		actions.add_child(edit)
		var del := Kit.icon_button(&"delete", tr(&"editor_rules.row.delete"))
		del.disabled = not allowed
		del.pressed.connect(_on_delete.bind(i))
		del.set_meta(&"focus_key", "del:%d" % i)
		actions.add_child(del)
		box.add_child(actions)
		if i < _rules.size() - 1:
			box.add_child(HSeparator.new())
		_list.add_child(box)


func _on_toggle(on: bool, i: int) -> void:
	if i >= _rules.size():
		return
	var r: Dictionary = _rules[i].duplicate(true)
	r["enabled"] = on
	_rows_key = ""
	var issues := Kit.as_array(_editor().call(&"set_rule", i, r))
	if not issues.is_empty():
		# Switching a broken rule on is refused: open it so the problem shows where it can be fixed.
		_open_draft(i)
		_issues = issues
		_build_builder()
	_queue_refresh()


func _on_move(from: int, to: int) -> void:
	if to < 0 or to >= _rules.size():
		return
	if bool(_editor().call(&"move_rule", from, to)):
		_focus_key = ("up:%d" if to < from else "down:%d") % to
		if not _draft.is_empty() and int(_draft.index) == from:
			_draft.index = to
		elif not _draft.is_empty() and int(_draft.index) == to:
			_draft.index = from
	_queue_refresh()


func _on_delete(i: int) -> void:
	if not bool(_editor().call(&"remove_rule", i)):
		return
	if not _draft.is_empty():
		var at := int(_draft.index)
		if at == i:
			_draft = {}
			Kit.clear(_builder)
		elif at > i:
			_draft.index = at - 1
	_focus_key = "add"
	_queue_refresh()


func _on_add() -> void:
	_open_draft(-1)
	_build_builder()
	_chain_focus()
	_focus_first_builder_control()


func _on_edit(i: int) -> void:
	_open_draft(i)
	_build_builder()
	_chain_focus()
	_focus_first_builder_control()


# --- the draft --------------------------------------------------------------------------------------

func _open_draft(index: int) -> void:
	_issues = []
	if index < 0 or index >= _rules.size():
		var hostile := {"min_stance": Bp.STANCE_HOSTILE, "domain": -1, "min_grade": Bp.GRADE_BEARING}
		var sink := _first_sink(["mount_aim_point", "missile_target", "helm_speed"])
		_draft = {
			"index": -1, "enabled": true, "join": &"all", "kept": null,
			"items": [_compare(&"nearest_contact_range_m", Bp.CMP_LT, DRAFT_RANGE_M, hostile)],
			"sink": sink, "value": _default_value(sink, hostile),
		}
		return
	var r: Dictionary = _rules[index]
	var when := Kit.as_dict(r.get("when", {}))
	var items: Array = []
	var join: StringName = &"all"
	var kept: Variant = null
	var op := StringName(when.get("op", &""))
	if op == &"compare" and _showable(when):
		items = [when.duplicate(true)]
	elif (op == &"all" or op == &"any") and _flat(when):
		join = op
		for c: Variant in Kit.as_array(when.get("items", [])):
			items.append((c as Dictionary).duplicate(true))
	else:
		kept = when.duplicate(true)
	_draft = {
		"index": index, "enabled": Kit.get_bool(r, "enabled", true), "join": join, "kept": kept, "items": items,
		"sink": Kit.as_dict(r.get("sink", {})).duplicate(true), "value": Kit.as_dict(r.get("value", {})).duplicate(true),
	}


## True when an all or any condition holds only comparisons the builder can show.
static func _flat(c: Dictionary) -> bool:
	var items := Kit.as_array(c.get("items", []))
	if items.is_empty() or items.size() > MAX_ITEMS:
		return false
	for i: Variant in items:
		if not i is Dictionary or not _showable(i as Dictionary):
			return false
	return true


## A comparison the builder can show: anything but a number on the left.
static func _showable(c: Dictionary) -> bool:
	return StringName(c.get("op", &"")) == &"compare" and StringName(Kit.as_dict(c.get("lhs", {})).get("kind", &"")) != &"const"


func _compare(lhs_kind: StringName, cmp: int, value: float, filter: Dictionary = {}) -> Dictionary:
	return {"op": &"compare", "lhs": _operand(lhs_kind, 0.0, filter), "cmp": cmp, "rhs": _operand(&"const", value)}


func _operand(kind: StringName, value: float = 0.0, filter: Dictionary = {}) -> Dictionary:
	var f := filter.duplicate() if not filter.is_empty() else {"min_stance": Bp.STANCE_SUSPECT, "domain": -1, "min_grade": Bp.GRADE_BEARING}
	var part := -1
	if _operand_needs(kind) == &"part":
		var parts := _operand_parts(kind)
		part = parts[0] if not parts.is_empty() else -1
	return {"kind": kind, "value": value, "part": part, "filter": f}


func _operand_info(kind: StringName) -> Dictionary:
	for o: Dictionary in Kit.dicts(_vocab.get("operands", [])):
		if StringName(o.get("kind", &"")) == kind:
			return o
	return {}


func _operand_needs(kind: StringName) -> StringName:
	return StringName(_operand_info(kind).get("needs", &""))


func _operand_unit(kind: StringName) -> StringName:
	return StringName(_operand_info(kind).get("unit", &"none"))


## Parts an operand may read: mounts for ammunition, any part for health.
func _operand_parts(kind: StringName) -> Array[int]:
	var out: Array[int] = []
	if kind == &"ammo_fraction":
		for s: Dictionary in _sinks():
			var k := Kit.get_str(Kit.as_dict(s.get("sink", {})), "kind")
			var p := Kit.get_int(Kit.as_dict(s.get("sink", {})), "part", -1)
			if (k == "mount_fire_permission" or k == "missile_launch") and p >= 0 and not out.has(p):
				out.append(p)
	else:
		for p: Dictionary in _parts:
			out.append(Kit.get_int(p, "slot", -1))
	out.sort()
	return out


func _sinks() -> Array[Dictionary]:
	return Kit.dicts(_vocab.get("sinks", []))


func _first_sink(kinds: Array) -> Dictionary:
	var sinks := _sinks()
	for k: Variant in kinds:
		for s: Dictionary in sinks:
			if Kit.get_str(Kit.as_dict(s.get("sink", {})), "kind") == String(k):
				return Kit.as_dict(s.get("sink", {})).duplicate()
	return Kit.as_dict(sinks[0].get("sink", {})).duplicate() if not sinks.is_empty() else {"kind": &"helm_speed", "part": -1, "track": -1}


func _value_type(sink: Dictionary) -> StringName:
	for s: Dictionary in _sinks():
		var k := Kit.as_dict(s.get("sink", {}))
		if Kit.get_str(k, "kind") == Kit.get_str(sink, "kind") and Kit.get_int(k, "part", -1) == Kit.get_int(sink, "part", -1):
			return StringName(s.get("value_type", &"scalar"))
	return &"scalar"


func _default_value(sink: Dictionary, filter: Dictionary = {}) -> Dictionary:
	match _value_type(sink):
		&"nearest_contact":
			var f := filter.duplicate() if not filter.is_empty() else {"min_stance": Bp.STANCE_HOSTILE, "domain": -1, "min_grade": Bp.GRADE_BEARING}
			return {"type": &"nearest_contact", "value": f}
		&"bool":
			return {"type": &"bool", "value": true}
		&"stance":
			return {"type": &"stance", "value": Bp.STANCE_HOSTILE}
		&"rung":
			return {"type": &"rung", "value": Bp.RUNG_CLASS}
		&"preset":
			return {"type": &"preset", "value": Bp.PRESET_AGGRESSIVE}
		&"emcon":
			return {"type": &"emcon", "value": Bp.EMCON_ACTIVE}
	var kind := Kit.get_str(sink, "kind")
	var v := 1.0 if kind == "power_priority" else 0.0
	if kind == "helm_speed":
		v = 20.0
	return {"type": &"scalar", "value": v}


func _draft_rule() -> Dictionary:
	var when: Dictionary
	if _draft.kept != null:
		when = _draft.kept
	elif (_draft.items as Array).size() == 1:
		when = (_draft.items as Array)[0]
	else:
		when = {"op": _draft.join, "items": _draft.items}
	return {"enabled": _draft.enabled, "when": when, "sink": _draft.sink, "value": _draft.value}


func _close_builder() -> void:
	_draft = {}
	_issues = []
	Kit.clear(_builder)
	_focus_key = "add"
	_queue_refresh()


func _on_store() -> void:
	var e := _editor()
	var rule := _draft_rule()
	var index := int(_draft.index)
	var issues: Array
	if index < 0:
		issues = Kit.as_array(e.call(&"add_rule", -1, rule))
	else:
		issues = Kit.as_array(e.call(&"set_rule", index, rule))
	if issues.is_empty():
		_draft = {}
		_issues = []
		Kit.clear(_builder)
		_focus_key = "edit:%d" % (index if index >= 0 else _rules.size())
		_queue_refresh()
		return
	_issues = issues
	_build_builder()
	_chain_focus()


# --- the builder ------------------------------------------------------------------------------------

func _build_builder() -> void:
	Kit.clear(_builder)
	if _draft.is_empty():
		return
	_builder.add_child(HSeparator.new())
	var index := int(_draft.index)
	var title := tr(&"editor_rules.builder.new") if index < 0 else tr(&"editor_rules.builder.edit").format([Kit.number(float(index + 1), &"none", 0)])
	_builder.add_child(Kit.label(title, &"PanelTitle", true))
	_builder.add_child(Kit.label("editor_rules.builder.when", &"Body"))
	if _draft.kept != null:
		var phrase := _Text.resolve(Kit.as_dict(_rules[index].get("text", {}))) if index >= 0 and index < _rules.size() else ""
		_builder.add_child(Kit.label(tr(&"editor_rules.builder.kept").format([phrase]), &"Dense", true))
		var replace := Kit.inline_button("editor_rules.builder.replace")
		replace.tooltip_text = tr(&"editor_rules.builder.replace_tip")
		replace.pressed.connect(func() -> void:
			_draft.kept = null
			_draft.items = [_compare(&"own_speed_kn", Bp.CMP_LT, 12.0)]
			_build_builder()
			_chain_focus())
		_builder.add_child(replace)
	else:
		var items: Array = _draft.items
		if items.size() > 1:
			var join_row := Kit.hbox(1)
			var join := OptionButton.new()
			join.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
			join.custom_minimum_size = Vector2(FIELD_W, FIELD_H)
			join.add_item(tr(&"editor_rules.builder.all"), 0)
			join.add_item(tr(&"editor_rules.builder.any"), 1)
			join.select(1 if _draft.join == &"any" else 0)
			join.accessibility_name = tr(&"editor_rules.builder.join")
			join.item_selected.connect(func(i: int) -> void: _draft.join = &"any" if i == 1 else &"all")
			join_row.add_child(join)
			_builder.add_child(join_row)
		for i in items.size():
			_build_comparison(i, items[i])
		var more := Kit.inline_button("editor_rules.builder.add_comparison")
		more.disabled = items.size() >= MAX_ITEMS
		more.pressed.connect(func() -> void:
			(_draft.items as Array).append(_compare(&"own_speed_kn", Bp.CMP_LT, 12.0))
			_build_builder()
			_chain_focus())
		_builder.add_child(more)
	_builder.add_child(Kit.label("editor_rules.builder.then", &"Body"))
	_build_sink()
	_build_value()
	for issue: Variant in _issues:
		var text := _Text.resolve(Kit.as_dict(issue).get("text", {})) if issue is Dictionary else ""
		if not text.is_empty():
			_builder.add_child(Kit.label(text, &"SeverityFail", true))
	var preview := _preview()
	if not preview.is_empty():
		_builder.add_child(Kit.label(preview, &"Dense", true))
	var buttons := Kit.hbox(2)
	var store := Kit.inline_button("editor_rules.builder.add" if index < 0 else "editor_rules.builder.store")
	store.pressed.connect(_on_store)
	store.set_meta(&"focus_key", "store")
	buttons.add_child(store)
	var cancel := Kit.inline_button("editor_rules.builder.cancel")
	cancel.pressed.connect(_close_builder)
	cancel.set_meta(&"focus_key", "cancel")
	buttons.add_child(cancel)
	_builder.add_child(buttons)


## Where the draft would sit and what it overrides, in one line.
func _preview() -> String:
	var index := int(_draft.index)
	var at := index if index >= 0 else _rules.size()
	return tr(&"editor_rules.builder.position").format([Kit.number(float(at + 1), &"none", 0)])


func _build_comparison(i: int, c: Dictionary) -> void:
	var box := Kit.vbox(1)
	var lhs := Kit.as_dict(c.get("lhs", {}))
	var rhs := Kit.as_dict(c.get("rhs", {}))
	var count := (_draft.items as Array).size()
	if count > 1:
		var head := Kit.hbox(1)
		var n := Kit.label(tr(&"editor_rules.builder.comparison").format([Kit.number(float(i + 1), &"none", 0)]), &"Dense", true)
		n.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		head.add_child(n)
		var remove := Kit.icon_button(&"remove", tr(&"editor_rules.builder.remove_comparison"))
		remove.pressed.connect(func() -> void:
			(_draft.items as Array).remove_at(i)
			_rebuild())
		head.add_child(remove)
		box.add_child(head)
	# What is read, then what qualifies it (a part, or which contacts), then the test.
	var flow := _flow()
	flow.add_child(_operand_pick(lhs, func(kind: StringName) -> void:
		c["lhs"] = _operand(kind, 0.0, Kit.as_dict(lhs.get("filter", {})))
		if StringName(Kit.as_dict(c.get("rhs", {})).get("kind", &"")) == &"const":
			c["rhs"] = _operand(&"const", _default_const(kind))
		_rebuild(), true))
	box.add_child(flow)
	_operand_extra(box, lhs)
	var flow2 := _flow()
	var cmp := OptionButton.new()
	cmp.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	cmp.custom_minimum_size = Vector2(0.0, FIELD_H)
	for j in CMPS.size():
		cmp.add_item(tr(StringName("units.cmp." + CMP_NAMES[j])), CMPS[j])
	_select_id(cmp, Kit.get_int(c, "cmp", Bp.CMP_LT))
	cmp.accessibility_name = tr(&"editor_rules.builder.cmp")
	cmp.item_selected.connect(func(idx: int) -> void: c["cmp"] = cmp.get_item_id(idx))
	flow2.add_child(cmp)
	var rhs_pick := _operand_pick(rhs, func(kind: StringName) -> void:
		c["rhs"] = _operand(kind, _default_const(StringName(lhs.get("kind", &""))) if kind == &"const" else 0.0)
		_rebuild())
	rhs_pick.custom_minimum_size.x = 0.0
	flow2.add_child(rhs_pick)
	if StringName(rhs.get("kind", &"")) == &"const":
		flow2.add_child(_const_field(rhs, _operand_unit(StringName(lhs.get("kind", &"")))))
	box.add_child(flow2)
	_operand_extra(box, rhs)
	if i < count - 1:
		box.add_child(HSeparator.new())
	_builder.add_child(box)


func _rebuild() -> void:
	_issues = []
	_build_builder()
	_chain_focus()


## A sensible number to compare an operand against when the right side becomes a number.
func _default_const(kind: StringName) -> float:
	match _operand_unit(kind):
		&"pct":
			return 0.5
		&"m":
			return DRAFT_RANGE_M
		&"kn":
			return 12.0
		&"s":
			return 60.0
	return 1.0


func _operand_pick(op: Dictionary, on_pick: Callable, left: bool = false) -> OptionButton:
	var o := OptionButton.new()
	o.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	o.custom_minimum_size = Vector2(FIELD_W, FIELD_H)
	o.accessibility_name = tr(&"editor_rules.builder.operand")
	o.tooltip_text = tr(&"editor_rules.builder.operand_tip")
	var operands := Kit.dicts(_vocab.get("operands", []))
	var current := StringName(op.get("kind", &""))
	for j in operands.size():
		var kind := StringName(operands[j].get("kind", &""))
		# A number is only ever the right-hand side: the left reads the ship or the sea.
		if left and kind == &"const":
			continue
		var idx := o.item_count
		o.add_item(tr(StringName(operands[j].get("label", &""))), idx)
		o.set_item_metadata(idx, kind)
		if kind == current:
			o.select(idx)
	o.item_selected.connect(func(idx: int) -> void: on_pick.call(StringName(o.get_item_metadata(idx))))
	return o


## The part or contact filter an operand needs, under its picker.
func _operand_extra(box: VBoxContainer, op: Dictionary) -> void:
	var kind := StringName(op.get("kind", &""))
	match _operand_needs(kind):
		&"part":
			var flow := _flow()
			var pick := OptionButton.new()
			pick.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
			pick.custom_minimum_size = Vector2(FIELD_W, FIELD_H)
			pick.accessibility_name = tr(&"editor_rules.builder.part")
			var parts := _operand_parts(kind)
			for p in parts:
				pick.add_item(Inspector.part_name(p, _parts), p)
			_select_id(pick, Kit.get_int(op, "part", -1))
			if pick.selected < 0 and pick.item_count > 0:
				pick.select(0)
				op["part"] = pick.get_item_id(0)
			pick.item_selected.connect(func(idx: int) -> void: op["part"] = pick.get_item_id(idx))
			flow.add_child(Kit.label("editor_rules.builder.of", &"Dense"))
			flow.add_child(pick)
			box.add_child(flow)
		&"filter":
			var f := Kit.as_dict(op.get("filter", {}))
			if f.is_empty():
				f = {"min_stance": Bp.STANCE_SUSPECT, "domain": -1, "min_grade": Bp.GRADE_BEARING}
				op["filter"] = f
			box.add_child(_filter_editor(f))


## Lowest stance, domain and lowest track grade of the contacts an operand or value counts.
func _filter_editor(f: Dictionary) -> Control:
	var flow := _flow()
	var stance := OptionButton.new()
	stance.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	stance.custom_minimum_size = Vector2(0.0, FIELD_H)
	stance.accessibility_name = tr(&"editor_rules.filter.stance")
	for s in STANCES:
		var text := tr(&"editor_rules.filter.stance_at_least").format([_word(&"stance", s)])
		if s == Bp.STANCE_FRIEND:
			text = tr(&"editor_rules.filter.any_stance")
		elif s == Bp.STANCE_HOSTILE:
			text = tr(&"editor_rules.filter.hostile_only")
		stance.add_item(text, s)
	_select_id(stance, Kit.get_int(f, "min_stance", Bp.STANCE_SUSPECT))
	stance.item_selected.connect(func(idx: int) -> void: f["min_stance"] = stance.get_item_id(idx))
	flow.add_child(stance)
	var domain := OptionButton.new()
	domain.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	domain.custom_minimum_size = Vector2(0.0, FIELD_H)
	domain.accessibility_name = tr(&"editor_rules.filter.domain")
	for j in DOMAINS.size():
		domain.add_item(tr(&"editor_rules.filter.any_domain") if DOMAINS[j] < 0 else _word(&"domain", DOMAINS[j]), j)
		domain.set_item_metadata(j, DOMAINS[j])
		if DOMAINS[j] == Kit.get_int(f, "domain", -1):
			domain.select(j)
	domain.item_selected.connect(func(idx: int) -> void: f["domain"] = int(domain.get_item_metadata(idx)))
	flow.add_child(domain)
	var grade := OptionButton.new()
	grade.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	grade.custom_minimum_size = Vector2(0.0, FIELD_H)
	grade.accessibility_name = tr(&"editor_rules.filter.grade")
	for g in GRADES:
		grade.add_item(tr(&"editor_rules.filter.any_grade") if g == Bp.GRADE_BEARING else tr(&"editor_rules.filter.grade_at_least").format([_word(&"grade", g)]), g)
	_select_id(grade, Kit.get_int(f, "min_grade", Bp.GRADE_BEARING))
	grade.item_selected.connect(func(idx: int) -> void: f["min_grade"] = grade.get_item_id(idx))
	flow.add_child(grade)
	return flow


## A number in the unit of what it is compared with; percentages are typed in percent.
func _const_field(op: Dictionary, unit: StringName) -> SpinBox:
	var f := SpinBox.new()
	f.custom_minimum_size = Vector2(FIELD_W, FIELD_H)
	f.select_all_on_focus = true
	f.update_on_text_changed = true
	f.get_line_edit().accessibility_name = tr(&"editor_rules.builder.number")
	f.allow_greater = true
	f.allow_lesser = true
	var scale := 100.0 if unit == &"pct" else 1.0
	match unit:
		&"pct":
			f.min_value = 0.0
			f.max_value = 100.0
			f.step = 1.0
			f.allow_greater = false
			f.allow_lesser = false
		&"m":
			f.min_value = 0.0
			f.max_value = 100000.0
			f.step = 100.0
			f.allow_lesser = false
		&"deg":
			f.min_value = -180.0
			f.max_value = 360.0
			f.step = 1.0
		_:
			f.min_value = -100000.0
			f.max_value = 100000.0
			f.step = 0.1 if unit == &"kw" or unit == &"none" else 1.0
	f.suffix = tr(StringName("editor_control.unit." + String(unit))) if unit != &"none" else ""
	f.set_value_no_signal(Kit.get_float(op, "value") * scale)
	f.value_changed.connect(func(v: float) -> void: op["value"] = v / scale)
	return f


func _build_sink() -> void:
	var sink: Dictionary = _draft.sink
	var kinds: Array[String] = []
	for s: Dictionary in _sinks():
		var k := Kit.get_str(Kit.as_dict(s.get("sink", {})), "kind")
		if not kinds.has(k):
			kinds.append(k)
	kinds.sort_custom(func(a: String, b: String) -> bool: return SINK_ORDER.find(a) < SINK_ORDER.find(b))
	var flow := _flow()
	var kind_pick := OptionButton.new()
	kind_pick.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	kind_pick.custom_minimum_size = Vector2(FIELD_W, FIELD_H)
	kind_pick.accessibility_name = tr(&"editor_rules.builder.sink")
	kind_pick.tooltip_text = tr(&"editor_rules.builder.sink_tip")
	var current := Kit.get_str(sink, "kind")
	for j in kinds.size():
		kind_pick.add_item(tr(StringName("editor_rules.sink." + kinds[j])), j)
		kind_pick.set_item_metadata(j, kinds[j])
		if kinds[j] == current:
			kind_pick.select(j)
	kind_pick.item_selected.connect(func(idx: int) -> void:
		_draft.sink = _first_sink([String(kind_pick.get_item_metadata(idx))])
		_draft.value = _default_value(_draft.sink)
		_rebuild())
	flow.add_child(kind_pick)
	# The part, for a sink bound to one.
	var targets: Array[Dictionary] = []
	for s: Dictionary in _sinks():
		var k := Kit.as_dict(s.get("sink", {}))
		if Kit.get_str(k, "kind") == current and Kit.get_int(k, "part", -1) >= 0:
			targets.append(s)
	if not targets.is_empty():
		var part_pick := OptionButton.new()
		part_pick.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		part_pick.custom_minimum_size = Vector2(FIELD_W, FIELD_H)
		part_pick.accessibility_name = tr(&"editor_rules.builder.sink_part")
		for j in targets.size():
			var k := Kit.as_dict(targets[j].get("sink", {}))
			var p := Kit.get_int(k, "part", -1)
			var name := _Text.resolve(targets[j].get("label", {})) if current == "flood_valve" else Inspector.part_name(p, _parts)
			part_pick.add_item(name, j)
			part_pick.set_item_metadata(j, k)
			if p == Kit.get_int(sink, "part", -1):
				part_pick.select(j)
		part_pick.item_selected.connect(func(idx: int) -> void:
			var before := _value_type(_draft.sink)
			_draft.sink = Kit.as_dict(part_pick.get_item_metadata(idx)).duplicate()
			if _value_type(_draft.sink) != before:
				_draft.value = _default_value(_draft.sink)
			_rebuild())
		flow.add_child(part_pick)
	_builder.add_child(flow)


func _build_value() -> void:
	var sink: Dictionary = _draft.sink
	var value: Dictionary = _draft.value
	var vt := _value_type(sink)
	var kind := Kit.get_str(sink, "kind")
	if StringName(value.get("type", &"")) != vt:
		value = _default_value(sink)
		_draft.value = value
	match vt:
		&"nearest_contact":
			_builder.add_child(Kit.label("editor_rules.value.nearest", &"Dense"))
			var f := Kit.as_dict(value.get("value", {}))
			if f.is_empty():
				f = {"min_stance": Bp.STANCE_HOSTILE, "domain": -1, "min_grade": Bp.GRADE_BEARING}
				value["value"] = f
			_builder.add_child(_filter_editor(f))
		&"bool":
			var o := _value_option()
			o.add_item(tr(StringName("editor_rules.value.on." + kind)), 1)
			o.add_item(tr(StringName("editor_rules.value.off." + kind)), 0)
			o.select(0 if Kit.get_bool(value, "value", true) else 1)
			o.item_selected.connect(func(idx: int) -> void: value["value"] = o.get_item_id(idx) == 1)
			_builder.add_child(_value_row(o))
		&"stance":
			_builder.add_child(_value_row(_enum_option(value, &"stance", [Bp.STANCE_FRIEND, Bp.STANCE_NEUTRAL, Bp.STANCE_SUSPECT, Bp.STANCE_HOSTILE])))
		&"rung":
			_builder.add_child(_value_row(_enum_option(value, &"rung", RUNGS)))
		&"preset":
			_builder.add_child(_value_row(_enum_option(value, &"preset", PRESETS)))
		&"emcon":
			_builder.add_child(_value_row(_enum_option(value, &"emcon", EMCONS)))
		_:
			var f := SpinBox.new()
			f.custom_minimum_size = Vector2(FIELD_W, FIELD_H)
			f.select_all_on_focus = true
			f.update_on_text_changed = true
			f.get_line_edit().accessibility_name = tr(&"editor_rules.builder.number")
			match kind:
				"helm_heading":
					f.min_value = 0.0
					f.max_value = 359.0
					f.step = 1.0
					f.suffix = tr(&"editor_control.unit.deg")
				"helm_speed":
					f.min_value = 0.0
					f.max_value = 60.0
					f.step = 1.0
					f.suffix = tr(&"editor_control.unit.kn")
				"power_priority":
					f.min_value = 1.0
					f.max_value = 5.0
					f.step = 1.0
				_:
					f.min_value = -100000.0
					f.max_value = 100000.0
					f.step = 0.1
			f.set_value_no_signal(Kit.get_float(value, "value"))
			value["value"] = f.value
			f.value_changed.connect(func(v: float) -> void: value["value"] = v)
			_builder.add_child(_value_row(f))


func _value_option() -> OptionButton:
	var o := OptionButton.new()
	o.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	o.custom_minimum_size = Vector2(FIELD_W, FIELD_H)
	o.accessibility_name = tr(&"editor_rules.builder.value")
	return o


func _enum_option(value: Dictionary, family: StringName, values: Array[int]) -> OptionButton:
	var o := _value_option()
	for v in values:
		o.add_item(_word(family, v), v)
	_select_id(o, Kit.get_int(value, "value", values[0]))
	if o.selected < 0:
		o.select(0)
		value["value"] = values[0]
	o.item_selected.connect(func(idx: int) -> void: value["value"] = o.get_item_id(idx))
	return o


func _value_row(field: Control) -> Control:
	var flow := _flow()
	flow.add_child(Kit.label("editor_rules.builder.to", &"Dense"))
	flow.add_child(field)
	return flow


# --- helpers ----------------------------------------------------------------------------------------

func _flow() -> HFlowContainer:
	var f := HFlowContainer.new()
	f.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	f.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	return f


static func _select_id(o: OptionButton, id: int) -> void:
	var idx := o.get_item_index(id)
	if idx >= 0:
		o.select(idx)


func _remember_focus() -> void:
	var f := get_viewport().gui_get_focus_owner() if get_viewport() != null else null
	if f != null and is_ancestor_of(f) and f.has_meta(&"focus_key"):
		_focus_key = String(f.get_meta(&"focus_key"))


func _restore_focus() -> void:
	if _focus_key.is_empty():
		return
	for c: Node in find_children("*", "Control", true, false):
		if String(c.get_meta(&"focus_key", "")) == _focus_key and (c as Control).is_visible_in_tree():
			var b := c as BaseButton
			if b == null or not b.disabled:
				Ui.focus(c as Control)
				break
	_focus_key = ""


func _focus_first_builder_control() -> void:
	_show_builder.call_deferred()
	for c: Node in _builder.find_children("*", "Control", true, false):
		var ctl := c as Control
		if ctl.focus_mode != Control.FOCUS_NONE and not ctl is LineEdit:
			Ui.focus(ctl)
			return


## Scroll the dock so the builder's title is at the top of the view.
func _show_builder() -> void:
	# Containers settle over a frame; the scroll range follows them.
	await get_tree().process_frame
	await get_tree().process_frame
	if not is_inside_tree():
		return
	var p := get_parent()
	while p != null and not p is ScrollContainer:
		p = p.get_parent()
	if p == null or _builder.get_child_count() < 2:
		return
	var scroll := p as ScrollContainer
	var title := _builder.get_child(1) as Control
	scroll.scroll_vertical = int(title.global_position.y - scroll.global_position.y + scroll.scroll_vertical)


func _chain_focus() -> void:
	var list: Array = []
	for c: Node in find_children("*", "Control", true, false):
		var ctl := c as Control
		if ctl.focus_mode == Control.FOCUS_NONE or not ctl.is_visible_in_tree():
			continue
		if ctl is LineEdit and ctl.get_parent() is SpinBox and (ctl.get_parent() as Control).focus_mode != Control.FOCUS_NONE:
			continue
		list.append(ctl)
	_open_ends(Kit.chain_vertical(list))


## The first and last controls hand Up and Down on to the rest of the dock (the control panel above, the
## tab bar), instead of stopping at the table's edge.
static func _open_ends(live: Array[Control]) -> void:
	if live.is_empty():
		return
	live[0].focus_neighbor_top = NodePath()
	live[0].focus_previous = NodePath()
	live[-1].focus_neighbor_bottom = NodePath()
	live[-1].focus_next = NodePath()
