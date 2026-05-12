// moat_core/src/lib.rs
pub mod capabilities;
pub mod config;
pub mod consent;
pub mod execution_plan;
pub mod external_api;
pub mod gateway;
pub mod gateway_transport;
pub mod hooks;
pub mod injector;
pub mod manifest;
pub mod mcp;
pub mod memory;
pub mod meta_brain;
pub mod orchestrator;
pub mod parser;
pub mod plugins;
pub mod reflection;
pub mod rule_store;
pub mod sandbox;
pub mod sdk;
pub mod sentinel;
pub mod shortcuts;
pub mod skills;
pub mod swarm;
pub mod system;
pub mod trust_store;
pub mod types;

pub mod voice;
pub mod worker_pool;

// ============================================================
// § SHARED — Error Taxonomy & Result Alias
// ============================================================

#[derive(Debug, thiserror::Error)]
pub enum MoatError {
    #[error("meta-brain graph error: {0}")]
    Graph(String),

    #[error("meta-brain database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("VSA engine error: {0}")]
    Vsa(String),

    #[error("orchestrator error: {0}")]
    Orchestrator(String),

    #[error("sandbox error: {0}")]
    Sandbox(#[from] crate::sandbox::SandboxError),

    #[error("security violation: {0}")]
    SecurityViolation(String),

    #[error("invariant violation: {0}")]
    InvariantViolation(String),

    #[error("invalid state transition: {0}")]
    InvalidStateTransition(String),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("LLM API error: {0}")]
    Llm(String),

    #[error("context injection error: {0}")]
    Injection(String),

    #[error("settings error: {0}")]
    Settings(String),
}

pub type MoatResult<T> = Result<T, MoatError>;
