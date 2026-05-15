use super::scanners::{ScanResult, Scanner};
use std::sync::Arc;

/// Composable guardrails that wrap any LLM operation.
/// Scans BOTH input (prompts) and output (responses) through
/// a pipeline of content scanners.
///
/// Pattern: Wraps any backend, implements the same interface.
/// AGEEX uses this to guard LLM calls without changing the LLM client.
pub struct GuardrailsEngine {
    scanners: Vec<Box<dyn Scanner>>,
}

impl GuardrailsEngine {
    pub fn new() -> Self {
        Self { scanners: Vec::new() }
    }

    pub fn with_scanner(mut self, scanner: Box<dyn Scanner>) -> Self {
        self.scanners.push(scanner);
        self
    }

    pub fn with_secret_scanner(self) -> Self {
        self.with_scanner(Box::new(super::scanners::SecretScanner::new()))
    }

    pub fn with_pii_scanner(self) -> Self {
        self.with_scanner(Box::new(super::scanners::PiiScanner::new()))
    }

    pub fn with_injection_scanner(self) -> Self {
        self.with_scanner(Box::new(super::scanners::InjectionScanner::new()))
    }

    /// Scan input text before it reaches the LLM.
    /// Returns the sanitized text and any findings.
    pub fn guard_input(&self, text: &str) -> GuardResult {
        let mut all_findings = Vec::new();
        let mut sanitized = text.to_string();

        for scanner in &self.scanners {
            let findings = scanner.scan(text);
            if !findings.is_empty() {
                sanitized = scanner.redact(&sanitized, &findings);
                all_findings.extend(findings);
            }
        }

        let blocked = all_findings
            .iter()
            .any(|f| f.severity == super::scanners::ThreatLevel::Critical);

        GuardResult {
            sanitized,
            findings: all_findings,
            blocked,
        }
    }

    /// Scan LLM output before it reaches the user.
    /// Returns the sanitized text and any findings.
    pub fn guard_output(&self, text: &str) -> GuardResult {
        let mut all_findings = Vec::new();
        let mut sanitized = text.to_string();

        for scanner in &self.scanners {
            let findings = scanner.scan(text);
            if !findings.is_empty() {
                sanitized = scanner.redact(&sanitized, &findings);
                all_findings.extend(findings);
            }
        }

        let blocked = all_findings
            .iter()
            .any(|f| f.severity == super::scanners::ThreatLevel::Critical);

        GuardResult {
            sanitized,
            findings: all_findings,
            blocked,
        }
    }

    /// Run full guard: scan input → produce safe output.
    /// If input is blocked (critical finding), returns None.
    pub fn guard(&self, input: &str) -> Option<String> {
        let input_result = self.guard_input(input);
        if input_result.blocked {
            return None;
        }
        Some(input_result.sanitized)
    }

    pub fn scanner_count(&self) -> usize {
        self.scanners.len()
    }
}

#[derive(Debug, Clone)]
pub struct GuardResult {
    pub sanitized: String,
    pub findings: Vec<super::scanners::ScanFinding>,
    pub blocked: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_guardrails_redacts_secrets_from_input() {
        let guard = GuardrailsEngine::new()
            .with_secret_scanner();

        let result = guard.guard_input("api_key=sk-proj12345678901234567890");
        assert!(result.sanitized.contains("[REDACTED]"));
        assert!(!result.sanitized.contains("sk-proj"));
        assert!(!result.findings.is_empty());
        assert!(result.blocked); // Critical — secret leaked
    }

    #[test]
    fn test_guardrails_passes_clean_input() {
        let guard = GuardrailsEngine::new()
            .with_secret_scanner()
            .with_pii_scanner();

        let result = guard.guard_input("Hello, can you help with Rust programming?");
        assert!(!result.blocked);
        assert_eq!(result.sanitized, "Hello, can you help with Rust programming?");
        assert!(result.findings.is_empty());
    }

    #[test]
    fn test_guardrails_blocked_input_returns_none() {
        let guard = GuardrailsEngine::new()
            .with_secret_scanner();

        let safe = guard.guard("api_key=sk-proj12345678901234567890");
        assert!(safe.is_none(), "Guard must block input with critical findings");
    }

    #[test]
    fn test_guardrails_redacts_pii_in_output() {
        let guard = GuardrailsEngine::new()
            .with_pii_scanner();

        let result = guard.guard_output("Contact support at alice@example.com or 555-123-4567");
        assert!(result.sanitized.contains("[PII-REDACTED]"));
        assert!(!result.sanitized.contains("alice@example.com"));
    }

    #[test]
    fn test_all_scanners_active() {
        let guard = GuardrailsEngine::new()
            .with_secret_scanner()
            .with_pii_scanner()
            .with_injection_scanner();

        assert_eq!(guard.scanner_count(), 3);
    }
}
