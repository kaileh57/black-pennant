extends ScrollContainer
## Doctrine bulletin, tallies and Intel spends.
## DESIGN.md L772, L815. Unit: ui-chart-panels.
## Reads and calls: Campaign.doctrine_bulletin, Campaign.intel_options, Campaign.propose.
##
## What the Combine is learning, in the open so it can be countered (L772): every counter card in
## force or queued, soonest first, with its region and arrival; on a queued card, the two Intel
## spends the theatre offers for it (delay 72 h, cancel), priced by `Campaign.intel_options`. Then
## the four win-method tallies as gauges against the card threshold of 10 (they decay with a
## 120 h half-life between operations, so they carry no trend arrow), and the other Intel spends:
## locate a region's convoys for 4 days, locate an ace (L815).
##
## A spend the pool cannot pay is shown disabled with what it lacks. In co-op a spend is a proposal
## like any other decision (L837).

signal focus_changed
signal rejected(text: Variant)

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const GAUGE_SCENE := preload("res://ui/widgets/gauge.tscn")
## `Bp` values used here, kept so the script parses before the extension loads.
const THEATRE_ACTION_SPEND_INTEL := 5
const INTEL_DELAY_CARD := 0
const INTEL_CANCEL_CARD := 1
const INTEL_REVEAL_CONVOYS := 2
const INTEL_REVEAL_ACE := 3
const PHASE_CHART := 1
## A tally queues its counter at this value (L772).
const TALLY_CARD_AT := 10.0

var _source: RefCounted
var _signature := ""
var _buttons: Array[Button] = []

@onready var _pool: Label = $Column/Pool
@onready var _cards: VBoxContainer = $Column/Cards/List
@onready var _tallies: VBoxContainer = $Column/Tallies/List
@onready var _tally_note: Label = $Column/Tallies/Note
@onready var _intel: VBoxContainer = $Column/Intel/List


func _ready() -> void:
	Ui.set_separation($Column, 4)
	for box: Control in [$Column/Cards, $Column/Tallies, $Column/Intel]:
		Ui.set_separation(box, 1)
	Ui.set_separation(_cards, 3)
	Ui.set_separation(_tallies, 1)
	Ui.set_separation(_intel, 1)
	$Column/Cards/Title.text = &"chart.bulletin.cards"
	$Column/Tallies/Title.text = &"chart.bulletin.tallies"
	$Column/Intel/Title.text = &"chart.bulletin.intel"
	_pool.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_tally_note.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_pool.text = ""
	_tally_note.text = ""
	accessibility_name = tr(&"chart.tab.bulletin")


func bind(source: RefCounted) -> void:
	_source = source
	_signature = ""


## Re-read the bulletin, tallies and spends (chart and resources changes).
func refresh() -> void:
	if _source == null or not is_node_ready():
		return
	# A hidden tab reads nothing (service quotes run a rearm on a copy of the ledger); the table
	# refreshes the tab when it opens.
	if not is_visible_in_tree() and not _signature.is_empty():
		_signature = "stale"
		return
	var chart: Dictionary = _source.call(&"chart")
	var cards: Array[Dictionary] = []
	cards.assign(_source.call(&"bulletin"))
	var options: Array[Dictionary] = []
	options.assign(_source.call(&"intel_options"))
	var res: Dictionary = _source.call(&"resources")
	var open: bool = Kit.get_int(chart, "phase", -1) == PHASE_CHART and _source.call(&"is_advancing") != true
	var coop: bool = _source.call(&"is_coop") == true
	var tallies := Kit.dicts(chart.get("tallies"))
	var hour := Kit.get_int(chart, "hour")
	var sig := str([cards, options, Kit.get_int(res, "intel"), open, coop, tallies, hour if not cards.is_empty() else 0, chart.is_empty()])
	if sig == _signature:
		return
	_signature = sig
	render(chart, cards, options, Kit.get_int(res, "intel"), open, coop)


func render(chart: Dictionary, cards: Array[Dictionary], options: Array[Dictionary], intel: int, open: bool, coop: bool) -> void:
	_buttons.clear()
	Kit.clear(_cards)
	Kit.clear(_tallies)
	Kit.clear(_intel)
	if chart.is_empty():
		_pool.text = tr(&"chart.bulletin.no_campaign")
		for box: CanvasItem in [$Column/Cards, $Column/Tallies, $Column/Intel]:
			box.visible = false
		focus_changed.emit()
		return
	for box: CanvasItem in [$Column/Cards, $Column/Tallies, $Column/Intel]:
		box.visible = true
	_pool.text = tr(&"chart.bulletin.pool").format([Kit.number(intel, &"intel", 0)])
	var hour := Kit.get_int(chart, "hour")
	# Cards.
	if cards.is_empty():
		_cards.add_child(Kit.label(tr(&"chart.bulletin.no_cards"), &"Body", true))
	for card: Dictionary in cards:
		_cards.add_child(_card_row(card, options, intel, open, coop, hour))
	# Tallies.
	var tallies := Kit.dicts(chart.get("tallies"))
	_tally_note.text = tr(&"chart.bulletin.tally_note").format([Kit.number(TALLY_CARD_AT, &"none", 0)])
	if tallies.is_empty():
		_tallies.add_child(Kit.label(tr(&"chart.bulletin.no_tallies"), &"Body", true))
	for t: Dictionary in tallies:
		var g: Control = GAUGE_SCENE.instantiate()
		g.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		_tallies.add_child(g)
		g.call(&"configure", {"label": Kit.as_dict(t.get("text")), "unit": &"none", "decimals": 1, "min": 0.0, "max": TALLY_CARD_AT,
			"band_low": 0.0, "band_high": TALLY_CARD_AT * 0.75, "caution_high": TALLY_CARD_AT * 0.75, "trend_waiver": &"chart.bulletin.no_trend", "show_scale": true})
		g.call(&"set_reading", clampf(Kit.get_float(t, "value"), 0.0, TALLY_CARD_AT * 2.0))
	# Other spends.
	var any := false
	for o: Dictionary in options:
		var kind := Kit.get_int(o, "kind", -1)
		if kind != INTEL_REVEAL_CONVOYS and kind != INTEL_REVEAL_ACE:
			continue
		any = true
		var text := ""
		var cost := Kit.get_int(o, "cost")
		if kind == INTEL_REVEAL_CONVOYS:
			text = tr(&"chart.bulletin.reveal_convoys").format([_source.call(&"region_name", StringName(Kit.get_str(o, "region"))), Kit.number(cost, &"intel", 0)])
		else:
			text = tr(&"chart.bulletin.reveal_ace").format([_source.call(&"ace_name", StringName(Kit.get_str(o, "ace"))), Kit.number(cost, &"intel", 0)])
		_intel.add_child(_spend_button(text, o, intel, open, coop))
	if not any:
		_intel.add_child(Kit.label(tr(&"chart.bulletin.no_spends"), &"Body", true))
	focus_changed.emit()


func _card_row(card: Dictionary, options: Array[Dictionary], intel: int, open: bool, coop: bool, hour: int) -> Control:
	var box := Kit.vbox(0)
	var id := StringName(Kit.get_str(card, "id"))
	var region := StringName(Kit.get_str(card, "region"))
	var arrives := Kit.get_int(card, "arrives_h")
	box.add_child(Kit.label(_Text.resolve(StringName(Kit.get_str(card, "name", String(id)))), &"Body", true))
	var when := ""
	if arrives <= 0 or arrives <= hour:
		when = tr(&"chart.bulletin.in_force").format([_source.call(&"region_name", region)])
	else:
		when = tr(&"chart.bulletin.arrives").format([_source.call(&"region_name", region), _dtg(arrives), Kit.number(arrives - hour, &"h", 0)])
	box.add_child(Kit.label(when, &"Dense", true))
	var desc := _Text.resolve(StringName(Kit.get_str(card, "text")))
	if not desc.is_empty():
		box.add_child(Kit.label(desc, &"Dense", true))
	var row := Kit.hbox(1)
	for o: Dictionary in options:
		var kind := Kit.get_int(o, "kind", -1)
		if (kind != INTEL_DELAY_CARD and kind != INTEL_CANCEL_CARD) or StringName(Kit.get_str(o, "card")) != id or StringName(Kit.get_str(o, "region")) != region:
			continue
		var cost := Kit.number(Kit.get_int(o, "cost"), &"intel", 0)
		var text := tr(&"chart.bulletin.delay").format([cost]) if kind == INTEL_DELAY_CARD else tr(&"chart.bulletin.cancel").format([cost])
		var b := _spend_button(text, o, intel, open, coop)
		b.size_flags_horizontal = Control.SIZE_SHRINK_BEGIN
		row.add_child(b)
	if row.get_child_count() > 0:
		box.add_child(row)
	else:
		row.free()
	return box


func _spend_button(text: String, option: Dictionary, intel: int, open: bool, coop: bool) -> Button:
	var b := Kit.inline_button(tr(&"chart.bulletin.propose").format([text]) if coop else text)
	b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	var cost := Kit.get_int(option, "cost")
	b.disabled = not open or cost > intel
	if cost > intel:
		b.tooltip_text = tr(&"chart.bulletin.short").format([Kit.number(cost, &"intel", 0), Kit.number(intel, &"intel", 0)])
	elif not open:
		b.tooltip_text = tr(&"chart.place.busy")
	else:
		b.tooltip_text = tr(&"chart.bulletin.spend_tip")
	var spend := option.duplicate()
	b.pressed.connect(func() -> void:
		if _source != null:
			_source.call(&"propose", {"kind": THEATRE_ACTION_SPEND_INTEL, "intel": spend})
			_signature = ""
			refresh())
	_buttons.append(b)
	return b


func focus_controls() -> Array[Control]:
	var out: Array[Control] = []
	for b: Button in _buttons:
		if is_instance_valid(b) and not b.disabled:
			out.append(b)
	return out


static func _dtg(hour: int) -> String:
	var tx := Kit.autoload(&"Tx")
	if tx != null and tx.has_method(&"date_time_group"):
		return String(tx.call(&"date_time_group", hour))
	return ""
