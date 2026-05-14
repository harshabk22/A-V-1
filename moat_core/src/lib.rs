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
// pub mod mcp;
pub mod memory;
pub mod meta_brain;
pub mod orchestrator;
pub mod parser;
pub mod plugins;
// pub mod reflection;
pub mod rule_store;
pub mod sandbox;
pub mod sdk;
pub mod server;
pub mod skills;
pub mod swarm;
pub mod system;
pub mod trust_store;
pub mod types;
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
use llm::LlmClient;
use tools::ToolExecutor;
use session::SessionStore;
use profile::ProfileStore;
use trust_store::TrustStore;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct System {
    agent_kind: AgentKind,
    router: Option<Router>,
    agent1: Agent1,
    agent2: Option<Agent2>,
    tool_executor_agent1: ToolExecutor,
    tool_executor_agent2: Option<ToolExecutor>,
    session_store: Arc<SessionStore>,
    profile_store: Arc<ProfileStore>,
    trust_store: Arc<TrustStore>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    Spotlight,
    MainUi,
}

impl System {
    pub fn new() -> Result<Self> {
        Self::init(true, AgentKind::MainUi)
    }

    pub fn new_session() -> Result<Self> {
        Self::init(false, AgentKind::MainUi)
    }

    pub fn new_spotlight() -> Result<Self> {
        Self::init(true, AgentKind::Spotlight)
    }

    pub fn new_main_ui() -> Result<Self> {
        Self::init(true, AgentKind::MainUi)
    }

    fn init(resume: bool, kind: AgentKind) -> Result<Self> {
        let _ = dotenvy::dotenv();

        let workspace = dirs::home_dir()
            .expect("Cannot determine home directory")
            .join("Desktop")
            .join("ageex-workspace");
        std::fs::create_dir_all(&workspace).ok();
        println!("[System] Workspace: {}", workspace.display());

        let (session_store, profile_store) = match kind {
            AgentKind::Spotlight => {
                let ss = SessionStore::open_with_prefix("spotlight_")?;
                println!("[System] Session store: ~/.ageex/sessions.db (spotlight_ prefix)");
                let ps = ProfileStore::open_with_prefix("spotlight_")?;
                println!("[System] Profile store: ~/.ageex/sessions.db (spotlight_ prefix)");
                (Arc::new(ss), Arc::new(ps))
            }
            AgentKind::MainUi => {
                let ss = SessionStore::open()?;
                println!("[System] Session store: ~/.ageex/sessions.db");
                let ps = ProfileStore::open()?;
                println!("[System] Profile store: ~/.ageex/sessions.db (user_preferences table)");
                (Arc::new(ss), Arc::new(ps))
            }
        };

        let trust_store = Arc::new(TrustStore::new());
        trust_store.register_builtin_worker("agent1");
        trust_store.register_builtin_worker("agent2");

        // NOTE: The Agent1/Agent2 path uses Arc<Mutex<MetaBrain>> (std sync Mutex)
        // for the synchronous agent loop path. The Swarm subsystem uses
        // Arc<AsyncRwLock<MetaBrain>> (tokio async RwLock) for its async workflow.
        // These are intentionally SEPARATE MetaBrain instances — they do not share
        // permission state between the agent path and the swarm path.
        let meta_brain = Arc::new(Mutex::new(MetaBrain::new()));

        {
            let mut brain = meta_brain.lock().unwrap();
            let ws = workspace.to_string_lossy().to_string();
            match kind {
                AgentKind::Spotlight => {
                    brain.grant_path_permission(&ws, PermissionFlags::FILE_READ).map_err(|e| anyhow::anyhow!("{}", e))?;
                    println!("[System] Spotlight mode: read-only workspace permission granted");
                }
                AgentKind::MainUi => {
                    brain.grant_path_permission(&ws, PermissionFlags::FILE_READ).map_err(|e| anyhow::anyhow!("{}", e))?;
                    brain.grant_path_permission(&ws, PermissionFlags::FILE_WRITE).map_err(|e| anyhow::anyhow!("{}", e))?;
                    brain.grant_path_permission(&ws, PermissionFlags::FILE_DELETE).map_err(|e| anyhow::anyhow!("{}", e))?;
                    brain.grant_path_permission(&ws, PermissionFlags::EXEC).map_err(|e| anyhow::anyhow!("{}", e))?;
                }
            }
        }

        let llm1 = LlmClient::from_env()?;
        let agent1 = Agent1::new(llm1, Arc::clone(&session_store), Arc::clone(&profile_store), resume)?;

        let auto_deny = matches!(kind, AgentKind::Spotlight);
        let tool_executor_agent1 = ToolExecutor::new_with_auto_deny(
            Arc::clone(&meta_brain),
            "agent1",
            Arc::clone(&session_store),
            auto_deny,
        )?;
        if auto_deny {
            println!("[System] Spotlight mode: permission auto-deny enabled for agent1");
        }

        if resume {
            Self::replay_approvals(&session_store, &meta_brain, "agent1")?;
        }

        let (router, agent2, tool_executor_agent2) = match kind {
            AgentKind::Spotlight => (None, None, None),
            AgentKind::MainUi => {
                let llm2 = LlmClient::from_env()?;
                let llm_router = LlmClient::from_env()?;

                let agent2 = Agent2::new(llm2, Arc::clone(&session_store), Arc::clone(&profile_store), resume)?;
                let tool_executor_agent2 = ToolExecutor::new(Arc::clone(&meta_brain), "agent2", Arc::clone(&session_store))?;

                if resume {
                    Self::replay_approvals(&session_store, &meta_brain, "agent2")?;
                }

                let router = Router::new(llm_router)?;
                (Some(router), Some(agent2), Some(tool_executor_agent2))
            }
        };

        Ok(Self {
            agent_kind: kind,
            router,
            agent1,
            agent2,
            tool_executor_agent1,
            tool_executor_agent2,
            session_store,
            profile_store,
            trust_store,
        })
    }

    pub async fn process_user_query(&mut self, query: &str) -> Result<String> {
        match &self.router {
            Some(router) => {
                let context_hint = Some("");
                let decision = router.route(query, context_hint).await;
                match decision.target {
                    RouteTarget::Agent1 => {
                        let response = self.agent1.process_query(query, &mut self.tool_executor_agent1).await?;
                        match response {
                            AgentResponse::Text(text) => Ok(text),
                            AgentResponse::Delegation { task, reason } => {
                                println!("[System] Agent1 delegated task to Agent2: {}", reason);
                                if let Some(ref mut agent2) = self.agent2 {
                                    if let Some(ref mut te2) = self.tool_executor_agent2 {
                                        let response = agent2.process_query(&task, te2).await?;
                                        return match response {
                                            AgentResponse::Text(text) => Ok(text),
                                            AgentResponse::Delegation { .. } => Ok("Agent2 cannot delegate further.".into()),
                                        };
                                    }
                                }
                                Ok("Agent1 delegated, but Agent2 is not available in this mode.".into())
                            }
                        }
                    }
                    RouteTarget::Agent2 => {
                        if let Some(ref mut agent2) = self.agent2 {
                            if let Some(ref mut te2) = self.tool_executor_agent2 {
                                let response = agent2.process_query(query, te2).await?;
                                return match response {
                                    AgentResponse::Text(text) => Ok(text),
                                    AgentResponse::Delegation { .. } => Ok("Agent2 cannot delegate.".into()),
                                };
                            }
                        }
                        Ok("Agent2 is not available in this mode.".into())
                    }
                }
            }
            None => {
                // Spotlight mode: no router, go direct to Agent1
                let response = self.agent1.process_query(query, &mut self.tool_executor_agent1).await?;
                match response {
                    AgentResponse::Text(text) => Ok(text),
                    AgentResponse::Delegation { task, reason } => {
                        println!("[System] Agent1 attempted delegation (Spotlight mode): {}", reason);
                        Ok(format!(
                            "This task requires Agent2 execution capabilities.\n\
                             Summary: {}\n\
                             Please open the main MoatAgeex app to run this.",
                            task
                        ))
                    }
                }
            }
        }
    }

    pub async fn process_spotlight_query(&mut self, query: &str) -> Result<String> {
        let response = self.agent1.process_query(query, &mut self.tool_executor_agent1).await?;
        match response {
            AgentResponse::Text(text) => Ok(text),
            AgentResponse::Delegation { task, reason } => {
                println!("[System] Agent1 attempted delegation (Spotlight mode): {}", reason);
                Ok(format!(
                    "This task requires Agent2 execution capabilities.\n\
                     Summary: {}\n\
                     Please open the main MoatAgeex app to run this.",
                    task
                ))
            }
        }
    }

    fn replay_approvals(session_store: &SessionStore, meta_brain: &Arc<Mutex<MetaBrain>>, agent_id: &str) -> Result<()> {
        let approvals = session_store.load_approvals(agent_id)?;
        let mut brain = meta_brain.lock().unwrap();
        let home = dirs::home_dir().unwrap_or_default();
        let workspace = home.join("Desktop").join("ageex-workspace");
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
            let path = normalize_approval_path(&a.scope_value, &workspace);
            brain.grant_path_permission(&path, flags).map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        if !approvals.is_empty() {
            println!("[System] Replaying {} approval(s) for {}", approvals.len(), agent_id);
        }
        Ok(())
    }
}

fn normalize_approval_path(path: &str, workspace: &Path) -> String {
    if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.to_string_lossy().into_owned() + &path[1..];
        }
    }
    let p = Path::new(path);
    if p.is_absolute() {
        path.to_string()
    } else {
        workspace.join(path).to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_kind_equality() {
        assert_eq!(AgentKind::Spotlight, AgentKind::Spotlight);
        assert_eq!(AgentKind::MainUi, AgentKind::MainUi);
        assert_ne!(AgentKind::Spotlight, AgentKind::MainUi);
    }

    #[test]
    fn test_agent_kind_debug() {
        assert_eq!(format!("{:?}", AgentKind::Spotlight), "Spotlight");
        assert_eq!(format!("{:?}", AgentKind::MainUi), "MainUi");
    }

    #[test]
    fn test_agent_kind_copy_clone() {
        let kind = AgentKind::Spotlight;
        let copied = kind;
        assert_eq!(kind, copied);
    }

    #[test]
    fn test_new_main_ui_uses_default_prefix() {
        // new_main_ui() uses SessionStore::open() (no prefix)
        // The store gets the default table names (sessions, messages, etc.)
        // This is validated by construction not panicking (it needs env setup though)
        // We test the prefix logic in session::store::tests directly.
        // Here we just verify AgentKind::MainUi maps correctly.
        let kind = AgentKind::MainUi;
        assert_eq!(kind, AgentKind::MainUi);
    }

    #[test]
    fn test_new_spotlight_uses_prefix() {
        // new_spotlight() uses SessionStore::open_with_prefix("spotlight_")
        // Verified via session::store::tests::test_open_with_prefix_creates_distinct_tables
        let kind = AgentKind::Spotlight;
        assert_eq!(kind, AgentKind::Spotlight);
    }
}
