use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

fn page_fn(name: &str) -> Option<js_sys::Function> { js_sys::Reflect::get(&js_sys::global(), &name.into()).ok().and_then(|f| f.dyn_into::<js_sys::Function>().ok()) }

#[wasm_bindgen]
pub fn fnweb_mp_text(text: String) { super::push_text(text); }

#[wasm_bindgen]
pub fn fnweb_mp_bin(bytes: Vec<u8>) { super::push_bin(bytes); }

#[wasm_bindgen]
pub fn fnweb_mp_autoplay() { super::request_autoplay(); }

pub fn send(bytes: &[u8]) {
    if let Some(f) = page_fn("__fnweb_mp_send") { let _ = f.call1(&JsValue::NULL, &js_sys::Uint8Array::from(bytes)); }
}

pub fn feed(text: &str) {
    if let Some(f) = page_fn("__fnweb_mp_feed") { let _ = f.call1(&JsValue::NULL, &text.into()); }
}

pub fn send_text(json: &str) -> bool {
    page_fn("__fnweb_mp_text_send").and_then(|f| f.call1(&JsValue::NULL, &json.into()).ok()).and_then(|v| v.as_bool()).unwrap_or(false)
}

pub fn create(name: &str) -> bool {
    page_fn("__fnweb_mp_create").map(|f| f.call1(&JsValue::NULL, &name.into()).is_ok()).unwrap_or(false)
}

pub fn join(code: &str, name: &str) -> bool {
    page_fn("__fnweb_mp_join").map(|f| f.call2(&JsValue::NULL, &code.into(), &name.into()).is_ok()).unwrap_or(false)
}

pub fn peek(code: &str) -> bool {
    page_fn("__fnweb_mp_peek").map(|f| f.call1(&JsValue::NULL, &code.into()).is_ok()).unwrap_or(false)
}

pub fn leave() {
    if let Some(f) = page_fn("__fnweb_mp_leave") { let _ = f.call0(&JsValue::NULL); }
}
