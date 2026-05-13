use super::decision::{RouteDecision, RouteTarget};
use regex::Regex;

fn agent2_strong_patterns() -> Vec<Regex> {
    vec![
        Regex::new(r"(?i)\b(create|write|generate|make|build|produce)\b.{0,30}\b(file|script|program|function|class|module|test)\b").unwrap(),
        Regex::new(r"(?i)\b(run|execute|launch|start|deploy|install|uninstall)\b").unwrap(),
        Regex::new(r"(?i)\b(edit|modify|update|change|refactor|rename|delete|remove)\b.{0,30}\b(file|function|class|method|variable|line)\b").unwrap(),
        Regex::new(r"(?i)\b(npm|cargo|pip|yarn|brew|apt|git commit|git push)\b").unwrap(),
        Regex::new(r"(?i)\b(mkdir|touch|mv|cp|rm)\b").unwrap(),
    ]
}

fn agent1_strong_patterns() -> Vec<Regex> {
    vec![
        Regex::new(r"(?i)\b(show me|display|print out|read|view|look at)\b.{0,50}\.(?:md|rs|toml|txt|json|yaml|yml|sh|py|ts|js)").unwrap(),
        Regex::new(r"(?i)\b(show|display)\b.{0,30}\b(contents?|content of|file)\b").unwrap(),
        Regex::new(r"(?i)\b(what is|what are|explain|describe|summarize|list)\b").unwrap(),
        Regex::new(r"(?i)\b(check|verify|confirm|does|is there|exists?)\b").unwrap(),
        Regex::new(r"(?i)\bcat\s+\S+\b").unwrap(),
    ]
}

fn agent2_weak_patterns() -> Vec<Regex> {
    vec![
        Regex::new(r"(?i)\b(update|fix|patch)\b").unwrap(),
        Regex::new(r"(?i)\b(add|append|insert)\b").unwrap(),
        Regex::new(r"(?i)\b(test|debug|lint|format)\b").unwrap(),
    ]
}

#[derive(Default)]
pub struct RegexClassifier;

impl RegexClassifier {
    pub fn classify(&self, query: &str) -> RouteDecision {
        let strong2 = agent2_strong_patterns();
        for pattern in &strong2 {
            if pattern.is_match(query) {
                return RouteDecision::agent2_strong(&format!("Strong Agent2 pattern: {}", pattern.as_str()));
            }
        }

        let strong1 = agent1_strong_patterns();
        for pattern in &strong1 {
            if pattern.is_match(query) {
                return RouteDecision::agent1_strong(&format!("Strong Agent1 pattern: {}", pattern.as_str()));
            }
        }

        let weak_matches: Vec<_> = agent2_weak_patterns().into_iter().filter(|p| p.is_match(query)).collect();
        if !weak_matches.is_empty() {
            let confidence = 0.55 + (weak_matches.len() as f32 * 0.05).min(0.1);
            return RouteDecision::agent2_weak(confidence, "Weak Agent2 signals — LLM fallback recommended");
        }

        RouteDecision {
            target: RouteTarget::Agent1,
            confidence: 0.40,
            method: super::decision::RouteMethod::RegexWeak,
            reason: "No strong signal — requires LLM classification".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strong_agent2_create_file() {
        let c = RegexClassifier::default();
        let d = c.classify("create a new file called main.rs");
        assert_eq!(d.target, RouteTarget::Agent2);
        assert!(d.confidence >= 0.9);
    }

    #[test]
    fn test_strong_agent2_run_command() {
        let c = RegexClassifier::default();
        let d = c.classify("run cargo test");
        assert_eq!(d.target, RouteTarget::Agent2);
        assert!(d.confidence >= 0.9);
    }

    #[test]
    fn test_strong_agent1_show_file() {
        let c = RegexClassifier::default();
        let d = c.classify("show me the content of main.rs");
        assert_eq!(d.target, RouteTarget::Agent1);
        assert!(d.confidence >= 0.9);
    }

    #[test]
    fn test_weak_update_triggers_fallback() {
        let c = RegexClassifier::default();
        let d = c.classify("update the readme");
        assert!(d.needs_llm_fallback() || d.target == RouteTarget::Agent2);
    }

    #[test]
    fn test_ambiguous_triggers_fallback() {
        let c = RegexClassifier::default();
        let d = c.classify("what did I tell you earlier about the project?");
        assert!(d.needs_llm_fallback());
    }
}