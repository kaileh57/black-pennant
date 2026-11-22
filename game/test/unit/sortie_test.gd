extends SceneTree
## Headless proof of the first playable sortie. Units gd-link-channel, gd-host, gd-session. Run from game/:
##   godot --headless --path . -s res://test/unit/sortie_test.gd
## Starts a skirmish through the Session autoload with the 22 m starter against a small enemy group, plays
## 600 frames of the sortie at 60 frames a second, orders a course and fires the guns by hand through
## BpSessionCheck (the Orders facade is a later unit), pauses, compresses time and prints what the snapshots
## showed. Then it ends the sortie on the host thread, checks the end and the summary, leaves, starts a sandbox
## with a hostile and a passive target row, sees it run, leaves again, and exits 1 on any failure. With
## `-- --frames=N` it plays N skirmish frames instead of 600.

const STARTER := "starters/fac22.bp.ron"
const SORTIE_FRAMES := 600
## Frames counted from the first sortie frame.
const COURSE_AT := 60
const TAKE_AT := 90
const FIRE_AT: Array[int] = [100, 160, 220, 280]
## Pause for a second of frames, then ask for 4x compression and come back to 1x.
const PAUSE_AT := 320
const PAUSE_CHECK: Array[int] = [360, 380]
const RESUME_AT := 381
const COMPRESS_AT := 420
const COMPRESS_CHECK := 470
const UNCOMPRESS_AT := 500
## Frames allowed for loading before the run fails.
const LOADING_LIMIT := 900
## Frames the ended sortie has to report its end, and frames the sandbox runs.
const END_LIMIT := 180
const SANDBOX_FRAMES := 180
const TARGET_CLASS := "combine/act1/gunboat.bp.ron"

var _session: BpSession
var _check: BpSessionCheck
var _frame := 0
var _sortie_frame := -1
var _frames := SORTIE_FRAMES
var _failures := 0
var _signals: Dictionary[String, int] = {}
var _states: Array[int] = []
var _start_pos := Vector2.ZERO
var _course_to := Vector2.ZERO
var _course_bearing := 0.0
var _guns: PackedInt32Array = []
var _max_tracks := 0
var _loading := 0.0
var _failed_text := ""
var _wall_start := 0
var _paused_ticks: Array[int] = []
var _compress_rate := 0
## 0 skirmish, 1 waiting for the end, 2 sandbox loading, 3 sandbox running.
var _phase := 0
var _phase_frame := 0


func _initialize() -> void:
	Engine.max_fps = 60
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--frames="):
			_frames = maxi(60, arg.trim_prefix("--frames=").to_int())
	_check = BpSessionCheck.new()


func _process(_delta: float) -> bool:
	_frame += 1
	if _frame == 1:
		return _start()
	_check.observe()
	var state := _session.state()
	if _phase > 0:
		return _after_skirmish(state)
	if _sortie_frame < 0:
		if state == Bp.SESSION_IN_SORTIE:
			_sortie_frame = _frame
			_wall_start = Time.get_ticks_msec()
			var f := _check.facts()
			_start_pos = f.get("pos", Vector2.ZERO)
			print("sortie_test: in sortie after %d frames; own %d at %s heading %.0f, %d vessels, mounts %s" % [_frame, f.get("own", -1), str(_start_pos), f.get("heading_deg", 0.0), f.get("vessels", 0), str(f.get("mounts", []))])
		elif state == Bp.SESSION_FAILED or _frame > LOADING_LIMIT:
			_fail("the sortie never started (state %d, %s)" % [state, _failed_text])
			return _finish()
		return false
	var since := _frame - _sortie_frame
	if since == COURSE_AT:
		_order_course()
	if since == TAKE_AT:
		# The own design is drawn by now: its gun mounts (decoy and missile launchers take other orders).
		_guns = _check.facts().get("guns", PackedInt32Array())
		print("sortie_test: taking guns ", _guns)
		if _guns.is_empty():
			_fail("the own design has no gun mount to take")
		for slot: int in _guns:
			if not _check.take_mount(slot, true):
				_fail("take_mount %d was not sent" % slot)
	if since in FIRE_AT:
		for slot: int in _guns:
			_check.fire_mount(slot, -1)
	if since == PAUSE_AT and not _session.set_paused(true):
		_fail("set_paused(true) refused in single-player")
	if since in PAUSE_CHECK:
		_paused_ticks.append(int(_check.facts().get("tick", -1)))
	if since == RESUME_AT:
		if not _session.pacing().get("paused", false):
			_fail("pacing does not say paused")
		if _paused_ticks.size() != 2 or _paused_ticks[0] != _paused_ticks[1]:
			_fail("time moved while paused: %s" % str(_paused_ticks))
		_session.set_paused(false)
	if since == COMPRESS_AT and not _session.request_time_rate(4):
		_fail("request_time_rate(4) refused")
	if since == COMPRESS_CHECK:
		_compress_rate = int(_session.pacing().get("rate", 0))
		print("sortie_test: effective rate after asking for 4x: %d (blocked %d)" % [_compress_rate, _session.pacing().get("blocked", -1)])
	if since == UNCOMPRESS_AT:
		_session.step_time_rate(-1)
		_session.request_time_rate(1)
	_max_tracks = maxi(_max_tracks, int(_check.facts().get("tracks", 0)))
	if since >= _frames or state == Bp.SESSION_ENDED or state == Bp.SESSION_FAILED:
		_report()
		if state != Bp.SESSION_IN_SORTIE:
			_fail("the skirmish left the sortie early (state %d)" % state)
			return _finish()
		if not _check.stop_sortie(Bp.OUTCOME_WITHDRAWN):
			_fail("stop_sortie was not sent")
			return _finish()
		_phase = 1
		_phase_frame = _frame
	return false


## The end of the skirmish, then a sandbox on the same process.
func _after_skirmish(state: int) -> bool:
	var waited := _frame - _phase_frame
	match _phase:
		1:
			if state == Bp.SESSION_ENDED:
				var summary: Variant = _session.last_summary()
				print("sortie_test: ended after %d frames, summary %s" % [waited, str(summary)])
				if _signals["sortie_ended"] != 1:
					_fail("sortie_ended fired %d times" % _signals["sortie_ended"])
				if summary == null or int((summary as Dictionary).get("outcome", -1)) != Bp.OUTCOME_WITHDRAWN:
					_fail("the summary does not carry the outcome: %s" % str(summary))
				# Single-player runs the server over loopback (L27, L899), which records the sortie.
				var replays := _session.replays()
				print("sortie_test: %d replay(s) after the skirmish" % replays.size())
				if replays.is_empty():
					_fail("the offline skirmish recorded no replay")
				_session.leave(false)
				if _session.state() != Bp.SESSION_IDLE:
					_fail("leave did not return to idle (state %d)" % _session.state())
				var problems := _session.start_sandbox({
					"blueprint": STARTER, "weather": Bp.WEATHER_HAZE, "sea_state": 2, "time_of_day": Bp.TIME_DAY, "seed": 77,
					"targets": [{"blueprint": TARGET_CLASS, "hostile": true, "count": 2}, {"blueprint": TARGET_CLASS, "hostile": false, "count": 1}],
				})
				if not problems.is_empty():
					_fail("start_sandbox refused: %s" % str(problems))
					return _finish()
				_phase = 2
				_phase_frame = _frame
			elif waited > END_LIMIT:
				_fail("the stopped sortie never ended (state %d)" % state)
				return _finish()
		2:
			if state == Bp.SESSION_IN_SORTIE:
				_phase = 3
				_phase_frame = _frame
			elif state == Bp.SESSION_FAILED or waited > LOADING_LIMIT:
				_fail("the sandbox never started (state %d, %s)" % [state, _failed_text])
				return _finish()
		3:
			if waited >= SANDBOX_FRAMES:
				var f := _check.facts()
				var info := _session.info()
				print("sortie_test: sandbox ran %d frames: own %d, tick %d, mode %d, kind %d, weather %d, designs %d" % [waited, f.get("own", -1), f.get("tick", -1), info.get("mode", -1), info.get("kind", -1), (info.get("weather", {}) as Dictionary).get("kind", -1), f.get("designs", 0)])
				if int(f.get("own", -1)) < 0:
					_fail("the sandbox has no own ship")
				if int(info.get("kind", -1)) != Bp.KIND_SANDBOX:
					_fail("the session is not a sandbox")
				if _signals["sortie_started"] != 2:
					_fail("sortie_started fired %d times over two sorties" % _signals["sortie_started"])
				return _finish()
	return false


func _start() -> bool:
	_session = root.get_node("Session") as BpSession
	if _session == null:
		_fail("no Session autoload")
		return _finish()
	for s: String in ["state_changed", "loading_progress", "sortie_started", "sortie_ended", "pacing_changed", "command_rejected", "connection_changed", "failed"]:
		_signals[s] = 0
	_session.state_changed.connect(func(st: int) -> void: _signals["state_changed"] += 1; _states.append(st))
	_session.loading_progress.connect(func(f: float) -> void: _signals["loading_progress"] += 1; _loading = f)
	_session.sortie_started.connect(func() -> void: _signals["sortie_started"] += 1)
	_session.sortie_ended.connect(func(_s: Dictionary) -> void: _signals["sortie_ended"] += 1)
	_session.pacing_changed.connect(func() -> void: _signals["pacing_changed"] += 1; _session.pacing())
	_session.command_rejected.connect(_on_rejected)
	_session.connection_changed.connect(func() -> void: _signals["connection_changed"] += 1; _session.connections())
	_session.failed.connect(func(t: Dictionary) -> void: _signals["failed"] += 1; _failed_text = str(t))
	var setup := {
		"template": Bp.MISSION_CONVOY_RAID, "act": Bp.ACT_1, "enemy_per_player": 2, "difficulty": {"preset": "normal"},
		"weather": Bp.WEATHER_CLEAR, "sea_state": 3, "time_of_day": Bp.TIME_DUSK, "seed": 1234, "blueprint": STARTER,
	}
	var problems := _session.start_skirmish(setup)
	if not problems.is_empty():
		_fail("start_skirmish refused: %s" % str(problems))
		return _finish()
	if _session.state() != Bp.SESSION_LOADING:
		_fail("not loading after start_skirmish (state %d)" % _session.state())
	return false


## A course 60 degrees to starboard of the ship's head, 3 km out, at 25 kn.
func _order_course() -> void:
	var f := _check.facts()
	var here: Vector2 = f.get("pos", _start_pos)
	_course_bearing = fmod(float(f.get("heading_deg", 0.0)) + 60.0, 360.0)
	var rad := deg_to_rad(_course_bearing)
	# Compass bearing to map (x, z): north is -z, east is +x.
	_course_to = here + Vector2(sin(rad), -cos(rad)) * 3000.0
	if not _check.set_course(_course_to, 25.0):
		_fail("set_course was not sent")


func _on_rejected(text: Dictionary) -> void:
	_signals["command_rejected"] += 1
	print("sortie_test: command rejected: ", text)


func _report() -> void:
	var f := _check.facts()
	var info := _session.info()
	var pacing := _session.pacing()
	var pos: Vector2 = f.get("pos", _start_pos)
	var moved := pos.distance_to(_start_pos)
	var wall_s := (Time.get_ticks_msec() - _wall_start) / 1000.0
	var ordered := float(f.get("ordered_heading_deg", -1.0))
	var off := absf(fmod(ordered - _course_bearing + 540.0, 360.0) - 180.0)
	print("sortie_test: %d sortie frames in %.1f s wall, sortie time %.2f s, host tick %.1f Hz, tick %d" % [_frame - _sortie_frame, wall_s, _session.time_s(), _session.host_tick_hz(), f.get("tick", -1)])
	print("sortie_test: own ship moved %.0f m (from %s to %s), heading %.0f, speed %.1f kn" % [moved, str(_start_pos), str(pos), f.get("heading_deg", 0.0), f.get("speed_kn", 0.0)])
	print("sortie_test: ordered heading %.0f for a course bearing %.0f (off %.0f), ordered speed %.1f kn, route %d" % [ordered, _course_bearing, off, f.get("ordered_speed_kn", 0.0), f.get("route", 0)])
	print("sortie_test: tracks now %d, at most %d, created %d; vessels drawn %d, reveals %d, designs %d" % [f.get("tracks", 0), _max_tracks, f.get("tracks_created", 0), f.get("vessels", 0), f.get("reveals", 0), f.get("designs", 0)])
	print("sortie_test: events %d; mounts fired %d (own %d); projectiles %d (own %d); shells in flight now %d; rejected %d" % [f.get("events", 0), f.get("mount_fired", 0), f.get("own_mount_fired", 0), f.get("projectiles", 0), f.get("own_projectiles", 0), f.get("shells", 0), f.get("rejected", 0)])
	print("sortie_test: event kinds ", f.get("kinds", {}))
	print("sortie_test: signals ", _signals, " states ", _states, " extrapolated %.3f" % f.get("extrapolated", 0.0))
	print("sortie_test: pacing ", pacing)
	print("sortie_test: session ", info)
	print("sortie_test: connections ", _session.connections())
	if _signals["failed"] > 0:
		_fail("failed: " + _failed_text)
	if _loading < 1.0:
		_fail("loading never reached 1 (%.2f)" % _loading)
	if moved < 20.0:
		_fail("the own ship did not move (%.1f m)" % moved)
	if off > 15.0:
		_fail("the ordered heading does not follow the course (off %.0f degrees)" % off)
	if int(f.get("own_projectiles", 0)) == 0 and int(f.get("own_mount_fired", 0)) == 0:
		_fail("no own gun fired")
	if int(f.get("rejected", 0)) > 0:
		_fail("%d commands were rejected" % int(f.get("rejected", 0)))
	if _max_tracks == 0:
		_fail("no track was ever held")
	if _signals["pacing_changed"] < 2:
		_fail("pause and resume did not announce pacing (%d)" % _signals["pacing_changed"])
	if _session.time_s() < (_frames / 60.0) * 0.5:
		_fail("sortie time %.1f s lags the %d frames" % [_session.time_s(), _frames])


func _finish() -> bool:
	if _session != null:
		_session.leave(false)
		if _session.state() != Bp.SESSION_IDLE:
			_fail("leave did not return to idle")
	print("sortie_test: ", "FAILED (%d)" % _failures if _failures > 0 else "passed")
	quit(1 if _failures > 0 else 0)
	return true


func _fail(text: String) -> void:
	_failures += 1
	push_error("sortie_test: " + text)
