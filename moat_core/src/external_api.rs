use crate::config::{
    deepseek_account_name, load_provider_profiles, nvidia_account_name, parse_user_document,
    user_md_path, DeepSeekProfileMeta, NvidiaProfileMeta, ProviderHealth, DEEPSEEK_KEY_ACCOUNT,
    KEYCHAIN_SERVICE_NAME, NVIDIA_KEY_ACCOUNT,
};
use crate::orchestrator::{BoxFuture, LlmMessage, LlmProvider, LlmResponse, LlmTool, LlmToolCall};
use crate::{MoatError, MoatResult};
use dashmap::DashMap;
use keyring::Entry;
use reqwest::Client;
use serde_json::{json, Value};
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[cfg(test)]
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ExternalApiConfig {
    pub nvidia_key_env: String,
    pub deepseek_key_env: String,
    pub user_md_path: PathBuf,
    pub default_provider: Arc<Mutex<String>>,
}

impl Default for ExternalApiConfig {
    fn default() -> Self {
        Self {
            nvidia_key_env: "NVIDIA_API_KEY".into(),
            deepseek_key_env: "DEEPSEEK_API_KEY".into(),
            user_md_path: user_md_path(),
            default_provider: Arc::new(Mutex::new("deepseek".to_string())),
        }
    }
}

pub trait ProviderFactory: Send + Sync {
    fn provider_for(&self, api_provider: &str) -> MoatResult<Arc<dyn LlmProvider>>;
    fn routed_provider(&self, subsystem: &str, requested_model: &str) -> MoatResult<(Arc<dyn LlmProvider>, String)>;
    fn invalidate_cache(&self);
    fn has_stored_key(&self, api_provider: &str) -> MoatResult<bool>;
    fn set_provider_key(&self, api_provider: &str, api_key: &str) -> MoatResult<()>;
    fn remove_provider_key(&self, api_provider: &str) -> MoatResult<()>;
    fn set_default_provider(&self, provider: &str);
    fn get_default_provider(&self) -> String;
    fn provider_health<'a>(
        &'a self,
        api_provider: &'a str,
    ) -> BoxFuture<'a, MoatResult<ProviderHealth>>;
    fn provider_variant_health<'a>(
        &'a self,
        api_provider: &'a str,
        variant: Option<&'a str>,
    ) -> BoxFuture<'a, MoatResult<ProviderHealth>> {
        let _ = variant;
        self.provider_health(api_provider)
    }
}

pub trait SecretStore: Send + Sync {
    fn get_secret(&self, account: &str) -> MoatResult<Option<String>>;
    fn set_secret(&self, account: &str, secret: &str) -> MoatResult<()>;
    fn delete_secret(&self, account: &str) -> MoatResult<()>;
}

#[derive(Default)]
pub struct KeyringSecretStore;

impl SecretStore for KeyringSecretStore {
    fn get_secret(&self, account: &str) -> MoatResult<Option<String>> {
        let entry = Entry::new(KEYCHAIN_SERVICE_NAME, account)
            .map_err(|err| MoatError::Settings(format!("failed to open keychain entry: {err}")))?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(MoatError::Settings(format!(
                "failed to read keychain secret for {account}: {err}"
            ))),
        }
    }

    fn set_secret(&self, account: &str, secret: &str) -> MoatResult<()> {
        let entry = Entry::new(KEYCHAIN_SERVICE_NAME, account)
            .map_err(|err| MoatError::Settings(format!("failed to open keychain entry: {err}")))?;
        entry.set_password(secret).map_err(|err| {
            MoatError::Settings(format!(
                "failed to save keychain secret for {account}: {err}"
            ))
        })
    }

    fn delete_secret(&self, account: &str) -> MoatResult<()> {
        let entry = Entry::new(KEYCHAIN_SERVICE_NAME, account)
            .map_err(|err| MoatError::Settings(format!("failed to open keychain entry: {err}")))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(MoatError::Settings(format!(
                "failed to delete keychain secret for {account}: {err}"
            ))),
        }
    }
}

pub struct ExternalProviderFactory {
    client: Client,
    config: ExternalApiConfig,
    cache: DashMap<String, Arc<dyn LlmProvider>>,
    secrets: Arc<dyn SecretStore>,
}

impl ExternalProviderFactory {
    pub fn new(config: ExternalApiConfig) -> Self {
        Self::with_secret_store(config, Arc::new(KeyringSecretStore))
    }

    pub fn with_secret_store(config: ExternalApiConfig, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            client: Client::new(),
            config,
            cache: DashMap::new(),
            secrets,
        }
    }

    fn resolve_for_subsystem(&self, subsystem: &str) -> MoatResult<Arc<dyn LlmProvider>> {
        let provider_name = self.config.default_provider.lock().unwrap().clone();
        eprintln!("[DEL] resolve_for_subsystem({}) → provider={}", subsystem, provider_name);
        self.provider_for(&provider_name).map_err(|err| {
            MoatError::Llm(format!(
                "no provider available for subsystem '{subsystem}': {err}"
            ))
        })
    }

    fn nvidia_env_key(&self) -> &str {
        self.config.nvidia_key_env.as_str()
    }

    fn deepseek_env_key(&self) -> &str {
        self.config.deepseek_key_env.as_str()
    }

    fn account_name_for_provider(&self, api_provider: &str) -> MoatResult<String> {
        let normalized = api_provider.trim().to_ascii_lowercase();
        if normalized == "nvidia" {
            return Ok(NVIDIA_KEY_ACCOUNT.to_string());
        }
        if let Some(profile_id) = normalized.strip_prefix("nvidia:") {
            return Ok(nvidia_account_name(profile_id));
        }
        if normalized == "deepseek" {
            return Ok(DEEPSEEK_KEY_ACCOUNT.to_string());
        }
        if let Some(profile_id) = normalized.strip_prefix("deepseek:") {
            return Ok(deepseek_account_name(profile_id));
        }
        Err(MoatError::Settings(format!(
            "unsupported provider '{api_provider}'; MoatAgeex supports nvidia and deepseek"
        )))
    }

    fn nvidia_profiles(&self) -> Vec<NvidiaProfileMeta> {
        load_provider_profiles()
            .map(|profiles| profiles.nvidia_profiles)
            .unwrap_or_else(|_| crate::config::default_provider_profiles().nvidia_profiles)
    }

    fn deepseek_profiles(&self) -> Vec<DeepSeekProfileMeta> {
        load_provider_profiles()
            .map(|profiles| profiles.deepseek_profiles)
            .unwrap_or_else(|_| crate::config::default_provider_profiles().deepseek_profiles)
    }

    fn read_nvidia_frontmatter_key(&self) -> MoatResult<Option<String>> {
        match std::fs::read_to_string(&self.config.user_md_path) {
            Ok(markdown) => {
                let doc = parse_user_document(&markdown)?;
                Ok(doc
                    .frontmatter
                    .nvidia_api_key
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(MoatError::Settings(format!(
                "failed to read USER.md for NVIDIA key resolution: {err}"
            ))),
        }
    }

    fn read_deepseek_frontmatter_key(&self) -> MoatResult<Option<String>> {
        match std::fs::read_to_string(&self.config.user_md_path) {
            Ok(markdown) => {
                let doc = parse_user_document(&markdown)?;
                Ok(doc
                    .frontmatter
                    .deepseek_api_key
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(MoatError::Settings(format!(
                "failed to read USER.md for DeepSeek key resolution: {err}"
            ))),
        }
    }

    fn read_nvidia_credentials(&self) -> MoatResult<Vec<NvidiaCredential>> {
        let mut credentials = Vec::new();

        if let Some(api_key) = self.read_nvidia_frontmatter_key()? {
            credentials.push(NvidiaCredential {
                profile_id: "user-md".to_string(),
                label: "USER.md NVIDIA".to_string(),
                model_hint: String::new(),
                api_key,
            });
            return Ok(credentials);
        }

        let env_key = self.nvidia_env_key();

        let env_secret = env::var(env_key)
            .map(|value| value.trim().to_string())
            .ok()
            .filter(|value| !value.is_empty());
        if let Some(api_key) = env_secret {
            credentials.push(NvidiaCredential {
                profile_id: "env".to_string(),
                label: "Environment NVIDIA".to_string(),
                model_hint: String::new(),
                api_key,
            });
            return Ok(credentials);
        }

        for profile in self.nvidia_profiles() {
            let account = nvidia_account_name(&profile.id);
            if let Some(secret) = self.secrets.get_secret(&account)? {
                let trimmed = secret.trim();
                if !trimmed.is_empty() {
                    credentials.push(NvidiaCredential {
                        profile_id: profile.id,
                        label: profile.label,
                        model_hint: profile.model_hint,
                        api_key: trimmed.to_string(),
                    });
                }
            }
        }

        if credentials.is_empty() {
            Err(missing_api_key_error("nvidia", env_key))
        } else {
            Ok(credentials)
        }
    }

    fn read_deepseek_credentials(&self) -> MoatResult<Vec<DeepSeekCredential>> {
        let mut credentials = Vec::new();

        if let Some(api_key) = self.read_deepseek_frontmatter_key()? {
            credentials.push(DeepSeekCredential {
                profile_id: "user-md".to_string(),
                label: "USER.md DeepSeek".to_string(),
                model_hint: String::new(),
                api_key,
            });
            return Ok(credentials);
        }

        let env_key = self.deepseek_env_key();
        let env_secret = env::var(env_key)
            .map(|value| value.trim().to_string())
            .ok()
            .filter(|value| !value.is_empty());
        if let Some(api_key) = env_secret {
            credentials.push(DeepSeekCredential {
                profile_id: "env".to_string(),
                label: "Environment DeepSeek".to_string(),
                model_hint: String::new(),
                api_key,
            });
            return Ok(credentials);
        }

        for profile in self.deepseek_profiles() {
            let account = deepseek_account_name(&profile.id);
            if let Some(secret) = self.secrets.get_secret(&account)? {
                let trimmed = secret.trim();
                if !trimmed.is_empty() {
                    credentials.push(DeepSeekCredential {
                        profile_id: profile.id,
                        label: profile.label,
                        model_hint: profile.model_hint,
                        api_key: trimmed.to_string(),
                    });
                }
            }
        }

        if credentials.is_empty() {
            Err(missing_api_key_error("deepseek", env_key))
        } else {
            Ok(credentials)
        }
    }

    async fn provider_health_for_variant(
        &self,
        api_provider: &str,
        variant: Option<&str>,
    ) -> MoatResult<ProviderHealth> {
        match api_provider.trim().to_ascii_lowercase().as_str() {
            "nvidia" => {
                if let Some(profile_id) = variant {
                    let account = nvidia_account_name(profile_id);
                    let Some(secret) = self.secrets.get_secret(&account)? else {
                        return Ok(ProviderHealth::Missing);
                    };
                    let trimmed = secret.trim();
                    if trimmed.is_empty() {
                        return Ok(ProviderHealth::Missing);
                    }
                    let provider = NvidiaExternalClient {
                        client: self.client.clone(),
                        api_key: trimmed.to_string(),
                    };
                    return match provider.check_health().await {
                        Ok(()) => Ok(ProviderHealth::Valid),
                        Err(_) => Ok(ProviderHealth::Invalid),
                    };
                }

                match self.provider_for("nvidia") {
                    Ok(provider) => match provider.check_health().await {
                        Ok(()) => Ok(ProviderHealth::Valid),
                        Err(_) => Ok(ProviderHealth::Invalid),
                    },
                    Err(err) => {
                        if missing_key_provider_from_error(&err).is_some() {
                            Ok(ProviderHealth::Missing)
                        } else {
                            Err(err)
                        }
                    }
                }
            }
            "deepseek" => {
                if let Some(profile_id) = variant {
                    let account = deepseek_account_name(profile_id);
                    let Some(secret) = self.secrets.get_secret(&account)? else {
                        return Ok(ProviderHealth::Missing);
                    };
                    let trimmed = secret.trim();
                    if trimmed.is_empty() {
                        return Ok(ProviderHealth::Missing);
                    }
                    let provider = DeepSeekExternalClient {
                        client: self.client.clone(),
                        api_key: trimmed.to_string(),
                    };
                    return match provider.check_health().await {
                        Ok(()) => Ok(ProviderHealth::Valid),
                        Err(_) => Ok(ProviderHealth::Invalid),
                    };
                }

                match self.provider_for("deepseek") {
                    Ok(provider) => match provider.check_health().await {
                        Ok(()) => Ok(ProviderHealth::Valid),
                        Err(_) => Ok(ProviderHealth::Invalid),
                    },
                    Err(err) => {
                        if missing_key_provider_from_error(&err).is_some() {
                            Ok(ProviderHealth::Missing)
                        } else {
                            Err(err)
                        }
                    }
                }
            }
            other => Err(MoatError::Llm(format!(
                "unsupported external provider '{other}'; MoatAgeex supports nvidia and deepseek"
            ))),
        }
    }
}

impl ProviderFactory for ExternalProviderFactory {
    fn provider_for(&self, api_provider: &str) -> MoatResult<Arc<dyn LlmProvider>> {
        let key = api_provider.trim().to_ascii_lowercase();
        if let Some(existing) = self.cache.get(&key) {
            return Ok(Arc::clone(existing.value()));
        }

        let provider: Arc<dyn LlmProvider> = match key.as_str() {
            "nvidia" => Arc::new(NvidiaMultiClient {
                client: self.client.clone(),
                credentials: self.read_nvidia_credentials()?,
            }),
            "deepseek" => Arc::new(DeepSeekMultiClient {
                client: self.client.clone(),
                credentials: self.read_deepseek_credentials()?,
            }),
            other => {
                return Err(MoatError::Llm(format!(
                    "unsupported external provider '{other}'; MoatAgeex supports nvidia and deepseek"
                )))
            }
        };

        self.cache.insert(key, Arc::clone(&provider));
        Ok(provider)
    }

    fn routed_provider(
        &self,
        subsystem: &str,
        requested_model: &str,
    ) -> MoatResult<(Arc<dyn LlmProvider>, String)> {
        let provider = self.resolve_for_subsystem(subsystem)?;
        Ok((provider, requested_model.to_string()))
    }

    fn invalidate_cache(&self) {
        self.cache.clear();
    }

    fn has_stored_key(&self, api_provider: &str) -> MoatResult<bool> {
        let account = self.account_name_for_provider(api_provider)?;
        Ok(self.secrets.get_secret(&account)?.is_some())
    }

    fn set_provider_key(&self, api_provider: &str, api_key: &str) -> MoatResult<()> {
        let account = self.account_name_for_provider(api_provider)?;
        let trimmed = api_key.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        self.secrets.set_secret(&account, trimmed)
    }

    fn remove_provider_key(&self, api_provider: &str) -> MoatResult<()> {
        let account = self.account_name_for_provider(api_provider)?;
        self.secrets.delete_secret(&account)
    }

    fn set_default_provider(&self, provider: &str) {
        eprintln!("[DEL] set_default_provider → {}", provider);
        *self.config.default_provider.lock().unwrap() = provider.to_string();
        self.invalidate_cache();
    }

    fn get_default_provider(&self) -> String {
        self.config.default_provider.lock().unwrap().clone()
    }

    fn provider_health<'a>(
        &'a self,
        api_provider: &'a str,
    ) -> BoxFuture<'a, MoatResult<ProviderHealth>> {
        Box::pin(async move { self.provider_health_for_variant(api_provider, None).await })
    }

    fn provider_variant_health<'a>(
        &'a self,
        api_provider: &'a str,
        variant: Option<&'a str>,
    ) -> BoxFuture<'a, MoatResult<ProviderHealth>> {
        Box::pin(async move {
            self.provider_health_for_variant(api_provider, variant)
                .await
        })
    }
}

const MISSING_API_KEY_PREFIX: &str = "missing_api_key::";

fn missing_api_key_error(api_provider: &str, env_key: &str) -> MoatError {
    MoatError::Llm(format!("{MISSING_API_KEY_PREFIX}{api_provider}::{env_key}"))
}

pub fn missing_key_provider_from_error(error: &MoatError) -> Option<String> {
    missing_key_provider_from_text(&error.to_string())
}

pub fn missing_key_provider_from_text(text: &str) -> Option<String> {
    let start = text.find(MISSING_API_KEY_PREFIX)? + MISSING_API_KEY_PREFIX.len();
    let rest = &text[start..];
    let provider = rest.split("::").next()?.trim();
    if provider.is_empty() {
        None
    } else {
        Some(provider.to_string())
    }
}

pub fn provider_display_name(provider: &str) -> &'static str {
    match provider.trim().to_ascii_lowercase().as_str() {
        "nvidia" => "NVIDIA",
        "deepseek" => "DeepSeek",
        _ => "External",
    }
}

#[derive(Clone)]
struct NvidiaCredential {
    profile_id: String,
    label: String,
    model_hint: String,
    api_key: String,
}

fn nvidia_model_match_score(requested_model: &str, model_hint: &str) -> u8 {
    let requested = requested_model.trim().to_ascii_lowercase();
    let hint = model_hint.trim().to_ascii_lowercase();
    if requested.is_empty() || hint.is_empty() {
        return 0;
    }
    if requested == hint {
        return 3;
    }
    if requested.contains(&hint) || hint.contains(&requested) {
        return 2;
    }
    let requested_slug = requested.replace(['/', ':'], "-");
    let hint_slug = hint.replace(['/', ':'], "-");
    if requested_slug.contains(&hint_slug) || hint_slug.contains(&requested_slug) {
        return 1;
    }
    0
}

#[derive(Clone)]
struct DeepSeekCredential {
    profile_id: String,
    label: String,
    model_hint: String,
    api_key: String,
}

fn deepseek_model_match_score(requested_model: &str, model_hint: &str) -> u8 {
    let requested = requested_model.trim().to_ascii_lowercase();
    let hint = model_hint.trim().to_ascii_lowercase();
    if requested.is_empty() || hint.is_empty() {
        return 0;
    }
    if requested == hint {
        return 3;
    }
    if requested.contains(&hint) || hint.contains(&requested) {
        return 2;
    }
    0
}

async fn read_json_response(response: reqwest::Response, provider: &str) -> MoatResult<Value> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| MoatError::Llm(format!("{provider} response read failed: {err}")))?;

    if status.as_u16() == 429 {
        return Err(MoatError::Llm(format!(
            "{provider} rate limit hit (429). Slow down or rotate the provider."
        )));
    }

    if !status.is_success() {
        return Err(MoatError::Llm(format!(
            "{provider} API error {}: {}",
            status.as_u16(),
            body.chars().take(256).collect::<String>()
        )));
    }

    serde_json::from_str(&body)
        .map_err(|err| MoatError::Llm(format!("{provider} JSON parse failed: {err}")))
}

async fn read_health_response(response: reqwest::Response, provider: &str) -> MoatResult<()> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| MoatError::Llm(format!("{provider} health check failed: {err}")))?;

    if !status.is_success() {
        return Err(MoatError::Llm(format!(
            "{provider} health check returned {}: {}",
            status.as_u16(),
            body.chars().take(128).collect::<String>()
        )));
    }

    Ok(())
}

fn extract_chat_completion_content(payload: &Value, provider: &str) -> MoatResult<(String, u32)> {
    let text = payload
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| MoatError::Llm(format!("{provider} response missing assistant content")))?
        .to_string();

    let tokens = payload
        .get("usage")
        .and_then(|usage| usage.get("total_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;

    Ok((text, tokens))
}

struct NvidiaExternalClient {
    client: Client,
    api_key: String,
}

const NVIDIA_INFERENCE_HEALTHCHECK_MODEL: &str = "meta/llama-3.1-8b-instruct";
const DEEPSEEK_INFERENCE_HEALTHCHECK_MODEL: &str = "deepseek-chat";

struct NvidiaMultiClient {
    client: Client,
    credentials: Vec<NvidiaCredential>,
}

impl LlmProvider for NvidiaMultiClient {
    fn complete<'a>(
        &'a self,
        model: &'a str,
        system: &'a str,
        user: &'a str,
        max_tokens: usize,
        temperature: f32,
    ) -> BoxFuture<'a, MoatResult<(String, u32)>> {
        self.complete_stream(model, system, user, max_tokens, temperature, None)
    }

    fn complete_stream<'a>(
        &'a self,
        model: &'a str,
        system: &'a str,
        user: &'a str,
        max_tokens: usize,
        temperature: f32,
        tx: Option<tokio::sync::mpsc::Sender<String>>,
    ) -> BoxFuture<'a, MoatResult<(String, u32)>> {
        Box::pin(async move {
            let mut errors = Vec::new();
            let mut ordered_credentials = self.credentials.clone();
            ordered_credentials.sort_by(|left, right| {
                let left_score = nvidia_model_match_score(model, &left.model_hint);
                let right_score = nvidia_model_match_score(model, &right.model_hint);
                right_score
                    .cmp(&left_score)
                    .then_with(|| left.label.cmp(&right.label))
            });

            for credential in &ordered_credentials {
                let client = NvidiaExternalClient {
                    client: self.client.clone(),
                    api_key: credential.api_key.clone(),
                };
                match client
                    .complete_stream(model, system, user, max_tokens, temperature, tx.clone())
                    .await
                {
                    Ok(result) => return Ok(result),
                    Err(err) => errors.push(format!(
                        "{} [{}] {}{}: {}",
                        credential.label,
                        credential.profile_id,
                        provider_display_name("nvidia"),
                        if credential.model_hint.trim().is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", credential.model_hint.trim())
                        },
                        err
                    )),
                }
            }

            Err(MoatError::Llm(format!(
                "No NVIDIA credential succeeded. Tried: {}",
                errors.join(" | ")
            )))
        })
    }

    fn check_health<'a>(&'a self) -> BoxFuture<'a, MoatResult<()>> {
        Box::pin(async move {
            for credential in &self.credentials {
                let client = NvidiaExternalClient {
                    client: self.client.clone(),
                    api_key: credential.api_key.clone(),
                };
                if client.check_health().await.is_ok() {
                    return Ok(());
                }
            }
            Err(MoatError::Llm(
                "all configured NVIDIA credentials failed health checks".into(),
            ))
        })
    }
    fn chat_complete<'a>(
        &'a self,
        model: &'a str,
        messages: Vec<LlmMessage>,
        tools: Option<Vec<LlmTool>>,
        max_tokens: usize,
        temperature: f32,
    ) -> BoxFuture<'a, MoatResult<LlmResponse>> {
        Box::pin(async move {
            let mut errors = Vec::new();
            let mut ordered_credentials = self.credentials.clone();
            ordered_credentials.sort_by(|left, right| {
                let left_score = nvidia_model_match_score(model, &left.model_hint);
                let right_score = nvidia_model_match_score(model, &right.model_hint);
                right_score
                    .cmp(&left_score)
                    .then_with(|| left.label.cmp(&right.label))
            });

            for credential in &ordered_credentials {
                let client = NvidiaExternalClient {
                    client: self.client.clone(),
                    api_key: credential.api_key.clone(),
                };
                match client
                    .chat_complete(
                        model,
                        messages.clone(),
                        tools.clone(),
                        max_tokens,
                        temperature,
                    )
                    .await
                {
                    Ok(result) => return Ok(result),
                    Err(err) => errors.push(format!(
                        "{} [{}] {}{}: {}",
                        credential.label,
                        credential.profile_id,
                        provider_display_name("nvidia"),
                        if credential.model_hint.trim().is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", credential.model_hint.trim())
                        },
                        err
                    )),
                }
            }

            Err(MoatError::Llm(format!(
                "No NVIDIA credential succeeded for chat_complete. Tried: {}",
                errors.join(" | ")
            )))
        })
    }
}

impl LlmProvider for NvidiaExternalClient {
    fn complete<'a>(
        &'a self,
        model: &'a str,
        system: &'a str,
        user: &'a str,
        max_tokens: usize,
        temperature: f32,
    ) -> crate::orchestrator::BoxFuture<'a, MoatResult<(String, u32)>> {
        self.complete_stream(model, system, user, max_tokens, temperature, None)
    }

    fn complete_stream<'a>(
        &'a self,
        model: &'a str,
        system: &'a str,
        user: &'a str,
        max_tokens: usize,
        temperature: f32,
        tx: Option<tokio::sync::mpsc::Sender<String>>,
    ) -> crate::orchestrator::BoxFuture<'a, MoatResult<(String, u32)>> {
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let model = model.to_string();
        let system = system.to_string();
        let user = user.to_string();

        Box::pin(async move {
            if let Some(channel) = tx {
                let response = client
                    .post("https://integrate.api.nvidia.com/v1/chat/completions")
                    .bearer_auth(&api_key)
                    .json(&json!({
                        "model": model,
                        "messages": [
                            {"role": "system", "content": system},
                            {"role": "user", "content": user}
                        ],
                        "max_tokens": max_tokens,
                        "temperature": temperature,
                        "stream": true,
                    }))
                    .send()
                    .await
                    .map_err(|err| MoatError::Llm(format!("nvidia request failed: {err}")))?;

                let status = response.status();
                if !status.is_success() {
                    let body = response.text().await.unwrap_or_default();
                    return Err(MoatError::Llm(format!(
                        "nvidia API error {}: {}",
                        status.as_u16(),
                        body.chars().take(256).collect::<String>()
                    )));
                }

                use futures_util::StreamExt;
                let mut stream = response.bytes_stream();
                let mut full_text = String::new();
                let mut buffer = String::new();

                while let Some(item) = stream.next().await {
                    let bytes = item.map_err(|e| MoatError::Llm(format!("stream error: {e}")))?;
                    let chunk_str = String::from_utf8_lossy(&bytes);
                    buffer.push_str(&chunk_str);

                    while let Some(newline_idx) = buffer.find('\n') {
                        let line = buffer[..newline_idx].trim().to_string();
                        buffer.drain(..=newline_idx);

                        if line.starts_with("data: ") {
                            let data = &line["data: ".len()..];
                            if data == "[DONE]" {
                                continue;
                            }
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                                if let Some(choices) =
                                    json.get("choices").and_then(|c| c.as_array())
                                {
                                    if let Some(delta) = choices.get(0).and_then(|c| c.get("delta"))
                                    {
                                        if let Some(content) =
                                            delta.get("content").and_then(|c| c.as_str())
                                        {
                                            full_text.push_str(content);
                                            let _ = channel
                                                .send(format!("__STREAM_CHUNK__:{}", content))
                                                .await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                let approx_tokens = (full_text.len() / 4) as u32;
                return Ok((full_text, approx_tokens));
            }

            // Fallback to non-streaming if no channel is provided
            let response = self
                .client
                .post("https://integrate.api.nvidia.com/v1/chat/completions")
                .bearer_auth(&self.api_key)
                .json(&json!({
                    "model": model,
                    "messages": [
                        {"role": "system", "content": system},
                        {"role": "user", "content": user}
                    ],
                    "max_tokens": max_tokens,
                    "temperature": temperature,
                }))
                .send()
                .await
                .map_err(|err| MoatError::Llm(format!("nvidia request failed: {err}")))?;
            let payload = read_json_response(response, "nvidia").await?;
            extract_chat_completion_content(&payload, "nvidia")
        })
    }

    fn chat_complete<'a>(
        &'a self,
        model: &'a str,
        messages: Vec<LlmMessage>,
        tools: Option<Vec<LlmTool>>,
        max_tokens: usize,
        temperature: f32,
    ) -> crate::orchestrator::BoxFuture<'a, MoatResult<LlmResponse>> {
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let model = model.to_string();

        Box::pin(async move {
            let mut payload = json!({
                "model": model,
                "messages": messages,
                "max_tokens": max_tokens,
                "temperature": temperature,
                "stream": false,
            });

            if let Some(t) = tools {
                if !t.is_empty() {
                    payload["tools"] = serde_json::to_value(t).unwrap();
                }
            }

            let response = client
                .post("https://integrate.api.nvidia.com/v1/chat/completions")
                .bearer_auth(&api_key)
                .timeout(std::time::Duration::from_secs(60))
                .json(&payload)
                .send()
                .await
                .map_err(|err| {
                    MoatError::Llm(format!("nvidia request failed or timed out: {err}"))
                })?;

            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(MoatError::Llm(format!(
                    "nvidia API error {}: {}",
                    status.as_u16(),
                    body.chars().take(256).collect::<String>()
                )));
            }

            let json: serde_json::Value = response
                .json()
                .await
                .map_err(|err| MoatError::Llm(format!("failed to parse nvidia response: {err}")))?;

            let choice = json
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|c| c.first())
                .ok_or_else(|| MoatError::Llm("nvidia response missing choices".into()))?;

            let message = choice
                .get("message")
                .ok_or_else(|| MoatError::Llm("nvidia response missing message".into()))?;

            let content = message
                .get("content")
                .and_then(serde_json::Value::as_str)
                .map(|s| s.to_string());

            let tool_calls: Option<Vec<LlmToolCall>> =
                if let Some(tc_val) = message.get("tool_calls") {
                    serde_json::from_value(tc_val.clone()).ok()
                } else {
                    None
                };

            let tokens = json
                .get("usage")
                .and_then(|usage| usage.get("total_tokens"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32;

            Ok(LlmResponse {
                content,
                tool_calls,
                tokens,
            })
        })
    }

    fn check_health<'a>(&'a self) -> BoxFuture<'a, MoatResult<()>> {
        Box::pin(async move {
            let response = self
                .client
                .post("https://integrate.api.nvidia.com/v1/chat/completions")
                .bearer_auth(&self.api_key)
                .json(&json!({
                    "model": NVIDIA_INFERENCE_HEALTHCHECK_MODEL,
                    "messages": [
                        {"role": "user", "content": "ping"}
                    ],
                    "max_tokens": 1,
                    "temperature": 0.0,
                }))
                .send()
                .await
                .map_err(|err| {
                    MoatError::Llm(format!("nvidia inference health request failed: {err}"))
                })?;
            read_health_response(response, "nvidia").await
        })
    }
}

// ─── DeepSeek Provider ────────────────────────────────────────────────

struct DeepSeekExternalClient {
    client: Client,
    api_key: String,
}

struct DeepSeekMultiClient {
    client: Client,
    credentials: Vec<DeepSeekCredential>,
}

impl LlmProvider for DeepSeekMultiClient {
    fn complete<'a>(
        &'a self,
        model: &'a str,
        system: &'a str,
        user: &'a str,
        max_tokens: usize,
        temperature: f32,
    ) -> crate::orchestrator::BoxFuture<'a, MoatResult<(String, u32)>> {
        self.complete_stream(model, system, user, max_tokens, temperature, None)
    }

    fn complete_stream<'a>(
        &'a self,
        model: &'a str,
        system: &'a str,
        user: &'a str,
        max_tokens: usize,
        temperature: f32,
        tx: Option<tokio::sync::mpsc::Sender<String>>,
    ) -> crate::orchestrator::BoxFuture<'a, MoatResult<(String, u32)>> {
        Box::pin(async move {
            let mut errors = Vec::new();
            let mut ordered_credentials = self.credentials.clone();
            ordered_credentials.sort_by(|left, right| {
                let left_score = deepseek_model_match_score(model, &left.model_hint);
                let right_score = deepseek_model_match_score(model, &right.model_hint);
                right_score
                    .cmp(&left_score)
                    .then_with(|| left.label.cmp(&right.label))
            });

            for credential in ordered_credentials {
                let client = DeepSeekExternalClient {
                    client: self.client.clone(),
                    api_key: credential.api_key.clone(),
                };
                match client.complete_stream(model, system, user, max_tokens, temperature, tx.clone()).await {
                    Ok(result) => return Ok(result),
                    Err(err) => {
                        errors.push(format!(
                            "{}{}: {err}",
                            credential.label,
                            if credential.model_hint.trim().is_empty() {
                                String::new()
                            } else {
                                format!(" ({})", credential.model_hint.trim())
                            },
                        ));
                    }
                }
            }

            Err(MoatError::Llm(format!(
                "No DeepSeek credential succeeded for complete_stream. Tried: {}",
                errors.join(" | ")
            )))
        })
    }

    fn chat_complete<'a>(
        &'a self,
        model: &'a str,
        messages: Vec<LlmMessage>,
        tools: Option<Vec<LlmTool>>,
        max_tokens: usize,
        temperature: f32,
    ) -> crate::orchestrator::BoxFuture<'a, MoatResult<LlmResponse>> {
        Box::pin(async move {
            let mut errors = Vec::new();
            let mut ordered_credentials = self.credentials.clone();
            ordered_credentials.sort_by(|left, right| {
                let left_score = deepseek_model_match_score(model, &left.model_hint);
                let right_score = deepseek_model_match_score(model, &right.model_hint);
                right_score
                    .cmp(&left_score)
                    .then_with(|| left.label.cmp(&right.label))
            });

            for credential in ordered_credentials {
                let client = DeepSeekExternalClient {
                    client: self.client.clone(),
                    api_key: credential.api_key.clone(),
                };
                match client.chat_complete(model, messages.clone(), tools.clone(), max_tokens, temperature).await {
                    Ok(result) => return Ok(result),
                    Err(err) => {
                        errors.push(format!(
                            "{}{}: {err}",
                            credential.label,
                            if credential.model_hint.trim().is_empty() {
                                String::new()
                            } else {
                                format!(" ({})", credential.model_hint.trim())
                            },
                        ));
                    }
                }
            }

            Err(MoatError::Llm(format!(
                "No DeepSeek credential succeeded for chat_complete. Tried: {}",
                errors.join(" | ")
            )))
        })
    }
}

impl LlmProvider for DeepSeekExternalClient {
    fn complete<'a>(
        &'a self,
        model: &'a str,
        system: &'a str,
        user: &'a str,
        max_tokens: usize,
        temperature: f32,
    ) -> crate::orchestrator::BoxFuture<'a, MoatResult<(String, u32)>> {
        self.complete_stream(model, system, user, max_tokens, temperature, None)
    }

    fn complete_stream<'a>(
        &'a self,
        model: &'a str,
        system: &'a str,
        user: &'a str,
        max_tokens: usize,
        temperature: f32,
        tx: Option<tokio::sync::mpsc::Sender<String>>,
    ) -> crate::orchestrator::BoxFuture<'a, MoatResult<(String, u32)>> {
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let model = model.to_string();
        let system = system.to_string();
        let user = user.to_string();

        Box::pin(async move {
            if let Some(channel) = tx {
                let response = client
                    .post("https://api.deepseek.com/chat/completions")
                    .bearer_auth(&api_key)
                    .json(&json!({
                        "model": model,
                        "messages": [
                            {"role": "system", "content": system},
                            {"role": "user", "content": user}
                        ],
                        "max_tokens": max_tokens,
                        "temperature": temperature,
                        "stream": true,
                    }))
                    .send()
                    .await
                    .map_err(|err| MoatError::Llm(format!("deepseek request failed: {err}")))?;

                let status = response.status();
                if !status.is_success() {
                    let body = response.text().await.unwrap_or_default();
                    return Err(MoatError::Llm(format!(
                        "deepseek API error {}: {}",
                        status.as_u16(),
                        body.chars().take(256).collect::<String>()
                    )));
                }

                use futures_util::StreamExt;
                let mut stream = response.bytes_stream();
                let mut full_text = String::new();
                let mut buffer = String::new();

                while let Some(item) = stream.next().await {
                    let bytes = item.map_err(|e| MoatError::Llm(format!("deepseek stream error: {e}")))?;
                    let chunk_str = String::from_utf8_lossy(&bytes);
                    buffer.push_str(&chunk_str);

                    while let Some(newline_idx) = buffer.find('\n') {
                        let line = buffer[..newline_idx].trim().to_string();
                        buffer.drain(..=newline_idx);

                        if line.starts_with("data: ") {
                            let data = &line["data: ".len()..];
                            if data == "[DONE]" {
                                continue;
                            }
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                                if let Some(choices) =
                                    json.get("choices").and_then(|c| c.as_array())
                                {
                                    if let Some(delta) = choices.get(0).and_then(|c| c.get("delta"))
                                    {
                                        if let Some(content) =
                                            delta.get("content").and_then(|c| c.as_str())
                                        {
                                            full_text.push_str(content);
                                            let _ = channel
                                                .send(format!("__STREAM_CHUNK__:{}", content))
                                                .await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                let approx_tokens = (full_text.len() / 4) as u32;
                return Ok((full_text, approx_tokens));
            }

            let response = self
                .client
                .post("https://api.deepseek.com/chat/completions")
                .bearer_auth(&api_key)
                .json(&json!({
                    "model": model,
                    "messages": [
                        {"role": "system", "content": system},
                        {"role": "user", "content": user}
                    ],
                    "max_tokens": max_tokens,
                    "temperature": temperature,
                    "stream": false,
                }))
                .send()
                .await
                .map_err(|err| MoatError::Llm(format!("deepseek request failed: {err}")))?;

            let payload = response
                .json::<serde_json::Value>()
                .await
                .map_err(|err| MoatError::Llm(format!("deepseek JSON parse: {err}")))?;

            let (text, tokens) = extract_chat_completion_content(&payload, "deepseek")?;
            let text = text.replace("__", ":");
            Ok((text, tokens))
        })
    }

    fn chat_complete<'a>(
        &'a self,
        model: &'a str,
        messages: Vec<LlmMessage>,
        tools: Option<Vec<LlmTool>>,
        max_tokens: usize,
        temperature: f32,
    ) -> crate::orchestrator::BoxFuture<'a, MoatResult<LlmResponse>> {
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let model = model.to_string();

        Box::pin(async move {
            let mut payload = json!({
                "model": model,
                "messages": messages,
                "max_tokens": max_tokens,
                "temperature": temperature,
                "stream": false,
            });

            if let Some(mut t) = tools {
                if !t.is_empty() {
                    for tool in &mut t {
                        tool.function.name = tool.function.name.replace(':', "__");
                    }
                    payload["tools"] = serde_json::to_value(&t).unwrap();
                }
            }

            let response = client
                .post("https://api.deepseek.com/chat/completions")
                .bearer_auth(&api_key)
                .timeout(std::time::Duration::from_secs(60))
                .json(&payload)
                .send()
                .await
                .map_err(|err| {
                    MoatError::Llm(format!("deepseek request failed or timed out: {err}"))
                })?;

            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(MoatError::Llm(format!(
                    "deepseek API error {}: {}",
                    status.as_u16(),
                    body.chars().take(256).collect::<String>()
                )));
            }

            let json: serde_json::Value = response
                .json()
                .await
                .map_err(|err| MoatError::Llm(format!("failed to parse deepseek response: {err}")))?;

            let choice = json
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|c| c.first())
                .ok_or_else(|| MoatError::Llm("deepseek response missing choices".into()))?;

            let message = choice
                .get("message")
                .ok_or_else(|| MoatError::Llm("deepseek response missing message".into()))?;

            let mut content = message
                .get("content")
                .and_then(serde_json::Value::as_str)
                .map(|s| s.to_string());
            if let Some(ref c) = content {
                if c.contains("__") {
                    content = Some(c.replace("__", ":"));
                }
            }

            let mut tool_calls: Option<Vec<LlmToolCall>> =
                if let Some(tc_val) = message.get("tool_calls") {
                    serde_json::from_value(tc_val.clone()).ok()
                } else {
                    None
                };
            if let Some(ref mut calls) = tool_calls {
                for call in calls {
                    call.function.name = call.function.name.replace("__", ":");
                }
            }

            let tokens = json
                .get("usage")
                .and_then(|usage| usage.get("total_tokens"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32;

            Ok(LlmResponse {
                content,
                tool_calls,
                tokens,
            })
        })
    }

    fn check_health<'a>(&'a self) -> BoxFuture<'a, MoatResult<()>> {
        Box::pin(async move {
            let response = self
                .client
                .post("https://api.deepseek.com/chat/completions")
                .bearer_auth(&self.api_key)
                .json(&json!({
                    "model": DEEPSEEK_INFERENCE_HEALTHCHECK_MODEL,
                    "messages": [
                        {"role": "user", "content": "ping"}
                    ],
                    "max_tokens": 1,
                    "temperature": 0.0,
                }))
                .send()
                .await
                .map_err(|err| {
                    MoatError::Llm(format!("deepseek inference health request failed: {err}"))
                })?;
            read_health_response(response, "deepseek").await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[derive(Default)]
    struct InMemorySecretStore {
        secrets: Mutex<HashMap<String, String>>,
    }

    impl SecretStore for InMemorySecretStore {
        fn get_secret(&self, account: &str) -> MoatResult<Option<String>> {
            Ok(self
                .secrets
                .lock()
                .expect("lock secrets")
                .get(account)
                .cloned())
        }

        fn set_secret(&self, account: &str, secret: &str) -> MoatResult<()> {
            self.secrets
                .lock()
                .expect("lock secrets")
                .insert(account.to_string(), secret.to_string());
            Ok(())
        }

        fn delete_secret(&self, account: &str) -> MoatResult<()> {
            self.secrets.lock().expect("lock secrets").remove(account);
            Ok(())
        }
    }

    #[test]
    fn provider_factory_prefers_user_md_then_env_then_secret_store_for_nvidia() {
        let store = Arc::new(InMemorySecretStore::default());
        store
            .set_secret(NVIDIA_KEY_ACCOUNT, "stored-nvidia")
            .expect("store secret");
        let user_md_path = std::env::temp_dir().join(format!(
            "moat_external_api_test_{}_USER.md",
            uuid::Uuid::new_v4()
        ));
        fs::write(
            &user_md_path,
            "---\nname: \"User\"\ncore_preferences: \"\"\nchat_model: null\nplanner_model: null\nworker_model: null\nnvidia_api_key: frontmatter-nvidia\n---\n",
        )
        .expect("write USER.md");
        unsafe {
            std::env::set_var("NVIDIA_API_KEY", "env-nvidia");
        }

        let factory = ExternalProviderFactory::with_secret_store(
            ExternalApiConfig {
                user_md_path: user_md_path.clone(),
                ..ExternalApiConfig::default()
            },
            store,
        );
        let credentials = factory.read_nvidia_credentials().expect("read credentials");
        assert_eq!(credentials.len(), 1);
        assert_eq!(credentials[0].profile_id, "user-md");
        assert_eq!(credentials[0].api_key, "frontmatter-nvidia");

        let _ = fs::remove_file(user_md_path);
        unsafe {
            std::env::remove_var("NVIDIA_API_KEY");
        }
    }

    #[test]
    fn provider_factory_can_set_and_remove_nvidia_keys() {
        let store = Arc::new(InMemorySecretStore::default());
        let factory =
            ExternalProviderFactory::with_secret_store(ExternalApiConfig::default(), store);

        assert!(!factory.has_stored_key("nvidia").expect("missing key"));
        factory
            .set_provider_key("nvidia", "secret-1")
            .expect("save key");
        assert!(factory.has_stored_key("nvidia").expect("saved key"));
        factory.remove_provider_key("nvidia").expect("remove key");
        assert!(!factory.has_stored_key("nvidia").expect("removed key"));
    }
}
