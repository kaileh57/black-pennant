extends SceneTree
## Headless proof of Splash (unit gd-editor-splash) end to end through the real session. Run from game/:
##   godot --headless --path . -s res://test/unit/splash_test.gd [-- --bp=starters/destroyer90.bp.ron]
## Opens a sandbox dock on a starter, splashes it (Space saves first), times the load, pushes a settings
## edit and a parts edit into the running basin, rewinds across the edit, restarts, ends and reads the report
## card; then a pre-built splash (the worker's ready world), a progressive splash, a shell edit that closes
## the basin back to the dock, and Launch against launch_state(). Files, trial histories and settings it
## changed are put back afterwards. Exits 1 on any failure.

const LOAD_LIMIT := 1500

var editor: Node
var session: Node
var settings: Node
var failures := 0
var cards: Array[Dictionary] = []
var returned: Array[Dictionary] = []
var jobs: Dictionary = {}
var bp := "starters/fac22.bp.ron"


func _initialize() -> void:
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--bp="):
			bp = arg.trim_prefix("--bp=")
	_run.call_deferred()


func check(ok: bool, what: String) -> void:
	if not ok:
		failures += 1
		push_error("splash_test: " + what)


func frames(n: int) -> void:
	for i in n:
		await process_frame


func wait_state(want: int, limit: int) -> bool:
	for i in limit:
		if int(session.state()) == want:
			return true
		await process_frame
	return int(session.state()) == want


func wait_card(before: int, limit: int) -> bool:
	for i in limit:
		if cards.size() > before:
			return true
		await process_frame
	return cards.size() > before


func wait_job(job: String, limit: int) -> bool:
	for i in limit:
		if int(jobs.get(job, -1)) == Bp.JOB_DONE:
			return true
		await process_frame
	return int(jobs.get(job, -1)) == Bp.JOB_DONE


func listing(dir: String) -> PackedStringArray:
	return DirAccess.get_files_at(dir) if DirAccess.dir_exists_absolute(dir) else PackedStringArray()


## End a running basin and wait for its card.
func end_basin() -> void:
	var n := cards.size()
	editor.splash_end()
	check(await wait_card(n, 400), "splash_ended arrives")
	check(not editor.in_splash(), "in_splash cleared")
	session.leave(false)
	await frames(10)


func _run() -> void:
	await process_frame
	editor = root.get_node("/root/Editor")
	session = root.get_node("/root/Session")
	settings = root.get_node("/root/Settings")
	editor.splash_ended.connect(func(card: Dictionary) -> void: cards.append(card))
	editor.returned_to_dock.connect(func(text: Dictionary) -> void: returned.append(text))
	editor.job_changed.connect(func(job: StringName, st: int) -> void: jobs[String(job)] = st)
	var files_before := listing("user://blueprints")
	var trials_before := listing("user://trials")
	var pre_before: Variant = settings.get_value(&"editor.prebuilt_splash")
	var prog_before: Variant = settings.get_value(&"editor.progressive_splash")
	settings.set_value(&"editor.prebuilt_splash", false)
	settings.set_value(&"editor.progressive_splash", false)

	check(not editor.splash({}), "no splash with the dock closed")
	var issues: Array = editor.open(Bp.DOCK_SANDBOX, {"blueprint": bp})
	for i: Dictionary in issues:
		check(not bool(i["fatal"]), "the dock opened")
	check(await wait_job("gauges", 1200), "the worker built the design")

	# 1. Splash: saved, handed to the session, loaded and timed.
	var t0 := Time.get_ticks_msec()
	check(editor.splash({"water": 3, "target": Bp.SPLASH_TARGET_DRONES}), "splash starts")
	check(editor.in_splash(), "in_splash after splash()")
	check(not editor.splash({}), "a second splash() while one runs is refused")
	var opts: Dictionary = editor.splash_options()
	check(int(opts["water"]) == 3 and int(opts["target"]) == Bp.SPLASH_TARGET_DRONES, "options kept: " + str(opts))
	check(not bool(editor.undo_state()["dirty"]), "Space saved the design")
	check(String(editor.design_path()).begins_with("user://blueprints/"), "saved as a player file: " + String(editor.design_path()))
	check(await wait_state(Bp.SESSION_IN_SORTIE, LOAD_LIMIT), "the basin runs")
	var stats: Dictionary = editor.splash_stats()
	print("splash_test: ", bp, " cold load ", stats, " wall ", Time.get_ticks_msec() - t0, " ms")
	check(float(stats["last_ms"]) > 0.0, "the load was timed")
	check(not editor.splash({}), "no splash while a sortie runs")

	# 2. Live edits, a rewind across them (the edits are pushed again), restart.
	await frames(240)
	check(not editor.splash_rewind(-1.0), "a negative rewind is refused")
	check(not editor.splash_rewind(NAN), "a NaN rewind is refused")
	editor.set_design_name("Splash test")
	check(editor.splash_apply_edits(), "a settings edit goes in live")
	await frames(60)
	editor.select_parts(PackedInt32Array([0]), false)
	check(editor.delete_selection() > 0, "a part removed in the dock")
	check(editor.splash_apply_edits(), "a parts edit goes in live")
	await frames(90)
	check(editor.splash_rewind(3.0), "rewind 3 s, across the parts edit")
	await frames(60)
	check(editor.splash_apply_edits(), "apply after the rewind")
	check(returned.is_empty(), "no return to dock for kit or settings edits: " + str(returned))
	check(int(session.state()) == Bp.SESSION_IN_SORTIE, "the basin still runs after live edits")
	check(editor.splash_restart(), "restart")
	await frames(60)

	# 3. End: the report card.
	var n := cards.size()
	await end_basin()
	if cards.size() > n:
		var card: Dictionary = cards[n]
		print("splash_test: card ", card)
		for k: String in ["duration_s", "rounds_fired", "hit_rate", "targets_destroyed", "time_to_first_hit_s", "damage_taken", "peak_heel_deg", "score", "tip"]:
			check(card.has(k), "card has " + k)
		check(String(card["tip"]["key"]) != "", "the card has one tip")
	check(int(session.state()) == Bp.SESSION_IDLE, "back to idle")

	# 4. Pre-built splash: the worker keeps a world ready after 500 ms idle; Space swaps it in.
	settings.set_value(&"editor.prebuilt_splash", true)
	editor.save_design()
	jobs.erase("prebuild")
	check(await wait_job("prebuild", 1200), "the pre-build ran")
	await frames(5)
	check(bool(editor.splash_stats()["ready"]), "a ready world matches the design")
	t0 = Time.get_ticks_msec()
	check(editor.splash({}), "a pre-built splash")
	check(await wait_state(Bp.SESSION_IN_SORTIE, LOAD_LIMIT), "the pre-built basin runs")
	print("splash_test: pre-built load ", editor.splash_stats()["last_ms"], " ms (wall ", Time.get_ticks_msec() - t0, ")")
	await frames(30)
	await end_basin()
	settings.set_value(&"editor.prebuilt_splash", false)

	# 5. Progressive splash: ship and calm water first.
	settings.set_value(&"editor.progressive_splash", true)
	check(editor.splash({"water": 6}), "a progressive splash")
	check(await wait_state(Bp.SESSION_IN_SORTIE, LOAD_LIMIT), "the progressive basin runs")
	print("splash_test: progressive load ", editor.splash_stats()["last_ms"], " ms")
	await frames(120)
	await end_basin()
	settings.set_value(&"editor.progressive_splash", false)

	# 6. A shell change closes the basin back to the dock, with a card.
	check(editor.splash({}), "a splash for the shell edit")
	check(await wait_state(Bp.SESSION_IN_SORTIE, LOAD_LIMIT), "that basin runs")
	await frames(60)
	var beam: Dictionary = {}
	for h: Dictionary in editor.handles():
		if String(h["id"]) == "beam" and int(h["station"]) == 1:
			beam = h
	check(not beam.is_empty(), "a beam handle")
	if not beam.is_empty():
		editor.set_handle("beam", 1, float(beam["value"]) + 0.5, false)
	n = cards.size()
	check(not editor.splash_apply_edits(), "a shell edit does not go in")
	check(returned.size() == 1, "returned_to_dock fired once")
	check(await wait_card(n, 400), "the closed basin still ends with a card")
	check(not editor.in_splash(), "in_splash cleared after the shell edit")
	session.leave(false)
	await frames(10)
	stats = editor.splash_stats()
	print("splash_test: stats ", stats)
	check(float(stats["p95_ms"]) > 0.0, "p95 over the splashes")

	# 7. Launch agrees with launch_state().
	var can: bool = bool(editor.launch_state()["can_launch"])
	check(editor.launch() == can, "launch() follows launch_state().can_launch")

	# Put back what the run changed.
	editor.close()
	settings.set_value(&"editor.prebuilt_splash", pre_before)
	settings.set_value(&"editor.progressive_splash", prog_before)
	for f: String in listing("user://blueprints"):
		if not files_before.has(f):
			DirAccess.remove_absolute("user://blueprints/" + f)
	for f: String in listing("user://trials"):
		if not trials_before.has(f):
			DirAccess.remove_absolute("user://trials/" + f)
	await frames(5)
	print("splash_test: ", "ok" if failures == 0 else "%d failures" % failures)
	quit(1 if failures > 0 else 0)
