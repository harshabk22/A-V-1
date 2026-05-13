use sha2::{Sha256, Digest};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    raw: String,
}

impl SessionKey {
    pub fn agent1(session_id: &str) -> Self {
        let hash = Self::hash_key(&format!("agent1:{}", session_id));
        Self { raw: format!("a1:{}", hash) }
    }

    pub fn agent2(session_id: &str) -> Self {
        let hash = Self::hash_key(&format!("agent2:{}", session_id));
        Self { raw: format!("a2:{}", hash) }
    }

    fn hash_key(input: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(input.as_bytes());
        format!("{:x}", hasher.finalize())[..16].to_string()
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }
}

impl fmt::Display for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.raw)
    }
}