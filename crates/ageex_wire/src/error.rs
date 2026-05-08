use core::fmt;

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AirgapError {
    None = 0,
    InvalidOp = -1,
    MalformedPayload = -2,
    BufferOverflow = -3,
    PermissionDenied = -4,
    MemoryOutOfBounds = -5,
    SequenceViolation = -6,
    HostError = -7,
    Timeout = -8,
}

impl AirgapError {
    pub fn to_i32(self) -> i32 {
        self as i32
    }

    pub fn from_i32(v: i32) -> Option<Self> {
        Some(match v {
            0 => Self::None,
            -1 => Self::InvalidOp,
            -2 => Self::MalformedPayload,
            -3 => Self::BufferOverflow,
            -4 => Self::PermissionDenied,
            -5 => Self::MemoryOutOfBounds,
            -6 => Self::SequenceViolation,
            -7 => Self::HostError,
            -8 => Self::Timeout,
            _ => return None,
        })
    }
}

impl fmt::Display for AirgapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => write!(f, "no error"),
            Self::InvalidOp => write!(f, "invalid operation code"),
            Self::MalformedPayload => write!(f, "malformed capability-request payload"),
            Self::BufferOverflow => write!(f, "response buffer overflow"),
            Self::PermissionDenied => write!(f, "capability denied by policy"),
            Self::MemoryOutOfBounds => write!(f, "WASM linear memory access out of bounds"),
            Self::SequenceViolation => write!(f, "sequence number violation"),
            Self::HostError => write!(f, "host-side internal error"),
            Self::Timeout => write!(f, "operation timed out"),
        }
    }
}