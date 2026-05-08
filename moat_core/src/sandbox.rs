// moat_core/src/sandbox.rs
//! Dynamic macOS Seatbelt (sb) profile generation and sandboxed execution.

use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use std::{
    fmt::Write as _,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::config;

// ─── Error / Result types ────────────────────────────────────────────────────

#[derive(Debug)]
pub enum SandboxError {
    PolicyViolation {
        stderr: String,
    },
    ExecutableNotFound(io::Error),
    CommandFailed {
        exit_code: Option<i32>,
        stderr: String,
    },
    Io(io::Error),
    TimedOut,
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PolicyViolation { stderr } => write!(f, "Seatbelt policy violation: {stderr}"),
            Self::ExecutableNotFound(e) => write!(f, "sandbox-exec not found: {e}"),
            Self::CommandFailed { exit_code, stderr } => {
                write!(f, "command failed (exit {:?}): {}", exit_code, stderr)
            }
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::TimedOut => write!(f, "command timed out and process was killed"),
        }
    }
}
impl std::error::Error for SandboxError {}

pub type Result<T> = std::result::Result<T, SandboxError>;

// ─── Sandbox policy violations ───────────────────────────────────────────────

const SEATBELT_DENIAL_MARKERS: &[&str] = &[
    "Operation not permitted",
    "Permission denied",
    "sandbox violation",
    "deny file-read",
    "deny file-write",
    "deny network-outbound",
    "deny network-inbound",
    "deny process-exec",
    "deny mach-lookup",
    "deny sysctl-read",
];

fn classify_exit(exit_code: Option<i32>, stderr: &str) -> Result<()> {
    if exit_code == Some(0) {
        return Ok(());
    }
    let lower = stderr.to_lowercase();
    if SEATBELT_DENIAL_MARKERS
        .iter()
        .any(|m| lower.contains(&m.to_lowercase()))
    {
        Err(SandboxError::PolicyViolation {
            stderr: stderr.to_owned(),
        })
    } else {
        Err(SandboxError::CommandFailed {
            exit_code,
            stderr: stderr.to_owned(),
        })
    }
}

// ─── Access mode flags & Input types ────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAccess {
    ReadOnly,
    ReadWrite,
    WriteOnly,
}

impl FileAccess {
    fn sb_operations(self) -> &'static str {
        match self {
            Self::ReadOnly => "file-read*",
            Self::ReadWrite => "file-read* file-write*",
            Self::WriteOnly => "file-write*",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PathRule {
    pub path: String,
    pub recursive: bool,
    pub access: FileAccess,
}

impl PathRule {
    pub fn read_only(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            recursive: false,
            access: FileAccess::ReadOnly,
        }
    }
    pub fn read_only_recursive(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            recursive: true,
            access: FileAccess::ReadOnly,
        }
    }
    pub fn read_write_recursive(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            recursive: true,
            access: FileAccess::ReadWrite,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NetworkRule {
    pub host: String,
    pub port: Option<u16>,
}

impl NetworkRule {
    pub fn host_port(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port: Some(port),
        }
    }
}

// ─── Input validation ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ValidationError {
    InvalidPath(String),
    InvalidHost(String),
}
impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath(p) => write!(f, "invalid path for sandbox profile: {p:?}"),
            Self::InvalidHost(h) => write!(f, "invalid host for sandbox profile: {h:?}"),
        }
    }
}
impl std::error::Error for ValidationError {}

fn validate_path(path: &str) -> std::result::Result<(), ValidationError> {
    if path.is_empty() || !path.starts_with('/') {
        return Err(ValidationError::InvalidPath(path.to_owned()));
    }
    const BLOCKED: &[char] = &['"', '\\', '\0', '\n', '\r', '(', ')'];
    if path.chars().any(|c| BLOCKED.contains(&c)) || path.split('/').any(|seg| seg == "..") {
        return Err(ValidationError::InvalidPath(path.to_owned()));
    }
    Ok(())
}

fn validate_host(host: &str) -> std::result::Result<(), ValidationError> {
    if host.is_empty() {
        return Err(ValidationError::InvalidHost(host.to_owned()));
    }
    const BLOCKED: &[char] = &['"', '\\', '\0', '\n', '\r', '(', ')'];
    if host.chars().any(|c| BLOCKED.contains(&c)) {
        return Err(ValidationError::InvalidHost(host.to_owned()));
    }
    Ok(())
}

// ─── Profile builder ─────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct SandboxProfileBuilder {
    path_rules: Vec<PathRule>,
    network_rules: Vec<NetworkRule>,
}

impl SandboxProfileBuilder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn add_path_rules(mut self, rules: impl IntoIterator<Item = PathRule>) -> Self {
        self.path_rules.extend(rules);
        self
    }
    pub fn add_network_rules(mut self, rules: impl IntoIterator<Item = NetworkRule>) -> Self {
        self.network_rules.extend(rules);
        self
    }

    pub fn build(&self) -> std::result::Result<String, ValidationError> {
        for rule in &self.path_rules {
            validate_path(&rule.path)?;
        }
        for rule in &self.network_rules {
            validate_host(&rule.host)?;
        }

        let mut sb = String::with_capacity(1024);
        sb.push_str("(version 1)\n(deny default)\n\n;; process machinery\n");
        sb.push_str("(allow process-fork)\n(allow process-exec)\n");
        sb.push_str("(allow signal (target self))\n(allow sysctl-read)\n\n");

        sb.push_str(";; EXPLICIT GLOBAL DENY: Prevent deletion always\n");
        sb.push_str("(deny file-write-unlink)\n\n");

        sb.push_str(";; EXPLICIT GLOBAL ALLOW: Read access allowed by default\n");
        sb.push_str("(allow file-read*)\n\n");

        sb.push_str(";; dynamic linker and system frameworks\n");
        for p in &[
            "/usr/lib",
            "/usr/libexec",
            "/System/Library/Frameworks",
            "/System/Library/PrivateFrameworks",
            "/System/Library/Extensions",
            "/private/var/db/dyld",
            "/System/Volumes/Preboot",
            "/usr/share/zoneinfo",
            "/private/etc",
            "/dev/null",
            "/dev/urandom",
            "/dev/random",
        ] {
            let _ = writeln!(sb, "(allow file-read* (subpath \"{p}\"))");
        }
        sb.push('\n');

        if !self.path_rules.is_empty() {
            sb.push_str(";; path whitelist\n");
            for rule in &self.path_rules {
                let ops = rule.access.sb_operations();
                let directive = if rule.recursive { "subpath" } else { "literal" };
                let _ = writeln!(sb, "(allow {ops} ({directive} \"{}\"))", rule.path);
            }
            sb.push('\n');
        }

        if !self.network_rules.is_empty() {
            sb.push_str(";; network egress whitelist\n(allow network-outbound\n");
            for rule in &self.network_rules {
                if let Some(port) = rule.port {
                    let _ = writeln!(sb, "  (remote tcp \"{}:{port}\")", rule.host);
                } else {
                    let _ = writeln!(
                        sb,
                        "  (remote tcp \"{}:*\")\n  (remote udp \"{}:*\")",
                        rule.host, rule.host
                    );
                }
            }
            sb.push_str(")\n\n;; DNS resolver\n");
            sb.push_str("(allow network-outbound (remote udp \"*:53\") (remote tcp \"*:53\"))\n");
            sb.push_str("(allow network-outbound (local unix-socket (path-prefix \"/private/var/run/mDNSResponder\")))\n\n");
        }
        Ok(sb)
    }
}

// ─── Sandbox input / output ──────────────────────────────────────────────────

#[derive(Debug)]
pub struct SandboxedCommand<'a> {
    pub binary: &'a str,
    pub args: &'a [String],
    pub profile: &'a str,
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct SandboxOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

// ─── Executor ────────────────────────────────────────────────────────────────

pub fn execute(spec: &SandboxedCommand<'_>) -> Result<SandboxOutput> {
    let mut cmd = Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-p")
        .arg(spec.profile)
        .arg(spec.binary)
        .args(spec.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            SandboxError::ExecutableNotFound(e)
        } else {
            SandboxError::Io(e)
        }
    })?;

    let Some(mut out_pipe) = child.stdout.take() else {
        return Err(SandboxError::Io(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "sandbox stdout pipe missing",
        )));
    };
    let Some(mut err_pipe) = child.stderr.take() else {
        return Err(SandboxError::Io(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "sandbox stderr pipe missing",
        )));
    };

    let out_thread = thread::spawn(move || {
        let mut s = String::new();
        let _ = out_pipe.read_to_string(&mut s);
        s
    });
    let err_thread = thread::spawn(move || {
        let mut s = String::new();
        let _ = err_pipe.read_to_string(&mut s);
        s
    });

    let status = match spec.timeout {
        None => child.wait().map_err(SandboxError::Io)?,
        Some(deadline) => {
            let start = Instant::now();
            loop {
                if let Some(status) = child.try_wait().map_err(SandboxError::Io)? {
                    break status;
                }
                if start.elapsed() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(SandboxError::TimedOut);
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    };

    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
    let code = status.code().unwrap_or(-1);

    classify_exit(Some(code), &stderr)?; // ✅ FIX APPLIED

    Ok(SandboxOutput {
        stdout,
        stderr,
        exit_code: code,
    })
}

pub struct WorkspaceCapability {
    dir: Dir,
    root: PathBuf,
}

impl WorkspaceCapability {
    pub fn open(workspace_path: &Path) -> Result<Self> {
        let dir =
            Dir::open_ambient_dir(workspace_path, ambient_authority()).map_err(SandboxError::Io)?;
        Ok(Self {
            dir,
            root: workspace_path.to_path_buf(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn create_dir_all(&self, rel_path: &Path) -> Result<()> {
        if rel_path.as_os_str().is_empty() || rel_path == Path::new(".") {
            return Ok(());
        }
        self.dir.create_dir_all(rel_path).map_err(SandboxError::Io)
    }

    pub fn read_to_string(&self, rel_path: &Path) -> Result<String> {
        self.dir.read_to_string(rel_path).map_err(SandboxError::Io)
    }

    pub fn atomic_write(&self, rel_path: &Path, data: &[u8]) -> Result<()> {
        let mut tmp_name = rel_path.as_os_str().to_owned();
        tmp_name.push(".tmp");
        let tmp_path = Path::new(&tmp_name);

        if let Some(parent) = rel_path.parent() {
            self.create_dir_all(parent)?;
        }

        {
            let mut file = self
                .dir
                .open_with(
                    tmp_path,
                    OpenOptions::new().write(true).create(true).truncate(true),
                )
                .map_err(SandboxError::Io)?;
            file.write_all(data).map_err(SandboxError::Io)?;
            file.sync_all().map_err(SandboxError::Io)?;
        }

        self.dir
            .rename(tmp_path, &self.dir, rel_path)
            .map_err(SandboxError::Io)?;
        Ok(())
    }
}

pub fn cleanup_workspace_tmp_files() -> Result<()> {
    fn walk(dir: &Path) -> io::Result<()> {
        if !dir.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                walk(&path)?;
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("tmp") {
                let _ = fs::remove_file(&path);
            }
        }
        Ok(())
    }

    walk(&config::workspace_root()).map_err(SandboxError::Io)
}
