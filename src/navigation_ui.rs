//! Quick Open, Command Palette and Go to Line share an overlay, not editor space.
//! One worker at a time performs indexing/filtering; query changes cancel the
//! previous filter and the next idle tick replaces it with the newest request.
use super::*;
use kureha_code::navigation::{self, CommandDescriptor, FileMatch, Index};
use scarlet_ui::event::KeyModifiers;
use std::{
    result::Result,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

const VISIBLE_RESULTS: usize = 80;
const CACHE_LIFETIME: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Closed,
    Files,
    Commands,
    Line,
}
#[derive(Clone, Debug)]
enum Action {
    File(PathBuf),
    Command(&'static str),
}
#[derive(Clone, Debug)]
struct Row {
    label: String,
    detail: String,
    key: String,
    action: Action,
}
enum WorkerResult {
    Index(Result<Index, String>),
    Matches(Vec<FileMatch>),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkerKind {
    Index,
    Matches,
}
struct Worker {
    receiver: mpsc::Receiver<WorkerResult>,
    cancel: Arc<AtomicBool>,
    generation: u64,
    kind: WorkerKind,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
#[derive(Default)]
struct Service {
    root: Option<PathBuf>,
    root_generation: u64,
    index: Option<Arc<Index>>,
    indexed_at: Option<Instant>,
    worker: Option<Worker>,
    generation: u64,
    last_query: Option<(Mode, String)>,
    displayed_query: Option<(Mode, String)>,
    requested: bool,
    error: Option<String>,
}
#[derive(Clone)]
pub(crate) struct Navigation {
    mode: State<Mode>,
    query: State<String>,
    rows: State<Vec<Row>>,
    selected: State<Option<usize>>,
    hint: State<String>,
    service: Arc<Mutex<Service>>,
    pub(super) row_inputs: super::ui::RowInputs,
}
impl Navigation {
    /// The old editor can receive another queued event before the overlay is
    /// mounted. Route text to the query instead of modifying source code.
    pub(super) fn capture_editor_input(&self, event: &Event) -> bool {
        if !self.is_open() {
            return false;
        }
        match event {
            Event::Keyboard(KeyEvent::Char { c }) => {
                if !c.is_control() {
                    self.query.update(|text| text.push(*c));
                }
                true
            }
            Event::Keyboard(KeyEvent::Pressed {
                keycode: KeyCode::Backspace,
                modifiers,
            }) if !modifiers.primary() && !modifiers.alt => {
                self.query.update(|text| {
                    text.pop();
                });
                true
            }
            Event::TextInputCommit { text, .. } => {
                self.query.update(|query| query.push_str(text));
                true
            }
            Event::Keyboard(_)
            | Event::TextInputPreedit { .. }
            | Event::TextInputDeleteSurroundingText { .. }
            | Event::TextInputDone { .. } => true,
            _ => false,
        }
    }
    pub(crate) fn new() -> Self {
        Self {
            mode: State::new(generate_state_id(), Mode::Closed),
            query: State::new(generate_state_id(), String::new()),
            rows: State::initial(generate_state_id()),
            selected: State::initial(generate_state_id()),
            hint: State::new(generate_state_id(), String::new()),
            service: Arc::new(Mutex::new(Service::default())),
            row_inputs: Default::default(),
        }
    }
    pub(crate) fn is_open(&self) -> bool {
        self.mode.get() != Mode::Closed
    }
    pub(crate) fn listenables(&self) -> Vec<&dyn Listenable> {
        vec![
            &self.mode,
            &self.query,
            &self.rows,
            &self.selected,
            &self.hint,
        ]
    }
    fn effective_mode(&self) -> Mode {
        let mode = self.mode.get();
        if mode == Mode::Files {
            match self.query.get().trim_start().chars().next() {
                Some('>') => Mode::Commands,
                Some(':') => Mode::Line,
                _ => mode,
            }
        } else {
            mode
        }
    }
    pub(crate) fn close(&self) {
        self.mode.set(Mode::Closed);
        let mut service = self.service.lock().unwrap();
        service.generation = service.generation.wrapping_add(1);
        service.last_query = None;
        service.displayed_query = None;
        service.requested = false;
        if let Some(worker) = &service.worker {
            worker.cancel.store(true, Ordering::Relaxed);
        }
    }
    fn show_rows(&self, rows: Vec<Row>) {
        self.row_inputs.retain("palette:", std::iter::empty());
        self.selected
            .set(if rows.is_empty() { None } else { Some(0) });
        self.rows.set(rows);
    }
}

macro_rules! command {
    ($id:literal, $label:literal, $keywords:literal, $shortcut:literal) => {
        CommandDescriptor {
            id: $id,
            label: $label,
            keywords: $keywords,
            shortcut: $shortcut,
        }
    };
}
const COMMANDS: &[CommandDescriptor] = &[
    command!(
        "save-all",
        "File: Save All",
        "save all files changed",
        "Primary+Alt+S"
    ),
    command!(
        "quick-open",
        "Go: Quick Open",
        "files filename fuzzy",
        "Primary+P"
    ),
    command!(
        "go-line",
        "Go: Go to Line / Column",
        "navigate row",
        "Ctrl+G"
    ),
    command!("open", "File: Open File…", "browse picker", "Primary+O"),
    command!(
        "open-folder",
        "File: Open Folder…",
        "workspace project directory",
        "Primary+Shift+O"
    ),
    command!("new", "File: New File", "untitled document", "Primary+N"),
    command!("save", "File: Save", "write", "Primary+S"),
    command!(
        "save-as",
        "File: Save As…",
        "write rename copy",
        "Primary+Shift+S"
    ),
    command!("close", "File: Close Editor", "tab", "Primary+W"),
    command!(
        "find",
        "Edit: Find / Replace",
        "search current document",
        "Primary+F"
    ),
    command!(
        "project-search",
        "Search: Find in Files",
        "workspace project search",
        "Primary+Shift+F"
    ),
    command!("undo", "Edit: Undo", "history", "Primary+Z"),
    command!("redo", "Edit: Redo", "history", "Primary+Shift+Z"),
    command!(
        "comment",
        "Edit: Toggle Line Comment",
        "code uncomment",
        "Primary+/"
    ),
    command!("indent", "Edit: Indent Lines", "code tab", "Primary+]"),
    command!(
        "outdent",
        "Edit: Outdent Lines",
        "code unindent tab",
        "Primary+["
    ),
    command!("move-up", "Edit: Move Line Up", "code reorder", "Alt+Up"),
    command!(
        "move-down",
        "Edit: Move Line Down",
        "code reorder",
        "Alt+Down"
    ),
    command!(
        "duplicate-up",
        "Edit: Copy Line Up",
        "code duplicate",
        "Alt+Shift+Up"
    ),
    command!(
        "duplicate-down",
        "Edit: Copy Line Down",
        "code duplicate",
        "Alt+Shift+Down"
    ),
    command!(
        "split-right",
        "View: Split Editor Right",
        "group side by side",
        "Primary+\\"
    ),
    command!(
        "split-down",
        "View: Split Editor Down",
        "group horizontal",
        "Primary+Shift+\\"
    ),
    command!(
        "focus-other",
        "View: Focus Other Editor Group",
        "pane switch",
        ""
    ),
    command!(
        "close-split",
        "View: Close Second Editor Group",
        "pane unsplit",
        ""
    ),
    command!(
        "theme-light",
        "Preferences: Color Theme — Light",
        "appearance colors",
        ""
    ),
    command!(
        "theme-dark",
        "Preferences: Color Theme — Dark",
        "appearance colors",
        ""
    ),
    command!("minimap", "View: Toggle Minimap", "overview", ""),
    command!(
        "terminal",
        "View: Toggle Terminal",
        "shell console panel",
        "Primary+J"
    ),
    command!(
        "problems",
        "View: Problems",
        "diagnostics errors warnings",
        ""
    ),
    command!(
        "definition",
        "Go: Go to Definition",
        "symbol lsp declaration",
        "F12"
    ),
    command!(
        "references",
        "Go: Find All References",
        "symbol usages lsp",
        "Shift+F12"
    ),
    command!(
        "rename-symbol",
        "Code: Rename Symbol in Current File",
        "refactor lsp",
        "F2"
    ),
    command!(
        "format-document",
        "Code: Format Document",
        "prettify organize lsp",
        "Alt+Shift+F"
    ),
    command!(
        "restart-language",
        "Code: Restart Language Server",
        "lsp reconnect",
        ""
    ),
    command!(
        "hover",
        "Code: Show Hover",
        "documentation type lsp",
        "Primary+K Primary+I"
    ),
    command!(
        "completion",
        "Code: Trigger Suggestions",
        "completion autocomplete lsp",
        "Ctrl+Space"
    ),
    command!("next-tab", "View: Next Editor", "tab", "Ctrl+Tab"),
    command!(
        "previous-tab",
        "View: Previous Editor",
        "tab",
        "Ctrl+Shift+Tab"
    ),
    command!(
        "refresh-index",
        "Go: Refresh Project File Index",
        "quick open reload files",
        ""
    ),
];

impl App {
    pub(crate) fn show_quick_open(&self) {
        self.show_navigation(Mode::Files);
    }
    pub(crate) fn show_commands(&self) {
        self.show_navigation(Mode::Commands);
    }
    pub(crate) fn show_go_to_line(&self) {
        self.show_navigation(Mode::Line);
    }
    fn show_navigation(&self, mode: Mode) {
        if self.blocked() {
            return;
        }
        self.dismiss_language_panel();
        self.find_open.set(false);
        self.project.close();
        self.terminal.blur();
        self.nav.close();
        self.nav.service.lock().unwrap().error = None;
        self.nav.query.set(String::new());
        self.nav.show_rows(Vec::new());
        self.nav.mode.set(mode);
        self.poll_navigation();
    }
    /// Call before workbench shortcuts. While open, other shortcut handlers
    /// should stop dispatching and return false to the focused TextField.
    pub(crate) fn navigation_key(&self, key: KeyCode, modifiers: KeyModifiers) -> bool {
        if modifiers.primary() && !modifiers.alt && matches!(key, KeyCode::Char('p' | 'P')) {
            if modifiers.shift {
                self.show_commands();
            } else {
                self.show_quick_open();
            }
            return true;
        }
        if modifiers.control
            && !modifiers.super_key
            && !modifiers.alt
            && !modifiers.shift
            && matches!(key, KeyCode::Char('g' | 'G'))
        {
            self.show_go_to_line();
            return true;
        }
        if key == KeyCode::F(1) && !modifiers.primary() && !modifiers.alt {
            self.show_commands();
            return true;
        }
        if !self.nav.is_open() {
            return false;
        }
        if modifiers.primary() || modifiers.control || modifiers.alt || modifiers.shift {
            return false;
        }
        match key {
            KeyCode::Escape => self.nav.close(),
            KeyCode::Enter => self.accept_navigation(),
            KeyCode::Up | KeyCode::Down => {
                self.poll_navigation();
                let count = self.nav.rows.get().len();
                if count > 0 {
                    let current = self.nav.selected.get().unwrap_or(0).min(count - 1);
                    let next = if key == KeyCode::Down {
                        (current + 1) % count
                    } else {
                        (current + count - 1) % count
                    };
                    self.nav.selected.set(Some(next));
                }
            }
            _ => return false,
        }
        true
    }
    pub(crate) fn poll_navigation(&self) {
        let mode = self.nav.effective_mode();
        let query = self.nav.query.get();
        let mut service = self.nav.service.lock().unwrap();
        if mode == Mode::Closed {
            if service
                .worker
                .as_ref()
                .is_some_and(|w| !matches!(w.receiver.try_recv(), Err(mpsc::TryRecvError::Empty)))
            {
                service.worker = None;
            }
            return;
        }
        let root = if mode == Mode::Files {
            let id = self.active_id();
            let rt = self.runtime.lock().unwrap();
            (
                rt.explorer.root.clone().or_else(|| {
                    rt.workspace
                        .get(id)
                        .and_then(|doc| doc.path.as_ref()?.parent().map(Path::to_path_buf))
                }),
                rt.explorer.generation,
            )
        } else {
            (service.root.clone(), service.root_generation)
        };
        if service.root != root.0 || service.root_generation != root.1 {
            service.root = root.0;
            service.root_generation = root.1;
            service.index = None;
            service.indexed_at = None;
            service.last_query = None;
            service.error = None;
            service.generation = service.generation.wrapping_add(1);
            if let Some(worker) = &service.worker {
                worker.cancel.store(true, Ordering::Relaxed);
            }
        }
        if service.last_query.is_none()
            && service
                .indexed_at
                .is_some_and(|t| t.elapsed() > CACHE_LIFETIME)
        {
            service.index = None;
            service.indexed_at = None;
        }
        let request = (mode, query.clone());
        if service.last_query.as_ref() != Some(&request) {
            service.last_query = Some(request.clone());
            service.displayed_query = None;
            service.generation = service.generation.wrapping_add(1);
            service.requested = true;
            if let Some(worker) = &service.worker {
                // Typing while indexing must not restart the same traversal.
                if worker.kind == WorkerKind::Matches || mode != Mode::Files {
                    worker.cancel.store(true, Ordering::Relaxed);
                }
            }
            self.nav.show_rows(Vec::new());
        }
        let completed =
            service
                .worker
                .as_ref()
                .and_then(|worker| match worker.receiver.try_recv() {
                    Ok(result) => Some(Ok(result)),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(Err("Navigation worker stopped unexpectedly".to_owned()))
                    }
                    Err(mpsc::TryRecvError::Empty) => None,
                });
        if let Some(result) = completed {
            let worker = service.worker.take().unwrap();
            if !worker.cancel.load(Ordering::Relaxed) {
                match result {
                    Ok(WorkerResult::Index(Ok(index))) if !index.report.cancelled => {
                        service.index = Some(Arc::new(index));
                        service.indexed_at = Some(Instant::now());
                        service.error = None;
                        service.requested = true;
                    }
                    Ok(WorkerResult::Index(Err(error))) | Err(error) => {
                        service.error = Some(error.clone());
                        service.requested = false;
                        self.nav.hint.set(error);
                    }
                    Ok(WorkerResult::Matches(hits))
                        if worker.generation == service.generation && mode == Mode::Files =>
                    {
                        let index = service.index.as_ref().unwrap();
                        let rows = hits
                            .into_iter()
                            .map(|hit| {
                                let path = index.files[hit.file_index].clone();
                                let label = path
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into_owned();
                                Row {
                                    label,
                                    detail: hit.relative_path.clone(),
                                    key: hit.relative_path,
                                    action: Action::File(path),
                                }
                            })
                            .collect::<Vec<_>>();
                        let count = rows.len();
                        self.nav.show_rows(rows);
                        self.nav.hint.set(index_hint(index, count));
                        service.displayed_query = Some(request.clone());
                    }
                    _ => {}
                }
            }
        }
        if !service.requested {
            return;
        }
        match mode {
            Mode::Closed => {}
            Mode::Commands => {
                let rows = navigation::filter_commands(COMMANDS, &query, VISIBLE_RESULTS)
                    .into_iter()
                    .map(|hit| {
                        let command = COMMANDS[hit.command_index];
                        Row {
                            label: command.label.into(),
                            detail: shortcut(command.shortcut),
                            key: command.id.into(),
                            action: Action::Command(command.id),
                        }
                    })
                    .collect::<Vec<_>>();
                let count = rows.len();
                self.nav.show_rows(rows);
                self.nav
                    .hint
                    .set(navigation::validate_query(&query).err().map_or_else(
                        || {
                            format!(
                                "{count} commands · ↑ ↓ to select · Enter to run · Esc to close"
                            )
                        },
                        |e| e.to_string(),
                    ));
                service.requested = false;
                service.displayed_query = Some(request);
            }
            Mode::Line => {
                self.nav.hint.set(if query.trim().is_empty() {
                    "Enter line:column · Enter to jump · Esc to close".into()
                } else {
                    navigation::parse_line_column(&query)
                        .map(|p| {
                            format!("Go to line {}, column {} · Enter to jump", p.line, p.column)
                        })
                        .unwrap_or_else(|e| e.to_string())
                });
                service.requested = false;
                service.displayed_query = Some(request);
            }
            Mode::Files => {
                if let Err(error) = navigation::validate_query(&query) {
                    self.nav.hint.set(error.to_string());
                    service.requested = false;
                    return;
                }
                let Some(root) = service.root.clone() else {
                    self.nav.hint.set(
                        "Open a folder to search files · Type > for commands or : for a line"
                            .into(),
                    );
                    service.requested = false;
                    return;
                };
                if service.worker.is_some() {
                    self.nav.hint.set(
                        if service.index.is_some() {
                            "Searching files…"
                        } else {
                            "Indexing project files…"
                        }
                        .into(),
                    );
                    return;
                }
                if let Some(error) = &service.error {
                    self.nav.hint.set(error.clone());
                    service.requested = false;
                    return;
                }
                let index = service.index.clone();
                let kind = if index.is_some() {
                    WorkerKind::Matches
                } else {
                    WorkerKind::Index
                };
                let cancel = Arc::new(AtomicBool::new(false));
                let worker_cancel = cancel.clone();
                let (sender, receiver) = mpsc::sync_channel(1);
                let spawn = thread::Builder::new()
                    .name("kureha-navigation".into())
                    .spawn(move || {
                        let result = match index {
                            Some(index) => {
                                WorkerResult::Matches(navigation::quick_open_cancellable(
                                    &index,
                                    &query,
                                    VISIBLE_RESULTS,
                                    &worker_cancel,
                                ))
                            }
                            None => WorkerResult::Index(navigation::scan(&root, &worker_cancel)),
                        };
                        let _ = sender.send(result);
                    });
                match spawn {
                    Ok(_) => {
                        service.worker = Some(Worker {
                            receiver,
                            cancel,
                            generation: service.generation,
                            kind,
                        });
                        self.nav.hint.set(
                            if kind == WorkerKind::Index {
                                "Indexing project files…"
                            } else {
                                "Searching files…"
                            }
                            .into(),
                        );
                    }
                    Err(error) => {
                        self.nav
                            .hint
                            .set(format!("Could not start file search: {error}"));
                    }
                }
                service.requested = false;
            }
        }
    }
    fn accept_navigation(&self) {
        if self.blocked() {
            return;
        }
        self.poll_navigation();
        if self.nav.effective_mode() == Mode::Line {
            let position = match navigation::parse_line_column(&self.nav.query.get()) {
                Ok(position) => position,
                Err(error) => {
                    self.nav.hint.set(error.to_string());
                    return;
                }
            };
            let id = self.active_id();
            let Some(bindings) = self.bindings(id) else {
                return;
            };
            let text = bindings.document.get();
            let byte = navigation::line_column_to_offset(&text.as_str(), position);
            let selection = TextSelection::collapsed(byte);
            if let Some(doc) = self.runtime.lock().unwrap().workspace.get_mut(id) {
                doc.editor.moved(selection);
            }
            self.nav.close();
            bindings.selection.set(selection);
            self.invalidate_clipboard();
            self.update_position();
            return;
        }
        let request = (self.nav.effective_mode(), self.nav.query.get());
        if self.nav.service.lock().unwrap().displayed_query.as_ref() != Some(&request) {
            return;
        }
        let Some(index) = self.nav.selected.get() else {
            return;
        };
        let Some(row) = self.nav.rows.get().get(index).cloned() else {
            return;
        };
        self.nav.close();
        match row.action {
            Action::File(path) => self.open(path),
            Action::Command(id) => self.execute_navigation_command(id),
        }
    }
    fn execute_navigation_command(&self, id: &str) {
        match id {
            "quick-open" => self.show_quick_open(),
            "go-line" => self.show_go_to_line(),
            "open" => self.picker(DialogPurpose::Open),
            "open-folder" => self.show_folder(),
            "new" => self.new_document(),
            "save" => self.save(),
            "save-all" => self.save_all(),
            "save-as" => self.picker(DialogPurpose::Save(self.active_id())),
            "close" => self.request_close(Some(self.active_id())),
            "find" => self.show_find(),
            "project-search" => self.show_project_search(),
            "undo" => self.undo(false),
            "redo" => self.undo(true),
            "comment" => self.edit_command(super::editing_ui::Command::Comment),
            "indent" => self.edit_command(super::editing_ui::Command::Indent),
            "outdent" => self.edit_command(super::editing_ui::Command::Outdent),
            "move-up" => self.edit_command(super::editing_ui::Command::MoveUp),
            "move-down" => self.edit_command(super::editing_ui::Command::MoveDown),
            "duplicate-up" => self.edit_command(super::editing_ui::Command::DuplicateUp),
            "duplicate-down" => self.edit_command(super::editing_ui::Command::DuplicateDown),
            "split-right" => self.split_editor(false),
            "split-down" => self.split_editor(true),
            "focus-other" => self.focus_other_group(),
            "close-split" => self.close_split(),
            "theme-light" => self.set_theme(kureha_code::theme::Theme::Light),
            "theme-dark" => self.set_theme(kureha_code::theme::Theme::Dark),
            "minimap" => self.toggle_minimap(),
            "terminal" => self.toggle_terminal(),
            "problems" => self.show_problems(),
            "definition" => self.language_request("textDocument/definition"),
            "references" => self.find_references(),
            "rename-symbol" => self.show_rename(),
            "format-document" => self.format_document(),
            "restart-language" => self.restart_language_server(),
            "hover" => self.language_request("textDocument/hover"),
            "completion" => self.language_request("textDocument/completion"),
            "next-tab" => self.next_tab(false),
            "previous-tab" => self.next_tab(true),
            "refresh-index" => {
                let mut service = self.nav.service.lock().unwrap();
                service.index = None;
                service.indexed_at = None;
                service.error = None;
                drop(service);
                self.show_quick_open();
            }
            _ => {}
        }
    }
    pub(crate) fn navigation_overlay(&self) -> impl View + Clone + use<> {
        let colors = self.theme.get().colors();
        let submit = self.clone();
        let close = self.clone();
        let rows = self.clone();
        let expected_query = self.nav.query.get();
        let placeholder = match self.nav.mode.get() {
            Mode::Commands => "Type a command",
            Mode::Line => "Go to line:column",
            _ => "Search files by name · > commands · : line",
        };
        let height = if self.nav.effective_mode() == Mode::Line {
            0.0
        } else {
            (self.nav.rows.get().len().min(8) as f32 * 34.0).min(280.0)
        };
        if_view!(self.nav.is_open(),
            super::popup::Anchored::top_center(vstack! {
                TextField::new(self.nav.query.clone()).placeholder(placeholder).placeholder_color(colors.muted).font_size(13.0)
                    .text_color(colors.foreground).background_color(colors.elevated)
                    .border_color(colors.border).focused_border_color(colors.accent)
                    .autofocus(true).on_submit(move || submit.accept_navigation())
                    .on_cancel(move || close.nav.close()).frame(f32::INFINITY, 32.0),
                ListView::new(self.nav.rows.clone(), self.nav.selected.clone(), 34.0, move |index, row: Row, selected| {
                    let app = rows.clone();
                    let query = expected_query.clone();
                    let input = app.nav.row_inputs.get(format!("palette:{}", row.key));
                    super::ui::list_row(hstack! {
                        Text::new(row.label).font_size(13.0).color(colors.foreground).padding(6.0)
                            .frame_width(260.0).clip(),
                        Text::new(row.detail).font_size(11.0).color(colors.muted).frame_width(300.0).clip(),
                        Spacer::new(),
                    }.spacing(8.0).frame(f32::INFINITY, 34.0), input, selected == Some(index), true, colors,
                        move || {
                            if app.nav.query.get() == query {
                                app.nav.selected.set(Some(index)); app.accept_navigation();
                            }
                        })
                }).item_key(|row| row.key.clone().into()).frame(f32::INFINITY, height),
                Text::from_state(self.nav.hint.clone()).font_size(11.0).color(colors.muted)
                    .frame_width(f32::INFINITY),
            }.spacing(6.0).padding(8.0).background(colors.elevated).frame_width(f32::INFINITY).border(colors.border, 1.0).clip()),
            VStack::new(())
        )
    }
}
fn shortcut(shortcut: &str) -> String {
    shortcut.replace(
        "Primary",
        if cfg!(target_os = "macos") {
            "Cmd"
        } else {
            "Ctrl"
        },
    )
}
fn index_hint(index: &Index, matches: usize) -> String {
    let prefix = if matches == 0 {
        "No matching files".into()
    } else {
        format!("{matches} results · {} indexed files", index.files.len())
    };
    if index.truncated {
        format!(
            "{prefix} · Partial index: {} limits, {} errors",
            index.report.limits.len(),
            index.report.errors.len() + index.report.suppressed_errors
        )
    } else {
        format!("{prefix} · ↑ ↓ select · Enter open · Esc close")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app_with_text(text: &str) -> App {
        let app = App::new();
        app.runtime.lock().unwrap().workspace.documents[0].editor =
            kureha_code::editor::Editor::new(text);
        app.sync_editor(app.active_id());
        app
    }
    fn primary() -> KeyModifiers {
        let mut modifiers = KeyModifiers::empty();
        if cfg!(target_os = "macos") {
            modifiers.super_key = true;
        } else {
            modifiers.control = true;
        }
        modifiers
    }
    #[test]
    fn shortcuts_preserve_textfield_editing_and_goto_uses_control_on_macos() {
        let app = app_with_text("猫🍁x\nnext line");
        assert!(app.navigation_key(KeyCode::Char('p'), primary()));
        assert_eq!(app.nav.mode.get(), Mode::Files);
        let mut palette = primary();
        palette.shift = true;
        assert!(app.navigation_key(KeyCode::Char('p'), palette));
        assert_eq!(app.nav.mode.get(), Mode::Commands);
        for key in ['a', 'c', 'x', 'v', 'z', 'y'] {
            assert!(!app.navigation_key(KeyCode::Char(key), primary()));
        }
        app.nav.close();
        if cfg!(target_os = "macos") {
            assert!(!app.navigation_key(KeyCode::Char('g'), primary()));
        }
        let mut control = KeyModifiers::empty();
        control.control = true;
        assert!(app.navigation_key(KeyCode::Char('g'), control));
        assert_eq!(app.nav.mode.get(), Mode::Line);
        app.nav.query.set("1:2".into());
        assert!(app.navigation_key(KeyCode::Enter, KeyModifiers::empty()));
        assert!(!app.nav.is_open());
        assert_eq!(
            app.bindings(app.active_id())
                .unwrap()
                .selection
                .get()
                .caret
                .byte,
            3
        );
    }
    #[test]
    fn goto_affects_only_the_active_editor_group_and_invalid_input_stays_open() {
        let app = app_with_text("猫🍁x\nnext line");
        let id = app.active_id();
        let left = app.bindings(id).unwrap();
        left.selection.set(TextSelection::collapsed(3));
        app.split_editor(false);
        app.show_go_to_line();
        app.nav.query.set("0:2".into());
        app.accept_navigation();
        assert!(app.nav.is_open());
        app.nav.query.set("2:4".into());
        app.accept_navigation();
        assert!(!app.nav.is_open());
        assert_eq!(app.bindings(id).unwrap().selection.get().caret.byte, 12);
        assert_eq!(left.selection.get().caret.byte, 3);
    }
    #[test]
    fn stale_worker_matches_never_open_the_previous_querys_file() {
        let app = app_with_text("");
        let root = PathBuf::from("/project");
        app.runtime.lock().unwrap().explorer.root = Some(root.clone());
        app.nav.mode.set(Mode::Files);
        app.nav.query.set("new.rs".into());
        let (sender, receiver) = mpsc::sync_channel(1);
        sender
            .send(WorkerResult::Matches(vec![FileMatch {
                file_index: 0,
                relative_path: "old.rs".into(),
                score: 1,
                matched_ranges: vec![],
            }]))
            .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut service = app.nav.service.lock().unwrap();
            service.root = Some(root.clone());
            service.index = Some(Arc::new(Index {
                root: root.clone(),
                files: vec![root.join("old.rs"), root.join("new.rs")],
                ..Index::default()
            }));
            service.last_query = Some((Mode::Files, "old.rs".into()));
            service.generation = 1;
            service.worker = Some(Worker {
                receiver,
                cancel: cancel.clone(),
                generation: 1,
                kind: WorkerKind::Matches,
            });
        }
        app.accept_navigation();
        assert!(
            app.runtime.lock().unwrap().job.is_none(),
            "stale row started opening a file"
        );
        assert!(cancel.load(Ordering::Relaxed));
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.nav.service.lock().unwrap().displayed_query.as_ref()
            != Some(&(Mode::Files, "new.rs".into()))
        {
            app.poll_navigation();
            assert!(Instant::now() < deadline, "newest query did not complete");
            thread::yield_now();
        }
        assert_eq!(app.nav.rows.get().len(), 1);
        assert_eq!(app.nav.rows.get()[0].label, "new.rs");
        app.nav.close();
    }
    #[test]
    fn navigation_overlay_preserves_editor_bounds_and_returns_keyboard_focus() {
        use scarlet_ui::pipeline::RenderingPipeline;
        let app = app_with_text(&"line of source code\n".repeat(80));
        let window = || {
            Window::new("Navigation test", app.content())
                .decorated(false)
                .size(Size::new(1100.0, 760.0))
        };
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_paint_enabled(false);
        pipeline.set_root(window().create_element());
        pipeline.layout_initial();
        pipeline.render();
        fn document(
            e: &dyn Element,
            origin: Point,
        ) -> Option<(scarlet_ui::element::ElementId, Rect)> {
            let point = Point::new(origin.x + e.position().x, origin.y + e.position().y);
            if e.type_name_debug().contains("TextViewRenderObject") {
                return Some((e.id(), Rect::new(point, e.bounds().size)));
            }
            e.children()
                .iter()
                .find_map(|child| document(child.as_ref(), point))
        }
        let original = document(pipeline.element_tree().root().unwrap(), Point::ZERO).unwrap();
        let bindings = app.bindings(app.active_id()).unwrap();
        for mode in [Mode::Commands, Mode::Line, Mode::Files] {
            app.show_navigation(mode);
            // Input can arrive in the same platform batch before the overlay
            // has mounted and taken focus from the previous editor.
            pipeline.handle_event(&Event::Keyboard(KeyEvent::Char { c: 'R' }));
            assert_eq!(app.nav.query.get(), "R");
            assert_eq!(
                bindings.document.get().as_str(),
                "line of source code\n".repeat(80)
            );
            app.nav.query.set(String::new());
            pipeline
                .element_tree_mut()
                .root_mut()
                .unwrap()
                .update(&window());
            pipeline.layout_initial();
            pipeline.render();
            assert_eq!(
                document(pipeline.element_tree().root().unwrap(), Point::ZERO).unwrap(),
                original
            );
            pipeline.handle_event(&Event::Keyboard(KeyEvent::Char { c: 'a' }));
            pipeline.render();
            assert_eq!(
                app.nav.query.get(),
                "a",
                "typing did not reach the navigation field"
            );
            pipeline.handle_event(&Event::Keyboard(KeyEvent::Pressed {
                keycode: KeyCode::Char('a'),
                modifiers: primary(),
            }));
            pipeline.handle_event(&Event::Keyboard(KeyEvent::Char { c: 'b' }));
            pipeline.render();
            assert_eq!(
                app.nav.query.get(),
                "b",
                "Select All did not stay in the navigation field"
            );
            assert_eq!(bindings.selection.get(), TextSelection::collapsed(0));
            pipeline.handle_event(&Event::Keyboard(KeyEvent::Pressed {
                keycode: KeyCode::Escape,
                modifiers: KeyModifiers::empty(),
            }));
            pipeline
                .element_tree_mut()
                .root_mut()
                .unwrap()
                .update(&window());
            pipeline.layout_initial();
            pipeline.render();
            assert!(!app.nav.is_open());
        }
        pipeline.handle_event(&Event::Keyboard(KeyEvent::Pressed {
            keycode: KeyCode::Char('a'),
            modifiers: primary(),
        }));
        pipeline.render();
        assert_eq!(
            bindings.selection.get().normalized_range(),
            Some(0..bindings.document.get().len())
        );
        pipeline.teardown();
    }

    #[test]
    fn palette_rows_are_flat_and_activate_across_their_full_width() {
        use scarlet_ui::{
            event::{MouseButton, MouseEvent},
            pipeline::RenderingPipeline,
            renderer::{PaintCommand, PaintContext},
            views::modifiers::OnEventRenderObject,
        };

        fn row(e: &dyn Element, origin: Point) -> Option<(&dyn Element, Rect)> {
            let p = Point::new(origin.x + e.position().x, origin.y + e.position().y);
            if e.render_object()
                .is_some_and(|r| r.as_any().is::<OnEventRenderObject>())
            {
                return Some((e, Rect::new(p, e.bounds().size)));
            }
            e.children().iter().find_map(|child| row(child.as_ref(), p))
        }

        fn assert_flat(e: &dyn Element) {
            if let Some(render) = e.render_object() {
                let mut paint = PaintContext::new();
                render.paint(&mut paint, Point::ZERO);
                assert!(
                    !paint.commands().iter().any(|command| matches!(
                        command,
                        PaintCommand::StrokeRect { .. }
                            | PaintCommand::StrokeRoundedRect { .. }
                            | PaintCommand::FillRoundedRect { .. }
                            | PaintCommand::FillVerticalGradientRoundedRect { .. }
                    )),
                    "row painted button or focus chrome"
                );
            }
            for child in e.children() {
                assert_flat(child.as_ref());
            }
        }

        for theme in kureha_code::theme::Theme::ALL {
            // Label, detail column, and trailing blank space are one target.
            for offset in [20.0, 400.0, 590.0] {
                let app = app_with_text("source");
                app.set_theme(theme);
                app.show_commands();
                app.nav.query.set("minimap".into());
                app.poll_navigation();
                assert_eq!(app.nav.rows.get().len(), 1);
                let visible = app.minimap_visible.get();
                let window = || {
                    Window::new("Palette row", app.navigation_overlay())
                        .decorated(false)
                        .size(Size::new(700.0, 300.0))
                };
                let mut pipeline = RenderingPipeline::new();
                pipeline.set_paint_enabled(false);
                pipeline.set_root(window().create_element());
                pipeline.layout_initial();
                pipeline.render();
                let (element, bounds) =
                    row(pipeline.element_tree().root().unwrap(), Point::ZERO).expect("palette row");
                assert!(bounds.size.width > 590.0);
                assert_flat(element);
                let x = (bounds.origin.x + offset) as i32;
                let y = (bounds.origin.y + bounds.size.height / 2.0) as i32;
                let outside = (bounds.origin.x + bounds.size.width + 15.0) as i32;
                for event in [
                    MouseEvent::Moved { x, y },
                    MouseEvent::ButtonPressed {
                        button: MouseButton::Left,
                        x,
                        y,
                        click_count: 1,
                    },
                    MouseEvent::Moved { x: outside, y },
                    MouseEvent::ButtonReleased {
                        button: MouseButton::Left,
                        x: outside,
                        y,
                        click_count: 1,
                    },
                ] {
                    pipeline.handle_event(&Event::Mouse(event));
                    pipeline.render();
                }
                assert_eq!(
                    app.minimap_visible.get(),
                    visible,
                    "release outside must cancel"
                );
                assert!(app.nav.is_open());
                for event in [
                    MouseEvent::Moved { x, y },
                    MouseEvent::ButtonPressed {
                        button: MouseButton::Left,
                        x,
                        y,
                        click_count: 1,
                    },
                ] {
                    pipeline.handle_event(&Event::Mouse(event));
                    pipeline.render();
                    let (element, _) =
                        row(pipeline.element_tree().root().unwrap(), Point::ZERO).unwrap();
                    assert_flat(element);
                    assert_eq!(
                        app.minimap_visible.get(),
                        visible,
                        "press must not activate"
                    );
                }
                // An ordinary workbench rebuild must retain the pending press.
                pipeline
                    .element_tree_mut()
                    .root_mut()
                    .unwrap()
                    .update(&window());
                pipeline.layout_initial();
                pipeline.render();
                pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonReleased {
                    button: MouseButton::Left,
                    x,
                    y,
                    click_count: 1,
                }));
                assert_eq!(app.minimap_visible.get(), !visible);
                assert!(!app.nav.is_open());
                pipeline.teardown();
            }
        }
    }
    #[test]
    fn navigation_modes_accept_prefixes_and_close_cancels_inflight_work() {
        let nav = Navigation::new();
        nav.mode.set(Mode::Files);
        nav.query.set(">theme".into());
        assert_eq!(nav.effective_mode(), Mode::Commands);
        nav.query.set(":42:3".into());
        assert_eq!(nav.effective_mode(), Mode::Line);
        nav.query.set("main.rs".into());
        assert_eq!(nav.effective_mode(), Mode::Files);
        let (_sender, receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        nav.service.lock().unwrap().worker = Some(Worker {
            receiver,
            cancel: cancel.clone(),
            generation: 0,
            kind: WorkerKind::Index,
        });
        nav.close();
        assert!(!nav.is_open());
        assert!(cancel.load(Ordering::Relaxed));
    }
    #[test]
    fn palette_descriptors_are_unique_and_include_core_workbench_actions() {
        let ids: BTreeSet<_> = COMMANDS.iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), COMMANDS.len());
        for required in [
            "project-search",
            "split-right",
            "definition",
            "terminal",
            "theme-dark",
            "go-line",
        ] {
            assert!(ids.contains(required));
        }
        assert!(navigation::filter_commands(COMMANDS, "split", 10).len() >= 2);
    }
}
