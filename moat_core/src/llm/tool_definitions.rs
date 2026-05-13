use super::types::{FunctionDefinition, ToolDefinition};
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