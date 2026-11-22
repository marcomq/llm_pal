extends Control
## Pal's display: KITT's voice box, reacting only to Pal's own voice. L or a tap on the DE/EN label top right
## switches the language and restarts Pal.
## P or a tap on P0/P1 top left switches party mode (Pal answers only "KITT", jokes and silence) and restarts Pal.
## A tap on … (or S) opens the settings; saving them restarts Pal.

const SETTINGS_PATH := "user://settings.cfg"
const RECORD_AUDIO := "android.permission.RECORD_AUDIO"
## Must match package/unique_name in export_presets.cfg; models go to this app's folder (adb push).
const ANDROID_MODELS := "/storage/emulated/0/Android/data/io.github.marcomq.llmpal/files/models"
## Pal's voice is levelled to about 0.1 RMS; this maps it to 0..1.
const LEVEL_GAIN := 5.0
const ATTACK := 30.0
const RELEASE := 15.0
## Bulbs light up fast and glow after for about this long (seconds to 1/e).
const BULB_ON := 0.02
const BULB_AFTERGLOW := 0.18
const CENTER_ROWS := 8
const SIDE_ROWS := 6
## While nothing moves, Godot wakes this often and draws nothing.
const ACTIVE_FPS := 30
const IDLE_FPS := 8
const SETTLED := 0.004
const TEXT_RED := Color(0.6, 0.15, 0.1)
## For what is being said right now.
const TEXT_BRIGHT := Color(1.0, 0.34, 0.24)
## The user's line is gone this long after its last change, the fade included. The reply fades when Pal is done.
const SUBTITLE_SECONDS := 5.0
const SUBTITLE_FADE := 0.5
## A reply is bright this long after it begins.
const REPLY_BRIGHT_MS := 1500
const TEXTS := {
	"de": {"loading": "lädt Modelle …", "listening": "hört zu", "thinking": "denkt nach", "speaking": "spricht",
		"stopped": "gestoppt", "error": "Fehler: ", "no_mic": "kein Zugriff aufs Mikrofon",
		"model_failed": "neues Modell lief nicht, zurück zum alten",
		"persona_failed": "Persönlichkeit nicht lesbar, nutze KITT: %s",
		"voice_failed": "Stimme nicht lesbar, nutze die Standardstimme: %s",
		"remove_title": "Unbenutztes Modell löschen?", "remove_text": "%s löschen und %s GB freigeben?",
		"remove": "Löschen", "keep": "Behalten", "remove_failed": "%s konnte nicht gelöscht werden"},
	"en": {"loading": "loading models …", "listening": "listening", "thinking": "thinking", "speaking": "speaking",
		"stopped": "stopped", "error": "Error: ", "no_mic": "no access to the microphone",
		"model_failed": "the new model didn't run, back to the old one",
		"persona_failed": "can't read the personality, using KITT: %s",
		"voice_failed": "can't read the voice, using the default one: %s",
		"remove_title": "Delete the unused model?", "remove_text": "Delete %s and free %s GB?",
		"remove": "Delete", "keep": "Keep", "remove_failed": "couldn't delete %s"},
}

@onready var pal: PalNode = $Pal
@onready var kitt: ColorRect = $Kitt
@onready var status: Label = $Status
@onready var language: Label = $Language
@onready var menu: Label = $Menu
@onready var party: Label = $Party
@onready var you: Label = $You
@onready var reply: Label = $Reply

var _cfg := ConfigFile.new()
var _settings: Settings
var _removal: ConfirmationDialog
var _start_note := ""
var _party := false
var _lang := "de"
var _level := 0.0
var _phase := 0.0
var _think := 0.0
var _error := ""
var _shown_ms := {}
var _reply_bright_until := 0
var _center := PackedFloat32Array()
var _side := PackedFloat32Array()
var _tags := RegEx.create_from_string("(\\s*\\[[^\\]]*\\])+\\s*$")


func _ready() -> void:
	_center.resize(CENTER_ROWS)
	_side.resize(SIDE_ROWS)
	resized.connect(_fit_display)
	_fit_display()
	_cfg.load(SETTINGS_PATH)
	_lang = _cfg.get_value("display", "language", "de")
	language.text = _lang.to_upper()
	_party = _cfg.get_value("user", "party", false)
	_show_party()
	# Still on trial at launch: the last run never got ready with it, e.g. killed for lack of memory.
	_back_to_last_model()
	pal.failed.connect(func(msg: String) -> void:
		if _back_to_last_model():
			print("model failed: ", msg)
			_restart.call_deferred()
		else:
			_error = msg)
	pal.reply_failed.connect(func(msg: String) -> void: _error = msg)
	pal.ready_to_listen.connect(func(device: String) -> void:
		print("listening: ", device)
		_model_works.call_deferred())
	pal.heard.connect(func(text: String) -> void:
		_error = ""
		print("you: ", text)
		_subtitle(you, _tags.sub(text, ""))
		_subtitle(reply, ""))
	pal.said.connect(func(sentence: String) -> void:
		print("pal: ", sentence)
		if reply.text.is_empty():
			_reply_bright_until = Time.get_ticks_msec() + REPLY_BRIGHT_MS
		_subtitle(reply, (reply.text + " " + sentence).strip_edges()))
	pal.turn_done.connect(func(stt_ms: int, ms: int) -> void: print("     text after %d ms, first sentence after %d ms" % [stt_ms, ms]))
	if OS.get_name() != "Android":
		_fit_window()
		_start()
	else:
		get_tree().on_request_permissions_result.connect(_on_permission_result)
		if OS.request_permission(RECORD_AUDIO):
			_start()


## Desktop: 9:16 at 90 % of the usable screen height, centred.
func _fit_window() -> void:
	var screen := DisplayServer.window_get_current_screen()
	var usable := DisplayServer.screen_get_usable_rect(screen)
	var height := int(usable.size.y * 0.9)
	var window := Vector2i(height * 9 / 16, height)
	DisplayServer.window_set_size(window)
	DisplayServer.window_set_position(usable.position + (usable.size - window) / 2)


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
		for code in Settings.LANGUAGES:
			DirAccess.make_dir_recursive_absolute(ANDROID_MODELS.path_join("pocket/voices").path_join(code))
	_error = _start_note
	_start_note = ""
	pal.language = _lang
	var llm := OS.get_environment("PAL_LLM")
	pal.llm_model = llm if not llm.is_empty() else Settings.local_model(_cfg)
	pal.smart_turn = OS.get_environment("PAL_SMART_TURN") != "0"
	pal.barge_in = OS.get_environment("PAL_BARGE_IN") != "0"
	pal.user_name = Settings.user_name(_cfg)
	pal.party_mode = _party
	pal.persona_file = _usable_persona()
	pal.clear_llm_cache = _take_stale_cache()
	pal.pocket_voice = _usable_voice()
	var fish: bool = Settings.voice_source(_cfg) == "fish"
	pal.fish_api_key = _cfg.get_value("fish", "api_key", "") if fish else ""
	pal.fish_voice_id = _cfg.get_value("fish", "voice_id", "") if fish else ""
	pal.fish_model = Settings.fish_model(_cfg)
	pal.llm_provider = Settings.provider(_cfg)
	pal.llm_url = Settings.url(_cfg)
	pal.llm_remote_model = Settings.model(_cfg)
	pal.llm_effort = Settings.effort(_cfg)
	pal.llm_api_key = _cfg.get_value("llm", "api_key", "")
	pal.llm_fallback = _cfg.get_value("llm", "fallback", false)
	pal.start(_model_dir())


## The chosen voice; one of one's own that broke since it was chosen gives way to the first catalog voice.
func _usable_voice() -> String:
	var chosen := Settings.voice(_cfg, _lang, _model_dir())
	if chosen in Settings.VOICES:
		return chosen
	var wrong: String = PalNode.voice_error(Settings.voice_path(_model_dir(), _lang, chosen))
	if wrong.is_empty():
		return chosen
	_error = TEXTS[_lang]["voice_failed"] % wrong
	return Settings.VOICES[0]


func _take_stale_cache() -> bool:
	var stale: bool = _cfg.get_value("llm", "cache_stale", false)
	if stale:
		_cfg.set_value("llm", "cache_stale", false)
		_cfg.save(SETTINGS_PATH)
	return stale


## The chosen character file; if it broke since it was chosen, says so and returns "" for the built-in one.
func _usable_persona() -> String:
	var file := Settings.persona_path(_cfg, _lang, _model_dir())
	var wrong: String = PalNode.persona_error(file, _lang) if not file.is_empty() else ""
	if wrong.is_empty():
		return file
	_error = TEXTS[_lang]["persona_failed"] % wrong
	return ""


## PAL_MODELS overrides the models/ folder next to the Godot project (desktop) or the app folder (Android).
func _model_dir() -> String:
	var dir := OS.get_environment("PAL_MODELS")
	if not dir.is_empty():
		return dir
	if OS.get_name() == "Android":
		return ANDROID_MODELS
	return ProjectSettings.globalize_path("res://").path_join("../models").simplify_path()


## Undoes a model change that is still on trial; true if there was one.
func _back_to_last_model() -> bool:
	var last: String = _cfg.get_value("llm", "trying_from", "")
	if last.is_empty() or not Settings.runs_local_model(_cfg):
		return false
	_cfg.set_value("llm", "replaced", Settings.local_model(_cfg))
	_cfg.set_value("llm", "local_model", last)
	_cfg.set_value("llm", "trying_from", "")
	_cfg.save(SETTINGS_PATH)
	_start_note = TEXTS[_lang]["model_failed"]
	return true


## Pal is ready, so a model on trial works: only now may the one it replaced go, if the user agrees.
func _model_works() -> void:
	var old: String = _cfg.get_value("llm", "trying_from", "")
	if not old.is_empty() and Settings.runs_local_model(_cfg):
		_cfg.set_value("llm", "replaced", old)
		_cfg.set_value("llm", "trying_from", "")
		_cfg.save(SETTINGS_PATH)
	_offer_removal()


func _offer_removal() -> void:
	var old: String = _cfg.get_value("llm", "replaced", "")
	if old.is_empty() or is_instance_valid(_removal) or not OS.get_environment("PAL_LLM").is_empty():
		return
	var file := FileAccess.open(_model_dir().path_join(old), FileAccess.READ)
	if file == null or old == Settings.local_model(_cfg):
		_forget_replaced()
		return
	var gb := String.num(file.get_length() / 1e9, 1)
	var t: Dictionary = TEXTS[_lang]
	_removal = ConfirmationDialog.new()
	_removal.title = t["remove_title"]
	_removal.dialog_text = t["remove_text"] % [old.get_basename(), gb.replace(".", ",") if _lang == "de" else gb]
	_removal.dialog_autowrap = true
	_removal.ok_button_text = t["remove"]
	_removal.cancel_button_text = t["keep"]
	_removal.confirmed.connect(func() -> void:
		if pal.remove_llm(_model_dir(), old):
			_forget_replaced()
		else:
			_error = TEXTS[_lang]["remove_failed"] % old.get_basename()
			_removal.queue_free())
	_removal.canceled.connect(_forget_replaced)
	add_child(_removal)
	_removal.popup_centered(Vector2i(int(size.x * 0.85), 0))


func _forget_replaced() -> void:
	_cfg.set_value("llm", "replaced", "")
	_cfg.save(SETTINGS_PATH)
	if is_instance_valid(_removal):
		_removal.queue_free()



func _process(delta: float) -> void:
	var state := pal.state()
	status.text = TEXTS[_lang]["error"] + _error if _error else _state_text(state)
	_tint_text(state, pal.hearing())
	var target := clampf(pal.level() * LEVEL_GAIN, 0.0, 1.0)
	var rate := ATTACK if target > _level else RELEASE
	_level = lerpf(_level, target, 1.0 - exp(-rate * delta))

	var thinking := state == PalNode.THINKING or state == PalNode.LOADING
	_phase += delta * 6.0
	_think = lerpf(_think, 1.0 if thinking else 0.0, 1.0 - exp(-6.0 * delta))

	var fading := _fade_subtitles(state == PalNode.THINKING or state == PalNode.SPEAKING)
	var moving := thinking or state == PalNode.SPEAKING or target > 0.0 or fading \
		or maxf(_level, maxf(_think, maxf(_center[0], _side[0]))) > SETTLED
	var dialog := is_instance_valid(_settings) or is_instance_valid(_removal)
	Engine.max_fps = ACTIVE_FPS if moving or dialog else IDLE_FPS
	if not moving:
		return

	var mat := kitt.material as ShaderMaterial
	# Thinking and loading light only the middle segment, softly pulsing.
	var bar := _think * (0.7 + 0.3 * sin(_phase * 0.8))
	_center = _bulbs(_center, maxf(_level * CENTER_ROWS, bar), delta)
	_side = _bulbs(_side, _level * 0.8 * SIDE_ROWS, delta)
	mat.set_shader_parameter("center", _center)
	mat.set_shader_parameter("side", _side)
	mat.set_shader_parameter("time_s", Time.get_ticks_msec() / 1000.0)
	kitt.queue_redraw()


func _fit_display() -> void:
	(kitt.material as ShaderMaterial).set_shader_parameter("aspect", size.x / maxf(size.y, 1.0))
	kitt.queue_redraw()
	_subtitle(you, you.text)
	_subtitle(reply, reply.text)


func _tint_text(state: int, hearing: bool) -> void:
	_tint(you, state == PalNode.THINKING)
	_tint(reply, Time.get_ticks_msec() < _reply_bright_until)
	_tint(status, hearing or state == PalNode.SPEAKING)


func _tint(label: Label, bright: bool) -> void:
	var color := TEXT_BRIGHT if bright else TEXT_RED
	if label.get_theme_color("font_color") != color:
		label.add_theme_color_override("font_color", color)


## A long text shows its last lines.
func _subtitle(label: Label, text: String) -> void:
	if text != label.text:
		_shown_ms[label] = Time.get_ticks_msec()
		label.modulate.a = 1.0
	label.text = text
	label.lines_skipped = maxi(label.get_line_count() - label.max_lines_visible, 0)


## True while fading. `replying` holds the reply: between sentences Pal may be thinking again.
func _fade_subtitles(replying: bool) -> bool:
	if replying:
		_shown_ms[reply] = Time.get_ticks_msec()
	var you_fading := _fade(you, SUBTITLE_SECONDS)
	var reply_fading := _fade(reply, SUBTITLE_FADE)
	return you_fading or reply_fading


## Faded text is cleared, so a later sentence doesn't bring it back.
func _fade(label: Label, seconds: float) -> bool:
	if label.text.is_empty():
		return false
	var age: float = (Time.get_ticks_msec() - _shown_ms.get(label, 0)) / 1000.0
	var alpha := clampf((seconds - age) / SUBTITLE_FADE, 0.0, 1.0)
	label.modulate.a = alpha
	if alpha == 0.0:
		_subtitle(label, "")
	return alpha < 1.0


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
			return TEXTS[_lang]["listening"] + (" •" if pal.hearing() else "")
		PalNode.THINKING:
			key = "thinking"
		PalNode.SPEAKING:
			key = "speaking"
	return TEXTS[_lang][key]


## _input, not _unhandled_input: this Control consumes clicks and touches before they would get there.
func _input(event: InputEvent) -> void:
	if is_instance_valid(_settings) or is_instance_valid(_removal):
		return
	var key := event as InputEventKey
	var click := event as InputEventMouseButton
	var pressed_key := key != null and key.pressed and not key.echo
	var tapped := click != null and click.pressed
	if (pressed_key and key.keycode == KEY_L) or (tapped and language.get_global_rect().has_point(click.position)):
		_switch_language()
	elif (pressed_key and key.keycode == KEY_S) or (tapped and menu.get_global_rect().has_point(click.position)):
		_open_settings()
	elif (pressed_key and key.keycode == KEY_P) or (tapped and party.get_global_rect().has_point(click.position)):
		_party = not _party
		_show_party()
		_cfg.set_value("user", "party", _party)
		_cfg.save(SETTINGS_PATH)
		_restart()


func _switch_language() -> void:
	_lang = "en" if _lang == "de" else "de"
	language.text = _lang.to_upper()
	_cfg.set_value("display", "language", _lang)
	_cfg.save(SETTINGS_PATH)
	_restart()



func _open_settings() -> void:
	_settings = Settings.new(_cfg, SETTINGS_PATH, _lang, _model_dir())
	_settings.saved.connect(_restart)
	add_child(_settings)
	_settings.open()


func _restart() -> void:
	if OS.get_name() == "Android" and not OS.get_granted_permissions().has(RECORD_AUDIO):
		_error = TEXTS[_lang]["no_mic"]
		return
	_subtitle(you, "")
	_subtitle(reply, "")
	pal.stop()
	_start()


func _show_party() -> void:
	party.text = "P1" if _party else "P0"
