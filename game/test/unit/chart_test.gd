extends SceneTree
## Headless checks for the chart table. Units ui-chart, ui-chart-panels, ui-chart-cove. Run from
## game/ with a throwaway HOME, because the live part starts and saves a real campaign:
##   HOME=/tmp/bp-chart-test godot --headless --path . -s res://test/unit/chart_test.gd
## Part one drives the table from the fixture source (res://test/ui/chart_fixture.gd): orders,
## proposals, Intel spends, cove services, the log's unread marks and filters, keyboard steps on the
## chart, the clock bar's Continue rule. Part two runs a real campaign through the Campaign autoload:
## a transit committed from the table, theatre time animated to the arrival interrupt, the passage
## drawn and cleared. Every text key the chart scripts use must resolve. Exits 1 on any failure.

## Loaded at run time: the chart scripts name autoloads, which a -s script's preloads cannot see.
var TABLE: PackedScene
var Fixture: GDScript
var LogPanel: GDScript
const CHART_DIR := "res://ui/chart/"
const LIMIT_FRAMES := 1500
const PHASE_CHART := 1
const PHASE_BRIEFING := 3
const PHASE_DRY_DOCK := 4

var _failures := 0
var _frame := 0
var _step := 0
var _table: Control
var _campaign: Node
var _interrupts: Array[Dictionary] = []
var _hours := 0
var _start_frame := 0


func _initialize() -> void:
	Engine.max_fps = 60


func _process(_delta: float) -> bool:
	_frame += 1
	match _step:
		0:
			if _frame < 3:
				return false
			TABLE = load("res://ui/chart/chart_table.tscn")
			Fixture = load("res://test/ui/chart_fixture.gd")
			LogPanel = load("res://ui/chart/theatre_log.gd")
			_check_keys()
			_fixture_setup()
			_step = 10
		10, 11:
			# Two frames for the containers to lay the table out.
			_step += 1
		12:
			_fixture_checks()
			_step = 1
		1:
			if not _live_start():
				return _finish()
			_step = 2
			_start_frame = _frame
		2:
			# A transit from the table, animated to its arrival.
			if not _interrupts.is_empty() and not _campaign.call(&"is_advancing"):
				_live_arrived()
				_live_wait()
				_step = 3
				_start_frame = _frame
			elif _timed_out("the transit never arrived"):
				return _finish()
		3:
			# A 72 h wait: any operation offered on the way is brought forward in the list.
			if not _campaign.call(&"is_advancing"):
				_live_wait_done()
				_live_dock()
				_step = 4
				_start_frame = _frame
			elif _timed_out("the wait never ended"):
				return _finish()
		4:
			# The dock closed by hand: the phase falls back to the chart.
			if int(_campaign.call(&"phase")) == PHASE_CHART:
				_live_router()
				_step = 5
				_start_frame = _frame
			elif _timed_out("the phase did not return from the dry dock"):
				return _finish()
		5, 6, 7:
			# The router's chart: Esc opens the pause menu over it, Esc again closes it.
			if _frame - _start_frame >= 3:
				_live_router_step(_step)
				_step += 1
				_start_frame = _frame
		8:
			# The player's hold outlives the pause menu, which releases the clock as it closes.
			# Two seconds at 1 h/s: an unheld clock would run at least one hour.
			if _frame - _start_frame >= 120:
				_expect(_hours == _hours_at_hold, "time ran on after the pause menu closed over a held clock (%d to %d)" % [_hours_at_hold, _hours])
				var pause: Button = _routed.get_node(^"Page/Layout/TopBar/Row/ClockBar/Pause")
				pause.emit_signal(&"pressed")
				_campaign.call(&"set_advance_speed", 48.0)
				_step = 9
				_start_frame = _frame
		9:
			if not _campaign.call(&"is_advancing"):
				_live_brief()
				return _finish()
			elif _timed_out("the held wait never ended"):
				return _finish()
	return false


func _timed_out(what: String) -> bool:
	if _frame - _start_frame <= LIMIT_FRAMES:
		return false
	_fail("%s (%d hours, interrupts %s)" % [what, _hours, str(_interrupts)])
	return true


func _fail(message: String) -> void:
	_failures += 1
	printerr("chart_test: FAIL ", message)


func _expect(cond: bool, message: String) -> void:
	if not cond:
		_fail(message)


func _finish() -> bool:
	for part: String in ["fixture", "live", "router"]:
		_expect(_parts_done.has(part), "the %s part did not run to its end" % part)
	if _table != null and is_instance_valid(_table):
		_table.free()
	# Closing the campaign routes to the main menu, whose facades are other units' work: stop routing.
	var screens := root.get_node_or_null(^"Screens")
	if screens != null:
		screens.set(&"routing", false)
		screens.call(&"clear")
	if _campaign != null and _campaign.call(&"is_active"):
		var slot: Dictionary = _campaign.call(&"current_slot")
		_campaign.call(&"close_campaign")
		var path := String(slot.get("path", ""))
		if not path.is_empty():
			_campaign.call(&"delete_slot", path)
	print("chart_test: %s (%d failures)" % ["ok" if _failures == 0 else "FAILED", _failures])
	quit(1 if _failures > 0 else 0)
	return true


# --- text keys ----------------------------------------------------------------------------------

## Every literal chart.* key in the chart scripts and scenes, and every dynamic family, resolves.
func _check_keys() -> void:
	var re := RegEx.create_from_string("\"(chart\\.[a-z_.]+[a-z_])\"")
	var keys: Dictionary[String, bool] = {}
	for f: String in DirAccess.get_files_at(CHART_DIR):
		if f.get_extension() != "gd" and f.get_extension() != "tscn":
			continue
		var text := FileAccess.get_file_as_string(CHART_DIR + f)
		for m: RegExMatch in re.search_all(text):
			keys[m.get_string(1)] = true
	for kind: String in ["cove", "shipyard", "depot", "base", "harbour", "radar_site", "esm_site", "sea", "group", "convoy", "offer", "lane", "cut", "route", "closed"]:
		keys["chart.legend." + kind] = true
	for f: String in ["all", "own", "combine", "operations"]:
		keys["chart.log.filter." + f] = true
	for r: String in ["salvage", "intel", "fuel", "ammo"]:
		keys["chart.res.%s_tip" % r] = true
	var missing := PackedStringArray()
	for k: String in keys:
		if k.ends_with(".") or k.ends_with("_"):
			continue
		if String(TranslationServer.translate(StringName(k))) == k:
			missing.append(k)
	_expect(missing.is_empty(), "text keys with no English: %s" % ", ".join(missing))
	_expect(keys.size() > 150, "only %d chart keys found; the scan is broken" % keys.size())


# --- fixture ------------------------------------------------------------------------------------

func _node(path: String) -> Node:
	return _table.get_node(NodePath(path))


var _fx: RefCounted
## Set at the end of each part, so a script error that cuts a part short still fails the run.
var _parts_done: Array[String] = []


func _fixture_setup() -> void:
	_fx = Fixture.new()
	_fx.set(&"state", _fx.call(&"busy_state"))
	_table = TABLE.instantiate()
	root.add_child(_table)
	_table.call(&"use_source", _fx)


func _fixture_checks() -> void:
	var fx := _fx
	var state: Dictionary = fx.get(&"state")
	var map: Control = _node("Page/Layout/Body/ChartMap")
	var transit: Button = _node("Page/Layout/Body/Side/Margin/Scroll/Column/Place/Actions/Transit")
	var wait: Button = _node("Page/Layout/Body/Side/Margin/Scroll/Column/Place/WaitRow/Wait")
	var sent: Array = fx.get(&"sent")

	# SEXTANT's own place offers no transit; a reachable cove offers one, worded for co-op.
	_expect(map.call(&"selected") == &"cove.pell", "the table did not start on SEXTANT's cove: %s" % map.call(&"selected"))
	_expect(transit.disabled, "a transit to SEXTANT's own cove is offered")
	map.call(&"select", &"cove.wren", true)
	_expect(not transit.disabled, "the reachable cove offers no transit")
	_expect(transit.text.contains("Wren Cove") and transit.text.begins_with("Propose"), "co-op transit text: %s" % transit.text)
	transit.emit_signal(&"pressed")
	_expect(sent.any(func(s: Dictionary) -> bool: return s.has("propose") and int(s["propose"].get("kind", -1)) == 0 and s["propose"].get("node") == &"cove.wren"), "Transit sent no transit proposal: %s" % str(sent))
	wait.emit_signal(&"pressed")
	_expect(sent.any(func(s: Dictionary) -> bool: return s.has("propose") and int(s["propose"].get("kind", -1)) == 2 and int(s["propose"].get("hours", 0)) == 12), "Wait sent no 12 h wait: %s" % str(sent))

	# Keyboard steps: from Wren Cove, up goes to a node above it; a direction with nothing leaves.
	var up: StringName = map.call(&"step", Vector2.UP)
	_expect(up != &"" and float(map.call(&"node_position", up).y) < float(map.call(&"node_position", &"cove.wren").y), "step up from Wren Cove gave %s" % up)
	map.call(&"select", &"cove.wren", false)
	var down: StringName = map.call(&"step", Vector2.DOWN)
	_expect(down == &"" or float(map.call(&"node_position", down).y) > float(map.call(&"node_position", &"cove.wren").y), "step down from the southernmost cove gave %s" % down)
	_expect(int(map.call(&"skipped_polygons")) < 20, "%d coast polygons do not fill" % int(map.call(&"skipped_polygons")))

	# Operations: three offers, the first selected, Open briefing sends, Decline asks first.
	var ops: Control = _node("Page/Layout/Body/Side/Margin/Scroll/Column/Tabs/Operations")
	var rows: Array = ops.call(&"focus_controls")
	_expect(rows.size() >= 3, "operations list has %d controls" % rows.size())
	_expect(ops.call(&"select_operation", &"op.act1.feint") == true and ops.call(&"selected_operation") == &"op.act1.feint", "select_operation did not select the feint")
	_expect(ops.call(&"select_operation", &"op.nowhere") == false, "an unknown operation was selected")
	(_node("Page/Layout/Body/Side/Margin/Scroll/Column/Tabs/Operations/Actions/Brief") as Button).emit_signal(&"pressed")
	_expect(sent.any(func(s: Dictionary) -> bool: return s.get("open_briefing") == &"op.act1.feint"), "Open briefing sent nothing")

	# Bulletin: the 5 Intel cancel is closed with 4 in the pool; the 2 Intel delay is open.
	var bulletin: Control = _node("Page/Layout/Body/Side/Margin/Scroll/Column/Tabs/Bulletin")
	_table.get_node("Page/Layout/Body/Side/Margin/Scroll/Column/Tabs").set(&"current_tab", 2)
	bulletin.call(&"refresh")
	var spends: Array = bulletin.get(&"_buttons")
	var open_spends := 0
	var closed_spends := 0
	for b: Button in spends:
		if b.disabled:
			closed_spends += 1
		else:
			open_spends += 1
	_expect(closed_spends == 1 and open_spends == 3, "Intel spends open %d, closed %d (want 3 and 1)" % [open_spends, closed_spends])

	# Proposals: the partner's proposal can be accepted, declined or confirmed; ours withdrawn.
	var proposals: Control = _node("Page/Layout/Body/Side/Margin/Scroll/Column/ProposalsPanel")
	_expect(proposals.visible, "open proposals are hidden")
	var labels := PackedStringArray()
	for b: Button in proposals.call(&"focus_controls"):
		labels.append(String(TranslationServer.translate(StringName(b.text))))
	_expect(labels.has("Accept") and labels.has("Withdraw") and labels.count("Confirm as host") == 2, "proposal answers: %s" % str(labels))

	# Cove: repair 180 with 1,240 in the pool is open, rearm buys a 45 shortfall, save is open.
	var cove: Control = _node("Page/Layout/Body/Side/Margin/Scroll/Column/Tabs/Cove")
	var repair: Button = cove.get_node(^"Column/Services/Repair")
	_expect(repair.visible and not repair.disabled, "repair is closed: %s" % repair.tooltip_text)
	_expect(not (cove.get_node(^"Column/Services/RepairPart") as Button).visible, "a part repair is offered though the pool pays in full")
	_expect((cove.get_node(^"Column/Saving/Save") as Button).disabled == false, "save is closed at a cove")
	repair.emit_signal(&"pressed")
	_expect(sent.any(func(s: Dictionary) -> bool: return s.get("buy") == &"repair_full"), "Repair sent nothing")

	# Resources: a change boxes the value that changed, and only it.
	var res: Control = _node("Page/Layout/TopBar/Row/ResourcesBar")
	var r2: Dictionary = (state["resources"] as Dictionary).duplicate()
	r2["salvage"] = 1060
	res.call(&"show_resources", r2)
	_expect(res.call(&"boxed") == [&"salvage"], "boxed after a Salvage change: %s" % str(res.call(&"boxed")))

	# Log: newest first, five unread until read, filters by category, rows name their place.
	var log: Control = _node("Page/Layout/Body/Side/Margin/Scroll/Column/Tabs/Log")
	_expect(int(log.call(&"unread_count")) == 5, "log unread %d, want 5" % int(log.call(&"unread_count")))
	log.call(&"mark_read")
	_expect(int(log.call(&"unread_count")) == 0, "log still unread after reading")
	_expect(LogPanel.call(&"category", &"lane_cut") == &"combine" and LogPanel.call(&"category", &"sextant_moved") == &"own" and LogPanel.call(&"category", &"act_won") == &"operations", "log categories")
	_expect(LogPanel.call(&"target_of", {"kind": &"lane_cut", "a": &"depot.skerrow", "b": &"sea.saddle"}) == &"depot.skerrow", "lane event target")

	# Clock bar: Continue only after an interrupt that leaves time to run.
	var clock: Control = _node("Page/Layout/TopBar/Row/ClockBar")
	clock.call(&"on_interrupted", {"kind": 3, "text": {"key": &"campaign.interrupt.operation_offered", "args": ["x"]}})
	_expect(clock.call(&"can_continue") == true, "no Continue after an offer stopped a transit")
	clock.call(&"on_interrupted", {"kind": 0, "text": {"key": &"campaign.interrupt.arrived", "args": ["x"]}})
	_expect(clock.call(&"can_continue") == false, "Continue offered after arriving")
	_expect((clock.get_node(^"Time/Hunter") as Label).visible, "the hunter warning is hidden")

	# An interrupt names an offer that arrived in the same hour: the table reads the new chart before
	# bringing it forward (Campaign emits the interrupt before the chart change).
	var fresh := {"id": &"op.act1.depot_strike", "kind": 0, "template": 1, "name": &"op.act1.depot_strike.name", "node": &"depot.skerrow", "region": &"region.gannet", "forced": false, "transit_h": 11}
	(state["chart"]["offers"] as Array).append(fresh)
	fx.set(&"live", true)
	_table.call(&"_on_interrupted", {"kind": 3, "text": {"key": &"campaign.interrupt.operation_offered", "args": [{"key": &"op.act1.depot_strike.name"}]}, "operation": &"op.act1.depot_strike"})
	_expect(ops.call(&"selected_operation") == &"op.act1.depot_strike", "the newly offered operation is not brought forward: %s" % ops.call(&"selected_operation"))
	fx.set(&"live", false)

	# The empty state: no campaign, nothing offered, nothing breaks.
	var empty: RefCounted = Fixture.new()
	_table.call(&"use_source", empty)
	_expect(transit.disabled and wait.disabled, "orders are open with no campaign")
	_expect(not proposals.visible, "proposals show with no campaign")
	_table.free()
	_table = null
	_parts_done.append("fixture")


# --- live ---------------------------------------------------------------------------------------

func _live_start() -> bool:
	_campaign = root.get_node_or_null(^"Campaign")
	if _campaign == null:
		_fail("no Campaign autoload")
		return false
	if _campaign.call(&"suspend_slot") != null:
		_fail("a suspend slot exists; run with a throwaway HOME")
		return false
	var problems: Array = _campaign.call(&"new_campaign", {"slot_name": "Chart test", "ironman": false, "seed": 4242, "starter": "starters/fac22.bp.ron", "tutorial_done": true,
		"difficulty": {"preset": &"normal", "ai_accuracy": 1.0, "ai_reaction_s": 2.0, "adaptation_cadence": 1.0, "hull_count": 300, "economy": 1.0, "clock_days": 40}})
	if not problems.is_empty():
		_fail("new_campaign refused: %s" % str(problems))
		return false
	_campaign.connect(&"interrupted", func(i: Dictionary) -> void: _interrupts.append(i))
	_campaign.connect(&"hour_advanced", func(_h: int) -> void: _hours += 1)
	_campaign.call(&"set_advance_speed", 48.0)
	_table = TABLE.instantiate()
	root.add_child(_table)
	var map: Control = _node("Page/Layout/Body/ChartMap")
	map.call(&"select", &"", false)
	_table.call(&"refresh")
	_expect(map.call(&"selected") == StringName(_campaign.call(&"chart").get("sextant")), "the live table did not start on SEXTANT")
	_expect((_node("Page/Layout/Body/Side/Margin/Scroll/Column/Tabs/Operations") as Control).call(&"focus_controls").size() >= 2, "no live operations listed")
	map.call(&"select", &"cove.pell", true)
	var transit: Button = _node("Page/Layout/Body/Side/Margin/Scroll/Column/Place/Actions/Transit")
	if transit.disabled:
		_fail("no transit to Pell Cove: %s" % transit.tooltip_text)
		return false
	transit.emit_signal(&"pressed")
	_expect(_campaign.call(&"is_advancing") == true, "the transit did not start theatre time")
	_time_hour_refresh()
	_expect(not (map.get(&"_passage") as Array).is_empty(), "the committed passage is not drawn")
	_expect(transit.disabled and (_node("Page/Layout/Body/Side/Margin/Scroll/Column/Place/WaitRow/Wait") as Button).disabled, "orders stay open while time runs")
	_expect((_node("Page/Layout/TopBar/Row/ClockBar/Pause") as Button).visible, "no Hold time while time runs")
	return true


## Time one full re-read of the table, as each theatre hour causes.
func _time_hour_refresh() -> void:
	var t0 := Time.get_ticks_usec()
	for i in 10:
		_table.call(&"_on_chart_changed")
	var us := (Time.get_ticks_usec() - t0) / 10
	print("chart_test: one chart change re-reads the table in %d us" % us)
	_expect(us < 4000, "a chart change costs %d us" % us)


func _live_arrived() -> void:
	var map: Control = _node("Page/Layout/Body/ChartMap")
	var last: Dictionary = _interrupts[-1]
	print("chart_test: live transit stopped after %d hours: %s" % [_hours, str(last.get("text"))])
	if int(last.get("kind", -1)) == 0:
		_expect((map.get(&"_passage") as Array).is_empty(), "the passage is still drawn after arriving")
		_expect(map.call(&"selected") == &"cove.pell", "the arrival cove is not selected")
		var state: Label = _node("Page/Layout/TopBar/Row/ClockBar/Time/State")
		_expect(state.text.contains("Pell Cove"), "the clock bar does not say SEXTANT arrived: %s" % state.text)
		_expect(not (_node("Page/Layout/Body/Side/Margin/Scroll/Column/Place/WaitRow/Wait") as Button).disabled, "Wait is closed after arriving")
	_expect(int(_node("Page/Layout/Body/Side/Margin/Scroll/Column/Tabs/Log").call(&"unread_count")) > 0, "the log has nothing new after a transit")
	_parts_done.append("live")


func _live_wait() -> void:
	_interrupts.clear()
	var wait: Button = _node("Page/Layout/Body/Side/Margin/Scroll/Column/Place/WaitRow/Wait")
	var hours: OptionButton = _node("Page/Layout/Body/Side/Margin/Scroll/Column/Place/WaitRow/WaitHours")
	hours.select(4)
	hours.emit_signal(&"item_selected", 4)
	_expect(wait.text.contains("72"), "the wait button reads %s" % wait.text)
	wait.emit_signal(&"pressed")
	_expect(_campaign.call(&"is_advancing") == true, "the wait did not start theatre time")


func _live_wait_done() -> void:
	var ops: Control = _node("Page/Layout/Body/Side/Margin/Scroll/Column/Tabs/Operations")
	var offered := false
	for i: Dictionary in _interrupts:
		if int(i.get("kind", -1)) == 3 or int(i.get("kind", -1)) == 4:
			offered = true
			var op := StringName(i.get("operation", &""))
			# Later interrupts may have moved the selection on; the last offer is the one selected.
			if i == _interrupts[-1]:
				_expect(ops.call(&"selected_operation") == op, "the offered operation %s is not selected (%s)" % [op, ops.call(&"selected_operation")])
	print("chart_test: the wait stopped after %d hours with %d interrupts (an offer among them: %s)" % [_hours, _interrupts.size(), offered])


func _live_dock() -> void:
	var tabs: TabContainer = _node("Page/Layout/Body/Side/Margin/Scroll/Column/Tabs")
	tabs.current_tab = 1
	var dock: Button = _node("Page/Layout/Body/Side/Margin/Scroll/Column/Tabs/Cove/Column/Dock")
	if dock.disabled:
		_fail("Enter dry dock is closed at a cove: %s" % dock.tooltip_text)
		return
	dock.emit_signal(&"pressed")
	_expect(int(_campaign.call(&"phase")) == PHASE_DRY_DOCK, "Enter dry dock did not open the dock phase")
	var editor := root.get_node_or_null(^"Editor")
	_expect(editor != null and editor.call(&"is_open") == true, "the editor is not open after Enter dry dock")
	if editor != null:
		editor.call(&"close")


var _routed: Node
var _hours_at_hold := 0


func _live_router() -> void:
	var screens := root.get_node(^"Screens")
	if is_instance_valid(_table):
		_table.free()
	_table = null
	screens.call(&"start_routing")
	_routed = screens.call(&"change", &"chart")
	_expect(_routed != null and screens.call(&"top_name") == &"chart", "the router did not open the chart")


func _live_router_step(step: int) -> void:
	var screens := root.get_node(^"Screens")
	match step:
		5:
			var focus := root.gui_get_focus_owner()
			_expect(focus != null and _routed.is_ancestor_of(focus), "nothing in the chart took focus: %s" % str(focus))
			# A slow 6 h wait, held from the clock bar.
			_campaign.call(&"set_advance_speed", 1.0)
			var hours: OptionButton = _routed.get_node(^"Page/Layout/Body/Side/Margin/Scroll/Column/Place/WaitRow/WaitHours")
			hours.select(0)
			hours.emit_signal(&"item_selected", 0)
			(_routed.get_node(^"Page/Layout/Body/Side/Margin/Scroll/Column/Place/WaitRow/Wait") as Button).emit_signal(&"pressed")
			var pause: Button = _routed.get_node(^"Page/Layout/TopBar/Row/ClockBar/Pause")
			_expect(pause.visible, "no Hold time while the wait runs")
			pause.emit_signal(&"pressed")
			_hours_at_hold = _hours
			var ev := InputEventAction.new()
			ev.action = &"ui_cancel"
			ev.pressed = true
			Input.parse_input_event(ev)
		6:
			_expect(screens.call(&"top_name") == &"pause_menu", "Esc over the chart opened %s" % screens.call(&"top_name"))
			screens.call(&"back")
		7:
			_expect(screens.call(&"top_name") == &"chart", "the pause menu did not close back to the chart: %s" % screens.call(&"top_name"))


func _live_brief() -> void:
	var screens := root.get_node(^"Screens")
	if _routed == null or not is_instance_valid(_routed):
		_fail("the routed chart is gone")
		return
	var brief: Button = _routed.get_node(^"Page/Layout/Body/Side/Margin/Scroll/Column/Tabs/Operations/Actions/Brief")
	if brief.disabled:
		_fail("Open briefing is closed: %s" % brief.tooltip_text)
		return
	brief.emit_signal(&"pressed")
	_expect(int(_campaign.call(&"phase")) == PHASE_BRIEFING, "Open briefing did not open the briefing phase")
	_expect(screens.call(&"base_name") == &"briefing", "the router did not move to the briefing: %s" % screens.call(&"base_name"))
	_parts_done.append("router")
