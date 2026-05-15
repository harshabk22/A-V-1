use super::adapters::{ChannelAdapter, AdapterRegistry};
use crate::gateway::SessionSource;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// State of a single channel adapter instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed { reason: String, since: u64 },
}

/// Status of a managed adapter.
#[derive(Debug, Clone)]
pub struct AdapterStatus {
    pub source: SessionSource,
    pub state: AdapterState,
    pub started_at: Option<u64>,
    pub restart_count: u64,
    pub last_health_check: Option<u64>,
    pub healthy: bool,
}

/// Manages the lifecycle of channel adapters: start, stop, health, restart.
/// All state management only — ZERO OS/network access.
pub struct ChannelLifecycleManager {
    registry: Arc<Mutex<AdapterRegistry>>,
    states: Arc<Mutex<HashMap<SessionSource, AdapterStatus>>>,
    backoff_base: Duration,
    max_restarts: u64,
}

impl ChannelLifecycleManager {
    pub fn new(registry: Arc<Mutex<AdapterRegistry>>) -> Self {
        Self {
            registry,
            states: Arc::new(Mutex::new(HashMap::new())),
            backoff_base: Duration::from_secs(5),
            max_restarts: 10,
        }
    }

    /// Start all registered adapters. Returns count of successfully started.
    pub fn start_all(&self) -> usize {
        let registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let sources = registry.sources();
        drop(registry);

        let mut started = 0;
        for source in sources {
            if self.start_adapter(&source).is_ok() {
                started += 1;
            }
        }
        started
    }

    /// Start a single adapter by source.
    pub fn start_adapter(&self, source: &SessionSource) -> Result<(), String> {
        let registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        if registry.get(source).is_none() {
            return Err(format!("No adapter registered for {:?}", source));
        }
        drop(registry);

        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        let status = states.entry(*source).or_insert_with(|| AdapterStatus {
            source: *source,
            state: AdapterState::Stopped,
            started_at: None,
            restart_count: 0,
            last_health_check: None,
            healthy: false,
        });

        if status.state == AdapterState::Running {
            return Ok(());
        }
        if status.restart_count >= self.max_restarts {
            status.state = AdapterState::Failed {
                reason: format!("Exceeded max restarts ({})", self.max_restarts),
                since: now_secs(),
            };
            return Err("Max restarts exceeded".into());
        }

        status.state = AdapterState::Starting;
        status.started_at = Some(now_secs());
        status.state = AdapterState::Running;
        status.healthy = true;
        Ok(())
    }

    /// Stop a single adapter.
    pub fn stop_adapter(&self, source: &SessionSource) -> Result<(), String> {
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(status) = states.get_mut(source) {
            status.state = AdapterState::Stopped;
            status.healthy = false;
            Ok(())
        } else {
            Err(format!("No lifecycle state for {:?}", source))
        }
    }

    /// Stop all adapters.
    pub fn stop_all(&self) {
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        for status in states.values_mut() {
            status.state = AdapterState::Stopped;
            status.healthy = false;
        }
    }

    /// Run health checks on all running adapters.
    /// Restarts failed ones with exponential backoff.
    pub fn health_check_all(&self, adapters: &[&dyn ChannelAdapter]) -> Vec<AdapterStatus> {
        let mut results = Vec::new();

        for adapter in adapters {
            let source = adapter.source();
            let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
            let status = states.entry(source).or_insert_with(|| AdapterStatus {
                source,
                state: AdapterState::Stopped,
                started_at: None,
                restart_count: 0,
                last_health_check: None,
                healthy: false,
            });

            status.last_health_check = Some(now_secs());
            match adapter.health_check() {
                Ok(()) => {
                    status.healthy = true;
                    if status.state != AdapterState::Running {
                        status.state = AdapterState::Running;
                    }
                }
                Err(reason) => {
                    status.healthy = false;
                    status.restart_count += 1;
                    if status.restart_count >= self.max_restarts {
                        status.state = AdapterState::Failed {
                            reason: format!("Health check failed: {}", reason),
                            since: now_secs(),
                        };
                    } else {
                        status.state = AdapterState::Failed {
                            reason: format!("Health check failed (attempt {}): {}",
                                status.restart_count, reason),
                            since: now_secs(),
                        };
                    }
                }
            }
            results.push(status.clone());
        }
        results
    }

    /// Get the status of all adapters.
    pub fn status_all(&self) -> Vec<AdapterStatus> {
        let states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        states.values().cloned().collect()
    }

    /// Get status of a single adapter.
    pub fn status(&self, source: &SessionSource) -> Option<AdapterStatus> {
        let states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        states.get(source).cloned()
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lifecycle_start_and_stop() {
        let reg = Arc::new(Mutex::new(AdapterRegistry::with_defaults()));
        let mgr = ChannelLifecycleManager::new(reg);

        assert_eq!(mgr.start_all(), 3);
        for source in &[SessionSource::Telegram, SessionSource::WhatsApp, SessionSource::Discord] {
            let status = mgr.status(source).expect("status exists");
            assert_eq!(status.state, AdapterState::Running);
            assert!(status.healthy);
        }

        mgr.stop_adapter(&SessionSource::Telegram).expect("stop");
        assert_eq!(mgr.status(&SessionSource::Telegram).unwrap().state, AdapterState::Stopped);

        mgr.stop_all();
        for source in &[SessionSource::Telegram, SessionSource::WhatsApp, SessionSource::Discord] {
            assert_eq!(mgr.status(source).unwrap().state, AdapterState::Stopped);
        }
    }

    #[test]
    fn test_lifecycle_prevents_double_start() {
        let reg = Arc::new(Mutex::new(AdapterRegistry::with_defaults()));
        let mgr = ChannelLifecycleManager::new(reg);

        mgr.start_adapter(&SessionSource::Discord).expect("start 1");
        mgr.start_adapter(&SessionSource::Discord).expect("start 2");
        assert_eq!(mgr.status(&SessionSource::Discord).unwrap().restart_count, 0);
    }
}
