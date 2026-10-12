use std::future::Future;
use std::sync::atomic::Ordering;

use ageex_wire::codec::wire;
use ageex_wire::AirgapError;
use tokio::sync::oneshot;
use tracing;
use wasmtime::*;

use crate::engine::{AirgapBundle, AirgapEnvelope, StoreData};

const AERR_NONE: i32 = AirgapError::None as i32;
const AERR_INVALID_OP: i32 = AirgapError::InvalidOp as i32;
#[allow(dead_code)]
const AERR_MALFORMED: i32 = AirgapError::MalformedPayload as i32;
const AERR_BUFFER_OVERFLOW: i32 = AirgapError::BufferOverflow as i32;
const AERR_PERMISSION_DENIED: i32 = AirgapError::PermissionDenied as i32;
const AERR_MEMORY_OOB: i32 = AirgapError::MemoryOutOfBounds as i32;
const AERR_HOST_ERROR: i32 = AirgapError::HostError as i32;

// ─── Bounds constants for semantic validation ────────────────────────────

const MAX_PATH_LEN: u32 = 4096;
const MAX_DATA_LEN: u32 = 16 * 1024 * 1024;
const MAX_EXEC_LEN: u32 = 4096;
const MAX_ARGS: u32 = 64;
const MAX_ARG_LEN: u32 = 4096;
const MAX_HOST_LEN: u32 = 256;
const MAX_QUERY_LEN: u32 = 4096;
const MAX_MAX_RESULTS: u32 = 100;
const MAX_KEY_LEN: u32 = 1024;
const MAX_VALUE_LEN: u32 = 1 * 1024 * 1024;
const MAX_AGENT_ID_LEN: u32 = 128;
const MAX_TASK_LEN: u32 = 64 * 1024;
const MAX_CONTEXT_LEN: u32 = 16 * 1024;
const MAX_OPTIONS_LEN: u32 = 16 * 1024;
const MAX_MSG_LEN: u32 = 16 * 1024;

// ─── Helper: get Memory from a Caller. Caller needs &mut for get_export. ─

fn get_memory(caller: &mut Caller<'_, StoreData>) -> Result<Memory, i32> {
    caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or(AERR_HOST_ERROR)
}

// ─── Helper: safe read from WASM linear memory ───────────────────────────

fn try_read_mem(caller: &Caller<'_, StoreData>, mem: &Memory, ptr: u32, len: u32) -> Result<Vec<u8>, i32> {
    if len == 0 {
        return Ok(Vec::new());
    }
    let offset = ptr as usize;
    let end = offset.checked_add(len as usize).ok_or(AERR_MEMORY_OOB)?;
    let total = mem.data_size(caller);
    if end > total {
        return Err(AERR_MEMORY_OOB);
    }
    let mut buf = vec![0u8; len as usize];
    mem.read(caller, offset, &mut buf)
        .map_err(|_| AERR_MEMORY_OOB)?;
    Ok(buf)
}

// ─── Semantic validation ─────────────────────────────────────────────────

fn contains_path_traversal(bytes: &[u8]) -> bool {
    bytes.windows(2).any(|w| w == b"..") || bytes.contains(&0)
}

fn contains_nul(bytes: &[u8]) -> bool {
    bytes.contains(&0)
}

fn semantic_validate(req: &wire::ParsedRequest<'_>) -> Result<(), String> {
    match req {
        wire::ParsedRequest::FileRead { path } => {
            if path.is_empty() {
                return Err("FileRead: empty path".into());
            }
            if path.len() as u32 > MAX_PATH_LEN {
                return Err(format!("FileRead: path exceeds {MAX_PATH_LEN} bytes"));
            }
            if contains_path_traversal(path) {
                return Err(format!("FileRead: path traversal in {:?}", String::from_utf8_lossy(path)));
            }
        }
        wire::ParsedRequest::FileWrite { path, data } => {
            if path.is_empty() {
                return Err("FileWrite: empty path".into());
            }
            if path.len() as u32 > MAX_PATH_LEN {
                return Err(format!("FileWrite: path exceeds {MAX_PATH_LEN} bytes"));
            }
            if contains_path_traversal(path) {
                return Err(format!("FileWrite: path traversal in {:?}", String::from_utf8_lossy(path)));
            }
            if data.len() as u32 > MAX_DATA_LEN {
                return Err(format!("FileWrite: data exceeds {MAX_DATA_LEN} bytes"));
            }
        }
        wire::ParsedRequest::ProcessSpawn { executable, args } => {
            if executable.is_empty() {
                return Err("ProcessSpawn: empty executable".into());
            }
            if executable.len() as u32 > MAX_EXEC_LEN {
                return Err(format!("ProcessSpawn: executable exceeds {MAX_EXEC_LEN} bytes"));
            }
            if contains_nul(executable) {
                return Err("ProcessSpawn: NUL byte in executable".into());
            }
            if contains_path_traversal(executable) {
                return Err(format!(
                    "ProcessSpawn: path traversal in {:?}",
                    String::from_utf8_lossy(executable)
                ));
            }
            if args.len() as u32 > MAX_ARGS {
                return Err(format!("ProcessSpawn: exceeds {MAX_ARGS} args"));
            }
            for (i, arg) in args.iter().enumerate() {
                if arg.len() as u32 > MAX_ARG_LEN {
                    return Err(format!("ProcessSpawn: arg[{i}] exceeds {MAX_ARG_LEN} bytes"));
                }
                if contains_nul(arg) {
                    return Err(format!("ProcessSpawn: NUL byte in arg[{i}]"));
                }
                if contains_path_traversal(arg) {
                    return Err(format!(
                        "ProcessSpawn: path traversal in arg[{i}]: {:?}",
                        String::from_utf8_lossy(arg)
                    ));
                }
            }
        }
        wire::ParsedRequest::NetworkOutbound {
            method,
            host,
            port,
            path,
            body,
        } => {
            if *method > 3 {
                return Err(format!("NetworkOutbound: invalid HTTP method {method}"));
            }
            if host.is_empty() {
                return Err("NetworkOutbound: empty host".into());
            }
            if host.len() as u32 > MAX_HOST_LEN {
                return Err(format!("NetworkOutbound: host exceeds {MAX_HOST_LEN} bytes"));
            }
            if contains_nul(host) {
                return Err("NetworkOutbound: NUL byte in host".into());
            }
            if *port == 0 {
                return Err("NetworkOutbound: port 0 disallowed".into());
            }
            if path.len() as u32 > MAX_PATH_LEN {
                return Err(format!("NetworkOutbound: path exceeds {MAX_PATH_LEN} bytes"));
            }
            if contains_path_traversal(path) {
                return Err(format!("NetworkOutbound: path traversal in {:?}", String::from_utf8_lossy(path)));
            }
            if body.len() as u32 > MAX_DATA_LEN {
                return Err(format!("NetworkOutbound: body exceeds {MAX_DATA_LEN} bytes"));
            }
        }
        wire::ParsedRequest::AppLaunch { app_name } => {
            if app_name.is_empty() {
                return Err("AppLaunch: empty app name".into());
            }
            if app_name.len() as u32 > MAX_HOST_LEN {
                return Err(format!("AppLaunch: name exceeds {MAX_HOST_LEN} bytes"));
            }
            if contains_nul(app_name) {
                return Err("AppLaunch: NUL byte in app name".into());
            }
            if contains_path_traversal(app_name) {
                return Err(format!("AppLaunch: path traversal in {:?}", String::from_utf8_lossy(app_name)));
            }
        }
        wire::ParsedRequest::MemorySearch {
            query,
            max_results,
        } => {
            if query.len() as u32 > MAX_QUERY_LEN {
                return Err(format!("MemorySearch: query exceeds {MAX_QUERY_LEN} bytes"));
            }
            if *max_results > MAX_MAX_RESULTS {
                return Err(format!("MemorySearch: max_results {max_results} > {MAX_MAX_RESULTS}"));
            }
        }
        wire::ParsedRequest::MemoryStore { key, value } => {
            if key.is_empty() {
                return Err("MemoryStore: empty key".into());
            }
            if key.len() as u32 > MAX_KEY_LEN {
                return Err(format!("MemoryStore: key exceeds {MAX_KEY_LEN} bytes"));
            }
            if value.len() as u32 > MAX_VALUE_LEN {
                return Err(format!("MemoryStore: value exceeds {MAX_VALUE_LEN} bytes"));
            }
        }
        wire::ParsedRequest::DelegateTask { agent_id, task } => {
            if agent_id.is_empty() {
                return Err("DelegateTask: empty agent_id".into());
            }
            if agent_id.len() as u32 > MAX_AGENT_ID_LEN {
                return Err(format!("DelegateTask: agent_id exceeds {MAX_AGENT_ID_LEN} bytes"));
            }
            if task.len() as u32 > MAX_TASK_LEN {
                return Err(format!("DelegateTask: task exceeds {MAX_TASK_LEN} bytes"));
            }
        }
        wire::ParsedRequest::ConsentRequest { context, options } => {
            if context.len() as u32 > MAX_CONTEXT_LEN {
                return Err(format!("ConsentRequest: context exceeds {MAX_CONTEXT_LEN} bytes"));
            }
            if options.len() as u32 > MAX_OPTIONS_LEN {
                return Err(format!("ConsentRequest: options exceeds {MAX_OPTIONS_LEN} bytes"));
            }
        }
        wire::ParsedRequest::YieldDone { message } => {
            if message.len() as u32 > MAX_MSG_LEN {
                return Err(format!("YieldDone: message exceeds {MAX_MSG_LEN} bytes"));
            }
        }
    }
    Ok(())
}

// ─── Async processing: sends envelope + oneshot, awaits DEL response ─────

async fn process_airgap_send_async(
    op: u32,
    raw: Vec<u8>,
    agent_id: [u8; 32],
    seq: u64,
    tx: &tokio::sync::mpsc::UnboundedSender<AirgapBundle>,
    mem: &Memory,
    caller: &mut Caller<'_, StoreData>,
    resp_ptr: u32,
    resp_cap: u32,
) -> i32 {
    let parsed = match wire::parse_request(&raw) {
        Some(p) => p,
        None => {
            tracing::warn!(target: "ageex.security", agent_id=?agent_id, seq, "airgap_send: malformed payload");
            return AERR_MALFORMED;
        }
    };

    let declared_op = parsed.op().to_u32();
    if declared_op != op {
        tracing::error!(
            target: "ageex.security",
            agent_id = ?agent_id,
            seq,
            declared_op,
            wire_op = op,
            "airgap_send: opcode mismatch"
        );
        return AERR_INVALID_OP;
    }

    if let Err(violation) = semantic_validate(&parsed) {
        tracing::error!(
            target: "ageex.security",
            agent_id = ?agent_id,
            seq,
            op = %parsed.op_str(),
            violation = %violation,
            "SECURITY VIOLATION: agent attempted illegal capability request"
        );
        if let Ok(mut guard) = caller.data().violations.lock() {
            guard.push(crate::engine::ViolationRecord {
                seq,
                op: parsed.op_str().to_string(),
                violation: violation.clone(),
            });
        }
        return AERR_PERMISSION_DENIED;
    }

    let parsed_op = parsed.op();
    let op_str = parsed.op_str();

    let envelope = AirgapEnvelope {
        agent_id,
        seq,
        op: parsed_op,
        request_bytes: raw,
    };

    let (response_tx, response_rx) = oneshot::channel();
    let bundle: AirgapBundle = (envelope, response_tx);

    if tx.send(bundle).is_err() {
        tracing::error!(target: "ageex.reasoning", agent_id=?agent_id, seq, "airgap channel closed");
        return AERR_HOST_ERROR;
    }

    tracing::debug!(
        target: "ageex.reasoning",
        agent_id = ?agent_id,
        seq,
        op = %op_str,
        "airgap_send: enqueued, awaiting DEL response"
    );

    let response = match response_rx.await {
        Ok(r) => r,
        Err(_) => {
            tracing::error!(target: "ageex.reasoning", agent_id=?agent_id, seq, "oneshot sender dropped");
            return AERR_HOST_ERROR;
        }
    };

    let response_bytes = response.encode_response();

    if (resp_cap as usize) < response_bytes.len() {
        tracing::error!(
            target: "ageex.reasoning",
            agent_id = ?agent_id,
            seq,
            "guest response buffer capacity insufficient: {} < {}",
            resp_cap,
            response_bytes.len()
        );
        return AERR_BUFFER_OVERFLOW;
    }

    if resp_ptr > 0 || !response_bytes.is_empty() {
        if let Err(e) = mem.write(caller, resp_ptr as usize, &response_bytes) {
            tracing::error!(target: "ageex.reasoning", agent_id=?agent_id, seq, "write to WASM memory failed: {:?}", e);
            return AERR_MEMORY_OOB;
        }
    }

    tracing::debug!(
        target: "ageex.reasoning",
        agent_id = ?agent_id,
        seq,
        "airgap_send: response written to WASM memory ({} bytes)",
        response_bytes.len()
    );

    AERR_NONE
}

fn process_mem_request(
    _op: u32,
    raw: &[u8],
    agent_id: [u8; 32],
    seq: u64,
    tx: &tokio::sync::mpsc::UnboundedSender<AirgapBundle>,
) -> i32 {
    let parsed = match wire::parse_request(raw) {
        Some(p) => p,
        None => {
            tracing::warn!(target: "ageex.security", agent_id=?agent_id, seq, "mem_request: malformed payload");
            return AERR_MALFORMED;
        }
    };

    if let Err(violation) = semantic_validate(&parsed) {
        tracing::error!(
            target: "ageex.security",
            agent_id = ?agent_id,
            seq,
            op = %parsed.op_str(),
            violation = %violation,
            "SECURITY VIOLATION: agent attempted illegal memory operation"
        );
        return AERR_PERMISSION_DENIED;
    }

    let envelope = AirgapEnvelope {
        agent_id,
        seq,
        op: parsed.op(),
        request_bytes: raw.to_vec(),
    };

    let (response_tx, _response_rx) = oneshot::channel();
    let bundle: AirgapBundle = (envelope, response_tx);

    let _ = tx.send(bundle);

    AERR_NONE
}

fn process_consent_poll(
    raw: &[u8],
    agent_id: [u8; 32],
    seq: u64,
    tx: &tokio::sync::mpsc::UnboundedSender<AirgapBundle>,
) -> u32 {
    let parsed = match wire::parse_request(raw) {
        Some(p) => p,
        None => {
            tracing::warn!(target: "ageex.security", agent_id=?agent_id, seq, "consent_poll: malformed payload");
            return 0;
        }
    };

    if let Err(violation) = semantic_validate(&parsed) {
        tracing::error!(
            target: "ageex.security",
            agent_id = ?agent_id,
            seq,
            op = %parsed.op_str(),
            violation = %violation,
            "SECURITY VIOLATION: agent attempted illegal consent request"
        );
        return 0;
    }

    let envelope = AirgapEnvelope {
        agent_id,
        seq,
        op: parsed.op(),
        request_bytes: raw.to_vec(),
    };

    let (response_tx, _response_rx) = oneshot::channel();
    let bundle: AirgapBundle = (envelope, response_tx);

    let _ = tx.send(bundle);

    0
}

fn log_agent_message(agent_id: [u8; 32], level: u32, msg_bytes: &[u8]) {
    let msg = String::from_utf8_lossy(msg_bytes);
    let agent_str = format!(
        "{:02x}{:02x}{:02x}...",
        agent_id[0], agent_id[1], agent_id[2]
    );
    match level {
        0 => tracing::debug!(target: "ageex.agent", agent_id = %agent_str, "{msg}"),
        1 => tracing::info!(target: "ageex.agent", agent_id = %agent_str, "{msg}"),
        2 => tracing::warn!(target: "ageex.agent", agent_id = %agent_str, "{msg}"),
        _ => tracing::error!(target: "ageex.agent", agent_id = %agent_str, "{msg}"),
    }
}

// ─── Register all host imports into the linker

/// Register all host imports for the `"ageex"` namespace.
pub fn register_imports(linker: &mut Linker<StoreData>) -> Result<(), Error> {
    // ── __ageex_airgap_send(op:u32, req_ptr:u32, req_len:u32, resp_ptr:u32, resp_cap:u32) → i32 ──

    linker.func_wrap_async(
        "ageex",
        "__ageex_airgap_send",
        |mut caller: Caller<'_, StoreData>,
         (op, req_ptr, req_len, resp_ptr, resp_cap): (u32, u32, u32, u32, u32)|
         -> Box<dyn Future<Output = (i32,)> + Send + '_> {
            let mem = match get_memory(&mut caller) {
                Ok(m) => m,
                Err(e) => return Box::new(std::future::ready((e,))),
            };

            let (agent_id, tx, seq) = {
                let data = caller.data();
                (
                    data.agent_id,
                    data.request_tx.clone(),
                    data.seq_counter.fetch_add(1, Ordering::Relaxed),
                )
            };

            let raw = match try_read_mem(&caller, &mem, req_ptr, req_len) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(target: "ageex.security", agent_id=?agent_id, seq, "airgap_send: OOB read");
                    return Box::new(std::future::ready((e,)));
                }
            };

            Box::new(async move {
                let status = process_airgap_send_async(
                    op, raw, agent_id, seq, &tx, &mem, &mut caller, resp_ptr, resp_cap,
                ).await;
                (status,)
            })
        },
    )?;

    // ── __ageex_mem_request(op, req_ptr, req_len, val_ptr, val_len, buf_ptr, buf_cap) → i32 ──

    linker.func_wrap_async(
        "ageex",
        "__ageex_mem_request",
        |mut caller: Caller<'_, StoreData>,
         (op, req_ptr, req_len, _val_ptr, _val_len, _buf_ptr, _buf_cap): (u32, u32, u32, u32, u32, u32, u32)|
         -> Box<dyn Future<Output = (i32,)> + Send + '_> {
            let mem = match get_memory(&mut caller) {
                Ok(m) => m,
                Err(e) => return Box::new(std::future::ready((e,))),
            };

            let (agent_id, tx, seq) = {
                let data = caller.data();
                (
                    data.agent_id,
                    data.request_tx.clone(),
                    data.seq_counter.fetch_add(1, Ordering::Relaxed),
                )
            };

            let raw = match try_read_mem(&caller, &mem, req_ptr, req_len) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(target: "ageex.security", agent_id=?agent_id, seq, "mem_request: OOB read");
                    return Box::new(std::future::ready((e,)));
                }
            };

            Box::new(async move {
                (process_mem_request(op, &raw, agent_id, seq, &tx),)
            })
        },
    )?;

    // ── __ageex_consent_poll(req_ptr, req_len, timeout_ms) → u32 ──

    linker.func_wrap_async(
        "ageex",
        "__ageex_consent_poll",
        |mut caller: Caller<'_, StoreData>,
         (req_ptr, req_len, _timeout_ms): (u32, u32, u32)|
         -> Box<dyn Future<Output = (u32,)> + Send + '_> {
            let mem = match get_memory(&mut caller) {
                Ok(m) => m,
                Err(_) => return Box::new(std::future::ready((0,))),
            };

            let (agent_id, tx, seq) = {
                let data = caller.data();
                (
                    data.agent_id,
                    data.request_tx.clone(),
                    data.seq_counter.fetch_add(1, Ordering::Relaxed),
                )
            };

            let raw = match try_read_mem(&caller, &mem, req_ptr, req_len) {
                Ok(v) => v,
                Err(_) => return Box::new(std::future::ready((0,))),
            };

            Box::new(async move {
                (process_consent_poll(&raw, agent_id, seq, &tx),)
            })
        },
    )?;

    // ── __ageex_agent_log(level, msg_ptr, msg_len) → () ──

    linker.func_wrap_async(
        "ageex",
        "__ageex_agent_log",
        |mut caller: Caller<'_, StoreData>,
         (level, msg_ptr, msg_len): (u32, u32, u32)|
         -> Box<dyn Future<Output = ()> + Send + '_> {
            let mem = match get_memory(&mut caller) {
                Ok(m) => m,
                Err(_) => return Box::new(std::future::ready(())),
            };

            let agent_id = caller.data().agent_id;

            let msg_bytes = match try_read_mem(&caller, &mem, msg_ptr, msg_len) {
                Ok(v) => v,
                Err(_) => return Box::new(std::future::ready(())),
            };

            Box::new(async move {
                log_agent_message(agent_id, level, &msg_bytes);
            })
        },
    )?;

    // ── __ageex_agent_yield_done(result_ptr, result_len) → u32 ──

    linker.func_wrap_async(
        "ageex",
        "__ageex_agent_yield_done",
        |mut caller: Caller<'_, StoreData>,
         (result_ptr, result_len): (u32, u32)|
         -> Box<dyn Future<Output = (u32,)> + Send + '_> {
            let mem = match get_memory(&mut caller) {
                Ok(m) => m,
                Err(_) => return Box::new(std::future::ready((1,))),
            };

            let msg_bytes = match try_read_mem(&caller, &mem, result_ptr, result_len) {
                Ok(v) => v,
                Err(_) => return Box::new(std::future::ready((1,))),
            };

            {
                let data = caller.data();
                let mut guard = data
                    .yield_message
                    .lock()
                    .unwrap_or_else(|e: std::sync::PoisonError<_>| e.into_inner());
                *guard = msg_bytes;
            }
            caller.data().yield_flag.store(true, Ordering::Release);

            Box::new(std::future::ready((0,)))
        },
    )?;

    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_rejects_path_traversal_in_file_read() {
        let mut buf = [0u8; 256];
        let n = wire::encode_file_read(&mut buf, b"../../etc/passwd").unwrap();
        let parsed = wire::parse_request(&buf[..n]).unwrap();
        let err = semantic_validate(&parsed);
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("path traversal"));
    }

    #[test]
    fn semantic_rejects_invalid_http_method() {
        let mut buf = [0u8; 256];
        let n = wire::encode_network_outbound(&mut buf, 9, b"example.com", 443, b"/", b"").unwrap();
        let parsed = wire::parse_request(&buf[..n]).unwrap();
        let err = semantic_validate(&parsed);
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("HTTP method"));
    }

    #[test]
    fn semantic_rejects_port_zero() {
        let mut buf = [0u8; 256];
        let n = wire::encode_network_outbound(&mut buf, 1, b"example.com", 0, b"/", b"").unwrap();
        let parsed = wire::parse_request(&buf[..n]).unwrap();
        let err = semantic_validate(&parsed);
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("port 0"));
    }

    #[test]
    fn semantic_accepts_valid_file_read() {
        let mut buf = [0u8; 256];
        let n = wire::encode_file_read(&mut buf, b"notes/today.md").unwrap();
        let parsed = wire::parse_request(&buf[..n]).unwrap();
        assert!(semantic_validate(&parsed).is_ok());
    }

    #[test]
    fn semantic_accepts_valid_memory_search() {
        let mut buf = [0u8; 256];
        let n = wire::encode_memory_search(&mut buf, b"meeting", 5).unwrap();
        let parsed = wire::parse_request(&buf[..n]).unwrap();
        assert!(semantic_validate(&parsed).is_ok());
    }

    #[test]
    fn semantic_rejects_nul_in_executable() {
        let mut buf = [0u8; 256];
        let n = wire::encode_process_spawn(&mut buf, b"/bin/ls\0x", &[]).unwrap();
        let parsed = wire::parse_request(&buf[..n]).unwrap();
        let err = semantic_validate(&parsed);
        assert!(err.is_err());
    }
}
