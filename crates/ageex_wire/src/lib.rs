#![no_std]

extern crate alloc;

pub mod codec;
pub mod error;
pub mod ops;
pub mod types;

pub use error::AirgapError;
pub use ops::CapOp;
pub use types::{AirgapRequest, AirgapResponse, AirgapResult};
