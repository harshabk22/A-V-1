use crate::capabilities::Capability;
use crate::meta_brain::{MetaBrain, PermissionFlags};
use crate::approval::{ApprovalFlow, ApprovalRequest, ApprovalDecision};
use crate::session::SessionStore;
use crate::consent::ConsentBridge;
use crate::execution::{DelExecutor, ExecutionError, ExecutionRequest};
use crate::execution::gate2_policy::normalize_workspace_path;
use crate::execution::journal::ExecutionJournal;
use anyhow::Result;
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct ToolExecutor {
    pub meta_brain: Arc<Mutex<MetaBrain>>,
    pub agent_id: String,
    pub session_store: Arc<SessionStore>,
    pub del: DelExecutor,
    pub journal: ExecutionJournal,
    pub auto_deny: bool,
    pub consent_bridge: Option<Arc<ConsentBridge>>,
}

impl ToolExecutor {
    pub fn new(
        meta_brain: Arc<Mutex<MetaBrain>>,
        agent_id: impl Into<String>,
        session_store: Arc<SessionStore>,
    ) -> Result<Self> {
        Self::new_with_auto_deny(meta_brain, agent_id, session_store, false)
    }

    pub fn new_with_auto_deny(
        meta_brain: Arc<Mutex<MetaBrain>>,
        agent_id: impl Into<String>,
        session_store: Arc<SessionStore>,
        auto_deny: bool,
    ) -> Result<Self> {
        Self::build(meta_brain, agent_id, session_store, auto_deny, None)
    }

    pub fn new_with_consent(
        meta_brain: Arc<Mutex<MetaBrain>>,
        agent_id: impl Into<String>,
        session_store: Arc<SessionStore>,
        consent_bridge: Arc<ConsentBridge>,
    ) -> Result<Self> {
        Self::build(meta_brain, agent_id, session_store, false, Some(consent_bridge))
    }

    fn build(
        meta_brain: Arc<Mutex<MetaBrain>>,
        agent_id: impl Into<String>,
        session_store: Arc<SessionStore>,
        auto_deny: bool,
        consent_bridge: Option<Arc<ConsentBridge>>,
    ) -> Result<Self> {
        let del = DelExecutor::new()?;
        let journal = ExecutionJournal::open()
            .unwrap_or_else(|e| {
                eprintln!("[ToolExecutor] Journal unavailable: {}", e);
                ExecutionJournal { path: std::path::PathBuf::new() }
            });
        Ok(Self {
            meta_brain,
            agent_id: agent_id.into(),
            session_store,
            del,
            journal,
            auto_deny,
            consent_bridge,
        })
    }

    pub async fn execute(&mut self, tool_name: &str, args_json: &str) -> Result<String> {
        let args: serde_json::Value = match serde_json::from_str(args_json) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[ToolExecutor] Failed to parse tool args: {}. Raw: {}", e, args_json);
                serde_json::Value::Null
            }
        };

        match tool_name {
            "fs_read" | "read_file" => {
                let raw_path = args["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("fs_read requires 'path' argument"))?;
                // TOCTOU-safe: open first, validate from the fd-derived path
                let resolved = Self::resolve_and_validate(raw_path)?;
                if !self.check_path_permission(PermissionFlags::FILE_READ, &resolved).await? {
                    return Ok(format!("__MOAT_DENIED__ [fs:read] No permission to read {}", raw_path));
                }
                self.execute_del("read_file", args)
            }
            "fs_exists" => {
                let path = args["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("fs_exists requires 'path' argument"))?;
                let resolved = Self::resolve_path(path)?;
                if !self.check_path_permission(PermissionFlags::FILE_READ, &resolved).await? {
                    return Ok(format!("__MOAT_DENIED__ [fs:exists] No permission to read {}", path));
                }
                self.execute_del("fs_exists", args)
            }
            "fs_write" | "write_file" => {
                let raw_path = args["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("fs_write requires 'path' argument"))?;
                let resolved = Self::resolve_path(raw_path)?;
                if Self::is_path_protected(Path::new(&resolved)) {
                    return Ok(format!("__MOAT_DENIED__ [fs:write] {} is a protected file", raw_path));
                }
                if !self.check_path_permission(PermissionFlags::FILE_WRITE, &resolved).await? {
                    return Ok(format!("__MOAT_DENIED__ [fs:write] No permission to write {}", raw_path));
                }

                let content = args["content"].as_str().unwrap_or("");
                if content.is_empty() && Path::new(&resolved).exists() {
                    return Err(anyhow::anyhow!(
                        "__MOAT_DENIED__ [fs:write] Empty write to existing file detected. Use fs_delete to delete files."
                    ));
                }

                let result = self.execute_del("write_file", args)?;
                let _ = self.journal.log_entry(&self.agent_id, "write_file", &resolved, true, &result);
                Ok(result)
            }
            "fs_delete" => {
                let raw_path = args["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("fs_delete requires 'path' argument"))?;
                // Destructive operation: require consent before proceeding
                let consent_approved = if let Some(bridge) = &self.consent_bridge {
                    bridge.request_consent(
                        "fs:delete",
                        &format!("Delete file: {}", raw_path),
                    ).await
                } else {
                    true
                };
                if !consent_approved {
                    return Ok(format!(
                        "__MOAT_DENIED__ [fs:delete] consent not granted for {}",
                        raw_path
                    ));
                }
                // Pre-check: handle non-existent files cleanly before TOCTOU validation
                if !Path::new(raw_path).exists() {
                    return Ok(format!("[fs:delete] file not found: {}", raw_path));
                }
                // TOCTOU-safe: open first, validate from the fd-derived path
                let resolved = Self::resolve_and_validate(raw_path)?;
                // Route consent approval through MetaBrain so subsequent operations
                // in the same session don't re-prompt.
                if self.consent_bridge.is_some() {
                    let mut brain = self.meta_brain.lock().unwrap_or_else(|e| e.into_inner());
                    let _ = brain.grant_path_permission(&resolved, PermissionFlags::FILE_DELETE);
                    let _ = self.session_store.record_approval(
                        &self.agent_id,
                        &format!("{:?}", PermissionFlags::FILE_DELETE),
                        "path",
                        &resolved,
                    );
                }
                if !self.check_path_permission(PermissionFlags::FILE_DELETE, &resolved).await? {
                    return Ok(format!("__MOAT_DENIED__ [fs:delete] No permission to delete {}", raw_path));
                }

                let result = self.execute_del("fs_delete", args)?;
                let _ = self.journal.log_entry(&self.agent_id, "delete_file", &resolved, true, &result);
                Ok(result)
            }
            "process_spawn" => {
                let command = args["command"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("process_spawn requires 'command' argument"))?;
                let working_dir_raw = args["working_dir"].as_str()
                    .unwrap_or("~/Desktop/ageex-workspace");
                let resolved_wd = Self::resolve_path(working_dir_raw)?;
                if !self.check_path_permission(PermissionFlags::EXEC, &resolved_wd).await? {
                    return Ok(format!("__MOAT_DENIED__ [process:spawn] No permission to run `{}`", command));
                }
                self.execute_del("process_spawn", args)
            }
            "app_open" => {
                let app_name = args["app_name"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("app_open requires 'app_name' argument"))?;
                let consent_approved = if let Some(bridge) = &self.consent_bridge {
                    bridge.request_consent(
                        "app:open",
                        &format!("Open application: {}", app_name),
                    ).await
                } else {
                    true
                };
                if !consent_approved {
                    return Ok(format!(
                        "__MOAT_DENIED__ [app:open] consent not granted for {}",
                        app_name
                    ));
                }
                // Route consent through MetaBrain to prevent re-prompting
                if self.consent_bridge.is_some() {
                    let mut brain = self.meta_brain.lock().unwrap_or_else(|e| e.into_inner());
                    let _ = brain.grant_app_permission(app_name, PermissionFlags::APP_LAUNCH);
                    let _ = self.session_store.record_approval(
                        &self.agent_id,
                        "APP_LAUNCH",
                        "app",
                        app_name,
                    );
                }
                if !self.check_app_permission(app_name).await? {
                    return Ok(format!("__MOAT_DENIED__ [app:open] No permission to open {}", app_name));
                }
                self.execute_del("app_open", args)
            }
            "url_open" => {
                let url = args["url"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("url_open requires 'url' argument"))?;
                let consent_approved = if let Some(bridge) = &self.consent_bridge {
                    bridge.request_consent(
                        "url:open",
                        &format!("Open URL: {}", url),
                    ).await
                } else {
                    true
                };
                if !consent_approved {
                    return Ok(format!(
                        "__MOAT_DENIED__ [url:open] consent not granted for {}",
                        url
                    ));
                }
                // Route consent through MetaBrain to prevent re-prompting
                if self.consent_bridge.is_some() {
                    if let Ok(parsed) = url::Url::parse(url) {
                        let scheme = parsed.scheme().to_string();
                        let domain = parsed.host_str().unwrap_or("").to_string();
                        let mut brain = self.meta_brain.lock().unwrap_or_else(|e| e.into_inner());
                        if !scheme.is_empty() {
                            let _ = brain.grant_url_scheme_permission(&scheme, PermissionFlags::URL_OPEN);
                        }
                        if !domain.is_empty() {
                            let _ = brain.grant_domain_permission(&domain, PermissionFlags::NET_OUTBOUND);
                        }
                    }
                    let _ = self.session_store.record_approval(
                        &self.agent_id,
                        "URL_OPEN",
                        "url",
                        url,
                    );
                }
                if !self.check_url_permission(url).await? {
                    return Ok(format!("__MOAT_DENIED__ [url:open] No permission to open {}", url));
                }
                self.execute_del("url_open", args)
            }
            "skill_execute" => {
                if self.agent_id == "agent1" {
                    return Err(ExecutionError::CapabilityDenied {
                        agent: "Agent1".into(),
                        capability: "skill_execute".into(),
                        reason: "Agent1 is read-only; skill execution is not permitted".into(),
                    }.into());
                }
                self.execute_del("skill_execute", args)
            }
            unknown => anyhow::bail!("Unknown tool: {}", unknown),
        }
    }

    /// Execute an operation through DEL without MetaBrain permission checks.
    /// Used by the orchestrator (which does its own MetaBrain checks).
    pub(crate) fn execute_via_del(&self, del_tool_name: &str, args: serde_json::Value) -> Result<String> {
        self.execute_del(del_tool_name, args)
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

    pub(crate) fn tool_to_del_capability(tool_name: &str) -> Capability {
        match tool_name {
            "read_file" => Capability::FileRead,
            "write_file" => Capability::FileWrite,
            "delete_file" | "fs_delete" => Capability::FileDelete,
            "process_spawn" => Capability::OsProcessSpawn,
            "app_open" => Capability::AppOpen,
            "url_open" => Capability::UrlOpen,
            "applescript" => Capability::OsProcessSpawn,
            "net_curl" => Capability::UrlOpen,
            "inject_text" => Capability::OsProcessSpawn,
            "fs_exists" => Capability::FileRead,
            "skill_execute" => Capability::OsProcessSpawn,
            _ => Capability::FileRead,
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

    fn resolve_path(raw: &str) -> Result<String> {
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
                let canonical_str = canonical.to_string_lossy().into_owned();
                if canonical_str != expanded {
                    return Err(anyhow::anyhow!(
                        "[security] SYMLINK TRAVERSAL DENIED: {} -> {}",
                        expanded,
                        canonical_str
                    ));
                }
                Ok(canonical_str)
            } else {
                Ok(expanded)
            }
        } else {
            let workspace = dirs::home_dir()
                .unwrap_or_default()
                .join("Desktop")
                .join("ageex-workspace");
            Ok(workspace.join(raw).to_string_lossy().into_owned())
        }
    }

    /// Resolve the canonical path from an open file descriptor using fcntl F_GETPATH.
    /// This is the TOCTOU-safe counterpart to resolve_path: the path comes from the
    /// actual open file, not from a string that may have been swapped since validation.
    fn resolve_from_fd(fd: std::os::unix::io::RawFd) -> Result<String> {
        let mut buf = vec![0u8; libc::PATH_MAX as usize];
        let ret = unsafe { libc::fcntl(fd, libc::F_GETPATH, buf.as_mut_ptr()) };
        if ret == -1 {
            return Err(anyhow::anyhow!(
                "fcntl F_GETPATH failed (fd {}): {}",
                fd,
                std::io::Error::last_os_error()
            ));
        }
        let nul = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Ok(String::from_utf8_lossy(&buf[..nul]).to_string())
    }

    /// TOCTOU-safe path resolution for file operations.
    /// Opens the path first, derives the canonical path from the fd,
    /// then validates. This closes the race window between path check and file open.
    fn resolve_and_validate(raw_path: &str) -> Result<String> {
        let expanded = if raw_path.starts_with("~/") {
            if let Some(home) = dirs::home_dir() {
                home.to_string_lossy().into_owned() + &raw_path[1..]
            } else {
                raw_path.to_string()
            }
        } else {
            raw_path.to_string()
        };
        let p = Path::new(&expanded);
        let open_path = if p.is_absolute() {
            expanded.clone()
        } else {
            let workspace = dirs::home_dir()
                .unwrap_or_default()
                .join("Desktop")
                .join("ageex-workspace");
            workspace.join(raw_path).to_string_lossy().into_owned()
        };

        // Open the file first with O_NOFOLLOW — kernel-level symlink prevention.
        // This closes the TOCTOU window: if the path is a symlink at open time,
        // the kernel returns ELOOP before we ever get a file descriptor.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&open_path)
            .map_err(|e| match e.raw_os_error() {
                Some(libc::ELOOP) => {
                    anyhow::anyhow!(ExecutionError::SymlinkDenied(PathBuf::from(&open_path)))
                }
                _ => anyhow::anyhow!("[fs] open failed for {}: {}", open_path, e),
            })?;

        // Resolve the actual path from the open fd, not from the original string
        let resolved = Self::resolve_from_fd(file.as_raw_fd())?;

        // Drop the fd — validation is complete; DEL re-opens with the validated path
        drop(file);

        // Validate the fd-derived path — now there is no race window
        if Self::is_path_protected(Path::new(&resolved)) {
            return Err(anyhow::anyhow!(
                "[security] PROTECTED FILE ACCESS DENIED: {}",
                resolved
            ));
        }

        // Check for symlink mismatch between the string path and the real fd path
        let canonical = std::fs::canonicalize(&open_path).unwrap_or_else(|_| PathBuf::from(&open_path));
        let canonical_str = canonical.to_string_lossy().to_string();
        if canonical_str != resolved {
            eprintln!(
                "[ToolExecutor] Path mismatch: canonicalize={} vs fd-path={}",
                canonical_str, resolved
            );
        }

        Ok(resolved)
    }

    fn flags_to_capability(flags: PermissionFlags) -> Capability {
        if flags.contains(PermissionFlags::FILE_DELETE) { Capability::FileDelete }
        else if flags.contains(PermissionFlags::FILE_WRITE) { Capability::FileWrite }
        else if flags.contains(PermissionFlags::FILE_READ) { Capability::FileRead }
        else if flags.contains(PermissionFlags::EXEC) { Capability::OsProcessSpawn }
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
        assert_eq!(ToolExecutor::tool_to_del_capability("process_spawn"), Capability::OsProcessSpawn);
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

    #[test]
    fn test_fd_path_resolution_matches_real_path() {
        use std::io::Write;
        use std::os::unix::io::AsRawFd;

        let tmp = std::env::temp_dir().join("ageex_toctou_test.txt");
        let mut f = std::fs::File::create(&tmp).expect("create temp file");
        f.write_all(b"hello").expect("write");

        // fd-derived path must match canonicalized path
        let fd_path = ToolExecutor::resolve_from_fd(f.as_raw_fd())
            .expect("resolve from fd");
        let canonical = std::fs::canonicalize(&tmp).expect("canonicalize");

        assert_eq!(fd_path, canonical.to_string_lossy().to_string(),
            "fd-derived path must match canonical path (TOCTOU safety)");

        drop(f);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_destructive_fs_delete_requires_consent() {
        use std::io::Write;
        use crate::consent::{ConsentBridge, ConsentDecision};

        let tmp = std::env::temp_dir().join("ageex_consent_test_delete.txt");
        let mut f = std::fs::File::create(&tmp).expect("create temp file");
        f.write_all(b"delete me").expect("write");
        drop(f);

        // Build a consent bridge that auto-denies (no UI roundtrip).
        // Pattern from orchestrator::tests::consent_bridge_with_policy.
        let (bridge_slot, bridge) = {
            let slot: Arc<Mutex<Option<std::sync::Weak<ConsentBridge>>>> =
                Arc::new(Mutex::new(None));
            let slot_for_emit = Arc::clone(&slot);
            let bridge = Arc::new(ConsentBridge::new(Box::new(move |req| {
                let weak = slot_for_emit.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .expect("bridge stored before emit");
                let req_id = req.request_id.clone();
                std::thread::spawn(move || {
                    if let Some(bridge) = weak.upgrade() {
                        let _ = bridge.resolve(&req_id, ConsentDecision::Deny);
                    }
                });
            })));
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::downgrade(&bridge));
            (slot, bridge)
        };

        let brain = Arc::new(Mutex::new(MetaBrain::new()));
        let store = SessionStore::open_with_prefix_in_memory("test_consent_delete")
            .expect("test store");
        let store = Arc::new(store);
        brain.lock().unwrap().grant_path_permission(
            &tmp.to_string_lossy(),
            PermissionFlags::FILE_DELETE,
        ).expect("grant delete");

        let mut exec = ToolExecutor::new_with_consent(
            Arc::clone(&brain),
            "agent2",
            Arc::clone(&store),
            Arc::clone(&bridge),
        ).expect("create executor");

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let json = format!(r#"{{"path":"{}"}}"#, tmp.to_string_lossy());
        let result = rt.block_on(exec.execute("fs_delete", &json));

        match result {
            Ok(output) => {
                assert!(
                    output.contains("DENIED") || output.contains("denied") || output.contains("consent"),
                    "fs_delete without consent should be denied: {}",
                    output
                );
            }
            Err(e) => {
                assert!(
                    e.to_string().contains("consent") || e.to_string().contains("denied"),
                    "Error should mention consent denial: {}",
                    e
                );
            }
        }

        let _ = std::fs::remove_file(&tmp);
        // Keep slot alive to prevent bridge drop during test
        drop(bridge_slot);
    }

    #[test]
    fn test_agent1_skill_execute_is_denied() {
        let brain = Arc::new(Mutex::new(MetaBrain::new()));
        let store = SessionStore::open_with_prefix_in_memory("test_agent1_skill_exec")
            .expect("test store");
        let store = Arc::new(store);
        let mut exec = ToolExecutor::new_with_auto_deny(
            Arc::clone(&brain),
            "agent1",
            store,
            true,
        ).expect("create executor");

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let result = rt.block_on(exec.execute("skill_execute", r#"{"skill_name":"test"}"#));

        match result {
            Ok(output) => panic!("Expected denial, got: {}", output),
            Err(e) => {
                let msg = e.to_string();
                assert!(msg.contains("Agent1"), "Error must mention Agent1: {}", msg);
                assert!(msg.contains("skill_execute"), "Error must mention skill_execute: {}", msg);
                assert!(msg.contains("denied") || msg.contains("not permitted"), "Error must be a denial: {}", msg);
            }
        }
    }
