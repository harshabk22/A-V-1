# DEL File Deletion Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `FileDelete` capability, `fs_delete` tool, block write-as-delete proxy, and add enterprise safety nets (trash, protected patterns, severity logging).

**Architecture:** New `FileDelete` capability flows through the DEL pipeline: MetaBrain approval → Gate1 schema → Gate2 policy (path allowlist + protected patterns) → Gate3 sandbox (trash-then-remove) → Journal (severity=destructive). Existing `write_file` gate is hardened with content-inspection to block empty writes to existing files.

**Tech Stack:** Rust, serde, petgraph, regex, TOML

---

### Task 1: Add `FileDelete` to Capability Enum

**Files:**
- Modify: `moat_core/src/capabilities.rs:4-63`

- [ ] **Step 1: Write the failing test**

Append to the bottom of `capabilities.rs` inside the existing `#[cfg(test)]` block (or create one if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_delete_capability_exists() {
        let cap = Capability::FileDelete;
        assert_eq!(format!("{}", cap), "fs:delete");
    }

    #[test]
    fn test_agent2_default_includes_file_delete() {
        let set = CapabilitySet::agent2_default();
        assert!(set.has(&Capability::FileDelete));
    }

    #[test]
    fn test_agent1_default_lacks_file_delete() {
        let set = CapabilitySet::agent1_default();
        assert!(!set.has(&Capability::FileDelete));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p moat_core -- test_file_delete test_agent2_default_includes_file_delete test_agent1_default_lacks_file_delete`
Expected: COMPILE ERROR — `FileDelete` variant not found in `Capability` enum.

- [ ] **Step 3: Implement minimal code**

In `capabilities.rs`:

Add variant to enum (line 6, before `ProcessSpawn`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    FileRead,
    FileWrite,
    FileDelete,
    FileExists,
    ProcessSpawn,
    AppOpen,
    UrlOpen,
    SkillExecute,
}
```

Add Display arm (after `FileWrite` arm):

```rust
impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Capability::FileRead => write!(f, "fs:read"),
            Capability::FileWrite => write!(f, "fs:write"),
            Capability::FileDelete => write!(f, "fs:delete"),
            Capability::FileExists => write!(f, "fs:exists"),
            Capability::ProcessSpawn => write!(f, "process:spawn"),
            Capability::AppOpen => write!(f, "app:open"),
            Capability::UrlOpen => write!(f, "url:open"),
            Capability::SkillExecute => write!(f, "skill:execute"),
        }
    }
}
```

Add to `agent2_default()`:

```rust
pub fn agent2_default() -> Self {
    Self::new(vec![
        Capability::FileRead,
        Capability::FileWrite,
        Capability::FileDelete,
        Capability::FileExists,
        Capability::ProcessSpawn,
        Capability::AppOpen,
        Capability::UrlOpen,
        Capability::SkillExecute,
    ])
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p moat_core -- test_file_delete test_agent2_default_includes_file_delete test_agent1_default_lacks_file_delete`
Expected: 3 tests PASS

- [ ] **Step 5: Commit**

```bash
git add moat_core/src/capabilities.rs
git commit -m "feat: add FileDelete capability variant"
```

---

### Task 2: Add `fs_delete` Tool Definition for Agent2

**Files:**
- Modify: `moat_core/src/llm/tool_definitions.rs:104-243`

- [ ] **Step 1: Write the failing test**

Add a test module at the bottom of `tool_definitions.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent2_has_fs_delete_tool() {
        let tools = agent2_tools();
        let has_delete = tools.iter().any(|t| t.function.name == "fs_delete");
        assert!(has_delete, "Agent2 tools must include fs_delete");
    }

    #[test]
    fn test_agent1_lacks_fs_delete_tool() {
        let tools = agent1_tools();
        let has_delete = tools.iter().any(|t| t.function.name == "fs_delete");
        assert!(!has_delete, "Agent1 must not have fs_delete (read-only)");
    }

    #[test]
    fn test_fs_delete_tool_requires_path() {
        let tool = agent2_tools().into_iter().find(|t| t.function.name == "fs_delete").unwrap();
        let params = tool.function.parameters;
        let required: Vec<String> = params["required"].as_array().unwrap()
            .iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert!(required.contains(&"path".to_string()));
        assert!(!required.contains(&"content".to_string()),
            "fs_delete must not require content (write_file requires it, delete_file does not)");
        assert_eq!(required.len(), 1, "fs_delete should require only 'path'");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p moat_core -- test_agent2_has_fs_delete_tool test_agent1_lacks_fs_delete_tool test_fs_delete_tool_requires_path`
Expected: FAIL — `test_agent2_has_fs_delete_tool` fails, fs_delete not found.

- [ ] **Step 3: Implement minimal code**

In `agent2_tools()`, add the `fs_delete` tool after the `fs_write` tool (inside the `tools.extend()` call, before `process_spawn`):

```rust
ToolDefinition {
    tool_type: "function".into(),
    function: FunctionDefinition {
        name: "fs_delete".into(),
        description: "Delete a file. WARNING: Destructive — file is moved to trash (~/.ageex/trash/). Requires MetaBrain approval.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the file to delete"
                }
            },
            "required": ["path"]
        }),
    },
},
```

Insert this between the `fs_write` tool definition and the `process_spawn` tool definition in the `tools.extend(vec![...])` block.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p moat_core -- test_agent2_has_fs_delete_tool test_agent1_lacks_fs_delete_tool test_fs_delete_tool_requires_path`
Expected: 3 tests PASS

- [ ] **Step 5: Commit**

```bash
git add moat_core/src/llm/tool_definitions.rs
git commit -m "feat: add fs_delete tool definition for Agent2"
```

---

### Task 3: Add `fs_delete` Executor Arm and DEL Capability Mapping

**Files:**
- Modify: `moat_core/src/tools/executor.rs:38-92, 184-191`

- [ ] **Step 1: Write the failing test**

Add tests to the existing `#[cfg(test)] mod tests` block:

```rust
#[test]
fn test_fs_delete_requires_approval_when_not_granted() {
    // Verify tool_to_del_capability maps delete_file to FileDelete
    assert_eq!(
        ToolExecutor::tool_to_del_capability("delete_file"),
        Capability::FileDelete
    );

    // Verify read/write/spawn mappings are unchanged
    assert_eq!(
        ToolExecutor::tool_to_del_capability("read_file"),
        Capability::FileRead
    );
    assert_eq!(
        ToolExecutor::tool_to_del_capability("write_file"),
        Capability::FileWrite
    );
    assert_eq!(
        ToolExecutor::tool_to_del_capability("process_spawn"),
        Capability::ProcessSpawn
    );
}

#[test]
fn test_unknown_tool_defaults_to_fileread() {
    assert_eq!(
        ToolExecutor::tool_to_del_capability("nonexistent_tool"),
        Capability::FileRead
    );
}
```

Note: The `tool_to_del_capability` function is currently `fn tool_to_del_capability(tool_name: &str) -> Capability` — it's NOT `pub`. We need to make it `pub(crate)` or `pub` for testing. Update the function signature:

```rust
pub(crate) fn tool_to_del_capability(tool_name: &str) -> Capability {
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p moat_core -- test_fs_delete_requires_approval`
Expected: COMPILE ERROR or TEST FAILURE — if `delete_file` not mapped to `FileDelete`.

If the function isn't `pub`, the test won't compile. Fix the visibility first, then verify the test fails on the mapping.

- [ ] **Step 3: Implement minimal code**

In `executor.rs`:

**a)** Add the `"fs_delete"` match arm in `execute()` — insert after the `"fs_write"` arm (before `"process_spawn"`):

```rust
            "fs_delete" => {
                let raw_path = args["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("fs_delete requires 'path' argument"))?;
                let normalized = normalize_workspace_path(raw_path);
                if !self.check_permission_with_approval(
                    Capability::FileDelete,
                    PermissionScope::Path(PathBuf::from(&normalized)),
                )? {
                    return Ok(format!("[fs:delete] DENIED: No permission to delete {}", raw_path));
                }
                self.execute_del("delete_file", args)
            }
```

**b)** Add `"delete_file"` capability mapping in `tool_to_del_capability()`:

```rust
    pub(crate) fn tool_to_del_capability(tool_name: &str) -> Capability {
        match tool_name {
            "read_file" => Capability::FileRead,
            "write_file" => Capability::FileWrite,
            "delete_file" => Capability::FileDelete,
            "process_spawn" => Capability::ProcessSpawn,
            _ => Capability::FileRead,
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p moat_core -- test_fs_delete_requires_approval test_unknown_tool_defaults_to_fileread`
Expected: 2 tests PASS

Also run the full suite to check nothing broke:
Run: `cargo test -p moat_core`
Expected: All 77 existing tests PASS

- [ ] **Step 5: Commit**

```bash
git add moat_core/src/tools/executor.rs
git commit -m "feat: add fs_delete executor arm and FileDelete DEL capability mapping"
```

---

### Task 4: Add `delete_file` Schema Validation (Gate1)

**Files:**
- Modify: `moat_core/src/execution/gate1_schema.rs:41-66`

- [ ] **Step 1: Write the failing test**

Add tests to the existing `#[cfg(test)] mod tests`:

```rust
#[test]
fn test_validate_delete_file_schema_valid() {
    let req = ExecutionRequest::new(
        "agent2",
        "delete_file",
        Capability::FileDelete,
        serde_json::json!({"path": "/workspace/test.txt"})
    );
    assert!(Gate1Schema::validate(req).is_ok());
}

#[test]
fn test_validate_delete_file_schema_invalid_no_path() {
    let req = ExecutionRequest::new(
        "agent2",
        "delete_file",
        Capability::FileDelete,
        serde_json::json!({})
    );
    let result = Gate1Schema::validate(req);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.gate1_passed, false);
    assert!(err.denied_reason.unwrap().contains("path"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p moat_core -- test_validate_delete_file_schema_valid test_validate_delete_file_schema_invalid_no_path`
Expected: COMPILE ERROR — `Capability::FileDelete` not found (if Task 1 not done) OR test failure for schema validation (delete_file not handled by Gate1).

- [ ] **Step 3: Implement minimal code**

In `gate1_schema.rs`, add to `validate_tool_schema()`:

```rust
            "delete_file" | "fs_delete" => {
                if args.get("path").and_then(|v| v.as_str()).is_none() {
                    bail!("delete_file requires 'path' (string)");
                }
            },
```

Insert this case after the `"write_file"` block and before the `"process_spawn"` block.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p moat_core -- test_validate_delete_file_schema`
Expected: 2 tests PASS

- [ ] **Step 5: Commit**

```bash
git add moat_core/src/execution/gate1_schema.rs
git commit -m "feat: add delete_file schema validation in Gate1"
```

---

### Task 5: Add `delete_file` Sandbox Execution with Trash (Gate3)

**Files:**
- Modify: `moat_core/src/execution/gate3_sandbox.rs:15-130`

- [ ] **Step 1: Write the failing test**

Add tests to a new `#[cfg(test)] mod tests` block at the bottom of `gate3_sandbox.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::Capability;
    use std::fs;

    #[test]
    fn test_delete_file_moves_to_trash() {
        let tmp = std::env::temp_dir();
        let workspace = tmp.clone();
        let test_file = tmp.join("test_delete_me.txt");
        fs::write(&test_file, "hello world").unwrap();

        let req = ExecutionRequest::new(
            "agent2",
            "delete_file",
            Capability::FileDelete,
            serde_json::json!({"path": test_file.to_string_lossy()}),
        );

        let result = Gate3Sandbox::execute(&req);
        match &result {
            Ok(r) => {
                assert!(r.success, "delete should succeed");
                assert!(r.output.contains("trash"), "output should mention trash: {}", r.output);
            }
            Err(e) => panic!("Delete failed: {:?}", e.denied_reason),
        }

        // Original file should be gone
        assert!(!test_file.exists(), "file should be deleted from original location");

        // Trash file should exist
        let trash_dir = dirs::home_dir().unwrap().join(".ageex").join("trash");
        let mut trash_found = false;
        if trash_dir.exists() {
            for entry in fs::read_dir(&trash_dir).unwrap() {
                let entry = entry.unwrap();
                if entry.file_name().to_string_lossy().contains("test_delete_me") {
                    trash_found = true;
                    // Clean up trash entry
                    fs::remove_file(entry.path()).ok();
                    break;
                }
            }
        }
        assert!(trash_found, "file should appear in trash directory: {:?}", trash_dir);
    }

    #[test]
    fn test_delete_nonexistent_file_fails() {
        let tmp = std::env::temp_dir();
        let nonexistent = tmp.join("does_not_exist_xyz");
        let _ = fs::remove_file(&nonexistent);

        let req = ExecutionRequest::new(
            "agent2",
            "delete_file",
            Capability::FileDelete,
            serde_json::json!({"path": nonexistent.to_string_lossy()}),
        );

        let result = Gate3Sandbox::execute(&req);
        match result {
            Ok(r) => assert!(!r.success || r.output.contains("not found") || r.output.contains("No such file"),
                "should report error for nonexistent file, got: {}", r.output),
            Err(e) => assert!(!e.gate3_passed, "should fail gate3 for nonexistent file"),
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p moat_core -- test_delete_file_moves_to_trash test_delete_nonexistent_file_fails`
Expected: COMPILE ERROR — `"delete_file"` not handled in Gate3 match, or test failure.

- [ ] **Step 3: Implement minimal code**

In `gate3_sandbox.rs`:

**a)** Add the match arm in `execute()` after the `write_file` arm:

```rust
            "delete_file" | "fs_delete" => Self::execute_delete_file(req, start),
```

The full `execute()` match should now be:

```rust
    pub fn execute(req: &ExecutionRequest) -> Result<ExecutionResult, ExecutionResult> {
        let start = std::time::Instant::now();

        match req.tool_name.as_str() {
            "read_file" | "fs_read" => Self::execute_read_file(req, start),
            "write_file" | "fs_write" => Self::execute_write_file(req, start),
            "delete_file" | "fs_delete" => Self::execute_delete_file(req, start),
            "process_spawn" => Self::execute_process_spawn(req, start),
            _ => Err(ExecutionResult::gate3_denied(
                &req.request_id,
                &format!("Unknown sandbox tool: {}", req.tool_name)
            )),
        }
    }
```

**b)** Add the `execute_delete_file` method after `execute_write_file`:

```rust
    fn execute_delete_file(req: &ExecutionRequest, start: std::time::Instant) -> Result<ExecutionResult, ExecutionResult> {
        let raw = req.args["path"].as_str().unwrap();
        let normalized = normalize_workspace_path(raw);
        let path = Self::resolve_path(&normalized, &req.working_dir);

        if !path.exists() {
            return Err(ExecutionResult::gate3_denied(
                &req.request_id,
                &format!("Delete failed: file not found: {:?}", path)
            ));
        }

        let trash_dir = dirs::home_dir()
            .ok_or_else(|| ExecutionResult::gate3_denied(&req.request_id, "Cannot determine home directory"))?
            .join(".ageex").join("trash");
        fs::create_dir_all(&trash_dir)
            .map_err(|e| ExecutionResult::gate3_denied(&req.request_id, &format!("Cannot create trash dir: {}", e)))?;

        let original_name = path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let trash_name = format!("{}_{}", uuid::Uuid::new_v4(), original_name);
        let trash_path = trash_dir.join(&trash_name);

        fs::rename(&path, &trash_path)
            .map_err(|e| ExecutionResult::gate3_denied(&req.request_id, &format!("Delete (rename to trash) failed: {}", e)))?;

        let elapsed = start.elapsed().as_millis() as u64;
        eprintln!("[Gate3] ✓ delete_file succeeded ({:.1}ms)", elapsed as f64);
        Ok(ExecutionResult::success(
            &req.request_id,
            format!("Deleted {:?} (moved to trash: {:?})", path, trash_path),
            Some(0),
            elapsed,
        ))
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p moat_core -- test_delete_file_moves_to_trash test_delete_nonexistent_file_fails`
Expected: 2 tests PASS

- [ ] **Step 5: Commit**

```bash
git add moat_core/src/execution/gate3_sandbox.rs
git commit -m "feat: add delete_file sandbox execution with trash safety net"
```

---

### Task 6: Harden Gate2 — `#[serde(default)]`, Content Inspection, Protected Patterns

**Files:**
- Modify: `moat_core/src/execution/gate2_policy.rs:1-206`
- Modify: `moat_core/policies/default.toml:1-24`

- [ ] **Step 1: Write the failing test**

Add tests to the existing `#[cfg(test)] mod tests` in `gate2_policy.rs`:

```rust
#[test]
fn test_write_empty_to_existing_file_blocked() {
    let tmp = std::env::temp_dir();
    let test_file = tmp.join("existing.txt");
    std::fs::write(&test_file, "some content").unwrap();

    let gate = Gate2Policy::default();
    let req = ExecutionRequest::new(
        "agent2",
        "write_file",
        Capability::FileWrite,
        serde_json::json!({
            "path": test_file.to_string_lossy(),
            "content": ""
        })
    );

    let result = gate.enforce(&req);
    assert!(result.is_err(), "Empty write to existing file should be blocked");
    let err = result.unwrap_err();
    assert!(err.denied_reason.unwrap().contains("empty") || err.denied_reason.unwrap().contains("fs_delete"),
        "Denial should mention file deletion intent");

    std::fs::remove_file(&test_file).ok();
}

#[test]
fn test_delete_protected_pattern_denied() {
    let gate = Gate2Policy::default();
    let req = ExecutionRequest::new(
        "agent2",
        "delete_file",
        Capability::FileDelete,
        serde_json::json!({
            "path": "~/Desktop/ageex-workspace/debug.log"
        })
    );

    let result = gate.enforce(&req);
    assert!(result.is_err(), "Deleting *.log should be denied by protected_patterns");
    let err = result.unwrap_err();
    assert!(err.denied_reason.unwrap().contains("protected"),
        "Denial should mention protected pattern");
}

#[test]
fn test_delete_allowed_path_succeeds() {
    let gate = Gate2Policy::default();
    let req = ExecutionRequest::new(
        "agent2",
        "delete_file",
        Capability::FileDelete,
        serde_json::json!({
            "path": "~/Desktop/ageex-workspace/test.rs"
        })
    );

    let result = gate.enforce(&req);
    assert!(result.is_ok(), "Deleting non-protected workspace file should pass Gate2");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p moat_core -- test_write_empty_to_existing_file_blocked test_delete_protected_pattern_denied test_delete_allowed_path_succeeds`
Expected: COMPILE ERROR — `ExecutionPolicy` has no `protected_patterns` field, or tests fail because checks not implemented.

- [ ] **Step 3: Implement minimal code**

**6a) Add `#[serde(default)]` and new fields to `ExecutionPolicy`:**

```rust
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct ExecutionPolicy {
    pub allow_network: bool,
    pub allow_process_spawn: bool,
    pub max_file_write_size_mb: usize,
    pub min_write_size_bytes: usize,
    pub blocked_commands: Vec<String>,
    pub allowed_paths: Vec<String>,
    pub protected_patterns: Vec<String>,
    pub require_approval_for: Vec<String>,
}
```

**6b) Update `Default` impl with new defaults:**

```rust
impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            allow_network: false,
            allow_process_spawn: true,
            max_file_write_size_mb: 10,
            min_write_size_bytes: 1,
            blocked_commands: vec![
                "rm -rf /".into(),
                "sudo".into(),
                "mkfs".into(),
                "dd if=".into(),
            ],
            allowed_paths: vec![
                "~/Desktop/ageex-workspace".into(),
            ],
            protected_patterns: vec![
                "*.log".into(),
                "*.db".into(),
                "*.sqlite".into(),
                ".git/**.into(),
                "*.key".into(),
                "*.pem".into(),
                "Cargo.lock".into(),
            ],
            require_approval_for: vec![
                "ProcessSpawn".into(),
                "FileWrite".into(),
                "FileDelete".into(),
            ],
        }
    }
}
```

**6c) Add content-inspection to `enforce()` — insert after the existing write_file size check block (after line 96):**

```rust
        // Block empty writes to existing files (proxy for delete)
        if req.tool_name == "write_file" || req.tool_name == "fs_write" {
            if let Some(content) = req.args.get("content").and_then(|v| v.as_str()) {
                if content.is_empty() {
                    if let Some(path_str) = req.args.get("path").and_then(|v| v.as_str()) {
                        let normalized = Self::expand_tilde(&normalize_workspace_path(path_str));
                        let path = PathBuf::from(&normalized);
                        if path.exists() {
                            return Err(ExecutionResult::gate2_denied(
                                &req.request_id,
                                "Empty write to existing file detected — use fs_delete to delete files."
                            ));
                        }
                    }
                }
                // Block writes below minimum size
                if content.len() < self.policy.min_write_size_bytes {
                    return Err(ExecutionResult::gate2_denied(
                        &req.request_id,
                        &format!("Write content too small ({} bytes < {} minimum). Use fs_delete to delete files.",
                            content.len(), self.policy.min_write_size_bytes)
                    ));
                }
            }
        }
```

**6d) Add protected-pattern check for delete_file — insert BEFORE the write_file check block (after the path allowlist check at line 84):**

```rust
        // Protected pattern check for delete operations
        if req.tool_name == "delete_file" || req.tool_name == "fs_delete" {
            if let Some(path_str) = req.args.get("path").and_then(|v| v.as_str()) {
                let normalized = Self::expand_tilde(&normalize_workspace_path(path_str));
                let path = PathBuf::from(&normalized);
                let resolved = if path.is_absolute() { path.clone() } else { req.working_dir.join(&path) };
                if Self::is_path_protected(&resolved, &self.policy.protected_patterns) {
                    return Err(ExecutionResult::gate2_denied(
                        &req.request_id,
                        &format!("File {:?} matches a protected pattern. Deletion denied.", resolved)
                    ));
                }
            }
        }
```

**6e) Add the `is_path_protected` helper method:**

```rust
    fn is_path_protected(path: &Path, patterns: &[String]) -> bool {
        let path_str = path.to_string_lossy().to_string();
        let file_name = path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();

        for pattern in patterns {
            if pattern.starts_with("*.") {
                let ext = pattern.trim_start_matches('*');
                if file_name.ends_with(ext) {
                    return true;
                }
            } else if pattern.contains("/**") {
                let prefix = pattern.trim_end_matches("/**");
                if path_str.contains(prefix) {
                    return true;
                }
            } else if file_name == *pattern {
                return true;
            }
        }
        false
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p moat_core -- test_write_empty_to_existing_file_blocked test_delete_protected_pattern_denied test_delete_allowed_path_succeeds`
Expected: 3 tests PASS

Also run all existing Gate2 tests:
Run: `cargo test -p moat_core -- test_blocked_command_denied test_allowed_path_passes test_disallowed_path_denied`
Expected: All 3 existing tests PASS

- [ ] **Step 5: Update policy TOML**

Update `moat_core/policies/default.toml`:

```toml
# AGEEX Default Execution Policy
# Path: ~/.ageex/policy.toml (copied from this template on first run)

allow_network = false
allow_process_spawn = true
max_file_write_size_mb = 10
min_write_size_bytes = 1

blocked_commands = [
    "rm -rf /",
    "sudo",
    "mkfs",
    "dd if=/dev/",
    "chmod 777 /",
    ":(){:|:&};:",
]

allowed_paths = [
    "~/Desktop/ageex-workspace",
]

protected_patterns = [
    "*.log",
    "*.db",
    "*.sqlite",
    ".git/**",
    "*.key",
    "*.pem",
    "Cargo.lock",
]

require_approval_for = [
    "ProcessSpawn",
    "FileWrite",
    "FileDelete",
]
```

- [ ] **Step 6: Commit**

```bash
git add moat_core/src/execution/gate2_policy.rs moat_core/policies/default.toml
git commit -m "feat: harden Gate2 with content inspection, protected patterns, and serde default"
```

---

### Task 7: Wire Into System — MetaBrain Init + Replay Approvals

**Files:**
- Modify: `moat_core/src/lib.rs:66-106, 184-192`

- [ ] **Step 1: Verify existing tests still pass first**

Run: `cargo test -p moat_core`
Expected: All tests PASS (Task 1-6 tests plus original 77).

- [ ] **Step 2: Add `FileDelete` grant to MetaBrain init**

In `lib.rs`, inside the MetaBrain init block (after the `FileWrite` grant for agent2, before the `ProcessSpawn` grant), add:

```rust
        brain.grant_permission(
            "agent2",
            Capability::FileDelete,
            PermissionScope::Path(workspace.clone()),
        )?;
```

The agent2 grants block should now look like:

```rust
        brain.grant_permission(
            "agent2",
            Capability::FileRead,
            PermissionScope::Path(workspace.clone()),
        )?;
        brain.grant_permission(
            "agent2",
            Capability::FileWrite,
            PermissionScope::Path(workspace.clone()),
        )?;
        brain.grant_permission(
            "agent2",
            Capability::FileDelete,
            PermissionScope::Path(workspace.clone()),
        )?;
        brain.grant_permission(
            "agent2",
            Capability::ProcessSpawn,
            PermissionScope::Path(workspace),
        )?;
```

- [ ] **Step 3: Add `"FileDelete"` to replay_approvals match**

In `lib.rs`, find the `replay_approvals` function, locate the string match on `a.capability.as_str()`. Add before the wildcard `_ => continue`:

```rust
            "FileDelete" => Capability::FileDelete,
```

The match should now be:

```rust
            let cap: Capability = match a.capability.as_str() {
                "FileRead" => Capability::FileRead,
                "FileWrite" => Capability::FileWrite,
                "FileDelete" => Capability::FileDelete,
                "FileExists" => Capability::FileExists,
                "ProcessSpawn" => Capability::ProcessSpawn,
                "AppOpen" => Capability::AppOpen,
                "UrlOpen" => Capability::UrlOpen,
                "SkillExecute" => Capability::SkillExecute,
                _ => continue,
            };
```

- [ ] **Step 4: Run all tests**

Run: `cargo test -p moat_core`
Expected: All tests PASS (original 77 + new 12 from Tasks 1-6 = 89).

- [ ] **Step 5: Commit**

```bash
git add moat_core/src/lib.rs
git commit -m "feat: wire FileDelete into MetaBrain init and approval replay"
```

---

### Task 8: Add Severity Tagging to Execution Journal

**Files:**
- Modify: `moat_core/src/execution/journal.rs:1-75`

- [ ] **Step 1: Write the failing test**

Add tests to a new `#[cfg(test)] mod tests` block at the bottom of `journal.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::Capability;
    use std::io::BufRead;

    #[test]
    fn test_journal_logs_destructive_severity_for_delete() {
        let tmp = std::env::temp_dir().join(format!("journal_delete_test_{}.jsonl", uuid::Uuid::new_v4()));
        let journal = ExecutionJournal { path: tmp.clone() };

        let req = ExecutionRequest::new(
            "agent2",
            "delete_file",
            Capability::FileDelete,
            serde_json::json!({"path": "test.txt"})
        );
        journal.log_request(&req).unwrap();

        let result = ExecutionResult::success(
            &req.request_id,
            "Deleted test.txt".into(),
            Some(0),
            10,
        );
        journal.log_result(&result).unwrap();

        // Read back and check severity
        let file = std::fs::File::open(&tmp).unwrap();
        let lines: Vec<String> = std::io::BufReader::new(file).lines()
            .filter_map(|l| l.ok()).collect();

        let result_line = lines.iter().find(|l| l.contains("\"type\":\"result\"")).unwrap();
        assert!(result_line.contains("\"severity\":\"destructive\""),
            "Delete result should have severity=destructive, got: {}", result_line);

        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn test_journal_logs_normal_severity_for_read() {
        let tmp = std::env::temp_dir().join(format!("journal_read_test_{}.jsonl", uuid::Uuid::new_v4()));
        let journal = ExecutionJournal { path: tmp.clone() };

        let req = ExecutionRequest::new(
            "agent1",
            "read_file",
            Capability::FileRead,
            serde_json::json!({"path": "test.txt"})
        );
        journal.log_request(&req).unwrap();

        let result = ExecutionResult::success(
            &req.request_id,
            "file contents".into(),
            None,
            5,
        );
        journal.log_result(&result).unwrap();

        let file = std::fs::File::open(&tmp).unwrap();
        let lines: Vec<String> = std::io::BufReader::new(file).lines()
            .filter_map(|l| l.ok()).collect();

        let result_line = lines.iter().find(|l| l.contains("\"type\":\"result\"")).unwrap();
        assert!(result_line.contains("\"severity\":\"normal\""),
            "Read result should have severity=normal, got: {}", result_line);

        std::fs::remove_file(&tmp).ok();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p moat_core -- test_journal_logs_destructive_severity_for_delete test_journal_logs_normal_severity_for_read`
Expected: TEST FAILURE — severity field not present in journal output.

- [ ] **Step 3: Update `ExecutionJournal::log_result()`**

Find the `log_result` method. Add a `severity` field to the JSON output.

Current code (approximate):

```rust
    pub fn log_result(&self, result: &ExecutionResult) -> Result<()> {
        let entry = serde_json::json!({
            "type": "result",
            "request_id": result.request_id.to_string(),
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "success": result.success,
            "gate1_passed": result.gate1_passed,
            "gate2_passed": result.gate2_passed,
            "gate3_passed": result.gate3_passed,
            "exit_code": result.exit_code,
            "denied_reason": result.denied_reason,
            "execution_time_ms": result.execution_time_ms,
            "output": result.output.chars().take(200).collect::<String>(),
        });
        // ... append to file
    }
```

Update to include severity. But `log_result` doesn't have access to the tool_name. We need to pass it or infer from the request. Let's add severity as a parameter.

Update the `log_result` signature:

```rust
    pub fn log_result(&self, result: &ExecutionResult, severity: &str) -> Result<()> {
        let entry = serde_json::json!({
            "type": "result",
            "request_id": result.request_id.to_string(),
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "success": result.success,
            "gate1_passed": result.gate1_passed,
            "gate2_passed": result.gate2_passed,
            "gate3_passed": result.gate3_passed,
            "exit_code": result.exit_code,
            "denied_reason": result.denied_reason,
            "execution_time_ms": result.execution_time_ms,
            "severity": severity,
            "output": result.output.chars().take(200).collect::<String>(),
        });
        // ... append to file (existing write logic unchanged)
    }
```

Then update `mod.rs` `DelExecutor::execute()` to compute severity:

```rust
    pub fn execute(&self, req: ExecutionRequest) -> ExecutionResult {
        eprintln!("[DEL] Request {} → {}", req.request_id, req.tool_name);

        if let Err(e) = self.journal.log_request(&req) {
            eprintln!("[DEL] Journal write failed: {}", e);
        }

        let severity = match req.tool_name.as_str() {
            "delete_file" | "fs_delete" => "destructive",
            _ => "normal",
        };

        let req = match Gate1Schema::validate(req) {
            Ok(r) => r,
            Err(result) => {
                self.journal.log_result(&result, severity).ok();
                return result;
            }
        };

        if let Err(result) = self.policy.enforce(&req) {
            self.journal.log_result(&result, severity).ok();
            return result;
        }

        let result = match Gate3Sandbox::execute(&req) {
            Ok(r) => r,
            Err(r) => r,
        };

        if let Err(e) = self.journal.log_result(&result, severity) {
            eprintln!("[DEL] Journal write failed: {}", e);
        }

        result
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p moat_core -- test_journal_logs_destructive_severity_for_delete test_journal_logs_normal_severity_for_read`
Expected: 2 tests PASS

Run all journal tests:
Run: `cargo test -p moat_core -- journal`
Expected: All existing journal tests PASS

- [ ] **Step 5: Commit**

```bash
git add moat_core/src/execution/journal.rs moat_core/src/execution/mod.rs
git commit -m "feat: add severity tagging to execution journal (destructive for deletes)"
```

---

### Task 9: Final Verification — Full Test Suite Run

**Files:**
- None (verification only)

- [ ] **Step 1: Run all tests**

```bash
cd moat_core && cargo test
```
Expected: All tests PASS (77 original + 12 new = 89+ tests, 0 failures)

- [ ] **Step 2: Run cargo build to check for warnings**

```bash
cd moat_core && cargo build 2>&1 | grep -E "warning|error"
```
Expected: Only pre-existing deprecation warnings. No new warnings from our changes.

- [ ] **Step 3: Run the verify_del example**

```bash
cd moat_core && cargo run --example verify_del 2>&1
```
Expected: DEL verification passes (may need to check if the example uses any new features).

- [ ] **Step 4: Final commit if any cleanup needed**

```bash
git add -A
git commit -m "chore: final verification cleanup"
```

---

### Test Count Summary

| Category | New Tests |
|----------|-----------|
| Capability enum (Task 1) | 3 |
| Tool definitions (Task 2) | 3 |
| Executor mapping (Task 3) | 2 |
| Gate1 schema (Task 4) | 2 |
| Gate3 sandbox (Task 5) | 2 |
| Gate2 policy (Task 6) | 3 |
| Journal severity (Task 8) | 2 |
| **Total new tests** | **17** |
| **Expected total tests** | **77 + 17 = 94** |
