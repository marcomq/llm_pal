//! Text to sound.
//!
//! ```text
//! handlers::split   tokens → sentences (sentences.rs)
//! handlers::speak   holds a reply until released, pauses on barge-in;
//!                   digits → words (numbers.rs) → tts::PocketSpeaker::say
//!                        thread: Pocket TTS → voice_filter → leveler
//!                        thread: playback.rs → loudspeaker, echo reference for hear::echo
//! ```
//!
//! Which topics the handlers read and what they report: the diagram in `transport.rs`.

pub mod handlers;
pub mod leveler;
pub mod numbers;
pub mod playback;
pub mod sentences;
pub mod tts;
pub mod voice_filter;
