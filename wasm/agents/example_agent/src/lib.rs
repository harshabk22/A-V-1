#![no_std]

extern crate alloc;

#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

use ageex_agent_sdk::host;
use ageex_wire::types::AirgapResponse;
use ageex_wire::CapOp;

/// WASM agent entry point. Called by the host runtime.
/// Reads a hardcoded file through the airgap → DEL pipeline.
#[no_mangle]
pub extern "C" fn agent_step() {
    let path = b"/etc/hosts\0";
    let path_no_nul = &path[..path.len() - 1];

    let mut resp_buf = [0u8; 4096];

    let status = host::airgap_send_file_read(path_no_nul, &mut resp_buf);

    if status != 0 {
        host::agent_log_error(
            alloc::format!("airgap_send_file_read failed with status {}", status).as_bytes(),
        );
        host::agent_yield_done(b"error: airgap_send failed");
        return;
    }

    let agent_id = [0u8; 32];
    let response = match AirgapResponse::decode_from_wasm(agent_id, 0, CapOp::FileRead, &resp_buf) {
        Some(r) => r,
        None => {
            host::agent_log_error(b"failed to decode airgap response");
            host::agent_yield_done(b"error: decode failed");
            return;
        }
    };

    match response.result {
        ageex_wire::AirgapResult::Success(data) => {
            let msg = alloc::format!(
                "FileRead success: {} bytes (first 100: {:?})",
                data.len(),
                &data[..data.len().min(100)]
            );
            host::agent_log_info(msg.as_bytes());
            host::agent_yield_done(msg.as_bytes());
        }
        ageex_wire::AirgapResult::Error(e) => {
            let msg = alloc::format!("FileRead error: {:?}", e);
            host::agent_log_error(msg.as_bytes());
            host::agent_yield_done(msg.as_bytes());
        }
    }
}
