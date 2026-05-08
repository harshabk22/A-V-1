use crate::config;
use crate::execution_plan::ExecutionTask;
use crate::orchestrator::{
    ActionIntent, Orchestrator, ReactContinuation, ReactOutcome, SubTaskResult, TaskStatus,
    YieldStatus,
};
use crate::sandbox::cleanup_workspace_tmp_files;
use crate::swarm::SwarmGateway;
use crate::{MoatError, MoatResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc::Sender, RwLock as AsyncRwLock, Semaphore};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerContextRequest {
    pub worker_id: Uuid,
    pub task_id: String,
    pub question: String,
    pub blocking: bool,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerContextResponse {
    pub worker_id: Uuid,
    pub task_id: String,
    pub answer: String,
}

#[derive(Debug, Clone)]
pub struct TaskOutcome {
    pub node: ExecutionTask,
    pub results: Vec<SubTaskResult>,
    pub summary: String,
    pub artifact_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct SubAgentRequest {
    pub agent_id: String,
    pub task_instruction: String,
    pub original_tool_call: ActionIntent,
    pub continuation: ReactContinuation,
}

#[derive(Debug, Clone)]
pub enum WorkerRunOutcome {
    Completed(TaskOutcome),
    Yielded {
        node: ExecutionTask,
        request: SubAgentRequest,
    },
}

pub struct WorkerPool {
    init_semaphore: Arc<Semaphore>,
    run_semaphore: Arc<Semaphore>,
    base_orchestrator: Arc<Orchestrator>,
    gateway: Option<Arc<SwarmGateway>>,
}

struct InitializedSandbox {
    node: ExecutionTask,
    base_orchestrator: Arc<Orchestrator>,
    gateway: Option<Arc<SwarmGateway>>,
}

impl WorkerPool {
    pub fn new(
        max_workers: usize,
        base_orchestrator: Arc<Orchestrator>,
        gateway: Option<Arc<SwarmGateway>>,
    ) -> Self {
        Self {
            init_semaphore: Arc::new(Semaphore::new(max_workers.max(1))),
            run_semaphore: Arc::new(Semaphore::new(max_workers.max(1))),
            base_orchestrator,
            gateway,
        }
    }

    pub async fn dispatch_worker(
        &self,
        node: ExecutionTask,
        context: &str,
        root_node_id: u64,
        status_tx: Option<Sender<String>>,
        continuation: Option<ReactContinuation>,
        injected_observation: Option<String>,
        execution_world_state: Option<Arc<AsyncRwLock<crate::system::ExecutionWorldState>>>,
    ) -> MoatResult<WorkerRunOutcome> {
        let _run_permit = self
            .run_semaphore
            .acquire()
            .await
            .map_err(|_| MoatError::Orchestrator("worker pool shut down".into()))?;
        let sandbox = {
            let _permit = self
                .init_semaphore
                .acquire()
                .await
                .map_err(|_| MoatError::Orchestrator("worker pool shut down".into()))?;
            let result = self.initialize_sandbox(&node).await;
            result?
        };
        sandbox
            .run_react_loop(
                context,
                root_node_id,
                status_tx,
                continuation,
                injected_observation,
                execution_world_state,
            )
            .await
    }

    async fn initialize_sandbox(&self, node: &ExecutionTask) -> MoatResult<InitializedSandbox> {
        cleanup_workspace_tmp_files()?;
        Ok(InitializedSandbox {
            node: node.clone(),
            base_orchestrator: Arc::clone(&self.base_orchestrator),
            gateway: self.gateway.clone(),
        })
    }

    pub async fn request_context(
        &self,
        request: WorkerContextRequest,
        status_tx: Option<Sender<String>>,
    ) -> Option<WorkerContextResponse> {
        if let Some(tx) = status_tx {
            let payload = serde_json::json!({
                "kind": "worker_context_request",
                "worker_id": request.worker_id,
                "task_id": request.task_id,
                "question": request.question,
                "blocking": request.blocking,
                "timeout_secs": request.timeout_secs,
            });
            let _ = tx.send(payload.to_string()).await;
        }

        if request.blocking {
            tokio::time::sleep(Duration::from_secs(request.timeout_secs.max(1))).await;
        }
        None
    }
}

impl InitializedSandbox {
    async fn run_react_loop(
        self,
        context: &str,
        root_node_id: u64,
        status_tx: Option<Sender<String>>,
        continuation: Option<ReactContinuation>,
        injected_observation: Option<String>,
        execution_world_state: Option<Arc<AsyncRwLock<crate::system::ExecutionWorldState>>>,
    ) -> MoatResult<WorkerRunOutcome> {
        let node = self.node;
        let worker_id = node.assigned_worker.clone();

        let outcome = match &self.gateway {
            Some(gateway) => {
                let session = gateway.open_session(&worker_id).await?;
                let session_id = session.session_id;
                let planner = gateway.planner_for_agent(&session.agent_name)?;
                let agent = gateway.agent(
                    &session.agent_name,
                    self.base_orchestrator.worker.consent_bridge.clone(),
                )?;
                let orchestrator = self
                    .base_orchestrator
                    .with_planner(planner)
                    .with_swarm_gateway(Arc::clone(gateway))
                    .with_agent_spec(agent.spec);
                let outcome = orchestrator
                    .run_react_outcome(
                        &node.instructions,
                        context,
                        root_node_id,
                        status_tx.clone(),
                        continuation,
                        injected_observation,
                        execution_world_state.clone(),
                    )
                    .await;
                gateway.close_session(session_id).await;
                outcome?
            }
            None => {
                if !worker_id.eq_ignore_ascii_case("primary") {
                    return Err(MoatError::Orchestrator(format!(
                        "worker '{}' is unavailable without a swarm gateway",
                        worker_id
                    )));
                }
                self.base_orchestrator
                    .as_ref()
                    .with_planner(self.base_orchestrator.planner.clone())
                    .run_react_outcome(
                        &node.instructions,
                        context,
                        root_node_id,
                        status_tx,
                        continuation,
                        injected_observation,
                        execution_world_state,
                    )
                    .await?
            }
        };

        match outcome {
            ReactOutcome::Completed(results) => {
                let summary = results
                    .iter()
                    .map(|result| match &result.status {
                        TaskStatus::Success { stdout } => {
                            format!("{}:{}:{}", result.subtask_id, result.action, stdout.trim())
                        }
                        TaskStatus::SoftDenial { reason } => format!(
                            "{}:{}:SOFT DENIAL: {}",
                            result.subtask_id,
                            result.action,
                            reason.trim()
                        ),
                        TaskStatus::Failed { stderr, .. } => {
                            format!("{}:{}:{}", result.subtask_id, result.action, stderr.trim())
                        }
                        TaskStatus::Blocked { blocked_by } => format!(
                            "{}:{}:blocked by {}",
                            result.subtask_id, result.action, blocked_by
                        ),
                        TaskStatus::Cancelled { stage } => format!(
                            "{}:{}:CANCELLED: {}",
                            result.subtask_id,
                            result.action,
                            stage.trim()
                        ),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                let artifact_paths = node
                    .produces
                    .iter()
                    .map(|artifact| config::workspace_root().join(&artifact.expected_path))
                    .collect();

                Ok(WorkerRunOutcome::Completed(TaskOutcome {
                    node,
                    results,
                    summary,
                    artifact_paths,
                }))
            }
            ReactOutcome::Yielded(YieldStatus::AwaitingSubAgent {
                agent_id,
                task_instruction,
                original_tool_call,
                continuation,
            }) => Ok(WorkerRunOutcome::Yielded {
                node,
                request: SubAgentRequest {
                    agent_id,
                    task_instruction,
                    original_tool_call,
                    continuation,
                },
            }),
        }
    }
}
