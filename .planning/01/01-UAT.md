---
status: complete
phase: 01-agent-architecture-audit
source: ROADMAP.md success criteria
started: 2026-05-14T00:00:00Z
updated: 2026-05-15T00:00:00Z
---

## Current Test

[testing complete]

## Tests

### 1. Persona-Code Consistency
expected: AGENT personas in markdown match Rust capability sets and tool definitions. Agent1 is read-only, Agent2 is full-capability executor.
result: pass
verified: agent1_default() = FileRead/FileExists/OsProcessSpawn/AppOpen/UrlOpen (no SkillExecute/AgentSpawn). agent2_default() = all caps including SkillExecute + AgentSpawn.

### 2. Capability Gating Verification
expected: Agent1 cannot execute skill_execute at runtime (P5 fix). Agent1 default lacks SkillExecute. OsProcessSpawn separated from AgentSpawn. WR-08 filters skill_execute from Agent1 tool defs.
result: pass
verified: ExecutionError::CapabilityDenied for Agent1 skill_execute. Test included in 167 suite.

### 3. Security Model Audit
expected: TOCTOU file ops use O_NOFOLLOW. Symlink traversal denied (not logged). Destructive ops (fs_delete/app_open/url_open) require consent via ConsentBridge. consent routes through MetaBrain grant to prevent re-prompting. Protected files (.key, .pem, .db, .sqlite, .log, .git/, Cargo.lock) blocked for read/write/delete.
result: pass
verified: O_NOFOLLOW at executor.rs:557,562. ConsentBridge gating on fs_delete/app_open/url_open. MetaBrain grant_post consent.

### 4. Prompt Injection Hardening
expected: System messages wrapped in <|SYSTEM_START|>...<|SYSTEM_END|>. Tool/user messages have delimiter patterns stripped and replaced with [FILTERED].
result: pass
verified: Message::system() wraps delimiters. strip_instruction_delimiters() filters from user/tool. Tests included.

### 5. Tool-Call Protocol Correctness
expected: Two-phase tool execution (collect all results, then atomic append). PC-01 sequencing invariant rejects tool-after-user. PC-03 validates non-empty tool_call.id. Empty choices in complete_simple don't panic.
result: pass
verified: Phase 1 collect, Phase 2 atomic append in agent.rs. PC-01 bail! on tool-after-user. PC-03 debug_assert on empty id. CR-05 safe choices access.

### 6. Subagent Definitions Integrity
expected: All 33 .agent/agents/gsd-*.md files have valid YAML frontmatter with name/description/tools/color fields. No structural corruption.
result: pass
verified: All 33 files structurally intact. No corruption found in review.

### 7. Test Coverage Baseline
expected: cargo test --all passes with 167 tests minimum. No regressions. All Wave 1-6 tests included.
result: pass
verified: 167 passed, 0 failed, 0 ignored.

### 8. Build Health
expected: cargo check produces zero errors. Warnings limited to pre-existing deprecation notices.
result: pass
verified: 0 errors. Only pre-existing deprecation + unused import warnings.

## Summary

total: 8
passed: 8
issues: 0
pending: 0
skipped: 0

## Gaps

[none yet]
