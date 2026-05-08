use serde_json::json;
use crate::orchestrator::{LlmTool, LlmFunctionDef};

pub fn get_hardcoded_tools() -> Vec<LlmTool> {
    let tools = vec![
        ("fs:read", "Read file contents", json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"string","nullable":true},"_thought":{"type":"string"}},"required":["target","_thought"]})),
        ("fs:write", "Write file contents", json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"string"},"_thought":{"type":"string"}},"required":["target","payload","_thought"]})),
        ("app:launch", "Launch application", json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"string","nullable":true},"_thought":{"type":"string"}},"required":["target","_thought"]})),
        ("process:spawn", "Spawn background process", json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"array","items":{"type":"string"}},"_thought":{"type":"string"}},"required":["target","payload","_thought"]})),
        ("os:open_url", "Open a URL", json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"string","nullable":true},"_thought":{"type":"string"}},"required":["target","_thought"]})),
        ("os:applescript", "Run AppleScript", json!({"type":"object","properties":{"target":{"type":"string","nullable":true},"payload":{"type":"string"},"_thought":{"type":"string"}},"required":["payload","_thought"]})),
        ("os:inject_text", "Inject text into active app", json!({"type":"object","properties":{"target":{"type":"string","nullable":true},"payload":{"type":"object","properties":{"text_to_inject":{"type":"string"},"expected_app_name":{"type":"string"}},"required":["text_to_inject","expected_app_name"]},"_thought":{"type":"string"}},"required":["payload","_thought"]})),
        ("os:click", "Click UI element", json!({"type":"object","properties":{"target":{"type":"string","nullable":true},"payload":{"type":"object","properties":{"element_id":{"type":"number"}},"required":["element_id"]},"_thought":{"type":"string"}},"required":["payload","_thought"]})),
        ("os:type_text", "Type text into UI element", json!({"type":"object","properties":{"target":{"type":"string","nullable":true},"payload":{"type":"object","properties":{"element_id":{"type":"number"},"text":{"type":"string"}},"required":["element_id","text"]},"_thought":{"type":"string"}},"required":["payload","_thought"]})),
        ("net:curl", "Perform HTTP request", json!({"type":"object","properties":{"target":{"type":"string"},"payload":{"type":"object","properties":{"method":{"type":"string"},"body":{"type":"string"},"headers":{"type":"object"}}},"_thought":{"type":"string"}},"required":["target","payload","_thought"]})),
        ("memory:search", "Search durable knowledge", json!({"type":"object","properties":{"target":{"type":"string","nullable":true},"payload":{"type":"string"},"_thought":{"type":"string"}},"required":["payload","_thought"]})),
        ("session:search", "Search previous sessions", json!({"type":"object","properties":{"target":{"type":"string","nullable":true},"payload":{"type":"string"},"_thought":{"type":"string"}},"required":["payload","_thought"]})),
        ("memory:learn", "Store durable knowledge", json!({"type":"object","properties":{"target":{"type":"string","nullable":true},"payload":{"type":"object","properties":{"keywords":{"type":"string"},"skill_description":{"type":"string"}},"required":["keywords","skill_description"]},"_thought":{"type":"string"}},"required":["payload","_thought"]})),
        ("meta:query_brain", "Check permissions", json!({"type":"object","properties":{"target":{"type":"string","nullable":true},"payload":{"type":"string"},"_thought":{"type":"string"}},"required":["payload","_thought"]})),
        ("system:spawn_worker", "Delegate task to sub-agent", json!({"type":"object","properties":{"target":{"type":"string","nullable":true},"payload":{"type":"object","properties":{"target_agent_id":{"type":"string"},"task":{"type":"string"},"specialization":{"type":"string"}}},"_thought":{"type":"string"}},"required":["payload","_thought"]})),
        ("propose_system_skill", "Propose a new system skill", json!({"type":"object","properties":{"target":{"type":"string","nullable":true},"payload":{"type":"object","properties":{"skill_name":{"type":"string"},"description":{"type":"string"},"source_code":{"type":"string"},"language":{"type":"string"},"parameters":{"type":"object"}},"required":["skill_name","description","source_code","language"]},"_thought":{"type":"string"}},"required":["payload","_thought"]})),
        ("user:ask_choice", "Ask user for choice", json!({"type":"object","properties":{"target":{"type":"string","nullable":true},"payload":{"type":"object","properties":{"context":{"type":"string"},"options":{"type":"array","items":{"type":"string"}}},"required":["context","options"]},"_thought":{"type":"string"}},"required":["payload","_thought"]})),
        ("done", "Finish task", json!({"type":"object","properties":{"target":{"type":"string","nullable":true},"payload":{"type":"string"},"_thought":{"type":"string"}},"required":["payload","_thought"]}))
    ];

    tools.into_iter().map(|(name, desc, params)| {
        LlmTool {
            type_: "function".into(),
            function: LlmFunctionDef {
                name: name.into(),
                description: Some(desc.into()),
                parameters: params,
            }
        }
    }).collect()
}
