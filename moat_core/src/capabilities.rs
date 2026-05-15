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
    /// Read channel history (AGX3-only)
    SessionRead,
    /// Reply within channel (AGX3-only)
    SessionReply,
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
            Capability::SessionRead => write!(f, "session:read"),
            Capability::SessionReply => write!(f, "session:reply"),
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

    /// AGX3 default: zero OS capabilities.
    /// AGX3 can only: read channel history, respond in-channel.
    /// Cannot: touch filesystem, spawn processes, launch apps, open URLs.
    pub fn agx3_default() -> Self {
        Self::new(vec![
            Capability::SessionRead,
            Capability::SessionReply,
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agx3_has_zero_os_capabilities() {
        let caps = CapabilitySet::agx3_default();
        // AGX3 must NOT have any OS-touching capabilities
        assert!(!caps.has(&Capability::FileRead), "AGX3 must not have FileRead");
        assert!(!caps.has(&Capability::FileWrite), "AGX3 must not have FileWrite");
        assert!(!caps.has(&Capability::FileDelete), "AGX3 must not have FileDelete");
        assert!(!caps.has(&Capability::OsProcessSpawn), "AGX3 must not have OsProcessSpawn");
        assert!(!caps.has(&Capability::AgentSpawn), "AGX3 must not have AgentSpawn");
        assert!(!caps.has(&Capability::AppOpen), "AGX3 must not have AppOpen");
        assert!(!caps.has(&Capability::UrlOpen), "AGX3 must not have UrlOpen");
        assert!(!caps.has(&Capability::SkillExecute), "AGX3 must not have SkillExecute");
        // AGX3 must have session-only capabilities
        assert!(caps.has(&Capability::SessionRead), "AGX3 must have SessionRead");
        assert!(caps.has(&Capability::SessionReply), "AGX3 must have SessionReply");
    }
}