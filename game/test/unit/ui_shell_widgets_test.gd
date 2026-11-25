extends SceneTree
## Headless checks for the Ui and Screens autoloads, the shared widgets and the boot helpers.
## Units: ui-shell, ui-widgets, ui-boot. Run from game/:
##   godot --headless --path . -s res://test/unit/ui_shell_widgets_test.gd
## Exits 1 on any failure. Calls no Rust facade that is still a stub, so it runs on any tree.

# Scripts load at run time, once the autoloads they name exist (a -s script compiles first).
var Gauge: GDScript
var AlertBadge: GDScript
var DamageHatch: GDScript
var StanceSymbol: GDScript
var UiText: GDScript
var BootScript: GDScript

var _failures := 0
var _checks := 0
var _frame := 0
var _step := 0
var _dialog_choice: StringName = &"?"


func _process(_delta: float) -> bool:
	_frame += 1
	if _frame == 2:
		Gauge = load("res://ui/widgets/gauge.gd")
		AlertBadge = load("res://ui/widgets/alert_badge.gd")
		DamageHatch = load("res://ui/widgets/damage_hatch.gd")
		StanceSymbol = load("res://ui/widgets/stance_symbol.gd")
		UiText = load("res://ui/widgets/ui_text.gd")
		BootScript = load("res://boot/boot.gd")
		_pure_checks()
		_palette_checks()
		_screens_start()
	elif _frame == 5:
		_screens_focus_checks()
	elif _frame == 8:
		_screens_pop_checks()
	elif _frame == 11:
		_dialog_checks()
	elif _frame == 14:
		_dialog_after_checks()
		_widget_scene_checks()
	elif _frame == 16:
		_ui().blink_off = false
		_ui().changed.emit(&"blink")
	elif _frame == 17:
		print("ui_shell_widgets_test: %d checks, %d failures" % [_checks, _failures])
		quit(1 if _failures > 0 else 0)
		return true
	return false


func _expect(ok: bool, what: String) -> void:
	_checks += 1
	if not ok:
		_failures += 1
		printerr("ui_shell_widgets_test: FAIL ", what)


func _ui() -> Node:
	return root.get_node("Ui")


func _screens() -> Node:
	return root.get_node("Screens")


# --- pure logic ------------------------------------------------------------------------------------

func _pure_checks() -> void:
	# Gauge state against band and limits (HUD rule 12; limits turn alert colour only when passed).
	_expect(Gauge.classify(NAN, 0, 10, NAN, 12, NAN, 14) == Gauge.State.NO_READING, "gauge no reading")
	_expect(Gauge.classify(5, 0, 10, NAN, 12, NAN, 14) == Gauge.State.NORMAL, "gauge in band")
	_expect(Gauge.classify(11, 0, 10, NAN, 12, NAN, 14) == Gauge.State.OUTSIDE_BAND, "gauge above band")
	_expect(Gauge.classify(12, 0, 10, NAN, 12, NAN, 14) == Gauge.State.CAUTION, "gauge at caution limit")
	_expect(Gauge.classify(15, 0, 10, NAN, 12, NAN, 14) == Gauge.State.WARNING, "gauge past warning")
	_expect(Gauge.classify(-1, 0, 10, 0, NAN, -0.5, NAN) == Gauge.State.WARNING, "gauge low warning")
	_expect(Gauge.classify(5, NAN, NAN, NAN, NAN, NAN, NAN) == Gauge.State.NORMAL, "gauge with no band")
	_expect(is_equal_approx(Gauge.scale_fraction(5, 0, 10), 0.5), "gauge fraction mid")
	_expect(is_equal_approx(Gauge.scale_fraction(50, 0, 10), 1.0), "gauge fraction clamps")
	_expect(is_equal_approx(Gauge.scale_fraction(3, 2, 2), 0.5), "gauge degenerate scale")
	# Alert blink rules (L1676-1687).
	_expect(AlertBadge.blink_rate(AlertBadge.PRIORITY_EMERGENCY, AlertBadge.ALERT_UNACKNOWLEDGED, true) == 2, "newest top emergency blinks fast")
	_expect(AlertBadge.blink_rate(AlertBadge.PRIORITY_ALARM, AlertBadge.ALERT_UNACKNOWLEDGED, false) == 1, "older alarm blinks slow")
	_expect(AlertBadge.blink_rate(AlertBadge.PRIORITY_WARNING, AlertBadge.ALERT_UNACKNOWLEDGED, true) == 1, "warning blinks at 1 Hz")
	_expect(AlertBadge.blink_rate(AlertBadge.PRIORITY_CAUTION, AlertBadge.ALERT_UNACKNOWLEDGED, true) == 0, "caution steady")
	_expect(AlertBadge.blink_rate(AlertBadge.PRIORITY_ALARM, AlertBadge.ALERT_ACKNOWLEDGED, true) == 0, "acknowledged steady")
	_expect(AlertBadge.blink_rate(AlertBadge.PRIORITY_ALARM, AlertBadge.ALERT_CLEARED, true) == 0, "cleared steady")
	# Damage bands (L1651).
	_expect(DamageHatch.level_for_health(0.9) == DamageHatch.DAMAGE_INTACT, "health 90% intact")
	_expect(DamageHatch.level_for_health(0.75) == DamageHatch.DAMAGE_DAMAGED, "health 75% damaged")
	_expect(DamageHatch.level_for_health(0.40) == DamageHatch.DAMAGE_DAMAGED, "health 40% damaged")
	_expect(DamageHatch.level_for_health(0.39) == DamageHatch.DAMAGE_HEAVY, "health 39% heavy")
	_expect(DamageHatch.level_for_health(0.01) == DamageHatch.DAMAGE_HEAVY, "health 1% heavy")
	_expect(DamageHatch.level_for_health(0.0) == DamageHatch.DAMAGE_DESTROYED, "health 0 destroyed")
	# Hatch geometry: horizontal lines every 4 px over 24 px rows give 6 lines inside (0 is the edge).
	var flood: PackedVector2Array = DamageHatch.hatch_lines(Rect2(0, 0, 24, 24), 0.0, 4.0)
	_expect(flood.size() / 2 >= 6 and flood.size() / 2 <= 7, "flooding hatch line count %d" % (flood.size() / 2))
	var all_inside := true
	for p: Vector2 in DamageHatch.hatch_lines(Rect2(2, 3, 30, 17), 45.0, 3.0):
		if p.x < 2.0 - 0.01 or p.x > 32.01 or p.y < 2.99 or p.y > 20.01:
			all_inside = false
	_expect(all_inside, "45 degree hatch stays inside its rect")
	var diag: PackedVector2Array = DamageHatch.hatch_lines(Rect2(0, 0, 24, 24), 45.0, 6.0)
	var rising: bool = diag.size() >= 2 and (diag[1] - diag[0]).x * (diag[1] - diag[0]).y < 0.0
	_expect(rising, "45 degree hatch rises to the right")
	_expect(DamageHatch.hatch_lines(Rect2(0, 0, 0, 10), 45.0, 3.0).is_empty(), "empty rect, no lines")
	# Stance frames are closed outlines on the icon grid.
	for s in 4:
		var pts: PackedVector2Array = StanceSymbol.frame_points(s)
		_expect(pts.size() >= 4 and pts[0].distance_to(pts[-1]) < 0.01, "stance %d frame closed" % s)
		var inside := true
		for p: Vector2 in pts:
			if p.x < 3.9 or p.x > 20.1 or p.y < 5.9 or p.y > 18.1:
				inside = false
		_expect(inside, "stance %d frame inside the icon box" % s)
	# Boot helpers.
	_expect(is_equal_approx(BootScript.percentile(PackedFloat32Array([5, 1, 4, 2, 3]), 0.95), 5.0), "p95 of five samples")
	_expect(is_equal_approx(BootScript.percentile(PackedFloat32Array([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]), 0.5), 5.0), "median by nearest rank")
	_expect(BootScript.percentile(PackedFloat32Array(), 0.95) < 0.0, "no samples")
	var report: Dictionary = BootScript.startup_report()
	for key: String in ["rendering_method", "driver", "adapter", "api_version", "fallbacks"]:
		_expect(report.has(key), "startup report has " + key)
	var m: Dictionary = BootScript.machine()
	for key: String in ["adapter", "vendor", "driver_version", "api_version", "processor", "model", "memory_gb", "rendering_method", "window_px"]:
		_expect(m.has(key), "machine has " + key)
	# Text resolution never calls Rust for empty text.
	_expect(UiText.resolve({}) == "", "empty MsgDict")
	_expect(UiText.resolve({"key": &""}) == "", "MsgDict with empty key")
	_expect(UiText.resolve(&"") == "", "empty key")
	_expect(UiText.resolve("Hood") == "Hood", "player text passes through")
	_expect(UiText.resolve(&"widgets.alert.cleared") == "Cleared", "key translates")
	# Ui tokens.
	var ui := _ui()
	_expect(ui.spacing(2) == 12 and ui.spacing(99) == 48, "spacing steps")
	_expect(ui.font_size(&"Body") == 22, "body size")
	_expect(ui.clamp_scale(5.0) == 2.0 and ui.clamp_scale(0.1) == 0.75 and ui.clamp_scale(NAN) == 1.0, "UI scale clamp")
	_expect(is_equal_approx(ui.motion_s(&"panel"), 0.15) and is_equal_approx(ui.motion_s(&"nonsense"), 0.0), "motion durations")
	ui.reduce_motion = true
	_expect(ui.motion_s(&"panel") == 0.0 and is_equal_approx(ui.fade_s(&"feedback"), 0.15), "reduce motion: instant moves, 150 ms fades")
	ui.reduce_motion = false
	_expect(ui.icon(&"close") != null, "icon loads")
	var t: Tween = ui.tween_for(root, &"Test.signal")
	_expect(t.get_meta(&"sim_signal", &"") == &"Test.signal", "tween names its sim signal")
	t.kill()


func _palette_checks() -> void:
	var ui := _ui()
	var layer := CanvasLayer.new()
	root.add_child(layer)
	var probe := Control.new()
	layer.add_child(probe)
	var dusk_white: Color = ui.colour(&"text.primary")
	_expect(probe.get_theme_color(&"text_primary", &"Gauge").is_equal_approx(dusk_white), "dusk theme under a CanvasLayer")
	ui.call(&"_apply_palette", &"high_contrast")
	var hc: Color = ui.colour(&"text.primary")
	_expect(not hc.is_equal_approx(dusk_white), "high-contrast tokens differ")
	_expect(probe.get_theme_color(&"text_primary", &"Gauge").is_equal_approx(hc), "palette switch reaches a Control under a CanvasLayer")
	ui.call(&"_apply_palette", &"dusk")
	_expect(probe.get_theme_color(&"text_primary", &"Gauge").is_equal_approx(dusk_white), "palette switch back to dusk")
	_expect(ui.palette == &"dusk", "palette name back to dusk")
	layer.queue_free()


# --- screens -------------------------------------------------------------------------------------

func _screens_start() -> void:
	var screens := _screens()
	_expect(screens.depth() == 0, "empty stack at start")
	var consent: Node = screens.change(&"consent")
	_expect(consent != null and screens.base_name() == &"consent", "change opens a root by name")
	_expect(screens.change(&"consent") == consent, "change to the same base is a no-op")


func _screens_focus_checks() -> void:
	var screens := _screens()
	var focus := root.get_viewport().gui_get_focus_owner()
	_expect(focus == null, "consent opens with nothing focused, nothing preselected (got %s)" % [focus])
	# The player then moves to a button; a pushed screen must hand that focus back when it closes.
	(screens.top().get_node("Scroll/Center/Margin/Column/Buttons/Send") as Control).grab_focus()
	var pushed: Node = screens.push(&"res://boot/consent.tscn")
	_expect(pushed != null and screens.depth() == 2, "push adds a screen")
	_expect(screens.top() == pushed, "pushed screen is the top")
	_expect(screens.get_node("Overlay/InputShield").visible, "shield up while a screen is pushed")
	# The first direction pressed picks the button on that side: Right is Don't send.
	var right := InputEventAction.new()
	right.action = &"ui_right"
	right.pressed = true
	pushed.call(&"_unhandled_input", right)


func _screens_pop_checks() -> void:
	var screens := _screens()
	var focus := root.get_viewport().gui_get_focus_owner()
	_expect(focus != null and screens.top().is_ancestor_of(focus), "pushed screen took focus")
	_expect(focus != null and focus.name == &"DontSend", "Right with nothing focused picks Don't send (got %s)" % [focus])
	screens.pop()
	_expect(screens.depth() == 1, "pop returns to the base")
	_expect(not screens.get_node("Overlay/InputShield").visible, "shield down at the base")
	_expect(not screens.back(), "back at the base does nothing")


func _dialog_checks() -> void:
	var screens := _screens()
	var focus := root.get_viewport().gui_get_focus_owner()
	_expect(focus != null and screens.top().is_ancestor_of(focus), "focus restored to the base after pop")
	var dialog: Node = screens.open_dialog(&"shell.session.failed_title", &"shell.session.failed_body",
		[{"id": &"keep", "label": &"shell.dialog.close"}, {"id": &"leave", "label": &"shell.dialog.close"}], &"keep", &"keep")
	_expect(dialog != null and screens.modal_count() == 1, "dialog opens")
	dialog.connect(&"chosen", func(id: StringName) -> void: _dialog_choice = id)
	_expect(dialog.get_node("Center/Panel/Margin/Column/Title").text == "Session ended", "dialog title translated")
	var esc := InputEventAction.new()
	esc.action = &"ui_cancel"
	esc.pressed = true
	Input.parse_input_event(esc)


func _dialog_after_checks() -> void:
	var screens := _screens()
	_expect(_dialog_choice == &"keep", "Esc picks the cancel choice (got %s)" % _dialog_choice)
	_expect(screens.modal_count() == 0, "dialog closed")
	screens.toast(&"widgets.alert.cleared")
	_expect(screens.get_node("Toasts/Toast/Panel").visible, "toast shows")
	screens.show_loading(&"boot.warmup.status", 0.5)
	_expect(screens.is_loading(), "cover shows")
	screens.hide_loading()
	_expect(not screens.is_loading(), "cover lifts")
	screens.clear()
	_expect(screens.depth() == 0, "clear empties the stack")


func _widget_scene_checks() -> void:
	var gauge: Control = (load("res://ui/widgets/gauge.tscn") as PackedScene).instantiate()
	root.add_child(gauge)
	gauge.configure({"label": &"widgets.alert.caution", "min": 0.0, "max": 40.0, "band_low": 5.0, "band_high": 30.0, "caution_high": 34.0, "warning_high": 38.0})
	_expect(gauge.state() == Gauge.State.NO_READING, "gauge starts without a reading")
	gauge.queue_free()
	var badge: Control = (load("res://ui/widgets/alert_badge.tscn") as PackedScene).instantiate()
	root.add_child(badge)
	badge.priority = AlertBadge.PRIORITY_ALARM
	_expect(badge.get_combined_minimum_size().x >= 24.0, "badge has a size")
	badge.queue_free()
	# Draw paths: every badge priority and state, with and without blink-off; a gauge with a
	# needle, trend and a passed limit; a selected symbol with a trend. They draw over the next
	# frames and must raise no script error.
	var holder := VBoxContainer.new()
	holder.name = &"DrawHolder"
	root.add_child(holder)
	for p in 4:
		for st in [AlertBadge.ALERT_UNACKNOWLEDGED, AlertBadge.ALERT_ACKNOWLEDGED, AlertBadge.ALERT_CLEARED]:
			var b: Control = AlertBadge.new()
			holder.add_child(b)
			b.priority = p
			b.state = st
			b.blink_fast = p == AlertBadge.PRIORITY_EMERGENCY
	var g: Control = Gauge.new()
	holder.add_child(g)
	g.custom_minimum_size = Vector2(240, 0)
	g.min_value = 0.0
	g.max_value = 40.0
	g.band_low = 5.0
	g.band_high = 30.0
	g.caution_high = 34.0
	g.warning_high = 38.0
	g.value = 36.0
	g.rate = 0.5
	g.call(&"_changed")
	_expect(g.state() == Gauge.State.CAUTION, "gauge past caution after a reading")
	var sym: Control = StanceSymbol.new()
	holder.add_child(sym)
	sym.selected = true
	sym.symbol_px = 48.0
	_ui().blink_off = true
	_ui().changed.emit(&"blink")
