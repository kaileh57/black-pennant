extends Node
## Regression check for suspend and resume (DESIGN.md L112, L826-828): a campaign sortie runs a minute of
## sortie time, is paused and left with Save and leave, then resumed from the suspend slot twice. Each
## resumed sortie must show the player's own hull while still paused, keep the suspend save until its
## first picture, unpause when asked and run on, and suspend again.
## Drives the autoloads the menus call (Campaign, Session, Hud), not the screens, so it runs headless:
##   HOME=<scratch> XDG_DATA_HOME=<scratch>/.local/share godot --headless --path game \
##       res://test/ui/resume_play.tscn
## Never run it with the real HOME: it writes a campaign and a suspend save under the user folder.
## Prints one line per check and exits 0 when every check passed, 1 otherwise.

const SESSION_IN_SORTIE := 3
## Real seconds any one wait may take.
const WAIT_LIMIT_S := 90.0
## Sortie seconds played before the first Save and leave: more than one 60 s checkpoint.
const FIRST_RUN_S := 65.0

var _failed := 0


func _ready() -> void:
	if get_tree().current_scene != self:
		return
	process_mode = Node.PROCESS_MODE_ALWAYS
	_run.call_deferred()


func _check(ok: bool, what: String) -> void:
	print("resume: ", "ok   " if ok else "FAIL ", what)
	if not ok:
		_failed += 1


func _real_s() -> float:
	return Time.get_ticks_msec() / 1000.0


## Wait until `cond` holds or WAIT_LIMIT_S passes; returns whether it held.
func _until(cond: Callable, limit_s := WAIT_LIMIT_S) -> bool:
	var end := _real_s() + limit_s
	while not cond.call():
		if _real_s() > end:
			return false
		await get_tree().process_frame
	return true


func _wait(seconds: float) -> void:
	await get_tree().create_timer(seconds, true).timeout


func _in_sortie() -> bool:
	return Session.state() == SESSION_IN_SORTIE


func _paused() -> bool:
	return Session.pacing().get("paused", false) == true


func _has_suspend() -> bool:
	return Campaign.suspend_slot() != null


## What the pause menu does for Save and leave: pause, then leave (which writes the suspend save) and close
## the campaign.
func _save_and_leave(n: int) -> void:
	Session.set_paused(true)
	await _wait(1.0)
	_check(Session.can_suspend(), "round %d: the sortie can be suspended" % n)
	Session.leave(false)
	Campaign.close_campaign()
	# The menu is back a frame later; nobody clicks Resume sortie sooner.
	await _wait(1.0)
	_check(_has_suspend(), "round %d: Save and leave wrote the suspend save" % n)


func _run() -> void:
	# Round 0: a fresh campaign's first operation.
	var problems: Array = Campaign.new_campaign({"seed": 7, "slot_name": "Resume check"})
	_check(problems.is_empty(), "a new campaign starts %s" % [problems])
	var offers: Array = Campaign.chart().get("offers", [])
	_check(not offers.is_empty(), "the chart offers an operation")
	if offers.is_empty():
		_finish()
		return
	_check(Campaign.open_briefing(offers[0].get("id")), "the briefing opens")
	_check(Campaign.launch_sortie(), "the sortie launches")
	_check(await _until(_in_sortie), "round 0: the sortie shows its first picture")
	_check(await _until(func() -> bool: return Hud.has_ship(), 10.0), "round 0: the player owns a hull")
	Session.request_time_rate(4)
	var start: float = Session.time_s()
	_check(await _until(func() -> bool: return Session.time_s() - start >= FIRST_RUN_S), "round 0: %.0f s of sortie time pass" % FIRST_RUN_S)
	await _save_and_leave(0)

	for n: int in [1, 2]:
		_check(Campaign.resume_suspend(), "round %d: Resume sortie is accepted" % n)
		_check(_has_suspend(), "round %d: the suspend save stays until the first picture" % n)
		_check(await _until(_in_sortie), "round %d: the resumed sortie shows its first picture" % n)
		await get_tree().process_frame
		_check(not _has_suspend(), "round %d: the suspend save is gone once the sortie runs" % n)
		_check(await _until(func() -> bool: return Hud.has_ship(), 10.0), "round %d: the player owns a hull while paused" % n)
		_check(_paused(), "round %d: the sortie resumes paused, as it was left" % n)
		var t0: float = Session.time_s()
		_check(Session.set_paused(false), "round %d: unpause is sent" % n)
		_check(await _until(func() -> bool: return not _paused(), 10.0), "round %d: the sortie unpauses" % n)
		_check(await _until(func() -> bool: return Session.time_s() > t0 + 1.0, 10.0), "round %d: sortie time advances (%.1f s to %.1f s)" % [n, t0, Session.time_s()])
		_check(Hud.has_ship(), "round %d: the player still owns a hull" % n)
		await _save_and_leave(n)
	_finish()


func _finish() -> void:
	print("resume: %s, %d failed" % ["passed" if _failed == 0 else "failed", _failed])
	get_tree().quit(0 if _failed == 0 else 1)
