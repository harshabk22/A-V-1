use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    FileRead,
    FileWrite,
    FileDelete,
    FileExists,
    /// routes to sandbox-exec + Seatbelt profile
    OsProcessSpawn,
    /// routes to wasmtime instance pool, NEVER touches OS
    AgentSpawn,
    AppOpen,
    UrlOpen,
    SkillExecute,
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Capability::FileRead => write!(f, "fs:read"),
            Capability::FileWrite => write!(f, "fs:write"),
            Capability::FileDelete => write!(f, "fs:delete"),
            Capability::FileExists => write!(f, "fs:exists"),
            Capability::OsProcessSpawn => write!(f, "os:process_spawn"),
            Capability::AgentSpawn => write!(f, "agent:spawn"),
            Capability::AppOpen => write!(f, "app:open"),
            Capability::UrlOpen => write!(f, "url:open"),
            Capability::SkillExecute => write!(f, "skill:execute"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitySet {
    capabilities: Vec<Capability>,
}

impl CapabilitySet {
    pub fn new(capabilities: Vec<Capability>) -> Self {
        Self { capabilities }
    }

    pub fn has(&self, cap: &Capability) -> bool {
        self.capabilities.contains(cap)
    }

    pub fn contains_all(&self, other: &CapabilitySet) -> bool {
        other.capabilities.iter().all(|c| self.capabilities.contains(c))
    }

    pub fn agent1_default() -> Self {
        Self::new(vec![
            Capability::FileRead,
            Capability::FileExists,
            Capability::OsProcessSpawn,
            Capability::AppOpen,
            Capability::UrlOpen,
        ])
    }

    pub fn agent2_default() -> Self {
        Self::new(vec![
            Capability::FileRead,
            Capability::FileWrite,
            Capability::FileExists,
            Capability::OsProcessSpawn,
            Capability::AgentSpawn,
            Capability::AppOpen,
            Capability::UrlOpen,
            Capability::SkillExecute,
        ])
    }
}