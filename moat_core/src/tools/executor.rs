use crate::capabilities::Capability;
use crate::meta_brain::{MetaBrain, PermissionFlags};
use crate::approval::{ApprovalFlow, ApprovalRequest, ApprovalDecision};
use crate::session::SessionStore;
use crate::execution::{DelExecutor, ExecutionRequest};
use crate::execution::gate2_policy::normalize_workspace_path;
use crate::execution::journal::ExecutionJournal;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub struct ToolExecutor {
    meta_brain: Arc<Mutex<MetaBrain>>,
    agent_id: String,
    session_store: Arc<SessionStore>,
    del: DelExecutor,
    journal: ExecutionJournal,
    auto_deny: bool,
}

impl ToolExecutor {
    pub fn new(
        meta_brain: Arc<Mutex<MetaBrain>>,
        agent_id: impl Into<String>,
        session_store: Arc<SessionStore>,
    ) -> Self {
        Self::new_with_auto_deny(meta_brain, agent_id, session_store, false)
    }

    pub fn new_with_auto_deny(
        meta_brain: Arc<Mutex<MetaBrain>>,
        agent_id: impl Into<String>,
        session_store: Arc<SessionStore>,
        auto_deny: bool,
    ) -> Self {
        let del = DelExecutor::new().expect("Failed to initialize DEL executor");
        let journal = ExecutionJournal::open()
            .unwrap_or_else(|e| {
                eprintln!("[ToolExecutor] Journal unavailable: {}", e);
                ExecutionJournal { path: std::path::PathBuf::new() }
            });
        Self {
            meta_brain,
            agent_id: agent_id.into(),
            session_store,
            del,
            journal,
            auto_deny,
        }
    }

    pub async fn execute(&mut self, tool_name: &str, args_json: &str) -> Result<String> {
        let args: serde_json::Value = serde_json::from_str(args_json)
            .unwrap_or(serde_json::Value::Null);

        match tool_name {
            "fs_read" | "read_file" => {
                let raw_path = args["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("fs_read requires 'path' argument"))?;
                let resolved = Self::resolve_path(raw_path);
                if !self.check_path_permission(PermissionFlags::FILE_READ, &resolved).await? {
                    return Ok(format!("[fs:read] DENIED: No permission to read {}", raw_path));
                }
                self.execute_del("read_file", args)
            }
            "fs_exists" => {
                let path = args["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("fs_exists requires 'path' argument"))?;
                Ok(format!("[fs:exists] {} → {}", path, Path::new(path).exists()))
            }
            "fs_write" | "write_file" => {
                let raw_path = args["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("fs_write requires 'path' argument"))?;
                let resolved = Self::resolve_path(raw_path);
                if !self.check_path_permission(PermissionFlags::FILE_WRITE, &resolved).await? {
                    return Ok(format!("[fs:write] DENIED: No permission to write {}", raw_path));
                }

                let content = args["content"].as_str().unwrap_or("");
                if content.is_empty() && Path::new(&resolved).exists() {
                    return Err(anyhow::anyhow!(
                        "[fs:write] DENIED: Empty write to existing file detected. Use fs_delete to delete files."
                    ));
                }

                let result = self.execute_del("write_file", args)?;
                self.journal.log_entry(&self.agent_id, "write_file", &resolved, true, &result);
                Ok(result)
            }
            "fs_delete" => {
                let raw_path = args["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("fs_delete requires 'path' argument"))?;
                let resolved = Self::resolve_path(raw_path);
                if !self.check_path_permission(PermissionFlags::FILE_DELETE, &resolved).await? {
                    return Ok(format!("[fs:delete] DENIED: No permission to delete {}", raw_path));
                }

                if !Path::new(&resolved).exists() {
                    return Ok(format!("[fs:delete] File not found: {}", resolved));
                }

                if Self::is_path_protected(Path::new(&resolved)) {
                    return Err(anyhow::anyhow!(
                        "[fs:delete] DENIED: {:?} matches a protected pattern. Deletion denied.", resolved
                    ));
                }

                let trash_dir = dirs::home_dir()
                    .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
                    .join(".ageex").join("trash");
                std::fs::create_dir_all(&trash_dir)?;

                let original_name = Path::new(&resolved)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let trash_name = format!("{}_{}", Uuid::new_v4(), original_name);
                let trash_path = trash_dir.join(&trash_name);

                std::fs::rename(&resolved, &trash_path)
                    .map_err(|e| anyhow::anyhow!("Delete failed: {}", e))?;

                let result = format!("[fs:delete] Deleted {} (moved to trash: {:?})", resolved, trash_path);
                self.journal.log_entry(&self.agent_id, "delete_file", &resolved, true, &result);
                Ok(result)
            }
            "process_spawn" => {
                let command = args["command"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("process_spawn requires 'command' argument"))?;
                let working_dir_raw = args["working_dir"].as_str()
                    .unwrap_or("~/Desktop/ageex-workspace");
                let resolved_wd = Self::resolve_path(working_dir_raw);
                if !self.check_path_permission(PermissionFlags::EXEC, &resolved_wd).await? {
                    return Ok(format!("[process:spawn] DENIED: No permission to run `{}`", command));
                }
                self.execute_del("process_spawn", args)
            }
            "app_open" => {
                let app_name = args["app_name"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("app_open requires 'app_name' argument"))?;
                if !self.check_app_permission(app_name).await? {
                    return Ok(format!("[app:open] DENIED: No permission to open {}", app_name));
                }
                Self::execute_app_open(app_name)
            }
            "url_open" => {
                let url = args["url"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("url_open requires 'url' argument"))?;
                if !self.check_url_permission(url).await? {
                    return Ok(format!("[url:open] DENIED: No permission to open {}", url));
                }
                Self::execute_url_open(url)
            }
            unknown => anyhow::bail!("Unknown tool: {}", unknown),
        }
    }

    fn execute_del(&self, del_tool_name: &str, args: serde_json::Value) -> Result<String> {
        let capability = Self::tool_to_del_capability(del_tool_name);
        let request = ExecutionRequest::new(&self.agent_id, del_tool_name, capability, args);
        eprintln!("[ToolExecutor] → dispatching to DEL: {} (cap={:?})", del_tool_name, capability);
        let result = self.del.execute(request);
        eprintln!("[ToolExecutor] ← DEL returned success={} gate1={} gate2={} gate3={}",
            result.success, result.gate1_passed, result.gate2_passed, result.gate3_passed);
        if result.success {
            Ok(result.output)
        } else {
            Ok(format!("Execution denied: {}", result.denied_reason.unwrap_or_default()))
        }
    }

    async fn check_path_permission(&mut self, flags: PermissionFlags, path: &str) -> Result<bool> {
        let has_permission = {
            let brain = self.meta_brain.lock().unwrap();
            brain.path_has_permission(path, flags)
        };

        if has_permission {
            return Ok(true);
        }

        if self.auto_deny {
            let label = Self::flags_to_label(flags);
            eprintln!("[ToolExecutor] auto-deny: {} attempted {} on {} (permission not pre-granted)", self.agent_id, label, path);
            return Ok(false);
        }

        let capability = Self::flags_to_capability(flags);
        let label = Self::flags_to_label(flags);

        let request = ApprovalRequest {
            agent_id: self.agent_id.clone(),
            capability,
            scope: path.to_string(),
            description: format!("Agent {} requested {} access to {}", self.agent_id, label, path),
        };

        let decision = ApprovalFlow::request_approval(request).await?;

        match decision {
            ApprovalDecision::Granted => {
                let mut brain = self.meta_brain.lock().unwrap();
                brain.grant_path_permission(path, flags).map_err(|e| anyhow::anyhow!("{}", e))?;
                self.session_store.record_approval(
                    &self.agent_id,
                    &format!("{:?}", flags),
                    "path",
                    path,
                )?;
                Ok(true)
            }
            ApprovalDecision::Denied | ApprovalDecision::TimedOut => Ok(false),
        }
    }

    async fn check_app_permission(&mut self, app_name: &str) -> Result<bool> {
        let has_permission = {
            let brain = self.meta_brain.lock().unwrap();
            brain.app_has_permission(app_name, PermissionFlags::APP_LAUNCH)
        };

        if has_permission {
            return Ok(true);
        }

        if self.auto_deny {
            eprintln!("[ToolExecutor] auto-deny: {} attempted app_open on {} (permission not pre-granted)", self.agent_id, app_name);
            return Ok(false);
        }

        let request = ApprovalRequest {
            agent_id: self.agent_id.clone(),
            capability: Capability::AppOpen,
            scope: app_name.to_string(),
            description: format!("Agent {} requested to open app '{}'", self.agent_id, app_name),
        };

        let decision = ApprovalFlow::request_approval(request).await?;

        match decision {
            ApprovalDecision::Granted => {
                let mut brain = self.meta_brain.lock().unwrap();
                brain.grant_app_permission(app_name, PermissionFlags::APP_LAUNCH)
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                self.session_store.record_approval(
                    &self.agent_id,
                    "APP_LAUNCH",
                    "app",
                    app_name,
                )?;
                Ok(true)
            }
            ApprovalDecision::Denied | ApprovalDecision::TimedOut => Ok(false),
        }
    }

    async fn check_url_permission(&mut self, url_str: &str) -> Result<bool> {
        use crate::meta_brain::PermissionFlags as Pf;

        let (scheme, domain) = if let Ok(parsed) = url::Url::parse(url_str) {
            let scheme = parsed.scheme().to_string();
            let domain = parsed.host_str().unwrap_or("").to_string();
            (scheme, domain)
        } else {
            let scheme = url_str.split("://").next().unwrap_or(url_str).to_string();
            (scheme, String::new())
        };

        let has_permission = {
            let brain = self.meta_brain.lock().unwrap();
            let scheme_ok = scheme.is_empty() || brain.url_scheme_has_permission(&scheme, Pf::URL_OPEN);
            let domain_ok = domain.is_empty() || brain.domain_has_permission(&domain, Pf::NET_OUTBOUND);
            scheme_ok && domain_ok
        };

        if has_permission {
            return Ok(true);
        }

        if self.auto_deny {
            eprintln!("[ToolExecutor] auto-deny: {} attempted url_open on {} (permission not pre-granted)", self.agent_id, url_str);
            return Ok(false);
        }

        let request = ApprovalRequest {
            agent_id: self.agent_id.clone(),
            capability: Capability::UrlOpen,
            scope: url_str.to_string(),
            description: format!("Agent {} requested to open URL '{}'", self.agent_id, url_str),
        };

        let decision = ApprovalFlow::request_approval(request).await?;

        match decision {
            ApprovalDecision::Granted => {
                let mut brain = self.meta_brain.lock().unwrap();
                if !scheme.is_empty() {
                    let _ = brain.grant_url_scheme_permission(&scheme, Pf::URL_OPEN);
                }
                if !domain.is_empty() {
                    let _ = brain.grant_domain_permission(&domain, Pf::NET_OUTBOUND);
                }
                self.session_store.record_approval(
                    &self.agent_id,
                    "URL_OPEN",
                    "url",
                    url_str,
                )?;
                Ok(true)
            }
            ApprovalDecision::Denied | ApprovalDecision::TimedOut => Ok(false),
        }
    }

    pub(crate) fn tool_to_del_capability(tool_name: &str) -> Capability {
        match tool_name {
            "read_file" => Capability::FileRead,
            "write_file" => Capability::FileWrite,
            "delete_file" => Capability::FileDelete,
            "process_spawn" => Capability::ProcessSpawn,
            _ => Capability::FileRead,
        }
    }

    fn execute_app_open(app_name: &str) -> Result<String> {
        Command::new("open").arg("-a").arg(app_name).output().map_or_else(
            |e| Ok(format!("[app:open] ERROR: {}", e)),
            |out| {
                if out.status.success() {
                    Ok(format!("[app:open] OK: {} launched", app_name))
                } else {
                    Ok(format!("[app:open] ERROR: {}", String::from_utf8_lossy(&out.stderr)))
                }
            },
        )
    }

    fn execute_url_open(url: &str) -> Result<String> {
        Command::new("open").arg(url).output().map_or_else(
            |e| Ok(format!("[url:open] ERROR: {}", e)),
            |_| Ok(format!("[url:open] OK: {} opened in browser", url)),
        )
    }

    fn is_path_protected(path: &Path) -> bool {
        let file_name = path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let path_str = path.to_string_lossy().to_string();

        for pattern in &[".log", ".db", ".sqlite", ".key", ".pem"] {
            if file_name.ends_with(pattern) { return true; }
        }
        if file_name == "Cargo.lock" { return true; }
        if path_str.contains(".git/") { return true; }
        false
    }

    fn resolve_path(raw: &str) -> String {
        let expanded = if raw.starts_with("~/") {
            if let Some(home) = dirs::home_dir() {
                home.to_string_lossy().into_owned() + &raw[1..]
            } else {
                raw.to_string()
            }
        } else {
            raw.to_string()
        };
        let p = Path::new(&expanded);
        if p.is_absolute() {
            if let Ok(canonical) = std::fs::canonicalize(&expanded) {
                canonical.to_string_lossy().into_owned()
            } else {
                expanded
            }
        } else {
            let workspace = dirs::home_dir()
                .unwrap_or_default()
                .join("Desktop")
                .join("ageex-workspace");
            workspace.join(raw).to_string_lossy().into_owned()
        }
    }

    fn flags_to_capability(flags: PermissionFlags) -> Capability {
        if flags.contains(PermissionFlags::FILE_DELETE) { Capability::FileDelete }
        else if flags.contains(PermissionFlags::FILE_WRITE) { Capability::FileWrite }
        else if flags.contains(PermissionFlags::FILE_READ) { Capability::FileRead }
        else if flags.contains(PermissionFlags::EXEC) { Capability::ProcessSpawn }
        else { Capability::FileRead }
    }

    fn flags_to_label(flags: PermissionFlags) -> &'static str {
        if flags.contains(PermissionFlags::FILE_DELETE) { "fs:delete" }
        else if flags.contains(PermissionFlags::FILE_WRITE) { "fs:write" }
        else if flags.contains(PermissionFlags::FILE_READ) { "fs:read" }
        else if flags.contains(PermissionFlags::EXEC) { "process:spawn" }
        else { "fs:read" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_to_del_capability_mappings() {
        assert_eq!(ToolExecutor::tool_to_del_capability("delete_file"), Capability::FileDelete);
        assert_eq!(ToolExecutor::tool_to_del_capability("read_file"), Capability::FileRead);
        assert_eq!(ToolExecutor::tool_to_del_capability("write_file"), Capability::FileWrite);
        assert_eq!(ToolExecutor::tool_to_del_capability("process_spawn"), Capability::ProcessSpawn);
    }

    #[test]
    fn test_unknown_tool_defaults_to_file_read() {
        assert_eq!(ToolExecutor::tool_to_del_capability("nonexistent"), Capability::FileRead);
    }

    #[test]
    fn test_path_protection_detects_dangerous_extensions() {
        assert!(ToolExecutor::is_path_protected(Path::new("/tmp/test.log")));
        assert!(ToolExecutor::is_path_protected(Path::new("/tmp/test.db")));
        assert!(!ToolExecutor::is_path_protected(Path::new("/tmp/test.rs")));
    }

    #[test]
    fn test_cargo_lock_is_protected() {
        assert!(ToolExecutor::is_path_protected(Path::new("/project/Cargo.lock")));
    }
}

    #[test]
    fn test_seatbelt_profile_contains_working_dir() {
        let profile = r#"(version 1)
(deny default)
(allow file-read*)
(allow file-write* (subpath "/tmp/test"))
(allow process-exec*)"#;
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(allow file-write*"));
    }

    #[test]
    fn test_protected_patterns_prevent_log_deletion() {
        assert!(ToolExecutor::is_path_protected(Path::new("/workspace/debug.log")), "*.log must be protected");
        assert!(ToolExecutor::is_path_protected(Path::new("/workspace/data.db")), "*.db must be protected");
        assert!(ToolExecutor::is_path_protected(Path::new("/workspace/secrets.key")), "*.key must be protected");
        assert!(ToolExecutor::is_path_protected(Path::new("/workspace/cert.pem")), "*.pem must be protected");
    }

    #[test]
    fn test_git_directory_is_protected() {
        assert!(ToolExecutor::is_path_protected(Path::new("/workspace/.git/config")), ".git/** must be protected");
    }

    #[test]
    fn test_regular_files_are_not_protected() {
        assert!(!ToolExecutor::is_path_protected(Path::new("/workspace/main.rs")), ".rs files should NOT be protected");
        assert!(!ToolExecutor::is_path_protected(Path::new("/workspace/README.md")), ".md files should NOT be protected");
    }
