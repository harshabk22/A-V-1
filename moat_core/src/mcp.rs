use crate::{MoatError, MoatResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

#[derive(Debug, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: u64,
    pub result: Option<Value>,
    pub error: Option<Value>,
}

pub struct McpClient {
    child: Child,
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    stdout_rx: Arc<Mutex<tokio::sync::mpsc::Receiver<String>>>,
    request_id: AtomicU64,
}

impl McpClient {
    pub async fn new(cmd: &str, args: &[&str]) -> MoatResult<Self> {
        let mut child = Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| MoatError::Orchestrator(format!("Failed to start MCP server: {}", e)))?;

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        let (tx, rx) = tokio::sync::mpsc::channel(100);

        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if tx.send(line).await.is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            child,
            stdin: Arc::new(Mutex::new(stdin)),
            stdout_rx: Arc::new(Mutex::new(rx)),
            request_id: AtomicU64::new(1),
        })
    }

    pub async fn initialize(&self) -> MoatResult<Value> {
        self.call(
            "initialize",
            Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "roots": { "listChanged": true }
                },
                "clientInfo": {
                    "name": "MoatAgeex",
                    "version": "1.0.0"
                }
            })),
        )
        .await
    }

    pub async fn list_tools(&self) -> MoatResult<Value> {
        self.call("tools/list", None).await
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> MoatResult<Value> {
        self.call(
            "tools/call",
            Some(serde_json::json!({
                "name": name,
                "arguments": arguments
            })),
        )
        .await
    }

    async fn call(&self, method: &str, params: Option<Value>) -> MoatResult<Value> {
        let id = self.request_id.fetch_add(1, Ordering::SeqCst);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        };
        let req_str = serde_json::to_string(&req).unwrap() + "\n";

        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(req_str.as_bytes())
                .await
                .map_err(|e| MoatError::Orchestrator(e.to_string()))?;
            stdin
                .flush()
                .await
                .map_err(|e| MoatError::Orchestrator(e.to_string()))?;
        }

        let mut rx = self.stdout_rx.lock().await;
        // Simple synchronous wait for the exact response ID.
        // Note: A real MCP client would multiplex responses.
        while let Some(line) = rx.recv().await {
            if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(&line) {
                if resp.id == id {
                    if let Some(err) = resp.error {
                        return Err(MoatError::Orchestrator(format!("MCP Error: {}", err)));
                    }
                    return Ok(resp.result.unwrap_or(Value::Null));
                }
            }
        }

        Err(MoatError::Orchestrator("MCP Connection closed".into()))
    }
}
