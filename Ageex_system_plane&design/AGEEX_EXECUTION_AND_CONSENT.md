# AGEEX Execution and Consent Model

## 1. Purpose

This document defines how AGEEX decides whether an operation may execute, when consent is required, and how approval is recorded.

The rule is simple: no OS-side effect is allowed unless it has passed through the Deterministic Execution Layer (DEL), MetaBrain, and the applicable consent rules.

## 2. Execution Boundary

All OS-affecting actions must enter through the DEL. Direct calls to the operating system from agents are prohibited.

Execution is split into three concerns:

1. **Schema validation** — the request must be structurally valid.
2. **Policy evaluation** — the request must be allowed by deterministic rules.
3. **Sandbox execution** — the request runs only inside the constrained runtime or process wrapper.

If any stage fails, the system returns a structured denial instead of proceeding.

## 3. Risk Classes

AGEEX treats actions by reversibility and impact:

- **Read-only**: safe inspection actions.
- **Reversible write**: can be rolled back or is low-risk.
- **Irreversible destructive**: delete, send, push, install, bulk modify, external side effects.
- **System-level**: privileged or broad-impact operations.

Irreversible destructive and system-level actions require explicit user approval and must be recorded.

## 4. Consent Requirements

Consent is required when:

- The operation is irreversible.
- The operation has external side effects.
- The operation affects user data, org data, or system state materially.
- The operation crosses a new scope that has not already been granted.

Consent must be:

- Explicit,
- Structured,
- Verifiable,
- Auditable.

## 5. MetaBrain Permissions

MetaBrain is the permission graph for AGEEX.

It stores:

- agent roots,
- resource scopes,
- permission edges,
- session-aware grants.

A permission grant must be written to MetaBrain after approval so the system can remember the decision for the correct scope.

## 6. Approval Flow

1. The agent proposes an operation.
2. The DEL classifies the risk.
3. If consent is required, the request is routed to the consent engine.
4. The human approves or denies.
5. On approval, the permission is written to MetaBrain.
6. The operation executes only if the updated permission graph allows it.
7. The result is journaled.

No approval path may bypass MetaBrain or the DEL.

## 7. Denial Behavior

If an operation is denied:

- The request must fail cleanly.
- A structured denial report must be returned to the agent.
- The denial must include the reason and, where possible, safer alternatives.
- The system must not silently retry destructive requests.

## 8. Required Invariants

AGEEX must never:

- Treat a missing grant as equivalent to approval.
- Execute a destructive action because an LLM “seems sure”.
- Persist a consent outcome without a matching scope and actor.
- Allow a connector path to create direct execution authority.

## 9. Audit and Replay

Every approval, denial, and execution must be journaled.

The journal is used for:

- audit,
- replay,
- debugging,
- compliance.
