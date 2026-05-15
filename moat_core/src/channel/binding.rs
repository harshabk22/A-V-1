use crate::gateway::SessionSource;
use crate::session::SessionStore;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use anyhow::Result;

/// Maps external channel identifiers to internal AGEEX session IDs.
/// Provides session creation, lookup, and isolation per channel/user.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChannelSessionKey {
    pub source: SessionSource,
    pub channel_id: String,
    pub user_id: String,
}

/// The session binding system maps `(source, channel_id, user_id)`
/// to internal SQLite session IDs. This ensures:
/// - Different channels never share sessions
/// - Different users within a channel get isolated sessions
/// - Sessions persist across adapter restarts
pub struct SessionBinding {
    /// In-memory cache: ChannelSessionKey → session_id
    bindings: Mutex<HashMap<ChannelSessionKey, String>>,
    session_store: Arc<SessionStore>,
}

impl SessionBinding {
    pub fn new(session_store: Arc<SessionStore>) -> Self {
        Self {
            bindings: Mutex::new(HashMap::new()),
            session_store,
        }
    }

    /// Resolve or create a session for a channel message.
    /// Returns the session ID.
    pub fn resolve_session(&self, source: SessionSource, channel_id: &str, user_id: &str) -> Result<String> {
        let key = ChannelSessionKey {
            source,
            channel_id: channel_id.to_string(),
            user_id: user_id.to_string(),
        };

        // Check cache first
        {
            let bindings = self.bindings.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(session_id) = bindings.get(&key) {
                return Ok(session_id.clone());
            }
        }

        // Create new session in SQLite
        let agent_prefix = source.as_str();
        let session_id = self.session_store.create_session(agent_prefix, None)?;

        // Cache it
        let mut bindings = self.bindings.lock().unwrap_or_else(|e| e.into_inner());
        bindings.insert(key, session_id.clone());

        Ok(session_id)
    }

    /// Get the session ID for a channel, if it exists.
    pub fn get_session(&self, source: SessionSource, channel_id: &str, user_id: &str) -> Option<String> {
        let key = ChannelSessionKey {
            source,
            channel_id: channel_id.to_string(),
            user_id: user_id.to_string(),
        };
        let bindings = self.bindings.lock().unwrap_or_else(|e| e.into_inner());
        bindings.get(&key).cloned()
    }

    /// Remove a session binding (does NOT delete the session data).
    pub fn unbind(&self, source: SessionSource, channel_id: &str, user_id: &str) {
        let key = ChannelSessionKey {
            source,
            channel_id: channel_id.to_string(),
            user_id: user_id.to_string(),
        };
        let mut bindings = self.bindings.lock().unwrap_or_else(|e| e.into_inner());
        bindings.remove(&key);
    }

    /// Number of active session bindings in cache.
    pub fn binding_count(&self) -> usize {
        let bindings = self.bindings.lock().unwrap_or_else(|e| e.into_inner());
        bindings.len()
    }

    /// Check if a session is external (not from local UI/CLI).
    pub fn is_external_session(source: &SessionSource) -> bool {
        !matches!(source, SessionSource::DesktopUi | SessionSource::CommandBar | SessionSource::Cli)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_binding_resolves_new_session() {
        let store = SessionStore::open_with_prefix_in_memory("test_binding")
            .expect("open store");
        let store = Arc::new(store);
        let binding = SessionBinding::new(store);

        let sid = binding.resolve_session(
            SessionSource::Telegram,
            "chat_123",
            "user_456",
        ).expect("resolve");

        assert!(!sid.is_empty());
        assert_eq!(binding.binding_count(), 1);
    }

    #[test]
    fn test_binding_reuses_existing_session() {
        let store = SessionStore::open_with_prefix_in_memory("test_binding_reuse")
            .expect("open store");
        let store = Arc::new(store);
        let binding = SessionBinding::new(store);

        let sid1 = binding.resolve_session(
            SessionSource::WhatsApp,
            "chat_abc",
            "user_def",
        ).expect("resolve 1");

        let sid2 = binding.resolve_session(
            SessionSource::WhatsApp,
            "chat_abc",
            "user_def",
        ).expect("resolve 2");

        assert_eq!(sid1, sid2);
        assert_eq!(binding.binding_count(), 1);
    }

    #[test]
    fn test_binding_isolates_different_channels() {
        let store = SessionStore::open_with_prefix_in_memory("test_binding_isolate")
            .expect("open store");
        let store = Arc::new(store);
        let binding = SessionBinding::new(store);

        let sid_tg = binding.resolve_session(
            SessionSource::Telegram,
            "chat_1",
            "user_a",
        ).expect("resolve tg");

        let sid_dc = binding.resolve_session(
            SessionSource::Discord,
            "chat_1",
            "user_a",
        ).expect("resolve dc");

        assert_ne!(sid_tg, sid_dc, "Different channels must have isolated sessions");
        assert_eq!(binding.binding_count(), 2);
    }

    #[test]
    fn test_is_external_session() {
        assert!(SessionBinding::is_external_session(&SessionSource::WhatsApp));
        assert!(SessionBinding::is_external_session(&SessionSource::Discord));
        assert!(SessionBinding::is_external_session(&SessionSource::Telegram));
        assert!(!SessionBinding::is_external_session(&SessionSource::DesktopUi));
        assert!(!SessionBinding::is_external_session(&SessionSource::Cli));
    }
}
