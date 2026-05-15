use super::request::ExecutionRequest;
use super::result::ExecutionResult;
use super::gate2_policy::normalize_workspace_path;
use anyhow::Result;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

#[derive(Clone)]
pub struct ExecutionJournal {
    pub path: PathBuf,
}

impl ExecutionJournal {
    pub fn open() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        let journal_path = home.join(".ageex").join("execution.journal");

        if let Some(parent) = journal_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        Ok(Self { path: journal_path })
    }

    pub fn log_request(&self, req: &ExecutionRequest) -> Result<()> {
        let mut normalized_args = req.args.clone();
        if let Some(path_val) = req.args.get("path").and_then(|v| v.as_str()) {
            normalized_args["path"] = serde_json::Value::String(normalize_workspace_path(path_val));
        }
        let entry = serde_json::json!({
            "type": "request",
            "request_id": req.request_id,
            "timestamp": req.timestamp,
            "agent_id": req.agent_id,
            "tool_name": req.tool_name,
            "capability": format!("{:?}", req.capability),
            "args": normalized_args,
        });
        self.append_line(&serde_json::to_string(&entry)?)
    }

    pub fn log_result(&self, res: &ExecutionResult) -> Result<()> {
        let entry = serde_json::json!({
            "type": "result",
            "request_id": res.request_id,
            "timestamp": Self::now(),
            "success": res.success,
            "gate1": res.gate1_passed,
            "gate2": res.gate2_passed,
            "gate3": res.gate3_passed,
            "exit_code": res.exit_code,
            "denied_reason": res.denied_reason,
            "execution_time_ms": res.execution_time_ms,
            "output_preview": &res.output[..res.output.len().min(200)],
        });
        self.append_line(&serde_json::to_string(&entry)?)
    }

    fn append_line(&self, line: &str) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    pub fn log_entry(
        &self,
        agent_id: &str,
        tool_name: &str,
        path: &str,
        success: bool,
        message: &str,
    ) -> Result<()> {
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
                writeln!(file, "{}", entry)?;
                Ok(())
            }
            Err(e) => Err(anyhow::anyhow!("[Journal] Open failed: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_journal_logs_destructive_severity() {
        let tmp = std::env::temp_dir().join(format!("test_delete_journal_{}.jsonl", uuid::Uuid::new_v4()));
        let journal = ExecutionJournal { path: tmp.clone() };
        
        journal.log_entry("agent2", "delete_file", "/tmp/test.txt", true, "Deleted file");
        
        let content = std::fs::read_to_string(&tmp).unwrap();
        assert!(content.contains("\"severity\":\"destructive\""));
        assert!(content.contains("\"tool_name\":\"delete_file\""));
        
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn test_journal_logs_normal_severity() {
        let tmp = std::env::temp_dir().join(format!("test_read_journal_{}.jsonl", uuid::Uuid::new_v4()));
        let journal = ExecutionJournal { path: tmp.clone() };
        
        journal.log_entry("agent1", "read_file", "/tmp/test.txt", true, "Read 100 bytes");
        
        let content = std::fs::read_to_string(&tmp).unwrap();
        assert!(content.contains("\"severity\":\"normal\""));
        
        std::fs::remove_file(&tmp).ok();
    }
}
