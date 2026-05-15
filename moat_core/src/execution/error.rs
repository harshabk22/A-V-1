use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("capability denied: agent {agent} attempted {capability} — {reason}")]
    CapabilityDenied {
        agent: String,
        capability: String,
        reason: String,
    },
}
