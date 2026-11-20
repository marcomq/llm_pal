//! macOS and other desktops: nothing to set up. `android.rs` has the same functions with real work.

/// cpal finds its devices by itself.
pub(crate) fn init_audio() {}

/// stderr already reaches the terminal.
pub(crate) fn init_log() {}

/// Lowering a thread's priority slows the whole process on macOS, and desktops have cores to spare.
pub(crate) fn background_thread() {}
