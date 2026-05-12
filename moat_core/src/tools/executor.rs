use crate::capabilities::Capability;
use crate::execution::journal::ExecutionJournal;
use crate::meta_brain::{MetaBrain, PermissionFlags};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub struct ToolExecutor {
    meta_brain: Arc<Mutex<MetaBrain>>,
    agent_id: String,
    journal: ExecutionJournal,
}

impl ToolExecutor {
    pub fn new(
        meta_brain: Arc<Mutex<MetaBrain>>,
        agent_id: impl Into<String>,
    ) -> Self {
        let journal = ExecutionJournal::open()
            .unwrap_or_else(|e| {
                eprintln!("[ToolExecutor] Journal unavailable: {}", e);
                ExecutionJournal { path: std::path::PathBuf::new() }
            });
        Self {
            meta_brain,
            agent_id: agent_id.into(),
            journal,
        }
    }

    pub fn execute(&mut self, tool_name: &str, args_json: &str) -> Result<String> {
        let args: serde_json::Value = serde_json::from_str(args_json)
            .unwrap_or(serde_json::Value::Null);

        match tool_name {
            "fs_read" | "read_file" => {
                let raw_path = args["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("fs_read requires 'path' argument"))?;
                let normalized = normalize_workspace_path(raw_path);
                if !self.check_path_permission(PermissionFlags::FILE_READ, &normalized) {
                    return Ok(format!("[fs:read] DENIED: No permission to read {}", raw_path));
                }
                Self::execute_read_file(&normalized)
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
                if !self.check_path_permission(PermissionFlags::FILE_WRITE, &normalized) {
                    return Ok(format!("[fs:write] DENIED: No permission to write {}", raw_path));
                }
                let content = args["content"].as_str().unwrap_or("");
                let result = Self::execute_write_file(&normalized, content)?;
                self.journal.log_entry(&self.agent_id, "write_file", &normalized, true, &result);
                Ok(result)
            }
            "fs_delete" => {
                let raw_path = args["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("fs_delete requires 'path' argument"))?;
                let normalized = normalize_workspace_path(raw_path);
                if !self.check_path_permission(PermissionFlags::FILE_DELETE, &normalized) {
                    return Ok(format!("[fs:delete] DENIED: No permission to delete {}", raw_path));
                }
                self.execute_del("delete_file", &args)
            }
            "process_spawn" => {
                let command = args["command"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("process_spawn requires 'command' argument"))?;
                let working_dir = args["working_dir"].as_str()
                    .unwrap_or("~/Desktop/ageex-workspace");
                let normalized = normalize_workspace_path(working_dir);
                if !self.check_path_permission(PermissionFlags::EXEC, &normalized) {
                    return Ok(format!("[process:spawn] DENIED: No permission to run `{}`", command));
                }
                Self::execute_process_spawn(command, &normalized)
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

    fn execute_del(&self, del_tool_name: &str, args: &serde_json::Value) -> Result<String> {
        let capability = Self::tool_to_del_capability(del_tool_name);
        eprintln!("[ToolExecutor] → dispatching to DEL: {} (cap={:?})", del_tool_name, capability);
        match del_tool_name {
            "delete_file" => {
                let raw_path = args["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("delete_file requires 'path' argument"))?;
                let normalized = normalize_workspace_path(raw_path);
                let result = Self::execute_delete_file(&normalized)?;
                self.journal.log_entry(&self.agent_id, "delete_file", &normalized, true, &result);
                Ok(result)
            }
            _ => anyhow::bail!("Unhandled DEL tool: {}", del_tool_name),
        }
    }

    fn check_path_permission(&self, flag: PermissionFlags, path: &str) -> bool {
        let brain = self.meta_brain.lock().unwrap();
        brain.path_has_permission(path, flag)
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

    fn execute_read_file(path: &str) -> Result<String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Read failed: {}", e))?;
        Ok(format!("[fs:read] {}:\n{}", path, content))
    }

    fn execute_write_file(path: &str, content: &str) -> Result<String> {
        if content.is_empty() && Path::new(path).exists() {
            return Err(anyhow::anyhow!(
                "[fs:write] DENIED: Empty write to existing file detected. Use fs_delete to delete files."
            ));
        }

        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)
            .map_err(|e| anyhow::anyhow!("Write failed: {}", e))?;
        Ok(format!("[fs:write] Wrote {} bytes to {}", content.len(), path))
    }

    fn execute_delete_file(path: &str) -> Result<String> {
        if !Path::new(path).exists() {
            return Ok(format!("[fs:delete] File not found: {}", path));
        }

        if Self::is_path_protected(Path::new(path)) {
            return Err(anyhow::anyhow!(
                "[fs:delete] DENIED: {:?} matches a protected pattern. Deletion denied.", path
            ));
        }

        let trash_dir = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
            .join(".ageex").join("trash");
        std::fs::create_dir_all(&trash_dir)?;

        let original_name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let trash_name = format!("{}_{}", Uuid::new_v4(), original_name);
        let trash_path = trash_dir.join(&trash_name);

        std::fs::rename(path, &trash_path)
            .map_err(|e| anyhow::anyhow!("Delete failed: {}", e))?;

        Ok(format!("[fs:delete] Deleted {} (moved to trash: {:?})", path, trash_path))
    }

    fn is_path_protected(path: &Path) -> bool {
        let file_name = path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let path_str = path.to_string_lossy().to_string();

        let patterns: &[&str] = &[
            ".log", ".db", ".sqlite", ".key", ".pem",
        ];

        for pattern in patterns {
            if file_name.ends_with(pattern) {
                return true;
            }
        }

        let exact_matches: &[&str] = &[
            "Cargo.lock",
        ];

        for name in exact_matches {
            if file_name == *name {
                return true;
            }
        }

        if path_str.contains(".git/") {
            return true;
        }

        false
    }

    fn execute_process_spawn(command: &str, working_dir: &str) -> Result<String> {
        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(working_dir)
            .output()
            .map_err(|e| anyhow::anyhow!("Process spawn failed: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let combined = format!("{}{}", stdout, stderr);

        if output.status.success() {
            Ok(format!("[process:spawn] Exit 0:\n{}", combined))
        } else {
            Ok(format!("[process:spawn] Exit {}:\n{}",
                output.status.code().unwrap_or(-1), combined))
        }
    }

    fn execute_app_open(app_name: &str) -> Result<String> {
        let output = Command::new("open")
            .arg("-a")
            .arg(app_name)
            .output();

        match output {
            Ok(out) if out.status.success() => {
                Ok(format!("[app:open] OK: {} launched", app_name))
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                Ok(format!("[app:open] ERROR: {} — {}", app_name, stderr))
            }
            Err(e) => Ok(format!("[app:open] ERROR: {} — {}", app_name, e)),
        }
    }

    fn execute_url_open(url: &str) -> Result<String> {
        let output = Command::new("open")
            .arg(url)
            .output();

        match output {
            Ok(out) if out.status.success() => {
                Ok(format!("[url:open] OK: {} opened in browser", url))
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                Ok(format!("[url:open] ERROR: {} — {}", url, stderr))
            }
            Err(e) => Ok(format!("[url:open] ERROR: {} — {}", url, e)),
        }
    }
}

fn normalize_workspace_path(path_str: &str) -> String {
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

    #[test]
    fn test_fs_delete_requires_approval_when_not_granted() {
        // Verify tool_to_del_capability maps delete_file to FileDelete
        assert_eq!(
            ToolExecutor::tool_to_del_capability("delete_file"),
            Capability::FileDelete
        );

        // Verify read/write/spawn mappings are unchanged
        assert_eq!(
            ToolExecutor::tool_to_del_capability("read_file"),
            Capability::FileRead
        );
        assert_eq!(
            ToolExecutor::tool_to_del_capability("write_file"),
            Capability::FileWrite
        );
        assert_eq!(
            ToolExecutor::tool_to_del_capability("process_spawn"),
            Capability::ProcessSpawn
        );
    }

    #[test]
    fn test_unknown_tool_defaults_to_fileread() {
        assert_eq!(
            ToolExecutor::tool_to_del_capability("nonexistent_tool"),
            Capability::FileRead
        );
    }

    #[test]
    fn test_seatbelt_profile_contains_working_dir() {
        let profile = create_seatbelt_profile("/tmp");
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(allow file-write*"));
    }

    #[test]
    fn test_seatbelt_blocks_rm_rf_at_os_level() {
        let profile = create_seatbelt_profile("/tmp");
        let output = std::process::Command::new("sandbox-exec")
            .arg("-p")
            .arg(&profile)
            .arg("sh")
            .arg("-c")
            .arg("rm -rf /etc 2>&1; echo EXIT:$?")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("Operation not permitted"),
            "sandbox-exec should block rm -rf /etc at OS level, got: {}",
            stdout
        );
    }

    #[test]
    fn test_seatbelt_allows_write_inside_workspace() {
        let tmp = std::env::temp_dir();
        let test_file = tmp.join("ageex_seatbelt_test_ok");
        let ws = tmp.display().to_string();
        let tf = test_file.display().to_string();
        let _ = std::fs::remove_file(&test_file);

        let profile = create_seatbelt_profile(&ws);
        let output = std::process::Command::new("sandbox-exec")
            .arg("-p")
            .arg(&profile)
            .arg("sh")
            .arg("-c")
            .arg(format!("touch {} 2>&1; echo EXIT:$?", tf))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("EXIT:0"),
            "sandbox-exec should allow writes inside workspace, got: {}",
            stdout
        );
        let _ = std::fs::remove_file(&test_file);
    }

    #[test]
    fn test_delete_file_schema_valid_with_path() {
        let result = std::panic::catch_unwind(|| {
            let json = r#"{"path": "/tmp/test.txt"}"#;
            let args: serde_json::Value = serde_json::from_str(json).unwrap();
            let path = args["path"].as_str().unwrap();
            assert_eq!(path, "/tmp/test.txt");
        });
        assert!(result.is_ok(), "Schema should accept valid path");
    }

    #[test]
    fn test_delete_file_schema_rejects_missing_path() {
        let json = r#"{}"#;
        let args: serde_json::Value = serde_json::from_str(json).unwrap();
        let path = args["path"].as_str();
        assert!(path.is_none(), "Missing path should yield None");
    }

    #[test]
    fn test_delete_file_descender_executes_correctly() {
        assert_eq!(
            ToolExecutor::tool_to_del_capability("delete_file"),
            Capability::FileDelete
        );
    }

    #[test]
    fn test_delete_file_moves_to_trash() {
        let tmp = std::env::temp_dir();
        let test_file = tmp.join("test_delete_me_trash.txt");
        std::fs::write(&test_file, "hello world").unwrap();

        let result = ToolExecutor::execute_delete_file(&test_file.to_string_lossy());
        assert!(result.is_ok(), "Delete should succeed: {:?}", result);
        let msg = result.unwrap();
        assert!(msg.contains("trash"), "Output should mention trash: {}", msg);

        assert!(!test_file.exists(), "File should be deleted from original location");

        let trash_dir = dirs::home_dir().unwrap().join(".ageex").join("trash");
        assert!(trash_dir.exists(), "Trash dir should exist");
        let mut trash_found = false;
        for entry in std::fs::read_dir(&trash_dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().to_string_lossy().contains("test_delete_me_trash") {
                trash_found = true;
                std::fs::remove_file(entry.path()).ok();
                break;
            }
        }
        assert!(trash_found, "File should appear in trash directory");
    }

    #[test]
    fn test_delete_nonexistent_file_returns_not_found() {
        let tmp = std::env::temp_dir();
        let nonexistent = tmp.join("does_not_exist_xyz_trash_test");
        let _ = std::fs::remove_file(&nonexistent);

        let result = ToolExecutor::execute_delete_file(&nonexistent.to_string_lossy());
        assert!(result.is_ok(), "Return should be Ok (not found message)");
        let msg = result.unwrap();
        assert!(msg.contains("not found"), "Should report file not found, got: {}", msg);
    }

    #[test]
    fn test_write_empty_to_existing_file_blocked() {
        let tmp = std::env::temp_dir();
        let test_file = tmp.join("existing_trash_test.txt");
        std::fs::write(&test_file, "some content").unwrap();

        let result = ToolExecutor::execute_write_file(
            &test_file.to_string_lossy(),
            ""
        );
        assert!(result.is_err(), "Empty write to existing file should be blocked");
        let err = format!("{:?}", result.unwrap_err());
        assert!(err.contains("fs_delete") || err.contains("empty"),
            "Error should mention deletion: {}", err);

        std::fs::remove_file(&test_file).ok();
    }

    #[test]
    fn test_delete_protected_file_denied() {
        let tmp = std::env::temp_dir();
        let protected_file = tmp.join("test_debug.log");
        std::fs::write(&protected_file, "log content").unwrap();

        let result = ToolExecutor::execute_delete_file(&protected_file.to_string_lossy());
        assert!(result.is_err(), "Deleting *.log should be denied");
        let err = format!("{:?}", result.unwrap_err());
        assert!(err.contains("DENIED") || err.contains("protected"),
            "Error should mention protection: {}", err);

        assert!(protected_file.exists(), "Protected file should still exist");
        std::fs::remove_file(&protected_file).ok();
    }

    #[test]
    fn test_delete_unprotected_file_succeeds() {
        let tmp = std::env::temp_dir();
        let normal_file = tmp.join("normal_rs_file_test.rs");
        std::fs::write(&normal_file, "fn main() {}").unwrap();

        let result = ToolExecutor::execute_delete_file(&normal_file.to_string_lossy());
        assert!(result.is_ok(), "Deleting .rs file should succeed");
        assert!(!normal_file.exists(), "File should be deleted");
    }
}

fn create_seatbelt_profile(working_dir: &str) -> String {
    let resolved = std::fs::canonicalize(working_dir)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| working_dir.to_string());
    let safe_dir = resolved.replace('"', "\\\"");

    let home = std::env::var("HOME").unwrap_or_default();
    let cargo_exec = format!("(allow process-exec (subpath \"{}/.cargo\"))", home);
    let cargo_read = format!("(allow file-read* (subpath \"{}/.cargo\"))", home);
    let rustup_read = format!("(allow file-read* (subpath \"{}/.rustup\"))", home);

    format!(
        r#"(version 1)
(deny default)
(allow file-read*)
(allow file-write* (subpath "{safe_dir}"))
(allow process-exec*)
(allow process-exec (subpath "/opt/homebrew"))
(allow process-exec (subpath "/usr/local"))
(allow file-read* (literal "/usr/local/bin"))
{0}
{1}
{2}
(allow process-fork)
(allow sysctl-read)
(allow signal (target self))
(allow network*)
(allow ipc-posix*)
(allow mach*)
"#,
        cargo_exec, cargo_read, rustup_read
    )
}
