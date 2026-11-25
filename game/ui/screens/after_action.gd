extends Control
## After-action report with the belief-map overlay.
## DESIGN.md L70. Unit: ui-after-action.
## Reads and calls: Campaign.after_action, Campaign.close_after_action, Hud.objectives, BpPlotView.show_static.
##
## A campaign base screen, opened by the router on the after-action phase (L70: recover, then read
## the report). It shows what the sortie earned and what the Combine learned about you at fleet
## level, never what one enemy remembers:
## - Result: the outcome (title and the bound Meta line beside it), how the sortie ended in plain words
##   (`end_reason`, a MsgDict; every hull disabled for 5 minutes, objective done or failed, every hull
##   lost, time limit, withdrawal), enemy tonnage disabled of the tonnage committed, kills by method, own
##   cells lost and any hull lost. A report that does not say how it ended (the result does not carry the
##   reason yet) shows the primary objective's last text from `Hud.objectives()` instead, which says so
##   for a sortie lost to disabled hulls.
## - Payout: Salvage from wrecks and prizes, Intel, blueprints, fuel and ammunition taken (L811),
##   the repair bill per player and what Salvage is left after it.
## - Ghost conditions: each with Met or Not met as glyph and word, and what it paid.
## - Theatre: the regional Alert after the sortie with its causes (shown rounded down, L749) and the
##   operation clock pushback (L750).
## - Right column: the task-group belief map over the sortie area (BpPlotView in after-action
##   mode), the win-method tallies as gauges against the card threshold of 10, any counter card
##   now queued on the doctrine bulletin with its arrival date-time group and what it costs to delay
##   or cancel (L772), and the theatre events the sortie caused.
## Everything is there at once: no count-ups, fades or staggered reveals (banned-pattern table).
## The one action is Return to the chart table, which takes the first focus. Esc only moves focus to
## it, so a stray key never throws the report away; Up and Down scroll the report. Left of it, a
## quiet Copy playtest report puts the session's playtest report (L2209) on the clipboard for the
## tester's issue form, and says so in a line beside it.
## With no report (a co-op member before the host's arrives) the screen says so and still offers
## the way back.
##
## Narrow windows (UI scale 200%, L1714) stack the right column under the report; the body scrolls
## on one axis.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const GAUGE_SCENE := preload("res://ui/widgets/gauge.tscn")

## `Bp` values used here, kept so the script parses before the extension loads.
const NONE := -1
const PHASE_AFTER_ACTION := 6
## Combine causes of an Alert change, in `Bp.ALERT_CAUSE_*` order; the words are the campaign's.
const CAUSE_KEYS: Array[StringName] = [
	&"campaign.cause.observed_kill", &"campaign.cause.unobserved_kill_noticed", &"campaign.cause.sighting",
	&"campaign.cause.evidence", &"campaign.cause.quiet_decay", &"campaign.cause.act_start", &"campaign.cause.sextant_sunk",
]
## A tally that reaches this queues its counter card (L772).
const TALLY_CARD_AT := 10.0
const STACK_BELOW_PX := 1400.0
const CHART_MIN_H := 400.0
const CLOSE_MIN_W := 360.0
## Theatre events listed at most; the full record is the chart table's theatre log.
const EVENTS_SHOWN := 12

@onready var _title: Label = $Margin/Column/Header/Title
@onready var _outcome: Label = $Margin/Column/Header/Outcome
@onready var _body: BoxContainer = $Margin/Column/Scroll/Body
@onready var _report: VBoxContainer = $Margin/Column/Scroll/Body/Report
@onready var _side: VBoxContainer = $Margin/Column/Scroll/Body/Side
@onready var _chart: Control = $Margin/Column/Scroll/Body/Side/Chart
@onready var _chart_note: Label = $Margin/Column/Scroll/Body/Side/ChartNote
@onready var _doctrine: VBoxContainer = $Margin/Column/Scroll/Body/Side/Doctrine
@onready var _log_rule: HSeparator = $Margin/Column/Scroll/Body/Side/LogRule
@onready var _log_title: Label = $Margin/Column/Scroll/Body/Side/LogTitle
@onready var _log: VBoxContainer = $Margin/Column/Scroll/Body/Side/Log
@onready var _close: Button = $Margin/Column/Footer/Close

var _copy_report: Button
var _copy_status: Label

var _report_data: Dictionary = {}
var _players: Array[Dictionary] = []
var _regions: Array[Dictionary] = []
var _days_left := -1.0
var _objectives: Array[Dictionary] = []
var _live := false


func _ready() -> void:
	Ui.set_margins($Margin, 5)
	Ui.set_separation($Margin/Column, 3)
	Ui.set_separation($Margin/Column/Header, 4)
	Ui.set_separation(_body, 5)
	Ui.set_separation(_report, 2)
	Ui.set_separation(_side, 2)
	Ui.set_separation(_doctrine, 2)
	Ui.set_separation(_log, 0)
	_chart.custom_minimum_size = Vector2(0.0, CHART_MIN_H)
	_close.custom_minimum_size = Vector2(CLOSE_MIN_W, Kit.PRIMARY_H)
	_title.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_close.pressed.connect(_on_close)
	_build_copy_report()
	accessibility_name = tr(&"after_action.accessible_name")
	$Margin/Column/Scroll.accessibility_name = tr(&"after_action.accessible_report")
	_chart.accessibility_name = tr(&"after_action.accessible_chart")
	var scroll: ScrollContainer = $Margin/Column/Scroll
	scroll_by_keys(scroll)
	Kit.chain_vertical([scroll, _close])
	Kit.chain_horizontal([_copy_report, _close])
	_copy_report.focus_neighbor_left = _copy_report.get_path_to(_copy_report)
	_copy_report.focus_neighbor_top = _copy_report.get_path_to(scroll)
	_close.focus_neighbor_right = _close.get_path_to(_close)
	resized.connect(_fit_layout)
	_fit_layout()
	show_report({}, [], [])


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		show_report(_report_data, _players, _regions, _days_left, _objectives)


func setup(_args: Dictionary) -> void:
	_live = true
	var campaign := Kit.autoload(&"Campaign")
	if campaign != null and campaign.has_signal(&"after_action_ready") and not campaign.is_connected(&"after_action_ready", refresh):
		campaign.connect(&"after_action_ready", refresh)
	refresh()


func default_focus() -> Control:
	return _close


## Esc does not dismiss the report (it shows once): it puts focus on Return to the chart table.
func _unhandled_input(event: InputEvent) -> void:
	if event.is_action_pressed(&"ui_cancel", false, true) and Screens.top() == self:
		get_viewport().set_input_as_handled()
		Ui.focus(_close)


func refresh() -> void:
	var regions := Kit.dicts(Kit.as_dict(Kit.call_on(&"Campaign", &"chart")).get("regions"))
	var act := Kit.as_dict(Kit.call_on(&"Campaign", &"act_info"))
	show_report(Kit.as_dict(Kit.call_on(&"Campaign", &"after_action")), Kit.dicts(Kit.call_on(&"Campaign", &"players")), regions, Kit.get_float(act, "clock_days_left", -1.0), Kit.dicts(Kit.call_on(&"Hud", &"objectives")))
	_show_chart()


## Draw an `AfterActionDict` with the campaign's players (`Campaign.players()`) and regions
## (`RegionDict`s, for names). `days_left` is the act clock after the sortie, or below 0 for none.
## `objectives` are the sortie's last `ObjectiveDict`s (`Hud.objectives()`), read only when the report
## does not say how the sortie ended.
## Also the test and capture entry point: it calls no facade except Tx and Content for formatting.
func show_report(report: Dictionary, players: Array[Dictionary], regions: Array[Dictionary], days_left: float = -1.0, objectives: Array[Dictionary] = []) -> void:
	_report_data = report
	_players = players
	_regions = regions
	_days_left = days_left
	_objectives = objectives
	Kit.clear(_report)
	Kit.clear(_doctrine)
	Kit.clear(_log)
	_side.visible = not report.is_empty()
	if report.is_empty():
		_title.text = tr(&"after_action.title.none")
		_outcome.call(&"show_value", "")
		_paragraph(_report, tr(&"after_action.empty"))
		_chart_note.visible = false
		_paragraph(_doctrine, tr(&"after_action.doctrine.none"))
		_log_rule.visible = false
		_log_title.visible = false
		return
	_title.text = operation_title(StringName(Kit.get_str(report, "operation")))
	_outcome.call(&"show_value", _tx_str(&"word", [&"outcome", Kit.get_int(report, "outcome")]))
	_chart_note.visible = true
	_build_result(report)
	_report.add_child(HSeparator.new())
	_build_payout(report)
	var ghosts := Kit.dicts(report.get("ghosts"))
	if not ghosts.is_empty():
		_report.add_child(HSeparator.new())
		_build_ghosts(ghosts)
	_report.add_child(HSeparator.new())
	_build_theatre(report)
	_build_doctrine(report)
	_build_log(Kit.dicts(report.get("events")))


## The operation's name ("Prize capture") from its id's name key, or a plain title for a sortie
## with no operation or a name this build does not know.
static func operation_title(op: StringName) -> String:
	if op != &"":
		var key := StringName(String(op) + ".name")
		var text := String(TranslationServer.translate(key))
		if text != String(key) and not text.is_empty():
			return text
	return String(TranslationServer.translate(&"after_action.title.plain"))


func _section(parent: Control, key: StringName) -> void:
	parent.add_child(Kit.label(tr(key), &"PanelTitle", true))


func _paragraph(parent: Control, text: String) -> void:
	if text.is_empty():
		return
	var l := Kit.label(text, &"Body", true)
	l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	parent.add_child(l)


## A two-column table: term (Dense) and value (Body). Rows with an empty value are left out.
func _facts(parent: Control, rows: Array) -> void:
	var grid := GridContainer.new()
	grid.columns = 2
	grid.add_theme_constant_override(&"h_separation", Ui.spacing(4))
	grid.add_theme_constant_override(&"v_separation", Ui.spacing(0))
	for row: Variant in rows:
		if not row is Array or (row as Array).size() < 2 or String((row as Array)[1]).is_empty():
			continue
		var term := Kit.label(String((row as Array)[0]), &"Dense", true)
		term.autowrap_mode = TextServer.AUTOWRAP_OFF
		grid.add_child(term)
		var value := Kit.label(String((row as Array)[1]), &"Body", true)
		value.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		grid.add_child(value)
	if grid.get_child_count() > 0:
		parent.add_child(grid)
	else:
		grid.free()


# --- result --------------------------------------------------------------------------------------

func _build_result(report: Dictionary) -> void:
	_section(_report, &"after_action.section.result")
	var stats := Kit.as_dict(report.get("stats"))
	var rows: Array = [[tr(&"after_action.result.outcome"), _tx_str(&"word", [&"outcome", Kit.get_int(report, "outcome", NONE)])]]
	rows.append_array(end_rows(report, _objectives))
	rows.append_array(result_rows(stats, Kit.dicts(report.get("kills_by_method")), _lost_names(report)))
	_facts(_report, rows)


## How the sortie ended, as one `[term, value]` row: the report's `end_reason` sentence; without one,
## the primary objective's last text (the first non-ghost `ObjectiveDict`); with neither, no row.
static func end_rows(report: Dictionary, objectives: Array[Dictionary]) -> Array:
	var why := _Text.resolve(report.get("end_reason"))
	if not why.is_empty():
		return [[String(TranslationServer.translate(&"after_action.result.ended")), why]]
	for o: Dictionary in objectives:
		if not Kit.get_bool(o, "ghost"):
			var text := _Text.resolve(o.get("text"))
			if not text.is_empty():
				return [[String(TranslationServer.translate(&"after_action.result.objective")), text]]
			break
	return []


## Result rows `[term, value]`: tonnage disabled, kills by method, cells lost, hulls lost.
static func result_rows(stats: Dictionary, kills: Array[Dictionary], lost_names: PackedStringArray) -> Array:
	var t := func(k: StringName) -> String: return String(TranslationServer.translate(k))
	var rows: Array = []
	var total := Kit.get_float(stats, "enemy_tonnage_total_t")
	var disabled := Kit.get_float(stats, "enemy_tonnage_disabled_t")
	if total > 0.0:
		rows.append([t.call(&"after_action.result.tonnage"), t.call(&"after_action.result.tonnage_value").format([
			Kit.number(disabled, &"t", 0), Kit.number(total, &"t", 0), Kit.percent(clampf(disabled / total, 0.0, 1.0))])])
	rows.append([t.call(&"after_action.result.kills"), kills_text(kills)])
	var struck := Kit.get_int(stats, "enemy_struck")
	if struck > 0:
		rows.append([t.call(&"after_action.result.struck"), Kit.number(struck, &"none", 0)])
	rows.append([t.call(&"after_action.result.cells"), Kit.percent(clampf(Kit.get_float(stats, "own_cells_lost"), 0.0, 1.0))])
	if not lost_names.is_empty():
		rows.append([t.call(&"after_action.result.hulls_lost"), ", ".join(lost_names)])
	return rows


## "Gunfire 2, capture 1", most kills first; "None" without kills.
static func kills_text(kills: Array[Dictionary]) -> String:
	var rows := kills.duplicate()
	rows.sort_custom(func(a: Dictionary, b: Dictionary) -> bool:
		var ca := Kit.get_int(a, "count")
		var cb := Kit.get_int(b, "count")
		return ca > cb if ca != cb else Kit.get_int(a, "method") < Kit.get_int(b, "method"))
	var parts := PackedStringArray()
	for k: Dictionary in rows:
		var n := Kit.get_int(k, "count")
		if n <= 0:
			continue
		parts.append(String(TranslationServer.translate(&"after_action.result.kill_entry")).format([
			_tx_str(&"word", [&"kill", Kit.get_int(k, "method")]), Kit.number(n, &"none", 0)]))
	if parts.is_empty():
		return String(TranslationServer.translate(&"after_action.result.no_kills"))
	return ", ".join(parts)


func _player_name(slot: int) -> String:
	for p: Dictionary in _players:
		if Kit.get_int(p, "player", NONE) == slot:
			var n := Kit.get_str(p, "name")
			if not n.is_empty():
				return n
	return tr(&"after_action.player").format([Kit.number(slot + 1, &"none", 0)])


func _lost_names(report: Dictionary) -> PackedStringArray:
	var out := PackedStringArray()
	var lost: Variant = report.get("hulls_lost")
	if lost is PackedInt32Array or lost is Array:
		for slot: Variant in lost:
			out.append(_player_name(int(slot)))
	return out


# --- payout --------------------------------------------------------------------------------------

func _build_payout(report: Dictionary) -> void:
	_section(_report, &"after_action.section.payout")
	var p := Kit.as_dict(report.get("payout"))
	var repairs: Array = []
	for r: Dictionary in Kit.dicts(report.get("repair_cost")):
		repairs.append([_player_name(Kit.get_int(r, "player")), Kit.get_int(r, "salvage")])
	_facts(_report, payout_rows(p, repairs))


## Payout rows `[term, value]`. `repairs` lists `[player name, salvage]`. Salvage rows always show;
## the rest only when they are not zero.
static func payout_rows(p: Dictionary, repairs: Array) -> Array:
	var t := func(k: StringName) -> String: return String(TranslationServer.translate(k))
	var rows: Array = []
	var wrecks := Kit.get_int(p, "salvage_wrecks")
	var prizes := Kit.get_int(p, "salvage_prizes")
	rows.append([t.call(&"after_action.payout.wrecks"), Kit.number(wrecks, &"salvage", 0)])
	rows.append([t.call(&"after_action.payout.prizes"), Kit.number(prizes, &"salvage", 0)])
	var intel := Kit.get_int(p, "intel")
	if intel > 0:
		rows.append([t.call(&"after_action.payout.intel"), Kit.number(intel, &"intel", 0)])
	var names := PackedStringArray()
	for id: Variant in Kit.as_array(p.get("blueprints")):
		names.append(part_name(StringName(id)))
	if not names.is_empty():
		rows.append([t.call(&"after_action.payout.blueprints"), ", ".join(names)])
	var fuel := Kit.get_float(p, "fuel_l")
	if fuel > 0.0:
		rows.append([t.call(&"after_action.payout.fuel"), Kit.number(fuel, &"l", 0)])
	var ammo := Kit.get_float(p, "ammo_kg")
	if ammo > 0.0:
		rows.append([t.call(&"after_action.payout.ammo"), t.call(&"after_action.payout.kg").format([Kit.number(ammo, &"none", 0)])])
	var bill := 0
	var bill_parts := PackedStringArray()
	for r: Variant in repairs:
		var pair := r as Array
		if pair == null or pair.size() < 2 or int(pair[1]) <= 0:
			continue
		bill += int(pair[1])
		bill_parts.append(t.call(&"after_action.payout.repair_entry").format([String(pair[0]), Kit.number(int(pair[1]), &"salvage", 0)]))
	rows.append([t.call(&"after_action.payout.repairs"), ", ".join(bill_parts) if not bill_parts.is_empty() else t.call(&"after_action.payout.no_repairs")])
	var net := wrecks + prizes - bill
	var key := &"after_action.payout.net_gain" if net >= 0 else &"after_action.payout.net_loss"
	rows.append([t.call(&"after_action.payout.net"), t.call(key).format([Kit.number(absi(net), &"salvage", 0)])])
	return rows


## A part's display name from Content, or its id when Content does not know it.
static func part_name(id: StringName) -> String:
	var d := Kit.as_dict(Kit.call_on(&"Content", &"part", [id]))
	var key := StringName(Kit.get_str(d, "name"))
	return String(TranslationServer.translate(key)) if key != &"" else String(id)


# --- ghosts --------------------------------------------------------------------------------------

func _build_ghosts(ghosts: Array[Dictionary]) -> void:
	_section(_report, &"after_action.section.ghosts")
	for g: Dictionary in ghosts:
		var met := Kit.get_bool(g, "met")
		var row := Kit.hbox(2)
		var mark := Kit.hbox(1)
		mark.custom_minimum_size = Vector2(Ui.spacing(6) * 2.75, 0.0)
		var word := Kit.label(tr(&"after_action.ghost.met") if met else tr(&"after_action.ghost.missed"), &"SeverityPass" if met else &"SeverityAdvisory", true)
		word.autowrap_mode = TextServer.AUTOWRAP_OFF
		mark.add_child(word)
		if met:
			var icon := TextureRect.new()
			icon.texture = Ui.icon(&"check_pass")
			icon.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
			icon.stretch_mode = TextureRect.STRETCH_KEEP_ASPECT_CENTERED
			icon.custom_minimum_size = Vector2(24.0, 24.0)
			icon.size_flags_vertical = Control.SIZE_SHRINK_CENTER
			mark.add_child(icon)
		row.add_child(mark)
		var l := Kit.label(_Text.resolve(g.get("text")), &"Body", true)
		l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		row.add_child(l)
		# What it paid, or would have paid; the verdict beside it says which.
		var reward := Kit.label(_Text.resolve(g.get("reward")), &"Body", true)
		reward.autowrap_mode = TextServer.AUTOWRAP_OFF
		reward.horizontal_alignment = HORIZONTAL_ALIGNMENT_RIGHT
		row.add_child(reward)
		_report.add_child(row)


# --- theatre -------------------------------------------------------------------------------------

func _build_theatre(report: Dictionary) -> void:
	_section(_report, &"after_action.section.theatre")
	var lines := alert_lines(Kit.dicts(report.get("alert_changes")), _regions)
	if lines.is_empty():
		_paragraph(_report, tr(&"after_action.alert.none"))
	for line: String in lines:
		_paragraph(_report, line)
	var days := Kit.get_float(report, "clock_pushback_days")
	if days > 0.0:
		var shown := Kit.number(days, &"none", 0 if is_equal_approx(days, roundf(days)) else 1)
		var key := &"after_action.clock.pushed_one" if is_equal_approx(days, 1.0) else &"after_action.clock.pushed"
		var line := tr(key).format([shown])
		if _days_left >= 0.0:
			line = tr(&"after_action.clock.with_left").format([line, Kit.number(floorf(_days_left), &"none", 0)])
		_paragraph(_report, line)
	elif _days_left >= 0.0:
		# Every success pushes the clock back at least a day (L750), so none means the objective failed.
		_paragraph(_report, tr(&"after_action.clock.not_done").format([Kit.number(floorf(_days_left), &"none", 0)]))


## One line per region: its Alert after the sortie (rounded down, L749) and the causes, in the
## order they happened: "Gannet Banks: Alert 2 after sighting, observed kill."
static func alert_lines(changes: Array[Dictionary], regions: Array[Dictionary]) -> PackedStringArray:
	var order: Array[StringName] = []
	var last := {}
	var causes := {}
	for c: Dictionary in changes:
		var r := StringName(Kit.get_str(c, "region"))
		if not order.has(r):
			order.append(r)
			causes[r] = PackedStringArray()
		last[r] = Kit.get_float(c, "alert")
		var cause := Kit.get_int(c, "cause", NONE)
		if cause >= 0 and cause < CAUSE_KEYS.size():
			var word := String(TranslationServer.translate(CAUSE_KEYS[cause]))
			var list: PackedStringArray = causes[r]
			if not list.has(word):
				list.append(word)
				causes[r] = list
	var out := PackedStringArray()
	for r: StringName in order:
		var alert := clampi(int(floorf(float(last[r]))), 0, 5)
		var list: PackedStringArray = causes[r]
		var key := &"after_action.alert.line" if not list.is_empty() else &"after_action.alert.line_plain"
		out.append(String(TranslationServer.translate(key)).format([region_name(r, regions), Kit.number(alert, &"none", 0), ", ".join(list)]))
	return out


static func region_name(id: StringName, regions: Array[Dictionary]) -> String:
	for r: Dictionary in regions:
		if StringName(Kit.get_str(r, "id")) == id:
			var key := StringName(Kit.get_str(r, "name"))
			if key != &"":
				return String(TranslationServer.translate(key))
	return String(id)


# --- doctrine and log (right column) -------------------------------------------------------------

func _build_doctrine(report: Dictionary) -> void:
	var tallies := Kit.dicts(report.get("tallies"))
	if tallies.is_empty():
		_paragraph(_doctrine, tr(&"after_action.doctrine.no_tallies"))
	else:
		_paragraph(_doctrine, tr(&"after_action.doctrine.lead").format([Kit.number(TALLY_CARD_AT, &"none", 0)]))
		var sorted := tallies.duplicate()
		sorted.sort_custom(func(a: Dictionary, b: Dictionary) -> bool: return Kit.get_float(a, "value") > Kit.get_float(b, "value"))
		for tally: Dictionary in sorted:
			var g: Control = GAUGE_SCENE.instantiate()
			g.size_flags_horizontal = Control.SIZE_EXPAND_FILL
			_doctrine.add_child(g)
			var value := maxf(Kit.get_float(tally, "value"), 0.0)
			g.call(&"configure", {
				"label": tally.get("text", {}), "unit": &"none", "decimals": 1, "min": 0.0,
				"max": maxf(TALLY_CARD_AT * 1.2, ceilf(value)), "band_low": 0.0, "band_high": TALLY_CARD_AT,
				"caution_high": TALLY_CARD_AT, "trend_waiver": &"after_action.doctrine.waiver",
			})
			g.call(&"set_reading", value)
	var cards := Kit.dicts(report.get("queued_cards"))
	if cards.is_empty():
		_paragraph(_doctrine, tr(&"after_action.cards.none"))
		return
	for c: Dictionary in cards:
		_paragraph(_doctrine, tr(&"after_action.cards.queued").format([
			tr(StringName(Kit.get_str(c, "name"))), region_name(StringName(Kit.get_str(c, "region")), _regions),
			_tx_str(&"date_time_group", [Kit.get_int(c, "arrives_h")])]))
		var desc := StringName(Kit.get_str(c, "text"))
		if desc != &"":
			_paragraph(_doctrine, tr(desc))
	_paragraph(_doctrine, tr(&"after_action.cards.counter"))


func _build_log(events: Array[Dictionary]) -> void:
	_log_rule.visible = not events.is_empty()
	_log_title.visible = not events.is_empty()
	if events.is_empty():
		return
	var rows: Array = []
	for e: Dictionary in events.slice(maxi(events.size() - EVENTS_SHOWN, 0)):
		var text := _Text.resolve(e.get("text"))
		if text.is_empty():
			continue
		rows.append([_tx_str(&"date_time_group", [Kit.get_int(e, "hour")]), text])
	_facts(_log, rows)


# --- chart ---------------------------------------------------------------------------------------

## The belief samples over the sortie map. The map comes from the operation's briefing while the
## campaign still offers it; otherwise the plot draws the samples on open water.
func _show_chart() -> void:
	if not _live or _report_data.is_empty() or not _chart.has_method(&"show_static"):
		return
	var belief: Variant = _report_data.get("belief")
	var op := StringName(Kit.get_str(_report_data, "operation"))
	var map := {}
	if op != &"":
		map = Kit.as_dict(Kit.as_dict(Kit.call_on(&"Campaign", &"briefing", [op])).get("map"))
	_chart.call(&"set_mode", &"after_action")
	_chart.call(&"show_static", map, PackedVector2Array(), belief if belief is PackedVector3Array else PackedVector3Array())


# --- actions -------------------------------------------------------------------------------------

## Copy playtest report and its status line, at the footer's start (the screen is the end of a
## core loop, when the report has just been written).
func _build_copy_report() -> void:
	var footer: HBoxContainer = $Margin/Column/Footer
	_copy_report = Kit.inline_button("settings_ui.playtest.copy_report")
	_copy_report.name = &"CopyReport"
	_copy_report.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	_copy_report.pressed.connect(copy_report)
	_copy_status = Kit.label("", &"Body", true)
	_copy_status.name = &"CopyStatus"
	# It wraps in the room the footer has left, so the footer never widens the screen at 200% (L1713).
	_copy_status.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_copy_status.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	_copy_status.visible = false
	footer.add_child(_copy_report)
	footer.move_child(_copy_report, 0)
	footer.add_child(_copy_status)
	footer.move_child(_copy_status, 1)


## Put the playtest report on the clipboard (nothing is sent) and say whether there was one.
func copy_report() -> void:
	var text: Variant = Kit.call_on(&"App", &"playtest_report_text")
	var ok := text is String and not (text as String).is_empty()
	if ok and DisplayServer.get_name() != "headless":
		DisplayServer.clipboard_set(text)
	_copy_status.text = tr(&"settings_ui.playtest.report_copied" if ok else &"settings_ui.playtest.nothing_to_copy")
	_copy_status.visible = true


func _on_close() -> void:
	if _live:
		Kit.call_on(&"Campaign", &"close_after_action")


func _fit_layout() -> void:
	if not is_node_ready():
		return
	var stacked := size.x < STACK_BELOW_PX
	_body.vertical = stacked
	_close.custom_minimum_size = Vector2(0.0 if stacked else CLOSE_MIN_W, Kit.PRIMARY_H)


## Make a scroll a focus stop that moves with Up and Down (keys, D-pad, stick) and pages with Page Up
## and Page Down, so everything in it reads without a mouse (L1738). At either end the press is left
## alone, so focus moves on to the neighbour as usual.
static func scroll_by_keys(scroll: ScrollContainer) -> void:
	scroll.focus_mode = Control.FOCUS_ALL
	scroll.gui_input.connect(func(event: InputEvent) -> void:
		var line := float(Ui.font_size(&"Body")) * 3.0
		var delta := 0.0
		if event.is_action_pressed(&"ui_down", true, true):
			delta = line
		elif event.is_action_pressed(&"ui_up", true, true):
			delta = -line
		elif event.is_action_pressed(&"ui_page_down", true, true):
			delta = scroll.size.y * 0.9
		elif event.is_action_pressed(&"ui_page_up", true, true):
			delta = -scroll.size.y * 0.9
		if delta == 0.0:
			return
		var before := scroll.scroll_vertical
		scroll.scroll_vertical = before + int(delta)
		if scroll.scroll_vertical != before:
			scroll.accept_event())


static func _tx_str(method: StringName, args: Array) -> String:
	var v: Variant = Kit.call_on(&"Tx", method, args)
	return String(v) if v is String or v is StringName else ""
