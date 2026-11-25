extends Control
## Windowed capture of the plot table (unit gd-plot): a BpPlotView fed by BpPlotCheck's scripted pictures,
## saved as PNGs at fixed steps, then the briefing and after-action charts from a real operation's map:
##   godot --path game --resolution 1600x900 res://test/capture/plot_capture.tscn -- --out=/abs/dir
## Each shot prints what the redraw showed (symbols, labels, hidden labels, overlaps). Headless, and when
## instanced by check_project, it does nothing.

## Frames each case is given to settle before its shot.
const SETTLE := 20

var _out := "user://captures"
var _plot: BpPlotView
var _check: BpPlotCheck
var _cases: Array[String] = ["empty", "golden", "selected", "near", "far", "crowded", "calm", "briefing", "after_action", "golden_hc", "golden_2x"]
var _case := -1
var _frame := 0
var _failures := 0


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--out="):
			_out = arg.trim_prefix("--out=")
		elif arg.begins_with("--case="):
			_cases = [arg.trim_prefix("--case=")]
	DirAccess.make_dir_recursive_absolute(_out)
	_plot = BpPlotView.new()
	_plot.set_anchors_preset(Control.PRESET_FULL_RECT)
	add_child(_plot)
	_check = BpPlotCheck.new()
	_next()


func _next() -> void:
	_case += 1
	_frame = 0
	if _case >= _cases.size():
		get_tree().quit(1 if _failures > 0 else 0)
		return
	var name := _cases[_case]
	_check.clear(_plot)
	# High contrast swaps the theme; 2x is the UI scale ceiling (accessibility floor 2).
	get_tree().root.theme = load("res://ui/theme/project_theme_high_contrast.tres") if name == "golden_hc" else null
	get_tree().root.content_scale_factor = 2.0 if name == "golden_2x" else 1.0
	_plot.set_mode(&"sortie")
	_plot.set_follow_own(true)
	_plot.set_scale_step(4)
	_plot.set_layers(-1)
	_plot.show_static({}, PackedVector2Array(), PackedVector3Array())
	match name:
		"golden", "selected", "near", "far", "golden_hc", "golden_2x":
			_check.install(_plot, "plot_40_tracks")
		"crowded":
			_check.install(_plot, "crowded")
		"calm":
			_check.install(_plot, "calm")
		"briefing", "after_action":
			var b := _check.briefing()
			if b.is_empty():
				push_error("plot_capture: no operation has a chart with land")
				_failures += 1
				_next()
				return
			_plot.set_mode(&"briefing" if name == "briefing" else &"after_action")
			var belief: PackedVector3Array = b["belief"] if name == "after_action" else PackedVector3Array()
			_plot.show_static(b["map"], b["route"], belief)
	if name == "near":
		_plot.set_scale_step(2)
	elif name == "far":
		_plot.set_scale_step(6)


func _process(_delta: float) -> void:
	if _plot == null or _case < 0 or _case >= _cases.size():
		return
	_frame += 1
	var name := _cases[_case]
	if name == "selected" and _frame == 4:
		var ids := _check.track_ids(_plot)
		_check.select(_plot, ids[0])
		_plot.move_plot_cursor(Vector2(-160, 40))
	# Live cases move on two ticks a frame, so every frame redraws and the timing below is steady state.
	if name in ["golden", "selected", "near", "far", "crowded", "calm", "golden_hc", "golden_2x"] and _frame > 8:
		_check.advance(_plot, 2)
	if _frame < SETTLE:
		return
	var image := get_viewport().get_texture().get_image()
	var path := _out.path_join("plot_%s.png" % name)
	image.save_png(path)
	var facts := _check.facts(_plot)
	print("plot_capture: ", name, " ", facts, " legend=", _plot.legend().size(), " cursor=", _plot.plot_cursor())
	if int(facts.get("overlaps", 0)) != 0 or int(facts.get("top_covers", 0)) != 0:
		push_error("plot_capture: overlapping labels, or a label over a top-priority symbol, in " + name)
		_failures += 1
	_next()
