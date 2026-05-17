#![allow(improper_ctypes)]

#[link(wasm_import_module = "ageex")]
extern "C" {
    pub fn __ageex_airgap_send(
        op: u32,
        req_ptr: *const u8,
        req_len: u32,
        resp_ptr: *mut u8,
        resp_cap: u32,
    ) -> i32;

    pub fn __ageex_mem_request(
        op: u32,
        key_ptr: *const u8,
        key_len: u32,
        val_ptr: *const u8,
        val_len: u32,
        buf_ptr: *mut u8,
        buf_cap: u32,
    ) -> i32;

    pub fn __ageex_consent_poll(
        req_ptr: *const u8,
        req_len: u32,
        timeout_ms: u32,
    ) -> u32;

    pub fn __ageex_agent_log(
        level: u32,
        msg_ptr: *const u8,
        msg_len: u32,
    );

    pub fn __ageex_agent_yield_done(
        result_ptr: *const u8,
        result_len: u32,
    ) -> u32;
}