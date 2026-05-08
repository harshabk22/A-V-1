use crate::sdk::RuntimeHook;
use crate::{config, MoatError, MoatResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

const MANIFEST_FILENAMES: &[&str] = &[
    "moat.plugin.json",
    "moat.plugin.yaml",
    "moat.plugin.yml",
    "openclaw.plugin.json",
    "plugin.json",
    "plugin.yaml",
    "plugin.yml",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub enabled_by_default: bool,
    #[serde(default)]
    pub hooks: Vec<RuntimeHook>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub entrypoint: Option<String>,
    #[serde(default)]
    pub config_schema: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LoadedPluginManifest {
    pub manifest: PluginManifest,
    pub plugin_root: PathBuf,
    pub manifest_path: PathBuf,
}

pub fn ensure_plugins_vault() -> MoatResult<PathBuf> {
    let root = config::plugins_root();
    fs::create_dir_all(&root)
        .map_err(|err| MoatError::Settings(format!("failed to create plugins vault: {err}")))?;
    Ok(root)
}

pub fn discover_plugin_manifests(root: &Path) -> MoatResult<Vec<LoadedPluginManifest>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    if !root.is_dir() {
        return Err(MoatError::Settings(format!(
            "plugins root '{}' is not a directory",
            root.display()
        )));
    }

    let mut plugins = Vec::new();
    let mut seen_ids = BTreeSet::new();
    let entries = fs::read_dir(root).map_err(|err| {
        MoatError::Settings(format!(
            "failed to read plugins root '{}': {err}",
            root.display()
        ))
    })?;

    for entry in entries {
        let entry = entry.map_err(|err| {
            MoatError::Settings(format!(
                "failed to read plugin entry under '{}': {err}",
                root.display()
            ))
        })?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        match load_plugin_manifest(&path)? {
            Some(loaded) => {
                if !seen_ids.insert(loaded.manifest.id.clone()) {
                    return Err(MoatError::Settings(format!(
                        "duplicate plugin id '{}' under '{}'",
                        loaded.manifest.id,
                        root.display()
                    )));
                }
                plugins.push(loaded);
            }
            None => continue,
        }
    }

    plugins.sort_by(|left, right| left.manifest.id.cmp(&right.manifest.id));
    Ok(plugins)
}

pub fn load_plugin_manifest(plugin_root: &Path) -> MoatResult<Option<LoadedPluginManifest>> {
    let Some(manifest_path) = find_manifest_path(plugin_root) else {
        return Ok(None);
    };
    let raw = fs::read_to_string(&manifest_path).map_err(|err| {
        MoatError::Settings(format!(
            "failed to read plugin manifest '{}': {err}",
            manifest_path.display()
        ))
    })?;

    let mut manifest = if manifest_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    {
        serde_json::from_str::<PluginManifest>(&raw).map_err(|err| {
            MoatError::Settings(format!(
                "failed to parse plugin manifest '{}': {err}",
                manifest_path.display()
            ))
        })?
    } else {
        serde_yaml::from_str::<PluginManifest>(&raw).map_err(|err| {
            MoatError::Settings(format!(
                "failed to parse plugin manifest '{}': {err}",
                manifest_path.display()
            ))
        })?
    };

    normalize_manifest(&mut manifest, &manifest_path)?;
    validate_manifest_paths(&manifest, plugin_root, &manifest_path)?;

    Ok(Some(LoadedPluginManifest {
        manifest,
        plugin_root: plugin_root.to_path_buf(),
        manifest_path,
    }))
}

fn find_manifest_path(plugin_root: &Path) -> Option<PathBuf> {
    for filename in MANIFEST_FILENAMES {
        let direct = plugin_root.join(filename);
        if direct.is_file() {
            return Some(direct);
        }
    }
    let codex = plugin_root.join(".codex-plugin").join("plugin.json");
    if codex.is_file() {
        return Some(codex);
    }
    None
}

fn normalize_manifest(manifest: &mut PluginManifest, manifest_path: &Path) -> MoatResult<()> {
    manifest.id = normalize_id(&manifest.id).ok_or_else(|| {
        MoatError::Settings(format!(
            "plugin manifest '{}' has an invalid id",
            manifest_path.display()
        ))
    })?;
    manifest.name = normalize_optional_text(manifest.name.take());
    manifest.description = normalize_optional_text(manifest.description.take());
    manifest.version = normalize_optional_text(manifest.version.take());
    manifest.entrypoint = normalize_optional_text(manifest.entrypoint.take());
    manifest.capabilities = normalize_string_list(std::mem::take(&mut manifest.capabilities));
    manifest.channels = normalize_string_list(std::mem::take(&mut manifest.channels));
    manifest.skills = normalize_string_list(std::mem::take(&mut manifest.skills));
    manifest.providers = normalize_string_list(std::mem::take(&mut manifest.providers));
    manifest.hooks.sort_by_key(|hook| hook.as_str());
    manifest.hooks.dedup();
    Ok(())
}

fn validate_manifest_paths(
    manifest: &PluginManifest,
    plugin_root: &Path,
    manifest_path: &Path,
) -> MoatResult<()> {
    let Some(entrypoint) = &manifest.entrypoint else {
        return Ok(());
    };
    let entrypoint_path = Path::new(entrypoint);
    if entrypoint_path.is_absolute()
        || entrypoint_path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        return Err(MoatError::SecurityViolation(format!(
            "plugin manifest '{}' entrypoint must stay inside plugin root",
            manifest_path.display()
        )));
    }

    let joined = plugin_root.join(entrypoint_path);
    if !joined.starts_with(plugin_root) {
        return Err(MoatError::SecurityViolation(format!(
            "plugin manifest '{}' entrypoint escapes plugin root",
            manifest_path.display()
        )));
    }
    Ok(())
}

fn normalize_id(raw: &str) -> Option<String> {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty()
        || normalized.len() > 96
        || !normalized
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
    {
        return None;
    }
    Some(normalized)
}

fn normalize_optional_text(raw: Option<String>) -> Option<String> {
    raw.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_string_list(values: Vec<String>) -> Vec<String> {
    let mut normalized = values
        .into_iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn default_enabled() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::{discover_plugin_manifests, load_plugin_manifest};
    use crate::sdk::RuntimeHook;
    use std::fs;
    use std::path::PathBuf;

    fn temp_plugin_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "moatageex-plugin-test-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("create temp root");
        root
    }

    #[test]
    fn loads_and_normalizes_plugin_manifest() {
        let root = temp_plugin_root("load");
        let plugin = root.join("demo");
        fs::create_dir_all(&plugin).expect("create plugin dir");
        fs::write(
            plugin.join("moat.plugin.yaml"),
            r#"
id: Demo_Plugin
name: " Demo "
entrypoint: "runner.wasm"
hooks:
  - before_prompt_build
  - before_prompt_build
capabilities:
  - " Gateway.RPC "
  - "gateway.rpc"
channels:
  - " WebChat "
"#,
        )
        .expect("write manifest");

        let loaded = load_plugin_manifest(&plugin)
            .expect("load manifest")
            .expect("manifest present");
        assert_eq!(loaded.manifest.id, "demo_plugin");
        assert_eq!(loaded.manifest.hooks, vec![RuntimeHook::BeforePromptBuild]);
        assert_eq!(loaded.manifest.capabilities, vec!["gateway.rpc"]);
        assert_eq!(loaded.manifest.channels, vec!["webchat"]);
    }

    #[test]
    fn rejects_entrypoint_path_escape() {
        let root = temp_plugin_root("escape");
        fs::write(
            root.join("plugin.json"),
            r#"{"id":"demo","entrypoint":"../outside.wasm"}"#,
        )
        .expect("write manifest");

        assert!(load_plugin_manifest(&root).is_err());
    }

    #[test]
    fn discovers_plugins_in_stable_order() {
        let root = temp_plugin_root("discover");
        for id in ["beta", "alpha"] {
            let plugin = root.join(id);
            fs::create_dir_all(&plugin).expect("create plugin dir");
            fs::write(
                plugin.join("openclaw.plugin.json"),
                format!(r#"{{"id":"{id}","hooks":["gateway_start"]}}"#),
            )
            .expect("write manifest");
        }

        let plugins = discover_plugin_manifests(&root).expect("discover");
        assert_eq!(
            plugins
                .iter()
                .map(|plugin| plugin.manifest.id.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
    }
}
