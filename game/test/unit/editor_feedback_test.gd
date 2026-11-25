extends SceneTree
## Headless scenario for the dry dock's files, control ladder, graph and feedback (units gd-editor-io,
## gd-editor-control, gd-editor-graph, gd-editor-analysis). Run from game/ with a scratch home so the
## player's own blueprint folder is never touched:
##   HOME=/tmp/bp-scratch godot --headless --path . -s res://test/unit/editor_feedback_test.gd
## Saves, renames, moves, copies, shares and imports a design (a hostile name and a long non-ASCII path
## included); edits presets, sliders, doctrine, rules, priorities, limits and the repair order; wires a
## constant into the helm through the graph and forks a stage; waits for gauges, notes and the trial
## battery and reads every feedback getter. Prints main-thread timings of the getters on the 90 m destroyer.
## Exits 1 on any failure.

var editor: Node
var failures: int = 0
var jobs: Dictionary = {}


func _initialize() -> void:
	_run.call_deferred()


func check(ok: bool, what: String) -> void:
	if not ok:
		failures += 1
		push_error("editor_feedback_test: " + what)


func _on_job(job: StringName, state: int) -> void:
	jobs[String(job)] = state


## Wait for `job` to finish, for at most `frames` frames at 60 Hz of wall time: headless runs uncapped, so
## a frame count alone times out early when the worker shares a loaded machine.
func wait_job(job: String, frames: int) -> bool:
	var deadline := Time.get_ticks_msec() + frames * 1000 / 60
	while Time.get_ticks_msec() < deadline:
		await process_frame
		if int(jobs.get(job, -1)) == Bp.JOB_DONE:
			return true
	return false


func wait_built() -> bool:
	for i in 900:
		await process_frame
		if int(jobs.get("gauges", -1)) == Bp.JOB_DONE and editor.graph_nodes(Bp.NONE).size() > 0:
			return true
	return false


func no_fatal(issues: Array, what: String) -> void:
	for i: Dictionary in issues:
		check(not bool(i["fatal"]), what + ": " + str(i["text"]))


func _run() -> void:
	await process_frame
	check(OS.get_user_data_dir().find("Application Support") >= 0 or OS.get_user_data_dir().length() > 0, "user dir")
	editor = root.get_node("/root/Editor")
	editor.job_changed.connect(_on_job)

	# Closed dock: every getter answers.
	check(editor.browse("").size() >= 0, "browse closed")
	check(editor.gauges().is_empty(), "gauges closed")
	check(editor.layers().has("legal"), "layers closed")
	check(editor.scorecard() == null, "scorecard closed")
	check(not editor.save_design(), "save closed")
	check(editor.new_design("", 30.0).size() == 1, "new_design closed is one fatal issue")

	no_fatal(editor.open(Bp.DOCK_SANDBOX, {}), "open")
	await _files()
	await _control()
	await _graph()
	_scripts()
	await _feedback()
	await _timings()
	await _lobby()

	editor.close()
	print("editor_feedback_test: %d failures" % failures)
	quit(1 if failures > 0 else 0)


func _files() -> void:
	no_fatal(editor.new_design("stock:starters/patrol15.bp.ron", 0.0), "new from starter")
	check(editor.design_path() == "", "a starter has no path")
	check(not editor.is_dirty(), "a starter opens clean")
	check(not editor.save_design(), "save without a path asks for Save as")
	check(not editor.save_design_as("../escape"), "no escape from the blueprint folder")
	check(not editor.save_design_as("res://x"), "no res:// writes")
	check(editor.save_design_as("G11/Test ship"), "save as")
	var path: String = editor.design_path()
	check(path == "user://blueprints/G11/Test ship.bp.ron", "saved path " + path)
	check(not editor.is_dirty(), "clean after save")
	check(FileAccess.file_exists(path), "file written")
	check(not FileAccess.file_exists(path + ".tmp"), "no temporary file left")
	var info: Dictionary = editor.file_info(path)
	check(String(info.get("kind", "")) == "ship" and int(info["parts"]) > 0, "file info")
	check(Array(info["lineage"]).size() >= 1, "a fork of a stock starter records its origin")
	var listing: Array = editor.browse("G11")
	check(listing.size() == 1 and String(listing[0]["name"]) == "Test ship", "browse lists the file")
	check(editor.make_folder("G11/Sub"), "make folder")
	check(editor.browse("G11")[0]["is_folder"], "folders first")
	check(editor.rename_file(path, "Renamed"), "rename")
	check(editor.design_path() == "user://blueprints/G11/Renamed.bp.ron", "the design follows its file")
	check(editor.move_file(editor.design_path(), "G11/Sub"), "move")
	check(editor.design_path() == "user://blueprints/G11/Sub/Renamed.bp.ron", "the design follows a move")
	check(not editor.move_file("G11", "G11/Sub"), "a folder never moves into itself")
	var copy: String = editor.duplicate_file(editor.design_path())
	check(copy == "user://blueprints/G11/Sub/Renamed 2.bp.ron", "duplicate " + copy)
	check(not editor.delete_file("G11/Sub"), "a folder with files is not deleted")
	check(editor.delete_file(copy), "delete moves to the trash")
	check(not FileAccess.file_exists(copy), "deleted")
	var stock_copy: String = editor.duplicate_file("stock:starters/fac22.bp.ron")
	check(stock_copy.begins_with("user://blueprints/") and FileAccess.file_exists(stock_copy), "fork a stock file")
	check(editor.browse("stock:").size() == 3, "stock tree")
	check(editor.browse("stock:starters").size() == 6, "six starters")
	check(editor.file_issues("stock:starters/destroyer90.bp.ron", "standard").size() >= 0, "file issues")

	# Share strings: a hostile name stays inside Imported/.
	editor.set_design_name("../../evil/name")
	var s: String = editor.share_string()
	check(s.begins_with("BPN1:"), "share string prefix")
	check(editor.is_share_string("  " + s), "paste detection")
	check(editor.share_string_is_long() == (s.length() > 4000), "long string flag")
	var wrapped: String = ""
	for i in range(0, s.length(), 60):
		wrapped += s.substr(i, 60) + "\n"
	var imp: Dictionary = editor.import_share_string(wrapped, "")
	check(bool(imp["ok"]) and String(imp["kind"]) == "ship", "import share string")
	var ipath: String = imp["path"]
	check(ipath.begins_with("user://blueprints/Imported/") and ipath.get_base_dir() == "user://blueprints/Imported", "imported inside Imported/: " + ipath)
	var bad: Dictionary = editor.import_share_string("BPN1:not-a-real-string", "")
	check(not bool(bad["ok"]) and Array(bad["issues"]).size() == 1, "a damaged string is refused")
	var kit: Dictionary = editor.import_share_string(editor.kit_share_string(&"kit.gun"), "")
	check(bool(kit["ok"]) and String(kit["kind"]) == "kit" and String(kit["path"]).begins_with("user://blueprints/kits/"), "kit import " + str(kit))

	# A long non-ASCII path (L1359-1360).
	var deep: String = "G11"
	for i in 3:
		deep += "/" + "Ståle 丸 ".repeat(9) + str(i)
	editor.set_design_name("Kōbe 丸")
	check(editor.save_design_as(deep + "/" + "Ship ø".repeat(12)), "save at a long non-ASCII path")
	var long_path: String = editor.design_path()
	check(ProjectSettings.globalize_path(long_path).length() > 260, "path over 260 characters")
	no_fatal(editor.load_design(long_path), "load from the long path")
	check(editor.info()["name"] == "Kōbe 丸", "non-ASCII name survives")

	# Export and import through an OS path.
	var os_path: String = OS.get_user_data_dir().path_join("export_test")
	check(editor.export_file(os_path), "export")
	var back: Dictionary = editor.import_file(os_path + ".bp.ron", "G11")
	check(bool(back["ok"]) and String(back["path"]).begins_with("user://blueprints/G11/"), "import file " + str(back))
	no_fatal(editor.load_design(ipath), "load the imported design")
	check(bool(editor.load_design("../../settings.cfg")[0]["fatal"]), "a bad path is fatal")


func _control() -> void:
	no_fatal(editor.load_design("stock:starters/fac22.bp.ron"), "load fac22")
	check(await wait_built(), "fac22 built")
	var sl: Array = editor.sliders(Bp.NONE)
	check(sl.size() == 20, "20 sliders")
	check(bool(sl[0]["basic"]), "basic sliders first")
	var applied: float = editor.set_slider("gunnery.aggression", 0.733)
	check(is_equal_approx(applied, 0.73), "slider steps")
	var found: bool = false
	for d: Dictionary in editor.sliders(Bp.OFFICER_GUNNERY):
		if String(d["key"]) == "gunnery.aggression":
			found = bool(d["overridden"])
	check(found, "hand glyph on an overridden slider")
	editor.reset_slider("gunnery.aggression")
	check(editor.set_preset(Bp.NONE, Bp.PRESET_AGGRESSIVE), "ship preset")
	check(int(editor.emission_settings()["emcon"]) == Bp.EMCON_ACTIVE, "the preset's emission setting follows")
	check(bool(editor.preset(Bp.NONE)["overridden"]), "preset overridden")
	var rows: Array = editor.doctrine(Bp.NONE)
	check(rows.size() > 0, "doctrine rows")
	var row: Dictionary = rows[0].duplicate()
	row["min_stance"] = Bp.STANCE_NEUTRAL
	check(not editor.set_doctrine(Bp.NONE, row), "never auto-engage Neutral")
	var vocab: Dictionary = editor.rule_vocabulary()
	check(Array(vocab["operands"]).size() == 12 and int(vocab["max_rules"]) == 32, "rule vocabulary")
	var speed_sink: Dictionary = {}
	var gun_sink: Dictionary = {}
	for s: Dictionary in vocab["sinks"]:
		if String(s["sink"]["kind"]) == "helm_speed":
			speed_sink = s["sink"]
		if String(s["sink"]["kind"]) == "mount_fire_permission" and gun_sink.is_empty():
			gun_sink = s["sink"]
	check(not speed_sink.is_empty() and not gun_sink.is_empty(), "sinks of this design")
	var when: Dictionary = { "op": &"compare", "lhs": { "kind": &"nearest_contact_range_m", "value": 0.0, "part": -1, "filter": { "min_stance": 2, "domain": -1, "min_grade": 0 } }, "cmp": Bp.CMP_LT, "rhs": { "kind": &"const", "value": 4000.0, "part": -1, "filter": {} } }
	var rule: Dictionary = { "enabled": true, "when": when, "sink": gun_sink, "value": { "type": &"nearest_contact", "value": { "min_stance": 2, "domain": -1, "min_grade": 0 } } }
	check(editor.add_rule(Bp.NONE, rule).is_empty(), "the L336 example rule is stored")
	var slow: Dictionary = { "enabled": true, "when": when, "sink": speed_sink, "value": { "type": &"scalar", "value": 8.0 } }
	check(editor.add_rule(0, slow).is_empty(), "a helm rule at the top")
	var wrong: Dictionary = slow.duplicate(true)
	wrong["value"] = { "type": &"bool", "value": true }
	check(editor.add_rule(Bp.NONE, wrong).size() == 1, "a value that does not fit is refused")
	var rules: Array = editor.rules()
	check(rules.size() == 2 and bool(rules[0]["valid"]) and String(rules[0]["sink"]["kind"]) == "helm_speed", "rules table")
	check(editor.move_rule(0, 1) and String(editor.rules()[1]["sink"]["kind"]) == "helm_speed", "reorder")
	check(editor.remove_rule(1), "remove")
	var pp: Array = editor.power_priorities()
	check(pp.size() > 0, "power consumers")
	var slot: int = int(pp[0]["slot"])
	var other: int = 5 if int(pp[0]["default"]) != 5 else 4
	check(editor.set_power_priority(slot, other) and bool(editor.power_priorities()[0]["overridden"]), "priority override")
	check(editor.set_power_priority(slot, int(pp[0]["default"])) and not bool(editor.power_priorities()[0]["overridden"]), "the default value is automatic")
	var gun: int = int(gun_sink["part"])
	var lim: Dictionary = editor.mount_limits(gun)
	check(bool(lim["auto"]), "limits auto-set")
	check(editor.set_mount_limits(gun, { "train_min_deg": -120.0, "train_max_deg": 120.0 }), "hand-set limits")
	check(not bool(editor.mount_limits(gun)["auto"]) and is_equal_approx(float(editor.mount_limits(gun)["train_max_deg"]), 120.0), "limits stored")
	check(editor.set_mount_limits(gun, {}) and bool(editor.mount_limits(gun)["auto"]), "back to auto")
	var order: PackedInt32Array = editor.repair_order()
	order.reverse()
	check(editor.set_repair_order(order) and editor.repair_order() == order, "repair order")
	check(not editor.set_repair_order(PackedInt32Array([0, 0, 1, 2, 3, 4])), "a repair order is a permutation")
	check(editor.set_emission_settings({ "burst_trigger_range_m": 15000.0 }) and not bool(editor.emission_settings()["burst_auto"]), "burst trigger")
	check(editor.auto_set_status().size() == 5, "auto-set values")
	var layers: Dictionary = editor.layers()
	check(int(layers["in_use"]) == Bp.LAYER_RULES and bool(layers["legal"]), "rules layer in use " + str(layers))
	check(int(layers["overrides"]) >= 4, "overrides counted")
	editor.reset_control()
	check(editor.rules().is_empty() and int(editor.layers()["overrides"]) == 0, "reset control")
	check(editor.undo() and editor.rules().size() == 1, "reset is one undo step")

	# A parts change reaches the rule builder once the worker has rebuilt the design.
	var guns_before: int = _gun_sinks()
	check(guns_before > 0, "gun sinks")
	editor.select_parts(PackedInt32Array([gun]), false)
	check(editor.delete_selection() > 0, "delete a gun")
	jobs["gauges"] = -1
	check(await wait_job("gauges", 900), "rebuilt after the delete")
	check(_gun_sinks() == guns_before - 1, "the rule builder follows the new design: %d then %d" % [guns_before, _gun_sinks()])
	check(editor.undo(), "undo the delete")


func _gun_sinks() -> int:
	var n: int = 0
	for s: Dictionary in editor.rule_vocabulary()["sinks"]:
		if String(s["sink"]["kind"]) == "mount_fire_permission":
			n += 1
	return n


func _graph() -> void:
	check(await wait_built(), "built for the graph")
	var nodes: Array = editor.graph_nodes(Bp.NONE)
	check(nodes.size() > 10, "generated graph shown")
	var speed_id: int = -1
	for n: Dictionary in nodes:
		if String(n["type"]) == "sink.helm_speed":
			speed_id = int(n["id"])
		check(n.has("inputs") and n.has("broken"), "node shape")
	check(speed_id >= 0, "helm speed sink")
	check(editor.graph_edges().size() > 0, "edges")
	var lib: Array = editor.node_library()
	check(lib.size() >= 60, "node library")
	var c: int = editor.graph_add_node("source.constant", Vector2(10, 20))
	check(c >= 0, "add a constant")
	check(editor.graph_set_param(c, "value", 12.0), "set its value")
	check(editor.graph_can_connect(c, 0, speed_id, 0), "constant to helm speed is allowed")
	check(not editor.graph_can_connect(speed_id, 0, c, 0), "a sink has no outputs")
	check(editor.graph_connect(c, 0, speed_id, 0), "connect")
	var pinned: bool = false
	for n: Dictionary in editor.graph_nodes(Bp.OFFICER_HELM):
		if int(n["id"]) == speed_id:
			pinned = bool(n["pinned"])
	check(pinned, "connecting to a generated node pins it")
	check(editor.graph_issues().is_empty(), "no graph issues: " + str(editor.graph_issues()))
	editor.graph_move_node(c, Vector2(300, 40))
	var anchor: int = editor.graph_replace_stage(Bp.STAGE_THREAT_RANKING)
	check(anchor >= 0, "fork threat ranking")
	check(editor.graph_issues().is_empty(), "a fresh fork compiles clean: " + str(editor.graph_issues()))
	check(editor.graph_restore_stage(Bp.STAGE_THREAT_RANKING), "restore")
	check(editor.save_design_as("G11/Graph ship"), "save the graph")
	no_fatal(editor.load_design("G11/Graph ship.bp.ron"), "reload")
	check(await wait_built(), "rebuilt")
	var moved: bool = false
	for n: Dictionary in editor.graph_nodes(Bp.NONE):
		if int(n["id"]) == c:
			moved = Vector2(n["pos"]).is_equal_approx(Vector2(300, 40))
	check(moved, "a dragged node keeps its place through save and load")
	check(editor.graph_remove_node(c), "remove a player node")
	check(not editor.graph_remove_node(speed_id), "generated nodes stay")
	check(editor.graph_live_values().is_empty(), "no live values in dry dock")


## L4: runs only in a build with the `luau` feature (script_add answers -1 elsewhere).
func _scripts() -> void:
	var i: int = editor.script_add("Lead")
	if i < 0:
		check(editor.scripts().is_empty(), "no scripts without Luau")
		return
	var d: Dictionary = editor.script_set_source(i, "--@in target: vec3\n--@out heading: angle\nreturn function(t) return nil end\n")
	check(String(d["error"]["key"]) == "", "a clean script: " + str(d["error"]))
	check(Array(d["inputs"]).size() == 1 and Array(d["outputs"]).size() == 1, "declared ports")
	d = editor.script_set_source(i, "return function( end")
	check(String(d["error"]["key"]) == "dock.script.error", "a compile error is reported")
	d = editor.script_set_source(i, "--@in x: banana\nreturn function() end")
	check(String(d["error"]["key"]) == "dock.script.bad_port", "a bad port type is reported")
	check(editor.script_set_enabled(i, false) and not bool(editor.scripts()[i]["enabled"]), "disable")
	var n: int = editor.graph_add_node("script:%d" % i, Vector2.ZERO)
	check(n >= 0, "a script node")
	check(editor.script_remove(i) and editor.scripts().is_empty(), "remove the script")
	var gone: bool = true
	for node: Dictionary in editor.graph_nodes(Bp.NONE):
		if int(node["id"]) == n:
			gone = false
	check(gone, "its node goes with it")
	check(editor.script_rearm(), "re-arm in dry dock")
	print("editor_feedback_test: L4 scripts checked")


func _feedback() -> void:
	check(await wait_job("notes", 900), "notes")
	var g: Dictionary = editor.gauges()
	for k in ["float", "speed", "power", "endurance", "punch", "sight", "resilience", "complexity", "stale", "rev"]:
		check(g.has(k), "gauge " + k)
	check(PackedFloat32Array(g["sight"]["coverage"]).size() == 36, "36 sectors")
	check(editor.gauge_latency().has("p95_ms"), "latency")
	var notes: Array = editor.notes()
	for n: Dictionary in notes:
		check(n.has("fix_label") and n.has("tier"), "note shape")
	var with_fix: int = -1
	for n: Dictionary in notes:
		if bool(n["has_fix"]) and with_fix < 0:
			with_fix = int(n["index"])
	if with_fix >= 0:
		check(editor.note_preview(with_fix), "Fix preview")
		editor.preview_clear()
	check(editor.suggestions().size() == 8, "eight Suggest buttons")
	for i in 20:
		await process_frame
	for sg: Dictionary in editor.suggestions():
		check(String(sg["text"]["key"]) != "dock.suggest.waiting", "every suggestion measured within 20 frames")
	var gun: int = -1
	for s: Dictionary in editor.rule_vocabulary()["sinks"]:
		if String(s["sink"]["kind"]) == "mount_fire_permission" and gun < 0:
			gun = int(s["sink"]["part"])
	check(editor.arcs(gun).size() > 0, "weapon arcs")
	var bal: Dictionary = editor.balance()
	check(float(bal["waterline_m"]) > 0.0, "waterline above the keel")
	check(editor.validation().is_empty(), "the stock starter is legal: " + str(editor.validation()))
	var ls: Dictionary = editor.launch_state()
	check(bool(ls["can_launch"]), "launch allowed")
	check(not editor.fit_check(), "no Fit Check outside a campaign")
	check(editor.refit_diff().has("affordable"), "refit diff shape")
	check(editor.quartermaster_result().has("kind"), "QM shape")
	# The battery runs on save; it takes up to a minute.
	check(editor.save_design(), "save for trials")
	var t0: int = Time.get_ticks_msec()
	check(await wait_job("trials", 60 * 120), "trial battery")
	print("editor_feedback_test: trial battery %d ms" % (Time.get_ticks_msec() - t0))
	var card = editor.scorecard()
	check(card != null and float(card["top_speed_kn"]) > 0.0, "scorecard")
	if card != null:
		check(editor.compare(card["hash"], card["hash"]).size() == 11, "compare against itself")
		check(bool(editor.gauges()["complexity"]["measured"]), "complexity measured")


func _timings() -> void:
	no_fatal(editor.load_design("stock:starters/destroyer90.bp.ron"), "load destroyer")
	check(await wait_built(), "destroyer built")
	await wait_job("notes", 900)
	var calls: Dictionary = {
		"graph_nodes": func(): return editor.graph_nodes(Bp.NONE),
		"graph_edges": func(): return editor.graph_edges(),
		"rules": func(): return editor.rules(),
		"rule_vocabulary": func(): return editor.rule_vocabulary(),
		"layers": func(): return editor.layers(),
		"validation": func(): return editor.validation(),
		"suggestions": func(): return editor.suggestions(),
		"gauges": func(): return editor.gauges(),
		"notes": func(): return editor.notes(),
		"balance": func(): return editor.balance(),
		"power_priorities": func(): return editor.power_priorities(),
		"share_string": func(): return editor.share_string(),
	}
	for name: String in calls:
		var t0: int = Time.get_ticks_usec()
		calls[name].call()
		var cold: int = Time.get_ticks_usec() - t0
		t0 = Time.get_ticks_usec()
		calls[name].call()
		var warm: int = Time.get_ticks_usec() - t0
		print("editor_feedback_test: %s cold %.1f ms, warm %.1f ms" % [name, cold / 1000.0, warm / 1000.0])
	# A slider change on the destroyer: the graph is recompiled on the main thread for the new revision.
	editor.set_slider("helm.evasion", 0.8)
	var t1: int = Time.get_ticks_usec()
	editor.graph_nodes(Bp.NONE)
	print("editor_feedback_test: graph_nodes after a tuning edit %.1f ms" % ((Time.get_ticks_usec() - t1) / 1000.0))


## Standard PvP rules allow presets, sliders and rules only (L969-971).
func _lobby() -> void:
	no_fatal(editor.open(Bp.DOCK_LOBBY, { "ruleset": "standard", "blueprint": "stock:starters/fac22.bp.ron" }), "open a standard lobby dock")
	check(int(editor.layers()["max_layer"]) == Bp.LAYER_RULES, "standard caps the ladder at rules")
	check(await wait_built(), "lobby design built")
	check(editor.graph_add_node("source.constant", Vector2.ZERO) < 0, "no graph edits under standard")
	check(editor.graph_replace_stage(Bp.STAGE_THREAT_RANKING) < 0, "no forked officers under standard")
	check(editor.script_add("x") < 0, "no scripts in a PvP lobby")
	check(editor.set_slider("helm.evasion", 0.9) > 0.0 and bool(editor.layers()["legal"]), "sliders are allowed")
