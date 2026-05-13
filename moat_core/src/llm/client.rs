use super::types::{ChatRequest, ChatResponse, LlmMessage, LlmProvider, ToolDefinition};
use anyhow::{Context, Result, bail};
use reqwest::Client;
use std::time::Duration;

const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 500;

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

    fn is_retryable(status: u16) -> bool {
        matches!(status, 429 | 502 | 503 | 504)
    }

    async fn send_with_retry(&self, url: &str, request: &ChatRequest) -> Result<ChatResponse> {
        let mut last_error = String::new();

        for attempt in 0..=MAX_RETRIES {
            let response = self.http
                .post(url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(request)
                .send()
                .await;

            match response {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        let parsed: ChatResponse = resp.json().await
                            .context("Failed to parse LLM response")?;
                        if parsed.choices.is_empty() {
                            bail!("LLM returned empty choices array");
                        }
                        return Ok(parsed);
                    }

                    let status_code = status.as_u16();
                    let error_body = resp.text().await.unwrap_or_default();
                    last_error = format!("LLM API error {}: {}", status_code, error_body);

                    if !Self::is_retryable(status_code) || attempt == MAX_RETRIES {
                        bail!("{}", last_error);
                    }
                }
                Err(e) => {
                    last_error = format!("{}", e);

                    if attempt == MAX_RETRIES {
                        bail!("Failed to send request to LLM provider after {} retries: {}", MAX_RETRIES, last_error);
                    }
                }
            }

            if attempt < MAX_RETRIES {
                let delay = Duration::from_millis(INITIAL_BACKOFF_MS * 2u64.pow(attempt));
                eprintln!("[LlmClient] Retry {}/{} in {:?}: {}", attempt + 1, MAX_RETRIES, delay, last_error);
                tokio::time::sleep(delay).await;
            }
        }

        Err(anyhow::anyhow!("{}", last_error))
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

        self.send_with_retry(&url, &request).await
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

        let parsed = self.send_with_retry(&url, &request).await?;

        Ok(parsed.choices[0].message.content.clone().unwrap_or_default())
    }
}