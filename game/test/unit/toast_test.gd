extends SceneTree
## Toast check (DESIGN.md L1616), headless. Unit: ui-tests. Run from game/:
##   godot --headless --path . -s res://test/unit/toast_test.gd
## Plays BpTestFeed's scripted 20 minute battle (fire, flooding, alerts of every priority, officer
## lines, the end) through the real router, Session, Hud and sortie HUD, half a second of battle per
## frame, and records every message given to the global toast. The check fails on any sim-event
## toast, more than 1 toast in 60 s of battle time or more than 3 in the battle; only saves, joins or
## leaves and connection loss may toast. It then plays `coop_host_loss` and records what the
## connection loss shows. Negative controls first feed the judge made-up toasts and send one
## sim-event toast through Screens.toast, and check both fail, so neither a lenient judge nor a
## silent recorder can pass. Exits 1 on any failure.

const Gates := preload("res://test/unit/gates.gd")
const TOAST_SCRIPT := "res://ui/widgets/toast.gd"
## Battle ticks a frame: half a second, so cues still arrive spread over many HUD frames.
const CHUNK := 30
const TICK_HZ := 60
## The battle ends at 1,200 s; a little slack, then the run fails.
const BATTLE_LIMIT_S := 1260.0
const HOST_LOSS_LIMIT_S := 240.0


## Stands in for the toast Screens holds: records each message, then shows it on the real one.
class Recorder extends Node:
	var real: Object
	var clock: Callable
	var records: Array[Dictionary] = []

	func show_message(message: Variant) -> void:
		records.append({"t": float(clock.call()), "key": Gates.toast_key(message), "message": message})
		if real != null and is_instance_valid(real):
			real.call(&"show_message", message)

	func dismiss() -> void:
		if real != null and is_instance_valid(real):
			real.call(&"dismiss")

	func is_showing() -> bool:
		return real != null and is_instance_valid(real) and bool(real.call(&"is_showing"))


var _failures := 0
var _checks := 0
var _feed: Node
var _recorder: Recorder
var _ticks := 0
var _priorities := {}
var _lines := 0
var _stray_toasts: Array[Node] = []


func _initialize() -> void:
	_run.call_deferred()


func _ok(cond: bool, what: String) -> void:
	_checks += 1
	if not cond:
		_failures += 1
		printerr("FAIL: ", what)


func _seconds() -> float:
	return float(_ticks) / TICK_HZ


func _frame() -> void:
	# Quality applies its frame cap at start-up; the battle clock is the feed's, so lift it.
	Engine.max_fps = 0
	await process_frame


func _run() -> void:
	await _frame()
	_negative_control()
	var screens: Node = root.get_node("Screens")
	var hud: Node = root.get_node("Hud")
	screens.call(&"start_routing")
	_recorder = Recorder.new()
	_recorder.name = &"ToastRecorder"
	_recorder.real = screens.get(&"_toast")
	_recorder.clock = _seconds
	root.add_child(_recorder)
	screens.set(&"_toast", _recorder)
	# The recorder itself must see what Screens.toast shows, or a swap that stopped intercepting
	# would pass every battle with zero toasts.
	screens.call(&"toast", {"key": &"log.fire.started", "args": []})
	_ok(_recorder.records.size() == 1 and _recorder.records[0]["key"] == "log.fire.started", "control: Screens.toast did not reach the recorder")
	_ok(not Gates.judge_toasts(_recorder.records, 1200.0).is_empty(), "control: a recorded sim-event toast passed")
	_recorder.records.clear()
	_recorder.dismiss()
	hud.connect(&"alert_raised", func(a: Dictionary) -> void: _priorities[int(a.get("priority", -1))] = true)
	hud.connect(&"officer_line", func(_l: Dictionary) -> void: _lines += 1)
	await _battle(screens)
	await _host_loss()
	screens.set(&"_toast", _recorder.real)
	print("toast_test: %d checks, %d failures" % [_checks, _failures])
	quit(1 if _failures > 0 else 0)


func _start(script: String) -> bool:
	if _feed != null:
		_feed.call(&"stop")
		_feed.queue_free()
		await _frame()
		await _frame()
	_feed = ClassDB.instantiate(&"BpTestFeed")
	root.add_child(_feed)
	_ticks = 0
	_recorder.records.clear()
	if not bool(_feed.call(&"start", script)):
		_ok(false, "BpTestFeed.start(%s) refused" % script)
		return false
	_feed.call(&"advance", 12)
	_ticks = 12
	await _frame()
	return true


func _battle(screens: Node) -> void:
	if not await _start("battle"):
		return
	var session: Node = root.get_node("Session")
	var in_sortie := false
	while _seconds() < BATTLE_LIMIT_S:
		_feed.call(&"advance", CHUNK)
		_ticks += CHUNK
		await _frame()
		var state := int(session.call(&"state"))
		in_sortie = in_sortie or state == Bp.SESSION_IN_SORTIE
		if _ticks % (TICK_HZ * 30) < CHUNK:
			_scan_stray_toasts()
		if state == Bp.SESSION_ENDED:
			break
	var ended := int(session.call(&"state")) == Bp.SESSION_ENDED
	_ok(in_sortie, "the battle never reached the sortie state")
	_ok(ended, "the battle did not end by %.0f s (state %d)" % [BATTLE_LIMIT_S, session.call(&"state")])
	# The check means nothing unless the HUD saw the battle the gate names.
	for p: int in [Bp.PRIORITY_CAUTION, Bp.PRIORITY_WARNING, Bp.PRIORITY_ALARM, Bp.PRIORITY_EMERGENCY]:
		_ok(_priorities.has(p), "the HUD never raised an alert of priority %d" % p)
	_ok(_lines >= 5, "only %d officer lines reached the HUD" % _lines)
	await _frame()
	await _frame()
	print("toast_test: battle %.0f s, alert priorities %s, %d officer lines, base %s, top %s" % [
		_seconds(), _priorities.keys(), _lines, screens.call(&"base_name"), screens.call(&"top_name")])
	var records := _recorder.records.duplicate()
	for n: Node in _stray_toasts:
		records.append({"t": _seconds(), "key": "<a second toast widget at %s>" % n.get_path()})
	for r: Dictionary in records:
		print("  toast at %.1f s: %s" % [r["t"], r["key"]])
	var problems := Gates.judge_toasts(records, _seconds())
	for p: String in problems:
		_ok(false, "battle: " + p)
	_ok(problems.is_empty(), "battle toast check")


## The co-op host goes quiet, then is lost: the loss may toast (it is connection loss), nothing
## else may.
func _host_loss() -> void:
	if not await _start("coop_host_loss"):
		return
	var session: Node = root.get_node("Session")
	var seen_sortie := false
	while _seconds() < HOST_LOSS_LIMIT_S:
		_feed.call(&"advance", CHUNK)
		_ticks += CHUNK
		await _frame()
		var state := int(session.call(&"state"))
		seen_sortie = seen_sortie or state == Bp.SESSION_IN_SORTIE
		if seen_sortie and state != Bp.SESSION_IN_SORTIE:
			break
	for _i: int in range(4):
		await _frame()
	_ok(seen_sortie, "coop_host_loss never reached the sortie")
	_ok(int(session.call(&"state")) != Bp.SESSION_IN_SORTIE, "the host loss did not end the sortie by %.0f s" % HOST_LOSS_LIMIT_S)
	print("toast_test: host loss at %.0f s, session state %d, %d toasts" % [_seconds(), session.call(&"state"), _recorder.records.size()])
	for r: Dictionary in _recorder.records:
		print("  toast at %.1f s: %s" % [r["t"], r["key"]])
	for p: String in Gates.judge_toasts(_recorder.records, _seconds()):
		_ok(false, "host loss: " + p)
	_feed.call(&"stop")
	_feed.queue_free()
	_feed = null
	await _frame()


## Any toast widget that is not the one Screens owns (L1605: one toast above every screen).
func _scan_stray_toasts() -> void:
	var owned: Variant = _recorder.real
	for n: Node in root.find_children("*", "Control", true, false):
		var s: Script = n.get_script()
		if s != null and s.resource_path == TOAST_SCRIPT and n != owned and not n in _stray_toasts:
			_stray_toasts.append(n)


## The judge must fail what L1616 fails, or a recorder that never fires would pass.
func _negative_control() -> void:
	var sim_event := [{"t": 200.0, "key": "log.fire.started"}]
	_ok(not Gates.judge_toasts(sim_event, 1200.0).is_empty(), "control: a sim-event toast passed")
	var plain := [{"t": 10.0, "key": ""}]
	_ok(not Gates.judge_toasts(plain, 1200.0).is_empty(), "control: a toast with no key passed")
	var too_close := [{"t": 100.0, "key": "editor_shell.saved"}, {"t": 150.0, "key": "log.network.joined"}]
	_ok(not Gates.judge_toasts(too_close, 1200.0).is_empty(), "control: two toasts inside 60 s passed")
	var spaced := [{"t": 100.0, "key": "editor_shell.saved"}, {"t": 161.0, "key": "log.network.joined"}]
	_ok(Gates.judge_toasts(spaced, 1200.0).is_empty(), "control: two toasts 61 s apart failed")
	var four := []
	for i: int in range(4):
		four.append({"t": 100.0 + 200.0 * i, "key": "editor_shell.saved"})
	_ok(not Gates.judge_toasts(four, 1200.0).is_empty(), "control: four toasts in a battle passed")
	_ok(Gates.judge_toasts([], 1200.0).is_empty(), "control: no toasts failed")
	_ok(Gates.toast_key({"key": &"net.end.host_lost", "args": []}) == "net.end.host_lost", "control: MsgDict key")
	_ok(Gates.toast_allowed("net.drop.left") and not Gates.toast_allowed("alert.fire"), "control: allow list")
