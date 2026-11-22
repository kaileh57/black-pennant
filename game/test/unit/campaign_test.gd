extends SceneTree
## Headless proof of the campaign loop through the Campaign autoload. Units gd-campaign, gd-campaign-saves.
## Run from game/ with a throwaway user folder, because it writes saves and a suspend slot:
##   HOME=/tmp/bp-campaign-home godot --headless --path . -s res://test/unit/campaign_test.gd
## New campaign, chart, a 6 h wait animated to its interrupt, a briefing, a sortie launched and quit (the
## suspend slot is written and the campaign closes), the suspend resumed (the slot is consumed), the sortie
## ended by withdrawal, the after-action report, cove quotes, a manual save, a reload, and the cleanup of
## every file it made. Exits 1 on any failure. It refuses to run when a suspend slot already exists, so it
## never replaces a player's suspended sortie.

const STARTER := "starters/fac22.bp.ron"
const LIMIT := 1200

var _campaign: BpCampaign
var _session: BpSession
var _check: BpSessionCheck
var _frame := 0
var _step := 0
var _step_frame := 0
var _failures := 0
var _signals: Dictionary[String, int] = {}
var _interrupt: Dictionary = {}
var _rejected: Array[String] = []
var _saved_paths: Array[String] = []
var _op := &""
var _hours_seen := 0
var _transit_to := &""


func _initialize() -> void:
	Engine.max_fps = 60
	_check = BpSessionCheck.new()


func _process(_delta: float) -> bool:
	_frame += 1
	if _frame == 1:
		return _start()
	_check.observe()
	var since := _frame - _step_frame
	if since > LIMIT:
		_fail("step %d timed out (campaign phase %d, session state %d, rejected %s)" % [_step, _campaign.phase(), _session.state(), str(_rejected)])
		return _finish()
	match _step:
		1:
			# The wait runs to its interrupt.
			if not _interrupt.is_empty():
				print("campaign_test: interrupted after %d hours: %s" % [_hours_seen, str(_interrupt)])
				if _campaign.phase() != Bp.PHASE_CHART:
					_fail("phase after the interrupt is %d" % _campaign.phase())
				if int(_campaign.chart().get("hour", 0)) < 1:
					_fail("no theatre hour passed")
				_start_transit()
		7:
			# The passage: SEXTANT is not at a cove and nothing can be saved until it arrives.
			if _campaign.is_advancing():
				var c := _campaign.chart()
				if bool(c.get("at_cove", true)) or int(c.get("can_save", -1)) == Bp.SAVE_ALLOWED:
					_fail("on passage the chart says at_cove %s, can_save %d" % [str(c.get("at_cove")), c.get("can_save", -1)])
			elif not _interrupt.is_empty():
				if int(_interrupt.get("kind", -1)) == Bp.INTERRUPT_ARRIVED:
					var c := _campaign.chart()
					print("campaign_test: arrived at %s after %d hours" % [c.get("sextant", ""), _hours_seen])
					if c.get("sextant", &"") != _transit_to or not bool(c.get("at_cove", false)) or int(c.get("can_save", -1)) != Bp.SAVE_ALLOWED:
						_fail("after arriving: sextant %s, at_cove %s, can_save %d" % [c.get("sextant", ""), str(c.get("at_cove")), c.get("can_save", -1)])
					_brief_and_launch()
				else:
					# An earlier interrupt (a new offer, a card): carry on with the passage.
					_interrupt = {}
					if not _campaign.advance():
						_fail("advance refused on passage: %s" % str(_rejected))
						return _finish()
		2:
			if _session.state() == Bp.SESSION_IN_SORTIE:
				print("campaign_test: sortie running; quitting it as the pause menu does")
				_session.leave(false)
				_campaign.close_campaign()
				_next(3)
			elif _session.state() == Bp.SESSION_FAILED:
				_fail("the campaign sortie failed to start")
				return _finish()
		3:
			if _campaign.phase() == Bp.PHASE_NONE:
				var s: Variant = _campaign.suspend_slot()
				if s == null:
					_fail("no suspend slot after quitting mid-sortie")
					return _finish()
				print("campaign_test: suspended: ", s)
				if not bool(s.get("is_suspend", false)) or not bool(s.get("loadable", false)):
					_fail("the suspend slot is not a loadable suspend: %s" % str(s))
				if not _campaign.resume_suspend():
					_fail("resume_suspend refused: %s" % str(_rejected))
					return _finish()
				if _campaign.suspend_slot() != null:
					_fail("resuming did not consume the suspend slot")
				if _campaign.phase() != Bp.PHASE_SORTIE:
					_fail("phase after resuming is %d" % _campaign.phase())
				_next(4)
		4:
			if _session.state() == Bp.SESSION_IN_SORTIE and since > 30:
				print("campaign_test: resumed sortie running; withdrawing")
				if not _check.stop_sortie(Bp.OUTCOME_WITHDRAWN):
					_fail("stop_sortie was not sent")
				_next(5)
			elif _session.state() == Bp.SESSION_FAILED:
				_fail("the resumed sortie failed")
				return _finish()
		5:
			if _campaign.phase() == Bp.PHASE_AFTER_ACTION:
				_after_action_report()
				_open_dock()
				_next(6)
		6:
			# The dock was closed by hand: the campaign returns to the chart table.
			if _campaign.phase() == Bp.PHASE_CHART:
				print("campaign_test: back at the chart after the dock; refit quote ", _campaign.refit_quote())
				_save_load_cleanup()
				return _finish()
	return false


func _start() -> bool:
	_campaign = root.get_node("Campaign") as BpCampaign
	_session = root.get_node("Session") as BpSession
	if _campaign == null or _session == null:
		_fail("no Campaign or Session autoload")
		return _finish()
	if _campaign.suspend_slot() != null:
		_fail("a suspend slot exists already; run with a throwaway HOME")
		return _finish()
	for s: String in ["chart_changed", "resources_changed", "phase_changed", "hour_advanced", "theatre_event", "interrupted", "rejected", "after_action_ready", "saved"]:
		_signals[s] = 0
	_campaign.chart_changed.connect(func() -> void: _signals["chart_changed"] += 1; _campaign.chart())
	_campaign.resources_changed.connect(func() -> void: _signals["resources_changed"] += 1; _campaign.resources())
	_campaign.phase_changed.connect(func(_p: int) -> void: _signals["phase_changed"] += 1)
	_campaign.hour_advanced.connect(func(_h: int) -> void: _signals["hour_advanced"] += 1; _hours_seen += 1)
	_campaign.theatre_event.connect(func(_e: Dictionary) -> void: _signals["theatre_event"] += 1)
	_campaign.interrupted.connect(func(i: Dictionary) -> void: _signals["interrupted"] += 1; _interrupt = i)
	_campaign.rejected.connect(func(t: Dictionary) -> void: _signals["rejected"] += 1; _rejected.append(str(t)))
	_campaign.after_action_ready.connect(func() -> void: _signals["after_action_ready"] += 1)
	_campaign.saved.connect(func(slot: Dictionary) -> void: _signals["saved"] += 1; _saved_paths.append(str(slot.get("path", ""))))

	# A bad starter is refused with a problem and starts nothing.
	var bad := _campaign.new_campaign({"starter": "starters/corvette40.bp.ron"})
	if bad.is_empty() or _campaign.is_active():
		_fail("a later act's starter was accepted")
	var problems := _campaign.new_campaign({
		"slot_name": "Campaign test", "difficulty": {"preset": &"normal", "ai_accuracy": 1.0, "ai_reaction_s": 2.0, "adaptation_cadence": 1.0, "hull_count": 300, "economy": 1.0, "clock_days": 40},
		"ironman": false, "seed": 4242, "starter": STARTER, "tutorial_done": true,
	})
	if not problems.is_empty():
		_fail("new_campaign refused: %s" % str(problems))
		return _finish()
	if _campaign.phase() != Bp.PHASE_CHART:
		_fail("phase after new_campaign is %d" % _campaign.phase())
	var chart := _campaign.chart()
	var res := _campaign.resources()
	var players := _campaign.players()
	var act := _campaign.act_info()
	print("campaign_test: day %d hour %d at %s, %d offers, %d coves, salvage %d, %d player(s), act %d cap %.0f t" % [chart.get("day", 0), chart.get("hour", 0), chart.get("sextant", ""), chart.get("offers", []).size(), chart.get("coves", []).size(), res.get("salvage", 0), players.size(), act.get("act", -1), act.get("cap_t", 0.0)])
	if int(res.get("salvage", 0)) != 400:
		_fail("starting Salvage is %d, not 400" % res.get("salvage", 0))
	if players.size() != 1 or not bool(players[0].get("is_local", false)):
		_fail("players(): %s" % str(players))
	if (chart.get("offers", []) as Array).is_empty():
		_fail("no operation on offer")
	if int(chart.get("can_save", -1)) != Bp.SAVE_ALLOWED:
		_fail("cannot save at the start: %d" % chart.get("can_save", -1))
	if (_campaign.current_slot().get("path", "") as String).is_empty():
		_fail("the new campaign was not written to disk")
	if _campaign.difficulty().get("preset", &"") != &"normal":
		_fail("difficulty preset is %s" % str(_campaign.difficulty()))
	if not _campaign.set_difficulty({"ai_accuracy": 1.2, "ai_reaction_s": 2.0, "adaptation_cadence": 1.0, "hull_count": 300, "economy": 1.0, "clock_days": 40}):
		_fail("set_difficulty refused between operations")
	if _campaign.set_difficulty({"ai_accuracy": 1.0, "ai_reaction_s": 2.0, "adaptation_cadence": 1.0, "hull_count": 400, "economy": 1.0, "clock_days": 40}):
		_fail("hull count changed after the start")
	var q := _campaign.service_quotes()
	print("campaign_test: quotes ", q, " bulletin ", _campaign.doctrine_bulletin().size(), " intel options ", _campaign.intel_options().size())
	for cove: StringName in chart.get("coves", []):
		if cove != chart.get("sextant", &""):
			var p := _campaign.transit_preview(cove)
			print("campaign_test: transit to %s: %s" % [cove, str(p)])
			break
	# A 6 hour wait commits and runs at once.
	_campaign.set_advance_speed(48.0)
	if not _campaign.propose({"kind": Bp.THEATRE_ACTION_WAIT, "hours": 6}):
		_fail("the wait was refused: %s" % str(_rejected))
		return _finish()
	if not _campaign.is_advancing():
		_fail("a committed wait did not start theatre time")
	if _campaign.propose({"kind": Bp.THEATRE_ACTION_WAIT}):
		_fail("an incomplete action was accepted")
	_next(1)
	return false


func _start_transit() -> void:
	var c := _campaign.chart()
	for cove: StringName in c.get("coves", []):
		if cove != c.get("sextant", &"") and bool(_campaign.transit_preview(cove).get("reachable", false)):
			_transit_to = cove
			break
	if _transit_to == &"":
		_fail("no cove to sail to")
		return
	_interrupt = {}
	_hours_seen = 0
	if not _campaign.propose({"kind": Bp.THEATRE_ACTION_TRANSIT, "node": _transit_to}):
		_fail("the transit was refused: %s" % str(_rejected))
		return
	if not _campaign.is_advancing():
		_fail("a committed transit did not start theatre time")
	_next(7)


func _brief_and_launch() -> void:
	var offers: Array = _campaign.chart().get("offers", [])
	if offers.is_empty():
		_fail("no offer after the wait")
		return
	_op = offers[0].get("id", &"")
	var b: Variant = _campaign.briefing(_op)
	if b == null:
		_fail("no briefing for %s" % _op)
		return
	print("campaign_test: briefing %s at %s, %.1f km, %d threats, %d tags, dtg %s" % [b.get("name", ""), b.get("node", ""), b.get("distance_km", 0.0), b.get("threats", []).size(), b.get("tags", []).size(), b.get("dtg", "")])
	if not _campaign.open_briefing(_op):
		_fail("open_briefing refused: %s" % str(_rejected))
		return
	if _campaign.phase() != Bp.PHASE_BRIEFING:
		_fail("phase after open_briefing is %d" % _campaign.phase())
	if not _campaign.launch_sortie():
		_fail("launch_sortie refused: %s" % str(_rejected))
		return
	if _campaign.phase() != Bp.PHASE_SORTIE or _campaign.can_save() != Bp.SAVE_IN_SORTIE:
		_fail("after launch: phase %d, can_save %d" % [_campaign.phase(), _campaign.can_save()])
	_next(2)


func _open_dock() -> void:
	if not _campaign.open_dry_dock():
		_fail("open_dry_dock refused: %s" % str(_rejected))
		return
	if _campaign.phase() != Bp.PHASE_DRY_DOCK:
		_fail("phase after open_dry_dock is %d" % _campaign.phase())
	var editor := root.get_node("Editor")
	var issues: Array = editor.call("open", Bp.DOCK_CAMPAIGN, {})
	for i: Dictionary in issues:
		if bool(i.get("fatal", false)):
			_fail("the campaign dock did not open: %s" % str(i))
	print("campaign_test: dock open %s, limits %s" % [str(editor.call("is_open")), str(editor.call("limits"))])
	editor.call("close")


func _after_action_report() -> void:
	var aa: Variant = _campaign.after_action()
	if aa == null:
		_fail("no after-action report")
		return
	print("campaign_test: after action outcome %d, payout %s, %d events, pushback %.1f" % [aa.get("outcome", -1), str(aa.get("payout", {})), aa.get("events", []).size(), aa.get("clock_pushback_days", 0.0)])
	if int(aa.get("outcome", -1)) != Bp.OUTCOME_WITHDRAWN:
		_fail("outcome is %d" % aa.get("outcome", -1))
	_campaign.close_after_action()
	if _campaign.phase() != Bp.PHASE_CHART:
		_fail("phase after the report is %d" % _campaign.phase())


func _save_load_cleanup() -> void:
	if _campaign.theatre_log(50).is_empty():
		_fail("the theatre log is empty")
	if not _campaign.save_now():
		_fail("save_now refused: %s" % str(_rejected))
	var path := _campaign.current_slot().get("path", "") as String
	var slots := _campaign.slots()
	print("campaign_test: %d slot(s) on disk; current %s" % [slots.size(), path])
	var found := false
	for s: Dictionary in slots:
		if s.get("path", "") == path:
			found = bool(s.get("loadable", false))
	if not found:
		_fail("the saved slot is not listed as loadable")
	var problems := _campaign.load_slot(path)
	if not problems.is_empty() or _campaign.phase() != Bp.PHASE_CHART:
		_fail("load_slot: %s, phase %d" % [str(problems), _campaign.phase()])
	if not _campaign.load_slot("user://../secret.bpsave").size() == 1:
		_fail("a path outside the saves folder was not refused")
	var campaign_id := _campaign.current_slot().get("campaign_id", "") as String
	_campaign.close_campaign()
	if _campaign.is_active() or _campaign.phase() != Bp.PHASE_NONE:
		_fail("close_campaign left the campaign open")
	# Clean up every file of this campaign.
	for s: Dictionary in _campaign.slots():
		if s.get("campaign_id", "") == campaign_id and not _campaign.delete_slot(s.get("path", "")):
			_fail("delete_slot failed for %s" % s.get("path", ""))
	for s: Dictionary in _campaign.slots():
		if s.get("campaign_id", "") == campaign_id:
			_fail("a file of the test campaign is left: %s" % s.get("path", ""))
	# A reused seed starts a separate campaign; Ironman keeps one rolling file.
	var ids: Array[String] = []
	for ironman: bool in [false, true]:
		var p := _campaign.new_campaign({"slot_name": "Seed test", "seed": 99, "ironman": ironman})
		if not p.is_empty():
			_fail("new_campaign (ironman %s) refused: %s" % [str(ironman), str(p)])
			continue
		var slot := _campaign.current_slot()
		ids.append(slot.get("campaign_id", "") as String)
		if ironman and not (slot.get("path", "") as String).ends_with("/ironman.bpsave"):
			_fail("the Ironman save is %s" % slot.get("path", ""))
		if ironman and not _campaign.save_now():
			_fail("an Ironman save_now at the cove was refused")
		_campaign.close_campaign()
	if ids.size() == 2 and ids[0] == ids[1]:
		_fail("two campaigns with one seed share the id %s" % ids[0])
	var ironman_files := 0
	for s: Dictionary in _campaign.slots():
		if s.get("campaign_id", "") in ids:
			if s.get("campaign_id", "") == ids[-1]:
				ironman_files += 1
			_campaign.delete_slot(s.get("path", ""))
	if ironman_files != 1:
		_fail("the Ironman campaign has %d files" % ironman_files)


func _next(step: int) -> void:
	_step = step
	_step_frame = _frame


func _fail(text: String) -> void:
	_failures += 1
	printerr("campaign_test: FAIL ", text)


func _finish() -> bool:
	print("campaign_test: signals ", _signals)
	if _signals.get("saved", 0) < 2:
		_fail("saved fired %d times" % _signals.get("saved", 0))
	if _signals.get("hour_advanced", 0) < 1:
		_fail("hour_advanced never fired")
	if _signals.get("after_action_ready", 0) != 1:
		_fail("after_action_ready fired %d times" % _signals.get("after_action_ready", 0))
	if _session != null:
		_session.leave(false)
	print("campaign_test: %s" % ("PASS" if _failures == 0 else "%d failure(s)" % _failures))
	quit(1 if _failures > 0 else 0)
	return true
