//! Local Whisper STT provider — transcribes audio using `whisper-rs`.
//!
//! Loads a GGML Whisper model from `~/.moatageex/models/` and performs
//! local inference with no network dependency.

use super::actor_ear::SttProvider;
use std::path::PathBuf;
use std::sync::Arc;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

// ─────────────────────────────────────────────────────────────────────────────
// § Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Default model filename to look for.
const DEFAULT_MODEL_FILENAME: &str = "ggml-base.en.bin";

/// Models directory under the MoatAgeex config root.
const MODELS_SUBDIR: &str = "models";

// ─────────────────────────────────────────────────────────────────────────────
// § Local Whisper STT Provider
// ─────────────────────────────────────────────────────────────────────────────

/// Speech-to-Text provider using local Whisper inference via `whisper-rs`.
///
/// The model file is loaded once during initialization and held in memory.
/// Transcription runs on a blocking thread to avoid starving the async runtime.
pub struct LocalWhisperStt {
    /// The loaded Whisper context (thread-safe).
    ctx: Arc<WhisperContext>,
}

impl LocalWhisperStt {
    /// Initialize the Whisper STT provider.
    ///
    /// Looks for a GGML model file at:
    /// 1. `model_path` if provided
    /// 2. `~/.moatageex/models/ggml-base.en.bin`
    ///
    /// Returns an error if the model file is not found or fails to load.
    pub fn new(model_path: Option<PathBuf>) -> Result<Self, String> {
        let path = match model_path {
            Some(p) => p,
            None => Self::default_model_path()?,
        };

        if !path.exists() {
            return Err(format!(
                "Whisper model not found at: {}. \
                 Download ggml-base.en.bin from https://huggingface.co/ggerganov/whisper.cpp/tree/main \
                 and place it in ~/.moatageex/models/",
                path.display()
            ));
        }

        eprintln!("[WhisperSTT] Loading model from: {}", path.display());

        let ctx = WhisperContext::new_with_params(
            path.to_str()
                .ok_or_else(|| "Model path contains invalid UTF-8".to_string())?,
            WhisperContextParameters::default(),
        )
        .map_err(|e| format!("Failed to load Whisper model: {:?}", e))?;

        eprintln!("[WhisperSTT] Model loaded successfully.");

        Ok(Self { ctx: Arc::new(ctx) })
    }

    /// Default model path: `~/.moatageex/models/ggml-base.en.bin`
    fn default_model_path() -> Result<PathBuf, String> {
        let home = dirs::home_dir().ok_or("Could not determine home directory")?;
        Ok(home
            .join(".moatageex")
            .join(MODELS_SUBDIR)
            .join(DEFAULT_MODEL_FILENAME))
    }

    /// Run Whisper inference on audio data.
    ///
    /// This is a blocking operation that should be called from a blocking thread.
    fn transcribe_blocking(ctx: &WhisperContext, audio_data: &[f32]) -> Result<String, String> {
        // Create inference parameters
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });

        // Configure for real-time speech transcription
        params.set_n_threads(2);
        params.set_translate(false);
        params.set_language(Some("en"));
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_print_special(false);
        params.set_suppress_blank(true);
        params.set_suppress_non_speech_tokens(true);

        // Disable token timestamps — we only need the text
        params.set_token_timestamps(false);

        // Create a state for this transcription
        let mut state = ctx
            .create_state()
            .map_err(|e| format!("Failed to create Whisper state: {:?}", e))?;

        // Run the model
        state
            .full(params, audio_data)
            .map_err(|e| format!("Whisper inference failed: {:?}", e))?;

        // Extract text segments
        let num_segments = state
            .full_n_segments()
            .map_err(|e| format!("Failed to get segment count: {:?}", e))?;

        let mut text = String::new();
        for i in 0..num_segments {
            if let Ok(segment_text) = state.full_get_segment_text(i) {
                let cleaned = Self::clean_segment(&segment_text);
                if !cleaned.is_empty() {
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(&cleaned);
                }
            }
        }

        Ok(text.trim().to_string())
    }

    /// Clean a Whisper text segment by removing common artifacts.
    fn clean_segment(text: &str) -> String {
        let trimmed = text.trim();

        // Skip common Whisper hallucinations
        let hallucinations = [
            "[BLANK_AUDIO]",
            "(silence)",
            "(music)",
            "(applause)",
            "(laughter)",
            "[Music]",
            "[MUSIC]",
            "[Silence]",
            "[ Silence ]",
            "(inaudible)",
            "...",
            "you",
            "You",
            // Common single-word hallucinations when there's background noise
            "Thanks for watching!",
            "Thank you for watching.",
            "Subscribe to my channel.",
            "Please subscribe.",
        ];

        if hallucinations
            .iter()
            .any(|h| trimmed.eq_ignore_ascii_case(h))
        {
            return String::new();
        }

        // Remove timestamp patterns like [00:00:00.000 --> 00:00:05.000]
        let mut result = trimmed.to_string();
        if let Some(end_bracket) = result.find(']') {
            if result.starts_with('[') && result[..end_bracket].contains("-->") {
                result = result[end_bracket + 1..].trim().to_string();
            }
        }

        result
    }
}

#[async_trait::async_trait]
impl SttProvider for LocalWhisperStt {
    async fn transcribe(&self, audio_data: &[f32]) -> Result<String, String> {
        if audio_data.is_empty() {
            return Ok(String::new());
        }

        let ctx = Arc::clone(&self.ctx);
        let data = audio_data.to_vec();

        // Run inference on a blocking thread — Whisper is CPU-intensive
        // and must not block the tokio runtime.
        let result = tokio::task::spawn_blocking(move || Self::transcribe_blocking(&ctx, &data))
            .await
            .map_err(|e| format!("Whisper task panicked: {:?}", e))?;

        result
    }
}
