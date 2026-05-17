# AGEEX Connected Apps and AGX3 Isolation

## 1. Purpose

AGX3 exists to isolate connected apps and external input channels from the main reasoning and execution paths.

Its job is to receive external app inputs, keep them separated by source and user, and prevent those inputs from becoming a trigger path into system capabilities.

## 2. Core Rule

External app input is **not authority**.

A WhatsApp message, Slack message, email, calendar event, or plugin event may become context, but it must never become direct permission to act on the OS.

## 3. AGX3 Responsibilities

AGX3 may:

- ingest messages from external apps,
- preserve source isolation,
- summarize safe context,
- respond within that same channel,
- hand off filtered context to the Memory Manager.

AGX3 may not:

- call filesystem tools,
- launch processes,
- write to the OS,
- trigger AGX1 or AGX2 directly,
- bypass the DEL,
- leak cross-app data.

## 4. Isolation Model

Each connected app must remain isolated by:

- source identity,
- user identity,
- channel identity,
- session identity.

A message from WhatsApp must not automatically affect Slack, email, GitHub, or the UI session unless a deliberate, approved workflow says so.

## 5. Prompt Injection Posture

Assume every external message may be hostile.

AGX3 must therefore:

- strip unsafe directives,
- avoid treating external text as system instructions,
- pass only minimal safe context to the Memory Manager,
- never attach execution authority to message content.

If a prompt injection succeeds in the content, it still must not gain OS capability.

## 6. Routing Rules

- **UI sessions** are the only authoritative trigger for AGX1 and AGX2.
- **External apps** feed AGX3 only.
- **Memory Manager** may summarize and filter across channels, but cannot create execution authority.
- **DEL** remains the only execution gate.

## 7. Connected App Scope

Initial connected apps may include:

- WhatsApp,
- Slack,
- email,
- GitHub,
- calendar.

Each integration must be independently scoped and revocable.

## 8. Enterprise Use

For enterprise deployments:

- each user has isolated app context,
- org-level policies may allow shared general knowledge,
- sensitive app data remains private unless explicitly shared.

## 9. Required Invariants

AGEEX must never:

- let an incoming app message trigger execution by itself,
- mix app contexts across users,
- allow AGX3 to become a hidden backdoor into AGX2,
- use connector data as a substitute for permission.
