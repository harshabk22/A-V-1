use moat_core::System;
use std::io::{self, Write};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("╔══════════════════════════════════════════╗");
    println!("║    Moat Core Interactive Shell           ║");
    println!("╚══════════════════════════════════════════╝");
    println!("Commands:  /quit  /new  /sessions  /help");
    println!();

    let mut system = System::new()?;

    println!("── System ready ──────────────────────────────");
    println!("Waiting for your input...\n");

    let mut session_used = false;

    loop {
        if session_used {
            print!("\n>> ");
        } else {
            print!(">> ");
            session_used = true;
        }
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim().to_string();

        if input.is_empty() {
            continue;
        }

        match input.as_str() {
            "/quit" => {
                println!("\nGoodbye.");
                break;
            }
            "/help" => {
                println!("Available commands:");
                println!("  /quit    — Exit the shell");
                println!("  /new     — Start a fresh session (ignores history)");
                println!("  /sessions — List active agent sessions");
                println!();
                println!("Just type your question or instruction — the system will");
                println!("route it to Agent1 (read-only) or Agent2 (executor) automatically.");
                println!();
                println!("Agent1 can: read files, check existence, open apps/URLs, delegate tasks");
                println!("Agent2 can: read/write/delete files, run commands, execute skills");
                continue;
            }
            "/new" => {
                system = System::new_session()?;
                println!("── New session started ─────────────────────");
                continue;
            }
            "/sessions" => {
                println!("Session display not yet implemented. Use /new for fresh sessions.");
                continue;
            }
            _ => {}
        }

        match system.process_user_query(&input).await {
            Ok(response) => println!("{}", response),
            Err(e) => println!("System error: {}", e),
        }
    }

    Ok(())
}
