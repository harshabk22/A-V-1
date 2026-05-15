use super::journal_chain::{verify_chain, ChainError, JournalEntry, JournalOperation};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum ReplayEvent {
    OperationExecuted {
        seq: u64,
        agent_id: String,
        tool_name: String,
        args: String,
    },
    DenialReplayed {
        seq: u64,
        agent_id: String,
        tool_name: String,
        reason: String,
    },
    ReplayComplete {
        entries_processed: u64,
    },
}

pub struct ReplayEngine {
    journal_path: PathBuf,
}

impl ReplayEngine {
    pub fn new(journal_path: impl Into<PathBuf>) -> Self {
        Self {
            journal_path: journal_path.into(),
        }
    }

    /// Verify the chain journal integrity, then replay all entries
    /// from `from_seq` forward. Returns an iterator of ReplayEvents.
    ///
    /// The chain journal (SHA-256 chained, all operations) remains
    /// completely separate from session SQLite (user+assistant+tool
    /// messages, for conversation continuity).
    pub fn replay_from(&self, from_seq: u64) -> Result<Vec<ReplayEvent>, ChainError> {
        let total = verify_chain(&self.journal_path)?;
        if from_seq >= total {
            return Ok(vec![ReplayEvent::ReplayComplete {
                entries_processed: 0,
            }]);
        }

        let mut events = Vec::new();
        let mut file = std::fs::File::open(&self.journal_path)?;
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut buf)?;

        let mut cursor = 0usize;
        while cursor < buf.len() {
            let entry: JournalEntry = if let Ok(e) = bincode::deserialize(&buf[cursor..]) {
                e
            } else {
                break;
            };

            if entry.seq >= from_seq {
                match &entry.operation {
                    JournalOperation::Allow { tool_name, args } => {
                        events.push(ReplayEvent::OperationExecuted {
                            seq: entry.seq,
                            agent_id: entry.agent_id.clone(),
                            tool_name: tool_name.clone(),
                            args: args.clone(),
                        });
                    }
                    JournalOperation::Deny { tool_name, reason } => {
                        events.push(ReplayEvent::DenialReplayed {
                            seq: entry.seq,
                            agent_id: entry.agent_id.clone(),
                            tool_name: tool_name.clone(),
                            reason: reason.clone(),
                        });
                    }
                }
            }

            let entry_bytes = bincode::serialize(&entry).unwrap_or_default();
            cursor += entry_bytes.len();
        }

        events.push(ReplayEvent::ReplayComplete {
            entries_processed: events.len() as u64,
        });

        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::journal_chain::ChainJournal;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("replay_test_{}.bin", name))
    }

    #[test]
    fn test_replay_replays_all_entries() {
        let path = temp_path("replay_all");
        let mut journal = ChainJournal::open(&path).expect("open");
        journal.append("agent1".into(), JournalOperation::Allow {
            tool_name: "fs_read".into(),
            args: r#"{"path":"/tmp/a.txt"}"#.into(),
        }).expect("append 1");
        journal.append("agent1".into(), JournalOperation::Deny {
            tool_name: "fs_delete".into(),
            reason: "consent denied".into(),
        }).expect("append 2");
        journal.append("agent2".into(), JournalOperation::Allow {
            tool_name: "process_spawn".into(),
            args: r#"{"command":"ls"}"#.into(),
        }).expect("append 3");
        drop(journal);

        let engine = ReplayEngine::new(&path);
        let events = engine.replay_from(0).expect("replay");

        assert_eq!(events.len(), 4); // 3 ops + 1 ReplayComplete
        assert!(matches!(events[0], ReplayEvent::OperationExecuted { seq: 0, .. }));
        assert!(matches!(events[1], ReplayEvent::DenialReplayed { seq: 1, .. }));
        assert!(matches!(events[2], ReplayEvent::OperationExecuted { seq: 2, .. }));
        assert!(matches!(events[3], ReplayEvent::ReplayComplete { .. }));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_replay_from_mid_sequence() {
        let path = temp_path("replay_mid");
        let mut journal = ChainJournal::open(&path).expect("open");
        journal.append("agent1".into(), JournalOperation::Allow {
            tool_name: "fs_read".into(),
            args: r#"{}"#.into(),
        }).expect("append 1");
        journal.append("agent1".into(), JournalOperation::Allow {
            tool_name: "fs_write".into(),
            args: r#"{}"#.into(),
        }).expect("append 2");
        journal.append("agent2".into(), JournalOperation::Deny {
            tool_name: "process_spawn".into(),
            reason: "blocked".into(),
        }).expect("append 3");
        drop(journal);

        let engine = ReplayEngine::new(&path);
        let events = engine.replay_from(1).expect("replay"); // skip seq 0

        // Should get seq 1, 2, + ReplayComplete
        assert_eq!(events.len(), 3);
        match &events[0] {
            ReplayEvent::OperationExecuted { seq, .. } => assert_eq!(*seq, 1),
            _ => panic!("Expected OperationExecuted at seq 1"),
        }
        match &events[1] {
            ReplayEvent::DenialReplayed { seq, .. } => assert_eq!(*seq, 2),
            _ => panic!("Expected DenialReplayed at seq 2"),
        }

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_replay_empty_journal() {
        let path = temp_path("replay_empty");
        ChainJournal::open(&path).expect("open"); // just create, don't append
        let engine = ReplayEngine::new(&path);
        let events = engine.replay_from(0).expect("replay");
        assert_eq!(events.len(), 1); // just ReplayComplete
        assert!(matches!(events[0], ReplayEvent::ReplayComplete { entries_processed: 0 }));
        std::fs::remove_file(&path).ok();
    }
}
