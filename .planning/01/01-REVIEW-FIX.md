---
phase: 01
fixed_at: 2026-05-14T00:00:00Z
review_path: .planning/01/01-REVIEW.md
iteration: 1
findings_in_scope: 15
fixed: 15
skipped: 0
status: all_fixed
---

# Phase 01: Code Review Fix Report

**Fixed at:** 2026-05-14T00:00:00Z
**Source review:** .planning/01/01-REVIEW.md
**Iteration:** 1

**Summary:**
- Findings in scope: 15 (5 Critical + 10 Warning)
- Fixed: 15
- Skipped: 0

## Fixed Issues

### CR-01: MetaBrain Flush Loop Silently Swallows Errors

**Files modified:** `moat_core/src/meta_brain.rs`
**Commit:** 9f41da9
**Applied fix:** Replaced `let _ = Self::flush_arc(...)` with `if let Err(e)` pattern that logs the error via `eprintln!` and re-sets the dirty flag for retry, preventing silent data loss from serialization or disk errors.

### CR-02: TrustStore Defined But Never Instantiated

**Files modified:** `moat_core/src/lib.rs`
**Commit:** d704fcc
**Applied fix:** Added `trust_store: Arc<TrustStore>` field to the `System` struct and instantiated it in `System::init()` with `register_builtin_worker("agent1")` and `register_builtin_worker("agent2")` calls. The `TrustStore` import was added to the use block.

### CR-03: Sensitive File Protection Only Guards Deletes

**Files modified:** `moat_core/src/tools/executor.rs`
**Commit:** c59698c
**Applied fix:** Extended `is_path_protected` checks to the `fs_read` and `fs_write` arms of `ToolExecutor::execute()`, preventing read/write access to `.key`, `.pem`, `.db`, `.sqlite`, `.log` files and `.git/` directories.

### CR-04: JSON Parsing Errors Silently Swallowed

**Files modified:** `moat_core/src/agent.rs`, `moat_core/src/tools/executor.rs`
**Commit:** c217bad
**Applied fix:** Replaced `serde_json::from_str(x).unwrap_or(serde_json::Value::Null)` with `match` blocks that log parse errors via `eprintln!` before falling back to `Value::Null`, in both `agent.rs:297` (delegate args) and `executor.rs:55` (tool args).

### CR-05: complete_simple Panics on Empty Choices

**Files modified:** `moat_core/src/llm/client.rs`
**Commit:** d541e65
**Applied fix:** Replaced `parsed.choices[0]` index access with `parsed.choices.first()...ok_or_else(|| anyhow::anyhow!("LLM router returned empty choices"))` to prevent index-out-of-bounds panic on empty choices arrays.

### WR-01: Agent1 / Agent2 Near-Duplicate Code

**Files modified:** `moat_core/src/agent.rs`, `moat_core/src/lib.rs`, `moat_core/src/llm/mod.rs`
**Commit:** 5827921
**Applied fix:** Consolidated `Agent1` and `Agent2` structs (previously ~350 lines of near-identical code) into a single `Agent` struct parametrized by an `AgentId` enum field. `Agent1` and `Agent2` are preserved as backward-compatible type aliases. Agent-specific behavior (tool selection, delegation support, capability defaults, prompt loading, log labels) is dispatched via `self.id` match arms. File reduced from 836 to 594 lines (~29% reduction). Updated `lib.rs` to use `Agent::new_agent1()` / `Agent::new_agent2()` constructors. Added `ToolDefinition` re-export in `llm/mod.rs`.

### WR-02: ContextStore::prepend_system_addendum Appends Indefinitely

**Files modified:** `moat_core/src/context/context_store.rs`
**Commit:** 1a5b0d8
**Applied fix:** Added `MAX_SYSTEM_PROMPT_SIZE` constant (8000 chars) to `ContextStore`. When the primary system message exceeds this size, `prepend_system_addendum` inserts a separate system-addendum message at position 1 instead of appending to the first message, preventing unbounded system prompt growth.

### WR-03: CheckpointStore::lock().unwrap() Poison Hazard

**Files modified:** `moat_core/src/context/context_store.rs`
**Commit:** 89032a2
**Applied fix:** Replaced all 5 instances of `self.messages.lock().unwrap()` with `self.messages.lock().unwrap_or_else(|e| e.into_inner())` across `append`, `get_all`, `replace_all`, `count`, and `prepend_system_addendum`. This recovers from poisoned mutexes in long-running daemon processes.

### WR-04: Inconsistent Locking Strategy

**Files modified:** `moat_core/src/lib.rs`, `moat_core/src/swarm.rs`
**Commit:** 900d747
**Applied fix:** Added documentation comments explaining that the agent path uses `Arc<Mutex<MetaBrain>>` (std sync Mutex) while the swarm path uses `Arc<AsyncRwLock<MetaBrain>>` (tokio async RwLock), and that these are intentionally separate MetaBrain instances that do not share permission state.

### WR-05: ProfileExtractor::extract_and_store Can Fail Entire Query

**Files modified:** `moat_core/src/agent.rs`
**Commit:** a6b2782
**Applied fix:** Changed `ProfileExtractor::extract_and_store(...)?;` to `if let Err(e) = ProfileExtractor::extract_and_store(...) { eprintln!(...); }` in both `process_query` methods. Profile extraction failures are now logged but non-blocking for the main user interaction.

### WR-06: resolve_path Canonicalizes Paths — Symlink Attacks

**Files modified:** `moat_core/src/tools/executor.rs`
**Commit:** b044dda
**Applied fix:** Added a check in `resolve_path` that logs a warning via `eprintln!` when the canonicalized path differs from the raw path, detecting symlink-based path traversal attempts.

### WR-07: Agent Persona File Ambiguity

**Files modified:** `moat_core/src/session/config.rs`
**Commit:** e810288
**Applied fix:** Added a warning in `load_prompt` that logs via `eprintln!` when both `agents/{name}.md` and `moat_core/agents/{name}.md` exist, alerting operators that the richer crate copy is being silently shadowed by the root copy.

### WR-08: agent1_tools() Includes skill_execute in Extended Details

**Files modified:** `moat_core/src/llm/tool_definitions.rs`, `moat_core/src/agent.rs`
**Commit:** 66cd0b0
**Applied fix:** Parameterized `tool_schema_details()` to accept an `AgentId` argument, filtering tools by agent type. Agent1's `maybe_inject_tool_details` now passes `AgentId::Agent1` (excluding skill_execute), while Agent2 passes `AgentId::Agent2`. Updated test to match new signature.

### WR-09: Router::make_delegation_decision Always Returns Agent2 Target

**Files modified:** `moat_core/src/router/mod.rs`
**Commit:** 45dbb12
**Applied fix:** Removed the dead `make_delegation_decision` method from `Router`, which was never called and always returned `RouteTarget::Agent2` with `confidence: 1.0`.

### WR-10: metabrain Flush Loop Polls Every 250ms

**Files modified:** `moat_core/src/meta_brain.rs`
**Commit:** 8448079
**Applied fix:** Replaced fixed-interval polling with a `tokio::sync::Notify`-based approach. Added `notify: Arc<Notify>` field to `FlushState`. `mark_dirty()` now calls `notify.notify_one()` to wake the flush loop. The flush loop uses `tokio::select!` with `notify.notified()` for event-driven wakeup and a 250ms fallback timeout. Removed `#[derive(Clone)]` from `FlushState` (no longer needed).

---

_Fixed: 2026-05-14T00:00:00Z_
_Fixer: the agent (gsd-code-fixer)_
_Iteration: 1_
