//! The extension workers. WebAssembly never runs on the UI thread: the UI
//! queues requests here and drains replies from its poll loop. Each extension
//! has its own worker thread ("lane"), so one that waits (on a process, say)
//! or runs long never holds up the others.

use super::{
    Extension, Registry,
    process::Grants,
    runtime::{
        Access, Budgets, CallError, CommandOutcome, Decoration, DocumentSnapshot, FormatOptions,
        MAX_WASM_BYTES, Runtime,
    },
};
use std::{
    collections::HashMap,
    ops::Range,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
};

/// Consecutive budget overruns after which an extension is turned off.
const MAX_FUEL_FAILURES: u32 = 3;

enum Request {
    Command {
        id: u64,
        extension: String,
        command: String,
        document: DocumentSnapshot,
    },
    Decorations {
        id: u64,
        extension: String,
        document: DocumentSnapshot,
        range: Range<usize>,
    },
    Format {
        id: u64,
        extension: String,
        language: String,
        document: DocumentSnapshot,
        options: FormatOptions,
    },
}

#[derive(Debug)]
pub enum Reply {
    Command {
        id: u64,
        result: Result<CommandOutcome, String>,
    },
    Decorations {
        id: u64,
        result: Result<Vec<Decoration>, String>,
    },
    Format {
        id: u64,
        result: Result<CommandOutcome, String>,
    },
}

impl Request {
    fn extension(&self) -> &str {
        match self {
            Self::Command { extension, .. }
            | Self::Decorations { extension, .. }
            | Self::Format { extension, .. } => extension,
        }
    }
    /// The reply for a request that never reached a lane.
    fn refuse(self, reason: String) -> Reply {
        match self {
            Self::Command { id, .. } => Reply::Command {
                id,
                result: Err(reason),
            },
            Self::Decorations { id, .. } => Reply::Decorations {
                id,
                result: Err(reason),
            },
            Self::Format { id, .. } => Reply::Format {
                id,
                result: Err(reason),
            },
        }
    }
}

pub struct Host {
    registry: &'static Registry,
    budgets: Budgets,
    /// Programs the user allowed; shared with every instance.
    grants: Grants,
    /// One request queue per extension that has been used, created on demand.
    lanes: Mutex<HashMap<String, Sender<Request>>>,
    outbox: Sender<Reply>,
    replies: Mutex<Receiver<Reply>>,
}

/// Request ids are unique in the process, not just per worker, so callers
/// can keep one table of pending requests.
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

enum Slot {
    Ready(Box<Runtime>),
    Failed(String),
}

/// One extension's worker state; lives on that extension's thread.
struct Lane {
    registry: &'static Registry,
    extension: &'static Extension,
    budgets: Budgets,
    grants: Grants,
    slot: Option<Slot>,
    /// Consecutive budget overruns, kept apart because overrun instances are dropped.
    overruns: u32,
}

impl Lane {
    fn run<T>(
        &mut self,
        call: impl FnOnce(&mut Runtime) -> Result<T, CallError>,
    ) -> Result<T, String> {
        let (id, name) = (
            self.extension.manifest.id.as_str(),
            self.extension.manifest.name.as_str(),
        );
        let (extension, budgets) = (self.extension, self.budgets);
        let grants = self.grants.clone();
        let slot = self
            .slot
            .get_or_insert_with(|| match instantiate(extension, budgets, grants) {
                Ok(runtime) => Slot::Ready(Box::new(runtime)),
                Err(e) => Slot::Failed(e),
            });
        let runtime = match slot {
            Slot::Ready(runtime) => runtime,
            Slot::Failed(reason) => return Err(format!("{name} is not available: {reason}")),
        };
        let result = call(runtime);
        for line in runtime.take_log() {
            eprintln!("[extension {id}] {line}");
        }
        match result {
            Ok(value) => {
                self.overruns = 0;
                Ok(value)
            }
            Err(CallError::Fuel) => {
                // Fuel can run out mid-update of the guest's own state (its
                // allocator, say), so the instance is discarded like a trap.
                self.slot = None;
                self.overruns += 1;
                let message = format!("{name} {}", CallError::Fuel);
                if self.overruns >= MAX_FUEL_FAILURES {
                    self.slot = Some(Slot::Failed(
                        "it repeatedly exceeded its time budget".into(),
                    ));
                    return Err(format!("{message}; it is now turned off until restart"));
                }
                Err(message)
            }
            Err(CallError::Trap(e)) => {
                // A trapped instance may hold broken state; start fresh next time.
                self.slot = None;
                Err(format!("{name} {}", CallError::Trap(e)))
            }
            Err(e @ CallError::Protocol(_)) => Err(format!("{name} {e}")),
            Err(CallError::Extension(e)) => Err(format!("{name}: {e}")),
        }
    }
    fn handle(&mut self, request: Request) -> Reply {
        match request {
            Request::Command {
                id,
                extension,
                command,
                document,
            } => {
                let known = self
                    .extension
                    .manifest
                    .contributes
                    .commands
                    .iter()
                    .any(|c| c.id == command);
                let result = if known {
                    self.run(|runtime| runtime.command(&command, document))
                } else {
                    Err(format!("{extension} does not contribute command {command}"))
                };
                Reply::Command { id, result }
            }
            Request::Decorations {
                id,
                document,
                range,
                ..
            } => Reply::Decorations {
                id,
                result: self.run(|runtime| runtime.decorations(document, range)),
            },
            Request::Format {
                id,
                extension,
                language,
                document,
                options,
            } => {
                let known = self
                    .registry
                    .formatters(&language)
                    .any(|e| e.manifest.id == extension);
                let result = if known {
                    self.run(|runtime| runtime.format(document, &language, options))
                } else {
                    Err(format!("{extension} does not format {language}"))
                };
                Reply::Format { id, result }
            }
        }
    }
}

fn instantiate(extension: &Extension, budgets: Budgets, grants: Grants) -> Result<Runtime, String> {
    let path = extension
        .wasm
        .as_ref()
        .ok_or("it has no WebAssembly module")?;
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if metadata.len() > MAX_WASM_BYTES {
        return Err(format!(
            "its module exceeds {} MiB",
            MAX_WASM_BYTES / 1024 / 1024
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let access = Access {
        extension: extension.manifest.id.clone(),
        programs: extension.manifest.capabilities.process.clone(),
        grants,
    };
    Runtime::with_access(&bytes, budgets, access)
}

impl Host {
    /// Prepares workers for extensions in `registry`. Each extension's thread
    /// starts when it is first used. `grants` decides which declared programs
    /// may run; later changes to it apply at once.
    pub fn start(registry: &'static Registry, budgets: Budgets, grants: Grants) -> Self {
        let (outbox, replies) = mpsc::channel();
        Self {
            registry,
            budgets,
            grants,
            lanes: Mutex::new(HashMap::new()),
            outbox,
            replies: Mutex::new(replies),
        }
    }
    /// The queue for `extension`, starting its thread on first use.
    fn lane(&self, extension: &str) -> Result<Sender<Request>, String> {
        // Checked on every request, so turning an extension off stops it from
        // running even if a caller missed it.
        if !super::is_enabled(extension) {
            return Err(format!("{extension} is disabled"));
        }
        let mut lanes = self.lanes.lock().unwrap();
        if let Some(lane) = lanes.get(extension) {
            return Ok(lane.clone());
        }
        let entry = self
            .registry
            .extensions
            .iter()
            .find(|e| e.manifest.id == extension)
            .ok_or_else(|| format!("{extension} is not available: it is not installed"))?;
        let (requests, inbox) = mpsc::channel::<Request>();
        let outbox = self.outbox.clone();
        let mut lane = Lane {
            registry: self.registry,
            extension: entry,
            budgets: self.budgets,
            grants: self.grants.clone(),
            slot: None,
            overruns: 0,
        };
        std::thread::Builder::new()
            .name(format!("extension {extension}"))
            .spawn(move || {
                for request in inbox {
                    if outbox.send(lane.handle(request)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| format!("Cannot start the worker for {extension}: {e}"))?;
        lanes.insert(extension.to_owned(), requests.clone());
        Ok(requests)
    }
    fn send(&self, request: impl FnOnce(u64) -> Request) -> Option<u64> {
        let id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        let request = request(id);
        let lane = match self.lane(request.extension()) {
            Ok(lane) => lane,
            // Answer through the normal reply path so callers see one flow.
            Err(reason) => return self.outbox.send(request.refuse(reason)).ok().map(|()| id),
        };
        match lane.send(request) {
            Ok(()) => Some(id),
            Err(mpsc::SendError(request)) => {
                let reason = format!("the worker for {} has stopped", request.extension());
                self.outbox.send(request.refuse(reason)).ok().map(|()| id)
            }
        }
    }
    /// Queues a command. `None` if the worker is gone.
    pub fn command(
        &self,
        extension: &str,
        command: &str,
        document: DocumentSnapshot,
    ) -> Option<u64> {
        self.send(|id| Request::Command {
            id,
            extension: extension.into(),
            command: command.into(),
            document,
        })
    }
    /// Queues a decorations query. `None` if the worker is gone.
    pub fn decorations(
        &self,
        extension: &str,
        document: DocumentSnapshot,
        range: Range<usize>,
    ) -> Option<u64> {
        self.send(|id| Request::Decorations {
            id,
            extension: extension.into(),
            document,
            range,
        })
    }
    /// Queues a whole-document format. `None` if the worker is gone.
    pub fn format(
        &self,
        extension: &str,
        language: &str,
        document: DocumentSnapshot,
        options: FormatOptions,
    ) -> Option<u64> {
        self.send(|id| Request::Format {
            id,
            extension: extension.into(),
            language: language.into(),
            document,
            options,
        })
    }
    /// The grants this worker checks, to update when the settings change.
    pub fn grants(&self) -> &Grants {
        &self.grants
    }
    /// Replies that arrived since the last poll. Never blocks.
    pub fn poll(&self) -> Vec<Reply> {
        self.replies.lock().unwrap().try_iter().collect()
    }
}

static HOST: OnceLock<Host> = OnceLock::new();

/// The process-wide worker, once the registry has loaded.
pub fn host() -> Option<&'static Host> {
    let registry = super::loaded()?;
    Some(HOST.get_or_init(|| {
        Host::start(
            registry,
            Budgets::default(),
            super::process::grants().clone(),
        )
    }))
}
/// The worker if something already started it.
pub fn running() -> Option<&'static Host> {
    HOST.get()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::runtime::tests::{Guest, snapshot};
    use std::time::{Duration, Instant};

    fn install(name: &str, extensions: &[(&str, Option<Vec<u8>>)]) -> &'static Registry {
        let dir = std::env::temp_dir().join(format!(
            "kureha-extension-host-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        for (id, wasm) in extensions {
            let folder = dir.join(id);
            std::fs::create_dir_all(&folder).unwrap();
            let module = if let Some(bytes) = wasm {
                std::fs::write(folder.join("ext.wasm"), bytes).unwrap();
                "wasm = 'ext.wasm'\n"
            } else {
                ""
            };
            std::fs::write(
                folder.join("extension.toml"),
                format!(
                    "[extension]\nid='{id}'\nname='Demo {id}'\nversion='1'\napi=1\n{module}\
                     [[contributes.commands]]\nid='demo.run'\ntitle='Run'\n"
                ),
            )
            .unwrap();
        }
        let registry = Registry::load(&dir);
        assert!(registry.errors.is_empty(), "{:?}", registry.errors);
        Box::leak(Box::new(registry))
    }
    fn start(registry: &'static Registry) -> Host {
        // Small budgets keep overrun tests fast in unoptimised builds.
        Host::start(
            registry,
            Budgets {
                activate: 1_000_000,
                query: 100_000,
                task: 1_000_000,
            },
            Grants::default(),
        )
    }
    fn wait(host: &Host, id: u64) -> Reply {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(reply) = host.poll().into_iter().find(|reply| match reply {
                Reply::Command { id: got, .. }
                | Reply::Decorations { id: got, .. }
                | Reply::Format { id: got, .. } => *got == id,
            }) {
                return reply;
            }
            assert!(Instant::now() < deadline, "no reply for request {id}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn command(host: &Host, extension: &str, command: &str) -> Result<CommandOutcome, String> {
        let id = host.command(extension, command, snapshot("hello")).unwrap();
        match wait(host, id) {
            Reply::Command { result, .. } => result,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn commands_run_on_the_worker_and_are_checked_against_the_manifest() {
        let registry = install(
            "commands",
            &[
                (
                    "ex.ok",
                    Some(Guest::new().replying(r#"{"status":"ran"}"#).wasm()),
                ),
                ("ex.empty", None),
            ],
        );
        let host = start(registry);
        assert_eq!(
            command(&host, "ex.ok", "demo.run")
                .unwrap()
                .status
                .as_deref(),
            Some("ran")
        );
        assert!(
            command(&host, "ex.ok", "demo.other")
                .unwrap_err()
                .contains("does not contribute")
        );
        assert!(
            command(&host, "ex.empty", "demo.run")
                .unwrap_err()
                .contains("no WebAssembly module")
        );
        let id = host.decorations("ex.ok", snapshot("hello"), 0..5).unwrap();
        assert!(matches!(
            wait(&host, id),
            Reply::Decorations { result: Err(e), .. } if e.contains("decorations")
        ));
    }

    #[test]
    fn repeated_budget_overruns_turn_an_extension_off() {
        let registry = install(
            "fuel",
            &[(
                "ex.spin",
                Some(Guest::new().body("(loop $l (br $l)) (i64.const 0)").wasm()),
            )],
        );
        let host = start(registry);
        for _ in 1..MAX_FUEL_FAILURES {
            assert!(
                command(&host, "ex.spin", "demo.run")
                    .unwrap_err()
                    .contains("too long")
            );
        }
        assert!(
            command(&host, "ex.spin", "demo.run")
                .unwrap_err()
                .contains("turned off")
        );
        assert!(
            command(&host, "ex.spin", "demo.run")
                .unwrap_err()
                .contains("not available")
        );
    }

    #[test]
    fn a_trap_restarts_the_instance() {
        let registry = install(
            "trap",
            &[("ex.trap", Some(Guest::new().body("unreachable").wasm()))],
        );
        let host = start(registry);
        for _ in 0..2 {
            assert!(
                command(&host, "ex.trap", "demo.run")
                    .unwrap_err()
                    .contains("crashed")
            );
        }
    }
}
