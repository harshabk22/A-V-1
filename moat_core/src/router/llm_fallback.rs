use super::decision::{RouteDecision, RouteTarget, RouteMethod};
use crate::llm::LlmClient;
use anyhow::Result;

pub struct LlmRouter {
    client: LlmClient,
}

impl LlmRouter {
    pub fn new(client: LlmClient) -> Self {
        Self { client }
    }

    pub async fn classify(&self, query: &str, context_hint: Option<&str>) -> Result<RouteDecision> {
        let context_block = context_hint
            .map(|c| format!("\nRecent context:\n{}", &c[..c.len().min(300)]))
            .unwrap_or_default();

        let prompt = format!(
            r#"Classify this user query. Respond with EXACTLY one word: READER or EXECUTOR.

READER = read files, explain code, answer questions, search, view logs
EXECUTOR = write files, run commands, edit code, install packages, create/modify anything

{context_block}

Query: {query}

Classification:"#
        );

        let response = self.client.complete_simple(&prompt).await?;
        let classification = response.trim().to_uppercase();

        let (target, confidence) = if classification.starts_with("EXECUTOR") {
            (RouteTarget::Agent2, 0.82f32)
        } else {
            (RouteTarget::Agent1, 0.82f32)
        };

        Ok(RouteDecision {
            target,
            confidence,
            method: RouteMethod::LlmFallback,
            reason: format!("LLM classified as {}", classification),
        })
    }
}