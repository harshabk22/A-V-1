use active_win_pos_rs::get_active_window;
use arboard::Clipboard;
use enigo::{Enigo, Key, KeyboardControllable};
use regex::Regex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, RwLock as AsyncRwLock};
use tokio::time::timeout;
use url::Url;
use uuid::Uuid;

use crate::config;
use crate::consent::ConsentBridge;
use crate::external_api::ExternalApiConfig;
use crate::external_api::{missing_key_provider_from_error, provider_display_name};
use crate::hooks::run_precompute_memory_hook;
use crate::memory;
use crate::meta_brain::{MetaBrain, PermissionFlags, SubGraph};
use crate::sandbox::{
    cleanup_workspace_tmp_files, execute as execute_sandboxed, FileAccess, NetworkRule, PathRule,
    SandboxProfileBuilder, SandboxedCommand, WorkspaceCapability,
};
use crate::skills::{
    get_skill, install_skill, interpreter_for_language, language_extension, list_skills,
    SkillProposal, SynthesizedSkill,
};
use crate::swarm::SwarmGateway;
use crate::types::{IntentPayload, TypedIntent, WorkspacePath};

use crate::{MoatError, MoatResult};

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

// ─────────────────────────────────────────────────────────────────────────────
// § Configuration
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PlannerConfig {
    pub model: String,
    pub max_subtasks: usize,
    pub temperature: f32,
    pub max_tokens: usize,
}

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub model: String,
    pub command_timeout: Duration,
    pub allow_high_risk: bool,
    pub external_apis: ExternalApiConfig,
}

// ─────────────────────────────────────────────────────────────────────────────
// § ReAct Schema (Indestructible Payload)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionIntent {
    pub thought: Option<String>,
    pub action: String,
    pub target: String,
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct AskChoicePayload {
    context: String,
    options: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct LearnPayload {
    keywords: String,
    skill_description: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SubSpawnPayload {
    #[serde(alias = "target_agent_id")]
    agent_id: String,
    #[serde(alias = "task")]
    task_instruction: String,
    #[serde(default, alias = "role")]
    specialization: Option<String>,
}

/// Bridge trait for Visual-Semantic Driver UI interactions.
/// Implemented in the Tauri layer where AX accessibility APIs are available.
pub trait UiActionBridge: Send + Sync {
    /// Click an element by its scraped ID. Returns a success message or error.
    fn click(&self, element_id: usize) -> Result<String, String>;
    /// Type text into an element by its scraped ID. Returns a success message or error.
    fn type_text(&self, element_id: usize, text: &str) -> Result<String, String>;
    /// Re-scrape the active window's accessibility tree.
    /// Returns the flat text representation for LLM context injection,
    /// and updates the internal element map for subsequent click/type_text calls.
    fn refresh_ui_tree(&self) -> Result<String, String>;
}

/// Reversibility classification for the Barge-In system.
/// Determines what happens when a user interrupts mid-execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolReversibility {
    /// Safe to kill at any point. No side effects. (fs:read, memory:search, meta:query_brain)
    Reversible,
    /// Writes to a .tmp file first; can be rolled back by deleting the temp. (fs:write)
    Staged,
    /// Irreversible once started. Must complete. (os:click, os:type_text, app:launch, process:spawn)
    Committed,
}

impl ToolReversibility {
    /// Classify a tool action string into its reversibility tier.
    pub fn classify(action: &str) -> Self {
        match action {
            "fs:read" | "memory:search" | "session:search" | "memory:learn"
            | "meta:query_brain" | "user:ask_choice" | "done" => Self::Reversible,
            "fs:write" => Self::Staged,
            "app:launch"
            | "process:spawn"
            | "os:open_url"
            | "os:applescript"
            | "os:inject_text"
            | "os:click"
            | "os:type_text"
            | "net:curl"
            | "swarm:delegate"
            | "system:spawn_worker"
            | "propose_system_skill" => Self::Committed,
            _ => Self::Committed, // unknown tools are treated as committed for safety
        }
    }
}

#[derive(Debug, Clone)]
pub enum TaskStatus {
    Success {
        stdout: String,
    },
    /// Tool executed successfully but the semantic result is negative/empty.
    /// E.g., "No permissions found", "No relevant memories", empty search results.
    /// The orchestrator handles these mechanically — the LLM never decides routing.
    SoftDenial {
        reason: String,
    },
    Failed {
        exit_code: Option<i32>,
        stderr: String,
    },
    Blocked {
        blocked_by: usize,
    },
    /// Tool execution was aborted mid-flight by a barge-in signal.
    Cancelled {
        stage: String,
    },
}

#[derive(Debug, Clone)]
pub struct SubTaskResult {
    pub subtask_id: usize,
    pub action: String,
    pub target: String,
    pub status: TaskStatus,
    pub elapsed: Duration,
    pub tokens_used: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactContinuation {
    pub context_trace: Vec<LlmMessage>,
    pub turns: usize,
    pub searches: usize,
    #[serde(default)]
    pub session_searches: usize,
    pub total_tokens: u32,
    pub delegate_count: usize,
    pub high_risk_approvals: usize,
    pub loop_window: Vec<ToolExecution>,
    pub pending_tool_call: ActionIntent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum YieldStatus {
    AwaitingSubAgent {
        agent_id: String,
        task_instruction: String,
        original_tool_call: ActionIntent,
        continuation: ReactContinuation,
    },
}

#[derive(Debug, Clone)]
pub enum ReactOutcome {
    Completed(Vec<SubTaskResult>),
    Yielded(YieldStatus),
}

// ─────────────────────────────────────────────────────────────────────────────
// § Payload Coercion Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn value_to_string_vec(v: Option<&Value>) -> Result<Vec<String>, String> {
    match v {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::Array(arr)) => arr
            .iter()
            .enumerate()
            .map(|(i, el)| match el {
                Value::String(s) => Ok(s.clone()),
                other => Err(format!(
                    "process:spawn arg[{i}] must be a string, got {}",
                    other
                )),
            })
            .collect(),
        Some(Value::String(s)) => {
            eprintln!("[WARN] process:spawn payload is a string, not a JSON array. Whitespace-split is a lossy fallback.");
            Ok(s.split_whitespace().map(str::to_owned).collect())
        }
        Some(other) => Err(format!(
            "process:spawn payload must be a JSON array, got {}",
            other
        )),
    }
}

fn value_to_string(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn read_clipboard_backup(clipboard: &mut Clipboard) -> Result<ClipboardBackup, String> {
    if let Ok(files) = clipboard.get().file_list() {
        return Ok(ClipboardBackup::FileList(files));
    }
    if let Ok(image) = clipboard.get_image() {
        return Ok(ClipboardBackup::Image(image));
    }
    if let Ok(html) = clipboard.get().html() {
        return Ok(ClipboardBackup::Html(html));
    }
    if let Ok(text) = clipboard.get_text() {
        return Ok(ClipboardBackup::Text(text));
    }
    Ok(ClipboardBackup::Empty)
}

fn restore_clipboard_backup(
    clipboard: &mut Clipboard,
    backup: ClipboardBackup,
) -> Result<(), String> {
    match backup {
        ClipboardBackup::Text(text) => clipboard
            .set_text(text)
            .map_err(|err| format!("Clipboard restore failed: {err}")),
        ClipboardBackup::Html(html) => clipboard
            .set_html(html, None)
            .map_err(|err| format!("Clipboard restore failed: {err}")),
        ClipboardBackup::Image(image) => clipboard
            .set_image(image)
            .map_err(|err| format!("Clipboard restore failed: {err}")),
        ClipboardBackup::FileList(files) => clipboard
            .set()
            .file_list(&files)
            .map_err(|err| format!("Clipboard restore failed: {err}")),
        ClipboardBackup::Empty => clipboard
            .clear()
            .map_err(|err| format!("Clipboard clear failed: {err}")),
    }
}

fn extract_inject_text_payload(v: Option<&Value>) -> Option<(String, String)> {
    match v {
        Some(Value::Object(object)) => {
            let text = object
                .get("text_to_inject")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)?;
            let expected_app_name = object
                .get("expected_app_name")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)?;
            Some((text, expected_app_name))
        }
        _ => None,
    }
}

fn inject_text_via_clipboard(text_to_inject: &str) -> Result<(), String> {
    let mut clipboard =
        Clipboard::new().map_err(|err| format!("Clipboard access failed: {err}"))?;
    let backup = read_clipboard_backup(&mut clipboard)?;

    clipboard
        .set_text(text_to_inject.to_string())
        .map_err(|err| format!("Clipboard write failed: {err}"))?;

    let mut enigo = Enigo::new();
    #[cfg(target_os = "macos")]
    {
        enigo.key_down(Key::Meta);
        enigo.key_click(Key::Layout('v'));
        enigo.key_up(Key::Meta);
    }
    #[cfg(not(target_os = "macos"))]
    {
        enigo.key_down(Key::Control);
        enigo.key_click(Key::Layout('v'));
        enigo.key_up(Key::Control);
    }

    thread::sleep(Duration::from_millis(50));

    restore_clipboard_backup(&mut clipboard, backup)?;

    Ok(())
}

fn active_app_matches(expected_app_name: &str) -> Result<bool, String> {
    let window = get_active_window()
        .map_err(|_| "Failed to read the active OS window for injection safety.".to_string())?;
    Ok(window
        .app_name
        .trim()
        .eq_ignore_ascii_case(expected_app_name.trim()))
}

fn extract_curl_options(v: Option<&Value>) -> (String, String, Vec<(String, String)>) {
    let default_method = "GET".to_string();
    let default_body = String::new();
    let default_headers = vec![];
    let obj = match v {
        Some(Value::Object(m)) => m,
        _ => return (default_method, default_body, default_headers),
    };
    let method = obj
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_ascii_uppercase();
    let body = obj
        .get("body")
        .map(|b| value_to_string(Some(b)))
        .unwrap_or_default();
    let headers = match obj.get("headers") {
        Some(Value::Object(hmap)) => hmap
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
            .collect(),
        _ => vec![],
    };
    (method, body, headers)
}

fn extract_host(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    parsed.host_str().map(|h| h.to_ascii_lowercase())
}

// ─────────────────────────────────────────────────────────────────────────────
// § LLM Provider
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LlmMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<LlmToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LlmTool {
    #[serde(rename = "type")]
    pub type_: String,
    pub function: LlmFunctionDef,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LlmFunctionDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LlmToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub function: LlmFunctionCall,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LlmFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<LlmToolCall>>,
    pub tokens: u32,
}

pub trait LlmProvider: Send + Sync {
    fn chat_complete<'a>(
        &'a self,
        _model: &'a str,
        _messages: Vec<LlmMessage>,
        _tools: Option<Vec<LlmTool>>,
        _max_tokens: usize,
        _temperature: f32,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<LlmResponse, MoatError>> + Send + 'a>,
    > {
        Box::pin(async move {
            Err(MoatError::Llm(
                "chat_complete not implemented for this provider".into(),
            ))
        })
    }

    #[deprecated(note = "Migrating to strictly typed tool_calls via chat_complete")]
    fn complete<'a>(
        &'a self,
        model: &'a str,
        system: &'a str,
        user: &'a str,
        max_tokens: usize,
        temperature: f32,
    ) -> BoxFuture<'a, MoatResult<(String, u32)>>;

    #[deprecated(note = "Migrating to strictly typed tool_calls via chat_complete")]
    fn complete_stream<'a>(
        &'a self,
        model: &'a str,
        system: &'a str,
        user: &'a str,
        max_tokens: usize,
        temperature: f32,
        tx: Option<tokio::sync::mpsc::Sender<String>>,
    ) -> BoxFuture<'a, MoatResult<(String, u32)>> {
        Box::pin(async move {
            let res = self
                .complete(model, system, user, max_tokens, temperature)
                .await;
            if let Ok((text, _)) = &res {
                if let Some(channel) = tx {
                    let _ = channel.send(text.clone()).await;
                }
            }
            res
        })
    }

    fn check_health<'a>(&'a self) -> BoxFuture<'a, MoatResult<()>> {
        Box::pin(async { Ok(()) })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// § Planner (The Meta-Awareness Paradigm)
// ─────────────────────────────────────────────────────────────────────────────

pub const REACT_SYSTEM_TEMPLATE: &str = r#"SYSTEM: MoatAgeex — autonomous macOS agent.
Outputs are parsed by a Rust Object-Capability Hypervisor that executes <action> blocks, enforces permissions, and returns observations.

HARD LIMITS (not negotiable):
  max_turns=15 | memory_searches=10 | session_searches=6 | token_budget=24000 (compression at 70%)

CAPABILITIES: Only tools listed in ACTION SCHEMA below. No arbitrary shell, no full filesystem, no unlisted network domains.
Prior memory: Only <knowledge_memory>, <recalled_knowledge>, or session:search results.

OUTPUT FORMAT:
  Emit ONLY an <action> block. No preamble, no narration, no apologies.
  All reasoning goes in _thought (max 30 words inside the JSON).

KEY RULES:
  1. Direct Answer: If the answer is in memory/context, emit done immediately. No extra search.
  2. Fail-Fast: On CAPABILITY DENIED or PERMISSION DENIED, do NOT retry. Use done or user:ask_choice.
  3. Exit: Emit done on the VERY NEXT turn after you have the answer. Every extra turn wastes budget.
  4. meta:query_brain: Only for path/domain permission checks, never for tool existence.
  5. memory:search/session:search: Use when durable facts or prior sessions may have the answer.
  6. Messaging: Use os:open_url with deep links (whatsapp://, mailto:, facetime:). URL-encode all content.
  7. Ambiguous/destructive → user:ask_choice. Conversational → done with plain text.
  8. Never retry a failed concept more than twice.
  9. NEVER output "Session Summary", "Previous actions", or recap your history. The 'done' payload must be a direct, concise response to the user, like ChatGPT. No fluff.
"#;

#[derive(Clone)]
pub struct PlannerAgent {
    pub config: PlannerConfig,
    pub provider: Arc<dyn LlmProvider>,
    model_override: Arc<std::sync::RwLock<String>>,
}

impl PlannerAgent {
    pub fn new(config: PlannerConfig, provider: Arc<dyn LlmProvider>) -> Self {
        let model_override = Arc::new(std::sync::RwLock::new(config.model.clone()));
        Self {
            config,
            provider,
            model_override,
        }
    }

    pub fn set_model(&self, model: impl Into<String>) {
        if let Ok(mut slot) = self.model_override.write() {
            *slot = model.into();
        }
    }

    pub fn current_model(&self) -> String {
        self.model_override
            .read()
            .map(|slot| slot.clone())
            .unwrap_or_else(|_| self.config.model.clone())
    }

    pub async fn generate_intent_raw(
        &self,
        rolling_context: &str,
        tx: Option<tokio::sync::mpsc::Sender<String>>,
    ) -> MoatResult<(String, u32)> {
        self.generate_intent_raw_with_system(REACT_SYSTEM_TEMPLATE, rolling_context, tx)
            .await
    }

    pub async fn generate_intent_raw_with_system(
        &self,
        system_prompt: &str,
        rolling_context: &str,
        tx: Option<tokio::sync::mpsc::Sender<String>>,
    ) -> MoatResult<(String, u32)> {
        let model = self.current_model();
        timeout(
            PLANNER_REPLY_TIMEOUT,
            self.provider.complete_stream(
                &model,
                system_prompt,
                rolling_context,
                self.config.max_tokens,
                self.config.temperature,
                tx,
            ),
        )
        .await
        .map_err(|_| {
            MoatError::Orchestrator(format!(
                "planner provider timed out after {}s",
                PLANNER_REPLY_TIMEOUT.as_secs()
            ))
        })?
    }

    pub async fn generate_intent_tools(
        &self,
        messages: Vec<LlmMessage>,
        tools: Option<Vec<LlmTool>>,
    ) -> MoatResult<LlmResponse> {
        let model = self.current_model();
        timeout(
            PLANNER_REPLY_TIMEOUT,
            self.provider.chat_complete(
                &model,
                messages,
                tools,
                self.config.max_tokens,
                self.config.temperature,
            ),
        )
        .await
        .map_err(|_| {
            MoatError::Orchestrator(format!(
                "planner provider timed out after {}s",
                PLANNER_REPLY_TIMEOUT.as_secs()
            ))
        })?
    }

    pub fn build_llm_tools(&self) -> Vec<LlmTool> {
        let tools = vec![
            (
                "fs:read",
                "Read file contents",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"string"},"_thought":{"type":"string"}},"required":["target","_thought"]}),
            ),
            (
                "fs:write",
                "Write file contents",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"string"},"_thought":{"type":"string"}},"required":["target","payload","_thought"]}),
            ),
            (
                "app:launch",
                "Launch application",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"string"},"_thought":{"type":"string"}},"required":["target","_thought"]}),
            ),
            (
                "process:spawn",
                "Spawn background process",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"array","items":{"type":"string"}},"_thought":{"type":"string"}},"required":["target","payload","_thought"]}),
            ),
            (
                "os:open_url",
                "Open a URL",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"string"},"_thought":{"type":"string"}},"required":["target","_thought"]}),
            ),
            (
                "os:applescript",
                "Run AppleScript",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"string"},"_thought":{"type":"string"}},"required":["payload","_thought"]}),
            ),
            (
                "os:inject_text",
                "Inject text into active app",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"object","properties":{"text_to_inject":{"type":"string"},"expected_app_name":{"type":"string"}},"required":["text_to_inject","expected_app_name"]},"_thought":{"type":"string"}},"required":["payload","_thought"]}),
            ),
            (
                "os:click",
                "Click UI element",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"object","properties":{"element_id":{"type":"number"}},"required":["element_id"]},"_thought":{"type":"string"}},"required":["payload","_thought"]}),
            ),
            (
                "os:type_text",
                "Type text into UI element",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"object","properties":{"element_id":{"type":"number"},"text":{"type":"string"}},"required":["element_id","text"]},"_thought":{"type":"string"}},"required":["payload","_thought"]}),
            ),
            (
                "net:curl",
                "Perform HTTP request",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"object","properties":{"method":{"type":"string"},"body":{"type":"string"},"headers":{"type":"object"}}},"_thought":{"type":"string"}},"required":["target","payload","_thought"]}),
            ),
            (
                "memory:search",
                "Search durable knowledge",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"string"},"_thought":{"type":"string"}},"required":["payload","_thought"]}),
            ),
            (
                "session:search",
                "Search previous sessions",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"string"},"_thought":{"type":"string"}},"required":["payload","_thought"]}),
            ),
            (
                "memory:learn",
                "Store durable knowledge",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"object","properties":{"keywords":{"type":"string"},"skill_description":{"type":"string"}},"required":["keywords","skill_description"]},"_thought":{"type":"string"}},"required":["payload","_thought"]}),
            ),
            (
                "meta:query_brain",
                "Check permissions",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"string"},"_thought":{"type":"string"}},"required":["payload","_thought"]}),
            ),
            (
                "system:spawn_worker",
                "Delegate task to sub-agent",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"object","properties":{"target_agent_id":{"type":"string"},"task":{"type":"string"},"specialization":{"type":"string"}}},"_thought":{"type":"string"}},"required":["payload","_thought"]}),
            ),
            (
                "propose_system_skill",
                "Propose a new system skill",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"object","properties":{"skill_name":{"type":"string"},"description":{"type":"string"},"source_code":{"type":"string"},"language":{"type":"string"},"parameters":{"type":"object"}},"required":["skill_name","description","source_code","language"]},"_thought":{"type":"string"}},"required":["payload","_thought"]}),
            ),
            (
                "user:ask_choice",
                "Ask user for choice",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"object","properties":{"context":{"type":"string"},"options":{"type":"array","items":{"type":"string"}}},"required":["context","options"]},"_thought":{"type":"string"}},"required":["payload","_thought"]}),
            ),
            (
                "done",
                "Finish task",
                serde_json::json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"string"},"_thought":{"type":"string"}},"required":["payload","_thought"]}),
            ),
        ];

        tools
            .into_iter()
            .map(|(name, desc, params)| LlmTool {
                type_: "function".into(),
                function: LlmFunctionDef {
                    name: name.into(),
                    description: Some(desc.into()),
                    parameters: params,
                },
            })
            .collect()
    }

    pub fn parse_intent(raw: &str) -> ActionIntent {
        serde_json::from_str(raw).unwrap_or_else(|_| Self::safe_retry_intent())
    }

    fn safe_retry_intent() -> ActionIntent {
        ActionIntent {
            thought: None,
            action: "system:parse_error".to_string(),
            target: "parser".to_string(),
            payload: Some(Value::String(
                "[SYSTEM: Your last response could not be parsed. You MUST respond with a valid <action> JSON block. Try again.]".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone)]
enum PermissionGrant {
    Path {
        path: String,
        permissions: PermissionFlags,
    },
    App {
        app: String,
        permissions: PermissionFlags,
    },
    UrlScheme {
        scheme: String,
        permissions: PermissionFlags,
    },
    Tool {
        tool: String,
        permissions: PermissionFlags,
    },
}

impl PermissionGrant {
    fn description(&self) -> (String, String) {
        match self {
            Self::Path { path, permissions } => (
                "permission:file".into(),
                format!(
                    "Grant {} access to {}",
                    permission_flags_label(*permissions),
                    path
                ),
            ),
            Self::App { app, permissions } => (
                "permission:app".into(),
                format!(
                    "Grant {} access to app {}",
                    permission_flags_label(*permissions),
                    app
                ),
            ),
            Self::UrlScheme {
                scheme,
                permissions,
            } => (
                "permission:url_scheme".into(),
                format!(
                    "Grant {} access to URL scheme {}",
                    permission_flags_label(*permissions),
                    scheme
                ),
            ),
            Self::Tool { tool, permissions } => (
                "permission:tool".into(),
                format!(
                    "Grant {} access to tool {}",
                    permission_flags_label(*permissions),
                    tool
                ),
            ),
        }
    }

    fn apply(&self, brain: &mut MetaBrain) -> MoatResult<()> {
        match self {
            Self::Path { path, permissions } => {
                brain.grant_path_permission(path, *permissions)?;
            }
            Self::App { app, permissions } => {
                brain.grant_app_permission(app, *permissions)?;
            }
            Self::UrlScheme {
                scheme,
                permissions,
            } => {
                brain.grant_url_scheme_permission(scheme, *permissions)?;
            }
            Self::Tool { tool, permissions } => {
                brain.grant_tool_permission(tool, *permissions)?;
            }
        }
        Ok(())
    }
}

fn permission_flags_label(flags: PermissionFlags) -> String {
    let mut labels = Vec::new();
    if flags.contains(PermissionFlags::FILE_READ) {
        labels.push("read");
    }
    if flags.contains(PermissionFlags::FILE_WRITE) {
        labels.push("write");
    }
    if flags.contains(PermissionFlags::FILE_DELETE) {
        labels.push("delete");
    }
    if flags.contains(PermissionFlags::NET_OUTBOUND) {
        labels.push("network");
    }
    if flags.contains(PermissionFlags::APP_LAUNCH) {
        labels.push("app-launch");
    }
    if flags.contains(PermissionFlags::URL_OPEN) {
        labels.push("url-open");
    }
    if flags.contains(PermissionFlags::TOOL_EXEC) || flags.contains(PermissionFlags::EXEC) {
        labels.push("execute");
    }
    if labels.is_empty() {
        "no".into()
    } else {
        labels.join("+")
    }
}

fn canonicalize_root(path: &str) -> Result<PathBuf, String> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(format!(
            "CAPABILITY DENIED: root path must be absolute: {path}"
        ));
    }
    fs::canonicalize(p).map_err(|e| format!("CAPABILITY DENIED: invalid root '{path}': {}", e))
}

fn resolve_workspace_target(target: &str) -> Result<(WorkspacePath, PathBuf), String> {
    let workspace = config::workspace_root();
    let rel = WorkspacePath::from_target(target).map_err(|err| err.to_string())?;
    Ok((rel.clone(), workspace.join(rel.as_path())))
}

fn authorize_path(
    target: &str,
    allowed_roots: &[String],
    for_write: bool,
) -> Result<PathBuf, String> {
    if allowed_roots.is_empty() {
        return Err("CAPABILITY DENIED: no allowed paths configured.".into());
    }

    if target.contains('\0') || target.contains('\n') || target.contains('\r') {
        return Err("CAPABILITY DENIED: path contains control characters".into());
    }

    let target_path = Path::new(target);
    if !target_path.is_absolute() {
        return Err(format!(
            "CAPABILITY DENIED: path must be absolute: {}",
            target
        ));
    }

    let canonical_roots: Vec<PathBuf> = allowed_roots
        .iter()
        .map(|r| canonicalize_root(r))
        .collect::<Result<_, _>>()?;

    let candidate = if for_write {
        if target_path.exists() {
            fs::canonicalize(target_path).map_err(|e| {
                format!("CAPABILITY DENIED: cannot canonicalize '{}': {}", target, e)
            })?
        } else {
            let parent = target_path.parent().ok_or_else(|| {
                format!(
                    "CAPABILITY DENIED: write target '{}' has no parent directory",
                    target
                )
            })?;
            let canonical_parent = fs::canonicalize(parent).map_err(|e| {
                format!(
                    "CAPABILITY DENIED: write parent '{}' does not exist or is invalid: {}",
                    parent.display(),
                    e
                )
            })?;
            let name = target_path.file_name().ok_or_else(|| {
                format!(
                    "CAPABILITY DENIED: write target '{}' must end with a file or directory name",
                    target
                )
            })?;
            canonical_parent.join(name)
        }
    } else {
        fs::canonicalize(target_path)
            .map_err(|e| format!("CAPABILITY DENIED: cannot canonicalize '{}': {}", target, e))?
    };

    if canonical_roots
        .iter()
        .any(|root| candidate.starts_with(root))
    {
        Ok(candidate)
    } else {
        Err(format!(
            "CAPABILITY DENIED: '{}' is outside configured allowed paths",
            target
        ))
    }
}

fn subgraph_path_rules(subgraph: &SubGraph) -> Vec<PathRule> {
    subgraph
        .allowed_paths
        .iter()
        .map(|(path, access)| PathRule {
            path: path.clone(),
            recursive: true,
            access: *access,
        })
        .collect()
}

fn subgraph_network_rules(subgraph: &SubGraph) -> Vec<NetworkRule> {
    subgraph
        .allowed_domains
        .iter()
        .map(|domain| NetworkRule {
            host: domain.clone(),
            port: None,
        })
        .collect()
}

fn run_command(cmd: &mut Command, timeout: Duration) -> MoatResult<TaskStatus> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        MoatError::Orchestrator(format!("Command execution failed to start: {}", e))
    })?;

    let Some(mut stdout_pipe) = child.stdout.take() else {
        return Err(MoatError::Orchestrator(
            "Command stdout pipe was not available".into(),
        ));
    };
    let Some(mut stderr_pipe) = child.stderr.take() else {
        return Err(MoatError::Orchestrator(
            "Command stderr pipe was not available".into(),
        ));
    };

    let out_thread = thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout_pipe.read_to_string(&mut s);
        s
    });
    let err_thread = thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr_pipe.read_to_string(&mut s);
        s
    });

    let start = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| MoatError::Orchestrator(format!("Command wait failed: {}", e)))?
        {
            break status;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: format!("Command timed out after {:?}", timeout),
            });
        }
        thread::sleep(Duration::from_millis(10));
    };

    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
    if status.success() {
        Ok(TaskStatus::Success { stdout })
    } else {
        Ok(TaskStatus::Failed {
            exit_code: status.code(),
            stderr,
        })
    }
}

fn sandbox_status(
    binary: &str,
    args: &[String],
    subgraph: &SubGraph,
    timeout: Duration,
) -> MoatResult<TaskStatus> {
    sandbox_status_with_extra_paths(binary, args, subgraph, Vec::new(), timeout)
}

fn sandbox_status_with_extra_paths(
    binary: &str,
    args: &[String],
    subgraph: &SubGraph,
    extra_path_rules: Vec<PathRule>,
    timeout: Duration,
) -> MoatResult<TaskStatus> {
    let profile = SandboxProfileBuilder::new()
        .add_path_rules(subgraph_path_rules(subgraph))
        .add_path_rules(extra_path_rules)
        .add_network_rules(subgraph_network_rules(subgraph))
        .build()
        .map_err(|e| {
            MoatError::Sandbox(crate::sandbox::SandboxError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                e.to_string(),
            )))
        })?;

    match execute_sandboxed(&SandboxedCommand {
        binary,
        args,
        profile: &profile,
        timeout: Some(timeout),
    }) {
        Ok(output) => Ok(TaskStatus::Success {
            stdout: output.stdout,
        }),
        Err(err) => Ok(TaskStatus::Failed {
            exit_code: None,
            stderr: err.to_string(),
        }),
    }
}

fn expected_value_type(schema_value: &Value) -> &'static str {
    match schema_value {
        Value::String(kind) => match kind.trim().to_ascii_lowercase().as_str() {
            "string" => "string",
            "number" => "number",
            "integer" => "integer",
            "boolean" => "boolean",
            "array" => "array",
            "object" => "object",
            "any" => "any",
            _ => "unknown",
        },
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Null => "any",
    }
}

fn permission_guidance_denial(target: &str) -> String {
    format!(
        "CAPABILITY DENIED: access to '{}' is forbidden. Use 'meta:query_brain' to check your current allowed permissions, or use 'user:ask_choice' to ask the human to grant you access.",
        target
    )
}

fn is_blocked_process_spawn_binary(binary: &str) -> bool {
    matches!(
        binary.trim().to_ascii_lowercase().as_str(),
        "bash" | "sh" | "zsh" | "python" | "python3" | "ruby" | "perl" | "node"
    )
}

fn actual_value_type(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Number(number) if number.as_i64().is_some() || number.as_u64().is_some() => {
            "integer"
        }
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Null => "null",
    }
}

fn value_matches_schema(value: &Value, schema_value: &Value) -> bool {
    match expected_value_type(schema_value) {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "any" => true,
        "unknown" => true,
        _ => true,
    }
}

fn schema_value_for_error(schema_value: &Value) -> String {
    match schema_value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn payload_value_to_cli_arg(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

fn build_synthesized_skill_args(
    payload: Option<&Value>,
    schema_json: &str,
) -> Result<Vec<String>, String> {
    let schema: Value = serde_json::from_str(schema_json)
        .map_err(|err| format!("invalid installed parameter schema: {err}"))?;
    let schema_object = schema
        .as_object()
        .ok_or_else(|| "installed parameter schema must be a JSON object".to_string())?;
    let payload_object = payload
        .and_then(Value::as_object)
        .ok_or_else(|| "synthesized skill payload must be a JSON object".to_string())?;

    for required_key in schema_object.keys() {
        if !payload_object.contains_key(required_key) {
            return Err(format!(
                "missing required skill parameter '{}'",
                required_key
            ));
        }
    }
    for key in payload_object.keys() {
        if !schema_object.contains_key(key) {
            return Err(format!("unexpected skill parameter '{}'", key));
        }
    }

    let mut ordered_keys = schema_object.keys().cloned().collect::<Vec<_>>();
    ordered_keys.sort();

    let mut args = Vec::with_capacity(ordered_keys.len() * 2);
    for key in ordered_keys {
        let schema_value = schema_object
            .get(&key)
            .ok_or_else(|| format!("missing installed schema for '{}'", key))?;
        let value = payload_object
            .get(&key)
            .ok_or_else(|| format!("missing payload value for '{}'", key))?;
        if !value_matches_schema(value, schema_value) {
            return Err(format!(
                "parameter '{}' expected {}, got {}",
                key,
                schema_value_for_error(schema_value),
                actual_value_type(value)
            ));
        }
        args.push(format!("--{}", key.replace('_', "-")));
        args.push(payload_value_to_cli_arg(value));
    }
    Ok(args)
}

struct TempWorkspaceSkillScript {
    absolute_path: PathBuf,
}

impl Drop for TempWorkspaceSkillScript {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.absolute_path);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// § Worker Agent
// ─────────────────────────────────────────────────────────────────────────────

pub struct WorkerAgent {
    pub config: WorkerConfig,
    pub meta_brain: Arc<AsyncRwLock<MetaBrain>>,
    pub db: Arc<AsyncMutex<Connection>>,
    pub consent_bridge: Option<Arc<ConsentBridge>>,
    pub ui_bridge: Option<Arc<dyn UiActionBridge>>,
}

impl WorkerAgent {
    pub fn new(
        config: WorkerConfig,
        meta_brain: Arc<AsyncRwLock<MetaBrain>>,
        db: Arc<AsyncMutex<Connection>>,
    ) -> Self {
        Self {
            config,
            meta_brain,
            db,
            consent_bridge: None,
            ui_bridge: None,
        }
    }

    pub fn with_consent_bridge(
        config: WorkerConfig,
        meta_brain: Arc<AsyncRwLock<MetaBrain>>,
        db: Arc<AsyncMutex<Connection>>,
        bridge: Arc<ConsentBridge>,
    ) -> Self {
        Self {
            config,
            meta_brain,
            db,
            consent_bridge: Some(bridge),
            ui_bridge: None,
        }
    }

    pub fn with_ui_bridge(mut self, bridge: Arc<dyn UiActionBridge>) -> Self {
        self.ui_bridge = Some(bridge);
        self
    }

    fn curl_max_response_bytes(&self) -> usize {
        std::env::var("MOAT_CURL_MAX_RESPONSE_BYTES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(512 * 1024)
    }

    fn curl_allowed_methods(&self) -> Vec<String> {
        let configured = std::env::var("MOAT_ALLOWED_CURL_METHODS")
            .ok()
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|method| !method.is_empty())
                    .map(|method| method.to_ascii_uppercase())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if configured.is_empty() {
            vec!["GET".into(), "POST".into()]
        } else {
            configured
        }
    }

    async fn search_session_transcripts(&self, query: &str, limit: usize) -> MoatResult<String> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok("session:search requires a non-empty query.".to_string());
        }

        let fts_query = format!("\"{}\"", trimmed.replace('"', " "));
        let db = self.db.lock().await;
        let mut stmt = db
            .prepare(
                "SELECT
                    gs.source,
                    gs.channel,
                    gs.title,
                    snippet(gateway_messages_fts, 0, '[', ']', ' ... ', 18),
                    gm.created_at
                 FROM gateway_messages_fts
                 JOIN gateway_messages gm ON gm.id = gateway_messages_fts.rowid
                 JOIN gateway_sessions gs ON gs.session_id = gm.session_id
                 WHERE gateway_messages_fts MATCH ?1
                 ORDER BY bm25(gateway_messages_fts), gm.created_at DESC
                 LIMIT ?2",
            )
            .map_err(MoatError::Database)?;
        let rows = stmt
            .query_map(params![fts_query, limit.max(1) as i64], |row| {
                let source: String = row.get(0)?;
                let channel: String = row.get(1)?;
                let title: String = row.get(2)?;
                let snippet: String = row.get(3)?;
                let timestamp: i64 = row.get(4)?;
                Ok((source, channel, title, snippet, timestamp))
            })
            .map_err(MoatError::Database)?;

        let mut lines = Vec::new();
        for row in rows {
            let (source, channel, title, snippet, timestamp) = row.map_err(MoatError::Database)?;
            let channel_label = if channel.trim().is_empty() {
                "default".to_string()
            } else {
                channel
            };
            lines.push(format!(
                "[{}:{} @ {}] {}\n{}",
                source.trim(),
                channel_label.trim(),
                timestamp,
                title.trim(),
                snippet.trim()
            ));
        }

        if lines.is_empty() {
            Ok("No relevant session transcripts found.".to_string())
        } else {
            Ok(lines.join("\n---\n"))
        }
    }

    async fn request_permission(&self, grant: PermissionGrant) -> MoatResult<bool> {
        let Some(bridge) = &self.consent_bridge else {
            return Ok(false);
        };

        let (action, payload) = grant.description();
        let approved = bridge.request_consent(&action, &payload).await;
        if !approved {
            return Ok(false);
        }

        {
            let mut brain = self.meta_brain.write().await;
            grant.apply(&mut brain)?;
        }
        MetaBrain::flush_arc(Arc::clone(&self.meta_brain)).await?;
        Ok(true)
    }

    fn high_risk_quota_exceeded(approvals: &Arc<std::sync::atomic::AtomicUsize>) -> bool {
        approvals.load(std::sync::atomic::Ordering::SeqCst) >= 3
    }

    fn record_high_risk_approval(approvals: &Arc<std::sync::atomic::AtomicUsize>) -> usize {
        approvals.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
    }

    async fn request_action_consent(
        &self,
        intent: &ActionIntent,
        session_id: &str,
        agent_label: &str,
        high_risk_approvals: Arc<std::sync::atomic::AtomicUsize>,
    ) -> MoatResult<bool> {
        let Some(bridge) = &self.consent_bridge else {
            return Ok(false);
        };

        if Self::high_risk_quota_exceeded(&high_risk_approvals) {
            return Err(MoatError::Orchestrator(
                "High-risk approval cap exceeded for this session. Request auto-denied.".into(),
            ));
        }

        let approved = bridge
            .request_action_consent(intent, agent_label, session_id)
            .await?;
        if !approved {
            return Ok(false);
        }

        let _count = Self::record_high_risk_approval(&high_risk_approvals);
        Ok(true)
    }

    fn permission_roots_for(
        paths: &[(String, FileAccess)],
        permission: PermissionFlags,
    ) -> Vec<String> {
        paths
            .iter()
            .filter_map(|(path, access)| match (permission, access) {
                (PermissionFlags::FILE_READ, FileAccess::ReadOnly)
                | (PermissionFlags::FILE_READ, FileAccess::ReadWrite)
                | (PermissionFlags::FILE_WRITE, FileAccess::WriteOnly)
                | (PermissionFlags::FILE_WRITE, FileAccess::ReadWrite)
                | (PermissionFlags::FILE_DELETE, FileAccess::WriteOnly)
                | (PermissionFlags::FILE_DELETE, FileAccess::ReadWrite) => Some(path.clone()),
                _ => None,
            })
            .collect()
    }

    fn write_grant_scope(target: &str) -> String {
        let path = Path::new(target);
        if path.is_dir() {
            target.to_string()
        } else {
            path.parent().unwrap_or(path).to_string_lossy().into_owned()
        }
    }

    async fn authorize_path_with_permission(
        &self,
        target: &str,
        permission: PermissionFlags,
    ) -> Result<PathBuf, String> {
        let roots = {
            let brain = self.meta_brain.read().await;
            if !brain.path_has_permission(target, permission) {
                return Err(permission_guidance_denial(target));
            }
            let snapshot = brain.permission_snapshot();
            Self::permission_roots_for(&snapshot.paths, permission)
        };
        authorize_path(
            target,
            &roots,
            permission.contains(PermissionFlags::FILE_WRITE)
                || permission.contains(PermissionFlags::FILE_DELETE),
        )
    }

    async fn ensure_path_permission(
        &self,
        target: &str,
        permission: PermissionFlags,
    ) -> Result<PathBuf, String> {
        match self
            .authorize_path_with_permission(target, permission)
            .await
        {
            Ok(path) => Ok(path),
            Err(reason) => {
                let grant_path = if permission.contains(PermissionFlags::FILE_WRITE)
                    || permission.contains(PermissionFlags::FILE_DELETE)
                {
                    Self::write_grant_scope(target)
                } else {
                    target.to_string()
                };
                match self
                    .request_permission(PermissionGrant::Path {
                        path: grant_path,
                        permissions: permission,
                    })
                    .await
                {
                    Ok(true) => {
                        self.authorize_path_with_permission(target, permission)
                            .await
                    }
                    Ok(false) => Err(reason),
                    Err(err) => Err(err.to_string()),
                }
            }
        }
    }

    async fn ensure_app_permission(&self, app_name: &str) -> Result<(), String> {
        let has_permission = {
            let brain = self.meta_brain.read().await;
            brain.app_has_permission(app_name, PermissionFlags::APP_LAUNCH)
        };
        if has_permission {
            return Ok(());
        }

        match self
            .request_permission(PermissionGrant::App {
                app: app_name.to_string(),
                permissions: PermissionFlags::APP_LAUNCH,
            })
            .await
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!("CAPABILITY DENIED: app '{}' not allowed", app_name)),
            Err(err) => Err(err.to_string()),
        }
    }

    async fn ensure_tool_permission(&self, tool: &str) -> Result<(), String> {
        if tool.contains('/') || tool.contains('\\') {
            return Err(permission_guidance_denial(tool));
        }

        let has_permission = {
            let brain = self.meta_brain.read().await;
            let check = |candidate: &str| {
                brain.tool_has_permission(
                    candidate,
                    PermissionFlags::TOOL_EXEC | PermissionFlags::EXEC,
                ) || brain.tool_has_permission(candidate, PermissionFlags::TOOL_EXEC)
            };
            check(tool)
                || (tool == "os:applescript" && (check("osascript") || check("os:applescript")))
        };
        if has_permission {
            return Ok(());
        }

        if matches!(tool, "os:inject_text" | "os:applescript") {
            return Err(permission_guidance_denial(tool));
        }

        match self
            .request_permission(PermissionGrant::Tool {
                tool: tool.to_string(),
                permissions: PermissionFlags::TOOL_EXEC | PermissionFlags::EXEC,
            })
            .await
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(permission_guidance_denial(tool)),
            Err(err) => Err(err.to_string()),
        }
    }

    async fn ensure_url_scheme_permission(&self, scheme: &str) -> Result<(), String> {
        let has_permission = {
            let brain = self.meta_brain.read().await;
            brain.url_scheme_has_permission(scheme, PermissionFlags::URL_OPEN)
        };
        if has_permission {
            return Ok(());
        }

        match self
            .request_permission(PermissionGrant::UrlScheme {
                scheme: scheme.to_string(),
                permissions: PermissionFlags::URL_OPEN,
            })
            .await
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!(
                "CAPABILITY DENIED: URL scheme '{}' not allowed",
                scheme
            )),
            Err(err) => Err(err.to_string()),
        }
    }

    async fn relevant_subgraph(
        &self,
        root_node_id: u64,
        intent: &ActionIntent,
    ) -> MoatResult<SubGraph> {
        let query = format!(
            "{} {} {}",
            intent.action,
            intent.target,
            value_to_string(intent.payload.as_ref())
        );
        let brain = self.meta_brain.read().await;
        brain.focused_subgraph(root_node_id, &query, 5, 4)
    }

    async fn handle_skill_proposal(
        &self,
        intent: &ActionIntent,
        session_id: &str,
        agent_label: &str,
    ) -> MoatResult<TaskStatus> {
        let typed = TypedIntent::from_action_intent(intent)?;
        let IntentPayload::ProposeSkill {
            skill_name,
            description,
            source_code,
            language,
            parameter_schema_json,
        } = typed.payload
        else {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: "propose_system_skill payload is invalid".into(),
            });
        };

        let Some(bridge) = &self.consent_bridge else {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: "Skill installation requires an active consent bridge.".into(),
            });
        };

        let proposal = SkillProposal {
            skill_name,
            description,
            source_code,
            language,
            parameter_schema: parameter_schema_json,
        };
        let approved = bridge
            .request_skill_review(session_id, agent_label, &proposal)
            .await?;
        if !approved {
            return Ok(TaskStatus::Success {
                stdout: "[SYSTEM: Skill proposal rejected by user]".into(),
            });
        }

        let installed = {
            let db = self.db.lock().await;
            install_skill(&db, &proposal)?
        };

        Ok(TaskStatus::Success {
            stdout: format!(
                "Installed synthesized skill '{}' ({}) with parameter schema {}",
                installed.skill_name, installed.language, installed.parameter_schema
            ),
        })
    }

    fn write_temp_workspace_skill_script(
        &self,
        skill: &SynthesizedSkill,
        source_code: &str,
    ) -> MoatResult<TempWorkspaceSkillScript> {
        let capability = WorkspaceCapability::open(&config::workspace_root())?;
        let relative_path = PathBuf::from(format!(
            ".moat_run_skills/{}_{}_{}.{}",
            skill.skill_name,
            &skill.code_hash[..12.min(skill.code_hash.len())],
            Uuid::new_v4(),
            language_extension(&skill.language)
        ));
        capability.atomic_write(relative_path.as_path(), source_code.as_bytes())?;
        Ok(TempWorkspaceSkillScript {
            absolute_path: config::workspace_root().join(relative_path),
        })
    }

    async fn action_synthesized_skill(
        &self,
        intent: &ActionIntent,
        root_node_id: u64,
    ) -> MoatResult<Option<TaskStatus>> {
        let skill = {
            let db = self.db.lock().await;
            get_skill(&db, &intent.action)?
        };
        let Some(skill) = skill else {
            return Ok(None);
        };

        let args =
            match build_synthesized_skill_args(intent.payload.as_ref(), &skill.parameter_schema) {
                Ok(args) => args,
                Err(err) => {
                    return Ok(Some(TaskStatus::Failed {
                        exit_code: None,
                        stderr: err,
                    }))
                }
            };

        let interpreter = interpreter_for_language(&skill.language);
        if let Err(reason) = self.ensure_tool_permission(interpreter).await {
            return Ok(Some(TaskStatus::Failed {
                exit_code: None,
                stderr: reason,
            }));
        }

        let source_code = match fs::read_to_string(&skill.script_path) {
            Ok(source) => source,
            Err(err) => {
                return Ok(Some(TaskStatus::Failed {
                    exit_code: None,
                    stderr: format!(
                        "failed to read installed skill '{}' from vault: {}",
                        skill.skill_name, err
                    ),
                }))
            }
        };

        let temp_script = match self.write_temp_workspace_skill_script(&skill, &source_code) {
            Ok(script) => script,
            Err(err) => {
                return Ok(Some(TaskStatus::Failed {
                    exit_code: None,
                    stderr: format!("failed to stage skill in workspace: {err}"),
                }))
            }
        };

        let mut exec_args = vec![temp_script.absolute_path.to_string_lossy().into_owned()];
        exec_args.extend(args);

        let subgraph = self.relevant_subgraph(root_node_id, intent).await?;
        let status = sandbox_status_with_extra_paths(
            interpreter,
            &exec_args,
            &subgraph,
            vec![PathRule::read_write_recursive(
                config::workspace_root().to_string_lossy().into_owned(),
            )],
            self.config.command_timeout,
        )?;

        drop(temp_script);
        Ok(Some(status))
    }

    pub async fn execute_capability_task(
        &self,
        intent: &ActionIntent,
        root_node_id: u64,
        session_id: &str,
        agent_label: &str,
        high_risk_approvals: Arc<std::sync::atomic::AtomicUsize>,
        execution_world_state: Option<Arc<tokio::sync::RwLock<crate::system::ExecutionWorldState>>>,
    ) -> MoatResult<TaskStatus> {
        if let Some(ref state_lock) = execution_world_state {
            let state = state_lock.read().await;
            if state.is_capability_denied(&intent.action, &intent.target) {
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: "[SYSTEM SHORT-CIRCUIT] This capability was already denied in this session. You MUST use 'done' or 'user:ask_choice'. Do not retry.".into(),
                });
            }
        }

        if let Err(err) = TypedIntent::from_action_intent(intent) {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: format!("Aegis rejected action payload: {err}"),
            });
        }

        if intent.action == "propose_system_skill" {
            return self
                .handle_skill_proposal(intent, session_id, agent_label)
                .await;
        }

        if let Some(status) = self.action_synthesized_skill(intent, root_node_id).await? {
            return Ok(status);
        }

        match intent.action.as_str() {
            "fs:read" => self.action_fs_read(intent).await,
            "fs:write" => self.action_fs_write(intent).await,
            "fs:delete" => self.action_fs_delete(intent).await,
            "app:launch" => self.action_app_launch(intent).await,
            "process:spawn" => self.action_process_spawn(intent, root_node_id).await,
            "os:open_url" => self.action_os_open_url(intent).await,
            "os:applescript" => {
                self.action_os_applescript(
                    intent,
                    session_id,
                    agent_label,
                    Arc::clone(&high_risk_approvals),
                )
                .await
            }
            "os:inject_text" => {
                self.action_os_inject_text(
                    intent,
                    session_id,
                    agent_label,
                    Arc::clone(&high_risk_approvals),
                )
                .await
            }
            "net:curl" => {
                self.action_net_curl(
                    intent,
                    root_node_id,
                    session_id,
                    agent_label,
                    high_risk_approvals,
                )
                .await
            }
            "meta:query_brain" => self.action_meta_query_brain(intent).await,
            "session:search" => self.action_session_search(intent).await,
            "os:click" => {
                self.action_os_click(
                    intent,
                    session_id,
                    agent_label,
                    Arc::clone(&high_risk_approvals),
                )
                .await
            }
            "os:type_text" => {
                self.action_os_type_text(
                    intent,
                    session_id,
                    agent_label,
                    Arc::clone(&high_risk_approvals),
                )
                .await
            }
            other => Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: format!("unsupported action: {other}"),
            }),
        }
    }

    async fn action_meta_query_brain(&self, intent: &ActionIntent) -> MoatResult<TaskStatus> {
        let keyword = value_to_string(intent.payload.as_ref());
        let keyword = if keyword.trim().is_empty() {
            intent.target.trim()
        } else {
            keyword.trim()
        };

        if keyword.is_empty() || keyword.eq_ignore_ascii_case("metabrain") {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: "meta:query_brain requires a non-empty keyword payload".into(),
            });
        }

        let observation = {
            let brain = self.meta_brain.read().await;
            brain.extract_relevant_subgraph(keyword)
        };

        let stdout = if observation.starts_with("No permissions found") {
            format!("{}\nNote: You can still attempt the action. If permission is needed, the system will pause and ask the user for consent automatically.", observation)
        } else {
            observation
        };

        Ok(TaskStatus::Success { stdout })
    }

    async fn action_session_search(&self, intent: &ActionIntent) -> MoatResult<TaskStatus> {
        let query = value_to_string(intent.payload.as_ref());
        let observation = self.search_session_transcripts(&query, 5).await?;
        if observation.starts_with("No relevant session transcripts") {
            Ok(TaskStatus::SoftDenial {
                reason: observation,
            })
        } else {
            Ok(TaskStatus::Success {
                stdout: observation,
            })
        }
    }

    // ── Visual-Semantic Driver: os:click ─────────────────────────────────

    async fn action_os_click(
        &self,
        intent: &ActionIntent,
        session_id: &str,
        agent_label: &str,
        high_risk_approvals: Arc<std::sync::atomic::AtomicUsize>,
    ) -> MoatResult<TaskStatus> {
        // Extract element_id from payload
        let element_id = match intent.payload.as_ref() {
            Some(Value::Object(m)) => m.get("element_id").and_then(Value::as_u64),
            Some(Value::Number(n)) => n.as_u64(),
            _ => None,
        };
        let element_id = match element_id {
            Some(id) => id as usize,
            None => {
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: "os:click requires payload {\"element_id\": number}".into(),
                });
            }
        };

        // Check for UI bridge availability
        let bridge = match &self.ui_bridge {
            Some(b) => Arc::clone(b),
            None => {
                return Ok(TaskStatus::SoftDenial {
                    reason: "No UI bridge available. The Visual-Semantic Driver is not active for this session.".into(),
                });
            }
        };

        // Require MetaBrain tool permission
        let has_permission = self.ensure_tool_permission("os:click").await.is_ok();
        if !has_permission {
            let approved = self
                .request_action_consent(
                    intent,
                    session_id,
                    agent_label,
                    Arc::clone(&high_risk_approvals),
                )
                .await?;
            if !approved {
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: "CAPABILITY DENIED: User denied os:click permission.".into(),
                });
            }
            {
                let mut brain = self.meta_brain.write().await;
                brain.grant_tool_permission(
                    "os:click",
                    PermissionFlags::TOOL_EXEC | PermissionFlags::EXEC,
                )?;
            }
            MetaBrain::flush_arc(Arc::clone(&self.meta_brain)).await?;
        }

        // Execute the click via the bridge
        match bridge.click(element_id) {
            Ok(msg) => {
                // Update UI state silently without context bloat
                let _ = bridge.refresh_ui_tree();
                Ok(TaskStatus::Success { stdout: msg })
            }
            Err(err) => Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: err,
            }),
        }
    }

    // ── Visual-Semantic Driver: os:type_text ─────────────────────────────

    async fn action_os_type_text(
        &self,
        intent: &ActionIntent,
        session_id: &str,
        agent_label: &str,
        high_risk_approvals: Arc<std::sync::atomic::AtomicUsize>,
    ) -> MoatResult<TaskStatus> {
        // Extract element_id and text from payload
        let (element_id, text) = match intent.payload.as_ref() {
            Some(Value::Object(m)) => {
                let id = m.get("element_id").and_then(Value::as_u64);
                let text = m
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                (id, text)
            }
            _ => (None, String::new()),
        };
        let element_id = match element_id {
            Some(id) => id as usize,
            None => {
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: "os:type_text requires payload {\"element_id\": number, \"text\": \"string\"}".into(),
                });
            }
        };
        if text.is_empty() {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: "os:type_text requires a non-empty \"text\" field in the payload.".into(),
            });
        }

        // Check for UI bridge availability
        let bridge = match &self.ui_bridge {
            Some(b) => Arc::clone(b),
            None => {
                return Ok(TaskStatus::SoftDenial {
                    reason: "No UI bridge available. The Visual-Semantic Driver is not active for this session.".into(),
                });
            }
        };

        // Require MetaBrain tool permission
        let has_permission = self.ensure_tool_permission("os:type_text").await.is_ok();
        if !has_permission {
            let approved = self
                .request_action_consent(
                    intent,
                    session_id,
                    agent_label,
                    Arc::clone(&high_risk_approvals),
                )
                .await?;
            if !approved {
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: "CAPABILITY DENIED: User denied os:type_text permission.".into(),
                });
            }
            {
                let mut brain = self.meta_brain.write().await;
                brain.grant_tool_permission(
                    "os:type_text",
                    PermissionFlags::TOOL_EXEC | PermissionFlags::EXEC,
                )?;
            }
            MetaBrain::flush_arc(Arc::clone(&self.meta_brain)).await?;
        }

        // Cap text length for safety
        const TYPE_TEXT_MAX_BYTES: usize = 4_096;
        if text.len() > TYPE_TEXT_MAX_BYTES {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: "os:type_text payload exceeds maximum size limit.".into(),
            });
        }

        // Execute the type_text via the bridge
        match bridge.type_text(element_id, &text) {
            Ok(msg) => {
                // Update UI state silently without context bloat
                let _ = bridge.refresh_ui_tree();
                Ok(TaskStatus::Success { stdout: msg })
            }
            Err(err) => Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: err,
            }),
        }
    }

    async fn action_fs_read(&self, intent: &ActionIntent) -> MoatResult<TaskStatus> {
        let target = intent.target.trim();

        // ── Determine if this is an external absolute path or a workspace-scoped one ──
        let is_external = {
            let path = Path::new(target);
            if path.is_absolute() {
                let root = config::workspace_root();
                path.strip_prefix(&root).is_err()
            } else {
                false
            }
        };

        if is_external {
            // ── External absolute path: use MetaBrain permission + direct read ──
            let abs = PathBuf::from(target);
            let safe_path = match self
                .ensure_path_permission(&abs.to_string_lossy(), PermissionFlags::FILE_READ)
                .await
            {
                Ok(p) => p,
                Err(reason) => {
                    return Ok(TaskStatus::Failed {
                        exit_code: None,
                        stderr: reason,
                    });
                }
            };

            // Security: never read if the canonical path has a symlink escape
            let canonical = match std::fs::canonicalize(&safe_path) {
                Ok(c) => c,
                Err(e) => {
                    return Ok(TaskStatus::Failed {
                        exit_code: None,
                        stderr: format!("Cannot resolve path '{}': {}", safe_path.display(), e),
                    });
                }
            };

            match std::fs::read_to_string(&canonical) {
                Ok(data) => Ok(TaskStatus::Success { stdout: data }),
                Err(e) => Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: format!("{} ({})", e, canonical.display()),
                }),
            }
        } else {
            // ── Workspace-scoped path: use existing sandbox capability ──
            let (relative_path, absolute_path) = match resolve_workspace_target(target) {
                Ok(paths) => paths,
                Err(reason) => {
                    return Ok(TaskStatus::Failed {
                        exit_code: None,
                        stderr: reason,
                    });
                }
            };

            let safe_path = match self
                .ensure_path_permission(
                    &absolute_path.to_string_lossy(),
                    PermissionFlags::FILE_READ,
                )
                .await
            {
                Ok(_) => absolute_path,
                Err(reason) => {
                    return Ok(TaskStatus::Failed {
                        exit_code: None,
                        stderr: reason,
                    });
                }
            };

            let capability = match WorkspaceCapability::open(&config::workspace_root()) {
                Ok(cap) => cap,
                Err(err) => {
                    return Ok(TaskStatus::Failed {
                        exit_code: None,
                        stderr: format!("Workspace capability open failed: {err}"),
                    });
                }
            };

            match capability.read_to_string(relative_path.as_path()) {
                Ok(data) => Ok(TaskStatus::Success { stdout: data }),
                Err(e) => Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: format!("{} ({})", e, safe_path.display()),
                }),
            }
        }
    }

    async fn action_fs_write(&self, intent: &ActionIntent) -> MoatResult<TaskStatus> {
        let (relative_path, absolute_path) = match resolve_workspace_target(&intent.target) {
            Ok(paths) => paths,
            Err(reason) => {
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: reason,
                });
            }
        };

        let safe_path = match self
            .ensure_path_permission(
                &absolute_path.to_string_lossy(),
                PermissionFlags::FILE_WRITE,
            )
            .await
        {
            Ok(_) => absolute_path,
            Err(reason) => {
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: reason,
                });
            }
        };
        let data = value_to_string(intent.payload.as_ref());
        let capability = match WorkspaceCapability::open(&config::workspace_root()) {
            Ok(cap) => cap,
            Err(err) => {
                let _ = cleanup_workspace_tmp_files();
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: format!("Workspace capability open failed: {err}"),
                });
            }
        };

        match capability.atomic_write(relative_path.as_path(), data.as_bytes()) {
            Ok(_) => {
                let db = self.db.lock().await;
                if let Err(err) = memory::mark_sandbox_written(&db, &safe_path, true) {
                    eprintln!(
                        "[Memory] failed to mark sandbox-written file {}: {err}",
                        safe_path.display()
                    );
                }
                Ok(TaskStatus::Success {
                    stdout: format!("wrote {} bytes to {}", data.len(), safe_path.display()),
                })
            }
            Err(e) => {
                let _ = cleanup_workspace_tmp_files();
                Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: e.to_string(),
                })
            }
        }
    }

    async fn action_fs_delete(&self, intent: &ActionIntent) -> MoatResult<TaskStatus> {
        let (relative_path, absolute_path) = match resolve_workspace_target(&intent.target) {
            Ok(paths) => paths,
            Err(reason) => {
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: reason,
                });
            }
        };

        if let Err(reason) = self
            .ensure_path_permission(
                &absolute_path.to_string_lossy(),
                PermissionFlags::FILE_DELETE,
            )
            .await
        {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: reason,
            });
        }

        let capability = match WorkspaceCapability::open(&config::workspace_root()) {
            Ok(cap) => cap,
            Err(err) => {
                let _ = cleanup_workspace_tmp_files();
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: format!("Workspace capability open failed: {err}"),
                });
            }
        };

        match capability.remove_file(relative_path.as_path()) {
            Ok(abs) => Ok(TaskStatus::Success {
                stdout: format!("deleted {}", abs.display()),
            }),
            Err(e) => Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: e.to_string(),
            }),
        }
    }

    async fn action_app_launch(&self, intent: &ActionIntent) -> MoatResult<TaskStatus> {
        if let Err(reason) = self.ensure_app_permission(&intent.target).await {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: reason,
            });
        }
        let mut cmd = Command::new("open");
        cmd.arg("-a").arg(&intent.target);
        let status = run_command(&mut cmd, self.config.command_timeout)?;

        // Update UI state silently after launching an app
        if let TaskStatus::Success { .. } = &status {
            if let Some(bridge) = &self.ui_bridge {
                // Non-blocking sleep to allow app animation
                tokio::time::sleep(Duration::from_millis(250)).await;
                let _ = bridge.refresh_ui_tree();
            }
        }
        Ok(status)
    }

    async fn action_process_spawn(
        &self,
        intent: &ActionIntent,
        root_node_id: u64,
    ) -> MoatResult<TaskStatus> {
        if is_blocked_process_spawn_binary(&intent.target) {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: "CAPABILITY DENIED: Interactive shells and interpreters cannot be spawned directly via process:spawn.".into(),
            });
        }
        if let Err(reason) = self.ensure_tool_permission(&intent.target).await {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: reason,
            });
        }
        let args = match value_to_string_vec(intent.payload.as_ref()) {
            Ok(a) => a,
            Err(msg) => {
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: msg,
                })
            }
        };

        let subgraph = self.relevant_subgraph(root_node_id, intent).await?;
        sandbox_status(
            &intent.target,
            &args,
            &subgraph,
            self.config.command_timeout,
        )
    }

    async fn action_os_open_url(&self, intent: &ActionIntent) -> MoatResult<TaskStatus> {
        let parsed = match Url::parse(&intent.target) {
            Ok(url) => url,
            Err(_) => {
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: format!("malformed URL: {}", intent.target),
                })
            }
        };
        if intent.target.contains('\0')
            || intent.target.contains('\n')
            || intent.target.contains('\r')
        {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: "CAPABILITY DENIED: URL contains control characters".into(),
            });
        }
        if let Err(reason) = self.ensure_url_scheme_permission(parsed.scheme()).await {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: reason,
            });
        }
        let mut cmd = Command::new("open");
        cmd.arg(&intent.target);
        run_command(&mut cmd, self.config.command_timeout)
    }

    async fn action_os_applescript(
        &self,
        intent: &ActionIntent,
        session_id: &str,
        agent_label: &str,
        high_risk_approvals: Arc<std::sync::atomic::AtomicUsize>,
    ) -> MoatResult<TaskStatus> {
        if !self.config.allow_high_risk {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: "CAPABILITY DENIED: high-risk actions are disabled".into(),
            });
        }
        let has_persistent_permission = self.ensure_tool_permission("os:applescript").await.is_ok();
        let script_raw = value_to_string(intent.payload.as_ref());
        if script_raw.is_empty() {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: "os:applescript requires a payload".into(),
            });
        }

        const APPLESCRIPT_MAX_BYTES: usize = 8_192;
        let script = script_raw.replace('\0', "");
        if script.len() > APPLESCRIPT_MAX_BYTES {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: "Script exceeds size limit".into(),
            });
        }

        if !has_persistent_permission {
            let approved = self
                .request_action_consent(
                    intent,
                    session_id,
                    agent_label,
                    Arc::clone(&high_risk_approvals),
                )
                .await?;
            if !approved {
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: "User denied AppleScript execution.".into(),
                });
            }
            {
                let mut brain = self.meta_brain.write().await;
                brain.grant_tool_permission(
                    "os:applescript",
                    PermissionFlags::TOOL_EXEC | PermissionFlags::EXEC,
                )?;
                brain.grant_tool_permission(
                    "osascript",
                    PermissionFlags::TOOL_EXEC | PermissionFlags::EXEC,
                )?;
            }
            MetaBrain::flush_arc(Arc::clone(&self.meta_brain)).await?;
        }

        let tmp_path = std::env::temp_dir().join(format!("moat_as_{}.applescript", Uuid::new_v4()));
        if let Err(e) = std::fs::write(&tmp_path, script.as_bytes()) {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: format!("Temp write failed: {}", e),
            });
        }
        #[cfg(unix)]
        {
            if let Err(err) = std::fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o400))
            {
                let _ = std::fs::remove_file(&tmp_path);
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: format!("Temp permission lock failed: {}", err),
                });
            }
        }

        let mut cmd = Command::new("osascript");
        cmd.arg(&tmp_path);
        let status = run_command(&mut cmd, self.config.command_timeout)?;
        let _ = std::fs::remove_file(&tmp_path);

        // Active Vision Loop: refresh UI tree after AppleScript execution
        if let TaskStatus::Success { stdout } = &status {
            if let Some(bridge) = &self.ui_bridge {
                if let Ok(tree_text) = bridge.refresh_ui_tree() {
                    return Ok(TaskStatus::Success {
                        stdout: format!(
                            "{}\n\n<active_ui_tree>\n{}\n</active_ui_tree>",
                            stdout, tree_text
                        ),
                    });
                }
            }
        }
        Ok(status)
    }

    async fn action_os_inject_text(
        &self,
        intent: &ActionIntent,
        session_id: &str,
        agent_label: &str,
        high_risk_approvals: Arc<std::sync::atomic::AtomicUsize>,
    ) -> MoatResult<TaskStatus> {
        if !self.config.allow_high_risk {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: "CAPABILITY DENIED: high-risk actions are disabled".into(),
            });
        }
        let has_persistent_permission = self.ensure_tool_permission("os:inject_text").await.is_ok();

        let (text_to_inject, expected_app_name) = match extract_inject_text_payload(
            intent.payload.as_ref(),
        ) {
            Some((text, expected_app_name))
                if !text.trim().is_empty() && !expected_app_name.trim().is_empty() =>
            {
                (text, expected_app_name)
            }
            _ => {
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr:
                        "os:inject_text requires payload {\"text_to_inject\":\"...\",\"expected_app_name\":\"...\"}"
                            .into(),
                });
            }
        };

        const INJECT_TEXT_MAX_BYTES: usize = 16_384;
        if text_to_inject.len() > INJECT_TEXT_MAX_BYTES {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: "Injected text exceeds size limit".into(),
            });
        }

        if !has_persistent_permission {
            let approved = self
                .request_action_consent(
                    intent,
                    session_id,
                    agent_label,
                    Arc::clone(&high_risk_approvals),
                )
                .await?;
            if !approved {
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: "User denied text injection.".into(),
                });
            }

            {
                let mut brain = self.meta_brain.write().await;
                brain.grant_tool_permission(
                    "os:inject_text",
                    PermissionFlags::TOOL_EXEC | PermissionFlags::EXEC,
                )?;
            }
            MetaBrain::flush_arc(Arc::clone(&self.meta_brain)).await?;
        }

        match active_app_matches(&expected_app_name) {
            Ok(true) => {}
            Ok(false) => {
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: "[SYSTEM: Target app lost focus. Injection aborted for safety.]".into(),
                });
            }
            Err(err) => {
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: format!("Active window check failed: {err}"),
                });
            }
        }

        match inject_text_via_clipboard(&text_to_inject) {
            Ok(()) => Ok(TaskStatus::Success {
                stdout: format!(
                    "Injected {} bytes into the active application.",
                    text_to_inject.len()
                ),
            }),
            Err(err) => Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: err,
            }),
        }
    }

    async fn action_net_curl(
        &self,
        intent: &ActionIntent,
        root_node_id: u64,
        session_id: &str,
        agent_label: &str,
        high_risk_approvals: Arc<std::sync::atomic::AtomicUsize>,
    ) -> MoatResult<TaskStatus> {
        if !self.config.allow_high_risk {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: "CAPABILITY DENIED: high-risk actions are disabled".into(),
            });
        }

        let (method, body, headers) = extract_curl_options(intent.payload.as_ref());
        let method_upper = method.to_ascii_uppercase();
        if !self.curl_allowed_methods().contains(&method_upper) {
            return Ok(TaskStatus::Failed {
                exit_code: None,
                stderr: format!(
                    "CAPABILITY DENIED: HTTP method '{}' not allowed",
                    method_upper
                ),
            });
        }
        let host = match extract_host(&intent.target) {
            Some(host) => host,
            None => {
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: format!("net:curl: could not extract host: {}", intent.target),
                })
            }
        };
        let has_domain_permission = {
            let brain = self.meta_brain.read().await;
            brain.domain_has_permission(&host, PermissionFlags::NET_OUTBOUND)
        };
        if !has_domain_permission {
            let approved = self
                .request_action_consent(
                    intent,
                    session_id,
                    agent_label,
                    Arc::clone(&high_risk_approvals),
                )
                .await?;
            if !approved {
                return Ok(TaskStatus::Failed {
                    exit_code: None,
                    stderr: "User denied high-risk action".into(),
                });
            }
            {
                let mut brain = self.meta_brain.write().await;
                brain.grant_domain_permission(&host, PermissionFlags::NET_OUTBOUND)?;
            }
            MetaBrain::flush_arc(Arc::clone(&self.meta_brain)).await?;
        }

        let timeout_secs = self.config.command_timeout.as_secs().to_string();
        let max_bytes = self.curl_max_response_bytes().to_string();
        let mut args = vec![
            "--silent".to_string(),
            "--fail-with-body".to_string(),
            "--max-time".to_string(),
            timeout_secs,
            "--max-filesize".to_string(),
            max_bytes,
            "--max-redirs".to_string(),
            "5".to_string(),
            "-X".to_string(),
            method_upper,
        ];

        for (k, v) in &headers {
            let safe_header = format!(
                "{}: {}",
                k.replace(['\r', '\n'], ""),
                v.replace(['\r', '\n'], "")
            );
            args.push("-H".to_string());
            args.push(safe_header);
        }
        if !body.is_empty() {
            args.push("--data-raw".to_string());
            args.push(body);
        }
        args.push(intent.target.clone());

        let subgraph = self.relevant_subgraph(root_node_id, intent).await?;
        sandbox_status("curl", &args, &subgraph, self.config.command_timeout)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// § Orchestrator (The ReAct Loop)
// ─────────────────────────────────────────────────────────────────────────────

const MAX_SWARM_DEPTH: usize = 3;
const MAX_DELEGATE_TURNS: usize = 5;
const LOOP_WINDOW: usize = 8; // Widened from 5 to catch A-B-A-B ping-pong patterns
const HARD_STOP_IDENTICAL: usize = 3;
const SOFT_WARN_SAME_CALL: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubAgentHandlingMode {
    InlineDelegate,
    YieldToSupervisor,
}

pub type SharedContext = Arc<AsyncRwLock<VersionedContext>>;

pub struct VersionedContext {
    history: Vec<LlmMessage>,
    generation: AtomicU64,
}

impl VersionedContext {
    pub fn new(seed: Vec<LlmMessage>) -> Self {
        Self {
            history: seed,
            generation: AtomicU64::new(0),
        }
    }

    pub fn snapshot(&self) -> Vec<LlmMessage> {
        self.history.clone()
    }

    pub fn push(&mut self, entry: LlmMessage) {
        self.history.push(entry);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    pub fn len(&self) -> usize {
        self.history.len()
    }

    pub fn try_swap(&mut self, expected_generation: u64, replacement: Vec<LlmMessage>) -> bool {
        if self
            .generation
            .compare_exchange(
                expected_generation,
                expected_generation + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            self.history = replacement;
            true
        } else {
            false
        }
    }

    pub fn prune_if_needed(&mut self, token_budget: usize) {
        // Aggressive sliding window: enforce max 6 history entries at ALL times,
        // not just when token budget > 18k. This prevents unbounded context growth
        // that causes the 24k token wall crash.
        if self.history.len() <= 6 {
            return;
        }

        let generation = self.generation.load(Ordering::SeqCst);

        // Light pruning: if under 12k tokens, just keep the sliding window
        // (first entry + last 5 entries) without summarization overhead.
        if token_budget <= 12_000 {
            let mut replacement = Vec::with_capacity(6);
            if let Some(first) = self.history.first() {
                replacement.push(first.clone());
            }
            replacement.extend(
                self.history[self.history.len().saturating_sub(5)..]
                    .iter()
                    .cloned(),
            );
            let _ = self.try_swap(generation, replacement);
            return;
        }

        // Heavy pruning: extract facts + summarize dropped turns + keep recent 3
        let facts = self.extract_facts();
        let summary = self.summarize_turns();

        let mut replacement = Vec::new();
        if let Some(first) = self.history.first() {
            replacement.push(first.clone());
        }
        if !facts.is_empty() {
            replacement.push(LlmMessage {
                role: "system".into(),
                content: Some(format!("[FACTS FROM PRIOR TURNS]\n{}", facts)),
                tool_calls: None,
                tool_call_id: None,
            });
        }
        if !summary.is_empty() {
            replacement.push(LlmMessage {
                role: "system".into(),
                content: Some(format!("[SUMMARY OF DROPPED TURNS]\n{}", summary)),
                tool_calls: None,
                tool_call_id: None,
            });
        }
        if self.history.len() > 3 {
            replacement.extend(
                self.history[self.history.len().saturating_sub(3)..]
                    .iter()
                    .cloned(),
            );
        }

        let _ = self.try_swap(generation, replacement);
    }

    fn extract_facts(&self) -> String {
        self.history
            .iter()
            .filter(|entry| {
                entry.content.as_deref().unwrap_or("").contains("SUCCESS:")
                    || entry.content.as_deref().unwrap_or("").contains("FAILED:")
            })
            .take(6)
            .cloned()
            .map(|m| m.content.clone().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn summarize_turns(&self) -> String {
        self.history
            .iter()
            .skip(1)
            .take(self.history.len().saturating_sub(4))
            .map(|entry| {
                entry
                    .content
                    .as_deref()
                    .unwrap_or("")
                    .chars()
                    .take(240)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n---\n")
    }
}

async fn snapshot_context(context: &SharedContext) -> Vec<LlmMessage> {
    context.read().await.snapshot()
}

async fn push_context(context: &SharedContext, entry: LlmMessage) {
    context.write().await.push(entry);
}

async fn prune_context_if_needed(context: &SharedContext, token_budget: usize) {
    context.write().await.prune_if_needed(token_budget);
}

async fn context_len(context: &SharedContext) -> usize {
    context.read().await.len()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolExecution {
    call_hash: [u8; 32],
    result_hash: [u8; 32],
}

pub struct LoopDetector {
    window: VecDeque<ToolExecution>,
}

pub enum LoopVerdict {
    Clear,
    SoftWarn { message: String },
    HardStop { message: String },
}

pub fn normalize_result_string(s: &str) -> String {
    let mut out = s.to_owned();

    let patterns = vec![
        (
            r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})",
            "<TIMESTAMP>",
        ),
        (
            r"(?:Mon|Tue|Wed|Thu|Fri|Sat|Sun)\s+\w+\s+\d{1,2}\s+\d{2}:\d{2}:\d{2}(?:\s+\d{4})?",
            "<TIMESTAMP>",
        ),
        (r"\b\d{2}:\d{2}:\d{2}(?:\.\d+)?\b", "<TIMESTAMP>"),
        (
            r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
            "<UUID>",
        ),
        (r"0x[0-9a-fA-F]{6,}", "<ADDR>"),
        (r"(?i)\bpid[= ]\d+", "<PID>"),
        (r"(?i)\bprocess\s+\d+", "process <PID>"),
        (
            r"(?i)\b(?:attempt|try|retry)\s+\d+(?:\s*[/of]+\s*\d+)?",
            "<COUNTER>",
        ),
        (
            r"\b\d+(?:\.\d+)?\s*(?:ms|milliseconds?|s|seconds?)\b",
            "<ELAPSED>",
        ),
        (r"(?i)\bfd[= ]\d+\b", "fd <FD>"),
    ];

    for (pat, rep) in patterns {
        if let Ok(re) = Regex::new(pat) {
            out = re.replace_all(&out, rep).into_owned();
        }
    }

    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl LoopDetector {
    pub fn new() -> Self {
        Self {
            window: VecDeque::with_capacity(LOOP_WINDOW),
        }
    }

    pub fn from_window(window: Vec<ToolExecution>) -> Self {
        Self {
            window: window.into_iter().collect(),
        }
    }

    pub fn snapshot(&self) -> Vec<ToolExecution> {
        self.window.iter().cloned().collect()
    }

    fn analyze(&mut self, execution: ToolExecution) -> LoopVerdict {
        let same_call = self
            .window
            .iter()
            .filter(|prev| prev.call_hash == execution.call_hash)
            .count()
            + 1;
        let identical = self
            .window
            .iter()
            .filter(|prev| {
                prev.call_hash == execution.call_hash && prev.result_hash == execution.result_hash
            })
            .count()
            + 1;

        if self.window.len() == LOOP_WINDOW {
            self.window.pop_front();
        }
        self.window.push_back(execution);

        if identical >= HARD_STOP_IDENTICAL {
            LoopVerdict::HardStop {
                message:
                    "Loop detector tripped: identical call/result pair repeated. Escalate to done."
                        .into(),
            }
        } else if self.detect_ping_pong() {
            // OpenJarvis-style ping-pong detection: A-B-A-B or A-B-C-A-B-C cycling
            LoopVerdict::HardStop {
                message: "Loop detector tripped: repetitive tool-calling pattern detected (ping-pong cycle). Change approach or emit done.".into(),
            }
        } else if same_call >= SOFT_WARN_SAME_CALL {
            LoopVerdict::SoftWarn {
                message:
                    "Loop detector warning: same call is repeating. Change strategy or finish."
                        .into(),
            }
        } else {
            LoopVerdict::Clear
        }
    }

    /// Detect A-B-A-B or A-B-C-A-B-C cycling patterns in the call hash sequence.
    /// Modeled after OpenJarvis's `_detect_ping_pong` which checks periods 2 and 3.
    fn detect_ping_pong(&self) -> bool {
        let hashes: Vec<_> = self.window.iter().map(|e| &e.call_hash).collect();
        let n = hashes.len();
        for period in [2, 3] {
            if n >= period * 2 {
                let tail = &hashes[n - period * 2..];
                let pattern = &tail[..period];
                if tail
                    .iter()
                    .enumerate()
                    .all(|(i, h)| *h == pattern[i % period])
                {
                    return true;
                }
            }
        }
        false
    }

    fn hash_execution(intent: &ActionIntent, status: &TaskStatus) -> ToolExecution {
        let mut call_hasher = Sha256::new();
        call_hasher.update(intent.action.as_bytes());
        if !intent.target.is_empty() {
            call_hasher.update(intent.target.as_bytes());
        }
        if let Some(payload) = &intent.payload {
            call_hasher.update(payload.to_string().as_bytes());
        }
        let call_hash: [u8; 32] = call_hasher.finalize().into();

        let mut result_hasher = Sha256::new();
        match status {
            TaskStatus::Success { stdout } => {
                result_hasher.update(&[1_u8]);
                let normalized = normalize_result_string(stdout.trim());
                result_hasher.update(normalized.as_bytes());
            }
            TaskStatus::SoftDenial { reason } => {
                // Hash same discriminant as Failed so loop detector catches retries
                result_hasher.update(&[0_u8]);
                let normalized = normalize_result_string(reason.trim());
                result_hasher.update(normalized.as_bytes());
            }
            TaskStatus::Failed { exit_code, stderr } => {
                result_hasher.update(&[0_u8]);
                let normalized = normalize_result_string(stderr.trim());
                result_hasher.update(normalized.as_bytes());
                if let Some(code) = exit_code {
                    result_hasher.update(&code.to_le_bytes());
                }
            }
            TaskStatus::Blocked { blocked_by } => {
                result_hasher.update(&[0_u8]);
                result_hasher.update(format!("blocked:{blocked_by}").as_bytes());
            }
            TaskStatus::Cancelled { stage } => {
                result_hasher.update(&[2_u8]);
                result_hasher.update(stage.as_bytes());
            }
        }
        let result_hash: [u8; 32] = result_hasher.finalize().into();

        ToolExecution {
            call_hash,
            result_hash,
        }
    }
}

pub struct Orchestrator {
    pub planner: PlannerAgent,
    pub worker: Arc<WorkerAgent>,
    pub meta_brain: Arc<AsyncRwLock<MetaBrain>>,
    pub db: Arc<AsyncMutex<Connection>>,
    pub injector_config: crate::injector::InjectorConfig,
    pub swarm_gateway: Option<Arc<SwarmGateway>>,
    pub active_agent: Option<crate::swarm::AgentSpec>,
    /// Visual-Semantic Driver bridge for os:click / os:type_text.
    pub ui_bridge: Option<Arc<dyn UiActionBridge>>,
}

impl Orchestrator {
    pub fn new(
        planner: PlannerAgent,
        worker: WorkerAgent,
        meta_brain: Arc<AsyncRwLock<MetaBrain>>,
        db: Arc<AsyncMutex<Connection>>,
        injector_config: crate::injector::InjectorConfig,
    ) -> Self {
        Self {
            planner,
            worker: Arc::new(worker),
            meta_brain,
            db,
            injector_config,
            swarm_gateway: None,
            active_agent: None,
            ui_bridge: None,
        }
    }

    pub fn with_swarm_gateway(mut self, swarm_gateway: Arc<SwarmGateway>) -> Self {
        self.swarm_gateway = Some(swarm_gateway);
        self
    }

    pub fn with_planner(&self, planner: PlannerAgent) -> Self {
        Self {
            planner,
            worker: Arc::clone(&self.worker),
            meta_brain: Arc::clone(&self.meta_brain),
            db: Arc::clone(&self.db),
            injector_config: self.injector_config.clone(),
            swarm_gateway: self.swarm_gateway.clone(),
            active_agent: self.active_agent.clone(),
            ui_bridge: self.ui_bridge.clone(),
        }
    }

    pub fn with_ui_bridge(mut self, bridge: Arc<dyn UiActionBridge>) -> Self {
        // Also set on the inner WorkerAgent so action handlers can access it
        let old = &*self.worker;
        let new_worker = WorkerAgent {
            config: old.config.clone(),
            meta_brain: Arc::clone(&old.meta_brain),
            db: Arc::clone(&old.db),
            consent_bridge: old.consent_bridge.clone(),
            ui_bridge: Some(Arc::clone(&bridge)),
        };
        self.worker = Arc::new(new_worker);
        self.ui_bridge = Some(bridge);
        self
    }

    pub fn with_agent_spec(&self, active_agent: crate::swarm::AgentSpec) -> Self {
        let mut cloned = self.with_planner(self.planner.clone());
        cloned.active_agent = Some(active_agent);
        cloned
    }

    fn agent_allows_process_spawn(&self) -> bool {
        self.active_agent
            .as_ref()
            .map(|spec| {
                spec.allowed_tools
                    .iter()
                    .any(|tool| tool.eq_ignore_ascii_case("process:spawn"))
            })
            .unwrap_or(false)
    }

    async fn visible_synthesized_skills(&self) -> MoatResult<Vec<SynthesizedSkill>> {
        if !self.agent_allows_process_spawn() {
            return Ok(Vec::new());
        }
        let db = self.db.lock().await;
        list_skills(&db)
    }

    /// Maps a tool name to its JSON schema description for prompt injection.
    /// Returns `None` for unknown tool names so they are silently excluded.
    fn tool_schema_entry(tool: &str) -> Option<(&'static str, &'static str)> {
        match tool {
            "fs:read" => Some(("fs:read", "payload: <file path string>")),
            "fs:write" => Some(("fs:write", "payload: <file contents string>")),
            "app:launch" => Some(("app:launch", "payload: null")),
            "process:spawn" => Some(("process:spawn", "payload: JSON ARRAY of arguments, e.g. [\"--flag\", \"/path\"]")),
            "os:open_url" => Some(("os:open_url", "payload: null")),
            "os:applescript" => Some(("os:applescript", "payload: <AppleScript string>")),
            "os:inject_text" => Some(("os:inject_text", "payload: JSON object {\"text_to_inject\":\"...\",\"expected_app_name\":\"...\"}")),
            "os:click" => Some(("os:click", "payload: JSON object {\"element_id\": number} — click a UI element from the active_ui_tree")),
            "os:type_text" => Some(("os:type_text", "payload: JSON object {\"element_id\": number, \"text\": \"string\"} — type text into a UI element")),
            "net:curl" => Some(("net:curl", "payload: JSON object {\"method\":\"POST\",\"body\":\"...\",\"headers\":{}}")),
            "memory:search" => Some(("memory:search", "payload: <specific search query string>")),
            "session:search" => Some(("session:search", "payload: <specific transcript search query string>")),
            "memory:learn" => Some(("memory:learn", "payload: JSON object {\"keywords\":\"...\", \"skill_description\":\"...\"}")),
            "meta:query_brain" => Some(("meta:query_brain", "payload: <keyword string to probe whether a file path or network domain is permitted>")),
            "swarm:delegate" | "system:spawn_worker" => Some(("system:spawn_worker", "payload: JSON object {\"task\":\"...\", \"specialization\":\"coder\"} or {\"target_agent_id\":\"coder\", \"task\":\"...\"}")),
            "propose_system_skill" => Some(("propose_system_skill", "payload: JSON object {\"skill_name\":\"...\",\"description\":\"...\",\"source_code\":\"...\",\"language\":\"python|bash\",\"parameters\":{\"url\":\"string\"}}")),
            "user:ask_choice" => Some(("user:ask_choice", "payload: JSON object {\"context\":\"...\", \"options\":[\"Option A\", \"Option B\"]}")),
            "done" => Some(("done", "payload: <direct, concise response to the user. DO NOT write summaries or recaps of your actions>")),
            _ => None,
        }
    }

    /// All known built-in tool names, used as fallback when no agent spec restricts the tool set.
    const ALL_TOOLS: &'static [&'static str] = &[
        "fs:read",
        "fs:write",
        "app:launch",
        "process:spawn",
        "os:open_url",
        "os:applescript",
        "os:inject_text",
        "os:click",
        "os:type_text",
        "net:curl",
        "memory:search",
        "session:search",
        "memory:learn",
        "meta:query_brain",
        "system:spawn_worker",
        "propose_system_skill",
        "user:ask_choice",
        "done",
    ];

    async fn build_react_system_prompt(&self) -> MoatResult<String> {
        let mut prompt = REACT_SYSTEM_TEMPLATE.to_string();

        // ── Dynamic ACTION SCHEMA based on agent-local allowed_tools ──
        let tool_list: Vec<&str> = match &self.active_agent {
            Some(spec) if !spec.allowed_tools.is_empty() => {
                let mut tools: Vec<&str> = Vec::new();
                for tool in &spec.allowed_tools {
                    if let Some((name, _)) = Self::tool_schema_entry(tool) {
                        if !tools
                            .iter()
                            .any(|existing| existing.eq_ignore_ascii_case(name))
                        {
                            tools.push(name);
                        }
                    }
                }
                // Always include done and user:ask_choice — universally available.
                for essential in &["done", "user:ask_choice"] {
                    if !tools.iter().any(|t| t.eq_ignore_ascii_case(essential)) {
                        tools.push(essential);
                    }
                }
                tools
            }
            _ => Self::ALL_TOOLS.to_vec(),
        };

        prompt.push_str("\n\nACTION SCHEMA\n");
        prompt.push_str(
            "End EVERY turn with exactly one action block — no exceptions:\n<action>\n{\n",
        );
        prompt.push_str("  \"_thought\": <string — ONE sentence (max 30 words) explaining your tool choice. MANDATORY.>,\n");

        // Build action enum from allowed tools only
        let action_enum: Vec<String> = tool_list.iter().map(|t| format!("\"{}\"", t)).collect();
        prompt.push_str(&format!("  \"action\":  <{}>,\n", action_enum.join(" | ")));
        prompt.push_str("  \"target\":  <string — absolute file path, exact app name, binary name, URL, or \"procedural\" for memory, \"metabrain\" for graph lookup, or \"user\" for conversation>,\n");

        // Build payload descriptions from allowed tools only
        prompt.push_str("  \"payload\": <");
        let mut payload_parts: Vec<String> = Vec::new();
        for tool_name in &tool_list {
            if let Some((_, schema_desc)) = Self::tool_schema_entry(tool_name) {
                payload_parts.push(format!("for {}: {}", tool_name, schema_desc));
            }
        }
        payload_parts.push("null otherwise".to_string());
        prompt.push_str(&payload_parts.join(" |\n              "));
        prompt.push_str(">\n}\n</action>\n");

        // ── Synthesized system skills ──
        let skills = self.visible_synthesized_skills().await?;
        if !skills.is_empty() {
            prompt.push_str("\n\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
            prompt.push_str("SYNTHESIZED SYSTEM SKILLS\n");
            prompt.push_str("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
            prompt.push_str(
                "The following installed skills are valid action names for this session.\n\
                 For these tools, set target to \"system\" and payload to a JSON object matching the exact parameter schema.\n\
                 Do not invent or omit keys.\n\n",
            );
            for skill in skills {
                prompt.push_str(&format!(
                    "- action: \"{}\"\n  description: {}\n  parameter_schema: {}\n",
                    skill.skill_name,
                    skill.description.trim(),
                    skill.parameter_schema
                ));
            }
        }

        Ok(prompt)
    }

    async fn synthesized_skill_for_action(
        &self,
        action: &str,
    ) -> MoatResult<Option<SynthesizedSkill>> {
        if !self.agent_allows_process_spawn() {
            return Ok(None);
        }
        let db = self.db.lock().await;
        get_skill(&db, action)
    }

    async fn enforce_agent_tool_policy(&self, intent: &ActionIntent) -> Option<String> {
        let spec = self.active_agent.as_ref()?;
        if spec.allowed_tools.is_empty() {
            return None;
        }
        // No need for cross-alias logic — normalize_action_alias already converts
        // request_sub_agent to swarm:delegate before we reach this point.
        if spec
            .allowed_tools
            .iter()
            .any(|tool| tool.eq_ignore_ascii_case(&intent.action))
        {
            return None;
        }
        if self.agent_allows_process_spawn() {
            match self.synthesized_skill_for_action(&intent.action).await {
                Ok(Some(_)) => return None,
                Ok(None) => {}
                Err(err) => {
                    return Some(format!(
                        "Action '{}' could not be validated against synthesized skills: {}",
                        intent.action, err
                    ))
                }
            }
        }
        Some(format!(
            "Action '{}' is not allowed for swarm agent '{}'. Allowed tools: {}",
            intent.action,
            spec.name,
            spec.allowed_tools.join(", ")
        ))
    }

    fn parse_ask_choice_payload(payload: Option<&Value>) -> Result<AskChoicePayload, String> {
        let payload = payload.ok_or_else(|| {
            "user:ask_choice requires payload {\"context\":\"...\",\"options\":[...]}".to_string()
        })?;
        let parsed: AskChoicePayload = serde_json::from_value(payload.clone())
            .map_err(|err| format!("user:ask_choice payload is invalid JSON object: {err}"))?;
        if parsed.context.trim().is_empty() {
            return Err("user:ask_choice context cannot be empty".into());
        }
        if parsed.options.is_empty() {
            return Err("user:ask_choice options cannot be empty".into());
        }

        for (index, option) in parsed.options.iter().enumerate() {
            let option = option.trim();
            if option.is_empty() {
                return Err(format!("user:ask_choice option {} is empty", index + 1));
            }
        }
        Ok(parsed)
    }

    fn missing_key_user_message(agent_name: &str, provider: &str) -> String {
        format!(
            "Agent '{agent_name}' requires a valid {} API key. Please click 'Settings' at the top of the window to configure it.",
            provider_display_name(provider)
        )
    }

    fn missing_key_message_from_error(
        &self,
        agent_name: &str,
        error: &MoatError,
    ) -> Option<String> {
        let provider = missing_key_provider_from_error(error)?;
        Some(Self::missing_key_user_message(agent_name, &provider))
    }

    fn format_choice_prompt(parsed: &AskChoicePayload) -> String {
        let mut lines = vec![parsed.context.trim().to_string()];
        for (index, option) in parsed.options.iter().enumerate() {
            lines.push(format!("{}. {}", index + 1, option.trim()));
        }
        lines.join("\n")
    }

    fn ask_choice_telemetry(parsed: &AskChoicePayload, request_id: &str) -> String {
        json!({
            "type": "ask_choice",
            "request_id": request_id,
            "context": crate::consent::clean_consent_summary(parsed.context.trim()),
            "options": parsed
                .options
                .iter()
                .map(|option| option.trim())
                .collect::<Vec<_>>(),
        })
        .to_string()
    }

    fn parse_sub_agent_payload(payload: Option<&Value>) -> Result<SubSpawnPayload, String> {
        let payload = payload.ok_or_else(|| {
            "system:spawn_worker requires payload {\"task\":\"...\",\"specialization\":\"coder\"} or {\"target_agent_id\":\"coder\",\"task\":\"...\"}"
                .to_string()
        })?;
        let parsed: SubSpawnPayload = serde_json::from_value(payload.clone())
            .map_err(|err| format!("system:spawn_worker payload is invalid JSON object: {err}"))?;
        if parsed.task_instruction.trim().is_empty() {
            return Err("system:spawn_worker task cannot be empty".into());
        }
        if parsed.agent_id.trim().is_empty()
            && parsed
                .specialization
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_none()
        {
            return Err(
                "system:spawn_worker requires either target_agent_id or specialization".into(),
            );
        }
        Ok(parsed)
    }

    fn resolve_delegate_target(&self, parsed: &SubSpawnPayload) -> Result<String, String> {
        let direct_target = parsed.agent_id.trim();
        if !direct_target.is_empty() {
            return Ok(direct_target.to_string());
        }

        let specialization = parsed
            .specialization
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                "system:spawn_worker requires either target_agent_id or specialization".to_string()
            })?;

        let gateway = self
            .swarm_gateway
            .as_ref()
            .ok_or_else(|| "swarm gateway unavailable for worker spawning".to_string())?;
        gateway
            .resolve_specialization(specialization)
            .ok_or_else(|| format!("No swarm worker matches specialization '{specialization}'"))
    }

    async fn search_session_transcripts(&self, query: &str, limit: usize) -> MoatResult<String> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok("session:search requires a non-empty query.".to_string());
        }

        let fts_query = format!("\"{}\"", trimmed.replace('"', " "));
        let db = self.db.lock().await;
        let mut stmt = db
            .prepare(
                "SELECT
                    gs.source,
                    gs.channel,
                    gs.title,
                    snippet(gateway_messages_fts, 0, '[', ']', ' ... ', 18),
                    gm.created_at
                 FROM gateway_messages_fts
                 JOIN gateway_messages gm ON gm.id = gateway_messages_fts.rowid
                 JOIN gateway_sessions gs ON gs.session_id = gm.session_id
                 WHERE gateway_messages_fts MATCH ?1
                 ORDER BY bm25(gateway_messages_fts), gm.created_at DESC
                 LIMIT ?2",
            )
            .map_err(MoatError::Database)?;
        let rows = stmt
            .query_map(params![fts_query, limit.max(1) as i64], |row| {
                let source: String = row.get(0)?;
                let channel: String = row.get(1)?;
                let title: String = row.get(2)?;
                let snippet: String = row.get(3)?;
                let timestamp: i64 = row.get(4)?;
                Ok((source, channel, title, snippet, timestamp))
            })
            .map_err(MoatError::Database)?;

        let mut lines = Vec::new();
        for row in rows {
            let (source, channel, title, snippet, timestamp) = row.map_err(MoatError::Database)?;
            let channel_label = if channel.trim().is_empty() {
                "default".to_string()
            } else {
                channel
            };
            lines.push(format!(
                "[{}:{} @ {}] {}\n{}",
                source.trim(),
                channel_label.trim(),
                timestamp,
                title.trim(),
                snippet.trim()
            ));
        }

        if lines.is_empty() {
            Ok("No relevant session transcripts found.".to_string())
        } else {
            Ok(lines.join("\n---\n"))
        }
    }

    fn format_delegate_observation(
        target_agent_id: &str,
        task: &str,
        results: &[SubTaskResult],
    ) -> String {
        let mut lines = vec![
            format!("Delegated agent: {}", target_agent_id.trim()),
            format!("Delegated task: {}", task.trim()),
        ];
        if results.is_empty() {
            lines.push("Delegate produced no observable output.".into());
            return lines.join("\n");
        }

        for result in results {
            let summary = match &result.status {
                TaskStatus::Success { stdout } => format!("SUCCESS: {}", stdout.trim()),
                TaskStatus::SoftDenial { reason } => format!("SOFT DENIAL: {}", reason.trim()),
                TaskStatus::Failed { stderr, .. } => format!("FAILED: {}", stderr.trim()),
                TaskStatus::Blocked { blocked_by } => format!("BLOCKED: {}", blocked_by),
                TaskStatus::Cancelled { stage } => format!("CANCELLED: {}", stage.trim()),
            };
            lines.push(format!(
                "Step {} | action={} | target={} | {}",
                result.subtask_id, result.action, result.target, summary
            ));
        }
        lines.join("\n")
    }

    async fn log_kernel_crash(&self, query: &str, error: &str) -> MoatResult<()> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let memory_text = format!(
            "[KERNEL ERROR] query=\"{}\" error=\"{}\"",
            query
                .chars()
                .take(256)
                .collect::<String>()
                .replace('"', "'"),
            error.replace('"', "'")
        );

        let _id = {
            let db = self.db.lock().await;
            memory::store_knowledge(
                &db,
                memory::KnowledgeKind::CrashNote,
                "kernel",
                Some("Kernel pipeline error"),
                &memory_text,
                Some(timestamp),
            )?
        };

        Ok(())
    }

    async fn remember_skill(&self, payload: Option<&Value>) -> MoatResult<String> {
        let payload = payload.ok_or_else(|| {
            MoatError::Orchestrator(
                "memory:learn requires payload {\"keywords\":\"...\",\"skill_description\":\"...\"}"
                    .into(),
            )
        })?;
        let parsed: LearnPayload = serde_json::from_value(payload.clone()).map_err(|err| {
            MoatError::Orchestrator(format!(
                "memory:learn payload is invalid JSON object: {err}"
            ))
        })?;

        let keywords = parsed.keywords.trim();
        let skill_description = parsed.skill_description.trim();
        if keywords.is_empty() || skill_description.is_empty() {
            return Err(MoatError::Orchestrator(
                "memory:learn requires non-empty keywords and skill_description".into(),
            ));
        }

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let skill_block = format!(
            "[LEARNED SKILL]\nKeywords: {}\nSkill: {}",
            keywords, skill_description
        );

        let _id = format!("skill::{}::{}", timestamp, Uuid::new_v4());
        {
            let db = self.db.lock().await;
            memory::store_knowledge(
                &db,
                memory::KnowledgeKind::Skill,
                "memory:learn",
                Some(&format!("Skill: {}", keywords)),
                &skill_block,
                Some(timestamp),
            )?;
        }

        Ok(skill_block)
    }

    pub async fn run_react(
        &self,
        objective: &str,
        context_bootstrap: &str,
        root_node_id: u64,
        status_tx: Option<tokio::sync::mpsc::Sender<String>>,
    ) -> MoatResult<Vec<SubTaskResult>> {
        match self
            .run_react_scoped_mode(
                objective,
                context_bootstrap,
                root_node_id,
                status_tx,
                0,
                Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                SubAgentHandlingMode::InlineDelegate,
                None,
                None,
                None,
            )
            .await?
        {
            ReactOutcome::Completed(results) => Ok(results),
            ReactOutcome::Yielded(_) => Err(MoatError::Orchestrator(
                "unexpected yielded sub-agent request in inline ReAct mode".into(),
            )),
        }
    }

    pub async fn run_react_outcome(
        &self,
        objective: &str,
        context_bootstrap: &str,
        root_node_id: u64,
        status_tx: Option<tokio::sync::mpsc::Sender<String>>,
        continuation: Option<ReactContinuation>,
        injected_observation: Option<String>,
        execution_world_state: Option<Arc<tokio::sync::RwLock<crate::system::ExecutionWorldState>>>,
    ) -> MoatResult<ReactOutcome> {
        self.run_react_scoped_mode(
            objective,
            context_bootstrap,
            root_node_id,
            status_tx,
            0,
            Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            SubAgentHandlingMode::YieldToSupervisor,
            continuation,
            injected_observation,
            execution_world_state,
        )
        .await
    }

    pub fn run_react_scoped<'a>(
        &'a self,
        objective: &'a str,
        context_bootstrap: &'a str,
        root_node_id: u64,
        status_tx: Option<tokio::sync::mpsc::Sender<String>>,
        swarm_depth: usize,
        delegate_count: Arc<std::sync::atomic::AtomicUsize>,
    ) -> BoxFuture<'a, MoatResult<Vec<SubTaskResult>>> {
        Box::pin(async move {
            match self
                .run_react_scoped_mode(
                    objective,
                    context_bootstrap,
                    root_node_id,
                    status_tx,
                    swarm_depth,
                    delegate_count,
                    SubAgentHandlingMode::InlineDelegate,
                    None,
                    None,
                    None,
                )
                .await?
            {
                ReactOutcome::Completed(results) => Ok(results),
                ReactOutcome::Yielded(_) => Err(MoatError::Orchestrator(
                    "unexpected yielded sub-agent request in inline scoped ReAct mode".into(),
                )),
            }
        })
    }

    pub fn run_react_scoped_outcome<'a>(
        &'a self,
        objective: &'a str,
        context_bootstrap: &'a str,
        root_node_id: u64,
        status_tx: Option<tokio::sync::mpsc::Sender<String>>,
        swarm_depth: usize,
        delegate_count: Arc<std::sync::atomic::AtomicUsize>,
        continuation: Option<ReactContinuation>,
        injected_observation: Option<String>,
    ) -> BoxFuture<'a, MoatResult<ReactOutcome>> {
        self.run_react_scoped_mode(
            objective,
            context_bootstrap,
            root_node_id,
            status_tx,
            swarm_depth,
            delegate_count,
            SubAgentHandlingMode::YieldToSupervisor,
            continuation,
            injected_observation,
            None,
        )
    }

    fn run_react_scoped_mode<'a>(
        &'a self,
        objective: &'a str,
        context_bootstrap: &'a str,
        root_node_id: u64,
        status_tx: Option<tokio::sync::mpsc::Sender<String>>,
        swarm_depth: usize,
        delegate_count: Arc<std::sync::atomic::AtomicUsize>,
        sub_agent_mode: SubAgentHandlingMode,
        continuation: Option<ReactContinuation>,
        injected_observation: Option<String>,
        execution_world_state: Option<Arc<tokio::sync::RwLock<crate::system::ExecutionWorldState>>>,
    ) -> BoxFuture<'a, MoatResult<ReactOutcome>> {
        Box::pin(async move {
            let mut initial_trace = continuation
                .as_ref()
                .map(|saved| {
                    if saved.context_trace.is_empty() {
                        vec![LlmMessage {
                            role: "system".into(),
                            content: Some(context_bootstrap.to_string()),
                            tool_calls: None,
                            tool_call_id: None,
                        }]
                    } else {
                        saved.context_trace.clone()
                    }
                })
                .unwrap_or_else(|| {
                    vec![LlmMessage {
                        role: "system".into(),
                        content: Some(context_bootstrap.to_string()),
                        tool_calls: None,
                        tool_call_id: None,
                    }]
                });

            if continuation.is_none() {
                let prelude = crate::hooks::run_active_memory_prelude(
                    objective,
                    None,
                    &[],
                    Arc::clone(&self.db),
                    status_tx.clone(),
                )
                .await;
                if !prelude.context_block.is_empty() {
                    initial_trace.push(LlmMessage {
                        role: "system".into(),
                        content: Some(format!(
                            "<active_memory_prelude>\n{}\n</active_memory_prelude>",
                            prelude.context_block
                        )),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
            }

            let mut turns = continuation.as_ref().map(|saved| saved.turns).unwrap_or(0);
            let mut searches = continuation
                .as_ref()
                .map(|saved| saved.searches)
                .unwrap_or(0);
            let mut session_searches = continuation
                .as_ref()
                .map(|saved| saved.session_searches)
                .unwrap_or(0);
            let mut total_tokens = continuation
                .as_ref()
                .map(|saved| saved.total_tokens)
                .unwrap_or(0);
            let mut results = Vec::new();
            let mut loop_detector = LoopDetector::from_window(
                continuation
                    .as_ref()
                    .map(|saved| saved.loop_window.clone())
                    .unwrap_or_default(),
            );
            let max_turns = self.planner.config.max_subtasks.max(1);
            let high_risk_approvals = Arc::new(std::sync::atomic::AtomicUsize::new(
                continuation
                    .as_ref()
                    .map(|saved| saved.high_risk_approvals)
                    .unwrap_or(0),
            ));
            let session_id = format!("react-{}", Uuid::new_v4());
            let agent_label = self
                .active_agent
                .as_ref()
                .map(|agent| agent.name.clone())
                .unwrap_or_else(|| "primary".to_string());

            if let Some(saved) = &continuation {
                delegate_count.store(saved.delegate_count, std::sync::atomic::Ordering::SeqCst);
            }

            let context_trace: SharedContext =
                Arc::new(AsyncRwLock::new(VersionedContext::new(initial_trace)));

            if let (Some(saved), Some(observation)) = (&continuation, injected_observation.as_ref())
            {
                push_context(
                    &context_trace,
                    LlmMessage {
                        role: "system".into(),
                        content: Some(format!(
                            "Turn {} INTENT: {} | TARGET: {}\nOBSERVATION: {}",
                            saved.turns,
                            saved.pending_tool_call.action,
                            saved.pending_tool_call.target,
                            observation
                        )),
                        tool_calls: None,
                        tool_call_id: None,
                    },
                )
                .await;
            }

            if let Some(tx) = &status_tx {
                let banner = if continuation.is_some() {
                    "🔄 ReAct Loop Resumed. Generating next intent..."
                } else {
                    "🔄 ReAct Loop Initialized. Generating first intent..."
                };
                let _ = tx.send(banner.into()).await;
            }

            // ── Cache the system prompt ONCE before the loop ──────────────
            // The action schema never changes mid-loop, so building it every
            // turn was pure waste. Hermes builds its system prompt once per
            // run_conversation call.
            let cached_system_prompt = self.build_react_system_prompt().await?;

            loop {
                turns += 1;

                if turns > max_turns {
                    if let Some(tx) = &status_tx {
                        let _ = tx
                            .send(format!(
                                "CRITICAL: {}-turn limit exceeded. Terminating loop.",
                                max_turns
                            ))
                            .await;
                    }
                    results.push(SubTaskResult {
                        subtask_id: turns,
                        action: "system:limit".into(),
                        target: "react".into(),
                        status: TaskStatus::Failed {
                            exit_code: None,
                            stderr: format!("Max turns ({}) exceeded.", max_turns),
                        },
                        elapsed: Duration::ZERO,
                        tokens_used: 0,
                    });
                    break;
                }

                // ── Hermes-inspired proactive compression ──────────────────
                // Instead of crashing at 24k, proactively compress at 70%
                // capacity (16,800 tokens) to prevent ever hitting the wall.
                // This mirrors Hermes' preflight compression strategy.
                const PROACTIVE_THRESHOLD: u32 = 16_800;
                const HARD_TOKEN_LIMIT: u32 = 24_000;

                if total_tokens > HARD_TOKEN_LIMIT {
                    if let Some(tx) = &status_tx {
                        let _ = tx
                            .send("❌ CRITICAL: 24,000 Token Wall Hit. Terminating loop.".into())
                            .await;
                    }
                    results.push(SubTaskResult {
                        subtask_id: turns,
                        action: "system:limit".into(),
                        target: "react".into(),
                        status: TaskStatus::Failed {
                            exit_code: None,
                            stderr: "Token budget (24k) exhausted.".into(),
                        },
                        elapsed: Duration::ZERO,
                        tokens_used: 0,
                    });
                    break;
                }

                // Proactive compression: compress well before the hard limit
                // so the loop can continue productively instead of crashing.
                if total_tokens > PROACTIVE_THRESHOLD {
                    prune_context_if_needed(&context_trace, total_tokens as usize).await;
                    if let Some(tx) = &status_tx {
                        let _ = tx
                            .send(format!(
                                "📦 Proactive compression: {} tokens used ({}% of budget). Pruning oldest turns.",
                                total_tokens,
                                (total_tokens * 100) / HARD_TOKEN_LIMIT
                            ))
                            .await;
                    }
                } else if context_len(&context_trace).await > 8 {
                    // Light sliding-window even below threshold if history is long
                    prune_context_if_needed(&context_trace, total_tokens as usize).await;
                }

                // ── Build prompt: flat join, no XML wrapping ──────────────
                // XML tags like <initial_context>, <turn_history> added ~80
                // tokens per turn for zero benefit — the system prompt already
                // tells the LLM the context structure. Hermes uses plain
                // message concatenation.
                let mut messages = vec![LlmMessage {
                    role: "system".into(),
                    content: Some(cached_system_prompt.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                }];
                messages.extend(snapshot_context(&context_trace).await);
                let llm_tools = Some(self.planner.build_llm_tools());
                let (raw, tokens) = match self
                    .planner
                    .generate_intent_tools(messages, llm_tools)
                    .await
                {
                    Ok(mut resp) => {
                        let mut action = "done".to_string();
                        let mut target = "user".to_string();
                        let mut payload = serde_json::Value::Null;
                        let mut thought: Option<String> = None;
                        if let Some(mut tc) = resp.tool_calls.take() {
                            if let Some(t) = tc.pop() {
                                action = t.function.name;
                                target = "user".to_string(); // fallback
                                if let Ok(args) =
                                    serde_json::from_str::<serde_json::Value>(&t.function.arguments)
                                {
                                    if let Some(tgt) = args.get("target").and_then(|v| v.as_str()) {
                                        target = tgt.to_string();
                                    }
                                    if let Some(pld) = args.get("payload") {
                                        payload = pld.clone();
                                    }
                                    if let Some(th) = args.get("_thought").and_then(|v| v.as_str())
                                    {
                                        thought = Some(th.to_string());
                                    }
                                }
                            }
                        } else if let Some(content) = &resp.content {
                            // No tool_calls returned — LLM gave a conversational reply.
                            // Treat as "done" with the content as the payload so the user sees it.
                            let trimmed = content.trim();
                            if !trimmed.is_empty() {
                                payload = serde_json::Value::String(trimmed.to_string());
                            }
                        }
                        let mut intent_obj = serde_json::json!({
                            "action": action,
                            "target": target,
                            "payload": payload
                        });
                        if let Some(th) = thought {
                            intent_obj["thought"] = serde_json::Value::String(th);
                        }
                        let intent_json = intent_obj.to_string();
                        (intent_json, resp.tokens)
                    }
                    Err(err) => {
                        if let Some(message) = self.missing_key_message_from_error(
                            self.active_agent
                                .as_ref()
                                .map(|agent| agent.name.as_str())
                                .unwrap_or("primary"),
                            &err,
                        ) {
                            if let Some(tx) = &status_tx {
                                let _ = tx.send(format!("⚠️ {message}")).await;
                            }
                            results.push(SubTaskResult {
                                subtask_id: turns,
                                action: "done".into(),
                                target: "user".into(),
                                status: TaskStatus::Success { stdout: message },
                                elapsed: Duration::ZERO,
                                tokens_used: 0,
                            });
                            break;
                        }
                        return Err(err);
                    }
                };
                total_tokens += tokens;

                if let Some(tx) = &status_tx {
                    let monologue = if let Some(start) = raw.find("<action>") {
                        raw[..start].trim()
                    } else {
                        raw.trim()
                    };
                    if !monologue.is_empty() {
                        let _ = tx.send(format!("🤖 Agent Thoughts: \n{}", monologue)).await;
                    }
                }

                let intent = PlannerAgent::parse_intent(&raw);

                if intent.action == "system:parse_error" {
                    let observation = "[SYSTEM: Your last response could not be parsed. You MUST respond with a valid <action> JSON block. Try again.]";
                    push_context(
                        &context_trace,
                        LlmMessage {
                            role: "system".into(),
                            content: Some(format!(
                            "Turn {} INTENT: system:parse_error | TARGET: parser\nOBSERVATION: {}",
                            turns, observation)),
                            tool_calls: None,
                            tool_call_id: None,
                        },
                    )
                    .await;
                    if let Some(tx) = &status_tx {
                        let _ = tx.send(observation.to_string()).await;
                    }
                    continue;
                }

                if let Some(err) = self.enforce_agent_tool_policy(&intent).await {
                    let mut final_err = err.clone();
                    if let Some(bridge) = &self.worker.consent_bridge {
                        let forced = AskChoicePayload {
                            context: format!(
                                "Sandbox tool policy blocked action '{}': {}. How should the agent proceed?",
                                intent.action, err
                            ),
                            options: vec![
                                "Remind agent to use system:spawn_worker to delegate".into(),
                                "Cancel task".into(),
                            ],
                        };
                        let choice_request_id = uuid::Uuid::new_v4().to_string();
                        if let Some(tx) = &status_tx {
                            let _ = tx
                                .send(Self::ask_choice_telemetry(&forced, &choice_request_id))
                                .await;
                            let _ = tx
                                .send("⏸️ Waiting for user choice on blocked tool...".to_string())
                                .await;
                        }
                        let selection = bridge.request_user_choice(&choice_request_id).await;
                        let observation = if selection.is_empty() {
                            "[USER CHOICE]: (no selection — user dismissed)".to_string()
                        } else {
                            format!("[USER CHOICE]: \"{}\"", selection)
                        };
                        final_err = format!("{} \n {}", err, observation);
                    }

                    push_context(
                        &context_trace,
                        LlmMessage {
                            role: "system".into(),
                            content: Some(format!(
                                "Turn {} INTENT: {} | TARGET: {}\nOBSERVATION: FAILED: {}",
                                turns, intent.action, intent.target, final_err
                            )),
                            tool_calls: None,
                            tool_call_id: None,
                        },
                    )
                    .await;
                    results.push(SubTaskResult {
                        subtask_id: turns,
                        action: intent.action.clone(),
                        target: intent.target.clone(),
                        status: TaskStatus::Failed {
                            exit_code: None,
                            stderr: final_err,
                        },
                        elapsed: Duration::ZERO,
                        tokens_used: tokens,
                    });
                    continue;
                }

                // ── Mechanical Capability Denial Filter ──────────────────────
                // Short-circuit: if this action+target was already denied in
                // this workflow run, fail immediately without executing, saving
                // the entire turn cost.
                if let Some(ref state_lock) = execution_world_state {
                    let state = state_lock.read().await;
                    if state.is_capability_denied(&intent.action, &intent.target) {
                        let err = format!(
                            "CAPABILITY DENIED (cached): '{}' on '{}' was already denied earlier in this workflow run. \
                             Do NOT retry. Use 'done' to report the limitation or 'user:ask_choice' to request permission.",
                            intent.action, intent.target
                        );
                        push_context(
                            &context_trace,
                            LlmMessage {
                                role: "system".into(),
                                content: Some(format!(
                                    "Turn {} INTENT: {} | TARGET: {}\nOBSERVATION: {}",
                                    turns, intent.action, intent.target, err
                                )),
                                tool_calls: None,
                                tool_call_id: None,
                            },
                        )
                        .await;
                        results.push(SubTaskResult {
                            subtask_id: turns,
                            action: intent.action.clone(),
                            target: intent.target.clone(),
                            status: TaskStatus::Failed {
                                exit_code: None,
                                stderr: err,
                            },
                            elapsed: Duration::ZERO,
                            tokens_used: tokens,
                        });
                        continue;
                    }
                }

                if intent.action == "done" {
                    if let Some(tx) = &status_tx {
                        let _ = tx
                            .send("✅ Task marked complete or yielded by agent.".into())
                            .await;
                    }
                    results.push(SubTaskResult {
                        subtask_id: turns,
                        action: intent.action.clone(),
                        target: intent.target.clone(),
                        status: TaskStatus::Success {
                            stdout: value_to_string(intent.payload.as_ref()),
                        },
                        elapsed: Duration::ZERO,
                        tokens_used: tokens,
                    });
                    break;
                }

                if intent.action == "swarm:delegate"
                    || intent.action == "system:spawn_worker"
                    || intent.action == "request_sub_agent"
                {
                    let parsed = match Self::parse_sub_agent_payload(intent.payload.as_ref()) {
                        Ok(parsed) => parsed,
                        Err(err) => {
                            push_context(
                                &context_trace,
                                LlmMessage {
                                    role: "system".into(),
                                    content: Some(format!(
                                        "Turn {} ERROR: {}. Fix the payload and continue.",
                                        turns, err
                                    )),
                                    tool_calls: None,
                                    tool_call_id: None,
                                },
                            )
                            .await;
                            results.push(SubTaskResult {
                                subtask_id: turns,
                                action: intent.action.clone(),
                                target: intent.target.clone(),
                                status: TaskStatus::Failed {
                                    exit_code: None,
                                    stderr: err,
                                },
                                elapsed: Duration::ZERO,
                                tokens_used: tokens,
                            });
                            continue;
                        }
                    };
                    let target_agent_id = match self.resolve_delegate_target(&parsed) {
                        Ok(agent_id) => agent_id,
                        Err(err) => {
                            push_context(
                                &context_trace,
                                LlmMessage {
                                    role: "system".into(),
                                    content: Some(format!(
                                        "Turn {} ERROR: {}. Fix the payload and continue.",
                                        turns, err
                                    )),
                                    tool_calls: None,
                                    tool_call_id: None,
                                },
                            )
                            .await;
                            results.push(SubTaskResult {
                                subtask_id: turns,
                                action: intent.action.clone(),
                                target: intent.target.clone(),
                                status: TaskStatus::Failed {
                                    exit_code: None,
                                    stderr: err,
                                },
                                elapsed: Duration::ZERO,
                                tokens_used: tokens,
                            });
                            continue;
                        }
                    };

                    let used = delegate_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if used > MAX_DELEGATE_TURNS {
                        let forced = AskChoicePayload {
                            context: format!(
                                "Delegation limit ({MAX_DELEGATE_TURNS}) reached. I need your guidance to proceed."
                            ),
                            options: vec![
                                "Continue without delegation".into(),
                                "Stop and summarize progress".into(),
                            ],
                        };
                        let forced_choice_id = uuid::Uuid::new_v4().to_string();
                        if let Some(tx) = &status_tx {
                            let _ = tx
                                .send(Self::ask_choice_telemetry(&forced, &forced_choice_id))
                                .await;
                        }
                        if let Some(bridge) = &self.worker.consent_bridge {
                            let selection = bridge.request_user_choice(&forced_choice_id).await;
                            let observation = if selection.is_empty() {
                                "[USER CHOICE]: (no selection — user dismissed)".to_string()
                            } else {
                                format!("[USER CHOICE]: \"{}\"", selection)
                            };
                            push_context(
                                &context_trace,
                                LlmMessage {
                                    role: "system".into(),
                                    content: Some(format!(
                                        "Turn {} OBSERVATION: {}",
                                        turns, observation
                                    )),
                                    tool_calls: None,
                                    tool_call_id: None,
                                },
                            )
                            .await;
                            results.push(SubTaskResult {
                                subtask_id: turns,
                                action: "user:ask_choice".into(),
                                target: "user".into(),
                                status: TaskStatus::Success {
                                    stdout: observation,
                                },
                                elapsed: Duration::ZERO,
                                tokens_used: tokens,
                            });
                            continue;
                        } else {
                            let output = Self::format_choice_prompt(&forced);
                            results.push(SubTaskResult {
                                subtask_id: turns,
                                action: "user:ask_choice".into(),
                                target: "user".into(),
                                status: TaskStatus::Success { stdout: output },
                                elapsed: Duration::ZERO,
                                tokens_used: tokens,
                            });
                            break;
                        }
                    }

                    match sub_agent_mode {
                        SubAgentHandlingMode::YieldToSupervisor => {
                            if let Some(tx) = &status_tx {
                                let _ = tx
                                    .send(format!(
                                        "⏸️ Yielding to the session assistant for sub-agent '{}'...",
                                        target_agent_id
                                    ))
                                    .await;
                            }
                            let continuation = ReactContinuation {
                                context_trace: snapshot_context(&context_trace).await,
                                turns,
                                searches,
                                session_searches,
                                total_tokens,
                                delegate_count: delegate_count
                                    .load(std::sync::atomic::Ordering::SeqCst),
                                high_risk_approvals: high_risk_approvals
                                    .load(std::sync::atomic::Ordering::SeqCst),
                                loop_window: loop_detector.snapshot(),
                                pending_tool_call: intent.clone(),
                            };
                            return Ok(ReactOutcome::Yielded(YieldStatus::AwaitingSubAgent {
                                agent_id: target_agent_id,
                                task_instruction: parsed.task_instruction,
                                original_tool_call: intent,
                                continuation,
                            }));
                        }
                        SubAgentHandlingMode::InlineDelegate => {
                            if swarm_depth + 1 >= MAX_SWARM_DEPTH {
                                let err =
                                    "MAX_SWARM_DEPTH exceeded. You must resolve the task yourself."
                                        .to_string();
                                push_context(&context_trace, LlmMessage { role: "system".into(), content: Some(format!(
                                    "Turn {} INTENT: swarm:delegate | TARGET: {}\nOBSERVATION: FAILED: {}",
                                    turns, target_agent_id, err)), tool_calls: None, tool_call_id: None })
                                .await;
                                results.push(SubTaskResult {
                                    subtask_id: turns,
                                    action: intent.action.clone(),
                                    target: target_agent_id.clone(),
                                    status: TaskStatus::Failed {
                                        exit_code: None,
                                        stderr: err,
                                    },
                                    elapsed: Duration::ZERO,
                                    tokens_used: tokens,
                                });
                                continue;
                            }

                            let Some(gateway) = &self.swarm_gateway else {
                                let err = "swarm gateway unavailable for delegation".to_string();
                                push_context(&context_trace, LlmMessage { role: "system".into(), content: Some(format!(
                                    "Turn {} INTENT: swarm:delegate | TARGET: {}\nOBSERVATION: FAILED: {}",
                                    turns, target_agent_id, err)), tool_calls: None, tool_call_id: None })
                                .await;
                                results.push(SubTaskResult {
                                    subtask_id: turns,
                                    action: intent.action.clone(),
                                    target: target_agent_id.clone(),
                                    status: TaskStatus::Failed {
                                        exit_code: None,
                                        stderr: err,
                                    },
                                    elapsed: Duration::ZERO,
                                    tokens_used: tokens,
                                });
                                continue;
                            };

                            if let Some(tx) = &status_tx {
                                let _ = tx
                                    .send(format!(
                                        "🕸️ Delegating to swarm agent '{}'...",
                                        target_agent_id
                                    ))
                                    .await;
                            }

                            let delegate_start = std::time::Instant::now();
                            let delegate_context = run_precompute_memory_hook(
                                &parsed.task_instruction,
                                Arc::clone(&self.db),
                                status_tx.clone(),
                            )
                            .await;
                            match gateway
                                .run_delegate(
                                    self,
                                    Arc::clone(&self.worker),
                                    &target_agent_id,
                                    &parsed.task_instruction,
                                    &delegate_context,
                                    root_node_id,
                                    status_tx.clone(),
                                    swarm_depth,
                                    Arc::clone(&delegate_count),
                                )
                                .await
                            {
                                Ok(delegate_results) => {
                                    let delegate_tokens: u32 = delegate_results
                                        .iter()
                                        .map(|result| result.tokens_used)
                                        .sum();
                                    total_tokens += delegate_tokens;
                                    let observation = Self::format_delegate_observation(
                                        &target_agent_id,
                                        &parsed.task_instruction,
                                        &delegate_results,
                                    );
                                    push_context(&context_trace, LlmMessage { role: "system".into(), content: Some(format!(
                                        "Turn {} INTENT: swarm:delegate | TARGET: {}\nOBSERVATION: {}",
                                        turns, target_agent_id, observation)), tool_calls: None, tool_call_id: None })
                                    .await;
                                    results.push(SubTaskResult {
                                        subtask_id: turns,
                                        action: intent.action.clone(),
                                        target: target_agent_id.clone(),
                                        status: TaskStatus::Success {
                                            stdout: observation,
                                        },
                                        elapsed: delegate_start.elapsed(),
                                        tokens_used: tokens + delegate_tokens,
                                    });
                                }
                                Err(err) => {
                                    if let Some(message) =
                                        self.missing_key_message_from_error(&target_agent_id, &err)
                                    {
                                        push_context(&context_trace, LlmMessage { role: "system".into(), content: Some(format!(
                                            "Turn {} INTENT: swarm:delegate | TARGET: {}\nOBSERVATION: {}",
                                            turns, target_agent_id, message)), tool_calls: None, tool_call_id: None })
                                        .await;
                                        results.push(SubTaskResult {
                                            subtask_id: turns,
                                            action: intent.action.clone(),
                                            target: target_agent_id.clone(),
                                            status: TaskStatus::Success { stdout: message },
                                            elapsed: delegate_start.elapsed(),
                                            tokens_used: tokens,
                                        });
                                        continue;
                                    }

                                    let err_string = format!(
                                        "[SYSTEM FAULT] Delegated agent '{}' failed: {}",
                                        target_agent_id, err
                                    );
                                    let _ = self
                                        .log_kernel_crash(&parsed.task_instruction, &err_string)
                                        .await;
                                    push_context(&context_trace, LlmMessage { role: "system".into(), content: Some(format!(
                                        "Turn {} INTENT: swarm:delegate | TARGET: {}\nOBSERVATION: FAILED: {}",
                                        turns, target_agent_id, err_string)), tool_calls: None, tool_call_id: None })
                                    .await;
                                    results.push(SubTaskResult {
                                        subtask_id: turns,
                                        action: intent.action.clone(),
                                        target: target_agent_id,
                                        status: TaskStatus::Failed {
                                            exit_code: None,
                                            stderr: err_string,
                                        },
                                        elapsed: delegate_start.elapsed(),
                                        tokens_used: tokens,
                                    });
                                }
                            }
                            continue;
                        }
                    }
                }

                if intent.action == "user:ask_choice" {
                    if intent.target != "user" {
                        let err = "user:ask_choice target must be \"user\"".to_string();
                        push_context(
                            &context_trace,
                            LlmMessage {
                                role: "system".into(),
                                content: Some(format!(
                                    "Turn {} ERROR: {}. Fix the action and continue.",
                                    turns, err
                                )),
                                tool_calls: None,
                                tool_call_id: None,
                            },
                        )
                        .await;
                        results.push(SubTaskResult {
                            subtask_id: turns,
                            action: intent.action.clone(),
                            target: intent.target.clone(),
                            status: TaskStatus::Failed {
                                exit_code: None,
                                stderr: err,
                            },
                            elapsed: Duration::ZERO,
                            tokens_used: tokens,
                        });
                        continue;
                    }

                    let parsed = match Self::parse_ask_choice_payload(intent.payload.as_ref()) {
                        Ok(parsed) => parsed,
                        Err(err) => {
                            push_context(
                                &context_trace,
                                LlmMessage {
                                    role: "system".into(),
                                    content: Some(format!(
                                        "Turn {} ERROR: {}. Fix the payload and continue.",
                                        turns, err
                                    )),
                                    tool_calls: None,
                                    tool_call_id: None,
                                },
                            )
                            .await;
                            results.push(SubTaskResult {
                                subtask_id: turns,
                                action: intent.action.clone(),
                                target: intent.target.clone(),
                                status: TaskStatus::Failed {
                                    exit_code: None,
                                    stderr: err,
                                },
                                elapsed: Duration::ZERO,
                                tokens_used: tokens,
                            });
                            continue;
                        }
                    };

                    // ── True pause/resume: suspend on oneshot until user selects ──
                    let choice_request_id = uuid::Uuid::new_v4().to_string();

                    if let Some(tx) = &status_tx {
                        let _ = tx
                            .send(Self::ask_choice_telemetry(&parsed, &choice_request_id))
                            .await;
                    }

                    // If consent bridge is available, truly suspend; otherwise fall back to break.
                    if let Some(bridge) = &self.worker.consent_bridge {
                        if let Some(tx) = &status_tx {
                            let _ = tx.send("⏸️ Waiting for user choice...".to_string()).await;
                        }
                        let selection = bridge.request_user_choice(&choice_request_id).await;
                        let observation = if selection.is_empty() {
                            "[USER CHOICE]: (no selection — user dismissed)".to_string()
                        } else {
                            format!("[USER CHOICE]: \"{}\"", selection)
                        };
                        push_context(
                            &context_trace,
                            LlmMessage {
                                role: "system".into(),
                                content: Some(format!(
                                    "Turn {} OBSERVATION: {}",
                                    turns, observation
                                )),
                                tool_calls: None,
                                tool_call_id: None,
                            },
                        )
                        .await;
                        results.push(SubTaskResult {
                            subtask_id: turns,
                            action: intent.action.clone(),
                            target: intent.target.clone(),
                            status: TaskStatus::Success {
                                stdout: observation,
                            },
                            elapsed: Duration::ZERO,
                            tokens_used: tokens,
                        });
                        continue;
                    } else {
                        // No bridge — legacy fallback (break the loop)
                        let output = Self::format_choice_prompt(&parsed);
                        results.push(SubTaskResult {
                            subtask_id: turns,
                            action: intent.action.clone(),
                            target: intent.target.clone(),
                            status: TaskStatus::Success { stdout: output },
                            elapsed: Duration::ZERO,
                            tokens_used: tokens,
                        });
                        break;
                    }
                }

                if intent.action == "memory:search" {
                    searches += 1;
                    if searches > 10 {
                        push_context(&context_trace, LlmMessage { role: "system".into(), content: Some(format!("Turn {} OBSERVATION: memory:search budget (10) exhausted. You must proceed with available context.", turns)), tool_calls: None, tool_call_id: None }).await;
                        continue;
                    }

                    let query = value_to_string(intent.payload.as_ref());
                    if let Some(tx) = &status_tx {
                        let _ = tx
                            .send(format!("🔍 Searching Neural Memory for: '{}'", query))
                            .await;
                    }

                    let max_chars = std::env::var("MOAT_MEMORY_MAX_CHARS")
                        .ok()
                        .and_then(|v| v.parse::<usize>().ok())
                        .filter(|v| *v > 0)
                        .unwrap_or(600);

                    let found = {
                        let db = self.db.lock().await;
                        let mut stmt = db
                            .prepare(
                                "SELECT title, text FROM knowledge_memory_fts 
                                 WHERE knowledge_memory_fts MATCH ?1 
                                 ORDER BY rank 
                                 LIMIT 3",
                            )
                            .unwrap_or_else(|_| panic!("Failed to prepare query"));
                        let query_cleaned = query.replace('\"', " ");
                        let fts_query = format!("\"{}\"", query_cleaned);

                        let mut hits = Vec::new();
                        if let Ok(rows) = stmt.query_map(rusqlite::params![fts_query], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        }) {
                            for row in rows {
                                if let Ok((title, text)) = row {
                                    hits.push((title, text));
                                }
                            }
                        }

                        hits.into_iter()
                            .map(|(title, text)| {
                                let clipped: String = text.chars().take(max_chars).collect();
                                let clipped_text = if clipped.len() == text.len() {
                                    clipped
                                } else {
                                    format!("{clipped}…")
                                };
                                format!("[{}] {}", title, clipped_text)
                            })
                            .collect::<Vec<_>>()
                            .join("\n---\n")
                    };
                    let found = if found.is_empty() {
                        "No relevant memories found at current similarity threshold.".to_string()
                    } else {
                        found
                    };

                    push_context(
                        &context_trace,
                        LlmMessage {
                            role: "system".into(),
                            content: Some(format!(
                                "Turn {} INTENT: memory:search | QUERY: {}\nOBSERVATION:\n{}",
                                turns, query, found
                            )),
                            tool_calls: None,
                            tool_call_id: None,
                        },
                    )
                    .await;
                    results.push(SubTaskResult {
                        subtask_id: turns,
                        action: intent.action.clone(),
                        target: intent.target.clone(),
                        status: if found.starts_with("No relevant memories") {
                            TaskStatus::SoftDenial {
                                reason: found.clone(),
                            }
                        } else {
                            TaskStatus::Success {
                                stdout: format!("Searched memory for: {}", query),
                            }
                        },
                        elapsed: Duration::ZERO,
                        tokens_used: tokens,
                    });
                    continue;
                }

                if intent.action == "session:search" {
                    session_searches += 1;
                    if session_searches > 6 {
                        push_context(&context_trace, LlmMessage { role: "system".into(), content: Some(format!("Turn {} OBSERVATION: session:search budget (6) exhausted. Continue with the transcripts already retrieved.", turns)), tool_calls: None, tool_call_id: None }).await;
                        continue;
                    }

                    let query = value_to_string(intent.payload.as_ref());
                    if let Some(tx) = &status_tx {
                        let _ = tx
                            .send(format!("🧵 Searching session transcripts for: '{}'", query))
                            .await;
                    }

                    let found = self.search_session_transcripts(&query, 5).await?;
                    push_context(
                        &context_trace,
                        LlmMessage {
                            role: "system".into(),
                            content: Some(format!(
                                "Turn {} INTENT: session:search | QUERY: {}\nOBSERVATION:\n{}",
                                turns, query, found
                            )),
                            tool_calls: None,
                            tool_call_id: None,
                        },
                    )
                    .await;
                    results.push(SubTaskResult {
                        subtask_id: turns,
                        action: intent.action.clone(),
                        target: intent.target.clone(),
                        status: if found.starts_with("No relevant session transcripts") {
                            TaskStatus::SoftDenial {
                                reason: found.clone(),
                            }
                        } else {
                            TaskStatus::Success {
                                stdout: format!("Searched session transcripts for: {}", query),
                            }
                        },
                        elapsed: Duration::ZERO,
                        tokens_used: tokens,
                    });
                    continue;
                }

                if intent.action == "memory:learn" {
                    if intent.target != "procedural" {
                        let err = "memory:learn target must be \"procedural\"".to_string();
                        push_context(
                            &context_trace,
                            LlmMessage {
                                role: "system".into(),
                                content: Some(format!(
                                    "Turn {} ERROR: {}. Fix the action and continue.",
                                    turns, err
                                )),
                                tool_calls: None,
                                tool_call_id: None,
                            },
                        )
                        .await;
                        results.push(SubTaskResult {
                            subtask_id: turns,
                            action: intent.action.clone(),
                            target: intent.target.clone(),
                            status: TaskStatus::Failed {
                                exit_code: None,
                                stderr: err,
                            },
                            elapsed: Duration::ZERO,
                            tokens_used: tokens,
                        });
                        continue;
                    }
                    match self.remember_skill(intent.payload.as_ref()).await {
                        Ok(skill_block) => {
                            push_context(&context_trace, LlmMessage { role: "system".into(), content: Some(format!(
                                "Turn {} INTENT: memory:learn | TARGET: procedural\nOBSERVATION: SUCCESS: {}",
                                turns, skill_block)), tool_calls: None, tool_call_id: None })
                            .await;
                            results.push(SubTaskResult {
                                subtask_id: turns,
                                action: intent.action.clone(),
                                target: intent.target.clone(),
                                status: TaskStatus::Success {
                                    stdout: format!("Learned procedural skill.\n{}", skill_block),
                                },
                                elapsed: Duration::ZERO,
                                tokens_used: tokens,
                            });
                        }
                        Err(err) => {
                            let err = err.to_string();
                            push_context(
                                &context_trace,
                                LlmMessage {
                                    role: "system".into(),
                                    content: Some(format!(
                                        "Turn {} ERROR: {}. Fix the payload and continue.",
                                        turns, err
                                    )),
                                    tool_calls: None,
                                    tool_call_id: None,
                                },
                            )
                            .await;
                            results.push(SubTaskResult {
                                subtask_id: turns,
                                action: intent.action.clone(),
                                target: intent.target.clone(),
                                status: TaskStatus::Failed {
                                    exit_code: None,
                                    stderr: err,
                                },
                                elapsed: Duration::ZERO,
                                tokens_used: tokens,
                            });
                        }
                    }
                    continue;
                }

                let start = std::time::Instant::now();
                let mut status = self
                    .worker
                    .execute_capability_task(
                        &intent,
                        root_node_id,
                        &session_id,
                        &agent_label,
                        Arc::clone(&high_risk_approvals),
                        execution_world_state.clone(),
                    )
                    .await?;

                // ── Capture capability denials into workflow-wide cache ──────────
                if let TaskStatus::Failed { stderr, .. } = &status {
                    if stderr.contains("CAPABILITY DENIED")
                        || stderr.contains("PERMISSION DENIED")
                        || stderr.contains("not allowed for swarm agent")
                    {
                        if let Some(ref state_lock) = execution_world_state {
                            let mut state = state_lock.write().await;
                            state.record_denial(&intent.action, &intent.target);
                        }

                        // ELEVATE TO USER ASK CHOICE
                        if let Some(bridge) = &self.worker.consent_bridge {
                            let forced = AskChoicePayload {
                                context: format!(
                                    "The action '{}' was denied: {}. How would you like me to proceed?",
                                    intent.action, stderr
                                ),
                                options: vec!["Acknowledge and proceed".into(), "Cancel task".into()],
                            };
                            let choice_request_id = uuid::Uuid::new_v4().to_string();
                            if let Some(tx) = &status_tx {
                                let _ = tx
                                    .send(Self::ask_choice_telemetry(&forced, &choice_request_id))
                                    .await;
                                let _ = tx
                                    .send(
                                        "⏸️ Waiting for user choice on denied capability..."
                                            .to_string(),
                                    )
                                    .await;
                            }
                            let selection = bridge.request_user_choice(&choice_request_id).await;
                            let observation = if selection.is_empty() {
                                "[USER CHOICE]: (no selection — user dismissed)".to_string()
                            } else {
                                format!("[USER CHOICE]: \"{}\"", selection)
                            };

                            status = TaskStatus::Success {
                                stdout: format!("CAPABILITY DENIED. {}", observation),
                            };
                        }
                    }
                }

                let status_str = match &status {
                    TaskStatus::Success { stdout } => format!("SUCCESS: {}", stdout),
                    TaskStatus::SoftDenial { reason } => format!(
                        "SOFT DENIAL: {}. Do NOT retry this action with synonyms or rephrased queries. \
                         Accept this result and proceed with 'done' or an alternative approach.",
                        reason
                    ),
                    TaskStatus::Failed { stderr, .. } => format!("FAILED: {}", stderr),
                    TaskStatus::Blocked { .. } => "BLOCKED".into(),
                    TaskStatus::Cancelled { stage } => format!("CANCELLED at: {}", stage),
                };

                // ── Record SoftDenial in DAG denial cache ───────────────────
                if let TaskStatus::SoftDenial { .. } = &status {
                    if let Some(ref state_lock) = execution_world_state {
                        let mut state = state_lock.write().await;
                        state.record_denial(&intent.action, &intent.target);
                    }
                }

                // ── Hermes-inspired observation truncation ─────────────────
                // Cap observation text to prevent any single tool output from
                // blowing up the context window. Hermes uses enforce_turn_budget
                // with similar limits per tool result.
                const MAX_OBSERVATION_CHARS: usize = 1500;
                let truncated_status = if status_str.len() > MAX_OBSERVATION_CHARS {
                    let clipped: String = status_str.chars().take(MAX_OBSERVATION_CHARS).collect();
                    format!(
                        "{}… [truncated {} chars]",
                        clipped,
                        status_str.len() - MAX_OBSERVATION_CHARS
                    )
                } else {
                    status_str.clone()
                };
                push_context(
                    &context_trace,
                    LlmMessage {
                        role: "system".into(),
                        content: Some(format!(
                            "Turn {} INTENT: {} | TARGET: {}\nOBSERVATION: {}",
                            turns, intent.action, intent.target, truncated_status
                        )),
                        tool_calls: None,
                        tool_call_id: None,
                    },
                )
                .await;

                match loop_detector.analyze(LoopDetector::hash_execution(&intent, &status)) {
                    LoopVerdict::Clear => {}
                    LoopVerdict::SoftWarn { message } => {
                        push_context(
                            &context_trace,
                            LlmMessage {
                                role: "system".into(),
                                content: Some(format!("Turn {} LOOP WARNING: {}", turns, message)),
                                tool_calls: None,
                                tool_call_id: None,
                            },
                        )
                        .await;
                    }
                    LoopVerdict::HardStop { message } => {
                        results.push(SubTaskResult {
                            subtask_id: turns,
                            action: "done".into(),
                            target: "user".into(),
                            status: TaskStatus::Success { stdout: message },
                            elapsed: start.elapsed(),
                            tokens_used: tokens,
                        });
                        break;
                    }
                }

                results.push(SubTaskResult {
                    subtask_id: turns,
                    action: intent.action.clone(),
                    target: intent.target.clone(),
                    status,
                    elapsed: start.elapsed(),
                    tokens_used: tokens,
                });
            }

            Ok(ReactOutcome::Completed(results))
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// § Chat Agent
// ─────────────────────────────────────────────────────────────────────────────

pub struct ChatConfig {
    pub model: String,
    pub max_tokens: usize,
    pub temperature: f32,
}

pub struct ChatAgent {
    pub config: ChatConfig,
    pub provider: Arc<dyn LlmProvider>,
    model_override: Arc<std::sync::RwLock<String>>,
}

#[cfg(not(test))]
const CHAT_REPLY_TIMEOUT: Duration = Duration::from_secs(180);
#[cfg(test)]
const CHAT_REPLY_TIMEOUT: Duration = Duration::from_millis(50);
#[cfg(not(test))]
const PLANNER_REPLY_TIMEOUT: Duration = Duration::from_secs(180);
#[cfg(test)]
const PLANNER_REPLY_TIMEOUT: Duration = Duration::from_millis(50);

impl ChatAgent {
    pub fn new(config: ChatConfig, provider: Arc<dyn LlmProvider>) -> Self {
        let model_override = Arc::new(std::sync::RwLock::new(config.model.clone()));
        Self {
            config,
            provider,
            model_override,
        }
    }

    pub fn set_model(&self, model: impl Into<String>) {
        if let Ok(mut slot) = self.model_override.write() {
            *slot = model.into();
        }
    }

    pub fn current_model(&self) -> String {
        self.model_override
            .read()
            .map(|slot| slot.clone())
            .unwrap_or_else(|_| self.config.model.clone())
    }

    pub async fn reply(
        &self,
        query: &str,
        memory_context_xml: &str,
        memories: &[String],
    ) -> MoatResult<(String, u32)> {
        self.reply_streaming(query, memory_context_xml, memories, None)
            .await
    }

    /// Streaming variant: pipes token deltas through `tx` as they arrive
    /// instead of waiting for the full response. This eliminates TTFB
    /// latency on conversational replies — matching Hermes's always-stream
    /// pattern.
    pub async fn reply_streaming(
        &self,
        query: &str,
        memory_context_xml: &str,
        memories: &[String],
        tx: Option<tokio::sync::mpsc::Sender<String>>,
    ) -> MoatResult<(String, u32)> {
        let memory_block = if memories.is_empty() {
            "No relevant durable knowledge recalled.".to_string()
        } else {
            memories.join("\n")
        };
        let user_turn = format!("{}\n{}\n{}", memory_context_xml.trim(), memory_block, query);
        let model = self.current_model();
        let system_prompt = "You are MoatAgeex, an autonomous macOS agent. \
            You are currently in a text-only chat mode without tool access. DO NOT invent tools. \
            DO NOT output '# Session Summary' or recap past events. Answer the user directly and concisely.";
        timeout(
            CHAT_REPLY_TIMEOUT,
            self.provider.complete_stream(
                &model,
                system_prompt,
                &user_turn,
                self.config.max_tokens,
                self.config.temperature,
                tx,
            ),
        )
        .await
        .map_err(|_| {
            MoatError::Orchestrator(format!(
                "chat provider timed out after {}s",
                CHAT_REPLY_TIMEOUT.as_secs()
            ))
        })?
    }

    pub fn into_result(text: String, tokens: u32, elapsed: Duration) -> SubTaskResult {
        SubTaskResult {
            subtask_id: 0,
            action: "chat".into(),
            target: "user".into(),
            status: TaskStatus::Success { stdout: text },
            elapsed,
            tokens_used: tokens,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::authorize_path;
    use super::{
        ActionIntent, ChatAgent, ChatConfig, LlmMessage, LlmProvider, LlmResponse, LlmTool,
        LlmToolCall, Orchestrator, PlannerAgent, PlannerConfig, SubTaskResult, TaskStatus,
        WorkerAgent, WorkerConfig, CHAT_REPLY_TIMEOUT, PLANNER_REPLY_TIMEOUT,
        REACT_SYSTEM_TEMPLATE,
    };
    use crate::config;
    use crate::consent::{ConsentBridge, ConsentDecision, ConsentRequest};
    use crate::external_api::ExternalApiConfig;
    use crate::injector::InjectorConfig;
    use crate::memory::{self, KnowledgeKind};
    use crate::meta_brain::{MetaBrain, PermissionFlags, SYSTEM_ROOT_ID};

    use crate::MoatResult;
    use rusqlite::Connection;
    use serde_json::{json, Value};
    use std::collections::VecDeque;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::{Mutex as AsyncMutex, RwLock as AsyncRwLock};

    fn temp_dir(name: &str) -> PathBuf {
        let root = config::workspace_root().join("test_scratch");
        fs::create_dir_all(&root).expect("create workspace test root");
        let p = root.join(format!(
            "moat_orchestrator_test_{}_{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).expect("create temp test dir");
        p
    }

    fn prepare_test_db(path: &PathBuf) -> Arc<AsyncMutex<Connection>> {
        let conn = Connection::open(path).expect("open sqlite db");
        memory::prepare_tables(&conn).expect("prepare knowledge tables");
        Arc::new(AsyncMutex::new(conn))
    }

    fn consent_bridge_with_policy<F>(
        policy: F,
        counter: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<ConsentRequest>>>,
    ) -> Arc<ConsentBridge>
    where
        F: Fn(usize, &ConsentRequest) -> bool + Send + Sync + 'static,
    {
        let bridge_slot = Arc::new(Mutex::new(None::<std::sync::Weak<ConsentBridge>>));
        let bridge_slot_for_emit = Arc::clone(&bridge_slot);
        let policy = Arc::new(policy);
        let bridge = Arc::new(ConsentBridge::new(Box::new(move |req: ConsentRequest| {
            let approval_index = counter.fetch_add(1, Ordering::SeqCst) + 1;
            requests.lock().expect("lock requests").push(req.clone());
            let approved = policy(approval_index, &req);
            let weak = bridge_slot_for_emit
                .lock()
                .expect("lock bridge slot")
                .clone()
                .expect("bridge stored");
            std::thread::spawn(move || {
                if let Some(bridge) = weak.upgrade() {
                    let decision = if approved {
                        ConsentDecision::ApproveOnce
                    } else {
                        ConsentDecision::Deny
                    };
                    let _ = bridge.resolve(&req.request_id, decision);
                }
            });
        })));
        *bridge_slot.lock().expect("lock bridge slot") = Some(Arc::downgrade(&bridge));
        bridge
    }

    fn test_worker(
        meta_brain: Arc<AsyncRwLock<MetaBrain>>,
        db: Arc<AsyncMutex<Connection>>,
        consent_bridge: Option<Arc<ConsentBridge>>,
    ) -> WorkerAgent {
        let config = WorkerConfig {
            model: "test-model".into(),
            command_timeout: Duration::from_secs(1),
            allow_high_risk: true,
            external_apis: ExternalApiConfig::default(),
        };
        match consent_bridge {
            Some(bridge) => WorkerAgent::with_consent_bridge(config, meta_brain, db, bridge),
            None => WorkerAgent::new(config, meta_brain, db),
        }
    }

    fn test_injector_config() -> InjectorConfig {
        InjectorConfig {
            top_k_memories: 3,
            max_memory_chars: 512,
        }
    }

    struct StubProvider {
        responses: Arc<Mutex<VecDeque<(String, u32)>>>,
    }

    impl StubProvider {
        fn new(responses: Vec<(String, u32)>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
            }
        }
    }

    impl LlmProvider for StubProvider {
        fn chat_complete<'a>(
            &'a self,
            _model: &'a str,
            _messages: Vec<LlmMessage>,
            _tools: Option<Vec<LlmTool>>,
            _max_tokens: usize,
            _temperature: f32,
        ) -> super::BoxFuture<'a, MoatResult<LlmResponse>> {
            Box::pin(async move {
                let next = self
                    .responses
                    .lock()
                    .expect("lock stub responses")
                    .pop_front()
                    .expect("stub response");

                let cleaned = next
                    .0
                    .replace("<action>", "")
                    .replace("</action>", "")
                    .trim()
                    .to_string();
                let intent: ActionIntent =
                    serde_json::from_str(&cleaned).unwrap_or_else(|_| ActionIntent {
                        thought: None,
                        action: "done".into(),
                        target: "user".into(),
                        payload: Some(serde_json::Value::String(cleaned)),
                    });

                Ok(LlmResponse {
                    content: None,
                    tool_calls: Some(vec![LlmToolCall {
                        id: "call_1".into(),
                        type_: "function".into(),
                        function: crate::orchestrator::LlmFunctionCall {
                            name: intent.action,
                            arguments: serde_json::json!({"target": intent.target, "payload": intent.payload}).to_string()
                        }
                    }]),
                    tokens: next.1,
                })
            })
        }

        fn complete<'a>(
            &'a self,
            _model: &'a str,
            _system: &'a str,
            _user: &'a str,
            _max_tokens: usize,
            _temperature: f32,
        ) -> super::BoxFuture<'a, MoatResult<(String, u32)>> {
            Box::pin(async move {
                let next = self
                    .responses
                    .lock()
                    .expect("lock stub responses")
                    .pop_front()
                    .expect("stub response");
                Ok(next)
            })
        }
    }

    struct HangingProvider;

    impl LlmProvider for HangingProvider {
        fn chat_complete<'a>(
            &'a self,
            _model: &'a str,
            _messages: Vec<LlmMessage>,
            _tools: Option<Vec<LlmTool>>,
            _max_tokens: usize,
            _temperature: f32,
        ) -> super::BoxFuture<'a, MoatResult<LlmResponse>> {
            Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_secs(31)).await;
                Ok(LlmResponse {
                    content: Some("late".into()),
                    tool_calls: None,
                    tokens: 1,
                })
            })
        }

        fn complete<'a>(
            &'a self,
            _model: &'a str,
            _system: &'a str,
            _user: &'a str,
            _max_tokens: usize,
            _temperature: f32,
        ) -> super::BoxFuture<'a, MoatResult<(String, u32)>> {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_secs(31)).await;
                Ok(("late".into(), 1))
            })
        }
    }

    struct HangingPlannerProvider;

    impl LlmProvider for HangingPlannerProvider {
        fn chat_complete<'a>(
            &'a self,
            _model: &'a str,
            _messages: Vec<LlmMessage>,
            _tools: Option<Vec<LlmTool>>,
            _max_tokens: usize,
            _temperature: f32,
        ) -> super::BoxFuture<'a, MoatResult<LlmResponse>> {
            Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                Ok(LlmResponse {
                    content: None,
                    tool_calls: Some(vec![LlmToolCall {
                        id: "call_2".into(),
                        type_: "function".into(),
                        function: crate::orchestrator::LlmFunctionCall {
                            name: "done".into(),
                            arguments: serde_json::json!({"target": "user", "payload": "late"})
                                .to_string(),
                        },
                    }]),
                    tokens: 1,
                })
            })
        }

        fn complete<'a>(
            &'a self,
            _model: &'a str,
            _system: &'a str,
            _user: &'a str,
            _max_tokens: usize,
            _temperature: f32,
        ) -> super::BoxFuture<'a, MoatResult<(String, u32)>> {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok((
                    "{\"action\":\"done\",\"target\":\"user\",\"payload\":\"late\"}".into(),
                    1,
                ))
            })
        }
    }

    #[test]
    fn authorize_write_blocks_prefix_confusion() {
        let base = temp_dir("prefix");
        let root = base.join("allowed");
        let evil = base.join("allowed_evil");
        fs::create_dir_all(&root).expect("create allowed dir");
        fs::create_dir_all(&evil).expect("create evil dir");

        let target = evil.join("x.txt");
        let result = authorize_path(
            target.to_str().expect("target path"),
            &[root.to_string_lossy().into_owned()],
            true,
        );
        assert!(result.is_err(), "prefix-confused path should be denied");
    }

    #[cfg(unix)]
    #[test]
    fn authorize_read_blocks_symlink_escape() {
        use std::os::unix::fs::symlink;

        let base = temp_dir("symlink");
        let root = base.join("allowed");
        let outside = base.join("outside");
        fs::create_dir_all(&root).expect("create allowed dir");
        fs::create_dir_all(&outside).expect("create outside dir");

        let outside_file = outside.join("secret.txt");
        fs::write(&outside_file, "top-secret").expect("write outside file");

        let link_path = root.join("link_out");
        symlink(&outside, &link_path).expect("create symlink");

        let escaped = link_path.join("secret.txt");
        let result = authorize_path(
            escaped.to_str().expect("escaped path"),
            &[root.to_string_lossy().into_owned()],
            false,
        );
        assert!(result.is_err(), "symlink escape must be denied");
    }

    #[test]
    fn can_open_url_accepts_mailto_and_denies_unlisted_schemes() {
        let mut brain = MetaBrain::new();
        brain
            .grant_url_scheme_permission("mailto", PermissionFlags::URL_OPEN)
            .expect("grant mailto");

        assert!(brain.url_scheme_has_permission("mailto", PermissionFlags::URL_OPEN));
        assert!(!brain.url_scheme_has_permission("ftp", PermissionFlags::URL_OPEN));
    }

    #[tokio::test]
    async fn file_write_permission_persists_and_skips_second_prompt() {
        let base = temp_dir("file_write_e2e");
        let db_path = base.join("memory.sqlite");
        let target = base.join("notes.txt");
        let mut brain = MetaBrain::new();
        brain.attach_persistence(db_path.with_extension("bin"));
        let meta_brain = Arc::new(AsyncRwLock::new(brain));
        let db = prepare_test_db(&db_path);
        let approval_count = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let bridge = consent_bridge_with_policy(
            |_index, _req| true,
            Arc::clone(&approval_count),
            Arc::clone(&requests),
        );
        let worker = test_worker(Arc::clone(&meta_brain), Arc::clone(&db), Some(bridge));
        let intent = ActionIntent {
            thought: None,
            action: "fs:write".into(),
            target: target.to_string_lossy().into_owned(),
            payload: Some(json!("hello from moat")),
        };

        let first = worker
            .execute_capability_task(
                &intent,
                SYSTEM_ROOT_ID,
                "test-session",
                "tester",
                Arc::new(AtomicUsize::new(0)),
                None,
            )
            .await
            .expect("first write");
        assert!(matches!(first, TaskStatus::Success { .. }));
        assert_eq!(approval_count.load(Ordering::SeqCst), 1);

        let second = worker
            .execute_capability_task(
                &intent,
                SYSTEM_ROOT_ID,
                "test-session",
                "tester",
                Arc::new(AtomicUsize::new(0)),
                None,
            )
            .await
            .expect("second write");
        assert!(matches!(second, TaskStatus::Success { .. }));
        assert_eq!(approval_count.load(Ordering::SeqCst), 1);

        let parent = target
            .parent()
            .expect("parent")
            .to_string_lossy()
            .into_owned();
        let brain = meta_brain.read().await;
        assert!(brain.path_has_permission(&parent, PermissionFlags::FILE_WRITE));
        drop(brain);

        let persisted = MetaBrain::load_or_new(db_path.with_extension("bin"))
            .await
            .expect("load persisted brain");
        assert!(persisted.path_has_permission(&parent, PermissionFlags::FILE_WRITE));
        assert_eq!(requests.lock().expect("lock requests").len(), 1);
    }

    #[tokio::test]
    async fn file_read_permission_persists_and_skips_second_prompt() {
        let base = temp_dir("file_read_e2e");
        let db_path = base.join("memory.sqlite");
        let target = base.join("readme.txt");
        fs::write(&target, "read me").expect("seed read file");
        let mut brain = MetaBrain::new();
        brain.attach_persistence(db_path.with_extension("bin"));
        let meta_brain = Arc::new(AsyncRwLock::new(brain));
        let db = prepare_test_db(&db_path);
        let approval_count = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let bridge = consent_bridge_with_policy(
            |_index, _req| true,
            Arc::clone(&approval_count),
            Arc::clone(&requests),
        );
        let worker = test_worker(Arc::clone(&meta_brain), Arc::clone(&db), Some(bridge));
        let intent = ActionIntent {
            thought: None,
            action: "fs:read".into(),
            target: target.to_string_lossy().into_owned(),
            payload: None,
        };

        let first = worker
            .execute_capability_task(
                &intent,
                SYSTEM_ROOT_ID,
                "test-session",
                "tester",
                Arc::new(AtomicUsize::new(0)),
                None,
            )
            .await
            .expect("first read");
        assert!(matches!(first, TaskStatus::Success { .. }));
        assert_eq!(approval_count.load(Ordering::SeqCst), 1);

        let second = worker
            .execute_capability_task(
                &intent,
                SYSTEM_ROOT_ID,
                "test-session",
                "tester",
                Arc::new(AtomicUsize::new(0)),
                None,
            )
            .await
            .expect("second read");
        assert!(matches!(second, TaskStatus::Success { .. }));
        assert_eq!(approval_count.load(Ordering::SeqCst), 1);

        let brain = meta_brain.read().await;
        assert!(brain.path_has_permission(
            target.to_string_lossy().as_ref(),
            PermissionFlags::FILE_READ
        ));
        assert_eq!(requests.lock().expect("lock requests").len(), 1);
    }

    #[tokio::test]
    async fn chat_agent_reply_times_out_when_provider_stalls() {
        let agent = ChatAgent::new(
            ChatConfig {
                model: "stub".into(),
                max_tokens: 64,
                temperature: 0.0,
            },
            Arc::new(HangingProvider),
        );

        let err = agent
            .reply("hi", "<knowledge_memory/>", &[])
            .await
            .expect_err("stalled provider should time out");

        assert!(
            err.to_string().contains(&format!(
                "chat provider timed out after {}s",
                CHAT_REPLY_TIMEOUT.as_secs()
            )),
            "unexpected timeout error: {err}"
        );
    }

    #[tokio::test]
    async fn planner_agent_times_out_when_provider_stalls() {
        let planner = PlannerAgent::new(
            PlannerConfig {
                model: "stub".into(),
                max_subtasks: 4,
                temperature: 0.0,
                max_tokens: 64,
            },
            Arc::new(HangingPlannerProvider),
        );

        let err = planner
            .generate_intent_raw_with_system(REACT_SYSTEM_TEMPLATE, "Turn 1: hi", None)
            .await
            .expect_err("stalled planner provider should time out");

        assert!(
            err.to_string().contains(&format!(
                "planner provider timed out after {}s",
                PLANNER_REPLY_TIMEOUT.as_secs()
            )),
            "unexpected timeout error: {err}"
        );
    }

    #[tokio::test]
    async fn domain_permission_stays_scoped_to_approved_branch() {
        let base = temp_dir("domain_scope_e2e");
        let db_path = base.join("memory.sqlite");
        let mut brain = MetaBrain::new();
        brain.attach_persistence(db_path.with_extension("bin"));
        let meta_brain = Arc::new(AsyncRwLock::new(brain));
        let db = prepare_test_db(&db_path);
        let approval_count = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let bridge = consent_bridge_with_policy(
            |index, _req| index == 1,
            Arc::clone(&approval_count),
            Arc::clone(&requests),
        );
        let worker = test_worker(Arc::clone(&meta_brain), Arc::clone(&db), Some(bridge));
        let allowed_intent = ActionIntent {
            thought: None,
            action: "net:curl".into(),
            target: "https://api.github.com/".into(),
            payload: Some(json!({ "method": "GET" })),
        };
        let sibling_intent = ActionIntent {
            thought: None,
            action: "net:curl".into(),
            target: "https://example.com/".into(),
            payload: Some(json!({ "method": "GET" })),
        };

        let _ = worker
            .execute_capability_task(
                &allowed_intent,
                SYSTEM_ROOT_ID,
                "test-session",
                "tester",
                Arc::new(AtomicUsize::new(0)),
                None,
            )
            .await
            .expect("first curl");
        assert_eq!(approval_count.load(Ordering::SeqCst), 1);

        let _ = worker
            .execute_capability_task(
                &allowed_intent,
                SYSTEM_ROOT_ID,
                "test-session",
                "tester",
                Arc::new(AtomicUsize::new(0)),
                None,
            )
            .await
            .expect("second curl same domain");
        assert_eq!(approval_count.load(Ordering::SeqCst), 1);

        let denied = worker
            .execute_capability_task(
                &sibling_intent,
                SYSTEM_ROOT_ID,
                "test-session",
                "tester",
                Arc::new(AtomicUsize::new(0)),
                None,
            )
            .await
            .expect("sibling domain curl");
        assert_eq!(approval_count.load(Ordering::SeqCst), 2);
        match denied {
            TaskStatus::Failed { stderr, .. } => {
                assert!(stderr.contains("example.com") || stderr.contains("denied"));
            }
            other => panic!("expected denied sibling domain, got {other:?}"),
        }

        let brain = meta_brain.read().await;
        let subtree = brain
            .focused_subgraph(SYSTEM_ROOT_ID, "curl api github", 4, 3)
            .expect("extract subtree");
        assert!(subtree
            .allowed_domains
            .iter()
            .any(|domain| domain == "api.github.com"));
        assert!(!subtree
            .allowed_domains
            .iter()
            .any(|domain| domain == "example.com"));
    }

    #[tokio::test]
    async fn meta_query_brain_returns_focused_permission_context() {
        let base = temp_dir("meta_query_brain");
        let db_path = base.join("memory.sqlite");
        let mut seed_brain = MetaBrain::new();
        seed_brain.attach_persistence(db_path.with_extension("bin"));
        let meta_brain = Arc::new(AsyncRwLock::new(seed_brain));
        {
            let mut brain = meta_brain.write().await;
            brain
                .grant_path_permission("/workspace/src", PermissionFlags::FILE_READ)
                .expect("grant src");
            brain
                .grant_path_permission("/workspace/docs", PermissionFlags::FILE_READ)
                .expect("grant docs");
        }
        let db = prepare_test_db(&db_path);
        let worker = test_worker(Arc::clone(&meta_brain), Arc::clone(&db), None);
        let planner = PlannerAgent::new(
            PlannerConfig {
                model: "stub".into(),
                max_subtasks: 4,
                temperature: 0.0,
                max_tokens: 256,
            },
            Arc::new(StubProvider::new(vec![
                (
                    r#"{"action":"meta:query_brain","target":"metabrain","payload":"src"}"#.into(),
                    11,
                ),
                (
                    r#"{"action":"done","target":"user","payload":"done"}"#.into(),
                    7,
                ),
            ])),
        );
        let orchestrator = Orchestrator::new(
            planner,
            worker,
            Arc::clone(&meta_brain),
            Arc::clone(&db),
            test_injector_config(),
        );

        let results: Vec<SubTaskResult> = orchestrator
            .run_react("Inspect the src area", "bootstrap", SYSTEM_ROOT_ID, None)
            .await
            .expect("run react");

        let meta_query = &results[0];
        assert_eq!(meta_query.action, "meta:query_brain");
        match &meta_query.status {
            TaskStatus::Success { stdout } => {
                assert!(stdout.contains("(allow file-read* (subpath \"/workspace/src\"))"));
                assert!(!stdout.contains("/workspace/docs"));
            }
            other => panic!("expected successful meta query, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ask_choice_yields_control_with_formatted_options() {
        let base = temp_dir("ask_choice");
        let db_path = base.join("memory.sqlite");
        let mut brain = MetaBrain::new();
        brain.attach_persistence(db_path.with_extension("bin"));
        let meta_brain = Arc::new(AsyncRwLock::new(brain));
        let db = prepare_test_db(&db_path);
        let worker = test_worker(Arc::clone(&meta_brain), Arc::clone(&db), None);
        let planner = PlannerAgent::new(
            PlannerConfig {
                model: "stub".into(),
                max_subtasks: 3,
                temperature: 0.0,
                max_tokens: 256,
            },
            Arc::new(StubProvider::new(vec![(
                r#"{"action":"user:ask_choice","target":"user","payload":{"context":"I found two ways to do this.","options":["Option A: use curl","Option B: write a python script"]}}"#
                    .into(),
                9,
            )])),
        );
        let orchestrator = Orchestrator::new(
            planner,
            worker,
            Arc::clone(&meta_brain),
            Arc::clone(&db),
            test_injector_config(),
        );

        let results = orchestrator
            .run_react("Choose a strategy", "bootstrap", SYSTEM_ROOT_ID, None)
            .await
            .expect("run react");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].action, "user:ask_choice");
        match &results[0].status {
            TaskStatus::Success { stdout } => {
                assert!(stdout.contains("I found two ways to do this."));
                assert!(stdout.contains("1. Option A: use curl"));
                assert!(stdout.contains("2. Option B: write a python script"));
            }
            other => panic!("expected successful ask_choice, got {other:?}"),
        }
    }

    #[test]
    fn ask_choice_telemetry_is_clean_json() {
        let parsed = Orchestrator::parse_ask_choice_payload(Some(&serde_json::json!({
            "context": "I found two ways to do this.",
            "options": ["Option A: use curl", "Option B: write a python script"]
        })))
        .expect("parse ask choice payload");

        let telemetry = Orchestrator::ask_choice_telemetry(&parsed, "test-request-id");
        let v: serde_json::Value = serde_json::from_str(&telemetry).expect("valid json");
        assert_eq!(v["type"], "ask_choice");
        assert_eq!(v["request_id"], "test-request-id");
        assert_eq!(v["context"], "I found two ways to do this.");
        assert_eq!(v["options"][0], "Option A: use curl");
        assert_eq!(v["options"][1], "Option B: write a python script");
    }

    #[tokio::test]
    async fn swarm_delegate_blocks_when_depth_exceeded() {
        let base = temp_dir("delegate_depth");
        let db_path = base.join("memory.sqlite");
        let mut brain = MetaBrain::new();
        brain.attach_persistence(db_path.with_extension("bin"));
        let meta_brain = Arc::new(AsyncRwLock::new(brain));
        let db = prepare_test_db(&db_path);
        let worker = test_worker(Arc::clone(&meta_brain), Arc::clone(&db), None);
        let planner = PlannerAgent::new(
            PlannerConfig {
                model: "stub".into(),
                max_subtasks: 2,
                temperature: 0.0,
                max_tokens: 128,
            },
            Arc::new(StubProvider::new(vec![
                (
                    r#"{"action":"swarm:delegate","target":"swarm","payload":{"target_agent_id":"coder","task":"inspect src/lib.rs"}}"#
                        .into(),
                    8,
                ),
                (
                    r#"{"action":"done","target":"user","payload":"resolved locally"}"#
                        .into(),
                    6,
                ),
            ])),
        );
        let orchestrator = Orchestrator::new(
            planner,
            worker,
            Arc::clone(&meta_brain),
            Arc::clone(&db),
            test_injector_config(),
        );

        let results = orchestrator
            .run_react_scoped(
                "delegate",
                "bootstrap",
                SYSTEM_ROOT_ID,
                None,
                2,
                Arc::new(AtomicUsize::new(0)),
            )
            .await
            .expect("run react scoped");

        assert!(!results.is_empty());
        match &results[0].status {
            TaskStatus::Failed { stderr, .. } => {
                assert!(stderr.contains("MAX_SWARM_DEPTH exceeded"));
            }
            other => panic!("expected failed delegation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn swarm_delegate_forces_user_yield_when_limit_exceeded() {
        let base = temp_dir("delegate_limit");
        let db_path = base.join("memory.sqlite");
        let mut brain = MetaBrain::new();
        brain.attach_persistence(db_path.with_extension("bin"));
        let meta_brain = Arc::new(AsyncRwLock::new(brain));
        let db = prepare_test_db(&db_path);
        let worker = test_worker(Arc::clone(&meta_brain), Arc::clone(&db), None);
        let planner = PlannerAgent::new(
            PlannerConfig {
                model: "stub".into(),
                max_subtasks: 2,
                temperature: 0.0,
                max_tokens: 128,
            },
            Arc::new(StubProvider::new(vec![(
                r#"{"action":"swarm:delegate","target":"swarm","payload":{"target_agent_id":"coder","task":"inspect src/lib.rs"}}"#
                    .into(),
                8,
            )])),
        );
        let orchestrator = Orchestrator::new(
            planner,
            worker,
            Arc::clone(&meta_brain),
            Arc::clone(&db),
            test_injector_config(),
        );

        let results = orchestrator
            .run_react_scoped(
                "delegate",
                "bootstrap",
                SYSTEM_ROOT_ID,
                None,
                0,
                Arc::new(AtomicUsize::new(super::MAX_DELEGATE_TURNS)),
            )
            .await
            .expect("run react scoped");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].action, "user:ask_choice");
        match &results[0].status {
            TaskStatus::Success { stdout } => {
                assert!(stdout.contains("Delegation limit"));
            }
            other => panic!("expected forced yield, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn disallowed_swarm_tool_is_blocked_by_agent_policy() {
        let base = temp_dir("agent_tool_policy");
        let db_path = base.join("memory.sqlite");
        let mut brain = MetaBrain::new();
        brain.attach_persistence(db_path.with_extension("bin"));
        let meta_brain = Arc::new(AsyncRwLock::new(brain));
        let db = prepare_test_db(&db_path);
        let worker = test_worker(Arc::clone(&meta_brain), Arc::clone(&db), None);
        let planner = PlannerAgent::new(
            PlannerConfig {
                model: "stub".into(),
                max_subtasks: 2,
                temperature: 0.0,
                max_tokens: 128,
            },
            Arc::new(StubProvider::new(vec![
                (
                    r#"{"action":"fs:read","target":"/tmp/demo.txt","payload":null}"#.into(),
                    8,
                ),
                (
                    r#"{"action":"done","target":"user","payload":"stopped"}"#.into(),
                    6,
                ),
            ])),
        );
        let orchestrator = Orchestrator::new(
            planner,
            worker,
            Arc::clone(&meta_brain),
            Arc::clone(&db),
            test_injector_config(),
        )
        .with_agent_spec(crate::swarm::AgentSpec {
            name: "router".into(),
            role: "routes".into(),
            api_provider: "nvidia".into(),
            model_id: "meta/llama-3.1-70b-instruct".into(),
            allowed_tools: vec![
                "swarm:delegate".into(),
                "done".into(),
                "user:ask_choice".into(),
            ],
            instructions: "route only".into(),
            raw_markdown: "## router".into(),
        });

        let results = orchestrator
            .run_react("inspect the file", "bootstrap", SYSTEM_ROOT_ID, None)
            .await
            .expect("run react");

        assert!(!results.is_empty());
        match &results[0].status {
            TaskStatus::Failed { stderr, .. } => {
                assert!(stderr.contains("not allowed for swarm agent 'router'"));
            }
            other => panic!("expected agent policy failure, got {other:?}"),
        }
    }

    #[test]
    fn parse_intent_falls_back_safely_when_unrecoverable() {
        let intent = PlannerAgent::parse_intent("totally malformed planner output");
        assert_eq!(intent.action, "system:parse_error");
        assert_eq!(intent.target, "parser");
        assert!(matches!(intent.payload, Some(Value::String(_))));
    }

    #[test]
    fn normalize_result_string_strips_dynamic_runtime_noise() {
        let raw =
            "2026-04-14T10:44:55Z pid=4821 retry 2/3 after 15ms UUID 123e4567-e89b-12d3-a456-426614174000";
        let normalized = super::normalize_result_string(raw);
        assert!(!normalized.contains("2026-04-14"));
        assert!(!normalized.contains("4821"));
        assert!(!normalized.contains("123e4567-e89b-12d3-a456-426614174000"));
        assert!(normalized.contains("<TIMESTAMP>"));
        assert!(normalized.contains("<PID>"));
        assert!(normalized.contains("<COUNTER>"));
        assert!(normalized.contains("<ELAPSED>"));
        assert!(normalized.contains("<UUID>"));
    }

    #[test]
    fn loop_detector_hash_ignores_timestamp_only_stderr_differences() {
        let intent = ActionIntent {
            thought: None,
            action: "process:spawn".into(),
            target: "cargo".into(),
            payload: Some(json!(["test"])),
        };
        let first = TaskStatus::Failed {
            exit_code: Some(1),
            stderr: "2026-04-14T10:44:55Z pid=4821 retry 1/3 after 15ms".into(),
        };
        let second = TaskStatus::Failed {
            exit_code: Some(1),
            stderr: "2026-04-14T10:45:01Z pid=5199 retry 2/3 after 27ms".into(),
        };

        let first_hash = super::LoopDetector::hash_execution(&intent, &first);
        let second_hash = super::LoopDetector::hash_execution(&intent, &second);

        assert_eq!(first_hash.call_hash, second_hash.call_hash);
        assert_eq!(first_hash.result_hash, second_hash.result_hash);
    }

    #[tokio::test]
    async fn memory_learn_persists_skill_to_hdc_and_sqlite() {
        let base = temp_dir("memory_learn");
        let db_path = base.join("memory.sqlite");
        let mut brain = MetaBrain::new();
        brain.attach_persistence(db_path.with_extension("bin"));
        let meta_brain = Arc::new(AsyncRwLock::new(brain));
        let db = prepare_test_db(&db_path);
        let worker = test_worker(Arc::clone(&meta_brain), Arc::clone(&db), None);
        let planner = PlannerAgent::new(
            PlannerConfig {
                model: "stub".into(),
                max_subtasks: 4,
                temperature: 0.0,
                max_tokens: 256,
            },
            Arc::new(StubProvider::new(vec![
                (
                    r#"{"action":"memory:learn","target":"procedural","payload":{"keywords":"github api curl token","skill_description":"To authenticate with GitHub API, send header Authorization: Bearer <token>."}}"#
                        .into(),
                    11,
                ),
                (
                    r#"{"action":"done","target":"user","payload":"done"}"#.into(),
                    7,
                ),
            ])),
        );
        let orchestrator = Orchestrator::new(
            planner,
            worker,
            Arc::clone(&meta_brain),
            Arc::clone(&db),
            test_injector_config(),
        );

        let results = orchestrator
            .run_react(
                "Figure out GitHub auth syntax",
                "bootstrap",
                SYSTEM_ROOT_ID,
                None,
            )
            .await
            .expect("run react");

        assert_eq!(results[0].action, "memory:learn");
        match &results[0].status {
            TaskStatus::Success { stdout } => {
                assert!(stdout.contains("[LEARNED SKILL]"));
                assert!(stdout.contains("github api curl token"));
            }
            other => panic!("expected successful memory learn, got {other:?}"),
        }
        let conn = db.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM knowledge_memory WHERE kind = ?1 AND text LIKE '%Authorization: Bearer <token>%'",
                [KnowledgeKind::Skill.as_str()],
                |row| row.get(0),
            )
            .expect("count learned skills");
        assert_eq!(count, 1);
    }
}
enum ClipboardBackup {
    Text(String),
    Html(String),
    Image(arboard::ImageData<'static>),
    FileList(Vec<PathBuf>),
    Empty,
}
