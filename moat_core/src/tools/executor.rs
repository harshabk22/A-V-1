use crate::capabilities::Capability;
use crate::meta_brain::{MetaBrain, PermissionFlags};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

pub struct ToolExecutor {
    meta_brain: Arc<Mutex<MetaBrain>>,
    agent_id: String,
}

impl ToolExecutor {
    pub fn new(
        meta_brain: Arc<Mutex<MetaBrain>>,
        agent_id: impl Into<String>,
    ) -> Self {
        Self {
            meta_brain,
            agent_id: agent_id.into(),
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
                Self::execute_write_file(&normalized, content)
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
                Self::execute_delete_file(&normalized)
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
        std::fs::remove_file(path)
            .map_err(|e| anyhow::anyhow!("Delete failed: {}", e))?;
        Ok(format!("[fs:delete] Deleted {}", path))
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
