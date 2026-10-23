use kureha_code::{
    explorer::{self, Explorer, Listing, Row},
    files, search,
    workspace::{Bindings, DocumentId, Workspace},
};
use scarlet_ui::{
    Event, KeyCode, MenuBarModel, MenuEntry, MenuItemModel, PlatformWindow, WindowContext,
    WindowId,
    clipboard::{ClipboardHandle, TextClipboard},
    file_dialog::{FileDialog, FileDialogHandle, FileDialogMode, FileDialogOutcome},
    generate_state_id, hstack, if_view,
    prelude::*,
    scenes, vstack,
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
};
mod language;
mod popup;
mod ui;

#[derive(Clone, Copy)]
enum DialogPurpose {
    Open,
    OpenFolder,
    Save(DocumentId),
}
#[derive(Clone)]
enum FileOperation {
    Open,
    Save { id: DocumentId, revision: u64 },
}
enum Completion {
    Open(PathBuf, String),
    Save(PathBuf),
}
type JobResult = std::result::Result<Completion, String>;
struct FileJob {
    operation: FileOperation,
    receiver: mpsc::Receiver<JobResult>,
}
struct TreeJob {
    generation: u64,
    root: bool,
    path: PathBuf,
    receiver: mpsc::Receiver<std::result::Result<(PathBuf, Listing), String>>,
}
#[derive(Default)]
struct CloseRequest {
    tab: Option<DocumentId>,
    target: Option<DocumentId>,
    discarded: BTreeSet<DocumentId>,
}
#[derive(Clone, Copy)]
struct ClipboardTarget {
    id: DocumentId,
    generation: u64,
    navigation: u64,
    selection: TextSelection,
}
enum ClipboardJob {
    TerminalPaste {
        handle: ClipboardHandle<Option<String>>,
        generation: u64,
    },
    Copy {
        handle: ClipboardHandle<()>,
        cut: bool,
        target: ClipboardTarget,
    },
    Paste {
        handle: ClipboardHandle<Option<String>>,
        target: ClipboardTarget,
    },
}
#[derive(Default)]
struct Runtime {
    workspace: Workspace,
    explorer: Explorer,
    owner: Option<WindowId>,
    dialog: Option<(DialogPurpose, FileDialogHandle)>,
    job: Option<FileJob>,
    tree_jobs: Vec<TreeJob>,
    close: Option<CloseRequest>,
    title: String,
    clipboard: Option<TextClipboard>,
    clipboard_error: Option<String>,
    clipboard_job: Option<ClipboardJob>,
    menu_actions: Vec<fn(&App)>,
    language: language::Service,
    minimap: kureha_code::minimap::MinimapCache,
}
#[derive(Clone)]
struct App {
    editor_font: Option<FontStack>,
    theme: State<kureha_code::theme::Theme>,
    minimap_visible: State<bool>,
    terminal_visible: State<bool>,
    terminal: kureha_code::terminal::Terminal,
    active: State<usize>,
    chrome: State<u64>,
    status: State<String>,
    position: State<String>,
    language_status: State<String>,
    language_anchor: State<Point>,
    caret_rect: State<Rect>,
    tree_rows: State<Vec<Row>>,
    tree_selected: State<Option<usize>>,
    confirmation: State<String>,
    find_open: State<bool>,
    query: State<String>,
    replacement: State<String>,
    search_status: State<String>,
    runtime: Arc<Mutex<Runtime>>,
    navigation: Arc<AtomicU64>,
}
impl App {
    fn new() -> Self {
        let active = State::new(generate_state_id(), 0);
        let navigation = Arc::new(AtomicU64::new(0));
        let changed = navigation.clone();
        active.subscribe_any(Arc::new(move || {
            changed.fetch_add(1, Ordering::Relaxed);
        }));
        Self {
            editor_font: kureha_code::fonts::editor_stack(),
            theme: State::new(generate_state_id(), kureha_code::theme::Theme::Light),
            minimap_visible: State::new(generate_state_id(), true),
            terminal_visible: State::new(generate_state_id(), false),
            terminal: kureha_code::terminal::Terminal::new(),
            language_status: State::new(generate_state_id(), "Plain text".into()),
            language_anchor: State::new(generate_state_id(), Point::new(60.0, 30.0)),
            caret_rect: State::new(generate_state_id(), Rect::from_xywh(60.0, 10.0, 1.0, 20.0)),
            active,
            navigation,
            chrome: State::new(generate_state_id(), 0),
            status: State::new(generate_state_id(), "Ready".into()),
            position: State::new(generate_state_id(), "Ln 1, Col 1 · UTF-8 · LF".into()),
            tree_rows: State::initial(generate_state_id()),
            tree_selected: State::initial(generate_state_id()),
            confirmation: State::new(generate_state_id(), String::new()),
            find_open: State::new(generate_state_id(), false),
            query: State::new(generate_state_id(), String::new()),
            replacement: State::new(generate_state_id(), String::new()),
            search_status: State::new(generate_state_id(), "Literal search · Match case".into()),
            runtime: Arc::new(Mutex::new(Runtime::default())),
        }
    }
    fn set_theme(&self, theme: kureha_code::theme::Theme) {
        self.theme.set(theme);
        kureha_code::syntax::set_theme(theme);
        self.runtime.lock().unwrap().minimap.clear();
        self.changed();
    }
    fn toggle_minimap(&self) {
        self.minimap_visible.update(|shown| *shown = !*shown);
    }
    fn show_terminal(&self) {
        if !self.terminal.is_running() {
            let id = self.active_id();
            let root = {
                let rt = self.runtime.lock().unwrap();
                rt.explorer.root.clone().or_else(|| {
                    rt.workspace.get(id).and_then(|doc| {
                        doc.path
                            .as_ref()
                            .and_then(|p| p.parent().map(Path::to_path_buf))
                    })
                })
            }
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
            self.terminal.open(root);
        }
        self.terminal_visible.set(true);
        self.terminal.focus();
        self.dismiss_language_panel();
    }
    fn toggle_terminal(&self) {
        if self.terminal_visible.get() {
            self.terminal_visible.set(false);
            self.terminal.blur();
        } else {
            self.show_terminal();
        }
    }
    fn changed(&self) {
        self.chrome.update(|value| *value = value.wrapping_add(1));
    }
    fn invalidate_clipboard(&self) {
        self.navigation.fetch_add(1, Ordering::Relaxed);
    }
    fn active_id(&self) -> DocumentId {
        let rt = self.runtime.lock().unwrap();
        rt.workspace.documents[self.active.get().min(rt.workspace.documents.len() - 1)].id
    }
    fn window_title(&self) -> String {
        let id = self.active_id();
        let rt = self.runtime.lock().unwrap();
        let doc = rt.workspace.get(id).unwrap();
        format!(
            "{}{} — {}",
            if doc.editor.dirty() { "● " } else { "" },
            doc.name(),
            env!("APP_NAME")
        )
    }
    fn bindings(&self, id: DocumentId) -> Option<Bindings> {
        self.runtime
            .lock()
            .unwrap()
            .workspace
            .get(id)
            .map(|doc| doc.bindings.clone())
    }
    fn blocked(&self) -> bool {
        let rt = self.runtime.lock().unwrap();
        rt.dialog.is_some() || rt.close.is_some()
    }
    fn editable(&self, id: DocumentId) -> bool {
        let rt = self.runtime.lock().unwrap();
        rt.dialog.is_none()
            && rt.close.is_none()
            && rt.workspace.get(id).is_some()
            && !matches!(rt.job.as_ref().map(|job| &job.operation), Some(FileOperation::Save { id: saving, .. }) if *saving == id)
    }
    fn sync_editor(&self, id: DocumentId) {
        let snapshot = {
            let rt = self.runtime.lock().unwrap();
            rt.workspace.get(id).map(|doc| {
                (
                    doc.bindings.clone(),
                    doc.editor.document.clone(),
                    doc.editor.selection,
                )
            })
        };
        if let Some((bindings, document, selection)) = snapshot {
            bindings.document.set(document);
            bindings.selection.set(selection);
        }
    }
    fn edit(&self, id: DocumentId, delta: &EditDelta) {
        if !self.editable(id) {
            self.sync_editor(id);
            return;
        }
        let changed = {
            let mut rt = self.runtime.lock().unwrap();
            let Some(doc) = rt.workspace.get_mut(id) else {
                return;
            };
            let dirty = doc.editor.dirty();
            doc.editor.edit(delta);
            dirty != doc.editor.dirty()
        };
        self.sync_editor(id);
        if changed {
            self.changed();
        }
    }
    fn undo(&self, redo: bool) {
        let id = self.active_id();
        if !self.editable(id) {
            return;
        }
        {
            let mut rt = self.runtime.lock().unwrap();
            let doc = rt.workspace.get_mut(id).unwrap();
            doc.editor.moved(doc.bindings.selection.get());
            doc.editor.undo(redo);
        }
        self.sync_editor(id);
        self.changed();
    }
    fn select_all(&self) {
        let id = self.active_id();
        if !self.editable(id) {
            return;
        }
        let (bindings, selection) = {
            let mut rt = self.runtime.lock().unwrap();
            let doc = rt.workspace.get_mut(id).unwrap();
            let selection = TextSelection {
                anchor: TextPosition::new(0),
                caret: TextPosition::new(doc.editor.document.len()),
            };
            doc.editor.moved(selection);
            (doc.bindings.clone(), selection)
        };
        bindings.selection.set(selection);
    }
    fn activate(&self, id: DocumentId) {
        if self.blocked() {
            return;
        }
        self.activate_unchecked(id);
    }
    fn activate_unchecked(&self, id: DocumentId) {
        let index = self.runtime.lock().unwrap().workspace.index(id);
        if let Some(index) = index {
            self.active.set(index);
        }
    }
    fn next_tab(&self, backwards: bool) {
        if self.blocked() {
            return;
        }
        let count = self.runtime.lock().unwrap().workspace.documents.len();
        self.active
            .set((self.active.get() + if backwards { count - 1 } else { 1 }) % count);
    }
    fn new_document(&self) {
        if self.blocked() {
            return;
        }
        let id = self.runtime.lock().unwrap().workspace.new_document();
        self.activate_unchecked(id);
        self.changed();
    }
    fn remove_document(&self, id: DocumentId) {
        let active = self.active_id();
        let index = {
            let mut rt = self.runtime.lock().unwrap();
            let old_index = rt.workspace.index(id).unwrap_or(0);
            rt.workspace.remove(id);
            rt.workspace
                .index(active)
                .unwrap_or(old_index.min(rt.workspace.documents.len() - 1))
        };
        self.active.set(index);
        self.changed();
    }
    fn request_close(&self, tab: Option<DocumentId>) {
        {
            let mut rt = self.runtime.lock().unwrap();
            if rt.close.is_some() {
                return;
            }
            if rt.job.is_some() || rt.dialog.is_some() {
                drop(rt);
                self.status
                    .set("Finish the file operation before closing".into());
                return;
            }
            rt.close = Some(CloseRequest {
                tab,
                ..CloseRequest::default()
            });
        }
        self.invalidate_clipboard();
        self.advance_close();
    }
    fn advance_close(&self) {
        let decision = {
            let mut rt = self.runtime.lock().unwrap();
            let Some(close) = &rt.close else {
                return;
            };
            let target = rt
                .workspace
                .documents
                .iter()
                .find(|doc| {
                    close.tab.is_none_or(|id| id == doc.id)
                        && doc.editor.dirty()
                        && !close.discarded.contains(&doc.id)
                })
                .map(|doc| (doc.id, doc.name()));
            if let Some((id, _)) = &target {
                rt.close.as_mut().unwrap().target = Some(*id);
            }
            target
        };
        if let Some((id, name)) = decision {
            self.activate_unchecked(id);
            self.confirmation.set(format!("Save changes to {name}?"));
            open_window("confirm");
        } else {
            let close = self.runtime.lock().unwrap().close.take().unwrap();
            dismiss_window("confirm");
            if let Some(id) = close.tab {
                self.remove_document(id);
            } else {
                dismiss_window("main");
            }
        }
    }
    fn cancel_close(&self) {
        self.runtime.lock().unwrap().close = None;
        dismiss_window("confirm");
    }
    fn discard_close(&self) {
        let mut rt = self.runtime.lock().unwrap();
        if rt.job.is_some() || rt.dialog.is_some() {
            return;
        }
        if let Some(close) = rt.close.as_mut() {
            if let Some(id) = close.target {
                close.discarded.insert(id);
            }
        }
        drop(rt);
        self.advance_close();
    }
    fn picker(&self, purpose: DialogPurpose) {
        let (owner, path, name, root) = {
            let rt = self.runtime.lock().unwrap();
            if rt.job.is_some() || rt.dialog.is_some() {
                return;
            }
            if matches!(purpose, DialogPurpose::Open | DialogPurpose::OpenFolder)
                && rt.close.is_some()
            {
                return;
            }
            let Some(owner) = rt.owner else {
                return;
            };
            let doc = match purpose {
                DialogPurpose::Save(id) => rt.workspace.get(id),
                _ => None,
            };
            (
                owner,
                doc.and_then(|d| d.path.clone()),
                doc.map(|d| d.name()),
                rt.explorer.root.clone(),
            )
        };
        self.invalidate_clipboard();
        let mut dialog = FileDialog::new(match purpose {
            DialogPurpose::Open => FileDialogMode::Open,
            DialogPurpose::OpenFolder => FileDialogMode::OpenFolder,
            DialogPurpose::Save(_) => FileDialogMode::Save,
        });
        dialog.title = match purpose {
            DialogPurpose::Open => "Open File",
            DialogPurpose::OpenFolder => "Open Folder",
            DialogPurpose::Save(_) => "Save File",
        }
        .into();
        dialog.initial_directory = path
            .as_ref()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .or(root)
            .filter(|p| p.is_absolute())
            .map(Into::into);
        if matches!(purpose, DialogPurpose::Save(_)) {
            dialog.default_name = Some(name.unwrap_or_else(|| "Untitled.txt".into()));
        }
        let handle = dialog.show(owner);
        self.runtime.lock().unwrap().dialog = Some((purpose, handle));
    }
    fn worker(&self, operation: FileOperation, job: impl FnOnce() -> JobResult + Send + 'static) {
        self.invalidate_clipboard();
        let (sender, receiver) = mpsc::channel();
        match thread::Builder::new()
            .name("kureha-file".into())
            .stack_size(2 * 1024 * 1024)
            .spawn(move || {
                let _ = sender.send(job());
            }) {
            Ok(_) => {
                self.runtime.lock().unwrap().job = Some(FileJob {
                    operation,
                    receiver,
                })
            }
            Err(error) => self
                .status
                .set(format!("Could not start file operation: {error}")),
        }
        self.changed();
    }
    fn open(&self, path: PathBuf) {
        if self.blocked() || self.runtime.lock().unwrap().job.is_some() {
            return;
        }
        self.status.set("Opening…".into());
        self.worker(FileOperation::Open, move || {
            let path = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
            files::read(&path)
                .map(|text| Completion::Open(path, text))
                .map_err(|e| e.to_string())
        });
    }
    fn save(&self) {
        let id = {
            let rt = self.runtime.lock().unwrap();
            rt.close.as_ref().and_then(|close| close.target)
        }
        .unwrap_or_else(|| self.active_id());
        let path = self
            .runtime
            .lock()
            .unwrap()
            .workspace
            .get(id)
            .and_then(|doc| doc.path.clone());
        if let Some(path) = path {
            self.save_to(id, path);
        } else {
            self.picker(DialogPurpose::Save(id));
        }
    }
    fn save_to(&self, id: DocumentId, path: PathBuf) {
        let (document, revision, crlf) = {
            let rt = self.runtime.lock().unwrap();
            if rt.job.is_some() || rt.dialog.is_some() {
                return;
            }
            let Some(doc) = rt.workspace.get(id) else {
                return;
            };
            (doc.editor.document.clone(), doc.editor.revision(), doc.crlf)
        };
        self.status.set("Saving…".into());
        let runtime = self.runtime.clone();
        self.worker(FileOperation::Save { id, revision }, move || {
            let path = resolve_save_path(&path).map_err(|e| e.to_string())?;
            {
                let rt = runtime.lock().unwrap();
                if rt
                    .workspace
                    .find_path(&path)
                    .is_some_and(|other| other != id)
                {
                    return Err("That file is open in another tab; save to a different path".into());
                }
            }
            files::save(&path, &document.as_str(), crlf)
                .map(|_| Completion::Save(path))
                .map_err(|e| e.to_string())
        });
    }
    fn show_folder(&self) {
        self.picker(DialogPurpose::OpenFolder);
    }
    fn open_folder(&self, path: PathBuf) {
        if self.blocked() {
            return;
        }
        let generation = {
            let mut rt = self.runtime.lock().unwrap();
            if rt.tree_jobs.len() >= 4 {
                drop(rt);
                self.status
                    .set("Folder reads are pending; try again shortly".into());
                return;
            }
            rt.explorer.begin_root_request()
        };
        self.refresh_tree();
        self.status.set("Opening folder…".into());
        self.start_tree_job(path, generation, true);
    }

    fn start_tree_job(&self, path: PathBuf, generation: u64, root: bool) {
        let (sender, receiver) = mpsc::channel();
        let requested = path.clone();
        let started = thread::Builder::new()
            .name("kureha-explorer".into())
            .spawn(move || {
                let result = (|| {
                    let path = if root {
                        std::fs::canonicalize(requested)?
                    } else {
                        requested
                    };
                    explorer::scan(&path).map(|listing| (path, listing))
                })()
                .map_err(|error: std::io::Error| error.to_string());
                let _ = sender.send(result);
            });
        match started {
            Ok(_) => self.runtime.lock().unwrap().tree_jobs.push(TreeJob {
                path,
                generation,
                root,
                receiver,
            }),
            Err(error) => {
                self.runtime.lock().unwrap().explorer.complete(
                    generation,
                    &path,
                    Err(error.to_string()),
                );
                self.status.set(format!("Could not read folder: {error}"));
            }
        }
        self.refresh_tree();
    }
    fn open_tree_row(&self, row: Row, index: usize) {
        if self.blocked() {
            return;
        }
        let Some(path) = row.path else {
            return;
        };
        self.tree_selected.set(Some(index));
        if !row.directory {
            self.open(path);
            return;
        }
        let request = {
            let mut rt = self.runtime.lock().unwrap();
            if rt.tree_jobs.len() >= 4 {
                drop(rt);
                self.status
                    .set("Folder reads are pending; try again shortly".into());
                return;
            }
            let generation = rt.explorer.generation;
            rt.explorer
                .toggle(&path, row.depth)
                .map(|load| (load, generation))
        };
        if let Ok((true, generation)) = request {
            self.start_tree_job(path, generation, false);
        }
        self.refresh_tree();
    }
    fn refresh_tree(&self) {
        let rows = self.runtime.lock().unwrap().explorer.rows();
        self.tree_rows.set(rows);
        self.changed();
    }
    fn show_find(&self) {
        if self.blocked() {
            return;
        }
        if let Some(bindings) = self.bindings(self.active_id()) {
            let document = bindings.document.get();
            if let Some(range) = bindings.selection.get().normalized_range() {
                if let Some(text) = document.as_str().get(range) {
                    if !text.contains('\n') && text.len() <= 1024 {
                        self.query.set(text.into());
                    }
                }
            }
        }
        self.find_open.set(true);
    }
    fn find(&self, backwards: bool) {
        if self.blocked() {
            return;
        }
        let Some(bindings) = self.bindings(self.active_id()) else {
            return;
        };
        let selection = bindings.selection.get();
        let start = if backwards {
            selection.start().byte
        } else {
            selection.end().byte
        };
        let document = bindings.document.get();
        match search::find(&document.as_str(), &self.query.get(), start, backwards) {
            Some(range) => {
                bindings.selection.set(TextSelection {
                    anchor: TextPosition::new(range.start),
                    caret: TextPosition::new(range.end),
                });
                self.search_status.set("Match selected · Match case".into());
            }
            None => self.search_status.set(
                if self.query.get().is_empty() {
                    "Enter text to find"
                } else {
                    "No matches"
                }
                .into(),
            ),
        }
    }
    fn replace_match(&self) {
        let target = self.clipboard_target();
        let Some(bindings) = self.bindings(target.id) else {
            return;
        };
        let query = self.query.get();
        if !query.is_empty() {
            if let Some(range) = target.selection.normalized_range() {
                if bindings.document.get().as_str().get(range) == Some(query.as_str()) {
                    self.apply_clipboard(target, &self.replacement.get());
                }
            }
        }
        self.find(false);
    }
    fn replace_all(&self) {
        let id = self.active_id();
        if !self.editable(id) {
            return;
        }
        let Some(bindings) = self.bindings(id) else {
            return;
        };
        let document = bindings.document.get();
        let replacement = self
            .replacement
            .get()
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        match search::replace_all(
            &document.as_str(),
            &self.query.get(),
            &replacement,
            files::MAX_BYTES as usize,
        ) {
            Ok(Some((text, count))) => {
                {
                    let mut rt = self.runtime.lock().unwrap();
                    let doc = rt.workspace.get_mut(id).unwrap();
                    doc.editor.moved(bindings.selection.get());
                    doc.editor.edit_transaction(&EditDelta {
                        replaced_range: 0..document.len(),
                        inserted: text,
                    });
                }
                self.sync_editor(id);
                self.changed();
                self.search_status
                    .set(format!("Replaced {count} matches · Undo restores all"));
            }
            Ok(None) => self
                .search_status
                .set("No matches; document unchanged".into()),
            Err(error) => self.search_status.set(error),
        }
    }
    fn clipboard_service(&self) -> Option<TextClipboard> {
        if !self.editable(self.active_id()) {
            self.status.set("Finish the file operation first".into());
            return None;
        }
        let service = {
            let rt = self.runtime.lock().unwrap();
            if rt.clipboard_job.is_some() {
                Err("A clipboard operation is pending".into())
            } else {
                rt.clipboard.clone().ok_or_else(|| {
                    rt.clipboard_error
                        .clone()
                        .unwrap_or_else(|| "Desktop clipboard unavailable".into())
                })
            }
        };
        match service {
            Ok(service) => Some(service),
            Err(error) => {
                self.status.set(error);
                None
            }
        }
    }
    fn clipboard_target(&self) -> ClipboardTarget {
        let id = self.active_id();
        let bindings = self.bindings(id).unwrap();
        ClipboardTarget {
            id,
            generation: bindings.generation(),
            navigation: self.navigation.load(Ordering::Relaxed),
            selection: bindings.selection.get(),
        }
    }
    fn copy(&self, cut: bool) {
        let target = self.clipboard_target();
        let Some(range) = target.selection.normalized_range() else {
            return;
        };
        let Some(service) = self.clipboard_service() else {
            return;
        };
        let bindings = self.bindings(target.id).unwrap();
        let document = bindings.document.get();
        let text = document.as_str();
        let Some(selected) = text.get(range) else {
            return;
        };
        let handle = service.write_text(selected.into());
        self.runtime.lock().unwrap().clipboard_job = Some(ClipboardJob::Copy {
            handle,
            cut,
            target,
        });
        self.status.set("Copying to desktop clipboard…".into());
    }
    fn paste(&self) {
        let Some(service) = self.clipboard_service() else {
            return;
        };
        let target = self.clipboard_target();
        let handle = service.read_text();
        self.runtime.lock().unwrap().clipboard_job = Some(ClipboardJob::Paste { handle, target });
        self.status.set("Reading desktop clipboard…".into());
    }
    fn paste_terminal(&self) {
        let Some(service) = self.clipboard_service() else {
            return;
        };
        let handle = service.read_text();
        self.runtime.lock().unwrap().clipboard_job = Some(ClipboardJob::TerminalPaste {
            handle,
            generation: self.terminal.generation(),
        });
    }
    fn apply_clipboard(&self, target: ClipboardTarget, text: &str) -> bool {
        if self.active_id() != target.id
            || !self.editable(target.id)
            || self.navigation.load(Ordering::Relaxed) != target.navigation
        {
            return false;
        }
        let mut rt = self.runtime.lock().unwrap();
        let Some(doc) = rt.workspace.get_mut(target.id) else {
            return false;
        };
        if doc.bindings.generation() != target.generation
            || doc.bindings.selection.get() != target.selection
        {
            return false;
        }
        let range = target.selection.start().byte..target.selection.end().byte;
        if doc.editor.document.as_str().get(range.clone()).is_none() {
            return false;
        }
        let inserted = text.replace("\r\n", "\n").replace('\r', "\n");
        doc.editor.moved(target.selection);
        doc.editor.edit_transaction(&EditDelta {
            replaced_range: range,
            inserted,
        });
        drop(rt);
        self.sync_editor(target.id);
        self.changed();
        true
    }
    fn poll_clipboard(&self) {
        let completion = {
            let mut rt = self.runtime.lock().unwrap();
            let result = match rt.clipboard_job.as_ref() {
                Some(ClipboardJob::Copy { handle, .. }) => {
                    handle.try_take().map(|r| r.map(|()| None))
                }
                Some(ClipboardJob::Paste { handle, .. })
                | Some(ClipboardJob::TerminalPaste { handle, .. }) => handle.try_take(),
                None => None,
            };
            result.map(|r| (rt.clipboard_job.take().unwrap(), r))
        };
        let Some((job, result)) = completion else {
            return;
        };
        let message = match (job, result) {
            (ClipboardJob::Copy { cut: false, .. }, Ok(_)) => "Copied to desktop clipboard",
            (ClipboardJob::Copy { target, .. }, Ok(_)) => {
                if self.apply_clipboard(target, "") {
                    "Cut to desktop clipboard"
                } else {
                    "Copied; Cut cancelled because the tab, document, selection, or file operation changed"
                }
            }
            (ClipboardJob::Paste { target, .. }, Ok(Some(text))) if !text.is_empty() => {
                if self.apply_clipboard(target, &text) {
                    "Pasted from desktop clipboard"
                } else {
                    "Paste cancelled because the tab, document, selection, or file operation changed"
                }
            }
            (ClipboardJob::Paste { .. }, Ok(_)) => "Desktop clipboard is empty",
            (ClipboardJob::TerminalPaste { generation, .. }, Ok(Some(text)))
                if !text.is_empty() =>
            {
                if self.terminal_visible.get()
                    && self.terminal.focused()
                    && self.terminal.generation() == generation
                    && self.terminal.paste(&text)
                {
                    "Pasted into terminal"
                } else {
                    "Terminal paste cancelled"
                }
            }
            (ClipboardJob::TerminalPaste { .. }, Ok(_)) => "Desktop clipboard is empty",
            (_, Err(error)) => {
                self.status.set(error.to_string());
                return;
            }
        };
        self.status.set(message.into());
    }
    fn poll_menu(&self) {
        let actions = std::mem::take(&mut self.runtime.lock().unwrap().menu_actions);
        for action in actions {
            action(self);
        }
    }
    fn poll(&self) {
        let dialog = {
            let mut rt = self.runtime.lock().unwrap();
            let result = rt
                .dialog
                .as_ref()
                .and_then(|(purpose, handle)| handle.take_result().map(|r| (*purpose, r)));
            if result.is_some() {
                rt.dialog = None;
            }
            result
        };
        if let Some((purpose, result)) = dialog {
            match result {
                Ok(FileDialogOutcome::Selected(paths)) if !paths.is_empty() => {
                    let path = paths[0].as_path().to_path_buf();
                    match purpose {
                        DialogPurpose::Open => self.open(path),
                        DialogPurpose::OpenFolder => self.open_folder(path),
                        DialogPurpose::Save(id) => self.save_to(id, path),
                    }
                }
                Ok(_) => self.cancel_close(),
                Err(error) => {
                    self.status.set(error.to_string());
                    self.cancel_close();
                }
            }
        }
        let result = {
            let mut rt = self.runtime.lock().unwrap();
            let ready = rt
                .job
                .as_ref()
                .and_then(|job| match job.receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(Err("File worker stopped before completion".into()))
                    }
                });
            ready.map(|result| (rt.job.take().unwrap().operation, result))
        };
        if let Some((operation, result)) = result {
            self.complete_file(operation, result);
        }
        let tree_results = {
            let mut rt = self.runtime.lock().unwrap();
            let mut results = Vec::new();
            let mut index = 0;
            while index < rt.tree_jobs.len() {
                let ready = match rt.tree_jobs[index].receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(Err("Folder worker stopped before completion".into()))
                    }
                };
                if let Some(result) = ready {
                    results.push((rt.tree_jobs.remove(index), result));
                } else {
                    index += 1;
                }
            }
            results
        };
        for (job, result) in tree_results {
            let mut rt = self.runtime.lock().unwrap();
            if job.generation != rt.explorer.generation {
                continue;
            }
            if job.root {
                match result {
                    Ok((path, listing)) => {
                        rt.explorer.set_root(job.generation, path, listing);
                        drop(rt);
                        self.status.set("Folder opened".into());
                    }
                    Err(error) => {
                        drop(rt);
                        self.status.set(error);
                    }
                }
            } else {
                rt.explorer.complete(
                    job.generation,
                    &job.path,
                    result.map(|(_, listing)| listing),
                );
                drop(rt);
            }
            self.refresh_tree();
        }
        self.update_position();
    }
    fn complete_file(&self, operation: FileOperation, result: JobResult) {
        match (operation, result) {
            (FileOperation::Open, Ok(Completion::Open(path, text))) => {
                let id = self
                    .runtime
                    .lock()
                    .unwrap()
                    .workspace
                    .open(path.clone(), &text);
                self.language_opened(&path, id);
                self.activate_unchecked(id);
                self.status.set("Ready".into());
            }
            (FileOperation::Save { id, revision }, Ok(Completion::Save(path))) => {
                let (exists, clean, name) = {
                    let mut rt = self.runtime.lock().unwrap();
                    let exists = rt.workspace.saved(id, revision, path);
                    let doc = rt.workspace.get(id);
                    (
                        exists,
                        doc.is_some_and(|doc| !doc.editor.dirty()),
                        doc.map(|doc| doc.name()).unwrap_or_default(),
                    )
                };
                if exists {
                    self.status.set(format!(
                        "Saved {name}{}",
                        if clean {
                            ""
                        } else {
                            " · newer edits remain unsaved"
                        }
                    ));
                }
                if clean && self.runtime.lock().unwrap().close.is_some() {
                    self.advance_close();
                }
            }
            (_, Err(error)) => self.status.set(error),
            _ => self.status.set("Unexpected file operation result".into()),
        }
        self.changed();
    }
    fn update_position(&self) {
        let id = self.active_id();
        let rt = self.runtime.lock().unwrap();
        let doc = rt.workspace.get(id).unwrap();
        let byte = doc
            .bindings
            .selection
            .get()
            .caret
            .byte
            .min(doc.editor.document.len());
        let line = doc.editor.document.line_index_at_byte(byte);
        let start = doc.editor.document.line_range(line).map_or(0, |r| r.start);
        let column = doc.editor.document.line_text(line).map_or(1, |text| {
            text.get(..byte.saturating_sub(start))
                .map_or(1, |text| text.chars().count() + 1)
        });
        let label = format!(
            "Ln {}, Col {} · UTF-8 · {}",
            line + 1,
            column,
            if doc.crlf { "CRLF" } else { "LF" }
        );
        drop(rt);
        if self.position.get() != label {
            self.position.set(label);
        }
    }
}
fn resolve_save_path(path: &Path) -> std::io::Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let name = path
                .file_name()
                .ok_or_else(|| std::io::Error::other("Choose a file name"))?;
            Ok(std::fs::canonicalize(parent)?.join(name))
        }
        Err(error) => Err(error),
    }
}
impl View for App {
    fn create_element(&self) -> Box<dyn Element> {
        self.content().create_element()
    }
    fn listenables(&self) -> Vec<&dyn Listenable> {
        vec![
            &self.chrome,
            &self.active,
            &self.find_open,
            &self.theme,
            &self.minimap_visible,
            &self.terminal_visible,
        ]
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
impl Application for App {
    fn scenes(&self) -> impl Scene {
        scenes! {
            Window::new(self.window_title(), self.content()).scene_key("main")
                .app_id(env!("APP_ID")).background_color(self.theme.get().colors().background).menu_bar(self.menu()).size(Size::new(1100.0, 760.0)).min_size(Size::new(720.0, 400.0)),
            Window::new("Unsaved Changes", self.confirmation_view()).scene_key("confirm")
                .app_id(env!("APP_ID")).open_at_launch(false).size(Size::new(560.0, 240.0)),
        }
    }
    fn on_window_created(&mut self, ctx: &WindowContext, window: &mut dyn PlatformWindow) {
        if ctx.scene_key.as_str() == "main" {
            let clipboard = window.text_clipboard();
            let mut rt = self.runtime.lock().unwrap();
            rt.owner = Some(ctx.window_id);
            match clipboard {
                Ok(service) => rt.clipboard = Some(service),
                Err(error) => rt.clipboard_error = Some(error.to_string()),
            }
        } else if ctx.scene_key.as_str() == "confirm" {
            // Explicit focus also raises the new surface above the editor on SWS.
            let _ = window.focus();
        }
    }
    fn on_window_sync(&mut self, ctx: &WindowContext, window: &mut dyn PlatformWindow) {
        if ctx.scene_key.as_str() != "main" {
            return;
        }
        let title = self.window_title();
        let mut rt = self.runtime.lock().unwrap();
        if title != rt.title {
            window.set_title(&title);
            rt.title = title;
        }
    }
    fn on_window_close_requested(&mut self, ctx: &WindowContext) -> bool {
        match ctx.scene_key.as_str() {
            "main" => {
                self.request_close(None);
                false
            }
            "confirm" => {
                self.cancel_close();
                true
            }
            _ => true,
        }
    }
    fn on_idle(&mut self) {
        self.terminal.poll();
        self.poll_menu();
        self.poll();
        self.poll_clipboard();
        self.poll_language();
        self.poll_language_interactions();
        if kureha_code::syntax::poll() {
            self.changed();
        }
    }
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut app = App::new();
    if let Some(path) = std::env::args_os().nth(1) {
        let path = PathBuf::from(path);
        // Startup runs before the event loop. All enumeration and file reads
        // still happen on workers; metadata here only chooses the operation.
        if path.is_dir() {
            app.open_folder(path);
        } else {
            app.open(path);
        }
    }
    app.run()
        .map_err(|e| std::io::Error::other(format!("{e:?}")))?;
    Ok(())
}
#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
