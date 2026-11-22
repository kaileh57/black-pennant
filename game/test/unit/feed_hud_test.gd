extends SceneTree
## UI glue fed by BpTestFeed scripts, headless (DESIGN.md L1304: the suite loads the real gdclient
## library and feeds it scripted snapshots instead of mocking Rust classes). Unit: ui-tests.
## Run from game/:
##   godot --headless --path . -s res://test/unit/feed_hud_test.gd
## Each script runs as the live session through the real router, so Screens builds the world and
## the sortie HUD exactly as in play, and the checks read the HUD's own controls:
## - `calm`, 30 s (the structural half of the L1755 calm capture; the pixel half is
##   res://test/capture/calm_capture.tscn): no alert reaches Hud or the strip, the strip stays
##   hidden, no officer line, no toast, no running tween without a `sim_signal`, and every control
##   that changes between 5 s and 30 s is one of the sim-driven readouts listed in CALM_MAY_CHANGE.
## - `battle` to the sinking Emergency: the strip shows Hud's alerts in Hud's order, highest first.
## - `alert_flood`: flood mode reaches the strip; at most five rows, "+N" for the rest.
## - `track_events`: the track table holds one row per Hud track as tracks appear, merge and split.
## Exits 1 on any failure.

const Gates := preload("res://test/unit/gates.gd")
const TICK_HZ := 60
## Ticks a frame: a tenth of a second, so 10 Hz HUD sections update every frame or two.
const CHUNK := 6
## Controls that may change on a calm sea: each is a readout of a simulation value that keeps
## moving while nothing happens (the sortie clock, the ship's position and the sea it rides). A
## path matches when it contains one of these node names.
const CALM_MAY_CHANGE: Array[String] = ["PacingBar", "HelmCluster", "BridgeOverlay", "Reticle", "PlotTable", "Objectives"]

var _failures := 0
var _checks := 0
var _feed: Node
var _ticks := 0
var _hud_node: Node
var _alerts_raised := 0
var _lines := 0


func _initialize() -> void:
	_run.call_deferred()


func _ok(cond: bool, what: String) -> void:
	_checks += 1
	if not cond:
		_failures += 1
		printerr("FAIL: ", what)


func _frame() -> void:
	# Quality caps the frame rate at start-up; the feed's clock is the only one that matters.
	Engine.max_fps = 0
	await process_frame


func _run() -> void:
	await _frame()
	root.get_node("Screens").call(&"start_routing")
	var hud: Node = root.get_node("Hud")
	hud.connect(&"alert_raised", func(_a: Dictionary) -> void: _alerts_raised += 1)
	hud.connect(&"officer_line", func(_l: Dictionary) -> void: _lines += 1)
	await _calm()
	await _battle()
	await _flood()
	await _tracks()
	if _feed != null:
		_feed.call(&"stop")
		_feed.queue_free()
	await _frame()
	print("feed_hud_test: %d checks, %d failures" % [_checks, _failures])
	quit(1 if _failures > 0 else 0)


## Install a script and play until the HUD is up. False when it never came up.
func _start(script: String) -> bool:
	if _feed != null:
		_feed.call(&"stop")
		_feed.queue_free()
		_feed = null
		for _i: int in range(3):
			await _frame()
	_feed = ClassDB.instantiate(&"BpTestFeed")
	root.add_child(_feed)
	_ticks = 0
	if not bool(_feed.call(&"start", script)):
		_ok(false, "BpTestFeed.start(%s) refused" % script)
		return false
	_feed.call(&"advance", 12)
	_ticks = 12
	for _i: int in range(120):
		await _frame()
		_hud_node = _find_hud()
		if _hud_node != null and int(root.get_node("Session").call(&"state")) == Bp.SESSION_IN_SORTIE:
			return true
		_play(CHUNK)
	_ok(false, "%s: the sortie HUD never came up" % script)
	return false


func _play(ticks: int) -> void:
	_feed.call(&"advance", ticks)
	_ticks += ticks


func _until(seconds: float, chunk: int = CHUNK) -> void:
	while _ticks < int(seconds * TICK_HZ):
		_play(chunk)
		await _frame()


func _find_hud() -> Node:
	var found := root.find_children("SortieHud", "CanvasLayer", true, false)
	return null if found.is_empty() else found[0]


func _strip() -> Control:
	return _hud_node.get_node_or_null(^"Frame/Docks/Centre/Margin/Column/Top/Middle/AlertStrip") as Control


func _calm() -> void:
	if not await _start("calm"):
		return
	var hud: Node = root.get_node("Hud")
	var screens: Node = root.get_node("Screens")
	var toast: Node = screens.get(&"_toast")
	_alerts_raised = 0
	_lines = 0
	var untraced := {}
	var strip_seen := false
	var toast_seen := false
	await _until(5.0)
	var before := Gates.control_state(_hud_node)
	while _ticks < 30 * TICK_HZ:
		_play(CHUNK)
		await _frame()
		strip_seen = strip_seen or _strip().is_visible_in_tree()
		toast_seen = toast_seen or (toast != null and bool(toast.call(&"is_showing")))
		for t: String in Gates.untraced_tweens(self):
			untraced[t] = true
	var after := Gates.control_state(_hud_node)
	_ok(_alerts_raised == 0, "calm: %d alerts raised" % _alerts_raised)
	_ok((hud.call(&"alerts", false) as Array).is_empty(), "calm: Hud holds alerts %s" % str(hud.call(&"alerts", false)))
	_ok(((hud.call(&"alert_strip") as Dictionary).get("shown", []) as Array).is_empty(), "calm: the strip data is not empty")
	_ok(not strip_seen, "calm: the alert strip was shown")
	_ok(_lines == 0, "calm: %d officer lines" % _lines)
	_ok(not toast_seen, "calm: a toast showed")
	_ok(untraced.is_empty(), "calm: tweens with no sim_signal ran: %s" % str(untraced.keys()))
	_ok(before.size() >= 20, "calm: only %d HUD controls visible; the HUD did not draw" % before.size())
	var changed := Gates.changed_paths(before, after)
	var unexplained := PackedStringArray()
	for p: String in changed:
		var ok := false
		for allowed: String in CALM_MAY_CHANGE:
			ok = ok or p.contains(allowed)
		if not ok:
			unexplained.append(p)
	print("feed_hud_test: calm 5 s to 30 s, %d controls visible, %d changed, %d unexplained" % [before.size(), changed.size(), unexplained.size()])
	for p: String in unexplained:
		print("  ", p, "\n    before ", before.get(p, "<absent>"), "\n    after  ", after.get(p, "<absent>"))
	_ok(unexplained.is_empty(), "calm: controls changed with nothing happening")


func _battle() -> void:
	if not await _start("battle"):
		return
	var hud: Node = root.get_node("Hud")
	# 425 s: the sinking Emergency is up beside the flooding and fire Alarms and the power Warning.
	await _until(425.0, CHUNK * 5)
	await _frame()
	var strip_data: Dictionary = hud.call(&"alert_strip")
	var shown: Array = strip_data.get("shown", [])
	var strip := _strip()
	_ok(shown.size() >= 3, "battle: only %d alerts on the strip at 425 s" % shown.size())
	_ok(strip.is_visible_in_tree(), "battle: the alert strip is hidden with alerts up")
	var rows: Array = strip.get(&"_rows")
	var visible_rows := 0
	for r: Control in rows:
		visible_rows += 1 if r.visible else 0
	_ok(visible_rows == mini(shown.size(), 5), "battle: %d rows drawn for %d alerts" % [visible_rows, shown.size()])
	var drawn: Array = strip.get(&"_shown")
	var ids_hud: Array = shown.map(func(a: Dictionary) -> int: return int(a.get("id", -1)))
	var ids_strip: Array = drawn.map(func(a: Dictionary) -> int: return int(a.get("id", -1)))
	_ok(ids_hud == ids_strip, "battle: the strip order %s is not Hud's %s" % [ids_strip, ids_hud])
	if not shown.is_empty():
		_ok(int((shown[0] as Dictionary).get("priority", -1)) == Bp.PRIORITY_EMERGENCY, "battle: the top row is not the Emergency")
	for i: int in range(1, shown.size()):
		_ok(int((shown[i - 1] as Dictionary).get("priority", 0)) >= int((shown[i] as Dictionary).get("priority", 0)), "battle: strip not in priority order")
	var ack: Button = strip.get(&"_ack")
	_ok(not ack.disabled, "battle: Acknowledge is disabled with unacknowledged alerts")


func _flood() -> void:
	if not await _start("alert_flood"):
		return
	var hud: Node = root.get_node("Hud")
	var strip := _strip()
	var flood_seen := false
	var most := 0
	var more_ok := true
	while _ticks < 600 * TICK_HZ and not flood_seen:
		_play(CHUNK * 10)
		await _frame()
		var d: Dictionary = hud.call(&"alert_strip")
		var shown: Array = d.get("shown", [])
		most = maxi(most, shown.size())
		if int(d.get("more", 0)) > 0:
			var more: Button = strip.get(&"_more")
			more_ok = more_ok and more.is_visible_in_tree() and more.text.contains(str(int(d.get("more", 0))))
		if bool(d.get("flood", false)):
			flood_seen = true
			await _frame()
			var flood: Label = strip.get(&"_flood")
			_ok(flood.is_visible_in_tree() and not flood.text.is_empty(), "flood: the strip does not say it is in flood mode")
	_ok(flood_seen, "flood: Hud never entered flood mode in 600 s")
	_ok(most <= 5, "flood: %d alerts on the strip (limit 5)" % most)
	_ok(more_ok, "flood: '+N' missing or wrong while alerts overflowed")
	print("feed_hud_test: flood at %.0f s, most shown %d" % [float(_ticks) / TICK_HZ, most])


func _tracks() -> void:
	if not await _start("track_events"):
		return
	var hud: Node = root.get_node("Hud")
	var events := {}
	var on_event := func(kind: StringName, _a: int, _b: int) -> void: events[kind] = int(events.get(kind, 0)) + 1
	hud.connect(&"track_event", on_event)
	var list := _hud_node.get_node(^"Frame/Docks/LeftDock/Column/TrackList")
	var table: Tree = list.get(&"_table")
	var mismatches := 0
	var samples := 0
	var most := 0
	while _ticks < 180 * TICK_HZ:
		_play(CHUNK * 5)
		await _frame()
		await _frame()
		# The list leaves out the own hull, which the side picture also holds as a track.
		var own := int((hud.call(&"motion") as Dictionary).get("vessel", Bp.NONE))
		var n := 0
		for t: Dictionary in hud.call(&"tracks"):
			n += 1 if int(t.get("id", 0)) > 0 and (own == Bp.NONE or int(t.get("vessel", Bp.NONE)) != own) else 0
		var root_item := table.get_root()
		var rows := 0 if root_item == null else root_item.get_child_count()
		most = maxi(most, n)
		samples += 1
		if rows != n:
			mismatches += 1
			if mismatches <= 3:
				print("  at %.1f s the table has %d rows for %d tracks" % [float(_ticks) / TICK_HZ, rows, n])
	hud.disconnect(&"track_event", on_event)
	print("feed_hud_test: track events %s, most tracks %d, %d of %d samples mismatched" % [events, most, mismatches, samples])
	_ok(most > 0, "tracks: no track ever reached Hud")
	_ok(events.size() >= 3, "tracks: only %s track events reached Hud" % str(events.keys()))
	_ok(mismatches == 0, "tracks: the table disagreed with Hud in %d of %d samples" % [mismatches, samples])
