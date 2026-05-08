# Agent Dashboard Sidebar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an "Agents" sidebar tab (peer to "Session Control Plane") with WASM agent lifecycle monitoring and fuel-constrained step controls via Tauri IPC.

**Architecture:** A new Zustand `reasoningStore` holds agent roster and audit log state. A `ReasoningPanel` sidebar component renders the dashboard. Five new Tauri commands bridge IPC to the `ageex_reasoning::ReasoningEngine` on `AppState`. A background task polls the airgap envelope channel and emits JSON events to the frontend.

**Tech Stack:** React 18, TypeScript, Zustand, Tauri 2, ageex_reasoning (wasmtime 25.x)

---

## File Structure

- Create: `ageex_ui/src/stores/reasoningStore.ts` — Zustand store for agent roster, audit log, WASM loading state
- Create: `ageex_ui/src/components/ReasoningPanel.tsx` — Visual dashboard panel
- Create: `ageex_ui/src-tauri/src/reasoning_bridge.rs` — Tauri command implementations + background envelope polling
- Modify: `ageex_ui/src/types/ipc.ts` — Add reasoning types (AgentStatus, AirgapEntry, ReasoningStatus)
- Modify: `ageex_ui/src/components/ChatInterface.tsx` — Add "Agents" tab as sidebar peer
- Modify: `ageex_ui/src/components/AppShell.tsx` — Wire up reasoning events, add WASM file picker trigger
- Modify: `ageex_ui/src-tauri/Cargo.toml` — Add ageex_reasoning dependency
- Modify: `ageex_ui/src-tauri/src/lib.rs` — Add `mod reasoning_bridge`, extend `AppState`, register 5 commands + event emitters

---

### Task 1: Type definitions (ipc.ts)

**Files:**
- Modify: `ageex_ui/src/types/ipc.ts`

- [ ] **Step 1: Add reasoning types to ipc.ts**

At the end of the file (after line 351), append:

```typescript
export const REASONING_ENVELOPE_EVENT = "reasoning-airgap-envelope";
export const REASONING_VIOLATION_EVENT = "reasoning-security-violation";
export const REASONING_YIELD_EVENT = "reasoning-agent-yield";

export type AgentState = "idle" | "running" | "yielded" | "trapped";

export interface AgentStatus {
  agentId: string;
  state: AgentState;
  fuelRemaining: number;
  fuelBudget: number;
  envelopesSent: number;
  yieldMessage: string;
}

export interface ReasoningStatus {
  agents: AgentStatus[];
  envelopeQueueSize: number;
  engineReady: boolean;
}

export interface StepOutcomeResponse {
  agentYielded: boolean;
  yieldMessage: string;
  totalRequestsSent: number;
  fuelUsed: number;
}

export interface AirgapEnvelopeEvent {
  agentId: string;
  seq: number;
  op: string;
  requestBytes: number[];
}

export interface SecurityViolationEvent {
  agentId: string;
  seq: number;
  op: string;
  violation: string;
}

export interface AgentYieldEvent {
  agentId: string;
  yieldMessage: string;
  envelopesSent: number;
  fuelUsed: number;
}
```

- [ ] **Step 2: Verify TypeScript compiles**

Run: `npx tsc --noEmit` from `ageex_ui/`
Expected: Zero errors from ipc.ts

---

### Task 2: Zustand reasoningStore

**Files:**
- Create: `ageex_ui/src/stores/reasoningStore.ts`

- [ ] **Step 1: Write reasoningStore.ts**

```typescript
import { create } from "zustand";
import type {
  AgentStatus,
  AirgapEnvelopeEvent,
  SecurityViolationEvent,
  AgentYieldEvent,
} from "../types/ipc";

export interface AuditEntry {
  id: string;
  timestamp: number;
  kind: "envelope" | "violation" | "yield" | "step";
  agentId: string;
  message: string;
}

interface ReasoningState {
  agents: AgentStatus[];
  engineReady: boolean;
  envelopeQueueSize: number;
  auditLog: AuditEntry[];
  isStepPending: boolean;
  setEngineReady: (ready: boolean) => void;
  upsertAgent: (agent: AgentStatus) => void;
  removeAgent: (agentId: string) => void;
  setEnvelopeQueueSize: (size: number) => void;
  pushAudit: (entry: AuditEntry) => void;
  setIsStepPending: (pending: boolean) => void;
  handleEnvelopeEvent: (event: AirgapEnvelopeEvent) => void;
  handleViolationEvent: (event: SecurityViolationEvent) => void;
  handleYieldEvent: (event: AgentYieldEvent) => void;
  clearAll: () => void;
}

const MAX_AUDIT = 500;

function makeAuditId(): string {
  return `audit:${Date.now()}:${Math.random().toString(36).slice(2, 8)}`;
}

export const useReasoningStore = create<ReasoningState>((set, get) => ({
  agents: [],
  engineReady: false,
  envelopeQueueSize: 0,
  auditLog: [],
  isStepPending: false,

  setEngineReady: (ready) => set({ engineReady: ready }),
  setIsStepPending: (pending) => set({ isStepPending: pending }),

  upsertAgent: (agent) =>
    set((state) => {
      const idx = state.agents.findIndex((a) => a.agentId === agent.agentId);
      if (idx === -1) {
        return { agents: [...state.agents, agent] };
      }
      const agents = state.agents.slice();
      agents[idx] = agent;
      return { agents };
    }),

  removeAgent: (agentId) =>
    set((state) => ({
      agents: state.agents.filter((a) => a.agentId !== agentId),
    })),

  setEnvelopeQueueSize: (size) => set({ envelopeQueueSize: size }),

  pushAudit: (entry) =>
    set((state) => {
      const log =
        state.auditLog.length >= MAX_AUDIT
          ? [...state.auditLog.slice(1), entry]
          : [...state.auditLog, entry];
      return { auditLog: log };
    }),

  handleEnvelopeEvent: (event) => {
    const entry: AuditEntry = {
      id: makeAuditId(),
      timestamp: Date.now(),
      kind: "envelope",
      agentId: event.agentId,
      message: `airgap_send seq=${event.seq} op=${event.op}`,
    };
    get().pushAudit(entry);
    get().upsertAgent({
      agentId: event.agentId,
      state: "running",
      fuelRemaining: 0,
      fuelBudget: 0,
      envelopesSent: event.seq + 1,
      yieldMessage: "",
    });
  },

  handleViolationEvent: (event) => {
    const entry: AuditEntry = {
      id: makeAuditId(),
      timestamp: Date.now(),
      kind: "violation",
      agentId: event.agentId,
      message: `SECURITY ${event.op}: ${event.violation}`,
    };
    get().pushAudit(entry);
  },

  handleYieldEvent: (event) => {
    const entry: AuditEntry = {
      id: makeAuditId(),
      timestamp: Date.now(),
      kind: "yield",
      agentId: event.agentId,
      message: event.yieldMessage
        ? `yielded: ${event.yieldMessage}`
        : "agent yielded",
    };
    get().pushAudit(entry);
    get().upsertAgent({
      agentId: event.agentId,
      state: "yielded",
      fuelRemaining: 0,
      fuelBudget: 0,
      envelopesSent: event.envelopesSent,
      yieldMessage: event.yieldMessage,
    });
  },

  clearAll: () =>
    set({
      agents: [],
      engineReady: false,
      envelopeQueueSize: 0,
      auditLog: [],
      isStepPending: false,
    }),
}));
```

- [ ] **Step 2: Verify TypeScript compiles**

Run: `npx tsc --noEmit` from `ageex_ui/`
Expected: Zero errors from reasoningStore.ts

---

### Task 3: ReasoningPanel component

**Files:**
- Create: `ageex_ui/src/components/ReasoningPanel.tsx`

- [ ] **Step 1: Write ReasoningPanel.tsx**

```typescript
import { useEffect, useRef } from "react";
import { useReasoningStore, type AuditEntry } from "../stores/reasoningStore";

const AGENT_STATE_COLOR: Record<string, string> = {
  idle: "#8b98af",
  running: "#7ee0a1",
  yielded: "#ffbe5c",
  trapped: "#ff6b6b",
};

const AGENT_STATE_GLOW: Record<string, string> = {
  idle: "0 0 0px rgba(139,152,175,0)",
  running: "0 0 8px rgba(126,224,161,0.4)",
  yielded: "0 0 8px rgba(255,190,92,0.4)",
  trapped: "0 0 8px rgba(255,107,107,0.4)",
};

function AuditLine({ entry }: { entry: AuditEntry }) {
  const color =
    entry.kind === "violation"
      ? "#ff6b6b"
      : entry.kind === "yield"
        ? "#ffbe5c"
        : entry.kind === "envelope"
          ? "#8fd8b5"
          : "#9dc0ff";

  return (
    <div
      style={{
        fontFamily: "monospace",
        fontSize: "0.72rem",
        color,
        padding: "4px 0",
        borderBottom: "1px solid rgba(28,36,50,0.6)",
        wordBreak: "break-all",
      }}
    >
      <span style={{ opacity: 0.5 }}>
        {new Date(entry.timestamp).toLocaleTimeString()}
      </span>{" "}
      {entry.message}
    </div>
  );
}

export default function ReasoningPanel() {
  const agents = useReasoningStore((s) => s.agents);
  const engineReady = useReasoningStore((s) => s.engineReady);
  const envelopeQueueSize = useReasoningStore((s) => s.envelopeQueueSize);
  const auditLog = useReasoningStore((s) => s.auditLog);
  const isStepPending = useReasoningStore((s) => s.isStepPending);
  const auditScrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = auditScrollRef.current;
    if (el) {
      el.scrollTop = el.scrollHeight;
    }
  }, [auditLog.length]);

  return (
    <aside
      style={{
        width: "320px",
        minWidth: "320px",
        borderLeft: "1px solid #1c2432",
        background: "#0d1117",
        display: "flex",
        flexDirection: "column",
        minHeight: 0,
        overflow: "hidden",
      }}
    >
      <div
        style={{
          padding: "16px",
          borderBottom: "1px solid #1c2432",
          color: "#d9e8ff",
          fontWeight: 700,
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
        }}
      >
        <span>Agent Sandbox</span>
        <span
          style={{
            fontSize: "0.68rem",
            color: engineReady ? "#7ee0a1" : "#8b98af",
            fontFamily: "monospace",
          }}
        >
          {engineReady ? "ENGINE READY" : "ENGINE OFFLINE"}
        </span>
      </div>

      <div
        style={{
          flex: 1,
          overflowY: "auto",
          padding: "12px",
          display: "flex",
          flexDirection: "column",
          gap: "10px",
          overscrollBehavior: "contain",
        }}
      >
        {/* ── Agent Roster ── */}
        <div
          style={{
            border: "1px solid #263144",
            borderRadius: "14px",
            background: "#101722",
            padding: "12px",
          }}
        >
          <div
            style={{
              color: "#d9e8ff",
              fontSize: "0.82rem",
              fontWeight: 700,
              marginBottom: "8px",
              display: "flex",
              justifyContent: "space-between",
            }}
          >
            <span>Agent Roster</span>
            <span style={{ color: "#8ea3c2", fontSize: "0.76rem" }}>
              {agents.length} agent{agents.length !== 1 ? "s" : ""}
            </span>
          </div>
          {agents.length === 0 ? (
            <div style={{ color: "#7c8aa0", fontSize: "0.82rem" }}>
              No agents instantiated. Upload a .wasm file to begin.
            </div>
          ) : (
            <div style={{ display: "flex", flexDirection: "column", gap: "8px" }}>
              {agents.map((agent) => (
                <div
                  key={agent.agentId}
                  style={{
                    border: "1px solid #1f2b3b",
                    borderRadius: "12px",
                    background: "#0d141d",
                    padding: "10px 12px",
                  }}
                >
                  <div
                    style={{
                      display: "flex",
                      justifyContent: "space-between",
                      alignItems: "center",
                      marginBottom: "6px",
                    }}
                  >
                    <span
                      style={{
                        color: "#e6eefb",
                        fontFamily: "monospace",
                        fontSize: "0.78rem",
                        fontWeight: 700,
                      }}
                    >
                      {agent.agentId.length > 16
                        ? `${agent.agentId.slice(0, 16)}...`
                        : agent.agentId}
                    </span>
                    <span
                      style={{
                        display: "inline-flex",
                        alignItems: "center",
                        gap: "5px",
                        fontSize: "0.7rem",
                        fontWeight: 700,
                        color: AGENT_STATE_COLOR[agent.state] ?? "#8b98af",
                        textTransform: "uppercase",
                      }}
                    >
                      <span
                        style={{
                          width: "7px",
                          height: "7px",
                          borderRadius: "999px",
                          background:
                            AGENT_STATE_COLOR[agent.state] ?? "#8b98af",
                          boxShadow:
                            AGENT_STATE_GLOW[agent.state] ??
                            "0 0 0px rgba(139,152,175,0)",
                        }}
                      />
                      {agent.state}
                    </span>
                  </div>

                  {/* Fuel gauge */}
                  {agent.fuelBudget > 0 && (
                    <div style={{ marginBottom: "4px" }}>
                      <div
                        style={{
                          color: "#8ea3c2",
                          fontSize: "0.68rem",
                          marginBottom: "3px",
                        }}
                      >
                        Fuel: {agent.fuelRemaining.toLocaleString()} /{" "}
                        {agent.fuelBudget.toLocaleString()}
                      </div>
                      <div
                        style={{
                          height: "4px",
                          borderRadius: "2px",
                          background: "#1f2b3b",
                          overflow: "hidden",
                        }}
                      >
                        <div
                          style={{
                            height: "100%",
                            width: `${Math.min(
                              100,
                              (agent.fuelRemaining / agent.fuelBudget) * 100,
                            )}%`,
                            background:
                              agent.fuelRemaining / agent.fuelBudget > 0.5
                                ? "linear-gradient(90deg, #2c8d5a, #4ad18b)"
                                : agent.fuelRemaining / agent.fuelBudget > 0.2
                                  ? "linear-gradient(90deg, #8d6a2c, #d1a84a)"
                                  : "linear-gradient(90deg, #8d2c2c, #d14a4a)",
                            borderRadius: "2px",
                            transition: "width 0.3s ease",
                          }}
                        />
                      </div>
                    </div>
                  )}

                  <div
                    style={{
                      display: "flex",
                      gap: "16px",
                      color: "#6f8099",
                      fontSize: "0.68rem",
                      fontFamily: "monospace",
                    }}
                  >
                    <span>envelopes: {agent.envelopesSent}</span>
                    {agent.yieldMessage && (
                      <span
                        style={{
                          color: "#ffbe5c",
                          maxWidth: "120px",
                          overflow: "hidden",
                          textOverflow: "ellipsis",
                          whiteSpace: "nowrap",
                        }}
                      >
                        msg: {agent.yieldMessage}
                      </span>
                    )}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>

        {/* ── Airgap Queue Summary ── */}
        <div
          style={{
            border: "1px solid #263144",
            borderRadius: "14px",
            background: "#101722",
            padding: "12px",
          }}
        >
          <div
            style={{
              color: "#d9e8ff",
              fontSize: "0.82rem",
              fontWeight: 700,
              marginBottom: "4px",
            }}
          >
            Airgap Queue
          </div>
          <div
            style={{
              color: envelopeQueueSize > 0 ? "#ffbe5c" : "#7c8aa0",
              fontFamily: "monospace",
              fontSize: "0.86rem",
            }}
          >
            {envelopeQueueSize} pending envelope
            {envelopeQueueSize !== 1 ? "s" : ""}
          </div>
        </div>

        {/* ── Audit Log ── */}
        <div
          style={{
            border: "1px solid #263144",
            borderRadius: "14px",
            background: "#101722",
            padding: "12px",
            flex: 1,
            minHeight: 0,
            display: "flex",
            flexDirection: "column",
          }}
        >
          <div
            style={{
              color: "#d9e8ff",
              fontSize: "0.82rem",
              fontWeight: 700,
              marginBottom: "8px",
              display: "flex",
              justifyContent: "space-between",
            }}
          >
            <span>Audit Log</span>
            <span style={{ color: "#6f8099", fontSize: "0.7rem" }}>
              {auditLog.length} entries
            </span>
          </div>
          <div
            ref={auditScrollRef}
            style={{
              flex: 1,
              overflowY: "auto",
              overscrollBehavior: "contain",
              minHeight: 0,
            }}
          >
            {auditLog.length === 0 ? (
              <div style={{ color: "#7c8aa0", fontSize: "0.78rem" }}>
                No events yet. Step an agent to see capability requests.
              </div>
            ) : (
              auditLog.map((entry) => (
                <AuditLine key={entry.id} entry={entry} />
              ))
            )}
          </div>
        </div>
      </div>
    </aside>
  );
}
```

- [ ] **Step 2: Verify TypeScript compiles**

Run: `npx tsc --noEmit` from `ageex_ui/`
Expected: Zero errors

---

### Task 4: ChatInterface — Add "Agents" sidebar tab

**Files:**
- Modify: `ageex_ui/src/components/ChatInterface.tsx`

- [ ] **Step 1: Import ReasoningPanel**

At line 7 (after `import { useChatStore }`), add:

```typescript
import ReasoningPanel from "./ReasoningPanel";
import { useReasoningStore } from "../stores/reasoningStore";
```

- [ ] **Step 2: Add `showAgents` prop and state to ChatInterface**

In `ChatInterfaceProps` interface (line 127-140), add after `onToggleMemory`:

```typescript
  showAgents: boolean;
  onToggleAgents: () => void;
```

- [ ] **Step 3: Destructure showAgents and onToggleAgents**

At line 177 (after `onSelectSession,`), add:

```typescript
  showAgents,
  onToggleAgents,
```

- [ ] **Step 4: Add "Agents" toggle button in header**

After the "Memory" button (line 452, after `{showMemory ? "Hide Memory" : "Memory"}`) and before the "Reset Memory" button (line 454), insert:

```tsx
          <button
            type="button"
            onClick={onToggleAgents}
            disabled={isBooting}
            style={{
              border: "1px solid #2f3b4f",
              background: showAgents ? "#172132" : "#101722",
              color: isBooting ? "#596579" : "#edf3ff",
              borderRadius: "999px",
              padding: "9px 14px",
              cursor: isBooting ? "not-allowed" : "pointer",
            }}
          >
            {showAgents ? "Hide Agents" : "Agents"}
          </button>
```

- [ ] **Step 5: Add "Agents" sidebar panel**

After the closing `)}` of the `{showMemory && (` sidebar block (line 1131), before the final closing tag, insert:

```tsx
        {showAgents && (
          <ReasoningPanel />
        )}
```

- [ ] **Step 6: Verify TypeScript compiles**

Run: `npx tsc --noEmit` from `ageex_ui/`
Expected: Zero errors

---

### Task 5: Rust — Add ageex_reasoning dependency

**Files:**
- Modify: `ageex_ui/src-tauri/Cargo.toml`

- [ ] **Step 1: Add dependency**

After line 35 (`moat_core = { path = "../../moat_core" }`), add:

```toml
ageex_reasoning = { path = "../../crates/ageex_reasoning" }
```

- [ ] **Step 2: Verify cargo check compiles**

Run: `cargo check` from `ageex_ui/src-tauri/`
Expected: Compiles successfully (may warn about unused `ageex_reasoning`)

---

### Task 6: Rust — Create reasoning_bridge.rs

**Files:**
- Create: `ageex_ui/src-tauri/src/reasoning_bridge.rs`

- [ ] **Step 1: Write reasoning_bridge.rs**

```rust
use ageex_reasoning::{AirgapEnvelope, ReasoningEngine, ReasoningInstance, StepOutcome};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatusPayload {
    pub agent_id: String,
    pub state: String,
    pub fuel_remaining: u64,
    pub fuel_budget: u64,
    pub envelopes_sent: u64,
    pub yield_message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningStatusPayload {
    pub agents: Vec<AgentStatusPayload>,
    pub envelope_queue_size: u64,
    pub engine_ready: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepOutcomePayload {
    pub agent_yielded: bool,
    pub yield_message: String,
    pub total_requests_sent: u64,
    pub fuel_used: u64,
}

pub struct ReasoningBridge {
    pub engine: Option<ReasoningEngine>,
    pub instances: HashMap<String, ReasoningInstance>,
    pub rx: Option<tokio::sync::mpsc::UnboundedReceiver<AirgapEnvelope>>,
    pub last_status: AgentStatusPayload,
}

impl ReasoningBridge {
    pub fn new() -> Self {
        Self {
            engine: None,
            instances: HashMap::new(),
            rx: None,
            last_status: AgentStatusPayload {
                agent_id: String::new(),
                state: "idle".into(),
                fuel_remaining: 0,
                fuel_budget: 0,
                envelopes_sent: 0,
                yield_message: String::new(),
            },
        }
    }

    pub async fn init_engine(&mut self) -> Result<(), String> {
        let (engine, rx) = ReasoningEngine::new()
            .map_err(|e| format!("Failed to create ReasoningEngine: {}", e))?;
        self.engine = Some(engine);
        self.rx = Some(rx);
        Ok(())
    }

    pub fn get_status(&self) -> ReasoningStatusPayload {
        let agents: Vec<AgentStatusPayload> = self
            .instances
            .keys()
            .map(|id| AgentStatusPayload {
                agent_id: id.clone(),
                state: "idle".into(),
                fuel_remaining: 0,
                fuel_budget: 0,
                envelopes_sent: 0,
                yield_message: String::new(),
            })
            .collect();
        ReasoningStatusPayload {
            agents,
            envelope_queue_size: 0,
            engine_ready: self.engine.is_some(),
        }
    }
}

pub fn start_envelope_poller(app: AppHandle, rx: tokio::sync::mpsc::UnboundedReceiver<AirgapEnvelope>) {
    tauri::async_runtime::spawn(async move {
        let mut rx = rx;
        while let Some(envelope) = rx.recv().await {
            let agent_id = hex::encode(envelope.agent_id);
            let _ = app.emit(
                "reasoning-airgap-envelope",
                serde_json::json!({
                    "agentId": agent_id,
                    "seq": envelope.seq,
                    "op": envelope.op.name(),
                    "requestBytes": envelope.request_bytes,
                }),
            );
        }
    });
}
```

- [ ] **Step 2: Verify cargo check**

Run: `cargo check` from `ageex_ui/src-tauri/`
Expected: Compiles (may need `hex` crate — if so, add `hex = "0.4"` to Cargo.toml)

---

### Task 7: Rust — Extend AppState, add Tauri commands, wire up

**Files:**
- Modify: `ageex_ui/src-tauri/src/lib.rs`

- [ ] **Step 1: Add mod declaration and imports**

After line 3 (`mod ui_scraper;`), add:

```rust
mod reasoning_bridge;
```

After line 25 (`use std::sync::atomic::{AtomicBool, Ordering};`), add:

```rust
use reasoning_bridge::{ReasoningBridge, StepOutcomePayload};
```

- [ ] **Step 2: Add reasoning_bridge to AppState**

In the `AppState` struct (line 168-179), add after the `gateway_info` field:

```rust
    reasoning: Arc<AsyncMutex<ReasoningBridge>>,
```

- [ ] **Step 3: Initialize reasoning bridge in setup**

In the run() setup closure, after `app.manage(AppState { ... });` (line 1333-1340), add the reasoning_bridge initialization right before the `app.manage` call:

At line 1333, before:

```rust
            app.manage(AppState {
```

Insert:

```rust
            let reasoning_bridge = Arc::new(AsyncMutex::new(ReasoningBridge::new()));
            let envelope_app_handle = app_handle.clone();
            // Attempt to init the engine asynchronously
            {
                let bridge = Arc::clone(&reasoning_bridge);
                tauri::async_runtime::spawn(async move {
                    let mut guard = bridge.lock().await;
                    match guard.init_engine().await {
                        Ok(()) => {
                            let _ = envelope_app_handle.emit(
                                "live-telemetry",
                                "[REASONING] Engine initialized. Ready for WASM agents.",
                            );
                        }
                        Err(e) => {
                            let _ = envelope_app_handle.emit(
                                "live-telemetry",
                                format!("[REASONING] Engine init failed: {}", e),
                            );
                        }
                    }
                });
            }
```

And update the AppState construction to include the field:

```rust
            app.manage(AppState {
                kernel: Arc::clone(&kernel),
                consent_bridge: Arc::clone(&consent_bridge),
                ui_element_map: Arc::clone(&ui_element_map),
                voice_session: Arc::new(AsyncMutex::new(None)),
                voice_status: Arc::clone(&voice_status),
                gateway_info: Arc::clone(&gateway_info),
                reasoning: Arc::clone(&reasoning_bridge),
            });
```

- [ ] **Step 4: Add Tauri commands**

After the `stop_voice_session` command (before `#[cfg_attr(mobile, tauri::mobile_entry_point)]`), add these commands:

```rust
#[tauri::command]
async fn init_reasoning_engine(
    state: tauri::State<'_, AppState>,
) -> Result<bool, String> {
    let mut bridge = state.reasoning.lock().await;
    bridge.init_engine().await?;
    Ok(true)
}

#[tauri::command]
async fn instantiate_agent(
    wasm_bytes_base64: String,
    agent_label: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let mut bridge = state.reasoning.lock().await;
    let engine = bridge
        .engine
        .as_ref()
        .ok_or_else(|| "ReasoningEngine not initialized. Call init_reasoning_engine first.".to_string())?;

    use base64::Engine;
    let wasm_bytes = base64::engine::general_purpose::STANDARD
        .decode(&wasm_bytes_base64)
        .map_err(|e| format!("Invalid base64 WASM bytes: {}", e))?;

    let mut agent_id = [0u8; 32];
    let hash = blake3::hash(&wasm_bytes);
    agent_id.copy_from_slice(&hash.as_bytes()[..32]);

    let instance = engine
        .instantiate(&wasm_bytes, agent_id)
        .await
        .map_err(|e| format!("Failed to instantiate agent: {}", e))?;

    let id_key = format!("{}:{}", agent_label, hex::encode(agent_id));
    bridge.instances.insert(id_key.clone(), instance);

    Ok(bridge.instances.keys().cloned().collect())
}

#[tauri::command]
async fn step_agent(
    agent_key: String,
    fuel_budget: Option<u64>,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<StepOutcomePayload, String> {
    let mut bridge = state.reasoning.lock().await;
    let instance = bridge
        .instances
        .get_mut(&agent_key)
        .ok_or_else(|| format!("Agent '{}' not found", agent_key))?;

    let outcome = instance
        .call_step(fuel_budget)
        .await
        .map_err(|e| format!("Agent step failed: {}", e))?;

    let agent_id = hex::encode(bridge.instances.get(&agent_key).map(|_| [0u8;32]).unwrap_or([0u8;32]));

    if outcome.agent_yielded {
        let yield_msg = String::from_utf8_lossy(&outcome.yield_message).to_string();
        let _ = app.emit(
            "reasoning-agent-yield",
            serde_json::json!({
                "agentId": agent_key,
                "yieldMessage": yield_msg,
                "envelopesSent": outcome.total_requests_sent,
                "fuelUsed": outcome.fuel_used,
            }),
        );
    }

    Ok(StepOutcomePayload {
        agent_yielded: outcome.agent_yielded,
        yield_message: String::from_utf8_lossy(&outcome.yield_message).to_string(),
        total_requests_sent: outcome.total_requests_sent,
        fuel_used: outcome.fuel_used,
    })
}

#[tauri::command]
async fn list_agents(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let bridge = state.reasoning.lock().await;
    Ok(bridge.instances.keys().cloned().collect())
}

#[tauri::command]
async fn destroy_agent(
    agent_key: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let mut bridge = state.reasoning.lock().await;
    bridge.instances.remove(&agent_key);
    Ok(())
}
```

- [ ] **Step 5: Register commands in invoke_handler**

Add the 5 new commands to the handler list:

```rust
            init_reasoning_engine,
            instantiate_agent,
            step_agent,
            list_agents,
            destroy_agent
```

- [ ] **Step 6: Verify cargo check compiles**

Run: `cargo check` from `ageex_ui/src-tauri/`
Expected: Compiles (will likely need `base64 = "0.22"`, `blake3 = "1"`, `hex = "0.4"` in Cargo.toml)

---

### Task 8: AppShell — Wire up reasoning events and toggle

**Files:**
- Modify: `ageex_ui/src/components/AppShell.tsx`

- [ ] **Step 1: Add imports**

After line 12 (`import { useChatStore`), add:

```typescript
import { useReasoningStore } from "../stores/reasoningStore";
```

After the type imports from ipc.ts (line 33), add:

```typescript
  REASONING_ENVELOPE_EVENT,
  REASONING_VIOLATION_EVENT,
  REASONING_YIELD_EVENT,
  type AirgapEnvelopeEvent,
  type SecurityViolationEvent,
  type AgentYieldEvent,
```

- [ ] **Step 2: Add event listeners**

After line 320 (end of gateway event stream listener), before line 322 (`};`), add:

```typescript
      unlisteners.push(
        await listen<AirgapEnvelopeEvent>(REASONING_ENVELOPE_EVENT, (event) => {
          useReasoningStore.getState().handleEnvelopeEvent(event.payload);
        }),
      );
      unlisteners.push(
        await listen<SecurityViolationEvent>(REASONING_VIOLATION_EVENT, (event) => {
          useReasoningStore.getState().handleViolationEvent(event.payload);
        }),
      );
      unlisteners.push(
        await listen<AgentYieldEvent>(REASONING_YIELD_EVENT, (event) => {
          useReasoningStore.getState().handleYieldEvent(event.payload);
        }),
      );
```

- [ ] **Step 3: Add showAgents state and toggle**

After line 109 (`const [isCompactLayout, setIsCompactLayout]`, add:

```typescript
  const [showAgents, setShowAgents] = useState(false);
```

- [ ] **Step 4: Add handleToggleAgents**

After `handleToggleMemory` (line 671), add:

```typescript
  const handleToggleAgents = () => {
    const state = useChatStore.getState();
    if (state.isBooting) {
      return;
    }
    setShowAgents((prev) => !prev);
  };
```

- [ ] **Step 5: Pass props to ChatInterface**

In the `<ChatInterface` JSX (line 761), add the new props:

```tsx
        showAgents={showAgents}
        onToggleAgents={handleToggleAgents}
```

- [ ] **Step 6: Verify TypeScript compiles**

Run: `npx tsc --noEmit` from `ageex_ui/`
Expected: Zero errors

---

### Task 9: Final verification — build and test

- [ ] **Step 1: Run cargo check for backend**

Run: `cargo check` from `ageex_ui/src-tauri/`
Expected: Zero errors

- [ ] **Step 2: Run tsc for frontend**

Run: `npx tsc --noEmit` from `ageex_ui/`
Expected: Zero errors

- [ ] **Step 3: Commit all changes**

```bash
git add ageex_ui/src/types/ipc.ts \
        ageex_ui/src/stores/reasoningStore.ts \
        ageex_ui/src/components/ReasoningPanel.tsx \
        ageex_ui/src/components/ChatInterface.tsx \
        ageex_ui/src/components/AppShell.tsx \
        ageex_ui/src-tauri/Cargo.toml \
        ageex_ui/src-tauri/src/lib.rs \
        ageex_ui/src-tauri/src/reasoning_bridge.rs
git commit -m "feat: add Agent Dashboard sidebar with WASM reasoning engine bridge"
```