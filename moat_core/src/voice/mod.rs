//! Voice-Driven Actor Model for MoatAgeex.
//!
//! Three actors communicate via tokio mpsc channels:
//! - **Ear** (Audio Input / STT) — not yet implemented, will be Phase 31C
//! - **Brain** (Execution Engine) — receives transcripts, drives orchestrator
//! - **Mouth** (Audio Output / TTS) — speaks via macOS `say` command

pub mod actor_brain;
pub mod actor_ear;
pub mod actor_mouth;
pub mod messages;
#[cfg(feature = "whisper")]
pub mod whisper_stt;
