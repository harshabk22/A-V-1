//! Brain Actor — Execution Engine for the voice-driven actor model.
//!
//! Receives transcripts from the Ear actor, routes them through the
//! existing `MoatSystem::process_with_source()` pipeline, and sends speech feedback
//! to the Mouth actor. Handles barge-in by aborting running tool tasks.

use super::messages::{BrainToEar, BrainToMouth, EarToBrain};
use crate::gateway::SessionSource;
use crate::orchestrator::{SubTaskResult, TaskStatus};
use crate::system::MoatSystem;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

// ─────────────────────────────────────────────────────────────────────────────
// § Pre-Tool Speech Feedback
// ─────────────────────────────────────────────────────────────────────────────

/// Hardcoded pre-tool speech strings for sub-50ms audio feedback.
/// These are spoken *before* the tool executes so the user isn't
/// staring at silence during a 2-second app launch.
fn pre_route_speech(query: &str) -> &'static str {
    let q = query.trim().to_ascii_lowercase();

    // App operations
    if q.starts_with("open ") || q.starts_with("launch ") || q.starts_with("start ") {
        return "Opening...";
    }
    if q.starts_with("close ") || q.starts_with("quit ") {
        return "Closing...";
    }

    // Search
    if q.starts_with("search for ") || q.starts_with("google ") || q.starts_with("look up ") {
        return "Searching...";
    }

    // URL
    if q.starts_with("go to ") || q.starts_with("open http") || q.starts_with("open www.") {
        return "Opening...";
    }

    // System controls
    if q.starts_with("set volume") || q.starts_with("mute") || q.starts_with("unmute") {
        return "Adjusting volume...";
    }
    if q.contains("screenshot") {
        return "Taking screenshot...";
    }

    // Default for action tasks
    "Working on it..."
}

// ─────────────────────────────────────────────────────────────────────────────
// § Brain Actor
// ─────────────────────────────────────────────────────────────────────────────

/// The Brain actor — central execution engine of the voice model.
///
/// Receives user transcripts from the Ear, routes them through the
/// existing `MoatSystem::process_with_source()` pipeline, and sends results
/// to the Mouth for speech output.
pub struct BrainActor {
    /// Incoming messages from the Ear actor.
    ear_rx: mpsc::Receiver<EarToBrain>,
    /// Outgoing messages to the Mouth actor (TTS).
    mouth_tx: mpsc::Sender<BrainToMouth>,
    /// Outgoing control messages to the Ear actor.
    ear_control_tx: mpsc::Sender<BrainToEar>,
    /// The full MoatSystem pipeline (Router → Assistant → Orchestrator).
    system: Arc<MoatSystem>,
    /// Handle to the currently executing task (for barge-in abort).
    active_task: Option<JoinHandle<Vec<SubTaskResult>>>,
    /// Monotonic counter for generating unique root node IDs.
    node_counter: u64,
    /// Optional telemetry channel for forwarding voice events to the Tauri UI.
    telemetry_tx: Option<mpsc::Sender<String>>,
}

impl BrainActor {
    /// Create a new BrainActor.
    ///
    /// # Arguments
    /// * `system` — The fully initialized MoatSystem instance.
    /// * `ear_buffer` — Channel buffer size for the Ear → Brain channel.
    /// * `ear_control_buffer` — Channel buffer size for Brain → Ear control.
    ///
    /// # Returns
    /// `(actor, ear_tx, ear_control_tx, ear_control_rx)` — the actor, the sender for the Ear,
    /// the sender for Ear control, and the receiver for Ear control messages.
    pub fn new(
        system: Arc<MoatSystem>,
        mouth_tx: mpsc::Sender<BrainToMouth>,
        ear_buffer: usize,
        ear_control_buffer: usize,
        telemetry_tx: Option<mpsc::Sender<String>>,
    ) -> (
        Self,
        mpsc::Sender<EarToBrain>,
        mpsc::Sender<BrainToEar>,
        mpsc::Receiver<BrainToEar>,
    ) {
        let (ear_tx, ear_rx) = mpsc::channel(ear_buffer);
        let (ear_control_tx, ear_control_rx) = mpsc::channel(ear_control_buffer);

        let actor = Self {
            ear_rx,
            mouth_tx,
            ear_control_tx: ear_control_tx.clone(),
            system,
            active_task: None,
            node_counter: 1000, // start high to avoid collisions with text-mode IDs
            telemetry_tx,
        };

        (actor, ear_tx, ear_control_tx, ear_control_rx)
    }

    /// Emit a telemetry event to the Tauri UI (if the channel is connected).
    async fn emit_telemetry(&self, msg: &str) {
        if let Some(tx) = &self.telemetry_tx {
            let _ = tx.send(msg.to_string()).await;
        }
    }

    /// Run the Brain actor's event loop.
    ///
    /// Processes `EarToBrain` messages until the channel is closed.
    pub async fn run(mut self) {
        eprintln!("[Brain] Actor started.");

        // Tell the Ear to start listening
        let _ = self.ear_control_tx.send(BrainToEar::StartListening).await;

        while let Some(msg) = self.ear_rx.recv().await {
            match msg {
                EarToBrain::CaptureReady => {
                    self.emit_telemetry("[VOICE] Microphone capture ready.")
                        .await;
                }
                EarToBrain::CaptureFault(reason) => {
                    self.handle_capture_fault(reason).await;
                    break;
                }
                EarToBrain::Transcript(text) => {
                    self.handle_transcript(text).await;
                }
                EarToBrain::BargeIn => {
                    self.handle_barge_in().await;
                }
                EarToBrain::EndOfUtterance => {
                    // No-op for now — the transcript is the main signal.
                    // This can later be used for turn-taking heuristics.
                    eprintln!("[Brain] End of utterance detected.");
                }
                EarToBrain::Shutdown => {
                    self.handle_shutdown().await;
                    break;
                }
            }
        }

        // Clean shutdown
        let _ = self.mouth_tx.send(BrainToMouth::SessionComplete).await;
        eprintln!("[Brain] Actor stopped.");
    }

    /// Process a user transcript through the full MoatSystem pipeline.
    async fn handle_transcript(&mut self, text: String) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }

        eprintln!(
            "[Brain] Received transcript: \"{}\"",
            &trimmed[..trimmed.len().min(80)]
        );

        // ── Telemetry: emit user voice event to UI ──
        self.emit_telemetry(&format!("[USER VOICE]: {}", trimmed))
            .await;

        // ── Step 1: Pause the Ear during processing to avoid feedback ──
        let _ = self.ear_control_tx.send(BrainToEar::PauseListening).await;

        // ── Step 2: Pre-tool speech feedback (sub-50ms) ──
        let speech = pre_route_speech(trimmed);
        self.emit_telemetry(&format!("[AGENT VOICE]: {}", speech))
            .await;
        let _ = self
            .mouth_tx
            .send(BrainToMouth::Speak(speech.to_string()))
            .await;

        // ── Step 3: Generate a unique root node ID for this voice turn ──
        self.node_counter += 1;
        let root_node_id = self.node_counter;

        // ── Step 4: Create a status channel for streaming updates ──
        let (status_tx, mut status_rx) = mpsc::channel::<String>(32);

        // ── Step 5: Spawn the MoatSystem voice pipeline in a separate task ──
        let system = Arc::clone(&self.system);
        let query = trimmed.to_string();

        let task_handle: JoinHandle<Vec<SubTaskResult>> = tokio::spawn(async move {
            match system
                .process_with_source(
                    &query,
                    root_node_id,
                    Some(status_tx),
                    SessionSource::Voice,
                    Some("voice_session"),
                )
                .await
            {
                Ok(results) => results,
                Err(err) => {
                    eprintln!("[Brain] MoatSystem::process_with_source error: {}", err);
                    vec![SubTaskResult {
                        subtask_id: 0,
                        action: "system:error".to_string(),
                        target: String::new(),
                        status: TaskStatus::Failed {
                            exit_code: None,
                            stderr: err.to_string(),
                        },
                        elapsed: std::time::Duration::ZERO,
                        tokens_used: 0,
                    }]
                }
            }
        });

        self.active_task = Some(task_handle);

        // ── Step 6: Drain status updates while task runs ──
        // We forward interesting status messages to the Mouth for speech.
        // This runs concurrently — when the task finishes, the channel closes.
        while let Some(status_msg) = status_rx.recv().await {
            eprintln!(
                "[Brain] Status: {}",
                &status_msg[..status_msg.len().min(120)]
            );
            // Forward status to UI telemetry
            self.emit_telemetry(&status_msg).await;
        }

        // ── Step 7: Collect results ──
        let results = if let Some(handle) = self.active_task.take() {
            match handle.await {
                Ok(results) => results,
                Err(err) => {
                    eprintln!("[Brain] Task panicked or was cancelled: {}", err);
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        // ── Step 8: Speak the final result to the user ──
        let speech_output = Self::summarize_results(&results);
        if !speech_output.is_empty() {
            self.emit_telemetry(&format!("[AGENT VOICE]: {}", speech_output))
                .await;
            let _ = self.mouth_tx.send(BrainToMouth::Speak(speech_output)).await;
        }

        // ── Step 9: Re-enable the Ear for the next utterance ──
        let _ = self.ear_control_tx.send(BrainToEar::StartListening).await;
    }

    /// Handle a barge-in signal — user spoke while we were executing.
    async fn handle_barge_in(&mut self) {
        eprintln!("[Brain] BARGE-IN received!");

        // ── Step 1: Immediately silence the Mouth ──
        let _ = self.mouth_tx.send(BrainToMouth::Silence).await;

        // ── Step 2: Abort the running task if possible ──
        if let Some(handle) = self.active_task.take() {
            // Check if the task is still running
            if !handle.is_finished() {
                eprintln!("[Brain] Aborting active task due to barge-in.");
                handle.abort();

                // Wait briefly for the abort to take effect
                let _ = tokio::time::timeout(tokio::time::Duration::from_millis(100), handle).await;
            }
        }

        // ── Step 3: Confirm cancellation to the user ──
        self.emit_telemetry("[AGENT VOICE]: Stopped.").await;
        let _ = self
            .mouth_tx
            .send(BrainToMouth::Speak("Stopped.".to_string()))
            .await;

        // ── Step 4: Re-enable the Ear for the next utterance ──
        let _ = self.ear_control_tx.send(BrainToEar::StartListening).await;
    }

    async fn handle_shutdown(&mut self) {
        eprintln!("[Brain] Shutdown requested.");
        if let Some(handle) = self.active_task.take() {
            if !handle.is_finished() {
                handle.abort();
                let _ = tokio::time::timeout(tokio::time::Duration::from_millis(100), handle).await;
            }
        }
        let _ = self.mouth_tx.send(BrainToMouth::Silence).await;
        let _ = self.mouth_tx.send(BrainToMouth::SessionComplete).await;
    }

    async fn handle_capture_fault(&mut self, reason: String) {
        eprintln!("[Brain] Voice capture fault: {reason}");
        if let Some(handle) = self.active_task.take() {
            if !handle.is_finished() {
                handle.abort();
                let _ = tokio::time::timeout(tokio::time::Duration::from_millis(100), handle).await;
            }
        }
        let _ = self.ear_control_tx.send(BrainToEar::Shutdown).await;
        let _ = self.mouth_tx.send(BrainToMouth::Silence).await;
        self.emit_telemetry(&format!(
            "[VOICE ERROR] Microphone capture failed: {}",
            reason
        ))
        .await;
        self.emit_telemetry("[VOICE] Voice session stopped.").await;
    }

    /// Summarize task results into a concise speech string.
    ///
    /// Prioritizes brevity — the user doesn't want a verbose report read aloud.
    fn summarize_results(results: &[SubTaskResult]) -> String {
        if results.is_empty() {
            return "Done.".to_string();
        }

        // Count successes and failures
        let successes = results
            .iter()
            .filter(|r| matches!(r.status, TaskStatus::Success { .. }))
            .count();
        let failures = results
            .iter()
            .filter(|r| matches!(r.status, TaskStatus::Failed { .. }))
            .count();
        let cancellations = results
            .iter()
            .filter(|r| matches!(r.status, TaskStatus::Cancelled { .. }))
            .count();

        // Single result — speak the content directly
        if results.len() == 1 {
            match &results[0].status {
                TaskStatus::Success { stdout } => {
                    // For chat replies, speak the full response (truncated)
                    if results[0].action == "chat" || results[0].action == "done" {
                        let truncated: String = stdout.chars().take(300).collect();
                        return truncated;
                    }
                    return "Done.".to_string();
                }
                TaskStatus::Failed { stderr, .. } => {
                    let short: String = stderr.chars().take(100).collect();
                    return format!("Failed: {}", short);
                }
                TaskStatus::Cancelled { stage } => {
                    return format!("Cancelled at: {}", stage);
                }
                TaskStatus::SoftDenial { reason } => {
                    let short: String = reason.chars().take(100).collect();
                    return short;
                }
                TaskStatus::Blocked { .. } => {
                    return "Task is blocked.".to_string();
                }
            }
        }

        // Multiple results — summarize
        if failures == 0 && cancellations == 0 {
            format!("Completed {} steps successfully.", successes)
        } else if successes == 0 {
            format!("{} steps failed.", failures)
        } else {
            format!("{} steps completed, {} failed.", successes, failures)
        }
    }
}
