//! Shared message types for the Tri-Actor voice concurrency model.
//!
//! These enums define the contract between the Ear, Brain, and Mouth actors.
//! All messages are sent via bounded `tokio::sync::mpsc` channels.

/// Messages from the Ear actor → Brain actor.
#[derive(Debug)]
pub enum EarToBrain {
    /// Microphone capture initialized successfully and is ready to stream audio.
    CaptureReady,
    /// Microphone capture failed to initialize or crashed during the session.
    CaptureFault(String),
    /// User finished speaking — transcribed text ready for processing.
    Transcript(String),
    /// User started speaking while agent was executing — interrupt signal.
    BargeIn,
    /// Silence detected after speech — user stopped talking.
    EndOfUtterance,
    /// End the voice session and ask the Brain actor to shut down cleanly.
    Shutdown,
}

/// Messages from the Brain actor → Mouth actor.
#[derive(Debug)]
pub enum BrainToMouth {
    /// Speak this text aloud via TTS.
    Speak(String),
    /// Stop speaking immediately (barge-in or cancellation).
    Silence,
    /// The voice session is complete — shut down the Mouth actor.
    SessionComplete,
}

/// Messages from the Brain actor → Ear actor.
#[derive(Debug)]
pub enum BrainToEar {
    /// Start capturing audio and streaming to STT.
    StartListening,
    /// Pause audio capture (e.g., during TTS playback to avoid feedback loops).
    PauseListening,
    /// End the voice session and shut the Ear actor down cleanly.
    Shutdown,
}
