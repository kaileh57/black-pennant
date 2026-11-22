extends Node
## Windowed captures of the own-ship, weapons and damage-control HUD panels (units ui-hud-ship,
## ui-hud-weapons, ui-hud-damage). Runs the real Screens routing over a scripted BpTestFeed picture
## (or a live skirmish), jumps the script to a moment, opens a station and shoots the screen:
##   godot --path game --resolution 1600x900 res://test/capture/u8_capture.tscn -- \
##     --mode=scripted|skirmish --script=battle --at=400 --station=damage|networks --inject=1 \
##     --aim=1 --pacing=held|scale|paused --palette=dusk --scale=1 --frames=90 --name=x --out=/abs/dir
## --inject=1 rewrites the station's data with states the scripts do not produce (cut and
## jury-rigged conduits, buffers, local control, a flooded magazine, a pinned team) through the
## panels' own `show_state` entry points. --aim=1 shows the reticle with a readout. --pacing shows
## a held compression request, an overload time scale or the paused state.
## Headless, or when instanced by check_project, it does nothing.

const STARTER := "starters/fac22.bp.ron"
const LIMIT := 4000
const TICK_HZ := 60

var _mode := "scripted"
var _script := "battle"
var _at := 0.0
var _station := ""
var _inject := false
var _aim := false
var _pacing := ""
var _scroll := 0
var _checks := false
var _clicks := false
var _tab := -1
var _steps: Array[Callable] = []
var _step := 0
var _expect_cmd := ""
var _step_what := ""
var _focus_seen: Array[String] = []
var _failures := 0
var _palette := "dusk"
var _scale := 1.0
var _frames := 90
var _name := "u8"
var _out := "/tmp"
var _feed: Node = null
var _frame := 0
var _sortie_frame := -1
var _active := false
var _jumped := false


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
			"at": _at = float(kv[1])
			"station": _station = kv[1]
			"inject": _inject = kv[1] == "1"
			"aim": _aim = kv[1] == "1"
			"pacing": _pacing = kv[1]
			"scroll": _scroll = int(kv[1])
			"checks": _checks = kv[1] == "1"
			"clicks": _clicks = kv[1] == "1"
			"tab": _tab = ["power", "fuel", "data"].find(kv[1])
			"palette": _palette = kv[1]
			"scale": _scale = float(kv[1])
			"frames": _frames = int(kv[1])
			"name": _name = kv[1]
			"out": _out = kv[1]
	DirAccess.make_dir_recursive_absolute(_out)
	Settings.reload()
	Ui.refresh()
	Ui.call(&"_apply_palette", StringName(_palette))
	Ui.call(&"_apply_scale", _scale)
	Screens.start_routing()
	if _mode == "scripted":
		_feed = ClassDB.instantiate(&"BpTestFeed")
		add_child(_feed)
		if not bool(_feed.call(&"start", _script)):
			push_error("u8_capture: the test feed refused " + _script)
			get_tree().quit(1)
			return
		_feed.call(&"advance", 12)
	else:
		var problems := Session.start_skirmish({
			"template": Bp.MISSION_CONVOY_RAID, "act": Bp.ACT_1, "enemy_per_player": 3, "difficulty": {"preset": "normal"},
			"weather": Bp.WEATHER_CLEAR, "sea_state": 3, "time_of_day": Bp.TIME_DUSK, "seed": 1234, "blueprint": STARTER,
		})
		if not problems.is_empty():
			push_error("u8_capture: start_skirmish refused: " + str(problems))
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
		push_error("u8_capture: ran out of frames")
		_end(1)
		return
	if _sortie_frame < 0:
		if Session.state() == Bp.SESSION_IN_SORTIE and not Screens.is_loading():
			_sortie_frame = _frame
		elif Session.state() == Bp.SESSION_FAILED:
			push_error("u8_capture: the session failed")
			_end(1)
		return
	var since := _frame - _sortie_frame
	var hud: Node = Screens.top()
	if hud == null or not hud.has_method(&"open_station"):
		if since > 120:
			push_error("u8_capture: no HUD on the stack")
			_end(1)
		return
	if since == 5 and _feed != null and _at > 0.0 and not _jumped:
		_jumped = true
		_feed.call(&"advance", int(_at * TICK_HZ))
	if _checks:
		_live_checks(hud, since)
	if _clicks and since >= 60:
		if _click_checks(hud, since):
			return
	if since == _frames - 30:
		if not _station.is_empty():
			hud.call(&"open_station", StringName(_station))
		if _pacing == "paused":
			Session.set_paused(true)
	if since == _frames - 20:
		_apply_injections(hud)
		if _tab >= 0 and _station == "networks":
			hud.get_node("Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Body/Panels/Networks").call(&"_on_tab", _tab)
	if since == _frames - 5 and _scroll > 0 and not _station.is_empty():
		var panel := hud.get_node_or_null("Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Body/Panels/" + _station.capitalize())
		var sc: ScrollContainer = panel.get_node_or_null(^"Scroll") if panel != null else null
		if sc != null:
			sc.scroll_vertical = _scroll
	if since == _frames:
		var path := _out.path_join(_name + ".png")
		get_viewport().get_texture().get_image().save_png(path)
		print("u8_capture: saved ", path)
		if _checks:
			print("u8_capture: live checks, %d failures" % _failures)
		_end(1 if _failures > 0 else 0)


## Every order button of the three units, pressed through its own signal in a scripted battle; the
## test feed's command log says whether the right command left. Then a keyboard walk through the
## open damage station. Returns true while it still runs (the shot waits for it).
func _click_checks(hud: Node, since: int) -> bool:
	if _steps.is_empty() and _step == 0:
		_build_steps(hud)
		_feed.call(&"take_commands")
	if _step > _steps.size():
		return false
	if (since - 60) % 8 != 0:
		return true
	if not _expect_cmd.is_empty() or not _step_what.is_empty():
		var cmds: PackedStringArray = _feed.call(&"take_commands")
		var joined := " | ".join(cmds)
		if _expect_cmd == "-":
			_check(cmds.is_empty(), _step_what + " sends nothing: " + joined)
		else:
			_check(joined.contains(_expect_cmd), _step_what + " sends " + _expect_cmd + ": " + joined)
		_expect_cmd = ""
		_step_what = ""
	if _step >= _steps.size():
		_step += 1
		print("u8_capture: focus walk ", " > ".join(_focus_seen))
		_frames = since + 10
		return false
	var c: Callable = _steps[_step]
	_step += 1
	c.call()
	return true


func _press(b: Variant, what: String, cmd: String) -> void:
	if b is BaseButton and is_instance_valid(b) and (b as BaseButton).is_visible_in_tree() and not (b as BaseButton).disabled:
		(b as BaseButton).pressed.emit()
		_expect_cmd = cmd
		_step_what = what
	else:
		print("u8_capture: SKIP ", what, " (no enabled control in this script)")


func _build_steps(hud: Node) -> void:
	var stack := "Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Body/Panels/"
	var dc: Node = hud.get_node(stack + "Damage")
	var np: Node = hud.get_node(stack + "Networks")
	var bottom := "Frame/Docks/Centre/Margin/Column/Bottom/Row/"
	var helm: Node = hud.get_node(bottom + "HelmCluster")
	var weapons: Node = hud.get_node("Frame/Docks/RightDock/Column/WeaponsPanel")
	var bar: Node = hud.get_node(^"Frame/Docks/RightDock/Column/PacingBar")
	_steps = [
		func() -> void: hud.call(&"open_station", &"damage"),
		func() -> void: _press(dc.get(&"_d_fire"), "Send a team to the fire", "FightFire"),
		func() -> void:
			# The flooding compartment: the deepest water.
			var best := -1
			var fill := 0.0
			for id: int in (dc.get(&"_live_by_id") as Dictionary):
				var f := float((dc.get(&"_live_by_id") as Dictionary)[id].get("fill", 0.0))
				if f > fill:
					fill = f
					best = id
			if best >= 0:
				dc.call(&"_on_picked", best),
		func() -> void: _press(dc.get(&"_d_patch"), "Send a team to patch", "Patch"),
		func() -> void: _press(dc.get(&"_d_valve"), "Open the flood valve", "FloodValve"),
		func() -> void:
			var rows: Dictionary = dc.get(&"_part_rows")
			var hit: Variant = null
			for slot: int in rows:
				var b: Button = rows[slot]["repair"]
				if b.visible and not b.disabled:
					hit = b
			_press(hit, "Repair a part", "Repair"),
		func() -> void: _press((dc.get(&"_order_rows") as Array)[2]["up"], "Move a repair priority up", "ReorderRepairs"),
		func() -> void: _press((dc.get(&"_order_rows") as Array)[0]["down"], "Move a repair priority down", "ReorderRepairs"),
		func() -> void: _press(_equip(dc, "pump"), "Run a pump", "Pump"),
		func() -> void: _press(_equip(dc, "door"), "Close or open a door", "Door"),
		func() -> void:
			var box: Node = dc.get(&"_d_mags")
			var b: Variant = box.get_child(0) if box.get_child_count() > 0 else null
			if b == null:
				for id: int in (dc.get(&"_static_by_id") as Dictionary):
					if bool((dc.get(&"_static_by_id") as Dictionary)[id].get("has_magazine", false)):
						dc.call(&"_on_picked", id)
				b = box.get_child(0) if box.get_child_count() > 0 else null
			_press(b, "Flood magazine asks first", "-"),
		func() -> void:
			var focus := get_viewport().gui_get_focus_owner()
			print("u8_capture: dialog focus ", focus.text if focus is Button else str(focus))
			_check(focus is Button and (focus as Button).text == tr(&"hud_damage.magazine.keep"), "the flood dialog focuses Keep the magazine")
			Screens.close_modals()
			_step_what = "Closing the dialog without a choice"
			_expect_cmd = "-",
		func() -> void:
			var box: Node = dc.get(&"_d_mags")
			var b: Variant = box.get_child(0) if box.get_child_count() > 0 else null
			_press(b, "Flood magazine dialog opens again", "-"),
		func() -> void:
			var focus := get_viewport().gui_get_focus_owner()
			var dialog: Node = focus
			while dialog != null and not dialog.has_method(&"choose"):
				dialog = dialog.get_parent()
			if dialog != null:
				dialog.call(&"choose", &"flood")
				_expect_cmd = "FloodMagazine"
				_step_what = "Choosing Flood the magazine"
			else:
				_check(false, "the flood dialog is open"),
		func() -> void: _focus_walk(dc),
		func() -> void: hud.call(&"open_station", &"networks"),
		func() -> void:
			var rows: Dictionary = np.get(&"_priority_rows")
			var hit: Variant = null
			for slot: int in rows:
				if not (rows[slot]["raise"] as Button).disabled:
					hit = rows[slot]["raise"]
			_press(hit, "Shed a consumer later", "SetPowerPriority"),
		func() -> void: _press((np.get(&"_tab_buttons") as Array)[1], "Show the fuel network", "-"),
		func() -> void: _press((np.get(&"_tab_buttons") as Array)[2], "Show the data network", "-"),
		func() -> void: hud.call(&"close_station"),
		func() -> void: _press(bar.get_node(^"Margin/Column/Row/Rates/Rate2"), "Ask for 2x", "RequestTimeRate"),
		func() -> void: _press(helm.get_node(^"Margin/Column/Header/Take"), "Take the helm", "TakeControl"),
		func() -> void:
			var rows: Dictionary = weapons.get(&"_group_rows")
			_press(rows[rows.keys()[0]]["take"] if not rows.is_empty() else null, "Take a weapons group", "TakeControl"),
		func() -> void:
			var tracks: Array = Hud.tracks()
			if not tracks.is_empty():
				Hud.select_track(int(tracks[0].get("id", -1)))
				weapons.call(&"refresh"),
		func() -> void:
			var hit: Variant = null
			var decoy: Variant = null
			var rows: Dictionary = weapons.get(&"_mount_rows")
			for slot: int in rows:
				if rows[slot]["launch"] != null:
					hit = rows[slot]["launch"]
				if rows[slot]["decoy"] != null:
					decoy = rows[slot]["decoy"]
			_press(hit, "Launch at the selected track", "Launch")
			if decoy != null:
				_steps.insert(_step, func() -> void: _press(decoy, "Launch decoys", "FireDecoy")),
	]


func _equip(dc: Node, kind: String) -> Variant:
	var rows: Dictionary = dc.get(&"_equip_rows")
	for k: String in rows:
		if k.begins_with(kind + ":"):
			return rows[k]["button"]
	return null


## Down through the damage station from its first control; the focus must move every step until
## the end of the chain, and the scroll must follow it.
func _focus_walk(dc: Node) -> void:
	var first: Control = Screens.first_focusable(dc)
	if first == null:
		_check(false, "the damage station has a focusable control")
		return
	first.grab_focus()
	var seen: Array[Control] = [first]
	for i in 60:
		var current := get_viewport().gui_get_focus_owner()
		if current == null:
			break
		var next := current.find_valid_focus_neighbor(SIDE_BOTTOM)
		if next == null or next == current:
			break
		next.grab_focus()
		seen.append(next)
	for c: Control in seen:
		_focus_seen.append(c.name if not (c is Button) else (c as Button).text)
	var unique: Array[Control] = []
	for c: Control in seen:
		if not unique.has(c):
			unique.append(c)
	_check(seen.size() >= 5 and unique.size() == seen.size(), "Down walks the damage station without looping (%d stops)" % seen.size())


## Live order checks through the panels' own buttons (the pointer path): take the helm and weapons
## group 1, see the reticle come up over the view and aim through the world view, then pause.
func _live_checks(hud: Node, since: int) -> void:
	var bottom := "Frame/Docks/Centre/Margin/Column/Bottom/Row/"
	var helm := hud.get_node(bottom + "HelmCluster")
	var weapons := hud.get_node("Frame/Docks/RightDock/Column/WeaponsPanel")
	var reticle := hud.get_node(^"Frame/View/Reticle") as Control
	var bar := hud.get_node(^"Frame/Docks/RightDock/Column/PacingBar")
	if since == _frames - 120:
		(helm.get_node(^"Margin/Column/Header/Take") as Button).pressed.emit()
		var rows: Dictionary = weapons.get(&"_group_rows")
		if rows.is_empty():
			_check(false, "the weapons panel lists no group")
		else:
			(rows[rows.keys()[0]]["take"] as Button).pressed.emit()
		Input.warp_mouse(get_viewport().get_visible_rect().size * Vector2(0.52, 0.45))
	if since == _frames - 60:
		_check(int(Hud.role_holder(Bp.ROLE_HELM, 0)) == Bp.HOLDER_PLAYER, "Take the helm hands the helm to the player")
		var holder := (helm.get_node(^"Margin/Column/Header/Holder") as Label).text
		_check(holder == tr(&"hud_ship.holder.player"), "the helm cluster says Player: " + holder)
		_check(reticle.is_processing(), "holding a weapons group brings the reticle up")
		var w: Node = Screens.world_view()
		var readout: Dictionary = w.call(&"aim_readout") if w != null else {}
		print("u8_capture: aim readout ", readout)
		_check(readout.has("valid"), "the world view answers the aim readout")
		(bar.get_node(^"Margin/Column/Row/Pause") as Button).pressed.emit()
	if since == _frames - 25:
		_check(bool(Session.pacing().get("paused", false)), "Pause pauses a single-player sortie")


func _check(ok: bool, what: String) -> void:
	print("u8_capture: ", "PASS " if ok else "FAIL ", what)
	if not ok:
		_failures += 1


func _apply_injections(hud: Node) -> void:
	var stack := "Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Body/Panels/"
	if _inject and _station == "damage":
		var dc := hud.get_node_or_null(stack + "Damage")
		if dc != null:
			var state: Dictionary = (dc.get(&"_state") as Dictionary).duplicate(true)
			_inject_damage(state)
			dc.call(&"show_state", state)
	if _inject and _station == "networks":
		var np := hud.get_node_or_null(stack + "Networks")
		if np != null:
			var state: Dictionary = (np.get(&"_state") as Dictionary).duplicate(true)
			_inject_networks(state)
			np.call(&"show_state", state)
	if _aim:
		var reticle := hud.get_node_or_null(^"Frame/View/Reticle")
		if reticle != null:
			reticle.set_process(false)
			reticle.call(&"show_aim", {"valid": true, "map": Vector2.ZERO, "range_m": 3650.0, "bearing_deg": 47.0, "track": 2, "in_arc": true, "time_of_flight_s": 6.4}, true, (reticle as Control).size * Vector2(0.55, 0.42))
	if _pacing == "held" or _pacing == "scale":
		var bar := hud.get_node_or_null(^"Frame/Docks/RightDock/Column/PacingBar")
		if bar != null:
			var info: Dictionary = Session.info()
			var p: Dictionary = Session.pacing()
			if _pacing == "held":
				p["requested"] = [{"player": int(info.get("player", 0)), "rate": 4}]
				p["rate"] = 1
				p["blocked"] = Bp.BLOCK_THREAT_IN_OWN_ENVELOPE
				p["blocked_text"] = {"key": &"sim.compress.threat_in_own_envelope", "args": []}
			else:
				p["time_scale"] = 0.7
				p["rung"] = 4
				p["sub_step"] = 2
				p["host_tick_ms"] = 9.4
			bar.call(&"show_pacing", p, info)


func _inject_damage(state: Dictionary) -> void:
	var live: Array = state.get("live", [])
	var statics: Array = state.get("statics", [])
	if statics.size() >= 3:
		# A fire beside a breach, a door left open.
		for c: Dictionary in live:
			var id := int(c["id"])
			if id == int(statics[1]["id"]):
				c["fire"] = 0.7
			if id == int(statics[2]["id"]):
				c["fill"] = 0.62
				c["breached"] = true
				c["sealed"] = false
	var parts: Array = state.get("parts", [])
	var levels := [Bp.DAMAGE_DAMAGED, Bp.DAMAGE_HEAVY, Bp.DAMAGE_DESTROYED]
	for i in mini(parts.size(), 9):
		var p: Dictionary = parts[i * 3 % parts.size()]
		p["level"] = levels[i % 3]
		p["health"] = [0.55, 0.2, 0.0][i % 3]
	var crew: Dictionary = state.get("crew", {})
	var teams: Array = crew.get("teams", [])
	if not teams.is_empty() and statics.size() >= 2:
		var t: Dictionary = teams[0]
		t["kind"] = Bp.TEAM_FIGHT_FIRE
		t["target"] = int(statics[1]["id"])
		t["progress"] = 0.4
		t["pinned"] = true
		t["text"] = {"key": &"hud.dc.team.fire", "args": [{"compartment": int(statics[1]["id"])}]}
	for row: Dictionary in state.get("controls", []):
		if StringName(row.get("kind", &"")) == &"magazine_flood":
			row["on"] = true
			row["usable"] = false
			break
	var summary: Dictionary = state.get("summary", {})
	summary["fires"] = 1
	summary["flooding"] = 1
	summary["breached"] = 1
	summary["parts_damaged"] = 3
	summary["parts_heavy"] = 3
	summary["parts_destroyed"] = 3
	summary["heel_deg"] = -4.0


func _inject_networks(state: Dictionary) -> void:
	for net: Dictionary in state.get("networks", []):
		var conduits: Array = net.get("conduits", [])
		for i in conduits.size():
			var c: Dictionary = conduits[i]
			if i % 5 == 1:
				c["cut"] = true
			elif i % 7 == 3:
				c["jury_rigged"] = true
		net["consumers_dead"] = 1
		net["consumers_on_buffer"] = 2
	var parts: Array = state.get("parts", [])
	var links := [Bp.LINK_BUFFER, Bp.LINK_LOCAL_CONTROL, Bp.LINK_DEAD, Bp.LINK_REDUNDANT]
	for i in mini(parts.size(), 4):
		var p: Dictionary = parts[i * 2]
		p["power"] = links[i]
		p["data"] = links[(i + 1) % 4]
	if parts.size() > 9:
		(parts[9] as Dictionary)["shed"] = true


func _end(code: int) -> void:
	_active = false
	if _feed != null:
		_feed.call(&"stop")
	else:
		Session.leave(false)
	get_tree().quit(code)
