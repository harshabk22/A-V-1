//! Mouth Actor — Audio Output (TTS) via macOS `say` command.
//!
//! Receives `BrainToMouth` messages and speaks them aloud. Supports
//! immediate cancellation via `Silence` by killing the running `say`
//! child process.

use super::messages::BrainToMouth;
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command as AsyncCommand;
use tokio::sync::{mpsc, Mutex};

const MIN_TTS_OUTPUT_VOLUME: u8 = 55;

/// Handle to a running `say` child process.
/// Wrapped in `Arc<Mutex<>>` so barge-in can access it from the message loop.
type ChildHandle = Arc<Mutex<Option<tokio::process::Child>>>;

/// The Mouth actor — receives text from the Brain and speaks it via macOS TTS.
pub struct MouthActor {
    /// Receives messages from the Brain actor.
    rx: mpsc::Receiver<BrainToMouth>,
    /// Handle to the currently speaking `say` process (if any).
    active_child: ChildHandle,
    /// System output volume to restore when the voice session ends.
    restore_output_volume: Option<u8>,
}

impl MouthActor {
    /// Create a new MouthActor.
    ///
    /// Returns the actor and the sender channel for the Brain to use.
    pub fn new(buffer_size: usize) -> (Self, mpsc::Sender<BrainToMouth>) {
        let (tx, rx) = mpsc::channel(buffer_size);
        let actor = Self {
            rx,
            active_child: Arc::new(Mutex::new(None)),
            restore_output_volume: None,
        };
        (actor, tx)
    }

    /// Run the Mouth actor's event loop.
    ///
    /// Processes `BrainToMouth` messages until `SessionComplete` is received
    /// or the channel is closed.
    pub async fn run(mut self) {
        eprintln!("[Mouth] Actor started.");

        while let Some(msg) = self.rx.recv().await {
            match msg {
                BrainToMouth::Speak(text) => {
                    self.ensure_minimum_output_volume().await;
                    self.speak(&text).await;
                }
                BrainToMouth::Silence => {
                    self.kill_active_speech().await;
                }
                BrainToMouth::SessionComplete => {
                    self.kill_active_speech().await;
                    self.restore_output_volume().await;
                    eprintln!("[Mouth] Session complete. Shutting down.");
                    break;
                }
            }
        }

        self.restore_output_volume().await;
        eprintln!("[Mouth] Actor stopped.");
    }

    /// Speak the given text using the macOS `say` command.
    ///
    /// Spawns an async child process and waits for it to finish.
    /// If a `Silence` message arrives while speaking, the child is killed
    /// by the `kill_active_speech` method called from the message loop.
    ///
    /// Note: We use `tokio::select!` to listen for both the child completing
    /// and new messages arriving, so barge-in works mid-speech.
    async fn speak(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }

        // Sanitize the text for the `say` command:
        // - Replace newlines with spaces
        // - Truncate to avoid absurdly long utterances
        let sanitized: String = text
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .take(500)
            .collect();

        eprintln!(
            "[Mouth] Speaking: \"{}\"",
            &sanitized[..sanitized.len().min(80)]
        );

        let child = match AsyncCommand::new("say")
            .arg(&sanitized)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                eprintln!("[Mouth] Failed to spawn `say`: {}", err);
                return;
            }
        };

        // Store the child handle so barge-in can kill it
        {
            let mut guard = self.active_child.lock().await;
            *guard = Some(child);
        }

        // Wait for either: the child to finish, or a new message (barge-in)
        loop {
            let child_done = {
                let mut guard = self.active_child.lock().await;
                match guard.as_mut() {
                    Some(child) => child.try_wait().ok().flatten().is_some(),
                    None => true, // already killed
                }
            };

            if child_done {
                // Child finished normally
                let mut guard = self.active_child.lock().await;
                *guard = None;
                break;
            }

            // Check for incoming messages with a short poll interval
            match tokio::time::timeout(tokio::time::Duration::from_millis(50), self.rx.recv()).await
            {
                Ok(Some(BrainToMouth::Silence)) => {
                    self.kill_active_speech().await;
                    eprintln!("[Mouth] Speech interrupted by barge-in.");
                    break;
                }
                Ok(Some(BrainToMouth::SessionComplete)) => {
                    self.kill_active_speech().await;
                    eprintln!("[Mouth] Session complete during speech. Shutting down.");
                    return;
                }
                Ok(Some(BrainToMouth::Speak(new_text))) => {
                    // New speech request while still speaking — kill current and start new
                    self.kill_active_speech().await;
                    eprintln!("[Mouth] Interrupted for new speech.");
                    // Recurse to speak the new text
                    Box::pin(self.speak(&new_text)).await;
                    return;
                }
                Ok(None) => {
                    // Channel closed
                    self.kill_active_speech().await;
                    break;
                }
                Err(_) => {
                    // Timeout — no message, keep waiting for child
                    continue;
                }
            }
        }
    }

    /// Kill the currently running `say` process (if any).
    async fn kill_active_speech(&self) {
        let mut guard = self.active_child.lock().await;
        if let Some(ref mut child) = *guard {
            let _ = child.kill().await;
            let _ = child.wait().await;
            eprintln!("[Mouth] Killed active speech process.");
        }
        *guard = None;
    }

    async fn ensure_minimum_output_volume(&mut self) {
        #[cfg(target_os = "macos")]
        {
            if self.restore_output_volume.is_some() {
                return;
            }
            let Some(current) = current_output_volume().await else {
                return;
            };
            if current >= MIN_TTS_OUTPUT_VOLUME {
                return;
            }
            if set_output_volume(MIN_TTS_OUTPUT_VOLUME).await {
                self.restore_output_volume = Some(current);
                eprintln!(
                    "[Mouth] Raised system output volume from {} to {} for voice playback.",
                    current, MIN_TTS_OUTPUT_VOLUME
                );
            }
        }
    }

    async fn restore_output_volume(&mut self) {
        #[cfg(target_os = "macos")]
        {
            if let Some(previous) = self.restore_output_volume.take() {
                if set_output_volume(previous).await {
                    eprintln!("[Mouth] Restored system output volume to {}.", previous);
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
async fn current_output_volume() -> Option<u8> {
    let output = AsyncCommand::new("osascript")
        .arg("-e")
        .arg("output volume of (get volume settings)")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let parsed = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u8>()
        .ok()?;
    Some(parsed.min(100))
}

#[cfg(target_os = "macos")]
async fn set_output_volume(volume: u8) -> bool {
    AsyncCommand::new("osascript")
        .arg("-e")
        .arg(format!("set volume output volume {}", volume.min(100)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}
