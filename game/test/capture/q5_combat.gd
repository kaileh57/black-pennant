extends Node
## Windowed play of one skirmish through the real screens and real input, with viewport captures at the
## moments a player watches: the approach, first contact, the first shots, hits, fire, flooding, a loss and
## the end screen. Orders go in as input events (right-click the sea, click a track, keys), never by calling
## Orders directly.
##   godot --path game --resolution 1920x1080 res://test/capture/q5_combat.tscn -- \
##     --template=9 --act=0 --enemies=1 --seed=7 --tod=2 --sea=3 --out=/abs/dir --name=q5 --minutes=6 \
##     --every=900 --compress=1 --manual=1 --scale=1
## --manual=1 also takes the first gun group by hand for a few seconds and fires it at the reticle.
## Headless, or when instanced by check_project, it does nothing.

const STARTER := "starters/fac22.bp.ron"
const TICK_HZ := 60

var _template := 9
var _act := 0
var _enemies := 1
var _seed := 7
var _tod := 2
var _sea := 3
var _out := "/tmp"
var _name := "q5"
var _minutes := 6.0
var _every := 900
var _compress := true
var _manual := false
var _scale := 1.0
var _bp := STARTER

var _check: RefCounted = null
var _frame := 0
var _sortie_frame := -1
var _active := false
var _shot := 0
var _seen: Dictionary = {}
var _designated := false
var _course_set := false
var _ended_frame := -1
var _last_kinds: Dictionary = {}
var _manual_at := -1


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		var kv := arg.trim_prefix("--").split("=", true, 1)
		if kv.size() != 2:
			continue
		match kv[0]:
			"template": _template = int(kv[1])
			"act": _act = int(kv[1])
			"enemies": _enemies = int(kv[1])
			"seed": _seed = int(kv[1])
			"tod": _tod = int(kv[1])
			"sea": _sea = int(kv[1])
			"out": _out = kv[1]
			"name": _name = kv[1]
			"minutes": _minutes = float(kv[1])
			"every": _every = int(kv[1])
			"compress": _compress = kv[1] == "1"
			"manual": _manual = kv[1] == "1"
			"scale": _scale = float(kv[1])
			"bp": _bp = kv[1]
	DirAccess.make_dir_recursive_absolute(_out)
	Settings.reload()
	Ui.refresh()
	Ui.call(&"_apply_scale", _scale)
	Screens.start_routing()
	_check = ClassDB.instantiate(&"BpSessionCheck")
	var problems: Array = Session.start_skirmish({
		"template": _template, "act": _act, "enemy_per_player": _enemies, "difficulty": {"preset": "normal"},
		"weather": Bp.WEATHER_CLEAR, "sea_state": _sea, "time_of_day": _tod, "seed": _seed, "blueprint": _bp,
	})
	if not problems.is_empty():
		push_error("q5_combat: start_skirmish refused: " + str(problems))
		get_tree().quit(1)
		return
	_active = true


func _process(_delta: float) -> void:
	if not _active:
		return
	_frame += 1
	if _sortie_frame < 0:
		if Session.state() == Bp.SESSION_IN_SORTIE and not Screens.is_loading():
			_sortie_frame = _frame
			print("q5_combat: in sortie at frame ", _frame)
		elif Session.state() == Bp.SESSION_FAILED:
			push_error("q5_combat: the session failed")
			_end(1)
		elif _frame > 3000:
			push_error("q5_combat: no sortie")
			_end(1)
		return
	_check.call(&"observe")
	var since := _frame - _sortie_frame
	var f: Dictionary = _check.call(&"facts")
	var view: Node = Screens.world_view()
	if since == 60:
		_shoot("start")
	if since == 90 and view != null:
		_set_course(view, f)
	if since == 150 and _compress:
		for i in 2:
			_key(KEY_EQUAL)
		print("q5_combat: asked for 4x")
	if not _designated and since > 150 and since % 30 == 0:
		_try_designate(view)
	if since % 600 == 0 and view != null and since > 200:
		_set_course(view, f)
	if _designated and view != null:
		_face_target(view, f)
	_watch(f, since)
	if _manual and _manual_at < 0 and int(_last_kinds.get("engaging", 0)) > 0:
		_manual_at = since
	if _manual_at > 0:
		_drive_manual(view, since - _manual_at)
	if since > 0 and since % _every == 0:
		_shoot("t%04d" % int(float(f.get("tick", 0)) / TICK_HZ))
	if since % 300 == 0:
		print("q5_combat: t=%ds pos=%s kn=%.1f tracks=%d designated=%d kinds=%s" % [int(float(f.get("tick", 0)) / TICK_HZ), str(f.get("pos", Vector2.ZERO)), float(f.get("speed_kn", 0.0)), int(f.get("tracks", 0)), int(f.get("designated", -1)), str(f.get("kinds", {}))])
	if Screens.is_open(&"sortie_end") or Screens.top_name() == &"sortie_end":
		if _ended_frame < 0:
			_ended_frame = _frame
			print("q5_combat: sortie ended at t=", int(float(f.get("tick", 0)) / TICK_HZ))
		if _frame - _ended_frame == 90:
			_shoot("end")
			_end(0)
		return
	if float(f.get("tick", 0)) > _minutes * 60.0 * TICK_HZ:
		_shoot("cap")
		_end(0)


## Orbit the camera toward the designated track, a few degrees a frame, the way a player drags the
## view round to watch the fall of shot. The camera pulls back once so the target is in frame.
var _zoomed := false
func _face_target(view: Node, f: Dictionary) -> void:
	var id := int(f.get("designated", -1))
	var tr: Dictionary = _track(id) if id >= 0 else {}
	if tr.is_empty():
		return
	if not _zoomed:
		view.call(&"zoom", -6.0)
		_zoomed = true
	var size := get_viewport().get_visible_rect().size
	var p: Vector2 = view.call(&"project", Vector2(tr.get("pos", Vector2.ZERO)), 2.0)
	var dx := p.x - size.x * 0.5
	if absf(dx) > size.x * 0.12 or p.x < 0.0 or p.x > size.x:
		view.call(&"orbit", clampf(dx * 0.01, -3.0, 3.0) if p.x > 0.0 and p.x < size.x else 3.0, 0.0)


## Take a capture the first time an event kind appears.
func _watch(f: Dictionary, _since: int) -> void:
	var kinds: Dictionary = f.get("kinds", {})
	for k: String in ["mount_fired", "shell_impact", "fire_started", "breach", "half_flooded", "body_spawned", "body_impact", "decoy_deployed", "status_changed", "vessel_lost", "split"]:
		var n := int(kinds.get(k, 0))
		if n > 0 and not _seen.has(k):
			_seen[k] = _frame
			print("q5_combat: first ", k, " at t=", int(float(f.get("tick", 0)) / TICK_HZ))
	# A short burst after the first occurrence lets the effect develop on screen.
	for k: String in _seen.keys():
		var at: int = _seen[k]
		if _frame - at == 20:
			_shoot("first_" + k)
	_last_kinds = kinds


## Right-click the sea toward the nearest contact, or straight ahead, the way a player sets a course.
func _set_course(view: Node, f: Dictionary) -> void:
	var size := get_viewport().get_visible_rect().size
	var target := Vector2(size.x * 0.5, size.y * 0.42)
	var tracks: PackedInt64Array = _check.call(&"tracks")
	for id: int in tracks:
		var t: Dictionary = _track(id)
		if t.is_empty() or int(t.get("stance", 0)) == Bp.STANCE_FRIEND:
			continue
		var p: Vector2 = view.call(&"project", Vector2(t.get("pos", Vector2.ZERO)), 0.0)
		if p.x > 40.0 and p.x < size.x - 40.0 and p.y > 40.0 and p.y < size.y - 40.0:
			target = p
		break
	var hit: Variant = view.call(&"sea_point", target)
	if hit == null:
		target = Vector2(size.x * 0.5, size.y * 0.55)
	_click(target, MOUSE_BUTTON_RIGHT)
	_course_set = true
	print("q5_combat: right-clicked the sea at ", target, " (ordered ", f.get("ordered_heading_deg", 0.0), ")")


## Click the nearest non-friendly track symbol on screen, as a player designates.
func _try_designate(view: Node) -> void:
	if view == null:
		return
	var size := get_viewport().get_visible_rect().size
	var tracks: PackedInt64Array = _check.call(&"tracks")
	for id: int in tracks:
		var t: Dictionary = _track(id)
		if t.is_empty() or int(t.get("stance", 0)) in [Bp.STANCE_FRIEND, Bp.STANCE_NEUTRAL]:
			continue
		var p: Vector2 = view.call(&"project", Vector2(t.get("pos", Vector2.ZERO)), 2.0)
		if p.x < 40.0 or p.x > size.x - 40.0 or p.y < 40.0 or p.y > size.y - 40.0:
			continue
		_click(p, MOUSE_BUTTON_LEFT)
		_designated = true
		print("q5_combat: clicked track ", id, " at ", p)
		return
	# Off the view: click the edge marker, as a player clicks the chevron at the screen's edge.
	var overlay := get_tree().root.find_child("BridgeOverlay", true, false) as Control
	if overlay == null:
		return
	var laid: Array = overlay.call(&"layout_entries")
	for e: Dictionary in laid[0]:
		var tr: Dictionary = _track(int(e["id"]))
		if tr.is_empty() or int(tr.get("stance", 0)) in [Bp.STANCE_FRIEND, Bp.STANCE_NEUTRAL]:
			continue
		var at: Vector2 = overlay.get_global_transform() * Vector2(e["pos"])
		_click(at, MOUSE_BUTTON_LEFT)
		_designated = true
		print("q5_combat: clicked edge marker of track ", e["id"], " at ", at)
		return


## Take gun group 1 by hand (hold its key), aim at the designated track and fire, then give it back.
func _drive_manual(view: Node, t: int) -> void:
	if view == null:
		return
	if t == 1:
		_key_down(KEY_1, true)
		print("q5_combat: holding group 1")
	if t > 1 and t < 300:
		var d: Dictionary = _check.call(&"facts")
		var id := int(d.get("designated", -1))
		var tr: Dictionary = _track(id) if id >= 0 else {}
		if not tr.is_empty():
			var p: Vector2 = view.call(&"project", Vector2(tr.get("pos", Vector2.ZERO)), 3.0)
			var motion := InputEventMouseMotion.new()
			motion.position = p
			motion.global_position = p
			Input.parse_input_event(motion)
		if t % 20 == 2:
			_click(get_viewport().get_mouse_position(), MOUSE_BUTTON_LEFT)
	if t == 120:
		_shoot("manual_aim")
	if t == 300:
		_key_down(KEY_1, false)
		print("q5_combat: released group 1")
		_manual = false
		_manual_at = -100000


func _track(id: int) -> Dictionary:
	var t: Variant = Hud.track(id)
	return t if t is Dictionary else {}


func _click(canvas_at: Vector2, button: MouseButton) -> void:
	# Input events arrive in window pixels; the HUD and the world view work in the stretched canvas.
	var at := get_viewport().get_final_transform() * canvas_at
	var motion := InputEventMouseMotion.new()
	motion.position = at
	motion.global_position = at
	Input.parse_input_event(motion)
	for pressed: bool in [true, false]:
		var ev := InputEventMouseButton.new()
		ev.button_index = button
		ev.pressed = pressed
		ev.position = at
		ev.global_position = at
		Input.parse_input_event(ev)


func _key(code: Key) -> void:
	_key_down(code, true)
	_key_down(code, false)


func _key_down(code: Key, pressed: bool) -> void:
	var ev := InputEventKey.new()
	ev.physical_keycode = code
	ev.keycode = code
	ev.pressed = pressed
	Input.parse_input_event(ev)


func _shoot(what: String) -> void:
	_shot += 1
	var path := "%s/%s_%02d_%s.png" % [_out, _name, _shot, what]
	var img := get_viewport().get_texture().get_image()
	img.save_png(path)
	print("q5_combat: shot ", path)


func _end(code: int) -> void:
	_active = false
	print("q5_combat: done ", code, " kinds ", _last_kinds)
	get_tree().quit(code)
