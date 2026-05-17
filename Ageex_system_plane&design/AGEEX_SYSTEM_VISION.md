# AGEEX System Vision

## 1. Purpose

AGEEX is an autonomous, highly secured, permission-based agentic system that operates on behalf of the user across macOS first, and later other platforms and servers.

The system exists to perform complex multi-step tasks while guaranteeing that destructive or high-impact operations cannot occur without explicit, structured, verifiable user approval.

## 2. Core Principles

1. **Single Law: DEL or it did not happen**  
   Every OS-side effect must flow through the Deterministic Execution Layer (DEL). If an operation did not pass through DEL, the system treats it as if it never happened.

2. **Structural security, not “smart” security**  
   Safety is enforced by architecture (capabilities, isolation, sandboxing), not by trusting an LLM to behave. LLMs can be prompt-injected; structure must assume this.

3. **Explicit consent for irreversible actions**  
   Delete, move, send, push, install, and any irreversible or user-impacting actions are blocked by default. They require explicit consent routed through the consent engine and recorded in MetaBrain.

4. **Context with boundaries**  
   Every agent sees only the minimal context it needs. Shared context exists, but sensitive or deeply identifying information is never blindly broadcast between agents or external connectors.

5. **Isolation between channels**  
   UI sessions, external app inputs (WhatsApp, etc.), and system integrations are isolated. An attacker controlling one channel must not gain capabilities in another.

6. **Enterprise-ready from day one**  
   The architecture must support multiple users and organizations: isolated per-user memory, organization-level shared knowledge, and admin-level control over skills and permissions.

## 3. Target Users

- **Primary**: the system owner (developer / power user) on macOS.
- **Secondary**: other individual developers.
- **Future**: teams and enterprises using AGEEX as part of their internal workflows (coding, documentation, workflow automation, integrations).

## 4. “Never Do” Invariants

AGEEX must **never**:

- Execute destructive operations (delete, bulk modify, wipe, send external messages, push code) without:
  - Correct structural capability,
  - Correct scope, and
  - Explicit positive consent from a human via the consent system.
- Allow external inputs (WhatsApp, Slack, email, etc.) to directly trigger OS-side effects.
- Leak sensitive user or org data from one channel (app/user/org) into another without an explicit design decision.
- Bypass the DEL for convenience: no “debug backdoors”, no un-gated `Command::new()` style calls.

## 5. Six-Month Success Criteria

AGEEX is considered successful when:

- It can perform multi-step tasks autonomously (e.g., “update this repo, run tests, summarize changes, draft an email”) while respecting all safety invariants.
- It is usable by a small number of external developers without hand-holding.
- Skills can be added and installed only through explicit approval flows, with a clear audit trail.
- The system can be deployed on macOS and a server platform (e.g., Linux VPS) with the same core architecture and safety guarantees.
