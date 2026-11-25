extends Node
## Multiplayer play driver (two real game processes). Boots the real main scene with its autoloads and
## router, then plays one side of a lobby with real mouse and key events, saving a capture at each step.
##   HOME=/scratch/a godot --path game --resolution 1920x1080 res://test/capture/mp_play.tscn -- \
##     --role=host --plan=skirmish --dir=/abs/captures [--name=Harbour] [--by=code|address] [--loopback-host]
## Roles talk through files in `--dir`: the host writes `join_code.txt`, `lobby_key.txt` and
## `address.txt` as its host panel shows them; the joiner types what it reads there. Plans: `skirmish`
## (co-op against AI), `pvp` (elimination 1v1). Headless, or when instanced by check_project, it does
## nothing.

const SETTLE := 20
const STEP_LIMIT_S := 60.0

var _role := "host"
var _plan := "skirmish"
var _dir := ""
var _name := ""
var _by := "code"
var _shots := 0
var _play_s := 60.0
var _design := ""
var _team := -1
var _code := ""
## Seconds into the sortie at which this side asks for 4x (negative: never).
var _compress_at := -1.0
## Seconds into the sortie at which this side leaves through the pause menu (negative: never).
var _leave_at := -1.0
## Seconds into the sortie at which this side asks to withdraw (no HUD control exists, so `Orders.withdraw`).
var _withdraw_at := -1.0
## Seconds into the sortie at which this process dies without a word (a crash or a pulled cable).
var _crash_at := -1.0
var _steer := false
var _shot_every := 2


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		var kv := arg.trim_prefix("--").split("=", true, 1)
		if kv.size() != 2:
			continue
		match kv[0]:
			"role": _role = kv[1]
			"plan": _plan = kv[1]
			"dir": _dir = kv[1]
			"name": _name = kv[1]
			"by": _by = kv[1]
			"play": _play_s = float(kv[1])
			"design": _design = kv[1]
			"team": _team = int(kv[1])
			"code": _code = kv[1]
			"compress": _compress_at = float(kv[1])
			"leave_at": _leave_at = float(kv[1])
			"withdraw_at": _withdraw_at = float(kv[1])
			"crash_at": _crash_at = float(kv[1])
			"steer": _steer = kv[1] == "1"
			"shot_every": _shot_every = int(kv[1])
	if _name.is_empty():
		_name = "Harbour" if _role == "host" else "Lantern"
	DirAccess.make_dir_recursive_absolute(_dir)
	_run.call_deferred()


func _run() -> void:
	# Stay alive across the scene change: the boot scene becomes the current scene.
	var root := get_tree().root
	get_parent().remove_child(self)
	root.add_child(self)
	get_tree().change_scene_to_file(ProjectSettings.get_setting("application/run/main_scene"))
	await _frames(10)
	var ok := false
	match _role:
		"host": ok = await _host()
		"join": ok = await _join()
		"typecode": ok = await _typecode()
		_: _log("unknown role " + _role)
	_log("done ok=%s" % ok)
	await _frames(30)
	get_tree().quit(0 if ok else 1)


# --- the two roles --------------------------------------------------------------------------------

func _host() -> bool:
	if not await _to_menu():
		return false
	if not await _open_multiplayer():
		return false
	var mp: Node = Screens.top()
	var mode: OptionButton = mp.get(&"_mode")
	var want: String = {"skirmish": "Skirmish", "pvp": "elimination", "sandbox": "Sandbox"}.get(_plan, "Sandbox")
	for i in mode.item_count:
		if mode.get_item_text(i).to_lower().contains(want.to_lower()):
			await _choose(mode, i)
			break
	await _shot("host_setup")
	await _click(mp.get(&"_host"))
	await _frames(SETTLE)
	await _shot("host_firewall")
	if Screens.modal_count() > 0:
		var b := _find_button(_modal_root(), "host")
		if b != null:
			await _click(b)
	if not await _until(func() -> bool: return Screens.base_name() == &"lobby", "the lobby"):
		await _shot("host_no_lobby")
		return false
	await _frames(SETTLE)
	await _shot("host_lobby")
	# Read the join code the way a player does: from the host panel.
	if not await _until(func() -> bool: return not String(Lobby.host_panel().get("join_code", "")).is_empty(), "a join code", 20.0):
		await _shot("host_no_code")
		return false
	await _frames(SETTLE)
	await _shot("host_panel")
	var lobby: Node = Screens.top()
	var panel: Node = lobby.get(&"_host_panel")
	var code_field: Label = panel.get(&"_code_field")
	var key_field: Label = panel.get(&"_key_field")
	_write("join_code.txt", code_field.text)
	_write("lobby_key.txt", key_field.text)
	var hp := Lobby.host_panel()
	_write("address.txt", "127.0.0.1:%d" % int(hp.get("port", 47610)))
	_log("panel %s" % hp)
	await _field_design(lobby)
	# Wait for the member.
	if not await _until(func() -> bool: return Lobby.lobby().get("players", []).size() >= 2, "a member", 90.0):
		await _shot("host_alone")
		return false
	await _frames(SETTLE)
	await _shot("host_member_joined")
	await _click(lobby.get(&"_ready_toggle"))
	await _frames(SETTLE)
	await _shot("host_ready")
	if not await _until(func() -> bool: return bool(Lobby.lobby().get("can_start", false)), "can start", 60.0):
		await _shot("host_cannot_start")
		_log("lobby %s" % Lobby.lobby())
		return false
	await _frames(SETTLE)
	await _shot("host_can_start")
	await _click(lobby.get(&"_start"))
	return await _sortie()


func _join() -> bool:
	if not await _to_menu():
		return false
	if not await _open_multiplayer():
		return false
	var mp: Node = Screens.top()
	if not await _until(func() -> bool: return FileAccess.file_exists(_dir.path_join("join_code.txt")), "the host's join code", 120.0):
		return false
	await _frames(30)
	if _by == "code":
		var code := FileAccess.get_file_as_string(_dir.path_join("join_code.txt")).strip_edges()
		await _click(mp.get(&"_code_field"))
		await _type(code)
		await _frames(SETTLE)
		await _shot("join_code_typed")
		await _click(mp.get(&"_join_code"))
	else:
		var addr := FileAccess.get_file_as_string(_dir.path_join("address.txt")).strip_edges()
		var key := FileAccess.get_file_as_string(_dir.path_join("lobby_key.txt")).strip_edges()
		await _click(mp.get(&"_address_field"))
		await _type(addr)
		await _click(mp.get(&"_key_field"))
		await _type(key)
		await _frames(SETTLE)
		await _shot("join_address_typed")
		await _click(mp.get(&"_join_address"))
	await _frames(3)
	await _shot("join_connecting")
	if not await _until(func() -> bool: return Screens.base_name() == &"lobby" or Session.state() == Bp.SESSION_LOADING or Session.state() == Bp.SESSION_IN_SORTIE, "the lobby", 40.0):
		await _shot("join_no_lobby")
		return false
	if Screens.base_name() != &"lobby":
		_log("joined a running sortie")
		return await _sortie()
	await _frames(SETTLE)
	await _shot("join_lobby")
	var lobby: Node = Screens.top()
	await _field_design(lobby)
	await _click(lobby.get(&"_ready_toggle"))
	await _frames(SETTLE * 3)
	await _shot("join_ready")
	if not await _until(func() -> bool: return Session.state() == Bp.SESSION_LOADING or Session.state() == Bp.SESSION_IN_SORTIE, "the host to start", 120.0):
		await _shot("join_no_start")
		return false
	return await _sortie()


## Type a join code someone gave us and see what the screen says (a bad or foreign code).
func _typecode() -> bool:
	if not await _to_menu():
		return false
	if not await _open_multiplayer():
		return false
	var mp: Node = Screens.top()
	await _click(mp.get(&"_code_field"))
	await _type(_code)
	await _frames(SETTLE)
	await _shot("typed_code")
	var b: Button = mp.get(&"_join_code")
	_log("join button disabled=%s" % b.disabled)
	await _click(b)
	await _frames(SETTLE * 6)
	await _shot("after_join_press")
	return true


func _leave_sortie() -> void:
	await _key(KEY_ESCAPE)
	await _frames(SETTLE)
	await _shot("%s_pause" % _role)
	var b := _find_button(Screens.top(), "leave the")
	_log("leave button %s" % (b.text if b != null else "none"))
	if b == null:
		return
	await _click(b)
	await _frames(SETTLE)
	await _shot("%s_leave_ask" % _role)
	if Screens.modal_count() > 0:
		var ok := _find_button(_modal_root(), "leave")
		_log("confirm %s" % (ok.text if ok != null else "none"))
		if ok != null:
			await _click(ok)
	await _frames(SETTLE * 3)
	await _shot("%s_left" % _role)


func _field_design(lobby: Node) -> void:
	var picker: Node = lobby.get(&"_picker")
	var list: ItemList = picker.get(&"list")
	await _frames(SETTLE)
	_log("designs: %s" % [range(list.item_count).map(func(i: int) -> String: return list.get_item_text(i))])
	for want: String in (_design.split("|") if not _design.is_empty() else PackedStringArray([""])):
		var pick := -1
		for i in list.item_count:
			var t := list.get_item_text(i).to_lower()
			if not list.is_item_selectable(i):
				continue
			if want.is_empty() or t.contains(want.to_lower()):
				pick = i
				break
		if pick >= 0:
			# Scroll the list until the row shows, as a player would with the wheel.
			var r := list.get_item_rect(pick)
			var bar := list.get_v_scroll_bar()
			if r.end.y - bar.value > list.size.y - 8.0 or r.position.y < bar.value:
				bar.value = maxf(0.0, r.position.y - 8.0)
				await _frames(5)
			await _click_at(list, list.get_item_rect(pick).get_center() - Vector2(0, bar.value))
		await _frames(SETTLE)
		await _shot("%s_design_picked" % _role)
		var field: Button = lobby.get(&"_field")
		_log("field disabled=%s" % field.disabled)
		await _click(field)
		await _frames(SETTLE * 2)
		await _shot("%s_design_fielded" % _role)
	if _team >= 0:
		var team: OptionButton = lobby.get(&"_team")
		if team.is_visible_in_tree():
			await _choose(team, _team)
			await _frames(SETTLE)
			await _shot("%s_team" % _role)


func _on_toast_event(_a: Variant = null, _b: Variant = null) -> void:
	for i in 3:
		await get_tree().create_timer(0.8).timeout
		await _shot("%s_toast_%d" % [_role, i])


func _sortie() -> bool:
	if not Session.is_connected(&"player_dropped", _on_toast_event):
		Session.connect(&"player_dropped", _on_toast_event)
		Session.connect(&"player_joined", _on_toast_event)
	if not await _until(func() -> bool: return Session.state() == Bp.SESSION_IN_SORTIE and not Screens.is_loading(), "the sortie", 90.0):
		await _shot("%s_no_sortie" % _role)
		_log("session state %d" % Session.state())
		return false
	await _frames(60)
	await _shot("%s_sortie_start" % _role)
	if _steer:
		await _key(KEY_H)
		await _frames(10)
		# Hold ahead for the whole sortie, the way a player leans on the key.
		var w := InputEventKey.new()
		w.keycode = KEY_W
		w.physical_keycode = KEY_W
		w.pressed = true
		Input.parse_input_event(w)
	var t0 := Time.get_ticks_msec()
	var n := 0
	var compressed := false
	while (Time.get_ticks_msec() - t0) / 1000.0 < _play_s:
		await get_tree().create_timer(5.0).timeout
		var t := (Time.get_ticks_msec() - t0) / 1000.0
		if not compressed and _compress_at >= 0.0 and t >= _compress_at:
			compressed = true
			# The time_faster key twice: 1x to 2x to 4x (the 4x button can sit under the right dock).
			await _key(KEY_EQUAL)
			await _frames(5)
			await _key(KEY_EQUAL)
			await _frames(30)
			await _shot("%s_asked_4x" % _role)
		if _crash_at >= 0.0 and t >= _crash_at:
			_log("crashing now")
			OS.kill(OS.get_process_id())
		if _withdraw_at >= 0.0 and t >= _withdraw_at:
			_withdraw_at = -1.0
			_log("withdraw sent: %s" % Orders.withdraw())
		if Screens.top_name() == &"sortie_end":
			break
		if _leave_at >= 0.0 and t >= _leave_at:
			await _leave_sortie()
			break
		n += 1
		if n % _shot_every == 0:
			await _shot("%s_sortie_%02d" % [_role, n])
		_log("t=%.0f tick rate %s tracks %d top %s" % [t, "%s" % [Session.pacing().get("rate"), ] + " req=" + str(Session.pacing().get("requested")), Hud.tracks().size(), Screens.top_name()])
		if Session.state() != Bp.SESSION_IN_SORTIE:
			break
	if _leave_at < 0.0:
		await _until(func() -> bool: return Screens.top_name() == &"sortie_end" or Session.state() != Bp.SESSION_IN_SORTIE, "the sortie to end", 30.0)
	_log("after play: state %d top %s" % [Session.state(), Screens.top_name()])
	await _frames(60)
	await _shot("%s_after_play" % _role)
	if Screens.top_name() == &"sortie_end":
		await _frames(SETTLE)
		await _shot("%s_sortie_end" % _role)
		var end: Node = Screens.top()
		var first: Button = null
		for nb: Node in end.find_children("*", "Button", true, false):
			if (nb as Button).is_visible_in_tree():
				_log("end button: %s" % tr((nb as Button).text))
				if first == null:
					first = nb
		first = end.get(&"_primary")
		if first != null:
			await _click(first)
			await _frames(SETTLE * 3)
			await _shot("%s_after_end" % _role)
			_log("after end: base %s state %d" % [Screens.base_name(), Session.state()])
			if Screens.base_name() == &"lobby":
				await get_tree().create_timer(4.0).timeout
				await _shot("%s_lobby_again" % _role)
				_log("lobby again: %s" % [Lobby.lobby().get("players", []).map(func(p: Dictionary) -> String: return "%s ready=%s" % [p.get("name"), p.get("ready")])])
	return true


# --- menus ----------------------------------------------------------------------------------------

func _to_menu() -> bool:
	var t0 := Time.get_ticks_msec()
	while Time.get_ticks_msec() - t0 < 60000:
		await _frames(5)
		if Screens.base_name() == &"main_menu" and Screens.modal_count() == 0:
			await _frames(SETTLE)
			await _shot("%s_menu" % _role)
			return true
		if Screens.base_name() == &"consent":
			await _frames(SETTLE)
			await _shot("%s_consent" % _role)
			var b := _find_button(Screens.top(), "continue")
			if b == null:
				b = _find_button(Screens.top(), "")
			if b != null:
				await _click(b)
				await _frames(10)
	_log("never reached the main menu (base %s)" % Screens.base_name())
	return false


func _open_multiplayer() -> bool:
	var menu: Node = Screens.top()
	var b: Button = menu.get(&"_multiplayer")
	# A window that has just opened can miss its first click; try again as a player would.
	for attempt in 4:
		await _click(b)
		if await _until(func() -> bool: return Screens.top_name() == &"multiplayer", "multiplayer screen", 3.0):
			break
	if Screens.top_name() != &"multiplayer":
		return false
	await _frames(SETTLE)
	var mp: Node = Screens.top()
	var name_field: LineEdit = mp.get(&"_name_field")
	if name_field.text.strip_edges() != _name:
		await _click(name_field)
		await _key(KEY_A, true)
		await _type(_name)
	await _frames(SETTLE)
	await _shot("%s_multiplayer" % _role)
	return true


func _modal_root() -> Node:
	var layer := Screens.get_node_or_null(^"Modal")
	if layer == null:
		for c in Screens.get_children():
			if c is CanvasLayer and (c as CanvasLayer).layer == 70:
				layer = c
	return layer if layer != null and layer.get_child_count() > 0 else Screens


func _find_button(root: Node, text: String) -> Button:
	if root == null:
		return null
	var found: Button = null
	for n: Node in root.find_children("*", "Button", true, false):
		var b := n as Button
		if not b.is_visible_in_tree() or b.disabled:
			continue
		if text.is_empty() or b.text.to_lower().contains(text.to_lower()) or tr(b.text).to_lower().contains(text.to_lower()):
			found = b
	return found


# --- input ----------------------------------------------------------------------------------------

func _screen_point(c: Control, local: Vector2) -> Vector2:
	return c.get_viewport().get_screen_transform() * (c.get_global_transform_with_canvas() * local)


func _click(c: Control) -> void:
	if c == null:
		_log("click on null")
		return
	await _click_at(c, c.size / 2.0)


func _click_at(c: Control, local: Vector2) -> void:
	var p := _screen_point(c, local)
	var motion := InputEventMouseMotion.new()
	motion.position = p
	motion.global_position = p
	Input.parse_input_event(motion)
	await _frames(2)
	for pressed: bool in [true, false]:
		var e := InputEventMouseButton.new()
		e.button_index = MOUSE_BUTTON_LEFT
		e.pressed = pressed
		e.position = p
		e.global_position = p
		Input.parse_input_event(e)
		await _frames(2)


func _choose(o: OptionButton, index: int) -> void:
	await _click(o)
	await _frames(5)
	var popup := o.get_popup()
	if popup.visible:
		o.select(index)
		o.item_selected.emit(index)
		popup.hide()
	await _frames(3)


func _key(code: Key, cmd: bool = false) -> void:
	for pressed: bool in [true, false]:
		var e := InputEventKey.new()
		e.keycode = code
		e.physical_keycode = code
		e.pressed = pressed
		e.meta_pressed = cmd and OS.get_name() == "macOS"
		e.ctrl_pressed = cmd and OS.get_name() != "macOS"
		Input.parse_input_event(e)
		await _frames(1)


func _hold(code: Key, seconds: float) -> void:
	var e := InputEventKey.new()
	e.keycode = code
	e.physical_keycode = code
	e.pressed = true
	Input.parse_input_event(e)
	await get_tree().create_timer(seconds).timeout
	var up := e.duplicate() as InputEventKey
	up.pressed = false
	Input.parse_input_event(up)
	await _frames(2)


func _type(text: String) -> void:
	for ch in text:
		var e := InputEventKey.new()
		e.pressed = true
		e.unicode = ch.unicode_at(0)
		e.keycode = OS.find_keycode_from_string(ch.to_upper()) if ch != " " else KEY_SPACE
		Input.parse_input_event(e)
		var up := e.duplicate() as InputEventKey
		up.pressed = false
		Input.parse_input_event(up)
		await _frames(1)


# --- plumbing -------------------------------------------------------------------------------------

func _frames(n: int) -> void:
	for i in n:
		await get_tree().process_frame


func _until(cond: Callable, what: String, limit_s: float = STEP_LIMIT_S) -> bool:
	var t0 := Time.get_ticks_msec()
	while not cond.call():
		if (Time.get_ticks_msec() - t0) / 1000.0 > limit_s:
			_log("timed out waiting for " + what)
			return false
		await _frames(3)
	return true


func _shot(stem: String) -> void:
	await RenderingServer.frame_post_draw
	var img := get_viewport().get_texture().get_image()
	_shots += 1
	var path := _dir.path_join("%s_%02d_%s.png" % [_role, _shots, stem])
	img.save_png(path)
	_log("shot " + path)


func _write(file: String, text: String) -> void:
	var f := FileAccess.open(_dir.path_join(file), FileAccess.WRITE)
	f.store_string(text)
	f.close()


func _log(text: String) -> void:
	print("mp_play[%s]: %s" % [_role, text])
