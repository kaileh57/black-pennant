extends Control
## Chart table root: the one light surface, drawn with the chart tokens.
## DESIGN.md L739-751, L1643. Unit: ui-chart.
## Reads and calls: Campaign.chart, Campaign.chart_changed, Campaign.phase_changed, Screens.
##
## Base screen of the campaign between operations (Screens routes the chart, advancing and over
## phases here). Layout: the clock bar and the shared pool across the top; the chart filling the
## left; on the right one docked column with open co-op proposals, the selected place (what it is,
## the transit preview and the Transit and Wait orders), and the tabs Operations (first: the
## most-used decision, L69), Cove, Bulletin and Log.
##
## The three time decisions of L69 live here: select a cove on the chart and Transit, Wait a number
## of hours, or refit from the Cove tab. In co-op they are proposals (L839) and the buttons say so.
## A refusal (`Campaign.rejected`) shows under the orders until the next decision; nothing toasts
## except the player's own save (L1605).
##
## Signals in: chart and resources changes re-read and redraw; each theatre hour moves SEXTANT along
## its committed route; theatre events feed the log and box what changed on the chart for the
## mode-change time; an interrupt stops the clock bar, says why, and brings forward what it is about
## (the offered operation, the arrival cove, the bulletin, the cove).
##
## Esc opens the pause menu over the table (`context` chart, which also holds running time); the
## `pause` action holds or releases running time. Focus: the chart moves its selection with the
## directions and hands focus to the column on its right edge; the column is one vertical chain.
##
## Everything is read through res://ui/chart/chart_source.gd, or a fixture given to `use_source`
## for captures and tests.

const Kit := preload("res://ui/screens/menu_kit.gd")
const Source := preload("res://ui/chart/chart_source.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")

## `Bp` values used here, kept so the script parses before the extension loads.
const PHASE_CHART := 1
const PHASE_ADVANCING := 2
const PHASE_OVER := 7
const THEATRE_ACTION_TRANSIT := 0
const THEATRE_ACTION_WAIT := 2
const INTERRUPT_ARRIVED := 0
const INTERRUPT_OPERATION_OFFERED := 3
const INTERRUPT_FORCED_OPERATION := 4
const INTERRUPT_CARD_ARRIVING := 6
const INTERRUPT_HULL_READY := 9
const DOCK_CAMPAIGN := 1
## Wait lengths offered, theatre hours. 12 is the default: half a day lets a passage or a patrol
## change without burning the operation clock.
const WAIT_HOURS: Array[int] = [6, 12, 24, 48, 72]
const DEFAULT_WAIT := 12
const LOG_TAB := 3

## The committed passage, kept across rebuilds of the table: `path`, `hours`, `start_h`, `to`.
static var _passage: Dictionary = {}
## Routes of open transit proposals by id, `{ "path", "hours" }` read when they were made (a
## committed transit's preview only says SEXTANT is under way).
static var _proposal_routes: Dictionary[int, Dictionary] = {}
static var _campaign_key := ""
static var _wait_index := 1

var _source: RefCounted
var _chart: Dictionary = {}
var _alert_floor: Dictionary[StringName, int] = {}
var _preview: Dictionary = {}
var _log_dirty := false
var _focus_dirty := false

@onready var _map: Control = $Page/Layout/Body/ChartMap
@onready var _clock: Control = $Page/Layout/TopBar/Row/ClockBar
@onready var _resources: Control = $Page/Layout/TopBar/Row/ResourcesBar
@onready var _side: PanelContainer = $Page/Layout/Body/Side
@onready var _proposals: Control = $Page/Layout/Body/Side/Margin/Scroll/Column/ProposalsPanel
@onready var _place_name: Label = $Page/Layout/Body/Side/Margin/Scroll/Column/Place/Name
@onready var _facts: Label = $Page/Layout/Body/Side/Margin/Scroll/Column/Place/Facts
@onready var _preview_label: Label = $Page/Layout/Body/Side/Margin/Scroll/Column/Place/Preview
@onready var _transit: Button = $Page/Layout/Body/Side/Margin/Scroll/Column/Place/Actions/Transit
@onready var _wait_hours: OptionButton = $Page/Layout/Body/Side/Margin/Scroll/Column/Place/WaitRow/WaitHours
@onready var _wait: Button = $Page/Layout/Body/Side/Margin/Scroll/Column/Place/WaitRow/Wait
@onready var _reason: Label = $Page/Layout/Body/Side/Margin/Scroll/Column/Place/Reason
@onready var _tabs: TabContainer = $Page/Layout/Body/Side/Margin/Scroll/Column/Tabs
@onready var _operations: Control = $Page/Layout/Body/Side/Margin/Scroll/Column/Tabs/Operations
@onready var _cove: Control = $Page/Layout/Body/Side/Margin/Scroll/Column/Tabs/Cove
@onready var _bulletin: Control = $Page/Layout/Body/Side/Margin/Scroll/Column/Tabs/Bulletin
@onready var _log: Control = $Page/Layout/Body/Side/Margin/Scroll/Column/Tabs/Log


func _ready() -> void:
	Ui.set_margins($Page/Layout/TopBar, 3)
	$Page/Layout/TopBar/Row.add_theme_constant_override(&"h_separation", Ui.spacing(5))
	$Page/Layout/TopBar/Row.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	Ui.set_separation($Page/Layout, 0)
	Ui.set_separation($Page/Layout/Body, 0)
	Ui.set_margins($Page/Layout/Body/Side/Margin, 3)
	Ui.set_separation($Page/Layout/Body/Side/Margin/Scroll/Column, 3)
	Ui.set_separation($Page/Layout/Body/Side/Margin/Scroll/Column/Place, 1)
	Ui.set_separation($Page/Layout/Body/Side/Margin/Scroll/Column/Place/Actions, 1)
	Ui.set_separation($Page/Layout/Body/Side/Margin/Scroll/Column/Place/WaitRow, 1)
	_transit.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
	for l: Label in [_place_name, _facts, _preview_label, _reason]:
		l.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		l.text = ""
	_side.accessibility_name = tr(&"chart.side.name")
	_reason.set("accessibility_live", 1)
	for b: Control in [_transit, _wait_hours, _wait]:
		b.custom_minimum_size = Vector2(0.0, Kit.BUTTON_H)
		b.focus_mode = Control.FOCUS_ALL
	_transit.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_wait.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_wait_hours.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_wait_hours.accessibility_name = tr(&"chart.wait.hours_name")
	_wait_hours.tooltip_text = tr(&"chart.wait.hours_tip")
	for h: int in WAIT_HOURS:
		_wait_hours.add_item(Kit.number(h, &"h", 0))
	_wait_hours.select(clampi(_wait_index, 0, WAIT_HOURS.size() - 1))
	_wait_hours.item_selected.connect(_on_wait_hours)
	_transit.pressed.connect(_on_transit)
	_wait.pressed.connect(_on_wait)
	_map.node_selected.connect(_on_node_selected)
	_map.node_activated.connect(_on_node_activated)
	_tabs.tab_changed.connect(_on_tab_changed)
	_tabs.get_tab_bar().focus_mode = Control.FOCUS_ALL
	_set_tab_titles()
	for panel: Control in [_operations, _cove, _bulletin, _log, _proposals]:
		if panel.has_signal(&"focus_changed"):
			panel.connect(&"focus_changed", _queue_focus_chain)
		if panel.has_signal(&"rejected"):
			panel.connect(&"rejected", show_reason)
	if _operations.has_signal(&"operation_selected"):
		_operations.connect(&"operation_selected", _on_operation_selected)
	if _log.has_signal(&"unread_changed"):
		_log.connect(&"unread_changed", func(_n: int) -> void: _set_tab_titles())
	if _log.has_signal(&"focus_requested"):
		_log.connect(&"focus_requested", _on_log_focus)
	_clock.state_changed.connect(_queue_focus_chain)
	resized.connect(_queue_fit)
	$Page/Layout/Body/Side/Margin/Scroll.resized.connect(_queue_fit)
	_queue_fit()
	_connect_campaign()
	use_source(Source.new())


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_set_tab_titles()
		_render_place()


## Read and send through `source` from now on (the live source by default; a fixture in captures).
func use_source(source: RefCounted) -> void:
	_source = source
	var key := Kit.get_str(Kit.as_dict(source.call(&"current_slot")), "campaign_id")
	if key != _campaign_key:
		_campaign_key = key
		_passage = {}
		_proposal_routes.clear()
	for child: Control in [_map, _clock, _resources, _proposals, _operations, _cove, _bulletin, _log]:
		if child.has_method(&"bind"):
			child.call(&"bind", source)
	refresh()


## Screens calls this with the `change` args; the chart needs none.
func setup(_args: Dictionary) -> void:
	refresh()


## The most-used control takes focus: the first operation on offer, else the chart.
func default_focus() -> Control:
	if _operations.has_method(&"first_control"):
		var c: Variant = _operations.call(&"first_control")
		if c is Control and _tabs.current_tab == 0:
			return c
	return _map


## Re-read everything and redraw.
func refresh() -> void:
	if _source == null or not is_node_ready():
		return
	_chart = _source.call(&"chart")
	_map.call(&"refresh")
	if _map.call(&"selected") == &"":
		_map.call(&"select", StringName(Kit.get_str(_chart, "sextant")), false)
	_update_passage()
	_clock.call(&"refresh")
	_resources.call(&"refresh")
	for panel: Control in [_proposals, _operations, _cove, _bulletin, _log]:
		panel.call(&"refresh")
	_remember_alerts()
	_render_place()
	_queue_focus_chain()


func _connect_campaign() -> void:
	var c := Kit.autoload(&"Campaign")
	if c == null:
		return
	var wires: Dictionary[StringName, Callable] = {
		&"chart_changed": _on_chart_changed,
		&"resources_changed": _on_resources_changed,
		&"phase_changed": _on_phase_changed,
		&"hour_advanced": _on_hour_advanced,
		&"theatre_event": _on_theatre_event,
		&"interrupted": _on_interrupted,
		&"proposal_changed": _on_proposal_changed,
		&"rejected": _on_rejected,
		&"saved": _on_saved,
	}
	for sig: StringName in wires:
		if c.has_signal(sig) and not c.is_connected(sig, wires[sig]):
			c.connect(sig, wires[sig])


# --- campaign signals ----------------------------------------------------------------------------

## True for the live source, which then forgets its remembered reads: a Campaign signal means
## something changed.
func _live() -> bool:
	if _source == null or _source.call(&"is_live") != true:
		return false
	_source.call(&"invalidate")
	return true


func _on_chart_changed() -> void:
	if not _live():
		return
	_chart = _source.call(&"chart")
	_map.call(&"refresh")
	_update_passage()
	_clock.call(&"refresh")
	for panel: Control in [_proposals, _operations, _cove, _bulletin]:
		panel.call(&"refresh")
	_remember_alerts()
	_render_place()
	_queue_focus_chain()


func _on_resources_changed() -> void:
	if not _live():
		return
	_resources.call(&"refresh")
	_cove.call(&"refresh")
	_bulletin.call(&"refresh")


func _on_phase_changed(_phase: int) -> void:
	if not _live():
		return
	_chart = _source.call(&"chart")
	_clock.call(&"refresh")
	_render_place()
	_queue_focus_chain()


func _on_hour_advanced(hour: int) -> void:
	if not _live():
		return
	_clock.call(&"on_hour", hour)
	_update_passage(hour)


func _on_theatre_event(event: Dictionary) -> void:
	if not _live():
		return
	var kind := StringName(Kit.get_str(event, "kind"))
	match kind:
		&"lane_cut", &"lane_restored":
			_map.call(&"flash", &"lane", StringName(Kit.get_str(event, "a")), StringName(Kit.get_str(event, "b")))
		&"alert_changed":
			var region := StringName(Kit.get_str(event, "region"))
			var floor_now := floori(Kit.get_float(event, "alert"))
			if _alert_floor.has(region) and _alert_floor[region] != floor_now:
				_map.call(&"flash", &"region", region)
			_alert_floor[region] = floor_now
		&"alert_escalated":
			_map.call(&"flash", &"region", StringName(Kit.get_str(event, "region")))
		&"depot_emptied", &"group_sighted", &"ace_revealed":
			_map.call(&"flash", &"node", StringName(Kit.get_str(event, "node")))
		&"sextant_moved":
			_passage = {}
			_map.call(&"set_passage", [], 0.0)
			_map.call(&"flash", &"node", StringName(Kit.get_str(event, "to")))
		&"sextant_sunk":
			_passage = {}
			_map.call(&"set_passage", [], 0.0)
			_map.call(&"flash", &"node", StringName(Kit.get_str(event, "relocated_to")))
		&"hunter_group_arrived":
			_map.call(&"flash", &"node", StringName(Kit.get_str(_chart, "sextant")))
	if kind != &"hour_advanced" and kind != &"interrupted":
		_mark_log_dirty()


func _mark_log_dirty() -> void:
	if _log_dirty:
		return
	_log_dirty = true
	_refresh_log.call_deferred()


func _refresh_log() -> void:
	_log_dirty = false
	if is_inside_tree():
		_log.call(&"refresh")


func _on_interrupted(interrupt: Dictionary) -> void:
	if not _live():
		return
	# Campaign emits the interrupt before the folded chart change, so read the new state first:
	# the offer an interrupt names is not in the last chart yet.
	_on_chart_changed()
	_clock.call(&"on_interrupted", interrupt)
	bring_forward(interrupt)


## Show what an interrupt is about: the offered operation, the arrival cove, the bulletin or the
## cove services (L741: the player decides next).
func bring_forward(interrupt: Dictionary) -> void:
	match Kit.get_int(interrupt, "kind", -1):
		INTERRUPT_ARRIVED:
			_passage = {}
			_map.call(&"set_passage", [], 0.0)
			var node := StringName(Kit.get_str(interrupt, "node"))
			_map.call(&"select", node, false)
			_render_place()
		INTERRUPT_OPERATION_OFFERED, INTERRUPT_FORCED_OPERATION:
			_tabs.current_tab = 0
			_operations.call(&"select_operation", StringName(Kit.get_str(interrupt, "operation")))
		INTERRUPT_CARD_ARRIVING:
			_tabs.current_tab = 2
		INTERRUPT_HULL_READY:
			_tabs.current_tab = 1


func _on_proposal_changed(proposal: Dictionary, state: StringName) -> void:
	if not _live():
		return
	var id := Kit.get_int(proposal, "id", -1)
	var action := Kit.as_dict(proposal.get("action"))
	if Kit.get_int(action, "kind", -1) == THEATRE_ACTION_TRANSIT:
		var to := StringName(Kit.get_str(action, "node"))
		match state:
			&"made":
				var p := Kit.as_dict(_source.call(&"transit_preview", to))
				_proposal_routes[id] = {"path": Kit.as_array(p.get("path")), "hours": Kit.get_int(p, "hours", -1)}
			&"committed":
				var route: Dictionary = _proposal_routes.get(id, {})
				_start_passage(Kit.as_array(route.get("path")), to, Kit.get_int(route, "hours", -1))
				_proposal_routes.erase(id)
			&"withdrawn":
				_proposal_routes.erase(id)
	if state == &"committed":
		_clock.call(&"on_committed")
	_proposals.call(&"refresh")
	_queue_focus_chain()


func _on_rejected(text: Dictionary) -> void:
	show_reason(text)


func _on_saved(_slot: Dictionary) -> void:
	if _live():
		_cove.call(&"refresh")


## Show a refusal or problem under the orders (a `MsgDict`, text key or plain text); empty hides it.
func show_reason(text: Variant) -> void:
	var s := _Text.resolve(text)
	_reason.text = s
	_reason.visible = not s.is_empty()


# --- selection and orders ------------------------------------------------------------------------

func _on_node_selected(_node: StringName) -> void:
	show_reason("")
	_render_place()


func _on_node_activated(node: StringName) -> void:
	_map.call(&"select", node, false)
	_render_place()
	# Activation goes to the order the place offers: Transit to an open cove, else the operation
	# list when an operation is on offer there.
	if not _transit.disabled:
		Ui.focus(_transit)
		return
	if _operations.has_method(&"select_at_node") and _operations.call(&"select_at_node", node) == true:
		_tabs.current_tab = 0
		var c: Variant = _operations.call(&"first_control")
		if c is Control:
			Ui.focus(c)


func _on_operation_selected(operation: Dictionary) -> void:
	var node := StringName(Kit.get_str(operation, "node"))
	if node != &"":
		_map.call(&"select", node, false)
		_render_place()


func _on_log_focus(target: StringName) -> void:
	if _map.call(&"has_node_id", target) == true:
		_map.call(&"select", target, false)
		_map.call(&"flash", &"node", target)
		_render_place()
	else:
		_map.call(&"flash", &"region", target)


func _decisions_open() -> bool:
	return Kit.get_int(_chart, "phase", -1) == PHASE_CHART and not (_source != null and _source.call(&"is_advancing") == true)


func _render_place() -> void:
	if not is_node_ready() or _source == null:
		return
	var coop: bool = _source.call(&"is_coop") == true
	var open := _decisions_open()
	var wait_h := WAIT_HOURS[clampi(_wait_hours.selected, 0, WAIT_HOURS.size() - 1)]
	_wait.text = tr(&"chart.wait.propose" if coop else &"chart.wait.order").format([Kit.number(wait_h, &"h", 0)])
	_wait.disabled = not open
	_wait.tooltip_text = tr(&"chart.wait.tip") if open else _busy_reason()
	_wait_hours.disabled = not open
	if _chart.is_empty():
		_place_name.text = tr(&"chart.place.no_campaign")
		_facts.text = tr(&"chart.place.no_campaign_body")
		_preview_label.text = ""
		_transit.text = tr(&"chart.transit.none")
		_transit.disabled = true
		_transit.visible = false
		_map.call(&"set_preview", [])
		return
	var node := StringName(_map.call(&"selected"))
	var sextant := StringName(Kit.get_str(_chart, "sextant"))
	if node == &"":
		node = sextant
	var info: Dictionary = _source.call(&"node_info", node)
	_place_name.text = _source.call(&"node_name", node)
	_facts.text = _facts_text(node, info)
	_preview = {}
	if node == sextant or info.is_empty():
		var bound := not Kit.get_bool(_chart, "at_cove", true)
		_preview_label.text = (tr(&"chart.place.sextant_bound") if bound else tr(&"chart.place.sextant_here")) if node == sextant else ""
		_transit.text = tr(&"chart.transit.none")
		_transit.disabled = true
		_transit.visible = false
		_transit.tooltip_text = tr(&"chart.transit.pick_tip")
		_map.call(&"set_preview", [])
		return
	_preview = _source.call(&"transit_preview", node)
	var reachable := Kit.get_bool(_preview, "reachable")
	_preview_label.text = _Text.resolve(_preview.get("text", {}))
	var hours := Kit.get_int(_preview, "hours")
	if reachable:
		_transit.text = tr(&"chart.transit.propose" if coop else &"chart.transit.order").format([_source.call(&"node_name", node), Kit.number(hours, &"h", 0)])
		_transit.tooltip_text = tr(&"chart.transit.tip_cut") if Kit.get_bool(_preview, "passes_cut_lane") else tr(&"chart.transit.tip")
	else:
		_transit.text = tr(&"chart.transit.none")
		_transit.tooltip_text = _preview_label.text
	# Only a cove SEXTANT can reach gets the order; elsewhere the preview line says why not.
	_transit.visible = reachable
	_transit.disabled = not (reachable and open)
	if reachable and not open:
		_transit.tooltip_text = _busy_reason()
	_map.call(&"set_preview", Kit.as_array(_preview.get("path")) if reachable else [])


func _busy_reason() -> String:
	if Kit.get_int(_chart, "phase", -1) == PHASE_OVER:
		return tr(&"chart.place.over")
	return tr(&"chart.place.busy")


## What the place is: kind and region, the region's Alert and Supply, and what is there now.
func _facts_text(node: StringName, info: Dictionary) -> String:
	if info.is_empty():
		return ""
	var kind := StringName(Kit.get_str(info, "kind"))
	var region := StringName(Kit.get_str(info, "region"))
	var parts := PackedStringArray()
	parts.append(tr(&"chart.place.kind").format([tr(StringName("chart.legend." + String(kind))), _source.call(&"region_name", region)]))
	for r: Dictionary in Kit.dicts(_chart.get("regions")):
		if StringName(Kit.get_str(r, "id")) == region:
			if Kit.get_bool(r, "open", true):
				parts.append(tr(&"chart.place.region").format([Kit.number(Kit.get_int(r, "alert"), &"none", 0), Kit.number(roundf(Kit.get_float(r, "supply")), &"pct", 0)]))
			else:
				parts.append(tr(&"chart.place.region_closed"))
	if kind == &"cove" and not Kit.as_array(_chart.get("coves")).has(node):
		parts.append(tr(&"chart.place.cove_closed"))
	var offers := 0
	for o: Dictionary in Kit.dicts(_chart.get("offers")):
		if StringName(Kit.get_str(o, "node")) == node:
			offers += 1
	if offers > 0:
		parts.append(tr(&"chart.place.offers").format([Kit.number(offers, &"none", 0)]))
	var contacts := 0
	for c: Dictionary in Kit.dicts(_chart.get("contacts")):
		if StringName(Kit.get_str(c, "node")) == node:
			contacts += 1
	if contacts > 0:
		parts.append(tr(&"chart.place.contacts").format([Kit.number(contacts, &"none", 0)]))
	return " ".join(parts)


func _on_transit() -> void:
	var node := StringName(_map.call(&"selected"))
	if node == &"" or _preview.is_empty() or not Kit.get_bool(_preview, "reachable"):
		return
	var path := Kit.as_array(_preview.get("path"))
	var hours := Kit.get_int(_preview, "hours")
	if _source.call(&"propose", {"kind": THEATRE_ACTION_TRANSIT, "node": node}) == true:
		show_reason("")
		if _source.call(&"is_coop") != true:
			_start_passage(path, node, hours)
			_clock.call(&"on_committed")
		_chart = _source.call(&"chart")
		_render_place()


func _on_wait() -> void:
	var hours := WAIT_HOURS[clampi(_wait_hours.selected, 0, WAIT_HOURS.size() - 1)]
	if _source.call(&"propose", {"kind": THEATRE_ACTION_WAIT, "hours": hours}) == true:
		show_reason("")
		if _source.call(&"is_coop") != true:
			_clock.call(&"on_committed")
		_chart = _source.call(&"chart")
		_render_place()


func _on_wait_hours(index: int) -> void:
	_wait_index = index
	_render_place()


# --- passage -------------------------------------------------------------------------------------

func _start_passage(path: Array, to: StringName, hours: int = -1) -> void:
	if path.size() < 2:
		_passage = {}
		return
	if hours <= 0:
		hours = maxi(1, path.size() - 1)
	_passage = {"path": path, "hours": hours, "start_h": Kit.get_int(_chart, "hour"), "to": to}
	_map.call(&"set_preview", [])
	_update_passage()


func _update_passage(hour: int = -1) -> void:
	if _passage.is_empty():
		_map.call(&"set_passage", [], 0.0)
		return
	if hour < 0:
		hour = Kit.get_int(_chart, "hour")
	var hours := maxi(1, int(_passage.get("hours", 1)))
	var fraction := float(hour - int(_passage.get("start_h", hour))) / float(hours)
	# Arrived, or the theatre no longer agrees about the destination: drop the route.
	if fraction >= 1.0 or (Kit.get_bool(_chart, "at_cove") and not _source.call(&"is_advancing")):
		_passage = {}
		_map.call(&"set_passage", [], 0.0)
		return
	_map.call(&"set_passage", _passage["path"], fraction)


func _remember_alerts() -> void:
	for r: Dictionary in Kit.dicts(_chart.get("regions")):
		_alert_floor[StringName(Kit.get_str(r, "id"))] = Kit.get_int(r, "alert")


# --- fitting the column ---------------------------------------------------------------------------

## Width of the order column: a third of the screen, within these bounds (logical px at the UI
## scale), so the chart keeps most of the width at 100% and the column still fits at 200% (L1715).
## Past that the page scrolls vertically: the chart keeps a usable minimum height.
const SIDE_MIN := 380.0
const SIDE_MAX := 640.0
## The tabs never shrink below this; past it the column scrolls on one axis (L1715).
const TABS_MIN := 300.0
## Narrower than this (logical px) the column moves under the chart.
const NARROW_W := 1400.0
const MAP_MIN := Vector2(320, 420)
var _fit_queued := false


func _queue_fit() -> void:
	if _fit_queued:
		return
	_fit_queued = true
	_fit.call_deferred()


## Size the column to the screen and let the tabs take the height the column has left. Below
## `NARROW_W` logical px (a large UI scale) the chart takes the full width on top with the column
## under it, and the page scrolls on one axis (L1715): the chart keeps the scale it has at 100%.
func _fit() -> void:
	_fit_queued = false
	if not is_inside_tree():
		return
	var body: BoxContainer = $Page/Layout/Body
	var scroll: ScrollContainer = $Page/Layout/Body/Side/Margin/Scroll
	var column: VBoxContainer = $Page/Layout/Body/Side/Margin/Scroll/Column
	var narrow := size.x < NARROW_W
	if body.vertical != narrow:
		body.vertical = narrow
	if narrow:
		_side.custom_minimum_size.x = 0.0
		_map.custom_minimum_size = Vector2(0.0, floorf(size.x * 0.75))
		scroll.vertical_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
		_tabs.custom_minimum_size.y = TABS_MIN
		return
	scroll.vertical_scroll_mode = ScrollContainer.SCROLL_MODE_AUTO
	_map.custom_minimum_size = MAP_MIN
	var w := clampf(size.x * 0.34, SIDE_MIN, SIDE_MAX)
	if not is_equal_approx(_side.custom_minimum_size.x, w):
		_side.custom_minimum_size.x = w
	var others := column.get_combined_minimum_size().y - _tabs.get_combined_minimum_size().y
	var want := maxf(TABS_MIN, scroll.size.y - others)
	if absf(_tabs.custom_minimum_size.y - want) > 1.0:
		_tabs.custom_minimum_size.y = want


# --- tabs and focus ------------------------------------------------------------------------------

func _set_tab_titles() -> void:
	if not is_node_ready():
		return
	_tabs.set_tab_title(0, tr(&"chart.tab.operations"))
	_tabs.set_tab_title(1, tr(&"chart.tab.cove"))
	_tabs.set_tab_title(2, tr(&"chart.tab.bulletin"))
	var unread := int(_log.call(&"unread_count")) if _log.has_method(&"unread_count") else 0
	_tabs.set_tab_title(LOG_TAB, tr(&"chart.tab.log_unread").format([Kit.number(unread, &"none", 0)]) if unread > 0 else tr(&"chart.tab.log"))


func _on_tab_changed(tab: int) -> void:
	if tab == LOG_TAB and _log.has_method(&"mark_read"):
		_log.call(&"mark_read")
	_set_tab_titles()
	var panel := _tabs.get_current_tab_control()
	if panel != null and panel.has_method(&"refresh"):
		panel.call(&"refresh")
	_queue_focus_chain()
	_queue_fit()


func _queue_focus_chain() -> void:
	if _focus_dirty:
		return
	_focus_dirty = true
	_chain_focus.call_deferred()


## One vertical chain down the column (proposals, the orders, the tab bar, the open tab's
## controls); the orders row also chains left to right; Left from the column and Right from the
## chart cross between them; Up from the top of either reaches the clock bar.
func _chain_focus() -> void:
	_focus_dirty = false
	if not is_inside_tree():
		return
	var column: Array[Control] = []
	column.append_array(_controls_of(_proposals))
	if _transit.visible and not _transit.disabled:
		column.append(_transit)
	var row: Array[Control] = []
	for c: Control in [_wait_hours, _wait]:
		if c.visible and not (c is BaseButton and (c as BaseButton).disabled):
			row.append(c)
	if not row.is_empty():
		column.append(row[0])
	column.append(_tabs.get_tab_bar())
	var tab_panel := _tabs.get_current_tab_control()
	if tab_panel != null:
		column.append_array(_controls_of(tab_panel))
	var live := Kit.chain_vertical(column)
	for c: Control in live:
		c.focus_neighbor_left = c.get_path_to(_map)
	if not row.is_empty():
		Kit.chain_horizontal(row)
		row[0].focus_neighbor_left = row[0].get_path_to(_map)
		for c: Control in row.slice(1):
			c.focus_neighbor_top = row[0].focus_neighbor_top
			c.focus_neighbor_bottom = row[0].focus_neighbor_bottom
			c.focus_next = row[0].focus_next
			c.focus_previous = row[0].focus_previous
	var clock_controls: Array[Control] = []
	clock_controls.assign(_clock.call(&"controls"))
	var top: Control = clock_controls[0] if not clock_controls.is_empty() else null
	if not live.is_empty():
		_map.focus_neighbor_right = _map.get_path_to(live[0])
		# Shift+Tab (the one focus key left, L1738) leaves the chart for the column at once.
		_map.focus_previous = _map.get_path_to(live[0])
		_map.focus_next = _map.get_path_to(live[0])
		if top != null:
			live[0].focus_neighbor_top = live[0].get_path_to(top)
	if top != null:
		_map.focus_neighbor_top = _map.get_path_to(top)
		Kit.chain_horizontal(clock_controls)
		for c: Control in clock_controls:
			c.focus_neighbor_bottom = c.get_path_to(live[0] if not live.is_empty() else _map)
	else:
		_map.focus_neighbor_top = _map.get_path_to(_map)


func _controls_of(panel: Control) -> Array[Control]:
	var out: Array[Control] = []
	if panel != null and panel.visible and panel.has_method(&"focus_controls"):
		for c: Variant in panel.call(&"focus_controls"):
			if c is Control and is_instance_valid(c):
				out.append(c)
	return out


# --- input ---------------------------------------------------------------------------------------

func _unhandled_input(event: InputEvent) -> void:
	if not is_visible_in_tree():
		return
	var screens := Kit.autoload(&"Screens")
	var on_top := screens == null or (int(screens.call(&"depth")) <= 1 and int(screens.call(&"modal_count")) == 0)
	if not on_top:
		return
	# Esc, the pad's Start (`menu`) or its back button open the pause menu over the table.
	if event.is_action_pressed(&"ui_cancel") or (InputMap.has_action(&"menu") and event.is_action_pressed(&"menu")):
		if screens != null:
			screens.call(&"push", &"pause_menu", {"context": &"chart"})
		get_viewport().set_input_as_handled()
	elif InputMap.has_action(&"pause") and event.is_action_pressed(&"pause"):
		if _clock.call(&"toggle_hold") == true:
			get_viewport().set_input_as_handled()
