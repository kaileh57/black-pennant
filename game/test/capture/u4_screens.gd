extends Control
## Windowed capture of the multiplayer, blueprint, replay and benchmark screens with hand-built data.
## Units: ui-lobby, ui-blueprints, ui-replays, ui-benchmark. Run from the repository root:
##   godot --path game --resolution 1600x900 res://test/capture/u4_screens.tscn -- --shot=<name> [--out=<dir>]
##     [--scale=<ui scale, 0.75 to 2>] [--frames=<settle frames>]
## `--shot` picks one scenario of `SHOTS` below. The screen is built, fed plain data through its
## render entry point (no facade is called that writes, hosts, or touches the network), left to
## settle for 60 frames, saved as <out>/<shot>.png and the window closes.

const OUT_DEFAULT := "user://captures"
const SETTLE_FRAMES := 60

var _shot := ""
var _out := OUT_DEFAULT
var _frames := 0
var _settle := SETTLE_FRAMES
## Timed actions of a live shot: frame number to Callable.
var _steps: Dictionary[int, Callable] = {}
var _previous_name := ""
var _screen: Node


func _ready() -> void:
	for a: String in OS.get_cmdline_user_args():
		if a.begins_with("--shot="):
			_shot = a.trim_prefix("--shot=")
		elif a.begins_with("--out="):
			_out = a.trim_prefix("--out=")
		elif a.begins_with("--scale="):
			get_tree().root.content_scale_factor = clampf(float(a.trim_prefix("--scale=")), 0.75, 2.0)
		elif a.begins_with("--frames="):
			_settle = maxi(int(a.trim_prefix("--frames=")), 10)
	set_anchors_preset(Control.PRESET_FULL_RECT)
	if _shot.is_empty():
		# Loaded by the compile check or opened without a shot: nothing to capture.
		set_process(false)
		return
	if _shot.ends_with("_hc"):
		_high_contrast()
	var base := _shot.trim_suffix("_hc")
	if not has_method("_shot_" + base):
		push_error("u4_screens: no shot %s" % _shot)
		get_tree().quit(1)
		return
	call("_shot_" + base)


func _process(_delta: float) -> void:
	_frames += 1
	if _steps.has(_frames):
		(_steps[_frames] as Callable).call()
	if _frames == _settle:
		DirAccess.make_dir_recursive_absolute(_out)
		var img := get_viewport().get_texture().get_image()
		var scale_tag := "" if is_equal_approx(get_tree().root.content_scale_factor, 1.0) else "_x%s" % String.num(get_tree().root.content_scale_factor, 2)
		var path := _out.path_join(_shot + scale_tag + ".png")
		var err := img.save_png(path)
		print("u4_screens: saved %s (%d)" % [path, err])
		get_tree().quit(0 if err == OK else 1)


func _high_contrast() -> void:
	var project := ThemeDB.get_project_theme()
	var hc := load("res://ui/theme/project_theme_high_contrast.tres") as Theme
	if project != null and hc != null:
		project.merge_with(hc)


func _add(path: String) -> Node:
	var scene := load(path) as PackedScene
	_screen = scene.instantiate()
	add_child(_screen)
	return _screen


# --- replays -------------------------------------------------------------------------------------

func _replay_rows() -> Array[Dictionary]:
	var now := int(Time.get_unix_time_from_system())
	var out: Array[Dictionary] = []
	out.append({"path": "user://replays/000012-a.bpreplay", "mode": 0, "operation": "op.act1.convoy_raid", "players": PackedStringArray(["Halyard", "Secondmate"]),
		"duration_s": 1834.0, "recorded_unix": now - 3600, "pinned": true, "size_mb": 24.6, "playable": true, "problem": {"key": &"", "args": []}})
	out.append({"path": "user://replays/000011-b.bpreplay", "mode": 3, "operation": "", "players": PackedStringArray(["Halyard", "Kestrel"]),
		"duration_s": 912.0, "recorded_unix": now - 7200, "pinned": false, "size_mb": 9.1, "playable": true, "problem": {"key": &"", "args": []}})
	out.append({"path": "user://replays/000010-c.bpreplay", "mode": 1, "operation": "op.act1.convoy_raid", "players": PackedStringArray(["Halyard"]),
		"duration_s": 1240.0, "recorded_unix": now - 86400, "pinned": false, "size_mb": 14.0, "playable": true, "problem": {"key": &"", "args": []}})
	out.append({"path": "user://replays/000009-d.bpreplay", "mode": 2, "operation": "", "players": PackedStringArray(["Halyard", "Kestrel", "Brine"]),
		"duration_s": 2410.0, "recorded_unix": now - 172800, "pinned": false, "size_mb": 30.2, "playable": false,
		"problem": {"key": &"session.replay.other_format", "args": [1, 2]}})
	return out


func _shot_replays() -> void:
	var s := _add("res://ui/screens/replay_browser.tscn")
	s.call(&"show_rows", _replay_rows())


func _shot_replays_empty() -> void:
	var s := _add("res://ui/screens/replay_browser.tscn")
	var none: Array[Dictionary] = []
	s.call(&"show_rows", none)


# --- multiplayer ---------------------------------------------------------------------------------

func _rulesets() -> Array[Dictionary]:
	return [{"id": "standard", "kind": 0, "tonnage_cap_t": 400.0, "complexity_per_vessel": 600.0, "complexity_per_lobby": 3600.0},
		{"id": "open", "kind": 1, "tonnage_cap_t": 400.0, "complexity_per_vessel": 600.0, "complexity_per_lobby": 3600.0}]


func _multiplayer() -> Node:
	var s := _add("res://ui/screens/multiplayer.tscn")
	var notice := {"key": &"net.host.firewall", "args": [47610]}
	s.call(&"fill", _rulesets(), notice, false, {0: 4, 1: 4, 2: 8, 3: 4, 4: 0}, 47610, 1.0)
	return s


func _shot_multiplayer() -> void:
	var s := _multiplayer()
	(s.get(&"_name_field") as LineEdit).text = "Halyard"
	s.call(&"_check_code")


func _shot_multiplayer_code() -> void:
	var s := _multiplayer()
	(s.get(&"_name_field") as LineEdit).text = "Halyard"
	(s.get(&"_code_field") as LineEdit).text = "1A2B-3C4D-5E6F-7G8H-9J0K-MNPQ-RSTV-WX"
	s.call(&"show_code_check", {"valid": true, "address": "203.0.113.5", "port": 47610, "protocol": 3, "protocol_ok": true, "text": {"key": &"", "args": []}})


func _shot_multiplayer_failed() -> void:
	var s := _multiplayer()
	(s.get(&"_code_field") as LineEdit).text = "1A2B-3C4D-5E6F-7G8H"
	s.call(&"show_code_check", {"valid": false, "text": {"key": &"net.join_code.wrong_length", "args": []}})
	s.call(&"show_join_failed", TranslationServer.translate(&"net.refusal.lobby_full"))


func _shot_multiplayer_joining() -> void:
	var s := _multiplayer()
	(s.get(&"_code_field") as LineEdit).text = "1A2B-3C4D-5E6F-7G8H-9J0K-MNPQ-RSTV-WX"
	s.call(&"show_code_check", {"valid": true, "address": "203.0.113.5", "port": 47610})
	s.call(&"_show_joining", "203.0.113.5:47610")


# --- lobby ---------------------------------------------------------------------------------------

func _conn(ping: int, loss: float, behind: int, state: int, warning: Dictionary = {}) -> Dictionary:
	return {"player": 0, "name": "", "state": state, "ping_ms": ping, "loss": loss, "frames_behind": behind, "quality": 0 if ping < 120 else (1 if ping < 300 else 2),
		"cap_kbps": 256, "is_host": false, "warning": -1, "warning_text": warning, "held_s": -1, "officers_in_command": false}


func _players(pvp: bool) -> Array[Dictionary]:
	var out: Array[Dictionary] = []
	out.append({"slot": 0, "name": "Halyard", "team": 0, "ready": true, "blueprint_hash": "ab12", "blueprint_name": "Fast attack craft 22", "tonnage_t": 118.0,
		"complexity": 212.0, "upload": -1.0, "is_host": true, "is_local": true, "conn": _conn(0, 0.0, 0, 2)})
	out.append({"slot": 1, "name": "Kestrel", "team": 1 if pvp else 0, "ready": false, "blueprint_hash": "cd34", "blueprint_name": "Corvette 48", "tonnage_t": 386.0,
		"complexity": 401.0, "upload": -1.0, "is_host": false, "is_local": false, "conn": _conn(64, 0.004, 2, 2)})
	out.append({"slot": 2, "name": "Brine", "team": 0, "ready": false, "blueprint_hash": "ef56", "blueprint_name": "", "tonnage_t": -1.0,
		"complexity": -1.0, "upload": 0.42, "is_host": false, "is_local": false, "conn": _conn(312, 0.11, 18, 3, {"key": &"net.warning.packet_loss", "args": []})})
	if pvp:
		out.append({"slot": 3, "name": "Gannet", "team": 1, "ready": true, "blueprint_hash": "", "blueprint_name": "", "tonnage_t": -1.0,
			"complexity": -1.0, "upload": -1.0, "is_host": false, "is_local": false, "conn": _conn(41, 0.0, 1, 2)})
	return out


func _lobby_dict(pvp: bool) -> Dictionary:
	return {"mode": 3 if pvp else 2, "name": "Halyard's harbour", "ruleset": _rulesets()[0], "players": _players(pvp), "max_players": 4 if pvp else 8,
		"issues": [{"key": &"net.lobby.issue.player_not_ready", "args": ["Kestrel"]}, {"key": &"net.lobby.issue.no_blueprint", "args": ["Gannet"]}] if pvp else [{"key": &"net.lobby.issue.player_not_ready", "args": ["Kestrel"]}],
		"can_start": false, "saturated": false, "is_host": true,
		"setup": {"seed": 1, "weather": 0, "sea_state": 3, "time_of_day": 1, "skirmish": null}}


func _host_panel_dict(portmap: int) -> Dictionary:
	var trouble := portmap == 4 or portmap == 3
	return {"hosting": true, "bound": true, "port": 47610, "portmap": portmap,
		"portmap_text": {"key": StringName("net.portmap." + ["", "trying", "mapped", "double_nat", "failed"][portmap]), "args": []},
		"external_address": "203.0.113.5", "lan_address": "192.168.1.24", "join_code": "1A2B-3C4D-5E6F-7G8H-9J0K-MNPQ-RSTV-WX",
		"lobby_key": "K7QM-2R9X-PD4T-H8WN", "vpn_address": "", "vpn_join_code": "", "upload_kbps": 1000, "client_limit": 3, "saturated": false,
		"manual_steps": [{"key": &"net.host.manual.forward", "args": [47610, "192.168.1.24"]}, {"key": &"net.host.manual.vpn", "args": []}] if trouble else [],
		"banned": 0}


func _shot_lobby() -> void:
	var s := _add("res://ui/screens/lobby.tscn")
	var lobby := _lobby_dict(true)
	s.call(&"show_lobby", lobby)
	s.get(&"_picker").call(&"load_entries")
	(s.get(&"_host_panel") as Node).call(&"show_panel", _host_panel_dict(2), _players(true))


func _shot_lobby_member() -> void:
	var s := _add("res://ui/screens/lobby.tscn")
	var lobby := _lobby_dict(false)
	lobby["is_host"] = false
	lobby["setup"] = null
	var players: Array[Dictionary] = _players(false)
	players[0]["is_local"] = false
	players[1]["is_local"] = true
	lobby["players"] = players
	s.call(&"show_lobby", lobby)
	var rows: Array[Dictionary] = []
	for p: Dictionary in players:
		var c: Dictionary = p["conn"].duplicate()
		c["name"] = p["name"]
		c["is_host"] = p["is_host"]
		rows.append(c)
	(s.get(&"_connection_panel") as Node).call(&"show_rows", rows, 60.0)


func _shot_lobby_empty() -> void:
	_add("res://ui/screens/lobby.tscn")


func _shot_host_panel_failed() -> void:
	var s := _add("res://ui/screens/host_panel.tscn")
	s.get(&"_backing").visible = true
	s.call(&"show_panel", _host_panel_dict(4), _players(false))


func _shot_connection() -> void:
	var s := _add("res://ui/screens/connection_panel.tscn")
	s.get(&"_backing").visible = true
	var rows: Array[Dictionary] = []
	for p: Dictionary in _players(false):
		var c: Dictionary = p["conn"].duplicate()
		c["name"] = p["name"]
		c["is_host"] = p["is_host"]
		rows.append(c)
	var dropped := _conn(0, 0.0, 0, 4)
	dropped["name"] = "Gannet"
	dropped["held_s"] = 212
	dropped["officers_in_command"] = true
	rows.append(dropped)
	s.call(&"show_rows", rows, 52.0)
	s.call(&"show_host_lost", true)
	s.call(&"show_rehosted", "2B3C-4D5E-6F7G-8H9J-0KMN-PQRS-TVWX-YZ")


# --- blueprints ----------------------------------------------------------------------------------

func _info(name_text: String, length: float, beam: float, t: float, cost: int, parts: int, extra: Dictionary = {}) -> Dictionary:
	var d := {"path": "user://blueprints/" + name_text.to_lower().replace(" ", "_") + ".bp.ron", "stock": false, "name": name_text, "author": "Halyard",
		"hash": "", "format": 1, "game_version": "0.4.0", "kind": &"ship", "length_m": length, "beam_m": beam, "displacement_t": t, "cost": cost,
		"complexity": 214.0, "parts": parts, "modified_unix": int(Time.get_unix_time_from_system()) - parts * 3600,
		"lineage": [{"hash": "ab", "author": "Halyard"}, {"hash": "cd", "author": "Kestrel"}], "needs_migration": false, "newer_format": false}
	d.merge(extra, true)
	return d


func _browser() -> Node:
	var s := _add("res://ui/screens/blueprint_browser.tscn")
	var rows: Array[Dictionary] = [
		{"path": "", "label": "My designs", "stock": false, "depth": 0},
		{"path": "Corvettes", "label": "Corvettes", "stock": false, "depth": 1},
		{"path": "Imported", "label": "Imported", "stock": false, "depth": 1},
		{"path": "stock:", "label": "Stock designs", "stock": true, "depth": 0, "group": true},
		{"path": "stock:starters", "label": "Starter hulls", "stock": true, "depth": 1},
		{"path": "stock:classes", "label": "Enemy classes", "stock": true, "depth": 1},
	]
	s.call(&"show_tree", rows)
	return s


func _shot_blueprints() -> void:
	var s := _browser()
	var entries: Array[Dictionary] = []
	for i: Dictionary in [_info("Gannet", 34.5, 7.2, 212.0, 1840, 61), _info("Picket boat", 18.0, 4.4, 52.0, 610, 22),
			_info("Long gun corvette", 52.0, 8.8, 512.0, 4120, 118, {"needs_migration": true}), _info("Big monitor", 96.0, 16.0, 2410.0, 12200, 220),
			_info("Future frigate", 70.0, 10.0, 980.0, 7800, 140, {"newer_format": true, "game_version": "0.9.0"})]:
		entries.append({"name": String(i["path"]).get_file(), "path": i["path"], "is_folder": false, "info": i})
	s.call(&"show_folder", "", entries)


func _shot_blueprints_empty() -> void:
	var s := _browser()
	var none: Array[Dictionary] = []
	s.call(&"show_folder", "Imported", none)


func _shot_share_copy() -> void:
	var s := _add("res://ui/screens/share_dialog.tscn")
	var text := "BPN1:"
	var alphabet := "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
	var rng := RandomNumberGenerator.new()
	rng.seed = 7
	for i in 4800:
		text += alphabet[rng.randi_range(0, alphabet.length() - 1)]
	s.call(&"show_copy", text, "Long gun corvette", true)


func _shot_share_import() -> void:
	var s := _add("res://ui/screens/share_dialog.tscn")
	s.call(&"show_import", "BPN1:KLUv_QBYbRAAxkx0IpCnDgCKZz2sVrXyQ\n  2h3ZEu8qyB1mcwT")
	s.call(&"show_result", {"ok": true, "kind": &"ship", "path": "user://blueprints/Imported/gannet.bp.ron", "name": "Gannet",
		"issues": [{"text": {"key": &"blueprints.problem.migrate", "args": []}, "fatal": false}]})


# --- replay controls and benchmark ---------------------------------------------------------------

func _shot_replay_controls() -> void:
	var s := _add("res://ui/screens/replay_controls.tscn")
	# Hand-built data only: keep the controls from reading the (idle) session.
	s.set(&"_bound", true)
	s.call(&"show_info", {"tick": 41220, "duration_ticks": 110040, "time_s": 687.0, "duration_s": 1834.0, "rate": 2.0, "paused": false,
		"viewer_side": -1, "sides": PackedInt32Array([0, 1]), "pinned": true})
	s.call(&"show_kill_frame", {"victim": 12, "killer": 40, "from_s": 660.0, "to_s": 690.0}, false)


func _report() -> Dictionary:
	var stages: Array[Dictionary] = []
	for row: Array in [[1, 2, Vector3(9.8, 12.4, 15.1)], [1, 0, Vector3(7.1, 8.9, 11.0)], [1, 1, Vector3(8.2, 10.6, 13.3)], [2, 2, Vector3(10.3, 13.9, 17.2)], [3, 2, Vector3(11.0, 15.2, 19.9)]]:
		stages.append({"stage": row[0], "tier": row[1], "frame_ms": row[2], "gpu_ms": Vector3.ZERO, "tick_ms": Vector2(4.1, 5.3), "adapter_ms_p99": 1.4,
			"missed_vblanks": 0.012, "frames": 10800, "seconds": 180.0, "failed": false})
	stages.append({"stage": 4, "tier": 2, "frame_ms": Vector3.ZERO, "gpu_ms": Vector3.ZERO, "tick_ms": Vector2.ZERO, "adapter_ms_p99": 0.0, "missed_vblanks": 0.0, "frames": 0, "seconds": 0.0, "failed": false})
	stages.append({"stage": 5, "tier": 2, "frame_ms": Vector3.ZERO, "gpu_ms": Vector3.ZERO, "tick_ms": Vector2.ZERO, "adapter_ms_p99": 0.0, "missed_vblanks": 0.0, "frames": 0, "seconds": 0.0, "failed": false})
	return {"build": "0.4.0-dev+1a2b3c", "os": "macOS 27.0", "cpu": "Apple M5 Pro", "ram_gb": 24.0, "adapter": "Apple M5 Pro", "driver_version": "Metal 4.0",
		"rendering_method": "forward_plus", "rendering_driver": "metal", "fallbacks": PackedStringArray(), "tier": 2, "window": Vector2i(1600, 900), "scale_3d": 1.0,
		"stages": stages, "pipeline_compilations": 3, "video_mem_mb": 1840.0, "peak_mem_mb": 2210.0, "sim_calib_us": 1.12, "editor_gauge_ms_p95": 38.0,
		"editor_splash_ms_p95": 410.0, "thermal": PackedInt32Array([0, 0, 1]), "dpi_awareness": "", "problems": PackedStringArray(), "json": "{}",
		"soak": {"frame_p95_last_5_min_ms": 14.1, "pass": true}}


func _shot_benchmark() -> void:
	var s := _add("res://ui/screens/benchmark.tscn")
	s.call(&"_show_estimate")


func _shot_benchmark_running() -> void:
	var s := _add("res://ui/screens/benchmark.tscn")
	s.call(&"_show_state", &"running")
	s.call(&"show_stage", {"stage": 4, "progress": 0.42, "seconds_left": 51.0, "text": {"key": &"bench.stage.editor", "args": []}})


func _shot_benchmark_results() -> void:
	var s := _add("res://ui/screens/benchmark.tscn")
	s.call(&"show_report", _report())


# --- real runs through boot ----------------------------------------------------------------------

## The game's own start-up with this run's command line (for example `--replay <path>`): boot
## routes to the screen the flag asks for, and the capture shows what the player would see.
func _shot_boot() -> void:
	# Boot runs only as the main scene; here it is a child, so its start-up is called directly.
	var boot := _add("res://boot/boot.tscn")
	boot.call_deferred(&"_run")


## A real loopback lobby (run with `--loopback-host`: bound to 127.0.0.1, no port mapping, no
## firewall prompt): boot to the menu, open Multiplayer, host a sandbox lobby, let the router open
## the lobby, then capture it with the host panel's real join code and key.
func _shot_lobby_live() -> void:
	if not "--loopback-host" in OS.get_cmdline_user_args():
		push_error("u4_screens: lobby_live hosts; run it with --loopback-host so nothing binds 0.0.0.0 or maps a port")
		get_tree().quit(1)
		return
	_shot_boot()
	# Hosting stores the display name in the real settings; put the player's own back afterwards.
	_steps[399] = func() -> void:
		_previous_name = Lobby.display_name()
	_steps[_settle - 2] = func() -> void:
		Lobby.leave()
		Lobby.set_display_name(_previous_name)
	_steps[400] = func() -> void:
		var mp: Node = Screens.change(&"multiplayer")
		mp.set(&"firewall_seen", true)
		(mp.get(&"_name_field") as LineEdit).text = "Halyard"
		mp.call(&"_refresh_buttons")
		mp.call(&"_on_host")
	_steps[650] = func() -> void:
		var lobby_screen: Node = Screens.top()
		lobby_screen.call(&"_on_field")
	_steps[760] = func() -> void:
		(Screens.top().get(&"_ready_toggle") as Button).button_pressed = true
	_steps[_settle - 3] = func() -> void:
		print("u4_screens: base=%s lobby=%s" % [Screens.base_name(), Lobby.lobby()])
		print("u4_screens: panel=%s" % [Lobby.host_panel()])


## A real, shortened benchmark run (development build: every stage at 3% of its length): boot, open
## the benchmark, run every stage, capture the first battle stage at --frames and the report after.
func _shot_bench_live() -> void:
	_shot_boot()
	_steps[400] = func() -> void:
		Screens.change(&"benchmark", {"start": true, "duration_scale": 0.03})
	_steps[_settle - 1] = func() -> void:
		print("u4_screens: bench stage=%s" % [Bench.stage()])
