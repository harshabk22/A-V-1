use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use crate::capabilities::Capability;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRequest {
    pub request_id: String,
    pub agent_id: String,
    pub tool_name: String,
    pub capability: Capability,
    pub args: serde_json::Value,
    pub working_dir: PathBuf,
    pub timestamp: i64,
}

impl ExecutionRequest {
    pub fn new(
        agent_id: &str,
        tool_name: &str,
        capability: Capability,
        args: serde_json::Value,
    ) -> Self {
        Self {
            request_id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            tool_name: tool_name.to_string(),
            capability,
            args,
            working_dir: Self::default_workspace(),
            timestamp: Self::now(),
        }
    }

    fn default_workspace() -> PathBuf {
        dirs::home_dir()
            .expect("Cannot determine home directory")
            .join("Desktop")
            .join("ageex-workspace")
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }
}
