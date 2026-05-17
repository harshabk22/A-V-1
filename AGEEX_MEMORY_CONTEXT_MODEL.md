# AGEEX Memory & Context Model

## 1. Goals

- Keep LLM context within practical limits without losing critical information.
- Prevent sensitive or irrelevant data from leaking between agents or channels.
- Support long-term learning (skills and user preferences) in a controlled, auditable way.

## 2. Memory Layers

1. **Per-Agent Session Memory (SQLite)**  
   - Stores raw messages for each session:
     - user,
     - assistant,
     - tool results,
     - system and recall prompts.
   - Isolated per user and per agent by default.

2. **Shared General Context**
   - Stores high-level, non-sensitive information:
     - project names,
     - high-level project state,
     - user preferences (e.g., “prefers dark mode”, “prefers concise explanations”),
     - general organization knowledge approved for sharing.
   - Used by the Memory Manager to provide small context snippets to AGX1, AGX2, AGX3.

3. **Long-Term Skill & Permission Store**
   - Separately tracks:
     - installed skills (with trust tier and manifest),
     - granted permissions (MetaBrain graph),
     - org-level policies (what agents are allowed to do by default).

## 3. Memory Manager Agent

- The Memory Manager is an LLM-driven summarizer with the following constraints:
  - Operates on bounded windows of messages, not entire lifetime histories.
  - Produces:
    - compressed summaries,
    - key-value style facts,
    - tags/labels for retrieval.
  - Its input always goes through redaction and filtering rules to remove:
    - secrets,
    - credentials,
    - irrelevant low-level details.

- After each summarization job:
  - Working memory is treated as discarded.
  - Only the written outputs in the stores are preserved.

## 4. Context Flow Rules

1. **Into AGX1 (Read Agent)**
   - Receives:
     - user’s current query,
     - small summary of relevant session history,
     - relevant pieces from shared general context.
   - Does not receive:
     - raw external app histories (WhatsApp, etc.),
     - unrelated agent outputs.

2. **Into AGX2 (Write/Skill Agent)**
   - Receives:
     - full task description from the UI,
     - necessary file/skill context,
     - filtered summaries of related sessions and approvals.
   - Does not receive:
     - arbitrary external app messages,
     - org-/user-level secrets beyond what is required for the task.

3. **Into AGX3 (Connector Agent)**
   - Receives:
     - inputs from its channel (e.g., WhatsApp messages),
     - minimal shared context specific to that channel and user.
   - Outputs:
     - safe textual replies for that channel,
     - summary updates to memory.

4. **Between Agents via Memory Manager**
   - Cross-agent context is **not** direct.  
   - Memory Manager:
     - reads from per-agent stores and shared context,
     - produces a targeted summary for the requesting agent,
     - writes back only generalized, non-sensitive information.

## 5. Context Overflow & Compression

- Each session has an effective context limit tied to the active model (e.g., 128K tokens).
- When a session approaches a threshold (e.g., 70% of the limit):
  - Memory Manager is called to:
    - summarize older segments,
    - store summaries into the shared context and per-agent stores,
    - mark older raw messages as “archived” (not used in live prompts).
- No destructive deletion:
  - Raw history remains in persistent storage for audit and replay.
  - Compression affects what goes into **new prompts**, not what is stored on disk.

## 6. Multi-User and Enterprise Behavior

- Each user:
  - has their own isolated session and personal memory space,
  - may share org-level context defined by administrators.
- Enterprise admins:
  - control which skills and shared knowledge are available,
  - can inspect audit logs and consent records,
  - cannot weaken core invariants (e.g., irreversible operations still require consent and DEL enforcement).
