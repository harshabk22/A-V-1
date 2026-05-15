use super::types::{FunctionDefinition, ToolDefinition};
use crate::agent::AgentId;
use serde_json::json;

/// Tools available to Agent1 (read-only)
pub fn agent1_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "fs_read".into(),
                description: "Read the contents of a file. Only works within ~/Desktop/ageex-workspace and ~/Documents.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute path to the file to read"
                        }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "fs_exists".into(),
                description: "Check if a file or directory exists at the given path.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute path to check"
                        }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "app_open".into(),
                description: "Open a macOS application by name.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "app_name": {
                            "type": "string",
                            "description": "Name of the macOS application to open (e.g. 'Safari', 'Finder')"
                        }
                    },
                    "required": ["app_name"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "url_open".into(),
                description: "Open a URL in the default browser.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "The URL to open"
                        }
                    },
                    "required": ["url"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "delegate_to_executor".into(),
                description: "Use this when the user is asking you to WRITE, MODIFY, CREATE, or EXECUTE something. \
                              You CANNOT do these things. Instead of saying 'I cannot do that', call this tool \
                              to hand the task to Agent2 who CAN execute it. \
                              Examples of when to call this: writing files, running commands, editing code, \
                              installing packages, creating anything new.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "task": {
                            "type": "string",
                            "description": "The EXACT task to delegate. Be specific. Include all context."
                        },
                        "reason": {
                            "type": "string",
                            "description": "Why you are delegating (what you cannot do)"
                        }
                    },
                    "required": ["task"]
                }),
            },
        },
    ]
}

/// Tools available to Agent2 (executor) — same base tools as Agent1 minus delegate_to_executor
pub fn agent2_tools() -> Vec<ToolDefinition> {
    let mut tools = vec![
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "fs_read".into(),
                description: "Read the contents of a file.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute path to the file to read"
                        }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "fs_exists".into(),
                description: "Check if a file or directory exists at the given path.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute path to check"
                        }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "app_open".into(),
                description: "Open a macOS application by name.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "app_name": {
                            "type": "string",
                            "description": "Name of the macOS application to open"
                        }
                    },
                    "required": ["app_name"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "url_open".into(),
                description: "Open a URL in the default browser.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "The URL to open"
                        }
                    },
                    "required": ["url"]
                }),
            },
        },
    ];

    tools.extend(vec![
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "fs_write".into(),
                description: "Write content to a file. Requires MetaBrain permission — will ask user if not already granted.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute path to write to"
                        },
                        "content": {
                            "type": "string",
                            "description": "Content to write into the file"
                        }
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "process_spawn".into(),
                description: "Run a terminal command. Output is captured and returned. Requires MetaBrain permission.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute"
                        },
                        "working_dir": {
                            "type": "string",
                            "description": "Working directory for the command (defaults to ~/Desktop/ageex-workspace)"
                        }
                    },
                    "required": ["command"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "skill_execute".into(),
                description: "Execute an installed skill by name with parameters.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "skill_name": {
                            "type": "string",
                            "description": "Name of the installed skill to execute"
                        },
                        "params": {
                            "type": "object",
                            "description": "Parameters to pass to the skill"
                        }
                    },
                    "required": ["skill_name"]
                }),
            },
        },
    ]);

    tools
}

pub fn tool_index_summary() -> String {
    let tools = agent2_tools();
    let mut lines = Vec::new();
    for tool in &tools {
        let name = &tool.function.name;
        let desc = &tool.function.description;
        let params: Vec<String> = tool.function.parameters["properties"]
            .as_object()
            .map(|props| props.keys().cloned().collect())
            .unwrap_or_default();
        lines.push(format!("  - {}({}) — {}", name, params.join(", "), desc));
    }
    lines.join("\n")
}

pub fn tool_schema_details(agent: AgentId) -> String {
    let tools = match agent {
        AgentId::Agent1 => agent1_tools(),
        AgentId::Agent2 => agent2_tools(),
        AgentId::Agent3 => vec![],
    };
    let mut lines = Vec::new();
    for tool in &tools {
        let name = &tool.function.name;
        let desc = &tool.function.description;
        let params = serde_json::to_string_pretty(&tool.function.parameters)
            .unwrap_or_default();
        lines.push(format!("## {}\n{}\n```json\n{}\n```\n", name, desc, params));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_index_summary_has_all_tools() {
        let summary = tool_index_summary();
        assert!(summary.contains("fs_read"), "Missing fs_read in: {}", summary);
        assert!(summary.contains("fs_write"), "Missing fs_write");
        assert!(summary.contains("fs_exists"), "Missing fs_exists");
        assert!(summary.contains("process_spawn"), "Missing process_spawn");
    }

    #[test]
    fn test_tool_schema_details_includes_parameters() {
        let details = tool_schema_details(AgentId::Agent2);
        assert!(details.contains("path"), "Missing path in: {}", details);
        assert!(details.contains("content") || details.contains("command"), "Missing params in: {}", details);
    }
}