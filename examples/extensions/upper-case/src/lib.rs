//! Example Kureha Code extension: upper-cases the selection.
//!
//! The ABI is described in `docs/extensions.md`. This file is the whole
//! extension; a future SDK crate would hide the exports below.
use serde_json::{Value, json};

#[link(wasm_import_module = "kureha")]
unsafe extern "C" {
    fn kx_host(ptr: *const u8, len: usize) -> i64;
    fn kx_host_take(ptr: *mut u8, len: usize) -> i32;
}

/// Buffers are boxed slices so their layout is exactly `len` bytes.
fn leak(bytes: Box<[u8]>) -> (*mut u8, usize) {
    let len = bytes.len();
    (Box::into_raw(bytes).cast::<u8>(), len)
}

#[unsafe(no_mangle)]
pub extern "C" fn kx_alloc(len: usize) -> *mut u8 {
    leak(vec![0; len].into_boxed_slice()).0
}

/// # Safety
/// `ptr` and `len` must come from `kx_alloc` or a `kx_call` reply.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kx_free(ptr: *mut u8, len: usize) {
    drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)) });
}

/// # Safety
/// The host passes a buffer it obtained from `kx_alloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kx_call(ptr: *const u8, len: usize) -> i64 {
    let input = unsafe { std::slice::from_raw_parts(ptr, len) };
    let reply = match serde_json::from_slice::<Value>(input) {
        Ok(message) => handle(&message),
        Err(e) => json!({"error": format!("invalid message: {e}")}),
    };
    let (ptr, len) = leak(serde_json::to_vec(&reply).unwrap().into_boxed_slice());
    ((ptr as i64) << 32) | len as i64
}

fn host(request: &Value) -> Value {
    let request = serde_json::to_vec(request).unwrap();
    let len = unsafe { kx_host(request.as_ptr(), request.len()) };
    if len < 0 {
        return json!({"error": "host request failed"});
    }
    let mut reply = vec![0; len as usize];
    if unsafe { kx_host_take(reply.as_mut_ptr(), reply.len()) } != 0 {
        return json!({"error": "host reply was lost"});
    }
    serde_json::from_slice(&reply).unwrap_or(Value::Null)
}

fn handle(message: &Value) -> Value {
    match message["kind"].as_str() {
        Some("activate") => json!({}),
        Some("command") if message["command"] == "upper-case.selection" => {
            let selection = &message["document"]["selection"];
            let (start, end) = (selection[0].as_u64(), selection[1].as_u64());
            let (Some(start), Some(end)) = (start, end) else {
                return json!({"error": "missing selection"});
            };
            if start == end {
                return json!({"status": "Select some text to upper-case"});
            }
            let read = host(&json!({"op": "read", "start": start, "end": end}));
            match read["text"].as_str() {
                Some(text) => json!({
                    "edits": [{"start": start, "end": end, "text": text.to_uppercase()}],
                    "status": format!("Upper-cased {} characters", text.chars().count()),
                }),
                None => {
                    json!({"error": read["error"].as_str().unwrap_or("cannot read the selection")})
                }
            }
        }
        _ => json!({"error": "unsupported request"}),
    }
}
