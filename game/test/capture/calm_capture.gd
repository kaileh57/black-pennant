extends Node
## The 30 s calm capture (DESIGN.md L1755: shows no alert pixels or untraced change). Unit: ui-tests.
## Windowed, through the real router, Session, Hud, world and sortie HUD, with BpTestFeed's `calm`
## script (own hull under way, no contacts, no alerts) played at the real 60 ticks a second:
##   godot --path game --resolution 1600x900 res://test/capture/calm_capture.tscn -- \
##     --out=/abs/dir [--palette=dusk|high_contrast] [--scale=1] [--seconds=30]
## From 5 s, every 0.7 s (off the phase of the 1 Hz and 2.5 Hz blink clocks, so a pulse cannot
## hide between samples), it renders one frame with the 3D world switched off, so only the HUD and
## other 2D layers are in the image (the sea moves on its own and is not a HUD change), and checks:
## - every 5 s: no pixel within Gates.ALERT_RGB_DISTANCE of an alert colour of the palette in use;
## - every 16 px block that differs from the first sample lies inside a control whose text, value,
##   visibility or rect changed between those samples (a traced change); anything else is an
##   untraced change;
## - no alert reaches Hud, no officer line, no toast, and no running tween without `sim_signal`.
## Saves calm_<palette>_<scale>x_full.png (world and HUD) and calm_<palette>_<scale>x_hud.png (the
## last HUD-only sample). Exits 1 on any failure. Headless, or when instanced by check_project, it
## does nothing.

const Gates := preload("res://test/unit/gates.gd")
const TICK_HZ := 60
const FIRST_SAMPLE_S := 5.0
const SAMPLE_EVERY_S := 0.7
const SCAN_EVERY_S := 5.0
## Frames the run may spend loading before it fails.
const LOAD_LIMIT := 1800

var _out := "user://captures"
var _palette := "dusk"
var _scale := 1.0
var _seconds := 30.0
var _feed: Node
var _ticks := 0
var _carry := 0.0
var _failures := 0
var _alerts := 0
var _lines := 0
var _toasts := 0
var _untraced_tweens := {}
var _running := false


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		var kv := arg.trim_prefix("--").split("=", true, 1)
		if kv.size() != 2:
			continue
		match kv[0]:
			"out": _out = kv[1]
			"palette": _palette = kv[1]
			"scale": _scale = float(kv[1])
			"seconds": _seconds = clampf(float(kv[1]), 10.0, 600.0)
	DirAccess.make_dir_recursive_absolute(_out)
	# A covered window draws nothing on macOS (`window_can_draw` turns false), which would leave
	# stale frames; keep the short capture window above other windows while it runs.
	get_window().always_on_top = true
	_run.call_deferred()


func _fail(what: String) -> void:
	_failures += 1
	printerr("calm_capture: FAIL ", what)


func _seconds_played() -> float:
	return float(_ticks) / TICK_HZ


## The feed follows the wall clock while the run is live, so the capture is 30 s of real play.
func _process(delta: float) -> void:
	if not _running or _feed == null:
		return
	_carry += delta * TICK_HZ
	var n := int(_carry)
	if n > 0:
		_carry -= n
		_feed.call(&"advance", n)
		_ticks += n
	for t: String in Gates.untraced_tweens(get_tree()):
		_untraced_tweens[t] = true


func _run() -> void:
	Settings.reload()
	Ui.refresh()
	Ui.call(&"_apply_palette", StringName(_palette))
	Ui.call(&"_apply_scale", _scale)
	Screens.start_routing()
	Hud.alert_raised.connect(func(_a: Dictionary) -> void: _alerts += 1)
	Hud.officer_line.connect(func(_l: Dictionary) -> void: _lines += 1)
	_feed = ClassDB.instantiate(&"BpTestFeed")
	add_child(_feed)
	if not bool(_feed.call(&"start", "calm")):
		_fail("BpTestFeed.start(calm) refused")
		_finish()
		return
	_feed.call(&"advance", 12)
	_ticks = 12
	_running = true
	var waited := 0
	while Session.state() != Bp.SESSION_IN_SORTIE or Screens.is_loading() or _hud() == null:
		await get_tree().process_frame
		waited += 1
		if waited > LOAD_LIMIT:
			_fail("the calm sortie never came up (session %d, loading %s)" % [Session.state(), Screens.is_loading()])
			_finish()
			return
	# Count only what happens once the picture is up.
	_alerts = 0
	_lines = 0
	_untraced_tweens.clear()
	var start_ticks := _ticks
	var toast: Node = Screens.get(&"_toast")
	var first: Image
	var first_state: Dictionary
	var last: Image
	var samples := 0
	var next_s := FIRST_SAMPLE_S
	var next_scan_s := FIRST_SAMPLE_S
	var changed_samples := 0
	while float(_ticks - start_ticks) / TICK_HZ < _seconds:
		await get_tree().process_frame
		if toast != null and bool(toast.call(&"is_showing")):
			_toasts += 1
		var now_s := float(_ticks - start_ticks) / TICK_HZ
		if now_s < next_s:
			continue
		next_s += SAMPLE_EVERY_S
		var state := Gates.control_state(_hud())
		var img := await _hud_only()
		samples += 1
		if now_s >= next_scan_s:
			next_scan_s += SCAN_EVERY_S
			_judge_pixels(img, now_s)
		if first == null:
			first = img
			first_state = state
		elif _judge_change(first, first_state, img, state, now_s):
			changed_samples += 1
		last = img
	# The last sample switched the world off for a frame; let it draw again first.
	for _i: int in range(3):
		if not await _drawn():
			_fail("the window drew nothing (covered or minimised?)")
	var full := get_viewport().get_texture().get_image()
	var stem := "%s/calm_%s_%sx" % [_out, _palette, String.num(_scale, 2)]
	_save(full, stem + "_full.png")
	if last != null:
		_save(last, stem + "_hud.png")
	if _alerts > 0:
		_fail("%d alerts raised on a calm sea" % _alerts)
	if _lines > 0:
		_fail("%d officer lines on a calm sea" % _lines)
	if _toasts > 0:
		_fail("a toast showed for %d frames" % _toasts)
	if not _untraced_tweens.is_empty():
		_fail("tweens ran with no sim_signal: %s" % str(_untraced_tweens.keys()))
	if last != null:
		_negative_control(last)
	if samples < 2:
		_fail("only %d samples taken" % samples)
	print("calm_capture: %.1f s played, %d HUD samples, %d with a changed block, %d failures" % [float(_ticks - start_ticks) / TICK_HZ, samples, changed_samples, _failures])
	_finish()


func _hud() -> Node:
	var found := get_tree().root.find_children("SortieHud", "CanvasLayer", true, false)
	return null if found.is_empty() else found[0]


## One frame with the 3D world off: only canvas layers draw.
func _hud_only() -> Image:
	var vp := get_viewport()
	vp.disable_3d = true
	for _i: int in range(2):
		if not await _drawn():
			_fail("the window drew nothing (covered or minimised?)")
	var img := vp.get_texture().get_image()
	vp.disable_3d = false
	return img


func _judge_pixels(img: Image, at_s: float) -> void:
	var colours: Array[Color] = []
	for t: StringName in Gates.ALERT_TOKENS:
		if Ui.has_colour(t):
			colours.append(Ui.colour(t))
	var found := Gates.colour_pixels(img, colours)
	if int(found["count"]) > 0:
		_fail("%d alert-colour pixels at %.1f s inside %s" % [found["count"], at_s, found["bounds"]])
		_save(img, "%s/calm_alert_pixels_%ds.png" % [_out, int(at_s)])


## True when any block changed (traced or not).
func _judge_change(a: Image, a_state: Dictionary, b: Image, b_state: Dictionary, at_s: float) -> bool:
	var blocks := Gates.diff_blocks(a, b)
	if blocks.is_empty():
		return false
	var traced: Array[Rect2] = []
	var hud := _hud()
	for p: String in Gates.changed_paths(a_state, b_state):
		var c := hud.get_node_or_null(NodePath(p)) as Control
		if c != null and c.is_visible_in_tree():
			traced.append(Gates.window_rect(c).grow(2.0))
	var untraced := 0
	var bounds := Rect2i()
	for blk: Rect2i in blocks:
		var r := Rect2(blk)
		var hit := false
		for t: Rect2 in traced:
			hit = hit or t.intersects(r)
		if not hit:
			bounds = blk if untraced == 0 else bounds.merge(blk)
			untraced += 1
	print("calm_capture: %.1f s, %d changed blocks, %d traced controls, %d untraced blocks" % [at_s, blocks.size(), traced.size(), untraced])
	if untraced > 0:
		_fail("%d blocks changed with no control change behind them at %.0f s, inside %s" % [untraced, at_s, bounds])
		_save(b, "%s/calm_untraced_%ds.png" % [_out, int(at_s)])
	return true


## The judges must catch what the gate fails: an alert-coloured badge drawn into the last sample
## has to count as alert pixels and as a changed block.
func _negative_control(img: Image) -> void:
	var probe := img.duplicate() as Image
	probe.convert(Image.FORMAT_RGBA8)
	probe.fill_rect(Rect2i(Vector2i(40, 40), Vector2i(24, 12)), Ui.colour(&"alert.caution"))
	var colours: Array[Color] = [Ui.colour(&"alert.caution"), Ui.colour(&"alert.warning")]
	if int(Gates.colour_pixels(probe, colours)["count"]) < 24 * 12:
		_fail("control: an alert-colour badge was not counted")
	if Gates.diff_blocks(img, probe).is_empty():
		_fail("control: a drawn badge was not seen as a change")


func _save(img: Image, path: String) -> void:
	if img.save_png(path) != OK:
		_fail("cannot save " + path)
	else:
		print("calm_capture: saved ", path)


func _finish() -> void:
	_running = false
	if _feed != null:
		_feed.call(&"stop")
	get_tree().quit(1 if _failures > 0 else 0)


## Wait for the next drawn frame, or 30 process frames when the OS skips drawing (an occluded
## window draws nothing on macOS), so a covered window slows a run down but never stalls it.
## False when nothing was drawn: the image would be a stale frame.
func _drawn() -> bool:
	var state := {"drawn": false}
	var mark := func() -> void: state["drawn"] = true
	RenderingServer.frame_post_draw.connect(mark, CONNECT_ONE_SHOT)
	for _i: int in range(30):
		await get_tree().process_frame
		if state["drawn"]:
			return true
	if RenderingServer.frame_post_draw.is_connected(mark):
		RenderingServer.frame_post_draw.disconnect(mark)
	return false
