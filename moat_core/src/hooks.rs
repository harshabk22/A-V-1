use crate::gateway::GatewaySearchHit;
use crate::memory::{compile_context_summary, MemorySummaryOptions};
use chrono::Utc;

use crate::MoatResult;
use rusqlite::Connection;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc::Sender, Mutex as AsyncMutex};
use uuid::Uuid;

const MEMORY_HOOK_TIMEOUT: Duration = Duration::from_secs(15);
const ACTIVE_MEMORY_HOOK_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Default)]
pub struct ActiveMemoryPrelude {
    pub context_block: String,
    pub memories: Vec<String>,
}

pub async fn run_precompute_memory_hook(
    query: &str,
    db: Arc<AsyncMutex<Connection>>,

    status_tx: Option<Sender<String>>,
) -> String {
    if let Some(tx) = &status_tx {
        let _ = tx.send("🧠 Searching memory...".into()).await;
    }

    let query = query.to_string();
    let task = tokio::spawn(async move {
        compile_context_summary(&query, &db, &MemorySummaryOptions::default()).await
    });

    match tokio::time::timeout(MEMORY_HOOK_TIMEOUT, task).await {
        Ok(Ok(Ok(summary))) => summary,
        Ok(Ok(Err(err))) => format!("[SYSTEM FAULT] memory_searcher failed safely: {err}"),
        Ok(Err(join_err)) => format!("[SYSTEM FAULT] memory_searcher panicked safely: {join_err}"),
        Err(_) => {
            format!(
                "[SYSTEM FAULT] memory_searcher timed out after {}s and was terminated safely.",
                MEMORY_HOOK_TIMEOUT.as_secs()
            )
        }
    }
}

pub async fn run_active_memory_prelude(
    query: &str,
    session_context: Option<&str>,
    gateway_hits: &[GatewaySearchHit],
    db: Arc<AsyncMutex<Connection>>,

    status_tx: Option<Sender<String>>,
) -> ActiveMemoryPrelude {
    if let Some(tx) = &status_tx {
        let _ = tx
            .send("🧠 Activating rolling/session memory...".into())
            .await;
    }

    let query = query.to_string();
    let session_context = session_context
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let gateway_hits = gateway_hits.to_vec();
    let task = tokio::spawn(async move {
        compile_context_summary(
            &query,
            &db,
            &MemorySummaryOptions {
                top_k: 6,
                max_chars: 700,
                max_file_reads: 2,
            },
        )
        .await
    });

    let compiled_summary = match tokio::time::timeout(ACTIVE_MEMORY_HOOK_TIMEOUT, task).await {
        Ok(Ok(Ok(summary))) => summary.trim().to_string(),
        Ok(Ok(Err(err))) => format!("[SYSTEM FAULT] active_memory failed safely: {err}"),
        Ok(Err(join_err)) => {
            format!("[SYSTEM FAULT] active_memory panicked safely: {join_err}")
        }
        Err(_) => format!(
            "[SYSTEM FAULT] active_memory timed out after {}s and was terminated safely.",
            ACTIVE_MEMORY_HOOK_TIMEOUT.as_secs()
        ),
    };

    let mut context_sections = Vec::new();
    let mut memories = Vec::new();
    if !compiled_summary.is_empty() {
        context_sections.push(compiled_summary.clone());
        memories.push(compiled_summary.clone());
    }
    if let Some(context) = session_context {
        let clipped = clip_text(&context, 1400);
        context_sections.push(format!(
            "<rolling_session_context>\n{}\n</rolling_session_context>",
            clipped
        ));
        memories.push(format!(
            "[rolling_session_context]\n{}",
            clip_text(&clipped, 900)
        ));
    }
    if !gateway_hits.is_empty() {
        let gateway_context = gateway_hits
            .iter()
            .map(|hit| {
                format!(
                    "- [{}] {}{} :: {}",
                    hit.source.as_str(),
                    hit.title,
                    hit.channel
                        .as_deref()
                        .map(|channel| format!(" ({channel})"))
                        .unwrap_or_default(),
                    clip_text(&hit.snippet, 240)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        context_sections.push(format!(
            "<gateway_session_hits>\n{}\n</gateway_session_hits>",
            gateway_context
        ));
        memories.push(format!("[gateway_session_hits]\n{}", gateway_context));
    }

    ActiveMemoryPrelude {
        context_block: context_sections.join("\n"),
        memories,
    }
}

pub async fn report_background_fault(status_tx: Option<Sender<String>>, label: &str, err: &str) {
    if let Some(tx) = status_tx {
        let _ = tx
            .send(format!(
                "{{\"kind\":\"background_error\",\"severity\":\"error\",\"subsystem\":\"{}\",\"message\":\"{}\",\"trace_id\":\"{}\",\"timestamp\":{}}}",
                label.replace('"', "'"),
                err.replace('"', "'"),
                Uuid::new_v4(),
                Utc::now().timestamp_millis()
            ))
            .await;
    }
}

#[allow(dead_code)]
pub async fn maybe_fault<T>(
    result: MoatResult<T>,
    label: &str,
    status_tx: Option<Sender<String>>,
) -> MoatResult<T> {
    match result {
        Ok(value) => Ok(value),
        Err(err) => {
            report_background_fault(status_tx, label, &err.to_string()).await;
            Err(err)
        }
    }
}

fn clip_text(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let clipped: String = text.chars().take(max_chars).collect();
    if clipped.chars().count() == text.chars().count() {
        clipped
    } else {
        format!("{clipped}…")
    }
}
