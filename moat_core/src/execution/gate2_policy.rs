use super::request::ExecutionRequest;
use super::result::ExecutionResult;
use anyhow::{Result, Context};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::fs;

#[derive(Debug, Deserialize, Clone)]
pub struct ExecutionPolicy {
    pub allow_network: bool,
    pub allow_process_spawn: bool,
    pub max_file_write_size_mb: usize,
    pub blocked_commands: Vec<String>,
    pub allowed_paths: Vec<String>,
    pub require_approval_for: Vec<String>,
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            allow_network: false,
            allow_process_spawn: true,
            max_file_write_size_mb: 10,
            blocked_commands: vec![
                "rm -rf /".into(),
                "sudo".into(),
                "mkfs".into(),
                "dd if=".into(),
            ],
            allowed_paths: vec![
                "~/Desktop/ageex-workspace".into(),
            ],
            require_approval_for: vec![
                "ProcessSpawn".into(),
                "FileWrite".into(),
            ],
        }
    }
}

pub struct Gate2Policy {
    policy: ExecutionPolicy,
}

impl Gate2Policy {
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read policy file: {:?}", path))?;
        let policy: ExecutionPolicy = toml::from_str(&contents)
            .with_context(|| "Failed to parse TOML policy")?;
        Ok(Self { policy })
    }

    pub fn default() -> Self {
        Self { policy: ExecutionPolicy::default() }
    }

    pub fn enforce(&self, req: &ExecutionRequest) -> Result<(), ExecutionResult> {
        let start = std::time::Instant::now();

        if let Some(command) = req.args.get("command").and_then(|v| v.as_str()) {
            for blocked in &self.policy.blocked_commands {
                if command.contains(blocked) {
                    return Err(ExecutionResult::gate2_denied(
                        &req.request_id,
                        &format!("Blocked command pattern: {}", blocked)
                    ));
                }
            }
        }

        if let Some(path_str) = req.args.get("path").and_then(|v| v.as_str()) {
            let normalized = Self::expand_tilde(&normalize_workspace_path(path_str));
            let mut path = PathBuf::from(&normalized);
            if !path.is_absolute() {
                path = req.working_dir.join(&path);
            }
            if !Self::is_path_allowed(&path, &self.policy.allowed_paths) {
                return Err(ExecutionResult::gate2_denied(
                    &req.request_id,
                    &format!("Path not in allowlist: {:?}", path)
                ));
            }
        }

        if req.tool_name == "write_file" || req.tool_name == "fs_write" {
            if let Some(content) = req.args.get("content").and_then(|v| v.as_str()) {
                let size_mb = content.len() / (1024 * 1024);
                if size_mb > self.policy.max_file_write_size_mb {
                    return Err(ExecutionResult::gate2_denied(
                        &req.request_id,
                        &format!("File too large: {}MB > {}MB limit", size_mb, self.policy.max_file_write_size_mb)
                    ));
                }
            }
        }

        if !self.policy.allow_network
            && (req.tool_name == "web_request" || req.tool_name == "url_open")
        {
            return Err(ExecutionResult::gate2_denied(
                &req.request_id,
                "Network access disabled by policy"
            ));
        }

        if !self.policy.allow_process_spawn && req.tool_name == "process_spawn" {
            return Err(ExecutionResult::gate2_denied(
                &req.request_id,
                "Process spawning disabled by policy"
            ));
        }

        eprintln!("[Gate2] ✓ Policy checks passed ({:.1}ms)", start.elapsed().as_secs_f64() * 1000.0);
        Ok(())
    }

    fn is_path_allowed(path: &Path, allowed: &[String]) -> bool {
        for allowed_pattern in allowed {
            let allowed_path = PathBuf::from(Self::expand_tilde(allowed_pattern));
            if path.starts_with(&allowed_path) {
                return true;
            }
        }
        false
    }

    fn expand_tilde(path: &str) -> String {
        if path.starts_with("~/") {
            if let Some(home) = dirs::home_dir() {
                return path.replacen("~", &home.display().to_string(), 1);
            }
        }
        path.to_string()
    }
}

pub(crate) fn normalize_workspace_path(path_str: &str) -> String {
    if path_str.starts_with("/workspace/") {
        let suffix = &path_str["/workspace/".len()..];
        let home = dirs::home_dir().expect("No home dir");
        return home
            .join("Desktop")
            .join("ageex-workspace")
            .join(suffix)
            .to_string_lossy()
            .to_string();
    }
    if path_str == "/workspace" {
        let home = dirs::home_dir().expect("No home dir");
        return home
            .join("Desktop")
            .join("ageex-workspace")
            .to_string_lossy()
            .to_string();
    }
    path_str.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::Capability;

    #[test]
    fn test_blocked_command_denied() {
        let gate = Gate2Policy::default();
        let req = ExecutionRequest::new(
            "agent2",
            "process_spawn",
            Capability::OsProcessSpawn,
            serde_json::json!({"command": "rm -rf / --no-preserve-root"})
        );
        assert!(gate.enforce(&req).is_err());
    }

    #[test]
    fn test_allowed_path_passes() {
        let gate = Gate2Policy::default();
        let req = ExecutionRequest::new(
            "agent2",
            "write_file",
            Capability::FileWrite,
            serde_json::json!({
                "path": "~/Desktop/ageex-workspace/test.txt",
                "content": "hello"
            })
        );
        assert!(gate.enforce(&req).is_ok());
    }

    #[test]
    fn test_disallowed_path_denied() {
        let gate = Gate2Policy::default();
        let req = ExecutionRequest::new(
            "agent2",
            "write_file",
            Capability::FileWrite,
            serde_json::json!({
                "path": "/etc/passwd",
                "content": "pwned"
            })
        );
        assert!(gate.enforce(&req).is_err());
    }
}
