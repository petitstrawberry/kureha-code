//! LSP workbench integration. All process I/O belongs to the transport workers.
use super::*;
use kureha_code::lsp::{self, Client, Event as LspEvent};
use serde_json::{Value, json};
use std::result::Result;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Snapshot {
    id: DocumentId,
    path: PathBuf,
    revision: u64,
    text: Arc<String>,
    caret: usize,
    saved: bool,
}
#[derive(Clone)]
struct Guard {
    id: DocumentId,
    revision: u64,
    caret: usize,
    navigation: u64,
}
#[derive(Clone)]
pub(super) struct Problem {
    pub path: PathBuf,
    pub range: std::ops::Range<usize>,
    pub message: String,
    pub severity: u64,
    revision: u64,
}
#[derive(Clone)]
struct Choice {
    label: String,
    detail: String,
    item: Value,
}
#[derive(Clone)]
enum Panel {
    Closed,
    Problems,
    Hover(String),
    Completion(Guard, Vec<Choice>),
}
impl Default for Panel {
    fn default() -> Self {
        Self::Closed
    }
}
struct Pending {
    guard: Guard,
    method: String,
    attempts: u8,
    started: Instant,
    params: Value,
    hover_generation: Option<u64>,
}
struct OpenDocument {
    id: DocumentId,
    revision: u64,
    version: i32,
    text: String,
    saved: bool,
}
struct Session {
    client: Option<Client>,
    starting: Option<mpsc::Receiver<Result<Client, String>>>,
    initialize: Option<u64>,
    ready: bool,
    capabilities: Value,
    pending: BTreeMap<u64, Pending>,
    documents: BTreeMap<PathBuf, OpenDocument>,
    root: PathBuf,
    language: String,
    label: String,
    failure: Option<String>,
    started: Instant,
    diagnostic_requests: BTreeMap<PathBuf, (u64, Instant)>,
}
#[derive(Default)]
pub(super) struct Service {
    sessions: BTreeMap<String, Session>,
    problems: Vec<Problem>,
    panel: Panel,
    navigation: Option<(PathBuf, Value, u64)>,
    last_status: String,
    snapshots: BTreeMap<DocumentId, Snapshot>,
    last_poll: Option<Instant>,
    selected_completion: usize,
    hover_generation: u64,
    hover_target: Option<(DocumentId, usize, Point, Instant)>,
    completion_due: Option<(DocumentId, u64, usize, Instant)>,
    hover_sent: bool,
    hover_hide: Option<Instant>,
    chord: Option<Instant>,
}

fn language(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "rs" => Some("rust"),
        "py" | "pyi" => Some("python"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "jsx" => Some("javascriptreact"),
        "ts" => Some("typescript"),
        "tsx" => Some("typescriptreact"),
        "json" | "jsonc" => Some("json"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "hpp" => Some("cpp"),
        _ => None,
    }
}
fn command(language: &str) -> Result<(String, Vec<String>), String> {
    let key = language.to_ascii_uppercase();
    if let Ok(program) = std::env::var(format!("KUREHA_LSP_{key}")) {
        if !program.is_empty() {
            return Ok((program, vec![]));
        }
    }
    let config_path = std::env::var_os("KUREHA_LSP_CONFIG")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
                })
                .map(|base| base.join("kureha-code/lsp.toml"))
        });
    if let Some(path) = config_path.filter(|path| path.is_file()) {
        let source =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let config: toml::Value =
            toml::from_str(&source).map_err(|e| format!("LSP configuration: {e}"))?;
        if let Some(server) = config.get("servers").and_then(|v| v.get(language)) {
            let program = server
                .get("command")
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .ok_or("LSP server command is empty")?;
            let args = server
                .get("args")
                .and_then(|v| v.as_array())
                .map(|args| {
                    args.iter()
                        .map(|v| {
                            v.as_str()
                                .map(String::from)
                                .ok_or("LSP arguments must be strings".to_string())
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?
                .unwrap_or_default();
            return Ok((program.into(), args));
        }
    }
    Ok(match language {
        "rust" => ("rust-analyzer".into(), vec![]),
        "python" => ("pylsp".into(), vec![]),
        "javascript" | "javascriptreact" | "typescript" | "typescriptreact" => {
            ("typescript-language-server".into(), vec!["--stdio".into()])
        }
        "json" => ("vscode-json-language-server".into(), vec!["--stdio".into()]),
        "c" | "cpp" => ("clangd".into(), vec![]),
        _ => return Err("No language server configured".into()),
    })
}
fn session_key(language: &str, root: &Path) -> String {
    format!("{language}:{}", root.display())
}
fn uri(path: &Path) -> String {
    lsp::path_to_uri(path).unwrap_or_default()
}
fn position(text: &str, byte: usize) -> Value {
    let mut byte = byte.min(text.len());
    while !text.is_char_boundary(byte) {
        byte -= 1;
    }
    let before = &text[..byte];
    let line = before.bytes().filter(|b| *b == b'\n').count();
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    json!({"line":line,"character":text[start..byte].encode_utf16().count()})
}
fn offset(text: &str, pos: &Value) -> Option<usize> {
    let line = usize::try_from(pos.get("line")?.as_u64()?).ok()?;
    let units = usize::try_from(pos.get("character")?.as_u64()?).ok()?;
    let start = if line == 0 {
        0
    } else {
        text.match_indices('\n').nth(line - 1)?.0 + 1
    };
    let end = text[start..].find('\n').map_or(text.len(), |n| start + n);
    let mut count = 0;
    for (i, ch) in text[start..end].char_indices() {
        if count == units {
            return Some(start + i);
        }
        count += ch.len_utf16();
        if count > units {
            return None;
        }
    }
    (count == units).then_some(end)
}
fn range(text: &str, value: &Value) -> Option<std::ops::Range<usize>> {
    let start = offset(text, value.get("start")?)?;
    let end = offset(text, value.get("end")?)?;
    (start <= end).then_some(start..end)
}
fn markup(value: &Value) -> String {
    if let Some(s) = value.as_str() {
        s.into()
    } else if let Some(s) = value.get("value").and_then(Value::as_str) {
        s.into()
    } else if let Some(array) = value.as_array() {
        array.iter().map(markup).collect::<Vec<_>>().join("\n")
    } else {
        String::new()
    }
}
fn parse_diagnostics(path: &Path, revision: u64, text: &str, items: &Value) -> Vec<Problem> {
    items
        .as_array()
        .into_iter()
        .flatten()
        .take(500)
        .filter_map(|diagnostic| {
            let range = range(text, diagnostic.get("range")?)?;
            Some(Problem {
                path: path.into(),
                range,
                revision,
                message: diagnostic
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .chars()
                    .take(1000)
                    .collect(),
                severity: diagnostic
                    .get("severity")
                    .and_then(Value::as_u64)
                    .unwrap_or(1),
            })
        })
        .collect()
}
impl Service {
    fn ensure(&mut self, language: &str, root: &Path) -> String {
        let key = session_key(language, root);
        if !self.sessions.contains_key(&key) {
            let (sender, receiver) = mpsc::channel();
            let root = root.to_owned();
            let cwd = root.clone();
            let lang = language.to_string();
            let label = command(language)
                .map(|(program, _)| program)
                .unwrap_or_else(|_| language.into());
            let config = command(language);
            let started = thread::Builder::new()
                .name("kureha-language-server".into())
                .spawn(move || {
                    let result =
                        config.and_then(|(program, args)| Client::spawn(&program, &args, &cwd));
                    let _ = sender.send(result);
                });
            let failure = started.err().map(|e| e.to_string());
            self.sessions.insert(
                key.clone(),
                Session {
                    client: None,
                    starting: Some(receiver),
                    initialize: None,
                    ready: false,
                    capabilities: Value::Null,
                    pending: BTreeMap::new(),
                    documents: BTreeMap::new(),
                    root,
                    language: lang,
                    label,
                    failure,
                    started: Instant::now(),
                    diagnostic_requests: BTreeMap::new(),
                },
            );
        }
        key
    }
}
impl App {
    fn language_snapshots(&self) -> Vec<Snapshot> {
        let mut rt = self.runtime.lock().unwrap();
        let snapshots = rt
            .workspace
            .documents
            .iter()
            .filter_map(|doc| {
                let path = doc.path.clone()?;
                let text = rt
                    .language
                    .snapshots
                    .get(&doc.id)
                    .filter(|old| old.revision == doc.editor.revision() && old.path == path)
                    .map(|old| old.text.clone())
                    .unwrap_or_else(|| Arc::new(doc.editor.document.as_str().to_string()));
                Some(Snapshot {
                    id: doc.id,
                    path,
                    revision: doc.editor.revision(),
                    text,
                    caret: doc.bindings.selection.get().caret.byte,
                    saved: !doc.editor.dirty(),
                })
            })
            .collect::<Vec<_>>();
        rt.language.snapshots = snapshots.iter().cloned().map(|doc| (doc.id, doc)).collect();
        snapshots
    }
    pub(super) fn poll_language(&self) {
        {
            let mut rt = self.runtime.lock().unwrap();
            if rt
                .language
                .last_poll
                .is_some_and(|last| last.elapsed() < Duration::from_millis(150))
            {
                return;
            }
            rt.language.last_poll = Some(Instant::now());
        }
        let snapshots = self.language_snapshots();
        let root = self.runtime.lock().unwrap().explorer.root.clone();
        let active = self.active_id();
        let mut changed = false;
        let mut responses = Vec::new();
        {
            let mut rt = self.runtime.lock().unwrap();
            let service = &mut rt.language;
            for doc in &snapshots {
                if let Some(lang) = language(&doc.path) {
                    service.ensure(
                        lang,
                        root.as_deref()
                            .unwrap_or_else(|| doc.path.parent().unwrap_or(Path::new("/"))),
                    );
                }
            }
            for session in service.sessions.values_mut() {
                if session.failure.is_some() {
                    continue;
                }
                if let Some(receiver) = &session.starting {
                    match receiver.try_recv() {
                        Ok(Ok(client)) => {
                            let root_uri = uri(&session.root);
                            let params = json!({"processId":std::process::id(),"clientInfo":{"name":"Kureha Code","version":env!("CARGO_PKG_VERSION")},"rootUri":root_uri,"initializationOptions":if cfg!(test) {json!({"checkOnSave":false,"cargo":{"buildScripts":{"enable":false}},"procMacro":{"enable":false}})} else {Value::Null},"workspaceFolders":[{"uri":root_uri,"name":session.root.file_name().unwrap_or_default().to_string_lossy()}],"capabilities":{"general":{"positionEncodings":["utf-16"]},"textDocument":{"diagnostic":{"dynamicRegistration":false,"relatedDocumentSupport":false},"synchronization":{"didSave":true},"completion":{"completionItem":{"snippetSupport":false,"insertReplaceSupport":false}},"publishDiagnostics":{"versionSupport":true},"hover":{"contentFormat":["plaintext"]}},"workspace":{"configuration":true,"workspaceFolders":true}}});
                            match client.request("initialize", params) {
                                Ok(id) => session.initialize = Some(id),
                                Err(e) => session.failure = Some(e),
                            }
                            session.client = Some(client);
                            session.starting = None;
                            changed = true;
                        }
                        Ok(Err(e)) => {
                            session.failure = Some(e);
                            session.starting = None;
                            changed = true;
                        }
                        Err(mpsc::TryRecvError::Disconnected) => {
                            session.failure = Some("Language-server worker stopped".into());
                            session.starting = None;
                            changed = true;
                        }
                        Err(mpsc::TryRecvError::Empty) => {}
                    }
                }
                if !session.ready && session.started.elapsed() > Duration::from_secs(30) {
                    session.failure = Some("Language server initialization timed out".into());
                    session.client = None;
                    changed = true;
                    continue;
                }
                let Some(client) = &mut session.client else {
                    continue;
                };
                for event in client.poll() {
                    responses.push((session_key(&session.language, &session.root), event));
                }
                if !session.ready {
                    continue;
                }
                let docs: Vec<_> = snapshots
                    .iter()
                    .filter(|doc| {
                        language(&doc.path) == Some(session.language.as_str())
                            && root
                                .as_deref()
                                .unwrap_or_else(|| doc.path.parent().unwrap_or(Path::new("/")))
                                == session.root
                    })
                    .collect();
                let closed: Vec<_> = session
                    .documents
                    .keys()
                    .filter(|path| !docs.iter().any(|doc| &doc.path == *path))
                    .cloned()
                    .collect();
                for path in closed {
                    let _ = client.notify(
                        "textDocument/didClose",
                        json!({"textDocument":{"uri":uri(&path)}}),
                    );
                    session.documents.remove(&path);
                }
                for doc in docs {
                    let mut failure = None;
                    match session.documents.get_mut(&doc.path) {
                        None => {
                            if let Err(e)=client.notify("textDocument/didOpen",json!({"textDocument":{"uri":uri(&doc.path),"languageId":session.language,"version":1,"text":doc.text.as_str()}})) {failure=Some(e);}
                            session.documents.insert(
                                doc.path.clone(),
                                OpenDocument {
                                    id: doc.id,
                                    revision: doc.revision,
                                    version: 1,
                                    text: doc.text.as_str().to_string(),
                                    saved: doc.saved,
                                },
                            );
                        }
                        Some(old) if old.revision != doc.revision || old.id != doc.id => {
                            old.version = old.version.saturating_add(1);
                            let kind = session
                                .capabilities
                                .get("textDocumentSync")
                                .and_then(|v| {
                                    v.as_u64()
                                        .or_else(|| v.get("change").and_then(Value::as_u64))
                                })
                                .unwrap_or(1);
                            let change = if kind == 2 {
                                json!({"range":{"start":{"line":0,"character":0},"end":position(&old.text,old.text.len())},"text":doc.text.as_str()})
                            } else {
                                json!({"text":doc.text.as_str()})
                            };
                            if let Err(e)=client.notify("textDocument/didChange",json!({"textDocument":{"uri":uri(&doc.path),"version":old.version},"contentChanges":[change]})) {failure=Some(e);}
                            old.id = doc.id;
                            old.revision = doc.revision;
                            old.text = doc.text.as_str().to_string();
                        }
                        _ => {}
                    }
                    if let Some(old) = session.documents.get_mut(&doc.path) {
                        if doc.saved && !old.saved {
                            let include = session
                                .capabilities
                                .pointer("/textDocumentSync/save/includeText")
                                .and_then(Value::as_bool)
                                .unwrap_or(false);
                            let mut params = json!({"textDocument":{"uri":uri(&doc.path)}});
                            if include {
                                params["text"] = json!(doc.text.as_str());
                            }
                            if let Err(e) = client.notify("textDocument/didSave", params) {
                                failure = Some(e);
                            }
                        }
                        old.saved = doc.saved;
                    }
                    if session
                        .capabilities
                        .get("diagnosticProvider")
                        .is_some_and(|v| !v.is_null() && v != &Value::Bool(false))
                        && session
                            .diagnostic_requests
                            .get(&doc.path)
                            .is_none_or(|(rev, time)| {
                                *rev != doc.revision || time.elapsed() > Duration::from_secs(2)
                            })
                        && !session
                            .pending
                            .values()
                            .any(|p| p.method == "textDocument/diagnostic" && p.guard.id == doc.id)
                    {
                        let mut params = json!({"textDocument":{"uri":uri(&doc.path)}});
                        if let Some(identifier) = session
                            .capabilities
                            .pointer("/diagnosticProvider/identifier")
                            .and_then(Value::as_str)
                        {
                            params["identifier"] = json!(identifier);
                        }
                        if let Ok(id) = client.request("textDocument/diagnostic", params.clone()) {
                            session.pending.insert(
                                id,
                                Pending {
                                    guard: Guard {
                                        id: doc.id,
                                        revision: doc.revision,
                                        caret: doc.caret,
                                        navigation: self.navigation.load(Ordering::Relaxed),
                                    },
                                    method: "textDocument/diagnostic".into(),
                                    params,
                                    hover_generation: None,
                                    attempts: 0,
                                    started: Instant::now(),
                                },
                            );
                            session
                                .diagnostic_requests
                                .insert(doc.path.clone(), (doc.revision, Instant::now()));
                        }
                    }
                    if failure.is_some() {
                        session.failure = failure;
                        changed = true;
                    }
                }
                let expired: Vec<_> = session
                    .pending
                    .iter()
                    .filter(|(_, p)| p.started.elapsed() > Duration::from_secs(15))
                    .map(|(id, _)| *id)
                    .collect();
                for id in expired {
                    session.pending.remove(&id);
                    let _ = client.notify("$/cancelRequest", json!({"id":id}));
                }
            }
            let previous = service.problems.len();
            service.problems.retain(|problem| {
                snapshots
                    .iter()
                    .any(|doc| doc.path == problem.path && doc.revision == problem.revision)
            });
            changed |= previous != service.problems.len();
        }
        for (key, event) in responses {
            changed |= self.language_event(&key, event, &snapshots);
        }
        let status = {
            let mut rt = self.runtime.lock().unwrap();
            let service = &mut rt.language;
            let current = snapshots
                .iter()
                .find(|doc| doc.id == active)
                .and_then(|doc| language(&doc.path).map(|lang| (doc, lang)));
            let status = if let Some((doc, lang)) = current {
                let key = session_key(
                    lang,
                    root.as_deref()
                        .unwrap_or_else(|| doc.path.parent().unwrap_or(Path::new("/"))),
                );
                service
                    .sessions
                    .get(&key)
                    .map(|s| {
                        if let Some(e) = &s.failure {
                            format!("{lang}: {e}")
                        } else if s.ready {
                            format!(
                                "{lang} · {} diagnostics",
                                service
                                    .problems
                                    .iter()
                                    .filter(|p| p.path == doc.path)
                                    .count()
                            )
                        } else {
                            format!("{lang}: starting {}…", s.label)
                        }
                    })
                    .unwrap_or_default()
            } else {
                "Plain text".into()
            };
            if status != service.last_status {
                service.last_status = status.clone();
                Some(status)
            } else {
                None
            }
        };
        if let Some(status) = status {
            self.language_status.set(status);
        }
        if changed {
            self.changed();
        }
    }
    fn language_event(&self, key: &str, event: LspEvent, snapshots: &[Snapshot]) -> bool {
        let mut rt = self.runtime.lock().unwrap();
        match event {
            LspEvent::Failed(error) => {
                if let Some(session) = rt.language.sessions.get_mut(key) {
                    session.failure = Some(error);
                    session.client = None;
                }
                true
            }
            LspEvent::Notification { method, params } => {
                if method != "textDocument/publishDiagnostics" {
                    return false;
                }
                let Some(path) = params
                    .get("uri")
                    .and_then(Value::as_str)
                    .and_then(|uri| lsp::uri_to_path(uri).ok())
                else {
                    return false;
                };
                let Some(doc) = snapshots.iter().find(|doc| doc.path == path) else {
                    return false;
                };
                let Some(open) = rt
                    .language
                    .sessions
                    .get(key)
                    .and_then(|s| s.documents.get(&path))
                else {
                    return false;
                };
                if params
                    .get("version")
                    .and_then(Value::as_i64)
                    .is_some_and(|version| version != i64::from(open.version))
                {
                    return false;
                }
                rt.language.problems.retain(|p| p.path != path);
                for diagnostic in params
                    .get("diagnostics")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .take(500)
                {
                    let Some(range) = diagnostic
                        .get("range")
                        .and_then(|value| range(&doc.text, value))
                    else {
                        continue;
                    };
                    let message = diagnostic
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .chars()
                        .take(1000)
                        .collect();
                    rt.language.problems.push(Problem {
                        path: path.clone(),
                        range,
                        message,
                        severity: diagnostic
                            .get("severity")
                            .and_then(Value::as_u64)
                            .unwrap_or(1),
                        revision: doc.revision,
                    });
                }
                true
            }
            LspEvent::Response { id, result, error } => {
                let Some(session) = rt.language.sessions.get_mut(key) else {
                    return false;
                };
                if session.initialize == Some(id) {
                    session.initialize = None;
                    if let Some(error) = error {
                        session.failure = Some(format!("Initialize failed: {error:?}"));
                        return true;
                    }
                    let encoding = result
                        .pointer("/capabilities/positionEncoding")
                        .and_then(Value::as_str)
                        .unwrap_or("utf-16");
                    if encoding != "utf-16" {
                        session.failure =
                            Some(format!("Unsupported position encoding: {encoding}"));
                        return true;
                    }
                    session.capabilities =
                        result.get("capabilities").cloned().unwrap_or(Value::Null);
                    if let Some(client) = &mut session.client {
                        if let Err(error) = client.notify("initialized", json!({})) {
                            session.failure = Some(error);
                            return true;
                        }
                    }
                    session.ready = true;
                    return true;
                }
                let Some(request) = session.pending.remove(&id) else {
                    return false;
                };
                let guard = request.guard;
                let valid = snapshots.iter().any(|doc| {
                    doc.id == guard.id
                        && doc.revision == guard.revision
                        && (request.method == "textDocument/diagnostic" || doc.caret == guard.caret)
                }) && (request.method == "textDocument/diagnostic"
                    || self.navigation.load(Ordering::Relaxed) == guard.navigation);
                if !valid
                    || request
                        .hover_generation
                        .is_some_and(|generation| generation != rt.language.hover_generation)
                {
                    return false;
                }
                if let Some(error) = error {
                    if matches!(error.code, -32800 | -32801 | -32802) {
                        if request.method != "textDocument/diagnostic" && request.attempts < 2 {
                            let session = rt.language.sessions.get_mut(key).unwrap();
                            if let Some(client) = &session.client {
                                if let Ok(id) =
                                    client.request(&request.method, request.params.clone())
                                {
                                    session.pending.insert(
                                        id,
                                        Pending {
                                            guard,
                                            method: request.method,
                                            params: request.params,
                                            hover_generation: request.hover_generation,
                                            attempts: request.attempts + 1,
                                            started: Instant::now(),
                                        },
                                    );
                                }
                            }
                        }
                        return false;
                    }
                    drop(rt);
                    self.status
                        .set(format!("Language server: {}", error.message));
                    return true;
                }
                match request.method.as_str() {
                    "textDocument/diagnostic" => {
                        let Some(doc) = snapshots.iter().find(|doc| doc.id == guard.id) else {
                            return false;
                        };
                        if result.get("kind").and_then(Value::as_str) == Some("full") {
                            rt.language.problems.retain(|p| p.path != doc.path);
                            rt.language.problems.extend(parse_diagnostics(
                                &doc.path,
                                doc.revision,
                                &doc.text,
                                result.get("items").unwrap_or(&Value::Null),
                            ));
                        }
                    }
                    "textDocument/completion" => {
                        let values = result
                            .as_array()
                            .or_else(|| result.get("items").and_then(Value::as_array));
                        let mut items = values
                            .into_iter()
                            .flatten()
                            .filter(|v| {
                                v.get("insertTextFormat").and_then(Value::as_u64) != Some(2)
                            })
                            .take(100)
                            .map(|item| Choice {
                                label: item
                                    .get("label")
                                    .and_then(Value::as_str)
                                    .unwrap_or("Completion")
                                    .into(),
                                detail: item
                                    .get("detail")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .chars()
                                    .take(120)
                                    .collect(),
                                item: item.clone(),
                            })
                            .collect::<Vec<_>>();
                        items.sort_by_key(|item| {
                            item.item
                                .get("sortText")
                                .and_then(Value::as_str)
                                .unwrap_or(&item.label)
                                .to_string()
                        });
                        rt.language.selected_completion = 0;
                        rt.language.panel = if items.is_empty() {
                            Panel::Closed
                        } else {
                            Panel::Completion(guard, items)
                        };
                    }
                    "textDocument/hover" => {
                        rt.language.selected_completion = 0;
                        let text: String = markup(result.get("contents").unwrap_or(&Value::Null))
                            .chars()
                            .take(5000)
                            .collect();
                        rt.language.panel = if text.is_empty() {
                            Panel::Closed
                        } else {
                            Panel::Hover(text)
                        };
                    }
                    "textDocument/definition" => {
                        let location = result.as_array().and_then(|v| v.first()).unwrap_or(&result);
                        let Some(uri) = location
                            .get("uri")
                            .or_else(|| location.get("targetUri"))
                            .and_then(Value::as_str)
                        else {
                            drop(rt);
                            self.status.set("No definition found".into());
                            return true;
                        };
                        let Some(range) = location
                            .get("range")
                            .or_else(|| location.get("targetSelectionRange"))
                        else {
                            return false;
                        };
                        let Ok(path) = lsp::uri_to_path(uri) else {
                            return false;
                        };
                        let range = range.clone();
                        drop(rt);
                        self.language_navigate(path, range);
                        return true;
                    }
                    _ => {}
                }
                true
            }
        }
    }
    pub(super) fn language_request(&self, method: &str) {
        let rect = self.caret_rect.get();
        self.language_anchor
            .set(Point::new(rect.origin.x, rect.origin.y + rect.size.height));
        self.language_request_at(method, None, None, false);
    }
    fn language_request_at(
        &self,
        method: &str,
        byte: Option<usize>,
        hover_generation: Option<u64>,
        quiet: bool,
    ) {
        if self.blocked() {
            return;
        }
        self.runtime.lock().unwrap().language.last_poll = None;
        self.poll_language();
        let id = self.active_id();
        let navigation = self.navigation.load(Ordering::Relaxed);
        let snapshots = self.language_snapshots();
        let Some(doc) = snapshots.iter().find(|doc| doc.id == id) else {
            if !quiet {
                self.status
                    .set("Save this file before using a language server".into());
            }
            return;
        };
        let root = self
            .runtime
            .lock()
            .unwrap()
            .explorer
            .root
            .clone()
            .unwrap_or_else(|| doc.path.parent().unwrap_or(Path::new("/")).into());
        let Some(lang) = language(&doc.path) else {
            if !quiet {
                self.status
                    .set("No language server configured for this file type".into());
            }
            return;
        };
        let mut rt = self.runtime.lock().unwrap();
        let Some(session) = rt.language.sessions.get_mut(&session_key(lang, &root)) else {
            return;
        };
        if !session.ready {
            drop(rt);
            if !quiet {
                self.status
                    .set("Language server is not ready; see its status".into());
            }
            return;
        }
        let capability = match method {
            "textDocument/completion" => "completionProvider",
            "textDocument/definition" => "definitionProvider",
            _ => "hoverProvider",
        };
        if session
            .capabilities
            .get(capability)
            .is_none_or(|v| v.is_null() || v == &Value::Bool(false))
        {
            drop(rt);
            if !quiet {
                self.status
                    .set("The server does not support this command".into());
            }
            return;
        }
        let params = json!({"textDocument":{"uri":uri(&doc.path)},"position":position(&doc.text,byte.unwrap_or(doc.caret).min(doc.text.len())),"context":{"triggerKind":1}});
        let result = session
            .client
            .as_mut()
            .unwrap()
            .request(method, params.clone());
        match result {
            Ok(request) => {
                session.pending.insert(
                    request,
                    Pending {
                        guard: Guard {
                            id,
                            revision: doc.revision,
                            caret: doc.caret,
                            navigation,
                        },
                        method: method.into(),
                        params,
                        hover_generation,
                        attempts: 0,
                        started: Instant::now(),
                    },
                );
            }
            Err(error) => {
                drop(rt);
                self.status.set(error);
            }
        }
    }
    pub(super) fn schedule_completion(&self, id: DocumentId, delta: &EditDelta) {
        let mut rt = self.runtime.lock().unwrap();
        let next = rt.workspace.get(id).map(|doc| {
            (
                doc.editor.revision(),
                doc.bindings.selection.get().caret.byte,
            )
        });
        rt.language.hover_generation = rt.language.hover_generation.wrapping_add(1);
        rt.language.hover_target = None;
        if !matches!(rt.language.panel, Panel::Problems) {
            rt.language.panel = Panel::Closed;
        }
        rt.language.completion_due = next
            .filter(|_| {
                delta
                    .inserted
                    .chars()
                    .last()
                    .is_some_and(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '.' | ':'))
            })
            .map(|(rev, caret)| (id, rev, caret, Instant::now()));
    }
    pub(super) fn pointer_hover(&self, id: DocumentId, hit: Option<(usize, Point)>) {
        let mut rt = self.runtime.lock().unwrap();
        let hit = hit.and_then(|(byte, point)| {
            let doc = rt.workspace.get(id)?;
            let text = doc.editor.document.as_str();
            let ch = text.get(byte..)?.chars().next()?;
            if !ch.is_alphanumeric() && ch != '_' {
                return None;
            }
            let start = text[..byte]
                .char_indices()
                .rev()
                .take_while(|(_, ch)| ch.is_alphanumeric() || *ch == '_')
                .last()
                .map_or(byte, |(start, _)| start);
            Some((start, point))
        });
        if rt
            .language
            .hover_target
            .as_ref()
            .map(|(doc, byte, _, _)| (*doc, *byte))
            == hit.map(|(byte, _)| (id, byte))
        {
            return;
        }
        rt.language.hover_generation = rt.language.hover_generation.wrapping_add(1);
        rt.language.hover_target = hit.map(|(byte, point)| (id, byte, point, Instant::now()));
        rt.language.hover_sent = false;
        rt.language.hover_hide = if hit.is_none() {
            Some(Instant::now())
        } else {
            None
        };
        if hit.is_some() && matches!(rt.language.panel, Panel::Hover(_)) {
            rt.language.panel = Panel::Closed;
            drop(rt);
            self.changed();
        }
    }
    pub(super) fn keep_hover_open(&self) {
        self.runtime.lock().unwrap().language.hover_hide = None;
    }
    pub(super) fn definition_at(&self, id: DocumentId, byte: usize) {
        if id != self.active_id() {
            return;
        }
        let Some(bindings) = self.bindings(id) else {
            return;
        };
        let document = bindings.document.get();
        if byte > document.len() || !document.as_str().is_char_boundary(byte) {
            return;
        }
        bindings.selection.set(TextSelection::collapsed(byte));
        self.language_request("textDocument/definition");
    }
    pub(super) fn poll_language_interactions(&self) {
        let active = self.active_id();
        let hide = {
            let mut rt = self.runtime.lock().unwrap();
            if rt
                .language
                .hover_hide
                .is_some_and(|at| at.elapsed() > Duration::from_millis(300))
            {
                rt.language.hover_hide = None;
                if matches!(rt.language.panel, Panel::Hover(_)) {
                    rt.language.panel = Panel::Closed;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if hide {
            self.changed();
        }
        let (completion, hover) = {
            let mut rt = self.runtime.lock().unwrap();
            let completion = rt
                .language
                .completion_due
                .filter(|(_, _, _, at)| at.elapsed() >= Duration::from_millis(200));
            if completion.is_some() {
                rt.language.completion_due = None;
            }
            let completion = completion.filter(|(id, rev, caret, _)| {
                *id == active
                    && rt.workspace.get(*id).is_some_and(|doc| {
                        doc.editor.revision() == *rev
                            && doc.bindings.selection.get().caret.byte == *caret
                    })
            });
            let hover = rt.language.hover_target.filter(|(id, _, _, at)| {
                !rt.language.hover_sent
                    && *id == active
                    && at.elapsed() >= Duration::from_millis(500)
            });
            if hover.is_some() {
                rt.language.hover_sent = true;
            }
            (
                completion,
                hover.map(|(_, byte, point, _)| (byte, point, rt.language.hover_generation)),
            )
        };
        if completion.is_some() {
            let rect = self.caret_rect.get();
            self.language_anchor
                .set(Point::new(rect.origin.x, rect.origin.y + rect.size.height));
            self.language_request_at("textDocument/completion", None, None, true);
        }
        if let Some((byte, point, generation)) = hover {
            if !matches!(
                self.runtime.lock().unwrap().language.panel,
                Panel::Completion(..)
            ) {
                self.language_anchor
                    .set(Point::new(point.x, point.y + 18.0));
                self.language_request_at("textDocument/hover", Some(byte), Some(generation), true);
            }
        }
    }
    pub(super) fn language_key(
        &self,
        key: KeyCode,
        modifiers: scarlet_ui::event::KeyModifiers,
    ) -> bool {
        let mut rt = self.runtime.lock().unwrap();
        if let Panel::Completion(_, items) = &rt.language.panel {
            let count = items.len();
            let selected = rt.language.selected_completion.min(count.saturating_sub(1));
            if count > 0
                && matches!(
                    key,
                    KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Tab
                )
                && !modifiers.shift
                && !modifiers.primary()
            {
                match key {
                    KeyCode::Up => rt.language.selected_completion = (selected + count - 1) % count,
                    KeyCode::Down => rt.language.selected_completion = (selected + 1) % count,
                    _ => {
                        drop(rt);
                        self.accept_completion(selected);
                        return true;
                    }
                }
                drop(rt);
                self.changed();
                return true;
            }
        }
        if modifiers.primary() && matches!(key, KeyCode::Char('k' | 'K')) {
            rt.language.chord = Some(Instant::now());
            return true;
        }
        let chord = rt
            .language
            .chord
            .take()
            .is_some_and(|at| at.elapsed() < Duration::from_secs(2));
        drop(rt);
        if chord && modifiers.primary() && matches!(key, KeyCode::Char('t' | 'T')) {
            self.set_theme(self.theme.get().toggle());
            return true;
        }
        if chord && modifiers.primary() && matches!(key, KeyCode::Char('m' | 'M')) {
            self.toggle_minimap();
            return true;
        }
        if modifiers.primary() && matches!(key, KeyCode::Char('i' | 'I')) {
            self.language_request(if chord {
                "textDocument/hover"
            } else {
                "textDocument/completion"
            });
            return true;
        }
        false
    }
    pub(super) fn restart_language_server(&self) {
        self.runtime.lock().unwrap().language = Service::default();
        self.status.set("Restarting language servers…".into());
        self.changed();
    }
    pub(super) fn show_problems(&self) {
        self.runtime.lock().unwrap().language.panel = Panel::Problems;
        self.changed();
    }
    pub(super) fn dismiss_language_panel(&self) -> bool {
        let mut rt = self.runtime.lock().unwrap();
        let open = !matches!(rt.language.panel, Panel::Closed);
        rt.language.panel = Panel::Closed;
        rt.language.completion_due = None;
        rt.language.hover_generation = rt.language.hover_generation.wrapping_add(1);
        rt.language.hover_target = None;
        drop(rt);
        if open {
            self.changed();
        }
        open
    }
    fn language_navigate(&self, path: PathBuf, target: Value) {
        let existing = self.runtime.lock().unwrap().workspace.find_path(&path);
        if let Some(id) = existing {
            self.activate(id);
            self.language_select(id, &target);
        } else {
            if self.runtime.lock().unwrap().job.is_some() {
                self.status
                    .set("Finish the file operation before navigating".into());
                return;
            }
            let navigation = self.navigation.load(Ordering::Relaxed);
            self.open(path.clone());
            self.runtime.lock().unwrap().language.navigation = Some((path, target, navigation));
        }
    }
    fn language_select(&self, id: DocumentId, target: &Value) {
        let bindings = {
            let mut rt = self.runtime.lock().unwrap();
            let Some(doc) = rt.workspace.get_mut(id) else {
                return;
            };
            let Some(range) = range(&doc.editor.document.as_str(), target) else {
                return;
            };
            let selection = TextSelection {
                anchor: TextPosition::new(range.start),
                caret: TextPosition::new(range.end),
            };
            doc.editor.moved(selection);
            (doc.bindings.clone(), selection)
        };
        bindings.0.selection.set(bindings.1);
        self.dismiss_language_panel();
        self.status.set("Definition opened".into());
        self.changed();
    }
    pub(super) fn language_opened(&self, path: &Path, id: DocumentId) {
        let target =
            {
                let mut rt = self.runtime.lock().unwrap();
                if rt.language.navigation.as_ref().is_some_and(|(p, _, n)| {
                    p == path && *n == self.navigation.load(Ordering::Relaxed)
                }) {
                    rt.language.navigation.take().map(|(_, range, _)| range)
                } else {
                    rt.language.navigation = None;
                    None
                }
            };
        if let Some(target) = target {
            self.language_select(id, &target);
        }
    }
    fn accept_completion(&self, index: usize) {
        let id = self.active_id();
        if !self.editable(id) {
            return;
        }
        let mut rt = self.runtime.lock().unwrap();
        let Panel::Completion(guard, items) = rt.language.panel.clone() else {
            return;
        };
        let Some(choice) = items.get(index) else {
            return;
        };
        let Some(doc) = rt.workspace.get_mut(id) else {
            return;
        };
        if guard.id != id
            || guard.revision != doc.editor.revision()
            || guard.caret != doc.bindings.selection.get().caret.byte
            || guard.navigation != self.navigation.load(Ordering::Relaxed)
        {
            rt.language.panel = Panel::Closed;
            return;
        }
        let original = doc.editor.document.as_str();
        let mut edits = Vec::new();
        if let Some(edit) = choice.item.get("textEdit") {
            let Some(r) = edit
                .get("range")
                .or_else(|| edit.get("replace"))
                .and_then(|v| range(&original, v))
            else {
                return;
            };
            let Some(text) = edit.get("newText").and_then(Value::as_str) else {
                return;
            };
            edits.push((r, text.to_string()));
        } else {
            let prefix = original[..guard.caret]
                .char_indices()
                .rev()
                .take_while(|(_, ch)| ch.is_alphanumeric() || *ch == '_')
                .last()
                .map_or(guard.caret, |(i, _)| i);
            let text = choice
                .item
                .get("insertText")
                .and_then(Value::as_str)
                .unwrap_or(&choice.label);
            edits.push((prefix..guard.caret, text.into()));
        }
        let primary = edits[0].clone();
        for edit in choice
            .item
            .get("additionalTextEdits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(r) = edit.get("range").and_then(|v| range(&original, v)) else {
                return;
            };
            let Some(text) = edit.get("newText").and_then(Value::as_str) else {
                return;
            };
            edits.push((r, text.into()));
        }
        edits.sort_by_key(|(range, _)| range.start);
        if edits.windows(2).any(|pair| pair[0].0.end > pair[1].0.start) {
            return;
        }
        let shift: isize = edits
            .iter()
            .filter(|(range, _)| range.end <= primary.0.start && range != &primary.0)
            .map(|(range, text)| text.len() as isize - range.len() as isize)
            .sum();
        let mut text = original.to_string();
        for (range, inserted) in edits.into_iter().rev() {
            text.replace_range(range, &inserted);
        }
        let end = (primary.0.start as isize + shift).max(0) as usize + primary.1.len();
        doc.editor.edit_transaction(&EditDelta {
            replaced_range: 0..original.len(),
            inserted: text,
        });
        doc.editor.moved(TextSelection::collapsed(end));
        rt.language.panel = Panel::Closed;
        drop(rt);
        self.sync_editor(id);
        self.changed();
        self.status.set("Completion applied".into());
    }
    pub(super) fn language_diagnostics(&self, id: DocumentId) -> Vec<TextDiagnosticSpan> {
        let rt = self.runtime.lock().unwrap();
        let Some(doc) = rt.workspace.get(id) else {
            return vec![];
        };
        rt.language
            .problems
            .iter()
            .filter(|p| doc.path.as_ref() == Some(&p.path) && p.revision == doc.editor.revision())
            .map(|p| {
                let mut range = p.range.clone();
                if range.is_empty() {
                    if let Some(ch) = doc.editor.document.as_str()[range.end..].chars().next() {
                        range.end += ch.len_utf8();
                    }
                }
                TextDiagnosticSpan {
                    range,
                    color: if p.severity == 1 {
                        Color::rgb(214, 48, 49)
                    } else {
                        Color::rgb(186, 111, 0)
                    },
                }
            })
            .collect()
    }
    pub(super) fn language_popup(&self) -> impl View + Clone + use<> {
        let (panel, selected) = {
            let rt = self.runtime.lock().unwrap();
            (rt.language.panel.clone(), rt.language.selected_completion)
        };
        let colors = self.theme.get().colors();
        let (rows, height) = match panel {
            Panel::Completion(_, items) => {
                let len = items.len();
                (
                    items
                        .into_iter()
                        .enumerate()
                        .map(|(index, choice)| {
                            let app = self.clone();
                            Either::A(
                                Button::new(format!("{}   {}", choice.label, choice.detail))
                                    .header_style()
                                    .font_size(13.0)
                                    .text_color(colors.foreground)
                                    .background_color(if selected == index {
                                        colors.selection
                                    } else {
                                        colors.elevated
                                    })
                                    .on_click(move || app.accept_completion(index))
                                    .padding(5.0)
                                    .frame(420.0, 26.0),
                            )
                        })
                        .collect::<Vec<_>>(),
                    (len.min(8) as f32 * 26.0).max(26.0),
                )
            }
            Panel::Hover(text) => {
                let mut lines = Vec::new();
                for line in text.lines().take(80) {
                    let chars = line.chars().collect::<Vec<_>>();
                    if chars.is_empty() {
                        lines.push(String::new());
                    }
                    for chunk in chars.chunks(52) {
                        lines.push(chunk.iter().collect::<String>());
                    }
                }
                let height = (lines.len() as f32 * 19.0).clamp(19.0, 220.0);
                (
                    lines
                        .into_iter()
                        .map(|line| {
                            Either::B(
                                Text::new(line)
                                    .font_size(13.0)
                                    .color(colors.foreground)
                                    .frame(420.0, 19.0),
                            )
                        })
                        .collect::<Vec<_>>(),
                    height,
                )
            }
            _ => (Vec::new(), 0.0),
        };
        let shown = !rows.is_empty();
        let hovered = self.clone();
        let scroll = ScrollView::new(VStack::new(super::ui::ViewCollection(rows)).spacing(0.0))
            .scroll_to_vertical_range(selected as f32 * 26.0, (selected + 1) as f32 * 26.0)
            .frame(420.0, height);
        if_view!(
            shown,
            super::popup::Anchored::new(
                Surface::floating(scroll)
                    .fill(colors.elevated)
                    .border_color(colors.border)
                    .corner_radius(4.0),
                self.language_anchor.clone()
            )
            .on_event_capture(move |event| {
                if matches!(
                    event,
                    Event::Mouse(
                        scarlet_ui::event::MouseEvent::Moved { .. }
                            | scarlet_ui::event::MouseEvent::Entered { .. }
                    )
                ) {
                    hovered.keep_hover_open();
                }
                false
            }),
            VStack::new(())
        )
    }
    pub(super) fn language_panel(&self) -> impl View + Clone + use<> {
        let (panel, problems) = {
            let rt = self.runtime.lock().unwrap();
            (rt.language.panel.clone(), rt.language.problems.clone())
        };
        let is_open = matches!(panel, Panel::Problems);
        let close = self.clone();
        let (title, entries): (
            String,
            Vec<(
                String,
                Option<usize>,
                Option<(PathBuf, std::ops::Range<usize>)>,
            )>,
        ) = match panel {
            Panel::Closed => (String::new(), vec![]),
            Panel::Completion(_, items) => (
                if items.is_empty() {
                    "No completions"
                } else {
                    "Completions · choose to insert"
                }
                .into(),
                items
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (format!("{}  {}", c.label, c.detail), Some(i), None))
                    .collect(),
            ),
            Panel::Hover(text) => (
                "Symbol information".into(),
                vec![(
                    if text.is_empty() {
                        "No information".into()
                    } else {
                        text
                    },
                    None,
                    None,
                )],
            ),
            Panel::Problems => (
                format!("Problems ({})", problems.len()),
                problems
                    .iter()
                    .take(100)
                    .map(|p| {
                        (
                            format!(
                                "{} · {} · {}",
                                p.path.file_name().unwrap_or_default().to_string_lossy(),
                                if p.severity == 1 { "Error" } else { "Warning" },
                                p.message
                            ),
                            None,
                            Some((p.path.clone(), p.range.clone())),
                        )
                    })
                    .collect(),
            ),
        };
        let rows=entries.into_iter().map(|(label,completion,target)|{
            let app=self.clone();Button::new(label).header_style().font_size(12.0).padding(5.0).on_click(move ||{
                if let Some(index)=completion {app.accept_completion(index);} else if let Some((path,range))=&target {
                    let text={let rt=app.runtime.lock().unwrap();rt.workspace.find_path(path).and_then(|id|rt.workspace.get(id)).map(|doc|doc.editor.document.as_str().to_string())};
                    if let Some(text)=text {app.language_navigate(path.clone(),json!({"start":position(&text,range.start),"end":position(&text,range.end)}));}
                }
            }).frame_width(f32::INFINITY)
        }).collect::<Vec<_>>();
        if_view!(is_open,vstack!{hstack!{Text::new(title).font_size(12.0),Spacer::new(),Button::new("×").header_style().on_click(move ||{close.dismiss_language_panel();}),}.spacing(8.0),ScrollView::new(VStack::new(super::ui::ViewCollection(rows)).spacing(0.0)).frame(f32::INFINITY,120.0),}.spacing(4.0).padding(8.0).background(self.theme.get().colors().surface),VStack::new(()))
    }
}

#[cfg(test)]
#[path = "language_tests.rs"]
mod tests;
