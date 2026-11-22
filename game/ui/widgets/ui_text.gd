class_name UiText
extends RefCounted
## Turns the three kinds of text the UI is handed into display strings. Unit: ui-widgets.
## - `StringName`: a text key, translated with tr() (L1707). `&""` gives "".
## - `Dictionary`: a `MsgDict`, formatted by `Tx.msg` (numbers, units, nested keys, track numbers).
##   An empty one, or one whose key is `&""`, gives "" without calling Rust.
## - `String`: shown as it is. Only player-typed text (design and player names) arrives this way.
## Anything else gives "".


static func resolve(value: Variant) -> String:
	if value is StringName:
		return "" if value == &"" else String(TranslationServer.translate(value))
	if value is Dictionary:
		var d := value as Dictionary
		if d.is_empty() or StringName(d.get("key", &"")) == &"":
			return ""
		var tx := _autoload(&"Tx")
		if tx == null:
			return String(TranslationServer.translate(StringName(d.get("key", &""))))
		return String(tx.call(&"msg", d))
	if value is String:
		return value
	return ""


## True when `value` would resolve to no text.
static func is_empty(value: Variant) -> bool:
	if value is StringName:
		return value == &""
	if value is Dictionary:
		return (value as Dictionary).is_empty() or StringName((value as Dictionary).get("key", &"")) == &""
	if value is String:
		return (value as String).is_empty()
	return true


static func _autoload(autoload_name: StringName) -> Node:
	var tree := Engine.get_main_loop() as SceneTree
	if tree == null or tree.root == null:
		return null
	return tree.root.get_node_or_null(NodePath(String(autoload_name)))
