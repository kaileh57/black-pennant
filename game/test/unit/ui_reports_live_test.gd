extends SceneTree
## Headless end-to-end run of the briefing, after-action report and sortie summary against the real
## Campaign, Session, Hud and Tx facades. Units ui-briefing, ui-after-action. Run from game/ with a
## throwaway user folder, because it writes campaign saves:
##   HOME=/tmp/bp-reports-home godot --headless --path . -s res://test/unit/ui_reports_live_test.gd
## Campaign: a new campaign, an offer briefed through the screen, Back to the chart table, the same
## offer reopened and declined, the next one launched from the Launch button, the sortie stopped as
## a withdrawal, the real after-action report drawn and closed. Skirmish: a sortie started, stopped,
## and its summary drawn from `Session.last_summary`. The plot is left out of the screens (unit gd-plot).
## Exits 1 on any failure.

const STARTER := "starters/fac22.bp.ron"
const LIMIT := 1500

var _campaign: Node
var _session: Node
var _check: BpSessionCheck
var _frame := 0
var _step := 0
var _step_frame := 0
var _failures := 0
var _rejected: Array[String] = []
var _screen: Control


func _initialize() -> void:
	Engine.max_fps = 60
	_check = BpSessionCheck.new()


func _fail(what: String) -> void:
	_failures += 1
	printerr("FAIL: ", what)


func _next(step: int) -> void:
	_step = step
	_step_frame = _frame


func _finish() -> bool:
	if _screen != null and is_instance_valid(_screen):
		_screen.queue_free()
	if _campaign != null and bool(_campaign.call(&"is_active")):
		_campaign.call(&"close_campaign")
	print("ui_reports_live_test: %s" % ("ok" if _failures == 0 else "%d failure(s)" % _failures))
	quit(1 if _failures > 0 else 0)
	return true


## A screen with the plot swapped for a plain Control (the plot is gd-plot's and not finished).
func _open(path: String) -> Control:
	if _screen != null and is_instance_valid(_screen):
		_screen.queue_free()
	_screen = (load(path) as PackedScene).instantiate()
	root.add_child(_screen)
	if _screen.get(&"_chart") != null:
		var stand_in := Control.new()
		_screen.add_child(stand_in)
		_screen.set(&"_chart", stand_in)
	_screen.call(&"setup", {})
	return _screen


func _labels(node: Node) -> PackedStringArray:
	var out := PackedStringArray()
	for l: Node in node.find_children("*", "Label", true, false):
		var label := l as Label
		if label.is_visible_in_tree() and not label.text.is_empty():
			# Scene labels hold a key and translate as they draw.
			out.append(label.text if label.auto_translate_mode == Node.AUTO_TRANSLATE_MODE_DISABLED else String(label.tr(label.text)))
	return out


func _process(_delta: float) -> bool:
	_frame += 1
	if _frame == 1:
		return _start()
	_check.observe()
	if _frame - _step_frame > LIMIT:
		_fail("step %d timed out (phase %d, session %d, rejected %s)" % [_step, _campaign.call(&"phase"), _session.call(&"state"), str(_rejected)])
		return _finish()
	match _step:
		1:
			if int(_session.call(&"state")) == Bp.SESSION_IN_SORTIE and _frame - _step_frame > 30:
				if not _check.stop_sortie(Bp.OUTCOME_WITHDRAWN):
					_fail("stop_sortie was not sent")
				_next(2)
			elif int(_session.call(&"state")) == Bp.SESSION_FAILED:
				_fail("the campaign sortie failed to start")
				return _finish()
		2:
			if int(_campaign.call(&"phase")) == Bp.PHASE_AFTER_ACTION:
				_report()
				_skirmish()
				_next(3)
		3:
			if int(_session.call(&"state")) == Bp.SESSION_IN_SORTIE and _frame - _step_frame > 30:
				if not _check.stop_sortie(Bp.OUTCOME_WITHDRAWN):
					_fail("stop_sortie was not sent (skirmish)")
				_next(4)
			elif int(_session.call(&"state")) == Bp.SESSION_FAILED:
				_fail("the skirmish failed to start")
				return _finish()
		4:
			if int(_session.call(&"state")) == Bp.SESSION_ENDED:
				_summary()
				return _finish()
	return false


func _start() -> bool:
	_campaign = root.get_node("Campaign")
	_session = root.get_node("Session")
	_campaign.connect(&"rejected", func(t: Dictionary) -> void: _rejected.append(str(t)))
	var problems: Array = _campaign.call(&"new_campaign", {"slot_name": "Reports test", "seed": 777, "ironman": false, "starter": STARTER, "tutorial_done": true})
	if not problems.is_empty():
		_fail("new_campaign refused: %s" % str(problems))
		return _finish()
	var offers: Array = (_campaign.call(&"chart") as Dictionary).get("offers", [])
	if offers.size() < 2:
		_fail("fewer than two offers: %d" % offers.size())
		return _finish()
	# The first template offer: only templates can be declined.
	var op := &""
	for o: Dictionary in offers:
		if int(o.get("kind", -1)) == Bp.OPERATION_TEMPLATE and op == &"":
			op = StringName(o.get("id", &""))
	if op == &"":
		_fail("no template operation on offer: %s" % str(offers))
		return _finish()
	# Open the briefing as the chart's operations panel does; the router would then open the screen.
	if not bool(_campaign.call(&"open_briefing", op)):
		_fail("open_briefing refused: %s" % str(_rejected))
		return _finish()
	if _campaign.call(&"briefed_operation") != op:
		_fail("briefed_operation is %s, not %s" % [_campaign.call(&"briefed_operation"), op])
	var screen := _open("res://ui/screens/briefing.tscn")
	var title := (screen.get_node("Margin/Column/Header/Title") as Label).text
	var orders := _labels(screen.get_node("Margin/Column/Scroll/Body/Orders"))
	print("ui_reports_live_test: Fit Check block: %s" % str(_labels(screen.get_node("Margin/Column/Scroll/Body/Side/Fit"))))
	print("ui_reports_live_test: briefing '%s', dtg '%s', %d lines" % [title, (screen.get_node("Margin/Column/Header/Dtg") as Label).text, orders.size()])
	for line: String in orders.slice(0, 12):
		print("  | ", line)
	if title == TranslationServer.translate(&"briefing.title.none") or orders.size() < 10:
		_fail("the real briefing did not draw")
	for line: String in orders:
		if line.contains("{") or line.contains("briefing.") or line.contains("campaign."):
			_fail("unfilled text: " + line)
	# Back to the chart keeps the operation accepted; reopening returns to it.
	(screen.get_node("Margin/Column/Footer/Back") as Button).pressed.emit()
	if int(_campaign.call(&"phase")) != Bp.PHASE_CHART or _campaign.call(&"briefed_operation") != &"":
		_fail("Back left phase %d, briefed %s" % [_campaign.call(&"phase"), _campaign.call(&"briefed_operation")])
	if not bool(_campaign.call(&"open_briefing", op)):
		_fail("reopening the briefing refused: %s" % str(_rejected))
		return _finish()
	screen.call(&"refresh")
	if not (screen.get_node("Margin/Column/Footer/Decline") as Button).visible:
		_fail("a template operation offers no Decline")
	# Decline it from the screen (the dialog's choice).
	screen.call(&"_on_decline_chosen", &"decline")
	var still := false
	for o: Dictionary in (_campaign.call(&"chart") as Dictionary).get("offers", []):
		still = still or StringName(o.get("id", &"")) == op
	print("ui_reports_live_test: after decline phase %d, offer still listed %s, rejected %s" % [_campaign.call(&"phase"), str(still), str(_rejected)])
	if int(_campaign.call(&"phase")) != Bp.PHASE_CHART or still:
		_fail("decline left phase %d with the offer listed %s" % [_campaign.call(&"phase"), str(still)])
	# Launch the next offer from the screen's button.
	offers = (_campaign.call(&"chart") as Dictionary).get("offers", [])
	if offers.is_empty():
		_fail("no offer left to launch")
		return _finish()
	op = StringName((offers[0] as Dictionary).get("id", &""))
	if not bool(_campaign.call(&"open_briefing", op)):
		_fail("open_briefing (second) refused: %s" % str(_rejected))
		return _finish()
	screen.call(&"refresh")
	var kind := int((_campaign.call(&"briefing", op) as Dictionary).get("kind", -1))
	if (screen.get_node("Margin/Column/Footer/Decline") as Button).visible != (kind == Bp.OPERATION_TEMPLATE):
		_fail("Decline shown for operation kind %d" % kind)
	(screen.get_node("Margin/Column/Footer/Launch") as Button).pressed.emit()
	if int(_campaign.call(&"phase")) != Bp.PHASE_SORTIE:
		_fail("Launch left phase %d: %s; status '%s'" % [_campaign.call(&"phase"), str(_rejected), (screen.get_node("Margin/Column/Status") as Label).text])
		return _finish()
	_next(1)
	return false


func _report() -> void:
	var screen := _open("res://ui/screens/after_action.tscn")
	var title := (screen.get_node("Margin/Column/Header/Title") as Label).text
	var outcome := (screen.get_node("Margin/Column/Header/Outcome") as Label).text
	var lines := _labels(screen.get_node("Margin/Column/Scroll/Body"))
	print("ui_reports_live_test: report '%s' / '%s', %d lines" % [title, outcome, lines.size()])
	for line: String in lines:
		print("  | ", line)
		if line.contains("{") or line.contains("after_action.") or line.contains("campaign."):
			_fail("unfilled text: " + line)
	if title == TranslationServer.translate(&"after_action.title.none") or outcome.is_empty():
		_fail("the real report did not draw")
	(screen.get_node("Margin/Column/Footer/Close") as Button).pressed.emit()
	if int(_campaign.call(&"phase")) != Bp.PHASE_CHART:
		_fail("closing the report left phase %d" % _campaign.call(&"phase"))
	_campaign.call(&"close_campaign")


func _skirmish() -> void:
	var problems: Array = _session.call(&"start_skirmish", {
		"template": Bp.MISSION_CONVOY_RAID, "act": Bp.ACT_1, "enemy_per_player": 1, "difficulty": {"preset": "normal"},
		"weather": Bp.WEATHER_CLEAR, "sea_state": 3, "time_of_day": Bp.TIME_DUSK, "seed": 99, "blueprint": STARTER,
	})
	if not problems.is_empty():
		_fail("start_skirmish refused: %s" % str(problems))


func _summary() -> void:
	var screen := _open("res://ui/screens/sortie_end.tscn")
	var base := "Scroll/Center/Panel/Margin/Column/"
	var title := (screen.get_node(base + "Header/Title") as Label).text
	var meta := (screen.get_node(base + "Header/Meta") as Label).text
	var lines := _labels(screen.get_node(base))
	print("ui_reports_live_test: summary '%s' / '%s', %d lines" % [title, meta, lines.size()])
	for line: String in lines:
		print("  | ", line)
		if line.contains("{") or line.contains("sortie_end."):
			_fail("unfilled text: " + line)
	if title != String(Kit_word(Bp.OUTCOME_WITHDRAWN)) or not meta.begins_with(String(Kit_mode())):
		_fail("summary title '%s' or meta '%s' wrong" % [title, meta])
	(screen.get_node(base + "Actions/Primary") as Button).pressed.emit()
	if int(_session.call(&"state")) != Bp.SESSION_IDLE:
		_fail("Return to the menu left session state %d" % _session.call(&"state"))


func Kit_word(outcome: int) -> String:
	return String(root.get_node("Tx").call(&"word", &"outcome", outcome))


func Kit_mode() -> String:
	return String(root.get_node("Tx").call(&"word", &"mode", 1))
