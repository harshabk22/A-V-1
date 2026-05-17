# AGEEX Agent Architecture

## 1. Agent Roles Overview

AGEEX defines three primary agents plus one role-specific memory agent:

- **AGX1 (Read Agent)**  
  - Capabilities: read-only file access, open URL, open app.  
  - No write, no delete, no network-POST-type actions.  
  - Used for inspection, exploration, and analysis.

- **AGX2 (Write/Skill Agent)**  
  - Capabilities: workspace writes, skill execution, tool use, controlled process spawn.  
  - Always behind MetaBrain and DEL, and subject to consent for irreversible operations.  
  - This is the primary “do work” agent.

- **AGX3 (Connector Agent)**  
  - Capabilities: **none** on the OS.  
  - Receives inputs from external apps (WhatsApp, Slack, email, etc.) via MCP or similar protocols.  
  - Produces safe context summaries and responses for those channels.  
  - Cannot trigger AGX1 or AGX2 directly and cannot reach the execution layer.

- **Memory Manager Agent (LLM summarizer)**  
  - Role-specific agent that compresses and filters context for each of AGX1, AGX2, AGX3.  
  - Operates with **limited, scoped context** per call and per agent.  
  - After each job for an agent, its working context is considered zeroed; long-term storage is external.

## 2. Isolation Rules

1. **Agent-to-Agent Isolation**
   - AGX1, AGX2, and AGX3 do not share live state directly.
   - Any cross-agent context flows only via the Memory & Context Manager, which enforces filtering and redaction rules.

2. **Connector Isolation (AGX3)**
   - External app messages are buffered and treated as inputs into a **context-only layer**.
   - AGX3 can:
     - Read those inputs,
     - Generate replies for that channel,
     - Store safe, high-level memory.
   - AGX3 cannot:
     - Initiate tasks in AGX2,
     - Call tools that touch the filesystem, network, or processes,
     - Influence AGX1 beyond providing pre-filtered context approved by the Memory Manager.

3. **Triggering Rules**
   - Only **UI-initiated sessions** trigger the reasoning layer to perform real work (AGX1/AGX2).
   - External app inputs (AGX3) never directly trigger AGX2 to act on the OS.

## 3. Memory Agent Behavior

- The Memory Manager is an LLM with a strict, narrow role:
  - Given a chunk of session history, it produces a **short, filtered summary**.
  - It never sees full, raw histories unless explicitly allowed by design.
  - It does not hold long-term state internally; instead, summaries and selected data are written to the shared memory store.

- For each agent:
  - Input: scoped context + current query.
  - Output: distilled, safe context relevant to that agent’s task.
  - After output: no assumption that the Memory Manager “remembers” previous invocations.

## 4. Session Model

- A **session** is a bounded interaction between a user and one or more agents, with:
  - User messages,
  - Agent responses,
  - Tool calls and results,
  - Consent decisions and execution logs.

- Sessions are:
  - Isolated per user by default.
  - Optionally share **org-level knowledge** (e.g., company policies), but not personal data, across users.

## 5. Platform Considerations

- Initial implementation: macOS host, Rust core.
- Future targets: Linux servers (e.g., AWS, VPS) with the same agent boundaries and capability model.
- No platform is allowed to run a variant of AGEEX that violates the agent isolation rules above.
