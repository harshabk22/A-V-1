use crate::abi;
use ageex_wire::codec::wire;

pub fn airgap_send(req: &[u8], resp: &mut [u8]) -> i32 {
    if req.is_empty() {
        return -2;
    }
    let op = req[0] as u32;
    unsafe {
        abi::__ageex_airgap_send(op, req.as_ptr(), req.len() as u32, resp.as_mut_ptr(), resp.len() as u32)
    }
}

pub fn airgap_send_file_read(path: &[u8], resp: &mut [u8]) -> i32 {
    let mut buf = [0u8; 4096];
    match wire::encode_file_read(&mut buf, path) {
        Ok(n) => unsafe {
            abi::__ageex_airgap_send(0x01, buf.as_ptr(), n as u32, resp.as_mut_ptr(), resp.len() as u32)
        },
        Err(_) => -3,
    }
}

pub fn airgap_send_file_write(path: &[u8], data: &[u8], resp: &mut [u8]) -> i32 {
    let mut buf = [0u8; 8192];
    match wire::encode_file_write(&mut buf, path, data) {
        Ok(n) => unsafe {
            abi::__ageex_airgap_send(0x02, buf.as_ptr(), n as u32, resp.as_mut_ptr(), resp.len() as u32)
        },
        Err(_) => -3,
    }
}

pub fn airgap_send_process_spawn(executable: &[u8], args: &[&[u8]], resp: &mut [u8]) -> i32 {
    let mut buf = [0u8; 4096];
    match wire::encode_process_spawn(&mut buf, executable, args) {
        Ok(n) => unsafe {
            abi::__ageex_airgap_send(0x03, buf.as_ptr(), n as u32, resp.as_mut_ptr(), resp.len() as u32)
        },
        Err(_) => -3,
    }
}

pub fn airgap_send_network_outbound(
    method: u8,
    host: &[u8],
    port: u16,
    path: &[u8],
    body: &[u8],
    resp: &mut [u8],
) -> i32 {
    let mut buf = [0u8; 8192];
    match wire::encode_network_outbound(&mut buf, method, host, port, path, body) {
        Ok(n) => unsafe {
            abi::__ageex_airgap_send(0x04, buf.as_ptr(), n as u32, resp.as_mut_ptr(), resp.len() as u32)
        },
        Err(_) => -3,
    }
}

pub fn airgap_send_app_launch(app_name: &[u8], resp: &mut [u8]) -> i32 {
    let mut buf = [0u8; 512];
    match wire::encode_app_launch(&mut buf, app_name) {
        Ok(n) => unsafe {
            abi::__ageex_airgap_send(0x05, buf.as_ptr(), n as u32, resp.as_mut_ptr(), resp.len() as u32)
        },
        Err(_) => -3,
    }
}

pub fn mem_request_search(query: &[u8], max_results: u32, out: &mut [u8]) -> i32 {
    let mut buf = [0u8; 4096];
    match wire::encode_memory_search(&mut buf, query, max_results) {
        Ok(n) => unsafe {
            abi::__ageex_mem_request(0x06, buf.as_ptr(), n as u32, core::ptr::null(), 0, out.as_mut_ptr(), out.len() as u32)
        },
        Err(_) => -3,
    }
}

pub fn mem_request_store(key: &[u8], value: &[u8], out: &mut [u8]) -> i32 {
    let mut buf = [0u8; 8192];
    match wire::encode_memory_store(&mut buf, key, value) {
        Ok(n) => unsafe {
            abi::__ageex_mem_request(0x07, buf.as_ptr(), n as u32, core::ptr::null(), 0, out.as_mut_ptr(), out.len() as u32)
        },
        Err(_) => -3,
    }
}

pub fn consent_poll(context: &[u8], options: &[u8], timeout_ms: u32) -> u32 {
    let mut buf = [0u8; 4096];
    match wire::encode_consent_request(&mut buf, context, options) {
        Ok(n) => unsafe { abi::__ageex_consent_poll(buf.as_ptr(), n as u32, timeout_ms) },
        Err(_) => 0,
    }
}

pub fn agent_log(level: u32, msg: &[u8]) {
    unsafe {
        abi::__ageex_agent_log(level, msg.as_ptr(), msg.len() as u32);
    }
}

pub fn agent_log_debug(msg: &[u8]) {
    agent_log(0, msg);
}

pub fn agent_log_info(msg: &[u8]) {
    agent_log(1, msg);
}

pub fn agent_log_warn(msg: &[u8]) {
    agent_log(2, msg);
}

pub fn agent_log_error(msg: &[u8]) {
    agent_log(3, msg);
}

pub fn agent_yield_done(message: &[u8]) -> u32 {
    unsafe { abi::__ageex_agent_yield_done(message.as_ptr(), message.len() as u32) }
}