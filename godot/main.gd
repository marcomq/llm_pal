extends Control
## Pal's display: KITT's voice box or HAL's eye, reacting only to Pal's own voice. Space or a tap on the KITT/HAL
## label top left switches; L or a tap on the DE/EN label top right switches the language and restarts Pal.

const SETTINGS_PATH := "user://settings.cfg"
const RECORD_AUDIO := "android.permission.RECORD_AUDIO"
## Must match package/unique_name in export_presets.cfg; models go to this app's folder (adb push).
const ANDROID_MODELS := "/storage/emulated/0/Android/data/io.github.marcomq.llmpal/files/models"
## Speech RMS rarely exceeds 0.15; this maps it to 0..1.
const LEVEL_GAIN := 7.0
const ATTACK := 30.0
const RELEASE := 15.0
## Bulbs light up fast and glow after for about this long (seconds to 1/e).
const BULB_ON := 0.02
const BULB_AFTERGLOW := 0.18
const CENTER_ROWS := 8
const SIDE_ROWS := 6
const TEXTS := {
	"de": {"loading": "lädt Modelle …", "listening": "hört zu", "thinking": "denkt nach", "speaking": "spricht",
		"stopped": "gestoppt", "error": "Fehler: ", "no_mic": "kein Zugriff aufs Mikrofon"},
	"en": {"loading": "loading models …", "listening": "listening", "thinking": "thinking", "speaking": "speaking",
		"stopped": "stopped", "error": "Error: ", "no_mic": "no access to the microphone"},
}

@onready var pal: PalNode = $Pal
@onready var kitt: ColorRect = $Kitt
@onready var hal: TextureRect = $Hal
@onready var status: Label = $Status
@onready var language: Label = $Language
@onready var display: Label = $Display

var _cfg := ConfigFile.new()
var _hal := false
var _lang := "de"
var _level := 0.0
var _phase := 0.0
var _think := 0.0
var _error := ""
var _center := PackedFloat32Array()
var _side := PackedFloat32Array()


func _ready() -> void:
	_center.resize(CENTER_ROWS)
	_side.resize(SIDE_ROWS)
	_cfg.load(SETTINGS_PATH)
	_hal = _cfg.get_value("display", "hal", false)
	_lang = _cfg.get_value("display", "language", "de")
	language.text = _lang.to_upper()
	_apply_mode()
	pal.failed.connect(func(msg: String) -> void: _error = msg)
	pal.ready_to_listen.connect(func(device: String) -> void: print("listening: ", device))
	pal.heard.connect(func(text: String) -> void: print("you: ", text))
	pal.said.connect(func(sentence: String) -> void: print("pal: ", sentence))
	pal.turn_done.connect(func(stt_ms: int, ms: int) -> void: print("     text after %d ms, first sentence after %d ms" % [stt_ms, ms]))
	if OS.get_name() != "Android":
		_start()
	else:
		get_tree().on_request_permissions_result.connect(_on_permission_result)
		if OS.request_permission(RECORD_AUDIO):
			_start()


func _on_permission_result(permission: String, granted: bool) -> void:
	if permission != RECORD_AUDIO:
		return
	if granted:
		_start()
	else:
		_error = TEXTS[_lang]["no_mic"]


func _start() -> void:
	if OS.get_name() == "Android":
		# Created by the app, so it owns the folder; one made by adb stays unreadable for it.
		DirAccess.make_dir_recursive_absolute(ANDROID_MODELS)
	_error = ""
	pal.language = _lang
	pal.start(_model_dir(), _stt_library())


## PAL_MODELS overrides the models/ folder next to the Godot project (desktop) or the app folder (Android).
func _model_dir() -> String:
	var dir := OS.get_environment("PAL_MODELS")
	if not dir.is_empty():
		return dir
	if OS.get_name() == "Android":
		return ANDROID_MODELS
	return ProjectSettings.globalize_path("res://").path_join("../models").simplify_path()


## On Android the library sits in the APK's native folder, so the loader finds it by name.
func _stt_library() -> String:
	if OS.get_name() == "Android":
		return "libpal_stt.so"
	return ProjectSettings.globalize_path("res://bin/macos/libpal_stt.dylib")


func _process(delta: float) -> void:
	var state := pal.state()
	var target := clampf(pal.level() * LEVEL_GAIN, 0.0, 1.0)
	var rate := ATTACK if target > _level else RELEASE
	_level = lerpf(_level, target, 1.0 - exp(-rate * delta))

	var thinking := state == PalNode.THINKING or state == PalNode.LOADING
	_phase += delta * 6.0
	_think = lerpf(_think, 1.0 if thinking else 0.0, 1.0 - exp(-6.0 * delta))
	var pulse := 0.4 + 0.25 * _think * (0.5 + 0.5 * sin(_phase * 0.7))

	var mat := (hal if _hal else kitt).material as ShaderMaterial
	mat.set_shader_parameter("aspect", size.x / maxf(size.y, 1.0))
	if _hal:
		mat.set_shader_parameter("level", _level)
		mat.set_shader_parameter("glow", pulse)
	else:
		var breath := _think * (0.35 + 0.25 * sin(_phase * 0.8))
		_center = _bulbs(_center, maxf(_level, breath) * CENTER_ROWS, delta)
		_side = _bulbs(_side, maxf(_level * 0.8, breath * 0.6) * SIDE_ROWS, delta)
		mat.set_shader_parameter("center", _center)
		mat.set_shader_parameter("side", _side)
		mat.set_shader_parameter("time_s", Time.get_ticks_msec() / 1000.0)
	status.text = TEXTS[_lang]["error"] + _error if _error else _state_text(state)


## Rows count from the middle; `lit` rows (fractional) are on. Bulbs warm up quickly and fade slowly.
func _bulbs(rows: PackedFloat32Array, lit: float, delta: float) -> PackedFloat32Array:
	for i in rows.size():
		var target := clampf(lit - i, 0.0, 1.0)
		var tau := BULB_ON if target > rows[i] else BULB_AFTERGLOW
		rows[i] = lerpf(rows[i], target, 1.0 - exp(-delta / tau))
	return rows


func _state_text(state: int) -> String:
	var key := "stopped"
	match state:
		PalNode.LOADING:
			key = "loading"
		PalNode.LISTENING:
			key = "listening"
		PalNode.THINKING:
			key = "thinking"
		PalNode.SPEAKING:
			key = "speaking"
	return TEXTS[_lang][key]


## _input, not _unhandled_input: this Control consumes clicks and touches before they would get there.
func _input(event: InputEvent) -> void:
	var key := event as InputEventKey
	var click := event as InputEventMouseButton
	var pressed_key := key != null and key.pressed and not key.echo
	var tapped := click != null and click.pressed
	if (pressed_key and key.keycode == KEY_L) or (tapped and language.get_global_rect().has_point(click.position)):
		_switch_language()
	elif (pressed_key and key.keycode == KEY_SPACE) or (tapped and display.get_global_rect().has_point(click.position)):
		_hal = not _hal
		_apply_mode()
		_cfg.set_value("display", "hal", _hal)
		_cfg.save(SETTINGS_PATH)


func _switch_language() -> void:
	_lang = "en" if _lang == "de" else "de"
	language.text = _lang.to_upper()
	_cfg.set_value("display", "language", _lang)
	_cfg.save(SETTINGS_PATH)
	if OS.get_name() == "Android" and not OS.get_granted_permissions().has(RECORD_AUDIO):
		_error = TEXTS[_lang]["no_mic"]
		return
	pal.stop()
	_start()


func _apply_mode() -> void:
	kitt.visible = not _hal
	hal.visible = _hal
	display.text = "HAL" if _hal else "KITT"
