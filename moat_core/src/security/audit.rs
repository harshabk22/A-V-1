use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::Mutex;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuditError {
    #[error("audit db error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("audit io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub enum AuditEventType {
    SecurityViolation,
    ConsentDenied,
    ConsentGranted,
    ConsentPermanent,
    InjectionDetected,
    SecretDetected,
    PiiDetected,
    PathProtected,
    SymlinkDenied,
    DepthExceeded,
    FuelExhausted,
    CapabilityDenied,
}

impl AuditEventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SecurityViolation => "security_violation",
            Self::ConsentDenied => "consent_denied",
            Self::ConsentGranted => "consent_granted",
            Self::ConsentPermanent => "consent_permanent",
            Self::InjectionDetected => "injection_detected",
            Self::SecretDetected => "secret_detected",
            Self::PiiDetected => "pii_detected",
            Self::PathProtected => "path_protected",
            Self::SymlinkDenied => "symlink_denied",
            Self::DepthExceeded => "depth_exceeded",
            Self::FuelExhausted => "fuel_exhausted",
            Self::CapabilityDenied => "capability_denied",
        }
    }
}

pub struct AuditLogger {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl AuditLogger {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, AuditError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS audit_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp_ns INTEGER NOT NULL,
                event_type TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                summary TEXT NOT NULL,
                details TEXT,
                severity TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON audit_log(timestamp_ns);
            CREATE INDEX IF NOT EXISTS idx_audit_type ON audit_log(event_type);
            CREATE INDEX IF NOT EXISTS idx_audit_agent ON audit_log(agent_id);"
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            path,
        })
    }

    pub fn log_event(
        &self,
        event_type: AuditEventType,
        agent_id: &str,
        summary: &str,
        details: Option<&str>,
        severity: &str,
    ) -> Result<(), AuditError> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let timestamp_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64;
        conn.execute(
            "INSERT INTO audit_log (timestamp_ns, event_type, agent_id, summary, details, severity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                timestamp_ns,
                event_type.as_str(),
                agent_id,
                summary,
                details.unwrap_or(""),
                severity,
            ],
        )?;
        Ok(())
    }

    pub fn event_count(&self, event_type: Option<AuditEventType>) -> Result<i64, AuditError> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let count: i64 = if let Some(et) = event_type {
            conn.query_row(
                "SELECT COUNT(*) FROM audit_log WHERE event_type = ?1",
                params![et.as_str()],
                |row| row.get(0),
            )?
        } else {
            conn.query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))?
        };
        Ok(count)
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ageex_audit_test_{}.db", name))
    }

    #[test]
    fn test_audit_log_consent_event() {
        let path = temp_path("consent");
        let logger = AuditLogger::open(&path).expect("open audit db");
        logger.log_event(
            AuditEventType::ConsentDenied,
            "agent2",
            "Delete /tmp/test.txt denied",
            Some("consent not granted by user"),
            "major",
        ).expect("log event");
        let count = logger.event_count(Some(AuditEventType::ConsentDenied)).expect("count");
        assert_eq!(count, 1);
        drop(logger);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_audit_log_multiple_events() {
        let path = temp_path("multi");
        let logger = AuditLogger::open(&path).expect("open audit db");
        logger.log_event(AuditEventType::InjectionDetected, "agent3", "injection in WhatsApp msg", None, "critical").unwrap();
        logger.log_event(AuditEventType::SecretDetected, "agent2", "API key in output", None, "critical").unwrap();
        logger.log_event(AuditEventType::PathProtected, "agent1", "attempted read of .pem", None, "major").unwrap();

        let total = logger.event_count(None).expect("count");
        assert_eq!(total, 3);
        drop(logger);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_audit_logger_reuses_existing_db() {
        let path = temp_path("reuse");
        let logger = AuditLogger::open(&path).expect("open");
        logger.log_event(AuditEventType::SymlinkDenied, "agent2", "symlink blocked", None, "major").unwrap();
        drop(logger);

        let logger2 = AuditLogger::open(&path).expect("reopen");
        let count = logger2.event_count(None).expect("count");
        assert_eq!(count, 1, "Events must persist across reopen");
        drop(logger2);
        let _ = std::fs::remove_file(&path);
    }
}
