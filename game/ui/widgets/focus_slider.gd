extends HSlider
## HSlider that shows the focus ring. DESIGN.md L1716 (focus 2 px, 3:1), L1738 (themed focus).
## Unit: ui-widgets.
##
## Godot's Slider has no `focus` style box: a focused slider only swaps its grabber area for the
## highlight, which is hard to see, so keyboard and gamepad players lose track of focus on a page of
## sliders. This draws the theme's Button focus box (2 px ui.focus outside the control) while the
## slider has visible focus. Focus grabbed for the pointer (hidden focus) draws nothing.
## Load by path: `const FocusSlider := preload("res://ui/widgets/focus_slider.gd")`, then
## `FocusSlider.new()` where `HSlider.new()` was.


func _init() -> void:
	focus_entered.connect(queue_redraw)
	focus_exited.connect(queue_redraw)


func _draw() -> void:
	if not has_focus(true):
		return
	var box := get_theme_stylebox(&"focus", &"Button")
	if box != null:
		draw_style_box(box, Rect2(Vector2.ZERO, size))
