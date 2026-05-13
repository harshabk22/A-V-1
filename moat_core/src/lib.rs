// moat_core/src/lib.rs
pub mod config;
pub mod consent;
pub mod execution_plan;
pub mod external_api;
pub mod gateway;
pub mod gateway_transport;
pub mod hooks;
pub mod injector;
pub mod manifest;
pub mod mcp;
pub mod memory;
pub mod meta_brain;
pub mod orchestrator;
pub mod parser;
pub mod plugins;
pub mod reflection;
pub mod rule_store;
pub mod sandbox;
pub mod sdk;
pub mod sentinel;
pub mod shortcuts;
pub mod skills;
pub mod swarm;
pub mod system;
pub mod trust_store;
pub mod types;

pub mod voice;
pub mod worker_pool;

// ============================================================
// § SHARED — Error Taxonomy & Result Alias
// ============================================================

#[derive(Debug, thiserror::Error)]
pub enum MoatError {
    #[error("meta-brain graph error: {0}")]
    Graph(String),

    #[error("meta-brain database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("VSA engine error: {0}")]
    Vsa(String),

    #[error("orchestrator error: {0}")]
    Orchestrator(String),

    #[error("sandbox error: {0}")]
    Sandbox(#[from] crate::sandbox::SandboxError),

    #[error("security violation: {0}")]
    SecurityViolation(String),

    #[error("invariant violation: {0}")]
    InvariantViolation(String),

    #[error("invalid state transition: {0}")]
    InvalidStateTransition(String),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("LLM API error: {0}")]
    Llm(String),

    #[error("context injection error: {0}")]
    Injection(String),

    #[error("settings error: {0}")]
    Settings(String),
}

pub type MoatResult<T> = Result<T, MoatError>;

// ── Agent infrastructure (Phase 7-9) ──
pub mod router;
pub mod agent;
pub mod capabilities;
pub mod context;
pub mod approval;
pub mod session;
pub mod llm;
pub mod tools;
pub mod profile;
pub mod execution;

use agent::{Agent1, Agent2, AgentResponse};
use router::{Router, RouteTarget};
use meta_brain::{MetaBrain, PermissionFlags};
use capabilities::Capability;
use llm::LlmClient;
use tools::ToolExecutor;
use session::SessionStore;
use profile::{ProfileStore, ProfileExtractor};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct System {
    router: Router,
    agent1: Agent1,
    agent2: Agent2,
    tool_executor_agent1: ToolExecutor,
    tool_executor_agent2: ToolExecutor,
    session_store: Arc<SessionStore>,
    profile_store: Arc<ProfileStore>,
}

impl System {
    pub fn new() -> Result<Self> {
        Self::init(true)
    }

    pub fn new_session() -> Result<Self> {
        Self::init(false)
    }

    fn init(resume: bool) -> Result<Self> {
        let _ = dotenvy::dotenv();

        let workspace = dirs::home_dir()
            .expect("Cannot determine home directory")
            .join("Desktop")
            .join("ageex-workspace");
        std::fs::create_dir_all(&workspace).ok();
        println!("[System] Workspace: {}", workspace.display());

        let session_store = Arc::new(SessionStore::open()?);
        println!("[System] Session store: ~/.ageex/sessions.db");

        let profile_store = Arc::new(ProfileStore::open()?);
        println!("[System] Profile store: ~/.ageex/sessions.db (user_preferences table)");

        let meta_brain = Arc::new(Mutex::new(MetaBrain::new()));

        {
            let mut brain = meta_brain.lock().unwrap();
            let ws = workspace.to_string_lossy().to_string();
            brain.grant_path_permission(&ws, PermissionFlags::FILE_READ).map_err(|e| anyhow::anyhow!("{}", e))?;
            brain.grant_path_permission(&ws, PermissionFlags::FILE_WRITE).map_err(|e| anyhow::anyhow!("{}", e))?;
            brain.grant_path_permission(&ws, PermissionFlags::FILE_DELETE).map_err(|e| anyhow::anyhow!("{}", e))?;
            brain.grant_path_permission(&ws, PermissionFlags::EXEC).map_err(|e| anyhow::anyhow!("{}", e))?;
        }

        let llm1 = LlmClient::from_env()?;
        let llm2 = LlmClient::from_env()?;
        let llm_router = LlmClient::from_env()?;

        let agent1 = Agent1::new(llm1, Arc::clone(&session_store), Arc::clone(&profile_store), resume)?;
        let agent2 = Agent2::new(llm2, Arc::clone(&session_store), Arc::clone(&profile_store), resume)?;

        if resume {
            Self::replay_approvals(&session_store, &meta_brain, "agent1")?;
            Self::replay_approvals(&session_store, &meta_brain, "agent2")?;
        }

        let tool_executor_agent1 = ToolExecutor::new(Arc::clone(&meta_brain), "agent1", Arc::clone(&session_store));
        let tool_executor_agent2 = ToolExecutor::new(Arc::clone(&meta_brain), "agent2", Arc::clone(&session_store));

        Ok(Self {
            router: Router::new(llm_router)?,
            agent1,
            agent2,
            tool_executor_agent1,
            tool_executor_agent2,
            session_store,
            profile_store,
        })
    }

    pub async fn process_user_query(&mut self, query: &str) -> Result<String> {
        let context_hint = Some("");
        let decision = self.router.route(query, context_hint).await;
        match decision.target {
            RouteTarget::Agent1 => {
                let response = self.agent1.process_query(query, &mut self.tool_executor_agent1).await?;
                match response {
                    AgentResponse::Text(text) => Ok(text),
                    AgentResponse::Delegation { task, reason } => {
                        println!("[System] Agent1 delegated task to Agent2: {}", reason);
                        let response = self.agent2.process_query(&task, &mut self.tool_executor_agent2).await?;
                        match response {
                            AgentResponse::Text(text) => Ok(text),
                            AgentResponse::Delegation { .. } => Ok("Agent2 cannot delegate further.".into()),
                        }
                    }
                }
            }
            RouteTarget::Agent2 => {
                let response = self.agent2.process_query(query, &mut self.tool_executor_agent2).await?;
                match response {
                    AgentResponse::Text(text) => Ok(text),
                    AgentResponse::Delegation { .. } => Ok("Agent2 cannot delegate.".into()),
                }
            }
        }
    }

    fn replay_approvals(session_store: &SessionStore, meta_brain: &Arc<Mutex<MetaBrain>>, agent_id: &str) -> Result<()> {
        let approvals = session_store.load_approvals(agent_id)?;
        let mut brain = meta_brain.lock().unwrap();
        for a in &approvals {
            let flags = match a.capability.as_str() {
                "FileRead" => PermissionFlags::FILE_READ,
                "FileWrite" => PermissionFlags::FILE_WRITE,
                "FileDelete" => PermissionFlags::FILE_DELETE,
                "ProcessSpawn" | "FileSpawn" => PermissionFlags::EXEC,
                "AppOpen" => PermissionFlags::APP_LAUNCH,
                "UrlOpen" => PermissionFlags::URL_OPEN,
                "SkillExecute" => PermissionFlags::TOOL_EXEC,
                _ => continue,
            };
            brain.grant_path_permission(&a.scope_value, flags).map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        if !approvals.is_empty() {
            println!("[System] Replaying {} approval(s) for {}", approvals.len(), agent_id);
        }
        Ok(())
    }
}
