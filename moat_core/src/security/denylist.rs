use std::path::Path;

/// Blocklist of sensitive paths that must never be written to,
/// regardless of MetaBrain permissions or consent approval.
///
/// Pattern: hermes-agent `file_safety.rs:build_write_denied_paths()`.
const SENSITIVE_WRITE_PATTERNS: &[(&str, &str)] = &[
    // SSH keys
    (".ssh/id_rsa", "private SSH key"),
    (".ssh/id_ed25519", "private Ed25519 key"),
    (".ssh/id_ecdsa", "private ECDSA key"),
    (".ssh/authorized_keys", "authorized SSH keys"),
    // Shell configuration
    (".bashrc", "bash shell config"),
    (".zshrc", "zsh shell config"),
    (".bash_profile", "bash profile"),
    (".profile", "shell profile"),
    // Environment and secrets
    (".env", "environment variables"),
    (".env.local", "local environment variables"),
    (".env.production", "production environment"),
    ("credentials.json", "credential file"),
    ("service-account.json", "service account key"),
    // System security
    ("/etc/sudoers", "sudoers file"),
    ("/etc/passwd", "password file"),
    ("/etc/shadow", "shadow password file"),
    ("/etc/hosts", "hosts file"),
    // Git security
    (".git/config", "git config"),
    (".git/hooks", "git hooks directory"),
    // Certificate/private key
    ("*.pem", "PEM certificate/key"),
    ("*.key", "private key file"),
    // Database files
    ("*.db", "database file"),
    ("*.sqlite", "SQLite database file"),
    (".ageex/sessions.db", "session database"),
    (".ageex/audit.db", "audit database"),
];

/// Check if a path is on the write denylist.
/// Returns Some(description) if blocked, None if allowed.
pub fn is_write_denied(path: &Path) -> Option<&'static str> {
    let path_str = path.to_string_lossy();
    let file_name = path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    for (pattern, description) in SENSITIVE_WRITE_PATTERNS {
        if pattern.starts_with('*') {
            let suffix = &pattern[1..];
            if file_name.ends_with(suffix) {
                return Some(description);
            }
        } else if pattern.starts_with('/') {
            if path_str == *pattern {
                return Some(description);
            }
        } else {
            if path_str.contains(pattern) || path_str.ends_with(pattern) {
                return Some(description);
            }
        }
    }
    None
}

/// Environment variable names that must be stripped from subprocess
/// environments to prevent credential leakage.
const CREDENTIAL_ENV_BLOCKLIST: &[&str] = &[
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "NVIDIA_API_KEY",
    "GEMINI_API_KEY",
    "DEEPSEEK_API_KEY",
    "GITHUB_TOKEN",
    "GITHUB_PAT",
    "GH_TOKEN",
    "DOCKER_PASSWORD",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "HERMES_API_KEY",
    "NOTION_TOKEN",
    "SLACK_BOT_TOKEN",
    "SLACK_USER_TOKEN",
    "DISCORD_TOKEN",
    "TELEGRAM_BOT_TOKEN",
    "WHATSAPP_TOKEN",
    "MOAT_API_KEY",
    "AGEEX_API_KEY",
    "HF_TOKEN",
    "HUGGINGFACE_TOKEN",
];

/// Strips credential env vars from a key-value map.
/// Returns a new map with only non-credential entries.
pub fn strip_credential_env(
    env: &std::collections::HashMap<String, String>,
) -> std::collections::HashMap<String, String> {
    env.iter()
        .filter(|(k, _)| !is_credential_env(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Check if an env var name matches a credential pattern.
pub fn is_credential_env(name: &str) -> bool {
    CREDENTIAL_ENV_BLOCKLIST
        .iter()
        .any(|blocked| name.eq_ignore_ascii_case(blocked))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_ssh_key_write_denied() {
        assert!(is_write_denied(Path::new("/home/user/.ssh/id_rsa")).is_some());
        assert!(is_write_denied(Path::new("/home/user/.ssh/id_ed25519")).is_some());
    }

    #[test]
    fn test_env_file_write_denied() {
        assert!(is_write_denied(Path::new("/project/.env")).is_some());
        assert!(is_write_denied(Path::new("/project/.env.production")).is_some());
    }

    #[test]
    fn test_pem_key_write_denied() {
        assert!(is_write_denied(Path::new("/workspace/cert.pem")).is_some());
        assert!(is_write_denied(Path::new("/workspace/secrets.key")).is_some());
    }

    #[test]
    fn test_normal_file_write_allowed() {
        assert!(is_write_denied(Path::new("/project/main.rs")).is_none());
        assert!(is_write_denied(Path::new("/project/README.md")).is_none());
    }

    #[test]
    fn test_credential_env_stripping() {
        let mut env = std::collections::HashMap::new();
        env.insert("OPENAI_API_KEY".into(), "sk-secret".into());
        env.insert("HOME".into(), "/home/user".into());
        env.insert("ANTHROPIC_API_KEY".into(), "sk-ant-secret".into());
        env.insert("PATH".into(), "/usr/bin".into());

        let stripped = strip_credential_env(&env);
        assert!(!stripped.contains_key("OPENAI_API_KEY"));
        assert!(!stripped.contains_key("ANTHROPIC_API_KEY"));
        assert!(stripped.contains_key("HOME"));
        assert!(stripped.contains_key("PATH"));
    }

    #[test]
    fn test_credential_env_detection() {
        assert!(is_credential_env("OPENAI_API_KEY"));
        assert!(is_credential_env("GITHUB_TOKEN"));
        assert!(!is_credential_env("HOME"));
        assert!(!is_credential_env("USER"));
    }
}
