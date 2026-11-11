//! Minimal glue for Kureha Code extensions (extension API 1).
//!
//! It hides the raw ABI described in `docs/extensions.md`: buffer exports,
//! the two-step host call and JSON framing. An extension writes one handler
//! and exports it:
//!
//! ```ignore
//! use kureha_extension::{Reply, Request};
//! fn handle(request: Request) -> Reply {
//!     match request {
//!         Request::Command { id: "demo.hello", .. } => Reply::status("Hello"),
//!         _ => Reply::unsupported(),
//!     }
//! }
//! kureha_extension::export!(handle);
//! ```
//!
//! The FFI exists only on `wasm32`, so pure helpers can be tested natively.

use std::ops::Range;

pub use serde_json::{self, Value, json};

/// Largest range one `read` may return; mirrors the host limit.
pub const MAX_READ_BYTES: usize = 512 * 1024;

/// The document lent to one call. Offsets are UTF-8 byte offsets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    pub path: String,
    pub version: u64,
    pub length: usize,
    /// Ordered, possibly empty.
    pub selection: Range<usize>,
}
impl Document {
    fn parse(value: &Value) -> Option<Self> {
        let offset = |v: &Value| v.as_u64().and_then(|n| usize::try_from(n).ok());
        let selection = value.get("selection")?;
        Some(Self {
            path: value.get("path")?.as_str()?.to_owned(),
            version: value.get("version")?.as_u64()?,
            length: offset(value.get("length")?)?,
            selection: offset(&selection[0])?..offset(&selection[1])?,
        })
    }
    /// Reads `range` of this document from the host.
    pub fn read(&self, range: Range<usize>) -> Result<String, String> {
        if range.len() > MAX_READ_BYTES {
            return Err(format!(
                "the text is too large ({} KiB; the limit is {} KiB)",
                range.len() / 1024,
                MAX_READ_BYTES / 1024
            ));
        }
        let reply = host(&json!({"op": "read", "start": range.start, "end": range.end}))?;
        reply["text"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "the host returned no text".into())
    }
    /// Reads the whole document.
    pub fn text(&self) -> Result<String, String> {
        self.read(0..self.length)
    }
}

/// One message from the host.
#[derive(Debug)]
pub enum Request<'a> {
    Activate,
    Command {
        id: &'a str,
        document: Document,
    },
    Decorations {
        document: Document,
        range: Range<usize>,
    },
    Other(&'a Value),
}
impl<'a> Request<'a> {
    pub fn parse(message: &'a Value) -> Self {
        let document = || message.get("document").and_then(Document::parse);
        match message["kind"].as_str() {
            Some("activate") => Self::Activate,
            Some("command") => match (message["command"].as_str(), document()) {
                (Some(id), Some(document)) => Self::Command { id, document },
                _ => Self::Other(message),
            },
            Some("decorations") => {
                let range = &message["range"];
                match (document(), range[0].as_u64(), range[1].as_u64()) {
                    (Some(document), Some(start), Some(end)) => Self::Decorations {
                        document,
                        range: start as usize..end as usize,
                    },
                    _ => Self::Other(message),
                }
            }
            _ => Self::Other(message),
        }
    }
}

/// The reply to one message.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply(pub Value);
impl Reply {
    pub fn ok() -> Self {
        Self(json!({}))
    }
    pub fn unsupported() -> Self {
        Self::error("unsupported request")
    }
    /// Shown in the status bar; nothing is applied.
    pub fn error(message: impl Into<String>) -> Self {
        Self(json!({"error": message.into()}))
    }
    /// A status bar message without edits.
    pub fn status(message: impl Into<String>) -> Self {
        Self(json!({"status": message.into()}))
    }
    /// Replaces `range` with `text`, applied as one undo step.
    pub fn edit(range: Range<usize>, text: impl Into<String>) -> Self {
        Self(json!({"edits": [{"start": range.start, "end": range.end, "text": text.into()}]}))
    }
    pub fn with_status(mut self, message: impl Into<String>) -> Self {
        self.0["status"] = Value::String(message.into());
        self
    }
}

/// Decodes one host message, runs `handler` and encodes its reply.
pub fn dispatch(input: &[u8], handler: fn(Request) -> Reply) -> Vec<u8> {
    let reply = match serde_json::from_slice::<Value>(input) {
        Ok(message) => handler(Request::parse(&message)),
        Err(e) => Reply::error(format!("invalid message: {e}")),
    };
    serde_json::to_vec(&reply.0).unwrap_or_else(|_| br#"{"error":"cannot encode reply"}"#.to_vec())
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "kureha")]
unsafe extern "C" {
    fn kx_host(ptr: *const u8, len: usize) -> i64;
    fn kx_host_take(ptr: *mut u8, len: usize) -> i32;
}

/// Sends one request to the host and returns its JSON reply.
#[cfg(target_arch = "wasm32")]
pub fn host(request: &Value) -> Result<Value, String> {
    let request = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    let len = unsafe { kx_host(request.as_ptr(), request.len()) };
    if len < 0 {
        return Err("the host refused the request".into());
    }
    let mut reply = vec![0; len as usize];
    if unsafe { kx_host_take(reply.as_mut_ptr(), reply.len()) } != 0 {
        return Err("the host reply was lost".into());
    }
    let reply: Value = serde_json::from_slice(&reply).map_err(|e| e.to_string())?;
    match reply.get("error").and_then(Value::as_str) {
        Some(error) => Err(error.to_owned()),
        None => Ok(reply),
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub fn host(_request: &Value) -> Result<Value, String> {
    Err("the Kureha host is only available inside WebAssembly".into())
}

/// Buffers are boxed slices so their layout is exactly `len` bytes.
#[doc(hidden)]
pub fn __leak(bytes: Box<[u8]>) -> (*mut u8, usize) {
    let len = bytes.len();
    (Box::into_raw(bytes).cast::<u8>(), len)
}

/// Defines the module exports (`kx_alloc`, `kx_free`, `kx_call`) for `handler`,
/// a function in the calling module.
#[macro_export]
macro_rules! export {
    ($handler:ident) => {
        // Native builds (tests) have no exports but still type-check the handler.
        #[cfg(not(target_arch = "wasm32"))]
        const _: fn($crate::Request) -> $crate::Reply = $handler;
        #[cfg(target_arch = "wasm32")]
        mod __kureha_exports {
            #[unsafe(no_mangle)]
            pub extern "C" fn kx_alloc(len: usize) -> *mut u8 {
                $crate::__leak(::std::vec![0u8; len].into_boxed_slice()).0
            }
            /// # Safety
            /// `ptr` and `len` come from `kx_alloc` or a `kx_call` reply.
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn kx_free(ptr: *mut u8, len: usize) {
                drop(unsafe {
                    ::std::boxed::Box::from_raw(::std::ptr::slice_from_raw_parts_mut(ptr, len))
                });
            }
            /// # Safety
            /// The host passes a buffer it obtained from `kx_alloc`.
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn kx_call(ptr: *const u8, len: usize) -> i64 {
                let input = unsafe { ::std::slice::from_raw_parts(ptr, len) };
                let reply = $crate::dispatch(input, super::$handler);
                let (ptr, len) = $crate::__leak(reply.into_boxed_slice());
                ((ptr as i64) << 32) | len as i64
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo(request: Request) -> Reply {
        match request {
            Request::Activate => Reply::ok(),
            Request::Command { id, document } => {
                Reply::edit(document.selection, id).with_status(document.path)
            }
            _ => Reply::unsupported(),
        }
    }

    #[test]
    fn dispatch_parses_requests_and_encodes_replies() {
        let reply = dispatch(br#"{"kind":"activate"}"#, echo);
        assert_eq!(reply, b"{}");
        let message = json!({
            "kind": "command", "command": "x.y",
            "document": {"path": "/a.txt", "version": 3, "length": 9, "selection": [2, 5]}
        });
        let reply: Value =
            serde_json::from_slice(&dispatch(&serde_json::to_vec(&message).unwrap(), echo))
                .unwrap();
        assert_eq!(
            reply,
            json!({"edits": [{"start": 2, "end": 5, "text": "x.y"}], "status": "/a.txt"})
        );
        let reply: Value = serde_json::from_slice(&dispatch(b"nope", echo)).unwrap();
        assert!(reply["error"].as_str().unwrap().contains("invalid message"));
    }
}
