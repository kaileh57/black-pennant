extends VBoxContainer
## Operations on offer; accept, decline, open briefing.
## DESIGN.md L776-782. Unit: ui-chart-panels.
## Reads and calls: Campaign.chart, Campaign.propose, Campaign.open_briefing.
##
## Top: the act in play (story operations done, the hull cap, the capstone, retreat or Open
## Theatre). Then the offers as one selectable list (cards only for selectable things, L1565): name,
## kind, the place and region, LANTERN's passage each way, and "cannot be declined" for a forced one.
## Under it the selected offer: its forecast (the weather the sortie will have, L743), expected
## opposition, constraint tags, clock pushback and expiry, from `Campaign.briefing`. The list and the
## detail scroll; Open briefing (the prominent order, L66) and Decline stay pinned under them, so the
## next order is always on screen. Decline asks first; a forced operation cannot be declined (L749).
##
## In co-op, Open briefing proposes accepting the operation and Decline proposes declining it; the
## proposals panel shows the vote (L839).

signal operation_selected(offer: Dictionary)
signal focus_changed
signal rejected(text: Variant)

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
## `Bp` values used here, kept so the script parses before the extension loads.
const OPERATION_TEMPLATE := 0
const OPERATION_STORY := 1
const OPERATION_ACE_DUEL := 2
const OPERATION_CAPSTONE := 3
const OPERATION_TUTORIAL := 4
const PHASE_CHART := 1
const THEATRE_ACTION_DECLINE_OPERATION := 4

var _source: RefCounted
var _offers: Array[Dictionary] = []
var _chart: Dictionary = {}
var _selected: StringName = &""
var _rows: Array[Button] = []
var _group := ButtonGroup.new()
var _signature := ""
## Briefings by operation for the current offers: generating one takes about 15 ms (gd-campaign), so
## it is read once per offer list, never per theatre hour.
var _briefs: Dictionary[StringName, Dictionary] = {}

@onready var _scroll: ScrollContainer = $Scroll
@onready var _act: Label = $Scroll/Column/Act
@onready var _list: VBoxContainer = $Scroll/Column/List
@onready var _empty: Label = $Scroll/Column/List/Empty
@onready var _detail: VBoxContainer = $Scroll/Column/Detail
@onready var _name: Label = $Scroll/Column/Detail/Name
@onready var _summary: Label = $Scroll/Column/Detail/Summary
@onready var _actions: HBoxContainer = $Actions
@onready var _brief: Button = $Actions/Brief
@onready var _decline: Button = $Actions/Decline


func _ready() -> void:
	Ui.set_separation(self, 2)
	Ui.set_separation($Scroll/Column, 2)
	Ui.set_separation(_list, 1)
	Ui.set_separation(_detail, 1)
	Ui.set_separation(_actions, 1)
	for l: Label in [_act, _empty, _name, _summary]:
		l.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		l.text = ""
	_brief.custom_minimum_size = Vector2(0.0, Kit.PRIMARY_H)
	_decline.custom_minimum_size = Vector2(0.0, Kit.PRIMARY_H)
	_brief.text = tr(&"chart.ops.brief")
	_brief.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_brief.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
	_brief.clip_text = true
	_decline.text = tr(&"chart.ops.decline")
	_brief.pressed.connect(_on_brief)
	_decline.pressed.connect(_on_decline)
	_group.allow_unpress = false
	accessibility_name = tr(&"chart.tab.operations")


func bind(source: RefCounted) -> void:
	_source = source
	_signature = ""


## Re-read the offers and the act (on `Campaign.chart_changed`); rebuilds only what changed.
func refresh() -> void:
	if _source == null or not is_node_ready():
		return
	_chart = _source.call(&"chart")
	var offers := Kit.dicts(_chart.get("offers"))
	var sig := str(offers) + str(_source.call(&"is_coop")) + str(Kit.get_int(_chart, "phase")) + str(Kit.get_int(_chart, "act"))
	if sig == _signature:
		_render_detail()
		return
	_signature = sig
	# The act's progress changes only with a result or an act change, which also change the phase.
	_render_act(_source.call(&"act_info"))
	_briefs.clear()
	_offers = offers
	_rebuild_rows()
	_render_detail()
	focus_changed.emit()


func _render_act(act: Dictionary) -> void:
	if act.is_empty():
		_act.text = ""
		_act.visible = false
		return
	_act.visible = true
	var n := Kit.get_int(act, "act")
	var parts := PackedStringArray()
	parts.append(tr(&"chart.ops.act").format([_word(&"act", n), tr(StringName("act.%d.name" % (n + 1)))]))
	if Kit.get_bool(act, "open_theatre"):
		parts.append(tr(&"chart.ops.open_theatre"))
	else:
		parts.append(tr(&"chart.ops.story").format([Kit.number(Kit.get_int(act, "story_done"), &"none", 0), Kit.number(Kit.get_int(act, "story_total"), &"none", 0)]))
		if Kit.get_bool(act, "capstone_open"):
			parts.append(tr(&"chart.ops.capstone_open"))
	parts.append(tr(&"chart.ops.cap").format([Kit.number(Kit.get_float(act, "cap_t"), &"t", 0)]))
	if Kit.get_bool(act, "retreat"):
		parts.append(tr(&"chart.ops.retreat"))
	_act.text = " ".join(parts)


func _rebuild_rows() -> void:
	for row: Button in _rows:
		_list.remove_child(row)
		row.queue_free()
	_rows.clear()
	var ids: Array[StringName] = []
	for o: Dictionary in _offers:
		ids.append(StringName(Kit.get_str(o, "id")))
	if not ids.has(_selected):
		_selected = ids[0] if not ids.is_empty() else &""
	_empty.visible = _offers.is_empty()
	_empty.text = tr(&"chart.ops.empty") if _chart.is_empty() == false else tr(&"chart.ops.no_campaign")
	for o: Dictionary in _offers:
		var b := Button.new()
		var id := StringName(Kit.get_str(o, "id"))
		b.toggle_mode = true
		b.button_group = _group
		b.focus_mode = Control.FOCUS_ALL
		b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		b.alignment = HORIZONTAL_ALIGNMENT_LEFT
		b.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		b.custom_minimum_size = Vector2(0.0, Kit.PRIMARY_H)
		b.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
		b.text = "%s\n%s" % [_offer_name(o), _offer_line(o)]
		b.accessibility_name = b.text.replace("\n", ". ")
		b.set_pressed_no_signal(id == _selected)
		b.toggled.connect(func(on: bool) -> void:
			if on:
				_pick(id))
		b.focus_entered.connect(func() -> void: _pick(id))
		_list.add_child(b)
		_rows.append(b)


func _offer_name(o: Dictionary) -> String:
	return _Text.resolve(StringName(Kit.get_str(o, "name", Kit.get_str(o, "id"))))


## Kind, place and region, and LANTERN's passage each way, with the forced mark (L749, L776). The
## operation costs the passage out, the sortie and the passage home in theatre time.
func _offer_line(o: Dictionary) -> String:
	var parts := PackedStringArray()
	var kind := kind_word(o)
	var region: String = _source.call(&"region_name", StringName(Kit.get_str(o, "region"))) if _source != null else ""
	var node := StringName(Kit.get_str(o, "node"))
	var place: String = _source.call(&"node_name", node) if _source != null and node != &"" else ""
	var where := region if place.is_empty() else tr(&"chart.ops.where").format([place, region])
	var away := Kit.number(Kit.get_int(o, "transit_h"), &"h", 0)
	# A template operation is named after its template, so its line starts with the place.
	if kind == _offer_name(o):
		parts.append(tr(&"chart.ops.line_short").format([where, away]))
	else:
		parts.append(tr(&"chart.ops.line").format([kind, where, away]))
	if Kit.get_bool(o, "forced"):
		parts.append(tr(&"chart.ops.forced"))
	return " ".join(parts)


## The word for an offer's kind: its mission template, or story, ace duel, capstone, tutorial.
static func kind_word(o: Dictionary) -> String:
	match Kit.get_int(o, "kind", -1):
		OPERATION_TEMPLATE:
			var w := _word(&"mission", Kit.get_int(o, "template", -1))
			return w if not w.is_empty() else String(TranslationServer.translate(&"chart.ops.kind.template"))
		OPERATION_STORY:
			return String(TranslationServer.translate(&"chart.ops.kind.story"))
		OPERATION_ACE_DUEL:
			return String(TranslationServer.translate(&"chart.ops.kind.ace_duel"))
		OPERATION_CAPSTONE:
			return String(TranslationServer.translate(&"chart.ops.kind.capstone"))
		OPERATION_TUTORIAL:
			return String(TranslationServer.translate(&"chart.ops.kind.tutorial"))
	return String(TranslationServer.translate(&"chart.ops.kind.template"))


func _offer(id: StringName) -> Dictionary:
	for o: Dictionary in _offers:
		if StringName(Kit.get_str(o, "id")) == id:
			return o
	return {}


func _pick(id: StringName) -> void:
	if id == _selected:
		return
	_selected = id
	for i in _rows.size():
		_rows[i].set_pressed_no_signal(StringName(Kit.get_str(_offers[i], "id")) == id)
	_render_detail()
	operation_selected.emit(_offer(id))


## Select an offer by operation id (an interrupt named it). Returns false when it is not on offer.
func select_operation(id: StringName) -> bool:
	if _offer(id).is_empty():
		return false
	_pick(id)
	var i := _offers.find(_offer(id))
	if i >= 0 and i < _rows.size():
		_scroll.ensure_control_visible.call_deferred(_rows[i])
	return true


## Select the first offer at a node. Returns false when none is there.
func select_at_node(node: StringName) -> bool:
	for o: Dictionary in _offers:
		if StringName(Kit.get_str(o, "node")) == node:
			return select_operation(StringName(Kit.get_str(o, "id")))
	return false


func selected_operation() -> StringName:
	return _selected


func _render_detail() -> void:
	var o := _offer(_selected)
	_detail.visible = not o.is_empty()
	_actions.visible = _detail.visible
	$Scroll/Column/Rule.visible = _detail.visible
	if o.is_empty():
		return
	var coop: bool = _source != null and _source.call(&"is_coop") == true
	_name.text = _offer_name(o)
	if not _briefs.has(_selected) and _source != null:
		_briefs[_selected] = _source.call(&"briefing", _selected)
	var b: Dictionary = _briefs.get(_selected, {})
	_summary.text = "\n".join(_summary_lines(o, b))
	var open: bool = Kit.get_int(_chart, "phase", -1) == PHASE_CHART and _source != null and _source.call(&"is_advancing") != true
	_brief.disabled = not open
	_brief.text = tr(&"chart.ops.brief_named").format([_name.text]) if open else tr(&"chart.ops.brief")
	_brief.tooltip_text = tr(&"chart.ops.brief_tip_coop" if coop else &"chart.ops.brief_tip") if open else tr(&"chart.place.busy")
	var forced := Kit.get_bool(o, "forced")
	_decline.text = tr(&"chart.ops.decline_propose" if coop else &"chart.ops.decline")
	_decline.disabled = forced or not open
	_decline.tooltip_text = tr(&"chart.ops.forced_tip") if forced else tr(&"chart.ops.decline_tip")


func _summary_lines(o: Dictionary, b: Dictionary) -> PackedStringArray:
	var lines := PackedStringArray()
	lines.append(_offer_line(o))
	if b.is_empty():
		return lines
	var w := Kit.as_dict(b.get("weather"))
	lines.append(tr(&"chart.ops.forecast").format([_word(&"weather", Kit.get_int(w, "kind")), Kit.number(Kit.get_int(w, "sea_state"), &"none", 0), _word(&"time", Kit.get_int(b, "time_of_day")), Kit.number(Kit.get_float(w, "visibility_m") / 1000.0, &"km", 0)]))
	var lo := 0
	var hi := 0
	for t: Dictionary in Kit.dicts(b.get("threats")):
		lo += Kit.get_int(t, "count_min")
		hi += maxi(Kit.get_int(t, "count_max"), Kit.get_int(t, "count_min"))
	if hi > 0:
		var count := Kit.number(lo, &"none", 0) if lo == hi else tr(&"chart.ops.range").format([Kit.number(lo, &"none", 0), Kit.number(hi, &"none", 0)])
		lines.append(tr(&"chart.ops.threat").format([count]))
	var tags := PackedStringArray()
	for t: Variant in Kit.as_array(b.get("tags")):
		var s := _Text.resolve(t)
		if not s.is_empty():
			tags.append(s)
	if not tags.is_empty():
		lines.append(tr(&"chart.ops.tags").format([" ".join(tags)]))
	var pushback := Kit.get_float(b, "pushback_days")
	if pushback > 0.0:
		lines.append(tr(&"chart.ops.pushback").format([Kit.number(pushback, &"none", 0)]))
	var expires := Kit.get_int(b, "expires_h", -1)
	if expires >= 0:
		lines.append(tr(&"chart.ops.expires").format([_dtg(expires)]))
	return lines


func _on_brief() -> void:
	if _selected == &"" or _source == null:
		return
	_source.call(&"open_briefing", _selected)


func _on_decline() -> void:
	var o := _offer(_selected)
	if o.is_empty() or Kit.get_bool(o, "forced"):
		return
	var screens := Kit.autoload(&"Screens")
	var body := tr(&"chart.ops.decline_body").format([_offer_name(o)])
	if screens == null:
		_decline_now(_selected)
		return
	var id := _selected
	var dialog: Node = screens.call(&"open_dialog", &"chart.ops.decline_title", body,
		[{"id": &"decline", "label": &"chart.ops.decline_confirm"}, {"id": &"keep", "label": &"chart.ops.keep"}], &"keep", &"keep")
	if dialog != null:
		dialog.connect(&"chosen", func(choice: StringName) -> void:
			if choice == &"decline":
				_decline_now(id))


func _decline_now(id: StringName) -> void:
	if _source == null:
		return
	_source.call(&"propose", {"kind": THEATRE_ACTION_DECLINE_OPERATION, "operation": id})


## The prominent control: the selected offer's row, else none.
func first_control() -> Control:
	for i in _rows.size():
		if StringName(Kit.get_str(_offers[i], "id")) == _selected:
			return _rows[i]
	return _rows[0] if not _rows.is_empty() else null


func focus_controls() -> Array[Control]:
	var out: Array[Control] = []
	for r: Button in _rows:
		out.append(r)
	if _detail.visible:
		for c: Button in [_brief, _decline]:
			if not c.disabled:
				out.append(c)
	return out


static func _word(kind: StringName, value: int) -> String:
	var tx := Kit.autoload(&"Tx")
	if tx != null and tx.has_method(&"word"):
		return String(tx.call(&"word", kind, value))
	return ""


static func _dtg(hour: int) -> String:
	var tx := Kit.autoload(&"Tx")
	if tx != null and tx.has_method(&"date_time_group"):
		return String(tx.call(&"date_time_group", hour))
	return ""
