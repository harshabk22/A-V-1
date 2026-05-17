use crate::{MoatError, MoatResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const KEYCHAIN_SERVICE_NAME: &str = "com.moatageex.vault";
pub const NVIDIA_KEY_ACCOUNT: &str = "nvidia_api_key";
pub const DEEPSEEK_KEY_ACCOUNT: &str = "deepseek_api_key";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderHealth {
    Missing,
    Valid,
    Invalid,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserFrontmatter {
    pub name: String,
    pub core_preferences: String,
    #[serde(default)]
    pub chat_model: Option<String>,
    #[serde(default)]
    pub planner_model: Option<String>,
    #[serde(default)]
    pub worker_model: Option<String>,
    #[serde(default)]
    pub nvidia_api_key: Option<String>,
    #[serde(default)]
    pub deepseek_api_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserDocument {
    pub frontmatter: UserFrontmatter,
    pub body: String,
    pub raw_markdown: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SettingsState {
    pub has_nvidia: bool,
    pub has_deepseek: bool,
    pub chat_model: Option<String>,
    pub planner_model: Option<String>,
    pub worker_model: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SettingsUpdate {
    #[serde(default)]
    pub chat_model: Option<String>,
    #[serde(default)]
    pub planner_model: Option<String>,
    #[serde(default)]
    pub worker_model: Option<String>,
    #[serde(default)]
    pub nvidia_api_key: Option<String>,
    #[serde(default)]
    pub deepseek_api_key: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderProfiles {
    #[serde(default)]
    pub nvidia_profiles: Vec<NvidiaProfileMeta>,
    #[serde(default)]
    pub deepseek_profiles: Vec<DeepSeekProfileMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NvidiaProfileMeta {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub model_hint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeepSeekProfileMeta {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub model_hint: String,
}

pub fn moat_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".moatageex")
}

pub fn swarm_root() -> PathBuf {
    moat_root().join("swarm")
}

pub fn workspace_root() -> PathBuf {
    moat_root().join("workspace")
}

pub fn skills_root() -> PathBuf {
    moat_root().join("skills")
}

pub fn plugins_root() -> PathBuf {
    moat_root().join("plugins")
}

pub fn sqlite_db_path() -> PathBuf {
    moat_root().join("moat.db")
}

pub fn meta_brain_bin_path() -> PathBuf {
    moat_root().join("moat.bin")
}

pub fn provider_profiles_path() -> PathBuf {
    moat_root().join("provider_profiles.yaml")
}

pub fn user_md_path() -> PathBuf {
    swarm_root().join("USER.md")
}

pub fn ensure_moat_root() -> MoatResult<PathBuf> {
    let root = moat_root();
    std::fs::create_dir_all(&root)
        .map_err(|err| MoatError::Settings(format!("failed to create moat root: {err}")))?;
    Ok(root)
}

pub fn nvidia_account_name(profile_id: &str) -> String {
    let normalized = profile_id.trim();
    if normalized.is_empty() || normalized == "default" {
        NVIDIA_KEY_ACCOUNT.to_string()
    } else {
        format!("{NVIDIA_KEY_ACCOUNT}::{normalized}")
    }
}

pub fn deepseek_account_name(profile_id: &str) -> String {
    let normalized = profile_id.trim();
    if normalized.is_empty() || normalized == "default" {
        DEEPSEEK_KEY_ACCOUNT.to_string()
    } else {
        format!("{DEEPSEEK_KEY_ACCOUNT}::{normalized}")
    }
}

pub fn default_provider_profiles() -> ProviderProfiles {
    ProviderProfiles {
        nvidia_profiles: vec![NvidiaProfileMeta {
            id: "default".to_string(),
            label: "Default NVIDIA".to_string(),
            model_hint: String::new(),
        }],
        deepseek_profiles: vec![DeepSeekProfileMeta {
            id: "default".to_string(),
            label: "Default DeepSeek".to_string(),
            model_hint: String::new(),
        }],
    }
}

fn normalize_provider_profiles(mut profiles: ProviderProfiles) -> ProviderProfiles {
    let mut seen = std::collections::BTreeSet::new();
    profiles.nvidia_profiles.retain(|profile| {
        let id = profile.id.trim();
        let label = profile.label.trim();
        !id.is_empty() && !label.is_empty() && seen.insert(id.to_string())
    });
    if profiles.nvidia_profiles.is_empty() {
        profiles.nvidia_profiles = default_provider_profiles().nvidia_profiles;
    }
    seen.clear();
    profiles.deepseek_profiles.retain(|profile| {
        let id = profile.id.trim();
        let label = profile.label.trim();
        !id.is_empty() && !label.is_empty() && seen.insert(id.to_string())
    });
    if profiles.deepseek_profiles.is_empty() {
        profiles.deepseek_profiles = default_provider_profiles().deepseek_profiles;
    }
    profiles
}

pub fn load_provider_profiles() -> MoatResult<ProviderProfiles> {
    let path = provider_profiles_path();
    match std::fs::read_to_string(&path) {
        Ok(raw) => {
            let parsed = serde_yaml::from_str::<ProviderProfiles>(&raw)
                .unwrap_or_else(|_| default_provider_profiles());
            Ok(normalize_provider_profiles(parsed))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(default_provider_profiles()),
        Err(err) => Err(MoatError::Settings(format!(
            "failed to read provider profiles: {err}"
        ))),
    }
}

pub fn save_provider_profiles(profiles: &ProviderProfiles) -> MoatResult<()> {
    ensure_moat_root()?;
    let normalized = normalize_provider_profiles(profiles.clone());
    let rendered = serde_yaml::to_string(&normalized).map_err(|err| {
        MoatError::Settings(format!("failed to serialize provider profiles: {err}"))
    })?;
    std::fs::write(provider_profiles_path(), rendered)
        .map_err(|err| MoatError::Settings(format!("failed to write provider profiles: {err}")))?;
    Ok(())
}

pub fn default_user_document() -> UserDocument {
    let body = "[User can write freeform notes here]\n".to_string();
    let frontmatter = UserFrontmatter {
        name: "User".to_string(),
        core_preferences: String::new(),
        chat_model: None,
        planner_model: None,
        worker_model: None,
        nvidia_api_key: None,
        deepseek_api_key: None,
    };
    let raw_markdown = render_user_document(&frontmatter, &body).unwrap_or_else(|_| {
        "---\nname: \"User\"\ncore_preferences: \"\"\nchat_model: null\nplanner_model: null\nworker_model: null\nnvidia_api_key: null\n---\n[User can write freeform notes here]\n".to_string()
    });
    UserDocument {
        frontmatter,
        body,
        raw_markdown,
    }
}

pub fn parse_user_document(markdown: &str) -> MoatResult<UserDocument> {
    let trimmed = markdown.trim_matches('\u{feff}');
    if let Some((yaml, body)) = split_frontmatter(trimmed) {
        match serde_yaml::from_str::<UserFrontmatter>(&yaml) {
            Ok(frontmatter) => {
                let raw_markdown = render_user_document(&frontmatter, body)?;
                Ok(UserDocument {
                    frontmatter,
                    body: body.to_string(),
                    raw_markdown,
                })
            }
            Err(_) => {
                let mut doc = default_user_document();
                doc.body = trimmed.to_string();
                doc.raw_markdown = render_user_document(&doc.frontmatter, &doc.body)?;
                Ok(doc)
            }
        }
    } else {
        let mut doc = default_user_document();
        doc.body = if trimmed.is_empty() {
            doc.body.clone()
        } else {
            trimmed.to_string()
        };
        doc.raw_markdown = render_user_document(&doc.frontmatter, &doc.body)?;
        Ok(doc)
    }
}

pub fn render_user_document(frontmatter: &UserFrontmatter, body: &str) -> MoatResult<String> {
    let yaml = serde_yaml::to_string(frontmatter).map_err(|err| {
        MoatError::Settings(format!("failed to serialize USER.md frontmatter: {err}"))
    })?;
    let body = if body.is_empty() {
        String::new()
    } else {
        body.to_string()
    };
    Ok(format!("---\n{}---\n{}", yaml, body))
}

pub async fn load_user_document(path: impl AsRef<Path>) -> MoatResult<UserDocument> {
    let markdown = tokio::fs::read_to_string(path.as_ref())
        .await
        .map_err(|err| MoatError::Settings(format!("failed to read USER.md: {err}")))?;
    parse_user_document(&markdown)
}

pub async fn save_user_frontmatter(
    path: impl AsRef<Path>,
    frontmatter: UserFrontmatter,
) -> MoatResult<()> {
    let path = path.as_ref();
    let existing = match tokio::fs::read_to_string(path).await {
        Ok(markdown) => parse_user_document(&markdown)?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => default_user_document(),
        Err(err) => {
            return Err(MoatError::Settings(format!(
                "failed to read USER.md for update: {err}"
            )))
        }
    };

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| MoatError::Settings(format!("failed to create settings root: {err}")))?;
    }

    let rendered = render_user_document(&frontmatter, &existing.body)?;
    tokio::fs::write(path, rendered)
        .await
        .map_err(|err| MoatError::Settings(format!("failed to write USER.md: {err}")))?;
    Ok(())
}

fn split_frontmatter(markdown: &str) -> Option<(String, &str)> {
    let rest = markdown.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let yaml = rest[..end].to_string();
    let body = &rest[end + 5..];
    Some((yaml, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_render_user_document_preserves_body() {
        let source =
            "---\nname: Harsha\ncore_preferences: concise\n---\nFreeform notes\n- keep this\n";
        let doc = parse_user_document(source).expect("parse user document");
        assert_eq!(doc.frontmatter.name, "Harsha");
        assert_eq!(doc.frontmatter.core_preferences, "concise");
        assert_eq!(doc.body, "Freeform notes\n- keep this\n");

        let rendered = render_user_document(&doc.frontmatter, &doc.body).expect("render");
        assert!(rendered.contains("name: Harsha"));
        assert!(rendered.ends_with("Freeform notes\n- keep this\n"));
    }

    #[test]
    fn malformed_frontmatter_repairs_safely() {
        let source = "---\nname: [oops\n---\nnotes\n";
        let doc = parse_user_document(source).expect("repair malformed frontmatter");
        assert_eq!(doc.frontmatter.name, "User");
        assert!(doc.body.contains("[oops"));
    }
}
