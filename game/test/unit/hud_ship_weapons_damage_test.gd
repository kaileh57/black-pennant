extends SceneTree
## Headless checks for the own-ship cluster, pacing bar, objectives, weapons panel, reticle, damage
## control and networks panels, on hand-built data. Units: ui-hud-ship, ui-hud-weapons,
## ui-hud-damage. Run from game/:
##   godot --headless --path . -s res://test/unit/hud_ship_weapons_damage_test.gd
## Exits 1 on any failure. No session runs; the facades answer as when idle. Windowed captures of
## the same panels with live and scripted data are res://test/capture/u8_capture.tscn.

## Loaded at run time: the panels compile against the autoloads, which a preload here would beat.
var Pacing: GDScript
var Helm: GDScript
var Objectives: GDScript
var Weapons: GDScript
var Reticle: GDScript
var Damage: GDScript
var Networks: GDScript

## `Gauge.State.CAUTION`.
const GAUGE_CAUTION := 2

var _failures := 0
var _checks := 0
var _frame := 0
var _nodes: Dictionary[String, Node] = {}


func _process(_delta: float) -> bool:
	_frame += 1
	if _frame == 2:
		Pacing = load("res://ui/hud/pacing_bar.gd")
		Helm = load("res://ui/hud/helm_cluster.gd")
		Objectives = load("res://ui/hud/objectives.gd")
		Weapons = load("res://ui/hud/weapons_panel.gd")
		Reticle = load("res://ui/hud/reticle.gd")
		Damage = load("res://ui/hud/damage_control.gd")
		Networks = load("res://ui/hud/networks_panel.gd")
		for name: String in ["pacing_bar", "helm_cluster", "objectives", "weapons_panel", "reticle", "damage_control", "networks_panel"]:
			var n := (load("res://ui/hud/%s.tscn" % name) as PackedScene).instantiate()
			if n is Control:
				(n as Control).size = Vector2(640, 600)
			root.add_child(n)
			_nodes[name] = n
	if _frame == 4:
		_pacing_checks()
		_helm_checks()
		_objective_checks()
		_weapon_checks()
		_reticle_checks()
		_damage_checks()
		_network_checks()
		for n: Node in _nodes.values():
			n.queue_free()
	if _frame < 6:
		return false
	print("hud_ship_weapons_damage_test: %d checks, %d failures" % [_checks, _failures])
	quit(1 if _failures > 0 else 0)
	return true


func _expect(ok: bool, what: String) -> void:
	_checks += 1
	if not ok:
		_failures += 1
		push_error("hud_ship_weapons_damage_test: " + what)


func _t(key: StringName) -> String:
	return String(TranslationServer.translate(key))


# --- pacing --------------------------------------------------------------------------------------

func _pacing_checks() -> void:
	var p := {"paused": false, "rate": 1, "requested": [{"player": 0, "rate": 4}, {"player": 1, "rate": 1}], "blocked": Bp.BLOCK_WEAPON_INBOUND,
		"blocked_text": {"key": &"sim.compress.weapon_inbound", "args": []}, "time_scale": 1.0, "rung": 0, "sub_step": 0, "host_tick_ms": 4.0, "can_pause": true, "can_compress": false}
	var info := {"state": Bp.SESSION_IN_SORTIE, "kind": Bp.KIND_SKIRMISH, "net": Bp.NET_OFFLINE, "player": 0}
	_expect(Pacing.requested_by(p, 0, 1) == 4, "the player's own request is read from the list")
	_expect(Pacing.requested_by(p, 7, 2) == 2, "a player the list does not name asks for the effective rate")
	_expect(Pacing.held_text(p, info, 4, 1) == _t(&"sim.compress.weapon_inbound"), "a held request shows the server's reason as it is")
	_expect(Pacing.held_text(p, info, 1, 1).is_empty(), "nothing is held when the request is met")
	var no_reason := p.duplicate()
	no_reason["blocked"] = Bp.NONE
	no_reason["blocked_text"] = {}
	_expect(Pacing.held_text(no_reason, info, 4, 1).contains(_t(&"hud_ship.held.player_n").format(["2"])), "in co-op the player asking for less is named (L114)")
	no_reason["requested"] = [{"player": 0, "rate": 2}]
	_expect(Pacing.held_text(no_reason, info, 2, 1) == _t(&"hud_ship.held.plain").format([Pacing.rate_text(1.0, 0)]), "a held request with no reason still says it is held")
	_expect(Pacing.rate_text(0.7, 1).begins_with("0") and Pacing.rate_text(0.7, 1).ends_with("x"), "rates read as a number and x")
	_expect(Pacing.rung_text(0, 0).is_empty(), "rung 0 says nothing (quiet when normal)")
	_expect(Pacing.rung_text(2, 1).contains(_t(&"hud_ship.rung.kinematic_all")), "rung 2 step 2 names hulls simplified at any range")
	_expect(Pacing.rung_text(3, 9).contains(_t(&"hud_ship.rung.projectiles_1000")), "an out-of-range sub-step clamps to the last text")
	_expect(Pacing.rung_text(9, 0).is_empty(), "an unknown rung says nothing")
	var bar: Node = _nodes["pacing_bar"]
	bar.call(&"show_pacing", p, info)
	_expect((bar.get_node(^"Margin/Column/Held") as Control).visible, "the held line shows while the request is held")
	_expect((bar.get_node(^"Margin/Column/Row/Rates/Rate4") as Button).button_pressed, "the 4x toggle shows the player's request")
	_expect(not (bar.get_node(^"Margin/Column/Scale") as Control).visible, "no time-scale line at 1.0")
	var slow := p.duplicate()
	slow["time_scale"] = 0.6
	slow["rung"] = 4
	bar.call(&"show_pacing", slow, info)
	_expect((bar.get_node(^"Margin/Column/Scale") as Control).visible, "an overload time scale always shows (L1022)")
	_expect((bar.get_node(^"Margin/Column/Rung") as Control).visible, "a stepped-down ladder shows its rung")
	_expect((bar.get_node(^"Margin/Column/Tick") as Control).visible, "single-player shows the host tick while the ladder is stepped down")
	var pvp := info.duplicate()
	pvp["kind"] = Bp.KIND_PVP
	var pvp_pacing := p.duplicate()
	pvp_pacing["can_pause"] = false
	bar.call(&"show_pacing", pvp_pacing, pvp)
	_expect(not (bar.get_node(^"Margin/Column/Row/Rates") as Control).visible and not (bar.get_node(^"Margin/Column/Row/Pause") as Control).visible, "PvP has neither pause nor compression")
	_expect((bar.get_node(^"Margin/Column/Row/Effective") as Control).visible, "the effective rate always shows")
	var paused := p.duplicate()
	paused["paused"] = true
	bar.call(&"show_pacing", paused, info)
	_expect((bar.get_node(^"Margin/Column/Row/Rates/Rate2") as Button).disabled, "rate buttons rest while paused")
	_expect((bar.get_node(^"Margin/Column/Row/Pause") as Button).text == _t(&"hud_ship.pause.resume"), "Pause becomes Resume while paused")
	bar.call(&"show_pacing", {}, {"state": Bp.SESSION_IDLE})
	_expect(not (bar as Control).visible, "the bar hides outside a sortie")


# --- helm ----------------------------------------------------------------------------------------

func _helm_checks() -> void:
	var meter: GDScript = Helm.get(&"RateMeter")
	var m: Object = meter.new()
	m.feed(10.0, 1.0, false)
	var r: float = m.feed(12.0, 2.0, false)
	_expect(is_equal_approx(r, 2.0), "the first rate is the plain difference over sortie time")
	m.feed(12.0, 2.0, false)
	_expect(is_equal_approx(float(m.get(&"rate")), 2.0), "a paused reading (no time passed) keeps the rate")
	var h: Object = meter.new()
	h.feed(359.0, 0.0, true)
	_expect(is_equal_approx(h.feed(1.0, 1.0, true), 2.0), "headings wrap through north")
	var g: Object = meter.new()
	g.feed(1.0, 10.0, false)
	_expect(is_nan(g.feed(2.0, 30.0, false)), "a gap of more than 5 s drops the rate")
	_expect(Helm.heading_line(NAN, 0.0) == _t(&"hud_ship.rudder.amidships"), "rudder amidships")
	_expect(Helm.heading_line(90.0, -0.5).contains(_t(&"hud_ship.rudder.port").format([""]).strip_edges().split(" ")[0]), "port rudder is named")
	var route: String = Helm.route_line({"pos": Vector2.ZERO, "route": PackedVector2Array([Vector2(1000, 0), Vector2(2000, 0)])})
	_expect(route.contains("090") and route.contains("2"), "the route line gives the next waypoint's bearing east as 090: " + route)
	_expect(Helm.route_line({"route": PackedVector2Array()}).is_empty(), "no route, no line")
	_expect(Helm.status_key(Bp.VESSEL_ACTIVE) == &"" and Helm.status_key(Bp.VESSEL_LOST) == &"hud_ship.status.lost", "status words")
	_expect(Helm.power_line(800.0, 1000.0, 0, 800.0).contains("1"), "power line names generation")
	_expect(Helm.power_line(600.0, 1000.0, 2, 900.0) != Helm.power_line(900.0, 1000.0, 2, 900.0), "a short supply reads differently")
	var helm: Node = _nodes["helm_cluster"]
	helm.call(&"show_limits", {"max_speed_kn": 30.0, "astern_speed_kn": 18.0, "heel_caution_deg": 25.0, "heel_limit_deg": 50.0})
	helm.call(&"show_motion", {"vessel": 5, "pos": Vector2.ZERO, "speed_kn": 12.0, "heading_deg": 47.0, "heel_deg": -30.0, "trim_deg": 0.2, "ordered_speed_kn": 20.0, "ordered_heading_deg": 60.0, "rudder": 0.3, "route": PackedVector2Array(), "status": Bp.VESSEL_ACTIVE}, 3.0)
	var speed: Object = helm.get(&"_speed")
	var heel: Object = helm.get(&"_heel")
	_expect(is_equal_approx(float(speed.get(&"value")), 12.0) and is_equal_approx(float(speed.get(&"max_value")), 33.0) and is_equal_approx(float(speed.get(&"min_value")), -18.0), "speed reads against astern and top speed")
	_expect(int(heel.call(&"state")) == GAUGE_CAUTION, "30 degrees of heel is past the 25 degree caution")
	_expect(heel.get(&"label") == &"hud_ship.gauge.heel_port", "negative heel is to port")
	# Folded, heel, trim, fuel and power stay out of the way unless one is past its caution limit.
	var trim: Control = helm.get(&"_trim")
	_expect(not bool(helm.get(&"expanded")) and (heel as Control).get_parent().visible and not trim.get_parent().visible, "folded, the heel past caution shows and the level trim does not")
	(helm.get_node(^"Margin/Column/Header/More") as Button).button_pressed = true
	_expect(trim.get_parent().visible, "opened, every secondary gauge shows")
	(helm.get_node(^"Margin/Column/Header/More") as Button).button_pressed = false
	helm.call(&"show_power", {"generation_kw": 1000.0, "demand_kw": 950.0, "supplied_kw": 950.0, "fuel_l": 500.0, "fuel": 0.15, "fuel_flow_lps": 1.0, "shed_count": 0})
	var fuel: Object = helm.get(&"_fuel")
	var power: Object = helm.get(&"_power")
	_expect(int(fuel.call(&"state")) == GAUGE_CAUTION, "15% fuel is past the 20% caution")
	_expect(float(fuel.get(&"rate")) < 0.0, "burning fuel trends down")
	_expect(int(power.call(&"state")) == GAUGE_CAUTION, "95% of generation is past the 90% caution")
	helm.call(&"show_motion", {}, NAN)
	_expect((helm.get_node(^"Margin/Column/Empty") as Control).visible and not (helm.get_node(^"Margin/Column/Scroll/Gauges/Grid") as Control).visible, "no hull shows the empty state")


# --- objectives ----------------------------------------------------------------------------------

func _objective_checks() -> void:
	var primary := {"id": 1, "ghost": false, "state": Bp.OBJECTIVE_ACTIVE, "text": {"key": &"hud_ship.objective.unnamed", "args": []}, "progress": 0.4, "marker": Vector2(100, 200)}
	var ghost := {"id": 2, "ghost": true, "state": Bp.OBJECTIVE_FAILED, "text": {"key": &"hud_ship.objective.unnamed", "args": []}, "progress": 0.0, "marker": null}
	_expect(Objectives.objective_text(primary).contains("40"), "a partly met objective gives its progress")
	_expect(Objectives.objective_text(ghost) == _t(&"hud_ship.objective.failed").format([_t(&"hud_ship.objective.ghost").format([_t(&"hud_ship.objective.unnamed")])]), "a failed ghost condition says both")
	_expect(Objectives.glyph_for(Bp.OBJECTIVE_DONE) == &"check_pass" and Objectives.glyph_for(Bp.OBJECTIVE_ACTIVE) == &"", "only done and failed carry a glyph")
	var ally := {"vessel": 9, "player": 1, "name": "Kestrel", "speed_kn": 14.2, "cells_lost": 0.08, "burning": true, "flooding": false, "emcon": Bp.EMCON_SILENT, "officers_in_command": true, "pos": Vector2(5, 5), "heading_deg": 10.0}
	var at: String = Objectives.ally_text(ally)
	_expect(at.begins_with("Kestrel") and at.contains(_t(&"hud_ship.allies.fire")) and at.contains(_t(&"hud_ship.allies.officers")), "the ally row names the hull and its state: " + at)
	var panel: Node = _nodes["objectives"]
	panel.call(&"show_lists", [primary, ghost] as Array[Dictionary], [ally] as Array[Dictionary])
	_expect((panel.get_node(^"Margin/Column/Rows") as Node).get_child_count() == 2, "one row per objective")
	_expect((panel as Control).visible and (panel.get_node(^"Margin/Column/Allies") as Control).visible, "allies show under the objectives")
	var targets: Array = panel.call(&"focus_targets")
	_expect(targets.size() == 3, "the list button, the marker button and the ally row take focus")
	var summary := panel.get_node(^"Margin/Column/Summary") as Label
	var ghost_row := (panel.get_node(^"Margin/Column/Rows") as Node).get_child(1) as Control
	_expect(summary.visible and summary.text.contains("1") and not ghost_row.visible, "folded, the ghost conditions read as one counted line: " + summary.text)
	(panel.get_node(^"Margin/Column/Header/More") as Button).button_pressed = true
	_expect(ghost_row.visible and not summary.visible, "listed, each ghost condition has its row")
	(panel.get_node(^"Margin/Column/Header/More") as Button).button_pressed = false
	panel.call(&"show_lists", [] as Array[Dictionary], [] as Array[Dictionary])
	_expect(not (panel as Control).visible, "nothing to show hides the panel")


# --- weapons -------------------------------------------------------------------------------------

func _weapon_checks() -> void:
	_expect(Weapons.rounds_text({"ready": 24, "magazine": 120, "reload_s": 0.0}) == _t(&"hud_weapons.rounds.of").format(["24", "120"]), "rounds read ready of magazine")
	_expect(Weapons.rounds_text({"ready": 2, "magazine": 2, "reload_s": 14.0}).contains("2"), "a reloading launcher gives its reload")
	_expect(Weapons.rounds_text({"ready": 0, "magazine": 0}) == _t(&"hud_weapons.rounds.empty"), "an empty mount says so")
	_expect(Weapons.is_launcher({"category": Bp.CATEGORY_MISSILE}) and not Weapons.is_launcher({"category": Bp.CATEGORY_GUN}), "missile parts are launchers, guns are not")
	_expect(Weapons.extra_text({"local_control": true}, false).contains(_t(&"hud_weapons.local_control")), "local control is noted (L394)")
	_expect(Weapons.extra_text({"local_control": false, "solution": {"converged": true}}, false).is_empty(), "the solution hides unless asked for")
	_expect(Weapons.solution_text({"converged": false, "iterations": 6, "error_m": 1.2}).contains("6"), "an unconverged solution gives its iterations (L518)")
	var groups: Array[Dictionary] = [{"group": 0, "name": {"key": &"hud.group.name", "args": [1]}, "mounts": PackedInt32Array([3, 4]), "holder": Bp.HOLDER_PLAYER, "status": Bp.ENGAGE_HOLDING, "key": 1}]
	var mounts: Array[Dictionary] = [
		{"slot": 3, "name": {"key": &"hud_weapons.title", "args": []}, "group": 0, "status": Bp.ENGAGE_HOLDING, "track": 12, "reason": {"key": &"sim.hold.slewing", "args": []}, "holder": Bp.HOLDER_PLAYER, "local_control": false, "solution": null, "ready": 10, "magazine": 50, "reload_s": 0.0, "effective_range_m": 8000.0},
		{"slot": 4, "name": {"key": &"hud_weapons.title", "args": []}, "group": 0, "status": Bp.ENGAGE_IDLE, "track": -1, "reason": {"key": &"sim.hold.no_target", "args": []}, "holder": Bp.HOLDER_OFFICER, "local_control": true, "solution": null, "ready": 4, "magazine": 4, "reload_s": 0.0, "effective_range_m": 20000.0},
		{"slot": 9, "name": {"key": &"hud_weapons.title", "args": []}, "group": -1, "status": Bp.ENGAGE_IDLE, "track": -1, "reason": {"key": &"sim.hold.no_target", "args": []}, "holder": Bp.HOLDER_OFFICER, "local_control": false, "solution": null, "ready": 0, "magazine": 0, "reload_s": 0.0, "effective_range_m": 0.0},
	]
	var statics: Array = [{"slot": 3, "category": Bp.CATEGORY_GUN}, {"slot": 4, "category": Bp.CATEGORY_MISSILE}, {"slot": 9, "category": Bp.CATEGORY_DEFENCE}]
	var panel: Node = _nodes["weapons_panel"]
	panel.call(&"show_weapons", groups, mounts, statics, Bp.NONE)
	var rows: Dictionary = panel.get(&"_mount_rows")
	_expect(rows.size() == 3, "a row per mount, decoys included")
	_expect(rows[4]["launch"] != null and (rows[4]["launch"] as Button).disabled, "a launcher with no track to launch at is disabled")
	_expect(rows[3]["launch"] == null, "a gun has no launch button")
	_expect(rows[9]["decoy"] != null and (rows[9]["decoy"] as Button).disabled, "an empty decoy launcher is disabled")
	_expect((rows[3]["reason"] as Label).text == _t(&"sim.hold.slewing"), "the hold reason always shows (L353)")
	_expect((rows[4]["extra"] as Label).visible, "local control shows its line")
	var group_rows: Dictionary = panel.get(&"_group_rows")
	_expect((group_rows[0]["hand"] as Control).visible, "a group the player holds shows the hand glyph (rule 9)")
	panel.call(&"show_weapons", groups, mounts, statics, 12)
	_expect(not (rows[4]["launch"] as Button).disabled, "with a track the launcher is ready")
	var first_row: Object = rows[3]["name"]
	panel.call(&"show_weapons", groups, mounts, statics, 12)
	_expect(is_instance_valid(first_row) and panel.get(&"_mount_rows")[3]["name"] == first_row, "rows update in place at 10 Hz")


# --- reticle -------------------------------------------------------------------------------------

func _reticle_checks() -> void:
	var lines: PackedStringArray = Reticle.readout_lines({"valid": true, "range_m": 3650.0, "bearing_deg": 47.0, "track": 2, "in_arc": false, "time_of_flight_s": 6.4})
	_expect(lines.size() == 4 and lines[3] == _t(&"hud_weapons.aim.out_of_arc"), "the readout gives range, flight, track and out of arc")
	_expect(Reticle.readout_lines({"valid": false, "range_m": 0.0}).size() == 1, "open sky says there is nothing to lay on")
	_expect(Reticle.readout_lines({}).is_empty(), "no readout, no lines")
	var r: Node = _nodes["reticle"]
	_expect((r as Control).mouse_filter == Control.MOUSE_FILTER_IGNORE, "the reticle never takes a click from the HUD shell")
	_expect(not r.is_processing(), "without a held weapon role the reticle does no per-frame work")


# --- damage control ------------------------------------------------------------------------------

func _dc_state() -> Dictionary:
	var statics: Array[Dictionary] = [
		{"id": 0, "name": {"key": &"hud_damage.compartment", "args": [1]}, "deck": 0, "rect": Rect2(0, 0, 5, 4), "side_rect": Rect2(0, 0, 5, 2), "volume_m3": 40.0, "has_magazine": false, "below_waterline": true, "neighbours": PackedInt32Array([1])},
		{"id": 1, "name": {"key": &"hud_damage.compartment", "args": [2]}, "deck": 0, "rect": Rect2(5, 0, 5, 4), "side_rect": Rect2(5, 0, 5, 2), "volume_m3": 40.0, "has_magazine": true, "below_waterline": true, "neighbours": PackedInt32Array([0, 2])},
		{"id": 2, "name": {"key": &"hud_damage.compartment", "args": [3]}, "deck": 1, "rect": Rect2(0, 0, 10, 4), "side_rect": Rect2(0, 2, 10, 2), "volume_m3": 80.0, "has_magazine": false, "below_waterline": false, "neighbours": PackedInt32Array([1])},
	]
	var live: Array[Dictionary] = [
		{"id": 0, "fill": 0.6, "fire": 0.0, "breached": true, "valve_open": false, "sealed": true},
		{"id": 1, "fill": 0.0, "fire": 0.5, "breached": false, "valve_open": false, "sealed": true},
		{"id": 2, "fill": 0.0, "fire": 0.0, "breached": false, "valve_open": false, "sealed": true},
	]
	var teams: Array[Dictionary] = [
		{"index": 0, "kind": Bp.TEAM_PATCH, "target": 0, "progress": 0.3, "pinned": true, "text": {}},
		{"index": 1, "kind": Bp.TEAM_IDLE, "target": -1, "progress": 0.0, "pinned": false, "text": {}},
	]
	return {
		"session": Bp.SESSION_IN_SORTIE, "statics": statics, "live": live,
		"crew": {"original_berths": 8, "surviving_berths": 8, "passengers": 0, "passengers_lost": 0, "teams": teams, "repair_order": PackedInt32Array([0, 1, 2, 3, 4, 5])},
		"controls": [
			{"slot": 7, "kind": &"pump", "compartment": 0, "on": false, "usable": true, "name": {"key": &"hud_damage.compartment", "args": [9]}},
			{"slot": -1, "kind": &"valve", "compartment": 0, "on": false, "usable": true, "name": {}},
			{"slot": 8, "kind": &"magazine_flood", "compartment": 1, "on": false, "usable": true, "name": {"key": &"hud_damage.compartment", "args": [8]}},
		],
		"outline": {"plan": PackedVector2Array([Vector2(0, 0), Vector2(10, 0), Vector2(10, 4), Vector2(0, 4)]), "profile": PackedVector2Array([Vector2(0, 0), Vector2(10, 0), Vector2(10, 4), Vector2(0, 4)]), "waterline_m": 1.5, "length_m": 10.0, "beam_m": 4.0, "decks": PackedFloat32Array([2.0])},
		"summary": {"flooding": 1, "fires": 1, "breached": 1, "parts_damaged": 1, "parts_heavy": 0, "parts_destroyed": 0, "cells_lost": 0.02, "heel_deg": -3.0, "trim_deg": 0.0, "teams_idle": 1, "pumps_running": 0},
		"parts_static": [{"slot": 3, "name": {"key": &"hud_damage.compartment", "args": [7]}, "compartment": 1, "cell": Vector3i(10, 0, 2), "size": Vector3i(2, 1, 2)}],
		"parts": [{"slot": 3, "level": Bp.DAMAGE_DAMAGED, "health": 0.5}],
	}


func _damage_checks() -> void:
	var teams: Array[Dictionary] = [
		{"index": 0, "kind": Bp.TEAM_FIGHT_FIRE, "pinned": true},
		{"index": 1, "kind": Bp.TEAM_REPAIR, "pinned": false},
		{"index": 2, "kind": Bp.TEAM_IDLE, "pinned": false},
	]
	_expect(Damage.pick_team(teams) == 2, "a new job goes to the team standing by")
	teams[2]["kind"] = Bp.TEAM_PATCH
	_expect(Damage.pick_team(teams) == 1, "else to the first team the officer directs")
	_expect(Damage.pick_team([] as Array[Dictionary]) == 0, "no teams falls back to team 1")
	_expect(Damage.valid_order([5, 4, 3, 2, 1, 0] as Array[int]) and not Damage.valid_order([0, 0, 1, 2, 3, 4] as Array[int]), "a repair order holds each priority once")
	_expect(Damage.moved([0, 1, 2, 3, 4, 5] as Array[int], 2, -1) == ([0, 2, 1, 3, 4, 5] as Array[int]), "Move up swaps with the entry above")
	_expect(Damage.moved([0, 1, 2, 3, 4, 5] as Array[int], 0, -1) == ([0, 1, 2, 3, 4, 5] as Array[int]), "the top entry cannot move up")
	_expect(Damage.summary_text({"teams_idle": 2}).begins_with(_t(&"hud_damage.sum.none")), "no damage says so")
	_expect(Damage.state_text({}, false) == _t(&"hud_damage.state.sound"), "a sound compartment says so")
	_expect(Damage.state_text({"fill": 0.5, "valve_open": true}, true).contains(_t(&"hud_damage.state.valve")), "an open flood valve is named below the waterline")
	var dc: Node = _nodes["damage_control"]
	var state := _dc_state()
	dc.call(&"show_state", state)
	_expect(int(dc.get(&"_selected")) == 1, "the station opens on the burning magazine compartment")
	_expect((dc.get(&"_d_fire") as Button).visible and not (dc.get(&"_d_fire") as Button).disabled, "a fire offers to send a team")
	_expect(String((dc.get(&"_d_fire") as Button).text).contains("2"), "the team standing by is the one sent")
	_expect(((dc.get(&"_d_mags") as Node).get_child_count()) == 1, "the magazine in the compartment can be flooded")
	dc.call(&"_on_picked", 0)
	_expect(int(dc.get(&"_selected")) == 0, "picking a compartment selects it")
	_expect((dc.get(&"_d_patch") as Button).visible and (dc.get(&"_d_patch") as Button).disabled, "a breach over half flooded cannot be patched (L652)")
	_expect((dc.get(&"_d_valve") as Button).visible, "a compartment below the waterline offers its flood valve")
	dc.call(&"_on_deck_step", 1)
	_expect(int(dc.get(&"_selected")) == 2 and int(dc.get(&"_deck")) == 1, "stepping up a deck selects the nearest compartment there")
	var sch: Control = dc.get(&"_schematic")
	sch.size = Vector2(500, sch.get_combined_minimum_size().y)
	sch.call(&"_relayout")
	var plan_pt: Vector2 = sch.call(&"_plan_point", Vector2(5, 2))
	_expect(int(sch.call(&"pick_at", plan_pt)) == 2, "a click on the plan picks the compartment on the shown deck")
	var prof_pt: Vector2 = sch.call(&"_profile_point", Vector2(2, 1))
	_expect(int(sch.call(&"pick_at", prof_pt)) == 0, "a click on the profile picks by height, with up drawn up")
	var team_rows: Array = dc.get(&"_team_rows")
	_expect(team_rows.size() == 2 and (team_rows[0]["back"] as Control).visible and not (team_rows[1]["back"] as Control).visible, "only a team on your order offers Hand back")
	var order_rows: Array = dc.get(&"_order_rows")
	_expect((order_rows[0]["up"] as Button).disabled and (order_rows[5]["down"] as Button).disabled, "the ends of the repair order cannot move further")
	var no_berths := _dc_state()
	no_berths["crew"]["surviving_berths"] = 0
	dc.call(&"_on_picked", 1)
	dc.call(&"show_state", no_berths)
	_expect((dc.get(&"_d_fire") as Button).disabled, "with no surviving berths no job can start (L707)")
	dc.call(&"show_state", {"session": Bp.SESSION_IN_SORTIE})
	_expect((dc.get_node(^"Empty") as Control).visible, "before the first picture the station waits")


# --- networks ------------------------------------------------------------------------------------

func _network_checks() -> void:
	_expect(Networks.next_priority(3, 1) == 2 and Networks.next_priority(1, 1) == 1 and Networks.next_priority(5, -1) == 5, "Raise sheds later (a smaller number), within 1 to 5")
	var net := {"kind": Bp.NETWORK_POWER, "conduits": [
		{"id": 0, "from": 0, "to": 1, "cut": true, "jury_rigged": false, "redundant": true, "pinned": false},
		{"id": 1, "from": 1, "to": 2, "cut": false, "jury_rigged": true, "redundant": false, "pinned": false},
		{"id": 2, "from": 0, "to": 2, "cut": false, "jury_rigged": false, "redundant": false, "pinned": false},
	], "consumers_dead": 1, "consumers_on_buffer": 1}
	var s: String = Networks.summary_text(net, 2, 0)
	_expect(s.contains(_t(&"hud_damage.net.sum.cut").format(["1"])) and s.contains(_t(&"hud_damage.net.sum.rigged").format(["1"])), "the summary counts cut and jury-rigged conduits: " + s)
	_expect(Networks.tab_text(Bp.NETWORK_POWER, net) != _t(&"hud_damage.net.power"), "a network with cuts says so on its button")
	_expect(Networks.consumer_text("Gun 1", Bp.NETWORK_DATA, Bp.LINK_LOCAL_CONTROL, false).contains(_t(&"hud_damage.net.link.local")), "local control is named with its error (L394)")
	_expect(Networks.consumer_text("Pump", Bp.NETWORK_FUEL, Bp.LINK_BUFFER, false).contains(_t(&"hud_damage.net.link.buffer_fuel")), "a fuel buffer is the day tank")
	var np: Node = _nodes["networks_panel"]
	var dc_state := _dc_state()
	np.call(&"show_state", {
		"session": Bp.SESSION_IN_SORTIE, "networks": [net, {"kind": Bp.NETWORK_FUEL, "conduits": []}, {"kind": Bp.NETWORK_DATA, "conduits": []}],
		"statics": dc_state["statics"], "outline": dc_state["outline"], "crew": dc_state["crew"],
		"parts_static": [{"slot": 3, "part": &"gun_76", "name": {"key": &"hud_damage.compartment", "args": [7]}}, {"slot": 4, "part": &"cabin", "name": {"key": &"hud_damage.compartment", "args": [8]}}],
		"parts": [{"slot": 3, "power": Bp.LINK_BUFFER, "fuel": Bp.LINK_NETWORK, "data": Bp.LINK_NETWORK, "shed": false, "priority": 3}, {"slot": 4, "power": Bp.LINK_NETWORK, "fuel": Bp.LINK_NETWORK, "data": Bp.LINK_NETWORK, "shed": true, "priority": 5}],
		"demand": {&"gun_76": 40.0, &"cabin": 0.0},
	})
	var conduit_rows: Dictionary = np.get(&"_conduit_rows")
	_expect(conduit_rows.size() == 2 and conduit_rows[0]["send"] != null and conduit_rows[1]["send"] == null, "a cut conduit offers a jury rig; a jury-rigged one does not")
	_expect((np.call(&"affected_consumers", Bp.NETWORK_POWER) as Array).size() == 2, "power lists the consumer on its battery and the shed one")
	_expect((np.get(&"_priority_rows") as Dictionary).size() == 1, "only parts that draw power have a priority row")
	Networks.shown_kind = Bp.NETWORK_DATA
	np.call(&"show_state", np.get(&"_state"))
	_expect((np.get(&"_priority_rows") as Dictionary).is_empty(), "priorities belong to the power network only")
	Networks.shown_kind = Bp.NETWORK_POWER
