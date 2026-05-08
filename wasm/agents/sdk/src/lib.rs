#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc;

pub mod abi;
pub mod host;

pub use ageex_wire::{AirgapError, CapOp};