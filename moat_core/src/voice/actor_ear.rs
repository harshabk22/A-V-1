//! Ear Actor — Audio Input (STT) with macOS VoiceProcessingIO for hardware AEC.
//!
//! Captures microphone input via CoreAudio's `kAudioUnitSubType_VoiceProcessingIO`,
//! performs energy-based Voice Activity Detection (VAD), and sends transcribed
//! text to the Brain actor. Supports barge-in detection and pause/resume via
//! control messages from the Brain.

use super::messages::{BrainToEar, EarToBrain};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

// ─────────────────────────────────────────────────────────────────────────────
// § STT Provider Trait
// ─────────────────────────────────────────────────────────────────────────────

/// Generic Speech-to-Text provider trait.
///
/// Implementations can wrap OpenAI Whisper, Deepgram, local Whisper.cpp, etc.
/// The audio format contract is: **mono, 16kHz, f32 PCM**.
#[async_trait::async_trait]
pub trait SttProvider: Send + Sync {
    /// Transcribe a buffer of raw PCM f32 audio data (mono, 16kHz).
    ///
    /// Returns the transcribed text, or an error string.
    async fn transcribe(&self, audio_data: &[f32]) -> Result<String, String>;
}

/// Stub STT provider for testing the audio pipeline without a real API.
///
/// Returns a debug message including the audio buffer length and peak amplitude.
pub struct StubSttProvider;

#[async_trait::async_trait]
impl SttProvider for StubSttProvider {
    async fn transcribe(&self, audio_data: &[f32]) -> Result<String, String> {
        let peak = audio_data.iter().map(|s| s.abs()).fold(0.0_f32, f32::max);
        let duration_secs = audio_data.len() as f32 / 16_000.0;
        eprintln!(
            "[StubSTT] Received {:.1}s of audio ({} samples, peak={:.4})",
            duration_secs,
            audio_data.len(),
            peak
        );
        Ok(format!(
            "[stub transcription: {:.1}s of audio, peak={:.4}]",
            duration_secs, peak
        ))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// § VAD Constants
// ─────────────────────────────────────────────────────────────────────────────

/// RMS energy threshold above which we consider the user to be speaking.
const SPEECH_THRESHOLD: f32 = 0.015;

/// RMS energy threshold below which we consider silence.
const SILENCE_THRESHOLD: f32 = 0.008;

/// How many consecutive silent chunks are needed to trigger end-of-utterance.
/// At 16kHz mono with 1024-sample chunks (~64ms each), 11 chunks ≈ 700ms.
const SILENCE_CHUNKS_FOR_END: usize = 11;

/// Minimum number of speech chunks before we consider it a valid utterance
/// (avoids transcribing single clicks/pops).
const MIN_SPEECH_CHUNKS: usize = 5;

/// Audio capture sample rate (STT standard).
#[allow(dead_code)]
const SAMPLE_RATE: f64 = 16_000.0;

/// Audio capture chunk size (samples per callback).
#[allow(dead_code)]
const CHUNK_SIZE: usize = 1024;

// ─────────────────────────────────────────────────────────────────────────────
// § VAD State Machine
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VadState {
    /// Waiting for speech to begin.
    Idle,
    /// Speech detected — buffering audio.
    Speaking,
    /// Silence detected after speech — waiting to confirm end-of-utterance.
    TrailingSilence,
}

/// Compute the RMS (Root Mean Square) energy of an audio chunk.
fn rms_energy(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

// ─────────────────────────────────────────────────────────────────────────────
// § Ear Actor
// ─────────────────────────────────────────────────────────────────────────────

/// The Ear actor — captures microphone input, detects speech, and sends
/// transcriptions to the Brain actor.
pub struct EarActor {
    /// Outgoing messages to the Brain actor.
    brain_tx: mpsc::Sender<EarToBrain>,
    /// Incoming control messages from the Brain actor.
    control_rx: mpsc::Receiver<BrainToEar>,
    /// Speech-to-Text provider.
    stt_provider: Arc<dyn SttProvider>,
    /// Whether the agent is currently executing/speaking (for barge-in detection).
    agent_busy: Arc<AtomicBool>,
}

impl EarActor {
    /// Create a new EarActor.
    ///
    /// # Arguments
    /// * `brain_tx` — Channel to send transcripts/barge-in signals to the Brain.
    /// * `control_rx` — Channel to receive Start/Pause commands from the Brain.
    /// * `stt_provider` — The STT implementation to use for transcription.
    pub fn new(
        brain_tx: mpsc::Sender<EarToBrain>,
        control_rx: mpsc::Receiver<BrainToEar>,
        stt_provider: Arc<dyn SttProvider>,
    ) -> Self {
        Self {
            brain_tx,
            control_rx,
            stt_provider,
            agent_busy: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Run the Ear actor's event loop.
    ///
    /// Sets up the CoreAudio VoiceProcessingIO AudioUnit for hardware AEC,
    /// then enters the main loop processing audio chunks and control messages.
    pub async fn run(mut self) {
        eprintln!("[Ear] Actor started.");

        // ── Set up audio capture via CoreAudio VoiceProcessingIO ──
        let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<f32>>(64);
        let capture_running = Arc::new(AtomicBool::new(false));
        let capture_running_clone = Arc::clone(&capture_running);
        let capture_status_tx = self.brain_tx.clone();

        // Spawn the audio capture on a blocking thread (CoreAudio callbacks
        // are real-time and must not be mixed with async).
        let capture_handle = std::thread::spawn(move || {
            if let Err(err) =
                run_audio_capture(audio_tx, capture_running_clone, capture_status_tx.clone())
            {
                eprintln!("[Ear] Audio capture error: {}", err);
                let _ = capture_status_tx.blocking_send(EarToBrain::CaptureFault(err));
            }
        });

        // ── Main event loop ──
        let mut listening = false;
        let mut vad_state = VadState::Idle;
        let mut audio_buffer: Vec<f32> = Vec::new();
        let mut speech_chunk_count: usize = 0;
        let mut silence_chunk_count: usize = 0;

        loop {
            tokio::select! {
                // ── Control messages from Brain ──
                ctrl = self.control_rx.recv() => {
                    match ctrl {
                        Some(BrainToEar::StartListening) => {
                            eprintln!("[Ear] StartListening received.");
                            listening = true;
                            self.agent_busy.store(false, Ordering::Relaxed);
                            capture_running.store(true, Ordering::Relaxed);
                            // Reset VAD state
                            vad_state = VadState::Idle;
                            audio_buffer.clear();
                            speech_chunk_count = 0;
                            silence_chunk_count = 0;
                        }
                        Some(BrainToEar::PauseListening) => {
                            eprintln!("[Ear] PauseListening received.");
                            listening = false;
                            self.agent_busy.store(true, Ordering::Relaxed);
                            capture_running.store(false, Ordering::Relaxed);
                            // Clear buffer — don't transcribe partial audio
                            audio_buffer.clear();
                            vad_state = VadState::Idle;
                            speech_chunk_count = 0;
                            silence_chunk_count = 0;
                        }
                        Some(BrainToEar::Shutdown) => {
                            eprintln!("[Ear] Shutdown received.");
                            capture_running.store(false, Ordering::Relaxed);
                            audio_buffer.clear();
                            break;
                        }
                        None => {
                            eprintln!("[Ear] Control channel closed. Shutting down.");
                            break;
                        }
                    }
                }

                // ── Audio chunks from CoreAudio capture ──
                chunk = audio_rx.recv() => {
                    let Some(samples) = chunk else {
                        eprintln!("[Ear] Audio channel closed. Shutting down.");
                        break;
                    };

                    if !listening {
                        continue; // Paused — ignore audio
                    }

                    let energy = rms_energy(&samples);

                    match vad_state {
                        VadState::Idle => {
                            if energy > SPEECH_THRESHOLD {
                                // Speech started!
                                vad_state = VadState::Speaking;
                                speech_chunk_count = 1;
                                silence_chunk_count = 0;
                                audio_buffer.clear();
                                audio_buffer.extend_from_slice(&samples);

                                // Barge-in detection: if the agent is busy, signal immediately
                                if self.agent_busy.load(Ordering::Relaxed) {
                                    eprintln!("[Ear] BARGE-IN detected during agent execution!");
                                    let _ = self.brain_tx.send(EarToBrain::BargeIn).await;
                                }
                            }
                        }

                        VadState::Speaking => {
                            audio_buffer.extend_from_slice(&samples);
                            if energy > SPEECH_THRESHOLD {
                                speech_chunk_count += 1;
                                silence_chunk_count = 0;
                            } else if energy < SILENCE_THRESHOLD {
                                silence_chunk_count += 1;
                                if silence_chunk_count >= SILENCE_CHUNKS_FOR_END {
                                    vad_state = VadState::TrailingSilence;
                                }
                            }
                        }

                        VadState::TrailingSilence => {
                            // End of utterance confirmed — transcribe the buffered audio
                            if speech_chunk_count >= MIN_SPEECH_CHUNKS {
                                eprintln!(
                                    "[Ear] End of utterance: {} speech chunks, {} samples",
                                    speech_chunk_count,
                                    audio_buffer.len()
                                );

                                let _ = self.brain_tx.send(EarToBrain::EndOfUtterance).await;

                                // Transcribe
                                let buffer = std::mem::take(&mut audio_buffer);
                                let stt = Arc::clone(&self.stt_provider);
                                let brain_tx = self.brain_tx.clone();

                                tokio::spawn(async move {
                                    match stt.transcribe(&buffer).await {
                                        Ok(text) if !text.trim().is_empty() => {
                                            eprintln!("[Ear] Transcription: \"{}\"", &text[..text.len().min(80)]);
                                            let _ = brain_tx.send(EarToBrain::Transcript(text)).await;
                                        }
                                        Ok(_) => {
                                            eprintln!("[Ear] Transcription was empty, ignoring.");
                                        }
                                        Err(err) => {
                                            eprintln!("[Ear] STT error: {}", err);
                                        }
                                    }
                                });
                            } else {
                                eprintln!(
                                    "[Ear] Discarding short utterance ({} chunks < minimum {})",
                                    speech_chunk_count, MIN_SPEECH_CHUNKS
                                );
                            }

                            // Reset for next utterance
                            vad_state = VadState::Idle;
                            audio_buffer.clear();
                            speech_chunk_count = 0;
                            silence_chunk_count = 0;
                        }
                    }
                }
            }
        }

        // Clean shutdown
        capture_running.store(false, Ordering::Relaxed);
        let _ = capture_handle.join();
        eprintln!("[Ear] Actor stopped.");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// § CoreAudio VoiceProcessingIO Capture
// ─────────────────────────────────────────────────────────────────────────────

/// Set up and run the CoreAudio AudioUnit for microphone capture.
///
/// Tries VoiceProcessingIO first (hardware AEC), then falls back to
/// HALOutput (standard mic, no AEC) if any step fails.
///
/// This function runs on a dedicated OS thread (not tokio) because CoreAudio
/// callbacks are real-time and must not block on async operations.
#[cfg(target_os = "macos")]
fn run_audio_capture(
    audio_tx: mpsc::Sender<Vec<f32>>,
    running: Arc<AtomicBool>,
    status_tx: mpsc::Sender<EarToBrain>,
) -> Result<(), String> {
    use coreaudio::audio_unit::IOType;

    // ── Try VoiceProcessingIO first (provides hardware AEC) ──
    match try_start_audio_unit(IOType::VoiceProcessingIO, &audio_tx, &running) {
        Ok(mut audio_unit) => {
            eprintln!(
                "[Ear] AudioUnit started (VoiceProcessingIO, AEC enabled). Capturing audio..."
            );
            let _ = status_tx.blocking_send(EarToBrain::CaptureReady);
            run_capture_loop(&audio_tx);
            let _ = audio_unit.stop();
            eprintln!("[Ear] AudioUnit stopped.");
            return Ok(());
        }
        Err(vprio_err) => {
            eprintln!(
                "[Ear] VoiceProcessingIO failed ({}), falling back to HALOutput...",
                vprio_err
            );
        }
    }

    // ── Fallback to HALOutput (standard mic capture, no AEC) ──
    match try_start_audio_unit(IOType::HalOutput, &audio_tx, &running) {
        Ok(mut audio_unit) => {
            eprintln!("[Ear] AudioUnit started (HALOutput, no AEC). Capturing audio...");
            let _ = status_tx.blocking_send(EarToBrain::CaptureReady);
            run_capture_loop(&audio_tx);
            let _ = audio_unit.stop();
            eprintln!("[Ear] AudioUnit stopped.");
            Ok(())
        }
        Err(hal_err) => {
            Err(format!(
                "Both VoiceProcessingIO and HALOutput failed to start. \
                 Check microphone permissions in System Settings > Privacy & Security > Microphone. \
                 HALOutput error: {}",
                hal_err
            ))
        }
    }
}

/// Attempt the full AudioUnit lifecycle: create → configure → callback → initialize → start.
///
/// Returns the running AudioUnit on success, or an error string if any step fails.
#[cfg(target_os = "macos")]
fn try_start_audio_unit(
    io_type: coreaudio::audio_unit::IOType,
    audio_tx: &mpsc::Sender<Vec<f32>>,
    running: &Arc<AtomicBool>,
) -> Result<coreaudio::audio_unit::AudioUnit, String> {
    use coreaudio::audio_unit::render_callback::{self, data};

    let mut audio_unit = setup_audio_unit(io_type)?;

    eprintln!(
        "[Ear] {:?} format configured. Setting up callback...",
        io_type
    );

    // ── Set up the input callback ──
    let tx = audio_tx.clone();
    let is_running = Arc::clone(running);

    audio_unit
        .set_input_callback(move |args: render_callback::Args<data::Interleaved<f32>>| {
            if !is_running.load(Ordering::Relaxed) {
                return Ok(());
            }
            let samples: Vec<f32> = args.data.buffer.to_vec();
            let _ = tx.try_send(samples);
            Ok(())
        })
        .map_err(|e| format!("Failed to set input callback for {:?}: {:?}", io_type, e))?;

    // ── Initialize and Start ──
    audio_unit
        .initialize()
        .map_err(|e| format!("Failed to initialize {:?}: {:?}", io_type, e))?;

    audio_unit
        .start()
        .map_err(|e| format!("Failed to start {:?}: {:?}", io_type, e))?;

    Ok(audio_unit)
}

/// Block the capture thread until the audio channel is closed (actor shutdown).
#[cfg(target_os = "macos")]
fn run_capture_loop(audio_tx: &mpsc::Sender<Vec<f32>>) {
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if audio_tx.is_closed() {
            break;
        }
    }
}

/// Helper: create and configure a CoreAudio AudioUnit for mic capture.
///
/// Handles the common setup steps for both VoiceProcessingIO and HALOutput:
/// - enable input on Element 1
/// - disable output on Element 0
/// - set stream format to mono 16kHz f32
#[cfg(target_os = "macos")]
fn setup_audio_unit(
    io_type: coreaudio::audio_unit::IOType,
) -> Result<coreaudio::audio_unit::AudioUnit, String> {
    use coreaudio::audio_unit::AudioUnit;

    let mut audio_unit = AudioUnit::new(io_type)
        .map_err(|e| format!("Failed to create {:?} AudioUnit: {:?}", io_type, e))?;

    // MUST uninitialize before changing EnableIO or StreamFormat properties
    audio_unit.uninitialize().unwrap_or(());

    // Enable input on the input scope of the input element (Element 1)
    let enable_input = 1u32;
    let disable_output = 0u32;

    audio_unit
        .set_property(
            coreaudio_sys::kAudioOutputUnitProperty_EnableIO,
            coreaudio::audio_unit::Scope::Input,
            coreaudio::audio_unit::Element::Input,
            Some(&enable_input),
        )
        .map_err(|e| format!("Failed to enable input IO: {:?}", e))?;

    audio_unit
        .set_property(
            coreaudio_sys::kAudioOutputUnitProperty_EnableIO,
            coreaudio::audio_unit::Scope::Output,
            coreaudio::audio_unit::Element::Output,
            Some(&disable_output),
        )
        .unwrap_or(());

    // ── Configure stream format: mono, 16kHz, f32 ──
    let mut input_format = audio_unit
        .input_stream_format()
        .map_err(|e| format!("Failed to read input stream format: {:?}", e))?;

    input_format.sample_rate = 16_000.0;
    input_format.channels = 1;
    input_format.sample_format = coreaudio::audio_unit::SampleFormat::F32;
    input_format.flags = coreaudio::audio_unit::audio_format::LinearPcmFlags::IS_FLOAT
        | coreaudio::audio_unit::audio_format::LinearPcmFlags::IS_PACKED;

    audio_unit
        .set_stream_format(
            input_format,
            coreaudio::audio_unit::Scope::Output,
            coreaudio::audio_unit::Element::Input,
        )
        .map_err(|e| format!("Failed to set 16kHz stream format: {:?}", e))?;

    Ok(audio_unit)
}

/// Fallback for non-macOS platforms — audio capture is not available.
#[cfg(not(target_os = "macos"))]
fn run_audio_capture(
    _audio_tx: mpsc::Sender<Vec<f32>>,
    _running: Arc<AtomicBool>,
    _status_tx: mpsc::Sender<EarToBrain>,
) -> Result<(), String> {
    eprintln!("[Ear] Audio capture is only available on macOS.");
    Err("VoiceProcessingIO is only available on macOS".into())
}
