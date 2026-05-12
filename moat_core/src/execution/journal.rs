use std::fs::OpenOptions;
use std::io::Write;

pub struct ExecutionJournal {
    pub path: std::path::PathBuf,
}

impl ExecutionJournal {
    pub fn open() -> std::io::Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "No home directory"))?;
        let path = home.join(".ageex").join("execution.journal");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self { path })
    }

    pub fn log_entry(
        &self,
        agent_id: &str,
        tool_name: &str,
        path: &str,
        success: bool,
        message: &str,
    ) {
        let severity = match tool_name {
            "delete_file" | "fs_delete" => "destructive",
            _ => "normal",
        };

        let entry = serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "agent_id": agent_id,
            "tool_name": tool_name,
            "path": path,
            "success": success,
            "severity": severity,
            "message": message,
        });

        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(mut file) => {
                if let Err(e) = writeln!(file, "{}", entry) {
                    eprintln!("[Journal] Write failed: {}", e);
                }
            }
            Err(e) => {
                eprintln!("[Journal] Open failed: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_journal_logs_destructive_severity_for_delete() {
        let tmp = std::env::temp_dir().join("test_delete_journal.jsonl");
        let journal = ExecutionJournal { path: tmp.clone() };

        journal.log_entry("agent2", "delete_file", "/tmp/test.txt", true, "Deleted /tmp/test.txt");

        let content = std::fs::read_to_string(&tmp).unwrap();
        assert!(content.contains("\"severity\":\"destructive\""),
            "Delete should have destructive severity, got: {}", content);
        assert!(content.contains("\"tool_name\":\"delete_file\""));

        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn test_journal_logs_normal_severity_for_read() {
        let tmp = std::env::temp_dir().join("test_read_journal.jsonl");
        let journal = ExecutionJournal { path: tmp.clone() };

        journal.log_entry("agent1", "read_file", "/tmp/test.txt", true, "Read 100 bytes");

        let content = std::fs::read_to_string(&tmp).unwrap();
        assert!(content.contains("\"severity\":\"normal\""),
            "Read should have normal severity, got: {}", content);

        std::fs::remove_file(&tmp).ok();
    }
}
