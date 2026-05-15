use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("capability denied: agent {agent} attempted {capability} — {reason}")]
    CapabilityDenied {
        agent: String,
        capability: String,
        reason: String,
    },

    #[error("symlink traversal denied: {0}")]
    SymlinkDenied(PathBuf),
}
