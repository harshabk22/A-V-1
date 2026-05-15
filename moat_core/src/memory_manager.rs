use crate::context::{ContextStore, Message, ContextCompressor};
use crate::session::SessionStore;
use crate::security::guardrails::GuardrailsEngine;
use crate::security::scanners::{Scanner, SecretScanner, InjectionScanner};
use anyhow::Result;
use std::sync::Arc;

/// Dedicated Memory Manager Agent.
///
/// Operates on bounded windows of messages, produces compressed summaries,
/// filters and redacts sensitive content. Manages per-agent context scoping.
///
/// Constraints:
/// - Retrieval/filtering/scope management ONLY — NO execution
/// - Input always goes through redaction pipeline
/// - Working memory is discarded after each job
/// - Only written outputs in the stores are preserved
pub struct MemoryManagerAgent {
    session_store: Arc<SessionStore>,
    guardrails: GuardrailsEngine,
    /// Max messages to process in a single summarization window.
    window_size: usize,
    /// Max summary output characters.
    max_summary_chars: usize,
}

impl MemoryManagerAgent {
    pub fn new(session_store: Arc<SessionStore>) -> Self {
        Self {
            session_store,
            guardrails: GuardrailsEngine::new()
                .with_secret_scanner()
                .with_injection_scanner(),
            window_size: 50,
            max_summary_chars: 2000,
        }
    }

    /// Summarize recent messages for a given session.
    /// Produces a compressed, redacted summary suitable for context injection.
    pub fn summarize_session(&self, session_id: &str) -> Result<String> {
        let messages = self.session_store.load_messages(session_id)?;

        let window: Vec<&crate::session::MessageRecord> = messages
            .iter()
            .rev()
            .take(self.window_size)
            .rev()
            .collect();

        if window.is_empty() {
            return Ok(String::new());
        }

        let raw_summary = self.build_raw_summary(&window);
        let guard_result = self.guardrails.guard_input(&raw_summary);

        let summary = guard_result.sanitized;
        let summary = if summary.len() > self.max_summary_chars {
            summary[..self.max_summary_chars].to_string()
        } else {
            summary
        };

        Ok(summary)
    }

    /// Build a filtered, scoped context block for an agent.
    /// Strips secrets, redacts PII, and limits to relevant scope.
    pub fn build_scoped_context(
        &self,
        session_id: &str,
        scope_tags: &[&str],
        max_chars: usize,
    ) -> Result<String> {
        let messages = self.session_store.load_messages(session_id)?;

        // Filter by scope tags
        let filtered: Vec<&crate::session::MessageRecord> = messages
            .iter()
            .filter(|m| {
                scope_tags.iter().any(|tag| m.content.to_lowercase().contains(&tag.to_lowercase()))
            })
            .take(self.window_size)
            .collect();

        if filtered.is_empty() {
            return Ok(String::new());
        }

        let raw = self.build_raw_summary(&filtered);
        let guard_result = self.guardrails.guard_input(&raw);

        let context = if guard_result.sanitized.len() > max_chars {
            guard_result.sanitized[..max_chars].to_string()
        } else {
            guard_result.sanitized
        };

        Ok(context)
    }

    /// Produce context injection for an agent prompt.
    /// Returns `"[CONTEXT SUMMARY]\n{summary}"` or empty string.
    pub fn context_injection(&self, session_id: &str, max_chars: usize) -> Result<String> {
        let summary = self.summarize_session(session_id)?;
        if summary.is_empty() {
            return Ok(String::new());
        }
        let truncated = if summary.len() > max_chars {
            summary[..max_chars].to_string()
        } else {
            summary
        };
        Ok(format!("[CONTEXT SUMMARY]\n{}", truncated))
    }

    fn build_raw_summary(&self, messages: &[&crate::session::MessageRecord]) -> String {
        let mut lines = Vec::new();
        for msg in messages {
            match msg.role.as_str() {
                "user" => lines.push(format!("User: {}", msg.content)),
                "assistant" => lines.push(format!("Assistant: {}", msg.content)),
                "tool" => {
                    let truncated = if msg.content.len() > 100 {
                        format!("{}...", &msg.content[..100])
                    } else {
                        msg.content.clone()
                    };
                    lines.push(format!("Tool: {}", truncated));
                }
                _ => {}
            }
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_manager_creates_context_injection() {
        let store = SessionStore::open_with_prefix_in_memory("test_mem_context")
            .expect("open store");
        let store = Arc::new(store);

        let session_id = store.create_session("agent2", None).expect("create session");
        store.append_message(&session_id, "user", "What is Rust?").expect("append");
        store.append_message(&session_id, "assistant", "Rust is a systems programming language.").expect("append");

        let manager = MemoryManagerAgent::new(store);
        let injection = manager.context_injection(&session_id, 500).expect("context");
        assert!(injection.contains("[CONTEXT SUMMARY]"));
        assert!(injection.contains("Rust"));
    }

    #[test]
    fn test_memory_manager_redacts_secrets_in_summary() {
        let store = SessionStore::open_with_prefix_in_memory("test_mem_redact")
            .expect("open store");
        let store = Arc::new(store);

        let session_id = store.create_session("agent2", None).expect("create session");
        store.append_message(&session_id, "user", "api_key=sk-projABCDEFGHIJKLMNOPQRSTUV").expect("append");
        store.append_message(&session_id, "assistant", "I received your key [REDACTED]").expect("append");

        let manager = MemoryManagerAgent::new(store);
        let summary = manager.summarize_session(&session_id).expect("summarize");
        assert!(!summary.contains("sk-proj"), "API key must be redacted: {}", summary);
        assert!(summary.contains("[REDACTED]"), "Redacted marker expected");
    }

    #[test]
    fn test_memory_manager_empty_session() {
        let store = SessionStore::open_with_prefix_in_memory("test_mem_empty")
            .expect("open store");
        let store = Arc::new(store);
        let session_id = store.create_session("agent3", None).expect("create session");

        let manager = MemoryManagerAgent::new(store);
        let summary = manager.summarize_session(&session_id).expect("summarize");
        assert!(summary.is_empty(), "Empty session must produce empty summary");

        let injection = manager.context_injection(&session_id, 500).expect("injection");
        assert!(injection.is_empty(), "Empty session must produce empty injection");
    }

    #[test]
    fn test_memory_manager_scoped_context_filters_by_tags() {
        let store = SessionStore::open_with_prefix_in_memory("test_mem_scope")
            .expect("open store");
        let store = Arc::new(store);

        let session_id = store.create_session("agent2", None).expect("create session");
        store.append_message(&session_id, "user", "Write a Rust function").expect("append");
        store.append_message(&session_id, "assistant", "fn main() { println!(\"hello\"); }").expect("append");
        store.append_message(&session_id, "user", "Update the Python script").expect("append");
        store.append_message(&session_id, "assistant", "print('hello world')").expect("append");

        let manager = MemoryManagerAgent::new(store);
        let scoped = manager.build_scoped_context(&session_id, &["rust"], 1000).expect("scoped");
        assert!(scoped.contains("Rust"), "Must include Rust messages");
        assert!(!scoped.contains("Python"), "Must exclude Python messages");
    }
}
