use ageex_reasoning::{AirgapEnvelope, ReasoningEngine, ReasoningInstance, StepOutcome};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatusPayload {
    pub agent_id: String,
    pub state: String,
    pub fuel_remaining: u64,
    pub fuel_budget: u64,
    pub envelopes_sent: u64,
    pub yield_message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningStatusPayload {
    pub agents: Vec<AgentStatusPayload>,
    pub envelope_queue_size: u64,
    pub engine_ready: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepOutcomePayload {
    pub agent_yielded: bool,
    pub yield_message: String,
    pub total_requests_sent: u64,
    pub fuel_used: u64,
}

pub struct ReasoningBridge {
    pub engine: Option<ReasoningEngine>,
    pub instances: HashMap<String, ReasoningInstance>,
    pub rx: Option<tokio::sync::mpsc::UnboundedReceiver<AirgapEnvelope>>,
}

impl ReasoningBridge {
    pub fn new() -> Self {
        Self {
            engine: None,
            instances: HashMap::new(),
            rx: None,
        }
    }

    pub async fn init_engine(&mut self) -> Result<(), String> {
        let (engine, rx) = ReasoningEngine::new()
            .map_err(|e| format!("Failed to create ReasoningEngine: {}", e))?;
        self.engine = Some(engine);
        self.rx = Some(rx);
        Ok(())
    }

    pub fn get_status(&self) -> ReasoningStatusPayload {
        let agents: Vec<AgentStatusPayload> = self
            .instances
            .keys()
            .map(|id| AgentStatusPayload {
                agent_id: id.clone(),
                state: "idle".into(),
                fuel_remaining: 0,
                fuel_budget: 0,
                envelopes_sent: 0,
                yield_message: String::new(),
            })
            .collect();
        ReasoningStatusPayload {
            agents,
            envelope_queue_size: 0,
            engine_ready: self.engine.is_some(),
        }
    }
}

pub fn start_envelope_poller(app: AppHandle, rx: tokio::sync::mpsc::UnboundedReceiver<AirgapEnvelope>) {
    tauri::async_runtime::spawn(async move {
        let mut rx = rx;
        while let Some(envelope) = rx.recv().await {
            let agent_id = hex::encode(envelope.agent_id);
            let _ = app.emit(
                "reasoning-airgap-envelope",
                serde_json::json!({
                    "agentId": agent_id,
                    "seq": envelope.seq,
                    "op": envelope.op.name(),
                    "requestBytes": envelope.request_bytes,
                }),
            );
        }
    });
}