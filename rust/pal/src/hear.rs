//! Microphone to text.
//!
//! ```text
//! capture       cpal callback: downmix into a lock-free ring
//!    │
//!    ▼          thread pal-audio (mic.rs, Core::feed)
//! resample → echo (barge-in only) → vad → segmenter ─┬─ speech starts  → Event::SpeechStart, BargeIn while Pal speaks
//!                                                    ├─ short pause    → turn (Smart Turn) → Event::Pause(Segment)
//!                                                    └─ speech ended   → Event::Segment(Segment)
//!                                     each Segment carries its audio and prosody features (prosody.rs)
//!                                     a lost device → Event::Lost
//!    │
//!    ▼          the pal_mic endpoint (mic.rs): each event as an mq-bridge message
//!
//! stt           a Segment's audio → text (Parakeet), an mq-bridge caller
//! ```
//!
//! Where the events go and who asks `stt`: the diagram in `transport.rs`.

pub mod capture;
pub mod echo;
pub mod event;
pub mod mic;
pub mod prosody;
pub mod resample;
pub mod segmenter;
pub mod stt;
pub mod turn;
pub mod vad;
