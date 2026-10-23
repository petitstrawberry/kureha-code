//! Bounded JSON-RPC over a child process's stdio. The caller owns the LSP
//! initialize/initialized/shutdown/exit handshake and must negotiate UTF-16.
//!
//! `request`, `notify`, and `poll` never wait for pipe I/O. Queue saturation is
//! explicit: outgoing calls return an error; incoming overflow fails the client.
//! A terminal failure is delivered once, after already received events.
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{self, BufRead, BufReader, Read, Write},
    ops::Range,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
    time::Duration,
};

const MAX_HEADER_BYTES: usize = 8 * 1024;
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_QUEUED_BYTES: usize = 32 * 1024 * 1024;
const MAX_EVENTS: usize = 256;
const MAX_OUTGOING: usize = 32;
const MAX_STDERR_BYTES: usize = 16 * 1024;

/// Configure only the child. Graphical launches need not source /etc/profile.sh.
#[cfg(any(target_os = "scarlet", test))]
fn configure_scarlet_child(
    command: &mut Command,
    program: &str,
    inherited: impl Fn(&str) -> Option<std::ffi::OsString>,
) {
    const TOOLCHAIN: &str = "/opt/scarlet/toolchains/rust/current/bin";
    if inherited("PATH").is_none_or(|path| path.is_empty()) {
        command.env("PATH", format!("{TOOLCHAIN}:/bin:/usr/bin"));
    }
    // Init supplies /bin:/usr/bin even to GUI services. Preserve that PATH and
    // explicit tool overrides; don't configure a user's alternate server/wrapper.
    if matches!(program, "rust-analyzer" | "/bin/rust-analyzer") {
        for (key, tool) in [("CARGO", "cargo"), ("RUSTC", "rustc")] {
            if inherited(key).is_none() {
                command.env(key, format!("{TOOLCHAIN}/{tool}"));
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Response {
        id: u64,
        /// Null for error responses; inspect `error` before using this value.
        result: Value,
        error: Option<ResponseError>,
    },
    Notification {
        method: String,
        params: Value,
    },
    Failed(String),
}

#[derive(Default)]
struct Inbox {
    events: VecDeque<Event>,
    bytes: usize,
    failure: Option<String>,
}

#[derive(Default)]
struct Shared {
    inbox: Mutex<Inbox>,
    stderr: Mutex<VecDeque<u8>>,
    stopped: AtomicBool,
}

impl Shared {
    fn fail(&self, message: impl Into<String>) {
        let mut inbox = self.inbox.lock().unwrap();
        if !self.stopped.swap(true, Ordering::AcqRel) {
            inbox.failure = Some(message.into());
        }
    }

    fn event(&self, event: Event, bytes: usize) -> Result<(), String> {
        let mut inbox = self.inbox.lock().unwrap();
        if self.stopped.load(Ordering::Acquire) {
            return Err("LSP transport has stopped".into());
        }
        if inbox.events.len() >= MAX_EVENTS || inbox.bytes + bytes > MAX_QUEUED_BYTES {
            return Err("LSP incoming event queue is full; poll more frequently".into());
        }
        inbox.bytes += bytes;
        inbox.events.push_back(event);
        Ok(())
    }

    fn stderr_tail(&self) -> String {
        let bytes: Vec<_> = self.stderr.lock().unwrap().iter().copied().collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

pub struct Client {
    child: Mutex<Child>,
    outgoing: SyncSender<Vec<u8>>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
}

impl Client {
    /// Launch directly (no shell), with piped stdin/stdout/stderr and `cwd`.
    /// PATH lookup and process creation happen here; callers may spawn on their
    /// own startup worker if even that latency is unsuitable for their UI.
    pub fn spawn(program: &str, args: &[String], cwd: &Path) -> Result<Self, String> {
        let mut command = Command::new(program);
        #[cfg(target_os = "scarlet")]
        configure_scarlet_child(&mut command, program, |key| std::env::var_os(key));
        let child = command
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Cannot start LSP server {program:?}: {e}"))?;
        let (outgoing, incoming) = mpsc::sync_channel::<Vec<u8>>(MAX_OUTGOING);
        let shared = Arc::new(Shared::default());
        // Construct the owner before creating workers, so any thread-start
        // failure kills and reaps the process through Drop.
        let mut client = Self {
            child: Mutex::new(child),
            outgoing,
            shared,
            next_id: AtomicU64::new(1),
        };
        let child = client.child.get_mut().unwrap();
        let mut stdin = child.stdin.take().ok_or("Missing LSP stdin")?;
        let stdout = child.stdout.take().ok_or("Missing LSP stdout")?;
        let mut stderr = child.stderr.take().ok_or("Missing LSP stderr")?;
        let state = client.shared.clone();
        thread::Builder::new()
            .name("lsp-stderr".into())
            .spawn(move || {
                let mut buffer = [0; 4096];
                loop {
                    match stderr.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => {
                            let mut tail = state.stderr.lock().unwrap();
                            let excess = (tail.len() + n).saturating_sub(MAX_STDERR_BYTES);
                            tail.drain(..excess);
                            tail.extend(&buffer[..n]);
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| format!("Cannot start LSP stderr reader: {e}"))?;
        let state = client.shared.clone();
        thread::Builder::new()
            .name("lsp-writer".into())
            .spawn(move || {
                while !state.stopped.load(Ordering::Acquire) {
                    match incoming.recv_timeout(Duration::from_millis(100)) {
                        Ok(body) => {
                            if let Err(e) = write_frame(&mut stdin, &body) {
                                state.fail(format!("LSP stdin write failed: {e}"));
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .map_err(|e| format!("Cannot start LSP writer: {e}"))?;
        let state = client.shared.clone();
        let replies = client.outgoing.clone();
        thread::Builder::new()
            .name("lsp-reader".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                while !state.stopped.load(Ordering::Acquire) {
                    let result = (|| {
                        let body = read_frame(&mut reader)?.ok_or_else(|| {
                            "LSP stdout closed (server exited or closed its pipe)".to_string()
                        })?;
                        let size = body.len();
                        let message: Value = serde_json::from_slice(&body)
                            .map_err(|e| format!("Invalid LSP JSON: {e}"))?;
                        dispatch(message, size, &state, &replies)
                    })();
                    if let Err(e) = result {
                        state.fail(e);
                        break;
                    }
                }
            })
            .map_err(|e| format!("Cannot start LSP reader: {e}"))?;
        Ok(client)
    }

    /// Return an ID after enqueueing, without waiting for stdin or a response.
    /// IDs remain within LSP's signed 32-bit integer range and are never reused.
    pub fn request(&self, method: &str, params: Value) -> Result<u64, String> {
        let id = self
            .next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n <= i32::MAX as u64).then_some(n + 1)
            })
            .map_err(|_| "LSP request IDs exhausted".to_string())?;
        self.send(json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))?;
        Ok(id)
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.send(json!({"jsonrpc":"2.0", "method":method, "params":params}))
    }

    fn send(&self, mut message: Value) -> Result<(), String> {
        if self.shared.stopped.load(Ordering::Acquire) {
            return Err("LSP transport has stopped; poll for failure details".into());
        }
        if message["method"].as_str().is_none_or(str::is_empty) {
            return Err("LSP method must be a nonempty string".into());
        }
        // JSON-RPC params are optional, but when present must be structured.
        if message["params"].is_null() {
            message.as_object_mut().unwrap().remove("params");
        } else if !message["params"].is_object() && !message["params"].is_array() {
            return Err("LSP params must be an object, array, or null".into());
        }
        enqueue(&self.outgoing, &message)
    }

    /// Drain currently available events. Never waits for a server response.
    /// A clean stdout EOF also produces Failed; the lifecycle owner knows
    /// whether this was expected after its shutdown/exit sequence.
    pub fn poll(&self) -> Vec<Event> {
        let mut inbox = self.shared.inbox.lock().unwrap();
        let mut events: Vec<_> = inbox.events.drain(..).collect();
        inbox.bytes = 0;
        if let Some(mut failure) = inbox.failure.take() {
            if let Ok(Some(status)) = self.child.lock().unwrap().try_wait() {
                failure.push_str(&format!("\nServer {status}"));
            }
            let tail = self.shared.stderr_tail();
            if !tail.is_empty() {
                failure.push_str("\nServer stderr (tail):\n");
                failure.push_str(&tail);
            }
            events.push(Event::Failed(failure));
        }
        events
    }

    pub fn try_poll(&self) -> Vec<Event> {
        self.poll()
    }

    /// Latest 16 KiB, including output arriving after a failure was polled.
    pub fn stderr_tail(&self) -> String {
        self.shared.stderr_tail()
    }

    /// Nonblocking process status, for lifecycle owners verifying shutdown/exit.
    pub fn try_wait(&self) -> Result<Option<std::process::ExitStatus>, String> {
        self.child
            .lock()
            .unwrap()
            .try_wait()
            .map_err(|error| format!("Cannot inspect LSP server exit: {error}"))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        let child = self.child.get_mut().unwrap();
        // Kill before waiting: a server that ignores shutdown must not keep the
        // editor alive. Do not join pipe workers; descendants can inherit pipes.
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn enqueue(sender: &SyncSender<Vec<u8>>, message: &Value) -> Result<(), String> {
    let body = serde_json::to_vec(message).map_err(|e| format!("Cannot encode LSP JSON: {e}"))?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(format!("LSP frame exceeds {MAX_FRAME_BYTES} bytes"));
    }
    sender.try_send(body).map_err(|e| match e {
        TrySendError::Full(_) => "LSP outgoing queue is full; retry later".into(),
        TrySendError::Disconnected(_) => "LSP writer disconnected".into(),
    })
}

fn write_frame(writer: &mut impl Write, body: &[u8]) -> io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body)?;
    writer.flush()
}

/// Bound headers before allocating the body, and distinguish EOF between
/// frames from truncation inside either part. Buffered reads preserve surplus
/// bytes when multiple frames arrive in one OS read.
fn read_frame(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>, String> {
    let mut total = 0;
    let mut length = None;
    loop {
        let mut line = Vec::new();
        let n = reader
            .take((MAX_HEADER_BYTES - total + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|e| format!("LSP header read failed: {e}"))?;
        if n == 0 {
            return if total == 0 {
                Ok(None)
            } else {
                Err("Truncated LSP header".into())
            };
        }
        total += n;
        if total > MAX_HEADER_BYTES {
            return Err("LSP header is too large".into());
        }
        if !line.ends_with(b"\r\n") || !line.is_ascii() {
            return Err("LSP headers must be ASCII with CRLF endings".into());
        }
        if line == b"\r\n" {
            break;
        }
        let line = std::str::from_utf8(&line[..line.len() - 2]).unwrap();
        let (name, value) = line.split_once(':').ok_or("Malformed LSP header")?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        {
            return Err("Invalid LSP header name".into());
        }
        let value = value.trim();
        if name.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() {
                return Err("Duplicate LSP Content-Length".into());
            }
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err("Invalid LSP Content-Length".into());
            }
            let n: usize = value.parse().map_err(|_| "Invalid LSP Content-Length")?;
            if n == 0 || n > MAX_FRAME_BYTES {
                return Err("LSP body size is outside limits".into());
            }
            length = Some(n);
        } else if name.eq_ignore_ascii_case("Content-Type") {
            for parameter in value.split(';').skip(1) {
                if let Some((key, encoding)) = parameter.trim().split_once('=') {
                    let encoding = encoding.trim().trim_matches('"');
                    if key.trim().eq_ignore_ascii_case("charset")
                        && !encoding.eq_ignore_ascii_case("utf-8")
                        && !encoding.eq_ignore_ascii_case("utf8")
                    {
                        return Err(format!("Unsupported LSP charset: {encoding}"));
                    }
                }
            }
        }
    }
    let mut body = vec![0; length.ok_or("Missing LSP Content-Length")?];
    reader
        .read_exact(&mut body)
        .map_err(|e| format!("Truncated or unreadable LSP body: {e}"))?;
    Ok(Some(body))
}

fn dispatch(
    mut message: Value,
    bytes: usize,
    shared: &Shared,
    outgoing: &SyncSender<Vec<u8>>,
) -> Result<(), String> {
    if !message.is_object() || message["jsonrpc"] != "2.0" {
        return Err("Invalid LSP JSON-RPC envelope".into());
    }
    if let Some(method) = message.get("method") {
        let method = method
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("Invalid LSP method")?
            .to_owned();
        if message.get("result").is_some() || message.get("error").is_some() {
            return Err("LSP message mixes request and response fields".into());
        }
        let params = message
            .get_mut("params")
            .map(Value::take)
            .unwrap_or(Value::Null);
        if !params.is_null() && !params.is_object() && !params.is_array() {
            return Err("Invalid LSP params".into());
        }
        if let Some(id) = message.get("id") {
            if !id.is_string() && id.as_i64().is_none_or(|n| i32::try_from(n).is_err()) {
                return Err("Invalid LSP server request ID".into());
            }
            enqueue(outgoing, &server_reply(id.clone(), &method, &params))
        } else {
            shared.event(Event::Notification { method, params }, bytes)
        }
    } else {
        let id = message["id"]
            .as_u64()
            .filter(|n| *n <= i32::MAX as u64)
            .ok_or("LSP response ID is not a client request ID")?;
        let has_result = message.get("result").is_some();
        let error = if let Some(error) = message.get("error") {
            if has_result {
                return Err("LSP response contains both result and error".into());
            }
            let code = error["code"]
                .as_i64()
                .filter(|n| i32::try_from(*n).is_ok())
                .ok_or("Invalid LSP error code")?;
            let message = error["message"]
                .as_str()
                .ok_or("Invalid LSP error message")?
                .to_owned();
            Some(ResponseError {
                code,
                message,
                data: error.get("data").cloned(),
            })
        } else {
            if !has_result {
                return Err("LSP response has neither result nor error".into());
            }
            None
        };
        let result = message
            .get_mut("result")
            .map(Value::take)
            .unwrap_or(Value::Null);
        shared.event(Event::Response { id, result, error }, bytes)
    }
}

fn server_reply(id: Value, method: &str, params: &Value) -> Value {
    let result = match method {
        "workspace/configuration" => match params["items"].as_array() {
            Some(items) => Value::Array(vec![Value::Null; items.len()]),
            None => {
                return json!({"jsonrpc":"2.0", "id":id, "error":{"code":-32602,"message":"configuration requires items"}});
            }
        },
        // This minimal backend does not implement dynamic capability tracking,
        // UI prompts, workspace mutation, or refresh providers. Do not advertise
        // those client capabilities during initialize.
        "workspace/workspaceFolders"
        | "window/showMessageRequest"
        | "window/workDoneProgress/create" => Value::Null,
        "workspace/applyEdit" => {
            json!({"applied":false,"failureReason":"Workspace edits require editor approval and are not supported by this transport"})
        }
        _ => {
            return json!({"jsonrpc":"2.0", "id":id, "error":{"code":-32601,"message":format!("Unsupported client method: {method}")}});
        }
    };
    json!({"jsonrpc":"2.0", "id":id, "result":result})
}

/// Encode an absolute path without canonicalizing it (unsaved files are valid).
/// Unix filenames are lossless, including non-UTF-8 bytes. Windows supports
/// drive paths; remote/UNC authorities are intentionally rejected.
pub fn path_to_uri(path: &Path) -> Result<String, String> {
    if !path.is_absolute() {
        return Err("File URI requires an absolute path".into());
    }
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let bytes = {
        let path = path
            .to_str()
            .ok_or("Path is not Unicode")?
            .replace('\\', "/");
        if path.starts_with("//") {
            return Err("UNC paths are unsupported".into());
        }
        if path.as_bytes().get(1) == Some(&b':') {
            format!("/{path}").into_bytes()
        } else {
            path.into_bytes()
        }
    };
    if !bytes.starts_with(b"/") || bytes.contains(&0) {
        return Err("Invalid absolute file path".into());
    }
    let mut uri = String::from("file://");
    const HEX: &[u8] = b"0123456789ABCDEF";
    for b in bytes {
        if b.is_ascii_alphanumeric() || b"/-._~:".contains(&b) {
            uri.push(char::from(b));
        } else {
            uri.push('%');
            uri.push(char::from(HEX[(b >> 4) as usize]));
            uri.push(char::from(HEX[(b & 15) as usize]));
        }
    }
    Ok(uri)
}

/// Decode local file URIs only. Decode exactly once; '+' is a literal plus.
/// Query strings, fragments, NUL and encoded path separators are rejected.
pub fn uri_to_path(uri: &str) -> Result<PathBuf, String> {
    let (scheme, rest) = uri.split_once(':').ok_or("URI has no scheme")?;
    if !scheme.eq_ignore_ascii_case("file") {
        return Err("Expected a file URI".into());
    }
    let path = if let Some(rest) = rest.strip_prefix("//") {
        let slash = rest.find('/').ok_or("File URI has no absolute path")?;
        let authority = &rest[..slash];
        if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
            return Err("Remote file URI authorities are unsupported".into());
        }
        &rest[slash..]
    } else {
        rest
    };
    if !path.starts_with('/') || path.contains(['?', '#', '\\']) {
        return Err("Invalid file URI path (encode reserved characters)".into());
    }
    let mut decoded = Vec::new();
    let bytes = path.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3).ok_or("Truncated URI escape")?;
            let digit = |b: u8| (b as char).to_digit(16).map(|n| n as u8);
            let b = digit(hex[0])
                .zip(digit(hex[1]))
                .map(|(a, b)| a * 16 + b)
                .ok_or("Invalid URI escape")?;
            if b == b'/' || (cfg!(windows) && b == b'\\') {
                return Err("Encoded path separator in file URI".into());
            }
            i += 3;
            b
        } else {
            let b = bytes[i];
            if b.is_ascii_control() || b == b' ' {
                return Err("Unescaped whitespace/control in file URI".into());
            }
            i += 1;
            b
        };
        if b == 0 {
            return Err("NUL in file URI".into());
        }
        decoded.push(b);
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(PathBuf::from(std::ffi::OsString::from_vec(decoded)))
    }
    #[cfg(not(unix))]
    {
        let path = String::from_utf8(decoded).map_err(|_| "File URI is not Unicode")?;
        #[cfg(windows)]
        let path = if path.as_bytes().get(2) == Some(&b':') {
            &path[1..]
        } else {
            &path
        };
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err("File URI is not absolute on this platform".into());
        }
        Ok(path)
    }
}

#[derive(Debug, Copy, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

impl Position {
    pub fn to_value(self) -> Value {
        json!({"line":self.line,"character":self.character})
    }

    pub fn from_value(value: &Value) -> Result<Self, String> {
        let field = |name| {
            value[name]
                .as_u64()
                .filter(|n| *n <= i32::MAX as u64)
                .map(|n| n as u32)
                .ok_or_else(|| format!("Invalid LSP position {name}"))
        };
        Ok(Self {
            line: field("line")?,
            character: field("character")?,
        })
    }
}

/// Convert a byte offset, clamped to the document and rounded down to a UTF-8
/// boundary. Both CRLF and lone CR/LF delimit lines; offsets inside CRLF map to
/// the preceding line's end.
pub fn offset_to_position(text: &str, offset: usize) -> Position {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let mut position = Position::default();
    let mut chars = text.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if i >= offset {
            break;
        }
        if ch == '\r' {
            if chars.peek().is_some_and(|(_, c)| *c == '\n') {
                if offset == i + 1 {
                    break;
                }
                chars.next();
            }
            position.line += 1;
            position.character = 0;
        } else if ch == '\n' {
            position.line += 1;
            position.character = 0;
        } else {
            position.character += ch.len_utf16() as u32;
        }
    }
    position
}

/// Invalid line or a column splitting a surrogate pair returns None. A column
/// beyond the line is clamped to its end, as required by LSP.
pub fn position_to_offset(text: &str, position: Position) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut start = 0;
    for _ in 0..position.line {
        let relative = bytes[start..]
            .iter()
            .position(|b| *b == b'\r' || *b == b'\n')?;
        start += relative;
        let cr = bytes[start] == b'\r';
        start += 1;
        if cr && bytes.get(start) == Some(&b'\n') {
            start += 1;
        }
    }
    let end = start
        + bytes[start..]
            .iter()
            .position(|b| *b == b'\r' || *b == b'\n')
            .unwrap_or(bytes.len() - start);
    let mut column = 0;
    for (i, ch) in text[start..end].char_indices() {
        if column == position.character {
            return Some(start + i);
        }
        column += ch.len_utf16() as u32;
        if column > position.character {
            return None;
        }
    }
    Some(end)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Range<usize>,
    pub new_text: String,
}

/// Convert an ordinary/annotated TextEdit against the original document.
/// InsertReplaceEdit is deliberately not guessed; the caller chooses a range.
pub fn text_edit(text: &str, edit: &Value) -> Result<TextEdit, String> {
    let start = Position::from_value(&edit["range"]["start"])?;
    let end = Position::from_value(&edit["range"]["end"])?;
    if start > end {
        return Err("Reversed LSP text edit range".into());
    }
    let start = position_to_offset(text, start).ok_or("Invalid text edit start")?;
    let end = position_to_offset(text, end).ok_or("Invalid text edit end")?;
    let new_text = edit["newText"]
        .as_str()
        .ok_or("Text edit has no newText string")?
        .to_owned();
    Ok(TextEdit {
        range: start..end,
        new_text,
    })
}

/// Return validated, nonoverlapping edits in descending byte order. Insertions
/// at the same position retain the server's order when applied in this order.
/// An insertion followed by a replacement at that position is also supported.
pub fn text_edits(text: &str, edits: &Value) -> Result<Vec<TextEdit>, String> {
    let edits = edits.as_array().ok_or("Expected a TextEdit array")?;
    let mut converted = edits
        .iter()
        .map(|e| text_edit(text, e))
        .collect::<Result<Vec<_>, _>>()?;
    // Stable ascending sort preserves the protocol's ordering for equal starts.
    converted.sort_by_key(|e| e.range.start);
    for pair in converted.windows(2) {
        if pair[0].range.end > pair[1].range.start {
            return Err("Overlapping LSP text edits".into());
        }
    }
    converted.reverse();
    Ok(converted)
}

pub fn apply_text_edits(text: &str, edits: &Value) -> Result<String, String> {
    let edits = text_edits(text, edits)?;
    let mut text = text.to_owned();
    for edit in edits {
        text.replace_range(edit.range, &edit.new_text);
    }
    Ok(text)
}

#[cfg(test)]
#[path = "lsp_tests.rs"]
mod tests;
