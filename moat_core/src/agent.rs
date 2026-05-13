use crate::capabilities::CapabilitySet;
use crate::context::{ContextStore, Message, TokenBudget, BudgetStatus, ContextCompressor};
use crate::llm::{LlmClient, LlmMessage, agent1_tools, agent2_tools};
use crate::session::{SessionStore, AgentConfig, MessageRecord};
use crate::profile::{ProfileStore, ProfileExtractor};
use crate::tools::ToolExecutor;
use anyhow::{Result, bail};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub enum AgentId {
    Agent1,
    Agent2,
}

pub enum AgentResponse {
    Text(String),
    Delegation { task: String, reason: String },
}

#[derive(Debug, Clone)]
pub struct LoopConfig {
    pub max_iterations: u32,
    pub max_tool_calls: u32,
    pub enable_progressive_disclosure: bool,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: 12,
            max_tool_calls: 16,
            enable_progressive_disclosure: true,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct LoopState {
    pub iteration: u32,
    pub tool_calls: u32,
    pub last_tool_failed: bool,
}

enum AgentAction {
    Respond(String),
    UseTool { tool_name: String, args: serde_json::Value },
}

pub struct Agent1 {
    pub capabilities: CapabilitySet,
    context: ContextStore,
    token_budget: TokenBudget,
    llm: LlmClient,
    session_id: String,
    session_store: Arc<SessionStore>,
    profile_store: Arc<ProfileStore>,
    loop_config: LoopConfig,
    loop_state: LoopState,
}

impl Agent1 {
    pub fn new(
        llm: LlmClient,
        session_store: Arc<SessionStore>,
        profile_store: Arc<ProfileStore>,
        resume: bool,
    ) -> Result<Self> {
        let system_prompt = AgentConfig::load_agent1_prompt()?;

        let session_id = if resume {
            match session_store.get_latest_session("agent1")? {
                Some(session) => {
                    println!("[Agent1] Resuming session {}", &session.id[..8]);
                    session.id
                }
                None => {
                    let id = session_store.create_session("agent1", None)?;
                    println!("[Agent1] No previous session found, started {}", &id[..8]);
                    id
                }
            }
        } else {
            let id = session_store.create_session("agent1", None)?;
            println!("[Agent1] New session {}", &id[..8]);
            id
        };

        let context = ContextStore::new(&system_prompt);
        let messages = session_store.load_messages(&session_id)?;

        let msg_count = messages.len();
        for msg in &messages {
            let role = match msg.role.as_str() {
                "user" => crate::context::Role::User,
                "assistant" => crate::context::Role::Assistant,
                "tool" => crate::context::Role::Tool,
                _ => continue,
            };
            context.append(crate::context::Message {
                role,
                content: msg.content.clone(),
                timestamp: msg.timestamp as u64,
            })?;
        }

        if msg_count > 0 {
            println!("[Agent1] Loaded {} messages from storage", msg_count);
        }

        // Inject user profile into context if available
        let profile_injection = profile_store.build_context_injection()?;
        if !profile_injection.is_empty() {
            println!("[Agent1] Injecting user profile ({} chars)", profile_injection.len());
            context.prepend_system_addendum(&profile_injection)?;
        }

        Ok(Self {
            capabilities: CapabilitySet::agent1_default(),
            context,
            token_budget: TokenBudget::new_128k(),
            llm,
            session_id,
            session_store,
            profile_store,
            loop_config: LoopConfig::default(),
            loop_state: LoopState::default(),
        })
    }

    pub async fn process_query(&mut self, query: &str, tool_executor: &mut ToolExecutor) -> Result<AgentResponse> {
        self.context.append(Message::user(query))?;
        self.session_store.append_message(&self.session_id, "user", query)?;

        let recent = vec![MessageRecord {
            id: 0,
            session_id: self.session_id.clone(),
            role: "user".into(),
            content: query.to_string(),
            timestamp: 0,
        }];
        ProfileExtractor::extract_and_store(&recent, &self.session_id, &self.profile_store)?;

        self.check_and_compress_context()?;

        self.run_loop(query, tool_executor).await
    }

    async fn safe_execute_tool(
        &mut self,
        tool_name: &str,
        args_json: &str,
        tool_executor: &mut ToolExecutor,
    ) -> Result<String> {
        match tool_executor.execute(tool_name, args_json) {
            Ok(output) => {
                self.loop_state.last_tool_failed = false;
                Ok(output)
            }
            Err(e) => {
                self.loop_state.last_tool_failed = true;
                let wrapped = format!(
                    "[TOOL_ERROR] Tool '{}' failed with error: {}. \
                    Do NOT retry the same call blindly. Consider alternative strategies.",
                    tool_name, e
                );
                self.context.append(Message::tool(&wrapped))?;
                Ok(wrapped)
            }
        }
    }

    fn maybe_inject_tool_details(&mut self, user_query: &str) -> Result<()> {
        if !self.loop_config.enable_progressive_disclosure {
            return Ok(());
        }
        if self.loop_state.iteration == 1 {
            return Ok(());
        }
        if self.context.has_tag("tool_details_injected") {
            return Ok(());
        }
        if self.loop_state.tool_calls > 0
            || user_query.to_lowercase().contains("tool")
            || user_query.to_lowercase().contains("command")
            || user_query.to_lowercase().contains("file")
        {
            let details = crate::llm::tool_definitions::tool_schema_details();
            self.context.prepend_system_addendum(&format!(
                "\n## Tool Details (Extended)\n{}\n", details
            ))?;
            self.context.mark_tag("tool_details_injected");
        }
        Ok(())
    }

    pub async fn run_loop(
        &mut self,
        user_query: &str,
        tool_executor: &mut ToolExecutor,
    ) -> Result<AgentResponse> {
        self.loop_state = LoopState::default();

        loop {
            self.loop_state.iteration += 1;

            if self.loop_state.iteration > self.loop_config.max_iterations {
                let msg = format!(
                    "Iteration budget exhausted after {} steps. \
                    I need you to either narrow the task or confirm I should continue in a new request.",
                    self.loop_config.max_iterations
                );
                return Ok(AgentResponse::Text(msg));
            }

            self.maybe_inject_tool_details(user_query)?;

            let messages = self.context_to_llm_messages();
            let response = self.llm.chat(messages, Some(agent1_tools())).await?;
            let choice = &response.choices[0];

            if let Some(tool_calls) = &choice.message.tool_calls {
                let mut llm_messages = self.context_to_llm_messages();

                let assistant_msg = LlmMessage {
                    role: "assistant".into(),
                    content: choice.message.content.clone().unwrap_or_default(),
                    tool_call_id: None,
                    tool_calls: Some(tool_calls.clone()),
                };
                llm_messages.push(assistant_msg);

                for tool_call in tool_calls {
                    if tool_call.function.name == "delegate_to_executor" {
                        let args: serde_json::Value = serde_json::from_str(&tool_call.function.arguments)
                            .unwrap_or(serde_json::Value::Null);
                        let task = args["task"].as_str().unwrap_or("").to_string();
                        let reason = args["reason"].as_str().unwrap_or("requires execution").to_string();
                        println!("[Agent1] Delegating to Agent2: {}", task);
                        return Ok(AgentResponse::Delegation { task, reason });
                    }

                    self.loop_state.tool_calls += 1;
                    if self.loop_state.tool_calls > self.loop_config.max_tool_calls {
                        let msg = format!(
                            "Tool-call budget exhausted after {} tools. \
                            Stopping to avoid runaway execution.",
                            self.loop_config.max_tool_calls
                        );
                        return Ok(AgentResponse::Text(msg));
                    }

                    let tool_result = self.safe_execute_tool(
                        &tool_call.function.name,
                        &tool_call.function.arguments,
                        tool_executor,
                    ).await?;

                    llm_messages.push(LlmMessage::tool_result(&tool_call.id, &tool_result));
                    self.context.append(Message::tool(&tool_result))?;
                }

                let final_response = self.llm.chat(llm_messages, None).await?;
                let final_text = final_response.choices[0].message.content.clone()
                    .unwrap_or_else(|| "I completed the requested operation.".into());

                self.context.append(Message::assistant(&final_text))?;
                self.session_store.append_message(&self.session_id, "assistant", &final_text)?;
                return Ok(AgentResponse::Text(final_text));
            }

            let text = choice.message.content.clone()
                .unwrap_or_else(|| "I don't have a response for that.".into());
            self.context.append(Message::assistant(&text))?;
            self.session_store.append_message(&self.session_id, "assistant", &text)?;
            return Ok(AgentResponse::Text(text));
        }
    }

    fn context_to_llm_messages(&self) -> Vec<LlmMessage> {
        self.context.get_all()
            .into_iter()
            .map(|m| {
                let role = match m.role {
                    crate::context::Role::System => "system",
                    crate::context::Role::User => "user",
                    crate::context::Role::Assistant => "assistant",
                    crate::context::Role::Tool => "tool",
                };
                LlmMessage {
                    role: role.into(),
                    content: m.content,
                    tool_call_id: None,
                    tool_calls: None,
                }
            })
            .collect()
    }

    fn check_and_compress_context(&mut self) -> Result<()> {
        let all_messages = self.context.get_all();
        let full_text = serde_json::to_string(&all_messages)?;
        self.token_budget.update(&full_text)?;

        if let BudgetStatus::NeedsCompression { current, max } = self.token_budget.status() {
            println!("[Agent1] Context at {}/{} tokens, compressing...", current, max);

            let mut messages = all_messages;
            ContextCompressor::precompress_tool_outputs(&mut messages);
            let compressed = ContextCompressor::compress_tiered(messages)?;

            let compressed_text = serde_json::to_string(&compressed)?;
            let new_count = TokenBudget::count_tokens(&compressed_text)?;

            if ContextCompressor::compression_saved_enough(current, new_count, 0.15) {
                self.context.replace_all(compressed)?;
                self.token_budget.update(&compressed_text)?;
                println!("[Agent1] Compressed to {} tokens", new_count);
            } else {
                bail!("Context full — anti-thrash guard triggered. Start a new session.");
            }
        }

        Ok(())
    }
}

pub struct Agent2 {
    pub capabilities: CapabilitySet,
    context: ContextStore,
    token_budget: TokenBudget,
    llm: LlmClient,
    session_id: String,
    session_store: Arc<SessionStore>,
    profile_store: Arc<ProfileStore>,
    loop_config: LoopConfig,
    loop_state: LoopState,
}

impl Agent2 {
    pub fn new(
        llm: LlmClient,
        session_store: Arc<SessionStore>,
        profile_store: Arc<ProfileStore>,
        resume: bool,
    ) -> Result<Self> {
        let system_prompt = AgentConfig::load_agent2_prompt()?;

        let session_id = if resume {
            match session_store.get_latest_session("agent2")? {
                Some(session) => {
                    println!("[Agent2] Resuming session {}", &session.id[..8]);
                    session.id
                }
                None => {
                    let id = session_store.create_session("agent2", None)?;
                    println!("[Agent2] No previous session found, started {}", &id[..8]);
                    id
                }
            }
        } else {
            let id = session_store.create_session("agent2", None)?;
            println!("[Agent2] New session {}", &id[..8]);
            id
        };

        let context = ContextStore::new(&system_prompt);
        let messages = session_store.load_messages(&session_id)?;

        let msg_count = messages.len();
        for msg in &messages {
            let role = match msg.role.as_str() {
                "user" => crate::context::Role::User,
                "assistant" => crate::context::Role::Assistant,
                "tool" => crate::context::Role::Tool,
                _ => continue,
            };
            context.append(crate::context::Message {
                role,
                content: msg.content.clone(),
                timestamp: msg.timestamp as u64,
            })?;
        }

        if msg_count > 0 {
            println!("[Agent2] Loaded {} messages from storage", msg_count);
        }

        // Inject user profile into context if available
        let profile_injection = profile_store.build_context_injection()?;
        if !profile_injection.is_empty() {
            println!("[Agent2] Injecting user profile ({} chars)", profile_injection.len());
            context.prepend_system_addendum(&profile_injection)?;
        }

        Ok(Self {
            capabilities: CapabilitySet::agent2_default(),
            context,
            token_budget: TokenBudget::new_128k(),
            llm,
            session_id,
            session_store,
            profile_store,
            loop_config: LoopConfig::default(),
            loop_state: LoopState::default(),
        })
    }

    pub async fn process_query(&mut self, query: &str, tool_executor: &mut ToolExecutor) -> Result<AgentResponse> {
        self.context.append(Message::user(query))?;
        self.session_store.append_message(&self.session_id, "user", query)?;

        // Extract preferences from user message
        let recent = vec![MessageRecord {
            id: 0,
            session_id: self.session_id.clone(),
            role: "user".into(),
            content: query.to_string(),
            timestamp: 0,
        }];
        ProfileExtractor::extract_and_store(&recent, &self.session_id, &self.profile_store)?;

        self.check_and_compress_context()?;

        let messages = self.context_to_llm_messages();
        let response = self.llm.chat(messages, Some(agent2_tools())).await?;
        let choice = &response.choices[0];

        if let Some(tool_calls) = &choice.message.tool_calls {
            let mut llm_messages = self.context_to_llm_messages();

            let assistant_msg = LlmMessage {
                role: "assistant".into(),
                content: choice.message.content.clone().unwrap_or_default(),
                tool_call_id: None,
                tool_calls: Some(tool_calls.clone()),
            };
            llm_messages.push(assistant_msg);

            for tool_call in tool_calls {
                let tool_result = tool_executor.execute(
                    &tool_call.function.name,
                    &tool_call.function.arguments,
                )?;

                llm_messages.push(LlmMessage::tool_result(
                    &tool_call.id,
                    &tool_result,
                ));

                self.context.append(Message::tool(&tool_result))?;
            }

            let final_response = self.llm.chat(llm_messages, None).await?;
            let final_text = final_response.choices[0]
                .message.content.clone()
                .unwrap_or_else(|| "Operation completed.".into());

            self.context.append(Message::assistant(&final_text))?;
            self.session_store.append_message(&self.session_id, "assistant", &final_text)?;
            return Ok(AgentResponse::Text(final_text));
        }

        let text = choice.message.content.clone()
            .unwrap_or_else(|| "I don't have a response for that.".into());

        self.context.append(Message::assistant(&text))?;
        self.session_store.append_message(&self.session_id, "assistant", &text)?;
        Ok(AgentResponse::Text(text))
    }

    fn context_to_llm_messages(&self) -> Vec<LlmMessage> {
        self.context.get_all()
            .into_iter()
            .map(|m| {
                let role = match m.role {
                    crate::context::Role::System => "system",
                    crate::context::Role::User => "user",
                    crate::context::Role::Assistant => "assistant",
                    crate::context::Role::Tool => "tool",
                };
                LlmMessage {
                    role: role.into(),
                    content: m.content,
                    tool_call_id: None,
                    tool_calls: None,
                }
            })
            .collect()
    }

    fn check_and_compress_context(&mut self) -> Result<()> {
        let all_messages = self.context.get_all();
        let full_text = serde_json::to_string(&all_messages)?;
        self.token_budget.update(&full_text)?;

        if let BudgetStatus::NeedsCompression { current, max } = self.token_budget.status() {
            println!("[Agent2] Context at {}/{} tokens, compressing...", current, max);

            let mut messages = all_messages;
            ContextCompressor::precompress_tool_outputs(&mut messages);
            let compressed = ContextCompressor::compress_tiered(messages)?;

            let compressed_text = serde_json::to_string(&compressed)?;
            let new_count = TokenBudget::count_tokens(&compressed_text)?;

            if ContextCompressor::compression_saved_enough(current, new_count, 0.15) {
                self.context.replace_all(compressed)?;
                self.token_budget.update(&compressed_text)?;
                println!("[Agent2] Compressed to {} tokens", new_count);
            } else {
                bail!("Context full — anti-thrash guard triggered. Start a new session.");
            }
        }

        Ok(())
    }
}