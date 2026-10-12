use crate::capabilities::Capability;
use anyhow::Result;
use std::io::{self, Write};

pub struct ApprovalRequest {
    pub agent_id: String,
    pub capability: Capability,
    pub scope: String,
    pub description: String,
}

pub enum ApprovalDecision {
    Granted,
    Denied,
    TimedOut,
}

pub struct ApprovalFlow;

impl ApprovalFlow {
    pub async fn request_approval(request: ApprovalRequest) -> Result<ApprovalDecision> {
        println!("\n╔════════════════════════════════════════════════════════╗");
        println!("║          PERMISSION REQUEST FROM AGENT                  ║");
        println!("╠════════════════════════════════════════════════════════╣");
        println!("║ Agent:      {:44} ║", request.agent_id);
        println!("║ Operation:  {:43} ║", format!("{:?}", request.capability));
        println!("║ Target:     {:44} ║", request.scope);
        println!("║ Details:    {:44} ║", request.description);
        println!("╚════════════════════════════════════════════════════════╝");
        println!();
        print!("Grant permission? [y/N]: ");
        io::stdout().flush()?;

        let result = tokio::task::spawn_blocking(move || {
            let mut input = String::new();
            io::stdin().read_line(&mut input).map(|_| input)
        })
        .await
        .unwrap_or_else(|_| Ok(String::new()))
        .ok()
        .unwrap_or_default();

        let decision = match result.trim().to_lowercase().as_str() {
            "y" | "yes" => ApprovalDecision::Granted,
            "" => ApprovalDecision::TimedOut,
            _ => ApprovalDecision::Denied,
        };

        Ok(decision)
    }
}
