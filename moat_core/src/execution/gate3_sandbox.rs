use super::request::ExecutionRequest;
use super::result::ExecutionResult;
use super::gate2_policy::normalize_workspace_path;
use anyhow::{Result, Context};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::fs;
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub struct Gate3Sandbox;

impl Gate3Sandbox {
    pub fn execute(req: &ExecutionRequest) -> Result<ExecutionResult, ExecutionResult> {
        let start = std::time::Instant::now();

        match req.tool_name.as_str() {
            "read_file" | "fs_read" => Self::execute_read_file(req, start),
            "write_file" | "fs_write" => Self::execute_write_file(req, start),
            "process_spawn" => Self::execute_process_spawn(req, start),
            "app_open" => Self::execute_app_open(req, start),
            "url_open" => Self::execute_url_open(req, start),
            "applescript" => Self::execute_applescript(req, start),
            "net_curl" => Self::execute_net_curl(req, start),
            "inject_text" => Self::execute_inject_text(req, start),
            "fs_exists" => Self::execute_fs_exists(req, start),
            "fs_delete" => Self::execute_fs_delete(req, start),
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

        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|e| ExecutionResult::gate3_denied(
                &req.request_id,
                &format!("Read open failed (O_NOFOLLOW): {}. Symlink traversal denied.", e),
            ))?;

        let content = std::io::read_to_string(file)
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

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|e| ExecutionResult::gate3_denied(
                &req.request_id,
                &format!("Write open failed (O_NOFOLLOW): {}. Symlink traversal denied.", e),
            ))?;

        use std::io::Write;
        file.write_all(content.as_bytes())
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

    fn execute_app_open(req: &ExecutionRequest, start: std::time::Instant) -> Result<ExecutionResult, ExecutionResult> {
        let app_name = req.args["app_name"].as_str().unwrap_or("");
        let output = std::process::Command::new("open")
            .arg("-a").arg(app_name)
            .output()
            .map_err(|e| ExecutionResult::gate3_denied(&req.request_id, &format!("app_open failed: {}", e)))?;

        let elapsed = start.elapsed().as_millis() as u64;
        if output.status.success() {
            Ok(ExecutionResult::success(&req.request_id, format!("[app:open] OK: {} launched", app_name), Some(0), elapsed))
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Ok(ExecutionResult::success(&req.request_id, format!("[app:open] ERROR: {}", stderr), output.status.code(), elapsed))
        }
    }

    fn execute_url_open(req: &ExecutionRequest, start: std::time::Instant) -> Result<ExecutionResult, ExecutionResult> {
        let url = req.args["url"].as_str().unwrap_or("");
        let output = std::process::Command::new("open")
            .arg(url)
            .output()
            .map_err(|e| ExecutionResult::gate3_denied(&req.request_id, &format!("url_open failed: {}", e)))?;

        let elapsed = start.elapsed().as_millis() as u64;
        if output.status.success() {
            Ok(ExecutionResult::success(&req.request_id, format!("[url:open] OK: {} opened in browser", url), Some(0), elapsed))
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Ok(ExecutionResult::success(&req.request_id, format!("[url:open] ERROR: {}", stderr), output.status.code(), elapsed))
        }
    }

    fn execute_applescript(req: &ExecutionRequest, start: std::time::Instant) -> Result<ExecutionResult, ExecutionResult> {
        let script = req.args["script"].as_str().unwrap_or("");
        let home = dirs::home_dir()
            .ok_or_else(|| ExecutionResult::gate3_denied(&req.request_id, "Cannot determine home directory"))?;
        let ageex_tmp = home.join(".ageex").join("tmp");
        fs::create_dir_all(&ageex_tmp)
            .map_err(|e| ExecutionResult::gate3_denied(&req.request_id, &format!("ageex tmp dir creation failed: {}", e)))?;
        let tmp_path = ageex_tmp.join(format!("ageex_as_{}.applescript", uuid::Uuid::new_v4()));
        fs::write(&tmp_path, script.as_bytes())
            .map_err(|e| ExecutionResult::gate3_denied(&req.request_id, &format!("applescript temp write failed: {}", e)))?;

        #[cfg(unix)]
        {
            if let Err(e) = std::fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o400)) {
                let _ = fs::remove_file(&tmp_path);
                return Err(ExecutionResult::gate3_denied(&req.request_id, &format!("applescript perm lock failed: {}", e)));
            }
        }

        let output = std::process::Command::new("osascript")
            .arg(&tmp_path)
            .output()
            .map_err(|e| {
                let _ = fs::remove_file(&tmp_path);
                ExecutionResult::gate3_denied(&req.request_id, &format!("applescript execution failed: {}", e))
            })?;

        let _ = fs::remove_file(&tmp_path);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let combined = format!("{}{}", stdout, stderr);
        let elapsed = start.elapsed().as_millis() as u64;
        Ok(ExecutionResult::success(&req.request_id, combined, output.status.code(), elapsed))
    }

    fn execute_net_curl(req: &ExecutionRequest, start: std::time::Instant) -> Result<ExecutionResult, ExecutionResult> {
        let url = req.args["url"].as_str().unwrap_or("");
        let method = req.args["method"].as_str().unwrap_or("GET");
        let body = req.args["body"].as_str().unwrap_or("");
        let headers = req.args["headers"].as_str().unwrap_or("");
        let timeout_secs = req.args["timeout_secs"].as_str().unwrap_or("30");
        let max_bytes = req.args["max_bytes"].as_str().unwrap_or("524288");

        let mut cmd = std::process::Command::new("curl");
        cmd.arg("--silent").arg("--fail-with-body")
            .arg("--max-time").arg(timeout_secs)
            .arg("--max-filesize").arg(max_bytes)
            .arg("--max-redirs").arg("5")
            .arg("-X").arg(method);

        for header_line in headers.lines().filter(|l| !l.trim().is_empty()) {
            let parts: Vec<&str> = header_line.splitn(2, ':').collect();
            if parts.len() == 2 {
                let key = parts[0].trim().replace(['\r', '\n'], "");
                let val = parts[1].trim().replace(['\r', '\n'], "");
                cmd.arg("-H").arg(format!("{}: {}", key, val));
            }
        }

        if !body.is_empty() {
            cmd.arg("--data-raw").arg(body);
        }
        cmd.arg(url);

        let output = cmd.output()
            .map_err(|e| ExecutionResult::gate3_denied(&req.request_id, &format!("curl execution failed: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let combined = format!("{}{}", stdout, stderr);
        let elapsed = start.elapsed().as_millis() as u64;
        Ok(ExecutionResult::success(&req.request_id, combined, output.status.code(), elapsed))
    }

    fn execute_inject_text(req: &ExecutionRequest, start: std::time::Instant) -> Result<ExecutionResult, ExecutionResult> {
        let text = req.args["text"].as_str().unwrap_or("");
        let _expected_app = req.args["expected_app_name"].as_str().unwrap_or("");

        use arboard::Clipboard;
        use enigo::{Enigo, Key, KeyboardControllable};

        let mut clipboard = Clipboard::new()
            .map_err(|e| ExecutionResult::gate3_denied(&req.request_id, &format!("clipboard init failed: {}", e)))?;

        let original = clipboard.get_text().unwrap_or_default();

        clipboard.set_text(text)
            .map_err(|e| ExecutionResult::gate3_denied(&req.request_id, &format!("clipboard write failed: {}", e)))?;

        std::thread::sleep(std::time::Duration::from_millis(50));

        let mut enigo = Enigo::new();
        enigo.key_down(Key::Meta);
        enigo.key_click(Key::Layout('v'));
        enigo.key_up(Key::Meta);

        std::thread::sleep(std::time::Duration::from_millis(100));

        let _ = clipboard.set_text(&original);

        let elapsed = start.elapsed().as_millis() as u64;
        Ok(ExecutionResult::success(&req.request_id, format!("Injected {} bytes into active application.", text.len()), Some(0), elapsed))
    }

    fn execute_fs_exists(req: &ExecutionRequest, start: std::time::Instant) -> Result<ExecutionResult, ExecutionResult> {
        let raw = req.args["path"].as_str().unwrap_or("");
        let normalized = normalize_workspace_path(raw);
        let path = Self::resolve_path(&normalized, &req.working_dir);
        let exists = path.exists();
        let elapsed = start.elapsed().as_millis() as u64;
        Ok(ExecutionResult::success(&req.request_id, format!("[fs:exists] {} → {}", raw, exists), None, elapsed))
    }

    fn execute_fs_delete(req: &ExecutionRequest, start: std::time::Instant) -> Result<ExecutionResult, ExecutionResult> {
        let raw = req.args["path"].as_str().unwrap_or("");
        let normalized = normalize_workspace_path(raw);
        let path = Self::resolve_path(&normalized, &req.working_dir);

        let trash_dir = dirs::home_dir()
            .ok_or_else(|| ExecutionResult::gate3_denied(&req.request_id, "Cannot determine home directory"))?
            .join(".ageex").join("trash");
        fs::create_dir_all(&trash_dir)
            .map_err(|e| ExecutionResult::gate3_denied(&req.request_id, &format!("trash dir creation failed: {}", e)))?;

        let original_name = path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let trash_name = format!("{}_{}", uuid::Uuid::new_v4(), original_name);
        let trash_path = trash_dir.join(&trash_name);

        fs::rename(&path, &trash_path)
            .map_err(|e| ExecutionResult::gate3_denied(&req.request_id, &format!("Delete failed: {}", e)))?;

        let elapsed = start.elapsed().as_millis() as u64;
        Ok(ExecutionResult::success(&req.request_id, format!("[fs:delete] Deleted {} (moved to trash: {:?})", path.display(), trash_path), None, elapsed))
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

        let home = dirs::home_dir().context("Cannot determine home directory")?;
        let ageex_tmp = home.join(".ageex").join("tmp");
        fs::create_dir_all(&ageex_tmp)?;
        let temp = ageex_tmp.join(format!("ageex-{}.sb", uuid::Uuid::new_v4()));
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
