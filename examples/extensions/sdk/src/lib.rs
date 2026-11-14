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
    /// The environment for the programs Kureha starts in `folder` (with
    /// `[contributes.environment]`). Programs run in that folder, from
    /// Kureha's own environment, for up to 15 minutes. Reply with
    /// [`Reply::environment`].
    Environment {
        folder: &'a str,
    },
    /// Format the whole document. Reply with edits (or none).
    Format {
        document: Document,
        language: &'a str,
        tab_size: u32,
        insert_spaces: bool,
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
            Some("environment") => match message["folder"].as_str() {
                Some(folder) => Self::Environment { folder },
                None => Self::Other(message),
            },
            Some("format") => {
                let options = &message["options"];
                match (document(), message["language"].as_str()) {
                    (Some(document), Some(language)) => Self::Format {
                        document,
                        language,
                        tab_size: options["tab_size"].as_u64().unwrap_or(4) as u32,
                        insert_spaces: options["insert_spaces"].as_bool().unwrap_or(true),
                    },
                    _ => Self::Other(message),
                }
            }
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

/// Where a program runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cwd {
    /// The folder of the document (the default).
    #[default]
    Document,
    /// The open folder, or the document's folder without one.
    Workspace,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Input {
    #[default]
    Empty,
    Text(String),
    Document,
}

/// A program to run, which the extension must declare in `[capabilities]
/// process` and the user must allow. Programs can run only while handling a
/// command or a format request; the call waits until the program ends.
///
/// ```ignore
/// let output = Process::new("ruff")
///     .args(["format", "-"])
///     .stdin_document()
///     .run()?;
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    program: String,
    args: Vec<String>,
    input: Input,
    cwd: Cwd,
    timeout_ms: Option<u64>,
}
impl Process {
    /// A bare program name, looked up on the editor's `PATH`.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            input: Input::Empty,
            cwd: Cwd::Document,
            timeout_ms: None,
        }
    }
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }
    pub fn args<I: IntoIterator<Item = S>, S: Into<String>>(mut self, args: I) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }
    /// Sends `text` as standard input.
    pub fn stdin(mut self, text: impl Into<String>) -> Self {
        self.input = Input::Text(text.into());
        self
    }
    /// Sends the whole document as standard input, straight from the host.
    pub fn stdin_document(mut self) -> Self {
        self.input = Input::Document;
        self
    }
    pub fn cwd(mut self, cwd: Cwd) -> Self {
        self.cwd = cwd;
        self
    }
    /// Stops the program after this long (the host allows up to 20 s; 5 s by default).
    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = Some(ms);
        self
    }
    /// The host request this run sends.
    pub fn request(&self) -> Value {
        let mut request = json!({
            "op": "process",
            "program": self.program,
            "args": self.args,
            "cwd": match self.cwd {
                Cwd::Document => "document",
                Cwd::Workspace => "workspace",
            },
        });
        match &self.input {
            Input::Empty => {}
            Input::Text(text) => request["stdin"] = json!(text),
            Input::Document => request["stdin_document"] = json!(true),
        }
        if let Some(ms) = self.timeout_ms {
            request["timeout_ms"] = json!(ms);
        }
        request
    }
    /// Runs the program to completion. Refusals (not declared, not allowed,
    /// not found), timeouts and oversized output are errors; a failing exit
    /// status is not, see `Output::success`.
    pub fn run(&self) -> Result<Output, String> {
        Output::parse(&host(&self.request())?)
    }
}

/// What a program printed and how it ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    /// The exit code, or `None` if a signal ended the program.
    pub status: Option<i32>,
    pub stdout: String,
    /// The first 16 KiB at most.
    pub stderr: String,
}
impl Output {
    pub fn parse(reply: &Value) -> Result<Self, String> {
        Ok(Self {
            status: reply["status"].as_i64().and_then(|n| i32::try_from(n).ok()),
            stdout: reply["stdout"]
                .as_str()
                .ok_or("the host returned no output")?
                .to_owned(),
            stderr: reply["stderr"].as_str().unwrap_or_default().to_owned(),
        })
    }
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
    /// A one-line reason for a failed run: the first line of standard error,
    /// else the exit status.
    pub fn failure(&self) -> String {
        match self
            .stderr
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
        {
            Some(line) => line.chars().take(200).collect(),
            None => match self.status {
                Some(code) => format!("exited with status {code}"),
                None => "was stopped by a signal".into(),
            },
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
    /// Offers one of this extension's own commands as a way out of an error
    /// (an `environment` call's, for now): Kureha shows it as a button
    /// titled as in the manifest. At most two; others are ignored.
    pub fn offer(mut self, command: &str) -> Self {
        match self.0.get_mut("actions").and_then(Value::as_array_mut) {
            Some(actions) => actions.push(json!(command)),
            None => self.0["actions"] = json!([command]),
        }
        self
    }
    /// A status bar message without edits.
    pub fn status(message: impl Into<String>) -> Self {
        Self(json!({"status": message.into()}))
    }
    /// Replaces `range` with `text`, applied as one undo step.
    pub fn edit(range: Range<usize>, text: impl Into<String>) -> Self {
        Self(json!({"edits": [{"start": range.start, "end": range.end, "text": text.into()}]}))
    }
    /// A successful reply that changes nothing.
    pub fn unchanged() -> Self {
        Self(json!({"edits": []}))
    }
    /// An environment: each variable set to a value, or (`None`) removed.
    pub fn environment<I, K>(vars: I) -> Self
    where
        I: IntoIterator<Item = (K, Option<String>)>,
        K: Into<String>,
    {
        let vars: serde_json::Map<String, Value> = vars
            .into_iter()
            .map(|(key, value)| (key.into(), value.map_or(Value::Null, Value::String)))
            .collect();
        Self(json!({ "environment": vars }))
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

    #[test]
    fn process_builds_the_host_request_and_reads_its_output() {
        let run = Process::new("ruff")
            .args(["format", "-"])
            .stdin_document()
            .timeout_ms(9000);
        assert_eq!(
            run.request(),
            json!({"op": "process", "program": "ruff", "args": ["format", "-"],
                   "cwd": "document", "stdin_document": true, "timeout_ms": 9000})
        );
        let run = Process::new("sh")
            .arg("-c")
            .arg("cat")
            .stdin("hi")
            .cwd(Cwd::Workspace);
        assert_eq!(run.request()["stdin"], "hi");
        assert_eq!(run.request()["cwd"], "workspace");
        assert!(run.request().get("stdin_document").is_none());

        let output =
            Output::parse(&json!({"status": 2, "stdout": "", "stderr": "\nerror: bad\nmore"}))
                .unwrap();
        assert!(!output.success());
        assert_eq!(output.failure(), "error: bad");
        let output = Output::parse(&json!({"status": null, "stdout": "", "stderr": ""})).unwrap();
        assert_eq!(output.failure(), "was stopped by a signal");
        assert!(Output::parse(&json!({})).is_err());
    }
}
