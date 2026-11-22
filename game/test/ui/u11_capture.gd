extends Node
## Windowed captures of the dry dock's feedback panels, for review against DESIGN.md "Interface and visual
## design". Units ui-editor-gauges, ui-editor-notes, ui-editor-fit, ui-editor-trials, ui-editor-splash.
## Run from the repository root with a throwaway HOME (saves, trials and campaigns write user:// files):
##   HOME=/tmp/bp-u11 godot --path game --resolution 1600x900 res://test/ui/u11_capture.tscn -- \
##       --state=<state> --out=<folder> [--bp=starters/fac22.bp.ron]
## States: strip, gauge_float, gauge_sight, gauge_punch, gauge_resilience, gauge_complexity, stale, closed,
## notes_preview, suggest, trials, trials_compare, trials_fixture, fit, fit_fixture, fit_none, options,
## splash_bar, scope, report, report_fixture; any with _hc (high contrast) or _s200 (UI scale 200%).
## Writes <folder>/u11_<state>.png, prints what it checked, and exits 1 on a failed setup or check.

const SKY := """
shader_type sky;
uniform vec3 zenith;
uniform vec3 horizon;
uniform vec3 sun_col;
void sky() {
	float h = clamp(EYEDIR.y, -1.0, 1.0);
	vec3 c = mix(horizon, zenith, pow(clamp(h, 0.0, 1.0), 0.45));
	if (h < 0.0) { c = horizon * mix(0.6, 0.25, clamp(-h * 4.0, 0.0, 1.0)); }
	float sd = dot(EYEDIR, LIGHT0_DIRECTION);
	c += sun_col * (pow(max(sd, 0.0), 6.0) * 0.6 + smoothstep(0.9995, 0.9998, sd) * 30.0);
	COLOR = c;
}
"""

var _state := "strip"
var _base := "strip"
var _out := "user://captures"
var _bp := "starters/fac22.bp.ron"
var _dock: Control
var _failures := 0


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--state="):
			_state = arg.trim_prefix("--state=")
		elif arg.begins_with("--out="):
			_out = arg.trim_prefix("--out=")
		elif arg.begins_with("--bp="):
			_bp = arg.trim_prefix("--bp=")
	_base = _state.replace("_hc", "").replace("_s200", "")
	DirAccess.make_dir_recursive_absolute(_out)
	var guard := get_tree().create_timer(240.0)
	guard.timeout.connect(func() -> void:
		push_error("u11_capture: timed out in state " + _state)
		get_tree().quit(1))
	_run()


func _frames(n: int) -> void:
	for i in n:
		await get_tree().process_frame


## Wait up to `max_frames` for `cond` to hold.
func _until(cond: Callable, max_frames: int) -> bool:
	for i in max_frames:
		if bool(cond.call()):
			return true
		await get_tree().process_frame
	return bool(cond.call())


func _check(ok: bool, what: String) -> void:
	if not ok:
		_failures += 1
		push_error("u11_capture: FAILED " + what)
	else:
		print("u11_capture: ok ", what)


func _run() -> void:
	await _frames(2)
	if _state.contains("_hc"):
		Settings.set_value(&"access.palette", 1)
		Ui.call(&"_apply_palette", &"high_contrast")
	if _state.contains("_s200"):
		get_tree().root.content_scale_factor = 2.0
	if _base == "fit":
		if not await _open_campaign_dock():
			get_tree().quit(1)
			return
	elif _base == "closed":
		Editor.close()
		Screens.change(&"dry_dock")
	else:
		var issues: Array = Screens.open_dry_dock(Bp.DOCK_SANDBOX, {"blueprint": _bp})
		for i: Dictionary in issues:
			if bool(i.get("fatal", false)):
				push_error("u11_capture: open refused " + str(issues))
				get_tree().quit(1)
				return
	_dock = Screens.top() as Control
	if _dock == null:
		push_error("u11_capture: no dock screen")
		get_tree().quit(1)
		return
	_light(_dock.get_node(^"Scroll/Frame/Body/Centre/Stage/Viewport") as SubViewport)
	if _base != "closed":
		await _until(func() -> bool: return not Kit.as_dict(Editor.gauges()).is_empty(), 600)
		await _frames(30)
	await _apply_state()
	await _frames(20)
	if _base == "stale":
		# The live strip refreshes on its own; draw the hand-built stale strip again just before the shot.
		_stale_strip()
		await _frames(2)
	var path := _out.path_join("u11_%s.png" % _state)
	get_viewport().get_texture().get_image().save_png(path)
	print("u11_capture: saved ", path)
	if Editor.in_splash():
		Editor.splash_end()
		await _frames(30)
	Editor.close()
	get_tree().quit(1 if _failures > 0 else 0)


const Kit := preload("res://ui/screens/menu_kit.gd")


func _node(path: String) -> Node:
	return _dock.get_node_or_null(NodePath(path))


func _strip() -> Node:
	return _node("Scroll/Frame/Gauges")


func _right_tabs() -> TabContainer:
	return _node("Scroll/Frame/Body/RightDock/Tabs") as TabContainer


func _apply_state() -> void:
	match _base:
		"strip", "closed":
			print("u11_capture: gauges ", Editor.gauges().keys(), " latency ", Editor.gauge_latency())
			if _base == "strip":
				# Main-thread cost of the panels' redraws (budget 2 ms a frame, L1449).
				for target: Array in [[_strip(), &"_refresh"], [_node("Scroll/Frame/Body/RightDock/Tabs/Notes"), &"_refresh"], [_node("Scroll/Frame/Body/RightDock/Tabs/Trials"), &"_refresh"], [_node("Scroll/Frame/Body/RightDock/Tabs/Fit"), &"_refresh"]]:
					var t0 := Time.get_ticks_usec()
					for i in 10:
						(target[0] as Node).call(target[1])
					print("u11_capture: %s.%s %.2f ms" % [(target[0] as Node).name, target[1], (Time.get_ticks_usec() - t0) / 10000.0])
		"gauge_float", "gauge_sight", "gauge_punch", "gauge_resilience", "gauge_complexity":
			_strip().call(&"_open_detail", StringName(_base.trim_prefix("gauge_")))
		"stale":
			_stale_strip()
		"notes_preview":
			var notes: Array = Editor.notes()
			print("u11_capture: notes ", notes.size())
			var panel := _node("Scroll/Frame/Body/RightDock/Tabs/Notes")
			var fixed := false
			for n: Dictionary in notes:
				if bool(n["has_fix"]):
					panel.call(&"_preview_note", int(n["index"]))
					fixed = true
					break
			if not fixed:
				print("u11_capture: no note with a Fix on this design")
		"suggest":
			var panel := _node("Scroll/Frame/Body/RightDock/Tabs/Notes")
			(panel.get(&"_suggest_toggle") as Button).button_pressed = true
			await _until(func() -> bool:
				for s: Dictionary in Editor.suggestions():
					if (s["text"] as Dictionary).get("key", &"") == &"dock.suggest.waiting":
						return false
				return true, 300)
			await _frames(5)
			var kinds := []
			for s: Dictionary in Editor.suggestions():
				if bool(s["available"]):
					kinds.append(int(s["kind"]))
			print("u11_capture: suggestions available ", kinds)
			if not kinds.is_empty():
				panel.call(&"_preview_suggestion", kinds[0])
		"trials", "trials_compare":
			await _save_and_trial()
			if _base == "trials_compare":
				var h: Array = Editor.handles()
				Editor.set_handle(h[0]["id"], int(h[0]["station"]), float(h[0]["value"]) + 0.3, false)
				await _frames(5)
				await _save_and_trial()
				print("u11_capture: history ", Editor.scorecard_history().size(), " path ", Editor.design_path(), " info hash ", Editor.info().get("hash"))
			_right_tabs().current_tab = 1
		"trials_fixture":
			_right_tabs().current_tab = 1
			await _frames(2)
			var panel := _node("Scroll/Frame/Body/RightDock/Tabs/Trials")
			panel.set(&"_open", true)
			panel.set(&"_saved", true)
			panel.set(&"_caution_at", 400.0)
			panel.set(&"_card", _fixture_card(true))
			var older := _fixture_card(false)
			older["hash"] = "00ff"
			older["label"] = "Fast attack craft"
			older["saved_unix"] = 1_794_000_000
			var hist: Array[Dictionary] = [older]
			panel.set(&"_history", hist)
			panel.call(&"render")
			var rows: Array[Dictionary] = [
				{"metric": &"dock.metric.turning_circle_m", "before": 310.0, "after": 284.0, "unit": &"m", "better": 1},
				{"metric": &"dock.metric.top_speed_kn", "before": 34.2, "after": 33.1, "unit": &"kn", "better": -1},
				{"metric": &"dock.metric.fight_score", "before": 38.0, "after": 46.0, "unit": &"none", "better": 1},
				{"metric": &"dock.metric.fight_mean_hull_fraction_left", "before": 0.61, "after": 0.61, "unit": &"pct", "better": 0},
				{"metric": &"dock.metric.complexity_points", "before": 388.0, "after": 431.0, "unit": &"none", "better": -1},
			]
			panel.call(&"render_compare_rows", rows)
			await _frames(3)
			for sc: Node in panel.find_children("*", "ScrollContainer", true, false):
				(sc as ScrollContainer).scroll_vertical = 100000
		"fit":
			_right_tabs().current_tab = 2
			await _frames(5)
			await _until(func() -> bool: return int(Editor.fit_result()["state"]) == Bp.JOB_DONE, 3000)
			print("u11_capture: fit ", Editor.fit_result())
			Editor.quartermaster()
			await _until(func() -> bool: return int(Editor.quartermaster_result()["state"]) == Bp.JOB_DONE, 1200)
			print("u11_capture: quartermaster ", Editor.quartermaster_result())
			print("u11_capture: refit ", Editor.refit_diff())
		"fit_fixture", "fit_none":
			_right_tabs().set_tab_hidden(2, false)
			_right_tabs().current_tab = 2
			await _frames(2)
			var panel := _node("Scroll/Frame/Body/RightDock/Tabs/Fit")
			panel.set(&"_campaign", true)
			panel.set(&"_budget", 120)
			panel.set(&"_fit", {
				"state": Bp.JOB_DONE, "passes": false, "top_speed_kn": 31.4, "time_to_turn_180_s": 41.0, "range_km": 610.0, "simulated": true,
				"tags": [
					{"text": {"key": &"dock.tag.draft", "args": ["2.5"]}, "pass": false, "value": 2.8, "limit": 2.5, "unit": &"m"},
					{"text": {"key": &"dock.tag.range", "args": [{"num": 500.0, "unit": &"km"}]}, "pass": true, "value": 610.0, "limit": 500.0, "unit": &"km"},
				],
				"detect": [{"class_id": &"combine.patrol", "we_detect_km": 24.0, "they_detect_km": 17.0}],
			})
			panel.set(&"_qm_result", {"state": Bp.JOB_DONE, "kind": Bp.QM_REFIT, "fit": {"passes": true}, "blocking": {},
				"diff": {"changes": [
					{"kind": &"removed", "part": &"part.prop.shaft", "name": &"Shaft and propeller", "cell": Vector3i(2, 0, 6), "from_cell": null},
					{"kind": &"added", "part": &"part.prop.waterjet", "name": &"Waterjet drive", "cell": Vector3i(2, 0, 6), "from_cell": null},
					{"kind": &"moved", "part": &"part.fuel.tank", "name": &"Fuel tank", "cell": Vector3i(4, 1, 9), "from_cell": Vector3i(4, 0, 9)},
				], "structure_delta_t": 0.0, "salvage": 140, "hours": 2, "from_store": {&"part.prop.waterjet": 1}, "affordable": false}})
			panel.set(&"_diff", {"changes": [{"kind": &"added", "part": &"part.gun.20", "name": &"20 mm gun", "cell": Vector3i(3, 3, 14), "from_cell": null}],
				"structure_delta_t": 1.2, "salvage": 60, "hours": 2, "from_store": {}, "affordable": true})
			if _base == "fit_none":
				panel.set(&"_qm_result", {"state": Bp.JOB_DONE, "kind": Bp.QM_NO_LEGAL_REFIT, "diff": null, "fit": null,
					"blocking": {"key": &"dock.tag.draft", "args": ["2.5"]}})
			panel.call(&"render", true)
			await _frames(3)
			for sc: Node in panel.find_children("*", "ScrollContainer", true, false):
				(sc as ScrollContainer).scroll_vertical = 100000
		"options":
			var pp0 := _node("SplashOptions") as PopupPanel
			print("u11_capture: options before: size ", pp0.size, " content min ", pp0.get_contents_minimum_size())
			for l: Node in pp0.find_children("*", "Label", true, false):
				print("u11_capture:   label ", (l as Label).text.left(20), " wrap ", (l as Label).autowrap_mode, " size ", (l as Label).size, " min ", (l as Label).get_combined_minimum_size())
			pp0.reset_size()
			print("u11_capture: options after reset: size ", pp0.size)
			_dock.call(&"_open_splash_options")
			await _frames(3)
			var pp := _node("SplashOptions") as PopupPanel
			print("u11_capture: options popup at ", pp.position, " size ", pp.size, " content min ", pp.get_contents_minimum_size(), " child min ", (_node("SplashOptions/Options") as Control).get_combined_minimum_size(), " more button ", (_node("Scroll/Frame/TopBar/Margin/Row/SplashMore") as Control).get_global_rect())
		"splash_bar", "scope", "report", "again":
			_dock.call(&"_do_splash")
			var ok := await _until(func() -> bool: return Session.state() == Bp.SESSION_IN_SORTIE, 1800)
			_check(ok, "splash reached the sortie (state %d)" % Session.state())
			await _frames(240)
			if _base == "scope":
				var bar := _node("SplashUi/Bar")
				(bar.get(&"_scope") as MenuButton).show_popup()
				await _frames(5)
				var ports: Dictionary = bar.get(&"_menu_ports")
				print("u11_capture: scope ports ", ports.size())
				if not ports.is_empty():
					bar.call(&"_on_scope_picked", ports.keys()[0])
				await _frames(120)
			if _base == "report" or _base == "again":
				Editor.splash_end()
				var ended := await _until(func() -> bool: return (_node("ReportLayer/Report") as Control).visible, 900)
				_check(ended, "report card shown")
				await _frames(30)
			if _base == "again":
				# Esc through the real input path closes the card; Splash again starts the next basin.
				_key(KEY_ESCAPE)
				await _frames(3)
				_check(not (_node("ReportLayer/Report") as Control).visible, "Esc closed the report")
				_node("ReportLayer/Report").call(&"show_card", {"duration_s": 4.0, "tip": {}})
				await _frames(3)
				_node("ReportLayer/Report").call(&"_on_again")
				var again := await _until(func() -> bool: return Editor.in_splash() and Session.state() == Bp.SESSION_IN_SORTIE, 1800)
				_check(again, "Splash again started a new basin")
				_check(not (_node("ReportLayer/Report") as Control).visible, "the card hid for the new splash")
				await _frames(60)
		"report_fixture":
			_node("ReportLayer/Report").call(&"show_card", {"duration_s": 94.0, "rounds_fired": 62, "hit_rate": 0.27, "targets_destroyed": 2,
				"time_to_first_hit_s": 11.5, "damage_taken": 0.08, "peak_heel_deg": 14.0, "score": 46.0,
				"tip": {"key": &"splash.tip.masked", "args": ["Gun 2"]}})


func _key(code: Key) -> void:
	for pressed: bool in [true, false]:
		var e := InputEventKey.new()
		e.keycode = code
		e.physical_keycode = code
		e.pressed = pressed
		Input.parse_input_event(e)


func _stale_strip() -> void:
	var g: Dictionary = Editor.gauges().duplicate(true)
	g["stale"] = PackedStringArray(["endurance", "sight", "resilience", "complexity"])
	var fl: Dictionary = g["float"]
	fl["check"] = Bp.CHECK_CAUTION
	fl["gm_m"] = 0.22
	var pw: Dictionary = g["power"]
	pw["check"] = Bp.CHECK_FAIL
	pw["combat_demand_kw"] = 380.0
	_strip().call(&"show_strip", g, {"split_strip": true, "p95_ms": 132.0}, true)


func _save_and_trial() -> void:
	var before := str(Editor.scorecard())
	if String(Editor.design_path()).is_empty() or String(Editor.design_path()).begins_with("res://"):
		_check(Editor.save_design_as("U11 capture"), "save as")
	else:
		_check(Editor.save_design(), "save")
	var ok := await _until(func() -> bool: return Editor.scorecard() != null and str(Editor.scorecard()) != before, 5400)
	_check(ok, "a scorecard arrived")
	print("u11_capture: scorecard ", Editor.scorecard())


func _fixture_card(newer: bool) -> Dictionary:
	return {"hash": "abcd" if newer else "00ff", "turning_circle_m": 284.0 if newer else 310.0, "time_to_90pct_speed_s": 22.5, "top_speed_kn": 33.1,
		"heel_recovery_s": 6.2, "peak_heel_deg": 18.0, "score": 46.0 if newer else 38.0, "per_seed": PackedFloat32Array([55.0, 46.0, 37.0]),
		"wins": 2, "hull_left": 0.61, "rounds_per_kill": 88.0, "first_salvo_hit_rate": 0.33, "complexity_points": 431.0 if newer else 388.0,
		"short_trial": newer, "opponents": [&"combine.patrol"], "saved_unix": 1_794_100_000, "label": "Fast attack craft",
		"drift": [{"gauge": "speed", "predicted": 33.1, "simulated": 29.4}] if newer else []}


## A campaign at its first cove with the first offer briefed, and the dry dock opened from it.
func _open_campaign_dock() -> bool:
	var campaign: Node = get_node(^"/root/Campaign")
	var problems: Array = campaign.call(&"new_campaign", {
		"slot_name": "U11 capture", "difficulty": {"preset": &"normal", "ai_accuracy": 1.0, "ai_reaction_s": 2.0, "adaptation_cadence": 1.0, "hull_count": 300, "economy": 1.0, "clock_days": 40},
		"ironman": false, "seed": 4242, "starter": "starters/fac22.bp.ron", "tutorial_done": true,
	})
	if not problems.is_empty():
		push_error("u11_capture: new_campaign refused " + str(problems))
		return false
	await _frames(2)
	var offers: Array = Kit.as_dict(campaign.call(&"chart")).get("offers", [])
	if offers.is_empty():
		push_error("u11_capture: no offer")
		return false
	var op: StringName = offers[0].get("id", &"")
	if not bool(campaign.call(&"open_briefing", op)):
		push_error("u11_capture: open_briefing refused")
		return false
	if not bool(campaign.call(&"open_dry_dock")):
		push_error("u11_capture: open_dry_dock refused")
		return false
	var issues: Array = Screens.open_dry_dock(Bp.DOCK_CAMPAIGN, {})
	for i: Dictionary in issues:
		if bool(i.get("fatal", false)):
			push_error("u11_capture: campaign dock refused " + str(issues))
			return false
	print("u11_capture: campaign dock open, mode ", Editor.dock_mode(), " limits ", Editor.limits())
	return true


func _light(vp: SubViewport) -> void:
	if vp == null:
		return
	var tokens: Dictionary = JSON.parse_string(FileAccess.get_file_as_string("res://art/tokens/world_tokens.json"))
	var dusk := {}
	for e: Dictionary in tokens["sky"]:
		if e["time"] == "dusk":
			dusk = e
	var lin := func(entry: Dictionary) -> Color:
		var v: Array = entry["linear_rgb"]
		return Color(float(v[0]), float(v[1]), float(v[2]))
	var sun := DirectionalLight3D.new()
	sun.light_color = (lin.call(dusk["key_light"]) as Color).linear_to_srgb()
	sun.light_energy = 2.0
	sun.shadow_enabled = true
	sun.directional_shadow_max_distance = 300.0
	sun.rotation_degrees = Vector3(-14.0, 125.0, 0.0)
	vp.add_child(sun)
	var shader := Shader.new()
	shader.code = SKY
	var mat := ShaderMaterial.new()
	mat.shader = shader
	for k: String in ["zenith", "horizon"]:
		var c: Color = lin.call(dusk[k])
		mat.set_shader_parameter(k, Vector3(c.r, c.g, c.b))
	var kc: Color = lin.call(dusk["key_light"])
	mat.set_shader_parameter("sun_col", Vector3(kc.r, kc.g, kc.b))
	var sky := Sky.new()
	sky.sky_material = mat
	var env := Environment.new()
	env.background_mode = Environment.BG_SKY
	env.sky = sky
	env.ambient_light_source = Environment.AMBIENT_SOURCE_SKY
	env.tonemap_mode = Environment.TONE_MAPPER_AGX
	var we := WorldEnvironment.new()
	we.environment = env
	vp.add_child(we)
