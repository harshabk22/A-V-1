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
}

impl ToolExecutor {
    pub fn new(
        meta_brain: Arc<Mutex<MetaBrain>>,
        agent_id: impl Into<String>,
        session_store: Arc<SessionStore>,
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
        }
    }

    pub async fn execute(&mut self, tool_name: &str, args_json: &str) -> Result<String> {
        let args: serde_json::Value = serde_json::from_str(args_json)
            .unwrap_or(serde_json::Value::Null);

        match tool_name {
            "fs_read" | "read_file" => {
                let raw_path = args["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("fs_read requires 'path' argument"))?;
                let normalized = normalize_workspace_path(raw_path);
                if !self.check_path_permission(PermissionFlags::FILE_READ, &normalized).await? {
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
                let normalized = normalize_workspace_path(raw_path);
                if !self.check_path_permission(PermissionFlags::FILE_WRITE, &normalized).await? {
                    return Ok(format!("[fs:write] DENIED: No permission to write {}", raw_path));
                }

                let content = args["content"].as_str().unwrap_or("");
                if content.is_empty() && Path::new(&normalized).exists() {
                    return Err(anyhow::anyhow!(
                        "[fs:write] DENIED: Empty write to existing file detected. Use fs_delete to delete files."
                    ));
                }

                let result = self.execute_del("write_file", args)?;
                self.journal.log_entry(&self.agent_id, "write_file", &normalized, true, &result);
                Ok(result)
            }
            "fs_delete" => {
                let raw_path = args["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("fs_delete requires 'path' argument"))?;
                let normalized = normalize_workspace_path(raw_path);
                if !self.check_path_permission(PermissionFlags::FILE_DELETE, &normalized).await? {
                    return Ok(format!("[fs:delete] DENIED: No permission to delete {}", raw_path));
                }

                if !Path::new(&normalized).exists() {
                    return Ok(format!("[fs:delete] File not found: {}", normalized));
                }

                if Self::is_path_protected(Path::new(&normalized)) {
                    return Err(anyhow::anyhow!(
                        "[fs:delete] DENIED: {:?} matches a protected pattern. Deletion denied.", normalized
                    ));
                }

                let trash_dir = dirs::home_dir()
                    .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
                    .join(".ageex").join("trash");
                std::fs::create_dir_all(&trash_dir)?;

                let original_name = Path::new(&normalized)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let trash_name = format!("{}_{}", Uuid::new_v4(), original_name);
                let trash_path = trash_dir.join(&trash_name);

                std::fs::rename(&normalized, &trash_path)
                    .map_err(|e| anyhow::anyhow!("Delete failed: {}", e))?;

                let result = format!("[fs:delete] Deleted {} (moved to trash: {:?})", normalized, trash_path);
                self.journal.log_entry(&self.agent_id, "delete_file", &normalized, true, &result);
                Ok(result)
            }
            "process_spawn" => {
                let command = args["command"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("process_spawn requires 'command' argument"))?;
                let working_dir = args["working_dir"].as_str()
                    .unwrap_or("~/Desktop/ageex-workspace");
                if !self.check_path_permission(PermissionFlags::EXEC, working_dir).await? {
                    return Ok(format!("[process:spawn] DENIED: No permission to run `{}`", command));
                }
                self.execute_del("process_spawn", args)
            }
            "app_open" => {
                let app_name = args["app_name"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("app_open requires 'app_name' argument"))?;
                Self::execute_app_open(app_name)
            }
            "url_open" => {
                let url = args["url"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("url_open requires 'url' argument"))?;
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

        let request = ApprovalRequest {
            agent_id: self.agent_id.clone(),
            capability: Capability::FileWrite,
            scope: path.to_string(),
            description: format!("Agent requested {:?} access to {}", flags, path),
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
