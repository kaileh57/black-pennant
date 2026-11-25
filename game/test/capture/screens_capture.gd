extends Node
## Capture harness (DESIGN.md L1749-1755). Unit: ui-tests. Captures every screen with real data at
## UI scale 1x and 2x, each in dusk and in high contrast; chart screens are captured in the chart
## palette (what the dusk setting shows on the chart table) and in high contrast. Windowed:
##   HOME=/tmp/bp-capture-home godot --path game --resolution 1600x900 \
##     res://test/capture/screens_capture.tscn -- --out=/abs/dir \
##     [--only=stage,stage] [--palettes=dusk,high_contrast] [--scales=1,2] [--cvd=1] \
##     [--scratch-home] [--loopback-host]
## Everything runs through the real autoloads and router; nothing is hand-built:
## - menus: consent, main menu, credits, settings (every section), bindings, campaign slots, new
##   campaign, skirmish and sandbox setup, multiplayer, replay browser, blueprint browser (My
##   designs and the stock tree), share dialog, benchmark, the quit dialog and the loading cover,
##   each over the main menu as the game opens it;
## - `battle`: BpTestFeed's scripted battle at the sinking Emergency (fire, flooding, alerts of every
##   priority) on the bridge, the plot, each station (log, damage, networks, control), the pause
##   menu, then the sortie summary when the battle ends. With `--cvd=1` the bridge and plot shots
##   are also written in greyscale and as protan, deutan and tritan simulations (L1754 item 6);
## - `reference`: the 41-hull reference battle on the bridge and plot (declutter gate, L1764);
## - `dock`: the dry dock with a stock corvette, every tab of both docks and the graph and script
##   modes;
## - `campaign` (needs `--scratch-home`, because it writes a campaign save): a new campaign's
##   chart and each of its side tabs, a briefing, the campaign dry dock with Fit Check, the real
##   campaign sortie and its after-action report;
## - `lobby` (needs `--loopback-host` and `--scratch-home`): a sandbox lobby hosted on 127.0.0.1
##   with the host panel.
## Not reached here, so not captured by this harness: the replay root and its controls (needs a
## recorded replay), the connection panel, the tutorial overlay and the Splash bar, options and
## report; res://test/capture/u4_screens.tscn, res://test/ui/tutorial_capture.tscn and
## res://test/capture/splash_capture.tscn capture those.
## Files are <stage>_<palette>_<scale>x.png (palette `chart` for chart screens). Exits 1 if a stage
## could not reach its screen. Headless, or when instanced by check_project, it does nothing.
## Run it with a throwaway HOME: settings, first-launch flags and saves land in that user folder.

const Gates := preload("res://test/unit/gates.gd")
const STOCK := "starters/corvette40.bp.ron"
const STARTER := "starters/fac22.bp.ron"
const TICK_HZ := 60
## Frames a screen settles before a shot: layout, theme switch, fades (the slowest is 300 ms).
const SETTLE := 30
## Frames a stage may wait for its screen before it fails.
const LIMIT := 2400
## `access.palette` values in the settings schema, in order.
const PALETTE_SETTING: Array[StringName] = [&"dusk", &"high_contrast"]
const RESTORED: Array[StringName] = [&"access.palette", &"access.ui_scale", &"access.reduce_motion", &"access.blink_off"]
const STAGES: Array[String] = [
	"consent", "main_menu", "credits", "settings", "settings_bindings", "campaign_slots", "campaign_new",
	"skirmish_setup", "sandbox_setup", "multiplayer", "replay_browser", "blueprint_browser", "blueprint_browser_stock", "share_dialog",
	"benchmark", "quit_dialog", "loading", "battle", "reference", "dock", "campaign", "lobby",
]
## Over the main menu as the menu itself pushes them.
const PUSHED: Dictionary[String, Dictionary] = {
	"credits": {}, "settings_bindings": {}, "campaign_slots": {}, "campaign_new": {},
	"skirmish_setup": {}, "sandbox_setup": {}, "multiplayer": {}, "replay_browser": {},
	"blueprint_browser": {}, "blueprint_browser_stock": {"folder": "stock:starters"}, "benchmark": {},
	"share_dialog": {"path": STOCK, "mode": &"copy"},
}

var _out := "user://captures"
var _only: PackedStringArray = []
var _palettes: PackedStringArray = ["dusk", "high_contrast"]
var _scales: Array[float] = [1.0, 2.0]
var _cvd := false
var _scratch := false
var _loopback := false
var _failures := 0
var _shots := 0
var _feed: Node
var _feed_rate := 0
var _saved_settings := {}


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	var args := OS.get_cmdline_user_args()
	_scratch = "--scratch-home" in args
	_loopback = "--loopback-host" in args
	for arg: String in args:
		var kv := arg.trim_prefix("--").split("=", true, 1)
		if kv.size() != 2:
			continue
		match kv[0]:
			"out": _out = kv[1]
			"only": _only = kv[1].split(",", false)
			"palettes": _palettes = kv[1].split(",", false)
			"cvd": _cvd = kv[1] == "1"
			"scales":
				_scales.clear()
				for s: String in kv[1].split(",", false):
					_scales.append(clampf(float(s), 0.75, 2.0))
	DirAccess.make_dir_recursive_absolute(_out)
	# A covered window draws nothing on macOS (`window_can_draw` turns false), which would leave
	# stale frames; keep the short capture window above other windows while it runs.
	get_window().always_on_top = true
	_run.call_deferred()


func _fail(what: String) -> void:
	_failures += 1
	printerr("screens_capture: FAIL ", what)


func _wants(stage: String) -> bool:
	return _only.is_empty() or stage in _only


## Keeps a scripted session moving at its own pace while shots are taken.
func _process(_delta: float) -> void:
	if _feed != null and _feed_rate > 0:
		_feed.call(&"advance", _feed_rate)


func _run() -> void:
	Settings.reload()
	Ui.refresh()
	for key: StringName in RESTORED:
		_saved_settings[key] = Settings.get_value(key)
	# First-launch OS flags may have switched on reduce motion; the shots show the default motion.
	Settings.set_value(&"access.reduce_motion", false)
	Settings.set_value(&"access.blink_off", false)
	Screens.start_routing()
	for stage: String in STAGES:
		if not _wants(stage):
			continue
		print("screens_capture: stage ", stage)
		await _stage(stage)
		await _home()
	for key: StringName in _saved_settings:
		Settings.set_value(key, _saved_settings[key])
	print("screens_capture: %d shots, %d failures, in %s" % [_shots, _failures, _out])
	get_tree().quit(1 if _failures > 0 else 0)


func _stage(stage: String) -> void:
	match stage:
		"consent":
			Screens.change(&"consent")
			await _shoot("consent")
		"main_menu":
			await _shoot("main_menu")
		"quit_dialog":
			Screens.open_dialog(&"menu.quit.unsaved_title", &"menu.quit.unsaved_body", [
				{"id": &"quit", "label": &"menu.quit.confirm"},
				{"id": &"stay", "label": &"menu.quit.stay"},
			], &"stay", &"stay")
			await _shoot("quit_dialog")
		"loading":
			Screens.show_loading(&"shell.loading.sortie", 0.4)
			await _shoot("loading")
			Screens.hide_loading()
		"battle":
			await _battle()
		"reference":
			await _reference()
		"dock":
			await _dock()
		"campaign":
			if not _scratch:
				print("screens_capture: campaign skipped: it writes a save; run with a throwaway HOME and --scratch-home")
				return
			await _campaign()
		"lobby":
			if not (_scratch and _loopback):
				print("screens_capture: lobby skipped: run with --loopback-host and --scratch-home")
				return
			await _lobby()
		"settings":
			var node: Node = Screens.push(&"settings")
			if node == null:
				_fail("settings did not open")
				return
			await _shoot("settings")
			for section: StringName in node.get(&"_sections").slice(1):
				node.call(&"_show_section", section)
				await _shoot("settings_%s" % section)
		_:
			if PUSHED.has(stage):
				var screen := stage.trim_suffix("_stock")
				var node: Node = Screens.push(StringName(screen), PUSHED[stage])
				if node == null:
					_fail("%s did not open" % stage)
					return
				await _shoot(stage)


## Back to the main menu with nothing over it and no session.
func _home() -> void:
	_feed_rate = 0
	if _feed != null:
		_feed.call(&"stop")
		_feed.queue_free()
		_feed = null
	if Session.state() != Bp.SESSION_IDLE:
		Session.leave(false)
	if Campaign.is_active():
		Campaign.close_campaign()
	if Editor.is_open():
		Editor.close()
	if Screens.is_loading():
		Screens.hide_loading()
	await _frames(4)
	Screens.close_modals()
	Screens.clear()
	Screens.change(&"main_menu")
	await _frames(4)


# --- shots ----------------------------------------------------------------------------------------

## Palette and UI scale through the real settings, so the settings screen and every reader of
## them show what the shot shows. The player's values come back at the end of the run.
func _apply(palette: StringName, scale: float) -> void:
	var index := maxi(PALETTE_SETTING.find(palette), 0)
	if int(Settings.get_value(&"access.palette")) != index:
		Settings.set_value(&"access.palette", index)
	if not is_equal_approx(float(Settings.get_value(&"access.ui_scale")), scale):
		Settings.set_value(&"access.ui_scale", scale)


func _frames(n: int) -> void:
	for _i: int in range(n):
		await get_tree().process_frame


## Every palette and scale of the current screen. Chart screens name the dusk shot `chart`.
func _shoot(stage: String, chart: bool = false, cvd: bool = false) -> void:
	for palette: String in _palettes:
		for scale: float in _scales:
			_apply(StringName(palette), scale)
			await _frames(SETTLE)
			if not await _drawn():
				_fail("%s: the window drew nothing for 30 frames (covered or minimised?)" % stage)
				continue
			var img := get_viewport().get_texture().get_image()
			var tag := "chart" if chart and palette == "dusk" else palette
			_save(img, "%s_%s_%sx" % [stage, tag, String.num(scale, 2)])
			if cvd and _cvd and palette == "dusk" and is_equal_approx(scale, 1.0):
				_save(Gates.greyscale(img), "%s_greyscale" % stage)
				for kind: String in Gates.CVD:
					_save(Gates.colour_blind(img, kind), "%s_%s" % [stage, kind])
	_apply(&"dusk", 1.0)


func _save(img: Image, stem: String) -> void:
	var path := _out.path_join(stem + ".png")
	if img.save_png(path) != OK:
		_fail("cannot save " + path)
		return
	_shots += 1
	print("screens_capture: saved ", path)


## Wait until `cond` holds, at most LIMIT frames. False on time-out (and a failure is recorded).
## Let a Fit Check finish before the shot, so it shows its verdicts (the seakeeping sweep takes seconds):
## waits while `read` gives a running or not yet started job, up to LIMIT frames, and never fails the stage.
func _fit_settled(read: Callable) -> void:
	for _i: int in range(LIMIT):
		var state := int((read.call() as Dictionary).get("state", Bp.JOB_IDLE))
		if state == Bp.JOB_DONE or state == Bp.JOB_FAILED or state == Bp.JOB_STALE:
			return
		await get_tree().process_frame


func _until(cond: Callable, what: String) -> bool:
	for _i: int in range(LIMIT):
		if bool(cond.call()):
			return true
		await get_tree().process_frame
	_fail("timed out waiting for " + what)
	return false


# --- scripted sorties -----------------------------------------------------------------------------

func _start_feed(script: String) -> bool:
	_feed = ClassDB.instantiate(&"BpTestFeed")
	add_child(_feed)
	if not bool(_feed.call(&"start", script)):
		_fail("BpTestFeed.start(%s) refused" % script)
		return false
	_feed.call(&"advance", 12)
	_feed_rate = 1
	return await _until(func() -> bool: return Session.state() == Bp.SESSION_IN_SORTIE and not Screens.is_loading() and _hud() != null, "the %s sortie" % script)


## Jump the script forward `seconds` (in one-second steps, so every cue reaches the HUD), then let
## the picture settle at the real rate.
func _skip(seconds: float) -> void:
	var rate := _feed_rate
	_feed_rate = 0
	for _i: int in range(int(seconds)):
		_feed.call(&"advance", TICK_HZ)
		await get_tree().process_frame
	_feed_rate = rate
	await _frames(20)


func _hud() -> Node:
	var found := get_tree().root.find_children("SortieHud", "CanvasLayer", true, false)
	return null if found.is_empty() else found[0]


func _battle() -> void:
	if not await _start_feed("battle"):
		return
	# 425 s: the sinking Emergency, two Alarms, a Warning and a Caution are up.
	await _skip(425.0)
	var hud := _hud()
	await _shoot("battle_bridge", false, true)
	hud.call(&"set_view", Bp.VIEW_PLOT)
	await _shoot("battle_plot", false, true)
	hud.call(&"set_view", Bp.VIEW_BRIDGE)
	for station: StringName in [&"damage", &"log", &"networks", &"control"]:
		hud.call(&"open_station", station)
		await _shoot("battle_station_%s" % station)
	hud.call(&"close_station")
	Screens.push(&"pause_menu")
	await _shoot("pause_menu")
	Screens.pop()
	# To the end of the battle: the router pushes the sortie summary.
	await _skip(1_200.0 - 425.0 + 2.0)
	if await _until(func() -> bool: return Screens.top_name() == &"sortie_end", "the sortie summary"):
		await _shoot("sortie_end")


func _reference() -> void:
	if not await _start_feed("reference_battle"):
		return
	await _skip(90.0)
	var hud := _hud()
	await _shoot("reference_bridge", false, true)
	hud.call(&"set_view", Bp.VIEW_PLOT)
	await _shoot("reference_plot", false, true)


# --- dock, campaign, lobby ------------------------------------------------------------------------

func _dock() -> void:
	var issues: Variant = Screens.open_dry_dock(Bp.DOCK_SANDBOX, {"blueprint": STOCK})
	if not issues is Array:
		_fail("the dry dock did not open")
		return
	if await _until(func() -> bool: return Screens.base_name() == &"dry_dock" and Editor.is_open(), "the dry dock"):
		await _frames(60)
		await _shoot("dry_dock")
		await _dock_states(Screens.top(), "dry_dock")


## Every tab of both docks and the graph and script modes, each as its own state.
func _dock_states(dock: Node, stem: String) -> void:
	if dock == null:
		return
	for side: StringName in [&"_left_tabs", &"_right_tabs"]:
		var tabs: TabContainer = dock.get(side)
		if tabs == null:
			continue
		var first := tabs.current_tab
		for i: int in range(tabs.get_tab_count()):
			if i == first or tabs.is_tab_hidden(i):
				continue
			tabs.current_tab = i
			await _shoot("%s_%s" % [stem, tabs.get_tab_title(i).to_lower().replace(" ", "_")])
		tabs.current_tab = first
	var modes: Array = dock.get(&"_mode_buttons")
	for i: int in range(1, modes.size()):
		var b := modes[i] as Button
		if b != null and b.is_visible_in_tree() and not b.disabled:
			b.pressed.emit()
			await _frames(10)
			await _shoot("%s_mode_%s" % [stem, String(b.name).to_snake_case()])
	if not modes.is_empty() and modes[0] is Button:
		(modes[0] as Button).pressed.emit()


func _campaign() -> void:
	var problems: Array = Campaign.new_campaign({"slot_name": "Capture", "seed": 1749, "ironman": false, "starter": STARTER, "tutorial_done": true})
	if not problems.is_empty():
		_fail("new_campaign refused: %s" % str(problems))
		return
	if not await _until(func() -> bool: return Screens.base_name() == &"chart", "the chart table"):
		return
	await _frames(30)
	await _shoot("chart_table", true)
	var chart_tabs: TabContainer = Screens.top().get(&"_tabs")
	if chart_tabs != null:
		for i: int in range(1, chart_tabs.get_tab_count()):
			if chart_tabs.is_tab_hidden(i):
				continue
			chart_tabs.current_tab = i
			await _shoot("chart_table_tab_%d" % i, true)
		chart_tabs.current_tab = 0
	var offers: Array = (Campaign.chart() as Dictionary).get("offers", [])
	if offers.is_empty():
		_fail("a new campaign has no operation on offer")
		return
	var op := StringName((offers[0] as Dictionary).get("id", &""))
	if not Campaign.open_briefing(op):
		_fail("open_briefing(%s) refused" % op)
		return
	if not await _until(func() -> bool: return Screens.base_name() == &"briefing", "the briefing"):
		return
	await _fit_settled(func() -> Dictionary: return Campaign.briefing_fit())
	await _shoot("briefing")
	if Campaign.open_dry_dock():
		if await _until(func() -> bool: return Screens.base_name() == &"dry_dock" and Editor.is_open(), "the campaign dry dock"):
			await _frames(60)
			await _shoot("dry_dock_campaign")
			var right: TabContainer = Screens.top().get(&"_right_tabs")
			if right != null:
				for i: int in range(right.get_tab_count()):
					if not right.is_tab_hidden(i) and right.get_tab_title(i).to_lower().contains("fit"):
						right.current_tab = i
						await _fit_settled(func() -> Dictionary: return Editor.fit_result())
						await _shoot("dry_dock_campaign_fit_check")
		# Leave the dock the way its Leave button does once nothing is unsaved: back to the briefing.
		var dock: Node = Screens.top()
		if dock != null and dock.has_method(&"_close_and_route"):
			dock.call(&"_close_and_route")
		await _frames(10)
		if Screens.base_name() != &"briefing":
			Campaign.open_briefing(op)
		await _until(func() -> bool: return Screens.base_name() == &"briefing", "the briefing after the dock")
	if not Campaign.launch_sortie():
		_fail("launch_sortie refused")
		return
	if not await _until(func() -> bool: return Session.state() == Bp.SESSION_IN_SORTIE and not Screens.is_loading() and _hud() != null, "the campaign sortie"):
		return
	await _frames(240)
	await _shoot("campaign_sortie")
	var check: Object = ClassDB.instantiate(&"BpSessionCheck")
	if not bool(check.call(&"stop_sortie", Bp.OUTCOME_WITHDRAWN)):
		_fail("stop_sortie was refused")
		return
	if await _until(func() -> bool: return Screens.base_name() == &"after_action", "the after-action report"):
		await _frames(30)
		await _shoot("after_action")


func _lobby() -> void:
	var previous: String = Lobby.display_name()
	var mp: Node = Screens.push(&"multiplayer")
	await _frames(10)
	mp.set(&"firewall_seen", true)
	(mp.get(&"_name_field") as LineEdit).text = "Halyard"
	mp.call(&"_refresh_buttons")
	mp.call(&"_on_host")
	if await _until(func() -> bool: return Screens.base_name() == &"lobby", "the lobby"):
		await _frames(60)
		await _shoot("lobby")
		var panel: Node = Screens.push(&"host_panel")
		if panel != null:
			await _shoot("host_panel")
	Lobby.leave()
	Lobby.set_display_name(previous)


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
