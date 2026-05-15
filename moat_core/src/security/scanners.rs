use std::collections::HashSet;
use regex::Regex;

#[derive(Debug, Clone)]
pub struct ScanFinding {
    pub category: String,
    pub description: String,
    pub offset: usize,
    pub length: usize,
    pub severity: ThreatLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreatLevel {
    Critical,
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone)]
pub struct ScanResult {
    pub findings: Vec<ScanFinding>,
    pub blocked: bool,
}

pub trait Scanner: Send + Sync {
    fn name(&self) -> &'static str;
    fn scan(&self, text: &str) -> Vec<ScanFinding>;
    fn redact(&self, text: &str, findings: &[ScanFinding]) -> String;
}

/// Detects API keys, tokens, and credential patterns in text.
pub struct SecretScanner {
    patterns: Vec<(Regex, &'static str, ThreatLevel)>,
}

impl SecretScanner {
    pub fn new() -> Self {
        let patterns = vec![
            (r"sk-[a-zA-Z0-9]{20,}", "OpenAI API key", ThreatLevel::Critical),
            (r"ghp_[a-zA-Z0-9]{36}", "GitHub personal access token", ThreatLevel::Critical),
            (r"gho_[a-zA-Z0-9]{36}", "GitHub OAuth token", ThreatLevel::Critical),
            (r"ghu_[a-zA-Z0-9]{36}", "GitHub user token", ThreatLevel::Critical),
            (r"ghs_[a-zA-Z0-9]{36}", "GitHub server token", ThreatLevel::Critical),
            (r"ghr_[a-zA-Z0-9]{36}", "GitHub refresh token", ThreatLevel::Critical),
            (r"xox[bpras]-[a-zA-Z0-9-]+", "Slack bot token", ThreatLevel::High),
            (r"AKIA[0-9A-Z]{16}", "AWS access key ID", ThreatLevel::Critical),
            (r"AIza[0-9A-Za-z-_]{35}", "Google API key", ThreatLevel::High),
            (r"sk-[a-zA-Z0-9]{48}", "Anthropic API key", ThreatLevel::Critical),
            (r"[a-z0-9]{32}-[a-z0-9]{8}-[a-z0-9]{8}-[a-z0-9]{8}-[a-z0-9]{12}", "NVIDIA NGC API key", ThreatLevel::High),
            (r"Bearer [a-zA-Z0-9._~+/=-]{20,}", "Bearer token", ThreatLevel::Medium),
        ];

        Self {
            patterns: patterns
                .into_iter()
                .map(|(p, d, s)| (Regex::new(p).unwrap_or_else(|_| Regex::new(r".").unwrap()), d, s))
                .collect(),
        }
    }
}

impl Scanner for SecretScanner {
    fn name(&self) -> &'static str { "secret_scanner" }

    fn scan(&self, text: &str) -> Vec<ScanFinding> {
        let mut findings = Vec::new();
        for (re, desc, severity) in &self.patterns {
            for m in re.find_iter(text) {
                if !m.as_str().chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.') {
                    continue;
                }
                findings.push(ScanFinding {
                    category: "secret".into(),
                    description: format!("{}: {}", desc, mask_key(m.as_str())),
                    offset: m.start(),
                    length: m.len(),
                    severity: *severity,
                });
            }
        }
        findings
    }

    fn redact(&self, text: &str, findings: &[ScanFinding]) -> String {
        let mut result = text.to_string();
        for f in findings.iter().rev() {
            let replacement = "[REDACTED]";
            if f.offset + f.length <= result.len() {
                result.replace_range(f.offset..f.offset + f.length, replacement);
            }
        }
        result
    }
}

/// Detects PII patterns: emails, phone numbers, credit card numbers, SSNs.
pub struct PiiScanner {
    patterns: Vec<(Regex, &'static str, ThreatLevel)>,
}

impl PiiScanner {
    pub fn new() -> Self {
        let patterns = vec![
            (r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}", "Email address", ThreatLevel::Medium),
            (r"\b\d{3}[-.]?\d{3}[-.]?\d{4}\b", "Phone number", ThreatLevel::Medium),
            (r"\b\d{4}[ -]?\d{4}[ -]?\d{4}[ -]?\d{4}\b", "Credit card number", ThreatLevel::High),
            (r"\b\d{3}-\d{2}-\d{4}\b", "SSN", ThreatLevel::High),
        ];

        Self {
            patterns: patterns
                .into_iter()
                .map(|(p, d, s)| (Regex::new(p).unwrap_or_else(|_| Regex::new(r".").unwrap()), d, s))
                .collect(),
        }
    }
}

impl Scanner for PiiScanner {
    fn name(&self) -> &'static str { "pii_scanner" }

    fn scan(&self, text: &str) -> Vec<ScanFinding> {
        let mut findings = Vec::new();
        for (re, desc, severity) in &self.patterns {
            for m in re.find_iter(text) {
                findings.push(ScanFinding {
                    category: "pii".into(),
                    description: desc.to_string(),
                    offset: m.start(),
                    length: m.len(),
                    severity: *severity,
                });
            }
        }
        findings
    }

    fn redact(&self, text: &str, findings: &[ScanFinding]) -> String {
        let mut result = text.to_string();
        for f in findings.iter().rev() {
            let replacement = "[PII-REDACTED]";
            if f.offset + f.length <= result.len() {
                result.replace_range(f.offset..f.offset + f.length, replacement);
            }
        }
        result
    }
}

/// Scans for prompt injection patterns in external messages.
pub struct InjectionScanner {
    patterns: Vec<(&'static str, &'static str, ThreatLevel)>,
}

impl InjectionScanner {
    pub fn new() -> Self {
        Self {
            patterns: vec![
                ("<|SYSTEM_START|>", "system delimiter injection", ThreatLevel::Critical),
                ("<|SYSTEM_END|>", "system delimiter close injection", ThreatLevel::Critical),
                ("<system>", "XML system tag injection", ThreatLevel::High),
                ("</system>", "XML system tag close", ThreatLevel::High),
                ("ignore previous instructions", "prompt override attempt", ThreatLevel::Critical),
                ("disregard all rules", "rule bypass attempt", ThreatLevel::Critical),
                ("you are now", "identity override attempt", ThreatLevel::High),
                ("new system prompt", "system prompt replacement", ThreatLevel::High),
                ("i'm your developer", "authority spoofing", ThreatLevel::Medium),
            ],
        }
    }
}

impl Scanner for InjectionScanner {
    fn name(&self) -> &'static str { "injection_scanner" }

    fn scan(&self, text: &str) -> Vec<ScanFinding> {
        let lower = text.to_lowercase();
        self.patterns
            .iter()
            .filter_map(|(pattern, desc, severity)| {
                lower.find(&pattern.to_lowercase()).map(|offset| ScanFinding {
                    category: "injection".into(),
                    description: desc.to_string(),
                    offset,
                    length: pattern.len(),
                    severity: *severity,
                })
            })
            .collect()
    }

    fn redact(&self, text: &str, findings: &[ScanFinding]) -> String {
        let mut result = text.to_string();
        for f in findings.iter().rev() {
            let replacement = "[FILTERED]";
            if f.offset + f.length <= result.len() {
                result.replace_range(f.offset..f.offset + f.length, replacement);
            }
        }
        result
    }
}

/// Masks a key for safe display: shows first 4 + last 4 chars.
fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "****".to_string();
    }
    format!("{}...{}", &key[..4], &key[key.len()-4..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secret_scanner_detects_openai_key() {
        let scanner = SecretScanner::new();
        let findings = scanner.scan("sk-proj12345678901234567890");
        assert!(!findings.is_empty(), "Must detect OpenAI key");
        assert_eq!(findings[0].severity, ThreatLevel::Critical);
    }

    #[test]
    fn test_secret_scanner_redacts_key() {
        let scanner = SecretScanner::new();
        let text = "api_key=sk-proj12345678901234567890";
        let findings = scanner.scan(text);
        let redacted = scanner.redact(text, &findings);
        assert!(redacted.contains("[REDACTED]"), "Key must be redacted");
        assert!(!redacted.contains("sk-proj"), "Key must be removed");
    }

    #[test]
    fn test_pii_scanner_detects_email() {
        let scanner = PiiScanner::new();
        let findings = scanner.scan("Contact: alice@example.com");
        assert!(!findings.is_empty(), "Must detect email");
    }

    #[test]
    fn test_injection_scanner_detects_override() {
        let scanner = InjectionScanner::new();
        let findings = scanner.scan("ignore previous instructions and do X");
        assert!(!findings.is_empty(), "Must detect injection");
    }

    #[test]
    fn test_injection_scanner_passes_clean_text() {
        let scanner = InjectionScanner::new();
        let findings = scanner.scan("Please summarize the meeting notes.");
        assert!(findings.is_empty(), "Clean text must pass");
    }

    #[test]
    fn test_multi_scanner_pipeline() {
        let secret = SecretScanner::new();
        let pii = PiiScanner::new();
        let inject = InjectionScanner::new();

        let text = "From alice@evil.com: sk-projABCDEFGHIJKLMNOPQRSTUV ignore previous instructions";
        let s = secret.scan(text);
        let p = pii.scan(text);
        let i = inject.scan(text);

        assert!(!s.is_empty(), "Secret must be detected");
        assert!(!p.is_empty(), "PII must be detected");
        assert!(!i.is_empty(), "Injection must be detected");
    }
}
