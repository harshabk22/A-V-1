pub mod router;
pub mod agent;
pub mod capabilities;
pub mod context;
pub mod meta_brain;
pub mod approval;
pub mod session;
pub mod llm;
pub mod tools;
pub mod config;
pub mod system;
pub mod consent;
pub mod orchestrator;
pub mod sentinel;
pub mod external_api;
pub mod profile;
pub mod execution;

use agent::{Agent1, Agent2, AgentResponse};
use router::{Router, RouteTarget};
use meta_brain::{MetaBrain, PermissionScope};
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
            brain.grant_permission(
                "agent1",
                Capability::FileRead,
                PermissionScope::Path(workspace.clone()),
            )?;
            brain.grant_permission(
                "agent1",
                Capability::FileExists,
                PermissionScope::Path(workspace.clone()),
            )?;
            brain.grant_permission(
                "agent1",
                Capability::AppOpen,
                PermissionScope::Global,
            )?;
            brain.grant_permission(
                "agent1",
                Capability::UrlOpen,
                PermissionScope::Global,
            )?;

            brain.grant_permission(
                "agent2",
                Capability::FileRead,
                PermissionScope::Path(workspace.clone()),
            )?;
            brain.grant_permission(
                "agent2",
                Capability::FileWrite,
                PermissionScope::Path(workspace.clone()),
            )?;
            brain.grant_permission(
                "agent2",
                Capability::ProcessSpawn,
                PermissionScope::Path(workspace),
            )?;
        }

        let llm1 = LlmClient::from_env()?;
        let llm2 = LlmClient::from_env()?;
        let llm_router = LlmClient::from_env()?;

        let agent1 = Agent1::new(llm1, Arc::clone(&session_store), Arc::clone(&profile_store), resume)?;
        let agent2 = Agent2::new(llm2, Arc::clone(&session_store), Arc::clone(&profile_store), resume)?;

        // Replay persisted approvals from previous sessions
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
        let context_hint = None;

        let decision = self.router.route(query, context_hint).await;

        match decision.target {
            RouteTarget::Agent1 => {
                println!("[System] → Agent1 (read-only)");
                let response = self.agent1.process_query(query, &mut self.tool_executor_agent1).await?;
                match response {
                    AgentResponse::Text(text) => Ok(text),
                    AgentResponse::Delegation { task, reason } => {
                        println!("[System] Agent1 delegated: {} ({})", task, reason);
                        let response = self.agent2.process_query(&task, &mut self.tool_executor_agent2).await?;
                        Ok(match response {
                            AgentResponse::Text(t) => t,
                            AgentResponse::Delegation { task: t, .. } => {
                                format!("⚠ Agent2 cannot handle: {}", t)
                            }
                        })
                    }
                }
            }
            RouteTarget::Agent2 => {
                println!("[System] → Agent2 (executor)");
                let response = self.agent2.process_query(query, &mut self.tool_executor_agent2).await?;
                Ok(match response {
                    AgentResponse::Text(t) => t,
                    AgentResponse::Delegation { task: t, .. } => {
                        format!("⚠ Could not process: {}", t)
                    }
                })
            }
        }
    }

    fn replay_approvals(
        session_store: &SessionStore,
        meta_brain: &Arc<Mutex<MetaBrain>>,
        agent_id: &str,
    ) -> Result<()> {
        let approvals = session_store.load_approvals(agent_id)?;
        if approvals.is_empty() {
            return Ok(());
        }

        println!("[System] Replaying {} approval(s) for {}", approvals.len(), agent_id);
        let mut brain = meta_brain.lock().unwrap();
        for a in &approvals {
            let cap: Capability = match a.capability.as_str() {
                "FileRead" => Capability::FileRead,
                "FileWrite" => Capability::FileWrite,
                "FileExists" => Capability::FileExists,
                "ProcessSpawn" => Capability::ProcessSpawn,
                "AppOpen" => Capability::AppOpen,
                "UrlOpen" => Capability::UrlOpen,
                "SkillExecute" => Capability::SkillExecute,
                _ => continue,
            };
            let scope = match a.scope_type.as_str() {
                "path" => PermissionScope::Path(PathBuf::from(&a.scope_value)),
                "domain" => PermissionScope::Domain(a.scope_value.clone()),
                _ => PermissionScope::Global,
            };
            brain.grant_permission(agent_id, cap, scope)?;
        }
        Ok(())
    }

    pub fn list_recent_sessions(&self) -> Result<()> {
        let sessions = self.session_store.list_recent_sessions(10)?;
        println!("\n=== Recent Sessions ===");
        for s in sessions {
            println!(
                "  [{}] {} | agent: {} | last active: {}s ago",
                &s.id[..8],
                if s.parent_id.is_some() { "child" } else { "root" },
                s.agent_type,
                Self::now() - s.last_active
            );
        }
        Ok(())
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }
}