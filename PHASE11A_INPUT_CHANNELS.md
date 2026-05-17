# AGEEX PHASE 11A — INPUT CHANNEL SEPARATION
## Spotlight Agent1 vs Tauri Agent2 (Isolated Sessions + Delegation)
**Date:** 2026-05-13

**Scope:** UI / process architecture only. No MCP, no new tools, no DEL changes.

---

## 1. Goals and Non-Goals

### 1.1 Goals

1. **Isolate Agent1 and Agent2 sessions**
   - Agent1 (Spotlight) has its own session store and profile tables.
   - Agent2 (Tauri UI) retains the existing session store and profiles.

2. **Define separate input channels**
   - Agent1: Spotlight-like overlay UI (separate binary / app), short-lived, single-question interactions.
   - Agent2: Existing Tauri + React app, persistent multi-turn sessions.

3. **Allow clean handoff from Agent1 → Agent2**
   - Agent1 can classify a task as "too big / requires execution" and hand it off to Agent2.
   - Handoff is explicit, observable, and does not mix history or state.

### 1.2 Non-Goals

- No MCP integration yet (that's Phase 11B).
- No DEL changes — Agent2 still uses DEL exactly as in Phase 10.
- No shared sessions — Agent1 and Agent2 remain logically separate.

---

## 2. Target Architecture (After Phase 11A)

```text
           ┌────────────────────┐
           │  Spotlight UI      │  (macOS overlay app)
           │  (agent1_client)   │
           └────────┬───────────┘
                    │
                    │ HTTP / IPC (localhost)
                    ▼
             ┌───────────────┐
             │ moat_core     │
             │ Agent1 API    │  (isolated sessions)
             └───────────────┘

           ┌────────────────────┐
           │  Tauri + React UI  │  (ageex_ui)
           │  (main window)     │
           └────────┬───────────┘
                    │
                    │ Tauri command → Rust
                    ▼
             ┌───────────────┐
             │ moat_core     │
             │ Agent2 API    │  (existing sessions + DEL)
             └───────────────┘
```

- Agent1 and Agent2 are **separate request paths** into the same `moat_core` binary.
- Each path maps to a different `System` instance configured with:
  - Different `session_store` table prefixes.
  - Different `profile_store` prefixes.
  - Different prompts (agent1.md vs agent2.md) — already in place.

---

## 3. Implementation Plan

### 3.1 Add Multi-Profile System Entrypoints

**File: `src/lib.rs`**

Add two constructors instead of one generic `System::new()`:

```rust
pub struct System {
    agent_kind: AgentKind,
    agent1: Agent1,
    agent2: Agent2,
    meta_brain: Arc<MetaBrain>,
    session_store: Arc<SessionStore>,
    profile_store: Arc<ProfileStore>,
    // ... other fields
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    Spotlight,   // Agent1-only
    MainUi,      // Agent2-only
}

impl System {
    pub fn new_spotlight() -> anyhow::Result<Self> {
        Self::new_with_kind(AgentKind::Spotlight)
    }

    pub fn new_main_ui() -> anyhow::Result<Self> {
        Self::new_with_kind(AgentKind::MainUi)
    }

    fn new_with_kind(kind: AgentKind) -> anyhow::Result<Self> {
        // 1. Initialize MetaBrain (shared across kinds)
        // 2. Initialize SessionStore and ProfileStore with different prefixes
        let (session_store, profile_store) = match kind {
            AgentKind::Spotlight => {
                SessionStore::open_with_prefix("spotlight_" )?;
                ProfileStore::open_with_prefix("spotlight_" )?;
            }
            AgentKind::MainUi => {
                SessionStore::open_with_prefix("main_" )?;
                ProfileStore::open_with_prefix("main_" )?;
            }
        };
        // 3. Load appropriate prompts and construct agents
        // 4. Return System { .. }
        todo!("Wire Agent1/Agent2 construction per kind")
    }
}
```

**File: `src/session/store.rs`**

Add prefix-aware open methods:

```rust
impl SessionStore {
    pub fn open_with_prefix(prefix: &str) -> anyhow::Result<Self> {
        let db_path = dirs::home_dir()
            .unwrap()
            .join(".ageex")
            .join("sessions.db");
        let mut conn = rusqlite::Connection::open(db_path)?;
        // Namespacing via table names: spotlight_sessions vs main_sessions
        Self::run_migrations_with_prefix(&mut conn, prefix)?;
        Ok(Self { conn, prefix: prefix.to_string() })
    }

    fn run_migrations_with_prefix(conn: &mut rusqlite::Connection, prefix: &str) -> rusqlite::Result<()> {
        let sessions_table = format!("{}sessions", prefix);
        let approvals_table = format!("{}approvals", prefix);
        // create tables with prefix in the name
        conn.execute(&format!(
            "CREATE TABLE IF NOT EXISTS {} (...)",
            sessions_table
        ), [])?;
        // same for approvals, messages, etc.
        Ok(())
    }
}
```

**File: `src/profile/store.rs`**

Mirror the same pattern with `open_with_prefix("spotlight_")` and `open_with_prefix("main_")`.

> Brutal but necessary: you **must** keep Agent1 and Agent2 state separate at the DB level, or Spotlight will inherit all of Agent2’s long, noisy history and profiles.

---

### 3.2 CLI and Tauri Entrypoints

**CLI (for development):**

**File: `src/main.rs`**

Change the existing interactive shell to allow choosing the mode:

```rust
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let kind = if args.iter().any(|a| a == "--spotlight") {
        AgentKind::Spotlight
    } else {
        AgentKind::MainUi
    };

    let mut system = match kind {
        AgentKind::Spotlight => System::new_spotlight()?,
        AgentKind::MainUi => System::new_main_ui()?,
    };

    run_shell(&mut system)
}
```

This lets you test Agent1-only vs Agent2-only paths from the terminal without Tauri:

```bash
cargo run -- --spotlight   # Agent1-only test
cargo run                  # Agent2-only test
```

**Tauri Backend:**

- Tauri’s Rust backend should construct `System::new_main_ui()` **only**, since it is the main app.
- Spotlight UI will be a separate binary that hits `System::new_spotlight()` via HTTP/IPC (see next section).

---

### 3.3 Spotlight UI Process and IPC

**New crate / binary:** `spotlight_client` (outside `moat_core`, in the workspace)

Responsibilities:
- Show a macOS-style overlay input box (SwiftUI / Tauri-lite / electron-lite; implementation detail for later).
- On submit:
  - Send `{ query }` to `moat_core` via HTTP or a simple Unix domain socket.
  - Receive `{ answer }` and render, then auto-dismiss.

For Phase 11A, you **do not** implement the full macOS integration. You define the IPC contract and provide a minimal CLI-based Spotlight simulation.

**IPC Contract (Phase 11A):**

- Endpoint: `POST http://127.0.0.1:8765/spotlight/query`
- Request:

```json
{ "query": "open debug.log" }
```

- Response:

```json
{ "answer": "The contents of debug.log are..." }
```

**File: `src/server/spotlight.rs` (new)**

```rust
use crate::{System, AgentKind};
use axum::{Router, routing::post, extract::Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Deserialize)]
struct SpotlightRequest {
    query: String,
}

#[derive(Serialize)]
struct SpotlightResponse {
    answer: String,
}

pub fn router() -> Router {
    let system = Arc::new(Mutex::new(System::new_spotlight().expect("spotlight system")));

    Router::new().route("/spotlight/query", post({
        let system = system.clone();
        move |Json(req): Json<SpotlightRequest>| {
            let system = system.clone();
            async move {
                let mut sys = system.lock().await;
                let resp = sys.process_user_query(&req.query).await;
                match resp {
                    Ok(r) => Json(SpotlightResponse { answer: r.to_string() }),
                    Err(e) => Json(SpotlightResponse { answer: format!("Error: {}", e) }),
                }
            }
        }
    }))
}
```

**File: `src/main.rs`**

Add a command-line mode to start the Spotlight server:

```rust
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--spotlight-server") {
        return run_spotlight_server();
    }
    // ... existing CLI shell
}

#[tokio::main]
async fn run_spotlight_server() -> anyhow::Result<()> {
    let app = server::spotlight::router();
    axum::Server::bind(&"127.0.0.1:8765".parse()?)
        .serve(app.into_make_service())
        .await?;
    Ok(())
}
```

This gives you a **clean separation**:
- Spotlight UI process (future) talks HTTP to `/spotlight/query`.
- That hits Agent1-only `System::new_spotlight()`.

---

### 3.4 Agent1 → Agent2 Task Handoff

Handoff is **logical**, not session-sharing.

**Prompt updates (agents/agent1.md):**

Add a section:

```markdown
## Delegation to Agent2 (Main UI)

You are NOT allowed to modify files or run commands.
When a user asks you to perform actions that require writing, deleting,
compiling, or running tools, you MUST:

1. Clearly say that this is an Agent2 task.
2. Summarize the task in one sentence.
3. Suggest that the user open the main MoatAgeex app for execution.

Example:
"This is a code refactor request that requires file edits. I'll summarize
what needs to be done; please open the main MoatAgeex app so Agent2 can
apply these changes. Summary: ..."
```

**Important:**
- In Phase 11A, **Agent1 does NOT auto-trigger Agent2**. It only explains and summarizes.
- True cross-process handoff (e.g., auto-opening Tauri and pushing a queued task) is a later, more complex feature.

---

## 4. Testing Plan (Phase 11A)

### 4.1 Automated

1. **Session prefixing tests**
   - Add tests to `session::store::tests` to verify:
     - `open_with_prefix("spotlight_")` and `open_with_prefix("main_")` create distinct tables.
     - Messages written via `spotlight_` do not appear in `main_` queries.

2. **Profile prefixing tests**
   - Similar tests for `ProfileStore` — preferences and traits scoped per prefix.

3. **System constructors**
   - Add tests to `system::tests`:
     - `System::new_spotlight()` initializes Agent1-only configuration.
     - `System::new_main_ui()` initializes Agent2-focused configuration.

### 4.2 Manual

1. **Spotlight vs Main UI isolation**
   - Run spotlight server:
     - `cargo run -- --spotlight-server`
   - In another shell, send:

```bash
curl -X POST http://127.0.0.1:8765/spotlight/query   -H 'Content-Type: application/json'   -d '{"query":"what files have we touched?"}'
```

   - Verify:
     - Response is from Agent1.
     - It does NOT mention Agent2’s long history.

2. **Main UI unchanged**
   - Run main CLI:
     - `cargo run`
   - Verify all previous behaviors still work (DEL, FileDelete, Hermes loop, approvals).

3. **Delegation messaging**
   - Through Spotlight endpoint, ask:
     - `"refactor main.rs to use a config struct"`
   - Verify Agent1 responds with:
     - Clear statement that this requires Agent2.
     - A one-sentence summary.

---

## 5. Exit Criteria for Phase 11A

Phase 11A is complete when:

- `System::new_spotlight()` and `System::new_main_ui()` both work and are covered by tests.
- Spotlight session/profile tables are isolated from main session/profile tables.
- The Spotlight HTTP endpoint (`/spotlight/query`) works and routes to Agent1-only.
- The main CLI / Tauri UI still work as before, unchanged in behavior.
- Agent1 prompt properly communicates delegation, without auto-triggering Agent2.

Only then do you proceed to Phase 11B (Agent2 MCP + skills integration) on top of this clean separation.
