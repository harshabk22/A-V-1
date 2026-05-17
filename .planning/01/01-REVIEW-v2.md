---
phase: 01-review-v2
reviewed: 2026-05-15T07:30:00Z
depth: deep
files_reviewed: 50
files_reviewed_list:
  - moat_core/src/agent.rs
  - moat_core/src/capabilities.rs
  - moat_core/src/lib.rs
  - moat_core/src/execution/error.rs
  - moat_core/src/execution/mod.rs
  - moat_core/src/execution/spawn.rs
  - moat_core/src/execution/loop_guard.rs
  - moat_core/src/execution/journal_chain.rs
  - moat_core/src/execution/replay.rs
  - moat_core/src/execution/gate1_schema.rs
  - moat_core/src/execution/gate2_policy.rs
  - moat_core/src/execution/gate3_sandbox.rs
  - moat_core/src/execution/journal.rs
  - moat_core/src/tools/executor.rs
  - moat_core/src/consent.rs
  - moat_core/src/meta_brain.rs
  - moat_core/src/orchestrator.rs
  - moat_core/src/session/config.rs
  - moat_core/src/context/compressor.rs
  - moat_core/src/context/context_store.rs
  - moat_core/src/llm/client.rs
  - moat_core/src/llm/mod.rs
  - moat_core/src/llm/tool_definitions.rs
  - moat_core/src/router/mod.rs
  - moat_core/src/security/mod.rs
  - moat_core/src/security/scanners.rs
  - moat_core/src/security/guardrails.rs
  - moat_core/src/security/audit.rs
  - moat_core/src/security/denylist.rs
  - moat_core/src/registry.rs
  - moat_core/src/events.rs
  - moat_core/src/channel/mod.rs
  - moat_core/src/channel/adapters.rs
  - moat_core/src/channel/lifecycle.rs
  - moat_core/src/channel/binding.rs
  - moat_core/src/memory_manager.rs
  - moat_core/src/skill_graph.rs
  - moat_core/src/config.rs
  - moat_core/src/gateway.rs
  - moat_core/src/main.rs
  - moat_core/src/system.rs
  - moat_core/src/swarm.rs
  - moat_core/Cargo.toml
  - agents/agent3.md
  - ageex_ui/src-tauri/Cargo.toml
  - ageex_ui/src-tauri/src/lib.rs
  - ageex_ui/src-tauri/src/overlay.rs
  - ageex_ui/src/components/CommandPalette.tsx
  - PRODUCTION_READINESS_REPORT.md
findings:
  critical: 3
  warning: 8
  info: 6
  total: 17
status: issues_found
---

# Phase 01: Deep Code Review Report

**Reviewed:** 2026-05-15
**Depth:** deep (cross-file import graph, call chain tracing, security model verification)
**Files Reviewed:** 50
**Status:** issues_found

## Summary

Deep cross-file review of the AGEEX system: the `moat_core` zero-trust kernel (42 modules), the `ageex_ui` Tauri application (3 files), AGX3 agent prompt, and the production readiness report. The architecture demonstrates defense-in-depth: content scanners → `strip_instruction_delimiters()` → MetaBrain permission graph → ConsentBridge → DEL 3-gate pipeline (schema/policy/sandbox) → Chain Journal. AGX3 isolation is structurally enforced — zero tool definitions, zero OS capabilities, the `CapabilitySet::agx3_default()` returns only `SessionRead`/`SessionReply`. Channel routing correctly forces external sources through `must_route_through_agx3()`.

**Three BLOCKER issues found:** (1) prompt injection delimiter bypass on session restore — raw persisted messages loaded via direct struct construction bypass `strip_instruction_delimiters()`, (2) `resolve_and_validate` TOCTOU window — the fd is dropped before DEL's Gate3 re-opens without O_NOFOLLOW, (3) Agent1's `CapabilitySet` includes `OsProcessSpawn` despite being read-only — a structural inconsistency that would grant MetaBrain permission if the tool list were ever expanded.

Eight WARNINGS cover string-based denial detection (false positives), silent journal write failures, hardcoded paths across modules, and unsafe public API surface (`execute_via_del`).

---

## Critical Issues

### CR-01: Prompt Injection Delimiter Bypass on Session Restore

**File:** `moat_core/src/agent.rs:210-217`
**Issue:** Messages loaded from the session store are appended directly via struct literal:
```rust
context.append(crate::context::Message {
    role,
    content: msg.content.clone(),       // ← RAW content, no stripping
    timestamp: msg.timestamp as u64,
    tool_call_id: None,
    tool_calls: None,
})?;
```
This bypasses the `Message::user()` and `Message::tool()` constructors, which call `strip_instruction_delimiters()` to replace `<>SYSTEM_START|>`, `<>SYSTEM_END|>`, `<system>`, `</system>` with `[FILTERED]`. When a previous session's tool output contained these patterns (e.g., from reading a file with injected delimiters), the raw content was stored in SQLite and reloaded unprotected on session reopen. The attacker wins on replay even if the initial insertion was filtered.

**Call chain:** `Agent::new_inner` (line 203) → iterates `session_store.load_messages` → direct struct construction → `context.append` — no delimiter stripping anywhere in this path. Compare with live execution: `process_query` (line 251) → `Message::user(query)` which goes through `strip_instruction_delimiters`. The asymmetry creates the bypass.

**Fix:**
```rust
// agent.rs, replace lines 210-217:
for msg in &messages {
    let role = match msg.role.as_str() {
        "user" => crate::context::Role::User,
        "assistant" => crate::context::Role::Assistant,
        "tool" => crate::context::Role::Tool,
        _ => continue,
    };
    // Use constructors that apply strip_instruction_delimiters
    let message = match role {
        crate::context::Role::User => crate::context::Message::user(&msg.content),
        crate::context::Role::Tool => crate::context::Message::tool(&msg.content),
        _ => crate::context::Message {
            role,
            content: msg.content.clone(),
            timestamp: msg.timestamp as u64,
            tool_call_id: None,
            tool_calls: None,
        },
    };
    context.append(message)?;
}
```

---

### CR-02: TOCTOU in `resolve_and_validate` — Fd Dropped Before DEL Uses O_NOFOLLOW

**Files:** `moat_core/src/tools/executor.rs:516-576`, `moat_core/src/execution/gate3_sandbox.rs:35-46`

**Issue:** The `ToolExecutor::resolve_and_validate()` function correctly opens a path with `O_NOFOLLOW` and resolves the canonical path from the fd via `fcntl(F_GETPATH)`. However, it then **drops the fd** (line 555: `drop(file)`) and returns the validated path string. DEL's Gate3 subsequently re-opens the path in `execute_read_file` (gate3_sandbox.rs:40) using `fs::read_to_string(&path)` — which follows symlinks by default (no `O_NOFOLLOW`).

```
resolve_and_validate(): open(O_NOFOLLOW) → F_GETPATH → drop(fd)  // ← window opens
                         ↓
                    [race window]
                         ↓
Gate3::execute_read_file(): fs::read_to_string(resolved_path)     // ← follows symlinks
```

Between the `drop(file)` and the DEL's `fs::read_to_string`, a concurrent rename could replace the validated file with a symlink pointing outside the workspace. The initial `O_NOFOLLOW` protection is wasted because the actual read in Gate3 does NOT use `O_NOFOLLOW`.

This affects `fs_read` and `fs_delete` — the two operations that use `resolve_and_validate`. `fs_write` uses `resolve_path` instead, which has its own canonicalize-check-then-open TOCTOU but no `O_NOFOLLOW` at any stage.

**Fix:** Pass the open fd through to DEL's Gate3 rather than dropping it and re-opening:
```rust
// executor.rs: After resolve_and_validate, keep the fd and pass it to DEL:
// Instead of returning String, return (String, RawFd) that Gate3 receives.
// Gate3 should use fs::File::from_raw_fd(fd) for its read operation,
// avoiding a second open entirely.
```
Or alternatively, have Gate3's `execute_read_file` also use `OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW)` to re-open with symlink protection.

---

### CR-03: Agent1 `CapabilitySet` Includes `OsProcessSpawn` — Read-Only Inconsistency

**File:** `moat_core/src/capabilities.rs:60-67`
**Issue:** `CapabilitySet::agent1_default()` includes `Capability::OsProcessSpawn`:
```rust
pub fn agent1_default() -> Self {
    Self::new(vec![
        Capability::FileRead,
        Capability::FileExists,
        Capability::OsProcessSpawn,    // ← READ-ONLY agent with exec capability
        Capability::AppOpen,
        Capability::UrlOpen,
    ])
}
```
Agent1 is documented as "read-only" (page2 prompt says "You CANNOT write files, execute terminal commands"). The structural defense against process spawning is the tool list — `agent1_tools()` does NOT include `process_spawn`, so the LLM never calls it through the `process_query` → `run_loop` → tool call → `execute` path.

However, the `CapabilitySet` is the source of truth for the DEL pipeline. If `process_spawn` were ever added to Agent1's tool list (or if a `delegate_to_executor` variant were to call it), `ToolExecutor::tool_to_del_capability` would map it to `Capability::OsProcessSpawn`, and since the CapabilitySet already includes it, the capability check would pass. The only remaining defense is MetaBrain, which for Spotlight mode correctly omits EXEC permission.

**Fix:** Remove `Capability::OsProcessSpawn` from `agent1_default()`. Agent1 is read-only and should never have process-spawn capability at the capability-set level:
```rust
pub fn agent1_default() -> Self {
    Self::new(vec![
        Capability::FileRead,
        Capability::FileExists,
        Capability::AppOpen,
        Capability::UrlOpen,
    ])
}
```
This also aligns with the `delegate_to_executor` tool description: "You CANNOT do these things. ... hand the task to Agent2 who CAN execute it."

---

## Warnings

### WR-01: String-Based Denial Detection in `safe_execute_tool`

**File:** `moat_core/src/agent.rs:278`
**Issue:** The agent loop detects denied operations via substring matching:
```rust
if output.contains("DENIED") || output.contains("No permission") {
```
If a legitimate file read returns content containing the word "DENIED" (e.g., a log file, a README about denial-of-service), the agent loop treats it as a denied tool call, increments the failure counter, and injects `[TOOL_DENIED]` wrapping. After 3 consecutive matches, it forces `[LOOP_STOP]`. This causes false-negative loop stops on benign content.

**Fix:** Use a structured signal from the ToolExecutor rather than string matching. Add a dedicated error variant or prefix the denial message with a unique token (e.g., `__MOAT_DENIED__`) that is checked exactly instead of substring matching.

---

### WR-02: ExecutionJournal Silently Swallows Write Errors

**File:** `moat_core/src/execution/journal.rs:100-113`
**Issue:** The `log_entry` method uses `eprintln!()` to report write failures but never propagates errors:
```rust
match OpenOptions::new().create(true).append(true).open(&self.path) {
    Ok(mut file) => {
        if let Err(e) = writeln!(file, "{}", entry) {
            eprintln!("[Journal] Write failed: {}", e);  // swallowed
        }
    }
    Err(e) => {
        eprintln!("[Journal] Open failed: {}", e);  // swallowed
    }
}
```
The audit trail silently loses entries when the disk is full or the journal path is unwritable. No backpressure or alternative logging path exists.

**Fix:** Return `Result` from `log_entry` and let callers decide whether to propagate or log-and-continue. At minimum, write to the audit logger (SQLite) as a fallback.

---

### WR-03: Seatbelt Profile Uses Shared `/tmp` — Information Leak

**File:** `moat_core/src/execution/gate3_sandbox.rs:281`
**Issue:** The seatbelt profile is written to `std::env::temp_dir()` (shared `/tmp`):
```rust
let temp = std::env::temp_dir().join(format!("ageex-{}.sb", uuid::Uuid::new_v4()));
```
On macOS, `/tmp` (actually `/private/tmp`) is world-readable. Between the `fs::write` and `sandbox-exec -f <profile>` invocation, another process on the same machine can read the profile. The profile contains the workspace path and toolchain paths (`.cargo`, `.rustup`), which leak directory structure. The time window is measured in microseconds, but with `fs::write` + `Command::new()` execution, it's observable.

**Fix:** Create a dedicated temp directory inside `~/.ageex/` with 0700 permissions:
```rust
let ageex_tmp = home.join(".ageex").join("tmp");
fs::create_dir_all(&ageex_tmp)?;
let temp = ageex_tmp.join(format!("ageex-{}.sb", uuid::Uuid::new_v4()));
```

---

### WR-04: Blocked Command Detection Uses Substring `.contains()` — False Positives

**File:** `moat_core/src/execution/gate2_policy.rs:67`
**Issue:** The command blocklist is checked with `command.contains(blocked)`:
```rust
let blocked_commands = vec!["rm -rf /", "sudo", "mkfs", "dd if="];
for blocked in &self.policy.blocked_commands {
    if command.contains(blocked) { ... }
}
```
This means `sudo` also blocks `visudo`, `pseudo`, or any command with "sudo" as a substring. `rm -rf /` blocks `echo rm -rf /` (educational logging). `dd if=` blocks `dd if=/dev/zero`.

**Fix:** Use word-boundary or prefix matching:
```rust
let first_token = command.split_whitespace().next().unwrap_or("");
if first_token == blocked { ... }
```

---

### WR-05: `resolve_and_validate` Requires File Existence — Fails on `fs_delete` for Non-Existing Files

**File:** `moat_core/src/tools/executor.rs:516-576`
**Issue:** `resolve_and_validate` opens with `OpenOptions::new().read(true)` — requires the file to exist. `fs_delete` routes through this path (line 147). If the file doesn't exist, the open fails with "open failed" rather than a clean "file not found". The error message is misleading and doesn't distinguish between permission errors and non-existence.

**Fix:** Handle non-existent files explicitly before attempting `resolve_and_validate`. For deletes on non-existing paths, return a clean `[fs:delete] file not found:` message.

---

### WR-06: `execute_via_del` — Unsafe Public API Exposing Permission Bypass

**File:** `moat_core/src/tools/executor.rs:269-272`
**Issue:** `ToolExecutor` exposes a public method that bypasses all MetaBrain permission checks:
```rust
/// Execute an operation through DEL without MetaBrain permission checks.
/// Used by the orchestrator (which does its own MetaBrain checks).
pub fn execute_via_del(&self, del_tool_name: &str, args: serde_json::Value) -> Result<String> {
    self.execute_del(del_tool_name, args)
}
```
Any code with a `ToolExecutor` reference can call this and skip MetaBrain entirely. The orchestrator (WorkerAgent) uses its own private `self.execute_del`, not this method. The public method is unused dead code with a dangerous API surface.

**Fix:** Either make this method `pub(crate)` (restrict to moat_core internal use) or remove it entirely. The orchestrator already has its own private execute_del on WorkerAgent.

---

### WR-07: `GuardrailsEngine::guard_output()` Never Blocks Critical Findings

**File:** `moat_core/src/security/guardrails.rs:62-82`
**Issue:** The output guard always sets `blocked: false`:
```rust
pub fn guard_output(&self, text: &str) -> GuardResult {
    // ... scanning and redaction ...
    let blocked = false; // output is redacted, never blocked
    GuardResult { sanitized, findings, blocked }
}
```
If the LLM outputs a secret (e.g., an API key it learned from previous context), the output is redacted in the sanitized text but the `blocked` flag is false. The caller must explicitly use the sanitized text — but nothing enforces this. The Tauri frontend processes raw results directly.

**Fix:** Set `blocked: true` for Critical-severity findings in output. In the Tauri integration, check `blocked` before displaying results. The current design trusts callers to use `sanitized` — add enforcement.

---

### WR-08: Dual MetaBrain Instances Not Sharing Permission State

**Files:** `moat_core/src/lib.rs:171-175`, `moat_core/src/swarm.rs:113-123`
**Issue:** Two separate MetaBrain instances exist — the agent path uses `Arc<Mutex<MetaBrain>>` (std sync) while the swarm path uses `Arc<AsyncRwLock<MetaBrain>>` (tokio async). The comment at lib.rs:171-175 explicitly states they are "intentionally SEPARATE MetaBrain instances." Permission grants in one path (e.g., consent-approved file writes) are invisible to the other path. Users may be re-prompted for the same permission across paths.

**Fix:** Either unify on a single persistable MetaBrain with async locks throughout, or prominently document this split in user-facing UI and ensure permission replay across both instances on session resume.

---

## Info

### IN-01: Dead Code — `is_connector()` Method Never Called

**File:** `moat_core/src/agent.rs:94-96`
**Issue:** The method `fn is_connector(&self) -> bool { matches!(self.id, AgentId::Agent3) }` is defined but never invoked anywhere in the codebase. Channel routing uses `must_route_through_agx3` in `channel/mod.rs` instead.

**Fix:** Remove dead code or wire it into the channel routing logic.

---

### IN-02: Synthetic `MessageRecord` with `timestamp: 0` in Profile Extraction

**File:** `moat_core/src/agent.rs:254-263`
**Issue:** `process_query` creates a synthetic `MessageRecord` with `timestamp: 0`:
```rust
let recent = vec![MessageRecord {
    id: 0, session_id: ..., role: "user", content: ..., timestamp: 0,
}];
```
A zero timestamp could confuse time-based profile logic or analytics.

**Fix:** Use `std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()` for the timestamp.

---

### IN-03: Hardcoded Workspace Path Appears in 5+ Files

**Files:** `lib.rs:146-147`, `executor.rs:488-492`, `gate2_policy.rs:32-33`, `gate3_sandbox.rs`, others
**Issue:** The path `~/Desktop/ageex-workspace` is hardcoded in multiple locations. The existing `workspace_root()` in `config.rs` returns `~/.moatageex/workspace` — there are **two different workspace locations** in active use.

**Fix:** Consolidate on a single `config::workspace_root()` constant and use it everywhere. Add a `resolve_workspace_path()` function.

---

### IN-04: `safe_execute_tool` Uses `unwrap_or_else` — Masks Infrastructure Errors

**File:** `moat_core/src/agent.rs:451-453`
**Issue:** Errors from `safe_execute_tool` are caught with a fallback string, severing error propagation:
```rust
.unwrap_or_else(|e| format!("[tool_error] {}: {}", tc.function.name, e));
```
Infrastructure errors (network, DB) become indistinguishable from tool errors in the LLM context.

**Fix:** Distinguish between operational errors (→ Ok with message) and infrastructure errors (→ propagate Err).

---

### IN-05: Delimiter Stripping Misses Unicode Homoglyphs

**Files:** `moat_core/src/context/context_store.rs:27-31`, `moat_core/src/security/scanners.rs:157-203`
**Issue:** Both `strip_instruction_delimiters` and `InjectionScanner::scan` use case-sensitive ASCII-only matching. Unicode homoglyph attacks (e.g., Cyrillic `Ѕ` for Latin `S`) bypass both layers. Low risk since LLMs rarely produce such confusables in tool output.

**Fix:** As defense-in-depth, add Unicode NFKC normalization: `text.nfkc().collect::<String>()`.

---

### IN-06: Scattered `eprintln!` Debug Logging — No Structured Logging

**Files:** Multiple (agent.rs, executor.rs, journal.rs, gate1-3, consent.rs, orchestrator.rs, etc.)
**Issue:** Debug output uses raw `eprintln!()` throughout. No log levels, no structured fields, no correlation IDs. Sensitive data (paths, tool names) is printed unfiltered to stderr.

**Fix:** Adopt the `tracing` crate (already in Cargo.toml). Replace `eprintln!` with `tracing::info!`/`warn!`/`error!`. Use spans for correlation (session_id/request_id).

---

_Reviewed: 2026-05-15T07:30:00Z_
_Reviewer: the agent (gsd-code-reviewer)_
_Depth: deep_
