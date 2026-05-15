# AGEEX Production-Ready System Blueprint
## Complete Reference Analysis + Implementation Roadmap

**Generated:** 2026-05-15
**Updated:** 2026-05-15 (Wave 7 complete)
**References analyzed:** hermes-agent, openclaw, OpenJarvis
**Current state:** phase10-final (167 tests, 0 failures). Wave 7 (DEL Integration) complete.
**Design authority:** `Ageex_system_plane&design/` (7 documents)

---

## 1. Reference System Comparison Matrix

| Capability | AGEEX Current | hermes-agent | openclaw | OpenJarvis | Target |
|------------|--------------|-------------|----------|-----------|--------|
| Read Agent (AGX1) | BUILT (95%) | Built (AIAgent+Platform Hints) | Built (PI agents) | Built (simple agent) | **100%** |
| Write Agent (AGX2) | BUILT (95%) | Built (ToolUsingAgent) | Built (PI agents) | Built (orchestrator agent) | **100%** |
| Connector Agent (AGX3) | MISSING | Built (34 platform adapters) | Built (30+ channels) | Built (channel layer) | **BUILD** |
| DEL/Gate System | BUILT (100%) ✓ | Built (toolset gating) | Built (tool policy) | Built (guardrails) | **100%** |
| MetaBrain/Permissions | BUILT (100%) | N/A (string-based) | N/A (allowlist) | Built (GuardrailsEngine) | **100%** |
| Consent/Approval | BUILT (95%) | Built (approval.py) | Built (exec approvals) | Built (Guardrails) | **100%** |
| Chain Journal | BUILT (90%) | N/A | N/A | Built (audit logger) | **100%** |
| Memory Manager | PARTIAL (40%) | Built (MemoryManager + 8 providers) | Built (memory-core plugin) | Built (5 backends) | **BUILD** |
| Multi-Platform Gateway | PARTIAL (15%) | Built (34 adapters) | Built (30+ channels) | Built (channel layer) | **BUILD** |
| Cron/Scheduling | MISSING | Built (cron/ scheduler) | Built (cron/ service) | Built (scheduler crate) | **BUILD** |
| Skills System | BUILT (100%) | Built (skills/ + guard) | Built (53 skills) | Built (skills/ + signing) | **ENHANCE** |
| Prompt Injection Defense | BUILT (delimiters) | Built (10 threat patterns) | Built (SECURITY.md) | Built (Scanner pipeline) | **ENHANCE** |
| Multi-User/Enterprise | MISSING | Built (mem0 multi-user) | Not designed | Not designed | **LATER** |
| Frontend | BUILT (85%) | Built (TUI + Web) | Built (Control UI) | Built (Tauri + NSPanel) | **ENHANCE** |
| Deployment | MISSING (macOS only) | Built (Docker) | Built (Docker + Fly) | Built (Docker + GPU) | **BUILD** |
| Documentation | PARTIAL | Built (docs/) | Built (docs/) | Built (mkdocs + mermaid) | **BUILD** |

---

## 2. What to Adopt — By Reference System

### 2.1 From hermes-agent

| # | Pattern | Adoption Logic | Complexity | Priority |
|---|---------|---------------|------------|----------|
| H1 | **Multi-platform gateway adapter pattern** (`gateway/platforms/base.py`) | AGEEX's AGX3 needs per-platform channel adapters with the hermes `BasePlatformAdapter` abstract class pattern. Map `SessionSource` → platform adapters that handle message normalization, reply routing, and media delivery per platform. | HIGH | **CRITICAL** |
| H2 | **Platform hints for prompt injection** (`prompt_builder.py:411-550`) | When AGX3 responds to external platforms, inject platform-specific formatting rules into the system prompt (WhatsApp=no markdown, Telegram=no tables, Discord=media syntax). Prevents malformed responses. | LOW | **CRITICAL** |
| H3 | **Subagent delegation with blocked tools** (`delegate_tool.py:5 blocked tools`, `max_spawn_depth`) | AGEEX's Agent1→Agent2 delegation needs capability stripping. Define `CapabilitySet::subagent_default()` that strips `FileDelete`, `AppOpen`, `UrlOpen`, `SkillExecute` from child agents. Enforce `MAX_SPAWN_DEPTH=1`. | MEDIUM | HIGH |
| H4 | **Memory Manager with pluggable backends** (`agent/memory_manager.py`, `plugins/memory/`) | AGEEX's Wave 9 (Memory Manager Agent). Adopt hermes `MemoryManager` → `MemoryProvider` ABC pattern. `build_system_prompt()` for pre-turn injection. `prefetch_all(user_message)` for recall. `sync_all()` for post-turn persistence. Context fencing with `<memory-context>` tags. | LOW-MEDIUM | HIGH |
| H5 | **ToolCallGuardrailController** (`agent/tool_guardrails.py`) | Detect loops where model repeatedly calls the same failing tool. AGEEX already has `HashDedupDetector` and `PingPongDetector` (WM-02). Enhance with patience counter, repeated failure pattern detection, and automatic loop-stop injection. | LOW | MEDIUM |
| H6 | **Permanent allowlist for consent** (`tools/approval.py:allowlist`) | User-configurable commands/paths that never require consent. Write to MetaBrain with `CreatePermanentPolicy` → persists across sessions. Reduces consent fatigue. | LOW | MEDIUM |
| H7 | **Cron/scheduler system** (`cron/jobs.py`, `cron/scheduler.py`) | Autonomous scheduled tasks. Job store (SQLite), scheduler loop (background thread), per-job overrides (skills/model/toolsets), multi-platform delivery. File-lock-based duplicate tick prevention. 3-min hard interrupt on cron sessions. | MEDIUM | MEDIUM |
| H8 | **SOUL.md personality override** (`prompt_builder.py:load_soul_md()`) | Add `~/.ageex/SOUL.md` that replaces agent identity. Scan for prompt injection before loading. Per-profile SOUL.md variants. Used as PRIMARY identity when present. | LOW | MEDIUM |
| H9 | **Credential scoping** (`env_passthrough.py`, `_HERMES_PROVIDER_ENV_BLOCKLIST`) | Strip API keys from subprocess environments. Skills declare `required_environment_variables` but provider credentials blocked via deny-list. | LOW | MEDIUM |
| H10 | **Skills Guard static analysis** (`tools/skills_guard.py:50+ threat patterns`) | Scan externally-sourced skills for injection/exfiltration/destruction patterns before install. Regex-based static analysis with trust tiers (builtin/trusted/community). | LOW-MEDIUM | MEDIUM |
| H11 | **File write denylist** (`file_safety.py:build_write_denied_paths()`) | Block writes to `~/.ssh/`, `~/.env`, shell RC files, credential files, `/etc/sudoers`. Add to AGEEX's MetaBrain protected paths. | LOW | HIGH |

### 2.2 From openclaw

| # | Pattern | Adoption Logic | Complexity | Priority |
|---|---------|---------------|------------|----------|
| O1 | **Channel lifecycle manager** (`src/gateway/server-channels.ts`) | Channel start/stop/health/restart with backoff and concurrency control. Per-channel runtime isolation. Health monitoring with restart policies. Essential for multi-platform reliability. | HIGH | **CRITICAL** |
| O2 | **Plugin SDK with manifest discovery** (`plugin-sdk/channel-contract.ts`) | AGEEX already has `sdk.rs` traits and `plugins.rs` manifest discovery. Expand SDK surface per plugin type: channel adapters, LLM providers, memory backends, tool handlers. Keep manifest format compatible with `openclaw.plugin.json` (already done). | MEDIUM | HIGH |
| O3 | **Provider abstraction layer** (`extensions/openai/`, `extensions/anthropic/`) | Separate LLM providers from core. Define `trait LlmProvider` with auth/catalog/completion. Plugins register via manifest. AGEEX currently has hardcoded `LlmClient` — needs abstraction. | HIGH | HIGH |
| O4 | **Channel→Session binding system** (`channels/plugins/binding-registry.ts`) | Map external channel identifiers (chat_id, user_id) to internal session contexts. Routing controls (not auth boundaries). Per-channel context visibility modes. | HIGH | HIGH |
| O5 | **Webhook system** (`extensions/webhooks/`) | Authenticated inbound HTTP webhooks with route/path/secret resolution. Binds external automation triggers to AGEX sessions. Controller-scoped to specific session. | MEDIUM | MEDIUM |
| O6 | **Security audit CLI** (`openclaw security audit --deep`) | `ageex security audit` command: scans config, checks MetaBrain integrity, validates chain journal, lists open consent gaps, flags dangerous configs. | LOW | MEDIUM |
| O7 | **Dangerous config flag taxonomy** | `dangerously*` prefix for break-glass options (e.g., `dangerously_bypass_consent`, `dangerously_allow_network_all`). Explicitly documents risk. | LOW | LOW |
| O8 | **Docker deployment** (`Dockerfile`, `docker-compose.yml`) | Multi-service Docker deployment: gateway + reasoning service. Non-root user, read-only FS option, capability dropping, health check endpoint. | MEDIUM | HIGH |
| O9 | **Temp folder isolation** | Dedicated `/tmp/ageex` root instead of raw `os.tmpdir()`. Prevents temp file collision and path traversal. | LOW | LOW |

### 2.3 From OpenJarvis

| # | Pattern | Adoption Logic | Complexity | Priority |
|---|---------|---------------|------------|----------|
| J1 | **Native macOS Overlay (NSPanel+WKWebView)** (`lib.rs:1204-1500`) | AGEEX's Spotlight Agent1 needs: NSPanel with `NSWindowStyleMaskNonActivatingPanel` (doesn't steal focus), `NSFloatingWindowLevel` (always on top), `canJoinAllSpaces` (cross-Space), `setMovableByWindowBackground` (drag anywhere), WKWebView with full transparency, WKScriptMessageHandler bridge (JS↔Rust), frontmost app restoration on hide. | LOW | **CRITICAL** |
| J2 | **Registry Pattern** (`core/registry.py`) | Define `trait RegistryItem: Send + Sync { fn key(&self) -> &str; }` and `struct Registry<T: RegistryItem>`. Use `#[derive(Registry)]` macro. Apply to: agents, tools, memory backends, LLM providers, scanners, skills. Enables runtime component swapping. | MEDIUM | HIGH |
| J3 | **EventBus pub/sub** (`core/events.py`) | `EventBus` with strongly-typed Rust enum variants: `ToolCallStart`, `ToolCallEnd`, `ConsentRequest`, `ConsentDecision`, `SecurityViolation`, `MemoryUpdate`, `AgentStep`. Decouples telemetry, tracing, consent logging from orchestrator. | LOW | HIGH |
| J4 | **GuardrailsEngine composable wrapper** (`security/guardrails.py`) | `trait LlmBackend { fn complete(&self, messages: &[Message]) -> Response; }`. `struct GuardedLlm<B: LlmBackend> { inner: B, scanners: Vec<Box<dyn Scanner>> }`. Scans both input (prompt injection) and output (data exfiltration). | MEDIUM | HIGH |
| J5 | **Content Scanner Pipeline** (`security/scanner.py`) | `trait Scanner { fn scan(&self, text: &str) -> ScanResult; fn redact(&self, text: &str, findings: &[ScanFinding]) -> String; }`. SecretScanner (API keys, tokens), PIIScanner (emails, credit cards). Guards data leaks (orthogonal to MetaBrain which guards actions). | MEDIUM | HIGH |
| J6 | **Multi-crate architecture** (17 crates) | Split 5910-line `orchestrator.rs` into: `moat_telemetry`, `moat_traces`, `moat_tools`, `moat_security`, `moat_workflow`, `moat_scheduler`. Following OpenJarvis's crate-per-concern pattern. | MEDIUM | MEDIUM |
| J7 | **Command Palette (Cmd+K)** (`components/CommandPalette.tsx`) | Global search + agent switching. Reuses existing Zustand store. ~300 lines TypeScript. | LOW | MEDIUM |
| J8 | **Dashboard Page** (`pages/DashboardPage.tsx`) | Recharts energy/telemetry dashboards: capability usage, consent decisions, agent activity, tool call metrics, memory trends. | MEDIUM | MEDIUM |
| J9 | **Setup Screen** (`components/SetupScreen.tsx`) | First-run onboarding: API key entry, model download, provider config, basic validation. | LOW | MEDIUM |
| J10 | **TOML Config Schema** (`core/config.py`) | `~/.ageex/config.toml` with sections: `[core]`, `[engine]`, `[security]`, `[memory]`, `[channels]`, `[telemetry]`. Hardware auto-detection for optimal engine selection. Preset system for common use cases. | MEDIUM | MEDIUM |
| J11 | **TOCTOU-safe file ops** (`tools/file_read.py`) | OpenJarvis's `is_sensitive_file()` blocks `.ssh/`, `.env`, `*.pem`, `*.key`, cloud configs. AGEEX already has `is_path_protected` + O_NOFOLLOW. Enhance with additional sensitive patterns. | LOW | LOW |
| J12 | **Model Context Protocol (MCP)** (`tools/mcp_adapter.py`) | MCP client support gated behind AGX2 capabilities. Wrap external MCP tools behind ToolRegistry interface. Capability-checked before execution. | MEDIUM | MEDIUM |

---

## 3. Critical Gap Resolution (V1 + V2)

### V1: DEL Bypassed by Orchestrator → RESOLVED ✅

**Root cause:** `orchestrator.rs` WorkerAgent directly called `fs::read_to_string()`, `fs::write()`, `Command::new()`, `open` command, AppleScript without routing through `DelExecutor`.

**Resolution (committed):**
1. Removed all 12 DEL fallbacks across 8 action handlers in `orchestrator.rs`
2. `WorkerAgent::new()` and `with_consent_bridge()` now return `Result<Self>` — fail-fast on DEL init failure
3. `del` field changed from `Option<DelExecutor>` to plain `DelExecutor` (always present)
4. Added `execute_del()` helper method on `WorkerAgent` that wraps `DelExecutor.execute()`
5. All action handlers (`read_file`, `write_file`, `process_spawn`, `app_open`, `url_open`, `applescript`, `net_curl`, `skill_execute`, `text_injection`) route through `execute_del()` → Gate1 → Gate2 → Gate3
6. On DEL failure: return `TaskStatus::Failed { exit_code, stderr }` — never fall back to direct OS operations
7. `ToolExecutor::fs_delete` now routes through DEL (`execute_del("fs_delete", args)`) instead of `std::fs::rename()` directly
8. `with_ui_bridge` clones `DelExecutor` from old worker
9. Added `#[derive(Clone)]` to `DelExecutor`, `ExecutionJournal`, `Gate2Policy`
10. Gate2 allowlist expanded to include `~/.moatageex/workspace` alongside `~/Desktop/ageex-workspace`
11. Gate2 network gating extended to cover `net_curl`; process spawn gating covers `app_open`, `applescript`, `net_curl`
12. 167 tests pass. No non-DEL OS operations remain in any production code path.

### V2: AGX3 Isolation Missing → Wave 8

**Root cause:** No AGX3 agent struct. External sources route through `process_with_source()` which triggers full agent execution.

**Fix plan (from hermes-agent gateway model + AGEEX design docs):**
1. Create `AgentId::Agent3` with `CapabilitySet::agx3_default()` — an EMPTY capability set
2. AGX3 capabilities: `SessionRead` (read channel history), `SessionReply` (respond in-channel), `MemoryRead` (read shared context), `MemoryWrite` (write summaries)
3. AGX3 explicitly CANNOT: file operations, process spawn, app launch, URL open, skill execute, agent spawn
4. Route all non-CLI sources through AGX3 exclusively at `process_with_source()`
5. AGX3 can hand off filtered context to Memory Manager, which can trigger AGX1/AGX2 on user's behalf from the UI
6. Add platform hints injection for correct response formatting per channel (hermes-agent H2)
7. Add prompt injection scanning for external messages (hermes-agent context threat patterns)

---

## 4. Final System Architecture Blueprint

```
┌─────────────────────────────────────────────────────────────────────┐
│                        AGEEX SYSTEM PLANE                           │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐              │
│  │    AGX1      │  │    AGX2      │  │    AGX3      │              │
│  │  Read Agent  │  │ Write Agent  │  │  Connector   │              │
│  │              │  │              │  │   Agent      │              │
│  │ Capabilities:│  │ Capabilities:│  │ Capabilities:│              │
│  │ FileRead     │  │ All AGX1 +   │  │ SessionRead  │              │
│  │ FileExists   │  │ FileWrite    │  │ SessionReply │              │
│  │ OsProcSpawn  │  │ FileDelete   │  │ MemoryRead   │              │
│  │ AppOpen      │  │ SkillExecute │  │ MemoryWrite  │              │
│  │ UrlOpen      │  │ AgentSpawn   │  │ (ZERO OS)    │              │
│  │              │  │              │  │              │              │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘              │
│         │                 │                 │                      │
│         └─────────┬───────┴─────────┬───────┘                      │
│                   │                 │                              │
│          ┌────────▼─────────────────▼────────┐                     │
│          │         MEMORY MANAGER            │                     │
│          │  - Per-agent scoped context       │                     │
│          │  - Redaction/filtering pipeline   │                     │
│          │  - Context compression            │                     │
│          │  - Cross-agent knowledge routing  │                     │
│          │  - Working memory discarded       │                     │
│          └────────┬─────────────────┬────────┘                     │
│                   │                 │                              │
│  ┌────────────────▼─────────────────▼────────────────┐            │
│  │                    METABRAIN                       │            │
│  │  - Permission graph (petgraph DiGraph)             │            │
│  │  - Hierarchical path/domain inheritance            │            │
│  │  - Node types: Path, Domain, App, UrlScheme, Tool  │            │
│  │  - PermissionFlags bitmap (9 flags)                │            │
│  │  - Session-aware grants + replay                   │            │
│  │  - Policy table (SQLite + SHA-256 hashes)          │            │
│  └────────────────┬─────────────────┬────────────────┘            │
│                   │                 │                              │
│  ┌────────────────▼─────────────────▼────────────────┐            │
│  │                  CONSENT BRIDGE                    │            │
│  │  - Three-way decisions (Approve/Deny/Permanent)    │            │
│  │  - Structured requests (SHA-256 hashes)            │            │
│  │  - Oneshot channel (emit → await → resolve)        │            │
│  │  - Permanent allowlist support                     │            │
│  │  - Auto-deny for subagent children                 │            │
│  │  - Chain journal integration                      │            │
│  └────────────────┬─────────────────┬────────────────┘            │
│                   │                 │                              │
│  ┌────────────────▼─────────────────▼────────────────┐            │
│  │         DETERMINISTIC EXECUTION LAYER (DEL)        │            │
│  │  ┌─────────┐  ┌──────────┐  ┌──────────────────┐  │            │
│  │  │ Gate 1  │→│  Gate 2  │→│     Gate 3        │  │            │
│  │  │ Schema  │  │  Policy  │  │     Sandbox       │  │            │
│  │  │Validate │  │ Evaluate │  │   (Seatbelt)      │  │            │
│  │  └─────────┘  └──────────┘  └──────────────────┘  │            │
│  └────────────────┬─────────────────┬────────────────┘            │
│                   │                 │                              │
│  ┌────────────────▼─────────────────▼────────────────┐            │
│  │                CHAIN JOURNAL                       │            │
│  │  - SHA-256 chained (prev_hash → entry_hash)        │            │
│  │  - O_APPEND only (no UPDATE/DELETE/TRUNCATE)       │            │
│  │  - Types: Allow(tool, args), Deny(tool, reason)    │            │
│  │  - verify_chain() tamper detection                 │            │
│  │  - ReplayEngine::replay_from()                     │            │
│  └────────────────────────────────────────────────────┘            │
│                                                                     │
│  ┌────────────────────────────────────────────────────┐            │
│  │              INFRASTRUCTURE LAYER                  │            │
│  │                                                    │            │
│  │  ┌──────────┐ ┌────────┐ ┌───────┐ ┌───────────┐  │            │
│  │  │ Registry │ │EventBus│ │Config │ │ Plugin Mgr │  │            │
│  │  │ (J2)     │ │ (J3)   │ │(J10)  │ │ (O2)       │  │            │
│  │  └──────────┘ └────────┘ └───────┘ └───────────┘  │            │
│  │                                                    │            │
│  │  ┌──────────┐ ┌────────┐ ┌───────┐ ┌───────────┐  │            │
│  │  │Scanners  │ │MCP Clnt│ │Cron   │ │ Webhooks   │  │            │
│  │  │ (J5)     │ │ (J12)  │ │(H7)   │ │ (O5)       │  │            │
│  │  └──────────┘ └────────┘ └───────┘ └───────────┘  │            │
│  └────────────────────────────────────────────────────┘            │
│                                                                     │
│  ┌────────────────────────────────────────────────────┐            │
│  │              STORAGE LAYER                         │            │
│  │                                                    │            │
│  │  ┌──────────┐ ┌────────┐ ┌───────┐ ┌───────────┐  │            │
│  │  │ Session  │ │Memory  │ │Skills │ │ Journal    │  │            │
│  │  │ SQLite   │ │ SQLite │ │Vault  │ │ (Chain)    │  │            │
│  │  └──────────┘ └────────┘ └───────┘ └───────────┘  │            │
│  │                                                    │            │
│  │  ┌──────────┐ ┌────────┐                           │            │
│  │  │ Audit DB │ │Config  │                           │            │
│  │  │ (J5)     │ │ TOML   │                           │            │
│  │  └──────────┘ └────────┘                           │            │
│  └────────────────────────────────────────────────────┘            │
│                                                                     │
│  ┌────────────────────────────────────────────────────┐            │
│  │              GATEWAY / CHANNEL LAYER               │            │
│  │                                                    │            │
│  │  ┌──────────┐ ┌────────┐ ┌───────┐ ┌───────────┐  │            │
│  │  │Telegram  │ │WhatsApp│ │Discord│ │  Slack     │  │            │
│  │  │ Adapter  │ │Adapter │ │Adapter│ │  Adapter   │  │            │
│  │  └────┬─────┘ └───┬────┘ └──┬────┘ └─────┬─────┘  │            │
│  │       │           │         │             │        │            │
│  │       └───────────┴────┬────┴─────────────┘        │            │
│  │                        │                           │            │
│  │              ┌─────────▼─────────┐                  │            │
│  │              │  Channel Lifecycle│                  │            │
│  │              │  Manager (O1)     │                  │            │
│  │              │  start/stop/health│                  │            │
│  │              └─────────┬─────────┘                  │            │
│  │                        │                           │            │
│  │              ┌─────────▼─────────┐                  │            │
│  │              │  Session Binding  │                  │            │
│  │              │  System (O4)      │                  │            │
│  │              │  channel→session  │                  │            │
│  │              └─────────┬─────────┘                  │            │
│  │                        │                           │            │
│  │              ┌─────────▼─────────┐                  │            │
│  │              │      AGX3         │                  │            │
│  │              │  Connector Agent  │                  │            │
│  │              │  (ZERO OS CAPS)   │                  │            │
│  │              └───────────────────┘                  │            │
│  └────────────────────────────────────────────────────┘            │
│                                                                     │
│  ┌────────────────────────────────────────────────────┐            │
│  │              FRONTEND LAYER                        │            │
│  │                                                    │            │
│  │  ┌──────────┐ ┌──────────┐ ┌───────────────────┐  │            │
│  │  │ Desktop  │ │ Spotlight│ │ Command Palette   │  │            │
│  │  │ Tauri App│ │ Overlay  │ │ (J7)              │  │            │
│  │  │          │ │ (J1)     │ │ Cmd+K             │  │            │
│  │  └──────────┘ └──────────┘ └───────────────────┘  │            │
│  │                                                    │            │
│  │  ┌──────────┐ ┌──────────┐ ┌───────────────────┐  │            │
│  │  │ Chat UI  │ │ Consent  │ │ Dashboard         │  │            │
│  │  │          │ │ Modal    │ │ (J8)              │  │            │
│  │  └──────────┘ └──────────┘ └───────────────────┘  │            │
│  │                                                    │            │
│  │  ┌──────────┐ ┌──────────┐ ┌───────────────────┐  │            │
│  │  │ Settings │ │ Channel  │ │ Setup Screen      │  │            │
│  │  │          │ │ Dashboard│ │ (J9)              │  │            │
│  │  └──────────┘ └──────────┘ └───────────────────┘  │            │
│  └────────────────────────────────────────────────────┘            │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 5. Implementation Waves (Priority Order)

### Wave 7 — DEL Integration ✅ [DONE]
| # | Task | Source | Complexity |
|---|------|--------|------------|
| 7.1 | Extract all OS operations from orchestrator.rs → execute_del() | hermes-agent H1 | HIGH |
| 7.2 | Route through DelExecutor (Gate1→2→3) | current DEL | MEDIUM |
| 7.3 | Integrate ChainJournal into ConsentBridge (consent decisions → chain) | current journal | LOW |
| 7.4 | Add journal verification to session resume | current replay | LOW |

**Completed:** All 12 DEL fallbacks removed. 8 action handlers route through `execute_del()`. `WorkerAgent::new()` fail-fast on DEL init. `fs_delete` through DEL. Gate2 expanded. 167 tests pass.

### Wave 8 — AGX3 Isolation ✅ [DONE]
| # | Task | Source | Complexity | Status |
|---|------|--------|------------|--------|
| 8.1 | AgentId::Agent3 + CapabilitySet::agx3_default() (EMPTY OS caps) | AGEEX design | LOW | DONE |
| 8.5 | Platform hints injection for AGX3 system prompt (5 platforms) | hermes-agent H2 | LOW | DONE |
| 8.6 | Prompt injection scanning for external messages (11 patterns) | hermes-agent H10 | MEDIUM | DONE |
| 8.7 | AGX3 cannot trigger AGX1/AGX2, cannot reach DEL (structural) | AGEEX design | MEDIUM | DONE |
| 8.2 | Channel adapter impls for Telegram, WhatsApp, Discord | hermes-agent H1 | HIGH | PENDING |
| 8.3 | Channel lifecycle manager (start/stop/health/restart) | openclaw O1 | HIGH | PENDING |
| 8.4 | Session binding system (channel→session mapping) | openclaw O4 | HIGH | PENDING |

**Completed:** Agent3 exists with zero tools/OS caps. Platform hints for 5 platforms. Injection scanning with 11 threat patterns. `must_route_through_agx3()` guard. Channel adapter stubs + lifecycle manager deferred to Wave 12.

### Wave 9 — Memory Manager & Security ✅ [DONE]
| # | Task | Source | Complexity | Status |
|---|------|--------|------------|--------|
| 9.2 | Redaction/filtering pipeline for secrets/PII | OpenJarvis J5 | MEDIUM | DONE |
| 9.3 | GuardrailsEngine composable wrapper (scan input + output) | OpenJarvis J4 | MEDIUM | DONE |
| 9.4 | Content scanner pipeline (SecretScanner, PIIScanner) | OpenJarvis J5 | MEDIUM | DONE |
| 9.5 | Audit logger (SQLite: security events, consent, violations) | OpenJarvis J5 | LOW | DONE |
| 9.6 | File write denylist (block .ssh/, .env, shell RC, credentials) | hermes-agent H11 | LOW | DONE |
| 9.7 | Credential scoping (strip API keys from subprocess env) | hermes-agent H9 | LOW | DONE |
| 9.1 | Dedicated MemoryManagerAgent struct | hermes-agent H4 | MEDIUM | PENDING |

**Completed:** Scanner trait + 3 scanners. GuardrailsEngine composable wrapper. AuditLogger SQLite. File write denylist (24 patterns). Credential env stripping (23 vars). 20 new tests. 194 total.

### Wave 10 — Infrastructure & Extensibility [HIGH]
| # | Task | Source | Complexity |
|---|------|--------|------------|
| 10.1 | Registry pattern (agents, tools, providers, memory backends) | OpenJarvis J2 | MEDIUM |
| 10.2 | EventBus pub/sub for cross-cutting concerns | OpenJarvis J3 | LOW |
| 10.3 | Provider abstraction layer (LLM providers as plugins) | openclaw O3 | HIGH |
| 10.4 | Plugin SDK expansion (add subpaths per plugin type) | openclaw O2 | MEDIUM |
| 10.5 | MCP client integration (gated behind AGX2 capabilities) | OpenJarvis J12 | MEDIUM |

### Wave 11 — Native Overlay & Frontend [HIGH]
| # | Task | Source | Complexity |
|---|------|--------|------------|
| 11.1 | NSPanel + WKWebView Spotlight overlay (non-activating, transparent) | OpenJarvis J1 | LOW |
| 11.2 | WKScriptMessageHandler bridge (JS↔Rust) | OpenJarvis J1 | LOW |
| 11.3 | Frontmost app restoration on overlay hide | OpenJarvis J1 | LOW |
| 11.4 | Command Palette (Cmd+K) for agent switching | OpenJarvis J7 | LOW |
| 11.5 | Dashboard page (Recharts: capabilities, consent, metrics) | OpenJarvis J8 | MEDIUM |
| 11.6 | Setup screen (API keys, model download, providers) | OpenJarvis J9 | LOW |

### Wave 12 — Automation & Deployment [MEDIUM]
| # | Task | Source | Complexity |
|---|------|--------|------------|
| 12.1 | Cron/scheduler system (SQLite jobs, background loop) | hermes-agent H7 | MEDIUM |
| 12.2 | Webhook system (authenticated inbound HTTP) | openclaw O5 | MEDIUM |
| 12.3 | TOML config schema with sections + presets | OpenJarvis J10 | MEDIUM |
| 12.4 | Docker deployment (Dockerfile, compose, health check) | openclaw O8 | MEDIUM |
| 12.5 | Skills Guard static analysis (injection/exfiltration patterns) | hermes-agent H10 | LOW-MEDIUM |
| 12.6 | SOUL.md personality override system | hermes-agent H8 | LOW |

### Wave 13 — Polish & Enterprise [LATER]
| # | Task | Source | Complexity |
|---|------|--------|------------|
| 13.1 | Multi-crate architecture (split orchestrator.rs) | OpenJarvis J6 | MEDIUM |
| 13.2 | Architecture docs with mermaid diagrams | OpenJarvis docs | LOW-MEDIUM |
| 13.3 | Multi-user/enterprise (per-user isolation, org-level knowledge) | AGEEX design | HIGH |
| 13.4 | Toolset composition for platform-specific enablement | hermes-agent H3 | MEDIUM |
| 13.5 | Security audit CLI (`ageex security audit`) | openclaw O6 | LOW |
| 13.6 | Permanent allowlist for consent | hermes-agent H6 | LOW |

---

## 6. Production Readiness Scorecard (Updated)

| Category | Before | After Waves 7-8 | After Waves 7-12 |
|----------|--------|-----------------|------------------|
| Core agent system (AGX1/AGX2) | 95% | 95% | 100% |
| AGX3 connector agent | 15% | 90% | 100% |
| Permission system (MetaBrain) | 100% | 100% | 100% |
| Consent system (ConsentBridge) | 95% | 100% | 100% |
| DEL enforcement | 100% ✓ | 100% | 100% |
| Security hardening | 90% | 95% | 100% |
| Chain journal | 90% | 100% | 100% |
| Context compression | 100% | 100% | 100% |
| Memory Manager Agent | 40% | 40% | 95% |
| Skills system | 100% | 100% | 100% |
| Tool protocol | 100% | 100% | 100% |
| Multi-platform gateway | 15% | 85% | 95% |
| Cron/webhook automation | 0% | 0% | 90% |
| Plugin/provider ecosystem | 30% | 30% | 80% |
| Frontend UX | 85% | 85% | 95% |
| Deployment | 10% | 10% | 85% |
| Documentation | 40% | 40% | 80% |
| **Overall** | **73%** | **82%** | **94%** |

---

## 7. Reference File Inventory (for implementation)

### hermes-agent key files:
```
_dev/references/hermes-agent/
├── gateway/platforms/base.py          (3716 lines — BasePlatformAdapter ABC)
├── gateway/run.py                      (16672 lines — gateway runner)
├── gateway/session.py                  (1392 lines — SessionSource dataclass)
├── tools/delegate_tool.py              (2767 lines — delegation system)
├── tools/approval.py                   (1369 lines — consent patterns)
├── tools/skills_guard.py              (932 lines — injection scanning)
├── tools/file_safety.py               (111 lines — file write denylist)
├── tools/env_passthrough.py           (145 lines — credential scoping)
├── agent/memory_manager.py            (555 lines — MemoryManager + provider ABC)
├── agent/prompt_builder.py            (1456 lines — identity, hints, SOUL.md)
├── agent/tool_guardrails.py           (455 lines — loop detection)
├── cron/jobs.py                        (1114 lines — job store)
├── cron/scheduler.py                   (1820 lines — scheduler loop)
├── SECURITY.md                         (331 lines — trust model)
└── plugins/memory/                     (8+ memory backends)
```

### openclaw key files:
```
_dev/references/openclaw/
├── src/gateway/server-channels.ts      (channel lifecycle manager)
├── src/channels/plugins/               (152 files — channel binding)
├── src/plugin-sdk/channel-contract.ts  (channel plugin SDK)
├── src/plugins/                        (471 files — plugin system)
├── src/cron/service.ts                 (scheduling engine)
├── src/cron/isolated-agent.ts          (isolated cron execution)
├── extensions/webhooks/                (webhook system)
├── extensions/AGENTS.md                (extension boundary rules)
├── SECURITY.md                         (378 lines)
├── docker-compose.yml
└── ui/src/ui/                          (Control UI frontend)
```

### OpenJarvis key files:
```
_dev/references/OpenJarvis/
├── frontend/src-tauri/src/lib.rs       (L1204-1500 — NSPanel overlay)
├── src/openjarvis/core/registry.py     (Registry pattern)
├── src/openjarvis/core/events.py       (EventBus pub/sub)
├── src/openjarvis/core/config.py       (TOML config + hardware detection)
├── src/openjarvis/security/guardrails.py (GuardrailsEngine wrapper)
├── src/openjarvis/security/scanner.py  (SecretScanner, PIIScanner)
├── src/openjarvis/security/audit.py    (AuditLogger SQLite)
├── src/openjarvis/security/file_policy.py (sensitive file blocking)
├── src/openjarvis/tools/_stubs.py      (BaseTool ABC, ToolExecutor)
├── src/openjarvis/tools/mcp_adapter.py (MCP integration)
├── src/openjarvis/agents/_stubs.py     (BaseAgent ABC, AgentRegistry)
├── rust/crates/                        (17 Rust crates)
├── frontend/src/pages/DashboardPage.tsx
├── frontend/src/components/CommandPalette.tsx
├── frontend/src/components/SetupScreen.tsx
├── deploy/docker/Dockerfile
├── docs/architecture/                  (11 architecture docs with mermaid)
└── mkdocs.yml
```
