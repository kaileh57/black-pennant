extends RefCounted
## Expression evaluation for the play driver: the autoloads are in scope by name. Test only.

const NAMES: PackedStringArray = ["App", "Settings", "Quality", "Tx", "Content", "Session", "Orders",
	"Hud", "Editor", "Campaign", "Lobby", "Tutorial", "AudioFeed", "Bench", "Ui", "Screens"]


static func run(source: String, root: Window) -> String:
	var expr := Expression.new()
	var names := NAMES.duplicate()
	names.append("root")
	var err := expr.parse(source, names)
	if err != OK:
		return "parse error: " + expr.get_error_text()
	var values: Array = []
	for n: String in NAMES:
		values.append(root.get_node_or_null(NodePath(n)))
	values.append(root)
	var result: Variant = expr.execute(values, null, true)
	if expr.has_execute_failed():
		return "execute error: " + expr.get_error_text()
	return JSON.stringify(result, " ", false) if (result is Dictionary or result is Array) else str(result)
