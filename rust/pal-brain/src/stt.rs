//! Speech to text: loads the pal-stt shared library (whisper.cpp) at runtime.
//! Linking whisper.cpp directly would clash with llama.cpp's different ggml version.

use std::ffi::{CStr, CString, c_char};
use std::path::{Path, PathBuf};

use libloading::Library;
use parakeet_rs::{ExecutionConfig, ParakeetTDT, Transcriber};

use crate::{Error, Result};

type NewFn = unsafe extern "C" fn(*const c_char, *mut *mut c_char) -> *mut Handle;
type TranscribeFn = unsafe extern "C" fn(*mut Handle, *const f32, usize, *mut *mut c_char) -> *mut c_char;
type SetLanguageFn = unsafe extern "C" fn(*mut Handle, *const c_char) -> bool;
type FreeStringFn = unsafe extern "C" fn(*mut c_char);
type FreeFn = unsafe extern "C" fn(*mut Handle);

#[repr(C)]
struct Handle {
    _private: [u8; 0],
}

pub struct Stt {
    handle: *mut Handle,
    transcribe: TranscribeFn,
    set_language: SetLanguageFn,
    free_string: FreeStringFn,
    free: FreeFn,
    _lib: Library,
}

// The handle is only used through &mut self.
unsafe impl Send for Stt {}

fn err(e: impl std::fmt::Display) -> Error {
    Error::Stt(e.to_string())
}

/// `libpal_stt` next to the running executable; test binaries live one level deeper in `deps/`.
pub fn default_library() -> PathBuf {
    let name = libloading::library_filename("pal_stt");
    let exe = std::env::current_exe().unwrap_or_default();
    let dir = exe.parent().unwrap_or(Path::new("."));
    let here = dir.join(&name);
    if here.exists() || dir.file_name().is_none_or(|n| n != "deps") { here } else { dir.join("..").join(name) }
}

impl Stt {
    pub fn new(library: &Path, model: &Path) -> Result<Self> {
        // SAFETY: pal-stt runs no initialisers with preconditions; the signatures match pal-stt/src/lib.rs.
        unsafe {
            let lib = Library::new(library).map_err(|e| err(format!("{}: {e}", library.display())))?;
            let new: NewFn = *lib.get(b"pal_stt_new").map_err(err)?;
            let transcribe: TranscribeFn = *lib.get(b"pal_stt_transcribe").map_err(err)?;
            let set_language: SetLanguageFn = *lib.get(b"pal_stt_set_language").map_err(err)?;
            let free_string: FreeStringFn = *lib.get(b"pal_stt_free_string").map_err(err)?;
            let free: FreeFn = *lib.get(b"pal_stt_free").map_err(err)?;
            let path = CString::new(model.to_str().ok_or_else(|| err("model path is not UTF-8"))?).map_err(err)?;
            let mut e = std::ptr::null_mut();
            let handle = new(path.as_ptr(), &mut e);
            if handle.is_null() {
                return Err(take_error(free_string, e));
            }
            Ok(Self { handle, transcribe, set_language, free_string, free, _lib: lib })
        }
    }

    /// The spoken language as an ISO 639-1 code; whisper detects it until set.
    pub fn set_language(&mut self, code: &str) -> Result<()> {
        let c = CString::new(code).map_err(err)?;
        // SAFETY: the handle is live and exclusively borrowed; c is NUL-terminated.
        if unsafe { (self.set_language)(self.handle, c.as_ptr()) } {
            Ok(())
        } else {
            Err(err(format!("unknown language {code:?}")))
        }
    }

    /// Text of 16 kHz mono speech, empty if whisper heard nothing.
    pub fn transcribe(&mut self, pcm: &[f32]) -> Result<String> {
        let mut e = std::ptr::null_mut();
        // SAFETY: the handle is live and exclusively borrowed; pcm is valid for its length.
        unsafe {
            let text = (self.transcribe)(self.handle, pcm.as_ptr(), pcm.len(), &mut e);
            if text.is_null() {
                return Err(take_error(self.free_string, e));
            }
            let s = CStr::from_ptr(text).to_string_lossy().into_owned();
            (self.free_string)(text);
            Ok(s)
        }
    }
}

/// # Safety
/// `e` is null or a string from pal-stt.
unsafe fn take_error(free_string: FreeStringFn, e: *mut c_char) -> Error {
    if e.is_null() {
        return err("unknown error");
    }
    let msg = unsafe { CStr::from_ptr(e) }.to_string_lossy().into_owned();
    unsafe { free_string(e) };
    err(msg)
}

/// Whisper through libpal_stt for a `.bin` model, otherwise Parakeet TDT from a model folder.
pub enum Recognizer {
    Whisper(Stt),
    Parakeet(Box<ParakeetTDT>),
}

impl Recognizer {
    /// `language` is an ISO 639-1 code; Parakeet ignores it and detects the language itself.
    pub fn new(library: &Path, model: &Path, language: &str) -> Result<Self> {
        if model.extension().is_some_and(|e| e == "bin") {
            let mut stt = Stt::new(library, model)?;
            stt.set_language(language)?;
            return Ok(Self::Whisper(stt));
        }
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(4));
        let config =
            ExecutionConfig::new().with_environment(pal_core::vad::environment()?.clone()).with_intra_threads(threads);
        let parakeet =
            ParakeetTDT::from_pretrained(model, Some(config)).map_err(|e| err(format!("{}: {e}", model.display())))?;
        Ok(Self::Parakeet(Box::new(parakeet)))
    }

    /// Text of 16 kHz mono speech, empty if nothing was understood.
    pub fn transcribe(&mut self, pcm: &[f32]) -> Result<String> {
        match self {
            Self::Whisper(stt) => stt.transcribe(pcm),
            Self::Parakeet(p) => {
                Ok(p.transcribe_samples(pcm.to_vec(), 16_000, 1, None).map_err(err)?.text.trim().to_string())
            }
        }
    }
}

impl Drop for Stt {
    fn drop(&mut self) {
        // SAFETY: the handle came from pal_stt_new and the library is still loaded.
        unsafe { (self.free)(self.handle) };
    }
}
