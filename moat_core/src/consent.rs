use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::meta_brain::ExecutionPolicy;
use crate::orchestrator::ActionIntent;
use crate::rule_store::RuleConflictReview;
use crate::skills::{sha256_hex, SkillProposal};
use crate::types::{IntentPayload, TypedIntent};
use crate::{MoatError, MoatResult};

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConsentDecision {
    ApproveOnce,
    Deny,
    CreatePermanentPolicy,
}

impl ConsentDecision {
    pub fn is_approved(self) -> bool {
        !matches!(self, Self::Deny)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SubSpawnPolicyRequest {
    pub source_worker: String,
    pub target_worker: String,
    pub allowed_tool: String,
    pub can_create_permanent_policy: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillReviewRequest {
    pub skill_name: String,
    pub description: String,
    pub source_code: String,
    pub code_hash: String,
    pub language: String,
    pub parameter_schema: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConsentRequest {
    pub request_id: String,
    pub session_id: String,
    pub action: String,
    pub summary: String,
    pub payload: String,
    pub summary_hash: String,
    pub payload_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_review: Option<RuleConflictReview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub_spawn_policy: Option<SubSpawnPolicyRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_review: Option<SkillReviewRequest>,
}

pub type EmitFn = Box<dyn Fn(ConsentRequest) + Send + Sync + 'static>;

pub struct ConsentBridge {
    pending: Mutex<HashMap<String, oneshot::Sender<ConsentDecision>>>,
    /// Pending user:ask_choice requests — oneshot resolves with the selected option string.
    choice_pending: Mutex<HashMap<String, oneshot::Sender<String>>>,
    emit: EmitFn,
}

#[derive(Serialize)]
struct GenericConsentPayload<'a> {
    action: &'a str,
    payload: &'a str,
}

#[cfg(test)]
mod tests {
    use super::{build_consent_request, build_generic_consent_request, ConsentRequest};
    use crate::orchestrator::ActionIntent;
    use serde_json::json;

    #[test]
    fn consent_request_json_contract_roundtrip() {
        let req = ConsentRequest {
            request_id: "abc-123".into(),
            session_id: "session-1".into(),
            action: "os:applescript".into(),
            summary: "Agent 'core_os' requests OS shell execution: `osascript tell application \\\"Finder\\\" to activate`".into(),
            payload: "eyJhY3Rpb24iOiJvczphcHBsZXNjcmlwdCJ9".into(),
            summary_hash: "hash-a".into(),
            payload_hash: "hash-b".into(),
            rule_review: None,
            sub_spawn_policy: None,
            skill_review: None,
        };

        let json = serde_json::to_string(&req).expect("serialize consent request");
        assert!(json.contains("\"request_id\""));
        assert!(json.contains("\"session_id\""));
        assert!(json.contains("\"action\""));
        assert!(json.contains("\"payload\""));

        let decoded: ConsentRequest =
            serde_json::from_str(&json).expect("deserialize consent request");
        assert_eq!(decoded.request_id, req.request_id);
        assert_eq!(decoded.session_id, req.session_id);
        assert_eq!(decoded.action, req.action);
        assert_eq!(decoded.summary, req.summary);
        assert_eq!(decoded.payload, req.payload);
        assert_eq!(decoded.summary_hash, req.summary_hash);
        assert_eq!(decoded.payload_hash, req.payload_hash);
    }

    #[test]
    fn structured_consent_hashes_are_distinct() {
        let intent = ActionIntent {
            thought: None,
            action: "fs:write".into(),
            target: "notes/today.md".into(),
            payload: Some(json!("hello world")),
        };

        let req = build_consent_request(&intent, "coder", "session-1").expect("build consent");
        assert_ne!(req.summary_hash, req.payload_hash);
    }

    #[test]
    fn generic_consent_hashes_are_distinct() {
        let req = build_generic_consent_request(
            "permission:file",
            "Agent 'system' requests permission escalation.".into(),
            serde_json::to_vec(&json!({"action":"permission:file","payload":"Grant read access"}))
                .expect("serialize"),
            "session-2",
        )
        .expect("build generic consent");
        assert_ne!(req.summary_hash, req.payload_hash);
    }
}

/// Strip `<thinking>...</thinking>` blocks, code fences, and LLM system fluff
/// from a string, returning a clean human-readable sentence.
pub fn clean_consent_summary(raw: &str) -> String {
    // Remove <thinking>...</thinking> blocks (greedy, case-insensitive)
    let re_thinking = regex::Regex::new(r"(?is)<thinking>.*?</thinking>").unwrap_or_else(|_| {
        regex::Regex::new(r".").unwrap() // fallback — should never happen
    });
    let stripped = re_thinking.replace_all(raw, "");
    // Remove triple-backtick code fences
    let re_fence =
        regex::Regex::new(r"(?s)```.*?```").unwrap_or_else(|_| regex::Regex::new(r".").unwrap());
    let stripped = re_fence.replace_all(&stripped, "");
    // Collapse whitespace
    stripped
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

impl ConsentBridge {
    pub fn new(emit: EmitFn) -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            choice_pending: Mutex::new(HashMap::new()),
            emit,
        }
    }

    pub async fn request_consent(&self, action: &str, payload: &str) -> bool {
        let summary = format!("System requests approval for action '{}'.", action.trim());
        let payload_bytes = match serde_json::to_vec(&GenericConsentPayload { action, payload }) {
            Ok(bytes) => bytes,
            Err(err) => {
                eprintln!("[ConsentBridge] failed to serialize generic payload: {err}");
                return false;
            }
        };

        match build_generic_consent_request(action, summary, payload_bytes, "system-session") {
            Ok(request) => self.request_with_prebuilt(request).await.is_approved(),
            Err(err) => {
                eprintln!("[ConsentBridge] failed to build generic request: {err}");
                false
            }
        }
    }

    pub async fn request_action_consent(
        &self,
        intent: &ActionIntent,
        agent_label: &str,
        session_id: &str,
    ) -> MoatResult<bool> {
        let request = build_consent_request(intent, agent_label, session_id)?;
        Ok(self.request_with_prebuilt(request).await.is_approved())
    }

    pub async fn request_rule_review(
        &self,
        session_id: &str,
        review: RuleConflictReview,
    ) -> MoatResult<bool> {
        let summary = format!(
            "The assistant loop proposes a learned rule for worker '{}'. Review and approve before storing it.",
            review.new_rule.target_worker
        );
        let payload = serde_json::to_vec(&review).map_err(MoatError::Serde)?;
        let mut request =
            build_generic_consent_request("rule:review", summary, payload, session_id)?;
        request.rule_review = Some(review);
        Ok(self.request_with_prebuilt(request).await.is_approved())
    }

    pub async fn request_sub_spawn_policy(
        &self,
        session_id: &str,
        source_worker: &str,
        target_worker: &str,
        allowed_tool: &str,
    ) -> MoatResult<ConsentDecision> {
        let normalized_tool = allowed_tool.trim();
        let summary = format!(
            "Worker '{}' requests permission to spawn '{}' using tool '{}'.",
            source_worker.trim(),
            target_worker.trim(),
            normalized_tool
        );
        let payload = serde_json::to_vec(&ExecutionPolicy {
            policy_id: String::new(),
            source_worker: source_worker.trim().to_string(),
            target_worker: target_worker.trim().to_string(),
            allowed_tool: normalized_tool.to_string(),
            policy_hash: String::new(),
        })
        .map_err(MoatError::Serde)?;
        let mut request =
            build_generic_consent_request("request_sub_agent", summary, payload, session_id)?;
        request.sub_spawn_policy = Some(SubSpawnPolicyRequest {
            source_worker: source_worker.trim().to_string(),
            target_worker: target_worker.trim().to_string(),
            allowed_tool: normalized_tool.to_string(),
            can_create_permanent_policy: true,
        });
        Ok(self.request_with_prebuilt(request).await)
    }

    pub async fn request_skill_review(
        &self,
        session_id: &str,
        agent_label: &str,
        proposal: &SkillProposal,
    ) -> MoatResult<bool> {
        let summary = format!(
            "Agent '{}' proposes installing system skill '{}' (language: {}). Review the source code before installation.",
            agent_label.trim(),
            proposal.skill_name.trim(),
            proposal.language.trim()
        );
        let review = SkillReviewRequest {
            skill_name: proposal.skill_name.trim().to_string(),
            description: proposal.description.trim().to_string(),
            source_code: proposal.source_code.clone(),
            code_hash: sha256_hex(proposal.source_code.as_bytes()),
            language: proposal.language.trim().to_string(),
            parameter_schema: proposal.parameter_schema.clone(),
        };
        let payload = serde_json::to_vec(&review).map_err(MoatError::Serde)?;
        let mut request =
            build_generic_consent_request("propose_system_skill", summary, payload, session_id)?;
        request.skill_review = Some(review);
        Ok(self.request_with_prebuilt(request).await.is_approved())
    }

    async fn request_with_prebuilt(&self, request: ConsentRequest) -> ConsentDecision {
        let request_id = request.request_id.clone();
        let (tx, rx) = oneshot::channel::<ConsentDecision>();

        match self.pending.lock() {
            Ok(mut map) => {
                map.insert(request_id.clone(), tx);
            }
            Err(poison) => {
                eprintln!("[ConsentBridge] pending map mutex poisoned: {poison}");
                return ConsentDecision::Deny;
            }
        }

        (self.emit)(request);

        match rx.await {
            Ok(decision) => decision,
            Err(_) => {
                eprintln!("[ConsentBridge] receiver dropped for request {request_id} — denying");
                ConsentDecision::Deny
            }
        }
    }

    pub fn resolve(&self, request_id: &str, decision: ConsentDecision) -> Result<(), String> {
        let tx = match self.pending.lock() {
            Ok(mut map) => map.remove(request_id),
            Err(poison) => return Err(format!("[ConsentBridge] mutex poisoned: {poison}")),
        };

        match tx {
            Some(sender) => {
                let _ = sender.send(decision);
                Ok(())
            }
            None => Err(format!(
                "ConsentBridge: unknown or already-resolved request_id '{request_id}'"
            )),
        }
    }

    // ── Choice bridge (user:ask_choice pause/resume) ──

    /// Suspend the calling async task until the user selects a choice option.
    /// Returns the selected option string.
    pub async fn request_user_choice(&self, request_id: &str) -> String {
        let (tx, rx) = oneshot::channel::<String>();

        match self.choice_pending.lock() {
            Ok(mut map) => {
                map.insert(request_id.to_string(), tx);
            }
            Err(poison) => {
                eprintln!("[ConsentBridge] choice_pending mutex poisoned: {poison}");
                return String::new();
            }
        }

        match rx.await {
            Ok(selection) => selection,
            Err(_) => {
                eprintln!(
                    "[ConsentBridge] choice receiver dropped for request {request_id} — returning empty"
                );
                String::new()
            }
        }
    }

    /// Resolve a pending user:ask_choice request with the user's selected option.
    pub fn resolve_choice(&self, request_id: &str, selection: String) -> Result<(), String> {
        let tx = match self.choice_pending.lock() {
            Ok(mut map) => map.remove(request_id),
            Err(poison) => return Err(format!("[ConsentBridge] choice mutex poisoned: {poison}")),
        };

        match tx {
            Some(sender) => {
                let _ = sender.send(selection);
                Ok(())
            }
            None => Err(format!(
                "ConsentBridge: unknown or already-resolved choice request_id '{request_id}'"
            )),
        }
    }
}

pub fn build_human_summary(intent: &ActionIntent, agent_label: &str) -> String {
    match TypedIntent::from_action_intent(intent) {
        Ok(typed) => match typed.payload {
            IntentPayload::FileWrite { path, contents } => format!(
                "Agent '{}' requests to write {} bytes to path: {}",
                agent_label,
                contents.len(),
                path.as_path().display()
            ),
            IntentPayload::FileRead { path } => format!(
                "Agent '{}' requests to read: {}",
                agent_label,
                path.as_path().display()
            ),
            IntentPayload::ProcessSpawn { executable, args } => format!(
                "Agent '{}' requests OS shell execution: `{} {}`",
                agent_label,
                executable,
                args.join(" ")
            ),
            IntentPayload::OpenUrl { url } => format!(
                "Agent '{}' requests to open URL: {}",
                agent_label,
                url.as_str()
            ),
            IntentPayload::AppleScript { .. } => format!(
                "Agent '{}' requests invocation of tool: 'os:applescript'",
                agent_label
            ),
            IntentPayload::InjectText {
                text,
                expected_app_name,
            } => format!(
                "Agent '{}' requests to inject {} bytes into the active application '{}'",
                agent_label,
                text.len(),
                expected_app_name
            ),
            IntentPayload::Curl { url, method, .. } => format!(
                "Agent '{}' requests network call: {} {}",
                agent_label,
                method,
                url.as_str()
            ),
            IntentPayload::ProposeSkill {
                skill_name,
                language,
                ..
            } => format!(
                "Agent '{}' proposes installing synthesized skill '{}' ({})",
                agent_label, skill_name, language
            ),
            _ => format!(
                "Agent '{}' requests invocation of tool: '{}'",
                agent_label, intent.action
            ),
        },
        Err(_) => format!(
            "Agent '{}' requests invocation of tool: '{}'",
            agent_label, intent.action
        ),
    }
}

pub fn build_execution_payload(intent: &ActionIntent) -> MoatResult<Vec<u8>> {
    serde_json::to_vec(intent).map_err(MoatError::Serde)
}

pub fn build_consent_request(
    intent: &ActionIntent,
    agent_label: &str,
    session_id: &str,
) -> MoatResult<ConsentRequest> {
    let summary = build_human_summary(intent, agent_label);
    let payload_bytes = build_execution_payload(intent)?;
    build_generic_consent_request(&intent.action, summary, payload_bytes, session_id)
}

pub fn build_generic_consent_request(
    action: &str,
    summary: String,
    payload_bytes: Vec<u8>,
    session_id: &str,
) -> MoatResult<ConsentRequest> {
    let summary_hash = sha256_hex(summary.as_bytes());
    let payload_hash = sha256_hex(&payload_bytes);

    if summary_hash == payload_hash {
        return Err(MoatError::InvariantViolation(
            "consent::build_consent_request: summary_hash == payload_hash".into(),
        ));
    }

    Ok(ConsentRequest {
        request_id: Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        action: action.to_string(),
        summary,
        payload: BASE64_STANDARD.encode(payload_bytes),
        summary_hash,
        payload_hash,
        rule_review: None,
        sub_spawn_policy: None,
        skill_review: None,
    })
}
