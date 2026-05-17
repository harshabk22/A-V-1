use ageex_wire::codec::wire;
use ageex_wire::{
    AirgapError, AirgapRequest, AirgapResponse, AirgapResult, CapOp,
};
use crate::capabilities::Capability;
use crate::execution::{ExecutionRequest, ExecutionResult, DelExecutor};

/// Single entry point for all WASM agent capability requests.
/// Routes through DEL: Gate1 → Gate2 → Gate3 → Journal.
pub fn handle_airgap_request(req: AirgapRequest) -> AirgapResponse {
    let result = match req.op {
        CapOp::FileRead => handle_file_read(&req),
        CapOp::FileWrite => handle_file_write(&req),
        CapOp::ProcessSpawn => handle_process_spawn(&req),
        CapOp::AppLaunch => handle_app_launch(&req),
        CapOp::MemorySearch => handle_memory_search(&req),
        CapOp::MemoryStore => handle_memory_store(&req),
        _ => AirgapResult::Error(AirgapError::InvalidOp),
    };

    AirgapResponse {
        agent_id: req.agent_id,
        seq: req.seq,
        op: req.op,
        result,
    }
}

fn agent_id_str(agent_id: &[u8; 32]) -> String {
    let mut s = String::with_capacity(24);
    s.push_str("wasm_");
    for byte in &agent_id[..8] {
        s.push_str(&format!("{:02x}", byte));
    }
    s
}

fn del_executor() -> Result<DelExecutor, AirgapError> {
    DelExecutor::new().map_err(|_| AirgapError::HostError)
}

fn map_del_result(result: ExecutionResult) -> AirgapResult {
    if result.success {
        AirgapResult::Success(result.output.into_bytes())
    } else {
        let denial_json = serde_json::to_vec(&result).unwrap_or_default();
        AirgapResult::Success(denial_json)
    }
}

fn handle_file_read(req: &AirgapRequest) -> AirgapResult {
    let path = match decode_file_read_path(&req.payload) {
        Ok(p) => p,
        Err(_) => return AirgapResult::Error(AirgapError::MalformedPayload),
    };

    let del = match del_executor() {
        Ok(d) => d,
        Err(e) => return AirgapResult::Error(e),
    };

    let exec_req = ExecutionRequest::new(
        &agent_id_str(&req.agent_id),
        "fs_read",
        Capability::FileRead,
        serde_json::json!({"path": path}),
    );

    map_del_result(del.execute(exec_req))
}

fn handle_file_write(req: &AirgapRequest) -> AirgapResult {
    let parsed = match wire::parse_request(&req.payload) {
        Some(wire::ParsedRequest::FileWrite { path, data }) => {
            let path_str = String::from_utf8_lossy(path).to_string();
            let content = String::from_utf8_lossy(data).to_string();
            Some((path_str, content))
        }
        _ => None,
    };

    let (path, content) = match parsed {
        Some(p) => p,
        None => return AirgapResult::Error(AirgapError::MalformedPayload),
    };

    let del = match del_executor() {
        Ok(d) => d,
        Err(e) => return AirgapResult::Error(e),
    };

    let exec_req = ExecutionRequest::new(
        &agent_id_str(&req.agent_id),
        "fs_write",
        Capability::FileWrite,
        serde_json::json!({"path": path, "content": content}),
    );

    map_del_result(del.execute(exec_req))
}

fn handle_process_spawn(req: &AirgapRequest) -> AirgapResult {
    let parsed = match wire::parse_request(&req.payload) {
        Some(wire::ParsedRequest::ProcessSpawn { executable, args }) => {
            let exec = String::from_utf8_lossy(executable).to_string();
            let args_vec: Vec<String> = args
                .iter()
                .map(|a| String::from_utf8_lossy(a).to_string())
                .collect();
            Some((exec, args_vec))
        }
        _ => None,
    };

    let (executable, args) = match parsed {
        Some(p) => p,
        None => return AirgapResult::Error(AirgapError::MalformedPayload),
    };

    let del = match del_executor() {
        Ok(d) => d,
        Err(e) => return AirgapResult::Error(e),
    };

    let command = if args.is_empty() {
        executable
    } else {
        format!("{} {}", executable, args.join(" "))
    };

    let exec_req = ExecutionRequest::new(
        &agent_id_str(&req.agent_id),
        "process_spawn",
        Capability::OsProcessSpawn,
        serde_json::json!({"command": command}),
    );

    map_del_result(del.execute(exec_req))
}

fn handle_app_launch(req: &AirgapRequest) -> AirgapResult {
    let app_name = match decode_app_launch(&req.payload) {
        Ok(p) => p,
        Err(_) => return AirgapResult::Error(AirgapError::MalformedPayload),
    };

    let del = match del_executor() {
        Ok(d) => d,
        Err(e) => return AirgapResult::Error(e),
    };

    let exec_req = ExecutionRequest::new(
        &agent_id_str(&req.agent_id),
        "app_open",
        Capability::AppOpen,
        serde_json::json!({"app_name": app_name}),
    );

    map_del_result(del.execute(exec_req))
}

fn handle_memory_search(req: &AirgapRequest) -> AirgapResult {
    let query = match decode_memory_search_query(&req.payload) {
        Ok(q) => q,
        Err(_) => return AirgapResult::Error(AirgapError::MalformedPayload),
    };

    AirgapResult::Success(
        format!("memory_search: {}", query).into_bytes(),
    )
}

fn handle_memory_store(req: &AirgapRequest) -> AirgapResult {
    let (_key, _value) = match decode_memory_store_pair(&req.payload) {
        Ok(p) => p,
        Err(_) => return AirgapResult::Error(AirgapError::MalformedPayload),
    };

    AirgapResult::Success(b"stored".to_vec())
}

fn decode_file_read_path(payload: &[u8]) -> Result<String, ()> {
    let parsed = wire::parse_request(payload).ok_or(())?;
    match parsed {
        wire::ParsedRequest::FileRead { path } => {
            Ok(String::from_utf8_lossy(path).to_string())
        }
        _ => Err(()),
    }
}

fn decode_app_launch(payload: &[u8]) -> Result<String, ()> {
    let parsed = wire::parse_request(payload).ok_or(())?;
    match parsed {
        wire::ParsedRequest::AppLaunch { app_name } => {
            Ok(String::from_utf8_lossy(app_name).to_string())
        }
        _ => Err(()),
    }
}

fn decode_memory_search_query(payload: &[u8]) -> Result<String, ()> {
    let parsed = wire::parse_request(payload).ok_or(())?;
    match parsed {
        wire::ParsedRequest::MemorySearch { query, .. } => {
            Ok(String::from_utf8_lossy(query).to_string())
        }
        _ => Err(()),
    }
}

fn decode_memory_store_pair(payload: &[u8]) -> Result<(String, String), ()> {
    let parsed = wire::parse_request(payload).ok_or(())?;
    match parsed {
        wire::ParsedRequest::MemoryStore { key, value } => {
            Ok((
                String::from_utf8_lossy(key).to_string(),
                String::from_utf8_lossy(value).to_string(),
            ))
        }
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ageex_wire::AirgapResult;

    #[test]
    fn test_decode_file_read_path_valid() {
        let mut buf = [0u8; 256];
        let n = wire::encode_file_read(&mut buf, b"/tmp/test.txt").unwrap();
        let payload = &buf[..n];
        let path = decode_file_read_path(payload).unwrap();
        assert_eq!(path, "/tmp/test.txt");
    }

    #[test]
    fn test_decode_file_read_path_malformed() {
        assert!(decode_file_read_path(&[0xFF, 0x00]).is_err());
    }

    #[test]
    fn test_handle_airgap_request_invalid_op() {
        let req = AirgapRequest {
            agent_id: [0u8; 32],
            seq: 1,
            op: CapOp::NetworkOutbound,
            payload: vec![],
        };
        let resp = handle_airgap_request(req);
        assert_eq!(resp.seq, 1);
        assert!(matches!(resp.result, AirgapResult::Error(AirgapError::InvalidOp)));
    }

    #[test]
    fn test_handle_airgap_request_malformed_file_read() {
        let req = AirgapRequest {
            agent_id: [0u8; 32],
            seq: 2,
            op: CapOp::FileRead,
            payload: b"bad payload".to_vec(),
        };
        let resp = handle_airgap_request(req);
        assert!(matches!(resp.result, AirgapResult::Error(AirgapError::MalformedPayload)));
    }

    #[test]
    fn test_airgap_response_encode_decode_roundtrip_success() {
        let agent_id = [1u8; 32];
        let resp = AirgapResponse {
            agent_id,
            seq: 42,
            op: CapOp::FileRead,
            result: AirgapResult::Success(b"hello world".to_vec()),
        };
        let encoded = resp.encode_response();
        let decoded = AirgapResponse::decode_from_wasm(agent_id, 42, CapOp::FileRead, &encoded);
        assert!(decoded.is_some());
        let decoded = decoded.unwrap();
        assert_eq!(decoded.agent_id, agent_id);
        assert_eq!(decoded.seq, 42);
        assert_eq!(decoded.op, CapOp::FileRead);
        assert_eq!(decoded.result, AirgapResult::Success(b"hello world".to_vec()));
    }
}
