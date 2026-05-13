use rusqlite::{Connection, Result as SqlResult, params};
use anyhow::Result;

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
}

impl ProfileStore {
    pub fn open() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        let db_path = home.join(".ageex").join("sessions.db");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        let store = Self { conn };
        store.run_migrations()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.run_migrations()?;
        Ok(store)
    }

    fn run_migrations(&self) -> Result<()> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS user_preferences (
                key         TEXT PRIMARY KEY,
                value       TEXT NOT NULL,
                confidence  REAL NOT NULL DEFAULT 0.5,
                source      TEXT NOT NULL,
                updated_at  INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS user_corrections (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                original    TEXT NOT NULL,
                correction  TEXT NOT NULL,
                session_id  TEXT NOT NULL,
                created_at  INTEGER NOT NULL
            );
        ")?;
        Ok(())
    }

    pub fn upsert_preference(&self, key: &str, value: &str, confidence: f32, source: &str) -> Result<()> {
        let now = Self::now();
        self.conn.execute(
            "INSERT INTO user_preferences (key, value, confidence, source, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                confidence = MAX(confidence, excluded.confidence),
                source = excluded.source,
                updated_at = excluded.updated_at",
            params![key, value, confidence, source, now],
        )?;
        Ok(())
    }

    pub fn get_preference(&self, key: &str) -> Result<Option<UserPreference>> {
        let result = self.conn.query_row(
            "SELECT key, value, confidence, source, updated_at
             FROM user_preferences WHERE key = ?1",
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
        let mut stmt = self.conn.prepare(
            "SELECT key, value, confidence, source, updated_at
             FROM user_preferences
             ORDER BY confidence DESC, updated_at DESC"
        )?;
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
        self.conn.execute(
            "INSERT INTO user_corrections (original, correction, session_id, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![original, correction, session_id, Self::now()],
        )?;
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
            "## User Profile (learned from past sessions)
{}
",
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
}