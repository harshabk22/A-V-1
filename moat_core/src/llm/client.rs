use super::types::{ChatRequest, ChatResponse, LlmMessage, LlmProvider, ToolDefinition};
use anyhow::{Context, Result, bail};
use reqwest::Client;

pub struct LlmClient {
    provider: LlmProvider,
    model: String,
    api_key: String,
    http: Client,
}

impl LlmClient {
    pub fn from_env() -> Result<Self> {
        let provider = LlmProvider::from_env();
        let api_key = std::env::var(provider.api_key_env_var())
            .with_context(|| format!(
                "Missing API key. Set {} environment variable.",
                provider.api_key_env_var()
            ))?;

        let model = std::env::var("AGEEX_MODEL")
            .unwrap_or_else(|_| provider.default_model().to_string());

        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()?;

        let api_key = api_key.strip_prefix("Bearer ").unwrap_or(&api_key).to_string();

        Ok(Self { provider, model, api_key, http })
    }

    /// Single chat completion call (non-streaming)
    pub async fn chat(
        &self,
        messages: Vec<LlmMessage>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<ChatResponse> {
        let url = format!("{}/chat/completions", self.provider.base_url());

        let request = ChatRequest {
            model: self.model.clone(),
            messages,
            tools,
            temperature: 0.2,
            max_tokens: 4096,
        };

        let response = self.http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to LLM provider")?;

        let status = response.status();

        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            bail!("LLM API error {}: {}", status, error_body);
        }

        let parsed: ChatResponse = response.json().await
            .context("Failed to parse LLM response")?;

        if parsed.choices.is_empty() {
            bail!("LLM returned empty choices array");
        }

        Ok(parsed)
    }

    /// Minimal single-shot completion for LLM router fallback
    /// No tools, no system prompt — just the prompt, max 10 tokens
    pub async fn complete_simple(&self, prompt: &str) -> Result<String> {
        let url = format!("{}/chat/completions", self.provider.base_url());

        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![LlmMessage::user(prompt)],
            tools: None,
            temperature: 0.0,
            max_tokens: 10,
        };

        let response = self.http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to LLM provider for simple completion")?;

        let status = response.status();

        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            bail!("LLM API error {}: {}", status, error_body);
        }

        let parsed: ChatResponse = response.json().await
            .context("Failed to parse LLM simple completion response")?;

        if parsed.choices.is_empty() {
            bail!("LLM returned empty choices array");
        }

        Ok(parsed.choices[0].message.content.clone().unwrap_or_default())
    }
}