//! The few things that differ per platform: `android.rs` on Android, `desktop.rs` everywhere else.

#[cfg_attr(target_os = "android", path = "android.rs")]
#[cfg_attr(not(target_os = "android"), path = "desktop.rs")]
mod imp;

pub(crate) use imp::*;
