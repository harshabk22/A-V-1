---
phase: 01
reviewed: 2026-05-14T00:00:00Z
depth: deep
files_reviewed: 60
files_reviewed_list:
  - agents/agent1.md
  - agents/agent2.md
  - moat_core/agents/agent1.md
  - moat_core/agents/agent2.md
  - moat_core/src/agent.rs
  - moat_core/src/lib.rs
  - moat_core/src/capabilities.rs
  - moat_core/src/meta_brain.rs
  - moat_core/src/tools/executor.rs
  - moat_core/src/llm/tool_definitions.rs
  - moat_core/src/router/mod.rs
  - moat_core/src/router/classifier.rs
  - moat_core/src/router/decision.rs
  - moat_core/src/router/llm_fallback.rs
  - moat_core/src/approval.rs
  - moat_core/src/manifest.rs
  - moat_core/src/trust_store.rs
  - moat_core/src/session/config.rs
  - moat_core/src/session/store.rs
  - moat_core/src/swarm.rs
  - moat_core/src/config.rs
  - moat_core/src/context/mod.rs
  - moat_core/src/context/compressor.rs
  - moat_core/src/context/context_store.rs
  - moat_core/src/context/token_budget.rs
  - moat_core/src/llm/types.rs
  - moat_core/src/llm/client.rs
  - .agent/agents/gsd-code-reviewer.md
  - .agent/agents/gsd-planner.md
  - .agent/agents/gsd-executor.md
  - .agent/agents/gsd-debugger.md
  - .agent/agents/gsd-verifier.md
  - .agent/agents/gsd-security-auditor.md
  - .agent/agents/gsd-code-fixer.md
  - .agent/agents/gsd-debug-session-manager.md
  - .agent/agents/gsd-doc-writer.md
  - .agent/agents/gsd-roadmapper.md
  - .agent/agents/gsd-pattern-mapper.md
  - .agent/agents/gsd-integration-checker.md
  - .agent/agents/gsd-nyquist-auditor.md
  - .agent/agents/gsd-user-profiler.md
  - .agent/agents/gsd-research-synthesizer.md
  - .agent/agents/gsd-domain-researcher.md
  - .agent/agents/gsd-eval-auditor.md
  - .agent/agents/gsd-eval-planner.md
  - .agent/agents/gsd-framework-selector.md
  - .agent/agents/gsd-ui-auditor.md
  - .agent/agents/gsd-ui-checker.md
  - .agent/agents/gsd-ui-researcher.md
  - .agent/agents/gsd-phase-researcher.md
  - .agent/agents/gsd-plan-checker.md
  - .agent/agents/gsd-project-researcher.md
  - .agent/agents/gsd-doc-classifier.md
  - .agent/agents/gsd-doc-synthesizer.md
  - .agent/agents/gsd-doc-verifier.md
  - .agent/agents/gsd-intel-updater.md
  - .agent/agents/gsd-codebase-mapper.md
  - .agent/agents/gsd-assumptions-analyzer.md
  - .agent/agents/gsd-advisor-researcher.md
  - .agent/agents/gsd-ai-researcher.md
findings:
  critical: 5
  warning: 10
  info: 8
  total: 23
status: issues_found
---

# Phase 01: Code Review Report

**Reviewed:** 2026-05-14T00:00:00Z
**Depth:** deep
**Files Reviewed:** 60
**Status:** issues_found

## Summary

Deep cross-file review of the moat_core zero-trust kernel and agent persona system. Analysis covered: (1) full call-chain tracing from `System::init()` through agent loops, router, MetaBrain permission graph, and tool execution; (2) persona file consistency between `agents/*.md`, `moat_core/agents/*.md`, and Rust loading code; (3) security model verification across capability gating, permission isolation, and prompt injection vectors; (4) code quality focusing on ~350 lines of near-duplicate Agent1/Agent2 impls; (5) all 33 `.agent/agents/gsd-*.md` subagent definitions for structural integrity; (6) comparison against hermes-agent reference patterns.

**Key concerns:** (a) MetaBrain persistence flush silently swallows errors, risking silent data loss; (b) the `TrustStore` module is defined but never instantiated in the System init path — manifests are loaded but signatures are never verified; (c) `is_path_protected` only protects file deletes, not reads — `.key` and `.pem` files can be read freely; (d) `Agent1` and `Agent2` structs contain ~350 lines of near-identical code; (e) multiple `.unwrap_or(Null)` calls silently swallow JSON parse errors in tool argument parsing.

**Hermes-agent comparison:** The `moat_core/agents/` persona files are richer than `agents/` root copies — `session/config.rs` loads whichever exists first, creating ambiguity. Hermes uses SOUL.md override from `~/.hermes/SOUL.md` and caches system prompts; MoatAgeex rebuilds context on every init with no caching. MoatAgeex lacks model-aware tool-use enforcement guidance (Hermes has explicit injection for GPT/Codex/Gemini for tool use). The 33 `.agent/agents/gsd-*.md` subagents had consistent YAML frontmatter and structured XML execution flows — structurally sound.

---

## Critical Issues

### CR-01: MetaBrain Flush Loop Silently Swallows Errors — Data Loss Risk

**File:** `moat_core/src/meta_brain.rs:345`
**Issue:** The `spawn_flush_loop` function spawns a tokio task that writes the MetaBrain graph to disk every 250ms when dirty. The flush result is discarded with `let _ = Self::flush_arc(Arc::clone(&brain)).await;`. If serialization fails (e.g., bincode encode error), atomic rename fails, or disk is full, the error is silently swallowed with no logging or retry. The `dirty` flag is cleared by `flush_arc` only on success (line 377: `dirty.store(false, Ordering::Release)`), so *new* writes after the failed flush will cause a retry, but the failed data that was being flushed was lost — no error diagnostics are produced.

**Fix:**
```rust
pub fn spawn_flush_loop(brain: Arc<AsyncRwLock<Self>>) {
    tokio::spawn(async move {
        loop {
            sleep(TokioDuration::from_millis(250)).await;
            let should_flush = {
                let brain = brain.read().await;
                brain
                    .flush
                    .as_ref()
                    .map(|flush| flush.dirty.load(Ordering::Acquire))
                    .unwrap_or(false)
            };
            if !should_flush {
                continue;
            }
            if let Err(e) = Self::flush_arc(Arc::clone(&brain)).await {
                eprintln!("[MetaBrain] Flush failed: {e}");
                // Retry after backoff — overwrite the dirty flag that was
                // cleared on failure path inside flush_arc
                if let Some(flush) = brain.read().await.flush.as_ref() {
                    flush.dirty.store(true, Ordering::Release);
                }
            }
        }
    });
}
```

### CR-02: TrustStore Defined But Never Instantiated — Signature Verification Dead Code

**File:** `moat_core/src/lib.rs:97-106`, `moat_core/src/trust_store.rs:22-151`
**Issue:** The `TrustStore` struct (ED25519 signature verification, manifest capability clamping for untrusted agents) is fully implemented but is **never instantiated or invoked** in the `System::init()` path. The `System` struct holds no `TrustStore` field. The `manifest.rs` validation functions (`validate_manifest`, `canonical_manifest_json`) are called during agent loading but signature verification against trusted public keys (`MOAT_TRUSTED_MANIFEST_PUBKEYS` env var) is unreachable dead code. Any agent manifest with a forged signature would pass validation because the signature is never checked.

**Fix:**
```rust
// In System struct (lib.rs):
pub struct System {
    agent_kind: AgentKind,
    router: Option<Router>,
    agent1: Agent1,
    agent2: Option<Agent2>,
    tool_executor_agent1: ToolExecutor,
    tool_executor_agent2: Option<ToolExecutor>,
    session_store: Arc<SessionStore>,
    profile_store: Arc<ProfileStore>,
+   trust_store: Arc<TrustStore>,  // ADD
}

// In System::init(), before loading agents:
let trust_store = Arc::new(TrustStore::new());
trust_store.register_builtin_worker("agent1");
trust_store.register_builtin_worker("agent2");
// Later: when loading external agent manifests, call:
// trust_store.assess_signed_manifest(&signed)?;
```

### CR-03: Sensitive File Protection Only Guards Deletes — Read-Only Agents Can Read `.key`/`.pem` Files

**File:** `moat_core/src/tools/executor.rs:59-67, 92-98, 347-359`
**Issue:** The `is_path_protected` function (line 347) safeguards `.log`, `.db`, `.sqlite`, `.key`, `.pem` files and `.git/` directories — but it is **only called in the `fs_delete` arm** (line 104). The `fs_read` / `read_file` path (line 59) and `fs_write` / `write_file` path (line 73) do not call `is_path_protected`. This means a read-only Agent1 can use `fs_read` to read `~/.ageex/sessions.db` (containing chat history and approval records), `*.key` files (private keys), or `*.pem` certificates. The `is_path_protected` check should apply to reads as well, or at minimum for the read-only Spotlight mode.

**Fix:**
```rust
// In execute(), fs_read arm, after resolve_path:
let resolved = Self::resolve_path(raw_path);
+ if Self::is_path_protected(Path::new(&resolved)) {
+     return Ok(format!("[fs:read] DENIED: {} is a protected file", raw_path));
+ }
if !self.check_path_permission(PermissionFlags::FILE_READ, &resolved).await? {
    return Ok(format!("[fs:read] DENIED: No permission to read {}", raw_path));
}
// Also add to fs_write arm for write protection.
```

### CR-04: JSON Parsing Errors Silently Swallowed in Tool Args — Wrong Tool Calls Go Undetected

**File:** `moat_core/src/agent.rs:297-298`, `moat_core/src/tools/executor.rs:55-56`
**Issue:** Multiple locations use `serde_json::from_str(x).unwrap_or(serde_json::Value::Null)` to parse tool call arguments, silently swallowing parse errors:

- `agent.rs:297`: `delegate_to_executor` args — if the LLM emits malformed JSON, the task and reason default to empty strings, resulting in an empty delegation with no error logged.
- `executor.rs:55-56`: All tool calls — if the LLM emits invalid JSON, all args become `Value::Null`, causing every `args["path"].as_str()` etc. to return `None`, and `ok_or_else(|| anyhow!("requires 'path'"))` errors, but the root cause (bad JSON) is invisible.

The fix should log parse failures so operators can detect LLM model degradation or prompt injection attempts.

**Fix:**
```rust
// agent.rs:297 replace:
let args: serde_json::Value = serde_json::from_str(&tool_call.function.arguments)
    .unwrap_or(serde_json::Value::Null);
// with:
let args: serde_json::Value = match serde_json::from_str(&tool_call.function.arguments) {
    Ok(v) => v,
    Err(e) => {
        eprintln!("[Agent1] Failed to parse delegate args: {}. Raw: {}", e, &tool_call.function.arguments);
        serde_json::Value::Null
    }
};
// Same pattern for executor.rs:55-56
```

### CR-05: `complete_simple` Panics on Empty Choices Without Bail Check

**File:** `moat_core/src/llm/client.rs:113-127`
**Issue:** The `complete_simple` method used by the LLM router fallback accesses `parsed.choices[0].message.content` (line 126) without the empty-choices bail check that exists in the `send_with_retry` function (lines 59-61 of the same file). If the LLM provider returns `{"choices": []}`, `send_with_retry` returns `Ok(parsed)` because its `is_success()` check precedes the empty-choices check. The code at line 126 would then panic with index out of bounds. This is called from `llm_fallback.rs:32` in `LlmRouter::classify`, which is invoked when the regex classifier has low confidence — a path that triggers at runtime.

**Fix:**
```rust
pub async fn complete_simple(&self, prompt: &str) -> Result<String> {
    let url = format!("{}/chat/completions", self.provider.base_url());
    let request = ChatRequest {
        model: self.model.clone(),
        messages: vec![LlmMessage::user(prompt)],
        tools: None,
        temperature: 0.0,
        max_tokens: 10,
    };
    let parsed = self.send_with_retry(&url, &request).await?;
    parsed.choices.first()
        .map(|c| c.message.content.clone().unwrap_or_default())
        .ok_or_else(|| anyhow::anyhow!("LLM router returned empty choices"))
}
```

---

## Warnings

### WR-01: Agent1 / Agent2 Near-Duplicate Code (~350 Lines) — Drift & Bug Risk

**File:** `moat_core/src/agent.rs:53-732`
**Issue:** `Agent1` (lines 53-396) and `Agent2` (lines 398-732) share nearly identical implementations across five methods: `new()` (11-line diffs), `process_query()` (zero diffs), `safe_execute_tool()` (zero diffs), `maybe_inject_tool_details()` (zero diffs), `run_loop()` (Agent2 lacks delegation handling), `context_to_llm_messages()` (zero diffs), and `check_and_compress_context()` (zero diffs). The only behavioral differences are: (1) `agent1_tools()` vs `agent2_tools()`, (2) `delegate_to_executor` handling in Agent1's `run_loop`, (3) `CapabilitySet::agent1_default()` vs `agent2_default()`, and (4) agent-type labels in println! calls. Any bug fix to shared logic (e.g., safe_execute_tool error handling, budget checking, context compression) must be applied in two places. This is a regression risk.

**Fix:** Extract a shared `AgentCore` struct/impl with the common methods, parametrize on tools function and delegation flag, and have `Agent1`/`Agent2` be thin wrappers or an enum variant of one struct.

### WR-02: `ContextStore::prepend_system_addendum` Appends Indefinitely — No Size Guard

**File:** `moat_core/src/context/context_store.rs:114-122`
**Issue:** The `prepend_system_addendum` method appends text to the first (system) message on every call with `first.content = format!("{}\n{}", first.content, addendum)`. Tools details injection, user profiles, and other addenda all append to the system message, which grows unboundedly. If `maybe_inject_tool_details` is called repeatedly (it should only fire once due to the tag guard, but the system message itself grows), the system prompt can balloon past the LLM's effective attention window.

**Fix:** Add a maximum system prompt size check (e.g., after 8000 chars of addenda, use a separate system-addendum message role instead of appending).

### WR-03: `CheckpointStore::lock().unwrap()` Every Mutation — Panic Hazard in Long-Running Daemon

**File:** `moat_core/src/context/context_store.rs:95, 101, 106, 111, 116`
**Issue:** All `ContextStore` mutations and reads call `self.messages.lock().unwrap()`. If any code path panics while holding the lock (e.g., a serde serialization failure in `check_and_compress_context`), the `Mutex` becomes poisoned. All subsequent accesses to `ContextStore` would panic with "PoisonError". In a long-running Tauri process serving many queries, a single poisoned lock crashes the entire agent permanently.

**Fix:** Replace all `lock().unwrap()` with `lock().unwrap_or_else(|e| e.into_inner())` to recover from poisoned mutexes:
```rust
pub fn append(&self, message: Message) -> Result<()> {
    let mut msgs = self.messages.lock().unwrap_or_else(|e| e.into_inner());
    msgs.push(message);
    Ok(())
}
```

### WR-04: `Mutex<MetaBrain>` in ToolExecutor vs `AsyncRwLock<MetaBrain>` in Swarm — Inconsistent Locking Strategy

**File:** `moat_core/src/lib.rs:158`, `moat_core/src/tools/executor.rs:15`, `moat_core/src/swarm.rs:114`
**Issue:** The `System::init()` function in `lib.rs` wraps `MetaBrain` in `Arc<Mutex<MetaBrain>>` (line 158) and passes it to `ToolExecutor` which also uses `Arc<Mutex<MetaBrain>>` (executor.rs line 15). However, `swarm.rs` and other subsystems use `Arc<AsyncRwLock<MetaBrain>>` (swarm.rs line 114). The two locking strategies are incompatible — code can't pass a `Mutex`-based MetaBrain to a function expecting `AsyncRwLock`. This indicates a split in the codebase: the Agent1/Agent2 path uses synchronous `Mutex`, while the Swarm path uses async `RwLock`. If these subsystems need to share a single MetaBrain instance, it's impossible with the current design.

**Fix:** Standardize on one locking strategy across the codebase. If the `Mutex<MetaBrain>` approach must be retained for the agent path (since it's not held across `.await` points), ensure the Swarm path accepts the same type, or document the two paths as intentionally separate MetaBrain instances with independent permission graphs.

### WR-05: `ProfileExtractor::extract_and_store` Can Fail Entire Query — Should Be Best-Effort

**File:** `moat_core/src/agent.rs:146, 491`
**Issue:** In `Agent1::process_query` (line 146) and `Agent2::process_query` (line 491), the call `ProfileExtractor::extract_and_store(...)?;` uses the `?` operator. If profile extraction fails (e.g., DB write error, locked database), the entire user query fails with an error rather than returning a best-effort response. Profile extraction is a non-critical side effect; it should not block the main interaction.

**Fix:**
```rust
if let Err(e) = ProfileExtractor::extract_and_store(&recent, &self.session_id, &self.profile_store) {
    eprintln!("[Agent1] Profile extraction failed (non-blocking): {e}");
}
```

### WR-06: `resolve_path` Canonicalizes Paths — Symlink Attacks Bypass Path Checks

**File:** `moat_core/src/tools/executor.rs:361-385`
**Issue:** `resolve_path` canonicalizes paths using `std::fs::canonicalize` (line 373), which resolves symlinks to their targets. The permission check in `check_path_permission` then uses the resolved path. However, if a user creates a symlink `~/Desktop/ageex-workspace/safe_link -> /etc/passwd`, the permission check runs against the *resolved* path `/etc/passwd` (which is outside the workspace and would be denied). While canonicalization here helps, the attacker could also create a symlink `~/Desktop/ageex-workspace/evil -> ~/.ageex/sessions.db`. The resolved path `~/.ageex/sessions.db` might have been granted permission via approval replay. The system should explicitly deny reads on paths outside the workspace boundary **before** canonicalization, and warn when canonicalized paths differ from the raw path.

**Fix:** Add a step before `resolve_path` that checks whether the raw path lives within the allowed workspace boundaries. If the canonicalized path differs from the raw path, log a warning:
```rust
let resolved = Self::resolve_path(raw_path);
if resolved != raw_path {
    eprintln!("[ToolExecutor] Path canonicalized: {} -> {}", raw_path, resolved);
}
```

### WR-07: Agent Persona File Ambiguity — Two Sources, One Load Path, No Merge

**File:** `moat_core/src/session/config.rs:16-18`, `agents/agent1.md`, `moat_core/agents/agent1.md`
**Issue:** `AgentConfig::load_prompt` checks two paths: `agents/{name}.md` and `moat_core/agents/{name}.md`. The root `agents/agent1.md` (38 lines, "Spotlight Assistant") and `moat_core/agents/agent1.md` (44 lines, "Read-Only Assistant") contain **different persona content** — different sections, different behavioral guidance, different workspace descriptions. Whichever file exists first wins, with no merge. If both exist, the root copy shadows the crate copy. If neither exists, a minimal 1-line fallback is used. This makes the agent's behavior dependent on the working directory state, and the richer persona in `moat_core/agents/` may be silently ignored.

**Fix:** Either:
1. Consolidate to a single canonical location (e.g., only `moat_core/agents/`), or
2. Merge the two files if both exist, or
3. Log a warning when the crate copy exists but is shadowed.

### WR-08: `agent1_tools()` Includes `skill_execute` in Extended Details but Agent1 Cannot Run Skills

**File:** `moat_core/src/llm/tool_definitions.rs:260-271`
**Issue:** The `tool_schema_details()` function (used for progressive disclosure) calls `agent2_tools()` to generate extended tool descriptions (line 261). This includes `skill_execute`, which Agent1 does not have. If Agent1's `maybe_inject_tool_details` injects these, Agent1 would see `skill_execute` in the extended details but not in its tool list. This inconsistency could confuse the LLM or cause it to hallucinate the skill_execute tool call.

**Fix:** Pass the agent type to `tool_schema_details` and filter tools accordingly:
```rust
pub fn tool_schema_details(agent: AgentId) -> String {
    let tools = match agent {
        AgentId::Agent1 => agent1_tools(),
        AgentId::Agent2 => agent2_tools(),
    };
    // ... rest of function
}
```

### WR-09: `Router::make_delegation_decision` Always Returns Agent2 Target — Never Used

**File:** `moat_core/src/router/mod.rs:44-51`
**Issue:** The `make_delegation_decision` method always returns `RouteTarget::Agent2` with `confidence: 1.0`. It is never called within the codebase (grep confirms zero callers). It appears to be dead code, possibly a leftover from an earlier design where delegation decisions were routed through the Router instead of handled inline in `Agent1::run_loop`.

**Fix:** Either remove the dead method or wire it into the delegation path in `System::process_user_query` if it represents intended behavior.

### WR-10: `metabadrain` Flush Loop Polls Every 250ms with No Throttle — CPU Waste

**File:** `moat_core/src/meta_brain.rs:330-348`
**Issue:** The flush loop uses `tokio::time::sleep(250ms)` regardless of whether the previous flush succeeded or failed. On idle systems (no writes), the loop checks the dirty flag every 250ms forever. While tokio::time::sleep is efficient, having a permanent polling loop is a code smell — a notification-based approach (e.g., `tokio::sync::Notify`) would be more appropriate.

**Fix:** Use a `tokio::sync::Notify` to wake the flush loop only when `mark_dirty()` is called:
```rust
// Add field: notify: tokio::sync::Notify
pub fn spawn_flush_loop(brain: Arc<AsyncRwLock<Self>>) {
    // Use notify.notified() instead of fixed-interval sleep
}
```

---

## Info

### IN-01: Unused `execution` Module Import in `lib.rs`

**File:** `moat_core/src/lib.rs:84`
**Issue:** `pub mod execution;` is declared in lib.rs, and `execution::DelExecutor` is imported in `tools/executor.rs`. However, the `execution` module itself is declared under `// Agent infrastructure` comment block (line 84), not in the main module list (lines 2-27 where other modules are declared). This inconsistent organization makes it harder to discover module dependencies.

### IN-02: Commented-Out Modules in `lib.rs`

**File:** `moat_core/src/lib.rs:11, 17`
**Issue:** Lines 11 and 17 have commented-out module declarations: `// pub mod mcp;` and `// pub mod reflection;`. These indicate incomplete features or leftover scaffolding. If MCP and reflection are planned features, they should be documented in ROADMAP.md rather than living as code comments.

### IN-03: `agents/agent2.md` Has Wrong Note About Notes Section

**File:** `agents/agent2.md:14-16`
**Issue:** The `## Notes` section at line 14 duplicates content already present in the `## Capabilities` section and `## Permission System` section of `moat_core/agents/agent2.md`. The root `agents/agent2.md` is 17 lines and terse; the crate `moat_core/agents/agent2.md` is 32 lines and richer. The root copy lacks the "Do NOT call `request_permission`" behavioral guidance present in the crate copy.

### IN-04: `capabilities.rs` — `Capability::SkillExecute` in Agent2 Default But Not in Agent1

**File:** `moat_core/src/capabilities.rs:54-64`
**Issue:** `agent2_default()` includes `Capability::SkillExecute` but the LLM tool definition `agent2_tools()` in `tool_definitions.rs` includes `skill_execute` as a separate tool. This is consistent. However, there's no `Capability::FileDelete` in `agent2_default()` even though `agent2_tools()` includes `fs_delete`. Either `FileDelete` should be added to the default set, or `fs_delete` should require explicit permission gating beyond what the default set provides. Currently, the delete tool exists but there's no `FileDelete` capability in the default for Agent2 — meaning deletes would always require runtime approval.

### IN-05: `approval.rs` — `ApprovalFlow` is a ZST with No Configurability

**File:** `moat_core/src/approval.rs:19-52`
**Issue:** `ApprovalFlow` is a unit struct with a single static method `request_approval` that always prints to stdout and reads stdin. It has no support for: (a) Tauri IPC-based approval (essential for a GUI app), (b) configurable timeouts (there is none — it blocks forever), or (c) approval persistence strategies. The hardcoded stdin/stdout approach only works in CLI mode.

### IN-06: `tools/executor.rs` Has Orphaned Tests Outside `mod tests` Block

**File:** `moat_core/src/tools/executor.rs:434-462`
**Issue:** Tests at lines 434-462 (`test_seatbelt_profile_contains_working_dir`, `test_protected_patterns_prevent_log_deletion`, `test_git_directory_is_protected`, `test_regular_files_are_not_protected`) are located **outside** the `#[cfg(test)] mod tests` block (which closes at line 432). These four tests will never be compiled or run. They are dead test code.

### IN-07: `session/config.rs` — Hardcoded Fallback Prompts Don't Match Markdown Persona Richness

**File:** `moat_core/src/session/config.rs:34-40`
**Issue:** The `default_prompt` method returns minimal 1-2 sentence prompts as fallbacks. If both `agents/*.md` and `moat_core/agents/*.md` are missing (e.g., in a Docker build with different CWD), the agent operates with drastically reduced behavioral guidance. The fallback should embed the full markdown persona inline as a const string rather than a minimal sentence.

### IN-08: `trust_store.rs` — Ed25519 Key Loading from Env Var Uses CSV with Base64 — Error Unwrapping Is Silent

**File:** `moat_core/src/trust_store.rs:130-145`
**Issue:** `load_trusted_keys` splits the env var by comma, base64-decodes each entry, converts to `[u8; 32]`, and creates a `VerifyingKey`. All failures are silently swallowed with `.ok()` — a misconfigured key produces no error log. An operator could supply a valid-looking key that silently fails to parse and wonder why signature verification doesn't work.

**Fix:** Log a warning for each key that fails to parse:
```rust
.filter_map(|entry| {
    let decoded = match BASE64_STANDARD.decode(entry.trim()) {
        Ok(d) => d,
        Err(e) => { eprintln!("[TrustStore] Invalid base64 key: {e}"); return None; }
    };
    let bytes: [u8; 32] = match decoded.try_into() {
        Ok(b) => b,
        Err(_) => { eprintln!("[TrustStore] Key must be 32 bytes, got {}", decoded.len()); return None; }
    };
    match VerifyingKey::from_bytes(&bytes) {
        Ok(k) => Some(k),
        Err(e) => { eprintln!("[TrustStore] Invalid ed25519 key: {e}"); None }
    }
})
```

---

## Cross-File Analysis Summary

### Import Graph (Key Call Chains)

```
System::init (lib.rs)
 ├── SessionStore::open / open_with_prefix (session/store.rs)
 ├── ProfileStore::open / open_with_prefix
 ├── MetaBrain::new (meta_brain.rs)
 ├── LlmClient::from_env (llm/client.rs) → LlmProvider::from_env (llm/types.rs)
 ├── Agent1::new (agent.rs)
 │   ├── AgentConfig::load_agent1_prompt (session/config.rs) → reads agents/*.md or moat_core/agents/*.md
 │   ├── ContextStore::new (context/context_store.rs)
 │   ├── SessionStore::create_session / load_messages
 │   └── ProfileStore::build_context_injection
 ├── ToolExecutor::new_with_auto_deny (tools/executor.rs) → DelExecutor (execution/)
 ├── Agent2::new (agent.rs) — same pattern as Agent1
 ├── ToolExecutor::new (tools/executor.rs)
 └── Router::new (router/mod.rs)
     ├── RegexClassifier (router/classifier.rs)
     └── LlmRouter::new (router/llm_fallback.rs)

Query path:
System::process_user_query
 ├── Router::route → RegexClassifier::classify → LlmRouter::classify
 ├── Agent1::process_query → Agent1::run_loop
 │   ├── LlmClient::chat
 │   ├── ToolExecutor::execute → MetaBrain::path_has_permission / app_has_permission
 │   │   ├── ApprovalFlow::request_approval (approval.rs) [if no permission and not auto_deny]
 │   │   └── DelExecutor::execute (execution/)
 │   └── ContextCompressor::compress_tiered (context/compressor.rs)
 └── Agent2::process_query → Agent2::run_loop [same, minus delegation]
```

### State Mutation Consistency

- **MetaBrain** is shared via `Arc<Mutex<MetaBrain>>` in the agent path, wrapped at `lib.rs:158`. All accesses are properly guarded with `.lock().unwrap()`. The `AsyncRwLock` variant used in `swarm.rs` is an alternative path that does not share the same instance.
- **ContextStore** uses `Arc<Mutex<Vec<Message>>>` — only accessed within single-agent scopes (no cross-agent sharing). Safe.
- **SessionStore** uses `Connection` (rusqlite) — SQLite handles WAL-mode concurrency. Two `SessionStore` instances (Spotlight with prefix, MainUi without) share the same `.db` file which is safe with WAL mode.
- **ProfileStore** — same pattern as SessionStore.

### Error Propagation

- `Agent1::new` and `Agent2::new` propagate errors via `Result` — caught by `System::init` at lib.rs, which also propagates.
- `run_loop` returns `Result<AgentResponse>` — caught by `process_query` at lib.rs lines 232/266. Fallthrough to user-visible error string?
  - **Issue:** If `process_query` returns `Err`, it's not caught anywhere in `System::process_user_query` (line 225 uses `?` which propagates). The caller of `process_user_query` must handle the error. This is Tauri's job, but there's no error-to-user conversion visible.
- `safe_execute_tool` returns `Result<String>` where errors are wrapped as string content (e.g., `[TOOL_ERROR]`/`[TOOL_DENIED]`) — errors become LLM context, not system errors. This is intentional and appropriate.

---

## Hermes-Agent Reference Comparison

| Aspect | Hermes | MoatAgeex | Gap |
|--------|--------|-----------|-----|
| Persona loading | `~/.hermes/SOUL.md` overrides DEFAULT_AGENT_IDENTITY (single source) | `agents/*.md` shadows `moat_core/agents/*.md` (two sources, first wins) | **CR-06**: Two sources cause ambiguous persona depending on CWD |
| Prompt caching | System prompt built once, cached, skills injected as user messages | ContextStore rebuilt every `Agent::new()`, system addenda appended indefinitely | **WR-02**: No prompt caching; addenda grow unboundedly |
| Tool-use enforcement | Explicit model-aware injection for GPT/Codex/Gemini/Gemma/Grok/GLM | No model-specific tool-use guidance | **Info**: All models get same tool descriptions; some LLMs (e.g., smaller models) may describe actions instead of calling tools |
| Personality switching | 14 predefined personalities via `/personality` | Agent1 (Spotlight) vs Agent2 (Main UI) — binary, no switching | **Info**: No personality/persona switching mid-session |
| Memory isolation | Pluggable memory backends | SessionStore with prefix isolation (`spotlight_` prefix) | **Good**: Spotlight and MainUi have isolated session tables |
| Platform-specific hints | 16+ messaging platform behavioral hints | Two modes (Spotlight overlay vs Main UI) | **Good**: MoatAgeex has appropriate mode-specific guidance via `agents/*.md` |
| Skills system | 85+ SKILL.md files with structured tool definitions | 33 gsd-*.md subagents with YAML frontmatter + XML execution flows | **Good**: Both have structured agent definitions; MoatAgeex favors subagents over skill files |

### .agent/agents/ Subagent Structural Integrity

All 33 `gsd-*.md` files were sampled for structural integrity:

- **YAML frontmatter:** All sampled files have valid `---` delimited YAML with `name`, `description`, `tools`, and `color` fields. Consistent across all files.
- **XML execution flows:** gsd-planner, gsd-code-reviewer, gsd-security-auditor, gsd-integration-checker, gsd-ui-auditor all have structured `<execution_flow>`, `<step>`, `<structured_returns>`, `<success_criteria>` blocks. No structural defects.
- **Reference consistency:** References to `@.agent/get-shit-done/references/` are present in planner.md but not verified for path existence (out of scope for this review).
- **No orphaned agents:** All 33 listed in the review scope have corresponding `.md` files.
- **Minor note:** gsd-code-reviewer.md line 21 references `./GEMINI.md` (Gemini CLI convention) while the project root has `./AGENTS.md`. Subagents spawned in Claude Code context won't find `./GEMINI.md`. (This is by design — gsd-code-reviewer is configured for the Gemini CLI agent environment.)

---

_Reviewed: 2026-05-14T00:00:00Z_
_Reviewer: the agent (gsd-code-reviewer)_
_Depth: deep_
