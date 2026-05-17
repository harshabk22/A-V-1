use alloc::vec::Vec;
use crate::error::AirgapError;
use crate::ops::CapOp;

/// Request dispatched from a WASM agent via the airgap channel,
/// consumed by moat_core's DEL pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AirgapRequest {
    pub agent_id: [u8; 32],
    pub seq: u64,
    pub op: CapOp,
    pub payload: Vec<u8>,
}

/// Response sent back to the WASM agent after DEL processing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AirgapResponse {
    pub agent_id: [u8; 32],
    pub seq: u64,
    pub op: CapOp,
    pub result: AirgapResult,
}

/// Outcome of a capability request processed through DEL.
///
/// `Success` carries output bytes: real output (file contents, stdout)
/// or a serialized `ExecutionResult` when the operation was denied.
///
/// `Error` is reserved for host failures (malformed payload, channel
/// closed, unknown opcode).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AirgapResult {
    Success(Vec<u8>),
    Error(AirgapError),
}

impl AirgapResponse {
    /// Encode result into WASM memory at `resp_ptr`.
    ///
    /// Layout: `[tag:u8 (0=Success,1=Error)][err_code:i32 LE][len:u32 LE][payload:u8×len]`
    pub fn encode_response(&self) -> Vec<u8> {
        match &self.result {
            AirgapResult::Success(bytes) => {
                let mut out = Vec::with_capacity(9 + bytes.len());
                out.push(0);                                    // tag: success
                out.extend_from_slice(&0i32.to_le_bytes());     // err_code: 0
                out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                out.extend_from_slice(bytes);
                out
            }
            AirgapResult::Error(e) => {
                let msg = alloc::format!("{}", e).into_bytes();
                let mut out = Vec::with_capacity(9 + msg.len());
                out.push(1);                                    // tag: error
                out.extend_from_slice(&e.to_i32().to_le_bytes()); // err_code
                out.extend_from_slice(&(msg.len() as u32).to_le_bytes());
                out.extend_from_slice(&msg);
                out
            }
        }
    }

    /// Decode a response from the format produced by `encode_response`.
    pub fn decode_from_wasm(
        agent_id: [u8; 32],
        seq: u64,
        op: CapOp,
        raw: &[u8],
    ) -> Option<Self> {
        if raw.len() < 9 {
            return None;
        }
        let tag = raw[0];
        let _err_code = i32::from_le_bytes(raw[1..5].try_into().ok()?);
        let payload_len = u32::from_le_bytes(raw[5..9].try_into().ok()?) as usize;
        if raw.len() < 9 + payload_len {
            return None;
        }
        let result = match tag {
            0 => AirgapResult::Success(raw[9..9 + payload_len].to_vec()),
            1 => AirgapResult::Error(AirgapError::from_i32(_err_code).unwrap_or(AirgapError::HostError)),
            _ => return None,
        };
        Some(AirgapResponse { agent_id, seq, op, result })
    }
}
