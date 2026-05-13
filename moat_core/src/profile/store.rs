use rusqlite::{Connection, Result as SqlResult, params};
use anyhow::{Result, bail};
use regex::Regex;

fn validate_prefix(prefix: &str) -> Result<()> {
    if prefix.is_empty() {
        bail!("prefix must not be empty");
    }
    if prefix.len() > 32 {
        bail!("prefix must be at most 32 characters, got {}", prefix.len());
    }
    let re = Regex::new(r"^[a-zA-Z0-9_]+$").unwrap();
    if !re.is_match(prefix) {
        bail!("prefix must contain only alphanumeric characters and underscores, got: '{}'", prefix);
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct UserPreference {
    pub key: String,
    pub value: String,
    pub confidence: f32,
    pub source: String,
    pub updated_at: i64,
}

pub struct ProfileStore {
    conn: Connection,
    prefix: String,
}

impl ProfileStore {
    pub fn open() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        let db_path = home.join(".ageex").join("sessions.db");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        let store = Self { conn, prefix: String::new() };
        store.run_migrations()?;
        Ok(store)
    }

    pub fn open_with_prefix(prefix: &str) -> Result<Self> {
        validate_prefix(prefix)?;

        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        let db_path = home.join(".ageex").join("sessions.db");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        let store = Self { conn, prefix: prefix.to_string() };
        store.run_migrations()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn, prefix: String::new() };
        store.run_migrations()?;
        Ok(store)
    }

    pub fn open_with_prefix_in_memory(prefix: &str) -> Result<Self> {
        validate_prefix(prefix)?;

        let conn = Connection::open_in_memory()?;
        let store = Self { conn, prefix: prefix.to_string() };
        store.run_migrations()?;
        Ok(store)
    }

    fn prefs_table(&self) -> String {
        format!("{}user_preferences", self.prefix)
    }

    fn corrections_table(&self) -> String {
        format!("{}user_corrections", self.prefix)
    }

    fn run_migrations(&self) -> Result<()> {
        let pt = self.prefs_table();
        let ct = self.corrections_table();

        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {pt} (
                key         TEXT PRIMARY KEY,
                value       TEXT NOT NULL,
                confidence  REAL NOT NULL DEFAULT 0.5,
                source      TEXT NOT NULL,
                updated_at  INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS {ct} (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                original    TEXT NOT NULL,
                correction  TEXT NOT NULL,
                session_id  TEXT NOT NULL,
                created_at  INTEGER NOT NULL
            );"
        );

        self.conn.execute_batch(&sql)?;
        Ok(())
    }

    pub fn upsert_preference(&self, key: &str, value: &str, confidence: f32, source: &str) -> Result<()> {
        let now = Self::now();
        let pt = self.prefs_table();
        let sql = format!(
            "INSERT INTO {pt} (key, value, confidence, source, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                confidence = MAX(confidence, excluded.confidence),
                source = excluded.source,
                updated_at = excluded.updated_at"
        );
        self.conn.execute(&sql, params![key, value, confidence, source, now])?;
        Ok(())
    }

    pub fn get_preference(&self, key: &str) -> Result<Option<UserPreference>> {
        let pt = self.prefs_table();
        let sql = format!(
            "SELECT key, value, confidence, source, updated_at FROM {pt} WHERE key = ?1"
        );
        let result = self.conn.query_row(
            &sql,
            params![key],
            |row| Ok(UserPreference {
                key: row.get(0)?,
                value: row.get(1)?,
                confidence: row.get(2)?,
                source: row.get(3)?,
                updated_at: row.get(4)?,
            }),
        );
        match result {
            Ok(p) => Ok(Some(p)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_all_preferences(&self) -> Result<Vec<UserPreference>> {
        let pt = self.prefs_table();
        let sql = format!(
            "SELECT key, value, confidence, source, updated_at FROM {pt} ORDER BY confidence DESC, updated_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let prefs = stmt.query_map([], |row| {
            Ok(UserPreference {
                key: row.get(0)?,
                value: row.get(1)?,
                confidence: row.get(2)?,
                source: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })?.collect::<SqlResult<Vec<_>>>()?;
        Ok(prefs)
    }

    pub fn record_correction(&self, original: &str, correction: &str, session_id: &str) -> Result<()> {
        let ct = self.corrections_table();
        let sql = format!(
            "INSERT INTO {ct} (original, correction, session_id, created_at) VALUES (?1, ?2, ?3, ?4)"
        );
        self.conn.execute(&sql, params![original, correction, session_id, Self::now()])?;
        Ok(())
    }

    pub fn build_context_injection(&self) -> Result<String> {
        let prefs = self.get_all_preferences()?;
        if prefs.is_empty() {
            return Ok(String::new());
        }

        let high_conf: Vec<_> = prefs.iter()
            .filter(|p| p.confidence >= 0.85)
            .collect();

        if high_conf.is_empty() {
            return Ok(String::new());
        }

        let lines: Vec<String> = high_conf.iter()
            .map(|p| format!("- {}: {}", p.key, p.value))
            .collect();

        Ok(format!(
            "## User Profile (learned from past sessions)\n{}\n",
            lines.join("\n")
        ))
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_upsert_and_get_preference() {
        let store = ProfileStore::open_in_memory().unwrap();
        store.upsert_preference("project_name", "Ageex", 0.9, "session-1").unwrap();
        let pref = store.get_preference("project_name").unwrap().unwrap();
        assert_eq!(pref.value, "Ageex");
        assert_eq!(pref.confidence, 0.9);
    }

    #[test]
    fn test_confidence_never_decreases_on_upsert() {
        let store = ProfileStore::open_in_memory().unwrap();
        store.upsert_preference("lang", "Rust", 0.9, "s1").unwrap();
        store.upsert_preference("lang", "Rust", 0.3, "s2").unwrap();
        let pref = store.get_preference("lang").unwrap().unwrap();
        assert_eq!(pref.confidence, 0.9);
    }

    #[test]
    fn test_build_context_injection_empty() {
        let store = ProfileStore::open_in_memory().unwrap();
        let injection = store.build_context_injection().unwrap();
        assert!(injection.is_empty());
    }

    #[test]
    fn test_build_context_injection_with_data() {
        let store = ProfileStore::open_in_memory().unwrap();
        store.upsert_preference("project_name", "Ageex", 0.9, "s1").unwrap();
        store.upsert_preference("preferred_language", "Rust", 0.85, "s1").unwrap();
        let injection = store.build_context_injection().unwrap();
        assert!(injection.contains("Ageex"));
        assert!(injection.contains("Rust"));
    }

    #[test]
    fn test_prefix_isolated_preferences() {
        let spotlight = ProfileStore::open_with_prefix_in_memory("spotlight_").unwrap();
        let main = ProfileStore::open_with_prefix_in_memory("main_").unwrap();

        spotlight.upsert_preference("pref_a", "spotlight_val", 0.9, "s_s").unwrap();
        main.upsert_preference("pref_a", "main_val", 0.9, "s_m").unwrap();

        let sp = spotlight.get_preference("pref_a").unwrap().unwrap();
        let mp = main.get_preference("pref_a").unwrap().unwrap();
        assert_eq!(sp.value, "spotlight_val");
        assert_eq!(mp.value, "main_val");
    }

    #[test]
    fn test_prefix_isolated_from_empty() {
        let prefixed = ProfileStore::open_with_prefix_in_memory("spotlight_").unwrap();
        let plain = ProfileStore::open_in_memory().unwrap();

        prefixed.upsert_preference("only_in_spotlight", "value", 1.0, "s").unwrap();

        let from_plain = plain.get_preference("only_in_spotlight").unwrap();
        assert!(from_plain.is_none());
    }

    #[test]
    fn test_prefix_validation_rejects_sql_injection() {
        assert!(ProfileStore::open_with_prefix_in_memory("x; DROP TABLE user_preferences;--").is_err());
    }

    #[test]
    fn test_prefix_validation_rejects_special_chars() {
        assert!(ProfileStore::open_with_prefix_in_memory("bad prefix").is_err());
        assert!(ProfileStore::open_with_prefix_in_memory("bad'prefix").is_err());
    }

    #[test]
    fn test_prefix_validation_rejects_empty() {
        assert!(ProfileStore::open_with_prefix_in_memory("").is_err());
    }

    #[test]
    fn test_prefix_validation_rejects_too_long() {
        let long = "a".repeat(33);
        assert!(ProfileStore::open_with_prefix_in_memory(&long).is_err());
    }

    #[test]
    fn test_prefix_validation_accepts_valid() {
        assert!(ProfileStore::open_with_prefix_in_memory("spotlight_").is_ok());
        assert!(ProfileStore::open_with_prefix_in_memory("main_").is_ok());
        assert!(ProfileStore::open_with_prefix_in_memory("agent1_").is_ok());
        assert!(ProfileStore::open_with_prefix_in_memory("P").is_ok());
    }
}
