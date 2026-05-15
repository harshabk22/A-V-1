use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use anyhow::Result;

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
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: strip_instruction_delimiters(content.into()),
            timestamp: Self::now(),
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            timestamp: Self::now(),
            tool_call_id: None,
        }
    }

    pub fn tool(content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: strip_instruction_delimiters(content.into()),
            timestamp: Self::now(),
            tool_call_id: None,
        }
    }

    pub fn tool_with_id(content: impl Into<String>, tool_call_id: String) -> Self {
        Self {
            role: Role::Tool,
            content: strip_instruction_delimiters(content.into()),
            timestamp: Self::now(),
            tool_call_id: Some(tool_call_id),
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
        let mut store = ContextStore::new("system prompt");
        assert!(!store.has_tag("tool_details_injected"));
        store.mark_tag("tool_details_injected");
        assert!(store.has_tag("tool_details_injected"));
        assert!(!store.has_tag("nonexistent"));
    }
}