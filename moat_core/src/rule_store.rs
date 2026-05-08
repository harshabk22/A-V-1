use chrono::{Duration, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{MoatError, MoatResult};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearnedRule {
    pub id: String,
    pub target_worker: String,
    pub rule_text: String,
    pub usage_count: u32,
    pub confidence_score: f32,
    pub expires_at: chrono::DateTime<Utc>,
}

impl LearnedRule {
    pub fn new(target_worker: impl Into<String>, rule_text: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            target_worker: target_worker.into(),
            rule_text: rule_text.into(),
            usage_count: 0,
            confidence_score: 0.5,
            expires_at: Utc::now() + Duration::days(7),
        }
    }

    pub fn record_success(&mut self) {
        self.usage_count += 1;
        self.confidence_score = (self.confidence_score + 0.1).min(1.0);
        self.expires_at = Utc::now() + Duration::days(7);
    }

    pub fn record_failure(&mut self) {
        self.usage_count += 1;
        self.confidence_score = (self.confidence_score - 0.2).max(0.0);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuleConflictReview {
    pub new_rule: LearnedRule,
    pub conflicting_rules: Vec<LearnedRule>,
}

pub fn prepare_tables(conn: &Connection) -> MoatResult<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS learned_rules (
            id TEXT PRIMARY KEY,
            target_worker TEXT NOT NULL,
            rule_text TEXT NOT NULL,
            usage_count INTEGER NOT NULL,
            confidence_score REAL NOT NULL,
            expires_at TEXT NOT NULL
        )",
        [],
    )
    .map_err(MoatError::Database)?;
    Ok(())
}

pub fn load_rules(conn: &Connection) -> MoatResult<Vec<LearnedRule>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, target_worker, rule_text, usage_count, confidence_score, expires_at
             FROM learned_rules",
        )
        .map_err(MoatError::Database)?;
    let rows = stmt
        .query_map([], |row| {
            let expires_at: String = row.get(5)?;
            let expires_at = chrono::DateTime::parse_from_rfc3339(&expires_at)
                .map(|value| value.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now() + Duration::days(7));
            Ok(LearnedRule {
                id: row.get(0)?,
                target_worker: row.get(1)?,
                rule_text: row.get(2)?,
                usage_count: row.get::<_, i64>(3)? as u32,
                confidence_score: row.get(4)?,
                expires_at,
            })
        })
        .map_err(MoatError::Database)?;

    let mut rules = Vec::new();
    for row in rows {
        rules.push(row.map_err(MoatError::Database)?);
    }
    Ok(rules)
}

pub fn store_rule(conn: &Connection, rule: &LearnedRule) -> MoatResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO learned_rules
         (id, target_worker, rule_text, usage_count, confidence_score, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            rule.id,
            rule.target_worker,
            rule.rule_text,
            rule.usage_count as i64,
            rule.confidence_score,
            rule.expires_at.to_rfc3339()
        ],
    )
    .map_err(MoatError::Database)?;
    Ok(())
}

pub fn delete_rule(conn: &Connection, rule_id: &str) -> MoatResult<()> {
    conn.execute("DELETE FROM learned_rules WHERE id = ?1", params![rule_id])
        .map_err(MoatError::Database)?;
    Ok(())
}

pub fn keyword_overlap(left: &str, right: &str) -> f32 {
    let left_tokens = tokenize(left);
    let right_tokens = tokenize(right);
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return 0.0;
    }
    let overlap = left_tokens
        .iter()
        .filter(|token| right_tokens.contains(*token))
        .count();
    overlap as f32 / left_tokens.len().max(right_tokens.len()) as f32
}

pub fn check_conflicts(new_rule: &LearnedRule, existing_rules: &[LearnedRule]) -> Vec<LearnedRule> {
    existing_rules
        .iter()
        .filter(|rule| {
            rule.target_worker == new_rule.target_worker
                && keyword_overlap(&rule.rule_text, &new_rule.rule_text) > 0.6
        })
        .cloned()
        .collect()
}

fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = text
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| token.len() >= 3)
        .collect::<Vec<_>>();
    tokens.sort();
    tokens.dedup();
    tokens
}

#[cfg(test)]
mod tests {
    use super::{check_conflicts, LearnedRule};

    #[test]
    fn conflict_detection_matches_same_worker() {
        let new_rule = LearnedRule::new("coder", "Prefer cargo check before cargo test");
        let existing = vec![LearnedRule::new(
            "coder",
            "Prefer cargo check before cargo test on Rust edits",
        )];
        assert_eq!(check_conflicts(&new_rule, &existing).len(), 1);
    }
}
