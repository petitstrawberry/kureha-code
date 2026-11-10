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
    Themes {
        code_only: bool,
    },
}
#[derive(Clone, Debug)]
enum Action {
    File(PathBuf),
    Command(&'static str),
    Theme(kureha_code::theme::Theme),
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
    preview_original: Arc<Mutex<Option<kureha_code::theme::Theme>>>,
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
            preview_original: Default::default(),
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
    fn set_hint(&self, hint: String) {
        if self.hint.get() != hint {
            self.hint.set(hint);
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
        "markdown-refresh",
        "Markdown: Refresh Preview",
        "reload images",
        ""
    ),
    command!(
        "markdown-preview-side",
        "Markdown: Open Preview to Side",
        "split source",
        ""
    ),
    command!(
        "markdown-source",
        "Markdown: Show Source",
        "preview editor",
        ""
    ),
    command!(
        "markdown-file-sync",
        "Markdown: Toggle File Sync",
        "preview follow pin",
        ""
    ),
    command!(
        "markdown-scroll-sync",
        "Markdown: Toggle Scroll Sync",
        "preview position",
        ""
    ),
    command!(
        "markdown-preview",
        "Markdown: Open Preview",
        "source render",
        "Primary+Shift+V"
    ),
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
    command!(
        "settings",
        "Preferences: Open Settings",
        "appearance preferences",
        "Primary+,"
    ),
    command!(
        "settings-file",
        "Preferences: Open Settings File",
        "configuration toml",
        ""
    ),
    command!(
        "acknowledgements",
        "Help: Acknowledgements",
        "licenses copyright fonts themes",
        ""
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
        "View: Close Editor Group",
        "pane unsplit",
        ""
    ),
    command!(
        "select-color-theme",
        "Preferences: Select Color Theme",
        "appearance colors palette",
        "Primary+K Primary+T"
    ),
    command!(
        "select-code-theme",
        "Preferences: Select Code Color Theme",
        "code editor terminal syntax colors palette",
        ""
    ),
    command!(
        "theme-edit",
        "Preferences: Customize Theme…",
        "appearance colors settings toml",
        ""
    ),
    command!(
        "theme-reload",
        "Preferences: Reload Custom Theme",
        "appearance colors settings",
        ""
    ),
    command!(
        "tab-outer-left",
        "View: Move Tab to Far Left",
        "entire editor area layout",
        ""
    ),
    command!(
        "tab-outer-right",
        "View: Move Tab to Far Right",
        "entire editor area layout",
        ""
    ),
    command!(
        "tab-outer-top",
        "View: Move Tab to Top",
        "entire editor area layout",
        ""
    ),
    command!(
        "tab-outer-bottom",
        "View: Move Tab to Bottom",
        "entire editor area layout",
        ""
    ),
    command!("word-wrap", "View: Toggle Word Wrap", "code wrapping lines", "Alt+Z"),
    command!("minimap", "View: Toggle Minimap", "overview", ""),
    command!(
        "restart-terminal",
        "Terminal: Restart Session",
        "shell restart",
        ""
    ),
    command!("new-terminal", "Terminal: New Session", "shell tab", ""),
    command!(
        "panel",
        "View: Toggle Panel",
        "panel terminal console dock",
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
    command!(
        "go-back",
        "Go: Back",
        "navigation history previous location",
        "GoBack"
    ),
    command!(
        "go-forward",
        "Go: Forward",
        "navigation history next location",
        "GoForward"
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
    pub(super) fn close_navigation(&self) {
        let original = self.nav.preview_original.lock().unwrap().take();
        let was_open = self.nav.is_open();
        self.nav.close();
        if was_open {
            let session = self.runtime.lock().unwrap().terminal_return_focus.take();
            if let Some(id) = session {
                self.terminal.select(id);
            }
        }
        if let Some(original) = original {
            if self.theme.get() != original {
                self.apply_theme(original);
            }
        }
    }
    pub(super) fn show_theme_picker(&self, code_only: bool) {
        self.show_navigation(Mode::Themes { code_only });
    }
    fn preview_selected_theme(&self) {
        if !matches!(self.nav.mode.get(), Mode::Themes { .. }) {
            return;
        }
        let query = (self.nav.effective_mode(), self.nav.query.get());
        if self.nav.service.lock().unwrap().displayed_query.as_ref() != Some(&query) {
            return;
        }
        let selected = self
            .nav
            .selected
            .get()
            .and_then(|index| self.nav.rows.get().get(index).cloned());
        if let Some(Row {
            action: Action::Theme(theme),
            ..
        }) = selected
        {
            if self.theme.get() != theme {
                self.apply_theme(theme);
            }
        }
    }
    fn theme_rows(&self, code_only: bool, query: &str) -> Vec<Row> {
        use kureha_code::theme::{preset, Theme, PRESETS};
        let original = self
            .nav
            .preview_original
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| self.theme.get());
        let mut choices = vec![preset("kureha").unwrap(), Theme::Light, Theme::Dark];
        choices.extend(
            PRESETS
                .iter()
                .filter(|(id, _)| *id != "kureha")
                .filter_map(|(id, _)| preset(id)),
        );
        let mut rows = choices
            .into_iter()
            .map(|choice| {
                let label = choice.name().to_owned();
                let theme = if code_only {
                    original.with_code_theme(&choice)
                } else {
                    self.preferences
                        .get()
                        .font_size
                        .map_or(choice.clone(), |size| choice.with_font_size(size))
                };
                Row {
                    key: format!("theme:{label}"),
                    label,
                    detail: if theme == original {
                        "Current".into()
                    } else {
                        String::new()
                    },
                    action: Action::Theme(theme),
                }
            })
            .collect::<Vec<_>>();
        // Keep a customized palette available, including editor-only overrides.
        if !rows
            .iter()
            .any(|row| matches!(&row.action, Action::Theme(theme) if *theme == original))
        {
            rows.insert(
                0,
                Row {
                    label: original.name().into(),
                    detail: "Current · customized".into(),
                    key: "theme:current-custom".into(),
                    action: Action::Theme(original),
                },
            );
        }
        let query = query.trim().to_lowercase();
        rows.retain(|row| row.label.to_lowercase().contains(&query));
        rows
    }
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
        self.close_navigation();
        self.runtime.lock().unwrap().terminal_return_focus =
            self.terminal.focused_session().map(|session| session.id);
        self.blur_terminals();
        if matches!(mode, Mode::Themes { .. }) {
            *self.nav.preview_original.lock().unwrap() = Some(self.theme.get());
        }
        self.nav.service.lock().unwrap().error = None;
        self.nav.query.set(String::new());
        self.nav.show_rows(Vec::new());
        self.nav.mode.set(mode);
        self.poll_navigation();
    }
    /// Call before workbench shortcuts. While open, other shortcut handlers
    /// should stop dispatching and return false to the focused TextField.
    pub(crate) fn navigation_key(&self, key: KeyCode, modifiers: KeyModifiers) -> bool {
        if !self.nav.is_open() && modifiers.alt && !modifiers.primary() && !modifiers.control && !modifiers.shift && matches!(key, KeyCode::Char('z' | 'Z')) {
            self.toggle_word_wrap();
            return true;
        }
        if !self.nav.is_open()
            && cfg!(target_os = "macos")
            && modifiers.control
            && !modifiers.super_key
            && !modifiers.alt
            && matches!(key, KeyCode::Char('-' | '_'))
        {
            if modifiers.shift {
                self.go_forward();
            } else {
                self.go_back();
            }
            return true;
        }
        if !self.nav.is_open()
            && !cfg!(target_os = "macos")
            && modifiers.alt
            && !modifiers.primary()
            && !modifiers.shift
        {
            match key {
                KeyCode::Left => {
                    self.go_back();
                    return true;
                }
                KeyCode::Right => {
                    self.go_forward();
                    return true;
                }
                _ => {}
            }
        }
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
            KeyCode::Escape => self.close_navigation(),
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
                    self.preview_selected_theme();
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
                    id.and_then(|id| rt.workspace.get(id))
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
            // Keep the previous file results until the worker publishes the next
            // set. Clearing them here collapses and re-expands the popup per key.
            let retain_results = mode == Mode::Files
                && service
                    .last_query
                    .as_ref()
                    .is_some_and(|(mode, _)| *mode == Mode::Files);
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
            if !retain_results {
                self.nav.show_rows(Vec::new());
            }
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
                        self.nav.show_rows(Vec::new());
                        self.nav.set_hint(error);
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
                        self.nav.set_hint(index_hint(index, count));
                        service.displayed_query = Some(request.clone());
                    }
                    _ => {}
                }
            }
        }
        if !service.requested {
            drop(service);
            self.preview_selected_theme();
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
                    .set_hint(navigation::validate_query(&query).err().map_or_else(
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
            Mode::Themes { code_only } => {
                let rows = if navigation::validate_query(&query).is_ok() {
                    self.theme_rows(code_only, &query)
                } else {
                    Vec::new()
                };
                let selected = if query.trim().is_empty() {
                    rows.iter()
                        .position(|row| row.detail.starts_with("Current"))
                } else {
                    None
                };
                let empty = rows.is_empty();
                self.nav.show_rows(rows);
                if selected.is_some() {
                    self.nav.selected.set(selected);
                }
                self.nav.set_hint(
                    if empty {
                        "No matching themes"
                    } else {
                        "↑ ↓ or hover to preview · Enter to keep · Esc to restore"
                    }
                    .into(),
                );
                service.requested = false;
                service.displayed_query = Some(request);
            }
            Mode::Line => {
                self.nav.set_hint(if query.trim().is_empty() {
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
                    self.nav.show_rows(Vec::new());
                    self.nav.set_hint(error.to_string());
                    service.requested = false;
                    return;
                }
                let Some(root) = service.root.clone() else {
                    self.nav.set_hint(
                        "Open a folder to search files · Type > for commands or : for a line"
                            .into(),
                    );
                    service.requested = false;
                    return;
                };
                if service.worker.is_some() {
                    self.nav.set_hint(
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
                    self.nav.show_rows(Vec::new());
                    self.nav.set_hint(error.clone());
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
                        self.nav.set_hint(
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
                            .set_hint(format!("Could not start file search: {error}"));
                    }
                }
                service.requested = false;
            }
        }
        drop(service);
        self.preview_selected_theme();
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
                    self.nav.set_hint(error.to_string());
                    return;
                }
            };
            let Some(id) = self.active_id() else {
                return;
            };
            let Some(bindings) = self.bindings(id) else {
                return;
            };
            let text = bindings.document.get();
            let byte = navigation::line_column_to_offset(&text.as_str(), position);
            let selection = TextSelection::collapsed(byte);
            if let Some(doc) = self.runtime.lock().unwrap().workspace.get_mut(id) {
                doc.editor.moved(selection);
            }
            if bindings.selection.get() != selection {
                self.remember_location();
            }
            self.close_navigation();
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
        let code_only = matches!(self.nav.mode.get(), Mode::Themes { code_only: true });
        let selected_theme = (row.key != "theme:current-custom").then(|| row.label.clone());
        if matches!(row.action, Action::Theme(_)) {
            self.nav.preview_original.lock().unwrap().take();
        }
        self.close_navigation();
        match row.action {
            Action::File(path) => self.open(path),
            Action::Command(id) => self.execute_navigation_command(id),
            Action::Theme(theme) => {
                self.apply_theme(theme);
                if let Some(name) = selected_theme {
                    self.change_preferences(if code_only {
                        super::preferences::Change::Code(Some(name))
                    } else {
                        super::preferences::Change::Color(Some(name))
                    });
                }
            }
        }
    }
    fn execute_navigation_command(&self, id: &str) {
        match id {
            "go-back" => self.go_back(),
            "go-forward" => self.go_forward(),
            "quick-open" => self.show_quick_open(),
            "go-line" => self.show_go_to_line(),
            "open" => self.picker(DialogPurpose::Open),
            "open-folder" => self.show_folder(),
            "new" => self.new_document(),
            "save" => self.save(),
            "save-all" => self.save_all(),
            "save-as" => self.save_as(),
            "close" => self.close_active(),
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
            "markdown-refresh" => self.refresh_markdown_preview(),
            "markdown-preview" => self.open_markdown_preview(),
            "markdown-preview-side" => self.markdown_preview_to_side(),
            "markdown-source" => self.markdown_source(),
            "markdown-file-sync" => self.markdown_follow_toggle(),
            "markdown-scroll-sync" => self.markdown_scroll_toggle(),
            "split-right" => self.split_editor(false),
            "split-down" => self.split_editor(true),
            "focus-other" => self.focus_other_group(),
            "close-split" => self.close_split(),
            "select-color-theme" => self.show_theme_picker(false),
            "select-code-theme" => self.show_theme_picker(true),
            "settings" => self.open_settings(),
            "settings-file" => self.open_preferences_file(),
            "acknowledgements" => self.open_acknowledgements(),
            "theme-edit" => self.open_theme_settings(),
            "theme-reload" => self.reload_theme_settings(),
            "word-wrap" => self.toggle_word_wrap(),
            "minimap" => self.toggle_minimap(),
            "tab-outer-left" => {
                if let Some(id) = self.group_id(self.focused_group.get()) {
                    self.split_tab_outer(id, self.focused_group.get(), tab_drag::Edge::Left);
                }
            }
            "tab-outer-right" => {
                if let Some(id) = self.group_id(self.focused_group.get()) {
                    self.split_tab_outer(id, self.focused_group.get(), tab_drag::Edge::Right);
                }
            }
            "tab-outer-top" => {
                if let Some(id) = self.group_id(self.focused_group.get()) {
                    self.split_tab_outer(id, self.focused_group.get(), tab_drag::Edge::Top);
                }
            }
            "tab-outer-bottom" => {
                if let Some(id) = self.group_id(self.focused_group.get()) {
                    self.split_tab_outer(id, self.focused_group.get(), tab_drag::Edge::Bottom);
                }
            }
            "panel" => self.toggle_panel(),
            "restart-terminal" => self.restart_terminal(),
            "new-terminal" => self.new_terminal(),
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
        let back = self.clone();
        let themes = matches!(self.nav.mode.get(), Mode::Themes { .. });
        let expected_query = self.nav.query.get();
        let placeholder = match self.nav.mode.get() {
            Mode::Commands => "Type a command",
            Mode::Line => "Go to line:column",
            Mode::Themes { code_only: false } => "Select Color Theme",
            Mode::Themes { code_only: true } => "Select Code Color Theme",
            _ => "Search files by name · > commands · : line",
        };
        let height = if self.nav.effective_mode() == Mode::Line {
            0.0
        } else {
            (self.nav.rows.get().len().min(if themes { 7 } else { 8 }) as f32 * 34.0).min(280.0)
        };
        if_view!(self.nav.is_open(),
            super::popup::Anchored::top_center(vstack! {
                if_view!(themes,
                    hstack! {
                        Button::new("Commands").icon(Icon::ArrowLeft).header_style().font_size(12.0)
                            .text_color(colors.foreground).background_color(Color::TRANSPARENT)
                            .on_click(move || back.show_commands()),
                        Spacer::new(),
                    }.frame(f32::INFINITY, 24.0),
                    VStack::new(())),
                TextField::new(self.nav.query.clone()).placeholder(placeholder).placeholder_color(colors.muted).font_size(13.0)
                    .text_color(colors.foreground).background_color(colors.elevated)
                    .border_color(colors.border).focused_border_color(colors.accent).material(self.theme.get().input_material())
                    .autofocus(true).on_submit(move || submit.accept_navigation())
                    .on_cancel(move || close.close_navigation()).frame(f32::INFINITY, 32.0),
                ListView::new(self.nav.rows.clone(), self.nav.selected.clone(), 34.0, move |index, row: Row, selected| {
                    let app = rows.clone();
                    let query = expected_query.clone();
                    let hover_query = query.clone();
                    let hover = app.clone();
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
                        }).on_event_capture(move |event| {
                            if themes && matches!(event, Event::Mouse(scarlet_ui::event::MouseEvent::Moved { .. }))
                                && hover.nav.query.get() == hover_query && hover.nav.selected.get() != Some(index) {
                                hover.nav.selected.set(Some(index));
                                hover.preview_selected_theme();
                            }
                            false
                        })
                }).item_key(|row| row.key.clone().into()).frame(f32::INFINITY, height),
                Text::from_state(self.nav.hint.clone()).font_size(11.0).color(colors.muted)
                    .frame_width(f32::INFINITY),
            }.spacing(8.0).padding(12.0).frame_width(f32::INFINITY).surface_style(self.theme.get().popup_style()).corner_radius(12.0).clip_content(true)),
            VStack::new(())
        )
    }
}
fn shortcut(shortcut: &str) -> String {
    let shortcut = match shortcut {
        "GoBack" => {
            if cfg!(target_os = "macos") {
                "Ctrl+-"
            } else {
                "Alt+Left"
            }
        }
        "GoForward" => {
            if cfg!(target_os = "macos") {
                "Ctrl+Shift+-"
            } else {
                "Alt+Right"
            }
        }
        value => value,
    };
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
        let app = App::with_document();
        app.runtime.lock().unwrap().workspace.documents[0].editor =
            kureha_code::editor::Editor::new(text);
        app.sync_editor(app.document_id());
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
            app.bindings(app.document_id())
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
        let id = app.document_id();
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
    fn theme_picker_is_nested_previews_and_restores_or_commits() {
        use kureha_code::theme::{preset, Theme};
        let app = app_with_text("let value = 1;");
        let original = app.theme.get();
        assert_eq!(original.name(), "Kureha");
        app.show_commands();
        assert!(!app
            .nav
            .rows
            .get()
            .iter()
            .any(|row| row.key == "theme-monokai"));
        app.nav.query.set("Select Color Theme".into());
        app.poll_navigation();
        let entry = app
            .nav
            .rows
            .get()
            .iter()
            .position(|row| row.key == "select-color-theme")
            .unwrap();
        app.nav.selected.set(Some(entry));
        app.accept_navigation();
        assert_eq!(app.nav.mode.get(), Mode::Themes { code_only: false });
        assert_eq!(app.theme.get(), original);
        assert!(app.nav.rows.get().iter().any(|row| row.label == "Light"));
        app.nav.query.set("monokai".into());
        app.poll_navigation();
        assert_eq!(app.theme.get(), preset("monokai").unwrap());
        assert!(app.nav.is_open());
        app.navigation_key(KeyCode::Escape, KeyModifiers::empty());
        assert_eq!(app.theme.get(), original);
        assert!(!app.nav.is_open());

        app.show_theme_picker(false);
        app.nav.query.set("Nord".into());
        app.poll_navigation();
        app.accept_navigation();
        let committed = preset("nord").unwrap();
        assert_eq!(app.theme.get(), committed);
        app.close_navigation();
        assert_eq!(app.theme.get(), committed);
        app.show_theme_picker(false);
        app.nav.query.set("Monokai".into());
        app.poll_navigation();
        app.show_commands(); // Back to the parent cancels the pending choice.
        assert_eq!(app.theme.get(), committed);
        app.show_theme_picker(false);
        app.nav.query.set("Kureha".into());
        app.poll_navigation();
        app.set_theme(Theme::Light); // A settings reload supersedes the preview.
        app.close_navigation();
        assert_eq!(app.theme.get(), Theme::Light);
    }

    #[test]
    fn code_theme_preview_restores_custom_fonts_and_preserves_workbench_colors() {
        let app = app_with_text("source");
        let custom =
            kureha_code::theme::parse_theme("name='Custom'\nbase='kureha'\n[code]\nfont_size=18")
                .unwrap();
        app.set_theme(custom.clone());
        app.show_theme_picker(true);
        assert_eq!(app.theme.get(), custom);
        app.nav.query.set("monokai".into());
        app.poll_navigation();
        assert_eq!(app.theme.get().colors(), custom.colors());
        assert_eq!(app.theme.get().editor_font_size(), 18.0);
        assert_eq!(
            app.theme.get().editor_colors(),
            kureha_code::theme::preset("monokai")
                .unwrap()
                .editor_colors()
        );
        assert_eq!(
            app.theme.get().terminal_palette(),
            kureha_code::theme::preset("monokai")
                .unwrap()
                .terminal_palette()
        );
        app.navigation_key(KeyCode::Escape, KeyModifiers::empty());
        assert_eq!(app.theme.get(), custom);
        app.show_theme_picker(true);
        app.nav.query.set("Nord".into());
        app.poll_navigation();
        let preview = app.theme.get();
        app.accept_navigation();
        assert_eq!(app.theme.get(), preview);
        assert_eq!(app.theme.get().colors(), custom.colors());
    }

    #[test]
    fn theme_confirmation_persists_choices_but_preview_and_cancel_do_not() {
        let app = App::new();
        app.preferences.update(|p| p.font_size = Some(20.0));
        app.set_theme(app.theme.get().with_font_size(20.0));
        let original = app.preferences.get();
        app.open_settings();
        app.show_theme_picker(false);
        app.nav.query.set("Monokai".into());
        app.poll_navigation();
        assert_eq!(app.theme.get().editor_font_size(), 20.0);
        assert_eq!(app.preferences.get(), original);
        app.close_navigation();
        assert_eq!(app.preferences.get(), original);
        app.show_theme_picker(false);
        app.nav.query.set("Monokai".into());
        app.accept_navigation();
        assert_eq!(
            app.preferences.get().color_theme.as_deref(),
            Some("Monokai")
        );
        assert_eq!(app.preferences.get().font_size, Some(20.0));
        app.show_theme_picker(true);
        app.nav.query.set("Nord".into());
        app.accept_navigation();
        assert_eq!(app.preferences.get().code_theme.as_deref(), Some("Nord"));
    }
    #[test]
    fn mounted_theme_hover_previews_without_losing_query_focus() {
        use scarlet_ui::{
            event::MouseEvent, pipeline::RenderingPipeline, views::modifiers::OnEventRenderObject,
        };
        let app = app_with_text("source");
        let original = app.theme.get();
        app.show_theme_picker(false);
        app.nav.query.set("Dark".into());
        app.poll_navigation();
        let window = || {
            Window::new("Theme picker", app.content())
                .decorated(false)
                .size(Size::new(1100.0, 760.0))
        };
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_root(window().create_element());
        pipeline.layout_initial();
        pipeline.render();
        fn rows(e: &dyn Element, parent: Point, found: &mut Vec<Rect>) {
            let origin = Point::new(parent.x + e.position().x, parent.y + e.position().y);
            if e.bounds().size.height == 34.0
                && e.bounds().size.width > 500.0
                && e.render_object()
                    .is_some_and(|r| r.as_any().is::<OnEventRenderObject>())
            {
                found.push(Rect::new(origin, e.bounds().size));
            }
            for child in e.children() {
                rows(child.as_ref(), origin, found);
            }
        }
        let mut bounds = Vec::new();
        rows(
            pipeline.element_tree().root().unwrap(),
            Point::ZERO,
            &mut bounds,
        );
        bounds.sort_by(|a, b| a.origin.y.total_cmp(&b.origin.y));
        bounds.dedup_by(|a, b| a.origin.y == b.origin.y);
        assert!(bounds.len() >= 2);
        let row = bounds[1];
        pipeline.handle_event(&Event::Mouse(MouseEvent::Moved {
            x: (row.origin.x + 80.0) as i32,
            y: (row.origin.y + 17.0) as i32,
        }));
        assert_eq!(app.nav.selected.get(), Some(1));
        assert_eq!(
            app.theme.get(),
            kureha_code::theme::preset("solarized-dark").unwrap()
        );
        pipeline
            .element_tree_mut()
            .root_mut()
            .unwrap()
            .update(&window());
        pipeline.render();
        pipeline.handle_event(&Event::Keyboard(KeyEvent::Char { c: 'x' }));
        assert_eq!(app.nav.query.get(), "Darkx");
        assert_eq!(
            app.bindings(app.document_id())
                .unwrap()
                .document
                .get()
                .as_str(),
            "source"
        );
        pipeline.handle_event(&Event::Keyboard(KeyEvent::Pressed {
            keycode: KeyCode::Escape,
            modifiers: KeyModifiers::empty(),
        }));
        assert_eq!(app.theme.get(), original);
        pipeline.teardown();
    }

    #[test]
    fn quick_open_retains_results_and_geometry_while_a_new_query_is_pending() {
        use scarlet_ui::pipeline::RenderingPipeline;
        let app = app_with_text("");
        let root = PathBuf::from("/project");
        app.runtime.lock().unwrap().explorer.root = Some(root.clone());
        app.nav.mode.set(Mode::Files);
        let index = Arc::new(Index {
            root: root.clone(),
            files: vec![root.join("old.rs"), root.join("new.rs")],
            ..Index::default()
        });
        app.nav.show_rows(vec![Row {
            label: "old.rs".into(),
            detail: "old.rs".into(),
            key: "old.rs".into(),
            action: Action::File(root.join("old.rs")),
        }]);
        let (sender, receiver) = mpsc::sync_channel(1);
        {
            let mut service = app.nav.service.lock().unwrap();
            service.root = Some(root.clone());
            service.index = Some(index);
            service.last_query = Some((Mode::Files, "old".into()));
            service.displayed_query = service.last_query.clone();
            // Keep an old filter pending deterministically, without sleeps.
            service.worker = Some(Worker {
                receiver,
                cancel: Arc::new(AtomicBool::new(false)),
                generation: 0,
                kind: WorkerKind::Matches,
            });
        }
        let window = || {
            Window::new("Quick Open", app.navigation_overlay())
                .decorated(false)
                .size(Size::new(800.0, 600.0))
        };
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_paint_enabled(false);
        pipeline.set_root(window().create_element());
        pipeline.layout_initial();
        pipeline.render();
        fn panel(e: &dyn Element) -> Option<(scarlet_ui::element::ElementId, Rect)> {
            if e.type_name_debug().contains("TextFieldRenderObject") {
                return Some((e.id(), e.bounds()));
            }
            e.children().iter().find_map(|c| panel(c.as_ref()))
        }
        let original_field = panel(pipeline.element_tree().root().unwrap()).unwrap();
        for query in ["n", "ne", "new"] {
            app.nav.query.set(query.into());
            app.poll_navigation();
            assert_eq!(
                app.nav.rows.get()[0].label,
                "old.rs",
                "pending query erased visible results"
            );
            assert_eq!(app.nav.selected.get(), Some(0));
            app.accept_navigation();
            assert!(app.nav.is_open());
            assert!(
                app.runtime.lock().unwrap().job.is_none(),
                "stale visible row was accepted"
            );
            pipeline
                .element_tree_mut()
                .root_mut()
                .unwrap()
                .update(&window());
            pipeline.render();
            assert_eq!(
                panel(pipeline.element_tree().root().unwrap()).unwrap(),
                original_field
            );
        }
        drop(sender);
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.nav.service.lock().unwrap().displayed_query.as_ref()
            != Some(&(Mode::Files, "new".into()))
        {
            app.poll_navigation();
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert_eq!(app.nav.rows.get().len(), 1);
        assert_eq!(app.nav.rows.get()[0].label, "new.rs");
        pipeline.teardown();
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
        let bindings = app.bindings(app.document_id()).unwrap();
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
    fn palette_rows_keep_quiet_selection_and_activate_across_their_full_width() {
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
                            | PaintCommand::FillVerticalGradientRoundedRect { .. }
                    )),
                    "row painted a button border or gradient"
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
                app.set_theme(theme.clone());
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
            "panel",
            "select-color-theme",
            "go-line",
        ] {
            assert!(ids.contains(required));
        }
        assert!(navigation::filter_commands(COMMANDS, "split", 10).len() >= 2);
    }
}
