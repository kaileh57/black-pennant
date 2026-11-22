extends SceneTree
## Headless checks for the tutorial overlay. Unit: ui-tutorial. Run from game/:
##   godot --headless --path . -s res://test/unit/tutorial_overlay_test.gd
## Exits 1 on any failure. The pure helpers are checked on hand-built data; the card is then driven through
## the real `Tutorial` facade (start, Continue, the allow list, stop) with the overlay mounted on its own.

const SCENE := "res://ui/tutorial/tutorial_overlay.tscn"

var Overlay: GDScript
var _failures := 0
var _checks := 0
var _frame := 0
var _overlay: Node
var _stopped_ms := -1
var _end_frame := -1


func _process(_delta: float) -> bool:
	_frame += 1
	if _frame == 2:
		Overlay = load("res://ui/tutorial/tutorial_overlay.gd")
		_pure()
		_start_flow()
	elif _frame == 4:
		_check_first_step()
		root.get_node("Tutorial").call(&"continue_step")
	elif _frame == 6:
		_check_beam_step()
		root.get_node("Tutorial").call(&"stop")
		_stopped_ms = Time.get_ticks_msec()
	elif _frame > 6 and _stopped_ms >= 0 and Time.get_ticks_msec() - _stopped_ms > 600:
		_stopped_ms = -1
		_end_frame = _frame + 2
		# The overlay polls the state four times a second: Stop emits nothing.
		_check(not (_overlay.get_node("Card") as Control).visible, "the card hides once the tutorial stops")
		_check_finish_text()
		_overlay.queue_free()
		root.get_node("Editor").call(&"close")
	elif _frame == _end_frame:
		print("tutorial_overlay_test: %d checks, %d failures" % [_checks, _failures])
		quit(1 if _failures > 0 else 0)
		return true
	return false


func _check(ok: bool, what: String) -> void:
	_checks += 1
	if not ok:
		_failures += 1
		printerr("FAIL: " + what)


func _near(a: float, b: float) -> bool:
	return absf(a - b) < 1e-6


func _pure() -> void:
	# The beam demonstration: hold, narrow, hold, widen back, hold; ends exactly where it began.
	var from := 5.8
	var low := 4.0
	_check(_near(Overlay.beam_value(0.0, from, low, false), from), "beam starts at its width")
	_check(_near(Overlay.beam_value(6.5, from, low, false), low), "beam is narrowest after the narrowing")
	_check(_near(Overlay.beam_value(8.0, from, low, false), low), "beam holds narrow")
	_check(_near(Overlay.beam_value(18.0, from, low, false), from), "beam ends at its width")
	_check(_near(Overlay.beam_value(99.0, from, low, false), from), "beam stays at its width after the end")
	var prev := from
	var falling := true
	for i: int in range(15, 66):
		var v: float = Overlay.beam_value(i * 0.1, from, low, false)
		falling = falling and v <= prev + 1e-9
		prev = v
	_check(falling, "beam narrows without going back")
	_check(_near(Overlay.beam_value(3.0, from, low, true), low), "reduce motion steps straight to narrow")
	_check(_near(Overlay.beam_value(12.0, from, low, true), from), "reduce motion steps straight back")
	_check(_near(Overlay.beam_value(-1.0, from, low, false), from), "a negative clock keeps the width")
	# The midship handle is the widest beam station; other handles are ignored.
	var handles: Array[Dictionary] = [
		{"id": "draft", "station": 3, "value": 9.0},
		{"id": "beam", "station": 0, "value": 5.2},
		{"id": "beam", "station": 2, "value": 5.8},
		{"id": "beam", "station": 6, "value": 0.6},
	]
	_check(int(Overlay.pick_beam(handles).get("station", -1)) == 2, "pick_beam takes the widest station")
	var none: Array[Dictionary] = [{"id": "draft", "station": 1, "value": 1.0}]
	_check(Overlay.pick_beam(none).is_empty(), "pick_beam with no beam handle is empty")
	# The Fix demonstration previews the blocked-arc note, else the first Fix.
	var notes: Array[Dictionary] = [
		{"index": 0, "has_fix": false, "text": {"key": "notes.arc_blocked"}},
		{"index": 1, "has_fix": true, "text": {"key": "notes.no_director"}},
		{"index": 2, "has_fix": true, "text": {"key": "notes.arc_blocked"}},
	]
	_check(Overlay.pick_fix_note(notes) == 2, "pick_fix_note prefers the blocked arc")
	notes.pop_back()
	_check(Overlay.pick_fix_note(notes) == 1, "pick_fix_note falls back to the first Fix")
	var no_fix: Array[Dictionary] = [{"index": 0, "has_fix": false, "text": "bad"}]
	_check(Overlay.pick_fix_note(no_fix) == -1, "pick_fix_note with no Fix is none")
	# Reward paths take the dock's stock form.
	_check(Overlay.stock_path("starters/fac22.bp.ron") == "stock:starters/fac22.bp.ron", "stock_path adds stock:")
	_check(Overlay.stock_path(" stock:a.bp.ron ") == "stock:a.bp.ron", "stock_path keeps stock:")
	_check(Overlay.stock_path("user://blueprints/a.bp.ron") == "user://blueprints/a.bp.ron", "stock_path keeps user://")
	# Placement: beside the control on the first side with room, never over it or what it must avoid.
	var screen := Rect2(0, 0, 1920, 1080)
	var size := Vector2(600, 180)
	var bounds: Array[Rect2] = [screen]
	var no_avoid: Array[Rect2] = []
	var rest := Vector2(660, 112)
	var mid_target := Rect2(900, 400, 120, 48)
	var cands := _sides(mid_target, size)
	var at: Vector2 = Overlay.place_beside(bounds, mid_target, size, cands, no_avoid, rest)
	_check(at.y >= mid_target.end.y, "a free control gets the card below it")
	var low_target := Rect2(900, 1000, 120, 48)
	at = Overlay.place_beside(bounds, low_target, size, _sides(low_target, size), no_avoid, rest)
	_check(at.y + size.y <= low_target.position.y, "a control at the bottom gets the card above it")
	_check(at.y + size.y <= screen.end.y and at.y >= 0.0, "the card stays on screen")
	var strip: Array[Rect2] = [Rect2(0, 430, 1920, 400)]
	at = Overlay.place_beside(bounds, mid_target, size, cands, strip, rest)
	_check(not Rect2(at, size).intersects(strip[0]), "the card keeps off an avoided strip when it can")
	_check(not Rect2(at, size).intersects(mid_target.grow(8.0)), "the card never covers the control")
	# The basin: clear of every HUD cluster, off the own ship, low on the left when that is free.
	var card := Vector2(624, 219)
	var docks: Array[Rect2] = [Rect2(0, 104, 470, 300), Rect2(0, 710, 470, 370), Rect2(1450, 104, 470, 976), Rect2(651, 120, 617, 290), Rect2(630, 870, 660, 210), Rect2(340, 0, 1240, 96)]
	var ship := Rect2(640, 480, 690, 520)
	var marks: Array[Rect2] = []
	var pref := Vector2(16, 710 - 16 - card.y)
	var found: Array = Overlay.basin_spot(screen, card, docks, ship, marks, pref)
	var box := Rect2(found[0], card)
	_check(float(found[1]) < 0.5 and not box.intersects(ship), "the basin card finds a spot off the own ship: %s" % box)
	for r: Rect2 in docks:
		_check(not box.intersects(r), "the basin card keeps off the cluster %s" % r)
	_check(box.position.x < 470.0 and box.end.y <= 710.0 - 8.0, "the basin card rests low on the left, over the weapons panel: %s" % box)
	# No clear spot: it covers the ship before it covers a cluster.
	var crowded: Array[Rect2] = docks.duplicate()
	crowded.append(Rect2(0, 404, 470, 306))
	found = Overlay.basin_spot(screen, card, crowded, ship, marks, pref)
	box = Rect2(found[0], card)
	for r: Rect2 in crowded:
		_check(not box.grow(8.0).intersects(r), "with only the ship left free the card covers the ship, not the cluster %s: %s" % [r, box])
	var tiny: Array[Rect2] = [Rect2(0, 0, 640, 200)]
	var huge := Rect2(0, 0, 640, 200)
	at = Overlay.place_beside(tiny, huge, size, _sides(huge, size), no_avoid, rest)
	_check(at == rest, "with no room the card rests")


func _sides(t: Rect2, size: Vector2) -> Array[Vector2]:
	var mid := t.get_center()
	return [
		Vector2(mid.x - size.x * 0.5, t.end.y + 16.0),
		Vector2(mid.x - size.x * 0.5, t.position.y - 16.0 - size.y),
		Vector2(t.end.x + 16.0, mid.y - size.y * 0.5),
		Vector2(t.position.x - 16.0 - size.x, mid.y - size.y * 0.5),
	]


func _start_flow() -> void:
	var tutorial := root.get_node("Tutorial")
	_check(bool(tutorial.call(&"start", 0)), "Tutorial.start(0) runs")
	_overlay = (load(SCENE) as PackedScene).instantiate()
	root.add_child(_overlay)


func _check_first_step() -> void:
	var card := _overlay.get_node("Card") as Control
	_check(card.visible, "the overlay pulls the step announced before it existed")
	var text := (_overlay.get_node("Card/Margin/Column/Text") as Label).text
	_check(text == TranslationServer.translate(&"tutorial.open"), "the card shows the open step's text")
	var progress := (_overlay.get_node("Card/Margin/Column/Footer/Progress") as Label).text
	# The card counts the spec's six steps (L252-276), not the script's steps: several script steps make one.
	var step: Dictionary = root.get_node("Tutorial").call(&"step")
	var lessons := int(step.get("lessons", 0))
	_check(lessons == 6 and int(step.get("lesson", 0)) == 1, "the first step is lesson 1 of 6: %s" % str(step.get("lesson")))
	_check(progress == TranslationServer.translate(&"tutorial_overlay.progress").format(["1", "6"]), "the progress line counts 1 of 6: " + progress)
	_check((_overlay.get_node("Card/Margin/Column/Footer/Continue") as Control).visible, "a text step offers Continue")
	_check((_overlay.get_node("Card/Margin/Column/Footer/Skip") as Control).visible, "Skip is always offered")
	_check(not bool(_overlay.call(&"_allowed", "editor.tray.kit.gun")), "the text step keeps the tray inert")
	_check(bool(_overlay.call(&"_allowed", "tutorial.continue")), "the text step allows Continue")


func _check_beam_step() -> void:
	var text := (_overlay.get_node("Card/Margin/Column/Text") as Label).text
	_check(text == TranslationServer.translate(&"tutorial.beam"), "Continue moves the card to the beam step")
	_check(not (_overlay.get_node("Card/Margin/Column/Footer/Continue") as Control).visible, "an action step has no Continue")
	_check(String(_overlay.get(&"_target_id")) == "editor.handle.beam", "the beam step points at the beam handle")
	_check(bool(_overlay.call(&"_allowed", "editor.handle.beam")), "the beam step allows the handle")


func _check_finish_text() -> void:
	var info := {"name": "Fast attack craft", "length_m": 22.0, "displacement_t": 130.0, "parts": 32}
	var added: String = _overlay.call(&"finish_text", &"added", info)
	_check(added.begins_with("Fast attack craft") and added.contains("32"), "the added line names the hull: " + added)
	var unnamed: String = _overlay.call(&"finish_text", &"kept", {})
	_check(unnamed.begins_with(TranslationServer.translate(&"tutorial_overlay.finish.reward_name")), "a nameless reward gets the stand-in name")
	_check(String(_overlay.call(&"finish_text", &"failed", info)) == TranslationServer.translate(&"tutorial_overlay.finish.failed"), "a failed copy says so")
	_check(String(_overlay.call(&"finish_text", &"none", info)) == TranslationServer.translate(&"tutorial_overlay.finish.none"), "no reward has its own line")
