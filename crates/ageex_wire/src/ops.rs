use core::fmt;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapOp {
    FileRead = 0x01,
    FileWrite = 0x02,
    ProcessSpawn = 0x03,
    NetworkOutbound = 0x04,
    AppLaunch = 0x05,
    MemorySearch = 0x06,
    MemoryStore = 0x07,
    DelegateTask = 0x08,
    ConsentRequest = 0x09,
    YieldDone = 0x0A,
}

impl CapOp {
    pub fn from_u32(v: u32) -> Option<Self> {
        Some(match v {
            0x01 => Self::FileRead,
            0x02 => Self::FileWrite,
            0x03 => Self::ProcessSpawn,
            0x04 => Self::NetworkOutbound,
            0x05 => Self::AppLaunch,
            0x06 => Self::MemorySearch,
            0x07 => Self::MemoryStore,
            0x08 => Self::DelegateTask,
            0x09 => Self::ConsentRequest,
            0x0A => Self::YieldDone,
            _ => return None,
        })
    }

    pub fn to_u32(self) -> u32 {
        self as u32
    }

    pub fn byte_tag(self) -> u8 {
        self as u8
    }

    pub fn from_byte_tag(b: u8) -> Option<Self> {
        Self::from_u32(b as u32)
    }
}

impl fmt::Display for CapOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileRead => write!(f, "FileRead(0x01)"),
            Self::FileWrite => write!(f, "FileWrite(0x02)"),
            Self::ProcessSpawn => write!(f, "ProcessSpawn(0x03)"),
            Self::NetworkOutbound => write!(f, "NetworkOutbound(0x04)"),
            Self::AppLaunch => write!(f, "AppLaunch(0x05)"),
            Self::MemorySearch => write!(f, "MemorySearch(0x06)"),
            Self::MemoryStore => write!(f, "MemoryStore(0x07)"),
            Self::DelegateTask => write!(f, "DelegateTask(0x08)"),
            Self::ConsentRequest => write!(f, "ConsentRequest(0x09)"),
            Self::YieldDone => write!(f, "YieldDone(0x0A)"),
        }
    }
}