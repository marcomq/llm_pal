//! Microphone to text: capture → resample → echo cancellation → VAD → segmenter → Smart Turn → prosody (`mic`),
//! then speech to text (`stt`).

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
