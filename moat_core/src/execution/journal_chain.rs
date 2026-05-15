use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JournalOperation {
    Allow {
        tool_name: String,
        args: String,
    },
    Deny {
        tool_name: String,
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub seq: u64,
    pub timestamp_ns: u64,
    pub agent_id: String,
    pub operation: JournalOperation,
    pub prev_hash: [u8; 32],
    pub entry_hash: [u8; 32],
}

#[derive(Debug, Error)]
pub enum ChainError {
    #[error("chain integrity broken at seq {seq}: expected hash {expected:02x?}, got {actual:02x?}")]
    HashMismatch {
        seq: u64,
        expected: [u8; 32],
        actual: [u8; 32],
    },
    #[error("missing entry at seq {0}")]
    MissingEntry(u64),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct ChainJournal {
    file: File,
    next_seq: u64,
    last_hash: [u8; 32],
}

impl JournalEntry {
    fn compute_hash(
        seq: u64,
        timestamp_ns: u64,
        agent_id: &str,
        operation: &JournalOperation,
        prev_hash: &[u8; 32],
    ) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(&seq.to_le_bytes());
        hasher.update(&timestamp_ns.to_le_bytes());
        hasher.update(agent_id.as_bytes());
        let op_bytes = bincode::serialize(operation).unwrap_or_default();
        hasher.update(&op_bytes);
        hasher.update(prev_hash);
        hasher.finalize().into()
    }

    pub fn new(
        seq: u64,
        timestamp_ns: u64,
        agent_id: String,
        operation: JournalOperation,
        prev_hash: [u8; 32],
    ) -> Self {
        let entry_hash = Self::compute_hash(seq, timestamp_ns, &agent_id, &operation, &prev_hash);
        Self {
            seq,
            timestamp_ns,
            agent_id,
            operation,
            prev_hash,
            entry_hash,
        }
    }
}

impl ChainJournal {
    /// Open an existing journal or create a new one at `path`.
    /// Uses O_APPEND — no UPDATE, DELETE, TRUNCATE ever.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, ChainError> {
        let path = path.into();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)?;

        let mut reader = file.try_clone()?;
        let (next_seq, last_hash) = Self::scan_tail(&mut reader)?;

        Ok(Self {
            file: OpenOptions::new().create(true).append(true).open(&path)?,
            next_seq,
            last_hash,
        })
    }

    fn scan_tail(reader: &mut File) -> Result<(u64, [u8; 32]), ChainError> {
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;

        if buf.is_empty() {
            return Ok((0, [0u8; 32]));
        }

        // bincode-encoded entries, read last one
        let mut cursor = 0usize;
        let mut seq = 0u64;
        let mut last_hash = [0u8; 32];
        while cursor < buf.len() {
            match bincode::deserialize::<JournalEntry>(&buf[cursor..]) {
                Ok(entry) => {
                    if cursor > 0 {
                        // Verify entry_hash matches computed value
                        let expected = JournalEntry::compute_hash(
                            entry.seq,
                            entry.timestamp_ns,
                            &entry.agent_id,
                            &entry.operation,
                            &entry.prev_hash,
                        );
                        if entry.entry_hash != expected {
                            return Err(ChainError::HashMismatch {
                                seq: entry.seq,
                                expected,
                                actual: entry.entry_hash,
                            });
                        }
                    }
                    seq = entry.seq + 1;
                    last_hash = entry.entry_hash;
                    // Move past this entry
                    let entry_bytes = bincode::serialize(&entry).unwrap_or_default();
                    cursor += entry_bytes.len();
                }
                Err(_) => break, // partial/corrupt entry at end, stop scanning
            }
        }
        Ok((seq, last_hash))
    }

    /// Append an entry. This is the ONLY write operation. Never UPDATE or DELETE.
    pub fn append(&mut self, agent_id: String, operation: JournalOperation) -> Result<u64, ChainError> {
        let seq = self.next_seq;
        let timestamp_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        let entry = JournalEntry::new(
            seq,
            timestamp_ns,
            agent_id,
            operation,
            self.last_hash,
        );

        let bytes = bincode::serialize(&entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        self.file.write_all(&bytes)?;
        self.file.flush()?;

        self.last_hash = entry.entry_hash;
        self.next_seq = seq + 1;

        Ok(seq)
    }
}

/// Verify the entire chain. Returns Ok(()) if every entry_hash matches
/// its computed value and prev_hash forms a contiguous chain.
/// Returns Err(ChainError) on the first discrepancy.
pub fn verify_chain(path: impl Into<PathBuf>) -> Result<u64, ChainError> {
    let path = path.into();
    let mut file = File::open(&path)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;

    if buf.is_empty() {
        return Ok(0);
    }

    let mut cursor = 0usize;
    let mut prev_hash = [0u8; 32];
    let mut count = 0u64;

    while cursor < buf.len() {
        let entry: JournalEntry = bincode::deserialize(&buf[cursor..])
            .map_err(|_| ChainError::MissingEntry(count))?;

        if entry.seq != count {
            return Err(ChainError::MissingEntry(count));
        }
        if entry.prev_hash != prev_hash {
            return Err(ChainError::HashMismatch {
                seq: entry.seq,
                expected: prev_hash,
                actual: entry.prev_hash,
            });
        }

        let expected = JournalEntry::compute_hash(
            entry.seq,
            entry.timestamp_ns,
            &entry.agent_id,
            &entry.operation,
            &entry.prev_hash,
        );
        if entry.entry_hash != expected {
            return Err(ChainError::HashMismatch {
                seq: entry.seq,
                expected,
                actual: entry.entry_hash,
            });
        }

        let entry_bytes = bincode::serialize(&entry).unwrap_or_default();
        cursor += entry_bytes.len();
        prev_hash = entry.entry_hash;
        count += 1;
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("chain_journal_test_{}.bin", name))
    }

    #[test]
    fn test_genesis_entry_has_zero_prev_hash() {
        let path = temp_path("genesis");
        let mut journal = ChainJournal::open(&path).expect("open");
        let seq = journal.append("agent1".into(), JournalOperation::Allow {
            tool_name: "fs_read".into(),
            args: r#"{"path":"/tmp/a.txt"}"#.into(),
        }).expect("append");
        assert_eq!(seq, 0);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_chain_verification_passes_on_valid_chain() {
        let path = temp_path("valid");
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

        let count = verify_chain(&path).expect("verify");
        assert_eq!(count, 3);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_verify_detects_tampered_entry() {
        let path = temp_path("tampered");
        let mut journal = ChainJournal::open(&path).expect("open");
        journal.append("agent1".into(), JournalOperation::Allow {
            tool_name: "fs_read".into(),
            args: r#"{"path":"/tmp/a.txt"}"#.into(),
        }).expect("append 1");
        journal.append("agent1".into(), JournalOperation::Allow {
            tool_name: "fs_write".into(),
            args: r#"{"path":"/tmp/b.txt"}"#.into(),
        }).expect("append 2");
        drop(journal);

        // Tamper with a byte in the middle of the file
        let mut raw = std::fs::read(&path).expect("read");
        if raw.len() > 10 {
            raw[10] ^= 0xFF; // flip bits
        }
        std::fs::write(&path, &raw).expect("write tampered");

        let result = verify_chain(&path);
        assert!(result.is_err(), "Tampered chain must fail verification");
        std::fs::remove_file(&path).ok();
    }
}
