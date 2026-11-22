//! Text to sound.
//!
//! ```text
//! handlers::speak   tokens → sentences (sentences.rs);
//!                   holds a reply until released, pauses on barge-in;
//!                   digits → words (numbers.rs) → tts::PocketSpeaker::say
//!                        thread: Fish Audio (optional, fish.rs) or Pocket TTS → voice_filter → leveler
//!                        thread: playback.rs → loudspeaker, echo reference for hear::echo
//! ```
//!
//! Which topics the handlers read and what they report: the diagram in `transport.rs`.

pub mod fish;
pub mod handlers;
pub mod leveler;
pub mod numbers;
pub mod playback;
pub mod sentences;
pub mod tts;
pub mod voice_filter;
