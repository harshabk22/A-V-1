use rusqlite::{Connection, Result as SqlResult, params};
use uuid::Uuid;
use std::path::PathBuf;
use anyhow::{Result, Context};

#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: String,
    pub parent_id: Option<String>,
    pub agent_type: String,
    pub created_at: i64,
    pub last_active: i64,
}

#[derive(Debug, Clone)]
pub struct MessageRecord {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone)]
pub struct ApprovalRecord {
    pub id: i64,
    pub agent_type: String,
    pub capability: String,
    pub scope_type: String,
    pub scope_value: String,
    pub timestamp: i64,
}

pub struct SessionStore {
    conn: Connection,
}

impl SessionStore {
    pub fn open() -> Result<Self> {
        let db_path = Self::db_path()?;

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        let conn = Connection::open(&db_path)
            .with_context(|| format!("Failed to open database: {}", db_path.display()))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;

        let store = Self { conn };
        store.run_migrations()?;
        store.clean_corrupted_preferences()?;

        Ok(store)
    }

    fn db_path() -> Result<PathBuf> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        Ok(home.join(".ageex").join("sessions.db"))
    }

    fn run_migrations(&self) -> Result<()> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS sessions (
                id          TEXT PRIMARY KEY,
                parent_id   TEXT REFERENCES sessions(id),
                agent_type  TEXT NOT NULL,
                created_at  INTEGER NOT NULL,
                last_active INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS messages (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id  TEXT NOT NULL REFERENCES sessions(id),
                role        TEXT NOT NULL,
                content     TEXT NOT NULL,
                timestamp   INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_session_messages
                ON messages(session_id, timestamp);

            CREATE INDEX IF NOT EXISTS idx_sessions_agent_type
                ON sessions(agent_type, last_active);

            CREATE TABLE IF NOT EXISTS approvals (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_type  TEXT NOT NULL,
                capability  TEXT NOT NULL,
                scope_type  TEXT NOT NULL,
                scope_value TEXT NOT NULL DEFAULT '',
                timestamp   INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_approvals_agent
                ON approvals(agent_type, capability);

            -- FTS5 full-text search over messages
            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts
                USING fts5(content, role, session_id UNINDEXED, tokenize='porter ascii');

            -- Keep FTS in sync automatically
            CREATE TRIGGER IF NOT EXISTS messages_fts_insert
                AFTER INSERT ON messages BEGIN
                    INSERT INTO messages_fts(rowid, content, role, session_id)
                    VALUES (new.id, new.content, new.role, new.session_id);
                END;

            CREATE TRIGGER IF NOT EXISTS messages_fts_delete
                AFTER DELETE ON messages BEGIN
                    DELETE FROM messages_fts WHERE rowid = old.id;
                END;
        ")?;

        Ok(())
    }

    pub fn clean_corrupted_preferences(&self) -> Result<()> {
        let common_words = ["called", "named", "is", "the", "a", "an", "my"];
        for word in &common_words {
            self.conn.execute(
                "DELETE FROM user_preferences WHERE LOWER(value) = ?1",
                params![word],
            )?;
        }
        Ok(())
    }

    pub fn create_session(&self, agent_type: &str, parent_id: Option<&str>) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Self::now();

        self.conn.execute(
            "INSERT INTO sessions (id, parent_id, agent_type, created_at, last_active)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, parent_id, agent_type, now],
        )?;

        Ok(id)
    }

    pub fn get_latest_session(&self, agent_type: &str) -> Result<Option<SessionRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_id, agent_type, created_at, last_active
             FROM sessions
             WHERE agent_type = ?1
             ORDER BY last_active DESC
             LIMIT 1"
        )?;

        let result = stmt.query_row(params![agent_type], |row| {
            Ok(SessionRecord {
                id: row.get(0)?,
                parent_id: row.get(1)?,
                agent_type: row.get(2)?,
                created_at: row.get(3)?,
                last_active: row.get(4)?,
            })
        });

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn append_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
    ) -> Result<()> {
        let now = Self::now();

        self.conn.execute(
            "INSERT INTO messages (session_id, role, content, timestamp)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_id, role, content, now],
        )?;

        self.conn.execute(
            "UPDATE sessions SET last_active = ?1 WHERE id = ?2",
            params![now, session_id],
        )?;

        Ok(())
    }

    pub fn load_messages(&self, session_id: &str) -> Result<Vec<MessageRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, role, content, timestamp
             FROM messages
             WHERE session_id = ?1
             ORDER BY timestamp ASC"
        )?;

        let messages = stmt.query_map(params![session_id], |row| {
            Ok(MessageRecord {
                id: row.get(0)?,
                session_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                timestamp: row.get(4)?,
            })
        })?
        .collect::<SqlResult<Vec<_>>>()?;

        Ok(messages)
    }

    pub fn list_recent_sessions(&self, limit: usize) -> Result<Vec<SessionRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_id, agent_type, created_at, last_active
             FROM sessions
             ORDER BY last_active DESC
             LIMIT ?1"
        )?;

        let sessions = stmt.query_map(params![limit as i64], |row| {
            Ok(SessionRecord {
                id: row.get(0)?,
                parent_id: row.get(1)?,
                agent_type: row.get(2)?,
                created_at: row.get(3)?,
                last_active: row.get(4)?,
            })
        })?
        .collect::<SqlResult<Vec<_>>>()?;

        Ok(sessions)
    }

    pub fn record_approval(
        &self,
        agent_type: &str,
        capability: &str,
        scope_type: &str,
        scope_value: &str,
    ) -> Result<()> {
        let now = Self::now();
        self.conn.execute(
            "INSERT INTO approvals (agent_type, capability, scope_type, scope_value, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![agent_type, capability, scope_type, scope_value, now],
        )?;
        Ok(())
    }

    pub fn load_approvals(&self, agent_type: &str) -> Result<Vec<ApprovalRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, agent_type, capability, scope_type, scope_value, timestamp
             FROM approvals
             WHERE agent_type = ?1
             ORDER BY timestamp ASC"
        )?;

        let records = stmt.query_map(params![agent_type], |row| {
            Ok(ApprovalRecord {
                id: row.get(0)?,
                agent_type: row.get(1)?,
                capability: row.get(2)?,
                scope_type: row.get(3)?,
                scope_value: row.get(4)?,
                timestamp: row.get(5)?,
            })
        })?
        .collect::<SqlResult<Vec<_>>>()?;

        Ok(records)
    }

    /// Full-text search across all messages with optional agent filter
    pub fn search_messages(
        &self,
        query: &str,
        agent_type: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MessageRecord>> {
        let sql = match agent_type {
            Some(_) => "
                SELECT m.id, m.session_id, m.role, m.content, m.timestamp
                FROM messages m
                JOIN messages_fts fts ON m.id = fts.rowid
                JOIN sessions s ON m.session_id = s.id
                WHERE messages_fts MATCH ?1 AND s.agent_type = ?3
                ORDER BY rank
                LIMIT ?2",
            None => "
                SELECT m.id, m.session_id, m.role, m.content, m.timestamp
                FROM messages m
                JOIN messages_fts fts ON m.id = fts.rowid
                WHERE messages_fts MATCH ?1
                ORDER BY rank
                LIMIT ?2",
        };

        let mut stmt = self.conn.prepare(sql)?;
        let agent_param = agent_type.unwrap_or("");

        let messages = if agent_type.is_some() {
            stmt.query_map(params![query, limit as i64, agent_param], |row| {
                Ok(MessageRecord {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    timestamp: row.get(4)?,
                })
            })?.collect::<SqlResult<Vec<_>>>()?
        } else {
            stmt.query_map(params![query, limit as i64], |row| {
                Ok(MessageRecord {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    timestamp: row.get(4)?,
                })
            })?.collect::<SqlResult<Vec<_>>>()?
        };

        Ok(messages)
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

    fn test_store() -> SessionStore {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        let store = SessionStore { conn };
        store.run_migrations().unwrap();
        store
    }

    #[test]
    fn test_create_and_retrieve_session() {
        let store = test_store();
        let id = store.create_session("agent1", None).unwrap();
        let latest = store.get_latest_session("agent1").unwrap();
        assert!(latest.is_some());
        assert_eq!(latest.unwrap().id, id);
    }

    #[test]
    fn test_append_and_load_messages() {
        let store = test_store();
        let id = store.create_session("agent2", None).unwrap();

        store.append_message(&id, "user", "Hello agent").unwrap();
        store.append_message(&id, "assistant", "Hello user").unwrap();

        let messages = store.load_messages(&id).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[1].role, "assistant");
    }

    #[test]
    fn test_record_and_load_approvals() {
        let store = test_store();
        store.record_approval("agent2", "FileWrite", "path", "/workspace").unwrap();
        store.record_approval("agent2", "FileWrite", "path", "/workspace/subdir").unwrap();

        let approvals = store.load_approvals("agent2").unwrap();
        assert_eq!(approvals.len(), 2);
        assert_eq!(approvals[0].capability, "FileWrite");
        assert_eq!(approvals[1].scope_value, "/workspace/subdir");

        let agent1_approvals = store.load_approvals("agent1").unwrap();
        assert_eq!(agent1_approvals.len(), 0);
    }

    #[test]
    fn test_latest_session_is_most_recent() {
        let store = test_store();
        let _id1 = store.create_session("agent1", None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let id2 = store.create_session("agent1", None).unwrap();

        let latest = store.get_latest_session("agent1").unwrap().unwrap();
        assert_eq!(latest.id, id2);
    }

    #[test]
    fn test_fts5_search_messages() {
        let store = test_store();
        let sid = store.create_session("agent1", None).unwrap();
        store.append_message(&sid, "user", "show me the file").unwrap();
        store.append_message(&sid, "assistant", "Here is the file content").unwrap();
        store.append_message(&sid, "user", "write a new script").unwrap();

        let results = store.search_messages("file", None, 10).unwrap();
        assert_eq!(results.len(), 2); // both "file" matches
        assert!(results[0].content.contains("file"));
    }
}