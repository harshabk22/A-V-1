use super::request::ExecutionRequest;
use super::result::ExecutionResult;
use super::gate2_policy::normalize_workspace_path;
use anyhow::{Result, Context};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::fs;

pub struct Gate3Sandbox;

impl Gate3Sandbox {
    pub fn execute(req: &ExecutionRequest) -> Result<ExecutionResult, ExecutionResult> {
        let start = std::time::Instant::now();

        match req.tool_name.as_str() {
            "read_file" | "fs_read" => Self::execute_read_file(req, start),
            "write_file" | "fs_write" => Self::execute_write_file(req, start),
            "process_spawn" => Self::execute_process_spawn(req, start),
            _ => Err(ExecutionResult::gate3_denied(
                &req.request_id,
                &format!("Unknown sandbox tool: {}", req.tool_name)
            )),
        }
    }

    fn execute_read_file(req: &ExecutionRequest, start: std::time::Instant) -> Result<ExecutionResult, ExecutionResult> {
        let raw = req.args["path"].as_str().unwrap();
        let normalized = normalize_workspace_path(raw);
        let path = Self::resolve_path(&normalized, &req.working_dir);

        let content = fs::read_to_string(&path)
            .map_err(|e| ExecutionResult::gate3_denied(&req.request_id, &format!("Read failed: {}", e)))?;

        let elapsed = start.elapsed().as_millis() as u64;
        eprintln!("[Gate3] ✓ read_file succeeded ({:.1}ms)", elapsed as f64);
        Ok(ExecutionResult::success(&req.request_id, content, None, elapsed))
    }

    fn execute_write_file(req: &ExecutionRequest, start: std::time::Instant) -> Result<ExecutionResult, ExecutionResult> {
        let raw = req.args["path"].as_str().unwrap();
        let content = req.args["content"].as_str().unwrap();
        let normalized = normalize_workspace_path(raw);
        let path = Self::resolve_path(&normalized, &req.working_dir);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| ExecutionResult::gate3_denied(&req.request_id, &format!("mkdir failed: {}", e)))?;
        }

        fs::write(&path, content)
            .map_err(|e| ExecutionResult::gate3_denied(&req.request_id, &format!("Write failed: {}", e)))?;

        let elapsed = start.elapsed().as_millis() as u64;
        eprintln!("[Gate3] ✓ write_file succeeded ({:.1}ms)", elapsed as f64);
        Ok(ExecutionResult::success(&req.request_id, format!("Wrote {} bytes to {:?}", content.len(), path), None, elapsed))
    }

    fn execute_process_spawn(req: &ExecutionRequest, start: std::time::Instant) -> Result<ExecutionResult, ExecutionResult> {
        let command = req.args["command"].as_str().unwrap();
        let profile_path = Self::create_seatbelt_profile(&req.working_dir)
            .map_err(|e| ExecutionResult::gate3_denied(&req.request_id, &format!("Seatbelt setup failed: {}", e)))?;

        let output = Command::new("sandbox-exec")
            .arg("-f").arg(&profile_path)
            .arg("/bin/sh").arg("-c").arg(command)
            .current_dir(&req.working_dir)
            .output()
            .map_err(|e| ExecutionResult::gate3_denied(&req.request_id, &format!("Execution failed: {}", e)))?;

        fs::remove_file(profile_path).ok();

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let combined = format!("{}{}", stdout, stderr);
        let exit_code = output.status.code();

        let elapsed = start.elapsed().as_millis() as u64;
        eprintln!("[Gate3] ✓ process_spawn completed (exit: {:?}, {:.1}ms)", exit_code, elapsed as f64);
        Ok(ExecutionResult::success(&req.request_id, combined, exit_code, elapsed))
    }

    fn create_seatbelt_profile(working_dir: &Path) -> Result<PathBuf> {
        let home = dirs::home_dir().context("Cannot determine home directory")?;
        let cargo_path = home.join(".cargo");
        let rustup_path = home.join(".rustup");

        let profile = format!(
            r#"(version 1)
(deny default)
(allow process-exec (literal "/bin/sh"))
(allow process-exec (subpath "/usr/local"))
(allow process-exec (subpath "/opt/homebrew"))
(allow process-exec (subpath "{cargo}"))
(allow file-read* (subpath "{working}"))
(allow file-write* (subpath "{working}"))
(allow file-read* (literal "/usr/local/bin"))
(allow file-read* (subpath "{cargo}"))
(allow file-read* (subpath "{rustup}"))
(allow file-read-metadata)
(deny network*)
(allow system-socket (require-all (socket-domain AF_UNIX)))
"#,
            working = working_dir.display(),
            cargo = cargo_path.display(),
            rustup = rustup_path.display(),
        );

        let temp = std::env::temp_dir().join(format!("ageex-{}.sb", uuid::Uuid::new_v4()));
        fs::write(&temp, profile)?;
        Ok(temp)
    }

    fn resolve_path(path_str: &str, working_dir: &Path) -> PathBuf {
        let expanded = if path_str.starts_with("~/") {
            if let Some(home) = dirs::home_dir() {
                path_str.replacen("~", &home.display().to_string(), 1)
            } else {
                path_str.to_string()
            }
        } else {
            path_str.to_string()
        };
        let path = PathBuf::from(&expanded);
        if path.is_absolute() {
            path
        } else {
            working_dir.join(path)
        }
    }
}
