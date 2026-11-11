//! The extension worker. WebAssembly never runs on the UI thread: the UI
//! queues requests here and drains replies from its poll loop.

use super::{
    Registry,
    runtime::{
        Budgets, CallError, CommandOutcome, Decoration, DocumentSnapshot, FormatOptions,
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

pub struct Host {
    requests: Sender<Request>,
    replies: Mutex<Receiver<Reply>>,
}

/// Request ids are unique in the process, not just per worker, so callers
/// can keep one table of pending requests.
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

enum Slot {
    Ready(Box<Runtime>),
    Failed(String),
}

struct Worker {
    registry: &'static Registry,
    budgets: Budgets,
    slots: HashMap<String, Slot>,
    /// Consecutive budget overruns, kept apart because overrun instances are dropped.
    overruns: HashMap<String, u32>,
}

impl Worker {
    fn slot(&mut self, extension: &str) -> &mut Slot {
        let (registry, budgets) = (self.registry, self.budgets);
        self.slots.entry(extension.to_owned()).or_insert_with(|| {
            match instantiate(registry, extension, budgets) {
                Ok(runtime) => Slot::Ready(Box::new(runtime)),
                Err(e) => Slot::Failed(e),
            }
        })
    }
    fn run<T>(
        &mut self,
        extension: &str,
        call: impl FnOnce(&mut Runtime) -> Result<T, CallError>,
    ) -> Result<T, String> {
        let name = self
            .registry
            .extensions
            .iter()
            .find(|e| e.manifest.id == extension)
            .map_or(extension, |e| e.manifest.name.as_str())
            .to_owned();
        let runtime = match self.slot(extension) {
            Slot::Ready(runtime) => runtime,
            Slot::Failed(reason) => return Err(format!("{name} is not available: {reason}")),
        };
        let result = call(runtime);
        for line in runtime.take_log() {
            eprintln!("[extension {extension}] {line}");
        }
        match result {
            Ok(value) => {
                self.overruns.remove(extension);
                Ok(value)
            }
            Err(CallError::Fuel) => {
                // Fuel can run out mid-update of the guest's own state (its
                // allocator, say), so the instance is discarded like a trap.
                self.slots.remove(extension);
                let overruns = self.overruns.entry(extension.to_owned()).or_default();
                *overruns += 1;
                let message = format!("{name} {}", CallError::Fuel);
                if *overruns >= MAX_FUEL_FAILURES {
                    self.slots.insert(
                        extension.to_owned(),
                        Slot::Failed("it repeatedly exceeded its time budget".into()),
                    );
                    return Err(format!("{message}; it is now turned off until restart"));
                }
                Err(message)
            }
            Err(CallError::Trap(e)) => {
                // A trapped instance may hold broken state; start fresh next time.
                self.slots.remove(extension);
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
                let known = self.registry.extensions.iter().any(|e| {
                    e.manifest.id == extension
                        && e.manifest
                            .contributes
                            .commands
                            .iter()
                            .any(|c| c.id == command)
                });
                let result = if known {
                    self.run(&extension, |runtime| runtime.command(&command, document))
                } else {
                    Err(format!("{extension} does not contribute command {command}"))
                };
                Reply::Command { id, result }
            }
            Request::Decorations {
                id,
                extension,
                document,
                range,
            } => Reply::Decorations {
                id,
                result: self.run(&extension, |runtime| runtime.decorations(document, range)),
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
                    self.run(&extension, |runtime| {
                        runtime.format(document, &language, options)
                    })
                } else {
                    Err(format!("{extension} does not format {language}"))
                };
                Reply::Format { id, result }
            }
        }
    }
}

fn instantiate(registry: &Registry, extension: &str, budgets: Budgets) -> Result<Runtime, String> {
    let entry = registry
        .extensions
        .iter()
        .find(|e| e.manifest.id == extension)
        .ok_or("it is not installed")?;
    let path = entry.wasm.as_ref().ok_or("it has no WebAssembly module")?;
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if metadata.len() > MAX_WASM_BYTES {
        return Err(format!(
            "its module exceeds {} MiB",
            MAX_WASM_BYTES / 1024 / 1024
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    Runtime::new(&bytes, budgets)
}

impl Host {
    /// Starts the worker thread for extensions in `registry`.
    pub fn start(registry: &'static Registry, budgets: Budgets) -> Self {
        let (requests, inbox) = mpsc::channel::<Request>();
        let (outbox, replies) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("extension-host".into())
            .spawn(move || {
                let mut worker = Worker {
                    registry,
                    budgets,
                    slots: HashMap::new(),
                    overruns: HashMap::new(),
                };
                for request in inbox {
                    if outbox.send(worker.handle(request)).is_err() {
                        break;
                    }
                }
            });
        if let Err(e) = spawned {
            eprintln!("Cannot start the extension worker: {e}");
        }
        Self {
            requests,
            replies: Mutex::new(replies),
        }
    }
    fn send(&self, request: impl FnOnce(u64) -> Request) -> Option<u64> {
        let id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        self.requests.send(request(id)).ok().map(|()| id)
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
    /// Replies that arrived since the last poll. Never blocks.
    pub fn poll(&self) -> Vec<Reply> {
        self.replies.lock().unwrap().try_iter().collect()
    }
}

static HOST: OnceLock<Host> = OnceLock::new();

/// The process-wide worker, once the registry has loaded.
pub fn host() -> Option<&'static Host> {
    let registry = super::loaded()?;
    Some(HOST.get_or_init(|| Host::start(registry, Budgets::default())))
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
