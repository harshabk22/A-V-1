use ageex_reasoning::ReasoningEngine;
use ageex_wire::{AirgapResponse, AirgapResult, AirgapError};
use std::path::PathBuf;

fn example_agent_wasm() -> Vec<u8> {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let wasm_path = crate_dir.join("../../wasm/target/wasm32-wasip1/debug/example_agent.wasm");
    if !wasm_path.exists() {
        let wasm_manifest = crate_dir.join("../../wasm/Cargo.toml");
        let status = std::process::Command::new("cargo")
            .args(["build", "--manifest-path", wasm_manifest.to_str().unwrap()])
            .status();
        let _ = status;
    }
    std::fs::read(&wasm_path).expect(&format!(
        "example_agent.wasm not found at {:?}. Build it first: cd wasm && cargo build",
        wasm_path
    ))
}

#[tokio::test]
async fn test_round_trip_agent_step_with_mock_del() {
    let wasm_bytes = example_agent_wasm();
    let (engine, rx) = ReasoningEngine::new().expect("engine creation");

    let poller = tokio::spawn(async move {
        let mut rx = rx;
        while let Some((envelope, response_tx)) = rx.recv().await {
            let resp = AirgapResponse {
                agent_id: envelope.agent_id,
                seq: envelope.seq,
                op: envelope.op,
                result: AirgapResult::Success(b"mock: /etc/hosts content".to_vec()),
            };
            let _ = response_tx.send(resp);
        }
    });

    let agent_id = [0xAAu8; 32];
    let mut instance = engine
        .instantiate(&wasm_bytes, agent_id)
        .await
        .expect("instantiation");

    let outcome = instance
        .call_step(Some(1_000_000))
        .await
        .expect("agent_step");

    poller.abort();

    assert!(outcome.agent_yielded, "agent should have yielded");
    let yield_msg = String::from_utf8_lossy(&outcome.yield_message);
    assert!(
        yield_msg.contains("FileRead success"),
        "unexpected yield message: {}",
        yield_msg
    );
    assert!(
        yield_msg.contains("24 bytes"),
        "expected 24 bytes in yield message: {}",
        yield_msg
    );
}

#[tokio::test]
async fn test_round_trip_agent_step_malformed_returns_error() {
    let wasm_bytes = example_agent_wasm();
    let (engine, rx) = ReasoningEngine::new().expect("engine creation");

    let poller = tokio::spawn(async move {
        let mut rx = rx;
        while let Some((envelope, response_tx)) = rx.recv().await {
            let resp = AirgapResponse {
                agent_id: envelope.agent_id,
                seq: envelope.seq,
                op: envelope.op,
                result: AirgapResult::Error(AirgapError::PermissionDenied),
            };
            let _ = response_tx.send(resp);
        }
    });

    let agent_id = [0xBBu8; 32];
    let mut instance = engine
        .instantiate(&wasm_bytes, agent_id)
        .await
        .expect("instantiation");

    let outcome = instance
        .call_step(Some(1_000_000))
        .await
        .expect("agent_step");

    poller.abort();

    assert!(outcome.agent_yielded, "agent should have yielded");
    let yield_msg = String::from_utf8_lossy(&outcome.yield_message);
    assert!(
        yield_msg.contains("FileRead error"),
        "unexpected yield message: {}",
        yield_msg
    );
}
