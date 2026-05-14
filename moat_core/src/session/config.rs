use anyhow::{Result, Context};
use std::path::PathBuf;

pub struct AgentConfig;

impl AgentConfig {
    pub fn load_agent1_prompt() -> Result<String> {
        Self::load_prompt("agent1")
    }

    pub fn load_agent2_prompt() -> Result<String> {
        Self::load_prompt("agent2")
    }

    fn load_prompt(agent_name: &str) -> Result<String> {
        let root_path = PathBuf::from(format!("agents/{}.md", agent_name));
        let crate_path = PathBuf::from(format!("moat_core/agents/{}.md", agent_name));

        // Warn when the root copy shadows the (richer) crate copy
        if root_path.exists() && crate_path.exists() {
            eprintln!(
                "[Config] WARNING: agents/{}.md shadows moat_core/agents/{}.md — \
                the crate copy has richer persona content that is being ignored. \
                Consider consolidating to a single source.",
                agent_name, agent_name
            );
        }

        let paths = [&root_path, &crate_path];

        for path in &paths {
            if path.exists() {
                let content = std::fs::read_to_string(path)
                    .with_context(|| format!("Failed to read {}", path.display()))?;
                println!("[Config] Loaded {} prompt from {}", agent_name, path.display());
                return Ok(content);
            }
        }

        println!("[Config] agents/{}.md not found, using built-in defaults", agent_name);
        Ok(Self::default_prompt(agent_name))
    }

    fn default_prompt(agent_name: &str) -> String {
        match agent_name {
            "agent1" => "You are a read-only assistant. You can read files, check if files exist, open applications, and open URLs. You CANNOT write files, execute terminal commands, or use skills. If asked to write or execute, tell the user you cannot do that.".into(),
            "agent2" => "You are an executor agent. You can read and write files, execute terminal commands, run skills, open applications, and open URLs. All write and execution operations require MetaBrain permission.".into(),
            _ => "You are a helpful assistant.".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fallback_prompt_loads_when_file_missing() {
        let prompt = AgentConfig::default_prompt("agent1");
        assert!(!prompt.is_empty());
        assert!(prompt.contains("read-only"));
    }
}