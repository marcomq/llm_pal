//! Speech to text with whisper.cpp behind a small C ABI, loaded at runtime by pal-brain.

use std::ffi::{CStr, CString, c_char};

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

/// whisper.cpp refuses input shorter than 1 s.
const MIN_SAMPLES: usize = 16_000 + 1600;
/// Segments whisper itself rates as silence are dropped; they are often hallucinated ("Subtitles by ...").
const NO_SPEECH_MAX: f32 = 0.6;
/// Peak below -50 dBFS counts as silence; whisper invents sentences for it.
const SILENCE_PEAK: f32 = 0.003;
/// Speech has about 3 words per second; more tokens than this are a hallucination loop ("A room filled. A room filled.
/// ..."), which otherwise runs on for seconds.
const TOKENS_PER_SECOND: usize = 8;

pub struct PalStt {
    state: WhisperState,
    threads: i32,
    language: String,
}

/// The encoder normally works on a 30 s window (1500 frames of 20 ms). Short clips only need their own length plus
/// a margin, which makes the encoder several times faster.
fn audio_ctx(samples: usize) -> i32 {
    (samples.div_ceil(320) + 128).next_multiple_of(64).min(1500) as i32
}

fn max_tokens(samples: usize) -> i32 {
    (samples.div_ceil(16_000) * TOKENS_PER_SECOND + 16) as i32
}

impl PalStt {
    fn new(model: &str) -> Result<Self, String> {
        if !std::path::Path::new(model).is_file() {
            return Err(format!("no whisper model at {model}"));
        }
        whisper_rs::install_logging_hooks();
        let ctx =
            WhisperContext::new_with_params(model, WhisperContextParameters::default()).map_err(|e| e.to_string())?;
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(4)) as i32;
        Ok(Self { state: ctx.create_state().map_err(|e| e.to_string())?, threads, language: "auto".into() })
    }

    fn transcribe(&mut self, pcm: &[f32]) -> Result<String, String> {
        if pcm.iter().all(|s| s.abs() < SILENCE_PEAK) {
            return Ok(String::new());
        }
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(&self.language));
        params.set_n_threads(self.threads);
        params.set_no_context(true);
        params.set_single_segment(true);
        params.set_no_timestamps(true);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        // Without this, an uncertain result is decoded up to five more times at rising temperatures: seconds on a phone.
        params.set_temperature_inc(0.0);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        let mut padded;
        let audio = if pcm.len() < MIN_SAMPLES {
            padded = pcm.to_vec();
            padded.resize(MIN_SAMPLES, 0.0);
            &padded[..]
        } else {
            pcm
        };
        params.set_audio_ctx(audio_ctx(audio.len()));
        params.set_max_tokens(max_tokens(pcm.len()));
        self.state.full(params, audio).map_err(|e| e.to_string())?;

        let mut text = String::new();
        for seg in self.state.as_iter() {
            if seg.no_speech_probability() <= NO_SPEECH_MAX {
                text.push_str(seg.to_str_lossy().map_err(|e| e.to_string())?.trim());
                text.push(' ');
            }
        }
        Ok(strip_annotations(&text))
    }
}

/// Removes whisper's sound notes like "* Musik *", "[Applaus]" or "(lacht)"; an unclosed one keeps its text.
fn strip_annotations(text: &str) -> String {
    let mut out = String::new();
    let mut note = String::new();
    let mut close = None;
    for c in text.chars() {
        match (close, c) {
            (None, '*' | '[' | '(') => {
                close = Some(if c == '*' {
                    '*'
                } else if c == '[' {
                    ']'
                } else {
                    ')'
                })
            }
            (Some(end), _) if c == end => {
                close = None;
                note.clear();
            }
            (None, _) => out.push(c),
            _ => note.push(c),
        }
    }
    out.push_str(&note);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn c_string(s: String) -> *mut c_char {
    CString::new(s.replace('\0', "")).expect("no NUL left").into_raw()
}

/// # Safety
/// `err` must be null or writable; a message written there is freed with [`pal_stt_free_string`].
unsafe fn set_err(err: *mut *mut c_char, msg: String) {
    if !err.is_null() {
        unsafe { *err = c_string(msg) };
    }
}

/// Loads a ggml whisper model. Returns null on failure and sets `*err`.
///
/// # Safety
/// `model` is a NUL-terminated UTF-8 path; `err` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pal_stt_new(model: *const c_char, err: *mut *mut c_char) -> *mut PalStt {
    let result = unsafe { CStr::from_ptr(model) }.to_str().map_err(|e| e.to_string()).and_then(PalStt::new);
    match result {
        Ok(stt) => Box::into_raw(Box::new(stt)),
        Err(e) => {
            unsafe { set_err(err, e) };
            std::ptr::null_mut()
        }
    }
}

/// Transcribes 16 kHz mono samples. Returns the text (possibly empty), or null on failure and sets `*err`.
///
/// # Safety
/// `stt` comes from [`pal_stt_new`] and is not used concurrently; `pcm` points to `len` samples.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pal_stt_transcribe(
    stt: *mut PalStt,
    pcm: *const f32,
    len: usize,
    err: *mut *mut c_char,
) -> *mut c_char {
    let pcm = if len == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(pcm, len) } };
    match unsafe { &mut *stt }.transcribe(pcm) {
        Ok(text) => c_string(text),
        Err(e) => {
            unsafe { set_err(err, e) };
            std::ptr::null_mut()
        }
    }
}

/// Sets the spoken language as an ISO 639-1 code such as "de" or "en"; false if whisper does not know it.
///
/// # Safety
/// `stt` comes from [`pal_stt_new`] and is not used concurrently; `language` is NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pal_stt_set_language(stt: *mut PalStt, language: *const c_char) -> bool {
    let Ok(code) = unsafe { CStr::from_ptr(language) }.to_str() else { return false };
    if whisper_rs::get_lang_id(code).is_none() {
        return false;
    }
    unsafe { &mut *stt }.language = code.to_string();
    true
}

/// # Safety
/// `s` is null or a string returned by this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pal_stt_free_string(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

/// # Safety
/// `stt` is null or comes from [`pal_stt_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pal_stt_free(stt: *mut PalStt) {
    if !stt.is_null() {
        drop(unsafe { Box::from_raw(stt) });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn strips_sound_notes() {
        assert_eq!(super::strip_annotations(" * Musik * "), "");
        assert_eq!(super::strip_annotations("Na ja (lacht) gut [Applaus]"), "Na ja gut");
        assert_eq!(super::strip_annotations("Ich (glaube das nicht"), "Ich glaube das nicht");
    }
}
