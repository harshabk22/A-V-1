use crate::config;
use crate::consent::ConsentBridge;
use crate::external_api::ProviderFactory;
use crate::injector::{build_injected_prompt_with_swarm, InjectorConfig};
use crate::memory::{build_prompt_memory_block, PromptMemoryOptions};
use crate::meta_brain::{MetaBrain, NodeKind, PermissionFlags};
use crate::orchestrator::{
    ActionIntent, Orchestrator, PlannerAgent, PlannerConfig, SubTaskResult, WorkerAgent,
};
#[derive(Debug, Clone)]
pub struct TemplateDescriptor {
    pub id: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct RouterDecision {
    pub target_template: Option<String>,
}

use crate::{MoatError, MoatResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex as AsyncMutex, RwLock as AsyncRwLock};
use url::Url;
use uuid::Uuid;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserProfile {
    pub name: String,
    pub core_preferences: String,
    pub chat_model: Option<String>,
    pub planner_model: Option<String>,
    pub worker_model: Option<String>,
    pub summary: String,
    pub facts: Vec<String>,
    pub body_markdown: String,
    pub raw_markdown: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSpec {
    pub name: String,
    pub role: String,
    pub api_provider: String,
    pub model_id: String,
    pub allowed_tools: Vec<String>,
    pub instructions: String,
    pub raw_markdown: String,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub user: UserProfile,
    pub agents: Vec<AgentSpec>,
}

impl Workspace {
    pub fn default_root() -> PathBuf {
        config::swarm_root()
    }

    pub async fn ensure_scaffold(root: impl AsRef<Path>) -> MoatResult<PathBuf> {
        let root = root.as_ref().to_path_buf();
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|err| MoatError::Graph(format!("failed to create swarm root: {err}")))?;

        let user_path = root.join("USER.md");
        if !tokio::fs::try_exists(&user_path)
            .await
            .map_err(|err| MoatError::Graph(format!("failed to inspect USER.md: {err}")))?
        {
            tokio::fs::write(&user_path, default_user_template())
                .await
                .map_err(|err| MoatError::Graph(format!("failed to write USER.md: {err}")))?;
        }

        let soul_path = root.join("SOUL.md");
        if !tokio::fs::try_exists(&soul_path)
            .await
            .map_err(|err| MoatError::Graph(format!("failed to inspect SOUL.md: {err}")))?
        {
            tokio::fs::write(&soul_path, default_soul_template())
                .await
                .map_err(|err| MoatError::Graph(format!("failed to write SOUL.md: {err}")))?;
        }

        Ok(root)
    }

    pub async fn load(root: impl AsRef<Path>) -> MoatResult<Self> {
        let root = root.as_ref().to_path_buf();
        let user_md = tokio::fs::read_to_string(root.join("USER.md"))
            .await
            .map_err(|err| MoatError::Graph(format!("failed to read USER.md: {err}")))?;
        let soul_md = tokio::fs::read_to_string(root.join("SOUL.md"))
            .await
            .map_err(|err| MoatError::Graph(format!("failed to read SOUL.md: {err}")))?;

        Ok(Self {
            root,
            user: parse_user_profile(&user_md),
            agents: parse_agent_specs(&soul_md),
        })
    }
}

pub struct SwarmGateway {
    pub workspace: Arc<Workspace>,
    /// NOTE: The Swarm subsystem uses `Arc<AsyncRwLock<MetaBrain>>` (tokio async RwLock),
    /// separate from the agent path which uses `Arc<Mutex<MetaBrain>>` (std sync Mutex).
    /// These are intentionally independent MetaBrain instances.
    pub brain: Arc<AsyncRwLock<MetaBrain>>,
    pub sessions: Arc<AsyncMutex<HashMap<Uuid, ReactSession>>>,
    pub provider_factory: Arc<dyn ProviderFactory>,
    pub planner_defaults: PlannerConfig,
    pub injector_config: InjectorConfig,
}

impl SwarmGateway {
    pub fn new(
        workspace: Workspace,
        brain: Arc<AsyncRwLock<MetaBrain>>,
        provider_factory: Arc<dyn ProviderFactory>,
        planner_defaults: PlannerConfig,
        injector_config: InjectorConfig,
    ) -> Self {
        Self {
            workspace: Arc::new(workspace),
            brain,
            sessions: Arc::new(AsyncMutex::new(HashMap::new())),
            provider_factory,
            planner_defaults,
            injector_config,
        }
    }

    pub fn default_agent_name(&self) -> Option<String> {
        self.workspace.agents.first().map(|spec| spec.name.clone())
    }

    pub fn resolve_specialization(&self, specialization: &str) -> Option<String> {
        let needle = specialization.trim();
        if needle.is_empty() {
            return self.default_agent_name();
        }

        if let Some(spec) = self
            .workspace
            .agents
            .iter()
            .find(|spec| spec.name.eq_ignore_ascii_case(needle))
        {
            return Some(spec.name.clone());
        }

        let lower = needle.to_ascii_lowercase();
        if let Some(spec) = self.workspace.agents.iter().find(|spec| {
            spec.role.to_ascii_lowercase().contains(&lower)
                || spec.name.to_ascii_lowercase().contains(&lower)
        }) {
            return Some(spec.name.clone());
        }

        self.default_agent_name()
    }

    pub fn templates(&self) -> Vec<TemplateDescriptor> {
        self.workspace
            .agents
            .iter()
            .filter(|spec| {
                !spec.name.eq_ignore_ascii_case("assistant")
                    && !spec.name.eq_ignore_ascii_case("router")
            })
            .map(|spec| TemplateDescriptor {
                id: spec.name.clone(),
                description: format!("{} {}", spec.role.trim(), spec.instructions.trim())
                    .trim()
                    .to_string(),
            })
            .collect()
    }

    pub fn resolve_template<'a>(&'a self, decision: &RouterDecision) -> MoatResult<&'a AgentSpec> {
        let id = decision
            .target_template
            .as_deref()
            .ok_or_else(|| MoatError::Graph("No target_template".to_string()))?;
        self.workspace
            .agents
            .iter()
            .find(|spec| spec.name.eq_ignore_ascii_case(id))
            .ok_or_else(|| MoatError::Graph(format!("Template '{}' not found", id)))
    }

    pub async fn open_session(&self, agent_name: &str) -> MoatResult<ReactSession> {
        let spec = self
            .workspace
            .agents
            .iter()
            .find(|spec| spec.name.eq_ignore_ascii_case(agent_name))
            .cloned()
            .ok_or_else(|| MoatError::Graph(format!("unknown swarm agent '{agent_name}'")))?;
        let session = ReactSession {
            session_id: Uuid::new_v4(),
            agent_name: spec.name.clone(),
            user_profile: self.workspace.user.clone(),
            agent_spec: spec,
        };
        self.sessions
            .lock()
            .await
            .insert(session.session_id, session.clone());
        Ok(session)
    }

    pub async fn close_session(&self, session_id: Uuid) {
        self.sessions.lock().await.remove(&session_id);
    }

    pub async fn spawn_for_decision(
        &self,
        decision: &RouterDecision,
        consent_bridge: Option<Arc<ConsentBridge>>,
    ) -> MoatResult<(ReactSession, SwarmAgent)> {
        let template = self.resolve_template(decision)?;
        let session = self.open_session(&template.name).await?;
        let agent = self.agent(&session.agent_name, consent_bridge)?;
        Ok((session, agent))
    }

    pub fn agent(
        &self,
        agent_name: &str,
        consent_bridge: Option<Arc<ConsentBridge>>,
    ) -> MoatResult<SwarmAgent> {
        let spec = self
            .workspace
            .agents
            .iter()
            .find(|spec| spec.name.eq_ignore_ascii_case(agent_name))
            .cloned()
            .ok_or_else(|| MoatError::Graph(format!("unknown swarm agent '{agent_name}'")))?;
        Ok(SwarmAgent {
            workspace: Arc::clone(&self.workspace),
            spec,
            brain: Arc::clone(&self.brain),
            consent_bridge,
        })
    }

    pub fn planner_for_agent(&self, agent_name: &str) -> MoatResult<PlannerAgent> {
        let spec = self
            .workspace
            .agents
            .iter()
            .find(|spec| spec.name.eq_ignore_ascii_case(agent_name))
            .ok_or_else(|| MoatError::Graph(format!("unknown swarm agent '{agent_name}'")))?;
        let model_override = if spec.name.eq_ignore_ascii_case("assistant")
            || spec.name.eq_ignore_ascii_case("router")
        {
            self.workspace
                .user
                .planner_model
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        } else {
            self.workspace
                .user
                .worker_model
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        };
        let provider_name = spec.api_provider.clone();
        let model_id = model_override.unwrap_or_else(|| spec.model_id.clone());
        let provider = self.provider_factory.provider_for(&provider_name)?;
        let config = PlannerConfig {
            model: model_id,
            max_subtasks: self.planner_defaults.max_subtasks,
            temperature: self.planner_defaults.temperature,
            max_tokens: self.planner_defaults.max_tokens,
        };
        Ok(PlannerAgent::new(config, provider))
    }

    pub async fn run_delegate(
        &self,
        template: &Orchestrator,
        worker: Arc<WorkerAgent>,
        target_agent_id: &str,
        task: &str,
        precomputed_context_block: &str,
        root_node_id: u64,
        status_tx: Option<tokio::sync::mpsc::Sender<String>>,
        depth: usize,
        delegate_count: Arc<std::sync::atomic::AtomicUsize>,
    ) -> MoatResult<Vec<SubTaskResult>> {
        let session = self.open_session(target_agent_id).await?;
        let agent = self.agent(&session.agent_name, worker.consent_bridge.clone())?;
        let memory_context_xml = {
            let db = template.db.lock().await;
            build_prompt_memory_block(
                &db,
                &PromptMemoryOptions {
                    max_entries: 14,
                    max_chars_per_entry: self.injector_config.max_memory_chars.min(240),
                },
            )
            .unwrap_or_else(|_| {
                "<knowledge_memory>\nNo durable knowledge recorded yet.\n</knowledge_memory>"
                    .to_string()
            })
        };
        let prompt =
            { agent.build_prompt(task, &memory_context_xml, Some(precomputed_context_block))? };
        let planner = self.planner_for_agent(&session.agent_name)?;
        let delegated = template
            .with_planner(planner)
            .with_agent_spec(agent.spec.clone());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(45),
            delegated.run_react_scoped(
                task,
                &prompt,
                root_node_id,
                status_tx,
                depth + 1,
                delegate_count,
            ),
        )
        .await;
        self.close_session(session.session_id).await;
        match result {
            Ok(inner) => inner,
            Err(_) => Err(MoatError::Orchestrator("delegate session timed out".into())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReactSession {
    pub session_id: Uuid,
    pub agent_name: String,
    pub user_profile: UserProfile,
    pub agent_spec: AgentSpec,
}

pub struct SwarmAgent {
    pub workspace: Arc<Workspace>,
    pub spec: AgentSpec,
    pub brain: Arc<AsyncRwLock<MetaBrain>>,
    pub consent_bridge: Option<Arc<ConsentBridge>>,
}

impl SwarmAgent {
    pub fn build_prompt(
        &self,
        user_query: &str,
        memory_context_xml: &str,
        precomputed_context: Option<&str>,
    ) -> MoatResult<String> {
        build_injected_prompt_with_swarm(
            user_query,
            memory_context_xml,
            precomputed_context,
            Some(&self.workspace.user),
            Some(&self.spec),
            None,
        )
    }

    pub async fn request_and_apply_consent(&self, intent: &ActionIntent) -> MoatResult<bool> {
        let Some(bridge) = &self.consent_bridge else {
            return Ok(false);
        };

        let payload_text = extract_payload_text(intent.payload.as_ref())
            .unwrap_or_else(|| intent.target.trim().to_string());
        let approved = bridge.request_consent(&intent.action, &payload_text).await;
        if !approved {
            return Ok(false);
        }

        let permissions = permission_flags_for_intent(intent);
        if permissions.is_empty() {
            return Ok(true);
        }

        let classification = classify_payload_target(&payload_text)
            .or_else(|| classify_payload_target(intent.target.trim()));
        let Some((kind, resource)) = classification else {
            return Ok(true);
        };

        {
            let mut brain = self.brain.write().await;
            match kind {
                NodeKind::Path => {
                    brain.grant_path_permission(&resource, permissions)?;
                }
                NodeKind::Domain => {
                    brain.grant_domain_permission(&resource, permissions)?;
                }
                _ => {}
            }
        }

        MetaBrain::flush_arc(Arc::clone(&self.brain)).await?;
        Ok(true)
    }
}

fn extract_payload_text(payload: Option<&Value>) -> Option<String> {
    match payload {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(Value::Object(map)) => map
            .get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        _ => None,
    }
}

fn permission_flags_for_intent(intent: &ActionIntent) -> PermissionFlags {
    match intent.action.as_str() {
        "fs:read" => PermissionFlags::FILE_READ,
        "fs:write" => PermissionFlags::FILE_WRITE,
        "net:curl" => PermissionFlags::NET_OUTBOUND,
        "os:open_url" => PermissionFlags::URL_OPEN,
        "app:launch" => PermissionFlags::APP_LAUNCH,
        "process:spawn" | "os:applescript" | "os:inject_text" => {
            PermissionFlags::TOOL_EXEC | PermissionFlags::EXEC
        }
        _ => PermissionFlags::NONE,
    }
}

fn classify_payload_target(raw: &str) -> Option<(NodeKind, String)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.starts_with('/') {
        return Some((NodeKind::Path, raw.to_string()));
    }
    if let Ok(url) = Url::parse(raw) {
        if let Some(host) = url.host_str() {
            return Some((NodeKind::Domain, host.to_ascii_lowercase()));
        }
    }
    if looks_like_domain(raw) {
        return Some((NodeKind::Domain, raw.trim_matches('.').to_ascii_lowercase()));
    }
    None
}

fn looks_like_domain(raw: &str) -> bool {
    let candidate = raw.trim().trim_matches('.');
    candidate.contains('.')
        && candidate
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-'))
}

fn parse_user_profile(markdown: &str) -> UserProfile {
    let doc =
        config::parse_user_document(markdown).unwrap_or_else(|_| config::default_user_document());
    let has_frontmatter = markdown.trim_start().starts_with("---");
    let has_explicit_name = !doc.frontmatter.name.trim().is_empty()
        && (has_frontmatter || doc.frontmatter.name != "User");
    let summary = if has_explicit_name {
        doc.frontmatter.name.trim().to_string()
    } else {
        doc.body
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default()
            .to_string()
    };
    let mut facts = Vec::new();
    if has_explicit_name {
        facts.push(format!("Name: {}", doc.frontmatter.name.trim()));
    }
    if !doc.frontmatter.core_preferences.trim().is_empty() {
        facts.push(format!(
            "Core Preferences: {}",
            doc.frontmatter.core_preferences.trim()
        ));
    }
    UserProfile {
        name: doc.frontmatter.name,
        core_preferences: doc.frontmatter.core_preferences,
        chat_model: doc
            .frontmatter
            .chat_model
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        planner_model: doc
            .frontmatter
            .planner_model
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        worker_model: doc
            .frontmatter
            .worker_model
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        summary,
        facts,
        body_markdown: doc.body,
        raw_markdown: doc.raw_markdown,
    }
}

fn parse_agent_specs(markdown: &str) -> Vec<AgentSpec> {
    let mut specs = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_body = Vec::new();

    let flush_section = |specs: &mut Vec<AgentSpec>,
                         current_name: &mut Option<String>,
                         current_body: &mut Vec<String>| {
        let Some(name) = current_name.take() else {
            current_body.clear();
            return;
        };
        let raw_markdown = current_body.join("\n").trim().to_string();
        let role = current_body
            .iter()
            .map(|line| line.trim())
            .find(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('-'))
            .unwrap_or_default()
            .to_string();
        let api_provider = current_body
            .iter()
            .map(|line| line.trim())
            .find_map(|line| {
                line.strip_prefix("api_provider:")
                    .or_else(|| line.strip_prefix("provider:"))
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| "nvidia".to_string());
        let model_id = current_body
            .iter()
            .map(|line| line.trim())
            .find_map(|line| {
                line.strip_prefix("model_id:")
                    .or_else(|| line.strip_prefix("model:"))
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| "meta/llama-3.1-70b-instruct".to_string());
        let allowed_tools = current_body
            .iter()
            .map(|line| line.trim())
            .find_map(|line| line.strip_prefix("allowed_tools:").map(str::trim))
            .map(parse_allowed_tools)
            .unwrap_or_default();
        let mut spec = AgentSpec {
            name,
            role: role.clone(),
            api_provider,
            model_id,
            allowed_tools,
            instructions: raw_markdown.clone(),
            raw_markdown,
        };
        ensure_assistant_capabilities(&mut spec);
        ensure_os_inject_capabilities(&mut spec);
        ensure_coder_capabilities(&mut spec);
        ensure_core_os_capabilities(&mut spec);
        specs.push(spec);
        current_body.clear();
    };

    for line in markdown.lines() {
        if let Some(name) = line.trim().strip_prefix("## ") {
            flush_section(&mut specs, &mut current_name, &mut current_body);
            current_name = Some(name.trim().to_string());
        } else {
            current_body.push(line.to_string());
        }
    }
    flush_section(&mut specs, &mut current_name, &mut current_body);

    if specs.is_empty() && !markdown.trim().is_empty() {
        specs.push(AgentSpec {
            name: "primary".into(),
            role: markdown
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("primary")
                .to_string(),
            api_provider: "nvidia".into(),
            model_id: "meta/llama-3.1-70b-instruct".into(),
            allowed_tools: Vec::new(),
            instructions: markdown.trim().to_string(),
            raw_markdown: markdown.trim().to_string(),
        });
    }

    if !specs
        .iter()
        .any(|spec| spec.name.eq_ignore_ascii_case("assistant"))
    {
        specs.push(default_assistant_spec());
    }

    specs
}

fn parse_allowed_tools(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.starts_with('[') {
        if let Ok(values) = serde_json::from_str::<Vec<String>>(trimmed) {
            return values
                .into_iter()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .collect();
        }
    }
    trimmed
        .split(',')
        .map(|value| value.trim().trim_matches('"').to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn ensure_coder_capabilities(spec: &mut AgentSpec) {
    if !spec.name.eq_ignore_ascii_case("coder") {
        return;
    }

    for tool in [
        "fs:read",
        "fs:write",
        "process:spawn",
        "propose_system_skill",
        "meta:query_brain",
        "memory:search",
        "session:search",
        "memory:learn",
        "done",
        "user:ask_choice",
    ] {
        if !spec.allowed_tools.iter().any(|existing| existing == tool) {
            spec.allowed_tools.push(tool.to_string());
        }
    }
}

fn ensure_assistant_capabilities(spec: &mut AgentSpec) {
    if !spec.name.eq_ignore_ascii_case("assistant") {
        return;
    }

    for tool in [
        "system:spawn_worker",
        "memory:search",
        "session:search",
        "memory:learn",
        "meta:query_brain",
        "done",
        "user:ask_choice",
    ] {
        if !spec.allowed_tools.iter().any(|existing| existing == tool) {
            spec.allowed_tools.push(tool.to_string());
        }
    }
}

fn ensure_os_inject_capabilities(spec: &mut AgentSpec) {
    let has_os_control = spec
        .allowed_tools
        .iter()
        .any(|tool| tool.eq_ignore_ascii_case("os:applescript"));
    let has_inject = spec
        .allowed_tools
        .iter()
        .any(|tool| tool.eq_ignore_ascii_case("os:inject_text"));
    if has_os_control && !has_inject {
        spec.allowed_tools.push("os:inject_text".to_string());
    }
}

fn ensure_core_os_capabilities(spec: &mut AgentSpec) {
    if !spec.name.eq_ignore_ascii_case("core_os") {
        return;
    }

    for tool in ["app:launch", "os:open_url", "session:search"] {
        if !spec.allowed_tools.iter().any(|existing| existing == tool) {
            spec.allowed_tools.push(tool.to_string());
        }
    }
}

fn default_user_template() -> String {
    config::default_user_document().raw_markdown
}

fn default_soul_template() -> &'static str {
    "## assistant\nrole: Act as the authoritative session-aware assistant. Answer directly when possible, search memory and transcripts when useful, and spawn specialists only when needed.\napi_provider: nvidia\nmodel_id: meta/llama-3.1-70b-instruct\nallowed_tools: [\"system:spawn_worker\", \"memory:search\", \"session:search\", \"memory:learn\", \"meta:query_brain\", \"done\", \"user:ask_choice\", \"app:launch\", \"os:open_url\", \"fs:read\", \"fs:list\"]\n\n## router\nrole: Route tasks to the right specialist and ask for clarification when intent is ambiguous.\napi_provider: nvidia\nmodel_id: meta/llama-3.1-70b-instruct\nallowed_tools: [\"system:spawn_worker\", \"done\", \"user:ask_choice\"]\n\n## core_os\nrole: Operate on the host system, inspect permissions, and handle OS-facing tasks.\napi_provider: nvidia\nmodel_id: meta/llama-3.1-70b-instruct\nallowed_tools: [\"os:applescript\", \"os:inject_text\", \"process:spawn\", \"meta:query_brain\", \"memory:search\", \"session:search\", \"memory:learn\", \"done\", \"user:ask_choice\", \"app:launch\", \"os:open_url\"]\n\n## coder\nrole: Solve code and workspace tasks using safe tool execution.\napi_provider: nvidia\nmodel_id: meta/llama-3.1-70b-instruct\nallowed_tools: [\"fs:read\", \"fs:write\", \"process:spawn\", \"propose_system_skill\", \"meta:query_brain\", \"memory:search\", \"session:search\", \"memory:learn\", \"done\", \"user:ask_choice\"]\n"
}

fn default_assistant_spec() -> AgentSpec {
    AgentSpec {
        name: "assistant".into(),
        role: "Act as the authoritative session-aware assistant. Answer directly when possible, search memory and transcripts when useful, and spawn specialists only when needed.".into(),
        api_provider: "nvidia".into(),
        model_id: "meta/llama-3.1-70b-instruct".into(),
        allowed_tools: vec![
            "system:spawn_worker".into(),
            "memory:search".into(),
            "session:search".into(),
            "memory:learn".into(),
            "meta:query_brain".into(),
            "done".into(),
            "user:ask_choice".into(),
            "app:launch".into(),
            "os:open_url".into(),
            "fs:read".into(),
            "fs:list".into(),
        ],
        instructions: "assistant".into(),
        raw_markdown: "## assistant".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderHealth;
    use crate::consent::{ConsentDecision, ConsentRequest};
    use crate::external_api::ProviderFactory;
    use crate::injector::InjectorConfig;
    use crate::meta_brain::{MetaBrain, PermissionFlags};
    use crate::orchestrator::{BoxFuture, LlmProvider, PlannerConfig};
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use tokio::sync::RwLock as AsyncRwLock;

    struct StubProvider;

    impl LlmProvider for StubProvider {
        fn complete<'a>(
            &'a self,
            _model: &'a str,
            _system: &'a str,
            _user: &'a str,
            _max_tokens: usize,
            _temperature: f32,
        ) -> BoxFuture<'a, MoatResult<(String, u32)>> {
            Box::pin(async { Ok(("{}".to_string(), 0)) })
        }

        fn check_health<'a>(&'a self) -> BoxFuture<'a, MoatResult<()>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct RecordingProviderFactory {
        requested_providers: Arc<Mutex<Vec<String>>>,
    }

    impl ProviderFactory for RecordingProviderFactory {
        fn provider_for(&self, api_provider: &str) -> MoatResult<Arc<dyn LlmProvider>> {
            self.requested_providers
                .lock()
                .expect("lock provider requests")
                .push(api_provider.to_string());
            Ok(Arc::new(StubProvider))
        }

        fn routed_provider(
            &self,
            _subsystem: &str,
            requested_model: &str,
        ) -> MoatResult<(Arc<dyn LlmProvider>, String)> {
            Ok((Arc::new(StubProvider), requested_model.to_string()))
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

        fn provider_health<'a>(
            &'a self,
            _api_provider: &'a str,
        ) -> BoxFuture<'a, MoatResult<ProviderHealth>> {
            Box::pin(async { Ok(ProviderHealth::Valid) })
        }
    }

    #[tokio::test]
    async fn workspace_load_reads_user_and_soul_files() {
        let root = std::env::temp_dir().join(format!("moat_swarm_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(
            root.join("USER.md"),
            "---\nname: \"Harsha\"\ncore_preferences: \"prefers concise summaries\"\n---\nHarsha is the operator.\n- prefers concise summaries\n",
        )
        .expect("write USER.md");
        fs::write(
            root.join("SOUL.md"),
            "## planner\napi_provider: nvidia\nmodel_id: meta/llama-3.1-405b-instruct\nallowed_tools: [\"system:spawn_worker\", \"done\"]\nPlans work.\n\n## executor\napi_provider: nvidia\nmodel: meta/llama-3.1-70b-instruct\nallowed_tools: [\"process:spawn\", \"done\"]\nExecutes work.\n",
        )
        .expect("write SOUL.md");

        let workspace = Workspace::load(&root).await.expect("load workspace");
        assert_eq!(workspace.user.name, "Harsha");
        assert_eq!(workspace.user.core_preferences, "prefers concise summaries");
        assert_eq!(workspace.user.summary, "Harsha");
        assert_eq!(workspace.agents.len(), 3);
        assert_eq!(workspace.agents[0].name, "planner");
        assert_eq!(workspace.agents[0].api_provider, "nvidia");
        assert_eq!(workspace.agents[0].model_id, "meta/llama-3.1-405b-instruct");
        assert_eq!(
            workspace.agents[0].allowed_tools,
            vec!["system:spawn_worker", "done"]
        );
        assert_eq!(workspace.agents[1].api_provider, "nvidia");
        assert_eq!(workspace.agents[1].model_id, "meta/llama-3.1-70b-instruct");
        assert_eq!(
            workspace.agents[1].allowed_tools,
            vec!["process:spawn", "done"]
        );
        assert_eq!(workspace.agents[2].name, "assistant");
        assert!(workspace.agents[2]
            .allowed_tools
            .contains(&"system:spawn_worker".to_string()));
        assert!(workspace.agents[2]
            .allowed_tools
            .contains(&"session:search".to_string()));
    }

    #[tokio::test]
    async fn ensure_scaffold_creates_default_workspace_files() {
        let root = std::env::temp_dir().join(format!("moat_scaffold_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);

        let scaffold_root = Workspace::ensure_scaffold(&root)
            .await
            .expect("scaffold workspace");
        let user_md = fs::read_to_string(scaffold_root.join("USER.md")).expect("read USER.md");
        let soul_md = fs::read_to_string(scaffold_root.join("SOUL.md")).expect("read SOUL.md");

        assert!(user_md.contains("---"));
        assert!(user_md.contains("name: User"));
        assert!(user_md.contains("core_preferences:"));
        assert!(soul_md.contains("## router"));
        assert!(soul_md.contains("## assistant"));
        assert!(soul_md.contains("## core_os"));
        assert!(soul_md.contains("## coder"));
        assert!(soul_md.contains("allowed_tools:"));
        assert!(soul_md.contains("api_provider: nvidia"));
        assert!(soul_md.contains("model_id: meta/llama-3.1-70b-instruct"));
    }

    #[test]
    fn planner_for_agent_uses_worker_model_override() {
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let provider_factory: Arc<dyn ProviderFactory> = Arc::new(RecordingProviderFactory {
            requested_providers: Arc::clone(&requests),
        });
        let workspace = Workspace {
            root: std::env::temp_dir(),
            user: UserProfile {
                worker_model: Some("meta/llama-3.1-70b-instruct".into()),
                ..UserProfile::default()
            },
            agents: vec![AgentSpec {
                name: "core_os".into(),
                role: "Operate".into(),
                api_provider: "nvidia".into(),
                model_id: "meta/llama-3.1-405b-instruct".into(),
                allowed_tools: vec!["process:spawn".into(), "done".into()],
                instructions: "Do the work".into(),
                raw_markdown: "Do the work".into(),
            }],
        };
        let gateway = SwarmGateway::new(
            workspace,
            Arc::new(AsyncRwLock::new(MetaBrain::new())),
            provider_factory,
            PlannerConfig {
                model: "stub".into(),
                max_subtasks: 10,
                temperature: 0.1,
                max_tokens: 256,
            },
            InjectorConfig {
                top_k_memories: 5,
                max_memory_chars: 600,
            },
        );

        let planner = gateway
            .planner_for_agent("core_os")
            .expect("planner for worker override");

        let requested = requests.lock().expect("lock recorded providers");
        assert_eq!(requested.as_slice(), ["nvidia"]);
        assert_eq!(planner.config.model, "meta/llama-3.1-70b-instruct");
    }

    #[test]
    fn templates_exclude_assistant_and_router() {
        let workspace = Workspace {
            root: PathBuf::from("/tmp/moat_templates"),
            user: UserProfile::default(),
            agents: vec![
                AgentSpec {
                    name: "assistant".into(),
                    role: "assistant".into(),
                    api_provider: "nvidia".into(),
                    model_id: "meta/llama-3.1-70b-instruct".into(),
                    allowed_tools: vec!["system:spawn_worker".into(), "done".into()],
                    instructions: String::new(),
                    raw_markdown: String::new(),
                },
                AgentSpec {
                    name: "router".into(),
                    role: "router".into(),
                    api_provider: "nvidia".into(),
                    model_id: "meta/llama-3.1-70b-instruct".into(),
                    allowed_tools: vec!["system:spawn_worker".into(), "done".into()],
                    instructions: String::new(),
                    raw_markdown: String::new(),
                },
                AgentSpec {
                    name: "coder".into(),
                    role: "codes".into(),
                    api_provider: "nvidia".into(),
                    model_id: "meta/llama-3.1-70b-instruct".into(),
                    allowed_tools: vec!["fs:read".into(), "done".into()],
                    instructions: String::new(),
                    raw_markdown: String::new(),
                },
            ],
        };
        let gateway = SwarmGateway::new(
            workspace,
            Arc::new(AsyncRwLock::new(MetaBrain::new())),
            Arc::new(RecordingProviderFactory {
                requested_providers: Arc::new(Mutex::new(Vec::new())),
            }),
            PlannerConfig {
                model: "meta/llama-3.1-405b-instruct".into(),
                max_subtasks: 15,
                temperature: 0.0,
                max_tokens: 128,
            },
            InjectorConfig {
                top_k_memories: 5,
                max_memory_chars: 600,
            },
        );

        let templates = gateway.templates();
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].id, "coder");
    }

    #[test]
    fn resolve_specialization_matches_role_keyword() {
        let gateway = SwarmGateway::new(
            Workspace {
                root: std::env::temp_dir(),
                user: UserProfile::default(),
                agents: vec![
                    AgentSpec {
                        name: "router".into(),
                        role: "Route tasks".into(),
                        api_provider: "nvidia".into(),
                        model_id: "meta/llama-3.1-70b-instruct".into(),
                        allowed_tools: vec!["system:spawn_worker".into(), "done".into()],
                        instructions: "Route".into(),
                        raw_markdown: "Route".into(),
                    },
                    AgentSpec {
                        name: "coder".into(),
                        role: "Solve code and workspace tasks".into(),
                        api_provider: "nvidia".into(),
                        model_id: "meta/llama-3.1-70b-instruct".into(),
                        allowed_tools: vec!["process:spawn".into(), "done".into()],
                        instructions: "Code".into(),
                        raw_markdown: "Code".into(),
                    },
                ],
            },
            Arc::new(AsyncRwLock::new(MetaBrain::new())),
            Arc::new(RecordingProviderFactory {
                requested_providers: Arc::new(Mutex::new(Vec::new())),
            }),
            PlannerConfig {
                model: "stub".into(),
                max_subtasks: 10,
                temperature: 0.1,
                max_tokens: 256,
            },
            InjectorConfig {
                top_k_memories: 5,
                max_memory_chars: 600,
            },
        );

        assert_eq!(
            gateway.resolve_specialization("code").as_deref(),
            Some("coder")
        );
    }

    #[tokio::test]
    async fn request_and_apply_consent_classifies_paths_and_domains() {
        let mut brain = MetaBrain::new();
        brain.attach_persistence(
            std::env::temp_dir().join(format!("moat_swarm_brain_{}.bin", std::process::id())),
        );
        let brain = Arc::new(AsyncRwLock::new(brain));
        let requests = Arc::new(Mutex::new(Vec::<ConsentRequest>::new()));
        let requests_clone = Arc::clone(&requests);
        let bridge = Arc::new(ConsentBridge::new(Box::new(move |req| {
            requests_clone.lock().expect("lock requests").push(req);
        })));

        let workspace = Arc::new(Workspace {
            root: std::env::temp_dir(),
            user: UserProfile::default(),
            agents: vec![AgentSpec {
                name: "executor".into(),
                role: "executes".into(),
                api_provider: "nvidia".into(),
                model_id: "meta/llama-3.1-70b-instruct".into(),
                allowed_tools: vec!["fs:read".into(), "net:curl".into(), "done".into()],
                instructions: "Do the work".into(),
                raw_markdown: "Do the work".into(),
            }],
        });
        let agent = SwarmAgent {
            workspace,
            spec: AgentSpec {
                name: "executor".into(),
                role: "executes".into(),
                api_provider: "nvidia".into(),
                model_id: "meta/llama-3.1-70b-instruct".into(),
                allowed_tools: vec!["fs:read".into(), "net:curl".into(), "done".into()],
                instructions: "Do the work".into(),
                raw_markdown: "Do the work".into(),
            },
            brain: Arc::clone(&brain),
            consent_bridge: Some(Arc::clone(&bridge)),
        };

        let path_intent = ActionIntent {
            thought: None,
            action: "fs:read".into(),
            target: "/tmp/demo.txt".into(),
            payload: Some(json!("/tmp/demo.txt")),
        };
        let domain_intent = ActionIntent {
            thought: None,
            action: "net:curl".into(),
            target: "https://api.github.com/".into(),
            payload: Some(json!("https://api.github.com/")),
        };

        let resolver = Arc::clone(&bridge);
        let requests_for_path = Arc::clone(&requests);
        tokio::spawn(async move {
            loop {
                if let Some(req) = requests_for_path
                    .lock()
                    .expect("lock requests")
                    .last()
                    .cloned()
                {
                    let _ = resolver.resolve(&req.request_id, ConsentDecision::ApproveOnce);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        agent
            .request_and_apply_consent(&path_intent)
            .await
            .expect("apply path consent");
        {
            let brain = brain.read().await;
            assert!(brain.path_has_permission("/tmp/demo.txt", PermissionFlags::FILE_READ));
        }

        let requests_clone = Arc::clone(&requests);
        let resolver = Arc::clone(&bridge);
        tokio::spawn(async move {
            loop {
                if requests_clone.lock().expect("lock requests").len() >= 2 {
                    let req = requests_clone
                        .lock()
                        .expect("lock requests")
                        .last()
                        .cloned()
                        .expect("latest request");
                    let _ = resolver.resolve(&req.request_id, ConsentDecision::ApproveOnce);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        agent
            .request_and_apply_consent(&domain_intent)
            .await
            .expect("apply domain consent");
        let brain = brain.read().await;
        assert!(brain.domain_has_permission("api.github.com", PermissionFlags::NET_OUTBOUND));
    }
}
