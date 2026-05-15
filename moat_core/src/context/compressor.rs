use super::{Message, Role, SummaryTemplate, TokenBudget};
use anyhow::Result;

pub struct ContextCompressor;

impl ContextCompressor {
    pub fn precompress_tool_outputs(messages: &mut Vec<Message>) {
        for msg in messages.iter_mut() {
            if msg.role == Role::Tool {
                let original_len = msg.content.len();

                if original_len > 500 {
                    let lines = msg.content.lines().count();
                    msg.content = format!(
                        "[Tool output compressed: {} lines, {} bytes]",
                        lines,
                        original_len
                    );
                }
            }
        }
    }

    pub fn compress_tiered(messages: Vec<Message>) -> Result<Vec<Message>> {
        let total = messages.len();

        if total <= 10 {
            return Ok(messages);
        }

        let head_size = 2;
        let tail_size = (total as f64 * 0.20) as usize;

        let compressible_start = head_size;
        let compressible_end = total.saturating_sub(tail_size);

        if compressible_end <= compressible_start {
            return Ok(messages);
        }

        let mut compressed = Vec::new();

        compressed.extend_from_slice(&messages[0..head_size]);

        let middle_messages = &messages[compressible_start..compressible_end];
        let summary = Self::build_summary_from_messages(middle_messages);
        compressed.push(Message::system(summary.to_message_content()));

        compressed.extend_from_slice(&messages[compressible_end..]);

        Ok(compressed)
    }

    fn build_summary_from_messages(messages: &[Message]) -> SummaryTemplate {
        let mut completed = Vec::new();
        let mut pending = Vec::new();

        for msg in messages.iter() {
            if msg.role == Role::Tool {
                completed.push(format!("Executed tool operation ({}... chars)",
                    msg.content.len().min(50)));
            } else if msg.role == Role::User {
                pending.push(format!("User query: {}...",
                    msg.content.chars().take(50).collect::<String>()));
            }
        }

        SummaryTemplate {
            active_task: "Continuing from previous context".into(),
            completed_actions: completed,
            current_state: format!("{} messages compressed", messages.len()),
            pending_items: pending,
            blockers: vec![],
        }
    }

    pub fn compression_saved_enough(
        original_token_count: usize,
        compressed_token_count: usize,
        min_savings_ratio: f64,
    ) -> bool {
        let saved = original_token_count.saturating_sub(compressed_token_count);
        let savings_ratio = saved as f64 / original_token_count as f64;
        savings_ratio >= min_savings_ratio
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_precompress_large_tool_output() {
        let mut messages = vec![
            Message::tool("x".repeat(1000)),
        ];

        ContextCompressor::precompress_tool_outputs(&mut messages);

        assert!(messages[0].content.contains("compressed"));
        assert!(messages[0].content.len() < 200);
    }

    #[test]
    fn test_tiered_compression_protects_head_tail() {
        let messages: Vec<Message> = (0..20)
            .map(|i| {
                if i == 0 {
                    Message::system("System prompt")
                } else if i % 2 == 0 {
                    Message::user(format!("Query {}", i))
                } else {
                    Message::assistant(format!("Response {}", i))
                }
            })
            .collect();

        let compressed = ContextCompressor::compress_tiered(messages.clone()).unwrap();

        assert_eq!(compressed[0].role, Role::System);
        assert_eq!(compressed[0].content, "<|SYSTEM_START|>\nSystem prompt\n<|SYSTEM_END|>");

        assert!(compressed.iter().any(|m| m.content.contains("CONTEXT SUMMARY")));

        let last_original = messages.last().unwrap();
        let last_compressed = compressed.last().unwrap();
        assert_eq!(last_compressed.content, last_original.content);
    }
}