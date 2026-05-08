use crate::orchestrator::ActionIntent;
use crate::{MoatError, MoatResult};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DfaState {
    SeekingObject,
    InString,
    Escape,
    Balanced,
}

pub fn extract_action_json(raw: &str) -> Option<String> {
    let candidate = extract_action_block(raw).unwrap_or_else(|| raw.trim().to_string());
    let candidate = strip_to_json_object(&candidate);
    if candidate.is_empty() {
        return None;
    }
    let mut state = DfaState::SeekingObject;
    let mut depth = 0usize;
    let mut start = None;
    for (idx, ch) in candidate.char_indices() {
        match state {
            DfaState::SeekingObject => {
                if ch == '{' {
                    start = Some(idx);
                    depth = 1;
                    state = DfaState::Balanced;
                }
            }
            DfaState::Balanced => match ch {
                '"' => state = DfaState::InString,
                '{' => depth += 1,
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        let start = start?;
                        return Some(candidate[start..=idx].trim().to_string());
                    }
                }
                _ => {}
            },
            DfaState::InString => match ch {
                '\\' => state = DfaState::Escape,
                '"' => state = DfaState::Balanced,
                _ => {}
            },
            DfaState::Escape => state = DfaState::InString,
        }
    }
    None
}

pub fn strip_to_json_object(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|&c| c != '\u{FEFF}' && c != '\u{200B}' && c != '\u{200C}' && c != '\u{00A0}')
        .collect::<String>()
        .replace("\r\n", "\n")
        .replace('\r', "\n");

    let after_fence: &str = {
        let trimmed = cleaned.trim_start();
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("```json") {
            let rest = &trimmed[7..];
            rest.strip_prefix('\n').unwrap_or(rest)
        } else if lower.starts_with("```") {
            let rest = &trimmed[3..];
            rest.strip_prefix('\n').unwrap_or(rest)
        } else {
            trimmed
        }
    };

    let start = match after_fence.find('{') {
        Some(i) => i,
        None => return String::new(),
    };

    let end = match after_fence.rfind('}') {
        Some(i) if i >= start => i,
        _ => return String::new(),
    };

    after_fence[start..=end].to_owned()
}

pub fn parse_action_intent(raw: &str) -> MoatResult<ActionIntent> {
    let json_text = extract_action_json(raw).ok_or_else(|| {
        MoatError::Orchestrator("parser DFA could not locate an action object".into())
    })?;
    let mut value: Value = serde_json::from_str(&json_text)
        .map_err(|err| MoatError::Orchestrator(format!("parser JSON decode failed: {err}")))?;
    apply_field_defaults(&mut value);
    value_to_intent(value)
}

pub fn safe_retry_intent() -> ActionIntent {
    ActionIntent {
        thought: Some("Parser fallback: unable to decode action safely.".into()),
        action: "system:parse_error".into(),
        target: "parser".into(),
        payload: Some(Value::String(
            "[SYSTEM: Your last response could not be parsed. You MUST respond with a valid <action> JSON block. Try again.]".into(),
        )),
    }
}

pub fn apply_field_defaults(value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        *value = json!({
            "action": "done",
            "target": Value::Null,
            "payload": Value::Null,
        });
        return;
    };

    let has_action = object
        .get("action")
        .and_then(Value::as_str)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    if !has_action {
        object.insert("action".into(), Value::String("done".into()));
    }
    object.entry("target").or_insert(Value::Null);
    object.entry("payload").or_insert(Value::Null);
}

pub fn value_to_intent(mut value: Value) -> MoatResult<ActionIntent> {
    apply_field_defaults(&mut value);
    let object = value
        .as_object()
        .ok_or_else(|| MoatError::Orchestrator("parser value was not an object".into()))?;

    let thought = object
        .get("_thought")
        .or_else(|| object.get("thought"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let raw_action = object
        .get("action")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "done".into());
    let (action, aliased_action) = match raw_action.trim().to_ascii_lowercase().as_str() {
        "text" | "chat" | "reply" | "answer" => ("done".to_string(), true),
        _ => (raw_action, false),
    };
    let target = object
        .get("target")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| {
            if aliased_action {
                "user".to_string()
            } else {
                value
            }
        })
        .unwrap_or_else(|| {
            if action == "done" || aliased_action {
                "user".to_string()
            } else {
                String::new()
            }
        });
    Ok(ActionIntent {
        thought,
        action,
        target,
        payload: object.get("payload").cloned(),
    })
}

fn extract_action_block(raw: &str) -> Option<String> {
    let cleaned = raw
        .replace('\u{feff}', "")
        .replace('\u{200b}', "")
        .replace("```json", "")
        .replace("```JSON", "")
        .replace("```", "");
    let lower = cleaned.to_ascii_lowercase();
    let start_tag = "<action>";
    let end_tag = "</action>";
    let start = lower.find(start_tag)?;
    let end = lower.rfind(end_tag)?;
    if end <= start {
        return None;
    }
    Some(cleaned[start + start_tag.len()..end].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::{extract_action_json, parse_action_intent, strip_to_json_object};

    #[test]
    fn dfa_extracts_balanced_json_inside_action_block() {
        let raw = "noise <action>{\"action\":\"done\",\"target\":\"user\",\"payload\":\"ok\"}</action> trailer";
        let json = extract_action_json(raw).expect("extract json");
        assert!(json.starts_with('{'));
        assert!(json.ends_with('}'));
    }

    #[test]
    fn parser_handles_braces_inside_strings() {
        let raw = "<action>{\"action\":\"done\",\"target\":\"user\",\"payload\":\"literal { brace } text\"}</action>";
        let intent = parse_action_intent(raw).expect("parse intent");
        assert_eq!(intent.action, "done");
        assert_eq!(intent.target, "user");
    }

    #[test]
    fn strip_to_json_object_removes_markdown_and_prose() {
        let raw = "Here you go\n```json\n{\"action\":\"done\",\"target\":\"user\",\"payload\":\"ok\"}\n```\nThanks";
        let stripped = strip_to_json_object(raw);
        assert_eq!(
            stripped,
            "{\"action\":\"done\",\"target\":\"user\",\"payload\":\"ok\"}"
        );
    }
}
