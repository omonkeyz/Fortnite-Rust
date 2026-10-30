pub const KEY: &str = "fnweb-mp-name";

#[cfg(target_arch = "wasm32")]
fn storage() -> Option<wasm_bindgen::JsValue> {
    use wasm_bindgen::JsValue;
    let s = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("localStorage")).ok()?;
    (!s.is_undefined() && !s.is_null()).then_some(s)
}

#[cfg(target_arch = "wasm32")]
fn call(s: &wasm_bindgen::JsValue, name: &str, args: &[&str]) -> Option<wasm_bindgen::JsValue> {
    use wasm_bindgen::{JsCast, JsValue};
    let f: js_sys::Function = js_sys::Reflect::get(s, &JsValue::from_str(name)).ok()?.dyn_into().ok()?;
    let a = js_sys::Array::new();
    for x in args { a.push(&JsValue::from_str(x)); }
    f.apply(s, &a).ok()
}

#[cfg(target_arch = "wasm32")]
pub fn saved() -> Option<String> {
    let s = storage()?;
    call(&s, "getItem", &[KEY])?.as_string().and_then(|n| fn_net::chat::clean_name(&n))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn saved() -> Option<String> { None }

#[cfg(target_arch = "wasm32")]
pub fn save(name: &str) {
    if let Some(s) = storage() { let _ = call(&s, "setItem", &[KEY, name]); }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn save(_name: &str) {}

#[cfg(target_arch = "wasm32")]
pub fn get(key: &str) -> Option<String> { call(&storage()?, "getItem", &[key])?.as_string() }

#[cfg(not(target_arch = "wasm32"))]
pub fn get(_key: &str) -> Option<String> { None }

#[cfg(target_arch = "wasm32")]
pub fn set(key: &str, value: &str) { if let Some(s) = storage() { let _ = call(&s, "setItem", &[key, value]); } }

#[cfg(not(target_arch = "wasm32"))]
pub fn set(_key: &str, _value: &str) {}
