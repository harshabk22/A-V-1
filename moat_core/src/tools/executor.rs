use crate::capabilities::Capability;
use crate::meta_brain::{MetaBrain, PermissionScope};
use crate::approval::{ApprovalFlow, ApprovalRequest, ApprovalDecision};
use crate::session::SessionStore;
use crate::execution::{DelExecutor, ExecutionRequest};
use crate::execution::gate2_policy::normalize_workspace_path;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

pub struct ToolExecutor {
    meta_brain: Arc<Mutex<MetaBrain>>,
    agent_id: String,
    session_store: Arc<SessionStore>,
    del: DelExecutor,
}

impl ToolExecutor {
    pub fn new(
        meta_brain: Arc<Mutex<MetaBrain>>,
        agent_id: impl Into<String>,
        session_store: Arc<SessionStore>,
    ) -> Self {
        let del = DelExecutor::new().expect("Failed to initialize DEL executor");
        Self {
            meta_brain,
            agent_id: agent_id.into(),
            session_store,
            del,
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
                if !self.check_permission_with_approval(
                    Capability::FileRead,
                    PermissionScope::Path(PathBuf::from(&normalized)),
                )? {
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
                if !self.check_permission_with_approval(
                    Capability::FileWrite,
                    PermissionScope::Path(PathBuf::from(&normalized)),
                )? {
                    return Ok(format!("[fs:write] DENIED: No permission to write {}", raw_path));
                }
                self.execute_del("write_file", args)
            }
            "process_spawn" => {
                let command = args["command"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("process_spawn requires 'command' argument"))?;
                let working_dir = args["working_dir"].as_str()
                    .unwrap_or("~/Desktop/ageex-workspace");
                if !self.check_permission_with_approval(
                    Capability::ProcessSpawn,
                    PermissionScope::Path(PathBuf::from(working_dir)),
                )? {
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

    fn check_permission_with_approval(
        &mut self,
        capability: Capability,
        scope: PermissionScope,
    ) -> Result<bool> {
        let has_permission = {
            let brain = self.meta_brain.lock().unwrap();
            brain.check_permission(&self.agent_id, capability, &scope)
        };

        if has_permission {
            return Ok(true);
        }

        let request = ApprovalRequest {
            agent_id: self.agent_id.clone(),
            capability,
            scope: scope.clone(),
            description: format!("Agent requested {:?} access to {:?}", capability, scope),
        };

        let decision = ApprovalFlow::request_approval(request)?;

        match decision {
            ApprovalDecision::Granted => {
                let mut brain = self.meta_brain.lock().unwrap();
                brain.grant_permission(&self.agent_id, capability, scope.clone())?;
                let (scope_type, scope_value) = scope_to_parts(&scope);
                self.session_store.record_approval(
                    &self.agent_id,
                    &format!("{:?}", capability),
                    &scope_type,
                    &scope_value,
                )?;
                Ok(true)
            }
            ApprovalDecision::Denied => Ok(false),
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

    fn tool_to_del_capability(tool_name: &str) -> Capability {
        match tool_name {
            "read_file" => Capability::FileRead,
            "write_file" => Capability::FileWrite,
            "process_spawn" => Capability::ProcessSpawn,
            _ => Capability::FileRead,
        }
    }
}

fn scope_to_parts(scope: &PermissionScope) -> (String, String) {
    match scope {
        PermissionScope::Path(p) => ("path".into(), p.display().to_string()),
        PermissionScope::Domain(d) => ("domain".into(), d.clone()),
        PermissionScope::Global => ("global".into(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
