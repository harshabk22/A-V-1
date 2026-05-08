use crate::gateway::{
    GatewayClientDescriptor, GatewayClientRole, GatewayMessageRecord, GatewayPairingRequest,
    GatewayRpcRequest, GatewayRpcResponse, GatewaySearchHit, GatewayServer, GatewayServerEvent,
    GatewaySessionContext, GatewaySessionRecord, GatewayTrustedDevice, SessionSource,
};
use crate::orchestrator::{SubTaskResult, TaskStatus};
use crate::{MoatError, MoatResult};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::{accept_async, tungstenite::Message};

type GatewayRpcFuture =
    Pin<Box<dyn Future<Output = MoatResult<GatewayRpcResponse>> + Send + 'static>>;
pub type GatewayRpcHandler =
    Arc<dyn Fn(String, GatewayRpcRequest) -> GatewayRpcFuture + Send + Sync>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayServerInfo {
    pub bind_addr: String,
    pub protocol: String,
    pub started_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayWireSubTaskResult {
    pub subtask_id: usize,
    pub action: String,
    pub target: String,
    pub status: GatewayWireTaskStatus,
    pub elapsed_ms: u64,
    pub tokens_used: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GatewayWireTaskStatus {
    Success {
        stdout: String,
    },
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
    Cancelled {
        stage: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GatewayWireResponse {
    Execution {
        session_id: Option<String>,
        results: Vec<GatewayWireSubTaskResult>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayTransportConnectRequest {
    pub client_id: Option<String>,
    pub role: GatewayClientRole,
    pub source: SessionSource,
    pub channel_id: Option<String>,
    pub label: Option<String>,
    pub device_id: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub subscribe_events: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayTransportInboundFrame {
    Connect {
        connect: GatewayTransportConnectRequest,
    },
    Request {
        request_id: String,
        request: GatewayRpcRequest,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayTransportFrame {
    Hello {
        info: GatewayServerInfo,
    },
    Connected {
        client: GatewayClientDescriptor,
    },
    Response {
        request_id: String,
        ok: bool,
        response: Option<GatewayWireResponse>,
        error: Option<String>,
    },
    Event {
        event: GatewayServerEvent,
    },
    Error {
        message: String,
    },
}

pub struct GatewayTransportServer {
    info: GatewayServerInfo,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl GatewayTransportServer {
    pub async fn start(
        handler: GatewayRpcHandler,
        gateway_server: Arc<GatewayServer>,
    ) -> MoatResult<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|err| {
            MoatError::Orchestrator(format!("gateway transport bind failed: {err}"))
        })?;
        let bind_addr = listener.local_addr().map_err(|err| {
            MoatError::Orchestrator(format!("gateway transport local addr failed: {err}"))
        })?;
        let info = GatewayServerInfo {
            bind_addr: bind_addr.to_string(),
            protocol: "moat_gateway_ws_v2".to_string(),
            started_at: current_timestamp(),
        };
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let task_info = info.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        break;
                    }
                    accept_result = listener.accept() => {
                        match accept_result {
                            Ok((stream, _addr)) => {
                                let handler = Arc::clone(&handler);
                                let gateway_server = Arc::clone(&gateway_server);
                                let connection_info = task_info.clone();
                                tokio::spawn(async move {
                                    let _ = serve_connection(stream, connection_info, handler, gateway_server).await;
                                });
                            }
                            Err(err) => {
                                eprintln!("[GatewayTransport] accept failed: {err}");
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(Self {
            info,
            shutdown_tx: Some(shutdown_tx),
            task,
        })
    }

    pub fn info(&self) -> GatewayServerInfo {
        self.info.clone()
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.task.await;
    }
}

async fn serve_connection(
    stream: TcpStream,
    info: GatewayServerInfo,
    handler: GatewayRpcHandler,
    gateway_server: Arc<GatewayServer>,
) -> MoatResult<()> {
    let ws_stream = accept_async(stream)
        .await
        .map_err(|err| MoatError::Orchestrator(format!("websocket handshake failed: {err}")))?;
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();
    let (out_tx, mut out_rx) = mpsc::channel::<String>(128);

    let hello =
        serde_json::to_string(&GatewayTransportFrame::Hello { info }).map_err(MoatError::Serde)?;
    out_tx
        .send(hello)
        .await
        .map_err(|_| MoatError::Orchestrator("gateway writer dropped before hello".into()))?;

    let writer_task = tokio::spawn(async move {
        while let Some(line) = out_rx.recv().await {
            if ws_sender.send(Message::Text(line.into())).await.is_err() {
                break;
            }
        }
    });

    let (client, subscribe_events) =
        perform_connect_handshake(&mut ws_receiver, &out_tx, &gateway_server).await?;

    let event_task = if subscribe_events {
        let event_tx = out_tx.clone();
        let mut events = gateway_server.subscribe();
        Some(tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        let frame = GatewayTransportFrame::Event { event };
                        let Ok(serialized) = serde_json::to_string(&frame) else {
                            continue;
                        };
                        if event_tx.send(serialized).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }))
    } else {
        None
    };

    while let Some(Ok(msg)) = ws_receiver.next().await {
        let line = match msg {
            Message::Text(text) => text.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };

        if line.trim().is_empty() {
            continue;
        }

        let frame: GatewayTransportInboundFrame = match serde_json::from_str(&line) {
            Ok(frame) => frame,
            Err(err) => {
                let error_frame = GatewayTransportFrame::Response {
                    request_id: "unknown".to_string(),
                    ok: false,
                    response: None,
                    error: Some(format!("invalid gateway request: {err}")),
                };
                let serialized = serde_json::to_string(&error_frame).map_err(MoatError::Serde)?;
                if out_tx.send(serialized).await.is_err() {
                    break;
                }
                continue;
            }
        };

        let GatewayTransportInboundFrame::Request {
            request_id,
            request,
        } = frame
        else {
            let error_frame = GatewayTransportFrame::Error {
                message: "gateway client is already connected; duplicate connect frame rejected"
                    .to_string(),
            };
            let serialized = serde_json::to_string(&error_frame).map_err(MoatError::Serde)?;
            if out_tx.send(serialized).await.is_err() {
                break;
            }
            continue;
        };

        let response = (handler)(request_id.clone(), request).await;
        let wire_frame = match response {
            Ok(response) => GatewayTransportFrame::Response {
                request_id,
                ok: true,
                response: Some(serialize_gateway_response(response)),
                error: None,
            },
            Err(err) => GatewayTransportFrame::Response {
                request_id,
                ok: false,
                response: None,
                error: Some(err.to_string()),
            },
        };
        let serialized = serde_json::to_string(&wire_frame).map_err(MoatError::Serde)?;
        if out_tx.send(serialized).await.is_err() {
            break;
        }
    }

    if let Some(event_task) = event_task {
        event_task.abort();
    }
    gateway_server.disconnect_client(&client.client_id);
    drop(out_tx);
    let _ = writer_task.await;
    Ok(())
}

async fn perform_connect_handshake(
    ws_receiver: &mut futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<TcpStream>,
    >,
    out_tx: &mpsc::Sender<String>,
    gateway_server: &Arc<GatewayServer>,
) -> MoatResult<(GatewayClientDescriptor, bool)> {
    while let Some(Ok(msg)) = ws_receiver.next().await {
        let line = match msg {
            Message::Text(text) => text.to_string(),
            Message::Close(_) => {
                return Err(MoatError::Orchestrator(
                    "gateway connection closed before connect handshake".into(),
                ))
            }
            _ => continue,
        };

        if line.trim().is_empty() {
            continue;
        }

        let frame: GatewayTransportInboundFrame = match serde_json::from_str(&line) {
            Ok(frame) => frame,
            Err(err) => {
                let serialized = serde_json::to_string(&GatewayTransportFrame::Error {
                    message: format!("invalid gateway frame before connect: {err}"),
                })
                .map_err(MoatError::Serde)?;
                let _ = out_tx.send(serialized).await;
                return Err(MoatError::Orchestrator(
                    "gateway handshake failed before connect".into(),
                ));
            }
        };

        match frame {
            GatewayTransportInboundFrame::Connect { connect } => {
                let client = gateway_server.connect_client(
                    connect.role,
                    connect.source,
                    connect.channel_id.as_deref(),
                    connect.label.as_deref(),
                    connect.client_id.as_deref(),
                    connect.device_id.as_deref(),
                    connect.capabilities,
                );
                let serialized = serde_json::to_string(&GatewayTransportFrame::Connected {
                    client: client.clone(),
                })
                .map_err(MoatError::Serde)?;
                out_tx.send(serialized).await.map_err(|_| {
                    MoatError::Orchestrator("gateway writer dropped before connect ack".into())
                })?;
                return Ok((client, connect.subscribe_events));
            }
            GatewayTransportInboundFrame::Request { .. } => {
                let serialized = serde_json::to_string(&GatewayTransportFrame::Error {
                    message: "gateway client must send connect before request".to_string(),
                })
                .map_err(MoatError::Serde)?;
                let _ = out_tx.send(serialized).await;
                return Err(MoatError::Orchestrator(
                    "gateway request arrived before connect".into(),
                ));
            }
        }
    }

    Err(MoatError::Orchestrator(
        "gateway connection closed before connect handshake".into(),
    ))
}

fn serialize_gateway_response(response: GatewayRpcResponse) -> GatewayWireResponse {
    match response {
        GatewayRpcResponse::Execution {
            session_id,
            results,
        } => GatewayWireResponse::Execution {
            session_id: Some(session_id.to_string()),
            results: results.into_iter().map(serialize_subtask_result).collect(),
        },
        GatewayRpcResponse::Sessions { sessions } => GatewayWireResponse::Sessions { sessions },
        GatewayRpcResponse::Messages { messages } => GatewayWireResponse::Messages { messages },
        GatewayRpcResponse::SessionContext { context } => {
            GatewayWireResponse::SessionContext { context }
        }
        GatewayRpcResponse::SearchResults { hits } => GatewayWireResponse::SearchResults { hits },
        GatewayRpcResponse::Clients { clients } => GatewayWireResponse::Clients { clients },
        GatewayRpcResponse::PairingRequests { requests } => {
            GatewayWireResponse::PairingRequests { requests }
        }
        GatewayRpcResponse::PairingApproval { device } => {
            GatewayWireResponse::PairingApproval { device }
        }
        GatewayRpcResponse::TrustedDevices { devices } => {
            GatewayWireResponse::TrustedDevices { devices }
        }
        GatewayRpcResponse::DeviceTrustRevoked { revoked } => {
            GatewayWireResponse::DeviceTrustRevoked { revoked }
        }
    }
}

fn serialize_subtask_result(result: SubTaskResult) -> GatewayWireSubTaskResult {
    GatewayWireSubTaskResult {
        subtask_id: result.subtask_id,
        action: result.action,
        target: result.target,
        status: serialize_task_status(result.status),
        elapsed_ms: result.elapsed.as_millis() as u64,
        tokens_used: result.tokens_used,
    }
}

fn serialize_task_status(status: TaskStatus) -> GatewayWireTaskStatus {
    match status {
        TaskStatus::Success { stdout } => GatewayWireTaskStatus::Success { stdout },
        TaskStatus::SoftDenial { reason } => GatewayWireTaskStatus::SoftDenial { reason },
        TaskStatus::Failed { exit_code, stderr } => {
            GatewayWireTaskStatus::Failed { exit_code, stderr }
        }
        TaskStatus::Blocked { blocked_by } => GatewayWireTaskStatus::Blocked { blocked_by },
        TaskStatus::Cancelled { stage } => GatewayWireTaskStatus::Cancelled { stage },
    }
}

fn current_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
