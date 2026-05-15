use sha2::{Digest, Sha256};

/// Detects repeated identical tool calls via SHA-256 dedup.
/// If the same (tool_name, args) pair appears more than `max_consecutive`
/// times in a row, returns `LoopDetected`.
pub struct HashDedupDetector {
    max_consecutive: usize,
    last_hash: Option<Vec<u8>>,
    consecutive_count: usize,
}

pub enum GuardDecision {
    Proceed,
    LoopDetected {
        tool_name: String,
        consecutive_count: usize,
    },
}

impl HashDedupDetector {
    pub fn new(max_consecutive: usize) -> Self {
        Self {
            max_consecutive,
            last_hash: None,
            consecutive_count: 0,
        }
    }

    fn hash(tool_name: &str, args: &str) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update(tool_name.as_bytes());
        hasher.update(b":");
        hasher.update(args.as_bytes());
        hasher.finalize().to_vec()
    }

    pub fn check(&mut self, tool_name: &str, args: &str) -> GuardDecision {
        let h = Self::hash(tool_name, args);
        if self.last_hash.as_ref() == Some(&h) {
            self.consecutive_count += 1;
            if self.consecutive_count > self.max_consecutive {
                return GuardDecision::LoopDetected {
                    tool_name: tool_name.into(),
                    consecutive_count: self.consecutive_count,
                };
            }
        } else {
            self.last_hash = Some(h);
            self.consecutive_count = 1;
        }
        GuardDecision::Proceed
    }

    pub fn reset(&mut self) {
        self.last_hash = None;
        self.consecutive_count = 0;
    }
}

/// Detects ping-pong patterns in spawn lineage history.
/// A sliding window of `window_size` agent IDs is maintained.
/// If pattern A→B→A or A→B→C→A is detected, returns LoopDetected.
pub struct PingPongDetector {
    window_size: usize,
    history: Vec<String>,
}

impl PingPongDetector {
    pub fn new(window_size: usize) -> Self {
        Self {
            window_size,
            history: Vec::with_capacity(window_size),
        }
    }

    /// Seed the root agent so that spawn-back-to-root is detected.
    pub fn seed_root(&mut self, root_agent_id: &str) {
        if self.history.is_empty() {
            self.history.push(root_agent_id.to_string());
        }
    }

    /// Push a spawn event and check for ping-pong loops.
    /// If the `spawnee` (the agent being spawned) already exists in the
    /// lineage history, this is a re-spawn loop.
    pub fn check_spawn(&mut self, _spawner: &str, spawnee: &str) -> GuardDecision {
        if self.history.contains(&spawnee.to_string()) {
            return GuardDecision::LoopDetected {
                tool_name: "agent_spawn".into(),
                consecutive_count: self.history.len(),
            };
        }

        if self.history.len() >= self.window_size {
            self.history.remove(0);
        }
        self.history.push(spawnee.to_string());

        GuardDecision::Proceed
    }

    pub fn reset(&mut self) {
        self.history.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_dedup_detects_three_identical() {
        let mut detector = HashDedupDetector::new(3);
        assert!(matches!(detector.check("fs_read", r#"{"path":"/tmp/a.txt"}"#), GuardDecision::Proceed));
        assert!(matches!(detector.check("fs_read", r#"{"path":"/tmp/a.txt"}"#), GuardDecision::Proceed));
        assert!(matches!(detector.check("fs_read", r#"{"path":"/tmp/a.txt"}"#), GuardDecision::Proceed));
        // 4th identical call must trigger
        match detector.check("fs_read", r#"{"path":"/tmp/a.txt"}"#) {
            GuardDecision::LoopDetected { tool_name, consecutive_count } => {
                assert_eq!(tool_name, "fs_read");
                assert_eq!(consecutive_count, 4);
            }
            _ => panic!("Expected LoopDetected on 4th identical call"),
        }
    }

    #[test]
    fn test_hash_dedup_resets_on_different_call() {
        let mut detector = HashDedupDetector::new(3);
        detector.check("fs_read", r#"{"path":"/tmp/a.txt"}"#);
        detector.check("fs_read", r#"{"path":"/tmp/a.txt"}"#);
        // Different args break the streak
        assert!(matches!(detector.check("fs_read", r#"{"path":"/tmp/b.txt"}"#), GuardDecision::Proceed));
        // Streak is now 1 for the new hash
        assert!(matches!(detector.check("fs_read", r#"{"path":"/tmp/b.txt"}"#), GuardDecision::Proceed));
    }

    #[test]
    fn test_ping_pong_detects_a_b_a() {
        let mut detector = PingPongDetector::new(6);
        detector.seed_root("agent_a");
        // A spawns B
        assert!(matches!(detector.check_spawn("agent_a", "agent_b"), GuardDecision::Proceed));
        // B spawns A — ping-pong back to root!
        match detector.check_spawn("agent_b", "agent_a") {
            GuardDecision::LoopDetected { .. } => {}
            _ => panic!("Expected LoopDetected on B→A ping-pong"),
        }
    }

    #[test]
    fn test_ping_pong_detects_a_b_c_a() {
        let mut detector = PingPongDetector::new(6);
        detector.seed_root("agent_a");
        assert!(matches!(detector.check_spawn("agent_a", "agent_b"), GuardDecision::Proceed));
        assert!(matches!(detector.check_spawn("agent_b", "agent_c"), GuardDecision::Proceed));
        // C spawns A — cycle back to a visited agent
        match detector.check_spawn("agent_c", "agent_a") {
            GuardDecision::LoopDetected { .. } => {}
            _ => panic!("Expected LoopDetected on C→A cycle"),
        }
    }

    #[test]
    fn test_ping_pong_allows_linear_chain() {
        let mut detector = PingPongDetector::new(6);
        detector.seed_root("root");
        assert!(matches!(detector.check_spawn("root", "a"), GuardDecision::Proceed));
        assert!(matches!(detector.check_spawn("a", "b"), GuardDecision::Proceed));
        assert!(matches!(detector.check_spawn("b", "c"), GuardDecision::Proceed));
        assert!(matches!(detector.check_spawn("c", "d"), GuardDecision::Proceed));
        // Linear root→A→B→C→D — no cycles
    }
}
