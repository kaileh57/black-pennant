extends Node
## End-to-end check of the test feed, the tutorial driver and the benchmark driver (units gd-testfeed,
## gd-tutorial, gd-benchmark) through the real autoloads:
##   godot --headless --path game res://test/capture/scripted_check.tscn
## Every built-in script is installed as the session, advanced, steered and stopped; the tutorial opens the
## dock and walks its first steps; a shortened benchmark runs a battle stage, the dry-dock stage and
## sim-calib. Exits 1 on any failure. With a window and `-- --out=<dir>` it also shows res://world/world.tscn
## and saves a PNG of the scripted battle and of the reference battle:
##   godot --path game --resolution 1600x900 res://test/capture/scripted_check.tscn -- --out=/abs/dir
## When instanced by check_project it does nothing.

var _failures := 0
var _out := ""
var _world: Node3D


func _ready() -> void:
	if get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--out="):
			_out = arg.trim_prefix("--out=")
	if not _out.is_empty():
		DirAccess.make_dir_recursive_absolute(_out)
		_world = (load("res://world/world.tscn") as PackedScene).instantiate() as Node3D
		add_child(_world)
	_run.call_deferred()


func _run() -> void:
	await _frames(2)
	await _check_feed()
	await _check_tutorial()
	if _out.is_empty():
		await _check_bench()
	else:
		await _capture_bench()
	print("scripted_check: %d failures" % _failures)
	get_tree().quit(1 if _failures > 0 else 0)


func _frames(n: int) -> void:
	for i: int in n:
		await get_tree().process_frame


func _expect(ok: bool, what: String) -> void:
	if not ok:
		_failures += 1
		push_error("scripted_check: " + what)


func _check_feed() -> void:
	# The feed is used as its doc says: created with new(), never added to the tree, freed after use.
	var feed := BpTestFeed.new()
	var check := BpSessionCheck.new()
	var lost: Array[bool] = []
	var on_lost := func(can_rehost: bool) -> void: lost.append(can_rehost)
	Session.host_lost.connect(on_lost)
	_expect(not feed.start("no_such_script"), "an unknown script starts")
	for script_name: String in feed.scripts():
		var started := Time.get_ticks_msec()
		_expect(feed.start(script_name), "script %s does not start" % script_name)
		_expect(not feed.start("calm"), "a second script starts while %s is live" % script_name)
		var built_ms := Time.get_ticks_msec() - started
		await _frames(1)
		var loading := Session.state()
		feed.advance(30)
		await _frames(1)
		check.observe()
		_expect(Session.state() == Bp.SESSION_IN_SORTIE, "%s: session state %d after the first frames (was %d)" % [script_name, Session.state(), loading])
		_expect(Session.info().get("kind") == Bp.KIND_SCRIPTED, "%s: session kind %s" % [script_name, Session.info().get("kind")])
		# A minute of play (20 s for the co-op member, whose host is lost at 40 s), the last half second one
		# frame at a time.
		feed.advance(1200 if script_name == "coop_host_loss" else 3600)
		await _frames(1)
		check.observe()
		for i: int in 30:
			feed.advance(2)
			await _frames(1)
			check.observe()
		var facts := check.facts()
		var tick: int = facts.get("tick", -1)
		_expect(tick > (1100 if script_name == "coop_host_loss" else 3500), "%s: sampled tick %d" % [script_name, tick])
		_expect(float(facts.get("extrapolated", 1.0)) < 0.01, "%s: extrapolated %s" % [script_name, facts.get("extrapolated")])
		# An order reaches the script and turns the ship.
		var pos: Vector2 = facts.get("pos", Vector2.ZERO)
		if script_name != "coop_host_loss":
			_expect(check.set_course(pos + Vector2(4000, 0), 20.0), "%s: the course order was not sent" % script_name)
			var commands := feed.take_commands()
			_expect(commands.size() == 1 and commands[0].begins_with("SetCourse"), "%s: captured %s" % [script_name, commands])
			feed.advance(60 * 30)
			await _frames(1)
			var after := check.facts()
			_expect(absf(float(after.get("ordered_heading_deg", 0.0)) - 90.0) < 5.0, "%s: ordered heading %s" % [script_name, after.get("ordered_heading_deg")])
		print("scripted_check: %s built in %d ms, tick %d, vessels %s, tracks %s, designs %s, events %s" % [script_name, built_ms, tick, facts.get("vessels"), facts.get("tracks"), facts.get("designs"), facts.get("events")])
		if not _out.is_empty() and (script_name == "battle" or script_name == "reference_battle"):
			for i: int in 90:
				feed.advance(1)
				await _frames(1)
			var img := get_viewport().get_texture().get_image()
			img.save_png(_out.path_join("feed_%s.png" % script_name))
		if script_name == "coop_host_loss":
			# Host silent at 30 s: host_lost, and a re-host is possible (the succession list names a member).
			feed.advance(60 * 12)
			await _frames(2)
			_expect(lost == [true], "coop_host_loss: host_lost %s at 32 s" % [lost])
			# Lost at 40 s. The state after it depends on link::net::end_msg (unit gd-link-net).
			feed.advance(60 * 10)
			await _frames(2)
			print("scripted_check: coop_host_loss: session state %d after host loss" % Session.state())
		feed.stop()
		_expect(Session.state() == Bp.SESSION_IDLE, "%s: session state %d after stop" % [script_name, Session.state()])
	# Stop then start in one frame works.
	_expect(feed.start("calm"), "calm does not start right after a stop")
	feed.stop()
	await _frames(2)
	Session.host_lost.disconnect(on_lost)
	feed.free()


func _check_tutorial() -> void:
	var steps: Array[String] = []
	var on_step := func(step: Dictionary) -> void: steps.append(String(step.get("id", "")))
	Tutorial.step_changed.connect(on_step)
	_expect(not Tutorial.start(9), "fallback level 9 starts")
	_expect(Tutorial.is_allowed("editor.tray"), "a control is gated while off")
	_expect(Tutorial.start(0), "the tutorial does not start")
	await _frames(2)
	_expect(Tutorial.state() == Bp.TUTORIAL_RUNNING, "tutorial state %d" % Tutorial.state())
	_expect(Editor.is_open() and Editor.dock_mode() == Bp.DOCK_TUTORIAL, "the dock did not open in tutorial mode")
	var step: Dictionary = Tutorial.step()
	_expect(step.get("id") == "open" and bool(step.get("needs_continue")), "first step %s" % step)
	_expect(not Tutorial.is_allowed("editor.tray") and Tutorial.is_allowed("tutorial.continue") and Tutorial.is_allowed("editor.camera.orbit"), "first step gating")
	Tutorial.continue_step()
	await _frames(1)
	step = Tutorial.step()
	_expect(step.get("id") == "beam" and step.get("point_at") == "editor.handle.beam", "second step %s" % step)
	_expect(Tutorial.is_allowed("editor.handle.beam") and not Tutorial.is_allowed("editor.launch"), "beam step gating")
	_expect(steps == ["open", "beam"], "announced steps %s" % [steps])
	Tutorial.stop()
	_expect(Tutorial.state() == Bp.TUTORIAL_OFF and Tutorial.step() == null, "tutorial did not stop")
	var log_text := Tutorial.log_text()
	_expect(log_text.contains("open:") and log_text.contains("fallback level 0"), "log text %s" % log_text)
	Tutorial.step_changed.disconnect(on_step)
	Editor.close()
	print("scripted_check: tutorial log\n" + log_text)


func _check_bench() -> void:
	var stages := PackedInt32Array([Bp.BENCH_REFERENCE_BATTLE, Bp.BENCH_EDITOR, Bp.BENCH_SIM_CALIB])
	var seen: Array[int] = []
	var on_stage := func(s: Dictionary) -> void:
		var st: int = s.get("stage", -1)
		if seen.is_empty() or seen[-1] != st:
			seen.append(st)
	Bench.stage_changed.connect(on_stage)
	_expect(Bench.start({"stages": stages, "duration_scale": 0.05, "tier": Bp.QUALITY_LOW}), "the benchmark does not start")
	_expect(not Bench.start({}), "a second benchmark starts")
	var waited := 0
	while Bench.stage().get("stage") != Bp.BENCH_DONE and waited < 60 * 240:
		await _frames(1)
		Bench.frame_sample(16.7, -1.0, false)
		waited += 1
	var report := Bench.report()
	_expect(not report.is_empty(), "no report after %d frames (stage %s)" % [waited, Bench.stage()])
	var runs: Array = report.get("stages", [])
	_expect(runs.size() == 2, "battle stages in the report: %d" % runs.size())
	for r: Dictionary in runs:
		_expect(not bool(r.get("failed", true)) and int(r.get("frames", 0)) > 10, "stage %s" % r)
	_expect(float(report.get("sim_calib_us", 0.0)) > 0.0, "sim-calib %s" % report.get("sim_calib_us"))
	_expect(float(report.get("editor_gauge_ms_p95", 0.0)) > 0.0 and float(report.get("editor_splash_ms_p95", 0.0)) > 0.0, "editor stage figures")
	_expect(Session.state() == Bp.SESSION_IDLE, "session state %d after the benchmark" % Session.state())
	_expect(seen.has(Bp.BENCH_REFERENCE_BATTLE) and seen.has(Bp.BENCH_EDITOR) and seen.has(Bp.BENCH_SIM_CALIB) and seen[-1] == Bp.BENCH_DONE, "stages seen %s" % [seen])
	var path := Bench.save_report()
	_expect(path.begins_with("user://bench/") and FileAccess.file_exists(path), "report file %s" % path)
	var parsed: Variant = JSON.parse_string(FileAccess.get_file_as_string(path))
	_expect(parsed is Dictionary, "the report file is not JSON")
	_expect(Bench.issue_form_url().begins_with("https://github.com/"), "issue form %s" % Bench.issue_form_url())
	print("scripted_check: benchmark report " + String(report.get("json", "")))
	# The other battle stages and the soak, shortened.
	seen.clear()
	_expect(Bench.start({"stages": PackedInt32Array([Bp.BENCH_COOP_REFERENCE, Bp.BENCH_MINIMUM_SWARM]), "soak": true, "duration_scale": 0.05, "tier": Bp.QUALITY_MEDIUM}), "the second benchmark does not start")
	waited = 0
	while Bench.stage().get("stage") != Bp.BENCH_DONE and waited < 60 * 400:
		await _frames(1)
		Bench.frame_sample(8.0, 5.0, waited % 100 == 0)
		waited += 1
	report = Bench.report()
	runs = report.get("stages", [])
	_expect(runs.size() == 3, "battle stages in the second report: %d" % runs.size())
	for r: Dictionary in runs:
		_expect(not bool(r.get("failed", true)) and int(r.get("frames", 0)) > 10, "stage %s" % r)
	var soak: Variant = report.get("soak")
	_expect(soak is Dictionary and bool(soak.get("pass", false)), "soak %s" % [soak])
	_expect(seen.has(Bp.BENCH_COOP_REFERENCE) and seen.has(Bp.BENCH_MINIMUM_SWARM) and seen.has(Bp.BENCH_SOAK), "stages seen %s" % [seen])
	print("scripted_check: second benchmark report " + String(report.get("json", "")))
	# Cancel mid-stage: the session goes back to idle and the player's tier comes back.
	var tier_before: int = Settings.get_value(&"graphics.tier")
	_expect(Bench.start({"stages": PackedInt32Array([Bp.BENCH_REFERENCE_BATTLE]), "duration_scale": 0.5, "tier": Bp.QUALITY_LOW}), "the third benchmark does not start")
	await _frames(120)
	Bench.cancel()
	await _frames(3)
	_expect(Bench.stage().get("stage") == Bp.BENCH_IDLE and Session.state() == Bp.SESSION_IDLE, "after cancel: stage %s, session %d" % [Bench.stage(), Session.state()])
	if tier_before != -1:
		_expect(Settings.get_value(&"graphics.tier") == tier_before, "tier setting not restored")
	Bench.stage_changed.disconnect(on_stage)


## Windowed: a short reference-battle stage drawn by the world view, captured mid-stage; the report's
## adapter, window and pipeline figures come from the real renderer.
func _capture_bench() -> void:
	_expect(Bench.start({"stages": PackedInt32Array([Bp.BENCH_REFERENCE_BATTLE]), "duration_scale": 0.1}), "the windowed benchmark does not start")
	var waited := 0
	var shot := false
	while Bench.stage().get("stage") != Bp.BENCH_DONE and waited < 60 * 300:
		await _frames(1)
		waited += 1
		if not shot and Session.state() == Bp.SESSION_IN_SORTIE and float(Bench.stage().get("progress", 0.0)) > 0.5:
			shot = true
			get_viewport().get_texture().get_image().save_png(_out.path_join("bench_reference_battle.png"))
	var report := Bench.report()
	_expect(not report.is_empty(), "no windowed report")
	print("scripted_check: windowed benchmark report " + String(report.get("json", "")))
