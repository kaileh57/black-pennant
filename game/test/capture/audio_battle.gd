extends Node
## Windowed audio check of a real skirmish (unit audio-player): shows res://world/world.tscn, starts a convoy
## raid through the Session autoload, steers the own hull at the nearest track at 4x, and logs every sound
## the Sound autoload starts next to the simulation events of the same frames:
##   godot --path game --resolution 1280x720 res://test/capture/audio_battle.tscn -- --out=/abs/dir
## Writes <out>/audio_log.tsv (one row per started sound), <out>/audio_events.txt (event and sound tallies,
## loop levels against speed) and two PNGs. Then it plays the test feed's `battle` script (fire, flooding
## and all four alert priorities on the own hull) at 8x for 520 script seconds and writes
## <out>/feed_log.tsv and <out>/feed_events.txt the same way. Headless, and when instanced by check_project,
## it does nothing.

const STARTER := "starters/fac22.bp.ron"
const _Text := preload("res://ui/widgets/ui_text.gd")
## Sortie frames to run, and when to take each shot.
var _limit := 60 * 150
const SHOTS: Array[int] = [600, 3600]

var _out := "user://captures"
var _world: Node3D
var _check: BpSessionCheck
var _log: FileAccess
var _frame := 0
var _sortie_frame := -1
var _shot := 0
var _active := false
var _played: Dictionary = {}
var _speed_samples: Array[String] = []
var _wall_ms := 0
var _range_m := -1.0
## 0: the sandbox; 1: the test feed's battle script.
var _phase := 0
var _feed: BpTestFeed
var _feed_ticks := 0
## Script seconds of the battle script to play, and ticks per frame.
const FEED_S := 520
const FEED_STEP := 8


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	var feed_only := false
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--out="):
			_out = arg.trim_prefix("--out=")
		feed_only = feed_only or arg == "--feed-only"
		if arg.begins_with("--seconds="):
			_limit = 60 * int(arg.trim_prefix("--seconds="))
	DirAccess.make_dir_recursive_absolute(_out)
	_log = FileAccess.open(_out.path_join("audio_log.tsv"), FileAccess.WRITE)
	_log.store_line("frame\tsortie_s\tkind\tsound\tbus\tvolume_db\tpitch\tpositional\tdistance_m\tpriority")
	Sound.played.connect(_on_played)
	_check = BpSessionCheck.new()
	_world = (load("res://world/world.tscn") as PackedScene).instantiate() as Node3D
	add_child(_world)
	if feed_only:
		_active = true
		_start_feed()
		return
	# A sandbox with two hostile gunboats (they start 9 km out), so the guns talk within a few minutes.
	var problems := Session.start_sandbox({
		"blueprint": STARTER, "weather": Bp.WEATHER_CLEAR, "sea_state": 4, "time_of_day": Bp.TIME_DUSK, "seed": 4242,
		"targets": [{"blueprint": "combine/act1/gunboat.bp.ron", "hostile": true, "count": 2}],
	})
	if not problems.is_empty():
		push_error("audio_battle: start_skirmish refused: " + str(problems))
		get_tree().quit(1)
		return
	_active = true
	_wall_ms = Time.get_ticks_msec()


func _sortie_s() -> float:
	if _phase == 1:
		return float(_feed_ticks) / 60.0
	return float(_check.facts().get("tick", 0)) / 60.0


func _on_played(info: Dictionary) -> void:
	var key := "%s %s" % [info.get("kind"), info.get("sound")]
	_played[key] = int(_played.get(key, 0)) + 1
	_log.store_line("%d\t%.2f\t%s\t%s\t%s\t%.1f\t%.2f\t%s\t%.0f\t%d" % [
		_frame, _sortie_s(), info.get("kind"), info.get("sound"), info.get("bus"), float(info.get("volume_db", 0.0)),
		float(info.get("pitch", 1.0)), info.get("positional"), float(info.get("distance_m", 0.0)), int(info.get("priority", 0))])


func _process(_delta: float) -> void:
	if not _active:
		return
	_frame += 1
	if _phase == 1:
		_feed_frame()
		return
	_check.observe()
	if _sortie_frame < 0:
		if Session.state() == Bp.SESSION_IN_SORTIE:
			_sortie_frame = _frame
			print("audio_battle: rate up ", Session.step_time_rate(1), " ", Session.step_time_rate(1), " pacing ", Session.pacing())
		elif Session.state() == Bp.SESSION_FAILED or _frame > 2400:
			push_error("audio_battle: the sortie did not start")
			_end(1)
		return
	var since := _frame - _sortie_frame
	if since % 30 == 0:
		_steer(since)
	if since % 300 == 0:
		var st: Dictionary = Sound.debug_state()
		var m: Dictionary = Hud.motion()
		var loops: Dictionary = st["loops"]
		var parts: Array[String] = []
		for stem: StringName in loops:
			var l: Dictionary = loops[stem]
			parts.append("%s %.1f dB x%.2f" % [stem, float(l.get("volume_db", 0.0)), float(l.get("pitch", 1.0))])
		_speed_samples.append("t %.0f s range %.0f m speed %.1f kn power %.2f | %s" % [_sortie_s(), _range_m, float(m.get("speed_kn", 0.0)), float(m.get("power", 0.0)), ", ".join(parts)])
	if _shot < SHOTS.size() and since == SHOTS[_shot]:
		get_viewport().get_texture().get_image().save_png(_out.path_join("audio_battle_%d.png" % _shot))
		_shot += 1
	if since >= _limit or Session.state() != Bp.SESSION_IN_SORTIE:
		_report("audio_events.txt")
		Session.leave(false)
		_start_feed()


## Head for the nearest track that is not a friend at most speed, designate it, and once inside 5 km take
## the gun mounts and fire them at it by hand, so shots, splashes and hits are heard however doctrine holds.
func _steer(since: int) -> void:
	var f := _check.facts()
	var own: Vector2 = f.get("pos", Vector2.ZERO)
	var best := -1
	var best_d := INF
	var best_pos := Vector2.ZERO
	for t: Dictionary in Hud.tracks():
		if int(t.get("stance", Bp.STANCE_FRIEND)) == Bp.STANCE_FRIEND:
			continue
		var p: Vector2 = t.get("pos", Vector2.ZERO)
		var d := own.distance_to(p)
		if d < best_d:
			best_d = d
			best = int(t.get("id", -1))
			best_pos = p
	var top := float(Hud.motion_limits().get("max_speed_kn", 20.0)) * 0.95
	_range_m = best_d if best > 0 else -1.0
	if best > 0:
		_check.set_course(best_pos, top if best_d > 2500.0 else top * 0.5)
		_check.designate(best)
		if best_d < 5000.0:
			for slot: int in f.get("guns", PackedInt32Array()):
				_check.take_mount(slot, true)
				_check.fire_mount(slot, best)
	elif since < 200:
		var heading := deg_to_rad(float(f.get("heading_deg", 0.0)))
		_check.set_course(own + Vector2(sin(heading), -cos(heading)) * 5000.0, top)


func _start_feed() -> void:
	_log.close()
	_log = FileAccess.open(_out.path_join("feed_log.tsv"), FileAccess.WRITE)
	_log.store_line("frame\tscript_s\tkind\tsound\tbus\tvolume_db\tpitch\tpositional\tdistance_m\tpriority")
	_played.clear()
	_speed_samples.clear()
	_check = BpSessionCheck.new()
	_feed = BpTestFeed.new()
	if not _feed.start("battle"):
		push_error("audio_battle: the battle script did not start")
		_end(1)
		return
	_phase = 1
	_feed_ticks = 0
	_wall_ms = Time.get_ticks_msec()


func _feed_frame() -> void:
	_feed.advance(FEED_STEP)
	_feed_ticks += FEED_STEP
	_check.observe()
	if _feed_ticks % (60 * 20) < FEED_STEP:
		var st: Dictionary = Sound.debug_state()
		var loops: Dictionary = st["loops"]
		var parts: Array[String] = []
		for stem: StringName in loops:
			var l: Dictionary = loops[stem]
			parts.append("%s %.1f dB" % [stem, float(l.get("volume_db", 0.0))])
		_speed_samples.append("t %.0f s | %s" % [_sortie_s(), ", ".join(parts)])
	if _feed_ticks == 60 * 200:
		get_viewport().get_texture().get_image().save_png(_out.path_join("feed_battle.png"))
	if _feed_ticks >= 60 * FEED_S:
		_report("feed_events.txt")
		_feed.stop()
		_feed.free()
		_end(0)


func _report(file: String) -> void:
	var f := _check.facts()
	var out := FileAccess.open(_out.path_join(file), FileAccess.WRITE)
	out.store_line("wall s %.1f, sortie s %.1f, frames %d" % [(Time.get_ticks_msec() - _wall_ms) / 1000.0, _sortie_s(), _frame])
	out.store_line("sim events: " + str(f.get("kinds", {})))
	out.store_line("projectiles %s own %s mount_fired %s" % [f.get("projectiles"), f.get("own_projectiles"), f.get("mount_fired")])
	var keys: Array = _played.keys()
	keys.sort()
	for k: String in keys:
		out.store_line("%6d  %s" % [_played[k], k])
	out.store_line("pacing " + str(Session.pacing()))
	for e: Dictionary in Hud.log_entries({}, 0, 40):
		out.store_line("log: " + _Text.resolve(e.get("text")))
	for line: String in _speed_samples:
		out.store_line(line)
	out.close()


func _end(code: int) -> void:
	_active = false
	_log.close()
	print("audio_battle: done, ", _out)
	get_tree().quit(code)
