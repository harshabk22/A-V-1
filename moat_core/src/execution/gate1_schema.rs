use super::request::ExecutionRequest;
use super::result::ExecutionResult;
use anyhow::{Result, bail};
use regex::Regex;
use std::sync::LazyLock;

static SECRET_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| vec![
    Regex::new(r"sk-[a-zA-Z0-9]{48}").unwrap(),
    Regex::new(r"ghp_[a-zA-Z0-9]{36}").unwrap(),
    Regex::new(r"nvapi-[a-zA-Z0-9]+").unwrap(),
    Regex::new(r#"(?i)password["']?\s*[:=]\s*["']?[^\s"']+"#).unwrap(),
    Regex::new(r#"(?i)token["']?\s*[:=]\s*["']?[^\s"']+"#).unwrap(),
]);

pub struct Gate1Schema;

impl Gate1Schema {
    pub fn validate(mut req: ExecutionRequest) -> Result<ExecutionRequest, ExecutionResult> {
        let start = std::time::Instant::now();

        if !req.args.is_object() {
            return Err(ExecutionResult::gate1_denied(
                &req.request_id,
                "Tool args must be a JSON object"
            ));
        }

        if let Err(e) = Self::validate_tool_schema(&req.tool_name, &req.args) {
            return Err(ExecutionResult::gate1_denied(
                &req.request_id,
                &format!("Schema validation failed: {}", e)
            ));
        }

        req.args = Self::redact_json_secrets(req.args);

        eprintln!("[Gate1] ✓ Schema valid, secrets redacted ({:.1}ms)", start.elapsed().as_secs_f64() * 1000.0);
        Ok(req)
    }

    fn validate_tool_schema(tool_name: &str, args: &serde_json::Value) -> Result<()> {
        match tool_name {
            "read_file" | "fs_read" => {
                if args.get("path").and_then(|v| v.as_str()).is_none() {
                    bail!("read_file requires 'path' (string)");
                }
            },
            "write_file" | "fs_write" => {
                if args.get("path").and_then(|v| v.as_str()).is_none() {
                    bail!("write_file requires 'path' (string)");
                }
                if args.get("content").and_then(|v| v.as_str()).is_none() {
                    bail!("write_file requires 'content' (string)");
                }
            },
            "process_spawn" => {
                if args.get("command").and_then(|v| v.as_str()).is_none() {
                    bail!("process_spawn requires 'command' (string)");
                }
            },
            _ => {
                // Unknown tool — pass through (allow dynamic tools)
            }
        }
        Ok(())
    }

    fn redact_json_secrets(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::String(s) => {
                serde_json::Value::String(Self::redact_secrets(&s))
            },
            serde_json::Value::Object(mut map) => {
                for (_, v) in map.iter_mut() {
                    *v = Self::redact_json_secrets(v.clone());
                }
                serde_json::Value::Object(map)
            },
            serde_json::Value::Array(arr) => {
                serde_json::Value::Array(
                    arr.into_iter().map(Self::redact_json_secrets).collect()
                )
            },
            other => other,
        }
    }

    fn redact_secrets(text: &str) -> String {
        let mut result = text.to_string();
        for pattern in SECRET_PATTERNS.iter() {
            result = pattern.replace_all(&result, "[REDACTED]").to_string();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::Capability;

    #[test]
    fn test_redact_openai_key() {
        let text = "OPENAI_API_KEY=sk-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKL";
        let redacted = Gate1Schema::redact_secrets(text);
        assert!(redacted.contains("[REDACTED]"));
        assert!(!redacted.contains("sk-abc"));
    }

    #[test]
    fn test_validate_read_file_schema() {
        let req = ExecutionRequest::new(
            "agent1",
            "read_file",
            Capability::FileRead,
            serde_json::json!({"path": "/workspace/test.txt"})
        );
        assert!(Gate1Schema::validate(req).is_ok());
    }

    #[test]
    fn test_reject_invalid_schema() {
        let req = ExecutionRequest::new(
            "agent2",
            "write_file",
            Capability::FileWrite,
            serde_json::json!({"path": "/workspace/test.txt"})
        );
        let result = Gate1Schema::validate(req);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.gate1_passed, false);
    }
}
