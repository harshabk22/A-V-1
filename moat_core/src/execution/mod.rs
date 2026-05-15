pub mod request;
pub mod result;
pub mod gate1_schema;
pub mod gate2_policy;
pub mod gate3_sandbox;
pub mod journal;
pub mod error;
pub mod spawn;
pub mod loop_guard;
pub mod journal_chain;
pub mod replay;

pub use request::ExecutionRequest;
pub use result::ExecutionResult;
pub use error::ExecutionError;
pub use spawn::{AgentPool, SpawnContext, SpawnError, MAX_SPAWN_DEPTH};
pub use loop_guard::{HashDedupDetector, PingPongDetector, GuardDecision};

use gate1_schema::Gate1Schema;
use gate2_policy::Gate2Policy;
use gate3_sandbox::Gate3Sandbox;
use journal::ExecutionJournal;
use anyhow::Result;

pub struct DelExecutor {
    policy: Gate2Policy,
    journal: ExecutionJournal,
}

impl DelExecutor {
    pub fn new() -> Result<Self> {
        let policy = Gate2Policy::default();
        let journal = ExecutionJournal::open()?;
        Ok(Self { policy, journal })
    }

    pub fn execute(&self, req: ExecutionRequest) -> ExecutionResult {
        eprintln!("[DEL] Request {} → {}", req.request_id, req.tool_name);

        if let Err(e) = self.journal.log_request(&req) {
            eprintln!("[DEL] Journal write failed: {}", e);
        }

        let req = match Gate1Schema::validate(req) {
            Ok(r) => r,
            Err(result) => {
                self.journal.log_result(&result).ok();
                return result;
            }
        };

        if let Err(result) = self.policy.enforce(&req) {
            self.journal.log_result(&result).ok();
            return result;
        }

        let result = match Gate3Sandbox::execute(&req) {
            Ok(r) => r,
            Err(r) => r,
        };

        if let Err(e) = self.journal.log_result(&result) {
            eprintln!("[DEL] Journal write failed: {}", e);
        }

        result
    }
}
