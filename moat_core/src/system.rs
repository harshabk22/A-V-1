use chrono::Local;
use dashmap::DashMap;
use rusqlite::Connection;
use std::{collections::HashMap, env, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{mpsc::Sender, Mutex as AsyncMutex, RwLock as AsyncRwLock};
use uuid::Uuid;

use crate::config::{self, SettingsState, SettingsUpdate, UserFrontmatter};
use crate::consent::ConsentBridge;
use crate::execution_plan::WorkerDescriptor;
use crate::external_api::{ExternalApiConfig, ExternalProviderFactory, ProviderFactory};
use crate::gateway::{
    self, GatewayControlPlane, GatewayMessageRecord, GatewayRpcRequest, GatewayRpcResponse,
    GatewaySearchHit, GatewayServer, GatewaySessionContext, GatewaySessionRecord,
    SessionMessageRole, SessionSource,
};
use crate::gateway_transport::{GatewayRpcHandler, GatewayServerInfo, GatewayTransportServer};
use crate::hooks::run_active_memory_prelude;
use crate::injector::InjectorConfig;
use crate::manifest::AgentManifest;
use crate::memory::{
    self, build_overview as build_memory_overview, build_prompt_memory_block, clear_knowledge,
    delete_knowledge, store_knowledge, KnowledgeKind, PromptMemoryOptions,
};
use crate::meta_brain::{prepare_policy_table, MetaBrain, NodeId, PermissionFlags};
use crate::orchestrator::{
    LlmProvider, Orchestrator, PlannerAgent, PlannerConfig, SubTaskResult, TaskStatus,
    UiActionBridge, WorkerAgent, WorkerConfig,
};

use crate::plugins::ensure_plugins_vault;
use crate::rule_store;
use crate::skills::{ensure_skills_vault, prepare_skills_table};
use crate::swarm::{SwarmGateway, Workspace};
use crate::trust_store::TrustStore;

use crate::{MoatError, MoatResult};

const FACTORY_DEFAULT_CHAT_MODEL: &str = "meta/llama-3.1-70b-instruct";
const FACTORY_DEFAULT_PLANNER_MODEL: &str = "meta/llama-3.1-405b-instruct";
const FACTORY_DEFAULT_WORKER_MODEL: &str = "meta/llama-3.1-70b-instruct";

const SESSION_COMPACTION_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_DESKTOP_SESSION_TITLE: &str = "MoatAgeex Desktop";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionFastPath {
    DirectChat,
    Specialist(&'static str),
    Assistant,
}

#[derive(Debug, Clone)]
pub struct MoatSystemConfig {
    pub db_path: PathBuf,
    pub planner: PlannerConfig,
    pub worker: WorkerConfig,
    pub process_timeout: Duration,
    pub external_apis: ExternalApiConfig,
    pub swarm_workspace_root: Option<PathBuf>,
    pub default_swarm_agent: Option<String>,
}

#[derive(Clone)]
struct DynamicProvider {
    factory: Arc<dyn ProviderFactory>,
    subsystem: String,
}

impl LlmProvider for DynamicProvider {
    fn complete<'a>(
        &'a self,
        model: &'a str,
        system: &'a str,
        user: &'a str,
        max_tokens: usize,
        temperature: f32,
    ) -> crate::orchestrator::BoxFuture<'a, MoatResult<(String, u32)>> {
        Box::pin(async move {
            let (provider, routed_model) = self.factory.routed_provider(&self.subsystem, model)?;
            provider
                .complete(&routed_model, system, user, max_tokens, temperature)
                .await
        })
    }

    fn check_health<'a>(&'a self) -> crate::orchestrator::BoxFuture<'a, MoatResult<()>> {
        Box::pin(async move {
            let (provider, _model) = self.factory.routed_provider(&self.subsystem, "")?;
            provider.check_health().await
        })
    }
}

#[derive(Debug, Clone)]
pub struct HostEnvironment {
    pub os_name: String,
    pub username: String,
    pub home_dir: PathBuf,
}

impl HostEnvironment {
    pub fn detect() -> Self {
        let os_name = match env::consts::OS {
            "macos" => "macOS",
            "windows" => "Windows",
            "linux" => "Linux",
            other => other,
        }
        .to_string();
        let username = env::var("USER")
            .or_else(|_| env::var("USERNAME"))
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let home_dir = dirs::home_dir()
            .or_else(|| env::var_os("HOME").map(PathBuf::from))
            .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/"));
        Self {
            os_name,
            username,
            home_dir,
        }
    }

    pub fn to_xml(&self) -> String {
        format!(
            "<host_environment>\nCurrent Time: {}\nOS: {}\nUser: {}\nHome Directory: {}\n</host_environment>",
            Local::now().format("%Y-%m-%d %H:%M:%S"),
            self.os_name,
            self.username,
            self.home_dir.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::MoatSystem;
    use crate::config::ProviderHealth;
    use crate::external_api::{ExternalApiConfig, ProviderFactory};
    use crate::gateway::{GatewayControlPlane, GatewayServer, SessionSource};
    use crate::injector::InjectorConfig;
    use crate::memory::{self, KnowledgeKind};
    use crate::meta_brain::MetaBrain;
    use crate::orchestrator::{
        BoxFuture, LlmProvider, Orchestrator, PlannerAgent, PlannerConfig, WorkerAgent,
        WorkerConfig,
    };
    use crate::trust_store::TrustStore;

    use crate::MoatResult;
    use dashmap::DashMap;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::{Mutex as AsyncMutex, RwLock as AsyncRwLock};

    struct StubProvider {
        response: Arc<Mutex<Option<(String, u32)>>>,
    }

    struct TestProviderFactory {
        provider: Arc<dyn LlmProvider>,
    }

    impl StubProvider {
        fn new(xml: &str) -> Self {
            Self {
                response: Arc::new(Mutex::new(Some((xml.to_string(), 42)))),
            }
        }
    }

    impl LlmProvider for StubProvider {
        fn complete<'a>(
            &'a self,
            _model: &'a str,
            _system: &'a str,
            _user: &'a str,
            _max_tokens: usize,
            _temperature: f32,
        ) -> BoxFuture<'a, MoatResult<(String, u32)>> {
            Box::pin(async move {
                Ok(self
                    .response
                    .lock()
                    .expect("lock stub response")
                    .take()
                    .expect("stub response"))
            })
        }
    }

    impl ProviderFactory for TestProviderFactory {
        fn provider_for(&self, _api_provider: &str) -> MoatResult<Arc<dyn LlmProvider>> {
            Ok(Arc::clone(&self.provider))
        }

        fn routed_provider(
            &self,
            _subsystem: &str,
            requested_model: &str,
        ) -> MoatResult<(Arc<dyn LlmProvider>, String)> {
            Ok((Arc::clone(&self.provider), requested_model.to_string()))
        }

        fn invalidate_cache(&self) {}

        fn has_stored_key(&self, _api_provider: &str) -> MoatResult<bool> {
            Ok(false)
        }

        fn set_provider_key(&self, _api_provider: &str, _api_key: &str) -> MoatResult<()> {
            Ok(())
        }

        fn remove_provider_key(&self, _api_provider: &str) -> MoatResult<()> {
            Ok(())
        }

        fn set_default_provider(&self, _provider: &str) {}

        fn get_default_provider(&self) -> String {
            "nvidia".to_string()
        }

        fn provider_health<'a>(
            &'a self,
            _api_provider: &'a str,
        ) -> BoxFuture<'a, MoatResult<ProviderHealth>> {
            Box::pin(async { Ok(ProviderHealth::Missing) })
        }

        fn provider_variant_health<'a>(
            &'a self,
            _api_provider: &'a str,
            _variant: Option<&'a str>,
        ) -> BoxFuture<'a, MoatResult<ProviderHealth>> {
            Box::pin(async { Ok(ProviderHealth::Missing) })
        }
    }

    #[tokio::test]
    async fn hard_reset_memory_clears_sqlite_and_hdc() {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        MoatSystem::prepare_database(&conn).expect("prepare database");
        memory::store_knowledge(
            &conn,
            KnowledgeKind::Fact,
            "test",
            Some("User fact"),
            "User said their name is Harsha",
            Some(123),
        )
        .expect("insert durable knowledge");

        let db = Arc::new(AsyncMutex::new(conn));
        let provider = Arc::new(StubProvider::new(
            "<knowledge_memory><facts><fact>Name: Harsha</fact></facts></knowledge_memory>",
        ));
        let meta_brain = Arc::new(AsyncRwLock::new(MetaBrain::new()));
        let planner = PlannerAgent::new(
            PlannerConfig {
                model: "stub".into(),
                max_subtasks: 15,
                temperature: 0.0,
                max_tokens: 128,
            },
            provider.clone(),
        );
        let worker = WorkerAgent::new(
            WorkerConfig {
                model: "stub".into(),
                command_timeout: Duration::from_secs(5),
                allow_high_risk: true,
                external_apis: ExternalApiConfig::default(),
            },
            Arc::clone(&meta_brain),
            Arc::clone(&db),
        ).expect("test worker");
        let provider_factory: Arc<dyn ProviderFactory> = Arc::new(TestProviderFactory {
            provider: provider.clone(),
        });
        let trust_store = Arc::new(TrustStore::new());
        let orchestrator = Arc::new(Orchestrator::new(
            planner,
            worker,
            Arc::clone(&meta_brain),
            Arc::clone(&db),
            InjectorConfig {
                top_k_memories: 5,
                max_memory_chars: 600,
            },
        ));

        let gateway = Arc::new(GatewayControlPlane::default());
        let gateway_server = Arc::new(GatewayServer::with_default_adapters(Arc::clone(&gateway)));
        let system = MoatSystem {
            meta_brain: Arc::clone(&meta_brain),
            db: Arc::clone(&db),
            gateway,
            gateway_server,
            gateway_transport: Arc::new(AsyncMutex::new(None)),
            session_lanes: Arc::new(DashMap::new()),
            provider_factory,
            orchestrator,
            process_timeout: Duration::from_secs(5),
            memory_top_k: 5,
            memory_max_chars: 600,
            planner_defaults: PlannerConfig {
                model: "stub-planner".into(),
                max_subtasks: 15,
                temperature: 0.0,
                max_tokens: 128,
            },
            trust_store,
            swarm_workspace_root: None,
            default_swarm_agent_preference: None,
            swarm_gateway: Arc::new(AsyncRwLock::new(None)),
            default_swarm_agent: Arc::new(AsyncRwLock::new(None)),
        };

        system.hard_reset_memory().await.expect("hard reset");

        let db = db.lock().await;
        let knowledge_count: i64 = db
            .query_row("SELECT COUNT(*) FROM knowledge_memory", [], |row| {
                row.get(0)
            })
            .expect("knowledge count");
        assert_eq!(knowledge_count, 0);
        drop(db);
    }

    #[test]
    fn execution_world_state_denial_deduplication() {
        let mut state = super::ExecutionWorldState::default();
        state.record_denial("fs:read", "/tmp/secret");
        state.record_denial("fs:read", "/tmp/secret");
        state.record_denial("process:spawn", "");
        assert_eq!(state.denied_capabilities.len(), 2);
        assert_eq!(state.denied_capabilities[0], "fs:read:/tmp/secret");
        assert_eq!(state.denied_capabilities[1], "process:spawn");
    }

    #[test]
    fn execution_world_state_is_capability_denied_exact_and_broad() {
        let mut state = super::ExecutionWorldState::default();
        state.record_denial("fs:read", "/tmp/secret");
        state.record_denial("process:spawn", "");

        // Exact match
        assert!(state.is_capability_denied("fs:read", "/tmp/secret"));
        // Broad match (bare action matches any target)
        assert!(state.is_capability_denied("process:spawn", "anything"));
        // Non-matching
        assert!(!state.is_capability_denied("fs:write", "/tmp/secret"));
        assert!(!state.is_capability_denied("fs:read", "/other/path"));
    }

    #[test]
    fn execution_world_state_to_xml_output_format() {
        let mut state = super::ExecutionWorldState::default();
        state.record_denial("fs:read", "/tmp/secret");
        state.record_completion("task_1", "core_os", "Opened Safari", true);
        state.record_completion("task_2", "coder", "Permission denied", false);
        state.record_artifact("task_1", "/tmp/output.txt");

        let xml = state.to_xml_context();
        assert!(xml.contains("<execution_world_state>"));
        assert!(xml.contains("</execution_world_state>"));
        assert!(xml.contains("<denied_capabilities>"));
        assert!(xml.contains("fs:read:/tmp/secret"));
        assert!(xml.contains("<completed_subtasks>"));
        assert!(xml.contains("[SUCCESS] task_1 (core_os): Opened Safari"));
        assert!(xml.contains("[FAILED] task_2 (coder): Permission denied"));
        assert!(xml.contains("<live_artifacts>"));
        assert!(xml.contains("/tmp/output.txt"));
    }

    #[test]
    fn execution_world_state_extract_denials_from_summary() {
        let mut state = super::ExecutionWorldState::default();
        let summary = "CAPABILITY DENIED: path '/etc/shadow' is outside allowed paths\nSome other line\nPERMISSION DENIED: write access blocked";
        state.extract_denials_from_summary(summary);

        assert_eq!(state.denied_capabilities.len(), 2);
        assert!(state
            .denied_capabilities
            .iter()
            .any(|d| d.contains("path '/etc/shadow'")));
        assert!(state
            .denied_capabilities
            .iter()
            .any(|d| d.contains("write access blocked")));

        // Dedup on re-extract
        state.extract_denials_from_summary(summary);
        assert_eq!(state.denied_capabilities.len(), 2);
    }

    #[test]
    fn authoritative_loop_keeps_specialists_as_hints_only() {
        assert_eq!(
            MoatSystem::authoritative_specialization_hint("open safari", SessionSource::DesktopUi),
            Some("core_os")
        );
        assert_eq!(
            MoatSystem::authoritative_specialization_hint("hello there", SessionSource::DesktopUi),
            None
        );
        assert_eq!(
            MoatSystem::authoritative_specialization_hint(
                "implement the parser fix",
                SessionSource::DesktopUi
            ),
            None
        );
    }
}

fn gateway_role_for_source(source: SessionSource) -> gateway::GatewayClientRole {
    match source {
        SessionSource::DesktopUi | SessionSource::CommandBar => {
            gateway::GatewayClientRole::DesktopClient
        }
        SessionSource::WebChat => gateway::GatewayClientRole::WebChatClient,
        SessionSource::Cli => gateway::GatewayClientRole::CliClient,
        SessionSource::Automation => gateway::GatewayClientRole::AutomationClient,
        SessionSource::WhatsApp
        | SessionSource::Discord
        | SessionSource::Slack
        | SessionSource::Telegram
        | SessionSource::Signal => gateway::GatewayClientRole::ChannelAdapter,
        SessionSource::System => gateway::GatewayClientRole::Node,
        SessionSource::Api => gateway::GatewayClientRole::ControlPlaneClient,
    }
}

pub use crate::memory::{KnowledgeRecord, MemoryOverview};

/// Typed shared execution state for a single workflow run.
/// Replaces the unstructured String scratchpad with structured fields
/// that provide mechanical capability denial filtering and clean prompt injection.
#[derive(Debug, Clone, Default)]
pub struct ExecutionWorldState {
    /// Capability+target pairs denied during this workflow run.
    /// Format: "action:target" e.g. "fs:read:/tmp/secret", or bare "process:spawn".
    pub denied_capabilities: Vec<String>,
    /// Key-value facts discovered by workers (e.g. OS version, file paths found).
    pub discovered_facts: HashMap<String, String>,
    /// Task IDs that completed with a one-line summary. Format: "[SUCCESS|FAILED] task_id (worker): summary"
    pub completed_subtasks: Vec<String>,
    /// Artifact paths produced by workers, keyed by task_id.
    pub live_artifacts: HashMap<String, String>,
}

impl ExecutionWorldState {
    /// Maximum characters allowed in the serialized XML to prevent token budget exhaustion.
    const MAX_XML_CHARS: usize = 4000;

    /// Serialize to structured XML for prompt injection.
    /// Caps output at `MAX_XML_CHARS` by truncating oldest completed_subtasks first,
    /// since denied_capabilities are the most critical data for downstream workers.
    pub fn to_xml_context(&self) -> String {
        let mut xml = String::with_capacity(Self::MAX_XML_CHARS);
        xml.push_str("<execution_world_state>\n");

        if !self.denied_capabilities.is_empty() {
            xml.push_str("<denied_capabilities>\n");
            for cap in &self.denied_capabilities {
                xml.push_str("- ");
                xml.push_str(cap);
                xml.push('\n');
            }
            xml.push_str("</denied_capabilities>\n");
        }

        if !self.completed_subtasks.is_empty() {
            xml.push_str("<completed_subtasks>\n");
            // If we're over budget, skip oldest entries (keep denials + newest completions)
            let skip = if xml.len() > Self::MAX_XML_CHARS / 2 {
                self.completed_subtasks.len().saturating_sub(3)
            } else {
                0
            };
            for entry in self.completed_subtasks.iter().skip(skip) {
                xml.push_str("- ");
                xml.push_str(entry);
                xml.push('\n');
            }
            xml.push_str("</completed_subtasks>\n");
        }

        if !self.discovered_facts.is_empty() {
            xml.push_str("<discovered_facts>\n");
            for (key, value) in &self.discovered_facts {
                xml.push_str("- ");
                xml.push_str(key);
                xml.push_str(": ");
                xml.push_str(value);
                xml.push('\n');
            }
            xml.push_str("</discovered_facts>\n");
        }

        if !self.live_artifacts.is_empty() {
            xml.push_str("<live_artifacts>\n");
            for (task_id, path) in &self.live_artifacts {
                xml.push_str("- ");
                xml.push_str(task_id);
                xml.push_str(": ");
                xml.push_str(path);
                xml.push('\n');
            }
            xml.push_str("</live_artifacts>\n");
        }

        xml.push_str("</execution_world_state>");

        // Hard cap: trim from the middle (completed_subtasks) if still over budget
        if xml.len() > Self::MAX_XML_CHARS {
            xml.truncate(Self::MAX_XML_CHARS);
            // Find last clean newline to avoid mid-tag truncation
            if let Some(pos) = xml.rfind('\n') {
                xml.truncate(pos + 1);
            }
            xml.push_str("</execution_world_state>");
        }

        xml
    }

    /// Check if a specific action+target combination was already denied.
    /// Matches both exact "action:target" and broad "action" denials.
    pub fn is_capability_denied(&self, action: &str, target: &str) -> bool {
        let exact_key = format!("{}:{}", action, target.trim());
        self.denied_capabilities
            .iter()
            .any(|d| d == &exact_key || d == action)
    }

    /// Record a denied capability. Deduplicates automatically.
    pub fn record_denial(&mut self, action: &str, target: &str) {
        let key = if target.trim().is_empty() {
            action.to_string()
        } else {
            format!("{}:{}", action, target.trim())
        };
        if !self.denied_capabilities.contains(&key) {
            self.denied_capabilities.push(key);
        }
    }

    /// Record a completed subtask with summary.
    pub fn record_completion(&mut self, task_id: &str, worker: &str, summary: &str, success: bool) {
        let tag = if success { "SUCCESS" } else { "FAILED" };
        let clipped_summary: String = summary.chars().take(200).collect();
        self.completed_subtasks.push(format!(
            "[{}] {} ({}): {}",
            tag, task_id, worker, clipped_summary
        ));
    }

    /// Record a produced artifact.
    pub fn record_artifact(&mut self, task_id: &str, path: &str) {
        self.live_artifacts
            .insert(task_id.to_string(), path.to_string());
    }

    /// Extract denied capabilities from a worker failure summary.
    /// Parses the "CAPABILITY DENIED" prefix used consistently across the orchestrator.
    pub fn extract_denials_from_summary(&mut self, summary: &str) {
        for line in summary.lines() {
            let line = line.trim();
            if let Some(rest) = line
                .strip_prefix("CAPABILITY DENIED:")
                .or_else(|| line.strip_prefix("PERMISSION DENIED:"))
            {
                let denial = rest.trim();
                if !denial.is_empty() && !self.denied_capabilities.contains(&denial.to_string()) {
                    self.denied_capabilities.push(denial.to_string());
                }
            }
        }
    }

    /// Check if the world state is empty (no data recorded yet).
    pub fn is_empty(&self) -> bool {
        self.denied_capabilities.is_empty()
            && self.discovered_facts.is_empty()
            && self.completed_subtasks.is_empty()
            && self.live_artifacts.is_empty()
    }
}

pub struct MoatSystem {
    pub meta_brain: Arc<AsyncRwLock<MetaBrain>>,
    db: Arc<AsyncMutex<Connection>>,
    gateway: Arc<GatewayControlPlane>,
    gateway_server: Arc<GatewayServer>,
    gateway_transport: Arc<AsyncMutex<Option<GatewayTransportServer>>>,
    session_lanes: Arc<DashMap<Uuid, Arc<AsyncMutex<()>>>>,
    provider_factory: Arc<dyn ProviderFactory>,

    pub orchestrator: Arc<Orchestrator>,
    pub process_timeout: Duration,
    pub memory_top_k: usize,
    pub memory_max_chars: usize,
    planner_defaults: PlannerConfig,
    trust_store: Arc<TrustStore>,

    swarm_workspace_root: Option<PathBuf>,
    default_swarm_agent_preference: Option<String>,
    pub swarm_gateway: Arc<AsyncRwLock<Option<Arc<SwarmGateway>>>>,
    pub default_swarm_agent: Arc<AsyncRwLock<Option<String>>>,
}

impl MoatSystem {
    fn env_usize(key: &str, default: usize) -> usize {
        std::env::var(key)
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default)
    }

    fn env_list(key: &str) -> Vec<String> {
        std::env::var(key)
            .ok()
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn clip_text(text: &str, max_chars: usize) -> String {
        if max_chars == 0 {
            return String::new();
        }
        let clipped: String = text.chars().take(max_chars).collect();
        if clipped.len() == text.len() {
            clipped
        } else {
            format!("{clipped}…")
        }
    }

    async fn load_memory_context_block(&self) -> String {
        let db = self.db.lock().await;
        build_prompt_memory_block(
            &db,
            &PromptMemoryOptions {
                max_entries: self.memory_top_k.max(8),
                max_chars_per_entry: self.memory_max_chars.min(260),
            },
        )
        .unwrap_or_else(|_| {
            "<knowledge_memory>\nNo durable knowledge recorded yet.\n</knowledge_memory>"
                .to_string()
        })
    }

    async fn remember_knowledge(
        &self,
        kind: KnowledgeKind,
        source: &str,
        title: Option<&str>,
        text: &str,
        timestamp: u64,
    ) -> MoatResult<String> {
        let id = {
            let db = self.db.lock().await;
            store_knowledge(&db, kind, source, title, text, Some(timestamp))?
        };
        Ok(id)
    }

    fn build_live_provider(
        factory: &Arc<dyn ProviderFactory>,
        subsystem: &str,
    ) -> MoatResult<Arc<dyn LlmProvider>> {
        Ok(Arc::new(DynamicProvider {
            factory: Arc::clone(factory),
            subsystem: subsystem.to_string(),
        }))
    }

    async fn build_swarm_gateway(
        workspace_root: Option<PathBuf>,
        provider_factory: Arc<dyn ProviderFactory>,
        meta_brain: Arc<AsyncRwLock<MetaBrain>>,
        planner_defaults: PlannerConfig,
        injector_config: InjectorConfig,
        default_agent_preference: Option<String>,
    ) -> (Option<Arc<SwarmGateway>>, Option<String>, Option<PathBuf>) {
        let Some(root) = Self::resolve_swarm_root(workspace_root) else {
            return (None, default_agent_preference, None);
        };

        match Workspace::ensure_scaffold(&root).await {
            Ok(scaffold_root) => match Workspace::load(&scaffold_root).await {
                Ok(workspace) => {
                    let gateway = Arc::new(SwarmGateway::new(
                        workspace,
                        meta_brain,
                        provider_factory,
                        planner_defaults,
                        injector_config,
                    ));
                    let default_agent = default_agent_preference
                        .clone()
                        .or_else(|| gateway.default_agent_name());
                    (Some(gateway), default_agent, Some(scaffold_root))
                }
                Err(err) => {
                    eprintln!(
                        "[Swarm] failed to load workspace {}: {err}",
                        scaffold_root.display()
                    );
                    (None, default_agent_preference, Some(scaffold_root))
                }
            },
            Err(err) => {
                eprintln!(
                    "[Swarm] failed to scaffold workspace {}: {err}",
                    root.display()
                );
                (None, default_agent_preference, Some(root))
            }
        }
    }

    pub async fn init(cfg: MoatSystemConfig) -> MoatResult<Self> {
        Self::init_internal(cfg, None, None).await
    }

    pub async fn init_with_consent(
        cfg: MoatSystemConfig,
        consent_bridge: Arc<ConsentBridge>,
    ) -> MoatResult<Self> {
        Self::init_internal(cfg, Some(consent_bridge), None).await
    }

    pub async fn init_with_consent_and_ui_bridge(
        cfg: MoatSystemConfig,
        consent_bridge: Arc<ConsentBridge>,
        ui_bridge: Arc<dyn UiActionBridge>,
    ) -> MoatResult<Self> {
        Self::init_internal(cfg, Some(consent_bridge), Some(ui_bridge)).await
    }

    async fn init_internal(
        cfg: MoatSystemConfig,
        consent_bridge: Option<Arc<ConsentBridge>>,
        ui_bridge: Option<Arc<dyn UiActionBridge>>,
    ) -> MoatResult<Self> {
        config::ensure_moat_root()?;
        if let Some(parent) = cfg.db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                MoatError::Settings(format!("failed to prepare database root: {err}"))
            })?;
        }
        let memory_capacity = Self::env_usize("MOAT_MEMORY_CAPACITY", 1000);
        let memory_db_retention = Self::env_usize(
            "MOAT_MEMORY_DB_RETENTION",
            memory_capacity.saturating_mul(4).max(1),
        );

        let conn = Connection::open(&cfg.db_path).map_err(MoatError::Database)?;
        Self::prepare_database(&conn)?;
        Self::prune_memory_retention(&conn, memory_db_retention)?;

        let brain_path = if cfg.db_path == config::sqlite_db_path() {
            config::meta_brain_bin_path()
        } else {
            cfg.db_path.with_extension("bin")
        };
        if let Some(parent) = brain_path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                MoatError::Settings(format!("failed to prepare metabrain root: {err}"))
            })?;
        }
        let meta_brain = MetaBrain::load_or_new(brain_path).await?;

        ensure_skills_vault()?;
        ensure_plugins_vault()?;
        let gateway = Arc::new(GatewayControlPlane::default());
        let gateway_server = Arc::new(GatewayServer::with_default_adapters(Arc::clone(&gateway)));
        gateway_server.load_trusted_devices(&conn)?;

        let resolved_swarm_root = Self::resolve_swarm_root(cfg.swarm_workspace_root.clone());
        let (initial_user_doc, resolved_user_md_path) = if let Some(root) = &resolved_swarm_root {
            let scaffold_root = Workspace::ensure_scaffold(root).await?;
            let user_md_path = scaffold_root.join("USER.md");
            (
                config::load_user_document(&user_md_path).await?,
                user_md_path,
            )
        } else {
            (config::default_user_document(), config::user_md_path())
        };

        let mut external_api_config = cfg.external_apis.clone();
        external_api_config.user_md_path = resolved_user_md_path;

        let provider_factory: Arc<dyn ProviderFactory> =
            Arc::new(ExternalProviderFactory::new(external_api_config));
        let _effective_chat_model = initial_user_doc
            .frontmatter
            .chat_model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(FACTORY_DEFAULT_CHAT_MODEL)
            .to_string();
        let effective_planner_model = initial_user_doc
            .frontmatter
            .planner_model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(FACTORY_DEFAULT_PLANNER_MODEL)
            .to_string();
        let effective_worker_model = initial_user_doc
            .frontmatter
            .worker_model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(FACTORY_DEFAULT_WORKER_MODEL)
            .to_string();
        let trust_store = Arc::new(TrustStore::new());
        let planner_provider = Self::build_live_provider(&provider_factory, "planner")?;
        let mut planner_defaults = cfg.planner.clone();
        planner_defaults.model = effective_planner_model.clone();

        let db = Arc::new(AsyncMutex::new(conn));
        let meta_brain = Arc::new(AsyncRwLock::new(meta_brain));
        MetaBrain::spawn_flush_loop(Arc::clone(&meta_brain));

        let injector_config = InjectorConfig {
            top_k_memories: Self::env_usize("MOAT_MEMORY_TOP_K", 5),
            max_memory_chars: Self::env_usize("MOAT_MEMORY_MAX_CHARS", 600),
        };

        let mut worker_cfg = cfg.worker;
        worker_cfg.model = effective_worker_model.clone();
        let worker = match consent_bridge {
            Some(bridge) => WorkerAgent::with_consent_bridge(
                worker_cfg.clone(),
                Arc::clone(&meta_brain),
                Arc::clone(&db),
                bridge,
            ).map_err(|e| MoatError::Orchestrator(e.to_string()))?,
            None => WorkerAgent::new(worker_cfg, Arc::clone(&meta_brain), Arc::clone(&db))
                .map_err(|e| MoatError::Orchestrator(e.to_string()))?,
        };
        let (swarm_gateway, default_swarm_agent, swarm_workspace_root) = Self::build_swarm_gateway(
            resolved_swarm_root,
            Arc::clone(&provider_factory),
            Arc::clone(&meta_brain),
            planner_defaults.clone(),
            injector_config.clone(),
            cfg.default_swarm_agent.clone(),
        )
        .await;

        let planner = PlannerAgent::new(planner_defaults.clone(), Arc::clone(&planner_provider));
        let mut orchestrator = Orchestrator::new(
            planner,
            worker,
            Arc::clone(&meta_brain),
            Arc::clone(&db),
            injector_config.clone(),
        );
        if let Some(bridge) = ui_bridge {
            orchestrator = orchestrator.with_ui_bridge(bridge);
        }
        let orchestrator = Arc::new(orchestrator);
        if let Some(gateway) = &swarm_gateway {
            trust_store.clear_builtin_workers();
            for spec in &gateway.workspace.agents {
                trust_store.register_builtin_worker(&spec.name);
            }
        }

        Ok(Self {
            meta_brain,
            db,
            gateway,
            gateway_server,
            gateway_transport: Arc::new(AsyncMutex::new(None)),
            session_lanes: Arc::new(DashMap::new()),
            provider_factory,

            orchestrator,
            process_timeout: cfg.process_timeout,
            memory_top_k: injector_config.top_k_memories,
            memory_max_chars: injector_config.max_memory_chars,
            planner_defaults,
            trust_store,
            swarm_workspace_root,
            default_swarm_agent_preference: cfg.default_swarm_agent,
            swarm_gateway: Arc::new(AsyncRwLock::new(swarm_gateway)),
            default_swarm_agent: Arc::new(AsyncRwLock::new(default_swarm_agent)),
        })
    }

    fn resolve_swarm_root(workspace_root: Option<PathBuf>) -> Option<PathBuf> {
        let workspace_root = workspace_root.map(|path| {
            if path.as_os_str().is_empty() {
                Workspace::default_root()
            } else {
                path
            }
        });

        workspace_root.or_else(|| {
            let current = std::env::current_dir().ok()?;
            if current.join("USER.md").is_file() && current.join("SOUL.md").is_file() {
                Some(current)
            } else {
                Some(Workspace::default_root())
            }
        })
    }

    fn prepare_database(conn: &Connection) -> MoatResult<()> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;",
        )
        .map_err(MoatError::Database)?;
        memory::prepare_tables(conn)?;
        gateway::prepare_tables(conn)?;
        prepare_policy_table(conn)?;
        rule_store::prepare_tables(conn)?;
        prepare_skills_table(conn)?;

        Ok(())
    }

    fn prune_memory_retention(conn: &Connection, keep: usize) -> MoatResult<()> {
        let ids = memory::list_knowledge(conn, keep)?
            .into_iter()
            .map(|record| record.id)
            .collect::<Vec<_>>();
        if ids.is_empty() {
            clear_knowledge(conn)?;
            return Ok(());
        }
        let placeholders = std::iter::repeat("?")
            .take(ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "DELETE FROM knowledge_memory WHERE id NOT IN ({})",
            placeholders
        );
        let params = rusqlite::params_from_iter(ids.iter());
        conn.execute(&sql, params).map_err(MoatError::Database)?;
        Ok(())
    }

    #[cfg(debug_assertions)]
    pub async fn seed_root_environment(&self, root_path: &str) -> MoatResult<()> {
        let snapshot = {
            let mut brain = self.meta_brain.write().await;

            // User requested: "global read allowed"
            let _ = brain.grant_path_permission("/", PermissionFlags::FILE_READ);
            // User requested: "apps/browser navigation allowed"
            let _ = brain.grant_app_permission("*", PermissionFlags::APP_LAUNCH);
            let _ = brain.grant_url_scheme_permission("*", PermissionFlags::URL_OPEN);
            let _ = brain.grant_domain_permission("*", PermissionFlags::NET_OUTBOUND);
            // Allow basic system navigation via tools
            let _ = brain.grant_tool_permission(
                "os:applescript",
                PermissionFlags::TOOL_EXEC | PermissionFlags::EXEC,
            );
            let _ = brain.grant_tool_permission(
                "osascript",
                PermissionFlags::TOOL_EXEC | PermissionFlags::EXEC,
            );
            let _ = brain
                .grant_tool_permission("open", PermissionFlags::TOOL_EXEC | PermissionFlags::EXEC);

            // Scoped write access for the workspace
            brain.grant_path_permission(
                root_path,
                PermissionFlags::FILE_READ | PermissionFlags::FILE_WRITE,
            )?;
            brain.snapshot()
        };
        let _ = snapshot;
        MetaBrain::flush_arc(Arc::clone(&self.meta_brain)).await
    }

    #[cfg(debug_assertions)]
    pub async fn seed_test_environment(&self) -> MoatResult<()> {
        let default_root = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .to_string_lossy()
            .into_owned();
        let test_root = std::env::var("MOAT_TEST_ROOT").unwrap_or(default_root);
        self.seed_root_environment(&test_root).await?;

        let snapshot = {
            let mut brain = self.meta_brain.write().await;
            for tool in Self::env_list("MOAT_ALLOWED_CLI_TOOLS") {
                let _ = brain.grant_tool_permission(
                    &tool,
                    PermissionFlags::TOOL_EXEC | PermissionFlags::EXEC,
                );
            }
            for app in Self::env_list("MOAT_ALLOWED_APPS") {
                let _ = brain.grant_app_permission(&app, PermissionFlags::APP_LAUNCH);
            }
            for scheme in Self::env_list("MOAT_ALLOWED_URL_SCHEMES") {
                let _ = brain.grant_url_scheme_permission(&scheme, PermissionFlags::URL_OPEN);
            }
            for domain in Self::env_list("MOAT_ALLOWED_CURL_DOMAINS") {
                let _ = brain.grant_domain_permission(&domain, PermissionFlags::NET_OUTBOUND);
            }
            brain.snapshot()
        };
        let _ = snapshot;
        MetaBrain::flush_arc(Arc::clone(&self.meta_brain)).await
    }

    async fn ensure_source_session(
        &self,
        source: SessionSource,
        channel: Option<&str>,
    ) -> MoatResult<Uuid> {
        let db = self.db.lock().await;
        let (_client, session_id) = self.gateway_server.ensure_session_for_client(
            &db,
            gateway_role_for_source(source),
            source,
            channel,
            Some(DEFAULT_DESKTOP_SESSION_TITLE),
        )?;
        Ok(session_id)
    }

    async fn append_session_message(
        &self,
        session_id: Uuid,
        role: SessionMessageRole,
        content: &str,
    ) -> MoatResult<()> {
        let db = self.db.lock().await;
        self.gateway
            .append_message(&db, session_id, role, content)?;
        Ok(())
    }

    pub async fn list_gateway_sessions(
        &self,
        limit: usize,
    ) -> MoatResult<Vec<GatewaySessionRecord>> {
        let db = self.db.lock().await;
        self.gateway_server.list_sessions(&db, limit)
    }

    pub async fn get_gateway_session_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> MoatResult<Vec<GatewayMessageRecord>> {
        let parsed = Uuid::parse_str(session_id).map_err(|err| {
            MoatError::InvariantViolation(format!(
                "invalid gateway session id '{session_id}': {err}"
            ))
        })?;
        let db = self.db.lock().await;
        self.gateway_server
            .load_session_messages(&db, parsed, limit)
    }

    pub async fn search_gateway_messages(
        &self,
        query: &str,
        limit: usize,
    ) -> MoatResult<Vec<GatewaySearchHit>> {
        let db = self.db.lock().await;
        self.gateway_server.search_messages(&db, query, limit)
    }

    pub async fn get_gateway_session_context(
        &self,
        session_id: &str,
        max_chars: usize,
    ) -> MoatResult<GatewaySessionContext> {
        let session_id = Uuid::parse_str(session_id).map_err(|err| {
            MoatError::Settings(format!("invalid session id '{session_id}': {err}"))
        })?;
        let db = self.db.lock().await;
        self.gateway_server
            .load_session_context(&db, session_id, max_chars)
    }

    pub async fn handle_gateway_rpc(
        &self,
        request: GatewayRpcRequest,
        status_tx: Option<Sender<String>>,
    ) -> MoatResult<GatewayRpcResponse> {
        match request {
            GatewayRpcRequest::ExecutePrompt {
                source,
                channel,
                text,
                root_node_id,
            } => {
                let session_id = self
                    .ensure_source_session(source, channel.as_deref())
                    .await?;
                let results = self
                    .process_with_source(&text, root_node_id, status_tx, source, channel.as_deref())
                    .await?;
                Ok(GatewayRpcResponse::Execution {
                    session_id,
                    results,
                })
            }
            GatewayRpcRequest::ListSessions { limit } => Ok(GatewayRpcResponse::Sessions {
                sessions: self.list_gateway_sessions(limit).await?,
            }),
            GatewayRpcRequest::GetSessionMessages { session_id, limit } => {
                Ok(GatewayRpcResponse::Messages {
                    messages: self
                        .get_gateway_session_messages(&session_id, limit)
                        .await?,
                })
            }
            GatewayRpcRequest::GetSessionContext {
                session_id,
                max_chars,
            } => Ok(GatewayRpcResponse::SessionContext {
                context: self
                    .get_gateway_session_context(&session_id, max_chars)
                    .await?,
            }),
            GatewayRpcRequest::SearchMessages { query, limit } => {
                Ok(GatewayRpcResponse::SearchResults {
                    hits: self.search_gateway_messages(&query, limit).await?,
                })
            }
            GatewayRpcRequest::ListGatewayClients => Ok(GatewayRpcResponse::Clients {
                clients: self.gateway_server.list_clients(),
            }),
            GatewayRpcRequest::ListGatewayPairingRequests => {
                Ok(GatewayRpcResponse::PairingRequests {
                    requests: self.gateway_server.list_pairing_requests(),
                })
            }
            GatewayRpcRequest::ApproveGatewayPairing { pairing_code } => {
                let db = self.db.lock().await;
                Ok(GatewayRpcResponse::PairingApproval {
                    device: self
                        .gateway_server
                        .approve_pairing_with_store(&db, &pairing_code)?,
                })
            }
            GatewayRpcRequest::ListTrustedGatewayDevices => {
                Ok(GatewayRpcResponse::TrustedDevices {
                    devices: self.gateway_server.list_trusted_devices(),
                })
            }
            GatewayRpcRequest::RevokeGatewayDeviceTrust { device_id, role } => {
                let db = self.db.lock().await;
                Ok(GatewayRpcResponse::DeviceTrustRevoked {
                    revoked: self
                        .gateway_server
                        .revoke_device_trust_with_store(&db, &device_id, role)?,
                })
            }
        }
    }

    pub async fn gateway_server_info(&self) -> Option<GatewayServerInfo> {
        let guard = self.gateway_transport.lock().await;
        guard.as_ref().map(GatewayTransportServer::info)
    }

    pub async fn start_gateway_transport(self: &Arc<Self>) -> MoatResult<GatewayServerInfo> {
        let mut guard = self.gateway_transport.lock().await;
        if let Some(server) = guard.as_ref() {
            return Ok(server.info());
        }

        let system = Arc::clone(self);
        let gateway_server = Arc::clone(&self.gateway_server);
        let handler: GatewayRpcHandler = Arc::new(move |request_id, request| {
            let system = Arc::clone(&system);
            let gateway_server = Arc::clone(&gateway_server);
            Box::pin(async move {
                let (status_tx, mut status_rx) = tokio::sync::mpsc::channel::<String>(64);
                let request_id_for_status = request_id.clone();
                let status_forwarder = tokio::spawn(async move {
                    while let Some(message) = status_rx.recv().await {
                        gateway_server.emit_request_status(&request_id_for_status, message);
                    }
                });

                let response = system.handle_gateway_rpc(request, Some(status_tx)).await;
                let _ = status_forwarder.await;
                response
            })
        });
        let server =
            GatewayTransportServer::start(handler, Arc::clone(&self.gateway_server)).await?;
        let info = server.info();
        let mut control_events = self.gateway.subscribe();
        let gateway_server = Arc::clone(&self.gateway_server);
        tokio::spawn(async move {
            loop {
                match control_events.recv().await {
                    Ok(event) => gateway_server.emit_control_plane_event(event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        *guard = Some(server);
        Ok(info)
    }

    fn build_session_summary_prompt(
        existing_summary: Option<&str>,
        messages: &[GatewayMessageRecord],
    ) -> String {
        let foundation = existing_summary
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("No prior summary.");
        let transcript = messages
            .iter()
            .map(|message| {
                format!(
                    "{}: {}",
                    match message.role {
                        SessionMessageRole::User => "USER",
                        SessionMessageRole::Agent => "AGENT",
                        SessionMessageRole::System => "SYSTEM",
                        SessionMessageRole::Tool => "TOOL",
                    },
                    message.content.trim()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "[HISTORICAL CONTEXT - DO NOT REPEAT]\n{}\n\nRecent Turns:\n{}",
            foundation, transcript
        )
    }

    fn fallback_session_summary(
        existing_summary: Option<&str>,
        messages: &[GatewayMessageRecord],
    ) -> String {
        let mut lines = Vec::new();
        lines.push("[HISTORICAL CONTEXT - DO NOT REPEAT]".to_string());
        if let Some(summary) = existing_summary
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            lines.push(format!("Prior Context: {}", Self::clip_text(summary, 600)));
        }
        lines.extend(messages.iter().rev().take(8).rev().map(|message| {
            format!(
                "{}: {}",
                match message.role {
                    SessionMessageRole::User => "User",
                    SessionMessageRole::Agent => "Agent",
                    SessionMessageRole::System => "System",
                    SessionMessageRole::Tool => "Tool",
                },
                Self::clip_text(message.content.trim(), 280)
            )
        }));
        lines.join("\n")
    }

    async fn maybe_compact_session(
        &self,
        session_id: Uuid,
        status_tx: Option<Sender<String>>,
    ) -> MoatResult<()> {
        let Some((existing_summary, hot_messages)) = self.gateway.compaction_input(session_id)
        else {
            return Ok(());
        };

        if let Some(tx) = &status_tx {
            let _ = tx
                .send("Memory: Compacting rolling session context...".into())
                .await;
        }

        let prompt = Self::build_session_summary_prompt(existing_summary.as_deref(), &hot_messages);
        let provider = self
            .provider_factory
            .provider_for(&self.provider_factory.get_default_provider())
            .unwrap_or_else(|_| self.orchestrator.planner.provider.clone());
        let summary_result = tokio::time::timeout(
            SESSION_COMPACTION_TIMEOUT,
            provider.complete(
                "meta/llama-3.1-70b-instruct",
                "Summarize the session. You MUST preserve exact file paths, URLs, IP addresses, identifiers, user preferences, unresolved tasks, approvals, and explicit constraints. Discard conversational filler and repetition. Return plain text only.",
                &prompt,
                500,
                0.0,
            ),
        )
        .await;

        let summary = match summary_result {
            Ok(Ok((text, _))) => text.trim().to_string(),
            _ => String::new(),
        };

        let summary = if summary.is_empty() {
            Self::fallback_session_summary(existing_summary.as_deref(), &hot_messages)
        } else {
            summary
        };

        {
            let db = self.db.lock().await;
            self.gateway
                .apply_compaction(&db, session_id, &summary, hot_messages.len())?;
        }

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let memory_text = format!("[SESSION SUMMARY]\n{summary}");
        if let Err(err) = self
            .remember_knowledge(
                KnowledgeKind::SessionSummary,
                "session_compaction",
                Some("Compacted session summary"),
                &memory_text,
                timestamp,
            )
            .await
        {
            eprintln!("[Memory] failed to persist session summary: {err}");
        }
        Ok(())
    }

    async fn record_session_response(
        &self,
        session_id: Uuid,
        content: &str,
        status_tx: Option<Sender<String>>,
    ) -> MoatResult<()> {
        self.append_session_message(session_id, SessionMessageRole::Agent, content)
            .await?;
        let _ = self.maybe_compact_session(session_id, status_tx).await;
        Ok(())
    }

    async fn persist_result_turn(
        &self,
        _key_prefix: &str,
        user_query: &str,
        results: &[SubTaskResult],
        session_id: Uuid,
        status_tx: Option<Sender<String>>,
    ) -> MoatResult<()> {
        let turn = Self::build_action_memory(user_query, results, self.memory_max_chars);
        self.record_session_response(session_id, &turn, status_tx)
            .await?;
        Ok(())
    }

    async fn execute_authoritative_session_agent(
        &self,
        user_query: &str,
        root_node_id: NodeId,
        status_tx: Option<Sender<String>>,
        preferred_specialization: Option<&str>,
        session_context: Option<&str>,
        gateway_hits: &[GatewaySearchHit],
        extra_context: Option<&str>,
    ) -> MoatResult<Option<Vec<SubTaskResult>>> {
        let gateway = self.swarm_gateway.read().await.clone();
        let Some(gateway) = gateway else {
            return Ok(None);
        };

        let assistant_spec = match gateway
            .workspace
            .agents
            .iter()
            .find(|spec| spec.name.eq_ignore_ascii_case("assistant"))
            .cloned()
        {
            Some(spec) => spec,
            None => return Ok(None),
        };

        if let Some(tx) = &status_tx {
            let _ = tx
                .send("Session Agent: Running authoritative assistant loop...".into())
                .await;
        }

        let memory_context_xml = self.load_memory_context_block().await;
        let active_memory = run_active_memory_prelude(
            user_query,
            session_context,
            gateway_hits,
            Arc::clone(&self.db),
            status_tx.clone(),
        )
        .await;
        let mut precomputed_context = active_memory.context_block;
        if let Some(specialization) = preferred_specialization
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            precomputed_context.push_str("\n<routing_hint>\n");
            precomputed_context.push_str(&format!(
                "If specialist work is required, prefer spawning the '{}' worker. If you can answer directly, do so without delegation.",
                specialization
            ));
            precomputed_context.push_str("\n</routing_hint>");
        }
        if let Some(extra_context) = extra_context
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            precomputed_context.push_str("\n<execution_context>\n");
            precomputed_context.push_str(extra_context);
            precomputed_context.push_str("\n</execution_context>");
        }

        let agent = gateway.agent(
            &assistant_spec.name,
            self.orchestrator.worker.consent_bridge.clone(),
        )?;
        let prompt =
            { agent.build_prompt(user_query, &memory_context_xml, Some(&precomputed_context))? };
        let planner = gateway.planner_for_agent(&assistant_spec.name)?;
        let delegated = self
            .orchestrator
            .with_planner(planner)
            .with_agent_spec(agent.spec.clone());
        let results = delegated
            .run_react(user_query, &prompt, root_node_id, status_tx)
            .await?;
        Ok(Some(results))
    }

    fn session_lane(&self, session_id: Uuid) -> Arc<AsyncMutex<()>> {
        self.session_lanes
            .entry(session_id)
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .value()
            .clone()
    }

    pub async fn process(
        &self,
        user_query: &str,
        root_node_id: NodeId,
        status_tx: Option<Sender<String>>,
    ) -> MoatResult<Vec<SubTaskResult>> {
        self.process_with_source(
            user_query,
            root_node_id,
            status_tx,
            SessionSource::DesktopUi,
            None,
        )
        .await
    }

    pub async fn process_with_source(
        &self,
        user_query: &str,
        root_node_id: NodeId,
        status_tx: Option<Sender<String>>,
        source: SessionSource,
        channel: Option<&str>,
    ) -> MoatResult<Vec<SubTaskResult>> {
        let session_id = self.ensure_source_session(source, channel).await?;
        let lane = self.session_lane(session_id);
        let _lane_guard = lane.lock().await;
        self.append_session_message(session_id, SessionMessageRole::User, user_query)
            .await?;
        let result = self
            .process_inner(
                user_query,
                root_node_id,
                status_tx.clone(),
                session_id,
                source,
            )
            .await;
        if let Err(err) = &result {
            let _ = self
                .append_session_message(
                    session_id,
                    SessionMessageRole::System,
                    &format!("[CRITICAL ERROR]\n{}", err),
                )
                .await;
        }
        result
    }

    async fn worker_descriptors(&self) -> MoatResult<Vec<WorkerDescriptor>> {
        let gateway = self.swarm_gateway.read().await.clone();
        if let Some(gateway) = gateway {
            let workers = gateway
                .workspace
                .agents
                .iter()
                .filter(|spec| {
                    !spec.name.eq_ignore_ascii_case("assistant")
                        && !spec.name.eq_ignore_ascii_case("router")
                        && spec.allowed_tools.iter().any(|tool| {
                            !matches!(
                                tool.as_str(),
                                "swarm:delegate"
                                    | "system:spawn_worker"
                                    | "done"
                                    | "user:ask_choice"
                            )
                        })
                })
                .map(|spec| {
                    let assessment = self.trust_store.assess_builtin_manifest(AgentManifest {
                        schema_version: 1,
                        id: spec.name.clone(),
                        description: spec.role.clone(),
                        requested_capabilities: spec.allowed_tools.clone(),
                        min_kernel_version: env!("CARGO_PKG_VERSION").into(),
                    })?;
                    Ok(WorkerDescriptor {
                        id: assessment.manifest.id,
                        description: assessment.manifest.description,
                        effective_capabilities: assessment.effective_capabilities,
                    })
                })
                .collect::<MoatResult<Vec<_>>>()?;
            if !workers.is_empty() {
                return Ok(workers);
            }
        }

        Ok(vec![WorkerDescriptor {
            id: "primary".into(),
            description: "Fallback primary worker for action tasks.".into(),
            effective_capabilities: vec!["tool:read_only".into()],
        }])
    }

    fn classify_session_fast_path(user_query: &str, source: SessionSource) -> SessionFastPath {
        let query = user_query.trim().to_ascii_lowercase();
        if query.is_empty() {
            return SessionFastPath::DirectChat;
        }

        let os_action_prefixes = [
            "open ",
            "launch ",
            "start ",
            "close ",
            "quit ",
            "go to ",
            "search for ",
            "look up ",
            "google ",
            "mute",
            "unmute",
            "set volume",
            "take screenshot",
            "screenshot",
            "switch to ",
            "focus ",
            "show ",
            "hide ",
        ];

        if os_action_prefixes
            .iter()
            .any(|prefix| query.starts_with(prefix))
        {
            return SessionFastPath::Specialist("core_os");
        }

        let command_like_markers = [
            "click ",
            "select ",
            "choose ",
            "type ",
            "fill ",
            "scroll ",
            "drag ",
            "drop ",
            "run ",
            "execute ",
            "create ",
            "write ",
            "edit ",
            "fix ",
            "implement ",
            "debug ",
            "analyze ",
            "investigate ",
            "read ",
            "list ",
            "find ",
            "set ",
            "start ",
            "stop ",
            "pause ",
            "resume ",
            "calculate ",
            "remind ",
            "schedule ",
        ];
        if command_like_markers
            .iter()
            .any(|marker| query.starts_with(marker))
        {
            return SessionFastPath::Assistant;
        }

        let greeting_or_followup = [
            "hi",
            "hello",
            "hey",
            "thanks",
            "thank you",
            "what happened",
            "who are you",
            "what can you do",
            "help",
            "okay",
            "ok",
            "good morning",
            "good evening",
        ]
        .iter()
        .any(|marker| query.starts_with(marker));

        let is_question = [
            "who ", "what ", "where ", "when ", "why ", "how ", "can ", "could ", "would ", "do ",
            "does ", "is ", "are ",
        ]
        .iter()
        .any(|q| query.starts_with(q));

        if greeting_or_followup || (is_question && !matches!(source, SessionSource::CommandBar)) {
            return SessionFastPath::DirectChat;
        }

        SessionFastPath::Assistant
    }

    fn authoritative_specialization_hint(
        user_query: &str,
        source: SessionSource,
    ) -> Option<&'static str> {
        match Self::classify_session_fast_path(user_query, source) {
            SessionFastPath::Specialist(worker_name) => Some(worker_name),
            SessionFastPath::DirectChat | SessionFastPath::Assistant => None,
        }
    }

    async fn process_inner(
        &self,
        user_query: &str,
        root_node_id: NodeId,
        status_tx: Option<Sender<String>>,
        session_id: Uuid,
        source: SessionSource,
    ) -> MoatResult<Vec<SubTaskResult>> {
        let session_context = self.gateway.session_prompt_context(session_id);
        let specialization_hint = Self::authoritative_specialization_hint(user_query, source);

        if let Some(tx) = &status_tx {
            let _ = tx
                .send("Session Agent: Executing authoritative Hermes-style loop...".into())
                .await;
        }
        let gateway_hits = self.search_gateway_messages(user_query, 5).await?;

        if let Some(results) = self
            .execute_authoritative_session_agent(
                user_query,
                root_node_id,
                status_tx.clone(),
                specialization_hint,
                session_context.as_deref(),
                &gateway_hits,
                None,
            )
            .await?
        {
            self.persist_result_turn(
                "authoritative_session_loop",
                user_query,
                &results,
                session_id,
                status_tx.clone(),
            )
            .await?;
            Ok(results)
        } else {
            Err(MoatError::Orchestrator("No authoritative session agent loop found in the workspace configuration. Split-brain fallback disabled.".into()))
        }
    }

    fn build_action_memory(objective: &str, results: &[SubTaskResult], max_chars: usize) -> String {
        let mut lines = vec![format!("Objective: {objective}")];
        for result in results {
            let observation = match &result.status {
                TaskStatus::Success { stdout } => format!(
                    "[SUCCESS] {}",
                    Self::clip_text(stdout.trim(), max_chars.min(280))
                ),
                TaskStatus::SoftDenial { reason } => format!(
                    "[SOFT DENIAL] {}",
                    Self::clip_text(reason.trim(), max_chars.min(280))
                ),
                TaskStatus::Failed { exit_code, stderr } => format!(
                    "[FAILED] (exit {:?}): {}",
                    exit_code,
                    Self::clip_text(stderr.trim(), max_chars.min(280))
                ),
                TaskStatus::Blocked { blocked_by } => format!("blocked by subtask {blocked_by}"),
                TaskStatus::Cancelled { stage } => format!(
                    "[CANCELLED] at: {}",
                    Self::clip_text(stage.trim(), max_chars.min(280))
                ),
            };
            lines.push(format!(
                "Step {} | action={} | target={} | {}",
                result.subtask_id, result.action, result.target, observation
            ));
        }
        lines.join("\n")
    }

    pub async fn get_memory_overview(&self) -> MoatResult<MemoryOverview> {
        let db = self.db.lock().await;
        build_memory_overview(&db, 48)
    }

    pub async fn delete_knowledge_memory(&self, id: &str) -> MoatResult<()> {
        let db = self.db.lock().await;
        delete_knowledge(&db, id)?;
        drop(db);

        Ok(())
    }

    fn settings_swarm_root(&self) -> PathBuf {
        self.swarm_workspace_root
            .clone()
            .unwrap_or_else(config::swarm_root)
    }

    pub fn settings_swarm_root_for_ui(&self) -> PathBuf {
        self.settings_swarm_root()
    }

    async fn reload_swarm_workspace(&self) -> MoatResult<()> {
        let (gateway, default_agent, _) = Self::build_swarm_gateway(
            Some(self.settings_swarm_root()),
            Arc::clone(&self.provider_factory),
            Arc::clone(&self.meta_brain),
            self.planner_defaults.clone(),
            self.orchestrator.injector_config.clone(),
            self.default_swarm_agent_preference.clone(),
        )
        .await;

        {
            let mut slot = self.swarm_gateway.write().await;
            *slot = gateway;
        }
        {
            let mut slot = self.default_swarm_agent.write().await;
            *slot = default_agent;
        }
        Ok(())
    }

    pub async fn get_settings(&self) -> MoatResult<SettingsState> {
        let root = Workspace::ensure_scaffold(self.settings_swarm_root()).await?;
        let user_doc = config::load_user_document(root.join("USER.md")).await?;
        let provider_profiles = config::load_provider_profiles()?;
        let mut has_any_nvidia_key = false;
        for profile in provider_profiles.nvidia_profiles {
            let has_key = self
                .provider_factory
                .has_stored_key(&format!("nvidia:{}", profile.id))?;
            has_any_nvidia_key |= has_key;
        }
        let mut has_any_deepseek_key = false;
        for profile in provider_profiles.deepseek_profiles {
            let has_key = self
                .provider_factory
                .has_stored_key(&format!("deepseek:{}", profile.id))?;
            has_any_deepseek_key |= has_key;
        }
        let has_frontmatter_nvidia_key = user_doc
            .frontmatter
            .nvidia_api_key
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty());
        let has_env_nvidia_key = std::env::var("NVIDIA_API_KEY")
            .ok()
            .map(|value| value.trim().to_string())
            .is_some_and(|value| !value.is_empty());
        let has_frontmatter_deepseek_key = user_doc
            .frontmatter
            .deepseek_api_key
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty());
        let has_env_deepseek_key = std::env::var("DEEPSEEK_API_KEY")
            .ok()
            .map(|value| value.trim().to_string())
            .is_some_and(|value| !value.is_empty());

        Ok(SettingsState {
            has_nvidia: has_any_nvidia_key || has_frontmatter_nvidia_key || has_env_nvidia_key,
            has_deepseek: has_any_deepseek_key || has_frontmatter_deepseek_key || has_env_deepseek_key,
            chat_model: user_doc.frontmatter.chat_model,
            planner_model: user_doc.frontmatter.planner_model,
            worker_model: user_doc.frontmatter.worker_model,
        })
    }

    pub async fn save_settings(&self, update: SettingsUpdate) -> MoatResult<()> {
        let root = Workspace::ensure_scaffold(self.settings_swarm_root()).await?;
        let existing_user_doc = config::load_user_document(root.join("USER.md")).await?;
        let normalized_chat_model = update
            .chat_model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let normalized_planner_model = update
            .planner_model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let normalized_worker_model = update
            .worker_model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let normalized_nvidia_key = if let Some(key) = update.nvidia_api_key.as_deref() {
            let trimmed = key.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        } else {
            existing_user_doc.frontmatter.nvidia_api_key.clone()
        };
        let normalized_deepseek_key = if let Some(key) = update.deepseek_api_key.as_deref() {
            let trimmed = key.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        } else {
            existing_user_doc.frontmatter.deepseek_api_key.clone()
        };
        config::save_user_frontmatter(
            root.join("USER.md"),
            UserFrontmatter {
                name: existing_user_doc.frontmatter.name,
                core_preferences: existing_user_doc.frontmatter.core_preferences,
                chat_model: normalized_chat_model.clone(),
                planner_model: normalized_planner_model.clone(),
                worker_model: normalized_worker_model.clone(),
                nvidia_api_key: normalized_nvidia_key.clone(),
                deepseek_api_key: normalized_deepseek_key.clone(),
            },
        )
        .await?;

        match update.nvidia_api_key.as_deref().map(str::trim) {
            Some("") => self.provider_factory.remove_provider_key("nvidia")?,
            Some(trimmed) if !trimmed.is_empty() => {
                self.provider_factory.set_provider_key("nvidia", trimmed)?;
            }
            _ => {}
        }

        match update.deepseek_api_key.as_deref().map(str::trim) {
            Some("") => self.provider_factory.remove_provider_key("deepseek")?,
            Some(trimmed) if !trimmed.is_empty() => {
                self.provider_factory.set_provider_key("deepseek", trimmed)?;
                self.provider_factory.set_default_provider("deepseek");
            }
            _ => {}
        }

        if let Some(ref provider_name) = update.provider {
            let normalized = provider_name.trim().to_ascii_lowercase();
            if normalized == "nvidia" || normalized == "deepseek" {
                self.provider_factory.set_default_provider(&normalized);
            }
        }

        self.provider_factory.invalidate_cache();
        self.orchestrator.planner.set_model(
            normalized_planner_model
                .clone()
                .unwrap_or_else(|| FACTORY_DEFAULT_PLANNER_MODEL.to_string()),
        );
        self.reload_swarm_workspace().await?;
        Ok(())
    }

    pub async fn hard_reset_memory(&self) -> MoatResult<()> {
        {
            let db = self.db.lock().await;
            clear_knowledge(&db)?;
            gateway::reset_tables(&db)?;
        }

        self.gateway.reset_runtime_state();
        Ok(())
    }

    pub async fn log_crash(&self, query: &str, error: &str) -> MoatResult<()> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let memory_text = format!(
            "[KERNEL ERROR] query=\"{}\" error=\"{}\"",
            query
                .chars()
                .take(256)
                .collect::<String>()
                .replace('"', "'"),
            error.replace('"', "'")
        );

        self.remember_knowledge(
            KnowledgeKind::CrashNote,
            "kernel",
            Some("Kernel pipeline error"),
            &memory_text,
            timestamp.as_secs(),
        )
        .await
        .map(|_| ())
    }
}
