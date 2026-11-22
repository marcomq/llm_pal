//! What Pal says.
//!
//! ```text
//! transcript + prosody_tags → persona::user_line
//!   ├─ persona: joke, quip, pooled     a canned reply, no LLM (texts in language/)
//!   └─ enrich::prompt_line → persona::messages (system prompt + history)
//!        → brain::handler ─┬─ local_llm    llama.cpp on its own thread
//!                          └─ remote_llm   a server via mq-bridge; local_llm as fallback
//! character    who Pal is: built in or from a text file, per language
//! party    party mode: which lines are room talk, and what Pal says when the room goes quiet
//! placeholders    time, date and the like in a quip's text
//! ```
//!
//! How the messages reach `brain::handler` and where its tokens go: the diagram in `transport.rs`.

pub mod brain;
pub mod character;
pub mod enrich;
pub mod language;
pub mod local_llm;
pub mod party;
pub mod persona;
pub mod placeholders;
pub mod prosody_tags;
pub mod remote_llm;
