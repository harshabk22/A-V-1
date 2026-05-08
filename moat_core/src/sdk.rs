use crate::gateway::SessionSource;
use crate::MoatResult;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Base message abstraction for all gateway channels
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlatformMessage {
    pub message_id: String,
    pub thread_id: String,
    pub content: String,
    pub sender: String,
    pub attachments: Vec<String>,
}

/// A generic channel adapter that pushes user inputs to the Gateway Control Plane
#[async_trait]
pub trait GatewayAdapter: Send + Sync {
    /// Returns a unique identifier for this adapter (e.g. "discord", "whatsapp", "webchat")
    fn adapter_id(&self) -> &str;

    /// Returns the session source type
    fn source_type(&self) -> SessionSource;

    /// Connects the adapter to the external platform and begins streaming messages
    async fn start(&self) -> MoatResult<()>;

    /// Disconnects gracefully
    async fn stop(&self) -> MoatResult<()>;

    /// Sends an outgoing message back to the external platform channel
    async fn send_reply(&self, thread_id: &str, reply: &str) -> MoatResult<()>;
}

/// Allows plugins to easily inject memory or system state seamlessly
#[async_trait]
pub trait ContextEngine: Send + Sync {
    fn engine_name(&self) -> &str;
    async fn fetch_context(&self, query: &str) -> MoatResult<String>;
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeHook {
    GatewayStart,
    GatewayStop,
    SessionStart,
    SessionEnd,
    BeforePromptBuild,
    BeforeReply,
    BeforeToolCall,
    AfterToolCall,
    BeforeCompaction,
    AfterCompaction,
}

impl RuntimeHook {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GatewayStart => "gateway_start",
            Self::GatewayStop => "gateway_stop",
            Self::SessionStart => "session_start",
            Self::SessionEnd => "session_end",
            Self::BeforePromptBuild => "before_prompt_build",
            Self::BeforeReply => "before_reply",
            Self::BeforeToolCall => "before_tool_call",
            Self::AfterToolCall => "after_tool_call",
            Self::BeforeCompaction => "before_compaction",
            Self::AfterCompaction => "after_compaction",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeHookContext {
    pub hook: RuntimeHook,
    pub session_id: Option<String>,
    pub source: Option<SessionSource>,
    pub request_id: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeHookResult {
    #[serde(default)]
    pub context_block: Option<String>,
    #[serde(default)]
    pub transformed_payload: Option<Value>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[async_trait]
pub trait RuntimeHookPlugin: Send + Sync {
    fn plugin_id(&self) -> &str;
    fn hooks(&self) -> &[RuntimeHook];

    async fn invoke_hook(
        &self,
        context: RuntimeHookContext,
    ) -> MoatResult<Option<RuntimeHookResult>>;
}

#[derive(Default)]
pub struct RuntimeHookRegistry {
    plugins: Vec<std::sync::Arc<dyn RuntimeHookPlugin>>,
}

impl RuntimeHookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, plugin: std::sync::Arc<dyn RuntimeHookPlugin>) {
        if self
            .plugins
            .iter()
            .any(|existing| existing.plugin_id() == plugin.plugin_id())
        {
            return;
        }
        self.plugins.push(plugin);
    }

    pub async fn invoke_all(
        &self,
        hook: RuntimeHook,
        mut context: RuntimeHookContext,
    ) -> MoatResult<Vec<RuntimeHookResult>> {
        context.hook = hook;
        let mut results = Vec::new();
        for plugin in &self.plugins {
            if !plugin.hooks().contains(&hook) {
                continue;
            }
            if let Some(result) = plugin.invoke_hook(context.clone()).await? {
                results.push(result);
            }
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::{RuntimeHook, RuntimeHookContext, RuntimeHookPlugin, RuntimeHookRegistry};
    use crate::MoatResult;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Arc;

    struct TestPlugin;

    #[async_trait]
    impl RuntimeHookPlugin for TestPlugin {
        fn plugin_id(&self) -> &str {
            "test"
        }

        fn hooks(&self) -> &[RuntimeHook] {
            &[RuntimeHook::BeforePromptBuild]
        }

        async fn invoke_hook(
            &self,
            context: RuntimeHookContext,
        ) -> MoatResult<Option<super::RuntimeHookResult>> {
            Ok(Some(super::RuntimeHookResult {
                context_block: Some(format!("hook={}", context.hook.as_str())),
                transformed_payload: Some(json!({"ok": true})),
                warnings: Vec::new(),
            }))
        }
    }

    #[tokio::test]
    async fn hook_registry_invokes_matching_plugins_once() {
        let plugin = Arc::new(TestPlugin);
        let mut registry = RuntimeHookRegistry::new();
        registry.register(plugin.clone());
        registry.register(plugin);

        let results = registry
            .invoke_all(
                RuntimeHook::BeforePromptBuild,
                RuntimeHookContext {
                    hook: RuntimeHook::GatewayStart,
                    session_id: None,
                    source: None,
                    request_id: None,
                    payload: json!({}),
                },
            )
            .await
            .expect("invoke hooks");

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].context_block.as_deref(),
            Some("hook=before_prompt_build")
        );
    }
}
