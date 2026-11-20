//! macOS and other desktops: nothing to set up. `android.rs` has the same functions with real work.

/// cpal finds its devices by itself.
pub(crate) fn init_audio() {}

/// stderr already reaches the terminal.
pub(crate) fn init_log() {}

/// The tracing filter, from `RUST_LOG`.
pub(crate) fn log_filter() -> String {
    std::env::var("RUST_LOG").unwrap_or_default()
}

/// Lowering a thread's priority slows the whole process on macOS, and desktops have cores to spare.
pub(crate) fn background_thread() {}
