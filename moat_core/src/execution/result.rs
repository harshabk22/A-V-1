use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub request_id: String,
    pub success: bool,
    pub gate1_passed: bool,
    pub gate2_passed: bool,
    pub gate3_passed: bool,
    pub output: String,
    pub exit_code: Option<i32>,
    pub denied_reason: Option<String>,
    pub execution_time_ms: u64,
}

impl ExecutionResult {
    pub fn gate1_denied(request_id: &str, reason: &str) -> Self {
        Self {
            request_id: request_id.to_string(),
            success: false,
            gate1_passed: false,
            gate2_passed: false,
            gate3_passed: false,
            output: String::new(),
            exit_code: None,
            denied_reason: Some(reason.to_string()),
            execution_time_ms: 0,
        }
    }

    pub fn gate2_denied(request_id: &str, reason: &str) -> Self {
        Self {
            request_id: request_id.to_string(),
            success: false,
            gate1_passed: true,
            gate2_passed: false,
            gate3_passed: false,
            output: String::new(),
            exit_code: None,
            denied_reason: Some(reason.to_string()),
            execution_time_ms: 0,
        }
    }

    pub fn gate3_denied(request_id: &str, reason: &str) -> Self {
        Self {
            request_id: request_id.to_string(),
            success: false,
            gate1_passed: true,
            gate2_passed: true,
            gate3_passed: false,
            output: String::new(),
            exit_code: None,
            denied_reason: Some(reason.to_string()),
            execution_time_ms: 0,
        }
    }

    pub fn success(
        request_id: &str,
        output: String,
        exit_code: Option<i32>,
        elapsed_ms: u64,
    ) -> Self {
        Self {
            request_id: request_id.to_string(),
            success: true,
            gate1_passed: true,
            gate2_passed: true,
            gate3_passed: true,
            output,
            exit_code,
            denied_reason: None,
            execution_time_ms: elapsed_ms,
        }
    }
}
