//! One extension's WebAssembly instance. Runs only on the extension worker.
//!
//! ABI (see `docs/extensions.md`), all integers little-endian `i32`/`i64`:
//!
//! - The module exports `memory`, `kx_alloc(len) -> ptr`, `kx_free(ptr, len)`
//!   and `kx_call(ptr, len) -> i64`. The host writes one JSON message into a
//!   buffer from `kx_alloc`, calls `kx_call`, and frees the buffer afterwards.
//!   `kx_call` returns its JSON reply as `(ptr << 32) | len`; the host frees it.
//! - The module may import `kureha.kx_host(ptr, len) -> i64`, which takes a JSON
//!   request and returns the reply length (negative on failure), and
//!   `kureha.kx_host_take(ptr, len) -> i32`, which copies that pending reply
//!   into guest memory. The two steps avoid re-entering the guest's allocator.
//!
//! Every pointer and length from the guest is checked before use. Each call
//! runs with a fuel budget and the store caps linear memory.
//!
//! Host requests (`op`): `read` (document text), `log`, and `process`, which
//! runs a program the extension declared and the user granted. `process` is
//! synchronous and only allowed in commands and formatters; the extension's own
//! worker thread waits for it (see `process.rs` for the limits).

use super::process::{self, Grants};
use serde_json::{Value, json};
use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use wasmi::{
    Caller, Config, Engine, Extern, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder,
    TrapCode, TypedFunc,
};

pub const MAX_WASM_BYTES: u64 = 16 * 1024 * 1024;
/// Upper bound for any single message in either direction.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
pub const MEMORY_LIMIT: usize = 64 * 1024 * 1024;

/// Fuel per call. One unit is roughly one interpreted instruction, so the
/// wall-clock cost differs by platform; see the `fuel_throughput` test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budgets {
    /// Module start and `activate`.
    pub activate: u64,
    /// Side-effect-free queries, which run while the user waits.
    pub query: u64,
    /// Commands and other tasks.
    pub task: u64,
}
impl Default for Budgets {
    fn default() -> Self {
        Self {
            activate: 20_000_000,
            query: 2_000_000,
            task: 1_000_000_000,
        }
    }
}
pub const MAX_EDITS: usize = 4096;
pub const MAX_DECORATIONS: usize = 10_000;
/// Fixed fuel charged for each host function call.
pub const HOST_CALL_FUEL: u64 = 10_000;
/// Fuel charged per byte the host reads, encodes or copies for the guest.
/// Host work is native, but it must not be free or a guest could loop on it.
pub const HOST_BYTE_FUEL: u64 = 4;
const MAX_STATUS: usize = 256;
const MAX_LOG_ENTRIES: usize = 64;
const MAX_LOG_BYTES: usize = 1024;

/// An immutable document revision lent to one call.
#[derive(Clone, Debug)]
pub struct DocumentSnapshot {
    pub path: PathBuf,
    pub version: u64,
    pub text: Arc<String>,
    pub selection: Range<usize>,
    /// The open folder, if any; a process may run there.
    pub workspace: Option<PathBuf>,
}
impl DocumentSnapshot {
    fn describe(&self) -> Value {
        json!({
            "path": self.path.to_string_lossy(),
            "version": self.version,
            "length": self.text.len(),
            "selection": [self.selection.start, self.selection.end],
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Range<usize>,
    pub text: String,
}

/// Theme roles an extension may use; colors stay with the host theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecorationStyle {
    Highlight,
    Info,
    Warning,
    Error,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoration {
    pub range: Range<usize>,
    pub style: DecorationStyle,
}

/// Indentation the editor would use; the same values sent to language servers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatOptions {
    pub tab_size: u32,
    pub insert_spaces: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandOutcome {
    /// Sorted and non-overlapping, on char boundaries of the snapshot.
    pub edits: Vec<TextEdit>,
    pub status: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// The call used up its fuel budget.
    Fuel,
    /// The guest trapped or broke the ABI.
    Trap(String),
    /// The reply was not a valid message.
    Protocol(String),
    /// The extension reported an error itself.
    Extension(String),
}
impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fuel => f.write_str("took too long and was stopped"),
            Self::Trap(e) => write!(f, "crashed: {e}"),
            Self::Protocol(e) => write!(f, "sent an invalid reply: {e}"),
            Self::Extension(e) => f.write_str(e),
        }
    }
}
fn trap(error: wasmi::Error) -> CallError {
    if error.as_trap_code() == Some(TrapCode::OutOfFuel) {
        CallError::Fuel
    } else {
        CallError::Trap(error.to_string())
    }
}

/// Who the instance belongs to and which programs it may run.
#[derive(Clone, Debug, Default)]
pub struct Access {
    pub extension: String,
    /// The manifest's `[capabilities] process`.
    pub programs: Vec<String>,
    pub grants: Grants,
}

/// The kind of call in progress; only tasks may wait on the outside world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Call {
    Activate,
    Query,
    Task,
    /// Computing a folder's environment: a task whose programs run in that
    /// folder, from the base environment, for up to
    /// [`process::MAX_ENVIRONMENT_TIMEOUT`].
    Environment,
}

struct HostState {
    limits: StoreLimits,
    document: Option<DocumentSnapshot>,
    pending: Option<Vec<u8>>,
    log: Vec<String>,
    access: Access,
    call: Call,
    /// Processes started in the current call.
    runs: u32,
    /// The folder of an environment call.
    folder: Option<PathBuf>,
    /// Declared programs refused in this call because the user has not
    /// allowed them; the app offers to.
    refused: std::collections::BTreeSet<String>,
    /// The extension's own commands its error reply offers as next steps
    /// (`"actions"`), at most [`MAX_ACTIONS`].
    offered: Vec<String>,
}
/// Commands an error reply may offer.
pub const MAX_ACTIONS: usize = 2;

/// Takes `units` from the caller's fuel, failing like an instruction would.
fn charge(caller: &mut Caller<'_, HostState>, units: u64) -> Result<(), wasmi::Error> {
    let fuel = caller.get_fuel()?;
    if fuel < units {
        caller.set_fuel(0)?;
        return Err(TrapCode::OutOfFuel.into());
    }
    caller.set_fuel(fuel - units)
}
fn byte_fuel(bytes: usize) -> u64 {
    (bytes as u64).saturating_mul(HOST_BYTE_FUEL)
}

fn guest_range(memory_len: usize, ptr: i32, len: i32) -> Option<Range<usize>> {
    let start = usize::try_from(ptr as u32).ok()?;
    let len = usize::try_from(len).ok()?;
    let end = start.checked_add(len)?;
    (end <= memory_len).then_some(start..end)
}
fn caller_memory(caller: &Caller<'_, HostState>) -> Result<Memory, wasmi::Error> {
    caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| wasmi::Error::new("module does not export memory"))
}

/// Byte range inside `text` on char boundaries, from untrusted JSON numbers.
fn text_range(text: &str, start: &Value, end: &Value) -> Result<Range<usize>, String> {
    let start = start.as_u64().and_then(|v| usize::try_from(v).ok());
    let end = end.as_u64().and_then(|v| usize::try_from(v).ok());
    match (start, end) {
        (Some(start), Some(end))
            if start <= end
                && end <= text.len()
                && text.is_char_boundary(start)
                && text.is_char_boundary(end) =>
        {
            Ok(start..end)
        }
        _ => Err("range must be ordered byte offsets on character boundaries".into()),
    }
}

/// Where a process runs: the document's folder, or the open folder.
fn process_directory(document: Option<&DocumentSnapshot>, place: &str) -> Option<PathBuf> {
    let folder = || {
        document
            .map(|d| d.path.as_path())
            .filter(|path| path.is_absolute())
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    };
    let workspace = || document.and_then(|d| d.workspace.clone());
    let candidates = match place {
        "workspace" => [workspace(), folder()],
        _ => [folder(), workspace()],
    };
    // Unsaved documents without an open folder run in the home directory, or
    // the temporary directory where there is none (build sandboxes, services).
    candidates
        .into_iter()
        .flatten()
        .chain(std::env::home_dir())
        .chain([std::env::temp_dir()])
        .find(|dir| dir.is_dir())
}

/// Runs a declared and granted program. Returns the reply and the number of
/// document bytes piped to it, which the caller charges as fuel.
fn run_process(state: &mut HostState, request: &Value) -> (Value, usize) {
    let error = |message: String| (json!({ "error": message }), 0);
    if !matches!(state.call, Call::Task | Call::Environment) {
        return error("programs can only run in commands, formatters and environments".into());
    }
    let environment = state.call == Call::Environment;
    let Some(program) = request.get("program").and_then(Value::as_str) else {
        return error("`program` must be a string".into());
    };
    if !state.access.programs.iter().any(|p| p == program) {
        return error(format!(
            "`{program}` is not declared in [capabilities] process"
        ));
    }
    if !state.access.grants.allows(&state.access.extension, program) {
        state.refused.insert(program.to_owned());
        return error(process::permission_hint(&state.access.extension, program));
    }
    if state.runs >= process::MAX_RUNS_PER_CALL {
        return error(format!(
            "at most {} programs may run in one call",
            process::MAX_RUNS_PER_CALL
        ));
    }
    let args = match request.get("args") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) if items.len() <= process::MAX_ARGS => {
            match items
                .iter()
                .map(|item| {
                    item.as_str()
                        .filter(|a| a.len() <= process::MAX_ARG_BYTES && !a.contains('\0'))
                        .map(String::from)
                })
                .collect::<Option<Vec<_>>>()
            {
                Some(args) => args,
                None => {
                    return error(format!(
                        "`args` must be strings of at most {} bytes without NUL",
                        process::MAX_ARG_BYTES
                    ));
                }
            }
        }
        Some(_) => {
            return error(format!(
                "`args` must be an array of at most {} strings",
                process::MAX_ARGS
            ));
        }
    };
    let pipe_document = request.get("stdin_document").and_then(Value::as_bool) == Some(true);
    let stdin = match (request.get("stdin"), pipe_document) {
        (Some(_), true) => return error("give `stdin` or `stdin_document`, not both".into()),
        (Some(Value::String(text)), false) => Some(text.as_bytes().to_vec()),
        (Some(_), false) => return error("`stdin` must be a string".into()),
        (None, true) => match &state.document {
            Some(document) => Some(document.text.as_bytes().to_vec()),
            None => return error("no document is available in this call".into()),
        },
        (None, false) => None,
    };
    let longest = if environment {
        process::MAX_ENVIRONMENT_TIMEOUT
    } else {
        process::MAX_TIMEOUT
    };
    let timeout = match request.get("timeout_ms") {
        None => process::DEFAULT_TIMEOUT,
        Some(ms) => match ms.as_u64().map(Duration::from_millis) {
            Some(t) if !t.is_zero() && t <= longest => t,
            _ => {
                return error(format!(
                    "`timeout_ms` must be between 1 and {}",
                    longest.as_millis()
                ));
            }
        },
    };
    let place = request
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or("document");
    if !matches!(place, "document" | "workspace") {
        return error("`cwd` must be \"document\" or \"workspace\"".into());
    }
    let cwd = match &state.folder {
        // An environment is computed in its folder, whatever `cwd` says.
        Some(folder) if environment => Some(folder.clone()),
        _ => process_directory(state.document.as_ref(), place),
    };
    let Some(cwd) = cwd else {
        return error("no folder is available to run the program in".into());
    };
    // Programs see the overlay (a dev shell's PATH, say), except the one
    // computing it, which starts from the login shell's (or Kureha's own).
    let search = if environment {
        crate::environment::base_var_os("PATH")
    } else {
        crate::environment::var_os("PATH")
    };
    let Some(path) = process::resolve(program, search.as_deref()) else {
        return error(format!("`{program}` was not found on PATH"));
    };
    state.runs += 1;
    let piped = if pipe_document {
        stdin.as_ref().map_or(0, Vec::len)
    } else {
        0
    };
    let reply = match process::run(process::Run {
        program: path,
        args,
        stdin,
        cwd,
        timeout,
        overlay: !environment,
    }) {
        Ok(output) => json!({
            "status": output.status,
            "stdout": output.stdout,
            "stderr": output.stderr,
        }),
        Err(e) => json!({ "error": e }),
    };
    (reply, piped)
}

fn handle_host_request(state: &mut HostState, request: &[u8]) -> (Value, usize) {
    let request: Value = match serde_json::from_slice(request) {
        Ok(value) => value,
        Err(e) => return (json!({"error": format!("invalid JSON: {e}")}), 0),
    };
    let reply = match request.get("op").and_then(Value::as_str) {
        Some("process") => return run_process(state, &request),
        Some("read") => {
            let Some(document) = &state.document else {
                return (json!({"error": "no document is available in this call"}), 0);
            };
            match text_range(&document.text, &request["start"], &request["end"]) {
                Ok(range) if range.len() <= MAX_MESSAGE_BYTES / 2 => {
                    json!({"text": &document.text[range]})
                }
                Ok(_) => json!({"error": "read is too large; request a smaller range"}),
                Err(e) => json!({"error": e}),
            }
        }
        Some("log") => {
            let mut message = request
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if message.len() > MAX_LOG_BYTES {
                let mut end = MAX_LOG_BYTES;
                while !message.is_char_boundary(end) {
                    end -= 1;
                }
                message.truncate(end);
            }
            if state.log.len() < MAX_LOG_ENTRIES {
                state.log.push(message);
            }
            json!({})
        }
        Some(other) => json!({"error": format!("unknown op `{other}`")}),
        None => json!({"error": "`op` is required"}),
    };
    (reply, 0)
}

fn kx_host(mut caller: Caller<'_, HostState>, ptr: i32, len: i32) -> Result<i64, wasmi::Error> {
    let memory = caller_memory(&caller)?;
    let data = memory.data(&caller);
    let Some(range) = guest_range(data.len(), ptr, len) else {
        return Err(wasmi::Error::new(
            "kx_host: request is outside linear memory",
        ));
    };
    if range.len() > MAX_MESSAGE_BYTES {
        return Ok(-1);
    }
    let request = data[range].to_vec();
    charge(&mut caller, HOST_CALL_FUEL + byte_fuel(request.len()))?;
    let state = caller.data_mut();
    let (reply, piped) = handle_host_request(state, &request);
    let reply = serde_json::to_vec(&reply).expect("JSON values always serialize");
    state.pending = None;
    charge(&mut caller, byte_fuel(reply.len()) + byte_fuel(piped))?;
    if reply.len() > MAX_MESSAGE_BYTES {
        return Ok(-1);
    }
    let len = reply.len() as i64;
    caller.data_mut().pending = Some(reply);
    Ok(len)
}

fn kx_host_take(
    mut caller: Caller<'_, HostState>,
    ptr: i32,
    len: i32,
) -> Result<i32, wasmi::Error> {
    let memory = caller_memory(&caller)?;
    let Some(reply) = caller.data_mut().pending.take() else {
        return Ok(-1);
    };
    charge(&mut caller, HOST_CALL_FUEL + byte_fuel(reply.len()))?;
    let data = memory.data_mut(&mut caller);
    match guest_range(data.len(), ptr, len) {
        Some(range) if range.len() == reply.len() => {
            data[range].copy_from_slice(&reply);
            Ok(0)
        }
        _ => Err(wasmi::Error::new(
            "kx_host_take: buffer does not match the pending reply",
        )),
    }
}

pub struct Runtime {
    budgets: Budgets,
    last_fuel: u64,
    store: Store<HostState>,
    memory: Memory,
    alloc: TypedFunc<i32, i32>,
    free: TypedFunc<(i32, i32), ()>,
    call: TypedFunc<(i32, i32), i64>,
}

impl Runtime {
    /// Compiles and instantiates a module without process access, then
    /// sends `activate`.
    pub fn new(wasm: &[u8], budgets: Budgets) -> Result<Self, String> {
        Self::with_access(wasm, budgets, Access::default())
    }
    /// Like `new`, for the extension `access` describes.
    pub fn with_access(wasm: &[u8], budgets: Budgets, access: Access) -> Result<Self, String> {
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, wasm).map_err(|e| format!("invalid module: {e}"))?;
        // Only the documented host functions exist. No WASI, no ambient access.
        if let Some(import) = module
            .imports()
            .find(|i| i.module() != "kureha" || !matches!(i.name(), "kx_host" | "kx_host_take"))
        {
            return Err(format!(
                "imports `{}.{}`, which this host does not provide",
                import.module(),
                import.name()
            ));
        }
        let limits = StoreLimitsBuilder::new()
            .memory_size(MEMORY_LIMIT)
            .instances(1)
            .memories(1)
            .tables(4)
            .table_elements(100_000)
            .build();
        let mut store = Store::new(
            &engine,
            HostState {
                limits,
                document: None,
                pending: None,
                log: Vec::new(),
                access,
                call: Call::Activate,
                runs: 0,
                folder: None,
                refused: Default::default(),
                offered: Vec::new(),
            },
        );
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(budgets.activate)
            .map_err(|e| e.to_string())?;
        let mut linker = Linker::new(&engine);
        linker
            .func_wrap("kureha", "kx_host", kx_host)
            .and_then(|l| l.func_wrap("kureha", "kx_host_take", kx_host_take))
            .map_err(|e| e.to_string())?;
        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .map_err(|e| trap(e).to_string())?;
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or("does not export `memory`")?;
        let alloc = instance
            .get_typed_func(&store, "kx_alloc")
            .map_err(|e| format!("export `kx_alloc`: {e}"))?;
        let free = instance
            .get_typed_func(&store, "kx_free")
            .map_err(|e| format!("export `kx_free`: {e}"))?;
        let call = instance
            .get_typed_func(&store, "kx_call")
            .map_err(|e| format!("export `kx_call`: {e}"))?;
        let mut runtime = Self {
            budgets,
            last_fuel: 0,
            store,
            memory,
            alloc,
            free,
            call,
        };
        runtime
            .invoke(&json!({"kind": "activate"}), budgets.activate, None)
            .map_err(|e| format!("activate {e}"))?;
        Ok(runtime)
    }

    /// Fuel the most recent call used, including host-function charges.
    pub fn last_fuel(&self) -> u64 {
        self.last_fuel
    }

    /// Messages the extension logged so far, oldest first.
    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.store.data_mut().log)
    }
    /// Programs refused since the last call for want of the user's grant.
    pub fn take_refused(&mut self) -> std::collections::BTreeSet<String> {
        std::mem::take(&mut self.store.data_mut().refused)
    }
    /// The command ids the last error reply offered (`"actions"`).
    pub fn take_offered(&mut self) -> Vec<String> {
        std::mem::take(&mut self.store.data_mut().offered)
    }

    fn invoke(
        &mut self,
        message: &Value,
        fuel: u64,
        document: Option<DocumentSnapshot>,
    ) -> Result<Value, CallError> {
        let bytes = serde_json::to_vec(message).expect("JSON values always serialize");
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(CallError::Protocol(
                "message to extension is too large".into(),
            ));
        }
        self.store
            .set_fuel(fuel)
            .map_err(|e| CallError::Trap(e.to_string()))?;
        let state = self.store.data_mut();
        state.document = document;
        state.pending = None;
        state.runs = 0;
        state.call = match message.get("kind").and_then(Value::as_str) {
            Some("activate") => Call::Activate,
            Some("command" | "format") => Call::Task,
            Some("environment") => Call::Environment,
            _ => Call::Query,
        };
        state.folder = message
            .get("folder")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .filter(|_| state.call == Call::Environment);
        let result = self.exchange(&bytes);
        self.last_fuel = fuel.saturating_sub(self.store.get_fuel().unwrap_or(0));
        let state = self.store.data_mut();
        state.document = None;
        state.pending = None;
        let reply = result?;
        state.offered = reply
            .get("actions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|id| id.len() <= 128)
            .take(MAX_ACTIONS)
            .map(String::from)
            .collect();
        if let Some(error) = reply.get("error") {
            return Err(CallError::Extension(
                error
                    .as_str()
                    .unwrap_or("reported an error")
                    .chars()
                    .take(MAX_STATUS)
                    .collect(),
            ));
        }
        Ok(reply)
    }

    fn exchange(&mut self, bytes: &[u8]) -> Result<Value, CallError> {
        let len = bytes.len() as i32;
        let ptr = self.alloc.call(&mut self.store, len).map_err(trap)?;
        let input = guest_range(self.memory.data_size(&self.store), ptr, len)
            .ok_or_else(|| CallError::Trap("kx_alloc returned memory out of bounds".into()))?;
        self.memory.data_mut(&mut self.store)[input].copy_from_slice(bytes);
        let packed = self.call.call(&mut self.store, (ptr, len));
        // Free the request even when the call failed, unless fuel is gone.
        let _ = self.free.call(&mut self.store, (ptr, len));
        let packed = packed.map_err(trap)?;
        let (out_ptr, out_len) = ((packed >> 32) as i32, packed as i32);
        let output = guest_range(self.memory.data_size(&self.store), out_ptr, out_len)
            .filter(|r| r.len() <= MAX_MESSAGE_BYTES)
            .ok_or_else(|| CallError::Trap("reply is outside linear memory or too large".into()))?;
        let reply = serde_json::from_slice(&self.memory.data(&self.store)[output]);
        let _ = self.free.call(&mut self.store, (out_ptr, out_len));
        reply.map_err(|e| CallError::Protocol(format!("invalid JSON: {e}")))
    }

    /// Runs a contributed command against the active document.
    pub fn command(
        &mut self,
        command: &str,
        document: DocumentSnapshot,
    ) -> Result<CommandOutcome, CallError> {
        let message = json!({
            "kind": "command",
            "command": command,
            "document": document.describe(),
        });
        let text = document.text.clone();
        let reply = self.invoke(&message, self.budgets.task, Some(document))?;
        parse_command_reply(&text, &reply).map_err(CallError::Protocol)
    }

    /// Formats the whole document as `language`.
    pub fn format(
        &mut self,
        document: DocumentSnapshot,
        language: &str,
        options: FormatOptions,
    ) -> Result<CommandOutcome, CallError> {
        let message = json!({
            "kind": "format",
            "document": document.describe(),
            "language": language,
            "options": {"tab_size": options.tab_size, "insert_spaces": options.insert_spaces},
        });
        let text = document.text.clone();
        let reply = self.invoke(&message, self.budgets.task, Some(document))?;
        parse_command_reply(&text, &reply).map_err(CallError::Protocol)
    }

    /// Asks for the environment of `folder`: variables to set or (`None`)
    /// to remove.
    pub fn environment(
        &mut self,
        folder: &Path,
    ) -> Result<std::collections::BTreeMap<String, Option<String>>, CallError> {
        let message = json!({
            "kind": "environment",
            "folder": folder.to_string_lossy(),
        });
        let reply = self.invoke(&message, self.budgets.task, None)?;
        let vars = reply
            .get("environment")
            .ok_or_else(|| CallError::Protocol("missing `environment`".into()))?;
        crate::environment::parse_exported(&vars.to_string()).map_err(CallError::Protocol)
    }

    /// Asks for decorations within `range` of the document.
    pub fn decorations(
        &mut self,
        document: DocumentSnapshot,
        range: Range<usize>,
    ) -> Result<Vec<Decoration>, CallError> {
        let message = json!({
            "kind": "decorations",
            "document": document.describe(),
            "range": [range.start, range.end],
        });
        let text = document.text.clone();
        let reply = self.invoke(&message, self.budgets.query, Some(document))?;
        parse_decorations_reply(&text, &range, &reply).map_err(CallError::Protocol)
    }
}

fn parse_command_reply(text: &str, reply: &Value) -> Result<CommandOutcome, String> {
    let mut outcome = CommandOutcome::default();
    if let Some(edits) = reply.get("edits") {
        let edits = edits.as_array().ok_or("`edits` must be an array")?;
        if edits.len() > MAX_EDITS {
            return Err(format!("more than {MAX_EDITS} edits"));
        }
        for edit in edits {
            let range =
                text_range(text, &edit["start"], &edit["end"]).map_err(|e| format!("edit {e}"))?;
            let inserted = edit["text"]
                .as_str()
                .ok_or("edit `text` must be a string")?;
            outcome.edits.push(TextEdit {
                range,
                text: inserted.into(),
            });
        }
        outcome.edits.sort_by_key(|e| (e.range.start, e.range.end));
        if outcome
            .edits
            .windows(2)
            .any(|pair| pair[0].range.end > pair[1].range.start)
        {
            return Err("edits overlap".into());
        }
    }
    if let Some(status) = reply.get("status") {
        let status = status.as_str().ok_or("`status` must be a string")?;
        outcome.status = Some(
            status
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_STATUS)
                .collect(),
        );
    }
    Ok(outcome)
}

fn parse_decorations_reply(
    text: &str,
    requested: &Range<usize>,
    reply: &Value,
) -> Result<Vec<Decoration>, String> {
    let items = reply
        .get("decorations")
        .and_then(Value::as_array)
        .ok_or("`decorations` must be an array")?;
    if items.len() > MAX_DECORATIONS {
        return Err(format!("more than {MAX_DECORATIONS} decorations"));
    }
    items
        .iter()
        .map(|item| {
            let range = text_range(text, &item["start"], &item["end"])
                .map_err(|e| format!("decoration {e}"))?;
            if range.start < requested.start || range.end > requested.end {
                return Err("decoration lies outside the requested range".into());
            }
            let style = match item["style"].as_str() {
                Some("highlight") => DecorationStyle::Highlight,
                Some("info") => DecorationStyle::Info,
                Some("warning") => DecorationStyle::Warning,
                Some("error") => DecorationStyle::Error,
                _ => {
                    return Err(
                        "decoration `style` must be highlight, info, warning or error".into(),
                    );
                }
            };
            Ok(Decoration { range, style })
        })
        .collect()
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
pub(crate) mod tests;
