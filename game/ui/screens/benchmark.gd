extends Control
## Benchmark: stage progress, results, copy JSON, open issue form.
## DESIGN.md L1392-1401. Unit: ui-benchmark.
## Reads and calls: Bench.start, Bench.stage_changed, Bench.frame_sample, Bench.report, Bench.save_report.
##
## A base screen from Main menu > Benchmark, or from `--benchmark`, which opens it with
## `{ "start": true }` and starts at once. Three states:
## - Ready: what the run does, in order (ordered content, so the stages are numbered), how long it
##   takes, that nothing is sent anywhere (L1322), the optional 20-minute soak, and Start.
## - Running: battle stages are ordinary sessions that `Screens` leaves to this screen (L1393), so
##   the screen builds the sortie world itself, under a strip with the stage, its progress and time
##   left, and Cancel (Esc). The camera runs a fixed orbit driven by stage time, the same path every
##   run. Every frame's CPU time, and on D3D12 and Vulkan the viewport's measured GPU time, go to
##   `Bench.frame_sample` (Metal passes -1, L2022). The dry-dock and calibration stages run off the
##   main thread with no world; the strip then sits over the plain screen.
## - Results: one table row per stage (tier, frame p50, p95 and p99, GPU p95, simulation tick p95
##   and p99, adapter p99, missed vblanks), the machine and build facts, the soak verdict, and Copy
##   report (the JSON the issue form takes), Save report, Open the issue form in the browser, and
##   Run again. On Windows, Run under Vulkan relaunches the game with `--rendering-driver vulkan`
##   for the driver A/B of L2045.
## `setup` also takes `duration_scale: float` for development runs (Bench ignores it in release).
## `show_stage(stage)` and `show_report(report)` render plain data (`BenchStageDict`,
## `BenchReportDict`) for tests and captures.

const Kit := preload("res://ui/screens/menu_kit.gd")

const WORLD_SCENE := "res://world/world.tscn"
## `Bp.BENCH_*`, `Bp.SESSION_*`, `Bp.KIND_BENCHMARK`, `Bp.QUALITY_*`.
const BENCH_IDLE := 0
const BENCH_REFERENCE := 1
const BENCH_COOP := 2
const BENCH_SWARM := 3
const BENCH_EDITOR := 4
const BENCH_CALIB := 5
const BENCH_SOAK := 6
const BENCH_DONE := 7
const SESSION_IDLE := 0
const SESSION_LOADING := 1
const SESSION_IN_SORTIE := 3
const SESSION_FAILED := 6
const KIND_BENCHMARK := 8
const STAGE_WORDS: Array[StringName] = [&"", &"benchmark.stage.reference", &"benchmark.stage.coop", &"benchmark.stage.swarm", &"benchmark.stage.editor", &"benchmark.stage.calib", &"benchmark.stage.soak"]
const TIER_WORDS: Array[StringName] = [&"units.word.quality.low", &"units.word.quality.medium", &"units.word.quality.high", &"units.word.quality.fallback"]
## Fixed camera path: one orbit of the battle every 60 s at the default pitch.
const ORBIT_DEG_PER_S := 6.0
## A frame later than this share of the display's frame interval missed its vblank.
const MISSED_VBLANK_FACTOR := 1.5
## Minutes the default plan takes (L1393-1399): 3 + 3 + 3 (stage 1 at the tier, Low and Medium),
## 2, 2, plus about 2 for the dry-dock and calibration stages.
const PLAN_MIN := 15
const SOAK_MIN := 20
const COLS := 8

var _back: Button
var _title: Label
var _ready_box: VBoxContainer
var _soak: CheckBox
var _start: Button
var _estimate: Label
var _run_box: VBoxContainer
var _strip: PanelContainer
var _stage_text: Label
var _progress: ProgressBar
var _left_text: Label
var _cancel: Button
var _results_box: VBoxContainer
var _table: Tree
var _facts: Label
var _verdict: Label
var _copy: Button
var _save: Button
var _issue: Button
var _again: Button
var _vulkan: Button
var _note: Label
var _run_page: VBoxContainer
var _run_list: Label
## Stage lines seen in this run, oldest first; all but the last are done.
var _run_lines: PackedStringArray = []
var _backing: Panel
var _page: MarginContainer

var _state: StringName = &"ready"
var _world: Node
var _report: Dictionary = {}
var _stage: Dictionary = {}
var _bound := false
var _stage_start_ms := 0
var _orbit_applied := 0.0
var _gpu_rid := RID()
var _measure_gpu := false
var _refresh_hz := 60.0
## Development runs only: shortens every stage (`Bench` ignores it in release builds).
var _duration_scale := 0.0


func _ready() -> void:
	accessibility_name = tr(&"benchmark.title")
	_backing = Panel.new()
	_backing.set_anchors_preset(Control.PRESET_FULL_RECT)
	add_child(_backing)
	_page = MarginContainer.new()
	_page.set_anchors_preset(Control.PRESET_FULL_RECT)
	Ui.set_margins(_page, 5)
	add_child(_page)
	var scroll := ScrollContainer.new()
	scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	scroll.follow_focus = true
	_page.add_child(scroll)
	var column := Kit.vbox(3)
	column.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	column.size_flags_vertical = Control.SIZE_EXPAND_FILL
	scroll.add_child(column)

	var header := Kit.hbox(4)
	_title = Kit.label("benchmark.title", &"ScreenTitle")
	_title.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	header.add_child(_title)
	_back = Kit.inline_button("benchmark.back")
	_back.custom_minimum_size = Vector2(200.0, Kit.BUTTON_H)
	_back.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	_back.pressed.connect(_leave)
	header.add_child(_back)
	column.add_child(header)
	column.add_child(HSeparator.new())

	_ready_box = Kit.vbox(3)
	column.add_child(_ready_box)
	var intro := Kit.label("benchmark.intro", &"Body")
	intro.custom_minimum_size = Vector2(880.0, 0.0)
	intro.size_flags_horizontal = Control.SIZE_SHRINK_BEGIN
	_ready_box.add_child(intro)
	_ready_box.add_child(Kit.label("benchmark.plan_title", &"PanelTitle"))
	var plan := Kit.label("", &"Body", true)
	plan.text = plan_text()
	_ready_box.add_child(plan)
	_soak = CheckBox.new()
	_soak.text = "benchmark.soak"
	_soak.focus_mode = Control.FOCUS_ALL
	_soak.custom_minimum_size = Vector2(0.0, Kit.BUTTON_H)
	_soak.toggled.connect(func(_on: bool) -> void: _show_estimate())
	_ready_box.add_child(_soak)
	_estimate = Kit.label("", &"Body", true)
	_ready_box.add_child(_estimate)
	var privacy := Kit.label("benchmark.privacy", &"Body")
	privacy.custom_minimum_size = Vector2(880.0, 0.0)
	privacy.size_flags_horizontal = Control.SIZE_SHRINK_BEGIN
	_ready_box.add_child(privacy)
	_start = Kit.button("benchmark.start", true)
	_start.size_flags_horizontal = Control.SIZE_SHRINK_BEGIN
	_start.custom_minimum_size = Vector2(360.0, Kit.PRIMARY_H)
	_start.pressed.connect(_on_start)
	_ready_box.add_child(_start)

	_run_page = Kit.vbox(2)
	_run_page.add_child(Kit.label("benchmark.run_title", &"PanelTitle"))
	_run_list = Kit.label("", &"Body", true)
	_run_page.add_child(_run_list)
	_run_page.add_child(Kit.label("benchmark.run_help", &"Body"))
	column.add_child(_run_page)

	_results_box = Kit.vbox(3)
	_results_box.size_flags_vertical = Control.SIZE_EXPAND_FILL
	column.add_child(_results_box)
	_verdict = Kit.label("", &"Body", true)
	_results_box.add_child(_verdict)
	_table = Tree.new()
	_table.columns = COLS
	_table.column_titles_visible = true
	_table.hide_root = true
	_table.select_mode = Tree.SELECT_ROW
	_table.custom_minimum_size = Vector2(0.0, 300.0)
	_table.focus_mode = Control.FOCUS_ALL
	_table.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_table.accessibility_name = tr(&"benchmark.results")
	_table.set_column_expand(0, true)
	_table.set_column_clip_content(0, true)
	for c in range(1, COLS):
		_table.set_column_expand(c, false)
		_table.set_column_custom_minimum_width(c, 140)
	_table.set_column_custom_minimum_width(0, 240)
	_table.set_column_custom_minimum_width(2, 240)
	_table.set_column_custom_minimum_width(4, 190)
	_results_box.add_child(_table)
	var actions := Kit.hbox(3)
	_copy = Kit.inline_button("benchmark.copy")
	_copy.icon = Ui.icon(&"copy")
	_copy.custom_minimum_size = Vector2(0.0, Kit.PRIMARY_H)
	_copy.pressed.connect(_on_copy)
	actions.add_child(_copy)
	_issue = Kit.inline_button("benchmark.issue")
	_issue.icon = Ui.icon(&"link")
	_issue.custom_minimum_size = Vector2(0.0, Kit.PRIMARY_H)
	_issue.pressed.connect(_on_issue)
	actions.add_child(_issue)
	_save = Kit.inline_button("benchmark.save")
	_save.icon = Ui.icon(&"save")
	_save.custom_minimum_size = Vector2(0.0, Kit.PRIMARY_H)
	_save.pressed.connect(_on_save)
	actions.add_child(_save)
	_again = Kit.inline_button("benchmark.again")
	_again.custom_minimum_size = Vector2(0.0, Kit.PRIMARY_H)
	_again.pressed.connect(_on_again)
	actions.add_child(_again)
	_vulkan = Kit.inline_button("benchmark.vulkan")
	_vulkan.custom_minimum_size = Vector2(0.0, Kit.PRIMARY_H)
	_vulkan.visible = OS.get_name() == "Windows"
	_vulkan.pressed.connect(_on_vulkan)
	actions.add_child(_vulkan)
	_results_box.add_child(actions)
	_note = Kit.label("", &"Body", true)
	_note.visible = false
	_results_box.add_child(_note)
	_results_box.add_child(Kit.label("benchmark.machine", &"PanelTitle"))
	_facts = Kit.label("", &"Body", true)
	_results_box.add_child(_facts)

	var strip_frame := VBoxContainer.new()
	strip_frame.set_anchors_preset(Control.PRESET_FULL_RECT)
	strip_frame.mouse_filter = Control.MOUSE_FILTER_IGNORE
	add_child(strip_frame)
	var strip_space := Control.new()
	strip_space.size_flags_vertical = Control.SIZE_EXPAND_FILL
	strip_space.mouse_filter = Control.MOUSE_FILTER_IGNORE
	strip_frame.add_child(strip_space)
	_strip = PanelContainer.new()
	strip_frame.add_child(_strip)
	var sm := MarginContainer.new()
	Ui.set_margins(sm, 3)
	_strip.add_child(sm)
	_run_box = Kit.vbox(1)
	sm.add_child(_run_box)
	var srow := Kit.hbox(4)
	_stage_text = Kit.label("", &"PanelTitle", true)
	_stage_text.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_stage_text.autowrap_mode = TextServer.AUTOWRAP_OFF
	srow.add_child(_stage_text)
	_left_text = Kit.label("", &"Body", true)
	_left_text.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	_left_text.autowrap_mode = TextServer.AUTOWRAP_OFF
	srow.add_child(_left_text)
	_cancel = Kit.inline_button("benchmark.cancel")
	_cancel.custom_minimum_size = Vector2(200.0, Kit.BUTTON_H)
	_cancel.pressed.connect(_on_cancel)
	srow.add_child(_cancel)
	_run_box.add_child(srow)
	_progress = ProgressBar.new()
	_progress.min_value = 0.0
	_progress.max_value = 1.0
	_progress.step = 0.001
	_progress.show_percentage = false
	_progress.custom_minimum_size = Vector2(0.0, 12.0)
	_progress.accessibility_name = tr(&"benchmark.progress")
	_run_box.add_child(_progress)

	_apply_columns()
	_show_estimate()
	_show_state(&"ready")
	set_process(false)


func _apply_columns() -> void:
	var titles: Array[StringName] = [&"benchmark.col.stage", &"benchmark.col.tier", &"benchmark.col.frame", &"benchmark.col.gpu",
		&"benchmark.col.tick", &"benchmark.col.adapter", &"benchmark.col.missed", &"benchmark.col.frames"]
	for c in titles.size():
		_table.set_column_title(c, tr(titles[c]))
		_table.set_column_title_alignment(c, HORIZONTAL_ALIGNMENT_LEFT)


static func plan_text() -> String:
	var lines := PackedStringArray()
	for i in 6:
		lines.append(Kit.tr_key(&"benchmark.plan_line").format([i + 1, Kit.tr_key(StringName("benchmark.plan.%d" % (i + 1)))]))
	return "\n".join(lines)


func _show_estimate() -> void:
	var minutes := PLAN_MIN + (SOAK_MIN if _soak.button_pressed else 0)
	_estimate.text = tr(&"benchmark.estimate").format([minutes])


# --- facade -------------------------------------------------------------------------------------

func setup(args: Dictionary) -> void:
	_bind()
	_duration_scale = float(args.get("duration_scale", 0.0))
	var last := Kit.as_dict(Kit.call_on(&"Bench", &"report"))
	var stage := Kit.as_dict(Kit.call_on(&"Bench", &"stage"))
	var running := Kit.get_int(stage, "stage", BENCH_IDLE)
	if running != BENCH_IDLE and running != BENCH_DONE:
		show_stage(stage)
	elif bool(args.get("start", false)):
		_on_start()
	elif not last.is_empty():
		show_report(last)


func _bind() -> void:
	if _bound:
		return
	_bound = true
	var bench := Kit.autoload(&"Bench")
	if bench != null:
		for pair: Array in [[&"stage_changed", show_stage], [&"finished", _on_finished]]:
			if bench.has_signal(pair[0]) and not bench.is_connected(pair[0], pair[1]):
				bench.connect(pair[0], pair[1])
	var session := Kit.autoload(&"Session")
	if session != null and session.has_signal(&"state_changed") and not session.is_connected(&"state_changed", _on_session_state):
		session.connect(&"state_changed", _on_session_state)


func _exit_tree() -> void:
	_free_world()
	if _measure_gpu and _gpu_rid.is_valid():
		RenderingServer.viewport_set_measure_render_time(_gpu_rid, false)


func default_focus() -> Control:
	match _state:
		&"running":
			return _cancel
		&"results":
			return _copy
	return _start


func _unhandled_input(event: InputEvent) -> void:
	if event.is_action_pressed(&"ui_cancel", false, true) and Screens.top() == self:
		get_viewport().set_input_as_handled()
		if _state == &"running":
			_on_cancel()
		else:
			_leave()


func _show_state(state: StringName) -> void:
	_state = state
	_ready_box.visible = state == &"ready"
	_results_box.visible = state == &"results"
	_run_page.visible = state == &"running"
	_strip.visible = state == &"running"
	_back.disabled = state == &"running"
	_title.text = tr(&"benchmark.title_results") if state == &"results" else tr(&"benchmark.title")
	_link_focus()


# --- running ------------------------------------------------------------------------------------

func _on_start() -> void:
	if _state == &"running":
		return
	_bind()
	var options := {"soak": _soak.button_pressed}
	if _duration_scale > 0.0:
		options["duration_scale"] = _duration_scale
	var ok: Variant = Kit.call_on(&"Bench", &"start", [options])
	if ok != true:
		Screens.open_message(&"benchmark.busy_title", &"benchmark.busy_body")
		return
	_run_lines = PackedStringArray()
	_show_state(&"running")
	_begin_frame_feed()
	show_stage(Kit.as_dict(Kit.call_on(&"Bench", &"stage")))


func _begin_frame_feed() -> void:
	_gpu_rid = get_viewport().get_viewport_rid()
	var driver := RenderingServer.get_current_rendering_driver_name()
	# Metal exposes no viewport GPU time; frame time and missed vblanks stand in (L2022).
	_measure_gpu = driver == "d3d12" or driver == "vulkan"
	if _measure_gpu:
		RenderingServer.viewport_set_measure_render_time(_gpu_rid, true)
	var hz := DisplayServer.screen_get_refresh_rate()
	_refresh_hz = hz if hz > 1.0 else 60.0
	set_process(true)


## Show a `BenchStageDict`.
func show_stage(stage: Dictionary) -> void:
	var previous := Kit.get_int(_stage, "stage", -1)
	_stage = stage
	var s := Kit.get_int(stage, "stage", BENCH_IDLE)
	if s == BENCH_IDLE or s == BENCH_DONE:
		if _state == &"running" and s == BENCH_IDLE:
			_end_run()
			_show_state(&"ready")
		return
	if _state != &"running":
		_show_state(&"running")
	if s != previous:
		_stage_start_ms = Time.get_ticks_msec()
		_orbit_applied = 0.0
	var text := Kit._Text.resolve(stage.get("text", {}))
	_stage_text.text = text if not text.is_empty() else tr(STAGE_WORDS[clampi(s, 0, STAGE_WORDS.size() - 1)])
	if _run_lines.is_empty() or _run_lines[-1] != _stage_text.text:
		_run_lines.append(_stage_text.text)
	var lines := PackedStringArray()
	for i in _run_lines.size():
		lines.append(tr(&"benchmark.run_done" if i < _run_lines.size() - 1 else &"benchmark.run_now").format([_run_lines[i]]))
	_run_list.text = "\n".join(lines)
	var p := clampf(Kit.get_float(stage, "progress"), 0.0, 1.0)
	_progress.value = p
	var left := Kit.get_float(stage, "seconds_left")
	_left_text.text = tr(&"benchmark.left").format([_duration(left)]) if left >= 1.0 else (tr(&"benchmark.loading") if p <= 0.0 else "")
	# The battle stages draw the world; the others run with the plain screen behind the strip.
	var battle := s == BENCH_REFERENCE or s == BENCH_COOP or s == BENCH_SWARM or s == BENCH_SOAK
	_backing.visible = not battle or _world == null
	_page.visible = not battle or _world == null


static func _duration(seconds: float) -> String:
	var s: Variant = Kit.call_on(&"Tx", &"duration", [maxf(seconds, 0.0)])
	return String(s) if s is String and not (s as String).is_empty() else "%d s" % int(seconds)


func _on_session_state(state: int) -> void:
	if _state != &"running" or not is_inside_tree():
		return
	var info := Kit.as_dict(Kit.call_on(&"Session", &"info"))
	var ours := Kit.get_int(info, "kind", -1) == KIND_BENCHMARK
	if (state == SESSION_LOADING or state == SESSION_IN_SORTIE) and ours:
		_ensure_world()
	elif state == SESSION_IDLE or state == SESSION_FAILED:
		_free_world()
	show_stage(_stage)


func _ensure_world() -> void:
	if _world != null and is_instance_valid(_world):
		return
	var scene := load(WORLD_SCENE) as PackedScene
	if scene == null:
		push_error("benchmark: cannot load %s" % WORLD_SCENE)
		return
	_world = scene.instantiate()
	# Behind the strip: the world is a Node3D drawn by the viewport, the strip a Control on top.
	add_child(_world)
	move_child(_world, 0)
	_orbit_applied = 0.0


func _free_world() -> void:
	if _world != null and is_instance_valid(_world):
		_world.queue_free()
	_world = null


func _view() -> Node:
	if _world == null or not is_instance_valid(_world):
		return null
	return _world.get_node_or_null(^"View")


func _process(delta: float) -> void:
	if _state != &"running":
		set_process(false)
		return
	var frame_ms := delta * 1000.0
	var gpu_ms := -1.0
	if _measure_gpu and _gpu_rid.is_valid():
		gpu_ms = RenderingServer.viewport_get_measured_render_time_gpu(_gpu_rid)
	var missed := frame_ms > MISSED_VBLANK_FACTOR * 1000.0 / _refresh_hz
	Kit.call_on(&"Bench", &"frame_sample", [frame_ms, gpu_ms, missed])
	_drive_camera()


## The fixed camera path: the orbit's yaw is a function of stage time alone, so every run and every
## machine sees the same pictures at the same moments of the stage.
func _drive_camera() -> void:
	var view := _view()
	if view == null:
		return
	var t := (Time.get_ticks_msec() - _stage_start_ms) / 1000.0
	var target := t * ORBIT_DEG_PER_S
	var step := target - _orbit_applied
	if absf(step) > 0.0001:
		view.call(&"orbit", step, 0.0)
		_orbit_applied = target


func _on_cancel() -> void:
	Kit.call_on(&"Bench", &"cancel")
	_end_run()
	_show_state(&"results" if not _report.is_empty() else &"ready")
	# The Stop button just went away with the run; put keys and the gamepad back on the page.
	Ui.focus(Screens.first_focusable(self) if _state == &"results" else _start)


func _end_run() -> void:
	set_process(false)
	_free_world()
	if _measure_gpu and _gpu_rid.is_valid():
		RenderingServer.viewport_set_measure_render_time(_gpu_rid, false)
	_measure_gpu = false
	_backing.visible = true
	_page.visible = true


func _on_finished(report: Dictionary) -> void:
	_end_run()
	show_report(report)


# --- results ------------------------------------------------------------------------------------

## Render a `BenchReportDict`.
func show_report(report: Dictionary) -> void:
	_report = report
	_table.clear()
	var root := _table.create_item()
	for st: Dictionary in Kit.dicts(report.get("stages", [])):
		var item := _table.create_item(root)
		var s := Kit.get_int(st, "stage")
		item.set_text(0, tr(STAGE_WORDS[clampi(s, 0, STAGE_WORDS.size() - 1)]))
		var tier := Kit.get_int(st, "tier", -1)
		item.set_text(1, tr(TIER_WORDS[tier]) if tier >= 0 and tier < TIER_WORDS.size() else "")
		if Kit.get_bool(st, "failed"):
			item.set_text(2, tr(&"benchmark.failed"))
			item.set_icon(2, Ui.icon(&"check_fail"))
			continue
		var battle := s == BENCH_REFERENCE or s == BENCH_COOP or s == BENCH_SWARM or s == BENCH_SOAK
		if not battle:
			# Timing stages have one figure each and no tier: it reads in the stage cell.
			item.set_text(0, tr(&"benchmark.job_row").format([item.get_text(0), _job_figure(s, report)]))
			item.set_text(1, "")
			continue
		item.set_text(2, triple(st.get("frame_ms", Vector3.ZERO)))
		var gpu: Variant = st.get("gpu_ms", Vector3.ZERO)
		item.set_text(3, ms(gpu.y) if gpu is Vector3 and gpu.y > 0.0 else tr(&"benchmark.not_measured"))
		var tick: Variant = st.get("tick_ms", Vector2.ZERO)
		item.set_text(4, tr(&"benchmark.pair").format([ms(tick.x), ms(tick.y)]) if tick is Vector2 else "")
		item.set_text(5, ms(Kit.get_float(st, "adapter_ms_p99")))
		item.set_text(6, Kit.percent(Kit.get_float(st, "missed_vblanks")))
		item.set_text(7, Kit.number(float(Kit.get_int(st, "frames")), &"none", 0))
		for c in range(2, COLS):
			item.set_text_alignment(c, HORIZONTAL_ALIGNMENT_RIGHT)
	# Every row shows; the page scrolls instead of the table.
	_table.custom_minimum_size = Vector2(0.0, 72.0 + 46.0 * float(root.get_child_count()))
	_facts.text = facts_text(report)
	_verdict.text = verdict_text(report)
	_note.visible = false
	_show_state(&"results")


func _job_figure(stage: int, report: Dictionary) -> String:
	if stage == BENCH_CALIB:
		return tr(&"benchmark.calib").format([Kit.number(Kit.get_float(report, "sim_calib_us"), &"none", 1)])
	return tr(&"benchmark.editor").format([ms(Kit.get_float(report, "editor_gauge_ms_p95")), ms(Kit.get_float(report, "editor_splash_ms_p95"))])


static func ms(v: float) -> String:
	return Kit.tr_key(&"benchmark.ms").format([Kit.number(v, &"none", 1)])


## "p50 / p95 / p99" of a Vector3 of milliseconds.
static func triple(v: Variant) -> String:
	if not v is Vector3:
		return ""
	var t := v as Vector3
	return Kit.tr_key(&"benchmark.triple").format([Kit.number(t.x, &"none", 1), Kit.number(t.y, &"none", 1), Kit.number(t.z, &"none", 1)])


static func verdict_text(report: Dictionary) -> String:
	var lines := PackedStringArray()
	lines.append(Kit.tr_key(&"benchmark.done"))
	var soak := Kit.as_dict(report.get("soak"))
	if not soak.is_empty():
		var p95 := Kit.get_float(soak, "frame_p95_last_5_min_ms")
		lines.append(Kit.tr_key(&"benchmark.soak_pass" if Kit.get_bool(soak, "pass") else &"benchmark.soak_fail").format([ms(p95)]))
	for p: Variant in Kit.as_array(report.get("problems", [])):
		var s := String(p)
		if not s.is_empty():
			lines.append(s)
	return "\n".join(lines)


static func facts_text(r: Dictionary) -> String:
	var lines := PackedStringArray()
	lines.append(Kit.tr_key(&"benchmark.fact.build").format([Kit.get_str(r, "build"), Kit.get_str(r, "os")]))
	lines.append(Kit.tr_key(&"benchmark.fact.cpu").format([Kit.get_str(r, "cpu"), Kit.number(Kit.get_float(r, "ram_gb"), &"none", 0)]))
	var driver := Kit.get_str(r, "driver_version")
	lines.append(Kit.tr_key(&"benchmark.fact.gpu").format([Kit.get_str(r, "adapter"), driver]) if not driver.is_empty() else Kit.tr_key(&"benchmark.fact.gpu_no_driver").format([Kit.get_str(r, "adapter")]))
	var fallbacks := PackedStringArray()
	var raw: Variant = r.get("fallbacks", PackedStringArray())
	for f: Variant in (Array(raw) if raw is PackedStringArray else Kit.as_array(raw)):
		fallbacks.append(String(f))
	lines.append(Kit.tr_key(&"benchmark.fact.render").format([Kit.get_str(r, "rendering_method"), Kit.get_str(r, "rendering_driver"),
		", ".join(fallbacks) if not fallbacks.is_empty() else Kit.tr_key(&"benchmark.none")]))
	var tier := Kit.get_int(r, "tier", -1)
	var window: Variant = r.get("window", Vector2i.ZERO)
	var w := window as Vector2i if window is Vector2i else Vector2i.ZERO
	lines.append(Kit.tr_key(&"benchmark.fact.tier").format([Kit.tr_key(TIER_WORDS[tier]) if tier >= 0 and tier < TIER_WORDS.size() else "",
		"%d x %d" % [w.x, w.y], Kit.percent(Kit.get_float(r, "scale_3d", 1.0))]))
	lines.append(Kit.tr_key(&"benchmark.fact.memory").format([Kit.number(Kit.get_float(r, "peak_mem_mb"), &"none", 0), Kit.number(Kit.get_float(r, "video_mem_mb"), &"none", 0)]))
	lines.append(Kit.tr_key(&"benchmark.fact.pipelines").format([Kit.get_int(r, "pipeline_compilations")]))
	var dpi := Kit.get_str(r, "dpi_awareness")
	if not dpi.is_empty() and dpi != "n/a":
		lines.append(Kit.tr_key(&"benchmark.fact.dpi").format([dpi]))
	var thermal: Variant = r.get("thermal", PackedInt32Array())
	if thermal is PackedInt32Array and not (thermal as PackedInt32Array).is_empty():
		var peak := 0
		for v: int in thermal as PackedInt32Array:
			peak = maxi(peak, v)
		lines.append(Kit.tr_key(&"benchmark.fact.thermal").format([Kit.tr_key(StringName("benchmark.thermal.%d" % clampi(peak, 0, 3)))]))
	return "\n".join(lines)


func _on_copy() -> void:
	var json := Kit.get_str(_report, "json")
	if json.is_empty():
		_say(tr(&"benchmark.no_report"))
		return
	DisplayServer.clipboard_set(json)
	_say(tr(&"benchmark.copied"))


func _on_save() -> void:
	var path: Variant = Kit.call_on(&"Bench", &"save_report")
	if path is String and not (path as String).is_empty():
		_say(tr(&"benchmark.saved").format([ProjectSettings.globalize_path(path)]))
	else:
		_say(tr(&"benchmark.save_failed"))


func _on_issue() -> void:
	var url: Variant = Kit.call_on(&"Bench", &"issue_form_url")
	if url is String and (url as String).begins_with("https://"):
		OS.shell_open(url)
		_say(tr(&"benchmark.issue_opened"))


func _on_again() -> void:
	_report = {}
	_show_state(&"ready")
	Ui.focus(_start)


func _on_vulkan() -> void:
	var args := PackedStringArray(["--rendering-driver", "vulkan", "--", "--benchmark"])
	var pid := OS.create_process(OS.get_executable_path(), args)
	if pid > 0:
		get_tree().quit()
	else:
		_say(tr(&"benchmark.vulkan_failed"))


func _say(text: String) -> void:
	_note.text = text
	_note.visible = not text.is_empty()


func _leave() -> void:
	if _state == &"running":
		return
	Screens.change(&"main_menu")


func _link_focus() -> void:
	Kit.chain_vertical([_soak, _start])
	_soak.focus_neighbor_top = _soak.get_path_to(_back)
	Kit.chain_horizontal([_copy, _issue, _save, _again, _vulkan])
	for c: Control in [_copy, _issue, _save, _again, _vulkan]:
		c.focus_neighbor_top = c.get_path_to(_table)
		c.focus_neighbor_bottom = c.get_path_to(c)
	_table.focus_neighbor_bottom = _table.get_path_to(_copy)
	_table.focus_neighbor_top = _table.get_path_to(_back)
	_back.focus_neighbor_bottom = _back.get_path_to(_soak if _state == &"ready" else _table)
