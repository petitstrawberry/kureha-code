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
    References(Vec<Location>, bool),
    Rename(Guard, State<String>),
}
impl Default for Panel {
    fn default() -> Self {
        Self::Closed
    }
}
#[derive(Clone)]
struct Location {
    path: PathBuf,
    range: Value,
}
struct Pending {
    guard: Guard,
    method: String,
    attempts: u8,
    started: Instant,
    params: Value,
    hover_generation: Option<u64>,
    document_version: Option<i32>,
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
const MAX_LANGUAGE_EDITS: usize = 4096;
const MAX_LANGUAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_REFERENCES: usize = 1000;

fn symbol_at(text: &str, caret: usize) -> Option<&str> {
    if !text.is_char_boundary(caret) {
        return None;
    }
    let identifier = |ch: char| ch.is_alphanumeric() || matches!(ch, '_' | '$');
    let start = text[..caret]
        .char_indices()
        .rev()
        .take_while(|(_, ch)| identifier(*ch))
        .last()
        .map_or(caret, |(i, _)| i);
    let end = text[caret..]
        .char_indices()
        .take_while(|(_, ch)| identifier(*ch))
        .last()
        .map_or(caret, |(i, ch)| caret + i + ch.len_utf8());
    (start < end).then(|| &text[start..end])
}
fn valid_symbol_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 256
        && chars
            .next()
            .is_some_and(|ch| ch.is_alphabetic() || matches!(ch, '_' | '$'))
        && chars.all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '$'))
}
fn locations(result: &Value) -> Result<(Vec<Location>, bool), String> {
    if result.is_null() {
        return Ok((vec![], false));
    }
    let items = result.as_array().ok_or("Invalid reference response")?;
    let mut found = Vec::new();
    for item in items.iter().take(MAX_REFERENCES) {
        let path = item
            .get("uri")
            .and_then(Value::as_str)
            .ok_or("Reference is missing its URI")?;
        let path = lsp::uri_to_path(path).map_err(|_| "Reference uses an unsupported URI")?;
        let range = item.get("range").ok_or("Reference is missing its range")?;
        for endpoint in ["start", "end"] {
            let point = range.get(endpoint).ok_or("Invalid reference range")?;
            if point.get("line").and_then(Value::as_u64).is_none()
                || point.get("character").and_then(Value::as_u64).is_none()
            {
                return Err("Invalid reference position".into());
            }
        }
        found.push(Location {
            path,
            range: range.clone(),
        });
    }
    found.sort_by_key(|location| {
        (
            location.path.clone(),
            location
                .range
                .pointer("/start/line")
                .and_then(Value::as_u64),
            location
                .range
                .pointer("/start/character")
                .and_then(Value::as_u64),
        )
    });
    found.dedup_by(|a, b| a.path == b.path && a.range == b.range);
    Ok((found, items.len() > MAX_REFERENCES))
}

/// Refuse the entire WorkspaceEdit when it reaches outside the active document.
/// No file writes or partial cross-file rename can occur through this path.
fn rename_edits(value: &Value, path: &Path, version: Option<i32>) -> Result<Vec<Value>, String> {
    if value.is_null() {
        return Ok(vec![]);
    }
    if !value.is_object() {
        return Err("Invalid rename response; no changes applied".into());
    }
    if value.get("changes").is_some() && value.get("documentChanges").is_some() {
        return Err("Ambiguous rename response; no changes applied".into());
    }
    let mut edits = Vec::new();
    let mut collect = |document_uri: &str, list: &Value| -> Result<(), String> {
        if lsp::uri_to_path(document_uri).ok().as_deref() != Some(path) {
            return Err("Rename affects other files. Only current-file rename is supported; no changes applied".into());
        }
        let list = list
            .as_array()
            .ok_or("Invalid rename edits; no changes applied")?;
        if edits.len().saturating_add(list.len()) > MAX_LANGUAGE_EDITS {
            return Err("Rename exceeds 4096 edits; no changes applied".into());
        }
        if list.iter().any(|edit| edit.get("annotationId").is_some()) {
            return Err("Rename requires change annotations; no changes applied".into());
        }
        edits.extend(list.iter().cloned());
        Ok(())
    };
    if let Some(changes) = value.get("changes") {
        for (document_uri, list) in changes.as_object().ok_or("Invalid rename changes")? {
            collect(document_uri, list)?;
        }
    }
    if let Some(changes) = value.get("documentChanges") {
        for document in changes
            .as_array()
            .ok_or("Invalid rename document changes")?
        {
            if document.get("kind").is_some() {
                return Err("Rename requires file operations; no changes applied".into());
            }
            let target = document
                .get("textDocument")
                .ok_or("Missing rename target")?;
            let document_uri = target
                .get("uri")
                .and_then(Value::as_str)
                .ok_or("Missing rename URI")?;
            if lsp::uri_to_path(document_uri).ok().as_deref() != Some(path) {
                return Err("Rename affects other files. Only current-file rename is supported; no changes applied".into());
            }
            if let Some(server_version) = target.get("version").filter(|v| !v.is_null()) {
                if server_version.as_i64() != version.map(i64::from) {
                    return Err("Rename document version changed; no changes applied".into());
                }
            }
            collect(
                target
                    .get("uri")
                    .and_then(Value::as_str)
                    .ok_or("Missing rename URI")?,
                document.get("edits").ok_or("Missing rename edits")?,
            )?;
        }
    }
    if value.get("changes").is_none() && value.get("documentChanges").is_none() {
        return Err("Rename response contains no supported changes".into());
    }
    Ok(edits)
}

fn prepare_edits(
    text: &str,
    edits: &[Value],
    selection: TextSelection,
) -> Result<(kureha_code::editing::EditPlan, Vec<EditDelta>), String> {
    if edits.len() > MAX_LANGUAGE_EDITS {
        return Err("Language edit exceeds 4096 changes".into());
    }
    let mut parts = Vec::new();
    let mut inserted_bytes = 0usize;
    for edit in edits {
        let replaced_range = range(text, edit.get("range").ok_or("Missing edit range")?)
            .ok_or("Invalid edit range")?;
        let inserted = edit
            .get("newText")
            .and_then(Value::as_str)
            .ok_or("Missing edit text")?;
        inserted_bytes = inserted_bytes.saturating_add(inserted.len());
        if inserted_bytes > MAX_LANGUAGE_BYTES {
            return Err("Language edit exceeds 16 MiB".into());
        }
        parts.push(EditDelta {
            replaced_range,
            inserted: inserted.replace("\r\n", "\n").replace('\r', "\n"),
        });
    }
    parts.sort_by_key(|edit| (edit.replaced_range.start, edit.replaced_range.end));
    if parts.windows(2).any(|pair| {
        pair[0].replaced_range.end > pair[1].replaced_range.start
            || pair[0].replaced_range.start == pair[1].replaced_range.start
    }) {
        return Err("Overlapping language edits; no changes applied".into());
    }
    let output_size = parts.iter().fold(text.len(), |size, edit| {
        size - edit.replaced_range.len() + edit.inserted.len()
    });
    if output_size > MAX_LANGUAGE_BYTES {
        return Err("Language edit exceeds the 16 MiB document limit".into());
    }
    let start = parts.first().map_or(0, |edit| edit.replaced_range.start);
    let end = parts.last().map_or(0, |edit| edit.replaced_range.end);
    let mut inserted = text[start..end].to_string();
    for edit in parts.iter().rev() {
        inserted.replace_range(
            edit.replaced_range.start - start..edit.replaced_range.end - start,
            &edit.inserted,
        );
    }
    let map = |byte: usize| {
        let mut shift = 0isize;
        for edit in &parts {
            if byte < edit.replaced_range.start {
                break;
            }
            if byte <= edit.replaced_range.end {
                let mut within = if byte == edit.replaced_range.end {
                    edit.inserted.len()
                } else {
                    (byte - edit.replaced_range.start).min(edit.inserted.len())
                };
                while !edit.inserted.is_char_boundary(within) {
                    within -= 1;
                }
                return (edit.replaced_range.start as isize + shift) as usize + within;
            }
            shift += edit.inserted.len() as isize - edit.replaced_range.len() as isize;
        }
        (byte as isize + shift) as usize
    };
    let selection = TextSelection {
        anchor: TextPosition::new(map(selection.anchor.byte)),
        caret: TextPosition::new(map(selection.caret.byte)),
    };
    Ok((
        kureha_code::editing::EditPlan {
            parts: parts.clone(),
            edit: EditDelta {
                replaced_range: start..end,
                inserted,
            },
            selection,
        },
        parts,
    ))
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
                    caret: if self.focused_group.get() == 1 {
                        rt.second_bindings
                            .get(&doc.id)
                            .unwrap_or(&doc.bindings)
                            .selection
                            .get()
                            .caret
                            .byte
                    } else {
                        doc.bindings.selection.get().caret.byte
                    },
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
                            let params = json!({"processId":std::process::id(),"clientInfo":{"name":"Kureha Code","version":env!("CARGO_PKG_VERSION")},"rootUri":root_uri,"initializationOptions":if cfg!(test) {json!({"checkOnSave":false,"cargo":{"buildScripts":{"enable":false}},"procMacro":{"enable":false}})} else {Value::Null},"workspaceFolders":[{"uri":root_uri,"name":session.root.file_name().unwrap_or_default().to_string_lossy()}],"capabilities":{"general":{"positionEncodings":["utf-16"]},"textDocument":{"diagnostic":{"dynamicRegistration":false,"relatedDocumentSupport":false},"synchronization":{"didSave":true},"completion":{"completionItem":{"snippetSupport":false,"insertReplaceSupport":false}},"publishDiagnostics":{"versionSupport":true},"hover":{"contentFormat":["plaintext"]},"rename":{"prepareSupport":true,"honorsChangeAnnotations":false}},"workspace":{"configuration":true,"workspaceFolders":true}}});
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
                                    document_version: None,
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
                if matches!(rt.language.panel, Panel::Rename(..))
                    && matches!(
                        request.method.as_str(),
                        "textDocument/completion" | "textDocument/hover"
                    )
                {
                    return false;
                }
                let guard = request.guard;
                let valid = snapshots.iter().any(|doc| {
                    doc.id == guard.id
                        && doc.revision == guard.revision
                        && (matches!(
                            request.method.as_str(),
                            "textDocument/diagnostic" | "textDocument/formatting"
                        ) || doc.caret == guard.caret)
                }) && (request.method == "textDocument/diagnostic"
                    || self.navigation.load(Ordering::Relaxed) == guard.navigation);
                if !valid
                    || request
                        .hover_generation
                        .is_some_and(|generation| generation != rt.language.hover_generation)
                {
                    if matches!(
                        request.method.as_str(),
                        "textDocument/formatting" | "textDocument/rename"
                    ) {
                        drop(rt);
                        self.status
                            .set("Language edit discarded because the document changed".into());
                    }
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
                                            document_version: request.document_version,
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
                    "textDocument/formatting" | "textDocument/rename" => {
                        let is_rename = request.method == "textDocument/rename";
                        let edits = if is_rename {
                            let Some(doc) = snapshots.iter().find(|doc| doc.id == guard.id) else {
                                return false;
                            };
                            rename_edits(&result, &doc.path, request.document_version)
                        } else if result.is_null() {
                            Ok(vec![])
                        } else {
                            result
                                .as_array()
                                .cloned()
                                .ok_or_else(|| "Invalid formatting response".into())
                        };
                        drop(rt);
                        match edits {
                            Ok(edits) => self.apply_language_edits(
                                &guard,
                                &edits,
                                if is_rename { "Rename" } else { "Format" },
                            ),
                            Err(error) => {
                                self.status.set(error);
                                self.dismiss_language_panel();
                            }
                        }
                        return true;
                    }
                    "textDocument/prepareRename" => {
                        let Some(doc) = snapshots.iter().find(|doc| doc.id == guard.id) else {
                            return false;
                        };
                        let prepared = result.get("range").unwrap_or(&result);
                        let text = range(&doc.text, prepared)
                            .and_then(|r| doc.text.get(r))
                            .or_else(|| {
                                result
                                    .get("defaultBehavior")
                                    .and_then(Value::as_bool)
                                    .filter(|yes| *yes)
                                    .and_then(|_| symbol_at(&doc.text, doc.caret))
                            });
                        let Some(text) = text else {
                            drop(rt);
                            self.status.set("This symbol cannot be renamed".into());
                            return true;
                        };
                        let name = result
                            .get("placeholder")
                            .and_then(Value::as_str)
                            .unwrap_or(text);
                        rt.language.panel =
                            Panel::Rename(guard, State::new(generate_state_id(), name.to_string()));
                    }
                    "textDocument/references" => match locations(&result) {
                        Ok((items, truncated)) => {
                            rt.language.selected_completion = 0;
                            rt.language.panel = Panel::References(items, truncated);
                        }
                        Err(error) => {
                            drop(rt);
                            self.status.set(error);
                            return true;
                        }
                    },
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
        self.language_send(method, byte, hover_generation, quiet, None);
    }
    fn language_send(
        &self,
        method: &str,
        byte: Option<usize>,
        hover_generation: Option<u64>,
        quiet: bool,
        rename_name: Option<&str>,
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
            "textDocument/references" => "referencesProvider",
            "textDocument/formatting" => "documentFormattingProvider",
            "textDocument/rename" | "textDocument/prepareRename" => "renameProvider",
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
        let mut params = json!({"textDocument":{"uri":uri(&doc.path)},"position":position(&doc.text,byte.unwrap_or(doc.caret).min(doc.text.len()))});
        match method {
            "textDocument/completion" => params["context"] = json!({"triggerKind":1}),
            "textDocument/references" => params["context"] = json!({"includeDeclaration":true}),
            "textDocument/formatting" => {
                params.as_object_mut().unwrap().remove("position");
                params["options"] = json!({"tabSize":4,"insertSpaces":true});
            }
            "textDocument/rename" => params["newName"] = json!(rename_name.unwrap_or_default()),
            "textDocument/prepareRename"
                if !session
                    .capabilities
                    .pointer("/renameProvider/prepareProvider")
                    .and_then(Value::as_bool)
                    .unwrap_or(false) =>
            {
                let name = symbol_at(&doc.text, doc.caret)
                    .unwrap_or_default()
                    .to_string();
                let guard = Guard {
                    id,
                    revision: doc.revision,
                    caret: doc.caret,
                    navigation,
                };
                rt.language.panel = Panel::Rename(guard, State::new(generate_state_id(), name));
                drop(rt);
                self.changed();
                return;
            }
            _ => {}
        }
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
                        document_version: session.documents.get(&doc.path).map(|doc| doc.version),
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
    pub(super) fn format_document(&self) {
        self.language_request("textDocument/formatting");
    }
    pub(super) fn find_references(&self) {
        self.language_request("textDocument/references");
    }
    pub(super) fn show_rename(&self) {
        self.dismiss_language_panel();
        self.runtime.lock().unwrap().language.completion_due = None;
        self.language_request("textDocument/prepareRename");
    }
    pub(super) fn language_rename_open(&self) -> bool {
        matches!(
            self.runtime.lock().unwrap().language.panel,
            Panel::Rename(..)
        )
    }
    fn submit_rename(&self) {
        let (guard, name) = {
            let rt = self.runtime.lock().unwrap();
            let Panel::Rename(guard, name) = &rt.language.panel else {
                return;
            };
            (guard.clone(), name.get())
        };
        if !valid_symbol_name(&name) {
            self.status.set("Use an identifier up to 256 bytes, with letters, digits, _ or $; the first character cannot be a digit".into());
            return;
        }
        let id = self.active_id();
        let caret = self
            .bindings(id)
            .map(|bindings| bindings.selection.get().caret.byte);
        let valid = id == guard.id
            && caret == Some(guard.caret)
            && self.navigation.load(Ordering::Relaxed) == guard.navigation
            && self
                .runtime
                .lock()
                .unwrap()
                .workspace
                .get(id)
                .is_some_and(|doc| doc.editor.revision() == guard.revision);
        if !valid {
            self.dismiss_language_panel();
            self.status
                .set("Rename cancelled because the document changed".into());
            return;
        }
        self.dismiss_language_panel();
        self.language_send("textDocument/rename", None, None, false, Some(&name));
    }
    fn apply_language_edits(&self, guard: &Guard, edits: &[Value], label: &str) {
        let id = self.active_id();
        if id != guard.id
            || !self.editable(id)
            || self.navigation.load(Ordering::Relaxed) != guard.navigation
        {
            self.status.set(format!(
                "{label} discarded because the active editor changed"
            ));
            return;
        }
        let Some(bindings) = self.bindings(id) else {
            return;
        };
        let selection = bindings.selection.get();
        let mut rt = self.runtime.lock().unwrap();
        let Some(doc) = rt.workspace.get_mut(id) else {
            return;
        };
        if doc.editor.revision() != guard.revision {
            drop(rt);
            self.status
                .set(format!("{label} discarded because the document changed"));
            return;
        }
        let original = doc.editor.document.as_str();
        match prepare_edits(&original, edits, selection) {
            Ok((plan, _parts)) => {
                if original[plan.edit.replaced_range.clone()] == plan.edit.inserted {
                    drop(rt);
                    self.status.set(format!("{label}: no changes needed"));
                    return;
                }
                doc.editor.moved(selection);
                doc.editor.edit_plan(&plan);
                rt.language.panel = Panel::Closed;
                rt.language.completion_due = None;
                drop(rt);
                self.sync_editor(id);
                self.changed();
                self.status
                    .set(format!("{label} applied · Undo restores the previous text"));
            }
            Err(error) => {
                drop(rt);
                self.status.set(error);
            }
        }
    }
    pub(super) fn schedule_completion(&self, id: DocumentId, delta: &EditDelta) {
        let caret = self
            .bindings(id)
            .map(|b| b.selection.get().caret.byte)
            .unwrap_or(0);
        let mut rt = self.runtime.lock().unwrap();
        let next = rt
            .workspace
            .get(id)
            .map(|doc| (doc.editor.revision(), caret));
        rt.language.hover_generation = rt.language.hover_generation.wrapping_add(1);
        rt.language.hover_target = None;
        if !matches!(rt.language.panel, Panel::Problems | Panel::References(..)) {
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
        let active_caret = self
            .bindings(active)
            .map(|b| b.selection.get().caret.byte)
            .unwrap_or(0);
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
                    && rt
                        .workspace
                        .get(*id)
                        .is_some_and(|doc| doc.editor.revision() == *rev && active_caret == *caret)
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
        if completion.is_some() && !self.language_rename_open() {
            let rect = self.caret_rect.get();
            self.language_anchor
                .set(Point::new(rect.origin.x, rect.origin.y + rect.size.height));
            self.language_request_at("textDocument/completion", None, None, true);
        }
        if let Some((byte, point, generation)) = hover {
            if !matches!(
                self.runtime.lock().unwrap().language.panel,
                Panel::Completion(..) | Panel::Rename(..)
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
        if matches!(rt.language.panel, Panel::Rename(..)) {
            drop(rt);
            if key == KeyCode::Enter {
                self.submit_rename();
                return true;
            }
            if key == KeyCode::Escape {
                self.dismiss_language_panel();
                return true;
            }
            return false;
        }
        if key == KeyCode::F(2) {
            drop(rt);
            self.show_rename();
            return true;
        }
        if key == KeyCode::F(12) && modifiers.shift {
            drop(rt);
            self.find_references();
            return true;
        }
        if modifiers.alt && modifiers.shift && matches!(key, KeyCode::Char('f' | 'F')) {
            drop(rt);
            self.format_document();
            return true;
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
        let mut open = !matches!(rt.language.panel, Panel::Closed);
        for session in rt.language.sessions.values_mut() {
            let cancelled: Vec<_> = session
                .pending
                .iter()
                .filter(|(_, request)| request.method == "textDocument/prepareRename")
                .map(|(id, _)| *id)
                .collect();
            open |= !cancelled.is_empty();
            for id in cancelled {
                session.pending.remove(&id);
                if let Some(client) = &session.client {
                    let _ = client.notify("$/cancelRequest", json!({"id":id}));
                }
            }
        }
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
    pub(super) fn language_navigate(&self, path: PathBuf, target: Value) {
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
            self.open(path.clone());
            // Starting the file worker invalidates navigation. Record its new
            // generation, otherwise every unopened-file target is discarded.
            let navigation = self.navigation.load(Ordering::Relaxed);
            let mut rt = self.runtime.lock().unwrap();
            if matches!(
                rt.job.as_ref().map(|job| &job.operation),
                Some(FileOperation::Open { .. })
            ) {
                rt.language.navigation = Some((path, target, navigation));
            }
        }
    }
    fn language_select(&self, id: DocumentId, target: &Value) {
        let Some(active_bindings) = self.bindings(id) else {
            return;
        };
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
            (active_bindings, selection)
        };
        bindings.0.selection.set(bindings.1);
        if !matches!(
            self.runtime.lock().unwrap().language.panel,
            Panel::References(..)
        ) {
            self.dismiss_language_panel();
        }
        self.status.set("Location opened".into());
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
        let caret = self
            .bindings(id)
            .map(|b| b.selection.get().caret.byte)
            .unwrap_or(0);
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
            || guard.caret != caret
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
        let end = (primary.0.start as isize + shift).max(0) as usize + primary.1.len();
        let parts: Vec<_> = edits
            .into_iter()
            .map(|(replaced_range, inserted)| EditDelta {
                replaced_range,
                inserted,
            })
            .collect();
        doc.editor.edit_parts(&parts, TextSelection::collapsed(end));
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
        let rename = match &panel {
            Panel::Rename(_, name) => Some(name.clone()),
            _ => None,
        };
        let rename_input = rename
            .clone()
            .unwrap_or_else(|| State::new(generate_state_id(), String::new()));
        let submit = self.clone();
        let cancel = self.clone();
        let rename_view = if_view!(rename.is_some(), super::popup::Anchored::new(
            Surface::floating(vstack! {
                TextField::new(rename_input).font_size(13.0).autofocus(true)
                    .text_color(colors.foreground).background_color(colors.elevated)
                    .border_color(colors.border).focused_border_color(colors.accent)
                    .on_submit(move || submit.submit_rename())
                    .on_cancel(move || { cancel.dismiss_language_panel(); }).frame(320.0, 30.0),
                Text::new("Current file only · Enter apply · Esc cancel").font_size(11.0).color(colors.muted),
            }.spacing(5.0).padding(8.0)).fill(colors.elevated).border_color(colors.border).corner_radius(4.0), self.language_anchor.clone()), VStack::new(()));
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
        let information = if_view!(
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
        );
        ZStack::new((information, rename_view)).alignment(Alignment::TopLeading)
    }
    pub(super) fn language_panel(&self) -> impl View + Clone + use<> {
        let (panel, problems, root) = {
            let rt = self.runtime.lock().unwrap();
            (
                rt.language.panel.clone(),
                rt.language.problems.clone(),
                rt.explorer.root.clone(),
            )
        };
        let colors = self.theme.get().colors();
        let is_open = matches!(panel, Panel::Problems | Panel::References(..));
        let close = self.clone();
        let (title, entries): (String, Vec<(String, Option<Location>)>) = match panel {
            Panel::References(locations, truncated) => {
                let title = format!("References ({}){}", locations.len(), if truncated {" · first 1000 shown"} else {""});
                let entries = locations.into_iter().map(|location| {
                    let display = root.as_deref().and_then(|root| location.path.strip_prefix(root).ok()).unwrap_or(&location.path);
                    let line = location.range.pointer("/start/line").and_then(Value::as_u64).unwrap_or(0).saturating_add(1);
                    let column = location.range.pointer("/start/character").and_then(Value::as_u64).unwrap_or(0).saturating_add(1);
                    (format!("{}:{line}:{column}", display.display()), Some(location))
                }).collect();
                (title, entries)
            }
            Panel::Problems => (format!("Problems ({})", problems.len()), problems.iter().take(100).map(|problem| {
                let label = format!("{} · {} · {}", problem.path.file_name().unwrap_or_default().to_string_lossy(), if problem.severity == 1 {"Error"} else {"Warning"}, problem.message);
                let target = {
                    let rt = self.runtime.lock().unwrap();
                    rt.workspace.find_path(&problem.path).and_then(|id| rt.workspace.get(id)).map(|doc| {
                        let text = doc.editor.document.as_str();
                        Location { path:problem.path.clone(), range:json!({"start":position(&text, problem.range.start),"end":position(&text, problem.range.end)}) }
                    })
                };
                (label, target)
            }).collect()),
            _ => (String::new(), vec![]),
        };
        let entries = if entries.is_empty() {
            vec![("No results".into(), None)]
        } else {
            entries
        };
        let rows = entries
            .into_iter()
            .map(|(label, target)| {
                let app = self.clone();
                Button::new(label)
                    .header_style()
                    .font_size(12.0)
                    .text_color(colors.foreground)
                    .background_color(colors.surface)
                    .padding(5.0)
                    .on_click(move || {
                        if let Some(location) = &target {
                            app.language_navigate(location.path.clone(), location.range.clone());
                        }
                    })
                    .frame(f32::INFINITY, 26.0)
            })
            .collect::<Vec<_>>();
        if_view!(is_open, vstack! {
            hstack! { Text::new(title).font_size(12.0).color(colors.foreground), Spacer::new(), Button::new("×").header_style().text_color(colors.foreground).on_click(move || { close.dismiss_language_panel(); }), }.spacing(8.0),
            ScrollView::new(VStack::new(super::ui::ViewCollection(rows)).spacing(0.0)).frame(f32::INFINITY, 140.0),
        }.spacing(4.0).padding(8.0).background(colors.surface), VStack::new(()))
    }
}

#[cfg(test)]
#[path = "language_tests.rs"]
mod tests;
