extends SceneTree
## Headless checks for the briefing, after-action report, sortie summary and special-operations bar.
## Units: ui-briefing, ui-after-action, ui-hud-ops. Run from game/:
##   godot --headless --path . -s res://test/unit/ui_reports_test.gd
## Exits 1 on any failure. Orders, Campaign and Editor are swapped for in-memory fakes while the bar and the
## screens send commands, so nothing reaches a session; Tx, Content and Hud are the real ones.

const Samples := preload("res://test/capture/u5_samples.gd")
const CSVS: Array[String] = ["res://i18n/ui_briefing.csv", "res://i18n/ui_after_action.csv", "res://i18n/ui_hud_ops.csv"]
const MY_SCRIPTS: Array[String] = [
	"res://ui/screens/briefing.gd", "res://ui/screens/after_action.gd", "res://ui/screens/sortie_end.gd", "res://ui/hud/actions_bar.gd",
]


## Stands in for Orders: records every call and answers `accept`.
class FakeOrders extends Node:
	var calls: Array = []
	var accept := true

	func start_action(action: int, target: Dictionary) -> bool:
		calls.append(["start_action", action, target])
		return accept

	func cancel_action(id: int) -> bool:
		calls.append(["cancel_action", id])
		return accept

	func prize(prize_id: int, decision: int) -> bool:
		calls.append(["prize", prize_id, decision])
		return accept

	func offboard(slot: int, order: Dictionary) -> bool:
		calls.append(["offboard", slot, order])
		return accept


## Stands in for Campaign: records calls; `briefed` is the open operation.
class FakeCampaign extends Node:
	signal chart_changed
	signal proposal_changed(proposal: Dictionary, state: StringName)
	signal rejected(text: Dictionary)
	signal after_action_ready
	var calls: Array = []
	var briefed: StringName = &""
	var phase_value := 3
	var launch_ok := false

	func phase() -> int:
		return phase_value

	func briefed_operation() -> StringName:
		return briefed

	func briefing(op: StringName) -> Variant:
		return Samples.briefing() if op == briefed and op != &"" else null

	func chart() -> Dictionary:
		return {"proposals": [], "regions": Samples.regions()}

	func players() -> Array[Dictionary]:
		return [Samples.players()[0]]

	func launch_sortie() -> bool:
		calls.append(["launch_sortie"])
		if not launch_ok:
			rejected.emit({"key": &"campaign.reject.no_briefing", "args": []})
		return launch_ok

	func close_briefing() -> void:
		calls.append(["close_briefing"])

	func open_dry_dock() -> bool:
		calls.append(["open_dry_dock"])
		return true

	func propose(action: Dictionary) -> bool:
		calls.append(["propose", action])
		briefed = &""
		return true

	func after_action() -> Variant:
		return Samples.after_action(true)

	func act_info() -> Dictionary:
		return {"clock_days_left": 31.6}

	func close_after_action() -> void:
		calls.append(["close_after_action"])


## Stands in for the sortie's BpPlotView: the line and point picks.
class FakePlot extends Control:
	signal line_drawn(a: Vector2, b: Vector2)
	signal sea_clicked(map: Vector2, button: int)
	var tools: Array[StringName] = []

	func set_tool(tool: StringName) -> void:
		tools.append(tool)


## Stands in for Hud: the selected track and the vessel drawn for track 12 (`Bp.NONE` until set).
class FakeHud extends Node:
	signal actions_changed
	signal offboard_changed
	signal ship_static_changed
	signal selection_changed(track: int)
	var vessel_of_12 := -1
	var lookups := 0

	func selected_track() -> int:
		return 12

	func track(id: int) -> Variant:
		lookups += 1
		return {"id": id, "vessel": vessel_of_12} if id == 12 else null

	func available_actions() -> Array[Dictionary]:
		return []

	func actions() -> Array[Dictionary]:
		return []

	func offboard() -> Array[Dictionary]:
		return []

	func objectives() -> Array[Dictionary]:
		return []


## Stands in for Editor: the Fit Check result of the fielded design.
class FakeEditor extends Node:
	signal fit_changed

	func fit_result() -> Dictionary:
		return Samples.fit(2)


## The screen scripts, loaded once the autoloads they name exist (a preload here would compile them
## before `Ui` is registered).
var Briefing: GDScript
var AfterAction: GDScript
var SortieEnd: GDScript
var ActionsBar: GDScript
var _failures := 0
## Tests that ran to their end; a script error stops a test early and shows here.
var _reached: Array[String] = []
var _frame := 0
var _real: Dictionary = {}
var _orders: FakeOrders
var _campaign: FakeCampaign
var _editor: FakeEditor
var _hud: FakeHud
var _done := false


func _process(_delta: float) -> bool:
	_frame += 1
	if _frame == 1:
		_swap_in_fakes()
		return false
	if _frame == 2:
		_run()
	if _done or _frame > 600:
		_check(_done, "the tests finished within 600 frames")
		_check(_reached.size() == 11, "every test ran to its end: " + str(_reached))
		_swap_out_fakes()
		print("ui_reports_test: %s" % ("ok" if _failures == 0 else "%d failure(s)" % _failures))
		quit(1 if _failures > 0 else 0)
		return true
	return false


func _swap_in_fakes() -> void:
	_orders = FakeOrders.new()
	_campaign = FakeCampaign.new()
	_editor = FakeEditor.new()
	_hud = FakeHud.new()
	for pair: Array in [["Orders", _orders], ["Campaign", _campaign], ["Editor", _editor], ["Hud", _hud]]:
		var real := root.get_node_or_null(NodePath(pair[0]))
		if real != null:
			real.name = String(pair[0]) + "Real"
			_real[pair[0]] = real
		(pair[1] as Node).name = pair[0]
		root.add_child(pair[1])


func _swap_out_fakes() -> void:
	for n: String in ["Orders", "Campaign", "Editor", "Hud"]:
		var fake := root.get_node_or_null(NodePath(n))
		if fake != null:
			root.remove_child(fake)
			fake.free()
		if _real.has(n):
			(_real[n] as Node).name = n


func _check(ok: bool, what: String) -> void:
	if not ok:
		_failures += 1
		printerr("FAIL: ", what)


func _run() -> void:
	Briefing = load("res://ui/screens/briefing.gd")
	AfterAction = load("res://ui/screens/after_action.gd")
	SortieEnd = load("res://ui/screens/sortie_end.gd")
	ActionsBar = load("res://ui/hud/actions_bar.gd")
	for script: GDScript in [Briefing, AfterAction, SortieEnd, ActionsBar]:
		_check(script != null and script.can_instantiate(), "script compiles: %s" % (script.resource_path if script != null else "?"))
	_test_csvs()
	_test_split_orders()
	_test_threats_and_tags()
	_test_fit_lines()
	_test_seakeeping_lines()
	_test_execution_rows()
	_test_briefing_screen()
	_test_after_action()
	_test_sortie_end()
	_test_actions_bar()
	await _test_keyboard_scroll()
	_done = true


# --- copy ----------------------------------------------------------------------------------------

## Every key the scripts and scenes use has an English row, and no row is unused.
func _test_csvs() -> void:
	var keys := {}
	for path: String in CSVS:
		var f := FileAccess.open(path, FileAccess.READ)
		_check(f != null, "csv opens: " + path)
		if f == null:
			continue
		var header := f.get_csv_line()
		_check(header.size() >= 3 and header[0] == "keys" and header[2] == "en", "csv header: " + path)
		while not f.eof_reached():
			var row := f.get_csv_line()
			if row.size() < 3 or row[0].is_empty():
				continue
			_check(not keys.has(row[0]), "duplicate key " + row[0])
			_check(not row[1].is_empty() and not row[2].is_empty(), "context and English for " + row[0])
			keys[row[0]] = row[2]
	var used := {}
	var re := RegEx.create_from_string("\"((?:briefing|after_action|sortie_end|hud_ops)\\.[a-z0-9_.]+)\"")
	for path: String in MY_SCRIPTS + ["res://ui/screens/briefing.tscn", "res://ui/screens/after_action.tscn", "res://ui/screens/sortie_end.tscn", "res://ui/hud/actions_bar.tscn"]:
		var text := FileAccess.get_file_as_string(path)
		for m: RegExMatch in re.search_all(text):
			used[m.get_string(1)] = true
	for k: String in used:
		_check(keys.has(k), "key has an English row: " + k)
	for k: String in keys:
		_check(used.has(k), "row is used: " + k)
	_reached.append("_test_csvs")

# --- briefing ------------------------------------------------------------------------------------

func _test_split_orders() -> void:
	var markers := ["Situation:", "Mission:", "Execution:"]
	var parts: Variant = Briefing.split_orders("Situation: a convoy sails. Mission: sink it. Execution: close unseen.", markers)
	_check(parts[0] == "A convoy sails.", "situation part: " + parts[0])
	_check(parts[1] == "Sink it.", "mission part: " + parts[1])
	_check(parts[2] == "Close unseen.", "execution part: " + parts[2])
	parts = Briefing.split_orders("Mission: first. Situation: wrong order. Execution: x.", markers)
	_check(parts[0].begins_with("Mission: first") and parts[1].is_empty(), "out-of-order markers keep the whole text")
	parts = Briefing.split_orders("No markers here.", markers)
	_check(parts[0] == "No markers here." and parts[2].is_empty(), "missing markers keep the whole text")
	parts = Briefing.split_orders("", markers)
	_check(parts[0].is_empty(), "empty text")
	# Every Act 1 briefing in the shipped CSV splits into three parts.
	var f := FileAccess.open("res://i18n/operations_act1.csv", FileAccess.READ)
	if f != null:
		f.get_csv_line()
		while not f.eof_reached():
			var row := f.get_csv_line()
			if row.size() >= 3 and row[0].ends_with(".briefing"):
				var p: Variant = Briefing.split_orders(row[2], markers)
				_check(not p[1].is_empty() and not p[2].is_empty(), "briefing splits in three: " + row[0])
	_reached.append("_test_split_orders")

func _test_threats_and_tags() -> void:
	var sorted: Variant = Briefing.sort_threats(Kit_dicts(Samples.briefing()["threats"]))
	_check(sorted.size() == 3, "three threats")
	_check(int(sorted[0]["confidence"]) == 2 and int(sorted[2]["confidence"]) == 0, "confirmed first, possible last")
	_check(Briefing.count_text({"count_min": 2, "count_max": 3}) == "2 to 3", "count range: " + Briefing.count_text({"count_min": 2, "count_max": 3}))
	_check(Briefing.count_text({"count_min": 4, "count_max": 1}) == "4", "max below min reads as min")
	var tags: Array = Samples.briefing()["tags"]
	_check(Briefing.tag_verdicts(tags, {}) == [-1, -1, -1], "no fit, no verdicts")
	_check(Briefing.tag_verdicts(tags, {"state": 1}) == [-1, -1, -1], "running fit, no verdicts")
	_check(Briefing.tag_verdicts(tags, Samples.fit(2)) == [1, 1, 0], "verdicts by sentence")
	var reordered := Samples.fit(2)
	var rt: Array = reordered["tags"]
	rt.reverse()
	_check(Briefing.tag_verdicts(tags, reordered) == [1, 1, 0], "verdicts by sentence, any order")
	var unnamed := Samples.fit(4)
	for t: Dictionary in unnamed["tags"]:
		t["text"] = {"key": &"other", "args": []}
	_check(Briefing.tag_verdicts(tags, unnamed) == [1, 1, 0], "verdicts by position when no sentence matches")
	_reached.append("_test_threats_and_tags")

func Kit_dicts(v: Variant) -> Array[Dictionary]:
	var out: Array[Dictionary] = []
	for d: Variant in v:
		out.append(d)
	return out


func _test_fit_lines() -> void:
	var b := Samples.briefing()
	_check(Briefing.fit_lines(b, {})[0] == TranslationServer.translate(&"briefing.fit.unavailable"), "no result reads as unavailable")
	_check(Briefing.fit_lines(b, {"state": 0}).size() == 1, "idle is one line")
	var running: Variant = Briefing.fit_lines(b, {"state": 1})
	_check(running.size() == 1 and running[0].contains("4"), "running names the sea state: " + running[0])
	var done: Variant = Briefing.fit_lines(b, Samples.fit(2))
	_check(done[0].contains("1") and done[0].contains("3"), "fails 1 of 3: " + done[0])
	_check(done.size() == 6, "verdict, speed, turn, range and two detections: %d" % done.size())
	_check(done[4].begins_with("Tally class patrol boat"), "detection names the threat: " + done[4])
	var stale: Variant = Briefing.fit_lines(b, Samples.fit(4))
	_check(stale[0] == TranslationServer.translate(&"briefing.fit.stale") and stale.size() == 7, "stale result keeps its values under a note")
	var est := Samples.fit(2)
	est["simulated"] = false
	est["time_to_turn_180_s"] = -1.0
	var lines: Variant = Briefing.fit_lines(b, est)
	_check(lines[-1] == TranslationServer.translate(&"briefing.fit.estimated"), "gauge-only result says so")
	_reached.append("_test_fit_lines")


## The seakeeping sweep's verdict under the tag verdict: safe, can roll over (sea state and where the sea
## came from), a range of stability too short, and a sweep the time limit cut short.
func _test_seakeeping_lines() -> void:
	var b := Samples.briefing()
	var tx := func(k: StringName) -> String: return String(TranslationServer.translate(k))
	var with := func(sk: Dictionary) -> PackedStringArray:
		var f := Samples.fit(2)
		f["seakeeping"] = sk
		return Briefing.fit_lines(b, f)
	var base := {"sea_state": 4, "complete": true, "rolls_over": false, "safe": true, "worst_heel_deg": 23.0, "worst_from_deg": 120.0, "worst_rolled_over": false, "short_range": false, "range_deg": 64.0, "runs": 6, "directions": 6}
	var safe: PackedStringArray = with.call(base)
	_check(safe.size() == 7 and safe[1].contains("4") and safe[1].contains("23"), "safe names the sea state and worst heel: " + str(safe))
	var over := base.duplicate()
	over.merge({"rolls_over": true, "safe": false, "worst_heel_deg": 41.0, "worst_from_deg": 300.0, "worst_rolled_over": true}, true)
	var rolls: PackedStringArray = with.call(over)
	_check(rolls[1].contains("4") and rolls[1].contains(tx.call(&"editor_fit.sea.from.300")), "rolls over names the sea state and the port bow: " + rolls[1])
	var short := base.duplicate()
	short.merge({"rolls_over": true, "safe": false, "short_range": true, "range_deg": 38.0}, true)
	var s2: PackedStringArray = with.call(short)
	_check(s2[1].contains("38") and s2[1] != rolls[1], "short range names the range of stability: " + s2[1])
	var cut := base.duplicate()
	cut.merge({"complete": false, "safe": false, "runs": 3}, true)
	var c2: PackedStringArray = with.call(cut)
	_check(c2[1] == tx.call(&"editor_fit.sea.unfinished").format(["3", "6", "4"]), "a cut sweep says it did not finish: " + c2[1])
	_check(not c2[1].contains(tx.call(&"editor_fit.sea.safe").substr(0, 10)), "a cut sweep never reads as safe")
	_check(Briefing.fit_lines(b, Samples.fit(2)).size() == 6, "no sweep, no seakeeping line")
	_reached.append("_test_seakeeping_lines")

func _test_execution_rows() -> void:
	var b := Samples.briefing()
	var rows: Variant = Briefing.execution_rows(b)
	var terms: Variant = rows.map(func(r: Array) -> String: return r[0])
	_check(terms.has(TranslationServer.translate(&"briefing.exec.approach")), "approach row")
	_check(terms.has(TranslationServer.translate(&"briefing.exec.emcon")), "emission row when recommended")
	_check(String(rows[-1][1]).contains("Z"), "offer lapses at a date-time group: " + String(rows[-1][1]))
	b["forced"] = true
	b["recommended_emcon"] = -1
	b["actions"] = PackedInt32Array()
	rows = Briefing.execution_rows(b)
	terms = rows.map(func(r: Array) -> String: return r[0])
	_check(not terms.has(TranslationServer.translate(&"briefing.exec.emcon")), "no emission row without a recommendation")
	_check(rows[-1][1] == TranslationServer.translate(&"briefing.exec.forced"), "forced operation cannot be declined")
	_check(rows[1][1] == TranslationServer.translate(&"briefing.exec.weapons_only"), "no station action reads weapons only")
	_reached.append("_test_execution_rows")

func _test_briefing_screen() -> void:
	var screen: Control = (load("res://ui/screens/briefing.tscn") as PackedScene).instantiate()
	root.add_child(screen)
	_campaign.briefed = &"op.act1.prize_capture"
	_campaign.calls.clear()
	# The plot is another unit's; a plain Control without show_static stands in so setup skips it.
	var stand_in := Control.new()
	screen.add_child(stand_in)
	screen.set(&"_chart", stand_in)
	screen.call(&"setup", {})
	var launch: Button = screen.get_node("Margin/Column/Footer/Launch")
	var decline: Button = screen.get_node("Margin/Column/Footer/Decline")
	var title: Label = screen.get_node("Margin/Column/Header/Title")
	_check(title.text == "Prize capture", "briefing title: " + title.text)
	_check(launch.visible and not launch.disabled and decline.visible, "launch and decline offered")
	_check(screen.call(&"default_focus") == launch, "launch takes the first focus")
	launch.pressed.emit()
	_check(_campaign.calls.has(["launch_sortie"]), "launch calls the campaign")
	var status: Label = screen.get_node("Margin/Column/Status")
	_check(status.visible and not status.text.is_empty(), "a refused launch shows its reason")
	screen.call(&"_on_decline_chosen", &"decline")
	var proposed := false
	for c: Array in _campaign.calls:
		if c[0] == "propose" and int(c[1]["kind"]) == 4 and c[1]["operation"] == &"op.act1.prize_capture":
			proposed = true
	_check(proposed, "decline proposes the decline action")
	_check(_campaign.calls.has(["close_briefing"]), "a committed decline goes back to the chart")
	# Forced operations cannot be declined; the empty state offers only the way back.
	var forced := Samples.briefing()
	forced["forced"] = true
	screen.call(&"show_briefing", forced, {})
	_check(not decline.visible, "no decline on a forced operation")
	screen.call(&"show_briefing", {}, {})
	_check(not launch.visible and screen.call(&"default_focus") == screen.get_node("Margin/Column/Footer/Back"), "empty briefing offers the way back")
	# Co-op: a pending accept lists who has answered and offers the host's confirm.
	screen.call(&"show_briefing", Samples.briefing(), Samples.fit(2))
	screen.call(&"show_coop", Samples.players(), Samples.accept_proposal(), true)
	var coop: VBoxContainer = screen.get_node("Margin/Column/Scroll/Body/Side/Coop")
	var texts := PackedStringArray()
	for c: Node in coop.get_children():
		if c is Label:
			texts.append((c as Label).text)
		elif c is Button:
			texts.append("[%s]" % (c as Button).text)
	_check(coop.visible and texts.size() == 3, "co-op rows: " + str(texts))
	_check(texts[1].begins_with("Bram"), "waiting player listed: " + str(texts))
	screen.queue_free()
	_reached.append("_test_briefing_screen")

# --- after action --------------------------------------------------------------------------------

func _test_after_action() -> void:
	var lines: Variant = AfterAction.alert_lines(Kit_dicts([
		{"region": &"gannet", "alert": 1.5, "cause": 2},
		{"region": &"corran", "alert": 0.5, "cause": 3},
		{"region": &"gannet", "alert": 2.99, "cause": 0},
		{"region": &"gannet", "alert": 3.2, "cause": 2},
		{"region": &"corran", "alert": 9.0, "cause": 99},
	]), Samples.regions())
	_check(lines.size() == 2, "one line per region")
	_check(lines[0] == "Gannet Banks: Alert 3 after sighting, observed kill.", "alert floors and lists causes once: " + lines[0])
	_check(lines[1] == "Corran Sound: Alert 5 after evidence found.", "alert clamps at 5 and drops unknown causes: " + lines[1])
	var kills: Array[Dictionary] = [{"method": 6, "count": 1}, {"method": 0, "count": 3}, {"method": 1, "count": 0}]
	_check(AfterAction.kills_text(kills) == "Gunfire 3, Capture 1", "kills most first, zero dropped: " + AfterAction.kills_text(kills))
	_check(AfterAction.kills_text(Kit_dicts([])) == TranslationServer.translate(&"after_action.result.no_kills"), "no kills")
	var rows: Variant = AfterAction.payout_rows({"salvage_wrecks": 100, "salvage_prizes": 0, "intel": 0, "blueprints": [], "fuel_l": 0.0, "ammo_kg": 0.0}, [["Ada", 160], ["Bram", 0]])
	_check(rows.size() == 4, "wrecks, prizes, repairs, net: %d" % rows.size())
	_check(String(rows[-1][1]).contains("60") and String(rows[-1][1]).ends_with(TranslationServer.translate(&"after_action.payout.net_loss").get_slice("}", 1)), "net shortfall after repairs: " + String(rows[-1][1]))
	_check(AfterAction.operation_title(&"op.act1.prize_capture") == "Prize capture", "operation title from its name key")
	# How the sortie ended: the report's reason; without one, the primary objective's last text; else no row.
	var ended := TranslationServer.translate(&"after_action.result.ended")
	var objective_term := TranslationServer.translate(&"after_action.result.objective")
	var last: Array[Dictionary] = [{"id": 1, "ghost": true, "text": {"key": &"objective.ghost.no_kills", "args": []}}, {"id": 0, "ghost": false, "text": {"key": &"objective.end.hulls_disabled", "args": []}}]
	var why_rows: Array = AfterAction.end_rows({"end_reason": {"key": &"campaign.end.withdrew", "args": []}}, last)
	_check(why_rows.size() == 1 and why_rows[0][0] == ended and why_rows[0][1] == TranslationServer.translate(&"campaign.end.withdrew"), "the report's reason is the row: %s" % [why_rows])
	why_rows = AfterAction.end_rows({}, last)
	_check(why_rows.size() == 1 and why_rows[0][0] == objective_term and String(why_rows[0][1]).begins_with("Every ship was sunk or dead in the water"), "without a reason the primary objective's last text stands in: %s" % [why_rows])
	_check(AfterAction.end_rows({}, Kit_dicts([])).is_empty() and AfterAction.end_rows({"end_reason": {}}, Kit_dicts([])).is_empty(), "neither: no row")
	for key: StringName in [&"campaign.end.objective_done", &"campaign.end.objective_failed", &"campaign.end.hulls_lost", &"campaign.end.hulls_disabled", &"campaign.end.time_limit", &"campaign.end.withdrew"]:
		_check(TranslationServer.translate(key) != key, "%s has English" % key)
	_check(AfterAction.operation_title(&"op.unknown") == TranslationServer.translate(&"after_action.title.plain"), "unknown operation gets the plain title")
	_check(AfterAction.operation_title(&"") == TranslationServer.translate(&"after_action.title.plain"), "no operation gets the plain title")
	var screen: Control = (load("res://ui/screens/after_action.tscn") as PackedScene).instantiate()
	root.add_child(screen)
	_campaign.calls.clear()
	var stand_in := Control.new()
	screen.add_child(stand_in)
	screen.set(&"_chart", stand_in)
	screen.call(&"setup", {})
	var title: Label = screen.get_node("Margin/Column/Header/Title")
	var outcome: Label = screen.get_node("Margin/Column/Header/Outcome")
	_check(title.text == "Prize capture" and outcome.text == "Victory" and outcome.visible, "report title and outcome: %s / %s" % [title.text, outcome.text])
	var doctrine: VBoxContainer = screen.get_node("Margin/Column/Scroll/Body/Side/Doctrine")
	var gauges := doctrine.find_children("*", "Control", false, false).filter(func(n: Node) -> bool: return n.get_script() == load("res://ui/widgets/gauge.gd"))
	_check(gauges.size() == 2, "one gauge per tally: %d" % gauges.size())
	# The session's playtest report (DESIGN.md L2209) can be copied from here too, beside the one action.
	var copy := screen.get_node_or_null("Margin/Column/Footer/CopyReport") as Button
	var copy_status := screen.get_node_or_null("Margin/Column/Footer/CopyStatus") as Label
	_check(copy != null and TranslationServer.translate(copy.text) == TranslationServer.translate(&"settings_ui.playtest.copy_report") and copy_status != null and not copy_status.visible, "the footer offers Copy playtest report")
	_check(copy != null and copy.get_index() < (screen.get_node("Margin/Column/Footer/Close") as Node).get_index(), "it sits before Return to the chart table")
	if copy != null:
		copy.pressed.emit()
		var said := [TranslationServer.translate(&"settings_ui.playtest.report_copied"), TranslationServer.translate(&"settings_ui.playtest.nothing_to_copy")]
		_check(copy_status.visible and said.has(copy_status.text), "pressing it says what happened: " + copy_status.text)
		_check(not _campaign.calls.has(["close_after_action"]), "copying does not leave the report")
	(screen.get_node("Margin/Column/Footer/Close") as Button).pressed.emit()
	_check(_campaign.calls.has(["close_after_action"]), "close returns to the chart table")
	screen.call(&"show_report", {}, Kit_dicts([]), Kit_dicts([]))
	_check(title.text == TranslationServer.translate(&"after_action.title.none") and not outcome.visible, "empty report")
	screen.queue_free()
	_reached.append("_test_after_action")

# --- sortie summary ------------------------------------------------------------------------------

func _test_sortie_end() -> void:
	var s := Samples.stats(true)
	s["torpedoes_fired"] = 0
	var rows: Variant = SortieEnd.stat_rows(s)
	var shown: Variant = rows.filter(func(r: Array) -> bool: return not String(r[1]).is_empty())
	var terms: Variant = shown.map(func(r: Array) -> String: return r[0])
	_check(not terms.has(TranslationServer.translate(&"sortie_end.stat.torpedoes")), "unused torpedoes left out")
	_check(shown[0][1] == "63 of 412 (15%)", "gun hits: " + String(shown[0][1]))
	s["first_detection_s"] = -1.0
	rows = SortieEnd.stat_rows(s)
	var never := false
	for r: Array in rows:
		if r[0] == TranslationServer.translate(&"sortie_end.stat.first_detection"):
			never = r[1] == TranslationServer.translate(&"sortie_end.stat.never")
	_check(never, "no detection reads never")
	_check(SortieEnd.lead_text({}) == TranslationServer.translate(&"sortie_end.lead.no_enemy"), "no enemy tonnage")
	var end_rows: Array = SortieEnd.end_rows({"end_reason": {"key": &"campaign.end.time_limit", "args": []}})
	_check(end_rows.size() == 1 and end_rows[0][0] == TranslationServer.translate(&"sortie_end.ended") and end_rows[0][1] == TranslationServer.translate(&"campaign.end.time_limit"), "the summary says how the sortie ended: %s" % [end_rows])
	_check(SortieEnd.end_rows({}).is_empty(), "no reason, no row")
	_check(SortieEnd.lead_text({"enemy_tonnage_total_t": 100.0, "enemy_tonnage_disabled_t": 250.0}).contains("100%"), "disabled clamps to the total")
	var screen: Control = (load("res://ui/screens/sortie_end.tscn") as PackedScene).instantiate()
	root.add_child(screen)
	var primary: Button = screen.get_node("Scroll/Center/Panel/Margin/Column/Actions/Primary")
	var secondary: Button = screen.get_node("Scroll/Center/Panel/Margin/Column/Actions/Secondary")
	screen.call(&"show_summary", Samples.summary(true), 4, Samples.objectives())
	_check(primary.text == "sortie_end.to_lobby" and secondary.visible, "PvP returns to the lobby, can leave")
	screen.call(&"show_summary", Samples.summary(false), 2, Kit_dicts([]))
	_check(primary.text == "sortie_end.to_menu" and not secondary.visible, "skirmish returns to the menu")
	var objectives: VBoxContainer = screen.get_node("Scroll/Center/Panel/Margin/Column/Objectives")
	var first_text := ""
	for l: Node in objectives.get_child(0).find_children("*", "Label", true, false):
		first_text = (l as Label).text
	_check(first_text == "Objective 1", "objective without text is numbered: " + first_text)
	_check(screen.call(&"default_focus") == primary, "primary takes the first focus")
	screen.queue_free()
	_reached.append("_test_sortie_end")

# --- special operations --------------------------------------------------------------------------

func _test_actions_bar() -> void:
	_check(ActionsBar.unit_state({"body": 5}) == 2 and ActionsBar.unit_state({"body": -1, "recharging_s": 3.0}) == 3, "unit airborne and recharging")
	_check(ActionsBar.unit_state({"body": -1, "aboard": 2}) == 1 and ActionsBar.unit_state({"body": -1}) == 0, "unit ready and empty")
	var latest: Variant = ActionsBar.latest_by_action(Kit_dicts([{"id": 2, "action": 0, "state": 2}, {"id": 5, "action": 0, "state": 0}, {"id": 3, "action": 4, "state": 3}]))
	_check(int(latest[0]["id"]) == 5 and int(latest[4]["id"]) == 3, "latest action per type")
	_check(ActionsBar.action_status({"state": 1, "block": {"key": &"sim.action.too_fast", "args": []}}) == "Paused: Too fast for the action.", "paused says why")
	_check(ActionsBar.last_attempt({"state": 2, "action": 5, "block": {"key": &"sim.action.target_lost", "args": []}}).contains("Target lost"), "aborted says why")
	_check(ActionsBar.start_key(4, &"line") == "hud_ops.start.mines" and ActionsBar.start_key(9, &"line") == "hud_ops.start.line", "start wording")
	var bar: Control = (load("res://ui/hud/actions_bar.tscn") as PackedScene).instantiate()
	root.add_child(bar)
	var plot := FakePlot.new()
	bar.add_child(plot)
	bar.call(&"set_plot", plot)
	var none := Kit_dicts([])
	bar.call(&"show_ops", none, none, none, -1)
	_check(not bar.visible, "nothing to show hides the bar")
	_orders.calls.clear()
	bar.call(&"show_ops", Samples.available(), none, Samples.offboard(), -1)
	_check(bar.visible, "actions show the bar")
	var board := _button(bar, "action:0:start")
	_check(board != null and board.disabled, "board needs a selected track")
	bar.call(&"show_ops", Samples.available(), none, Samples.offboard(), 12)
	board = _button(bar, "action:0:start")
	_check(board != null and not board.disabled, "board enabled with a track")
	board.pressed.emit()
	_check(_orders.calls.size() == 1 and _orders.calls[0][0] == "start_action" and _orders.calls[0][2]["track"] == 12, "board targets the selected track: " + str(_orders.calls))
	# The boarding starts on approach: track 12 has no drawn vessel yet, so no prize vessel is known.
	_hud.vessel_of_12 = -1
	bar.call(&"show_ops", Samples.available(), Samples.actions(), Samples.offboard(), 12)
	_check(not (bar.get(&"_prize_vessel") as Dictionary).has(3), "no prize vessel before the target is drawn")
	_hud.vessel_of_12 = 9001
	bar.call(&"show_ops", Samples.available(), Samples.actions(), Samples.offboard(), 12)
	_check(int((bar.get(&"_prize_vessel") as Dictionary).get(3, -1)) == 9001, "prize vessel resolved once the target is drawn")
	var lookups := _hud.lookups
	bar.call(&"show_ops", Samples.available(), Samples.actions(), Samples.offboard(), 12)
	_check(_hud.lookups == lookups, "a resolved prize vessel is not looked up again")
	var recover := _button(bar, "action:3:start")
	_check(recover != null and recover.disabled, "recover disabled when the reason says so")
	# Under way: progress, why paused, cancel.
	bar.call(&"show_ops", Samples.available(), Samples.actions(), Samples.offboard(), 12)
	var cancel := _button(bar, "action:0:cancel")
	_check(cancel != null and _button(bar, "action:0:start") == null, "cancel replaces start while under way")
	cancel.pressed.emit()
	_check(_orders.calls[-1] == ["cancel_action", 3], "cancel sends the action id")
	# Prize: the boarding ordered on track 12 took vessel 9001.
	bar.call(&"show_ops", Samples.available(), Samples.actions_done(), none, 12)
	var strip := _button(bar, "prize:3:strip")
	_check(strip != null and _button(bar, "prize:3:home") != null, "prize offers home and strip")
	strip.pressed.emit()
	_check(_button(bar, "prize:3:strip_confirm") != null and _orders.calls[-1][0] != "prize", "strip asks again first")
	_button(bar, "prize:3:strip_confirm").pressed.emit()
	_check(_orders.calls[-1] == ["prize", 9001, 1], "strip decides the prize: " + str(_orders.calls[-1]))
	_check(_button(bar, "prize:3:strip") == null, "a stripped prize offers nothing more")
	# Offboard orders.
	bar.call(&"show_ops", none, none, Samples.offboard(), -1)
	var shadow := _button(bar, "unit:7:shadow")
	_check(shadow != null and shadow.disabled, "shadow needs a track")
	_button(bar, "unit:7:recall").pressed.emit()
	_check(_orders.calls[-1][0] == "offboard" and int(_orders.calls[-1][2]["kind"]) == 1, "recall order")
	_orders.accept = false
	_button(bar, "unit:7:recall").pressed.emit()
	var shown_refusal := false
	for l: Node in bar.find_children("*", "Label", true, false):
		if (l as Label).text == TranslationServer.translate(&"hud_ops.refused"):
			shown_refusal = true
	_check(shown_refusal, "a refused order says so on its row")
	_orders.accept = true
	bar.call(&"begin_pick", 7, 2)
	plot.sea_clicked.emit(Vector2(300, -400), MOUSE_BUTTON_RIGHT)
	_check(_orders.calls[-1][0] != "offboard" or int(_orders.calls[-1][2]["kind"]) != 2, "a right click does not pick")
	plot.sea_clicked.emit(Vector2(300, -400), MOUSE_BUTTON_LEFT)
	var last: Array = _orders.calls[-1]
	_check(last[0] == "offboard" and int(last[2]["kind"]) == 2 and last[2]["centre"] == Vector2(300, -400) and float(last[2]["radius_m"]) == 2000.0, "orbit at the picked point: " + str(last))
	plot.tools.clear()
	bar.call(&"begin_line")
	_check(plot.tools == [&"line"], "the plot gets the line tool: " + str(plot.tools))
	plot.line_drawn.emit(Vector2(0, 0), Vector2(0, 500))
	_check(plot.tools == [&"line", &"orders"], "the plot goes back to orders: " + str(plot.tools))
	last = _orders.calls[-1]
	_check(last[0] == "start_action" and int(last[1]) == 4 and last[2]["kind"] == &"line" and last[2]["b"] == Vector2(0, 500), "mine line starts lay mines: " + str(last))
	bar.call(&"begin_line")
	var before := _orders.calls.size()
	plot.line_drawn.emit(Vector2(1, 1), Vector2(1, 1))
	_check(_orders.calls.size() == before, "a zero-length line sends nothing")
	bar.call(&"begin_line")
	bar.call(&"cancel_pick")
	_check(plot.tools[-1] == &"orders" and not plot.line_drawn.is_connected(Callable(bar, &"_on_line_drawn")), "cancel releases the plot")
	# Without set_plot the bar finds a live plot even while it is hidden (bridge view), and releases
	# that same plot.
	bar.call(&"set_plot", null)
	bar.call(&"begin_pick", 7, 4)
	_check(bar.get(&"_pick_plot") == null, "no live BpPlotView in the tree (the screens above are being freed): no plot taken")
	_check((bar.get_node("Panel/Margin/Scroll/Column/Pick") as Control).visible, "the pick hint shows anyway")
	bar.call(&"cancel_pick")
	bar.queue_free()
	_reached.append("_test_actions_bar")

## Up and Down reach the bottom of the briefing with no mouse, then Down moves on to Launch.
func _test_keyboard_scroll() -> void:
	# A 1600 x 800 frame: wide enough for two columns, short enough that the order scrolls.
	var frame := Control.new()
	frame.size = Vector2(1600, 800)
	root.add_child(frame)
	var screen: Control = (load("res://ui/screens/briefing.tscn") as PackedScene).instantiate()
	frame.add_child(screen)
	screen.call(&"show_briefing", Samples.briefing(), Samples.fit(2))
	await process_frame
	await process_frame
	var scroll: ScrollContainer = screen.get_node("Margin/Column/Scroll")
	var launch: Button = screen.get_node("Margin/Column/Footer/Launch")
	var bar := scroll.get_v_scroll_bar()
	_check(bar.max_value - bar.page > 1.0, "the briefing is taller than its scroll: %.0f over %.0f" % [bar.max_value, bar.page])
	scroll.grab_focus()
	var steps := 0
	while steps < 200 and root.gui_get_focus_owner() == scroll:
		var ev := InputEventAction.new()
		ev.action = &"ui_down"
		ev.pressed = true
		root.push_input(ev)
		var up := InputEventAction.new()
		up.action = &"ui_down"
		up.pressed = false
		root.push_input(up)
		steps += 1
	_check(steps > 1 and steps < 200, "Down scrolled in %d steps" % steps)
	_check(scroll.scroll_vertical >= int(bar.max_value - bar.page) - 1, "the scroll reached the end: %d" % scroll.scroll_vertical)
	_check(root.gui_get_focus_owner() == launch, "past the end, focus moves to Launch: %s" % str(root.gui_get_focus_owner()))
	var back := InputEventAction.new()
	back.action = &"ui_up"
	back.pressed = true
	root.push_input(back)
	_check(root.gui_get_focus_owner() == scroll, "Up from Launch returns to the order")
	frame.queue_free()
	_reached.append("_test_keyboard_scroll")


func _button(bar: Node, key: String) -> Button:
	for b: Node in bar.find_children("*", "Button", true, false):
		if String(b.get_meta(&"ops_key", "")) == key and b.is_inside_tree() and not b.is_queued_for_deletion():
			return b as Button
	return null
