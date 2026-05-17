# MoatAgeex — Comprehensive Architecture Document

## Table of Contents

1. [System Entry Points & Wiring](#1-system-entry-points--wiring)
2. [Agent System](#2-agent-system)
3. [LLM / Provider System](#3-llm--provider-system)
4. [Orchestrator](#4-orchestrator)
5. [DEL Execution Layer](#5-del-execution-layer)
6. [Capabilities System](#6-capabilities-system)
7. [Security System](#7-security-system)
8. [Consent System](#8-consent-system)
9. [MetaBrain Permission Graph](#9-metabrain-permission-graph)
10. [Swarm / Gateway](#10-swarm--gateway)
11. [Channel System](#11-channel-system)
12. [Memory System](#12-memory-system)
13. [Context System](#13-context-system)
14. [WASM Reasoning System](#14-wasm-reasoning-system)
15. [Error Taxonomy](#15-error-taxonomy)

---

## 1. System Entry Points & Wiring

There are **two** system entry points, representing different architectural eras that coexist:

### 1A. `lib.rs` → `System` (Agent1/Agent2 Agent System)

**File:** `moat_core/src/lib.rs:106-116`

```rust
pub struct System {
    agent_kind: AgentKind,    // Spotlight | MainUi
    router: Option<Router>,   // Routes queries to Agent1 or Agent2
    agent1: Agent1,           // Read-only "Spotlight" agent
    agent2: Option<Agent2>,   // Execution-capable agent (MainUi only)
    tool_executor_agent1: ToolExecutor,
    tool_executor_agent2: Option<ToolExecutor>,
    session_store: Arc<SessionStore>,
    profile_store: Arc<ProfileStore>,
    trust_store: Arc<TrustStore>,
}
```

**Agent Kind enum** (`lib.rs:118-122`):
- `Spotlight` — read-only mode, single Agent1, zero write capability
- `MainUi` — full agent pair (Agent1 + Agent2) with router

**Initialization** (`lib.rs:141-261`):
1. Opens SQLite session/profile stores at `~/.ageex/sessions.db`
2. Creates `Arc<Mutex<MetaBrain>>` (synchronous Mutex) and grants workspace permissions
3. Creates `LlmClient::from_env()` (reads `AGEEX_MODEL` and provider-specific API key env vars)
4. Creates Agent1 with `ToolExecutor::new_with_auto_deny`
5. In MainUi mode: creates Agent2, a second LlmClient for routing, and `Router::new`
6. On resume: replays stored approvals into MetaBrain

**Query flow** (`lib.rs:263-319`):
1. Router classifies query → `RouteTarget::Agent1` or `RouteTarget::Agent2`
2. Agent1 can respond directly or delegate to Agent2 via `AgentResponse::Delegation`
3. Spotlight mode: direct to Agent1, delegation results in a "please open main app" message

### 1B. `system.rs` → `MoatSystem` (Swarm Orchestrator System)

**File:** `moat_core/src/system.rs:600-620`

```rust
pub struct MoatSystem {
    pub meta_brain: Arc<AsyncRwLock<MetaBrain>>,  // tokio RwLock
    db: Arc<AsyncMutex<Connection>>,
    gateway: Arc<GatewayControlPlane>,
    gateway_server: Arc<GatewayServer>,
    gateway_transport: Arc<AsyncMutex<Option<GatewayTransportServer>>>,
    session_lanes: Arc<DashMap<Uuid, Arc<AsyncMutex<()>>>>,
    provider_factory: Arc<dyn ProviderFactory>,
    pub orchestrator: Arc<Orchestrator>,
    pub swarm_gateway: Arc<AsyncRwLock<Option<Arc<SwarmGateway>>>>,
    pub default_swarm_agent: Arc<AsyncRwLock<Option<String>>>,
    // ...
}
```

**Initialization** (`system.rs:741-915`):

1. Opens SQLite database, prepares all tables (knowledge memory, gateway, policies, skills, rules)
2. Loads or creates MetaBrain from `~/.ageex/metabrain.bin` (bincode-serialized)
3. Resolves swarm workspace root (current dir if USER.md + SOUL.md exist, else `~/.ageex/swarm/`)
4. Creates `ExternalProviderFactory` with `KeyringSecretStore`
5. Reads `USER.md` frontmatter for model overrides (`chat_model`, `planner_model`, `worker_model`)
6. Builds `PlannerAgent` + `WorkerAgent` (with optional `ConsentBridge`)
7. Builds `SwarmGateway` from the workspace (`USER.md` + `SOUL.md`)
8. Wires everything into `MoatSystem`

**execute_task flow** (`system.rs:1496-1733`):

```
User Query → process_with_source() → ensure_source_session() → append user msg
  → process_inner() → load gateway session context + FTS search hits
  → execute_authoritative_session_agent() → load swarm assistant agent
  → build prompt (memory + precomputed context) → planner_for_agent()
  → orchestrator.run_react() → collect SubTaskResults → persist_result_turn()
```

---

## 2. Agent System

**File:** `moat_core/src/agent.rs`

### AgentId Enum (`agent.rs:10-15`)

```rust
pub enum AgentId {
    Agent1,  // Spotlight / read-only
    Agent2,  // Main UI / executor
    Agent3,  // AGX3 / Connector Agent (external channels)
}
```

### AgentResponse Enum (`agent.rs:17-20`)

```rust
pub enum AgentResponse {
    Text(String),
    Delegation { task: String, reason: String },
}
```

Only Agent1 can produce `Delegation` responses.

### Agent Struct (`agent.rs:66-77`)

```rust
pub struct Agent {
    id: AgentId,
    pub capabilities: CapabilitySet,
    context: ContextStore,
    token_budget: TokenBudget,
    llm: LlmClient,
    session_id: String,
    session_store: Arc<SessionStore>,
    profile_store: Arc<ProfileStore>,
    loop_config: LoopConfig,
    loop_state: LoopState,
}
```

Type aliases: `pub type Agent1 = Agent;` `pub type Agent2 = Agent;`

### Agent Characteristics by ID

| Feature | Agent1 | Agent2 | AGX3 |
|---------|--------|--------|------|
| **Label** | `Agent1` | `Agent2` | `AGX3` |
| **Delegation** | Yes (`delegate_to_executor`) | No | No |
| **Tools** | `fs_read`, `fs_exists`, `app_open`, `url_open`, `delegate_to_executor` | `fs_read/write/exists`, `process_spawn`, `skill_execute`, `app_open`, `url_open` | Empty (no tools) |
| **Capabilities** | FileRead, FileExists, AppOpen, UrlOpen | All (FileRead/Write, ProcessSpawn, AgentSpawn, AppOpen, UrlOpen, SkillExecute) | SessionRead, SessionReply only |
| **Prompt** | `AgentConfig::load_agent1_prompt()` | `AgentConfig::load_agent2_prompt()` | `AgentConfig::load_agent3_prompt()` |
| **Role** | Read-only assistant | Full executor | External channel connector |

### Constructors

- `new_agent1(llm, session_store, profile_store, resume)` → `agent.rs:125-132`
- `new_agent2(llm, session_store, profile_store, resume)` → `agent.rs:135-142`
- `new_agent3(llm, session_store, profile_store, resume)` → `agent.rs:148-155`

All call `new_inner()` which:
1. Loads the agent-specific system prompt
2. Creates or resumes a session (via prefix: `agent1_*`, `agent2_*`, `agent3_*`)
3. Loads prior messages from the session store into `ContextStore`
4. Injects user profile (from `ProfileStore`) as a system addendum
5. Initializes `LoopConfig::default()` (12 max iterations, 16 max tool calls)

### LoopConfig (`agent.rs:22-37`)

```rust
pub struct LoopConfig {
    pub max_iterations: u32,        // default: 12
    pub max_tool_calls: u32,        // default: 16
    pub enable_progressive_disclosure: bool,  // default: true
}
```

### Interaction Loop (`agent.rs:376-498`)

```
process_query(query) → appends user message → profile extraction → context compression check
  → run_loop():
    loop {
      iteration += 1
      check iteration budget
      maybe_inject_tool_details (progressive disclosure on 2nd+ iteration)
      serialize context → Vec<LlmMessage>
      llm.chat(messages, tools) → ChatResponse
      if tool_calls in response:
        check for delegate_to_executor (Agent1 only) → early exit
        execute ALL tools, collect results (Phase 1)
        atomically append: assistant-with-tool-calls, then all tool results (Phase 2)
        continue loop
      else:
        return Text response
    }
```

### safe_execute_tool (`agent.rs:275-350`)

Tool execution with denial/failure handling:
- Detects `__MOAT_DENIED__` prefix → wraps as `[TOOL_DENIED]`
- Detects `[LOOP_STOP]` prefix → records for early exit after atomic append
- Tracks consecutive identical failures (tool+args hash) → after 3 consecutive failures returns `[LOOP_STOP]`
- Adds `[TASK_COMPLETE]` hint for `fs_write`/`write_file` tool results

### Progressive Disclosure (`agent.rs:352-374`)

On iteration 2+, if the user query mentions tools/commands/files or if a tool call has been made, injects extended tool schema details into context. Tagged to prevent re-injection.

### Context Compression (`agent.rs:521-546`)

- Computes token count via `TokenBudget::count_tokens()`
- If status is `NeedsCompression`: precompresses tool outputs, tiered compression
- Anti-thrash guard: if compression doesn't save ≥15% tokens, bails with error

---

## 3. LLM / Provider System

### Two LLM Client Systems

There are **two** LLM client systems that coexist:

#### 3A. `llm/client.rs` → `LlmClient` (Agent1/Agent2 path)

Used by the `lib.rs` `System` (Agent1/Agent2). Simple wrapper around `reqwest::Client`:

```rust
pub struct LlmClient {
    provider: LlmProvider,  // enum of supported backends
    model: String,
    api_key: String,
    http: Client,
}
```

- `from_env()` reads `AGEEX_MODEL` env var and provider-specific API key env var
- Retry logic: 3 retries with exponential backoff (500ms base)
- Retryable HTTP codes: 429, 502, 503, 504
- `chat(messages, tools)` → sends to `{base_url}/chat/completions`
- `complete_simple(prompt)` → minimal single-shot for LLM router (max 10 tokens)

#### 3B. `external_api.rs` → `ExternalProviderFactory` (Orchestrator path)

Used by `MoatSystem` (Swarm + Orchestrator). Implements `ProviderFactory` trait.

**File:** `moat_core/src/external_api.rs`

### ProviderFactory Trait (`external_api.rs:38-59`)

```rust
pub trait ProviderFactory: Send + Sync {
    fn provider_for(&self, api_provider: &str) -> MoatResult<Arc<dyn LlmProvider>>;
    fn routed_provider(&self, subsystem: &str, requested_model: &str) -> MoatResult<(Arc<dyn LlmProvider>, String)>;
    fn invalidate_cache(&self);
    fn has_stored_key(&self, api_provider: &str) -> MoatResult<bool>;
    fn set_provider_key(&self, api_provider: &str, api_key: &str) -> MoatResult<()>;
    fn remove_provider_key(&self, api_provider: &str) -> MoatResult<()>;
    fn set_default_provider(&self, provider: &str);
    fn get_default_provider(&self) -> String;
    fn provider_health(&self, api_provider: &str) -> MoatResult<ProviderHealth>;
}
```

### ExternalProviderFactory (`external_api.rs:105-110`)

```rust
pub struct ExternalProviderFactory {
    client: Client,
    config: ExternalApiConfig,
    cache: DashMap<String, Arc<dyn LlmProvider>>,  // provider cache
    secrets: Arc<dyn SecretStore>,
}
```

### API Key Resolution Chain

For each provider (NVIDIA / DeepSeek), keys are resolved in this priority order:

1. **USER.md frontmatter** (`nvidia_api_key` / `deepseek_api_key` fields) — returned immediately if found
2. **Environment variable** (`NVIDIA_API_KEY` / `DEEPSEEK_API_KEY`) — returned if present
3. **macOS Keychain** — iterates over configured provider profiles, checks keychain entries via `KeyringSecretStore`

The `SecretStore` trait (`external_api.rs:61-65`) abstracts key storage:

```rust
pub trait SecretStore: Send + Sync {
    fn get_secret(&self, account: &str) -> MoatResult<Option<String>>;
    fn set_secret(&self, account: &str, secret: &str) -> MoatResult<()>;
    fn delete_secret(&self, account: &str) -> MoatResult<()>;
}
```

`KeyringSecretStore` uses the `keyring` crate (macOS Keychain) with service name `"moatageex"`.

### Provider Implementations

#### NvidiaExternalClient (`external_api.rs:627-1011`)
- Endpoint: `https://integrate.api.nvidia.com/v1/chat/completions`
- Methods: `complete`, `complete_stream`, `chat_complete`, `check_health`
- `complete_stream`: SSE streaming with `data: [DONE]` termination
- `chat_complete`: Non-streaming with tool calls, 60s timeout
- `check_health`: Uses `meta/llama-3.1-8b-instruct` with single-token "ping"

#### NvidiaMultiClient (`external_api.rs:635-776`)
- Wraps multiple `NvidiaCredential` entries
- Sorts credentials by model_match_score (exact → contains → slug match)
- Falls through all credentials on failure; returns all errors if none succeed
- `check_health`: succeeds if ANY credential passes

#### DeepSeekExternalClient (`external_api.rs:1015-1380`)
- Endpoint: `https://api.deepseek.com/chat/completions`
- Same pattern as NVIDIA but with DeepSeek-specific URL and response parsing
- **Key difference**: Tool function names have `:` replaced with `__` before sending (DeepSeek doesn't support `:` in tool names), and reversed on response parsing
- Health check uses `deepseek-chat` model

#### DeepSeekMultiClient (`external_api.rs:1020-1131`)
- Same multi-credential fall-through pattern as `NvidiaMultiClient`
- Sorts by `deepseek_model_match_score`

### Model Matching (`external_api.rs:520-561`)

Both providers use scoring to select the best credential:
- 3 points: exact model match
- 2 points: mutual substring match  
- 1 point: slug match (replacing `/` and `:` with `-`)
- 0 points: no match

### DynamicProvider (`system.rs:64-93`)

Created in `MoatSystem::build_live_provider` — wraps `ProviderFactory` with a specific subsystem name for routing.

---

## 4. Orchestrator

**File:** `moat_core/src/orchestrator.rs` (6007 lines)

### LlmProvider Trait (`orchestrator.rs:432-486`)

```rust
pub trait LlmProvider: Send + Sync {
    fn chat_complete(..) → BoxFuture<LlmResponse>;
    fn complete(..) → BoxFuture<(String, u32)>;      // deprecated
    fn complete_stream(..) → BoxFuture<(String, u32)>; // deprecated
    fn check_health(..) → BoxFuture<()>;
}
```

### LLM Message Types (`orchestrator.rs:385-430`)

```rust
pub struct LlmMessage { role: String, content: Option<String>, tool_calls: Option<Vec<LlmToolCall>>, tool_call_id: Option<String> }
pub struct LlmTool { type_: String, function: LlmFunctionDef }
pub struct LlmFunctionDef { name: String, description: Option<String>, parameters: serde_json::Value }
pub struct LlmToolCall { id: String, type_: String, function: LlmFunctionCall }
pub struct LlmFunctionCall { name: String, arguments: String }
pub struct LlmResponse { content: Option<String>, tool_calls: Option<Vec<LlmToolCall>>, tokens: u32 }
```

### PlannerAgent (`orchestrator.rs:517-729`)

```rust
pub struct PlannerAgent {
    pub config: PlannerConfig,            // model, max_subtasks(15), temperature(0.0), max_tokens(4096)
    pub provider: Arc<dyn LlmProvider>,
    model_override: Arc<RwLock<String>>,   // runtime model switching
}
```

**Methods:**
- `generate_intent_raw(rolling_context, tx?)` → calls `complete_stream` with `REACT_SYSTEM_TEMPLATE`
- `generate_intent_tools(messages, tools)` → calls `chat_complete` with tool definitions
- `build_llm_tools()` → returns 17 tool definitions
- `parse_intent(raw)` → `serde_json::from_str` with fallback to `safe_retry_intent()`

### REACT_SYSTEM_TEMPLATE (`orchestrator.rs:492-515`)

Hard-coded system prompt governing planner behavior:
- max_turns=15, memory_searches=10, session_searches=6, token_budget=24000
- Output format: `<action>` blocks only, no preamble
- Fail-fast: never retry CAPABILITY DENIED or PERMISSION DENIED
- Exit: emit `done` on next turn after having answer
- Rule 8: never retry a failed concept more than twice
- Rule 9: never output "Session Summary" or re-cap history

### Tool Schemas (17 tools, `orchestrator.rs:609-699`)

| Tool | Description | Key Parameters |
|------|-------------|---------------|
| `fs:read` | Read file contents | `target` (path), `payload` |
| `fs:write` | Write file contents | `target`, `payload` |
| `app:launch` | Launch application | `target` (app name) |
| `process:spawn` | Spawn background process | `target` (binary), `payload` (string array) |
| `os:open_url` | Open URL | `target` (URL), `payload` |
| `os:applescript` | Run AppleScript | `payload` (script text) |
| `os:inject_text` | Inject text via clipboard+paste | `text_to_inject`, `expected_app_name` |
| `os:click` | Click UI element (AX) | `element_id` (number) |
| `os:type_text` | Type into UI element (AX) | `element_id`, `text` |
| `net:curl` | HTTP request via curl | `target` (URL), method, body, headers |
| `memory:search` | Search durable knowledge (FTS5) | `payload` (query) |
| `session:search` | Search previous session transcripts (FTS5) | `payload` (query) |
| `memory:learn` | Store durable knowledge | `payload` {keywords, skill_description} |
| `meta:query_brain` | Check permissions in MetaBrain | `payload` (keyword) |
| `system:spawn_worker` | Delegate to sub-agent | `target_agent_id`, `task`, `specialization` |
| `propose_system_skill` | Propose new synthesized skill | `skill_name`, `description`, `source_code`, `language`, `parameters` |
| `user:ask_choice` | Ask user for choice | `context`, `options` (array) |
| `done` | Finish task | `payload` (final answer) |

### ToolReversibility (`orchestrator.rs:119-150`)

Classifies tools for barge-in system:
- **Reversible**: `fs:read`, `memory:search`, `session:search`, `memory:learn`, `meta:query_brain`, `user:ask_choice`, `done`
- **Staged**: `fs:write` (writes to `.tmp` first)
- **Committed**: `app:launch`, `process:spawn`, `os:open_url`, `os:applescript`, `os:inject_text`, `os:click`, `os:type_text`, `net:curl`, `swarm:delegate`, `system:spawn_worker`, `propose_system_skill`

### WorkerAgent (`orchestrator.rs:1197-1203`)

```rust
pub struct WorkerAgent {
    pub config: WorkerConfig,
    pub meta_brain: Arc<AsyncRwLock<MetaBrain>>,
    pub db: Arc<AsyncMutex<Connection>>,
    pub consent_bridge: Option<Arc<ConsentBridge>>,
    pub ui_bridge: Option<Arc<dyn UiActionBridge>>,
    pub del: DelExecutor,
}
```

**execute_capability_task** (`orchestrator.rs:1748-1843`):

```rust
execute_capability_task(intent, root_node_id, session_id, agent_label, high_risk_approvals, execution_world_state)
```

1. Check `ExecutionWorldState` cache for previously denied capabilities → short-circuit
2. Validate via `TypedIntent::from_action_intent` (Aegis payload validation)
3. Handle `propose_system_skill` via consent bridge
4. Check if action matches a synthesized skill → execute if so
5. Dispatch to appropriate handler:

| Action | Handler | Permission Check |
|--------|---------|-----------------|
| `fs:read` | `action_fs_read` | MetaBrain path:read + consent fallback |
| `fs:write` | `action_fs_write` | MetaBrain path:write + consent fallback |
| `app:launch` | `action_app_launch` | MetaBrain app:launch + consent fallback |
| `process:spawn` | `action_process_spawn` | Sandbox profile + MetaBrain tool:exec |
| `os:open_url` | `action_os_open_url` | URL scheme permission + consent |
| `os:applescript` | `action_os_applescript` | HIGH RISK — requires action consent |
| `os:inject_text` | `action_os_inject_text` | HIGH RISK — requires action consent + active app match |
| `os:click` | `action_os_click` | HIGH RISK — UI bridge + tool permission |
| `os:type_text` | `action_os_type_text` | HIGH RISK — UI bridge + tool permission |
| `net:curl` | `action_net_curl` | Domain permission + consent |
| `meta:query_brain` | `action_meta_query_brain` | No permission (read-only) |
| `session:search` | `action_session_search` | No permission (read-only) |

### Orchestrator (`orchestrator.rs`)

Core struct for the ReAct execution loop. Contains:
- `planner: PlannerAgent` — LLM-based intent generation
- `worker: WorkerAgent` — capability task execution
- `meta_brain`, `db` — shared state
- `injector_config` — memory injection config
- `agent_spec` (optional) — current swarm agent spec
- `swarm_gateway` (optional) — for sub-agent delegation

**Key Methods:**

- `with_planner(planner)` — returns new Orchestrator with different planner
- `with_agent_spec(spec)` — attaches swarm agent spec for tool filtering
- `run_react(query, system_prompt, root_node_id, status_tx)` — main ReAct loop
- `run_react_scoped_mode(query, system_prompt, root_node_id, status_tx, depth, delegate_count)` — swarm delegation variant

### run_react Flow (`orchestrator.rs` ~line 3700-4890)

```
1. Build full prompt: system_prompt + enforced_budget_xml + memory_context + user query
2. Inject host_environment metadata
3. Optional: execution_world_state injection, agent_spec filtering
4. Main ReAct loop (max 15 turns):
   a. Check turn budget, token budget (trigger compaction if needed)
   b. Call planner.generate_intent_raw(rolling_context) → parse into ActionIntent
   c. Guard patterns: detect consecutive done/failed/duplicate turns
   d. Handle "done" → break
   e. Handle "swarm:delegate" / "system:spawn_worker":
      - SubAgentHandlingMode::YieldToSupervisor → yield with continuation
      - SubAgentHandlingMode::InlineDelegate → spawn swarm agent inline
   f. Handle "user:ask_choice" → pause on oneshot until user selects
   g. Handle "memory:search" → FTS5 query with budget counter
   h. Handle "session:search" → gateway message search with budget counter
   i. Handle "memory:learn" → store in knowledge_memory table
   j. For capability actions: worker.execute_capability_task()
   k. Push observation into context_trace (system message with INTENT|OBSERVATION format)
   l. LoopDetector analysis (hash matching to detect repeated failures)
   m. Return ReactOutcome::Completed(results) or Yielded(status)
```

### ExecutionWorldState (`system.rs:448-598`)

Structured shared execution state (replaces unstructured scratchpad):
- `denied_capabilities`: deduplicated capability+target denials (cached for short-circuit)
- `discovered_facts`: key-value facts from workers
- `completed_subtasks`: one-line summaries
- `live_artifacts`: paths produced by workers

Serialized to XML for prompt injection, capped at 4000 chars with oldest subtasks truncated first.

### Process Spawn & Sandbox (`orchestrator.rs:1010-1052`)

`action_process_spawn` creates a Seatbelt sandbox profile and executes via `sandbox-exec`:
- Allows: `/bin/sh`, `/usr/local`, `/opt/homebrew`, `.cargo`, `.rustup`
- Allows: file read/write in workspace, Unix domain sockets
- Denies: all network access
- Extra path rules derived from MetaBrain subgraph
- Blocked binaries: `bash`, `sh`, `zsh`, `python`, `python3`, `ruby`, `perl`, `node`

### UiActionBridge (`orchestrator.rs:104-115`)

Trait for visual-semantic driver (AX accessibility APIs):

```rust
pub trait UiActionBridge: Send + Sync {
    fn click(&self, element_id: usize) -> Result<String, String>;
    fn type_text(&self, element_id: usize, text: &str) -> Result<String, String>;
    fn refresh_ui_tree(&self) -> Result<String, String>;
}
```

---

## 5. DEL Execution Layer

**Directory:** `moat_core/src/execution/`

### Architecture

The DEL (Distributed Execution Layer) is a **three-gate pipeline** that validates and executes capability requests:

```
ExecutionRequest → Gate1 (Schema) → Gate2 (Policy) → Gate3 (Sandbox) → ExecutionResult
                        |                |                  |
                    [Journal]        [Journal]          [Journal]
```

All gates are journalled to the execution journal.

### DelExecutor (`execution/mod.rs:25-69`)

```rust
pub struct DelExecutor {
    policy: Gate2Policy,
    journal: ExecutionJournal,
}
```

`execute(req)`: Gate1.validate → Gate2.enforce → Gate3.execute → journal log

### ExecutionRequest (`execution/request.rs:5-14`)

```rust
pub struct ExecutionRequest {
    pub request_id: String,       // UUIDv4
    pub agent_id: String,         // "agent1" | "agent2" | "wasm_<hex>"
    pub tool_name: String,        // "fs_read", "process_spawn", etc.
    pub capability: Capability,   // typed capability enum
    pub args: serde_json::Value,  // JSON object of arguments
    pub working_dir: PathBuf,     // default: ~/Desktop/ageex-workspace
    pub timestamp: i64,
}
```

### ExecutionResult (`execution/result.rs:3-14`)

```rust
pub struct ExecutionResult {
    pub request_id: String,
    pub success: bool,
    pub gate1_passed: bool,
    pub gate2_passed: bool,
    pub gate3_passed: bool,
    pub output: String,
    pub exit_code: Option<i32>,
    pub denied_reason: Option<String>,
    pub execution_time_ms: u64,
}
```

Factory methods: `gate1_denied(id, reason)`, `gate2_denied(id, reason)`, `gate3_denied(id, reason)`, `success(id, output, exit_code, elapsed)`

### Gate1: Schema Validation & Secret Redaction (`execution/gate1_schema.rs`)

**Validation rules** per tool name:

| Tool | Required Fields |
|------|----------------|
| `read_file` / `fs_read` | `path` (string) |
| `write_file` / `fs_write` | `path` (string), `content` (string) |
| `process_spawn` | `command` (string) |
| `app_open` | `app_name` (string) |
| `url_open` | `url` (string) |
| `applescript` | `script` (string) |
| `net_curl` | `url` (string) |
| `inject_text` | `text` (string) |
| `fs_exists` / `fs_delete` | `path` (string) |
| `skill_execute` | `skill_name` or `command` (string) |

**Secret patterns redacted** (line 7-13):
- `sk-[a-zA-Z0-9]{48}` — OpenAI key
- `ghp_[a-zA-Z0-9]{36}` — GitHub PAT
- `nvapi-[a-zA-Z0-9]+` — NVIDIA API key
- `password[...]=...` — passwords
- `token[...]=...` — tokens

Redaction is recursive through all JSON values (strings, objects, arrays).

### Gate2: Policy Enforcement (`execution/gate2_policy.rs`)

`ExecutionPolicy` defaults (line 19-41):
- `allow_network: true`
- `allow_process_spawn: true`
- `max_file_write_size_mb: 10`
- `blocked_commands: ["rm -rf /", "sudo", "mkfs", "dd if="]`
- `allowed_paths: ["~/Desktop/ageex-workspace", "~/.moatageex/workspace"]`
- `require_approval_for: ["ProcessSpawn", "FileWrite"]`

**Checks performed:**
1. Command prefix/Token matching against `blocked_commands`
2. Path allowlisting: expands `~`, normalizes `/workspace/` → `~/Desktop/ageex-workspace/`, checks `starts_with` against allowed paths
3. File size limit (10MB default)
4. Network disable policy check
5. Process spawn disable policy check

Supports TOML policy files via `load_from_file(path)`.

### Gate3: Sandbox Execution (`execution/gate3_sandbox.rs`)

**13 tool implementations:**

| Tool | Implementation |
|------|---------------|
| `read_file` / `fs_read` | `OpenOptions::read()` + `O_NOFOLLOW` (symlink protection) |
| `write_file` / `fs_write` | `OpenOptions::write().create().truncate()` + `O_NOFOLLOW`, auto-creates parent dirs |
| `process_spawn` | `sandbox-exec -f <seatbelt_profile> /bin/sh -c <command>` |
| `app_open` | `open -a <app_name>` |
| `url_open` | `open <url>` |
| `applescript` | writes script to ageex tmp dir → `osascript <path>` → 0o400 permission → cleanup |
| `net_curl` | `curl --silent --fail-with-body --max-time <t> --max-filesize <b> --max-redirs 5 -X <method> [headers] [data] <url>` |
| `inject_text` | clipboard backup → set text → Cmd+V via enigo → restore clipboard |
| `fs_exists` | `Path::exists()` |
| `fs_delete` | `rename` to ageex trash dir (soft delete) |

**Seatbelt profile** (line 282-313): Generated dynamically per execution:
- Allows process-exec: `/bin/sh`, `/usr/local/**`, `/opt/homebrew/**`, `.cargo/**`
- File read/write: workspace directory only
- File read: `/usr/local/bin`, `.cargo/**`, `.rustup/**`
- Network: denied entirely
- Socket: allow AF_UNIX only

### Spawn Context (`execution/spawn.rs`)

```rust
pub struct SpawnContext {
    pub capability_mask: CapabilitySet,
    pub remaining_fuel: u64,
    pub spawn_depth: u32,          // MAX_SPAWN_DEPTH = 4
}
```

`AgentPool::spawn_agent()` enforces:
1. **Capability subset rule**: child cannot exceed parent's capabilities
2. **Fuel carving**: atomic deduction from parent
3. **Depth cap**: MAX_SPAWN_DEPTH = 4

---

## 6. Capabilities System

**File:** `moat_core/src/capabilities.rs`

### Capability Enum (line 4-21)

```rust
pub enum Capability {
    FileRead,         // → "fs:read"
    FileWrite,        // → "fs:write"
    FileDelete,       // → "fs:delete"
    FileExists,       // → "fs:exists"
    OsProcessSpawn,   // → "os:process_spawn" — sandbox-exec + Seatbelt
    AgentSpawn,       // → "agent:spawn" — wasmtime instance pool
    AppOpen,          // → "app:open"
    UrlOpen,          // → "url:open"
    SkillExecute,     // → "skill:execute"
    SessionRead,      // → "session:read" (AGX3-only)
    SessionReply,     // → "session:reply" (AGX3-only)
}
```

### CapabilitySet (line 42-89)

```rust
pub struct CapabilitySet { capabilities: Vec<Capability> }
```

**Default sets:**

| Agent | Capabilities |
|-------|-------------|
| **Agent1** (agent1_default) | FileRead, FileExists, AppOpen, UrlOpen |
| **Agent2** (agent2_default) | FileRead, FileWrite, FileExists, OsProcessSpawn, AgentSpawn, AppOpen, UrlOpen, SkillExecute |
| **AGX3** (agx3_default) | SessionRead, SessionReply only |

`CapabilitySet::contains_all(other)` — used for spawn capability subset checking.

---

## 7. Security System

**Directory:** `moat_core/src/security/`

### 7A. Guardrails Engine (`security/guardrails.rs`)

```rust
pub struct GuardrailsEngine {
    scanners: Vec<Box<dyn Scanner>>,
}
```

Composable pipeline: `guard_input(text)` and `guard_output(text)` both run through all scanners. If any scanner finds a `Critical` threat, input is blocked.

**GuardResult:** `{ sanitized, findings, blocked }`

Used by `MemoryManagerAgent` for session summarization and scoped context building.

### 7B. Scanners (`security/scanners.rs`)

#### Scanner Trait (line 27-31)

```rust
pub trait Scanner: Send + Sync {
    fn name(&self) -> &'static str;
    fn scan(&self, text: &str) -> Vec<ScanFinding>;
    fn redact(&self, text: &str, findings: &[ScanFinding]) -> String;
}
```

#### SecretScanner (line 34-96)
Detects 12 credential patterns: OpenAI keys, GitHub tokens (all types), Slack bot tokens, AWS access keys, Google API keys, Anthropic keys, NVIDIA NGC keys, Bearer tokens.

#### PiiScanner (line 99-150)
Detects: email addresses, phone numbers, credit card numbers, SSNs.

#### InjectionScanner (line 153-204)
Detects prompt injection patterns:
- `<|SYSTEM_START|>`, `<|SYSTEM_END|>` (system delimiters)
- `<system>`, `</system>` (XML tags)
- `ignore previous instructions`, `disregard all rules`
- `you are now`, `new system prompt`
- `i'm your developer` (authority spoofing)

### 7C. Channel Threat Scanning (`channel/mod.rs:110-119`)

Additional patterns scanned on external channel messages:
- `as an AI language model, you should` (role manipulation)
- `sudo` (command injection)
- `rm -rf` (destructive command)

### 7D. Audit Logger (`security/audit.rs`)

`AuditLogger` records all execution events for post-mortem analysis.

### 7E. Denylist (`security/denylist.rs`)

Persistent deny list of paths/commands that have been permanently denied.

---

## 8. Consent System

**File:** `moat_core/src/consent.rs`

### ConsentBridge (line 68-74)

```rust
pub struct ConsentBridge {
    pending: Mutex<HashMap<String, oneshot::Sender<ConsentDecision>>>,
    choice_pending: Mutex<HashMap<String, oneshot::Sender<String>>>,
    emit: EmitFn,                          // callback to emit ConsentRequest
    chain_journal: Mutex<Option<ChainJournal>>,
}
```

**Architecture:** Async pause/resume via oneshot channels:
1. Agent invokes `request_consent(action, payload)` or `request_action_consent(intent, agent_label, session_id)`
2. ConsentBridge creates a `ConsentRequest` (with SHA-256 hashes of summary and payload), inserts a oneshot sender into `pending` HashMap
3. Calls `emit(request)` — async callback that surfaces the request to the UI
4. Await on oneshot receiver — **truly suspends the calling async task**
5. UI calls `resolve(request_id, decision)` → sends on oneshot → wakes the agent

### ConsentDecision Enum (line 18-22)

```rust
pub enum ConsentDecision {
    ApproveOnce,             // approved for this single use
    Deny,                     // denied
    CreatePermanentPolicy,   // approved + create perm policy in MetaBrain
}
```

### Request Types

| Method | Use Case |
|--------|----------|
| `request_consent(action, payload)` | Generic permission request |
| `request_action_consent(intent, agent_label, session_id)` | ReAct action tool consent (high risk) |
| `request_user_choice(request_id)` | `user:ask_choice` — pauses for user selection |
| `request_rule_review(session_id, review)` | Rule learning proposal |
| `request_sub_spawn_policy(session_id, source, target, tool)` | Sub-agent spawn permission |
| `request_skill_review(session_id, agent_label, proposal)` | Synthesized skill review |

### ConsentRequest (`consent.rs:50-64`)

```rust
pub struct ConsentRequest {
    pub request_id: String,
    pub session_id: String,
    pub action: String,
    pub summary: String,           // human-readable description
    pub payload: String,           // base64-encoded execution payload
    pub summary_hash: String,      // SHA-256
    pub payload_hash: String,      // SHA-256 (must differ from summary_hash)
    pub rule_review: Option<RuleConflictReview>,
    pub sub_spawn_policy: Option<SubSpawnPolicyRequest>,
    pub skill_review: Option<SkillReviewRequest>,
}
```

### Human Summary Builder (`consent.rs:410-472`)

Formats tool-specific human-readable summaries:
- FileRead: "Agent 'X' requests to read: /path"
- FileWrite: "Agent 'X' requests to write N bytes to path: /path"
- ProcessSpawn: "Agent 'X' requests OS shell execution: `exec args`"
- OpenUrl: "Agent 'X' requests to open URL: https://..."
- AppleScript: "Agent 'X' requests invocation of tool: 'os:applescript'"
- InjectText: "Agent 'X' requests to inject N bytes into the active application 'app'"
- Curl: "Agent 'X' requests network call: GET https://..."
- ProposeSkill: "Agent 'X' proposes installing synthesized skill 'name' (lang)"

### Chain Journal (`consent.rs:181-191`, `consent.rs:330-351`)

Every consent decision is also appended to `~/.ageex/chain.journal` as an immutable audit log with `Allow`/`Deny` operations.

---

## 9. MetaBrain Permission Graph

**File:** `moat_core/src/meta_brain.rs` (1375 lines)

### Core Data Structures

```rust
pub struct MetaBrain {
    inner: MetaBrainInner,
    flush: Option<FlushState>,        // async persistence to .bin file
}

struct MetaBrainInner {
    graph: DiGraph<NodeWeight, EdgeWeight>,  // petgraph directed graph
    bloom: BloomFilter,                      // 4096-bit, 4 hash functions
    stable_id_map: HashMap<NodeId, NodeIndex>,
    key_map: HashMap<String, NodeId>,
    next_id: NodeId,
}
```

### Node Types (line 68-78)

```rust
pub enum NodeKind {
    Root,        // SYSTEM_ROOT_ID (0)
    Scope,       // 5 scope categories
    Path,        // file path permission
    Domain,      // network domain permission
    App,         // application launch permission
    UrlScheme,   // URL scheme permission
    Tool,        // tool execution permission
}
```

### NodeKeys

| Key Pattern | Purpose |
|------------|---------|
| `root:system` | Root node |
| `scope:path` | Path permission scope |
| `scope:domain` | Domain permission scope |
| `scope:app` | App permission scope |
| `scope:url_scheme` | URL scheme permission scope |
| `scope:tool` | Tool permission scope |
| `path:/users/x/...` | Specific path node (hierarchical) |
| `domain:github.com` | Specific domain node (reversed hierarchy: com → github.com → api.github.com) |
| `app:*` | Wildcard app permission |
| `url_scheme:https` | Specific URL scheme |
| `tool:os:applescript` | Specific tool permission |

### PermissionFlags (line 28-50)

```rust
pub struct PermissionFlags(pub u32);
// Bit flags:
const FILE_READ:    Self(1 << 0);
const FILE_WRITE:   Self(1 << 1);
const NET_OUTBOUND: Self(1 << 2);
const NET_INBOUND:  Self(1 << 3);
const EXEC:         Self(1 << 4);
const URL_OPEN:     Self(1 << 5);
const APP_LAUNCH:   Self(1 << 6);
const TOOL_EXEC:    Self(1 << 7);
const FILE_DELETE:  Self(1 << 8);
```

### Grant Methods

| Method | Description |
|--------|------------|
| `grant_path_permission(path, flags)` | Creates hierarchical path chain from root |
| `grant_domain_permission(domain, flags)` | Creates reversed domain hierarchy |
| `grant_app_permission(app_name, flags)` | Single node under app scope |
| `grant_url_scheme_permission(scheme, flags)` | Single node under url_scheme scope |
| `grant_tool_permission(tool, flags)` | Single node under tool scope |

### Check Methods

All check methods support hierarchical inheritance:
- `path_has_permission(path, flags)` — checks: `/tmp/project/src/main.rs` inherits from `/tmp/project`
- `domain_has_permission(domain, flags)` — checks: `api.github.com` inherits from `github.com`, also checks wildcard `domain:*`
- `app_has_permission(app_name, flags)` — also checks `app:*`
- `url_scheme_has_permission(scheme, flags)` — also checks `url_scheme:*`
- `tool_has_permission(tool, flags)` — also checks `tool:*`

### Subgraph Extraction

- `extract_subgraph(root_id, depth)` — BFS from root, collects all active permission nodes
- `focused_subgraph(root_id, query, depth, max_matches)` — query-scored subgraph extraction with ancestor inclusion
- `extract_relevant_subgraph(keyword)` — token-based search for `meta:query_brain` tool, renders Seatbelt-style output

### Persistence

- Serialized via `bincode` to `~/.ageex/metabrain.bin`
- Async flush loop: `spawn_flush_loop()` — wakes on `mark_dirty()` notify or 250ms fallback
- Atomic write: temp file → rename

### Policy Verification

```rust
fn verify_policy(conn, source_worker, target_worker, allowed_tool) -> bool;
fn create_policy(conn, source_worker, target_worker, allowed_tool) -> ExecutionPolicy;
```

Policy hashes computed via SHA-256 of `"relationship" + source + "::" + target + "::" + tool`.

---

## 10. Swarm / Gateway

**File:** `moat_core/src/swarm.rs`

### Workspace (line 56-60)

```rust
pub struct Workspace {
    pub root: PathBuf,          // workspace directory
    pub user: UserProfile,      // from USER.md
    pub agents: Vec<AgentSpec>, // from SOUL.md
}
```

Default templates include 4 agents: `assistant`, `router`, `core_os`, `coder`.

### AgentSpec (line 44-53)

```rust
pub struct AgentSpec {
    pub name: String,              // "assistant", "core_os", "coder", etc.
    pub role: String,              // natural language role description
    pub api_provider: String,      // "nvidia" or "deepseek"
    pub model_id: String,          // e.g., "meta/llama-3.1-405b-instruct"
    pub allowed_tools: Vec<String>, // tool allowlist
    pub instructions: String,      // full instruction text
    pub raw_markdown: String,      // original markdown section
}
```

### Built-in Capability Augmentation

File automatically augments tool lists for known agents:
- **coder** (`ensure_coder_capabilities`): adds fs:read, fs:write, process:spawn, propose_system_skill, meta:query_brain, memory:search, session:search, memory:learn, done, user:ask_choice
- **assistant** (`ensure_assistant_capabilities`): adds system:spawn_worker, memory:search, session:search, memory:learn, meta:query_brain, done, user:ask_choice
- **core_os** (`ensure_core_os_capabilities`): adds app:launch, os:open_url, session:search
- Any agent with `os:applescript` also gets `os:inject_text`

### SwarmGateway (line 113-123)

```rust
pub struct SwarmGateway {
    pub workspace: Arc<Workspace>,
    pub brain: Arc<AsyncRwLock<MetaBrain>>,
    pub sessions: Arc<AsyncMutex<HashMap<Uuid, ReactSession>>>,
    pub provider_factory: Arc<dyn ProviderFactory>,
    pub planner_defaults: PlannerConfig,
    pub injector_config: InjectorConfig,
}
```

**Key Methods:**
- `planner_for_agent(agent_name)` — creates `PlannerAgent` with agent-specific model
  - `assistant`/`router`: uses `user.planner_model` override
  - Other agents: uses `user.worker_model` override
- `resolve_specialization(specialization)` — fuzzy matches agent names/roles
- `open_session(agent_name)` / `close_session(session_id)` — session lifecycle
- `run_delegate(template, worker, target_agent_id, task, ...)` — spawns swarm agent inline
  - Builds prompt with memory context + precomputed context
  - Creates dedicated planner for the target agent
  - Calls `orchestrator.run_react_scoped()` with 45s timeout

### SwarmAgent (line 358-363)

```rust
pub struct SwarmAgent {
    pub workspace: Arc<Workspace>,
    pub spec: AgentSpec,
    pub brain: Arc<AsyncRwLock<MetaBrain>>,
    pub consent_bridge: Option<Arc<ConsentBridge>>,
}
```

`request_and_apply_consent(intent)`: Parses action → resolves permission flags → classifies payload as path or domain → grants in MetaBrain if approved.

### Routing

`SwarmGateway::run_delegate()` routes user queries to the right specialist:
- Resolves specialization hints (e.g., "code" → "coder", "open safari" → "core_os")
- Creates isolated `ReactSession` with per-agent model selection
- Inline delegation: the orchestrator spawns a sub-agent within the same ReAct loop with depth tracking (MAX_SWARM_DEPTH)

---

## 11. Channel System

**Directory:** `moat_core/src/channel/`

### Architecture

External messages flow through: `Channel Adapter` → `Threat Scan` → `Session Binding` → `AGX3 (Agent3)` → `Swarm Gateway`

### ChannelMessage (`adapters.rs:5-14`)

```rust
pub struct ChannelMessage {
    pub channel_id: String,
    pub user_id: String,
    pub user_name: String,
    pub text: String,
    pub chat_type: ChatType,          // DirectMessage | GroupChat | Channel | Thread
    pub reply_to: Option<String>,
    pub media_urls: Vec<String>,
}
```

### ChannelAdapter Trait (`adapters.rs:35-52`)

```rust
pub trait ChannelAdapter: Send + Sync {
    fn source(&self) -> SessionSource;
    fn name(&self) -> &'static str;
    fn max_chars(&self) -> usize;
    fn markdown_allowed(&self) -> bool;
    fn parse_message(&self, raw: &serde_json::Value) -> Option<ChannelMessage>;
    fn format_reply(&self, reply: &ChannelReply) -> String;
    fn health_check(&self) -> Result<(), String>;
}
```

### Implemented Adapters

| Adapter | Source | Max Chars | Markdown |
|---------|--------|-----------|----------|
| `TelegramAdapter` | Telegram | 4096 | Yes |
| `WhatsAppAdapter` | WhatsApp | 4096 | No (stripped) |
| `DiscordAdapter` | Discord | 2000 | Yes |

Each adapter parses platform-specific JSON into `ChannelMessage` and formats `ChannelReply` back to platform-specific JSON.

### SessionBinding (`binding.rs:21-25`)

```rust
pub struct SessionBinding {
    bindings: Mutex<HashMap<ChannelSessionKey, String>>,
    session_store: Arc<SessionStore>,
}
```

Maps `(source, channel_id, user_id)` → internal session ID. Provides session isolation:
- Different channels never share sessions
- Different users within a channel get isolated sessions
- Sessions persist across adapter restarts

### ChannelLifecycleManager (`lifecycle.rs:30-35`)

```rust
pub struct ChannelLifecycleManager {
    registry: Arc<Mutex<AdapterRegistry>>,
    states: Arc<Mutex<HashMap<SessionSource, AdapterStatus>>>,
    backoff_base: Duration,       // 5s
    max_restarts: u64,            // 10
}
```

State management for adapter lifecycle: Started, Running, Stopping, Stopped, Failed. Health checks with restart on failure (up to 10 restarts).

### Platform Hints (`mod.rs:9-66`)

`PlatformHint` struct provides per-source formatting rules injected into AGX3's system prompt:
- WhatsApp: no markdown, plain text only, 4096 max chars
- Slack: markdown allowed, no table syntax, 40000 max chars
- Discord: markdown allowed, 2000 max chars
- Telegram: markdown auto-converted, 4096 max chars
- Signal: no markdown, 64000 max chars

### External Message Routing (`mod.rs:123-125`)

```rust
pub fn must_route_through_agx3(source: &SessionSource) -> bool {
    !matches!(source, SessionSource::Cli | SessionSource::DesktopUi | SessionSource::CommandBar)
}
```

All external sources (WhatsApp, Slack, Discord, Telegram, Signal) route through AGX3. CLI/DesktopUI sources bypass the connector agent.

---

## 12. Memory System

**Files:** `moat_core/src/memory.rs`, `moat_core/src/memory_manager.rs`

### Knowledge Memory (`memory.rs`)

SQLite-based durable knowledge store with FTS5 indexing:

**KnowledgeKind enum (line 29-37):**
```rust
pub enum KnowledgeKind {
    Preference,       // User preferences
    Fact,             // General facts
    Skill,            // Learned skills "[LEARNED SKILL]"
    SessionSummary,   // Compaction summaries "[SESSION SUMMARY]"
    ActionOutcome,    // Action results
    CrashNote,        // Kernel errors "[KERNEL ERROR]"
}
```

**Table schema** (line 129-178): `knowledge_memory` + `knowledge_memory_fts` virtual table with insert/update/delete triggers.

**Key functions:**
- `store_knowledge(conn, kind, source, title, text, timestamp)` → ID format: `"{kind}::{unix_ts}::{v4_uuid}"`
- `delete_knowledge(conn, id)` / `clear_knowledge(conn)` / `list_knowledge(conn, limit)`
- `build_prompt_memory_block(conn, options)` → XML-formatted context block grouped by kind
- `compile_context_summary(query, db, options)` → FTS5 search + sandbox output collection
- `build_overview(conn, knowledge_limit)` → `MemoryOverview` with stats

**Prompt memory block format** (line 418-461):
```xml
<knowledge_memory>
  <preferences>
    - [source] title :: content
  </preferences>
  <facts>...</facts>
  <skills>...</skills>
  <session_summaries>...</session_summaries>
  <action_outcomes>...</action_outcomes>
  <crash_notes>...</crash_notes>
</knowledge_memory>
```

### MemoryManagerAgent (`memory_manager.rs`)

```rust
pub struct MemoryManagerAgent {
    session_store: Arc<SessionStore>,
    guardrails: GuardrailsEngine,   // SecretScanner + InjectionScanner
    window_size: usize,             // 50 messages
    max_summary_chars: usize,       // 2000 chars
}
```

**Operations:**
- `summarize_session(session_id)` — last 50 messages, guardrail-scanned, truncated
- `build_scoped_context(session_id, scope_tags, max_chars)` — filters messages by tags, redacts
- `context_injection(session_id, max_chars)` — `"[CONTEXT SUMMARY]\n{summary}"`

---

## 13. Context System

**Directory:** `moat_core/src/context/`

### Message (`context_store.rs:13-22`)

```rust
pub struct Message {
    pub role: Role,                    // System | User | Assistant | Tool
    pub content: String,
    pub timestamp: u64,
    pub tool_call_id: Option<String>,  // for tool result messages
    pub tool_calls: Option<Vec<ToolCall>>, // for assistant messages with pending calls
}
```

**Security invariants:**
- System messages are wrapped in `<|SYSTEM_START|>...<|SYSTEM_END|>` delimiters
- User and Tool messages have system delimiters stripped (to prevent prompt injection)
- Tool results also strip `<system>` and `</system>` tags

**Factory methods:** `system(content)`, `user(content)`, `assistant(content)`, `assistant_with_tool_calls(content, calls)`, `tool(content)`, `tool_with_id(content, id)`, `tool_result(call_id, content)`

### ContextStore (`context_store.rs:118-122`)

```rust
pub struct ContextStore {
    messages: Arc<Mutex<Vec<Message>>>,
    system_prompt: String,
    tags: Arc<Mutex<Vec<String>>>,
}
```

**PC-01 Protocol Invariant**: Tool messages must follow Assistant (with tool_calls) or another Tool message. Attempting to append a tool message after User returns an error.

**Method:**
- `append(message)` — validates PC-01 invariant, pushes message
- `get_all()` — returns cloned vec
- `replace_all(messages)` — full replacement (for compression)
- `prepend_system_addendum(addendum)` — appends to first system message if room (<8000 chars), otherwise inserts separate system-addendum message
- `mark_tag(tag)` / `has_tag(tag)` — context state tracking

### TokenBudget (`context/token_budget.rs`)

`TokenBudget::new_128k()` — 128k token budget. `update(text)` estimates tokens. `status()` returns `BudgetStatus::Ok | NeedsCompaction { current, max }`.

### ContextCompressor (`context/compressor.rs`)

Two-tier compression:
1. `precompress_tool_outputs(messages)` — compresses tool outputs >500 chars to summary
2. `compress_tiered(messages)` — keeps head (2 msgs) + tail (20%), compresses middle into a `SummaryTemplate` system message

Anti-thrash guard: `compression_saved_enough(original, compressed, min_ratio)` — must save ≥15%.

---

## 14. WASM Reasoning System

### Architecture

```
┌─────────────────────────────────────────────┐
│  WASM Agent (.wasm)                          │
│  ┌─────────────────────────────────────┐    │
│  │ SDK (host.rs)                        │    │
│  │ airgap_send_file_read()             │    │
│  │ airgap_send_process_spawn()         │    │
│  │ mem_request_search()                │    │
│  │ agent_yield_done()                  │    │
│  └──────────┬──────────────────────────┘    │
│             │ extern "C" (ageex namespace)   │
│             │ __ageex_airgap_send(op, ...)   │
│             │ __ageex_mem_request(...)       │
│             │ __ageex_consent_poll(...)      │
│             │ __ageex_agent_yield_done(...)  │
│             │ __ageex_agent_log(...)         │
└─────────────┼───────────────────────────────┘
              │
┌─────────────▼───────────────────────────────┐
│  ageex_reasoning (Host)                      │
│  ┌─────────────────────────────────────┐    │
│  │ ReasoningEngine + Linker            │    │
│  │ imports.rs: process_airgap_send_async│   │
│  │   semantic_validate()               │    │
│  │   AirgapEnvelope → mpsc channel     │    │
│  └──────────┬──────────────────────────┘    │
│             │                                │
│  ┌──────────▼──────────────────────────┐    │
│  │ Airgap Poller                       │    │
│  │ handle_airgap_request(req)          │    │
│  │   → DEL (Gate1→Gate2→Gate3)         │    │
│  │   ← AirgapResponse (oneshot)        │    │
│  └─────────────────────────────────────┘    │
└─────────────────────────────────────────────┘
```

### CapOp Enum (`ageex_wire/src/ops.rs:4-16`)

```rust
#[repr(u32)]
pub enum CapOp {
    FileRead = 0x01,
    FileWrite = 0x02,
    ProcessSpawn = 0x03,
    NetworkOutbound = 0x04,
    AppLaunch = 0x05,
    MemorySearch = 0x06,
    MemoryStore = 0x07,
    DelegateTask = 0x08,
    ConsentRequest = 0x09,
    YieldDone = 0x0A,
}
```

### Wire Protocol (`ageex_wire/src/codec/wire.rs`)

Binary encoding/decoding for each `CapOp`. Each request type has a dedicated encoder:
- `encode_file_read(buf, path)` → `<0x01><path_len:u32 LE><path_bytes>`
- `encode_file_write(buf, path, data)` → `<0x02><path_len:u32><path><data_len:u32><data>`
- `encode_process_spawn(buf, exec, args)` → `<0x03><exec_len:u32><exec><args_count:u32><arg_lens...><args...>`
- etc.

### ReasoningEngine (`ageex_reasoning/src/engine.rs:43-47`)

```rust
pub struct ReasoningEngine {
    engine: Engine,                         // wasmtime Engine
    linker: Linker<StoreData>,              // linked host imports
    tx: mpsc::UnboundedSender<AirgapBundle>, // airgap channel sender
}
```

**Configuration** (line 71-78):
- `async_support(true)` — WASM async support
- `consume_fuel(true)` — fuel metering (100M default budget)
- `cranelift_nan_canonicalization(true)` — deterministic NaN behavior
- `wasm_multi_memory(true)` — multi-memory proposal

### StoreData (`engine.rs:24-31`)

```rust
pub struct StoreData {
    pub agent_id: [u8; 32],                            // 32-byte agent fingerprint
    pub seq_counter: AtomicU64,                         // monotonic request counter
    pub request_tx: mpsc::UnboundedSender<AirgapBundle>, // airgap channel
    pub yield_flag: AtomicBool,                         // agent yielded
    pub yield_message: Mutex<Vec<u8>>,                  // yield payload
    pub violations: Mutex<Vec<ViolationRecord>>,        // security violations
}
```

### ReasoningInstance (`engine.rs:50-54`)

```rust
pub struct ReasoningInstance {
    store: Store<StoreData>,
    _instance: Instance,                  // compiled WASM instance
    step_func: TypedFunc<(), ()>,         // agent_step() export
}
```

`call_step(fuel?)` → invokes `agent_step()`, returns `StepOutcome` with:
- `agent_yielded: bool`
- `yield_message: Vec<u8>` — agent's output
- `total_requests_sent: u64` — capability requests during this step
- `fuel_used: u64`
- `violations: Vec<ViolationRecord>`

### Host Imports (`ageex_reasoning/src/imports.rs`)

Five host functions registered in the `"ageex"` namespace:

| Import | Signature | Description |
|--------|-----------|-------------|
| `__ageex_airgap_send` | `(op, req_ptr, req_len, resp_ptr, resp_cap) → i32` | Send capability request, await DEL response (async) |
| `__ageex_mem_request` | `(op, req_ptr, req_len, val_ptr, val_len, buf_ptr, buf_cap) → i32` | Memory search/store request (fire-and-forget) |
| `__ageex_consent_poll` | `(req_ptr, req_len, timeout_ms) → u32` | Request user consent |
| `__ageex_agent_yield_done` | `(result_ptr, result_len) → u32` | Agent yields with final result |
| `__ageex_agent_log` | `(level, msg_ptr, msg_len) → ()` | Structured logging (0=debug,1=info,2=warn,3=error) |

### Semantic Validation (`imports.rs:78-233`)

Every capability request from WASM is semantically validated BEFORE reaching DEL:

| Request Type | Validations |
|-------------|------------|
| FileRead | Non-empty path, ≤4096 bytes, no path traversal (`..` or `\0`) |
| FileWrite | Path same as above + data ≤16MB |
| ProcessSpawn | Non-empty executable ≤4096 bytes, no NUL, no traversal; args ≤64, each ≤4096 bytes |
| NetworkOutbound | HTTP method 0-3, non-empty host ≤256 bytes, port ≠ 0, path ≤4096 bytes, body ≤16MB |
| AppLaunch | Non-empty name ≤256 bytes, no NUL, no traversal |
| MemorySearch | Query ≤4096 bytes, max_results ≤100 |
| MemoryStore | Non-empty key ≤1024 bytes, value ≤1MB |
| DelegateTask | Agent ID ≤128 bytes, task ≤64KB |
| ConsentRequest | Context ≤16KB, options ≤16KB |
| YieldDone | Message ≤16KB |

### Airgap Bridge (`moat_core/src/airgap.rs`)

`handle_airgap_request(req: AirgapRequest) → AirgapResponse`: Routes capability requests through DEL:
- `CapOp::FileRead` → `DelExecutor.execute("fs_read", Capability::FileRead, {path})`
- `CapOp::FileWrite` → `DelExecutor.execute("fs_write", Capability::FileWrite, {path, content})`
- `CapOp::ProcessSpawn` → `DelExecutor.execute("process_spawn", Capability::OsProcessSpawn, {command})`
- `CapOp::AppLaunch` → `DelExecutor.execute("app_open", Capability::AppOpen, {app_name})`
- `CapOp::MemorySearch` → returns stub (placeholder)
- `CapOp::MemoryStore` → returns stub (placeholder)
- All others → `AirgapError::InvalidOp`

### WASM SDK (`wasm/agents/sdk/`)

**File:** `wasm/agents/sdk/src/abi.rs` — Raw extern "C" declarations for all 5 host imports.

**File:** `wasm/agents/sdk/src/host.rs` — Safe rust wrappers:
- `airgap_send(req, resp)` → generic send
- `airgap_send_file_read(path, resp)` → encodes wire format, calls airgap
- `airgap_send_file_write(path, data, resp)`
- `airgap_send_process_spawn(executable, args, resp)`
- `airgap_send_network_outbound(method, host, port, path, body, resp)`
- `airgap_send_app_launch(app_name, resp)`
- `mem_request_search(query, max_results, out)` / `mem_request_store(key, value, out)`
- `consent_poll(context, options, timeout_ms)`
- `agent_log_*` helpers (debug/info/warn/error)
- `agent_yield_done(message)`

### Example Agent Structure

A WASM agent must:
1. Export `agent_step() → ()`
2. Call `agent_yield_done(message)` to return results
3. Use capability requests via `airgap_send` (sync/async via the airgap channel)
4. Implement `#![no_std]` with `alloc`

---

## 15. Error Taxonomy

**File:** `moat_core/src/lib.rs:39-76`

```rust
pub enum MoatError {
    Graph(String),              // meta-brain graph errors
    Database(rusqlite::Error),   // SQLite errors
    Vsa(String),                 // VSA engine errors
    Orchestrator(String),        // orchestrator failures
    Sandbox(SandboxError),       // sandbox execution errors
    SecurityViolation(String),   // security guard failures
    InvariantViolation(String),  // internal invariant checks
    InvalidStateTransition(String),
    Serde(serde_json::Error),    // serialization errors
    Llm(String),                 // LLM API errors
    Injection(String),           // context injection errors
    Settings(String),            // configuration errors
}
```

**Additional error types:**
- `ExecutionError` (`execution/error.rs`): `CapabilityDenied`, `SymlinkDenied`
- `SpawnError` (`execution/spawn.rs`): `CapabilityEscalation`, `InsufficientFuel`, `DepthExceeded`
- `AirgapError` (`ageex_wire/src/error.rs`): `None`, `InvalidOp`, `MalformedPayload`, `BufferOverflow`, `PermissionDenied`, `MemoryOutOfBounds`, `HostError`
