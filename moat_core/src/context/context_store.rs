use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use anyhow::{Result, bail};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    pub timestamp: u64,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<crate::llm::ToolCall>>,
}

const SYSTEM_START: &str = "<|SYSTEM_START|>";
const SYSTEM_END: &str = "<|SYSTEM_END|>";

fn strip_instruction_delimiters(mut s: String) -> String {
    for pattern in &[SYSTEM_START, SYSTEM_END, "</system>", "<system>"] {
        s = s.replace(pattern, "[FILTERED]");
    }
    s
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        let inner = content.into();
        let wrapped = format!("{}\n{}\n{}", SYSTEM_START, inner, SYSTEM_END);
        Self {
            role: Role::System,
            content: wrapped,
            timestamp: Self::now(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: strip_instruction_delimiters(content.into()),
            timestamp: Self::now(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            timestamp: Self::now(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    pub fn assistant_with_tool_calls(
        content: impl Into<String>,
        tool_calls: Vec<crate::llm::ToolCall>,
    ) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            timestamp: Self::now(),
            tool_call_id: None,
            tool_calls: Some(tool_calls),
        }
    }

    pub fn tool(content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: strip_instruction_delimiters(content.into()),
            timestamp: Self::now(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    pub fn tool_with_id(content: impl Into<String>, tool_call_id: String) -> Self {
        Self {
            role: Role::Tool,
            content: strip_instruction_delimiters(content.into()),
            timestamp: Self::now(),
            tool_call_id: Some(tool_call_id),
            tool_calls: None,
        }
    }

    pub fn tool_result(call_id: String, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: strip_instruction_delimiters(content.into()),
            timestamp: Self::now(),
            tool_call_id: Some(call_id),
            tool_calls: None,
        }
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}

pub struct ContextStore {
    messages: Arc<Mutex<Vec<Message>>>,
    #[allow(dead_code)]
    system_prompt: String,
    tags: Arc<Mutex<Vec<String>>>,
}

impl ContextStore {
    pub fn new(system_prompt: impl Into<String>) -> Self {
        let prompt = system_prompt.into();
        let messages = vec![Message::system(prompt.clone())];

        Self {
            messages: Arc::new(Mutex::new(messages)),
            system_prompt: prompt,
            tags: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn append(&self, message: Message) -> Result<()> {
        let mut msgs = self.messages.lock().unwrap_or_else(|e| e.into_inner());
        // PC-01 sequencing invariant: tool messages must follow
        // assistant-with-tool-calls or another tool message
        if message.role == Role::Tool {
            let last = msgs.last().ok_or_else(|| {
                anyhow::anyhow!("Tool message appended to empty context — protocol violation")
            })?;
            match &last.role {
                Role::Assistant if last.tool_calls.is_some() => {}
                Role::Tool => {}
                other => bail!(
                    "PC-01 violation: tool message follows {:?}, expected assistant-with-tool-calls or tool",
                    other
                ),
            }
        }
        msgs.push(message);
        Ok(())
    }

    pub fn get_all(&self) -> Vec<Message> {
        self.messages.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn replace_all(&self, messages: Vec<Message>) -> Result<()> {
        let mut msgs = self.messages.lock().unwrap_or_else(|e| e.into_inner());
        *msgs = messages;
        Ok(())
    }

    pub fn count(&self) -> usize {
        self.messages.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Maximum size of the primary system message before addenda overflow
    /// to separate system messages instead of bloating the first one.
    const MAX_SYSTEM_PROMPT_SIZE: usize = 8000;

    pub fn prepend_system_addendum(&self, addendum: &str) -> Result<()> {
        let mut msgs = self.messages.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(first) = msgs.first() {
            if first.role == Role::System && first.content.len() < Self::MAX_SYSTEM_PROMPT_SIZE {
                // Still has room — append to the first system message
                if let Some(first_mut) = msgs.first_mut() {
                    first_mut.content = format!("{}\n{}", first_mut.content, addendum);
                }
            } else {
                // System prompt too large — insert a separate system-addendum message
                msgs.insert(1, Message::system(format!("[System Addendum]\n{}", addendum)));
            }
        }
        Ok(())
    }

    pub fn mark_tag(&self, tag: &str) {
        if let Ok(mut tags) = self.tags.lock() {
            if !tags.iter().any(|t| t == tag) {
                tags.push(tag.to_string());
            }
        }
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.lock().map(|tags| tags.iter().any(|t| t == tag)).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mark_tag_and_has_tag() {
        let store = ContextStore::new("system prompt");
        assert!(!store.has_tag("tool_details_injected"));
        store.mark_tag("tool_details_injected");
        assert!(store.has_tag("tool_details_injected"));
        assert!(!store.has_tag("nonexistent"));
    }

    /// PC-01: Appending a tool message after a user message must return
    /// an error — the LLM protocol requires tool messages to follow
    /// assistant-with-tool-calls or another tool message.
    #[test]
    fn test_pc01_tool_after_user_is_rejected() {
        let store = ContextStore::new("system prompt");
        store.append(Message::user("hello")).expect("append user");
        let result = store.append(Message::tool_with_id("result", "call_1".into()));
        assert!(result.is_err(), "Tool after user must be rejected");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("PC-01"), "Error must cite PC-01 invariant: {}", err);
        assert!(err.contains("User"), "Error must mention the preceding role: {}", err);
    }

    /// PC-02: Valid multi-tool-call sequence — assistant-with-tool-calls
    /// followed by tool messages is accepted. The second tool failing
    /// (producing [tool_error]) must still produce a valid sequence.
    #[test]
    fn test_pc02_multi_tool_call_sequence_is_accepted() {
        use crate::llm::{ToolCall, FunctionCall};
        let store = ContextStore::new("system prompt");

        // Build an assistant message with two tool calls
        let tool_calls = vec![
            ToolCall {
                id: "call_1".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "fs_read".into(),
                    arguments: r#"{"path":"/tmp/a.txt"}"#.into(),
                },
            },
            ToolCall {
                id: "call_2".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "nonexistent_tool".into(),
                    arguments: "{}".into(),
                },
            },
        ];
        let assistant_content = "I'll read the file and try the other tool.";

        // Append assistant-with-tool-calls
        store.append(Message::assistant_with_tool_calls(
            assistant_content,
            tool_calls,
        )).expect("append assistant-with-tool-calls");

        // Append tool result 1 (success)
        store.append(Message::tool_result(
            "call_1".into(),
            "file contents here",
        )).expect("append tool result 1");

        // Append tool result 2 (error — second tool failed)
        store.append(Message::tool_result(
            "call_2".into(),
            "[tool_error] nonexistent_tool: Unknown tool: nonexistent_tool",
        )).expect("append tool result 2");

        // Verify context state
        let msgs = store.get_all();
        // Messages: system, assistant-with-tool-calls, tool, tool
        assert_eq!(msgs.len(), 4, "Expected system + assistant + 2 tool messages");

        assert_eq!(msgs[1].role, Role::Assistant);
        assert!(msgs[1].tool_calls.is_some(), "Assistant must have tool_calls");
        assert_eq!(msgs[1].tool_calls.as_ref().unwrap().len(), 2);

        assert_eq!(msgs[2].role, Role::Tool);
        assert_eq!(msgs[2].tool_call_id.as_deref(), Some("call_1"));
        assert!(msgs[2].content.contains("file contents"),
            "First tool result should have content: {}", msgs[2].content);

        assert_eq!(msgs[3].role, Role::Tool);
        assert_eq!(msgs[3].tool_call_id.as_deref(), Some("call_2"));
        assert!(msgs[3].content.contains("[tool_error]"),
            "Second tool result should be an error: {}", msgs[3].content);

        // Verify the sequence is in correct order (assistant-before-tools)
        assert!(matches!(msgs[1].role, Role::Assistant));
        assert!(matches!(msgs[2].role, Role::Tool));
        assert!(matches!(msgs[3].role, Role::Tool));
    }
}