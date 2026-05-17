# WASM → DEL Integration Design

## Goal
Wire `ageex_reasoning` (wasmtime host) into `moat_core` (DEL pipeline) so
WASM agent capability requests flow through Gate1→Gate2→Gate3→Journal
instead of dead-ending into an unread mpsc channel.

## New Types (ageex_wire)

```rust
pub struct AirgapRequest {
    pub agent_id: [u8; 32],
    pub seq: u64,
    pub op: CapOp,
    pub payload: Vec<u8>,
}

pub struct AirgapResponse {
    pub agent_id: [u8; 32],
    pub seq: u64,
    pub op: CapOp,
    pub result: AirgapResult,
}

pub enum AirgapResult {
    Success(Vec<u8>),
    Error(AirgapError),
}
```

## Architecture

```
WASM Agent
  │ __ageex_airgap_send(FileRead, req_ptr, req_len, resp_ptr, resp_cap)
  │
  ├─ read request bytes from WASM linear memory
  ├─ validate (path traversal, size, NUL) via existing semantic_validate
  ├─ create oneshot channel
  ├─ send AirgapEnvelope { agent_id, seq, op, request_bytes, response_tx }
  ├─ await oneshot → AirgapResponse
  ├─ serialize response to bincode
  ├─ write to WASM memory at resp_ptr
  └─ return AERR_NONE

POLLER (tauri async task)
  ├─ recv AirgapEnvelope
  ├─ build AirgapRequest { agent_id, seq, op, payload }
  ├─ call moat_core::airgap::handle_airgap_request(req)
  │     ├─ decode wire bytes → path/args using ageex_wire::codec
  │     ├─ build ExecutionRequest { tool_name, capability, args }
  │     ├─ call DelExecutor::execute(req)   ← existing DEL core loop
  │     │     ├─ Gate1Schema::validate
  │     │     ├─ Gate2Policy::enforce
  │     │     ├─ Gate3Sandbox::execute
  │     │     └─ ExecutionJournal::log_result
  │     └─ map ExecutionResult → AirgapResult
  │           success → AirgapResult::Success(output_bytes)
  │           denied  → AirgapResult::Success(serialized ExecutionResult)
  │           error   → AirgapResult::Error(AirgapError::HostError)
  ├─ send AirgapResponse on envelope.response_tx
  └─ loop
```

## Key Design Decisions

1. **Oneshot instead of DashMap polling** — Each envelope carries its own
   response channel. No polling loops, no shared mutable map, no cleanup.
2. **Denials are Success, not Error** — `ExecutionResult` with
   `success=false` is serialized into `AirgapResult::Success(denial_bytes)`
   so the WASM agent can inspect `denied_reason`, `gate1_passed`, etc.
   `AirgapResult::Error` is only for host failures (bad opcode, malformed
   payload, channel closed).
3. **No new DEL types** — Uses existing `ExecutionRequest` struct
   (string tool_name + serde_json::Value args) and existing
   `DelExecutor::execute()` core loop. Zero new Gate1/Gate2/Gate3 code.
4. **bincode serialization** — Both request validation and response
   encoding already use bincode. Adding `AirgapResponse` as a bincode
   type maintains consistency.

## Changed Files

| File | Change |
|------|--------|
| `crates/ageex_wire/src/lib.rs` | Add AirgapRequest, AirgapResponse, AirgapResult, serde derives |
| `crates/ageex_reasoning/src/engine.rs` | AirgapEnvelope gains response_tx: oneshot::Sender |
| `crates/ageex_reasoning/src/imports.rs` | process_airgap_send → async, awaits oneshot, writes to WASM mem |
| `moat_core/Cargo.toml` | Add ageex_wire dependency |
| `moat_core/src/airgap.rs` | New file: AirgapHandler + handle_airgap_request |
| `moat_core/src/lib.rs` | Export airgap module |
| `ageex_ui/src-tauri/Cargo.toml` | moat_core already depends; add oneshot if needed |
| `ageex_ui/src-tauri/src/reasoning_bridge.rs` | start_envelope_poller calls moat_core::airgap |
| `ageex_ui/src-tauri/src/lib.rs` | Call start_envelope_poller after engine init |
| `wasm/agents/sdk/src/lib.rs` | Add AirgapResponse deserialization, oneshot import support |

## New Files

| File | Purpose |
|------|---------|
| `wasm/agents/example_agent/src/lib.rs` | Minimal agent: reads file via airgap, yields result |
| `wasm/agents/example_agent/Cargo.toml` | Builds to wasm32-wasip1, depends on SDK |

## Tests

1. `airgap.rs` — AirgapHandler maps CapOp→ExecutionRequest→DelExecutor correctly
2. `imports.rs` — oneshot wiring in process_airgap_send round-trips
3. Example WASM agent — compile + verify agent_step export exists
4. Integration — mock DelExecutor, verify WASM→envelope→handler→response loop
