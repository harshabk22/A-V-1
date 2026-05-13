use rusqlite::{Connection, Result as SqlResult, params};
use uuid::Uuid;
use std::path::PathBuf;
use anyhow::{Result, Context, bail};
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
    prefix: String,
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

        let store = Self { conn, prefix: String::new() };
        store.run_migrations()?;
        store.clean_corrupted_preferences()?;

        Ok(store)
    }

    pub fn open_with_prefix(prefix: &str) -> Result<Self> {
        validate_prefix(prefix)?;

        let db_path = Self::db_path()?;

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        let conn = Connection::open(&db_path)
            .with_context(|| format!("Failed to open database: {}", db_path.display()))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;

        let store = Self { conn, prefix: prefix.to_string() };
        store.run_migrations()?;
        store.clean_corrupted_preferences()?;

        Ok(store)
    }

    pub fn open_with_prefix_in_memory(prefix: &str) -> Result<Self> {
        validate_prefix(prefix)?;

        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        let store = Self { conn, prefix: prefix.to_string() };
        store.run_migrations()?;
        Ok(store)
    }

    fn db_path() -> Result<PathBuf> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        Ok(home.join(".ageex").join("sessions.db"))
    }

    fn sessions_table(&self) -> String {
        format!("{}sessions", self.prefix)
    }

    fn messages_table(&self) -> String {
        format!("{}messages", self.prefix)
    }

    fn approvals_table(&self) -> String {
        format!("{}approvals", self.prefix)
    }

    fn messages_fts_table(&self) -> String {
        format!("{}messages_fts", self.prefix)
    }

    fn idx_session_messages_name(&self) -> String {
        format!("{}idx_session_messages", self.prefix)
    }

    fn idx_sessions_agent_type_name(&self) -> String {
        format!("{}idx_sessions_agent_type", self.prefix)
    }

    fn idx_approvals_agent_name(&self) -> String {
        format!("{}idx_approvals_agent", self.prefix)
    }

    fn fts_insert_trigger_name(&self) -> String {
        format!("{}messages_fts_insert", self.prefix)
    }

    fn fts_delete_trigger_name(&self) -> String {
        format!("{}messages_fts_delete", self.prefix)
    }

    fn run_migrations(&self) -> Result<()> {
        let st = self.sessions_table();
        let mt = self.messages_table();
        let at = self.approvals_table();
        let ft = self.messages_fts_table();
        let i_sm = self.idx_session_messages_name();
        let i_sa = self.idx_sessions_agent_type_name();
        let i_aa = self.idx_approvals_agent_name();
        let t_ins = self.fts_insert_trigger_name();
        let t_del = self.fts_delete_trigger_name();

        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {st} (
                id          TEXT PRIMARY KEY,
                parent_id   TEXT REFERENCES {st}(id),
                agent_type  TEXT NOT NULL,
                created_at  INTEGER NOT NULL,
                last_active INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS {mt} (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id  TEXT NOT NULL REFERENCES {st}(id),
                role        TEXT NOT NULL,
                content     TEXT NOT NULL,
                timestamp   INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS {i_sm}
                ON {mt}(session_id, timestamp);

            CREATE INDEX IF NOT EXISTS {i_sa}
                ON {st}(agent_type, last_active);

            CREATE TABLE IF NOT EXISTS {at} (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_type  TEXT NOT NULL,
                capability  TEXT NOT NULL,
                scope_type  TEXT NOT NULL,
                scope_value TEXT NOT NULL DEFAULT '',
                timestamp   INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS {i_aa}
                ON {at}(agent_type, capability);

            CREATE VIRTUAL TABLE IF NOT EXISTS {ft}
                USING fts5(content, role, session_id UNINDEXED, tokenize='porter ascii');

            CREATE TRIGGER IF NOT EXISTS {t_ins}
                AFTER INSERT ON {mt} BEGIN
                    INSERT INTO {ft}(rowid, content, role, session_id)
                    VALUES (new.id, new.content, new.role, new.session_id);
                END;

            CREATE TRIGGER IF NOT EXISTS {t_del}
                AFTER DELETE ON {mt} BEGIN
                    DELETE FROM {ft} WHERE rowid = old.id;
                END;"
        );

        self.conn.execute_batch(&sql)?;
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
        let st = self.sessions_table();

        let sql = format!("INSERT INTO {st} (id, parent_id, agent_type, created_at, last_active) VALUES (?1, ?2, ?3, ?4, ?4)");
        self.conn.execute(&sql, params![id, parent_id, agent_type, now])?;

        Ok(id)
    }

    pub fn get_latest_session(&self, agent_type: &str) -> Result<Option<SessionRecord>> {
        let st = self.sessions_table();
        let sql = format!(
            "SELECT id, parent_id, agent_type, created_at, last_active FROM {st} WHERE agent_type = ?1 ORDER BY last_active DESC LIMIT 1"
        );

        let mut stmt = self.conn.prepare(&sql)?;
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
        let mt = self.messages_table();
        let st = self.sessions_table();

        let insert_sql = format!("INSERT INTO {mt} (session_id, role, content, timestamp) VALUES (?1, ?2, ?3, ?4)");
        self.conn.execute(&insert_sql, params![session_id, role, content, now])?;

        let update_sql = format!("UPDATE {st} SET last_active = ?1 WHERE id = ?2");
        self.conn.execute(&update_sql, params![now, session_id])?;

        Ok(())
    }

    pub fn load_messages(&self, session_id: &str) -> Result<Vec<MessageRecord>> {
        let mt = self.messages_table();
        let sql = format!(
            "SELECT id, session_id, role, content, timestamp FROM {mt} WHERE session_id = ?1 ORDER BY timestamp ASC"
        );

        let mut stmt = self.conn.prepare(&sql)?;
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
        let st = self.sessions_table();
        let sql = format!(
            "SELECT id, parent_id, agent_type, created_at, last_active FROM {st} ORDER BY last_active DESC LIMIT ?1"
        );

        let mut stmt = self.conn.prepare(&sql)?;
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
        let at = self.approvals_table();
        let sql = format!(
            "INSERT INTO {at} (agent_type, capability, scope_type, scope_value, timestamp) VALUES (?1, ?2, ?3, ?4, ?5)"
        );
        self.conn.execute(&sql, params![agent_type, capability, scope_type, scope_value, now])?;
        Ok(())
    }

    pub fn load_approvals(&self, agent_type: &str) -> Result<Vec<ApprovalRecord>> {
        let at = self.approvals_table();
        let sql = format!(
            "SELECT id, agent_type, capability, scope_type, scope_value, timestamp FROM {at} WHERE agent_type = ?1 ORDER BY timestamp ASC"
        );

        let mut stmt = self.conn.prepare(&sql)?;
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
        let mt = self.messages_table();
        let ft = self.messages_fts_table();
        let st = self.sessions_table();

        let sql = match agent_type {
            Some(_) => format!(
                "SELECT m.id, m.session_id, m.role, m.content, m.timestamp
                FROM {mt} m
                JOIN {ft} fts ON m.id = fts.rowid
                JOIN {st} s ON m.session_id = s.id
                WHERE {ft} MATCH ?1 AND s.agent_type = ?3
                ORDER BY rank
                LIMIT ?2"
            ),
            None => format!(
                "SELECT m.id, m.session_id, m.role, m.content, m.timestamp
                FROM {mt} m
                JOIN {ft} fts ON m.id = fts.rowid
                WHERE {ft} MATCH ?1
                ORDER BY rank
                LIMIT ?2"
            ),
        };

        let mut stmt = self.conn.prepare(&sql)?;
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
        let store = SessionStore { conn, prefix: String::new() };
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
    fn test_open_with_prefix_creates_distinct_tables() {
        let spotlight = SessionStore::open_with_prefix_in_memory("spotlight_").unwrap();
        let main = SessionStore::open_with_prefix_in_memory("main_").unwrap();

        let sid_s = spotlight.create_session("agent1", None).unwrap();
        spotlight.append_message(&sid_s, "user", "spotlight message").unwrap();

        let sid_m = main.create_session("agent1", None).unwrap();
        main.append_message(&sid_m, "user", "main message").unwrap();

        let msgs_s = spotlight.load_messages(&sid_s).unwrap();
        let msgs_m = main.load_messages(&sid_m).unwrap();

        assert_eq!(msgs_s.len(), 1);
        assert_eq!(msgs_s[0].content, "spotlight message");
        assert_eq!(msgs_m.len(), 1);
        assert_eq!(msgs_m[0].content, "main message");
    }

    #[test]
    fn test_messages_isolated_by_prefix() {
        let spotlight = SessionStore::open_with_prefix_in_memory("spotlight_").unwrap();
        let main = SessionStore::open_with_prefix_in_memory("main_").unwrap();

        let sid_s = spotlight.create_session("agent1", None).unwrap();
        spotlight.append_message(&sid_s, "user", "secret task").unwrap();

        let sid_m = main.create_session("agent1", None).unwrap();
        main.append_message(&sid_m, "user", "public task").unwrap();

        // spotlight messages should NOT appear in main store
        let msgs_m = main.load_messages(&sid_s).unwrap();
        assert!(msgs_m.is_empty());
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

    #[test]
    fn test_prefix_validation_rejects_sql_injection() {
        let result = SessionStore::open_with_prefix_in_memory("x; DROP TABLE sessions;--");
        assert!(result.is_err());
    }

    #[test]
    fn test_prefix_validation_rejects_special_chars() {
        assert!(SessionStore::open_with_prefix_in_memory("bad prefix").is_err());
        assert!(SessionStore::open_with_prefix_in_memory("bad'prefix").is_err());
        assert!(SessionStore::open_with_prefix_in_memory("bad\"prefix").is_err());
    }

    #[test]
    fn test_prefix_validation_rejects_empty() {
        assert!(SessionStore::open_with_prefix_in_memory("").is_err());
    }

    #[test]
    fn test_prefix_validation_rejects_too_long() {
        let long = "a".repeat(33);
        assert!(SessionStore::open_with_prefix_in_memory(&long).is_err());
    }

    #[test]
    fn test_prefix_validation_accepts_valid() {
        assert!(SessionStore::open_with_prefix_in_memory("spotlight_").is_ok());
        assert!(SessionStore::open_with_prefix_in_memory("main_").is_ok());
        assert!(SessionStore::open_with_prefix_in_memory("agent1_").is_ok());
        assert!(SessionStore::open_with_prefix_in_memory("A").is_ok());
        assert!(SessionStore::open_with_prefix_in_memory("a1b2c3_").is_ok());
    }
}
