extends Node
## Regression check for the alert strip with real alerts (DESIGN.md L1676-1687): an offline skirmish
## (convoy raid, Act 1, the 22 m starter, seed 7) plays at 4x, heading for the nearest Hostile
## track, until the simulation raises alerts and
## clears them, with the real strip scene (res://ui/hud/alert_strip.tscn) live on Hud. Each time the
## strip's dictionary or the strip itself changes, one line is printed with the sortie time, every alert
## on the strip (id, priority word, state) and the rows the strip draws. The run checks that:
## - every alert Hud puts on the strip shows as a row with its sentence, in Hud's order, within the
##   frame after `alerts_changed`;
## - an alert that leaves the strip (a Caution that self-clears, a Cleared alert after its 30 s, an
##   acknowledged one rectified) is gone from the rows too, and the strip hides when nothing is left;
## - at least one alert was raised and at least one left the strip during the run.
## Drives the autoloads the menus call (Session, Hud), not the screens, so it runs headless:
##   HOME=<scratch> godot --headless --path game res://test/ui/alert_strip_play.tscn \
##       [-- --minutes=8 --template=0 --seed=7 --enemies=1 --limit=420 --wait=1]
## Never run it with the real HOME. Prints one line per check and exits 0 when every check passed.

const SESSION_IN_SORTIE := 3
const STARTER := "starters/fac22.bp.ron"
## Bp.MISSION_CONVOY_RAID by default (the run that met the most alerts); 9 is hunter-group survival.
var _template := 0
var _seed := 7
var _enemies := 1
## Sortie minutes the run plays at most (it stops early once an alert has come and gone and the strip
## is calm again, or has shown a Cleared alert leave).
var _minutes := 8.0
## Real seconds the whole run may take (`--limit=`).
var _limit_s := 420.0
## `--wait=1`: only head for Hostile tracks, else hold course (let hunters come).
var _wait := false
const STATE_CLEARED := 2
const STANCE_HOSTILE := 3

var _strip: Node
var _failed := 0
var _raised: Dictionary[int, bool] = {}
var _left: Dictionary[int, bool] = {}
var _cleared_seen: Dictionary[int, bool] = {}
var _cleared_left := 0
var _last_ids: Array[int] = []
var _last_line := ""
var _mismatch_frames := 0
## Frames in a row the rows have disagreed with Hud; one frame of lag after a change is allowed.
var _off_run := 0


func _ready() -> void:
	if get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--minutes="):
			_minutes = maxf(0.5, float(arg.trim_prefix("--minutes=")))
		elif arg.begins_with("--template="):
			_template = int(arg.trim_prefix("--template="))
		elif arg.begins_with("--enemies="):
			_enemies = int(arg.trim_prefix("--enemies="))
		elif arg.begins_with("--limit="):
			_limit_s = maxf(60.0, float(arg.trim_prefix("--limit=")))
		elif arg == "--wait=1":
			_wait = true
		elif arg.begins_with("--seed="):
			_seed = int(arg.trim_prefix("--seed="))
	process_mode = Node.PROCESS_MODE_ALWAYS
	_run.call_deferred()


func _check(ok: bool, what: String) -> void:
	print("alert strip: ", "ok   " if ok else "FAIL ", what)
	if not ok:
		_failed += 1


func _real_s() -> float:
	return Time.get_ticks_msec() / 1000.0


func _run() -> void:
	var start := _real_s()
	_strip = load("res://ui/hud/alert_strip.tscn").instantiate()
	add_child(_strip)
	for i in 30:
		await get_tree().process_frame
	var problems: Array = Session.start_skirmish({"blueprint": STARTER, "seed": _seed, "template": _template, "enemy_per_player": _enemies})
	_check(problems.is_empty(), "the skirmish starts (%s)" % [problems])
	while Session.state() != SESSION_IN_SORTIE and _real_s() - start < 120.0:
		await get_tree().process_frame
	_check(Session.state() == SESSION_IN_SORTIE, "the sortie is under way")
	if Session.state() != SESSION_IN_SORTIE:
		_finish()
		return
	Session.request_time_rate(4)
	var t0: float = Session.time_s()
	var next_status := 0.0
	while Session.state() == SESSION_IN_SORTIE and Session.time_s() - t0 < _minutes * 60.0 and _real_s() - start < _limit_s:
		await get_tree().process_frame
		_observe()
		if Session.time_s() - t0 >= next_status:
			next_status += 20.0
			_steer()
		if _cleared_left > 0 and _last_ids.is_empty():
			break
	print("alert strip: stopped at sortie time %.1f s, real %.1f s" % [Session.time_s() - t0, _real_s() - start])
	_check(not _raised.is_empty(), "alerts reached the strip (%d)" % _raised.size())
	_check(not _left.is_empty(), "alerts left the strip (%d)" % _left.size())
	if not _cleared_seen.is_empty():
		_check(_cleared_left > 0, "a Cleared alert left the strip after its hold (%d of %d)" % [_cleared_left, _cleared_seen.size()])
	else:
		print("alert strip: note no alert reached the Cleared state in this run")
	_check(_mismatch_frames == 0, "the rows matched Hud's strip on every frame after the first (%d frames off)" % _mismatch_frames)
	_finish()


## Head for the first Hostile track (else the primary objective) at full speed, so the sortie meets
## the enemy and its weapons, and print how many alerts and tracks the HUD holds.
func _steer() -> void:
	var goal: Variant = null
	for t: Dictionary in Hud.tracks():
		if int(t.get("stance", -1)) == STANCE_HOSTILE and t.get("pos", null) is Vector2:
			goal = t["pos"]
			break
	if goal == null and not _wait:
		for o: Dictionary in Hud.objectives():
			if not bool(o.get("ghost", false)) and o.get("marker", null) is Vector2:
				goal = o["marker"]
				break
	if goal == null and not _wait:
		for t: Dictionary in Hud.tracks():
			if t.get("pos", null) is Vector2:
				goal = t["pos"]
				break
	if goal is Vector2:
		Orders.set_course(PackedVector2Array([goal]), 40.0, false)
	print("alert strip: status t %.0f s alerts %d tracks %d goal %s" % [Session.time_s(), Hud.alerts(false).size(), Hud.tracks().size(), goal])


## Compare the strip's rows with Hud.alert_strip() once a frame, and log changes.
func _observe() -> void:
	var data: Dictionary = Hud.alert_strip()
	var shown: Array = data.get("shown", [])
	var ids: Array[int] = []
	var parts: PackedStringArray = []
	for a: Dictionary in shown:
		var id := int(a.get("id", -1))
		ids.append(id)
		_raised[id] = true
		if int(a.get("state", 0)) == STATE_CLEARED:
			_cleared_seen[id] = true
		parts.append("%d %s/%d" % [id, String(a.get("word", "")).get_slice(".", String(a.get("word", "")).get_slice_count(".") - 1), int(a.get("state", 0))])
	for id: int in _last_ids:
		if not ids.has(id):
			_left[id] = true
			if _cleared_seen.has(id):
				_cleared_left += 1
	_last_ids = ids
	var rows: PackedStringArray = []
	var rows_box: Node = _strip.get_node(^"Margin/Column/Rows")
	for row: Node in rows_box.get_children():
		if (row as Control).visible:
			rows.append((row.get_child(1) as Button).text)
	var want := mini(shown.size(), 5)
	var line := "strip [%s] more %d flood %s rows %d %s visible %s" % [", ".join(parts), int(data.get("more", 0)), bool(data.get("flood", false)), rows.size(), rows, _strip.visible]
	var off := rows.size() != want or (_strip as Control).visible != (want > 0 or int(data.get("more", 0)) > 0 or bool(data.get("flood", false)) or float(data.get("silenced_s", 0.0)) > 0.0)
	for i in mini(rows.size(), want):
		if rows[i].is_empty():
			off = true
	_off_run = _off_run + 1 if off else 0
	if _off_run >= 2:
		_mismatch_frames += 1
	if line != _last_line:
		_last_line = line
		print("alert strip: t %.1f s " % Session.time_s(), line, "  OFF" if off else "")


func _finish() -> void:
	if Session.state() == SESSION_IN_SORTIE:
		Session.leave(false)
	for i in 10:
		await get_tree().process_frame
	print("alert strip: %s" % ("passed" if _failed == 0 else "failed (%d)" % _failed))
	get_tree().quit(0 if _failed == 0 else 1)
