extends Control
## Briefing in orders format with the route chart, tags and Fit Check summary.
## DESIGN.md L66, L1704. Unit: ui-briefing.
## Reads and calls: Campaign.briefing, Campaign.open_briefing, Campaign.launch_sortie, Campaign.briefing_fit, BpPlotView.show_static.
##
## A campaign base screen: the router opens it when the campaign phase becomes briefing, with no
## arguments, so the operation comes from `Campaign.briefed_operation()`. `setup({ "operation": id })`
## also works and opens the briefing first when the phase is not briefing yet.
##
## Layout (L1704 orders format, L801): the operation name as the screen title with its date-time
## group beside it (the one bound Meta line, L1578). The left column reads as an order:
## - Situation: the briefing's situation paragraph, the forecast (the sortie's own weather, L743)
##   as a short fact table, and the threat picture graded by confidence, confirmed first, with the
##   Combine's committed force.
## - Mission: the mission paragraph, the two to four constraint tags, each with its Fit Check
##   verdict as glyph and word once Fit Check has run (L1689), and the optional ghost conditions
##   with what they pay.
## - Execution: the execution paragraph and the facts the player plans by: approach distance,
##   station actions, the recommended emission control, clock pushback and when the offer lapses.
## The right column holds the route chart (BpPlotView in briefing mode), the Fit Check summary
## (Campaign.briefing_fit, which runs Fit Check on the fielded hull as the briefing opens: speed, 180
## degree turn, range, detection against the known threats, the tag count and the seakeeping sweep's
## verdict: safe, can roll over in that sea and from where, or not finished) and, in co-op, who has
## accepted the operation (L839).
##
## The briefing text is one translated paragraph that starts each part with a marker word
## ("Situation:", "Mission:", "Execution:"). The markers are text keys of this screen, so a
## translation splits the same way; text whose markers are missing shows whole under Situation.
##
## Launch sortie is the most-used control: the tallest button, bottom right, first focus. Back to
## the chart table keeps the operation accepted; Decline asks first and is offered only for a
## template operation that is not forced (the theatre keeps story operations, ace duels and the
## capstone on offer whatever the player says). Open the dry dock is where Fit Check and the Quartermaster run against
## this briefing; the dock returns here. A co-op member cannot launch (the host does) and says so.
## Esc goes back to the chart table. A refused launch or decline shows the reason in the status
## line under the title, never in a toast.
##
## Narrow windows (UI scale 200%, L1714) stack the chart column under the order so nothing clips;
## the whole body scrolls on one axis.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const _FitCheck := preload("res://ui/editor/fit_check.gd")

## `Bp` values used here, kept so the script parses before the extension loads.
const NONE := -1
const PHASE_BRIEFING := 3
const JOB_IDLE := 0
const JOB_RUNNING := 1
const JOB_DONE := 2
const JOB_FAILED := 3
const JOB_STALE := 4
const THEATRE_ACTION_ACCEPT_OPERATION := 3
const THEATRE_ACTION_DECLINE_OPERATION := 4
const OPERATION_TEMPLATE := 0
const CHECK_PASS := 0
const CHECK_FAIL := 2

## Below this width (logical pixels) the chart column moves under the order.
const STACK_BELOW_PX := 1400.0
const CHART_MIN_H := 480.0
const LAUNCH_MIN_W := 320.0

@onready var _title: Label = $Margin/Column/Header/Title
@onready var _dtg: Label = $Margin/Column/Header/Dtg
@onready var _status: Label = $Margin/Column/Status
@onready var _body: BoxContainer = $Margin/Column/Scroll/Body
@onready var _orders: VBoxContainer = $Margin/Column/Scroll/Body/Orders
@onready var _side: VBoxContainer = $Margin/Column/Scroll/Body/Side
@onready var _chart: Control = $Margin/Column/Scroll/Body/Side/Chart
@onready var _fit: VBoxContainer = $Margin/Column/Scroll/Body/Side/Fit
@onready var _coop_rule: HSeparator = $Margin/Column/Scroll/Body/Side/CoopRule
@onready var _coop_title: Label = $Margin/Column/Scroll/Body/Side/CoopTitle
@onready var _coop: VBoxContainer = $Margin/Column/Scroll/Body/Side/Coop
@onready var _back: Button = $Margin/Column/Footer/Back
@onready var _decline: Button = $Margin/Column/Footer/Decline
@onready var _dock: Button = $Margin/Column/Footer/Dock
@onready var _launch: Button = $Margin/Column/Footer/Launch

var _operation: StringName = &""
var _briefing: Dictionary = {}
var _fit_result: Dictionary = {}
## True once `setup` connected to the facades (the compile check never calls it).
var _live := false
var _member := false
var _busy := false
var _coop_buttons: Array[Control] = []


func _ready() -> void:
	Ui.set_margins($Margin, 5)
	Ui.set_separation($Margin/Column, 3)
	Ui.set_separation($Margin/Column/Header, 4)
	Ui.set_separation(_body, 5)
	Ui.set_separation(_orders, 2)
	Ui.set_separation(_side, 2)
	Ui.set_separation(_fit, 1)
	Ui.set_separation(_coop, 1)
	Ui.set_separation($Margin/Column/Footer, 3)
	_chart.custom_minimum_size = Vector2(0.0, CHART_MIN_H)
	_launch.custom_minimum_size = Vector2(LAUNCH_MIN_W, Kit.PRIMARY_H)
	for b: Button in [_back, _decline, _dock]:
		b.custom_minimum_size = Vector2(0.0, Kit.BUTTON_H)
	for l: Label in [_title, _status]:
		l.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_status.text = ""
	_status.visible = false
	_back.pressed.connect(_on_back)
	_decline.pressed.connect(_on_decline)
	_dock.pressed.connect(_on_dock)
	_launch.pressed.connect(_on_launch)
	accessibility_name = tr(&"briefing.accessible_name")
	$Margin/Column/Scroll.accessibility_name = tr(&"briefing.accessible_orders")
	_chart.accessibility_name = tr(&"briefing.accessible_chart")
	scroll_by_keys($Margin/Column/Scroll)
	resized.connect(_fit_layout)
	# The route chart fits the map to its own size: fit again once layout has given it one.
	_chart.resized.connect(_show_chart)
	_fit_layout()
	show_briefing({}, {})


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		show_briefing(_briefing, _fit_result)


func setup(args: Dictionary) -> void:
	_live = true
	_member = _is_member()
	var campaign := Kit.autoload(&"Campaign")
	if campaign != null:
		_connect(campaign, &"chart_changed", _on_chart_changed)
		_connect(campaign, &"proposal_changed", _on_proposal_changed)
		_connect(campaign, &"rejected", _on_rejected)
		if campaign.has_signal(&"briefing_fit_changed"):
			_connect(campaign, &"briefing_fit_changed", _on_fit_changed)
	var wanted := StringName(Kit.get_str(args, "operation"))
	if wanted != &"" and Kit.call_on(&"Campaign", &"phase") != PHASE_BRIEFING:
		# Opening moves the phase; the router then keeps this screen, already the base.
		Kit.call_on(&"Campaign", &"open_briefing", [wanted])
	refresh()


func default_focus() -> Control:
	return _launch if _launch.visible and not _launch.disabled else _back


func _unhandled_input(event: InputEvent) -> void:
	if event.is_action_pressed(&"ui_cancel", false, true) and Screens.top() == self:
		get_viewport().set_input_as_handled()
		_on_back()


## Pull the open briefing and the Fit Check result from the facades and draw them.
func refresh() -> void:
	var op: Variant = Kit.call_on(&"Campaign", &"briefed_operation")
	var had := _operation
	_operation = StringName(op) if op is StringName or op is String else &""
	if _live and had != &"" and _operation == &"" and Kit.call_on(&"Campaign", &"phase") == PHASE_BRIEFING:
		# The operation left the table under this screen (a co-op decline committed): back to the chart.
		Kit.call_on(&"Campaign", &"close_briefing")
		return
	var b: Variant = Kit.call_on(&"Campaign", &"briefing", [_operation]) if _operation != &"" else null
	var fit: Variant = Kit.call_on(&"Campaign", &"briefing_fit") if _operation != &"" else null
	show_briefing(Kit.as_dict(b), Kit.as_dict(fit))
	_show_chart()
	_show_coop()


## Draw a `BriefingDict` and a `FitResultDict` (either may be empty). Also the test and capture
## entry point: it calls no facade except Tx and Content for formatting.
func show_briefing(briefing: Dictionary, fit: Dictionary) -> void:
	_briefing = briefing
	_fit_result = fit
	Kit.clear(_orders)
	Kit.clear(_fit)
	_side.visible = not briefing.is_empty()
	if briefing.is_empty():
		_show_empty()
		return
	var name_key := StringName(Kit.get_str(briefing, "name"))
	_title.text = tr(name_key) if name_key != &"" else tr(&"briefing.title.unnamed")
	_dtg.call(&"show_value", Kit.get_str(briefing, "dtg"))
	var parts := split_orders(tr(StringName(Kit.get_str(briefing, "text"))), [
		tr(&"briefing.marker.situation"), tr(&"briefing.marker.mission"), tr(&"briefing.marker.execution")])
	_build_situation(briefing, parts[0])
	_orders.add_child(HSeparator.new())
	_build_mission(briefing, parts[1], fit)
	_orders.add_child(HSeparator.new())
	_build_execution(briefing, parts[2])
	_build_fit(briefing, fit)
	_decline.visible = can_decline(briefing)
	_launch.visible = true
	_launch.disabled = _member
	_dock.visible = true
	if _member:
		_say(tr(&"briefing.status.member"))
	_link_focus()


func _show_empty() -> void:
	_title.text = tr(&"briefing.title.none")
	_dtg.call(&"show_value", "")
	_paragraph(_orders, tr(&"briefing.empty"))
	_paragraph(_fit, tr(&"briefing.fit.no_briefing"))
	_decline.visible = false
	_launch.visible = false
	_dock.visible = false
	_coop_rule.visible = false
	_coop_title.visible = false
	_coop.visible = false
	_link_focus()


# --- orders --------------------------------------------------------------------------------------

## Split orders-format text at its three markers, in order. Returns three paragraphs with the
## markers removed; when any marker is missing or out of order the whole text is the first part.
static func split_orders(text: String, markers: Array) -> PackedStringArray:
	var out := PackedStringArray(["", "", ""])
	var at: Array[int] = []
	var from := 0
	for m: Variant in markers:
		var marker := String(m)
		var i := text.find(marker, from) if not marker.is_empty() else -1
		if i < 0 or at.size() >= 3:
			out[0] = text.strip_edges()
			return out
		at.append(i)
		from = i + marker.length()
	if at.size() != 3:
		out[0] = text.strip_edges()
		return out
	for k in 3:
		var start := at[k] + String(markers[k]).length()
		var end := at[k + 1] if k < 2 else text.length()
		out[k] = sentence_start(text.substr(start, end - start).strip_edges())
	return out


## The first letter in upper case: a part cut after its marker starts a sentence of its own.
static func sentence_start(s: String) -> String:
	if s.is_empty():
		return s
	return s.left(1).to_upper() + s.substr(1)


func _section(title_key: StringName) -> void:
	_orders.add_child(Kit.label(tr(title_key), &"PanelTitle", true))


func _paragraph(parent: Control, text: String) -> void:
	if text.is_empty():
		return
	var l := Kit.label(text, &"Body", true)
	l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	parent.add_child(l)


## A two-column fact table: term (Dense) and value (Body), rows of `[term, value]`. Rows with an
## empty value are left out.
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


func _build_situation(b: Dictionary, paragraph: String) -> void:
	_section(&"briefing.section.situation")
	_paragraph(_orders, paragraph)
	_paragraph(_orders, area_text(b))
	_facts(_orders, forecast_rows(Kit.as_dict(b.get("weather")), Kit.get_int(b, "time_of_day", NONE)))
	var threats := sort_threats(Kit.dicts(b.get("threats")))
	if threats.is_empty():
		_paragraph(_orders, tr(&"briefing.threat.none"))
	else:
		_paragraph(_orders, tr(&"briefing.threat.lead"))
		_orders.add_child(_threat_table(threats))
	var hulls := Kit.get_int(b, "committed_hulls")
	if hulls > 0:
		var key := &"briefing.threat.committed_one" if hulls == 1 else &"briefing.threat.committed"
		_paragraph(_orders, tr(key).format([Kit.number(hulls, &"none", 0), Kit.number(Kit.get_float(b, "committed_tonnage_t"), &"t", 0)]))


## "Off Crane Head, Gannet Banks." from the node and region names, or "" when neither is known.
static func area_text(b: Dictionary) -> String:
	var node := StringName(Kit.get_str(b, "node"))
	var region := StringName(Kit.get_str(b, "region"))
	var node_name := ""
	var region_name := ""
	for n: Dictionary in Kit.dicts(Kit.as_dict(Kit.call_on(&"Content", &"theatre_graph")).get("nodes")):
		if StringName(Kit.get_str(n, "id")) == node:
			node_name = String(TranslationServer.translate(StringName(Kit.get_str(n, "name"))))
	for r: Dictionary in Kit.dicts(Kit.as_dict(Kit.call_on(&"Campaign", &"chart")).get("regions")):
		if StringName(Kit.get_str(r, "id")) == region:
			region_name = String(TranslationServer.translate(StringName(Kit.get_str(r, "name"))))
	if node_name.is_empty() and region_name.is_empty():
		return ""
	if node_name.is_empty():
		return String(TranslationServer.translate(&"briefing.area.region")).format([region_name])
	if region_name.is_empty():
		return String(TranslationServer.translate(&"briefing.area.node")).format([node_name])
	return String(TranslationServer.translate(&"briefing.area.both")).format([node_name, region_name])


## Forecast rows `[term, value]` from a `WeatherDict` and a `Bp.TIME_*` value.
static func forecast_rows(w: Dictionary, time_of_day: int) -> Array:
	var t := func(k: StringName) -> String: return String(TranslationServer.translate(k))
	if w.is_empty():
		return [[t.call(&"briefing.forecast.weather"), t.call(&"briefing.forecast.unknown")]]
	var wave := Kit.number(Kit.get_float(w, "wave_height_m"), &"m", 1)
	var sea: String = t.call(&"briefing.forecast.sea_value").format([Kit.number(Kit.get_int(w, "sea_state"), &"none", 0), wave])
	var wind_kn := Kit.get_float(w, "wind_kn")
	var wind: String = t.call(&"briefing.forecast.calm")
	if wind_kn >= 0.5:
		wind = t.call(&"briefing.forecast.wind_value").format([_tx_str(&"bearing", [Kit.get_float(w, "wind_from_deg")]), Kit.number(wind_kn, &"kn", 0)])
	return [
		[t.call(&"briefing.forecast.weather"), _tx_str(&"word", [&"weather", Kit.get_int(w, "kind")])],
		[t.call(&"briefing.forecast.light"), _tx_str(&"word", [&"time", time_of_day]) if time_of_day >= 0 else ""],
		[t.call(&"briefing.forecast.sea"), sea],
		[t.call(&"briefing.forecast.wind"), wind],
		[t.call(&"briefing.forecast.visibility"), _tx_str(&"range", [Kit.get_float(w, "visibility_m")])],
	]


## Threat entries graded by confidence: confirmed first, then probable, then possible; within a
## grade the larger force first, then by class id so the order never shuffles.
static func sort_threats(threats: Array[Dictionary]) -> Array[Dictionary]:
	var out := threats.duplicate()
	out.sort_custom(func(a: Dictionary, b: Dictionary) -> bool:
		var ca := Kit.get_int(a, "confidence")
		var cb := Kit.get_int(b, "confidence")
		if ca != cb:
			return ca > cb
		var na := Kit.get_int(a, "count_max")
		var nb := Kit.get_int(b, "count_max")
		if na != nb:
			return na > nb
		return Kit.get_str(a, "class_id") < Kit.get_str(b, "class_id"))
	return out


## "2", or "2 to 3" when the count is uncertain.
static func count_text(entry: Dictionary) -> String:
	var lo := maxi(Kit.get_int(entry, "count_min"), 0)
	var hi := maxi(Kit.get_int(entry, "count_max"), lo)
	if hi == lo:
		return Kit.number(lo, &"none", 0)
	return String(TranslationServer.translate(&"briefing.threat.range")).format([Kit.number(lo, &"none", 0), Kit.number(hi, &"none", 0)])


func _threat_table(threats: Array[Dictionary]) -> GridContainer:
	var grid := GridContainer.new()
	grid.columns = 3
	grid.add_theme_constant_override(&"h_separation", Ui.spacing(4))
	grid.add_theme_constant_override(&"v_separation", Ui.spacing(0))
	for key: StringName in [&"briefing.threat.col_confidence", &"briefing.threat.col_count", &"briefing.threat.col_type"]:
		var head := Kit.label(tr(key), &"Dense", true)
		head.autowrap_mode = TextServer.AUTOWRAP_OFF
		grid.add_child(head)
	for t: Dictionary in threats:
		var word := Kit.label(_tx_str(&"word", [&"confidence", Kit.get_int(t, "confidence")]), &"Body", true)
		word.autowrap_mode = TextServer.AUTOWRAP_OFF
		grid.add_child(word)
		var count := Kit.label(count_text(t), &"Body", true)
		count.autowrap_mode = TextServer.AUTOWRAP_OFF
		count.horizontal_alignment = HORIZONTAL_ALIGNMENT_RIGHT
		grid.add_child(count)
		var name_text := _Text.resolve(t.get("name"))
		var ace := Kit.get_str(t, "ace")
		if not ace.is_empty():
			name_text = tr(&"briefing.threat.with_ace").format([name_text, ace_name(StringName(ace))])
		var type_label := Kit.label(name_text, &"Body", true)
		type_label.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		grid.add_child(type_label)
	return grid


## An ace's display name from Content, or its id when Content does not know it.
static func ace_name(id: StringName) -> String:
	for a: Dictionary in Kit.dicts(Kit.call_on(&"Content", &"aces")):
		if StringName(Kit.get_str(a, "id")) == id:
			var key := StringName(Kit.get_str(a, "name"))
			if key != &"":
				return String(TranslationServer.translate(key))
	return String(id)


func _build_mission(b: Dictionary, paragraph: String, fit: Dictionary) -> void:
	_section(&"briefing.section.mission")
	_paragraph(_orders, paragraph)
	var tags: Array = Kit.as_array(b.get("tags"))
	if not tags.is_empty():
		_paragraph(_orders, tr(&"briefing.tags.lead"))
		var verdicts := tag_verdicts(tags, fit)
		for i in tags.size():
			_orders.add_child(_tag_row(_Text.resolve(tags[i]), verdicts[i]))
	var ghosts := Kit.dicts(b.get("ghosts"))
	if not ghosts.is_empty():
		_paragraph(_orders, tr(&"briefing.ghosts.lead"))
		var rows: Array = []
		for g: Dictionary in ghosts:
			rows.append([_Text.resolve(g.get("text")), _Text.resolve(g.get("reward"))])
		_orders.add_child(_two_columns(rows))


## Rows of `[text, reward]` as a table: the text takes the width, the reward sits at the right.
func _two_columns(rows: Array) -> GridContainer:
	var grid := GridContainer.new()
	grid.columns = 2
	grid.add_theme_constant_override(&"h_separation", Ui.spacing(4))
	grid.add_theme_constant_override(&"v_separation", Ui.spacing(0))
	for row: Variant in rows:
		var pair := row as Array
		var text := Kit.label(String(pair[0]), &"Body", true)
		text.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		grid.add_child(text)
		var reward := Kit.label(String(pair[1]), &"Body", true)
		reward.autowrap_mode = TextServer.AUTOWRAP_OFF
		reward.horizontal_alignment = HORIZONTAL_ALIGNMENT_RIGHT
		grid.add_child(reward)
	return grid


## Per briefing tag, its Fit Check verdict: 1 pass, 0 fail, -1 not checked. Tags match the Fit
## Check's by sentence; when no sentence matches and the counts agree, by position.
static func tag_verdicts(tags: Array, fit: Dictionary) -> Array[int]:
	var out: Array[int] = []
	out.resize(tags.size())
	out.fill(-1)
	var state := Kit.get_int(fit, "state", JOB_IDLE)
	if state != JOB_DONE and state != JOB_STALE:
		return out
	var results := Kit.dicts(fit.get("tags"))
	var used: Array[bool] = []
	used.resize(results.size())
	used.fill(false)
	var matched := 0
	for i in tags.size():
		for j in results.size():
			if not used[j] and results[j].get("text") == tags[i]:
				used[j] = true
				out[i] = 1 if Kit.get_bool(results[j], "pass") else 0
				matched += 1
				break
	if matched == 0 and results.size() == tags.size():
		for i in tags.size():
			out[i] = 1 if Kit.get_bool(results[i], "pass") else 0
	return out


## A tag sentence with its verdict as glyph and word on the Severity stripe (L1689).
func _tag_row(text: String, verdict: int) -> HBoxContainer:
	var row := Kit.hbox(2)
	row.add_child(_verdict(verdict))
	var l := Kit.label(text, &"Body", true)
	l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	row.add_child(l)
	return row


func _verdict(verdict: int) -> Control:
	var box := Kit.hbox(1)
	box.custom_minimum_size = Vector2(Ui.spacing(6) * 2.75, 0.0)
	var word: Label
	var stem: StringName = &""
	match verdict:
		1:
			stem = &"check_pass"
			word = Kit.label(_tx_str(&"word", [&"check", CHECK_PASS]), &"SeverityPass", true)
		0:
			stem = &"check_fail"
			word = Kit.label(_tx_str(&"word", [&"check", CHECK_FAIL]), &"SeverityFail", true)
		_:
			word = Kit.label(tr(&"briefing.tags.unchecked"), &"SeverityAdvisory", true)
	word.autowrap_mode = TextServer.AUTOWRAP_OFF
	box.add_child(word)
	if stem != &"":
		var icon := TextureRect.new()
		icon.texture = Ui.icon(stem)
		icon.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
		icon.stretch_mode = TextureRect.STRETCH_KEEP_ASPECT_CENTERED
		icon.custom_minimum_size = Vector2(24.0, 24.0)
		icon.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		box.add_child(icon)
	return box


func _build_execution(b: Dictionary, paragraph: String) -> void:
	_section(&"briefing.section.execution")
	_paragraph(_orders, paragraph)
	_facts(_orders, execution_rows(b))


## Execution facts `[term, value]` from a `BriefingDict`. Empty values are left out by the table.
static func execution_rows(b: Dictionary) -> Array:
	var rows: Array = []
	var t := func(k: StringName) -> String: return String(TranslationServer.translate(k))
	var km := Kit.get_float(b, "distance_km")
	if km > 0.0:
		rows.append([t.call(&"briefing.exec.approach"), Kit.number(km, &"km", 0 if km >= 10.0 else 1)])
	var words := PackedStringArray()
	var actions: Variant = b.get("actions")
	if actions is PackedInt32Array or actions is Array:
		for a: Variant in actions:
			var w := _tx_str(&"word", [&"station", int(a)])
			if not w.is_empty():
				words.append(w)
	rows.append([t.call(&"briefing.exec.actions"), ", ".join(words) if not words.is_empty() else t.call(&"briefing.exec.weapons_only")])
	var emcon := Kit.get_int(b, "recommended_emcon", NONE)
	if emcon >= 0:
		rows.append([t.call(&"briefing.exec.emcon"), t.call(&"briefing.exec.emcon_value").format([_tx_str(&"word", [&"emcon", emcon])])])
	var days := Kit.get_float(b, "pushback_days")
	if days > 0.0:
		var key := &"briefing.exec.clock_value_one" if is_equal_approx(days, 1.0) else &"briefing.exec.clock_value"
		var shown := Kit.number(days, &"none", 0 if is_equal_approx(days, roundf(days)) else 1)
		rows.append([t.call(&"briefing.exec.clock"), t.call(key).format([shown])])
	if Kit.get_bool(b, "forced"):
		rows.append([t.call(&"briefing.exec.offer"), t.call(&"briefing.exec.forced")])
	else:
		var expires := Kit.get_int(b, "expires_h", NONE)
		if expires >= 0:
			rows.append([t.call(&"briefing.exec.offer"), t.call(&"briefing.exec.lapses").format([_tx_str(&"date_time_group", [expires])])])
	return rows


# --- Fit Check -----------------------------------------------------------------------------------

func _build_fit(b: Dictionary, fit: Dictionary) -> void:
	for line: String in fit_lines(b, fit):
		_paragraph(_fit, line)


## The Fit Check summary as lines: the state, then (for a current or stale result) the verdict, the
## seakeeping sweep's verdict, speed, turn, range and detection against each known threat class (L801).
static func fit_lines(b: Dictionary, fit: Dictionary) -> PackedStringArray:
	var out := PackedStringArray()
	var t := func(k: StringName) -> String: return String(TranslationServer.translate(k))
	if fit.is_empty():
		out.append(t.call(&"briefing.fit.unavailable"))
		return out
	var state := Kit.get_int(fit, "state", JOB_IDLE)
	match state:
		JOB_IDLE:
			out.append(t.call(&"briefing.fit.idle"))
			return out
		JOB_RUNNING:
			var sea := Kit.get_int(Kit.as_dict(b.get("weather")), "sea_state")
			out.append(t.call(&"briefing.fit.running").format([Kit.number(sea, &"none", 0)]))
			return out
		JOB_FAILED:
			out.append(t.call(&"briefing.fit.failed"))
			return out
		JOB_STALE:
			out.append(t.call(&"briefing.fit.stale"))
		JOB_DONE:
			pass
		_:
			out.append(t.call(&"briefing.fit.unavailable"))
			return out
	var tags := Kit.dicts(fit.get("tags"))
	var failed := 0
	for r: Dictionary in tags:
		if not Kit.get_bool(r, "pass"):
			failed += 1
	if tags.is_empty():
		out.append(t.call(&"briefing.fit.no_tags"))
	elif failed == 0:
		out.append(t.call(&"briefing.fit.passes"))
	else:
		out.append(t.call(&"briefing.fit.fails").format([Kit.number(failed, &"none", 0), Kit.number(tags.size(), &"none", 0)]))
	# The seakeeping sweep: safe, can roll over (sea state and where the sea came from), or not finished.
	var sea := _FitCheck.seakeeping_verdict(fit)
	if not sea.is_empty():
		out.append(String(sea["text"]))
	var speed := Kit.get_float(fit, "top_speed_kn", -1.0)
	if speed >= 0.0:
		out.append(t.call(&"briefing.fit.speed").format([Kit.number(speed, &"kn", 1)]))
	var turn := Kit.get_float(fit, "time_to_turn_180_s", -1.0)
	if turn > 0.0:
		out.append(t.call(&"briefing.fit.turn").format([Kit.number(turn, &"s", 0)]))
	elif Kit.get_bool(fit, "simulated"):
		out.append(t.call(&"briefing.fit.no_turn"))
	var range_km := Kit.get_float(fit, "range_km", -1.0)
	if range_km >= 0.0:
		out.append(t.call(&"briefing.fit.range").format([Kit.number(range_km, &"km", 0)]))
	var names := {}
	for th: Dictionary in Kit.dicts(b.get("threats")):
		names[StringName(Kit.get_str(th, "class_id"))] = _Text.resolve(th.get("name"))
	for d: Dictionary in Kit.dicts(fit.get("detect")):
		var id := StringName(Kit.get_str(d, "class_id"))
		var who: String = names.get(id, String(id))
		out.append(t.call(&"briefing.fit.detect").format([who, Kit.number(Kit.get_float(d, "we_detect_km"), &"km", 1), Kit.number(Kit.get_float(d, "they_detect_km"), &"km", 1)]))
	if not Kit.get_bool(fit, "simulated", true):
		out.append(t.call(&"briefing.fit.estimated"))
	return out


# --- chart ---------------------------------------------------------------------------------------

## Hand the sortie map and route to the plot in briefing mode. Only from `setup`, so the plot is
## not called while the compile check instantiates the scene.
func _show_chart() -> void:
	if not _live or _briefing.is_empty() or not _chart.has_method(&"show_static"):
		return
	var route: Variant = _briefing.get("route")
	var path: PackedVector2Array = route if route is PackedVector2Array else PackedVector2Array()
	var map := Kit.as_dict(_briefing.get("map"))
	_chart.call(&"set_mode", &"briefing")
	_chart.call(&"show_static", map, path, PackedVector3Array())
	_frame_chart(map, path)


## Plot scales in metres per pixel, in `BpPlotView` step order.
const PLOT_STEPS_M: Array[float] = [1.0, 2.5, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0]
## Markers left out of the framing (they still draw when they fall inside it).
const FRAME_SKIP: Array[String] = ["units.marker.convoy_entry", "units.marker.convoy_exit"]
## Sea kept around the framed route and markers, metres.
const FRAME_PAD_M := 2500.0


## Frame the route chart on the route and the briefing's markers (the theatre sends only those the
## operation uses), at the closest plot scale that holds them, rather than the whole sortie map.
func _frame_chart(map: Dictionary, route: PackedVector2Array) -> void:
	var size := _chart.size
	if size.x < 2.0 or size.y < 2.0 or not _chart.has_method(&"set_scale_step"):
		return
	var pts := PackedVector2Array(route)
	for m: Dictionary in Kit.dicts(map.get("markers")):
		var pos: Variant = m.get("pos")
		# Convoy entry and exit are where a convoy comes from and goes to, often far off the action.
		if pos is Vector2 and (pos as Vector2).is_finite() and not Kit.get_str(m, "label") in FRAME_SKIP:
			pts.append(pos)
	if pts.is_empty():
		return
	var box := Rect2(pts[0], Vector2.ZERO)
	for p: Vector2 in pts:
		box = box.expand(p)
	box = box.grow(FRAME_PAD_M)
	var step := PLOT_STEPS_M.size() - 1
	for i in PLOT_STEPS_M.size():
		if PLOT_STEPS_M[i] * size.x >= box.size.x and PLOT_STEPS_M[i] * size.y >= box.size.y:
			step = i
			break
	_chart.call(&"set_scale_step", step)
	_chart.call(&"centre_on", box.get_center())


# --- co-op ---------------------------------------------------------------------------------------

## True on a co-op member (a lobby this process joined rather than hosts).
static func _is_member() -> bool:
	return Kit.call_on(&"Lobby", &"is_active") == true and Kit.call_on(&"Lobby", &"is_host") != true


func _show_coop() -> void:
	if not _live:
		return
	var proposal := {}
	for p: Dictionary in Kit.dicts(Kit.as_dict(Kit.call_on(&"Campaign", &"chart")).get("proposals")):
		var action := Kit.as_dict(p.get("action"))
		if Kit.get_int(action, "kind", NONE) == THEATRE_ACTION_ACCEPT_OPERATION and StringName(Kit.get_str(action, "operation")) == _operation:
			proposal = p
	show_coop(Kit.dicts(Kit.call_on(&"Campaign", &"players")), proposal, not _member)


## Who has accepted the operation, from `Campaign.players()` and the open accept proposal (empty
## when none is open, which means every player has accepted). Hidden for a single player.
func show_coop(players: Array[Dictionary], proposal: Dictionary, is_host: bool) -> void:
	Kit.clear(_coop)
	_coop_buttons.clear()
	var coop := players.size() > 1 and not _briefing.is_empty()
	_coop_rule.visible = coop
	_coop_title.visible = coop
	_coop.visible = coop
	if not coop:
		_link_focus()
		return
	var accepted: Array = []
	var raw: Variant = proposal.get("accepted")
	if raw is PackedInt32Array or raw is Array:
		accepted = Array(raw)
	var local_accepted := proposal.is_empty()
	for p: Dictionary in players:
		var slot := Kit.get_int(p, "player")
		var name_text := Kit.get_str(p, "name")
		if name_text.is_empty():
			name_text = tr(&"briefing.coop.unnamed").format([Kit.number(slot + 1, &"none", 0)])
		var key := &"briefing.coop.accepted"
		if not Kit.get_bool(p, "connected", true):
			key = &"briefing.coop.away"
		elif not proposal.is_empty() and not accepted.has(slot):
			key = &"briefing.coop.waiting"
		if Kit.get_bool(p, "is_local") and accepted.has(slot):
			local_accepted = true
		_paragraph(_coop, tr(key).format([name_text]))
	if proposal.is_empty():
		_paragraph(_coop, tr(&"briefing.coop.all"))
	else:
		var id := Kit.get_int(proposal, "id", NONE)
		if not local_accepted:
			var accept := Kit.inline_button("briefing.coop.accept")
			accept.pressed.connect(func() -> void: Kit.call_on(&"Campaign", &"respond", [id, true]))
			_coop.add_child(accept)
			_coop_buttons.append(accept)
		if is_host:
			var confirm := Kit.inline_button("briefing.coop.confirm")
			confirm.pressed.connect(func() -> void: Kit.call_on(&"Campaign", &"confirm", [id]))
			_coop.add_child(confirm)
			_coop_buttons.append(confirm)
	_link_focus()


# --- actions -------------------------------------------------------------------------------------

func _on_launch() -> void:
	if _busy or _member or not _live:
		return
	_busy = true
	_say("")
	var ok: Variant = Kit.call_on(&"Campaign", &"launch_sortie")
	_busy = false
	if ok != true and _status.text.is_empty():
		_say(tr(&"briefing.status.launch_failed"))


func _on_back() -> void:
	if _live:
		Kit.call_on(&"Campaign", &"close_briefing")


func _on_dock() -> void:
	if not _live:
		return
	_say("")
	var ok: Variant = Kit.call_on(&"Campaign", &"open_dry_dock")
	if ok != true and _status.text.is_empty():
		_say(tr(&"briefing.status.dock_failed"))


## Only template operations leave the table when declined; story operations, ace duels, the
## capstone and forced operations stay on offer (the theatre keeps them), so they offer no Decline.
static func can_decline(b: Dictionary) -> bool:
	return not b.is_empty() and not Kit.get_bool(b, "forced") and Kit.get_int(b, "kind", NONE) == OPERATION_TEMPLATE


func _on_decline() -> void:
	if not can_decline(_briefing):
		return
	var dialog: Node = Screens.open_dialog(&"briefing.decline.title", &"briefing.decline.body", [
		{"id": &"keep", "label": &"briefing.decline.keep"},
		{"id": &"decline", "label": &"briefing.decline.confirm"},
	], &"keep", &"keep")
	if dialog != null:
		dialog.connect(&"chosen", _on_decline_chosen)


func _on_decline_chosen(id: StringName) -> void:
	if id != &"decline" or _operation == &"" or not _live:
		return
	_say("")
	var ok: Variant = Kit.call_on(&"Campaign", &"propose", [{
		"kind": THEATRE_ACTION_DECLINE_OPERATION, "operation": _operation, "node": &"", "hours": 0,
		"player": NONE, "intel": null,
	}])
	# A committed decline clears the briefed operation: go back to the chart table. In co-op it is
	# a proposal first, and the briefing stays open until it commits.
	if ok == true and Kit.call_on(&"Campaign", &"briefed_operation") != _operation:
		Kit.call_on(&"Campaign", &"close_briefing")


func _say(text: String) -> void:
	_status.text = text
	_status.visible = not text.is_empty()


# --- facade signals ------------------------------------------------------------------------------

func _on_chart_changed() -> void:
	refresh()


func _on_proposal_changed(_proposal: Dictionary, _state: StringName) -> void:
	_show_coop()


func _on_rejected(text: Dictionary) -> void:
	_say(_Text.resolve(text))


func _on_fit_changed() -> void:
	if _briefing.is_empty():
		return
	show_briefing(_briefing, Kit.as_dict(Kit.call_on(&"Campaign", &"briefing_fit")))


# --- layout and focus ----------------------------------------------------------------------------

func _fit_layout() -> void:
	if not is_node_ready():
		return
	var stacked := size.x < STACK_BELOW_PX
	_body.vertical = stacked
	# Stacked (a narrow window or a large UI scale) the footer row needs every pixel.
	_launch.custom_minimum_size = Vector2(0.0 if stacked else LAUNCH_MIN_W, Kit.PRIMARY_H)


## Focus runs from the scrolling order (Up and Down scroll it) through the co-op buttons to the
## footer row, Launch last; Tab cycles the same order.
func _link_focus() -> void:
	var scroll: ScrollContainer = $Margin/Column/Scroll
	var footer: Array[Control] = []
	for b: Button in [_back, _decline, _dock, _launch]:
		if b.visible and not b.disabled:
			footer.append(b)
	Kit.chain_horizontal(footer)
	var chain: Array = [scroll]
	chain.append_array(_coop_buttons)
	var landing: Control = _launch if footer.has(_launch) else (footer[-1] if not footer.is_empty() else null)
	if landing != null:
		chain.append(landing)
	Kit.chain_vertical(chain)
	var above: Control = chain[-2] if chain.size() >= 2 else scroll
	for b: Control in footer:
		b.focus_neighbor_top = b.get_path_to(above)
		b.focus_neighbor_bottom = b.get_path_to(b)
	scroll.focus_neighbor_left = scroll.get_path_to(scroll)
	scroll.focus_neighbor_right = scroll.get_path_to(scroll)
	var ring: Array[Control] = [scroll]
	for c: Control in _coop_buttons:
		ring.append(c)
	ring.append_array(footer)
	for i in ring.size():
		ring[i].focus_next = ring[i].get_path_to(ring[(i + 1) % ring.size()])
		ring[i].focus_previous = ring[i].get_path_to(ring[(i - 1 + ring.size()) % ring.size()])


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


func _connect(node: Object, sig: StringName, callable: Callable) -> void:
	if node.has_signal(sig) and not node.is_connected(sig, callable):
		node.connect(sig, callable)
