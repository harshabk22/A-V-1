use crate::config;
use crate::orchestrator::ActionIntent;
use crate::{MoatError, MoatResult};
use serde_json::Value;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use url::Url;

const URL_OPEN_SCHEMES: &[&str] = &["http", "https", "mailto", "whatsapp", "facetime"];
const CURL_SCHEMES: &[&str] = &["http", "https"];

pub mod intent {
    pub use crate::orchestrator::ActionIntent;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePath(PathBuf);

impl WorkspacePath {
    pub fn validate(raw: &str) -> MoatResult<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(MoatError::SecurityViolation(
                "workspace path cannot be empty".into(),
            ));
        }
        if raw.contains('\0') {
            return Err(MoatError::SecurityViolation(
                "workspace path contains a NUL byte".into(),
            ));
        }

        let path = PathBuf::from(raw);
        if path.is_absolute() {
            return Err(MoatError::SecurityViolation(format!(
                "workspace path must be relative, got '{}'",
                raw
            )));
        }

        for component in path.components() {
            match component {
                Component::Normal(_) | Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(MoatError::SecurityViolation(format!(
                        "workspace path '{}' escapes the scoped workspace",
                        raw
                    )));
                }
            }
        }

        Ok(Self(path))
    }

    pub fn from_target(raw: &str) -> MoatResult<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(MoatError::SecurityViolation(
                "workspace target cannot be empty".into(),
            ));
        }

        let raw_path = PathBuf::from(raw);
        if raw_path.is_absolute() {
            let root = config::workspace_root();
            let relative = raw_path.strip_prefix(&root).map_err(|_| {
                MoatError::SecurityViolation(format!(
                    "absolute target '{}' is outside the scoped workspace '{}'",
                    raw,
                    root.display()
                ))
            })?;
            return Self::validate(&relative.to_string_lossy());
        }

        Self::validate(raw)
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl fmt::Display for WorkspacePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedSchemeUrl {
    raw: String,
    parsed: Url,
}

impl AllowedSchemeUrl {
    pub fn new(raw: &str, allowed_schemes: &[&str]) -> MoatResult<Self> {
        let parsed = Url::parse(raw)
            .map_err(|err| MoatError::Orchestrator(format!("invalid URL '{raw}': {err}")))?;
        let scheme = parsed.scheme().to_ascii_lowercase();
        if !allowed_schemes.iter().any(|allowed| scheme == *allowed) {
            return Err(MoatError::SecurityViolation(format!(
                "scheme '{}' is not permitted for URL '{}'",
                parsed.scheme(),
                raw
            )));
        }
        Ok(Self {
            raw: raw.to_string(),
            parsed,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn host_str(&self) -> Option<&str> {
        self.parsed.host_str()
    }

    pub fn scheme(&self) -> &str {
        self.parsed.scheme()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentPayload {
    None,
    FileRead {
        path: WorkspacePath,
    },
    FileWrite {
        path: WorkspacePath,
        contents: String,
    },
    FileDelete {
        path: WorkspacePath,
    },
    AppLaunch {
        app_name: String,
    },
    ProcessSpawn {
        executable: String,
        args: Vec<String>,
    },
    OpenUrl {
        url: AllowedSchemeUrl,
    },
    AppleScript {
        script: String,
    },
    InjectText {
        text: String,
        expected_app_name: String,
    },
    Curl {
        url: AllowedSchemeUrl,
        method: String,
        body: String,
        headers: Vec<(String, String)>,
    },
    MemorySearch {
        query: String,
    },
    SessionSearch {
        query: String,
    },
    MemoryLearn {
        keywords: String,
        skill_description: String,
    },
    ProposeSkill {
        skill_name: String,
        description: String,
        source_code: String,
        language: String,
        parameter_schema_json: String,
    },
    QueryBrain {
        keyword: String,
    },
    Delegate {
        target_agent_id: String,
        task: String,
        specialization: Option<String>,
    },
    AskChoice {
        context: String,
        options: Vec<String>,
    },
    Done {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedIntent {
    pub action: String,
    pub target: String,
    pub payload: IntentPayload,
}

impl TypedIntent {
    pub fn from_action_intent(intent: &ActionIntent) -> MoatResult<Self> {
        let payload = match intent.action.as_str() {
            "fs:read" => IntentPayload::FileRead {
                path: WorkspacePath::from_target(intent.target.trim())?,
            },
            "fs:write" => IntentPayload::FileWrite {
                path: WorkspacePath::from_target(intent.target.trim())?,
                contents: payload_as_string(intent.payload.as_ref()),
            },
            "fs:delete" => IntentPayload::FileDelete {
                path: WorkspacePath::from_target(intent.target.trim())?,
            },
            "app:launch" => IntentPayload::AppLaunch {
                app_name: intent.target.trim().to_string(),
            },
            "process:spawn" => IntentPayload::ProcessSpawn {
                executable: intent.target.trim().to_string(),
                args: payload_as_string_vec(intent.payload.as_ref())?,
            },
            "os:open_url" => IntentPayload::OpenUrl {
                url: AllowedSchemeUrl::new(intent.target.trim(), URL_OPEN_SCHEMES)?,
            },
            "os:applescript" => IntentPayload::AppleScript {
                script: payload_as_string(intent.payload.as_ref()),
            },
            "os:inject_text" => {
                let payload = intent.payload.as_ref().ok_or_else(|| {
                    MoatError::Orchestrator(
                        "os:inject_text requires payload {\"text_to_inject\":\"...\",\"expected_app_name\":\"...\"}".into(),
                    )
                })?;
                let Some(object) = payload.as_object() else {
                    return Err(MoatError::Orchestrator(
                        "os:inject_text payload must be a JSON object".into(),
                    ));
                };
                let text = object
                    .get("text_to_inject")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let expected_app_name = object
                    .get("expected_app_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if text.is_empty() {
                    return Err(MoatError::Orchestrator(
                        "os:inject_text requires non-empty text_to_inject".into(),
                    ));
                }
                if expected_app_name.is_empty() {
                    return Err(MoatError::Orchestrator(
                        "os:inject_text requires non-empty expected_app_name".into(),
                    ));
                }
                IntentPayload::InjectText {
                    text,
                    expected_app_name,
                }
            }
            "net:curl" => {
                let (method, body, headers) = payload_as_curl(intent.payload.as_ref());
                IntentPayload::Curl {
                    url: AllowedSchemeUrl::new(intent.target.trim(), CURL_SCHEMES)?,
                    method,
                    body,
                    headers,
                }
            }
            "memory:search" => IntentPayload::MemorySearch {
                query: payload_as_string(intent.payload.as_ref()),
            },
            "session:search" => IntentPayload::SessionSearch {
                query: payload_as_string(intent.payload.as_ref()),
            },
            "memory:learn" => {
                let payload = intent.payload.as_ref().ok_or_else(|| {
                    MoatError::Orchestrator(
                        "memory:learn requires payload {\"keywords\":\"...\",\"skill_description\":\"...\"}"
                            .into(),
                    )
                })?;
                let Some(object) = payload.as_object() else {
                    return Err(MoatError::Orchestrator(
                        "memory:learn payload must be a JSON object".into(),
                    ));
                };
                let keywords = object
                    .get("keywords")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let skill_description = object
                    .get("skill_description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                IntentPayload::MemoryLearn {
                    keywords,
                    skill_description,
                }
            }
            "propose_system_skill" => {
                let payload = intent.payload.as_ref().ok_or_else(|| {
                    MoatError::Orchestrator(
                        "propose_system_skill requires payload {\"skill_name\":\"...\",\"description\":\"...\",\"source_code\":\"...\",\"language\":\"python|bash\",\"parameters\":{...}}"
                            .into(),
                    )
                })?;
                let Some(object) = payload.as_object() else {
                    return Err(MoatError::Orchestrator(
                        "propose_system_skill payload must be a JSON object".into(),
                    ));
                };
                let skill_name = object
                    .get("skill_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let description = object
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let source_code = object
                    .get("source_code")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let language = object
                    .get("language")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let parameters = object.get("parameters").ok_or_else(|| {
                    MoatError::Orchestrator(
                        "propose_system_skill requires a 'parameters' JSON object".into(),
                    )
                })?;
                if !parameters.is_object() {
                    return Err(MoatError::Orchestrator(
                        "propose_system_skill parameters must be a JSON object".into(),
                    ));
                }
                let parameter_schema_json =
                    serde_json::to_string(parameters).map_err(MoatError::Serde)?;
                IntentPayload::ProposeSkill {
                    skill_name,
                    description,
                    source_code,
                    language,
                    parameter_schema_json,
                }
            }
            "meta:query_brain" => IntentPayload::QueryBrain {
                keyword: payload_as_string(intent.payload.as_ref()),
            },
            "swarm:delegate" | "request_sub_agent" | "system:spawn_worker" => {
                let payload = intent.payload.as_ref().ok_or_else(|| {
                    MoatError::Orchestrator(
                        "system:spawn_worker requires payload {\"task\":\"...\",\"specialization\":\"coder\"} or {\"target_agent_id\":\"coder\",\"task\":\"...\"}"
                            .into(),
                    )
                })?;
                let Some(object) = payload.as_object() else {
                    return Err(MoatError::Orchestrator(
                        "system:spawn_worker payload must be a JSON object".into(),
                    ));
                };
                IntentPayload::Delegate {
                    target_agent_id: object
                        .get("agent_id")
                        .or_else(|| object.get("target_agent_id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                    task: object
                        .get("task_instruction")
                        .or_else(|| object.get("task"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                    specialization: object
                        .get("specialization")
                        .or_else(|| object.get("role"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToOwned::to_owned),
                }
            }
            "user:ask_choice" => {
                let payload = intent.payload.as_ref().ok_or_else(|| {
                    MoatError::Orchestrator(
                        "user:ask_choice requires payload {\"context\":\"...\",\"options\":[...]}."
                            .into(),
                    )
                })?;
                let Some(object) = payload.as_object() else {
                    return Err(MoatError::Orchestrator(
                        "user:ask_choice payload must be a JSON object".into(),
                    ));
                };
                let context = object
                    .get("context")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let options = object
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(|value| value.trim().to_string())
                            .filter(|value| !value.is_empty())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                IntentPayload::AskChoice { context, options }
            }
            "done" => IntentPayload::Done {
                message: payload_as_string(intent.payload.as_ref()),
            },
            _ => IntentPayload::None,
        };

        Ok(Self {
            action: intent.action.clone(),
            target: intent.target.clone(),
            payload,
        })
    }
}

fn payload_as_string(payload: Option<&Value>) -> String {
    match payload {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(value)) => value.clone(),
        Some(other) => other.to_string(),
    }
}

fn payload_as_string_vec(payload: Option<&Value>) -> MoatResult<Vec<String>> {
    match payload {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value.as_str().map(|text| text.to_string()).ok_or_else(|| {
                    MoatError::Orchestrator(
                        "process:spawn payload must be an array of strings".into(),
                    )
                })
            })
            .collect(),
        Some(Value::String(value)) => Ok(value.split_whitespace().map(str::to_owned).collect()),
        Some(_) => Err(MoatError::Orchestrator(
            "process:spawn payload must be an array of strings".into(),
        )),
    }
}

fn payload_as_curl(payload: Option<&Value>) -> (String, String, Vec<(String, String)>) {
    let Some(Value::Object(object)) = payload else {
        return ("GET".to_string(), String::new(), Vec::new());
    };
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .trim()
        .to_ascii_uppercase();
    let body = object
        .get("body")
        .map(|value| payload_as_string(Some(value)))
        .unwrap_or_default();
    let headers = object
        .get("headers")
        .and_then(Value::as_object)
        .map(|headers| {
            headers
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|text| (key.clone(), text.to_string()))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (method, body, headers)
}

#[cfg(test)]
mod tests {
    use super::{AllowedSchemeUrl, IntentPayload, TypedIntent, WorkspacePath};
    use crate::config;
    use crate::orchestrator::ActionIntent;
    use serde_json::json;

    #[test]
    fn workspace_path_accepts_relative_scoped_path() {
        let path = WorkspacePath::validate("notes/today.md").expect("workspace path");
        assert_eq!(path.as_path(), std::path::Path::new("notes/today.md"));
    }

    #[test]
    fn workspace_path_accepts_absolute_path_inside_workspace_by_normalizing() {
        let absolute = config::workspace_root().join("notes/today.md");
        let path = WorkspacePath::from_target(&absolute.to_string_lossy()).expect("workspace path");
        assert_eq!(path.as_path(), std::path::Path::new("notes/today.md"));
    }

    #[test]
    fn workspace_path_rejects_parent_escape() {
        let err = WorkspacePath::validate("../../etc/passwd").expect_err("reject escape");
        assert!(err.to_string().contains("workspace"));
    }

    #[test]
    fn allowed_scheme_url_rejects_unknown_scheme() {
        let err = AllowedSchemeUrl::new("ftp://example.com", &["https"]).expect_err("reject ftp");
        assert!(err.to_string().contains("scheme"));
    }

    #[test]
    fn typed_intent_accepts_system_spawn_worker_specialization_payload() {
        let intent = ActionIntent {
            thought: None,
            action: "system:spawn_worker".into(),
            target: "swarm".into(),
            payload: Some(json!({
                "task": "inspect the workspace history",
                "specialization": "coder"
            })),
        };

        let typed = TypedIntent::from_action_intent(&intent).expect("typed intent");
        match typed.payload {
            IntentPayload::Delegate {
                target_agent_id,
                task,
                specialization,
            } => {
                assert!(target_agent_id.is_empty());
                assert_eq!(task, "inspect the workspace history");
                assert_eq!(specialization.as_deref(), Some("coder"));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }
}
