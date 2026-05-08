use crate::swarm::{AgentSpec, UserProfile};
use crate::system::HostEnvironment;

use crate::MoatResult;
use chrono::Local;
use std::fmt::Write as _;

#[derive(Debug, Clone)]
pub struct InjectorConfig {
    pub top_k_memories: usize,
    pub max_memory_chars: usize,
}

fn host_environment_xml() -> String {
    let env = HostEnvironment::detect();
    let os_name = env.os_name;
    let username = env.username;
    let home_dir = env.home_dir.display().to_string();
    format!(
        "<host_environment>\nOS: {os_name}\nUser: {username}\nHome Directory: {home_dir}\nRule: You are operating on {os_name}. The current user is '{username}'. The absolute home directory is '{home_dir}'. You MUST use these exact absolute paths. Do NOT hallucinate or guess generic paths like '/Users/username/'.\n</host_environment>"
    )
}

pub fn build_injected_prompt(user_query: &str, memory_context_xml: &str) -> MoatResult<String> {
    build_injected_prompt_with_swarm(user_query, memory_context_xml, None, None, None, None)
}

pub fn build_injected_prompt_with_swarm(
    user_query: &str,
    memory_context_xml: &str,
    precomputed_context: Option<&str>,
    user_profile: Option<&UserProfile>,
    agent_spec: Option<&AgentSpec>,
    shared_execution_notes: Option<&str>,
) -> MoatResult<String> {
    let current_time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let system_state = format!(
        "<system_state>\nCurrent Time: {}\nOS: {}\n</system_state>",
        current_time,
        HostEnvironment::detect().os_name
    );
    let host_environment = host_environment_xml();

    let capacity = 160 + user_query.len();
    let mut prompt = String::with_capacity(
        capacity + memory_context_xml.len() + system_state.len() + host_environment.len(),
    );
    prompt.push_str(memory_context_xml.trim());
    if let Some(profile) = user_profile {
        prompt.push_str("\n<user_profile>\n");
        prompt.push_str(&profile.raw_markdown);
        prompt.push_str("\n</user_profile>");
    }
    if let Some(spec) = agent_spec {
        prompt.push_str("\n<agent_spec>\n");
        prompt.push_str(&spec.raw_markdown);
        prompt.push_str("\n</agent_spec>");
    }
    if let Some(context) = precomputed_context {
        let trimmed = context.trim();
        if !trimmed.is_empty() {
            prompt.push_str("\n[SESSION CONTEXT]\n");
            prompt.push_str(trimmed);
        }
    }
    if let Some(notes) = shared_execution_notes {
        let trimmed = notes.trim();
        if !trimmed.is_empty() {
            prompt.push_str("\n[EXECUTION NOTES]\n");
            prompt.push_str(trimmed);
        }
    }

    prompt.push_str("[USER QUERY]\n");
    prompt.push_str(user_query);
    prompt.push('\n');
    prompt.push_str(&host_environment);
    prompt.push('\n');
    prompt.push_str(&system_state);
    Ok(prompt)
}

/// Slim context injection for atomic/leaf workflow nodes that don't need
/// recalled knowledge retrieval or the full durable knowledge block.
/// Saves ~1,500-2,000 tokens per worker invocation.
///
/// Retains: agent_spec (worker identity + tool list), host_environment, system_state,
///          precomputed_context (workflow global context), and the query itself.
/// Omits:   durable knowledge block, recalled knowledge search, user_profile.
pub fn build_slim_prompt(
    user_query: &str,
    precomputed_context: Option<&str>,
    agent_spec: Option<&AgentSpec>,
    shared_execution_notes: Option<&str>,
) -> MoatResult<String> {
    let env = HostEnvironment::detect();
    let current_time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut prompt = String::with_capacity(512 + user_query.len());

    if let Some(spec) = agent_spec {
        prompt.push_str("<agent_spec>\n");
        prompt.push_str(&spec.raw_markdown);
        prompt.push_str("\n</agent_spec>\n");
    }
    if let Some(ctx) = precomputed_context {
        let trimmed = ctx.trim();
        if !trimmed.is_empty() {
            prompt.push_str("[SESSION CONTEXT]\n");
            prompt.push_str(trimmed);
            prompt.push('\n');
        }
    }
    if let Some(notes) = shared_execution_notes {
        let trimmed = notes.trim();
        if !trimmed.is_empty() {
            prompt.push_str("[EXECUTION NOTES]\n");
            prompt.push_str(trimmed);
            prompt.push('\n');
        }
    }
    prompt.push_str("[USER QUERY]\n");
    prompt.push_str(user_query);
    prompt.push('\n');
    let _ = write!(
        prompt,
        "<host_environment>\nOS: {}\nUser: {}\nHome Directory: {}\n</host_environment>\n\
         <system_state>\nCurrent Time: {}\n</system_state>",
        env.os_name,
        env.username,
        env.home_dir.display(),
        current_time
    );
    Ok(prompt)
}

#[cfg(test)]
mod tests {
    use super::{build_injected_prompt, build_injected_prompt_with_swarm};
    use crate::swarm::{AgentSpec, UserProfile};

    #[test]
    fn injected_prompt_places_system_state_after_query() {
        let prompt = build_injected_prompt(
            "Who am I?",
            "<knowledge_memory><facts><fact>Name: Harsha</fact></facts><search_hints>harsha identity</search_hints></knowledge_memory>",
        )
        .expect("build prompt");

        let meta_pos = prompt.find("<knowledge_memory>").expect("knowledge memory");
        let query_pos = prompt.find("[USER QUERY]").expect("query");
        let host_pos = prompt.find("<host_environment>").expect("host environment");
        let system_pos = prompt.find("<system_state>").expect("system state");
        assert!(meta_pos < query_pos);
        assert!(query_pos < host_pos);
        assert!(host_pos < system_pos);
        assert!(!prompt.contains("<rules>"));
        assert!(prompt.contains("Home Directory: "));
        assert!(prompt.contains("Do NOT hallucinate or guess generic paths"));
        assert!(prompt.ends_with("</system_state>"));
    }

    #[test]
    fn injected_prompt_includes_only_specific_swarm_context() {
        let prompt = build_injected_prompt_with_swarm(
            "Plan the task",
            "<knowledge_memory><facts></facts><search_hints>task</search_hints></knowledge_memory>",
            Some("Prior context summary."),
            Some(&UserProfile {
                name: "Harsha".into(),
                core_preferences: "prefers concise summaries".into(),
                chat_model: None,
                planner_model: None,
                worker_model: None,
                summary: "Harsha is the operator.".into(),
                facts: vec!["prefers concise summaries".into()],
                body_markdown: "Harsha is the operator.\n- prefers concise summaries".into(),
                raw_markdown: "# User\nHarsha is the operator.\n- prefers concise summaries".into(),
            }),
            Some(&AgentSpec {
                name: "planner".into(),
                role: "Plans work".into(),
                api_provider: "nvidia".into(),
                model_id: "meta/llama-3.1-405b-instruct".into(),
                allowed_tools: vec!["done".into()],
                instructions: "Focus on decomposition".into(),
                raw_markdown: "## planner\nPlans work.\nFocus on decomposition".into(),
            }),
            None,
        )
        .expect("build swarm prompt");

        assert!(prompt.contains("<user_profile>"));
        assert!(prompt.contains("Harsha is the operator."));
        assert!(prompt.contains("[SESSION CONTEXT]"));
        assert!(prompt.contains("<agent_spec>"));
        assert!(prompt.contains("Focus on decomposition"));
        assert!(!prompt.contains("executor"));
    }
}
