extends SceneTree
## Proof of hosting and joining through the Lobby and Session autoloads. Units gd-link-net and gd-lobby.
## Run from game/. The two-process run gives the joiner its own HOME, so the copies have their own user folder
## and client id:
##   godot --headless --path . -s res://test/unit/lobby_test.gd -- --role=host --port=P --rendezvous=FILE --loopback-host
##   HOME=DIR godot --headless --path . -s res://test/unit/lobby_test.gd -- --role=join --rendezvous=FILE
## The host opens a 1v1 PvP lobby on 127.0.0.1:P (no port mapping, no firewall prompt), makes a new lobby key
## once and checks the codes changed, then writes its join code to FILE. The joiner joins by code and takes
## team 1. Both field the 22 m starter and ready; the host starts; both run the sortie; both return to the
## lobby, ready again and run a second sortie; then the host leaves and the joiner must be told the host
## closed the session.
## A host on its own: `-- --role=solo --mode=skirmish|sandbox --port=P --loopback-host` hosts that mode,
## starts it alone and runs the sortie.
## Exits 1 on any failure.

const STARTER := "starters/fac22.bp.ron"
## Frames each phase may take before the run fails (60 frames a second).
const JOIN_LIMIT := 1200
const LOBBY_LIMIT := 1800
const LOADING_LIMIT := 1800
const SORTIE_FRAMES := 180
const END_LIMIT := 1200

var _lobby: BpLobby
var _session: BpSession
var _role := ""
var _mode := ""
var _port := 0
var _rendezvous := ""
var _frame := 0
var _phase := 0
var _phase_frame := 0
var _round := 1
var _failures := 0
var _signals: Dictionary[String, int] = {}
var _left_key := ""
var _join_failed := ""
var _first_key := ""
var _first_code := ""


func _initialize() -> void:
	Engine.max_fps = 60
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--role="):
			_role = arg.trim_prefix("--role=")
		elif arg.begins_with("--mode="):
			_mode = arg.trim_prefix("--mode=")
		elif arg.begins_with("--port="):
			_port = arg.trim_prefix("--port=").to_int()
		elif arg.begins_with("--rendezvous="):
			_rendezvous = arg.trim_prefix("--rendezvous=")


func _process(_delta: float) -> bool:
	_frame += 1
	if _frame == 1:
		return _start()
	_phase_frame += 1
	match _role:
		"host":
			return _host_step()
		"solo":
			return _solo_step()
	return _join_step()


func _start() -> bool:
	_lobby = root.get_node("Lobby") as BpLobby
	_session = root.get_node("Session") as BpSession
	var app: BpApp = root.get_node("App") as BpApp
	if _lobby == null or _session == null or app == null or not _role in ["host", "join", "solo"] or (_role != "solo" and _rendezvous.is_empty()):
		_fail("needs --role=host|join|solo, --rendezvous and the autoloads")
		return _finish()
	print("lobby_test[%s]: client id %s" % [_role, app.client_id()])
	for s: String in ["lobby_changed", "host_panel_changed", "joined"]:
		_lobby.connect(s, _count.bind(s))
	_lobby.starting.connect(func(_seconds: float) -> void: _count("starting"))
	_lobby.join_failed.connect(_on_join_failed)
	_lobby.left.connect(_on_left)
	if _role == "join":
		# The joiner runs with its own HOME, so its settings file is its own.
		_lobby.set_display_name("  Second\tmate  ")
		if _lobby.display_name() != "Secondmate":
			_fail("the display name was not cleaned: '%s'" % _lobby.display_name())
		return false
	var notice: Dictionary = _lobby.firewall_notice()
	if String(notice.get("key", "")) != "net.host.firewall":
		_fail("no firewall notice before hosting")
	if _role == "solo":
		return _host_solo()
	if FileAccess.file_exists(_rendezvous):
		DirAccess.remove_absolute(_rendezvous)
	if _lobby.host({ "mode": 99 }).is_empty():
		_fail("an unknown mode was accepted")
	if _lobby.host({ "mode": Bp.MODE_PVP_ELIMINATION, "ruleset": "standard", "tonnage_cap_t": 500.0, "port": _port }).is_empty():
		_fail("a tonnage cap that is no act cap was accepted")
	var problems: Array = _lobby.host({ "mode": Bp.MODE_PVP_ELIMINATION, "ruleset": "standard", "tonnage_cap_t": 400.0, "name": "Lobby test", "port": _port, "upload_kbps": 1000 })
	if not problems.is_empty():
		_fail("host refused: %s" % str(problems))
		return _finish()
	if not _lobby.is_host() or not _lobby.is_active():
		_fail("hosting did not make a lobby")
	if _lobby.host({ "mode": Bp.MODE_SANDBOX, "port": _port }).is_empty():
		_fail("a second host was accepted while hosting")
	return false


func _host_solo() -> bool:
	var options := { "port": _port, "upload_kbps": 1000, "weather": Bp.WEATHER_OVERCAST, "sea_state": 4, "seed": 99 }
	if _mode == "skirmish":
		options["mode"] = Bp.MODE_SKIRMISH
		options["skirmish"] = { "template": Bp.MISSION_CONVOY_RAID, "act": Bp.ACT_1, "enemy_per_player": 1, "difficulty": { "preset": "normal" }, "time_of_day": Bp.TIME_DAY }
	else:
		options["mode"] = Bp.MODE_SANDBOX
	var problems: Array = _lobby.host(options)
	if not problems.is_empty():
		_fail("host refused: %s" % str(problems))
		return _finish()
	var refused: Array = _lobby.select_blueprint(STARTER)
	if refused.size() > 0:
		_fail("the starter was refused: %s" % str(refused))
	_lobby.set_ready(true)
	return false


func _solo_step() -> bool:
	match _phase:
		0:
			var l: Dictionary = _lobby.lobby()
			if l.get("can_start", false):
				var setup: Variant = l.get("setup")
				if setup == null or int(setup.get("seed", 0)) != 99 or int(setup.get("sea_state", -1)) != 4:
					_fail("the working setup is wrong: %s" % str(setup))
				if _mode == "skirmish" and (setup == null or setup.get("skirmish") == null or int(setup.skirmish.get("template", -1)) != Bp.MISSION_CONVOY_RAID):
					_fail("the skirmish options are lost: %s" % str(setup))
				print("lobby_test[solo]: %s lobby can start, ruleset %s, %d t cap" % [_mode, l.ruleset.get("id", ""), l.ruleset.get("tonnage_cap_t", 0)])
				if not _lobby.start():
					_fail("start was refused")
				_next()
			elif _phase_frame > LOBBY_LIMIT:
				_fail("the lobby never became startable: %s" % _issue_keys(l))
				return _finish()
		1:
			var state := _session.state()
			if state == Bp.SESSION_IN_SORTIE:
				var info: Dictionary = _session.info()
				print("lobby_test[solo]: %s in sortie after %d frames: mode %d, kind %d, net %d, seed %d" % [_mode, _phase_frame, info.mode, info.kind, info.net, info.seed])
				var want := Bp.MODE_SKIRMISH if _mode == "skirmish" else Bp.MODE_SANDBOX
				if int(info.mode) != want or int(info.net) != Bp.NET_HOST:
					_fail("the sortie is not the hosted %s: %s" % [_mode, str(info)])
				_next()
			elif state == Bp.SESSION_FAILED or _phase_frame > LOADING_LIMIT:
				_fail("the hosted sortie never started (state %d)" % state)
				return _finish()
		2:
			if _phase_frame >= SORTIE_FRAMES:
				_lobby.leave()
				_next()
		3:
			if _phase_frame > 10:
				if _session.state() != Bp.SESSION_IDLE or _lobby.is_active():
					_fail("leaving did not end the session (state %d)" % _session.state())
				return _finish()
	return false


func _host_step() -> bool:
	match _phase:
		0:
			var panel: Dictionary = _lobby.host_panel()
			if panel.get("bound", false):
				_first_key = String(panel.get("lobby_key", ""))
				_first_code = String(panel.get("join_code", ""))
				if _first_key.length() != 16:
					_fail("the lobby key is not 16 characters: %s" % _first_key)
				_lobby.regenerate_secret()
				if _lobby.host_panel().get("bound", true):
					_fail("a new key did not wait for the port again")
				_next()
			elif _phase_frame > JOIN_LIMIT:
				_fail("the port was never bound")
				return _finish()
		1:
			var panel: Dictionary = _lobby.host_panel()
			if panel.get("bound", false):
				var key := String(panel.get("lobby_key", ""))
				var code := String(panel.get("join_code", ""))
				if key == _first_key or code == _first_code:
					_fail("the new secret did not change the key and code")
				var check: Dictionary = _lobby.check_join_code(code)
				if not check.get("valid", false) or int(check.get("port", 0)) != _port or not check.get("protocol_ok", false):
					_fail("the panel's join code does not check: %s" % str(check))
				if _lobby.check_join_code(_first_code.substr(0, 10)).get("valid", true):
					_fail("half a code checked as valid")
				print("lobby_test[host]: bound on %d, portmap %d, code %s, client limit %d" % [panel.get("port", 0), panel.get("portmap", -1), code, panel.get("client_limit", -1)])
				var f := FileAccess.open(_rendezvous, FileAccess.WRITE)
				f.store_string("%s\n127.0.0.1:%d\n%s\n" % [code, _port, key])
				f.close()
				var refused: Array = _lobby.select_blueprint(STARTER)
				if refused.size() > 0:
					_fail("the starter was refused: %s" % str(refused))
				if not _lobby.set_setup({ "seed": 4242, "time_of_day": Bp.TIME_DUSK }):
					_fail("the host could not change the setup")
				if _lobby.set_setup({ "tonnage_cap_t": 123.0 }):
					_fail("a cap that is no act cap was set")
				_lobby.set_ready(true)
				_next()
			elif _phase_frame > JOIN_LIMIT:
				_fail("the port was never bound with the new key")
				return _finish()
		2:
			var l: Dictionary = _lobby.lobby()
			var players: Array = l.get("players", [])
			# A setup change or a return to the lobby clears every ready flag: ready again.
			var mine := _own(l)
			if not mine.is_empty() and not mine.get("ready", false) and String(mine.get("blueprint_hash", "")) != "" and _phase_frame % 30 == 0:
				_lobby.set_ready(true)
			if players.size() == 2 and l.get("can_start", false):
				var setup: Dictionary = l.get("setup", {})
				if int(setup.get("seed", 0)) != 4242:
					_fail("the working setup lost the seed: %s" % str(setup))
				for p: Dictionary in players:
					if String(p.get("blueprint_name", "")).is_empty():
						_fail("a player's design has no name: %s" % str(p))
				print("lobby_test[host]: round %d lobby can start: %s" % [_round, str(players.map(func(p: Dictionary) -> String: return "'%s' team %d %s %.0f t" % [p.name, p.team, p.blueprint_name, p.tonnage_t]))])
				if not players.any(func(p: Dictionary) -> bool: return String(p.name) == "Secondmate"):
					_fail("the member's display name did not reach the host")
				if not _lobby.start():
					_fail("start was refused")
				_next()
			elif _phase_frame > LOBBY_LIMIT:
				_fail("round %d: the lobby never became startable: %s" % [_round, _issue_keys(l)])
				return _finish()
		3:
			var state := _session.state()
			if state == Bp.SESSION_IN_SORTIE:
				print("lobby_test[host]: round %d in sortie after %d frames" % [_round, _phase_frame])
				_next()
			elif state == Bp.SESSION_FAILED or _phase_frame > LOADING_LIMIT:
				_fail("round %d: the hosted sortie never started (state %d)" % [_round, state])
				return _finish()
		4:
			if _phase_frame >= SORTIE_FRAMES:
				var conns: Array = _session.connections()
				print("lobby_test[host]: %d connection rows, host tick %.1f Hz" % [conns.size(), _session.host_tick_hz()])
				if conns.size() != 2:
					_fail("the connection panel has %d rows" % conns.size())
				if _round == 1:
					# Back to the lobby on the same connection, and once more to sea.
					_session.leave(true)
					if _session.state() != Bp.SESSION_LOBBY or not _lobby.is_active():
						_fail("leave(true) did not return to the lobby (state %d)" % _session.state())
					_round = 2
					_phase = 2
					_phase_frame = 0
				else:
					if _lobby.connection_log_text().is_empty():
						_fail("the host's connection log is empty")
					_lobby.leave()
					_next()
		5:
			if _phase_frame > 10:
				if _session.state() != Bp.SESSION_IDLE or _lobby.is_active():
					_fail("leaving did not end the session (state %d)" % _session.state())
				return _finish()
	return false


func _join_step() -> bool:
	match _phase:
		0:
			if FileAccess.file_exists(_rendezvous):
				var lines := FileAccess.get_file_as_string(_rendezvous).strip_edges().split("\n")
				if lines.size() >= 3:
					if _lobby.join_address("nonsense", lines[2]):
						_fail("a bad address was accepted")
					if _lobby.join_code("NOT-A-CODE"):
						_fail("a bad code was accepted")
					if not _lobby.join_code(lines[0]):
						_fail("join_code refused %s" % lines[0])
						return _finish()
					print("lobby_test[join]: joining by code %s" % lines[0])
					_next()
			elif _phase_frame > JOIN_LIMIT:
				_fail("the host never wrote its code")
				return _finish()
		1:
			if _signals.get("joined", 0) > 0 and not _lobby.lobby().is_empty():
				if _lobby.is_host():
					_fail("a member thinks it is the host")
				if _lobby.set_setup({ "seed": 1 }):
					_fail("a member changed the setup")
				if _lobby.start():
					_fail("a member started the match")
				if _lobby.kick(0):
					_fail("a member kicked the host")
				if _lobby.lobby().get("setup") != null:
					_fail("a member sees the host's working setup")
				_lobby.set_team(1)
				var refused: Array = _lobby.select_blueprint(STARTER)
				if refused.size() > 0:
					_fail("the starter was refused: %s" % str(refused))
				_next()
			elif not _join_failed.is_empty() or _phase_frame > JOIN_LIMIT:
				_fail("the join did not complete: %s" % _join_failed)
				return _finish()
		2:
			# Ready once the team change has gone through (it clears ready), and again after a setup change.
			var mine := _own(_lobby.lobby())
			if int(mine.get("team", 0)) == 1 and not mine.get("ready", false) and String(mine.get("blueprint_hash", "")) != "" and _phase_frame % 30 == 0:
				_lobby.set_ready(true)
			var state := _session.state()
			if state == Bp.SESSION_IN_SORTIE:
				print("lobby_test[join]: round %d in sortie after %d frames; starting signals %d" % [_round, _phase_frame, _signals.get("starting", 0)])
				if _signals.get("starting", 0) < _round:
					_fail("no starting signal")
				_next()
			elif state == Bp.SESSION_FAILED or _phase_frame > LOBBY_LIMIT + LOADING_LIMIT:
				_fail("round %d: the joined sortie never started (state %d)" % [_round, state])
				return _finish()
		3:
			if _round == 1:
				# The host returns to the lobby: our sortie closes and we follow.
				if _session.state() == Bp.SESSION_ENDED:
					_session.leave(true)
					if _session.state() != Bp.SESSION_LOBBY or not _lobby.is_active():
						_fail("leave(true) did not return the member to the lobby (state %d)" % _session.state())
					_round = 2
					_phase = 2
					_phase_frame = 0
				elif _phase_frame > SORTIE_FRAMES + END_LIMIT:
					_fail("the first sortie never closed (state %d)" % _session.state())
					return _finish()
			elif not _left_key.is_empty():
				print("lobby_test[join]: left with %s after %d frames" % [_left_key, _phase_frame])
				if _left_key != "net.end.host_quit" and _left_key != "net.end.host_lost":
					_fail("the member was told %s" % _left_key)
				if _lobby.is_active():
					_fail("the lobby is still active after the host left")
				return _finish()
			elif _phase_frame > SORTIE_FRAMES + END_LIMIT:
				_fail("the member never heard the host leave")
				return _finish()
	return false


func _own(l: Dictionary) -> Dictionary:
	for p: Dictionary in l.get("players", []):
		if p.get("is_local", false):
			return p
	return {}


func _issue_keys(l: Dictionary) -> String:
	var keys: PackedStringArray = []
	for m: Dictionary in l.get("issues", []):
		keys.append(String(m.get("key", "")))
	return ", ".join(keys)


func _count(signal_name: String) -> void:
	_signals[signal_name] = _signals.get(signal_name, 0) + 1


func _on_join_failed(text: Dictionary) -> void:
	_join_failed = String(text.get("key", ""))
	print("lobby_test[%s]: join_failed %s" % [_role, str(text)])


func _on_left(text: Dictionary) -> void:
	_left_key = String(text.get("key", ""))
	if _left_key.is_empty():
		_left_key = "(chose to leave)"


func _next() -> void:
	_phase += 1
	_phase_frame = 0


func _fail(text: String) -> void:
	_failures += 1
	printerr("lobby_test[%s]: FAIL %s" % [_role, text])


func _finish() -> bool:
	print("lobby_test[%s]: %s, signals %s" % [_role, "passed" if _failures == 0 else "%d failures" % _failures, str(_signals)])
	quit(1 if _failures > 0 else 0)
	return true
