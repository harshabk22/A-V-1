use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::{mpsc, oneshot};
use wasmtime::*;

use ageex_wire::{AirgapResponse, CapOp};

/// A validated capability request crossing the airgap from a WASM agent.
/// Enqueued by host imports, consumed by the poller which routes through
/// moat_core::DEL and writes the response back via the bundled oneshot.
#[derive(Debug, Clone)]
pub struct AirgapEnvelope {
    pub agent_id: [u8; 32],
    pub seq: u64,
    pub op: CapOp,
    pub request_bytes: Vec<u8>,
}

/// Bundle sent through the mpsc channel: the envelope + a oneshot sender
/// that the poller uses to return the DEL response to the WASM import.
pub type AirgapBundle = (AirgapEnvelope, oneshot::Sender<AirgapResponse>);

/// Host-side mutable state shared across all WASM import calls for one agent instance.
pub struct StoreData {
    pub agent_id: [u8; 32],
    pub seq_counter: AtomicU64,
    pub request_tx: mpsc::UnboundedSender<AirgapBundle>,
    pub yield_flag: AtomicBool,
    pub yield_message: std::sync::Mutex<Vec<u8>>,
    pub violations: std::sync::Mutex<Vec<ViolationRecord>>,
}

#[derive(Debug, Clone)]
pub struct ViolationRecord {
    pub seq: u64,
    pub op: String,
    pub violation: String,
}

/// The compiled and linked WASM reasoning environment.
/// Holds the wasmtime Engine and Linker, which are expensive to create and
/// can be reused across multiple agent instantiations.
pub struct ReasoningEngine {
    engine: Engine,
    linker: Linker<StoreData>,
    tx: mpsc::UnboundedSender<AirgapBundle>,
}

/// A running agent instance: a Store + a WASM Instance with the exports resolved.
pub struct ReasoningInstance {
    store: Store<StoreData>,
    _instance: Instance,
    step_func: TypedFunc<(), ()>,
}

/// Outcome of a single `agent_step()` call.
pub struct StepOutcome {
    pub agent_yielded: bool,
    pub yield_message: Vec<u8>,
    pub total_requests_sent: u64,
    pub fuel_used: u64,
    pub violations: Vec<ViolationRecord>,
}

const DEFAULT_FUEL_BUDGET: u64 = 100_000_000;

impl ReasoningEngine {
    /// Create a new reasoning engine with all host imports registered.
    /// Returns the engine and the receiver end of the airgap channel.
    pub fn new() -> Result<(Self, mpsc::UnboundedReceiver<AirgapBundle>), Error> {
        let mut config = Config::new();

        config.async_support(true);
        config.consume_fuel(true);
        config.cranelift_nan_canonicalization(true);
        config.wasm_multi_memory(true);

        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);

        crate::imports::register_imports(&mut linker)?;

        let (tx, rx) = mpsc::unbounded_channel();

        Ok((Self { engine, linker, tx }, rx))
    }

    /// Instantiate a new agent from WASM bytes. The agent MUST export a
    /// function `agent_step() -> ()`.
    pub async fn instantiate(
        &self,
        wasm_bytes: &[u8],
        agent_id: [u8; 32],
    ) -> Result<ReasoningInstance, Error> {
        let module = Module::from_binary(&self.engine, wasm_bytes)?;

        let store_data = StoreData {
            agent_id,
            seq_counter: AtomicU64::new(0),
            request_tx: self.tx.clone(),
            yield_flag: AtomicBool::new(false),
            yield_message: std::sync::Mutex::new(Vec::new()),
            violations: std::sync::Mutex::new(Vec::new()),
        };

        let mut store = Store::new(&self.engine, store_data);

        let instance = self.linker.instantiate_async(&mut store, &module).await?;

        let step_func = instance
            .get_typed_func::<(), ()>(&mut store, "agent_step")
            .map_err(|e| {
                Error::msg(format!(
                    "WASM agent does not export required function agent_step(): {}",
                    e
                ))
            })?;

        Ok(ReasoningInstance {
            store,
            _instance: instance,
            step_func,
        })
    }
}

impl ReasoningInstance {
    /// Invoke the agent's `agent_step()` export with a fuel budget.
    /// Returns once the agent yields or exhausts its fuel.
    pub async fn call_step(&mut self, fuel: Option<u64>) -> Result<StepOutcome, Error> {
        let fuel_before = self.store.get_fuel()?;

        self.store
            .set_fuel(fuel.unwrap_or(DEFAULT_FUEL_BUDGET))?;

        let result = self.step_func.call_async(&mut self.store, ()).await;

        let fuel_after = self.store.get_fuel()?;
        let fuel_used = fuel_before.saturating_sub(fuel_after);

        match result {
            Ok(()) | Err(_) => {}
        }

        let is_trap = result.is_err();

        let data = self.store.data_mut();
        let agent_yielded = data.yield_flag.load(Ordering::Acquire);
        let total_requests_sent = data.seq_counter.load(Ordering::Relaxed);
        let yield_message = data
            .yield_message
            .lock()
            .map(|m| m.clone())
            .unwrap_or_default();
        let violations: Vec<ViolationRecord> = data
            .violations
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default();
        data.violations.lock().map(|mut v| v.clear());

        if is_trap && !agent_yielded {
            return Err(Error::msg(format!(
                "agent_step trapped without yielding: fuel_used={fuel_used}, err={:?}",
                result.err()
            )));
        }

        Ok(StepOutcome {
            agent_yielded,
            yield_message,
            total_requests_sent,
            fuel_used,
            violations,
        })
    }

    /// Reset the yield flag so the agent can be stepped again.
    pub fn reset(&mut self) {
        let data = self.store.data_mut();
        data.yield_flag.store(false, Ordering::Release);
    }
}
