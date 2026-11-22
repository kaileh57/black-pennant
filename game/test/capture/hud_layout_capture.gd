extends Node
## Windowed layout captures of the sortie HUD in one run: a live skirmish (or a scripted
## BpTestFeed picture) through the real routing, then a fixed walk through the HUD's states, each
## saved as a PNG and checked by a layout audit:
##   godot --path game --resolution 1920x1080 res://test/capture/hud_layout_capture.tscn -- \
##     --mode=skirmish|scripted|splash --script=reference_battle --scale=1 --palette=dusk \
##     --states=bridge,select,plot,log,damage,networks,control,held --wait=90 --out=/abs/dir
## The audit fails a state when two visible clusters overlap, a cluster leaves the visible rect, or
## a cluster's content runs outside it (outside scroll containers), and prints the share of the
## frame the clusters leave open in the bridge view. Exits 1 when any state failed.
## `--mode=splash` runs the HUD the way the tutorial's fight does: the dry dock opens on the starter and
## splashes against the patrol boat, and the dock's splash bar panel (with its Tune and edit toggle) and
## its scope readout count as clusters too, so the bar and the HUD's top row must share the width without
## overlap or clipping; the Tune dock, opened over the left dock on purpose, must stay on screen and under
## the bar. Extra states: `tune` (the Tune dock open), `struck` (the selected track card with its
## struck-colours line); any state name may end in
## `_s2` to take it at UI scale 200%.
## Headless, or when instanced by check_project, it does nothing.

const STARTER := "starters/fac22.bp.ron"
const LIMIT := 20000

var _mode := "skirmish"
var _script := "reference_battle"
var _palette := "dusk"
var _scale := 1.0
var _states: PackedStringArray = ["bridge", "select", "plot", "log", "damage", "networks", "control", "held"]
var _wait := 90
var _warm := 360
var _out := "/tmp"
var _prefix := ""
## Scripted mode: seconds of the script to jump through before the walk (one tick batch a frame).
var _skip := 0
var _skipped := 0
var _feed: Node = null
var _frame := 0
var _sortie_frame := -1
var _active := false
var _failures := 0
var _index := -1
var _countdown := 0
## Splash mode: the dry dock hosting the basin, and the frame the splash was asked for.
var _dock: Node = null
var _splash_frame := -1


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		var kv := arg.trim_prefix("--").split("=", true, 1)
		if kv.size() != 2:
			continue
		match kv[0]:
			"mode": _mode = kv[1]
			"script": _script = kv[1]
			"palette": _palette = kv[1]
			"scale": _scale = float(kv[1])
			"states": _states = kv[1].split(",", false)
			"wait": _wait = int(kv[1])
			"warm": _warm = int(kv[1])
			"out": _out = kv[1]
			"prefix": _prefix = kv[1]
			"skip": _skip = int(kv[1])
	DirAccess.make_dir_recursive_absolute(_out)
	Settings.reload()
	Ui.refresh()
	Ui.call(&"_apply_palette", StringName(_palette))
	Ui.call(&"_apply_scale", _scale)
	Screens.start_routing()
	if _mode == "splash":
		var issues: Array = Screens.open_dry_dock(Bp.DOCK_SANDBOX, {"blueprint": STARTER})
		for i: Dictionary in issues:
			if bool(i.get("fatal", false)):
				_fail("the dry dock refused the starter: " + str(issues))
				get_tree().quit(1)
				return
		_dock = Screens.top()
	elif _mode == "scripted":
		_feed = ClassDB.instantiate(&"BpTestFeed")
		add_child(_feed)
		if not bool(_feed.call(&"start", _script)):
			_fail("the test feed refused %s" % _script)
			get_tree().quit(1)
			return
		_feed.call(&"advance", 12)
	else:
		var problems := Session.start_skirmish({
			"template": Bp.MISSION_CONVOY_RAID, "act": Bp.ACT_1, "enemy_per_player": 3, "difficulty": {"preset": "normal"},
			"weather": Bp.WEATHER_CLEAR, "sea_state": 3, "time_of_day": Bp.TIME_DUSK, "seed": 1234, "blueprint": STARTER,
		})
		if not problems.is_empty():
			_fail("start_skirmish refused: " + str(problems))
			get_tree().quit(1)
			return
	_active = true


func _process(_delta: float) -> void:
	if not _active:
		return
	_frame += 1
	if _feed != null:
		_feed.call(&"advance", 1)
	if _frame > LIMIT:
		_fail("ran out of frames")
		_end()
		return
	if _mode == "splash" and _splash_frame < 0:
		# The dock shows the design first; then Splash, as the player presses it.
		if _frame < 60:
			return
		_splash_frame = _frame
		Editor.set_splash_options({"water": 3, "target": Bp.SPLASH_TARGET_PATROL_BOAT})
		_dock.call(&"_do_splash")
		return
	if _sortie_frame < 0:
		if Session.state() == Bp.SESSION_IN_SORTIE and not Screens.is_loading():
			_sortie_frame = _frame
		elif Session.state() == Bp.SESSION_FAILED:
			_fail("the session failed")
			_end()
		return
	var hud := _hud()
	if hud == null:
		if _frame - _sortie_frame > 120:
			_fail("no HUD on the stack")
			_end()
		return
	if _feed != null and _skipped < _skip:
		_feed.call(&"advance", 60)
		_skipped += 1
		return
	var since := _frame - _sortie_frame - _skipped
	if since == 30 and _mode == "skirmish":
		_course_ahead()
	if since < _warm:
		return
	if _countdown > 0:
		_countdown -= 1
		if _states[_index].trim_suffix("_s2") == "struck":
			_force_struck(hud)
		return
	if _index >= 0:
		_shoot(hud, _states[_index])
	_index += 1
	if _index >= _states.size():
		print("hud_layout: done, failures ", _failures)
		_end()
		return
	_enter(hud, _states[_index])
	_countdown = _wait


func _hud() -> Node:
	if _mode == "splash":
		var h: Variant = _dock.get(&"_hud") if _dock != null and is_instance_valid(_dock) else null
		if h is Node and is_instance_valid(h) and (h as Node).is_inside_tree():
			return h as Node
		return null
	var top: Node = Screens.top()
	if top != null and top.has_method(&"toggle_view"):
		return top
	return null


func _course_ahead() -> void:
	var m: Dictionary = Hud.motion()
	if not m.has("pos"):
		return
	var h := deg_to_rad(float(m.get("heading_deg", 0.0)) + 30.0)
	Orders.set_course(PackedVector2Array([Vector2(m["pos"]) + Vector2(sin(h), -cos(h)) * 4000.0]), 20.0, false)


## Put the HUD into a state from a clean bridge view.
func _enter(hud: Node, state: String) -> void:
	hud.call(&"close_station")
	# Any state ending in _s2 is taken at UI scale 200%, in every mode.
	Ui.call(&"_apply_scale", 2.0 if state.ends_with("_s2") else _scale)
	state = state.trim_suffix("_s2")
	if _mode == "splash":
		var tune := _dock.get(&"_tune") as Button
		var bar := _dock.get_node_or_null(^"SplashUi/Bar/Panel") as Control
		if tune != null and (bar == null or not bar.is_ancestor_of(tune)):
			_fail("Tune and edit is not in the splash bar")
		if tune == null:
			_fail("no Tune and edit toggle in the splash bar")
		elif tune.button_pressed != (state == "tune"):
			tune.button_pressed = state == "tune"
	var layers := hud.get_node_or_null(^"Frame/View/PlotTable/Side/Tools/Margin/Column/Layers") as Button
	if layers != null and layers.button_pressed:
		layers.button_pressed = false
	if state != "swap" and bool(Settings.get_value(&"hud.swap_docks")):
		Settings.set_value(&"hud.swap_docks", false)
		hud.call(&"_apply_swap")
	hud.call(&"set_view", Bp.VIEW_BRIDGE)
	if Session.pacing().get("paused", false):
		Session.set_paused(false)
	match state:
		"bridge":
			Hud.select_track(Bp.NONE)
		"select", "struck":
			Hud.select_step(1)
		"plot":
			Hud.select_step(1)
			hud.call(&"set_view", Bp.VIEW_PLOT)
		"layers":
			hud.call(&"set_view", Bp.VIEW_PLOT)
			var b := hud.get_node_or_null(^"Frame/View/PlotTable/Side/Tools/Margin/Column/Layers") as Button
			if b != null:
				b.button_pressed = true
		"swap":
			Settings.set_value(&"hud.swap_docks", true)
			hud.call(&"_apply_swap")
		"log", "damage", "networks", "control":
			hud.call(&"open_station", StringName(state))
		"held":
			Orders.take_control(Bp.ROLE_HELM, 0, true)
			Orders.take_control(Bp.ROLE_WEAPON_GROUP, 0, true)
			Hud.select_step(1)
			if _mode == "skirmish":
				Session.set_paused(true)
		"released":
			Orders.take_control(Bp.ROLE_HELM, 0, false)
			Orders.take_control(Bp.ROLE_WEAPON_GROUP, 0, false)


## The `struck` state: the selected track's card drawn as if the side had seen the hull strike its
## colours. No scripted picture strikes, so the flag is laid on the card's own copy each frame.
func _force_struck(hud: Node) -> void:
	var card := hud.get_node_or_null(^"Frame/Docks/LeftDock/Column/TrackCard")
	if card == null:
		return
	var data: Variant = card.get(&"_data")
	if data is Dictionary and not (data as Dictionary).is_empty() and not bool((data as Dictionary).get("struck", false)):
		var shown := (data as Dictionary).duplicate()
		shown["struck"] = true
		card.call(&"show_track", shown, card.get(&"_card"))


func _shoot(hud: Node, state: String) -> void:
	var name := "%s%s_%sx" % [_prefix, state, str(_scale)]
	if _mode == "splash":
		name = "%ssplash_%s_%sx" % [_prefix, state, str(_scale)]
	var path := _out.path_join(name + ".png")
	get_viewport().get_texture().get_image().save_png(path)
	var problems := _audit(hud, state)
	print("hud_layout: saved ", path, " visible ", get_viewport().get_visible_rect().size, " tracks ", Hud.tracks().size(), " problems ", problems.size())
	for p: String in problems:
		_fail("%s: %s" % [state, p])


## Overlaps between clusters, clusters off the screen, and content outside its cluster.
func _audit(hud: Node, state: String) -> PackedStringArray:
	var out := PackedStringArray()
	var screen := get_viewport().get_visible_rect()
	var shown: Array[Control] = []
	var rects: Array[Rect2] = []
	var listed: Array = (hud.call(&"clusters") as Array).duplicate()
	# In a splash the dock's bar, its readout and the Tune dock are drawn over the HUD as well.
	if _mode == "splash" and _dock != null:
		for path: NodePath in [^"SplashUi/Bar/Panel", ^"SplashUi/Bar/Readout"]:
			var c := _dock.get_node_or_null(path) as Control
			if c != null:
				listed.append(c)
		# The Tune dock is a panel the player opens over the left dock on purpose; it only has to stay on the
		# screen and under the bar.
		var tune := _dock.get_node_or_null(^"SplashUi/TuneDock") as Control
		var bar := _dock.get_node_or_null(^"SplashUi/Bar/Panel") as Control
		if tune != null and tune.is_visible_in_tree():
			if not screen.grow(1.0).encloses(tune.get_global_rect()):
				out.append("TuneDock leaves the screen: %s" % tune.get_global_rect())
			if bar != null and tune.get_global_rect().intersection(bar.get_global_rect()).has_area():
				out.append("TuneDock overlaps the splash bar")
	# The plot's tool column (with its open popovers) is a cluster too in the plot view.
	var side := hud.get_node_or_null(^"Frame/View/PlotTable/Side") as Control
	if side != null and side.is_visible_in_tree():
		listed.append(side)
	for c: Control in listed:
		if not c.is_visible_in_tree() or c.size.x < 1.0 or c.size.y < 1.0:
			continue
		# A cluster in a scrolling dock is seen through the dock's clip.
		var r := c.get_global_rect()
		var clip := c.get_parent().get_parent() as ScrollContainer
		if clip != null:
			r = r.intersection(clip.get_global_rect())
			if r.size.y < 1.0:
				continue
		shown.append(c)
		rects.append(r)
	for i in shown.size():
		var a := rects[i]
		if not screen.grow(1.0).encloses(a):
			out.append("%s leaves the screen: %s" % [shown[i].name, a])
		var min_size := shown[i].get_combined_minimum_size()
		var full := shown[i].get_global_rect()
		if min_size.x > full.size.x + 1.0 or min_size.y > full.size.y + 1.0:
			out.append("%s is smaller than its minimum: %s < %s" % [shown[i].name, a.size, min_size])
		var dock := shown[i].get_parent().get_parent() as Control
		if dock != null and String(dock.name).ends_with("Dock") and min_size.x > dock.custom_minimum_size.x + 1.0:
			out.append("%s is wider than its dock: %s > %s, from %s" % [shown[i].name, min_size.x, dock.custom_minimum_size.x, _widest(shown[i], dock.custom_minimum_size.x)])
		var centre := hud.get_node(^"Frame/Docks/Centre") as Control
		if centre.is_ancestor_of(shown[i]) and min_size.x > centre.size.x - 31.0:
			out.append("%s is wider than the centre: %s > %s, from %s" % [shown[i].name, min_size.x, centre.size.x - 32.0, _widest(shown[i], centre.size.x - 32.0)])
		for j in range(i + 1, shown.size()):
			var b := rects[j]
			var hit := a.intersection(b)
			if hit.size.x > 1.0 and hit.size.y > 1.0:
				out.append("%s overlaps %s by %s" % [shown[i].name, shown[j].name, hit.size])
		_audit_children(shown[i], shown[i].get_global_rect(), out)
	if state != "plot":
		var open := _open_share(shown, screen)
		print("hud_layout: ", state, " open sea share ", snappedf(open, 0.01))
	return out


## The deepest visible control whose minimum width passes `limit`, with its minimum.
func _widest(node: Control, limit: float) -> String:
	var found := "%s %s" % [node.get_path(), node.get_combined_minimum_size().x]
	for c: Node in node.get_children():
		var ctl := c as Control
		if ctl != null and ctl.visible and ctl.get_combined_minimum_size().x > limit - 32.0:
			return _widest(ctl, limit)
	return found


func _audit_children(node: Node, bounds: Rect2, out: PackedStringArray) -> void:
	for c: Node in node.get_children():
		var ctl := c as Control
		if ctl == null or not ctl.is_visible_in_tree():
			continue
		var r := ctl.get_global_rect()
		if r.size.x >= 1.0 and r.size.y >= 1.0 and not bounds.grow(1.0).encloses(r):
			out.append("%s runs outside %s: %s vs %s" % [ctl.get_path(), bounds, r, bounds])
			continue
		if ctl is ScrollContainer:
			continue
		_audit_children(ctl, bounds, out)


## The share of the screen no cluster covers, on an 8 px grid.
static func _open_share(shown: Array[Control], screen: Rect2) -> float:
	var step := 8.0
	var open := 0
	var total := 0
	var rects: Array[Rect2] = []
	for c: Control in shown:
		rects.append(c.get_global_rect())
	var y := screen.position.y + step * 0.5
	while y < screen.end.y:
		var x := screen.position.x + step * 0.5
		while x < screen.end.x:
			total += 1
			var hit := false
			for r: Rect2 in rects:
				if r.has_point(Vector2(x, y)):
					hit = true
					break
			if not hit:
				open += 1
			x += step
		y += step
	return float(open) / float(maxi(total, 1))


func _fail(text: String) -> void:
	_failures += 1
	push_error("hud_layout: " + text)


func _end() -> void:
	_active = false
	get_tree().quit(1 if _failures > 0 else 0)
