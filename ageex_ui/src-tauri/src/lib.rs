#![allow(unexpected_cfgs)]

mod context_scraper;
mod ui_bridge_impl;
mod ui_scraper;
mod reasoning_bridge;
mod overlay;

use keyring::Entry;
use moat_core::config::{
    deepseek_account_name, default_provider_profiles, load_provider_profiles, load_user_document,
    nvidia_account_name, sqlite_db_path, user_md_path, KEYCHAIN_SERVICE_NAME, SettingsState,
    SettingsUpdate,
};
use moat_core::consent::{ConsentBridge, ConsentDecision};
use moat_core::external_api::ExternalApiConfig;
use moat_core::gateway::{
    GatewayClientRole, GatewayRpcRequest, GatewayServerEvent, GatewaySessionContext,
    GatewaySessionRecord, SessionSource,
};
use moat_core::gateway_transport::{
    GatewayServerInfo, GatewayTransportConnectRequest, GatewayTransportFrame,
    GatewayTransportInboundFrame, GatewayWireResponse, GatewayWireTaskStatus,
};
use moat_core::orchestrator::{PlannerConfig, WorkerConfig};
use moat_core::system::{MemoryOverview, MoatSystem, MoatSystemConfig};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use reasoning_bridge::{ReasoningBridge, StepOutcomePayload};
use std::time::Duration;
use tauri::{Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tokio::sync::{Mutex as AsyncMutex, watch};
use uuid::Uuid;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

const CONSENT_EVENT_NAME: &str = "require-consent";
const CONTEXT_SCRAPED_EVENT_NAME: &str = "context_scraped";
const KERNEL_READY_EVENT_NAME: &str = "kernel-ready";
const GATEWAY_SERVER_EVENT_NAME: &str = "gateway-server-event";
const KERNEL_BOOTING_ERROR: &str = "Kernel is still booting, please wait.";
const GATEWAY_DESKTOP_DEVICE_ID: &str = "tauri_desktop";

async fn resolve_stored_nvidia_api_key(
    state: &tauri::State<'_, AppState>,
) -> Result<String, String> {
    let kernel = state
        .kernel
        .lock()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| KERNEL_BOOTING_ERROR.to_string())?;
    let root = kernel.settings_swarm_root_for_ui();
    let doc = match load_user_document(root.join("USER.md")).await {
        Ok(doc) => Some(doc),
        Err(_) => load_user_document(user_md_path()).await.ok(),
    };
    if let Some(key) = doc
        .as_ref()
        .and_then(|doc| doc.frontmatter.nvidia_api_key.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(key.to_string());
    }

    if let Ok(env_key) = std::env::var("NVIDIA_API_KEY") {
        let trimmed = env_key.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    let profiles = load_provider_profiles().unwrap_or_else(|_| default_provider_profiles());
    for profile in profiles.nvidia_profiles {
        let account = nvidia_account_name(&profile.id);
        let entry = Entry::new(KEYCHAIN_SERVICE_NAME, &account)
            .map_err(|err| format!("Failed to open NVIDIA keychain entry: {err}"))?;
        match entry.get_password() {
            Ok(secret) => {
                let trimmed = secret.trim();
                if !trimmed.is_empty() {
                    return Ok(trimmed.to_string());
                }
            }
            Err(keyring::Error::NoEntry) => continue,
            Err(err) => {
                return Err(format!("Failed to read NVIDIA keychain entry: {err}"));
            }
        }
    }

    Err("No stored NVIDIA API key found.".to_string())
}

#[derive(serde::Deserialize)]
struct NvidiaModelsResponse {
    #[serde(default)]
    data: Vec<NvidiaModelDescriptor>,
}

#[derive(serde::Deserialize)]
struct NvidiaModelDescriptor {
    id: String,
}

async fn validate_nvidia_inference_request(
    api_key: &str,
    model_name: &str,
) -> Result<(), String> {
    let trimmed_model = model_name.trim();
    if trimmed_model.is_empty() {
        return Err("Select a NVIDIA model before saving.".to_string());
    }

    let client = reqwest::Client::new();
    let response = client
        .post("https://integrate.api.nvidia.com/v1/chat/completions")
        .bearer_auth(api_key)
        .timeout(std::time::Duration::from_secs(15))
        .json(&serde_json::json!({
            "model": trimmed_model,
            "messages": [
                {"role": "user", "content": "ping"}
            ],
            "max_tokens": 1,
            "temperature": 0.0,
        }))
        .send()
        .await
        .map_err(|err| format!("NVIDIA inference validation failed: {err}"))?;

    let status = response.status().as_u16();
    if status == 200 {
        return Ok(());
    }

    if matches!(status, 400 | 401 | 403 | 404) {
        return Err(format!(
            "NVIDIA accepted the API key for model listing, but rejected inference for model '{trimmed_model}'."
        ));
    }

    let body = response.text().await.unwrap_or_default();
    let detail = body.chars().take(200).collect::<String>();
    Err(format!(
        "NVIDIA inference validation failed for model '{trimmed_model}' (HTTP {status}): {detail}"
    ))
}

fn emit_background_fault(app: &tauri::AppHandle, severity: &str, subsystem: &str, message: &str, code: Option<&str>) {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let payload = serde_json::json!({
        "kind": "background_error",
        "severity": severity,
        "subsystem": subsystem,
        "message": message,
        "trace_id": format!("{}:{}:{}", subsystem, code.unwrap_or("event"), timestamp),
        "timestamp": timestamp,
        "code": code,
    });
    let _ = app.emit("live-telemetry", payload.to_string());
}

pub struct AppState {
    kernel: Arc<AsyncMutex<Option<Arc<MoatSystem>>>>,
    consent_bridge: Arc<ConsentBridge>,
    ui_element_map: Arc<std::sync::Mutex<Vec<ui_scraper::ScrapedElement>>>,
    gateway_info: Arc<AsyncMutex<Option<GatewayServerInfo>>>,
    reasoning: Arc<AsyncMutex<ReasoningBridge>>,
    cancel_tx: Arc<watch::Sender<bool>>,
}

fn should_attach_active_ui_tree(task: &str) -> bool {
    let query = task.trim().to_ascii_lowercase();
    if query.is_empty() {
        return false;
    }

    let ui_intent_markers = [
        "click", "tap", "press", "select", "choose", "toggle", "check", "uncheck", "scroll",
        "drag", "drop", "type", "enter", "fill", "focus", "button", "menu", "dropdown",
        "dialog", "window", "tab", "field", "textbox", "input", "checkbox", "radio button",
        "current screen", "visible", "highlighted", "selected item",
    ];

    ui_intent_markers.iter().any(|marker| query.contains(marker))
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecuteTaskResponse {
    text: String,
    total_tokens: u32,
    session_id: Option<String>,
    role: String,
}

async fn gateway_server_info_from_state(
    state: &tauri::State<'_, AppState>,
) -> Result<GatewayServerInfo, String> {
    if let Some(info) = state.gateway_info.lock().await.clone() {
        return Ok(info);
    }

    let kernel = state
        .kernel
        .lock()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| KERNEL_BOOTING_ERROR.to_string())?;
    let info = kernel
        .gateway_server_info()
        .await
        .ok_or_else(|| "Gateway transport is unavailable.".to_string())?;
    *state.gateway_info.lock().await = Some(info.clone());
    Ok(info)
}

fn gateway_request_identity(
    request: &GatewayRpcRequest,
) -> (SessionSource, Option<String>, String, String) {
    match request {
        GatewayRpcRequest::ExecutePrompt {
            source, channel, ..
        } => (
            *source,
            channel.clone(),
            format!("Tauri {}", source.as_str()),
            format!(
                "tauri:{}:rpc",
                channel
                    .clone()
                    .unwrap_or_else(|| format!("{}:main", source.as_str()))
            ),
        ),
        _ => (
            SessionSource::DesktopUi,
            Some("desktop_ui:control".to_string()),
            "Tauri Control Plane".to_string(),
            "tauri:desktop:control".to_string(),
        ),
    }
}

async fn write_gateway_frame<S>(
    ws_sender: &mut S,
    frame: &GatewayTransportInboundFrame,
) -> Result<(), String>
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let serialized = serde_json::to_string(frame)
        .map_err(|err| format!("Failed to serialize gateway frame: {err}"))?;
    ws_sender
        .send(Message::Text(serialized.into()))
        .await
        .map_err(|err| format!("Failed to write gateway frame: {err}"))?;
    Ok(())
}

async fn call_gateway_rpc(
    state: &tauri::State<'_, AppState>,
    app: Option<&tauri::AppHandle>,
    request: GatewayRpcRequest,
    request_id_override: Option<String>,
) -> Result<GatewayWireResponse, String> {
    let info = gateway_server_info_from_state(state).await?;
    let request_id = request_id_override.unwrap_or_else(|| Uuid::new_v4().to_string());
    let (source, channel_id, label, client_id) = gateway_request_identity(&request);
    let ws_url = format!("ws://{}", info.bind_addr);
    let (ws_stream, _) = connect_async(&ws_url)
        .await
        .map_err(|err| format!("Failed to connect to gateway transport {}: {err}", ws_url))?;
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    write_gateway_frame(
        &mut ws_sender,
        &GatewayTransportInboundFrame::Connect {
            connect: GatewayTransportConnectRequest {
                client_id: Some(client_id),
                role: GatewayClientRole::ControlPlaneClient,
                source,
                channel_id,
                label: Some(label),
                device_id: Some(GATEWAY_DESKTOP_DEVICE_ID.to_string()),
                capabilities: vec![
                    "gateway.rpc".to_string(),
                    "session.history".to_string(),
                    "consent.desktop_ui".to_string(),
                ],
                subscribe_events: false,
            },
        },
    )
    .await?;
    write_gateway_frame(
        &mut ws_sender,
        &GatewayTransportInboundFrame::Request {
            request_id: request_id.clone(),
            request,
        },
    )
    .await?;

    while let Some(Ok(msg)) = ws_receiver.next().await {
        let line = match msg {
            Message::Text(text) => text.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };
        if line.trim().is_empty() {
            continue;
        }

        let frame: GatewayTransportFrame = serde_json::from_str(&line)
            .map_err(|err| format!("Failed to parse gateway frame: {err}"))?;
        match frame {
            GatewayTransportFrame::Hello { .. } => continue,
            GatewayTransportFrame::Connected { .. } => continue,
            GatewayTransportFrame::Error { message } => return Err(message),
            GatewayTransportFrame::Event { event } => {
                if let (
                    Some(app_handle),
                    GatewayServerEvent::RequestStatus {
                        request_id: event_request_id,
                        message,
                        ..
                    },
                ) = (app, event)
                {
                    if event_request_id == request_id {
                        if let Some(text) = message.strip_prefix("__STREAM_CHUNK__:") {
                            let _ = app_handle.emit(
                                GATEWAY_SERVER_EVENT_NAME,
                                serde_json::json!({
                                    "kind": "token_stream",
                                    "request_id": request_id,
                                    "text": text
                                })
                            );
                        } else {
                            let _ = app_handle.emit("live-telemetry", message);
                        }
                    }
                }
            }
            GatewayTransportFrame::Response {
                request_id: response_request_id,
                ok,
                response,
                error,
            } => {
                if response_request_id != request_id {
                    continue;
                }
                if ok {
                    return response.ok_or_else(|| {
                        "Gateway returned an empty successful response.".to_string()
                    });
                }
                return Err(error.unwrap_or_else(|| {
                    "Gateway returned an unspecified transport error.".to_string()
                }));
            }
        }
    }

    Err("Gateway connection closed before a response was received.".to_string())
}

fn format_gateway_execution_output(response: GatewayWireResponse) -> Result<ExecuteTaskResponse, String> {
    let GatewayWireResponse::Execution { results, session_id } = response else {
        return Err("Unexpected non-execution gateway response.".to_string());
    };

    let mut total_tokens = 0_u32;
    let mut clean_parts: Vec<String> = Vec::new();
    let mut action_success_parts: Vec<String> = Vec::new();
    let mut system_like = false;

    for res in &results {
        total_tokens += res.tokens_used;
        if res.action == "user:ask_choice" {
            continue;
        }

        match &res.status {
            GatewayWireTaskStatus::Success { stdout } => {
                let text = stdout.trim();
                // For "done" and "chat" actions, the stdout IS the agent's
                // human-facing response. Extract it cleanly.
                if !text.is_empty()
                    && (res.action == "done" || res.action == "chat")
                {
                    clean_parts.push(text.to_string());
                } else if !text.is_empty() {
                    action_success_parts.push(text.to_string());
                }
            }
            GatewayWireTaskStatus::Failed { exit_code, stderr } => {
                system_like = true;
                let code_str = exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "DENIED".to_string());
                clean_parts.push(format!(
                    "Action '{}' on '{}' failed (exit {}):\n{}",
                    res.action,
                    res.target,
                    code_str,
                    stderr.trim()
                ));
            }
            GatewayWireTaskStatus::SoftDenial { reason } => {
                system_like = true;
                clean_parts.push(format!(
                    "Permission needed: {}",
                    reason.trim()
                ));
            }
            GatewayWireTaskStatus::Blocked { blocked_by } => {
                system_like = true;
                clean_parts.push(format!("Waiting on subtask {}", blocked_by));
            }
            GatewayWireTaskStatus::Cancelled { stage } => {
                system_like = true;
                clean_parts.push(format!("Cancelled at: {}", stage.trim()));
            }
        }
    }

    // If we extracted clean human-facing content, use it directly.
    // Otherwise fall back to verbose telemetry format for debugging.
    let output_text = if !clean_parts.is_empty() {
        clean_parts.join("\n\n")
    } else if !action_success_parts.is_empty() {
        action_success_parts.join("\n\n")
    } else {
        // Fallback: verbose format for transparency when no done/chat output exists
        let mut verbose = String::new();
        for res in &results {
            if res.action == "user:ask_choice" {
                continue;
            }
            verbose.push_str(&format!(
                "Action: {} → {}\n",
                res.action, res.target
            ));
            if let GatewayWireTaskStatus::Success { stdout } = &res.status {
                if !stdout.trim().is_empty() {
                    verbose.push_str(&format!("Result: {}\n", stdout.trim()));
                }
            }
        }
        if verbose.trim().is_empty() {
            "Done.".to_string()
        } else {
            verbose.trim().to_string()
        }
    };

    Ok(ExecuteTaskResponse {
        text: output_text.trim().to_string(),
        total_tokens,
        session_id,
        role: if system_like { "system".into() } else { "agent".into() },
    })
}

fn spawn_gateway_event_stream(
    app: tauri::AppHandle,
    gateway_info: Arc<AsyncMutex<Option<GatewayServerInfo>>>,
    stream_flag: Arc<AtomicBool>,
) {
    if stream_flag
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    tauri::async_runtime::spawn(async move {
        let info = match gateway_info.lock().await.clone() {
            Some(info) => info,
            None => {
                stream_flag.store(false, Ordering::SeqCst);
                return;
            }
        };

        let ws_url = format!("ws://{}", info.bind_addr);
        let ws_stream = match connect_async(&ws_url).await {
            Ok((stream, _)) => stream,
            Err(err) => {
                let _ = app.emit(
                    "live-telemetry",
                    format!("[GATEWAY] Event stream connection failed: {}", err),
                );
                stream_flag.store(false, Ordering::SeqCst);
                return;
            }
        };

        let (mut ws_sender, mut ws_receiver) = ws_stream.split();
        if let Err(err) = write_gateway_frame(
            &mut ws_sender,
            &GatewayTransportInboundFrame::Connect {
                connect: GatewayTransportConnectRequest {
                    client_id: Some("tauri:desktop:event-stream".to_string()),
                    role: GatewayClientRole::ControlPlaneClient,
                    source: SessionSource::DesktopUi,
                    channel_id: Some("desktop_ui:events".to_string()),
                    label: Some("Tauri Event Stream".to_string()),
                    device_id: Some(GATEWAY_DESKTOP_DEVICE_ID.to_string()),
                    capabilities: vec!["gateway.events".to_string()],
                    subscribe_events: true,
                },
            },
        )
        .await
        {
            let _ = app.emit(
                "live-telemetry",
                format!("[GATEWAY] Event stream handshake failed: {}", err),
            );
            stream_flag.store(false, Ordering::SeqCst);
            return;
        }

        while let Some(Ok(msg)) = ws_receiver.next().await {
            let line = match msg {
                Message::Text(text) => text.to_string(),
                Message::Close(_) => break,
                _ => continue,
            };

            if line.trim().is_empty() {
                continue;
            }

            let frame: GatewayTransportFrame = match serde_json::from_str(&line) {
                Ok(frame) => frame,
                Err(err) => {
                    let _ = app.emit(
                        "live-telemetry",
                        format!("[GATEWAY] Failed to parse event stream frame: {}", err),
                    );
                    continue;
                }
            };

            match frame {
                GatewayTransportFrame::Hello { .. } => continue,
                GatewayTransportFrame::Connected { .. } => continue,
                GatewayTransportFrame::Response { .. } => continue,
                GatewayTransportFrame::Error { message } => {
                    let _ = app.emit(
                        "live-telemetry",
                        format!("[GATEWAY] Event stream rejected: {}", message),
                    );
                    break;
                }
                GatewayTransportFrame::Event { event } => {
                    let _ = app.emit(GATEWAY_SERVER_EVENT_NAME, event);
                }
            }
        }

        let _ = app.emit(
            "live-telemetry",
            "[GATEWAY] Event stream disconnected.".to_string(),
        );
        stream_flag.store(false, Ordering::SeqCst);
    });
}

#[tauri::command]
async fn cancel_execution(
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let _ = state.cancel_tx.send(true);
    if let Some(kernel) = state.kernel.lock().await.as_ref() {
        kernel.cancel_flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    Ok(())
}

#[tauri::command]
async fn execute_task(
    task: String,
    client_request_id: Option<String>,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<ExecuteTaskResponse, String> {
    // Inject the active UI tree into the task context if available
    let enriched_task = {
        let ui_tree = if should_attach_active_ui_tree(&task) {
            state
                .ui_element_map
                .lock()
                .ok()
                .filter(|elems| !elems.is_empty())
                .and_then(|_| {
                    let elems = state.ui_element_map.lock().ok()?;
                    if elems.is_empty() {
                        return None;
                    }
                    let mut lines = Vec::with_capacity(elems.len());
                    for elem in elems.iter() {
                        let display_role = match elem.role.as_str() {
                            "AXButton" => "Button",
                            "AXTextField" | "AXSearchField" => "TextField",
                            "AXTextArea" => "TextArea",
                            "AXLink" => "Link",
                            "AXMenuItem" => "MenuItem",
                            "AXCheckBox" => "CheckBox",
                            "AXRadioButton" => "RadioButton",
                            "AXPopUpButton" => "PopUpButton",
                            "AXStaticText" => "Text",
                            other => other,
                        };
                        if !elem.value.is_empty() && !elem.label.is_empty() {
                            lines.push(format!(
                                "[ID: {}] [{}] \"{}\" = \"{}\"",
                                elem.id, display_role, elem.label, elem.value
                            ));
                        } else if !elem.label.is_empty() {
                            lines.push(format!(
                                "[ID: {}] [{}] \"{}\"",
                                elem.id, display_role, elem.label
                            ));
                        } else if !elem.value.is_empty() {
                            lines.push(format!(
                                "[ID: {}] [{}] = \"{}\"",
                                elem.id, display_role, elem.value
                            ));
                        } else {
                            lines.push(format!("[ID: {}] [{}]", elem.id, display_role));
                        }
                    }
                    Some(lines.join("\n"))
                })
        } else {
            None
        };

        if let Some(tree_text) = ui_tree {
            format!(
                "{}\n\n<active_ui_tree>\n{}\n</active_ui_tree>\n\
                 Use os:click with {{\"element_id\": N}} to click elements. \
                 Use os:type_text with {{\"element_id\": N, \"text\": \"...\"}} to type into fields.",
                task, tree_text
            )
        } else {
            task
        }
    };

    let window_label = window.label().to_string();
    let (source, channel) = if window_label == "command_bar" {
        (SessionSource::CommandBar, Some("command_bar".to_string()))
    } else {
        (SessionSource::WebChat, Some("tauri_webchat:main".to_string()))
    };

    let mut cancel_rx = state.cancel_tx.subscribe();
    let _ = state.cancel_tx.send(false);
    cancel_rx.borrow_and_update();

    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(120),
        async {
            tokio::select! {
                rpc = call_gateway_rpc(
                    &state,
                    Some(&app),
                    GatewayRpcRequest::ExecutePrompt {
                        source,
                        channel,
                        text: enriched_task,
                        root_node_id: 0,
                    },
                    client_request_id,
                ) => rpc,
                _ = cancel_rx.changed() => Err("Cancelled by user".to_string()),
            }
        },
    )
    .await
    .unwrap_or_else(|_| Err("Execution timed out after 120 seconds".to_string()));

    match result {
        Ok(response) => {
            let output_log = format_gateway_execution_output(response)?;
            Ok(output_log)
        }
        Err(e) => {
            let err_string = format!("{:?}", e);
            Err(format!("Kernel Pipeline Error: {}", err_string))
        }
    }
}

// CRITICAL FIX: Removed the 'pub' keyword.
// This stops the Tauri macro from triggering an E0255 export collision.
#[tauri::command]
fn resolve_consent(
    request_id: String,
    decision: ConsentDecision,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    state.consent_bridge.resolve(&request_id, decision)
}

#[tauri::command]
fn resolve_choice(
    request_id: String,
    selection: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    state.consent_bridge.resolve_choice(&request_id, selection)
}

#[tauri::command]
async fn fetch_nvidia_models(
    api_key: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let mut resolved_key = api_key.trim().to_string();
    if resolved_key.is_empty() || resolved_key == "__stored__" {
        resolved_key = resolve_stored_nvidia_api_key(&state).await?;
    }
    if resolved_key.is_empty() {
        return Err("No stored NVIDIA API key found.".to_string());
    }

    let client = reqwest::Client::new();
    let response = client
        .get("https://integrate.api.nvidia.com/v1/models")
        .bearer_auth(resolved_key)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|err| format!("NVIDIA request failed: {err}"))?;

    let status = response.status().as_u16();
    if status == 200 {
        let payload: NvidiaModelsResponse = response
            .json()
            .await
            .map_err(|err| format!("Failed to parse NVIDIA models catalog: {err}"))?;
        let mut models = payload
            .data
            .into_iter()
            .map(|entry| entry.id.trim().to_string())
            .filter(|id| !id.is_empty())
            .collect::<Vec<_>>();
        models.sort();
        models.dedup();
        Ok(models)
    } else if matches!(status, 400 | 401 | 403 | 404) {
        Err("Invalid API Key or unable to fetch NVIDIA models".to_string())
    } else {
        let body = response
            .text()
            .await
            .unwrap_or_default();
        let detail = body.chars().take(200).collect::<String>();
        Err(format!(
            "Failed to fetch NVIDIA models (HTTP {}): {}",
            status,
            detail
        ))
    }
}

#[tauri::command]
async fn validate_nvidia_inference(
    api_key: String,
    model_name: String,
    state: tauri::State<'_, AppState>,
) -> Result<bool, String> {
    let mut resolved_key = api_key.trim().to_string();
    if resolved_key.is_empty() || resolved_key == "__stored__" {
        resolved_key = resolve_stored_nvidia_api_key(&state).await?;
    }
    if resolved_key.is_empty() {
        return Err("No stored NVIDIA API key found.".to_string());
    }

    validate_nvidia_inference_request(&resolved_key, &model_name).await?;
    Ok(true)
}

// ─── DeepSeek Key Resolution ────────────────────────────────────────

#[tauri::command]
async fn resolve_stored_deepseek_api_key(
    state: &tauri::State<'_, AppState>,
) -> Result<String, String> {
    let kernel = state
        .kernel
        .lock()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| KERNEL_BOOTING_ERROR.to_string())?;
    let root = kernel.settings_swarm_root_for_ui();
    let doc = match load_user_document(root.join("USER.md")).await {
        Ok(doc) => Some(doc),
        Err(_) => load_user_document(user_md_path()).await.ok(),
    };
    if let Some(key) = doc
        .as_ref()
        .and_then(|doc| doc.frontmatter.deepseek_api_key.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(key.to_string());
    }

    if let Ok(env_key) = std::env::var("DEEPSEEK_API_KEY") {
        let trimmed = env_key.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    let profiles = load_provider_profiles().unwrap_or_else(|_| default_provider_profiles());
    for profile in profiles.deepseek_profiles {
        let account = deepseek_account_name(&profile.id);
        let entry = Entry::new(KEYCHAIN_SERVICE_NAME, &account)
            .map_err(|err| format!("Failed to open DeepSeek keychain entry: {err}"))?;
        match entry.get_password() {
            Ok(secret) => {
                let trimmed = secret.trim();
                if !trimmed.is_empty() {
                    return Ok(trimmed.to_string());
                }
            }
            Err(keyring::Error::NoEntry) => continue,
            Err(err) => {
                return Err(format!("Failed to read DeepSeek keychain entry: {err}"));
            }
        }
    }

    Err("No stored DeepSeek API key found.".to_string())
}

async fn validate_deepseek_inference_request(
    api_key: &str,
    model_name: &str,
) -> Result<(), String> {
    let client = reqwest::Client::new();
    let response = client
        .post("https://api.deepseek.com/chat/completions")
        .bearer_auth(api_key)
        .json(&serde_json::json!({
            "model": model_name,
            "messages": [
                {"role": "user", "content": "ping"}
            ],
            "max_tokens": 1,
            "temperature": 0.0,
        }))
        .send()
        .await
        .map_err(|err| format!("DeepSeek request failed: {err}"))?;

    let status = response.status().as_u16();
    if status == 200 {
        Ok(())
    } else if matches!(status, 400 | 401 | 403 | 404) {
        Err("Invalid API Key or model not accessible".to_string())
    } else {
        let body = response.text().await.unwrap_or_default();
        let detail = body.chars().take(200).collect::<String>();
        Err(format!(
            "DeepSeek validation failed (HTTP {}): {}",
            status, detail
        ))
    }
}

#[tauri::command]
async fn fetch_deepseek_models(
    api_key: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let mut resolved_key = api_key.trim().to_string();
    if resolved_key.is_empty() || resolved_key == "__stored__" {
        resolved_key = resolve_stored_deepseek_api_key(&state).await?;
    }
    if resolved_key.is_empty() {
        return Err("No stored DeepSeek API key found.".to_string());
    }

    let client = reqwest::Client::new();
    let response = client
        .get("https://api.deepseek.com/models")
        .bearer_auth(resolved_key)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|err| format!("DeepSeek request failed: {err}"))?;

    let status = response.status().as_u16();
    if status == 200 {
        let payload: serde_json::Value = response
            .json()
            .await
            .map_err(|err| format!("Failed to parse DeepSeek models catalog: {err}"))?;
        let mut models: Vec<String> = payload
            .get("data")
            .and_then(|d| d.as_array())
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|e| e.get("id").and_then(|id| id.as_str()).map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        if models.is_empty() {
            models = vec![
                "deepseek-chat".to_string(),
                "deepseek-reasoner".to_string(),
            ];
        }
        models.sort();
        models.dedup();
        Ok(models)
    } else if matches!(status, 400 | 401 | 403 | 404) {
        Err("Invalid API Key or unable to fetch DeepSeek models".to_string())
    } else {
        let body = response.text().await.unwrap_or_default();
        let detail = body.chars().take(200).collect::<String>();
        Err(format!(
            "Failed to fetch DeepSeek models (HTTP {}): {}",
            status, detail
        ))
    }
}

#[tauri::command]
async fn validate_deepseek_inference(
    api_key: String,
    model_name: String,
    state: tauri::State<'_, AppState>,
) -> Result<bool, String> {
    let mut resolved_key = api_key.trim().to_string();
    if resolved_key.is_empty() || resolved_key == "__stored__" {
        resolved_key = resolve_stored_deepseek_api_key(&state).await?;
    }
    if resolved_key.is_empty() {
        return Err("No stored DeepSeek API key found.".to_string());
    }

    validate_deepseek_inference_request(&resolved_key, &model_name).await?;
    Ok(true)
}

#[tauri::command]
async fn get_memory_overview(
    state: tauri::State<'_, AppState>,
) -> Result<MemoryOverview, String> {
    let kernel = state
        .kernel
        .lock()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| KERNEL_BOOTING_ERROR.to_string())?;
    kernel
        .get_memory_overview()
        .await
        .map_err(|e| format!("Failed to load memory overview: {}", e))
}

#[tauri::command]
async fn list_gateway_sessions(
    limit: Option<usize>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<GatewaySessionRecord>, String> {
    let response = call_gateway_rpc(
        &state,
        None,
        GatewayRpcRequest::ListSessions {
            limit: limit.unwrap_or(24),
        },
        None,
    )
    .await
    .map_err(|e| format!("Failed to load gateway sessions: {}", e))?;
    match response {
        GatewayWireResponse::Sessions { sessions } => Ok(sessions),
        other => Err(format!("Unexpected gateway response: {:?}", other)),
    }
}

#[tauri::command]
async fn get_gateway_session_context(
    session_id: String,
    max_chars: Option<usize>,
    state: tauri::State<'_, AppState>,
) -> Result<GatewaySessionContext, String> {
    let response = call_gateway_rpc(
        &state,
        None,
        GatewayRpcRequest::GetSessionContext {
            session_id,
            max_chars: max_chars.unwrap_or(1600),
        },
        None,
    )
    .await
    .map_err(|e| format!("Failed to load gateway session context: {}", e))?;
    match response {
        GatewayWireResponse::SessionContext { context } => Ok(context),
        other => Err(format!("Unexpected gateway response: {:?}", other)),
    }
}

#[tauri::command]
async fn delete_knowledge_memory(
    id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let kernel = state
        .kernel
        .lock()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| KERNEL_BOOTING_ERROR.to_string())?;
    kernel
        .delete_knowledge_memory(&id)
        .await
        .map_err(|e| format!("Failed to delete knowledge entry: {}", e))
}

#[tauri::command]
async fn nuke_database(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let kernel = state
        .kernel
        .lock()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| KERNEL_BOOTING_ERROR.to_string())?;
    kernel
        .hard_reset_memory()
        .await
        .map_err(|e| format!("Failed to wipe memory: {}", e))
}

#[tauri::command]
async fn get_settings(state: tauri::State<'_, AppState>) -> Result<SettingsState, String> {
    let kernel = state
        .kernel
        .lock()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| KERNEL_BOOTING_ERROR.to_string())?;
    kernel
        .get_settings()
        .await
        .map_err(|e| format!("Failed to load settings: {}", e))
}

#[tauri::command]
async fn get_gateway_server_info(
    state: tauri::State<'_, AppState>,
) -> Result<Option<GatewayServerInfo>, String> {
    Ok(state.gateway_info.lock().await.clone())
}

#[tauri::command]
async fn save_settings(
    update: SettingsUpdate,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let kernel = state
        .kernel
        .lock()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| KERNEL_BOOTING_ERROR.to_string())?;
    kernel
        .save_settings(update)
        .await
        .map_err(|e| format!("Failed to save settings: {}", e))
}

#[tauri::command]
async fn update_user_config(
    update: SettingsUpdate,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    save_settings(update, state).await
}

#[tauri::command]
async fn init_reasoning_engine(
    state: tauri::State<'_, AppState>,
) -> Result<bool, String> {
    let mut bridge = state.reasoning.lock().await;
    bridge.init_engine().await?;
    Ok(true)
}

#[tauri::command]
async fn instantiate_agent(
    wasm_bytes_base64: String,
    agent_label: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, String> {
    use base64::Engine;
    let mut bridge = state.reasoning.lock().await;
    if bridge.engine.is_none() {
        bridge.init_engine().await?;
    }
    let engine = bridge
        .engine
        .as_ref()
        .ok_or_else(|| "ReasoningEngine not initialized. Call init_reasoning_engine first.".to_string())?;
    let wasm_bytes = base64::engine::general_purpose::STANDARD
        .decode(&wasm_bytes_base64)
        .map_err(|e| format!("Invalid base64 WASM bytes: {}", e))?;
    let mut agent_id = [0u8; 32];
    let hash = blake3::hash(&wasm_bytes);
    agent_id.copy_from_slice(&hash.as_bytes()[..32]);
    let instance = engine
        .instantiate(&wasm_bytes, agent_id)
        .await
        .map_err(|e| format!("Failed to instantiate agent: {}", e))?;
    let id_key = format!("{}:{}", agent_label, hex::encode(agent_id));
    bridge.instances.insert(id_key.clone(), instance);
    Ok(bridge.instances.keys().cloned().collect())
}

#[tauri::command]
async fn step_agent(
    agent_key: String,
    fuel_budget: Option<u64>,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<StepOutcomePayload, String> {
    let mut bridge = state.reasoning.lock().await;
    let instance = bridge
        .instances
        .get_mut(&agent_key)
        .ok_or_else(|| format!("Agent '{}' not found", agent_key))?;
    let outcome = instance
        .call_step(fuel_budget)
        .await
        .map_err(|e| format!("Agent step failed: {}", e))?;
    if outcome.agent_yielded {
        let yield_msg = String::from_utf8_lossy(&outcome.yield_message).to_string();
        let _ = app.emit(
            "reasoning-agent-yield",
            serde_json::json!({
                "agentId": agent_key,
                "yieldMessage": yield_msg,
                "envelopesSent": outcome.total_requests_sent,
                "fuelUsed": outcome.fuel_used,
            }),
        );
    }
    for v in &outcome.violations {
        let _ = app.emit(
            "reasoning-security-violation",
            serde_json::json!({
                "agentId": agent_key,
                "seq": v.seq,
                "op": v.op,
                "violation": v.violation,
            }),
        );
    }
    Ok(StepOutcomePayload {
        agent_yielded: outcome.agent_yielded,
        yield_message: String::from_utf8_lossy(&outcome.yield_message).to_string(),
        total_requests_sent: outcome.total_requests_sent,
        fuel_used: outcome.fuel_used,
    })
}

#[tauri::command]
async fn list_agents(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let bridge = state.reasoning.lock().await;
    Ok(bridge.instances.keys().cloned().collect())
}

#[tauri::command]
async fn destroy_agent(
    agent_key: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let mut bridge = state.reasoning.lock().await;
    bridge.instances.remove(&agent_key);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            if let Some(command_bar) = app.get_webview_window("command_bar") {
                overlay::configure_overlay_panel(&command_bar);

                let handle = command_bar.clone();
                command_bar.on_window_event(move |event| {
                    if let tauri::WindowEvent::Focused(false) = event {
                        let handle = handle.clone();
                        tauri::async_runtime::spawn(async move {
                            overlay::restore_frontmost_app();
                            let _ = handle.hide();
                        });
                    }
                });
            }

            #[cfg(target_os = "macos")]
            let shortcut = Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::Space);
            #[cfg(not(target_os = "macos"))]
            let shortcut = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space);
            let app_handle_for_shortcut = app.handle().clone();
            let _ = app_handle_for_shortcut.global_shortcut().on_shortcut(
                shortcut,
                move |app_handle, _shortcut, event| {
                    if event.state != ShortcutState::Pressed {
                        return;
                    }
                    if let Some(command_bar) = app_handle.get_webview_window("command_bar") {
                        overlay::save_frontmost_app();
                        let (scraped, ui_elements) = context_scraper::scrape_os_context();
                        // Store scraped UI elements for os:click / os:type_text
                        if let Some(elements) = ui_elements {
                            if let Some(state) = app_handle.try_state::<AppState>() {
                                if let Ok(mut map) = state.ui_element_map.lock() {
                                    *map = elements;
                                }
                            }
                        }
                        let _ = command_bar.emit(CONTEXT_SCRAPED_EVENT_NAME, scraped);
                        let _ = command_bar.show();
                        let _ = command_bar.set_focus();
                    }
                },
            );

            let app_handle = app.handle().clone();

            let consent_bridge = Arc::new(ConsentBridge::new(Box::new(move |req| {
                if let Err(e) = app_handle.emit(CONSENT_EVENT_NAME, &req) {
                    eprintln!("[ConsentBridge] emit failed: {e}");
                }
            })));
            let kernel = Arc::new(AsyncMutex::new(None));
            let gateway_info = Arc::new(AsyncMutex::new(None));
            let gateway_stream_running = Arc::new(AtomicBool::new(false));

            let cfg = MoatSystemConfig {
                db_path: sqlite_db_path(),
                planner: PlannerConfig {
                    model: "meta/llama-3.1-405b-instruct".into(),
                    max_subtasks: 15,
                    temperature: 0.1,
                    max_tokens: 1500,
                },
                worker: WorkerConfig {
                    model: "meta/llama-3.1-70b-instruct".into(),
                    command_timeout: Duration::from_secs(30),
                    allow_high_risk: true,
                    external_apis: ExternalApiConfig::default(),
                },
                process_timeout: std::time::Duration::from_secs(120),
                external_apis: ExternalApiConfig::default(),
                swarm_workspace_root: std::env::var("MOAT_SWARM_WORKSPACE_ROOT")
                    .ok()
                    .map(Into::into),
                default_swarm_agent: std::env::var("MOAT_SWARM_DEFAULT_AGENT")
                    .ok()
                    .filter(|value| !value.trim().is_empty()),
            };
            let ui_element_map = Arc::new(std::sync::Mutex::new(Vec::new()));
            let reasoning_bridge = Arc::new(AsyncMutex::new(ReasoningBridge::new()));
            let envelope_app_handle = app.handle().clone();
            {
                let bridge = Arc::clone(&reasoning_bridge);
                let poller_app = envelope_app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    let rx = {
                        let mut guard = bridge.lock().await;
                        match guard.init_engine().await {
                            Ok(()) => {
                                let _ = envelope_app_handle.emit(
                                    "live-telemetry",
                                    "[REASONING] Engine initialized. Ready for WASM agents.",
                                );
                                guard.rx.take()
                            }
                            Err(e) => {
                                let _ = envelope_app_handle.emit(
                                    "live-telemetry",
                                    format!("[REASONING] Engine init failed: {}", e),
                                );
                                None
                            }
                        }
                    };
                    if let Some(rx) = rx {
                        crate::reasoning_bridge::start_envelope_poller(poller_app, rx);
                    }
                });
            }
            let (cancel_tx, _cancel_rx) = watch::channel(false);
            app.manage(AppState {
                kernel: Arc::clone(&kernel),
                consent_bridge: Arc::clone(&consent_bridge),
                ui_element_map: Arc::clone(&ui_element_map),
                gateway_info: Arc::clone(&gateway_info),
                reasoning: Arc::clone(&reasoning_bridge),
                cancel_tx: Arc::new(cancel_tx),
            });

            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let ui_bridge = Arc::new(ui_bridge_impl::TauriUiActionBridge::new(
                    Arc::clone(&ui_element_map),
                ));
                match MoatSystem::init_with_consent_and_ui_bridge(
                    cfg,
                    Arc::clone(&consent_bridge),
                    ui_bridge,
                )
                .await
                {
                    Ok(kernel_instance) => {
                        let kernel_instance = Arc::new(kernel_instance);
                        let seed_result = {
                            #[cfg(debug_assertions)]
                            {
                                let seed_test = std::env::var("MOAT_SEED_TEST_ENV").as_deref() == Ok("1");
                                if seed_test {
                                    kernel_instance.seed_test_environment().await
                                } else if let Ok(path) = std::env::var("MOAT_BOOTSTRAP_ROOT") {
                                    if path.trim().is_empty() {
                                        Ok(())
                                    } else {
                                        kernel_instance.seed_root_environment(path.trim()).await
                                    }
                                } else {
                                    Ok(())
                                }
                            }
                            #[cfg(not(debug_assertions))]
                            {
                                Ok(())
                            }
                        };

                        if let Err(err) = seed_result {
                            eprintln!("[KernelBoot] seed failed: {err}");
                        }

                        match kernel_instance.start_gateway_transport().await {
                            Ok(info) => {
                                *gateway_info.lock().await = Some(info.clone());
                                let _ = app_handle.emit(
                                    "live-telemetry",
                                    format!(
                                        "[GATEWAY] Listening on {} ({})",
                                        info.bind_addr, info.protocol
                                    ),
                                );
                                spawn_gateway_event_stream(
                                    app_handle.clone(),
                                    Arc::clone(&gateway_info),
                                    Arc::clone(&gateway_stream_running),
                                );
                            }
                            Err(err) => {
                                *gateway_info.lock().await = None;
                                emit_background_fault(
                                    &app_handle,
                                    "error",
                                    "gateway_transport",
                                    &err.to_string(),
                                    Some("GATEWAY_TRANSPORT_START_FAILED"),
                                );
                            }
                        }

                        let mut slot = kernel.lock().await;
                        *slot = Some(kernel_instance);
                        drop(slot);

                        if let Err(err) = app_handle.emit(KERNEL_READY_EVENT_NAME, ()) {
                            eprintln!("[KernelBoot] ready emit failed: {err}");
                        }
                    }
                    Err(err) => {
                        eprintln!("[KernelBoot] init failed: {err}");
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            execute_task,
            cancel_execution,
            resolve_consent,
            get_memory_overview,
            list_gateway_sessions,
            get_gateway_session_context,
            delete_knowledge_memory,
            nuke_database,
            get_settings,
            get_gateway_server_info,
            save_settings,
            update_user_config,
            resolve_choice,
            fetch_nvidia_models,
            validate_nvidia_inference,
            fetch_deepseek_models,
            validate_deepseek_inference,
            init_reasoning_engine,
            instantiate_agent,
            step_agent,
            list_agents,
            destroy_agent
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|err| eprintln!("error while running tauri application: {err}"));
}
