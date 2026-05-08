use crate::error::AirgapError;
use crate::ops::CapOp;

/// Wire format tag for the request type. First byte of every request.
///
/// Layout per capability operation:
///
/// ```text
/// FileRead(0x01):
///   [0x01][path_len: u16 LE][path: u8×path_len]
///
/// FileWrite(0x02):
///   [0x02][path_len: u16 LE][path: u8×path_len][data_len: u32 LE][data: u8×data_len]
///
/// ProcessSpawn(0x03):
///   [0x03][exec_len: u16 LE][exec: u8×exec_len][argc: u8]
///         [arg0_len: u16 LE][arg0: u8×arg0_len]...
///
/// NetworkOutbound(0x04):
///   [0x04][method: u8][host_len: u16 LE][host: u8×host_len]
///         [port: u16 LE][path_len: u16 LE][path: u8×path_len]
///         [body_len: u32 LE][body: u8×body_len]
///
/// AppLaunch(0x05):
///   [0x05][app_len: u16 LE][app: u8×app_len]
///
/// MemorySearch(0x06):
///   [0x06][query_len: u16 LE][query: u8×query_len][max_results: u32 LE]
///
/// MemoryStore(0x07):
///   [0x07][key_len: u16 LE][key: u8×key_len][val_len: u32 LE][val: u8×val_len]
///
/// DelegateTask(0x08):
///   [0x08][agent_id_len: u16 LE][agent_id: u8×agent_id_len]
///         [task_len: u32 LE][task: u8×task_len]
///
/// ConsentRequest(0x09):
///   [0x09][context_len: u16 LE][context: u8×context_len]
///         [options_len: u16 LE][options: u8×options_len]
///
/// YieldDone(0x0A):
///   [0x0A][msg_len: u16 LE][msg: u8×msg_len]
/// ```
pub mod wire {
    use super::*;

    const U16_LEN: usize = core::mem::size_of::<u16>();
    const U32_LEN: usize = core::mem::size_of::<u32>();

    fn read_u16_le(src: &[u8], pos: &mut usize) -> Option<u16> {
        let end = pos.checked_add(U16_LEN)?;
        if end > src.len() {
            return None;
        }
        let val = u16::from_le_bytes([src[*pos], src[*pos + 1]]);
        *pos = end;
        Some(val)
    }

    fn read_u32_le(src: &[u8], pos: &mut usize) -> Option<u32> {
        let end = pos.checked_add(U32_LEN)?;
        if end > src.len() {
            return None;
        }
        let val = u32::from_le_bytes([src[*pos], src[*pos + 1], src[*pos + 2], src[*pos + 3]]);
        *pos = end;
        Some(val)
    }

    fn read_bytes<'a>(src: &'a [u8], pos: &mut usize, len: usize) -> Option<&'a [u8]> {
        let end = pos.checked_add(len)?;
        if end > src.len() {
            return None;
        }
        let slice = &src[*pos..end];
        *pos = end;
        Some(slice)
    }

    /// Parsed capability request from the wire. Lifetime is tied to the source buffer.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ParsedRequest<'a> {
        FileRead { path: &'a [u8] },
        FileWrite { path: &'a [u8], data: &'a [u8] },
        ProcessSpawn { executable: &'a [u8], args: alloc::vec::Vec<&'a [u8]> },
        NetworkOutbound { method: u8, host: &'a [u8], port: u16, path: &'a [u8], body: &'a [u8] },
        AppLaunch { app_name: &'a [u8] },
        MemorySearch { query: &'a [u8], max_results: u32 },
        MemoryStore { key: &'a [u8], value: &'a [u8] },
        DelegateTask { agent_id: &'a [u8], task: &'a [u8] },
        ConsentRequest { context: &'a [u8], options: &'a [u8] },
        YieldDone { message: &'a [u8] },
    }

    impl<'a> ParsedRequest<'a> {
        pub fn op(&self) -> CapOp {
            match self {
                Self::FileRead { .. } => CapOp::FileRead,
                Self::FileWrite { .. } => CapOp::FileWrite,
                Self::ProcessSpawn { .. } => CapOp::ProcessSpawn,
                Self::NetworkOutbound { .. } => CapOp::NetworkOutbound,
                Self::AppLaunch { .. } => CapOp::AppLaunch,
                Self::MemorySearch { .. } => CapOp::MemorySearch,
                Self::MemoryStore { .. } => CapOp::MemoryStore,
                Self::DelegateTask { .. } => CapOp::DelegateTask,
                Self::ConsentRequest { .. } => CapOp::ConsentRequest,
                Self::YieldDone { .. } => CapOp::YieldDone,
            }
        }

        pub fn op_str(&self) -> &'static str {
            match self {
                Self::FileRead { .. } => "FileRead",
                Self::FileWrite { .. } => "FileWrite",
                Self::ProcessSpawn { .. } => "ProcessSpawn",
                Self::NetworkOutbound { .. } => "NetworkOutbound",
                Self::AppLaunch { .. } => "AppLaunch",
                Self::MemorySearch { .. } => "MemorySearch",
                Self::MemoryStore { .. } => "MemoryStore",
                Self::DelegateTask { .. } => "DelegateTask",
                Self::ConsentRequest { .. } => "ConsentRequest",
                Self::YieldDone { .. } => "YieldDone",
            }
        }
    }

    /// Decode a capability request from raw bytes. Returns `None` if the data is
    /// structurally invalid (truncated, unknown opcode, negative lengths).
    pub fn parse_request(raw: &[u8]) -> Option<ParsedRequest<'_>> {
        let mut pos: usize = 0;
        let tag = *raw.get(pos)?;
        pos += 1;

        let _op = CapOp::from_byte_tag(tag)?;

        match tag {
            0x01 => {
                let path_len = read_u16_le(raw, &mut pos)? as usize;
                let path = read_bytes(raw, &mut pos, path_len)?;
                Some(ParsedRequest::FileRead { path })
            }
            0x02 => {
                let path_len = read_u16_le(raw, &mut pos)? as usize;
                let path = read_bytes(raw, &mut pos, path_len)?;
                let data_len = read_u32_le(raw, &mut pos)? as usize;
                let data = read_bytes(raw, &mut pos, data_len)?;
                Some(ParsedRequest::FileWrite { path, data })
            }
            0x03 => {
                let exec_len = read_u16_le(raw, &mut pos)? as usize;
                let executable = read_bytes(raw, &mut pos, exec_len)?;
                let argc = *raw.get(pos)? as usize;
                pos += 1;
                let mut args = alloc::vec::Vec::with_capacity(argc);
                for _ in 0..argc {
                    let arg_len = read_u16_le(raw, &mut pos)? as usize;
                    let arg = read_bytes(raw, &mut pos, arg_len)?;
                    args.push(arg);
                }
                Some(ParsedRequest::ProcessSpawn { executable, args })
            }
            0x04 => {
                let method = *raw.get(pos)?;
                pos += 1;
                let host_len = read_u16_le(raw, &mut pos)? as usize;
                let host = read_bytes(raw, &mut pos, host_len)?;
                let port = read_u16_le(raw, &mut pos)?;
                let path_len = read_u16_le(raw, &mut pos)? as usize;
                let path = read_bytes(raw, &mut pos, path_len)?;
                let body_len = read_u32_le(raw, &mut pos)? as usize;
                let body = read_bytes(raw, &mut pos, body_len)?;
                Some(ParsedRequest::NetworkOutbound {
                    method,
                    host,
                    port,
                    path,
                    body,
                })
            }
            0x05 => {
                let app_len = read_u16_le(raw, &mut pos)? as usize;
                let app_name = read_bytes(raw, &mut pos, app_len)?;
                Some(ParsedRequest::AppLaunch { app_name })
            }
            0x06 => {
                let query_len = read_u16_le(raw, &mut pos)? as usize;
                let query = read_bytes(raw, &mut pos, query_len)?;
                let max_results = read_u32_le(raw, &mut pos)?;
                Some(ParsedRequest::MemorySearch { query, max_results })
            }
            0x07 => {
                let key_len = read_u16_le(raw, &mut pos)? as usize;
                let key = read_bytes(raw, &mut pos, key_len)?;
                let val_len = read_u32_le(raw, &mut pos)? as usize;
                let value = read_bytes(raw, &mut pos, val_len)?;
                Some(ParsedRequest::MemoryStore { key, value })
            }
            0x08 => {
                let agent_id_len = read_u16_le(raw, &mut pos)? as usize;
                let agent_id = read_bytes(raw, &mut pos, agent_id_len)?;
                let task_len = read_u32_le(raw, &mut pos)? as usize;
                let task = read_bytes(raw, &mut pos, task_len)?;
                Some(ParsedRequest::DelegateTask { agent_id, task })
            }
            0x09 => {
                let context_len = read_u16_le(raw, &mut pos)? as usize;
                let context = read_bytes(raw, &mut pos, context_len)?;
                let options_len = read_u16_le(raw, &mut pos)? as usize;
                let options = read_bytes(raw, &mut pos, options_len)?;
                Some(ParsedRequest::ConsentRequest { context, options })
            }
            0x0A => {
                let msg_len = read_u16_le(raw, &mut pos)? as usize;
                let message = read_bytes(raw, &mut pos, msg_len)?;
                Some(ParsedRequest::YieldDone { message })
            }
            _ => None,
        }
    }

    // ─── Encoding (no_std, writes into caller-provided buffer) ──────────────

    fn write_u16_le(buf: &mut [u8], pos: &mut usize, val: u16) -> Result<(), AirgapError> {
        let end = pos.checked_add(U16_LEN).ok_or(AirgapError::BufferOverflow)?;
        if end > buf.len() {
            return Err(AirgapError::BufferOverflow);
        }
        let bytes = val.to_le_bytes();
        buf[*pos] = bytes[0];
        buf[*pos + 1] = bytes[1];
        *pos = end;
        Ok(())
    }

    fn write_u32_le(buf: &mut [u8], pos: &mut usize, val: u32) -> Result<(), AirgapError> {
        let end = pos.checked_add(U32_LEN).ok_or(AirgapError::BufferOverflow)?;
        if end > buf.len() {
            return Err(AirgapError::BufferOverflow);
        }
        let bytes = val.to_le_bytes();
        buf[*pos] = bytes[0];
        buf[*pos + 1] = bytes[1];
        buf[*pos + 2] = bytes[2];
        buf[*pos + 3] = bytes[3];
        *pos = end;
        Ok(())
    }

    fn write_bytes(
        buf: &mut [u8],
        pos: &mut usize,
        data: &[u8],
    ) -> Result<(), AirgapError> {
        let end = pos.checked_add(data.len()).ok_or(AirgapError::BufferOverflow)?;
        if end > buf.len() {
            return Err(AirgapError::BufferOverflow);
        }
        buf[*pos..end].copy_from_slice(data);
        *pos = end;
        Ok(())
    }

    /// Maximum size needed for a FileRead request with the given path.
    pub fn size_file_read(path: &[u8]) -> usize {
        1 + U16_LEN + path.len()
    }

    /// Maximum size needed for a FileWrite request.
    pub fn size_file_write(path: &[u8], data: &[u8]) -> usize {
        1 + U16_LEN + path.len() + U32_LEN + data.len()
    }

    /// Maximum size needed for a ProcessSpawn request.
    pub fn size_process_spawn(exec: &[u8], args: &[&[u8]]) -> usize {
        let mut n = 1 + U16_LEN + exec.len() + 1;
        for arg in args {
            n += U16_LEN + arg.len();
        }
        n
    }

    /// Maximum size needed for a NetworkOutbound request.
    pub fn size_network_outbound(
        host: &[u8],
        path: &[u8],
        body: &[u8],
    ) -> usize {
        1 + 1 + U16_LEN + host.len() + U16_LEN + U16_LEN + path.len() + U32_LEN + body.len()
    }

    pub fn size_app_launch(app_name: &[u8]) -> usize {
        1 + U16_LEN + app_name.len()
    }

    pub fn size_memory_search(query: &[u8]) -> usize {
        1 + U16_LEN + query.len() + U32_LEN
    }

    pub fn size_memory_store(key: &[u8], value: &[u8]) -> usize {
        1 + U16_LEN + key.len() + U32_LEN + value.len()
    }

    pub fn size_delegate_task(agent_id: &[u8], task: &[u8]) -> usize {
        1 + U16_LEN + agent_id.len() + U32_LEN + task.len()
    }

    pub fn size_consent_request(context: &[u8], options: &[u8]) -> usize {
        1 + U16_LEN + context.len() + U16_LEN + options.len()
    }

    pub fn size_yield_done(message: &[u8]) -> usize {
        1 + U16_LEN + message.len()
    }

    // ── Encoding functions ──────────────────────────────────────────────

    pub fn encode_file_read(buf: &mut [u8], path: &[u8]) -> Result<usize, AirgapError> {
        buf[0] = CapOp::FileRead.byte_tag();
        let mut pos: usize = 1;
        write_u16_le(buf, &mut pos, path.len() as u16)?;
        write_bytes(buf, &mut pos, path)?;
        Ok(pos)
    }

    pub fn encode_file_write(buf: &mut [u8], path: &[u8], data: &[u8]) -> Result<usize, AirgapError> {
        buf[0] = CapOp::FileWrite.byte_tag();
        let mut pos: usize = 1;
        write_u16_le(buf, &mut pos, path.len() as u16)?;
        write_bytes(buf, &mut pos, path)?;
        write_u32_le(buf, &mut pos, data.len() as u32)?;
        write_bytes(buf, &mut pos, data)?;
        Ok(pos)
    }

    pub fn encode_process_spawn(
        buf: &mut [u8],
        executable: &[u8],
        args: &[&[u8]],
    ) -> Result<usize, AirgapError> {
        buf[0] = CapOp::ProcessSpawn.byte_tag();
        let mut pos: usize = 1;
        write_u16_le(buf, &mut pos, executable.len() as u16)?;
        write_bytes(buf, &mut pos, executable)?;
        let argc = args.len().min(255);
        if pos >= buf.len() {
            return Err(AirgapError::BufferOverflow);
        }
        buf[pos] = argc as u8;
        pos += 1;
        for arg in args.iter().take(255) {
            write_u16_le(buf, &mut pos, arg.len() as u16)?;
            write_bytes(buf, &mut pos, arg)?;
        }
        Ok(pos)
    }

    pub fn encode_network_outbound(
        buf: &mut [u8],
        method: u8,
        host: &[u8],
        port: u16,
        path: &[u8],
        body: &[u8],
    ) -> Result<usize, AirgapError> {
        buf[0] = CapOp::NetworkOutbound.byte_tag();
        let mut pos: usize = 1;
        if pos >= buf.len() {
            return Err(AirgapError::BufferOverflow);
        }
        buf[pos] = method;
        pos += 1;
        write_u16_le(buf, &mut pos, host.len() as u16)?;
        write_bytes(buf, &mut pos, host)?;
        write_u16_le(buf, &mut pos, port)?;
        write_u16_le(buf, &mut pos, path.len() as u16)?;
        write_bytes(buf, &mut pos, path)?;
        write_u32_le(buf, &mut pos, body.len() as u32)?;
        write_bytes(buf, &mut pos, body)?;
        Ok(pos)
    }

    pub fn encode_app_launch(buf: &mut [u8], app_name: &[u8]) -> Result<usize, AirgapError> {
        buf[0] = CapOp::AppLaunch.byte_tag();
        let mut pos: usize = 1;
        write_u16_le(buf, &mut pos, app_name.len() as u16)?;
        write_bytes(buf, &mut pos, app_name)?;
        Ok(pos)
    }

    pub fn encode_memory_search(
        buf: &mut [u8],
        query: &[u8],
        max_results: u32,
    ) -> Result<usize, AirgapError> {
        buf[0] = CapOp::MemorySearch.byte_tag();
        let mut pos: usize = 1;
        write_u16_le(buf, &mut pos, query.len() as u16)?;
        write_bytes(buf, &mut pos, query)?;
        write_u32_le(buf, &mut pos, max_results)?;
        Ok(pos)
    }

    pub fn encode_memory_store(buf: &mut [u8], key: &[u8], value: &[u8]) -> Result<usize, AirgapError> {
        buf[0] = CapOp::MemoryStore.byte_tag();
        let mut pos: usize = 1;
        write_u16_le(buf, &mut pos, key.len() as u16)?;
        write_bytes(buf, &mut pos, key)?;
        write_u32_le(buf, &mut pos, value.len() as u32)?;
        write_bytes(buf, &mut pos, value)?;
        Ok(pos)
    }

    pub fn encode_delegate_task(
        buf: &mut [u8],
        agent_id: &[u8],
        task: &[u8],
    ) -> Result<usize, AirgapError> {
        buf[0] = CapOp::DelegateTask.byte_tag();
        let mut pos: usize = 1;
        write_u16_le(buf, &mut pos, agent_id.len() as u16)?;
        write_bytes(buf, &mut pos, agent_id)?;
        write_u32_le(buf, &mut pos, task.len() as u32)?;
        write_bytes(buf, &mut pos, task)?;
        Ok(pos)
    }

    pub fn encode_consent_request(
        buf: &mut [u8],
        context: &[u8],
        options: &[u8],
    ) -> Result<usize, AirgapError> {
        buf[0] = CapOp::ConsentRequest.byte_tag();
        let mut pos: usize = 1;
        write_u16_le(buf, &mut pos, context.len() as u16)?;
        write_bytes(buf, &mut pos, context)?;
        write_u16_le(buf, &mut pos, options.len() as u16)?;
        write_bytes(buf, &mut pos, options)?;
        Ok(pos)
    }

    pub fn encode_yield_done(buf: &mut [u8], message: &[u8]) -> Result<usize, AirgapError> {
        buf[0] = CapOp::YieldDone.byte_tag();
        let mut pos: usize = 1;
        write_u16_le(buf, &mut pos, message.len() as u16)?;
        write_bytes(buf, &mut pos, message)?;
        Ok(pos)
    }

    /// Decode a response payload from the host.
    /// Response format: [err: i32 LE][payload_len: u32 LE][payload: u8×payload_len]
    pub fn parse_response(raw: &[u8]) -> Option<(AirgapError, &[u8])> {
        let mut pos: usize = 0;
        let err_code = i32::from_le_bytes([
            *raw.get(pos)?,
            *raw.get(pos + 1)?,
            *raw.get(pos + 2)?,
            *raw.get(pos + 3)?,
        ]);
        pos += 4;
        let payload_len = read_u32_le(raw, &mut pos)? as usize;
        let payload = read_bytes(raw, &mut pos, payload_len)?;
        Some((AirgapError::from_i32(err_code).unwrap_or(AirgapError::HostError), payload))
    }

    /// Encode a response payload going host → agent.
    pub fn encode_response(buf: &mut [u8], err: AirgapError, payload: &[u8]) -> Result<usize, AirgapError> {
        let needed = 4 + U32_LEN + payload.len();
        if buf.len() < needed {
            return Err(AirgapError::BufferOverflow);
        }
        let err_bytes = err.to_i32().to_le_bytes();
        buf[0..4].copy_from_slice(&err_bytes);
        let mut pos: usize = 4;
        write_u32_le(buf, &mut pos, payload.len() as u32)?;
        write_bytes(buf, &mut pos, payload)?;
        Ok(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::wire::*;
    use crate::error::AirgapError;
    use alloc::vec;

    #[test]
    fn roundtrip_file_read() {
        let path = b"notes/today.md";
        let mut buf = [0u8; 256];
        let n = encode_file_read(&mut buf, path).unwrap();
        let parsed = parse_request(&buf[..n]).unwrap();
        assert_eq!(parsed, ParsedRequest::FileRead { path: path.as_slice() });
    }

    #[test]
    fn roundtrip_file_write() {
        let path = b"out/report.txt";
        let data = b"hello world";
        let mut buf = [0u8; 256];
        let n = encode_file_write(&mut buf, path, data).unwrap();
        let parsed = parse_request(&buf[..n]).unwrap();
        assert_eq!(
            parsed,
            ParsedRequest::FileWrite {
                path: path.as_slice(),
                data: data.as_slice()
            }
        );
    }

    #[test]
    fn roundtrip_process_spawn() {
        let exec = b"/usr/bin/git";
        let args: &[&[u8]] = &[b"log", b"--oneline"];
        let mut buf = [0u8; 512];
        let n = encode_process_spawn(&mut buf, exec, args).unwrap();
        let parsed = parse_request(&buf[..n]).unwrap();
        assert_eq!(
            parsed,
            ParsedRequest::ProcessSpawn {
                executable: exec.as_slice(),
                args: vec![args[0], args[1]]
            }
        );
    }

    #[test]
    fn roundtrip_network_outbound() {
        let host = b"api.example.com";
        let path = b"/v1/data";
        let body = b"{\"key\":\"val\"}";
        let mut buf = [0u8; 512];
        let n = encode_network_outbound(&mut buf, 1, host, 443, path, body).unwrap();
        let parsed = parse_request(&buf[..n]).unwrap();
        assert_eq!(
            parsed,
            ParsedRequest::NetworkOutbound {
                method: 1,
                host: host.as_slice(),
                port: 443,
                path: path.as_slice(),
                body: body.as_slice(),
            }
        );
    }

    #[test]
    fn roundtrip_memory_search() {
        let query = b"meeting notes";
        let mut buf = [0u8; 256];
        let n = encode_memory_search(&mut buf, query, 5).unwrap();
        let parsed = parse_request(&buf[..n]).unwrap();
        assert_eq!(
            parsed,
            ParsedRequest::MemorySearch {
                query: query.as_slice(),
                max_results: 5
            }
        );
    }

    #[test]
    fn roundtrip_memory_store() {
        let key = b"project_alpha";
        let val = b"status: in progress";
        let mut buf = [0u8; 256];
        let n = encode_memory_store(&mut buf, key, val).unwrap();
        let parsed = parse_request(&buf[..n]).unwrap();
        assert_eq!(
            parsed,
            ParsedRequest::MemoryStore {
                key: key.as_slice(),
                value: val.as_slice()
            }
        );
    }

    #[test]
    fn roundtrip_delegate_task() {
        let agent_id = b"coder";
        let task = b"refactor module X";
        let mut buf = [0u8; 256];
        let n = encode_delegate_task(&mut buf, agent_id, task).unwrap();
        let parsed = parse_request(&buf[..n]).unwrap();
        assert_eq!(
            parsed,
            ParsedRequest::DelegateTask {
                agent_id: agent_id.as_slice(),
                task: task.as_slice()
            }
        );
    }

    #[test]
    fn roundtrip_consent_request() {
        let context = b"Allow file write to ~/Documents/report.txt?";
        let options = b"Allow\x00Deny";
        let mut buf = [0u8; 256];
        let n = encode_consent_request(&mut buf, context, options).unwrap();
        let parsed = parse_request(&buf[..n]).unwrap();
        assert_eq!(
            parsed,
            ParsedRequest::ConsentRequest {
                context: context.as_slice(),
                options: options.as_slice()
            }
        );
    }

    #[test]
    fn roundtrip_yield_done() {
        let msg = b"Task completed successfully";
        let mut buf = [0u8; 256];
        let n = encode_yield_done(&mut buf, msg).unwrap();
        let parsed = parse_request(&buf[..n]).unwrap();
        assert_eq!(
            parsed,
            ParsedRequest::YieldDone {
                message: msg.as_slice()
            }
        );
    }

    #[test]
    fn response_roundtrip() {
        let mut buf = [0u8; 256];
        let payload = b"{\"status\":\"ok\"}";
        let n = encode_response(&mut buf, AirgapError::None, payload).unwrap();
        let (err, decoded) = parse_response(&buf[..n]).unwrap();
        assert_eq!(err, AirgapError::None);
        assert_eq!(decoded, payload.as_slice());
    }

    #[test]
    fn response_error() {
        let mut buf = [0u8; 256];
        let n = encode_response(&mut buf, AirgapError::PermissionDenied, b"").unwrap();
        let (err, decoded) = parse_response(&buf[..n]).unwrap();
        assert_eq!(err, AirgapError::PermissionDenied);
        assert!(decoded.is_empty());
    }

    #[test]
    fn rejects_truncated() {
        let raw = [0x01, 0x05, 0x00]; // FileRead with path_len=5 but only 2 bytes
        assert!(parse_request(&raw).is_none());
    }

    #[test]
    fn rejects_unknown_opcode() {
        let raw = [0xFF, 0x00, 0x00];
        assert!(parse_request(&raw).is_none());
    }
}