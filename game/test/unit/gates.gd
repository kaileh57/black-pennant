extends RefCounted
## Shared judges for the UI gates of DESIGN.md L1616 (toast check), L1749-1755 (capture harness,
## colour-blind and greyscale captures, the 30 s calm capture) and L1737 (motion only reports
## state). Unit: ui-tests. Pure functions on plain data and images, so the headless tests and the
## windowed captures judge the same way and a negative control can feed them made-up input.
## Preload it: `const Gates := preload("res://test/unit/gates.gd")`. Not a test itself.

## Toasts a battle may show (L1616): saves, joins and leaves, connection loss. Matched on the text
## key of the message; a message with no key (plain text) is never allowed.
const TOAST_ALLOWED_KEYS: Array[String] = [
	"editor_shell.saved", "editor_shell.capture.done", "campaign.event.saved", "benchmark.saved",
	"log.network.joined", "log.network.left", "session.failed.host_lost", "net.end.host_lost",
]
const TOAST_ALLOWED_PREFIXES: Array[String] = ["net.drop.", "net.end."]
## L1616: over 1 per 60 s or 3 per battle fails.
const TOAST_WINDOW_S := 60.0
const TOAST_PER_WINDOW := 1
const TOAST_PER_BATTLE := 3

## Alert hues (L1648: hue appears only for coding; amber is caution, red is warning and above).
const ALERT_TOKENS: Array[StringName] = [&"alert.caution", &"alert.warning"]
## A pixel within this RGB distance (0 to 1 per channel) of an alert token counts as an alert pixel.
## Wide enough to catch anti-aliased glyph cores, narrow enough to miss the warm greys.
const ALERT_RGB_DISTANCE := 0.10
## Changed pixels are gathered in square blocks of this many pixels.
const DIFF_BLOCK := 16

## Machado, Oliveira and Fernandes 2009, severity 1.0, applied in linear RGB.
const CVD: Dictionary[String, Array] = {
	"protan": [0.152286, 1.052583, -0.204868, 0.114503, 0.786281, 0.099216, -0.003882, -0.048116, 1.051998],
	"deutan": [0.367322, 0.860646, -0.227968, 0.280085, 0.672501, 0.047413, -0.011820, 0.042940, 0.968881],
	"tritan": [1.255528, -0.076749, -0.178779, -0.078411, 0.930809, 0.147602, 0.004733, 0.691367, 0.303900],
}


# --- toasts ---------------------------------------------------------------------------------------

## The text key of a toast message: a StringName or String key, or the `key` of a MsgDict.
static func toast_key(message: Variant) -> String:
	if message is Dictionary:
		return String((message as Dictionary).get("key", ""))
	if message is StringName or message is String:
		return String(message)
	return ""


static func toast_allowed(key: String) -> bool:
	if key.is_empty():
		return false
	if key in TOAST_ALLOWED_KEYS:
		return true
	for p: String in TOAST_ALLOWED_PREFIXES:
		if key.begins_with(p):
			return true
	return false


## Problems with the toasts of one battle, empty when it passes. Each record is
## `{ "t": seconds of battle time, "key": String }`, in the order they were shown.
static func judge_toasts(records: Array, battle_s: float) -> PackedStringArray:
	var out := PackedStringArray()
	for r: Dictionary in records:
		if not toast_allowed(String(r.get("key", ""))):
			out.append("sim-event toast '%s' at %.1f s" % [r.get("key", ""), float(r.get("t", 0.0))])
	if records.size() > TOAST_PER_BATTLE:
		out.append("%d toasts in a %.0f s battle (limit %d)" % [records.size(), battle_s, TOAST_PER_BATTLE])
	var times: Array[float] = []
	for r: Dictionary in records:
		times.append(float(r.get("t", 0.0)))
	times.sort()
	for i: int in range(times.size()):
		var n := 0
		for j: int in range(i, times.size()):
			if times[j] - times[i] < TOAST_WINDOW_S:
				n += 1
		if n > TOAST_PER_WINDOW:
			out.append("%d toasts inside 60 s from %.1f s" % [n, times[i]])
			break
	return out


# --- HUD state ------------------------------------------------------------------------------------

## What every visible Control under `root` shows: path -> signature (text, value, visibility,
## tint and rect). Two of these from different frames say which controls changed.
static func control_state(root: Node) -> Dictionary:
	var out := {}
	if root == null or not is_instance_valid(root):
		return out
	var nodes: Array[Node] = [root]
	nodes.append_array(root.find_children("*", "Control", true, false))
	for n: Node in nodes:
		var c := n as Control
		if c == null or not c.is_visible_in_tree():
			continue
		var sig := "%s|%s|%s|%s" % [c.get_global_rect(), c.modulate, c.self_modulate, _content(c)]
		out[String(root.get_path_to(c))] = sig
	return out


static func _content(c: Control) -> String:
	if c is Label:
		return (c as Label).text
	if c is Button:
		return "%s|%s|%s" % [(c as Button).text, (c as Button).button_pressed, (c as Button).disabled]
	if c is RichTextLabel:
		return (c as RichTextLabel).get_parsed_text()
	if c is LineEdit:
		return (c as LineEdit).text
	if c is Range:
		return str((c as Range).value)
	if c is TextureRect:
		var t := (c as TextureRect).texture
		return "" if t == null else str(t.get_rid())
	return ""


## Paths whose signature differs between two `control_state` results, added and removed included.
static func changed_paths(before: Dictionary, after: Dictionary) -> PackedStringArray:
	var out := PackedStringArray()
	for k: Variant in after:
		if not before.has(k) or before[k] != after[k]:
			out.append(String(k))
	for k: Variant in before:
		if not after.has(k):
			out.append(String(k))
	out.sort()
	return out


## The rect of a Control in window pixels, as the root viewport's texture shows it.
static func window_rect(c: Control) -> Rect2:
	var xf := c.get_viewport().get_final_transform() * c.get_global_transform_with_canvas()
	return xf * Rect2(Vector2.ZERO, c.size)


# --- images ---------------------------------------------------------------------------------------

## Blocks (DIFF_BLOCK square, clipped to the image) where two same-sized images differ. Rows are
## compared as whole byte runs first, so an unchanged frame costs one native comparison per row.
static func diff_blocks(a: Image, b: Image) -> Array[Rect2i]:
	var out: Array[Rect2i] = []
	if a == null or b == null or a.get_size() != b.get_size():
		if b != null:
			out.append(Rect2i(Vector2i.ZERO, b.get_size()))
		return out
	var ia := _rgba8(a)
	var ib := _rgba8(b)
	var w := ia.get_width()
	var h := ia.get_height()
	var da := ia.get_data()
	var db := ib.get_data()
	var stride := w * 4
	var marked := {}
	for y: int in range(h):
		var o := y * stride
		if da.slice(o, o + stride) == db.slice(o, o + stride):
			continue
		var by := y / DIFF_BLOCK
		for bx: int in range((w + DIFF_BLOCK - 1) / DIFF_BLOCK):
			var key := Vector2i(bx, by)
			if marked.has(key):
				continue
			var s := o + bx * DIFF_BLOCK * 4
			var e := mini(s + DIFF_BLOCK * 4, o + stride)
			if da.slice(s, e) != db.slice(s, e):
				marked[key] = true
	for k: Vector2i in marked:
		var r := Rect2i(k * DIFF_BLOCK, Vector2i(DIFF_BLOCK, DIFF_BLOCK))
		out.append(r.intersection(Rect2i(0, 0, w, h)))
	return out


## Pixels close to any of `colours` (RGB distance under ALERT_RGB_DISTANCE) and their bounds:
## `{ "count": int, "bounds": Rect2i }`. `step` samples every n-th pixel in both axes.
static func colour_pixels(img: Image, colours: Array[Color], step: int = 1) -> Dictionary:
	var im := _rgba8(img)
	var w := im.get_width()
	var h := im.get_height()
	var data := im.get_data()
	var targets: Array[Vector3i] = []
	for c: Color in colours:
		targets.append(Vector3i(c.r8, c.g8, c.b8))
	var limit := int(ALERT_RGB_DISTANCE * 255.0)
	var limit2 := limit * limit
	var count := 0
	var lo := Vector2i(w, h)
	var hi := Vector2i(-1, -1)
	step = maxi(step, 1)
	for y: int in range(0, h, step):
		var row := y * w * 4
		for x: int in range(0, w, step):
			var i := row + x * 4
			var r := data[i]
			var g := data[i + 1]
			var bl := data[i + 2]
			for t: Vector3i in targets:
				var dr := r - t.x
				var dg := g - t.y
				var dbl := bl - t.z
				if dr * dr + dg * dg + dbl * dbl <= limit2:
					count += 1
					lo = Vector2i(mini(lo.x, x), mini(lo.y, y))
					hi = Vector2i(maxi(hi.x, x), maxi(hi.y, y))
					break
	var bounds := Rect2i() if count == 0 else Rect2i(lo, hi - lo + Vector2i.ONE)
	return {"count": count, "bounds": bounds}


## A greyscale copy (Rec. 709 luma of linear light, re-encoded as sRGB).
static func greyscale(img: Image) -> Image:
	return _remap(img, [0.2126, 0.7152, 0.0722, 0.2126, 0.7152, 0.0722, 0.2126, 0.7152, 0.0722])


## A copy as seen with protanopia, deuteranopia or tritanopia (`kind` a key of CVD).
static func colour_blind(img: Image, kind: String) -> Image:
	if not CVD.has(kind):
		return img.duplicate() as Image
	return _remap(img, CVD[kind])


static func _remap(img: Image, m: Array) -> Image:
	var im := _rgba8(img)
	var data := im.get_data()
	var to_lin := PackedFloat32Array()
	to_lin.resize(256)
	for i: int in range(256):
		var c := i / 255.0
		to_lin[i] = c / 12.92 if c <= 0.04045 else pow((c + 0.055) / 1.055, 2.4)
	const N := 4096
	var to_srgb := PackedByteArray()
	to_srgb.resize(N + 1)
	for i: int in range(N + 1):
		var l := float(i) / N
		var s := l * 12.92 if l <= 0.0031308 else 1.055 * pow(l, 1.0 / 2.4) - 0.055
		to_srgb[i] = clampi(roundi(s * 255.0), 0, 255)
	var m0: float = m[0]
	var m1: float = m[1]
	var m2: float = m[2]
	var m3: float = m[3]
	var m4: float = m[4]
	var m5: float = m[5]
	var m6: float = m[6]
	var m7: float = m[7]
	var m8: float = m[8]
	for i: int in range(0, data.size(), 4):
		var r := to_lin[data[i]]
		var g := to_lin[data[i + 1]]
		var b := to_lin[data[i + 2]]
		data[i] = to_srgb[clampi(int((m0 * r + m1 * g + m2 * b) * N), 0, N)]
		data[i + 1] = to_srgb[clampi(int((m3 * r + m4 * g + m5 * b) * N), 0, N)]
		data[i + 2] = to_srgb[clampi(int((m6 * r + m7 * g + m8 * b) * N), 0, N)]
	return Image.create_from_data(im.get_width(), im.get_height(), false, Image.FORMAT_RGBA8, data)


static func _rgba8(img: Image) -> Image:
	if img.get_format() == Image.FORMAT_RGBA8:
		return img
	var c := img.duplicate() as Image
	c.convert(Image.FORMAT_RGBA8)
	return c


# --- motion ---------------------------------------------------------------------------------------

## Running tweens that do not name the sim signal they report (L1737: `Ui.tween_for` sets
## `sim_signal` metadata). Returns one line per such tween.
static func untraced_tweens(tree: SceneTree) -> PackedStringArray:
	var out := PackedStringArray()
	for t: Tween in tree.get_processed_tweens():
		if t.is_valid() and t.is_running() and not t.has_meta(&"sim_signal"):
			out.append(str(t))
	return out
