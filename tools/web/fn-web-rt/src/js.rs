//! Imports of `js/fnweb_rt.js` (works in the Window and in workers).
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/js/fnweb_rt.js")]
extern "C" {
    pub fn fnweb_is_window() -> bool;
    pub fn fnweb_now_ms() -> f64;
    pub fn fnweb_wall_ms() -> f64;
    pub fn fnweb_console(level: u32, text: &str);
    /// None = 404. Err = any other failure (network, status).
    #[wasm_bindgen(catch)]
    pub fn fnweb_http(method: &str, url: &str, range_start: f64, range_end: f64, body: Option<js_sys::Uint8Array>, token: Option<String>) -> Result<Option<js_sys::Uint8Array>, JsValue>;
    pub fn fnweb_io_port() -> bool;
    pub fn fnweb_io_request(id: u32, url: &str, range_start: f64, range_end: f64, token: Option<String>, high: bool);
    /// REMOTE MODE: the page keeps /fn/user in IndexedDB (from a worker: forwarded to the page).
    pub fn fnweb_user_save(path: &str, bytes: &[u8]);
    pub fn fnweb_user_delete(path: &str);
    pub fn fnweb_hardware_concurrency() -> u32;
    pub fn fnweb_request_workers(n: u32);
    pub fn fnweb_url_params() -> js_sys::Array;
    pub fn fnweb_stack() -> String;
    pub fn fnweb_stack_deep(n: u32) -> String;
}
