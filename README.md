# AGX (MoatAgeex)

A security-first desktop assistant and autonomous agent sandbox for your computer.

Most desktop AI assistants run terminal commands and modify files with unchecked permissions. AGX changes that: it sits between the model and your operating system, putting every command, file write, and process through a strict 3-gate security layer. If an agent tries to touch sensitive files or run risky tools, it asks for your permission first.

```
┌─────────────────────────────────────────────────────────────┐
│                 Desktop Interface (React + Tauri 2)         │
│    • Chat window               • Spotlight command bar      │
│    • Cryptographic consent     • Agent sandbox inspector    │
└──────────────────────────────┬──────────────────────────────┘
                               │ Tauri IPC
                               ▼
┌─────────────────────────────────────────────────────────────┐
│                 Moat Core (Rust Hypervisor)                 │
│    • Gate 1: Schema validation                              │
│    • Gate 2: Workspace policy checks                        │
│    • Gate 3: Sandboxed execution + User consent             │
│    • Local SQLite memory & session storage                  │
└──────────────────────────────┬──────────────────────────────┘
                               │ Airgap Protocol
                               ▼
┌─────────────────────────────────────────────────────────────┐
│                 WASM Reasoning Sandbox                      │
│    • Isolated Wasmtime guests                               │
│    • Fuel-metered execution (no infinite loops)             │
│    • Zero direct host network or filesystem access          │
└─────────────────────────────────────────────────────────────┘
```

---

## What's Inside

- **`ageex_ui`**: The desktop application built with React 19, TypeScript, Vite, and Tauri 2. Includes a Spotlight-style overlay bar (hotkey: `Super+Shift+Space`) and an agent inspector.
- **`moat_core`**: The Rust engine that handles LLM streaming, session transcripts, policy enforcement, and tool execution.
- **`crates/ageex_reasoning`**: The WebAssembly guest sandbox powered by Wasmtime with strict CPU/memory fuel limits.
- **`crates/ageex_wire`**: Compact binary wire protocol connecting guest agents to the host hypervisor.
- **`wasm/`**: Example WASM guest agents.

---

## Quick Start

### 1. Prerequisites
- **Rust** (1.78 or newer): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **Node.js** (v18 or newer) & `npm`

### 2. Configure API Keys
You can use either **NVIDIA NIM** (default, free trial tier available) or **DeepSeek**.

Copy the example environment file:
```bash
cp .env.example .env
```

Open `.env` and add your key:
```bash
# NVIDIA NIM (Default)
NVIDIA_API_KEY=nvapi-your-key-here

# Or DeepSeek
DEEPSEEK_API_KEY=sk-your-key-here
```

*(You can also set your API key directly inside the desktop app Settings modal or paste `nvapi-...` into the chat box).*

### 3. Run the Desktop App
```bash
cd ageex_ui
npm install
npm run dev
```

This compiles the Rust desktop shell and launches the desktop window.

### 4. Run the Terminal CLI (Optional)
If you just want to test the core engine from your terminal without the GUI:
```bash
cd moat_core
cargo run
```

---

## How the Security Gates Work

Whenever the model decides to run a tool (like writing a file or running a shell script), the request must pass three checkpoints before anything touches your machine:

1. **Gate 1 (Schema & Format)**: Verifies the tool name and argument types, and filters out malformed payloads.
2. **Gate 2 (Workspace Boundaries)**: Confirms the file path stays strictly inside the workspace directory (`~/.moatageex/workspace`) and checks that commands aren't on the blocklist.
3. **Gate 3 (Consent & Sandbox)**: For high-risk actions (modifying files, spawning child processes, or modifying system settings), a cryptographic consent modal pops up on your screen. You see the exact payload and hash before choosing to approve or deny.

---

## Running Tests

To verify that all components are healthy on your machine:

```bash
# Test the core engine (222 unit & integration tests)
cd moat_core
cargo test

# Test the WASM reasoning engine
cd ../crates/ageex_reasoning
cargo test

# Check the frontend build
cd ../../ageex_ui
npm run build
```

---

## License

MIT / Apache 2.0
