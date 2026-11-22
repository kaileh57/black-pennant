extends Node
## Autoload `Sound`. Plays what AudioFeed asks for: one-shots from AudioFeed.drain_events() and loops
## from AudioFeed.loops(), with the volume settings. DESIGN.md L1242-1247. Unit: audio-player.
## Files: `sound` in each AudioEventDict and AudioLoopDict is a file stem of the Rust `audio` crate's
## catalogue; the file is res://audio/gen/<sound>.wav (written by `cargo xtask gen`, never committed).
## AudioFeed.sound_ids() lists every stem and AudioFeed.sound_info(stem) says its bus, whether it
## loops and whether it is positional. This script names no sound itself: interface sounds are
## requested elsewhere with AudioFeed.ui_sound(Bp.UI_SOUND_*).
## Buses: `bus` is one of Sfx, Ambience, Alert, Radio, Ui, Music, the names in
## res://audio/bus_layout.tres. Settings map onto them: audio.effects to Sfx, audio.ambience to
## Ambience, audio.alerts to Alert, audio.radio to Radio, audio.interface to Ui, audio.menu_theme to
## Music, audio.master to Master.
## Loops: loop points are in each WAV's smpl chunk and the default import reads them. A loop missing
## from AudioFeed.loops() fades out over 150 ms (the menu theme, on Music, over 1 s). alert_alarm is
## a one-shot pattern: restart it for as long as it is listed.
## Reads and calls: AudioFeed.drain_events, AudioFeed.loops, AudioFeed.loops_rev, AudioFeed.listener,
## AudioFeed.sound_ids, AudioFeed.sound_info, Settings.get_value, Session.pacing.
##
## How it plays:
## - Every stem is loaded once at start-up on the loader thread. A stem whose file is missing (no
##   `cargo xtask gen` yet) is silent; the gap is reported once, as a warning, never per frame.
## - One-shots take a voice from a fixed pool per bus (positional ones from a pool of
##   AudioStreamPlayer3D on Sfx and Ambience). A full pool steals its lowest-priority, then quietest,
##   then oldest voice, and only for a sound of at least that priority; an alert always gets a voice.
## - Positions only pan: `volume_db` already carries the distance law and air absorption, so the 3D
##   players have attenuation, the distance low-pass and doppler switched off. The listener is an
##   AudioListener3D placed each frame at AudioFeed.listener().
## - `delay_s` is the speed-of-sound delay in sortie seconds: it runs at the sortie's time rate and
##   holds while the sortie is paused. Waiting sounds are dropped when the sortie ends.
## - Loops keep one voice per `id`. Volume, pitch and position glide to each 30 Hz update over 40 ms
##   so steps do not zipper; a new loop fades in over 60 ms (music over 1 s); beds and impact loops
##   start at a random point of the file so two fire loops never play in phase.
## - Volume settings are slider positions 0..1 mapped to amplitude as the square of the position
##   (0.5 is -12 dB, the menu theme's default 0.7 is -6 dB); 0 mutes the bus.
## - Interface feedback is hooked here once for the whole tree, not screen by screen: every button
##   press asks AudioFeed.ui_sound for a click; a toggle, an option picked, a tab changed or a slider
##   moved (at most every 70 ms) for a select tick, and only within 300 ms of a real press of a key,
##   mouse button, pad button or touch, so state set from code is silent. A control with the meta
##   `ui_sound` set to false
##   stays silent (for controls whose action plays its own sound, such as the dry dock's place and
##   remove, which the Rust editor queues).

## Emitted for every one-shot a voice starts and every loop that starts or stops, for logs and tests:
## `{ "kind": "one_shot" | "loop_start" | "loop_stop", "sound", "bus", "volume_db", "pitch",
## "positional", "distance_m" (listener to source, 0 when flat), "priority" }`. Nothing connects to it in
## play, so it costs one emit with no listener.
signal played(info: Dictionary)

## Buses of the layout, in layout order, and the setting each follows.
const BUSES: Array[StringName] = [&"Sfx", &"Ambience", &"Alert", &"Radio", &"Ui", &"Music"]
const BUS_SETTINGS: Dictionary[StringName, StringName] = {
	&"Master": &"audio.master",
	&"Sfx": &"audio.effects",
	&"Ambience": &"audio.ambience",
	&"Alert": &"audio.alerts",
	&"Radio": &"audio.radio",
	&"Ui": &"audio.interface",
	&"Music": &"audio.menu_theme",
}
## One-shot voices per bus: flat (non-positional) and positional. The feed starts at most 24 one-shots
## a frame.
const FLAT_VOICES: Dictionary[StringName, int] = {
	&"Sfx": 10, &"Ambience": 4, &"Alert": 4, &"Radio": 4, &"Ui": 8, &"Music": 1,
}
const POSITIONAL_VOICES: Dictionary[StringName, int] = {&"Sfx": 28, &"Ambience": 4}
## Most loop voices alive at once, leaving ones included (the feed lists about 30 at its busiest).
const MAX_LOOPS := 48
## Most one-shots waiting on a speed-of-sound delay or a file still loading.
const MAX_PENDING := 128
## Longest delay honoured, sortie seconds (sound travels about 10 km in 30 s).
const MAX_DELAY_S := 30.0
## A due one-shot whose file is still loading is dropped after this many wall seconds.
const LOAD_WAIT_S := 1.0
## Level window: quieter sounds are not started; louder ones are clamped to AudioStreamPlayer3D's
## max_db ceiling.
const SILENT_DB := -60.0
const MAX_DB := 6.0
const MIN_PITCH := 0.05
const MAX_PITCH := 16.0
## Loop envelopes, seconds.
const LOOP_FADE_OUT_S := 0.15
const MUSIC_FADE_OUT_S := 1.0
const LOOP_FADE_IN_S := 0.06
const MUSIC_FADE_IN_S := 1.0
const LOOP_GLIDE_S := 0.04
## Buses `hold_world` mutes: everything a sortie sounds. Ui and Music stay live.
const WORLD_BUSES: Array[StringName] = [&"Sfx", &"Ambience", &"Alert", &"Radio"]
## After `hold_world(false)` the world buses stay muted this long, so the held sortie's loops finish
## fading out unheard.
const HOLD_RELEASE_S := 0.5
## Shortest gap between two slider ticks, milliseconds.
const SLIDER_TICK_MS := 70
## An interface signal sounds only this soon after a press or release of a key, mouse button, pad button
## or touch:
## a toggle, tab or option set from code (a screen restoring its state) stays silent.
const INPUT_RECENT_MS := 300
## Slider positions below this mute the bus.
const MUTE_BELOW := 0.005
## Buses whose true loops start at a random point of the file.
const RANDOM_START_BUSES: Array[StringName] = [&"Sfx", &"Ambience", &"Radio"]
const GEN_DIR := "res://audio/gen/"
## At exit, how long the main thread waits after stopping voices that were playing. A stopped playback
## stays registered with the AudioServer until its mixer thread has faded it out and dropped it, and no
## frame runs after `_exit_tree`; quitting sooner leaves the voices' AudioStreamWAV and
## AudioStreamPlaybackWAV alive ("ERROR: N resources still in use at exit"). 50 ms was not always enough
## on the headless dummy driver; 100 to 300 ms always were.
const EXIT_DRAIN_MS := 200
## Session states whose delayed one-shots may still sound.
const SORTIE_STATES: Array[int] = [Bp.SESSION_LOADING, Bp.SESSION_IN_SORTIE, Bp.SESSION_ENDED, Bp.SESSION_REPLAY]


## One one-shot voice and what it is playing.
class Voice:
	extends RefCounted
	var player: Node
	var priority: int = 0
	var volume_db: float = 0.0
	var started_ms: int = 0

	func busy() -> bool:
		return player.get(&"playing") == true


## One loop voice and its envelope.
class LoopVoice:
	extends RefCounted
	var player: Node
	var positional: bool = false
	var sound: StringName = &""
	var bus: StringName = &""
	## Linear amplitude of the fade envelope, 0..1.
	var env: float = 0.0
	var fade_in_s: float = 0.06
	var fade_out_s: float = 0.15
	var leaving: bool = false
	var target_db: float = -60.0
	var level_db: float = -60.0
	var target_pitch: float = 1.0
	var target_pos: Vector3 = Vector3.ZERO


## A one-shot waiting for its delay or its file.
class Pending:
	extends RefCounted
	var event: Dictionary = {}
	var stem: StringName = &""
	var delay_s: float = 0.0
	var waited_s: float = 0.0
	var priority: int = 0


## stem -> sound_info Dictionary.
var _info: Dictionary[StringName, Dictionary] = {}
## stem -> loaded stream; absent while loading or when the file is missing.
var _streams: Dictionary[StringName, AudioStream] = {}
## stem -> resource path still loading on the loader thread.
var _loading: Dictionary[StringName, String] = {}
## Stems whose file is missing or failed to load.
var _missing: Dictionary[StringName, bool] = {}
## bus -> Array of Voice.
var _flat: Dictionary[StringName, Array] = {}
var _positional: Dictionary[StringName, Array] = {}
## Loop id -> LoopVoice. Voices replaced under a live id keep fading under a negative private id.
var _loops: Dictionary[int, LoopVoice] = {}
var _next_leaving_id: int = -1
## Stopped loop players kept for reuse.
var _spare_loop_players: Array[Node] = []
var _loop_player_count: int = 0
var _loops_rev: int = -1
var _pending: Array[Pending] = []
var _listener: AudioListener3D
## Sortie seconds per wall second: 0 while paused.
var _sim_rate: float = 1.0
var _feed: Node
var _session: Node
var _settings: Node
var _last_slider_tick_ms := -1_000
var _world_held := false
var _hold_serial := 0
var _last_press_ms := -1_000_000


func _ready() -> void:
	process_mode = Node.PROCESS_MODE_ALWAYS
	# After AudioFeed (-70) and after the scene nodes that request interface sounds in their own
	# _process, so every sound asked for in a frame starts in that frame.
	process_priority = 10
	_feed = get_node_or_null(^"/root/AudioFeed")
	_session = get_node_or_null(^"/root/Session")
	_settings = get_node_or_null(^"/root/Settings")
	_listener = AudioListener3D.new()
	_listener.name = &"Listener"
	add_child(_listener)
	_listener.make_current()
	_build_pools()
	_load_catalogue()
	get_tree().node_added.connect(_hook_control)
	_hook_tree(get_tree().root)
	_apply_all_volumes()
	if _settings != null and _settings.has_signal(&"changed"):
		_settings.connect(&"changed", _on_setting_changed)
	if _session != null:
		if _session.has_signal(&"pacing_changed"):
			_session.connect(&"pacing_changed", _on_pacing_changed)
		if _session.has_signal(&"state_changed"):
			_session.connect(&"state_changed", _on_session_state)
		_on_pacing_changed()


func _exit_tree() -> void:
	# Quitting with voices playing would leave their playbacks registered with the AudioServer: stop
	# them, then give the mixer thread time to drop them (EXIT_DRAIN_MS).
	var was_playing := stop_all_voices()
	if was_playing:
		OS.delay_msec(EXIT_DRAIN_MS)
	# Collect loads still in flight, or their loader tokens outlive the engine.
	for stem: StringName in _loading:
		ResourceLoader.load_threaded_get(_loading[stem])
	_loading.clear()
	_loops.clear()
	_pending.clear()
	_streams.clear()
	_flat.clear()
	_positional.clear()
	_spare_loop_players.clear()


## Stop every voice and drop its stream. True when any voice still held a stream: it was playing, or
## stopped so recently (a loop that just faded out) that its playback may still be registered with the
## AudioServer until the mixer thread lets it go.
func stop_all_voices() -> bool:
	var was_playing := false
	for child: Node in get_children():
		if child is AudioStreamPlayer or child is AudioStreamPlayer3D:
			if child.get(&"playing") == true or child.get(&"stream") != null:
				was_playing = true
			child.call(&"stop")
			child.set(&"stream", null)
	return was_playing


func _process(delta: float) -> void:
	if _feed == null:
		return
	var dt: float = delta if is_finite(delta) and delta > 0.0 else 0.0
	_poll_loading()
	_place_listener()
	var events: Array = _feed.call(&"drain_events")
	for item: Variant in events:
		if item is Dictionary:
			_take_event(item)
	_run_pending(dt)
	var rev: int = int(_feed.call(&"loops_rev"))
	if rev != _loops_rev:
		_loops_rev = rev
		_set_loops(_feed.call(&"loops"))
	_run_loops(dt)


# Catalogue ------------------------------------------------------------------------------------------

func _load_catalogue() -> void:
	var ids: PackedStringArray = _feed.call(&"sound_ids") if _feed != null else PackedStringArray()
	for stem_text: String in ids:
		var stem := StringName(stem_text)
		var info_v: Variant = _feed.call(&"sound_info", stem)
		if not info_v is Dictionary or (info_v as Dictionary).is_empty():
			continue
		_info[stem] = info_v
		var path := GEN_DIR + stem_text + ".wav"
		if not ResourceLoader.exists(path):
			_missing[stem] = true
			continue
		if ResourceLoader.load_threaded_request(path, "AudioStream") == OK:
			_loading[stem] = path
		else:
			_missing[stem] = true
	if not _missing.is_empty():
		push_warning("Sound: %d of %d sound files are missing under %s; run cargo xtask gen." % [_missing.size(), _info.size(), GEN_DIR])


func _poll_loading() -> void:
	if _loading.is_empty():
		return
	var done: Array[StringName] = []
	for stem: StringName in _loading:
		var path: String = _loading[stem]
		var status := ResourceLoader.load_threaded_get_status(path)
		if status == ResourceLoader.THREAD_LOAD_IN_PROGRESS:
			continue
		done.append(stem)
		if status == ResourceLoader.THREAD_LOAD_LOADED:
			var stream := ResourceLoader.load_threaded_get(path) as AudioStream
			if stream != null:
				_streams[stem] = stream
				continue
		_missing[stem] = true
		push_warning("Sound: could not load %s." % path)
	for stem: StringName in done:
		_loading.erase(stem)


## The stream of a stem, or null while it loads or when it is missing.
func _stream(stem: StringName) -> AudioStream:
	return _streams.get(stem, null)


# Pools ----------------------------------------------------------------------------------------------

func _build_pools() -> void:
	for bus: StringName in BUSES:
		var flat: Array = []
		for i: int in range(FLAT_VOICES.get(bus, 0)):
			flat.append(_new_voice(bus, false, "%s_%d" % [bus, i]))
		_flat[bus] = flat
		var positional: Array = []
		for i: int in range(POSITIONAL_VOICES.get(bus, 0)):
			positional.append(_new_voice(bus, true, "%s_3d_%d" % [bus, i]))
		_positional[bus] = positional


func _new_voice(bus: StringName, positional: bool, node_name: String) -> Voice:
	var v := Voice.new()
	v.player = _new_player(bus, positional)
	v.player.name = node_name
	add_child(v.player)
	return v


func _new_player(bus: StringName, positional: bool) -> Node:
	if positional:
		var p := AudioStreamPlayer3D.new()
		# Rust applies the distance law; the position only pans.
		p.attenuation_model = AudioStreamPlayer3D.ATTENUATION_DISABLED
		p.attenuation_filter_db = 0.0
		p.doppler_tracking = AudioStreamPlayer3D.DOPPLER_TRACKING_DISABLED
		p.max_db = MAX_DB
		p.area_mask = 0
		p.bus = bus
		return p
	var f := AudioStreamPlayer.new()
	f.bus = bus
	return f


## A free voice of the pool, or the one to steal for a sound of `priority` (null: drop the sound).
func _pick_voice(pool: Array, priority: int, bus: StringName) -> Voice:
	var victim: Voice = null
	for v: Voice in pool:
		if not v.busy():
			return v
		if victim == null or _steal_before(v, victim):
			victim = v
	if victim == null:
		return null
	# An alert is never lost; elsewhere a quieter-priority newcomer gives way.
	if bus != &"Alert" and victim.priority > priority:
		return null
	return victim


## True when `a` should be stolen before `b`: lower priority, then quieter, then older.
static func _steal_before(a: Voice, b: Voice) -> bool:
	if a.priority != b.priority:
		return a.priority < b.priority
	if not is_equal_approx(a.volume_db, b.volume_db):
		return a.volume_db < b.volume_db
	return a.started_ms < b.started_ms


# One-shots ------------------------------------------------------------------------------------------

func _take_event(event: Dictionary) -> void:
	var stem := _stem_of(event)
	if stem == &"" or _missing.has(stem):
		return
	var delay: float = clampf(_num(event.get("delay_s", 0.0), 0.0), 0.0, MAX_DELAY_S)
	if delay > 0.0 or _stream(stem) == null:
		_queue(stem, event, delay)
		return
	_start_one_shot(stem, event)


## The stem of an event or loop when it names a known sound, else &"".
func _stem_of(d: Dictionary) -> StringName:
	var raw: Variant = d.get("sound", &"")
	if not (raw is StringName or raw is String):
		return &""
	var stem := StringName(raw)
	return stem if _info.has(stem) else &""


func _queue(stem: StringName, event: Dictionary, delay: float) -> void:
	var p := Pending.new()
	p.event = event
	p.stem = stem
	p.delay_s = delay
	p.priority = int(_num(event.get("priority", 0), 0.0))
	if _pending.size() >= MAX_PENDING:
		# Drop the lowest-priority waiting sound, or the newcomer when it is lower still.
		var low := 0
		for i: int in range(_pending.size()):
			if _pending[i].priority < _pending[low].priority:
				low = i
		if _pending[low].priority > p.priority:
			return
		_pending.remove_at(low)
	_pending.append(p)


func _run_pending(dt: float) -> void:
	if _pending.is_empty():
		return
	var sim_dt := dt * _sim_rate
	var keep: Array[Pending] = []
	for p: Pending in _pending:
		p.delay_s -= sim_dt
		if p.delay_s > 0.0:
			keep.append(p)
			continue
		if _stream(p.stem) != null:
			_start_one_shot(p.stem, p.event)
			continue
		p.waited_s += dt
		if not _missing.has(p.stem) and p.waited_s < LOAD_WAIT_S:
			keep.append(p)
	_pending = keep


func _start_one_shot(stem: StringName, event: Dictionary) -> void:
	var info: Dictionary = _info[stem]
	var volume := _num(event.get("volume_db", 0.0), SILENT_DB - 1.0)
	if volume < SILENT_DB:
		return
	volume = minf(volume, MAX_DB)
	var bus := _bus_of(event, info)
	var pos: Variant = event.get("pos", null)
	var positional := _wants_position(event, info)
	var pool: Array = _positional.get(bus, []) if positional else _flat.get(bus, [])
	if pool.is_empty():
		# A positional sound on a bus without 3D voices plays flat.
		positional = false
		pool = _flat.get(bus, [])
	if pool.is_empty():
		return
	var priority := int(_num(event.get("priority", 0), 0.0))
	var v := _pick_voice(pool, priority, bus)
	if v == null:
		return
	v.priority = priority
	v.volume_db = volume
	v.started_ms = Time.get_ticks_msec()
	var player := v.player
	player.call(&"stop")
	player.set(&"stream", _stream(stem))
	player.set(&"volume_db", volume)
	player.set(&"pitch_scale", clampf(_num(event.get("pitch", 1.0), 1.0), MIN_PITCH, MAX_PITCH))
	if positional:
		var p3 := player as AudioStreamPlayer3D
		p3.position = pos
		p3.max_distance = _cull_distance(event)
	player.call(&"play")
	if played.has_connections():
		played.emit({
			"kind": "one_shot", "sound": stem, "bus": bus, "volume_db": volume,
			"pitch": float(player.get(&"pitch_scale")), "positional": positional,
			"distance_m": _listener.global_position.distance_to(pos) if positional else 0.0,
			"priority": priority,
		})


## True when a sound should be panned: the catalogue says positional, the event agrees and its
## position is a finite point.
static func _wants_position(d: Dictionary, info: Dictionary) -> bool:
	var pos: Variant = d.get("pos", null)
	return d.get("positional", false) == true and info.get("positional", false) == true \
		and pos is Vector3 and (pos as Vector3).is_finite()


## The layout bus a sound plays on: the event's when it is a layout bus, else the catalogue's.
static func _bus_of(d: Dictionary, info: Dictionary) -> StringName:
	var raw: Variant = d.get("bus", &"")
	if raw is StringName or raw is String:
		var b := StringName(raw)
		if BUSES.has(b):
			return b
	var fallback_v: Variant = info.get("bus", &"Sfx")
	var fallback := StringName(fallback_v) if (fallback_v is StringName or fallback_v is String) else &"Sfx"
	return fallback if BUSES.has(fallback) else &"Sfx"


## AudioStreamPlayer3D culls past max_distance; 0 means never. A margin past where the gain reaches
## silence, so a listener that moves after the gain was set does not cut a tail.
static func _cull_distance(d: Dictionary) -> float:
	var m := _num(d.get("max_distance_m", 0.0), 0.0)
	return m * 1.25 if m > 0.0 else 0.0


# Loops ----------------------------------------------------------------------------------------------

func _set_loops(list: Array) -> void:
	var seen: Dictionary[int, bool] = {}
	for item: Variant in list:
		if not item is Dictionary:
			continue
		var d: Dictionary = item
		var stem := _stem_of(d)
		if stem == &"":
			continue
		var id := int(_num(d.get("id", 0), 0.0))
		if id < 0 or seen.has(id):
			continue
		var lv: LoopVoice = _loops.get(id, null)
		if lv != null and lv.sound != stem:
			# The source now plays another file: the old one fades while the new one starts.
			_retire(id, lv)
			lv = null
		if lv == null:
			if _stream(stem) == null:
				# Still loading: look again next frame. A missing file stays silent.
				if not _missing.has(stem):
					_loops_rev = -1
				continue
			lv = _start_loop(stem, d)
			if lv == null:
				continue
			_loops[id] = lv
		seen[id] = true
		lv.leaving = false
		_aim_loop(lv, d)
	for id: int in _loops.keys():
		if not seen.has(id):
			_loops[id].leaving = true


func _retire(id: int, lv: LoopVoice) -> void:
	_loops.erase(id)
	lv.leaving = true
	_loops[_next_leaving_id] = lv
	_next_leaving_id = _next_leaving_id - 1 if _next_leaving_id > -1_000_000 else -1


func _start_loop(stem: StringName, d: Dictionary) -> LoopVoice:
	if _loops.size() >= MAX_LOOPS:
		return null
	var info: Dictionary = _info[stem]
	var bus := _bus_of(d, info)
	var lv := LoopVoice.new()
	lv.positional = _wants_position(d, info)
	lv.player = _loop_player(bus, lv.positional)
	lv.sound = stem
	lv.bus = bus
	lv.fade_in_s = MUSIC_FADE_IN_S if bus == &"Music" else LOOP_FADE_IN_S
	lv.fade_out_s = MUSIC_FADE_OUT_S if bus == &"Music" else LOOP_FADE_OUT_S
	_aim_loop(lv, d)
	lv.level_db = lv.target_db
	if lv.positional:
		(lv.player as AudioStreamPlayer3D).position = lv.target_pos
	lv.player.set(&"stream", _stream(stem))
	lv.player.set(&"pitch_scale", lv.target_pitch)
	lv.player.set(&"volume_db", -80.0)
	var from := 0.0
	# A one-shot pattern listed as a loop (alert_alarm) always starts at its head.
	if info.get("loop", false) == true and RANDOM_START_BUSES.has(bus):
		from = randf() * maxf(0.0, _num(info.get("duration_s", 0.0), 0.0) * 0.9)
	lv.player.call(&"play", from)
	if played.has_connections():
		played.emit(_loop_info(&"loop_start", lv))
	return lv


func _loop_info(kind: StringName, lv: LoopVoice) -> Dictionary:
	var pos: Vector3 = lv.target_pos
	return {
		"kind": String(kind), "sound": lv.sound, "bus": lv.bus, "volume_db": lv.target_db,
		"pitch": lv.target_pitch, "positional": lv.positional,
		"distance_m": _listener.global_position.distance_to(pos) if lv.positional else 0.0, "priority": 0,
	}


func _loop_player(bus: StringName, positional: bool) -> Node:
	for i: int in range(_spare_loop_players.size()):
		var p: Node = _spare_loop_players[i]
		if (p is AudioStreamPlayer3D) == positional:
			_spare_loop_players.remove_at(i)
			p.set(&"bus", bus)
			return p
	var player := _new_player(bus, positional)
	player.name = "Loop_%d" % _loop_player_count
	_loop_player_count += 1
	add_child(player)
	return player


func _aim_loop(lv: LoopVoice, d: Dictionary) -> void:
	lv.target_db = clampf(_num(d.get("volume_db", 0.0), SILENT_DB), -80.0, MAX_DB)
	lv.target_pitch = clampf(_num(d.get("pitch", 1.0), 1.0), MIN_PITCH, MAX_PITCH)
	if lv.positional:
		var pos: Variant = d.get("pos", null)
		if pos is Vector3 and (pos as Vector3).is_finite():
			lv.target_pos = pos
		(lv.player as AudioStreamPlayer3D).max_distance = _cull_distance(d)


func _run_loops(dt: float) -> void:
	if _loops.is_empty():
		return
	var glide := 1.0 - exp(-dt / LOOP_GLIDE_S) if dt > 0.0 else 0.0
	var gone: Array[int] = []
	for id: int in _loops:
		var lv: LoopVoice = _loops[id]
		if lv.leaving:
			lv.env -= dt / lv.fade_out_s
			if lv.env <= 0.0:
				gone.append(id)
				continue
		elif lv.env < 1.0:
			lv.env = minf(1.0, lv.env + dt / lv.fade_in_s)
		lv.level_db = lerpf(lv.level_db, lv.target_db, glide)
		var pitch := lerpf(float(lv.player.get(&"pitch_scale")), lv.target_pitch, glide)
		lv.player.set(&"pitch_scale", clampf(pitch, MIN_PITCH, MAX_PITCH))
		lv.player.set(&"volume_db", loop_volume_db(lv.level_db, lv.env))
		if lv.positional:
			var p3 := lv.player as AudioStreamPlayer3D
			p3.position = p3.position.lerp(lv.target_pos, glide)
		if lv.player.get(&"playing") != true and not lv.leaving:
			# A one-shot pattern (alert_alarm) restarts while it is listed; a loop stopped by an
			# audio device reset starts again.
			lv.player.call(&"play")
	for id: int in gone:
		var lv: LoopVoice = _loops[id]
		if played.has_connections():
			played.emit(_loop_info(&"loop_stop", lv))
		_loops.erase(id)
		lv.player.call(&"stop")
		lv.player.set(&"stream", null)
		_spare_loop_players.append(lv.player)


## Player volume of a loop at level `level_db` under a fade envelope `env` (linear 0..1).
static func loop_volume_db(level_db: float, env: float) -> float:
	if env <= 0.001:
		return -80.0
	return clampf(level_db + linear_to_db(minf(env, 1.0)), -80.0, MAX_DB)


# Interface feedback ----------------------------------------------------------------------------------

func _hook_tree(node: Node) -> void:
	_hook_control(node)
	for child: Node in node.get_children():
		_hook_tree(child)


## Connect a control's input signals to its interface sound (once per control).
func _hook_control(node: Node) -> void:
	if not node is Control or node.has_meta(&"_ui_sound_hooked"):
		return
	node.set_meta(&"_ui_sound_hooked", true)
	if node is OptionButton:
		(node as OptionButton).item_selected.connect(_on_select.bind(node))
	elif node is BaseButton:
		var b := node as BaseButton
		if b.toggle_mode:
			b.toggled.connect(_on_select.bind(node))
		else:
			b.pressed.connect(_on_click.bind(node))
	elif node is TabBar:
		(node as TabBar).tab_changed.connect(_on_select.bind(node))
	elif node is TabContainer:
		(node as TabContainer).tab_changed.connect(_on_select.bind(node))
	elif node is Slider:
		(node as Slider).value_changed.connect(_on_slider.bind(node))


func _input(event: InputEvent) -> void:
	# Presses and releases both count: a button fires on release, after a hold of any length.
	if event is InputEventKey or event is InputEventMouseButton or event is InputEventJoypadButton or event is InputEventScreenTouch:
		_last_press_ms = Time.get_ticks_msec()


func _silent(node: Node) -> bool:
	return _feed == null or node.get_meta(&"ui_sound", true) == false or Time.get_ticks_msec() - _last_press_ms > INPUT_RECENT_MS


func _on_click(node: Node) -> void:
	if not _silent(node):
		_feed.call(&"ui_sound", Bp.UI_SOUND_CLICK)


func _on_select(_value: Variant, node: Node) -> void:
	if not _silent(node):
		_feed.call(&"ui_sound", Bp.UI_SOUND_SELECT)


func _on_slider(_value: float, node: Node) -> void:
	var now := Time.get_ticks_msec()
	if _feed == null or node.get_meta(&"ui_sound", true) == false or now - _last_slider_tick_ms < SLIDER_TICK_MS:
		return
	# Only a slider the player moves ticks: a drag holds the mouse button down; arrow keys pass the
	# recent-press test.
	var c := node as Control
	if not (c.is_visible_in_tree() and (Input.is_mouse_button_pressed(MOUSE_BUTTON_LEFT) or Time.get_ticks_msec() - _last_press_ms <= INPUT_RECENT_MS)):
		return
	_last_slider_tick_ms = now
	_feed.call(&"ui_sound", Bp.UI_SOUND_SELECT)


# Listener -------------------------------------------------------------------------------------------

func _place_listener() -> void:
	var lv: Variant = _feed.call(&"listener")
	if not lv is Dictionary:
		return
	var l: Dictionary = lv
	var pos: Variant = l.get("pos", Vector3.ZERO)
	var fwd: Variant = l.get("forward", Vector3.FORWARD)
	var up: Variant = l.get("up", Vector3.UP)
	if not (pos is Vector3 and fwd is Vector3 and up is Vector3):
		return
	var t := listener_transform(pos, fwd, up)
	if t.origin.is_finite():
		_listener.global_transform = t


## The listener's transform at `pos`, facing `forward` with `up` (bad input gives a usable basis).
static func listener_transform(pos: Vector3, forward: Vector3, up: Vector3) -> Transform3D:
	var p := pos if pos.is_finite() else Vector3.ZERO
	var f := forward if forward.is_finite() and forward.length_squared() > 1e-8 else Vector3.FORWARD
	f = f.normalized()
	var u := up if up.is_finite() and up.length_squared() > 1e-8 else Vector3.UP
	u = u.normalized()
	if absf(f.dot(u)) > 0.999:
		u = Vector3.UP if absf(f.dot(Vector3.UP)) < 0.999 else Vector3.BACK
	return Transform3D(Basis.looking_at(f, u), p)


# Volume settings ------------------------------------------------------------------------------------

func _apply_all_volumes() -> void:
	for bus: StringName in BUS_SETTINGS:
		_apply_volume(bus)


func _on_setting_changed(key: StringName) -> void:
	for bus: StringName in BUS_SETTINGS:
		if BUS_SETTINGS[bus] == key:
			_apply_volume(bus)


## Mute the world buses while a sortie runs that the player is not playing (boot's graphics check under
## the loading cover), so a first launch does not open on alarms and gunfire; `false` releases them once
## that sortie's loops have faded.
func hold_world(on: bool) -> void:
	_hold_serial += 1
	if on:
		_world_held = true
		_apply_all_volumes()
		return
	var serial := _hold_serial
	get_tree().create_timer(HOLD_RELEASE_S, true).timeout.connect(func() -> void:
		if serial == _hold_serial:
			_world_held = false
			_apply_all_volumes())


func _apply_volume(bus: StringName) -> void:
	var index := AudioServer.get_bus_index(bus)
	if index < 0:
		return
	var value := 1.0
	if _settings != null:
		value = _num(_settings.call(&"get_value", BUS_SETTINGS[bus]), 1.0)
	value = clampf(value, 0.0, 1.0)
	AudioServer.set_bus_mute(index, value < MUTE_BELOW or (_world_held and WORLD_BUSES.has(bus)))
	AudioServer.set_bus_volume_db(index, slider_db(value))


## Bus gain of a slider position 0..1: amplitude is the square of the position.
static func slider_db(value: float) -> float:
	var v := clampf(value, 0.0, 1.0) if is_finite(value) else 1.0
	if v < MUTE_BELOW:
		return -80.0
	return maxf(linear_to_db(v * v), -80.0)


# Session --------------------------------------------------------------------------------------------

func _on_pacing_changed() -> void:
	if _session == null:
		return
	var p: Variant = _session.call(&"pacing")
	if not p is Dictionary:
		return
	_sim_rate = sim_rate(p)


## Sortie seconds per wall second of a PacingDict: 0 while paused.
static func sim_rate(pacing: Dictionary) -> float:
	if pacing.get("paused", false) == true:
		return 0.0
	var rate := clampf(_num(pacing.get("rate", 1), 1.0), 0.0, 16.0)
	var scale := clampf(_num(pacing.get("time_scale", 1.0), 1.0), 0.0, 4.0)
	return rate * scale


func _on_session_state(state: int) -> void:
	if not SORTIE_STATES.has(state):
		_pending.clear()
		_on_pacing_changed()


## A finite float from a Variant, else `fallback`.
static func _num(v: Variant, fallback: float) -> float:
	if v is float or v is int:
		var f := float(v)
		return f if is_finite(f) else fallback
	return fallback


## Test and diagnostics view: one-shot voices in use per bus (and how many of them positional), loops
## alive by stem with their playback position, waiting one-shots, and the catalogue's load state.
func debug_state() -> Dictionary:
	var busy: Dictionary = {}
	var busy_3d: Dictionary = {}
	for bus: StringName in BUSES:
		var n := 0
		var n3 := 0
		for v: Voice in _flat.get(bus, []):
			n += 1 if v.busy() else 0
		for v: Voice in _positional.get(bus, []):
			n3 += 1 if v.busy() else 0
		busy[bus] = n + n3
		busy_3d[bus] = n3
	var loops: Dictionary = {}
	for id: int in _loops:
		var lv: LoopVoice = _loops[id]
		loops[lv.sound] = {
			"leaving": lv.leaving, "env": lv.env, "bus": lv.bus, "playing": lv.player.get(&"playing") == true,
			"volume_db": lv.target_db, "pitch": lv.target_pitch,
			"position_s": float(lv.player.call(&"get_playback_position")),
		}
	return {
		"busy": busy,
		"busy_3d": busy_3d,
		"loops": loops,
		"pending": _pending.size(),
		"loaded": _streams.size(),
		"loading": _loading.size(),
		"missing": _missing.size(),
		"known": _info.size(),
	}
