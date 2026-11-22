//! Contract-owned (no work unit). The named Dictionary shapes of the GDScript-facing surface.
//! A shape that more than one method uses has a name and is specified here once; `#[func]` doc comments refer
//! to it by that name. A shape only one method uses is written out in full in that method's doc comment. UI
//! code is written against this file and the class files alone. On the Rust side each named shape is built by
//! exactly one function named `<shape>_dict`: in [`crate::convert`] when several classes share it, otherwise
//! in the file of the class that returns it.
//!
//! # Conventions
//! - Keys are `String` literals in snake_case. A key listed here is always present; "or null" marks the only
//!   keys that may hold `null`.
//! - `int` enums are `Bp.*` constants (`crate::enums`). Ids are `int`; `Bp.NONE` (-1) means none. Entity ids
//!   are opaque 64-bit `int`s. Track numbers are never reused within a sortie (L1661).
//! - Map positions are `Vector2(x, z)` in world metres (+x east, +z south: the simulation's world frame,
//!   `sim_core::math`). Bearings and headings are degrees true, 0..360, clockwise from north (-z). Speeds are knots, ranges metres, times seconds unless the key
//!   says otherwise (`_kn`, `_m`, `_km`, `_s`, `_deg`, `_kw`, `_l`, `_t`, `_h` for theatre hours).
//! - 3D positions are `Vector3` in render space: metres relative to the floating render origin (L1088).
//! - Text is never English: a text key is a `StringName` for `tr()`, and a sentence is a `MsgDict` for
//!   `Tx.msg()`. Player-typed names are plain `String`.
//! - Fractions are `float` 0.0..1.0 unless stated.
//! - Lists are `Array[Dictionary]` in the order stated by the getter (usually ascending id).
//! - Every getter documents its update rate and has a revision counter or change signal; UI redraws on the
//!   signal and never rebuilds per frame (2 ms adapter budget, L1449).
//!
//! # Text
//! `MsgDict` (one player-facing sentence, `protocol::text::Msg`):
//!   `key: StringName`, `args: Array` of: `int` | `String` (untranslated name) |
//!   `{ "num": float, "unit": StringName }` (unit one of `none m km kn deg s min h pct kw l t mm rounds salvage intel`) |
//!   `{ "key": StringName }` (nested text key) | `{ "track": int }` | `{ "part": int }` | `{ "compartment": int }` |
//!   `{ "stance": int }` | `{ "rung": int }` | `{ "grade": int }` | `{ "emcon": int }`.
//!   An empty sentence has `key == &""`.
//!
//! # Session and pacing
//! `PacingDict` (`protocol::Pacing`, L110-116): `paused: bool`, `rate: int` (effective 1, 2 or 4),
//!   `requested: Array[{ "player": int, "rate": int }]`, `blocked: int` (`Bp.BLOCK_*` or `Bp.NONE`),
//!   `blocked_text: MsgDict`, `time_scale: float` (1.0 down to 0.5), `rung: int` (0..4), `sub_step: int`,
//!   `host_tick_ms: float`, `can_pause: bool`, `can_compress: bool`.
//! `SessionDict`: `state: int` (`Bp.SESSION_*`), `kind: int` (`Bp.KIND_*`), `mode: int` (`Bp.MODE_*`),
//!   `net: int` (`Bp.NET_*`), `player: int` (own slot), `side: int`, `vessel: int` (entity or `Bp.NONE`),
//!   `tick: int`, `time_s: float` (sortie time), `seed: int`, `weather: WeatherDict`, `time_of_day: int`,
//!   `is_host: bool`, `loading: float` (0..1 while `state == Bp.SESSION_LOADING`), `error: MsgDict`.
//! `WeatherDict` (`content::weather::WeatherState`): `kind: int` (`Bp.WEATHER_*`), `sea_state: int` (WMO code 0..8),
//!   `wave_height_m: float`, `wind_from_deg: float` (the state stores where the wind blows toward; the converter
//!   turns it round), `wind_kn: float`, `cloud: float`, `visibility_m: float`,
//!   `rain: float`, `sun_elevation_deg: float`, `sun_azimuth_deg: float`.
//! `SummaryDict` (`protocol::SortieSummary`): `outcome: int` (`Bp.OUTCOME_*`), `winner_side: int`,
//!   `duration_s: float`, `objectives: Array[{ "id": int, "done": bool }]`, `stats: StatsDict`, `tip: MsgDict`.
//! `StatsDict` (`protocol::SortieStats`): `rounds_fired: int`, `rounds_hit: int`, `missiles_fired: int`,
//!   `missiles_hit: int`, `torpedoes_fired: int`, `torpedoes_hit: int`, `decoys_fired: int`,
//!   `missiles_seduced: int`, `fires: int`, `first_detection_s: float` (-1.0 = none), `radiating_s: float`,
//!   `enemy_tonnage_disabled_t: float`, `enemy_tonnage_total_t: float`, `own_cells_lost: float` (0 to 1
//!   share of own cells), `enemy_struck: int` (enemy hulls that struck their colours, captured prizes
//!   included), `alerts_raised: int`, `kills_by_method: Array[{ "method": int, "count": int }]`.
//! `ReportCardDict` (`editor::splash::ReportCard`, L216): `duration_s`, `rounds_fired: int`, `hit_rate: float`,
//!   `targets_destroyed: int`, `time_to_first_hit_s: float` (-1.0 = none), `damage_taken: float`,
//!   `peak_heel_deg: float`, `score: float`, `tip: MsgDict`.
//! `ConnectionDict` (`net::msg::PlayerLink`, L1033-1039): `player: int`, `name: String`, `state: int`
//!   (`Bp.CONN_*`: `CONN_WARNING` while `warning` is set, `CONN_DROPPED` while the slot is held), `ping_ms: int`,
//!   `loss: float`, `frames_behind: int`, `quality: int` (`Bp.NET_QUALITY_*`), `cap_kbps: int` (128 to 768),
//!   `is_host: bool`, `warning: int` (`Bp.DROP_WARNING_*` or `Bp.NONE`), `warning_text: MsgDict`,
//!   `held_s: int` (seconds left of the 5 minute rejoin hold, or `Bp.NONE`), `officers_in_command: bool`
//!   (true while held).
//!
//! # Own ship (all from `protocol::ShipStatus`)
//! `MotionDict` (30 Hz): `vessel: int`, `pos: Vector2`, `speed_kn`, `heading_deg`, `heel_deg`, `trim_deg`,
//!   `ordered_speed_kn`, `ordered_heading_deg`, `rudder: float` (-1..1), `power: float` (shaft power 0..1),
//!   `route: PackedVector2Array`, `designated: int` (track), `longest_envelope_m: float`, `cells_lost: float`,
//!   `status: int` (`Bp.VESSEL_*`).
//! `PowerDict` (2 Hz): `generation_kw`, `demand_kw`, `supplied_kw`, `fuel_l`, `fuel: float` (fraction),
//!   `fuel_flow_lps: float`, `shed_count: int`.
//! `PartStaticDict` (once per sortie, from the blueprint): `slot: int`, `part: StringName` (part id),
//!   `name: MsgDict` (label or part name), `category: int` (`Bp.CATEGORY_*`), `compartment: int`,
//!   `cell: Vector3i`, `size: Vector3i`, `kit: int` (kit slot or `Bp.NONE`), `is_mount: bool`,
//!   `is_emitter: bool`, `weapon_group: int` (0-based or `Bp.NONE`), `icon: StringName`.
//! `PartDict` (2 Hz): `slot: int`, `level: int` (`Bp.DAMAGE_*`), `health: float`, `power: int`, `fuel: int`,
//!   `data: int` (each `Bp.LINK_*`), `shed: bool`, `priority: int` (1..5),
//!   `readout: { "kind": int (Bp.READOUT_*), "a": float, "b": float }` (Rounds: ready, magazine; Cells: ready,
//!   reload seconds; Output: kW; Fuel: litres; Radiating, Running, Closed: 0 or 1; Aboard: count).
//! `MountDict` (10 Hz): `slot: int`, `name: MsgDict`, `group: int`, `status: int` (`Bp.ENGAGE_*`),
//!   `track: int`, `reason: MsgDict` (fire or hold reason, always present, L353), `holder: int` (`Bp.HOLDER_*`),
//!   `train_deg: float`, `elevation_deg: float`, `effective_range_m: float`, `local_control: bool`,
//!   `solution: SolutionDict or null`, `ready: int`, `magazine: int`, `reload_s: float`.
//! `SolutionDict`: `aim: Vector2` (map), `aim_alt_m: float`, `time_of_flight_s`, `iterations: int`,
//!   `converged: bool`, `error_m: float`.
//! `CompartmentStaticDict` (once): `id: int`, `name: MsgDict`, `deck: int`, `rect: Rect2` (plan view, metres,
//!   bow to the right), `side_rect: Rect2` (profile view), `volume_m3: float`, `has_magazine: bool`,
//!   `below_waterline: bool`, `neighbours: PackedInt32Array`.
//! `CompartmentDict` (2 Hz): `id: int`, `fill: float`, `fire: float`, `breached: bool`, `valve_open: bool`,
//!   `sealed: bool`.
//! `CrewDict` (on change): `original_berths: int`, `surviving_berths: int`, `passengers: int`,
//!   `passengers_lost: int`, `teams: Array[TeamDict]`, `repair_order: PackedInt32Array` (`Bp.REPAIR_*`).
//! `TeamDict`: `index: int`, `kind: int` (`Bp.TEAM_*`), `target: int` (compartment, conduit or part slot by
//!   kind; `Bp.NONE` when idle), `progress: float`, `pinned: bool`, `text: MsgDict`.
//! `NetworkDict` (2 Hz, damage-control networks view, L281-299): `kind: int` (`Bp.NETWORK_*`),
//!   `conduits: Array[{ "id": int, "from": int, "to": int, "cut": bool, "jury_rigged": bool, "redundant": bool, "pinned": bool }]`
//!   (`from`/`to` are compartment ids), `consumers_dead: int`, `consumers_on_buffer: int`.
//! `OffboardDict`: `slot: int`, `body: int` (entity or `Bp.NONE`), `aboard: int`, `endurance_s`,
//!   `recharging_s`, `name: MsgDict`.
//! `ActionDict` (`protocol::ActionView`, L786-797): `id: int`, `action: int` (`Bp.STATION_*`), `actor: int`,
//!   `state: int` (`Bp.ACTION_*`), `progress: float`, `block: MsgDict` (empty when not blocked),
//!   `at: Vector2 or null`.
//! `ObjectiveDict`: `id: int`, `ghost: bool`, `state: int` (`Bp.OBJECTIVE_*`), `text: MsgDict`,
//!   `progress: float`, `marker: Vector2 or null`.
//! `AllyDict`: `vessel: int`, `player: int`, `name: String`, `speed_kn`, `cells_lost: float`, `burning: bool`,
//!   `flooding: bool`, `emcon: int`, `officers_in_command: bool`, `pos: Vector2`, `heading_deg`.
//!
//! # Officers
//! `OfficerDict` (`protocol::OfficerReport`, L1664): `role: int` (`Bp.OFFICER_*`), `name: StringName` (billet
//!   text key), `task: MsgDict`, `reason: MsgDict`, `next: MsgDict`, `holder: int` (`Bp.HOLDER_*`),
//!   `layer: int` (`Bp.LAYER_*`), `overrides: int`, `advisory: MsgDict` (empty unless the player holds the role).
//! `BoardDict` (on change, at most 10 Hz): `officers: Array[OfficerDict]` in `Bp.OFFICER_*` order,
//!   `radiating: PackedInt32Array` (part slots), `esm_warning: bool`, `emcon: int`,
//!   `held: Array[RoleDict]`.
//! `RoleDict` (`protocol::ControlRole`): `kind: int` (`Bp.ROLE_*`), `index: int` (part slot for
//!   `Bp.ROLE_MOUNT`, group number for `Bp.ROLE_WEAPON_GROUP`, else 0).
//! `OfficerLineDict` (officer or ace radio line, L1706): `speaker: MsgDict`, `text: MsgDict`,
//!   `ace: StringName` (ace id or `&""`), `role: int` (`Bp.OFFICER_*` or `Bp.NONE`), `track: int`,
//!   `time_s: float`, `new_speaker: bool` (name the speaker), `sound: StringName` (radio cue stem:
//!   `&"radio_squelch_open"`, or `&"radio_ace_open"` for an ace; already queued on the audio feed).
//! `OverrideDict` (`protocol::OverrideView`, L351): `id: String` (opaque key for `Orders.revert`),
//!   `label: MsgDict`, `default_text: String`, `current_text: String`, `layer: int`.
//! `WhyDict` (L353): `slot: int`, `chain: Array[{ "text": MsgDict, "node": int, "part": int }]`.
//!
//! # Tracks
//! `TrackDict` (`protocol::TrackView`, 10 Hz; positions extrapolated per frame only inside `BpPlotView`):
//!   `id: int`, `label: String` (4 digits, tabular), `domain: int`, `stance: int`, `stance_reason: MsgDict`,
//!   `rung: int`, `rung_reason: MsgDict`, `grade: int`, `pos: Vector2`, `alt_m: float` (negative = depth),
//!   `course_deg`, `speed_kn`, `bearing_deg` and `range_m` (from own hull; -1.0 without a hull),
//!   `cpa_m: float`, `cpa_s: float`, `shape: ShapeDict`, `age_s: float`, `last_known: bool`,
//!   `class_id: StringName` (`&""` unless rung is Class or Exact), `class_name: MsgDict`,
//!   `candidates: int`, `sources: int` (`Bp.SOURCE_*` bits), `trend: int` (`Bp.TREND_*`), `inbound: bool`,
//!   `engaged_by: int`, `envelope_m: float`, `emitting: bool`, `struck: bool` (colours struck, seen by the
//!   side), `designated: bool`, `selected: bool`,
//!   `rank: int` (declutter rank, 0 = hide last, rule 14 L1670), `threat: float` (sort key, higher first),
//!   `vessel: int` (entity drawn in 3D for it, or `Bp.NONE`), `name: String` (accessible name, already
//!   translated: "Track 0412, Hostile, bearing 047").
//! `ShapeDict` (`protocol::UncertaintyShape`, L1662): `kind: int` (`Bp.SHAPE_*`); Circle: `radius_m`;
//!   Ellipse: `semi_major_m`, `semi_minor_m`, `bearing_deg`; Wedge: `apex: Vector2`, `bearing_deg`,
//!   `half_angle_deg`, `near_m`, `far_m`.
//! `TrackCardDict` (reply to `Orders.query_track_card`, L477): `track: int`,
//!   `candidates: Array[{ "class_id": StringName, "name": MsgDict, "domain": int, "civilian": bool, "own_side": bool }]`,
//!   `evidence: Array[{ "rung": int, "reason": MsgDict, "time_s": float }]`, `stance_reason: MsgDict`.
//!
//! # Alerts and log
//! `AlertDict` (`protocol::AlertView`, L1676-1687): `id: int`, `priority: int` (`Bp.PRIORITY_*`),
//!   `subsystem: int`, `state: int` (`Bp.ALERT_*`), `text: MsgDict`, `word: StringName` (priority word key),
//!   `raised_s: float`, `officer: int`, `blink_fast: bool`, `track: int`, `part: int`, `compartment: int`
//!   (each `Bp.NONE` when not about one), `sound: StringName` (`&"alert_caution"`, `&"alert_warning"`,
//!   `&"alert_alarm"` or `&"alert_emergency"`; already queued on the audio feed).
//! `AlertStripDict` (on change): `shown: Array[AlertDict]` (at most 5, highest priority then oldest),
//!   `more: int` ("+N"), `flood: bool`, `silenced_s: float` (seconds of silence left), `next_ack: int`
//!   (alert id Acknowledge would take, or `Bp.NONE`).
//! `LogEntryDict` (rule 15, L1671): `seq: int` (ascending), `time_s: float`, `kind: int` (`Bp.LOG_*`),
//!   `text: MsgDict`, `track: int`, `part: int`, `vessel: int`, `pos: Vector2 or null` (centre-on-click),
//!   `unread: bool`, `silent: bool` (routine: no sound, no highlight).
//!
//! # Environment, audio
//! `EnvironmentDict` (1 Hz and on weather change; lightning by signal): `weather: WeatherDict`,
//!   `blend_from: int`, `blend_to: int` (`Bp.WEATHER_*`), `blend: float`, `time_of_day: int`,
//!   `sun_dir: Vector3` (unit, towards the sun), `fog_density: float` (3.912 / V), `fog_sun_scatter: float`,
//!   `aerial_perspective: float`, `wind_offset: Vector2` (cloud drift, metres), `cloud_seed: int`,
//!   `preset: StringName` (for example `&"dusk_clear"`), `preset_to: StringName`.
//! `AudioEventDict` (one-shot): `sound: StringName` (an `audio::SoundId` file stem: the file is
//!   `res://audio/gen/<sound>.wav`, for example `&"weapon_gun_medium"`, `&"impact_splash_large"`,
//!   `&"alert_warning"`, `&"radio_squelch_open"`, `&"ui_click"`), `bus: StringName` (`audio::Bus::name()`:
//!   `&"Sfx"`, `&"Ambience"`, `&"Alert"`, `&"Radio"`, `&"Ui"`, `&"Music"`), `volume_db: float`,
//!   `pitch: float`, `positional: bool`, `pos: Vector3` (render space), `max_distance_m: float`,
//!   `delay_s: float` (speed-of-sound delay), `priority: int`.
//! `AudioLoopDict` (continuous, 30 Hz): `id: int` (stable per source), `sound: StringName`, `bus: StringName`,
//!   `volume_db: float`, `pitch: float`, `positional: bool`, `pos: Vector3`, `max_distance_m: float`.
//!   Stems that loop: the engine, wash, sea, wind and rain beds (`ambience_*` except thunder and sonar),
//!   `impact_fire_loop`, `impact_flooding_loop`, `impact_pump_loop`, the motor loops
//!   (`weapon_missile_motor`, `weapon_drone_motor`, `weapon_helicopter_rotor`, `weapon_rhib_outboard`,
//!   `weapon_turret_train`), `radio_static`, `alert_emergency` and `menu_theme`; `alert_alarm` is a
//!   one-shot pattern the player restarts for as long as it is listed. `AudioFeed.sound_info(stem)` says
//!   which is which.
//!
//! # Catalogue
//! `PartDefDict` (`content::part::PartDef`): `id: StringName`, `name: StringName`, `description: StringName`,
//!   `category: int`, `tier: int` (`Bp.TIER_*`), `size: Vector3i` (cells), `mass_kg: float`, `cost: int`,
//!   `power_kw: float` (demand, negative = supply), `placement: StringName` (`&"anywhere"`, `&"weather"`,
//!   `&"keel"`, `&"internal"`, `&"deck_edge"`), `tags: PackedStringArray` (snake_case `PartTag` words:
//!   `"crane"`, `"mine_rail"`, `"rhib_davit"`, `"boarding_kit"`, `"recovery_net"`, `"camera_mast"`,
//!   `"passenger_berths"`, `"point_defence"`, `"magazine"`, `"fuel_store"`, `"propulsion"`, `"steering"`,
//!   `"generation"`, `"low_signature"`; the same words as `Content.parts` `filter.tag` and template `needs`),
//!   `icon: StringName`, `available: bool`,
//!   `locked_reason: MsgDict`, `limit: int` (copies allowed by the ruleset, 0 = no limit), `in_store: int`,
//!   `stats: Array[{ "label": StringName, "value": float, "unit": StringName }]` (spec-sheet rows).
//! `KitDefDict` (`content::kit::KitDef`): `id: StringName`, `name: StringName`, `description: StringName`,
//!   `version: int`, `author: String`, `kind: StringName`, `parts: Array[{ "part": StringName, "offset": Vector3i, "yaw": int, "tilt": int }]`,
//!   `size: Vector3i`, `cost: int`, `mass_kg: float`, `tier: int`, `available: bool`, `locked_reason: MsgDict`,
//!   `settings: Array[{ "key": String, "label": StringName, "min": float, "max": float, "default": float }]`,
//!   `stock: bool`, `icon: StringName`.
//! `ClassDict` (enemy class for recognition and briefings): `id: StringName`, `name: StringName`,
//!   `role: StringName`, `tier: int`, `length_m`, `tonnage_t`, `blueprint: String` (stock path).
//! `RulesetDict` (`content::ruleset::Ruleset`): `id: String`, `kind: int` (`Bp.RULESET_*`), `tonnage_cap_t`,
//!   `complexity_per_vessel`, `complexity_per_lobby`, `max_length_m`, `luau: bool`,
//!   `part_limits: Dictionary` (`StringName` part id to `int`).
//! `DifficultyDict` (`content::campaign::DifficultySliders`, L847-860): `preset: StringName` (`&""` = custom),
//!   `ai_accuracy`, `ai_reaction_s`, `adaptation_cadence`, `hull_count: int`, `economy`, `clock_days: int`.
//!
//! # Blueprints and the dry dock
//! `BlueprintInfoDict`: `path: String` (`user://blueprints/...` or stock path), `stock: bool`, `name: String`,
//!   `author: String`, `hash: String` (hex), `format: int`, `game_version: String`, `kind: StringName`
//!   (`&"ship"`, `&"enemy_class"`, `&"kit"`), `length_m`, `beam_m`, `displacement_t`, `cost: int`,
//!   `complexity: float`, `parts: int`, `modified_unix: int`, `lineage: Array[{ "hash": String, "author": String }]`,
//!   `needs_migration: bool`, `newer_format: bool`.
//! `FolderEntryDict`: `name: String`, `path: String`, `is_folder: bool`, `info: BlueprintInfoDict or null`.
//! `IssueDict` (`content::validate::Issue` and load errors): `text: MsgDict`, `fatal: bool`, `part: int`,
//!   `cell: Vector3i or null`.
//! `StationDict` (`content::hull::Station`, L124): `index: int`, `x_m: float`, `beam_m`, `draft_m`,
//!   `deadrise_deg`, `flare_deg`, `sheer_m`; hull-level handles are in `HullDict`.
//! `HullDict`: `length_m`, `stations: Array[StationDict]`, `bow_rake_deg`, `transom: float`, `mirrored: bool`,
//!   `baked: bool` (freeform cells, one-way, L134), `deck_spacing_m`, `decks: PackedFloat32Array` (heights),
//!   `bulkheads: PackedFloat32Array` (x positions), `size_cells: Vector3i`, `waterline_m: float`.
//! `HandleDict` (one drag handle and its numeric field): `id: String` (`"beam"`, `"draft"`, `"deadrise"`,
//!   `"flare"`, `"sheer"`, `"bow_rake"`, `"transom"`), `station: int`, `value: float`, `min: float`,
//!   `max: float`, `step: float`, `unit: StringName`, `pos: Vector3` (render space of the editor view).
//! `PlacedPartDict` (`content::blueprint::PartPlacement`): `slot: int`, `part: StringName`, `cell: Vector3i`,
//!   `yaw: int` (quarter turns), `tilt: int`, `kit: int`, `label: String`, `load: float` (-1.0 = not loadable),
//!   `selected: bool`, `valid: bool`, `mirror_of: int`.
//! `PlacedKitDict`: `slot: int`, `kit: StringName`, `version: int`, `latest_version: int`, `cell: Vector3i`,
//!   `yaw: int`, `tilt: int`, `mirrored: bool`, `parts: PackedInt32Array`, `settings: Dictionary`
//!   (`String` key to `float`).
//! `GhostDict` (placement preview): `valid: bool`, `reason: MsgDict`, `cell: Vector3i`, `yaw: int`,
//!   `tilt: int`, `cells: PackedVector3Array` (occupied cells incl. mirrored copies), `cost: int`.
//! `PaintDict` (`content::blueprint::PaintEntry`): `index: int` (0..15), `token: StringName`
//!   (`&"hull.haze"`... or `&""`), `rgb: Vector3` (linear), `marking: bool`, `locked: bool` (first 9 entries).
//! `SelectionDict`: `parts: PackedInt32Array`, `kits: PackedInt32Array`, `cells: int` (count),
//!   `centre: Vector3`, `mass_kg`, `cost: int`.
//! `UndoDict`: `can_undo: bool`, `can_redo: bool`, `undo_label: MsgDict`, `redo_label: MsgDict`, `depth: int`,
//!   `dirty: bool` (unsaved changes).
//! `GaugeStripDict` (`editor::gauges::GaugeStrip`, L153-166; within 100 ms of an edit): `float: FloatGaugeDict`,
//!   `speed`, `power`, `endurance`, `punch`, `sight`, `resilience`, `complexity` (shapes below),
//!   `stale: PackedStringArray` (gauge names still updating, drawn with the stale glyph), `rev: int`.
//!   Each gauge has `check: int` (`Bp.CHECK_*`).
//!   `FloatGaugeDict`: `displacement_t`, `cap_t`, `reserve: float`, `draft_m`, `heel_deg`, `trim_deg`, `gm_m`,
//!     `gm_over_beam: float`, `gz: PackedVector2Array` (heel degrees, righting arm metres),
//!     `range_of_stability_deg`, `gm_caution_low: float`, `gm_caution_high: float`, `check`.
//!   `SpeedGaugeDict`: `top_speed_kn`, `turning_circle_m`, `check`.
//!   `PowerGaugeDict`: `generation_kw`, `cruise_demand_kw`, `combat_demand_kw`, `battery_kwh`, `check`.
//!   `EnduranceGaugeDict`: `cruise_kn`, `range_km`, `fuel_l`, `check`.
//!   `PunchGaugeDict`: `weight_of_fire_kg_per_min`, `ranges: Array[{ "slot": int, "range_m": float }]`, `check`.
//!   `SightGaugeDict`: `antenna_m`, `horizon_km: PackedFloat32Array` (3: 5 m, 20 m, 1,000 m targets),
//!     `radar_km: PackedFloat32Array` (3), `visual_km: PackedFloat32Array` (3),
//!     `coverage: PackedFloat32Array` (36 sectors of 10 degrees from the bow), `check`.
//!   `ResilienceGaugeDict`: `one_flooded: float`, `two_flooded: float`,
//!     `worst: Array[{ "a": int, "b": int, "heel_deg": float, "trim_deg": float, "survives": bool }]`,
//!     `systems_surviving_hit: float`, `check`.
//!   `ComplexityGaugeDict`: `points`, `caution_at`, `pvp_cap`, `check`, `measured: bool` (false until a trial ran).
//! `NoteDict` (`editor::notes::ShipNote`, L188-198): `index: int`, `id: StringName` (the note kind's stable id,
//!   `tutorial::note_id`, for example `arc_blocked`), `tier: int` (`Bp.NOTE_*`), `text: MsgDict`,
//!   `part: int`, `affected_parts: int`, `has_fix: bool`, `flips_tag: bool`, `score_effect: float` (0.0 = unknown),
//!   `fix_label: MsgDict`.
//! `SuggestionDict` (`editor::suggest::Suggestion`): `kind: int` (`Bp.SUGGEST_*`), `available: bool`,
//!   `text: MsgDict`.
//! `ArcDict` (`editor::gauges::ArcSector`): `from_deg`, `to_deg`, `blocked: bool`, `range_m`.
//! `ScorecardDict` (`editor::trials::Scorecard`, L227-229): `hash: String`, `turning_circle_m`,
//!   `time_to_90pct_speed_s`, `top_speed_kn`, `heel_recovery_s`, `peak_heel_deg`, `score: float`,
//!   `per_seed: PackedFloat32Array`, `wins: int`, `hull_left: float`, `rounds_per_kill: float`,
//!   `first_salvo_hit_rate: float`, `complexity_points: float`, `short_trial: bool`,
//!   `opponents: Array[StringName]`, `drift: Array[{ "gauge": String, "predicted": float, "simulated": float }]`,
//!   `saved_unix: int`, `label: String`.
//! `TagResultDict` (`editor::fitcheck::TagResult`): `text: MsgDict` (the tag as a sentence), `pass: bool`,
//!   `value: float`, `limit: float`, `unit: StringName`.
//! `FitResultDict` (`editor::fitcheck::FitResult`, L801): `state: int` (`Bp.JOB_*`), `passes: bool`,
//!   `tags: Array[TagResultDict]`, `top_speed_kn`, `time_to_turn_180_s`, `range_km`,
//!   `detect: Array[{ "class_id": StringName, "we_detect_km": float, "they_detect_km": float }]`,
//!   `simulated: bool`, `seakeeping: SeakeepingDict or null`.
//! `SeakeepingDict` (`editor::fitcheck::Seakeeping`): `sea_state: int`, `complete: bool` (every direction ran;
//!   a cut sweep is never safe), `rolls_over: bool`, `safe: bool`, `worst_heel_deg: float`,
//!   `worst_from_deg: float` (where the sea came from in the worst run, degrees off the bow, clockwise),
//!   `worst_rolled_over: bool` (that run went over), `short_range: bool` (the range of stability is under
//!   Fit Check's bar in this sea, so it can roll over whatever its runs did), `range_deg: float` (the range of
//!   stability), `runs: int` (directions run) and `directions: int` (directions due).
//! `RefitDiffDict` (`content::stats::RefitDiff` and `RefitCost`): `changes: Array[{ "kind": StringName
//!   (&"added", &"removed", &"moved"), "part": StringName, "name": StringName, "cell": Vector3i, "from_cell": Vector3i or null }]`,
//!   `structure_delta_t: float`, `salvage: int`, `hours: int`, `from_store: Dictionary` (part id to `int`),
//!   `affordable: bool`.
//! `QmResultDict` (`editor::quartermaster::QmResult`, L801-807): `state: int` (`Bp.JOB_*`), `kind: int`
//!   (`Bp.QM_*`), `diff: RefitDiffDict or null`, `fit: FitResultDict or null`, `blocking: MsgDict`.
//!
//! # Control ladder (L322-336)
//! `SliderDict` (L1): `key: String`, `label: StringName`, `tooltip: StringName`, `officer: int`, `min`, `max`,
//!   `step`, `default: float`, `value: float`, `overridden: bool`, `unit: StringName`, `basic: bool`
//!   (one of the 3 to 6 named sliders; false = under the expander).
//! `DoctrineEntryDict` (`content::graph::DoctrineEntry`, L479-489): `weapon: int` (`Bp.WEAPON_*`), `target: int`
//!   (`Bp.TARGET_*`), `min_stance: int`, `min_id: int`, `min_grade: int`, `salvo: int`,
//!   `effective_range_m: float` (-1.0 = default), `overridden: bool`.
//! `RuleDict` (`content::graph::Rule`, L336): `index: int`, `enabled: bool`, `when: ConditionDict`,
//!   `sink: SinkDict`, `value: ValueDict`, `text: MsgDict` (the rule as a sentence), `valid: bool`.
//! `ConditionDict`: `op: StringName` (`&"compare"`, `&"all"`, `&"any"`, `&"not"`); compare: `lhs: OperandDict`,
//!   `cmp: int` (`Bp.CMP_*`), `rhs: OperandDict`; all, any: `items: Array[ConditionDict]`; not: `item: ConditionDict`.
//! `OperandDict`: `kind: StringName` (`&"const"`, `&"own_speed_kn"`, `&"own_heel_deg"`, `&"fuel_fraction"`,
//!   `&"power_margin"`, `&"hull_health"`, `&"nearest_contact_range_m"`, `&"contact_count"`,
//!   `&"inbound_weapons"`, `&"part_health"`, `&"ammo_fraction"`, `&"sortie_time_s"`), `value: float` (const),
//!   `part: int`, `filter: { "min_stance": int, "domain": int, "min_grade": int }` (contact operands).
//! `SinkDict` (`content::graph::SinkKind`): `kind: StringName` (snake_case variant name), `part: int`, `track: int`.
//! `ValueDict` (`content::graph::Value`): `type: StringName` (`&"none"`, `&"bool"`, `&"scalar"`, `&"vec3"`,
//!   `&"text"`, `&"stance"`, `&"emcon"`, `&"preset"`, `&"rung"`, `&"nearest_contact"`), `value: Variant`.
//! `NodeTypeDict` (node library entry, L338-349): `type: String` (stable id), `group: int` (`Bp.NODE_*`),
//!   `name: StringName`, `description: StringName`,
//!   `inputs: Array[{ "name": StringName, "type": int }]`, `outputs: Array[...]` (same; `type` is `Bp.PORT_*`),
//!   `params: Array[{ "key": String, "label": StringName, "type": StringName, "default": Variant }]`,
//!   `writable: bool`, `phase_ok: bool`.
//! `GraphNodeDict`: `id: int`, `type: String`, `group: int`, `title: MsgDict`, `pos: Vector2`,
//!   `generated: bool`, `pinned: bool`, `replaced_stage: int` (`Bp.STAGE_*` or `Bp.NONE`), `officer: int`,
//!   `params: Dictionary` (`String` to `Variant`), `broken: bool` (a pinned node lost its input, L320),
//!   `inputs`, `outputs` as in `NodeTypeDict`.
//! `GraphEdgeDict`: `from_node: int`, `from_port: int`, `to_node: int`, `to_port: int`, `generated: bool`.
//! `ScriptDict` (L4, `content::graph::ScriptSource`; later phase): `index: int`, `name: String`,
//!   `source: String`, `enabled: bool`, `inputs`, `outputs` as in `NodeTypeDict`, `error: MsgDict`.
//!
//! # Campaign (L729-896)
//! `ResourcesDict` (`content::save::Resources`): `salvage: int`, `intel: int`, `fuel_l`, `ammo_kg: float`
//!   (total), `munitions: Dictionary` (munition id to `int`), `parts_store: Dictionary` (part id to `int`),
//!   `unlocked_parts: int`, `max_tier: int`.
//! `NodeDict` (theatre graph node, static): `id: StringName`, `name: StringName`, `pos: Vector2` (chart km),
//!   `region: StringName`, `kind: StringName` (`&"cove"`, `&"shipyard"`, `&"depot"`, `&"base"`, `&"sea"`...),
//!   `lanes: Array[StringName]`.
//! `RegionDict` (`theatre::RegionView`): `id: StringName`, `name: StringName`, `open: bool`, `alert: int`
//!   (floor, 0..5), `supply: float` (0..100), `patrol_factor: float`, `patrols: int` (patrols at sea now:
//!   the base count times `patrol_factor`, rounded), `combine_control: bool`,
//!   `cards: Array[CardDict]`, `queued: Array[CardDict]`, `weather: WeatherDict`.
//! `CardDict` (doctrine card, L772): `id: StringName`, `name: StringName`, `text: StringName`,
//!   `arrives_h: int` (theatre hour; 0 when active), `region: StringName`.
//! `ContactDict` (`theatre::ChartContact`): `id: int`, `node: StringName`, `destination: StringName`,
//!   `convoy: bool`, `hulls: int`, `ace: StringName`, `last_seen_h: int`.
//! `OfferDict` (`theatre::OperationOffer`): `id: StringName`, `kind: int` (`Bp.OPERATION_*`), `template: int`
//!   (`Bp.MISSION_*` or `Bp.NONE`), `name: StringName`, `node: StringName`, `region: StringName`,
//!   `forced: bool`, `transit_h: int`.
//! `ProposalDict` (`theatre::Proposal`, L839): `id: int`, `by: int` (player), `action: TheatreActionDict`,
//!   `accepted: PackedInt32Array` (players), `made_h: int`, `text: MsgDict`.
//! `TheatreActionDict` (`content::save::TheatreAction`): `kind: int` (`Bp.THEATRE_ACTION_*`), `node: StringName`,
//!   `hours: int`, `operation: StringName`, `player: int`, `intel: IntelDict or null`.
//! `IntelDict` (`content::save::IntelSpend`): `kind: int` (`Bp.INTEL_*`), `region: StringName`,
//!   `card: StringName`, `ace: StringName`, `cost: int`.
//! `ChartDict` (`theatre::ChartView`; on change, `Campaign.chart_changed`): `hour: int`, `day: int`,
//!   `act: int`, `retreat: bool`, `open_theatre: bool`, `clock_hours_left: float` (-1.0 = none),
//!   `sextant: StringName`, `coves: Array[StringName]`, `regions: Array[RegionDict]`,
//!   `cut_lanes: Array[{ "a": StringName, "b": StringName, "until_h": int }]`, `contacts: Array[ContactDict]`,
//!   `offers: Array[OfferDict]`, `proposals: Array[ProposalDict]`, `hunter_warning_h: int` (-1 = none),
//!   `tallies: Array[TallyDict]`, `time_of_day: int`, `phase: int` (`Bp.PHASE_*`), `at_cove: bool`,
//!   `can_save: int` (`Bp.SAVE_*`).
//! `TallyDict`: `text: MsgDict` (what is tallied), `value: float` (card at 10), `key: String`.
//! `ThreatDict` (`theatre::ThreatEntry`, L66): `class_id: StringName`, `name: MsgDict`, `count_min: int`,
//!   `count_max: int`, `confidence: int` (`Bp.CONFIDENCE_*`), `ace: StringName`.
//! `BriefingDict` (`theatre::Briefing`, orders format L1704): `operation: StringName`, `kind: int`,
//!   `name: StringName`, `text: StringName`, `region: StringName`, `node: StringName`, `weather: WeatherDict`,
//!   `time_of_day: int`, `distance_km`, `threats: Array[ThreatDict]`, `tags: Array[MsgDict]`,
//!   `ghosts: Array[{ "text": MsgDict, "reward": MsgDict }]`, `actions: PackedInt32Array` (`Bp.STATION_*`),
//!   `recommended_emcon: int` (`Bp.EMCON_*` or `Bp.NONE`), `route: PackedVector2Array`,
//!   `map: MapDict`, `committed_hulls: int`, `committed_tonnage_t`, `pushback_days`, `forced: bool`,
//!   `expires_h: int` (-1 = stays), `dtg: String` (date-time group).
//! `MapDict` (sortie area for the briefing chart and the plot's land layer): `size_m: Vector2`,
//!   `land: Array[PackedVector2Array]` (coast polygons), `shallows: Array[PackedVector2Array]`,
//!   `markers: Array[{ "name": String, "pos": Vector2, "label": StringName }]`.
//! `TheatreEventDict` (`theatre::TheatreEvent`, for the chart animation and log): `kind: StringName`
//!   (snake_case variant name), `text: MsgDict`, `hour: int`, plus the variant's fields under their Rust names
//!   with ids as `StringName` and players as `int`.
//! `InterruptDict` (`theatre::Interrupt`): `kind: int` (`Bp.INTERRUPT_*`), `text: MsgDict`, `node: StringName`,
//!   `operation: StringName`, `hours: int`, `player: int`.
//! `AfterActionDict` (`theatre::AfterAction`, L70): `operation: StringName`, `outcome: int`, `stats: StatsDict`,
//!   `payout: { "salvage_wrecks": int, "salvage_prizes": int, "intel": int, "blueprints": Array[StringName], "fuel_l": float, "ammo_kg": float }`,
//!   `tallies: Array[TallyDict]`, `kills_by_method: Array[{ "method": int, "count": int }]`,
//!   `queued_cards: Array[CardDict]`, `alert_changes: Array[{ "region": StringName, "alert": float, "cause": int }]`,
//!   `clock_pushback_days: float`, `belief: PackedVector3Array` (x, z, weight on the sortie map),
//!   `ghosts: Array[{ "text": MsgDict, "met": bool, "reward": MsgDict }]`,
//!   `repair_cost: Array[{ "player": int, "salvage": int }]`, `hulls_lost: PackedInt32Array`,
//!   `events: Array[TheatreEventDict]`.
//! `SaveSlotDict` (`content::save::SaveHeader`): `path: String`, `slot_name: String`, `campaign_id: String`,
//!   `sequence: int`, `ironman: bool`, `played_s: int`, `act: int`, `day: int`, `game_version: String`,
//!   `modified_unix: int`, `is_suspend: bool`, `loadable: bool`, `problem: MsgDict`, `players: PackedStringArray`.
//!
//! # Lobby (L955-1039)
//! `LobbyPlayerDict` (`net::msg::LobbyMember`): `slot: int`, `name: String`, `team: int`, `ready: bool`,
//!   `blueprint_hash: String` (hex, empty = none chosen; the exterior hash for the other PvP team),
//!   `blueprint_name: String` and `tonnage_t: float` (from the built design once it is in the local store,
//!   else `""` and -1.0), `complexity: float`, `upload: float` (received fraction of a design upload in
//!   progress, -1.0 = none), `is_host: bool`, `is_local: bool`, `conn: ConnectionDict`.
//! `LobbyDict` (`net::msg::LobbyView`, the same for the host and every member): `mode: int`,
//!   `name: String`, `ruleset: RulesetDict`, `players: Array[LobbyPlayerDict]`, `max_players: int`,
//!   `issues: Array[MsgDict]` (`modes::LobbyIssue` as sentences; empty = can start), `can_start: bool`,
//!   `saturated: bool`, `is_host: bool`, and the host's working setup, which the view does not carry:
//!   `setup: { "seed": int, "weather": int, "sea_state": int, "time_of_day": int, "skirmish": { "template":
//!   int, "act": int, "enemy_per_player": int, "difficulty": DifficultyDict } or null } or null` (`null` on a
//!   member).
//! `HostPanelDict` (`link::net::HostingInfo`, L992-1007, L1031): `hosting: bool`, `bound: bool` (the UDP
//!   port is open), `port: int`, `portmap: int` (`Bp.PORTMAP_*`), `portmap_text: MsgDict`
//!   (`net.portmap.*`), `external_address: String`, `lan_address: String`, `join_code: String`,
//!   `lobby_key: String` (16 characters), `vpn_address: String` (empty = no mesh VPN found),
//!   `vpn_join_code: String`, `upload_kbps: int`, `client_limit: int`, `saturated: bool`,
//!   `manual_steps: Array[MsgDict]`, `banned: int` (count).
//!
//! # Tutorial, benchmark
//! `TutorialStepDict` (`tutorial::Step`): `id: String`, `index: int`, `count: int`, `text: StringName`,
//!   `point_at: String` (stable control id or `""`), `allow: PackedStringArray` (empty = everything),
//!   `demo: String` (client animation name or `""`), `needs_continue: bool`, `lesson: int` and `lessons: int`
//!   (the spec's step number and how many there are; `lesson` 0 = unnumbered), `after: StringName` (what the
//!   player just achieved in the step before, or `&""`), `kits_wanted: PackedStringArray` and
//!   `kits_fitted: PackedStringArray` (the step's lesson kits not yet and already placed, in order).
//! `BenchStageDict`: `stage: int` (`Bp.BENCH_*`), `progress: float`, `seconds_left: float`, `text: MsgDict`.
//! `BenchReportDict` (L1322, L1401, L2045; also written as JSON): `build: String`, `os: String`, `cpu: String`,
//!   `ram_gb: float`, `adapter: String`, `driver_version: String`, `rendering_method: String`,
//!   `rendering_driver: String`, `fallbacks: PackedStringArray`, `tier: int`, `window: Vector2i`,
//!   `scale_3d: float`, `stages: Array[{ "stage": int, "frame_ms": Vector3 (p50, p95, p99), "gpu_ms": Vector3, "tick_ms": Vector2 (p95, p99), "adapter_ms_p99": float, "missed_vblanks": float }]`,
//!   `pipeline_compilations: int`, `video_mem_mb: float`, `peak_mem_mb: float`, `sim_calib_us: float`,
//!   `editor_gauge_ms_p95: float`, `editor_splash_ms_p95: float`, `thermal: PackedInt32Array`,
//!   `dpi_awareness: String`, `json: String`.
