use crate::capabilities::Capability;
use crate::meta_brain::PermissionScope;
use anyhow::Result;
use std::io::{self, Write};

pub struct ApprovalRequest {
    pub agent_id: String,
    pub capability: Capability,
    pub scope: PermissionScope,
    pub description: String,
}

pub enum ApprovalDecision {
    Granted,
    Denied,
}

pub struct ApprovalFlow;

impl ApprovalFlow {
    pub fn request_approval(request: ApprovalRequest) -> Result<ApprovalDecision> {
        println!("\n╔════════════════════════════════════════════════════════╗");
        println!("║          PERMISSION REQUEST FROM AGENT                  ║");
        println!("╠════════════════════════════════════════════════════════╣");
        println!("║ Agent:      {:44} ║", request.agent_id);
        println!("║ Operation:  {:43} ║", format!("{:?}", request.capability));
        println!("║ Target:     {:44} ║", Self::format_scope(&request.scope));
        println!("║ Details:    {:44} ║", request.description);
        println!("╚════════════════════════════════════════════════════════╝");
        println!();
        print!("Grant permission? [y/N]: ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        let decision = match input.trim().to_lowercase().as_str() {
            "y" | "yes" => ApprovalDecision::Granted,
            _ => ApprovalDecision::Denied,
        };

        Ok(decision)
    }

    fn format_scope(scope: &PermissionScope) -> String {
        match scope {
            PermissionScope::Path(p) => p.display().to_string(),
            PermissionScope::Domain(d) => d.clone(),
            PermissionScope::Global => "*".to_string(),
        }
    }
}