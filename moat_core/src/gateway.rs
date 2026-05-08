use crate::orchestrator::SubTaskResult;
use crate::{MoatError, MoatResult};
use dashmap::DashMap;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;

const DEFAULT_SESSION_TURN_THRESHOLD: usize = 20;
const DEFAULT_SESSION_TOKEN_THRESHOLD: usize = 2800;
const RECENT_CONTEXT_MESSAGES: usize = 6;
const GATEWAY_PAIRING_TTL_SECONDS: i64 = 60 * 60;
const GATEWAY_PAIRING_CODE_LEN: usize = 8;
const GATEWAY_PAIRING_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    DesktopUi,
    CommandBar,
    Voice,
    Automation,
    #[serde(rename = "webchat", alias = "web_chat")]
    WebChat,
    Cli,
    #[serde(rename = "whatsapp", alias = "whats_app")]
    WhatsApp,
    Discord,
    Slack,
    Telegram,
    Signal,
    Api,
    System,
}

impl SessionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DesktopUi => "desktop_ui",
            Self::CommandBar => "command_bar",
            Self::Voice => "voice",
            Self::Automation => "automation",
            Self::WebChat => "webchat",
            Self::Cli => "cli",
            Self::WhatsApp => "whatsapp",
            Self::Discord => "discord",
            Self::Slack => "slack",
            Self::Telegram => "telegram",
            Self::Signal => "signal",
            Self::Api => "api",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionMessageRole {
    User,
    Agent,
    System,
    Tool,
}

impl SessionMessageRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::System => "system",
            Self::Tool => "tool",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayMessageRecord {
    pub session_id: Uuid,
    pub role: SessionMessageRole,
    pub content: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySearchHit {
    pub session_id: Uuid,
    pub source: SessionSource,
    pub channel: Option<String>,
    pub title: String,
    pub snippet: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySessionRecord {
    pub session_id: Uuid,
    pub source: SessionSource,
    pub channel: Option<String>,
    pub title: String,
    pub latest_summary: Option<String>,
    pub total_messages: usize,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySessionContext {
    pub session_id: Uuid,
    pub source: SessionSource,
    pub channel: Option<String>,
    pub title: String,
    pub latest_summary: Option<String>,
    pub context: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GatewayEvent {
    SessionCreated {
        session_id: Uuid,
        source: SessionSource,
        channel: Option<String>,
    },
    MessageAppended {
        session_id: Uuid,
        role: SessionMessageRole,
        timestamp: i64,
    },
    SessionCompacted {
        session_id: Uuid,
        summary_chars: usize,
    },
    TokenStream {
        session_id: Uuid,
        text: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GatewayClientRole {
    DesktopClient,
    WebChatClient,
    CliClient,
    ControlPlaneClient,
    ChannelAdapter,
    Node,
    AutomationClient,
    Automation,
}

impl GatewayClientRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::DesktopClient => "desktop_client",
            Self::WebChatClient => "web_chat_client",
            Self::CliClient => "cli_client",
            Self::ControlPlaneClient => "control_plane_client",
            Self::ChannelAdapter => "channel_adapter",
            Self::Node => "node",
            Self::AutomationClient => "automation_client",
            Self::Automation => "automation",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GatewayClientTrustStatus {
    #[default]
    LocalTrusted,
    Trusted,
    PendingPairing,
    Unpaired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayPairingRequest {
    pub pairing_code: String,
    pub client_id: String,
    pub role: GatewayClientRole,
    pub source: SessionSource,
    pub channel_id: String,
    pub label: String,
    pub device_id: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayTrustedDevice {
    pub device_id: String,
    pub role: GatewayClientRole,
    pub source: SessionSource,
    pub label: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub approved_at: i64,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAdapterDescriptor {
    pub adapter_id: String,
    pub source: SessionSource,
    pub stable_channel_id: String,
    pub title: String,
    pub transport: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayClientDescriptor {
    pub client_id: String,
    pub role: GatewayClientRole,
    pub source: SessionSource,
    pub channel_id: String,
    pub label: String,
    pub device_id: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub trust_status: GatewayClientTrustStatus,
    pub connected_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GatewayServerEvent {
    AdapterRegistered {
        adapter_id: String,
        source: SessionSource,
        channel_id: String,
    },
    ClientConnected {
        client_id: String,
        role: GatewayClientRole,
        source: SessionSource,
        channel_id: String,
        trust_status: GatewayClientTrustStatus,
    },
    ClientPairingRequired {
        client_id: String,
        role: GatewayClientRole,
        source: SessionSource,
        channel_id: String,
        device_id: String,
        pairing_code: String,
        expires_at: i64,
    },
    ClientTrusted {
        device_id: String,
        role: GatewayClientRole,
        source: SessionSource,
    },
    ClientTrustRevoked {
        device_id: String,
        role: Option<GatewayClientRole>,
    },
    ClientDisconnected {
        client_id: String,
    },
    SessionResolved {
        client_id: String,
        session_id: Uuid,
        source: SessionSource,
        channel_id: String,
    },
    SessionCreated {
        session_id: Uuid,
        source: SessionSource,
        channel_id: Option<String>,
    },
    SessionMessageAppended {
        session_id: Uuid,
        role: SessionMessageRole,
        timestamp: i64,
    },
    SessionCompacted {
        session_id: Uuid,
        summary_chars: usize,
    },
    TokenStream {
        session_id: Uuid,
        text: String,
    },
    RequestStatus {
        request_id: String,
        message: String,
        timestamp: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GatewayRpcRequest {
    ExecutePrompt {
        source: SessionSource,
        channel: Option<String>,
        text: String,
        root_node_id: u64,
    },
    ListSessions {
        limit: usize,
    },
    GetSessionMessages {
        session_id: String,
        limit: usize,
    },
    GetSessionContext {
        session_id: String,
        max_chars: usize,
    },
    SearchMessages {
        query: String,
        limit: usize,
    },
    ListGatewayClients,
    ListGatewayPairingRequests,
    ApproveGatewayPairing {
        pairing_code: String,
    },
    ListTrustedGatewayDevices,
    RevokeGatewayDeviceTrust {
        device_id: String,
        role: Option<GatewayClientRole>,
    },
}

#[derive(Debug, Clone)]
pub enum GatewayRpcResponse {
    Execution {
        session_id: Uuid,
        results: Vec<SubTaskResult>,
    },
    Sessions {
        sessions: Vec<GatewaySessionRecord>,
    },
    Messages {
        messages: Vec<GatewayMessageRecord>,
    },
    SessionContext {
        context: GatewaySessionContext,
    },
    SearchResults {
        hits: Vec<GatewaySearchHit>,
    },
    Clients {
        clients: Vec<GatewayClientDescriptor>,
    },
    PairingRequests {
        requests: Vec<GatewayPairingRequest>,
    },
    PairingApproval {
        device: Option<GatewayTrustedDevice>,
    },
    TrustedDevices {
        devices: Vec<GatewayTrustedDevice>,
    },
    DeviceTrustRevoked {
        revoked: bool,
    },
}

#[derive(Debug, Clone, Default)]
struct RollingSessionBuffer {
    foundation_summary: Option<String>,
    hot_messages: Vec<GatewayMessageRecord>,
}

impl RollingSessionBuffer {
    fn estimated_hot_tokens(&self) -> usize {
        self.hot_messages
            .iter()
            .map(|message| estimate_tokens(&message.content))
            .sum()
    }

    fn needs_compaction(&self, turn_threshold: usize, token_threshold: usize) -> bool {
        self.hot_messages.len() >= turn_threshold || self.estimated_hot_tokens() >= token_threshold
    }
}

pub struct GatewayControlPlane {
    source_sessions: DashMap<String, Uuid>,
    buffers: DashMap<Uuid, RollingSessionBuffer>,
    turn_threshold: usize,
    token_threshold: usize,
    events: broadcast::Sender<GatewayEvent>,
}

pub struct GatewayServer {
    control_plane: Arc<GatewayControlPlane>,
    adapters: DashMap<String, ChannelAdapterDescriptor>,
    clients: DashMap<String, GatewayClientDescriptor>,
    pairing_requests: DashMap<String, GatewayPairingRequest>,
    trusted_devices: DashMap<String, GatewayTrustedDevice>,
    events: broadcast::Sender<GatewayServerEvent>,
}

impl GatewayControlPlane {
    pub fn new(turn_threshold: usize) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            source_sessions: DashMap::new(),
            buffers: DashMap::new(),
            turn_threshold: turn_threshold.max(4),
            token_threshold: DEFAULT_SESSION_TOKEN_THRESHOLD,
            events,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_SESSION_TURN_THRESHOLD)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<GatewayEvent> {
        self.events.subscribe()
    }

    pub fn reset_runtime_state(&self) {
        self.source_sessions.clear();
        self.buffers.clear();
    }

    pub fn ensure_session(
        &self,
        conn: &Connection,
        source: SessionSource,
        channel: Option<&str>,
        title: Option<&str>,
    ) -> MoatResult<Uuid> {
        let channel_value = channel.unwrap_or("").trim().to_string();
        let key = format!("{}:{}", source.as_str(), channel_value);
        if let Some(existing) = self.source_sessions.get(&key) {
            return Ok(*existing);
        }

        let maybe_existing = conn
            .query_row(
                "SELECT session_id, latest_summary
                 FROM gateway_sessions
                 WHERE source = ?1 AND channel = ?2
                 ORDER BY updated_at DESC
                 LIMIT 1",
                params![source.as_str(), channel_value],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .ok();

        let session_id = if let Some((session_id_raw, latest_summary)) = maybe_existing {
            let session_id = Uuid::parse_str(&session_id_raw).map_err(|err| {
                MoatError::InvariantViolation(format!("invalid stored session id: {err}"))
            })?;
            let mut buffer = RollingSessionBuffer {
                foundation_summary: latest_summary,
                hot_messages: self.load_recent_messages(
                    conn,
                    session_id,
                    RECENT_CONTEXT_MESSAGES,
                )?,
            };
            buffer.hot_messages.sort_by_key(|message| message.timestamp);
            self.buffers.insert(session_id, buffer);
            session_id
        } else {
            let session_id = Uuid::new_v4();
            let now = current_timestamp();
            let title = title.unwrap_or("MoatAgeex Session").trim();
            conn.execute(
                "INSERT INTO gateway_sessions (
                    session_id, source, channel, title, created_at, updated_at, total_messages
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?5, 0)",
                params![
                    session_id.to_string(),
                    source.as_str(),
                    channel_value,
                    if title.is_empty() {
                        "MoatAgeex Session"
                    } else {
                        title
                    },
                    now
                ],
            )
            .map_err(MoatError::Database)?;
            self.buffers
                .insert(session_id, RollingSessionBuffer::default());
            let _ = self.events.send(GatewayEvent::SessionCreated {
                session_id,
                source,
                channel: if channel_value.is_empty() {
                    None
                } else {
                    Some(channel_value.clone())
                },
            });
            session_id
        };

        self.source_sessions.insert(key, session_id);
        Ok(session_id)
    }

    pub fn append_message(
        &self,
        conn: &Connection,
        session_id: Uuid,
        role: SessionMessageRole,
        content: &str,
    ) -> MoatResult<i64> {
        let now = current_timestamp();
        conn.execute(
            "INSERT INTO gateway_messages (session_id, role, content, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_id.to_string(), role.as_str(), content, now],
        )
        .map_err(MoatError::Database)?;
        conn.execute(
            "UPDATE gateway_sessions
             SET updated_at = ?2,
                 total_messages = total_messages + 1
             WHERE session_id = ?1",
            params![session_id.to_string(), now],
        )
        .map_err(MoatError::Database)?;

        let record = GatewayMessageRecord {
            session_id,
            role,
            content: content.to_string(),
            timestamp: now,
        };
        self.buffers
            .entry(session_id)
            .or_default()
            .hot_messages
            .push(record);
        let _ = self.events.send(GatewayEvent::MessageAppended {
            session_id,
            role,
            timestamp: now,
        });
        Ok(now)
    }

    pub fn session_prompt_context(&self, session_id: Uuid) -> Option<String> {
        let buffer = self.buffers.get(&session_id)?;
        Self::render_prompt_context(
            buffer.foundation_summary.as_deref(),
            &buffer.hot_messages,
            usize::MAX,
        )
    }

    pub fn compaction_needed(&self, session_id: Uuid) -> bool {
        self.buffers
            .get(&session_id)
            .map(|buffer| buffer.needs_compaction(self.turn_threshold, self.token_threshold))
            .unwrap_or(false)
    }

    pub fn compaction_input(
        &self,
        session_id: Uuid,
    ) -> Option<(Option<String>, Vec<GatewayMessageRecord>)> {
        let buffer = self.buffers.get(&session_id)?;
        if !buffer.needs_compaction(self.turn_threshold, self.token_threshold) {
            return None;
        }
        Some((
            buffer.foundation_summary.clone(),
            buffer.hot_messages.clone(),
        ))
    }

    pub fn apply_compaction(
        &self,
        conn: &Connection,
        session_id: Uuid,
        summary: &str,
        source_message_count: usize,
    ) -> MoatResult<()> {
        let now = current_timestamp();
        conn.execute(
            "INSERT INTO gateway_session_summaries (
                session_id, summary_text, source_message_count, created_at
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                session_id.to_string(),
                summary,
                source_message_count as i64,
                now
            ],
        )
        .map_err(MoatError::Database)?;
        conn.execute(
            "UPDATE gateway_sessions
             SET latest_summary = ?2,
                 last_compacted_at = ?3,
                 compaction_count = compaction_count + 1,
                 updated_at = ?3
             WHERE session_id = ?1",
            params![session_id.to_string(), summary, now],
        )
        .map_err(MoatError::Database)?;

        let synthetic_summary = format!("[SESSION SUMMARY]\n{summary}");
        conn.execute(
            "INSERT INTO gateway_messages (session_id, role, content, created_at)
             VALUES (?1, 'system', ?2, ?3)",
            params![session_id.to_string(), synthetic_summary, now],
        )
        .map_err(MoatError::Database)?;
        conn.execute(
            "UPDATE gateway_sessions
             SET total_messages = total_messages + 1,
                 updated_at = ?2
             WHERE session_id = ?1",
            params![session_id.to_string(), now],
        )
        .map_err(MoatError::Database)?;

        let mut buffer = self.buffers.entry(session_id).or_default();
        buffer.foundation_summary = Some(summary.to_string());
        buffer.hot_messages.clear();

        let _ = self.events.send(GatewayEvent::SessionCompacted {
            session_id,
            summary_chars: summary.chars().count(),
        });
        Ok(())
    }

    pub fn latest_session_for_source(
        &self,
        conn: &Connection,
        source: SessionSource,
        channel: Option<&str>,
    ) -> MoatResult<Option<GatewaySessionRecord>> {
        let channel_value = channel.unwrap_or("").trim().to_string();
        let record = conn
            .query_row(
                "SELECT session_id, title, latest_summary, total_messages, updated_at
                 FROM gateway_sessions
                 WHERE source = ?1 AND channel = ?2
                 ORDER BY updated_at DESC
                 LIMIT 1",
                params![source.as_str(), channel_value],
                |row| {
                    Ok(GatewaySessionRecord {
                        session_id: Uuid::parse_str(&row.get::<_, String>(0)?).map_err(|err| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                Box::new(err),
                            )
                        })?,
                        source,
                        channel: if channel_value.is_empty() {
                            None
                        } else {
                            Some(channel_value.clone())
                        },
                        title: row.get(1)?,
                        latest_summary: row.get(2)?,
                        total_messages: row.get::<_, i64>(3)?.max(0) as usize,
                        updated_at: row.get(4)?,
                    })
                },
            )
            .ok();
        Ok(record)
    }

    fn load_recent_messages(
        &self,
        conn: &Connection,
        session_id: Uuid,
        limit: usize,
    ) -> MoatResult<Vec<GatewayMessageRecord>> {
        let mut stmt = conn
            .prepare(
                "SELECT role, content, created_at
                 FROM gateway_messages
                 WHERE session_id = ?1
                 ORDER BY id DESC
                 LIMIT ?2",
            )
            .map_err(MoatError::Database)?;
        let rows = stmt
            .query_map(params![session_id.to_string(), limit as i64], |row| {
                let role = match row.get::<_, String>(0)?.as_str() {
                    "user" => SessionMessageRole::User,
                    "agent" => SessionMessageRole::Agent,
                    "tool" => SessionMessageRole::Tool,
                    _ => SessionMessageRole::System,
                };
                Ok(GatewayMessageRecord {
                    session_id,
                    role,
                    content: row.get(1)?,
                    timestamp: row.get(2)?,
                })
            })
            .map_err(MoatError::Database)?;

        let mut messages = Vec::new();
        for row in rows {
            messages.push(row.map_err(MoatError::Database)?);
        }
        messages.reverse();
        Ok(messages)
    }

    pub fn render_prompt_context(
        foundation_summary: Option<&str>,
        messages: &[GatewayMessageRecord],
        max_chars: usize,
    ) -> Option<String> {
        let mut sections = Vec::new();
        if let Some(summary) = foundation_summary
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            sections.push(format!(
                "Foundation Summary:\n{}",
                clip_text(summary, max_chars.min(1200))
            ));
        }
        if !messages.is_empty() {
            let lines = messages
                .iter()
                .rev()
                .take(RECENT_CONTEXT_MESSAGES)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|message| {
                    format!(
                        "{}: {}",
                        message.role.as_str().to_ascii_uppercase(),
                        clip_text(&message.content, max_chars.min(320))
                    )
                })
                .collect::<Vec<_>>();
            sections.push(format!("Recent Turns:\n{}", lines.join("\n")));
        }
        if sections.is_empty() {
            None
        } else {
            Some(format!(
                "<rolling_session_memory>\n{}\n</rolling_session_memory>",
                sections.join("\n\n")
            ))
        }
    }
}

impl GatewayServer {
    pub fn new(control_plane: Arc<GatewayControlPlane>) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            control_plane,
            adapters: DashMap::new(),
            clients: DashMap::new(),
            pairing_requests: DashMap::new(),
            trusted_devices: DashMap::new(),
            events,
        }
    }

    pub fn with_default_adapters(control_plane: Arc<GatewayControlPlane>) -> Self {
        let server = Self::new(control_plane);
        for adapter in Self::default_adapters() {
            server.register_adapter(adapter);
        }
        server
    }

    pub fn subscribe(&self) -> broadcast::Receiver<GatewayServerEvent> {
        self.events.subscribe()
    }

    pub fn emit_request_status(&self, request_id: impl Into<String>, message: impl Into<String>) {
        let _ = self.events.send(GatewayServerEvent::RequestStatus {
            request_id: request_id.into(),
            message: message.into(),
            timestamp: current_timestamp(),
        });
    }

    pub fn emit_control_plane_event(&self, event: GatewayEvent) {
        let mapped = match event {
            GatewayEvent::SessionCreated {
                session_id,
                source,
                channel,
            } => GatewayServerEvent::SessionCreated {
                session_id,
                source,
                channel_id: channel,
            },
            GatewayEvent::MessageAppended {
                session_id,
                role,
                timestamp,
            } => GatewayServerEvent::SessionMessageAppended {
                session_id,
                role,
                timestamp,
            },
            GatewayEvent::SessionCompacted {
                session_id,
                summary_chars,
            } => GatewayServerEvent::SessionCompacted {
                session_id,
                summary_chars,
            },
            GatewayEvent::TokenStream { session_id, text } => {
                GatewayServerEvent::TokenStream { session_id, text }
            }
        };
        let _ = self.events.send(mapped);
    }

    pub fn control_plane(&self) -> &GatewayControlPlane {
        self.control_plane.as_ref()
    }

    pub fn register_adapter(&self, adapter: ChannelAdapterDescriptor) {
        let key = format!("{}:{}", adapter.source.as_str(), adapter.stable_channel_id);
        self.adapters.insert(key, adapter.clone());
        let _ = self.events.send(GatewayServerEvent::AdapterRegistered {
            adapter_id: adapter.adapter_id,
            source: adapter.source,
            channel_id: adapter.stable_channel_id,
        });
    }

    pub fn list_adapters(&self) -> Vec<ChannelAdapterDescriptor> {
        let mut adapters = self
            .adapters
            .iter()
            .map(|entry| entry.value().clone())
            .collect::<Vec<_>>();
        adapters.sort_by(|a, b| {
            a.source
                .as_str()
                .cmp(b.source.as_str())
                .then(a.stable_channel_id.cmp(&b.stable_channel_id))
        });
        adapters
    }

    pub fn list_clients(&self) -> Vec<GatewayClientDescriptor> {
        let mut clients = self
            .clients
            .iter()
            .map(|entry| entry.value().clone())
            .collect::<Vec<_>>();
        clients.sort_by(|a, b| {
            a.source
                .as_str()
                .cmp(b.source.as_str())
                .then(a.channel_id.cmp(&b.channel_id))
                .then(a.client_id.cmp(&b.client_id))
        });
        clients
    }

    pub fn list_pairing_requests(&self) -> Vec<GatewayPairingRequest> {
        self.cleanup_expired_pairing_requests(current_timestamp());
        let mut requests = self
            .pairing_requests
            .iter()
            .map(|entry| entry.value().clone())
            .collect::<Vec<_>>();
        requests.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then(a.source.as_str().cmp(b.source.as_str()))
                .then(a.device_id.cmp(&b.device_id))
        });
        requests
    }

    pub fn list_trusted_devices(&self) -> Vec<GatewayTrustedDevice> {
        let mut devices = self
            .trusted_devices
            .iter()
            .map(|entry| entry.value().clone())
            .collect::<Vec<_>>();
        devices.sort_by(|a, b| {
            a.source
                .as_str()
                .cmp(b.source.as_str())
                .then(a.device_id.cmp(&b.device_id))
                .then(a.role.as_str().cmp(b.role.as_str()))
        });
        devices
    }

    pub fn load_trusted_devices(&self, conn: &Connection) -> MoatResult<usize> {
        let mut stmt = conn
            .prepare(
                "SELECT device_id, role, source, label, capabilities, approved_at, last_seen_at
                 FROM gateway_trusted_devices",
            )
            .map_err(MoatError::Database)?;
        let rows = stmt
            .query_map([], |row| {
                let role_raw = row.get::<_, String>(1)?;
                let source_raw = row.get::<_, String>(2)?;
                let role = parse_gateway_client_role(&role_raw).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("unknown gateway client role '{role_raw}'"),
                        )),
                    )
                })?;
                let source = parse_session_source(&source_raw).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("unknown session source '{source_raw}'"),
                        )),
                    )
                })?;
                let capabilities_raw = row.get::<_, String>(4)?;
                let capabilities =
                    serde_json::from_str::<Vec<String>>(&capabilities_raw).unwrap_or_default();
                Ok(GatewayTrustedDevice {
                    device_id: row.get(0)?,
                    role,
                    source,
                    label: row.get(3)?,
                    capabilities: normalize_capabilities(capabilities),
                    approved_at: row.get(5)?,
                    last_seen_at: row.get(6)?,
                })
            })
            .map_err(MoatError::Database)?;

        let mut loaded = 0;
        for row in rows {
            let device = row.map_err(MoatError::Database)?;
            self.trusted_devices
                .insert(trusted_device_key(device.role, &device.device_id), device);
            loaded += 1;
        }
        Ok(loaded)
    }

    pub fn approve_pairing(&self, pairing_code: &str) -> MoatResult<Option<GatewayTrustedDevice>> {
        self.cleanup_expired_pairing_requests(current_timestamp());
        let code = normalize_pairing_code(pairing_code);
        if code.is_empty() {
            return Ok(None);
        }

        let Some((_, request)) = self.pairing_requests.remove(&code) else {
            return Ok(None);
        };
        if request.expires_at <= current_timestamp() {
            return Ok(None);
        }

        let now = current_timestamp();
        let device = GatewayTrustedDevice {
            device_id: request.device_id.clone(),
            role: request.role,
            source: request.source,
            label: request.label,
            capabilities: request.capabilities,
            approved_at: now,
            last_seen_at: now,
        };
        self.trusted_devices.insert(
            trusted_device_key(device.role, &device.device_id),
            device.clone(),
        );
        let _ = self.events.send(GatewayServerEvent::ClientTrusted {
            device_id: device.device_id.clone(),
            role: device.role,
            source: device.source,
        });
        Ok(Some(device))
    }

    pub fn approve_pairing_with_store(
        &self,
        conn: &Connection,
        pairing_code: &str,
    ) -> MoatResult<Option<GatewayTrustedDevice>> {
        let Some(device) = self.approve_pairing(pairing_code)? else {
            return Ok(None);
        };
        persist_trusted_device(conn, &device)?;
        Ok(Some(device))
    }

    pub fn revoke_device_trust(&self, device_id: &str, role: Option<GatewayClientRole>) -> bool {
        let normalized_device_id = normalize_optional_text(device_id);
        let Some(device_id) = normalized_device_id else {
            return false;
        };
        let keys = self
            .trusted_devices
            .iter()
            .filter(|entry| {
                entry.device_id == device_id && role.map_or(true, |role| role == entry.role)
            })
            .map(|entry| entry.key().clone())
            .collect::<Vec<_>>();
        let mut revoked = false;
        for key in keys {
            revoked |= self.trusted_devices.remove(&key).is_some();
        }
        if revoked {
            let _ = self
                .events
                .send(GatewayServerEvent::ClientTrustRevoked { device_id, role });
        }
        revoked
    }

    pub fn revoke_device_trust_with_store(
        &self,
        conn: &Connection,
        device_id: &str,
        role: Option<GatewayClientRole>,
    ) -> MoatResult<bool> {
        let normalized_device_id = normalize_optional_text(device_id);
        let Some(device_id) = normalized_device_id else {
            return Ok(false);
        };
        let revoked_memory = self.revoke_device_trust(&device_id, role);
        let changed = if let Some(role) = role {
            conn.execute(
                "DELETE FROM gateway_trusted_devices WHERE device_id = ?1 AND role = ?2",
                params![device_id, role.as_str()],
            )
            .map_err(MoatError::Database)?
        } else {
            conn.execute(
                "DELETE FROM gateway_trusted_devices WHERE device_id = ?1",
                params![device_id],
            )
            .map_err(MoatError::Database)?
        };
        Ok(revoked_memory || changed > 0)
    }

    pub fn connect_client(
        &self,
        role: GatewayClientRole,
        source: SessionSource,
        channel: Option<&str>,
        label: Option<&str>,
        requested_client_id: Option<&str>,
        device_id: Option<&str>,
        capabilities: Vec<String>,
    ) -> GatewayClientDescriptor {
        let now = current_timestamp();
        self.cleanup_expired_pairing_requests(now);
        let channel_id = self.resolve_channel_id(source, channel);
        let client_id = requested_client_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("{}:{}:{}", source.as_str(), channel_id, Uuid::new_v4()));
        let normalized_device_id = device_id
            .and_then(normalize_optional_text)
            .map(|value| value.to_string());
        let normalized_capabilities = normalize_capabilities(capabilities);
        let trust_status =
            self.resolve_trust_status(role, source, normalized_device_id.as_deref(), now);
        let client = GatewayClientDescriptor {
            client_id: client_id.clone(),
            role,
            source,
            channel_id: channel_id.clone(),
            label: label
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| source.as_str())
                .to_string(),
            device_id: normalized_device_id,
            capabilities: normalized_capabilities,
            trust_status,
            connected_at: now,
        };
        if matches!(
            client.trust_status,
            GatewayClientTrustStatus::PendingPairing
        ) {
            self.ensure_pairing_request(&client, now);
        } else if matches!(client.trust_status, GatewayClientTrustStatus::Trusted) {
            self.touch_trusted_device(&client, now);
        }
        self.clients.remove(&client_id);
        self.clients
            .insert(client.client_id.clone(), client.clone());
        let _ = self.events.send(GatewayServerEvent::ClientConnected {
            client_id: client.client_id.clone(),
            role: client.role,
            source: client.source,
            channel_id,
            trust_status: client.trust_status,
        });
        client
    }

    pub fn disconnect_client(&self, client_id: &str) {
        if self.clients.remove(client_id).is_some() {
            let _ = self.events.send(GatewayServerEvent::ClientDisconnected {
                client_id: client_id.to_string(),
            });
        }
    }

    fn resolve_trust_status(
        &self,
        role: GatewayClientRole,
        source: SessionSource,
        device_id: Option<&str>,
        now: i64,
    ) -> GatewayClientTrustStatus {
        if is_local_trusted_client(role, source) {
            return GatewayClientTrustStatus::LocalTrusted;
        }
        let Some(device_id) = device_id else {
            return GatewayClientTrustStatus::Unpaired;
        };
        let key = trusted_device_key(role, device_id);
        if let Some(mut trusted) = self.trusted_devices.get_mut(&key) {
            trusted.last_seen_at = now;
            return GatewayClientTrustStatus::Trusted;
        }
        GatewayClientTrustStatus::PendingPairing
    }

    fn touch_trusted_device(&self, client: &GatewayClientDescriptor, now: i64) {
        let Some(device_id) = client.device_id.as_deref() else {
            return;
        };
        let key = trusted_device_key(client.role, device_id);
        if let Some(mut trusted) = self.trusted_devices.get_mut(&key) {
            trusted.last_seen_at = now;
            trusted.label = client.label.clone();
            trusted.source = client.source;
            trusted.capabilities = client.capabilities.clone();
        }
    }

    fn ensure_pairing_request(
        &self,
        client: &GatewayClientDescriptor,
        now: i64,
    ) -> Option<GatewayPairingRequest> {
        let device_id = client.device_id.as_ref()?;
        if let Some(existing) = self
            .pairing_requests
            .iter()
            .find(|entry| entry.device_id == *device_id && entry.role == client.role)
            .map(|entry| entry.value().clone())
        {
            return Some(existing);
        }

        let mut code = generate_pairing_code();
        while self.pairing_requests.contains_key(&code) {
            code = generate_pairing_code();
        }
        let request = GatewayPairingRequest {
            pairing_code: code.clone(),
            client_id: client.client_id.clone(),
            role: client.role,
            source: client.source,
            channel_id: client.channel_id.clone(),
            label: client.label.clone(),
            device_id: device_id.clone(),
            capabilities: client.capabilities.clone(),
            created_at: now,
            expires_at: now + GATEWAY_PAIRING_TTL_SECONDS,
        };
        self.pairing_requests.insert(code, request.clone());
        let _ = self.events.send(GatewayServerEvent::ClientPairingRequired {
            client_id: request.client_id.clone(),
            role: request.role,
            source: request.source,
            channel_id: request.channel_id.clone(),
            device_id: request.device_id.clone(),
            pairing_code: request.pairing_code.clone(),
            expires_at: request.expires_at,
        });
        Some(request)
    }

    fn cleanup_expired_pairing_requests(&self, now: i64) {
        let expired = self
            .pairing_requests
            .iter()
            .filter(|entry| entry.expires_at <= now)
            .map(|entry| entry.key().clone())
            .collect::<Vec<_>>();
        for code in expired {
            self.pairing_requests.remove(&code);
        }
    }

    pub fn ensure_session_for_client(
        &self,
        conn: &Connection,
        role: GatewayClientRole,
        source: SessionSource,
        channel: Option<&str>,
        title: Option<&str>,
    ) -> MoatResult<(GatewayClientDescriptor, Uuid)> {
        let client = self.connect_client(role, source, channel, title, None, None, Vec::new());
        let session_id =
            self.control_plane
                .ensure_session(conn, source, Some(&client.channel_id), title)?;
        let _ = self.events.send(GatewayServerEvent::SessionResolved {
            client_id: client.client_id.clone(),
            session_id,
            source,
            channel_id: client.channel_id.clone(),
        });
        Ok((client, session_id))
    }

    pub fn list_sessions(
        &self,
        conn: &Connection,
        limit: usize,
    ) -> MoatResult<Vec<GatewaySessionRecord>> {
        let mut stmt = conn
            .prepare(
                "SELECT session_id, source, channel, title, latest_summary, total_messages, updated_at
                 FROM gateway_sessions
                 ORDER BY updated_at DESC
                 LIMIT ?1",
            )
            .map_err(MoatError::Database)?;
        let rows = stmt
            .query_map(params![limit.max(1) as i64], |row| {
                let source_raw = row.get::<_, String>(1)?;
                let source = parse_session_source(&source_raw).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("unknown session source '{source_raw}'"),
                        )),
                    )
                })?;
                let channel = row.get::<_, String>(2)?;
                Ok(GatewaySessionRecord {
                    session_id: Uuid::parse_str(&row.get::<_, String>(0)?).map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(err),
                        )
                    })?,
                    source,
                    channel: if channel.trim().is_empty() {
                        None
                    } else {
                        Some(channel)
                    },
                    title: row.get(3)?,
                    latest_summary: row.get(4)?,
                    total_messages: row.get::<_, i64>(5)?.max(0) as usize,
                    updated_at: row.get(6)?,
                })
            })
            .map_err(MoatError::Database)?;

        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row.map_err(MoatError::Database)?);
        }
        Ok(sessions)
    }

    pub fn load_session_messages(
        &self,
        conn: &Connection,
        session_id: Uuid,
        limit: usize,
    ) -> MoatResult<Vec<GatewayMessageRecord>> {
        self.control_plane
            .load_recent_messages(conn, session_id, limit.max(1))
    }

    pub fn load_session_context(
        &self,
        conn: &Connection,
        session_id: Uuid,
        max_chars: usize,
    ) -> MoatResult<GatewaySessionContext> {
        let session = conn
            .query_row(
                "SELECT source, channel, title, latest_summary, updated_at
                 FROM gateway_sessions
                 WHERE session_id = ?1",
                params![session_id.to_string()],
                |row| {
                    let source_raw = row.get::<_, String>(0)?;
                    let source = parse_session_source(&source_raw).ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("unknown session source '{source_raw}'"),
                            )),
                        )
                    })?;
                    let channel = row.get::<_, String>(1)?;
                    Ok((
                        source,
                        if channel.trim().is_empty() {
                            None
                        } else {
                            Some(channel)
                        },
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .map_err(MoatError::Database)?;
        let recent_messages =
            self.control_plane
                .load_recent_messages(conn, session_id, RECENT_CONTEXT_MESSAGES)?;
        let context = GatewayControlPlane::render_prompt_context(
            session.3.as_deref(),
            &recent_messages,
            max_chars.max(400),
        )
        .unwrap_or_else(|| {
            "<rolling_session_memory>\nNo recent session context.\n</rolling_session_memory>"
                .to_string()
        });
        Ok(GatewaySessionContext {
            session_id,
            source: session.0,
            channel: session.1,
            title: session.2,
            latest_summary: session.3,
            context,
            updated_at: session.4,
        })
    }

    pub fn search_messages(
        &self,
        conn: &Connection,
        query: &str,
        limit: usize,
    ) -> MoatResult<Vec<GatewaySearchHit>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }

        let mut stmt = conn
            .prepare(
                "SELECT
                    gm.session_id,
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
        let fts_query = format!("\"{}\"", trimmed.replace('"', "\"\""));
        let rows = stmt
            .query_map(params![fts_query, limit.max(1) as i64], |row| {
                let source_raw = row.get::<_, String>(1)?;
                let source = parse_session_source(&source_raw).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("unknown session source '{source_raw}'"),
                        )),
                    )
                })?;
                let channel = row.get::<_, String>(2)?;
                Ok(GatewaySearchHit {
                    session_id: Uuid::parse_str(&row.get::<_, String>(0)?).map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(err),
                        )
                    })?,
                    source,
                    channel: if channel.trim().is_empty() {
                        None
                    } else {
                        Some(channel)
                    },
                    title: row.get(3)?,
                    snippet: row.get(4)?,
                    timestamp: row.get(5)?,
                })
            })
            .map_err(MoatError::Database)?;

        let mut hits = Vec::new();
        for row in rows {
            hits.push(row.map_err(MoatError::Database)?);
        }
        Ok(hits)
    }

    pub fn resolve_channel_id(&self, source: SessionSource, requested: Option<&str>) -> String {
        let requested = requested
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        if let Some(channel) = requested {
            return channel;
        }
        Self::default_channel_id(source).to_string()
    }

    fn default_channel_id(source: SessionSource) -> &'static str {
        match source {
            SessionSource::DesktopUi => "desktop_ui:main",
            SessionSource::CommandBar => "command_bar:overlay",
            SessionSource::Voice => "voice:primary",
            SessionSource::Automation => "automation:sentinel",
            SessionSource::WebChat => "webchat:main",
            SessionSource::Cli => "cli:main",
            SessionSource::WhatsApp => "whatsapp:main",
            SessionSource::Discord => "discord:main",
            SessionSource::Slack => "slack:main",
            SessionSource::Telegram => "telegram:main",
            SessionSource::Signal => "signal:main",
            SessionSource::Api => "api:main",
            SessionSource::System => "system:kernel",
        }
    }

    fn default_adapters() -> Vec<ChannelAdapterDescriptor> {
        vec![
            Self::adapter(
                "desktop_ui",
                SessionSource::DesktopUi,
                "desktop_ui:main",
                "Desktop UI",
                "tauri",
            ),
            Self::adapter(
                "command_bar",
                SessionSource::CommandBar,
                "command_bar:overlay",
                "Command Bar",
                "tauri",
            ),
            Self::adapter(
                "voice",
                SessionSource::Voice,
                "voice:primary",
                "Voice",
                "local_voice",
            ),
            Self::adapter(
                "automation",
                SessionSource::Automation,
                "automation:sentinel",
                "Sentinel",
                "internal",
            ),
            Self::adapter(
                "webchat",
                SessionSource::WebChat,
                "webchat:main",
                "WebChat",
                "gateway",
            ),
            Self::adapter("cli", SessionSource::Cli, "cli:main", "CLI", "stdio"),
            Self::adapter(
                "whatsapp",
                SessionSource::WhatsApp,
                "whatsapp:main",
                "WhatsApp",
                "adapter",
            ),
            Self::adapter(
                "discord",
                SessionSource::Discord,
                "discord:main",
                "Discord",
                "adapter",
            ),
            Self::adapter(
                "slack",
                SessionSource::Slack,
                "slack:main",
                "Slack",
                "adapter",
            ),
            Self::adapter(
                "telegram",
                SessionSource::Telegram,
                "telegram:main",
                "Telegram",
                "adapter",
            ),
            Self::adapter(
                "signal",
                SessionSource::Signal,
                "signal:main",
                "Signal",
                "adapter",
            ),
            Self::adapter("api", SessionSource::Api, "api:main", "API", "rpc"),
            Self::adapter(
                "system",
                SessionSource::System,
                "system:kernel",
                "Kernel",
                "internal",
            ),
        ]
    }

    fn adapter(
        adapter_id: &str,
        source: SessionSource,
        stable_channel_id: &str,
        title: &str,
        transport: &str,
    ) -> ChannelAdapterDescriptor {
        ChannelAdapterDescriptor {
            adapter_id: adapter_id.to_string(),
            source,
            stable_channel_id: stable_channel_id.to_string(),
            title: title.to_string(),
            transport: transport.to_string(),
            enabled: true,
        }
    }
}

impl Default for GatewayControlPlane {
    fn default() -> Self {
        Self::with_defaults()
    }
}

impl Default for GatewayServer {
    fn default() -> Self {
        Self::with_default_adapters(Arc::new(GatewayControlPlane::default()))
    }
}

pub fn reset_tables(conn: &Connection) -> MoatResult<()> {
    conn.execute("DELETE FROM gateway_session_summaries", [])
        .map_err(MoatError::Database)?;
    conn.execute("DELETE FROM gateway_messages", [])
        .map_err(MoatError::Database)?;
    conn.execute("DELETE FROM gateway_sessions", [])
        .map_err(MoatError::Database)?;
    conn.execute("DELETE FROM gateway_pairing_requests", [])
        .map_err(MoatError::Database)?;
    conn.execute("DELETE FROM gateway_trusted_devices", [])
        .map_err(MoatError::Database)?;
    Ok(())
}

pub fn prepare_tables(conn: &Connection) -> MoatResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS gateway_sessions (
            session_id TEXT PRIMARY KEY,
            source TEXT NOT NULL,
            channel TEXT NOT NULL DEFAULT '',
            title TEXT NOT NULL,
            latest_summary TEXT,
            parent_session_id TEXT REFERENCES gateway_sessions(session_id),
            total_messages INTEGER NOT NULL DEFAULT 0,
            compaction_count INTEGER NOT NULL DEFAULT 0,
            input_tokens INTEGER DEFAULT 0,
            output_tokens INTEGER DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            last_compacted_at INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_gateway_sessions_source
            ON gateway_sessions(source, channel, updated_at DESC);

        CREATE TABLE IF NOT EXISTS gateway_messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES gateway_sessions(session_id) ON DELETE CASCADE,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            token_count INTEGER,
            finish_reason TEXT,
            reasoning TEXT,
            reasoning_details TEXT,
            tool_call_id TEXT,
            tool_calls TEXT,
            tool_name TEXT,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_gateway_messages_session
            ON gateway_messages(session_id, id DESC);

        CREATE VIRTUAL TABLE IF NOT EXISTS gateway_messages_fts
            USING fts5(content, content='gateway_messages', content_rowid='id');

        CREATE TRIGGER IF NOT EXISTS gateway_messages_fts_insert
        AFTER INSERT ON gateway_messages BEGIN
            INSERT INTO gateway_messages_fts(rowid, content) VALUES (new.id, new.content);
        END;
        CREATE TRIGGER IF NOT EXISTS gateway_messages_fts_delete
        AFTER DELETE ON gateway_messages BEGIN
            INSERT INTO gateway_messages_fts(gateway_messages_fts, rowid, content)
            VALUES('delete', old.id, old.content);
        END;
        CREATE TRIGGER IF NOT EXISTS gateway_messages_fts_update
        AFTER UPDATE ON gateway_messages BEGIN
            INSERT INTO gateway_messages_fts(gateway_messages_fts, rowid, content)
            VALUES('delete', old.id, old.content);
            INSERT INTO gateway_messages_fts(rowid, content) VALUES (new.id, new.content);
        END;

        CREATE TABLE IF NOT EXISTS gateway_session_summaries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES gateway_sessions(session_id) ON DELETE CASCADE,
            summary_text TEXT NOT NULL,
            source_message_count INTEGER NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_gateway_session_summaries_session
            ON gateway_session_summaries(session_id, created_at DESC);

        CREATE TABLE IF NOT EXISTS gateway_pairing_requests (
            pairing_code TEXT PRIMARY KEY,
            client_id TEXT NOT NULL,
            role TEXT NOT NULL,
            source TEXT NOT NULL,
            channel_id TEXT NOT NULL,
            label TEXT NOT NULL,
            device_id TEXT NOT NULL,
            capabilities TEXT NOT NULL DEFAULT '[]',
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_gateway_pairing_requests_device
            ON gateway_pairing_requests(device_id, role, expires_at DESC);

        CREATE TABLE IF NOT EXISTS gateway_trusted_devices (
            device_id TEXT NOT NULL,
            role TEXT NOT NULL,
            source TEXT NOT NULL,
            label TEXT NOT NULL,
            capabilities TEXT NOT NULL DEFAULT '[]',
            approved_at INTEGER NOT NULL,
            last_seen_at INTEGER NOT NULL,
            PRIMARY KEY (device_id, role)
        );",
    )
    .map_err(MoatError::Database)?;
    Ok(())
}

fn current_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
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

fn estimate_tokens(text: &str) -> usize {
    if let Ok(bpe) = tiktoken_rs::cl100k_base() {
        bpe.encode_with_special_tokens(text).len().max(1)
    } else {
        text.chars().count().saturating_div(4).max(1)
    }
}

fn parse_session_source(raw: &str) -> Option<SessionSource> {
    match raw.trim() {
        "desktop_ui" => Some(SessionSource::DesktopUi),
        "command_bar" => Some(SessionSource::CommandBar),
        "voice" => Some(SessionSource::Voice),
        "automation" => Some(SessionSource::Automation),
        "webchat" => Some(SessionSource::WebChat),
        "cli" => Some(SessionSource::Cli),
        "whatsapp" => Some(SessionSource::WhatsApp),
        "discord" => Some(SessionSource::Discord),
        "slack" => Some(SessionSource::Slack),
        "telegram" => Some(SessionSource::Telegram),
        "signal" => Some(SessionSource::Signal),
        "api" => Some(SessionSource::Api),
        "system" => Some(SessionSource::System),
        _ => None,
    }
}

fn parse_gateway_client_role(raw: &str) -> Option<GatewayClientRole> {
    match raw.trim() {
        "desktop_client" => Some(GatewayClientRole::DesktopClient),
        "web_chat_client" | "webchat_client" => Some(GatewayClientRole::WebChatClient),
        "cli_client" => Some(GatewayClientRole::CliClient),
        "control_plane_client" => Some(GatewayClientRole::ControlPlaneClient),
        "channel_adapter" => Some(GatewayClientRole::ChannelAdapter),
        "node" => Some(GatewayClientRole::Node),
        "automation_client" => Some(GatewayClientRole::AutomationClient),
        "automation" => Some(GatewayClientRole::Automation),
        _ => None,
    }
}

fn persist_trusted_device(conn: &Connection, device: &GatewayTrustedDevice) -> MoatResult<()> {
    let capabilities = serde_json::to_string(&device.capabilities).map_err(MoatError::Serde)?;
    conn.execute(
        "INSERT INTO gateway_trusted_devices (
            device_id, role, source, label, capabilities, approved_at, last_seen_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(device_id, role) DO UPDATE SET
            source = excluded.source,
            label = excluded.label,
            capabilities = excluded.capabilities,
            approved_at = excluded.approved_at,
            last_seen_at = excluded.last_seen_at",
        params![
            &device.device_id,
            device.role.as_str(),
            device.source.as_str(),
            &device.label,
            capabilities,
            device.approved_at,
            device.last_seen_at
        ],
    )
    .map_err(MoatError::Database)?;
    Ok(())
}

fn is_local_trusted_client(role: GatewayClientRole, source: SessionSource) -> bool {
    matches!(
        role,
        GatewayClientRole::DesktopClient
            | GatewayClientRole::ControlPlaneClient
            | GatewayClientRole::AutomationClient
    ) || matches!(
        source,
        SessionSource::DesktopUi
            | SessionSource::CommandBar
            | SessionSource::Voice
            | SessionSource::Automation
    )
}

fn trusted_device_key(role: GatewayClientRole, device_id: &str) -> String {
    format!("{}:{}", role.as_str(), device_id.trim())
}

fn normalize_optional_text(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn normalize_pairing_code(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace() && *ch != '-')
        .collect::<String>()
        .to_ascii_uppercase()
}

fn generate_pairing_code() -> String {
    let bytes = *Uuid::new_v4().as_bytes();
    bytes
        .iter()
        .take(GATEWAY_PAIRING_CODE_LEN)
        .map(|byte| {
            GATEWAY_PAIRING_ALPHABET[*byte as usize % GATEWAY_PAIRING_ALPHABET.len()] as char
        })
        .collect()
}

fn normalize_capabilities(capabilities: Vec<String>) -> Vec<String> {
    let mut normalized = capabilities
        .into_iter()
        .map(|capability| capability.trim().to_ascii_lowercase())
        .filter(|capability| !capability.is_empty())
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

#[cfg(test)]
mod tests {
    use super::{
        prepare_tables, GatewayClientRole, GatewayClientTrustStatus, GatewayControlPlane,
        GatewayServer, SessionMessageRole, SessionSource,
    };
    use rusqlite::Connection;

    #[test]
    fn ensure_session_reuses_latest_source_session() {
        let conn = Connection::open_in_memory().expect("open db");
        prepare_tables(&conn).expect("prepare tables");
        let gateway = GatewayControlPlane::default();
        let session_id = gateway
            .ensure_session(&conn, SessionSource::DesktopUi, None, Some("Desktop"))
            .expect("create session");
        gateway
            .append_message(&conn, session_id, SessionMessageRole::User, "hello")
            .expect("append");

        let reused = gateway
            .ensure_session(&conn, SessionSource::DesktopUi, None, Some("Desktop"))
            .expect("reuse session");
        assert_eq!(session_id, reused);
    }

    #[test]
    fn compaction_replaces_hot_buffer_with_summary() {
        let conn = Connection::open_in_memory().expect("open db");
        prepare_tables(&conn).expect("prepare tables");
        let gateway = GatewayControlPlane::new(4);
        let session_id = gateway
            .ensure_session(&conn, SessionSource::DesktopUi, None, Some("Desktop"))
            .expect("create session");

        for idx in 0..4 {
            gateway
                .append_message(
                    &conn,
                    session_id,
                    if idx % 2 == 0 {
                        SessionMessageRole::User
                    } else {
                        SessionMessageRole::Agent
                    },
                    &format!("turn {idx}"),
                )
                .expect("append");
        }

        assert!(gateway.compaction_needed(session_id));
        gateway
            .apply_compaction(&conn, session_id, "summary text", 4)
            .expect("compact");

        let context = gateway
            .session_prompt_context(session_id)
            .expect("session context");
        assert!(context.contains("Foundation Summary:"));
        assert!(!context.contains("Recent Turns:\nUSER: turn 0"));
    }

    #[test]
    fn gateway_pairs_external_devices_before_trusting_them() {
        let conn = Connection::open_in_memory().expect("open db");
        prepare_tables(&conn).expect("prepare tables");
        let gateway = GatewayServer::default();
        let pending = gateway.connect_client(
            GatewayClientRole::ChannelAdapter,
            SessionSource::Discord,
            Some("discord:guild:1"),
            Some("Discord"),
            Some("discord-client"),
            Some("discord-device"),
            vec![" Gateway.RPC ".to_string(), "gateway.rpc".to_string()],
        );

        assert_eq!(
            pending.trust_status,
            GatewayClientTrustStatus::PendingPairing
        );
        assert_eq!(pending.capabilities, vec!["gateway.rpc"]);
        let requests = gateway.list_pairing_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].device_id, "discord-device");

        let approved = gateway
            .approve_pairing_with_store(&conn, &requests[0].pairing_code)
            .expect("approve")
            .expect("trusted device");
        assert_eq!(approved.device_id, "discord-device");

        let restarted = GatewayServer::default();
        restarted
            .load_trusted_devices(&conn)
            .expect("load trusted devices");
        assert_eq!(restarted.list_trusted_devices().len(), 1);

        let trusted = restarted.connect_client(
            GatewayClientRole::ChannelAdapter,
            SessionSource::Discord,
            Some("discord:guild:1"),
            Some("Discord"),
            Some("discord-client"),
            Some("discord-device"),
            vec!["gateway.rpc".to_string()],
        );
        assert_eq!(trusted.trust_status, GatewayClientTrustStatus::Trusted);
        assert!(restarted
            .revoke_device_trust_with_store(
                &conn,
                "discord-device",
                Some(GatewayClientRole::ChannelAdapter)
            )
            .expect("revoke"));

        let after_revoke = GatewayServer::default();
        after_revoke
            .load_trusted_devices(&conn)
            .expect("reload trusted devices");
        assert!(after_revoke.list_trusted_devices().is_empty());
    }

    #[test]
    fn session_source_wire_names_match_gateway_channels() {
        assert_eq!(
            serde_json::to_string(&SessionSource::WebChat).expect("serialize"),
            "\"webchat\""
        );
        assert_eq!(
            serde_json::from_str::<SessionSource>("\"web_chat\"").expect("deserialize alias"),
            SessionSource::WebChat
        );
        assert_eq!(
            serde_json::to_string(&SessionSource::WhatsApp).expect("serialize"),
            "\"whatsapp\""
        );
    }
}
