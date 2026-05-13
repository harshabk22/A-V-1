use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryTemplate {
    pub active_task: String,
    pub completed_actions: Vec<String>,
    pub current_state: String,
    pub pending_items: Vec<String>,
    pub blockers: Vec<String>,
}

impl SummaryTemplate {
    pub fn empty() -> Self {
        Self {
            active_task: "None".into(),
            completed_actions: vec![],
            current_state: "Initial state".into(),
            pending_items: vec![],
            blockers: vec![],
        }
    }

    pub fn to_message_content(&self) -> String {
        format!(
            "[CONTEXT SUMMARY]\n             Active Task: {}\n             Completed Actions:\n{}\n             Current State: {}\n             Pending: {}\n             Blockers: {}",
            self.active_task,
            self.completed_actions.iter()
                .map(|a| format!("  • {}", a))
                .collect::<Vec<_>>()
                .join("\n"),
            self.current_state,
            if self.pending_items.is_empty() {
                "  None".to_string()
            } else {
                self.pending_items.iter()
                    .map(|p| format!("  • {}", p))
                    .collect::<Vec<_>>()
                    .join("\n")
            },
            if self.blockers.is_empty() {
                "  None".to_string()
            } else {
                self.blockers.iter()
                    .map(|b| format!("  • {}", b))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        )
    }
}