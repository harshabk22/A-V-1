use std::sync::Mutex;

/// Typed events that flow through the EventBus.
/// Decouples telemetry, auditing, consent logging, and monitoring
/// from the orchestrator and agent loops.
#[derive(Debug, Clone)]
pub enum SystemEvent {
    ToolCallStart {
        agent_id: String,
        tool_name: String,
        args: String,
    },
    ToolCallEnd {
        agent_id: String,
        tool_name: String,
        success: bool,
        duration_ms: u64,
    },
    ConsentRequested {
        agent_id: String,
        operation: String,
        summary: String,
    },
    ConsentDecision {
        agent_id: String,
        operation: String,
        approved: bool,
        permanent: bool,
    },
    SecurityViolation {
        agent_id: String,
        violation_type: String,
        description: String,
    },
    InjectionDetected {
        agent_id: String,
        source: String,
        pattern: String,
    },
    SecretDetected {
        agent_id: String,
        context: String,
    },
    AgentStep {
        agent_id: String,
        iteration: u32,
        tool_calls: u32,
    },
    AgentDelegated {
        from_agent: String,
        to_agent: String,
        task: String,
    },
    MemoryCompact {
        session_id: String,
        tokens_before: u64,
        tokens_after: u64,
    },
    SessionCreated {
        session_id: String,
        agent_type: String,
    },
    CapabilityDenied {
        agent_id: String,
        capability: String,
        reason: String,
    },
}

pub type EventHandler = Box<dyn Fn(&SystemEvent) + Send + Sync + 'static>;

/// Thread-safe publish/subscribe event bus.
/// Subscribers receive ALL events. Filtering is the subscriber's responsibility.
pub struct EventBus {
    subscribers: Mutex<Vec<EventHandler>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            subscribers: Mutex::new(Vec::new()),
        }
    }

    /// Subscribe to all system events. Returns a subscription token.
    pub fn subscribe(&self, handler: EventHandler) {
        let mut subs = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
        subs.push(handler);
    }

    /// Publish an event to all subscribers.
    /// If a subscriber panics, it is removed to prevent cascading failures.
    pub fn publish(&self, event: &SystemEvent) {
        let mut subs = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
        subs.retain(|handler| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                handler(event);
            })).is_ok()
        });
    }

    /// Number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        let subs = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
        subs.len()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_event_bus_publish_to_subscribers() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&counter);

        bus.subscribe(Box::new(move |_event| {
            c.fetch_add(1, Ordering::SeqCst);
        }));

        bus.publish(&SystemEvent::ToolCallStart {
            agent_id: "agent1".into(),
            tool_name: "fs_read".into(),
            args: "{}".into(),
        });
        bus.publish(&SystemEvent::ToolCallEnd {
            agent_id: "agent1".into(),
            tool_name: "fs_read".into(),
            success: true,
            duration_ms: 5,
        });

        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_event_bus_multiple_subscribers() {
        let bus = EventBus::new();
        let counter1 = Arc::new(AtomicUsize::new(0));
        let counter2 = Arc::new(AtomicUsize::new(0));
        let c1 = Arc::clone(&counter1);
        let c2 = Arc::clone(&counter2);

        bus.subscribe(Box::new(move |_| { c1.fetch_add(1, Ordering::SeqCst); }));
        bus.subscribe(Box::new(move |_| { c2.fetch_add(1, Ordering::SeqCst); }));

        bus.publish(&SystemEvent::AgentStep {
            agent_id: "agent2".into(),
            iteration: 1,
            tool_calls: 0,
        });

        assert_eq!(counter1.load(Ordering::SeqCst), 1);
        assert_eq!(counter2.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_event_bus_panicking_subscriber_is_removed() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&counter);

        bus.subscribe(Box::new(move |_| {
            panic!("simulated subscriber crash");
        }));
        bus.subscribe(Box::new(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
        }));

        assert_eq!(bus.subscriber_count(), 2);
        bus.publish(&SystemEvent::SessionCreated {
            session_id: "s1".into(),
            agent_type: "agent2".into(),
        });

        // Panicking subscriber should be removed
        assert_eq!(bus.subscriber_count(), 1);
        // Second subscriber should still get the event
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_event_bus_consent_event_flow() {
        let bus = EventBus::new();
        let approved = Arc::new(AtomicUsize::new(0));
        let denied = Arc::new(AtomicUsize::new(0));
        let app = Arc::clone(&approved);
        let den = Arc::clone(&denied);

        bus.subscribe(Box::new(move |event| {
            if let SystemEvent::ConsentDecision { approved, .. } = event {
                if *approved { app.fetch_add(1, Ordering::SeqCst); }
                else { den.fetch_add(1, Ordering::SeqCst); }
            }
        }));

        bus.publish(&SystemEvent::ConsentDecision {
            agent_id: "agent2".into(),
            operation: "fs_delete".into(),
            approved: true,
            permanent: false,
        });
        bus.publish(&SystemEvent::ConsentDecision {
            agent_id: "agent2".into(),
            operation: "process_spawn".into(),
            approved: false,
            permanent: false,
        });

        assert_eq!(approved.load(Ordering::SeqCst), 1);
        assert_eq!(denied.load(Ordering::SeqCst), 1);
    }
}
